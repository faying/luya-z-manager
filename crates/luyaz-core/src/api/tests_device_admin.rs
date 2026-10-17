//! Batch 5c: the device-admin routes (reads open, writes and the AT port behind `device_admin`),
//! and the barriers that keep scheduled jobs and scenarios away from the dangerous ones.

use super::device_admin::{Host, procs};
use super::tests_ops::start;
use super::*;
use std::fs;
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

type Log = Arc<Mutex<Vec<Value>>>;

/// A `/control` that logs every request and answers ok, or `fail` for the actions named there.
async fn control_server(log: Log, fail: &'static [&'static str]) -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let app = axum::Router::new().route(
        "/control",
        post(move |body: Bytes| {
            let log = log.clone();
            async move {
                let req: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                log.lock().unwrap().push(req.clone());
                let action = req["action"].as_str().unwrap_or("");
                let method = req["params"]["method"].as_str().unwrap_or("");
                if fail.contains(&action) || fail.contains(&method) {
                    return reply(503, json!({"ok": false, "error": {"code": "failed", "message": "vendor said no"}}));
                }
                reply(200, json!({"ok": true, "action": action, "result": {"fake": true}}))
            }
        }),
    );
    tokio::spawn(async move { axum::serve(l, app).await });
    format!("http://{addr}/control")
}

/// The fake device: these ubus answers, everything else fails.
fn ubus(o: &str, m: &str, a: &Value) -> Result<Value, String> {
    Ok(match (o, m) {
        ("system", "info") => json!({"uptime": 42}),
        ("zwrt_mc.device.manager", "get_device_info") => {
            if a["deviceInfoList"] == json!(["quicken_power_on"]) {
                json!({"quicken_power_on": "1"})
            } else {
                json!({"asked": a})
            }
        }
        ("zwrt_zte_mdm.api", "get_sim_info") => json!({"sim_states": "modem_init_complete"}),
        ("zwrt_zte_mdm.api", "get_imei") => json!({"imei": "860000000000000"}),
        ("zwrt_zte_mdm.api", "get_simlock_available_trials") => json!({"trials": 5}),
        ("zte_nwinfo_api", "nwinfo_get_stc_white_list_par") => json!({"par": 1}),
        ("zte_nwinfo_api", "nwinfo_get_stc_white_list_status") => json!({"status": 0}),
        ("zwrt_router.api", "router_get_dns_para") => {
            json!({"wan_dns_mode": "manual", "wan_prefer_dns_manual": "", "wan_standby_dns_manual": ""})
        }
        ("uci", "get") if a["config"] == "network" => json!({"value": ["9.9.9.9", "1.1.1.1"]}),
        _ => return Err(format!("{o} {m}: not faked")),
    })
}

struct Rig {
    root: std::path::PathBuf,
    log: Log,
    killed: Arc<Mutex<Vec<u32>>>,
    addr: std::net::SocketAddr,
}

async fn rig(name: &str, writes: bool, at: Option<crate::at::Target>) -> Rig {
    rig_failing(name, writes, at, &[]).await
}

async fn rig_failing(
    name: &str,
    writes: bool,
    at: Option<crate::at::Target>,
    fail: &'static [&'static str],
) -> Rig {
    let root = std::env::temp_dir().join(format!("u60d-admin-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let log: Log = Arc::default();
    let url = control_server(log.clone(), fail).await;
    let killed: Arc<Mutex<Vec<u32>>> = Arc::default();
    let k = killed.clone();
    let src = Sources {
        ubus: Arc::new(|o, m, a| {
            let r = ubus(o, m, &a);
            Box::pin(async move { r })
        }),
        control: Arc::new(control::Client::new(url, None)),
        ..Sources::none()
    };
    let host = Host {
        at,
        kill: Arc::new(move |pid| {
            k.lock().unwrap().push(pid);
            true
        }),
    };
    let c = Compat::new(root.to_str().unwrap(), src)
        .with_writes(writes)
        .with_admin(host);
    let addr = start(c).await;
    Rig {
        root,
        log,
        killed,
        addr,
    }
}

async fn call_as(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    source: Option<&str>,
    body: &str,
) -> (u16, Value) {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let header = source
        .map(|v| format!("{}: {v}\r\n", luyaz_api::headers::SOURCE_HEADER))
        .unwrap_or_default();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n{header}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    let status = out[9..12].parse().unwrap();
    let json = out.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
    (status, serde_json::from_str(json).unwrap_or(Value::Null))
}

async fn call(addr: std::net::SocketAddr, method: &str, path: &str, body: &str) -> (u16, Value) {
    call_as(addr, method, path, None, body).await
}

/// Every gated route (method, path, a body it accepts).
const GATED: &[(&str, &str, &str)] = &[
    ("PUT", "/api/device/fast-boot", r#"{"fast_boot":"1"}"#),
    ("POST", "/api/device/poweroff", ""),
    ("POST", "/api/device/factory-reset", ""),
    ("POST", "/api/sim/pin/verify", r#"{"pin_num":"1234"}"#),
    (
        "POST",
        "/api/sim/pin/change",
        r#"{"old_pin":"1","new_pin":"2"}"#,
    ),
    ("POST", "/api/sim/pin/mode", r#"{"pin_mode":"1"}"#),
    ("POST", "/api/sim/unlock", r#"{"nck":"1"}"#),
    ("PUT", "/api/cell/stc/params", r#"{"x":1}"#),
    ("POST", "/api/cell/stc/enable", ""),
    ("POST", "/api/cell/stc/disable", ""),
    ("POST", "/api/cell/stc/reset", ""),
    ("PUT", "/api/router/dns", r#"{"dns_mode":"auto"}"#),
    ("PUT", "/api/usb/mode", r#"{"mode":"share"}"#),
    ("POST", "/api/system/kill-bloat", r#"{"all":true}"#),
    ("GET", "/api/at/port", ""),
    ("POST", "/api/at/send", r#"{"command":"AT"}"#),
    ("POST", "/api/ussd/send", r#"{"code":"*100#"}"#),
    ("POST", "/api/ussd/respond", r#"{"reply":"1"}"#),
    ("POST", "/api/ussd/cancel", ""),
    ("GET", "/api/stk/menu", ""),
    ("POST", "/api/stk/select", r#"{"item_id":1}"#),
];

#[tokio::test]
async fn the_reads_are_served_with_writes_off() {
    // writes off
    let r = rig("reads", false, None).await;
    for (m, p, body, want) in [
        (
            "GET",
            "/api/device/fast-boot",
            "",
            json!({"fast_boot": "1"}),
        ),
        (
            "POST",
            "/api/device/power-save",
            r#"{"deviceInfoList":["power_save"]}"#,
            json!({"asked": {"deviceInfoList": ["power_save"]}}),
        ),
        (
            "GET",
            "/api/sim/info",
            "",
            json!({"sim_states": "modem_init_complete"}),
        ),
        ("GET", "/api/sim/lock-trials", "", json!({"trials": 5})),
        ("GET", "/api/cell/stc/params", "", json!({"par": 1})),
        ("GET", "/api/cell/stc/status", "", json!({"status": 0})),
        (
            "GET",
            "/api/router/dns",
            "",
            json!({"dns_mode": "manual", "prefer_dns_manual": "9.9.9.9", "standby_dns_manual": "1.1.1.1"}),
        ),
    ] {
        let (st, b) = call(r.addr, m, p, body).await;
        assert_eq!((st, &b["data"]), (200, &want), "{m} {p}: {b}");
    }
    let (st, b) = call(r.addr, "POST", "/api/device/power-save", "nope").await;
    assert_eq!((st, b["error"].as_str()), (400, Some("invalid JSON")));
    assert!(r.log.lock().unwrap().is_empty(), "a read wrote");
}

#[tokio::test]
async fn the_writes_are_not_served_with_writes_off() {
    let r = rig("gate-off", false, None).await;
    for (m, p, body) in GATED {
        let (st, b) = call(r.addr, m, p, body).await;
        assert_eq!(
            (st, b["error"].as_str()),
            (404, Some("not_in_compat_yet")),
            "{m} {p}: {b}"
        );
    }
    assert!(r.log.lock().unwrap().is_empty(), "wrote");
    assert!(r.killed.lock().unwrap().is_empty(), "killed");
}

#[tokio::test]
async fn power_save_post_is_the_read_put_stays_the_scenarios() {
    // POST reads, PUT is the scenario engine's (503 until it has started)
    let r = rig("ps-admin", true, None).await;
    let (st, b) = call(r.addr, "POST", "/api/device/power-save", "{}").await;
    assert_eq!((st, b["data"]["asked"].clone()), (200, json!({})));
    let (st, b) = call(r.addr, "PUT", "/api/device/power-save", r#"{"x":1}"#).await;
    assert_eq!(
        (st, b["error_en"].as_str()),
        (503, Some("the scenario engine has not started")),
        "{b}"
    );
    assert!(r.log.lock().unwrap().is_empty());
    // writes off: PUT is "not here", not a bare 405
    let r = rig("ps-off", false, None).await;
    for (m, p) in [
        ("PUT", "/api/device/power-save"),
        ("DELETE", "/api/device/fast-boot"),
        ("GET", "/api/device/poweroff"),
    ] {
        let (st, b) = call(r.addr, m, p, "{}").await;
        assert_eq!(
            (st, b["error"].as_str()),
            (404, Some("not_in_compat_yet")),
            "{m} {p}"
        );
    }
    let (st, _) = call(r.addr, "GET", "/api/device/fast-boot", "").await;
    assert_eq!(st, 200);
}

#[tokio::test]
async fn the_writes_go_through_control_as_the_callers_source() {
    let r = rig("writes", true, None).await;
    let vendor = |o: &str, m: &str, a: Value| json!({"action": "vendor.call", "params": {"object": o, "method": m, "args": a}});
    let cases: Vec<(&str, &str, &str, Value)> = vec![
        (
            "PUT",
            "/api/device/fast-boot",
            r#"{"fast_boot":"1"}"#,
            vendor(
                "zwrt_mc.device.manager",
                "set_device_info",
                json!({"deviceInfoList": {"quicken_power_on": "1"}}),
            ),
        ),
        (
            "POST",
            "/api/sim/pin/verify",
            r#"{"pin_num":"1234","puk_num":"","pin_encode_flag":"0"}"#,
            vendor(
                "zwrt_zte_mdm.api",
                "sim_verify_pin_puk",
                json!({"pin_num": "1234", "puk_num": "", "pin_encode_flag": "0"}),
            ),
        ),
        (
            "POST",
            "/api/sim/pin/change",
            r#"{"a":1}"#,
            vendor("zwrt_zte_mdm.api", "sim_change_pin", json!({"a": 1})),
        ),
        (
            "POST",
            "/api/sim/pin/mode",
            r#"{"a":1}"#,
            vendor("zwrt_zte_mdm.api", "sim_change_pin_mode", json!({"a": 1})),
        ),
        (
            "POST",
            "/api/sim/unlock",
            r#"{"a":1}"#,
            vendor("zwrt_zte_mdm.api", "set_simlock_nck", json!({"a": 1})),
        ),
        (
            "PUT",
            "/api/cell/stc/params",
            r#"{"a":1}"#,
            vendor(
                "zte_nwinfo_api",
                "nwinfo_set_stc_white_list_par",
                json!({"a": 1}),
            ),
        ),
        (
            "POST",
            "/api/cell/stc/enable",
            "",
            vendor("zte_nwinfo_api", "nwinfo_stc_cell_lock_enable", json!({})),
        ),
        (
            "POST",
            "/api/cell/stc/disable",
            "",
            vendor("zte_nwinfo_api", "nwinfo_stc_cell_lock_disable", json!({})),
        ),
        (
            "POST",
            "/api/cell/stc/reset",
            "",
            vendor("zte_nwinfo_api", "nwinfo_stc_cell_lock_reset", json!({})),
        ),
        (
            "PUT",
            "/api/router/dns",
            r#"{"dns_mode":"auto"}"#,
            vendor(
                "zwrt_router.api",
                "router_set_wan_dns",
                json!({"dns_mode": "auto"}),
            ),
        ),
        (
            "PUT",
            "/api/usb/mode",
            r#"{"usb_network_protocal":2,"mode":"share"}"#,
            json!({"action": "usb.set", "params": {"mode": "share", "network_protocol": "2"}}),
        ),
    ];
    for (m, p, body, want) in cases {
        r.log.lock().unwrap().clear();
        let (st, b) = call_as(r.addr, m, p, Some("web"), body).await;
        assert_eq!(st, 200, "{m} {p}: {b}");
        let log = r.log.lock().unwrap().clone();
        assert_eq!(log.len(), 1, "{m} {p}: {log:?}");
        assert_eq!(log[0]["action"], want["action"], "{m} {p}");
        assert_eq!(log[0]["params"], want["params"], "{m} {p}");
        assert_eq!(log[0]["source"], "web", "{m} {p}");
    }
    // fast-boot answers the value, as the agent's
    let (_, b) = call(
        r.addr,
        "PUT",
        "/api/device/fast-boot",
        r#"{"fast_boot":"0"}"#,
    )
    .await;
    assert_eq!(b["data"], json!({"fast_boot": "0"}));
    // no header: the screen
    assert_eq!(r.log.lock().unwrap().last().unwrap()["source"], "screen");
    // a bad body never reaches /control
    r.log.lock().unwrap().clear();
    for (m, p, body, want) in [
        ("PUT", "/api/device/fast-boot", r#"{"fast_boot":"2"}"#, 400),
        ("PUT", "/api/device/fast-boot", "x", 400),
        ("POST", "/api/sim/pin/verify", "x", 400),
        ("PUT", "/api/usb/mode", "{}", 400),
        ("PUT", "/api/usb/mode", r#"{"mode":true}"#, 400),
    ] {
        let (st, b) = call(r.addr, m, p, body).await;
        assert_eq!(st, want, "{m} {p} {body}: {b}");
    }
    assert!(r.log.lock().unwrap().is_empty());
}

#[test]
fn usb_and_dns_mapping_is_the_agents() {
    use super::device_admin::{dns_data, usb_params};
    assert_eq!(
        usb_params(&json!({"usb_port_switch": "1", "port_switch": "0"})).unwrap(),
        json!({"port_switch": "1"}),
        "the vendor's name, later in the agent's list, wins"
    );
    assert_eq!(usb_params(&json!({"x": 1})).unwrap_err().0, 400);
    // not manual: nothing filled; a CLI line fills both
    let auto = dns_data(
        &json!({"wan_dns_mode": "auto", "wan_prefer_dns_manual": ""}),
        None,
    );
    assert_eq!(auto, json!({"dns_mode": "auto", "prefer_dns_manual": ""}));
    let line = dns_data(
        &json!({"wan_dns_mode": "manual", "wan_prefer_dns_manual": ""}),
        Some(&json!("8.8.8.8")),
    );
    assert_eq!(line["prefer_dns_manual"], "8.8.8.8");
    assert!(line.get("standby_dns_manual").is_none());
}

#[tokio::test]
async fn factory_reset_and_power_off_are_the_stock_calls_and_say_when_they_fail() {
    let r = rig("power", true, None).await;
    let (st, b) = call_as(r.addr, "POST", "/api/device/factory-reset", Some("web"), "").await;
    assert_eq!(st, 200, "{b}");
    let (st, b) = call_as(r.addr, "POST", "/api/device/poweroff", Some("web"), "").await;
    assert_eq!((st, b["data"]["fake"].clone()), (200, json!(true)), "{b}");
    let log = r.log.lock().unwrap().clone();
    assert_eq!(
        log,
        vec![
            json!({"action": "vendor.call", "source": "web", "params": {"object": "zwrt_mc.device.manager",
                "method": "device_reset", "args": {"moduleName": "web", "resetFlag": 0}}}),
            json!({"action": "device.poweroff", "source": "web", "params": {}}),
        ]
    );
    // a failed vendor call: 503 with its error, and power off has no second way
    let r = rig_failing(
        "power-fail",
        true,
        None,
        &["device.poweroff", "device_reset"],
    )
    .await;
    let (st, b) = call(r.addr, "POST", "/api/device/factory-reset", "").await;
    assert_eq!(
        (st, b["error"].as_str()),
        (503, Some("vendor said no")),
        "{b}"
    );
    let (st, b) = call(r.addr, "POST", "/api/device/poweroff", "").await;
    assert_eq!((st, b["ok"].clone()), (503, json!(false)), "{b}");
    assert_eq!(r.log.lock().unwrap().len(), 2, "one call each, no fallback");
}

/// A /proc with these `(pid, argv0, comm, utime, rss pages)` and a total CPU line.
fn fake_proc(root: &std::path::Path, total: u64, procs: &[(u32, &str, &str, u64, u64)]) {
    let p = root.join("proc");
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    fs::write(
        p.join("stat"),
        format!("cpu  {total} 0 0 0 0 0 0\ncpu0 1 0 0 0\n"),
    )
    .unwrap();
    for (pid, argv0, comm, ut, rss) in procs {
        let d = p.join(pid.to_string());
        fs::create_dir_all(&d).unwrap();
        let mut f: Vec<String> = vec!["S".into()];
        f.extend((1..22).map(|i| match i {
            11 => ut.to_string(),
            21 => rss.to_string(),
            _ => "0".into(),
        }));
        fs::write(d.join("stat"), format!("{pid} ({comm}) {}", f.join(" "))).unwrap();
        // a kernel thread's (or a zombie's) cmdline is empty: the name is then comm
        let cmdline = if argv0.is_empty() {
            String::new()
        } else {
            format!("{argv0}\0-x\0")
        };
        fs::write(d.join("cmdline"), cmdline).unwrap();
    }
}

#[tokio::test]
async fn kill_bloat_kills_only_the_listed_daemons() {
    let r = rig("bloat", true, None).await;
    fake_proc(
        &r.root,
        1000,
        &[
            (10, "/usr/bin/zte_topsw_tr069", "tr069", 5, 100),
            (11, "/usr/bin/zte_topsw_mc", "zte_topsw_mc", 5, 100),
            (12, "", "zte_dua", 5, 50),
            (13, "/data/zte-agent", "zte-agent", 5, 10),
        ],
    );
    let (st, b) = call(r.addr, "POST", "/api/system/kill-bloat", r#"{"all":true}"#).await;
    assert_eq!(st, 200, "{b}");
    let mut killed = r.killed.lock().unwrap().clone();
    killed.sort();
    assert_eq!(killed, vec![10, 12]);
    assert_eq!(b["data"]["freed_rss_kb"], 600);
    // asked by pid: a sync-barrier daemon and an unknown pid are never signalled
    r.killed.lock().unwrap().clear();
    let (_, b) = call(
        r.addr,
        "POST",
        "/api/system/kill-bloat",
        r#"{"pids":[11,13,99,10]}"#,
    )
    .await;
    assert_eq!(*r.killed.lock().unwrap(), vec![10]);
    assert_eq!(
        b["data"]["skipped"],
        json!([{"pid": 11, "name": "zte_topsw_mc"}, {"pid": 13, "name": "zte-agent"}])
    );
    for body in ["{}", r#"{"pids":[]}"#, "x"] {
        let (st, _) = call(r.addr, "POST", "/api/system/kill-bloat", body).await;
        assert_eq!(st, 400, "{body}");
    }
}

#[test]
fn the_bloat_list_never_names_a_sync_barrier_daemon() {
    for d in procs::BLOAT {
        assert!(!procs::SYNC_BARRIER.contains(d), "{d} is in daemon.conf");
    }
}

#[tokio::test]
async fn top_is_a_read_with_cpu_since_the_last_call() {
    let r = rig("top", false, None).await;
    fake_proc(
        &r.root,
        1000,
        &[
            (10, "/usr/bin/zte_dua", "zte_dua", 100, 10),
            (20, "/bin/sh", "sh", 0, 5),
        ],
    );
    let _ = call(r.addr, "GET", "/api/system/top", "").await;
    fake_proc(
        &r.root,
        1100,
        &[
            (10, "/usr/bin/zte_dua", "zte_dua", 125, 10),
            (20, "/bin/sh", "sh", 0, 5),
        ],
    );
    let (st, b) = call(r.addr, "GET", "/api/system/top", "").await;
    assert_eq!(st, 200);
    let d = &b["data"];
    assert_eq!(
        (d["total_count"].clone(), d["bloat_count"].clone()),
        (json!(2), json!(1)),
        "{d}"
    );
    assert_eq!(
        d["processes"][0],
        json!({"pid": 10, "name": "zte_dua", "cpu_pct": 25.0, "rss_kb": 40,
        "state": "sleeping", "is_bloat": true})
    );
    assert_eq!(d["bloat_rss_kb"], 40);
}

#[tokio::test]
async fn the_at_routes_use_u60ds_port_and_the_agents_shapes() {
    let (port, sent) = crate::at::tests::fake_modem_parts(&[
        (0, "\r\nOK\r\n"),
        (100, "\r\n+CUSD: 0,\"00480069\",72\r\n"),
    ]);
    let t = crate::at::Target {
        lock: None,
        port: Some(port.clone()),
    };
    let r = rig("at", true, Some(t)).await;
    // the terminal: the agent's filter, the port named, op.interrupt first for the web
    let (st, b) = call_as(
        r.addr,
        "POST",
        "/api/at/send",
        Some("web"),
        r#"{"command":"AT+CSQ;$","timeout":2}"#,
    )
    .await;
    assert_eq!(st, 200, "{b}");
    assert_eq!(
        (b["data"]["command"].as_str(), b["data"]["port"].as_str()),
        (Some("AT+CSQ"), Some(port.as_str()))
    );
    assert!(
        b["data"]["response"].as_str().unwrap().contains("OK"),
        "{b}"
    );
    assert_eq!(sent.recv_timeout(Duration::from_secs(1)).unwrap(), "AT+CSQ");
    assert_eq!(
        r.log.lock().unwrap()[0],
        json!({"action": "op.interrupt", "source": "web", "params": {"what": "at"}})
    );
    for body in [
        r#"{"command":"ATI","timeout":1}"#,
        r#"{"command":"rm -rf"}"#,
        r#"{"x":1}"#,
        r#"{"command":";;"}"#,
    ] {
        let (st, _) = call(r.addr, "POST", "/api/at/send", body).await;
        assert!(st == 200 || st == 400, "{body}: {st}");
    }
    // a call naming no source (the screen's): a person's command, interrupted too
    let interrupts: Vec<Value> = r
        .log
        .lock()
        .unwrap()
        .iter()
        .map(|v| v["source"].clone())
        .collect();
    assert_eq!(interrupts, vec![json!("web"), json!("screen")]);
    while sent.try_recv().is_ok() {}
    // a +CFUN= set is refused before the port (owner 10-13), the test form is not a set
    for body in [r#"{"command":"AT+CFUN=0"}"#, r#"{"command":"at+cfun = 4"}"#, r#"{"command":"AT+CF;UN=1,1"}"#] {
        let (st, b) = call(r.addr, "POST", "/api/at/send", body).await;
        assert_eq!(st, 403, "{body}: {b}");
    }
    assert!(sent.recv_timeout(Duration::from_millis(300)).is_err(), "nothing reached the port");
    for c in ["AT+CFUN?", "AT+CFUN=?", "ATI"] {
        assert!(!super::device_admin::phone::radio_set(c), "{c}");
    }
    // calls are refused too (A2); `AT&D2` sanitizes to `ATD2`, a dial: refused on purpose
    // basic commands chain (V.250): a dial after another one is still a dial
    for c in ["ATD10086;", "atd 112", "ata", "AT+CHUP", "AT&D2", "ATE0D10086", "atz d112", "ATE0&D2"] {
        let body = json!({ "command": c }).to_string();
        let (st, b) = call(r.addr, "POST", "/api/at/send", &body).await;
        assert_eq!(st, 403, "{c}: {b}");
    }
    assert!(sent.recv_timeout(Duration::from_millis(300)).is_err(), "nothing reached the port");
    for c in ["AT+CHUP=?", "ATI", "AT+CSQ", "AT+CLCC", "AT&F", "ATE0", "AT+CGDCONT?", "AT^SYSINFO", "AT#ADC"] {
        assert!(!super::device_admin::phone::call_control(c), "{c}");
    }
    // USSD: the +CUSD after the OK, decoded (UCS-2)
    let (st, b) = call(r.addr, "POST", "/api/ussd/send", r#"{"code":"*100#"}"#).await;
    assert_eq!(st, 200, "{b}");
    assert_eq!(b["data"]["response"], "Hi");
    assert_eq!(
        (
            b["data"]["status"].clone(),
            b["data"]["session_active"].clone()
        ),
        (json!(0), json!(false))
    );
    assert_eq!(
        sent.recv_timeout(Duration::from_secs(1)).unwrap(),
        "AT+CUSD=1,\"*100#\",15"
    );
    // the port
    let (st, b) = call(r.addr, "GET", "/api/at/port", "").await;
    assert_eq!(
        (
            st,
            b["data"]["available"].clone(),
            b["data"]["port"].as_str()
        ),
        (200, json!(true), Some(port.as_str()))
    );
}

// ---- the barriers ----

const DANGEROUS: &[(&str, &str)] = &[
    ("POST", "/api/device/factory-reset"),
    ("POST", "/api/device/poweroff"),
    ("POST", "/api/system/kill-bloat"),
    ("POST", "/api/at/send"),
];

/// Each dangerous path, and the ways one might be written to slip past a check.
fn variants(p: &str) -> Vec<String> {
    let (head, last) = p.rsplit_once('/').unwrap();
    vec![
        p.to_string(),
        format!("{p}?x=1"),
        format!("{p}/"),
        p.replacen("/api/", "/api//", 1),
        p.replacen("/api/", "/api///", 1),
        p.to_ascii_uppercase(),
        format!("{head}/./{last}"),
        format!(
            "{head}/{}",
            last.replacen(&last[..1], &format!("%{:02X}", last.as_bytes()[0]), 1)
        ),
    ]
}

#[test]
fn the_scheduler_and_the_scenario_refuse_every_spelling() {
    let schedule = luyaz_policy::scheduler::Schedule::Once { at: 1 };
    for (m, p) in DANGEROUS {
        for v in variants(p) {
            assert!(
                luyaz_policy::scheduler::forbidden(&v).is_some(),
                "scheduler lets {v} through"
            );
            let action = luyaz_policy::scheduler::Action {
                method: (*m).into(),
                path: v.clone(),
                body: None,
            };
            assert!(
                luyaz_policy::scheduler::validate_job(&action, &schedule, &None).is_err(),
                "a job may call {v}"
            );
            let cfg: luyaz_policy::scenario::Config = serde_json::from_value(json!({
                "version": 1,
                "scenarios": [{"id": "x", "name": "x", "detect": {"type": "fallback"},
                    "actions": [{"method": m, "path": v}]}]
            }))
            .unwrap();
            assert!(
                luyaz_policy::scenario::validate(&cfg).is_err(),
                "a scenario may call {v}"
            );
        }
    }
    // the power-save read and the reboot stay allowed where they were
    assert!(luyaz_policy::scheduler::forbidden("/api/device/power-save").is_none());
    assert!(luyaz_policy::scheduler::forbidden("/api/device/reboot").is_none());
}

#[tokio::test]
async fn a_scheduled_job_never_reaches_the_dangerous_routes() {
    let r = rig("sched-barrier", true, None).await;
    fake_proc(&r.root, 1000, &[(10, "/usr/bin/zte_dua", "zte_dua", 1, 1)]);
    let io = crate::engines::scheduler::Http {
        base: format!("http://{}", r.addr),
        token: None,
    };
    // past the job file's own check: straight at u60d's routes, as the job's call would go
    let mut jobs = Vec::new();
    for (i, (m, p)) in DANGEROUS.iter().enumerate() {
        for (j, v) in variants(p).into_iter().enumerate() {
            let id = (i * 10 + j + 1) as u32;
            jobs.push(json!({"id": id, "name": "x", "enabled": true,
                "schedule": {"type": "once", "at": 1_791_774_000 - 30},
                "action": {"method": m, "path": v, "body": {"all": true, "command": "AT+CFUN=0"}},
                "last_run": null, "last_status": null, "last_restore": null, "created_at": 1}));
        }
    }
    let file = r.root.join("data/local/tmp/scheduler.json");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, json!({"jobs": jobs}).to_string()).unwrap();
    let f = file.clone();
    let ran = tokio::task::spawn_blocking(move || {
        crate::engines::scheduler::tick(&f, &io, (1_791_774_000, 3, 0, 0))
    })
    .await
    .unwrap();
    // switched off before anything is due, the reason saved
    assert_eq!(ran, 0);
    let saved: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    let saved = saved["jobs"].as_array().unwrap();
    assert_eq!(saved.len(), jobs.len());
    for j in saved {
        assert_eq!(j["enabled"], false, "{j}");
        assert!(
            j["last_error"].as_str().unwrap().starts_with("已停用"),
            "{j}"
        );
    }
    // and were a variant ever sent, u60d would not serve it: no route matches but the plain path
    for (m, p) in DANGEROUS {
        for v in variants(p).into_iter().skip(1).filter(|v| !v.contains('?')) {
            let (st, b) = call(r.addr, m, &v, r#"{"all":true,"command":"AT"}"#).await;
            assert_eq!(
                (st, b["error"].as_str()),
                (404, Some("not_in_compat_yet")),
                "{m} {v}: {b}"
            );
        }
    }
    assert!(
        r.log.lock().unwrap().is_empty(),
        "{:?}",
        r.log.lock().unwrap()
    );
    assert!(r.killed.lock().unwrap().is_empty());
}
