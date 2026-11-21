//! The frame's live parts on every migrated page (8c-A step 4): the top-bar status icons
//! and the alert bar under them.
//!
//! ```text
//!  u60d blocks (one SSE, hub.rs): signal, interfaces → network · wlan → Wi-Fi · sms → SMS
//!  poll() while tabs are open:  u60d /v2/api/public/status → services        ┐ every 10 s
//!                               u60d /v2/api/alerts       → bar              ┘ (and right after "Got it")
//!  each tab's stream: render(ctx) → leaves by id → only the changed ones are sent
//!  POST /alerts/read?seq ─▶ u60d /api/alerts/read (agent.rs) ─▶ refetch ─▶ bar
//! ```
//! Where the browser came from ([`Via`], docs/designs/remote-path-guard.md) is decided here from
//! its peer address and the device's LAN interfaces. Missing evidence is treated as remote.

use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use askama::Template;
use axum::extract::{ConnectInfo, Query, Request, State};
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;

use crate::hub::{Change, Hub, St};
use crate::i18n::Lang;
use crate::pages::Shared;

/// Status icons drawn filled when on (the sprite carries their fill symbols too).
pub const FILLED: [&str; 3] = ["WifiHigh", "Waves", "ChatCircleText"];

/// u60d's side reads (local files, cheap).
const POLL: Duration = Duration::from_secs(10);

/// What a tab's frame depends on besides the hub: who is looking, from where, at which page.
#[derive(Clone, Debug)]
pub struct Ctx {
    pub lang: Lang,
    pub path: String,
    pub remote: Via,
}

impl Ctx {
    pub fn of(h: &HeaderMap, peer: Option<IpAddr>, path: &str) -> Ctx {
        Ctx {
            lang: Lang::from_headers(h),
            path: path.to_string(),
            remote: via(h, peer),
        }
    }

    /// From a request: headers, and the peer address when the server recorded it.
    pub fn of_request(req: &Request, path: &str) -> Ctx {
        let peer = req
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip());
        Ctx::of(req.headers(), peer, path)
    }
}

// ---- where the browser came from (R18-P0-04, docs/designs/remote-path-guard.md) ----

/// The path the browser reached the device over: what a write could cut it off from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Via {
    /// On the device's LAN (wired, Wi-Fi, USB) or the device itself.
    Local,
    /// The peer is outside the device's known LAN.
    Remote,
    /// The peer or LAN networks are unavailable: warn like a remote connection.
    Unknown,
}

impl Via {
    /// Not on the LAN: writes that can cut the uplink get the remote warning.
    pub fn remote(self) -> bool {
        self != Via::Local
    }

    /// The top-bar badge, narrow screens.
    pub fn short(self, l: impl std::borrow::Borrow<Lang>) -> &'static str {
        let l = *l.borrow();
        match self {
            Via::Unknown => l.pick("Unknown source", "来源不明"),
            _ => l.pick("Remote", "远程"),
        }
    }

    /// The top-bar badge, wide screens.
    pub fn long(self, l: impl std::borrow::Borrow<Lang>) -> &'static str {
        let l = *l.borrow();
        match self {
            Via::Local => "",
            Via::Remote => l.pick("Remote connection", "远程访问"),
            Via::Unknown => l.pick("Unknown source", "来源不明"),
        }
    }

    /// The badge's title: which path, and what can cut it.
    pub fn why(self, l: impl std::borrow::Borrow<Lang>) -> &'static str {
        let l = *l.borrow();
        match self {
            Via::Local => "",
            Via::Remote => l.pick(
                "Connected remotely: changing the network, bands or LAN address can cut you off",
                "你正远程连接设备；改网络、锁频或改局域网地址可能让你断开",
            ),
            Via::Unknown => l.pick(
                "The connection source is unknown, so it is treated as remote: changing the network can cut you off",
                "认不出你从哪条路连进来，按远程处理：改网络可能让你断开",
            ),
        }
    }

    /// The confirm's warning before a write that can cut the uplink (write.html): the path, whether
    /// u60d reverts this step by itself (`Confirm::revert`), and how to take over locally
    /// (remote-path-guard.md).
    pub fn warn(self, l: impl std::borrow::Borrow<Lang>, revert: impl std::borrow::Borrow<crate::write::Revert>) -> String {
        let (l, revert) = (*l.borrow(), *revert.borrow());
        let path = match self {
            Via::Local => return String::new(),
            Via::Remote => l.pick("You are connected remotely.", "你正在远程操作。"),
            Via::Unknown => l.pick(
                "Your connection source is unknown; it is treated as remote.",
                "认不出你从哪条路连进来，按远程处理。",
            ),
        };
        use crate::write::Revert;
        let first = match revert {
            Revert::On => l.pick(
                "If this step cuts the mobile connection, the device reverts it by itself after the check; if the page still can't be reached, ",
                "如果这一步断了移动网络，设备检查后会自动退回；退回后还是打不开，",
            ),
            Revert::Off => l.pick(
                "Automatic revert is off: if this step cuts the connection it stays cut, and ",
                "自动退回没开：这一步断了网就一直断着，",
            ),
            Revert::No => l.pick(
                "This step is not reverted automatically: if it cuts the connection, ",
                "这一步不会自动退回：如果断了网，",
            ),
        };
        let rest = format!(
            "{first}{}",
            l.pick(
                "someone has to use the touch screen at the device, or join its Wi-Fi, cable or USB and open this page on the LAN.",
                "要有人在设备旁边用触屏，或连上设备的 Wi-Fi、网线、USB，在局域网里打开管理网页恢复。",
            )
        );
        format!("{path}{}{rest}", if l == Lang::En { " " } else { "" })
    }
}

/// v4-mapped v6 (a `[::]` listener) as plain v4.
fn plain(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    }
}

/// One interface network: address and prefix length.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Net(IpAddr, u8);

impl Net {
    fn contains(self, ip: IpAddr) -> bool {
        let bits = |a: IpAddr| -> u128 {
            match a {
                IpAddr::V4(v4) => u128::from(u32::from(v4)) << 96,
                IpAddr::V6(v6) => u128::from(v6),
            }
        };
        let (Net(net, len), ip) = (self, plain(ip));
        if net.is_ipv4() != ip.is_ipv4() {
            return false;
        }
        // v4 sits in the top 32 bits, so one mask from the top serves both families
        let len = u32::from(len).min(if net.is_ipv4() { 32 } else { 128 });
        if len == 0 {
            return true;
        }
        let mask = u128::MAX << (128 - len);
        bits(net) & mask == bits(ip) & mask
    }
}

/// The LAN networks out of `ip -o addr show` ("16: br-lan    inet 192.168.0.1/24 brd …").
pub fn lan_nets(ip_addr: &str) -> Vec<Net> {
    ip_addr
        .lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace().skip(1);
            let name = w.next()?.trim_end_matches(':').split('@').next()?;
            let fam = w.next()?;
            let (a, len) = w.next()?.split_once('/')?;
            if name != "br-lan" || !(fam == "inet" || fam == "inet6") {
                return None;
            }
            let ip: IpAddr = a.parse().ok()?;
            let prefix: u8 = len.parse().ok()?;
            let max_prefix = if ip.is_ipv4() { 32 } else { 128 };
            if prefix == 0 || prefix > max_prefix {
                return None;
            }
            Some(Net(ip, prefix))
        })
        .collect()
}

/// Classify only from the peer and known LAN networks, never from the untrusted Host header.
pub fn classify(peer: Option<IpAddr>, lan: &[Net]) -> Via {
    let Some(peer) = peer.map(plain) else {
        return Via::Unknown;
    };
    if peer.is_loopback() || lan.iter().any(|n| n.contains(peer)) {
        return Via::Local;
    }
    if !lan.iter().any(|Net(net, _)| net.is_ipv4() == peer.is_ipv4()) {
        Via::Unknown
    } else {
        Via::Remote
    }
}

const LAN_TTL: Duration = Duration::from_secs(30);

/// `ip`'s output; empty when it failed. None = there is no `ip` at all.
fn run_ip(args: &[&str]) -> Option<String> {
    match std::process::Command::new("ip").args(args).output() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => Some(String::new()),
        Ok(o) if !o.status.success() => Some(String::new()),
        Ok(o) => Some(String::from_utf8_lossy(&o.stdout).into_owned()),
    }
}

/// The LAN networks, or None off the device (no `ip`).
fn cached_lan() -> Option<Vec<Net>> {
    static C: Mutex<Option<(Instant, Option<Vec<Net>>)>> = Mutex::new(None);
    let mut c = C.lock().unwrap_or_else(|e| e.into_inner());
    match &*c {
        Some((at, nets)) if at.elapsed() < LAN_TTL => nets.clone(),
        _ => {
            let nets = run_ip(&["-o", "addr", "show"]).map(|o| lan_nets(&o));
            // a fallback that changes every warning is said once, not silently
            static SAID: std::sync::Once = std::sync::Once::new();
            match &nets {
                None => SAID.call_once(|| {
                    eprintln!("[luyaz-web] no `ip`: non-loopback viewers have unknown origin")
                }),
                Some(n) if n.is_empty() => SAID.call_once(|| {
                    eprintln!("[luyaz-web] `ip -o addr` gave no LAN network: non-loopback viewers have unknown origin")
                }),
                Some(_) => {}
            }
            *c = Some((Instant::now(), nets.clone()));
            nets
        }
    }
}

/// Where this request came from, read off the device (interfaces cached 30 s).
pub fn via(_h: &HeaderMap, peer: Option<IpAddr>) -> Via {
    if peer.map(plain).is_some_and(|p| p.is_loopback()) {
        return Via::Local;
    }
    match cached_lan() {
        Some(lan) => classify(peer, &lan),
        None => Via::Unknown,
    }
}

// ---- the view ----

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tone {
    Ok,
    Warn,
    Bad,
    Off,
}

pub struct Icon {
    pub id: &'static str,
    pub href: &'static str,
    pub icon: &'static str,
    pub tone: Tone,
    pub label: String,
    pub badge: u64,
    /// No answer yet: an empty placeholder keeps the id in the page for later patches.
    pub known: bool,
}

impl Icon {
    pub fn class(&self) -> &'static str {
        match self.tone {
            Tone::Ok => "t-st",
            Tone::Warn => "t-st t-st--warn",
            Tone::Bad => "t-st t-st--bad",
            Tone::Off => "t-st t-st--off",
        }
    }
    /// Filled when on, bold when off; the signal icon is always bold (filled it is a wedge).
    pub fn svg(&self) -> String {
        let fill = self.tone != Tone::Off && self.icon != "CellSignalFull";
        crate::shell::icon(self.icon, fill, 20, "")
    }
    pub fn badge_text(&self) -> String {
        if self.badge > 9 {
            "9+".into()
        } else {
            self.badge.to_string()
        }
    }
}

pub struct Bar {
    pub count: u64,
    pub label: String,
    pub detail: String,
    /// The newest event's seq: "Got it" marks everything up to it read.
    pub seq: u64,
    pub error: Option<String>,
}

#[derive(Template)]
#[template(path = "topbar.html")]
struct TopbarFrag<'a> {
    l: Lang,
    remote: Via,
    icons: &'a [Icon],
}

#[derive(Template)]
#[template(path = "alertbar.html")]
struct AlertbarFrag<'a> {
    l: Lang,
    bar: Option<&'a Bar>,
}

/// `interfaces.cellular` read through [`luyaz_api::firmware::wwaniface_connected`].
pub fn cell_up(c: &Value) -> Option<bool> {
    luyaz_api::firmware::wwaniface_connected(
        luyaz_api::firmware::Support::Unknown,
        c["connect_status"].as_str().unwrap_or(""),
        c["ipv4_address"].as_str(),
        None,
    )
}

fn block<'a>(st: &'a St, name: &str) -> Option<&'a Value> {
    st.blocks.get(name).map(|b| &b.data)
}

fn int(v: &Value) -> Option<i64> {
    crate::view::num(v).map(|f| f as i64)
}

pub fn icons(st: &St, l: Lang) -> Vec<Icon> {
    let t = |en, zh| l.pick(en, zh).to_string();
    let mut out = Vec::new();

    let sig = block(st, "signal");
    let cell = block(st, "interfaces").map(|i| &i["cellular"]);
    let known = sig.is_some() || cell.is_some();
    // B31 keeps claiming connected after a drop; only an IPv4 address confirms it.
    // None = claimed but unconfirmed: shown neutral, not as on or down.
    let up = cell.map(cell_up).unwrap_or(Some(false));
    let ty = sig
        .and_then(|s| s["type"].as_str())
        .unwrap_or("")
        .to_ascii_uppercase();
    let rsrp = sig
        .and_then(|s| {
            int(if ty == "LTE" {
                &s["lte_rsrp"]
            } else {
                &s["nr_rsrp"]
            })
        })
        .unwrap_or(0);
    let weak = rsrp != 0 && rsrp <= -110;
    let mut label = match up {
        Some(true) => t("Mobile data on", "移动数据已通"),
        Some(false) => t("Mobile data down", "移动数据断了"),
        None => t("Mobile data unconfirmed", "移动数据未确认"),
    };
    let on = up == Some(true);
    if on && !ty.is_empty() {
        label = format!("{label} · {ty}");
    }
    if on && weak {
        label = format!("{label} · {}", l.pick("weak signal", "信号弱"));
    }
    out.push(Icon {
        id: "st-net",
        href: "/signal",
        icon: "CellSignalFull",
        tone: match (up, weak) {
            (None, _) => Tone::Off,
            (Some(false), _) => Tone::Bad,
            (Some(true), true) => Tone::Warn,
            (Some(true), false) => Tone::Ok,
        },
        label,
        badge: 0,
        known,
    });

    let wlan = block(st, "wlan");
    let wifi = wlan.and_then(|w| int(&w["enabled"])).unwrap_or(0) != 0;
    out.push(Icon {
        id: "st-wifi",
        href: "/router/wifi",
        icon: "WifiHigh",
        tone: if wifi { Tone::Ok } else { Tone::Off },
        label: if wifi {
            t("Wi-Fi on", "Wi-Fi 已开启")
        } else {
            t("Wi-Fi off", "Wi-Fi 已关闭")
        },
        badge: 0,
        known: wlan.is_some(),
    });

    let svc = |name: &str, state: Tone| -> String {
        match (state, l) {
            (Tone::Ok, Lang::En) => format!("{name} running"),
            (Tone::Ok, Lang::Zh) => format!("{name} 运行中"),
            (Tone::Warn, Lang::En) => format!("{name} stopped"),
            (Tone::Warn, Lang::Zh) => format!("{name} 已停止"),
            (_, Lang::En) => format!("{name} not installed"),
            (_, Lang::Zh) => format!("{name} 未安装"),
        }
    };
    let chill = st.side.public.as_ref().map(|p| &p["services"]["chill"]);
    let chill_state = chill.and_then(|c| c["state"].as_str()).unwrap_or("");
    let chill_tone = match chill_state {
        "running" => Tone::Ok,
        "direct" => Tone::Warn,
        _ => Tone::Off,
    };
    out.push(Icon {
        id: "st-chill",
        href: "/services/chill",
        icon: "Waves",
        tone: chill_tone,
        // the old bar says "stopped" for both direct and stopped
        label: svc(
            "CHILL",
            if chill_state == "stopped" {
                Tone::Warn
            } else {
                chill_tone
            },
        ),
        badge: 0,
        known: chill.is_some_and(|c| c.is_object()),
    });

    let sms = block(st, "sms");
    let unread = sms.and_then(|s| int(&s["unread"])).unwrap_or(0).max(0) as u64;
    out.push(Icon {
        id: "st-sms",
        href: "/sms",
        icon: "ChatCircleText",
        tone: if unread > 0 { Tone::Ok } else { Tone::Off },
        label: match (unread, l) {
            (0, _) => t("No unread SMS", "无未读短信"),
            (n, Lang::En) => format!("{n} unread SMS"),
            (n, Lang::Zh) => format!("{n} 条未读短信"),
        },
        badge: unread,
        known: sms.is_some(),
    });
    out
}

/// The alert bar, or None: no unread alerts, the list not read yet, or on /alerts itself.
pub fn bar(st: &St, ctx: &Ctx) -> Option<Bar> {
    if ctx.path.trim_end_matches('/') == "/alerts" {
        return None;
    }
    let a = st.side.alerts.as_ref()?;
    let count = a["unread"].as_u64().filter(|n| *n > 0)?;
    let events = a["events"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let latest = events
        .iter()
        .find(|e| e["unread"].as_bool() == Some(true))
        .or(events.first());
    let l = ctx.lang;
    let label = latest
        .map(|e| {
            let (zh, en) = (e["label"].as_str(), e["label_en"].as_str());
            let picked = match l {
                Lang::En => en.or(zh),
                Lang::Zh => zh.or(en),
            };
            picked
                .filter(|s| !s.is_empty())
                .or(e["kind"].as_str())
                .unwrap_or("")
                .to_string()
        })
        .unwrap_or_default();
    let sms = &a["sms"];
    let sms_note = if sms["configured"].as_bool() != Some(true) {
        Some(l.pick("SMS alerts are not set up.", "短信告警未配置。"))
    } else if sms["recent"][0]["result"].as_str() == Some("failed") {
        Some(l.pick(
            "The last alert SMS failed to send.",
            "上一条告警短信发送失败。",
        ))
    } else {
        None
    };
    // the core prefixes some texts with its event id ("[fa717fe4-22962] …"); not for people
    let text = latest.and_then(|e| e["text"].as_str()).unwrap_or("");
    let text = match text.strip_prefix('[').and_then(|r| r.split_once("] ")) {
        Some((id, rest)) if !id.contains(' ') => rest,
        _ => text,
    };
    let detail = [text, sms_note.unwrap_or("")]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    Some(Bar {
        count,
        label,
        detail,
        seq: events.first().and_then(|e| e["seq"].as_u64()).unwrap_or(0),
        error: None,
    })
}

impl Bar {
    pub fn title(&self, l: Lang) -> String {
        let n = match l {
            Lang::En => format!("{} new alert(s)", self.count),
            Lang::Zh => format!("{} 条新告警", self.count),
        };
        if self.label.is_empty() {
            n
        } else {
            format!("{n}{}{}", l.pick(": ", "："), self.label)
        }
    }
}

fn topbar_html(st: &St, ctx: &Ctx) -> String {
    TopbarFrag {
        l: ctx.lang,
        remote: ctx.remote,
        icons: &icons(st, ctx.lang),
    }
    .render()
    .unwrap()
}

fn alertbar_html(bar: Option<&Bar>, l: Lang) -> String {
    AlertbarFrag { l, bar }.render().unwrap()
}

/// The frame's live HTML for the first page render: (top-bar status, banners: op bar + alert bar).
pub fn initial(st: &St, ctx: &Ctx) -> (String, String) {
    let (op, clock) =
        crate::opbar::view(st, &mut crate::opbar::Seen::default(), ctx, Instant::now());
    let op = fill(
        &crate::opbar::bar_html(op.as_ref(), ctx.lang),
        &crate::opbar::clock_html(clock.as_ref()),
    );
    (
        topbar_html(st, ctx),
        op + "\n" + &alertbar_html(bar(st, ctx).as_ref(), ctx.lang),
    )
}

/// The op bar's clock goes where the bar left `<!--slot:op-bar-clock-->`.
pub fn fill(bar: &str, clock: &str) -> String {
    bar.replacen("<!--slot:op-bar-clock-->", clock, 1)
}

/// The frame's leaves by id, read through this stream's `seen` (the op bar's memory). The op bar
/// is compared without its clock, so a running countdown sends only `#op-bar-clock`.
pub fn leaves(st: &St, ctx: &Ctx, seen: &mut crate::opbar::Seen) -> Vec<(String, String)> {
    let mut out = crate::pages::split_leaves(&topbar_html(st, ctx));
    let (op, clock) = crate::opbar::view(st, seen, ctx, Instant::now());
    out.push((
        "op-bar".into(),
        crate::opbar::bar_html(op.as_ref(), ctx.lang),
    ));
    if op.is_some() {
        out.push((
            "op-bar-clock".into(),
            crate::opbar::clock_html(clock.as_ref()),
        ));
    }
    out.push((
        "alert-bar".into(),
        alertbar_html(bar(st, ctx).as_ref(), ctx.lang),
    ));
    out
}

/// The leaves that changed since `sent`; a resent op bar carries the current clock.
pub fn diff(sent: &[(String, String)], now: &[(String, String)]) -> Vec<String> {
    let changed = |id: &str, html: &str| !sent.iter().any(|(i, h)| i == id && h == html);
    let bar_sent = now.iter().any(|(id, h)| id == "op-bar" && changed(id, h));
    let clock = now
        .iter()
        .find(|(id, _)| id == "op-bar-clock")
        .map_or("", |(_, h)| h.as_str());
    now.iter()
        .filter(|(id, html)| changed(id, html))
        .filter(|(id, _)| !(bar_sent && id == "op-bar-clock"))
        .map(|(id, html)| {
            if id == "op-bar" {
                fill(html, clock)
            } else {
                html.clone()
            }
        })
        .collect()
}

/// When this stream must look again although nothing arrived (the op bar's clock).
pub async fn wake(at: Option<Instant>) {
    match at {
        Some(t) => tokio::time::sleep_until(t.into()).await,
        None => std::future::pending().await,
    }
}

pub fn reads(name: &str) -> bool {
    matches!(
        name,
        "signal" | "interfaces" | "wlan" | "sms" | "side" | "op" | "exec"
    )
}

// ---- the poller ----

async fn get_data(http: &reqwest::Client, url: &str) -> Option<Value> {
    let v: Value = http.get(url).send().await.ok()?.json().await.ok()?;
    (v["ok"].as_bool() == Some(true)).then(|| v["data"].clone())
}

/// Polls the side reads while at least one tab is open; `kick` asks for a round now.
pub async fn poll(
    hub: Arc<Hub>,
    http: reqwest::Client,
    u60d: String,
    kick: Arc<tokio::sync::Notify>,
) {
    let mut subs = hub.subs_watch();
    let (public_url, alerts_url) = (
        format!("{u60d}/v2/api/public/status"),
        format!("{u60d}/v2/api/alerts"),
    );
    loop {
        while *subs.borrow_and_update() == 0 {
            if subs.changed().await.is_err() {
                return;
            }
        }
        let (public, alerts) =
            tokio::join!(get_data(&http, &public_url), get_data(&http, &alerts_url),);
        hub.set_side(|s| {
            s.agent = public.clone();
            s.public = public;
            s.alerts = alerts;
        });
        tokio::select! {
            _ = tokio::time::sleep(POLL) => {}
            _ = kick.notified() => {}
        }
    }
}

// ---- streams and the one write ----

#[derive(Deserialize)]
pub struct FrameQuery {
    #[serde(default)]
    g: u64,
    /// The page this stream belongs to (the alert bar hides on /alerts).
    #[serde(default)]
    p: String,
}

/// The stream of a page that has no live parts of its own (the hub pages): the frame only.
pub async fn events(
    State(app): State<Shared>,
    Query(q): Query<FrameQuery>,
    req: Request,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let path = if q.p.starts_with('/') {
        q.p
    } else {
        "/".into()
    };
    let ctx = Ctx::of_request(&req, &path);
    let mut sub = app.hub.subscribe();
    let stream = crate::pages::async_stream(move |tx| async move {
        let mut seen = crate::opbar::Seen::default();
        let (mut sent, g) = app
            .hub
            .read(|st| (leaves(st, &ctx, &mut seen), st.generation));
        if g != q.g {
            let all = diff(&[], &sent);
            if !crate::pages::send_out(&tx, all).await {
                return;
            }
        }
        loop {
            // Nothing may change for minutes: notice a closed tab from the channel (axum drops
            // the stream when a keep-alive write fails), not only when there is something to send.
            let at = app
                .hub
                .read(|st| crate::opbar::wake(st, &seen, Instant::now()));
            let r = tokio::select! {
                r = sub.rx.recv() => Some(r),
                _ = wake(at) => None,
                _ = tx.closed() => return,
            };
            match r {
                Some(Ok(Change::Block(name))) if !reads(&name) => continue,
                None | Some(Ok(_) | Err(RecvError::Lagged(_))) => {}
                Some(Err(RecvError::Closed)) => return,
            }
            let now = app.hub.read(|st| leaves(st, &ctx, &mut seen));
            let out = diff(&sent, &now);
            sent = now;
            if !crate::pages::send_out(&tx, out).await {
                return;
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

#[derive(Deserialize)]
pub struct ReadQuery {
    seq: u64,
    #[serde(default)]
    p: String,
}

/// "Got it": mark alerts read (u60d's `/api/alerts/read` since 8d-1b, the agent's with this tab's
/// own session when u60d does not take it), read the list again, answer with the bar.
pub async fn read(
    State(app): State<Shared>,
    Query(q): Query<ReadQuery>,
    req: Request,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let path = if q.p.starts_with('/') {
        q.p
    } else {
        "/".into()
    };
    let ctx = Ctx::of_request(&req, &path);
    // u60d's /v2/api/alerts/read first (8d-1b), the agent's when u60d does not take it (agent.rs)
    let who = crate::agent::Caller::of(&app, &req);
    let body = json!({"seq": q.seq});
    let err = match crate::agent::post_raw(&app, &who, "/api/alerts/read", &body).await {
        Ok((code, _)) if (200..300).contains(&code) => None,
        Ok((code, _)) => Some(format!("HTTP {code}")),
        Err(crate::agent::Fail::SignedOut) => {
            Some(ctx.lang.pick("Signed out", "已退出登录").to_string())
        }
        Err(f) => Some(f.text(ctx.lang)),
    };
    if err.is_none() {
        let alerts = get_data(&app.http, &format!("{}/v2/api/alerts", app.u60d_url)).await;
        if alerts.is_some() {
            app.hub.set_side(|s| s.alerts = alerts);
        }
        app.side_kick.notify_one();
    }
    let html = app.hub.read(|st| {
        let mut b = bar(st, &ctx);
        if let (Some(b), Some(e)) = (b.as_mut(), err) {
            b.error = Some(e);
        }
        alertbar_html(b.as_ref(), ctx.lang)
    });
    Sse::new(futures_util::stream::once(async move {
        Ok(crate::pages::patch(&html))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sse::Frame;

    fn hub_with(blocks: Value) -> Arc<Hub> {
        let hub = Hub::new();
        let mut bs = serde_json::Map::new();
        for (k, v) in blocks.as_object().unwrap() {
            bs.insert(
                k.clone(),
                json!({"revision":1,"observed_at":1,"stale":false,"data":v}),
            );
        }
        hub.apply(&Frame {
            event: "snapshot".into(),
            data: json!({"epoch":"e","blocks":bs}).to_string(),
        });
        hub
    }

    fn ctx(path: &str) -> Ctx {
        Ctx {
            lang: Lang::Zh,
            path: path.into(),
            remote: Via::Local,
        }
    }

    #[test]
    fn disconnected_is_not_connected() {
        use luyaz_api::firmware::wwaniface_claims_connected as connected;
        assert!(connected("ipv4_ipv6_connected"));
        assert!(connected("connected"));
        assert!(!connected("disconnected"));
        assert!(!connected("ipv4_disconnected"));
        assert!(!connected("connecting"));
        assert!(!connected(""));
    }

    #[test]
    fn icons_from_blocks() {
        let hub = hub_with(json!({
            "signal": {"type": "SA", "nr_rsrp": -112, "lte_rsrp": -90},
            "interfaces": {"cellular": {"connect_status": "ipv4_ipv6_connected", "ipv4_address": "10.1.2.3"}},
            "wlan": {"enabled": 0},
            "sms": {"unread": 12}
        }));
        let ic = hub.read(|st| icons(st, Lang::Zh));
        let net = &ic[0];
        assert_eq!(net.tone, Tone::Warn, "SA reads nr_rsrp, -112 is weak");
        assert_eq!(net.label, "移动数据已通 · SA · 信号弱");
        assert_eq!(ic[1].tone, Tone::Off);
        assert_eq!(ic[3].badge_text(), "9+");
        assert!(!ic[2].known, "no side data yet");
        let html = hub.read(|st| topbar_html(st, &ctx("/signal")));
        assert!(html.contains("id=\"st-chill\""));
    }

    #[test]
    fn disconnected_cell_is_bad() {
        let hub = hub_with(json!({
            "signal": {"type": "LTE", "lte_rsrp": -95},
            "interfaces": {"cellular": {"connect_status": "disconnected"}}
        }));
        let ic = hub.read(|st| icons(st, Lang::En));
        assert_eq!(ic[0].tone, Tone::Bad);
        assert_eq!(ic[0].label, "Mobile data down");
    }

    #[test]
    fn connected_without_ipv4_is_unconfirmed() {
        let hub = hub_with(json!({
            "signal": {"type": "LTE", "lte_rsrp": -95},
            "interfaces": {"cellular": {"connect_status": "ipv4_ipv6_connected", "ipv4_address": ""}}
        }));
        let ic = hub.read(|st| icons(st, Lang::En));
        assert_eq!(ic[0].tone, Tone::Off);
        assert_eq!(ic[0].label, "Mobile data unconfirmed");
        assert_eq!(cell_up(&json!({"connect_status": "ipv4_connected"})), None);
        assert_eq!(cell_up(&json!({"connect_status": "disconnected", "ipv4_address": "10.0.0.2"})), Some(false));
        assert_eq!(cell_up(&json!({"connect_status": "connected", "ipv4_address": "10.0.0.2"})), Some(true));
    }

    fn alerts(unread: u64, configured: bool) -> Value {
        json!({"unread": unread, "events": [
            {"seq": 9, "kind": "wifi_down", "label": "Wi-Fi 掉了", "label_en": "Wi-Fi dropped", "text": "已自动恢复", "unread": true},
            {"seq": 8, "kind": "x", "text": "", "unread": false}
        ], "sms": {"configured": configured, "recent": []}})
    }

    #[test]
    fn the_bar_shows_unread_alerts_except_on_their_page() {
        let hub = hub_with(json!({}));
        hub.set_side(|s| s.alerts = Some(alerts(2, false)));
        let b = hub.read(|st| bar(st, &ctx("/signal"))).unwrap();
        assert_eq!(b.title(Lang::Zh), "2 条新告警：Wi-Fi 掉了");
        assert_eq!(b.detail, "已自动恢复 短信告警未配置。");
        assert_eq!(b.seq, 9);
        assert!(hub.read(|st| bar(st, &ctx("/alerts/"))).is_none());
        hub.set_side(|s| s.alerts = Some(alerts(0, true)));
        assert!(hub.read(|st| bar(st, &ctx("/signal"))).is_none());
        let html = alertbar_html(None, Lang::Zh);
        assert!(html.contains("id=\"alert-bar\""), "{html}");
    }

    #[test]
    fn a_side_change_sends_only_its_icon() {
        let hub = hub_with(json!({"wlan": {"enabled": 1}}));
        let c = ctx("/charts");
        let a = hub.read(|st| leaves(st, &c, &mut crate::opbar::Seen::default()));
        hub.set_side(|s| {
            s.public = Some(json!({"services": {"chill": {"state": "running"}}}))
        });
        let b = hub.read(|st| leaves(st, &c, &mut crate::opbar::Seen::default()));
        let out = diff(&a, &b);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].contains("id=\"st-chill\"") && out[0].contains("CHILL 运行中"));
        // same data again: no change event, nothing to send
        let g = hub.read(|st| st.generation);
        hub.set_side(|s| {
            s.public = Some(json!({"services": {"chill": {"state": "running"}}}))
        });
        assert_eq!(hub.read(|st| st.generation), g);
    }

    #[test]
    fn via_by_peer_and_lan() {
        let ip = |s: &str| Some(s.parse::<IpAddr>().unwrap());
        // the device's `ip -o addr show` (10-19), addresses swapped for documentation ones
        let lan = lan_nets(
            "1: lo    inet 127.0.0.1/8 scope host lo\\       valid_lft forever preferred_lft forever\n\
             16: br-lan    inet 192.168.0.1/24 brd 192.168.0.255 scope global br-lan\\       valid_lft forever\n\
             16: br-lan    inet6 fd00:66::1/64 scope global \\       valid_lft forever\n\
             17: rmnet_data0    inet 192.0.2.121/30 scope global rmnet_data0\\       valid_lft forever\n\
             17: rmnet_data0    inet6 2001:db8::5a1d/64 scope global \\       valid_lft forever\n\
             25: tun0    inet 198.51.100.1/24 scope global tun0\\       valid_lft forever\n\
             62: chill0    inet 198.18.0.1/30 brd 198.18.0.3 scope global chill0\\       valid_lft forever",
        );
        assert_eq!(lan.len(), 2, "only br-lan is the LAN: {lan:?}");
        let c = |peer: Option<IpAddr>| classify(peer, &lan);
        assert_eq!(c(ip("192.168.0.23")), Via::Local);
        assert_eq!(c(ip("::ffff:192.168.0.23")), Via::Local);
        assert_eq!(c(ip("fd00:66::9")), Via::Local);
        assert_eq!(c(ip("127.0.0.1")), Via::Local);
        assert_eq!(c(ip("198.51.100.88")), Via::Remote);
        assert_eq!(c(ip("192.0.2.122")), Via::Remote);
        assert_eq!(c(None), Via::Unknown);
        assert_eq!(classify(ip("192.168.0.23"), &[]), Via::Unknown);
        assert_eq!(classify(ip("2001:db8::1"), &[Net("192.168.0.1".parse().unwrap(), 24)]), Via::Unknown);
        assert_eq!(classify(ip("127.0.0.1"), &[]), Via::Local);
        assert!(lan_nets("1: br-lan inet 192.168.0.1/0\n1: br-lan inet 192.168.0.1/33").is_empty());
        assert!(!Via::Local.remote() && Via::Remote.remote() && Via::Unknown.remote());
    }

    #[test]
    fn warn_says_whether_the_step_reverts() {
        use crate::write::Revert;
        let w = |r| Via::Remote.warn(Lang::Zh, r);
        assert!(w(Revert::On).contains("自动退回；"), "{}", w(Revert::On));
        assert!(w(Revert::Off).contains("自动退回没开"));
        assert!(w(Revert::No).contains("不会自动退回"));
        assert!(Via::Local.warn(Lang::Zh, Revert::No).is_empty());
        assert_eq!(Revert::txn(None), Revert::No);
        assert_eq!(Revert::txn(Some(&serde_json::json!({"rollback_enabled": false}))), Revert::Off);
        assert_eq!(Revert::txn(Some(&serde_json::json!({"rollback_enabled": true}))), Revert::On);
    }
}
