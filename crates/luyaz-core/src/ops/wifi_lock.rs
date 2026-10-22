//! Wi-Fi 锁和两把锁的顺序（u60-platform.md Phase 3「写操作的互斥」，D3，2026-10-08）。
//!
//! 有两把跨进程锁：写锁 `/var/run/luyaz-write.lock`（[`super::write_lock`]，u60d、`luyaz-fallback.sh`）
//! 和 Wi-Fi 锁 `luyaz_wifi::LOCK_FILE`（`/tmp/luyaz-wifi.lock`，Luya-Z 改名 D2 前是 `/tmp/u60-wifi.lock`；哨兵、`chaos.sh` 也拿）。
//! 写 `wireless` uci 包的动作（[`ACTIONS`]）**先拿写锁、再拿 Wi-Fi 锁**；不会只拿着 Wi-Fi 锁。
//!
//! 机制和 guard、agent 一样：同一个文件上的 `flock(LOCK_EX|LOCK_NB)` 轮询（busybox `flock -n`、agent 的
//! `libc::flock`、这里的 fs2 在 Linux 上都是 flock(2)，互相排斥）；每次拿都重新打开（create、不截断、
//! O_CLOEXEC），放的时候关掉，从不删；拿到后把自己的进程号写进去（下面「调用方自己拿着」按它判断；谁都不杀持锁者）。
//!
//! 和 D3 原文不同的两处（都是为了不卡住、不死锁）：
//! - **等 Wi-Fi 锁时不拿着写锁、也不占执行者。** `/control` 的处理在单一执行者里跑，在里面等 150 秒会停掉
//!   采集轮、让看门狗退出；拿着写锁等 150 秒又会让别的写等满 15 秒、回绝（D7）。所以每一次
//!   尝试是一个执行者任务：拿写锁 → 试一次 Wi-Fi 锁（不等）→ 拿到就做，拿不到就两把都放、在执行者外
//!   隔 `poll` 再试。放开的这段空当就是让反过来拿锁的一方（先 Wi-Fi 再写）过去的地方，不会死锁。
//!   累计 `wait`（默认 150 秒）还拿不到算这一轮超时：记一行流水账（`lock_wait`）、整次重来；
//!   `tries` 轮都超时就不写，回 503 `busy`。
//! - **调用方自己拿着 Wi-Fi 锁时不再拿。** guard（`restore_wifi`）和 agent（`apply_locked`、`wifi_set`……）
//!   都是拿着 Wi-Fi 锁再经 u60d 写的；u60d 再去拿就是等自己的调用方，只能等到对方超时。所以锁被占时看
//!   锁文件里的进程号：是本机调用方自己或它的祖先进程（**能验证的**持锁者：哨兵，以及核心里拿着锁经回环
//!   调 `/control` 的情景引擎——`peer_pid::find_pid` 最后也认核心自己），就只拿写锁照做
//!   （互斥由调用方的锁保证）。查不到调用方进程（[`Caller::Unknown`]）、核心自己内部的请求
//!   （[`Caller::Internal`]）和局域网来的（[`Caller::Remote`]）都不算拿着，照常等，等不到就不写（D8，10-14）。
//!
//! 拿不到就不写（D7/D8）：写锁拿不到（`write_lock::Refused`）、Wi-Fi 锁文件打不开，都在写之前停下、回错；
//! 不会只拿一把就写。
//!
//! 环境变量：`LUYAZ_WIFI_LOCK`（路径，空 = 不用 Wi-Fi 锁）、`LUYAZ_WIFI_LOCK_WAIT_MS`
//! （150000）、`LUYAZ_WIFI_LOCK_POLL_MS`（1000）、`LUYAZ_WIFI_LOCK_TRIES`（2）。

use fs2::FileExt;
use std::{
    fs::{File, OpenOptions},
    future::Future,
    io::{Read, Write},
    time::Duration,
};

/// 写 `wireless` uci 包（agent 那边这把锁管的范围）的 `/control` 动作。
/// `wifi.power_save`（hotplug 脚本 + iw）和 `nfc.set` 不写 `wireless`，agent 也不为它们拿锁。
pub const ACTIONS: &[&str] = &["wifi.apply", "wifi.reload", "wifi.set_module"];

pub fn covers_action(action: &str) -> bool {
    ACTIONS.contains(&action)
}

#[derive(Clone, Debug)]
pub struct Cfg {
    /// None = 不用 Wi-Fi 锁。
    pub path: Option<String>,
    /// 一轮里最多等多久（累计）。
    pub wait: Duration,
    /// 锁被占时隔多久再看一次。
    pub poll: Duration,
    /// 最多几轮。
    pub tries: u32,
}

#[cfg(test)]
thread_local! {
    /// 测试用（同 `write_lock::TEST`）。
    pub(crate) static TEST: std::cell::RefCell<Option<Cfg>> = const { std::cell::RefCell::new(None) };
}

impl Cfg {
    pub fn from_env() -> Self {
        #[cfg(test)]
        if let Some(c) = TEST.with(|t| t.borrow().clone()) {
            return c;
        }
        let ms = |name: &str, default: u64| {
            std::env::var(name)
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|v| *v > 0)
                .unwrap_or(default)
        };
        Self {
            path: match std::env::var("LUYAZ_WIFI_LOCK") {
                Ok(v) if v.is_empty() => None,
                Ok(v) => Some(v),
                Err(_) => Some(luyaz_wifi::LOCK_FILE.into()),
            },
            wait: Duration::from_millis(ms("LUYAZ_WIFI_LOCK_WAIT_MS", 150_000)),
            poll: Duration::from_millis(ms("LUYAZ_WIFI_LOCK_POLL_MS", 1_000)),
            tries: ms("LUYAZ_WIFI_LOCK_TRIES", 2) as u32,
        }
    }
}

/// 拿着 Wi-Fi 锁；丢掉就放（关文件）。
pub struct WifiLock {
    file: File,
}

impl Drop for WifiLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

fn open(path: &str) -> std::io::Result<File> {
    // std 打开带 O_CLOEXEC：拿着锁时起的子进程不会继承它
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

/// 试一次（不等）。`Ok(None)` = 别人拿着。拿到后写进自己的进程号（和 agent、guard 一样，只是诊断用）。
pub fn try_take(path: &str) -> std::io::Result<Option<WifiLock>> {
    let mut file = open(path)?;
    if file.try_lock_exclusive().is_err() {
        return Ok(None);
    }
    let _ = file.set_len(0);
    let _ = writeln!(file, "{}", std::process::id());
    Ok(Some(WifiLock { file }))
}

/// 锁文件里记的进程号（拿着的一方写的；可能是上一个拿过的，所以只在锁被占时看）。
pub fn holder(path: &str) -> Option<u32> {
    let mut s = String::new();
    File::open(path).ok()?.read_to_string(&mut s).ok()?;
    s.trim().parse().ok().filter(|p| *p > 0)
}

enum Peek {
    Free,
    Held(Option<u32>),
}

/// 看一眼锁空不空（空的话马上放掉，不写进程号）。文件打不开回错：不拿 Wi-Fi 锁就不写（D8）。
fn peek(path: &str) -> std::io::Result<Peek> {
    let file = open(path)?;
    if file.try_lock_exclusive().is_ok() {
        let _ = FileExt::unlock(&file);
        return Ok(Peek::Free);
    }
    Ok(Peek::Held(holder(path)))
}

/// 发这个 `/control` 请求的是谁。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Caller {
    /// 局域网来的：不可能拿着设备上的锁。
    Remote,
    /// 本机连接，但 /proc 里找不到进程：不能证明拿着锁，照常等（D8）。
    Unknown,
    /// 核心自己内部的请求（旧请求队列等，没有连接）：核心从不拿着 Wi-Fi 锁发 `/control`，照常等。
    Internal,
    /// 本机连接、找到了进程：持锁者是它或它的祖先才算拿着。
    Pid(u32),
}

/// `holder` 是 `caller` 自己或它的祖先（guard 的 curl 是 guard 的子进程或孙进程）。
pub fn covers(holder: u32, caller: u32, parent: impl Fn(u32) -> Option<u32>) -> bool {
    let mut pid = caller;
    for _ in 0..8 {
        if pid == holder {
            return true;
        }
        match parent(pid) {
            Some(p) if p > 1 && p != pid => pid = p,
            _ => return false,
        }
    }
    false
}

/// `/proc/<pid>/stat` 里的父进程号。
pub fn parent_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    crate::peer_pid::parse_ppid(&stat)
}

/// 一次尝试怎么拿 Wi-Fi 锁。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// 自己拿：拿不到就放掉写锁、这次不做。
    Own,
    /// 调用方拿着（或查不到调用方）：只拿写锁就做；锁正好空了就顺手拿上。
    Covered,
}

/// 第二把没拿到。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Second {
    /// 别人拿着（`Mode::Own`）：这次不做，在执行者外等一会再来。
    Held,
    /// 锁文件打不开：不写，回错（D8）。
    Open(String),
}

/// 执行者任务里、已经拿着写锁之后：按 `mode` 拿 Wi-Fi 锁。`Ok(None)` = 调用方拿着（`Covered`）。
pub fn second(path: &str, mode: Mode) -> Result<Option<WifiLock>, Second> {
    match (try_take(path), mode) {
        (Ok(Some(l)), _) => Ok(Some(l)),
        (Ok(None), Mode::Own) => Err(Second::Held),
        (Ok(None), Mode::Covered) => Ok(None),
        (Err(e), _) => {
            eprintln!("ops: cannot open Wi-Fi lock {path}: {e}; not written");
            Err(Second::Open(e.kind().to_string()))
        }
    }
}

/// 一轮等满没拿到（记账用）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Timeout {
    pub attempt: u32,
    pub waited_ms: u64,
    pub holder: Option<u32>,
    /// 这是最后一轮：之后不再做。
    pub last: bool,
}

/// [`run`] 没做。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Gave {
    /// `tries` 轮都超时。
    TimedOut,
    /// 锁文件打不开。
    Open(String),
}

/// 在执行者外跑整个过程。`job(mode)` 是一次尝试（一个执行者任务：写锁 → [`second`] → 做），
/// 返回 None = 这次没拿到 Wi-Fi 锁、什么都没做。`caller` 只在锁被占时才查（查 /proc 有成本）。
/// 返回 Err = 没做（超时或锁文件打不开）。
pub async fn run<T, J, F, C>(
    cfg: &Cfg,
    path: &str,
    caller: C,
    mut job: J,
    mut timed_out: impl FnMut(&Timeout),
) -> Result<T, Gave>
where
    J: FnMut(Mode) -> F,
    F: Future<Output = Option<T>>,
    C: Future<Output = Caller>,
{
    let mut caller = Some(caller);
    let mut known: Option<Caller> = None;
    let tries = cfg.tries.max(1);
    for attempt in 1..=tries {
        let start = tokio::time::Instant::now();
        let mut last_holder = None;
        loop {
            let peeked = match peek(path) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("ops: cannot open Wi-Fi lock {path}: {e}; not written");
                    return Err(Gave::Open(e.kind().to_string()));
                }
            };
            let mode = match peeked {
                Peek::Free => Some(Mode::Own),
                Peek::Held(h) => {
                    last_holder = h;
                    if known.is_none()
                        && let Some(c) = caller.take()
                    {
                        known = Some(c.await);
                    }
                    // D8：只有能验证的持锁者（进程号对得上它或它的祖先）才算拿着
                    let covered = match known.unwrap_or(Caller::Unknown) {
                        Caller::Unknown | Caller::Internal | Caller::Remote => false,
                        Caller::Pid(p) => h.is_some_and(|h| covers(h, p, parent_of)),
                    };
                    covered.then_some(Mode::Covered)
                }
            };
            if let Some(mode) = mode
                && let Some(out) = job(mode).await
            {
                return Ok(out);
            }
            if start.elapsed() >= cfg.wait {
                break;
            }
            tokio::time::sleep(cfg.poll).await;
        }
        timed_out(&Timeout {
            attempt,
            waited_ms: start.elapsed().as_millis() as u64,
            holder: last_holder,
            last: attempt == tries,
        });
    }
    Err(Gave::TimedOut)
}

#[cfg(test)]
mod tests;
