//! 原厂事件监听（docs/STATE_V2.md V2-31、V2-48）。
//!
//! datad 起一个长期运行的 `ubus listen <事件…>` 子进程，逐行读它的输出；每行按事件名查 [`Handler`] 表，
//! 每种事件各自（300 ms 去抖后）调它的回调：短信事件让短信读取「立即读」，Type-C 插拔和快充充电宝事件
//! 让 `typec`、`powerbank`、`charger` 块立即读，都唤醒执行者尽快开始一轮。
//! 同一种事件两次触发之间至少隔 `Handler::min_interval`（短信 2 秒）：密集时不会每 300 ms 整轮重采。
//!
//! - 这是**订阅**，不发任何请求（没有 `ubus call`、不经执行者后端），所以不违反「执行者单一在途」（V2-24/V2-28）：
//!   数据本身仍由执行者在它的采集轮里读。
//! - 只用 cli 方式（子进程），`LUYAZ_UBUS=socket` 时也一样：socket 后端在 Gate 0 之前不上机，
//!   直连 ubusd 的订阅（WATCH/SUBSCRIBE）以后再做。
//! - 子进程退出（或起不来）就退避重启：1 秒起、每次翻倍、最多 30 秒；连续跑满 60 秒算正常，退避回到 1 秒。
//!   日志只在第一次失败和退避到顶时各写一行，不刷屏。
//! - `LUYAZ_SMS_LISTEN=0` 关掉整个监听（默认开；名字是只有短信事件时起的）：短信退回按节拍读，
//!   `typec`、`powerbank` 退回 60 秒兜底轮询。
//! - 子进程不能比 datad 活得久：spawn 时设 `PR_SET_PDEATHSIG=SIGKILL`（datad 被 SIGKILL 也跟着死），
//!   datad 收到 SIGTERM/SIGINT 时先 [`shutdown`]（kill + wait 子进程）再退出。

use super::backend::cli_bin;
use std::{process::Stdio, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    sync::{mpsc, watch},
    time::{Instant, sleep},
};

/// 短信事件。
pub const EVENT: &str = "zwrt_wms_status_event";
/// Type-C 插拔：`{"cc_attch_state":1}`（拔掉为 0），B31 真机 2026-10-07。
pub const TYPEC_EVENT: &str = "BSP_TYPEC_EVENT";
/// 快充充电宝开关：`{"state":1}`，拔线后固件自己回 0。
pub const POWERBANK_EVENT: &str = "BSP_POWERBANK_EVENT";
pub const ENV_ENABLE: &str = "LUYAZ_SMS_LISTEN";
/// 短时间多次事件合并成一次。
pub const DEBOUNCE: Duration = Duration::from_millis(300);
/// 短信事件两次触发（踢执行者）之间的最小间隔，期间来的事件并进下一次。
pub const KICK_MIN_INTERVAL: Duration = Duration::from_secs(2);
/// USB 事件的最小间隔：插线和随后的角色切换要很快反映到屏幕上。
pub const USB_KICK_MIN_INTERVAL: Duration = Duration::from_secs(1);
pub const BACKOFF_MIN: Duration = Duration::from_secs(1);
pub const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// 子进程连续跑满这么久，退避回到最小值。
const HEALTHY_RUN: Duration = Duration::from_secs(60);

/// 一种事件和它的回调（去抖后调用）。
#[derive(Clone)]
pub struct Handler {
    pub event: &'static str,
    pub min_interval: Duration,
    pub fire: Arc<dyn Fn() + Send + Sync>,
}

/// 停止信号：`shutdown()` 置 true，监听任务 kill + wait 子进程后返回。
fn stop_tx() -> &'static watch::Sender<bool> {
    static STOP: std::sync::OnceLock<watch::Sender<bool>> = std::sync::OnceLock::new();
    STOP.get_or_init(|| watch::channel(false).0)
}
static TASK: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>> = std::sync::Mutex::new(None);

/// datad 退出前调：让监听子进程退出并收尸，最多等 `limit`。
pub async fn shutdown(limit: Duration) {
    stop_tx().send_replace(true);
    let task = TASK.lock().ok().and_then(|mut t| t.take());
    if let Some(task) = task {
        let _ = tokio::time::timeout(limit, task).await;
    }
}

/// `LUYAZ_SMS_LISTEN` 的值：只有 `0` 关闭。
pub fn enabled_from(value: Option<&str>) -> bool {
    value.map(str::trim) != Some("0")
}

/// `ubus listen` 的一行是 `events` 里的哪个事件：`{ "<事件名>": {...} }`，顶层只看第一个键。
pub fn event_of(line: &str, events: &[&str]) -> Option<usize> {
    match serde_json::from_str::<serde_json::Value>(line) {
        Ok(serde_json::Value::Object(m))
            if m.len() == 1 && m.values().all(serde_json::Value::is_object) =>
        {
            let name = m.keys().next()?;
            events.iter().position(|e| e == name)
        }
        _ => None,
    }
}

#[derive(Clone)]
pub struct Options {
    pub program: String,
    pub backoff_min: Duration,
    pub backoff_max: Duration,
    pub debounce: Duration,
}

impl Options {
    pub fn from_env() -> Self {
        Self {
            program: cli_bin(),
            backoff_min: BACKOFF_MIN,
            backoff_max: BACKOFF_MAX,
            debounce: DEBOUNCE,
        }
    }
}

/// 按环境变量起监听；关闭时返回 `false`、不起子进程。每种事件的 `fire` 在各自去抖后调用。
#[derive(Clone, Copy)]
pub enum Notice {
    Gap,
    Event(&'static str),
}
pub type Observer = Arc<dyn Fn(Notice) + Send + Sync>;

pub fn spawn_if_enabled(
    value: Option<&str>,
    opts: Options,
    handlers: Vec<Handler>,
    observe: Observer,
) -> bool {
    observe(Notice::Gap);
    if !enabled_from(value) {
        eprintln!("luyaz-core: {ENV_ENABLE}=0，不监听原厂事件");
        return false;
    }
    let mut events = Vec::new();
    let mut txs = Vec::new();
    for h in handlers {
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(debounce(rx, opts.debounce, h.min_interval, h.fire));
        events.push(h.event);
        txs.push(tx);
    }
    let task = tokio::spawn(supervise(opts, events, txs, observe));
    // 登记给 shutdown() 收尾；进程里只起一份，已有就换成新的（旧的 abort 后子进程靠 kill_on_drop 收掉）。
    if let Ok(mut slot) = TASK.lock()
        && let Some(old) = slot.replace(task)
    {
        old.abort();
    }
    true
}

/// 去抖：收到第一个事件后等 `window`，期间再来的都并进这一次，然后调一次 `fire`；
/// 离上次 `fire` 不到 `min_interval` 就等到满为止（期间的事件同样并进这一次）。
pub async fn debounce(
    mut rx: mpsc::Receiver<()>,
    window: Duration,
    min_interval: Duration,
    fire: Arc<dyn Fn() + Send + Sync>,
) {
    let mut last: Option<Instant> = None;
    while rx.recv().await.is_some() {
        sleep(window).await;
        if let Some(t) = last {
            tokio::time::sleep_until(t + min_interval).await;
        }
        while rx.try_recv().is_ok() {}
        fire();
        last = Some(Instant::now());
    }
}

/// 一直跑：起子进程、逐行读、退出就退避重启。`txs[i]` 收 `events[i]`。
pub async fn supervise(
    opts: Options,
    events: Vec<&'static str>,
    txs: Vec<mpsc::Sender<()>>,
    observe: Observer,
) {
    let mut backoff = opts.backoff_min;
    let mut failing = false;
    let names = events.join(" ");
    loop {
        let started = Instant::now();
        observe(Notice::Gap);
        let outcome = run_once(&opts.program, &events, &txs, &observe).await;
        observe(Notice::Gap);
        if txs.iter().any(|t| t.is_closed()) || *stop_tx().borrow() {
            return;
        }
        if started.elapsed() >= HEALTHY_RUN {
            backoff = opts.backoff_min;
            failing = false;
        }
        if !failing {
            eprintln!("luyaz-core: ubus listen {names} 退出（{outcome}），{backoff:?} 后重启");
            failing = true;
        }
        sleep(backoff).await;
        let next = (backoff * 2).min(opts.backoff_max);
        if next == opts.backoff_max && backoff != opts.backoff_max {
            eprintln!(
                "luyaz-core: ubus listen {names} 反复退出（{outcome}），之后每 {next:?} 重试"
            );
        }
        backoff = next;
    }
}

/// 跑一次子进程直到它退出；返回退出原因（写日志用）。
async fn run_once(
    program: &str,
    events: &[&'static str],
    txs: &[mpsc::Sender<()>],
    observe: &Observer,
) -> String {
    let mut stop = stop_tx().subscribe();
    if *stop.borrow_and_update() {
        return "datad 退出".into();
    }
    let child = crate::command::die_with_parent(&mut Command::new(program))
        .arg("listen")
        .args(events)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return format!("起不来：{e}"),
    };
    let Some(stdout) = child.stdout.take() else {
        return "没有 stdout".into();
    };
    let mut lines = BufReader::new(stdout);
    loop {
        let next = tokio::select! {
            r = bounded_line(&mut lines) => r,
            _ = stop.changed() => {
                let _ = child.kill().await; // kill + wait，不留僵尸
                return "datad 退出".into();
            }
        };
        match next {
            Ok(Some(line)) => {
                if !dispatch(&line, events, txs, observe) {
                    return "datad 退出".into();
                }
            }
            Ok(None) => break,
            Err(e) => {
                let _ = child.kill().await;
                return format!("读取出错：{e}");
            }
        }
    }
    match child.wait().await {
        Ok(s) => format!("{s}"),
        Err(e) => format!("{e}"),
    }
}

fn dispatch(
    line: &str,
    events: &[&'static str],
    txs: &[mpsc::Sender<()>],
    observe: &Observer,
) -> bool {
    if let Some(i) = event_of(line, events) {
        // Synchronous, before debounce or queueing, even with a full kick channel.
        observe(Notice::Event(events[i]));
        !matches!(
            txs[i].try_send(()),
            Err(mpsc::error::TrySendError::Closed(()))
        )
    } else {
        observe(Notice::Gap);
        true
    }
}

/// Bound a CLI line without allocating until an attacker-controlled newline arrives.
async fn bounded_line<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
) -> std::io::Result<Option<String>> {
    let mut line = Vec::new();
    loop {
        let buf = reader.fill_buf().await?;
        if buf.is_empty() {
            if line.is_empty() {
                return Ok(None);
            }
            return Err(std::io::Error::other("incomplete event line"));
        }
        let n = buf
            .iter()
            .position(|b| *b == b'\n')
            .map_or(buf.len(), |i| i + 1);
        if line.len() + n > 8192 {
            return Err(std::io::Error::other("event line exceeds 8 KiB"));
        }
        let ended = buf[n - 1] == b'\n';
        line.extend_from_slice(&buf[..n]);
        reader.consume(n);
        if ended {
            return String::from_utf8(line)
                .map(Some)
                .map_err(std::io::Error::other);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn bounded_event_lines_reject_loss_and_oversize() {
        for bytes in [b"partial".to_vec(), vec![b'x'; 8193], vec![255, b'\n']] {
            assert!(bounded_line(&mut bytes.as_slice()).await.is_err());
        }
        let mut good = b"{}\n{}\n".as_slice();
        assert_eq!(bounded_line(&mut good).await.unwrap(), Some("{}\n".into()));
        assert_eq!(bounded_line(&mut good).await.unwrap(), Some("{}\n".into()));
        assert_eq!(bounded_line(&mut good).await.unwrap(), None);
    }

    #[test]
    fn context_invalidation_is_never_debounced_or_dropped_with_kicks() {
        let (n, fire) = counter();
        let observe: Observer = Arc::new(move |_| fire());
        let (tx, _rx) = mpsc::channel(1);
        for _ in 0..100 {
            assert!(dispatch(
                r#"{"BSP_TYPEC_EVENT":{"cc_attch_state":1}}"#,
                &[TYPEC_EVENT],
                std::slice::from_ref(&tx),
                &observe
            ));
        }
        assert_eq!(n.load(Ordering::SeqCst), 100);
        assert!(dispatch(
            r#"{"BSP_TYPEC_EVENT":{},"unexpected":{}}"#,
            &[TYPEC_EVENT],
            &[tx],
            &observe
        ));
        assert_eq!(n.load(Ordering::SeqCst), 101);
    }

    fn counter() -> (Arc<AtomicUsize>, Arc<dyn Fn() + Send + Sync>) {
        let n = Arc::new(AtomicUsize::new(0));
        let m = n.clone();
        (
            n,
            Arc::new(move || {
                m.fetch_add(1, Ordering::SeqCst);
            }),
        )
    }

    const ALL: &[&str] = &[EVENT, TYPEC_EVENT, POWERBANK_EVENT];

    #[test]
    fn sms_listen_event_line_parsed() {
        let is_event = |l: &str| event_of(l, ALL) == Some(0);
        assert!(is_event(r#"{ "zwrt_wms_status_event": { "sms_new": 1 } }"#));
        assert!(is_event(r#"{"zwrt_wms_status_event":{}}"#));
        assert!(!is_event(
            r#"{ "other_event": { "zwrt_wms_status_event": 1 } }"#
        ));
        assert!(!is_event("zwrt_wms_status_event"));
        assert!(!is_event(""));
    }

    /// V2-48：USB 事件按名字分到各自的回调（B31 真机抓到的原样）。
    #[test]
    fn usb_event_lines_parsed() {
        assert_eq!(
            event_of(r#"{ "BSP_TYPEC_EVENT": {"cc_attch_state":1} }"#, ALL),
            Some(1)
        );
        assert_eq!(
            event_of(r#"{ "BSP_POWERBANK_EVENT": {"state":1} }"#, ALL),
            Some(2)
        );
        assert_eq!(event_of(r#"{ "BSP_CHARGER_EVENT": {} }"#, ALL), None);
    }

    /// V2-31：短时间多次事件只触发一次读取。
    #[tokio::test(start_paused = true)]
    async fn sms_events_coalesced() {
        let (n, fire) = counter();
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(debounce(rx, DEBOUNCE, KICK_MIN_INTERVAL, fire));
        for _ in 0..5 {
            tx.send(()).await.unwrap();
            sleep(Duration::from_millis(50)).await;
        }
        sleep(Duration::from_millis(400)).await;
        assert_eq!(n.load(Ordering::SeqCst), 1, "300 ms 内 5 个事件合并成 1 次");
        sleep(Duration::from_secs(3)).await;
        tx.send(()).await.unwrap();
        sleep(Duration::from_millis(400)).await;
        assert_eq!(
            n.load(Ordering::SeqCst),
            2,
            "隔够 2 秒后的新事件 300 ms 后再触发一次"
        );
    }

    /// V2-31：短信事件密集时，两次触发至少隔 2 秒（去抖 300 ms 保留），不会每 300 ms 整轮重采。
    #[tokio::test(start_paused = true)]
    async fn sms_event_kicks_at_least_2s_apart() {
        let times = Arc::new(std::sync::Mutex::new(Vec::new()));
        let t = times.clone();
        let fire: Arc<dyn Fn() + Send + Sync> =
            Arc::new(move || t.lock().unwrap().push(Instant::now()));
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(debounce(rx, DEBOUNCE, KICK_MIN_INTERVAL, fire));
        let t0 = Instant::now();
        // 10 秒里每 100 ms 一个事件。
        for _ in 0..100 {
            tx.send(()).await.unwrap();
            sleep(Duration::from_millis(100)).await;
        }
        sleep(Duration::from_secs(3)).await;
        let times = times.lock().unwrap().clone();
        assert_eq!(times[0] - t0, DEBOUNCE, "第一次仍是 300 ms 去抖后就触发");
        for w in times.windows(2) {
            assert!(w[1] - w[0] >= KICK_MIN_INTERVAL, "{:?}", w[1] - w[0]);
        }
        assert!(times.len() <= 7, "10 秒里最多约 6 次：{}", times.len());
        assert!(
            *times.last().unwrap() >= t0 + Duration::from_secs(10),
            "最后一批事件也触发了"
        );
    }

    /// V2-31：`LUYAZ_SMS_LISTEN=0` 不起监听。
    #[tokio::test]
    async fn sms_listen_disabled_by_env() {
        assert!(enabled_from(None));
        assert!(enabled_from(Some("1")));
        assert!(!enabled_from(Some("0")));
        let (_, fire) = counter();
        let opts = Options {
            program: "/nonexistent/ubus".into(),
            ..Options::from_env()
        };
        let h = Handler {
            event: EVENT,
            min_interval: KICK_MIN_INTERVAL,
            fire,
        };
        assert!(!spawn_if_enabled(
            Some("0"),
            opts,
            vec![h],
            Arc::new(|_| {})
        ));
    }

    fn mock_script() -> String {
        format!("{}/tests/mock_ubus.sh", env!("CARGO_MANIFEST_DIR"))
    }

    /// V2-31：监听子进程挂掉会退避重启，重启后的事件照样送到。
    #[tokio::test]
    async fn sms_listener_restarts_after_exit() {
        let dir = std::env::temp_dir().join(format!("datad-listen-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("listen.log");
        let events = dir.join("events");
        let _ = std::fs::remove_file(&log);
        std::fs::write(&events, "{ \"zwrt_wms_status_event\": {} }\n").unwrap();
        // 每个子进程读完已有的事件后约 0.2 秒退出。环境变量只给这个测试的 mock 用。
        let script = dir.join("ubus");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nMOCK_LISTEN_LOG='{}' MOCK_LISTEN_EVENTS_FILE='{}' MOCK_LISTEN_EXIT_AFTER=0 exec sh '{}' \"$@\"\n",
                log.display(),
                events.display(),
                mock_script()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (tx, mut rx) = mpsc::channel(64);
        let opts = Options {
            program: script.display().to_string(),
            backoff_min: Duration::from_millis(50),
            backoff_max: Duration::from_millis(200),
            debounce: DEBOUNCE,
        };
        let (tx2, _rx2) = mpsc::channel(64);
        let h = tokio::spawn(supervise(
            opts,
            vec![EVENT, TYPEC_EVENT],
            vec![tx, tx2],
            Arc::new(|_| {}),
        ));
        let mut got = 0;
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while got < 3 && std::time::Instant::now() < deadline {
            if tokio::time::timeout(Duration::from_millis(500), rx.recv())
                .await
                .ok()
                .flatten()
                .is_some()
            {
                got += 1;
            }
        }
        h.abort();
        let starts = std::fs::read_to_string(&log).unwrap_or_default();
        let starts: Vec<_> = starts.lines().collect();
        assert!(starts.len() >= 3, "至少重启两次：{starts:?}");
        assert!(
            starts
                .iter()
                .all(|l| *l == format!("listen {EVENT} {TYPEC_EVENT}"))
        );
        assert!(got >= 3, "每个子进程的事件都送到了：{got}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
