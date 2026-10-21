//! tests/data_test.c: noticing that core went away after having answered (a fake core
//! answers /v2/state and /v2/events, then goes silent with the stream open, then a fresh one
//! comes back); retries paced, across the 32-bit ms wrap; the /v2 blocks put back together read
//! exactly like the old /state; and /control parked until core answers.
// SPDX-License-Identifier: MIT

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
        let d = parse_snapshot(format!(r#"{{"interfaces":{{"cellular":{cell}}},"ts":1}}"#).as_bytes()).unwrap();
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
    let want = parse_snapshot(legacy.as_bytes()).unwrap();
    assert!(want.sms_n == 2 && want.bat_percent == 77 && want.clients_total == 2);
    assert!(want.usb_cc == 1 && want.powerbank == 0 && want.usb_power_role == "source" && want.usb_data_role == "host");
    assert_eq!(want.sms[0].text, "测试 \"q\"");
    assert_eq!(want.operator_name, "中国移动");
    assert_eq!(want.ambr_dl, 1000.5);
    assert!(want.qos_bearer == "nr_pdu" && want.qos_stale == 1);
    assert!(want.dps_mode == 0 && want.cell_data == 1 && want.cell_roam == 0);

    let mut d = CoreLink::new(cfg(1));
    // the snapshot event: every field the screen reads, byte for byte
    assert_eq!(feed(&mut d, "snapshot", &snap), 1);
    assert!(d.have_seq && d.seq == 41 && d.epoch == "e7");
    assert_eq!(parse_snapshot(&d.live_json).unwrap(), want);
    // GET /v2/state carries the same: same result, the stream position untouched
    assert_eq!(d.v2_apply_snapshot(snap.as_bytes(), false), 0);
    assert_eq!(d.seq, 41);

    // a block event moves one block; a heartbeat only seq
    assert_eq!(
        feed(&mut d, "block", r#"{"epoch":"e7","seq":42,"name":"battery","revision":2,"observed_at":2,"stale":false,"data":{"percent":76}}"#),
        1
    );
    assert!(d.live_data.bat_percent == 76 && d.live_data.sms_n == 2);
    assert_eq!(feed(&mut d, "heartbeat", r#"{"epoch":"e7","seq":43,"blocks":{},"exec_age_ms":0}"#), 0);
    // a block the screen does not read
    assert_eq!(feed(&mut d, "block", r#"{"epoch":"e7","seq":44,"name":"op","revision":2,"observed_at":2,"stale":false,"data":{}}"#), 0);

    // stale: left out like a failed read in the old /state, not the kept value
    let ev = format!(r#"{{"epoch":"e7","seq":45,"name":"clients","revision":2,"observed_at":1,"stale":true,"data":{L_CLI}}}"#);
    assert_eq!(feed(&mut d, "block", &ev), 1);
    assert!(d.live_data.clients_total == 0 && d.live_data.client_n == 0);
    let ev = format!(r#"{{"epoch":"e7","seq":46,"name":"sms_list","revision":2,"observed_at":1,"stale":true,"data":{{"list":{L_LIST}}}}}"#);
    assert_eq!(feed(&mut d, "block", &ev), 1);
    assert!(d.live_data.sms_n == 0 && d.live_data.sms_unread == 1);
    // typec stale: unknown (-1), not "nothing plugged in"
    let ev = format!(r#"{{"epoch":"e7","seq":47,"name":"typec","revision":2,"observed_at":1,"stale":true,"data":{L_TYPEC}}}"#);
    assert_eq!(feed(&mut d, "block", &ev), 1);
    assert!(d.live_data.usb_cc == -1 && d.live_data.powerbank == 0);
    assert!(!d.resync);

    // a gap, another epoch, or a block before any snapshot: resync, nothing applied
    assert_eq!(
        feed(&mut d, "block", r#"{"epoch":"e7","seq":49,"name":"battery","revision":3,"observed_at":3,"stale":false,"data":{"percent":10}}"#),
        0
    );
    assert!(d.resync && d.live_data.bat_percent == 76);
    d.resync = false;
    assert_eq!(feed(&mut d, "heartbeat", r#"{"epoch":"e8","seq":48,"blocks":{}}"#), 0);
    assert!(d.resync);
    d.resync = false;
    d.have_seq = false;
    assert_eq!(feed(&mut d, "block", r#"{"epoch":"e7","seq":47,"name":"battery","stale":false,"data":{"percent":10}}"#), 0);
    assert!(d.resync && d.live_data.bat_percent == 76);
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
    assert!(ok && s.net_type == "NR5G_SA");
    let alive = d.alive_wall();
    assert!(alive > 0);
    assert!(!d.silent());

    // silent past silent_ms with the stream still open: reported silent; the last snapshot is
    // still handed out (the screen dims it)
    spin(&mut d, 700);
    assert!(d.silent());
    let (ok, s) = d.refresh();
    assert!(ok && s.net_type == "NR5G_SA");
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
        d.live_valid = true;
        d.current_valid = true;
        d.next_retry_ms = due;
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
}

#[test]
fn sms_delete_needs_two_steps() {
    let mut d = CoreLink::new(cfg(1));
    d.current_data = parse_snapshot(format!(r#"{{"sms":{{"unread":1,"list":{L_LIST}}}}}"#).as_bytes()).unwrap();
    d.current_valid = true;
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
