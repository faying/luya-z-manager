//! T4 执行者测试（docs/STATE_V2.md 第 7、8 节）。全部用可控时钟（`start_paused`）和进程内的脚本后端，
//! 后端的延迟、超时都是 `tokio::time::sleep`，不依赖真实时间。

use super::*;
use crate::block::{BlockSpec, Event, EventSink, FAILURE_RETRY};
use crate::ubus::backend::BackendKind;
use serde_json::json;
use std::collections::HashMap;

/// 一次调用的脚本。
#[derive(Clone, Debug)]
enum Step {
    /// 等这么久，回这个值。
    Reply(Value, Duration),
    /// 等这么久，回失败。
    Fail(Duration),
    /// 等这么久（单请求超时），回超时。
    Timeout(Duration),
    /// 永远不回（后端自己的超时也坏了）。
    Hang,
}

#[derive(Default)]
struct MockInner {
    /// (时间, "对象.方法")
    log: Vec<(Instant, String)>,
    steps: HashMap<String, VecDeque<Step>>,
    defaults: HashMap<String, Step>,
    /// 模拟设备：`set` 写进来，没有脚本的 `list` 读出去。
    device: HashMap<String, Value>,
    in_flight: usize,
    max_in_flight: usize,
    off_executor: usize,
    /// 执行者最近一次 `set_round` 的值，以及每次调用时的值 (键, 在采集轮里)。
    round: bool,
    round_log: Vec<(String, bool)>,
    usb_listings: HashMap<String, Result<Vec<u8>, UbusError>>,
    usb_listing_log: Vec<String>,
    usb_listing_delay: Duration,
}

#[derive(Clone, Default)]
struct Mock(Arc<Mutex<MockInner>>);

impl Mock {
    fn default_step(&self, key: &str, step: Step) {
        lock(&self.0).defaults.insert(key.into(), step);
    }
    fn push(&self, key: &str, step: Step) {
        lock(&self.0)
            .steps
            .entry(key.into())
            .or_default()
            .push_back(step);
    }
    fn log(&self) -> Vec<(Instant, String)> {
        lock(&self.0).log.clone()
    }
    fn names(&self) -> Vec<String> {
        self.log().into_iter().map(|(_, k)| k).collect()
    }
    fn times(&self, key: &str) -> Vec<Instant> {
        self.log()
            .into_iter()
            .filter(|(_, k)| k == key)
            .map(|(t, _)| t)
            .collect()
    }
}

impl UbusBackend for Mock {
    async fn usb_methods(&mut self, object: &str) -> Result<Vec<u8>, UbusError> {
        let (result, delay) = {
            let mut m = lock(&self.0);
            m.usb_listing_log.push(object.into());
            (
                m.usb_listings
                    .get(object)
                    .cloned()
                    .unwrap_or_else(|| Err(UbusError::Io("no synthetic listing".into()))),
                m.usb_listing_delay,
            )
        };
        tokio::time::sleep(delay).await;
        result
    }
    fn kind(&self) -> BackendKind {
        BackendKind::Socket
    }
    fn set_round(&mut self, round: bool) {
        lock(&self.0).round = round;
    }
    async fn call(&mut self, object: &str, method: &str, args: &Value) -> Result<Value, UbusError> {
        let key = format!("{object}.{method}");
        let step = {
            let mut m = lock(&self.0);
            m.log.push((Instant::now(), key.clone()));
            let round = m.round;
            m.round_log.push((key.clone(), round));
            m.in_flight += 1;
            m.max_in_flight = m.max_in_flight.max(m.in_flight);
            if !super::on_executor() {
                m.off_executor += 1;
            }
            let scripted = m.steps.get_mut(&key).and_then(VecDeque::pop_front);
            scripted.or_else(|| m.defaults.get(&key).cloned())
        };
        let r = match step {
            Some(Step::Reply(v, d)) => {
                tokio::time::sleep(d).await;
                Ok(v)
            }
            Some(Step::Fail(d)) => {
                tokio::time::sleep(d).await;
                Err(UbusError::Status {
                    object: object.into(),
                    method: method.into(),
                    code: 4,
                })
            }
            Some(Step::Timeout(d)) => {
                tokio::time::sleep(d).await;
                Err(UbusError::Timeout {
                    object: object.into(),
                    detail: format!("ubus {object}: timed out"),
                })
            }
            Some(Step::Hang) => std::future::pending().await,
            None => {
                let mut m = lock(&self.0);
                if method == "set" {
                    m.device.insert(object.into(), args.clone());
                    Ok(json!({}))
                } else {
                    Ok(m.device.get(object).cloned().unwrap_or(json!({"ok":1})))
                }
            }
        };
        lock(&self.0).in_flight -= 1;
        r
    }
}

/// 带时间的事件记录。
#[derive(Clone, Default)]
struct Timed(Arc<Mutex<Vec<(Instant, Event)>>>);
impl EventSink for Timed {
    fn emit(&self, event: &Event) {
        lock(&self.0).push((Instant::now(), event.clone()));
    }
}
impl Timed {
    fn heartbeats(&self) -> Vec<(Instant, u64)> {
        lock(&self.0)
            .iter()
            .filter_map(|(t, e)| match e {
                Event::Heartbeat(h) => Some((*t, h.seq)),
                _ => None,
            })
            .collect()
    }
    fn heartbeat_ages(&self) -> Vec<(Instant, u64)> {
        lock(&self.0)
            .iter()
            .filter_map(|(t, e)| match e {
                Event::Heartbeat(h) => Some((*t, h.exec_age_ms)),
                _ => None,
            })
            .collect()
    }
    fn blocks(&self, name: &str) -> Vec<(Instant, crate::block::BlockEvent)> {
        lock(&self.0)
            .iter()
            .filter_map(|(t, e)| match e {
                Event::Block(b) if b.name == name => Some((*t, b.clone())),
                _ => None,
            })
            .collect()
    }
}

fn spec(name: &'static str, interval_s: u64) -> BlockSpec {
    BlockSpec::new(name, name, "list", Duration::from_secs(interval_s))
}

fn setup(specs: Vec<BlockSpec>, cfg: Config, sample_ms: u64) -> (Executor, Mock, Timed) {
    let mock = Mock::default();
    let sink = Timed::default();
    let hub = Arc::new(Hub::new(specs, Box::new(sink.clone())));
    let exec = Executor::spawn(mock.clone(), hub, cfg, Duration::from_millis(sample_ms));
    (exec, mock, sink)
}

/// 没有旧采集的周期采集。
struct NoLegacy;
impl RoundDriver for NoLegacy {
    fn legacy(&self) -> BoxFuture<'static, ()> {
        Box::pin(async {})
    }
}

/// 旧采集也发两个请求（经 `executor::call`）。
struct TwoLegacyCalls;
impl RoundDriver for TwoLegacyCalls {
    fn legacy(&self) -> BoxFuture<'static, ()> {
        Box::pin(async {
            preempt().await;
            let _ = call("legacy", "one", &json!({})).await;
            preempt().await;
            let _ = call("legacy", "two", &json!({})).await;
        })
    }
}

fn secs(d: Duration) -> f64 {
    d.as_secs_f64()
}

#[tokio::test(start_paused = true)]
async fn executor_is_only_ubus_caller() {
    let (exec, mock, _) = setup(vec![spec("a", 0), spec("b", 0)], Config::default(), 500);
    mock.default_step(
        "a.list",
        Step::Reply(json!({"v":1}), Duration::from_millis(30)),
    );
    mock.default_step(
        "legacy.one",
        Step::Reply(json!({}), Duration::from_millis(20)),
    );
    mock.default_step("ctl.x", Step::Reply(json!({}), Duration::from_millis(10)));
    mock.default_step("out.y", Step::Reply(json!({}), Duration::from_millis(5)));
    exec.start_rounds(Arc::new(TwoLegacyCalls));
    // 执行者外同时来的调用（登录校验之类）和控制任务。
    let mut handles = Vec::new();
    for i in 0..20u64 {
        let e = exec.clone();
        handles.push(tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(37 * i)).await;
            assert!(!on_executor());
            e.call("out", "y", &json!({})).await.unwrap();
        }));
    }
    for i in 0..5u64 {
        let e = exec.clone();
        handles.push(tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(111 * i)).await;
            e.control(async {
                call("ctl", "x", &json!({})).await.unwrap();
                call("ctl", "x", &json!({})).await.unwrap();
            })
            .await
            .unwrap();
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    tokio::time::sleep(Duration::from_secs(3)).await;
    let m = lock(&mock.0);
    assert_eq!(m.max_in_flight, 1, "同一时间最多一个在途");
    assert_eq!(m.off_executor, 0, "每个请求都在执行者任务里发");
    let count = |k: &str| m.log.iter().filter(|(_, n)| n == k).count();
    assert_eq!(count("out.y"), 20);
    assert_eq!(count("ctl.x"), 10);
    assert!(count("a.list") >= 3 && count("legacy.two") >= 3);
    drop(m);

    // 源码里除了执行者和 ubus 模块，没有别的地方能直接起 ubus 后端或 `ubus call`。
    // u60d: read.rs (`--once`, `--evlog-stats`) and server.rs (serve mode) each
    // build the backend once and hand it to the executor; they never run in the same process.
    let sources = [
        ("read.rs", include_str!("../read.rs")),
        ("auth.rs", include_str!("../auth.rs")),
        ("control.rs", include_str!("../control.rs")),
        ("server.rs", include_str!("../server.rs")),
        ("sms.rs", include_str!("../sms.rs")),
        ("state.rs", include_str!("../state.rs")),
        ("wifi.rs", include_str!("../wifi.rs")),
        ("qos.rs", include_str!("../qos.rs")),
        ("block.rs", include_str!("../block.rs")),
        ("main.rs", include_str!("../main.rs")),
    ];
    for (name, src) in sources {
        for needle in [
            "LUYAZ_UBUS_BIN",
            "CliBackend",
            "SocketBackend",
            "UbusClient",
            "\"/bin/ubus\"",
        ] {
            assert!(!src.contains(needle), "{name} 含 {needle}");
        }
        let spawns = src.matches("Backend::from_env").count();
        assert_eq!(
            spawns,
            usize::from(name == "server.rs" || name == "read.rs"),
            "{name}：后端只在 server.rs、read.rs 各建一次，交给执行者"
        );
    }
    // V2-31、V2-48：原厂事件监听只订阅（`ubus listen`），不发请求。
    let listen = include_str!("../ubus/listen.rs");
    assert!(listen.contains(".arg(\"listen\")"));
    assert!(!listen.contains("\"call\""), "listen.rs 不能发 ubus call");
    let state = include_str!("../state.rs");
    assert!(
        state.contains("crate::executor::call("),
        "state::ubus 走执行者"
    );
}

#[tokio::test(start_paused = true)]
async fn round_three_timeouts_within_budget_plus_one_timeout() {
    let (exec, mock, sink) = setup(
        vec![spec("a", 0), spec("b", 0), spec("c", 0)],
        Config::default(),
        1000,
    );
    for k in ["a.list", "b.list", "c.list"] {
        mock.default_step(k, Step::Timeout(Duration::from_secs(2)));
    }
    let t0 = Instant::now();
    exec.round_now(async {}).await;
    let took = t0.elapsed();
    // 第 1 个 0～2 秒超时，第 2 个 2～4 秒，第 3 个没开始。
    assert_eq!(mock.names(), ["a.list", "b.list"]);
    assert!(secs(took) >= 4.0 && secs(took) <= 5.0, "{took:?}");
    assert!(took <= ROUND_BUDGET + Duration::from_secs(2));
    // 这一轮的心跳照发。
    let hb = sink.heartbeats();
    assert_eq!(hb.len(), 1);
    assert!(hb[0].0 - t0 <= Duration::from_secs(5));
    // 超时的块读失败，本来就是 stale（从没读成功过），不发 block。
    assert!(sink.blocks("a").is_empty());
}

#[tokio::test(start_paused = true)]
async fn round_rotation_reads_skipped_blocks_first() {
    let (exec, mock, _) = setup(
        vec![spec("a", 0), spec("b", 0), spec("c", 0)],
        Config::default(),
        1000,
    );
    for k in ["a.list", "b.list", "c.list"] {
        mock.default_step(k, Step::Timeout(Duration::from_secs(2)));
    }
    exec.round_now(async {}).await;
    exec.round_now(async {}).await;
    exec.round_now(async {}).await;
    // 第 1 轮 a、b（c 没轮到）；第 2 轮先读 c，再 a（b 没轮到）；第 3 轮先读 b。
    assert_eq!(
        mock.names(),
        ["a.list", "b.list", "c.list", "a.list", "b.list", "c.list"]
    );

    // 慢块不会一直把后面的块挤掉：a 每次都用满预算，b 只在第 1 轮没轮到，之后每轮先读。
    let (exec, mock, _) = setup(vec![spec("a", 0), spec("b", 0)], Config::default(), 1000);
    mock.default_step(
        "a.list",
        Step::Reply(json!({}), Duration::from_millis(3100)),
    );
    for _ in 0..4 {
        exec.round_now(async {}).await;
    }
    assert_eq!(
        mock.names(),
        [
            "a.list", "b.list", "a.list", "b.list", "a.list", "b.list", "a.list"
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn heartbeat_sent_after_over_budget_round() {
    let (exec, mock, sink) = setup(
        vec![spec("a", 0), spec("b", 0), spec("c", 0)],
        Config::default(),
        1000,
    );
    mock.default_step(
        "a.list",
        Step::Reply(json!({"x":1}), Duration::from_millis(3500)),
    );
    let t0 = Instant::now();
    exec.round_now(async {}).await;
    assert_eq!(mock.names(), ["a.list"], "超预算后不再开始新请求");
    let hb = sink.heartbeats();
    assert_eq!(hb.len(), 1);
    assert_eq!(hb[0].0 - t0, Duration::from_millis(3500));
    let seqs: Vec<u64> = lock(&sink.0)
        .iter()
        .map(|(_, e)| match e {
            Event::Block(b) => b.seq,
            Event::Heartbeat(h) => h.seq,
        })
        .collect();
    assert_eq!(seqs, [1, 2], "a 的 block 占 1，心跳占 2");
    match &lock(&sink.0)[1].1 {
        Event::Heartbeat(h) => {
            assert_eq!(h.blocks.len(), 3);
            assert!(h.blocks[0].1 > 0);
            assert_eq!((h.blocks[1].1, h.blocks[2].1), (0, 0));
        }
        e => panic!("{e:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn heartbeat_gap_at_most_10s_without_control() {
    // 最坏情况：采样间隔 5 秒、每个对象都卡到 2 秒超时（socket 后端）。
    let (exec, mock, sink) = setup(
        vec![spec("a", 0), spec("b", 0), spec("c", 0), spec("d", 0)],
        Config::default(),
        5000,
    );
    for k in ["a.list", "b.list", "c.list", "d.list"] {
        mock.default_step(k, Step::Timeout(Duration::from_secs(2)));
    }
    let t0 = Instant::now();
    exec.start_rounds(Arc::new(NoLegacy));
    tokio::time::sleep(Duration::from_secs(120)).await;
    let hb = sink.heartbeats();
    assert!(hb.len() >= 10, "{}", hb.len());
    let mut prev = t0;
    for (t, _) in &hb {
        assert!(*t - prev <= Duration::from_secs(10), "{:?}", *t - prev);
        prev = *t;
    }
    // seq 连续（心跳也占号）。
    for w in hb.windows(2) {
        assert!(w[1].1 > w[0].1);
    }
}

#[tokio::test(start_paused = true)]
async fn block_interval_respected() {
    let (exec, mock, _) = setup(
        vec![spec("slow", 5), spec("fast", 0)],
        Config::default(),
        1000,
    );
    let t0 = Instant::now();
    exec.start_rounds(Arc::new(NoLegacy));
    tokio::time::sleep(Duration::from_millis(20_500)).await;
    let slow = mock.times("slow.list");
    let fast = mock.times("fast.list");
    // 每秒一轮：间隔 0 的块每轮读，间隔 5 秒的块满 5 秒才读。
    assert_eq!(fast.len(), 20, "{fast:?}");
    assert_eq!(slow.len(), 4, "{slow:?}");
    assert_eq!(slow[0] - t0, Duration::from_secs(1));
    for w in slow.windows(2) {
        assert_eq!(w[1] - w[0], Duration::from_secs(5));
    }
}

#[tokio::test(start_paused = true)]
async fn block_failure_retried_within_5s() {
    let (exec, mock, sink) = setup(vec![spec("x", 60)], Config::default(), 1000);
    exec.round_now(async {}).await; // 第一次读成功
    mock.push("x.list", Step::Fail(Duration::ZERO));
    exec.mark_immediate(None);
    exec.round_now(async {}).await; // 读失败 → stale
    let failed_at = *mock.times("x.list").last().unwrap();
    assert!(sink.blocks("x").last().unwrap().1.stale);
    exec.start_rounds(Arc::new(NoLegacy));
    tokio::time::sleep(Duration::from_secs(30)).await;
    let times = mock.times("x.list");
    assert_eq!(
        times.len(),
        3,
        "失败后重读一次，之后按 60 秒间隔：{times:?}"
    );
    let retry = times[2] - failed_at;
    assert!(retry <= FAILURE_RETRY, "{retry:?}");
    assert!(!sink.blocks("x").last().unwrap().1.stale, "重读成功翻回");
}

#[tokio::test(start_paused = true)]
async fn block_cache_off_reads_every_round() {
    let cfg = Config {
        cache: false,
        ..Config::default()
    };
    let (exec, mock, _) = setup(vec![spec("x", 60), spec("y", 300)], cfg, 1000);
    exec.start_rounds(Arc::new(NoLegacy));
    tokio::time::sleep(Duration::from_millis(10_500)).await;
    assert_eq!(mock.times("x.list").len(), 10);
    assert_eq!(mock.times("y.list").len(), 10);
}

/// 在执行者正读着第一块（慢块，1 秒）时提交任务。
async fn during_first_block<T: Send + 'static>(
    exec: &Executor,
    jobs: Vec<BoxFuture<'static, T>>,
) -> Vec<Result<T, Busy>> {
    let e = exec.clone();
    let round = tokio::spawn(async move { e.round_now(async {}).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let out = futures_util::future::join_all(jobs.into_iter().map(|j| exec.control(j))).await;
    round.await.unwrap();
    out
}

#[tokio::test(start_paused = true)]
async fn control_runs_before_next_block() {
    let (exec, mock, _) = setup(vec![spec("a", 0), spec("b", 0)], Config::default(), 1000);
    mock.default_step("a.list", Step::Reply(json!({}), Duration::from_secs(1)));
    let job: BoxFuture<'static, ()> = Box::pin(async {
        call("ctl", "set", &json!({})).await.unwrap();
    });
    during_first_block(&exec, vec![job]).await;
    assert_eq!(mock.names(), ["a.list", "ctl.set", "b.list"]);
}

#[tokio::test(start_paused = true)]
async fn control_queue_ten_requests_eight_ordered_two_busy() {
    let (exec, mock, _) = setup(vec![spec("a", 0), spec("b", 0)], Config::default(), 1000);
    mock.default_step("a.list", Step::Reply(json!({}), Duration::from_secs(1)));
    let t0 = Instant::now();
    let order = Arc::new(Mutex::new(Vec::new()));
    let jobs: Vec<BoxFuture<'static, (usize, Instant)>> = (0..10usize)
        .map(|i| {
            let order = order.clone();
            Box::pin(async move {
                call("ctl", "set", &json!({"i":i})).await.unwrap();
                lock(&order).push(i);
                (i, Instant::now())
            }) as BoxFuture<'static, _>
        })
        .collect();
    // 结果带上返回的时间：busy 的要立即返回。
    let e = exec.clone();
    let round = tokio::spawn(async move { e.round_now(async {}).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let results = futures_util::future::join_all(jobs.into_iter().map(|j| {
        let e = exec.clone();
        async move {
            let r = e.control(j).await;
            (r, Instant::now())
        }
    }))
    .await;
    round.await.unwrap();
    for (i, (r, at)) in results.iter().enumerate() {
        if i < 8 {
            assert_eq!(r.as_ref().unwrap().0, i);
            assert!(*at - t0 >= Duration::from_secs(1), "排队的等 a 读完才做");
        } else {
            assert_eq!(*r, Err(Busy));
            assert_eq!(*at - t0, Duration::from_millis(100), "满了立即回");
        }
    }
    assert_eq!(*lock(&order), (0..8).collect::<Vec<_>>());
    let names = mock.names();
    assert_eq!(names.first().map(String::as_str), Some("a.list"));
    assert_eq!(names.last().map(String::as_str), Some("b.list"));
    assert_eq!(names.len(), 10);
    // 做完之后队列空了，又能收 8 个。
    let more: Vec<BoxFuture<'static, (usize, Instant)>> = (0..8usize)
        .map(|i| Box::pin(async move { (i, Instant::now()) }) as BoxFuture<'static, _>)
        .collect();
    assert!(
        during_first_block(&exec, more)
            .await
            .iter()
            .all(Result::is_ok)
    );
    // HTTP 层回 503 和文档里的 body。
    let resp = crate::server::control_busy("wifi.set_module");
    assert_eq!(resp.status(), axum::http::StatusCode::SERVICE_UNAVAILABLE);
    let body = axum::body::to_bytes(resp.into_body(), 1 << 16)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap(),
        json!({"ok":false,"action":"wifi.set_module","error":{"code":"busy","message":"control queue full"}})
    );
}

#[tokio::test(start_paused = true)]
async fn control_multi_call_runs_as_one_task() {
    let (exec, mock, _) = setup(vec![spec("a", 0), spec("b", 0)], Config::default(), 1000);
    mock.default_step("a.list", Step::Reply(json!({}), Duration::from_secs(1)));
    for k in ["apn.mode", "apn.auto", "apn.manu", "apn.enabled"] {
        mock.default_step(k, Step::Reply(json!({}), Duration::from_millis(400)));
    }
    let apn_list: BoxFuture<'static, ()> = Box::pin(async {
        for m in ["mode", "auto", "manu", "enabled"] {
            call("apn", m, &json!({})).await.unwrap();
        }
    });
    let other: BoxFuture<'static, ()> = Box::pin(async {
        call("ctl", "set", &json!({})).await.unwrap();
    });
    during_first_block(&exec, vec![apn_list, other]).await;
    assert_eq!(
        mock.names(),
        [
            "a.list",
            "apn.mode",
            "apn.auto",
            "apn.manu",
            "apn.enabled",
            "ctl.set",
            "b.list"
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn control_success_marks_blocks_no_second_round() {
    let (exec, mock, _) = setup(
        vec![spec("battery", 60), spec("charger", 60)],
        Config::default(),
        1000,
    );
    exec.start_rounds(Arc::new(NoLegacy));
    tokio::time::sleep(Duration::from_millis(1500)).await; // 第 1 轮在 1 秒
    assert_eq!(mock.names(), ["battery.list", "charger.list"]);
    let rounds = exec.stats().rounds.load(Ordering::Relaxed);
    let e = exec.clone();
    exec.control(async move {
        call(
            "charger",
            "set",
            &json!({"direct_power_supply_mode":"enable"}),
        )
        .await
        .unwrap();
        e.mark_immediate(Some(&["charger"]));
    })
    .await
    .unwrap();
    // 控制做完，不另起一轮。
    assert!(exec.hub().is_immediate("charger"));
    assert!(!exec.hub().is_immediate("battery"));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(exec.stats().rounds.load(Ordering::Relaxed), rounds);
    assert_eq!(mock.names().len(), 3);
    // 下一轮（2 秒）只读被标的块，满 60 秒前不读别的。
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert_eq!(exec.stats().rounds.load(Ordering::Relaxed), rounds + 1);
    assert_eq!(
        mock.names(),
        [
            "battery.list",
            "charger.list",
            "charger.set",
            "charger.list"
        ]
    );
    assert_eq!(
        exec.hub().view("charger").unwrap().data,
        json!({"direct_power_supply_mode":"enable"})
    );
    // 没有映射：全部块。
    exec.control(async {}).await.unwrap();
    exec.mark_immediate(None);
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert_eq!(mock.names()[4..], ["battery.list", "charger.list"]);
}

#[tokio::test(start_paused = true)]
async fn control_result_not_overwritten_by_older_read() {
    // 充电器先读（旧值），电池读着的时候控制改了设备并标记充电器。
    let (exec, mock, sink) = setup(
        vec![spec("charger", 60), spec("battery", 60)],
        Config::default(),
        1000,
    );
    lock(&mock.0)
        .device
        .insert("charger".into(), json!({"mode":"disable"}));
    mock.push(
        "battery.list",
        Step::Reply(json!({}), Duration::from_secs(1)),
    );
    let e = exec.clone();
    let round = tokio::spawn(async move { e.round_now(async {}).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let e = exec.clone();
    exec.control(async move {
        call("charger", "set", &json!({"mode":"enable"}))
            .await
            .unwrap();
        e.mark_immediate(Some(&["charger"]));
    })
    .await
    .unwrap();
    round.await.unwrap();
    exec.round_now(async {}).await;
    let ev = sink.blocks("charger");
    assert_eq!(
        ev.iter()
            .map(|(_, b)| b.data["mode"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["disable", "enable"]
    );
    assert!(ev[1].1.seq > ev[0].1.seq && ev[1].1.revision > ev[0].1.revision);
    assert_eq!(
        exec.hub().legacy("charger").unwrap(),
        json!({"mode":"enable"})
    );
    // 控制插在充电器读之前时，同一轮就读到新值，没有旧值被发出去。
    let (exec, mock, sink) = setup(
        vec![spec("battery", 60), spec("charger", 60)],
        Config::default(),
        1000,
    );
    lock(&mock.0)
        .device
        .insert("charger".into(), json!({"mode":"disable"}));
    mock.push(
        "battery.list",
        Step::Reply(json!({}), Duration::from_secs(1)),
    );
    let job: BoxFuture<'static, ()> = Box::pin(async {
        call("charger", "set", &json!({"mode":"enable"}))
            .await
            .unwrap();
    });
    during_first_block(&exec, vec![job]).await;
    let ev = sink.blocks("charger");
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].1.data, json!({"mode":"enable"}));
}

#[tokio::test(start_paused = true)]
async fn legacy_calls_are_preempted_by_control_and_not_budget_cut() {
    let (exec, mock, _) = setup(vec![spec("a", 0)], Config::default(), 1000);
    mock.default_step(
        "a.list",
        Step::Reply(json!({}), Duration::from_millis(3500)),
    );
    mock.default_step("legacy.one", Step::Reply(json!({}), Duration::from_secs(1)));
    let e = exec.clone();
    let round = tokio::spawn(async move {
        e.round_now(async {
            TwoLegacyCalls.legacy().await;
        })
        .await
    });
    // a 读完（3.5 秒，已超预算）之后旧采集照样全读；控制在 legacy.one 和 legacy.two 之间插进来。
    tokio::time::sleep(Duration::from_millis(4000)).await;
    exec.control(async {
        call("ctl", "set", &json!({})).await.unwrap();
    })
    .await
    .unwrap();
    round.await.unwrap();
    assert_eq!(
        mock.names(),
        ["a.list", "legacy.one", "ctl.set", "legacy.two"]
    );
}

#[tokio::test(start_paused = true)]
async fn round_skips_timed_out_object_for_rest_of_round() {
    // R2：块 x 超时后，本轮旧采集里再调 x 直接跳过（算失败、不发请求）；控制任务不受影响；下一轮恢复。
    let (exec, mock, _) = setup(vec![spec("x", 0)], Config::default(), 1000);
    mock.push("x.list", Step::Timeout(Duration::from_secs(2)));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    let e = exec.clone();
    let round = tokio::spawn(async move {
        e.round_now(async move {
            let r = call("x", "status", &json!({})).await;
            lock(&s).push(r.map_err(|e| e.is_timeout()));
            preempt().await; // 这里插进来的控制任务照常调 x
            let r = call("x", "status", &json!({})).await;
            lock(&s).push(r.map_err(|e| e.is_timeout()));
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(2500)).await;
    exec.control(async { call("x", "set", &json!({})).await.unwrap() })
        .await
        .unwrap();
    round.await.unwrap();
    assert_eq!(*lock(&seen), [Err(true), Err(true)]);
    assert_eq!(mock.names(), ["x.list", "x.set"]);
    exec.round_now(async {}).await;
    assert_eq!(mock.names(), ["x.list", "x.set", "x.list"]);
}

#[tokio::test(start_paused = true)]
async fn set_interval_changes_sleep() {
    let (exec, mock, _) = setup(vec![spec("a", 0)], Config::default(), 5000);
    exec.start_rounds(Arc::new(NoLegacy));
    tokio::time::sleep(Duration::from_millis(1000)).await;
    exec.set_interval_ms(500);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    // 正在睡的那次按新间隔重算：0.5 秒时已到期，立刻一轮，之后每 0.5 秒一轮。
    assert_eq!(mock.times("a.list").len(), 3, "{:?}", mock.names());
}

#[tokio::test]
async fn call_without_executor_fails_instead_of_hanging() {
    if GLOBAL.get().is_none() {
        let e = call("a", "b", &json!({})).await.unwrap_err();
        assert!(e.to_string().contains("executor"), "{e}");
    }
}

/// 旧采集把 live 块的数据交进来（和 `state::collect` 一样经 `Hub::record`），每轮一个新值。
struct FeedLive(Arc<Hub>, Arc<AtomicU64>);
impl RoundDriver for FeedLive {
    fn legacy(&self) -> BoxFuture<'static, ()> {
        let (hub, n) = (self.0.clone(), self.1.clone());
        Box::pin(async move {
            let n = n.fetch_add(1, Ordering::Relaxed);
            hub.record("live", Ok(json!({"system":{"uptime":n}})), Instant::now());
        })
    }
}

#[tokio::test(start_paused = true)]
async fn live_block_cadence_follows_round_length() {
    let (exec, mock, sink) = setup(
        vec![
            spec("a", 0),
            BlockSpec::derived("live", Box::new(crate::block::LivePolicy)),
        ],
        Config::default(),
        1000,
    );
    let short = Step::Reply(json!({"x":1}), Duration::from_millis(100));
    mock.default_step("a.list", short.clone());
    // 第 4 轮的 a 很慢：这一轮 3.5 秒（超出预算）。
    for _ in 0..3 {
        mock.push("a.list", short.clone());
    }
    mock.push(
        "a.list",
        Step::Reply(json!({"x":1}), Duration::from_millis(3500)),
    );
    let t0 = Instant::now();
    exec.start_rounds(Arc::new(FeedLive(exec.hub().clone(), Arc::default())));
    tokio::time::sleep(Duration::from_millis(12_000)).await;
    let live = sink.blocks("live");
    let hb = sink.heartbeats();
    // 每轮一次，和心跳一一对应、同一时刻，live 在心跳前一个号。
    assert_eq!(live.len(), hb.len());
    for (l, h) in live.iter().zip(&hb) {
        assert_eq!(l.0, h.0);
        assert_eq!(l.1.seq + 1, h.1);
    }
    // 节拍 = 采样间隔 + 这一轮的长度；慢的那一轮之后不补发。
    let gaps: Vec<u64> = live
        .windows(2)
        .map(|w| (w[1].0 - w[0].0).as_millis() as u64)
        .collect();
    assert_eq!(live[0].0 - t0, Duration::from_millis(1100));
    assert_eq!(&gaps[..4], [1100, 1100, 4500, 1100]);
    assert_eq!(
        live.len(),
        7,
        "12 秒里：1.1、2.2、3.3、7.8、8.9、10.0、11.1"
    );
    assert_eq!(
        live.iter().map(|l| l.1.revision).collect::<Vec<_>>(),
        [1, 2, 3, 4, 5, 6, 7]
    );
}

/// 旧采集里的短信读取：和 `state::ubus_ttl` 一样按 10 秒缓存，读到就记进 `sms` 块；
/// `invalidate` 相当于 `state::invalidate_sms_cache`。
struct SmsLegacy {
    hub: Arc<Hub>,
    last: Arc<Mutex<Option<Instant>>>,
}
impl RoundDriver for SmsLegacy {
    fn legacy(&self) -> BoxFuture<'static, ()> {
        let (hub, last) = (self.hub.clone(), self.last.clone());
        Box::pin(async move {
            preempt().await;
            let fresh = lock(&last).is_some_and(|t| t.elapsed() < Duration::from_secs(10));
            if fresh {
                return;
            }
            let r = call("zwrt_wms", "list", &json!({})).await;
            *lock(&last) = Some(Instant::now());
            hub.record("sms", r.map_err(|e| e.to_string()), Instant::now());
        })
    }
}

/// V2-31：短信事件到达后，不等采样间隔、不等 10 秒列表缓存，下一轮（立即开始）就把新短信读进 sms 块；
/// 一轮开始前的多次事件只多出一轮。
#[tokio::test(start_paused = true)]
async fn sms_event_updates_block_within_one_round() {
    let (exec, mock, _) = setup(
        vec![BlockSpec::derived("sms", Box::new(crate::block::OnChange))],
        Config::default(),
        5000,
    );
    mock.default_step(
        "zwrt_wms.list",
        Step::Reply(json!({"max_id":1}), Duration::from_millis(20)),
    );
    let last = Arc::new(Mutex::new(None));
    exec.start_rounds(Arc::new(SmsLegacy {
        hub: exec.hub().clone(),
        last: last.clone(),
    }));
    tokio::time::sleep(Duration::from_millis(5500)).await; // 第 1 轮在 5 秒
    assert_eq!(exec.hub().view("sms").unwrap().data, json!({"max_id":1}));
    let rounds = exec.stats().rounds.load(Ordering::Relaxed);
    // 新短信到了；事件去抖后：清短信缓存 + 踢执行者。连踢 3 次只算一次。
    mock.default_step(
        "zwrt_wms.list",
        Step::Reply(json!({"max_id":2}), Duration::from_millis(20)),
    );
    let t0 = Instant::now();
    for _ in 0..3 {
        *lock(&last) = None;
        exec.kick(&["sms"]);
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(exec.hub().view("sms").unwrap().data, json!({"max_id":2}));
    assert_eq!(exec.stats().rounds.load(Ordering::Relaxed), rounds + 1);
    assert!(Instant::now() - t0 < Duration::from_secs(1));
    assert_eq!(mock.names(), ["zwrt_wms.list", "zwrt_wms.list"]);
    // 之后照常按采样间隔（下一轮在踢的那一轮结束后 5 秒），不再多跑。
    tokio::time::sleep(Duration::from_millis(4000)).await;
    assert_eq!(exec.stats().rounds.load(Ordering::Relaxed), rounds + 1);
}

#[tokio::test(start_paused = true)]
async fn rounds_and_heartbeats_continue_under_control_flood() {
    // 持续灌控制任务和内部任务：每个安全点只做进入时已在排队的，采集轮和心跳照常发生。
    let (exec, mock, sink) = setup(vec![spec("a", 0), spec("b", 0)], Config::default(), 1000);
    mock.default_step(
        "ctl.set",
        Step::Reply(json!({}), Duration::from_millis(200)),
    );
    mock.default_step(
        "int.get",
        Step::Reply(json!({}), Duration::from_millis(100)),
    );
    let t0 = Instant::now();
    exec.start_rounds(Arc::new(NoLegacy));
    let stop = Arc::new(AtomicBool::new(false));
    let mut floods = Vec::new();
    for _ in 0..4 {
        let (e, stop) = (exec.clone(), stop.clone());
        floods.push(tokio::spawn(async move {
            while !stop.load(Ordering::Relaxed) {
                let _ = e
                    .control(async {
                        call("ctl", "set", &json!({})).await.unwrap();
                    })
                    .await;
            }
        }));
    }
    for _ in 0..2 {
        let (e, stop) = (exec.clone(), stop.clone());
        floods.push(tokio::spawn(async move {
            while !stop.load(Ordering::Relaxed) {
                let _ = e.call("int", "get", &json!({})).await;
            }
        }));
    }
    tokio::time::sleep(Duration::from_secs(60)).await;
    stop.store(true, Ordering::Relaxed);
    let hb = sink.heartbeats();
    assert!(hb.len() >= 10, "心跳只有 {} 次", hb.len());
    let mut prev = t0;
    for (t, _) in &hb {
        assert!(*t - prev <= Duration::from_secs(10), "{:?}", *t - prev);
        prev = *t;
    }
    assert!(mock.times("a.list").len() >= 10);
    assert!(mock.times("ctl.set").len() >= 100, "控制任务也一直在做");
    // V2-32：一直在前进，看门狗不会动
    assert!(exec.exec_age_ms() < 1_000, "{}", exec.exec_age_ms());
    assert!(!exec.stalled(Duration::from_secs(30)));
    for f in floods {
        f.await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn control_calls_use_control_timeout_rounds_use_round_timeout() {
    // 执行者每次调用前告诉后端在不在采集轮里：块读取是轮里（socket 2 秒），控制任务和内部任务不是（8 秒）。
    let (exec, mock, _) = setup(vec![spec("a", 0), spec("b", 0)], Config::default(), 1000);
    mock.default_step("a.list", Step::Reply(json!({}), Duration::from_secs(1)));
    let job: BoxFuture<'static, ()> = Box::pin(async {
        call("ctl", "set", &json!({})).await.unwrap();
    });
    during_first_block(&exec, vec![job]).await;
    exec.call("int", "get", &json!({})).await.unwrap();
    let log = lock(&mock.0).round_log.clone();
    let want: Vec<(String, bool)> = [
        ("a.list", true),
        ("ctl.set", false),
        ("b.list", true),
        ("int.get", false),
    ]
    .iter()
    .map(|(k, r)| (k.to_string(), *r))
    .collect();
    assert_eq!(log, want);
}

// ---- E4 T3：独立心跳与执行者进度（V2-22、V2-32）、超时关闸（V2-33） ----

#[tokio::test(start_paused = true)]
async fn long_reload_short_polls_keep_heartbeat_and_no_stall() {
    // 原厂 reload 要 60 秒：先发 reload，再用 2 秒一次的短调用轮询，执行者一直在前进。
    let (exec, mock, sink) = setup(vec![spec("a", 0)], Config::default(), 1000);
    mock.default_step(
        "wlan.reload",
        Step::Reply(json!({}), Duration::from_millis(300)),
    );
    mock.default_step(
        "wlan.status",
        Step::Reply(json!({}), Duration::from_millis(100)),
    );
    exec.start_rounds(Arc::new(NoLegacy));
    exec.start_heartbeat(Duration::from_secs(5));
    let e = exec.clone();
    let job = tokio::spawn(async move {
        e.control(async {
            call("wlan", "reload", &json!({})).await.unwrap();
            for _ in 0..30 {
                tokio::time::sleep(Duration::from_secs(2)).await;
                call("wlan", "status", &json!({})).await.unwrap();
            }
        })
        .await
    });
    let mut max_age = 0;
    for _ in 0..62 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        max_age = max_age.max(exec.exec_age_ms());
        assert!(!exec.stalled(Duration::from_secs(30)));
    }
    job.await.unwrap().unwrap();
    assert!(max_age <= 2_500, "{max_age}");
    // 控制任务占着执行者的 60 秒里，独立定时器照样补心跳（间隔不超过 10 秒），exec_age 都小
    let hb = sink.heartbeat_ages();
    let mut prev: Option<Instant> = None;
    for (t, age) in &hb {
        if let Some(p) = prev {
            assert!(*t - p <= Duration::from_secs(10), "{:?}", *t - p);
        }
        assert!(*age <= 2_500, "{age}");
        prev = Some(*t);
    }
    assert!(hb.len() >= 12, "{}", hb.len());
}

#[tokio::test(start_paused = true)]
async fn stuck_call_is_reported_and_stalls_after_limit() {
    let (exec, mock, sink) = setup(vec![spec("a", 0)], Config::default(), 1000);
    mock.default_step("dev.set", Step::Hang);
    exec.start_heartbeat(Duration::from_secs(5));
    let e = exec.clone();
    tokio::spawn(async move {
        let _ = e
            .control(async {
                let _ = call("dev", "set", &json!({})).await;
            })
            .await;
    });
    tokio::time::sleep(Duration::from_secs(9)).await;
    // 还在调用自己的超时里
    assert!(!exec.stalled(Duration::from_secs(30)));
    tokio::time::sleep(Duration::from_secs(13)).await;
    // 22 秒没前进：订阅方按 >20 秒判卡死（D12），看门狗还没到
    assert!(exec.exec_age_ms() > 20_000, "{}", exec.exec_age_ms());
    assert!(!exec.stalled(Duration::from_secs(30)));
    let last = *sink.heartbeat_ages().last().unwrap();
    assert!(last.1 > 15_000, "{last:?}");
    tokio::time::sleep(Duration::from_secs(9)).await;
    assert!(exec.stalled(Duration::from_secs(30)));
}

#[tokio::test(start_paused = true)]
async fn idle_executor_is_never_stalled() {
    let (exec, _mock, sink) = setup(vec![spec("a", 0)], Config::default(), 1000);
    exec.start_heartbeat(Duration::from_secs(5));
    tokio::time::sleep(Duration::from_secs(100)).await;
    assert_eq!(exec.exec_age_ms(), 0);
    assert!(!exec.stalled(Duration::from_secs(30)));
    // 没有采集轮也有心跳
    let hb = sink.heartbeat_ages();
    assert!(hb.len() >= 19, "{}", hb.len());
    assert!(hb.iter().all(|(_, a)| *a == 0));
}

const OBJS: [&str; 10] = ["o0", "o1", "o2", "o3", "o4", "o5", "o6", "o7", "o8", "o9"];

/// 旧采集按顺序读 o0…o9 各一次（每次前是安全点）。
struct TenLegacyObjects;
impl RoundDriver for TenLegacyObjects {
    fn legacy(&self) -> BoxFuture<'static, ()> {
        Box::pin(async {
            for o in OBJS {
                preempt().await;
                let _ = call(o, "status", &json!({})).await;
            }
        })
    }
}

/// P1-2（V2-19）：ubusd 不回时旧采集到了 `LEGACY_TIMEOUT_LIMIT` 个超时对象就不再发新请求，
/// 剩下的算跳过；排进来的控制任务不受影响；下一轮重新读。慢但在回答的轮不截（见上面的 not_budget_cut）。
#[tokio::test(start_paused = true)]
async fn legacy_reads_stop_after_timeout_limit() {
    let (exec, mock, _) = setup(vec![], Config::default(), 1000);
    for o in OBJS {
        mock.push(
            &format!("{o}.status"),
            Step::Timeout(Duration::from_secs(2)),
        );
    }
    let e = exec.clone();
    let t0 = Instant::now();
    let round = tokio::spawn(async move { e.round_now(TenLegacyObjects.legacy()).await });
    // o5 超时的时候排进来的控制任务（在截了之后的安全点做）照常调
    tokio::time::sleep(Duration::from_secs(11)).await;
    exec.control(async { call("o9", "set", &json!({})).await.unwrap() })
        .await
        .unwrap();
    round.await.unwrap();
    let sent: Vec<_> = mock
        .names()
        .into_iter()
        .filter(|n| n.ends_with(".status"))
        .collect();
    assert_eq!(sent.len(), LEGACY_TIMEOUT_LIMIT, "{sent:?}");
    assert!(mock.names().contains(&"o9.set".to_string()));
    assert!(
        t0.elapsed()
            <= Duration::from_secs(2) * LEGACY_TIMEOUT_LIMIT as u32 + Duration::from_secs(1),
        "{:?}",
        t0.elapsed()
    );
    // 下一轮：都回答了，全读
    exec.round_now(TenLegacyObjects.legacy()).await;
    let n = mock
        .names()
        .iter()
        .filter(|n| n.ends_with(".status"))
        .count();
    assert_eq!(n, LEGACY_TIMEOUT_LIMIT + OBJS.len());
}

/// P1-2（V2-13）：一轮拖得很长（旧采集挨个 8 秒超时，cli 后端）时，块过了 max_age 由独立定时器置 stale，
/// 不等轮末。
#[tokio::test(start_paused = true)]
async fn block_goes_stale_mid_round_when_the_round_drags() {
    let (exec, mock, sink) = setup(vec![spec("a", 0)], Config::default(), 1000);
    for o in OBJS {
        mock.default_step(
            &format!("{o}.status"),
            Step::Timeout(Duration::from_secs(8)),
        );
    }
    exec.start_heartbeat(Duration::from_secs(5));
    exec.start_rounds(Arc::new(TenLegacyObjects));
    // 第一轮约 1 秒开始：读 a，然后旧采集 6 × 8 = 48 秒。a 的 max_age 是 15 秒。
    tokio::time::sleep(Duration::from_secs(22)).await;
    let ev = sink.blocks("a");
    assert!(ev.last().is_some_and(|(_, b)| b.stale), "{ev:?}");
    assert!(
        mock.times("o5.status").is_empty(),
        "still in the first round"
    );
    // 轮末之后下一轮重读 a，翻回来
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert!(!sink.blocks("a").last().unwrap().1.stale);
}

/// 一把被别人（应急脚本、agent）拿着的 flock：`hold` 之后放开。
fn held_lock(name: &str, hold: Duration) -> (String, std::thread::JoinHandle<()>) {
    use fs2::FileExt;
    let dir = std::env::temp_dir().join(format!("datad-exec-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("lock");
    let f = std::fs::File::create(&path).unwrap();
    f.lock_exclusive().unwrap();
    let t = std::thread::spawn(move || {
        std::thread::sleep(hold);
        FileExt::unlock(&f).unwrap();
    });
    (path.to_string_lossy().into_owned(), t)
}

/// P1-1：AT 写（`netselect.auto`）在一个控制任务里先等跨进程写锁、再等 AT 锁、再等基带回答，
/// 都不是 ubus 调用。每段做完记一次前进：exec_age 不会累加成整个任务的时长，看门狗不动。
/// 真机上三段最长 15 + 10 + 6 秒；这里各 1.2 秒（真实时间），用 2 秒的上限检查。
#[tokio::test(flavor = "current_thread")]
async fn lock_and_at_waits_count_as_progress() {
    let (exec, mock, _) = setup(vec![], Config::default(), 1000);
    mock.default_step(
        "uci.get",
        Step::Reply(json!({"value":"ONLINE"}), Duration::ZERO),
    );
    let stage = Duration::from_millis(1200);
    let (write_lock, w) = held_lock("write", stage);
    let (at_lock, a) = held_lock("at", stage * 2);
    let (port, sent) = crate::at::tests::fake_modem_after("\r\nOK\r\n", stage);
    let e = exec.clone();
    let job = tokio::spawn(async move {
        e.control(async move {
            let _w = crate::ops::write_lock::acquire_path(Some(write_lock)).await;
            crate::at::send_with(crate::at::Cmd::CopsAuto, Some(at_lock), Some(port)).await
        })
        .await
    });
    let start = std::time::Instant::now();
    let mut max_age = 0;
    while !job.is_finished() {
        tokio::time::sleep(Duration::from_millis(50)).await;
        max_age = max_age.max(exec.exec_age_ms());
        assert!(
            !exec.stalled(Duration::from_secs(2)),
            "stalled after {:?} (exec_age {} ms)",
            start.elapsed(),
            exec.exec_age_ms()
        );
        assert!(start.elapsed() < Duration::from_secs(20), "job never ended");
    }
    let answer = job.await.unwrap().unwrap().unwrap();
    assert!(answer.contains("OK"), "{answer:?}");
    assert_eq!(sent.recv().unwrap(), "AT+COPS=0");
    // 一共约 3.6 秒，每段不到 1.5 秒
    assert!(start.elapsed() >= stage * 3, "{:?}", start.elapsed());
    assert!(max_age < 1_800, "exec_age reached {max_age} ms");
    w.join().unwrap();
    a.join().unwrap();
}

/// 真卡在一段等待里（锁一直不放、子进程不受自己的超时管）照样不前进：看门狗照样会动。
#[tokio::test(start_paused = true)]
async fn a_wait_that_never_ends_still_stalls() {
    let (exec, _mock, _) = setup(vec![], Config::default(), 1000);
    let e = exec.clone();
    tokio::spawn(async move {
        let _ = e
            .control(async {
                progress();
                std::future::pending::<()>().await;
            })
            .await;
    });
    tokio::time::sleep(Duration::from_secs(29)).await;
    assert!(!exec.stalled(Duration::from_secs(30)));
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(exec.stalled(Duration::from_secs(30)));
}

/// 写的上下文里调一次（像拿着写锁的事务写或 `/control` 写）。
async fn write_call(
    exec: &Executor,
    object: &'static str,
    method: &'static str,
) -> Result<Value, UbusError> {
    exec.task(async move {
        let _w = super::write_scope();
        call(object, method, &json!({})).await
    })
    .await
}

#[tokio::test(start_paused = true)]
async fn timeout_holds_next_call_until_object_answers() {
    // V2-33（D28）：写超时了，下一个调用之前先用只读请求探测这个对象，回答了才放行。
    let (exec, mock, _) = setup(vec![spec("dev", 3600)], Config::default(), 1000);
    mock.push("dev.set", Step::Timeout(Duration::from_secs(8)));
    mock.push("dev.list", Step::Timeout(Duration::from_secs(8)));
    mock.push(
        "dev.list",
        Step::Reply(json!({}), Duration::from_millis(50)),
    );
    let r = write_call(&exec, "dev", "set").await;
    assert!(matches!(r, Err(UbusError::Timeout { .. })));
    write_call(&exec, "dev", "set").await.unwrap();
    assert_eq!(mock.names(), ["dev.set", "dev.list", "dev.list", "dev.set"]);
    // 放闸以后不再探测
    write_call(&exec, "other", "get").await.unwrap();
    assert_eq!(mock.names().last().unwrap(), "other.get");
    assert_eq!(mock.names().len(), 5);
}

#[tokio::test(start_paused = true)]
async fn gate_opens_after_four_failed_probes() {
    let (exec, mock, _) = setup(vec![], Config::default(), 1000);
    mock.push("dev.set", Step::Timeout(Duration::from_secs(8)));
    mock.default_step("system.board", Step::Timeout(Duration::from_secs(8)));
    let t0 = Instant::now();
    let _ = write_call(&exec, "dev", "set").await;
    write_call(&exec, "dev", "get").await.unwrap();
    // 块表里没有这个对象：问 ubusd 本身；4 次都超时也放行
    assert_eq!(
        mock.names(),
        [
            "dev.set",
            "system.board",
            "system.board",
            "system.board",
            "system.board",
            "dev.get"
        ]
    );
    assert_eq!(Instant::now() - t0, Duration::from_secs(40));
}

#[tokio::test(start_paused = true)]
async fn round_timeouts_do_not_close_the_gate() {
    let (exec, mock, _) = setup(vec![spec("a", 0)], Config::default(), 1000);
    mock.push("a.list", Step::Timeout(Duration::from_secs(2)));
    exec.start_rounds(Arc::new(NoLegacy));
    tokio::time::sleep(Duration::from_millis(3_500)).await;
    write_call(&exec, "ctl", "set").await.unwrap();
    let names = mock.names();
    let i = names.iter().position(|n| n == "ctl.set").unwrap();
    // 采集轮里的读超时不关闸：控制调用前面没有探测
    assert_eq!(names[..i], ["a.list"]);
}

#[tokio::test(start_paused = true)]
async fn probe_uses_the_vendor_read_for_written_objects() {
    // 块表里没有 zte_nwinfo_api：用固定表里的 nwinfo_get_netinfo 探测，不是只问 ubusd
    let (exec, mock, _) = setup(vec![spec("a", 3600)], Config::default(), 1000);
    mock.push(
        "zte_nwinfo_api.nwinfo_set_netselect",
        Step::Timeout(Duration::from_secs(8)),
    );
    let _ = write_call(&exec, "zte_nwinfo_api", "nwinfo_set_netselect").await;
    write_call(&exec, "zte_nwinfo_api", "nwinfo_set_netselect")
        .await
        .unwrap();
    assert_eq!(
        mock.names(),
        [
            "zte_nwinfo_api.nwinfo_set_netselect",
            "zte_nwinfo_api.nwinfo_get_netinfo",
            "zte_nwinfo_api.nwinfo_set_netselect"
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn read_timeouts_do_not_close_the_gate() {
    // 事务确认时读 nwinfo 超时（基带在重新注册）：不关闸，下一个写不用陪着探测
    let (exec, mock, _) = setup(vec![], Config::default(), 1000);
    mock.push(
        "zte_nwinfo_api.nwinfo_get_netinfo",
        Step::Timeout(Duration::from_secs(8)),
    );
    let r = exec
        .call("zte_nwinfo_api", "nwinfo_get_netinfo", &json!({}))
        .await;
    assert!(matches!(r, Err(UbusError::Timeout { .. })));
    write_call(&exec, "zte_nwinfo_api", "nwinfo_set_netselect")
        .await
        .unwrap();
    assert_eq!(
        mock.names(),
        [
            "zte_nwinfo_api.nwinfo_get_netinfo",
            "zte_nwinfo_api.nwinfo_set_netselect"
        ]
    );
}

// ------------------------------------------------------------ 旧采集超时的跨轮冷却（上游 v0.10.56 回学）

/// 打开冷却（建议值）的配置。
fn cooling() -> Config {
    Config {
        cooldown: COOLDOWN,
        ..Config::default()
    }
}

/// 一轮旧采集：调一次 `x status`，返回是否成功。
async fn legacy_x(exec: &Executor) -> Result<Value, UbusError> {
    exec.round_now(async { call("x", "status", &json!({})).await })
        .await
}

#[tokio::test(start_paused = true)]
async fn legacy_timeout_cools_object_across_rounds_then_expires() {
    let (exec, mock, _) = setup(vec![], cooling(), 1000);
    mock.push("x.status", Step::Timeout(Duration::from_secs(2)));
    assert!(legacy_x(&exec).await.unwrap_err().is_timeout());
    let t0 = Instant::now();
    // 冷却期内：下几轮都不发请求，算跳过。
    for _ in 0..3 {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let e = legacy_x(&exec).await.unwrap_err();
        assert!(matches!(e, UbusError::Skipped { .. }), "{e:?}");
    }
    assert_eq!(mock.names(), ["x.status"]);
    // 控制任务不管冷却，照常调。
    exec.control(async { call("x", "set", &json!({})).await.unwrap() })
        .await
        .unwrap();
    assert_eq!(mock.names(), ["x.status", "x.set"]);
    // 冷却过了：照常读。
    tokio::time::sleep_until(t0 + COOLDOWN).await;
    assert!(legacy_x(&exec).await.is_ok());
    assert_eq!(mock.names(), ["x.status", "x.set", "x.status"]);
}

#[tokio::test(start_paused = true)]
async fn cooldown_ignores_blocks_failures_and_can_be_off() {
    // 块超时不进冷却（块有自己的 5 秒失败重读，电池/充电不能空 30 秒以上）；
    // 旧采集里的非超时失败也不进冷却。
    let (exec, mock, _) = setup(vec![spec("b", 0)], cooling(), 1000);
    mock.push("b.list", Step::Timeout(Duration::from_secs(2)));
    mock.push("x.status", Step::Fail(Duration::ZERO));
    let _ = legacy_x(&exec).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(legacy_x(&exec).await.is_ok());
    let names = mock.names();
    assert_eq!(
        names.iter().filter(|n| *n == "b.list").count(),
        2,
        "{names:?}"
    );
    assert_eq!(
        names.iter().filter(|n| *n == "x.status").count(),
        2,
        "{names:?}"
    );

    // 默认（cooldown = 0）：只按本轮跳过（原来的行为），下一轮就再读。
    assert_eq!(Config::default().cooldown, Duration::ZERO);
    let (exec, mock, _) = setup(vec![], Config::default(), 1000);
    mock.push("x.status", Step::Timeout(Duration::from_secs(2)));
    let _ = legacy_x(&exec).await;
    assert!(legacy_x(&exec).await.is_ok());
    assert_eq!(mock.names(), ["x.status", "x.status"]);
}

#[tokio::test(start_paused = true)]
async fn cooled_object_keeps_heartbeats_flowing() {
    // 被冷却的对象一直挂着也不拖慢轮次：心跳间隔保持在 agent 的 20 秒静默线以内。
    let (exec, mock, sink) = setup(vec![spec("b", 0)], cooling(), 1000);
    mock.default_step("x.status", Step::Timeout(Duration::from_secs(8)));
    struct HungLegacy;
    impl RoundDriver for HungLegacy {
        fn legacy(&self) -> BoxFuture<'static, ()> {
            Box::pin(async {
                preempt().await;
                let _ = call("x", "status", &json!({})).await;
            })
        }
    }
    exec.start_heartbeat(Duration::from_secs(5));
    exec.start_rounds(Arc::new(HungLegacy));
    tokio::time::sleep(Duration::from_secs(60)).await;
    let x = mock.times("x.status").len();
    // 每 ~10 秒冷却 + 8 秒超时：60 秒里最多 4 次（不冷却时是 60/9 ≈ 6～7 次）。
    assert!((2..=4).contains(&x), "x.status called {x} times");
    let hb = sink.heartbeats();
    let gaps: Vec<_> = hb.windows(2).map(|w| w[1].0 - w[0].0).collect();
    assert!(
        gaps.iter().all(|g| *g <= Duration::from_secs(10)),
        "{gaps:?}"
    );
}

#[test]
fn cooldown_env_parsing() {
    assert_eq!(Config::cooldown_from(None), Duration::ZERO);
    assert_eq!(Config::cooldown_from(Some("x")), Duration::ZERO);
    assert_eq!(Config::cooldown_from(Some("0")), Duration::ZERO);
    assert_eq!(
        Config::cooldown_from(Some(" 30000 ")),
        Duration::from_secs(30)
    );
}

// USB gate fixtures are synthetic. Recorded listing bytes alone are not device support.
fn usb_listing(mock: &Mock, object: &str, text: &str) {
    lock(&mock.0)
        .usb_listings
        .insert(object.into(), Ok(text.as_bytes().to_vec()));
}
fn usb_mode_device(mock: &Mock, mode: &str) {
    lock(&mock.0)
        .device
        .insert("zwrt_bsp.usb".into(), json!({"mode":mode}));
}
const USB_PRESENT: &str = "'zwrt_bsp.usb' @1\n\t\"list\":{}\n\t\"set\":{\"mode\":\"String\"}\n";
async fn human_usb(exec: &Executor, object: &'static str, args: Value) -> Result<Value, UbusError> {
    exec.task(usb_human_scope(true, async move {
        call(object, "set", &args).await
    }))
    .await
}

#[tokio::test(start_paused = true)]
async fn usb_gate_unknown_internal_and_automatic_callers_never_probe_or_write() {
    let (exec, mock, _) = setup(vec![], Config::default(), 1000);
    for object in ["zwrt_bsp.usb", "zwrt_bsp.typec", "zwrt_bsp.powerbank"] {
        let args = if object.ends_with("typec") {
            json!({"DR_Swap":"device"})
        } else {
            json!({"state":1})
        };
        assert!(matches!(exec.call(object,"set",&args).await,
            Err(UbusError::CapabilityDenied(e)) if e.code == "capability_unverified"));
    }
    assert!(mock.names().is_empty());
    assert!(lock(&mock.0).usb_listing_log.is_empty());
}

#[tokio::test(start_paused = true)]
async fn usb_gate_recorded_missing_setter_does_not_disable_powerbank() {
    let (exec, mock, _) = setup(vec![], Config::default(), 1000);
    let fixture = "'zwrt_bsp.powerbank' @00000001\n\t\"set\":{\"state\":\"Integer\"}\n\t\"get\":{\"property\":\"String\"}\n'zwrt_bsp.typec' @00000002\n\t\"list\":{}\n\t\"set\":{\"PR_Swap\":\"String\",\"DR_Swap\":\"String\"}\n'zwrt_bsp.usb' @00000003\n\t\"list\":{}\n";
    for object in ["zwrt_bsp.usb", "zwrt_bsp.powerbank"] {
        usb_listing(&mock, object, fixture);
    }
    assert!(
        matches!(human_usb(&exec,"zwrt_bsp.usb",json!({"mode":"debug"})).await,
        Err(UbusError::CapabilityDenied(e)) if e.status() == 501 && e.preflight.as_ref().unwrap().proof == Some(luyaz_api::firmware::Proof::MethodAbsent))
    );
    assert!(
        mock.names().is_empty(),
        "absence refuses before any state read or write"
    );
    lock(&mock.0)
        .device
        .insert("zwrt_bsp.powerbank".into(), json!({"state":0}));
    lock(&mock.0).device.insert(
        "zwrt_bsp.typec".into(),
        json!({"cc_attch_state":1,"data_role":"host"}),
    );
    let result = exec
        .task(usb_human_scope(true, async {
            crate::control::execute(
                "vendor.call",
                &json!({
                    "object":"zwrt_bsp.powerbank", "method":"set", "args":{"state":1}
                }),
            )
            .await
        }))
        .await;
    assert!(
        matches!(result, crate::control::Outcome::Ok(_)),
        "{result:?}"
    );
    assert_eq!(
        mock.names()
            .iter()
            .filter(|s| s.as_str() == "zwrt_bsp.powerbank.set")
            .count(),
        1
    );
    assert!(!mock.names().contains(&"zwrt_bsp.usb.set".into()));
}

#[tokio::test(start_paused = true)]
async fn usb_gate_manual_readback_records_effect_but_missing_epochs_stay_unknown() {
    use luyaz_api::firmware::{Proof, Support};
    let (exec, mock, _) = setup(vec![], Config::default(), 1000);
    usb_listing(&mock, "zwrt_bsp.usb", USB_PRESENT);
    usb_mode_device(&mock, "user");
    assert!(
        human_usb(&exec, "zwrt_bsp.usb", json!({"mode":"debug"}))
            .await
            .is_ok()
    );
    assert_eq!(
        mock.names(),
        ["zwrt_bsp.usb.list", "zwrt_bsp.usb.set", "zwrt_bsp.usb.list"]
    );
    let report = exec
        .hub()
        .firmware()
        .report(luyaz_api::clock::BootTime::read().as_ref());
    report.validate().unwrap();
    assert!(
        report
            .evidence
            .iter()
            .any(|e| e.capability_id == "usb.mode.set"
                && e.result.as_ref().unwrap().proof == Some(Proof::WriteVerified))
    );
    assert_eq!(
        report
            .usb_write_policy("usb.mode.set")
            .unwrap()
            .assessment
            .status,
        Support::Unknown
    );
    assert!(matches!(
        exec.call("zwrt_bsp.usb", "set", &json!({"mode":"mtp"}))
            .await,
        Err(UbusError::CapabilityDenied(_))
    ));
    assert_eq!(
        mock.names().len(),
        3,
        "a prior human operation cannot authorize automation"
    );
}

#[tokio::test(start_paused = true)]
async fn usb_gate_ack_and_noop_do_not_verify_an_effect() {
    for already_set in [false, true] {
        let (exec, mock, _) = setup(vec![], Config::default(), 1000);
        usb_listing(&mock, "zwrt_bsp.usb", USB_PRESENT);
        usb_mode_device(&mock, if already_set { "debug" } else { "user" });
        mock.default_step("zwrt_bsp.usb.set", Step::Reply(json!({}), Duration::ZERO));
        assert!(
            human_usb(&exec, "zwrt_bsp.usb", json!({"mode":"debug"}))
                .await
                .is_ok()
        );
        let r = exec.hub().firmware().report(None);
        assert!(
            r.evidence
                .iter()
                .filter(|e| e.capability_id == "usb.mode.set")
                .all(|e| e.result.as_ref().unwrap().proof.is_none())
        );
    }
}

#[tokio::test(start_paused = true)]
async fn usb_gate_malformed_listing_disconnected_and_budget_expiry_never_write() {
    for case in ["malformed", "unplugged", "timeout"] {
        let (exec, mock, _) = setup(vec![], Config::default(), 1000);
        let object = if case == "unplugged" {
            "zwrt_bsp.typec"
        } else {
            "zwrt_bsp.usb"
        };
        usb_listing(
            &mock,
            object,
            if case == "malformed" {
                "'zwrt_bsp.usb' @1\n"
            } else if case == "unplugged" {
                "'zwrt_bsp.typec' @1\n\t\"set\":{\"DR_Swap\":\"String\"}\n\t\"list\":{}\n"
            } else {
                USB_PRESENT
            },
        );
        if case == "timeout" {
            lock(&mock.0).usb_listing_delay = Duration::from_secs(20);
        }
        let args = if case == "unplugged" {
            json!({"DR_Swap":"device"})
        } else {
            json!({"mode":"debug"})
        };
        assert!(matches!(
            human_usb(&exec, object, args).await,
            Err(UbusError::CapabilityDenied(_))
        ));
        assert!(mock.names().iter().all(|s| !s.ends_with(".set")), "{case}");
    }
}

#[tokio::test(start_paused = true)]
async fn usb_gate_rechecks_generation_after_waiting_for_preflight() {
    let (exec, mock, _) = setup(vec![], Config::default(), 1000);
    usb_listing(&mock, "zwrt_bsp.usb", USB_PRESENT);
    usb_mode_device(&mock, "user");
    lock(&mock.0).usb_listing_delay = Duration::from_millis(100);
    let hub = exec.hub().clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        hub.firmware().ubus(
            hub.firmware().token(),
            luyaz_api::clock::BootTime::read(),
            "zwrt_zte_mdm.api",
            "get_zwrt_common_info",
            Some(&json!({"model_name":"MU5250"})),
        );
    });
    assert!(
        matches!(human_usb(&exec,"zwrt_bsp.usb",json!({"mode":"debug"})).await,
        Err(UbusError::CapabilityDenied(e)) if e.code=="capability_context_changed")
    );
    assert!(!mock.names().contains(&"zwrt_bsp.usb.set".into()));
}

#[tokio::test(start_paused = true)]
async fn usb_gate_rejection_exits_attach_workflow_before_success_polling() {
    let (exec, mock, _) = setup(vec![], Config::default(), 1000);
    lock(&mock.0).device.insert(
        "zwrt_bsp.typec".into(),
        json!({"cc_attch_state":1,"data_role":"host"}),
    );
    let out = exec
        .task(async {
            crate::control::execute(
                "usb.attach_mode",
                &json!({"mode":"share","remembered":true}),
            )
            .await
        })
        .await;
    assert!(
        matches!(out, crate::control::Outcome::CapabilityDenied(_)),
        "{out:?}"
    );
    assert_eq!(
        mock.names(),
        ["zwrt_bsp.typec.list", "zwrt_bsp.usb.list"],
        "only the original remembered-choice preconditions; no success polling"
    );
    assert!(lock(&mock.0).usb_listing_log.is_empty());
}

#[tokio::test]
async fn at_boundary_requires_a_direct_online_read_before_any_port_access() {
    for mode in ["LOW_POWER", "OFFLINE", "", "ONLINE"] {
        let (exec, mock, _) = setup(vec![], Config::default(), 1000);
        mock.default_step(
            "uci.get",
            Step::Reply(json!({"value":mode}), Duration::ZERO),
        );
        let result = exec
            .control(async {
                crate::at::send_with(
                    crate::at::Cmd::Cgcontrdp,
                    Some("/does-not-exist/at-lock".into()),
                    Some("/does-not-exist/at-port".into()),
                )
                .await
            })
            .await
            .unwrap();
        let e = result.unwrap_err();
        assert_eq!(mock.names(), vec!["uci.get"]);
        if mode == "ONLINE" {
            assert!(e.contains("lock"), "{e}");
        } else {
            assert!(e.contains("radio_not_online"), "{e}");
        }
    }
}

#[tokio::test]
async fn configured_port_lookup_is_not_a_transport_read_proof() {
    let (exec, mock, _) = setup(vec![], Config::default(), 1000);
    mock.default_step(
        "uci.get",
        Step::Reply(json!({"value":"ONLINE"}), Duration::ZERO),
    );
    exec.control(async {
        crate::at::probe(crate::at::Target {
            lock: None,
            port: Some("/does-not-exist/port-only".into()),
        })
        .await
    })
    .await
    .unwrap()
    .unwrap();
    let report = exec
        .hub()
        .firmware()
        .report(luyaz_api::clock::BootTime::read().as_ref());
    assert!(
        report
            .evidence
            .iter()
            .all(|e| e.capability_id != "at.transport.read")
    );
}

/// Review fix: a lock wait past the 5 s admission window and unrelated USB/ubus
/// invalidations do not fail an admitted AT command or discard its answer; only an
/// observed radio change does. The stale answer is returned but is not evidence.
#[tokio::test(flavor = "current_thread")]
async fn admitted_at_survives_lock_waits_and_unrelated_invalidations() {
    use crate::device_profile::context::Loss;
    let (exec, mock, _) = setup(vec![], Config::default(), 1000);
    mock.default_step(
        "uci.get",
        Step::Reply(json!({"value":"ONLINE"}), Duration::ZERO),
    );
    let (at_lock, _a) = held_lock("at-wait", Duration::from_millis(6500));
    let (port, sent) = crate::at::tests::fake_modem_after("\r\nOK\r\n", Duration::ZERO);
    let e = exec.clone();
    let job = tokio::spawn(async move {
        e.control(async move {
            crate::at::send_with(crate::at::Cmd::CopsAuto, Some(at_lock), Some(port)).await
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    exec.hub().firmware().invalidate_context(Loss::Usb);
    exec.hub().firmware().invalidate_context(Loss::Ubus);
    let answer = job.await.unwrap().unwrap().unwrap();
    assert!(answer.contains("OK"), "{answer:?}");
    assert_eq!(sent.recv().unwrap(), "AT+COPS=0");

    // An observed departure from ONLINE while waiting stops it before the port.
    let (at_lock, _a) = held_lock("at-radio", Duration::from_millis(800));
    let (port, sent) = crate::at::tests::fake_modem_after("\r\nOK\r\n", Duration::ZERO);
    let e = exec.clone();
    let job = tokio::spawn(async move {
        e.control(async move {
            crate::at::send_with(crate::at::Cmd::CopsAuto, Some(at_lock), Some(port)).await
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let token = exec.hub().firmware().token();
    exec.hub()
        .firmware()
        .radio_read(token, Some("LOW_POWER"), luyaz_api::clock::BootTime::read());
    let err = job.await.unwrap().unwrap().unwrap_err();
    assert!(err.contains("radio_not_online"), "{err}");
    assert!(sent.try_recv().is_err(), "nothing reached the port");
}
