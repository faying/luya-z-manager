//! /router/stk (USSD and SIM Toolkit). Nothing is read on
//! entry: every request talks AT commands to the modem, so every send is tier 2 (an inline
//! confirm that names what it does; controls-inventory §/router/stk). There is no readback — the
//! carrier's / SIM's answer is the result — so a good reply is shown and the confirm area goes
//! quiet; a refusal or a lost reply says so (never "成功").
//!
//! ```text
//!  USSD  发送 ─▶ POST ussd/ask (signal code) ─▶ checked (digits and * # +) ─▶ confirm
//!        ─▶ POST ussd/send?h=<hash of the code the confirm named> ─▶ agent /api/ussd/send
//!        ─▶ #ussd-out: the reply, its status in words; session on → reply box + 结束会话,
//!           session over → 新建查询.  回复 / 结束会话 are the same two steps.
//!  STK   加载菜单 / 重新加载 ─▶ confirm ─▶ GET /api/stk/menu (≤ 30 s) ─▶ #stk-view
//!        菜单项 ─▶ confirm ─▶ POST /api/stk/select {item_id} ─▶ a sub-menu or a display text
//!        返回 ─▶ the previous menu (local; no request to the modem)
//! ```
//! - **No state on the server.** The menu path (the menus walked through, for 返回) lives in the
//!   tab as the signal `stk` (a JSON array of `{t: title, i: [{id, l}]}`); the server reads it from
//!   each POST and sets it with a `data-init` span (`#stk-sig`). Menu labels come from the SIM and
//!   may hold `$` or `@`, which Datastar would read as a signal or an action inside an expression,
//!   so they travel as `$` / `@` ([`js_json`]).
//! - Fixed vs the old page (same as the React page's notes): `/api/ussd/respond` answers 200 when
//!   the modem said ERROR (status -1): that is a failure here; the numeric `status` is shown in
//!   words; STK `reason`, `diagnostics` and `source` are shown; an empty sub-menu opens and says so.
//! - A POST whose reply is lost is `Outcome::Unknown` (the carrier may have been asked) and never
//!   sent again; `agent.rs` gives a POST 20 s (select ~13 s, USSD 8 s + setup), the slow menu read
//!   30 s (`AT+CUAD` / `AT+CUSATD` / `AT+STGI` take up to ~20 s).

use std::hash::{Hash, Hasher};
use std::time::Duration;

use askama::Template;
use axum::Router;
use axum::extract::{Query, Request, State};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::topbar::Via;
use crate::agent::{self, Caller, Fail, esc};
use crate::i18n::Lang;
use crate::pages::Shared;
use crate::write::{self, Confirm, Outcome, Phase, Tier};

pub const PATH: &str = "/router/stk";
const MENU_TIMEOUT: Duration = Duration::from_secs(30);

pub fn routes() -> Router<Shared> {
    Router::new()
        .route(PATH, get(page))
        .route("/router/stk/ussd/ask", post(ussd_ask))
        .route("/router/stk/ussd/send", post(ussd_send))
        .route("/router/stk/ussd/reply-ask", post(reply_ask))
        .route("/router/stk/ussd/reply", post(reply_send))
        .route("/router/stk/ussd/end-ask", post(end_ask))
        .route("/router/stk/ussd/end", post(end_send))
        .route("/router/stk/ussd/new", post(ussd_new))
        .route("/router/stk/ussd/cancel", post(ussd_cancel))
        .route("/router/stk/menu/ask", post(menu_ask))
        .route("/router/stk/menu/load", post(menu_load))
        .route("/router/stk/item/ask", post(item_ask))
        .route("/router/stk/item/select", post(item_select))
        .route("/router/stk/back", post(back))
        .route("/router/stk/menu/cancel", post(menu_cancel))
}

// ---- what the tab sends ----

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Default)]
pub struct Item {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    l: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Default)]
pub struct Menu {
    #[serde(default)]
    t: String,
    #[serde(default)]
    i: Vec<Item>,
}

/// The page's signals as Datastar posts them (`_tab` never leaves the browser).
#[derive(Deserialize, Default)]
struct Sig {
    #[serde(default)]
    code: String,
    #[serde(default)]
    reply: String,
    #[serde(default)]
    stk: Vec<Menu>,
}

/// One request, read once: language, remote, who, and the signals.
struct Rq {
    l: Lang,
    remote: Via,
    who: Caller,
    sig: Sig,
}

async fn rq(app: &Shared, req: Request) -> Rq {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let who = Caller::of(app, &req);
    let body = axum::body::to_bytes(req.into_body(), 64 * 1024)
        .await
        .map(|b| b.to_vec())
        .unwrap_or_default();
    Rq {
        l: ctx.lang,
        remote: ctx.remote,
        who,
        sig: serde_json::from_slice(&body).unwrap_or_default(),
    }
}

// ---- small helpers ----

/// Digits and `* # +`, what the agent keeps (it drops the rest silently).
pub fn ussd_ok(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit() || matches!(c, '*' | '#' | '+'))
}

/// What the confirm named, as a token for the send.
fn tok(s: &str) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// JSON as a JavaScript expression Datastar can read: `$` and `@` only occur inside JSON strings,
/// where `$` / `@` are the same characters.
pub fn js_json(v: &impl Serialize) -> String {
    serde_json::to_string(v)
        .unwrap_or_else(|_| "null".into())
        .replace('$', "\\u0024")
        .replace('@', "\\u0040")
}

fn one(html: String) -> impl IntoResponse {
    axum::response::sse::Sse::new(futures_util::stream::once(async move {
        Ok::<_, std::convert::Infallible>(crate::pages::patch(&html))
    }))
}

fn area(id: &'static str, rq: &Rq, set: &str, phase: &Phase) -> String {
    let cancel = if id == "stk-u" {
        "/router/stk/ussd/cancel"
    } else {
        "/router/stk/menu/cancel"
    };
    write::Area {
        id,
        l: rq.l,
        remote: rq.remote,
        set,
        cancel,
        phase,
    }
    .html()
}

fn pending(l: Lang) -> Phase {
    Phase::Pending(l.pick("Waiting for the modem…", "等调制解调器回答…").into())
}

/// A failed call as the area says it. A reply that never came may still have reached the carrier.
fn failed(f: Fail, l: Lang) -> Outcome {
    match f {
        Fail::Unreachable => Outcome::Unknown,
        f => Outcome::Failed(f.text(l)),
    }
}

fn confirm(l: Lang, title: String, what: String) -> Confirm {
    Confirm {
        tier: Tier::Inline,
        action: match l {
            Lang::En => format!("Confirm: {title}"),
            Lang::Zh => format!("确认：{title}"),
        },
        title,
        what,
        downtime: None,
        recovery: None,
        cuts_uplink: false,
        word: None,
        danger: false,
        revert: crate::write::Revert::No,
    }
}

fn err_html(e: Option<&str>) -> String {
    match e {
        None => "<div id=\"ussd-err\"></div>".into(),
        Some(e) => format!(
            "<div id=\"ussd-err\"><p role=\"alert\" class=\"nd-error stk-err\">{}</p></div>",
            esc(e)
        ),
    }
}

fn mark(tone: &str, text: &str) -> String {
    let sym = if tone == "warn" { "▲" } else { "●" };
    format!(
        "<span class=\"nd-mark nd-mark--{tone}\"><span class=\"nd-mark__sym\" aria-hidden=\"true\">{sym}</span><span>{}</span></span>",
        esc(text)
    )
}

// ---- the page ----

#[derive(Template)]
#[template(path = "stk.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    ussd_op: String,
    ussd_out: String,
    stk_view: String,
    stk_op: String,
}

impl Page {
    fn tabs(&self) -> Vec<(&'static str, &'static str, bool)> {
        vec![
            ("ussd", "USSD", true),
            ("stk", self.l.pick("STK Menu", "STK 菜单"), false),
        ]
    }
}

async fn page(State(app): State<Shared>, req: Request) -> axum::response::Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let shell = app
        .hub
        .read(|st| crate::shell::Shell::new(&headers, PATH).live(st, &ctx, None));
    let r = Rq {
        l: ctx.lang,
        remote: ctx.remote,
        who: Caller::of(&app, &req),
        sig: Sig::default(),
    };
    let html = Page {
        shell,
        l: ctx.lang,
        ussd_op: area("stk-u", &r, "", &Phase::Idle),
        ussd_out: ussd_out(ctx.lang, None),
        stk_view: view(ctx.lang, &[], None),
        stk_op: area("stk-m", &r, "", &Phase::Idle),
    }
    .render()
    .unwrap();
    crate::assets::maybe_gzip(&headers, "text/html; charset=utf-8", html.into_bytes(), "no-store")
}

// ---- USSD ----

/// The carrier's answer (`/api/ussd/send|respond` data).
#[derive(Debug, PartialEq)]
pub struct Reply {
    text: String,
    status: i64,
    active: bool,
}

pub fn reply_of(v: &Value) -> Reply {
    Reply {
        text: v["response"].as_str().unwrap_or("").to_owned(),
        status: v["status"]
            .as_i64()
            .or_else(|| v["status"].as_str().and_then(|s| s.parse().ok()))
            .unwrap_or(-1),
        active: v["session_active"].as_bool() == Some(true),
    }
}

/// The modem answered ERROR: the agent passes that through as a reply (status -1).
fn modem_error(r: &Reply) -> bool {
    r.status == -1 && r.text.to_ascii_uppercase().contains("ERROR")
}

fn status_words(s: i64, l: Lang) -> (&'static str, String) {
    match s {
        0 => ("ok", l.pick("Done", "已完成").into()),
        1 => ("ok", l.pick("Waiting for your reply", "等你回复").into()),
        2 => ("neutral", l.pick("Ended by the network", "运营商已结束会话").into()),
        -1 => ("warn", l.pick("No USSD reply from the network", "运营商没有回 USSD").into()),
        n => (
            "neutral",
            match l {
                Lang::En => format!("Status {n}"),
                Lang::Zh => format!("状态 {n}"),
            },
        ),
    }
}

/// `#ussd-out`: nothing yet, or the reply with what to do next.
fn ussd_out(l: Lang, r: Option<&Reply>) -> String {
    let Some(r) = r else {
        return format!(
            "<div id=\"ussd-out\" class=\"nd-group\"><div class=\"nd-row stk-t2\">{}</div></div>",
            l.pick(
                "No result yet. Send a code to see the carrier's reply.",
                "还没有结果。发送一个代码，运营商的回复会显示在这里。"
            )
        );
    };
    let (tone, words) = status_words(r.status, l);
    let text = if r.text.trim().is_empty() { "—" } else { r.text.trim() };
    let mut h = format!(
        "<div id=\"ussd-out\" class=\"nd-group\"><div class=\"nd-row stk-body\"><p class=\"nd-body stk-text\" data-data>{}</p><p class=\"nd-aux stk-meta\">{}{}</p></div>",
        esc(text),
        mark(tone, &words),
        if r.active {
            format!("<span>{}</span>", l.pick("Session active", "会话进行中"))
        } else {
            String::new()
        }
    );
    if r.active {
        h += &format!(
            "<div class=\"nd-row stk-body\"><label class=\"stk-pane stk-tight stk-full\" for=\"ussd-reply\"><span class=\"nd-row__label\">{}</span><span class=\"stk-inline\"><input id=\"ussd-reply\" class=\"nd-field nd-mono stk-grow\" inputmode=\"tel\" autocomplete=\"off\" placeholder=\"{}\" data-bind:reply data-on:keydown=\"evt.key === 'Enter' &amp;&amp; @post('/router/stk/ussd/reply-ask')\"><button type=\"button\" class=\"nd-btn nd-btn--primary\" disabled data-attr:disabled=\"!$reply.trim()\" data-on:click=\"@post('/router/stk/ussd/reply-ask')\">{}</button><button type=\"button\" class=\"nd-btn nd-btn--secondary\" aria-label=\"{}\" data-on:click=\"@post('/router/stk/ussd/end-ask')\">{}{}</button></span></label></div>",
            l.pick("USSD reply", "USSD 回复"),
            l.pick("Enter reply…", "输入回复…"),
            l.pick("Reply", "回复"),
            l.pick("End USSD session", "结束 USSD 会话"),
            crate::shell::icon("X", false, 20, ""),
            l.pick("End", "结束"),
        );
    } else {
        h += &format!(
            "<div class=\"nd-row\"><button type=\"button\" class=\"nd-btn nd-btn--secondary nd-btn--sm\" data-on:click=\"$code = ''; $reply = ''; @post('/router/stk/ussd/new')\">{}</button></div>",
            l.pick("New query", "新建查询")
        );
    }
    h + "</div>"
}

/// Empties the reply box after a good reply (a new value of `_x` makes the span run again).
fn clear_reply() -> String {
    format!(
        "<span id=\"ussd-clr\" hidden data-init=\"$reply = ''; $_x = '{}'\"></span>",
        write::next_op_id()
    )
}

pub async fn ussd_ask(State(app): State<Shared>, req: Request) -> impl IntoResponse {
    let r = rq(&app, req).await;
    let l = r.l;
    let code = r.sig.code.trim().to_owned();
    let e = if code.is_empty() {
        Some(l.pick("Enter a USSD code.", "请输入 USSD 代码。"))
    } else if !ussd_ok(&code) {
        Some(l.pick("Only digits and * # + can be sent.", "只能发送数字和 * # +。"))
    } else {
        None
    };
    if let Some(e) = e {
        return one(format!("{}\n{}", err_html(Some(e)), area("stk-u", &r, "", &Phase::Idle)));
    }
    let c = confirm(
        l,
        match l {
            Lang::En => format!("send {code}"),
            Lang::Zh => format!("发送 {code}"),
        },
        match l {
            Lang::En => format!("The device sends {code} to your carrier. Some codes subscribe to or cancel services, and the carrier may charge for them."),
            Lang::Zh => format!("设备把 {code} 发给运营商。有的代码会订购或退订业务，运营商可能收费。"),
        },
    );
    let set = format!("/router/stk/ussd/send?h={}", tok(&code));
    one(format!("{}\n{}", err_html(None), area("stk-u", &r, &set, &Phase::Ask(c))))
}

#[derive(Deserialize, Default)]
pub struct H {
    #[serde(default)]
    h: String,
}

fn changed(l: Lang) -> Phase {
    Phase::Done(Outcome::NotYet(
        l.pick(
            "Not sent: the text changed after you confirmed. Press the button again.",
            "没有发出：确认之后内容又改了，请再点一次按钮。",
        )
        .into(),
    ))
}

pub async fn ussd_send(
    State(app): State<Shared>,
    Query(q): Query<H>,
    req: Request,
) -> impl IntoResponse {
    let r = rq(&app, req).await;
    let l = r.l;
    let code = r.sig.code.trim().to_owned();
    let p = area("stk-u", &r, "", &pending(l));
    write::respond(p, async move {
        if !ussd_ok(&code) || tok(&code) != q.h {
            return area("stk-u", &r, "", &changed(l));
        }
        match agent::post(&app, &r.who, "/api/ussd/send", &json!({"code": code})).await {
            Ok(v) => format!(
                "{}\n{}\n{}",
                err_html(None),
                ussd_out(l, Some(&reply_of(&v))),
                area("stk-u", &r, "", &Phase::Idle)
            ),
            Err(f) => area("stk-u", &r, "", &Phase::Done(failed(f, l))),
        }
    })
}

pub async fn reply_ask(State(app): State<Shared>, req: Request) -> impl IntoResponse {
    let r = rq(&app, req).await;
    let l = r.l;
    let text = r.sig.reply.trim().to_owned();
    if text.is_empty() {
        return one(area("stk-u", &r, "", &Phase::Idle));
    }
    if !ussd_ok(&text) {
        return one(format!(
            "{}\n{}",
            err_html(Some(l.pick("Only digits and * # + can be sent.", "只能发送数字和 * # +。"))),
            area("stk-u", &r, "", &Phase::Idle)
        ));
    }
    let c = confirm(
        l,
        match l {
            Lang::En => format!("reply {text}"),
            Lang::Zh => format!("回复 {text}"),
        },
        match l {
            Lang::En => format!("The device answers the carrier's menu with {text}. The carrier may charge for what it selects."),
            Lang::Zh => format!("设备用 {text} 回答运营商的菜单。选到的业务运营商可能收费。"),
        },
    );
    let set = format!("/router/stk/ussd/reply?h={}", tok(&text));
    one(format!("{}\n{}", err_html(None), area("stk-u", &r, &set, &Phase::Ask(c))))
}

pub async fn reply_send(
    State(app): State<Shared>,
    Query(q): Query<H>,
    req: Request,
) -> impl IntoResponse {
    let r = rq(&app, req).await;
    let l = r.l;
    let text = r.sig.reply.trim().to_owned();
    let p = area("stk-u", &r, "", &pending(l));
    write::respond(p, async move {
        if !ussd_ok(&text) || tok(&text) != q.h {
            return area("stk-u", &r, "", &changed(l));
        }
        match agent::post(&app, &r.who, "/api/ussd/respond", &json!({"reply": text})).await {
            Ok(v) => {
                let rep = reply_of(&v);
                if modem_error(&rep) {
                    // the agent answered 200 for a modem ERROR: the session has most likely ended
                    return area(
                        "stk-u",
                        &r,
                        "",
                        &Phase::Done(Outcome::Failed(
                            l.pick(
                                "the modem answered ERROR; the session has probably ended. End it and send the code again.",
                                "调制解调器回了 ERROR，会话多半已经结束。结束会话后重新发送代码。",
                            )
                            .into(),
                        )),
                    );
                }
                format!(
                    "{}\n{}\n{}\n{}",
                    err_html(None),
                    ussd_out(l, Some(&rep)),
                    clear_reply(),
                    area("stk-u", &r, "", &Phase::Idle)
                )
            }
            Err(f) => area("stk-u", &r, "", &Phase::Done(failed(f, l))),
        }
    })
}

pub async fn end_ask(State(app): State<Shared>, req: Request) -> impl IntoResponse {
    let r = rq(&app, req).await;
    let l = r.l;
    let c = confirm(
        l,
        l.pick("End USSD session", "结束 USSD 会话").into(),
        l.pick(
            "The device ends this USSD session. To continue later, send the code again.",
            "设备结束这次 USSD 会话。想继续，要重新发送代码。",
        )
        .into(),
    );
    one(area("stk-u", &r, "/router/stk/ussd/end", &Phase::Ask(c)))
}

pub async fn end_send(State(app): State<Shared>, req: Request) -> impl IntoResponse {
    let r = rq(&app, req).await;
    let l = r.l;
    let p = area("stk-u", &r, "", &pending(l));
    write::respond(p, async move {
        match agent::post(&app, &r.who, "/api/ussd/cancel", &json!({})).await {
            Ok(_) => format!(
                "{}\n{}\n{}",
                ussd_out(l, None),
                clear_reply(),
                area(
                    "stk-u",
                    &r,
                    "",
                    &Phase::Done(Outcome::AcceptedSaid(
                        l.pick("USSD session ended.", "USSD 会话已结束。").into()
                    ))
                )
            ),
            Err(f) => area("stk-u", &r, "", &Phase::Done(failed(f, l))),
        }
    })
}

/// 新建查询: the signals were cleared by the button; this clears the reply and the notes.
pub async fn ussd_new(State(app): State<Shared>, req: Request) -> impl IntoResponse {
    let r = rq(&app, req).await;
    one(format!(
        "{}\n{}\n{}",
        err_html(None),
        ussd_out(r.l, None),
        area("stk-u", &r, "", &Phase::Idle)
    ))
}

pub async fn ussd_cancel(
    Query(q): Query<write::CancelQuery>,
    State(app): State<Shared>,
    req: Request,
) -> impl IntoResponse {
    let r = rq(&app, req).await;
    one(area("stk-u", &r, "", &write::cancel_phase(&q)))
}

// ---- STK ----

/// What the SIM answered, in the order the page shows it.
#[derive(Debug, PartialEq)]
enum Stk {
    /// A menu (title, items).
    Menu(Menu),
    /// `supported: false`, with the agent's reason.
    Unsupported(String),
    /// A text the SIM wants shown.
    Display(String),
}

fn items_of(v: &Value) -> Vec<Item> {
    v["items"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|i| {
                    Some(Item {
                        id: i["id"].as_u64().or_else(|| i["id"].as_str()?.parse().ok())?,
                        l: i["label"].as_str().unwrap_or("").to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn menu_of(v: &Value, l: Lang) -> Menu {
    Menu {
        t: v["title"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(l.pick("Menu", "菜单"))
            .to_owned(),
        i: items_of(v),
    }
}

fn stk_of(v: &Value, l: Lang) -> Stk {
    if v["supported"].as_bool() == Some(false) {
        return Stk::Unsupported(v["reason"].as_str().unwrap_or("").to_owned());
    }
    if v["type"] == "display" {
        return Stk::Display(v["data"].as_str().unwrap_or("").to_owned());
    }
    Stk::Menu(menu_of(v, l))
}

/// `+CUSATE: 0"text"OK` → `text`; None when there is no quoted text.
pub fn quoted(s: &str) -> Option<&str> {
    let a = s.find('"')? + 1;
    let n = s[a..].find('"')?;
    (n > 0).then(|| &s[a..a + n])
}

/// `#stk-view`: the load button, "not supported", or the current menu.
fn view(l: Lang, path: &[Menu], unsupported: Option<&str>) -> String {
    if let Some(_reason) = unsupported {
        return format!(
            "<div id=\"stk-view\" class=\"nd-group\"><div class=\"nd-row stk-wrap\"><span class=\"stk-grow\">{}</span><button type=\"button\" class=\"nd-btn nd-btn--secondary nd-btn--sm\" data-on:click=\"@post('/router/stk/menu/ask?k=load')\">{}</button></div></div>",
            mark("neutral", l.pick("This SIM has no STK menu.", "这张 SIM 没有 STK 菜单。")),
            l.pick("Try again", "再试一次")
        );
    }
    let Some(cur) = path.last() else {
        return format!(
            "<div id=\"stk-view\" class=\"nd-group\"><div class=\"nd-row stk-body\"><p class=\"nd-body stk-t2\">{}</p><div><button type=\"button\" class=\"nd-btn nd-btn--primary\" data-on:click=\"@post('/router/stk/menu/ask?k=load')\">{}</button></div></div></div>",
            l.pick(
                "Open the SIM's STK menu to reach the carrier's services.",
                "打开 SIM 的 STK 菜单，进入运营商的服务。"
            ),
            l.pick("Load STK Menu", "加载 STK 菜单")
        );
    };
    let mut h = String::from("<div id=\"stk-view\" class=\"grid gap-2\">");
    if path.len() > 1 {
        h += &format!(
            "<nav aria-label=\"{}\" class=\"nd-aux stk-crumbs\">",
            l.pick("Menu path", "菜单路径")
        );
        for (n, m) in path.iter().enumerate() {
            let last = n + 1 == path.len();
            if n > 0 {
                h += "<span aria-hidden=\"true\">/</span>";
            }
            h += &format!(
                "<span{}{} data-data>{}</span>",
                if last { " class=\"stk-here\"" } else { "" },
                if last { " aria-current=\"page\"" } else { "" },
                esc(&m.t)
            );
        }
        h += "</nav>";
    }
    h += &format!("<div class=\"stk-wrap\"><h3 class=\"nd-body stk-grow stk-title\" data-data>{}</h3>", esc(&cur.t));
    if path.len() > 1 {
        h += &format!(
            "<button type=\"button\" class=\"nd-btn nd-btn--secondary nd-btn--sm\" data-on:click=\"@post('/router/stk/back')\">{}{}</button>",
            crate::shell::icon("CaretLeft", false, 18, ""),
            l.pick("Back", "返回")
        );
    }
    h += &format!(
        "<button type=\"button\" class=\"nd-btn nd-btn--ghost\" aria-label=\"{0}\" title=\"{0}\" data-on:click=\"@post('/router/stk/menu/ask?k=reload')\">{1}</button></div>",
        l.pick("Reload menu", "重新加载菜单"),
        crate::shell::icon("ArrowClockwise", false, 20, "")
    );
    h += "<div class=\"nd-group\">";
    if cur.i.is_empty() {
        h += &format!("<div class=\"nd-row stk-t2\">{}</div>", l.pick("No items.", "没有菜单项。"));
    }
    for it in &cur.i {
        h += &format!(
            "<button type=\"button\" class=\"nd-row stk-item\" data-on:click=\"@post('/router/stk/item/ask?id={}')\"><span class=\"nd-row__text\"><span class=\"nd-row__label\" data-data>{}</span></span>{}</button>",
            it.id,
            esc(&it.l),
            crate::shell::icon("CaretRight", false, 16, "nd-row__chev")
        );
    }
    h + "</div></div>"
}

/// `#stk-sig`: sets the tab's menu path.
fn sig_span(path: &[Menu]) -> String {
    format!(
        "<span id=\"stk-sig\" hidden data-init=\"$stk = {}; $_x = '{}'\"></span>",
        esc(&js_json(&path)),
        write::next_op_id()
    )
}

fn disp(l: Lang, text: Option<&str>) -> String {
    let Some(t) = text else {
        return "<section id=\"stk-disp\" aria-live=\"polite\"></section>".into();
    };
    let shown = quoted(t);
    let mut h = format!(
        "<section id=\"stk-disp\" aria-live=\"polite\" class=\"grid gap-2\"><h3 class=\"nd-group-title\">{}</h3><div class=\"nd-group\"><div class=\"nd-row\"><p class=\"nd-body stk-text\" data-data>{}</p></div></div>",
        l.pick("SIM reply", "SIM 卡的回复"),
        match shown {
            Some(s) => esc(s),
            None if t.trim().is_empty() => l.pick("No response", "没有回复").into(),
            None => esc(t.trim()),
        }
    );
    if shown.is_some() {
        h += &format!(
            "<div class=\"stk-raw\" role=\"group\" aria-label=\"{}\"><pre data-data>{}</pre></div>",
            l.pick("Raw modem reply", "调制解调器原始回复"),
            esc(t.trim())
        );
    }
    h + "</section>"
}

/// `#stk-diag`: what the agent said about the SIM's state (`source`, `reason`, `diagnostics`).
fn diag(l: Lang, v: &Value) -> String {
    let mut lines: Vec<String> = Vec::new();
    if let Some(s) = v["source"].as_str().filter(|s| !s.is_empty()) {
        lines.push(format!("source: {s}"));
    }
    if let Some(s) = v["reason"].as_str().filter(|s| !s.is_empty()) {
        lines.push(format!("reason: {s}"));
    }
    if let Some(a) = v["diagnostics"].as_array() {
        lines.extend(a.iter().filter_map(|x| x.as_str().map(str::to_owned)));
    }
    if lines.is_empty() {
        return "<div id=\"stk-diag\"></div>".into();
    }
    format!(
        "<div id=\"stk-diag\"><div class=\"stk-raw\" role=\"group\" aria-label=\"{}\" tabindex=\"0\"><pre data-data>{}</pre></div></div>",
        l.pick("STK diagnostics", "STK 诊断"),
        esc(&lines.join("\n"))
    )
}

#[derive(Deserialize, Default)]
pub struct MenuQ {
    #[serde(default)]
    k: String,
}

pub async fn menu_ask(
    State(app): State<Shared>,
    Query(q): Query<MenuQ>,
    req: Request,
) -> impl IntoResponse {
    let r = rq(&app, req).await;
    let l = r.l;
    let title = if q.k == "reload" {
        l.pick("Reload menu", "重新加载菜单")
    } else {
        l.pick("Load STK Menu", "加载 STK 菜单")
    };
    let c = confirm(
        l,
        title.into(),
        l.pick(
            "The device asks the SIM card for its menu (up to about 20 s). This switches the modem's SIM Toolkit handling on; it does not order anything.",
            "设备向 SIM 卡要它的菜单（最长约 20 秒）。这会打开调制解调器的 SIM 工具包处理；不会订购任何业务。",
        )
        .into(),
    );
    one(area("stk-m", &r, "/router/stk/menu/load", &Phase::Ask(c)))
}

pub async fn menu_load(State(app): State<Shared>, req: Request) -> impl IntoResponse {
    let r = rq(&app, req).await;
    let l = r.l;
    let p = area("stk-m", &r, "", &pending(l));
    write::respond(p, async move {
        match agent::get_slow(&app, &r.who, "/api/stk/menu", MENU_TIMEOUT).await {
            Err(f) => area("stk-m", &r, "", &Phase::Done(Outcome::Failed(f.text(l)))),
            Ok(v) => {
                let (path, bad) = match stk_of(&v, l) {
                    Stk::Unsupported(_) => (vec![], Some("")),
                    Stk::Menu(m) => (vec![m], None),
                    Stk::Display(_) => (vec![], None),
                };
                format!(
                    "{}\n{}\n{}\n{}\n{}",
                    view(l, &path, bad),
                    sig_span(&path),
                    disp(l, None),
                    diag(l, &v),
                    area("stk-m", &r, "", &Phase::Idle)
                )
            }
        }
    })
}

#[derive(Deserialize, Default)]
pub struct ItemQ {
    #[serde(default)]
    id: u64,
}

fn gone(l: Lang) -> Phase {
    Phase::Done(Outcome::NotYet(
        l.pick(
            "That entry is not in the menu any more. Reload the menu.",
            "这一项已经不在菜单里了。请重新加载菜单。",
        )
        .into(),
    ))
}

pub async fn item_ask(
    State(app): State<Shared>,
    Query(q): Query<ItemQ>,
    req: Request,
) -> impl IntoResponse {
    let r = rq(&app, req).await;
    let l = r.l;
    let Some(it) = r.sig.stk.last().and_then(|m| m.i.iter().find(|i| i.id == q.id)) else {
        return one(area("stk-m", &r, "", &gone(l)));
    };
    let label = &it.l;
    let c = confirm(
        l,
        match l {
            Lang::En => format!("open {label}"),
            Lang::Zh => format!("打开 {label}"),
        },
        match l {
            Lang::En => format!("The device selects “{label}” on the SIM card. Some entries send an SMS or subscribe to a service, and the carrier may charge for it."),
            Lang::Zh => format!("设备在 SIM 卡上选择「{label}」。有的菜单项会发短信或订购业务，运营商可能收费。"),
        },
    );
    one(area("stk-m", &r, &format!("/router/stk/item/select?id={}", q.id), &Phase::Ask(c)))
}

pub async fn item_select(
    State(app): State<Shared>,
    Query(q): Query<ItemQ>,
    req: Request,
) -> impl IntoResponse {
    let r = rq(&app, req).await;
    let l = r.l;
    let p = area("stk-m", &r, "", &pending(l));
    write::respond(p, async move {
        if !r.sig.stk.last().is_some_and(|m| m.i.iter().any(|i| i.id == q.id)) {
            return area("stk-m", &r, "", &gone(l));
        }
        match agent::post(&app, &r.who, "/api/stk/select", &json!({"item_id": q.id})).await {
            Err(f) => area("stk-m", &r, "", &Phase::Done(failed(f, l))),
            Ok(v) => {
                let mut path = r.sig.stk.clone();
                let (mut un, mut text) = (None, None);
                match stk_of(&v, l) {
                    Stk::Unsupported(_) => {
                        path.clear();
                        un = Some("");
                    }
                    Stk::Menu(m) => path.push(m),
                    Stk::Display(t) => text = Some(t),
                }
                format!(
                    "{}\n{}\n{}\n{}",
                    view(l, &path, un),
                    sig_span(&path),
                    disp(l, text.as_deref()),
                    area("stk-m", &r, "", &Phase::Idle)
                )
            }
        }
    })
}

/// 返回: the previous menu; nothing is asked of the modem.
pub async fn back(State(app): State<Shared>, req: Request) -> impl IntoResponse {
    let r = rq(&app, req).await;
    let mut path = r.sig.stk.clone();
    if path.len() > 1 {
        path.pop();
    }
    one(format!(
        "{}\n{}\n{}\n{}",
        view(r.l, &path, None),
        sig_span(&path),
        disp(r.l, None),
        area("stk-m", &r, "", &Phase::Idle)
    ))
}

pub async fn menu_cancel(
    Query(q): Query<write::CancelQuery>,
    State(app): State<Shared>,
    req: Request,
) -> impl IntoResponse {
    let r = rq(&app, req).await;
    one(area("stk-m", &r, "", &write::cancel_phase(&q)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cjk(s: &str) -> bool {
        s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c) || ('\u{3000}'..='\u{303f}').contains(&c) || ('\u{ff00}'..='\u{ffef}').contains(&c))
    }

    #[test]
    fn only_digits_star_hash_plus_go_out() {
        assert!(ussd_ok("*#100#") && ussd_ok("+886") && ussd_ok("1"));
        assert!(!ussd_ok("") && !ussd_ok("*100# ") && !ussd_ok("*1;0#") && !ussd_ok("AT"));
    }

    #[test]
    fn replies_read_the_agent_shape() {
        let r = reply_of(&json!({"response":"menu","status":1,"session_active":true}));
        assert_eq!((r.status, r.active), (1, true));
        let e = reply_of(&json!({"response":"AT+CUSD=1,\"1\",15ERROR","status":-1,"session_active":false}));
        assert!(modem_error(&e));
        assert!(!modem_error(&reply_of(&json!({"response":"ok","status":0}))));
        assert_eq!(status_words(2, Lang::En).1, "Ended by the network");
        assert_eq!(status_words(7, Lang::Zh).1, "状态 7");
    }

    #[test]
    fn the_quoted_text_of_a_select_reply() {
        assert_eq!(quoted("+CUSATE: 0\"您的话费余额\"OK"), Some("您的话费余额"));
        assert_eq!(quoted("+CUSATE: 0OK"), None);
        assert_eq!(quoted("\"\""), None);
    }

    #[test]
    fn the_menu_path_survives_dollar_and_at_signs() {
        let p = vec![Menu { t: "$5 @plan".into(), i: vec![Item { id: 3, l: "a'b\"c".into() }] }];
        let js = js_json(&p);
        assert!(!js.contains('$') && !js.contains('@'), "{js}");
        // the span holds it in an attribute; reading it back gives the same menu
        let s = sig_span(&p);
        assert!(s.contains("id=\"stk-sig\"") && !s.contains("$5"), "{s}");
        let back: Vec<Menu> = serde_json::from_str(&js).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn menus_and_shapes() {
        let l = Lang::En;
        let v = json!({"supported":true,"title":"Home","items":[{"id":1,"label":"A"},{"id":"2","label":"B"}],"source":"at_cusatd"});
        let Stk::Menu(m) = stk_of(&v, l) else { panic!() };
        assert_eq!((m.t.as_str(), m.i.len()), ("Home", 2));
        assert!(matches!(stk_of(&json!({"supported":false,"reason":"x"}), l), Stk::Unsupported(r) if r == "x"));
        assert!(matches!(stk_of(&json!({"type":"display","data":"d"}), l), Stk::Display(d) if d == "d"));
        // a sub-menu with no items opens and says so
        let Stk::Menu(e) = stk_of(&json!({"type":"menu","title":"","items":[]}), l) else { panic!() };
        assert_eq!(e.t, "Menu");
        let h = view(l, &[m.clone(), e], None);
        assert!(h.contains("No items.") && h.contains("Back") && h.contains("aria-current=\"page\""), "{h}");
        let top = view(l, &[m], None);
        assert!(top.contains("stk-item") && !top.contains(">Back<"), "{top}");
    }

    #[test]
    fn device_text_is_escaped() {
        let m = Menu { t: "<b>x</b>".into(), i: vec![Item { id: 1, l: "<i>".into() }] };
        let h = view(Lang::En, &[m], None);
        assert!(!h.contains("<b>x") && !h.contains("<i>"), "{h}");
        let o = ussd_out(Lang::En, Some(&Reply { text: "<script>".into(), status: 0, active: false }));
        assert!(!o.contains("<script>"), "{o}");
    }

    #[test]
    fn the_english_page_has_no_chinese() {
        let l = Lang::En;
        let r = Rq { l, remote: Via::Local, who: Caller { token: None, peer: None, sid: None }, sig: Sig::default() };
        let m = Menu { t: "Home".into(), i: vec![Item { id: 1, l: "A".into() }] };
        let mut all = vec![
            ussd_out(l, None),
            ussd_out(l, Some(&Reply { text: "hi".into(), status: 1, active: true })),
            ussd_out(l, Some(&Reply { text: "hi".into(), status: 2, active: false })),
            ussd_out(l, Some(&Reply { text: "".into(), status: -1, active: false })),
            view(l, &[], None),
            view(l, &[], Some("")),
            view(l, &[m.clone(), m], None),
            disp(l, Some("+CUSATE: 0\"x\"OK")),
            disp(l, Some("")),
            diag(l, &json!({"reason":"r"})),
            err_html(Some("Enter a USSD code.")),
            area("stk-u", &r, "", &pending(l)),
            area("stk-u", &r, "", &changed(l)),
            area("stk-m", &r, "", &gone(l)),
            area("stk-u", &r, "", &Phase::Done(Outcome::Unknown)),
        ];
        for t in [
            confirm(l, "send *#100#".into(), "x".into()),
            confirm(l, "open A".into(), "y".into()),
        ] {
            all.push(area("stk-u", &r, "/x", &Phase::Ask(t)));
        }
        let html = all.join("\n");
        assert!(!cjk(&html), "{html}");
    }
}
