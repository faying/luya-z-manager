//! /sms/forward (短信转发): status, then three tabs — settings,
//! rules (add / edit / on-off / test / delete), log (retry the failed ones, clear). Batch 5a.
//!
//! Where each value comes from: the agent's `/api/sms/forward/config` and `/api/sms/forward/log`
//! with the tab's own session (agent.rs asks u60d first: it serves them once forwarding is handed
//! over), both every 30 s like the old page (which read the log only while its tab was open; here
//! the tabs are client-side, so the log comes along with the config: one more cheap read) and
//! again at once after a write on this page (`App::kick`).
//!
//! **Secrets.** A rule's destination holds credentials: the Telegram bot token, the whole webhook
//! URL (a Bark key sits in its path), the ntfy topic and token, the Discord / Slack webhook URL,
//! the webhook's custom headers. They stay on the server: never rendered, pushed, put in a URL or
//! logged. Rows and confirms name a destination by type and host only ([`target`]). The edit form
//! leaves those fields empty (「已设 · 留空不改」, like the Wi-Fi password): empty = keep, merged on
//! the server from a fresh read of the rule at send time; a destination of another type needs them
//! typed. Test and retry are built on the server from a fresh read by numeric rule id / log entry,
//! never from what the browser sends. The agent's error texts can quote a URL with the token in it
//! (reqwest's errors do), so every text from the agent goes through [`scrub`] first.
//!
//! Writes (controls-inventory.md §/sms/forward):
//! - tier 2 (in the page): save settings (`PUT /config`), rule on/off (`PUT /rules/toggle`),
//!   create / update (`POST` / `PUT /rules`), test (`POST /test {destination}`: really sends; an
//!   SMS destination costs a message), retry the failed (`POST /retry {index}` per entry, each
//!   looked up again by sms + rule right before its request, carrying on past a refusal);
//! - tier 3 (dialog): delete a rule (`DELETE /rules {id}`), clear the log (`POST /log/clear`).
//! - Read back by reading the config / log again (≤ 10 s); test and retry have no readback (an ok
//!   means the agent delivered it), the retry result comes from the log read after it.
//! - Never sent twice: no reply → 「连接中断，还没确认是否生效」, no resend.

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
use crate::write;

pub const PATH: &str = "/sms/forward";
const CFG: &str = "/api/sms/forward/config";
const LOG: &str = "/api/sms/forward/log";
const EVERY: Duration = Duration::from_secs(30);
const READBACK: Duration = Duration::from_secs(10);
pub const FILTERS: [&str; 4] = ["all", "sender", "content", "sender_and_content"];
pub const DESTS: [&str; 6] = ["telegram", "webhook", "sms", "ntfy", "discord", "slack"];
pub const METHODS: [&str; 3] = ["POST", "PUT", "GET"];
/// A secret shorter than this is not searched for in texts (it would match ordinary words).
const MIN_SECRET: usize = 4;

/// This page's routes (the page, its stream, its buttons).
pub fn routes() -> Router<Shared> {
    Router::new()
        .route(PATH, get(page))
        .route("/sms/forward/events", get(events))
        .route("/sms/forward/refresh", post(refresh))
        .route("/sms/forward/cfg/ask", post(cfg_ask))
        .route("/sms/forward/cfg/set", post(cfg_set))
        .route("/sms/forward/cfg/cancel", post(cfg_cancel))
        .route("/sms/forward/rule/form", post(rule_form))
        .route("/sms/forward/rule/close", post(rule_close))
        .route("/sms/forward/rule/ask", post(rule_ask))
        .route("/sms/forward/rule/set", post(rule_set))
        .route("/sms/forward/rule/cancel", post(rule_cancel))
        .route("/sms/forward/rules/toggle", post(toggle_ask))
        .route("/sms/forward/rules/toggle/set", post(toggle_set))
        .route("/sms/forward/rules/test", post(test_ask))
        .route("/sms/forward/rules/test/set", post(test_set))
        .route("/sms/forward/rules/del", post(del_ask))
        .route("/sms/forward/rules/del/set", post(del_set))
        .route("/sms/forward/rules/cancel", post(rules_cancel))
        .route("/sms/forward/log/retry", post(retry_ask))
        .route("/sms/forward/log/retry/set", post(retry_set))
        .route("/sms/forward/log/clear", post(clear_ask))
        .route("/sms/forward/log/clear/set", post(clear_set))
        .route("/sms/forward/log/cancel", post(log_cancel))
}

// ---- what the agent says ----

#[derive(Clone, Debug, PartialEq)]
pub struct Rule {
    pub id: u64,
    pub name: String,
    pub enabled: bool,
    /// One of [`FILTERS`] (anything else as it came).
    pub filter: String,
    pub patterns: Vec<String>,
    pub keywords: Vec<String>,
    /// The whole destination, secrets included: server side only.
    pub dest: Value,
}

impl Rule {
    pub fn dest_type(&self) -> &str {
        self.dest["type"].as_str().unwrap_or("")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub enabled: bool,
    pub poll: u64,
    pub mark_read: bool,
    pub delete_after: bool,
    pub rules: Vec<Rule>,
    /// `last_forwarded_id` (0 = none yet).
    pub last_id: u64,
}

fn strs(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(Into::into))
                .collect()
        })
        .unwrap_or_default()
}

/// `GET /api/sms/forward/config`'s data (`{config, last_forwarded_id}`).
pub fn config(v: &Value) -> Option<Config> {
    let c = v.get("config")?;
    Some(Config {
        enabled: c["enabled"].as_bool().unwrap_or(false),
        poll: c["poll_interval_secs"].as_u64().unwrap_or(0),
        mark_read: c["mark_read_after_forward"].as_bool().unwrap_or(false),
        delete_after: c["delete_after_forward"].as_bool().unwrap_or(false),
        rules: c["rules"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|r| {
                        Some(Rule {
                            id: r["id"].as_u64()?,
                            name: r["name"].as_str().unwrap_or("").into(),
                            enabled: r["enabled"].as_bool().unwrap_or(false),
                            filter: r["filter"]["type"].as_str().unwrap_or("").into(),
                            patterns: strs(&r["filter"]["patterns"]),
                            keywords: strs(&r["filter"]["keywords"]),
                            dest: r["destination"].clone(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
        last_id: v["last_forwarded_id"].as_u64().unwrap_or(0),
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// Device clock (local time labelled UTC).
    pub ts: i64,
    pub sms_id: u64,
    pub rule_id: u64,
    pub sender: String,
    pub preview: String,
    pub rule_name: String,
    pub dest_type: String,
    pub success: bool,
    pub error: String,
}

/// `GET /api/sms/forward/log`'s data, newest first (the order `{index}` counts in).
pub fn log(v: &Value) -> Option<Vec<Entry>> {
    Some(
        v.as_array()?
            .iter()
            .map(|e| Entry {
                ts: e["timestamp"].as_i64().unwrap_or(0),
                sms_id: e["sms_id"].as_u64().unwrap_or(0),
                rule_id: e["rule_id"].as_u64().unwrap_or(0),
                sender: e["sender"].as_str().unwrap_or("").into(),
                preview: e["content_preview"].as_str().unwrap_or("").into(),
                rule_name: e["rule_name"].as_str().unwrap_or("").into(),
                dest_type: e["destination_type"].as_str().unwrap_or("").into(),
                success: e["success"].as_bool().unwrap_or(false),
                error: e["error"].as_str().unwrap_or("").into(),
            })
            .collect(),
    )
}

async fn read_cfg(app: &Shared, who: &Caller) -> Result<Config, Fail> {
    let v = crate::agent::get(app, who, CFG).await?;
    config(&v).ok_or(Fail::Refused(200, String::new(), String::new()))
}

async fn read_log(app: &Shared, who: &Caller) -> Result<Vec<Entry>, Fail> {
    let v = crate::agent::get(app, who, LOG).await?;
    log(&v).ok_or(Fail::Refused(200, String::new(), String::new()))
}

// ---- secrets ----

/// The credentials in a destination (see the module doc).
pub fn dest_secrets(d: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let mut add = |v: &Value| {
        if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
            out.push(s.to_owned());
        }
    };
    match d["type"].as_str().unwrap_or("") {
        "telegram" => add(&d["bot_token"]),
        "webhook" => {
            add(&d["url"]);
            for h in d["headers"].as_array().into_iter().flatten() {
                add(&h["value"]);
            }
        }
        "ntfy" => {
            add(&d["topic"]);
            add(&d["token"]);
        }
        "discord" | "slack" => add(&d["webhook_url"]),
        _ => {}
    }
    // a URL's path and query alone too (an error may quote it without the host)
    let paths: Vec<String> = out
        .iter()
        .filter_map(|s| {
            let rest = s.split_once("://")?.1;
            rest.find('/').map(|i| rest[i..].to_owned())
        })
        .filter(|p| p.len() > 1)
        .collect();
    out.extend(paths);
    out
}

pub fn secrets(c: &Config) -> Vec<String> {
    let mut v: Vec<String> = c.rules.iter().flat_map(|r| dest_secrets(&r.dest)).collect();
    v.sort_by_key(|s| std::cmp::Reverse(s.len()));
    v.dedup();
    v
}

/// A URL's host (no user info, no port kept apart): `https://a:b@push.example.net:8443/x` →
/// `push.example.net:8443`. Empty when it is not a URL.
pub fn host(url: &str) -> String {
    let Some((_, rest)) = url.split_once("://") else {
        return String::new();
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let auth = &rest[..end];
    auth.rsplit_once('@').map_or(auth, |(_, h)| h).to_owned()
}

/// Agent text made safe to show: every URL cut to its host, then each known secret replaced by
/// 「•••」 (a token quoted outside a URL).
pub fn scrub(s: &str, secrets: &[String]) -> String {
    let mut out = cut_urls(s);
    for k in secrets {
        if k.chars().count() >= MIN_SECRET {
            out = out.replace(k.as_str(), "•••");
        }
    }
    out
}

fn cut_urls(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        let at = [rest.find("http://"), rest.find("https://")]
            .into_iter()
            .flatten()
            .min();
        let Some(i) = at else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..i]);
        let u = &rest[i..];
        let end = u
            .find(|c: char| c.is_whitespace() || "\"'<>()[]{}`,".contains(c))
            .unwrap_or(u.len());
        let url = &u[..end];
        let scheme = &url[..url.find("://").unwrap() + 3];
        let after = &url[scheme.len()..];
        let auth = after.find(['/', '?', '#']).unwrap_or(after.len());
        out.push_str(scheme);
        out.push_str(&host(url));
        if after.len() > auth + 1 {
            out.push_str("/…");
        }
        rest = &u[end..];
    }
}

// ---- words ----

pub fn filter_label(l: Lang, f: &str) -> String {
    match f {
        "all" => l.pick("All messages", "全部短信"),
        "sender" => l.pick("By sender (patterns)", "按发件人（模式）"),
        "content" => l.pick("By content (keywords)", "按内容（关键词）"),
        "sender_and_content" => l.pick("Sender + content", "发件人 + 内容"),
        _ => return f.to_owned(),
    }
    .into()
}

pub fn dest_label(l: Lang, d: &str) -> String {
    match d {
        "telegram" => "Telegram",
        "webhook" => "Webhook",
        "sms" => l.pick("SMS forward", "短信转发"),
        "ntfy" => "Ntfy",
        "discord" => "Discord",
        "slack" => "Slack",
        _ => return d.to_owned(),
    }
    .into()
}

/// Where a destination sends to, without its secrets: `POST push.example.net`, `Telegram chat
/// -100…`, the SMS number, `ntfy.sh`.
pub fn target(d: &Value) -> String {
    let s = |k: &str| d[k].as_str().unwrap_or("").to_owned();
    match d["type"].as_str().unwrap_or("") {
        "telegram" => format!("Telegram {}", s("chat_id")).trim_end().to_owned(),
        "webhook" => {
            let m = s("method");
            format!(
                "{} {}",
                if m.is_empty() { "POST".into() } else { m },
                host(&s("url"))
            )
        }
        "sms" => s("forward_number"),
        "ntfy" => format!("ntfy {}", host(&s("url"))).trim_end().to_owned(),
        "discord" => "Discord".into(),
        "slack" => "Slack".into(),
        t => t.to_owned(),
    }
}

/// "2026-09-24 08:31" of a device-clock epoch (local wall time labelled UTC: the UTC digits).
pub fn device_time(epoch: i64) -> String {
    if epoch <= 0 {
        return "—".into();
    }
    let days = epoch.div_euclid(86_400);
    let secs = epoch.rem_euclid(86_400);
    // civil from days (H. Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        secs / 3600,
        secs / 60 % 60
    )
}

// ---- the view ----

/// What the page has from the agent.
#[derive(Clone, Default)]
pub struct Got {
    pub cfg: Option<Config>,
    pub cfg_fail: Option<Fail>,
    pub cfg_at: Option<SystemTime>,
    pub log: Option<Vec<Entry>>,
    pub log_fail: Option<Fail>,
    pub log_at: Option<SystemTime>,
}

impl Got {
    pub fn update(&mut self, c: Result<Config, Fail>, g: Result<Vec<Entry>, Fail>) {
        match c {
            Ok(c) => {
                self.cfg = Some(c);
                self.cfg_fail = None;
                self.cfg_at = Some(SystemTime::now());
            }
            Err(f) => self.cfg_fail = Some(f),
        }
        match g {
            Ok(g) => {
                self.log = Some(g);
                self.log_fail = None;
                self.log_at = Some(SystemTime::now());
            }
            Err(f) => self.log_fail = Some(f),
        }
    }
}

pub struct RuleRow {
    pub id: u64,
    pub name: String,
    pub on: bool,
    pub filter: String,
    pub dest: String,
    pub target: String,
}

pub struct LogRow {
    pub time: String,
    pub sender: String,
    pub preview: String,
    pub rule: String,
    pub dest: String,
    pub ok: bool,
    pub error: String,
}

pub struct View {
    pub l: Lang,
    pub tone: &'static str,
    pub state: String,
    pub reason: Option<String>,
    pub last_id: Option<u64>,
    pub meta: Option<String>,
    pub retry: bool,
    /// Settings as read: (enabled, poll, mark read, delete after).
    pub cfg: Option<(bool, u64, bool, bool)>,
    pub stale: bool,
    pub rules: Option<Vec<RuleRow>>,
    pub log: Option<Vec<LogRow>>,
    pub log_err: Option<String>,
    pub log_meta: Option<String>,
    pub failed: usize,
}

impl View {
    pub fn symbol(&self) -> &'static str {
        match self.tone {
            "bad" => "■",
            "warn" => "▲",
            _ => "●",
        }
    }
    pub fn log_empty(&self) -> bool {
        self.log.as_ref().is_none_or(Vec::is_empty)
    }
    /// The settings form's draft signals, set only when missing (a push never resets typing).
    pub fn cfg_sigs(&self) -> String {
        let (e, p, m, d) = self.cfg.unwrap_or_default();
        json!({"_ce": e, "_cp": p.to_string(), "_cm": m, "_cd": d}).to_string()
    }
    /// The draft differs from what was read.
    pub fn cfg_dirty(&self) -> String {
        let (e, p, m, d) = self.cfg.unwrap_or_default();
        format!("$_ce !== {e} || String($_cp).trim() !== '{p}' || $_cm !== {m} || $_cd !== {d}")
    }
    /// Discard: back to what was read.
    pub fn cfg_reset(&self) -> String {
        let (e, p, m, d) = self.cfg.unwrap_or_default();
        format!("$_ce = {e}; $_cp = '{p}'; $_cm = {m}; $_cd = {d}")
    }
}

pub fn view(g: &Got, l: Lang) -> View {
    let c = g.cfg.as_ref();
    let sec = c.map(secrets).unwrap_or_default();
    let (mut tone, state, reason) = match (c, &g.cfg_fail) {
        (None, Some(f)) => (
            "bad",
            l.pick("Can't read forwarding settings", "读不到转发设置")
                .to_string(),
            Some(scrub(&f.text(l), &sec)),
        ),
        (None, None) => (
            "neutral",
            l.pick("Reading forwarding settings…", "正在读取转发设置…")
                .to_string(),
            None,
        ),
        (Some(c), _) => {
            let n = c.rules.iter().filter(|r| r.enabled).count();
            if !c.enabled {
                (
                    "neutral",
                    l.pick("Forwarding off", "转发已关").to_string(),
                    Some(
                        l.pick(
                            "Messages stay on the device. Turn it on under Settings.",
                            "短信留在设备上。在「设置」里打开。",
                        )
                        .to_string(),
                    ),
                )
            } else if n == 0 {
                (
                    "warn",
                    l.pick(
                        "Forwarding on · no rule enabled",
                        "转发已开 · 没有启用的规则",
                    )
                    .to_string(),
                    Some(
                        l.pick(
                            "Nothing is forwarded. Add or turn on a rule under Rules.",
                            "什么都不会转发。在「规则」里添加或打开一条规则。",
                        )
                        .to_string(),
                    ),
                )
            } else {
                (
                    "ok",
                    match l {
                        Lang::En => format!(
                            "Forwarding on · {n} rule{} enabled",
                            if n == 1 { "" } else { "s" }
                        ),
                        Lang::Zh => format!("转发已开 · {n} 条规则启用"),
                    },
                    Some(match l {
                        Lang::En => format!("Checks for new messages every {} s.", c.poll),
                        Lang::Zh => format!("每 {} 秒检查一次新短信。", c.poll),
                    }),
                )
            }
        }
    };
    let stale = c.is_some() && g.cfg_fail.is_some();
    let mut meta = None;
    if stale {
        tone = "stale";
        meta = Some(crate::agent::stopped(l, "Settings", "设置", g.cfg_at));
    }
    let rules = c.map(|c| {
        c.rules
            .iter()
            .map(|r| RuleRow {
                id: r.id,
                name: r.name.clone(),
                on: r.enabled,
                filter: filter_label(l, &r.filter),
                dest: dest_label(l, r.dest_type()),
                target: target(&r.dest),
            })
            .collect()
    });
    let log_rows = g.log.as_ref().map(|v| {
        v.iter()
            .map(|e| LogRow {
                time: device_time(e.ts),
                sender: e.sender.clone(),
                preview: e.preview.clone(),
                rule: e.rule_name.clone(),
                dest: if e.dest_type.is_empty() {
                    "—".into()
                } else {
                    dest_label(l, &e.dest_type)
                },
                ok: e.success,
                error: scrub(&e.error, &sec),
            })
            .collect()
    });
    let log_err = match (&g.log, &g.log_fail) {
        (None, Some(f)) => Some(match l {
            Lang::En => format!("Couldn't read the log: {}", scrub(&f.text(l), &sec)),
            Lang::Zh => format!("读不到日志：{}", scrub(&f.text(l), &sec)),
        }),
        _ => None,
    };
    let log_meta = (g.log.is_some() && g.log_fail.is_some())
        .then(|| crate::agent::stopped(l, "Log", "日志", g.log_at));
    View {
        l,
        tone,
        state,
        reason,
        last_id: c.map(|c| c.last_id).filter(|n| *n > 0),
        meta,
        retry: g.cfg_fail.is_some(),
        cfg: c.map(|c| (c.enabled, c.poll, c.mark_read, c.delete_after)),
        stale,
        rules,
        failed: g
            .log
            .as_ref()
            .map_or(0, |v| v.iter().filter(|e| !e.success).count()),
        log: log_rows,
        log_err,
        log_meta,
    }
}

#[derive(Template)]
#[template(path = "sms_forward_leaves.html")]
struct Leaves<'a> {
    v: &'a View,
}

fn leaves(g: &Got, l: Lang) -> Vec<(String, String)> {
    split_leaves(&Leaves { v: &view(g, l) }.render().unwrap())
}

// ---- writes: the agent, once ----

/// How an agent write went.
enum Sent {
    Ok,
    /// No answer: it may have happened.
    NoReply,
}

async fn send(
    app: &Shared,
    who: &Caller,
    method: reqwest::Method,
    path: &str,
    body: &Value,
    sec: &[String],
    l: Lang,
) -> Result<Sent, write::Outcome> {
    let r = if method == reqwest::Method::POST {
        crate::agent::post(app, who, path, body).await
    } else {
        crate::agent::send_json(app, who, method, path, body).await
    };
    match r {
        Ok(_) => Ok(Sent::Ok),
        Err(Fail::Unreachable) => Ok(Sent::NoReply),
        Err(f) => Err(write::Outcome::Failed(scrub(&f.text(l), sec))),
    }
}

/// Send, then read the config again until `check` holds.
async fn write_cfg(
    app: &Shared,
    who: &Caller,
    method: reqwest::Method,
    path: &str,
    body: &Value,
    sec: &[String],
    applied: String,
    check: impl Fn(&Config) -> bool,
    l: Lang,
) -> write::Outcome {
    let sent = match send(app, who, method, path, body, sec, l).await {
        Ok(s) => s,
        Err(o) => return o,
    };
    let held = write::read_back(
        &app.hub,
        READBACK,
        Duration::from_secs(1),
        Duration::from_secs(2),
        {
            let check = &check;
            move || async move { read_cfg(app, who).await.is_ok_and(|c| check(&c)) }
        },
    )
    .await;
    let _ = app.kick.send(PATH);
    match (held, sent) {
        (true, _) => write::Outcome::Applied(applied),
        (false, Sent::Ok) => write::Outcome::Accepted,
        (false, Sent::NoReply) => write::Outcome::Unknown,
    }
}

fn not_read(l: Lang, f: &Fail) -> write::Outcome {
    write::Outcome::NotYet(match l {
        Lang::En => format!(
            "Can't read the forwarding settings now ({}); try again",
            f.text(l)
        ),
        Lang::Zh => format!("现在读不到转发设置（{}），稍后再试", f.text(l)),
    })
}

fn gone(l: Lang) -> write::Outcome {
    write::Outcome::NotYet(
        l.pick("That rule is not there any more", "这条规则已经不在了")
            .into(),
    )
}

fn expired(l: Lang) -> write::Outcome {
    write::Outcome::NotYet(
        l.pick("The form expired; press it again", "表单已过期，请再按一次")
            .into(),
    )
}

// ---- settings ----

#[derive(Deserialize, Default)]
pub struct CfgIn {
    #[serde(default, rename = "_ce")]
    pub enabled: bool,
    #[serde(default, rename = "_cp")]
    pub poll: Value,
    #[serde(default, rename = "_cm")]
    pub mark_read: bool,
    #[serde(default, rename = "_cd")]
    pub delete_after: bool,
}

/// The settings to send, or why not (the agent wants 10 s or more; the old page also capped 3600).
pub fn cfg_check(f: &CfgIn, l: Lang) -> Result<Value, String> {
    let n = match &f.poll {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse::<u64>().ok(),
        _ => None,
    };
    let Some(n) = n.filter(|n| (10..=3600).contains(n)) else {
        return Err(l
            .pick(
                "Poll interval: a whole number of seconds from 10 to 3600.",
                "轮询间隔：10 到 3600 之间的整数秒。",
            )
            .into());
    };
    Ok(json!({
        "enabled": f.enabled,
        "poll_interval_secs": n,
        "mark_read_after_forward": f.mark_read,
        "delete_after_forward": f.delete_after,
    }))
}

pub fn cfg_confirm(b: &Value, l: Lang) -> write::Confirm {
    let n = b["poll_interval_secs"].as_u64().unwrap_or(0);
    let mut parts = vec![if b["enabled"] == true {
        l.pick(
            "New messages are forwarded by the enabled rules",
            "启用的规则会转发新短信",
        )
        .to_string()
    } else {
        l.pick(
            "Nothing is forwarded until you turn it back on",
            "在重新打开之前什么都不转发",
        )
        .to_string()
    }];
    parts.push(match l {
        Lang::En => format!("checked every {n} s"),
        Lang::Zh => format!("每 {n} 秒检查一次"),
    });
    if b["mark_read_after_forward"] == true {
        parts.push(
            l.pick("forwarded messages are marked read", "转发过的短信标为已读")
                .into(),
        );
    }
    let mut what = parts.join(l.pick(", ", "，")) + l.pick(".", "。");
    if b["delete_after_forward"] == true {
        what.push(' ');
        what.push_str(l.pick(
            "Every message that is forwarded successfully is then deleted from the device; it can't be recovered there.",
            "每条转发成功的短信随后会从设备上删除，删了在设备上找不回来。",
        ));
    }
    write::Confirm {
        tier: write::Tier::Inline,
        title: l.pick("Save Settings", "保存设置").into(),
        what,
        downtime: None,
        recovery: None,
        action: l.pick("Confirm: save settings", "确认：保存设置").into(),
        cuts_uplink: false,
        word: None,
        danger: b["delete_after_forward"] == true,
        revert: crate::write::Revert::No,
    }
}

fn cfg_same(c: &Config, b: &Value) -> bool {
    b["enabled"] == c.enabled
        && b["poll_interval_secs"] == c.poll
        && b["mark_read_after_forward"] == c.mark_read
        && b["delete_after_forward"] == c.delete_after
}

// ---- a rule ----

/// The rule form's signals (`_r…`). Secret fields come only from here and go only to the agent.
#[derive(Deserialize, Default, Clone)]
pub struct RuleIn {
    /// The rule edited (absent / negative: a new one).
    #[serde(default, rename = "_rid")]
    pub id: Option<i64>,
    #[serde(default, rename = "_rn")]
    pub name: String,
    #[serde(default = "yes", rename = "_re")]
    pub enabled: bool,
    #[serde(default, rename = "_rf")]
    pub filter: String,
    #[serde(default, rename = "_rp")]
    pub patterns: String,
    #[serde(default, rename = "_rk")]
    pub keywords: String,
    #[serde(default, rename = "_rd")]
    pub dest: String,
    #[serde(default, rename = "_rbt")]
    pub bot_token: String,
    #[serde(default, rename = "_rci")]
    pub chat_id: String,
    #[serde(default, rename = "_rsi")]
    pub silent: bool,
    #[serde(default, rename = "_rwu")]
    pub url: String,
    #[serde(default, rename = "_rwm")]
    pub method: String,
    #[serde(default, rename = "_rsn")]
    pub number: String,
    #[serde(default, rename = "_rnu")]
    pub ntfy_url: String,
    #[serde(default, rename = "_rnt")]
    pub topic: String,
    #[serde(default, rename = "_rnk")]
    pub token: String,
    /// Drop the stored ntfy token (an empty field keeps it).
    #[serde(default, rename = "_rnc")]
    pub clear_token: bool,
    #[serde(default, rename = "_rhu")]
    pub hook: String,
}

fn yes() -> bool {
    true
}

fn split(s: &str) -> Vec<String> {
    s.split([',', '，'])
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(Into::into)
        .collect()
}

/// A Telegram chat: a numeric id (groups and channels are negative) or a public `@username`.
fn is_chat_id(s: &str) -> bool {
    let d = s.strip_prefix('-').unwrap_or(s);
    if !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    s.strip_prefix('@').is_some_and(|u| {
        !u.is_empty() && u.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    })
}

/// A number to text: digits (short codes too), an optional leading `+`; spaces and dashes allowed.
fn is_number(s: &str) -> bool {
    let d: String = s
        .strip_prefix('+')
        .unwrap_or(s)
        .chars()
        .filter(|c| *c != ' ' && *c != '-')
        .collect();
    (1..=20).contains(&d.len()) && d.bytes().all(|b| b.is_ascii_digit())
}

fn is_url(s: &str) -> bool {
    (s.starts_with("https://") || s.starts_with("http://")) && !host(s).is_empty()
}

/// The rule's body for the agent (`{name, enabled, filter, destination}`, plus `id` for an
/// update), or what is missing. `cur`: the rule as stored now (an update), whose secrets fill the
/// ones left empty when the destination keeps its type.
pub fn rule_check(f: &RuleIn, cur: Option<&Rule>, l: Lang) -> Result<Value, String> {
    let name = f.name.trim();
    if name.is_empty() {
        return Err(l.pick("Give the rule a name.", "给规则起个名字。").into());
    }
    if !FILTERS.contains(&f.filter.as_str()) {
        return Err(l.pick("Choose a filter.", "选择筛选条件。").into());
    }
    let (p, k) = (split(&f.patterns), split(&f.keywords));
    let filter = match f.filter.as_str() {
        "all" => json!({"type": "all"}),
        "sender" | "sender_and_content" if p.is_empty() => {
            return Err(l
                .pick(
                    "Enter at least one sender pattern.",
                    "至少填一个发件人模式。",
                )
                .into());
        }
        "content" | "sender_and_content" if k.is_empty() => {
            return Err(l
                .pick("Enter at least one keyword.", "至少填一个关键词。")
                .into());
        }
        "sender" => json!({"type": "sender", "patterns": p}),
        "content" => json!({"type": "content", "keywords": k}),
        _ => json!({"type": "sender_and_content", "patterns": p, "keywords": k}),
    };
    if !DESTS.contains(&f.dest.as_str()) {
        return Err(l.pick("Choose a destination.", "选择转发目标。").into());
    }
    // the stored destination, when it is of the same type (its secrets may be kept)
    let old = cur
        .map(|r| &r.dest)
        .filter(|d| d["type"] == f.dest.as_str());
    let keep = |typed: &str, key: &str| -> String {
        let t = typed.trim();
        if t.is_empty() {
            old.and_then(|d| d[key].as_str()).unwrap_or("").to_owned()
        } else {
            t.to_owned()
        }
    };
    let missing = |en: &str, zh: &str| -> String {
        match l {
            Lang::En => format!("Fill in {en}."),
            _ => format!("把{zh}填上。"),
        }
    };
    let bad_url = || -> String {
        l.pick(
            "A URL starts with https:// (or http://) and a host.",
            "网址要以 https://（或 http://）开头并带主机名。",
        )
        .into()
    };
    let dest = match f.dest.as_str() {
        "telegram" => {
            let bt = keep(&f.bot_token, "bot_token");
            let ci = f.chat_id.trim();
            if bt.is_empty() {
                return Err(missing("the Bot Token", " Bot Token "));
            }
            if ci.is_empty() {
                return Err(missing("the Chat ID", " Chat ID "));
            }
            if !is_chat_id(ci) {
                return Err(l
                    .pick(
                        "A Chat ID is a number (like -100123456) or @channel.",
                        "Chat ID 是数字（像 -100123456）或 @频道名。",
                    )
                    .into());
            }
            json!({"type": "telegram", "bot_token": bt, "chat_id": ci, "silent": f.silent})
        }
        "webhook" => {
            let url = keep(&f.url, "url");
            if url.is_empty() {
                return Err(missing("the URL", "网址"));
            }
            if !is_url(&url) {
                return Err(bad_url());
            }
            let m = if METHODS.contains(&f.method.as_str()) {
                f.method.as_str()
            } else {
                "POST"
            };
            // the rule's custom headers are kept as they are (the form has no field for them)
            let headers = old.map_or_else(|| json!([]), |d| d["headers"].clone());
            let headers = if headers.is_array() {
                headers
            } else {
                json!([])
            };
            json!({"type": "webhook", "url": url, "method": m, "headers": headers})
        }
        "sms" => {
            let n = f.number.trim();
            if n.is_empty() {
                return Err(missing("the number to forward to", "转发到的号码"));
            }
            if !is_number(n) {
                return Err(l
                    .pick(
                        "A phone number is digits, with an optional + in front.",
                        "号码只能是数字，前面可以带 +。",
                    )
                    .into());
            }
            json!({"type": "sms", "forward_number": n})
        }
        "ntfy" => {
            let u = f.ntfy_url.trim();
            let topic = keep(&f.topic, "topic");
            if u.is_empty() {
                return Err(missing("the server URL", "服务器地址"));
            }
            if topic.is_empty() {
                return Err(missing("the topic", "主题"));
            }
            if !is_url(u) {
                return Err(bad_url());
            }
            let mut d = json!({"type": "ntfy", "url": u, "topic": topic});
            let tk = if f.clear_token && f.token.trim().is_empty() {
                String::new()
            } else {
                keep(&f.token, "token")
            };
            if !tk.is_empty() {
                d["token"] = json!(tk);
            }
            d
        }
        t => {
            let w = keep(&f.hook, "webhook_url");
            if w.is_empty() {
                return Err(missing("the Webhook URL", " Webhook 地址"));
            }
            if !is_url(&w) {
                return Err(bad_url());
            }
            json!({"type": t, "webhook_url": w})
        }
    };
    let mut b = json!({"name": name, "enabled": f.enabled, "filter": filter, "destination": dest});
    if let Some(r) = cur {
        b["id"] = json!(r.id);
    }
    Ok(b)
}

/// The stored rule matches what was sent.
fn rule_is(r: &Rule, b: &Value) -> bool {
    r.name == b["name"].as_str().unwrap_or("")
        && b["enabled"] == r.enabled
        && b["filter"]["type"] == r.filter.as_str()
        && strs(&b["filter"]["patterns"]) == r.patterns
        && strs(&b["filter"]["keywords"]) == r.keywords
        && same_dest(&r.dest, &b["destination"])
}

/// Same destination, the agent's defaults filled in (`silent` false, `method` POST, `headers` []).
fn same_dest(a: &Value, b: &Value) -> bool {
    let norm = |d: &Value| {
        let mut d = d.clone();
        if let Some(o) = d.as_object_mut() {
            match o.get("type").and_then(Value::as_str) {
                Some("telegram") => {
                    o.entry("silent").or_insert(json!(false));
                }
                Some("webhook") => {
                    o.entry("method").or_insert(json!("POST"));
                    o.entry("headers").or_insert(json!([]));
                }
                _ => {}
            }
            o.retain(|_, v| !v.is_null());
        }
        d
    };
    norm(a) == norm(b)
}

pub fn rule_confirm(b: &Value, edit: bool, l: Lang) -> write::Confirm {
    let ft = b["filter"]["type"].as_str().unwrap_or("");
    let t = target(&b["destination"]);
    let off = b["enabled"] != true;
    let what = match l {
        Lang::En => format!(
            "{} go to {t}{}.",
            match ft {
                "all" => "All messages".to_string(),
                _ => format!("Messages matching “{}”", filter_label(l, ft)),
            },
            if off {
                " once the rule is turned on"
            } else {
                ""
            }
        ),
        Lang::Zh => format!(
            "{}{}转发到 {t}。",
            match ft {
                "all" => "全部短信".to_string(),
                _ => format!("符合「{}」的短信", filter_label(l, ft)),
            },
            if off { "在规则打开后" } else { "" }
        ),
    };
    let name = b["name"].as_str().unwrap_or("");
    write::Confirm {
        tier: write::Tier::Inline,
        title: if edit {
            l.pick("Edit Rule", "编辑规则")
        } else {
            l.pick("New Rule", "新规则")
        }
        .into(),
        what,
        downtime: None,
        recovery: None,
        action: match (l, edit) {
            (Lang::En, false) => format!("Confirm: create rule {name}"),
            (Lang::En, true) => format!("Confirm: update rule {name}"),
            (Lang::Zh, false) => format!("确认：创建规则「{name}」"),
            (Lang::Zh, true) => format!("确认：更新规则「{name}」"),
        },
        cuts_uplink: false,
        word: None,
        danger: false,
        revert: crate::write::Revert::No,
    }
}

// ---- checked forms waiting for their confirm (secrets included: server side, 10 min) ----

#[derive(Clone)]
enum Held {
    Cfg(Value),
    Rule { body: Value, edit: Option<u64> },
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

// ---- the page and its stream ----

pub struct Poll {
    l: Lang,
    who: Caller,
    got: Got,
}

impl Polled for Poll {
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        let (c, g) = tokio::join!(read_cfg(app, &self.who), read_log(app, &self.who));
        self.got.update(c, g);
        (leaves(&self.got, self.l), EVERY)
    }
}

#[derive(Template)]
#[template(path = "sms_forward.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    leaves: Vec<(String, String)>,
    cfg_op: String,
    form_op: String,
    rules_op: String,
    log_op: String,
}

impl Page {
    /// (key, label, selected first)
    fn tabs(&self) -> Vec<(&'static str, &'static str, bool)> {
        let l = self.l;
        vec![
            ("cfg", l.pick("Settings", "设置"), true),
            ("rules", l.pick("Rules", "规则"), false),
            ("log", l.pick("Log", "日志"), false),
        ]
    }
    fn leaf(&self, id: &str) -> &str {
        self.leaves
            .iter()
            .find(|(k, _)| k == id)
            .map_or("", |(_, h)| h.as_str())
    }
}

/// The write areas: `fwd-cfg`, `fwd-rule` (the form), `fwd-rules` (a row's buttons), `fwd-log`.
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
        leaves: leaves(&Got::default(), l),
        cfg_op: area("fwd-cfg", l, ctx.remote, "", &idle),
        form_op: area("fwd-rule", l, ctx.remote, "", &idle),
        rules_op: area("fwd-rules", l, ctx.remote, "", &idle),
        log_op: area("fwd-log", l, ctx.remote, "", &idle),
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
        got: Got::default(),
    };
    crate::agent::stream(app, ctx, q.g, PATH, p)
}

/// 刷新 / 重试: read again now.
pub async fn refresh(State(app): State<Shared>) -> axum::http::StatusCode {
    let _ = app.kick.send(PATH);
    axum::http::StatusCode::NO_CONTENT
}

fn one(html: String) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(futures_util::stream::once(async move {
        Ok(crate::pages::patch(&html))
    }))
}

async fn body(req: axum::extract::Request) -> Vec<u8> {
    axum::body::to_bytes(req.into_body(), 16 * 1024)
        .await
        .map(|b| b.to_vec())
        .unwrap_or_default()
}

/// Who, the language, remote, from a request.
fn who(app: &Shared, req: &axum::extract::Request) -> (Caller, Lang, Via) {
    let ctx = crate::topbar::Ctx::of_request(req, PATH);
    (Caller::of(app, req), ctx.lang, ctx.remote)
}

fn cancel_of(id: &'static str, q: write::CancelQuery, req: axum::extract::Request) -> String {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    area(id, ctx.lang, ctx.remote, "", &write::cancel_phase(&q))
}

pub async fn cfg_cancel(
    Query(q): Query<write::CancelQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    one(cancel_of("fwd-cfg", q, req))
}
pub async fn rule_cancel(
    Query(q): Query<write::CancelQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    one(cancel_of("fwd-rule", q, req))
}
pub async fn rules_cancel(
    Query(q): Query<write::CancelQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    one(cancel_of("fwd-rules", q, req))
}
pub async fn log_cancel(
    Query(q): Query<write::CancelQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    one(cancel_of("fwd-log", q, req))
}

/// A write's reply: "正在…", then `done` (the area's result, and whatever else it patches).
fn respond<F>(pending: String, done: F) -> Sse<impl Stream<Item = Result<Event, Infallible>>>
where
    F: std::future::Future<Output = String> + Send + 'static,
{
    write::respond(pending, done)
}

fn pending(id: &str, l: Lang, en: &'static str, zh: &'static str) -> String {
    area(
        id,
        l,
        Via::Local,
        "",
        &write::Phase::Pending(l.pick(en, zh).into()),
    )
}

fn done(id: &str, l: Lang, o: write::Outcome) -> String {
    area(id, l, Via::Local, "", &write::Phase::Done(o))
}

// settings

pub async fn cfg_ask(State(app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    let (_, l, remote) = who(&app, &req);
    let f: CfgIn = serde_json::from_slice(&body(req).await).unwrap_or_default();
    let html = match cfg_check(&f, l) {
        Err(e) => done("fwd-cfg", l, write::Outcome::NotYet(e)),
        Ok(b) => {
            let c = cfg_confirm(&b, l);
            let set = format!("{PATH}/cfg/set?t={}", put(Held::Cfg(b)));
            area("fwd-cfg", l, remote, &set, &write::Phase::Ask(c))
        }
    };
    one(html)
}

#[derive(Deserialize, Default)]
pub struct TokenQuery {
    #[serde(default)]
    t: String,
}

pub async fn cfg_set(
    State(app): State<Shared>,
    Query(q): Query<TokenQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let (who, l, _) = who(&app, &req);
    let held = take(&q.t);
    respond(
        pending("fwd-cfg", l, "Saving settings…", "正在保存设置…"),
        async move {
            let Some(Held::Cfg(b)) = held else {
                return done("fwd-cfg", l, expired(l));
            };
            let b2 = b.clone();
            let o = write_cfg(
                &app,
                &who,
                reqwest::Method::PUT,
                CFG,
                &b,
                &[],
                l.pick("Settings saved", "设置已保存").into(),
                move |c| cfg_same(c, &b2),
                l,
            )
            .await;
            done("fwd-cfg", l, o)
        },
    )
}

// the rule form

#[derive(Template)]
#[template(path = "sms_forward_form.html")]
struct Form {
    l: Lang,
    open: bool,
    edit: bool,
    sigs: String,
    /// The stored destination's type when editing, and which of its secrets are set.
    old_type: String,
    has: Vec<&'static str>,
    /// The kept destination, by type and host ("POST push.example.net").
    old_target: String,
    headers: usize,
}

impl Form {
    /// The kept-secret placeholder text.
    fn kept(&self) -> &'static str {
        self.l.pick("Set · leave empty to keep it", "已设 · 留空不改")
    }
    /// The stored rule has `key` under `ty` (a field may be left empty).
    fn has_secret(&self, ty: &str, key: &str) -> bool {
        self.old_type == ty && self.has.contains(&key)
    }
    /// Placeholder of a secret field: 「已设 · 留空不改」 when the stored rule has it.
    fn ph(&self, ty: &str, key: &str, fallback: &'static str) -> String {
        if self.has_secret(ty, key) {
            self.kept().into()
        } else {
            fallback.into()
        }
    }
}

fn closed_form(l: Lang) -> String {
    Form {
        l,
        open: false,
        edit: false,
        sigs: String::new(),
        old_type: String::new(),
        has: vec![],
        old_target: String::new(),
        headers: 0,
    }
    .render()
    .unwrap()
}

/// The open form for `r` (None: a new rule). No secret goes into it.
pub fn open_form(r: Option<&Rule>, l: Lang) -> String {
    let s = |k: &str| r.and_then(|r| r.dest[k].as_str()).unwrap_or("").to_owned();
    let ty = r.map_or("telegram".to_owned(), |r| r.dest_type().to_owned());
    let sigs = json!({
        "_rid": r.map_or(-1, |r| r.id as i64),
        "_rn": r.map_or("", |r| r.name.as_str()),
        "_re": r.is_none_or(|r| r.enabled),
        "_rf": r.map_or("all", |r| r.filter.as_str()),
        "_rp": r.map(|r| r.patterns.join(", ")).unwrap_or_default(),
        "_rk": r.map(|r| r.keywords.join(", ")).unwrap_or_default(),
        "_rd": ty,
        "_rbt": "",
        "_rci": if ty == "telegram" { s("chat_id") } else { String::new() },
        "_rsi": r.is_some_and(|r| r.dest["silent"] == true),
        "_rwu": "",
        "_rwm": if ty == "webhook" && !s("method").is_empty() { s("method") } else { "POST".into() },
        "_rsn": if ty == "sms" { s("forward_number") } else { String::new() },
        "_rnu": if ty == "ntfy" { s("url") } else { "https://ntfy.sh".into() },
        "_rnt": "",
        "_rnk": "",
        "_rnc": false,
        "_rhu": "",
    });
    let has = ["bot_token", "url", "topic", "token", "webhook_url"]
        .into_iter()
        .filter(|k| !s(k).is_empty())
        .collect();
    Form {
        l,
        open: true,
        edit: r.is_some(),
        sigs: sigs.to_string(),
        old_type: if r.is_some() { ty } else { String::new() },
        has,
        old_target: r.map(|r| target(&r.dest)).unwrap_or_default(),
        headers: r.map_or(0, |r| r.dest["headers"].as_array().map_or(0, Vec::len)),
    }
    .render()
    .unwrap()
}

#[derive(Deserialize, Default)]
pub struct IdQuery {
    #[serde(default)]
    id: Option<u64>,
}

pub async fn rule_form(
    State(app): State<Shared>,
    Query(q): Query<IdQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let (who, l, remote) = who(&app, &req);
    let idle = area("fwd-rule", l, remote, "", &write::Phase::Idle);
    let html = match q.id {
        None => format!("{}\n{idle}", open_form(None, l)),
        Some(id) => match read_cfg(&app, &who).await {
            Err(f) => done("fwd-rule", l, not_read(l, &f)),
            Ok(c) => match c.rules.iter().find(|r| r.id == id) {
                None => done("fwd-rule", l, gone(l)),
                Some(r) => format!("{}\n{idle}", open_form(Some(r), l)),
            },
        },
    };
    one(html)
}

pub async fn rule_close(req: axum::extract::Request) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    one(format!(
        "{}\n{}",
        closed_form(ctx.lang),
        area("fwd-rule", ctx.lang, ctx.remote, "", &write::Phase::Idle)
    ))
}

pub async fn rule_ask(State(app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    let (who, l, remote) = who(&app, &req);
    let f: RuleIn = serde_json::from_slice(&body(req).await).unwrap_or_default();
    let edit = f.id.and_then(|i| u64::try_from(i).ok());
    let cur = match edit {
        None => Ok(None),
        Some(id) => match read_cfg(&app, &who).await {
            Err(e) => Err(not_read(l, &e)),
            Ok(c) => c
                .rules
                .into_iter()
                .find(|r| r.id == id)
                .map(Some)
                .ok_or_else(|| gone(l)),
        },
    };
    let html = match cur {
        Err(o) => done("fwd-rule", l, o),
        Ok(cur) => match rule_check(&f, cur.as_ref(), l) {
            Err(e) => done("fwd-rule", l, write::Outcome::NotYet(e)),
            Ok(b) => {
                let c = rule_confirm(&b, edit.is_some(), l);
                let set = format!("{PATH}/rule/set?t={}", put(Held::Rule { body: b, edit }));
                area("fwd-rule", l, remote, &set, &write::Phase::Ask(c))
            }
        },
    };
    one(html)
}

pub async fn rule_set(
    State(app): State<Shared>,
    Query(q): Query<TokenQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let (who, l, _) = who(&app, &req);
    let held = take(&q.t);
    respond(
        pending("fwd-rule", l, "Saving the rule…", "正在保存规则…"),
        async move {
            let Some(Held::Rule { body: b, edit }) = held else {
                return done("fwd-rule", l, expired(l));
            };
            let (method, before) = match edit {
                Some(_) => (reqwest::Method::PUT, vec![]),
                None => (
                    reqwest::Method::POST,
                    match read_cfg(&app, &who).await {
                        Ok(c) => c.rules.iter().map(|r| r.id).collect(),
                        Err(f) => return done("fwd-rule", l, not_read(l, &f)),
                    },
                ),
            };
            let sec = dest_secrets(&b["destination"]);
            let b2 = b.clone();
            let o = write_cfg(
                &app,
                &who,
                method,
                "/api/sms/forward/rules",
                &b,
                &sec,
                match edit {
                    Some(_) => l.pick("Rule updated", "规则已更新"),
                    None => l.pick("Rule created", "规则已创建"),
                }
                .into(),
                move |c| match edit {
                    Some(id) => c.rules.iter().any(|r| r.id == id && rule_is(r, &b2)),
                    None => c
                        .rules
                        .iter()
                        .any(|r| !before.contains(&r.id) && rule_is(r, &b2)),
                },
                l,
            )
            .await;
            let close = if matches!(o, write::Outcome::Applied(_)) {
                closed_form(l) + "\n"
            } else {
                String::new()
            };
            format!("{close}{}", done("fwd-rule", l, o))
        },
    )
}

// a row's buttons: on/off, test, delete

async fn find_rule(
    app: &Shared,
    who: &Caller,
    id: Option<u64>,
    l: Lang,
) -> Result<(Rule, Config), write::Outcome> {
    let c = read_cfg(app, who).await.map_err(|f| not_read(l, &f))?;
    let r = id
        .and_then(|id| c.rules.iter().find(|r| r.id == id).cloned())
        .ok_or_else(|| gone(l))?;
    Ok((r, c))
}

pub fn toggle_confirm(r: &Rule, on: bool, l: Lang) -> write::Confirm {
    let t = target(&r.dest);
    write::Confirm {
        tier: write::Tier::Inline,
        title: r.name.clone(),
        what: match (l, on) {
            (Lang::En, true) => format!("Matching messages start going to {t}."),
            (Lang::En, false) => format!("Messages stop going to {t} through this rule."),
            (Lang::Zh, true) => format!("符合条件的短信开始转发到 {t}。"),
            (Lang::Zh, false) => format!("这条规则不再把短信转发到 {t}。"),
        },
        downtime: None,
        recovery: None,
        action: match (l, on) {
            (Lang::En, true) => format!("Confirm: turn rule {} on", r.name),
            (Lang::En, false) => format!("Confirm: turn rule {} off", r.name),
            (Lang::Zh, true) => format!("确认：打开规则「{}」", r.name),
            (Lang::Zh, false) => format!("确认：关掉规则「{}」", r.name),
        },
        cuts_uplink: false,
        word: None,
        danger: false,
        revert: crate::write::Revert::No,
    }
}

pub fn test_confirm(r: &Rule, l: Lang) -> write::Confirm {
    let t = target(&r.dest);
    write::Confirm {
        tier: write::Tier::Inline,
        title: l.pick("Test rule", "测试规则").into(),
        what: match (l, r.dest_type() == "sms") {
            (Lang::En, true) => format!(
                "The device sends a test SMS to {t} over the mobile network; your carrier may charge for it."
            ),
            (Lang::Zh, true) => format!("设备会经移动网络给 {t} 发一条测试短信，运营商可能收费。"),
            (Lang::En, false) => format!("The device sends a real test message to {t} now."),
            (Lang::Zh, false) => format!("设备现在就给 {t} 发一条真实的测试消息。"),
        },
        downtime: None,
        recovery: None,
        action: match l {
            Lang::En => format!("Confirm: send a test to {t}"),
            Lang::Zh => format!("确认：给 {t} 发测试"),
        },
        cuts_uplink: false,
        word: None,
        danger: false,
        revert: crate::write::Revert::No,
    }
}

pub fn del_confirm(r: &Rule, l: Lang) -> write::Confirm {
    let t = target(&r.dest);
    write::Confirm {
        tier: write::Tier::Dialog,
        title: match l {
            Lang::En => format!("Delete rule “{}”?", r.name),
            Lang::Zh => format!("删除规则「{}」？", r.name),
        },
        what: match l {
            Lang::En => format!(
                "Messages stop going to {t} through this rule. The rule's settings, its token or URL included, are gone."
            ),
            Lang::Zh => {
                format!("这条规则不再把短信转发到 {t}。规则的设置（含令牌或网址）都会删掉。")
            }
        },
        downtime: None,
        recovery: Some(
            l.pick(
                "Create it again under Rules (you need its token or URL again).",
                "在「规则」里重新创建（要重新填令牌或网址）。",
            )
            .into(),
        ),
        action: match l {
            Lang::En => format!("Delete rule {}", r.name),
            Lang::Zh => format!("删除规则「{}」", r.name),
        },
        cuts_uplink: false,
        word: None,
        danger: true,
        revert: crate::write::Revert::No,
    }
}

/// Which button of a row (`/rules/{toggle,test,del}`).
#[derive(Clone, Copy)]
enum RowAct {
    Toggle,
    Test,
    Del,
}

async fn row_ask(app: Shared, req: axum::extract::Request, id: Option<u64>, act: RowAct) -> String {
    let (who, l, remote) = who(&app, &req);
    match find_rule(&app, &who, id, l).await {
        Err(o) => done("fwd-rules", l, o),
        Ok((r, _)) => {
            let (c, set) = match act {
                RowAct::Toggle => (
                    toggle_confirm(&r, !r.enabled, l),
                    format!("{PATH}/rules/toggle/set?id={}&on={}", r.id, !r.enabled),
                ),
                RowAct::Test => (
                    test_confirm(&r, l),
                    format!("{PATH}/rules/test/set?id={}", r.id),
                ),
                RowAct::Del => (
                    del_confirm(&r, l),
                    format!("{PATH}/rules/del/set?id={}", r.id),
                ),
            };
            area("fwd-rules", l, remote, &set, &write::Phase::Ask(c))
        }
    }
}

pub async fn toggle_ask(
    State(app): State<Shared>,
    Query(q): Query<IdQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    one(row_ask(app, req, q.id, RowAct::Toggle).await)
}
pub async fn test_ask(
    State(app): State<Shared>,
    Query(q): Query<IdQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    one(row_ask(app, req, q.id, RowAct::Test).await)
}
pub async fn del_ask(
    State(app): State<Shared>,
    Query(q): Query<IdQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    one(row_ask(app, req, q.id, RowAct::Del).await)
}

#[derive(Deserialize, Default)]
pub struct OnQuery {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    on: bool,
}

pub async fn toggle_set(
    State(app): State<Shared>,
    Query(q): Query<OnQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let (who, l, _) = who(&app, &req);
    respond(
        pending("fwd-rules", l, "Switching the rule…", "正在切换规则…"),
        async move {
            let r = match find_rule(&app, &who, q.id, l).await {
                Ok((r, _)) => r,
                Err(o) => return done("fwd-rules", l, o),
            };
            let (id, on) = (r.id, q.on);
            let o = write_cfg(
                &app,
                &who,
                reqwest::Method::PUT,
                "/api/sms/forward/rules/toggle",
                &json!({"id": id, "enabled": on}),
                &dest_secrets(&r.dest),
                match (l, on) {
                    (Lang::En, true) => format!("Rule {} is on", r.name),
                    (Lang::En, false) => format!("Rule {} is off", r.name),
                    (Lang::Zh, true) => format!("规则「{}」已打开", r.name),
                    (Lang::Zh, false) => format!("规则「{}」已关掉", r.name),
                },
                move |c| c.rules.iter().any(|r| r.id == id && r.enabled == on),
                l,
            )
            .await;
            done("fwd-rules", l, o)
        },
    )
}

pub async fn test_set(
    State(app): State<Shared>,
    Query(q): Query<IdQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let (who, l, _) = who(&app, &req);
    respond(
        pending("fwd-rules", l, "Sending a test…", "正在发送测试…"),
        async move {
            // the destination from a fresh read, never from the browser
            let (r, c) = match find_rule(&app, &who, q.id, l).await {
                Ok(x) => x,
                Err(o) => return done("fwd-rules", l, o),
            };
            let sec = secrets(&c);
            let t = target(&r.dest);
            let o = match send(
                &app,
                &who,
                reqwest::Method::POST,
                "/api/sms/forward/test",
                &json!({"destination": r.dest}),
                &sec,
                l,
            )
            .await
            {
                Err(o) => o,
                Ok(Sent::NoReply) => write::Outcome::Unknown,
                Ok(Sent::Ok) => write::Outcome::Applied(match l {
                    Lang::En => format!("Test sent to {t}"),
                    Lang::Zh => format!("测试已发到 {t}"),
                }),
            };
            done("fwd-rules", l, o)
        },
    )
}

pub async fn del_set(
    State(app): State<Shared>,
    Query(q): Query<IdQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let (who, l, _) = who(&app, &req);
    respond(
        pending("fwd-rules", l, "Deleting the rule…", "正在删除规则…"),
        async move {
            let r = match find_rule(&app, &who, q.id, l).await {
                Ok((r, _)) => r,
                Err(o) => return done("fwd-rules", l, o),
            };
            let id = r.id;
            let o = write_cfg(
                &app,
                &who,
                reqwest::Method::DELETE,
                "/api/sms/forward/rules",
                &json!({"id": id}),
                &dest_secrets(&r.dest),
                match l {
                    Lang::En => format!("Rule {} deleted", r.name),
                    Lang::Zh => format!("规则「{}」已删除", r.name),
                },
                move |c| !c.rules.iter().any(|r| r.id == id),
                l,
            )
            .await;
            done("fwd-rules", l, o)
        },
    )
}

// the log: retry the failed, clear

pub fn retry_confirm(n: usize, l: Lang) -> write::Confirm {
    write::Confirm {
        tier: write::Tier::Inline,
        title: l.pick("Retry Failed", "重试失败项").into(),
        what: match l {
            Lang::En => format!(
                "The device sends the {n} failed message{} again to their rules' destinations.",
                if n == 1 { "" } else { "s" }
            ),
            Lang::Zh => format!("设备把这 {n} 条失败的短信再发一次到各自规则的目标。"),
        },
        downtime: None,
        recovery: None,
        action: match l {
            Lang::En => format!("Confirm: retry {n} failed"),
            Lang::Zh => format!("确认：重试 {n} 条失败项"),
        },
        cuts_uplink: false,
        word: None,
        danger: false,
        revert: crate::write::Revert::No,
    }
}

pub fn clear_confirm(n: usize, l: Lang) -> write::Confirm {
    write::Confirm {
        tier: write::Tier::Dialog,
        title: l.pick("Clear the forwarding log?", "清空转发日志？").into(),
        what: match l {
            Lang::En => {
                format!("All {n} log entries are deleted. Failed entries can no longer be retried.")
            }
            Lang::Zh => format!("全部 {n} 条日志都会删掉，失败的条目也不能再重试。"),
        },
        downtime: None,
        recovery: Some(
            l.pick(
                "None: the log can't be brought back.",
                "无法恢复：日志删了就没有了。",
            )
            .into(),
        ),
        action: l.pick("Clear Log", "清空日志").into(),
        cuts_uplink: false,
        word: None,
        danger: true,
        revert: crate::write::Revert::No,
    }
}

pub async fn retry_ask(
    State(app): State<Shared>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let (who, l, remote) = who(&app, &req);
    let html = match read_log(&app, &who).await {
        Err(f) => done("fwd-log", l, not_read(l, &f)),
        Ok(g) => match g.iter().filter(|e| !e.success).count() {
            0 => done(
                "fwd-log",
                l,
                write::Outcome::NotYet(l.pick("No failed entries", "没有失败的条目").into()),
            ),
            n => area(
                "fwd-log",
                l,
                remote,
                &format!("{PATH}/log/retry/set"),
                &write::Phase::Ask(retry_confirm(n, l)),
            ),
        },
    };
    one(html)
}

/// The failed entries' (sms, rule) — what a retry looks up again before each request (the
/// agent rewrites an entry's time on every try, and a new message shifts the indices).
pub fn failed_keys(g: &[Entry]) -> Vec<(u64, u64, String)> {
    g.iter()
        .filter(|e| !e.success)
        .map(|e| (e.sms_id, e.rule_id, e.sender.clone()))
        .collect()
}

/// Where a failed entry is now (its `{index}`), if it is still failed.
pub fn index_of(g: &[Entry], sms: u64, rule: u64) -> Option<usize> {
    g.iter()
        .position(|e| !e.success && e.sms_id == sms && e.rule_id == rule)
}

pub fn retry_said(total: usize, failing: &[(String, String)], l: Lang) -> write::Outcome {
    if failing.is_empty() {
        return write::Outcome::Applied(match l {
            Lang::En => format!("All {total} sent again"),
            Lang::Zh => format!("{total} 条都已重新发出"),
        });
    }
    let list = failing
        .iter()
        .map(|(s, e)| {
            if e.is_empty() {
                s.clone()
            } else {
                format!("{s}: {e}")
            }
        })
        .collect::<Vec<_>>()
        .join(l.pick("; ", "；"));
    write::Outcome::Failed(match l {
        Lang::En => format!("{} of {total} still failing — {list}", failing.len()),
        Lang::Zh => format!("{total} 条里还有 {} 条失败——{list}", failing.len()),
    })
}

pub async fn retry_set(
    State(app): State<Shared>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let (who, l, _) = who(&app, &req);
    respond(
        pending(
            "fwd-log",
            l,
            "Retrying the failed entries…",
            "正在重试失败项…",
        ),
        async move {
            let (g, sec) = match tokio::join!(read_log(&app, &who), read_cfg(&app, &who)) {
                (Ok(g), Ok(c)) => (g, secrets(&c)),
                (Err(f), _) | (_, Err(f)) => return done("fwd-log", l, not_read(l, &f)),
            };
            let keys = failed_keys(&g);
            let mut lost = false;
            for (sms, rule, _) in &keys {
                // look it up again: a newly forwarded message shifts the indices
                let Ok(now) = read_log(&app, &who).await else {
                    lost = true;
                    break;
                };
                let Some(index) = index_of(&now, *sms, *rule) else {
                    continue; // already sent, or gone
                };
                match crate::agent::post(
                    &app,
                    &who,
                    "/api/sms/forward/retry",
                    &json!({"index": index}),
                )
                .await
                {
                    // the destination still refuses: carry on, the log read below says so
                    Ok(_) | Err(Fail::Refused(..)) => {}
                    // no answer / signed out: stop here, never resend
                    Err(_) => {
                        lost = true;
                        break;
                    }
                }
            }
            let _ = app.kick.send(PATH);
            let o = match read_log(&app, &who).await {
                Err(_) => write::Outcome::Unknown,
                Ok(_) if lost => write::Outcome::Unknown,
                Ok(after) => {
                    let failing: Vec<(String, String)> = keys
                        .iter()
                        .filter_map(|(sms, rule, sender)| {
                            after
                                .iter()
                                .find(|e| !e.success && e.sms_id == *sms && e.rule_id == *rule)
                                .map(|e| (sender.clone(), scrub(&e.error, &sec)))
                        })
                        .collect();
                    retry_said(keys.len(), &failing, l)
                }
            };
            done("fwd-log", l, o)
        },
    )
}

pub async fn clear_ask(
    State(app): State<Shared>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let (who, l, remote) = who(&app, &req);
    let html = match read_log(&app, &who).await {
        Err(f) => done("fwd-log", l, not_read(l, &f)),
        Ok(g) if g.is_empty() => done(
            "fwd-log",
            l,
            write::Outcome::NotYet(l.pick("The log is already empty", "日志已经是空的").into()),
        ),
        Ok(g) => area(
            "fwd-log",
            l,
            remote,
            &format!("{PATH}/log/clear/set"),
            &write::Phase::Ask(clear_confirm(g.len(), l)),
        ),
    };
    one(html)
}

pub async fn clear_set(
    State(app): State<Shared>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let (who, l, _) = who(&app, &req);
    respond(
        pending("fwd-log", l, "Clearing the log…", "正在清空日志…"),
        async move {
            let sent = match send(
                &app,
                &who,
                reqwest::Method::POST,
                "/api/sms/forward/log/clear",
                &json!({}),
                &[],
                l,
            )
            .await
            {
                Ok(s) => s,
                Err(o) => return done("fwd-log", l, o),
            };
            let held = write::read_back(
                &app.hub,
                READBACK,
                Duration::from_secs(1),
                Duration::from_secs(2),
                {
                    let (a, w) = (&app, &who);
                    move || async move { read_log(a, w).await.is_ok_and(|g| g.is_empty()) }
                },
            )
            .await;
            let _ = app.kick.send(PATH);
            let o = match (held, sent) {
                (true, _) => write::Outcome::Applied(l.pick("Log cleared", "日志已清空").into()),
                (false, Sent::Ok) => write::Outcome::Accepted,
                (false, Sent::NoReply) => write::Outcome::Unknown,
            };
            done("fwd-log", l, o)
        },
    )
}

#[cfg(test)]
mod tests;
