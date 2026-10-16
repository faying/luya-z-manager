//! /router/vpn (VPN 穿透), the old `router/vpn/page.tsx`: which passthroughs are on, the note on
//! what passthrough is, the L2TP / PPTP / IPSec switches, SIP ALG read-only.
//!
//! Where each value comes from (the old page read the agent's `/api/router/vpn`, i.e.
//! `zwrt_router.api router_get_alg_para` passed through): u60d's `router.alg` block part, the same
//! vendor read (`l2tp_passthrough`, `pptp_passthrough`, `ipsec_passthrough`, `alg_sip_enable` →
//! `l2tp`, `pptp`, `ipsec`, `sip`). A null field = the firmware didn't report it ("—", not "off");
//! `alg` null = the read failed. B31 (like B27) reports only SIP ALG.
//!
//! Writes (web/docs/controls-inventory.md §/router/vpn): each switch is one `vendor.call`
//! `zwrt_router.api router_set_alg_switch {<field>_passthrough: "1"|"0"}` (the args the agent's
//! `router_vpn_set` passed through), tier 2 locally, tier 3 over Tailscale. Read back from
//! `router.alg.<field>` (u60d drops its cache after a write) when the device reports that field;
//! a field it doesn't report (B31: all three) shows 「状态不明」 with two buttons 「打开」「关掉」
//! (user, 10-13) and can't be read back: 「设备已接受，读不回来确认」 at once, never success. SIP ALG
//! stays read-only like the old page (whether the firmware honours a SIP change is unknown).

use std::convert::Infallible;
use std::time::Duration;

use askama::Template;
use axum::Router;
use axum::extract::{Query, State};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::Polled;
use crate::hub::{Link, St};
use crate::i18n::Lang;
use crate::pages::{Shared, StreamQuery, split_leaves};
use crate::write;

const PATH: &str = "/router/vpn";

/// This page's routes (the page, its stream, its buttons).
pub fn routes() -> Router<Shared> {
    Router::new()
        .route(PATH, get(page))
        .route("/router/vpn/events", get(events))
        .route("/router/vpn/refresh", post(refresh))
        .route("/router/vpn/ask", post(ask))
        .route("/router/vpn/set", post(set))
        .route("/router/vpn/cancel", post(cancel))
}

// ---- what u60d says ----

/// The three switches: (key in `router.alg`, the setter's arg, the name).
pub const FIELDS: [(&str, &str, &str); 3] = [
    ("l2tp", "l2tp_passthrough", "L2TP"),
    ("pptp", "pptp_passthrough", "PPTP"),
    ("ipsec", "ipsec_passthrough", "IPSec"),
];

fn field(k: &str) -> Option<(&'static str, &'static str, &'static str)> {
    FIELDS.iter().find(|f| f.0 == k).copied()
}

/// `router.alg` and whether it is current. None: no router block yet, or the read failed.
pub fn alg(st: &St) -> Option<(&Value, bool)> {
    let b = st.blocks.get("router")?;
    let a = &b.data["alg"];
    a.is_object()
        .then_some((a, b.stale || st.link != Link::Live))
}

/// One flag of `router.alg`: None = not reported.
pub fn flag(st: &St, key: &str) -> Option<bool> {
    alg(st).and_then(|(a, _)| a[key].as_bool())
}

// ---- the view ----

pub struct Sw {
    pub key: &'static str,
    /// "L2TP" etc.
    pub name: &'static str,
    pub label: String,
    pub sub: String,
    pub on: Option<bool>,
    /// "正在打开…" while this tab's send is in flight.
    pub busy: Option<String>,
}

pub struct View {
    pub l: Lang,
    pub tone: &'static str,
    pub state: String,
    pub reason: Option<String>,
    pub meta: Option<String>,
    pub retry: bool,
    /// None: nothing read yet (skeleton rows).
    pub rows: Option<Vec<Sw>>,
    pub sip: Option<bool>,
    pub dis: bool,
    pub stale: bool,
}

impl View {
    pub fn symbol(&self) -> &'static str {
        match self.tone {
            "bad" => "■",
            "warn" => "▲",
            _ => "●",
        }
    }
    /// The aria-label of an unreported field's button ("打开 L2TP 穿透").
    /// Starts with the visible word (WCAG 2.5.3).
    pub fn act(&self, name: &str, on: bool) -> String {
        match (self.l, on) {
            (Lang::En, true) => format!("Turn on {name} passthrough"),
            (Lang::En, false) => format!("Turn off {name} passthrough"),
            (Lang::Zh, true) => format!("打开 {name} 穿透"),
            (Lang::Zh, false) => format!("关掉 {name} 穿透"),
        }
    }
    pub fn on_off(&self, v: bool) -> &'static str {
        if v {
            self.l.pick("On", "开")
        } else {
            self.l.pick("Off", "关")
        }
    }
}

fn desc(key: &str, l: Lang) -> &'static str {
    match key {
        "l2tp" => l.pick("Layer 2 Tunneling Protocol", "第二层隧道协议"),
        "pptp" => l.pick("Point-to-Point Tunneling Protocol", "点对点隧道协议"),
        _ => l.pick("Internet Protocol Security", "互联网安全协议"),
    }
}

fn turning(on: bool, l: Lang) -> String {
    if on {
        l.pick("Turning on…", "正在打开…")
    } else {
        l.pick("Turning off…", "正在关掉…")
    }
    .into()
}

/// `busy`: this tab's send in flight (field key, target).
pub fn view(st: &St, busy: Option<(&str, bool)>, l: Lang) -> View {
    let a = alg(st);
    let block = st.blocks.get("router");
    let stale = a.is_some_and(|(_, s)| s);
    let rows = a.map(|(v, _)| {
        FIELDS
            .iter()
            .map(|(k, _, name)| {
                let on = v[*k].as_bool();
                let d = desc(k, l);
                Sw {
                    key: k,
                    name,
                    label: match l {
                        Lang::En => format!("{name} Passthrough"),
                        Lang::Zh => format!("{name} 穿透"),
                    },
                    sub: if on.is_none() {
                        match l {
                            Lang::En => format!("{d} · state unknown: the device doesn't report it, a change can't be confirmed"),
                            Lang::Zh => format!("{d} · 设备没有报告，改了也读不回来确认"),
                        }
                    } else {
                        d.into()
                    },
                    on,
                    busy: busy.filter(|(b, _)| b == k).map(|(_, t)| turning(t, l)),
                }
            })
            .collect::<Vec<_>>()
    });
    let sip = a.and_then(|(v, _)| v["sip"].as_bool());
    let onoff = |v: bool| -> &'static str {
        if v {
            l.pick("On", "开")
        } else {
            l.pick("Off", "关")
        }
    };

    let (mut tone, state, reason, retry) = match (&a, block, st.link) {
        (None, Some(_), _) => (
            "bad",
            l.pick("Can't read VPN passthrough", "读不到 VPN 穿透设置").to_string(),
            Some(
                l.pick(
                    "The data service couldn't read it from the firmware.",
                    "数据服务没能从固件读到。",
                )
                .to_string(),
            ),
            true,
        ),
        (None, None, Link::Down) => (
            "bad",
            l.pick("Can't read VPN passthrough", "读不到 VPN 穿透设置").to_string(),
            Some(l.pick("Can't reach the data service.", "连不上数据服务。").to_string()),
            true,
        ),
        (None, None, _) => (
            "neutral",
            l.pick("Reading VPN passthrough…", "正在读取 VPN 穿透…").to_string(),
            None,
            false,
        ),
        (Some((v, _)), _, _) => {
            let on: Vec<&str> = FIELDS
                .iter()
                .filter(|f| v[f.0].as_bool() == Some(true))
                .map(|f| f.2)
                .collect();
            let unknown: Vec<&str> = FIELDS
                .iter()
                .filter(|f| v[f.0].as_bool().is_none())
                .map(|f| f.2)
                .collect();
            if unknown.len() == FIELDS.len() {
                match sip {
                    Some(s) => (
                        "neutral",
                        l.pick(
                            "This firmware doesn't report VPN passthrough",
                            "这版固件不报告 VPN 穿透",
                        )
                        .to_string(),
                        Some(match l {
                            Lang::En => format!("It only reports SIP ALG ({}, shown below). L2TP / PPTP / IPSec can still be turned on or off, but the page can't read the change back to confirm it.", onoff(s)),
                            Lang::Zh => format!("它只报告 SIP ALG（{}，见下面）。L2TP / PPTP / IPSec 照样能打开或关掉，但页面读不回来确认。", onoff(s)),
                        }),
                        false,
                    ),
                    None => (
                        "warn",
                        l.pick("Passthrough state unknown", "穿透状态不明").to_string(),
                        Some(
                            l.pick(
                                "The device answered without any passthrough fields.",
                                "设备回了，但没有任何穿透字段。",
                            )
                            .to_string(),
                        ),
                        false,
                    ),
                }
            } else if on.is_empty() {
                (
                    "neutral",
                    l.pick("All passthrough off", "穿透全部关着").to_string(),
                    Some(
                        l.pick(
                            "VPN clients behind the U60 may fail to connect.",
                            "U60 后面的 VPN 客户端可能连不上。",
                        )
                        .to_string(),
                    ),
                    false,
                )
            } else {
                let list = on.join(" · ");
                (
                    "ok",
                    match l {
                        Lang::En => format!("On: {list}"),
                        Lang::Zh => format!("开着：{list}"),
                    },
                    (!unknown.is_empty()).then(|| {
                        let u = unknown.join(" · ");
                        match l {
                            Lang::En => format!("Not reported: {u}"),
                            Lang::Zh => format!("没报告：{u}"),
                        }
                    }),
                    false,
                )
            }
        }
    };
    let mut meta = None;
    if stale {
        tone = "stale";
        meta = Some(
            match st.link {
                Link::Live => l.pick(
                    "Not current: the data service hasn't refreshed this — refresh before changing anything",
                    "不是最新：数据服务没刷新到，改之前先刷新",
                ),
                _ => l.pick(
                    "Not current: the data service is not connected — refresh before changing anything",
                    "不是最新：数据服务没连上，改之前先刷新",
                ),
            }
            .to_string(),
        );
    }
    View {
        l,
        tone,
        state,
        reason,
        meta,
        retry: retry || stale,
        rows,
        sip,
        dis: a.is_none() || stale || busy.is_some(),
        stale,
    }
}

#[derive(Template)]
#[template(path = "router_vpn_leaves.html")]
struct Leaves<'a> {
    v: &'a View,
}

fn leaves(app: &Shared, busy: Option<(&str, bool)>, l: Lang) -> Vec<(String, String)> {
    app.hub.read(|st| {
        split_leaves(
            &Leaves {
                v: &view(st, busy, l),
            }
            .render()
            .unwrap(),
        )
    })
}

fn rows_leaf(app: &Shared, busy: Option<(&str, bool)>, l: Lang) -> String {
    leaves(app, busy, l)
        .into_iter()
        .find(|(k, _)| k == "vpn-rows")
        .map_or_else(String::new, |(_, h)| h)
}

// ---- confirm, gate, write ----

fn action(name: &str, on: bool, l: Lang) -> String {
    match (l, on) {
        (Lang::En, true) => format!("turn {name} passthrough on"),
        (Lang::En, false) => format!("turn {name} passthrough off"),
        (Lang::Zh, true) => format!("打开 {name} 穿透"),
        (Lang::Zh, false) => format!("关掉 {name} 穿透"),
    }
}

/// Tier 2 (inline) on the LAN, tier 3 (dialog) over Tailscale: switching passthrough reloads the
/// firewall, which can drop a remote session.
pub fn confirm(key: &str, on: bool, remote: bool, l: Lang) -> write::Confirm {
    let name = field(key).map_or("", |f| f.2);
    let what = match (l, on) {
        (Lang::En, true) => format!("Devices on the U60 can open {name} VPN connections to servers outside."),
        (Lang::En, false) => format!("{name} VPN connections from devices on the U60 stop working; open ones drop."),
        (Lang::Zh, true) => format!("连着 U60 的设备可以向外面的服务器建立 {name} VPN 连接。"),
        (Lang::Zh, false) => format!("连着 U60 的设备的 {name} VPN 连接不能用了，已连上的会断开。"),
    };
    let act = action(name, on, l);
    write::Confirm {
        tier: if remote {
            write::Tier::Dialog
        } else {
            write::Tier::Inline
        },
        title: match l {
            Lang::En => format!("Change {name} passthrough?"),
            Lang::Zh => format!("改 {name} 穿透？"),
        },
        what,
        downtime: Some(
            l.pick(
                "Usually none; the firewall rules reload in a moment.",
                "一般不会断；防火墙规则会很快重新加载。",
            )
            .into(),
        ),
        recovery: Some(
            l.pick(
                "Flip the switch back here. If this page stops loading over Tailscale, someone at the device can change it from a device on its Wi-Fi.",
                "在这里把开关拨回去。如果经 Tailscale 打不开这页了，要有人在设备旁边用连着它 Wi-Fi 的设备改回来。",
            )
            .into(),
        ),
        action: match l {
            Lang::En => format!("Confirm: {act}"),
            Lang::Zh => format!("确认：{act}"),
        },
        cuts_uplink: true,
        word: None,
        danger: false,
    }
}

/// The switch's current state if it can be changed now (None = not reported), else why not.
pub fn gate(st: &St, key: &str, l: Lang) -> Result<Option<bool>, write::Outcome> {
    match alg(st) {
        Some((v, false)) => Ok(v[key].as_bool()),
        Some((_, true)) => Err(write::Outcome::NotYet(
            l.pick(
                "The settings shown are not current; refresh first",
                "显示的设置不是最新的，先刷新",
            )
            .into(),
        )),
        None => Err(write::Outcome::NotYet(
            l.pick(
                "VPN passthrough not read yet; try again shortly",
                "还没读到 VPN 穿透设置，稍后再试",
            )
            .into(),
        )),
    }
}

/// `reported`: the device reports this field, so the change can be read back.
pub fn write_of(key: &str, on: bool, reported: bool, l: Lang) -> write::Write {
    let (_, arg, name) = field(key).unwrap_or(FIELDS[0]);
    let k = key.to_owned();
    write::Write {
        action: "vendor.call",
        params: json!({"object": "zwrt_router.api", "method": "router_set_alg_switch",
            "args": {arg: if on { "1" } else { "0" }}}),
        readback: reported.then(|| -> write::Readback {
            Box::new(move |st: &St| flag(st, &k) == Some(on))
        }),
        applied: match (l, on) {
            (Lang::En, true) => format!("{name} passthrough on"),
            (Lang::En, false) => format!("{name} passthrough off"),
            (Lang::Zh, true) => format!("已打开 {name} 穿透"),
            (Lang::Zh, false) => format!("已关掉 {name} 穿透"),
        },
        timeout: write::READBACK,
        typed: None,
        request_timeout: None,
    }
}

/// A send to a field the device doesn't report: accepted, in words that say why it isn't confirmed.
fn unconfirmed(name: &str, l: Lang) -> write::Outcome {
    write::Outcome::AcceptedSaid(match l {
        Lang::En => format!("Accepted by the device, can't be read back to confirm ({name} passthrough is not reported)"),
        Lang::Zh => format!("设备已接受，读不回来确认（{name} 穿透设备不报告）"),
    })
}

// ---- the page and its stream ----

/// The page reads only u60d: the stream re-renders on `router` changes; `step` (also on Retry's
/// kick) re-renders too.
pub struct Poll {
    l: Lang,
}

impl Polled for Poll {
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        (leaves(app, None, self.l), Duration::from_secs(3600))
    }
    fn watches(&self, block: &str) -> bool {
        block == "router"
    }
    fn render(&mut self, app: &Shared) -> Option<Vec<(String, String)>> {
        Some(leaves(app, None, self.l))
    }
}

#[derive(Template)]
#[template(path = "router_vpn.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    leaves: Vec<(String, String)>,
    op: String,
}

impl Page {
    fn leaf(&self, id: &str) -> &str {
        self.leaves
            .iter()
            .find(|(k, _)| k == id)
            .map_or("", |(_, h)| h.as_str())
    }
}

fn area(l: Lang, remote: bool, key: &str, on: bool, phase: &write::Phase) -> String {
    let set = format!("/router/vpn/set?f={key}&on={on}");
    write::Area {
        id: "vpn-sw",
        l,
        remote,
        set: &set,
        cancel: "/router/vpn/cancel",
        phase,
    }
    .html()
}

pub async fn page(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
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
        leaves: leaves(&app, None, ctx.lang),
        op: area(ctx.lang, ctx.remote, "", false, &write::Phase::Idle),
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
    let p = Poll { l: ctx.lang };
    crate::agent::stream(app, ctx, q.g, PATH, p)
}

/// Retry / refresh: reconnect to u60d now and re-render.
pub async fn refresh(State(app): State<Shared>) -> axum::http::StatusCode {
    app.hub.retry();
    let _ = app.kick.send(PATH);
    axum::http::StatusCode::NO_CONTENT
}

fn one(html: String) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(futures_util::stream::once(async move {
        Ok(crate::pages::patch(&html))
    }))
}

#[derive(Deserialize, Default)]
pub struct AskQuery {
    #[serde(default)]
    f: String,
    /// An unreported field's button: the state asked for. Ignored for a reported field (its
    /// switch asks for the other state).
    #[serde(default)]
    on: Option<bool>,
}

/// What the confirm asks for: the other state of a reported field, the button's for an
/// unreported one (None: an unreported field without a button's state).
pub fn target(cur: Option<bool>, asked: Option<bool>) -> Option<bool> {
    match cur {
        Some(c) => Some(!c),
        None => asked,
    }
}

/// A switch (the confirm for the other state) or an unreported field's 「打开」/「关掉」, or why not.
pub async fn ask(
    State(app): State<Shared>,
    Query(q): Query<AskQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let Some((key, _, _)) = field(&q.f) else {
        return one(area(l, ctx.remote, "", false, &write::Phase::Idle));
    };
    let (target, phase) = match app.hub.read(|st| gate(st, key, l)) {
        Ok(cur) => match target(cur, q.on) {
            Some(t) => (t, write::Phase::Ask(confirm(key, t, ctx.remote, l))),
            None => (false, write::Phase::Idle),
        },
        Err(o) => (false, write::Phase::Done(o)),
    };
    one(area(l, ctx.remote, key, target, &phase))
}

#[derive(Deserialize)]
pub struct SetQuery {
    f: String,
    on: bool,
}

/// The confirmed switch: gate again, `router_set_alg_switch` once, read back `router.alg`.
pub async fn set(
    State(app): State<Shared>,
    Query(q): Query<SetQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    let key = field(&q.f).map(|f| f.0);
    let on = q.on;
    let pending = format!(
        "{}\n{}",
        rows_leaf(&app, key.map(|k| (k, on)), l),
        area(
            l,
            false,
            key.unwrap_or(""),
            on,
            &write::Phase::Pending(String::new())
        )
    );
    write::respond(pending, async move {
        let o = match key {
            None => write::Outcome::Failed(l.pick("unknown switch", "不认识的开关").into()),
            Some(k) => match app.hub.read(|st| gate(st, k, l)) {
                Err(o) => o,
                Ok(cur) => {
                    let reported = cur.is_some();
                    let o = write::execute(
                        &app.http,
                        &app.control_url,
                        &app.hub,
                        &write_of(k, on, reported, l),
                        l,
                    )
                    .await;
                    match o {
                        write::Outcome::Accepted if !reported => {
                            unconfirmed(field(k).map_or("", |f| f.2), l)
                        }
                        o => o,
                    }
                }
            },
        };
        format!(
            "{}\n{}",
            rows_leaf(&app, None, l),
            area(l, false, key.unwrap_or(""), on, &write::Phase::Done(o))
        )
    })
}

pub async fn cancel(
    Query(q): Query<write::CancelQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    one(area(l, false, "", false, &write::cancel_phase(&q)))
}

#[cfg(test)]
mod tests;
