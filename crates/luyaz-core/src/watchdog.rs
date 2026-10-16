//! datad 内部看门狗（write-op-layer.md「活性」，D12，STATE_V2.md V2-32）。
//!
//! 一个独立的系统线程（不在 tokio 里，运行时整个卡住也照样看），每秒看一次执行者：
//! 超过上限（默认 30 秒，`LUYAZ_WATCHDOG_S`，0 = 关）没有前进、而且不是一个还在自己超时里的调用，
//! 就记一行退出进程，让 procd 拉起。这样「进程还在但卡死」最终变成「进程不在」，触屏的应急退路在这期间可用。

use crate::executor::{CALL_LIMIT, Executor};
use std::time::Duration;

pub const DEFAULT_LIMIT: Duration = Duration::from_secs(30);
/// 退出码（sysexits 的 EX_SOFTWARE）。
const EXIT_STALLED: i32 = 70;

/// `LUYAZ_WATCHDOG_S`：秒；0 = 关；没设 = 30。不会小于单次调用上限（否则正常的慢调用也会被杀）。
pub fn limit_from_env() -> Option<Duration> {
    let limit = match std::env::var("LUYAZ_WATCHDOG_S") {
        Ok(v) => match v.trim().parse::<u64>() {
            Ok(0) => return None,
            Ok(s) => Duration::from_secs(s),
            Err(_) => DEFAULT_LIMIT,
        },
        Err(_) => DEFAULT_LIMIT,
    };
    Some(limit.max(CALL_LIMIT + Duration::from_secs(1)))
}

pub fn spawn(exec: Executor, limit: Duration) {
    let spawned = std::thread::Builder::new()
        .name("watchdog".into())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(1));
                if exec.stalled(limit) {
                    eprintln!(
                        "watchdog: executor made no progress for {} ms (limit {} s), exiting",
                        exec.exec_age_ms(),
                        limit.as_secs()
                    );
                    std::process::exit(EXIT_STALLED);
                }
            }
        });
    if let Err(e) = spawned {
        eprintln!("watchdog: cannot start: {e}");
    }
}
