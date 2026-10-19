//! /router/scenario (情景模式), the old `router/scenario/page.tsx` (batch 5b): where the device
//! thinks it is, the engine switch, a pin, the home networks (with a scan to pick them from), what
//! each scenario does, how eagerly it reacts, the run log.
//!
//! Where each value comes from (all through agent.rs with the tab's own session; agent.rs asks u60d
//! first once it has handed the scenarios over, else the agent): `GET /api/scenario` every 10 s
//! (like the old page) and again at once after a write on this page (`App::kick`), `GET
//! /api/scenario/log` every 15 s, `GET /api/scenario/template` and `GET /api/scenario/scan` only
//! inside a write (the scan uses the radios: inline confirm, never on load or on a timer).
//!
//! **The old page's writes were wrong** (inventory: it JSON-encoded the body twice, so `PUT
//! /api/scenario` failed, a pin always unpinned and the engine switch always turned it on). Here
//! the browser sends only what the user meant (an id, an on/off, a SSID, six numbers); the server
//! builds every body as plain JSON from a fresh read and proves each write by reading
//! `/api/scenario` again (the agent drops disk errors, so the readback is the only proof).
//!
//! Writes (tier per controls-inventory.md §/router/scenario):
//! - tier 2 (in the page): engine on/off (`PUT /enabled {"enabled":bool}`), pin / unpin (`POST /pin
//!   {"id":…|null}`), every config edit (`PUT /api/scenario`, the whole config: add / remove a home
//!   network, create the defaults, a scenario's Wi-Fi, the timing numbers), the scan;
//! - tier 3 (dialog): a scenario's Wi-Fi set to off, when the browser came over Tailscale.
//! - A config edit is read-modify-write on the config as a `serde_json::Value` (so `snapshot`,
//!   `verify_field`, `restore_on_exit`, the original names… are sent back as they were) from a
//!   fresh read at confirm time, under one lock, and only `config` is sent.
//! - Never sent twice: no reply → read back anyway → 「连接中断，还没确认是否生效」.
//!
//! Drafts: the six timing numbers are Datastar signals (`data-signals__ifmissing`), so a push never
//! wipes what is being typed. Everything else acts on a click (ask → confirm → set).
//!
//! CHILL-specific parts (the abroad section, a scenario's CHILL node, the restore texts) are in
//! `scenario/chill.rs`, which the public export drops.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use askama::Template;
use axum::Router;
use axum::extract::{Query, State};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::topbar::Via;
use crate::agent::{Caller, Fail, Polled};
use crate::i18n::Lang;
use crate::pages::{Shared, StreamQuery, split_leaves};
use crate::write::{self, Confirm, Outcome, Phase, Tier};

mod chill;

pub const PATH: &str = "/router/scenario";
const STATE: &str = "/api/scenario";
const TEMPLATE: &str = "/api/scenario/template";
const SCAN: &str = "/api/scenario/scan";
const LOG: &str = "/api/scenario/log";
const ENABLED: &str = "/api/scenario/enabled";
const PIN: &str = "/api/scenario/pin";
const EVERY: Duration = Duration::from_secs(10);
const LOG_EVERY: Duration = Duration::from_secs(15);
const READBACK: Duration = Duration::from_secs(15);
/// Turning the engine off runs the agent's boot-safe repair first (9–20 s on the device).
const ENGINE_OFF_READBACK: Duration = Duration::from_secs(45);
const SCAN_TIMEOUT: Duration = Duration::from_secs(45);
const HOME_ID: &str = "home";
const WIFI_PATH: &str = "/api/wifi/radio";
const AREAS: [&str; 8] = [
    "sc-eng", "sc-pin", "sc-home", "sc-scan", "sc-abroad", "sc-does", "sc-timing", "sc-setup",
];

/// `format!` in the page's language.
macro_rules! t {
    ($l:expr, $en:literal, $zh:literal $(, $k:ident = $v:expr)* $(,)?) => {
        match $l {
            Lang::En => format!($en $(, $k = $v)*),
            Lang::Zh => format!($zh $(, $k = $v)*),
        }
    };
}
pub(crate) use t;

/// This page's routes.
pub fn routes() -> Router<Shared> {
    let r = Router::new()
        .route(PATH, get(page))
        .route("/router/scenario/events", get(events))
        .route("/router/scenario/refresh", post(refresh))
        .route("/router/scenario/ask/engine", post(ask_engine))
        .route("/router/scenario/ask/pin", post(ask_pin))
        .route("/router/scenario/ask/add", post(ask_add))
        .route("/router/scenario/ask/del", post(ask_del))
        .route("/router/scenario/ask/scan", post(ask_scan))
        .route("/router/scenario/ask/wifi", post(ask_wifi))
        .route("/router/scenario/ask/timing", post(ask_timing))
        .route("/router/scenario/ask/defaults", post(ask_defaults))
        .route("/router/scenario/set", post(set))
        .route("/router/scenario/cancel", post(cancel));
    let r = r.merge(chill::routes());
    r
}

// ---- the scenario config as JSON ----

/// What the query strings of this page's buttons carry.
#[derive(Deserialize, Default)]
pub(crate) struct Args {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub on: Option<bool>,
    /// A SSID.
    #[serde(default)]
    pub s: String,
    /// A BSSID (empty = by name only).
    #[serde(default)]
    pub b: String,
    /// A CHILL node.
    #[serde(default)]
    pub m: String,
    #[serde(default)]
    pub a: String,
    #[serde(default)]
    pub t: String,
    #[serde(default)]
    pub lost: bool,
}

pub(crate) fn scenarios(st: &Value) -> &[Value] {
    st["config"]["scenarios"]
        .as_array()
        .map_or(&[], Vec::as_slice)
}

pub(crate) fn find<'a>(st: &'a Value, id: &str) -> Option<&'a Value> {
    scenarios(st).iter().find(|s| s["id"] == id)
}

fn find_mut<'a>(cfg: &'a mut Value, id: &str) -> Option<&'a mut Value> {
    cfg["scenarios"]
        .as_array_mut()?
        .iter_mut()
        .find(|s| s["id"] == id)
}

/// A scenario's name as shown: the English one for a built-in still under its factory name.
pub(crate) fn name_of(st: &Value, def: &Value, l: Lang) -> String {
    let id = def["id"].as_str().unwrap_or_default();
    if l == Lang::En
        && let Some(n) = st["names_en"][id].as_str()
    {
        return n.to_owned();
    }
    def["name"].as_str().unwrap_or(id).to_owned()
}

fn name_by_id(st: &Value, id: &str, l: Lang) -> String {
    find(st, id).map_or_else(|| id.to_owned(), |d| name_of(st, d, l))
}

/// Whether a scenario's Wi-Fi action turns the radios on (`None`: it has none).
fn wifi_of(def: &Value) -> Option<bool> {
    def["actions"]
        .as_array()?
        .iter()
        .find(|a| a["path"] == WIFI_PATH)
        .map(|a| a["body"]["ap_2g"] != false)
}

fn wifi_action(on: bool) -> Value {
    json!({
        "method": "PUT",
        "path": WIFI_PATH,
        "body": {"ap_2g": on, "ap_5g": on},
        "snapshot": ["wireless.main_2g.disabled", "wireless.main_5g.disabled"],
    })
}

/// (SSID, BSSID or "") of the home scenario's networks.
fn entries_of(def: Option<&Value>) -> Vec<(String, String)> {
    let Some(d) = def else { return Vec::new() };
    if d["detect"]["type"] != "ssid" {
        return Vec::new();
    }
    d["detect"]["entries"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|e| {
                    (
                        e["ssid"].as_str().unwrap_or_default().to_owned(),
                        e["bssid"].as_str().unwrap_or_default().to_owned(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn same_entry(a: &(String, String), ssid: &str, bssid: &str) -> bool {
    a.0 == ssid && a.1.eq_ignore_ascii_case(bssid)
}

fn home_entries(cfg_or_state: &Value) -> Vec<(String, String)> {
    // works on a state (config under "config") and on a config
    let scs = cfg_or_state["config"]["scenarios"]
        .as_array()
        .or_else(|| cfg_or_state["scenarios"].as_array());
    entries_of(scs.and_then(|a| a.iter().find(|s| s["id"] == HOME_ID)))
}

/// Percent-encode everything but unreserved characters: safe in a URL, in an HTML attribute and
/// inside a JS string (no quote, backslash or ampersand survives).
pub(crate) fn enc(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

// ---- config edits (pure functions on the config; proven by a readback) ----

/// One change to the config. `apply` runs on a fresh read, `holds` is the readback.
pub(crate) trait Edit: Send + Sync {
    fn needs_template(&self) -> bool {
        false
    }
    /// Change `cfg` (the `config` of a fresh read). `Err` = it can't be done any more (said as is).
    fn apply(&self, cfg: &mut Value, tpl: Option<&Value>, l: Lang) -> Result<(), String>;
    /// True once a read shows the change (`cfg` = `config` of that read).
    fn holds(&self, cfg: &Value) -> bool;
    /// 「正在…」.
    fn pending(&self, l: Lang) -> String;
    /// Said when read back.
    fn applied(&self, l: Lang) -> String;
}

pub(crate) fn gone(l: Lang) -> String {
    l.pick(
        "That scenario is not there any more",
        "这个情景已经不在了",
    )
    .into()
}

struct AddEntry {
    ssid: String,
    bssid: String,
}

impl Edit for AddEntry {
    fn apply(&self, cfg: &mut Value, _: Option<&Value>, l: Lang) -> Result<(), String> {
        if entries_of(cfg["scenarios"].as_array().and_then(|a| a.iter().find(|s| s["id"] == HOME_ID)))
            .iter()
            .any(|e| same_entry(e, &self.ssid, &self.bssid))
        {
            return Err(l
                .pick("Already in the list", "已经在列表里了")
                .into());
        }
        let home = find_mut(cfg, HOME_ID).ok_or_else(|| {
            l.pick(
                "There is no “home” scenario to add it to",
                "没有「在家」情景可以添加",
            )
            .to_owned()
        })?;
        if home["detect"]["type"] != "ssid" {
            return Err(l
                .pick(
                    "The home scenario doesn't detect by network name",
                    "「在家」情景不是按网络名称判断的",
                )
                .into());
        }
        let e = if self.bssid.is_empty() {
            json!({"ssid": self.ssid})
        } else {
            json!({"ssid": self.ssid, "bssid": self.bssid})
        };
        let list = home["detect"]["entries"]
            .as_array_mut()
            .ok_or_else(|| gone(l))?;
        list.push(e);
        Ok(())
    }
    fn holds(&self, cfg: &Value) -> bool {
        home_entries(cfg)
            .iter()
            .any(|e| same_entry(e, &self.ssid, &self.bssid))
    }
    fn pending(&self, l: Lang) -> String {
        l.pick("Adding…", "正在添加…").into()
    }
    fn applied(&self, l: Lang) -> String {
        t!(l, "Added “{s}” to the home networks", "已把「{s}」加入在家的网络", s = self.ssid)
    }
}

struct DelEntry {
    ssid: String,
    bssid: String,
}

impl Edit for DelEntry {
    fn apply(&self, cfg: &mut Value, _: Option<&Value>, l: Lang) -> Result<(), String> {
        let home = find_mut(cfg, HOME_ID).ok_or_else(|| gone(l))?;
        let list = home["detect"]["entries"]
            .as_array_mut()
            .ok_or_else(|| gone(l))?;
        let before = list.len();
        list.retain(|e| {
            !same_entry(
                &(
                    e["ssid"].as_str().unwrap_or_default().to_owned(),
                    e["bssid"].as_str().unwrap_or_default().to_owned(),
                ),
                &self.ssid,
                &self.bssid,
            )
        });
        if list.len() == before {
            return Err(l
                .pick(
                    "That network is not in the list any more",
                    "这个网络已经不在列表里了",
                )
                .into());
        }
        Ok(())
    }
    fn holds(&self, cfg: &Value) -> bool {
        !home_entries(cfg)
            .iter()
            .any(|e| same_entry(e, &self.ssid, &self.bssid))
    }
    fn pending(&self, l: Lang) -> String {
        l.pick("Removing…", "正在移除…").into()
    }
    fn applied(&self, l: Lang) -> String {
        t!(l, "Removed “{s}” from the home networks", "已把「{s}」从在家的网络里移除", s = self.ssid)
    }
}

struct Wifi {
    id: String,
    on: bool,
    name: String,
}

impl Edit for Wifi {
    fn apply(&self, cfg: &mut Value, _: Option<&Value>, l: Lang) -> Result<(), String> {
        let def = find_mut(cfg, &self.id).ok_or_else(|| gone(l))?;
        if def["detect"]["type"] == "fallback" && !self.on {
            return Err(l
                .pick(
                    "The away scenario always keeps Wi-Fi on",
                    "「外出」情景的 Wi-Fi 始终开着",
                )
                .into());
        }
        // Wi-Fi goes first: arriving from home means the APs are down, and anything after it may
        // need the network.
        let mut acts = vec![wifi_action(self.on)];
        if let Some(old) = def["actions"].as_array() {
            acts.extend(old.iter().filter(|a| a["path"] != WIFI_PATH).cloned());
        }
        def["actions"] = Value::Array(acts);
        Ok(())
    }
    fn holds(&self, cfg: &Value) -> bool {
        cfg["scenarios"]
            .as_array()
            .and_then(|a| a.iter().find(|s| s["id"] == self.id))
            .is_some_and(|d| wifi_of(d).unwrap_or(true) == self.on)
    }
    fn pending(&self, l: Lang) -> String {
        l.pick("Saving…", "正在保存…").into()
    }
    fn applied(&self, l: Lang) -> String {
        match (l, self.on) {
            (Lang::En, true) => format!("Wi-Fi on when entering “{}”", self.name),
            (Lang::En, false) => format!("Wi-Fi off when entering “{}”", self.name),
            (Lang::Zh, true) => format!("进入「{}」时会开 Wi-Fi", self.name),
            (Lang::Zh, false) => format!("进入「{}」时会关 Wi-Fi", self.name),
        }
    }
}

/// The six timing numbers, in `PARAMS` order.
struct Params([f64; 6]);

/// (key, signal, English label, Chinese label, input mode)
const PARAMS: [(&str, &str, &str, &str, &str); 6] = [
    ("scan_interval_away_charging_secs", "_tc", "Scan interval on charger (s)", "充电时扫描间隔（秒）", "numeric"),
    ("scan_interval_away_battery_secs", "_tb", "Scan interval on battery (s)", "用电池时扫描间隔（秒）", "numeric"),
    ("scan_interval_home_secs", "_th", "Scan interval at home (s)", "在家时扫描间隔（秒）", "numeric"),
    ("enter_hits", "_te", "Confirmations to enter", "进入需确认次数", "numeric"),
    ("exit_misses", "_tx", "Confirmations to leave", "离开需确认次数", "numeric"),
    // a negative number: a numeric keypad has no minus sign on iOS
    ("min_rssi_dbm", "_tr", "Signal floor without a hardware address (dBm)", "无硬件地址时的信号下限（dBm）", "text"),
];

impl Edit for Params {
    fn apply(&self, cfg: &mut Value, _: Option<&Value>, _: Lang) -> Result<(), String> {
        if !cfg["params"].is_object() {
            cfg["params"] = json!({});
        }
        for (i, (key, ..)) in PARAMS.iter().enumerate() {
            cfg["params"][*key] = if i < 5 {
                json!(self.0[i] as u64)
            } else {
                json!(self.0[i])
            };
        }
        Ok(())
    }
    fn holds(&self, cfg: &Value) -> bool {
        PARAMS
            .iter()
            .enumerate()
            .all(|(i, (key, ..))| cfg["params"][*key].as_f64() == Some(self.0[i]))
    }
    fn pending(&self, l: Lang) -> String {
        l.pick("Saving the timing…", "正在保存反应快慢…").into()
    }
    fn applied(&self, l: Lang) -> String {
        l.pick("Timing saved", "反应快慢已保存").into()
    }
}

struct Defaults;

impl Edit for Defaults {
    fn needs_template(&self) -> bool {
        true
    }
    fn apply(&self, cfg: &mut Value, tpl: Option<&Value>, l: Lang) -> Result<(), String> {
        if cfg["scenarios"].as_array().is_some_and(|a| !a.is_empty()) {
            return Err(l
                .pick("Scenarios are already set up", "情景已经设置过了")
                .into());
        }
        let tpl = tpl.filter(|t| t["scenarios"].is_array()).ok_or_else(|| {
            l.pick(
                "The device gave no template",
                "设备没有给出默认模板",
            )
            .to_owned()
        })?;
        *cfg = tpl.clone();
        Ok(())
    }
    fn holds(&self, cfg: &Value) -> bool {
        cfg["scenarios"].as_array().is_some_and(|a| !a.is_empty())
    }
    fn pending(&self, l: Lang) -> String {
        l.pick("Creating the defaults…", "正在创建默认情景…").into()
    }
    fn applied(&self, l: Lang) -> String {
        l.pick(
            "Default scenarios created — now add your home network",
            "已创建默认情景 — 接下来添加你家的网络",
        )
        .into()
    }
}

// ---- reads ----

async fn read_state(app: &Shared, who: &Caller) -> Result<Value, Fail> {
    crate::agent::get(app, who, STATE).await
}

async fn read_log(app: &Shared, who: &Caller) -> Result<String, Fail> {
    let v = crate::agent::get(app, who, LOG).await?;
    Ok(v["log"].as_str().unwrap_or_default().to_owned())
}

/// A network from a scan.
#[derive(Clone, Debug, PartialEq)]
pub struct Net {
    pub ssid: String,
    pub bssid: String,
    pub signal: i64,
}

fn nets(v: &Value) -> Vec<Net> {
    v["networks"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|n| Net {
                    ssid: n["ssid"].as_str().unwrap_or_default().to_owned(),
                    bssid: n["bssid"].as_str().unwrap_or_default().to_owned(),
                    signal: n["signal"].as_f64().unwrap_or(0.0).round() as i64,
                })
                .collect()
        })
        .unwrap_or_default()
}

const KEEP_SCAN: Duration = Duration::from_secs(1800);

/// The last scan of each session (the list survives a reconnect of the push stream).
fn scans() -> &'static Mutex<HashMap<String, (Instant, Vec<Net>)>> {
    static S: std::sync::OnceLock<Mutex<HashMap<String, (Instant, Vec<Net>)>>> =
        std::sync::OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

fn keep_scan(sid: &str, n: Vec<Net>) {
    let mut m = scans().lock().unwrap();
    m.retain(|_, (at, _)| at.elapsed() < KEEP_SCAN);
    m.insert(sid.to_owned(), (Instant::now(), n));
}

fn last_scan(sid: &str) -> Option<Vec<Net>> {
    let m = scans().lock().unwrap();
    m.get(sid)
        .filter(|(at, _)| at.elapsed() < KEEP_SCAN)
        .map(|(_, n)| n.clone())
}

// ---- the view ----

/// What the page has from the agent.
#[derive(Clone, Default)]
pub struct Got {
    pub st: Option<Value>,
    pub fail: Option<Fail>,
    pub at: Option<SystemTime>,
    pub log: Option<String>,
    pub log_fail: Option<Fail>,
    pub scan: Option<Vec<Net>>,
    /// Bumped by every early re-read (a button kicked the page): the elements that a click may
    /// have changed on the browser's side (a CHILL node `<select>`) are replaced, not morphed.
    pub rev: u64,
    pub chill: chill::Got,
}

impl Got {
    pub fn configured(&self) -> bool {
        self.st.as_ref().is_some_and(|s| !scenarios(s).is_empty())
    }
    pub fn stale(&self) -> bool {
        self.st.is_some() && self.fail.is_some()
    }
    /// Changes are off until the settings can be read (and are fresh).
    pub fn locked(&self) -> bool {
        self.st.is_none() || self.fail.is_some()
    }
}

pub struct PinRow {
    pub name: String,
    pub url: String,
    pub on: bool,
}

pub struct EntryRow {
    pub ssid: String,
    pub bssid: String,
    pub del: String,
}

pub struct NetRow {
    pub ssid: String,
    pub bssid: String,
    pub signal: i64,
    pub add: String,
}

pub struct DoesRow {
    pub name: String,
    /// Wi-Fi on when entering (the fallback has no switch).
    pub wifi: bool,
    pub fallback: bool,
    pub wifi_url: String,
    pub node: String,
}

pub struct ParamRow {
    pub sig: &'static str,
    pub label: &'static str,
    pub mode: &'static str,
}

pub struct View {
    pub l: Lang,
    pub tone: &'static str,
    pub state: String,
    pub reason: Option<String>,
    pub meta: Option<String>,
    pub retry: bool,
    pub loaded: bool,
    pub configured: bool,
    pub stale: bool,
    pub locked: bool,
    // right now
    pub current: Option<String>,
    pub last_switch: String,
    pub last_scan: String,
    pub sim_mcc: Option<String>,
    pub enabled: bool,
    pub eng_url: String,
    // pin
    pub pin_name: Option<String>,
    pub pin_rows: Vec<PinRow>,
    pub auto_on: bool,
    // home
    pub entries: Vec<EntryRow>,
    pub nameless: usize,
    // nearby
    pub scanned: Option<Vec<NetRow>>,
    pub scan_total: usize,
    // does
    pub does: Vec<DoesRow>,
    pub does_desc: String,
    pub does_note: String,
    // timing
    pub params: Vec<ParamRow>,
    pub sigs: String,
    pub dirty: String,
    pub reset: String,
    // log
    pub log: Option<String>,
    pub log_err: Option<String>,
}

impl View {
    pub fn symbol(&self) -> &'static str {
        match self.tone {
            "bad" => "■",
            "warn" | "stale" => "▲",
            _ => "●",
        }
    }
    pub fn plus(&self) -> String {
        "<svg width=\"20\" height=\"20\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" stroke-linecap=\"round\" aria-hidden=\"true\"><path d=\"M12 5v14M5 12h14\"/></svg>".into()
    }
    pub fn trash(&self) -> String {
        crate::shell::icon("Trash", false, 20, "")
    }
    pub fn refresh_icon(&self) -> String {
        crate::shell::icon("ArrowClockwise", false, 20, "")
    }
    pub fn signal_label(&self, n: &i64) -> String {
        format!("{n} dBm")
    }
}

pub fn view(g: &Got, l: Lang, sid: &str) -> View {
    let _ = sid;
    let st = g.st.as_ref();
    let nul = Value::Null;
    let s = st.unwrap_or(&nul);
    let configured = g.configured();
    let stale = g.stale();
    let locked = g.locked();
    let enabled = s["enabled"].as_bool().unwrap_or(false);
    let cur_id = s["current"].as_str().unwrap_or_default();
    let current = (!cur_id.is_empty()).then(|| name_by_id(s, cur_id, l));
    let pin_id = s["pin"].as_str().map(str::to_owned);
    let need = s["config"]["params"]["enter_hits"].as_u64().unwrap_or(2);

    let (mut tone, state, reason): (&'static str, String, Option<String>) = match (st, &g.fail) {
        (None, Some(f)) => (
            "bad",
            l.pick("Can't read scenarios", "读不到情景").into(),
            Some(f.text(l)),
        ),
        (None, None) => (
            "neutral",
            l.pick("Reading scenarios…", "正在读取情景…").into(),
            None,
        ),
        (Some(_), _) if !configured => (
            "neutral",
            l.pick("No scenarios yet", "还没有情景").into(),
            Some(
                l.pick(
                    "Create the defaults below, then add your home network.",
                    "先在下面创建默认情景，再添加家里的网络。",
                )
                .into(),
            ),
        ),
        (Some(_), _) if !enabled => (
            "neutral",
            l.pick("Engine off", "引擎已关闭").into(),
            Some(
                l.pick(
                    "The device is not switching scenarios by itself. Wi-Fi stays on.",
                    "设备不会自己切换情景，Wi-Fi 保持开着。",
                )
                .into(),
            ),
        ),
        (Some(_), _) => {
            let mut tone = if current.is_some() { "ok" } else { "neutral" };
            let mut reason = None;
            if s["guard_takeover"].as_bool() == Some(true) {
                tone = "warn";
                reason = Some(l.pick(
                    "The Wi-Fi watchdog took over while the agent was not responding. Staying in away; the pin is paused until 10 minutes of steady running.",
                    "后台曾无响应，Wi-Fi 看门狗已接管：保持在外出情景，固定暂停，稳定运行 10 分钟后恢复。",
                ).into());
            } else if let Some(e) = s["last_error"].as_str().filter(|e| !e.is_empty()) {
                tone = "warn";
                reason = Some(e.to_owned());
            } else if let Some(p) = &pin_id {
                let name = name_by_id(s, p, l);
                reason = Some(t!(l,
                    "Pinned to “{name}” — detection is not changing it.",
                    "已固定为「{name}」，识别结果不会改变它。"));
            }
            (
                tone,
                current.clone().unwrap_or_else(|| {
                    l.pick("Not decided yet", "还没判断出来").into()
                }),
                reason,
            )
        }
    };
    if stale {
        tone = "stale";
    }
    let cand = s["candidate"].as_str().unwrap_or_default();
    let candidate_text = (st.is_some() && !cand.is_empty() && cand != cur_id).then(|| {
        let name = name_by_id(s, cand, l);
        let hits = s["hits"].as_u64().unwrap_or(0);
        t!(l,
            "Seeing {name} — {hits} of {need} confirmations so far.",
            "看到了「{name}」— 已确认 {hits} / {need} 次。")
    });
    let meta = match (candidate_text, stale) {
        (c, true) => {
            let f = crate::agent::stopped(l, "Settings", "设置", g.at);
            Some(match c {
                Some(c) => format!("{c} · {f}"),
                None => f,
            })
        }
        (c, false) => c,
    };

    // pin
    let pin_rows = scenarios(s)
        .iter()
        .map(|d| {
            let id = d["id"].as_str().unwrap_or_default();
            PinRow {
                name: name_of(s, d, l),
                url: format!("{PATH}/ask/pin?id={}", enc(id)),
                on: pin_id.as_deref() == Some(id),
            }
        })
        .collect();

    // home
    let home = find(s, HOME_ID);
    let entries_raw = entries_of(home);
    let nameless = entries_raw.iter().filter(|e| e.1.is_empty()).count();
    let entries = entries_raw
        .iter()
        .map(|(ssid, bssid)| EntryRow {
            ssid: ssid.clone(),
            bssid: bssid.clone(),
            del: format!("{PATH}/ask/del?s={}&b={}", enc(ssid), enc(bssid)),
        })
        .collect();

    // nearby: what is already listed is hidden
    let scanned = g.scan.as_ref().map(|list| {
        list.iter()
            .filter(|n| !entries_raw.iter().any(|e| same_entry(e, &n.ssid, &n.bssid)))
            .map(|n| NetRow {
                ssid: n.ssid.clone(),
                bssid: n.bssid.clone(),
                signal: n.signal,
                add: format!("{PATH}/ask/add?s={}&b={}", enc(&n.ssid), enc(&n.bssid)),
            })
            .collect()
    });
    let scan_total = g.scan.as_ref().map_or(0, Vec::len);

    // what each scenario does
    let does_desc = l.pick(
        "Applied once on entering. A changed CHILL node is put back on leaving. Away is where every failure ends up, so its Wi-Fi always stays on.",
        "进入情景时执行一次；改过的 CHILL 节点在离开时恢复。「外出」是一切出错时的落脚点，所以它的 Wi-Fi 始终开着。",
    );
    let does = scenarios(s)
        .iter()
        .enumerate()
        .map(|(idx, d)| {
            let id = d["id"].as_str().unwrap_or_default();
            let wifi = wifi_of(d).unwrap_or(true);
            let _ = idx;
            DoesRow {
                name: name_of(s, d, l),
                wifi,
                fallback: d["detect"]["type"] == "fallback",
                wifi_url: format!("{PATH}/ask/wifi?id={}&on={}", enc(id), !wifi),
                node: chill::node_row(s, d, idx, &g.chill, locked, g.rev, l),
            }
        })
        .collect();

    // timing: the numbers as read; the draft is the browser's (signals), seeded once
    let shown = |i: usize| {
        let v = &s["config"]["params"][PARAMS[i].0];
        match v.as_f64() {
            Some(f) if f.fract() == 0.0 => format!("{}", f as i64),
            Some(f) => format!("{f}"),
            None => String::new(),
        }
    };
    let vals: Vec<String> = (0..6).map(shown).collect();
    let mut sigs = serde_json::Map::new();
    let mut dirty = Vec::new();
    let mut reset = Vec::new();
    for (i, (_, sig, ..)) in PARAMS.iter().enumerate() {
        sigs.insert((*sig).to_owned(), json!(vals[i]));
        dirty.push(format!("String(${sig}).trim() !== '{}'", vals[i]));
        reset.push(format!("${sig} = '{}'", vals[i]));
    }

    View {
        l,
        tone,
        state,
        reason,
        meta,
        retry: st.is_none() && g.fail.is_some(),
        loaded: st.is_some(),
        configured,
        stale,
        locked,
        current,
        last_switch: crate::sms_forward::device_time(s["last_switch"].as_i64().unwrap_or(0)),
        last_scan: crate::sms_forward::device_time(s["last_scan"].as_i64().unwrap_or(0)),
        sim_mcc: s["sim_mcc"].as_str().map(str::to_owned),
        enabled,
        eng_url: format!("{PATH}/ask/engine?on={}", !enabled),
        pin_name: pin_id.as_deref().map(|p| name_by_id(s, p, l)),
        pin_rows,
        auto_on: pin_id.is_none(),
        entries,
        nameless,
        scanned,
        scan_total,
        does,
        does_desc: does_desc.into(),
        does_note: chill::does_note(&g.chill, l),
        params: PARAMS
            .iter()
            .map(|(_, sig, en, zh, mode)| ParamRow {
                sig,
                label: l.pick(en, zh),
                mode,
            })
            .collect(),
        sigs: Value::Object(sigs).to_string(),
        dirty: dirty.join(" || "),
        reset: reset.join("; "),
        log: g.log.clone(),
        log_err: g.log_fail.as_ref().map(|f| f.text(l)),
    }
}

#[derive(Template)]
#[template(path = "scenario_leaves.html")]
struct Leaves<'a> {
    v: &'a View,
}

#[allow(unused_mut)]
fn leaves(g: &Got, l: Lang, sid: &str) -> Vec<(String, String)> {
    let v = view(g, l, sid);
    let mut out = split_leaves(&Leaves { v: &v }.render().unwrap());
    out.extend(chill::leaves(g, l));
    out
}

// ---- the page and its stream ----

pub struct Poll {
    l: Lang,
    who: Caller,
    got: Got,
    /// When the next read of the settings is due; asked for sooner = a button kicked the page.
    due: Instant,
    log_due: Instant,
}

impl Polled for Poll {
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        let now = Instant::now();
        let kicked = now + Duration::from_secs(1) < self.due;
        match read_state(app, &self.who).await {
            Ok(v) => {
                self.got.st = Some(v);
                self.got.fail = None;
                self.got.at = Some(SystemTime::now());
            }
            Err(f) => self.got.fail = Some(f),
        }
        if kicked || now >= self.log_due {
            match read_log(app, &self.who).await {
                Ok(t) => {
                    self.got.log = Some(t);
                    self.got.log_fail = None;
                }
                Err(f) => self.got.log_fail = Some(f),
            }
            self.log_due = now + LOG_EVERY;
        }
        if kicked {
            self.got.rev += 1;
        }
        if self.got.configured() {
            self.got.chill.fetch(app, &self.who).await;
        }
        self.got.scan = self.who.sid.as_deref().and_then(last_scan);
        self.due = Instant::now() + EVERY;
        (leaves(&self.got, self.l, ""), EVERY)
    }
}

#[derive(Template)]
#[template(path = "scenario.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    leaves: Vec<(String, String)>,
    ops: HashMap<&'static str, String>,
}

impl Page {
    fn leaf(&self, id: &str) -> &str {
        self.leaves
            .iter()
            .find(|(k, _)| k == id)
            .map_or("", |(_, h)| h.as_str())
    }
    fn op(&self, id: &str) -> &str {
        self.ops.get(id).map_or("", String::as_str)
    }
}

/// The write area `id` (`{id}-op`), its cancel posting to `cancel?a={id}`.
pub(crate) fn area(id: &str, l: Lang, remote: Via, set: &str, phase: &Phase) -> String {
    let cancel = format!("{PATH}/cancel?a={id}");
    write::Area {
        id,
        l,
        remote,
        set,
        cancel: &cancel,
        phase,
    }
    .html()
}

fn area_id(a: &str) -> &'static str {
    AREAS.iter().copied().find(|x| *x == a).unwrap_or("sc-home")
}

pub async fn page(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let shell = app.hub.read(|st| {
        crate::shell::Shell::new(&headers, PATH).live(
            st,
            &ctx,
            Some(format!("{PATH}/events?g={}", st.generation)),
        )
    });
    let idle = Phase::Idle;
    let ops = AREAS
        .iter()
        .map(|id| (*id, area(id, l, ctx.remote, "", &idle)))
        .collect();
    let html = Page {
        shell,
        l,
        leaves: leaves(&Got::default(), l, ""),
        ops,
    }
    .render()
    .unwrap();
    crate::assets::maybe_gzip(
        &headers,
        "text/html; charset=utf-8",
        html.into_bytes(),
        "no-store",
    )
}

pub async fn events(
    State(app): State<Shared>,
    Query(q): Query<StreamQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let now = Instant::now();
    let p = Poll {
        l: ctx.lang,
        who: Caller::of(&app, &req),
        got: Got::default(),
        due: now,
        log_due: now,
    };
    crate::agent::stream(app, ctx, q.g, PATH, p)
}

/// Retry / refresh the log: read everything again now.
pub async fn refresh(State(app): State<Shared>) -> axum::http::StatusCode {
    let _ = app.kick.send(PATH);
    axum::http::StatusCode::NO_CONTENT
}

pub(crate) fn one(html: String) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(futures_util::stream::once(async move {
        Ok(crate::pages::patch(&html))
    }))
}

async fn body(req: axum::extract::Request) -> Vec<u8> {
    axum::body::to_bytes(req.into_body(), 4 * 1024)
        .await
        .map(|b| b.to_vec())
        .unwrap_or_default()
}

fn done(id: &str, l: Lang, o: Outcome) -> String {
    area(id, l, Via::Local, "", &Phase::Done(o))
}

fn pending(id: &str, l: Lang, text: &str) -> String {
    area(id, l, Via::Local, "", &Phase::Pending(text.to_owned()))
}

pub(crate) fn not_read(l: Lang, f: &Fail) -> Outcome {
    Outcome::NotYet(t!(l,
        "Can't read the scenarios now ({e}); try again",
        "现在读不到情景设置（{e}），稍后再试",
        e = f.text(l)))
}

// ---- asks ----

/// What a confirmed write does.
pub(crate) enum Kind {
    Engine(bool),
    Pin(Option<String>),
    Edit(Box<dyn Edit>),
    Scan,
}

pub(crate) struct Held {
    area: &'static str,
    kind: Kind,
}

const STASH_TTL: Duration = Duration::from_secs(600);

fn stash() -> &'static Mutex<HashMap<String, (Instant, Held)>> {
    static S: std::sync::OnceLock<Mutex<HashMap<String, (Instant, Held)>>> =
        std::sync::OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

fn put(h: Held) -> String {
    let t = crate::auth::random_id();
    let mut m = stash().lock().unwrap();
    m.retain(|_, (at, _)| at.elapsed() < STASH_TTL);
    m.insert(t.clone(), (Instant::now(), h));
    t
}

fn take(t: &str) -> Option<Held> {
    let mut m = stash().lock().unwrap();
    m.remove(t)
        .filter(|(at, _)| at.elapsed() < STASH_TTL)
        .map(|(_, h)| h)
}

pub(crate) fn inline(title: String, what: String, action: String) -> Confirm {
    Confirm {
        tier: Tier::Inline,
        title,
        what,
        downtime: None,
        recovery: None,
        action,
        cuts_uplink: false,
        word: None,
        danger: false,
        revert: crate::write::Revert::No,
    }
}

/// Read the settings fresh (unless `need_state` is false), let `build` check the request against
/// them and say what it will do, and answer with the confirm of `area`.
pub(crate) async fn ask_with(
    app: &Shared,
    req: axum::extract::Request,
    area_name: &'static str,
    need_state: bool,
    build: impl FnOnce(&Value, &Value, Lang, Via) -> Result<(Kind, Confirm), String>,
) -> Response {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let who = Caller::of(app, &req);
    let (l, remote) = (ctx.lang, ctx.remote);
    let sent: Value = serde_json::from_slice(&body(req).await).unwrap_or(Value::Null);
    let st = if need_state {
        match read_state(app, &who).await {
            Ok(s) => s,
            Err(f) => return one(done(area_name, l, not_read(l, &f))).into_response(),
        }
    } else {
        Value::Null
    };
    let html = match build(&st, &sent, l, remote) {
        Err(e) => done(area_name, l, Outcome::NotYet(e)),
        Ok((kind, c)) => {
            let set = format!(
                "{PATH}/set?a={area_name}&t={}",
                put(Held {
                    area: area_name,
                    kind
                })
            );
            area(area_name, l, remote, &set, &Phase::Ask(c))
        }
    };
    one(html).into_response()
}

async fn ask_engine(
    State(app): State<Shared>,
    Query(a): Query<Args>,
    req: axum::extract::Request,
) -> Response {
    let on = a.on.unwrap_or(true);
    ask_with(&app, req, "sc-eng", true, move |_, _, l, _| {
        let what = if on {
            l.pick(
                "The device starts switching scenarios by itself. When it recognises home it switches its own Wi-Fi off.",
                "设备开始按看到的网络自己切换情景；认出在家时会关掉自己的 Wi-Fi。",
            )
        } else {
            let off = l.pick(
                "The device stops switching scenarios and goes back to the away settings: Wi-Fi on, a changed CHILL node put back. This can take up to a minute.",
                "设备不再切换情景，回到「外出」的设置：Wi-Fi 打开，改过的 CHILL 节点恢复。最长要一分钟左右。",
            );
            off
        };
        let action = match (l, on) {
            (Lang::En, true) => "Confirm: turn the engine on",
            (Lang::En, false) => "Confirm: turn the engine off",
            (Lang::Zh, true) => "确认：打开引擎",
            (Lang::Zh, false) => "确认：关闭引擎",
        };
        Ok((
            Kind::Engine(on),
            inline(
                l.pick("Scenario engine", "情景引擎").into(),
                what.into(),
                action.into(),
            ),
        ))
    })
    .await
}

async fn ask_pin(
    State(app): State<Shared>,
    Query(a): Query<Args>,
    req: axum::extract::Request,
) -> Response {
    ask_with(&app, req, "sc-pin", true, move |st, _, l, _| {
        let (kind, what, action) = if a.id.is_empty() {
            (
                Kind::Pin(None),
                l.pick(
                    "The device goes back to choosing the scenario from what it sees.",
                    "设备重新按看到的网络选择情景。",
                )
                .to_owned(),
                l.pick("Confirm: clear the pin", "确认：取消固定").to_owned(),
            )
        } else {
            let def = find(st, &a.id).ok_or_else(|| gone(l))?;
            let name = name_of(st, def, l);
            (
                Kind::Pin(Some(a.id.clone())),
                t!(l,
                    "The device switches to “{name}” now and stays there whatever it sees, also after a reboot.",
                    "设备现在切到「{name}」，之后不管看到什么都停在这里，重启后也不变。"),
                t!(l, "Confirm: pin to “{name}”", "确认：固定为「{name}」"),
            )
        };
        Ok((
            kind,
            inline(l.pick("Pin a scenario", "固定情景").into(), what, action),
        ))
    })
    .await
}

async fn ask_add(
    State(app): State<Shared>,
    Query(a): Query<Args>,
    req: axum::extract::Request,
) -> Response {
    ask_with(&app, req, "sc-scan", true, move |st, _, l, _| {
        let ssid = a.s.trim().to_owned();
        if ssid.is_empty() {
            return Err(l
                .pick(
                    "A hidden network has no name to add",
                    "隐藏的网络没有名称，不能添加",
                )
                .into());
        }
        if home_entries(st)
            .iter()
            .any(|e| same_entry(e, &ssid, &a.b))
        {
            return Err(l.pick("Already in the list", "已经在列表里了").into());
        }
        let what = t!(l,
            "Whenever the device sees “{ssid}” it counts as home and switches its own Wi-Fi off.",
            "设备看到「{ssid}」时就算作在家，会关掉自己的 Wi-Fi。");
        let action = t!(l, "Confirm: add {ssid}", "确认：添加 {ssid}");
        Ok((
            Kind::Edit(Box::new(AddEntry {
                ssid,
                bssid: a.b.clone(),
            })),
            inline(l.pick("Add a home network", "添加在家的网络").into(), what, action),
        ))
    })
    .await
}

async fn ask_del(
    State(app): State<Shared>,
    Query(a): Query<Args>,
    req: axum::extract::Request,
) -> Response {
    ask_with(&app, req, "sc-home", true, move |st, _, l, _| {
        if !home_entries(st)
            .iter()
            .any(|e| same_entry(e, &a.s, &a.b))
        {
            return Err(l
                .pick(
                    "That network is not in the list any more",
                    "这个网络已经不在列表里了",
                )
                .into());
        }
        let what = t!(l,
            "“{ssid}” no longer counts as home. You can add it again from a scan.",
            "「{ssid}」不再算作在家。之后可以从扫描结果里重新添加。",
            ssid = a.s);
        let action = t!(l, "Confirm: remove {ssid}", "确认：移除 {ssid}", ssid = a.s);
        Ok((
            Kind::Edit(Box::new(DelEntry {
                ssid: a.s.clone(),
                bssid: a.b.clone(),
            })),
            inline(l.pick("Remove a home network", "移除在家的网络").into(), what, action),
        ))
    })
    .await
}

async fn ask_scan(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    ask_with(&app, req, "sc-scan", false, |_, _, l, _| {
        Ok((
            Kind::Scan,
            inline(
                l.pick("Scan for networks", "扫描网络").into(),
                l.pick(
                    "The device scans for nearby Wi-Fi, usually 5–30 seconds. If no radio is free it adds a temporary scan interface and removes it afterwards. Devices on the U60's Wi-Fi may slow down briefly.",
                    "设备扫描附近的 Wi-Fi，一般 5–30 秒；没有空闲的无线接口时会临时建一个扫描用的虚拟网卡，扫完删掉。连着 U60 Wi-Fi 的设备可能短暂变慢。",
                )
                .into(),
                l.pick("Confirm: scan now", "确认：现在扫描").into(),
            ),
        ))
    })
    .await
}

async fn ask_wifi(
    State(app): State<Shared>,
    Query(a): Query<Args>,
    req: axum::extract::Request,
) -> Response {
    let on = a.on.unwrap_or(true);
    ask_with(&app, req, "sc-does", true, move |st, _, l, remote| {
        let def = find(st, &a.id).ok_or_else(|| gone(l))?;
        if def["detect"]["type"] == "fallback" && !on {
            return Err(l
                .pick(
                    "The away scenario always keeps Wi-Fi on",
                    "「外出」情景的 Wi-Fi 始终开着",
                )
                .into());
        }
        let name = name_of(st, def, l);
        let what = if on {
            t!(l,
                "On entering “{name}” the device switches its Wi-Fi on. Nothing changes right now.",
                "进入「{name}」时设备会打开 Wi-Fi。现在不会有变化。")
        } else {
            t!(l,
                "On entering “{name}” the device switches its own Wi-Fi off; everything connected to it drops off. Nothing changes right now.",
                "进入「{name}」时设备会关掉自己的 Wi-Fi，连着它的设备都会断开。现在不会有变化。")
        };
        let action = match on {
            true => t!(l, "Confirm: Wi-Fi on in “{name}”", "确认：「{name}」时开 Wi-Fi"),
            false => t!(l, "Confirm: Wi-Fi off in “{name}”", "确认：「{name}」时关 Wi-Fi"),
        };
        let mut c = inline(
            if on {
                l.pick("Wi-Fi in a scenario", "情景里的 Wi-Fi").into()
            } else {
                t!(l, "Switch Wi-Fi off in “{name}”?", "在「{name}」时关 Wi-Fi？")
            },
            what,
            action,
        );
        // Setting a scenario's Wi-Fi to off makes the device switch Wi-Fi off by itself later: a
        // Wi-Fi switch in effect (tier 2; tier 3 for a remote browser)
        if !on {
            c.cuts_uplink = true;
            if remote.remote() {
                c.tier = Tier::Dialog;
                c.downtime = Some(t!(l,
                    "From the moment the device enters “{name}” until it leaves it.",
                    "从设备进入「{name}」开始，到离开为止。"));
                c.recovery = Some(l.pick(
                    "Turn this switch back on, or turn the engine off (it puts Wi-Fi back on). Over Tailscale the page stays reachable through the mobile connection.",
                    "把这个开关重新打开，或关闭情景引擎（会把 Wi-Fi 打开）。经 Tailscale 访问时，这个页面仍可以通过移动网络打开。",
                ).into());
            }
        }
        Ok((
            Kind::Edit(Box::new(Wifi {
                id: a.id.clone(),
                on,
                name,
            })),
            c,
        ))
    })
    .await
}

/// The six numbers typed in the form, checked like the old page (plus whole numbers, which the
/// agent's integer fields need).
pub(crate) fn params_check(sent: &Value, l: Lang) -> Result<[f64; 6], String> {
    let mut v = [0.0; 6];
    for (i, (_, sig, ..)) in PARAMS.iter().enumerate() {
        let s = match &sent[*sig] {
            Value::String(s) => s.trim().to_owned(),
            Value::Number(n) => n.to_string(),
            _ => String::new(),
        };
        v[i] = s
            .parse::<f64>()
            .ok()
            .filter(|f| f.is_finite() && !s.is_empty())
            .ok_or_else(|| l.pick("Every field must be a number", "每一项都必须是数字").to_owned())?;
    }
    if v[..5].iter().any(|f| f.fract() != 0.0 || *f < 0.0 || *f > 4e9) {
        return Err(l
            .pick(
                "Intervals and confirmations must be whole numbers, 0 or more",
                "扫描间隔和确认次数必须是不小于 0 的整数",
            )
            .into());
    }
    if v[3] < 1.0 || v[4] < 1.0 {
        return Err(l
            .pick("Confirmations must be at least 1", "确认次数至少为 1")
            .into());
    }
    Ok(v)
}


async fn ask_timing(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    ask_with(&app, req, "sc-timing", true, |_, sent, l, _| {
        let v = params_check(sent, l)?;
        Ok((
            Kind::Edit(Box::new(Params(v))),
            inline(
                l.pick("Timing", "反应快慢").into(),
                l.pick(
                    "The device scans and decides with these numbers from its next scan on.",
                    "从下一次扫描起，设备按这些数字扫描和判断。",
                )
                .into(),
                l.pick("Confirm: save the timing", "确认：保存反应快慢")
                    .into(),
            ),
        ))
    })
    .await
}

async fn ask_defaults(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    ask_with(&app, req, "sc-setup", true, |st, _, l, _| {
        if !scenarios(st).is_empty() {
            return Err(l
                .pick("Scenarios are already set up", "情景已经设置过了")
                .into());
        }
        Ok((
            Kind::Edit(Box::new(Defaults)),
            inline(
                l.pick("Default scenarios", "默认情景").into(),
                l.pick(
                    "Create the two default scenarios — at home and away. Nothing happens until you add a home network, so this is safe to do now.",
                    "创建「在家」和「外出」两个默认情景。在你添加家里的网络之前什么都不会发生，所以现在创建是安全的。",
                )
                .into(),
                l.pick("Confirm: create defaults", "确认：创建默认情景")
                    .into(),
            ),
        ))
    })
    .await
}

// ---- sends ----

/// One write to the agent, once. `Err` = refused (and why); `Ok(NoReply)` = no answer.
async fn send_write(
    app: &Shared,
    who: &Caller,
    method: reqwest::Method,
    path: &str,
    body: &Value,
    l: Lang,
) -> Result<write::Sent, Outcome> {
    let r = if method == reqwest::Method::POST {
        crate::agent::post(app, who, path, body).await
    } else {
        crate::agent::send_json(app, who, method, path, body).await
    };
    match r {
        Ok(_) => Ok(write::Sent::Ok),
        Err(Fail::Unreachable) => Ok(write::Sent::NoReply),
        Err(f) => Err(Outcome::Failed(f.text(l))),
    }
}

/// Read the settings again until `check` holds (or `timeout`).
async fn held(
    app: &Shared,
    who: &Caller,
    timeout: Duration,
    check: impl Fn(&Value) -> bool,
) -> bool {
    write::read_back(
        &app.hub,
        timeout,
        Duration::from_secs(1),
        Duration::from_secs(2),
        {
            let check = &check;
            move || async move { read_state(app, who).await.is_ok_and(|s| check(&s)) }
        },
    )
    .await
}

fn verdict(held: bool, sent: write::Sent, applied: String) -> Outcome {
    if held {
        Outcome::Applied(applied)
    } else {
        sent.not_read()
    }
}

static EDIT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A config edit: fresh read, change the config, PUT it (the config only), read it back.
async fn run_edit(app: &Shared, who: &Caller, e: &dyn Edit, l: Lang) -> Outcome {
    let sent = {
        // two quick confirms must not both start from the same read
        let _g = EDIT_LOCK.lock().await;
        let st = match read_state(app, who).await {
            Ok(s) => s,
            Err(f) => return not_read(l, &f),
        };
        let mut cfg = st["config"].clone();
        let tpl = if e.needs_template() {
            match crate::agent::get(app, who, TEMPLATE).await {
                Ok(t) => Some(t),
                Err(f) => {
                    return Outcome::NotYet(t!(l,
                        "Can't read the default scenarios now ({e}); try again",
                        "现在读不到默认情景模板（{e}），稍后再试",
                        e = f.text(l)));
                }
            }
        } else {
            None
        };
        if let Err(m) = e.apply(&mut cfg, tpl.as_ref(), l) {
            return Outcome::NotYet(m);
        }
        match send_write(app, who, reqwest::Method::PUT, STATE, &cfg, l).await {
            Ok(s) => s,
            Err(o) => return o,
        }
    };
    let ok = held(app, who, READBACK, |s| e.holds(&s["config"])).await;
    verdict(ok, sent, e.applied(l))
}

async fn run(app: &Shared, who: &Caller, kind: Kind, l: Lang) -> Outcome {
    match kind {
        Kind::Engine(on) => {
            let sent = match send_write(
                app,
                who,
                reqwest::Method::PUT,
                ENABLED,
                &json!({"enabled": on}),
                l,
            )
            .await
            {
                Ok(s) => s,
                Err(o) => return o,
            };
            let t = if on { READBACK } else { ENGINE_OFF_READBACK };
            let ok = held(app, who, t, |s| s["enabled"].as_bool() == Some(on)).await;
            verdict(
                ok,
                sent,
                if on {
                    l.pick("Engine turned on", "引擎已开启").into()
                } else {
                    l.pick("Engine turned off", "引擎已关闭").into()
                },
            )
        }
        Kind::Pin(id) => {
            let sent = match send_write(
                app,
                who,
                reqwest::Method::POST,
                PIN,
                &json!({"id": id}),
                l,
            )
            .await
            {
                Ok(s) => s,
                Err(o) => return o,
            };
            let want = id.clone();
            let ok = held(app, who, READBACK, move |s| {
                s["pin"].as_str() == want.as_deref()
            })
            .await;
            let text = match &id {
                Some(i) => {
                    let name = match read_state(app, who).await {
                        Ok(s) => name_by_id(&s, i, l),
                        Err(_) => i.clone(),
                    };
                    t!(l, "Pinned to “{name}”", "已固定为「{name}」")
                }
                None => l.pick("Pin cleared", "已取消固定").into(),
            };
            verdict(ok, sent, text)
        }
        Kind::Edit(e) => run_edit(app, who, e.as_ref(), l).await,
        Kind::Scan => {
            match crate::agent::get_slow(app, who, SCAN, SCAN_TIMEOUT).await {
                Ok(v) => {
                    let n = nets(&v);
                    let found = n.len();
                    if let Some(sid) = &who.sid {
                        keep_scan(sid, n);
                    }
                    Outcome::Applied(match (found, l) {
                        (0, _) => l.pick("Nothing found.", "什么都没找到。").into(),
                        (n, Lang::En) => format!("Found {n} network{}", if n == 1 { "" } else { "s" }),
                        (n, Lang::Zh) => format!("找到 {n} 个网络"),
                    })
                }
                Err(Fail::Unreachable) => Outcome::NotYet(
                    l.pick(
                        "The scan got no answer in time. Try again in a moment.",
                        "扫描没有及时回应，稍后再试。",
                    )
                    .into(),
                ),
                Err(f) => Outcome::Ended(
                    t!(l, "Scan failed: {e}", "扫描失败：{e}", e = f.text(l)),
                    "bad",
                ),
            }
        }
    }
}

fn pending_text(k: &Kind, l: Lang) -> String {
    match k {
        Kind::Engine(true) => l.pick("Turning the engine on…", "正在打开引擎…").into(),
        Kind::Engine(false) => l
            .pick(
                "Turning the engine off… this can take up to a minute",
                "正在关闭引擎…最长要一分钟左右",
            )
            .into(),
        Kind::Pin(Some(_)) => l.pick("Pinning…", "正在固定…").into(),
        Kind::Pin(None) => l.pick("Clearing the pin…", "正在取消固定…").into(),
        Kind::Edit(e) => e.pending(l),
        Kind::Scan => l.pick("Scanning…", "扫描中…").into(),
    }
}

/// The confirm of any ask: send, read back, say.
async fn set(
    State(app): State<Shared>,
    Query(a): Query<Args>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let who = Caller::of(&app, &req);
    let l = ctx.lang;
    let id = area_id(&a.a);
    let h = take(&a.t).filter(|h| h.area == id);
    let pend = h
        .as_ref()
        .map(|h| pending_text(&h.kind, l))
        .unwrap_or_default();
    write::respond(pending(id, l, &pend), async move {
        let Some(h) = h else {
            return done(
                id,
                l,
                Outcome::NotYet(
                    l.pick("The form expired; press it again", "表单已过期，请再按一次")
                        .into(),
                ),
            );
        };
        let o = run(&app, &who, h.kind, l).await;
        let _ = app.kick.send(PATH);
        done(id, l, o)
    })
}

/// Cancel / Esc / a lost reply. The page also re-reads, which puts back anything the click
/// changed on the browser's side (a CHILL node `<select>`).
async fn cancel(
    State(app): State<Shared>,
    Query(a): Query<Args>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let q = write::CancelQuery { lost: a.lost };
    let _ = app.kick.send(PATH);
    one(area(
        area_id(&a.a),
        ctx.lang,
        ctx.remote,
        "",
        &write::cancel_phase(&q),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(configured: bool) -> Value {
        let scs = if configured {
            json!([
                {"id":"home","name":"在家","detect":{"type":"ssid","entries":[{"ssid":"Home-5G","bssid":"02:00:00:00:00:11"},{"ssid":"Home-2.4G"}]},
                 "actions":[{"method":"PUT","path":"/api/wifi/radio","body":{"ap_2g":false,"ap_5g":false},"snapshot":["wireless.main_2g.disabled"]},{"method":"PUT","path":"/api/device/power-save","body":{"x":1}}],"inhibit_sleep":true},
                {"id":"away","name":"外出","detect":{"type":"fallback"},"actions":[{"method":"PUT","path":"/api/wifi/radio","body":{"ap_2g":true,"ap_5g":true}}],"inhibit_sleep":false}
            ])
        } else {
            json!([])
        };
        json!({"enabled":true,"pin":null,"guard_takeover":false,"current":"home","candidate":"","hits":0,
            "last_switch":1_790_000_000,"last_scan":1_790_000_100,"last_error":null,
            "config":{"version":1,"home_mcc":"460","params":{"scan_interval_away_charging_secs":60,"scan_interval_away_battery_secs":300,"scan_interval_home_secs":60,"enter_hits":2,"exit_misses":2,"min_rssi_dbm":-70.0},"scenarios":scs},
            "names_en":{"home":"Home","away":"Away"},"sim_mcc":"460","pending_restore":[]})
    }

    fn got(st: Option<Value>, fail: Option<Fail>) -> Got {
        Got {
            st,
            fail,
            at: Some(SystemTime::now()),
            ..Got::default()
        }
    }

    fn cjk(s: &str) -> bool {
        s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c) || ('\u{3000}'..='\u{303f}').contains(&c) || ('\u{ff00}'..='\u{ffef}').contains(&c))
    }

    #[test]
    fn urls_are_safe_in_a_js_string() {
        assert_eq!(enc("a b'c&\\d"), "a%20b%27c%26%5Cd");
        assert_eq!(enc("Home-5G_2.4~"), "Home-5G_2.4~");
        assert_eq!(enc("日"), "%E6%97%A5");
    }

    #[test]
    fn home_entries_are_added_and_removed_by_name_and_hardware_address() {
        let mut cfg = state(true)["config"].clone();
        let add = |s: &str, b: &str| AddEntry { ssid: s.into(), bssid: b.into() };
        add("Cafe", "AA:BB").apply(&mut cfg, None, Lang::En).unwrap();
        assert!(add("Cafe", "aa:bb").holds(&cfg));
        // the same network twice, in any case: refused
        assert!(add("Cafe", "aa:bb").apply(&mut cfg, None, Lang::En).is_err());
        // a name-only entry has no bssid key
        add("Open", "").apply(&mut cfg, None, Lang::En).unwrap();
        let last = cfg["scenarios"][0]["detect"]["entries"].as_array().unwrap().last().unwrap().clone();
        assert_eq!(last, json!({"ssid":"Open"}));
        // everything else on the scenario is untouched
        assert_eq!(cfg["scenarios"][0]["actions"][0]["snapshot"][0], "wireless.main_2g.disabled");
        assert_eq!(cfg["scenarios"][0]["inhibit_sleep"], true);
        let del = |s: &str, b: &str| DelEntry { ssid: s.into(), bssid: b.into() };
        del("Cafe", "AA:BB").apply(&mut cfg, None, Lang::En).unwrap();
        assert!(del("Cafe", "AA:BB").holds(&cfg));
        assert!(del("Cafe", "AA:BB").apply(&mut cfg, None, Lang::En).is_err(), "already gone");
        // the name-only twin of a listed network is another entry
        del("Home-5G", "").apply(&mut cfg, None, Lang::En).unwrap_err();
        assert_eq!(home_entries(&cfg).len(), 3);
    }

    #[test]
    fn a_scenarios_wifi_goes_first_and_the_rest_is_kept() {
        let mut cfg = state(true)["config"].clone();
        let w = |id: &str, on| Wifi { id: id.into(), on, name: "x".into() };
        w("home", true).apply(&mut cfg, None, Lang::En).unwrap();
        let acts = cfg["scenarios"][0]["actions"].as_array().unwrap();
        assert_eq!(acts.len(), 2);
        assert_eq!(acts[0]["body"], json!({"ap_2g":true,"ap_5g":true}));
        assert_eq!(acts[1]["path"], "/api/device/power-save");
        assert!(w("home", true).holds(&cfg) && !w("home", false).holds(&cfg));
        // the away scenario never turns Wi-Fi off; an unknown id is gone
        assert!(w("away", false).apply(&mut cfg, None, Lang::En).is_err());
        assert!(w("nope", true).apply(&mut cfg, None, Lang::En).is_err());
        // a scenario without a Wi-Fi action counts as on
        cfg["scenarios"][1]["actions"] = json!([]);
        assert!(w("away", true).holds(&cfg));
    }

    #[test]
    fn defaults_only_fill_an_empty_config() {
        let tpl = json!({"version":1,"home_mcc":"460","params":{},"scenarios":[{"id":"home"}]});
        let mut empty = state(false)["config"].clone();
        Defaults.apply(&mut empty, Some(&tpl), Lang::En).unwrap();
        assert!(Defaults.holds(&empty));
        let mut full = state(true)["config"].clone();
        assert!(Defaults.apply(&mut full, Some(&tpl), Lang::En).is_err());
        assert!(Defaults.apply(&mut state(false)["config"].clone(), None, Lang::En).is_err());
    }

    #[test]
    fn timing_numbers_are_checked_and_sent_as_the_agent_wants_them() {
        let ok = json!({"_tc":"60","_tb":"300","_th":"90","_te":"3","_tx":"2","_tr":"-65.5"});
        let v = params_check(&ok, Lang::En).unwrap();
        assert_eq!(v, [60.0, 300.0, 90.0, 3.0, 2.0, -65.5]);
        let mut cfg = state(true)["config"].clone();
        Params(v).apply(&mut cfg, None, Lang::En).unwrap();
        assert!(Params(v).holds(&cfg));
        // integers go out as integers (a u32 field refuses 3.0)
        assert_eq!(cfg["params"]["enter_hits"].to_string(), "3");
        assert_eq!(cfg["params"]["min_rssi_dbm"].as_f64(), Some(-65.5));
        for (k, bad) in [("_tc", ""), ("_tb", "abc"), ("_th", "1.5"), ("_tc", "-1"), ("_te", "0"), ("_tx", "0")] {
            let mut x = ok.clone();
            x[k] = json!(bad);
            assert!(params_check(&x, Lang::En).is_err(), "{k}={bad}");
        }
        assert_eq!(
            params_check(&json!({}), Lang::Zh).unwrap_err(),
            "每一项都必须是数字"
        );
    }

    #[test]
    fn the_status_says_why() {
        let l = Lang::En;
        let v = view(&Got::default(), l, "");
        assert_eq!((v.tone, v.state.as_str()), ("neutral", "Reading scenarios…"));
        let v = view(&got(None, Some(Fail::Unreachable)), l, "");
        assert_eq!((v.tone, v.retry), ("bad", true));
        let v = view(&got(Some(state(false)), None), l, "");
        assert_eq!(v.state, "No scenarios yet");
        assert!(v.loaded && !v.configured);
        let mut s = state(true);
        let v = view(&got(Some(s.clone()), None), l, "");
        assert_eq!((v.tone, v.state.as_str()), ("ok", "Home"));
        assert!(!v.locked);
        s["enabled"] = json!(false);
        assert_eq!(view(&got(Some(s.clone()), None), l, "").state, "Engine off");
        s["enabled"] = json!(true);
        s["pin"] = json!("away");
        let v = view(&got(Some(s.clone()), None), l, "");
        assert!(v.reason.unwrap().contains("Pinned to “Away”"));
        assert_eq!(v.pin_name.as_deref(), Some("Away"));
        s["candidate"] = json!("away");
        s["hits"] = json!(1);
        assert!(view(&got(Some(s.clone()), None), l, "").meta.unwrap().contains("1 of 2"));
        // the read failed after one that worked: stale, controls off
        let v = view(&got(Some(s), Some(Fail::Unreachable)), l, "");
        assert_eq!(v.tone, "stale");
        assert!(v.locked && v.meta.unwrap().contains("Settings stopped at"));
    }

    #[test]
    fn already_listed_networks_are_hidden_from_the_scan() {
        let mut g = got(Some(state(true)), None);
        g.scan = Some(vec![
            Net { ssid: "Home-5G".into(), bssid: "02:00:00:00:00:11".into(), signal: -40 },
            Net { ssid: "Home-5G".into(), bssid: "02:00:00:00:00:99".into(), signal: -50 },
            Net { ssid: "".into(), bssid: "02:00:00:00:00:61".into(), signal: -83 },
        ]);
        let v = view(&g, Lang::En, "");
        let rows = v.scanned.unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].add.contains("s=Home-5G&b=02%3A00%3A00%3A00%3A00%3A99"));
        let html = Leaves { v: &view(&g, Lang::En, "") }.render().unwrap();
        assert!(html.contains("(hidden network)"));
        // a hidden network can't be added: no + on its row
        assert_eq!(html.matches("ask/add?").count(), 1);
    }

    #[test]
    fn the_english_page_has_no_chinese() {
        for g in [
            Got::default(),
            got(None, Some(Fail::Unreachable)),
            got(Some(state(false)), None),
            got(Some(state(true)), None),
            got(Some(state(true)), Some(Fail::Unreachable)),
        ] {
            let mut g = g;
            g.log = Some(String::new());
            let html = Leaves { v: &view(&g, Lang::En, "") }.render().unwrap();
            // device data (the CHILL node choices) is marked data-data and left out of the check
            let mut html = html;
            while let Some(i) = html.find("<select") {
                let j = html[i..].find("</select>").map_or(html.len(), |j| i + j + 9);
                html.replace_range(i..j, "");
            }
            assert!(!cjk(&html), "{}", html.chars().filter(|c| cjk(&c.to_string())).collect::<String>());
        }
        // and the Chinese one says it in Chinese
        let html = Leaves { v: &view(&got(Some(state(true)), None), Lang::Zh, "") }.render().unwrap();
        assert!(html.contains("代表「在家」的网络") && html.contains("反应快慢"));
    }

    #[test]
    fn the_timing_form_is_seeded_not_pushed_over_typing() {
        let v = view(&got(Some(state(true)), None), Lang::En, "");
        assert_eq!(
            serde_json::from_str::<Value>(&v.sigs).unwrap(),
            json!({"_tc":"60","_tb":"300","_th":"60","_te":"2","_tx":"2","_tr":"-70"})
        );
        let html = Leaves { v: &v }.render().unwrap();
        assert!(html.contains("data-signals__ifmissing"));
        assert!(v.dirty.contains("String($_tr).trim() !== '-70'"));
    }
}
