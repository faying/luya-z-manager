//! /scheduler (计划任务), the old `scheduler/page.tsx` + `jobs.ts`, with the old
//! `/router/schedule` (定时重启) folded in: that page only listed the reboot jobs read-only and
//! linked here, so it is now the 「定时重启」 section of this page (`#reboot`) and its path
//! redirects there (user, 10-13).
//!
//! ```text
//!  agent /api/scheduler/jobs (tab's own session; u60d first once it owns the jobs)
//!     ├─ on open, after each write (kick), on 刷新, every 5 min ─▶ status · 定时重启 · list
//!  新建任务 / 编辑 ─▶ #sch-form (signals _j*) ─▶ save/ask: check (incl. the forbidden list) ─▶
//!     tier 2 inline, or tier 3 dialog when the job's request is a tier-3 one (reboot …) ─▶
//!     save/set: POST (create) / PUT (update, keeps enabled + restore) ─▶ read the list back
//!  row switch ─▶ toggle/ask (on + tier-3 request → dialog; forbidden → refused) ─▶ PUT …/toggle
//!  row 删除   ─▶ del/ask (tier 2) ─▶ DELETE ─▶ read back: gone
//! ```
//! - Data is the agent's `/api/scheduler/jobs`; the old page had no refresh interval (SWR on
//!   focus), so this one reads on open, after its own writes and on 刷新, and only every
//!   [`EVERY`] otherwise (a job's last-run line moves by itself).
//! - Forbidden targets (eSIM, factory reset, kill-bloat, power off, AT terminal) are refused here
//!   too, with the same matching as the agent's / u60d's `forbidden()` (query dropped, trailing
//!   slashes trimmed, case folded): on save and on switching a job on. The agent / u60d check again.
//!   So are the few known actions u60d has no route for ([`NOT_ON_U60D`], 8d-2: u60d runs the jobs).
//! - Weekdays are the agent's: 0 = Monday … 6 = Sunday. No day picked = every day (the old form's
//!   hint; the agent refuses an empty list, so all seven are sent).
//! - Device clock = local wall time labelled UTC (CLAUDE.md): epochs are shown with their UTC
//!   digits and a one-time job's date/time is read as UTC digits, i.e. as the device shows it.
//! - Fixed against the old page (as the old new-design page did): an edit keeps the job's on/off
//!   state and its restore step; a body that is not JSON blocks the save. Edit opens the job in
//!   the form above the list (not in the row), so a push of the list never wipes what you type.
//! - Job names, paths and errors are device data: text content only (`data-data`), never in an
//!   expression, a URL or an `aria-label`; only numeric ids go into URLs.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use askama::Template;
use axum::Router;
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
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
use crate::write;

pub const PATH: &str = "/scheduler";
const API: &str = "/api/scheduler/jobs";
const API_TOGGLE: &str = "/api/scheduler/jobs/toggle";
/// The list's own pace (the old page: none).
const EVERY: Duration = Duration::from_secs(300);
/// Where the old /router/schedule goes.
pub const REBOOT_ANCHOR: &str = "/scheduler#reboot";

/// This page's routes (the page, its stream, its buttons) and the old /router/schedule.
pub fn routes() -> Router<Shared> {
    Router::new()
        .route(PATH, get(page))
        .route("/scheduler/events", get(events))
        .route("/scheduler/refresh", post(refresh))
        .route("/scheduler/form", post(form_open))
        .route("/scheduler/form/close", post(form_close))
        .route("/scheduler/save/ask", post(save_ask))
        .route("/scheduler/save/set", post(save_set))
        .route("/scheduler/save/cancel", post(save_cancel))
        .route("/scheduler/toggle/ask", post(toggle_ask))
        .route("/scheduler/toggle/set", post(toggle_set))
        .route("/scheduler/del/ask", post(del_ask))
        .route("/scheduler/del/set", post(del_set))
        .route("/scheduler/row/cancel", post(row_cancel))
        .route("/router/schedule", get(to_reboot))
        .route("/router/schedule/", get(to_reboot))
        .route("/router/schedule/{*rest}", get(to_reboot))
}

/// GET /router/schedule (and anything under it): merged into this page (302).
pub async fn to_reboot() -> Response {
    (StatusCode::FOUND, [(header::LOCATION, REBOOT_ANCHOR)]).into_response()
}

// ---- policy: what a job may call, and how heavy it is ----

/// The path as the agent's router sees it: query / fragment dropped, trailing slashes trimmed,
/// case folded, doubled slashes collapsed (the agent's `forbidden()`).
pub fn norm(path: &str) -> String {
    let p = path
        .trim()
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches('/')
        .to_ascii_lowercase();
    p.replace("//", "/")
}

/// Paths a scheduled job may never call (zte-agent scheduler.rs `forbidden`, u60-policy): eSIM,
/// factory reset, kill-bloat, power off, the AT terminal. The English name of what it is.
pub fn forbidden(path: &str) -> Option<&'static str> {
    let p = norm(path);
    if p == "/api/esim" || p.starts_with("/api/esim/") {
        Some("eSIM")
    } else if p == "/api/device/factory-reset" {
        Some("factory reset")
    } else if p == "/api/system/kill-bloat" {
        Some("kill-bloat")
    } else if p == "/api/device/poweroff" {
        Some("power off")
    } else if p == "/api/at/send" {
        Some("AT terminal")
    } else {
        None
    }
}

/// Why a forbidden job is refused, in the page's language.
pub fn forbidden_text(what: &str, l: Lang) -> String {
    match l {
        Lang::En => format!(
            "a scheduled job may not call {what} (it could cut this device's only uplink or wipe its settings)"
        ),
        Lang::Zh => {
            let w = match what {
                "factory reset" => "恢复出厂",
                "kill-bloat" => "一键结束进程",
                "power off" => "关机",
                "AT terminal" => "AT 终端",
                w => w,
            };
            format!("定时任务不能调{w}（会改到这台设备唯一的上网方式或清掉设置）")
        }
    }
}

/// Requests a job could name that u60d, which runs the jobs since the agent retired (8d-2), has no
/// route for (its compat layer, `crates/luyaz-core/src/api.rs`): such a job would only ever record
/// "not supported", so the form refuses it. Prefixes end in `/`. The tier-3 paths below that u60d
/// does serve (reboot, STC reset, USB mode, SIM PIN, SMS-forward log) stay allowed.
const NOT_ON_U60D: [&str; 6] = [
    "/api/modem/network-mode",
    "/api/cell/band/",
    "/api/cell/lock/",
    "/api/router/apn/",
    "/api/system/kill",
    "/api/sms/delete",
];

/// The English name of what u60d cannot run, for [`not_on_u60d`].
pub fn not_on_u60d(path: &str) -> Option<&'static str> {
    let p = norm(path);
    NOT_ON_U60D
        .iter()
        .find(|x| p == x.trim_end_matches('/') || (x.ends_with('/') && p.starts_with(*x)))
        .copied()
}

/// Why a job may not be saved or switched on: forbidden, or not runnable on u60d.
pub fn refusal(path: &str, l: Lang) -> Option<String> {
    if let Some(w) = forbidden(path) {
        return Some(forbidden_text(w, l));
    }
    not_on_u60d(path).map(|x| match l {
        Lang::En => format!(
            "a scheduled job can't call {} any more: the service that runs the jobs has no such action (do it by hand on its page)",
            x.trim_end_matches('/')
        ),
        Lang::Zh => format!(
            "定时任务不能再调 {}：现在跑任务的后台服务没有这个操作（请到对应页面手动做）",
            x.trim_end_matches('/')
        ),
    })
}

/// Requests that are tier 3 when done by hand (jobs.ts `TIER3_PREFIXES`): a job that sends one
/// later is as heavy, so creating, editing (while on) or switching on such a job is tier 3.
const TIER3_PREFIXES: [&str; 16] = [
    "/api/device/reboot",
    "/api/device/factory-reset",
    "/api/modem/network-mode",
    "/api/cell/band/",
    "/api/cell/lock/",
    "/api/cell/stc/reset",
    "/api/router/apn/",
    "/api/esim/switch",
    "/api/esim/delete",
    "/api/usb/mode",
    "/api/system/kill",
    "/api/sim/pin/",
    "/api/sim/unlock",
    "/api/at/send",
    "/api/sms/delete",
    "/api/sms/forward/log/clear",
];

pub fn tier3(method: &str, path: &str) -> bool {
    let m = method.trim().to_ascii_uppercase();
    if m == "GET" {
        return false;
    }
    let p = norm(path);
    if p == "/api/sms/forward/rules" && m == "DELETE" {
        return true;
    }
    TIER3_PREFIXES
        .iter()
        .any(|x| p.starts_with(x) || p == x.trim_end_matches('/'))
}

/// A scheduled reboot: POST /api/device/reboot.
pub fn is_reboot(method: &str, path: &str) -> bool {
    method.trim().eq_ignore_ascii_case("POST") && norm(path) == "/api/device/reboot"
}

// ---- the agent's job ----

#[derive(Clone, Debug, PartialEq)]
pub enum Sched {
    Recurring { time: String, days: Vec<u8> },
    Once { at: Option<i64> },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    pub id: u64,
    pub name: String,
    pub enabled: bool,
    pub sched: Sched,
    pub method: String,
    pub path: String,
    pub body: Option<Value>,
    /// As the agent has it (sent back unchanged on an edit).
    pub restore: Option<Value>,
    pub last_run: Option<i64>,
    pub last_status: Option<i64>,
    pub last_error: Option<String>,
    pub last_restore: Option<i64>,
    pub last_restore_status: Option<i64>,
    pub last_restore_error: Option<String>,
}

fn s_of(v: &Value) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}

pub fn job(v: &Value) -> Option<Job> {
    let s = &v["schedule"];
    let sched = match s["type"].as_str() {
        Some("once") => Sched::Once {
            at: s["at"].as_i64(),
        },
        _ => Sched::Recurring {
            time: s["time"].as_str().unwrap_or("").to_owned(),
            days: s["days"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|d| d.as_u64().and_then(|d| u8::try_from(d).ok()))
                        .collect()
                })
                .unwrap_or_default(),
        },
    };
    Some(Job {
        id: v["id"].as_u64()?,
        name: v["name"].as_str().unwrap_or("").to_owned(),
        enabled: v["enabled"].as_bool().unwrap_or(false),
        sched,
        method: v["action"]["method"].as_str().unwrap_or("").to_owned(),
        path: v["action"]["path"].as_str().unwrap_or("").to_owned(),
        body: v["action"].get("body").filter(|b| !b.is_null()).cloned(),
        restore: v.get("restore").filter(|r| !r.is_null()).cloned(),
        last_run: v["last_run"].as_i64(),
        last_status: v["last_status"].as_i64(),
        last_error: s_of(&v["last_error"]),
        last_restore: v["last_restore"].as_i64(),
        last_restore_status: v["last_restore_status"].as_i64(),
        last_restore_error: s_of(&v["last_restore_error"]),
    })
}

pub fn jobs(v: &Value) -> Option<Vec<Job>> {
    Some(v.as_array()?.iter().filter_map(job).collect())
}

impl Job {
    pub fn failed(&self) -> bool {
        self.last_error.is_some() || self.last_status.is_some_and(|s| s >= 400)
    }
    pub fn request(&self) -> String {
        format!("{} {}", self.method, self.path)
    }
    fn action(&self) -> Value {
        let mut a = json!({"method": self.method, "path": self.path});
        if let Some(b) = &self.body {
            a["body"] = b.clone();
        }
        a
    }
    fn sched_value(&self) -> Value {
        match &self.sched {
            Sched::Recurring { time, days } => {
                json!({"type": "recurring", "time": time, "days": days})
            }
            Sched::Once { at } => json!({"type": "once", "at": at}),
        }
    }
}

// ---- times (device clock: UTC digits) ----

/// Days since 1970-01-01 → (y, m, d) (Howard Hinnant's civil_from_days).
pub(crate) fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// "2026-10-13 04:30" of a device epoch, as the device shows it.
pub fn fmt_at(at: i64) -> String {
    let (y, m, d) = civil(at.div_euclid(86_400));
    let s = at.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", s / 3600, s / 60 % 60)
}

/// The value of a `datetime-local` input for a device epoch.
fn wall_input(at: i64) -> String {
    fmt_at(at).replace(' ', "T")
}

/// A `datetime-local` value ("2026-10-14T04:30", seconds optional) read as the device's clock.
pub fn parse_wall(s: &str) -> Option<i64> {
    let (date, time) = s.trim().split_once(['T', ' '])?;
    let mut d = date.split('-');
    let y: i64 = d.next()?.parse().ok()?;
    let mo: u32 = d.next()?.parse().ok()?;
    let da: u32 = d.next()?.parse().ok()?;
    let mut t = time.split(':');
    let h: i64 = t.next()?.parse().ok()?;
    let mi: i64 = t.next()?.parse().ok()?;
    let se: i64 = t
        .next()
        .map_or(Some(0), |x| x.split('.').next()?.parse().ok())?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&da) || h > 23 || mi > 59 || se > 59 {
        return None;
    }
    Some(days_from_civil(y, mo, da) * 86_400 + h * 3600 + mi * 60 + se)
}

/// "HH:MM" as the agent checks it (`validate_time`), zero-padded.
pub fn parse_time(s: &str) -> Option<String> {
    let (h, m) = s.trim().split_once(':')?;
    let (h, m): (u8, u8) = (h.parse().ok()?, m.parse().ok()?);
    (h <= 23 && m <= 59).then(|| format!("{h:02}:{m:02}"))
}

const DAYS_EN: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const DAYS_ZH: [&str; 7] = ["周一", "周二", "周三", "周四", "周五", "周六", "周日"];

pub fn day(l: Lang, i: usize) -> &'static str {
    match l {
        Lang::En => DAYS_EN[i % 7],
        Lang::Zh => DAYS_ZH[i % 7],
    }
}

/// "04:30 — Mon, Tue" / "04:30 — daily" / "Once at 2026-10-14 04:30" (jobs.ts `fmtSchedule`).
pub fn fmt_sched(s: &Sched, l: Lang) -> String {
    match s {
        Sched::Once { at: Some(at) } => match l {
            Lang::En => format!("Once at {}", fmt_at(*at)),
            Lang::Zh => format!("于 {} 执行一次", fmt_at(*at)),
        },
        Sched::Once { at: None } => l.pick("Once", "一次").into(),
        Sched::Recurring { time, days } => {
            let mut ds: Vec<u8> = days.iter().copied().filter(|d| *d < 7).collect();
            ds.sort_unstable();
            ds.dedup();
            let when = if ds.is_empty() || ds.len() == 7 {
                l.pick("daily", "每天").to_owned()
            } else {
                ds.iter()
                    .map(|d| day(l, *d as usize))
                    .collect::<Vec<_>>()
                    .join(l.pick(", ", "、"))
            };
            let t = if time.is_empty() { "?" } else { time };
            format!("{t} — {when}")
        }
    }
}

// ---- the view ----

pub struct Row {
    pub id: u64,
    pub name: String,
    pub on: bool,
    pub when: String,
    pub req: String,
    /// Refused as a scheduled job (the device keeps it off).
    pub banned: Option<String>,
    pub restore: Option<String>,
    pub restore_err: Option<String>,
    /// (tone, text, detail in data-data)
    pub last: (&'static str, String, String),
}

pub struct View {
    pub l: Lang,
    pub tone: &'static str,
    pub state: String,
    /// Plain reason, and names (device data) after it.
    pub reason: Option<String>,
    pub names: Option<String>,
    pub meta: Option<String>,
    pub retry: bool,
    pub stale: bool,
    pub locked: bool,
    pub rows: Option<Vec<Row>>,
    /// 定时重启: (tone, state, reason)
    pub reboot: (&'static str, String, String),
}

impl View {
    pub fn symbol(&self) -> &'static str {
        crate::sms::symbol(self.tone)
    }
    pub fn rsym(&self) -> &'static str {
        crate::sms::symbol(self.reboot.0)
    }
    pub fn msym(&self, tone: &str) -> &'static str {
        crate::sms::symbol(tone)
    }
}

pub fn view(got: &Got, l: Lang) -> View {
    let list = got.jobs.as_ref();
    let stale = list.is_some() && got.err.is_some();
    let (mut tone, state, reason, names) = match (list, &got.err) {
        (None, Some(e)) => (
            "bad",
            l.pick("Can't read the scheduled jobs", "读不到计划任务")
                .to_owned(),
            Some(e.text(l)),
            None,
        ),
        (None, None) => (
            "neutral",
            l.pick("Reading scheduled jobs…", "正在读取计划任务…")
                .to_owned(),
            None,
            None,
        ),
        (Some(js), _) => {
            let on = js.iter().filter(|j| j.enabled).count();
            let failing: Vec<&Job> = js.iter().filter(|j| j.failed()).collect();
            if js.is_empty() {
                (
                    "neutral",
                    l.pick("No scheduled jobs", "没有计划任务").to_owned(),
                    Some(
                        l.pick(
                            "Use “New Job” to call an API on a timetable, e.g. a nightly reboot.",
                            "用「新建任务」按时间表调用接口，比如每晚重启。",
                        )
                        .to_owned(),
                    ),
                    None,
                )
            } else if !failing.is_empty() {
                (
                    "warn",
                    match l {
                        Lang::En => format!("{} job(s) failed last time", failing.len()),
                        Lang::Zh => format!("{} 个任务上次没成功", failing.len()),
                    },
                    None,
                    Some(
                        failing
                            .iter()
                            .map(|j| j.name.as_str())
                            .collect::<Vec<_>>()
                            .join(" · "),
                    ),
                )
            } else {
                (
                    if on > 0 { "ok" } else { "neutral" },
                    match l {
                        Lang::En => format!("{on} of {} jobs on", js.len()),
                        Lang::Zh => format!("{} 个任务里 {on} 个开着", js.len()),
                    },
                    None,
                    None,
                )
            }
        }
    };
    if stale {
        tone = "stale";
    }
    let meta = stale.then(|| crate::agent::stopped(l, "List", "列表", got.at));
    let rows = list.map(|js| js.iter().map(|j| row(j, l)).collect());
    View {
        l,
        tone,
        state,
        reason,
        names,
        meta,
        retry: got.err.is_some(),
        stale,
        locked: list.is_none() || stale,
        rows,
        reboot: reboot(list.map(Vec::as_slice), &got.err, l),
    }
}

/// The old /router/schedule's status: on (when) / none / N switched off.
fn reboot(list: Option<&[Job]>, err: &Option<Fail>, l: Lang) -> (&'static str, String, String) {
    let Some(js) = list else {
        return match err {
            Some(e) => (
                "bad",
                l.pick("Can't read the scheduled jobs", "读不到计划任务")
                    .into(),
                e.text(l),
            ),
            None => (
                "neutral",
                l.pick("Reading scheduled jobs…", "正在读取计划任务…")
                    .into(),
                String::new(),
            ),
        };
    };
    let rb: Vec<&Job> = js
        .iter()
        .filter(|j| is_reboot(&j.method, &j.path))
        .collect();
    let on: Vec<&Job> = rb.iter().copied().filter(|j| j.enabled).collect();
    if !on.is_empty() {
        (
            "ok",
            l.pick("Scheduled reboot on", "定时重启已开").into(),
            on.iter()
                .map(|j| fmt_sched(&j.sched, l))
                .collect::<Vec<_>>()
                .join(" · "),
        )
    } else if !rb.is_empty() {
        (
            "neutral",
            l.pick("No scheduled reboot", "没有定时重启").into(),
            match l {
                Lang::En => format!("{} reboot job(s) exist but are switched off.", rb.len()),
                Lang::Zh => format!("有 {} 个重启任务，但都关着。", rb.len()),
            },
        )
    } else {
        (
            "neutral",
            l.pick("No scheduled reboot", "没有定时重启").into(),
            l.pick(
                "Create one if you want the device to restart on a timetable.",
                "想让设备按时间表重启，就新建一个重启任务。",
            )
            .into(),
        )
    }
}

fn row(j: &Job, l: Lang) -> Row {
    let restore = j.restore.as_ref().map(|r| {
        let mut s = match l {
            Lang::En => format!("Restores at {}", r["time"].as_str().unwrap_or("?")),
            Lang::Zh => format!("{} 恢复", r["time"].as_str().unwrap_or("?")),
        };
        if let Some(at) = j.last_restore {
            s += &match l {
                Lang::En => format!(" · last restore {}", fmt_at(at)),
                Lang::Zh => format!(" · 上次恢复 {}", fmt_at(at)),
            };
        }
        if let Some(st) = j.last_restore_status {
            s += &format!(" · HTTP {st}");
        }
        s
    });
    let last = match j.last_run {
        None => (
            "neutral",
            l.pick("Hasn't run yet", "还没运行过").to_owned(),
            String::new(),
        ),
        Some(at) if j.failed() => (
            "bad",
            match l {
                Lang::En => format!("Last run {} failed: ", fmt_at(at)),
                Lang::Zh => format!("上次 {} 没成功：", fmt_at(at)),
            },
            j.last_error
                .clone()
                .unwrap_or_else(|| format!("HTTP {}", j.last_status.unwrap_or(0))),
        ),
        Some(at) => (
            "ok",
            match (l, j.last_status) {
                (Lang::En, Some(s)) => format!("Last: {} · HTTP {s}", fmt_at(at)),
                (Lang::En, None) => format!("Last: {}", fmt_at(at)),
                (Lang::Zh, Some(s)) => format!("上次：{} · HTTP {s}", fmt_at(at)),
                (Lang::Zh, None) => format!("上次：{}", fmt_at(at)),
            },
            String::new(),
        ),
    };
    Row {
        id: j.id,
        name: j.name.clone(),
        on: j.enabled,
        when: fmt_sched(&j.sched, l),
        req: j.request(),
        banned: refusal(&j.path, l),
        restore,
        restore_err: j.last_restore_error.clone(),
        last,
    }
}

#[derive(Template)]
#[template(path = "scheduler_leaves.html")]
struct Leaves<'a> {
    v: &'a View,
}

// ---- the stream ----

/// What a tab has read: the list (kept when a later read fails), the last failure, when.
#[derive(Default)]
pub struct Got {
    pub jobs: Option<Vec<Job>>,
    pub err: Option<Fail>,
    pub at: Option<SystemTime>,
}

async fn read(app: &Shared, who: &Caller) -> Result<Vec<Job>, Fail> {
    let v = crate::agent::get(app, who, API).await?;
    jobs(&v).ok_or_else(|| Fail::Refused(200, "unexpected reply".into(), "unexpected reply".into()))
}

pub struct Poll {
    who: Caller,
    l: Lang,
    got: Got,
}

impl Poll {
    fn leaves(&self) -> Vec<(String, String)> {
        let v = view(&self.got, self.l);
        split_leaves(&Leaves { v: &v }.render().unwrap())
    }
}

impl Polled for Poll {
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        match read(app, &self.who).await {
            Ok(js) => {
                self.got.jobs = Some(js);
                self.got.err = None;
                self.got.at = Some(SystemTime::now());
            }
            Err(e) => self.got.err = Some(e),
        }
        (self.leaves(), EVERY)
    }
}

#[derive(Template)]
#[template(path = "scheduler.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    leaves: Vec<(String, String)>,
    save_op: String,
    row_op: String,
}

impl Page {
    fn leaf(&self, id: &str) -> &str {
        self.leaves
            .iter()
            .find(|(k, _)| k == id)
            .map_or("", |(_, h)| h.as_str())
    }
}

/// The write areas: `sch-save` (the form's create / update), `sch-row` (switch, delete).
fn area(id: &str, l: Lang, remote: Via, set: &str, phase: &write::Phase) -> String {
    let cancel = format!("{PATH}/{}/cancel", &id[4..]);
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

pub async fn page(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let p = Poll {
        who: Caller::of(&app, &req),
        l,
        got: Got::default(),
    };
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
        leaves: p.leaves(),
        save_op: area("sch-save", l, ctx.remote, "", &idle),
        row_op: area("sch-row", l, ctx.remote, "", &idle),
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
        got: Got::default(),
    };
    crate::agent::stream(app, ctx, q.g, PATH, p)
}

/// 刷新 / 重试: every open tab reads the list again.
pub async fn refresh(State(app): State<Shared>) -> StatusCode {
    let _ = app.kick.send(PATH);
    StatusCode::NO_CONTENT
}

fn one(html: String) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(futures_util::stream::once(async move {
        Ok(crate::pages::patch(&html))
    }))
}

// ---- the form ----

/// The form's draft (signals `_j*`).
#[derive(Deserialize, Clone, Debug, Default)]
pub struct JobIn {
    #[serde(default, rename = "_ji")]
    pub id: u64,
    #[serde(default, rename = "_jn")]
    pub name: String,
    #[serde(default, rename = "_jm")]
    pub method: String,
    #[serde(default, rename = "_jp")]
    pub path: String,
    #[serde(default, rename = "_jb")]
    pub body: String,
    #[serde(default, rename = "_jk")]
    pub kind: String,
    #[serde(default, rename = "_jt")]
    pub time: String,
    #[serde(default, rename = "_jd0")]
    pub d0: bool,
    #[serde(default, rename = "_jd1")]
    pub d1: bool,
    #[serde(default, rename = "_jd2")]
    pub d2: bool,
    #[serde(default, rename = "_jd3")]
    pub d3: bool,
    #[serde(default, rename = "_jd4")]
    pub d4: bool,
    #[serde(default, rename = "_jd5")]
    pub d5: bool,
    #[serde(default, rename = "_jd6")]
    pub d6: bool,
    #[serde(default, rename = "_ja")]
    pub at: String,
}

impl JobIn {
    fn days(&self) -> Vec<u8> {
        [
            self.d0, self.d1, self.d2, self.d3, self.d4, self.d5, self.d6,
        ]
        .iter()
        .enumerate()
        .filter(|(_, on)| **on)
        .map(|(i, _)| i as u8)
        .collect()
    }
}

/// A checked draft: the job as it would be (id 0 = new).
#[derive(Clone, Debug, PartialEq)]
pub struct Draft {
    pub id: u64,
    pub name: String,
    pub method: String,
    pub path: String,
    pub body: Option<Value>,
    pub sched: Sched,
}

impl Draft {
    fn job(&self) -> Job {
        Job {
            id: self.id,
            name: self.name.clone(),
            enabled: true,
            sched: self.sched.clone(),
            method: self.method.clone(),
            path: self.path.clone(),
            body: self.body.clone(),
            restore: None,
            last_run: None,
            last_status: None,
            last_error: None,
            last_restore: None,
            last_restore_status: None,
            last_restore_error: None,
        }
    }
    fn same(&self, j: &Job) -> bool {
        j.name == self.name
            && j.method == self.method
            && j.path == self.path
            && j.sched == self.sched
    }
}

/// The form's checks (old page `check()` + the agent's `validate_job` + the forbidden list):
/// every problem, in one sentence.
pub fn check(f: &JobIn, l: Lang) -> Result<Draft, String> {
    let mut bad: Vec<String> = Vec::new();
    let name = f.name.trim().to_owned();
    if name.is_empty() {
        bad.push(l.pick("give the job a name", "给任务起个名字").into());
    }
    let method = f.method.trim().to_ascii_uppercase();
    if !["GET", "POST", "PUT", "DELETE"].contains(&method.as_str()) {
        bad.push(l.pick("pick a method", "选一个方法").into());
    }
    let path = f.path.trim().to_owned();
    let n = norm(&path);
    if !path.starts_with("/api/") || n.starts_with("/api/scheduler") || n.starts_with("/api/auth") {
        bad.push(
            l.pick(
                "the path starts with /api/ (not /api/scheduler or /api/auth)",
                "路径要以 /api/ 开头（不能是 /api/scheduler 或 /api/auth）",
            )
            .into(),
        );
    } else if let Some(why) = refusal(&path, l) {
        bad.push(why);
    }
    let body = if f.body.trim().is_empty() {
        None
    } else {
        match serde_json::from_str::<Value>(&f.body) {
            Ok(v) => Some(v),
            Err(_) => {
                bad.push(
                    l.pick(
                        "the body is not valid JSON (fix it or leave it empty)",
                        "请求体不是有效的 JSON（改好或留空）",
                    )
                    .into(),
                );
                None
            }
        }
    };
    let sched = if f.kind == "once" {
        match parse_wall(&f.at).filter(|at| *at > 1_000_000_000) {
            Some(at) => Sched::Once { at: Some(at) },
            None => {
                bad.push(l.pick("pick a date and time", "选好日期和时间").into());
                Sched::Once { at: None }
            }
        }
    } else {
        let mut days = f.days();
        if days.is_empty() {
            days = (0..7).collect();
        }
        match parse_time(&f.time) {
            Some(time) => Sched::Recurring { time, days },
            None => {
                bad.push(l.pick("pick a time", "选好时间").into());
                Sched::Recurring {
                    time: String::new(),
                    days,
                }
            }
        }
    };
    if !bad.is_empty() {
        let mut s = bad.join(l.pick("; ", "；"));
        if let Some(c) = s.get(..1) {
            if l == Lang::En {
                s = c.to_ascii_uppercase() + &s[1..];
            }
        }
        s.push_str(l.pick(".", "。"));
        return Err(s);
    }
    Ok(Draft {
        id: f.id,
        name,
        method,
        path,
        body,
        sched,
    })
}

#[derive(Template)]
#[template(path = "scheduler_form.html")]
struct Form<'a> {
    l: Lang,
    open: bool,
    editing: bool,
    /// A fresh id per opening, so a new form starts from its own signals.
    nonce: String,
    sigs: String,
    title: &'a str,
}

impl Form<'_> {
    fn day(&self, i: &usize) -> &'static str {
        day(self.l, *i)
    }
}

fn form_html(l: Lang, j: Option<&Job>, editing: bool) -> String {
    let (mut kind, mut time, mut days, mut at) = (
        "recurring",
        "03:00".to_owned(),
        vec![true; 7],
        String::new(),
    );
    if let Some(j) = j {
        match &j.sched {
            Sched::Recurring { time: t, days: ds } => {
                time = t.clone();
                days = (0..7u8).map(|d| ds.is_empty() || ds.contains(&d)).collect();
            }
            Sched::Once { at: a } => {
                kind = "once";
                at = a.map(wall_input).unwrap_or_default();
            }
        }
    }
    let mut sigs = json!({
        "_ji": if editing { j.map_or(0, |j| j.id) } else { 0 },
        "_jn": j.map(|j| j.name.clone()).unwrap_or_default(),
        "_jm": j.map_or("POST".into(), |j| j.method.clone()),
        "_jp": j.map(|j| j.path.clone()).unwrap_or_default(),
        "_jb": j.and_then(|j| j.body.as_ref()).map(|b| serde_json::to_string_pretty(b).unwrap_or_default()).unwrap_or_default(),
        "_jk": kind,
        "_jt": time,
        "_ja": at,
    });
    for (i, on) in days.iter().enumerate() {
        sigs[format!("_jd{i}")] = json!(on);
    }
    let title = if editing {
        l.pick("Edit job", "编辑任务")
    } else {
        l.pick("New Job", "新建任务")
    };
    Form {
        l,
        open: true,
        editing,
        nonce: write::next_op_id().replace('-', ""),
        sigs: sigs.to_string(),
        title,
    }
    .render()
    .unwrap()
}

fn form_closed(l: Lang) -> String {
    Form {
        l,
        open: false,
        editing: false,
        nonce: String::new(),
        sigs: String::new(),
        title: "",
    }
    .render()
    .unwrap()
}

#[derive(Deserialize, Default)]
pub struct FormQuery {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    preset: String,
}

/// 新建任务 / 新建重启任务 / 编辑 (id): the form, filled.
pub async fn form_open(
    State(app): State<Shared>,
    Query(q): Query<FormQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let who = Caller::of(&app, &req);
    let idle = area("sch-save", l, ctx.remote, "", &write::Phase::Idle);
    let html = match q.id {
        Some(id) => match read(&app, &who).await {
            Ok(js) => match js.iter().find(|j| j.id == id) {
                Some(j) => form_html(l, Some(j), true),
                None => {
                    form_closed(l)
                        + "\n"
                        + &area("sch-save", l, ctx.remote, "", &write::Phase::Done(gone(l)))
                }
            },
            Err(e) => {
                form_closed(l)
                    + "\n"
                    + &area(
                        "sch-save",
                        l,
                        ctx.remote,
                        "",
                        &write::Phase::Done(write::Outcome::Failed(e.text(l))),
                    )
            }
        },
        None if q.preset == "reboot" => {
            let mut j = Draft {
                id: 0,
                name: l.pick("Nightly reboot", "每晚重启").into(),
                method: "POST".into(),
                path: "/api/device/reboot".into(),
                body: None,
                sched: Sched::Recurring {
                    time: "04:30".into(),
                    days: (0..7).collect(),
                },
            }
            .job();
            j.id = 0;
            form_html(l, Some(&j), false)
        }
        None => form_html(l, None, false),
    };
    let html = if html.contains("sch-save-op") {
        html
    } else {
        html + "\n" + &idle
    };
    one(html)
}

pub async fn form_close(req: axum::extract::Request) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    one(form_closed(ctx.lang)
        + "\n"
        + &area("sch-save", ctx.lang, ctx.remote, "", &write::Phase::Idle))
}

// ---- writes ----

fn gone(l: Lang) -> write::Outcome {
    write::Outcome::Failed(
        l.pick("the job is no longer there", "这个任务已经不在了")
            .into(),
    )
}

fn expired(l: Lang) -> write::Outcome {
    write::Outcome::NotYet(
        l.pick("The form expired; press it again", "表单已过期，请再按一次")
            .into(),
    )
}

/// A save waiting for its confirm.
#[derive(Clone)]
struct Held {
    draft: Draft,
    at: Instant,
}

const STASH_TTL: Duration = Duration::from_secs(600);

fn stash() -> &'static Mutex<HashMap<String, Held>> {
    static S: std::sync::OnceLock<Mutex<HashMap<String, Held>>> = std::sync::OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

fn put(d: Draft) -> String {
    let t = crate::auth::random_id();
    let mut m = stash().lock().unwrap();
    m.retain(|_, h| h.at.elapsed() < STASH_TTL);
    m.insert(
        t.clone(),
        Held {
            draft: d,
            at: Instant::now(),
        },
    );
    t
}

fn take(t: &str) -> Option<Draft> {
    stash()
        .lock()
        .unwrap()
        .remove(t)
        .filter(|h| h.at.elapsed() < STASH_TTL)
        .map(|h| h.draft)
}

/// What a confirm says for a job that would send `j`'s request (tier 3 when that request is a
/// tier-3 one and the job will be on).
fn confirm(
    l: Lang,
    j: &Job,
    heavy: bool,
    title: String,
    light: String,
    action: String,
) -> write::Confirm {
    let when = fmt_sched(&j.sched, l);
    let req = j.request();
    if heavy {
        write::Confirm {
            tier: write::Tier::Dialog,
            title,
            what: match l {
                Lang::En => format!(
                    "At {when} the device sends {req} by itself, with nobody there to confirm. Done by hand, this request needs a confirm dialog."
                ),
                Lang::Zh => format!(
                    "到 {when}，设备会自己发出 {req}，没有人在旁边确认。手动做这一步需要确认对话框。"
                ),
            },
            downtime: Some(if is_reboot(&j.method, &j.path) {
                l.pick(
                    "Nothing changes now. Each time it runs, the device restarts and is unreachable for about 90 seconds.",
                    "现在不会有变化。每次运行时设备都会重启，约 90 秒连不上。",
                )
                .into()
            } else {
                l.pick(
                    "Nothing changes now. What happens when it runs depends on the request.",
                    "现在不会有变化。运行时会怎样取决于这个请求。",
                )
                .into()
            }),
            recovery: Some(
                l.pick(
                    "Switch the job off or delete it here before it runs.",
                    "在它运行之前，到这里关掉或删除这个任务。",
                )
                .into(),
            ),
            action,
            cuts_uplink: false,
            word: None,
            danger: false,
            revert: crate::write::Revert::No,
        }
    } else {
        write::Confirm {
            tier: write::Tier::Inline,
            title,
            what: light,
            downtime: None,
            recovery: None,
            action,
            cuts_uplink: false,
            word: None,
            danger: false,
            revert: crate::write::Revert::No,
        }
    }
}

#[derive(Deserialize, Default)]
pub struct TokenQuery {
    #[serde(default)]
    t: String,
}

async fn body_of(req: axum::extract::Request) -> Vec<u8> {
    axum::body::to_bytes(req.into_body(), 64 * 1024)
        .await
        .map(|b| b.to_vec())
        .unwrap_or_default()
}

/// 创建 / 更新: check the draft, keep it under a token, ask (tier 2 or 3).
pub async fn save_ask(State(app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let who = Caller::of(&app, &req);
    let f: JobIn = serde_json::from_slice(&body_of(req).await).unwrap_or_default();
    let done = |o| area("sch-save", l, ctx.remote, "", &write::Phase::Done(o));
    let d = match check(&f, l) {
        Ok(d) => d,
        Err(s) => return one(done(write::Outcome::NotYet(s))),
    };
    // an edit: the job as it is now (its on/off state decides the tier)
    let enabled = if d.id == 0 {
        true
    } else {
        match read(&app, &who).await {
            Err(e) => return one(done(write::Outcome::Failed(e.text(l)))),
            Ok(js) => match js.iter().find(|j| j.id == d.id) {
                None => return one(done(gone(l))),
                Some(j) => j.enabled,
            },
        }
    };
    let j = d.job();
    let heavy = enabled && tier3(&d.method, &d.path);
    let req_s = j.request();
    let when = fmt_sched(&j.sched, l);
    let c = if d.id == 0 {
        confirm(
            l,
            &j,
            heavy,
            match l {
                Lang::En => format!("Create the job “{}”?", d.name),
                Lang::Zh => format!("创建任务「{}」？", d.name),
            },
            match l {
                Lang::En => format!("From now on the device sends {req_s} at {when}."),
                Lang::Zh => format!("从现在起，设备会在 {when} 发出 {req_s}。"),
            },
            l.pick("Confirm: create", "确认：创建").into(),
        )
    } else {
        confirm(
            l,
            &j,
            heavy,
            match l {
                Lang::En => format!("Save the changes to “{}”?", d.name),
                Lang::Zh => format!("保存对「{}」的修改？", d.name),
            },
            match (l, enabled) {
                (Lang::En, true) => format!("The job sends {req_s} at {when}."),
                (Lang::En, false) => {
                    format!("The job sends {req_s} at {when} once it is switched on.")
                }
                (Lang::Zh, true) => format!("这个任务会在 {when} 发出 {req_s}。"),
                (Lang::Zh, false) => format!("这个任务打开后，会在 {when} 发出 {req_s}。"),
            },
            l.pick("Confirm: save", "确认：保存").into(),
        )
    };
    let set = format!("{PATH}/save/set?t={}", put(d));
    one(area("sch-save", l, ctx.remote, &set, &write::Phase::Ask(c)))
}

/// Read the list until `ok` holds (three looks a second apart); None = could not read.
async fn read_back(app: &Shared, who: &Caller, ok: impl Fn(&[Job]) -> bool) -> Option<bool> {
    let mut read_once = false;
    for i in 0..3 {
        if i > 0 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        if let Ok(js) = read(app, who).await {
            read_once = true;
            if ok(&js) {
                return Some(true);
            }
        }
    }
    read_once.then_some(false)
}

/// One agent write and its readback → the outcome. `sent` = the write's own reply.
async fn finish(
    app: &Shared,
    who: &Caller,
    sent: Result<Value, Fail>,
    ok: impl Fn(&[Job], &Value) -> bool,
    applied: String,
    l: Lang,
) -> write::Outcome {
    let (reply, no_reply) = match sent {
        Ok(v) => (v, false),
        Err(Fail::Unreachable) => (Value::Null, true),
        Err(e) => return write::Outcome::Failed(e.text(l)),
    };
    let _ = app.kick.send(PATH);
    match read_back(app, who, |js| ok(js, &reply)).await {
        Some(true) => write::Outcome::Applied(applied),
        _ if no_reply => write::Outcome::Unknown,
        _ => write::Outcome::Accepted,
    }
}

pub async fn save_set(
    State(app): State<Shared>,
    Query(q): Query<TokenQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let who = Caller::of(&app, &req);
    let draft = take(&q.t);
    let pending = area(
        "sch-save",
        l,
        Via::Local,
        "",
        &write::Phase::Pending(l.pick("Saving…", "正在保存…").into()),
    );
    write::respond(pending, async move {
        let o = match draft {
            None => expired(l),
            Some(d) => save(&app, &who, d, l).await,
        };
        let close = if matches!(o, write::Outcome::Applied(_)) {
            form_closed(l) + "\n"
        } else {
            String::new()
        };
        format!(
            "{close}{}",
            area("sch-save", l, Via::Local, "", &write::Phase::Done(o))
        )
    })
}

async fn save(app: &Shared, who: &Caller, d: Draft, l: Lang) -> write::Outcome {
    let j = d.job();
    if d.id == 0 {
        let body = json!({"name": d.name, "action": j.action(), "schedule": j.sched_value()});
        let sent = crate::agent::post(app, who, API, &body).await;
        let d2 = d.clone();
        return finish(
            app,
            who,
            sent,
            move |js, reply| match reply["id"].as_u64() {
                Some(id) => js.iter().any(|x| x.id == id),
                None => js.iter().any(|x| d2.same(x)),
            },
            match l {
                Lang::En => format!("Created “{}”", d.name),
                Lang::Zh => format!("已创建「{}」", d.name),
            },
            l,
        )
        .await;
    }
    // the job as it is now: keep its on/off state and its restore step (PUT replaces it whole)
    let now = match read(app, who).await {
        Err(e) => return write::Outcome::Failed(e.text(l)),
        Ok(js) => match js.into_iter().find(|x| x.id == d.id) {
            None => return gone(l),
            Some(x) => x,
        },
    };
    let mut body = json!({"id": d.id, "name": d.name, "enabled": now.enabled,
        "action": j.action(), "schedule": j.sched_value()});
    if let Some(r) = &now.restore {
        body["restore"] = r.clone();
    }
    let sent = crate::agent::send_json(app, who, reqwest::Method::PUT, API, &body).await;
    let (d2, en) = (d.clone(), now.enabled);
    finish(
        app,
        who,
        sent,
        move |js, _| {
            js.iter()
                .any(|x| x.id == d2.id && d2.same(x) && x.enabled == en)
        },
        match l {
            Lang::En => format!("Saved “{}”", d.name),
            Lang::Zh => format!("已保存「{}」", d.name),
        },
        l,
    )
    .await
}

pub async fn save_cancel(
    Query(q): Query<write::CancelQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    one(area(
        "sch-save",
        ctx.lang,
        ctx.remote,
        "",
        &write::cancel_phase(&q),
    ))
}

#[derive(Deserialize, Default)]
pub struct RowQuery {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    on: bool,
}

/// The job `id` as it is now, or the outcome that says why not.
async fn current(app: &Shared, who: &Caller, id: u64, l: Lang) -> Result<Job, write::Outcome> {
    match read(app, who).await {
        Err(e) => Err(write::Outcome::Failed(e.text(l))),
        Ok(js) => js.into_iter().find(|j| j.id == id).ok_or_else(|| gone(l)),
    }
}

/// The row switch: on → refused if forbidden, tier 3 for a tier-3 request; off → tier 2.
pub async fn toggle_ask(
    State(app): State<Shared>,
    Query(q): Query<RowQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let who = Caller::of(&app, &req);
    let done = |o| area("sch-row", l, ctx.remote, "", &write::Phase::Done(o));
    let j = match current(&app, &who, q.id, l).await {
        Ok(j) => j,
        Err(o) => return one(done(o)),
    };
    if q.on
        && let Some(why) = refusal(&j.path, l)
    {
        return one(done(write::Outcome::Failed(why)));
    }
    let heavy = q.on && tier3(&j.method, &j.path);
    let when = fmt_sched(&j.sched, l);
    let c = confirm(
        l,
        &j,
        heavy,
        match (l, q.on) {
            (Lang::En, true) => format!("Switch on “{}”?", j.name),
            (Lang::En, false) => format!("Switch off “{}”?", j.name),
            (Lang::Zh, true) => format!("打开「{}」？", j.name),
            (Lang::Zh, false) => format!("关掉「{}」？", j.name),
        },
        match (l, q.on) {
            (Lang::En, true) => format!("“{}” runs again at {when}.", j.name),
            (Lang::En, false) => format!(
                "“{}” stops running until you switch it back on. It stays in the list.",
                j.name
            ),
            (Lang::Zh, true) => format!("「{}」会重新在 {when} 运行。", j.name),
            (Lang::Zh, false) => {
                format!("「{}」不再运行，直到你重新打开；它会留在列表里。", j.name)
            }
        },
        match (l, q.on) {
            (Lang::En, true) => "Confirm: switch on",
            (Lang::En, false) => "Confirm: switch off",
            (Lang::Zh, true) => "确认：打开",
            (Lang::Zh, false) => "确认：关掉",
        }
        .into(),
    );
    let set = format!("{PATH}/toggle/set?id={}&on={}", j.id, q.on);
    one(area("sch-row", l, ctx.remote, &set, &write::Phase::Ask(c)))
}

pub async fn toggle_set(
    State(app): State<Shared>,
    Query(q): Query<RowQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let who = Caller::of(&app, &req);
    let pending = area(
        "sch-row",
        l,
        Via::Local,
        "",
        &write::Phase::Pending(
            if q.on {
                l.pick("Switching on…", "正在打开…")
            } else {
                l.pick("Switching off…", "正在关掉…")
            }
            .into(),
        ),
    );
    write::respond(pending, async move {
        let o = async {
            let j = match current(&app, &who, q.id, l).await {
                Ok(j) => j,
                Err(o) => return o,
            };
            if q.on
                && let Some(why) = refusal(&j.path, l)
            {
                return write::Outcome::Failed(why);
            }
            let sent = crate::agent::send_json(
                &app,
                &who,
                reqwest::Method::PUT,
                API_TOGGLE,
                &json!({"id": q.id, "enabled": q.on}),
            )
            .await;
            let (id, on) = (q.id, q.on);
            finish(
                &app,
                &who,
                sent,
                move |js, _| js.iter().any(|x| x.id == id && x.enabled == on),
                match (l, on) {
                    (Lang::En, true) => format!("Switched on “{}”", j.name),
                    (Lang::En, false) => format!("Switched off “{}”", j.name),
                    (Lang::Zh, true) => format!("已打开「{}」", j.name),
                    (Lang::Zh, false) => format!("已关掉「{}」", j.name),
                },
                l,
            )
            .await
        }
        .await;
        area("sch-row", l, Via::Local, "", &write::Phase::Done(o))
    })
}

/// 删除 (tier 2; the old-old page deleted without any confirm).
pub async fn del_ask(
    State(app): State<Shared>,
    Query(q): Query<RowQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let who = Caller::of(&app, &req);
    let j = match current(&app, &who, q.id, l).await {
        Ok(j) => j,
        Err(o) => return one(area("sch-row", l, ctx.remote, "", &write::Phase::Done(o))),
    };
    let c = write::Confirm {
        tier: write::Tier::Inline,
        title: match l {
            Lang::En => format!("Delete “{}”?", j.name),
            Lang::Zh => format!("删除「{}」？", j.name),
        },
        what: match l {
            Lang::En => format!(
                "“{}” is removed and won't run again. To get it back, create it again.",
                j.name
            ),
            Lang::Zh => format!("「{}」会被删掉，不再运行；要用的话只能重新创建。", j.name),
        },
        downtime: None,
        recovery: None,
        action: l.pick("Confirm: delete", "确认：删除").into(),
        cuts_uplink: false,
        word: None,
        danger: true,
        revert: crate::write::Revert::No,
    };
    let set = format!("{PATH}/del/set?id={}", j.id);
    one(area("sch-row", l, ctx.remote, &set, &write::Phase::Ask(c)))
}

pub async fn del_set(
    State(app): State<Shared>,
    Query(q): Query<RowQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let who = Caller::of(&app, &req);
    let pending = area(
        "sch-row",
        l,
        Via::Local,
        "",
        &write::Phase::Pending(l.pick("Deleting…", "正在删除…").into()),
    );
    write::respond(pending, async move {
        let sent = crate::agent::send_json(
            &app,
            &who,
            reqwest::Method::DELETE,
            API,
            &json!({"id": q.id}),
        )
        .await;
        let id = q.id;
        let o = finish(
            &app,
            &who,
            sent,
            move |js, _| !js.iter().any(|x| x.id == id),
            l.pick("Job deleted", "任务已删除").into(),
            l,
        )
        .await;
        area("sch-row", l, Via::Local, "", &write::Phase::Done(o))
    })
}

pub async fn row_cancel(
    Query(q): Query<write::CancelQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    one(area(
        "sch-row",
        ctx.lang,
        ctx.remote,
        "",
        &write::cancel_phase(&q),
    ))
}

#[cfg(test)]
mod tests;
