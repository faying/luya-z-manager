//! 块模型（docs/STATE_V2.md 第 5、6 节）：每块的 revision、stale、max_age、采集间隔，
//! 以及 `/v2` 事件的 seq 分配点。
//!
//! - `Hub` 持有全部块和全局 `seq`，都在同一把锁里（V2-5）。执行者（`executor.rs`）读完一块调
//!   `Hub::record_read`，每轮结束调 `Hub::round_end`（max_age 检查、策略的轮末钩子、心跳）。
//! - 事件交给 `EventSink::emit`，在锁里调用、`seq` 已经分配好。T5 在这里接 broadcast
//!   （序列化一次、`send`），`/v2` 的 snapshot 也在同一把锁里拍（`Hub::snapshot`）。
//! - 发布时机由每块的 `PublishPolicy` 决定（扩展点）：默认 `OnChange`（V2-17），
//!   T5 的信号块（V2-15，阈值 + 30 秒）和 live 块（V2-16，每轮结束）各实现一个。
//!   不管策略怎么说，revision 只在「数据或 stale 变了」时 +1（V2-11），stale 翻转总是立即发布（V2-12）。
//! - 旧接口从 `Hub::legacy` 取数据：stale 的块按读失败处理（V2-29）。
//! - 块有两份值：`raw` 是 ubus 原始回复（旧接口用），`data` 是 `/v2` 发布的值，
//!   形状和旧 `/state` 里对应的子对象相同（`BlockSpec::shape` 从原始回复算出来）。
//! - 派生块（`BlockSpec::derived`，信号块、live 块）不发 ubus，由旧采集把算好的结果交给 `Hub::record`。

use serde_json::Value;
use std::{
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::time::Instant;

/// V2-13：max_age 的下限。
pub const MIN_MAX_AGE: Duration = Duration::from_secs(15);
/// V2-21：读失败的块多久内重读。
pub const FAILURE_RETRY: Duration = Duration::from_secs(5);

/// 从原始回复算 `/v2` 的 `data`；第二个参数按块名取别的块的原始回复（`deps` 里列出的）。
pub type ShapeFn = fn(&Value, &dyn Fn(&str) -> Option<Value>) -> Value;

/// 一块读什么、多久读一次、怎么发布。
pub struct BlockSpec {
    pub name: &'static str,
    pub object: &'static str,
    pub method: &'static str,
    pub args: Value,
    /// 采集间隔；0 = 每轮（V2-21）。
    pub interval: Duration,
    pub policy: Box<dyn PublishPolicy>,
    /// 派生块：执行者不读它（没有 ubus 请求），数据由旧采集经 `Hub::record` 交进来。
    pub derived: bool,
    /// 原始回复 → `/v2` 的 data；None = 原样。
    pub shape: Option<ShapeFn>,
    /// `shape` 用到的别的块：它们读成功后，这块的 data 跟着重算。
    pub deps: &'static [&'static str],
}

impl BlockSpec {
    pub fn new(
        name: &'static str,
        object: &'static str,
        method: &'static str,
        interval: Duration,
    ) -> Self {
        Self {
            name,
            object,
            method,
            args: Value::Object(Default::default()),
            interval,
            policy: Box::new(OnChange),
            derived: false,
            shape: None,
            deps: &[],
        }
    }

    /// 派生块：每轮由旧采集交数据（采集间隔按「每轮」算）。
    pub fn derived(name: &'static str, policy: Box<dyn PublishPolicy>) -> Self {
        Self {
            policy,
            derived: true,
            ..Self::new(name, "", "", Duration::ZERO)
        }
    }

    /// ubus 调用带参数（默认 `{}`）。
    pub fn with_args(mut self, args: Value) -> Self {
        self.args = args;
        self
    }

    pub fn with_shape(mut self, shape: ShapeFn, deps: &'static [&'static str]) -> Self {
        self.shape = Some(shape);
        self.deps = deps;
        self
    }
}

/// 阶段 1 的块：电池、充电器（ubus，间隔沿用 `ubus_ttl` 的 5 秒），信号、live、短信（派生，每轮）。
/// `/v2` 的 data 和旧 `/state` 同形：battery = `battery` 对象，charger = `power` 对象
/// （旧 `/state` 没有 `power` 时是 `{}`），signal = `net` 对象，live = `{system, runtime, traffic}`。
pub fn phase1_blocks() -> Vec<BlockSpec> {
    vec![
        BlockSpec::new(
            "battery",
            "zwrt_bsp.battery",
            "list",
            Duration::from_secs(5),
        )
        .with_shape(crate::state::battery_v2, &["charger"]),
        BlockSpec::new(
            "charger",
            "zwrt_bsp.charger",
            "list",
            Duration::from_secs(5),
        )
        .with_shape(crate::state::charger_v2, &[]),
        // 插线时的 USB 用法（2026-10-07，manager docs/designs/usb-attach-mode.md）：Type-C 口的角色和插线状态、
        // 快充充电宝状态。插拔靠 BSP_TYPEC_EVENT / BSP_POWERBANK_EVENT 立即重读（ubus/listen.rs），这里只是 60 秒兜底。
        // data 是原厂回复原样：typec = {power_role, data_role, cc_attch_state}，powerbank = {state}。
        BlockSpec::new("typec", "zwrt_bsp.typec", "list", Duration::from_secs(60)),
        BlockSpec::new(
            "powerbank",
            "zwrt_bsp.powerbank",
            "get",
            Duration::from_secs(60),
        )
        .with_args(serde_json::json!({"property": "state"})),
        BlockSpec::derived("signal", Box::new(SignalPolicy)),
        BlockSpec::derived("live", Box::new(LivePolicy)),
        // V2-30：短信摘要，由旧采集按原来的短信节拍（容量 30 秒、列表 10 秒）交进来。
        BlockSpec::derived("sms", Box::new(OnChange)),
        // SIM 身份和状态（旧 `/state` 的 `sim` 对象），变了才发（2026-10-03）。
        BlockSpec::derived("sim", Box::new(OnChange)),
        // E4 T13（V2-34）：写操作的界面数据，由事务引擎交（每轮一次 + 每次状态变化）。
        BlockSpec::derived("op", Box::new(OnChange)),
        BlockSpec::derived("advice", Box::new(OnChange)),
        // V2-46（2026-10-05）：旧 /state 的读者迁到 /v2 用，data 和旧 /state 同名对象同形，变了才发。
        BlockSpec::derived("qos", Box::new(OnChange)),
        BlockSpec::derived("clients", Box::new(OnChange)),
        BlockSpec::derived("wlan", Box::new(OnChange)),
        BlockSpec::derived("nfc", Box::new(OnChange)),
        // 第三批 3-3：APN 模式、cid 1 的 profile、手动/自动两张表（密码不出设备）。
        BlockSpec::derived("apn", Box::new(OnChange)),
        BlockSpec::derived("dhcp", Box::new(OnChange)),
        // 第四批 4-0：路由设置（防火墙、UPnP、端口转发、过滤、ALG、带宽分配、域名过滤）和 Wi-Fi 设置
        // （密码不出设备），给新网页用；60 秒缓存，写过之后整个缓存作废。
        BlockSpec::derived("router", Box::new(OnChange)),
        BlockSpec::derived("wifi", Box::new(OnChange)),
        BlockSpec::derived("interfaces", Box::new(OnChange)),
        BlockSpec::derived("uci_device_info", Box::new(OnChange)),
        // V2-47（2026-10-05）：旧 /state 的 `sms.list`（两库第一页解密后的最新几条），触屏迁到 /v2 用，变了才发。
        BlockSpec::derived("sms_list", Box::new(OnChange)),
    ]
}

/// 策略的回答。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Publish {
    /// 这次不发（stale 翻转除外）。
    No,
    /// 数据和上次发布的不同才发，发了 revision +1。
    IfChanged,
}

/// 策略看到的东西（默认策略用不到；信号块、live 块用）。
pub struct PolicyInput<'a> {
    /// 上次发布的数据（从没发布过是 None）。
    pub published: Option<&'a Value>,
    /// 最新一次读成功的数据。
    pub current: Option<&'a Value>,
    /// 距上次发布多久（从没发布过是 None）。
    pub since_publish: Option<Duration>,
}

/// 发布策略（扩展点）。只管「什么时候考虑发」；revision 和 stale 的规则由 `Hub` 统一执行。
pub trait PublishPolicy: Send {
    /// 读成功之后。
    fn on_read(&mut self, _input: &PolicyInput<'_>) -> Publish {
        Publish::IfChanged
    }
    /// 每轮结束（V2-16 的 live 块在这里发）。
    fn on_round_end(&mut self, _input: &PolicyInput<'_>) -> Publish {
        Publish::No
    }
}

/// V2-17：数据有任何变化就发。
pub struct OnChange;
impl PublishPolicy for OnChange {}

/// V2-16：live 块只在每轮结束时发（变了才发、才涨 revision）。
pub struct LivePolicy;
impl PublishPolicy for LivePolicy {
    fn on_read(&mut self, _input: &PolicyInput<'_>) -> Publish {
        Publish::No
    }
    fn on_round_end(&mut self, _input: &PolicyInput<'_>) -> Publish {
        Publish::IfChanged
    }
}

/// V2-15：距上次发布满这么久，把小于阈值的变化也发出去。
pub const SIGNAL_FORCE: Duration = Duration::from_secs(30);
/// V2-15：和上次发布的值相差这么多 dB 就立即发。
pub const SIGNAL_THRESHOLD_DB: f64 = 1.0;
/// 信号块（= 旧 `/state` 的 `net`）里按 dB 阈值比较的字段。
pub const SIGNAL_DB_FIELDS: &[&str] = &[
    "nr_rsrp", "nr_rsrq", "nr_snr", "lte_rsrp", "lte_rsrq", "lte_snr",
];
/// 只在 30 秒强制发布时带出去、自己不触发发布的字段（跟着 RSRP 抖）。
pub const SIGNAL_QUIET_FIELDS: &[&str] = &["bars", "rssi", "nr_rssi", "lte_rssi"];
/// 载波聚合串：每个载波里 rsrp/rsrq/sinr 按阈值、rssi 不触发，其余（序号、PCI、频段、
/// 频点、带宽）和载波个数变了立即发。认不出的格式整串比较。
pub const SIGNAL_CA_FIELDS: &[&str] = &["nrca", "lteca", "ltecasig"];

/// 信号块的比较键：`ident` 变了立即发，`db` 任何一项相差 ≥ 1 dB 立即发。
#[derive(PartialEq, Debug)]
struct SignalKey {
    ident: Vec<String>,
    db: Vec<Option<f64>>,
}

fn db_value(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn signal_key(net: &Value) -> SignalKey {
    let mut key = SignalKey {
        ident: Vec::new(),
        db: Vec::new(),
    };
    let Some(obj) = net.as_object() else {
        key.ident.push(net.to_string());
        return key;
    };
    for (k, v) in obj {
        let k = k.as_str();
        if SIGNAL_DB_FIELDS.contains(&k) {
            key.db.push(db_value(Some(v)));
        } else if SIGNAL_QUIET_FIELDS.contains(&k) {
        } else if SIGNAL_CA_FIELDS.contains(&k) {
            ca_key(k, v.as_str().unwrap_or_default(), &mut key);
        } else {
            key.ident.push(format!("{k}={v}"));
        }
    }
    key
}

/// nrca/lteca：`;` 分隔的载波，每个 11 个数（idx,PCI,?,band,arfcn,bw,?,rsrp,rsrq,sinr,rssi）；
/// ltecasig：每个载波 4 个数（rsrp,rsrq,sinr,rssi）。
fn ca_key(field: &str, s: &str, key: &mut SignalKey) {
    let (n, db) = if field == "ltecasig" {
        (4, 0..3)
    } else {
        (11, 7..10)
    };
    let records: Vec<&str> = s.split(';').filter(|r| !r.trim().is_empty()).collect();
    key.ident.push(format!("{field}#{}", records.len()));
    for r in records {
        let nums: Vec<f64> = r.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        if nums.len() != n || r.split(',').count() != n {
            key.ident.push(format!("{field}:{r}"));
            continue;
        }
        for (i, x) in nums.iter().enumerate() {
            if db.contains(&i) {
                key.db.push(Some(*x));
            } else if i < n - 1 {
                key.ident.push(format!("{field}:{i}={x}"));
            }
        }
    }
}

/// V2-15 的阈值判断：比的是上次发布的值。
pub fn signal_crosses(published: &Value, current: &Value) -> bool {
    let (p, c) = (signal_key(published), signal_key(current));
    if p.ident != c.ident || p.db.len() != c.db.len() {
        return true;
    }
    p.db.iter().zip(&c.db).any(|(a, b)| match (a, b) {
        (Some(a), Some(b)) => (a - b).abs() >= SIGNAL_THRESHOLD_DB,
        (a, b) => a != b,
    })
}

/// V2-15：信号块。阈值（≥ 1 dB、小区/制式/频段变了）立即发；满 30 秒强制发（数据相同则不发）。
pub struct SignalPolicy;
impl PublishPolicy for SignalPolicy {
    fn on_read(&mut self, input: &PolicyInput<'_>) -> Publish {
        let (Some(p), Some(c)) = (input.published, input.current) else {
            return Publish::IfChanged;
        };
        if signal_crosses(p, c) || input.since_publish.is_none_or(|d| d >= SIGNAL_FORCE) {
            Publish::IfChanged
        } else {
            Publish::No
        }
    }
}

/// 一条 `block` 事件（V2-10）。`data` 是发布出去的值，从没读成功过是 null。
#[derive(Debug, Clone, PartialEq)]
pub struct BlockEvent {
    pub seq: u64,
    pub name: &'static str,
    pub revision: u64,
    pub observed_at: u64,
    pub stale: bool,
    pub data: Value,
}

/// 一条 `heartbeat` 事件：每块的 observed_at（从没读成功过是 0），执行者多久没前进（D12，V2-32）。
#[derive(Debug, Clone, PartialEq)]
pub struct Heartbeat {
    pub seq: u64,
    pub blocks: Vec<(&'static str, u64)>,
    pub exec_age_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Block(BlockEvent),
    Heartbeat(Heartbeat),
}

/// 事件出口。在 `Hub` 的锁里调用，`seq` 已分配；不要在这里阻塞或回调 `Hub`。
pub trait EventSink: Send + Sync {
    fn emit(&self, event: &Event);
}

/// 等某几块发布的通知（`Hub::watch`）：块名表 + 唤醒。
type Watchers =
    std::sync::Arc<Mutex<Vec<(&'static [&'static str], std::sync::Arc<tokio::sync::Notify>)>>>;

/// 套在真正的出口外面：块事件先叫醒等这块的 `Hub::watch`（R18-P1-04：引擎不必自己定时看块）。
struct Tee {
    inner: Box<dyn EventSink>,
    watchers: Watchers,
}

impl EventSink for Tee {
    fn emit(&self, event: &Event) {
        if let Event::Block(b) = event {
            for (names, n) in self
                .watchers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
            {
                if names.contains(&b.name) {
                    n.notify_one();
                }
            }
        }
        self.inner.emit(event);
    }
}

/// 丢掉全部事件（测试和 u60d `--once` 用）。
pub struct NoSink;
impl EventSink for NoSink {
    fn emit(&self, _event: &Event) {}
}

/// `/v2` 看到的一块（snapshot 用）。
#[derive(Debug, Clone, PartialEq)]
pub struct BlockView {
    pub name: &'static str,
    pub revision: u64,
    pub observed_at: u64,
    pub stale: bool,
    pub data: Value,
}

/// Internal successful read, independent of publication throttling and wall time.
/// Not part of the /v2 wire schema.
#[derive(Clone)]
pub struct Observation {
    pub name: &'static str,
    pub data: Value,
    pub observed: Option<luyaz_api::clock::BootTime>,
    pub generation: u64,
}

struct Block {
    spec: BlockSpec,
    revision: u64,
    observed_at: u64,
    stale: bool,
    /// 最近一次读成功的原始回复（旧接口用；stale 时保留）。
    raw: Option<Value>,
    /// 最近一次读成功算出的 `/v2` data（stale 时保留）。
    data: Option<Value>,
    /// 上次发布的值。
    published: Option<Value>,
    published_at: Option<Instant>,
    last_attempt: Option<Instant>,
    last_success: Option<Instant>,
    observed_boot: Option<luyaz_api::clock::BootTime>,
    generation: u64,
    last_failed: bool,
    /// `/control` 之后「下一轮立即读」（V2-21、V2-27）。
    immediate: bool,
}

impl Block {
    fn new(spec: BlockSpec) -> Self {
        Self {
            spec,
            revision: 0,
            observed_at: 0,
            stale: true,
            raw: None,
            data: None,
            published: None,
            published_at: None,
            last_attempt: None,
            last_success: None,
            observed_boot: None,
            generation: 0,
            last_failed: false,
            immediate: false,
        }
    }

    fn view(&self) -> BlockView {
        BlockView {
            name: self.spec.name,
            revision: self.revision,
            observed_at: self.observed_at,
            stale: self.stale,
            data: self.published.clone().unwrap_or(Value::Null),
        }
    }

    /// 采集间隔为 0 的块按当前采样间隔算（V2-13）。
    fn max_age(&self, sample_interval: Duration) -> Duration {
        let base = if self.spec.interval.is_zero() {
            sample_interval
        } else {
            self.spec.interval
        };
        (base * 3).max(MIN_MAX_AGE)
    }
}

struct Inner {
    seq: u64,
    blocks: Vec<Block>,
    /// 最近一次发心跳的时间（轮末或独立定时器）。
    last_heartbeat: Option<Instant>,
}

/// 采集调度的开关（`executor::Config` 里的同名字段）。
#[derive(Debug, Clone, Copy)]
pub struct Schedule {
    /// `LUYAZ_CACHE` 没设成 0。关掉时每块每轮都读。
    pub cache: bool,
    pub sample_interval: Duration,
}

/// 每次读成功一块后调用（锁外），参数是块名。u60d 的策略（Phase 4，`plugin/`）跟着块的节拍取样，不另开定时器。
pub type AfterRead = Box<dyn Fn(&Hub, &'static str) + Send + Sync>;

pub struct Hub {
    inner: Mutex<Inner>,
    sink: Box<dyn EventSink>,
    watchers: Watchers,
    after_read: std::sync::OnceLock<AfterRead>,
    /// Shared by background collection, compat reads and the capability HTTP report.
    firmware: crate::device_profile::collect::Collector,
}

fn wall_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl Hub {
    pub fn new(specs: Vec<BlockSpec>, sink: Box<dyn EventSink>) -> Self {
        let watchers = Watchers::default();
        let sink = Box::new(Tee {
            inner: sink,
            watchers: watchers.clone(),
        });
        Self {
            inner: Mutex::new(Inner {
                seq: 0,
                blocks: specs.into_iter().map(Block::new).collect(),
                last_heartbeat: None,
            }),
            sink,
            watchers,
            after_read: std::sync::OnceLock::new(),
            firmware: crate::device_profile::collect::Collector::new(),
        }
    }

    pub(crate) fn firmware(&self) -> &crate::device_profile::collect::Collector {
        &self.firmware
    }

    /// 这几块有发布（数据变了或 stale 翻转）时叫醒返回的 `Notify`；没人在等时留一次许可，不会漏。
    pub fn watch(&self, names: &'static [&'static str]) -> std::sync::Arc<tokio::sync::Notify> {
        let n = std::sync::Arc::new(tokio::sync::Notify::new());
        self.watchers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((names, n.clone()));
        n
    }

    /// 装上「读成功后」回调；只能装一次（再装返回 false）。
    pub fn set_after_read(&self, f: AfterRead) -> bool {
        self.after_read.set(f).is_ok()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn len(&self) -> usize {
        self.lock().blocks.len()
    }

    /// 第 `i` 块的请求（对象、方法、参数）。
    pub fn request(&self, i: usize) -> (&'static str, &'static str, Value) {
        let g = self.lock();
        let s = &g.blocks[i].spec;
        (s.object, s.method, s.args.clone())
    }

    /// 这一刻该不该读第 `i` 块（V2-21）。
    pub fn due(&self, i: usize, now: Instant, sched: Schedule) -> bool {
        let g = self.lock();
        let b = &g.blocks[i];
        if b.spec.derived {
            // 派生块没有 ubus 请求，`/control` 标了「立即读」也不读。
            return false;
        }
        let Some(last) = b.last_attempt else {
            return true;
        };
        if b.immediate || !sched.cache || b.spec.interval.is_zero() {
            return true;
        }
        let elapsed = now.saturating_duration_since(last);
        if b.last_failed {
            // 失败的块 5 秒内重读：下一轮若会超过 5 秒，这一轮就读。
            let retry = FAILURE_RETRY
                .saturating_sub(sched.sample_interval)
                .min(b.spec.interval);
            return elapsed >= retry;
        }
        elapsed >= b.spec.interval
    }

    /// 把块标成「下一轮立即读」。`names` 为 None（没有映射）时标全部块（R12）。
    pub fn mark_immediate(&self, names: Option<&[&str]>) {
        let mut g = self.lock();
        for b in &mut g.blocks {
            if names.is_none_or(|n| n.contains(&b.spec.name)) {
                b.immediate = true;
            }
        }
    }

    #[cfg(test)]
    pub fn is_immediate(&self, name: &str) -> bool {
        self.lock()
            .blocks
            .iter()
            .any(|b| b.spec.name == name && b.immediate)
    }

    /// 派生块（或任何块）按名字交数据，规则同 `record_read`。
    pub fn record(&self, name: &str, result: Result<Value, String>, now: Instant) {
        let i = self.lock().blocks.iter().position(|b| b.spec.name == name);
        if let Some(i) = i {
            self.record_read(i, result, now);
        }
    }

    /// 读完第 `i` 块。`Ok` 必须是 JSON 对象，否则算读失败（返回无效数据）。
    pub fn record_read(&self, i: usize, result: Result<Value, String>, now: Instant) {
        if let Some(name) = self.record_read_locked(i, result, now)
            && let Some(f) = self.after_read.get()
        {
            f(self, name);
        }
    }

    /// `record_read` 在锁里的部分；读成功时返回块名。
    fn record_read_locked(
        &self,
        i: usize,
        result: Result<Value, String>,
        now: Instant,
    ) -> Option<&'static str> {
        let mut g = self.lock();
        let g = &mut *g;
        let shaped = match &result {
            Ok(v) if v.is_object() => Some(Self::shape(&g.blocks, i, v)),
            _ => None,
        };
        let b = &mut g.blocks[i];
        b.last_attempt = Some(now);
        b.immediate = false;
        match (result, shaped) {
            (Ok(v), Some(shaped)) => {
                b.last_failed = false;
                b.last_success = Some(now);
                b.observed_at = wall_secs();
                b.observed_boot = luyaz_api::clock::BootTime::read();
                if b.stale || b.data.as_ref() != Some(&shaped) {
                    b.generation = b.generation.saturating_add(1);
                }
                // 原始回复没变，依赖它的块不用重算（电池的 sysfs 电压电流每次读都变，别多发一条）。
                let raw_changed = b.raw.as_ref() != Some(&v);
                b.raw = Some(v);
                b.data = Some(shaped);
                let was_stale = b.stale;
                b.stale = false;
                let input = PolicyInput {
                    published: b.published.as_ref(),
                    current: b.data.as_ref(),
                    since_publish: b.published_at.map(|t| now.saturating_duration_since(t)),
                };
                let decision = b.spec.policy.on_read(&input);
                Self::publish(&mut g.seq, b, &*self.sink, decision, was_stale, now);
                if raw_changed {
                    Self::reshape_dependents(g, i, &*self.sink, now);
                }
                Some(g.blocks[i].spec.name)
            }
            _ => {
                b.last_failed = true;
                b.observed_boot = None;
                b.generation = b.generation.saturating_add(1);
                if !b.stale {
                    // V2-12：读失败立即 stale；数据保留。
                    b.stale = true;
                    Self::publish(&mut g.seq, b, &*self.sink, Publish::No, true, now);
                }
                None
            }
        }
    }

    /// 第 `i` 块的原始回复 `raw` → `/v2` data。
    fn shape(blocks: &[Block], i: usize, raw: &Value) -> Value {
        match blocks[i].spec.shape {
            Some(f) => f(raw, &|name| {
                blocks
                    .iter()
                    .find(|b| b.spec.name == name)
                    .and_then(|b| b.raw.clone())
            }),
            None => raw.clone(),
        }
    }

    /// 第 `i` 块刚读成功：data 依赖它的块（电池依赖充电器）重算，变了按读成功发布。
    /// stale 或没读成功过的块不动（等它自己读成功）。
    fn reshape_dependents(g: &mut Inner, i: usize, sink: &dyn EventSink, now: Instant) {
        let name = g.blocks[i].spec.name;
        for j in 0..g.blocks.len() {
            let b = &g.blocks[j];
            if j == i || b.stale || !b.spec.deps.contains(&name) {
                continue;
            }
            let Some(raw) = &b.raw else { continue };
            let shaped = Self::shape(&g.blocks, j, raw);
            let b = &mut g.blocks[j];
            if b.data.as_ref() == Some(&shaped) {
                continue;
            }
            b.data = Some(shaped);
            let input = PolicyInput {
                published: b.published.as_ref(),
                current: b.data.as_ref(),
                since_publish: b.published_at.map(|t| now.saturating_duration_since(t)),
            };
            let decision = b.spec.policy.on_read(&input);
            Self::publish(&mut g.seq, b, sink, decision, false, now);
        }
    }

    /// 发布一块：`flipped` 表示 stale 刚翻转（必须发）。数据或健康变了才涨 revision、发事件。
    fn publish(
        seq: &mut u64,
        b: &mut Block,
        sink: &dyn EventSink,
        decision: Publish,
        flipped: bool,
        now: Instant,
    ) {
        let data_changed = b.published != b.data;
        let data_goes_out = match decision {
            Publish::IfChanged => data_changed,
            // stale 翻转成 false 时，新值跟着发出去（不能发一条 stale=false 却带着旧值）。
            Publish::No => flipped && !b.stale && data_changed,
        };
        if !flipped && !data_goes_out {
            return;
        }
        if data_goes_out {
            b.published = b.data.clone();
        }
        b.published_at = Some(now);
        b.revision += 1;
        *seq += 1;
        let v = b.view();
        sink.emit(&Event::Block(BlockEvent {
            seq: *seq,
            name: v.name,
            revision: v.revision,
            observed_at: v.observed_at,
            stale: v.stale,
            data: v.data,
        }));
    }

    /// V2-13：超过 max_age 的块置 stale 并发布（轮末，以及一轮拖得很长时独立定时器每 5 秒一次）。
    pub fn expire(&self, now: Instant, sample_interval: Duration) {
        let mut g = self.lock();
        Self::expire_locked(&mut g, &*self.sink, now, sample_interval);
    }

    fn expire_locked(g: &mut Inner, sink: &dyn EventSink, now: Instant, sample_interval: Duration) {
        for b in &mut g.blocks {
            if !b.stale
                && let Some(ok) = b.last_success
                && now.saturating_duration_since(ok) > b.max_age(sample_interval)
            {
                b.stale = true;
                Self::publish(&mut g.seq, b, sink, Publish::No, true, now);
            }
        }
    }

    /// 每轮结束（V2-13、V2-16、V2-22）：超过 max_age 的块置 stale；各块策略的轮末钩子；最后发心跳。
    pub fn round_end(&self, now: Instant, sample_interval: Duration) -> Heartbeat {
        let mut g = self.lock();
        let g = &mut *g;
        Self::expire_locked(g, &*self.sink, now, sample_interval);
        for b in &mut g.blocks {
            if b.stale {
                continue;
            }
            let input = PolicyInput {
                published: b.published.as_ref(),
                current: b.data.as_ref(),
                since_publish: b.published_at.map(|t| now.saturating_duration_since(t)),
            };
            let decision = b.spec.policy.on_round_end(&input);
            if decision != Publish::No {
                Self::publish(&mut g.seq, b, &*self.sink, decision, false, now);
            }
        }
        // 执行者刚跑完一轮：它在前进。
        Self::emit_heartbeat(g, &*self.sink, now, 0)
    }

    fn emit_heartbeat(
        g: &mut Inner,
        sink: &dyn EventSink,
        now: Instant,
        exec_age_ms: u64,
    ) -> Heartbeat {
        g.seq += 1;
        g.last_heartbeat = Some(now);
        let hb = Heartbeat {
            seq: g.seq,
            blocks: g
                .blocks
                .iter()
                .map(|b| (b.spec.name, b.observed_at))
                .collect(),
            exec_age_ms,
        };
        sink.emit(&Event::Heartbeat(hb.clone()));
        hb
    }

    /// 独立定时器的心跳（V2-22、V2-32）：距上一条心跳满 `quiet` 才发，带执行者多久没前进。
    /// 执行者被长任务占着、或者卡住时，心跳照样有，订阅方看 `exec_age_ms` 判断它是不是卡了。
    pub fn heartbeat_if_quiet(
        &self,
        now: Instant,
        exec_age_ms: u64,
        quiet: Duration,
    ) -> Option<Heartbeat> {
        let mut g = self.lock();
        if g.last_heartbeat
            .is_some_and(|t| now.saturating_duration_since(t) < quiet)
        {
            return None;
        }
        Some(Self::emit_heartbeat(&mut g, &*self.sink, now, exec_age_ms))
    }

    /// 切点 `seq` 和全部块（V2-4）。
    #[cfg(test)]
    pub fn snapshot(&self) -> (u64, Vec<BlockView>) {
        self.snapshot_with(|| ()).1
    }

    /// V2-5：在分配 `seq` 的同一把锁里先执行 `subscribe`（订阅 broadcast），再拍快照。
    /// 锁里不能再调 `Hub` 的其他方法。
    pub fn snapshot_with<R>(&self, subscribe: impl FnOnce() -> R) -> (R, (u64, Vec<BlockView>)) {
        let g = self.lock();
        let r = subscribe();
        (r, (g.seq, g.blocks.iter().map(Block::view).collect()))
    }

    /// 一块现在发布出去的样子（Phase 4 的策略也用）。
    pub fn view(&self, name: &str) -> Option<BlockView> {
        self.lock()
            .blocks
            .iter()
            .find(|b| b.spec.name == name)
            .map(Block::view)
    }

    /// Capture related latest reads under one lock. Missing/stale data remains absent.
    /// BOOTTIME includes suspend; external wall timestamps and /v2 revision stay unchanged.
    pub fn observations(&self, names: &[&str]) -> Vec<Observation> {
        let g = self.lock();
        g.blocks
            .iter()
            .filter(|b| names.contains(&b.spec.name) && !b.stale)
            .filter_map(|b| {
                Some(Observation {
                    name: b.spec.name,
                    data: b.data.clone()?,
                    observed: b.observed_boot.clone(),
                    generation: b.generation,
                })
            })
            .collect()
    }

    /// 旧接口用（V2-29）：stale（含从没读成功）按读失败处理，不给保留的旧值。
    pub fn legacy(&self, name: &str) -> Result<Value, String> {
        let g = self.lock();
        match g.blocks.iter().find(|b| b.spec.name == name) {
            Some(b) if !b.stale => b.raw.clone().ok_or_else(|| "no data".into()),
            Some(_) => Err(format!("block {name} is stale")),
            None => Err(format!("no block {name}")),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    /// 记下所有事件。
    #[derive(Default, Clone)]
    pub(crate) struct Recorder(pub Arc<Mutex<Vec<Event>>>);
    impl EventSink for Recorder {
        fn emit(&self, event: &Event) {
            self.0.lock().unwrap().push(event.clone());
        }
    }
    impl Recorder {
        pub(crate) fn events(&self) -> Vec<Event> {
            self.0.lock().unwrap().clone()
        }
        pub(crate) fn blocks(&self, name: &str) -> Vec<BlockEvent> {
            self.events()
                .into_iter()
                .filter_map(|e| match e {
                    Event::Block(b) if b.name == name => Some(b),
                    _ => None,
                })
                .collect()
        }
    }

    fn make_hub(interval_s: u64) -> (Hub, Recorder) {
        let rec = Recorder::default();
        let hub = Hub::new(
            vec![BlockSpec::new(
                "battery",
                "zwrt_bsp.battery",
                "list",
                Duration::from_secs(interval_s),
            )],
            Box::new(rec.clone()),
        );
        (hub, rec)
    }

    const SAMPLE: Duration = Duration::from_secs(1);

    #[test]
    fn block_never_read_is_stale_null() {
        let (hub, rec) = make_hub(5);
        let v = hub.view("battery").unwrap();
        assert_eq!(
            (v.stale, v.data.clone(), v.observed_at, v.revision),
            (true, Value::Null, 0, 0)
        );
        assert!(hub.legacy("battery").is_err());
        // 失败不翻转（本来就 stale），不发事件。
        hub.record_read(0, Err("x".into()), Instant::now());
        assert!(rec.blocks("battery").is_empty());
        // 心跳里 observed_at 是 0。
        let hb = hub.round_end(Instant::now(), SAMPLE);
        assert_eq!(hb.blocks, vec![("battery", 0)]);
        // 第一次读成功：revision 1（数据变化和 stale 翻转合计只 +1）。
        hub.record_read(0, Ok(json!({"battery_capacity":80})), Instant::now());
        let ev = rec.blocks("battery");
        assert_eq!(ev.len(), 1);
        assert_eq!((ev[0].revision, ev[0].stale), (1, false));
        assert!(ev[0].observed_at > 0);
    }

    /// R18-P1-04：`watch` 只在它等的块发布时醒，同值重读不醒；没人在等时留一次许可。
    #[tokio::test]
    async fn watch_wakes_on_publish_only() {
        let (hub, _rec) = make_hub(5);
        let n = hub.watch(&["battery"]);
        let other = hub.watch(&["sms"]);
        let now = Instant::now();
        let woke = |n: &Arc<tokio::sync::Notify>| {
            let n = n.clone();
            async move {
                tokio::time::timeout(Duration::from_millis(20), n.notified())
                    .await
                    .is_ok()
            }
        };
        hub.record_read(0, Ok(json!({"a":1})), now);
        assert!(woke(&n).await, "first publish: permit kept");
        assert!(!woke(&other).await, "another block: not woken");
        hub.record_read(0, Ok(json!({"a":1})), now);
        assert!(!woke(&n).await, "same data: no publish, not woken");
        hub.record_read(0, Err("x".into()), now);
        assert!(woke(&n).await, "stale flip: woken");
    }

    #[test]
    fn block_revision_unchanged_when_data_same() {
        let (hub, rec) = make_hub(5);
        let now = Instant::now();
        hub.record_read(0, Ok(json!({"a":1})), now);
        hub.record_read(0, Ok(json!({"a":1})), now);
        hub.record_read(0, Ok(json!({"a":1})), now);
        assert_eq!(hub.view("battery").unwrap().revision, 1);
        assert_eq!(rec.blocks("battery").len(), 1);
    }

    #[test]
    fn block_revision_bumps_on_data_change() {
        let (hub, rec) = make_hub(5);
        let now = Instant::now();
        hub.record_read(0, Ok(json!({"a":1})), now);
        hub.record_read(0, Ok(json!({"a":2})), now);
        let ev = rec.blocks("battery");
        assert_eq!(ev.iter().map(|e| e.revision).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(ev[1].data, json!({"a":2}));
        // seq 是全局计数，每条事件 +1。
        assert_eq!(ev[1].seq, ev[0].seq + 1);
    }

    #[test]
    fn block_any_change_publishes() {
        let (hub, rec) = make_hub(5);
        let now = Instant::now();
        hub.record_read(0, Ok(json!({"a":1,"b":{"c":[1,2]}})), now);
        hub.record_read(0, Ok(json!({"a":1,"b":{"c":[1,3]}})), now);
        hub.record_read(0, Ok(json!({"a":1,"b":{"c":[1,3]},"d":null})), now);
        assert_eq!(rec.blocks("battery").len(), 3);
        // 返回的不是对象：读失败（无效数据）。
        hub.record_read(0, Ok(json!([1])), now);
        assert!(rec.blocks("battery")[3].stale);
    }

    #[test]
    fn block_stale_flip_publishes_twice_same_data() {
        let (hub, rec) = make_hub(5);
        let now = Instant::now();
        hub.record_read(0, Ok(json!({"a":1})), now);
        let first_seen = rec.blocks("battery")[0].observed_at;
        hub.record_read(0, Err("timeout".into()), now);
        // 连续失败不再发。
        hub.record_read(0, Err("timeout".into()), now);
        hub.record_read(0, Ok(json!({"a":1})), now);
        let ev = rec.blocks("battery");
        assert_eq!(ev.len(), 3, "{ev:?}");
        assert_eq!(
            ev.iter().map(|e| (e.revision, e.stale)).collect::<Vec<_>>(),
            [(1, false), (2, true), (3, false)]
        );
        // stale 时 data 是保留的旧值，observed_at 不更新。
        assert_eq!(ev[1].data, json!({"a":1}));
        assert_eq!(ev[1].observed_at, first_seen);
        assert_eq!(ev[2].data, json!({"a":1}));
    }

    #[test]
    fn block_starved_to_max_age_goes_stale() {
        // 间隔 5 秒 → max_age = max(15, 15) = 15 秒。
        let (hub, rec) = make_hub(5);
        let t0 = Instant::now();
        hub.record_read(0, Ok(json!({"a":1})), t0);
        hub.round_end(t0 + Duration::from_secs(15), SAMPLE);
        assert!(!hub.view("battery").unwrap().stale);
        hub.round_end(t0 + Duration::from_millis(15_001), SAMPLE);
        let ev = rec.blocks("battery");
        assert_eq!(ev.len(), 2);
        assert!(ev[1].stale);
        assert_eq!(ev[1].data, json!({"a":1}));
        assert!(hub.legacy("battery").is_err());
        // 已经 stale，再过 max_age 不再翻转。
        hub.round_end(t0 + Duration::from_secs(100), SAMPLE);
        assert_eq!(rec.blocks("battery").len(), 2);

        // 间隔 60 秒 → max_age 180 秒；间隔 0 → 按采样间隔 ×3，最少 15 秒。
        let (hub, rec) = make_hub(60);
        hub.record_read(0, Ok(json!({"a":1})), t0);
        hub.round_end(t0 + Duration::from_secs(179), SAMPLE);
        assert_eq!(rec.blocks("battery").len(), 1);
        hub.round_end(t0 + Duration::from_secs(181), SAMPLE);
        assert_eq!(rec.blocks("battery").len(), 2);
        let (hub, rec) = make_hub(0);
        hub.record_read(0, Ok(json!({"a":1})), t0);
        hub.round_end(t0 + Duration::from_secs(16), Duration::from_secs(6));
        assert_eq!(
            rec.blocks("battery").len(),
            1,
            "6 s × 3 = 18 s，16 s 不置 stale"
        );
        hub.round_end(t0 + Duration::from_secs(16), Duration::from_secs(1));
        assert_eq!(rec.blocks("battery").len(), 2, "1 s × 3 < 15 s，按 15 s 算");
    }

    #[test]
    fn block_stale_clears_after_recovery_read() {
        let (hub, rec) = make_hub(5);
        let t0 = Instant::now();
        hub.record_read(0, Ok(json!({"a":1})), t0);
        hub.round_end(t0 + Duration::from_secs(20), SAMPLE);
        hub.record_read(0, Ok(json!({"a":1})), t0 + Duration::from_secs(21));
        let ev = rec.blocks("battery");
        assert_eq!(
            ev.iter().map(|e| (e.revision, e.stale)).collect::<Vec<_>>(),
            [(1, false), (2, true), (3, false)]
        );
        assert_eq!(hub.legacy("battery"), Ok(json!({"a":1})));
        // 恢复时值也变了：一条事件，带新值。
        hub.record_read(0, Err("x".into()), t0 + Duration::from_secs(22));
        hub.record_read(0, Ok(json!({"a":2})), t0 + Duration::from_secs(23));
        let ev = rec.blocks("battery");
        assert_eq!(ev.len(), 5);
        assert_eq!((ev[4].revision, ev[4].stale), (5, false));
        assert_eq!(ev[4].data, json!({"a":2}));
    }

    #[test]
    fn heartbeat_takes_seq_after_blocks() {
        let (hub, rec) = make_hub(5);
        let now = Instant::now();
        hub.record_read(0, Ok(json!({"a":1})), now);
        let hb = hub.round_end(now, SAMPLE);
        assert_eq!(hb.seq, 2);
        let seqs: Vec<u64> = rec
            .events()
            .iter()
            .map(|e| match e {
                Event::Block(b) => b.seq,
                Event::Heartbeat(h) => h.seq,
            })
            .collect();
        assert_eq!(seqs, [1, 2]);
        assert_eq!(hub.snapshot().0, 2);
    }

    fn signal_hub() -> (Hub, Recorder) {
        let rec = Recorder::default();
        let hub = Hub::new(
            vec![BlockSpec::derived("signal", Box::new(SignalPolicy))],
            Box::new(rec.clone()),
        );
        (hub, rec)
    }

    fn net(rsrp: i64, snr: f64, pci: i64) -> Value {
        json!({"type":"SA","band":"n78","nr_band":"78","nr_rsrp":rsrp,"nr_rsrq":-11,
               "nr_snr":format!("{snr:.1}"),"nr_pci":pci,"nr_cell_id":7,"bars":4,
               "operator":"X","lteca":"","nrca":"","ltecasig":""})
    }

    fn at(t0: Instant, s: u64) -> Instant {
        t0 + Duration::from_secs(s)
    }

    #[test]
    fn signal_drift_0_3db_publishes_once_at_30s() {
        let (hub, rec) = signal_hub();
        let t0 = Instant::now();
        hub.record("signal", Ok(net(-100, 10.0, 1)), t0);
        // 每秒漂移 0.3 dB（+0.3、+0.3、−0.3、−0.3……），和上次发布的值累计始终 < 1 dB。
        let drift = [0.3, 0.6, 0.3, 0.0];
        for t in 1..60 {
            let snr = 10.0 + drift[(t as usize - 1) % 4];
            hub.record("signal", Ok(net(-100, snr, 1)), at(t0, t));
        }
        let ev = rec.blocks("signal");
        assert_eq!(ev.len(), 2, "{ev:?}");
        assert_eq!(ev[1].revision, 2);
        assert_eq!(ev[1].data["nr_snr"], "10.6", "第 30 秒的值（0.6 dB）送出去");
        // 下一次强制发布从第 30 秒算：第 60 秒。
        hub.record("signal", Ok(net(-100, 10.3, 1)), at(t0, 60));
        assert_eq!(rec.blocks("signal").len(), 3);
    }

    #[test]
    fn signal_unchanged_not_published() {
        let (hub, rec) = signal_hub();
        let t0 = Instant::now();
        for t in 0..120 {
            hub.record("signal", Ok(net(-100, 10.0, 1)), at(t0, t));
            hub.round_end(at(t0, t), Duration::from_secs(1));
        }
        let ev = rec.blocks("signal");
        assert_eq!(ev.len(), 1, "满 30 秒时数据完全相同：不发");
        assert_eq!(hub.view("signal").unwrap().revision, 1);
        // 只动「不触发」的字段（bars），30 秒内不发，满 30 秒发。
        let mut v = net(-100, 10.0, 1);
        v["bars"] = json!(3);
        hub.record("signal", Ok(v.clone()), at(t0, 120));
        assert_eq!(rec.blocks("signal").len(), 2, "距上次发布早已超过 30 秒");
        v["bars"] = json!(2);
        hub.record("signal", Ok(v), at(t0, 125));
        assert_eq!(rec.blocks("signal").len(), 2);
    }

    #[test]
    fn signal_1db_change_publishes_immediately() {
        let (hub, rec) = signal_hub();
        let t0 = Instant::now();
        hub.record("signal", Ok(net(-100, 10.0, 1)), t0);
        hub.record("signal", Ok(net(-101, 10.0, 1)), at(t0, 1));
        assert_eq!(rec.blocks("signal").len(), 2, "RSRP 差 1 dB");
        hub.record("signal", Ok(net(-101, 10.9, 1)), at(t0, 2));
        assert_eq!(rec.blocks("signal").len(), 2, "SINR 差 0.9 dB 不发");
        hub.record("signal", Ok(net(-101, 9.0, 1)), at(t0, 3));
        assert_eq!(
            rec.blocks("signal").len(),
            3,
            "SINR 差 1 dB（比上次发布的 10.0）"
        );
        // 载波聚合里某个载波的 RSRP 差 1 dB。
        let ca = |rsrp: i64| {
            let mut v = net(-101, 9.0, 1);
            v["nrca"] = json!(format!("1,671,0,78,627264,100,0,{rsrp},-10,12,-70"));
            v
        };
        hub.record("signal", Ok(ca(-95)), at(t0, 4));
        assert_eq!(rec.blocks("signal").len(), 4, "新增载波");
        hub.record("signal", Ok(ca(-95)), at(t0, 5));
        assert_eq!(rec.blocks("signal").len(), 4);
        hub.record("signal", Ok(ca(-96)), at(t0, 6));
        let ev = rec.blocks("signal");
        assert_eq!(ev.len(), 5);
        assert_eq!(
            ev.iter().map(|e| e.revision).collect::<Vec<_>>(),
            [1, 2, 3, 4, 5]
        );
        // 读失败：stale 立即发，不受阈值限制。
        hub.record("signal", Err("x".into()), at(t0, 7));
        assert!(rec.blocks("signal")[5].stale);
    }

    #[test]
    fn signal_cell_change_publishes_immediately() {
        let (hub, rec) = signal_hub();
        let t0 = Instant::now();
        hub.record("signal", Ok(net(-100, 10.0, 1)), t0);
        hub.record("signal", Ok(net(-100, 10.2, 2)), at(t0, 1));
        assert_eq!(rec.blocks("signal").len(), 2, "PCI 变了");
        let mut v = net(-100, 10.2, 2);
        v["type"] = json!("NSA");
        hub.record("signal", Ok(v.clone()), at(t0, 2));
        assert_eq!(rec.blocks("signal").len(), 3, "制式变了");
        v["band"] = json!("n41");
        hub.record("signal", Ok(v), at(t0, 3));
        let ev = rec.blocks("signal");
        assert_eq!(ev.len(), 4, "频段变了");
        assert_eq!(ev[3].data["band"], "n41");
    }

    fn live_hub() -> (Hub, Recorder) {
        let rec = Recorder::default();
        let hub = Hub::new(
            vec![
                BlockSpec::new("battery", "b", "list", Duration::from_secs(5)),
                BlockSpec::derived("live", Box::new(LivePolicy)),
            ],
            Box::new(rec.clone()),
        );
        (hub, rec)
    }

    #[test]
    fn live_block_published_every_round_end() {
        let (hub, rec) = live_hub();
        let t0 = Instant::now();
        let sample = Duration::from_secs(1);
        for t in 0..5u64 {
            let before = rec.blocks("live").len();
            hub.record("live", Ok(json!({"system":{"uptime":100 + t}})), at(t0, t));
            if t > 0 {
                // 读到的时候不发，轮末才发（第一次读成功是 stale 翻转，按 V2-12 立即发）。
                assert_eq!(rec.blocks("live").len(), before);
            }
            // 同一轮里别的块变了，不影响 live。
            hub.record_read(0, Ok(json!({ "n": t })), at(t0, t));
            hub.round_end(at(t0, t), sample);
            assert_eq!(rec.blocks("live").len(), t as usize + 1, "每轮一次");
        }
        let ev = rec.blocks("live");
        assert_eq!(ev.len(), 5);
        assert_eq!(ev[4].revision, 5);
        // 第 2 轮起：电池、live、心跳，live 紧挨在心跳前面。
        let seqs: Vec<(bool, u64)> = rec
            .events()
            .iter()
            .map(|e| match e {
                Event::Block(b) => (b.name == "live", b.seq),
                Event::Heartbeat(h) => (false, h.seq),
            })
            .collect();
        assert_eq!(seqs.len(), 15);
        for r in 1..5 {
            assert_eq!(seqs[r * 3 + 1], (true, r as u64 * 3 + 2));
        }
        // 数据没变的一轮：不发、不涨 revision，心跳照发。
        hub.record("live", Ok(json!({"system":{"uptime":104}})), at(t0, 5));
        let hb = hub.round_end(at(t0, 5), sample);
        assert_eq!(rec.blocks("live").len(), 5);
        assert_eq!(hb.seq, 16);
        // live 块 stale 由 max_age 决定（每轮的块，最少 15 秒）。
        hub.round_end(at(t0, 21), sample);
        assert!(rec.blocks("live")[5].stale);
        // 派生块不会被执行者读，/control 标「立即读」也不读。
        hub.mark_immediate(None);
        assert!(hub.due(0, at(t0, 21), sched()));
        assert!(!hub.due(1, at(t0, 21), sched()));
        let (fresh, _) = live_hub();
        assert!(!fresh.due(1, t0, sched()), "从没读过也不算到期");
    }

    fn sched() -> Schedule {
        Schedule {
            cache: true,
            sample_interval: Duration::from_secs(1),
        }
    }

    /// V2-48：插线用的两块读原厂对象，powerbank 带参数，60 秒兜底。
    #[test]
    fn usb_blocks_read_vendor_objects() {
        let hub = Hub::new(phase1_blocks(), Box::new(NoSink));
        let find = |name: &str| {
            (0..hub.len())
                .find(|&i| hub.lock().blocks[i].spec.name == name)
                .map(|i| (hub.request(i), hub.lock().blocks[i].spec.interval))
                .unwrap()
        };
        let ((o, m, a), every) = find("typec");
        assert_eq!(
            (o, m, a, every),
            (
                "zwrt_bsp.typec",
                "list",
                serde_json::json!({}),
                Duration::from_secs(60)
            )
        );
        let ((o, m, a), _) = find("powerbank");
        assert_eq!(
            (o, m, a),
            (
                "zwrt_bsp.powerbank",
                "get",
                serde_json::json!({"property": "state"})
            )
        );
    }

    #[test]
    fn signal_threshold_fields_classified() {
        // 不认识的 CA 格式整串比较；ltecasig 每个载波 4 个数。
        let base = json!({"ltecasig":"-90.0,-10.0,12.0,-63.0;"});
        let mut v = base.clone();
        v["ltecasig"] = json!("-90.5,-10.0,12.0,-50.0;");
        assert!(!signal_crosses(&base, &v), "0.5 dB、rssi 不触发");
        v["ltecasig"] = json!("-91.0,-10.0,12.0,-63.0;");
        assert!(signal_crosses(&base, &v));
        let odd = json!({"lteca":"1,2,3"});
        assert!(signal_crosses(&odd, &json!({"lteca":"1,2,4"})));
        assert!(!signal_crosses(&odd, &odd.clone()));
    }

    /// Phase 4：读成功才回调、在锁外（回调里能再用 Hub）、每次一回；只能装一次。
    #[test]
    fn after_read_runs_outside_the_lock_on_success_only() {
        let hub = Hub::new(phase1_blocks(), Box::new(NoSink));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        assert!(hub.set_after_read(Box::new(move |hub, name| {
            assert!(hub.inner.try_lock().is_ok(), "called with the hub locked");
            s.lock().unwrap().push(name);
        })));
        assert!(!hub.set_after_read(Box::new(|_, _| {})));
        let now = Instant::now();
        hub.record("battery", Ok(json!({"battery_capacity": 50})), now);
        hub.record("battery", Err("timeout".into()), now);
        hub.record("battery", Ok(json!("not an object")), now);
        hub.record("charger", Ok(json!({"charger_connect": 1})), now);
        assert_eq!(*seen.lock().unwrap(), vec!["battery", "charger"]);
    }
}

#[cfg(test)]
mod finding_observation_tests {
    use super::*;
    #[test]
    fn wall_timestamp_steps_do_not_change_internal_elapsed_evidence() {
        let hub = Hub::new(
            vec![BlockSpec::derived("sim", Box::new(OnChange))],
            Box::new(NoSink),
        );
        hub.record(
            "sim",
            Ok(serde_json::json!({"iccid":"fixture"})),
            Instant::now(),
        );
        let a = hub.observations(&["sim"]).pop().unwrap();
        assert!(a.observed.is_some());
        for wall in [
            0,
            1_900_000_000,
            1_900_000_000 + 28800,
            1_900_000_000 - 28800,
        ] {
            hub.lock().blocks[0].observed_at = wall;
            let b = hub.observations(&["sim"]).pop().unwrap();
            assert_eq!(b.observed, a.observed);
            assert_eq!(b.generation, a.generation);
            assert_eq!(
                hub.view("sim").unwrap().observed_at,
                wall,
                "wire timestamp remains display data"
            );
        }
    }
}
