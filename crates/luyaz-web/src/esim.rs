//! /router/esim (eSIM): a removable eUICC card (eSTK.me / 5ber)
//! managed through lpac on the device. Status first (the profile in use, or what the card is busy
//! with), then the profiles and their actions, adding a profile, pending notifications, the chip.
//!
//! Where each value comes from: the agent's `/api/esim/*` with the tab's own session (agent.rs asks
//! u60d first; it answers "not mine" until eSIM is handed over), polled like the old page: the
//! chip (`/api/esim/status`) on load, Retry and after a write; profiles and notifications only
//! when something may have changed: the tab's stream opened (it closes while the tab is hidden),
//! Refresh/Retry or a write ended, the `sim` block's epoch/phase moved, and every 10 s while a job
//! runs or the core serves its last list, and for `SETTLE_WINDOW` after. Every one of those runs
//! lpac against the card. While a job runs the agent answers
//! profiles with `busy:true, profiles:null`: the last list stays, and `/api/esim/job` (no card
//! access) says what the card is doing.
//!
//! Writes (web/docs/controls-inventory.md §/router/esim), each POSTed once, never retried:
//! - switch, tier 3 (dialog): `/api/esim/switch` → job. Read back: the profile reads `enabled`
//!   (the job saying "done" is not proof). A job that ends `rebooting` (the switch did not
//!   converge; the device reboots 2 s later, luyaz-web with it) closes the reply at once with
//!   "rebooting" and puts a read-only verifier on the page (`/after?k=`, retried by Datastar until
//!   the device is back) that reads the list after the restart. Without a session after the
//!   restart the page gets the "session expired" dialog: never "success".
//! - delete, tier 3 (dialog, type 「删除」/"delete"): `/api/esim/delete` → job. Read back: gone.
//! - download, tier 2: `/api/esim/download` → job (up to 10 min). Read back: one more profile.
//! - nickname, tier 2: `/api/esim/nickname` (synchronous). Read back: `profileNickname`.
//! - send notifications, tier 2: `/api/esim/notifications/process` → job. Read back: list empty.
//!
//! Privacy: rows are addressed by the card's ISD-P AID (`isdpAid`, a slot id, the same on every
//! card), never the ICCID. An ask keeps what it confirms (ICCID, nickname, activation code) under a
//! random token server-side; the confirm posts the token. The activation code and nickname travel
//! only in the ask's POST body (page-local `_es…` signals, sent with `filterSignals`); no URL, HTML
//! or push carries them, and nothing here logs them.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
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
use crate::pages::{Shared, StreamQuery, async_stream, patch, split_leaves};
use crate::write;

const PATH: &str = "/router/esim";
const PROFILES_EVERY: Duration = Duration::from_secs(10);
/// After the card stops reading busy/settling: keep re-reading this long
/// (`luyaz_esim::LIST_STALE_WINDOW`, 30 s, plus a poll).
const SETTLE_WINDOW: Duration = Duration::from_secs(40);
/// A chip read that failed is tried again this often (the old page's SWR retried too).
const CHIP_RETRY: Duration = Duration::from_secs(30);
const JOB_EVERY: Duration = Duration::from_millis(1500);
const NOTIF_JOB_EVERY: Duration = Duration::from_secs(2);
const JOB_LIMIT: Duration = Duration::from_secs(120);
/// A switch is checked card → identity → data in the engine. Its job carries `deadline_s`
/// (`luyaz_esim::SWITCH_DEADLINE`: busy retries, enable, resync, identity, data, notifications)
/// and the wait follows it; this is the floor, for a core that predates the field.
const SWITCH_LIMIT: Duration = Duration::from_secs(240);
/// Past the job's own `deadline_s`: the polls' slack.
const DEADLINE_SLACK: Duration = Duration::from_secs(15);
/// Downloads go over the mobile link to the SM-DP+ and can take minutes.
const DOWNLOAD_LIMIT: Duration = Duration::from_secs(600);
/// A switch re-attaches the modem (~40 s); the list may read busy meanwhile.
const SWITCH_READBACK: Duration = Duration::from_secs(45);
/// Readback reads of the card: no faster than the old page's profile poll.
const READBACK_EVERY: Duration = Duration::from_secs(10);
/// After a reboot announced by the job: how long the verifier waits for the device (old page 300 s).
const REBOOT_WAIT: Duration = Duration::from_secs(300);
/// The job said "rebooting" and this process is still alive after this long: no reboot happened.
const NO_REBOOT_AFTER: Duration = Duration::from_secs(30);
const STASH_TTL: Duration = Duration::from_secs(600);

/// This page's routes (the page, its stream, its buttons).
pub fn routes() -> Router<Shared> {
    Router::new()
        .route(PATH, get(page))
        .route("/router/esim/events", get(events))
        .route("/router/esim/refresh", post(refresh))
        .route("/router/esim/switch/ask", post(switch_ask))
        .route("/router/esim/delete/ask", post(delete_ask))
        .route("/router/esim/nick/ask", post(nick_ask))
        .route("/router/esim/download/ask", post(download_ask))
        .route("/router/esim/notif/ask", post(notif_ask))
        .route("/router/esim/set", post(set))
        .route("/router/esim/cancel", post(cancel))
        .route("/router/esim/stop", post(stop))
        .route("/router/esim/after", post(after))
}

// ---- what the agent says ----

fn text(v: &Value) -> Option<String> {
    v.as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(Into::into)
}

/// `/api/esim/status`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Chip {
    pub installed: bool,
    pub eid: Option<String>,
    pub free: Option<u64>,
    pub sgp22: Option<String>,
    pub fw: Option<String>,
    pub root_smds: Option<String>,
    pub default_smdp: Option<String>,
}

pub fn chip(v: &Value) -> Chip {
    Chip {
        installed: v["installed"].as_bool() == Some(true),
        eid: text(&v["eid"]),
        free: v["free_nvm"].as_u64(),
        sgp22: text(&v["sgp22_version"]),
        fw: text(&v["firmware_version"]),
        root_smds: text(&v["root_smds"]),
        default_smdp: text(&v["default_smdp"]),
    }
}

/// One profile of `/api/esim/profiles`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Profile {
    /// What the page's buttons address: the ISD-P AID when it is plain hex, else `n<index>`.
    pub key: String,
    pub iccid: String,
    pub enabled: bool,
    pub nick: Option<String>,
    pub spn: Option<String>,
    pub pname: Option<String>,
}

impl Profile {
    /// Nickname → carrier → profile name → ICCID (the old `nameOf`).
    pub fn name(&self) -> String {
        self.nick
            .clone()
            .or_else(|| self.spn.clone())
            .or_else(|| self.pname.clone())
            .unwrap_or_else(|| self.iccid.clone())
    }
    /// "carrier · profile name".
    pub fn sub(&self) -> String {
        [self.spn.as_deref(), self.pname.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

fn key_of(aid: Option<&str>, i: usize) -> String {
    match aid {
        Some(a) if !a.is_empty() && a.len() <= 64 && a.bytes().all(|c| c.is_ascii_hexdigit()) => {
            a.to_owned()
        }
        _ => format!("n{i}"),
    }
}

/// A key as the buttons send it: hex, or `n` + digits.
pub fn valid_key(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 64
        && (k.bytes().all(|c| c.is_ascii_hexdigit())
            || (k.starts_with('n') && k.len() > 1 && k[1..].bytes().all(|c| c.is_ascii_digit())))
}

/// The card's answer: the list, busy with a job, or the core's last list while the card settles
/// after a switch or another call holds it (`stale`/`updating`, its age in seconds). A settling
/// list is shown but proves no write.
#[derive(Clone, Debug, PartialEq)]
pub enum Listed {
    Busy,
    List(Vec<Profile>),
    Settling(Vec<Profile>, u64),
}

pub fn profiles(v: &Value) -> Listed {
    if v["busy"].as_bool() == Some(true) || !v["profiles"].is_array() {
        return Listed::Busy;
    }
    let ps = v["profiles"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, p)| Profile {
            key: key_of(p["isdpAid"].as_str(), i),
            iccid: p["iccid"].as_str().unwrap_or("").to_owned(),
            enabled: p["profileState"].as_str() == Some("enabled"),
            nick: text(&p["profileNickname"]),
            spn: text(&p["serviceProviderName"]),
            pname: text(&p["profileName"]),
        })
        .collect();
    if v["stale"].as_bool() == Some(true) {
        return Listed::Settling(ps, v["age_s"].as_u64().unwrap_or(0));
    }
    Listed::List(ps)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Notif {
    pub seq: i64,
    pub op: String,
    pub addr: String,
    pub iccid: String,
}

/// `/api/esim/notifications`: None = busy (`null` while a job runs).
pub fn notifs(v: &Value) -> Option<Vec<Notif>> {
    v.as_array().map(|a| {
        a.iter()
            .map(|n| Notif {
                seq: n["seqNumber"].as_i64().unwrap_or(0),
                op: n["profileManagementOperation"]
                    .as_str()
                    .unwrap_or("")
                    .to_owned(),
                addr: n["notificationAddress"].as_str().unwrap_or("").to_owned(),
                iccid: n["iccid"].as_str().unwrap_or("").to_owned(),
            })
            .collect()
    })
}

async fn read_list(app: &Shared, who: &Caller) -> Result<Listed, Fail> {
    crate::agent::get(app, who, "/api/esim/profiles")
        .await
        .map(|v| profiles(&v))
}

// ---- one tab's picture of the card ----

#[derive(Clone, Default)]
pub struct Card {
    pub chip: Option<Chip>,
    pub chip_fail: Option<Fail>,
    /// The last real list (kept while the card is busy).
    pub list: Option<Vec<Profile>>,
    pub list_fail: Option<Fail>,
    pub list_at: Option<SystemTime>,
    pub busy: bool,
    /// The list is the core's last one while the card settles: its age in seconds.
    pub settling: Option<u64>,
    /// The running job's kind while busy ("switch", "download", …).
    pub job: Option<String>,
    /// The core's shared word (`say`, `say_en`; core 1.32.0 on) for the running switch job
    pub job_say: Option<(String, String)>,
    /// The same for a SIM identity transition, from the `sim` block (any card change)
    pub sim_say: Option<(String, String)>,
    pub notifs: Option<Vec<Notif>>,
    /// The core's re-send of the receipts (`resend`, core 1.38.0 on; None = older core)
    pub resend: Option<Resend>,
}

/// Where the core's bounded re-send of the receipts is.
#[derive(Clone, Debug, PartialEq)]
pub enum Resend {
    /// the next round in this many seconds
    In(u64),
    /// the rounds ran out: send by hand
    Stopped,
}

/// `resend` of `/api/esim/notifications`.
pub fn resend_of(v: &Value) -> Option<Resend> {
    let r = v.get("resend")?;
    if r["gave_up"].as_bool() == Some(true) {
        return Some(Resend::Stopped);
    }
    r["next_s"].as_u64().map(Resend::In)
}

/// `say` / `say_en` of a job or the `sim` block; None when empty or absent (older core).
pub fn say_of(v: &Value) -> Option<(String, String)> {
    let zh = v["say"].as_str().filter(|s| !s.is_empty())?;
    Some((zh.to_owned(), v["say_en"].as_str().unwrap_or(zh).to_owned()))
}

fn said(s: &Option<(String, String)>, l: Lang) -> Option<String> {
    s.as_ref().map(|(zh, en)| match l {
        Lang::En => en.clone(),
        Lang::Zh => zh.clone(),
    })
}

impl Card {
    fn installed(&self) -> bool {
        self.chip.as_ref().is_some_and(|c| c.installed)
    }
    /// Shown, but the latest read failed.
    fn stale(&self) -> bool {
        self.list.is_some() && self.list_fail.is_some()
    }
    pub fn take_list(&mut self, r: Result<Listed, Fail>) {
        match r {
            Ok(Listed::List(ps)) => {
                self.list = Some(ps);
                self.list_fail = None;
                self.list_at = Some(SystemTime::now());
                self.busy = false;
                self.job = None;
                self.settling = None;
            }
            Ok(Listed::Settling(ps, age)) => {
                self.list = Some(ps);
                self.list_fail = None;
                self.busy = false;
                self.job = None;
                self.settling = Some(age);
            }
            Ok(Listed::Busy) => {
                self.busy = true;
                self.list_fail = None;
            }
            Err(f) => self.list_fail = Some(f),
        }
    }
}

/// The device was told to reboot by a switch job (this process): the status says so for a while.
fn reboot_mark() -> &'static Mutex<Option<Instant>> {
    static M: Mutex<Option<Instant>> = Mutex::new(None);
    &M
}

fn rebooting() -> bool {
    reboot_mark()
        .lock()
        .unwrap()
        .is_some_and(|t| t.elapsed() < REBOOT_WAIT)
}

/// Bumped by Retry and by every write that ended: the tabs read everything again.
static GEN: AtomicU64 = AtomicU64::new(0);

fn bump(app: &Shared) {
    GEN.fetch_add(1, Ordering::Relaxed);
    let _ = app.kick.send(PATH);
}

// ---- the view ----

pub struct Row {
    pub i: usize,
    pub key: String,
    pub name: String,
    pub nick: String,
    pub sub: String,
    pub iccid: String,
    pub on: bool,
}

pub struct ChipRow {
    pub label: &'static str,
    pub sub: Option<&'static str>,
    pub value: String,
}

pub struct View {
    pub l: Lang,
    pub tone: &'static str,
    pub state: String,
    pub reason: Option<String>,
    pub meta: Option<String>,
    /// The meta is device text (an ICCID).
    pub meta_data: bool,
    /// The state / reason carry device text (a profile's name, carrier).
    pub state_data: bool,
    pub reason_data: bool,
    /// The chip answered: lpac is not deployed.
    pub no_lpac: bool,
    pub retry: bool,
    /// None: not read yet (skeleton); Some(Err): can't read.
    pub rows: Option<Result<Vec<Row>, ()>>,
    pub stale: bool,
    pub locked: bool,
    pub notifs: Vec<Notif>,
    pub notif_dis: bool,
    /// one line on the automatic re-send, when the core runs one
    pub resend: Option<String>,
    pub chip: Vec<ChipRow>,
}

impl View {
    pub fn symbol(&self) -> &'static str {
        match self.tone {
            "bad" => "■",
            "warn" => "▲",
            _ => "●",
        }
    }
}

/// Bytes like the old `fmtBytes` (1024-based).
pub fn fmt_bytes(n: u64) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / 1024.0 / 1024.0)
    } else if n >= 1024 {
        format!("{} KB", (n as f64 / 1024.0).round())
    } else {
        format!("{n} B")
    }
}

fn job_word(kind: Option<&str>, l: Lang) -> &'static str {
    match kind {
        Some("switch") => l.pick("Switching…", "切换中…"),
        Some("delete") => l.pick("Deleting…", "删除中…"),
        Some("download") => l.pick("Fetching the profile…", "配置下载中…"),
        _ => l.pick("Working…", "处理中…"),
    }
}

pub fn view(c: &Card, reboot: bool, l: Lang) -> View {
    let installed = c.installed();
    let active = c.list.as_ref().and_then(|ps| ps.iter().find(|p| p.enabled));
    let stale = c.stale();
    let mut tone = "neutral";
    let mut state: String = l
        .pick("Reading the eUICC card…", "正在读取 eUICC 卡…")
        .into();
    let mut reason = None;
    let mut meta = None;
    let mut meta_data = false;
    let mut state_data = false;
    let mut reason_data = false;
    let mut retry = false;
    match (&c.chip, &c.chip_fail) {
        (None, Some(f)) => {
            tone = "bad";
            state = l
                .pick("Can't read the eUICC card", "读不到 eUICC 卡")
                .into();
            reason_data = true;
            reason = Some(match l {
                Lang::En => format!("Couldn't read the eUICC: {}", f.text(l)),
                Lang::Zh => format!("读取 eUICC 失败：{}", f.text(l)),
            });
            retry = true;
        }
        (None, None) => {}
        (Some(ch), _) if !ch.installed => {
            state = l
                .pick("lpac is missing on the device", "设备上没有 lpac")
                .into();
            reason = Some(l.pick(
                "eSIM needs the lpac bundle in /data/esim. Build it with scripts/esim/build-esim-bundle.sh in the repo, put it there and refresh.",
                "eSIM 要用 /data/esim 里的 lpac 套件。在仓库用 scripts/esim/build-esim-bundle.sh 打包，放上去后刷新。",
            ).into());
        }
        (Some(_), _) if reboot => {
            state = l.pick("Rebooting…", "重启中…").into();
            reason = Some(reboot_banner(l).into());
        }
        (Some(_), _) if c.busy => {
            state = said(&c.job_say, l).unwrap_or_else(|| job_word(c.job.as_deref(), l).into());
            reason = Some(
                l.pick(
                    "The card handles one operation at a time; the other actions wait until this one ends.",
                    "卡片一次只能做一件事，其他操作要等这件做完。",
                )
                .into(),
            );
        }
        (Some(_), _) => match (&c.list, &c.list_fail) {
            (None, Some(f)) => {
                tone = "bad";
                state = l.pick("Can't read the profiles", "读不到配置列表").into();
                reason_data = true;
                reason = Some(match l {
                    Lang::En => format!("Couldn't load profiles: {}", f.text(l)),
                    Lang::Zh => format!("加载配置失败：{}", f.text(l)),
                });
                retry = true;
            }
            (None, None) => {}
            (Some(ps), _) => match active {
                Some(a) => {
                    tone = "ok";
                    state_data = true;
                    reason_data = true;
                    state = match l {
                        Lang::En => format!("In use: {}", a.name()),
                        Lang::Zh => format!("使用中：{}", a.name()),
                    };
                    let s = a.sub();
                    reason = (!s.is_empty()).then_some(s);
                    meta = Some(a.iccid.clone());
                    meta_data = true;
                }
                None => {
                    tone = "warn";
                    state = l.pick("No profile is enabled", "没有启用的配置").into();
                    reason = Some(
                        if ps.is_empty() {
                            l.pick(
                                "The card has no profiles; add one below",
                                "卡里还没有配置，在下面添加",
                            )
                        } else {
                            l.pick(
                                "Switch to one of the profiles below to get mobile service.",
                                "切换到下面的一个配置才能用移动网络。",
                            )
                        }
                        .into(),
                    );
                }
            },
        },
    }
    if let Some(age) = c.settling
        && installed
        && !stale
        && !c.busy
        && !reboot
    {
        tone = "stale";
        meta = Some(match l {
            Lang::En => {
                format!("The card is still settling — this list is from {age} s ago, updating…")
            }
            Lang::Zh => format!("卡片还在稳定——这是 {age} 秒前的列表，正在更新…"),
        });
        meta_data = false;
    }
    // a SIM identity transition (also the firmware's own slot switch): the core's shared word
    if let Some(w) = said(&c.sim_say, l)
        && installed
        && !c.busy
        && !reboot
    {
        meta = Some(w);
        meta_data = false;
    }
    if installed && stale && !c.busy && !reboot {
        tone = "stale";
        meta = Some(format!(
            "{}{}",
            crate::agent::stopped(l, "List", "列表", c.list_at),
            l.pick(" — refresh before changing anything.", "，刷新后再改。")
        ));
        meta_data = false;
        retry = true;
    }
    let rows = match (&c.list, &c.list_fail) {
        (Some(ps), _) => Some(Ok(ps
            .iter()
            .enumerate()
            .map(|(i, p)| Row {
                i,
                key: p.key.clone(),
                name: p.name(),
                nick: p.nick.clone().unwrap_or_default(),
                sub: p.sub(),
                iccid: p.iccid.clone(),
                on: p.enabled,
            })
            .collect())),
        (None, Some(_)) => Some(Err(())),
        (None, None) => None,
    };
    let busy = c.busy || reboot;
    let notifs = c.notifs.clone().unwrap_or_default();
    let dash = || "—".to_string();
    let chip = match &c.chip {
        Some(ch) if ch.installed => vec![
            ChipRow {
                label: "EID",
                sub: None,
                value: ch.eid.clone().unwrap_or_else(dash),
            },
            ChipRow {
                label: l.pick("Free space", "剩余空间"),
                sub: Some(l.pick("room for new profiles", "还能装新配置的空间")),
                value: ch.free.map_or_else(dash, fmt_bytes),
            },
            ChipRow {
                label: "SGP.22",
                sub: Some(l.pick("Spec version", "规范版本")),
                value: ch.sgp22.clone().unwrap_or_else(dash),
            },
            ChipRow {
                label: l.pick("Chip firmware", "芯片固件"),
                sub: None,
                value: ch.fw.clone().unwrap_or_else(dash),
            },
            ChipRow {
                label: l.pick("Root SM-DS", "根 SM-DS"),
                sub: None,
                value: ch.root_smds.clone().unwrap_or_else(dash),
            },
            ChipRow {
                label: l.pick("Default SM-DP+", "默认 SM-DP+"),
                sub: None,
                value: ch.default_smdp.clone().unwrap_or_else(dash),
            },
        ],
        _ => Vec::new(),
    };
    View {
        l,
        tone,
        state,
        reason,
        meta,
        meta_data,
        state_data,
        reason_data,
        no_lpac: c.chip.as_ref().is_some_and(|ch| !ch.installed),
        retry,
        rows,
        stale,
        locked: c.list.is_none() || stale || busy || c.settling.is_some(),
        notif_dis: busy || notifs.is_empty(),
        resend: c.resend.as_ref().map(|r| match r {
            Resend::In(s) => {
                let m = s.div_ceil(60).max(1);
                match l {
                    Lang::En => format!("Sent again automatically in about {m} min"),
                    Lang::Zh => format!("约 {m} 分钟后自动重发"),
                }
            }
            Resend::Stopped => l
                .pick(
                    "Automatic re-sending has stopped: send them by hand",
                    "自动重发已停止，请手动发送",
                )
                .to_owned(),
        }),
        notifs,
        chip,
    }
}

fn reboot_banner(l: Lang) -> &'static str {
    l.pick(
        "The device is rebooting onto the new profile (about 90 s, up to 5 min).",
        "设备正在重启并切换到新配置（约 90 秒，最长 5 分钟）。",
    )
}

#[derive(Template)]
#[template(path = "esim_leaves.html")]
struct Leaves<'a> {
    v: &'a View,
}

pub fn render(c: &Card, l: Lang) -> Vec<(String, String)> {
    split_leaves(
        &Leaves {
            v: &view(c, rebooting(), l),
        }
        .render()
        .unwrap(),
    )
}

// ---- the stream ----

pub struct Poll {
    l: Lang,
    who: Caller,
    card: Card,
    seen_gen: u64,
    first: bool,
    next_chip: Option<Instant>,
    /// The next profile read; None = only on a trigger (see the module doc).
    next_prof: Option<Instant>,
    /// Re-read until then (the card was busy or settling).
    settle_until: Option<Instant>,
    /// The `sim` block's (epoch, phase) the last read saw.
    seen_sim: Option<(u64, String)>,
}

/// The `sim` block's identity epoch and phase (u60d stamps them every round).
fn sim_mark(app: &Shared) -> Option<(u64, String)> {
    app.hub.read(|st| {
        let d = &st.blocks.get("sim")?.data;
        Some((d["epoch"].as_u64()?, d["phase"].as_str().unwrap_or("").to_owned()))
    })
}

impl Poll {
    async fn chip(&mut self, app: &Shared) {
        match crate::agent::get(app, &self.who, "/api/esim/status").await {
            Ok(v) => {
                self.card.chip = Some(chip(&v));
                self.card.chip_fail = None;
                self.next_chip = None;
            }
            Err(f) => {
                self.card.chip_fail = Some(f);
                self.next_chip = Some(Instant::now() + CHIP_RETRY);
            }
        }
    }
    async fn list(&mut self, app: &Shared) {
        self.card.take_list(read_list(app, &self.who).await);
        if self.card.busy {
            let j = crate::agent::get(app, &self.who, "/api/esim/job")
                .await
                .ok()
                .filter(|j| j["status"] == "running");
            self.card.job = j.as_ref().and_then(|j| text(&j["kind"]));
            self.card.job_say = j.as_ref().and_then(say_of);
        }
        let now = Instant::now();
        if self.card.busy || self.card.settling.is_some() {
            self.settle_until = Some(now + SETTLE_WINDOW);
        }
        self.next_prof = self
            .settle_until
            .filter(|t| now < *t)
            .map(|_| now + PROFILES_EVERY);
    }
    async fn notifs(&mut self, app: &Shared) {
        // null while a job runs: keep the last list
        if let Ok(v) = crate::agent::get_whole(app, &self.who, "/api/esim/notifications").await
            && let Some(n) = notifs(&v["data"])
        {
            self.card.notifs = Some(n);
            self.card.resend = resend_of(&v);
        }
    }
}

impl Polled for Poll {
    fn watches(&self, block: &str) -> bool {
        block == "sim"
    }

    /// The card may have changed under us: every tab on this page reads it again.
    fn render(&mut self, app: &Shared) -> Option<Vec<(String, String)>> {
        if sim_mark(app) != self.seen_sim {
            let _ = app.kick.send(PATH);
        }
        None
    }

    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        let g = GEN.load(Ordering::Relaxed);
        let sim = sim_mark(app);
        let moved = sim != self.seen_sim;
        let all = self.first || g != self.seen_gen;
        self.first = false;
        self.seen_gen = g;
        self.seen_sim = sim;
        self.card.sim_say = app
            .hub
            .read(|st| st.blocks.get("sim").and_then(|b| say_of(&b.data)));
        let now = Instant::now();
        if all || self.next_chip.is_some_and(|t| now >= t) {
            self.chip(app).await;
        }
        if self.card.installed() && (all || moved || self.next_prof.is_some_and(|t| now >= t)) {
            self.list(app).await;
            // null while busy, so only worth asking when the list is real
            if !self.card.busy {
                self.notifs(app).await;
            }
        }
        let next = match (self.next_chip, self.next_prof.filter(|_| self.card.installed())) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let wait = next.map_or(Duration::from_secs(3600), |t| {
            t.saturating_duration_since(Instant::now())
                .max(Duration::from_millis(200))
        });
        (render(&self.card, self.l), wait)
    }
}

// ---- writes ----

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    Switch,
    Delete,
    Nick,
    Download,
    Notif,
}

impl Act {
    pub fn area(self) -> &'static str {
        match self {
            Act::Switch | Act::Delete | Act::Nick => "es-prof",
            Act::Download => "es-dl",
            Act::Notif => "es-nt",
        }
    }
    fn job_path(self) -> &'static str {
        match self {
            Act::Switch => "/api/esim/switch",
            Act::Delete => "/api/esim/delete",
            Act::Nick => "/api/esim/nickname",
            Act::Download => "/api/esim/download",
            Act::Notif => "/api/esim/notifications/process",
        }
    }
}

/// What an ask confirmed, kept for its send.
#[derive(Clone, Debug, Default)]
pub struct Req {
    pub iccid: String,
    pub key: String,
    pub name: String,
    pub nick: String,
    pub code: String,
    pub conf: String,
    /// Profiles on the card when asked (download's readback).
    pub before: usize,
    /// Delete: the word to type.
    pub word: Option<String>,
}

type Stash = Mutex<HashMap<String, (Instant, Act, Req)>>;

fn stash() -> &'static Stash {
    static S: std::sync::OnceLock<Stash> = std::sync::OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

fn put(act: Act, r: Req) -> String {
    let t = crate::auth::random_id();
    let mut m = stash().lock().unwrap();
    m.retain(|_, (at, _, _)| at.elapsed() < STASH_TTL);
    m.insert(t.clone(), (Instant::now(), act, r));
    t
}

fn take(t: &str) -> Option<(Act, Req)> {
    stash()
        .lock()
        .unwrap()
        .remove(t)
        .filter(|(at, _, _)| at.elapsed() < STASH_TTL)
        .map(|(_, a, r)| (a, r))
}

pub fn delete_word(l: Lang) -> &'static str {
    l.pick("delete", "删除")
}

fn job_clash(l: Lang) -> &'static str {
    l.pick(
        "Another eSIM operation is still running. Wait for it to finish, then try again.",
        "另一个 eSIM 操作还在进行，等它结束再试。",
    )
}

fn card_busy_note(l: Lang) -> &'static str {
    l.pick(
        "The card kept answering “busy” (catBusy) through the automatic retries and kept the old profile: restart the device (or take the card out and put it back), then switch again.",
        "卡一直回「忙」（catBusy），自动重试几分钟后还是原来的配置。重启设备（或把卡拔出来再插回去）后再切。",
    )
}

pub fn switch_recovery(from: Option<&str>, l: Lang) -> String {
    let n = from.unwrap_or("—");
    match l {
        Lang::En => format!(
            "Give the device a few minutes to finish restarting. If the new profile has no service, switch back to “{n}” here or on the touchscreen's eSIM page."
        ),
        Lang::Zh => {
            format!("等几分钟让设备重启完。新配置没有信号时，在这里或触屏的 eSIM 页切回「{n}」。")
        }
    }
}

/// The confirm of each write (the old page's dialogs and inline confirms, same words).
pub fn confirm(act: Act, r: &Req, from: Option<&str>, n_notifs: usize, l: Lang) -> write::Confirm {
    let name = &r.name;
    let inline = |title: &str, what: String, action: &str| write::Confirm {
        tier: write::Tier::Inline,
        title: title.into(),
        what,
        downtime: None,
        recovery: None,
        action: action.into(),
        cuts_uplink: false,
        word: None,
        danger: false,
        revert: crate::write::Revert::No,
    };
    match act {
        Act::Switch => write::Confirm {
            tier: write::Tier::Dialog,
            title: match l {
                Lang::En => format!("Switch to “{name}”?"),
                Lang::Zh => format!("切换到「{name}」？"),
            },
            what: match (l, from) {
                (Lang::En, Some(f)) => format!("The card enables “{name}” and the modem re-attaches on it (instead of “{f}”). Mobile data stops until it is registered."),
                (Lang::En, None) => format!("The card enables “{name}” and the modem re-attaches on it. Mobile data stops until it is registered."),
                (Lang::Zh, Some(f)) => format!("卡片启用「{name}」，基带用它重新注网（替换「{f}」）。注册上之前没有移动数据。"),
                (Lang::Zh, None) => format!("卡片启用「{name}」，基带用它重新注网。注册上之前没有移动数据。"),
            },
            downtime: Some(l.pick(
                "Usually about 40 s without a reboot. If the new profile doesn't take within about 30 s, the device reboots to finish (about 90 s more).",
                "通常约 40 秒，不用重启。新配置约 30 秒内没生效时，设备会重启来完成切换（再约 90 秒）。",
            ).into()),
            recovery: Some(switch_recovery(from, l)),
            action: match l {
                Lang::En => format!("Switch to “{name}”"),
                Lang::Zh => format!("确认：切换到「{name}」"),
            },
            cuts_uplink: true,
            word: None,
            danger: false,
            revert: crate::write::Revert::No,
        },
        Act::Delete => write::Confirm {
            tier: write::Tier::Dialog,
            title: match l {
                Lang::En => format!("Delete “{name}” from the card?"),
                Lang::Zh => format!("从卡上删除「{name}」？"),
            },
            what: l.pick(
                "The profile is erased from the eUICC. This can't be undone: getting it back needs a new activation code from the carrier.",
                "这个配置会从 eUICC 上抹掉，不能撤销；要找回只能向运营商重新要激活码。",
            ).into(),
            downtime: None,
            recovery: Some(l.pick(
                "None on the device. Ask the carrier for a new profile if you need it again.",
                "设备上没法恢复。以后还要用的话，向运营商重新申请配置。",
            ).into()),
            action: l.pick("Delete", "删除").into(),
            cuts_uplink: false,
            word: Some(delete_word(l).into()),
            danger: true,
            revert: crate::write::Revert::No,
        },
        Act::Nick => inline(
            l.pick("Rename", "改名"),
            match (l, r.nick.is_empty()) {
                (Lang::En, false) => format!("“{name}” is shown as “{}” here and on the touchscreen. The card itself doesn't change.", r.nick),
                (Lang::En, true) => format!("The nickname of “{name}” is cleared; the carrier's name is shown instead."),
                (Lang::Zh, false) => format!("「{name}」在这里和触屏上显示为「{}」。卡上的配置本身不变。", r.nick),
                (Lang::Zh, true) => format!("清除「{name}」的昵称，改为显示运营商名。"),
            },
            l.pick("Confirm: rename", "确认：改名"),
        ),
        Act::Download => inline(
            l.pick("Download the profile", "下载配置"),
            l.pick(
                "The device fetches the profile from the carrier's server over the mobile link and stores it on the card (uses card space; can take a few minutes). It is not enabled — switch to it when ready.",
                "设备经移动网络从运营商服务器下载配置，存到卡上（占用卡上空间，可能要几分钟）。不会自动启用，需要时再切换。",
            ).into(),
            l.pick("Confirm: download the profile", "确认：下载配置"),
        ),
        Act::Notif => inline(
            l.pick("Send the notifications", "发送通知"),
            match l {
                Lang::En => format!("The device sends {n_notifs} receipt(s) to the carriers' servers over the mobile link and removes them from the card once delivered."),
                Lang::Zh => format!("设备经移动网络把 {n_notifs} 条回执发给运营商服务器，送达后从卡上删掉。"),
            },
            l.pick("Confirm: send the notifications", "确认：发送通知"),
        ),
    }
}

/// How the job slot answered for our job.
#[derive(Clone, Debug, PartialEq)]
pub enum JobEnd {
    Done {
        rebooting: bool,
        /// A switch whose data call did not come back in time (the agent's `reason: "no_data"`):
        /// the card and the modem are on the new profile, no reboot was done.
        no_data: bool,
        /// The card took the profile but the modem stack never did, and the engine's one
        /// automatic reboot a day was already used (`reason: "identity_stuck"`).
        stuck: bool,
    },
    Error {
        message: String,
        reason: String,
    },
    /// The slot holds a newer job (e.g. the agent's own notifications job after a switch): ours
    /// ended unseen; the readback decides.
    Moved,
    TimedOut,
}

/// Job `ours`'s `deadline_s`, when it has one.
pub fn job_deadline(ours: u64, j: &Value) -> Option<Duration> {
    (j["id"].as_u64()? == ours)
        .then(|| j["deadline_s"].as_u64())
        .flatten()
        .filter(|s| *s > 0)
        .map(Duration::from_secs)
}

/// `/api/esim/job` for job `ours`: None = still running.
pub fn job_end(ours: u64, j: &Value) -> Option<JobEnd> {
    let id = j["id"].as_u64()?;
    if id > ours {
        return Some(JobEnd::Moved);
    }
    if id < ours {
        return None;
    }
    match j["status"].as_str() {
        Some("done") => Some(JobEnd::Done {
            rebooting: j["rebooting"].as_bool() == Some(true),
            no_data: j["reason"].as_str() == Some("no_data"),
            stuck: j["reason"].as_str() == Some("identity_stuck"),
        }),
        Some("error") => Some(JobEnd::Error {
            message: j["message"].as_str().unwrap_or("").to_owned(),
            reason: j["reason"].as_str().unwrap_or("").to_owned(),
        }),
        _ => None,
    }
}

/// The start POST's refusal (the agent's words; 409 = another job holds the card).
pub fn refusal(code: u16, v: &Value, l: Lang) -> write::Outcome {
    if code == 409 {
        return write::Outcome::Failed(job_clash(l).into());
    }
    write::Outcome::Failed(crate::agent::refused(code, v).text(l))
}

/// What to do about a job error with a known `reason`.
fn reason_note(reason: &str, l: Lang) -> Option<&'static str> {
    match reason {
        "card_busy" => Some(card_busy_note(l)),
        "cancelled" => Some(l.pick(
            "The retries were stopped; the card still has the old profile.",
            "已停止重试，卡上还是原来的配置。",
        )),
        _ => None,
    }
}

/// A job that ended in error: its message, and what to do about it.
pub fn job_failed(message: &str, reason: &str, l: Lang) -> write::Outcome {
    let m = if message.is_empty() {
        l.pick("the job failed", "任务失败").to_string()
    } else {
        message.to_owned()
    };
    let extra = if let Some(n) = reason_note(reason, l) {
        Some(n)
    } else if m.to_ascii_lowercase().contains("in progress") {
        Some(job_clash(l))
    } else {
        None
    };
    write::Outcome::Failed(match extra {
        Some(e) => format!("{m}. {e}"),
        None => m,
    })
}

pub fn applied(act: Act, r: &Req, l: Lang) -> String {
    match (act, l) {
        (Act::Switch, Lang::En) => format!("Switched to “{}”", r.name),
        (Act::Switch, Lang::Zh) => format!("已切换到「{}」", r.name),
        (Act::Delete, Lang::En) => format!("“{}” deleted from the card", r.name),
        (Act::Delete, Lang::Zh) => format!("已从卡上删除「{}」", r.name),
        (Act::Nick, Lang::En) => "Nickname saved".into(),
        (Act::Nick, Lang::Zh) => "昵称已保存".into(),
        (Act::Download, Lang::En) => "Profile downloaded to the card (not enabled)".into(),
        (Act::Download, Lang::Zh) => "配置已下载到卡上（没有启用）".into(),
        (Act::Notif, Lang::En) => "Notifications sent".into(),
        (Act::Notif, Lang::Zh) => "通知已发送".into(),
    }
}

/// The card took the profile but the modem never picked it up, and no automatic reboot was left.
pub fn identity_stuck(r: &Req, l: Lang) -> write::Outcome {
    write::Outcome::Ended(
        match l {
            Lang::En => format!(
                "The card is on “{}”, but the modem has not picked it up. Restart the device to finish (it already restarted once by itself in the last 24 h, so it won't again).",
                r.name
            ),
            Lang::Zh => format!(
                "卡已经换到「{}」，但调制解调器还没认出来。重启设备完成切换（24 小时内已自动重启过一次，不会再自己重启）。",
                r.name
            ),
        },
        "warn",
    )
}

/// The switch took (card and modem) but the data call did not come back: never plain success.
pub fn no_data(r: &Req, l: Lang) -> write::Outcome {
    write::Outcome::Ended(
        match l {
            Lang::En => format!(
                "Switched to “{}”, but mobile data has not come back yet. Wait a minute; if it stays down, check the APN or switch back.",
                r.name
            ),
            Lang::Zh => format!(
                "已切换到「{}」，但移动数据还没恢复。等一分钟；一直不通就检查 APN 或切回去。",
                r.name
            ),
        },
        "warn",
    )
}

/// The job (or the nickname call) said it worked, the card doesn't show it: never success.
pub fn not_read(act: Act, r: &Req, l: Lang) -> write::Outcome {
    write::Outcome::Ended(
        match (act, l) {
            (Act::Switch, Lang::En) => format!(
                "The device reported the switch done, but the card doesn't show “{}” in use yet — check the list above.",
                r.name
            ),
            (Act::Switch, Lang::Zh) => format!(
                "设备说切换完了，但卡上读回来还不是「{}」在用——看上面的列表。",
                r.name
            ),
            (_, Lang::En) => {
                "Accepted by the device, not read back yet — check the list above.".into()
            }
            (_, Lang::Zh) => "设备已接受，还没读回——看上面的列表。".into(),
        },
        "warn",
    )
}

fn timed_out(l: Lang) -> write::Outcome {
    write::Outcome::Ended(
        l.pick(
            "Still running after the wait; the list above shows the result when it ends.",
            "等了这么久任务还没结束；结束后上面的列表会显示结果。",
        )
        .into(),
        "warn",
    )
}

/// Does the card show the write?
pub fn shows(act: Act, r: &Req, list: &[Profile]) -> bool {
    match act {
        Act::Switch => list.iter().any(|p| p.iccid == r.iccid && p.enabled),
        Act::Delete => !list.iter().any(|p| p.iccid == r.iccid),
        Act::Nick => list
            .iter()
            .find(|p| p.iccid == r.iccid)
            .is_some_and(|p| p.nick.clone().unwrap_or_default() == r.nick),
        Act::Download => list.len() > r.before,
        Act::Notif => true,
    }
}

/// Read the card until it shows the write (busy = not yet).
async fn readback(app: &Shared, who: &Caller, act: Act, r: &Req) -> bool {
    let timeout = if act == Act::Switch {
        SWITCH_READBACK
    } else {
        write::READBACK
    };
    // at once, then at the old page's own pace (each read runs lpac against the card; u60d's
    // pushes say nothing about the card, so they don't wake this)
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let shown = if act == Act::Notif {
            crate::agent::get(app, who, "/api/esim/notifications")
                .await
                .ok()
                .and_then(|v| notifs(&v))
                .is_some_and(|n| n.is_empty())
        } else {
            matches!(read_list(app, who).await, Ok(Listed::List(ps)) if shows(act, r, &ps))
        };
        if shown {
            return true;
        }
        let next = tokio::time::Instant::now() + READBACK_EVERY;
        if next > deadline {
            return false;
        }
        tokio::time::sleep_until(next).await;
    }
}

/// Poll the job slot (no card access) until our job ends.
/// The core's shared words for job `ours` (`say` / `say_en`, cores from 1.32.0 on), in `l`.
pub fn job_say(ours: u64, j: &Value, l: Lang) -> Option<String> {
    if j["id"].as_u64()? != ours {
        return None;
    }
    let k = match l {
        Lang::En => "say_en",
        Lang::Zh => "say",
    };
    j[k].as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}

/// The core's word for the switch first, then the page's own sentence (older cores: as before).
pub fn with_say(o: write::Outcome, say: Option<&str>, l: Lang) -> write::Outcome {
    use write::Outcome as O;
    let Some(h) = say else { return o };
    let join = |t: String| match l {
        _ if t.is_empty() => h.to_owned(),
        Lang::En => format!("{h}. {t}"),
        Lang::Zh => format!("{h}。{t}"),
    };
    match o {
        O::Applied(t) => O::Applied(join(t)),
        O::Ended(t, tone) => O::Ended(join(t), tone),
        O::Failed(t) => O::Failed(join(t)),
        o => o,
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Poll the job slot until job `ours` ends; a switch waiting out a busy card shows its retries
/// (and Stop retrying) in `#es-retry`, cleared when the wait ends.
async fn wait_job(
    app: &Shared,
    who: &Caller,
    ours: u64,
    act: Act,
    say: &mut Option<String>,
    l: Lang,
    tx: &tokio::sync::mpsc::Sender<Event>,
) -> JobEnd {
    let mut seen = None;
    let mut shown = RETRY_EMPTY.to_string();
    let since = now_unix();
    let every = if act == Act::Notif {
        NOTIF_JOB_EVERY
    } else {
        JOB_EVERY
    };
    let limit = match act {
        Act::Download => DOWNLOAD_LIMIT,
        Act::Switch => SWITCH_LIMIT,
        _ => JOB_LIMIT,
    };
    let start = tokio::time::Instant::now();
    let mut deadline = start + limit;
    let mut tick = start;
    let end = loop {
        tick += every;
        if tick > deadline {
            break JobEnd::TimedOut;
        }
        tokio::time::sleep_until(tick).await;
        // a failed read is not an answer: keep polling until the deadline
        if let Ok(j) = crate::agent::get(app, who, "/api/esim/job").await {
            *say = job_say(ours, &j, l);
            if act == Act::Switch {
                let r = busy_retry(ours, &j, since, now_unix(), &mut seen);
                let html = retry_html(r.as_ref(), r.is_some() && stopping(), l);
                if html != shown {
                    let _ = tx.send(patch(&html)).await;
                    shown = html;
                }
            }
            if let Some(e) = job_end(ours, &j) {
                break e;
            }
            // the job's own bound (switch), never below ours
            if let Some(s) = job_deadline(ours, &j) {
                deadline = deadline.max(start + s + DEADLINE_SLACK);
            }
        }
    };
    if shown != RETRY_EMPTY {
        let _ = tx.send(patch(RETRY_EMPTY)).await;
    }
    end
}

// ---- a busy card's retries (core's catBusy retries, luyaz-esim P0-3) ----

/// `luyaz_esim::BUSY_TOTAL`: no retry starts past this from the switch's start; plus the last
/// try's enable and readback.
const BUSY_TOTAL_S: u64 = 240 + 15;
const STOP: &str = "/router/esim/stop";
const RETRY_EMPTY: &str = "<div id=\"es-retry\"></div>";

/// A switch waiting out a busy card: enable tries so far, when the next one starts, when the
/// waiting gives up (device-clock unix seconds).
#[derive(Clone, Debug, PartialEq)]
pub struct Retry {
    pub tries: u32,
    pub next_unix: u64,
    pub until_unix: u64,
}

/// Switch `ours` is between busy-card tries (`message` "card busy — retrying (try N in G s)",
/// `attempts`). The job says "in G s" once per try, so `seen` keeps that try's start time.
/// `since_unix`: when this page sent the switch. Every time here is on luyaz-web's own clock (the
/// device's, local wall time labelled UTC), never the job's `started_unix`, so the two shown
/// times agree even where the core's clock differs (the e2e mock).
pub fn busy_retry(
    ours: u64,
    j: &Value,
    since_unix: u64,
    now_unix: u64,
    seen: &mut Option<(u32, u64)>,
) -> Option<Retry> {
    if j["id"].as_u64()? != ours || j["status"].as_str() != Some("running") {
        return None;
    }
    let m = j["message"].as_str()?;
    if !m.starts_with("card busy") {
        return None;
    }
    let tries = u32::try_from(j["attempts"].as_u64()?).ok().filter(|t| *t > 0)?;
    let gap = m
        .rsplit_once(" in ")
        .and_then(|(_, t)| t.split(' ').next())
        .and_then(|g| g.parse::<u64>().ok())
        .unwrap_or(0);
    let next_unix = match *seen {
        Some((t, at)) if t == tries => at,
        _ => {
            *seen = Some((tries, now_unix + gap));
            now_unix + gap
        }
    };
    let until_unix = (since_unix + BUSY_TOTAL_S).max(next_unix);
    Some(Retry { tries, next_unix, until_unix })
}

/// "hh:mm:ss" of a device-clock unix second: local wall time labelled UTC, so the UTC digits.
fn hms(unix: u64) -> String {
    format!("{:02}:{:02}:{:02}", unix / 3600 % 24, unix / 60 % 60, unix % 60)
}

/// The Stop retrying button was pressed this recently: the next polls say "stopping".
fn stop_mark() -> &'static Mutex<Option<Instant>> {
    static M: Mutex<Option<Instant>> = Mutex::new(None);
    &M
}

fn stopping() -> bool {
    stop_mark()
        .lock()
        .unwrap()
        .is_some_and(|t| t.elapsed() < Duration::from_secs(10))
}

fn retry_note(t: &str, alert: bool) -> String {
    format!(
        "<div id=\"es-retry\"><p class=\"t-note{}\" role=\"{}\">{}</p></div>",
        if alert { " t-note--warn" } else { "" },
        if alert { "alert" } else { "status" },
        crate::agent::esc(t)
    )
}

/// The retry progress under the profiles, with Stop retrying (tier 1: it only ends the waiting;
/// the card keeps the profile it has). Empty when there is nothing to show.
pub fn retry_html(r: Option<&Retry>, stopping: bool, l: Lang) -> String {
    let Some(r) = r else {
        return RETRY_EMPTY.into();
    };
    if stopping {
        return retry_note(l.pick("Stopping the retries…", "正在停止重试…"), false);
    }
    let (next, until) = (hms(r.next_unix), hms(r.until_unix));
    let t = match l {
        Lang::En => format!(
            "The card is busy and hasn't taken the profile ({} {} so far). Next try at {next}; it gives up around {until} and the card keeps its old profile.",
            r.tries,
            if r.tries == 1 { "try" } else { "tries" }
        ),
        Lang::Zh => format!(
            "卡忙，还没换过去（已试 {} 次）。{next} 再试，约 {until} 放弃，放弃后卡上还是原来的配置。",
            r.tries
        ),
    };
    format!(
        "<div id=\"es-retry\" role=\"group\" class=\"nd-confirm\" aria-label=\"{}\"><p class=\"nd-confirm__text\" role=\"status\" aria-live=\"polite\">{}</p><div class=\"nd-confirm__actions\"><button type=\"button\" class=\"nd-btn nd-btn--secondary\" data-on:click=\"{}\">{}<span>{}</span></button></div></div>",
        l.pick("Retrying the switch", "切换重试中"),
        crate::agent::esc(&t),
        write::post_once(STOP),
        crate::shell::icon("Stop", false, 18, ""),
        l.pick("Stop retrying", "停止重试")
    )
}

/// What the Stop retrying press got from the core (`POST /api/esim/cancel`).
pub fn stop_reply(r: Result<(u16, &Value), &Fail>, l: Lang) -> String {
    match r {
        Ok((c, v)) if (200..300).contains(&c) && v["ok"] != false => {
            retry_html(Some(&Retry { tries: 0, next_unix: 0, until_unix: 0 }), true, l)
        }
        Ok((409, v)) if v["error"].as_str().is_some_and(|e| e.contains("past the card")) => retry_note(
            l.pick(
                "Too late to stop: the card has taken the profile and the switch is finishing.",
                "停不了了：卡已经换上新配置，切换正在收尾。",
            ),
            false,
        ),
        Ok((409, _)) => retry_note(
            l.pick("There is no retry to stop now.", "现在没有在重试的切换。"),
            false,
        ),
        Ok((c, v)) => retry_note(&crate::agent::refused(c, v).text(l), true),
        Err(f) => retry_note(&f.text(l), true),
    }
}

/// POST /router/esim/stop: stop a switch's busy-card retries. Answers at once; the switch's own
/// reply then ends with "retries stopped".
pub async fn stop(State(app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    let who = Caller::of(&app, &req);
    let r = crate::agent::post_raw(&app, &who, "/api/esim/cancel", &json!({})).await;
    if matches!(&r, Ok((c, v)) if (200..300).contains(c) && v["ok"] != false) {
        *stop_mark().lock().unwrap() = Some(Instant::now());
    }
    one(stop_reply(r.as_ref().map(|(c, v)| (*c, v)), l))
}

fn area_html(
    act: Act,
    l: Lang,
    remote: Via,
    set: &str,
    cancel: &str,
    phase: &write::Phase,
) -> String {
    write::Area {
        id: act.area(),
        l,
        remote,
        set,
        cancel,
        phase,
    }
    .html()
}

fn cancel_url(act: Act) -> String {
    format!("{PATH}/cancel?a={}", act.area())
}

fn area(act: Act, l: Lang, remote: Via, phase: &write::Phase) -> String {
    area_html(act, l, remote, "", &cancel_url(act), phase)
}

fn signals(v: Value) -> Event {
    Event::default()
        .event("datastar-patch-signals")
        .data(format!("signals {v}"))
}

/// The element that reads the card once the device is back from a reboot (a read, retried). A POST
/// although it only reads: proxy.rs ends a Datastar GET stream with the "session expired" notice
/// whenever that stream ends, and this one ends once it has its answer.
pub fn after_html(key: &str) -> String {
    format!(
        "<div id=\"es-after\" data-init=\"@post('{PATH}/after?k={key}', {{filterSignals: {{include: /^$/}}, openWhenHidden: true, retryInterval: 4000, retryScaler: 1, retryMaxWait: 4000, retryMaxCount: 80}})\"></div>"
    )
}

const AFTER_EMPTY: &str = "<div id=\"es-after\"></div>";

/// Send what the ask confirmed, once; follow the job; read back.
async fn run(
    app: Shared,
    act: Act,
    r: Req,
    who: Caller,
    l: Lang,
    tx: tokio::sync::mpsc::Sender<Event>,
) {
    let pending_text = match (act, l) {
        (Act::Switch, Lang::En) => format!("Switching to “{}”… usually about 40 s.", r.name),
        (Act::Switch, Lang::Zh) => format!("正在切换到「{}」…通常约 40 秒。", r.name),
        (Act::Delete, Lang::En) => format!("Deleting “{}”…", r.name),
        (Act::Delete, Lang::Zh) => format!("正在删除「{}」…", r.name),
        (Act::Nick, _) => l.pick("Saving the nickname…", "正在保存昵称…").into(),
        (Act::Download, _) => l
            .pick(
                "Fetching the profile… a few minutes is normal.",
                "配置下载中…要几分钟是正常的。",
            )
            .into(),
        (Act::Notif, _) => l.pick("Sending the notifications…", "正在发送通知…").into(),
    };
    // a reply lost during a switch (the device rebooted under it) gets the verifier
    let cancel = if act == Act::Switch {
        format!("{}&k={}", cancel_url(act), r.key)
    } else {
        cancel_url(act)
    };
    let pend = area_html(
        act,
        l,
        Via::Local,
        "",
        &cancel,
        &write::Phase::Pending(pending_text),
    );
    if tx.send(patch(&pend)).await.is_err() {
        return; // the tab went away before anything was sent: don't write
    }
    let body = match act {
        Act::Switch | Act::Delete => json!({"iccid": r.iccid}),
        Act::Nick => json!({"iccid": r.iccid, "nickname": r.nick}),
        Act::Download if r.conf.is_empty() => json!({"code": r.code}),
        Act::Download => json!({"code": r.code, "confirmation_code": r.conf}),
        Act::Notif => json!({}),
    };
    let o = match crate::agent::post_raw(&app, &who, act.job_path(), &body).await {
        Err(Fail::Unreachable) => write::Outcome::Unknown,
        Err(f) => write::Outcome::Failed(f.text(l)),
        Ok((code, v)) if !(200..300).contains(&code) || v["ok"] == false => refusal(code, &v, l),
        Ok((_, v)) if act == Act::Nick => {
            let _ = v;
            if readback(&app, &who, act, &r).await {
                write::Outcome::Applied(applied(act, &r, l))
            } else {
                not_read(act, &r, l)
            }
        }
        Ok((_, v)) => match v["data"]["job_id"].as_u64() {
            None => write::Outcome::Unknown,
            Some(id) => {
                let mut say = None;
                let end = wait_job(&app, &who, id, act, &mut say, l, &tx).await;
                // the core's shared word for a switch (`sim_say`); only on outcomes it describes
                let say = say.filter(|_| act == Act::Switch);
                let said = |o| with_say(o, say.as_deref(), l);
                match end {
                    JobEnd::Error { message, reason } if say.is_some() && !reason.is_empty() => {
                        // a known cause: the core's word plus what to do; the raw message is technical
                        eprintln!("[esim] switch {id}: {reason}: {message}");
                        said(write::Outcome::Failed(reason_note(&reason, l).unwrap_or("").into()))
                    }
                    JobEnd::Error { message, reason } => said(job_failed(&message, &reason, l)),
                    JobEnd::TimedOut => timed_out(l),
                    JobEnd::Done {
                        rebooting: true, ..
                    } if act == Act::Switch => {
                        *reboot_mark().lock().unwrap() = Some(Instant::now());
                        bump(&app);
                        let html = format!(
                            "{}\n{}",
                            area(
                                act,
                                l,
                                Via::Local,
                                &write::Phase::Pending(format!(
                                    "{} {}",
                                    reboot_banner(l),
                                    l.pick(
                                        "Sign in again once it is back: the status above then shows which profile is in use.",
                                        "回来后重新登录，上面的状态会显示用的是哪个配置。"
                                    )
                                ))
                            ),
                            after_html(&r.key)
                        );
                        // close the reply now: the reboot must not cut it (a cut reply reads as lost)
                        let _ = tx.send(patch(&html)).await;
                        return;
                    }
                    JobEnd::Done { stuck: true, .. } if act == Act::Switch => said(identity_stuck(&r, l)),
                    JobEnd::Done { no_data: true, .. } if act == Act::Switch => {
                        if readback(&app, &who, act, &r).await {
                            said(no_data(&r, l))
                        } else {
                            not_read(act, &r, l)
                        }
                    }
                    JobEnd::Done { .. } | JobEnd::Moved => {
                        // Moved: ours ended unseen, its last word (still running) is stale
                        let moved = matches!(end, JobEnd::Moved);
                        if readback(&app, &who, act, &r).await {
                            let o = write::Outcome::Applied(applied(act, &r, l));
                            if moved { o } else { said(o) }
                        } else {
                            not_read(act, &r, l)
                        }
                    }
                }
            }
        },
    };
    let ok = matches!(o, write::Outcome::Applied(_));
    bump(&app);
    let _ = tx
        .send(patch(&area(act, l, Via::Local, &write::Phase::Done(o))))
        .await;
    if ok {
        match act {
            Act::Nick => {
                let _ = tx.send(signals(json!({"_esEdit": ""}))).await;
            }
            Act::Download => {
                let _ = tx
                    .send(signals(json!({"_esCode": "", "_esConf": ""})))
                    .await;
            }
            _ => {}
        }
    }
}

// ---- the page ----

#[derive(Template)]
#[template(path = "esim.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    leaves: Vec<(String, String)>,
    prof_op: String,
    dl_op: String,
    nt_op: String,
}

impl Page {
    fn leaf(&self, id: &str) -> &str {
        self.leaves
            .iter()
            .find(|(k, _)| k == id)
            .map_or("", |(_, h)| h.as_str())
    }
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
    let idle = write::Phase::Idle;
    let html = Page {
        shell,
        l,
        // the card is read by the stream (lpac takes a while): the first screen is the skeleton
        leaves: render(&Card::default(), l),
        prof_op: area(Act::Switch, l, ctx.remote, &idle),
        dl_op: area(Act::Download, l, ctx.remote, &idle),
        nt_op: area(Act::Notif, l, ctx.remote, &idle),
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
    let p = Poll {
        l: ctx.lang,
        who: Caller::of(&app, &req),
        card: Card::default(),
        seen_gen: GEN.load(Ordering::Relaxed),
        first: true,
        next_chip: None,
        next_prof: None,
        settle_until: None,
        seen_sim: None,
    };
    crate::agent::stream(app, ctx, q.g, PATH, p)
}

/// Refresh / Retry: every tab reads the card again now.
pub async fn refresh(State(app): State<Shared>) -> axum::http::StatusCode {
    bump(&app);
    axum::http::StatusCode::NO_CONTENT
}

fn one(html: String) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(futures_util::stream::once(async move { Ok(patch(&html)) }))
}

#[derive(Deserialize, Default)]
pub struct KeyQuery {
    #[serde(default)]
    k: String,
}

/// The card now, for an ask: the profile asked about and the one in use, or why not now.
async fn gate(
    app: &Shared,
    who: &Caller,
    key: Option<&str>,
    l: Lang,
) -> Result<(Vec<Profile>, Option<Profile>), write::Outcome> {
    if rebooting() {
        return Err(write::Outcome::NotYet(reboot_banner(l).into()));
    }
    let ps = match read_list(app, who).await {
        Ok(Listed::List(ps)) => ps,
        Ok(Listed::Busy) => {
            return Err(write::Outcome::NotYet(job_clash(l).into()));
        }
        Ok(Listed::Settling(..)) => {
            return Err(write::Outcome::NotYet(
                l.pick(
                    "The card is still settling after the last change; try again in a few seconds.",
                    "卡片在上次改动后还没稳定，过几秒再试。",
                )
                .into(),
            ));
        }
        Err(f) => {
            return Err(write::Outcome::NotYet(match l {
                Lang::En => format!(
                    "Can't read the profiles now ({}); try again shortly",
                    f.text(l)
                ),
                Lang::Zh => format!("现在读不到配置列表（{}），稍后再试", f.text(l)),
            }));
        }
    };
    let Some(k) = key else {
        return Ok((ps, None));
    };
    let p = ps.iter().find(|p| p.key == k).cloned();
    if p.is_none() {
        return Err(write::Outcome::NotYet(
            l.pick(
                "That profile is no longer on the card; the list is refreshed",
                "卡上已经没有这个配置了；列表已刷新",
            )
            .into(),
        ));
    }
    Ok((ps, p))
}

fn ask_reply(
    act: Act,
    l: Lang,
    remote: Via,
    r: Result<(write::Confirm, Req), write::Outcome>,
) -> String {
    match r {
        Ok((c, req)) => {
            let set = format!("{PATH}/set?a={}&t={}", act.area(), put(act, req));
            area_html(
                act,
                l,
                remote,
                &set,
                &cancel_url(act),
                &write::Phase::Ask(c),
            )
        }
        Err(o) => area(act, l, remote, &write::Phase::Done(o)),
    }
}

async fn row_ask(app: Shared, act: Act, key: String, req: axum::extract::Request) -> String {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let (l, remote) = (ctx.lang, ctx.remote);
    let who = Caller::of(&app, &req);
    if !valid_key(&key) {
        return area(act, l, remote, &write::Phase::Idle);
    }
    let r = gate(&app, &who, Some(&key), l).await.and_then(|(ps, p)| {
        let p = p.expect("gate found it");
        let from = ps.iter().find(|x| x.enabled).map(Profile::name);
        if p.enabled {
            return Err(write::Outcome::NotYet(match (act, l) {
                (Act::Switch, _) => l
                    .pick("That profile is already in use", "这个配置已经在用了")
                    .into(),
                (_, _) => l
                    .pick(
                        "Can't delete the profile in use — switch away first",
                        "不能删除正在用的配置——先切到别的",
                    )
                    .into(),
            }));
        }
        let req = Req {
            iccid: p.iccid.clone(),
            key: p.key.clone(),
            name: p.name(),
            word: (act == Act::Delete).then(|| delete_word(l).into()),
            ..Req::default()
        };
        Ok((confirm(act, &req, from.as_deref(), 0, l), req))
    });
    if r.is_err() {
        bump(&app);
    }
    ask_reply(act, l, remote, r)
}

pub async fn switch_ask(
    State(app): State<Shared>,
    Query(q): Query<KeyQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    one(row_ask(app, Act::Switch, q.k, req).await)
}

pub async fn delete_ask(
    State(app): State<Shared>,
    Query(q): Query<KeyQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    one(row_ask(app, Act::Delete, q.k, req).await)
}

/// The page-local signals an ask posts (`filterSignals`).
#[derive(Deserialize, Default)]
pub struct FormIn {
    #[serde(default, rename = "_esNick")]
    nick: String,
    #[serde(default, rename = "_esCode")]
    code: String,
    #[serde(default, rename = "_esConf")]
    conf: String,
}

async fn form_in(req: axum::extract::Request) -> FormIn {
    let body = axum::body::to_bytes(req.into_body(), 16 * 1024)
        .await
        .unwrap_or_default();
    serde_json::from_slice(&body).unwrap_or_default()
}

/// "Save" of a nickname: unchanged closes the box; else the inline confirm.
pub async fn nick_ask(
    State(app): State<Shared>,
    Query(q): Query<KeyQuery>,
    req: axum::extract::Request,
) -> Response {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let (l, remote) = (ctx.lang, ctx.remote);
    let who = Caller::of(&app, &req);
    let f = form_in(req).await;
    let act = Act::Nick;
    if !valid_key(&q.k) {
        return one(area(act, l, remote, &write::Phase::Idle)).into_response();
    }
    let nick = f.nick.trim().to_owned();
    let r = gate(&app, &who, Some(&q.k), l).await;
    if let Ok((_, Some(p))) = &r
        && p.nick.clone().unwrap_or_default() == nick
    {
        let html = area(act, l, remote, &write::Phase::Idle);
        return Sse::new(futures_util::stream::iter([
            Ok::<_, Infallible>(patch(&html)),
            Ok(signals(json!({"_esEdit": ""}))),
        ]))
        .into_response();
    }
    let r = r.map(|(_, p)| {
        let p = p.expect("gate found it");
        let req = Req {
            iccid: p.iccid.clone(),
            key: p.key.clone(),
            name: p.name(),
            nick,
            ..Req::default()
        };
        (confirm(act, &req, None, 0, l), req)
    });
    one(ask_reply(act, l, remote, r)).into_response()
}

/// An activation code as the old page accepted it (`LPA:1$…` or `1$…`).
pub fn code_ok(c: &str) -> bool {
    c.get(..6).is_some_and(|p| p.eq_ignore_ascii_case("LPA:1$")) || c.starts_with("1$")
}

pub async fn download_ask(
    State(app): State<Shared>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let (l, remote) = (ctx.lang, ctx.remote);
    let who = Caller::of(&app, &req);
    let f = form_in(req).await;
    let act = Act::Download;
    let code = f.code.trim().to_owned();
    if !code_ok(&code) {
        let o = write::Outcome::NotYet(
            l.pick(
                "The activation code looks like LPA:1$smdp.example.com$XXXX-XXXX",
                "激活码的样子是 LPA:1$smdp.example.com$XXXX-XXXX",
            )
            .into(),
        );
        return one(area(act, l, remote, &write::Phase::Done(o)));
    }
    let r = gate(&app, &who, None, l).await.map(|(ps, _)| {
        let req = Req {
            code,
            conf: f.conf.trim().to_owned(),
            before: ps.len(),
            ..Req::default()
        };
        (confirm(act, &req, None, 0, l), req)
    });
    one(ask_reply(act, l, remote, r))
}

pub async fn notif_ask(
    State(app): State<Shared>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let (l, remote) = (ctx.lang, ctx.remote);
    let who = Caller::of(&app, &req);
    let act = Act::Notif;
    let r = if rebooting() {
        Err(write::Outcome::NotYet(reboot_banner(l).into()))
    } else {
        match crate::agent::get(&app, &who, "/api/esim/notifications").await {
            Ok(v) => match notifs(&v) {
                None => Err(write::Outcome::NotYet(job_clash(l).into())),
                Some(n) if n.is_empty() => Err(write::Outcome::NotYet(
                    l.pick("Nothing pending.", "没有待发送的通知。").into(),
                )),
                Some(n) => Ok(n.len()),
            },
            Err(f) => Err(write::Outcome::NotYet(f.text(l))),
        }
    };
    let r = r.map(|n| {
        let req = Req::default();
        (confirm(act, &req, None, n, l), req)
    });
    one(ask_reply(act, l, remote, r))
}

#[derive(Deserialize, Default)]
pub struct SetQuery {
    #[serde(default)]
    t: String,
    /// The area the confirm was in (for an expired token).
    #[serde(default)]
    a: String,
    #[serde(default)]
    word: Option<String>,
}

fn act_of_area(a: &str) -> Act {
    match a {
        "es-dl" => Act::Download,
        "es-nt" => Act::Notif,
        _ => Act::Switch,
    }
}

/// The confirm: what the ask kept, sent once.
pub async fn set(
    State(app): State<Shared>,
    Query(q): Query<SetQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    let who = Caller::of(&app, &req);
    let got = take(&q.t);
    Sse::new(async_stream(move |tx| async move {
        let Some((act, r)) = got else {
            let o = write::Outcome::NotYet(
                l.pick(
                    "This confirm expired; press the button again",
                    "这个确认已过期，再按一次按钮",
                )
                .into(),
            );
            let _ = tx
                .send(patch(&area(
                    act_of_area(&q.a),
                    l,
                    Via::Local,
                    &write::Phase::Done(o),
                )))
                .await;
            return;
        };
        if let Some(w) = &r.word
            && !write::word_ok(w, q.word.as_deref().unwrap_or(""))
        {
            let o = write::Outcome::Failed(
                l.pick("the confirmation word did not match", "输入的确认词不对")
                    .into(),
            );
            let _ = tx
                .send(patch(&area(act, l, Via::Local, &write::Phase::Done(o))))
                .await;
            return;
        }
        run(app, act, r, who, l, tx).await;
    }))
}

#[derive(Deserialize, Default)]
pub struct CancelQuery {
    #[serde(default)]
    a: String,
    #[serde(default)]
    lost: bool,
    /// A switch's profile: a lost reply then reads the card after the device is back.
    #[serde(default)]
    k: String,
}

pub async fn cancel(
    Query(q): Query<CancelQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let act = act_of_area(&q.a);
    let phase = write::cancel_phase(&write::CancelQuery { lost: q.lost });
    let mut html = area(act, ctx.lang, ctx.remote, &phase);
    if q.lost && act == Act::Switch && valid_key(&q.k) {
        html = format!("{html}\n{}", after_html(&q.k));
    }
    one(html)
}

/// What the verifier says once it has read the card (`no_restart`: this process was told the device
/// reboots and is still alive, so it did not).
pub fn after_outcome(
    target: Option<&Profile>,
    in_use: Option<&Profile>,
    no_restart: bool,
    l: Lang,
) -> write::Outcome {
    match target {
        Some(t) if t.enabled => write::Outcome::Applied(match l {
            Lang::En => format!("Switched to “{}”", t.name()),
            Lang::Zh => format!("已切换到「{}」", t.name()),
        }),
        _ => {
            let using = in_use.map_or_else(
                || l.pick("no profile", "没有配置").to_string(),
                |p| match l {
                    Lang::En => format!("“{}”", p.name()),
                    Lang::Zh => format!("「{}」", p.name()),
                },
            );
            write::Outcome::Ended(
                match (l, no_restart) {
                    (Lang::En, true) => format!(
                        "Not switched: the device did not restart and the card still uses {using}."
                    ),
                    (Lang::En, false) => format!(
                        "Not switched: the card now uses {using}. Switch again, or keep it."
                    ),
                    (Lang::Zh, true) => format!("没有切过去：设备没有重启，卡上用的还是{using}。"),
                    (Lang::Zh, false) => {
                        format!("没有切过去：卡上现在用的是{using}。可以再切一次，或者就这样用。")
                    }
                },
                "bad",
            )
        }
    }
}

/// The reboot verifier: waits for the device to be back, reads the list, says whether the switch
/// took. A read: Datastar retries it while the device is down.
pub async fn after(
    State(app): State<Shared>,
    Query(q): Query<KeyQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let who = Caller::of(&app, &req);
    let key = q.k;
    Sse::new(async_stream(move |tx| async move {
        if !valid_key(&key) {
            let _ = tx.send(patch(AFTER_EMPTY)).await;
            return;
        }
        let deadline = Instant::now() + REBOOT_WAIT;
        let o = loop {
            if tx.is_closed() {
                return;
            }
            // this process was told the device reboots: give the reboot time to happen
            let mark = *reboot_mark().lock().unwrap();
            if let Some(t) = mark
                && t.elapsed() < NO_REBOOT_AFTER
            {
                tokio::time::sleep(NO_REBOOT_AFTER - t.elapsed()).await;
                continue;
            }
            if let Ok(Listed::List(ps)) = read_list(&app, &who).await {
                let target = ps.iter().find(|p| p.key == key);
                let in_use = ps.iter().find(|p| p.enabled);
                break after_outcome(target, in_use, mark.is_some(), l);
            }
            if Instant::now() >= deadline {
                break write::Outcome::Ended(
                    l.pick(
                        "The profiles still can't be read; check the list above once the device is back.",
                        "还读不到配置列表；设备回来后看上面的列表确认用的是哪个配置。",
                    )
                    .into(),
                    "warn",
                );
            }
            tokio::time::sleep(PROFILES_EVERY).await;
        };
        *reboot_mark().lock().unwrap() = None;
        bump(&app);
        let html = format!(
            "{}\n{AFTER_EMPTY}",
            area(Act::Switch, l, Via::Local, &write::Phase::Done(o))
        );
        let _ = tx.send(patch(&html)).await;
    }))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod resend_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resend_reads_the_cores_word() {
        assert_eq!(resend_of(&json!({"data": []})), None, "older core");
        assert_eq!(resend_of(&json!({"resend": {"next_s": null, "gave_up": false}})), None);
        assert_eq!(resend_of(&json!({"resend": {"next_s": 90, "gave_up": false}})), Some(Resend::In(90)));
        assert_eq!(resend_of(&json!({"resend": {"next_s": null, "gave_up": true}})), Some(Resend::Stopped));
    }
}
