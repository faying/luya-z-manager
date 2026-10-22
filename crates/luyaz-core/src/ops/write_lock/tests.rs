//! 写锁拿不到就不写（D7）：被占等满、文件打不开都回 Refused，`fut` 一次都没跑；等锁时执行者照常做别的任务。
//! 另一个「进程」用同一文件的另一次打开拿着锁（flock 按打开的文件算，同进程里也互斥）。

use super::*;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

fn lock_path() -> PathBuf {
    std::env::temp_dir().join(format!("u60d-write-lock-{:016x}", rand::random::<u64>()))
}

fn hold(path: &std::path::Path) -> File {
    let f = open(path.to_str().unwrap()).unwrap();
    f.try_lock_exclusive().unwrap();
    f
}

async fn exec() -> (
    crate::executor::Executor,
    crate::ubus::mock_ubusd::MockUbusd,
) {
    let m = crate::ubus::mock_ubusd::MockUbusd::start_new().await;
    let e = crate::executor::Executor::spawn(
        m.backend(Duration::from_millis(50)),
        Arc::new(crate::block::Hub::new(
            vec![],
            Box::new(crate::block::NoSink),
        )),
        crate::executor::Config::default(),
        Duration::from_secs(1),
    );
    (e, m)
}

/// 一次「写」：数它跑了几次。
fn job(n: &Arc<AtomicU32>) -> impl Future<Output = ()> + Send + 'static {
    let n = n.clone();
    async move {
        n.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn refuses_when_someone_else_holds_it_and_the_executor_keeps_working() {
    let p = lock_path();
    let other = hold(&p);
    let (e, _m) = exec().await;
    let n = Arc::new(AtomicU32::new(0));
    let start = tokio::time::Instant::now();
    let waiting = tokio::spawn({
        let (e, n, p) = (e.clone(), n.clone(), p.clone());
        async move {
            run_path(
                &e,
                Some(p.to_str().unwrap().into()),
                Duration::from_millis(600),
                job(&n),
            )
            .await
        }
    });
    // 等锁的时候执行者不被占着：别的任务（采集轮也是执行者任务）马上做完
    tokio::time::sleep(Duration::from_millis(100)).await;
    let t = tokio::time::Instant::now();
    assert_eq!(e.task(async { 7 }).await, 7);
    assert!(
        t.elapsed() < Duration::from_millis(300),
        "{:?}",
        t.elapsed()
    );
    let got = waiting.await.unwrap();
    assert!(matches!(got, Err(Refused::Contended { .. })), "{got:?}");
    assert_eq!(got.unwrap_err().class(), "contended");
    assert!(start.elapsed() >= Duration::from_millis(600));
    assert_eq!(n.load(Ordering::SeqCst), 0, "nothing written");
    // 回绝后什么都没拿着：对方一放，新的请求就能做
    drop(other);
    run_path(
        &e,
        Some(p.to_str().unwrap().into()),
        Duration::from_secs(2),
        job(&n),
    )
    .await
    .unwrap();
    assert_eq!(n.load(Ordering::SeqCst), 1);
    let _ = std::fs::remove_file(&p);
}

#[tokio::test(flavor = "current_thread")]
async fn waits_for_a_release_inside_its_window_then_writes_holding_it() {
    let p = lock_path();
    let other = hold(&p);
    let t = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        drop(other);
    });
    let (e, _m) = exec().await;
    let q = p.clone();
    let held = run_path(
        &e,
        Some(p.to_str().unwrap().into()),
        Duration::from_secs(5),
        async move {
            // 写的时候锁在自己手里
            hold_fails(&q)
        },
    )
    .await
    .unwrap();
    assert!(held);
    t.join().unwrap();
    let _ = std::fs::remove_file(&p);
}

fn hold_fails(p: &std::path::Path) -> bool {
    open(p.to_str().unwrap())
        .unwrap()
        .try_lock_exclusive()
        .is_err()
}

#[tokio::test(flavor = "current_thread")]
async fn refuses_an_unopenable_lock_file_without_writing() {
    let p = lock_path().join("no-such-dir").join("lock");
    let (e, _m) = exec().await;
    let n = Arc::new(AtomicU32::new(0));
    let got = run_path(
        &e,
        Some(p.to_str().unwrap().into()),
        Duration::from_secs(5),
        job(&n),
    )
    .await;
    assert!(matches!(got, Err(Refused::Open(_))), "{got:?}");
    assert_eq!(got.unwrap_err().class(), "open_failed");
    assert_eq!(n.load(Ordering::SeqCst), 0);
    // 控制任务那条一样
    let got = run_control_path(
        &e,
        Some(p.to_str().unwrap().into()),
        Duration::ZERO,
        job(&n),
    )
    .await
    .unwrap();
    assert!(matches!(got, Err(Refused::Open(_))));
    assert_eq!(n.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn no_lock_configured_writes() {
    let (e, _m) = exec().await;
    let n = Arc::new(AtomicU32::new(0));
    run_path(&e, None, Duration::ZERO, job(&n)).await.unwrap();
    assert_eq!(n.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn wait_forever_gets_it_after_the_other_writer_lets_go() {
    let p = lock_path();
    let other = hold(&p);
    TEST.with(|t| *t.borrow_mut() = Some((Some(p.to_str().unwrap().into()), WAIT)));
    let start = tokio::time::Instant::now();
    let t = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(1500));
        drop(other);
    });
    let l = wait_forever().await;
    assert!(start.elapsed() >= Duration::from_millis(1400));
    assert!(hold_fails(&p));
    drop(l);
    t.join().unwrap();
    TEST.with(|t| *t.borrow_mut() = None);
    let _ = std::fs::remove_file(&p);
}
