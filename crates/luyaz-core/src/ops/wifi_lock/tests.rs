//! D3 的孪生并发测试：另一个「进程」（这里是线程，各自打开文件；flock 按打开的文件算，同一进程里
//! 两个打开也互斥）拿着 Wi-Fi 锁、写锁，u60d 这边按顺序拿。单线程运行时（`#[tokio::test]` 默认），
//! 和 u60d 的执行者一样，等锁的时候不能卡住运行时。

use super::*;
use crate::ops::write_lock;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::Instant,
};

fn dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("u60d-wifi-lock-{:016x}", rand::random::<u64>()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 每次尝试做的时候看到的：(怎么拿的, 写锁在手, Wi-Fi 锁被拿着)。
type Seen = Arc<Mutex<Vec<(Mode, bool, bool)>>>;

struct Paths {
    write: String,
    wifi: String,
}

fn paths() -> Paths {
    let d = dir();
    Paths {
        write: d.join("luyaz-write.lock").to_string_lossy().into(),
        wifi: d.join("luyaz-wifi.lock").to_string_lossy().into(),
    }
}

fn cfg(p: &Paths, wait_ms: u64, tries: u32) -> Cfg {
    Cfg {
        path: Some(p.wifi.clone()),
        wait: Duration::from_millis(wait_ms),
        poll: Duration::from_millis(20),
        tries,
    }
}

/// 另一个写者拿着锁（像 guard：`exec 9>>`、flock、写进程号）。
fn lock_file(path: &str, pid: u32) -> File {
    let mut f = open(path).unwrap();
    f.lock_exclusive().unwrap();
    f.set_len(0).unwrap();
    writeln!(f, "{pid}").unwrap();
    f
}

/// 别的打开能不能拿到（拿到马上放）。
fn free(path: &str) -> bool {
    let f = open(path).unwrap();
    let ok = f.try_lock_exclusive().is_ok();
    if ok {
        FileExt::unlock(&f).unwrap();
    }
    ok
}

/// 一次尝试，和 `server/write.rs` 的控制任务同样的顺序：写锁 → `second` → 做。
/// 做的时候记下两把锁是不是都在自己手里。
async fn attempt(p: &Paths, mode: Mode, seen: &Seen) -> Option<()> {
    let wl = write_lock::acquire_path(Some(p.write.clone())).await;
    let _wifi = second(&p.wifi, mode, wl.held()).ok()?;
    seen.lock()
        .unwrap()
        .push((mode, !free(&p.write), !free(&p.wifi)));
    Some(())
}

async fn go(
    p: &Paths,
    c: &Cfg,
    caller: Caller,
) -> (Option<()>, Vec<Timeout>, Vec<(Mode, bool, bool)>) {
    let seen = Arc::new(Mutex::new(vec![]));
    let mut timeouts = vec![];
    let out = run(
        c,
        &p.wifi,
        async move { caller },
        |mode| {
            let seen = seen.clone();
            async move { attempt(p, mode, &seen).await }
        },
        |t| timeouts.push(t.clone()),
    )
    .await;
    let seen = seen.lock().unwrap().clone();
    (out, timeouts, seen)
}

#[tokio::test]
async fn waits_for_the_wifi_lock_then_writes_holding_both() {
    let p = paths();
    let wifi = p.wifi.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let holder = thread::spawn(move || {
        let f = lock_file(&wifi, 999_999);
        tx.send(()).unwrap();
        thread::sleep(std::time::Duration::from_millis(300));
        drop(f);
    });
    rx.recv().unwrap();
    let t0 = Instant::now();
    let (out, timeouts, seen) = go(&p, &cfg(&p, 5_000, 2), Caller::Remote).await;
    holder.join().unwrap();
    assert_eq!(out, Some(()));
    assert!(t0.elapsed() >= std::time::Duration::from_millis(250));
    assert!(timeouts.is_empty(), "{timeouts:?}");
    // 只做了一次，做的时候两把都在手里，进程号写进了 Wi-Fi 锁
    assert_eq!(seen, [(Mode::Own, true, true)]);
    assert_eq!(holder_pid(&p.wifi), Some(std::process::id()));
    // 做完两把都放了
    assert!(free(&p.write) && free(&p.wifi));
}

fn holder_pid(path: &str) -> Option<u32> {
    holder(path)
}

/// 反过来拿锁的一方（先 Wi-Fi 锁、再写锁，像 agent 拿着 Wi-Fi 锁经 u60d 写、或者 luyaz-fallback.sh）
/// 在 u60d 等的空当拿到写锁：u60d 等 Wi-Fi 锁时不拿着写锁，不会死锁。一轮超时后记一行、整次重来、做成。
#[tokio::test]
async fn reverse_order_holder_gets_the_write_lock_in_the_gap_and_nobody_deadlocks() {
    let p = paths();
    let (wifi, write) = (p.wifi.clone(), p.write.clone());
    let (tx, rx) = std::sync::mpsc::channel();
    let holder = thread::spawn(move || {
        let w = lock_file(&wifi, 424_242);
        tx.send(()).unwrap();
        thread::sleep(std::time::Duration::from_millis(100));
        // 拿着 Wi-Fi 锁再要写锁：u60d 此时要是拿着写锁等 Wi-Fi 锁，这里就永远拿不到
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        let f = open(&write).unwrap();
        let mut got = false;
        while Instant::now() < deadline {
            if f.try_lock_exclusive().is_ok() {
                got = true;
                break;
            }
            thread::sleep(std::time::Duration::from_millis(5));
        }
        thread::sleep(std::time::Duration::from_millis(400));
        drop(f);
        drop(w);
        got
    });
    rx.recv().unwrap();
    let (out, timeouts, seen) = go(&p, &cfg(&p, 300, 3), Caller::Remote).await;
    assert!(
        holder.join().unwrap(),
        "reverse-order holder never got the write lock"
    );
    assert_eq!(out, Some(()));
    assert_eq!(seen, [(Mode::Own, true, true)]);
    // 第一轮 300 ms 超时：记了一行，带拿着的一方的进程号，不是最后一轮
    assert_eq!(timeouts.len(), 1, "{timeouts:?}");
    assert_eq!(timeouts[0].attempt, 1);
    assert_eq!(timeouts[0].holder, Some(424_242));
    assert!(!timeouts[0].last);
    assert!(timeouts[0].waited_ms >= 300);
}

#[tokio::test]
async fn gives_up_after_the_last_try_without_writing() {
    let p = paths();
    let wifi = p.wifi.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let holder = thread::spawn(move || {
        let f = lock_file(&wifi, 777);
        tx.send(()).unwrap();
        thread::sleep(std::time::Duration::from_millis(800));
        drop(f);
    });
    rx.recv().unwrap();
    let (out, timeouts, seen) = go(&p, &cfg(&p, 100, 2), Caller::Remote).await;
    holder.join().unwrap();
    assert_eq!(out, None);
    assert!(seen.is_empty());
    assert_eq!(
        timeouts
            .iter()
            .map(|t| (t.attempt, t.last))
            .collect::<Vec<_>>(),
        [(1, false), (2, true)]
    );
    // 等的时候写锁是空的（只在每次试的一瞬间拿）
    assert!(free(&p.write));
}

/// guard、agent 拿着 Wi-Fi 锁经 u60d 写：锁文件里是调用方自己的进程号，u60d 不等、只拿写锁照做。
#[tokio::test]
async fn caller_holding_the_wifi_lock_is_not_waited_for() {
    let p = paths();
    let wifi = p.wifi.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let holder = thread::spawn(move || {
        let f = lock_file(&wifi, std::process::id());
        tx.send(()).unwrap();
        let _ = done_rx.recv_timeout(std::time::Duration::from_secs(10));
        drop(f);
    });
    rx.recv().unwrap();
    let t0 = Instant::now();
    let (out, timeouts, seen) = go(&p, &cfg(&p, 10_000, 2), Caller::Pid(std::process::id())).await;
    done_tx.send(()).unwrap();
    holder.join().unwrap();
    assert_eq!(out, Some(()));
    assert!(t0.elapsed() < std::time::Duration::from_secs(2));
    assert!(timeouts.is_empty());
    // 写锁在手，Wi-Fi 锁在调用方手里
    assert_eq!(seen, [(Mode::Covered, true, true)]);
    // 别的本机进程拿着：照等（这里等满一轮）
    let p = paths();
    let wifi = p.wifi.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let holder = thread::spawn(move || {
        let f = lock_file(&wifi, 4_000_000);
        tx.send(()).unwrap();
        thread::sleep(std::time::Duration::from_millis(300));
        drop(f);
    });
    rx.recv().unwrap();
    let (out, timeouts, _) = go(&p, &cfg(&p, 100, 1), Caller::Pid(std::process::id())).await;
    holder.join().unwrap();
    assert_eq!(out, None);
    assert_eq!(timeouts.len(), 1);
}

#[test]
fn ancestry() {
    // guard (100) → 子 shell (200) → curl (300)
    let parent = |p: u32| match p {
        300 => Some(200),
        200 => Some(100),
        100 => Some(1),
        _ => None,
    };
    assert!(covers(100, 300, parent));
    assert!(covers(300, 300, parent));
    assert!(!covers(400, 300, parent));
    assert!(!covers(1, 300, parent));
    // 自己是自己的父进程（读坏了）：不打转
    assert!(!covers(5, 9, Some));
}

#[test]
fn second_lock_rules() {
    let p = paths();
    // 写锁没真拿到：不单独拿 Wi-Fi 锁、也不碰它
    assert!(matches!(second(&p.wifi, Mode::Own, false), Ok(None)));
    assert!(!std::path::Path::new(&p.wifi).exists());
    let f = lock_file(&p.wifi, 5);
    assert!(second(&p.wifi, Mode::Own, true).is_err());
    assert!(matches!(second(&p.wifi, Mode::Covered, true), Ok(None)));
    drop(f);
    // a child another test thread is forking holds a copy of the descriptor until its exec
    for _ in 0..100 {
        if free(&p.wifi) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let l = second(&p.wifi, Mode::Covered, true).unwrap();
    assert!(l.is_some() && !free(&p.wifi));
    drop(l);
    assert!(free(&p.wifi));
}

/// 只有写 `wireless` 的三个动作拿 Wi-Fi 锁（`run_control_from` 按它分）：其余写一律不碰。
#[test]
fn only_wireless_writes_take_the_wifi_lock() {
    let covered: Vec<&str> = crate::control::ACTIONS
        .iter()
        .chain(crate::control::E4_ACTIONS)
        .copied()
        .filter(|a| covers_action(a))
        .collect();
    assert_eq!(covered, ["wifi.set_module", "wifi.apply", "wifi.reload"]);
    for a in [
        "wifi.power_save",
        "nfc.set",
        "network.set_mode",
        "cellular.set",
        "vendor.call",
    ] {
        assert!(!covers_action(a), "{a}");
    }
}
