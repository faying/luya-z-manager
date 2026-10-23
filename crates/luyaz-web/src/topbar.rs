//! The frame's live parts on every migrated page (8c-A step 4): the top-bar status icons
//! and the alert bar under them.
//!
//! ```text
//!  u60d blocks (one SSE, hub.rs): signal, interfaces → network · wlan → Wi-Fi · sms → SMS
//!  poll() while tabs are open:  u60d /v2/api/public/status → Tailscale       ┐ every 10 s
//!                               u60d /v2/api/alerts       → bar              ┘ (and right after "Got it")
//!  each tab's stream: render(ctx) → leaves by id → only the changed ones are sent
//!  POST /alerts/read?seq ─▶ u60d /api/alerts/read (agent.rs) ─▶ refetch ─▶ bar
//! ```
//! Where the browser came from ([`Via`], docs/designs/remote-path-guard.md) is decided here from
//! its address, the Host it typed, the device's LAN interfaces and `ip route get` (a viewer
//! behind another node's subnet route answers over `tailscale0`, R18-P0-04).

use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use askama::Template;
use axum::extract::{ConnectInfo, Query, Request, State};
use axum::http::{HeaderMap, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;

use crate::hub::{Change, Hub, St};
use crate::i18n::Lang;
use crate::pages::Shared;

/// Status icons drawn filled when on (the sprite carries their fill symbols too).
pub const FILLED: [&str; 4] = ["WifiHigh", "Cloud", "Waves", "ChatCircleText"];

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
    /// A tailnet address, or a tailnet name / address in Host.
    Tailnet,
    /// Not on the LAN, and the device answers it over `tailscale0`: another node's subnet route.
    SubnetRoute,
    /// Neither (or no peer address): could be anything, so it is warned like a remote one.
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
            Via::Tailnet => l.pick("Remote via Tailscale", "经 Tailscale 远程访问"),
            Via::SubnetRoute => l.pick("Remote via a subnet route", "经子网路由远程访问"),
            Via::Unknown => l.pick("Unknown source", "来源不明"),
        }
    }

    /// The badge's title: which path, and what can cut it.
    pub fn why(self, l: impl std::borrow::Borrow<Lang>) -> &'static str {
        let l = *l.borrow();
        match self {
            Via::Local => "",
            Via::Tailnet => l.pick(
                "Connected over Tailscale, so changing the network, the bands or Tailscale can cut you off",
                "你正经 Tailscale 连到设备；改网络、锁频、关 Tailscale 可能让你断开",
            ),
            Via::SubnetRoute => l.pick(
                "Connected through another Tailscale node's subnet route, so changing the network, the LAN address or Tailscale can cut you off",
                "你经别的 Tailscale 节点的子网路由连到设备；改网络、改局域网地址、关 Tailscale 可能让你断开",
            ),
            Via::Unknown => l.pick(
                "Not from the device's LAN and not over Tailscale, so it is treated as remote: changing the network can cut you off",
                "认不出你从哪条路连进来（不在设备局域网，也不是 Tailscale），按远程处理：改网络可能让你断开",
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
            Via::Tailnet => l.pick("You are connected remotely over Tailscale.", "你正在经 Tailscale 远程操作。"),
            Via::SubnetRoute => l.pick(
                "You are connected remotely through a Tailscale subnet route.",
                "你正在经 Tailscale 子网路由远程操作。",
            ),
            Via::Unknown => l.pick(
                "Your connection is not from the device's LAN and not over Tailscale; it is treated as remote.",
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

fn tailnet_ip(ip: IpAddr) -> bool {
    match plain(ip) {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            o[0] == 100 && (o[1] & 0xc0) == 64
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0
        }
    }
}

/// The host part of a Host header: `[v6]:port`, `v4:port`, `name:port`, or no port.
fn host_part(host: &str) -> &str {
    if let Some(rest) = host.strip_prefix('[') {
        return rest.split(']').next().unwrap_or("");
    }
    match host.rsplit_once(':') {
        Some((h, port)) if port.bytes().all(|b| b.is_ascii_digit()) => h,
        _ => host,
    }
}

/// Host names a tailnet address or MagicDNS name (`*.ts.net`, or a single-label name other than
/// localhost).
fn tailnet_host(h: &HeaderMap) -> bool {
    let Some(host) = h.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let host = host_part(host.trim())
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if let Ok(ip) = host.split('%').next().unwrap_or("").parse::<IpAddr>() {
        return tailnet_ip(ip);
    }
    if host.is_empty() || host == "localhost" {
        return false;
    }
    host.ends_with(".ts.net") || !host.contains('.')
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

/// Interfaces that are not the LAN: loopback, Tailscale, the cellular data path, CHILL's tun.
fn not_lan(name: &str) -> bool {
    name == "lo" || ["tailscale", "rmnet", "chill"].iter().any(|p| name.starts_with(p))
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
            if not_lan(name) || !(fam == "inet" || fam == "inet6") {
                return None;
            }
            Some(Net(a.parse().ok()?, len.parse().ok()?))
        })
        .collect()
}

/// `ip route get` answered over `tailscale0`.
pub fn via_tailscale(ip_route: &str) -> bool {
    ip_route
        .split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .any(|w| w[0] == "dev" && w[1].starts_with("tailscale"))
}

/// The classification itself (the table in remote-path-guard.md), with the device reads passed in.
pub fn classify(
    h: &HeaderMap,
    peer: Option<IpAddr>,
    lan: &[Net],
    tailscale_route: impl FnOnce(IpAddr) -> bool,
) -> Via {
    if peer.is_some_and(tailnet_ip) || tailnet_host(h) {
        return Via::Tailnet;
    }
    let Some(peer) = peer.map(plain) else {
        return Via::Unknown;
    };
    if peer.is_loopback() || lan.iter().any(|n| n.contains(peer)) {
        return Via::Local;
    }
    if tailscale_route(peer) {
        Via::SubnetRoute
    } else {
        Via::Unknown
    }
}

const LAN_TTL: Duration = Duration::from_secs(30);
const ROUTE_TTL: Duration = Duration::from_secs(60);

/// `ip`'s output; empty when it failed. None = there is no `ip` at all: not the device (a Mac, the
/// browser-check container), where every viewer is local as before.
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
                    eprintln!("[luyaz-web] no `ip`: every viewer off the tailnet counts as local")
                }),
                Some(n) if n.is_empty() => SAID.call_once(|| {
                    eprintln!("[luyaz-web] `ip -o addr` gave no LAN network: viewers off the tailnet read as unknown")
                }),
                Some(_) => {}
            }
            *c = Some((Instant::now(), nets.clone()));
            nets
        }
    }
}

fn cached_route(peer: IpAddr) -> bool {
    static C: Mutex<Option<HashMap<IpAddr, (Instant, bool)>>> = Mutex::new(None);
    let mut c = C.lock().unwrap_or_else(|e| e.into_inner());
    let m = c.get_or_insert_with(HashMap::new);
    if let Some((_, ts)) = m.get(&peer).filter(|(at, _)| at.elapsed() < ROUTE_TTL) {
        return *ts;
    }
    let ts = via_tailscale(&run_ip(&["route", "get", &peer.to_string()]).unwrap_or_default());
    if m.len() > 256 {
        m.retain(|_, (at, _)| at.elapsed() < ROUTE_TTL);
    }
    m.insert(peer, (Instant::now(), ts));
    ts
}

/// Where this request came from, read off the device (interfaces cached 30 s, routes 60 s per peer).
pub fn via(h: &HeaderMap, peer: Option<IpAddr>) -> Via {
    // the cheap answers first: no `ip` runs for a tailnet peer or name
    if peer.is_some_and(tailnet_ip) || tailnet_host(h) {
        return Via::Tailnet;
    }
    if peer.map(plain).is_some_and(|p| p.is_loopback()) {
        return Via::Local;
    }
    match cached_lan() {
        Some(lan) => classify(h, peer, &lan, cached_route),
        None => Via::Local,
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

/// The vendor's `connect_status` values: `connected`, `ipv4_connected`, `ipv4_ipv6_connected`, …
/// and `disconnected`, `connecting`, … (a substring test would take `disconnected` as connected).
pub fn connected(status: &str) -> bool {
    status == "connected" || status.ends_with("_connected")
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
    let up = cell
        .and_then(|c| c["connect_status"].as_str())
        .is_some_and(connected);
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
    let mut label = if up {
        t("Mobile data on", "移动数据已通")
    } else {
        t("Mobile data down", "移动数据断了")
    };
    if up && !ty.is_empty() {
        label = format!("{label} · {ty}");
    }
    if up && weak {
        label = format!("{label} · {}", l.pick("weak signal", "信号弱"));
    }
    out.push(Icon {
        id: "st-net",
        href: "/signal",
        icon: "CellSignalFull",
        tone: match (up, weak) {
            (false, _) => Tone::Bad,
            (true, true) => Tone::Warn,
            (true, false) => Tone::Ok,
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
    let ts = st.side.agent.as_ref().map(|a| &a["services"]["tailscale"]);
    let ts_tone = match ts {
        Some(t) if t["running"].as_bool() == Some(true) => Tone::Ok,
        Some(t) if t["installed"].as_bool() == Some(true) => Tone::Warn,
        _ => Tone::Off,
    };
    out.push(Icon {
        id: "st-ts",
        href: "/services/tailscale",
        icon: "Cloud",
        tone: ts_tone,
        label: svc("Tailscale", ts_tone),
        badge: 0,
        known: ts.is_some(),
    });

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
    let text = latest.and_then(|e| e["text"].as_str()).unwrap_or("");
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
            format!("{n} · {}", self.label)
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
        // Tailscale is in u60d's public status too (8d-2: the agent's is gone)
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
    use axum::http::HeaderValue;

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
            "interfaces": {"cellular": {"connect_status": "ipv4_ipv6_connected"}},
            "wlan": {"enabled": 0},
            "sms": {"unread": 12}
        }));
        let ic = hub.read(|st| icons(st, Lang::Zh));
        let net = &ic[0];
        assert_eq!(net.tone, Tone::Warn, "SA reads nr_rsrp, -112 is weak");
        assert_eq!(net.label, "移动数据已通 · SA · 信号弱");
        assert_eq!(ic[1].tone, Tone::Off);
        assert_eq!(ic[4].badge_text(), "9+");
        assert!(!ic[2].known && !ic[3].known, "no side data yet");
        let html = hub.read(|st| topbar_html(st, &ctx("/signal")));
        assert!(html.contains("id=\"st-ts\"") && html.contains("id=\"st-chill\""));
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
        assert_eq!(b.title(Lang::Zh), "2 条新告警 · Wi-Fi 掉了");
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
            s.agent = Some(json!({"services": {"tailscale": {"installed": true, "running": true}}}))
        });
        let b = hub.read(|st| leaves(st, &c, &mut crate::opbar::Seen::default()));
        let out = diff(&a, &b);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].contains("id=\"st-ts\"") && out[0].contains("Tailscale 运行中"));
        // same data again: no change event, nothing to send
        let g = hub.read(|st| st.generation);
        hub.set_side(|s| {
            s.agent = Some(json!({"services": {"tailscale": {"installed": true, "running": true}}}))
        });
        assert_eq!(hub.read(|st| st.generation), g);
    }

    #[test]
    fn via_by_peer_host_lan_and_route() {
        let h = |host: &str| {
            let mut m = HeaderMap::new();
            m.insert(header::HOST, HeaderValue::from_str(host).unwrap());
            m
        };
        let ip = |s: &str| Some(s.parse::<IpAddr>().unwrap());
        // the device's `ip -o addr show` (10-19), addresses swapped for documentation ones
        let lan = lan_nets(
            "1: lo    inet 127.0.0.1/8 scope host lo\\       valid_lft forever preferred_lft forever\n\
             16: br-lan    inet 192.168.0.1/24 brd 192.168.0.255 scope global br-lan\\       valid_lft forever\n\
             16: br-lan    inet6 fd00:66::1/64 scope global \\       valid_lft forever\n\
             17: rmnet_data0    inet 192.0.2.121/30 scope global rmnet_data0\\       valid_lft forever\n\
             17: rmnet_data0    inet6 2001:db8::5a1d/64 scope global \\       valid_lft forever\n\
             25: tailscale0    inet 100.100.1.1/32 scope global tailscale0\\       valid_lft forever\n\
             25: tailscale0    inet6 fd7a:115c:a1e0::1/128 scope global \\       valid_lft forever\n\
             62: chill0    inet 198.18.0.1/30 brd 198.18.0.3 scope global chill0\\       valid_lft forever",
        );
        assert_eq!(lan.len(), 2, "lo, rmnet, tailscale and chill are not the LAN: {lan:?}");
        let c = |host: &str, peer: Option<IpAddr>, ts: bool| classify(&h(host), peer, &lan, |_| ts);
        assert_eq!(c("192.168.0.1", ip("100.101.1.2"), false), Via::Tailnet);
        assert_eq!(c("100.70.0.1:8080", None, false), Via::Tailnet);
        assert_eq!(c("[fd7a:115c:a1e0::1]:80", None, false), Via::Tailnet);
        assert_eq!(c("u60.tail1234.ts.net", None, false), Via::Tailnet);
        assert_eq!(c("u60", None, false), Via::Tailnet);
        assert_eq!(c("192.168.0.1:9090", ip("192.168.0.23"), true), Via::Local);
        assert_eq!(c("192.168.0.1:9090", ip("::ffff:192.168.0.23"), false), Via::Local);
        assert_eq!(c("[fd00:66::1]", ip("fd00:66::9"), false), Via::Local);
        assert_eq!(c("localhost:8080", ip("127.0.0.1"), false), Via::Local);
        // the bug (10-19): a computer behind another node's subnet route, Host = the LAN address
        assert_eq!(c("192.168.0.1:9090", ip("198.51.100.88"), true), Via::SubnetRoute);
        assert!(Via::SubnetRoute.remote());
        assert_eq!(c("192.168.0.1:9090", ip("198.51.100.88"), false), Via::Unknown);
        assert_eq!(c("192.168.0.1:9090", ip("192.0.2.122"), false), Via::Unknown, "the WAN side");
        assert_eq!(c("192.168.0.1:9090", None, false), Via::Unknown, "no peer: warn");
        assert_eq!(c("100.128.0.1", ip("192.0.2.66"), false), Via::Unknown, "outside 100.64/10");
        assert!(!Via::Local.remote() && Via::Unknown.remote());
    }

    #[test]
    fn route_parse() {
        // the device's `ip route get` (10-19), documentation addresses
        assert!(via_tailscale("198.51.100.88 dev tailscale0 table 52 src 100.100.1.1 uid 0 \n    cache "));
        assert!(!via_tailscale("192.168.0.23 dev br-lan src 192.168.0.1 uid 0 \n    cache "));
        assert!(!via_tailscale(""));
    }

    #[test]
    fn warn_says_whether_the_step_reverts() {
        use crate::write::Revert;
        let w = |r| Via::SubnetRoute.warn(Lang::Zh, r);
        assert!(w(Revert::On).contains("自动退回；"), "{}", w(Revert::On));
        assert!(w(Revert::Off).contains("自动退回没开"));
        assert!(w(Revert::No).contains("不会自动退回"));
        assert!(Via::Local.warn(Lang::Zh, Revert::No).is_empty());
        assert_eq!(Revert::txn(None), Revert::No);
        assert_eq!(Revert::txn(Some(&serde_json::json!({"rollback_enabled": false}))), Revert::Off);
        assert_eq!(Revert::txn(Some(&serde_json::json!({"rollback_enabled": true}))), Revert::On);
    }
}
