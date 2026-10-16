//! The screen's writes (step 8d) against a mock `/control` that also plays the modem: what goes to
//! `/control` (action, source, session), what the screen gets back, and what GET /api/netinfo shows.

use super::*;
use std::fs;
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The modem and the APN lists as the fake ubus answers them; `/control` writes change them.
#[derive(Default)]
pub(super) struct Dev {
    serving: (u16, u16),
    pub(super) manual_mode: bool,
    result: String,
    connected: bool,
    /// what netselect.register does: Some(true) takes, Some(false) fails, None hangs
    register_takes: Option<bool>,
    scan_polls: u32,
    pub(super) manual_apns: Vec<Value>,
    /// action → the replies it gets first (then ok)
    script: std::collections::HashMap<String, Vec<(u16, Value)>>,
    pub(super) log: Vec<Value>,
}

pub(super) type Shared = Arc<Mutex<Dev>>;

fn ok(result: Value) -> (u16, Value) {
    (200, json!({"ok": true, "result": result}))
}

/// One `/control` request: logged, scripted or played.
fn control(d: &Shared, req: &Value) -> (u16, Value) {
    let mut d = d.lock().unwrap();
    d.log.push(req.clone());
    let action = req["action"].as_str().unwrap_or("").to_string();
    if let Some(r) = d.script.get_mut(&action).filter(|v| !v.is_empty()) {
        return r.remove(0);
    }
    match action.as_str() {
        "netselect.session.open" => ok(json!({"session": "s1"})),
        "netselect.register" => {
            let t = req["params"]["mcc_mnc"].as_str().unwrap_or("").to_string();
            match d.register_takes {
                Some(true) => {
                    d.serving = (t[..3].parse().unwrap(), t[3..].parse().unwrap());
                    d.manual_mode = true;
                    d.result = "manual_success".into();
                }
                Some(false) => {
                    d.manual_mode = true;
                    d.result = "manual_fail".into();
                }
                None => {}
            }
            ok(json!({}))
        }
        "netselect.auto" => {
            d.manual_mode = false;
            ok(json!({}))
        }
        "cellular.redial" => {
            d.connected = true;
            ok(json!({}))
        }
        "netselect.scan" => {
            d.scan_polls = 2;
            d.connected = false;
            ok(json!({}))
        }
        "apn.add" => {
            let mut p = req["params"].clone();
            let n = d.manual_apns.len() + 1;
            d.manual_apns.push(
                json!({"profileId": format!("manu{n}"), "profilename": p["name"].take(),
                "wanapn": p["apn"].take(), "pdpType": p["pdp_type"].take(), "isEnable": false}),
            );
            ok(json!({}))
        }
        _ => ok(json!({})),
    }
}

pub(super) fn ubus(d: &Shared, obj: &str, m: &str) -> Result<Value, String> {
    let mut d = d.lock().unwrap();
    Ok(match (obj, m) {
        ("zte_nwinfo_api", "nwinfo_get_netinfo") => json!({
            "rmcc": d.serving.0, "rmnc": d.serving.1, "simcard_roam": "Roaming", "network_type": "SA",
            "net_select_mode": if d.manual_mode { "manual_select" } else { "auto_select" }}),
        ("zte_nwinfo_api", "nwinfo_m_netselect_result") => json!({"result": d.result}),
        ("zte_nwinfo_api", "nwinfo_m_netselect_status") => {
            if d.scan_polls > 0 {
                d.scan_polls -= 1;
                json!({"m_netselect_status": "manual_selecting"})
            } else {
                json!({"m_netselect_status": "manual_selected"})
            }
        }
        ("zte_nwinfo_api", "nwinfo_m_netselect_contents") => json!({"m_netselect_contents":
            "2,NTT DOCOMO,DOCOMO,44010,7;1,SoftBank,SoftBank,44020,7"}),
        ("zwrt_data", "get_wwaniface") => json!({"connect_status":
            if d.connected { "ipv4_ipv6_connected" } else { "disconnected" }}),
        ("zwrt_zte_mdm.api", "get_sim_info") => {
            json!({"sim_states": "modem_init_complete", "sim_iccid": "8981000000000000001F"})
        }
        ("zwrt_apn_object", "get_apn_mode") => json!({"apn_mode": 0}),
        ("zwrt_apn_object", "get_auto_apn_list") => json!({"apnListArray": [
            {"profilename": "Docomo", "wanapn": "spmode.ne.jp", "pdpType": 3, "profileId": "auto1", "isEnable": true},
            {"profilename": "Other", "wanapn": "other.example", "pdpType": 3, "profileId": "auto2", "isEnable": false}]}),
        ("zwrt_apn_object", "get_manu_apn_list") => json!({"apnListArray": d.manual_apns}),
        _ => return Err(format!("{obj} {m}: not faked")),
    })
}

/// A mock `/control` on loopback.
pub(super) async fn control_server(d: Shared) -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let app = axum::Router::new()
        .route(
            "/control",
            post(|State(d): State<Shared>, body: Bytes| async move {
                let req: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let (s, v) = control(&d, &req);
                reply(s, v)
            }),
        )
        .with_state(d);
    tokio::spawn(async move { axum::serve(l, app).await });
    format!("http://{addr}/control")
}

async fn setup(name: &str, dev: Dev) -> (std::path::PathBuf, Shared, Compat) {
    let root = std::env::temp_dir().join(format!("u60d-ops-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let d: Shared = Arc::new(Mutex::new(dev));
    let url = control_server(d.clone()).await;
    let d2 = d.clone();
    let src = Sources {
        ubus: Arc::new(move |o, m, _a| {
            let r = ubus(&d2, o, m);
            Box::pin(async move { r })
        }),
        control: Arc::new(control::Client::new(url, None)),
        ..Sources::none()
    };
    let c = Compat::new(root.to_str().unwrap(), src)
        .with_writes(true)
        .with_clock(1000);
    (root, d, c)
}

pub(super) async fn start(c: Compat) -> std::net::SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(serve(l, c));
    addr
}

pub(super) async fn call(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    body: &str,
) -> (u16, Value) {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    let status = out[9..12].parse().unwrap();
    let json = out.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
    (status, serde_json::from_str(json).unwrap_or(Value::Null))
}

/// GET /api/netinfo's `data` until `done` says so (at most 5 s).
pub(super) async fn until(addr: std::net::SocketAddr, done: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..500 {
        let (_, b) = call(addr, "GET", "/api/netinfo?lite=1&apn=1", "").await;
        if done(&b["data"]) {
            return b["data"].clone();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("never got there");
}

/// (action, source, session) of every `/control` request so far.
pub(super) fn brief(d: &Shared) -> Vec<(String, String, Value)> {
    d.lock()
        .unwrap()
        .log
        .iter()
        .map(|v| {
            (
                v["action"].as_str().unwrap_or("").to_string(),
                v["source"].as_str().unwrap_or("").to_string(),
                v["session"].clone(),
            )
        })
        .collect()
}

fn actions(d: &Shared) -> Vec<String> {
    brief(d).into_iter().map(|b| b.0).collect()
}

#[tokio::test]
async fn writes_are_404_until_step_9() {
    let root = std::env::temp_dir().join(format!("u60d-ops-off-{}", std::process::id()));
    let _ = fs::create_dir_all(&root);
    let addr = start(Compat::new(root.to_str().unwrap(), Sources::none())).await;
    for (m, p) in [
        ("POST", "/api/modem/register"),
        ("POST", "/api/modem/netselect/auto"),
        ("POST", "/api/netinfo/scan"),
        ("POST", "/api/netinfo/apn"),
        ("POST", "/api/scenario/pin"),
        ("POST", "/api/alerts/read"),
        ("GET", "/api/speedtest/servers"),
        ("POST", "/api/speedtest/start"),
        ("GET", "/api/speedtest/progress"),
        ("POST", "/api/speedtest/stop"),
        ("GET", "/api/esim/profiles"),
        ("GET", "/api/esim/job"),
        ("POST", "/api/esim/switch"),
    ] {
        let (st, b) = call(addr, m, p, "{}").await;
        assert_eq!(
            (st, b["error"].as_str()),
            (404, Some("not_in_compat_yet")),
            "{p}"
        );
    }
}

/// The register takes and the data holds: kept, in the session, as `screen`; marker gone.
#[tokio::test]
async fn a_register_that_takes_is_kept() {
    let dev = Dev {
        serving: (440, 20),
        connected: true,
        register_takes: Some(true),
        ..Dev::default()
    };
    let (root, d, c) = setup("reg-ok", dev).await;
    let addr = start(c).await;
    let (st, b) = call(
        addr,
        "POST",
        "/api/modem/register",
        r#"{"m_mcc_mnc":"44010","m_rat":"7"}"#,
    )
    .await;
    assert_eq!(
        (st, b["guard"]["phase"].as_str()),
        (202, Some("registering")),
        "{b}"
    );
    let g = until(addr, |d| d["guard"]["phase"] == "ok").await;
    assert_eq!(g["guard"]["target"], "44010");
    assert_eq!(g["selection"]["mode"], "manual");
    let s1 = json!("s1");
    assert_eq!(
        brief(&d),
        vec![
            (
                "netselect.session.open".into(),
                "screen".into(),
                Value::Null
            ),
            ("netselect.register".into(), "screen".into(), s1),
            (
                "netselect.session.close".into(),
                "screen".into(),
                Value::Null
            ),
        ]
    );
    assert!(!root.join("data/luyaz/core/netinfo.guard").exists());
    assert!(
        !root.join("data/netinfo.guard").exists(),
        "never the agent's marker"
    );
}

/// While a diagnosis runs (D10), search and register are refused before anything is touched.
#[tokio::test]
async fn the_diagnosis_gate_refuses_search_and_register() {
    let (_root, d, c) = setup("gate", Dev::default()).await;
    c.gate().diag_since = Some(c.clock.now() as u64);
    let addr = start(c).await;
    for (p, b) in [
        ("/api/netinfo/scan", ""),
        (
            "/api/modem/register",
            r#"{"m_mcc_mnc":"44010","m_rat":"7"}"#,
        ),
    ] {
        let (st, v) = call(addr, "POST", p, b).await;
        assert_eq!(
            (st, v["busy"].as_str()),
            (409, Some("diagnose")),
            "{p}: {v}"
        );
    }
    // the register's session was opened and closed again; nothing else
    assert_eq!(
        actions(&d),
        ["netselect.session.open", "netselect.session.close"]
    );
}

/// It fails: back to automatic and the data call redialled, all in the session; reason coded.
#[tokio::test]
async fn a_failed_register_goes_back_to_automatic_and_redials() {
    let dev = Dev {
        serving: (440, 20),
        connected: false,
        register_takes: Some(false),
        ..Dev::default()
    };
    let (root, d, c) = setup("reg-fail", dev).await;
    let addr = start(c).await;
    assert_eq!(
        call(
            addr,
            "POST",
            "/api/modem/register",
            r#"{"m_mcc_mnc":"44010","m_rat":"7"}"#
        )
        .await
        .0,
        202
    );
    let g = until(addr, |d| d["guard"]["phase"] == "reverted").await;
    assert_eq!(
        g["guard"]["reason_code"], "register_failed",
        "{}",
        g["guard"]
    );
    let a = actions(&d);
    assert!(
        a.contains(&"netselect.auto".to_string()) && a.contains(&"cellular.redial".to_string()),
        "{a:?}"
    );
    assert!(
        brief(&d)
            .iter()
            .filter(|b| !b.0.starts_with("netselect.session."))
            .all(|b| b.1 == "screen" && b.2 == "s1")
    );
    assert_eq!(
        a.last().map(String::as_str),
        Some("netselect.session.close")
    );
    assert!(!root.join("data/luyaz/core/netinfo.guard").exists());
}

/// "Back to automatic" while a register is guarded: the guard sees it and reverts.
#[tokio::test]
async fn auto_during_a_register_cancels_it() {
    let dev = Dev {
        serving: (440, 20),
        connected: true,
        register_takes: None,
        ..Dev::default()
    };
    let (_root, d, c) = setup("reg-cancel", dev).await;
    let addr = start(c).await;
    assert_eq!(
        call(
            addr,
            "POST",
            "/api/modem/register",
            r#"{"m_mcc_mnc":"44010","m_rat":"7"}"#
        )
        .await
        .0,
        202
    );
    let (st, _) = call(addr, "POST", "/api/modem/netselect/auto", "").await;
    assert_eq!(st, 202);
    let g = until(addr, |d| d["guard"]["phase"] == "reverted").await;
    assert_eq!(g["guard"]["reason_code"], "manual_auto");
    // already automatic (the register never took): no AT+COPS=0
    assert!(!actions(&d).contains(&"netselect.auto".to_string()));
}

/// The session refused (another change in progress): 409 with u60d's own sentence, nothing claimed.
#[tokio::test]
async fn a_busy_session_is_refused_with_its_sentence() {
    let mut dev = Dev::default();
    dev.script.insert(
        "netselect.session.open".into(),
        vec![(409, json!({"ok": false, "error": {"code": "busy", "message": "busy"},
            "doing": {"action": "network.set_mode", "say_zh": "正在换制式（网页发起，5 秒），稍等", "say_en": "Busy: network mode (Web)"}}))],
    );
    let (_root, _d, c) = setup("busy", dev).await;
    let addr = start(c).await;
    let (st, b) = call(
        addr,
        "POST",
        "/api/modem/register",
        r#"{"m_mcc_mnc":"44010","m_rat":"7"}"#,
    )
    .await;
    assert_eq!(
        (st, b["error"].as_str(), b["error_en"].as_str()),
        (
            409,
            Some("正在换制式（网页发起，5 秒），稍等"),
            Some("Busy: network mode (Web)")
        )
    );
    let g = until(addr, |_| true).await;
    assert_eq!(g["guard"]["phase"], "idle");
}

/// op_busy (a network-mode change confirming): held, not counted, then done.
#[tokio::test]
async fn op_busy_holds_back_to_automatic() {
    let mut dev = Dev {
        serving: (440, 20),
        manual_mode: true,
        connected: true,
        ..Dev::default()
    };
    dev.script.insert(
        "netselect.auto".into(),
        vec![(409, json!({"ok": false, "error": {"code": "op_busy", "message": "network mode is being confirmed"}}))],
    );
    let (_root, d, c) = setup("opbusy", dev).await;
    let addr = start(c).await;
    assert_eq!(
        call(addr, "POST", "/api/modem/netselect/auto", "").await.0,
        202
    );
    let g = until(addr, |d| d["guard"]["phase"] == "reverted").await;
    assert_eq!(g["guard"]["reason_code"], "manual_auto");
    let autos = actions(&d)
        .iter()
        .filter(|a| *a == "netselect.auto")
        .count();
    assert_eq!(autos, 2, "held once, then sent again");
}

/// u60d restarted mid-revert: its own marker is finished as `guard`.
#[tokio::test]
async fn resume_from_u60ds_own_marker() {
    let dev = Dev {
        serving: (440, 20),
        manual_mode: true,
        connected: true,
        ..Dev::default()
    };
    let (root, d, c) = setup("resume", dev).await;
    fs::create_dir_all(root.join("data/luyaz/core")).unwrap();
    fs::write(root.join("data/luyaz/core/netinfo.guard"), "revert 1\n").unwrap();
    // the agent's marker is never read
    fs::write(root.join("data/netinfo.guard"), "register 44010 1\n").unwrap();
    c.resume().await;
    let b = brief(&d);
    assert!(
        b.iter()
            .any(|x| x.0 == "netselect.auto" && x.1 == "guard" && x.2 == "s1"),
        "{b:?}"
    );
    assert!(!b.iter().any(|x| x.0 == "netselect.register"));
    assert!(!root.join("data/luyaz/core/netinfo.guard").exists());
    // writes off: nothing resumed
    fs::write(root.join("data/luyaz/core/netinfo.guard"), "revert 1\n").unwrap();
    let n = d.lock().unwrap().log.len();
    c.clone().with_writes(false).resume().await;
    assert_eq!(d.lock().unwrap().log.len(), n);
}

/// A search: in its session, data back afterwards, operators in GET.
#[tokio::test]
async fn an_operator_search() {
    let dev = Dev {
        serving: (440, 20),
        connected: true,
        ..Dev::default()
    };
    let (_root, d, c) = setup("scan", dev).await;
    let addr = start(c).await;
    assert_eq!(call(addr, "POST", "/api/netinfo/scan", "").await.0, 202);
    // a second press while searching is the same search
    assert_eq!(call(addr, "POST", "/api/netinfo/scan", "").await.0, 202);
    let g = until(addr, |d| d["scan"]["state"] == "done").await;
    let ops = g["scan"]["operators"].as_array().unwrap();
    assert_eq!(
        ops.iter()
            .map(|o| o["plmn"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["44010", "44020"]
    );
    let a = actions(&d);
    assert_eq!(a.iter().filter(|a| *a == "netselect.scan").count(), 1);
    assert!(
        a.contains(&"cellular.redial".to_string()),
        "data back after the search: {a:?}"
    );
    assert_eq!(
        d.lock().unwrap().log.last().unwrap()["params"]["result"],
        "scanned"
    );
}

/// A carrier candidate: copied to the manual list, dialled, remembered for this card.
#[tokio::test]
async fn a_candidate_apn_is_copied_dialled_and_remembered() {
    let (root, d, c) = setup("apn", Dev::default()).await;
    let addr = start(c).await;
    let (st, b) = call(addr, "POST", "/api/netinfo/apn", r#"{"id":"auto9"}"#).await;
    assert_eq!(
        (st, b["error_en"].as_str()),
        (404, Some("No such candidate APN"))
    );
    let (st, _) = call(addr, "POST", "/api/netinfo/apn", r#"{"id":"auto2"}"#).await;
    assert_eq!(st, 202);
    let g = until(addr, |d| d["apn"]["switching"] == false).await;
    assert_eq!(g["apn"]["switch_error"], "");
    let b = brief(&d);
    assert_eq!(
        b.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(),
        ["apn.add", "apn.enable", "apn.set_mode"]
    );
    assert!(b.iter().all(|x| x.1 == "screen" && x.2.is_null()));
    assert_eq!(
        d.lock().unwrap().log[1]["params"],
        json!({"profile_id": "manu1"})
    );
    let store: Value =
        serde_json::from_str(&fs::read_to_string(root.join("data/apn-pick.json")).unwrap())
            .unwrap();
    assert_eq!(
        store["picks"]["8981000000000000001"],
        json!({"manual_id": "manu1", "apn": "other.example"})
    );
    assert_eq!(g["apn"]["picked_id"], "manu1");
    // back to automatic forgets it
    assert_eq!(
        call(addr, "POST", "/api/netinfo/apn", r#"{"id":"auto"}"#)
            .await
            .0,
        202
    );
    until(addr, |d| {
        d["apn"]["switching"] == false && d["apn"]["picked_id"].is_null()
    })
    .await;
}

/// The pin file the engine reads; an unknown scenario is refused.
#[tokio::test]
async fn scenario_pin() {
    let (root, _d, c) = setup("pin", Dev::default()).await;
    fs::create_dir_all(root.join("data/scenario")).unwrap();
    fs::write(
        root.join("data/scenario/scenarios.json"),
        serde_json::to_string(&luyaz_policy::scenario::template()).unwrap(),
    )
    .unwrap();
    let addr = start(c).await;
    assert_eq!(
        call(addr, "POST", "/api/scenario/pin", r#"{"id":"nowhere"}"#)
            .await
            .0,
        400
    );
    assert_eq!(
        call(addr, "POST", "/api/scenario/pin", r#"{"id":"away"}"#)
            .await
            .0,
        200
    );
    assert_eq!(
        fs::read_to_string(root.join("data/scenario/pin")).unwrap(),
        "away"
    );
    assert_eq!(
        call(addr, "POST", "/api/scenario/pin", r#"{"id":null}"#)
            .await
            .0,
        200
    );
    assert!(!root.join("data/scenario/pin").exists());
}


/// A speedtest.net look-alike on loopback: the server list points at itself; the download is slow
/// and long, so a test stays running until stopped.
async fn speed_server() -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let base = format!("http://{addr}/st/");
    let list = json!([{"id": 7, "name": "Loop", "sponsor": "Test", "country": "JP", "host": addr.to_string(),
        "url": format!("{base}upload.php")}]);
    let app = axum::Router::new()
        .route(
            "/servers",
            get(move || async move { reply(200, list.clone()) }),
        )
        .route(
            "/st/random4000x4000.jpg",
            get(|| async {
                let chunks = tokio_stream::iter(
                    (0..2000).map(|_| Ok::<_, std::io::Error>(vec![0x5au8; 16384])),
                );
                let slow = tokio_stream::StreamExt::throttle(chunks, Duration::from_millis(5));
                axum::body::Body::from_stream(slow)
            }),
        )
        .fallback(|| async { "size=1" });
    tokio::spawn(async move { axum::serve(l, app).await });
    format!("http://{addr}/servers")
}

/// The page's speed test through the agent's engine: one at a time, D10's gate, stop.
#[tokio::test]
async fn speed_test_through_the_agents_engine() {
    let root = std::env::temp_dir().join(format!("u60d-ops-speed-{}", std::process::id()));
    let _ = fs::create_dir_all(&root);
    let src = Sources {
        speed_servers: speed_server().await,
        ..Sources::none()
    };
    let c = Compat::new(root.to_str().unwrap(), src).with_writes(true);
    let gate = c.gate.clone();
    let addr = start(c).await;
    let (st, b) = call(addr, "GET", "/api/speedtest/servers", "").await;
    assert_eq!((st, b["data"][0]["id"].as_u64()), (200, Some(7)), "{b}");
    // a diagnosis running: refused with its sentence
    gate.lock().unwrap().diag_since = Some(super::home::now_unix());
    let (st, b) = call(addr, "POST", "/api/speedtest/start", "{}").await;
    assert_eq!((st, b["busy"].as_str()), (409, Some("diagnose")));
    gate.lock().unwrap().diag_since = None;
    assert_eq!(
        call(addr, "POST", "/api/speedtest/start", r#"{"server_id":9}"#)
            .await
            .0,
        404
    );
    let (st, b) = call(addr, "POST", "/api/speedtest/start", r#"{"server_id":7}"#).await;
    assert_eq!((st, b["data"]["status"].as_str()), (200, Some("started")));
    let (st, b) = call(addr, "POST", "/api/speedtest/start", "{}").await;
    assert_eq!(
        (st, b["error"].as_str()),
        (409, Some("test already running"))
    );
    let (_, b) = call(addr, "GET", "/api/speedtest/progress", "").await;
    assert_eq!(b["data"]["server"], "Test (Loop)");
    assert_eq!(
        call(addr, "POST", "/api/speedtest/stop", "{}").await.1["data"]["status"],
        "stopping"
    );
    for _ in 0..300 {
        let (_, b) = call(addr, "GET", "/api/speedtest/progress", "").await;
        if b["data"]["phase"] == "cancelled" {
            assert_eq!(
                call(addr, "POST", "/api/speedtest/stop", "{}").await.1["data"]["status"],
                "not_running"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("never stopped");
}

/// A card that switches and a ZTE stack that follows (IMSI changes after the restart).
#[derive(Default)]
struct Card {
    enabled: Mutex<String>,
    imsi_reads: Mutex<u32>,
    rebooted: std::sync::atomic::AtomicBool,
}

impl luyaz_esim::Host for Card {
    fn installed(&self) -> bool {
        true
    }
    fn lpac(&self, args: &[&str], _t: Duration) -> Result<Value, String> {
        let mut on = self.enabled.lock().unwrap();
        match args {
            ["profile", "list"] => Ok(json!([
                {"iccid": "89852000000000000001", "profileState": if *on == "89852000000000000001" { "enabled" } else { "disabled" }},
                {"iccid": "89852000000000000002", "profileState": if *on == "89852000000000000002" { "enabled" } else { "disabled" }}])),
            ["profile", "enable", i] => {
                *on = i.to_string();
                Ok(Value::Null)
            }
            ["notification", "list"] => Ok(json!([])),
            _ => Ok(Value::Null),
        }
    }
    fn sh(&self, cmd: &str) -> String {
        if cmd.contains("get_sync_info") {
            return r#"{"noSyncModuleName":"sync success"}"#.into();
        }
        if cmd.contains("get_sim_info") {
            let mut n = self.imsi_reads.lock().unwrap();
            *n += 1;
            return json!({"sim_imsi": if *n > 1 { "440100000000002" } else { "440100000000001" }})
                .to_string();
        }
        String::new()
    }
    fn exists(&self, _: &str) -> bool {
        true
    }
    fn reboot(&self) {
        self.rebooted
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
    fn data_up(&self) -> bool {
        true
    }
    fn wake(&self, _: bool) {}
    fn sleep(&self, _: Duration) {}
}

/// The screen's eSIM page through the agent's engine: list, switch (u60d's /control told first,
/// as `screen`), job; no reboot when the stack follows.
#[tokio::test]
async fn esim_switch_through_the_agents_engine() {
    let d: Shared = Arc::default();
    let url = control_server(d.clone()).await;
    let card = Arc::new(Card::default());
    *card.enabled.lock().unwrap() = "89852000000000000001".into();
    let root = std::env::temp_dir().join(format!("u60d-ops-esim-{}", std::process::id()));
    let _ = fs::create_dir_all(&root);
    let src = Sources {
        control: Arc::new(control::Client::new(url, None)),
        esim: card.clone(),
        ..Sources::none()
    };
    let addr = start(Compat::new(root.to_str().unwrap(), src).with_writes(true)).await;
    let (st, b) = call(addr, "GET", "/api/esim/profiles", "").await;
    assert_eq!(
        (st, b["data"]["profiles"][0]["profileState"].as_str()),
        (200, Some("enabled")),
        "{b}"
    );
    assert_eq!(
        call(addr, "POST", "/api/esim/switch", r#"{"iccid":"12"}"#)
            .await
            .0,
        400
    );
    let (st, b) = call(
        addr,
        "POST",
        "/api/esim/switch",
        r#"{"iccid":"89852000000000000002"}"#,
    )
    .await;
    assert_eq!((st, b["data"]["status"].as_str()), (200, Some("running")));
    let mut j = Value::Null;
    for _ in 0..500 {
        j = call(addr, "GET", "/api/esim/job", "").await.1["data"].clone();
        if j["status"] != "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        (j["status"].as_str(), j["rebooting"].as_bool()),
        (Some("done"), Some(false)),
        "{j}"
    );
    assert!(!card.rebooted.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(
        brief(&d),
        vec![(
            "op.interrupt".to_string(),
            "screen".to_string(),
            Value::Null
        )]
    );
    assert_eq!(d.lock().unwrap().log[0]["params"], json!({"what": "esim"}));
}
