//! /services/chill: the writes. Tiers are the inventory's (web/docs/controls-inventory.md):
//!
//! ```text
//! tier 1 (post at once): exit proxy / direct_keep_ai / global, region, AI exit, profile, extend 2 h
//! tier 2 (inline confirm): start, stop, exit direct_all, home-exit switches, subscription refresh,
//!                          subscription link save, device bypass (switch and the stale ×)
//! ```
//! Every write goes to the agent once (never retried) and is read back from the agent. Start, stop,
//! profile and the link save are agent jobs: the handler follows `/api/services/chill/job` every
//! 1.5 s (up to 3 minutes) like the old page's `runJob`, then reads the state back.
//!
//! Names (nodes, providers, hostnames) never travel in a URL: a button carries a position and a
//! [`tag`] of the name, and the server looks the entry up again and refuses when it moved. The
//! subscription link is typed into a password field, sent in the body of `prov/save/ask`, kept
//! here under a one-time token until the confirm, and never written back to the page.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::{Query, Request, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::sse::{Event, Sse};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{Value, json};

use super::data::*;
use super::view::{AREAS, area};
use super::{PATH, bump, set_paused};
use crate::agent::{self, Caller, Fail};
use crate::i18n::Lang;
use crate::pages::{Shared, patch};
use crate::write::{self, Confirm, Outcome, Phase, Tier};

const STATUS: &str = "/api/services/chill";
const JOB: &str = "/api/services/chill/job";
const JOB_EVERY: Duration = Duration::from_millis(1500);
const JOB_LIMIT: Duration = Duration::from_secs(180);
const HOME_LIMIT: Duration = Duration::from_secs(90);

fn who(app: &Shared, req: &Request) -> (Caller, Lang, bool) {
    let ctx = crate::topbar::Ctx::of_request(req, PATH);
    (Caller::of(app, req), ctx.lang, ctx.remote)
}

fn one(html: String) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(futures_util::stream::once(async move { Ok(patch(&html)) }))
}

fn done(id: &str, l: Lang, o: Outcome) -> String {
    area(id, l, false, "", &Phase::Done(o))
}

fn pending(id: &str, l: Lang, text: &str) -> String {
    area(id, l, false, "", &Phase::Pending(text.to_owned()))
}

fn inline(title: String, what: String, action: String) -> Confirm {
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
    }
}

fn ask(id: &str, l: Lang, remote: bool, set: &str, c: Confirm) -> String {
    area(id, l, remote, set, &Phase::Ask(c))
}

fn failed(e: &Fail, l: Lang) -> Outcome {
    match e {
        Fail::Refused(409, m, _) if m.contains("in progress") => Outcome::Busy,
        e => Outcome::Failed(e.text(l)),
    }
}

/// `Err(outcome)` when the send did not go through; `Ok(true)` when it may have happened without a
/// reply (read back anyway).
fn sent(r: Result<Value, Fail>, l: Lang) -> Result<(Value, bool), Outcome> {
    match r {
        Ok(v) => Ok((v, false)),
        Err(Fail::Unreachable) => Ok((Value::Null, true)),
        Err(e) => Err(failed(&e, l)),
    }
}

async fn read(app: &Shared, who: &Caller, path: &str) -> Result<Value, Fail> {
    agent::get(app, who, path).await
}

/// Read `path` until `ok` holds: three tries, a second apart. None = never read.
async fn back(app: &Shared, who: &Caller, path: &str, ok: impl Fn(&Value) -> bool) -> Option<bool> {
    let mut any = false;
    for i in 0..3 {
        if i > 0 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        if let Ok(v) = read(app, who, path).await {
            any = true;
            if ok(&v) {
                return Some(true);
            }
        }
    }
    any.then_some(false)
}

/// One finished write: the readback decides the words.
async fn finish(app: &Shared, who: &Caller, path: &str, no_reply: bool, applied: String, ok: impl Fn(&Value) -> bool) -> Outcome {
    bump(app);
    match back(app, who, path, ok).await {
        Some(true) => Outcome::Applied(applied),
        _ if no_reply => Outcome::Unknown,
        _ => Outcome::Accepted,
    }
}

/// Start an agent job and follow it to its end: its message, or why not.
async fn run_job(app: &Shared, who: &Caller, method: reqwest::Method, path: &str, body: Value, l: Lang) -> Result<String, Outcome> {
    let r = agent::send_json(app, who, method, path, &body).await;
    let (reply, _) = sent(r, l)?;
    let Some(id) = reply["job_id"].as_i64() else {
        return Err(Outcome::Failed(l.pick("the device did not start the job", "设备没有开始这个任务").into()));
    };
    let until = tokio::time::Instant::now() + JOB_LIMIT;
    loop {
        tokio::time::sleep(JOB_EVERY).await;
        if let Ok(j) = read(app, who, JOB).await
            && j["id"].as_i64() == Some(id)
        {
            match s(&j, "status") {
                "done" => return Ok(s(&j, "message").to_owned()),
                "error" => {
                    let m = s(&j, "message");
                    return Err(Outcome::Failed(if m.is_empty() { l.pick("the job failed", "任务失败").into() } else { m.to_owned() }));
                }
                _ => {}
            }
        }
        if tokio::time::Instant::now() > until {
            return Err(Outcome::NotYet(
                l.pick("Still running after 3 minutes; check the status above", "3 分钟后还没结束，请看上方状态").into(),
            ));
        }
    }
}

// ---- small endpoints ----

/// 刷新 / 重试: every open tab reads everything again.
pub async fn refresh(State(app): State<Shared>) -> StatusCode {
    bump(&app);
    StatusCode::NO_CONTENT
}

#[derive(Deserialize, Default)]
pub struct PauseQuery {
    #[serde(default)]
    t: String,
    #[serde(default)]
    on: u8,
}

/// 暂停 / 继续 the log: this tab's stream stops (or resumes) reading it.
pub async fn pause(State(app): State<Shared>, Query(q): Query<PauseQuery>) -> StatusCode {
    set_paused(&q.t, q.on == 1);
    bump(&app);
    StatusCode::NO_CONTENT
}

#[derive(Deserialize, Default)]
pub struct CancelQ {
    #[serde(default)]
    a: String,
    #[serde(default)]
    lost: bool,
}

pub async fn cancel(Query(q): Query<CancelQ>, req: Request) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let id = format!("ch-{}", q.a);
    let id = AREAS.iter().find(|a| **a == id).copied().unwrap_or("ch-pw");
    one(area(id, ctx.lang, ctx.remote, "", &write::cancel_phase(&write::CancelQuery { lost: q.lost })))
}

// ---- start / stop (tier 2, job) ----

#[derive(Deserialize, Default)]
pub struct ActQ {
    #[serde(default)]
    act: String,
}

pub async fn power_ask(Query(q): Query<ActQ>, req: Request) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let (l, remote) = (ctx.lang, ctx.remote);
    let stop = q.act == "stop";
    let c = inline(
        l.pick(if stop { "Stop CHILL?" } else { "Start CHILL?" }, if stop { "停止 CHILL？" } else { "启动 CHILL？" }).into(),
        if stop {
            l.pick("All traffic goes direct, AI and VoWiFi too. You can start it again at any time.", "全屋流量改为直连，AI 和 VoWiFi 也直连。随时可以再打开。")
        } else {
            l.pick("Traffic starts going through CHILL; it takes about 10 seconds.", "全屋流量开始经过 CHILL，约 10 秒生效。")
        }
        .into(),
        l.pick(if stop { "Confirm: stop" } else { "Confirm: start" }, if stop { "确认：停止" } else { "确认：启动" }).into(),
    );
    one(ask("ch-pw", l, remote, &format!("{PATH}/power/set?act={}", if stop { "stop" } else { "start" }), c))
}

pub async fn power_set(State(app): State<Shared>, Query(q): Query<ActQ>, req: Request) -> impl IntoResponse {
    let (who, l, _) = who(&app, &req);
    let stop = q.act == "stop";
    let p = pending("ch-pw", l, if stop { l.pick("Stopping…", "停止中…") } else { l.pick("Starting…", "启动中…") });
    write::respond(p, async move {
        let path = if stop { "/api/services/chill/disable" } else { "/api/services/chill/enable" };
        let o = match run_job(&app, &who, reqwest::Method::POST, path, json!({}), l).await {
            Err(o) => o,
            Ok(_) => {
                finish(
                    &app,
                    &who,
                    STATUS,
                    false,
                    l.pick(if stop { "CHILL stopped" } else { "CHILL started" }, if stop { "CHILL 已停止" } else { "CHILL 已启动" }).into(),
                    move |v| {
                        let st = s(v, "state");
                        if stop { st != "running" } else { st == "running" || st == "direct" }
                    },
                )
                .await
            }
        };
        done("ch-pw", l, o)
    })
}

// ---- exit (tier 1; all direct tier 2) ----

#[derive(Deserialize, Default)]
pub struct ExitQ {
    #[serde(default)]
    v: String,
}

pub async fn exit_ask(req: Request) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let c = inline(
        l.pick("Switch to All direct?", "切到全部直连？").into(),
        l.pick("All traffic goes direct, AI and VoWiFi too. You can switch back at any time.", "全屋流量改为直连，AI 和 VoWiFi 也直连。随时可以改回来。").into(),
        l.pick("Confirm: all direct", "确认：全部直连").into(),
    );
    one(ask("ch-ex", l, ctx.remote, &format!("{PATH}/exit/set?v=direct_all"), c))
}

pub async fn exit_set(State(app): State<Shared>, Query(q): Query<ExitQ>, req: Request) -> impl IntoResponse {
    let (who, l, _) = who(&app, &req);
    let p = pending("ch-ex", l, l.pick("Switching…", "正在切换…"));
    write::respond(p, async move {
        let want = q.v;
        if !EXITS.contains(&want.as_str()) {
            return done("ch-ex", l, Outcome::Failed(l.pick("unknown exit", "不认识这个出口").into()));
        }
        let r = agent::send_json(&app, &who, reqwest::Method::PUT, "/api/services/chill/exit", &json!({"state": want})).await;
        let o = match sent(r, l) {
            Err(o) => o,
            Ok((_, no_reply)) => {
                let w = want.clone();
                let words = match l {
                    Lang::En => format!("Exit: {}", exit_label(&want, l)),
                    Lang::Zh => format!("出口：{}", exit_label(&want, l)),
                };
                finish(&app, &who, STATUS, no_reply, words, move |v| s(v, "exit") == w).await
            }
        };
        done("ch-ex", l, o)
    })
}

// ---- region / AI exit (tier 1) ----

#[derive(Deserialize, Default)]
pub struct PickQ {
    #[serde(default)]
    g: String,
    #[serde(default)]
    i: usize,
    #[serde(default)]
    h: String,
}

pub async fn pick_set(State(app): State<Shared>, Query(q): Query<PickQ>, req: Request) -> impl IntoResponse {
    let (who, l, _) = who(&app, &req);
    let main = q.g == "main";
    let id = if main { "ch-rg" } else { "ch-ax" };
    write::respond(pending(id, l, l.pick("Switching…", "正在切换…")), async move {
        let (key, group) = if main { ("region", MAIN_GROUP) } else { ("ai_exit", AI_GROUP) };
        let member = match read(&app, &who, STATUS).await {
            Err(e) => return done(id, l, Outcome::Failed(e.text(l))),
            Ok(v) => strs(&v[key]["options"]).into_iter().nth(q.i).filter(|m| tag(m) == q.h),
        };
        let Some(member) = member else {
            return done(id, l, Outcome::NotYet(l.pick("The list changed; press it again", "列表变了，请再按一次").into()));
        };
        let r = agent::send_json(&app, &who, reqwest::Method::PUT, "/api/services/chill/regions", &json!({"group": group, "member": member})).await;
        let o = match sent(r, l) {
            Err(o) => o,
            Ok((_, no_reply)) => {
                let m = member.clone();
                let words = match l {
                    Lang::En => format!("Switched to {member}"),
                    Lang::Zh => format!("已切到 {member}"),
                };
                finish(&app, &who, STATUS, no_reply, words, move |v| s(&v[key], "active") == m).await
            }
        };
        done(id, l, o)
    })
}

// ---- profile (tier 1, job) ----

pub async fn profile_set(State(app): State<Shared>, Query(q): Query<ExitQ>, req: Request) -> impl IntoResponse {
    let (who, l, _) = who(&app, &req);
    write::respond(pending("ch-pf", l, l.pick("Switching profile…", "正在切换档位…")), async move {
        let want = q.v;
        if !PROFILES.contains(&want.as_str()) {
            return done("ch-pf", l, Outcome::Failed(l.pick("unknown profile", "不认识这个档位").into()));
        }
        let o = match run_job(&app, &who, reqwest::Method::PUT, "/api/services/chill/profile", json!({"profile": want}), l).await {
            Err(o) => o,
            Ok(_) => {
                let w = want.clone();
                let words = match l {
                    Lang::En => format!("Profile: {}", profile_label(&want, l)),
                    Lang::Zh => format!("档位：{}", profile_label(&want, l)),
                };
                finish(&app, &who, STATUS, false, words, move |v| s(v, "profile") == w).await
            }
        };
        done("ch-pf", l, o)
    })
}

// ---- home exit (tier 2; extend tier 1) ----

#[derive(Deserialize, Default)]
pub struct HomeQ {
    #[serde(default)]
    item: String,
    #[serde(default)]
    on: u8,
}

const HOME: &str = "/api/services/chill/home";

fn home_words(item: &str, on: bool, l: Lang) -> String {
    let w = if on { l.pick("on", "打开") } else { l.pick("off", "关闭") };
    format!("{} · {w}", home_label(item, l))
}

pub async fn home_ask(State(app): State<Shared>, Query(q): Query<HomeQ>, req: Request) -> impl IntoResponse {
    let (who, l, remote) = who(&app, &req);
    let on = q.on == 1;
    if !HOME_ITEMS.contains(&q.item.as_str()) {
        return one(done("ch-ho", l, Outcome::Failed(l.pick("unknown item", "不认识这一项").into())));
    }
    let h = match read(&app, &who, HOME).await {
        Ok(h) => h,
        Err(e) => return one(done("ch-ho", l, Outcome::Failed(e.text(l)))),
    };
    if on && let Some(b) = h["blocked"].as_str() {
        return one(done("ch-ho", l, Outcome::NotYet(home_code_text(b, l))));
    }
    let roaming = h["where"]["true_roaming"].as_bool() != Some(false);
    let c = inline(
        home_words(&q.item, on, l),
        if roaming { l.pick("Drops ~5 s · uses roaming data", "会断一下，走漫游流量") } else { l.pick("Drops for ~5 s", "会断一下（约 5 秒）") }.into(),
        match l {
            Lang::En => format!("Confirm: {}", home_words(&q.item, on, l)),
            Lang::Zh => format!("确认：{}", home_words(&q.item, on, l)),
        },
    );
    one(ask("ch-ho", l, remote, &format!("{PATH}/home/set?item={}&on={}", q.item, u8::from(on)), c))
}

pub async fn home_set(State(app): State<Shared>, Query(q): Query<HomeQ>, req: Request) -> impl IntoResponse {
    let (who, l, _) = who(&app, &req);
    let on = q.on == 1;
    write::respond(pending("ch-ho", l, l.pick("Switching, this can take a minute…", "正在切换，可能要一分钟…")), async move {
        if !HOME_ITEMS.contains(&q.item.as_str()) {
            return done("ch-ho", l, Outcome::Failed(l.pick("unknown item", "不认识这一项").into()));
        }
        let body = json!({"item": q.item, "on": on, "confirm": true});
        let r = agent::send_json(&app, &who, reqwest::Method::PUT, HOME, &body).await;
        // a 409 carries a code ("busy", "mode_global", …) in the agent's error
        let no_reply = match r {
            Ok(_) => false,
            Err(Fail::Unreachable) => true,
            Err(Fail::Refused(409, m, _)) => return done("ch-ho", l, Outcome::Failed(home_code_text(&m, l))),
            Err(e) => return done("ch-ho", l, Outcome::Failed(e.text(l))),
        };
        bump(&app);
        // The switch runs in the background (checking → trial → applying): wait for it to end.
        let start = Instant::now();
        let mut read_any = false;
        let mut last = Value::Null;
        while start.elapsed() < HOME_LIMIT {
            tokio::time::sleep(JOB_EVERY).await;
            let Ok(h) = read(&app, &who, HOME).await else { continue };
            read_any = true;
            let busy = h["busy"].is_object();
            let is_on = h["items"][&q.item]["on"].as_bool() == Some(true);
            last = h;
            if busy {
                continue;
            }
            if is_on == on {
                bump(&app);
                return done("ch-ho", l, Outcome::Applied(home_words(&q.item, on, l)));
            }
            if start.elapsed() > Duration::from_secs(5) {
                break;
            }
        }
        let n = &last["notice"];
        let o = if read_any && s(n, "kind") == "failed" && s(n, "item") == q.item {
            let d = s(n, "detail");
            let t = if l == Lang::En && !s(n, "text_en").is_empty() { s(n, "text_en") } else { s(n, "text") };
            Outcome::Failed(if d.is_empty() { t.to_owned() } else { format!("{t} ({d})") })
        } else if read_any {
            Outcome::Accepted
        } else if no_reply {
            Outcome::Unknown
        } else {
            Outcome::Accepted
        };
        bump(&app);
        done("ch-ho", l, o)
    })
}

pub async fn home_extend(State(app): State<Shared>, req: Request) -> impl IntoResponse {
    let (who, l, _) = who(&app, &req);
    write::respond(pending("ch-ho", l, l.pick("Extending…", "正在延长…")), async move {
        let before = read(&app, &who, HOME).await.ok().and_then(|h| h["items"]["rest"]["until"].as_i64());
        let r = agent::send_json(&app, &who, reqwest::Method::PUT, HOME, &json!({"action": "extend"})).await;
        let o = match sent(r, l) {
            Err(Outcome::Failed(m)) => Outcome::Failed(home_code_text(&m, l)),
            Err(o) => o,
            Ok((_, no_reply)) => {
                finish(&app, &who, HOME, no_reply, l.pick("Extended by 2 hours", "已再开 2 小时").into(), move |h| {
                    h["items"]["rest"]["on"].as_bool() == Some(true) && h["items"]["rest"]["until"].as_i64() > before
                })
                .await
            }
        };
        done("ch-ho", l, o)
    })
}

// ---- subscriptions ----

const PROVIDERS: &str = "/api/services/chill/providers";

#[derive(Deserialize, Default)]
pub struct ProvQ {
    #[serde(default)]
    i: usize,
    #[serde(default)]
    h: String,
}

/// The provider at position `i` whose name still has tag `h`: (name, editable, updated_at).
async fn provider(app: &Shared, who: &Caller, q: &ProvQ, l: Lang) -> Result<(String, bool, String), Outcome> {
    let v = read(app, who, PROVIDERS).await.map_err(|e| Outcome::Failed(e.text(l)))?;
    v["providers"]
        .as_array()
        .and_then(|a| a.get(q.i))
        .filter(|p| tag(s(p, "name")) == q.h)
        .map(|p| (s(p, "name").to_owned(), p["editable"].as_bool() == Some(true), s(p, "updated_at").to_owned()))
        .ok_or_else(|| Outcome::NotYet(l.pick("The list changed; press it again", "列表变了，请再按一次").into()))
}

pub async fn prov_refresh_ask(State(app): State<Shared>, Query(q): Query<ProvQ>, req: Request) -> impl IntoResponse {
    let (who, l, remote) = who(&app, &req);
    let html = match provider(&app, &who, &q, l).await {
        Err(o) => done("ch-pv", l, o),
        Ok(_) => {
            let c = inline(
                l.pick("Refresh the subscription?", "刷新这个订阅？").into(),
                l.pick(
                    "Downloads the node list again now; nodes that disappeared stop being used.",
                    "现在重新下载节点列表；已经消失的节点不再使用。",
                )
                .into(),
                l.pick("Confirm: refresh", "确认：刷新").into(),
            );
            ask("ch-pv", l, remote, &format!("{PATH}/prov/refresh/set?i={}&h={}", q.i, q.h), c)
        }
    };
    one(html)
}

pub async fn prov_refresh_set(State(app): State<Shared>, Query(q): Query<ProvQ>, req: Request) -> impl IntoResponse {
    let (who, l, _) = who(&app, &req);
    write::respond(pending("ch-pv", l, l.pick("Refreshing…", "正在刷新…")), async move {
        let (name, _, before) = match provider(&app, &who, &q, l).await {
            Ok(p) => p,
            Err(o) => return done("ch-pv", l, o),
        };
        let r = agent::post(&app, &who, "/api/services/chill/providers/refresh", &json!({"name": name})).await;
        let o = match sent(r, l) {
            Err(o) => o,
            Ok((_, no_reply)) => {
                let n = name.clone();
                finish(&app, &who, PROVIDERS, no_reply, l.pick("Refreshed", "已刷新").into(), move |v| {
                    v["providers"].as_array().is_some_and(|a| a.iter().any(|p| s(p, "name") == n && s(p, "updated_at") != before))
                })
                .await
            }
        };
        done("ch-pv", l, o)
    })
}

const FORM: &str = "ch-pv-form";

fn form_closed() -> String {
    format!("<div id=\"{FORM}\"></div>")
}

fn form_open(l: Lang, name: &str, q: &ProvQ) -> String {
    format!(
        "<div id=\"{FORM}\"><div class=\"nd-group grid gap-3 p-4\" role=\"group\" aria-labelledby=\"ch-pv-ft\" data-signals=\"{{_cu: ''}}\"><div id=\"ch-pv-ft\" class=\"nd-row__label\">{} <span data-data>{}</span></div><label class=\"grid gap-1\"><span class=\"nd-aux\">{}</span><input class=\"nd-field nd-mono\" type=\"password\" autocomplete=\"off\" autocapitalize=\"off\" spellcheck=\"false\" inputmode=\"url\" placeholder=\"{}\" data-bind=\"_cu\"></label><div class=\"flex flex-wrap gap-2\"><button type=\"button\" class=\"nd-btn nd-btn--primary\" data-attr:disabled=\"!$_cu\" disabled data-on:click=\"@post('{PATH}/prov/save/ask?i={}&h={}', {{filterSignals: {{include: /^_cu$/, exclude: /(?!)/}}}})\">{}</button><button type=\"button\" class=\"nd-btn nd-btn--secondary\" data-on:click=\"@post('{PATH}/prov/edit/close')\">{}</button></div></div></div>",
        l.pick("Edit subscription link:", "编辑订阅链接："),
        crate::agent::esc(name),
        l.pick("Subscription URL", "订阅链接"),
        l.pick("https://… subscription URL", "https://… 订阅链接"),
        q.i,
        q.h,
        l.pick("Save", "保存"),
        l.pick("Cancel", "取消"),
    )
}

pub async fn prov_edit(State(app): State<Shared>, Query(q): Query<ProvQ>, req: Request) -> impl IntoResponse {
    let (who, l, remote) = who(&app, &req);
    let idle = area("ch-pv", l, remote, "", &Phase::Idle);
    let html = match provider(&app, &who, &q, l).await {
        Ok((name, true, _)) => format!("{}\n{idle}", form_open(l, &name, &q)),
        Ok(_) => format!("{}\n{}", form_closed(), done("ch-pv", l, Outcome::Failed(l.pick("this subscription has no editable link", "这个订阅的链接不能改").into()))),
        Err(o) => format!("{}\n{}", form_closed(), done("ch-pv", l, o)),
    };
    one(html)
}

pub async fn prov_edit_close(req: Request) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    one(format!("{}\n{}", form_closed(), area("ch-pv", ctx.lang, ctx.remote, "", &Phase::Idle)))
}

/// A link waiting for its confirm: the name it is for, the link. Only here, only until taken.
struct Held {
    name: String,
    url: String,
    at: Instant,
}

const STASH_TTL: Duration = Duration::from_secs(600);

fn stash() -> &'static Mutex<HashMap<String, Held>> {
    static S: std::sync::OnceLock<Mutex<HashMap<String, Held>>> = std::sync::OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

fn put(name: String, url: String) -> String {
    let t = crate::auth::random_id();
    let mut m = stash().lock().unwrap();
    m.retain(|_, h| h.at.elapsed() < STASH_TTL);
    m.insert(t.clone(), Held { name, url, at: Instant::now() });
    t
}

fn take(t: &str) -> Option<(String, String)> {
    stash().lock().unwrap().remove(t).filter(|h| h.at.elapsed() < STASH_TTL).map(|h| (h.name, h.url))
}

#[derive(Deserialize, Default)]
struct LinkIn {
    #[serde(default, rename = "_cu")]
    url: String,
}

pub async fn prov_save_ask(State(app): State<Shared>, Query(q): Query<ProvQ>, req: Request) -> impl IntoResponse {
    let (who, l, remote) = who(&app, &req);
    let body = axum::body::to_bytes(req.into_body(), 16 * 1024).await.map(|b| b.to_vec()).unwrap_or_default();
    let url = serde_json::from_slice::<LinkIn>(&body).unwrap_or_default().url.trim().to_owned();
    if !sub_url_ok(&url) {
        return one(done("ch-pv", l, Outcome::NotYet(l.pick("Enter a valid http(s) URL", "请输入有效的 http(s) URL").into())));
    }
    let html = match provider(&app, &who, &q, l).await {
        Ok((name, true, _)) => {
            let c = inline(
                l.pick("Save the subscription link?", "保存订阅链接？").into(),
                l.pick(
                    "CHILL reloads with the new subscription; connections may drop for a few seconds.",
                    "CHILL 用新订阅重新加载，连接可能断开几秒。",
                )
                .into(),
                l.pick("Confirm: save the link", "确认：保存订阅链接").into(),
            );
            ask("ch-pv", l, remote, &format!("{PATH}/prov/save/set?t={}", put(name, url)), c)
        }
        Ok(_) => done("ch-pv", l, Outcome::Failed(l.pick("this subscription has no editable link", "这个订阅的链接不能改").into())),
        Err(o) => done("ch-pv", l, o),
    };
    one(html)
}

#[derive(Deserialize, Default)]
pub struct TokenQ {
    #[serde(default)]
    t: String,
}

pub async fn prov_save_set(State(app): State<Shared>, Query(q): Query<TokenQ>, req: Request) -> impl IntoResponse {
    let (who, l, _) = who(&app, &req);
    let held = take(&q.t);
    write::respond(pending("ch-pv", l, l.pick("Saving and reloading…", "正在保存并重新加载…")), async move {
        let Some((name, url)) = held else {
            return done("ch-pv", l, Outcome::NotYet(l.pick("The form expired; press it again", "表单已过期，请再按一次").into()));
        };
        let o = match run_job(&app, &who, reqwest::Method::PUT, PROVIDERS, json!({"name": name, "url": url}), l).await {
            Err(o) => o,
            // the link itself has no readback: the job ended well, and that is all that is said
            Ok(_) => Outcome::Applied(
                l.pick("Link saved and CHILL reloaded (the link itself can't be read back)", "链接已保存，CHILL 已重新加载（链接本身读不回来）").into(),
            ),
        };
        bump(&app);
        let close = if matches!(o, Outcome::Applied(_)) { format!("{}\n", form_closed()) } else { String::new() };
        format!("{close}{}", done("ch-pv", l, o))
    })
}

// ---- device bypass (tier 2) ----

#[derive(Deserialize, Default)]
pub struct BypassQ {
    #[serde(default)]
    ip: String,
    #[serde(default)]
    on: u8,
}

const BYPASS: &str = "/api/services/chill/bypass";

pub async fn byp_ask(State(app): State<Shared>, Query(q): Query<BypassQ>, req: Request) -> impl IntoResponse {
    let (who, l, remote) = who(&app, &req);
    if !ipv4(&q.ip) {
        return one(done("ch-bp", l, Outcome::Failed(l.pick("not an IPv4 address", "不是 IPv4 地址").into())));
    }
    let on = q.on == 1;
    let name = match read(&app, &who, "/api/network/clients").await {
        Ok(c) => client_name(&c, &q.ip),
        Err(_) => q.ip.clone(),
    };
    let (title, what, action) = match (l, on) {
        (Lang::En, true) => (format!("Bypass {name}?"), format!("{name} goes straight to the internet without CHILL, AI and VoWiFi included."), "Confirm: bypass".to_owned()),
        (Lang::En, false) => (format!("Stop bypassing {name}?"), format!("{name} goes through CHILL again."), "Confirm: stop bypassing".to_owned()),
        (Lang::Zh, true) => (format!("让 {name} 绕行？"), format!("{name} 不经过 CHILL，直接上网（AI 和 VoWiFi 也一样）。"), "确认：绕行".to_owned()),
        (Lang::Zh, false) => (format!("取消 {name} 的绕行？"), format!("{name} 重新经过 CHILL。"), "确认：取消绕行".to_owned()),
    };
    one(ask("ch-bp", l, remote, &format!("{PATH}/byp/set?ip={}&on={}", q.ip, u8::from(on)), inline(title, what, action)))
}

pub async fn byp_set(State(app): State<Shared>, Query(q): Query<BypassQ>, req: Request) -> impl IntoResponse {
    let (who, l, _) = who(&app, &req);
    let on = q.on == 1;
    write::respond(pending("ch-bp", l, l.pick("Saving…", "正在保存…")), async move {
        if !ipv4(&q.ip) {
            return done("ch-bp", l, Outcome::Failed(l.pick("not an IPv4 address", "不是 IPv4 地址").into()));
        }
        // the list as it is now, not as the page drew it
        let cur = match read(&app, &who, BYPASS).await {
            Ok(v) => strs(&v["ips"]),
            Err(e) => return done("ch-bp", l, Outcome::Failed(e.text(l))),
        };
        let next = bypass_next(&cur, &q.ip, on);
        let r = agent::send_json(&app, &who, reqwest::Method::PUT, BYPASS, &json!({"ips": next})).await;
        let o = match sent(r, l) {
            Err(o) => o,
            Ok((_, no_reply)) => {
                let n = next.clone();
                let words = match (l, on) {
                    (Lang::En, true) => format!("{} now bypasses CHILL", q.ip),
                    (Lang::En, false) => format!("{} goes through CHILL again", q.ip),
                    (Lang::Zh, true) => format!("{} 已绕行", q.ip),
                    (Lang::Zh, false) => format!("{} 已取消绕行", q.ip),
                };
                finish(&app, &who, BYPASS, no_reply, words, move |v| same_set(&strs(&v["ips"]), &n)).await
            }
        };
        done("ch-bp", l, o)
    })
}
