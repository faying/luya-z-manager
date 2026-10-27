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

async fn wait(what: &str, f: impl Fn() -> bool) {
    for _ in 0..1000 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("never got there: {what}");
}

#[tokio::test]
async fn u60d_runs_the_scenario_engine() {
    let f = Fake::new("u60d-compat-scenario");
    let root = std::path::PathBuf::from(&f.root);
    let mut cfg = sc::template();
    cfg.scenarios[0].detect = sc::Detect::Ssid { entries: vec![SsidEntry { ssid: "TestHome".into(), bssid: None }] };
    f.write_config(&cfg);
    f.scan_returns(vec![net("TestHome", "02:00:00:00:00:01", -50.0)]);
    let fake = f.clone();
    let make: crate::engines::scenario::MakeIo = Arc::new(move |_c: &Compat, active: Arc<AtomicBool>| {
        *fake.active.lock().unwrap() = Some(active);
        fake.clone() as Arc<dyn luyaz_scenario::Io>
    });
    let c = Compat::new(root.to_str().unwrap(), Sources::none())
        .with_writes(true)
        .with_scenario_io(make.clone());
    tokio::spawn(crate::engines::scenario::run(c.clone(), make));
    let addr = start(c.clone()).await;

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
    // switched off over the web: bootsafe off the runtime thread, the reply after it
    let (status, body) = call(addr, "PUT", "/api/scenario/enabled", None, r#"{"enabled":false}"#).await;
    assert_eq!(status, 200);
    assert_eq!(body["data"]["enabled"], false);
    assert_eq!(call(addr, "PUT", "/api/scenario/enabled", None, r#"{"enabled":true}"#).await.0, 200);
    // CHILL's page, stopped: the state file as it is
    fs::create_dir_all(root.join("tmp")).unwrap();
    fs::write(root.join("tmp/chill.state"), r#"{"state":"stopped"}"#).unwrap();
    let (status, body) = call(addr, "GET", "/api/services/chill", None, "").await;
    assert_eq!(status, 200);
    assert_eq!(body["data"]["state"], "stopped");
    let _ = fs::remove_dir_all(&root);
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
