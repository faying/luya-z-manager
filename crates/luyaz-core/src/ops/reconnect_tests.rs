use super::*;
use std::sync::{Arc, Mutex};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("luyaz-reconnect-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.join("reconnect.inflight")
}

#[test]
fn session_is_up_with_a_global_address_of_either_family() {
    let v4 = "17: rmnet_data0    inet 10.0.0.2/30 brd 10.0.0.3 scope global rmnet_data0\n";
    let g6 = "240e00000000000000000000000000a1 11 40 00 00 rmnet_data0\n";
    let link6 = "fe800000000000000000000000000001 11 40 20 80 rmnet_data0\n";
    let other6 = "240e00000000000000000000000000a1 05 40 00 00 br-lan\n";
    assert_eq!(session(v4, ""), "up");
    assert_eq!(session("", g6), "up");
    assert_eq!(session("", link6), "down", "link-local is not a session");
    assert_eq!(session("", other6), "down", "another interface");
    assert_eq!(session("", ""), "down");
}

type Calls = Arc<Mutex<Vec<(u64, u64)>>>;

fn recorder(calls: &Calls, fail_enable: bool) -> impl Fn(Value) -> std::future::Ready<Result<Value, String>> {
    let calls = calls.clone();
    move |a: Value| {
        let (t, e) = (a["type"].as_u64().unwrap(), a["enable"].as_u64().unwrap());
        calls.lock().unwrap().push((t, e));
        std::future::ready(if fail_enable && e == 1 {
            Err("ubus down".into())
        } else {
            Ok(Value::Null)
        })
    }
}

#[tokio::test(start_paused = true)]
async fn redial_disables_both_then_enables_both_and_clears_the_marker() {
    let m = tmp("ok");
    let calls = Calls::default();
    let r = redial_at(&m, DOWN, recorder(&calls, false)).await;
    assert!(r.is_ok());
    assert_eq!(*calls.lock().unwrap(), [(1, 0), (2, 0), (1, 1), (2, 1)]);
    assert!(!m.exists());
}

#[tokio::test(start_paused = true)]
async fn a_failed_enable_is_retried_and_the_marker_stays_for_recovery() {
    let m = tmp("fail");
    let calls = Calls::default();
    let r = redial_at(&m, DOWN, recorder(&calls, true)).await;
    assert!(r.is_err());
    // each enable tried twice; the guard then re-enables synchronously and drops the marker
    assert_eq!(
        *calls.lock().unwrap(),
        [(1, 0), (2, 0), (1, 1), (1, 1), (2, 1), (2, 1)]
    );
    assert!(!m.exists());
}

#[tokio::test(start_paused = true)]
async fn no_marker_no_change() {
    let m = std::env::temp_dir().join("luyaz-reconnect-missing-dir/x/reconnect.inflight");
    let calls = Calls::default();
    let r = redial_at(&m, DOWN, recorder(&calls, false)).await;
    assert!(r.is_err());
    assert!(calls.lock().unwrap().is_empty(), "nothing touched");
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_redial_leaves_nothing_disabled() {
    let m = tmp("cancel");
    let calls = Calls::default();
    let fut = redial_at(&m, DOWN, recorder(&calls, false));
    // cancelled while down (core stopping, task aborted)
    let _ = tokio::time::timeout(Duration::from_secs(1), fut).await;
    assert_eq!(*calls.lock().unwrap(), [(1, 0), (2, 0)]);
    // the guard ran: marker gone (its sync enable is the real ubus, absent here)
    assert!(!m.exists());
}

#[test]
fn a_marker_left_by_a_killed_core_is_recovered_at_start() {
    let m = tmp("killed");
    std::fs::write(&m, b"redial\n").unwrap();
    let mut enabled = false;
    assert!(recover_at(&m, || enabled = true));
    assert!(enabled && !m.exists());
    assert!(!recover_at(&m, || panic!("no marker, nothing to do")));
}

#[test]
fn only_the_engine_can_find_it_never_control_screen_web_or_mcp() {
    use crate::ops::spec;
    assert!(spec::find("data.reconnect").is_none(), "/control looks up SPECS only");
    let s = spec::find_any("data.reconnect").expect("the engine drives it");
    assert_eq!(s.item, spec::DATA_SESSION);
    assert_eq!(s.rollback_capability, spec::RollbackCapability::Unsupported);
    assert_eq!(s.target(&json!({"anything": 1})), Ok(json!("up")));
}
