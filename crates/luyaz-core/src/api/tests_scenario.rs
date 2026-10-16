//! Step 9h: the scenario engine, u60d's with writes on.

use super::tests::call;
use super::*;
use std::fs;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use luyaz_scenario::testing::Fake;

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
