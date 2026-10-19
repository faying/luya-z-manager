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
    let op = op_of(&e.submit_from_loop(r).await);
    assert_eq!(end(&settle(&e, &id(&op)).await), pair("unverified", "no_rollback"));
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
    assert!(dev.s().strict.iter().all(|s| !s), "manual writes stay fail-open");
}
