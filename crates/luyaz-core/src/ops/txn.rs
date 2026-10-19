//! E4 写操作层的事务状态机（manager `docs/designs/write-op-layer.md`「状态表」，D13、D31、D32、D35）。
//!
//! 纯逻辑：不碰设备、不读时钟、不落盘。驱动（`engine.rs`）把写之后才采到的新鲜读数和时钟
//! （BOOTTIME 毫秒，跨核心重启连续）喂进来，按返回的 [`Next`] 去写设备、落盘。
//!
//! 进行中：accepted → applying → verifying →（没通）rolling_back → 终态。终态都放锁：
//!
//! | 终态 | 原因 |
//! |---|---|
//! | confirmed | verified / user_keep |
//! | unverified | no_rollback（自动退回关着，到点没通）/ apn_no_data（APN 在不该有数据时写入） |
//! | rolled_back | timeout / user_revert / reboot_loop |
//! | not_applied | ignored（读回从没变成过目标值） |
//! | rollback_failed | rollback_timeout |
//! | cancelled | superseded / preempted / manual_change / takeover / sim_changed / other_change |
//!
//! 确认（T4，D25、D33、D34）：配置读回 = 目标 + 已注册；「应当有数据」时还要数据通：
//! 已连接、有 IPv4（没有 IPv4 时有 IPv6，D41）、在蜂窝接口上一次 DNS 探测成功。APN 另外要求「写之后的新连接」。
//! 退回也按同一条规则确认。

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// 谁发起的写。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Screen,
    Web,
    Legacy,
    Guard,
    Scenario,
    Scheduler,
    Auto,
    /// MCP（Phase 8）提议的写。自己不算用户来源；人在触屏或网页确认过时 `approved_by` 是 screen/web（D23）。
    Mcp,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Screen => "screen",
            Self::Web => "web",
            Self::Legacy => "legacy",
            Self::Guard => "guard",
            Self::Scenario => "scenario",
            Self::Scheduler => "scheduler",
            Self::Auto => "auto",
            Self::Mcp => "mcp",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "screen" => Self::Screen,
            "web" => Self::Web,
            "legacy" => Self::Legacy,
            "guard" => Self::Guard,
            "scenario" => Self::Scenario,
            "scheduler" => Self::Scheduler,
            "auto" => Self::Auto,
            "mcp" => Self::Mcp,
            _ => return None,
        })
    }

    /// D16：screen、web（含 undo）、legacy 算用户写；guard 和其他自动来源不算。
    pub fn is_user(self) -> bool {
        matches!(self, Self::Screen | Self::Web | Self::Legacy)
    }

    /// 能不能覆盖同一项进行中的事务：用户写可以；guard 只能覆盖同一项（D15，调用方已比过项）。
    pub fn may_override(self) -> bool {
        self.is_user() || self == Self::Guard
    }

    /// 能当「确认者」的来源：人在这两处点了确认（D23）。
    pub fn can_confirm(self) -> bool {
        matches!(self, Self::Screen | Self::Web)
    }
}

/// 一次写的身份（T22，D23）：谁提的（`source`，线上字段名不变）和谁批准的（`approved_by`）。
/// 两者只在人确认过别人的提议时不同，那时 `approved_by` 只能是 screen 或 web。
/// 是不是用户写、能不能插队、算不算用户值、要不要自动退回，都按 `approved_by` 判断；
/// 所有输出只在两者不同时才带 `approved_by`，旧调用方看到的字节不变。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Who {
    pub source: Source,
    pub approved_by: Source,
}

impl From<Source> for Who {
    fn from(source: Source) -> Self {
        Self {
            source,
            approved_by: source,
        }
    }
}

impl Who {
    /// `approved_by` 和 `source` 不同时只能是 screen/web；明写 mcp 只在来源也是 mcp 时可以（Agent 自己批准，
    /// 第二期 D7：放不放行由 `ops::agent` 的白名单按动作定，取代第一期 T1 的一律拒绝）。
    pub fn new(source: Source, approved_by: Option<Source>) -> Result<Self, String> {
        if approved_by == Some(Source::Mcp) && source != Source::Mcp {
            return Err("approved_by mcp needs source mcp".into());
        }
        let approved_by = approved_by.unwrap_or(source);
        if approved_by != source && !approved_by.can_confirm() {
            return Err(format!(
                "approved_by must be screen or web (or the same as source), not {}",
                approved_by.as_str()
            ));
        }
        Ok(Self {
            source,
            approved_by,
        })
    }

    /// 和来源不同的确认者（相同为 None：不输出）。
    pub fn confirmed_by(self) -> Option<Source> {
        (self.approved_by != self.source).then_some(self.approved_by)
    }

    /// 给一行 JSON 加 `approved_by`（只在和来源不同时）。
    pub fn tag(self, v: &mut Value) {
        if let (Some(a), Some(m)) = (self.confirmed_by(), v.as_object_mut()) {
            m.insert("approved_by".into(), json!(a));
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Accepted,
    Applying,
    Verifying,
    RollingBack,
    Confirmed,
    Unverified,
    RolledBack,
    NotApplied,
    RollbackFailed,
    Cancelled,
}

impl Phase {
    pub fn is_final(self) -> bool {
        !matches!(
            self,
            Self::Accepted | Self::Applying | Self::Verifying | Self::RollingBack
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Verified,
    UserKeep,
    NoRollback,
    Timeout,
    UserRevert,
    RebootLoop,
    Ignored,
    RollbackTimeout,
    Superseded,
    Preempted,
    ManualChange,
    Takeover,
    SimChanged,
    ApnNoData,
    /// D40：确认或退回中，用户又改了别的影响上网的设置（或 agent 发了 `op.interrupt`），不再自动退回。
    OtherChange,
    /// 不需要确认的动作（状态文案表有这一行；描述表里还没有这类动作）。
    NoVerify,
}

/// 确认规则（D34）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confirm {
    /// 制式、锁频、锁小区、卡槽：读回 = 目标 + 已注册；应当有数据时还要数据通。
    #[default]
    Registered,
    /// APN：应当有数据时要写之后的新连接 + 数据通；不应当有数据时没法证明，记 unverified/apn_no_data。
    Apn,
    /// Wi-Fi 设置（第四批 4-0b）：读回 = 目标 + AP 已按 uci 起来/关掉（读数的 `registered` 在这里是这个意思，
    /// 见 `ops::wifi_dev`）；不看数据、不探测。
    Wifi,
    /// CHILL 档位、出口（第二期 D2、D10，见 `ops::chill`）：读回 = 目标 + mihomo 在跑；再经本机入口 7894 真去取
    /// generate_204，连续两次通才算过。不看蜂窝数据。
    Chill,
}

/// 一条数据连接的身份：地址 + 从什么时候起（BOOTTIME 毫秒，按 netifd 的 uptime 秒数推算，
/// 前后两次读数会差不到 1 秒）。地址是 IPv4；没有 IPv4 时用 IPv6（D41，只有 IPv6 的连接）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conn {
    pub ipv4: String,
    /// 只在没有 IPv4 时填（有 IPv4 时身份照旧只看 IPv4）。旧版落的盘里没有这个字段。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ipv6: String,
    pub up_since_ms: Option<u64>,
}

/// netifd 的 uptime 只到秒，推算出的起点前后会抖不到 1 秒。
const CONN_JITTER_MS: u64 = 2_000;

impl Conn {
    /// 连接身份用的地址：有 IPv4 用 IPv4，否则 IPv6；都没有为空。
    pub fn addr(&self) -> &str {
        if self.ipv4.is_empty() {
            &self.ipv6
        } else {
            &self.ipv4
        }
    }

    /// 只有 IPv6（确认时探测走 IPv6 DNS）。
    pub fn v6_only(&self) -> bool {
        self.ipv4.is_empty() && !self.ipv6.is_empty()
    }

    fn same(&self, other: &Conn) -> bool {
        self.addr() == other.addr()
            && match (self.up_since_ms, other.up_since_ms) {
                (Some(a), Some(b)) => a.abs_diff(b) <= CONN_JITTER_MS,
                _ => true,
            }
    }
}

/// 数据通路的读数。读不到的部分为 None：这一拍不拿来判断数据。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DataPath {
    /// 「应当有数据」：数据开关开，且（不在漫游或漫游开关开）。没读全为 None。
    pub expected: Option<bool>,
    /// `get_wwaniface` 的 connect_status 是已连接。
    pub connected: bool,
    pub conn: Conn,
    /// 蜂窝数据接口（`get_wwaniface` 的 ipv4_dev_name，空时用 netifd 的 l3_device；只有 IPv6 时用
    /// ipv6_dev_name，空时用 `zte_wan6` 的 l3_device）。
    pub iface: String,
    /// 运营商 DNS（IPv4 的在前；只有 IPv6 时是 IPv6 的）。
    pub dns: Vec<String>,
}

/// 一次 DNS 探测要用的：绑定哪个接口、问哪几个 DNS、走 IPv4 还是 IPv6（D41：连接只有 IPv6 时
/// 向 IPv6 DNS 发 AAAA 查询）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeTarget {
    pub iface: String,
    pub dns: Vec<String>,
    pub v6: bool,
    /// CHILL 的真路径探测（[`Confirm::Chill`]）：不问 DNS，经 mihomo 取一次 generate_204。
    pub chill: bool,
}

/// CHILL 要连续通几次才算过（D10）。
pub const CHILL_PASSES: u32 = 2;

/// D25：一轮最多探测失败 3 次，两次之间至少隔 5 秒。一轮都失败后，隔 30 秒再来一轮；换了连接
/// （重新拨号）马上重新算。切换时 PDP 不断开的话连接身份不变，只靠时间重来，免得数据晚通就永远确认不了。
/// 120 秒里最多约 4 轮 12 个查询（约 1.5 KB），通了就停。
pub const PROBE_TRIES: u32 = 3;
pub const PROBE_GAP_MS: u64 = 5_000;
pub const PROBE_ROUND_GAP_MS: u64 = 30_000;

/// 完整 SIM 身份（D32）：完整 ICCID + 卡槽。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimId {
    pub iccid: String,
    pub slot: i64,
}

impl SimId {
    /// ICCID 空或卡槽没报：没卡，或者基带重新注册时字段临时空着。
    fn blank(&self) -> bool {
        self.iccid.is_empty() || self.slot == 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimCheck {
    Same,
    Changed,
    /// 这次读到的身份是空的（写之前有卡）：这个读数不拿来判断，等下一拍。
    Unknown,
}

/// D32：写之前的 SIM 身份和这次读到的比。只有「读到一个不同的非空身份」才算换卡
/// （写之前没卡、现在有卡也算）；读到空的不算换卡，只是这一拍不判断。
pub fn sim_check(captured: &SimId, now: &SimId) -> SimCheck {
    match (captured.blank(), now.blank()) {
        (true, true) => SimCheck::Same,
        (false, true) => SimCheck::Unknown,
        _ if captured == now => SimCheck::Same,
        _ => SimCheck::Changed,
    }
}

/// 一次新鲜读数：配置读回（读不到为 None）、是否已注册、当前 SIM、数据通路。
#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    /// 经 `Spec::normalize` 同一套规范化的读回；空的、读不懂的为 None。
    pub value: Option<Value>,
    pub registered: bool,
    pub sim: SimId,
    /// 读不到为 None（应当有数据时就不能确认，等下一拍）。
    pub data: Option<DataPath>,
    /// 驱动按 [`Txn::wants_probe`] 做了探测时填结果；没做为 None。
    pub probe: Option<bool>,
}

/// 读回对不对得上目标：目标是对象时只比目标里有的键（APN 切模式的目标只有 `mode`，自动表挑哪条事先
/// 不知道）；其余整个值相等。锁频、锁小区的目标带全部键，等于整个值相等。
pub fn matches(goal: &Value, v: &Value) -> bool {
    match (goal, v) {
        (Value::Object(g), Value::Object(m)) if !g.is_empty() => {
            g.iter().all(|(k, gv)| m.get(k) == Some(gv))
        }
        _ => goal == v,
    }
}

/// 读回是空的（旧版读数器会把空字符串当读回）：不算「第三个值」。
pub fn blank(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Object(m) => m.is_empty(),
        Value::Array(a) => a.is_empty(),
        _ => false,
    }
}

/// 数据这一关的判断。
#[derive(Debug, PartialEq, Eq)]
enum DataJudge {
    Ok,
    /// 不应当有数据，APN 没法证明。
    ApnNoData,
    NotYet,
}

/// 落盘的「意图」：先落意图、再动手、再落结果（D31）。重启时遇到意图没清，不重做，按读回判断。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    Apply,
    Rollback,
}

/// 写调用的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applied {
    Done,
    /// 调用报错：读回仍是旧值就是没写进去。
    Failed,
    /// 调用超时（D28）：原厂可能还在做，只按读回判断。
    Unknown,
}

/// 驱动下一步要做的事。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Next {
    Wait,
    /// 到了终态。
    Done,
    /// 把这个值写回去（退回）；意图已经记在事务里，写之前先落盘。
    Rollback(Value),
}

/// 新事务的参数。
pub struct NewTxn {
    pub op_id: String,
    pub action: String,
    pub item: String,
    pub source: Source,
    /// 谁批准的（D23）；和 `source` 相同 = 没有另外的确认者。
    pub approved_by: Source,
    pub undo: bool,
    pub target: Value,
    /// 写之前的读回。
    pub old: Value,
    /// 退回目标：没覆盖别的事务时等于 old；覆盖写继承被覆盖事务的退回目标（D35）。
    pub rollback_to: Value,
    pub sim: SimId,
    /// 写之前的数据连接（APN 的「新连接」拿它比）。
    pub conn: Option<Conn>,
    pub confirm: Confirm,
    pub rollback_enabled: bool,
    pub deadline_ms: u64,
    pub boot_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Txn {
    pub op_id: String,
    pub action: String,
    pub item: String,
    pub source: Source,
    /// 谁批准的（T22，D23）：只在和 `source` 不同时有、才落盘；旧版落的盘里没有 = 和 `source` 相同。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<Source>,
    pub undo: bool,
    /// 目标值、旧值、退回目标、读回：规范化的 JSON 值（3-0）；旧版落的盘里是字符串，照样读得回。
    pub target: Value,
    pub old: Value,
    pub rollback_to: Value,
    pub phase: Phase,
    pub reason: Option<Reason>,
    /// 退回的起因（timeout / user_revert / reboot_loop），退回成功时就是终态原因。
    pub rollback_reason: Option<Reason>,
    /// 读回曾经对上过目标值（没对上过 = 写没生效；对上过又回旧值 = 带外改回）。
    pub ever_matched: bool,
    /// 写之后拿到过至少一次读数（一次都没有时到点不能判 not_applied）。
    pub read_ok: bool,
    /// 最近一次配置读回（流水账的「读回」）。
    #[serde(default)]
    pub readback: Option<Value>,
    /// 写调用本身报错了（读回仍是旧值就马上判 not_applied，不用等到点）。
    pub apply_failed: bool,
    pub intent: Option<Intent>,
    pub sim: SimId,
    #[serde(default)]
    pub confirm: Confirm,
    /// 「新连接」的参照（D34）：这条之后建立的、或 IP 变了的连接才算新的。写的时候是写之前的连接和
    /// 事务开始的时刻；发了退回以后换成退回那一刻看到的连接和时刻；整机重启后时刻归 0（都算新的）。
    #[serde(default)]
    pub ref_conn: Option<Conn>,
    #[serde(default)]
    pub ref_ms: u64,
    /// 最近一次读到的连接（发退回时拿来当参照）。
    #[serde(default)]
    pub last_conn: Option<Conn>,
    /// DNS 探测（D25）：在哪条连接上、失败了几次、上一次什么时候。
    #[serde(default)]
    pub probe_conn: Option<Conn>,
    #[serde(default)]
    pub probe_fails: u32,
    #[serde(default)]
    pub probe_last_ms: Option<u64>,
    /// 数据通过了（探测成功、或不应当有数据）。只给 status 看。
    #[serde(default)]
    pub data_ok: bool,
    /// CHILL 的真路径探测连续通了几次（[`Confirm::Chill`]）。
    #[serde(default)]
    pub chill_passes: u32,
    /// 最近一次读数里已注册（三行进度的第二行，T13）。
    #[serde(default)]
    pub registered: bool,
    pub rollback_enabled: bool,
    pub deadline_ms: u64,
    pub boot_id: String,
    /// 写之后经历的整机重启次数（D13）。
    pub boots: u32,
    /// 以前几次开机里已经等过的时长（D13 累计）。
    pub elapsed_prev_ms: u64,
    /// 当前这段等待（verifying 或 rolling_back）在这次开机里从哪一刻算起。
    pub wait_start_ms: u64,
    /// 最近一次喂读数或落盘时的时钟（整机重启后据此算上一次开机等了多久）。
    pub seen_ms: u64,
    pub created_ms: u64,
    /// 确认通用了多久（开始确认到数据通，跨重启累计；只在 confirmed/verified 时有）。
    /// 记进流水账，上机后按动作统计定 D24 时限（E4 T11）。
    #[serde(default)]
    pub took_ms: Option<u64>,
    /// 每次阶段变化 +1：驱动丢掉阶段变化之前发出去的读数。
    pub generation: u64,
}

impl Txn {
    pub fn new(n: NewTxn, now: u64) -> Self {
        Self {
            op_id: n.op_id,
            action: n.action,
            item: n.item,
            source: n.source,
            approved_by: (n.approved_by != n.source).then_some(n.approved_by),
            undo: n.undo,
            target: n.target,
            old: n.old,
            rollback_to: n.rollback_to,
            phase: Phase::Accepted,
            reason: None,
            rollback_reason: None,
            ever_matched: false,
            read_ok: false,
            readback: None,
            apply_failed: false,
            intent: None,
            sim: n.sim,
            confirm: n.confirm,
            ref_conn: n.conn.clone(),
            ref_ms: now,
            last_conn: n.conn,
            probe_conn: None,
            probe_fails: 0,
            probe_last_ms: None,
            data_ok: false,
            chill_passes: 0,
            registered: false,
            rollback_enabled: n.rollback_enabled,
            deadline_ms: n.deadline_ms,
            boot_id: n.boot_id,
            boots: 0,
            elapsed_prev_ms: 0,
            took_ms: None,
            wait_start_ms: now,
            seen_ms: now,
            created_ms: now,
            generation: 0,
        }
    }

    /// 这个事务的身份（`approved_by` 没有 = 和来源相同）。
    pub fn who(&self) -> Who {
        Who {
            source: self.source,
            approved_by: self.approved_by.unwrap_or(self.source),
        }
    }

    fn step(&mut self, phase: Phase) {
        self.phase = phase;
        self.generation += 1;
    }

    fn finish(&mut self, phase: Phase, reason: Reason) -> Next {
        self.step(phase);
        self.reason = Some(reason);
        self.intent = None;
        Next::Done
    }

    fn waiting(&self) -> bool {
        matches!(self.phase, Phase::Verifying | Phase::RollingBack)
    }

    /// 动手写之前：落盘意图。
    pub fn begin_apply(&mut self) {
        self.step(Phase::Applying);
        self.intent = Some(Intent::Apply);
    }

    /// 写调用回来了（成功、报错或超时），开始等确认。
    pub fn applied(&mut self, outcome: Applied, now: u64) {
        if self.phase != Phase::Applying {
            return;
        }
        self.intent = None;
        self.apply_failed = outcome == Applied::Failed;
        self.wait_start_ms = now;
        self.seen_ms = now;
        self.step(Phase::Verifying);
    }

    fn start_rollback(&mut self, why: Reason) -> Next {
        self.step(Phase::RollingBack);
        self.rollback_reason = Some(why);
        self.intent = Some(Intent::Rollback);
        // 等发退回也有时限（O7）：发之前一直核对不了 SIM 时不能永远占着锁。`seen_ms` 是最近一拍的时钟。
        self.wait_start_ms = self.seen_ms;
        Next::Rollback(self.rollback_to.clone())
    }

    /// 退回的写调用回来了：退回只发一次，之后按读回判断。
    pub fn rollback_sent(&mut self, now: u64) {
        if self.phase != Phase::RollingBack {
            return;
        }
        self.intent = None;
        self.wait_start_ms = now;
        self.seen_ms = now;
        // 退回也走确认：新连接从这一刻算，探测重新计数。
        self.ref_conn = self.last_conn.clone();
        self.ref_ms = now;
        self.reset_probe();
        self.generation += 1;
    }

    fn reset_probe(&mut self) {
        self.probe_conn = None;
        self.probe_fails = 0;
        self.probe_last_ms = None;
        self.data_ok = false;
        self.chill_passes = 0;
    }

    /// 这条连接是不是写（或退回）之后的新连接。
    fn new_conn(&self, c: &Conn) -> bool {
        if self.ref_ms == 0 {
            return true;
        }
        if let Some(r) = &self.ref_conn
            && r.addr() != c.addr()
        {
            return true;
        }
        c.up_since_ms.is_some_and(|t| t > self.ref_ms)
    }

    /// 这一拍正在等的值：确认时是目标值，退回时是退回目标。
    fn goal(&self) -> &Value {
        if self.phase == Phase::RollingBack {
            &self.rollback_to
        } else {
            &self.target
        }
    }

    /// 除了探测，数据这一关的其他条件。`Err` 里是还要不要探测（条件都齐了只差探测）。
    fn data_gate(&self, r: &Reading) -> Result<DataJudge, Option<ProbeTarget>> {
        if self.confirm == Confirm::Wifi {
            return Ok(DataJudge::Ok);
        }
        if self.confirm == Confirm::Chill {
            return Err(Some(ProbeTarget {
                iface: "chill".into(),
                dns: vec![],
                v6: false,
                chill: true,
            }));
        }
        let Some(d) = &r.data else {
            return Err(None);
        };
        match d.expected {
            None => return Err(None),
            Some(false) => {
                return Ok(match (self.confirm, self.phase) {
                    (Confirm::Apn, Phase::Verifying) => DataJudge::ApnNoData,
                    _ => DataJudge::Ok,
                });
            }
            Some(true) => {}
        }
        if !d.connected || d.conn.addr().is_empty() {
            return Err(None);
        }
        if self.confirm == Confirm::Apn && !self.new_conn(&d.conn) {
            return Err(None);
        }
        Err(Some(ProbeTarget {
            iface: d.iface.clone(),
            dns: d.dns.clone(),
            v6: d.conn.v6_only(),
            chill: false,
        }))
    }

    /// 这个读数要不要先做一次 DNS 探测（驱动在锁外做，结果填进 `Reading::probe` 再喂进来）。
    /// 只在别的条件都齐了才探测：读回 = 正在等的值、已注册、SIM 没变、应当有数据、已连接有 IPv4 或 IPv6
    /// （APN 还要新连接）。一轮失败满 3 次，同一条连接上要隔 30 秒才来下一轮；两次之间至少隔 5 秒。
    pub fn wants_probe(&self, r: &Reading, now: u64) -> Option<ProbeTarget> {
        if !self.waiting() || self.intent.is_some() || !r.registered {
            return None;
        }
        if !r.value.as_ref().is_some_and(|v| matches(self.goal(), v)) {
            return None;
        }
        if sim_check(&self.sim, &r.sim) != SimCheck::Same {
            return None;
        }
        let target = match self.data_gate(r) {
            Err(Some(t)) => t,
            _ => return None,
        };
        if self.confirm == Confirm::Chill {
            // 两次之间至少隔 5 秒；失败了照样隔 5 秒再来（到点没通就退回）
            if self
                .probe_last_ms
                .is_some_and(|t| now.saturating_sub(t) < PROBE_GAP_MS)
            {
                return None;
            }
            return Some(target);
        }
        let conn = &r.data.as_ref()?.conn;
        let round_over = self.probe_conn.as_ref().is_some_and(|c| c.same(conn))
            && self.probe_fails >= PROBE_TRIES;
        let gap = if round_over {
            PROBE_ROUND_GAP_MS
        } else {
            PROBE_GAP_MS
        };
        if self
            .probe_last_ms
            .is_some_and(|t| now.saturating_sub(t) < gap)
        {
            return None;
        }
        Some(target)
    }

    /// 记下探测结果；换了连接、或上一轮已经用完（这是新的一轮），就重新计数。
    fn note_probe(&mut self, r: &Reading, now: u64) {
        if self.confirm == Confirm::Chill {
            if let Some(ok) = r.probe {
                self.probe_last_ms = Some(now);
                if ok {
                    self.chill_passes += 1;
                } else {
                    self.chill_passes = 0;
                    self.probe_fails += 1;
                }
            }
            return;
        }
        let (Some(ok), Some(d)) = (r.probe, &r.data) else {
            return;
        };
        if !self.probe_conn.as_ref().is_some_and(|c| c.same(&d.conn))
            || self.probe_fails >= PROBE_TRIES
        {
            self.probe_conn = Some(d.conn.clone());
            self.probe_fails = 0;
        }
        self.probe_last_ms = Some(now);
        if !ok {
            self.probe_fails += 1;
        }
    }

    /// 数据这一关过没过（读回已经对上、已注册时才问）。
    fn data_judge(&mut self, r: &Reading) -> DataJudge {
        let j = match self.data_gate(r) {
            Ok(j) => j,
            Err(Some(_)) if self.confirm == Confirm::Chill => {
                if self.chill_passes >= CHILL_PASSES {
                    DataJudge::Ok
                } else {
                    DataJudge::NotYet
                }
            }
            Err(Some(_)) if r.probe == Some(true) => DataJudge::Ok,
            Err(_) => DataJudge::NotYet,
        };
        self.data_ok = j != DataJudge::NotYet;
        j
    }

    /// 到点没确认：自动退回开着就退，关着就以 unverified/no_rollback 结束（D30）。
    fn expire(&mut self, why: Reason) -> Next {
        if self.rollback_enabled {
            self.start_rollback(why)
        } else {
            self.finish(Phase::Unverified, Reason::NoRollback)
        }
    }

    /// 喂一次写之后才采到的读数。
    pub fn on_reading(&mut self, r: &Reading, now: u64) -> Next {
        if self.phase.is_final() {
            return Next::Done;
        }
        if !self.waiting() || self.intent.is_some() {
            return Next::Wait;
        }
        self.seen_ms = now;
        if let Some(d) = &r.data
            && !d.conn.addr().is_empty()
        {
            self.last_conn = Some(d.conn.clone());
        }
        // D32：SIM 变了就放弃，不往别的卡上写旧卡的值；身份临时读空的这一拍只看时限。
        match sim_check(&self.sim, &r.sim) {
            SimCheck::Changed => return self.finish(Phase::Cancelled, Reason::SimChanged),
            SimCheck::Unknown => return self.on_tick(now),
            SimCheck::Same => {}
        }
        self.registered = r.registered;
        self.note_probe(r, now);
        if let Some(v) = r.value.as_ref() {
            self.read_ok = true;
            self.readback = Some(v.clone());
            if self.phase == Phase::Verifying {
                if matches(&self.target, v) {
                    // 读回对上就记下（数据通不通另算）：对上过、数据一直不通，到点按没通处理，不算 not_applied。
                    self.ever_matched = true;
                    if r.registered {
                        match self.data_judge(r) {
                            DataJudge::Ok => {
                                self.took_ms = Some(
                                    self.elapsed_prev_ms + now.saturating_sub(self.wait_start_ms),
                                );
                                return self.finish(Phase::Confirmed, Reason::Verified);
                            }
                            DataJudge::ApnNoData => {
                                return self.finish(Phase::Unverified, Reason::ApnNoData);
                            }
                            DataJudge::NotYet => {}
                        }
                    }
                } else if *v == self.old {
                    // R3：对上过又回到旧值是带外改回；从没对上过、写又报了错，就是没写进去。
                    if self.ever_matched {
                        return self.finish(Phase::Cancelled, Reason::ManualChange);
                    }
                    if self.apply_failed {
                        return self.finish(Phase::NotApplied, Reason::Ignored);
                    }
                } else if !blank(v) {
                    // R3：既不是旧值也不是目标值的第三个值。
                    return self.finish(Phase::Cancelled, Reason::ManualChange);
                }
            } else if *v == self.rollback_to {
                // CHILL：值回去了就算退回完成（mihomo 可能正被闲置暂停停着，原值本来也未必测得通）
                if self.confirm == Confirm::Chill
                    || (r.registered && self.data_judge(r) != DataJudge::NotYet)
                {
                    let why = self.rollback_reason.unwrap_or(Reason::Timeout);
                    return self.finish(Phase::RolledBack, why);
                }
            } else if !matches(&self.target, v) && !blank(v) {
                return self.finish(Phase::Cancelled, Reason::ManualChange);
            }
        }
        self.on_tick(now)
    }

    /// 这一拍没拿到读数（读失败）：只看时限。
    pub fn on_tick(&mut self, now: u64) -> Next {
        if self.phase.is_final() {
            return Next::Done;
        }
        if self.phase == Phase::RollingBack && self.intent == Some(Intent::Rollback) {
            // 退回还没发出去（发之前读不到设备、核对不了 SIM）：到点就放弃，不永远等（O7）
            self.seen_ms = self.seen_ms.max(now);
            if now.saturating_sub(self.wait_start_ms) >= self.deadline_ms {
                return self.finish(Phase::RollbackFailed, Reason::RollbackTimeout);
            }
            return Next::Wait;
        }
        if !self.waiting() || self.intent.is_some() {
            return Next::Wait;
        }
        self.seen_ms = self.seen_ms.max(now);
        if now.saturating_sub(self.wait_start_ms) < self.deadline_ms {
            return Next::Wait;
        }
        if self.phase == Phase::RollingBack {
            return self.finish(Phase::RollbackFailed, Reason::RollbackTimeout);
        }
        if self.read_ok && !self.ever_matched {
            return self.finish(Phase::NotApplied, Reason::Ignored);
        }
        self.expire(Reason::Timeout)
    }

    /// 用户点「立即退回」：自动退回关着也照做（这是用户的写）；没有可退回的旧值时拒绝。
    pub fn revert(&mut self) -> Result<Next, &'static str> {
        match self.phase {
            Phase::Verifying if self.intent.is_none() => {
                if blank(&self.rollback_to) {
                    return Err("no previous setting to revert to");
                }
                Ok(self.start_rollback(Reason::UserRevert))
            }
            Phase::RollingBack => Err("already rolling back"),
            p if p.is_final() => Err("change already finished"),
            _ => Err("change is still being applied"),
        }
    }

    /// 用户点「保留现状」：取消退回，算用户确认。
    pub fn keep(&mut self) -> Result<(), &'static str> {
        match self.phase {
            Phase::Verifying if self.intent.is_none() => {
                self.finish(Phase::Confirmed, Reason::UserKeep);
                Ok(())
            }
            Phase::RollingBack => Err("already rolling back"),
            p if p.is_final() => Err("change already finished"),
            _ => Err("change is still being applied"),
        }
    }

    /// 被打断：同一项的覆盖写（superseded）、安全类写（preempted）、触屏接管（takeover）、
    /// 用户的其他影响上网的写（other_change，D40）。
    pub fn cancel(&mut self, why: Reason) {
        if !self.phase.is_final() {
            self.finish(Phase::Cancelled, why);
        }
    }

    /// 核心启动时接着跑落盘的事务。`boot_id` 变了 = 整机重启（D13）：
    /// 第 2 次开机仍未确认、或几次开机累计等待已超过时限，马上退回（reboot_loop）；否则时限从现在重新计。
    /// 写的意图没清的不重做（D31）：applying 当作已写、按读回判断。退回的意图没清的留着（O7）：驱动发之前
    /// 先读一次，读回已经是退回目标就不再写（发过了），否则照发——崩在发退回之前也不会丢掉这次退回。
    pub fn resume(&mut self, boot_id: &str, now: u64) -> Next {
        if self.phase.is_final() {
            return Next::Done;
        }
        let rebooted = boot_id != self.boot_id;
        if rebooted {
            if self.waiting() && self.intent.is_none() {
                self.elapsed_prev_ms += self.seen_ms.saturating_sub(self.wait_start_ms);
            }
            self.boots += 1;
            self.boot_id = boot_id.to_owned();
            // 时钟归零了：之前记的时刻都作废，开机后的连接都算新的。
            self.ref_ms = 0;
            self.reset_probe();
        }
        match (self.phase, self.intent) {
            (Phase::Accepted, _) => {
                // 还没落意图就停了：设备没被动过。
                return self.finish(Phase::NotApplied, Reason::Ignored);
            }
            (Phase::Applying, _) | (Phase::Verifying, Some(Intent::Apply)) => {
                self.intent = None;
                self.phase = Phase::Verifying;
                self.wait_start_ms = now;
            }
            (Phase::RollingBack, Some(_)) => {
                // 意图留着：驱动核对读回后决定发不发
                self.wait_start_ms = now;
            }
            _ if rebooted => self.wait_start_ms = now,
            _ => {}
        }
        self.seen_ms = now;
        self.generation += 1;
        if rebooted
            && self.phase == Phase::Verifying
            && (self.boots >= 2 || self.elapsed_prev_ms >= self.deadline_ms)
        {
            return self.expire(Reason::RebootLoop);
        }
        Next::Wait
    }

    /// 在等确认或等退回，并且没有在途的写：倒计时在走。
    pub fn counting(&self) -> bool {
        self.waiting() && self.intent.is_none()
    }

    /// 离到点还有多久（不在等时为 None）。
    pub fn remaining_ms(&self, now: u64) -> Option<u64> {
        self.waiting().then(|| {
            self.deadline_ms
                .saturating_sub(now.saturating_sub(self.wait_start_ms))
        })
    }

    /// 给客户端看的状态（`op.status`、提交和 busy 的回复里）。
    pub fn status(&self, now: u64) -> Value {
        let remaining = self.remaining_ms(now).map_or(Value::Null, |v| json!(v));
        let mut v = json!({
            "op_id": self.op_id,
            "action": self.action,
            "item": self.item,
            "source": self.source,
            "undo": self.undo,
            "phase": self.phase,
            "reason": self.reason,
            "rollback_reason": self.rollback_reason,
            "target": self.target,
            "old": self.old,
            "rollback_to": self.rollback_to,
            "rollback_enabled": self.rollback_enabled,
            "ever_matched": self.ever_matched,
            "data_ok": self.data_ok,
            "age_ms": now.saturating_sub(self.created_ms),
            "remaining_ms": remaining,
        });
        self.who().tag(&mut v);
        v
    }
}

#[cfg(test)]
mod tests;
