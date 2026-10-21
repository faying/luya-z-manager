//! /changes (改动记录), the old `changes/page.tsx` (E4 T9b; write-op-layer.md DD5, DD10, DD11):
//! every setting changed through u60d, newest first, two lines each, all words u60d's
//! (`journal.list` decorates every row: `what_*`, `change_*`, `result_*`, `source_*`, `mark`,
//! `hide`, `undo_view`). A row opens its detail (`?op=<op_id>`, or `?row=<key>` for a row without
//! one); undo / redo lives there.
//!
//! ```text
//!  u60d /control journal.list {limit 50} ─▶ CACHE (one read for every tab: when the `op` block's
//!     transaction ids or phases move, 15 s after the last read, after an undo, 重试) ─▶ kick
//!  撤销 (tier 3 for network mode, else 2) ─▶ read the journal again, the row's undo_view.ok?
//!     ─▶ /control {undo_view.request, undo: true, source: web, op_id: NEW} ─▶ follow THAT op_id
//!        in the `op` block: final phase → u60d's words and tone; still live → "the bar follows"
//! ```
//! - **The data is u60d's**: the agent's `/api/ops/journal` and `/api/ops/write` only relay
//!   `journal.list` and the write with `source: web` + `undo: true` + an op_id. Its two guards are
//!   kept here: only transaction actions ([`UNDO_ACTIONS`]) are sent, and nothing while u60d's
//!   executor is stuck.
//! - **Never the old op_id**: u60d dedups transaction actions by op_id, so resending the row's own
//!   id would answer with the old, finished transaction (a false success). Every undo gets
//!   [`write::next_op_id`], and the readback follows that id, not "the op block changed" (`last`
//!   may still hold the row's own transaction in a final phase).
//! - **The server picks the request**: the ask and the confirm carry only the row's op_id; the
//!   request is taken from a fresh read at send time, with `undo_view.ok` checked again (a second
//!   tab's undo turns the row into 「之后又改过」).
//! - The result stays in this page's write area (the old page went back to the list and left it to
//!   the bar; pages are separate loads here, so a brief result would be lost on the way).

use std::convert::Infallible;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use askama::Template;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;

use crate::topbar::Via;
use crate::agent::Polled;
use crate::hub::{Hub, St};
use crate::i18n::Lang;
use crate::pages::{Shared, split_leaves};
use crate::sms::ReadErr;
use crate::write;

pub const PATH: &str = "/changes";
/// Rows asked for (the old page's `JOURNAL_LIMIT`).
pub const LIMIT: u64 = 50;
/// Read again this long after the last read while a tab is open (the old page's poll).
const EVERY: Duration = Duration::from_secs(15);
/// How often a tab's stream checks whether a read is due.
const TICK: Duration = Duration::from_secs(5);
const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// A transaction write waits for the vendor call (the agent gives it 30 s).
const SEND_TIMEOUT: Duration = Duration::from_secs(30);
/// What the agent's `/api/ops/write` lets through: u60d's transaction actions.
pub const UNDO_ACTIONS: [&str; 11] = [
    "network.set_mode",
    "band.set_lte",
    "band.set_nr",
    "band.reset",
    "cell.lock_lte",
    "cell.lock_nr",
    "apn.set_mode",
    "apn.enable",
    "apn.modify",
    // items wifi.main / wifi.guest: the journal's request carries the action
    "wifi.settings",
    "wifi.guest",
];
/// Undo that reloads Wi-Fi (items wifi.main / wifi.guest): tier 3 too, in Wi-Fi words; Tailscale
/// goes over the mobile link, so no Tailscale warning.
pub const RELOADS_WIFI: [&str; 2] = ["wifi.settings", "wifi.guest"];
/// Undo that cuts the mobile link: the tier-3 dialog (DD10; controls-inventory 三（网络模式）/ 二).
pub const CUTS_UPLINK: [&str; 9] = [
    "network.set_mode",
    "band.set_lte",
    "band.set_nr",
    "band.reset",
    "cell.lock_lte",
    "cell.lock_nr",
    "apn.set_mode",
    "apn.enable",
    "apn.modify",
];

// ---- the shared read ----

#[derive(Default)]
pub struct Got {
    /// The newest good `entries` (newest first) and when it came.
    pub ok: Option<(Vec<Value>, SystemTime)>,
    pub err: Option<ReadErr>,
    at: Option<Instant>,
    tried: Option<OpKey>,
    inflight: bool,
    force: bool,
}

static CACHE: Mutex<Got> = Mutex::new(Got {
    ok: None,
    err: None,
    at: None,
    tried: None,
    inflight: false,
    force: false,
});

/// The `op` block's transaction ids and phases: when they move, a row was (or will be) added.
type OpKey = (String, String, String, String);

fn op_key(st: &St) -> OpKey {
    let op = st.blocks.get("op").map(|b| &b.data);
    let f = |side: &str, k: &str| {
        op.and_then(|o| o[side][k].as_str())
            .unwrap_or("")
            .to_owned()
    };
    (
        f("active", "op_id"),
        f("active", "phase"),
        f("last", "op_id"),
        f("last", "phase"),
    )
}

/// `journal.list` from u60d (a read: no op_id, not journaled).
async fn read_journal(app: &Shared) -> Result<Vec<Value>, ReadErr> {
    let body = json!({"action": "journal.list", "params": {"limit": LIMIT}, "source": "web"});
    let r = app
        .http
        .post(&app.control_url)
        .timeout(READ_TIMEOUT)
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            if e.is_connect() {
                ReadErr::Unreachable
            } else {
                ReadErr::NoReply
            }
        })?;
    let v: Value = r.json().await.map_err(|_| ReadErr::NoReply)?;
    if v["ok"].as_bool() == Some(true) {
        return Ok(v["result"]["entries"]
            .as_array()
            .cloned()
            .unwrap_or_default());
    }
    Err(ReadErr::Refused(
        v["error"]["message"]
            .as_str()
            .unwrap_or("refused")
            .to_owned(),
    ))
}

/// Read now, store it, kick the tabs.
pub async fn read_now(app: &Shared) -> Result<Vec<Value>, ReadErr> {
    let k = app.hub.read(op_key);
    let r = read_journal(app).await;
    {
        let mut c = CACHE.lock().unwrap();
        c.tried = Some(k);
        c.at = Some(Instant::now());
        match &r {
            Ok(e) => {
                c.ok = Some((e.clone(), SystemTime::now()));
                c.err = None;
            }
            Err(e) => {
                c.err = Some(e.clone());
            }
        }
    }
    let _ = app.kick.send(PATH);
    r
}

/// The newest good change log (newest first), if any read has come back.
pub fn cached() -> Option<Vec<Value>> {
    CACHE.lock().unwrap().ok.as_ref().map(|(e, _)| e.clone())
}

/// [`ensure`] for other pages (the network mode page's "last change" link).
pub fn ensure_read(app: &Shared) {
    ensure(app);
}

/// Start the shared read when it is due and none runs; the tabs hear about it through the kick.
fn ensure(app: &Shared) {
    let k = app.hub.read(op_key);
    let mut c = CACHE.lock().unwrap();
    let due = c.force || c.tried.as_ref() != Some(&k) || c.at.is_none_or(|t| t.elapsed() >= EVERY);
    if c.inflight || !due {
        return;
    }
    c.inflight = true;
    c.force = false;
    drop(c);
    let app = app.clone();
    tokio::spawn(async move {
        let _ = read_now(&app).await;
        CACHE.lock().unwrap().inflight = false;
    });
}

// ---- one row, in the page's language ----

/// u60d's paired texts: the page's language, the other when it is missing.
pub(crate) fn pick(e: &Value, field: &str, l: Lang) -> String {
    let zh = e[format!("{field}_zh")].as_str().unwrap_or("");
    let en = e[format!("{field}_en")].as_str().unwrap_or("");
    match l {
        Lang::En if !en.is_empty() => en,
        Lang::Zh if !zh.is_empty() => zh,
        _ => {
            if zh.is_empty() {
                en
            } else {
                zh
            }
        }
    }
    .to_owned()
}

/// The rows to list: u60d marks the ones that are not rows of their own.
pub fn visible(entries: &[Value]) -> Vec<&Value> {
    entries
        .iter()
        .filter(|e| e["hide"].as_bool() != Some(true))
        .collect()
}

/// FNV-1a: a key that stays with a row without an op_id when newer rows push it down.
fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)
    })
}

/// An op_id as u60d takes it (1–64 of `[A-Za-z0-9._-]`): safe in a URL and a JS string.
pub fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// Which row a detail is about.
#[derive(Clone, Debug, PartialEq, Deserialize, Default)]
pub struct Want {
    #[serde(default)]
    pub op: Option<String>,
    /// `row=<hex>` for a row without an op_id (t | action | source | result).
    #[serde(default)]
    pub row: Option<String>,
}

impl Want {
    pub fn of(e: &Value) -> Want {
        match e["op_id"].as_str().filter(|s| valid_id(s)) {
            Some(id) => Want {
                op: Some(id.to_owned()),
                row: None,
            },
            None => {
                let k = ["t", "action", "source", "result_zh"]
                    .map(|f| e[f].as_str().unwrap_or(""))
                    .join("|");
                Want {
                    op: None,
                    row: Some(format!("{:016x}", fnv(&k))),
                }
            }
        }
    }
    pub fn any(&self) -> bool {
        self.op.is_some() || self.row.is_some()
    }
    /// The query that opens this row (also the stream's).
    pub fn query(&self) -> String {
        match (&self.op, &self.row) {
            (Some(o), _) if valid_id(o) => format!("op={o}"),
            (_, Some(r)) if r.bytes().all(|b| b.is_ascii_hexdigit()) => format!("row={r}"),
            _ => String::new(),
        }
    }
    pub fn matches(&self, e: &Value) -> bool {
        self.any() && Want::of(e) == *self
    }
}

/// "10-03 14:32" from u60d's device-local "2026-10-03 14:32:07" (the old `shortWhen`).
pub fn short_when(t: &str) -> String {
    if t.len() < 16 || !t.is_char_boundary(16) {
        return t.to_owned();
    }
    format!("{} {}", &t[5..10], &t[11..16])
}

/// `ok` / `warn` / `bad` / `neutral` from u60d's `mark`.
pub fn tone(mark: &Value) -> &'static str {
    match mark.as_str() {
        Some("ok") => "ok",
        Some("warn") => "warn",
        Some("bad") => "bad",
        _ => "neutral",
    }
}

pub struct Row {
    pub href: String,
    pub tone: &'static str,
    pub what: String,
    /// "自动 → 只用 4G" (device text can be in it: eSIM names), may be empty.
    pub change: String,
    /// "10-03 14:32 · 网页 · 已切到只用 4G".
    pub line2: String,
    /// Who approved an agent change and whether it rolls back, for a `source: mcp` row (R18-P0-03).
    pub agent: Option<String>,
}

/// The agent label of a row the MCP agent made (R18-P0-03): where the permission came from, and
/// whether it rolls back. `approved_by` web/screen = a person approved the agent's proposal there;
/// missing or mcp = the agent approved it itself under its act token (the core's write gate allows
/// only those). A trial (a txn line, `rollback_to` present) is rolled back by the device by
/// itself; a plain write is not.
pub fn agent_label(e: &Value, l: Lang) -> Option<String> {
    if e["source"].as_str() != Some("mcp") {
        return None;
    }
    let who = match (e["approved_by"].as_str(), l) {
        (Some("web"), Lang::En) => "Proposed by the AI agent, approved by you on the web page",
        (Some("web"), _) => "由 AI 代理提议、你在网页批准",
        (Some("screen"), Lang::En) => "Proposed by the AI agent, approved by you on the screen",
        (Some("screen"), _) => "由 AI 代理提议、你在触屏批准",
        (_, Lang::En) => "Changed by the AI agent (its own act permission)",
        (_, _) => "由 AI 代理改动（它自己的 act 授权）",
    };
    let undo = match (e.get("rollback_to").is_some(), l) {
        (true, Lang::En) => "rolls back by itself",
        (false, Lang::En) => "no automatic rollback",
        (true, _) => "可自动回滚",
        (false, _) => "不能自动回滚",
    };
    Some(format!("{who} · {undo}"))
}

/// The second line, without repeating a source the result already starts with
/// ("情景跳过 ×5 …"), the old `entryLine2`.
pub fn line2(e: &Value, l: Lang) -> String {
    let result = pick(e, "result", l);
    let source = pick(e, "source", l);
    let mut parts = vec![short_when(e["t"].as_str().unwrap_or(""))];
    if !source.is_empty() && !result.starts_with(&source) {
        parts.push(source);
    }
    parts.push(result);
    parts.retain(|p| !p.is_empty());
    parts.join(" · ")
}

pub fn row(e: &Value, l: Lang) -> Row {
    let q = Want::of(e).query();
    Row {
        href: format!("{PATH}?{q}"),
        tone: tone(&e["mark"]),
        what: pick(e, "what", l),
        change: pick(e, "change", l),
        line2: line2(e, l),
        agent: agent_label(e, l),
    }
}

/// The undo button of a detail.
pub struct UndoBtn {
    pub label: String,
    pub on: bool,
    /// u60d's reason it can't be undone.
    pub why: Option<String>,
    /// Can be undone, but u60d's executor is stuck.
    pub stuck: bool,
    pub op: String,
}

pub struct Detail {
    pub tone: &'static str,
    pub result: String,
    pub what: String,
    pub change: String,
    pub meta: String,
    pub agent: Option<String>,
    pub undo: Option<UndoBtn>,
}

/// A row's detail; `gone` = it fell out of the latest 50 (shown as it was, without undo: it can't
/// be the latest change of its item any more).
pub fn detail(e: &Value, l: Lang, stuck: bool, gone: bool) -> Detail {
    let u = &e["undo_view"];
    let undo = (!gone && u.is_object()).then(|| {
        let ok = u["ok"].as_bool() == Some(true) && u["request"].is_object();
        let why = pick(u, "why", l);
        UndoBtn {
            label: pick(u, "label", l),
            on: ok && !stuck,
            why: (!ok && !why.is_empty()).then_some(why),
            stuck: ok && stuck,
            op: e["op_id"]
                .as_str()
                .filter(|s| valid_id(s))
                .unwrap_or("")
                .to_owned(),
        }
    });
    let meta = [
        short_when(e["t"].as_str().unwrap_or("")),
        pick(e, "source", l),
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join(" · ");
    Detail {
        tone: tone(&e["mark"]),
        result: pick(e, "result", l),
        what: pick(e, "what", l),
        change: pick(e, "change", l),
        meta,
        agent: agent_label(e, l),
        undo,
    }
}

pub enum Main {
    Loading,
    /// Never read: 「读不到改动记录 · 数据服务没响应」 + 重试.
    Failed,
    List {
        rows: Vec<Row>,
        /// The newest read failed: kept, greyed, 「数字停在 hh:mm」.
        stopped: Option<String>,
    },
    Detail(Detail),
}

pub struct View {
    pub l: Lang,
    pub main: Main,
}

impl View {
    pub fn sym(&self, tone: &str) -> &'static str {
        crate::sms::symbol(tone)
    }
    pub fn unreadable(&self) -> &'static str {
        self.l.pick(
            "Can't read change log · data service not responding",
            "读不到改动记录 · 数据服务没响应",
        )
    }
}

/// The page as the cache and the hub are now. `snap` = the detail's row as last seen (kept when
/// it leaves the latest 50).
pub fn view(got: &Got, want: &Want, snap: &mut Option<Value>, stuck: bool, l: Lang) -> View {
    let main = match &got.ok {
        None if got.err.is_some() => Main::Failed,
        None => Main::Loading,
        Some((entries, at)) => {
            let rows = visible(entries);
            let found = rows.iter().find(|e| want.matches(e)).map(|e| (*e).clone());
            if let Some(e) = found {
                *snap = Some(e.clone());
                Main::Detail(detail(&e, l, stuck, false))
            } else if let Some(e) = snap.as_ref() {
                Main::Detail(detail(e, l, stuck, true))
            } else {
                Main::List {
                    rows: rows.iter().map(|e| row(e, l)).collect(),
                    stopped: got
                        .err
                        .is_some()
                        .then(|| crate::agent::stopped(l, "Numbers", "数字", Some(*at))),
                }
            }
        }
    };
    View { l, main }
}

// ---- undo ----

/// Tier 3 (dialog) when the undo cuts the mobile link or reloads Wi-Fi, else tier 2.
pub fn tier(action: &str) -> write::Tier {
    if CUTS_UPLINK.contains(&action) || RELOADS_WIFI.contains(&action) {
        write::Tier::Dialog
    } else {
        write::Tier::Inline
    }
}

/// The confirm of a row's undo (the old page's words).
pub fn confirm(e: &Value, l: Lang) -> write::Confirm {
    let u = &e["undo_view"];
    let label = pick(u, "label", l);
    let what = pick(e, "what", l);
    let change = pick(e, "change", l);
    let action = u["request"]["action"].as_str().unwrap_or("");
    let t = tier(action);
    let colon = l.pick(": ", "：");
    if t == write::Tier::Dialog {
        write::Confirm {
            tier: t,
            title: format!("{label}{colon}{what}{}", l.pick("?", "？")),
            what: if change.is_empty() {
                label.clone()
            } else {
                format!(
                    "{}{change}",
                    l.pick("This change is undone: ", "撤回这次改动：")
                )
            },
            downtime: Some(
                if RELOADS_WIFI.contains(&action) {
                    l.pick(
                        "Wi-Fi reloads: devices on it drop for about 10–60 seconds.",
                        "Wi-Fi 重新加载：连着它的设备断开约 10–60 秒。",
                    )
                } else {
                    l.pick(
                        "The mobile connection drops for about 30 seconds while the modem re-attaches.",
                        "调制解调器重新附着，移动网络断开约 30 秒。",
                    )
                }
                .into(),
            ),
            recovery: Some(
                if RELOADS_WIFI.contains(&action) {
                    l.pick(
                        "If this browser is on the U60's Wi-Fi it loses the page for a while; if the name or password goes back, join with the old one. Over a cable, USB or Tailscale (which goes over the mobile link) the page is not affected.",
                        "如果这个浏览器连的是 U60 的 Wi-Fi，页面会断开一会儿；名称或密码改回去的话，要用原来的重新连。经网线、USB 或 Tailscale（走移动网络）打开的不受影响。",
                    )
                } else if action.starts_with("apn.") {
                    l.pick(
                        "Switch APN Mode back to Auto on the APN page (or on the device's touchscreen). If the page can't be reached, connect to the U60's Wi-Fi and open it from there.",
                        "在「APN 设置」页（或设备触屏上）把 APN 模式改回「自动」。如果打不开页面，连上 U60 的 Wi-Fi 再打开。",
                    )
                } else if action.starts_with("cell.") {
                    l.pick(
                        "Press “Unlock all” on the Cell lock page. If the page can't be reached, open cell lock on the device's touchscreen and restore the default, or reboot the device.",
                        "在「小区锁定」页按「全部解锁」。如果页面打不开，在设备触屏的小区锁定里恢复默认，或重启设备。",
                    )
                } else if action.starts_with("band.") {
                    l.pick(
                        "Press “Reset / unlock all” on the Band lock page. If the page can't be reached, open band lock on the device's touchscreen and restore the default.",
                        "在「频段锁定」页点「重置 / 全部解锁」。如果打不开页面，到设备触屏的「锁频」页恢复默认。",
                    )
                } else {
                    l.pick(
                        "Set it back on the Network mode page. If the page can't be reached, set the network mode back on the device's touchscreen.",
                        "在「网络模式」页改回来。如果页面打不开，在设备触屏上把网络模式改回来。",
                    )
                }
                .into(),
            ),
            action: label,
            cuts_uplink: !RELOADS_WIFI.contains(&action),
            word: None,
            danger: false,
            revert: crate::write::Revert::No,
        }
    } else {
        write::Confirm {
            tier: t,
            title: label.clone(),
            what: if change.is_empty() {
                label.clone()
            } else {
                format!("{label}{colon}{what} · {change}")
            },
            downtime: None,
            recovery: None,
            action: label,
            cuts_uplink: false,
            word: None,
            danger: false,
            revert: crate::write::Revert::No,
        }
    }
}

/// The `/control` body of a row's undo with a new `op_id`, or why it is not sent.
pub fn undo_body(e: &Value, op_id: &str, stuck: bool, l: Lang) -> Result<Value, write::Outcome> {
    let u = &e["undo_view"];
    if u["ok"].as_bool() != Some(true) || !u["request"].is_object() {
        let why = pick(u, "why", l);
        return Err(write::Outcome::NotYet(if why.is_empty() {
            l.pick("This change can't be undone", "这次改动不能撤销")
                .into()
        } else {
            why
        }));
    }
    if stuck {
        return Err(write::Outcome::NotYet(stuck_reason(l).into()));
    }
    let r = &u["request"];
    let action = r["action"].as_str().unwrap_or("");
    if !UNDO_ACTIONS.contains(&action) || !r["params"].is_object() {
        return Err(write::Outcome::Failed(
            l.pick("not a transaction action", "不是可以撤销的改动")
                .into(),
        ));
    }
    if e["op_id"].as_str() == Some(op_id) {
        // never: u60d would answer with the old, finished transaction
        return Err(write::Outcome::Failed("op_id".into()));
    }
    Ok(json!({
        "action": action,
        "params": r["params"],
        "undo": true,
        "source": "web",
        "op_id": op_id,
    }))
}

fn stuck_reason(l: Lang) -> &'static str {
    l.pick(
        "Data service not responding · settings can't be changed for now",
        "数据服务没响应 · 暂时不能改设置",
    )
}

/// What the `op` block says about one transaction.
#[derive(Debug, Clone, PartialEq)]
pub enum Seen {
    Nothing,
    Live(Value),
    Final(Value),
}

pub fn seen(st: &St, op_id: &str) -> Seen {
    let Some(op) = st.blocks.get("op").map(|b| &b.data) else {
        return Seen::Nothing;
    };
    if op["last"]["op_id"].as_str() == Some(op_id) {
        return Seen::Final(op["last"].clone());
    }
    if op["active"]["op_id"].as_str() == Some(op_id) {
        return Seen::Live(op["active"].clone());
    }
    Seen::Nothing
}

/// Wait (up to `timeout`) for transaction `op_id` to reach a final phase; the last thing seen.
/// `changes` was subscribed before the send.
pub async fn follow(
    hub: &Hub,
    op_id: &str,
    timeout: Duration,
    mut changes: tokio::sync::broadcast::Receiver<crate::hub::Change>,
) -> Seen {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut last = Seen::Nothing;
    loop {
        match hub.read(|st| seen(st, op_id)) {
            f @ Seen::Final(_) => return f,
            Seen::Live(v) => last = Seen::Live(v),
            Seen::Nothing => {}
        }
        match tokio::time::timeout_at(deadline, changes.recv()).await {
            Ok(Ok(_)) | Ok(Err(RecvError::Lagged(_))) => continue,
            _ => return last,
        }
    }
}

/// What the write area says once the readback is done.
pub fn outcome(s: &Seen, sent: write::Sent, l: Lang) -> write::Outcome {
    match s {
        Seen::Final(v) => {
            let say = pick(v, "say", l);
            match tone(&v["mark"]) {
                "ok" => write::Outcome::Applied(say),
                t => write::Outcome::Ended(say, if t == "neutral" { "warn" } else { t }),
            }
        }
        Seen::Live(v) => {
            let say = pick(v, "say", l);
            write::Outcome::AcceptedSaid(match l {
                Lang::En => format!(
                    "Accepted by the device · {say}; the result will show in the bar at the top"
                ),
                Lang::Zh => format!("设备已接受 · {say}，结果会显示在顶部横条"),
            })
        }
        Seen::Nothing => sent.not_read(),
    }
}

// ---- templates ----

#[derive(Template)]
#[template(path = "changes_leaves.html")]
struct Leaves<'a> {
    v: &'a View,
}

#[derive(Template)]
#[template(path = "changes.html")]
struct Page {
    shell: crate::shell::Shell,
    main: String,
    undo_op: String,
}

pub fn leaves(v: &View) -> Vec<(String, String)> {
    split_leaves(&Leaves { v }.render().unwrap())
}

fn area(l: Lang, remote: Via, set: &str, phase: &write::Phase) -> String {
    write::Area {
        id: "ch-undo",
        l,
        remote,
        set,
        cancel: "/changes/undo/cancel",
        phase,
    }
    .html()
}

// ---- the stream ----

pub struct Poll {
    l: Lang,
    want: Want,
    snap: Option<Value>,
}

impl Poll {
    fn leaves(&mut self, app: &Shared) -> Vec<(String, String)> {
        let stuck = app.hub.read(|st| st.stuck);
        let c = CACHE.lock().unwrap();
        leaves(&view(&c, &self.want, &mut self.snap, stuck, self.l))
    }
}

impl Polled for Poll {
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        ensure(app);
        (self.leaves(app), TICK)
    }
    fn watches(&self, block: &str) -> bool {
        block == "op" || block == "exec"
    }
    fn render(&mut self, app: &Shared) -> Option<Vec<(String, String)>> {
        ensure(app);
        Some(self.leaves(app))
    }
}

pub async fn page(
    State(app): State<Shared>,
    Query(want): Query<Want>,
    req: axum::extract::Request,
) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let q = want.query();
    let shell = app.hub.read(|st| {
        let sep = if q.is_empty() { "" } else { "&" };
        crate::shell::Shell::new(&headers, PATH).live(
            st,
            &ctx,
            Some(format!("{PATH}/events?g={}{sep}{q}", st.generation)),
        )
    });
    // the rows come with the first push (the first screen is near its 40 KiB)
    let v = View {
        l: ctx.lang,
        main: Main::Loading,
    };
    let main = leaves(&v).into_iter().map(|(_, h)| h).collect::<String>();
    let html = Page {
        shell,
        main,
        undo_op: area(ctx.lang, ctx.remote, "/changes/undo", &write::Phase::Idle),
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

#[derive(Deserialize, Default)]
pub struct EventsQuery {
    #[serde(default)]
    g: u64,
    #[serde(flatten)]
    want: Want,
}

pub async fn events(
    State(app): State<Shared>,
    Query(q): Query<EventsQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let p = Poll {
        l: ctx.lang,
        want: q.want,
        snap: None,
    };
    crate::agent::stream(app, ctx, q.g, PATH, p)
}

/// 重试: read now.
pub async fn refresh(State(app): State<Shared>) -> axum::http::StatusCode {
    app.hub.retry();
    CACHE.lock().unwrap().force = true;
    ensure(&app);
    axum::http::StatusCode::NO_CONTENT
}

fn one(html: String) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(futures_util::stream::once(async move {
        Ok(crate::pages::patch(&html))
    }))
}

#[derive(Deserialize, Default)]
pub struct Op {
    #[serde(default)]
    op: String,
}

/// The row with this op_id in a fresh read (falls back to what the tabs have when u60d doesn't
/// answer: the ask only shows the confirm, the send reads again).
async fn row_by_op(app: &Shared, op: &str) -> Result<Option<Value>, ReadErr> {
    let want = Want {
        op: Some(op.to_owned()),
        row: None,
    };
    let entries = read_now(app).await?;
    Ok(visible(&entries)
        .into_iter()
        .find(|e| want.matches(e))
        .cloned())
}

fn gone(l: Lang) -> write::Outcome {
    write::Outcome::NotYet(
        l.pick(
            "This change is no longer among the latest 50",
            "这次改动已不在最近 50 条里",
        )
        .into(),
    )
}

/// 撤销 / 重做: the confirm (tier 3 for network mode, else tier 2), or why not.
pub async fn ask(
    State(app): State<Shared>,
    Query(q): Query<Op>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let set = format!("/changes/undo?op={}", q.op);
    let stuck = app.hub.read(|st| st.stuck);
    let phase = if !valid_id(&q.op) {
        write::Phase::Done(gone(l))
    } else {
        match row_by_op(&app, &q.op).await {
            Err(e) => write::Phase::Done(write::Outcome::Failed(e.text(l))),
            Ok(None) => write::Phase::Done(gone(l)),
            Ok(Some(e)) => match undo_body(&e, "lw-check", stuck, l) {
                Err(o) => write::Phase::Done(o),
                Ok(_) => write::Phase::Ask(confirm(&e, l)),
            },
        }
    };
    one(area(l, ctx.remote, &set, &phase))
}

pub async fn cancel(
    Query(q): Query<write::CancelQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    one(area(
        ctx.lang,
        ctx.remote,
        "/changes/undo",
        &write::cancel_phase(&q),
    ))
}

/// The confirmed undo: read the row again, send its request once with a new op_id and
/// `undo: true`, follow that op_id in the `op` block.
pub async fn undo(
    State(app): State<Shared>,
    Query(q): Query<Op>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let l = Lang::from_headers(&headers);
    let set = format!("/changes/undo?op={}", q.op);
    let pending = area(
        l,
        Via::Local,
        &set,
        &write::Phase::Pending(l.pick("Sending…", "正在发送…").into()),
    );
    let done = async move {
        let o = run_undo(&app, &q.op, l).await;
        let _ = read_now(&app).await;
        area(l, Via::Local, &set, &write::Phase::Done(o))
    };
    write::respond(pending, done)
}

async fn run_undo(app: &Shared, op: &str, l: Lang) -> write::Outcome {
    if !valid_id(op) {
        return gone(l);
    }
    let e = match row_by_op(app, op).await {
        Err(e) => return write::Outcome::Failed(e.text(l)),
        Ok(None) => return gone(l),
        Ok(Some(e)) => e,
    };
    let id = write::next_op_id();
    let stuck = app.hub.read(|st| st.stuck);
    let body = match undo_body(&e, &id, stuck, l) {
        Ok(b) => b,
        Err(o) => return o,
    };
    // subscribe before sending, so a phase that lands during the request is not missed
    let changes = app.hub.watch();
    let sent =
        match write::send_body(&app.http, &app.control_url, &body, Some(SEND_TIMEOUT), l).await {
            Ok(s) => s,
            Err(o) => return o,
        };
    let s = follow(&app.hub, &id, write::READBACK, changes).await;
    outcome(&s, sent, l)
}

#[cfg(test)]
mod tests;
