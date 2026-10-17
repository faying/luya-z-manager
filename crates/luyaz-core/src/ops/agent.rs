//! Agent（MCP）自己批准的写怎么放行（u60-mcp.md「第二期 Decision ledger」D5、D6、D7、D11，Codex O1、O6、O9）。
//!
//! - 白名单（D7、D11）：只有 APN（全部 `apn.*`，含不经事务的 add/delete/set_pdp_type）和 CHILL（档位、出口）
//!   两层；别的动作、`op.keep`/`op.revert` 这类对进行中事务的操作一律拒绝。按**有效**批准者判断
//!   （`source=mcp` 不带 `approved_by` 也算 mcp，O1）。取代第一期 T1「一律拒绝 approved_by=mcp」。
//! - 按层限速（D5）：每层 30 分钟一次，只限 Agent；退回不经这里，不算次数。落盘在 ops 目录的
//!   `agent-limit.json`（Unix 秒），重启不清零；落不了盘就不放行（O6）。一次操作要几步写时（`apn_use`
//!   的候选 APN：先复制成手动、再切过去）拿一张票（[`Ticket`]），后面几步带着票不再计数。
//! - 敏感（D6、O9）：只看 APN 层。漫游中、且这张卡没被人标成「漫游不算贵」→ 敏感，只给网页地址；
//!   漫游状态或卡号读不到也算敏感。按卡设置存在 ops 目录的 `roam-cheap.json`，只有人能改（网页）。
//!   判定时读到的卡号交给事务（`Request::expect_iccid`），写之前再核对一次。

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
const CHEAP_FILE: &str = "roam-cheap.json";
/// 票只在这次操作里用：几步写最多这么久。
const TICKET_SECS: u64 = 10 * 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    Apn,
    Chill,
}

impl Layer {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Apn => "apn",
            Self::Chill => "chill",
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
    CHILL_ACTIONS.contains(&action).then_some(Layer::Chill)
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
    /// 这一层 30 分钟里做过了：下次可做的时刻（Unix 秒）。
    Limited { next_unix: u64 },
    /// 敏感：只给网页地址，交给人。
    Sensitive(String),
    /// 落不了盘（O6）。
    Store(String),
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
            Self::Limited { next_unix } => (
                429,
                "rate_limited",
                "the agent changed this layer less than 30 minutes ago".into(),
                json!({"next_unix": next_unix, "retry_after_s": next_unix.saturating_sub(now_unix)}),
            ),
            Self::Sensitive(why) => (403, "sensitive", why.clone(), json!({"page": "apn"})),
            Self::Store(why) => (503, "store_failed", why.clone(), json!({})),
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
}

#[derive(Default, Serialize, Deserialize)]
struct Limits {
    #[serde(flatten)]
    last: BTreeMap<String, u64>,
}

#[derive(Default, Serialize, Deserialize)]
struct Cheap {
    #[serde(default)]
    cheap: Vec<String>,
}

pub struct Gate {
    dir: Option<PathBuf>,
    st: Mutex<Vec<Ticket>>,
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
            st: Mutex::new(Vec::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Ticket>> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn load(&self) -> Limits {
        self.dir
            .as_ref()
            .and_then(|d| fs::read(d.join(LIMIT_FILE)).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save(&self, l: &Limits) -> Result<(), String> {
        let Some(dir) = &self.dir else { return Ok(()) };
        fs::create_dir_all(dir)
            .and_then(|()| {
                super::pending::write_atomic(
                    dir,
                    LIMIT_FILE,
                    &serde_json::to_vec(l).expect("limits serialize"),
                )
            })
            .map_err(|e| format!("cannot save {LIMIT_FILE}: {e}"))
    }

    /// 这一层上次是什么时候（给读的界面）。
    pub fn last(&self, layer: Layer) -> Option<u64> {
        self.load().last.get(layer.as_str()).copied()
    }

    /// 占这一层的位：30 分钟内做过就回下次时刻；先落盘成功再回票（O6）。
    /// 时钟往回跳了（上次的时刻在一个窗口以后）不算数。
    pub fn reserve(
        &self,
        layer: Layer,
        iccid: Option<String>,
        now: u64,
    ) -> Result<Ticket, Refusal> {
        let mut tickets = self.lock();
        tickets.retain(|t| now.saturating_sub(t.at) < TICKET_SECS);
        let mut l = self.load();
        let prev = l.last.get(layer.as_str()).copied();
        if let Some(at) = prev
            && at <= now + LIMIT_SECS
            && now < at + LIMIT_SECS
        {
            return Err(Refusal::Limited {
                next_unix: at + LIMIT_SECS,
            });
        }
        l.last.insert(layer.as_str().into(), now);
        self.save(&l).map_err(Refusal::Store)?;
        let t = Ticket {
            id: format!("{:016x}", rand::random::<u64>()),
            layer,
            iccid,
            prev,
            at: now,
        };
        tickets.push(t.clone());
        Ok(t)
    }

    /// 带着票的后续一步：票还在、层对得上就放行（不再计数）。
    pub fn redeem(&self, id: &str, layer: Layer, now: u64) -> Option<Ticket> {
        let tickets = self.lock();
        tickets
            .iter()
            .find(|t| t.id == id && t.layer == layer && now.saturating_sub(t.at) < TICKET_SECS)
            .cloned()
    }

    /// 没动设备（忙、被拒绝、参数错）：位置还回去。只在这层的记录还是这张票的时候还。
    pub fn release(&self, t: &Ticket) {
        let mut tickets = self.lock();
        tickets.retain(|x| x.id != t.id);
        let mut l = self.load();
        if l.last.get(t.layer.as_str()) != Some(&t.at) {
            return;
        }
        match t.prev {
            Some(p) => l.last.insert(t.layer.as_str().into(), p),
            None => l.last.remove(t.layer.as_str()),
        };
        if let Err(e) = self.save(&l) {
            eprintln!("ops: agent gate: {e}");
        }
    }

    /// 做完了：票作废（之后的写重新计数）。
    pub fn done(&self, t: &Ticket) {
        self.lock().retain(|x| x.id != t.id);
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
    let sim = crate::state::ubus("zwrt_zte_mdm.api", "get_sim_info", json!({})).await;
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

    fn dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("agent-gate-{:016x}", rand::random::<u64>()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn whitelist_is_apn_and_chill_only() {
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
        for a in [
            "esim.switch",
            "sim.set_slot",
            "network.set_mode",
            "band.reset",
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
    fn one_per_layer_per_half_hour_and_it_survives_a_restart() {
        let d = dir();
        let g = Gate::open(Some(d.clone()));
        let t = g.reserve(Layer::Apn, None, 1_000).unwrap();
        assert_eq!(
            g.reserve(Layer::Apn, None, 1_500),
            Err(Refusal::Limited {
                next_unix: 1_000 + LIMIT_SECS
            })
        );
        // 另一层不受影响
        g.reserve(Layer::Chill, None, 1_500).unwrap();
        // 重启不清零
        let g2 = Gate::open(Some(d.clone()));
        assert!(matches!(
            g2.reserve(Layer::Apn, None, 2_000),
            Err(Refusal::Limited { .. })
        ));
        // 票带着走几步
        assert_eq!(
            g.redeem(&t.id, Layer::Apn, 1_100).map(|x| x.id),
            Some(t.id.clone())
        );
        assert!(g.redeem(&t.id, Layer::Chill, 1_100).is_none());
        assert!(g.redeem("nope", Layer::Apn, 1_100).is_none());
        // 30 分钟后又可以
        g2.reserve(Layer::Apn, None, 1_000 + LIMIT_SECS).unwrap();
    }

    #[test]
    fn released_ticket_gives_the_slot_back() {
        let g = Gate::open(Some(dir()));
        g.reserve(Layer::Apn, None, 100).unwrap();
        let t = g.reserve(Layer::Apn, None, 100 + LIMIT_SECS).unwrap();
        g.release(&t);
        assert_eq!(g.last(Layer::Apn), Some(100));
        assert!(g.redeem(&t.id, Layer::Apn, 100 + LIMIT_SECS).is_none());
        g.reserve(Layer::Apn, None, 100 + LIMIT_SECS).unwrap();
    }

    #[test]
    fn a_clock_that_jumped_back_does_not_lock_forever() {
        let g = Gate::open(Some(dir()));
        g.reserve(Layer::Apn, None, 1_000_000).unwrap();
        g.reserve(Layer::Apn, None, 1_000).unwrap();
    }

    #[test]
    fn cannot_save_means_no() {
        let d = dir();
        fs::create_dir_all(d.join(LIMIT_FILE)).unwrap(); // 一个目录占着文件名：写不了
        let g = Gate::open(Some(d));
        assert!(matches!(
            g.reserve(Layer::Apn, None, 1),
            Err(Refusal::Store(_))
        ));
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
        let g = Gate::open(Some(dir()));
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
