//! /health (健康; curl keeps the JSON link status): the device check
//! (doctor.sh on the device, run by u60d/the agent every minute) and the crash logs kept by the
//! process supervisor.
//!
//! Where each value comes from (the old page read `/api/health` every 60 s; u60d serves it, the
//! agent when u60d says "not mine", both through `agent.rs` with the tab's own session):
//! - summary (「一切正常 / N 项异常，M 项需要注意」) + 「HH:mm 检查」, the error line, 「第一次检查」
//!   hint, the check list (level mark, name, detail; the manifest row first), the crash-log list
//!   (time, program, status): `GET /api/health`, polled every 60 s ([`EVERY`]) and re-read when
//!   the check was run from here (`App::kick`). Plain `/api/health` never runs the script.
//! - Crash log text (opened with 「查看」/「收起」, up to 256 KB): `GET /api/health/crashlog?program=
//!   &file=` on each opening, shown in a console band under its row. Which row is open is a page
//!   signal (`_hlopen`), so a push of the list does not close it (a re-sent row that is open
//!   loads its text again).
//!
//! The one write, 「现在检查」, tier 2 (inventory §/health: a GET, but it starts a process on the
//! device): inline confirm, then `GET /api/health?refresh=1` (u60d may take up to 20 s, so 30 s
//! here), only ever on that press. Its readback is the plain `GET /api/health`: `checked_at`
//! newer than before (it moves on every run, a failing one too; the error shows in the summary).
//! One run at a time (a second press says so). A reply that does not come → read back anyway;
//! nothing newer → 「连接中断，还没确认是否生效」.

use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use askama::Template;
use axum::Router;
use axum::extract::{Query, Request, State};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::Stream;
use serde_json::Value;

use crate::topbar::Via;
use crate::agent::{Caller, Fail, Polled, esc};
use crate::alerts_page::mdhm;
use crate::i18n::Lang;
use crate::pages::{Shared, StreamQuery, patch, split_leaves};
use crate::write::{self, Outcome, Phase};

const PATH: &str = "/health";
const EVERY: Duration = Duration::from_secs(60);
/// The check runs up to 20 s on the device.
const RUN_LIMIT: Duration = Duration::from_secs(30);
/// One run at a time, from any tab.
static RUNNING: AtomicBool = AtomicBool::new(false);

/// `GET /health`: the page for a browser once migrated (`proxy::health_page` checked the session),
/// luyaz-web's own link status for everyone else (the ship script reads it).
pub async fn dispatch(State(app): State<Shared>, req: Request) -> Response {
    if crate::proxy::health_page(req.uri().path(), req.headers(), &app.pages) {
        return page(State(app), req).await;
    }
    crate::pages::health(State(app)).await.into_response()
}

// ---- what the agent says ----

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Check {
    pub level: String,
    pub id: String,
    pub label: String,
    pub detail: String,
    pub label_en: Option<String>,
    pub detail_en: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Crash {
    pub program: String,
    pub file: String,
    pub time: i64,
    pub status: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Data {
    pub checks: Vec<Check>,
    pub bad: u64,
    pub warn: u64,
    pub checked_at: Option<i64>,
    pub error: Option<String>,
    pub crashlogs: Vec<Crash>,
}

fn opt(v: &Value) -> Option<String> {
    v.as_str().filter(|x| !x.is_empty()).map(Into::into)
}

pub fn parse(v: &Value) -> Data {
    let list = |k: &str| v[k].as_array().cloned().unwrap_or_default();
    let st = |x: &Value| x.as_str().unwrap_or("").to_owned();
    Data {
        checks: list("checks")
            .iter()
            .map(|c| Check {
                level: st(&c["level"]),
                id: st(&c["id"]),
                label: st(&c["label"]),
                detail: st(&c["detail"]),
                label_en: opt(&c["label_en"]),
                detail_en: opt(&c["detail_en"]),
            })
            .collect(),
        bad: v["bad"].as_u64().unwrap_or(0),
        warn: v["warn"].as_u64().unwrap_or(0),
        checked_at: v["checked_at"].as_i64(),
        error: opt(&v["error"]),
        crashlogs: list("crashlogs")
            .iter()
            .map(|c| Crash {
                program: st(&c["program"]),
                file: st(&c["file"]),
                time: c["time"].as_i64().unwrap_or(0),
                status: st(&c["status"]),
            })
            .collect(),
    }
}

/// The agent's `safe_name`: what may name a crash-log program or file.
pub fn safe_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 96
        && !n.starts_with('.')
        && n.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// An element id / signal key for one crash log.
fn rid(program: &str, file: &str) -> String {
    format!("{program}-{file}")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// "HH:mm" of a device-clock epoch.
fn hm(secs: i64) -> String {
    mdhm(secs)[6..].to_owned()
}

/// doctor.sh `--tsv` puts the manifest verdict last; people read it first.
fn rows(d: &Data, l: Lang) -> Vec<(String, String, &'static str, String)> {
    let pick = |zh: &String, en: &Option<String>| match l {
        Lang::En => en.clone().unwrap_or_else(|| zh.clone()),
        Lang::Zh => zh.clone(),
    };
    let mut r: Vec<_> = d
        .checks
        .iter()
        .map(|c| {
            let tone = match c.level.as_str() {
                "ok" => "ok",
                "warn" => "warn",
                _ => "bad",
            };
            (
                c.id.clone(),
                pick(&c.label, &c.label_en),
                tone,
                pick(&c.detail, &c.detail_en),
            )
        })
        .collect();
    if let Some(m) = r.iter().position(|x| x.0 == "manifest").filter(|m| *m > 0) {
        let row = r.remove(m);
        r.insert(0, row);
    }
    r
}

// ---- the page's leaves ----

struct Poll {
    who: Caller,
    l: Lang,
    data: Option<Data>,
    fail: Option<Fail>,
    at: Option<SystemTime>,
}

fn mark(tone: &str, text: &str) -> String {
    let sym = match tone {
        "bad" => "■",
        "warn" => "▲",
        _ => "●",
    };
    format!(
        "<span class=\"nd-mark nd-mark--{tone}\"><span class=\"nd-mark__sym\" aria-hidden=\"true\">{sym}</span><span>{}</span></span>",
        esc(text)
    )
}

impl Poll {
    fn new(who: Caller, l: Lang) -> Poll {
        Poll {
            who,
            l,
            data: None,
            fail: None,
            at: None,
        }
    }

    async fn fetch(&mut self, app: &Shared) {
        match crate::agent::get(app, &self.who, "/api/health").await {
            Ok(v) => {
                self.data = Some(parse(&v));
                self.fail = None;
                self.at = Some(SystemTime::now());
            }
            Err(f) => self.fail = Some(f),
        }
    }

    fn stale(&self) -> bool {
        self.data.is_some() && self.fail.is_some()
    }

    fn leaves(&self) -> Vec<(String, String)> {
        let l = self.l;
        let d = self.data.as_ref();
        let stale = self.stale();
        let st_cls = if stale { " nd-stale" } else { "" };
        let mut out = String::new();

        // summary
        let counts = |d: &Data| match l {
            Lang::En => format!("{} problem(s), {} to look at.", d.bad, d.warn),
            Lang::Zh => format!("{} 项异常，{} 项需要注意。", d.bad, d.warn),
        };
        let (mut tone, state, reason): (&str, String, Option<String>) = match (d, &self.fail) {
            (None, Some(f)) => (
                "bad",
                l.pick("Can't read the device check", "读不到设备检查结果")
                    .into(),
                Some(f.text(l)),
            ),
            (None, None) => (
                "neutral",
                l.pick("Reading the device check…", "正在读取设备检查结果…")
                    .into(),
                None,
            ),
            (Some(d), _) if d.error.is_some() => (
                "warn",
                l.pick("The check could not run", "检查没能运行").into(),
                Some(match l {
                    Lang::En => format!("The check could not run: {}", d.error.clone().unwrap()),
                    Lang::Zh => format!("检查没能运行：{}", d.error.clone().unwrap()),
                }),
            ),
            (Some(d), _) if d.checked_at.is_none() => (
                "neutral",
                l.pick("Not checked yet", "还没检查过").into(),
                Some(
                    l.pick(
                        "The first check runs about 20 seconds after the admin backend starts.",
                        "管理后台启动约 20 秒后做第一次检查。",
                    )
                    .into(),
                ),
            ),
            (Some(d), _) if d.bad > 0 => ("bad", counts(d), None),
            (Some(d), _) if d.warn > 0 => ("warn", counts(d), None),
            (Some(_), _) => (
                "ok",
                l.pick("Everything is fine.", "一切正常。").into(),
                None,
            ),
        };
        let checked = d.and_then(|d| d.checked_at).map(|t| match l {
            Lang::En => format!("checked {}", hm(t)),
            Lang::Zh => format!("{} 检查", hm(t)),
        });
        let mut meta: Vec<String> = checked.into_iter().collect();
        if stale {
            tone = "stale";
            meta.push(crate::agent::stopped(l, "Numbers", "数字", self.at));
        }
        let actions = if d.is_none() && self.fail.is_some() {
            format!(
                "<button type=\"button\" class=\"nd-btn nd-btn--secondary nd-btn--sm\" data-on:click=\"@post('/health/refresh')\">{}</button>",
                l.pick("Retry", "重试")
            )
        } else {
            format!(
                "<button type=\"button\" class=\"nd-btn nd-btn--secondary nd-btn--sm\" data-write=\"hl-chk\" data-on:click=\"@post('/health/check/ask')\">{}</button>",
                l.pick("Check now", "现在检查")
            )
        };
        out.push_str(&format!(
            "<div id=\"hl-status\"><section class=\"nd-status nd-status--{tone}{}\"><div class=\"nd-status__main\"><div class=\"nd-status__state\" role=\"status\">{}</div>{}{}</div><div class=\"nd-status__actions\">{actions}</div></section></div>\n<!--leaf-->\n",
            if tone == "stale" { " nd-stale" } else { "" },
            mark(tone, &state),
            reason.map_or(String::new(), |r| format!(
                "<div class=\"nd-status__reason\"><span{}>{}</span></div>",
                if tone == "bad" { " role=\"alert\"" } else { "" },
                esc(&r)
            )),
            if meta.is_empty() {
                String::new()
            } else {
                format!(
                    "<div class=\"nd-status__meta\"><span class=\"nd-fresh\">{}</span></div>",
                    esc(&meta.join(" · "))
                )
            },
        ));

        // checks
        out.push_str(&format!(
            "<div id=\"hl-checks\" class=\"nd-group{st_cls}\">"
        ));
        match d {
            None => out.push_str(
                "<div class=\"nd-row\"><span class=\"nd-skel\" style=\"width:16ch\"></span></div>",
            ),
            Some(d) if d.checks.is_empty() => out.push_str(&format!(
                "<div class=\"nd-row\"><span class=\"nd-aux\">{}</span></div>",
                if d.error.is_some() || d.checked_at.is_none() {
                    l.pick(
                        "No results yet. Press \"Check now\" to run the check.",
                        "还没有结果。点「现在检查」运行一遍。",
                    )
                } else {
                    l.pick("The check returned no items.", "这次检查没有返回任何项目。")
                }
            )),
            Some(d) => {
                for (_, label, tone, detail) in rows(d, l) {
                    let word = match tone {
                        "ok" => l.pick("OK", "正常"),
                        "warn" => l.pick("Needs a look", "需要注意"),
                        _ => l.pick("Problem", "异常"),
                    };
                    out.push_str(&format!(
                        "<div class=\"nd-row nd-row--two hl-item\"><span class=\"nd-row__text\"><span class=\"nd-row__label\">{}</span><span class=\"nd-row__sub hl-wrap\">{}</span></span><span class=\"hl-level\">{}</span></div>",
                        esc(&label),
                        esc(&detail),
                        mark(tone, word)
                    ));
                }
            }
        }
        out.push_str("</div>\n<!--leaf-->\n");

        // crash logs
        out.push_str("<div id=\"hl-crashes\">");
        match d {
            None => out.push_str(
                "<div class=\"nd-group\"><div class=\"nd-row\"><span class=\"nd-skel\" style=\"width:20ch\"></span></div></div>",
            ),
            Some(d) if d.crashlogs.is_empty() => out.push_str(&format!(
                "<p class=\"nd-body hl-note\">{}</p>",
                mark(
                    "ok",
                    l.pick(
                        "No crashes recorded. When a supervised program dies, its last output is kept here (5 per program).",
                        "没有崩溃记录。受监督的程序异常退出时，最后的输出会留在这里（每个程序 5 份）。"
                    )
                )
            )),
            Some(d) => {
                out.push_str(&format!("<div class=\"nd-group hl-crashes{st_cls}\">"));
                for c in &d.crashlogs {
                    out.push_str(&crash_row(c, l));
                }
                out.push_str("</div>");
            }
        }
        out.push_str("</div>\n");
        split_leaves(&out)
    }
}

fn crash_row(c: &Crash, l: Lang) -> String {
    let when = mdhm(c.time);
    let sub = format!(
        "<span class=\"nd-mono\">{}</span>{}",
        esc(&when),
        if c.status.is_empty() {
            String::new()
        } else {
            format!(" · {}", esc(&c.status))
        }
    );
    // Names that are not plain file names are listed but not opened (the agent would refuse them).
    if !safe_name(&c.program) || !safe_name(&c.file) {
        return format!(
            "<div class=\"nd-row nd-row--two\"><span class=\"nd-row__text\"><span class=\"nd-row__label\">{}</span><span class=\"nd-row__sub block\">{sub}</span></span></div>",
            esc(&c.program)
        );
    }
    let id = rid(&c.program, &c.file);
    let key = format!("{}/{}", c.program, c.file);
    let load = format!("@post('/health/log?program={}&file={}')", c.program, c.file);
    let (show, hide) = match l {
        Lang::En => (
            format!("Show the crash log of {} at {}", c.program, when),
            format!("Hide the crash log of {} at {}", c.program, when),
        ),
        Lang::Zh => (
            format!("查看 {} {} 的崩溃日志", c.program, when),
            format!("收起 {} {} 的崩溃日志", c.program, when),
        ),
    };
    let is_open = format!("$_hlopen === '{key}'");
    format!(
        "<div id=\"hl-c-{id}\" class=\"hl-crash\"><button type=\"button\" class=\"nd-row nd-row--two\" aria-expanded=\"false\" aria-controls=\"hl-p-{id}\" aria-label=\"{show}\" data-attr:aria-expanded=\"{is_open} ? 'true' : 'false'\" data-attr:aria-label=\"{is_open} ? '{hide}' : '{show}'\" data-on:click=\"$_hlopen = {is_open} ? '' : '{key}'; {is_open} &amp;&amp; {load}\"><span class=\"nd-row__text\"><span class=\"nd-row__label\">{}</span><span class=\"nd-row__sub block\">{sub}</span></span><span class=\"nd-aux hl-show\" data-text=\"{is_open} ? '{}' : '{}'\">{}</span></button>\
<div id=\"hl-p-{id}\" class=\"hl-panel\" style=\"display:none\" data-show=\"{is_open}\"><div id=\"hl-t-{id}\" data-init=\"{is_open} &amp;&amp; {load}\"><section class=\"nd-console\" aria-label=\"{}\"><p class=\"nd-console__muted\" role=\"status\">{}</p></section></div></div></div>",
        esc(&c.program),
        l.pick("Hide", "收起"),
        l.pick("Show", "查看"),
        l.pick("Show", "查看"),
        match l {
            Lang::En => format!("Crash log of {}", esc(&c.program)),
            Lang::Zh => format!("{} 的崩溃日志", esc(&c.program)),
        },
        l.pick("Loading…", "读取中…"),
        show = esc(&show),
        hide = esc(&hide),
    )
}

/// A crash log's text (or why not) in its console band.
fn log_html(program: &str, file: &str, text: Result<String, String>, l: Lang) -> String {
    let label = match l {
        Lang::En => format!("Crash log of {}", esc(program)),
        Lang::Zh => format!("{} 的崩溃日志", esc(program)),
    };
    let body = match text {
        Ok(t) => format!("<pre class=\"hl-pre\">{}</pre>", esc(&t)),
        Err(_) => format!(
            "<p role=\"alert\">{}</p>",
            l.pick("Could not load this log.", "读不到这份日志。")
        ),
    };
    format!(
        "<div id=\"hl-t-{}\"><section class=\"nd-console\" aria-label=\"{label}\" tabindex=\"0\">{body}</section></div>",
        rid(program, file)
    )
}

impl Polled for Poll {
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        self.fetch(app).await;
        let next = if self.data.is_none() {
            Duration::from_secs(10)
        } else {
            EVERY
        };
        (self.leaves(), next)
    }
}

// ---- the page ----

#[derive(Template)]
#[template(path = "health.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    leaves: Vec<(String, String)>,
    chk_op: String,
}

impl Page {
    fn leaf(&self, id: &str) -> &str {
        self.leaves
            .iter()
            .find(|(k, _)| k == id)
            .map_or("", |(_, h)| h.as_str())
    }
}

const CANCEL: &str = "/health/check/cancel";

fn chk_area(l: Lang, remote: Via, set: &str, phase: &Phase) -> String {
    write::Area {
        id: "hl-chk",
        l,
        remote,
        set,
        cancel: CANCEL,
        phase,
    }
    .html()
}

async fn page(State(app): State<Shared>, req: Request) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let mut p = Poll::new(Caller::of(&app, &req), ctx.lang);
    p.fetch(&app).await;
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
        leaves: p.leaves(),
        chk_op: chk_area(ctx.lang, ctx.remote, "", &Phase::Idle),
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

async fn events(
    State(app): State<Shared>,
    Query(q): Query<StreamQuery>,
    req: Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let p = Poll::new(Caller::of(&app, &req), ctx.lang);
    crate::agent::stream(app, ctx, q.g, PATH, p)
}

/// Retry: read again now.
async fn refresh(State(app): State<Shared>) -> axum::http::StatusCode {
    let _ = app.kick.send(PATH);
    axum::http::StatusCode::NO_CONTENT
}

fn one(html: String) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(futures_util::stream::once(async move { Ok(patch(&html)) }))
}

// ---- 「现在检查」 ----

pub fn check_confirm(l: Lang) -> write::Confirm {
    write::Confirm {
        tier: write::Tier::Inline,
        title: l.pick("Run the check", "运行检查").into(),
        what: l
            .pick(
                "Runs the health check script (doctor.sh) on the device now. It takes up to about 20 seconds.",
                "现在在设备上运行一遍健康检查脚本（doctor.sh），最长约 20 秒。",
            )
            .into(),
        downtime: None,
        recovery: None,
        action: l.pick("Confirm: run the check", "确认：运行检查").into(),
        cuts_uplink: false,
        word: None,
        danger: false,
        revert: crate::write::Revert::No,
    }
}

async fn check_ask(req: Request) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    one(if RUNNING.load(Ordering::SeqCst) {
        chk_area(
            l,
            ctx.remote,
            "",
            &Phase::Done(Outcome::NotYet(
                l.pick(
                    "A check is already running; wait for it to finish",
                    "已经有一次检查在运行，等它结束",
                )
                .into(),
            )),
        )
    } else {
        chk_area(
            l,
            ctx.remote,
            "/health/check/set",
            &Phase::Ask(check_confirm(l)),
        )
    })
}

/// Frees the one-run-at-a-time flag however the run ends (the tab may go away mid-run).
struct Running;
impl Drop for Running {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::SeqCst);
    }
}

async fn run_check(app: &Shared, who: &Caller, l: Lang) -> Outcome {
    let before = crate::agent::get(app, who, "/api/health")
        .await
        .ok()
        .and_then(|v| parse(&v).checked_at);
    let newer = |d: &Data| d.checked_at.is_some_and(|t| before.is_none_or(|b| t > b));
    let done = |d: &Data| {
        Outcome::Applied(match (d.checked_at, l) {
            (Some(t), Lang::En) => format!("Check finished at {}", hm(t)),
            (Some(t), Lang::Zh) => format!("{} 检查完成", hm(t)),
            (None, _) => l.pick("Check finished", "检查完成").into(),
        })
    };
    match crate::agent::get_slow(app, who, "/api/health?refresh=1", RUN_LIMIT).await {
        Ok(v) => {
            let d = parse(&v);
            if newer(&d) {
                done(&d)
            } else {
                Outcome::Accepted
            }
        }
        Err(Fail::Refused(c, m, en)) => Outcome::Failed(Fail::Refused(c, m, en).text(l)),
        Err(Fail::SignedOut) => Outcome::Failed(Fail::SignedOut.text(l)),
        // no answer: it may still have run; the plain read tells
        Err(Fail::Unreachable) => match crate::agent::get(app, who, "/api/health").await {
            Ok(v) if newer(&parse(&v)) => done(&parse(&v)),
            _ => Outcome::Unknown,
        },
    }
}

async fn check_set(State(app): State<Shared>, req: Request) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let (l, r) = (ctx.lang, ctx.remote);
    let who = Caller::of(&app, &req);
    let pending = chk_area(
        l,
        r,
        "",
        &Phase::Pending(
            l.pick(
                "Running the check… up to about 20 seconds",
                "正在运行检查…最长约 20 秒",
            )
            .into(),
        ),
    );
    write::respond(pending, async move {
        if RUNNING.swap(true, Ordering::SeqCst) {
            return chk_area(
                l,
                r,
                "",
                &Phase::Done(Outcome::NotYet(
                    l.pick(
                        "A check is already running; wait for it to finish",
                        "已经有一次检查在运行，等它结束",
                    )
                    .into(),
                )),
            );
        }
        let _free = Running;
        let o = run_check(&app, &who, l).await;
        let _ = app.kick.send(PATH);
        chk_area(l, r, "", &Phase::Done(o))
    })
}

async fn check_cancel(Query(q): Query<write::CancelQuery>, req: Request) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    one(chk_area(ctx.lang, ctx.remote, "", &write::cancel_phase(&q)))
}

// ---- a crash log's text ----

#[derive(serde::Deserialize)]
struct LogQuery {
    #[serde(default)]
    program: String,
    #[serde(default)]
    file: String,
}

async fn log(
    State(app): State<Shared>,
    Query(q): Query<LogQuery>,
    req: Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let who = Caller::of(&app, &req);
    let text = if safe_name(&q.program) && safe_name(&q.file) {
        crate::agent::get(
            &app,
            &who,
            &format!("/api/health/crashlog?program={}&file={}", q.program, q.file),
        )
        .await
        .map(|v| v["text"].as_str().unwrap_or("").to_owned())
        .map_err(|f| f.text(ctx.lang))
    } else {
        Err("bad name".into())
    };
    one(log_html(&q.program, &q.file, text, ctx.lang))
}

/// This page's routes (the page, its stream, its buttons).
pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/health/events", get(events))
        .route("/health/refresh", post(refresh))
        .route("/health/check/ask", post(check_ask))
        .route("/health/check/set", post(check_set))
        .route("/health/check/cancel", post(check_cancel))
        .route("/health/log", post(log))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Value {
        json!({
            "checks": [
                {"level": "ok", "id": "disk", "label": "/data 空间", "detail": "剩 1843 MB", "label_en": "/data space", "detail_en": "1843 MB free"},
                {"level": "warn", "id": "alerts", "label": "告警", "detail": "1 条未读", "label_en": null, "detail_en": null},
                {"level": "ok", "id": "manifest", "label": "清单", "detail": "一致", "label_en": "Manifest", "detail_en": "Matches"},
            ],
            "bad": 0, "warn": 1, "checked_at": 1_791_000_000, "error": null,
            "crashlogs": [
                {"program": "luyaz-ui", "file": "20261012-101112-up345.log", "time": 1_790_900_000, "size": 100, "status": "killed by SIGSEGV"},
                {"program": "../x", "file": "a.log", "time": 1_790_800_000, "size": 1, "status": ""},
            ],
        })
    }

    fn poll(l: Lang, data: Option<Data>, fail: Option<Fail>) -> Poll {
        let mut p = Poll::new(
            Caller {
                token: Some(Some("t".into())),
                peer: None,
                sid: None,
            },
            l,
        );
        p.data = data;
        p.fail = fail;
        p
    }

    fn leaf(p: &Poll, id: &str) -> String {
        p.leaves().into_iter().find(|(k, _)| k == id).unwrap().1
    }

    #[test]
    fn summary_counts_checked_time_and_the_button() {
        let p = poll(Lang::En, Some(parse(&sample())), None);
        let s = leaf(&p, "hl-status");
        assert!(
            s.contains("nd-status--warn") && s.contains("0 problem(s), 1 to look at."),
            "{s}"
        );
        assert!(
            s.contains("checked ") && s.contains("Check now") && s.contains("/health/check/ask"),
            "{s}"
        );
        let z = leaf(&poll(Lang::Zh, Some(parse(&sample())), None), "hl-status");
        assert!(
            z.contains("0 项异常，1 项需要注意。") && z.contains(" 检查"),
            "{z}"
        );
    }

    #[test]
    fn a_failed_run_and_no_run_yet_read_as_the_old_page() {
        let mut d = parse(&sample());
        d.error = Some("doctor.sh: not found".into());
        let s = leaf(&poll(Lang::En, Some(d), None), "hl-status");
        assert!(
            s.contains("The check could not run") && s.contains("doctor.sh: not found"),
            "{s}"
        );
        let mut d = parse(&sample());
        d.checked_at = None;
        let s = leaf(&poll(Lang::En, Some(d), None), "hl-status");
        assert!(
            s.contains("Not checked yet") && s.contains("about 20 seconds after"),
            "{s}"
        );
        let s = leaf(&poll(Lang::En, None, Some(Fail::Unreachable)), "hl-status");
        assert!(
            s.contains("Can't read the device check") && s.contains("/health/refresh"),
            "{s}"
        );
    }

    #[test]
    fn the_manifest_row_is_first_and_the_words_follow_the_language() {
        let p = poll(Lang::En, Some(parse(&sample())), None);
        let c = leaf(&p, "hl-checks");
        let (m, d) = (c.find("Manifest").unwrap(), c.find("/data space").unwrap());
        assert!(m < d, "{c}");
        // no English text from an older doctor: the Chinese is shown
        assert!(
            c.contains("告警") && c.contains("Needs a look") && c.contains(">OK<"),
            "{c}"
        );
    }

    #[test]
    fn a_crash_row_opens_by_signal_and_loads_its_text() {
        let p = poll(Lang::En, Some(parse(&sample())), None);
        let c = leaf(&p, "hl-crashes");
        assert!(
            c.contains("luyaz-ui") && c.contains("killed by SIGSEGV"),
            "{c}"
        );
        assert!(
            c.contains("@post('/health/log?program=luyaz-ui&file=20261012-101112-up345.log')"),
            "{c}"
        );
        assert!(
            c.contains("Show the crash log of luyaz-ui at 10-"),
            "{c}"
        );
        assert!(
            c.contains("$_hlopen === 'luyaz-ui/20261012-101112-up345.log'"),
            "{c}"
        );
        // an unsafe name is listed but cannot be opened
        assert_eq!(c.matches("hl-panel").count(), 1, "{c}");
    }

    #[test]
    fn log_text_is_escaped_and_failures_say_so() {
        let h = log_html("p", "a.log", Ok("<b>&x</b>".into()), Lang::En);
        assert!(
            h.contains("&lt;b&gt;&amp;x&lt;/b&gt;") && h.contains("id=\"hl-t-p-a_log\""),
            "{h}"
        );
        let h = log_html("p", "a.log", Err("x".into()), Lang::Zh);
        assert!(
            h.contains("读不到这份日志。") && h.contains("role=\"alert\""),
            "{h}"
        );
        assert!(
            safe_name("20261012-101112-up345.log")
                && !safe_name("../x")
                && !safe_name(".a")
                && !safe_name("")
        );
    }

    #[test]
    fn stale_keeps_the_list_and_says_since_when() {
        let mut p = poll(Lang::En, Some(parse(&sample())), Some(Fail::Unreachable));
        p.at = Some(SystemTime::now());
        let s = leaf(&p, "hl-status");
        assert!(
            s.contains("nd-status--stale") && s.contains("stopped at"),
            "{s}"
        );
        assert!(leaf(&p, "hl-checks").contains("nd-stale"));
    }
}
