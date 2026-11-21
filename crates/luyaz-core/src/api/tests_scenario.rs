//! Step 9h: the scenario engine, u60d's with writes on.

use super::tests::call;
use super::*;
use std::fs;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use luyaz_scenario::testing::Fake;
use super::tests::start;
use luyaz_policy::scenario::{self as sc, SsidEntry};
use luyaz_scenario::testing::net;
use luyaz_scenario::{DISABLED_FLAG, DISABLED_PAUSED, DISABLED_STOPPING, HEARTBEAT_FILE, LEASE_FILE, PIN_FILE, RESTORE_FILE, STATE_FILE};

async fn wait(what: &str, f: impl Fn() -> bool) {
    for _ in 0..1000 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("never got there: {what}");
}

fn compat(f: &Arc<Fake>) -> Compat {
    let fake = f.clone();
    Compat::new(&f.root, Sources::none())
        .with_writes(true)
        .with_scenario_io(Arc::new(move |_, active| {
            *fake.active.lock().unwrap() = Some(active);
            fake.clone()
        }))
}

#[tokio::test]
async fn u60d_runs_the_scenario_engine() {
    let f = Fake::new("u60d-compat-scenario");
    let root = std::path::PathBuf::from(&f.root);
    let mut cfg = sc::template();
    cfg.scenarios[0].detect = sc::Detect::Ssid { entries: vec![SsidEntry { ssid: "TestHome".into(), bssid: None }] };
    f.write_config(&cfg);
    f.scan_returns(vec![net("TestHome", "02:00:00:00:00:01", -50.0)]);
    let c = compat(&f);
    let addr = start(c.clone()).await;
    c.resume_scenario().await;

    // started: bootsafe, then ticks; home is seen twice and Wi-Fi goes off
    wait("started", || c.scenario.engine().is_some()).await;
    let lease = root.join("tmp/scenario.owner");
    assert!(fs::read_to_string(&lease).unwrap().starts_with(&std::process::id().to_string()));
    wait("home", || c.scenario.engine().is_some_and(|e| e.run_state().current == "home")).await;
    assert!(f.calls().contains(&"wifi false false".to_string()));
    assert!(root.join("tmp/scenario.heartbeat").exists());
    assert!(f.log().contains("u60d runs the engine"));
    let (status, body) = call(addr, "GET", "/api/scenario", None, "").await;
    assert_eq!(status, 200);
    assert_eq!(body["data"]["current"], "home");
    // the network that matched, for the home card (authenticated read only)
    assert_eq!(body["data"]["matched"]["ssid"], "TestHome", "{body}");
    assert_eq!(body["data"]["matched"]["id"], "home");
    assert!(body["data"]["matched"]["since"].as_i64().is_some());
    assert_eq!(body["data"]["enter_hits"], 2);
    assert_eq!(body["data"]["exit_misses"], 2);
    let (_, public) = call(addr, "GET", "/api/public/status", None, "").await;
    assert!(!public.to_string().contains("TestHome"), "{public}");
    assert_eq!(call(addr, "GET", "/api/scenario/log", None, "").await.0, 200);
    // the screen's pin goes to the engine (its full reply)
    let (status, body) = call(addr, "POST", "/api/scenario/pin", None, r#"{"id":"away"}"#).await;
    assert_eq!(status, 200);
    assert_eq!(body["data"]["pin"], "away");
    wait("pinned away", || c.scenario.engine().is_some_and(|e| e.run_state().current == "away")).await;
    assert_eq!(call(addr, "POST", "/api/scenario/pin", None, r#"{"id":null}"#).await.0, 200);
    // The reply follows worker shutdown and removal of its lease and heartbeat.
    let (status, body) = call(addr, "PUT", "/api/scenario/enabled", None, r#"{"enabled":false}"#).await;
    assert_eq!(status, 200);
    assert_eq!(body["data"]["enabled"], false);
    assert_eq!(body["data"]["disabled_state"], DISABLED_PAUSED);
    assert_dormant(&c, &f);
    // CHILL's page, stopped: the state file as it is
    fs::create_dir_all(root.join("tmp")).unwrap();
    fs::write(root.join("tmp/chill.state"), r#"{"state":"stopped"}"#).unwrap();
    let (status, body) = call(addr, "GET", "/api/services/chill", None, "").await;
    assert_eq!(status, 200);
    assert_eq!(body["data"]["state"], "stopped");
    let _ = fs::remove_dir_all(&root);
}

fn assert_dormant(c: &Compat, f: &Fake) {
    assert!(!c.scenario.worker_running());
    assert!(!std::path::Path::new(&f.file(LEASE_FILE)).exists());
    assert!(!std::path::Path::new(&f.file(HEARTBEAT_FILE)).exists());
}

#[tokio::test]
async fn paused_start_keeps_the_scenario_api_and_can_resume_auto_or_pin() {
    let f = Fake::new("paused-lifecycle");
    let mut cfg = sc::template();
    f.write_config(&cfg);
    fs::write(
        f.file(STATE_FILE),
        serde_json::to_vec(&sc::RunState {
            current: "abroad".into(),
            last_scan: 1_700_000_000,
            ..Default::default()
        })
        .unwrap(),
    )
    .unwrap();
    fs::write(f.file(DISABLED_FLAG), DISABLED_PAUSED).unwrap();
    let c = compat(&f);
    let addr = start(c.clone()).await;
    c.resume_scenario().await;
    let (st, body) = call(addr, "GET", "/api/scenario", None, "").await;
    assert_eq!(st, 200);
    assert_eq!(body["data"]["enabled"], false);
    assert_eq!(body["data"]["disabled_state"], DISABLED_PAUSED);
    assert_eq!(body["data"]["current"], "abroad");
    for p in ["/api/scenario/template", "/api/scenario/log", "/api/services/chill"] {
        assert_eq!(call(addr, "GET", p, None, "").await.0, 200, "{p}");
    }
    let (_, public) = call(addr, "GET", "/api/public/status", None, "").await;
    let sc = &public["data"]["scenario"];
    assert_eq!(sc["enabled"], false);
    assert_eq!(sc["current"], "");
    assert_eq!(sc["abroad"], false);
    cfg.scenarios[0].name = "Edited while off".into();
    let cfg = serde_json::to_value(cfg).unwrap();
    let (st, body) = call(addr, "PUT", "/api/scenario", None, &cfg.to_string()).await;
    assert_eq!(st, 200, "{body}");
    assert_eq!(body["data"]["config"], cfg);
    assert_dormant(&c, &f);
    assert!(f.calls().is_empty());
    assert_eq!(call(addr, "GET", "/api/scenario/scan", None, "").await.0, 200);
    assert_eq!(f.calls(), ["scan"]);

    for (method, path, request, pin) in [
        ("PUT", "/api/scenario/enabled", r#"{"enabled":true}"#, Value::Null),
        ("POST", "/api/scenario/pin", r#"{"id":"away"}"#, json!("away")),
    ] {
        let (st, body) = call(addr, method, path, None, request).await;
        assert_eq!(st, 200, "{body}");
        assert_eq!(body["data"]["enabled"], true);
        assert_eq!(body["data"]["pin"], pin);
        wait("worker heartbeat", || std::path::Path::new(&f.file(HEARTBEAT_FILE)).exists()).await;
        assert!(c.scenario.worker_running());
        let lease = fs::read_to_string(f.file(LEASE_FILE)).unwrap();
        assert_eq!(lease.lines().count(), 1);
        assert_eq!(call(addr, method, path, None, request).await.0, 200);
        assert_eq!(fs::read_to_string(f.file(LEASE_FILE)).unwrap(), lease);
        let (st, body) = call(addr, "PUT", "/api/scenario/enabled", None, r#"{"enabled":false}"#).await;
        assert_eq!(st, 200, "{body}");
        assert_eq!(body["data"]["disabled_state"], DISABLED_PAUSED);
        assert!(body["data"]["pin"].is_null());
        assert_dormant(&c, &f);
    }
    assert_eq!(f.log().matches("u60d runs the engine").count(), 2);
    f.calls();
    assert_eq!(call(addr, "PUT", "/api/scenario/enabled", None, r#"{"enabled":false}"#).await.0, 200);
    assert_eq!(call(addr, "PUT", "/api/scenario/enabled", None, r#"{"enabled":"true"}"#).await.0, 400);
    assert_eq!(call(addr, "POST", "/api/scenario/pin", None, r#"{"id":"missing"}"#).await.0, 400);
    assert_dormant(&c, &f);
    assert!(f.calls().is_empty());
}

#[tokio::test]
async fn starting_stopping_scenario_finishes_cleanup_without_repair() {
    let f = Fake::new("stopping-recovery");
    f.write_config(&sc::template());
    fs::write(f.file(DISABLED_FLAG), DISABLED_STOPPING).unwrap();
    fs::write(
        f.file(STATE_FILE),
        serde_json::to_vec(&sc::RunState {
            current: "abroad".into(),
            ..Default::default()
        })
        .unwrap(),
    )
    .unwrap();
    fs::write(f.file(PIN_FILE), "abroad").unwrap();
    fs::write(f.file(RESTORE_FILE), serde_json::to_vec(&[sc::chill_on_restore(0)]).unwrap()).unwrap();
    fs::write(f.file(HEARTBEAT_FILE), "13").unwrap();
    fs::write(f.file(LEASE_FILE), "99999999 old").unwrap();
    let c = compat(&f);
    let addr = start(c.clone()).await;
    c.resume_scenario().await;
    let (st, body) = call(addr, "GET", "/api/scenario", None, "").await;
    assert_eq!(st, 200);
    assert_eq!(body["data"]["disabled_state"], DISABLED_PAUSED);
    assert_eq!(body["data"]["current"], "abroad");
    assert!(body["data"]["pin"].is_null());
    assert_eq!(body["data"]["pending_restore"], json!([]));
    assert_dormant(&c, &f);
    assert_eq!(f.calls(), ["auto-sleep true"]);
    assert!(!f.log().contains("[bootsafe]"));
}

#[tokio::test]
async fn a_scenario_scan_in_flight_cannot_apply_after_off() {
    let f = Fake::new("stop-during-scan");
    let mut cfg = sc::template();
    cfg.params.enter_hits = 1;
    cfg.scenarios[0].detect = sc::Detect::Ssid { entries: vec![SsidEntry { ssid: "TestHome".into(), bssid: None }] };
    f.write_config(&cfg);
    f.scan_returns(vec![net("TestHome", "02:00:00:00:00:01", -50.0)]);
    fs::write(f.file(DISABLED_FLAG), DISABLED_PAUSED).unwrap();
    let (entered, observed) = std::sync::mpsc::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    f.d().scan_gate = Some((entered, blocked));
    let c = compat(&f);
    let addr = start(c.clone()).await;
    let (_, body) = call(addr, "PUT", "/api/scenario/enabled", None, r#"{"enabled":true}"#).await;
    assert_eq!(body["data"]["enabled"], true);
    crate::state::blocking(move || observed.recv_timeout(Duration::from_secs(3)).unwrap()).await;
    let off = tokio::spawn(call(addr, "PUT", "/api/scenario/enabled", None, r#"{"enabled":false}"#));
    wait("stopping", || fs::read_to_string(f.file(DISABLED_FLAG)).is_ok_and(|s| s == DISABLED_STOPPING)).await;
    assert!(!off.is_finished());
    assert_eq!(call(addr, "PUT", "/api/scenario/enabled", None, r#"{"enabled":true}"#).await.0, 409);
    assert_eq!(call(addr, "POST", "/api/scenario/pin", None, r#"{"id":"away"}"#).await.0, 409);
    release.send(()).unwrap();
    let (st, body) = off.await.unwrap();
    assert_eq!(st, 200);
    assert_eq!(body["data"]["disabled_state"], DISABLED_PAUSED);
    assert_dormant(&c, &f);
    assert_eq!(f.calls(), ["scan", "auto-sleep true"]);
}

/// The device's Io reaches the agent-shaped routes, as source `scenario`.
#[tokio::test]
async fn the_engine_reaches_its_own_routes() {
    let root = std::env::temp_dir().join(format!("u60d-compat-scen-io-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("data/scenario")).unwrap();
    let c = Compat::new(root.to_str().unwrap(), Sources::none()).with_writes(true);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(serve(l, c.clone()));
    // the engine is not running here: the route answers 503
    let io = (crate::engines::scenario::device_io(format!("http://127.0.0.1:{port}")))(&c, Arc::new(AtomicBool::new(true)));
    let (status, body) = tokio::task::spawn_blocking(move || io.get("/api/scenario/log")).await.unwrap();
    assert_eq!(status, 503, "{body}");
    let _ = fs::remove_dir_all(&root);
}

/// Wedged is judged on the monotonic clock the engine stamps (suspend not counted): a heartbeat
/// file left old by a long sleep is not a stuck engine, and nothing is judged before the first.
#[test]
fn wedged_only_past_the_longest_legitimate_gap() {
    assert!(!crate::engines::scenario::wedged(None));
    assert!(!crate::engines::scenario::wedged(Some(Duration::from_secs(899))));
    assert!(crate::engines::scenario::wedged(Some(luyaz_scenario::WEDGED + Duration::from_secs(1))));
    let f = Fake::new("u60d-wedged");
    fs::write(f.file(luyaz_scenario::HEARTBEAT_FILE), "5\n").unwrap(); // ancient, as after a sleep
    let e = luyaz_scenario::Engine::new(f.clone());
    assert_eq!(e.since_beat(), None);
    e.heartbeat();
    assert!(!crate::engines::scenario::wedged(e.since_beat()));
    // the agent waits for u60d's lease past u60d's watchdog
    assert!(luyaz_scenario::HANDBACK_WAIT > luyaz_scenario::WEDGED + Duration::from_secs(2));
}
