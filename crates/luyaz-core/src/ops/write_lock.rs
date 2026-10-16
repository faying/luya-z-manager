//! 跨进程写锁（D29，write-op-layer.md E-O2）：`/var/run/luyaz-write.lock`（flock）。
//!
//! datad 每个写步骤、以及启动时处理 takeover/pending 的全过程都拿着它；触屏/agent 的应急直写脚本
//! （`luyaz-fallback.sh`）从判定 datad 不在到写完也拿着它。拿不到就等（每 50 ms 试一次）；
//! 等满 15 秒还拿不到就记一行、不拿锁照做（比卡住执行者让看门狗杀掉好；15 秒小于订阅方判卡死的 20 秒）。
//! 拿着锁的这段在执行者里是「写的上下文」（`executor::write_scope`）：调用超时会关闸（V2-33）。
//! `LUYAZ_WRITE_LOCK` 指定路径，空串 = 不用锁。文件打不开时只记一次日志。
//! 哨兵、`luyaz-fallback.sh` 拿同一个文件（Luya-Z 改名 E2 前叫 `/var/run/u60-write.lock`，过渡期拿过一对，已去旧）。

use fs2::FileExt;
use std::{
    fs::{File, OpenOptions},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

const WAIT: Duration = Duration::from_secs(15);
const STEP: Duration = Duration::from_millis(50);

/// 拿着锁；丢掉就放。
pub struct WriteLock {
    file: Option<File>,
    /// 真拿着（或者配置成不用锁）；等满 15 秒照做时为 false（D3：这时不单独拿 Wi-Fi 锁）。
    held: bool,
    _scope: crate::executor::WriteScope,
}

impl WriteLock {
    pub fn held(&self) -> bool {
        self.held
    }
}

impl Drop for WriteLock {
    fn drop(&mut self) {
        if let Some(f) = &self.file {
            let _ = FileExt::unlock(f);
        }
    }
}

fn held(file: Option<File>) -> WriteLock {
    WriteLock {
        held: true,
        file,
        _scope: crate::executor::write_scope(),
    }
}

fn not_held() -> WriteLock {
    WriteLock {
        held: false,
        file: None,
        _scope: crate::executor::write_scope(),
    }
}

fn path() -> Option<String> {
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

static OPEN_FAILED: AtomicBool = AtomicBool::new(false);

pub async fn acquire() -> WriteLock {
    acquire_path(path()).await
}

/// `acquire`，路径由调用方给（`None` = 不用锁）。
pub(crate) async fn acquire_path(path: Option<String>) -> WriteLock {
    let lock = wait_for(path).await;
    // 等锁最多 15 秒，不是 ubus 调用：拿到（或等满）算执行者前进一次（V2-32），后面的写调用再各自记。
    crate::executor::progress();
    lock
}

async fn wait_for(path: Option<String>) -> WriteLock {
    let Some(path) = path else {
        return held(None);
    };
    let file = match open(&path) {
        Ok(f) => f,
        Err(e) => {
            if !OPEN_FAILED.swap(true, Ordering::Relaxed) {
                eprintln!("ops: cannot open write lock {path}: {e}; writing without it");
            }
            return not_held();
        }
    };
    let start = tokio::time::Instant::now();
    loop {
        if file.try_lock_exclusive().is_ok() {
            return held(Some(file));
        }
        if start.elapsed() >= WAIT {
            eprintln!(
                "ops: write lock {path} still held after {} s, writing anyway",
                WAIT.as_secs()
            );
            return not_held();
        }
        tokio::time::sleep(STEP).await;
    }
}
