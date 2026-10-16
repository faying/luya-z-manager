use super::*;
use luyaz_speedtest::Phase;

fn key(hour: u8) -> CellKey {
    CellKey { plmn: Some("460-11".into()), cell: Some(5_242_881_234), ch: Some(633_984), hour }
}

#[test]
fn history_from_events() {
    use crate::record::Event;
    let mut d = key_json(&key(20));
    d["link_ms"] = json!(41);
    d["in_use"] = json!(false);
    let mut s = key_json(&key(20));
    s["mbps"] = json!(6.5);
    let no_cell = json!({"plmn": "460-11", "hour": 20, "mbps": 3.0});
    let evs = vec![Event::new(1, "diagnose", d), Event::new(2, "speed", s), Event::new(3, "speed", no_cell), Event::new(4, "data_down", json!({}))];
    let (runs, sp) = past_from_events(&evs);
    assert_eq!(runs, [PastRun { key: key(20), link_ms: 41, in_use: false }]);
    assert_eq!(sp, [PastSpeed { key: key(20), mbps: 6.5 }]);
}

#[test]
fn gate_refuses_both_ways() {
    assert!(refusal_at(&Gate::default(), now_unix()).is_none());
    // a diagnosis is running: speed test / search / register get a 409 with a time
    let (code, body) = refusal_at(&Gate { diag_since: Some(now_unix()) }, now_unix()).unwrap();
    assert_eq!(code, 409);
    assert_eq!(body["error"], "正在诊断，约 20 秒后再试");
    assert_eq!(body["error_en"], "Diagnosing; try again in about 20 s");
    assert_eq!(refusal_at(&Gate { diag_since: Some(now_unix() - 60) }, now_unix()).unwrap().1["retry_after_s"], 1);
    assert_eq!(eta(100, 105), 15);
}

#[test]
fn speed_row_shows_the_route_and_no_verdict() {
    use luyaz_speedtest::SpeedTestProgress;
    let l = speed_row(&SpeedTestProgress::ended(Phase::Complete, Some(86.4)));
    assert_eq!((l.level, l.detail.as_str(), l.detail_en.as_str()), (Level::Info, "直连 ↓ 86 Mbps", "Direct ↓ 86 Mbps"));
    assert_eq!(speed_row(&SpeedTestProgress::ended(Phase::Cancelled, Some(3.0))).detail, "被停下");
    assert_eq!(speed_row(&SpeedTestProgress::ended(Phase::Error, None)).detail, "没测成");
    // the main sentence never looks at the speed row
    let mut r = Run::new(1, 0, "client", false);
    for l in r.layers.clone() {
        r.put(l.set(Level::Ok, "", ""));
    }
    r.speed = Some(speed_row(&SpeedTestProgress::ended(Phase::Complete, Some(2.0))));
    assert_eq!(pick_main(&r.layers).text, "没查到问题");
}

/// A device that answers at once: one SA reading, no stations, a quick link probe.
struct Fake {
    busy: Mutex<Option<&'static str>>,
    events: Mutex<Vec<(String, Value)>>,
}

impl Host for Fake {
    fn radio_inputs(&self) -> (Option<Value>, Option<Value>) {
        let st = json!({"net": {"type": "SA", "wan_status": "ipv4_connected", "nr_rsrp": -85, "nr_snr": "20",
            "nr_rsrq": -10, "mcc": 460, "mnc": 11, "nr_cell_id": 5_242_881_234i64, "nr_channel": 633_984,
            "wan_dns": "192.0.2.53"}});
        (Some(st), Some(json!({"net": {"story": {"state": "ok"}, "bars_tier": 4}})))
    }
    fn stations(&self) -> Vec<Sta> {
        Vec::new()
    }
    fn lease_mac(&self, ip: &str) -> Option<String> {
        (ip == "192.168.0.23").then(|| "02:00:00:00:00:23".into())
    }
    fn others_busy(&self) -> Option<&'static str> {
        *self.busy.lock().unwrap()
    }
    fn link_probe(&self, _: &[String]) -> LinkProbe {
        LinkProbe { sent: 10, rtts: vec![30; 10] }
    }
    fn push_event(&self, kind: &str, data: Value) {
        self.events.lock().unwrap().push((kind.into(), data));
    }
    fn events(&self, _: u64, _: u64) -> Vec<Event> {
        Vec::new()
    }
}

fn until(what: &str, f: impl Fn() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_run_waits_for_the_modem_claims_the_gate_and_logs_itself() {
    let host = Arc::new(Fake { busy: Mutex::new(Some("register")), events: Mutex::default() });
    let gate: Arc<Mutex<Gate>> = Arc::default();
    let d = Diagnosis::new(host.clone(), gate.clone());
    let (code, v) = d.start(Some("192.168.0.23".parse().unwrap()));
    assert_eq!((code, v["data"]["from"].as_str()), (202, Some("client")));
    // waiting for the register: the gate is not taken, so the register can finish
    until("waiting_for", || d.get().1["data"]["waiting_for"] == "register");
    assert!(gate.lock().unwrap().diag_since.is_none());
    // a second start joins the same run
    assert_eq!(d.start(None).1["joined"], true);
    *host.busy.lock().unwrap() = None;
    until("done", || d.get().1["data"]["state"] == "done");
    assert!(gate.lock().unwrap().diag_since.is_none(), "released when done");
    let ev = host.events.lock().unwrap().clone();
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].0, "diagnose");
    assert_eq!((ev[0].1["from"].as_str(), ev[0].1["link_ms"].as_u64()), (Some("client"), Some(30)));
    assert_eq!(ev[0].1["plmn"], "460-11");

    // feedback once, logged; speed through the caller's engine
    assert_eq!(d.feedback(br#"{"id":1,"right":true}"#).1["data"]["recorded"], true);
    assert_eq!(d.feedback(br#"{"id":1,"right":false}"#).1["data"]["already"], true);
    let (code, _) = d.speed(br#"{"id":1}"#, |done| {
        done(&SpeedTestProgress::ended(Phase::Complete, Some(42.0)));
        (200, json!({"ok": true}))
    });
    assert_eq!(code, 202);
    let kinds: Vec<String> = host.events.lock().unwrap().iter().map(|e| e.0.clone()).collect();
    assert_eq!(kinds, ["diagnose", "diagnose_feedback", "speed"]);
    assert_eq!(d.get().1["data"]["speed"]["detail_en"], "Direct ↓ 42 Mbps");
    // the engine refusing (a diagnosis-free 409) puts the old row back
    let (code, _) = d.speed(br#"{"id":1}"#, |_| (409, json!({"ok": false})));
    assert_eq!(code, 409);
    assert_eq!(d.get().1["data"]["speed"]["detail_en"], "Direct ↓ 42 Mbps");
}

#[test]
fn the_front_screen_is_loopback() {
    let host = Arc::new(Fake { busy: Mutex::new(None), events: Mutex::default() });
    let d = Diagnosis::new(host, Arc::default());
    assert_eq!(d.start(Some("127.0.0.1".parse().unwrap())).1["data"]["from"], "touch");
    until("done", || d.get().1["data"]["state"] == "done");
    assert_eq!(d.start(None).1["data"]["from"], "other");
}
