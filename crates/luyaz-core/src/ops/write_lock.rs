//! 跨进程写锁（D29，write-op-layer.md E-O2）：`/var/run/luyaz-write.lock`（flock）。
//!
//! 核心每个写步骤、以及启动时处理 takeover/pending 的全过程都拿着它；触屏的应急直写脚本
//! （`luyaz-fallback.sh`）从判定核心不在到写完也拿着它，哨兵写 Wi-Fi 时也拿。
//! **拿不到就不写**（CEO review 10-12 D7）：等满 15 秒还被占、或锁文件打不开，回 [`Refused`]，
//! 调用方在任何写之前停下、回可见的 busy/error，不自己再重试。
//! 等锁不占执行者（[`run`]）：每次尝试是一个执行者任务，只试一次锁（不等），拿到就在同一个任务里写完；
//! 被占就放开执行者、在外面隔 [`STEP`] 再试，所以等锁时采集轮、健康照常走。
//! 拿着锁的这段在执行者里是「写的上下文」（`executor::write_scope`）：调用超时会关闸（V2-33）。
//! `LUYAZ_WRITE_LOCK` 指定路径，空串 = 不用锁。
//! 哨兵、`luyaz-fallback.sh` 拿同一个文件（Luya-Z 改名 E2 前叫 `/var/run/u60-write.lock`，过渡期拿过一对，已去旧）。

use fs2::FileExt;
use std::{
    fs::{File, OpenOptions},
    future::Future,
    time::Duration,
};

pub const WAIT: Duration = Duration::from_secs(15);
pub const STEP: Duration = Duration::from_millis(100);

/// 拿着锁；丢掉就放。
pub struct WriteLock {
    file: Option<File>,
    _scope: crate::executor::WriteScope,
}

impl Drop for WriteLock {
    fn drop(&mut self) {
        if let Some(f) = &self.file {
            let _ = FileExt::unlock(f);
        }
    }
}

/// 没拿到锁，什么都没写。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    /// 等满还被别人拿着。
    Contended { waited_ms: u64 },
    /// 锁文件打不开（错误原文，不含私人信息）。
    Open(String),
}

impl Refused {
    /// 记账、回复用的分类。
    pub fn class(&self) -> &'static str {
        match self {
            Refused::Contended { .. } => "contended",
            Refused::Open(_) => "open_failed",
        }
    }
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::Contended { waited_ms } => {
                write!(f, "write lock busy after {waited_ms} ms; nothing written")
            }
            Refused::Open(e) => write!(f, "write lock cannot be opened ({e}); nothing written"),
        }
    }
}

#[cfg(test)]
thread_local! {
    /// 测试用：(路径, 最长等待)。单线程运行时里整个请求（含执行者）都在测试线程上，不影响别的测试。
    pub(crate) static TEST: std::cell::RefCell<Option<(Option<String>, Duration)>> =
        const { std::cell::RefCell::new(None) };
}

fn cfg() -> (Option<String>, Duration) {
    #[cfg(test)]
    if let Some(c) = TEST.with(|t| t.borrow().clone()) {
        return c;
    }
    (path(), WAIT)
}

pub(crate) fn path() -> Option<String> {
    match std::env::var("LUYAZ_WRITE_LOCK") {
        Ok(v) if v.is_empty() => None,
        Ok(v) => Some(v),
        Err(_) => Some("/var/run/luyaz-write.lock".into()),
    }
}

fn open(path: &str) -> std::io::Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
}

/// 试一次（不等）。`Ok(None)` = 别人拿着；`None` 路径 = 不用锁，算拿到。
pub(crate) fn try_take(path: Option<&str>) -> Result<Option<WriteLock>, Refused> {
    let Some(path) = path else {
        return Ok(Some(WriteLock {
            file: None,
            _scope: crate::executor::write_scope(),
        }));
    };
    let file = open(path).map_err(|e| Refused::Open(e.kind().to_string()))?;
    if file.try_lock_exclusive().is_err() {
        return Ok(None);
    }
    Ok(Some(WriteLock {
        file: Some(file),
        _scope: crate::executor::write_scope(),
    }))
}

/// 一次尝试的结果：拿到就做完了，被占就把没动过的 `fut` 还回来。
enum Try<T, F> {
    Done(T),
    Again(F),
}

/// 一次尝试的执行者任务：拿到锁就在这个任务里把 `fut` 做完（拿锁和写之间没有空当）。
async fn attempt<T, F>(path: Option<String>, fut: F) -> Result<Try<T, F>, Refused>
where
    F: Future<Output = T>,
{
    let lock = try_take(path.as_deref())?;
    // 试锁不是 ubus 调用：算执行者前进一次（V2-32），后面的写调用再各自记。
    crate::executor::progress();
    match lock {
        Some(lock) => {
            let out = fut.await;
            drop(lock);
            Ok(Try::Done(out))
        }
        None => Ok(Try::Again(fut)),
    }
}

/// 拿着写锁做 `fut`（内部任务，不计控制队列上限）。拿不到回 [`Refused`]，`fut` 一次都没跑。
pub async fn run<T, F>(exec: &crate::executor::Executor, fut: F) -> Result<T, Refused>
where
    T: Send + 'static,
    F: Future<Output = T> + Send + 'static,
{
    let (path, wait) = cfg();
    run_path(exec, path, wait, fut).await
}

/// `run`，路径和最长等待由调用方给（测试用）。
pub(crate) async fn run_path<T, F>(
    exec: &crate::executor::Executor,
    path: Option<String>,
    wait: Duration,
    fut: F,
) -> Result<T, Refused>
where
    T: Send + 'static,
    F: Future<Output = T> + Send + 'static,
{
    let r = waiting(path.clone(), wait, fut, |p, f| {
        let exec = exec.clone();
        async move { Ok::<_, crate::executor::Busy>(exec.task(attempt(p, f)).await) }
    })
    .await;
    r.expect("internal tasks are unbounded")
}

/// `run`，但每次尝试是一个控制任务（`/control`：排队满 8 个回 `executor::Busy`，V2-25）。
pub async fn run_control<T, F>(
    exec: &crate::executor::Executor,
    fut: F,
) -> Result<Result<T, Refused>, crate::executor::Busy>
where
    T: Send + 'static,
    F: Future<Output = T> + Send + 'static,
{
    let (path, wait) = cfg();
    run_control_path(exec, path, wait, fut).await
}

/// `run_control`，路径和最长等待由调用方给（测试用）。
pub(crate) async fn run_control_path<T, F>(
    exec: &crate::executor::Executor,
    path: Option<String>,
    wait: Duration,
    fut: F,
) -> Result<Result<T, Refused>, crate::executor::Busy>
where
    T: Send + 'static,
    F: Future<Output = T> + Send + 'static,
{
    waiting(path, wait, fut, |p, f| {
        let exec = exec.clone();
        async move { exec.control(attempt(p, f)).await }
    })
    .await
}

/// 在执行者外等：每隔 [`STEP`] 交一次尝试，累计满 `wait` 还被占就停。
async fn waiting<T, F, S, SF>(
    path: Option<String>,
    wait: Duration,
    fut: F,
    mut submit: S,
) -> Result<Result<T, Refused>, crate::executor::Busy>
where
    S: FnMut(Option<String>, F) -> SF,
    SF: Future<Output = Result<Result<Try<T, F>, Refused>, crate::executor::Busy>>,
{
    let start = tokio::time::Instant::now();
    let mut fut = fut;
    loop {
        match submit(path.clone(), fut).await? {
            Ok(Try::Done(out)) => return Ok(Ok(out)),
            Ok(Try::Again(f)) => fut = f,
            Err(e) => {
                eprintln!("ops: {e}");
                return Ok(Err(e));
            }
        }
        if start.elapsed() >= wait {
            let e = Refused::Contended {
                waited_ms: start.elapsed().as_millis() as u64,
            };
            eprintln!("ops: {e}");
            return Ok(Err(e));
        }
        tokio::time::sleep(STEP).await;
    }
}

/// 启动时处理 takeover/pending 用（执行者外、不能放弃：事务和退回要留着）：一直等到拿到。
/// 拿不到时每分钟记一行；文件打不开也照样隔一会再试，不会不拿锁就做。
pub async fn wait_forever() -> WriteLock {
    let (path, _) = cfg();
    let mut last_log: Option<tokio::time::Instant> = None;
    loop {
        let why = match try_take(path.as_deref()) {
            Ok(Some(l)) => return l,
            Ok(None) => "held by another writer".to_owned(),
            Err(e) => e.to_string(),
        };
        if last_log.is_none_or(|t| t.elapsed() >= Duration::from_secs(60)) {
            eprintln!("ops: start: write lock: {why}; pending changes wait for it");
            last_log = Some(tokio::time::Instant::now());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

#[cfg(test)]
mod tests;
