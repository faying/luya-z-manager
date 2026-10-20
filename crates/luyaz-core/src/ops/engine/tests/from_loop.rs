//! 闭环发起的执行（decision-execution-loop §4.3 E3）：退回能力没证实就不带自动退回，
//! 全局开关、Agent 批准、`rollback: Some(true)` 都打不开；写设备用严格写锁。人手动的照旧。

use super::*;

#[tokio::test(start_paused = true)]
async fn a_loop_change_without_proven_rollback_ends_no_rollback() {
    let dev = Dev::new();
    dev.s().registered = false; // 写进去了，但到点没通
    let e = engine(&dev, true, None); // 全局自动退回开着
    assert!(!spec().auto_rollback_allowed());
    let mut r = req("Only_LTE", Source::Mcp);
    r.rollback = Some(true);
    let op = op_of(&e.submit_from_loop(r, Box::new(|_| Ok(()))).await);
    assert_eq!(
        end(&settle(&e, &id(&op)).await),
        pair("unverified", "no_rollback")
    );
    assert_eq!(dev.s().writes, ["Only_LTE"], "no rollback write");
    assert_eq!(dev.s().strict, [true]);
}

#[tokio::test(start_paused = true)]
async fn a_manual_change_still_follows_the_global_switch() {
    let dev = Dev::new();
    dev.s().registered = false;
    let e = engine(&dev, true, None);
    let op = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    let fin = settle(&e, &id(&op)).await;
    assert_ne!(end(&fin), pair("unverified", "no_rollback"));
    assert_eq!(dev.s().writes.first().map(String::as_str), Some("Only_LTE"));
    assert!(
        dev.s().strict.iter().all(|s| !s),
        "manual writes stay fail-open"
    );
}

#[tokio::test(start_paused = true)]
async fn a_failed_precheck_writes_nothing_and_leaves_no_transaction() {
    let dev = Dev::new();
    let e = engine(&dev, true, None);
    let r = req("Only_LTE", Source::Mcp);
    let got = e
        .submit_from_loop(r, Box::new(|_| Err("finding_changed".into())))
        .await;
    assert!(matches!(&got, Submit::Refused(w) if w == "finding_changed"));
    assert!(dev.s().writes.is_empty());
    assert!(e.lock().active.is_none());
}

#[tokio::test(start_paused = true)]
async fn the_precheck_runs_once_inside_the_submit() {
    let dev = Dev::new();
    let e = engine(&dev, true, None);
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let s2 = seen.clone();
    let r = req("Only_LTE", Source::Mcp);
    let op = op_of(
        &e.submit_from_loop(
            r,
            Box::new(move |held| {
                s2.lock().unwrap().push(held);
                Ok(())
            }),
        )
        .await,
    );
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert_eq!(dev.s().writes.first().map(String::as_str), Some("Only_LTE"));
    let _ = id(&op);
}

/// decision-execution-loop §4.9：用户影响上网的写（事务、other_write、interrupt）各加一，
/// 闭环和自动来源的不算——闭环提议记下当时的值，变了就是 `human_wrote`。
#[tokio::test(start_paused = true)]
async fn user_writes_count_only_user_network_writes() {
    let dev = Dev::new();
    let e = engine(&dev, false, None);
    assert_eq!(e.user_writes(), 0);
    let _ = e.submit(req("Only_LTE", Source::Web)).await;
    assert_eq!(e.user_writes(), 1);
    let _ = e.other_write(Source::Scenario);
    assert_eq!(e.user_writes(), 1);
    let _ = e.other_write(Source::Screen);
    assert_eq!(e.user_writes(), 2);
    let _ = e.interrupt(Source::Web, "esim");
    assert_eq!(e.user_writes(), 3);
}
