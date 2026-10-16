//! 单一采集执行者（docs/STATE_V2.md 第 7、8 节，R13/R3/R12/R2）。
//!
//! 一个 tokio 任务持有 ubus 后端，datad 里所有 ubus 请求都经过它，同一时间最多一个在途：
//!
//! - **采集轮**：先读到期的块（`block::Hub`，每轮 ubus 预算 3 秒、没轮到的下一轮先读），
//!   再跑还没迁移的旧采集（`RoundDriver::legacy`，即 `state::collect` 那一套，不受 3 秒预算截断；
//!   只在本轮已有 `LEGACY_TIMEOUT_LIMIT` 个对象超时后不再发新请求），
//!   最后 `Hub::round_end`：max_age、轮末钩子、心跳（`seq` 在那里分配）。轮拖得很长时，独立心跳定时器也判 max_age。
//! - **任务**：`/control` 整个处理过程作为一个控制任务交给执行者（`Executor::control`，排队上限 8，
//!   满了立即 `Busy`）；其他地方（登录校验等）在执行者外调 `state::ubus` 时，自动包成一个内部任务
//!   （不计上限）。任务在块与块之间、旧采集的 `ubus_ttl` 调用之前（`preempt`）、以及轮间睡眠时执行，
//!   先来先做，一个任务里的多次调用连着做完。
//!
//! 执行者里的代码用 task-local（`CTX`）认出自己，直接用后端；执行者外的调用走队列。
//! 任务只在这些「安全点」执行：那里调用方不持有任何锁（旧采集里 `sms::prepare` 持着锁调 ubus，
//! 所以 `state::ubus` 本身不是安全点，只有 `ubus_ttl` 是）。

use crate::{
    block::{Hub, Schedule},
    ubus::{
        backend::UbusBackend,
        client::{RoundSkips, UbusError},
    },
};
use serde_json::Value;
use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Notify, oneshot},
    time::Instant,
};

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// V2-19：每轮 ubus 预算。
pub const ROUND_BUDGET: Duration = Duration::from_secs(3);
/// V2-25：排队中的控制任务上限。
pub const CONTROL_QUEUE: usize = 8;
/// 一次调用最长多久还算「在它自己的超时里」（cli、socket 的超时都是 8 秒，留 2 秒余量）。
/// 看门狗的上限必须大于它（V2-32）。
pub const CALL_LIMIT: Duration = Duration::from_secs(10);
/// V2-33：调用超时后最多探测几次。
pub const GATE_PROBES: u32 = 4;
/// 旧采集里超时过的对象跨轮冷却的建议值（`LUYAZ_UBUS_COOLDOWN_MS=10000`）。**默认关**（0，只按本轮跳过），
/// 设备试跑验证后才在启动环境里打开。上游 v0.10.56 用 30 秒（它的采集超时是 5 秒）；我们 socket 采集超时 2 秒，
/// 慢但有效的基带回复也可能踩到，所以取短一些，免得信号数据空太久。程序里不用（默认关），只给测试。
#[cfg(test)]
pub const COOLDOWN: Duration = Duration::from_secs(10);
pub const ENV_COOLDOWN_MS: &str = "LUYAZ_UBUS_COOLDOWN_MS";
/// 旧采集的超时上限（V2-19）：本轮已有这么多个对象超时（块和旧采集合计），旧采集就不再发新请求，
/// 剩下的读取本轮算跳过（`Skipped`，有 TTL 的沿用上一次的值，`state::ubus_ttl_read`）。
/// 只按超时数，不按用掉的时间：慢但在回答的轮照旧全读。ubusd 整个不回时一轮从 13 个对象挨个超时
/// （socket 约 26 秒、cli 约 104 秒）缩到 6 个；基带重启时只有基带那几个对象（mdm、nwinfo、data、wms）超时，到不了 6 个。
pub const LEGACY_TIMEOUT_LIMIT: usize = 6;

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub budget: Duration,
    /// `LUYAZ_CACHE` 没设成 0。
    pub cache: bool,
    pub control_queue: usize,
    /// 旧采集里超时的对象之后多久不再调（跨轮；0 = 关，默认）。块不受影响（块有自己的失败重读间隔）。
    pub cooldown: Duration,
}

impl Config {
    /// `LUYAZ_UBUS_COOLDOWN_MS` 的值：未设置、不是数字或 0 → 关；否则这么多毫秒。
    pub fn cooldown_from(value: Option<&str>) -> Duration {
        value
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map_or(Duration::ZERO, Duration::from_millis)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            budget: ROUND_BUDGET,
            cache: true,
            control_queue: CONTROL_QUEUE,
            cooldown: Duration::ZERO,
        }
    }
}

/// 控制队列满了（V2-25）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Busy;

/// 可装箱的后端（`UbusBackend` 用 `impl Future`，不能直接做 trait object）。
trait DynBackend: Send {
    fn call<'a>(
        &'a mut self,
        object: &'a str,
        method: &'a str,
        args: &'a Value,
    ) -> BoxFuture<'a, Result<Value, UbusError>>;
    fn set_round(&mut self, round: bool);
}

impl<B: UbusBackend> DynBackend for B {
    fn call<'a>(
        &'a mut self,
        object: &'a str,
        method: &'a str,
        args: &'a Value,
    ) -> BoxFuture<'a, Result<Value, UbusError>> {
        Box::pin(UbusBackend::call(self, object, method, args))
    }
    fn set_round(&mut self, round: bool) {
        UbusBackend::set_round(self, round);
    }
}

/// 旧采集（未迁移的读取）。每轮在块之后跑一次，future 在执行者里执行。
pub trait RoundDriver: Send + Sync {
    fn legacy(&self) -> BoxFuture<'static, ()>;
}

struct Job {
    control: bool,
    fut: BoxFuture<'static, ()>,
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    /// 排队中的控制任务数（正在执行的不算）。
    controls: usize,
}

#[derive(Default)]
struct RoundState {
    skips: RoundSkips,
    used: Duration,
    /// 本轮已经记过「旧采集到了超时上限」。
    cut_logged: bool,
}

/// 活性（D12、V2-32）：执行者最近一次「前进」= 完成一次调用、一个任务、一轮，或者闲着在等活。
struct Liveness {
    epoch: Instant,
    /// 最近一次前进（距 epoch 的毫秒 + 1）。
    last_ms: AtomicU64,
    /// 正在干活（闲着等活时不算卡）。
    busy: AtomicBool,
    /// 在途调用的「自己的超时」到哪（距 epoch 的毫秒 + 1；0 = 没有在途调用）。
    call_until_ms: AtomicU64,
}

impl Liveness {
    fn new() -> Self {
        Self {
            epoch: Instant::now(),
            last_ms: AtomicU64::new(1),
            // 刚起来就在干活（第一轮）；没活时主循环会标成闲着。
            busy: AtomicBool::new(true),
            call_until_ms: AtomicU64::new(0),
        }
    }
    fn at(&self, t: Instant) -> u64 {
        t.saturating_duration_since(self.epoch).as_millis() as u64 + 1
    }
    fn progress(&self) {
        self.last_ms
            .store(self.at(Instant::now()), Ordering::Relaxed);
    }
    fn set_busy(&self, busy: bool) {
        if busy {
            self.progress();
        }
        self.busy.store(busy, Ordering::Relaxed);
    }
    fn call_started(&self) {
        self.call_until_ms
            .store(self.at(Instant::now() + CALL_LIMIT), Ordering::Relaxed);
    }
    fn call_done(&self) {
        self.call_until_ms.store(0, Ordering::Relaxed);
        self.progress();
    }
    fn age_ms(&self, now: Instant) -> u64 {
        if !self.busy.load(Ordering::Relaxed) {
            return 0;
        }
        self.at(now)
            .saturating_sub(self.last_ms.load(Ordering::Relaxed))
    }
    /// 看门狗：超过 `limit` 没前进，并且不是一个还在自己超时里的调用。
    fn stalled(&self, now: Instant, limit: Duration) -> bool {
        let until = self.call_until_ms.load(Ordering::Relaxed);
        self.age_ms(now) > limit.as_millis() as u64 && !(until != 0 && self.at(now) < until)
    }
}

/// 计数（测试和日志用）。
#[derive(Debug, Default)]
pub struct Stats {
    pub rounds: AtomicU64,
    pub calls: AtomicU64,
}

struct Shared {
    queue: Mutex<Queue>,
    wake: Notify,
    backend: tokio::sync::Mutex<Box<dyn DynBackend>>,
    round: Mutex<RoundState>,
    hub: Arc<Hub>,
    cfg: Config,
    interval_ms: AtomicU64,
    /// 轮转：下一轮从这块开始（V2-20）。
    cursor: Mutex<usize>,
    driver: Mutex<Option<Arc<dyn RoundDriver>>>,
    /// 下一轮不等采样间隔、立即开始（短信事件，V2-31）。
    kick: AtomicBool,
    stats: Stats,
    live: Liveness,
    /// V2-33（D28）：写的上下文里调用超时了，这是那个对象。下一个写调用之前先探测它，回答了才放行。
    gate: Mutex<Option<String>>,
    /// 正在做写（拿着跨进程写锁的任务，`write_scope`）。
    writing: AtomicBool,
    /// 正在跑一轮（V2-13：一轮拖得很长时，独立定时器替轮末判 max_age）。
    in_round: AtomicBool,
    /// 旧采集里超时过的对象 → 冷却到什么时候（跨轮，`Config::cooldown`）。
    cooldown: Mutex<HashMap<String, Instant>>,
}

/// 写的上下文：拿着跨进程写锁的那段（`ops::write_lock`）。只有这里面的调用超时才关闸，
/// 也只有这里面的调用要等闸（D28 说的是「不放给下一个写」；读超时不关闸，免得确认期间整个执行者陪着探测）。
pub struct WriteScope(Option<Arc<Shared>>);

impl Drop for WriteScope {
    fn drop(&mut self) {
        if let Some(s) = &self.0 {
            s.writing.store(false, Ordering::Relaxed);
        }
    }
}

/// 在执行者里开始一段写；不在执行者里时什么都不做。
pub fn write_scope() -> WriteScope {
    WriteScope(
        CTX.try_with(|c| {
            c.shared.writing.store(true, Ordering::Relaxed);
            c.shared.clone()
        })
        .ok(),
    )
}

/// 探测用的只读请求：会被 `/control` 写到的原厂对象各一个（块表里没有它们）。
const PROBES: &[(&str, &str)] = &[
    ("zte_nwinfo_api", "nwinfo_get_netinfo"),
    ("zwrt_data", "get_wwaniface"),
    ("zwrt_zte_mdm.api", "get_sim_info"),
    ("zwrt_apn_object", "get_apn_mode"),
    ("zwrt_router.api", "router_get_status_no_auth"),
    ("zwrt_bsp.usb", "list"),
    ("zwrt_wms", "zwrt_wms_get_wms_capacity"),
    ("zwrt_nfc", "zwrt_nfc_wifi_get"),
];

#[derive(Clone)]
struct Ctx {
    shared: Arc<Shared>,
    /// 采集轮里（计预算、按 RoundSkips 跳过；可以在安全点插任务）。
    round: bool,
}

tokio::task_local! {
    static CTX: Ctx;
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Clone)]
pub struct Executor {
    shared: Arc<Shared>,
}

static GLOBAL: OnceLock<Executor> = OnceLock::new();

/// 装成全局执行者（`state::ubus` 在执行者外调用时用它）。只装一次。
pub fn install(e: &Executor) {
    let _ = GLOBAL.set(e.clone());
}

/// 当前是否在执行者里（测试用）。
#[cfg(test)]
pub fn on_executor() -> bool {
    CTX.try_with(|_| ()).is_ok()
}

/// 发一个 ubus 请求。执行者里直接发；执行者外包成内部任务排队。
pub async fn call(object: &str, method: &str, args: &Value) -> Result<Value, UbusError> {
    if let Ok(ctx) = CTX.try_with(Ctx::clone) {
        return ctx
            .shared
            .call_here(ctx.round, ctx.round, object, method, args)
            .await;
    }
    let Some(exec) = GLOBAL.get() else {
        return Err(UbusError::Io("ubus executor not running".into()));
    };
    exec.call(object, method, args).await
}

/// 任务里一段自己有上限的等待做完了（拿到跨进程写锁、AT 锁，一次 AT 交换，一个子进程）：记一次前进（V2-32）。
/// 这些等待不是 ubus 调用，不经 `raw_call`，不记的话一个写任务连着等锁、等 AT 可以 30 多秒不前进，
/// 订阅方 20 秒就判卡死、看门狗 30 秒退出。每段都有自己的上限（写锁 15 秒、AT 锁 10 秒……），
/// 所以真卡在某一段时照样不前进、照样触发看门狗。不在执行者里时什么都不做。
pub fn progress() {
    let _ = CTX.try_with(|c| c.shared.live.progress());
}

/// 阻塞线程里用的 `progress`（`spawn_blocking` 里没有 task-local）：在任务里先拿，再带进去。
#[derive(Clone)]
pub struct ProgressHandle(Option<Arc<Shared>>);

impl ProgressHandle {
    pub fn tick(&self) {
        if let Some(s) = &self.0 {
            s.live.progress();
        }
    }
}

pub fn progress_handle() -> ProgressHandle {
    ProgressHandle(CTX.try_with(|c| c.shared.clone()).ok())
}

/// 安全点：在采集轮里时，先把排队的任务做完（V2-24）。调用方不能持有任何锁。
pub async fn preempt() {
    if let Ok(ctx) = CTX.try_with(Ctx::clone)
        && ctx.round
    {
        ctx.shared.drain().await;
    }
}

impl Shared {
    /// `cool`：旧采集里的调用（跨轮冷却只管这些；块和任务不管）。
    async fn call_here(
        &self,
        round: bool,
        cool: bool,
        object: &str,
        method: &str,
        args: &Value,
    ) -> Result<Value, UbusError> {
        // V2-33：只有写的上下文等闸、关闸；采集轮和别的读不管（读超时照 V2-18 处理）。
        let writing = !round && self.writing.load(Ordering::Relaxed);
        if writing {
            self.reopen_gate().await;
        }
        let r = self.raw_call(round, cool, object, method, args).await;
        if writing && matches!(r, Err(UbusError::Timeout { .. })) {
            // 我们这边超时了，原厂那边可能还在做：结果未知，闸先关上。
            eprintln!(
                "executor: {object} {method} timed out; holding writes until {object} answers again"
            );
            *lock(&self.gate) = Some(object.to_owned());
        }
        r
    }

    /// 闸关着就用这个对象的只读请求探测（每次 ≤8 秒，最多 4 次）；回答了（成功或报错都算）就放闸。
    /// 4 次都超时也放闸并记一行：原厂可能还没好，写的结果交给事务按读回判断。探测本身算前进。
    async fn reopen_gate(&self) {
        let Some(object) = lock(&self.gate).clone() else {
            return;
        };
        let (o, m, a) = self.probe_for(&object);
        for n in 1..=GATE_PROBES {
            match self.raw_call(false, false, &o, &m, &a).await {
                Err(UbusError::Timeout { .. }) => {
                    eprintln!("executor: {object} probe {n}/{GATE_PROBES} timed out");
                }
                _ => {
                    eprintln!("executor: {object} answers again");
                    *lock(&self.gate) = None;
                    return;
                }
            }
        }
        eprintln!(
            "executor: {object} still not answering after {GATE_PROBES} probes, letting writes through"
        );
        *lock(&self.gate) = None;
    }

    /// 探测用的只读请求：先查 `PROBES`，再查块表里读这个对象的请求；都没有就问 ubusd 本身（`system board`）。
    fn probe_for(&self, object: &str) -> (String, String, Value) {
        if let Some((o, m)) = PROBES.iter().find(|(o, _)| *o == object) {
            let args = if *o == "zwrt_data" {
                serde_json::json!({"source_module":"web","cid":1,"connect_status":""})
            } else {
                Value::Object(Default::default())
            };
            return ((*o).into(), (*m).into(), args);
        }
        for i in 0..self.hub.len() {
            let (o, m, a) = self.hub.request(i);
            if o == object {
                return (o.into(), m.into(), a);
            }
        }
        (
            "system".into(),
            "board".into(),
            Value::Object(Default::default()),
        )
    }

    async fn raw_call(
        &self,
        round: bool,
        cool: bool,
        object: &str,
        method: &str,
        args: &Value,
    ) -> Result<Value, UbusError> {
        let legacy = round && cool;
        let cool = legacy && !self.cfg.cooldown.is_zero();
        if (round && lock(&self.round).skips.is_skipped(object))
            || (cool && self.cooling(object, Instant::now()))
            || (legacy && self.legacy_cut(object))
        {
            return Err(UbusError::Skipped {
                object: object.into(),
            });
        }
        self.stats.calls.fetch_add(1, Ordering::Relaxed);
        let start = Instant::now();
        // 采集轮里用短超时（socket 2 秒），控制任务和内部任务用长超时（8 秒，V2-19）。
        self.live.call_started();
        let r = {
            let mut b = self.backend.lock().await;
            b.set_round(round);
            b.call(object, method, args).await
        };
        self.live.call_done();
        if round {
            let mut rs = lock(&self.round);
            rs.used += start.elapsed();
            if let Err(e) = &r {
                rs.skips.record(e);
            }
        }
        if cool && matches!(r, Err(UbusError::Timeout { .. })) {
            let until = Instant::now() + self.cfg.cooldown;
            eprintln!(
                "executor: {object} {method} timed out in a round; not read again for {} ms",
                self.cfg.cooldown.as_millis()
            );
            lock(&self.cooldown).insert(object.to_owned(), until);
        }
        r
    }

    /// 旧采集到了本轮的超时上限（`LEGACY_TIMEOUT_LIMIT`）吗；第一次到的时候记一行。
    fn legacy_cut(&self, object: &str) -> bool {
        let mut rs = lock(&self.round);
        let n = rs.skips.timed_out();
        if n < LEGACY_TIMEOUT_LIMIT {
            return false;
        }
        if !rs.cut_logged {
            rs.cut_logged = true;
            eprintln!(
                "executor: {n} objects timed out this round; skipping the rest of the legacy reads (from {object})"
            );
        }
        true
    }

    /// 这个对象还在跨轮冷却里吗（过期的顺手删掉）。
    fn cooling(&self, object: &str, now: Instant) -> bool {
        let mut c = lock(&self.cooldown);
        match c.get(object) {
            Some(&until) if now < until => true,
            Some(_) => {
                c.remove(object);
                false
            }
            None => false,
        }
    }

    /// 安全点：按先后做完「进入时已在排队的」任务（快照计数），做的过程中新来的留给下一个安全点。
    /// 控制任务和内部任务同一规则，所以持续有任务进来也挡不住采集轮和轮末心跳（V2-24）。
    async fn drain(self: &Arc<Self>) {
        let n = lock(&self.queue).jobs.len();
        for _ in 0..n {
            let job = {
                let mut q = lock(&self.queue);
                let job = q.jobs.pop_front();
                if job.as_ref().is_some_and(|j| j.control) {
                    q.controls -= 1;
                }
                job
            };
            let Some(job) = job else { break };
            let ctx = Ctx {
                shared: self.clone(),
                round: false,
            };
            CTX.scope(ctx, job.fut).await;
            self.live.progress();
        }
    }

    fn queued(&self) -> bool {
        !lock(&self.queue).jobs.is_empty()
    }

    fn sample_interval(&self) -> Duration {
        Duration::from_millis(self.interval_ms.load(Ordering::Relaxed))
    }

    fn schedule(&self) -> Schedule {
        Schedule {
            cache: self.cfg.cache,
            sample_interval: self.sample_interval(),
        }
    }

    /// 一轮：块（预算内、轮转）→ 旧采集 → 轮末（max_age、心跳）。
    async fn round<T: Send>(self: &Arc<Self>, legacy: Option<BoxFuture<'_, T>>) -> Option<T> {
        {
            let mut rs = lock(&self.round);
            rs.skips.clear();
            rs.used = Duration::ZERO;
            rs.cut_logged = false;
        }
        self.in_round.store(true, Ordering::Relaxed);
        let ctx = Ctx {
            shared: self.clone(),
            round: true,
        };
        let out = CTX
            .scope(ctx, async {
                self.read_blocks().await;
                match legacy {
                    Some(f) => {
                        preempt().await;
                        Some(f.await)
                    }
                    None => None,
                }
            })
            .await;
        self.drain().await;
        self.live.progress();
        self.in_round.store(false, Ordering::Relaxed);
        self.hub.round_end(Instant::now(), self.sample_interval());
        self.stats.rounds.fetch_add(1, Ordering::Relaxed);
        out
    }

    async fn read_blocks(self: &Arc<Self>) {
        let n = self.hub.len();
        if n == 0 {
            return;
        }
        let start = *lock(&self.cursor) % n;
        for k in 0..n {
            let i = (start + k) % n;
            // V2-24：每读完一块（和读第一块之前），先做排队的任务。
            self.drain().await;
            if !self.hub.due(i, Instant::now(), self.schedule()) {
                continue;
            }
            // V2-19：满预算就不再开始新请求；没轮到的下一轮从这里开始（V2-20）。
            if lock(&self.round).used >= self.cfg.budget {
                *lock(&self.cursor) = i;
                return;
            }
            let (object, method, args) = self.hub.request(i);
            let r = self.call_here(true, false, object, method, &args).await;
            self.hub
                .record_read(i, r.map_err(|e| e.to_string()), Instant::now());
        }
    }

    fn push(&self, job: Job) -> Result<(), Busy> {
        {
            let mut q = lock(&self.queue);
            if job.control {
                if q.controls >= self.cfg.control_queue {
                    return Err(Busy);
                }
                q.controls += 1;
            }
            q.jobs.push_back(job);
        }
        self.wake.notify_one();
        Ok(())
    }

    /// 执行者主循环：没开始采集时只做任务；开始后「睡一个采样间隔（期间照做任务）→ 一轮」。
    async fn run(self: Arc<Self>) {
        let mut last_round_end = Instant::now();
        loop {
            let driver = lock(&self.driver).clone();
            let deadline = driver
                .as_ref()
                .map(|_| last_round_end + self.sample_interval());
            // V2-31：被踢过就不等间隔。轮本身仍在这个循环里串行跑，轮里照样先做排队的控制任务。
            let kicked = deadline.is_some() && self.kick.swap(false, Ordering::AcqRel);
            match deadline {
                Some(d) if kicked || Instant::now() >= d => {
                    let legacy = driver.map(|d| d.legacy());
                    self.round(legacy).await;
                    last_round_end = Instant::now();
                    continue;
                }
                Some(_) if self.queued() => {}
                Some(d) => {
                    self.live.set_busy(false);
                    tokio::select! {
                        _ = self.wake.notified() => {}
                        _ = tokio::time::sleep_until(d) => {}
                    }
                    self.live.set_busy(true);
                }
                None if self.queued() => {}
                None => {
                    self.live.set_busy(false);
                    self.wake.notified().await;
                    self.live.set_busy(true);
                }
            }
            self.drain().await;
            if deadline.is_none() && lock(&self.driver).is_some() {
                // 刚开始采集：从现在起睡一个间隔。
                last_round_end = Instant::now();
            }
        }
    }
}

impl Executor {
    /// 起执行者任务。之后要 `start_rounds` 才开始周期采集。
    pub fn spawn<B: UbusBackend + 'static>(
        backend: B,
        hub: Arc<Hub>,
        cfg: Config,
        sample_interval: Duration,
    ) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::default(),
            wake: Notify::new(),
            backend: tokio::sync::Mutex::new(Box::new(backend)),
            round: Mutex::default(),
            hub,
            cfg,
            interval_ms: AtomicU64::new(sample_interval.as_millis() as u64),
            cursor: Mutex::new(0),
            driver: Mutex::new(None),
            kick: AtomicBool::new(false),
            stats: Stats::default(),
            live: Liveness::new(),
            gate: Mutex::new(None),
            writing: AtomicBool::new(false),
            in_round: AtomicBool::new(false),
            cooldown: Mutex::default(),
        });
        tokio::spawn(shared.clone().run());
        Self { shared }
    }

    pub fn hub(&self) -> &Arc<Hub> {
        &self.shared.hub
    }

    #[cfg(test)]
    pub fn stats(&self) -> &Stats {
        &self.shared.stats
    }

    /// D12：执行者多久没前进（闲着等活时是 0）。
    pub fn exec_age_ms(&self) -> u64 {
        self.shared.live.age_ms(Instant::now())
    }

    /// 看门狗（V2-32）：超过 `limit` 没前进，并且不是一个还在自己超时里的调用。
    pub fn stalled(&self, limit: Duration) -> bool {
        self.shared.live.stalled(Instant::now(), limit)
    }

    /// V2-22：独立定时器的心跳。每 `every` 看一次，距上一条心跳满 `every` 就补一条（带 exec_age_ms）。
    /// 不经执行者：执行者被长任务占着或卡住时照样发。
    /// V2-13：正在跑一轮时（比如 ubusd 不回、旧采集挨个超时），同时判一次 max_age，块不必等到轮末才置 stale。
    /// 轮间的长任务（短信发送等）不判：下一轮会先读块，用不着让块翻 stale 再翻回来。
    pub fn start_heartbeat(&self, every: Duration) {
        let shared = self.shared.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                let now = Instant::now();
                if shared.in_round.load(Ordering::Relaxed) {
                    shared.hub.expire(now, shared.sample_interval());
                }
                shared
                    .hub
                    .heartbeat_if_quiet(now, shared.live.age_ms(now), every);
            }
        });
    }

    pub fn interval_ms(&self) -> u64 {
        self.shared.interval_ms.load(Ordering::Relaxed)
    }

    /// `state.set_interval`：改执行者的睡眠间隔，正在睡的这一次按新间隔重算。
    pub fn set_interval_ms(&self, ms: u64) {
        self.shared.interval_ms.store(ms, Ordering::Relaxed);
        self.shared.wake.notify_one();
    }

    /// 从现在起周期采集：睡一个采样间隔，采一轮，如此往复。
    pub fn start_rounds(&self, driver: Arc<dyn RoundDriver>) {
        *lock(&self.shared.driver) = Some(driver);
        self.shared.wake.notify_one();
    }

    /// 立刻采一轮（启动时的第一轮），`legacy` 是这一轮的旧采集；返回它的结果。
    pub async fn round_now<T, F>(&self, legacy: F) -> T
    where
        T: Send + 'static,
        F: Future<Output = T> + Send + 'static,
    {
        let shared = self.shared.clone();
        self.task(async move {
            shared
                .round(Some(Box::pin(legacy)))
                .await
                .expect("legacy result")
        })
        .await
    }

    fn submit<T, F>(&self, control: bool, fut: F) -> Result<oneshot::Receiver<T>, Busy>
    where
        T: Send + 'static,
        F: Future<Output = T> + Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        self.shared.push(Job {
            control,
            fut: Box::pin(async move {
                // 请求方断开了也做完（/control 一直是做完才回复）。
                let _ = tx.send(fut.await);
            }),
        })?;
        Ok(rx)
    }

    /// 控制任务（V2-24、V2-25）：排队中的已有 8 个就立即 `Busy`；否则挂到做完。
    pub async fn control<T, F>(&self, fut: F) -> Result<T, Busy>
    where
        T: Send + 'static,
        F: Future<Output = T> + Send + 'static,
    {
        let rx = self.submit(true, fut)?;
        Ok(rx.await.expect("executor dropped a control task"))
    }

    /// 发一个 ubus 请求：执行者里直接发，执行者外包成内部任务排队（不计控制队列上限）。
    pub async fn call(&self, object: &str, method: &str, args: &Value) -> Result<Value, UbusError> {
        if let Ok(ctx) = CTX.try_with(Ctx::clone) {
            return ctx
                .shared
                .call_here(ctx.round, ctx.round, object, method, args)
                .await;
        }
        let (o, m, a) = (object.to_owned(), method.to_owned(), args.clone());
        self.task(async move {
            let ctx = CTX.with(Ctx::clone);
            ctx.shared.call_here(false, false, &o, &m, &a).await
        })
        .await
    }

    /// 内部任务（不计上限）。
    pub async fn task<T, F>(&self, fut: F) -> T
    where
        T: Send + 'static,
        F: Future<Output = T> + Send + 'static,
    {
        let rx = self
            .submit(false, fut)
            .expect("internal tasks are unbounded");
        rx.await.expect("executor dropped a task")
    }

    /// 短信事件（V2-31）：`names` 的块标成立即读，并让下一轮不等采样间隔立即开始。
    /// 正在跑一轮时，这一轮跑完马上再跑一轮；多次调用在开始前只算一次。不另起并发的采集。
    pub fn kick(&self, names: &[&str]) {
        self.shared.hub.mark_immediate(Some(names));
        self.shared.kick.store(true, Ordering::Release);
        self.shared.wake.notify_one();
    }

    /// `/control` 成功后：相关块下一轮立即读，`None` = 全部块（R12）。不另起一轮（V2-27）。
    pub fn mark_immediate(&self, names: Option<&[&str]>) {
        self.shared.hub.mark_immediate(names);
    }
}

#[cfg(test)]
mod tests;
