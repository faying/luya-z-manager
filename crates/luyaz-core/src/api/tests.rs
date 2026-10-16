use super::*;
use std::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn scratch(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("u60d-compat-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

pub(super) async fn start(c: Compat) -> std::net::SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(serve(l, c));
    addr
}

/// One request, `Connection: close`; (status, JSON body).
pub(super) async fn call(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: &str,
) -> (u16, Value) {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let auth = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    let status = out[9..12].parse().unwrap();
    let json = out.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
    (status, serde_json::from_str(json).unwrap_or(Value::Null))
}

#[tokio::test]
async fn open_without_a_login() {
    let addr = start(Compat::new(scratch("open").to_str().unwrap(),
        Sources::none(),
    ))
    .await;
    assert_eq!(call(addr, "GET", "/api/screen", None, "").await.0, 200);
}

#[tokio::test]
async fn alerts_and_the_read_cursor() {
    let root = scratch("alerts");
    let d = root.join("data/alerts");
    fs::create_dir_all(&d).unwrap();
    fs::write(
        d.join("queue"),
        "7\t1758600000\t42\tagent-crash\tA\n8\t35392498\t7\twifi-takeover\tB\n",
    )
    .unwrap();
    // writes off (the shadow period): the agent owns the cursor, this is 404 like anything not here
    let off = start(Compat::new(root.to_str().unwrap(), Sources::none())).await;
    let (st, body) = call(off, "POST", "/api/alerts/read", None, r#"{"seq":99}"#).await;
    assert_eq!(
        (st, body["error"].as_str()),
        (404, Some("not_in_compat_yet"))
    );
    let addr =
        start(Compat::new(root.to_str().unwrap(), Sources::none()).with_writes(true)).await;
    let (_, body) = call(addr, "GET", "/api/alerts", None, "").await;
    let data = &body["data"];
    assert_eq!(data["unread"], 2);
    assert_eq!(data["events"][0]["seq"], 8);
    assert_eq!(data["events"][1]["label"], "管理后台意外退出，已自动重启");
    assert!(data["utc_offset"].is_i64());
    // past the newest: clamped; never backwards
    let (st, body) = call(addr, "POST", "/api/alerts/read", None, r#"{"seq":99}"#).await;
    assert_eq!((st, body["data"]["read"].clone()), (200, json!(8)));
    let (_, body) = call(addr, "POST", "/api/alerts/read", None, r#"{"seq":1}"#).await;
    assert_eq!(body["data"]["read"], 8);
    assert_eq!(fs::read_to_string(d.join("read")).unwrap(), "8\n");
    assert_eq!(
        call(addr, "POST", "/api/alerts/read", None, "{}").await.0,
        400
    );
    let (_, body) = call(addr, "GET", "/api/alerts", None, "").await;
    assert_eq!(body["data"]["unread"], 0);
    // the alert SMS number and the abroad switch (8d-1c)
    let set = |b: &'static str| call(addr, "PUT", "/api/alerts/sms", None, b);
    let (st, body) = set(r#"{"number":"+86 138-0000-0000","abroad":true}"#).await;
    assert_eq!(st, 200, "{body}");
    assert_eq!(
        fs::read_to_string(d.join("sms-to")).unwrap(),
        "+8613800000000\n"
    );
    assert!(d.join("sms-abroad").exists());
    let (_, all) = call(addr, "GET", "/api/alerts", None, "").await;
    assert_eq!(body["data"], all["data"]["sms"]);
    for bad in [r#"{"number":"12ab"}"#, r#"{"abroad":"yes"}"#, "nope"] {
        assert_eq!(set(bad).await.0, 400, "{bad}");
    }
    let (st, _) = set(r#"{"number":""}"#).await;
    assert!(st == 200 && !d.join("sms-to").exists(), "empty number: SMS alerts off");
}

#[tokio::test]
async fn screen_and_the_switched_off_neighbour_scan() {
    let addr = start(Compat::new(scratch("screen").to_str().unwrap(),
        Sources::none(),
    ))
    .await;
    let (st, body) = call(addr, "GET", "/api/screen", None, "").await;
    assert_eq!((st, body["data"]["v"].clone()), (200, json!(1)));
    assert!(body["data"]["battery"]["state"].is_string(), "{body}");
    let (st, body) = call(addr, "POST", "/api/netinfo/neighbors/scan", None, "").await;
    assert_eq!(st, 410);
    assert_eq!(
        body["error_en"],
        "Off: the stock scan drops data, finds nothing"
    );
}

fn put(root: &std::path::Path, path: &str, text: &str) {
    let p = root.join(path.trim_start_matches('/'));
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn now() -> u64 {
    super::home::now_unix()
}

const TSV2: &str = "#tsv2\nok\tfota\tZTE 自动升级\t已关闭\tZTE auto-update\tOff\nbad\tscreen\t触屏界面\t没在运行\tScreen UI\tNot running\nwarn\tsms\t告警短信\t没设号码\tAlert SMS\tNo number\n";

/// A device as the agent leaves it: doctor snapshot, scenario files (and the private build's own).
fn device(name: &str) -> std::path::PathBuf {
    let root = scratch(name);
    let snap = luyaz_api::health::Snapshot {
        checks: luyaz_api::health::parse_tsv2(TSV2),
        at_device: 1_790_000_000,
        ran: true,
        error: None,
    };
    put(
        &root,
        luyaz_api::health::SNAPSHOT_FILE,
        &snap.to_json().to_string(),
    );
    put(
        &root,
        "/data/alerts/queue",
        "7\t1758600000\t42\tagent-crash\tA\n",
    );
    put(
        &root,
        "/data/scenario/scenarios.json",
        &serde_json::to_string(&luyaz_policy::scenario::template()).unwrap(),
    );
    put(
        &root,
        "/data/scenario/state.json",
        r#"{"current":"home","candidate":"","hits":0,"misses":0,"applied":{},"last_switch":1790000123,"last_error":null,"last_scan":0}"#,
    );
    put(&root, "/data/scenario/pin", "home\n");
    root
}

#[tokio::test]
async fn public_status_says_whether_tailscale_is_there_and_runs() {
    let root = device("public-ts");
    let c = || Compat::new(root.to_str().unwrap(), Sources::none());
    let ts = |c: &Compat| status::gather(c)["services"]["tailscale"].clone();
    assert_eq!(ts(&c()), json!({"installed": false, "running": false}));
    put(&root, "/data/tailscale/tailscale", "");
    put(&root, "/proc/1/comm", "init\n");
    put(&root, "/proc/self/comm", "tailscaled\n");
    assert_eq!(
        ts(&c()),
        json!({"installed": true, "running": false}),
        "only numeric /proc entries"
    );
    put(&root, "/proc/812/comm", "tailscaled\n");
    assert_eq!(ts(&c()), json!({"installed": true, "running": true}));
}

#[tokio::test]
async fn public_status_has_what_the_screen_reads_without_a_login() {
    let root = device("public");
    let addr = start(Compat::new(root.to_str().unwrap(),
        Sources::none(),
    ))
    .await;
    let (st, body) = call(addr, "GET", "/api/public/status", None, "").await;
    assert_eq!(st, 200, "{body}");
    let d = &body["data"];
    assert_eq!(d["alerts"]["unread"], 1);
    assert_eq!(d["health"], json!({"checked": true, "bad": 1, "warn": 1}));
    assert!(d["clock"]["utc_offset"].is_i64());
    let s = &d["scenario"];
    assert_eq!(
        (
            s["configured"].clone(),
            s["enabled"].clone(),
            s["current"].clone()
        ),
        (json!(true), json!(true), json!("home"))
    );
    assert_eq!(
        (s["name"].as_str(), s["name_en"].as_str(), s["pin"].as_str()),
        (Some("在家"), Some("Home"), Some("home"))
    );
    assert_eq!(
        (
            s["last_switch"].clone(),
            s["abroad"].clone(),
            s["guard_takeover"].clone()
        ),
        (json!(1790000123), json!(false), json!(false))
    );
    // the screen's buffers (touch-ui scenario.c): services 4096, health 128
    for (v, cap) in [
        (&d["services"], 4096),
        (&d["health"], 128),
        (&d["alerts"], 128),
    ] {
        assert!(v.to_string().len() < cap, "{v}");
    }
    // the scenarios off
    put(&root, "/data/scenario/disabled", "");
    let (_, body) = call(addr, "GET", "/api/public/status", None, "").await;
    assert_eq!(body["data"]["scenario"]["enabled"], false);
}

#[tokio::test]
async fn health_from_the_agents_snapshot() {
    let root = device("health");
    put(
        &root,
        "/data/crashlog/zte-agent/20261008-101010-up99.log",
        "status: killed by SIGSEGV\n",
    );
    let addr = start(Compat::new(root.to_str().unwrap(), Sources::none())).await;
    let (st, body) = call(addr, "GET", "/api/health", None, "").await;
    assert_eq!(st, 200);
    let d = &body["data"];
    assert_eq!(
        (d["bad"].clone(), d["warn"].clone(), d["checked_at"].clone()),
        (json!(1), json!(1), json!(1_790_000_000))
    );
    assert_eq!(
        d["checks"][1],
        json!({"level": "bad", "id": "screen", "label": "触屏界面", "detail": "没在运行", "label_en": "Screen UI", "detail_en": "Not running"})
    );
    assert_eq!(d["crashlogs"][0]["program"], "zte-agent");
    assert_eq!(d["crashlogs"][0]["status"], "killed by SIGSEGV");
    // one crash log, the agent's route and reply; names checked
    let (st, body) = call(
        addr,
        "GET",
        "/api/health/crashlog?program=zte-agent&file=20261008-101010-up99.log",
        None,
        "",
    )
    .await;
    assert_eq!((st, body["data"]["text"].clone()), (200, json!("status: killed by SIGSEGV\n")));
    let (st, _) = call(addr, "GET", "/api/health/crashlog?program=..&file=x.log", None, "").await;
    assert_eq!(st, 400);
    let (st, _) = call(addr, "GET", "/api/health/crashlog?program=zte-agent&file=no.log", None, "").await;
    assert_eq!(st, 404);
    // no snapshot (the agent has not run the doctor yet)
    fs::remove_file(root.join("tmp/health.json")).unwrap();
    let (_, body) = call(addr, "GET", "/api/health", None, "").await;
    assert!(body["data"]["checked_at"].is_null() && body["data"]["checks"] == json!([]));
    let (_, body) = call(addr, "GET", "/api/public/status", None, "").await;
    assert_eq!(body["data"]["health"]["checked"], false);
}


/// The vendor reads as a device answers them (documentation addresses, made-up ids).
fn fake_ubus(s: &'static str, m: &'static str, _a: Value) -> UbusReply {
    let v = match (s, m) {
        ("zte_nwinfo_api", "nwinfo_get_netinfo") => Ok(json!({
            "rmcc": 440, "rmnc": 10, "network_provider_fullname": "NTT DOCOMO",
            "simcard_roam": "Roaming", "network_type": "SA", "net_select_mode": "auto_select"})),
        ("zwrt_data", "get_wwaniface") => Ok(json!({"connect_status": "ipv4_ipv6_connected"})),
        ("zwrt_apn_object", "get_apn_mode") => Ok(json!({"apn_mode": 0})),
        ("zwrt_apn_object", "get_apn_at_cid") => Ok(
            json!({"profilename": "Docomo", "wanapn": "spmode.ne.jp", "pdpType": 3, "profileId": "auto1"}),
        ),
        ("zwrt_apn_object", "get_auto_apn_list") => Ok(json!({"apnListArray": [
            {"profilename": "Docomo", "wanapn": "spmode.ne.jp", "pdpType": 3, "profileId": "auto1", "isEnable": true},
            {"profilename": "Docomo IoT", "wanapn": "iot.example", "pdpType": 3, "profileId": "auto2", "isEnable": false}]})),
        ("zwrt_apn_object", "get_manu_apn_list") => Ok(json!({"apnListArray": [
            {"profilename": "Mine", "wanapn": "mine.example", "pdpType": 3, "profileId": "manu1", "isEnable": true}]})),
        _ => Err(format!("{s} {m}: not faked")),
    };
    Box::pin(async move { v })
}

fn netinfo_device(name: &str) -> std::path::PathBuf {
    let root = device(name);
    let exit = |ip: &str, node: Value| {
        json!({"ip": ip, "geo": "日本 东京", "geo_en": "Tokyo, Japan", "isp": "Example", "isp_en": "Example",
            "node": node, "source": "h", "fetched_at": 1_790_000_000, "error": null, "error_en": null})
    };
    put(
        &root,
        luyaz_api::netinfo::EXITS_FILE,
        &json!({"direct": exit("198.51.100.7", Value::Null), "proxy": exit("203.0.113.9", json!("JP 03"))})
            .to_string(),
    );
    put(
        &root,
        luyaz_api::netinfo::APN_PICK_STORE,
        &json!({"picks": {"8981000000000000001": {"manual_id": "manu1", "apn": "mine.example"}},
            "last_iccid": "8981000000000000001",
            "notice": {"at": now() - 5, "text": "已换 APN", "text_en": "APN changed", "sticky": false}})
            .to_string(),
    );
    put(
        &root,
        "/tmp/dhcp.leases",
        "1790000000 02:00:00:00:00:01 192.168.0.100 phone *\n",
    );
    root
}

fn netinfo_sources() -> Sources {
    let hub = Arc::new(crate::block::Hub::new(
        vec![crate::block::BlockSpec::derived(
            "sim",
            Box::new(crate::block::OnChange),
        )],
        Box::new(crate::block::tests::Recorder::default()),
    ));
    hub.record(
        "sim",
        Ok(json!({"state": "ready", "imsi": "460011234567890", "iccid": "8981000000000000001F"})),
        tokio::time::Instant::now(),
    );
    Sources {
        hub: Some(hub),
        ubus: Arc::new(fake_ubus),
        stations: Arc::new(|| {
            vec![luyaz_api::netinfo::Station {
                mac: "02:00:00:00:00:01".into(),
                iface: "wlan0".into(),
                down: 5_000,
                up: 1_000,
                signal: Some(-60),
                band: "5 GHz",
                wifi_gen: Some(6),
                link_down_mbps: Some(1201),
                ..Default::default()
            }]
        }),
        ..Sources::none()
    }
}

/// Every piece the screen reads, from the sources the doc names; the sizes fit its buffers.
#[tokio::test]
async fn netinfo_as_the_agent_assembles_it() {
    let root = netinfo_device("netinfo");
    let addr = start(Compat::new(root.to_str().unwrap(), netinfo_sources())).await;
    // the first read starts the refresh pass; the second one sees it
    let (st, _) = call(addr, "GET", "/api/netinfo", None, "").await;
    assert_eq!(st, 200);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let (st, body) = call(addr, "GET", "/api/netinfo", None, "").await;
    assert_eq!(st, 200, "{body}");
    let d = &body["data"];
    // local: operators, roaming (the vendor's flag, as the agent), network type, data
    assert_eq!(
        (
            d["home_operator"]["mcc"].as_str(),
            d["serving_operator"]["mcc"].as_str(),
            d["serving_operator"]["mnc"].as_str(),
            d["roaming"].as_bool(),
            d["network_type"].as_str(),
            d["data_connected"].as_bool(),
        ),
        (
            Some("460"),
            Some("440"),
            Some("10"),
            Some(true),
            Some("SA"),
            Some(true)
        ),
        "{d}"
    );
    assert_eq!(d["selection"]["mode"], "auto");
    // exits from the agent's file
    assert_eq!(d["direct"]["ip"], "198.51.100.7");
    // the agent's operations: idle here
    assert_eq!(
        (
            d["guard"]["phase"].as_str(),
            d["scan"]["state"].as_str(),
            d["neighbors"]["state"].as_str(),
            d["neighbors"]["scanned_at"].as_i64(),
        ),
        (Some("idle"), Some("idle"), Some("unsupported"), Some(0))
    );
    // clients: the station with its lease
    let cl = &d["clients"]["list"][0];
    assert_eq!(
        (
            cl["name"].as_str(),
            cl["ip"].as_str(),
            cl["down_bytes"].as_u64(),
            cl["signal_tier"].as_str(),
            cl["band"].as_str()
        ),
        (
            Some("phone"),
            Some("192.168.0.100"),
            Some(5_000),
            Some("good"),
            Some("5 GHz")
        )
    );
    assert!(d["clients"]["at"].as_i64() > Some(0));
    // scenes from the engine's files, as the shared picker renders them
    let tpl = luyaz_policy::scenario::template();
    assert_eq!(
        d["scenes"]["list"].as_array().unwrap().len(),
        tpl.scenarios.len()
    );
    assert_eq!(
        (d["scenes"]["current"].as_str(), d["scenes"]["pin"].as_str()),
        (Some("home"), Some("home"))
    );
    assert_eq!(d["scenes"]["list_en"][0]["name_en"], "Home");
    // apn: the dialled one, the IoT candidate tagged, this card's pick and notice
    let a = &d["apn"];
    assert_eq!(
        (
            a["mode"].as_str(),
            a["in_use"]["apn"].as_str(),
            a["auto"][1]["iot"].as_bool(),
            a["picked_id"].as_str(),
            a["notice"]["text_en"].as_str(),
            a["switching"].as_bool(),
        ),
        (
            Some("auto"),
            Some("spmode.ne.jp"),
            Some(true),
            Some("manu1"),
            Some("APN changed"),
            Some(false)
        ),
        "{a}"
    );
    // touch-ui netinfo.c's buffers
    let len = |v: &Value| v.to_string().len();
    assert!(len(d) < 32768);
    for k in ["scan", "scenes", "clients", "apn", "neighbors"] {
        assert!(len(&d[k]) < 8192, "{k}");
    }
    assert!(len(&d["scenes"]["list"]) < 4096 && len(&d["scenes"]["list_en"]) < 4096);
    for k in ["direct", "proxy", "guard"] {
        assert!(len(&d[k]) < 1024, "{k}");
    }
    for k in ["home_operator", "serving_operator"] {
        assert!(len(&d[k]) < 512, "{k}");
    }

    // lite: no APN reads unless asked
    let (_, body) = call(addr, "GET", "/api/netinfo?lite=1", None, "").await;
    assert!(body["data"]["apn"].is_null());
    let (_, body) = call(addr, "GET", "/api/netinfo?lite=1&apn=1", None, "").await;
    let d = &body["data"];
    assert_eq!(d["apn"]["mode"], "auto");
}


/// On u60d's own listener the routes sit at `/v2/api/...`, need no login (the listener's auth
/// applies), leave u60d's static `/v2/*` routes alone, and see the peer address.
#[tokio::test]
async fn the_routes_on_u60ds_listener() {
    let c = Compat::new(scratch("v2").to_str().unwrap(),
        Sources::none(),
    );
    let peer = Router::new()
        .route(
            "/api/peer",
            get(|req: Request| async move {
                let ip = watch::remote(&req).map(|i| i.to_string());
                Json(json!({ "ip": ip }))
            }),
        )
        .layer(middleware::from_fn(peer_as_connect_info));
    let outer: Router = Router::new()
        .route("/v2/state", get(|| async { Json(json!({"state": true})) }))
        .nest_service("/v2", c.clone().v2_router())
        .nest_service("/x", peer);
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            crate::conn::DatadListener::new(l),
            outer.into_make_service_with_connect_info::<crate::conn::Peer>(),
        )
        .await
    });
    let (st, b) = call(addr, "GET", "/v2/api/screen", None, "").await;
    assert_eq!((st, b["ok"].clone()), (200, json!(true)), "{b}");
    assert_eq!(
        call(addr, "GET", "/v2/state", None, "").await.1["state"],
        true
    );
    let (st, b) = call(addr, "GET", "/v2/api/nope", None, "").await;
    assert_eq!((st, b["error"].clone()), (404, json!("not_in_compat_yet")));
    assert_eq!(
        call(addr, "GET", "/x/api/peer", None, "").await.1["ip"],
        "127.0.0.1"
    );
}
