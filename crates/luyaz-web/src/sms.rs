//! /sms (短信收件箱): status (unread, storage use), the SIM / device filter,
//! mark read and delete on a selection, the list, and a message opened in a dialog. The compose
//! page is `sms_compose.rs`; /sms/forward stays with the agent until 8d.
//!
//! ```text
//!  u60d `sms` block (unread, max_id, count) ─┬─▶ status: N 条未读
//!                                            └─▶ changed ─▶ ONE read for every open tab:
//!     /control sms.list_after {after_id, limit 50} until !has_more ─▶ CACHE ─▶ kick /sms streams
//!  agent /api/sms/capacity (each tab: 30 s, and after each list read) ─▶ SIM / device / total used
//!  标为已读 (tier 1) ─▶ sms.mark_read {ids, tag 0} ─▶ read the list again until those rows read
//!  open a row       ─▶ the dialog; unread → the same write for that one id, its mark follows it
//!  删除 (tier 3)     ─▶ dialog ─▶ sms.delete ─▶ list: survivors? ─▶ sms.db_delete them ─▶ list: gone?
//! ```
//! - **The list is `sms.list_after`, not the `sms_list` block**: the block is the newest 8 of each
//!   store with no tag (sent / failed / draft all read as read), no store and no year. list_after
//!   pages through both stores (u60d tags each row with the store it read it from, `mem_store`)
//!   and is a read on u60d's executor, so it runs only when the `sms` block moves (a new message,
//!   a tag change, a delete), after a write, on 刷新, and again 15 s after a failed read; one
//!   request at a time, shared by all tabs (like /router/qci's AT read).
//! - **Capacity** (per store used / total, 「快满了」) is not in any block: the agent's
//!   `/api/sms/capacity` with the tab's own session, like batch 1's agent reads.
//! - **Delete is the agent's `/api/sms/delete` order**: the firmware's delete answers ok but leaves
//!   rows stored on the SIM, so after `sms.delete` the list is read, and only the survivors go to
//!   `sms.db_delete` (u60d's fixed-SQL delete in the vendor's sms.db); then read again.
//! - Selection is plain checkboxes (`name=id`, value = the numeric id) that a push never touches
//!   (morph keeps a checkbox's state while the server never renders `checked`); the counts and
//!   the ids a button sends are read from the DOM. Only numeric ids ever go into an expression or
//!   a URL: numbers and texts are device data (a sender can be letters) and stay text content.
//! - Numbers and texts are never logged.

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

use crate::agent::{Caller, Polled};
use crate::hub::{Link, St};
use crate::i18n::Lang;
use crate::pages::{Shared, StreamQuery, async_stream, split_leaves};
use crate::write;

pub const PATH: &str = "/sms";
/// Capacity from the agent, per tab.
const CAP_EVERY: Duration = Duration::from_secs(30);
/// A whole list read (u60d pages ≤ 20 per store, each one ubus call).
const LIST_TIMEOUT: Duration = Duration::from_secs(30);
/// After a failed list read, the next one (the old page polled every 10 s).
const RETRY: Duration = Duration::from_secs(15);
/// The row shows two lines; the dialog has the whole text.
const PREVIEW: usize = 140;

// ---- the firmware's row ----

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Store {
    Sim,
    Device,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Msg {
    pub id: u64,
    /// Decoded; empty = unknown.
    pub number: String,
    pub text: String,
    /// "2026-05-29 10:56", or the raw field when it does not parse.
    pub date: String,
    /// "0" read, "1" unread, "2" sent, "3" send failed, "4" draft (the firmware's strings).
    pub tag: String,
    /// None: u60d older than the `mem_store` tag on list_after rows.
    pub store: Option<Store>,
}

impl Msg {
    pub fn unread(&self) -> bool {
        self.tag == "1"
    }
}

/// UCS-2 (UTF-16BE) hex → text, when it is that shape.
fn ucs2(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.len() < 4 || !s.len().is_multiple_of(4) || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let units: Vec<u16> = s
        .as_bytes()
        .chunks(4)
        .filter_map(|c| u16::from_str_radix(std::str::from_utf8(c).ok()?, 16).ok())
        .collect();
    Some(String::from_utf16_lossy(&units))
}

/// The body: UCS-2 hex as the firmware stores it (the old `decodeSms`), else as it is.
pub fn decode_text(raw: &str) -> String {
    match ucs2(raw) {
        Some(t) => t.trim_end_matches('\0').to_owned(),
        None => raw.to_owned(),
    }
}

/// The number: list_after gives it plain ("10086", "+8613…", a sender name) or, when it came out
/// of the vendor's envelope, as UCS-2 hex. A plain all-digit number can look like hex
/// ("861380000000"), so digits only decode when every unit is ASCII ("0031003000…").
pub fn decode_number(raw: &str) -> String {
    let s = raw.trim();
    if let Some(t) = ucs2(s) {
        let digits = s.bytes().all(|b| b.is_ascii_digit());
        let ascii_units = s.as_bytes().chunks(4).all(|c| c.starts_with(b"00"));
        if !t.chars().any(char::is_control) && (!digits || ascii_units) {
            return t;
        }
    }
    s.to_owned()
}

/// "26,05,29,10,56,56,+32" → "2026-05-29 10:56" (device local time, as stored); else as it is.
pub fn format_date(raw: &str) -> String {
    let p: Vec<&str> = raw.split(',').collect();
    if p.len() >= 6
        && p[..5]
            .iter()
            .all(|x| (1..=2).contains(&x.len()) && x.bytes().all(|b| b.is_ascii_digit()))
    {
        let n = |x: &str| x.parse::<u32>().unwrap_or(0);
        return format!(
            "{}-{:02}-{:02} {:02}:{:02}",
            2000 + n(p[0]),
            n(p[1]),
            n(p[2]),
            n(p[3]),
            n(p[4])
        );
    }
    raw.to_owned()
}

fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// One `sms.list_after` item.
pub fn msg(item: &Value) -> Option<Msg> {
    let id = item["id"]
        .as_u64()
        .or_else(|| item["id"].as_str()?.trim().parse().ok())?;
    let store = match &item["mem_store"] {
        Value::Number(n) => match n.as_i64() {
            Some(0) => Some(Store::Sim),
            Some(_) => Some(Store::Device),
            None => None,
        },
        Value::String(s) if s == "sim" => Some(Store::Sim),
        Value::String(_) => Some(Store::Device),
        _ => None,
    };
    Some(Msg {
        id,
        number: decode_number(item["number"].as_str().unwrap_or("")),
        text: decode_text(item["content"].as_str().unwrap_or("")),
        date: format_date(item["date"].as_str().unwrap_or("")),
        tag: text_of(&item["tag"]).unwrap_or_default(),
        store,
    })
}

/// "1;2;3" from a query (digits and `;` only), at most 500 ids like u60d.
pub fn parse_ids(s: &str) -> Vec<u64> {
    let mut v: Vec<u64> = s
        .split(';')
        .filter_map(|x| {
            let x = x.trim();
            (!x.is_empty() && x.len() <= 12 && x.bytes().all(|b| b.is_ascii_digit()))
                .then(|| x.parse().ok())
                .flatten()
        })
        .collect();
    v.sort_unstable();
    v.dedup();
    v.truncate(500);
    v
}

/// The firmware's id list: "1;2;3;".
fn id_list(ids: &[u64]) -> String {
    ids.iter().map(|i| format!("{i};")).collect()
}

// ---- the shared list ----

/// Why a list read has no answer.
#[derive(Clone, Debug, PartialEq)]
pub enum ReadErr {
    Unreachable,
    NoReply,
    /// u60d's own message.
    Refused(String),
}

impl ReadErr {
    pub fn text(&self, l: Lang) -> String {
        match self {
            ReadErr::Unreachable => l
                .pick("can't reach the core service", "连不上核心服务")
                .into(),
            ReadErr::NoReply => l
                .pick("the core service did not answer", "核心服务没有回应")
                .into(),
            ReadErr::Refused(m) => m.clone(),
        }
    }
}

/// The `sms` block's values: when they move, the list is read again.
type Key = (i64, i64, i64);

#[derive(Default)]
pub struct Got {
    /// The newest good list (newest first) and when it came.
    pub ok: Option<(Vec<Msg>, SystemTime)>,
    /// The newest read failed (cleared by a good one).
    pub err: Option<(ReadErr, Instant)>,
    tried: Option<Key>,
    inflight: bool,
    force: bool,
}

static CACHE: Mutex<Got> = Mutex::new(Got {
    ok: None,
    err: None,
    tried: None,
    inflight: false,
    force: false,
});
/// One list read at a time (the executor is serial anyway; this keeps them from queueing).
static READ: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn key(st: &St) -> Key {
    match st.blocks.get("sms").filter(|b| !b.stale) {
        Some(b) => {
            let n = |k: &str| b.data[k].as_i64().unwrap_or(-1);
            (n("unread"), n("max_id"), n("count"))
        }
        None => (-1, -1, -1),
    }
}

/// A `/control` read (no op_id: not a write, u60d does not journal it).
async fn control_read(app: &Shared, action: &str, params: Value) -> Result<Value, ReadErr> {
    let body = json!({"action": action, "params": params, "source": "web"});
    let r = app
        .http
        .post(&app.control_url)
        .timeout(LIST_TIMEOUT)
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
        Ok(v["result"].clone())
    } else {
        Err(ReadErr::Refused(
            v["error"]["message"]
                .as_str()
                .unwrap_or("refused")
                .to_owned(),
        ))
    }
}

/// Every message, newest first: list_after from id 0, 50 at a time.
async fn read_all(app: &Shared) -> Result<Vec<Msg>, ReadErr> {
    let mut out: Vec<Msg> = Vec::new();
    let mut after = 0_u64;
    // 2 stores × u60d's 1000 each
    for _ in 0..40 {
        let v = control_read(
            app,
            "sms.list_after",
            json!({"after_id": after, "limit": 50}),
        )
        .await?;
        let items = v["items"].as_array().cloned().unwrap_or_default();
        let before = after;
        for m in items.iter().filter_map(msg) {
            after = after.max(m.id);
            out.push(m);
        }
        if !v["has_more"].as_bool().unwrap_or(false) || after == before {
            break;
        }
    }
    out.sort_by_key(|m| std::cmp::Reverse(m.id));
    out.dedup_by_key(|m| m.id);
    Ok(out)
}

/// Read the list now (after a write; and the shared background read), store it, kick the tabs.
pub async fn read_now(app: &Shared) -> Result<Vec<Msg>, ReadErr> {
    read(app, false).await
}

async fn read(app: &Shared, background: bool) -> Result<Vec<Msg>, ReadErr> {
    let _one = READ.lock().await;
    let k = app.hub.read(key);
    let r = read_all(app).await;
    {
        let mut c = CACHE.lock().unwrap();
        c.tried = Some(k);
        if background {
            c.inflight = false;
        }
        match &r {
            Ok(l) => {
                c.ok = Some((l.clone(), SystemTime::now()));
                c.err = None;
            }
            Err(e) => c.err = Some((e.clone(), Instant::now())),
        }
    }
    let _ = app.kick.send(PATH);
    r
}

/// Starts the shared read when the `sms` block moved since the last one (or 刷新, or a failure
/// 15 s old) and none is running. The tabs hear about it through the kick.
fn ensure(app: &Shared) {
    let k = app.hub.read(key);
    let mut c = CACHE.lock().unwrap();
    let retry = c.err.as_ref().is_some_and(|(_, at)| at.elapsed() >= RETRY);
    if c.inflight || !(c.force || c.tried != Some(k) || retry) {
        return;
    }
    c.inflight = true;
    c.force = false;
    drop(c);
    let app = app.clone();
    tokio::spawn(async move {
        let _ = read(&app, true).await;
    });
}

fn cached(id: u64) -> Option<Msg> {
    let c = CACHE.lock().unwrap();
    c.ok.as_ref()?.0.iter().find(|m| m.id == id).cloned()
}

// ---- the view ----

/// Used / total of one store from the agent's capacity reply (the old `storeUsage`): used =
/// received + sent + drafts (`sms_<store>_{rev,send,draftbox}_total`; B27 has no
/// `sms_simused_total` and its `sms_nvused_total` read 0), `*used_total` only as the fallback.
pub fn store_usage(cap: &Value, store: &str) -> Option<(i64, i64)> {
    let num = |k: &str| -> Option<i64> {
        match &cap[k] {
            Value::Number(n) => n.as_i64(),
            Value::String(s) => s.trim().parse().ok(),
            _ => None,
        }
    };
    let total = num(&format!("sms_{store}_total")).filter(|t| *t > 0)?;
    let parts: Vec<Option<i64>> = ["rev", "send", "draftbox"]
        .iter()
        .map(|b| num(&format!("sms_{store}_{b}_total")))
        .collect();
    let used = if parts.iter().any(Option::is_some) {
        parts.iter().map(|p| p.unwrap_or(0)).sum()
    } else {
        num(&format!("sms_{store}used_total"))?
    };
    Some((used, total))
}

pub struct Row {
    pub id: u64,
    pub from: String,
    pub unread: bool,
    /// (tone, word) for unread / sent / failed / draft.
    pub word: Option<(&'static str, &'static str)>,
    pub date: String,
    pub store: Option<&'static str>,
    /// "sim" | "dev" | "" (the filter's key).
    pub s: &'static str,
    pub preview: String,
}

pub enum List {
    Loading,
    Failed,
    Rows(Vec<Row>),
}

pub struct View {
    pub l: Lang,
    /// "neutral" | "ok" | "warn" | "bad" | "stale"
    pub tone: &'static str,
    pub state: String,
    pub reason: Option<String>,
    pub capacity: [(&'static str, String); 4],
    /// 「列表停在 hh:mm」 when the list stopped updating.
    pub stopped: Option<String>,
    pub retry: bool,
    pub stale: bool,
    pub list: List,
    /// Every row knows its store: the filter is shown.
    pub stores: bool,
    pub n_sim: usize,
    pub n_dev: usize,
    /// Changes with the rows (the list's `data-init` runs again on each new list).
    pub rev: u64,
}

impl View {
    pub fn symbol(&self) -> &'static str {
        symbol(self.tone)
    }
    /// The list's `data-init`: the filter shown or not, then the counts again.
    pub fn init(&self) -> String {
        format!("$_r = {}; $_st = {}; {RECOUNT}", self.rev, self.stores)
    }
}

pub fn symbol(tone: &str) -> &'static str {
    match tone {
        "bad" => "■",
        "stale" | "warn" => "▲",
        _ => "●",
    }
}

pub fn word(tag: &str, l: Lang) -> Option<(&'static str, &'static str)> {
    Some(match tag {
        "1" => ("ok", l.pick("Unread", "未读")),
        "2" => ("neutral", l.pick("Sent", "已发送")),
        "3" => ("bad", l.pick("Send failed", "发送失败")),
        "4" => ("neutral", l.pick("Draft", "草稿")),
        _ => return None,
    })
}

pub fn store_label(s: Option<Store>, l: Lang) -> Option<&'static str> {
    s.map(|s| match s {
        Store::Sim => "SIM",
        Store::Device => l.pick("Device", "设备"),
    })
}

pub fn unknown(l: Lang) -> &'static str {
    l.pick("Unknown", "未知")
}

fn preview(t: &str) -> String {
    let flat = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > PREVIEW {
        let mut s: String = flat.chars().take(PREVIEW).collect();
        s.push('…');
        s
    } else {
        flat
    }
}

/// `list: false` = the list as loading (the page's own render: rows come with the first push,
/// so a long inbox never weighs on the first screen).
pub fn view(st: &St, got: &Got, cap: Option<&Value>, l: Lang, list: bool) -> View {
    let block = st.blocks.get("sms");
    let block_unread = block
        .filter(|b| !b.stale)
        .and_then(|b| b.data["unread"].as_i64());
    let msgs = got.ok.as_ref().map(|(m, _)| m);
    let unread = block_unread.or(msgs.map(|m| m.iter().filter(|m| m.unread()).count() as i64));
    let sim = cap.and_then(|c| store_usage(c, "sim"));
    let dev = cap.and_then(|c| store_usage(c, "nv"));
    let frac = |f: Option<(i64, i64)>| f.map_or("—".to_owned(), |(u, t)| format!("{u} / {t}"));
    let total = match (sim, dev) {
        (Some(a), Some(b)) => Some((a.0 + b.0, a.1 + b.1)),
        _ => None,
    };
    let capacity = [
        (l.pick("SIM Used", "SIM 已用"), frac(sim)),
        (l.pick("Device Used", "设备已用"), frac(dev)),
        (l.pick("Total Used", "总已用"), frac(total)),
        (
            l.pick("Unread", "未读"),
            match (msgs, unread) {
                (Some(_), Some(n)) => n.to_string(),
                _ => "—".into(),
            },
        ),
    ];
    let full = [("SIM", sim), (l.pick("Device", "设备"), dev)]
        .into_iter()
        .find_map(|(name, f)| {
            f.filter(|(u, t)| *u as f64 >= *t as f64 * 0.9)
                .map(|f| (name, f))
        });
    let link_down = st.link != Link::Live || block.is_some_and(|b| b.stale);
    let failed_now = got.err.is_some();
    let (mut tone, state, reason, retry) = match (msgs, &got.err) {
        (None, Some((e, _))) => (
            "bad",
            l.pick("Can't read messages", "读不到短信").to_owned(),
            Some(e.text(l)),
            true,
        ),
        (None, None) => (
            "neutral",
            l.pick("Reading messages…", "正在读取短信…").to_owned(),
            None,
            false,
        ),
        (Some(_), _) => match full {
            Some((name, (u, t))) => (
                "warn",
                match l {
                    Lang::En => format!("{name} message storage almost full"),
                    Lang::Zh => format!("{name} 短信存储快满了"),
                },
                Some(match l {
                    Lang::En => format!(
                        "{u} of {t} used. When it is full new messages can't be received; delete some old ones."
                    ),
                    Lang::Zh => {
                        format!("已用 {u} / {t}。满了以后收不到新短信，删掉一些旧短信。")
                    }
                }),
                false,
            ),
            None => (
                "ok",
                match (unread.unwrap_or(0), l) {
                    (0, _) => l.pick("No unread messages", "没有未读短信").to_owned(),
                    (n, Lang::En) => format!("{n} unread"),
                    (n, Lang::Zh) => format!("{n} 条未读"),
                },
                None,
                false,
            ),
        },
    };
    let stale = msgs.is_some() && (failed_now || link_down);
    if stale {
        tone = "stale";
    }
    let stopped =
        stale.then(|| crate::agent::stopped(l, "List", "列表", got.ok.as_ref().map(|(_, t)| *t)));
    let (list, stores, n_sim, n_dev, rev) = match msgs {
        _ if !list => (List::Loading, false, 0, 0, 0),
        None if got.err.is_some() => (List::Failed, false, 0, 0, 0),
        None => (List::Loading, false, 0, 0, 0),
        Some(m) => {
            let rows: Vec<Row> = m
                .iter()
                .map(|m| Row {
                    id: m.id,
                    from: if m.number.is_empty() {
                        unknown(l).to_owned()
                    } else {
                        m.number.clone()
                    },
                    unread: m.unread(),
                    word: word(&m.tag, l),
                    date: if m.date.is_empty() {
                        "—".into()
                    } else {
                        m.date.clone()
                    },
                    store: store_label(m.store, l),
                    s: match m.store {
                        Some(Store::Sim) => "sim",
                        Some(Store::Device) => "dev",
                        None => "",
                    },
                    preview: preview(&m.text),
                })
                .collect();
            let stores = !rows.is_empty() && rows.iter().all(|r| !r.s.is_empty());
            let n_sim = rows.iter().filter(|r| r.s == "sim").count();
            let n_dev = rows.iter().filter(|r| r.s == "dev").count();
            // a hash of what a row shows that the checkbox state depends on
            let rev = m.iter().fold(1469598103934665603_u64, |h, m| {
                (h ^ (m.id * 8 + m.tag.bytes().next().unwrap_or(0) as u64))
                    .wrapping_mul(1099511628211)
            }) % 1_000_000_007;
            (List::Rows(rows), stores, n_sim, n_dev, rev)
        }
    };
    View {
        l,
        tone,
        state,
        reason,
        capacity,
        stopped,
        retry,
        stale,
        list,
        stores,
        n_sim,
        n_dev,
        rev,
    }
}

// ---- the page's expressions (constants: only numeric ids ever come from the DOM) ----

/// The selected rows' ids, `;`-joined.
pub const SEL_IDS: &str = "[...document.querySelectorAll('#sms-list input[name=id]:checked')].map(c => c.value).join(';')";
/// The selected unread rows' ids.
pub const UNREAD_IDS: &str = "[...document.querySelectorAll('#sms-list input[name=id][data-u]:checked')].map(c => c.value).join(';')";
/// Counts the selection again and sets 全选 to match the rows the filter shows.
pub const RECOUNT: &str = "$_n = document.querySelectorAll('#sms-list input[name=id]:checked').length; $_u = document.querySelectorAll('#sms-list input[name=id][data-u]:checked').length; (a => a && (a.checked = $_n > 0 && $_n === document.querySelectorAll('#sms-list .sm-row' + ($_f === 'all' ? '' : '[data-s=' + $_f + ']')).length))(document.getElementById('sms-all'))";
/// Clears the selection (a filter change, a done write).
pub const CLEAR: &str = "document.querySelectorAll('#sms-list input[type=checkbox]').forEach(c => c.checked = false); $_n = 0; $_u = 0";
/// 全选: every row the filter shows takes 全选's state.
pub const ALL: &str = "document.querySelectorAll('#sms-list .sm-row' + ($_f === 'all' ? '' : '[data-s=' + $_f + ']') + ' input[name=id]').forEach(c => c.checked = el.checked)";

// ---- templates ----

#[derive(Template)]
#[template(path = "sms_leaves.html")]
struct Leaves<'a> {
    v: &'a View,
}

#[derive(Template)]
#[template(path = "sms.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    leaves: Vec<(String, String)>,
    read_op: String,
    del_op: String,
}

impl Page {
    fn leaf(&self, id: &str) -> &str {
        self.leaves
            .iter()
            .find(|(k, _)| k == id)
            .map_or("", |(_, h)| h.as_str())
    }
    fn filters(&self) -> [(&'static str, &'static str); 3] {
        [
            ("all", self.l.pick("All", "全部")),
            ("sim", "SIM"),
            ("dev", self.l.pick("Device", "设备")),
        ]
    }
    fn clear(&self) -> &'static str {
        CLEAR
    }
    fn recount(&self) -> &'static str {
        RECOUNT
    }
    fn read_js(&self) -> String {
        format!("@post('/sms/read?ids=' + {UNREAD_IDS}, {{retryMaxCount: 0}})")
    }
    fn del_js(&self) -> String {
        format!("@post('/sms/delete/ask?ids=' + {SEL_IDS})")
    }
    fn read_label(&self) -> &'static str {
        self.l
            .pick("'Mark Read (' + $_u + ')'", "'标为已读 (' + $_u + ')'")
    }
    fn del_label(&self) -> &'static str {
        self.l
            .pick("'Delete (' + $_n + ')'", "'删除 (' + $_n + ')'")
    }
}

/// The opened message (`sms_view.html`): replaces `#sms-view`, a fresh `<dialog>` each time.
#[derive(Template)]
#[template(path = "sms_view.html")]
struct ViewDialog<'a> {
    l: Lang,
    from: &'a str,
    date: &'a str,
    store: Option<&'static str>,
    lines: Vec<&'a str>,
    mark: String,
}

/// The dialog's status mark (`#sms-view-mark`): its tag, or how its mark-read went.
#[derive(Template)]
#[template(
    source = "<span id=\"sms-view-mark\" class=\"nd-mark nd-mark--{{ tone }}\"{% if alert %} role=\"alert\"{% else %} role=\"status\"{% endif %}><span class=\"nd-mark__sym\" aria-hidden=\"true\">{{ crate::sms::symbol(tone) }}</span><span>{{ text }}</span></span>",
    ext = "html"
)]
struct Mark<'a> {
    tone: &'a str,
    text: &'a str,
    alert: bool,
}

fn mark(tone: &str, text: &str, alert: bool) -> String {
    Mark { tone, text, alert }.render().unwrap()
}

/// The mark of a message as it is (no write going on).
fn tag_mark(m: &Msg, l: Lang) -> String {
    match word(&m.tag, l) {
        Some((tone, w)) => mark(tone, w, false),
        None => mark("ok", l.pick("Read", "已读"), false),
    }
}

// ---- the stream ----

pub struct Poll {
    who: Caller,
    l: Lang,
    cap: Option<Value>,
}

impl Poll {
    fn leaves(&self, app: &Shared, list: bool) -> Vec<(String, String)> {
        app.hub.read(|st| {
            let c = CACHE.lock().unwrap();
            let v = view(st, &c, self.cap.as_ref(), self.l, list);
            split_leaves(&Leaves { v: &v }.render().unwrap())
        })
    }
}

impl Polled for Poll {
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        ensure(app);
        // keep the last answer when one fails (the line shows "—" only until the first)
        if let Ok(v) = crate::agent::get(app, &self.who, "/api/sms/capacity").await {
            self.cap = Some(v);
        }
        (self.leaves(app, true), CAP_EVERY)
    }
    fn watches(&self, block: &str) -> bool {
        block == "sms"
    }
    fn render(&mut self, app: &Shared) -> Option<Vec<(String, String)>> {
        ensure(app);
        Some(self.leaves(app, true))
    }
}

fn read_area(l: Lang, phase: &write::Phase) -> String {
    write::Area {
        id: "sms-read",
        l,
        remote: crate::topbar::Via::Local,
        set: "/sms/read",
        cancel: "/sms/read/cancel",
        phase,
    }
    .html()
}

fn del_area(l: Lang, set: &str, phase: &write::Phase) -> String {
    write::Area {
        id: "sms-del",
        l,
        remote: crate::topbar::Via::Local,
        set,
        cancel: "/sms/delete/cancel",
        phase,
    }
    .html()
}

pub async fn page(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let p = Poll {
        who: Caller::of(&app, &req),
        l: ctx.lang,
        cap: None,
    };
    let shell = app.hub.read(|st| {
        crate::shell::Shell::new(&headers, PATH).live(
            st,
            &ctx,
            Some(format!("{PATH}/events?g={}", st.generation)),
        )
    });
    let html = Page {
        shell,
        l: ctx.lang,
        leaves: p.leaves(&app, false),
        read_op: read_area(ctx.lang, &write::Phase::Idle),
        del_op: del_area(ctx.lang, "/sms/delete", &write::Phase::Idle),
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
        who: Caller::of(&app, &req),
        l: ctx.lang,
        cap: None,
    };
    crate::agent::stream(app, ctx, q.g, PATH, p)
}

/// 刷新 / 重试: read the list now, and every tab asks the agent for capacity again.
pub async fn refresh(State(app): State<Shared>) -> axum::http::StatusCode {
    app.hub.retry();
    CACHE.lock().unwrap().force = true;
    ensure(&app);
    let _ = app.kick.send(PATH);
    axum::http::StatusCode::NO_CONTENT
}

fn one(html: String) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(futures_util::stream::once(async move {
        Ok(crate::pages::patch(&html))
    }))
}

use std::convert::Infallible;

#[derive(Deserialize, Default)]
pub struct Ids {
    #[serde(default)]
    ids: String,
}

#[derive(Deserialize, Default)]
pub struct One {
    #[serde(default)]
    id: u64,
}

/// Has every one of `ids` stopped being unread (read, or gone) in this list?
fn all_read(list: &[Msg], ids: &[u64]) -> bool {
    ids.iter().all(|id| {
        list.iter()
            .find(|m| m.id == *id)
            .is_none_or(|m| !m.unread())
    })
}

/// Sends `sms.mark_read` for `ids` and reads the list until they are read.
async fn mark_read(app: &Shared, ids: &[u64], l: Lang) -> write::Outcome {
    mark_read_within(app, ids, l, write::READBACK).await
}

async fn mark_read_within(app: &Shared, ids: &[u64], l: Lang, wait: Duration) -> write::Outcome {
    let w = write::Write {
        action: "sms.mark_read",
        params: json!({"ids": id_list(ids), "tag": 0}),
        readback: None,
        applied: String::new(),
        timeout: write::READBACK,
        typed: None,
        request_timeout: None,
    };
    let sent = match write::send(&app.http, &app.control_url, &w, l).await {
        Ok(s) => s,
        Err(o) => return o,
    };
    let ok = write::read_back(
        &app.hub,
        wait,
        Duration::from_millis(1500),
        Duration::from_secs(4),
        move || async move { read_now(app).await.is_ok_and(|list| all_read(&list, ids)) },
    )
    .await;
    if ok {
        write::Outcome::Applied(match l {
            Lang::En => format!("Marked {} as read", ids.len()),
            Lang::Zh => format!("已把 {} 条标为已读", ids.len()),
        })
    } else {
        sent.not_read()
    }
}

/// 标为已读 (tier 1): the selected unread rows.
pub async fn read_set(
    State(app): State<Shared>,
    Query(q): Query<Ids>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let l = Lang::from_headers(&headers);
    let ids = parse_ids(&q.ids);
    let pending = read_area(
        l,
        &write::Phase::Pending(l.pick("Marking as read…", "正在标为已读…").into()),
    );
    write::respond(pending, async move {
        if ids.is_empty() {
            return read_area(l, &write::Phase::Idle);
        }
        let o = mark_read(&app, &ids, l).await;
        let done = matches!(o, write::Outcome::Applied(_) | write::Outcome::Accepted);
        let mut html = read_area(l, &write::Phase::Done(o));
        if done {
            html.push('\n');
            html.push_str(&reset());
        }
        html
    })
}

/// The selection cleared after a write went through (`#sms-reset`, its `data-init` differs each
/// time so it runs again).
fn reset() -> String {
    format!(
        "<span id=\"sms-reset\" hidden data-init=\"{}; $_x = '{}'\"></span>",
        CLEAR.replace('\'', "&#39;"),
        write::next_op_id()
    )
}

pub async fn read_cancel(
    Query(q): Query<write::CancelQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    one(read_area(
        Lang::from_headers(&headers),
        &write::cancel_phase(&q),
    ))
}

/// The senders of `ids` for the delete dialog: up to three, then "…".
fn senders(list: &[Msg], ids: &[u64], l: Lang) -> String {
    let mut nums: Vec<String> = Vec::new();
    for id in ids {
        let n = list
            .iter()
            .find(|m| m.id == *id)
            .map(|m| m.number.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| unknown(l).to_owned());
        if !nums.contains(&n) {
            nums.push(n);
        }
    }
    let sep = l.pick(", ", "、");
    if nums.len() > 3 {
        format!("{} …", nums[..3].join(sep))
    } else {
        nums.join(sep)
    }
}

fn del_confirm(l: Lang, n: usize, from: &str) -> write::Confirm {
    let (title, what, action) = match l {
        Lang::En => (
            format!("Delete {n} message(s)?"),
            format!("{n} message(s) from {from} are deleted. This can't be undone."),
            format!("Delete ({n})"),
        ),
        Lang::Zh => (
            format!("删除 {n} 条短信？"),
            format!("来自 {from} 的 {n} 条短信会被删除，删除后没法恢复。"),
            format!("删除 ({n})"),
        ),
    };
    write::Confirm {
        tier: write::Tier::Dialog,
        title,
        what,
        downtime: None,
        recovery: None,
        action,
        cuts_uplink: false,
        word: None,
        danger: true,
        revert: crate::write::Revert::No,
    }
}

/// 删除 (N): the tier-3 dialog. The ids it names are the ids its confirm sends.
pub async fn delete_ask(Query(q): Query<Ids>, headers: HeaderMap) -> impl IntoResponse {
    let l = Lang::from_headers(&headers);
    let ids = parse_ids(&q.ids);
    if ids.is_empty() {
        return one(del_area(l, "/sms/delete", &write::Phase::Idle));
    }
    let list = cached_list();
    let set = format!(
        "/sms/delete?ids={}",
        ids.iter().map(u64::to_string).collect::<Vec<_>>().join(";")
    );
    one(del_area(
        l,
        &set,
        &write::Phase::Ask(del_confirm(l, ids.len(), &senders(&list, &ids, l))),
    ))
}

pub async fn delete_cancel(
    Query(q): Query<write::CancelQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    one(del_area(
        Lang::from_headers(&headers),
        "/sms/delete",
        &write::cancel_phase(&q),
    ))
}

fn present(list: &[Msg], ids: &[u64]) -> Vec<u64> {
    ids.iter()
        .copied()
        .filter(|id| list.iter().any(|m| m.id == *id))
        .collect()
}

/// `sms.delete`, then `sms.db_delete` for the rows it left (the agent's order), each read back
/// by presence in the list.
async fn delete(app: &Shared, ids: &[u64], l: Lang) -> write::Outcome {
    let w = |action: &'static str, ids: &[u64]| write::Write {
        action,
        params: json!({"ids": id_list(ids)}),
        readback: None,
        applied: String::new(),
        timeout: write::READBACK,
        typed: None,
        request_timeout: None,
    };
    let first = match write::send(&app.http, &app.control_url, &w("sms.delete", ids), l).await {
        Ok(s) => s,
        Err(o) => return o,
    };
    let survivors = match read_now(app).await {
        Ok(list) => present(&list, ids),
        // can't see: say what the send said
        Err(_) => return first.not_read(),
    };
    let applied = || {
        write::Outcome::Applied(match l {
            Lang::En => format!("Deleted {} message(s)", ids.len()),
            Lang::Zh => format!("已删除 {} 条短信", ids.len()),
        })
    };
    if survivors.is_empty() {
        return applied();
    }
    let second = match write::send(
        &app.http,
        &app.control_url,
        &w("sms.db_delete", &survivors),
        l,
    )
    .await
    {
        Ok(s) => s,
        Err(o) => return partial(l, ids.len() - survivors.len(), survivors.len()).unwrap_or(o),
    };
    let sv = &survivors;
    let gone = write::read_back(
        &app.hub,
        write::READBACK,
        Duration::from_millis(1500),
        Duration::from_secs(4),
        move || async move {
            read_now(app)
                .await
                .is_ok_and(|list| present(&list, sv).is_empty())
        },
    )
    .await;
    if gone {
        return applied();
    }
    let left = present(&cached_list(), &survivors).len();
    partial(l, ids.len() - left, left).unwrap_or(second.not_read())
}

fn cached_list() -> Vec<Msg> {
    CACHE
        .lock()
        .unwrap()
        .ok
        .as_ref()
        .map(|(m, _)| m.clone())
        .unwrap_or_default()
}

/// Some deleted, some still listed.
fn partial(l: Lang, done: usize, left: usize) -> Option<write::Outcome> {
    (done > 0).then(|| {
        write::Outcome::NotYet(match l {
            Lang::En => format!("Partly applied: {done} deleted, {left} still listed"),
            Lang::Zh => format!("部分生效：删掉了 {done} 条，还有 {left} 条没删掉"),
        })
    })
}

/// The dialog's confirm (tier 3).
pub async fn delete_set(
    State(app): State<Shared>,
    Query(q): Query<Ids>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let l = Lang::from_headers(&headers);
    let ids = parse_ids(&q.ids);
    let pending = del_area(
        l,
        "/sms/delete",
        &write::Phase::Pending(l.pick("Sending…", "提交中…").into()),
    );
    write::respond(pending, async move {
        if ids.is_empty() {
            return del_area(l, "/sms/delete", &write::Phase::Idle);
        }
        let o = delete(&app, &ids, l).await;
        let done = matches!(o, write::Outcome::Applied(_));
        let mut html = del_area(l, "/sms/delete", &write::Phase::Done(o));
        if done {
            html.push('\n');
            html.push_str(&reset());
        }
        html
    })
}

fn view_html(m: &Msg, l: Lang, mark: String) -> String {
    ViewDialog {
        l,
        from: if m.number.is_empty() {
            unknown(l)
        } else {
            &m.number
        },
        date: if m.date.is_empty() { "—" } else { &m.date },
        store: store_label(m.store, l),
        lines: m.text.lines().collect(),
        mark,
    }
    .render()
    .unwrap()
}

/// A row opened: the dialog (a fresh one replaces `#sms-view`); an unread message is marked read
/// (tier 1, that one id) and the dialog's mark follows the write. A failure says so in the
/// dialog with the old page's sentence.
pub async fn open(
    State(app): State<Shared>,
    Query(q): Query<One>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let l = Lang::from_headers(&headers);
    let m = cached(q.id);
    Sse::new(async_stream(move |tx| async move {
        let Some(m) = m else {
            return;
        };
        let unread = m.unread();
        let first = view_html(
            &m,
            l,
            if unread {
                mark(
                    "neutral",
                    l.pick("Marking as read…", "正在标为已读…"),
                    false,
                )
            } else {
                tag_mark(&m, l)
            },
        );
        let ev = Event::default()
            .event("datastar-patch-elements")
            .data(format!("mode replace\n{}", crate::sse::patch_data(&first)));
        if tx.send(ev).await.is_err() || !unread {
            return;
        }
        let o = mark_read(&app, &[m.id], l).await;
        let html = match &o {
            write::Outcome::Applied(_) => mark("ok", l.pick("Read", "已读"), false),
            write::Outcome::Accepted => mark("neutral", &o.text(l), false),
            _ => {
                let e = o.text(l);
                let e = e
                    .strip_prefix(l.pick("Not applied: ", "没有生效："))
                    .unwrap_or(&e);
                let t = match l {
                    Lang::En => format!(
                        "Couldn't mark the message as read: {e}. Use “Mark Read” to try again."
                    ),
                    Lang::Zh => {
                        format!("没能把这条短信标为已读：{e}。可以勾选后点「标为已读」再试。")
                    }
                };
                mark("bad", &t, true)
            }
        };
        let _ = tx.send(crate::pages::patch(&html)).await;
    }))
}

#[cfg(test)]
mod tests;
