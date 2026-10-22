use crate::{
    auth::{self, Sessions},
    block::{self, Hub},
    executor::{self, Executor, RoundDriver},
    model::{CoreVersion, Snapshot},
    state, v2,
};
use anyhow::Result;
use axum::{
    Json, Router,
    extract::rejection::JsonRejection,
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Map, Value, json};
use std::{future::IntoFuture, path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    net::TcpListener,
    sync::{Mutex, RwLock, Semaphore, watch},
};
use tower_http::limit::RequestBodyLimitLayer;

mod write;
#[cfg(test)]
pub(crate) use write::control_busy;

#[derive(Clone)]
pub struct App {
    inner: Arc<Inner>,
}
/// Shared server state behind every handler.
struct Inner {
    /// 单一采集执行者：采样循环、全部 ubus 调用、/control 排队（STATE_V2.md 第 7、8 节）。
    exec: Executor,
    /// Last collected legacy snapshot, plus the channel that announces a changed one.
    snapshot: RwLock<Snapshot>,
    tx: watch::Sender<Snapshot>,
    /// LAN credentials: the configured fixed token (if any) and the logins handed out by `/auth/*`.
    token: Option<String>,
    sessions: Mutex<Sessions>,
    /// `/v2/events` 的 SSE 连接名额。
    sse_slots: Arc<Semaphore>,
    _data_dir: PathBuf,
    /// `/v2` 的流（epoch + broadcast），事件由 `Hub` 在锁里发进来。
    feed: Arc<v2::Feed>,
    /// E4 写操作层：事务引擎（`ops/`）。
    ops: crate::ops::Engine<crate::ops::UbusDevice>,
    /// 启动进度（`App::start`）：监听先起来，第一轮采集和事务恢复在后台做。
    stage: watch::Sender<Stage>,
    /// 采样间隔（`App::start` 用）。
    interval: Duration,
    /// 收到 SIGTERM/SIGINT 后置 true（P2-4）：SSE 流看到就结束，服务在 `SHUTDOWN_GRACE` 内退出。
    stop: watch::Sender<bool>,
}

/// 收到退出信号后最多等连接收尾这么久（P2-4）。procd 的 `term_timeout` 约 5 秒，到了就 SIGKILL；
/// 这里留出余量。读不动的客户端（发送缓冲满）收不到流的结尾，graceful shutdown 会一直等它，所以要有上限。
const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

/// How many `/v2/events` subscribers may be connected at once.
const MAX_SSE_CLIENTS: usize = 16;

/// 启动进度（P1-3）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    /// 第一轮还没采完：读数据的接口先等（`starting_gate`），`/healthz` 回 503。
    Starting,
    /// 有第一份快照了：读接口照常；`/control` 还在等事务恢复（`ops.start`）。
    Snapshot,
    /// 全部就绪。
    Ready,
}

/// 启动时 `/control` 最多等这么久（等第一轮和事务恢复做完），还没好就回 503 `busy`（客户端照队列满重试）。
const CONTROL_START_WAIT: Duration = Duration::from_secs(20);
/// 启动时读数据的请求最多等这么久（等第一轮采完），还没好就回 503 `starting`。
/// 先等而不是马上回 503：以前端口要到第一轮采完才监听，有的客户端把「连得上」当成「有快照」。
const READ_START_WAIT: Duration = Duration::from_secs(10);
/// `/healthz` 判卡住的线：执行者这么久没前进就回 503（和订阅方判卡死的 20 秒一样，V2-32）。
const HEALTH_STALL_MS: u64 = 20_000;

/// V2-22：距上一条心跳满这么久，独立定时器就补一条。
const HEARTBEAT_EVERY: Duration = Duration::from_secs(5);

/// D18：应急直写脚本据此判断核心在不在（pid + /proc/<pid>/comm 前缀）。
/// `LUYAZ_PID_FILE`，默认 `/var/run/luyaz-core.pid`（Luya-Z 改名 D2；之前 `/var/run/zwrt-datad.pid`，
/// `luyaz-fallback.sh` 两个都看、认活着的那个），空串 = 不写。
pub fn write_pid_file() {
    let path = match std::env::var("LUYAZ_PID_FILE") {
        Ok(v) if v.is_empty() => return,
        Ok(v) => PathBuf::from(v),
        Err(_) => PathBuf::from("/var/run/luyaz-core.pid"),
    };
    let tmp = path.with_extension("pid.tmp");
    let r = std::fs::write(&tmp, format!("{}\n", std::process::id()))
        .and_then(|()| std::fs::rename(&tmp, &path));
    if let Err(e) = r {
        eprintln!("cannot write pid file {}: {e}", path.display());
    }
}

impl App {
    pub async fn new(data_dir: PathBuf, interval: Duration, token: Option<String>) -> Result<Self> {
        let feed = v2::Feed::new(v2::CAPACITY);
        let sink: Box<dyn block::EventSink> = Box::new(v2::FeedSink(feed.clone()));
        // T8: the event log's long-term layer (hour summaries, important events) on /data;
        // LUYAZ_LONGTERM=1 turns it on.
        let sink: Box<dyn block::EventSink> = if crate::longterm::enabled() {
            let dir = PathBuf::from(crate::state::host_path(crate::longterm::DIR));
            Box::new(crate::longterm::Tee {
                inner: sink,
                tx: std::sync::Mutex::new(crate::longterm::start(dir)),
            })
        } else {
            sink
        };
        let hub = Arc::new(Hub::new(block::phase1_blocks(), sink));
        // Phase 4（影子）：策略跟着块的读取节拍算，只记不做；LUYAZ_POLICIES=0 关掉。
        if crate::plugin::enabled() {
            crate::plugin::install(&hub);
        }
        let cfg = executor::Config {
            cache: state::cache_enabled(),
            cooldown: executor::Config::cooldown_from(
                std::env::var(executor::ENV_COOLDOWN_MS).ok().as_deref(),
            ),
            ..executor::Config::default()
        };
        let exec = Executor::spawn(
            crate::ubus::backend::Backend::from_env(),
            hub.clone(),
            cfg,
            interval,
        );
        executor::install(&exec);
        // V2-22：独立定时器补心跳（带 exec_age_ms）；V2-32：看门狗。第一轮之前就起：第一轮里每次调用都记前进，
        // 执行者闲着时不算卡（P1-3：以前第一轮在看门狗起来之前跑，ubusd 不回时没人管）。
        exec.start_heartbeat(HEARTBEAT_EVERY);
        if let Some(limit) = crate::watchdog::limit_from_env() {
            crate::watchdog::spawn(exec.clone(), limit);
        }
        // 第一轮之前的占位快照：只在 `Stage::Starting` 时存在，读接口这时先等第一轮（`starting_gate`），不会发出去。
        let placeholder = Snapshot {
            ts: 0,
            core: Default::default(),
            fields: Map::new(),
        };
        let changed = watch::channel(placeholder.clone()).0;
        crate::ops::engine::on_agent_end(Box::new(|t| {
            crate::ops::agent::event(&crate::ops::agent::txn_text(t))
        }));
        let ops = crate::ops::Engine::new(
            crate::ops::UbusDevice::new(exec.clone()),
            crate::ops::Config::from_env(),
            crate::ops::pending::Store::open(crate::ops::ops_dir()),
            crate::ops::record::Record::open(crate::ops::ops_dir()),
        );
        ops.register_user_writes();
        let (stage, _) = watch::channel(Stage::Starting);
        Ok(Self {
            inner: Arc::new(Inner {
                exec,
                snapshot: RwLock::new(placeholder),
                tx: changed,
                token,
                sessions: Mutex::new(Sessions::default()),
                sse_slots: Arc::new(Semaphore::new(MAX_SSE_CLIENTS)),
                _data_dir: data_dir,
                feed,
                ops,
                stage,
                interval,
                stop: watch::channel(false).0,
            }),
        })
    }

    /// 第一轮采集、周期采集、事务恢复、短信监听（P1-3：在监听起来之后做，`/healthz` 这期间回 503 `starting`）。
    /// 只调一次。
    pub async fn start(&self) {
        let app = self;
        // 第一轮在执行者里立即采（块 + 旧采集），之后执行者自己「睡一个采样间隔 → 一轮」。
        let interval_ms = app.inner.interval.as_millis() as u64;
        let hub = app.inner.exec.hub().clone();
        let initial = app
            .inner
            .exec
            .round_now(async move { state::collect(interval_ms, &hub).await })
            .await;
        *app.inner.snapshot.write().await = initial.clone();
        app.inner.tx.send_replace(initial);
        app.inner.stage.send_replace(Stage::Snapshot);
        app.inner.exec.start_rounds(Arc::new(app.clone()));
        // T10 (u60-sentinel.md D11): the crash watcher, woken by kmsg/rtnetlink; LUYAZ_CRASHWATCH=1
        if crate::crashwatch::io::enabled() {
            crate::crashwatch::io::spawn(app.inner.exec.hub().clone());
        }
        // 6e (u60-radio.md §13): pause CHILL while nobody can use it; LUYAZ_CHILL_IDLE=1
        if crate::engines::chill_idle::enabled() {
            crate::engines::chill_idle::spawn(app.inner.exec.hub().clone());
        }
        // CHILL end to end: a generate_204 through mihomo, budgeted, only with clients; LUYAZ_CHILL_PROBE=0 off
        if std::env::var("LUYAZ_CHILL_PROBE").map_or(true, |v| v != "0") {
            crate::engines::chill_probe::spawn(app.inner.exec.hub().clone());
        }
        // component versions: the daily look at the public repo's versions.json; LUYAZ_VERSION_CHECK=0
        if crate::versions::enabled() {
            crate::versions::spawn(app.clone());
        }
        // T9c: the boot line's facts and the key.log backfill, a minute into the boot
        if crate::longterm::enabled() {
            tokio::spawn(crate::longterm::boot_task());
        }
        // 排队的旧请求锁空出来后照原来的 /control 处理执行（返回 false = 执行者队列满，或跨进程写锁
        // 没拿到回了 503（D7：什么都没写）：放回队头稍后再来，过期照旧丢掉并记账）。
        let runner_app = app.clone();
        app.inner
            .ops
            .set_legacy_runner(Arc::new(move |action, params| {
                let app = runner_app.clone();
                Box::pin(async move {
                    let body = json!({"action": action, "params": params});
                    write::run_control(app, action, body)
                        .await
                        .is_ok_and(|r| r.status() != StatusCode::SERVICE_UNAVAILABLE)
                })
            }));
        // 有落盘的事务就接着确认（没有就不读设备）。要等跨进程写锁（D7：拿到才动，可能一直等），
        // 另起任务：不挡后面的事件监听；状态变了照样经观察者交 op 块。
        let starting = app.clone();
        tokio::spawn(async move { starting.inner.ops.start().await });
        // V2-34：引擎每次状态变化都交 op 块（在引擎的锁里），接上时先交一次。
        let hub_exec = app.inner.exec.clone();
        app.inner.ops.set_observer(Arc::new(move |v| {
            hub_exec
                .hub()
                .record("op", Ok(v), tokio::time::Instant::now());
        }));
        // V2-31、V2-48：监听原厂事件（只订阅，不发请求）。短信事件：短信读取立即重读；
        // Type-C 插拔、快充充电宝：typec、powerbank、charger 块立即重读。都让执行者立即开一轮。
        use crate::ubus::listen::{self, Handler};
        let sms_exec = app.inner.exec.clone();
        let usb_exec = app.inner.exec.clone();
        let usb: Arc<dyn Fn() + Send + Sync> =
            Arc::new(move || usb_exec.kick(&["typec", "powerbank", "charger"]));
        let context_hub = app.inner.exec.hub().clone();
        // B31：上次核心在重拨中途没了（标记还在），先把数据会话开回来；见 ops/reconnect.rs。
        tokio::task::spawn_blocking(crate::ops::reconnect::recover);
        // R18-P1-01：数据网卡（rmnet）的内核事件产生连接代次；见 device_profile/netlink.rs。
        crate::device_profile::netlink::spawn(context_hub.clone());
        listen::spawn_if_enabled(
            std::env::var(listen::ENV_ENABLE).ok().as_deref(),
            listen::Options::from_env(),
            vec![
                Handler {
                    event: listen::EVENT,
                    min_interval: listen::KICK_MIN_INTERVAL,
                    fire: Arc::new(move || {
                        state::invalidate_sms_cache();
                        sms_exec.kick(&["sms"]);
                    }),
                },
                Handler {
                    event: listen::TYPEC_EVENT,
                    min_interval: listen::USB_KICK_MIN_INTERVAL,
                    fire: usb.clone(),
                },
                Handler {
                    event: listen::POWERBANK_EVENT,
                    min_interval: listen::USB_KICK_MIN_INTERVAL,
                    fire: usb,
                },
                // R18-P1-01：卡事件、休眠唤醒只为 SIM／无线电代次订阅（observe 里处理），不额外踢执行者。
                Handler {
                    event: listen::CARD_EVENT,
                    min_interval: listen::USB_KICK_MIN_INTERVAL,
                    fire: Arc::new(|| {}),
                },
                Handler {
                    event: listen::POWER_EVENT,
                    min_interval: listen::USB_KICK_MIN_INTERVAL,
                    fire: Arc::new(|| {}),
                },
            ],
            Arc::new(move |notice| {
                use crate::device_profile::context::Loss;
                let loss = match notice {
                    listen::Notice::Event(listen::TYPEC_EVENT | listen::POWERBANK_EVENT) => {
                        Loss::Usb
                    }
                    listen::Notice::Event(listen::CARD_EVENT) => {
                        return context_hub.firmware().sim_event();
                    }
                    listen::Notice::Event(listen::POWER_EVENT) => {
                        return context_hub.firmware().radio_event();
                    }
                    listen::Notice::Ready => return context_hub.firmware().sim_listening(),
                    listen::Notice::Gap => Loss::Ubus,
                    _ => return,
                };
                context_hub.firmware().invalidate_context(loss);
            }),
        );
        app.inner.stage.send_replace(Stage::Ready);
    }

    fn stage(&self) -> Stage {
        *self.inner.stage.borrow()
    }

    /// 等到至少 `stage`，最多 `limit`；到了回 true。
    async fn wait_stage(&self, stage: Stage, limit: Duration) -> bool {
        let mut rx = self.inner.stage.subscribe();
        tokio::time::timeout(limit, rx.wait_for(|s| *s >= stage))
            .await
            .is_ok_and(|r| r.is_ok())
    }

    /// The blocks (u60d's touch compatibility layer reads the signal block's roaming fields).
    /// The decision loop's attempts the last core left open (§4.10): follow them to their end.
    pub fn resume_loop(&self) {
        crate::api::approve::resume(self.inner.ops.clone(), self.hub());
        crate::verified::install(self.inner.ops.clone(), self.hub());
    }

    pub fn hub(&self) -> Arc<crate::block::Hub> {
        self.inner.exec.hub().clone()
    }
    pub async fn snapshot(&self) -> Snapshot {
        self.inner.snapshot.read().await.clone()
    }
    /// `/v2/screen`'s reply (the touch layer's netwatch reads it in-process, step 9b).
    pub async fn screen_json(&self) -> Value {
        let snap = self.snapshot().await;
        let ts = snap.ts;
        let state = serde_json::to_value(&snap).unwrap_or(Value::Null);
        // E4 T13（V2-34、V2-38）：叠在首页的和 op 在同一把锁里算。
        let (screen_op, op) = self.inner.ops.screen();
        let mut net = crate::project::screen::net_view_op(
            &state,
            crate::cell_window::current(),
            screen_op.as_ref(),
        );
        // T6: the open "sure" finding over the network verdict (not over `story`), in
        // `network.health`'s words (the same the web shows)
        let health = crate::api::underlay::health_input("");
        if let (Some(f), Some(say)) = (&health.finding, luyaz_api::execution::say::health(&health)) {
            crate::project::screen::with_finding(&mut net, &f.layer, (&say.detail, &say.detail_en));
        }
        screen_json(ts, net, op, self.inner.exec.exec_age_ms())
    }
    /// 一轮里的旧采集（在执行者里跑，块已经读完）。别在持有 snapshot 锁时调 ubus：
    /// 控制任务只在 `ubus_ttl` 这类安全点插进来，这里的锁段里没有安全点。
    async fn refresh_snapshot(&self) {
        let next = state::collect(self.inner.exec.interval_ms(), self.inner.exec.hub()).await;
        // V2-34：op 块每轮交一次（进行中时倒计时跟着走）。
        self.inner.ops.publish_now();
        // 经验库的 advice 块：长期层线程算好，这里每轮交一次（派生块不交就过期）。
        self.inner.exec.hub().record(
            "advice",
            Ok(crate::longterm::places::advice_now()),
            tokio::time::Instant::now(),
        );
        let mut held = self.inner.snapshot.write().await;
        // Only a change in content is announced; a new timestamp alone just moves `ts` forward.
        let content_moved = held.core != next.core || held.fields != next.fields;
        if content_moved {
            let _ = self.inner.tx.send(next.clone());
            *held = next;
        } else {
            held.ts = next.ts;
        }
    }
}

impl RoundDriver for App {
    fn legacy(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>> {
        let app = self.clone();
        Box::pin(async move { app.refresh_snapshot().await })
    }
}

impl App {
    /// 收到退出信号时完成（`stop` 置 true；`App` 已经没了也算）。SSE 流用它结束自己。
    fn stopped(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let mut rx = self.inner.stop.subscribe();
        async move {
            let _ = rx.wait_for(|stop| *stop).await;
        }
    }

    /// 在 `listener` 上提供服务（P1-3：监听在 `App::start` 之前就起来，启动中的请求见 `starting_gate`）。
    pub async fn serve(
        self,
        listener: TcpListener,
        require_auth: bool,
        open_auth_routes: bool,
        ui: Option<Router>,
    ) -> Result<()> {
        let router = self.router(require_auth, open_auth_routes, ui);
        let server = axum::serve(
            crate::conn::DatadListener::new(listener),
            router.into_make_service_with_connect_info::<crate::conn::Peer>(),
        )
        .with_graceful_shutdown(shutdown(self.clone()))
        .into_future();
        // 从收到信号算起最多 `SHUTDOWN_GRACE`：还有连接没收尾（对端不读）也不再等。
        let deadline = {
            let stopped = self.stopped();
            async move {
                stopped.await;
                tokio::time::sleep(SHUTDOWN_GRACE).await;
            }
        };
        let result = tokio::select! {
            result = server => result,
            () = deadline => {
                eprintln!("shutdown: connections still open after {SHUTDOWN_GRACE:?}, exiting anyway");
                Ok(())
            }
        };
        result?;
        Ok(())
    }

    fn router(&self, require_auth: bool, open_auth_routes: bool, ui: Option<Router>) -> Router {
        let mut router = Router::new()
            .route("/", get(index))
            .route("/healthz", get(health))
            .route("/version", get(version))
            .route("/state", get(gone))
            .route("/events", get(gone))
            .route("/v2/events", get(v2_events))
            .route("/v2/state", get(v2_state))
            .route("/v2/screen", get(v2_screen))
            .route("/capabilities", get(capabilities))
            .route("/v2/device/capabilities", get(device_capabilities))
            .route("/control", post(control))
            .route("/v2/sentinel/event", post(sentinel_event))
            .route("/v2/scorecard", get(scorecard).post(scorecard))
            .route("/v2/longterm/calib", post(longterm_calib))
            .route("/v2/versions", get(versions))
            .route("/v2/dual/switches", get(dual_switches))
            .route("/v2/usage/days", get(usage_days))
            .route("/v2/usage/landing", get(usage_landing))
            .route("/v2/versions/check", post(versions_check));
        // M6b: the screen's agent-shaped API (`/v2/api/...`, compat's `/api/...` routes), loopback
        // listener only. Nested at `/v2`: the routes above are static and win over the nest's tail.
        if let Some(ui) = ui {
            router = router.nest_service("/v2", ui);
        }
        if open_auth_routes {
            router = router
                .route("/auth/login", post(auth_login))
                .route("/auth/exchange", post(auth_exchange));
        }
        router = router.layer(middleware::from_fn_with_state(self.clone(), starting_gate));
        if require_auth {
            router = router.layer(middleware::from_fn_with_state(self.clone(), authenticate));
        }
        router
            .layer(RequestBodyLimitLayer::new(1024 * 1024))
            .with_state(self.clone())
    }
}

/// P1-3：第一轮采完之前（`Stage::Starting`），读数据的接口先等第一轮（最多 10 秒），还没好就回 503 `starting`
/// （客户端按连不上处理、过一会儿重试）；`/`、`/healthz`、`/version`、`/capabilities`、登录照常。
/// `/control` 在处理函数里等就绪（`wait_stage(Ready)`）。
async fn starting_gate(State(app): State<App>, request: Request, next: Next) -> Response {
    let open = matches!(
        request.uri().path(),
        "/" | "/healthz"
            | "/version"
            | "/capabilities"
            | "/v2/device/capabilities"
            | "/control"
            | "/v2/scorecard"
            | "/v2/dual/switches"
            | "/v2/usage/days"
            | "/v2/usage/landing"
            | "/state"
            | "/events"
            | "/auth/login"
            | "/auth/exchange"
    );
    // the screen's API reads its own files and blocks; it must not wait for the first round
    let open = open || request.uri().path().starts_with("/v2/api/");
    if open || app.wait_stage(Stage::Snapshot, READ_START_WAIT).await {
        return next.run(request).await;
    }
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"ok":false,"error":{"code":"starting","message":"the core is starting (first collection round not finished)"}})),
    )
        .into_response()
}

/// `GET|POST /v2/scorecard?w=24h|7d[&summary=1]` (u60-sentinel.md §19 T9b): the device
/// scorecard over the guard ledger, as `doctor.sh --report` prints it, then `rc=<n>`. A POST body
/// is doctor's health check (scorecard::live::health); without it those rows read 没测. Reads
/// files only, so it does not wait for the first round. Worked out in a child (`u60d
/// --scorecard-live`, about 0.1 s and 4 MB on the device): the device build aborts on a panic, and
/// a ledger line the port trips on must not take the screen's data down with it; its memory goes
/// with it too. One at a time.
async fn scorecard(method: Method, uri: axum::http::Uri, body: axum::body::Bytes) -> Response {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    static ONE: Mutex<()> = Mutex::const_new(());
    let q = uri.query().unwrap_or("");
    let arg = |k: &str| {
        q.split('&')
            .find_map(|p| p.strip_prefix(k).and_then(|v| v.strip_prefix('=')))
    };
    let w = match arg("w") {
        Some(w @ ("24h" | "7d")) => w,
        _ => {
            return (StatusCode::BAD_REQUEST, "w=24h|7d\n").into_response();
        }
    };
    let failed = |why: &str| {
        eprintln!("scorecard: {why}");
        (StatusCode::INTERNAL_SERVER_ERROR, format!("{why}\n")).into_response()
    };
    let Ok(exe) = std::env::current_exe() else {
        return failed("no path to itself");
    };
    let mut cmd = tokio::process::Command::new(exe);
    // argv[0] "luyaz-core*" would start a second server (serve::invoked_as_core)
    cmd.arg0("u60d").args(["--scorecard-live", w]);
    if arg("summary") == Some("1") {
        cmd.arg("--summary");
    }
    let post = method == Method::POST;
    if post {
        cmd.args(["--body", "-"]);
    }
    let _one = ONE.lock().await;
    let child = crate::command::die_with_parent(&mut cmd)
        .stdin(if post {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn();
    let Ok(mut child) = child else {
        return failed("cannot start the card");
    };
    let mut stdin = child.stdin.take();
    let mut stdout = child.stdout.take();
    let run = async {
        if let Some(i) = stdin.as_mut() {
            i.write_all(&body).await?;
        }
        drop(stdin);
        let mut out = Vec::new();
        if let Some(o) = stdout.as_mut() {
            o.take(1 << 20).read_to_end(&mut out).await?;
        }
        Ok::<_, std::io::Error>((child.wait().await?, out))
    };
    match tokio::time::timeout(Duration::from_secs(30), run).await {
        Ok(Ok((st, out))) if st.success() => (
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; charset=utf-8",
            )],
            out,
        )
            .into_response(),
        Ok(Ok((st, _))) => failed(&format!("the card exited with {st}")),
        Ok(Err(e)) => failed(&format!("the card: {e}")),
        Err(_) => failed("the card took over 30 s"),
    }
}

/// Paths reachable without any credential even when the listener requires one.
const PUBLIC_PATHS: [&str; 4] = ["/", "/healthz", "/auth/login", "/auth/exchange"];

/// The credential a client sent, looked up in order: `Authorization: Bearer`, the older
/// `x-auth-token` header, then an `access_token=` query parameter (EventSource cannot set headers).
fn presented_credential(request: &Request) -> Option<&str> {
    let header = |name: &str| request.headers().get(name).and_then(|v| v.to_str().ok());
    if let Some(bearer) = header("authorization").and_then(|v| v.strip_prefix("Bearer ")) {
        return Some(bearer);
    }
    if let Some(token) = header("x-auth-token") {
        return Some(token);
    }
    let query = request.uri().query()?;
    query
        .split('&')
        .filter_map(|pair| pair.strip_prefix("access_token="))
        .next()
}

async fn authenticate(State(app): State<App>, request: Request, next: Next) -> Response {
    if PUBLIC_PATHS.contains(&request.uri().path()) {
        return next.run(request).await;
    }
    let allowed = match presented_credential(&request) {
        None => false,
        Some(given) if static_token_valid(app.inner.token.as_deref(), Some(given)) => true,
        Some(given) => app.inner.sessions.lock().await.validate(given),
    };
    if !allowed {
        return error_reply(
            StatusCode::UNAUTHORIZED,
            json!({"ok":false,"error":{"code":"unauthorized","message":"authentication required"}}),
        );
    }
    next.run(request).await
}

/// The fixed LAN token matches only when one is configured: no token configured never means open.
fn static_token_valid(configured: Option<&str>, presented: Option<&str>) -> bool {
    match (configured, presented) {
        (Some(expected), Some(given)) => same_bytes(given.as_bytes(), expected.as_bytes()),
        _ => false,
    }
}

fn error_reply(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}

/// A short `{"ok":false,"error":"<code>"}` reply used by the login routes.
fn auth_failure(status: StatusCode, code: &str) -> Response {
    error_reply(status, json!({"ok":false,"error":code}))
}

/// Hand out a new login session after a successful check.
async fn open_session(app: &App) -> Response {
    let issued = app.inner.sessions.lock().await.issue();
    match issued {
        Ok((token, expires_at)) => Json(auth::token_reply(token, expires_at)).into_response(),
        Err(_) => auth_failure(StatusCode::INTERNAL_SERVER_ERROR, "token_issue_failed"),
    }
}

/// `POST /auth/login`: HTTP Basic with the stock web UI's user and password.
async fn auth_login(State(app): State<App>, headers: HeaderMap) -> Response {
    let Some((user, pass)) = basic_credentials(&headers) else {
        return auth_failure(StatusCode::BAD_REQUEST, "missing_credentials");
    };
    if auth::verify_password(&user, &pass).await {
        open_session(&app).await
    } else {
        auth_failure(StatusCode::UNAUTHORIZED, "invalid_credentials")
    }
}

/// `POST /auth/exchange`: trade a token of the stock web UI (checked by `zwrt_web`) for a session.
async fn auth_exchange(
    State(app): State<App>,
    ConnectInfo(crate::conn::Peer(peer)): ConnectInfo<crate::conn::Peer>,
    headers: HeaderMap,
) -> Response {
    let text = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let web_token = text("x-web-token")
        .or_else(|| text("x-zte-webtoken"))
        .unwrap_or("");
    if web_token.is_empty() {
        return auth_failure(StatusCode::BAD_REQUEST, "missing_webtoken");
    }
    let zmode: i64 = text("x-z-mode").and_then(|v| v.parse().ok()).unwrap_or(0);
    let ztag = text("x-z-tag").unwrap_or("zwrt-datad");
    let remote = peer.ip().to_string();
    if auth::verify_webtoken(web_token, zmode, &remote, ztag).await {
        open_session(&app).await
    } else {
        auth_failure(StatusCode::UNAUTHORIZED, "invalid_webtoken")
    }
}

/// `Authorization: Basic …` split into user and password. Refused: oversized input, a NUL byte,
/// an empty user name, non-UTF-8, or either half longer than 256 bytes.
fn basic_credentials(headers: &HeaderMap) -> Option<(String, String)> {
    let raw = headers.get("authorization")?.to_str().ok()?;
    let encoded = raw.strip_prefix("Basic ")?;
    if encoded.len() > 1024 {
        return None;
    }
    let plain = decode_base64(encoded)?;
    if plain.len() > 512 || plain.contains(&0) {
        return None;
    }
    let text = String::from_utf8(plain).ok()?;
    let (user, pass) = text.split_once(':')?;
    let fits = |part: &str| part.len() <= 256;
    (!user.is_empty() && fits(user) && fits(pass)).then(|| (user.to_owned(), pass.to_owned()))
}

/// Standard-alphabet base64. Whitespace is skipped, the first `=` ends the input, any other
/// character outside the alphabet fails; leftover bits that do not fill a byte are dropped.
fn decode_base64(value: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut sextets = Vec::with_capacity(value.len());
    for c in value.bytes() {
        if c == b'=' {
            break;
        }
        if c.is_ascii_whitespace() {
            continue;
        }
        sextets.push(ALPHABET.iter().position(|&a| a == c)? as u32);
    }
    let mut bytes = Vec::with_capacity(sextets.len() * 3 / 4);
    for group in sextets.chunks(4) {
        // Pack up to four 6-bit values into the top of a 24-bit word, then take whole bytes.
        let word = group
            .iter()
            .enumerate()
            .fold(0u32, |w, (i, v)| w | (v << (18 - 6 * i)));
        let whole = group.len() * 6 / 8;
        bytes.extend((0..whole).map(|i| (word >> (16 - 8 * i)) as u8));
    }
    Some(bytes)
}

/// Equality whose running time does not depend on where the inputs differ.
fn same_bytes(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for i in 0..left.len() {
        difference |= left[i] ^ right[i];
    }
    difference == 0
}

async fn index() -> &'static str {
    "luyaz-core\n"
}
/// `/healthz`（P1-3）：200 = 第一轮采完了、执行者没卡（`exec_age_ms` ≤ 20 秒）；503 = 还在启动（`starting`）
/// 或执行者卡住（`stalled`，订阅方也按 20 秒判卡死，V2-32）。不经执行者，卡住时照样回。
async fn health(State(app): State<App>) -> Response {
    let age = app.inner.exec.exec_age_ms();
    let status = if app.stage() < Stage::Snapshot {
        "starting"
    } else if age > HEALTH_STALL_MS {
        "stalled"
    } else {
        "ok"
    };
    let code = if status == "ok" {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        code,
        Json(json!({"ok": status == "ok", "status": status, "exec_age_ms": age})),
    )
        .into_response()
}
/// POST /v2/sentinel/event (u60-sentinel.md §6): an event from luyaz-sentinel (a hand-back,
/// a crash of the screen UI, a Wi-Fi takeover …), `{"id","kind","text","wall","uptime"}`. Queued as
/// an alert once per id (`u60-alerts` `add_once`): the sentinel queues it itself when this did not
/// answer within 5 s, and then this only says it is there. Loopback only.
async fn sentinel_event(
    ConnectInfo(crate::conn::Peer(peer)): ConnectInfo<crate::conn::Peer>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Response {
    if !peer.ip().is_loopback() {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"ok": false, "error": "loopback only"})),
        )
            .into_response();
    }
    let Ok(Json(b)) = payload else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "bad json"})),
        )
            .into_response();
    };
    let (Some(id), Some(kind)) = (b["id"].as_str(), b["kind"].as_str()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "id and kind"})),
        )
            .into_response();
    };
    let text = b["text"].as_str().unwrap_or("").to_string();
    let wall = b["wall"].as_i64().unwrap_or_else(luyaz_api::alerts::now);
    let up = b["uptime"].as_u64().unwrap_or(0);
    let (id, kind) = (id.to_string(), kind.to_string());
    let dir = crate::state::host_path(luyaz_api::alerts::DIR);
    let r = tokio::task::spawn_blocking(move || {
        let a = luyaz_api::alerts::Alerts::at(std::path::Path::new(&dir));
        let r = a.add_once(&id, &kind, &text, wall, up);
        if let Ok((seq, true)) = &r {
            eprintln!("[sentinel] event {id} {kind} queued as alert {seq}: {text}");
        }
        r
    })
    .await;
    match r {
        Ok(Ok((seq, new))) => Json(json!({"ok": true, "seq": seq, "new": new})).into_response(),
        Ok(Err(e)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": e.to_string()})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": e.to_string()})),
        )
            .into_response(),
    }
}

/// doctor `--calibrate-standby`: `POST /v2/longterm/calib?id=<id>&rows=<n>` (busybox wget posts
/// a form, so the query carries it), loopback only; one `calib` event in the long-term layer.
async fn longterm_calib(
    ConnectInfo(crate::conn::Peer(peer)): ConnectInfo<crate::conn::Peer>,
    uri: axum::http::Uri,
) -> Response {
    if !peer.ip().is_loopback() {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"ok": false, "error": "loopback only"})),
        )
            .into_response();
    }
    let q = uri.query().unwrap_or("").to_string();
    let arg = |k: &str| {
        q.split('&')
            .find_map(|p| p.strip_prefix(k).and_then(|v| v.strip_prefix('=')))
            .map(str::to_string)
    };
    let (Some(id), Some(rows)) = (arg("id"), arg("rows").and_then(|r| r.parse::<u64>().ok()))
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "id and rows"})),
        )
            .into_response();
    };
    let dir = std::path::PathBuf::from(crate::state::host_path(crate::longterm::DIR));
    match tokio::task::spawn_blocking(move || crate::longterm::calib_event(&dir, &id, rows)).await {
        Ok(Ok(v)) => {
            eprintln!("[longterm] {v}");
            Json(json!({"ok": true})).into_response()
        }
        Ok(Err(e)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": e.to_string()})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": e.to_string()})),
        )
            .into_response(),
    }
}

/// Component versions, installed and newest (versions.rs).
/// `GET /v2/dual/switches?days=1..14&limit=N` (dsds_log.rs): the data-slot changes the
/// passive dual-SIM log saw, newest first: `{ok, data: {days, count, switches: [{t, from, to,
/// smart, before, after}]}}`. Reads files only (no first-round wait).
async fn dual_switches(uri: axum::http::Uri) -> Json<Value> {
    let q = uri.query().unwrap_or("").to_owned();
    let arg = |k: &str| {
        q.split('&')
            .find_map(|p| p.strip_prefix(k).and_then(|v| v.strip_prefix('=')))
            .and_then(|v| v.parse::<i64>().ok())
    };
    let (days, limit) = (arg("days").unwrap_or(14), arg("limit").unwrap_or(50).clamp(1, 200) as usize);
    let t = crate::qmi_nas::now() as i64;
    let v = tokio::task::spawn_blocking(move || {
        crate::dsds_log::switches(std::path::Path::new(crate::dsds_log::DIR), days, limit, t)
    })
    .await
    .unwrap_or(Value::Null);
    Json(json!({"ok": true, "data": v}))
}

/// `GET /v2/usage/days?n=1..400` (usage_log.rs): data usage per device date and card, newest
/// first, today included: `{ok, data: {days: [{day, cards: [{card, tail, op, rx, tx}]}]}}`.
/// Reads files only (no first-round wait).
async fn usage_days(uri: axum::http::Uri) -> Json<Value> {
    let n = uri
        .query()
        .unwrap_or("")
        .split('&')
        .find_map(|p| p.strip_prefix("n="))
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(30);
    let v = tokio::task::spawn_blocking(move || {
        crate::usage_log::days(std::path::Path::new(crate::usage_log::DIR), n)
    })
    .await
    .unwrap_or(Value::Null);
    Json(json!({"ok": true, "data": v}))
}

/// `GET /v2/usage/landing` (usage_log.rs): the last arrival in a country and the bytes used
/// since: `{ok, data: {mcc, from, at, rx, tx} | null}` (`from` null: the first record, not a
/// landing). Reads memory or a file only.
async fn usage_landing() -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| {
        crate::usage_log::landing(std::path::Path::new(crate::usage_log::DIR))
    })
    .await
    .unwrap_or(Value::Null);
    Json(json!({"ok": true, "data": v}))
}

async fn versions() -> Json<Value> {
    Json(crate::versions::report())
}
/// Look at the public repo now (the screen's and the web's 「检查更新」). The check runs on its own
/// task: the screen hangs up after a second and reads the answer later.
async fn versions_check() -> Json<Value> {
    match tokio::spawn(crate::versions::check()).await {
        Ok(v) => Json(v),
        Err(_) => Json(crate::versions::report()),
    }
}

async fn version() -> Json<CoreVersion> {
    Json(Default::default())
}
/// 旧的 `/state`、`/events`（2026-10 删，legacy-api-removal.md）：回 410，漏改的调用者看这条错误
/// 就知道该换 `/v2/state`、`/v2/events`。
async fn gone() -> Response {
    (
        StatusCode::GONE,
        Json(json!({"ok":false,"error":{"code":"gone","message":"removed; use /v2/state and /v2/events"}})),
    )
        .into_response()
}
/// Every `/control` action this build accepts: the interval setter plus `control::ACTIONS`.
fn capability_controls() -> Vec<&'static str> {
    std::iter::once("state.set_interval")
        .chain(crate::control::ACTIONS.iter().copied())
        .collect()
}

/// Passive shared report. Available before the first collection (all unknown), protected
/// by the listener's existing authentication policy. Reading never kicks the executor.
async fn device_capabilities(State(app): State<App>) -> impl IntoResponse {
    let report = app
        .inner
        .exec
        .hub()
        .firmware()
        .report(luyaz_api::clock::BootTime::read().as_ref());
    (
        [(axum::http::header::CACHE_CONTROL, "private, no-store")],
        Json(report),
    )
}

/// `/capabilities`: what this core speaks. Both `control` and `controls` carry the same list
/// (older clients read one, newer the other).
async fn capabilities() -> Json<Value> {
    let actions = Value::from(capability_controls());
    let mut reply = Map::new();
    reply.insert("schema_version".into(), 1.into());
    reply.insert("protocol".into(), 1.into());
    reply.insert("events".into(), json!(["snapshot", "block"]));
    reply.insert("transport".into(), json!(["http", "sse"]));
    reply.insert("control".into(), actions.clone());
    reply.insert("controls".into(), actions);
    reply.insert("rewrite".into(), "rust".into());
    Json(Value::Object(reply))
}

/// `/v2/events`（STATE_V2.md 第 2–4 节）：SSE 连接名额满了回 503。
async fn v2_events(State(app): State<App>) -> Response {
    let Ok(permit) = Arc::clone(&app.inner.sse_slots).try_acquire_owned() else {
        return error_reply(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"ok":false,"error":{"code":"sse_client_limit","message":"too many SSE clients"}}),
        );
    };
    let stream = v2::stream(app.inner.exec.hub(), &app.inner.feed, permit);
    v2::events_response(futures_util::StreamExt::take_until(stream, app.stopped()))
}

/// `/v2/state`（V2-6）：调试用，内容同 snapshot。
async fn v2_state(State(app): State<App>) -> Response {
    v2::state_response(app.inner.exec.hub(), &app.inner.feed)
}

/// `/v2/screen`：触屏首页信号卡和状态栏的结论（screen.rs），从当前这份 /state 算，
/// 另加采样循环记的 30 秒收发包数（cell_window.rs，判 stall 用；不进 /state）。
/// 不冻结（不是旧接口），靠 `v` 区分版本；`ts` 就是算它用的那份快照的 `ts`。
async fn v2_screen(State(app): State<App>) -> Json<Value> {
    Json(app.screen_json().await)
}

/// `/v2/screen` 的回复。`exec_age_ms`（V2-40）：触屏不订阅 `/v2/events`、看不到心跳，
/// 从这里判断核心卡没卡（这个请求读的是现成的快照，不经执行者，卡住时照样回）。
fn screen_json(
    ts: i64,
    net: crate::project::screen::NetView,
    op: Value,
    exec_age_ms: u64,
) -> Value {
    serde_json::json!({
        "v": crate::project::screen::SCREEN_VERSION,
        "ts": ts,
        "net": net,
        "op": op,
        "exec_age_ms": exec_age_ms,
    })
}

async fn control(
    State(app): State<App>,
    peer: Option<axum::Extension<ConnectInfo<crate::conn::Peer>>>,
    method: Method,
    payload: Result<Json<Value>, JsonRejection>,
) -> Response {
    let _ = method;
    let (action, body) = match control_request(payload) {
        Ok(parsed) => parsed,
        Err(refusal) => return *refusal,
    };
    let peer = peer.map(|p| p.0.0.0);
    write::handle(app, action, body, peer).await
}

/// Check a `/control` body: valid JSON, a non-empty `action`, and `params` (when present) an
/// object. Returns the action name and the body, or the 400 reply to send.
fn control_request(payload: Result<Json<Value>, JsonRejection>) -> Result<(String, Value), Box<Response>> {
    let invalid = |action: Option<&str>, message: &str| {
        let mut reply = json!({"ok":false,"error":{"code":"invalid_request","message":message}});
        if let Some(name) = action {
            reply["action"] = Value::from(name);
        }
        Box::new(error_reply(StatusCode::BAD_REQUEST, reply))
    };
    let Ok(Json(body)) = payload else {
        return Err(invalid(None, "request body must be valid JSON"));
    };
    let action = match body.get("action").and_then(Value::as_str) {
        Some(name) if !name.is_empty() => name.to_owned(),
        _ => return Err(invalid(None, "missing action")),
    };
    if matches!(body.get("params"), Some(params) if !params.is_object()) {
        return Err(invalid(Some(&action), "params must be a JSON object"));
    }
    Ok((action, body))
}

/// Resolves on SIGTERM (procd's stop) or Ctrl-C.
async fn termination_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut sigterm) => {
                tokio::select! {
                    _ = sigterm.recv() => {}
                    _ = tokio::signal::ctrl_c() => {}
                }
            }
            Err(e) => panic!("install SIGTERM handler: {e}"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn shutdown(app: App) {
    termination_signal().await;
    // a requested stop (X9): the running marker goes, so the next start does not call it a crash
    crate::svcwatch::clean_stop();
    // lan-client-id.md §4a: what lanid learned since its last hourly write
    crate::lanid::flush(true);
    // 收到 SIGTERM/SIGINT：先让 SSE 流结束（P2-4），再收掉 ubus listen 的子进程（最多 2 秒），
    // 然后 axum 等连接收尾（`serve` 里另有总上限）。
    app.inner.stop.send_replace(true);
    crate::ubus::listen::shutdown(std::time::Duration::from_secs(2)).await;
}

#[cfg(test)]
#[path = "server/device_capabilities_tests.rs"]
mod device_capabilities_tests;

#[cfg(test)]
#[path = "server/write_lock_tests.rs"]
mod write_lock_tests;

#[cfg(test)]
mod tests {
    #[test]
    fn screen_reply_carries_executor_age() {
        let v = super::screen_json(
            7,
            crate::project::screen::NetView::default(),
            serde_json::json!({"active": null}),
            21_000,
        );
        assert_eq!(v["exec_age_ms"], 21_000);
        assert_eq!(v["ts"], 7);
        assert!(v["op"].is_object() && v["net"].is_object());
    }

    use super::*;
    use std::collections::HashSet;
    #[test]
    fn version_shape() {
        let v = serde_json::to_value(CoreVersion::default()).unwrap();
        assert_eq!(v["name"], "u60d"); // u60d: CoreVersion reports u60d (PORTING.md)
    }

    #[test]
    fn capability_controls_are_the_kept_actions() {
        let controls = capability_controls();
        assert_eq!(controls.len(), 27);
        assert_eq!(controls.iter().copied().collect::<HashSet<_>>().len(), 27);
    }

    #[test]
    fn missing_static_token_never_disables_lan_authentication() {
        assert!(!static_token_valid(None, None));
        assert!(!static_token_valid(None, Some("anything")));
        assert!(static_token_valid(Some("secret"), Some("secret")));
        assert!(!static_token_valid(Some("secret"), Some("wrong")));
    }
}
