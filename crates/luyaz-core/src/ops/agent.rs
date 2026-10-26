//! Agent（MCP）自己批准的写怎么放行（u60-mcp.md「第二期 Decision ledger」D5、D6、D7、D11，Codex O1、O6、O9）。
//!
//! - 白名单（D7、D11）：只有 APN（全部 `apn.*`，含不经事务的 add/delete/set_pdp_type）和 CHILL（档位、出口）
//!   两层；别的动作、`op.keep`/`op.revert` 这类对进行中事务的操作一律拒绝。按**有效**批准者判断
//!   （`source=mcp` 不带 `approved_by` 也算 mcp，O1）。取代第一期 T1「一律拒绝 approved_by=mcp」。
//! - 按层限速（D5）：每层 30 分钟一次，只限 Agent；退回不经这里，不算次数。落盘在 ops 目录的
//!   `agent-limit.json` 保留显示用 Unix 秒，`agent-limit-clock.json` 记 boot_id + CLOCK_BOOTTIME；
//!   同次开机重启核心不清零，跨开机/旧档首次写先保守等 30 分钟；任一档落不了盘就不放行（O6）。
//!   一次操作要几步写时（`apn_use`
//!   的候选 APN：先复制成手动、再切过去）拿一张票（[`Ticket`]），后面几步带着票不再计数。
//! - 敏感（D6、O9）：只看 APN 层。漫游中、且这张卡没被人标成「漫游不算贵」→ 敏感，只给网页地址；
//!   漫游状态或卡号读不到也算敏感。按卡设置存在 ops 目录的 `roam-cheap.json`，只有人能改（网页）。
//!   判定时读到的卡号交给事务（`Request::expect_iccid`），写之前再核对一次。
//! - 会断连接的一层（D-g，10-23）：描述表里退回已实测（`RollbackCapability::CapturedAndProven`：锁频、
//!   恢复默认频段、锁小区）的动作。Agent 不能自己决定，只能转达主人的指令：请求要带
//!   `instruction: "owner"`，没带就拒绝（[`Refusal::NeedsInstruction`]）。设备分不出指令真假，所以靠三道
//!   防线限制损害：试用 + 退回，确认时除了蜂窝 DNS 还要设备自己看到 Tailscale 在线（[`tailnet_up`]，Agent
//!   自己的连接会一起断，看不到结果）；这一层整体 30 分钟一次；改动记录写「来源：你的指令（未验证）」。
//!   漫游中、人在国外（注册网络的国家码 `rmcc` 不是 460，插当地卡也算）、或这两样读不到，一律敏感
//!   （只给网页地址：国外开 NR 曾让基带断言、整机重启循环）；
//!   改之前 Tailscale 本来就不在线也不做（确认必然失败、白白断一次）。

use luyaz_api::clock::BootTime;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

/// 每层多久一次（D5）。
pub const LIMIT_SECS: u64 = 30 * 60;
const LIMIT_FILE: &str = "agent-limit.json";
// Keep the old flat wall-time file readable by previous binaries. This companion is paired
// with each wall timestamp; missing/mismatched evidence gets a bounded conservative hold.
const LIMIT_CLOCK_FILE: &str = "agent-limit-clock.json";
const CHEAP_FILE: &str = "roam-cheap.json";
/// 票只在这次操作里用：几步写最多这么久。
const TICKET_SECS: u64 = 10 * 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    Apn,
    Chill,
    /// 会断 Tailscale / 改 WAN 的（D-g）：只按主人的指令做。
    Link,
    /// regress 接受新水位（设计 2026-10-24 §4，CEO G2）：不看敏感、不占限速，只按主人的指令做。
    Regress,
}

impl Layer {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Apn => "apn",
            Self::Chill => "chill",
            Self::Link => "link",
            Self::Regress => "regress",
        }
    }
}

/// CHILL 的两个事务动作（`ops::chill`）。
pub const CHILL_ACTIONS: &[&str] = &["chill.profile", "chill.exit"];

/// 这个动作属于哪一层；None = 不在白名单里。
pub fn layer_of(action: &str) -> Option<Layer> {
    if action.starts_with("apn.") {
        return Some(Layer::Apn);
    }
    if CHILL_ACTIONS.contains(&action) {
        return Some(Layer::Chill);
    }
    if action == "regress.accept" {
        return Some(Layer::Regress);
    }
    link(action).then_some(Layer::Link)
}

/// D-g 那一层：描述表里退回已实测的动作（D42 逐项实测过才改成 `CapturedAndProven`，改了自动进来）。
pub fn link(action: &str) -> bool {
    super::spec::find(action).is_some_and(|s| {
        s.rollback_capability == super::spec::RollbackCapability::CapturedAndProven
    })
}

/// 请求里「这是主人的指令」的标记（D-g）。设备核实不了，只记成「未验证」。
pub fn by_instruction(body: &Value) -> bool {
    body.get("instruction").and_then(Value::as_str) == Some("owner")
}

/// 主人的本国（中国）的国家码；和情景引擎默认的 `home_mcc` 一样。
const HOME_MCC: i64 = 460;

/// D-g 的判定：漫游中、注册在外国网络（插当地卡也算，用户 10-23 定）、或读不到，都算敏感；
/// 不看「漫游不算贵」。
pub fn link_sensitive(roaming: Option<bool>, rmcc: Option<i64>) -> Result<(), String> {
    match (roaming, rmcc) {
        (Some(true), _) => Err("roaming: band and cell changes go to the owner on the web page".into()),
        (None, _) => Err("cannot read whether the device is roaming".into()),
        (_, Some(HOME_MCC)) => Ok(()),
        (_, Some(m)) if m > 0 => Err("abroad: band and cell changes go to the owner on the web page".into()),
        _ => Err("cannot read which country's network the device is on".into()),
    }
}

/// 现在漫游不漫游、在哪国的网络上（读原厂 netinfo 的 `simcard_roam`、`rmcc`）。
pub async fn link_sensitivity() -> Result<(), String> {
    let net = crate::state::ubus("zte_nwinfo_api", "nwinfo_get_netinfo", json!({})).await;
    let net = net.as_ref().ok();
    let rmcc = net.and_then(|n| match &n["rmcc"] {
        Value::Number(m) => m.as_i64(),
        Value::String(m) => m.trim().parse().ok(),
        _ => None,
    });
    link_sensitive(net.and_then(super::roaming), rmcc)
}

/// `tailscale status --json` 说本机在线：后台在跑、本节点连着控制面。读不到、不认识都算不在线。
pub fn tailnet_online(status: &Value) -> bool {
    status["BackendState"] == "Running" && status["Self"]["Online"] == true
}

/// 设备自己看 Tailscale 在不在线（阻塞，最多约 5 秒）。
pub async fn tailnet_up() -> bool {
    tokio::task::spawn_blocking(crate::api::home::tailscale_status)
        .await
        .ok()
        .flatten()
        .is_some_and(|v| tailnet_online(&v))
}

/// Agent 不用过关就能调的只读动作。
pub fn read_only(action: &str) -> bool {
    matches!(action, "op.status" | "journal.list")
}

/// 为什么不放行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// 不在白名单里。
    NotAllowed,
    /// 会断连接的动作没带「主人的指令」标记（D-g）。
    NeedsInstruction,
    /// 这一层 30 分钟里做过了：下次可做的时刻（Unix 秒）。
    Limited { next_unix: u64 },
    /// 敏感：只给网页地址，交给人。
    Sensitive(String),
    /// D-g 那一层的敏感（漫游）：给频段页的地址。
    SensitiveLink(String),
    /// 落不了盘（O6）。
    Store(String),
    /// 无法安全计算期限，拒绝自动写。
    Clock,
    /// 现在做不了（别的 CHILL 工作在跑、说不清 CHILL 通不通）：过一会再试。
    Busy(String),
    /// CHILL 没在跑（mihomo 不答话）：等人打开，不是过一会再试。
    Off(String),
}

impl Refusal {
    /// HTTP 状态、错误码、说明、附加字段。
    pub fn reply(&self, now_unix: u64) -> (u16, &'static str, String, Value) {
        match self {
            Self::NotAllowed => (
                403,
                "not_allowed",
                "this action is not open to the agent".into(),
                json!({}),
            ),
            Self::NeedsInstruction => (
                403,
                "needs_instruction",
                "this change can cut the agent's own link; it runs only on the owner's instruction \
                 (instruction: owner)"
                    .into(),
                json!({}),
            ),
            Self::Limited { next_unix } => (
                429,
                "rate_limited",
                "the 30-minute agent cooldown for this layer has not expired".into(),
                json!({"next_unix": next_unix, "retry_after_s": next_unix.saturating_sub(now_unix)}),
            ),
            Self::Sensitive(why) => (403, "sensitive", why.clone(), json!({"page": "apn"})),
            Self::SensitiveLink(why) => (403, "sensitive", why.clone(), json!({"page": "band"})),
            Self::Store(why) => (503, "store_failed", why.clone(), json!({})),
            Self::Clock => (
                503,
                "clock_unavailable",
                "cannot read the boot clock; ask the owner".into(),
                json!({}),
            ),
            Self::Busy(why) => (409, "busy", why.clone(), json!({})),
            Self::Off(why) => (409, "chill_off", why.clone(), json!({})),
        }
    }
}

/// 一次放行：带着它的后续几步不再计数；做不成（没动设备）时 [`Gate::release`] 还回去。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ticket {
    pub id: String,
    pub layer: Layer,
    /// 敏感判定时读到的卡（APN 层；原样的 `sim_iccid`）。
    pub iccid: Option<String>,
    /// 这次占位之前的那次（还回去时恢复）。
    prev: Option<u64>,
    at: u64,
    elapsed: BootTime,
    prev_clock: Option<LimitClock>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Limits {
    #[serde(flatten)]
    last: BTreeMap<String, u64>,
}

#[derive(Default, Serialize, Deserialize)]
struct Cheap {
    #[serde(default)]
    cheap: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct LimitClock {
    wall: u64,
    at: BootTime,
}

type LimitClocks = BTreeMap<String, LimitClock>;

#[derive(Default)]
struct GateState {
    tickets: Vec<Ticket>,
    limits: Limits,
    clocks: LimitClocks,
}

pub struct Gate {
    dir: Option<PathBuf>,
    st: Mutex<GateState>,
}

/// 卡号的规范写法（按卡设置用）：去空白、去结尾的 F。
pub fn norm_iccid(s: &str) -> String {
    s.trim().trim_end_matches(['F', 'f']).to_owned()
}

/// D6：漫游状态和卡号 → 敏感不敏感。`cheap` 是人标成「漫游不算贵」的卡。
pub fn sensitive(roaming: Option<bool>, iccid: &str, cheap: &[String]) -> Result<(), String> {
    let id = norm_iccid(iccid);
    match roaming {
        None => Err("cannot read whether the device is roaming".into()),
        Some(false) => Ok(()),
        Some(true) if id.is_empty() => Err("roaming and the SIM card number is unreadable".into()),
        Some(true) if cheap.iter().any(|c| norm_iccid(c) == id) => Ok(()),
        Some(true) => Err("roaming on a card not marked as cheap to roam; ask the owner".into()),
    }
}

impl Gate {
    pub fn open(dir: Option<PathBuf>) -> Self {
        Self {
            dir,
            st: Mutex::new(GateState::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, GateState> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn read<T: serde::de::DeserializeOwned + Default>(&self, file: &str) -> Result<T, String> {
        let path = self.dir.as_ref().expect("persistent gate").join(file);
        match fs::read(&path) {
            Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("cannot read {file}: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
            Err(e) => Err(format!("cannot read {file}: {e}")),
        }
    }

    fn load(&self, state: &GateState) -> Result<(Limits, LimitClocks), String> {
        if self.dir.is_none() {
            return Ok((state.limits.clone(), state.clocks.clone()));
        }
        Ok((self.read(LIMIT_FILE)?, self.read(LIMIT_CLOCK_FILE)?))
    }

    fn save(&self, state: &mut GateState, l: &Limits, clocks: &LimitClocks) -> Result<(), String> {
        if let Some(dir) = &self.dir {
            fs::create_dir_all(dir).map_err(|e| format!("cannot save agent limits: {e}"))?;
            // A partial save consumes a slot rather than authorising a write. A companion
            // timestamp only applies if its wall timestamp matches the old-format file.
            for (file, bytes) in [
                (LIMIT_FILE, serde_json::to_vec(l).expect("limits serialize")),
                (
                    LIMIT_CLOCK_FILE,
                    serde_json::to_vec(clocks).expect("clocks serialize"),
                ),
            ] {
                super::pending::write_atomic(dir, file, &bytes)
                    .map_err(|e| format!("cannot save {file}: {e}"))?;
            }
        }
        state.limits = l.clone();
        state.clocks = clocks.clone();
        Ok(())
    }

    /// Last wall timestamp for display only; it does not decide the deadline.
    pub fn last(&self, layer: Layer) -> Option<u64> {
        self.load(&self.lock())
            .ok()?
            .0
            .last
            .get(layer.as_str())
            .copied()
    }

    /// Thirty elapsed minutes per layer, including suspend. Core restarts in the same boot
    /// reuse the saved boot clock. Legacy/unpaired/previous-boot records wait one full window
    /// from the first attempted write; save that anchor so another core restart cannot reset it.
    pub fn reserve(
        &self,
        layer: Layer,
        iccid: Option<String>,
        now: u64,
    ) -> Result<Ticket, Refusal> {
        self.reserve_at(layer, iccid, now, BootTime::read().as_ref())
    }

    fn reserve_at(
        &self,
        layer: Layer,
        iccid: Option<String>,
        now: u64,
        elapsed: Option<&BootTime>,
    ) -> Result<Ticket, Refusal> {
        let elapsed = elapsed
            .filter(|t| !t.boot_id.is_empty())
            .ok_or(Refusal::Clock)?;
        let mut state = self.lock();
        state.tickets.retain(|t| {
            elapsed
                .elapsed_since(&t.elapsed)
                .is_some_and(|s| s < TICKET_SECS)
        });
        let (mut l, mut clocks) = self.load(&state).map_err(Refusal::Store)?;
        let key = layer.as_str();
        let prev_clock = clocks.get(key).cloned();
        // A missing legacy file/key must not erase a still-present elapsed record.
        if !l.last.contains_key(key)
            && let Some(c) = &prev_clock
        {
            l.last.insert(key.into(), c.wall);
        }
        let prev = l.last.get(key).copied();
        if let Some(wall) = prev {
            let valid = prev_clock
                .as_ref()
                .filter(|c| c.wall == wall)
                .and_then(|c| elapsed.elapsed_since(&c.at));
            let age = match valid {
                Some(age) => age,
                None => {
                    clocks.insert(
                        key.into(),
                        LimitClock {
                            wall,
                            at: elapsed.clone(),
                        },
                    );
                    self.save(&mut state, &l, &clocks).map_err(Refusal::Store)?;
                    0
                }
            };
            if age < LIMIT_SECS {
                return Err(Refusal::Limited {
                    next_unix: now.saturating_add(LIMIT_SECS - age),
                });
            }
        }
        l.last.insert(key.into(), now);
        clocks.insert(
            key.into(),
            LimitClock {
                wall: now,
                at: elapsed.clone(),
            },
        );
        self.save(&mut state, &l, &clocks).map_err(Refusal::Store)?;
        let t = Ticket {
            id: format!("{:016x}", rand::random::<u64>()),
            layer,
            iccid,
            prev,
            at: now,
            elapsed: elapsed.clone(),
            prev_clock,
        };
        state.tickets.push(t.clone());
        Ok(t)
    }

    pub fn redeem(&self, id: &str, layer: Layer, _now: u64) -> Option<Ticket> {
        self.redeem_at(id, layer, BootTime::read().as_ref())
    }

    fn redeem_at(&self, id: &str, layer: Layer, elapsed: Option<&BootTime>) -> Option<Ticket> {
        let elapsed = elapsed?;
        self.lock()
            .tickets
            .iter()
            .find(|t| {
                t.id == id
                    && t.layer == layer
                    && elapsed
                        .elapsed_since(&t.elapsed)
                        .is_some_and(|s| s < TICKET_SECS)
            })
            .cloned()
    }

    /// Nothing changed: restore both timestamps, only while this ticket still owns the slot.
    pub fn release(&self, t: &Ticket) {
        let mut state = self.lock();
        // A repeated release must not clear a replacement reserved in the same clock second.
        if !state.tickets.iter().any(|x| x.id == t.id) {
            return;
        }
        state.tickets.retain(|x| x.id != t.id);
        let (mut l, mut clocks) = match self.load(&state) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("ops: agent gate: {e}");
                return;
            }
        };
        let key = t.layer.as_str();
        if l.last.get(key) != Some(&t.at)
            || clocks.get(key)
                != Some(&LimitClock {
                    wall: t.at,
                    at: t.elapsed.clone(),
                })
        {
            return;
        }
        match t.prev {
            Some(p) => {
                l.last.insert(key.into(), p);
            }
            None => {
                l.last.remove(key);
            }
        }
        match &t.prev_clock {
            Some(p) => {
                clocks.insert(key.into(), p.clone());
            }
            None => {
                clocks.remove(key);
            }
        }
        if let Err(e) = self.save(&mut state, &l, &clocks) {
            eprintln!("ops: agent gate: {e}");
        }
    }

    pub fn done(&self, t: &Ticket) {
        self.lock().tickets.retain(|x| x.id != t.id);
    }

    /// 人标成「漫游不算贵」的卡（规范写法）。
    pub fn cheap(&self) -> Vec<String> {
        self.dir
            .as_ref()
            .and_then(|d| fs::read(d.join(CHEAP_FILE)).ok())
            .and_then(|b| serde_json::from_slice::<Cheap>(&b).ok())
            .map(|c| c.cheap)
            .unwrap_or_default()
    }

    /// 人在网页上改一张卡的设置（只有人能调，lweb 的 /agent-api 碰不到）。
    pub fn set_cheap(&self, iccid: &str, cheap: bool) -> Result<Vec<String>, String> {
        let id = norm_iccid(iccid);
        if id.is_empty() || id.len() > 24 || !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err("iccid must be the card number".into());
        }
        let Some(dir) = &self.dir else {
            return Err("no place to keep the setting".into());
        };
        let mut list: Vec<String> = self.cheap().iter().map(|c| norm_iccid(c)).collect();
        list.retain(|c| *c != id);
        if cheap {
            list.push(id);
        }
        list.sort();
        fs::create_dir_all(dir)
            .and_then(|()| {
                super::pending::write_atomic(
                    dir,
                    CHEAP_FILE,
                    &serde_json::to_vec(&json!({"cheap": list})).expect("serializes"),
                )
            })
            .map_err(|e| format!("cannot save {CHEAP_FILE}: {e}"))?;
        Ok(list)
    }
}

/// 进程里的那一个（`/control` 和兼容层的 `apn_use` 共用）。
pub fn gate() -> &'static Gate {
    static G: OnceLock<Gate> = OnceLock::new();
    G.get_or_init(|| Gate::open(super::ops_dir()))
}

/// D6：现在改 APN 敏不敏感；不敏感时回读到的卡号（原样的 `sim_iccid`，交给事务再核对，O9）。
pub async fn apn_sensitivity() -> Result<String, String> {
    let net = crate::state::ubus("zte_nwinfo_api", "nwinfo_get_netinfo", json!({})).await;
    let sim = crate::state::sim_info().await;
    let roaming = net.as_ref().ok().and_then(super::roaming);
    let iccid = sim
        .as_ref()
        .ok()
        .and_then(|s| s.get("sim_iccid"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    sensitive(roaming, &iccid, &gate().cheap())?;
    Ok(iccid)
}

/// 告警队列里 Agent 改动的那类（不发短信，见 luyaz-alerts 的 `NEVER`）。
pub const EVENT_KIND: &str = "agent-change";

/// 记一条 Agent 改动（Agent 经 MCP 的 get_recent_events 读回结果；主人的告警转发也看得到）。尽力而为。
pub fn event(text: &str) {
    let dir = crate::state::host_path(luyaz_api::alerts::DIR);
    let up = std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
        .map_or(0, |s| s as u64);
    let a = luyaz_api::alerts::Alerts::at(std::path::Path::new(&dir));
    if let Err(e) = a.add(EVENT_KIND, text, luyaz_api::alerts::now(), up) {
        eprintln!("ops: agent event not queued: {e}");
    }
}

/// 一个 Agent 事务结束的那一行：动作、旧值 → 目标、结果（op_id 供对照 op.status）。
pub fn txn_text(t: &super::txn::Txn) -> String {
    let show = |v: &Value| match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let reason = t
        .reason
        .map(|r| {
            serde_json::to_value(r)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default()
        })
        .unwrap_or_default();
    let phase = serde_json::to_value(t.phase)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    let mut s = format!(
        "agent {}: {} -> {}: {phase}/{reason} (op {})",
        t.action,
        show(&t.old),
        show(&t.target),
        t.op_id
    );
    if !t.rollback_enabled {
        s.push_str("; no automatic rollback");
    }
    if link(&t.action) {
        s.push_str("; 来源：你的指令（未验证）");
    }
    s
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boot(seconds: u64) -> BootTime {
        BootTime {
            boot_id: "boot-a".into(),
            seconds,
        }
    }

    fn open(dir: Option<PathBuf>) -> Gate {
        Gate::open(dir)
    }

    fn reserve(g: &Gate, layer: Layer, now: u64) -> Result<Ticket, Refusal> {
        g.reserve_at(layer, None, now, Some(&boot(now)))
    }

    fn redeem(g: &Gate, id: &str, layer: Layer, now: u64) -> Option<Ticket> {
        g.redeem_at(id, layer, Some(&boot(now)))
    }

    fn dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("agent-gate-{:016x}", rand::random::<u64>()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn whitelist_is_apn_chill_and_proven_link() {
        for a in [
            "apn.set_mode",
            "apn.enable",
            "apn.modify",
            "apn.add",
            "apn.delete",
            "apn.set_pdp_type",
            "apn.use",
        ] {
            assert_eq!(layer_of(a), Some(Layer::Apn), "{a}");
        }
        for a in CHILL_ACTIONS {
            assert_eq!(layer_of(a), Some(Layer::Chill));
        }
        // D-g：退回已实测的锁频、锁小区（只按主人的指令）
        for a in [
            "band.set_lte",
            "band.set_nr",
            "band.reset",
            "cell.lock_lte",
            "cell.lock_nr",
        ] {
            assert_eq!(layer_of(a), Some(Layer::Link), "{a}");
        }
        for a in [
            "esim.switch",
            "sim.set_slot",
            "network.set_mode",
            "sim.dual",
            "wifi.guest",
            "data.reconnect",
            "op.keep",
            "op.revert",
            "device.reboot",
            "device.reset",
            "wifi.settings",
            "cellular.set",
        ] {
            assert_eq!(layer_of(a), None, "{a}");
        }
    }

    #[test]
    fn link_needs_the_owner_marker_home_network_and_tailnet() {
        assert!(by_instruction(&json!({"instruction": "owner"})));
        assert!(!by_instruction(&json!({})));
        assert!(!by_instruction(&json!({"instruction": "agent"})));
        assert!(!by_instruction(&json!({"instruction": true})));
        assert_eq!(link_sensitive(Some(false), Some(460)), Ok(()));
        assert!(link_sensitive(Some(true), Some(460)).is_err());
        assert!(link_sensitive(None, Some(460)).is_err());
        // 国外插当地卡：不漫游，但注册在外国网络
        assert!(link_sensitive(Some(false), Some(440)).is_err());
        assert!(link_sensitive(Some(false), Some(0)).is_err());
        assert!(link_sensitive(Some(false), None).is_err());
        assert!(tailnet_online(&json!({"BackendState": "Running", "Self": {"Online": true}})));
        assert!(!tailnet_online(&json!({"BackendState": "Running", "Self": {"Online": false}})));
        assert!(!tailnet_online(&json!({"BackendState": "NeedsLogin", "Self": {"Online": true}})));
        assert!(!tailnet_online(&json!({"BackendState": "Running"})));
        assert!(!tailnet_online(&Value::Null));
    }

    #[test]
    fn link_is_one_shared_layer() {
        let g = open(None);
        reserve(&g, Layer::Link, 1_000).unwrap();
        assert!(matches!(
            reserve(&g, Layer::Link, 1_100),
            Err(Refusal::Limited { .. })
        ));
        reserve(&g, Layer::Apn, 1_100).unwrap();
    }

    #[test]
    fn one_per_layer_per_half_hour_and_it_survives_a_restart() {
        let d = dir();
        let g = open(Some(d.clone()));
        let t = reserve(&g, Layer::Apn, 1_000).unwrap();
        assert_eq!(
            reserve(&g, Layer::Apn, 1_500),
            Err(Refusal::Limited {
                next_unix: 1_000 + LIMIT_SECS
            })
        );
        // 另一层不受影响
        reserve(&g, Layer::Chill, 1_500).unwrap();
        // 重启不清零
        let g2 = open(Some(d.clone()));
        assert!(matches!(
            reserve(&g2, Layer::Apn, 2_000),
            Err(Refusal::Limited { .. })
        ));
        // 票带着走几步
        assert_eq!(
            redeem(&g, &t.id, Layer::Apn, 1_100).map(|x| x.id),
            Some(t.id.clone())
        );
        assert!(redeem(&g, &t.id, Layer::Chill, 1_100).is_none());
        assert!(redeem(&g, "nope", Layer::Apn, 1_100).is_none());
        // 30 分钟后又可以
        reserve(&g2, Layer::Apn, 1_000 + LIMIT_SECS).unwrap();
    }

    #[test]
    fn released_ticket_gives_the_slot_back() {
        let g = open(Some(dir()));
        reserve(&g, Layer::Apn, 100).unwrap();
        let t = reserve(&g, Layer::Apn, 100 + LIMIT_SECS).unwrap();
        g.release(&t);
        assert_eq!(g.last(Layer::Apn), Some(100));
        assert!(redeem(&g, &t.id, Layer::Apn, 100 + LIMIT_SECS).is_none());
        reserve(&g, Layer::Apn, 100 + LIMIT_SECS).unwrap();
    }

    #[test]
    fn wall_steps_do_not_bypass_limits_or_extend_tickets() {
        for jump in [-28800_i64, 28800] {
            let g = open(Some(dir()));
            let wall = 1_000_000;
            let t = g
                .reserve_at(Layer::Apn, None, wall, Some(&boot(100)))
                .unwrap();
            let stepped = (wall as i64 + jump) as u64;
            assert_eq!(
                g.reserve_at(Layer::Apn, None, stepped, Some(&boot(101))),
                Err(Refusal::Limited {
                    next_unix: stepped + LIMIT_SECS - 1
                })
            );
            assert!(g.redeem_at(&t.id, Layer::Apn, Some(&boot(699))).is_some());
            assert!(g.redeem_at(&t.id, Layer::Apn, Some(&boot(700))).is_none());
            g.reserve_at(Layer::Apn, None, stepped, Some(&boot(100 + LIMIT_SECS)))
                .unwrap();
        }
    }

    #[test]
    fn same_boot_restart_uses_elapsed_time_even_with_unsynchronised_wall_time() {
        let d = dir();
        let g = open(Some(d.clone()));
        g.reserve_at(Layer::Apn, None, 0, Some(&boot(0))).unwrap();
        let back = Gate::open(Some(d));
        assert_eq!(
            back.reserve_at(Layer::Apn, None, 999_999, Some(&boot(100))),
            Err(Refusal::Limited {
                next_unix: 999_999 + LIMIT_SECS - 100
            })
        );
        back.reserve_at(Layer::Apn, None, 1, Some(&boot(LIMIT_SECS)))
            .unwrap();
    }

    #[test]
    fn legacy_or_rebooted_limits_get_one_persisted_bounded_hold() {
        for legacy in [true, false] {
            let d = dir();
            if legacy {
                fs::write(d.join(LIMIT_FILE), r#"{"apn":1000000}"#).unwrap();
            } else {
                let old = Gate::open(Some(d.clone()));
                old.reserve_at(
                    Layer::Apn,
                    None,
                    1_000_000,
                    Some(&BootTime {
                        boot_id: "old-boot".into(),
                        seconds: 10,
                    }),
                )
                .unwrap();
            }
            let g = Gate::open(Some(d.clone()));
            assert_eq!(
                g.reserve_at(Layer::Apn, None, 1_000, Some(&boot(110))),
                Err(Refusal::Limited {
                    next_unix: 1_000 + LIMIT_SECS
                })
            );
            // Reopening the core does not restart the conservative window.
            let back = Gate::open(Some(d.clone()));
            assert!(matches!(
                back.reserve_at(Layer::Apn, None, 2_000_000, Some(&boot(200))),
                Err(Refusal::Limited { .. })
            ));
            back.reserve_at(Layer::Apn, None, 1_000, Some(&boot(110 + LIMIT_SECS)))
                .unwrap();
            // The old-format file remains a flat map previous binaries can read.
            let old: BTreeMap<String, u64> =
                serde_json::from_slice(&fs::read(d.join(LIMIT_FILE)).unwrap()).unwrap();
            assert_eq!(old["apn"], 1_000);
        }
    }

    #[test]
    fn unknown_clock_corrupt_state_or_companion_save_failure_cannot_authorise() {
        let g = open(Some(dir()));
        assert_eq!(
            g.reserve_at(Layer::Apn, None, 1_000, None),
            Err(Refusal::Clock)
        );
        let t = g
            .reserve_at(Layer::Apn, None, 1_000, Some(&boot(100)))
            .unwrap();
        assert!(g.redeem_at(&t.id, Layer::Apn, None).is_none());
        assert!(g.redeem_at(&t.id, Layer::Apn, Some(&boot(99))).is_none());
        assert!(
            g.redeem_at(
                &t.id,
                Layer::Apn,
                Some(&BootTime {
                    boot_id: "new-boot".into(),
                    seconds: 101
                })
            )
            .is_none()
        );
        let d = dir();
        fs::write(d.join(LIMIT_FILE), "broken").unwrap();
        assert!(matches!(
            reserve(&open(Some(d)), Layer::Apn, 1),
            Err(Refusal::Store(_))
        ));
        let d = dir();
        fs::create_dir_all(d.join(format!(".{LIMIT_CLOCK_FILE}.tmp"))).unwrap();
        let g = open(Some(d.clone()));
        assert!(matches!(reserve(&g, Layer::Apn, 1), Err(Refusal::Store(_))));
        assert_eq!(g.last(Layer::Apn), Some(1));
        assert!(g.lock().tickets.is_empty());
        // Legacy write succeeded but companion write failed. Repairing storage must not
        // release the consumed slot; persist a conservative anchor on the next attempt.
        fs::remove_dir(d.join(format!(".{LIMIT_CLOCK_FILE}.tmp"))).unwrap();
        assert!(matches!(
            reserve(&g, Layer::Apn, 2),
            Err(Refusal::Limited { .. })
        ));
    }

    #[test]
    fn missing_or_mismatched_wall_record_cannot_erase_the_elapsed_limit() {
        let d = dir();
        let g = open(Some(d.clone()));
        reserve(&g, Layer::Apn, 100).unwrap();
        fs::remove_file(d.join(LIMIT_FILE)).unwrap();
        assert!(matches!(
            reserve(&g, Layer::Apn, 101),
            Err(Refusal::Limited { .. })
        ));
        fs::write(d.join(LIMIT_FILE), r#"{"apn":9999999}"#).unwrap();
        assert_eq!(
            reserve(&g, Layer::Apn, 102),
            Err(Refusal::Limited {
                next_unix: 102 + LIMIT_SECS
            })
        );
        assert_eq!(
            reserve(&open(Some(d)), Layer::Apn, 103),
            Err(Refusal::Limited {
                next_unix: 102 + LIMIT_SECS
            })
        );
    }

    #[test]
    fn volatile_gate_and_a_failed_preflight_keep_the_same_limits() {
        let g = open(None);
        let t = reserve(&g, Layer::Apn, 100).unwrap();
        g.done(&t);
        assert!(matches!(
            reserve(&g, Layer::Apn, 101),
            Err(Refusal::Limited { .. })
        ));
        let t = reserve(&g, Layer::Apn, 100 + LIMIT_SECS).unwrap();
        g.release(&t);
        assert_eq!(g.last(Layer::Apn), Some(100));
        let expired = reserve(&g, Layer::Apn, 100 + LIMIT_SECS).unwrap();
        g.release(&t); // replay an old release in the very same elapsed and wall second
        assert_eq!(g.last(Layer::Apn), Some(expired.at));
        assert!(redeem(&g, &expired.id, Layer::Apn, 100 + LIMIT_SECS).is_some());
        let newer = reserve(&g, Layer::Apn, 100 + 2 * LIMIT_SECS).unwrap();
        g.release(&expired);
        assert_eq!(g.last(Layer::Apn), Some(newer.at));
    }

    #[test]
    fn cannot_save_means_no() {
        let d = dir();
        fs::create_dir_all(d.join(LIMIT_FILE)).unwrap(); // 一个目录占着文件名：写不了
        let g = open(Some(d));
        assert!(matches!(reserve(&g, Layer::Apn, 1), Err(Refusal::Store(_))));
    }

    #[test]
    fn roaming_is_sensitive_unless_the_owner_marked_the_card() {
        let cheap = vec!["8944000000000000001".to_string()];
        assert!(sensitive(Some(false), "", &cheap).is_ok());
        assert!(sensitive(None, "8944000000000000001", &cheap).is_err());
        assert!(sensitive(Some(true), "", &cheap).is_err());
        assert!(sensitive(Some(true), "8986000000000000002", &cheap).is_err());
        assert!(sensitive(Some(true), "8944000000000000001F", &cheap).is_ok());
    }

    #[test]
    fn cheap_cards_are_kept_per_card() {
        let g = open(Some(dir()));
        assert_eq!(
            g.set_cheap("8944000000000000001F", true).unwrap(),
            ["8944000000000000001"]
        );
        g.set_cheap("8986000000000000002", true).unwrap();
        assert_eq!(
            g.set_cheap("8944000000000000001", false).unwrap(),
            ["8986000000000000002"]
        );
        assert_eq!(g.cheap(), ["8986000000000000002"]);
        assert!(g.set_cheap("x y", true).is_err());
    }
}
