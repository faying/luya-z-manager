//! /router/stc (STC). Behind the core this is the firmware's
//! whitelist cell lock (`nwinfo_stc_cell_lock_*`, `nwinfo_*_stc_white_list_*`): once on, the modem
//! only camps on cells it has collected into the whitelist.
//!
//! Where each value comes from (the old page read these once and again after a write; neither
//! poll nor push: the stream re-reads on load, on 「刷新」/「重试」 and after every write):
//! - STC on / off (the switch and 「当前状态」), the status block: `GET /api/cell/stc/status`
//!   (`enabled`: a bool, a non-zero number, or "1" / "true" / "enabled"; the real field name is
//!   unconfirmed, so the old page's reading is kept);
//! - the four parameters (LTE / NR-SA collect timer, LTE / NR-SA whitelist max): `GET
//!   /api/cell/stc/params`.
//!
//! Failure modes, as the old page had them: **503 "Method not found"** on a read (B27 has no
//! `nwinfo_get_stc_white_list_*`) = this firmware has no STC: the page says so and locks every
//! control, the reset too, and nothing is retried (a re-read only on 「刷新」); any other failed read
//! with nothing shown yet = the service is down: the switch and the parameters are locked, the reset
//! stays (it does not depend on anything shown); a failed re-read with something shown = greyed,
//! 「数字停在 …」, writes locked.
//!
//! The writes (agent paths; none of the three reads anything back that the page shows right away):
//! - 「STC」 switch, **tier 3** (dialog, `cuts_uplink`): `POST /api/cell/stc/enable|disable`, then
//!   `status.enabled` must equal the target, read every 3 s for up to 35 s (the modem re-attaches,
//!   about 30 s; reads that fail meanwhile are not the answer). Not seen in time → 「设备已接受，
//!   还没读回」;
//! - 「应用参数」, **tier 2** (inline): `PUT /api/cell/stc/params` with the four values as strings,
//!   the confirm names them; readback = each field equals what was sent (3 reads, 2 s apart). The
//!   fields are page-local drafts over the device values, kept through every push; they take the
//!   device's values again once applied;
//! - 「重置白名单」, **tier 3** (dialog, `danger`, `cuts_uplink`): `POST /api/cell/stc/reset`. The
//!   whitelist has no read endpoint, so the result is 「设备已接受」, never "已重置".
//!
//! One STC write at a time from any tab (a second press says so).

use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use askama::Template;
use axum::Router;
use axum::extract::{Query, Request, State};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::topbar::Via;
use crate::agent::{Caller, Fail, Polled, esc};
use crate::i18n::Lang;
use crate::pages::{Shared, StreamQuery, patch, split_leaves};
use crate::write::{self, Confirm, Outcome, Phase, Sent, Tier};

const PATH: &str = "/router/stc";
/// The old page did not poll.
const IDLE: Duration = Duration::from_secs(24 * 3600);
const RETRY: Duration = Duration::from_secs(10);
/// The modem re-attaches for about 30 s after the switch.
const SWITCH_WAIT: Duration = Duration::from_secs(35);
static WRITING: AtomicBool = AtomicBool::new(false);

struct Free;
impl Drop for Free {
    fn drop(&mut self) {
        WRITING.store(false, Ordering::SeqCst);
    }
}

pub fn routes() -> Router<Shared> {
    Router::new()
        .route(PATH, get(page))
        .route("/router/stc/events", get(events))
        .route("/router/stc/refresh", post(refresh))
        .route("/router/stc/toggle/ask", post(toggle_ask))
        .route("/router/stc/toggle/set", post(toggle_set))
        .route("/router/stc/toggle/cancel", post(toggle_cancel))
        .route("/router/stc/params/ask", post(params_ask))
        .route("/router/stc/params/set", post(params_set))
        .route("/router/stc/params/cancel", post(params_cancel))
        .route("/router/stc/reset/ask", post(reset_ask))
        .route("/router/stc/reset/set", post(reset_set))
        .route("/router/stc/reset/cancel", post(reset_cancel))
}

// ---- what the agent says ----

/// The old `isOn`: true / non-zero number / "1" / "true" / "enabled" / non-zero numeric string.
pub fn is_on(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => Some(n.as_f64().is_some_and(|f| f != 0.0)),
        Value::String(s) => {
            let s = s.trim().to_lowercase();
            Some(if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
                s.parse::<u64>().is_ok_and(|n| n != 0)
            } else {
                s == "true" || s == "enabled"
            })
        }
        // the field is missing: the old page counted that as off
        _ => Some(false),
    }
}

/// The four parameters as the agent's strings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Params {
    pub lte_timer: String,
    pub nrsa_timer: String,
    pub lte_max: String,
    pub nrsa_max: String,
}

fn txt(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_owned(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

pub fn parse_params(v: &Value) -> Params {
    Params {
        lte_timer: txt(&v["lte_collect_timer"]),
        nrsa_timer: txt(&v["nrsa_collect_timer"]),
        lte_max: txt(&v["lte_whitelist_max"]),
        nrsa_max: txt(&v["nrsa_whitelist_max"]),
    }
}

/// The firmware has no such ubus method (503 "… (Method not found)"): retrying never helps.
pub fn unsupported(f: &Fail) -> bool {
    matches!(f, Fail::Refused(503, m, _) if m.to_lowercase().contains("method not found"))
}

/// A drafted value is kept as digits only (what goes into the page's signal).
fn digits(s: &str) -> bool {
    !s.is_empty() && s.len() <= 9 && s.bytes().all(|b| b.is_ascii_digit())
}

// ---- the leaves ----

struct Poll {
    who: Caller,
    l: Lang,
    status: Option<bool>,
    status_fail: Option<Fail>,
    params: Option<Params>,
    params_fail: Option<Fail>,
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

const SWITCH_TRACK: &str = "<span class=\"nd-switch__track\"><svg class=\"nd-switch__check\" viewBox=\"0 0 12 12\" aria-hidden=\"true\"><path d=\"M2.5 6.2 5 8.6 9.5 3.6\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.8\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/></svg><span class=\"nd-switch__thumb\"></span></span>";

impl Poll {
    fn new(who: Caller, l: Lang) -> Poll {
        Poll {
            who,
            l,
            status: None,
            status_fail: None,
            params: None,
            params_fail: None,
            at: None,
        }
    }

    async fn fetch(&mut self, app: &Shared) {
        match crate::agent::get(app, &self.who, "/api/cell/stc/status").await {
            Ok(v) => {
                self.status = is_on(&v["enabled"]);
                self.status_fail = None;
                self.at = Some(SystemTime::now());
            }
            Err(f) => self.status_fail = Some(f),
        }
        match crate::agent::get(app, &self.who, "/api/cell/stc/params").await {
            Ok(v) => {
                self.params = Some(parse_params(&v));
                self.params_fail = None;
                self.at = Some(SystemTime::now());
            }
            Err(f) => self.params_fail = Some(f),
        }
    }

    fn unsupported(&self) -> bool {
        (self.status.is_none() && self.status_fail.as_ref().is_some_and(unsupported))
            || (self.params.is_none() && self.params_fail.as_ref().is_some_and(unsupported))
    }

    fn down(&self) -> bool {
        !self.unsupported()
            && ((self.status.is_none() && self.status_fail.is_some())
                || (self.params.is_none() && self.params_fail.is_some()))
    }

    fn stale(&self) -> bool {
        (self.status.is_some() && self.status_fail.is_some())
            || (self.params.is_some() && self.params_fail.is_some())
    }

    fn leaves(&self) -> Vec<(String, String)> {
        let l = self.l;
        let (unsup, down, stale) = (self.unsupported(), self.down(), self.stale());
        let st_cls = if stale { " nd-stale" } else { "" };
        let mut out = String::new();

        // status block
        let (mut tone, state, reason): (&str, String, Option<String>) = if unsup {
            (
                "neutral",
                l.pick("Not available on this firmware", "这个固件没有此功能").into(),
                Some(
                    l.pick(
                        "The device's firmware doesn't answer the STC whitelist requests (the methods aren't there), so STC can't be read or changed from this page. The controls below are locked.",
                        "设备固件不回应 STC 白名单请求（没有这些方法），所以不能在这个页面读取或修改 STC。下面的控件已锁定。",
                    )
                    .into(),
                ),
            )
        } else if down {
            (
                "warn",
                l.pick("STC service unavailable", "STC 服务不可用").into(),
                Some(
                    l.pick(
                        "STC can't be reached right now (503), so the controls are off.",
                        "现在连不上 STC（503），所以控件先关掉。",
                    )
                    .into(),
                ),
            )
        } else if let Some(on) = self.status {
            (
                if on { "ok" } else { "neutral" },
                if on {
                    l.pick("Enabled", "已启用").into()
                } else {
                    l.pick("Disabled", "已禁用").into()
                },
                Some(
                    if on {
                        l.pick(
                            "The modem only uses cells in the collected whitelist.",
                            "调制解调器只使用已收集进白名单的小区。",
                        )
                    } else {
                        l.pick("The modem picks cells normally.", "调制解调器正常选择小区。")
                    }
                    .into(),
                ),
            )
        } else {
            ("neutral", l.pick("Reading STC…", "正在读取 STC…").into(), None)
        };
        let mut meta = String::new();
        if stale && !down {
            tone = "stale";
            meta = format!(
                "<div class=\"nd-status__meta\"><span class=\"nd-fresh\">{}</span></div>",
                esc(&format!(
                    "{} · {}",
                    crate::agent::stopped(l, "Settings", "设置", self.at),
                    l.pick("Refresh before changing anything.", "改任何东西之前先刷新。")
                ))
            );
        }
        let retry = if !unsup && (down || stale) {
            format!(
                "<button type=\"button\" class=\"nd-btn nd-btn--secondary nd-btn--sm\" data-on:click=\"@post('{PATH}/refresh')\">{}</button>",
                l.pick("Retry", "重试")
            )
        } else {
            String::new()
        };
        out.push_str(&format!(
            "<div id=\"stc-status\"><section class=\"nd-status nd-status--{tone}{}\"><div class=\"nd-status__main\"><div class=\"nd-status__state\" role=\"status\">{}</div>{}{meta}</div><div class=\"nd-status__actions\">{retry}</div></section></div>\n<!--leaf-->\n",
            if tone == "stale" { " nd-stale" } else { "" },
            mark(tone, &state),
            reason.map_or(String::new(), |r| format!("<div class=\"nd-status__reason\"><span>{}</span></div>", esc(&r))),
        ));

        // the switch
        let locked = unsup || down || stale || self.status.is_none();
        let label = l.pick("STC (whitelist cell lock)", "STC（白名单锁小区）");
        let sub = if unsup {
            Some(l.pick("Not available on this firmware", "这个固件没有此功能").to_owned())
        } else {
            self.status.map(|on| {
                format!(
                    "{}: {}",
                    l.pick("Current State", "当前状态"),
                    if on { l.pick("Enabled", "已启用") } else { l.pick("Disabled", "已禁用") }
                )
            })
        };
        let control = match self.status {
            _ if unsup => "<span class=\"nd-row__value\">—</span>".to_owned(),
            None if self.status_fail.is_some() => String::new(),
            None => "<span class=\"nd-skel\" style=\"width:6ch\"></span>".to_owned(),
            Some(on) => format!(
                "<span class=\"t-sw\"><button type=\"button\" role=\"switch\" class=\"nd-switch t-switch\" aria-label=\"STC\" aria-checked=\"{on}\"{}{}{}>{SWITCH_TRACK}</button></span>",
                if locked { " disabled data-disabled=\"true\"" } else { "" },
                if on { " data-selected=\"true\"" } else { "" },
                if locked {
                    String::new()
                } else {
                    format!(" data-on:click=\"@post('{PATH}/toggle/ask?on={}')\"", u8::from(!on))
                },
            ),
        };
        out.push_str(&format!(
            "<div id=\"stc-switch\" class=\"nd-group{st_cls}\"><div class=\"nd-row nd-row--two\"><span class=\"nd-row__text\"><span class=\"nd-row__label\">{}</span>{}</span>{control}</div></div>\n<!--leaf-->\n",
            esc(label),
            sub.map_or(String::new(), |s| format!("<span class=\"nd-row__sub block\">{}</span>", esc(&s))),
        ));

        // the parameters
        out.push_str(&self.params_leaf(unsup, down, stale));

        // the reset button
        out.push_str(&format!(
            "<div id=\"stc-resetbtn\"><button type=\"button\" class=\"nd-btn nd-btn--secondary\" data-write=\"stc-rs\"{} data-on:click=\"@post('{PATH}/reset/ask')\">{}</button></div>\n<!--leaf-->\n",
            if unsup { " disabled" } else { "" },
            l.pick("Reset Whitelist", "重置白名单"),
        ));
        split_leaves(&out)
    }

    fn params_leaf(&self, unsup: bool, down: bool, stale: bool) -> String {
        let l = self.l;
        let st_cls = if self.params.is_some() && self.params_fail.is_some() { " nd-stale" } else { "" };
        let locked = unsup || down || stale || self.params.is_none();
        let p = self.params.clone().unwrap_or_default();
        let shown = |s: &str| if digits(s) { s.to_owned() } else { String::new() };
        let signals = if self.params.is_some() {
            format!(
                " data-signals__ifmissing=\"{{stlt: '{}', stnt: '{}', stlm: '{}', stnm: '{}'}}\"",
                shown(&p.lte_timer),
                shown(&p.nrsa_timer),
                shown(&p.lte_max),
                shown(&p.nrsa_max)
            )
        } else {
            String::new()
        };
        let sec = l.pick("seconds", "秒");
        let cnt = l.pick("count", "个");
        let fields = [
            ("stlt", l.pick("LTE Collect Timer", "LTE 采集定时器"), sec),
            ("stnt", l.pick("NR-SA Collect Timer", "NR-SA 采集定时器"), sec),
            ("stlm", l.pick("LTE Whitelist Max", "LTE 白名单上限"), cnt),
            ("stnm", l.pick("NR-SA Whitelist Max", "NR-SA 白名单上限"), cnt),
        ];
        let mut f = String::new();
        for (sig, label, unit) in fields {
            let input = if self.params.is_some() {
                format!(
                    "<input id=\"stc-f-{sig}\" class=\"nd-field nd-mono\" inputmode=\"numeric\" pattern=\"[0-9]*\" autocomplete=\"off\" maxlength=\"9\" aria-describedby=\"stc-f-{sig}-u\" data-bind:{sig} data-on:input=\"${sig} = ${sig}.replace(/\\D/g, '')\"{}>",
                    if locked { " disabled" } else { "" }
                )
            } else if unsup {
                format!("<input id=\"stc-f-{sig}\" class=\"nd-field nd-mono\" value=\"\" placeholder=\"—\" aria-describedby=\"stc-f-{sig}-u\" disabled readonly>")
            } else {
                format!("<span id=\"stc-f-{sig}\" class=\"nd-field stc-skel\" aria-describedby=\"stc-f-{sig}-u\"><span class=\"nd-skel\" style=\"width:6ch\"></span></span>")
            };
            f.push_str(&format!(
                "<div class=\"stc-field\"><label class=\"stc-label\" for=\"stc-f-{sig}\">{}</label>{input}<span id=\"stc-f-{sig}-u\" class=\"nd-aux\">{}</span></div>",
                esc(label),
                esc(unit)
            ));
        }
        format!(
            "<div id=\"stc-params\" class=\"nd-group stc-params{st_cls}\"{signals}><div class=\"stc-grid\">{f}</div><div><button type=\"button\" class=\"nd-btn nd-btn--primary\" data-write=\"stc-pa\"{} data-on:click=\"@post('{PATH}/params/ask')\">{}</button></div></div>\n<!--leaf-->\n",
            if locked { " disabled" } else { "" },
            l.pick("Apply Parameters", "应用参数"),
        )
    }
}

impl Polled for Poll {
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        self.fetch(app).await;
        let next = if self.status.is_none() && self.params.is_none() && !self.unsupported() {
            RETRY
        } else {
            IDLE
        };
        (self.leaves(), next)
    }
}

// ---- the page ----

#[derive(Template)]
#[template(path = "stc.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    leaves: Vec<(String, String)>,
    tg_op: String,
    pa_op: String,
    rs_op: String,
}

impl Page {
    fn leaf(&self, id: &str) -> &str {
        self.leaves.iter().find(|(k, _)| k == id).map_or("", |(_, h)| h.as_str())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Which {
    Toggle,
    Params,
    Reset,
}

impl Which {
    fn id(self) -> &'static str {
        match self {
            Which::Toggle => "stc-tg",
            Which::Params => "stc-pa",
            Which::Reset => "stc-rs",
        }
    }
    fn dir(self) -> &'static str {
        match self {
            Which::Toggle => "toggle",
            Which::Params => "params",
            Which::Reset => "reset",
        }
    }
}

/// `set` is the confirm button's target ("" when there is nothing to confirm).
fn area(w: Which, l: Lang, remote: Via, set: &str, phase: &Phase) -> String {
    let cancel = format!("{PATH}/{}/cancel", w.dir());
    write::Area {
        id: w.id(),
        l,
        remote,
        set,
        cancel: &cancel,
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
        crate::shell::Shell::new(&headers, PATH).live(st, &ctx, Some(format!("{PATH}/events?g={}", st.generation)))
    });
    let idle = Phase::Idle;
    let html = Page {
        shell,
        l: ctx.lang,
        leaves: p.leaves(),
        tg_op: area(Which::Toggle, ctx.lang, ctx.remote, "", &idle),
        pa_op: area(Which::Params, ctx.lang, ctx.remote, "", &idle),
        rs_op: area(Which::Reset, ctx.lang, ctx.remote, "", &idle),
    }
    .render()
    .unwrap();
    crate::assets::maybe_gzip(&headers, "text/html; charset=utf-8", html.into_bytes(), "no-store")
}

async fn events(State(app): State<Shared>, Query(q): Query<StreamQuery>, req: Request) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let p = Poll::new(Caller::of(&app, &req), ctx.lang);
    crate::agent::stream(app, ctx, q.g, PATH, p)
}

async fn refresh(State(app): State<Shared>) -> axum::http::StatusCode {
    let _ = app.kick.send(PATH);
    axum::http::StatusCode::NO_CONTENT
}

fn one(html: String) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(futures_util::stream::once(async move { Ok(patch(&html)) }))
}

fn busy(l: Lang) -> Outcome {
    Outcome::NotYet(
        l.pick(
            "Another STC change is in progress; wait for it to finish",
            "另一项 STC 改动正在进行，等它结束",
        )
        .into(),
    )
}

fn cant_read(f: &Fail, l: Lang) -> Outcome {
    Outcome::NotYet(match l {
        Lang::En => format!("Can't read STC now, so nothing was sent: {}", f.text(l)),
        Lang::Zh => format!("现在读不到 STC，所以什么都没发送：{}", f.text(l)),
    })
}

fn unsupported_note(l: Lang) -> Outcome {
    Outcome::NotYet(
        l.pick(
            "Not available on this firmware; nothing was sent",
            "这个固件没有此功能，什么都没发送",
        )
        .into(),
    )
}

fn cancel_of(w: Which, app: &Shared, q: &write::CancelQuery, req: &Request) -> String {
    let ctx = crate::topbar::Ctx::of_request(req, PATH);
    if q.lost {
        let _ = app.kick.send(PATH);
    }
    area(w, ctx.lang, ctx.remote, "", &write::cancel_phase(q))
}

async fn toggle_cancel(State(app): State<Shared>, Query(q): Query<write::CancelQuery>, req: Request) -> impl IntoResponse {
    one(cancel_of(Which::Toggle, &app, &q, &req))
}
async fn params_cancel(State(app): State<Shared>, Query(q): Query<write::CancelQuery>, req: Request) -> impl IntoResponse {
    one(cancel_of(Which::Params, &app, &q, &req))
}
async fn reset_cancel(State(app): State<Shared>, Query(q): Query<write::CancelQuery>, req: Request) -> impl IntoResponse {
    one(cancel_of(Which::Reset, &app, &q, &req))
}

// ---- the switch ----

#[derive(Deserialize, Default)]
struct On {
    #[serde(default)]
    on: u8,
}

const RECOVERY: (&str, &str) = (
    "Turn STC off on this page. If the page can't be reached, connect to the U60's Wi-Fi and open it from there.",
    "在这个页面把 STC 关掉。如果页面打不开，连上设备的 Wi-Fi 再从那里打开。",
);

fn downtime(l: Lang) -> &'static str {
    l.pick(
        "The mobile connection may drop for about 30 seconds while the modem re-attaches.",
        "调制解调器重新附着，移动网络可能断开约 30 秒。",
    )
}

pub fn toggle_confirm(want: bool, l: Lang) -> Confirm {
    Confirm {
        tier: Tier::Dialog,
        title: if want {
            l.pick("Turn STC on?", "开启 STC？")
        } else {
            l.pick("Turn STC off?", "关闭 STC？")
        }
        .into(),
        what: if want {
            l.pick(
                "The modem only uses cells in the collected whitelist. If none of them is in reach, there is no mobile service until STC is off again.",
                "调制解调器只使用已收集进白名单的小区。如果一个都连不上，在 STC 关掉之前没有移动网络。",
            )
        } else {
            l.pick(
                "The modem stops limiting itself to the whitelist and picks cells normally.",
                "调制解调器不再只限于白名单，恢复正常选择小区。",
            )
        }
        .into(),
        downtime: Some(downtime(l).into()),
        recovery: Some(l.pick(RECOVERY.0, RECOVERY.1).into()),
        action: if want {
            l.pick("Turn STC on", "开启 STC")
        } else {
            l.pick("Turn STC off", "关闭 STC")
        }
        .into(),
        cuts_uplink: true,
        word: None,
        danger: false,
        revert: crate::write::Revert::No,
    }
}

async fn read_status(app: &Shared, who: &Caller) -> Result<Option<bool>, Fail> {
    crate::agent::get(app, who, "/api/cell/stc/status").await.map(|v| is_on(&v["enabled"]))
}

async fn toggle_ask(State(app): State<Shared>, Query(q): Query<On>, req: Request) -> impl IntoResponse {
    let who = Caller::of(&app, &req);
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let (l, r) = (ctx.lang, ctx.remote);
    let want = q.on == 1;
    let w = Which::Toggle;
    let fail = |o: Outcome| one(area(w, l, r, "", &Phase::Done(o)));
    if WRITING.load(Ordering::SeqCst) {
        return fail(busy(l));
    }
    match read_status(&app, &who).await {
        Err(f) if unsupported(&f) => fail(unsupported_note(l)),
        Err(f) => fail(cant_read(&f, l)),
        Ok(Some(now)) if now == want => {
            let _ = app.kick.send(PATH);
            fail(Outcome::NotYet(
                if want {
                    l.pick("STC is already on; the page is read again", "STC 已经是开着的，页面正在重新读取")
                } else {
                    l.pick("STC is already off; the page is read again", "STC 已经是关着的，页面正在重新读取")
                }
                .into(),
            ))
        }
        Ok(_) => one(area(
            w,
            l,
            r,
            &format!("{PATH}/toggle/set?on={}", u8::from(want)),
            &Phase::Ask(toggle_confirm(want, l)),
        )),
    }
}

async fn toggle_set(State(app): State<Shared>, Query(q): Query<On>, req: Request) -> impl IntoResponse {
    let who = Caller::of(&app, &req);
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let (l, r) = (ctx.lang, ctx.remote);
    let want = q.on == 1;
    let w = Which::Toggle;
    let pending = area(
        w,
        l,
        r,
        "",
        &Phase::Pending(
            l.pick(
                "Switching… the mobile connection may drop for about 30 seconds",
                "正在切换…移动网络可能断开约 30 秒",
            )
            .into(),
        ),
    );
    write::respond(pending, async move {
        let done = |o: Outcome| area(w, l, r, "", &Phase::Done(o));
        if WRITING.swap(true, Ordering::SeqCst) {
            return done(busy(l));
        }
        let _free = Free;
        let o = run_toggle(&app, &who, l, want).await;
        let _ = app.kick.send(PATH);
        done(o)
    })
}

async fn run_toggle(app: &Shared, who: &Caller, l: Lang, want: bool) -> Outcome {
    match read_status(app, who).await {
        Err(f) if unsupported(&f) => return unsupported_note(l),
        Err(f) => return cant_read(&f, l),
        Ok(_) => {}
    }
    let path = if want { "/api/cell/stc/enable" } else { "/api/cell/stc/disable" };
    let how = match crate::agent::post(app, who, path, &json!({})).await {
        Ok(_) => Sent::Ok,
        Err(Fail::Unreachable) => Sent::NoReply,
        Err(f) => return Outcome::Failed(f.text(l)),
    };
    let deadline = Instant::now() + SWITCH_WAIT;
    loop {
        // reads that fail while the modem re-attaches are not an answer: keep asking
        if let Ok(Some(now)) = read_status(app, who).await
            && now == want
        {
            return Outcome::Applied(
                if want {
                    l.pick("STC is on", "STC 已开启")
                } else {
                    l.pick("STC is off", "STC 已关闭")
                }
                .into(),
            );
        }
        if Instant::now() >= deadline {
            return how.not_read();
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

// ---- the parameters ----

/// The four drafts as Datastar posts them (signals `stlt` …).
#[derive(Deserialize, Default)]
struct Draft {
    #[serde(default)]
    stlt: String,
    #[serde(default)]
    stnt: String,
    #[serde(default)]
    stlm: String,
    #[serde(default)]
    stnm: String,
}

/// The values a confirm names and a send carries (URL of the confirm: not secret).
#[derive(Deserialize, Default, Clone, Debug, PartialEq)]
pub struct Four {
    #[serde(default)]
    lt: String,
    #[serde(default)]
    nt: String,
    #[serde(default)]
    lm: String,
    #[serde(default)]
    nm: String,
}

/// Checked values: whole numbers, none empty. Err = the sentence.
fn check_four(f: &Four, l: Lang) -> Result<Four, String> {
    let named = [
        (&f.lt, l.pick("LTE Collect Timer", "LTE 采集定时器")),
        (&f.nt, l.pick("NR-SA Collect Timer", "NR-SA 采集定时器")),
        (&f.lm, l.pick("LTE Whitelist Max", "LTE 白名单上限")),
        (&f.nm, l.pick("NR-SA Whitelist Max", "NR-SA 白名单上限")),
    ];
    for (v, name) in named {
        if !digits(v.trim()) {
            return Err(match l {
                Lang::En => format!("{name} must be a whole number"),
                Lang::Zh => format!("{name}要填整数"),
            });
        }
    }
    Ok(Four {
        lt: f.lt.trim().parse::<u64>().map(|n| n.to_string()).unwrap_or_default(),
        nt: f.nt.trim().parse::<u64>().map(|n| n.to_string()).unwrap_or_default(),
        lm: f.lm.trim().parse::<u64>().map(|n| n.to_string()).unwrap_or_default(),
        nm: f.nm.trim().parse::<u64>().map(|n| n.to_string()).unwrap_or_default(),
    })
}

fn same(p: &Params, f: &Four) -> bool {
    p.lte_timer == f.lt && p.nrsa_timer == f.nt && p.lte_max == f.lm && p.nrsa_max == f.nm
}

pub fn params_confirm(f: &Four, l: Lang) -> Confirm {
    Confirm {
        tier: Tier::Inline,
        title: l.pick("Apply Parameters", "应用参数").into(),
        what: match l {
            Lang::En => format!(
                "Whitelist collection uses the new timers and limits: LTE {} s / {} cells, NR-SA {} s / {} cells.",
                f.lt, f.lm, f.nt, f.nm
            ),
            Lang::Zh => format!(
                "白名单采集改用新的定时器和上限：LTE {} 秒 / {} 个小区，NR-SA {} 秒 / {} 个小区。",
                f.lt, f.lm, f.nt, f.nm
            ),
        },
        downtime: None,
        recovery: None,
        action: l.pick("Apply Parameters", "应用参数").into(),
        cuts_uplink: false,
        word: None,
        danger: false,
        revert: crate::write::Revert::No,
    }
}

fn set_url(f: &Four) -> String {
    format!("{PATH}/params/set?lt={}&nt={}&lm={}&nm={}", f.lt, f.nt, f.lm, f.nm)
}

async fn params_ask(State(app): State<Shared>, req: Request) -> impl IntoResponse {
    let who = Caller::of(&app, &req);
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let (l, r) = (ctx.lang, ctx.remote);
    let body = axum::body::to_bytes(req.into_body(), 4096).await.unwrap_or_default();
    let d: Draft = serde_json::from_slice(&body).unwrap_or_default();
    let w = Which::Params;
    let fail = |o: Outcome| one(area(w, l, r, "", &Phase::Done(o)));
    let four = match check_four(&Four { lt: d.stlt, nt: d.stnt, lm: d.stlm, nm: d.stnm }, l) {
        Ok(f) => f,
        Err(e) => return fail(Outcome::NotYet(e)),
    };
    if WRITING.load(Ordering::SeqCst) {
        return fail(busy(l));
    }
    match crate::agent::get(&app, &who, "/api/cell/stc/params").await {
        Err(f) if unsupported(&f) => fail(unsupported_note(l)),
        Err(f) => fail(cant_read(&f, l)),
        Ok(v) if same(&parse_params(&v), &four) => fail(Outcome::NotYet(
            l.pick("These are the values the device already has", "设备上已经是这些值").into(),
        )),
        Ok(_) => one(area(w, l, r, &set_url(&four), &Phase::Ask(params_confirm(&four, l)))),
    }
}

async fn params_set(State(app): State<Shared>, Query(q): Query<Four>, req: Request) -> impl IntoResponse {
    let who = Caller::of(&app, &req);
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let (l, r) = (ctx.lang, ctx.remote);
    let w = Which::Params;
    let pending = area(w, l, r, "", &Phase::Pending(l.pick("Applying…", "正在应用…").into()));
    write::respond(pending, async move {
        let done = |o: Outcome| area(w, l, r, "", &Phase::Done(o));
        let four = match check_four(&q, l) {
            Ok(f) => f,
            Err(e) => return done(Outcome::NotYet(e)),
        };
        if WRITING.swap(true, Ordering::SeqCst) {
            return done(busy(l));
        }
        let _free = Free;
        let o = run_params(&app, &who, l, &four).await;
        let _ = app.kick.send(PATH);
        let mut html = done(o.clone());
        if matches!(o, Outcome::Applied(_)) {
            // the drafts take the device's values again
            html.push_str(&format!(
                "\n<span id=\"stc-reset\" hidden data-init=\"$stlt = '{}'; $stnt = '{}'; $stlm = '{}'; $stnm = '{}'; $_x = '{}'\"></span>",
                four.lt,
                four.nt,
                four.lm,
                four.nm,
                write::next_op_id()
            ));
        }
        html
    })
}

async fn run_params(app: &Shared, who: &Caller, l: Lang, f: &Four) -> Outcome {
    match crate::agent::get(app, who, "/api/cell/stc/params").await {
        Err(e) if unsupported(&e) => return unsupported_note(l),
        Err(e) => return cant_read(&e, l),
        Ok(_) => {}
    }
    let body = json!({
        "lte_collect_timer": f.lt,
        "nrsa_collect_timer": f.nt,
        "lte_whitelist_max": f.lm,
        "nrsa_whitelist_max": f.nm,
    });
    let how = match crate::agent::send_json(app, who, reqwest::Method::PUT, "/api/cell/stc/params", &body).await {
        Ok(_) => Sent::Ok,
        Err(Fail::Unreachable) => Sent::NoReply,
        Err(e) => return Outcome::Failed(e.text(l)),
    };
    for n in 0..3 {
        if n > 0 {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        if let Ok(v) = crate::agent::get(app, who, "/api/cell/stc/params").await
            && same(&parse_params(&v), f)
        {
            return Outcome::Applied(l.pick("Parameters applied", "参数已应用").into());
        }
    }
    how.not_read()
}

// ---- the reset ----

pub fn reset_confirm(l: Lang) -> Confirm {
    Confirm {
        tier: Tier::Dialog,
        title: l.pick("Reset the STC whitelist?", "重置 STC 白名单？").into(),
        what: l
            .pick(
                "Clear the STC whitelist? The collected cells are gone for good.",
                "清空 STC 白名单？收集到的小区会全部丢掉，找不回来。",
            )
            .into(),
        downtime: Some(downtime(l).into()),
        recovery: Some(
            l.pick(
                "The whitelist can't be restored; the modem collects a new one over time. Turn STC off if service is poor meanwhile.",
                "白名单恢复不了；调制解调器会随时间重新收集。期间信号差的话，把 STC 关掉。",
            )
            .into(),
        ),
        action: l.pick("Reset Whitelist", "重置白名单").into(),
        cuts_uplink: true,
        word: None,
        danger: true,
        revert: crate::write::Revert::No,
    }
}

async fn reset_ask(State(app): State<Shared>, req: Request) -> impl IntoResponse {
    let _ = &app;
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let (l, r) = (ctx.lang, ctx.remote);
    let w = Which::Reset;
    if WRITING.load(Ordering::SeqCst) {
        return one(area(w, l, r, "", &Phase::Done(busy(l))));
    }
    one(area(w, l, r, &format!("{PATH}/reset/set"), &Phase::Ask(reset_confirm(l))))
}

async fn reset_set(State(app): State<Shared>, req: Request) -> impl IntoResponse {
    let who = Caller::of(&app, &req);
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let (l, r) = (ctx.lang, ctx.remote);
    let w = Which::Reset;
    let pending = area(w, l, r, "", &Phase::Pending(l.pick("Resetting…", "正在重置…").into()));
    write::respond(pending, async move {
        let done = |o: Outcome| area(w, l, r, "", &Phase::Done(o));
        if WRITING.swap(true, Ordering::SeqCst) {
            return done(busy(l));
        }
        let _free = Free;
        let o = match crate::agent::post(&app, &who, "/api/cell/stc/reset", &json!({})).await {
            Ok(_) => Outcome::AcceptedSaid(
                l.pick(
                    "Accepted by the device: whitelist reset (the whitelist can't be read back)",
                    "设备已接受：重置白名单（白名单读不回来）",
                )
                .into(),
            ),
            Err(Fail::Unreachable) => Outcome::Unknown,
            Err(f) => Outcome::Failed(f.text(l)),
        };
        let _ = app.kick.send(PATH);
        done(o)
    })
}

#[cfg(test)]
mod tests;
