//! Against a fake core: noticing that core went away after having answered (it answers
//! /v2/state and /v2/events, goes silent with the stream open, then a fresh one comes back);
//! retries paced, across the 32-bit ms wrap; the /v2 blocks put back together read exactly like
//! the old /state; /control parked until core answers; one full snapshot pinned field by field.

use super::*;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

const SNAP: &str = r#"{"epoch":"e1","seq":0,"blocks":{"signal":{"revision":1,"observed_at":1,"stale":false,"data":{"type":"NR5G_SA","operator":"X"}}}}"#;

#[test]
fn usb_capability_refusal_is_not_confused_with_another_busy_change() {
    // Synthetic complete Core reply, with an assessment larger than the old 2 KiB window.
    let body = format!(r#"{{"ok":false,"error":{{"code":"capability_unverified","policy":{{"padding":"{}"}}}}}}"#, "x".repeat(2500));
    let (_, en) = usb_refusal(body.as_bytes()).unwrap();
    assert!(en.contains("unverified") && en.contains("manually"));
    assert!(usb_refusal(br#"{"error":{"code":"usb_method_absent"}}"#).unwrap().1.contains("does not support"));
    assert!(usb_refusal(br#"{"error":{"code":"usb_preflight_unavailable"}}"#).unwrap().1.contains("no change was sent"));
    assert!(usb_refusal(br#"{"error":{"code":"busy"}}"#).is_none());
    assert!(usb_refusal(br#"{"error":{"message":"unknown"}}"#).is_none());
}

fn cfg(port: u16) -> Cfg {
    Cfg { host: "127.0.0.1".into(), port, retry_ms: 100, silent_ms: 400, silent_retry_ms: 150, io_ms: 300 }
}

/// A fake core on a thread. mode 0: answers everything, closes each connection after 20 ms;
/// mode 1: answers /v2/state and the first /v2/events, then holds that stream open and silent
/// and accepts nothing more; mode 2: accepts and hangs up at once; mode 3: accepts and holds
/// silent. Counts the accepts.
struct Fake {
    stop: Arc<AtomicBool>,
    accepts: Arc<AtomicUsize>,
    h: Option<JoinHandle<()>>,
}

impl Fake {
    fn start(l: TcpListener, mode: u8) -> Fake {
        let stop = Arc::new(AtomicBool::new(false));
        let accepts = Arc::new(AtomicUsize::new(0));
        let (st, ac) = (stop.clone(), accepts.clone());
        l.set_nonblocking(true).unwrap();
        let h = std::thread::spawn(move || {
            let mut held: Vec<TcpStream> = Vec::new();
            let mut i = 0;
            while !st.load(Ordering::Relaxed) {
                if mode == 1 && i >= 2 {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                match l.accept() {
                    Ok((mut c, _)) => {
                        ac.fetch_add(1, Ordering::Relaxed);
                        c.set_nonblocking(false).unwrap();
                        match mode {
                            2 => drop(c),
                            3 => held.push(c),
                            _ => {
                                answer(&mut c);
                                if mode == 1 && i == 1 {
                                    held.push(c);
                                } else {
                                    std::thread::sleep(Duration::from_millis(20));
                                    drop(c);
                                }
                            }
                        }
                        i += 1;
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(2)),
                }
            }
        });
        Fake { stop, accepts, h: Some(h) }
    }

    fn kill(mut self) -> usize {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.h.take() {
            h.join().unwrap();
        }
        self.accepts.load(Ordering::Relaxed)
    }
}

fn answer(c: &mut TcpStream) {
    let mut rq = [0u8; 1024];
    let n = c.read(&mut rq).unwrap_or(0);
    let rq = &rq[..n];
    let out = if rq.starts_with(b"GET /v2/state") {
        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{SNAP}", SNAP.len())
    } else if rq.starts_with(b"GET /v2/events") {
        format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\nevent: snapshot\ndata: {SNAP}\n\n")
    } else {
        return;
    };
    let _ = c.write_all(out.as_bytes());
}

fn listener() -> (TcpListener, u16) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let p = l.local_addr().unwrap().port();
    (l, p)
}

fn spin(d: &mut CoreLink, ms: u32) {
    let end = mono_ms32().wrapping_add(ms);
    while (end.wrapping_sub(mono_ms32()) as i32) > 0 {
        if d.poll(mono_ms32()) {
            d.commit_latest();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn feed(d: &mut CoreLink, name: &str, j: &str) -> i32 {
    d.process_sse_event(format!("event: {name}\ndata: {j}\n\n").as_bytes())
}

const L_NET: &str = r#"{"type":"NR5G_SA","operator":"CMCC","band":"3","nr_band":"78","bars":4,"nr_rsrp":-91,"lte_rsrp":-99,"mcc":460,"mnc":0,"nrca":"78,1,627264,100","HSR":false}"#;
const L_BAT: &str = r#"{"percent":77,"temp":31,"charging":1,"charger_connect":1,"bat_uv":4012000}"#;
const L_PWR: &str = r#"{"direct_supply":{"mode":"disable"}}"#;
const L_SYS: &str = r#"{"uptime":1234,"cpu_temp":45,"cpu_usage":7,"model":"MU5250","imei":"86"}"#;
const L_TRF: &str = r#"{"rx_speed":1000,"tx_speed":20,"day_rx_bytes":5000}"#;
const L_LIST: &str = r#"[{"id":9,"num":"10086","date":"08-27 04:00","unread":1,"text":"测试 \"q\""},{"id":8,"num":"10010","date":"08-26 01:00","unread":0,"text":"{x}"}]"#;
const L_SIM: &str = r#"{"state":"ready","iccid":"8986","imsi":"46000","spn":""}"#;
const L_QOS: &str = r#"{"qci":9,"ambr_dl":"1000.5","ambr_ul":"200","bearer":"nr_pdu","stale":true,"usb_mode":"rndis"}"#;
const L_CLI: &str = r#"{"total":2,"wifi":1,"lan":1,"list":[{"name":"mac","ip":"192.168.0.2","mac":"aa"}]}"#;
const L_WLAN: &str = r#"{"ssid":"U60","key":"k","enc":"psk2","enabled":1}"#;
const L_DHCP: &str = r#"{"ip":"192.168.0.1","start":"100","limit":"50","leasetime":"12h"}"#;
const L_IF: &str = r#"{"cellular":{"enable":1,"roam_enable":0,"connect_status":"ipv4_connected"}}"#;
const L_TYPEC: &str = r#"{"power_role":"source","data_role":"host","cc_attch_state":1}"#;

fn blk(n: &str, d: &str) -> String {
    format!(r#""{n}":{{"revision":1,"observed_at":1,"stale":false,"data":{d}}}"#)
}

#[test]
fn mobile_data_switch_reads_like_core() {
    // enable 0 but connected (the default after boot) counts as on; 0 and down is off
    let c = [
        (r#"{"enable":0,"connect_status":"ipv4_ipv6_connected"}"#, 1),
        (r#"{"enable":0,"connect_status":"connecting"}"#, 1),
        (r#"{"enable":0,"connect_status":"disconnected"}"#, 0),
        (r#"{"enable":0,"connect_status":""}"#, -1),
        (r#"{"enable":0}"#, -1),
        (r#"{"enable":1,"connect_status":"disconnected"}"#, 1),
    ];
    for (cell, want) in c {
        let d = decode_snapshot(format!(r#"{{"interfaces":{{"cellular":{cell}}},"ts":1}}"#).as_bytes()).unwrap();
        assert_eq!(d.cell_data, want, "{cell}");
    }
}

#[test]
fn v2_matches_legacy() {
    let legacy = format!(
        r#"{{"net":{L_NET},"battery":{L_BAT},"power":{L_PWR},"system":{L_SYS},"traffic":{L_TRF},"sms":{{"unread":1,"list":{L_LIST}}},"sim":{L_SIM},"qos":{L_QOS},"clients":{L_CLI},"wlan":{L_WLAN},"dhcp":{L_DHCP},"interfaces":{L_IF},"typec":{L_TYPEC},"powerbank":{{"state":0}},"ts":1}}"#
    );
    let snap = format!(
        r#"{{"epoch":"e7","seq":41,"blocks":{{{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},"nfc":{{"revision":0,"observed_at":0,"stale":true,"data":null}}}}}}"#,
        blk("signal", L_NET),
        blk("battery", L_BAT),
        blk("charger", L_PWR),
        blk("live", &format!(r#"{{"system":{L_SYS},"runtime":{{"x":1}},"traffic":{L_TRF}}}"#)),
        blk("sms", r#"{"unread":1,"max_id":9,"count":2}"#),
        blk("sms_list", &format!(r#"{{"list":{L_LIST}}}"#)),
        blk("sim", L_SIM),
        blk("qos", L_QOS),
        blk("clients", L_CLI),
        blk("wlan", L_WLAN),
        blk("dhcp", L_DHCP),
        blk("interfaces", L_IF),
        blk("op", r#"{"active":null}"#),
        blk("typec", L_TYPEC),
        blk("powerbank", r#"{"state":0}"#),
    );
    let want = decode_snapshot(legacy.as_bytes()).unwrap();
    assert!(want.inbox_n == 2 && want.batt_pct == 77 && want.lan_total == 2);
    assert!(want.usb_cc == 1 && want.powerbank == 0 && want.usb_power_role == "source" && want.usb_data_role == "host");
    assert_eq!(want.inbox[0].text, "测试 \"q\"");
    assert_eq!(want.carrier, "中国移动");
    assert_eq!(want.ambr_down, 1000.5);
    assert!(want.qos_bearer == "nr_pdu" && want.qos_stale == 1);
    assert!(want.dps_mode == 0 && want.cell_data == 1 && want.cell_roam == 0);

    let mut d = CoreLink::new(cfg(1));
    // the snapshot event: every field the screen reads, byte for byte
    assert_eq!(feed(&mut d, "snapshot", &snap), 1);
    assert!(d.have_seq && d.seq == 41 && d.epoch == "e7");
    assert_eq!(decode_snapshot(&d.latest_raw).unwrap(), want);
    // GET /v2/state carries the same: same result, the stream position untouched
    assert_eq!(d.v2_apply_snapshot(snap.as_bytes(), false), 0);
    assert_eq!(d.seq, 41);

    // a block event moves one block; a heartbeat only seq
    assert_eq!(
        feed(&mut d, "block", r#"{"epoch":"e7","seq":42,"name":"battery","revision":2,"observed_at":2,"stale":false,"data":{"percent":76}}"#),
        1
    );
    assert!(d.fresh_snap.batt_pct == 76 && d.fresh_snap.inbox_n == 2);
    assert_eq!(feed(&mut d, "heartbeat", r#"{"epoch":"e7","seq":43,"blocks":{},"exec_age_ms":0}"#), 0);
    // a block the screen does not read
    assert_eq!(feed(&mut d, "block", r#"{"epoch":"e7","seq":44,"name":"op","revision":2,"observed_at":2,"stale":false,"data":{}}"#), 0);

    // stale: left out like a failed read in the old /state, not the kept value
    let ev = format!(r#"{{"epoch":"e7","seq":45,"name":"clients","revision":2,"observed_at":1,"stale":true,"data":{L_CLI}}}"#);
    assert_eq!(feed(&mut d, "block", &ev), 1);
    assert!(d.fresh_snap.lan_total == 0 && d.fresh_snap.lan_n == 0);
    let ev = format!(r#"{{"epoch":"e7","seq":46,"name":"sms_list","revision":2,"observed_at":1,"stale":true,"data":{{"list":{L_LIST}}}}}"#);
    assert_eq!(feed(&mut d, "block", &ev), 1);
    assert!(d.fresh_snap.inbox_n == 0 && d.fresh_snap.sms_new == 1);
    // typec stale: unknown (-1), not "nothing plugged in"
    let ev = format!(r#"{{"epoch":"e7","seq":47,"name":"typec","revision":2,"observed_at":1,"stale":true,"data":{L_TYPEC}}}"#);
    assert_eq!(feed(&mut d, "block", &ev), 1);
    assert!(d.fresh_snap.usb_cc == -1 && d.fresh_snap.powerbank == 0);
    assert!(!d.resync);

    // a gap, another epoch, or a block before any snapshot: resync, nothing applied
    assert_eq!(
        feed(&mut d, "block", r#"{"epoch":"e7","seq":49,"name":"battery","revision":3,"observed_at":3,"stale":false,"data":{"percent":10}}"#),
        0
    );
    assert!(d.resync && d.fresh_snap.batt_pct == 76);
    d.resync = false;
    assert_eq!(feed(&mut d, "heartbeat", r#"{"epoch":"e8","seq":48,"blocks":{}}"#), 0);
    assert!(d.resync);
    d.resync = false;
    d.have_seq = false;
    assert_eq!(feed(&mut d, "block", r#"{"epoch":"e7","seq":47,"name":"battery","stale":false,"data":{"percent":10}}"#), 0);
    assert!(d.resync && d.fresh_snap.batt_pct == 76);
}

#[test]
fn retries_are_paced_when_core_never_answers() {
    // core hangs up, or holds silent: retries follow retry_ms (100 ms here), not every pass
    // of a 1 ms loop. 500 ms ≈ 6 tries × 2 connects; per pass would be ~1000.
    for mode in [2u8, 3] {
        let (l, port) = listener();
        let f = Fake::start(l, mode);
        let mut d = CoreLink::new(cfg(port));
        std::thread::sleep(Duration::from_millis(50));
        let end = mono_ms32().wrapping_add(500);
        while (end.wrapping_sub(mono_ms32()) as i32) > 0 {
            d.poll(mono_ms32());
            std::thread::sleep(Duration::from_millis(1));
        }
        let got = f.kill();
        assert!((1..=20).contains(&got), "mode {mode}: {got} connects in 500 ms");
        assert!(!d.refresh().0);
    }
}

#[test]
fn silent_then_back() {
    let mut d = CoreLink::new(cfg(1));
    // never answered: not valid, no alive time, not "silent"
    assert!(!d.refresh().0);
    assert_eq!(d.alive_wall(), 0);
    assert!(!d.silent());

    let (l, port) = listener();
    d.cfg.port = port;
    let f = Fake::start(l, 1);
    std::thread::sleep(Duration::from_millis(50));
    spin(&mut d, 150);
    let (ok, s) = d.refresh();
    assert!(ok && s.radio == "NR5G_SA");
    let alive = d.alive_wall();
    assert!(alive > 0);
    assert!(!d.silent());

    // silent past silent_ms with the stream still open: reported silent; the last snapshot is
    // still handed out (the screen dims it)
    spin(&mut d, 700);
    assert!(d.silent());
    let (ok, s) = d.refresh();
    assert!(ok && s.radio == "NR5G_SA");
    assert_eq!(d.alive_wall(), alive);
    f.kill();

    // a healthy core again (same port): back to live
    let l = TcpListener::bind(("127.0.0.1", port)).unwrap();
    let f = Fake::start(l, 0);
    std::thread::sleep(Duration::from_millis(50));
    spin(&mut d, 400);
    assert!(d.refresh().0);
    assert!(!d.silent());
    assert!(d.alive_wall() >= alive);
    f.kill();
}

#[test]
fn reconnect_due_across_the_wrap() {
    // a deadline set just before the 32-bit ms wrap, polled just after it, must retry; so must
    // 0 ("due") once now is past 2^31. One poll each, counted.
    let w: [(u32, u32, bool); 4] = [(0xFFFF_FF00, 0x10, true), (0, 0x8000_0010, true), (0x100, 0x10, false), (0x100, 0xFFFF_FFF0, false)];
    for (due, now, want) in w {
        let (l, port) = listener();
        let f = Fake::start(l, 2);
        std::thread::sleep(Duration::from_millis(50));
        let mut d = CoreLink::new(cfg(port));
        // pretend it had a snapshot, so the "never had one" path does not connect
        d.latest_ok = true;
        d.shown_ok = true;
        d.retry_at_ms = due;
        d.poll(now);
        std::thread::sleep(Duration::from_millis(50));
        let got = f.kill();
        assert!(if want { got >= 1 } else { got == 0 }, "due {due:08x} now {now:08x}: {got} connects");
    }
}

#[test]
fn control_waits_for_the_answer() {
    // core answers 409 with its sentence after 300 ms: the request stays parked till then,
    // and the screen gets the sentence; a 200 says nothing
    let (l, port) = listener();
    let h = std::thread::spawn(move || {
        for i in 0..2 {
            let (mut c, _) = l.accept().unwrap();
            let rq = crate::data::testsrv::read_req(&mut c);
            assert!(rq.contains("\"source\":\"screen\"") && rq.contains("\"action\":\"network.set_mode\""), "{rq}");
            assert!(rq.ends_with("\"params\":{\"mode\":\"Only_LTE\"}}"), "{rq}");
            std::thread::sleep(Duration::from_millis(300));
            let r = if i == 0 {
                "HTTP/1.1 409 Conflict\r\n\r\n{\"doing\":{\"say_zh\":\"正在换制式\",\"say_en\":\"Switching\"}}"
            } else {
                "HTTP/1.1 200 OK\r\n\r\n{}"
            };
            c.write_all(r.as_bytes()).unwrap();
        }
    });
    let mut d = CoreLink::new(cfg(port));
    assert!(d.control_fb("network.set_mode", r#"{"mode":"Only_LTE"}"#, "network.set_mode", None));
    assert_eq!(d.parked(), 1);
    std::thread::sleep(Duration::from_millis(100));
    d.control_reap(mono_ms32());
    assert_eq!(d.parked(), 1, "closed before core answered");
    std::thread::sleep(Duration::from_millis(400));
    d.control_reap(mono_ms32());
    assert_eq!(d.parked(), 0);
    let n = d.notice(4000).expect("a notice");
    assert!(n.kind == 1 && n.action == "network.set_mode" && n.say_zh == "正在换制式" && n.say_en == "Switching");
    d.notice = data_notice_t::default();
    assert!(d.control_fb("network.set_mode", r#"{"mode":"Only_LTE"}"#, "network.set_mode", None));
    std::thread::sleep(Duration::from_millis(500));
    d.control_reap(mono_ms32());
    assert!(d.parked() == 0 && d.notice(4000).is_none());
    h.join().unwrap();

    // nobody there: not sent, and no emergency write without arguments
    let (l, port) = listener();
    drop(l);
    let mut d = CoreLink::new(cfg(port));
    assert!(!d.control_fb("band.reset", "{}", "band.reset", None));
}

#[test]
fn pace_only_on_a_change() {
    let (port, seen, h) = crate::data::testsrv::canned(vec!["HTTP/1.1 200 OK\r\n\r\n{}".into()], 2);
    let mut d = CoreLink::new(cfg(port));
    d.set_pace(false);
    d.set_pace(false);
    d.set_pace(true);
    std::thread::sleep(Duration::from_millis(200));
    d.control_reap(mono_ms32());
    h.join().unwrap();
    let s = seen.lock().unwrap();
    assert_eq!(s.len(), 2);
    assert!(s[0].contains("\"milliseconds\":5000") && s[1].contains("\"milliseconds\":1000"));
    assert!(s.iter().all(|r| r.contains("\"client\":\"touch\"") && r.contains("\"ttl_s\":180")));
}

#[test]
fn sms_delete_needs_two_steps() {
    let mut d = CoreLink::new(cfg(1));
    d.shown_snap = decode_snapshot(format!(r#"{{"sms":{{"unread":1,"list":{L_LIST}}}}}"#).as_bytes()).unwrap();
    d.shown_ok = true;
    assert!(!d.sms_delete_tap(0), "not armed");
    assert!(d.sms_delete_arm(1));
    assert!(d.sms_delete_armed(1) && !d.sms_delete_armed(0));
    assert_eq!(d.sms_armed_index(), 1);
    assert!(!d.sms_delete_tap(1), "inside the debounce: the long press's own release");
    assert!(!d.sms_delete_arm(5));
}

/// `sim.dual` (global edition): both slots, null `ready` on the one that is off; absent = one slot.
#[test]
fn sim_dual_parse() {
    let dual = r#"{"state":"ready","iccid":"8986","dual":{"smart":false,"mode":"single","active_slot":1,"switching":false,"slots":[{"slot":1,"provisioned":true,"present":true,"ready":true,"operator":"CUCC","mcc":"460","mnc":"01","iccid_tail":"1111"},{"slot":2,"provisioned":false,"present":true,"ready":null,"operator":"CMCC","mcc":"","mnc":"","iccid_tail":"2222"}]},"spn":"x"}"#;
    let d = super::sim_dual(&crate::json::get(dual.as_bytes(), "dual", 2048).unwrap()).unwrap();
    assert!(d.mode == "single" && d.smart == 0 && d.active_slot == 1 && d.switching == 0 && d.slots.len() == 2);
    let (a, b) = (&d.slots[0], &d.slots[1]);
    assert!(a.slot == 1 && a.provisioned == 1 && a.ready == 1 && a.operator == "CUCC" && a.mnc == "01" && a.iccid_tail == "1111");
    assert!(b.slot == 2 && b.provisioned == 0 && b.present == 1 && b.ready == -1 && b.mcc.is_empty());
    // the top-level keys of sim are not taken from inside dual
    assert_eq!(crate::data::util::jget(dual.as_bytes(), "spn", 32), "x");
    assert!(crate::json::get(br#"{"state":"ready"}"#, "dual", 2048).is_none());
    assert!(super::sim_dual(b"null").is_none());
}

/// core 1.14 slot fields: state, role and the optional per-slot signal.
#[test]
fn sim_dual_parse_signal() {
    let dual = r#"{"dual":{"active_slot":1,"mode":"dual","slots":[{"iccid_tail":"3668","operator":"CUCC","present":true,"provisioned":true,"ready":true,"role":"data","signal":{"at":1792542594,"bars":5,"rat":"nr","rsrp":-84,"rsrq":-11,"snr":"18.7"},"slot":1,"state":"ready"},{"operator":"CMCC","present":true,"provisioned":true,"ready":true,"role":"standby","slot":2,"state":"ready"}],"smart":false,"switching":false}}"#;
    let d = super::sim_dual(&crate::json::get(dual.as_bytes(), "dual", 2048).unwrap()).unwrap();
    let (a, b) = (&d.slots[0], &d.slots[1]);
    assert!(a.role == "data" && a.state == "ready" && a.has_sig == 1 && a.sig_rat == "nr");
    assert!(a.sig_rsrp == -84 && a.sig_rsrq == -11 && a.sig_snr == "18.7" && a.sig_bars == 5);
    assert!(b.role == "standby" && b.has_sig == 0 && b.sig_snr.is_empty());
}

/// The whole old-/state parse of one full snapshot, pinned as text: caps, escapes, the operator
/// override, nested lists, sim.dual. LUYAZ_GOLDEN=write regenerates the expected file.
#[test]
fn full_snapshot_golden() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/");
    let src = std::fs::read(format!("{dir}snapshot-full.json")).unwrap();
    let got = format!("{:#?}\n", decode_snapshot(&src).unwrap());
    let path = format!("{dir}snapshot-full.golden");
    if std::env::var("LUYAZ_GOLDEN").as_deref() == Ok("write") {
        std::fs::write(&path, &got).unwrap();
    }
    assert_eq!(got, std::fs::read_to_string(&path).unwrap());
}

#[test]
fn clients_keep_lanid_fields_and_a_full_list() {
    let one = r#"{"alias":"Living-room TV with a long name","ip":"192.168.0.123","kind":"tv","mac":"02:00:00:00:00:aa","name":"Living-room TV with a long name","private":true,"vendor":"Example Electronics Co"}"#;
    assert!(one.len() >= 160);
    let list = vec![one; 16].join(",");
    let s = format!(r#"{{"total":16,"wifi":16,"lan":0,"list":[{list}]}}"#);
    let mut d = core_snapshot_t { lan: vec![lan_client_t::default(); 16], ..Default::default() };
    read_clients(s.as_bytes(), &mut d);
    assert_eq!(d.lan_n, 16);
    let c = &d.lan[15];
    assert_eq!((c.alias.as_str(), c.vendor.as_str(), c.kind.as_str(), c.private), ("Living-room TV with a long name", "Example Electronics Co", "tv", true));
    // an old core: no lanid fields, as before
    read_clients(br#"{"total":1,"list":[{"name":"n","ip":"1","mac":"m"}]}"#, &mut d);
    assert!(d.lan_n == 1 && d.lan[0].alias.is_empty() && !d.lan[0].private);
}

#[test]
fn background_contact_keeps_pumping_during_a_delayed_handshake() {
    use std::time::Instant;
    let (listener, port) = crate::data::testsrv::listen();
    let h = crate::data::testsrv::serve(listener, 2, |c, i| {
        let request = crate::data::testsrv::read_req(c);
        std::thread::sleep(Duration::from_millis(180));
        if i == 0 {
            assert!(request.starts_with("GET /v2/state "));
            c.write_all(format!("HTTP/1.0 200 OK\r\n\r\n{SNAP}").as_bytes()).unwrap();
        } else {
            assert!(request.starts_with("GET /v2/events "));
            c.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n").unwrap();
            // Keep the stream alive until the test has collected the handshake.
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    let mut core = CoreLink::background(cfg(port));
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut passes = 0;
    while !core.shown_ok {
        let at = Instant::now();
        core.poll(mono_ms32());
        core.commit_latest();
        assert!(at.elapsed() < Duration::from_millis(100), "UI pump waited on contact");
        assert!(Instant::now() < deadline);
        passes += 1;
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(passes >= 20, "the UI kept running before data arrived");
    assert_eq!(core.refresh().1.carrier, "X");
    core.close();
    h.join().unwrap();
}

/// The regress block (design review 4A/14A): absent on an older core, never read yet, fresh, then
/// stale with the last rows kept and the time they came.
#[test]
fn regress_block_absent_never_fresh_and_stale() {
    let snap = |extra: &str| format!(r#"{{"epoch":"e1","seq":1,"blocks":{{{}{extra}}}}}"#, blk("battery", L_BAT));
    let mut d = CoreLink::new(cfg(1));
    assert_eq!(feed(&mut d, "snapshot", &snap("")), 1);
    assert!(d.fresh_snap.regress.is_none(), "an older core: no block, no incident section data");
    let never = r#","regress":{"revision":0,"observed_at":0,"stale":true,"data":null}"#;
    assert_eq!(feed(&mut d, "snapshot", &snap(never)), 1);
    let r = d.fresh_snap.regress.clone().unwrap();
    assert!(r.has == 0 && r.fresh == 0 && r.open.is_empty());
    let data = r#"{"open_total":6,"open":[{"id":"mem-abs-1","kind":"regress-mem-abs","name_zh":"内存超标","name_en":"High memory","sub":"","from":1792800000,"to":null,"end":"","count":1,"observed_zh":"","observed_en":""}],"closed":[{"id":"q3","kind":"core-crash","name_zh":"数据服务崩溃","name_en":"Data service crash","sub":"","from":1792790000,"to":1792795000,"end":"quiet","count":3,"observed_zh":"原厂动作：redial","observed_en":"stock firmware: redial"}]}"#;
    assert_eq!(feed(&mut d, "snapshot", &snap(&format!(",{}", blk("regress", data)))), 1);
    let r = d.fresh_snap.regress.clone().unwrap();
    assert!(r.fresh == 1 && r.has == 1 && r.open_total == 6 && r.at > 0);
    assert_eq!((r.open[0].name.as_str(), r.open[0].to, r.closed[0].end.as_str(), r.closed[0].count), ("内存超标", 0, "quiet", 3));
    assert_eq!(r.closed[0].observed, "原厂动作：redial");
    let at = r.at;
    // stale: the last rows stay, greyed by the page, with when they came
    let ev = format!(r#"{{"epoch":"e1","seq":2,"name":"regress","revision":2,"observed_at":2,"stale":true,"data":{data}}}"#);
    assert_eq!(feed(&mut d, "block", &ev), 1);
    let r = d.fresh_snap.regress.clone().unwrap();
    assert!(r.fresh == 0 && r.has == 1 && r.open.len() == 1 && r.at == at);
}

/// The display block (core plug::display): a card, the line after it, nothing.
#[test]
fn display_card_and_the_line_after() {
    let d = super::display(br#"{"card":{"id":"p1","kind":"pair","code":"482913","left_s":28,"answered":false},"said":null}"#);
    assert!(d.id == "p1" && d.code == "482913" && d.left_s == 28 && d.answered == 0 && d.said.is_empty());
    let d = super::display(br#"{"card":{"id":"p1","kind":"pair","code":"482913","left_s":3,"answered":true},"said":null}"#);
    assert_eq!(d.answered, 1);
    let d = super::display(br#"{"card":null,"said":{"id":"p1","result":"done"}}"#);
    assert!(d.id.is_empty() && d.said == "done" && d.said_id == "p1");
    assert_eq!(super::display(br#"{"card":null,"said":null}"#), ui_model::display_t::default());
}

/// Heat care (core `heat` block, thermal-care §4.3): the level and the pause; no block = -1 (an
/// older core: the screen falls back to the battery temperature alone).
#[test]
fn heat_block_parses_level_and_pause() {
    let d = decode_snapshot(br#"{"heat":{"level":"hot","charge_paused":true,"chill":"hot","chill_now":"hot"}}"#).unwrap();
    assert_eq!((d.heat_level, d.heat_paused, d.heat_chill), (2, 1, 1));
    let d = decode_snapshot(br#"{"heat":{"level":"critical","charge_paused":true,"chill":"critical","chill_now":""}}"#).unwrap();
    assert_eq!(d.heat_chill, 0, "asked for, not done");
    let d = decode_snapshot(br#"{"heat":{"level":"critical","charge_paused":false}}"#).unwrap();
    assert_eq!((d.heat_level, d.heat_paused), (3, 0));
    let d = decode_snapshot(br#"{"heat":{"level":"normal"}}"#).unwrap();
    assert_eq!(d.heat_level, 0);
    let d = decode_snapshot(br#"{"battery":{}}"#).unwrap();
    assert_eq!(d.heat_level, -1);
}

/// The 5 GHz channel guard (core `wifi_dfs` block): the worst state wins; no block = -1.
#[test]
fn wifi_dfs_block_parses_the_state() {
    let d = decode_snapshot(br#"{"wifi_dfs":{"want_5g":true,"up":true,"channel":36,"width":160,"on_dfs":true,"down_since":null,"override_at":null}}"#).unwrap();
    assert_eq!((d.ap5_dfs, d.ap5_ch, d.ap5_width), (1, 36, 160));
    let d = decode_snapshot(br#"{"wifi_dfs":{"want_5g":true,"up":false,"channel":null,"on_dfs":null,"down_since":100,"override_at":50}}"#).unwrap();
    assert_eq!((d.ap5_dfs, d.ap5_ch), (2, 0));
    let d = decode_snapshot(br#"{"wifi_dfs":{"want_5g":true,"up":true,"channel":36,"width":80,"on_dfs":false,"down_since":null,"override_at":50}}"#).unwrap();
    assert_eq!(d.ap5_dfs, 3);
    let d = decode_snapshot(br#"{"wifi_dfs":{"want_5g":false,"up":false,"down_since":null,"override_at":50}}"#).unwrap();
    assert_eq!(d.ap5_dfs, 0);
    let d = decode_snapshot(br#"{"battery":{}}"#).unwrap();
    assert_eq!(d.ap5_dfs, -1);
}
