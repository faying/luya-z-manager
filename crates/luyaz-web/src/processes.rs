//! /tools/processes (进程监控), the old `tools/processes/page.tsx`: the process list, the ones on
//! the agent's kill list, and the two kill buttons. All data is the agent's `/api/system/top`.
//!
//! - Polled every 3 s like the old page, only while a tab's stream is open (`agent::stream` ends
//!   with the tab). `cpu_pct` is a delta since the previous `/api/system/top` call from anyone, so
//!   tabs share one answer (`top_now`, reused for 2 s): a second tab would otherwise shrink every
//!   delta. The first answer after the agent starts has all CPU at 0 ("CPU % shows next refresh").
//! - The full list is 50 fixed slots (`pc-r0`…`pc-r49`, one row `<div>` each (a `<tr>` patch is dropped by the browser outside a table), in the page's sort
//!   order): a push sends only the slots whose text changed, an empty slot is `hidden`. The sort
//!   (CPU / memory / name / PID, up or down) is a link back to the page (`?sort=&dir=`); the
//!   stream carries it, so each tab sorts its own list.
//! - Kill one (`{pids:[pid]}`) and kill all listed (`{all:true}`) are tier 3 (dialog). No readback
//!   (procd may start the process again at once): the agent's `killed` / `skipped` lists are the
//!   result, by name and PID (`Outcome::Ended`). Kill all lists the rows this tab has plus
//!   "N more" (`bloat_count` counts all of /proc, `top` only the 50 busiest).

use std::time::{Duration, Instant, SystemTime};

use askama::Template;
use axum::extract::{Query, State};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{Value, json};
use std::convert::Infallible;

use crate::topbar::Via;
use crate::agent::{Caller, Fail, Polled};
use crate::i18n::Lang;
use crate::pages::{Shared, split_leaves};
use crate::write;

const PATH: &str = "/tools/processes";
const API_TOP: &str = "/api/system/top";
const API_KILL: &str = "/api/system/kill-bloat";
const EVERY: Duration = Duration::from_secs(3);
const REUSE: Duration = Duration::from_secs(2);
/// `top` returns the 50 busiest.
const SLOTS: usize = 50;

/// Mirror of the agent's BLOAT_DAEMONS (system.rs), shown in the kill-all confirm.
const BLOAT_NAMES: [&str; 10] = [
    "zte_topsw_tr069",
    "zte_topsw_tr069_sub",
    "zte_mqtt_sdk_st",
    "zte_topsw_diag",
    "zte_topsw_samba",
    "zte_topsw_nfc",
    "zte_topsw_get_brand",
    "zte_topsw_jwxk_query",
    "zte-topsw-tunnel",
    "zte_dua",
];

pub fn routes() -> Router<Shared> {
    Router::new()
        .route(PATH, get(page))
        .route("/tools/processes/events", get(events))
        .route("/tools/processes/refresh", post(refresh))
        .route("/tools/processes/kill/ask", post(kill_ask))
        .route("/tools/processes/kill/set", post(kill_set))
        .route("/tools/processes/kill/cancel", post(kill_cancel))
}

// ---- the data ----

#[derive(Debug, Clone, PartialEq)]
pub struct Proc {
    pub pid: u64,
    pub name: String,
    pub cpu: f64,
    pub rss_kb: u64,
    pub state: String,
    pub bloat: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Top {
    pub procs: Vec<Proc>,
    pub total: u64,
    pub bloat_count: u64,
    pub bloat_cpu: f64,
    pub bloat_rss_kb: u64,
}

pub fn parse(v: &Value) -> Option<Top> {
    let a = v["processes"].as_array()?;
    let procs: Vec<Proc> = a
        .iter()
        .filter_map(|p| {
            Some(Proc {
                pid: p["pid"].as_u64()?,
                name: p["name"].as_str()?.to_owned(),
                cpu: p["cpu_pct"].as_f64().unwrap_or(0.0),
                rss_kb: p["rss_kb"].as_u64().unwrap_or(0),
                state: p["state"].as_str().unwrap_or("").to_owned(),
                bloat: p["is_bloat"].as_bool().unwrap_or(false),
            })
        })
        .collect();
    let listed = procs.iter().filter(|p| p.bloat).count() as u64;
    Some(Top {
        total: v["total_count"].as_u64().unwrap_or(procs.len() as u64),
        bloat_count: v["bloat_count"].as_u64().unwrap_or(listed),
        bloat_cpu: v["bloat_cpu_pct"].as_f64().unwrap_or(0.0),
        bloat_rss_kb: v["bloat_rss_kb"].as_u64().unwrap_or(0),
        procs,
    })
}

/// One answer for all tabs: `cpu_pct` is a delta since the last call by anyone.
type Cached = tokio::sync::Mutex<Option<(Instant, Value)>>;

fn cache() -> &'static Cached {
    static C: std::sync::OnceLock<Cached> = std::sync::OnceLock::new();
    C.get_or_init(|| tokio::sync::Mutex::new(None))
}

async fn top_now(app: &Shared, who: &Caller) -> Result<Value, Fail> {
    let mut g = cache().lock().await;
    if let Some((at, v)) = g.as_ref()
        && at.elapsed() < REUSE
    {
        return Ok(v.clone());
    }
    let v = crate::agent::get(app, who, API_TOP).await?;
    *g = Some((Instant::now(), v.clone()));
    Ok(v)
}

/// After a kill the next read must be a fresh one.
async fn forget(app: &Shared) {
    *cache().lock().await = None;
    let _ = app.kick.send(PATH);
}

#[derive(Default)]
pub struct Got {
    pub top: Option<Top>,
    pub err: Option<Fail>,
    pub at: Option<SystemTime>,
}

// ---- the sort ----

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Key {
    Cpu,
    Mem,
    Name,
    Pid,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Sort {
    pub key: Key,
    pub desc: bool,
}

#[derive(Deserialize, Default, Clone)]
pub struct SortQ {
    #[serde(default)]
    sort: String,
    #[serde(default)]
    dir: String,
}

impl Key {
    fn code(self) -> &'static str {
        match self {
            Key::Cpu => "cpu",
            Key::Mem => "mem",
            Key::Name => "name",
            Key::Pid => "pid",
        }
    }
    fn default_desc(self) -> bool {
        matches!(self, Key::Cpu | Key::Mem)
    }
}

impl Sort {
    pub fn of(q: &SortQ) -> Sort {
        let key = match q.sort.as_str() {
            "mem" => Key::Mem,
            "name" => Key::Name,
            "pid" => Key::Pid,
            _ => Key::Cpu,
        };
        let desc = match q.dir.as_str() {
            "desc" => true,
            "asc" => false,
            _ => key.default_desc(),
        };
        Sort { key, desc }
    }
    fn query(self) -> String {
        format!("sort={}&dir={}", self.key.code(), if self.desc { "desc" } else { "asc" })
    }
    fn apply(self, v: &mut [Proc]) {
        v.sort_by(|a, b| {
            let c = match self.key {
                Key::Cpu => a.cpu.total_cmp(&b.cpu),
                Key::Mem => a.rss_kb.cmp(&b.rss_kb),
                Key::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                Key::Pid => a.pid.cmp(&b.pid),
            };
            (if self.desc { c.reverse() } else { c }).then(a.pid.cmp(&b.pid))
        });
    }
}

// ---- the view ----

fn kb(n: u64) -> String {
    if n < 1024 {
        format!("{n} KB")
    } else {
        format!("{:.1} MB", n as f64 / 1024.0)
    }
}

fn target(p: &Proc) -> String {
    format!("{} ({})", p.name, p.pid)
}

fn state_word(s: &str, l: Lang) -> String {
    match s {
        "running" => l.pick("running", "运行中").into(),
        "sleeping" => l.pick("sleeping", "休眠").into(),
        "disk" => l.pick("disk wait", "等磁盘").into(),
        "zombie" => l.pick("zombie", "僵尸").into(),
        "stopped" => l.pick("stopped", "已停止").into(),
        "" => "—".into(),
        o => o.into(),
    }
}

pub struct BRow {
    pub pid: u64,
    pub name: String,
    pub sub: String,
    pub aria: String,
}

pub struct TRow {
    pub name: String,
    pub pid: u64,
    pub cpu: String,
    pub rss: String,
    pub state: String,
    pub bloat: bool,
}

pub struct View {
    pub l: Lang,
    pub tone: &'static str,
    pub state: String,
    pub reason: Option<String>,
    pub meta: Option<String>,
    pub retry: bool,
    pub stale: bool,
    /// The kill buttons wait for a list that is current.
    pub locked: bool,
    pub bloat_n: u64,
    pub bloat_rows: Vec<BRow>,
    /// Why the group is empty.
    pub bloat_note: Option<String>,
    pub cpu_hint: bool,
    pub empty: Option<String>,
    pub slots: Vec<Option<TRow>>,
}

impl View {
    pub fn symbol(&self) -> &'static str {
        match self.tone {
            "ok" => "●",
            "bad" => "■",
            "stale" => "▲",
            _ => "●",
        }
    }
    pub fn slot_ids(&self) -> Vec<usize> {
        (0..self.slots.len()).collect()
    }
}

pub fn view(got: &Got, sort: Sort, l: Lang) -> View {
    let top = got.top.as_ref();
    let stale = top.is_some() && got.err.is_some();
    let (mut tone, state, reason) = match (top, &got.err) {
        (None, Some(e)) => (
            "bad",
            l.pick("Couldn't read the process list", "读不到进程列表").to_owned(),
            Some(e.text(l)),
        ),
        (None, None) => (
            "neutral",
            l.pick("Reading processes…", "正在读取进程…").to_owned(),
            None,
        ),
        (Some(t), _) => (
            "ok",
            match l {
                Lang::En => format!("{} processes running", t.total),
                Lang::Zh => format!("{} 个进程在运行", t.total),
            },
            Some(if t.bloat_count > 0 {
                match l {
                    Lang::En => format!(
                        "{} of them can be killed: {:.1}% CPU, {} memory.",
                        t.bloat_count,
                        t.bloat_cpu,
                        kb(t.bloat_rss_kb)
                    ),
                    Lang::Zh => format!(
                        "其中 {} 个可以结束：占 CPU {:.1}%，内存 {}。",
                        t.bloat_count,
                        t.bloat_cpu,
                        kb(t.bloat_rss_kb)
                    ),
                }
            } else {
                l.pick(
                    "None of the processes on the agent's list is running.",
                    "后台名单里的进程一个都没在运行。",
                )
                .to_owned()
            }),
        ),
    };
    if stale {
        tone = "stale";
    }
    let procs = top.map(|t| t.procs.as_slice()).unwrap_or(&[]);
    // cpu_pct is a delta since the previous call: all zero on the first sample
    let known = procs.iter().any(|p| p.cpu > 0.0);
    let cpu = |p: &Proc| {
        if known {
            format!("{:.1}", p.cpu)
        } else {
            "—".into()
        }
    };
    let bloat_n = top.map_or(0, |t| t.bloat_count);
    let bloat_rows: Vec<BRow> = procs
        .iter()
        .filter(|p| p.bloat)
        .map(|p| BRow {
            pid: p.pid,
            name: p.name.clone(),
            sub: format!(
                "PID {} · {} · {}",
                p.pid,
                if known { format!("{}%", cpu(p)) } else { "—".into() },
                kb(p.rss_kb)
            ),
            aria: match l {
                Lang::En => format!("Kill {} ({})", p.name, p.pid),
                Lang::Zh => format!("结束 {}（{}）", p.name, p.pid),
            },
        })
        .collect();
    let bloat_note = match top {
        Some(_) if bloat_rows.is_empty() => Some(if bloat_n > 0 {
            match l {
                Lang::En => format!("{bloat_n} listed process(es) are running but not among the 50 busiest shown here. Use the button above to end them."),
                Lang::Zh => format!("名单里有 {bloat_n} 个进程在运行，但不在这里显示的最忙 50 个里。用上面的按钮结束它们。"),
            }
        } else {
            l.pick("Nothing to kill right now.", "现在没有可结束的进程。").to_owned()
        }),
        _ => None,
    };
    let mut sorted = procs.to_vec();
    sort.apply(&mut sorted);
    let mut slots: Vec<Option<TRow>> = sorted
        .iter()
        .take(SLOTS)
        .map(|p| {
            Some(TRow {
                name: p.name.clone(),
                pid: p.pid,
                cpu: cpu(p),
                rss: kb(p.rss_kb),
                state: state_word(&p.state, l),
                bloat: p.bloat,
            })
        })
        .collect();
    slots.resize_with(SLOTS, || None);
    let empty = match (top, &got.err) {
        (None, Some(_)) => Some(l.pick("Couldn't read the process list", "读不到进程列表").to_owned()),
        (None, None) => Some(l.pick("Loading…", "正在加载…").to_owned()),
        (Some(t), _) if t.procs.is_empty() => Some(l.pick("No processes found", "没有找到进程").to_owned()),
        _ => None,
    };
    View {
        l,
        tone,
        state,
        reason,
        meta: stale.then(|| crate::agent::stopped(l, "The list", "列表", got.at)),
        retry: got.err.is_some(),
        stale,
        locked: top.is_none() || stale,
        bloat_n,
        bloat_rows,
        bloat_note,
        cpu_hint: top.is_some() && !known,
        empty,
        slots,
    }
}

#[derive(Template)]
#[template(path = "processes_leaves.html")]
struct Leaves<'a> {
    v: &'a View,
}

pub struct Poll {
    who: Caller,
    l: Lang,
    sort: Sort,
    got: Got,
}

impl Poll {
    async fn fetch(&mut self, app: &Shared) {
        match top_now(app, &self.who).await {
            Ok(v) => match parse(&v) {
                Some(t) => {
                    self.got.top = Some(t);
                    self.got.err = None;
                    self.got.at = Some(SystemTime::now());
                }
                None => {
                    self.got.err = Some(Fail::Refused(200, "unexpected reply".into(), "unexpected reply".into()))
                }
            },
            Err(e) => self.got.err = Some(e),
        }
    }
    fn leaves(&self) -> Vec<(String, String)> {
        let v = view(&self.got, self.sort, self.l);
        split_leaves(&Leaves { v: &v }.render().unwrap())
    }
}

impl Polled for Poll {
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        self.fetch(app).await;
        (self.leaves(), EVERY)
    }
}

// ---- the page ----

#[derive(Template)]
#[template(path = "processes.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    leaves: Vec<(String, String)>,
    /// The slots, whole.
    rows: String,
    op: String,
    sort: Sort,
}

impl Page {
    fn leaf(&self, id: &str) -> &str {
        self.leaves
            .iter()
            .find(|(k, _)| k == id)
            .map_or("", |(_, h)| h.as_str())
    }
    /// (label, query, selected) of the sort choices.
    fn keys(&self) -> Vec<(&'static str, String, bool)> {
        let l = self.l;
        [
            (Key::Cpu, "CPU"),
            (Key::Mem, l.pick("Memory", "内存")),
            (Key::Name, l.pick("Name", "名称")),
            (Key::Pid, "PID"),
        ]
        .into_iter()
        .map(|(k, label)| {
            let s = Sort { key: k, desc: k.default_desc() };
            (label, s.query(), k == self.sort.key)
        })
        .collect()
    }
    fn flip(&self) -> String {
        Sort { desc: !self.sort.desc, ..self.sort }.query()
    }
    fn aria_sort(&self, k: Key) -> &'static str {
        if self.sort.key != k {
            "none"
        } else if self.sort.desc {
            "descending"
        } else {
            "ascending"
        }
    }
}

fn area(l: Lang, remote: Via, set: &str, phase: &write::Phase) -> String {
    write::Area {
        id: "pc-kill",
        l,
        remote,
        set,
        cancel: "/tools/processes/kill/cancel",
        phase,
    }
    .html()
}

pub async fn page(State(app): State<Shared>, Query(q): Query<SortQ>, req: axum::extract::Request) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let sort = Sort::of(&q);
    let mut p = Poll {
        who: Caller::of(&app, &req),
        l: ctx.lang,
        sort,
        got: Got::default(),
    };
    p.fetch(&app).await;
    let shell = app.hub.read(|st| {
        crate::shell::Shell::new(&headers, PATH).live(
            st,
            &ctx,
            Some(format!("{PATH}/events?g={}&{}", st.generation, sort.query())),
        )
    });
    // the table's rows come with the stream's first push (50 of them are ~4 KB of the first load)
    let mut leaves = p.leaves();
    for (id, h) in &mut leaves {
        if id.strip_prefix("pc-r").is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit())) {
            *h = format!("<div id=\"{id}\" role=\"row\" class=\"pc-tr\" hidden></div>");
        }
    }
    let rows = leaves
        .iter()
        .filter(|(k, _)| k.starts_with("pc-r") && k[4..].bytes().all(|b| b.is_ascii_digit()))
        .map(|(_, h)| h.as_str())
        .collect::<Vec<_>>()
        .join("");
    let html = Page {
        shell,
        l: ctx.lang,
        leaves,
        rows,
        op: area(ctx.lang, ctx.remote, "", &write::Phase::Idle),
        sort,
    }
    .render()
    .unwrap();
    crate::assets::maybe_gzip(&headers, "text/html; charset=utf-8", html.into_bytes(), "no-store")
}

#[derive(Deserialize, Default)]
pub struct EventsQ {
    #[serde(default)]
    g: u64,
    #[serde(default)]
    sort: String,
    #[serde(default)]
    dir: String,
}

pub async fn events(
    State(app): State<Shared>,
    Query(q): Query<EventsQ>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let p = Poll {
        who: Caller::of(&app, &req),
        l: ctx.lang,
        sort: Sort::of(&SortQ { sort: q.sort, dir: q.dir }),
        got: Got::default(),
    };
    crate::agent::stream(app, ctx, q.g, PATH, p)
}

pub async fn refresh(State(app): State<Shared>) -> axum::http::StatusCode {
    let _ = app.kick.send(PATH);
    axum::http::StatusCode::NO_CONTENT
}

// ---- killing ----

fn one(html: String) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(futures_util::stream::once(async move {
        Ok(crate::pages::patch(&html))
    }))
}

#[derive(Deserialize, Default)]
pub struct KillQ {
    #[serde(default)]
    pid: Option<u64>,
    #[serde(default)]
    all: bool,
}

/// What the confirm says, from the list as this server last read it.
pub fn confirm(top: Option<&Top>, q: &KillQ, remote: Via, l: Lang) -> Result<write::Confirm, write::Outcome> {
    let _ = remote;
    let downtime = l
        .pick(
            "Nothing goes offline; the process stops at once.",
            "不会断网；进程立刻停止。",
        )
        .to_owned();
    let recovery = l
        .pick(
            "The system may start it again by itself; otherwise restarting the device brings it back.",
            "系统可能会自己再拉起它；否则重启设备就会恢复。",
        )
        .to_owned();
    if q.all {
        let t = top.ok_or_else(|| {
            write::Outcome::NotYet(
                l.pick("The list isn't loaded yet; try again in a moment", "还没读到进程列表，稍后再试")
                    .into(),
            )
        })?;
        let rows: Vec<String> = t.procs.iter().filter(|p| p.bloat).map(target).collect();
        let more = (t.bloat_count as usize).saturating_sub(rows.len());
        let mut what = match l {
            Lang::En => format!(
                "Sends SIGKILL to every running process on the agent's list: {}.",
                if rows.is_empty() { "—".into() } else { rows.join(", ") }
            ),
            Lang::Zh => format!(
                "向后台名单里每个在运行的进程发 SIGKILL：{}。",
                if rows.is_empty() { "—".into() } else { rows.join("、") }
            ),
        };
        if more > 0 {
            what.push_str(&match l {
                Lang::En => format!(" …and {more} more not among the 50 busiest shown here."),
                Lang::Zh => format!(" 还有 {more} 个不在这里显示的最忙 50 个里。"),
            });
        }
        what.push_str(&match l {
            Lang::En => format!(" Names on the list: {}.", BLOAT_NAMES.join(", ")),
            Lang::Zh => format!(" 名单：{}。", BLOAT_NAMES.join("、")),
        });
        return Ok(write::Confirm {
            tier: write::Tier::Dialog,
            title: match l {
                Lang::En => format!("Kill all {} listed processes?", t.bloat_count),
                Lang::Zh => format!("结束名单里全部 {} 个进程？", t.bloat_count),
            },
            what,
            downtime: Some(downtime),
            recovery: Some(recovery),
            action: l.pick("Kill them", "全部结束").into(),
            cuts_uplink: false,
            word: None,
            danger: true,
            revert: crate::write::Revert::No,
        });
    }
    let pid = q.pid.ok_or_else(|| write::Outcome::Failed(l.pick("no process selected", "没有选中进程").into()))?;
    let name = top
        .and_then(|t| t.procs.iter().find(|p| p.pid == pid))
        .map_or_else(|| "?".to_owned(), |p| p.name.clone());
    Ok(write::Confirm {
        tier: write::Tier::Dialog,
        title: l.pick("Kill this process?", "结束这个进程？").into(),
        what: match l {
            Lang::En => format!("Sends SIGKILL to: {name} ({pid})"),
            Lang::Zh => format!("向这个进程发 SIGKILL：{name}（{pid}）"),
        },
        downtime: Some(downtime),
        recovery: Some(recovery),
        action: l.pick("Kill", "结束").into(),
        cuts_uplink: false,
        word: None,
        danger: true,
        revert: crate::write::Revert::No,
    })
}

pub async fn kill_ask(
    State(app): State<Shared>,
    Query(q): Query<KillQ>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let who = Caller::of(&app, &req);
    let top = top_now(&app, &who).await.ok().and_then(|v| parse(&v));
    one(match confirm(top.as_ref(), &q, ctx.remote, l) {
        Err(o) => area(l, ctx.remote, "", &write::Phase::Done(o)),
        Ok(c) => {
            let set = if q.all {
                format!("{PATH}/kill/set?all=true")
            } else {
                format!("{PATH}/kill/set?pid={}", q.pid.unwrap_or(0))
            };
            area(l, ctx.remote, &set, &write::Phase::Ask(c))
        }
    })
}

fn names(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|p| {
                    Some(format!("{} ({})", p["name"].as_str()?, p["pid"].as_u64()?))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The agent's reply as the result line (no readback: procd may start the process again).
pub fn outcome(v: &Value, l: Lang) -> write::Outcome {
    let killed = names(&v["killed"]);
    let skipped = names(&v["skipped"]);
    let freed = v["freed_rss_kb"].as_u64().unwrap_or(0);
    let mut s = if killed.is_empty() {
        l.pick("Nothing was killed.", "没有结束任何进程。").to_owned()
    } else {
        let mut k = match l {
            Lang::En => format!("Killed {}: {}", killed.len(), killed.join(", ")),
            Lang::Zh => format!("已结束 {} 个：{}", killed.len(), killed.join("、")),
        };
        if freed > 0 {
            k.push_str(&match l {
                Lang::En => format!(" · freed {}", kb(freed)),
                Lang::Zh => format!(" · 释放 {}", kb(freed)),
            });
        }
        k
    };
    if !skipped.is_empty() {
        s.push_str(&match l {
            Lang::En => format!(" Not killed (not on the agent's list or already gone): {}", skipped.join(", ")),
            Lang::Zh => format!(" 没有结束（不在后台名单里或已经不在）：{}", skipped.join("、")),
        });
    }
    let tone = if !killed.is_empty() && skipped.is_empty() { "ok" } else { "warn" };
    write::Outcome::Ended(s, tone)
}

pub async fn kill_set(
    State(app): State<Shared>,
    Query(q): Query<KillQ>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    let who = Caller::of(&app, &req);
    let pending = area(
        l,
        Via::Local,
        "",
        &write::Phase::Pending(l.pick("Killing…", "正在结束…").into()),
    );
    write::respond(pending, async move {
        let body = if q.all {
            json!({"all": true})
        } else {
            json!({"pids": [q.pid.unwrap_or(0)]})
        };
        let o = match crate::agent::post(&app, &who, API_KILL, &body).await {
            Ok(v) => outcome(&v, l),
            Err(Fail::Unreachable) => write::Outcome::Unknown,
            Err(e) => write::Outcome::Failed(e.text(l)),
        };
        forget(&app).await;
        area(l, Via::Local, "", &write::Phase::Done(o))
    })
}

pub async fn kill_cancel(
    Query(q): Query<write::CancelQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    one(area(ctx.lang, ctx.remote, "", &write::cancel_phase(&q)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn top() -> Value {
        json!({"processes":[
            {"pid":10,"name":"zte_dua","cpu_pct":0.0,"rss_kb":900,"state":"sleeping","is_bloat":true},
            {"pid":2,"name":"Alpha","cpu_pct":4.5,"rss_kb":20480,"state":"running","is_bloat":false},
            {"pid":7,"name":"beta","cpu_pct":9.1,"rss_kb":100,"state":"disk","is_bloat":false}],
            "total_count":80,"bloat_count":3,"bloat_cpu_pct":0.0,"bloat_rss_kb":2048})
    }

    fn got() -> Got {
        Got { top: parse(&top()), err: None, at: None }
    }

    #[test]
    fn the_status_and_the_listed_rows_like_the_old_page() {
        let v = view(&got(), Sort::of(&SortQ::default()), Lang::En);
        assert_eq!(v.state, "80 processes running");
        assert_eq!(v.reason.as_deref(), Some("3 of them can be killed: 0.0% CPU, 2.0 MB memory."));
        assert_eq!(v.bloat_n, 3);
        assert_eq!(v.bloat_rows.len(), 1);
        assert_eq!(v.bloat_rows[0].sub, "PID 10 · 0.0% · 900 KB");
        assert!(v.bloat_note.is_none());
        let first = v.slots[0].as_ref().unwrap();
        assert_eq!((first.name.as_str(), first.cpu.as_str(), first.state.as_str()), ("beta", "9.1", "disk wait"));
        assert!(v.slots[3].is_none() && v.slots.len() == SLOTS);
    }

    #[test]
    fn sorting_and_the_first_sample() {
        let name = Sort::of(&SortQ { sort: "name".into(), dir: "".into() });
        assert!(!name.desc);
        let v = view(&got(), name, Lang::En);
        let order: Vec<_> = v.slots.iter().flatten().map(|r| r.name.as_str()).collect();
        assert_eq!(order, ["Alpha", "beta", "zte_dua"]);
        let mem = Sort::of(&SortQ { sort: "mem".into(), dir: "asc".into() });
        let v = view(&got(), mem, Lang::En);
        assert_eq!(v.slots[0].as_ref().unwrap().name, "beta");
        let mut z = top();
        for p in z["processes"].as_array_mut().unwrap() {
            p["cpu_pct"] = json!(0.0);
        }
        let v = view(&Got { top: parse(&z), err: None, at: None }, name, Lang::En);
        assert!(v.cpu_hint);
        assert_eq!(v.slots[0].as_ref().unwrap().cpu, "—");
    }

    #[test]
    fn a_failed_read_keeps_the_list_and_says_it_stopped() {
        let g = Got { top: parse(&top()), err: Some(Fail::Unreachable), at: None };
        let v = view(&g, Sort::of(&SortQ::default()), Lang::En);
        assert!(v.stale && v.locked && v.retry);
        assert_eq!(v.tone, "stale");
        let v = view(&Got { top: None, err: Some(Fail::Unreachable), at: None }, Sort::of(&SortQ::default()), Lang::En);
        assert_eq!((v.tone, v.state.as_str()), ("bad", "Couldn't read the process list"));
    }

    #[test]
    fn the_confirm_lists_what_it_will_kill_and_the_rest() {
        let t = parse(&top());
        let c = confirm(t.as_ref(), &KillQ { pid: None, all: true }, Via::Local, Lang::En).unwrap();
        assert_eq!(c.title, "Kill all 3 listed processes?");
        assert!(c.what.contains("zte_dua (10)") && c.what.contains("and 2 more") && c.what.contains("zte-topsw-tunnel"));
        assert!(c.danger && c.tier == write::Tier::Dialog);
        let c = confirm(t.as_ref(), &KillQ { pid: Some(10), all: false }, Via::Local, Lang::En).unwrap();
        assert_eq!(c.what, "Sends SIGKILL to: zte_dua (10)");
        assert!(confirm(None, &KillQ { pid: None, all: true }, Via::Local, Lang::En).is_err());
    }

    #[test]
    fn the_result_names_killed_and_skipped() {
        let o = outcome(&json!({"killed":[{"pid":10,"name":"zte_dua"}],"skipped":[],"freed_rss_kb":2048}), Lang::En);
        assert_eq!(o, write::Outcome::Ended("Killed 1: zte_dua (10) · freed 2.0 MB".into(), "ok"));
        let o = outcome(&json!({"killed":[],"skipped":[{"pid":3,"name":"sshd"}],"freed_rss_kb":0}), Lang::En);
        assert_eq!(
            o,
            write::Outcome::Ended("Nothing was killed. Not killed (not on the agent's list or already gone): sshd (3)".into(), "warn")
        );
    }

    #[test]
    fn the_english_page_has_no_chinese() {
        for g in [got(), Got::default(), Got { top: None, err: Some(Fail::Unreachable), at: None }] {
            let v = view(&g, Sort::of(&SortQ::default()), Lang::En);
            let html = Leaves { v: &v }.render().unwrap();
            assert!(!html.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c) || ('\u{3000}'..='\u{303f}').contains(&c)), "{html}");
        }
    }
}
