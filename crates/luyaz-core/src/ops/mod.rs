//! E4 写操作层（manager `docs/designs/write-op-layer.md`）：核心里的通用事务引擎。
//!
//! - `txn`：事务状态机（纯逻辑）。
//! - `spec`：动作描述表（T2 只有网络模式）和安全类写的判断。
//! - `probe`：确认用的 DNS 探测，绑定蜂窝接口（T4）。
//! - `pending`：`pending.json` 落盘和 `takeover` 标记。
//! - `record`：流水账 `journal.jsonl` 和 `owners.json`，单一写者（T5）。
//! - `engine`：锁、覆盖/插队、确认驱动、续跑、旧请求队列。
//! - `ui`：给界面的文字和标志（T13：状态文案表、三行进度、撤销、首页「进行中」）。
//! - `write_lock`：跨进程写锁（flock，和应急直写脚本互斥，D29）。
//! - `wifi`、`wifi_dev`：Wi-Fi 设置的两项事务（第四批 4-0b：值和写法、锁和重载确认）。
//! - `wifi_lock`：写 `wireless` 的动作先写锁、再 Wi-Fi 锁（`luyaz_wifi::LOCK_FILE`，D3）。
//!
//! 环境变量：`LUYAZ_OPS_DIR`（落盘目录，默认见 [`ops_dir`]，空 = 不落盘）、
//! `LUYAZ_ROLLBACK`（`1` 才打开自动退回，D30）、`LUYAZ_OP_POLL_MS`、
//! `LUYAZ_LEGACY_TTL_MS`、各动作的 `deadline_env`、`LUYAZ_WRITE_LOCK`；时钟 `LUYAZ_UPTIME_PATH`（默认 `/proc/uptime`）。

pub mod agent;
pub mod apn;
pub mod chill;
pub mod dual_sim;
pub mod engine;
pub mod journal_view;
pub mod pending;
pub mod probe;
pub mod reconnect;
pub mod record;
pub mod spec;
pub mod txn;
pub mod ui;
pub mod wifi;
pub mod wifi_dev;
pub mod wifi_lock;
pub mod write_lock;

use crate::executor::Executor;
use serde_json::{Value, json};
use spec::{
    APN, BAND_ALL, BAND_LTE, BAND_NR, CELL_LTE, CELL_NR, DATA_SESSION, DUAL_SIM, NETWORK_MODE,
    Spec,
};
use std::{path::PathBuf, time::Instant};
use txn::{Conn, DataPath, ProbeTarget, Reading, SimId};

pub use engine::{Config, Engine, LegacyGate, Request, Submit, WriteError};
pub use txn::Source;

pub const OPS_OLD: &str = "/data/u60-ops";
/// Luya-Z 改名 D2：搬家（核心停着时整个目录 mv）之后用这个；在就用它，不在用旧的。
pub const OPS_NEW: &str = "/data/luyaz/core/ops";

pub fn ops_dir() -> Option<PathBuf> {
    match std::env::var("LUYAZ_OPS_DIR") {
        Ok(v) if v.is_empty() => None,
        Ok(v) => Some(v.into()),
        Err(_) => Some(pick_ops("").into()),
    }
}

/// `root` 下活着的 ops 目录（启动时定一次）。
pub fn pick_ops(root: &str) -> String {
    let n = format!("{root}{OPS_NEW}");
    if std::path::Path::new(&n).is_dir() { n } else { format!("{root}{OPS_OLD}") }
}

/// 真机：读写都经 `state::ubus`（执行者里的短任务）。
pub struct UbusDevice {
    exec: Executor,
    started: Instant,
    /// APN 指纹的本机密钥和旧内容保管箱（和 pending.json 同目录）。
    vault: apn::Vault,
    /// Wi-Fi 设置（锁的交接、重载确认、密码保管箱）。
    pub wifi: wifi_dev::WifiDev,
}

impl UbusDevice {
    pub fn new(exec: Executor) -> Self {
        Self {
            exec,
            started: Instant::now(),
            vault: apn::Vault::open(ops_dir()),
            wifi: wifi_dev::WifiDev::new(ops_dir()),
        }
    }
}

fn text(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// 已注册：`network_type` 有值，且不是无服务/受限/紧急（和首页 `screen::rat_of` 认 None 的写法一致）。
pub(crate) fn registered(network_type: &str) -> bool {
    let u = network_type.trim().to_ascii_uppercase();
    !u.is_empty()
        && ![
            "LIMIT",
            "EMERGENCY",
            "NO_SERVICE",
            "NOSERVICE",
            "NO SERVICE",
        ]
        .iter()
        .any(|w| u.contains(w))
}

/// 和首页（`screen.rs`）同一个判断：空、`Home`、`home`、`0` 都不算漫游；空的另外算「不知道」。
pub(crate) fn roaming(net: &Value) -> Option<bool> {
    let r = text(net, "simcard_roam");
    (!r.is_empty()).then(|| r != "Home" && r != "home" && r != "0")
}

/// `connect_status` 是已连接：`ipv4_connected`、`ipv4_ipv6_connected` 这类；`disconnected` 不算
/// （和 agent netwatch 的 `wan_connected` 一致）。
fn connected(status: &str) -> bool {
    status.ends_with("connected") && !status.contains("disconnect")
}

/// 移动数据开关。`enable` 是最近一次写进去的值，开机后默认 0，自动拨号照样连上（T12 真机，B31），
/// 所以 0 但正在连或已连上也算开着；0 且断着才算关了；状态读空不猜。
pub(crate) fn data_switch(wwan: &Value) -> Option<bool> {
    if flag(wwan, "enable")? {
        return Some(true);
    }
    let status = text(wwan, "connect_status");
    if status.is_empty() {
        return None;
    }
    Some(connected(&status) || status == "connecting")
}

fn flag(v: &Value, key: &str) -> Option<bool> {
    if let Some(b) = v.get(key).and_then(Value::as_bool) {
        return Some(b);
    }
    match text(v, key).as_str() {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    }
}

fn strings(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn first_address(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .map(|a| text(a, "address"))
        .unwrap_or_default()
}

/// 数据通路：`get_wwaniface`（开关、连接状态、接口）+ netifd 的 `zte_wan`（IPv4、连上多久、DNS）+
/// nwinfo 的漫游。`now_ms` 用来把 uptime 秒数换成连接的起点。
/// `zte_wan` 没有 IPv4 时（D41）看 `wan6`（netifd 的 `zte_wan6`）：有 IPv6 地址就用它当连接身份，
/// 连上多久、DNS 都按 `zte_wan6` 的（DNS 没有时用 `get_wwaniface` 的 ipv6_dns_prefer/standby），
/// 接口用 ipv6_dev_name（空时用 `zte_wan6` 的 l3_device）。
fn data_path(
    net: &Value,
    wwan: &Value,
    wan: &Value,
    wan6: Option<&Value>,
    uci_dns: Option<String>,
    now_ms: u64,
) -> DataPath {
    let expected = match (data_switch(wwan), flag(wwan, "roam_enable"), roaming(net)) {
        (Some(false), _, _) => Some(false),
        (Some(true), _, Some(false)) => Some(true),
        (Some(true), Some(roam_on), Some(true)) => Some(roam_on),
        _ => None,
    };
    let up_since = |v: &Value| {
        v.get("uptime")
            .and_then(Value::as_u64)
            .map(|secs| now_ms.saturating_sub(secs * 1000))
    };
    let connected = connected(&text(wwan, "connect_status"));
    let ipv4 = first_address(wan, "ipv4-address");
    if ipv4.is_empty()
        && let Some(wan6) = wan6
    {
        let ipv6 = first_address(wan6, "ipv6-address");
        if !ipv6.is_empty() {
            let mut iface = text(wwan, "ipv6_dev_name");
            if iface.is_empty() {
                iface = text(wan6, "l3_device");
            }
            let mut dns = strings(wan6, "dns-server");
            if dns.is_empty() {
                dns = ["ipv6_dns_prefer", "ipv6_dns_standby"]
                    .iter()
                    .map(|k| text(wwan, k))
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            return DataPath {
                expected,
                connected,
                conn: Conn {
                    ipv6,
                    up_since_ms: up_since(wan6),
                    ..Conn::default()
                },
                iface,
                dns,
            };
        }
    }
    let up_since_ms = up_since(wan);
    let mut iface = text(wwan, "ipv4_dev_name");
    if iface.is_empty() {
        iface = text(wan, "l3_device");
    }
    let mut dns = strings(wan, "dns-server");
    if dns.is_empty() {
        // uci 里是 `'a' 'b'` 或空格分开的一串。
        dns = uci_dns
            .unwrap_or_default()
            .split(|c: char| c == '\'' || c.is_whitespace())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
    }
    DataPath {
        expected,
        connected,
        conn: Conn {
            ipv4,
            up_since_ms,
            ..Conn::default()
        },
        iface,
        dns,
    }
}

fn sim_id(sim: &Value) -> SimId {
    SimId {
        iccid: text(sim, "sim_iccid"),
        slot: text(sim, "current_sim_slot").parse().unwrap_or(0),
    }
}

impl UbusDevice {
    /// APN 的提交检查（见 `ops::apn`）：记下正在用的内容（退回/撤销用）、撤销前核对现状、
    /// 改的不是正在用的那条就不算事务、自动模式下不启用手动 profile、旧内容没留就不撤。
    async fn resolve_apn(
        &self,
        spec: &'static Spec,
        target: Value,
        params: &Value,
    ) -> Result<Option<Value>, String> {
        let now = apn_now().await?.ok_or("cannot read APN settings")?;
        let cur = now.value(&self.vault);
        if let Some(c) = &now.content {
            self.vault.keep(&now.manual, c);
        }
        let restore = params.get("restore").is_some();
        if let Some(expect) = params.get("expect")
            && !txn::matches(expect, &cur)
        {
            return Err("APN settings changed since; not undoing".into());
        }
        let id = target
            .get("manual")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if target.get("modify") == Some(&Value::Bool(true)) {
            if now.mode != 1 || now.manual != id {
                return Ok(None);
            }
            let base = now.content_of(&id).cloned().unwrap_or(Value::Null);
            let c = apn::content_from_params(params, &base);
            self.vault.keep(&id, &c);
            return Ok(Some(
                json!({"mode": 1, "manual": id, "fp": self.vault.fp(&c)}),
            ));
        }
        if !restore {
            if spec.action == "apn.enable" && now.mode == 0 {
                // 自动模式下手动表不生效，启用不碰连接：不算事务（agent 的 dial_manual 先启用、再切手动，
                // 接着的切手动才是事务）
                return Ok(None);
            }
            if txn::matches(&target, &cur) {
                // 已经是这样：不算事务（新连接这一关永远等不到）
                return Ok(None);
            }
        }
        if let Some(fp) = target
            .get("fp")
            .and_then(Value::as_str)
            .filter(|f| !f.is_empty())
        {
            let same = now.content_of(&id).is_some_and(|c| self.vault.fp(c) == fp);
            if !same && self.vault.get(fp).is_none() {
                return Err("the old APN settings were not kept on the device".into());
            }
        }
        Ok(Some(target))
    }
}

impl UbusDevice {
    /// 读不到 `get_wwaniface` 就是 None（这一拍不判断数据）；`zte_wan` 也读不到时，`zte_wan6` 有 IPv6
    /// 地址就按只有 IPv6 的连接算，否则 None。`zte_wan6` 只在 `zte_wan` 没有 IPv4 时才读（D41）。
    async fn data(&self, net: &Value) -> Option<DataPath> {
        let wwan = crate::state::ubus(
            "zwrt_data",
            "get_wwaniface",
            json!({"source_module":"web","cid":1,"connect_status":""}),
        )
        .await
        .ok()?;
        let wan = crate::state::ubus("network.interface.zte_wan", "status", json!({}))
            .await
            .ok();
        let has_ipv4 = wan
            .as_ref()
            .is_some_and(|w| !first_address(w, "ipv4-address").is_empty());
        let wan6 = if has_ipv4 {
            None
        } else {
            crate::state::ubus("network.interface.zte_wan6", "status", json!({}))
                .await
                .ok()
                .filter(|w| !first_address(w, "ipv6-address").is_empty())
        };
        if wan6.is_some() {
            return Some(data_path(
                net,
                &wwan,
                wan.as_ref().unwrap_or(&json!({})),
                wan6.as_ref(),
                None,
                engine::Device::now_ms(self),
            ));
        }
        let wan = wan?;
        let no_dns = wan
            .get("dns-server")
            .and_then(Value::as_array)
            .is_none_or(|a| a.is_empty());
        let uci_dns = if no_dns {
            Some(crate::state::uci_read("network.zte_wan.dns").await)
        } else {
            None
        };
        Some(data_path(
            net,
            &wwan,
            &wan,
            None,
            uci_dns,
            engine::Device::now_ms(self),
        ))
    }
}

/// 这一项的读回，和 `Spec::normalize` 同一套写法；空的为 None。
fn readback(item: &str, net: &Value) -> Option<Value> {
    let b = |k: &str| spec::bands(&text(net, k)).filter(|v| !v.is_empty());
    match item {
        NETWORK_MODE => Some(text(net, "net_select"))
            .filter(|v| !v.is_empty())
            .map(|v| Value::String(spec::canonical_mode(&v).to_string())),
        BAND_LTE => b("lte_band").map(Value::String),
        BAND_NR => Some(json!({"sa": b("nr5g_sa_band_lock")?, "nsa": b("nr5g_nsa_band_lock")?})),
        CELL_LTE => cell(net, "lock_lte_cell", &["pci", "earfcn"]),
        CELL_NR => cell(net, "lock_nr_cell", &["pci", "arfcn", "band"]),
        BAND_ALL => Some(json!({
            "lte": b("lte_band")?,
            "sa": b("nr5g_sa_band_lock")?,
            "nsa": b("nr5g_nsa_band_lock")?,
            "lte_cell": readback(CELL_LTE, net)?,
            "nr_cell": readback(CELL_NR, net)?,
        })),
        _ => None,
    }
}

/// 小区锁读回：`lock_nr_cell` = `pci,arfcn,band`（B31 真机 10-12）；`lock_lte_cell` 按同样的顺序
/// 推断为 `pci,earfcn`（**未真机确认**：原厂网页没有 LTE 锁小区，设备在 SA 测不了，用户 10-12 定先这样）。
/// 空的或全是 0（开机是空串，恢复默认后是 `0,0,0`）= 没锁；段数不对、不是数字为 None。
fn cell(net: &Value, key: &str, names: &[&str]) -> Option<Value> {
    let raw = text(net, key);
    let parts: Vec<&str> = raw.split(',').map(str::trim).collect();
    if raw.trim().is_empty() || parts.iter().all(|p| p.parse::<u64>() == Ok(0)) {
        return Some(spec::cell_off());
    }
    if parts.len() != names.len() {
        return None;
    }
    let mut m = serde_json::Map::new();
    for (k, p) in names.iter().zip(parts) {
        m.insert(
            (*k).into(),
            Value::String(p.parse::<u64>().ok()?.to_string()),
        );
    }
    Some(Value::Object(m))
}

/// 原厂 default_band_lock 里的三组（未锁时 *_band_lock 读回正好等于它们，B31 真机）。
async fn default_bands() -> Result<Value, String> {
    let mut v = serde_json::Map::new();
    for (k, opt) in [
        ("lte", "default_lte_ext_band_lock"),
        ("sa", "default_nr5g_sa_band_lock"),
        ("nsa", "default_nr5g_nsa_band_lock"),
    ] {
        let raw = crate::state::uci_read(&format!("zwrt_zte_nwinfo.default_band_lock.{opt}")).await;
        match spec::bands(&raw) {
            Some(b) if !b.is_empty() => v.insert(k.into(), Value::String(b)),
            _ => return Err(format!("cannot read default bands: {opt}")),
        };
    }
    Ok(Value::Object(v))
}

/// APN 现状的一次新鲜读（不经缓存）；回复读不懂为 None。
async fn apn_now() -> Result<Option<apn::Now>, String> {
    let o = "zwrt_apn_object";
    let mode = crate::state::ubus(o, "get_apn_mode", json!({})).await?;
    let manual = crate::state::ubus(o, "get_manu_apn_list", json!({})).await?;
    Ok(apn::Now::parse(&mode, &manual))
}

/// 恢复上次验证过的连接（`crate::verified`）用的现状：卡、各项读回（写法和事务读回一样）、
/// 手动 APN 表的 (编号, 指纹)。一次新鲜读；读不到的项不放进去。
pub struct Current {
    pub iccid: String,
    pub items: std::collections::BTreeMap<String, Value>,
    pub apn_list: Vec<(String, String)>,
}

pub async fn current() -> Result<Current, String> {
    let net = crate::state::ubus("zte_nwinfo_api", "nwinfo_get_netinfo", json!({})).await?;
    let sim = crate::state::sim_info().await?;
    let mut items = std::collections::BTreeMap::new();
    for item in [CELL_LTE, CELL_NR, BAND_LTE, BAND_NR, NETWORK_MODE] {
        if let Some(v) = readback(item, &net) {
            items.insert(item.to_owned(), v);
        }
    }
    let mut apn_list = Vec::new();
    if let Some(n) = apn_now().await? {
        let vault = apn::Vault::open(ops_dir());
        items.insert(APN.to_owned(), n.value(&vault));
        apn_list = n.list.iter().map(|(id, c)| (id.clone(), vault.fp(c))).collect();
    }
    Ok(Current {
        iccid: sim_id(&sim).iccid,
        items,
        apn_list,
    })
}

type Step = (&'static str, &'static str, Value);

const RESET: (&str, &str) = ("zte_nwinfo_api", "nwinfo_reset_band_cell_setting");

/// 原厂一键恢复之后把 `keep` 里不是默认的频段、锁着的小区写回去（解锁小区、撤销恢复默认都用）。
fn restore_steps(keep: &Value, defaults: &Value) -> Vec<Step> {
    let mut steps = vec![(RESET.0, RESET.1, json!({}))];
    for k in ["lte", "sa", "nsa"] {
        if let Some(v) = keep.get(k).and_then(Value::as_str)
            && Some(v) != defaults.get(k).and_then(Value::as_str)
        {
            steps.push(crate::control::band_call(k, v));
        }
    }
    for (k, item) in [("lte_cell", CELL_LTE), ("nr_cell", CELL_NR)] {
        if let Some(c) = keep.get(k).filter(|c| **c != spec::cell_off()) {
            steps.push(cell_call(item, c));
        }
    }
    steps
}

fn cell_call(item: &str, c: &Value) -> Step {
    let s = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or_default();
    if item == CELL_LTE {
        (
            "zte_nwinfo_api",
            "nwinfo_lock_lte_cell",
            json!({"lock_lte_pci": s("pci"), "lock_lte_earfcn": s("earfcn")}),
        )
    } else {
        (
            "zte_nwinfo_api",
            "nwinfo_lock_nr_cell",
            json!({"lock_nr_pci": s("pci"), "lock_nr_earfcn": s("arfcn"), "lock_nr_cell_band": s("band")}),
        )
    }
}

/// 写这个值要不要先知道设备现状（`now` = 现在的 band.all 读回，`defaults` = 默认三组频段）。
fn needs_state(item: &str, value: &Value, params: &Value) -> bool {
    match item {
        CELL_LTE | CELL_NR => *value == spec::cell_off(),
        BAND_ALL => !params.as_object().is_none_or(|m| m.is_empty()),
        _ => false,
    }
}

/// 把这一项写成 `value` 要调的 ubus（依次）。`band.reset {}`（原始参数是空的）走原厂一键恢复，
/// 同时清小区锁（D22）；退回、撤销带的是三组频段，逐组写回。
fn write_steps(
    item: &str,
    value: &Value,
    params: &Value,
    state: Option<(&Value, &Value)>,
) -> Result<Vec<Step>, String> {
    if needs_state(item, value, params) {
        let (now, defaults) = state.ok_or("current bands not read")?;
        let keep = match item {
            // 解锁一种小区：其余照现状保留
            CELL_LTE | CELL_NR => {
                let mut k = now.clone();
                k[if item == CELL_LTE {
                    "lte_cell"
                } else {
                    "nr_cell"
                }] = spec::cell_off();
                k
            }
            _ => value.clone(),
        };
        return Ok(restore_steps(&keep, defaults));
    }
    let s = |k: &str| value.get(k).and_then(Value::as_str).unwrap_or_default();
    Ok(match item {
        NETWORK_MODE => vec![(
            "zte_nwinfo_api",
            "nwinfo_set_netselect",
            json!({"net_select": value}),
        )],
        BAND_LTE => vec![crate::control::band_call(
            "lte",
            value.as_str().unwrap_or_default(),
        )],
        BAND_NR => vec![
            crate::control::band_call("sa", s("sa")),
            crate::control::band_call("nsa", s("nsa")),
        ],
        BAND_ALL => vec![(RESET.0, RESET.1, json!({}))],
        CELL_LTE | CELL_NR => vec![cell_call(item, value)],
        other => return Err(format!("no writer for {other}")),
    })
}

impl engine::Device for UbusDevice {
    async fn read(&self, spec: &'static Spec) -> Result<Reading, String> {
        if let Some(kind) = wifi::Kind::of(spec.item) {
            return self.wifi.read(kind).await;
        }
        if matches!(spec.item, chill::PROFILE | chill::EXIT) {
            let value = if spec.item == chill::PROFILE {
                chill::read_profile()
            } else {
                Some(chill::read_exit().await?)
            };
            return Ok(Reading {
                value,
                registered: chill::up().await,
                // 换卡不影响 CHILL：SIM 固定为空（sim_check 当成同一张）
                sim: SimId {
                    iccid: String::new(),
                    slot: 0,
                },
                data: None,
                probe: None,
            });
        }
        if spec.item == DUAL_SIM {
            let net = crate::state::ubus("zte_nwinfo_api", "nwinfo_get_netinfo", json!({})).await?;
            let sim = crate::state::sim_info().await?;
            let smart = crate::state::uci_value(dual_sim::SMART_UCI).await;
            return Ok(Reading {
                value: dual_sim::readback(&sim, &smart),
                registered: registered(&text(&net, "network_type")),
                // 切主卡本来就换 ICCID：SIM 固定为空，sim_check 不取消（D1），由读回 + 数据通确认
                sim: SimId {
                    iccid: String::new(),
                    slot: 0,
                },
                data: self.data(&net).await,
                probe: None,
            });
        }
        if !matches!(
            spec.item,
            NETWORK_MODE | BAND_LTE | BAND_NR | BAND_ALL | CELL_LTE | CELL_NR | APN | DATA_SESSION
        ) {
            return Err(format!("no reader for {}", spec.item));
        }
        let net = crate::state::ubus("zte_nwinfo_api", "nwinfo_get_netinfo", json!({})).await?;
        let sim = crate::state::sim_info().await?;
        let value = match spec.item {
            APN => apn_now().await?.map(|n| n.value(&self.vault)),
            DATA_SESSION => Some(Value::from(reconnect::read_session().await?)),
            item => readback(item, &net),
        };
        Ok(Reading {
            value,
            registered: registered(&text(&net, "network_type")),
            sim: sim_id(&sim),
            data: self.data(&net).await,
            probe: None,
        })
    }

    async fn resolve(
        &self,
        spec: &'static Spec,
        target: Value,
        params: &Value,
    ) -> Result<Option<Value>, String> {
        if let Some(kind) = wifi::Kind::of(spec.item) {
            return self.wifi.resolve(kind, target, params).await;
        }
        if spec.item == APN {
            return self.resolve_apn(spec, target, params).await;
        }
        if spec.item == DUAL_SIM {
            let sim = crate::state::sim_info().await?;
            let i1 = crate::state::uci_value("zwrt_zte_mdm.sim_info.simcard_0_iccid").await;
            let i2 = crate::state::uci_value("zwrt_zte_mdm.sim_info.simcard_1_iccid").await;
            dual_sim::precheck(&sim, [&i1, &i2], &target)?;
            return Ok(Some(target));
        }
        if spec.item != BAND_ALL || target != spec::reset_marker() {
            return Ok(Some(target));
        }
        // 恢复默认 = 默认三组频段 + 两种小区都不锁
        let mut v = default_bands().await?;
        v["lte_cell"] = spec::cell_off();
        v["nr_cell"] = spec::cell_off();
        Ok(Some(v))
    }

    async fn write(
        &self,
        spec: &'static Spec,
        value: &Value,
        params: &Value,
        _strict: bool,
    ) -> Result<Value, WriteError> {
        // Wi-Fi、CHILL 各走自己的锁；下面的 ubus 写一律拿不到写锁就不写（D7，闭环、人手动一样）
        if let Some(kind) = wifi::Kind::of(spec.item) {
            return self.wifi.write(&self.exec, kind, value, params).await;
        }
        if matches!(spec.item, chill::PROFILE | chill::EXIT) {
            let want = value.as_str().unwrap_or_default();
            let r = if spec.item == chill::PROFILE {
                chill::write_profile(want).await
            } else {
                chill::write_exit(want).await
            };
            return r.map_err(|message| WriteError {
                message,
                timed_out: false,
            });
        }
        if spec.item == DATA_SESSION {
            // 闭环专用（`ops::reconnect`）：同样拿着写锁写，拿不到就不写
            let r = write_lock::run(&self.exec, async move {
            reconnect::redial(|a| async move {
                crate::executor::call("zwrt_qcmap_cli", "set_qcliiface", &a)
                    .await
                    .map_err(|e| e.to_string())
            })
            .await
        })
        .await;
        crate::state::invalidate_cache();
        return match r {
            Ok(Ok(v)) => {
                self.exec.mark_immediate(None);
                Ok(v)
            }
            Ok(Err(message)) => Err(WriteError {
                message,
                timed_out: false,
            }),
            Err(e) => Err(WriteError {
                message: e.to_string(),
                timed_out: false,
            }),
        };
    }
    let apn = if spec.item == APN {
        let now = apn_now()
            .await
            .and_then(|n| n.ok_or_else(|| "cannot read APN settings".to_owned()))
            .map_err(|message| WriteError {
                message,
                timed_out: false,
            })?;
        // 新写的内容只在这次 apn.modify 的原始参数里；退回、撤销（{restore}）从保管箱取
        let new = (spec.action == "apn.modify" && params.get("restore").is_none()).then(|| {
            let id = value.get("manual").and_then(Value::as_str).unwrap_or("");
            let base = now.content_of(id).cloned().unwrap_or(Value::Null);
            apn::content_from_params(params, &base)
        });
        Some(
            apn::steps(value, &now, new.as_ref(), &self.vault).map_err(|message| {
                WriteError {
                    message,
                    timed_out: false,
                }
            })?,
        )
    } else {
        None
    };
    let apn = if spec.item == DUAL_SIM {
        let sim = crate::state::sim_info()
            .await
            .and_then(|sim| dual_sim::steps(value, &sim))
            .map_err(|message| WriteError {
                message,
                timed_out: false,
            })?;
        Some(sim)
    } else {
        apn
    };
    let state = if apn.is_none() && needs_state(spec.item, value, params) {
        let net = crate::state::ubus("zte_nwinfo_api", "nwinfo_get_netinfo", json!({}))
            .await
            .map_err(|message| WriteError {
                message,
                timed_out: false,
            })?;
        let now = readback(BAND_ALL, &net).ok_or_else(|| WriteError {
            message: "cannot read current bands and cell locks".into(),
            timed_out: false,
        })?;
        let defaults = default_bands().await.map_err(|message| WriteError {
            message,
            timed_out: false,
        })?;
        Some((now, defaults))
    } else {
        None
    };
    let steps = match apn.map(Ok).unwrap_or_else(|| {
        write_steps(
            spec.item,
            value,
            params,
            state.as_ref().map(|(n, d)| (n, d)),
        )
    }) {
        Ok(v) => v,
        Err(message) => {
            return Err(WriteError {
                message,
                timed_out: false,
            });
        }
    };
    // 一个执行者任务：在执行者里拿跨进程写锁（D29）再依次调，锁不会和别的任务互等；
    // 几步的（5G 锁频、恢复默认的撤销）中途失败就停，结果交给读回判断。
    // 拿不到写锁（被占满 15 秒或打不开）就什么都不调（D7：人手动的、闭环的一样）
    let r = write_lock::run(&self.exec, async move {
            let mut last = Ok(Value::Null);
            for (object, method, args) in steps {
                // nwinfo_set_netselect 成功时什么都不回（T11 真机，B31）
                last =
                    crate::control::write_reply(crate::executor::call(object, method, &args).await);
                if last.is_err() {
                    break;
                }
            }
            last
        })
        .await;
        let r = r.map_err(|e| WriteError {
            message: e.to_string(),
            timed_out: false,
        })?;
        // 和 `/control` 一样：写过之后慢数据缓存作废，全部块下一轮立即读。
        crate::state::invalidate_cache();
        match r {
            Ok(v) => {
                self.exec.mark_immediate(None);
                Ok(v)
            }
            Err(e) => Err(WriteError {
                // D28：我们这边超时不代表原厂那边没做，结果未知，交给读回判断。
                timed_out: matches!(e, crate::ubus::client::UbusError::Timeout { .. }),
                message: e.to_string(),
            }),
        }
    }

    async fn probe(&self, target: &ProbeTarget) -> Result<(), txn::ProbeErr> {
        if target.chill {
            return match chill::probe().await {
                chill::Probe::Up => Ok(()),
                p => Err(format!("CHILL path {p:?}").into()),
            };
        }
        probe::dns(target).await
    }

    fn now_ms(&self) -> u64 {
        uptime_read().unwrap_or_else(|| self.started.elapsed().as_millis() as u64)
    }

    fn boot_id(&self) -> String {
        boot_id()
    }
}

fn uptime_read() -> Option<u64> {
    let path = std::env::var("LUYAZ_UPTIME_PATH").unwrap_or_else(|_| "/proc/uptime".into());
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
        .map(|secs| (secs * 1000.0) as u64)
}

/// BOOTTIME ms as the engine reads it (0 = unreadable; the gate then sees an old hold).
pub(crate) fn uptime_ms() -> u64 {
    uptime_read().unwrap_or(0)
}

pub(crate) fn boot_id() -> String {
    let path = std::env::var("LUYAZ_BOOT_ID_PATH")
        .unwrap_or_else(|_| "/proc/sys/kernel/random/boot_id".into());
    std::fs::read_to_string(path)
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D2：新目录在就用新的（整个目录搬，不按文件），不在用旧的。
    #[test]
    fn ops_dir_new_when_moved() {
        let r = std::env::temp_dir().join(format!("ops-pick-{:016x}", rand::random::<u64>()));
        let r = r.to_str().unwrap();
        assert_eq!(pick_ops(r), format!("{r}/data/u60-ops"));
        std::fs::create_dir_all(format!("{r}/data/luyaz/core/ops")).unwrap();
        assert_eq!(pick_ops(r), format!("{r}/data/luyaz/core/ops"));
        let _ = std::fs::remove_dir_all(r);
    }

    /// B31 真机 nwinfo_get_netinfo 的锁频字段（10-12，没锁时）。
    fn net_sample() -> Value {
        json!({
            "net_select": "TCHGWL_5G",
            "lte_band": "1,2,3,4,5,7,8,18,19,20,26,28,29,32,34,38,39,40,41,42,43,48,66,71",
            "nr5g_sa_band_lock": "1,2,3,5,7,8,18,20,26,28,29,38,40,41,48,66,71,75,77,78,79",
            "nr5g_nsa_band_lock": "1,2,3,5,7,8,18,20,26,28,29,38,40,41,48,66,71,75,77,78,79",
        })
    }

    #[test]
    fn band_readbacks_match_normalized_targets() {
        let net = net_sample();
        assert_eq!(readback(NETWORK_MODE, &net), Some(json!("WL_AND_5G")));
        let lte = spec::find("band.set_lte").unwrap();
        assert_eq!(
            readback(BAND_LTE, &net),
            lte.target(&json!({"bands": text(&net, "lte_band")})).ok()
        );
        let nr = spec::find("band.set_nr").unwrap();
        assert_eq!(
            readback(BAND_NR, &net),
            nr.target(&json!({"sa": text(&net, "nr5g_sa_band_lock"),
                              "nsa": text(&net, "nr5g_nsa_band_lock")}))
                .ok()
        );
        // 锁到 n78 后读回是 "78"，参数顺序乱、重复也对得上
        let locked = json!({"nr5g_sa_band_lock": "78", "nr5g_nsa_band_lock": "78,41"});
        assert_eq!(
            readback(BAND_NR, &locked),
            nr.target(&json!({"sa":"78","nsa":"41,78,41"})).ok()
        );
        // 读不到的为 None（不当成第三个值）
        assert_eq!(readback(BAND_NR, &json!({"nr5g_sa_band_lock": "78"})), None);
        assert_eq!(readback(BAND_LTE, &json!({})), None);
        let all = readback(BAND_ALL, &net).unwrap();
        assert_eq!(all["lte"], readback(BAND_LTE, &net).unwrap());
    }

    #[test]
    fn apn_block_hides_passwords() {
        let manual = json!({"apnListArray": [
            {"profilename":"Japan","wanapn":"plus.4g","username":"u","password":"secret","pdpType":3,
             "pppAuthMode":0,"profileId":"manu2","isEnable":false},
            {"profilename":"AU","wanapn":"x.au-net.ne.jp","username":"","password":"","pdpType":3,
             "pppAuthMode":2,"profileId":"manu3","isEnable":true}]});
        let auto = json!({"apnListArray": [{"profilename":"China Telecom","wanapn":"ctiot",
            "password":"","pdpType":3,"pppAuthMode":0,"profileId":"auto109590","isEnable":true}]});
        let b = crate::project::snapshot::apn_block(&[
            json!({"apn_mode": 0}),
            json!({"profileId": "auto109590"}),
            manual,
            auto,
        ]);
        let v = serde_json::to_value(&b).unwrap();
        assert_eq!(
            (v["mode"].clone(), v["current"].clone()),
            (json!(0), json!("auto109590"))
        );
        assert_eq!(v["manual"][0]["has_password"], true);
        assert_eq!(v["manual"][1]["has_password"], false);
        assert!(
            !v.to_string().contains("secret"),
            "password never leaves the device"
        );
    }

    #[test]
    fn cell_readbacks() {
        let n = |lte: &str, nr: &str| json!({"lock_lte_cell": lte, "lock_nr_cell": nr});
        // B31 真机：开机 "" / "0,0"，锁上 "603,627264,78"，恢复默认后 "0,0,0"
        assert_eq!(readback(CELL_NR, &n("0,0", "")), Some(spec::cell_off()));
        assert_eq!(
            readback(CELL_NR, &n("0,0", "0,0,0")),
            Some(spec::cell_off())
        );
        assert_eq!(readback(CELL_LTE, &n("0,0", "")), Some(spec::cell_off()));
        let nr = spec::find("cell.lock_nr").unwrap();
        assert_eq!(
            readback(CELL_NR, &n("0,0", "603,627264,78")),
            nr.target(&json!({"pci":"603","arfcn":"627264","band":"78"}))
                .ok()
        );
        assert_eq!(
            readback(CELL_LTE, &n("12,1650", "")),
            Some(json!({"pci":"12","earfcn":"1650"}))
        );
        assert_eq!(readback(CELL_NR, &n("0,0", "603,1")), None);
    }

    #[test]
    fn unlocking_a_cell_restores_the_rest() {
        let now = json!({"lte":"1,3","sa":"78","nsa":"1,78",
            "lte_cell":{"pci":"12","earfcn":"1650"},
            "nr_cell":{"pci":"603","arfcn":"627264","band":"78"}});
        let d = json!({"lte":"1,3","sa":"1,78","nsa":"1,78"});
        let off = spec::cell_off();
        let r = write_steps(CELL_NR, &off, &off, Some((&now, &d))).unwrap();
        let methods: Vec<&str> = r.iter().map(|s| s.1).collect();
        assert_eq!(
            methods,
            [
                "nwinfo_reset_band_cell_setting",
                "nwinfo_set_nrbandlock",
                "nwinfo_lock_lte_cell"
            ]
        );
        assert_eq!(r[1].2, json!({"nr5g_type":"0","nr5g_band":"78"}));
        assert!(write_steps(CELL_NR, &off, &off, None).is_err());
        let lock = json!({"pci":"603","arfcn":"627264","band":"78"});
        assert_eq!(
            write_steps(CELL_NR, &lock, &lock, None).unwrap()[0].2,
            json!({"lock_nr_pci":"603","lock_nr_earfcn":"627264","lock_nr_cell_band":"78"})
        );
    }

    #[test]
    fn band_write_steps() {
        let call = |o: &str, m: &str, a: Value| (o.to_owned(), m.to_owned(), a);
        let steps = |item, v: Value, p: Value| {
            write_steps(item, &v, &p, None)
                .unwrap()
                .into_iter()
                .map(|(o, m, a)| call(o, m, a))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            steps(BAND_LTE, json!("1,3"), json!({"bands":"3,1"})),
            [call(
                "zte_nwinfo_api",
                "nwinfo_set_lte_ext_band",
                json!({"lte_band":"1,3"})
            )]
        );
        let nr = json!({"sa":"78","nsa":"41,78"});
        assert_eq!(
            steps(BAND_NR, nr.clone(), nr),
            [
                call(
                    "zte_nwinfo_api",
                    "nwinfo_set_nrbandlock",
                    json!({"nr5g_type":"0","nr5g_band":"78"})
                ),
                call(
                    "zte_nwinfo_api",
                    "nwinfo_set_nrbandlock",
                    json!({"nr5g_type":"1","nr5g_band":"41,78"})
                ),
            ]
        );
        // 恢复默认：原始参数是空的 → 原厂一键恢复；退回/撤销带三组 → 逐组写
        let all = json!({"lte":"1,3","sa":"78","nsa":"78"});
        assert_eq!(
            steps(BAND_ALL, all.clone(), json!({})),
            [call(
                "zte_nwinfo_api",
                "nwinfo_reset_band_cell_setting",
                json!({})
            )]
        );
        let d = json!({"lte":"1,3","sa":"1,78","nsa":"1,78"});
        let r = write_steps(BAND_ALL, &all, &all, Some((&all, &d))).unwrap();
        // 恢复默认 + 两组不是默认的 5G；LTE 已是默认不写
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].1, "nwinfo_reset_band_cell_setting");
        assert!(write_steps("apn.x", &json!({}), &json!({}), None).is_err());
    }

    #[test]
    fn registration_words() {
        for t in ["SA", "NSA", "LTE", "WCDMA", "ENDC"] {
            assert!(registered(t), "{t}");
        }
        for t in [
            "",
            " ",
            "No Service",
            "NO_SERVICE",
            "Limited Service",
            "EMERGENCY",
        ] {
            assert!(!registered(t), "{t}");
        }
    }

    #[test]
    fn connected_words() {
        for s in ["ipv4_connected", "ipv6_connected", "ipv4_ipv6_connected"] {
            assert!(connected(s), "{s}");
        }
        for s in ["", "disconnected", "ipv4_disconnected", "connecting"] {
            assert!(!connected(s), "{s}");
        }
    }

    fn wwan(enable: i64, roam: i64) -> Value {
        // 写 0 关数据之后读到的就是断开（T12）
        let status = if enable == 1 {
            "ipv4_ipv6_connected"
        } else {
            "disconnected"
        };
        json!({"enable":enable,"roam_enable":roam,"connect_status":status,"ipv4_dev_name":"rmnet_data0"})
    }

    #[test]
    fn data_switch_reads_a_boot_default_enable_as_on() {
        let w = |e: Value, s: &str| data_switch(&json!({"enable":e,"connect_status":s}));
        assert_eq!(w(json!(1), "disconnected"), Some(true));
        // 开机后没人写过：enable 0，自动拨号连着（10-04 真机）
        assert_eq!(w(json!(0), "ipv4_ipv6_connected"), Some(true));
        assert_eq!(w(json!(0), "connecting"), Some(true));
        assert_eq!(w(json!(0), "disconnected"), Some(false));
        assert_eq!(w(json!(0), "disconnecting"), Some(false));
        assert_eq!(w(json!(0), ""), None);
        assert_eq!(
            data_switch(&json!({"connect_status":"ipv4_connected"})),
            None
        );
    }

    fn wan() -> Value {
        json!({"uptime":30,"l3_device":"rmnet_data9","ipv4-address":[{"address":"10.1.2.3","mask":30}],"dns-server":["192.0.2.53","2001:db8::53"]})
    }

    #[test]
    fn data_is_expected_only_with_the_switches_that_allow_it() {
        let home = json!({"simcard_roam":"Home"});
        let away = json!({"simcard_roam":"Roaming"});
        let unknown = json!({});
        let e = |net: &Value, w: Value| data_path(net, &w, &wan(), None, None, 100_000).expected;
        assert_eq!(e(&home, wwan(1, 0)), Some(true));
        assert_eq!(e(&home, wwan(0, 1)), Some(false));
        assert_eq!(e(&away, wwan(1, 0)), Some(false));
        assert_eq!(e(&away, wwan(1, 1)), Some(true));
        assert_eq!(e(&away, wwan(0, 1)), Some(false));
        // 不知道在不在漫游：不猜。
        assert_eq!(e(&unknown, wwan(1, 0)), None);
        assert_eq!(e(&home, json!({"roam_enable":0})), None);
        // 布尔、字符串写法都认
        assert_eq!(
            e(&away, json!({"enable":true,"roam_enable":false})),
            Some(false)
        );
        assert_eq!(
            e(&away, json!({"enable":"1","roam_enable":"1"})),
            Some(true)
        );
    }

    #[test]
    fn data_path_fields() {
        let d = data_path(
            &json!({"simcard_roam":"Home"}),
            &wwan(1, 0),
            &wan(),
            Some(&wan6()),
            None,
            100_000,
        );
        assert!(d.connected);
        // 有 IPv4：zte_wan6 不看
        assert_eq!(
            d.conn,
            Conn {
                ipv4: "10.1.2.3".into(),
                up_since_ms: Some(70_000),
                ..Conn::default()
            }
        );
        // 接口按 get_wwaniface 报的，不是 netifd 的 l3_device。
        assert_eq!(d.iface, "rmnet_data0");
        assert_eq!(d.dns, ["192.0.2.53", "2001:db8::53"]);
        let bare = data_path(
            &json!({}),
            &json!({"connect_status":"disconnected"}),
            &json!({"l3_device":"rmnet_data0"}),
            None,
            Some("'222.66.251.8' '116.236.159.8'".into()),
            1_000,
        );
        assert!(!bare.connected);
        assert_eq!(bare.iface, "rmnet_data0");
        assert_eq!(bare.conn, Conn::default());
        assert_eq!(bare.dns, ["222.66.251.8", "116.236.159.8"]);
    }

    fn wan6() -> Value {
        json!({"uptime":20,"l3_device":"rmnet_data9","ipv4-address":[],"ipv6-address":[{"address":"2001:db8:4f2a:1c07::1","mask":64}],"dns-server":["2001:db8:100::53"]})
    }

    #[test]
    fn ipv6_only_connection_uses_the_ipv6_address_and_dns() {
        let home = json!({"simcard_roam":"Home"});
        let w = json!({"enable":1,"roam_enable":0,"connect_status":"ipv6_connected","ipv4_dev_name":"","ipv6_dev_name":"rmnet_data1","ipv6_dns_prefer":"2001:db8:200::53","ipv6_dns_standby":"2001:db8:200::54"});
        let no_v4 = json!({"uptime":30,"l3_device":"rmnet_data0","ipv4-address":[],"dns-server":["192.0.2.53"]});
        let d = data_path(&home, &w, &no_v4, Some(&wan6()), None, 100_000);
        assert!(d.connected);
        assert_eq!(d.expected, Some(true));
        assert_eq!(
            d.conn,
            Conn {
                ipv6: "2001:db8:4f2a:1c07::1".into(),
                up_since_ms: Some(80_000),
                ..Conn::default()
            }
        );
        assert!(d.conn.v6_only());
        assert_eq!(d.iface, "rmnet_data1");
        assert_eq!(d.dns, ["2001:db8:100::53"]);
        // zte_wan6 没报 DNS：用 get_wwaniface 的；没报接口：用 zte_wan6 的 l3_device
        let mut bare6 = wan6();
        bare6["dns-server"] = json!([]);
        let mut w2 = w.clone();
        w2["ipv6_dev_name"] = json!("");
        let d = data_path(&home, &w2, &json!({}), Some(&bare6), None, 100_000);
        assert_eq!(d.dns, ["2001:db8:200::53", "2001:db8:200::54"]);
        assert_eq!(d.iface, "rmnet_data9");
        // zte_wan6 也没有地址：照旧（没有身份，不探测）
        let mut empty6 = wan6();
        empty6["ipv6-address"] = json!([]);
        let d = data_path(&home, &w, &no_v4, Some(&empty6), None, 100_000);
        assert_eq!(d.conn.addr(), "");
        assert_eq!(d.dns, ["192.0.2.53"]);
    }

    #[test]
    fn sim_identity_reads_number_or_string_slot() {
        assert_eq!(
            sim_id(&json!({"sim_iccid":"8986","current_sim_slot":2})),
            SimId {
                iccid: "8986".into(),
                slot: 2
            }
        );
        assert_eq!(sim_id(&json!({"current_sim_slot":"1"})).slot, 1);
    }
}
