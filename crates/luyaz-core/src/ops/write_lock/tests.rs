//! 严格版写锁（decision-execution-loop §4.3 E2）：拿不到就回 Busy，不照做；宽松版照旧。
//! 另一个「进程」用同一文件的另一次打开拿着锁（flock 按打开的文件算，同进程里也互斥）。

use super::*;
use std::path::PathBuf;

fn lock_path() -> PathBuf {
    std::env::temp_dir().join(format!("u60d-write-lock-{:016x}", rand::random::<u64>()))
}

fn hold(path: &std::path::Path) -> File {
    let f = open(path.to_str().unwrap()).unwrap();
    f.try_lock_exclusive().unwrap();
    f
}

#[tokio::test]
async fn strict_refuses_when_someone_else_holds_it() {
    let p = lock_path();
    let other = hold(&p);
    let start = tokio::time::Instant::now();
    let got =
        acquire_strict_path(Some(p.to_str().unwrap().into()), Duration::from_millis(200)).await;
    assert_eq!(got.err(), Some(Busy));
    assert!(start.elapsed() >= Duration::from_millis(200));
    // 回 Busy 后什么都没拿着：对方一放，别人马上拿得到
    drop(other);
    // 一点余量：零等待时一次 try_lock 偶尔失败（10-20 在容器里约每十次一回）
    let again =
        acquire_strict_path(Some(p.to_str().unwrap().into()), Duration::from_millis(500)).await;
    assert!(again.unwrap().held());
    let _ = std::fs::remove_file(&p);
}

#[tokio::test]
async fn strict_waits_for_a_release_inside_its_window() {
    let p = lock_path();
    let other = hold(&p);
    let t = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(150));
        drop(other);
    });
    let got = acquire_strict_path(Some(p.to_str().unwrap().into()), Duration::from_secs(5)).await;
    assert!(got.unwrap().held());
    t.join().unwrap();
    let _ = std::fs::remove_file(&p);
}

#[tokio::test]
async fn strict_refuses_an_unopenable_lock_file() {
    let p = lock_path().join("no-such-dir").join("lock");
    let got = acquire_strict_path(Some(p.to_str().unwrap().into()), Duration::ZERO).await;
    assert_eq!(got.err(), Some(Busy));
    // 宽松版照旧照做（fail-open）
    assert!(!acquire_path(Some(p.to_str().unwrap().into())).await.held());
}

#[tokio::test]
async fn strict_with_no_lock_configured_counts_as_held() {
    assert!(acquire_strict_path(None, Duration::ZERO)
        .await
        .unwrap()
        .held());
}
