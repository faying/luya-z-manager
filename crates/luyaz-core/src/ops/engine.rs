//! 事务引擎（write-op-layer.md 方案 B）：一把锁、覆盖与插队、确认驱动、落盘续跑、旧请求队列。
//!
//! - 同一时刻最多一个进行中的事务（含等确认的那段时间）；别的写回 busy（说清正在做什么、谁、多久）。
//! - 能插队的：同一项的用户写和 guard（覆盖，旧事务记 superseded，新事务继承它的退回目标，D35）；
//!   安全类写（关数据/漫游，旧事务记 preempted，D14）；对进行中事务的「立即退回」「保留现状」。
//! - 每一步对设备的读写都是执行者里的短任务（`Device` 经 `state::ubus`）；锁只在做决定时拿，
//!   从不跨 await 持有。
//! - 每次动设备之前先落意图、之后落结果（`pending.json`，D31）；核心重启后接着确认，写不重做；
//!   退回按读回幂等补发（O7：读回还不是退回目标就再发一次）。
//! - 没有 source 的旧请求：锁被占或执行者队列满时先回和今天一样的成功，排进旧请求队列，
//!   锁空出来再按原来的 `/control` 处理执行一次（每项只留最新、120 秒过期、安全类插队时清空）。

use super::{
    pending::{Last, Store},
    record::{self, Record},
    spec::{self, Spec},
    txn::{self, NewTxn, Phase, ProbeTarget, Reading, Reason, Source, Txn, Who},
    ui,
};
use crate::project::screen::ScreenOp;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex, MutexGuard, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// `/v2` 的 `op` 块的出口（V2-34）：在引擎的锁里调，交给 `Hub::record`。
pub type Observer = Arc<dyn Fn(Value) + Send + Sync>;

/// 引擎对设备的全部读写和时钟。真机是 `UbusDevice`，测试是假基带。
pub trait Device: Send + Sync + 'static {
    /// 一次新鲜读数（不用缓存）：这一项的配置读回、是否已注册、完整 SIM 身份、数据通路。
    fn read(&self, spec: &'static Spec) -> impl Future<Output = Result<Reading, String>> + Send;
    /// 一次 DNS 探测（D25、D33）：绑定到蜂窝接口，问运营商 DNS，有回答就算通。
    fn probe(&self, target: &ProbeTarget) -> impl Future<Output = Result<(), String>> + Send;
    /// 提交时按设备现状定目标值：占位换成真值（`band.reset {}` → 默认的三组频段）、APN 的指纹和检查；
    /// `Ok(None)` = 这次不算事务（改的不是正在用的 APN），照原来的 `/control` 做。别的原样回。
    fn resolve(
        &self,
        _spec: &'static Spec,
        target: Value,
        _params: &Value,
    ) -> impl Future<Output = Result<Option<Value>, String>> + Send {
        async move { Ok(Some(target)) }
    }
    /// 把这一项写成 `value`（规范化的目标值）。`params` 是这次写的原始 `/control` 参数（只在新写时有，
    /// 不落盘，比如 APN 密码只在这里出现）；退回时是 `Spec::to_params(退回目标)`。
    fn write(
        &self,
        spec: &'static Spec,
        value: &Value,
        params: &Value,
    ) -> impl Future<Output = Result<Value, WriteError>> + Send;
    /// BOOTTIME 毫秒：核心重启不归零、休眠时也走。所有时限都按它算，tokio 的计时只当节拍。
    fn now_ms(&self) -> u64;
    fn boot_id(&self) -> String;
}

/// 写调用失败。`timed_out` = 我们这边超时了，原厂那边做没做不知道（D28）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteError {
    pub message: String,
    pub timed_out: bool,
}

/// 执行一个排队的旧请求（走原来的 `/control` 处理）；返回 false = 执行者队列满，过一会再试。
pub type LegacyRunner = Arc<dyn Fn(String, Value) -> BoxFuture<bool> + Send + Sync>;

#[derive(Clone, Debug)]
pub struct Config {
    /// 到点没通自动退回（D30：所有写入口收进核心并验收之前默认关）。
    pub rollback: bool,
    /// 确认时多久读一次。
    pub poll: Duration,
    /// 旧请求排队多久就丢掉。
    pub legacy_ttl_ms: u64,
}

impl Config {
    pub fn from_env() -> Self {
        let ms = |name: &str, default: u64| {
            std::env::var(name)
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|v| *v > 0)
                .unwrap_or(default)
        };
        Self {
            rollback: std::env::var("LUYAZ_ROLLBACK").is_ok_and(|v| v == "1"),
            poll: Duration::from_millis(ms("LUYAZ_OP_POLL_MS", 2_000)),
            legacy_ttl_ms: ms("LUYAZ_LEGACY_TTL_MS", 120_000),
        }
    }
}

/// 一次新写。
pub struct Request {
    pub spec: &'static Spec,
    /// 规范化的目标值（`Spec::target`）。
    pub target: Value,
    /// 原始 `/control` 参数：只交给写调用，不落盘。
    pub params: Value,
    pub source: Source,
    /// 谁批准的（T22，D23）：人确认了别人的提议时是 screen/web，否则和 `source` 相同。
    /// 插队、自动退回、用户值都按它判断。
    pub approved_by: Source,
    pub op_id: Option<String>,
    pub undo: bool,
    /// 覆盖「带不带自动退回」（None = 按全局开关和批准者）。Agent 的 CHILL 写切前就确定不通时为
    /// `Some(false)`（第二期 D10：本来就不通的不退）。
    pub rollback: Option<bool>,
    /// 提交时要求的 SIM（第二期 O9）：敏感判定时读到的 ICCID；写之前读到的不是它就拒绝、不动设备。
    pub expect_iccid: Option<String>,
}

impl Request {
    pub fn who(&self) -> Who {
        Who {
            source: self.source,
            approved_by: self.approved_by,
        }
    }
}

#[derive(Debug)]
pub enum Submit {
    /// 这个 op_id 已经有了（幂等）：只回现有状态，不再动设备。
    Existing(Value),
    /// 别的事务进行中：它是什么。
    Busy(Value),
    /// 写之前读不到当前值，没动设备。
    NoCapture(String),
    /// 按设备现状不能这样改（自动模式下启用手动 APN、撤销时已被别人改过），没动设备。
    Refused(String),
    /// 这次不算事务：调用方照原来的 `/control` 做。
    Plain,
    /// 写了：写调用的结果（报错时事务仍按读回判断）和事务状态。
    Applied {
        result: Result<Value, String>,
        op: Value,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum LegacyGate {
    /// 照原来的处理做。
    Pass,
    /// 回成功、进旧请求队列。
    Queue,
}

struct LegacyReq {
    item: &'static str,
    action: String,
    params: Value,
    at_ms: u64,
}

const RECENT: usize = 16;
/// 等确认期间至少隔这么久落一次盘（重启后算上一次开机等了多久）。
const SEEN_SAVE_MS: u64 = 10_000;

/// DD18：第一次打开自动退回时的提示。
pub const NOTICE_ROLLBACK_ON: &str = "rollback_on";

/// D40：事务进行中来了影响上网的写，自动来源被挡回（409 `op_busy`）时说清是哪个事务。
fn busy_op(t: &Txn) -> Value {
    json!({"op_id": t.op_id, "item": t.item, "phase": t.phase})
}

/// 搜网会话（D17）：agent 的搜网/手动注册/回自动流程期间占住写锁。
#[derive(Clone, Debug)]
struct Session {
    id: String,
    who: Who,
    opened_ms: u64,
    renewed_ms: u64,
}

/// 会话最长多久（D17：7 分钟）。
pub const SESSION_MAX_MS: u64 = 7 * 60_000;
/// 多久没续就当 agent 不在了（agent 每 20 秒续一次）。
pub const SESSION_LEASE_MS: u64 = 60_000;

/// 会话里的步骤：只有带着当前会话号才能做。
pub const SESSION_STEPS: &[&str] = &["netselect.scan", "netselect.register"];
/// 会话里也能做、没有会话时也能做的（guard 退回自动、重拨；D15/D17）。
pub const SESSION_OPTIONAL: &[&str] = &["netselect.auto", "cellular.redial"];
/// 会话期间收 409 的写：影响上网的那些（和 agent 应急写覆盖的一致）。其他写（短信、USB……）照常。
pub const SESSION_BLOCKS: &[&str] = &[
    "cellular.set",
    "network.set_mode",
    "band.set_lte",
    "band.set_nr_sa",
    "band.set_nr_nsa",
    "band.reset",
    "cell.lock_lte",
    "cell.lock_nr",
    "apn.set_mode",
    "apn.enable",
    "apn.add",
    "apn.modify",
    "apn.delete",
    "modem.online",
    "apn.set_pdp_type",
];

#[derive(Default)]
struct St {
    active: Option<Box<Txn>>,
    session: Option<Session>,
    recent: VecDeque<Txn>,
    legacy: VecDeque<LegacyReq>,
    saved_seen_ms: u64,
    /// 最近结束的事务（界面上的结果行，V2-34、V2-37）。
    last: Option<Last>,
    /// DD18：「自动退回已打开」的提示有人点过「知道了」（`notice.json`）。
    notice_acked: bool,
}

struct Inner<D> {
    dev: D,
    cfg: Config,
    store: Store,
    record: Record,
    st: Mutex<St>,
    /// 叫醒驱动（用户点了退回）：退回只由驱动发，不会发两次。
    wake: Notify,
    runner: OnceLock<LegacyRunner>,
    draining: AtomicBool,
    observer: OnceLock<Observer>,
}

pub struct Engine<D: Device> {
    inner: Arc<Inner<D>>,
}

impl<D: Device> Clone for Engine<D> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

fn new_op_id() -> String {
    format!("{:016x}", rand::random::<u64>())
}

/// 客户端给的 op_id：1–64 个字母、数字、`.`、`_`、`-`。
pub fn valid_op_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// 进行中的事务能不能被这个请求覆盖：同一项、来源可以覆盖（用户写或 guard，D15/D16），
/// 并且旧事务在等确认（写调用或退回调用还在途时不行）。
fn overridable(t: &Txn, req: &Request) -> bool {
    t.item == req.spec.item
        && req.approved_by.may_override()
        && matches!(t.phase, Phase::Verifying | Phase::RollingBack)
        && t.intent.is_none()
}

/// 进行中的是 Agent 自己批准的事务、来的是人的写（不论哪一项）：人优先，Agent 的试用让开
/// （other_change，不再自动退回；第二期，用户 10-17 定）。
fn yields_to(t: &Txn, who: Who) -> bool {
    t.who().approved_by == Source::Mcp && who.approved_by.is_user() && !t.phase.is_final()
}

/// Agent 自己批准的事务结束时（第二期：结果和改动记录进 events）。只在真机上接（`server.rs`）。
pub type AgentEnd = Box<dyn Fn(&Txn) + Send + Sync>;
static AGENT_END: OnceLock<AgentEnd> = OnceLock::new();

pub fn on_agent_end(f: AgentEnd) {
    let _ = AGENT_END.set(f);
}

impl St {
    /// 人的写来了、进行中的是 Agent 的试用：取消它（[`yields_to`]）。取消了回 true。
    fn yield_to(&mut self, who: Who, rec: &Record, store: &Store) -> bool {
        match self.active.as_mut() {
            Some(t) if yields_to(t, who) => {
                t.cancel(Reason::OtherChange);
                self.retire(rec, store);
                store.clear();
                true
            }
            _ => false,
        }
    }

    fn live(&mut self, op_id: &str) -> Option<&mut Txn> {
        match &mut self.active {
            Some(t) if t.op_id == op_id => Some(t.as_mut()),
            _ => None,
        }
    }

    fn find(&self, op_id: &str, now: u64) -> Option<Value> {
        if let Some(t) = &self.active
            && t.op_id == op_id
        {
            return Some(self.view(t, now));
        }
        self.recent
            .iter()
            .rev()
            .find(|t| t.op_id == op_id)
            .map(|t| self.view(t, now))
    }

    /// DD10：同一项后来又有别的事务（结束的或进行中的）。
    fn ctx(&self, t: &Txn) -> ui::Ctx {
        let active_later = self
            .active
            .as_ref()
            .is_some_and(|a| a.op_id != t.op_id && a.item == t.item);
        let recent_later = self
            .recent
            .iter()
            .skip_while(|r| r.op_id != t.op_id)
            .skip(1)
            .any(|r| r.item == t.item);
        ui::Ctx {
            later_change: active_later || recent_later,
        }
    }

    fn view(&self, t: &Txn, now: u64) -> Value {
        ui::view(t, now, self.ctx(t))
    }

    /// `op` 块（V2-34）。不带 `age_ms`：它每拍都变，没有进行中的事务时块要保持不变、不发。
    /// 自动退回开着、还没人点过「知道了」时加 `notice: "rollback_on"`（V2-44）；没有时不带这个键。
    fn block(&self, rollback_enabled: bool, now: u64) -> Value {
        let view = |t: &Txn| {
            let mut v = self.view(t, now);
            if let Some(m) = v.as_object_mut() {
                m.remove("age_ms");
            }
            v
        };
        let last = self.last.as_ref().map(|l| {
            let mut v = view(&l.txn);
            v["acked"] = json!(l.acked);
            v["needs_ack"] = json!(!l.acked && ui::sticky(&l.txn));
            v
        });
        let mut b = json!({
            "rollback_enabled": rollback_enabled,
            "active": self.active.as_deref().map(view),
            "last": last,
        });
        if rollback_enabled && !self.notice_acked {
            b["notice"] = json!(NOTICE_ROLLBACK_ON);
        }
        b
    }

    fn remember(&mut self, t: Txn) {
        if self.recent.len() >= RECENT {
            self.recent.pop_front();
        }
        self.recent.push_back(t);
    }

    /// 把进行中的事务移进最近列表（它已经是终态）、记一行流水账。不删落盘：调用方决定。
    fn retire(&mut self, rec: &Record, store: &Store) {
        if let Some(t) = self.active.take() {
            eprintln!(
                "ops: {} {} from {:?} ended {:?}/{:?}",
                t.op_id, t.action, t.source, t.phase, t.reason
            );
            rec.append(record::txn_line(&t));
            if t.who().approved_by == Source::Mcp
                && let Some(f) = AGENT_END.get()
            {
                f(&t);
            }
            let last = Last {
                txn: (*t).clone(),
                acked: false,
            };
            store.save_last(&last);
            self.last = Some(last);
            self.remember(*t);
        }
    }
}

/// 旧请求队列的一行（排队、被更新的替换、丢掉）；真正执行时由 `/control` 那边再记一行结果。
fn legacy_line(r: &LegacyReq, result: &str, reason: Option<&str>) -> Value {
    json!({
        "source": Source::Legacy,
        "action": r.action,
        "item": r.item,
        "params": record::redact(&r.action, &r.params),
        "result": result,
        "reason": reason,
    })
}

fn doing_session(s: &Session, now: u64) -> Value {
    let age = now.saturating_sub(s.opened_ms);
    let (zh, en) = ui::busy_words("netselect.session", s.who, age);
    let mut v = json!({
        "op_id": s.id,
        "action": "netselect.session",
        "source": s.who.source,
        "phase": "session",
        "age_ms": age,
        "say_zh": zh,
        "say_en": en,
    });
    s.who.tag(&mut v);
    v
}

/// busy 回复里的「正在做什么」（V2-39 带一句话）。
fn doing(t: &Txn, now: u64) -> Value {
    let age = now.saturating_sub(t.created_ms);
    let (zh, en) = ui::busy_words(&t.item, t.who(), age);
    let mut v = json!({
        "op_id": t.op_id,
        "action": t.action,
        "source": t.source,
        "phase": t.phase,
        "age_ms": age,
        "say_zh": zh,
        "say_en": en,
    });
    t.who().tag(&mut v);
    v
}

impl<D: Device> Engine<D> {
    pub fn new(dev: D, cfg: Config, store: Store, record: Record) -> Self {
        let last = store.load_last();
        let notice_acked = store.notice_acked(NOTICE_ROLLBACK_ON);
        Self {
            inner: Arc::new(Inner {
                dev,
                cfg,
                store,
                record,
                st: Mutex::new(St {
                    last,
                    notice_acked,
                    ..St::default()
                }),
                wake: Notify::new(),
                runner: OnceLock::new(),
                draining: AtomicBool::new(false),
                observer: OnceLock::new(),
            }),
        }
    }

    pub fn set_legacy_runner(&self, runner: LegacyRunner) {
        let _ = self.inner.runner.set(runner);
    }

    /// 流水账和 owners（`journal.append`、旧请求直接执行的写也记在这里）。
    /// 设备（`/control` 交接 Wi-Fi 锁用）。
    pub fn device(&self) -> &D {
        &self.inner.dev
    }

    pub fn record(&self) -> &Record {
        &self.inner.record
    }

    fn lock(&self) -> MutexGuard<'_, St> {
        self.inner.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn now(&self) -> u64 {
        self.inner.dev.now_ms()
    }

    /// 落盘当前事务：阶段变了每次都落，只是又等了一拍的隔 10 秒落一次；到终态删掉。
    fn persist(&self, st: &mut St, force: bool) {
        let Some(t) = &st.active else {
            self.publish(st);
            return;
        };
        if !t.phase.is_final() {
            self.publish(st);
        }
        if t.phase.is_final() {
            self.inner.store.clear();
        } else if force || t.seen_ms.saturating_sub(st.saved_seen_ms) >= SEEN_SAVE_MS {
            let _ = self.inner.store.save(t);
            st.saved_seen_ms = t.seen_ms;
        }
    }

    /// 当前事务到了终态：放锁、记进最近列表、删落盘。
    fn finish(&self, st: &mut St) {
        if st.active.as_ref().is_some_and(|t| t.phase.is_final()) {
            st.retire(&self.inner.record, &self.inner.store);
            self.inner.store.clear();
            self.publish(st);
        }
    }

    /// 把 `op` 块交出去（V2-34）：在引擎的锁里算、在锁里交，先后不会乱。
    fn publish(&self, st: &St) {
        if let Some(obs) = self.inner.observer.get() {
            obs(st.block(self.inner.cfg.rollback, self.now()));
        }
    }

    /// 接上 `/v2` 的 `op` 块，并马上交一次（启动后、第一轮之前订阅的也有数据）。
    pub fn set_observer(&self, obs: Observer) {
        let _ = self.inner.observer.set(obs);
        self.publish_now();
    }

    /// 每轮采集交一次：进行中时倒计时跟着走，核心卡住时这块和别的块一样变 stale。
    pub fn publish_now(&self) {
        let st = self.lock();
        self.publish(&st);
    }

    /// `op` 块（按这一刻算）。
    #[cfg(test)]
    pub fn block(&self) -> Value {
        self.lock().block(self.inner.cfg.rollback, self.now())
    }

    /// `/v2/screen` 用的两样，在同一把锁里算（叠在首页的和 `op` 不会一个是进行中、一个已结束）。
    pub fn screen(&self) -> (Option<ScreenOp>, Value) {
        let st = self.lock();
        let now = self.now();
        let op = ui::screen_op(
            st.active.as_deref(),
            st.last.as_ref().map(|l| (&l.txn, l.acked)),
            now,
        );
        (op, st.block(self.inner.cfg.rollback, now))
    }

    /// 首页结论要叠的写操作（V2-38）。
    #[cfg(test)]
    pub fn screen_op(&self) -> Option<ScreenOp> {
        let st = self.lock();
        ui::screen_op(
            st.active.as_deref(),
            st.last.as_ref().map(|l| (&l.txn, l.acked)),
            self.now(),
        )
    }

    /// 「知道了」（V2-37）：只记账，只能点最近结束的那个；点过再点照样成功、不重复记账。
    pub fn ack(&self, op_id: &str, who: impl Into<Who>) -> Result<Value, String> {
        let who = who.into();
        let mut st = self.lock();
        let Some(last) = st.last.as_mut().filter(|l| l.txn.op_id == op_id) else {
            return Err("not the latest finished change".into());
        };
        if !last.acked {
            last.acked = true;
            self.inner.store.save_last(last);
            let mut line = json!({
                "source": who.source,
                "action": "op.ack",
                "op_id": op_id,
                "item": last.txn.item,
                "result": "ok",
            });
            who.tag(&mut line);
            self.inner.record.append(line);
        }
        self.publish(&st);
        Ok(json!({"op_id": op_id, "acked": true}))
    }

    /// 「自动退回已打开」的提示点「知道了」（DD18，V2-44）：落盘到 `notice.json`，触屏和网页点一次都算。
    /// 点过再点照样成功、不重复记账。
    pub fn notice_ack(&self, who: impl Into<Who>) -> Value {
        let who = who.into();
        let mut st = self.lock();
        if !st.notice_acked {
            st.notice_acked = true;
            self.inner.store.save_notice_ack(NOTICE_ROLLBACK_ON);
            let mut line = json!({
                "source": who.source,
                "action": "op.notice_ack",
                "notice": NOTICE_ROLLBACK_ON,
                "result": "ok",
            });
            who.tag(&mut line);
            self.inner.record.append(line);
        }
        self.publish(&st);
        json!({"notice": NOTICE_ROLLBACK_ON, "acked": true})
    }

    /// D40：事务在确认或退回中时来了影响上网的写（安全类写不走这里，照旧插队）。
    /// 用户的（screen、web、legacy）照做，同时取消这次自动退回（other_change）；自动来源的挡回去，
    /// 返回进行中的事务（`op_id`、`item`、`phase`），调用方回 409 `op_busy`。没有进行中的事务就照做。
    /// 用户与否按 `approved_by` 判断（T22，D23：人确认过的 MCP 写算用户写）。
    pub fn other_write(&self, who: impl Into<Who>) -> Result<(), Value> {
        let who = who.into();
        {
            let mut st = self.lock();
            // Wi-Fi 设置不影响上网：照做，不取消它
            let Some(t) = st.active.as_mut().filter(|t| !spec::is_wifi(&t.item)) else {
                return Ok(());
            };
            if !who.approved_by.is_user() {
                return Err(busy_op(t));
            }
            t.cancel(Reason::OtherChange);
            self.finish(&mut st);
        }
        self.kick_legacy();
        Ok(())
    }

    /// `op.interrupt`（D40）：agent 里不经核心的用户写（eSIM 切换、AT 终端）之前发。有进行中的事务就
    /// 取消（other_change），流水账另记一行带 `what`；没有就什么都不做。
    pub fn interrupt(&self, who: impl Into<Who>, what: &str) -> Value {
        let who = who.into();
        let op_id = {
            let mut st = self.lock();
            let Some(t) = st.active.as_mut().filter(|t| !spec::is_wifi(&t.item)) else {
                return json!({"interrupted": false});
            };
            t.cancel(Reason::OtherChange);
            let (op_id, item) = (t.op_id.clone(), t.item.clone());
            self.finish(&mut st);
            let mut line = json!({
                "source": who.source,
                "action": "op.interrupt",
                "op_id": op_id,
                "item": item,
                "what": what,
                "result": "ok",
            });
            who.tag(&mut line);
            self.inner.record.append(line);
            op_id
        };
        self.kick_legacy();
        json!({"interrupted": true, "op_id": op_id})
    }

    /// 核心启动时：有落盘的事务就接着跑（D13、D18、D31）。没有就什么都不读（不多发 ubus）。
    /// 整个过程拿着跨进程写锁（D29）：应急直写正在写时，等它写完再看 takeover 标记。
    pub async fn start(&self) {
        let _lock = super::write_lock::acquire().await;
        let takeover = self.inner.store.take_takeover();
        let Some(mut t) = self.inner.store.load() else {
            if takeover {
                eprintln!("ops: takeover marker found, no change was pending");
            }
            return;
        };
        let now = self.now();
        if takeover {
            t.cancel(Reason::Takeover);
        } else {
            // 要退回的（reboot_loop）由驱动发：发之前先核对 SIM。
            if let txn::Next::Rollback(v) = t.resume(&self.inner.dev.boot_id(), now) {
                eprintln!(
                    "ops: {} still unconfirmed after a reboot, rolling back to {v}",
                    t.op_id
                );
            }
        }
        let op_id = t.op_id.clone();
        eprintln!(
            "ops: resuming {op_id} {} ({:?}, boots {})",
            t.action, t.phase, t.boots
        );
        let live = {
            let mut st = self.lock();
            st.active = Some(Box::new(t));
            self.persist(&mut st, true);
            self.finish(&mut st);
            st.active.is_some()
        };
        if live {
            self.spawn_driver(op_id);
        }
    }

    /// 一次新写（带 source 的客户端）。
    ///
    /// 先看一眼锁（明显 busy 就不读设备），再读当前值和 SIM，然后在一把锁里重新判断、
    /// 覆盖旧事务（superseded，继承退回目标，D35）、落盘新事务的写意图——中间没有空窗，
    /// 被覆盖事务的 pending.json 直接被新的盖掉。
    pub async fn submit(&self, mut req: Request) -> Submit {
        let now = self.now();
        let op_id = req.op_id.clone().unwrap_or_else(new_op_id);
        let who = req.who();
        {
            let mut st = self.lock();
            if let Some(v) = st.find(&op_id, now) {
                return Submit::Existing(v);
            }
            if st.yield_to(req.who(), &self.inner.record, &self.inner.store) {
                self.publish(&st);
            }
            if let Some(s) = &st.session {
                return Submit::Busy(doing_session(s, now));
            }
            if let Some(t) = &st.active
                && !overridable(t, &req)
            {
                return Submit::Busy(doing(t, now));
            }
        }

        req.target = match self
            .inner
            .dev
            .resolve(req.spec, req.target, &req.params)
            .await
        {
            Ok(Some(v)) => v,
            Ok(None) => {
                // 照普通写做，但也要排在进行中的事务后面（APN 的普通写同样碰 APN）
                let mut st = self.lock();
                if st.yield_to(who, &self.inner.record, &self.inner.store) {
                    self.publish(&st);
                }
                if let Some(s) = &st.session {
                    return Submit::Busy(doing_session(s, now));
                }
                if let Some(t) = &st.active {
                    return Submit::Busy(doing(t, now));
                }
                return Submit::Plain;
            }
            Err(e) => return Submit::Refused(e),
        };
        // 写之前读当前值和 SIM（退回目标、R3 的「旧值」、D32 的 SIM 身份）。
        let (old, sim, conn) = match self.inner.dev.read(req.spec).await {
            Ok(Reading {
                value: Some(v),
                sim,
                data,
                ..
            }) => (
                v,
                sim,
                data.map(|d| d.conn).filter(|c| !c.addr().is_empty()),
            ),
            Ok(_) => {
                return Submit::NoCapture("cannot read current value: not reported".into());
            }
            Err(e) => return Submit::NoCapture(format!("cannot read current value: {e}")),
        };
        if let Some(want) = &req.expect_iccid
            && *want != sim.iccid
        {
            return Submit::Refused("the SIM changed since the check; not changing anything".into());
        }
        let now = self.now();
        {
            let mut st = self.lock();
            if let Some(v) = st.find(&op_id, now) {
                return Submit::Existing(v);
            }
            if let Some(s) = &st.session {
                return Submit::Busy(doing_session(s, now));
            }
            st.yield_to(req.who(), &self.inner.record, &self.inner.store);
            let mut inherit = None;
            if let Some(t) = st.active.as_mut() {
                if !overridable(t, &req) {
                    return Submit::Busy(doing(t, now));
                }
                inherit = Some(t.rollback_to.clone());
                t.cancel(Reason::Superseded);
                st.retire(&self.inner.record, &self.inner.store);
            }
            let mut t = Txn::new(
                NewTxn {
                    op_id: op_id.clone(),
                    action: req.spec.action.into(),
                    item: req.spec.item.into(),
                    source: req.source,
                    approved_by: req.approved_by,
                    undo: req.undo,
                    target: req.target.clone(),
                    rollback_to: inherit.unwrap_or_else(|| old.clone()),
                    old,
                    sim,
                    conn,
                    confirm: req.spec.confirm,
                    // legacy 来源的写不带自动退回（按批准者判断，T22；legacy 不会有别的批准者）。
                    // Agent 自己批准的（approved_by=mcp）一律带退回，不看全局开关（第二期 D4）；
                    // 切之前就确定不通的由调用方关掉（`Request::rollback`，D10）。
                    rollback_enabled: req.rollback.unwrap_or(
                        (self.inner.cfg.rollback || req.approved_by == Source::Mcp)
                            && req.approved_by != Source::Legacy,
                    ),
                    deadline_ms: req.spec.deadline_ms(),
                    boot_id: self.inner.dev.boot_id(),
                },
                now,
            );
            t.begin_apply();
            // O6：Agent 的事务先落盘成功（退回材料在里面）才动设备
            if req.approved_by == Source::Mcp && !self.inner.store.save(&t) {
                return Submit::Refused(
                    "cannot save the change on the device; not changing anything".into(),
                );
            }
            st.active = Some(Box::new(t));
            self.persist(&mut st, true);
        }
        let result = self
            .inner
            .dev
            .write(req.spec, &req.target, &req.params)
            .await;
        // 写到了设备（成功，或超时、结果未知）：这一项最后是这个来源写的（D16）。
        let reached = match &result {
            Ok(_) => true,
            Err(e) => e.timed_out,
        };
        if reached {
            self.inner.record.set_owner(
                req.spec.item,
                req.who(),
                req.undo,
                &req.target,
                Some(&op_id),
            );
        }
        let now = self.now();
        let (op, live) = {
            let mut st = self.lock();
            let live = match st.live(&op_id) {
                Some(t) => {
                    t.applied(
                        match &result {
                            Ok(_) => txn::Applied::Done,
                            Err(e) if e.timed_out => txn::Applied::Unknown,
                            Err(_) => txn::Applied::Failed,
                        },
                        now,
                    );
                    true
                }
                // 写调用在途时被安全类写打断了（preempted）。
                None => false,
            };
            self.persist(&mut st, true);
            (st.find(&op_id, now).unwrap_or(Value::Null), live)
        };
        if live {
            self.spawn_driver(op_id);
        } else {
            self.kick_legacy();
        }
        Submit::Applied {
            result: result.map_err(|e| e.message),
            op,
        }
    }

    fn spawn_driver(&self, op_id: String) {
        let e = self.clone();
        tokio::spawn(async move { e.drive(op_id).await });
    }

    /// 等确认：要退回就先退回（只有这里发退回）；否则每拍读一次，喂给状态机。
    async fn drive(&self, op_id: String) {
        loop {
            let (generation, spec, rollback) = {
                let mut st = self.lock();
                let Some(t) = st.live(&op_id) else { break };
                let rollback = (t.phase == Phase::RollingBack
                    && t.intent == Some(txn::Intent::Rollback))
                .then(|| t.rollback_to.clone());
                (t.generation, spec::find(&t.action), rollback)
            };
            // 发之前读不到设备时 send_rollback 回 false：下一拍再试。
            if let Some(v) = rollback
                && self.send_rollback(&op_id, spec, v).await
            {
                continue;
            }
            tokio::select! {
                _ = tokio::time::sleep(self.inner.cfg.poll) => {}
                _ = self.inner.wake.notified() => continue,
            }
            let mut reading = match spec {
                Some(s) => Some(self.inner.dev.read(s).await),
                None => None,
            };
            // 读数别的条件都齐了：在锁外做一次 DNS 探测（不经执行者、不拿写锁）。
            if let Some(Ok(r)) = &mut reading {
                let target = {
                    let mut st = self.lock();
                    let Some(t) = st.live(&op_id) else { break };
                    t.wants_probe(r, self.now())
                };
                if let Some(target) = target {
                    let result = self.inner.dev.probe(&target).await;
                    if let Err(e) = &result {
                        self.log_probe_failure(&op_id, &target, e);
                    }
                    r.probe = Some(result.is_ok());
                }
            }
            let now = self.now();
            let mut st = self.lock();
            let Some(t) = st.live(&op_id) else { break };
            if t.generation != generation {
                // 读的时候阶段变了（用户点了退回/保留）：这个读数不算。
                continue;
            }
            // 结果都在事务里：终态由 finish 收，要退回的下一圈由上面发。
            let _ = match &reading {
                Some(Ok(r)) => t.on_reading(r, now),
                _ => t.on_tick(now),
            };
            let changed = t.generation != generation;
            self.persist(&mut st, changed);
            self.finish(&mut st);
        }
        self.kick_legacy();
    }

    /// 探测失败每轮只在第一次失败、和失败满次数时各记一行（契约测试里每拍都会失败）。
    fn log_probe_failure(&self, op_id: &str, target: &ProbeTarget, e: &str) {
        let fails = {
            let mut st = self.lock();
            match st.live(op_id) {
                Some(t) => t.probe_fails,
                None => return,
            }
        };
        // 这次失败还没记进事务，fails 是之前的次数；已满 = 新的一轮或换了连接（会重新计数）。
        let first = fails == 0 || fails >= txn::PROBE_TRIES;
        if first || fails + 1 == txn::PROBE_TRIES {
            eprintln!(
                "ops: {op_id} DNS probe on {} failed ({e}){}",
                if target.iface.is_empty() {
                    "no interface"
                } else {
                    &target.iface
                },
                if fails + 1 == txn::PROBE_TRIES {
                    "; next round in 30 s"
                } else {
                    ""
                }
            );
        }
    }

    /// 发退回（意图已经在事务里）。发之前先读一次核对 SIM（D32）：换了卡就放弃、不写；
    /// 读不到就不发，返回 false（下一拍再试）。读回已经是退回目标（重启前发过了，O7）就不再写。
    /// 发出去以后只按读回判断，不发第二次。
    async fn send_rollback(&self, op_id: &str, spec: Option<&'static Spec>, value: Value) -> bool {
        {
            let mut st = self.lock();
            if st.live(op_id).is_none() {
                return true;
            }
            self.persist(&mut st, true);
        }
        let Some(spec) = spec else {
            // 描述表里已经没有这个动作（旧版本落的盘）：发不了，按发过处理，到点记 rollback_failed。
            self.rollback_sent(op_id);
            return true;
        };
        let reading = self.inner.dev.read(spec).await;
        {
            let mut st = self.lock();
            let Some(t) = st.live(op_id) else { return true };
            let check = match &reading {
                Ok(r) => txn::sim_check(&t.sim, &r.sim),
                Err(_) => txn::SimCheck::Unknown,
            };
            match check {
                txn::SimCheck::Same => {}
                txn::SimCheck::Changed => {
                    t.cancel(Reason::SimChanged);
                    self.finish(&mut st);
                    return true;
                }
                txn::SimCheck::Unknown => {
                    // 一直核对不了也有时限（O7）
                    let now = self.now();
                    let ended = t.on_tick(now) == txn::Next::Done;
                    if ended {
                        self.finish(&mut st);
                    }
                    return ended;
                }
            }
            if let Ok(Reading { value: Some(v), .. }) = &reading
                && txn::matches(&t.rollback_to, v)
            {
                drop(st);
                eprintln!("ops: {op_id} already at the rollback value, not writing again");
                self.rollback_sent(op_id);
                return true;
            }
        }
        if let Err(e) = self
            .inner
            .dev
            .write(spec, &value, &spec.to_params(&value))
            .await
        {
            eprintln!("ops: {op_id} rollback write failed: {}", e.message);
        }
        self.rollback_sent(op_id);
        true
    }

    fn rollback_sent(&self, op_id: &str) {
        let now = self.now();
        let mut st = self.lock();
        if let Some(t) = st.live(op_id) {
            t.rollback_sent(now);
            self.persist(&mut st, true);
        }
    }

    /// `op.status`：指定 op_id 的，或当前的 / 最近结束的那个。
    pub fn status(&self, op_id: Option<&str>) -> Value {
        let now = self.now();
        let st = self.lock();
        match op_id {
            Some(id) => st.find(id, now).unwrap_or(Value::Null),
            None => match &st.active {
                Some(t) => t.status(now),
                None => st.recent.back().map_or(Value::Null, |t| t.status(now)),
            },
        }
    }

    /// 「立即退回」：记下意图、落盘，叫醒驱动去发（发之前核对 SIM）。
    pub fn revert(&self, op_id: &str) -> Result<Value, String> {
        let now = self.now();
        let mut st = self.lock();
        let Some(t) = st.live(op_id) else {
            return Err("no such change in progress".into());
        };
        t.revert()?;
        let v = t.status(now);
        self.persist(&mut st, true);
        drop(st);
        self.inner.wake.notify_one();
        Ok(v)
    }

    /// 「保留现状」。
    pub fn keep(&self, op_id: &str) -> Result<Value, String> {
        let now = self.now();
        let v = {
            let mut st = self.lock();
            let Some(t) = st.live(op_id) else {
                return Err("no such change in progress".into());
            };
            t.keep()?;
            self.finish(&mut st);
            st.find(op_id, now).unwrap_or(Value::Null)
        };
        self.kick_legacy();
        Ok(v)
    }

    /// 安全类写（关数据/漫游）马上插队：取消进行中的事务（preempted），清空旧请求队列。
    pub fn preempt(&self) {
        let mut st = self.lock();
        // 关数据、关漫游不碰 Wi-Fi 设置的确认
        if let Some(t) = st.active.as_mut().filter(|t| !spec::is_wifi(&t.item)) {
            t.cancel(Reason::Preempted);
            self.finish(&mut st);
        }
        self.drop_legacy(&mut st, "a safety write");
    }

    /// 重启/关机被接受：清空旧请求队列（进行中的事务不放弃，开机后接着确认）。
    pub fn clear_legacy(&self, why: &str) {
        self.drop_legacy(&mut self.lock(), why);
    }

    fn drop_legacy(&self, st: &mut St, why: &str) {
        for r in st.legacy.drain(..) {
            eprintln!("ops: dropped queued legacy {} because of {why}", r.action);
            self.inner
                .record
                .append(legacy_line(&r, "dropped", Some(why)));
        }
    }

    /// 没有 source 的旧请求进来时：`item` 是它改的项（不在描述表里为 None），`safety` 是 D14 的安全类。
    pub fn legacy_gate(&self, item: Option<&'static str>, safety: bool) -> LegacyGate {
        if safety {
            self.preempt();
            return LegacyGate::Pass;
        }
        let Some(item) = item else {
            return LegacyGate::Pass;
        };
        let mut st = self.lock();
        if st.session.is_some() {
            return LegacyGate::Queue;
        }
        if st.yield_to(Source::Legacy.into(), &self.inner.record, &self.inner.store) {
            self.publish(&st);
            return LegacyGate::Pass;
        }
        let Some(t) = st.active.as_mut() else {
            return LegacyGate::Pass;
        };
        if t.item == item
            && matches!(t.phase, Phase::Verifying | Phase::RollingBack)
            && t.intent.is_none()
        {
            // 同一项：当作覆盖写，旧事务记 superseded。
            t.cancel(Reason::Superseded);
            self.finish(&mut st);
            return LegacyGate::Pass;
        }
        LegacyGate::Queue
    }

    /// 排一个旧请求（每项只留最新的）。
    pub fn enqueue_legacy(&self, item: &'static str, action: &str, params: Value) {
        let now = self.now();
        {
            let mut st = self.lock();
            let rec = &self.inner.record;
            st.legacy.retain(|r| {
                let keep = r.item != item;
                if !keep {
                    eprintln!("ops: queued legacy {} replaced by a newer one", r.action);
                    rec.append(legacy_line(r, "replaced", None));
                }
                keep
            });
            let r = LegacyReq {
                item,
                action: action.into(),
                params,
                at_ms: now,
            };
            rec.append(legacy_line(&r, "queued", None));
            st.legacy.push_back(r);
        }
        self.kick_legacy();
    }

    /// 锁空着就把排队的旧请求一个个做掉；执行者队列满就隔 1 秒再试；排太久的丢掉。
    fn kick_legacy(&self) {
        let Some(runner) = self.inner.runner.get().cloned() else {
            return;
        };
        if self.inner.draining.swap(true, Ordering::AcqRel) {
            return;
        }
        let e = self.clone();
        tokio::spawn(async move {
            loop {
                let req = {
                    let now = e.now();
                    let ttl = e.inner.cfg.legacy_ttl_ms;
                    let mut st = e.lock();
                    let rec = &e.inner.record;
                    if st.active.is_some() || st.session.is_some() {
                        None
                    } else {
                        st.legacy.retain(|r| {
                            let fresh = now.saturating_sub(r.at_ms) < ttl;
                            if !fresh {
                                eprintln!("ops: dropped legacy {} after {ttl} ms", r.action);
                                rec.append(legacy_line(r, "dropped", Some("expired")));
                            }
                            fresh
                        });
                        st.legacy.pop_front()
                    }
                };
                let Some(req) = req else {
                    e.inner.draining.store(false, Ordering::Release);
                    // 放下标志之后又来了活：再抢一次。
                    let more = {
                        let st = e.lock();
                        st.active.is_none() && st.session.is_none() && !st.legacy.is_empty()
                    };
                    if more && !e.inner.draining.swap(true, Ordering::AcqRel) {
                        continue;
                    }
                    return;
                };
                if !runner(req.action.clone(), req.params.clone()).await {
                    // 执行者队列满：放回队头（这期间同一项来了更新的就不放回）。
                    {
                        let mut st = e.lock();
                        if !st.legacy.iter().any(|r| r.item == req.item) {
                            st.legacy.push_front(req);
                        }
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        });
    }
}

/// 会话怎么样了（`netselect.session.*` 的回复、流水账）。
#[derive(Debug, PartialEq)]
pub enum SessionError {
    /// 别的事务或会话占着：它是什么。
    Busy(Value),
    /// 会话号不对，或会话已经结束。
    Gone,
}

impl<D: Device> Engine<D> {
    fn session_line(&self, s: &Session, result: &str, reason: Option<&str>) {
        let mut line = json!({
            "source": s.who.source,
            "item": "netselect.session",
            "op_id": s.id,
            "result": result,
            "reason": reason,
            "age_ms": self.now().saturating_sub(s.opened_ms),
        });
        s.who.tag(&mut line);
        self.inner.record.append(line);
    }

    /// 到点或租约过期的会话收回（记流水账、放出旧请求队列）。
    fn reap_session(&self, st: &mut St) {
        let now = self.now();
        let why = match &st.session {
            Some(s) if now.saturating_sub(s.opened_ms) >= SESSION_MAX_MS => "expired",
            Some(s) if now.saturating_sub(s.renewed_ms) >= SESSION_LEASE_MS => "agent_gone",
            _ => return,
        };
        let s = st.session.take().expect("checked");
        eprintln!("ops: session {} ended: {why}", s.id);
        self.session_line(&s, "ended", Some(why));
    }

    /// 开一个搜网会话。有事务或会话进行中就回它是什么。
    pub fn session_open(&self, who: impl Into<Who>) -> Result<Value, SessionError> {
        let who = who.into();
        let now = self.now();
        let s = {
            let mut st = self.lock();
            self.reap_session(&mut st);
            if let Some(s) = &st.session {
                return Err(SessionError::Busy(doing_session(s, now)));
            }
            if let Some(t) = &st.active {
                return Err(SessionError::Busy(doing(t, now)));
            }
            let s = Session {
                id: new_op_id(),
                who,
                opened_ms: now,
                renewed_ms: now,
            };
            st.session = Some(s.clone());
            s
        };
        self.session_line(&s, "opened", None);
        // 到点、agent 不续了也要收回，不靠下一个请求来碰
        let e = self.clone();
        let id = s.id.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let ended = {
                    let mut st = e.lock();
                    e.reap_session(&mut st);
                    st.session.as_ref().is_none_or(|s| s.id != id)
                };
                if ended {
                    e.kick_legacy();
                    return;
                }
            }
        });
        Ok(json!({"session": s.id, "max_ms": SESSION_MAX_MS, "lease_ms": SESSION_LEASE_MS}))
    }

    /// agent 还在：续租约。
    pub fn session_renew(&self, id: &str) -> Result<Value, SessionError> {
        let now = self.now();
        let mut st = self.lock();
        self.reap_session(&mut st);
        match st.session.as_mut() {
            Some(s) if s.id == id => {
                s.renewed_ms = now;
                Ok(
                    json!({"session": id, "left_ms": SESSION_MAX_MS.saturating_sub(now.saturating_sub(s.opened_ms))}),
                )
            }
            _ => Err(SessionError::Gone),
        }
    }

    /// 流程做完了：放锁（`result` 记进流水账）。
    pub fn session_close(&self, id: &str, result: Option<&str>) -> Result<(), SessionError> {
        let s = {
            let mut st = self.lock();
            self.reap_session(&mut st);
            match &st.session {
                Some(s) if s.id == id => st.session.take().expect("checked"),
                _ => return Err(SessionError::Gone),
            }
        };
        self.session_line(&s, "closed", result);
        self.kick_legacy();
        Ok(())
    }

    /// 一个写能不能现在做（会话规则）：步骤要带当前会话号；回自动、重拨带了就算会话里的、
    /// 没会话时也能做；影响上网的其他写在会话期间收 409。带当前会话号的请求顺便续租约。
    pub fn session_gate(&self, action: &str, session: Option<&str>) -> Result<(), SessionError> {
        let now = self.now();
        let mut st = self.lock();
        self.reap_session(&mut st);
        let current = st.session.as_mut();
        let step = SESSION_STEPS.contains(&action);
        let optional = SESSION_OPTIONAL.contains(&action);
        match (current, session) {
            (Some(s), Some(id)) if s.id == id && (step || optional) => {
                s.renewed_ms = now;
                Ok(())
            }
            (Some(s), _) if step || optional || SESSION_BLOCKS.contains(&action) => {
                Err(SessionError::Busy(doing_session(s, now)))
            }
            (None, _) if step => Err(SessionError::Gone),
            // 会话已经结束（到点、收回）：回自动、重拨照做，算 guard 自己的一步（D15）
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests;
