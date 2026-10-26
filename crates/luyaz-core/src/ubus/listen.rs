//! 原厂事件监听（docs/STATE_V2.md V2-31、V2-48）。
//!
//! 核心起一个长期运行的 `ubus listen <事件…>` 子进程，逐行读它的输出；每行按事件名查 [`Handler`] 表，
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
//! - 子进程不能比核心活得久：spawn 时设 `PR_SET_PDEATHSIG=SIGKILL`（核心被 SIGKILL 也跟着死），
//!   核心收到 SIGTERM/SIGINT 时先 [`shutdown`]（kill + wait 子进程）再退出。

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
/// SIM 热拔插：拔卡 `{"sim_hotswap":"sim hot remove"}`，插回是带 `sim_states` 的整份卡资料
/// （含个人信息，只看事件名，不留内容）。B31 真机 2026-10-19，firmware-capabilities §12.7。
pub const CARD_EVENT: &str = "zwrt_card_state_event";
/// 休眠／唤醒（`zte_topsw_mc` 发）。只看事件名：无线电代次换新（R18-P1-01，§12.8）。
pub const POWER_EVENT: &str = "zwrt_device_power_state_event";
/// ubusd 自己发的对象登记／注销：`{"ubus.object.add":{"id":…,"path":"zwrt_bsp.usb"}}`。
/// 只取 `path`，给 ubus 代次用（firmware-capabilities §12.10）。u60 真机空闲 90 秒 0 条。
pub const OBJECT_ADD_EVENT: &str = "ubus.object.add";
pub const OBJECT_REMOVE_EVENT: &str = "ubus.object.remove";
/// 订阅确认（R18-P1-01）：`ubus listen` 不回报何时登记好，核心自己 `ubus send` 这条，
/// 收到才算这一轮的订阅生效（[`Notice::Ready`]）。排在参数最后，前面的都已登记。
pub const PROBE_EVENT: &str = "luyaz_listen_probe";
/// 探针最多发几次、间隔多久；收到就停。
const PROBE_TRIES: u32 = 10;
const PROBE_EVERY: Duration = Duration::from_secs(1);
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

/// 核心退出前调：让监听子进程退出并收尸，最多等 `limit`。
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
#[derive(Clone)]
pub enum Notice {
    Gap,
    Event(&'static str),
    /// `ubus.object.add`／`ubus.object.remove` 的对象名；没有或读不懂是 `None`（当作相关）。
    Object(Option<String>),
    /// 本轮子进程收到了自己的探针：从这里起事件不会漏在登记之前（每轮至多一次）。
    Ready,
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
        return "核心退出".into();
    }
    let child = crate::command::die_with_parent(&mut Command::new(program))
        .arg("listen")
        .args(events)
        .arg(PROBE_EVENT)
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
    let seen = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let probe = tokio::spawn(send_probes(program.to_owned(), seen.clone()));
    let _probe = AbortOnDrop(probe);
    loop {
        let next = tokio::select! {
            r = bounded_line(&mut lines) => r,
            _ = stop.changed() => {
                let _ = child.kill().await; // kill + wait，不留僵尸
                return "核心退出".into();
            }
        };
        match next {
            Ok(Some(line)) => {
                // 每条探针都算 Ready：执行者那边一次 ubus 出错撤销了代次后会补发（[`reprobe`]），
                // 这个子进程还收得到，就说明订阅一直在。
                if event_of(&line, &[PROBE_EVENT]).is_some() {
                    seen.store(true, std::sync::atomic::Ordering::SeqCst);
                    observe(Notice::Ready);
                    continue;
                }
                if !dispatch(&line, events, txs, observe) {
                    return "核心退出".into();
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

struct AbortOnDrop(tokio::task::JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// 补发一条探针（R18-P1-01）：执行者的 ubus 调用出错撤销了 SIM／无线电代次，监听子进程却没断，
/// 收到这条就重新 Ready。发不出去（ubusd 真的坏了）就保持撤销。
pub fn reprobe() {
    let Ok(rt) = tokio::runtime::Handle::try_current() else {
        return;
    };
    rt.spawn(async {
        let _ = crate::command::die_with_parent(&mut Command::new(cli_bin()))
            .args(["send", PROBE_EVENT, "{}"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status()
            .await;
    });
}

/// 子进程起来后给自己发探针，直到收到或发满 [`PROBE_TRIES`] 次。发不出去只是不 Ready。
async fn send_probes(program: String, seen: Arc<std::sync::atomic::AtomicBool>) {
    for _ in 0..PROBE_TRIES {
        sleep(PROBE_EVERY / 4).await;
        if seen.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let _ = crate::command::die_with_parent(&mut Command::new(&program))
            .args(["send", PROBE_EVENT, "{}"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status()
            .await;
        sleep(PROBE_EVERY).await;
        if seen.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
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
        if matches!(events[i], OBJECT_ADD_EVENT | OBJECT_REMOVE_EVENT) {
            observe(Notice::Object(object_path(line, events[i])));
        } else {
            observe(Notice::Event(events[i]));
        }
        !matches!(
            txs[i].try_send(()),
            Err(mpsc::error::TrySendError::Closed(()))
        )
    } else {
        observe(Notice::Gap);
        true
    }
}

/// `path` of an object lifecycle line; anything unexpected is `None` (fails toward re-mint).
fn object_path(line: &str, event: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let path = v.get(event)?.get("path")?.as_str()?;
    (!path.is_empty() && path.len() <= 256).then(|| path.to_owned())
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
        let dir = std::env::temp_dir().join(format!("core-listen-{}", std::process::id()));
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
                .all(|l| *l == format!("listen {EVENT} {TYPEC_EVENT} {PROBE_EVENT}"))
        );
        assert!(got >= 3, "每个子进程的事件都送到了：{got}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// R18-P1-01：每条探针都变成 Ready（补发的也算），不算 Gap、不送给处理表。
    #[tokio::test]
    async fn every_probe_line_marks_the_subscription_ready() {
        let dir = std::env::temp_dir().join(format!("core-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let events = dir.join("events");
        std::fs::write(
            &events,
            format!("{{ \"{PROBE_EVENT}\": {{}} }}\n{{ \"{PROBE_EVENT}\": {{}} }}\n{{ \"{CARD_EVENT}\": {{}} }}\n"),
        )
        .unwrap();
        let script = dir.join("ubus");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nMOCK_LISTEN_EVENTS_FILE='{}' MOCK_LISTEN_EXIT_AFTER=0 exec sh '{}' \"$@\"\n",
                events.display(),
                mock_script()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let s2 = seen.clone();
        let observe: Observer = Arc::new(move |n| {
            s2.lock().unwrap().push(match n {
                Notice::Gap => "gap",
                Notice::Ready => "ready",
                Notice::Event(_) | Notice::Object(_) => "event",
            })
        });
        let (tx, _rx) = mpsc::channel(64);
        let out = run_once(
            &script.display().to_string(),
            &[CARD_EVENT],
            &[tx],
            &observe,
        )
        .await;
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen, ["ready", "ready", "event"], "{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }
    /// §12.10: object add/remove drive the ubus epoch through the same observer as `server.rs`.
    /// Nothing before the probe, a new value on every relevant add/remove (ABA), unrelated
    /// objects ignored, child exit clears it and the next child's probe mints again.
    #[tokio::test]
    async fn object_lifecycle_mints_the_ubus_epoch_only_after_the_probe() {
        use crate::device_profile::{collect::Collector, context::Loss};
        let dir = std::env::temp_dir().join(format!("core-objects-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let events = dir.join("events");
        let line =
            |e: &str, path: &str| format!("{{ \"{e}\": {{ \"id\": 7, \"path\": \"{path}\" }} }}\n");
        let probe = format!("{{ \"{PROBE_EVENT}\": {{}} }}\n");
        let body = [
            line(OBJECT_ADD_EVENT, "zwrt_bsp.usb"),
            probe.clone(),
            line(OBJECT_REMOVE_EVENT, "zwrt_bsp.usb"),
            line(OBJECT_ADD_EVENT, "zwrt_bsp.usb"),
            line(OBJECT_ADD_EVENT, "unrelated.object"),
            format!("{{ \"{OBJECT_REMOVE_EVENT}\": {{ \"id\": 7 }} }}\n"),
        ]
        .concat();
        std::fs::write(&events, body).unwrap();
        let script = dir.join("ubus");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nMOCK_LISTEN_EVENTS_FILE='{}' MOCK_LISTEN_EXIT_AFTER=0 exec sh '{}' \"$@\"\n",
                events.display(),
                mock_script()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let c = Arc::new(Collector::new());
        // (notice, ubus epoch right after it)
        let seen = Arc::new(std::sync::Mutex::new(Vec::<(String, Option<String>)>::new()));
        let (c2, s2) = (c.clone(), seen.clone());
        let observe: Observer = Arc::new(move |n| {
            let name = match &n {
                Notice::Gap => "gap".to_owned(),
                Notice::Ready => "ready".to_owned(),
                Notice::Event(e) => (*e).to_owned(),
                Notice::Object(p) => format!("object {}", p.as_deref().unwrap_or("?")),
            };
            match n {
                Notice::Object(p) => c2.ubus_object_event(p.as_deref()),
                Notice::Ready => c2.ubus_listening(),
                Notice::Gap => c2.invalidate_context(Loss::Ubus),
                Notice::Event(_) => {}
            }
            s2.lock()
                .unwrap()
                .push((name, c2.token().context.ubus_epoch));
        });
        let (tx1, _rx1) = mpsc::channel(64);
        let (tx2, _rx2) = mpsc::channel(64);
        let opts = Options {
            program: script.display().to_string(),
            backoff_min: Duration::from_millis(50),
            backoff_max: Duration::from_millis(50),
            debounce: DEBOUNCE,
        };
        let h = tokio::spawn(supervise(
            opts,
            vec![OBJECT_ADD_EVENT, OBJECT_REMOVE_EVENT],
            vec![tx1, tx2],
            observe,
        ));
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline
            && seen
                .lock()
                .unwrap()
                .iter()
                .filter(|(n, _)| n == "ready")
                .count()
                < 2
        {
            sleep(Duration::from_millis(50)).await;
        }
        h.abort();
        let seen = seen.lock().unwrap().clone();
        let _ = std::fs::remove_dir_all(&dir);
        let names: Vec<&str> = seen.iter().map(|(n, _)| n.as_str()).collect();
        let first = names
            .iter()
            .position(|n| *n == "ready")
            .expect("probe seen");
        let second = first
            + 1
            + names[first + 1..]
                .iter()
                .position(|n| *n == "ready")
                .unwrap();
        assert_eq!(
            &names[first - 2..=first + 6],
            [
                "gap",
                "object zwrt_bsp.usb",
                "ready",
                "object zwrt_bsp.usb",
                "object zwrt_bsp.usb",
                "object unrelated.object",
                "object ?",
                "gap",
                "gap",
            ],
            "{names:?}"
        );
        let epoch = |i: usize| seen[i].1.clone();
        assert_eq!(
            epoch(first - 1),
            None,
            "an add before the probe mints nothing"
        );
        let minted: Vec<String> = (first..=first + 2)
            .map(|i| epoch(i).expect("live"))
            .collect();
        assert!(
            minted[0] != minted[1] && minted[1] != minted[2] && minted[0] != minted[2],
            "remove + add of the same path: a new value each time"
        );
        assert_eq!(
            epoch(first + 3),
            epoch(first + 2),
            "unrelated object ignored"
        );
        assert_ne!(
            epoch(first + 4),
            epoch(first + 3),
            "unreadable path re-mints"
        );
        assert!(epoch(first + 4).is_some());
        assert_eq!(epoch(first + 5), None, "child exit clears it");
        assert!(
            (first + 5..second).all(|i| epoch(i).is_none()),
            "nothing before the next probe"
        );
        let again = epoch(second).expect("next probe mints again");
        assert!(!minted.contains(&again) && epoch(first + 4).as_ref() != Some(&again));
    }
}
