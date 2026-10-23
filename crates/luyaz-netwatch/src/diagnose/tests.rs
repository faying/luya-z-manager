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
    let (code, body) = refusal_at(&Gate { diag_since: Some(now_unix()), ..Gate::default() }, now_unix()).unwrap();
    assert_eq!(code, 409);
    assert_eq!(body["error"], "正在诊断，约 20 秒后再试");
    assert_eq!(body["error_en"], "Diagnosing; try again in about 20 s");
    assert_eq!(refusal_at(&Gate { diag_since: Some(now_unix() - 60), ..Gate::default() }, now_unix()).unwrap().1["retry_after_s"], 1);
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
    /// `wan_status` in the state
    wan: &'static str,
    /// probes sent: link, path, local DNS, eSIM channel
    calls: Mutex<[u32; 4]>,
    /// what `fresh_network` has (a Finding probe's result)
    fresh: Mutex<Option<NetworkEvidence>>,
    /// what `https_probe` answers; asked how many times
    https: Option<HttpsEvidence>,
    https_calls: Mutex<u32>,
}

fn fake(busy: Option<&'static str>) -> Fake {
    Fake { busy: Mutex::new(busy), events: Mutex::default(), wan: "ipv4_connected", calls: Mutex::default(), fresh: Mutex::default(), https: None, https_calls: Mutex::default() }
}

fn passed() -> NetworkEvidence {
    NetworkEvidence {
        path_reachable: ResultState::Passed,
        dns: ResultState::Passed,
        reason: "scoped_http_dns_only".into(),
        ..NetworkEvidence::default()
    }
}

impl Host for Fake {
    fn radio_inputs(&self) -> (Option<Value>, Option<Value>) {
        let st = json!({"net": {"type": "SA", "wan_status": self.wan, "nr_rsrp": -85, "nr_snr": "20",
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
        self.calls.lock().unwrap()[0] += 1;
        LinkProbe { sent: 10, rtts: vec![30; 10] }
    }
    fn sim_inputs(&self) -> SimIn {
        SimIn { state: Some("modem_init_complete".into()), channel: None }
    }
    fn esim_channel(&self) -> Option<EsimChannel> {
        self.calls.lock().unwrap()[3] += 1;
        Some(EsimChannel::Full)
    }
    fn path_probe(&self, _: Option<&Value>) -> NetworkEvidence {
        self.calls.lock().unwrap()[1] += 1;
        passed()
    }
    fn fresh_network(&self) -> Option<NetworkEvidence> {
        self.fresh.lock().unwrap().clone()
    }
    fn https_probe(&self, _: Option<&Value>, _: Duration) -> Option<HttpsEvidence> {
        *self.https_calls.lock().unwrap() += 1;
        self.https.clone()
    }
    fn local_dns(&self) -> Option<LocalDns> {
        self.calls.lock().unwrap()[2] += 1;
        Some(LocalDns { asked: 2, answered: 2, ms: Some(20) })
    }
    fn clock_status(&self) -> Option<EvidenceStatus> {
        Some(EvidenceStatus::Estimated)
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
    let host = Arc::new(fake(Some("register")));
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
    let host = Arc::new(fake(None));
    let d = Diagnosis::new(host, Arc::default());
    assert_eq!(d.start(Some("127.0.0.1".parse().unwrap())).1["data"]["from"], "touch");
    until("done", || d.get().1["data"]["state"] == "done");
    assert_eq!(d.start(None).1["data"]["from"], "other");
}

fn level_of(v: &Value, id: &str) -> String {
    v["layers"].as_array().unwrap().iter().find(|l| l["id"] == id).map(|l| l["level"].as_str().unwrap().to_string()).unwrap_or_default()
}

#[test]
fn every_layer_runs_when_the_bearer_is_up() {
    let host = Arc::new(fake(None));
    let d = Diagnosis::new(host.clone(), Arc::default());
    d.start(None);
    until("done", || d.get().1["data"]["state"] == "done");
    let v = d.get().1["data"].clone();
    let ids: Vec<&str> = v["layers"].as_array().unwrap().iter().map(|l| l["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["wifi", "sim", "signal", "limit", "bearer", "link", "dns", "e2e", "clock", "crowd"]);
    for (id, lv) in [("sim", "warn"), ("bearer", "ok"), ("link", "ok"), ("dns", "ok"), ("e2e", "ok"), ("clock", "info")] {
        assert_eq!(level_of(&v, id), lv, "{id}");
    }
    assert_eq!(*host.calls.lock().unwrap(), [1, 1, 1, 1]);
    assert_eq!(v["main"]["layer"], "sim");
}

#[test]
fn no_probes_past_a_bad_bearer() {
    let host = Arc::new(Fake { wan: "ipv4_disconnected", ..fake(None) });
    let gate: Arc<Mutex<Gate>> = Arc::default();
    let d = Diagnosis::new(host.clone(), gate.clone());
    d.start(None);
    until("done", || d.get().1["data"]["state"] == "done");
    let v = d.get().1["data"].clone();
    assert_eq!(level_of(&v, "bearer"), "bad");
    for id in ["link", "dns", "e2e", "clock", "crowd"] {
        assert_eq!(level_of(&v, id), "blocked", "{id}");
    }
    // link, path, local DNS and HTTPS probes never called; the card is still read
    assert_eq!(&host.calls.lock().unwrap()[..3], &[0, 0, 0]);
    assert_eq!(*host.https_calls.lock().unwrap(), 0);
    assert_eq!((v["main"]["layer"].as_str(), v["main"]["more"].as_u64()), (Some("bearer"), Some(1)));
    assert!(gate.lock().unwrap().diag_since.is_none());
    // the event carries the blocked layers as they are
    assert_eq!(host.events.lock().unwrap()[0].1["layers"]["dns"], "blocked");
}

#[test]
fn a_finding_probe_is_waited_for_and_reused() {
    let host = Arc::new(fake(None));
    let gate: Arc<Mutex<Gate>> = Arc::new(Mutex::new(Gate { finding_since: Some(now_unix()), ..Gate::default() }));
    let d = Diagnosis::new(host.clone(), gate.clone());
    d.start(None);
    until("waiting_for", || d.get().1["data"]["waiting_for"] == "finding");
    assert!(gate.lock().unwrap().diag_since.is_none());
    // the Finding probe ends: gate first, its result a moment later (as the core stores it)
    gate.lock().unwrap().finding_since = None;
    thread::sleep(Duration::from_millis(200));
    *host.fresh.lock().unwrap() = Some(passed());
    until("done", || d.get().1["data"]["state"] == "done");
    let v = d.get().1["data"].clone();
    assert_eq!(level_of(&v, "e2e"), "ok");
    assert_eq!(host.calls.lock().unwrap()[1], 0, "no second path probe");
}

#[test]
fn bearer_reads() {
    let route = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
rmnet_data0\t00000000\t00000000\t0001\t0\t0\t0\t00000000\t0\t0\t0\n\
br-lan\t0000A8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0\n";
    assert_eq!(default_routes(route), Some(vec!["rmnet_data0".to_string()]));
    assert_eq!(default_routes("Iface\tDestination\n"), Some(vec![]));
    assert_eq!(default_routes(""), None);
    let devs: Vec<String> = ["lo", "br-lan", "rmnet_data0", "tailscale0", "Meta"].map(String::from).to_vec();
    assert_eq!(ways_out("rmnet_data0", &["rmnet_data0".into()], &devs), 3);
    assert_eq!(ways_out("rmnet_data0", &["rmnet_data0".into()], &devs[..3]), 1);
    let idle: Vec<String> = ["tunl0", "sit0", "ip6tnl0", "tun0"].map(String::from).to_vec();
    assert_eq!(ways_out("rmnet_data0", &[], &idle), 2, "only tun0 counts");
    assert_eq!(rp_effective(Some(0), Some(1)), Some(1));
    assert_eq!(rp_effective(Some(2), Some(1)), Some(2));
    assert_eq!(rp_effective(None, None), None);
    // the state alone: wan_status and the Finding path's addresses
    let st = json!({"net": {"wan_status": "ipv4_connected"}, "interfaces": {
        "cellular": {"ipv4_address": "10.20.30.40", "ipv4_dev_name": "rmnet_data0"}, "wan4": {}}});
    let b = bearer_base(Some(&st));
    assert_eq!((b.connected, b.has_ip, b.iface.as_str()), (Some(true), Some(true), "rmnet_data0"));
    assert_eq!(bearer_base(None), BearerIn::default());
}

#[test]
fn https_that_never_gets_through_warns_beside_a_plain_http_answer() {
    use luyaz_api::execution::https::{Attempt, Sample, judge};
    let s: Vec<Sample> = ["apple", "tencent", "baidu"].iter().map(|o| Sample { org: o, attempt: Attempt::Unreachable }).collect();
    let host = Arc::new(Fake { https: Some(HttpsEvidence::of(&s, judge(&s, Some(0), None))), ..fake(None) });
    let d = Diagnosis::new(host.clone(), Arc::default());
    d.start(None);
    until("done", || d.get().1["data"]["state"] == "done");
    let v = d.get().1["data"].clone();
    assert_eq!(level_of(&v, "e2e"), "warn");
    assert_eq!(*host.https_calls.lock().unwrap(), 1);
}
