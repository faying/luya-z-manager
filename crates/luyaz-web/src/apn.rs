//! /router/apn (APN 设置), the old `router/apn/page.tsx`: which APN the data call dials with, APN
//! mode, operator IPv6, the manual profiles (enable / edit / delete / add) and the carrier's
//! auto-detected ones. Batch 3, step 3-3 (u60-web.md).
//!
//! Where each value comes from:
//! - mode, the manual and auto lists = u60d's `apn` block (`mode`, `manual[]`, `auto[]`; each
//!   profile `id, name, apn, username, has_password, auth, pdp, roaming_pdp, enabled`). Passwords
//!   never come back: the form's password is write-only (empty = keep). Empty pre-allocated slots
//!   (no name, no APN) are hidden, as the old page did;
//! - the IoT mark, "this SIM", "active for this SIM" and the notice about an automatic change =
//!   `/api/netinfo?lite=1&apn=1` (`apn.auto[].iot`, `picked_id`, `mode`, `in_use.apn`, `notice`),
//!   served by u60d since 8d-2;
//! - operator IPv6 = the agent's `/api/router/wan-ipv6` (`pdp_type` of the profile on cid 1,
//!   `wan_has_ipv6`): u60d's block has no PDP type of cid 1 yet.
//!
//! Writes, all tier 3, only through u60d `/control`: `apn.set_mode {mode}`, `apn.enable
//! {profile_id}`, `apn.modify {…}` (u60d runs these as a transaction on item `apn` when they change
//! the connection, else as a plain write: the reply's `op` says which), `apn.add`, `apn.delete`
//! (plain) and `apn.set_pdp_type {ipv6}` (plain, read back from the agent). A transaction is
//! followed in the `op` block (follow.rs, up to u60d's 90 s), a plain write is read back from the
//! `apn` block (15 s). Saving an edit with "make it the active profile" is two sends (modify, then
//! enable); a failed second step says the profile was saved but not switched.
//!
//! The form comes as a patch when "Add" / "Edit" is pressed (not in the first load). Its fields are
//! page-local signals (`_f…`); "Save" posts only those (Datastar `filterSignals`) to the ask, which
//! keeps them server-side under a random token for the confirm: the password is in that one POST
//! body and the `/control` body, never in a URL, the dialog's HTML or any push.
//!
//! "Use for this SIM" on a carrier profile (shown with two or more, old page D39) is not a
//! `/control` write: `POST /api/netinfo/apn {id: "auto…"}` (u60d compat `apn_use`, 202) copies the
//! candidate into a manual slot, enables it, sets manual mode and remembers it per ICCID; read back
//! from `/api/netinfo?lite=1&apn=1` until it is manual and dialling that APN (or `switch_error`).

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use askama::Template;
use axum::extract::{Query, State};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::topbar::Via;
use crate::agent::{Caller, Fail, Polled};
use crate::hub::{Link, St};
use crate::i18n::Lang;
use crate::pages::{Shared, StreamQuery, async_stream, patch, split_leaves};
use crate::write;

pub const PATH: &str = "/router/apn";
/// u60d confirms an APN change within 90 s (new connection + probe); a little more.
const FOLLOW: Duration = Duration::from_secs(100);
const SEND_TIMEOUT: Duration = Duration::from_secs(30);
const AGENT_POLL: Duration = Duration::from_secs(30);
/// How long a filled form waits for its confirm.
const STASH_TTL: Duration = Duration::from_secs(600);

// ---- the apn block ----

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub apn: String,
    pub username: String,
    pub has_password: bool,
    pub auth: i64,
    pub pdp: i64,
    pub roaming_pdp: i64,
    pub enabled: bool,
}

impl Profile {
    fn of(v: &Value) -> Profile {
        let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
        let n = |k: &str| v[k].as_i64().unwrap_or(0);
        Profile {
            id: s("id"),
            name: s("name"),
            apn: s("apn"),
            username: s("username"),
            has_password: v["has_password"].as_bool().unwrap_or(false),
            auth: n("auth"),
            pdp: n("pdp"),
            roaming_pdp: n("roaming_pdp"),
            enabled: v["enabled"].as_bool().unwrap_or(false),
        }
    }
    pub fn label(&self) -> &str {
        if self.name.is_empty() { &self.apn } else { &self.name }
    }
    /// "cmnet · IPv4v6 · PAP" (auth none is not shown).
    pub fn sub(&self) -> String {
        let mut p = vec![self.apn.clone(), pdp_word(self.pdp)];
        if self.auth != 0 {
            p.push(auth_word(self.auth));
        }
        p.retain(|x| !x.is_empty());
        p.join(" · ")
    }
}

pub fn pdp_word(n: i64) -> String {
    match n {
        1 => "IPv4".into(),
        2 => "IPv6".into(),
        3 => "IPv4v6".into(),
        0 => String::new(),
        n => n.to_string(),
    }
}

pub fn auth_word(n: i64) -> String {
    match n {
        0 => "none".into(),
        1 => "PAP".into(),
        2 => "CHAP".into(),
        n => n.to_string(),
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Apn {
    /// 1 = manual.
    pub mode: i64,
    pub manual: Vec<Profile>,
    pub auto: Vec<Profile>,
}

impl Apn {
    pub fn of(v: &Value) -> Apn {
        let list = |k: &str| -> Vec<Profile> {
            v[k].as_array()
                .map(|a| a.iter().map(Profile::of).filter(|p| !(p.name.is_empty() && p.apn.is_empty())).collect())
                .unwrap_or_default()
        };
        Apn {
            mode: v["mode"].as_i64().unwrap_or(0),
            manual: list("manual"),
            auto: list("auto"),
        }
    }
    pub fn manual_mode(&self) -> bool {
        self.mode == 1
    }
    /// The manual profile the data call uses (manual mode, enabled).
    pub fn in_use(&self, id: &str) -> bool {
        self.manual_mode() && self.manual.iter().any(|p| p.id == id && p.enabled)
    }
    /// The profile the data call dials with: manual mode → the enabled manual one; auto → the
    /// carrier's enabled one.
    pub fn dialing(&self) -> Option<&Profile> {
        if self.manual_mode() {
            self.manual.iter().find(|p| p.enabled)
        } else {
            self.auto.iter().find(|p| p.enabled)
        }
    }
    pub fn find(&self, id: &str) -> Option<&Profile> {
        self.manual.iter().find(|p| p.id == id)
    }
}

fn block(st: &St) -> Option<(Apn, bool)> {
    st.blocks
        .get("apn")
        .map(|b| (Apn::of(&b.data), b.stale || st.link != Link::Live))
}

pub fn apn(st: &St) -> Option<Apn> {
    block(st).map(|(a, _)| a)
}

fn stuck_reason(l: Lang) -> &'static str {
    l.pick(
        "Data service not responding · settings can't be changed for now",
        "数据服务没响应 · 暂时不能改设置",
    )
}

// ---- what the agent adds ----

/// From `/api/netinfo?lite=1&apn=1`'s `apn`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Marks {
    pub iot: HashSet<String>,
    pub picked: Option<String>,
    /// (zh, en)
    pub notice: Option<(String, String)>,
    /// netinfo's own view: manual mode, the APN dialled, a pick still running, its error.
    pub manual: bool,
    pub in_use_apn: String,
    pub switching: bool,
    pub switch_error: String,
}

impl Marks {
    pub fn of(v: &Value) -> Marks {
        let a = &v["apn"];
        Marks {
            iot: a["auto"]
                .as_array()
                .map(|l| {
                    l.iter()
                        .filter(|x| x["iot"].as_bool() == Some(true))
                        .filter_map(|x| x["id"].as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
            picked: a["picked_id"].as_str().filter(|s| !s.is_empty()).map(str::to_owned),
            notice: a["notice"]["text"].as_str().filter(|s| !s.is_empty()).map(|zh| {
                (zh.to_owned(), a["notice"]["text_en"].as_str().unwrap_or(zh).to_owned())
            }),
            manual: a["mode"] == "manual",
            in_use_apn: a["in_use"]["apn"].as_str().unwrap_or("").to_owned(),
            switching: a["switching"].as_bool().unwrap_or(false),
            switch_error: a["switch_error"].as_str().unwrap_or("").to_owned(),
        }
    }
    /// This SIM's pick is in use and dials `apn` (the old page's `pickedHere`).
    pub fn picked_here(&self, apn: &str) -> bool {
        self.manual && self.picked.is_some() && !apn.is_empty() && self.in_use_apn == apn
    }
}

/// Where a "use for this SIM" stands, from a netinfo read: Some(Ok) = done, Some(Err) = failed
/// (u60d's words), None = not yet.
pub fn pick_state(m: &Marks, apn: &str) -> Option<Result<(), String>> {
    if m.switching {
        None
    } else if m.manual && m.in_use_apn == apn {
        Some(Ok(()))
    } else if !m.switch_error.is_empty() {
        Some(Err(m.switch_error.clone()))
    } else {
        None
    }
}

/// From `/api/router/wan-ipv6`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wan {
    pub pdp: i64,
    pub live: bool,
}

impl Wan {
    pub fn of(v: &Value) -> Wan {
        Wan {
            pdp: v["pdp_type"].as_i64().unwrap_or(0),
            live: v["wan_has_ipv6"].as_bool().unwrap_or(false),
        }
    }
    pub fn on(self) -> bool {
        self.pdp == 2 || self.pdp == 3
    }
    pub fn pdp_text(&self) -> String {
        pdp_word(self.pdp)
    }
}

// ---- the view ----

pub struct Row {
    pub id: String,
    pub label: String,
    pub sub: String,
    /// "使用中" / "手动模式时用这条" / "本卡" / "本卡在用" / "物联网".
    pub marks: Vec<String>,
    /// Manual rows: can be enabled (not the enabled one), can be deleted (not in use).
    pub can_enable: bool,
    pub can_delete: bool,
    /// Carrier rows: "Use for this SIM" (two or more carrier profiles, not already this SIM's).
    pub can_pick: bool,
    pub apn: String,
}

pub struct View {
    pub l: Lang,
    pub stale: bool,
    pub loaded: bool,
    pub state: String,
    pub reason: String,
    pub warn: bool,
    pub notice: Option<String>,
    pub manual_mode: Option<bool>,
    pub v6: Option<Wan>,
    pub v6_err: Option<String>,
    pub manual: Vec<Row>,
    pub auto: Vec<Row>,
    pub dis: bool,
    pub why: Option<String>,
    pub txn: Option<String>,
    /// A "use for this SIM" is still switching (netinfo).
    pub picking: bool,
}

pub fn view(st: &St, marks: Option<&Marks>, wan: Option<&Result<Wan, Fail>>, l: Lang) -> View {
    let b = block(st);
    let a = b.as_ref().map(|(a, _)| a);
    let txn = crate::follow::running(st, &["apn"], l);
    let have_marks = marks.is_some();
    let m = marks.cloned().unwrap_or_default();
    let (state, reason, warn) = match a {
        None => (l.pick("Reading APN settings…", "正在读取 APN 设置…").to_string(), String::new(), false),
        Some(a) => {
            let d = a.dialing();
            let state = match (d, a.manual_mode()) {
                (Some(p), _) => match l {
                    Lang::En => format!("Data uses {}", p.label()),
                    Lang::Zh => format!("数据连接用的是「{}」", p.label()),
                },
                (None, true) => l.pick("Manual mode, but no profile is active", "手动模式，但没有启用的配置").into(),
                (None, false) => l.pick("Automatic APN", "自动 APN").into(),
            };
            let reason = if d.is_none() && a.manual_mode() {
                l.pick("Activate one of the manual profiles below, or switch back to Auto.", "在下面启用一个手动配置，或改回「自动」。")
            } else if a.manual_mode() {
                l.pick("Using manually configured APN profile", "使用手动配置的 APN 配置")
            } else {
                l.pick("Carrier APN detected automatically", "自动检测运营商 APN")
            };
            (state, reason.into(), d.is_none() && a.manual_mode())
        }
    };
    let manual = a
        .map(|a| {
            a.manual
                .iter()
                .map(|p| {
                    let mut marks = Vec::new();
                    if p.enabled {
                        marks.push(if a.manual_mode() {
                            l.pick("Active", "使用中")
                        } else {
                            l.pick("Used in Manual mode", "手动模式时用这条")
                        }
                        .to_string());
                    }
                    if m.picked.as_deref() == Some(p.id.as_str()) {
                        marks.push(l.pick("This SIM", "本卡").into());
                    }
                    Row {
                        id: p.id.clone(),
                        label: p.label().into(),
                        sub: p.sub(),
                        marks,
                        can_enable: !p.enabled,
                        can_delete: !a.in_use(&p.id),
                        can_pick: false,
                        apn: p.apn.clone(),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let auto = a
        .map(|a| {
            a.auto
                .iter()
                .map(|p| {
                    let mut marks = Vec::new();
                    if p.enabled && !a.manual_mode() {
                        marks.push(l.pick("Active", "使用中").to_string());
                    }
                    let here = m.picked_here(&p.apn);
                    if here {
                        marks.push(l.pick("Active for this SIM", "本卡在用").into());
                    }
                    if m.iot.contains(&p.id) {
                        marks.push(l.pick("IoT", "物联网").into());
                    }
                    Row {
                        id: p.id.clone(),
                        label: p.label().into(),
                        sub: [p.apn.clone(), pdp_word(p.pdp)].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" · "),
                        marks,
                        can_enable: false,
                        can_delete: false,
                        // needs netinfo's view (whether this SIM's pick is already it)
                        can_pick: have_marks && a.auto.len() >= 2 && !here && !p.apn.is_empty(),
                        apn: p.apn.clone(),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    View {
        l,
        stale: b.as_ref().is_some_and(|(_, s)| *s),
        loaded: a.is_some(),
        state,
        reason,
        warn,
        notice: m.notice.map(|(zh, en)| {
            let t = if l == Lang::En { en } else { zh };
            match l {
                Lang::En => format!("Changed automatically: {t}"),
                Lang::Zh => format!("已自动调整：{t}"),
            }
        }),
        manual_mode: a.map(Apn::manual_mode),
        v6: wan.and_then(|w| w.as_ref().ok().copied()),
        v6_err: wan.and_then(|w| w.as_ref().err()).map(|e| match e {
            Fail::SignedOut => e.text(l),
            _ => l.pick("Couldn't read WAN IPv6 state.", "读取 WAN IPv6 状态失败。").into(),
        }),
        manual,
        auto,
        dis: st.stuck || txn.is_some() || a.is_none(),
        why: st.stuck.then(|| stuck_reason(l).to_string()),
        txn,
        picking: m.switching,
    }
}

impl View {
    pub fn v6_on(&self) -> bool {
        self.v6.is_some_and(|w| w.on())
    }
}

#[derive(Template)]
#[template(path = "apn_leaves.html")]
struct Leaves<'a> {
    v: &'a View,
}

pub fn render(app: &Shared, marks: Option<&Marks>, wan: Option<&Result<Wan, Fail>>, l: Lang) -> Vec<(String, String)> {
    app.hub
        .read(|st| split_leaves(&Leaves { v: &view(st, marks, wan, l) }.render().unwrap()))
}

// ---- the form ----

/// What a filled form sends (the ask's body: the `_f…` signals).
#[derive(Deserialize, Clone, Debug, Default)]
pub struct FormIn {
    #[serde(default, rename = "_fid")]
    pub id: String,
    #[serde(default, rename = "_fname")]
    pub name: String,
    #[serde(default, rename = "_fapn")]
    pub apn: String,
    #[serde(default, rename = "_fuser")]
    pub user: String,
    #[serde(default, rename = "_fpw")]
    pub pw: String,
    #[serde(default, rename = "_fpdp")]
    pub pdp: Value,
    #[serde(default, rename = "_fauth")]
    pub auth: Value,
    #[serde(default, rename = "_fact")]
    pub act: bool,
}

fn int(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

/// The sends of a save, checked: Err = why not (said in the form's area).
#[derive(Clone, Debug, PartialEq)]
pub struct Save {
    /// None: add.
    pub id: Option<String>,
    pub name: String,
    pub apn: String,
    /// The steps: `(action, params)`; params may hold the password.
    pub steps: Vec<(&'static str, Value)>,
    /// The edit changes the connection (edits the profile in use, or switches to it in manual mode).
    pub redials: bool,
    /// The edit also makes it the enabled profile in auto mode (no redial).
    pub picks: bool,
}

pub fn check_save(f: &FormIn, a: Option<&Apn>, stuck: bool, l: Lang) -> Result<Save, write::Outcome> {
    if stuck {
        return Err(write::Outcome::NotYet(stuck_reason(l).into()));
    }
    let Some(a) = a else {
        return Err(write::Outcome::NotYet(l.pick("APN settings not read yet; try again shortly", "还没读到 APN 设置，稍后再试").into()));
    };
    let name = f.name.trim().to_string();
    let apn = f.apn.trim().to_string();
    if name.is_empty() || apn.is_empty() {
        return Err(write::Outcome::NotYet(l.pick("Name and APN are required", "名称和 APN 为必填项").into()));
    }
    let edit = (!f.id.is_empty()).then(|| f.id.clone());
    let editing = match &edit {
        Some(id) => Some(a.find(id).ok_or_else(|| write::Outcome::Failed(l.pick("that profile is gone", "这条配置已经不在了").into()))?),
        None => None,
    };
    if a.manual.iter().any(|p| p.name == name && Some(&p.id) != edit.as_ref()) {
        return Err(write::Outcome::NotYet(l.pick("An APN with this name already exists", "已存在同名 APN").into()));
    }
    let pdp = int(&f.pdp).filter(|n| (1..=3).contains(n)).unwrap_or(1);
    let auth = int(&f.auth).filter(|n| (0..=2).contains(n)).unwrap_or(0);
    let mut p = json!({"name": name, "apn": apn, "auth_mode": auth, "pdp_type": pdp});
    let user = f.user.trim();
    if editing.is_some() || !user.is_empty() {
        p["username"] = json!(user);
    }
    if !f.pw.is_empty() {
        p["password"] = json!(f.pw);
    }
    // roaming PDP: an edit keeps the profile's own, a new profile takes the PDP type (old page)
    p["roaming_pdp_type"] = json!(editing.map_or(pdp, |e| if e.roaming_pdp > 0 { e.roaming_pdp } else { pdp }));
    let mut steps = Vec::new();
    let (mut redials, mut picks) = (false, false);
    match editing {
        Some(e) => {
            p["profile_id"] = json!(e.id);
            steps.push(("apn.modify", p));
            let switch = f.act && !e.enabled;
            if switch {
                steps.push(("apn.enable", json!({"profile_id": e.id})));
            }
            redials = a.in_use(&e.id) || (switch && a.manual_mode());
            picks = switch && !a.manual_mode();
        }
        None => steps.push(("apn.add", p)),
    }
    Ok(Save { id: edit, name, apn, steps, redials, picks })
}

/// Filled forms waiting for their confirm, by a random token (only the server knows the password).
fn stash() -> &'static Mutex<HashMap<String, (Instant, Save)>> {
    static S: std::sync::OnceLock<Mutex<HashMap<String, (Instant, Save)>>> = std::sync::OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

fn put(s: Save) -> String {
    let t = crate::auth::random_id();
    let mut m = stash().lock().unwrap();
    m.retain(|_, (at, _)| at.elapsed() < STASH_TTL);
    m.insert(t.clone(), (Instant::now(), s));
    t
}

fn take(t: &str) -> Option<Save> {
    let mut m = stash().lock().unwrap();
    m.remove(t).filter(|(at, _)| at.elapsed() < STASH_TTL).map(|(_, s)| s)
}

#[derive(Template)]
#[template(path = "apn_form.html")]
struct Form<'a> {
    l: Lang,
    edit: Option<&'a Profile>,
    /// The `data-signals` object (JSON; askama escapes it for the attribute).
    sigs: String,
}

impl Form<'_> {
    fn has_pw(&self) -> bool {
        self.edit.is_some_and(|p| p.has_password)
    }
}

/// `#ap-form` for adding (None) or editing a profile; the signals start at its values.
fn form(p: Option<&Profile>, l: Lang) -> String {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let d = Profile::default();
    let q = p.unwrap_or(&d);
    let sigs = json!({
        "_fn": n, "_fid": q.id, "_fname": q.name, "_fapn": q.apn, "_fuser": q.username, "_fpw": "",
        "_fpdp": (if q.pdp == 0 { 1 } else { q.pdp }).to_string(), "_fauth": q.auth.to_string(), "_fact": q.enabled,
    });
    Form { l, edit: p, sigs: sigs.to_string() }.render().unwrap()
}

// ---- writes ----

#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    Mode(bool),
    V6(bool),
    Enable(String),
    Delete(String),
    Save(String),
    /// Use this carrier profile (`auto…`) for this SIM only.
    Pick(String),
}

impl Act {
    fn area_id(&self) -> &'static str {
        match self {
            Act::Mode(_) => "ap-mode",
            Act::V6(_) => "ap-v6",
            Act::Enable(_) | Act::Delete(_) => "ap-row",
            Act::Save(_) => "ap-save",
            Act::Pick(_) => "ap-pick",
        }
    }
    fn set_url(&self) -> String {
        match self {
            Act::Mode(on) => format!("{PATH}/mode/set?on={on}"),
            Act::V6(on) => format!("{PATH}/v6/set?on={on}"),
            Act::Enable(id) => format!("{PATH}/enable/set?id={id}"),
            Act::Delete(id) => format!("{PATH}/delete/set?id={id}"),
            Act::Save(t) => format!("{PATH}/save/set?t={t}"),
            Act::Pick(id) => format!("{PATH}/pick/set?id={id}"),
        }
    }
    fn cancel_url(&self) -> String {
        match self {
            Act::Mode(_) => format!("{PATH}/mode/cancel"),
            Act::V6(_) => format!("{PATH}/v6/cancel"),
            Act::Enable(_) | Act::Delete(_) => format!("{PATH}/row/cancel"),
            Act::Save(_) => format!("{PATH}/save/cancel"),
            Act::Pick(_) => format!("{PATH}/pick/cancel"),
        }
    }
}

fn area(act: &Act, l: Lang, remote: Via, phase: &write::Phase) -> String {
    let (set, cancel) = (act.set_url(), act.cancel_url());
    write::Area { id: act.area_id(), l, remote, set: &set, cancel: &cancel, phase }.html()
}

/// A profile id as u60d takes it (`manuN`, letters and digits).
fn manual_id(id: &str) -> bool {
    id.starts_with("manu") && id.len() <= 16 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// A carrier candidate id (`auto` + digits / letters; bare `auto` = back to Auto, not offered here).
fn auto_id(id: &str) -> bool {
    id.len() > 4 && id.starts_with("auto") && id.len() <= 24 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn downtime(l: Lang) -> String {
    l.pick(
        "The mobile data connection drops for about 30 seconds while it re-dials with the new APN.",
        "移动数据断开约 30 秒，用新的 APN 重新拨号。",
    )
    .into()
}

fn recovery(l: Lang) -> String {
    l.pick(
        "Switch APN Mode back to Auto on this page (or on the device's touchscreen). If the page can't be reached, connect to the U60's Wi-Fi and open it from there.",
        "在这个页面（或设备触屏上）把 APN 模式改回「自动」。如果打不开这个页面，连上 U60 的 Wi-Fi 再打开。",
    )
    .into()
}

fn dialog(title: String, what: String, down: Option<String>, rec: Option<String>, action: &str, cuts: bool) -> write::Confirm {
    write::Confirm {
        tier: write::Tier::Dialog,
        title,
        what,
        downtime: down,
        recovery: rec,
        action: action.into(),
        cuts_uplink: cuts,
        word: None,
        danger: false,
        revert: crate::write::Revert::No,
    }
}

/// The confirm of an action (the old page's words), or why it can't be sent.
pub fn confirm(act: &Act, a: Option<&Apn>, save: Option<&Save>, l: Lang) -> Result<write::Confirm, write::Outcome> {
    let not_read = || write::Outcome::NotYet(l.pick("APN settings not read yet; try again shortly", "还没读到 APN 设置，稍后再试").into());
    let gone = || write::Outcome::Failed(l.pick("that profile is gone", "这条配置已经不在了").into());
    Ok(match act {
        Act::Mode(on) => {
            a.ok_or_else(not_read)?;
            if *on {
                dialog(
                    l.pick("Switch to manual APN?", "改用手动 APN？").into(),
                    l.pick("The data connection re-dials with the active manual profile instead of the carrier's APN. With no working manual profile, mobile data stops.", "数据连接改用启用中的手动配置重新拨号，不再用运营商下发的 APN。手动配置不可用时，移动数据会断。").into(),
                    Some(downtime(l)),
                    Some(recovery(l)),
                    l.pick("Use manual APN", "改用手动 APN"),
                    true,
                )
            } else {
                dialog(
                    l.pick("Switch to automatic APN?", "改用自动 APN？").into(),
                    l.pick("The data connection re-dials with the APN the carrier supplies. Manual profiles stay saved.", "数据连接改用运营商下发的 APN 重新拨号。手动配置仍然保留。").into(),
                    Some(downtime(l)),
                    Some(recovery(l)),
                    l.pick("Use automatic APN", "改用自动 APN"),
                    true,
                )
            }
        }
        Act::V6(on) => dialog(
            if *on { l.pick("Request IPv6 from the carrier?", "向运营商请求 IPv6？") } else { l.pick("Stop requesting IPv6?", "不再请求 IPv6？") }.into(),
            if *on {
                l.pick("The dialing APN changes to IPv4v6 and the IPv6 leg of the data call comes up. Devices may start using IPv6 past an IPv4-only proxy.", "拨号 APN 改成 IPv4v6，数据连接的 IPv6 支路会建立。设备可能开始用 IPv6，绕过只支持 IPv4 的代理。")
            } else {
                l.pick("The dialing APN changes to IPv4-only. The IPv6 leg drops now; IPv4 stays connected. It stays this way after reconnects and reboots.", "拨号 APN 改成只用 IPv4。IPv6 支路马上断开，IPv4 保持连接。重连、重启后仍保持这样。")
            }
            .into(),
            Some(l.pick("IPv4 is not interrupted. Connections that were using IPv6 drop and reconnect over IPv4.", "IPv4 不中断。正在走 IPv6 的连接会断开，再经 IPv4 重连。").into()),
            Some(l.pick("Flip this switch back.", "把这个开关拨回去。").into()),
            if *on { l.pick("Request IPv6", "请求 IPv6") } else { l.pick("Use IPv4 only", "只用 IPv4") },
            false,
        ),
        Act::Enable(id) => {
            let a = a.ok_or_else(not_read)?;
            let p = a.find(id).ok_or_else(gone)?;
            let title = match l {
                Lang::En => format!("Switch to “{}”?", p.label()),
                Lang::Zh => format!("切换到「{}」？", p.label()),
            };
            if a.manual_mode() {
                let what = match l {
                    Lang::En => format!("The data connection re-dials with APN {}. If the carrier doesn't accept it, mobile data stops.", p.apn),
                    Lang::Zh => format!("数据连接用 APN {} 重新拨号。运营商不接受这个 APN 时，移动数据会断。", p.apn),
                };
                dialog(title, what, Some(downtime(l)), Some(recovery(l)), l.pick("Activate", "启用"), true)
            } else {
                let what = match l {
                    Lang::En => format!("“{}” becomes the active manual profile. APN Mode is Auto, so it is used once you switch to Manual.", p.label()),
                    Lang::Zh => format!("「{}」成为启用中的手动配置。现在是自动模式，改成手动后才会用它。", p.label()),
                };
                dialog(title, what, None, None, l.pick("Activate", "启用"), false)
            }
        }
        Act::Delete(id) => {
            let a = a.ok_or_else(not_read)?;
            let p = a.find(id).ok_or_else(gone)?;
            if a.in_use(id) {
                return Err(write::Outcome::NotYet(l.pick("Cannot delete the active APN", "无法删除正在使用的 APN").into()));
            }
            dialog(
                match l {
                    Lang::En => format!("Delete “{}”?", p.label()),
                    Lang::Zh => format!("删除「{}」？", p.label()),
                },
                l.pick("The profile is removed from the device. It isn't the active one, so the data connection is not affected.", "这个配置会从设备上删掉。它不是正在用的配置，不影响数据连接。").into(),
                None,
                Some(l.pick("Add it again with “Add Profile” (note the APN, username and password first).", "用「添加配置」重新加回来（先记下 APN、用户名和密码）。").into()),
                l.pick("Delete", "删除"),
                false,
            )
        }
        Act::Pick(id) => {
            let a = a.ok_or_else(not_read)?;
            let p = a.auto.iter().find(|p| &p.id == id).ok_or_else(gone)?;
            dialog(
                match l {
                    Lang::En => format!("Use {} for this SIM?", p.apn),
                    Lang::Zh => format!("只给这张卡用 {}？", p.apn),
                },
                l.pick(
                    "It is saved as a manual profile and the data connection re-dials with it. Only this SIM uses it: when another SIM goes in, APN goes back to Auto; when this SIM is back, it is used again.",
                    "它会存成一条手动 APN，数据连接用它重拨。只有这张卡用它：换别的卡时 APN 回到「自动」，换回这张卡时再用它。",
                )
                .into(),
                Some(downtime(l)),
                Some(recovery(l)),
                l.pick("Use for this SIM", "只给这张卡用"),
                true,
            )
        }
        Act::Save(_) => {
            let s = save.ok_or_else(not_read)?;
            let (title, what) = match (&s.id, l) {
                (Some(_), Lang::En) => (format!("Save changes to “{}”?", s.name), if s.redials {
                    format!("The data connection re-dials with APN {}. If the carrier doesn't accept it, mobile data stops.", s.apn)
                } else if s.picks {
                    format!("The saved profile changes and becomes the active manual profile. APN Mode is Auto, so it is used once you switch to Manual.")
                } else {
                    "The saved profile changes. It isn't in use, so the data connection is not affected.".into()
                }),
                (Some(_), Lang::Zh) => (format!("保存对「{}」的修改？", s.name), if s.redials {
                    format!("数据连接用 APN {} 重新拨号。运营商不接受这个 APN 时，移动数据会断。", s.apn)
                } else if s.picks {
                    "保存的配置会改，并成为启用中的手动配置。现在是自动模式，改成手动后才会用它。".into()
                } else {
                    "只改保存的配置。它没有在用，不影响数据连接。".into()
                }),
                (None, Lang::En) => (format!("Add “{}”?", s.name), "A new manual profile is saved. It isn't used until you activate it.".into()),
                (None, Lang::Zh) => (format!("添加「{}」？", s.name), "保存一个新的手动配置。启用之前不会用它。".into()),
            };
            dialog(
                title,
                what,
                s.redials.then(|| downtime(l)),
                s.redials.then(|| recovery(l)),
                if s.id.is_some() { l.pick("Save Changes", "保存修改") } else { l.pick("Add Profile", "添加配置") },
                s.redials,
            )
        }
    })
}

/// The steps of an action (the save's come from its stash).
fn steps(act: &Act, save: Option<&Save>) -> Vec<(&'static str, Value)> {
    match act {
        Act::Mode(on) => vec![("apn.set_mode", json!({"mode": i64::from(*on)}))],
        Act::V6(on) => vec![("apn.set_pdp_type", json!({"ipv6": on}))],
        Act::Enable(id) => vec![("apn.enable", json!({"profile_id": id}))],
        Act::Delete(id) => vec![("apn.delete", json!({"profile_id": id}))],
        Act::Save(_) => save.map(|s| s.steps.clone()).unwrap_or_default(),
        Act::Pick(_) => Vec::new(),
    }
}

/// The `apn` block shows step `action` done.
pub fn read_back(action: &str, p: &Value, save: Option<&Save>, a: &Apn) -> bool {
    let id = p["profile_id"].as_str().unwrap_or("");
    match action {
        "apn.set_mode" => a.mode == p["mode"].as_i64().unwrap_or(-1),
        "apn.enable" => a.find(id).is_some_and(|x| x.enabled),
        "apn.delete" => a.find(id).is_none(),
        "apn.modify" | "apn.add" => {
            let (name, apn) = (p["name"].as_str().unwrap_or(""), p["apn"].as_str().unwrap_or(""));
            let hit = |x: &Profile| x.name == name && x.apn == apn;
            if action == "apn.modify" {
                a.find(id).is_some_and(hit)
            } else {
                let _ = save;
                a.manual.iter().any(hit)
            }
        }
        _ => false,
    }
}

fn applied(act: &Act, l: Lang) -> String {
    match act {
        Act::Mode(true) => l.pick("APN mode set to Manual", "APN 模式已设为手动"),
        Act::Mode(false) => l.pick("APN mode set to Auto", "APN 模式已设为自动"),
        Act::V6(true) => l.pick("Operator IPv6 enabled", "已启用运营商 IPv6"),
        Act::V6(false) => l.pick("Operator IPv6 disabled; IPv4 stays connected", "已禁用运营商 IPv6，IPv4 保持连接"),
        Act::Enable(_) => l.pick("APN activated", "APN 已启用"),
        Act::Delete(_) => l.pick("APN deleted", "APN 已删除"),
        Act::Save(_) => l.pick("APN updated", "APN 已更新"),
        Act::Pick(_) => l.pick("Now used for this SIM", "已只给这张卡用"),
    }
    .into()
}

fn one(html: String) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(futures_util::stream::once(async move { Ok(patch(&html)) }))
}

fn gate(app: &Shared, l: Lang) -> Result<Option<Apn>, write::Outcome> {
    let (a, stuck) = app.hub.read(|st| (apn(st), st.stuck));
    if stuck {
        return Err(write::Outcome::NotYet(stuck_reason(l).into()));
    }
    Ok(a)
}

fn ask_html(act: &Act, save: Option<&Save>, l: Lang, remote: Via, app: &Shared) -> String {
    let phase = match gate(app, l).and_then(|a| confirm(act, a.as_ref(), save, l)) {
        Ok(c) => write::Phase::Ask(c),
        Err(o) => write::Phase::Done(o),
    };
    area(act, l, remote, &phase)
}

async fn run(app: Shared, act: Act, save: Option<Save>, who: Caller, l: Lang, tx: tokio::sync::mpsc::Sender<Event>) {
    let a = match gate(&app, l) {
        Ok(a) => a,
        Err(o) => {
            let _ = tx.send(patch(&area(&act, l, Via::Local, &write::Phase::Done(o)))).await;
            return;
        }
    };
    // checked again at the send (the list may have changed since the dialog opened)
    if let Err(o) = confirm(&act, a.as_ref(), save.as_ref(), l) {
        let _ = tx.send(patch(&area(&act, l, Via::Local, &write::Phase::Done(o)))).await;
        return;
    }
    let pending = |t: String| area(&act, l, Via::Local, &write::Phase::Pending(t));
    if tx.send(patch(&pending(l.pick("Sending…", "正在发送…").into()))).await.is_err() {
        return;
    }
    if let Act::Pick(id) = &act {
        let apn = a.as_ref().and_then(|a| a.auto.iter().find(|p| &p.id == id)).map(|p| p.apn.clone()).unwrap_or_default();
        let o = pick(&app, &who, id, &apn, l, &tx, &pending).await;
        // the rows' marks come from netinfo: re-read now, not at the next 30 s poll
        let _ = app.kick.send(PATH);
        let _ = tx.send(patch(&area(&act, l, Via::Local, &write::Phase::Done(o)))).await;
        return;
    }
    let all = steps(&act, save.as_ref());
    let n = all.len();
    let mut last = write::Outcome::Accepted;
    for (i, (action, params)) in all.into_iter().enumerate() {
        let op_id = write::next_op_id();
        let body = json!({"action": action, "params": params, "source": "web", "op_id": op_id});
        let changes = app.hub.watch();
        let (sent, reply) = match write::send_body_reply(&app.http, &app.control_url, &body, Some(SEND_TIMEOUT), l).await {
            Ok(r) => r,
            Err(o) => {
                last = if i > 0 { saved_not_switched(&o, l) } else { o };
                break;
            }
        };
        let txn = reply["op"]["op_id"].is_string();
        let o = if let Act::V6(on) = act {
            // the PDP type of cid 1 is the agent's to read
            let read = v6_read(&app, &who, on).await;
            if read { write::Outcome::Applied(applied(&act, l)) } else { sent.not_read() }
        } else {
            let sv = save.clone();
            let p2 = params.clone();
            let Some(f) = crate::follow::follow(
                &app,
                &tx,
                &op_id,
                changes,
                if txn { FOLLOW } else { write::READBACK },
                Duration::ZERO,
                l,
                &pending,
                move |st| apn(st).is_some_and(|a| read_back(action, &p2, sv.as_ref(), &a)),
            )
            .await
            else {
                return;
            };
            crate::follow::outcome(&f, sent, applied(&act, l), l)
        };
        let failed = !matches!(o, write::Outcome::Applied(_) | write::Outcome::Accepted | write::Outcome::AcceptedSaid(_))
            && !matches!(&o, write::Outcome::Ended(_, t) if *t == "ok");
        last = if failed && i > 0 { saved_not_switched(&o, l) } else { o };
        if failed || i + 1 == n {
            break;
        }
        let _ = tx.send(patch(&pending(l.pick("Saved · making it the active profile…", "已保存 · 正在设为使用中…").into()))).await;
    }
    if let (write::Outcome::Applied(t), Some(s)) = (&mut last, &save)
        && s.id.is_none()
    {
        *t = l.pick("APN added", "APN 已添加").into();
    }
    let ok = matches!(last, write::Outcome::Applied(_)) || matches!(&last, write::Outcome::Ended(_, t) if *t == "ok");
    let mut html = area(&act, l, Via::Local, &write::Phase::Done(last));
    if ok && matches!(act, Act::Save(_)) {
        html.push('\n');
        html.push_str("<div id=\"ap-form\"></div>");
    }
    let _ = tx.send(patch(&html)).await;
}

/// "Use for this SIM": u60d answers 202 and switches on a thread (copy into a manual slot, enable,
/// manual mode, remember per ICCID); read back from netinfo as the old page did.
async fn pick(
    app: &Shared,
    who: &Caller,
    id: &str,
    apn: &str,
    l: Lang,
    tx: &tokio::sync::mpsc::Sender<Event>,
    pending: &impl Fn(String) -> String,
) -> write::Outcome {
    match crate::agent::post_raw(app, who, "/api/netinfo/apn", &json!({"id": id})).await {
        Err(Fail::Unreachable) => return write::Outcome::Unknown,
        Err(f) => return write::Outcome::Failed(f.text(l)),
        Ok((code, v)) if !(200..300).contains(&code) || v["ok"] == false => {
            return write::Outcome::Failed(crate::agent::refused(code, &v).text(l));
        }
        Ok(_) => {}
    }
    let say = l.pick(
        "Switching · mobile data re-dials with the new APN…",
        "正在切换 · 移动数据用新的 APN 重新拨号…",
    );
    if tx.send(patch(&pending(say.into()))).await.is_err() {
        return write::Outcome::Accepted;
    }
    let end = std::sync::Arc::new(Mutex::new(None));
    let got = {
        let end = end.clone();
        write::read_back(&app.hub, FOLLOW, Duration::from_secs(3), Duration::from_secs(3), move || {
            let (app, who, end, apn) = (app.clone(), who.clone(), end.clone(), apn.to_owned());
            async move {
                let Ok(v) = crate::agent::get(&app, &who, "/api/netinfo?lite=1&apn=1").await else {
                    return false;
                };
                let s = pick_state(&Marks::of(&v), &apn);
                let done = s.is_some();
                *end.lock().unwrap() = s;
                done
            }
        })
        .await
    };
    let s = end.lock().unwrap().take();
    match s {
        Some(Ok(())) if got => write::Outcome::Applied(applied(&Act::Pick(id.into()), l)),
        Some(Err(e)) => write::Outcome::Failed(match l {
            Lang::En => luyaz_api::netinfo::error_en(&e).unwrap_or_else(|| "the switch did not go through".into()),
            Lang::Zh => e,
        }),
        _ => write::Outcome::Accepted,
    }
}

/// Step 2 of a save failed: the edit is saved, the switch is not.
fn saved_not_switched(o: &write::Outcome, l: Lang) -> write::Outcome {
    let why = o.text(l);
    write::Outcome::Failed(match l {
        Lang::En => format!("the profile was saved but not made active ({why})"),
        Lang::Zh => format!("配置已保存，但没有设为使用中（{why}）"),
    })
}

/// The agent shows the IPv6 setting (`pdp_type`) as wanted within the readback time (asked with
/// the session the write came from).
async fn v6_read(app: &Shared, who: &Caller, on: bool) -> bool {
    write::read_back(&app.hub, write::READBACK, Duration::from_secs(2), Duration::from_secs(3), || {
        let (app, who) = (app.clone(), who.clone());
        async move {
            crate::agent::get(&app, &who, "/api/router/wan-ipv6")
                .await
                .is_ok_and(|v| Wan::of(&v).on() == on)
        }
    })
    .await
}

// ---- the page and its stream ----

pub struct Poll {
    l: Lang,
    who: Caller,
    marks: Option<Marks>,
    wan: Option<Result<Wan, Fail>>,
}

impl Polled for Poll {
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        let (m, w) = tokio::join!(
            crate::agent::get(app, &self.who, "/api/netinfo?lite=1&apn=1"),
            crate::agent::get(app, &self.who, "/api/router/wan-ipv6"),
        );
        if let Ok(m) = m {
            self.marks = Some(Marks::of(&m));
        }
        self.wan = Some(w.map(|v| Wan::of(&v)));
        (render(app, self.marks.as_ref(), self.wan.as_ref(), self.l), AGENT_POLL)
    }
    fn watches(&self, block: &str) -> bool {
        matches!(block, "apn" | "op" | "exec")
    }
    fn render(&mut self, app: &Shared) -> Option<Vec<(String, String)>> {
        Some(render(app, self.marks.as_ref(), self.wan.as_ref(), self.l))
    }
}

#[derive(Template)]
#[template(path = "apn.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    /// The per-card roaming setting for the agent (second phase D6); None when the core has none.
    agent: Option<AgentRoam>,
    leaves: Vec<(String, String)>,
    mode_op: String,
    v6_op: String,
    row_op: String,
    save_op: String,
    pick_op: String,
}

impl Page {
    fn leaf(&self, id: &str) -> &str {
        self.leaves.iter().find(|(k, _)| k == id).map_or("", |(_, h)| h.as_str())
    }
}

/// The card in now and whether its owner marked roaming on it as not expensive.
pub struct AgentRoam {
    pub iccid: String,
    pub tail: String,
    pub cheap: bool,
}

/// The core's per-card setting for the SIM in now; None without a card number or a core that
/// does not have it.
async fn agent_roam(app: &Shared, who: &Caller) -> Option<AgentRoam> {
    let v = crate::agent::get(app, who, "/api/agent/roam-cheap").await.ok()?;
    let iccid = v["current_iccid"].as_str().filter(|s| !s.is_empty())?.to_owned();
    let tail = iccid[iccid.len().saturating_sub(4)..].to_owned();
    Some(AgentRoam {
        iccid,
        tail,
        cheap: v["current_cheap"] == true,
    })
}

/// POST /router/apn/agent-roam (a form of this page, a person's session only): set the card's
/// setting on the core, back to the page.
pub async fn agent_roam_set(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    let who = Caller::of(&app, &req);
    let Ok(b) = axum::body::to_bytes(req.into_body(), 4096).await else {
        return axum::http::StatusCode::BAD_REQUEST.into_response();
    };
    // two plain fields (the card number is digits and letters): no decoding needed
    let field = |k: &str| {
        std::str::from_utf8(&b)
            .ok()?
            .split('&')
            .find_map(|kv| kv.strip_prefix(k)?.strip_prefix('='))
            .map(str::to_owned)
    };
    let (Some(iccid), Some(cheap)) = (field("iccid"), field("cheap")) else {
        return axum::http::StatusCode::BAD_REQUEST.into_response();
    };
    let body = json!({"iccid": iccid, "cheap": cheap == "1"});
    if let Err(e) = crate::agent::put_raw(&app, &who, "/api/agent/roam-cheap", &body).await {
        eprintln!("[apn] roam setting not saved: {e:?}");
    }
    axum::response::Redirect::to(PATH).into_response()
}

pub async fn page(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    let headers = req.headers().clone();
    let agent = agent_roam(&app, &Caller::of(&app, &req)).await;
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let shell = app.hub.read(|st| {
        crate::shell::Shell::new(&headers, PATH).live(st, &ctx, Some(format!("{PATH}/events?g={}", st.generation)))
    });
    let idle = write::Phase::Idle;
    let html = Page {
        shell,
        l,
        agent,
        leaves: render(&app, None, None, l),
        mode_op: area(&Act::Mode(false), l, ctx.remote, &idle),
        v6_op: area(&Act::V6(false), l, ctx.remote, &idle),
        row_op: area(&Act::Enable(String::new()), l, ctx.remote, &idle),
        save_op: area(&Act::Save(String::new()), l, ctx.remote, &idle),
        pick_op: area(&Act::Pick(String::new()), l, ctx.remote, &idle),
    }
    .render()
    .unwrap();
    crate::assets::maybe_gzip(&headers, "text/html; charset=utf-8", html.into_bytes(), "no-store")
}

pub async fn events(State(app): State<Shared>, Query(q): Query<StreamQuery>, req: axum::extract::Request) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let p = Poll { l: ctx.lang, who: Caller::of(&app, &req), marks: None, wan: None };
    crate::agent::stream(app, ctx, q.g, PATH, p)
}

#[derive(Deserialize, Default)]
pub struct OnQuery {
    #[serde(default)]
    on: bool,
}

#[derive(Deserialize, Default)]
pub struct IdQuery {
    #[serde(default)]
    id: String,
}

#[derive(Deserialize, Default)]
pub struct TokenQuery {
    #[serde(default)]
    t: String,
}

fn id_ok(act: &Act) -> bool {
    match act {
        Act::Enable(id) | Act::Delete(id) => manual_id(id),
        Act::Pick(id) => auto_id(id),
        _ => true,
    }
}

fn bad_id(act: &Act, l: Lang, remote: Via) -> String {
    let why = if matches!(act, Act::Pick(_)) {
        l.pick("not a carrier profile", "不是运营商的配置")
    } else {
        l.pick("not a manual profile", "不是手动配置")
    };
    area(act, l, remote, &write::Phase::Done(write::Outcome::Failed(why.into())))
}

/// The ask of mode / v6 / enable / delete.
async fn ask(app: Shared, act: Act, req: axum::extract::Request) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    if !id_ok(&act) {
        return one(bad_id(&act, ctx.lang, ctx.remote));
    }
    one(ask_html(&act, None, ctx.lang, ctx.remote, &app))
}

async fn set(app: Shared, act: Act, req: axum::extract::Request) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    let who = Caller::of(&app, &req);
    let ok_id = id_ok(&act);
    Sse::new(async_stream(move |tx| async move {
        if !ok_id {
            let _ = tx.send(patch(&bad_id(&act, l, Via::Local))).await;
            return;
        }
        run(app, act, None, who, l, tx).await;
    }))
}

pub async fn mode_ask(State(app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    let on = app.hub.read(|st| apn(st).map(|a| !a.manual_mode())).unwrap_or(true);
    ask(app, Act::Mode(on), req).await
}
pub async fn mode_set(State(app): State<Shared>, Query(q): Query<OnQuery>, req: axum::extract::Request) -> impl IntoResponse {
    set(app, Act::Mode(q.on), req).await
}
pub async fn v6_ask(State(app): State<Shared>, Query(q): Query<OnQuery>, req: axum::extract::Request) -> impl IntoResponse {
    ask(app, Act::V6(q.on), req).await
}
pub async fn v6_set(State(app): State<Shared>, Query(q): Query<OnQuery>, req: axum::extract::Request) -> impl IntoResponse {
    set(app, Act::V6(q.on), req).await
}
pub async fn enable_ask(State(app): State<Shared>, Query(q): Query<IdQuery>, req: axum::extract::Request) -> impl IntoResponse {
    ask(app, Act::Enable(q.id), req).await
}
pub async fn enable_set(State(app): State<Shared>, Query(q): Query<IdQuery>, req: axum::extract::Request) -> impl IntoResponse {
    set(app, Act::Enable(q.id), req).await
}
pub async fn delete_ask(State(app): State<Shared>, Query(q): Query<IdQuery>, req: axum::extract::Request) -> impl IntoResponse {
    ask(app, Act::Delete(q.id), req).await
}
pub async fn delete_set(State(app): State<Shared>, Query(q): Query<IdQuery>, req: axum::extract::Request) -> impl IntoResponse {
    set(app, Act::Delete(q.id), req).await
}
pub async fn pick_ask(State(app): State<Shared>, Query(q): Query<IdQuery>, req: axum::extract::Request) -> impl IntoResponse {
    ask(app, Act::Pick(q.id), req).await
}
pub async fn pick_set(State(app): State<Shared>, Query(q): Query<IdQuery>, req: axum::extract::Request) -> impl IntoResponse {
    set(app, Act::Pick(q.id), req).await
}

/// "Add" (no id) / "Edit": the form, as a patch.
pub async fn form_open(State(app): State<Shared>, Query(q): Query<IdQuery>, req: axum::extract::Request) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    let a = app.hub.read(apn);
    let p = a.as_ref().and_then(|a| a.find(&q.id)).cloned();
    let clear = area(&Act::Save(String::new()), l, Via::Local, &write::Phase::Idle);
    one(format!("{}\n{clear}", form(p.as_ref(), l)))
}
pub async fn form_close() -> impl IntoResponse {
    one("<div id=\"ap-form\"></div>".into())
}

/// "Save": check the form (its signals are the body), keep it under a token, ask.
pub async fn save_ask(State(app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let body = axum::body::to_bytes(req.into_body(), 64 * 1024).await.unwrap_or_default();
    let f: FormIn = serde_json::from_slice(&body).unwrap_or_default();
    let (a, stuck) = app.hub.read(|st| (apn(st), st.stuck));
    let html = match check_save(&f, a.as_ref(), stuck, l) {
        Err(o) => area(&Act::Save(String::new()), l, ctx.remote, &write::Phase::Done(o)),
        Ok(s) => match confirm(&Act::Save(String::new()), a.as_ref(), Some(&s), l) {
            Err(o) => area(&Act::Save(String::new()), l, ctx.remote, &write::Phase::Done(o)),
            Ok(c) => {
                let t = put(s);
                area(&Act::Save(t), l, ctx.remote, &write::Phase::Ask(c))
            }
        },
    };
    one(html)
}

pub async fn save_set(State(app): State<Shared>, Query(q): Query<TokenQuery>, req: axum::extract::Request) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    let who = Caller::of(&app, &req);
    let save = take(&q.t);
    let act = Act::Save(q.t.clone());
    Sse::new(async_stream(move |tx| async move {
        let Some(s) = save else {
            let o = write::Outcome::NotYet(l.pick("The form expired; press Save again", "表单已过期，请再按一次「保存」").into());
            let _ = tx.send(patch(&area(&act, l, Via::Local, &write::Phase::Done(o)))).await;
            return;
        };
        run(app, act, Some(s), who, l, tx).await;
    }))
}

fn cancel(act: Act, q: &write::CancelQuery, req: &axum::extract::Request) -> String {
    let ctx = crate::topbar::Ctx::of_request(req, PATH);
    area(&act, ctx.lang, ctx.remote, &write::cancel_phase(q))
}
pub async fn mode_cancel(Query(q): Query<write::CancelQuery>, req: axum::extract::Request) -> impl IntoResponse {
    one(cancel(Act::Mode(false), &q, &req))
}
pub async fn v6_cancel(Query(q): Query<write::CancelQuery>, req: axum::extract::Request) -> impl IntoResponse {
    one(cancel(Act::V6(false), &q, &req))
}
pub async fn row_cancel(Query(q): Query<write::CancelQuery>, req: axum::extract::Request) -> impl IntoResponse {
    one(cancel(Act::Enable(String::new()), &q, &req))
}
pub async fn save_cancel(Query(q): Query<write::CancelQuery>, req: axum::extract::Request) -> impl IntoResponse {
    one(cancel(Act::Save(String::new()), &q, &req))
}
pub async fn pick_cancel(Query(q): Query<write::CancelQuery>, req: axum::extract::Request) -> impl IntoResponse {
    one(cancel(Act::Pick(String::new()), &q, &req))
}

#[cfg(test)]
mod tests;
