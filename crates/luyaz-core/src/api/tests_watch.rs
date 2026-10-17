//! Step 9b in u60d: the group the agent used to hand over together (netwatch, deep diagnosis,
//! search/register, the speed test, eSIM, the APN SIM-change monitor), u60d's with writes on.

use super::tests_ops::{Dev, Shared, brief, call, control_server, start, ubus};
use super::*;
use std::fs;
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn root(name: &str) -> std::path::PathBuf {
    let r = std::env::temp_dir().join(format!("u60d-watch-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&r);
    fs::create_dir_all(&r).unwrap();
    r
}

async fn wait(what: &str, f: impl Fn() -> bool) {
    for _ in 0..500 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("never got there: {what}");
}

/// A request with one extra header.
async fn call_as(addr: std::net::SocketAddr, path: &str, header: &str) -> (u16, Value) {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n{header}Content-Type: application/json\r\nContent-Length: 2\r\n\r\n{{}}"
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    let status = out[9..12].parse().unwrap();
    let json = out.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
    (status, serde_json::from_str(json).unwrap_or(Value::Null))
}

#[tokio::test]
async fn with_writes_on_the_group_is_served_and_the_rest_too() {
    let r = root("on");
    fs::create_dir_all(r.join("tmp")).unwrap();
    let c = Compat::new(r.to_str().unwrap(), Sources::none()).with_writes(true);
    let addr = start(c.clone()).await;
    let (st, b) = call(addr, "GET", "/api/diagnose", "").await;
    assert_eq!(st, 200, "{b}");
    // the screen's other writes and reads
    assert_eq!(call(addr, "POST", "/api/alerts/read", r#"{"seq":0}"#).await.0, 200);
    assert_eq!(call(addr, "GET", "/api/netinfo?lite=1", "").await.0, 200);
    // and someone looking at it keeps the exits fresh
    wait("exits wanted", || r.join("tmp/netinfo-exits.wanted").exists()).await;
}

#[tokio::test]
async fn a_diagnosis_runs_and_writes_the_signal_log() {
    let r = root("diag");
    let c = Compat::new(r.to_str().unwrap(), Sources::none())
        .with_writes(true)
        .with_clock(1000);
    tokio::spawn(watch::sample(c.clone()));
    let addr = start(c.clone()).await;
    wait("log open", || c.group.watch.writes()).await;

    // the agent forwarding a LAN client's run names it; loopback alone is the front screen
    let (st, b) = call_as(addr, "/api/diagnose", "X-U60-Client: 192.168.0.23\r\n").await;
    assert_eq!((st, b["data"]["from"].as_str()), (202, Some("client")));
    wait("done", || c.group.diag.get().1["data"]["state"] == "done").await;
    let (st, b) = call(addr, "GET", "/api/diagnose", "").await;
    assert_eq!((st, b["data"]["id"].as_u64()), (200, Some(1)));
    let (st, _) = call(
        addr,
        "POST",
        "/api/diagnose/feedback",
        r#"{"id":1,"right":false}"#,
    )
    .await;
    assert_eq!(st, 200);
    let (_, b) = call_as(addr, "/api/diagnose", "").await;
    assert_eq!(b["data"]["from"], "touch");
    wait("second run done", || {
        let v = c.group.diag.get().1;
        v["data"]["id"] == 2 && v["data"]["state"] == "done"
    })
    .await;

    // both events are in u60d's signal log, in the agent's format
    let dir = r.join("data/signal-log");
    let events: Vec<Value> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".events.jsonl"))
        .flat_map(|e| {
            fs::read_to_string(e.path())
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter_map(|l| serde_json::from_str(&l).ok())
        .collect();
    let kinds: Vec<&str> = events.iter().filter_map(|e| e["kind"].as_str()).collect();
    assert!(
        kinds.starts_with(&["diagnose", "diagnose_feedback"]),
        "{kinds:?}"
    );
    assert_eq!(events[0]["from"], "client");

    // the gate is free again
    assert!(c.gate.lock().unwrap().diag_since.is_none());
}

#[tokio::test]
async fn a_returning_card_gets_its_apn_pick_back_as_auto() {
    let r = root("apn");
    // the fake card is …0001; the device is on automatic APN; the card left with a pick
    let store = json!({"picks": {"8981000000000000001": {"manual_id": "m1", "apn": "spmode.ne.jp"}},
        "last_iccid": "8900000000000000002", "notice": null});
    fs::create_dir_all(r.join("data")).unwrap();
    fs::write(r.join("data/apn-pick.json"), store.to_string()).unwrap();
    let d: Shared = Arc::new(Mutex::new(Dev::default()));
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
    let c = Compat::new(r.to_str().unwrap(), src)
        .with_writes(true)
        .with_clock(1000);
    tokio::spawn(watch::apn_watch(c.clone()));
    let read = || -> Value {
        serde_json::from_str(&fs::read_to_string(r.join("data/apn-pick.json")).unwrap()).unwrap()
    };
    wait("committed", || {
        read()["last_iccid"] == "8981000000000000001"
    })
    .await;
    let sent: Vec<(String, String)> = brief(&d).into_iter().map(|(a, s, _)| (a, s)).collect();
    assert_eq!(
        sent,
        [
            ("apn.enable".to_string(), "auto".to_string()),
            ("apn.set_mode".to_string(), "auto".to_string())
        ]
    );
    let s = read();
    assert_eq!(
        s["notice"]["text_en"],
        "This SIM is back; APN spmode.ne.jp again"
    );
    assert_eq!(
        s["picks"]["8981000000000000001"]["manual_id"], "m1",
        "the pick is kept"
    );
}

#[tokio::test]
async fn diagnose_carries_the_open_finding_and_keeps_its_fields() {
    use luyaz_api::diagnose::Level;
    use luyaz_api::finding::{Confidence, Evidence, Finding, KIND_NO_INTERNET, LayerId, Verdict};
    let r = root("finding");
    fs::create_dir_all(r.join("tmp")).unwrap();
    let c = Compat::new(r.to_str().unwrap(), Sources::none()).with_writes(true);
    let addr = start(c.clone()).await;
    let (st, b) = call(addr, "GET", "/api/diagnose", "").await;
    assert_eq!(st, 200, "{b}");
    assert_eq!(b["ok"], true);
    assert_eq!(b["data"]["state"], "idle");
    assert!(b["data"]["finding"].is_null(), "{b}");

    let iccid = "89860012345678901234";
    let t = luyaz_netwatch::record::CLOCK_SET + 1_000_000;
    let v = Verdict::Open(Finding {
        kind: KIND_NO_INTERNET,
        iccid: iccid.into(),
        layer: LayerId::Internet,
        confidence: Confidence::Sure,
        symptom: "已连上，但外网不通".into(),
        symptom_en: "Connected, but the internet is unreachable".into(),
        evidence: vec![Evidence::new(LayerId::Internet, Level::Bad, "x", "x", t)],
        at: t,
    });
    c.group.findings.observe(&v, iccid, &c.group.watch, t);
    let (_, b) = call(addr, "GET", "/api/diagnose", "").await;
    assert_eq!(b["data"]["state"], "idle");
    let f = &b["data"]["finding"];
    assert_eq!(f["symptom"], "已连上，但外网不通");
    assert_eq!((f["layer"].as_str(), f["confidence"].as_str()), (Some("internet"), Some("sure")));
    assert_eq!(f["iccid_tail"], "1234");
    assert_eq!(f["evidence"][0]["layer"], "internet");
    assert!(!b.to_string().contains("8986001234"), "{b}");
}

#[tokio::test]
async fn ignore_closes_the_shown_finding() {
    use luyaz_api::finding::{Confidence, Evidence, Finding, KIND_NO_INTERNET, LayerId, Verdict};
    let r = root("dismiss");
    let c = Compat::new(r.to_str().unwrap(), Sources::none()).with_writes(true);
    let addr = start(c.clone()).await;
    let now = luyaz_netwatch::now_unix();
    let f = Finding {
        kind: KIND_NO_INTERNET,
        iccid: "89860000001234".into(),
        layer: LayerId::Data,
        confidence: Confidence::Sure,
        symptom: "已注册，但数据连接没建立".into(),
        symptom_en: "Registered, but the data connection is down".into(),
        evidence: vec![Evidence::new(
            LayerId::Data,
            luyaz_api::diagnose::Level::Bad,
            "x",
            "x",
            now,
        )],
        at: now,
    };
    c.group
        .findings
        .observe(&Verdict::Open(f), "89860000001234", &c.group.watch, now);
    let (_, b) = call(addr, "GET", "/api/diagnose", "").await;
    assert_eq!(b["data"]["finding"]["iccid_tail"], "1234");
    let body = format!(r#"{{"kind":"{KIND_NO_INTERNET}","iccid_tail":"1234"}}"#);
    assert_eq!(
        call(addr, "POST", "/api/diagnose/dismiss", &body).await.0,
        200
    );
    assert_eq!(
        call(addr, "POST", "/api/diagnose/dismiss", &body).await.0,
        404
    );
    assert_eq!(
        call(addr, "POST", "/api/diagnose/dismiss", "{}").await.0,
        400
    );
    let (_, b) = call(addr, "GET", "/api/diagnose", "").await;
    assert_eq!(b["data"]["finding"], Value::Null);
}
