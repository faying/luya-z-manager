//! /tools/at (AT terminal; batch 5c), the old `tools/at/page.tsx`: any AT command can be typed and
//! goes straight to the modem; the reply is the result (no readback).
//!
//! ```text
//!  GET page ─▶ #at-port "looking…" ─▶ data-init GET port ─▶ /api/at/port (probes the ports, ~7 s
//!        at worst; read once, and again on 「再查一次」, never polled)
//!  发送 ─▶ POST send (signal cmd) ─▶ must start with AT; ' ` $ ; | & dropped like the agent does
//!        ─▶ a risky command? ─▶ tier 3 dialog (POST confirm?h=<hash of the command it named>)
//!        ─▶ otherwise tier 1: sent at once ─▶ /api/at/send {command, timeout: 3}
//!        ─▶ the answer is appended to #at-log (a transcript in the tab), the command goes on top of
//!           #at-hist-list; ↑ / ↓ recall from that list.
//! ```
//! - **The history lives in the tab's DOM**, nowhere on the server and nothing shared with other
//!   sessions: the reply of a send carries the new log entry (`mode append`) and the new history
//!   button (`mode prepend`); a reload empties both, like the old page.
//! - **Tiers kept from the old page**: a normal command runs directly (the old page's own rule;
//!   the inventory lists it as tier 2), a risky one asks in a dialog. The old list is six patterns
//!   (`CFUN=0`, `+CRESET`, `&F`, `+NVWR`, `+QPOWD`, `+COPS=`); the inventory asks to widen it, so
//!   any `+CFUN=`, `+CGDCONT=` (APN), `+CPIN=`, `+CLCK=`, `+CPWD=` and `+CPWROFF` ask too (a
//!   stronger confirm only). The check runs on what was typed and on what will be sent (the agent
//!   removes `& ;` etc. first, so `AT&F` is sent as `ATF` and must not slip past).
//! - **No example commands in placeholders or buttons.** The box shows `AT+CGMR` (reads the
//!   revision) only. Nothing on the page offers `AT+CFUN`: the radio's low-power state is
//!   recoverable only by a reboot.
//! - A send whose reply is lost is never repeated (`retryMaxCount: 0`) and says so; the area's
//!   `data-write="at"` listens for it.

use std::convert::Infallible;
use std::hash::{Hash, Hasher};

use askama::Template;
use axum::Router;
use axum::extract::{Query, Request, State};
use axum::response::IntoResponse;
use axum::response::sse::{Event, Sse};
use axum::routing::{get, post};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::topbar::Via;
use crate::agent::{self, Caller, Fail, esc};
use crate::i18n::Lang;
use crate::pages::{Shared, async_stream};
use crate::write::{self, Confirm, Outcome, Phase, Tier};

pub const PATH: &str = "/tools/at";
const AREA: &str = "at";
const SET: &str = "/tools/at/send";
const PORT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(25);

pub fn routes() -> Router<Shared> {
    Router::new()
        .route(PATH, get(page))
        .route("/tools/at/port", get(port))
        .route("/tools/at/send", post(send))
        .route("/tools/at/confirm", post(confirm_send))
        .route("/tools/at/cancel", post(cancel))
}

// ---- the command ----

/// The agent's filter (`at_terminal.rs`): these characters are dropped before sending.
pub fn sanitize(cmd: &str) -> String {
    cmd.chars().filter(|c| !matches!(c, '\'' | '`' | '$' | ';' | '|' | '&')).collect()
}

fn starts_with_at(s: &str) -> bool {
    s.get(..2).is_some_and(|p| p.eq_ignore_ascii_case("AT"))
}

/// A `+CFUN=` set (anything but the `=?` test): refused, not asked. The radio's low-power state
/// comes back only with a restart (CLAUDE.md, re-checked on B31), and this device is someone's only
/// uplink abroad (owner, 10-13). u60d refuses it too.
pub fn radio_set(raw: &str, sent: &str) -> bool {
    let set = |s: &str| {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_uppercase();
        s.match_indices("CFUN=").any(|(i, _)| !s[i + 5..].starts_with('?'))
    };
    set(raw) || set(sent)
}

fn radio_refused(l: Lang) -> &'static str {
    l.pick(
        "Not sent: AT+CFUN can put the modem in low-power state, which only a device restart undoes. Restart the device from the Device page instead.",
        "没有发出：AT+CFUN 可能让调制解调器停在低功耗状态，只有重启设备才能恢复。要重启请到「设备控制」页。",
    )
}

/// Why a command asks first, and how to get back from it. Words: (reason, recovery).
pub fn danger(raw: &str, sent: &str, l: Lang) -> Option<(&'static str, &'static str)> {
    let norm = |s: &str| {
        s.chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_ascii_uppercase()
    };
    let (a, b) = (norm(raw), norm(sent));
    let hit = |p: &str| a.contains(p) || b.contains(p);
    Some(if hit("CFUN=0") {
        (
            l.pick("CFUN=0 will shut down the modem radio", "CFUN=0 会关掉调制解调器的射频"),
            l.pick(
                "Restart the device. The modem stays in low-power state until then; AT commands do not bring it back.",
                "重启设备。在那之前调制解调器一直停在低功耗状态，AT 命令救不回来。",
            ),
        )
    } else if hit("+CRESET") {
        (
            l.pick("+CRESET will reset the modem", "+CRESET 会重置调制解调器"),
            l.pick(
                "The modem comes back by itself after about a minute. If it doesn't, restart the device.",
                "调制解调器约一分钟后自己回来。不回来就重启设备。",
            ),
        )
    } else if hit("&F") {
        (
            l.pick("&F will factory-reset modem settings", "&F 会把调制解调器设置恢复出厂"),
            l.pick(
                "Modem settings changed with AT commands are lost and can't be restored from this page.",
                "用 AT 命令改过的调制解调器设置会丢，在这个页面里恢复不了。",
            ),
        )
    } else if hit("+NVWR") {
        (
            l.pick("+NVWR writes to non-volatile memory", "+NVWR 会写入非易失存储"),
            l.pick(
                "The old value can't be restored from this page. A wrong value can stop the modem from working until it is written back.",
                "旧值在这个页面里恢复不了。写错可能让调制解调器不能工作，直到写回去。",
            ),
        )
    } else if hit("+QPOWD") {
        (
            l.pick("+QPOWD will power down the modem", "+QPOWD 会关掉调制解调器电源"),
            l.pick("Restart the device to power the modem back up.", "重启设备才能让调制解调器重新上电。"),
        )
    } else if hit("+COPS=") {
        (
            l.pick("+COPS= will change network operator registration", "+COPS= 会改变网络运营商注册"),
            l.pick(
                "Send AT+COPS=0 to go back to automatic operator selection.",
                "发 AT+COPS=0 回到自动选择运营商。",
            ),
        )
    } else if hit("+CFUN=") {
        (
            l.pick("+CFUN= changes the modem's functionality level (radio off, airplane, restart)", "+CFUN= 会改变调制解调器的功能级别（关射频、飞行、重启）"),
            l.pick(
                "The modem can stay in low-power state until the device is restarted.",
                "调制解调器可能一直停在低功耗状态，直到重启设备。",
            ),
        )
    } else if hit("+CGDCONT=") {
        (
            l.pick("+CGDCONT= changes a data connection profile (APN)", "+CGDCONT= 会改数据连接配置（APN）"),
            l.pick(
                "The old APN is not kept by this page; set it again on the APN page.",
                "这个页面不会保留旧的 APN；到 APN 页面重新设置。",
            ),
        )
    } else if hit("+CPIN=") || hit("+CLCK=") || hit("+CPWD=") {
        (
            l.pick("This command works with the SIM's PIN or lock; wrong tries can lock the SIM", "这条命令处理 SIM 卡的 PIN 或锁；试错几次可能把 SIM 卡锁住"),
            l.pick(
                "A locked SIM needs its PUK code from the carrier.",
                "锁住的 SIM 卡要向运营商要 PUK 码才能解开。",
            ),
        )
    } else if hit("+CPWROFF") {
        (
            l.pick("+CPWROFF will power off the modem", "+CPWROFF 会关掉调制解调器"),
            l.pick("Restart the device to power the modem back up.", "重启设备才能让调制解调器重新上电。"),
        )
    } else {
        return None;
    })
}

/// A token for what the dialog named, so the confirm sends exactly that.
fn tok(s: &str) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// A JSON string as a JavaScript expression Datastar can read (`$` and `@` escaped).
fn js_str(s: &str) -> String {
    serde_json::to_string(s)
        .unwrap_or_else(|_| "\"\"".into())
        .replace('$', "\\u0024")
        .replace('@', "\\u0040")
}

// ---- fragments ----

fn area(rq: &Rq, set: &str, phase: &Phase) -> String {
    write::Area {
        id: AREA,
        l: rq.l,
        remote: rq.remote,
        set,
        cancel: "/tools/at/cancel",
        phase,
    }
    .html()
}

fn err_html(e: Option<&str>) -> String {
    match e {
        None => "<div id=\"at-err\"></div>".into(),
        Some(e) => format!("<div id=\"at-err\"><p role=\"alert\" class=\"nd-error at-error\">{}</p></div>", esc(e)),
    }
}

/// The port line: looking / available / none answered / could not check.
fn port_html(l: Lang, state: &PortState) -> String {
    let (tone, text, reason) = match state {
        PortState::Looking => ("neutral", l.pick("Looking for the AT port…", "正在找 AT 端口…").to_owned(), String::new()),
        PortState::Found(p) => (
            "ok",
            l.pick("AT port available", "AT 端口可用").to_owned(),
            format!("<span class=\"nd-mono\" data-data>{}</span>", esc(p)),
        ),
        PortState::None => (
            "bad",
            l.pick("No AT port answered", "没有 AT 端口回应").to_owned(),
            l.pick(
                "None of the modem's AT ports replied OK. Commands will fail until one does. Check again in a moment.",
                "调制解调器的 AT 端口都没有回 OK。有一个回应之前，命令都会失败。过一会儿再查。",
            )
            .to_owned(),
        ),
        PortState::Failed(e) => (
            "warn",
            l.pick("Couldn't check the AT port", "没能检查 AT 端口").to_owned(),
            esc(e),
        ),
    };
    let busy = matches!(state, PortState::Looking);
    format!(
        "<div id=\"at-port\"><section class=\"nd-status nd-status--{tone}\"><div class=\"nd-status__main\"><div class=\"nd-status__state\" role=\"status\">{}</div>{}</div><div class=\"nd-status__actions\"><button type=\"button\" class=\"nd-btn nd-btn--secondary\"{} data-on:click=\"@get('/tools/at/port')\">{}{}</button></div></section></div>",
        esc(&text),
        if reason.is_empty() { String::new() } else { format!("<div class=\"nd-status__reason\">{reason}</div>") },
        if busy { " disabled" } else { "" },
        crate::shell::icon("ArrowClockwise", false, 20, ""),
        l.pick("Check again", "再查一次"),
    )
}

#[derive(Debug, PartialEq)]
pub enum PortState {
    Looking,
    Found(String),
    None,
    Failed(String),
}

pub fn port_of(v: &Value) -> PortState {
    match v["port"].as_str().filter(|p| !p.is_empty()) {
        Some(p) if v["available"].as_bool() != Some(false) => PortState::Found(p.to_owned()),
        _ => PortState::None,
    }
}

/// Text from the modem for a `<div>`: no CR, no control characters, no blank lines (the patch
/// carries one `elements` line per line of HTML).
fn modem_text(s: &str) -> String {
    s.chars()
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .collect::<String>()
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The log entry: `> command`, the reply as the modem gave it, how long it took.
pub fn entry(l: Lang, cmd: &str, response: &str, ms: u64) -> String {
    let text = modem_text(response);
    // one HTML line: the reply's own line breaks are kept by `white-space: pre-wrap`, so they are
    // written as character references the patch will not split
    let body = if text.is_empty() {
        l.pick("(no reply)", "（没有回复）").to_owned()
    } else {
        esc(&text).replace('\n', "&#10;")
    };
    format!(
        "<div class=\"at-entry\" data-init=\"el.parentElement.scrollTop = el.parentElement.scrollHeight\"><div class=\"at-cmdline\" data-data>&gt; {}</div><div class=\"at-resp\" data-data>{body}</div><div class=\"at-ms\">{ms} ms</div></div>",
        esc(cmd)
    )
}

/// A history button: puts the command back in the box.
pub fn hist_item(l: Lang, cmd: &str) -> String {
    let label = match l {
        Lang::En => format!("Put {cmd} back in the box"),
        Lang::Zh => format!("把 {cmd} 放回输入框"),
    };
    format!(
        "<li><button type=\"button\" class=\"nd-btn nd-btn--secondary nd-mono at-hbtn\" data-cmd=\"{c}\" aria-label=\"{}\" data-data data-on:click=\"($cmd = el.dataset.cmd, document.getElementById('at-cmd').focus())\">{c}</button></li>",
        esc(&label),
        c = esc(cmd)
    )
}

/// A Datastar patch with its own target and mode.
fn patch_at(selector: &str, mode: &str, html: &str) -> Event {
    let mut d = format!("selector {selector}\nmode {mode}");
    for line in html.lines().filter(|l| !l.trim().is_empty()) {
        d.push_str("\nelements ");
        d.push_str(line);
    }
    Event::default().event("datastar-patch-elements").data(d)
}

// ---- the page ----

#[derive(Template)]
#[template(path = "at.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    port: String,
    op: String,
}

struct Rq {
    l: Lang,
    remote: Via,
    who: Caller,
    cmd: String,
}

#[derive(Deserialize, Default)]
struct Sig {
    #[serde(default)]
    cmd: String,
}

async fn rq(app: &Shared, req: Request) -> Rq {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let who = Caller::of(app, &req);
    let body = axum::body::to_bytes(req.into_body(), 16 * 1024)
        .await
        .map(|b| b.to_vec())
        .unwrap_or_default();
    let sig: Sig = serde_json::from_slice(&body).unwrap_or_default();
    Rq { l: ctx.lang, remote: ctx.remote, who, cmd: sig.cmd }
}

async fn page(State(app): State<Shared>, req: Request) -> axum::response::Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let shell = app
        .hub
        .read(|st| crate::shell::Shell::new(&headers, PATH).live(st, &ctx, None));
    let r = Rq { l: ctx.lang, remote: ctx.remote, who: Caller::of(&app, &req), cmd: String::new() };
    let html = Page {
        shell,
        l: ctx.lang,
        port: port_html(ctx.lang, &PortState::Looking),
        op: area(&r, SET, &Phase::Idle),
    }
    .render()
    .unwrap();
    crate::assets::maybe_gzip(&headers, "text/html; charset=utf-8", html.into_bytes(), "no-store")
}

/// GET port: "looking…" at once, then what the agent found. The agent probes up to five ports.
async fn port(State(app): State<Shared>, req: Request) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    let who = Caller::of(&app, &req);
    Sse::new(async_stream(move |tx| async move {
        if tx.send(crate::pages::patch(&port_html(l, &PortState::Looking))).await.is_err() {
            return;
        }
        let st = match agent::get_slow(&app, &who, "/api/at/port", PORT_TIMEOUT).await {
            Ok(v) => port_of(&v),
            Err(f) => PortState::Failed(f.text(l)),
        };
        let _ = tx.send(crate::pages::patch(&port_html(l, &st))).await;
    }))
}

fn one(html: String) -> impl IntoResponse {
    Sse::new(futures_util::stream::once(async move {
        Ok::<_, Infallible>(crate::pages::patch(&html))
    }))
}

// ---- send ----

/// 发送: check it; a risky command asks first (tier 3), the rest goes out.
async fn send(State(app): State<Shared>, req: Request) -> axum::response::Response {
    let r = rq(&app, req).await;
    let l = r.l;
    let raw = r.cmd.trim().to_owned();
    if raw.is_empty() {
        return axum::http::StatusCode::NO_CONTENT.into_response();
    }
    if !starts_with_at(&raw) {
        return one(format!(
            "{}\n{}",
            err_html(Some(l.pick("Command must start with AT", "命令必须以 AT 开头"))),
            area(&r, SET, &Phase::Idle)
        ))
        .into_response();
    }
    let sent = sanitize(&raw);
    if sent.is_empty() {
        return one(format!(
            "{}\n{}",
            err_html(Some(l.pick("Nothing is left after removing ' ` $ ; | &.", "去掉 ' ` $ ; | & 之后什么都没剩下。"))),
            area(&r, SET, &Phase::Idle)
        ))
        .into_response();
    }
    if radio_set(&raw, &sent) {
        return one(format!("{}\n{}", err_html(Some(radio_refused(l))), area(&r, SET, &Phase::Idle)))
            .into_response();
    }
    if let Some((why, recovery)) = danger(&raw, &sent, l) {
        let c = Confirm {
            tier: Tier::Dialog,
            title: l.pick("Send a risky AT command?", "发送有风险的 AT 命令？").into(),
            what: format!("{why}: {sent}"),
            downtime: Some(
                l.pick(
                    "Depends on the command: the mobile connection may drop until the modem is back.",
                    "看命令而定：移动网络可能断开，直到调制解调器恢复。",
                )
                .into(),
            ),
            recovery: Some(recovery.into()),
            action: l.pick("Send it", "发送").into(),
            cuts_uplink: true,
            word: None,
            danger: true,
            revert: crate::write::Revert::No,
        };
        let set = format!("/tools/at/confirm?h={}", tok(&sent));
        return one(format!("{}\n{}", err_html(None), area(&r, &set, &Phase::Ask(c)))).into_response();
    }
    run(app, r, raw, sent).into_response()
}

#[derive(Deserialize, Default)]
struct H {
    #[serde(default)]
    h: String,
}

/// The dialog's button: sends exactly the command it named, once.
async fn confirm_send(State(app): State<Shared>, Query(q): Query<H>, req: Request) -> axum::response::Response {
    let r = rq(&app, req).await;
    let l = r.l;
    let raw = r.cmd.trim().to_owned();
    let sent = sanitize(&raw);
    if !starts_with_at(&raw) || sent.is_empty() || tok(&sent) != q.h {
        return one(format!(
            "{}",
            area(
                &r,
                SET,
                &Phase::Done(Outcome::NotYet(
                    l.pick(
                        "Not sent: the command changed after you confirmed. Press Send again.",
                        "没有发出：确认之后命令又改了，请再点一次「发送」。",
                    )
                    .into()
                ))
            )
        ))
        .into_response();
    }
    if radio_set(&raw, &sent) {
        return one(format!("{}\n{}", err_html(Some(radio_refused(l))), area(&r, SET, &Phase::Idle)))
            .into_response();
    }
    run(app, r, raw, sent).into_response()
}

fn run(app: Shared, r: Rq, raw: String, sent: String) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let l = r.l;
    Sse::new(async_stream(move |tx| async move {
        let wait = area(&r, SET, &Phase::Pending(l.pick("Sending…", "正在发送…").into()));
        if tx.send(crate::pages::patch(&format!("{}\n{}", err_html(None), wait))).await.is_err() {
            // the tab went away before anything was sent: don't send
            return;
        }
        let body = json!({"command": sent, "timeout": 3});
        let res = agent::post(&app, &r.who, "/api/at/send", &body).await;
        let ev = |e: Event| tx.send(e);
        match res {
            Err(f) => {
                let o = match f {
                    Fail::Unreachable => Outcome::Unknown,
                    f => Outcome::Failed(f.text(l)),
                };
                let _ = ev(crate::pages::patch(&area(&r, SET, &Phase::Done(o)))).await;
            }
            Ok(v) => {
                let cmd = v["command"].as_str().filter(|c| !c.is_empty()).unwrap_or(&sent);
                let ms = v["elapsed_ms"].as_u64().unwrap_or(0);
                let resp = v["response"].as_str().unwrap_or("");
                let mut out = vec![
                    patch_at("#at-log", "append", &entry(l, cmd, resp, ms)),
                    patch_at("#at-hist-list", "prepend", &hist_item(l, cmd)),
                    crate::pages::patch(&format!(
                        "<span id=\"at-last\" class=\"nd-aux\">{}</span>\n<span id=\"at-clear\" hidden data-init=\"$cmd.trim() === {} &amp;&amp; ($cmd = '')\"></span>\n{}",
                        match l {
                            Lang::En => format!("Last response in {ms} ms"),
                            Lang::Zh => format!("上次响应耗时 {ms} ms"),
                        },
                        esc(&js_str(&raw)),
                        area(&r, SET, &Phase::Idle)
                    )),
                    patch_at("#at-log-empty", "remove", ""),
                    patch_at("#at-hist-empty", "remove", ""),
                ];
                if let Some(p) = v["port"].as_str().filter(|p| !p.is_empty()) {
                    out.push(crate::pages::patch(&port_html(l, &PortState::Found(p.to_owned()))));
                }
                for e in out {
                    if ev(e).await.is_err() {
                        break;
                    }
                }
            }
        }
    }))
}

async fn cancel(Query(q): Query<write::CancelQuery>, State(app): State<Shared>, req: Request) -> impl IntoResponse {
    let r = rq(&app, req).await;
    one(area(&r, SET, &write::cancel_phase(&q)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cjk(s: &str) -> bool {
        s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c) || ('\u{3000}'..='\u{303f}').contains(&c) || ('\u{ff00}'..='\u{ffef}').contains(&c))
    }

    #[test]
    fn the_agents_filter_and_the_at_prefix() {
        assert_eq!(sanitize("AT+CF;UN=0 | $x"), "AT+CFUN=0  x");
        assert!(starts_with_at("at+cgmr") && starts_with_at("AT") && !starts_with_at("A") && !starts_with_at("+CGMR"));
        assert!(!starts_with_at("é"), "a multi-byte start must not panic");
    }

    #[test]
    fn the_old_six_still_ask_and_the_rest_do_not() {
        let l = Lang::En;
        for c in ["AT+CFUN=0", "at+cfun = 0", "AT+CRESET", "AT&F", "AT+NVWR=1,2", "AT+QPOWD=1", "AT+COPS=1,2,\"46692\""] {
            assert!(danger(c, &sanitize(c), l).is_some(), "{c}");
        }
        // the filter hides the pattern in the sent form, the raw text still counts
        assert!(danger("AT+CF;UN=0", &sanitize("AT+CF;UN=0"), l).is_some());
        assert!(danger("AT&F", &sanitize("AT&F"), l).is_some());
        // widened (inventory): other CFUN sets, APN, PIN
        for c in ["AT+CFUN=4", "AT+CFUN=1,1", "AT+CGDCONT=1,\"IP\",\"x\"", "AT+CPIN=\"1234\"", "AT+CLCK=\"SC\",1,\"0000\"", "AT+CPWROFF"] {
            assert!(danger(c, &sanitize(c), l).is_some(), "{c}");
        }
        for c in ["AT", "ATI", "AT+CGMR", "AT+CFUN?", "AT+COPS?", "AT+CGDCONT?", "AT+CPIN?", "AT+CSQ"] {
            assert!(danger(c, &sanitize(c), l).is_none(), "{c}");
        }
        // CFUN sets are refused before they can ask; reads and the test form are not
        for c in ["AT+CFUN=0", "at+cfun = 4", "AT+CFUN=1,1", "AT+CF;UN=0", "AT+CSQ+CFUN=0", "AT+CFUN=?+CFUN=0"] {
            assert!(radio_set(c, &sanitize(c)), "{c}");
        }
        for c in ["AT+CFUN?", "AT+CFUN=?", "ATI", "AT+CSQ"] {
            assert!(!radio_set(c, &sanitize(c)), "{c}");
        }
        // CFUN=0 is the first reason, even next to other patterns
        assert!(danger("AT+CFUN=0", "AT+CFUN=0", l).unwrap().0.starts_with("CFUN=0"));
        assert_eq!(tok("AT+X"), tok("AT+X"));
        assert_ne!(tok("AT+X"), tok("AT+Y"));
    }

    #[test]
    fn a_log_entry_is_one_html_line_per_patch_line_and_escaped() {
        let e = entry(Lang::En, "AT+X<b>", "\r\n+X: 1\r\n\r\n<script>\r\nOK\r\n", 3312);
        assert!(!e.contains("<b>") && !e.contains("<script>"), "{e}");
        assert!(!e.contains('\n') && !e.contains('\r'), "{e:?}");
        assert!(e.contains("&#10;") && e.contains("3312 ms") && e.contains("&gt; AT+X"));
        assert!(entry(Lang::En, "AT", "\r\n", 5).contains("(no reply)"));
        let ev = format!("{:?}", patch_at("#at-log", "append", &e));
        assert!(ev.contains("selector #at-log") && ev.contains("mode append"), "{ev}");
    }

    #[test]
    fn history_buttons_and_the_clear_expression_survive_odd_text() {
        let h = hist_item(Lang::En, "AT+X=\"$1\" @y'");
        assert!(h.contains("data-cmd=\"AT+X=&quot;$1&quot; @y'\""), "{h}");
        // the expression never holds the raw text: `$` and `@` would be read by Datastar
        let j = js_str("AT$@\"x\\");
        assert!(!j.contains('$') && !j.contains('@'), "{j}");
        let back: String = serde_json::from_str(&j).unwrap();
        assert_eq!(back, "AT$@\"x\\");
    }

    #[test]
    fn the_port_line() {
        assert_eq!(port_of(&json!({"port":"/dev/at_mdm0","available":true})), PortState::Found("/dev/at_mdm0".into()));
        assert_eq!(port_of(&json!({"port":null,"available":false})), PortState::None);
        let h = port_html(Lang::En, &PortState::Found("/dev/at_mdm0".into()));
        assert!(h.contains("nd-status--ok") && h.contains("AT port available") && h.contains("/dev/at_mdm0"));
        assert!(port_html(Lang::En, &PortState::Looking).contains(" disabled"));
        assert!(port_html(Lang::En, &PortState::Failed("<x>".into())).contains("&lt;x&gt;"));
    }

    #[test]
    fn nothing_on_the_page_offers_cfun() {
        let l = Lang::En;
        let r = Rq { l, remote: Via::Local, who: Caller { token: None, peer: None, sid: None }, cmd: String::new() };
        let mut all = vec![
            port_html(l, &PortState::Looking),
            port_html(l, &PortState::None),
            err_html(Some("Command must start with AT")),
            area(&r, SET, &Phase::Idle),
            area(&r, SET, &Phase::Done(Outcome::Unknown)),
            entry(l, "AT", "OK", 1),
            hist_item(l, "AT"),
        ];
        let (why, rec) = danger("AT+CFUN=4", "AT+CFUN=4", l).unwrap();
        all.push(area(
            &r,
            "/x",
            &Phase::Ask(Confirm {
                tier: Tier::Dialog,
                title: "Send a risky AT command?".into(),
                what: why.into(),
                downtime: Some("d".into()),
                recovery: Some(rec.into()),
                action: "Send it".into(),
                cuts_uplink: true,
                word: None,
                danger: true,
                revert: crate::write::Revert::No,
            }),
        ));
        let html = all.join("\n");
        assert!(!cjk(&html), "{html}");
        // the shipped page (template) holds no example command but the harmless revision read
        let t = include_str!("../templates/at.html");
        assert!(!t.to_uppercase().contains("CFUN"), "no CFUN in the template");
        assert!(t.contains("placeholder=\"AT+CGMR\""));
    }
}
