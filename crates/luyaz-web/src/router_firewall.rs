//! /router/firewall (防火墙), the old `router/firewall/page.tsx`: the firewall switch and level,
//! NAT, UPnP, the port-forward switch, DMZ, the port-forward rules (add / delete) and the
//! read-only filter rules. Batch 4 (u60-web.md).
//!
//! Where each value comes from (the old page read four agent passthroughs every 30–60 s):
//! - u60d's `router` block: `firewall{enabled, level, nat, dmz, dmz_ip, port_forward, wan_ping,
//!   remote_web}` (`router_get_firewall_para`), `upnp{enabled, natpmp, notify_interval, ttl}`
//!   (`router_get_upnp`), `port_forward[]` and `filter_rules[]` (`{}` from the firmware = []).
//!   A null part = the firmware did not say or the read failed: "—", never "off".
//! - u60d's `dhcp` block (`ip`, `netmask`): the LAN subnet a DMZ host or a forward target must
//!   be in, and the address the recovery text names. No ip or netmask: those writes are refused
//!   (user decision 10-13: 「还没读到局域网网段，稍后再试」); turning DMZ off still sends the
//!   current host, like the old page.
//! - Not shown any more: the iOS-only `wan_ping_filter` (u60d has no such field).
//!
//! Writes (controls-inventory.md §/router/firewall, user decision for NAT off and DMZ), all
//! `vendor.call` `zwrt_router.api` with the agent's bodies (it passed the page's JSON through):
//! - tier 2, tier 3 over Tailscale: firewall `router_set_firewall_switch {firewall_switch}`, level
//!   `router_set_firewall_level {firewall_level}`, NAT on `router_set_nat_switch {nat_switch}`, UPnP
//!   `router_set_upnp_switch {enable_upnp, notify_interval, ttl, natpmp}` (the numbers sent back as
//!   read; a null one left out), port forwarding `router_set_portforward_switch
//!   {port_forward_switch}`, add / delete rule `router_set_portforward {action:"add", name,
//!   protocol, wan_port, lan_ip, lan_port, enabled}` / `{action:"delete", id}`;
//! - tier 3 everywhere: NAT off, and any DMZ change (`router_set_dmz {dmz_enabled, dmz_ip}`).
//!
//! Plain writes: read back from the `router` block (u60d drops its cache after a write). The two
//! forms (DMZ, a new rule) come as patches when opened; their fields are page-local signals posted
//! to the ask, which checks them and keeps them under a random token for the confirm (user text
//! never goes into a URL or a JS string).

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::Ipv4Addr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

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
use crate::agent::Polled;
use crate::hub::{Link, St};
use crate::i18n::Lang;
use crate::pages::{Shared, StreamQuery, split_leaves};
use crate::write;

pub const PATH: &str = "/router/firewall";
const OBJECT: &str = "zwrt_router.api";
pub const LEVELS: [&str; 3] = ["low", "medium", "high"];
pub const PROTOCOLS: [&str; 3] = ["TCP", "UDP", "Both"];

/// This page's routes (the page, its stream, its buttons).
pub fn routes() -> Router<Shared> {
    Router::new()
        .route(PATH, get(page))
        .route("/router/firewall/events", get(events))
        .route("/router/firewall/refresh", post(refresh))
        .route("/router/firewall/sw/ask", post(sw_ask))
        .route("/router/firewall/sw/set", post(sw_set))
        .route("/router/firewall/sw/cancel", post(sw_cancel))
        .route("/router/firewall/dmz/form", post(dmz_form))
        .route("/router/firewall/dmz/close", post(dmz_close))
        .route("/router/firewall/dmz/ask", post(dmz_ask))
        .route("/router/firewall/dmz/set", post(dmz_set))
        .route("/router/firewall/dmz/cancel", post(dmz_cancel))
        .route("/router/firewall/pf/form", post(pf_form))
        .route("/router/firewall/pf/close", post(pf_close))
        .route("/router/firewall/pf/ask", post(pf_ask))
        .route("/router/firewall/pf/set", post(pf_set))
        .route("/router/firewall/pf/cancel", post(pf_cancel))
        .route("/router/firewall/del/ask", post(del_ask))
        .route("/router/firewall/del/set", post(del_set))
        .route("/router/firewall/del/cancel", post(del_cancel))
}

// ---- what u60d says ----

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Fw {
    pub enabled: Option<bool>,
    /// None: not one of low / medium / high ("" on B31).
    pub level: Option<&'static str>,
    pub level_raw: String,
    pub nat: Option<bool>,
    pub dmz: Option<bool>,
    pub dmz_ip: String,
    pub pf: Option<bool>,
    pub wan_ping: Option<bool>,
    pub remote_web: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Upnp {
    pub enabled: Option<bool>,
    pub natpmp: Option<i64>,
    pub notify_interval: Option<i64>,
    pub ttl: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rule {
    pub id: String,
    pub name: String,
    pub protocol: String,
    pub wan_port: String,
    pub lan_ip: String,
    pub lan_port: String,
    pub enabled: Option<bool>,
}

impl Rule {
    fn same(&self, o: &Rule) -> bool {
        self.name == o.name
            && self.wan_port == o.wan_port
            && self.lan_ip == o.lan_ip
            && self.lan_port == o.lan_port
    }
    /// The id can go into a URL as it is (it is a firmware value).
    pub fn deletable(&self) -> bool {
        !self.id.is_empty()
            && self.id.len() <= 64
            && self
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    }
    pub fn label(&self) -> &str {
        if self.name.is_empty() {
            &self.id
        } else {
            &self.name
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filter {
    pub name: String,
    pub protocol: String,
    pub src_mac: String,
    pub src_ip: String,
    pub src_port: String,
    pub dst_ip: String,
    pub dst_port: String,
    pub action: String,
    pub enabled: Option<bool>,
}

/// The LAN from the `dhcp` block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lan {
    pub ip: Ipv4Addr,
    /// None: u60d gave no (or no usable) netmask.
    pub mask: Option<Ipv4Addr>,
}

impl Lan {
    fn prefix(&self) -> Option<u32> {
        let m = u32::from(self.mask?);
        (m.leading_ones() + m.trailing_zeros() == 32).then(|| m.leading_ones())
    }
    pub fn net(&self) -> Option<(Ipv4Addr, Ipv4Addr, u32)> {
        let p = self.prefix().filter(|p| (8..=30).contains(p))?;
        let m = u32::from(self.mask?);
        let n = u32::from(self.ip) & m;
        Some((Ipv4Addr::from(n), Ipv4Addr::from(n | !m), p))
    }
    /// "192.168.0.0/24" (or the U60's address when the subnet is unknown).
    pub fn text(&self) -> String {
        match self.net() {
            Some((n, _, p)) => format!("{n}/{p}"),
            None => self.ip.to_string(),
        }
    }
    /// Placeholder for a host field: "192.168.0.x" for a /24, else the network.
    pub fn hint(&self) -> String {
        match self.net() {
            Some((_, _, 24)) => {
                let o = self.ip.octets();
                format!("{}.{}.{}.x", o[0], o[1], o[2])
            }
            Some((n, _, _)) => n.to_string(),
            None => "192.168.0.x".into(),
        }
    }
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_owned(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// The `router` block's data and whether it is current.
fn router(st: &St) -> Option<(&Value, bool)> {
    st.blocks
        .get("router")
        .map(|b| (&b.data, b.stale || st.link != Link::Live))
}

pub fn firewall(st: &St) -> Option<Fw> {
    let f = &router(st)?.0["firewall"];
    if !f.is_object() {
        return None;
    }
    let level_raw = text(&f["level"]);
    Some(Fw {
        enabled: f["enabled"].as_bool(),
        level: LEVELS.iter().find(|l| **l == level_raw).copied(),
        level_raw,
        nat: f["nat"].as_bool(),
        dmz: f["dmz"].as_bool(),
        dmz_ip: text(&f["dmz_ip"]),
        pf: f["port_forward"].as_bool(),
        wan_ping: f["wan_ping"].as_bool(),
        remote_web: f["remote_web"].as_bool(),
    })
}

pub fn upnp(st: &St) -> Option<Upnp> {
    let u = &router(st)?.0["upnp"];
    u.is_object().then(|| Upnp {
        enabled: u["enabled"].as_bool(),
        natpmp: u["natpmp"].as_i64(),
        notify_interval: u["notify_interval"].as_i64(),
        ttl: u["ttl"].as_i64(),
    })
}

pub fn rules(st: &St) -> Option<Vec<Rule>> {
    let a = router(st)?.0["port_forward"].as_array()?;
    Some(
        a.iter()
            .map(|r| Rule {
                id: text(&r["id"]),
                name: text(&r["name"]),
                protocol: text(&r["protocol"]),
                wan_port: text(&r["wan_port"]),
                lan_ip: text(&r["lan_ip"]),
                lan_port: text(&r["lan_port"]),
                enabled: r["enabled"].as_bool(),
            })
            .collect(),
    )
}

pub fn filters(st: &St) -> Option<Vec<Filter>> {
    let a = router(st)?.0["filter_rules"].as_array()?;
    Some(
        a.iter()
            .map(|r| Filter {
                name: text(&r["name"]),
                protocol: text(&r["protocol"]),
                src_mac: text(&r["src_mac"]),
                src_ip: text(&r["src_ip"]),
                src_port: text(&r["src_port"]),
                dst_ip: text(&r["dst_ip"]),
                dst_port: text(&r["dst_port"]),
                action: text(&r["action"]),
                enabled: r["enabled"].as_bool(),
            })
            .collect(),
    )
}

pub fn lan(st: &St) -> Option<Lan> {
    let d = &st.blocks.get("dhcp")?.data;
    Some(Lan {
        ip: text(&d["ip"]).parse().ok()?,
        mask: text(&d["netmask"]).parse().ok(),
    })
}

/// The router parts are readable and current (a write needs both).
fn fresh(st: &St) -> bool {
    router(st).is_some_and(|(_, s)| !s)
}

// ---- checks ----

/// The LAN subnet, or "not read yet" (no `dhcp` ip / netmask, or no usable mask): a DMZ host or a
/// forward target is never sent unchecked (user decision 10-13).
pub fn subnet(st: &St, l: Lang) -> Result<Lan, write::Outcome> {
    lan(st).filter(|n| n.net().is_some()).ok_or_else(|| {
        write::Outcome::NotYet(
            l.pick(
                "LAN subnet not read yet, try again shortly",
                "还没读到局域网网段，稍后再试",
            )
            .into(),
        )
    })
}

/// A LAN host for DMZ / a forward: an IPv4 address in the LAN subnet, not the U60 itself, not the
/// network or broadcast address. (The writes call [`subnet`] first, so an unknown subnet never gets here.)
pub fn host_check(s: &str, lan: Option<Lan>, l: Lang) -> Result<Ipv4Addr, String> {
    let ip: Ipv4Addr = s.trim().parse().map_err(|_| {
        l.pick(
            "Not an IPv4 address (like 192.168.0.20)",
            "不是 IPv4 地址（例如 192.168.0.20）",
        )
        .to_string()
    })?;
    let Some(lan) = lan else { return Ok(ip) };
    if ip == lan.ip {
        return Err(l
            .pick("That is the U60's own address", "这是 U60 自己的地址")
            .into());
    }
    if let Some((n, b, _)) = lan.net() {
        let m = u32::from(lan.mask.unwrap_or(Ipv4Addr::BROADCAST));
        if u32::from(ip) & m != u32::from(n) {
            return Err(match l {
                Lang::En => format!("Not on the U60's LAN ({})", lan.text()),
                Lang::Zh => format!("不在 U60 的局域网里（{}）", lan.text()),
            });
        }
        if ip == n || ip == b {
            return Err(l
                .pick(
                    "That is the subnet's network or broadcast address",
                    "这是网段的网络地址或广播地址",
                )
                .into());
        }
    }
    Ok(ip)
}

/// 1–65535, or a range "8000-8010" (the old page's `isPort`).
pub fn port_ok(s: &str) -> bool {
    let ok = |p: &str| {
        !p.is_empty()
            && p.len() <= 5
            && p.bytes().all(|b| b.is_ascii_digit())
            && p.parse::<u32>().is_ok_and(|n| (1..=65535).contains(&n))
    };
    match s.split_once('-') {
        Some((a, b)) => ok(a) && ok(b),
        None => ok(s),
    }
}

// ---- the view ----

pub struct Ro {
    pub label: &'static str,
    pub value: &'static str,
}

pub struct View {
    pub l: Lang,
    pub tone: &'static str,
    pub state: String,
    pub reason: Option<String>,
    pub meta: Option<&'static str>,
    pub retry: bool,
    /// The firewall part (None: loading or unreadable).
    pub fw: Option<Fw>,
    pub stale: bool,
    pub dis: bool,
    pub upnp: Option<bool>,
    pub upnp_sub: Option<&'static str>,
    pub levels: Vec<(&'static str, &'static str, bool)>,
    pub level_text: String,
    pub ro: Vec<Ro>,
    pub dmz: String,
    pub rules: Option<Vec<Rule>>,
    pub filters: Option<Vec<Filter>>,
}

impl View {
    pub fn symbol(&self) -> &'static str {
        match self.tone {
            "bad" => "■",
            "warn" => "▲",
            _ => "●",
        }
    }
    /// The level buttons are off: no current settings, or a level that is none of the three.
    pub fn level_dis(&self) -> bool {
        self.dis || self.fw.as_ref().is_none_or(|f| f.level.is_none())
    }
    pub fn sw(&self, k: &str) -> Option<bool> {
        let f = self.fw.as_ref();
        match k {
            "fw" => f.and_then(|f| f.enabled),
            "nat" => f.and_then(|f| f.nat),
            "pf" => f.and_then(|f| f.pf),
            _ => self.upnp,
        }
    }
    /// (key, label) of the four switches, in page order.
    pub fn switches(&self) -> [(&'static str, &'static str); 4] {
        [
            ("fw", self.l.pick("Firewall", "防火墙")),
            ("nat", "NAT"),
            ("upnp", "UPnP"),
            ("pf", self.l.pick("Port Forwarding", "端口转发")),
        ]
    }
    /// The delete button's name: 「删除规则「NAS HTTPS」」 (inventory).
    pub fn del_label(&self, r: &Rule) -> String {
        match self.l {
            Lang::En => format!("Delete rule “{}”", r.label()),
            Lang::Zh => format!("删除规则「{}」", r.label()),
        }
    }
    pub fn rule_sub(&self, r: &Rule) -> String {
        let d = |s: &str| {
            if s.is_empty() {
                "—".to_string()
            } else {
                s.to_string()
            }
        };
        let off = if r.enabled == Some(false) {
            self.l.pick(" · off", " · 关")
        } else {
            ""
        };
        format!(
            "{} · WAN {} → {}:{}{off}",
            d(&r.protocol),
            d(&r.wan_port),
            d(&r.lan_ip),
            d(&r.lan_port)
        )
    }
    pub fn filter_sub(&self, r: &Filter) -> String {
        let d = |s: &str| {
            if s.is_empty() {
                "—".to_string()
            } else {
                s.to_string()
            }
        };
        let mut p = vec![d(&r.protocol)];
        if !r.src_mac.is_empty() {
            p.push(format!("MAC {}", r.src_mac));
        }
        let src = if r.src_port.is_empty() {
            d(&r.src_ip)
        } else {
            format!("{}:{}", d(&r.src_ip), r.src_port)
        };
        p.push(format!("{} {src}", self.l.pick("from", "源")));
        p.push(format!(
            "{} {}:{}",
            self.l.pick("to", "目标"),
            d(&r.dst_ip),
            d(&r.dst_port)
        ));
        p.join(" · ")
    }
    pub fn filter_value(&self, r: &Filter) -> String {
        if !r.action.is_empty() {
            let on = match r.enabled {
                Some(false) => self.l.pick(" (off)", "（关）"),
                _ => "",
            };
            return format!("{}{on}", r.action);
        }
        on_off(self.l, r.enabled).into()
    }
}

fn on_off(l: Lang, v: Option<bool>) -> &'static str {
    match v {
        None => "—",
        Some(true) => l.pick("On", "开"),
        Some(false) => l.pick("Off", "关"),
    }
}

pub fn level_label(l: Lang, k: &str) -> &'static str {
    match k {
        "low" => l.pick("Low", "低"),
        "medium" => l.pick("Medium", "中"),
        _ => l.pick("High", "高"),
    }
}

pub fn view(st: &St, l: Lang) -> View {
    let fw = firewall(st);
    let up = upnp(st);
    let rb = router(st);
    let stale = rb.is_some_and(|(_, s)| s);
    let known = matches!(st.link, Link::Live | Link::Down);
    let (mut tone, state, reason, retry) = match (&fw, rb) {
        (None, Some(_)) => (
            "bad",
            l.pick("Can't read firewall settings", "读不到防火墙设置")
                .to_string(),
            Some(
                l.pick(
                    "The device didn't answer the firewall read.",
                    "设备没有回答防火墙设置的读取。",
                )
                .to_string(),
            ),
            true,
        ),
        (None, None) if known => (
            "bad",
            l.pick("Can't read firewall settings", "读不到防火墙设置")
                .to_string(),
            Some(
                match st.link {
                    Link::Down => l.pick("Can't reach the data service.", "连不上数据服务。"),
                    _ => l.pick(
                        "The data service has no router settings.",
                        "数据服务没有路由设置。",
                    ),
                }
                .to_string(),
            ),
            true,
        ),
        (None, None) => (
            "neutral",
            l.pick("Reading firewall settings…", "正在读取防火墙设置…")
                .to_string(),
            None,
            false,
        ),
        (Some(f), _) => match (f.enabled, f.dmz) {
            (None, _) => (
                "neutral",
                l.pick("Firewall state unknown", "防火墙状态不明")
                    .to_string(),
                Some(
                    l.pick(
                        "The device didn't say whether the firewall is on.",
                        "设备没有返回防火墙是否打开。",
                    )
                    .to_string(),
                ),
                false,
            ),
            (Some(false), _) => (
                "warn",
                l.pick("Firewall off", "防火墙已关闭").to_string(),
                Some(
                    l.pick(
                        "Traffic from the internet is not filtered.",
                        "来自互联网的流量不经过滤。",
                    )
                    .to_string(),
                ),
                false,
            ),
            (Some(true), Some(true)) => (
                "warn",
                l.pick(
                    "Firewall on · DMZ exposes a device",
                    "防火墙已开启 · DMZ 暴露了一台设备",
                )
                .to_string(),
                Some(match l {
                    Lang::En => format!(
                        "{} receives all unsolicited traffic from the internet.",
                        if f.dmz_ip.is_empty() {
                            "—"
                        } else {
                            &f.dmz_ip
                        }
                    ),
                    Lang::Zh => format!(
                        "{} 会收到所有来自互联网的主动连接。",
                        if f.dmz_ip.is_empty() {
                            "—"
                        } else {
                            &f.dmz_ip
                        }
                    ),
                }),
                false,
            ),
            (Some(true), _) => (
                "ok",
                l.pick("Firewall on", "防火墙已开启").to_string(),
                f.level
                    .map(|k| format!("{}{}", l.pick("Level: ", "级别："), level_label(l, k))),
                false,
            ),
        },
    };
    let meta = (fw.is_some() && stale).then(|| {
        tone = "stale";
        l.pick(
            "Settings not current — refresh before changing anything",
            "设置不是最新的，刷新后再改",
        )
    });
    let dis = fw.is_none() || stale;
    let level = fw.as_ref().and_then(|f| f.level);
    let level_text = match &fw {
        Some(f) if f.level.is_none() && !f.level_raw.is_empty() => f.level_raw.clone(),
        Some(f) => f.level.map_or("—", |k| level_label(l, k)).into(),
        None => "—".into(),
    };
    let upnp_sub = match (&up, rb) {
        (None, Some(_)) => {
            Some(l.pick("Couldn't read UPnP from the device.", "读不到 UPnP 设置。"))
        }
        (Some(u), _) if u.enabled.is_none() => {
            Some(l.pick("The device didn't report UPnP.", "设备没有返回 UPnP 状态。"))
        }
        _ => None,
    };
    let ro = match &fw {
        Some(f) => [
            (
                f.wan_ping,
                l.pick("Answer ping from the internet", "响应来自互联网的 ping"),
            ),
            (
                f.remote_web,
                l.pick("Remote web management", "远程网页管理"),
            ),
        ]
        .into_iter()
        .filter_map(|(v, label)| {
            v.map(|v| Ro {
                label,
                value: on_off(l, Some(v)),
            })
        })
        .collect(),
        None => Vec::new(),
    };
    let dmz = match &fw {
        None => "—".into(),
        Some(f) => match f.dmz {
            None => "—".into(),
            Some(false) => l.pick("Off", "已关闭").into(),
            Some(true) => format!(
                "{} · {}",
                l.pick("On", "已启用"),
                if f.dmz_ip.is_empty() {
                    "—"
                } else {
                    &f.dmz_ip
                }
            ),
        },
    };
    View {
        l,
        tone,
        state,
        reason,
        meta,
        retry,
        stale,
        dis,
        upnp: up.and_then(|u| u.enabled),
        upnp_sub,
        levels: LEVELS
            .iter()
            .map(|k| (*k, level_label(l, k), level == Some(*k)))
            .collect(),
        level_text,
        ro,
        dmz,
        fw,
        rules: rules(st),
        filters: filters(st),
    }
}

#[derive(Template)]
#[template(path = "router_firewall_leaves.html")]
struct Leaves<'a> {
    v: &'a View,
}

fn leaves(app: &Shared, l: Lang) -> Vec<(String, String)> {
    app.hub
        .read(|st| split_leaves(&Leaves { v: &view(st, l) }.render().unwrap()))
}

// ---- writes ----

/// The switches and the level (one area, `fw-sw`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Sw {
    Fw(bool),
    Nat(bool),
    Upnp(bool),
    Pf(bool),
    Level(&'static str),
}

impl Sw {
    pub fn parse(k: &str, on: bool) -> Option<Sw> {
        Some(match k {
            "fw" => Sw::Fw(on),
            "nat" => Sw::Nat(on),
            "upnp" => Sw::Upnp(on),
            "pf" => Sw::Pf(on),
            _ => Sw::Level(LEVELS.iter().find(|l| **l == k)?),
        })
    }
    fn query(self) -> String {
        match self {
            Sw::Fw(on) => format!("k=fw&on={on}"),
            Sw::Nat(on) => format!("k=nat&on={on}"),
            Sw::Upnp(on) => format!("k=upnp&on={on}"),
            Sw::Pf(on) => format!("k=pf&on={on}"),
            Sw::Level(k) => format!("k={k}"),
        }
    }
    /// The current value, or None when it can't be changed now.
    fn current(self, st: &St) -> Option<bool> {
        let f = firewall(st)?;
        match self {
            Sw::Fw(_) => f.enabled,
            Sw::Nat(_) => f.nat,
            Sw::Pf(_) => f.pf,
            Sw::Upnp(_) => upnp(st)?.enabled,
            Sw::Level(k) => Some(f.level == Some(k)),
        }
    }
    fn name(self, l: Lang) -> &'static str {
        match self {
            Sw::Fw(_) => l.pick("the firewall", "防火墙"),
            Sw::Nat(_) => "NAT",
            Sw::Upnp(_) => "UPnP",
            Sw::Pf(_) => l.pick("port forwarding", "端口转发"),
            Sw::Level(_) => "",
        }
    }
    pub fn args(self, up: Option<&Upnp>) -> (&'static str, Value) {
        let b = |on: bool| if on { "1" } else { "0" };
        match self {
            Sw::Fw(on) => (
                "router_set_firewall_switch",
                json!({"firewall_switch": b(on)}),
            ),
            Sw::Nat(on) => ("router_set_nat_switch", json!({"nat_switch": b(on)})),
            Sw::Pf(on) => (
                "router_set_portforward_switch",
                json!({"port_forward_switch": b(on)}),
            ),
            Sw::Level(k) => ("router_set_firewall_level", json!({"firewall_level": k})),
            Sw::Upnp(on) => {
                let mut a = json!({"enable_upnp": i64::from(on)});
                if let Some(u) = up {
                    for (k, v) in [
                        ("notify_interval", u.notify_interval),
                        ("ttl", u.ttl),
                        ("natpmp", u.natpmp),
                    ] {
                        if let Some(v) = v {
                            a[k] = json!(v);
                        }
                    }
                }
                ("router_set_upnp_switch", a)
            }
        }
    }
    fn holds(self, st: &St) -> bool {
        let f = firewall(st);
        match self {
            Sw::Fw(on) => f.and_then(|f| f.enabled) == Some(on),
            Sw::Nat(on) => f.and_then(|f| f.nat) == Some(on),
            Sw::Pf(on) => f.and_then(|f| f.pf) == Some(on),
            Sw::Level(k) => f.and_then(|f| f.level) == Some(k),
            Sw::Upnp(on) => upnp(st).and_then(|u| u.enabled) == Some(on),
        }
    }
    fn applied(self, l: Lang) -> String {
        match self {
            Sw::Level(k) => format!(
                "{}{}",
                l.pick("Firewall level set to ", "防火墙级别已设为"),
                level_label(l, k)
            ),
            Sw::Fw(on) | Sw::Nat(on) | Sw::Upnp(on) | Sw::Pf(on) => match (l, on) {
                (Lang::En, true) => format!("Turned {} on", self.name(l)),
                (Lang::En, false) => format!("Turned {} off", self.name(l)),
                (Lang::Zh, true) => format!("已打开{}", self.name(l)),
                (Lang::Zh, false) => format!("已关掉{}", self.name(l)),
            },
        }
    }
    fn pending(self, l: Lang) -> String {
        match self {
            Sw::Level(k) => format!(
                "{}{}…",
                l.pick("Setting the level to ", "正在把级别设为"),
                level_label(l, k)
            ),
            Sw::Fw(on) | Sw::Nat(on) | Sw::Upnp(on) | Sw::Pf(on) => match (l, on) {
                (Lang::En, true) => format!("Turning {} on…", self.name(l)),
                (Lang::En, false) => format!("Turning {} off…", self.name(l)),
                (Lang::Zh, true) => format!("正在打开{}…", self.name(l)),
                (Lang::Zh, false) => format!("正在关掉{}…", self.name(l)),
            },
        }
    }
}

/// How to get back in when the change cut a remote browser off.
fn recovery(l: Lang, lan: Option<Lan>) -> String {
    match (l, lan) {
        (Lang::En, Some(n)) => format!("Join the U60's Wi-Fi or plug into its LAN, open this page at the U60's address ({}) and undo the change here.", n.ip),
        (Lang::Zh, Some(n)) => format!("连上 U60 的 Wi-Fi 或插网线到它的局域网口，用 U60 的地址（{}）打开这个页面，在这里改回来。", n.ip),
        (Lang::En, None) => "Join the U60's Wi-Fi or plug into its LAN, open this page at the U60's LAN address and undo the change here.".into(),
        (Lang::Zh, None) => "连上 U60 的 Wi-Fi 或插网线到它的局域网口，用 U60 的局域网地址打开这个页面，在这里改回来。".into(),
    }
}

/// Tier 2, or tier 3 when `dialog` (remote, or one the user put in tier 3).
fn confirm(
    dialog: bool,
    title: String,
    what: String,
    action: String,
    downtime: Option<String>,
    lan: Option<Lan>,
    l: Lang,
) -> write::Confirm {
    write::Confirm {
        tier: if dialog {
            write::Tier::Dialog
        } else {
            write::Tier::Inline
        },
        title,
        what,
        downtime: downtime.filter(|_| dialog),
        recovery: dialog.then(|| recovery(l, lan)),
        action,
        cuts_uplink: true,
        word: None,
        danger: false,
        revert: crate::write::Revert::No,
    }
}

pub fn sw_confirm(s: Sw, remote: Via, lan: Option<Lan>, l: Lang) -> write::Confirm {
    if let Sw::Level(k) = s {
        let lv = level_label(l, k);
        return match l {
            Lang::En => confirm(
                remote.remote(),
                format!("Set the firewall level to {lv}?"),
                format!(
                    "The firewall applies the {lv} rule set; some connections from outside may start or stop working."
                ),
                format!("Confirm: set the level to {lv}"),
                None,
                lan,
                l,
            ),
            Lang::Zh => confirm(
                remote.remote(),
                format!("把防火墙级别设为「{lv}」？"),
                format!("防火墙改用「{lv}」这一档规则，有些来自外部的连接可能开始或停止工作。"),
                format!("确认：设为「{lv}」"),
                None,
                lan,
                l,
            ),
        };
    }
    let (on, name) = match s {
        Sw::Fw(on) | Sw::Nat(on) | Sw::Upnp(on) | Sw::Pf(on) => (on, s.name(l)),
        Sw::Level(_) => unreachable!(),
    };
    let what = match (s, on) {
        (Sw::Fw(_), true) => l.pick("The firewall filters traffic from the internet again at the selected level.", "防火墙重新按所选级别过滤来自互联网的流量。"),
        (Sw::Fw(_), false) => l.pick("Traffic from the internet is no longer filtered. Devices on the LAN are more exposed until you turn it back on.", "来自互联网的流量不再过滤。在重新打开之前，局域网里的设备更容易受到攻击。"),
        (Sw::Nat(_), true) => l.pick("LAN devices share the mobile connection's address again.", "局域网设备重新共用移动网络的地址上网。"),
        (Sw::Nat(_), false) => l.pick("Devices on the U60 may lose internet: without address translation most setups stop working until NAT is back on.", "连着 U60 的设备可能会断网：没有地址转换，多数情况下要等 NAT 重新打开才能上网。"),
        (Sw::Upnp(_), true) => l.pick("Apps on the LAN can open ports to the internet by themselves.", "局域网里的应用可以自己向互联网开放端口。"),
        (Sw::Upnp(_), false) => l.pick("Apps can no longer open ports by themselves; games and calls may fall back to relays.", "应用不能再自己开放端口；游戏和通话可能改走中转。"),
        (_, true) => l.pick("The port forwarding rules below start forwarding traffic from the internet to LAN devices.", "下面的端口转发规则开始把来自互联网的流量转给局域网设备。"),
        (_, false) => l.pick("All port forwarding rules stop; services behind them become unreachable from outside.", "所有端口转发规则停止；它们后面的服务从外部访问不到了。"),
    };
    let nat_off = s == Sw::Nat(false);
    let (title, action) = match (l, on) {
        (Lang::En, true) => (
            format!("Turn {name} on?"),
            format!("Confirm: turn {name} on"),
        ),
        (Lang::En, false) => (
            format!("Turn {name} off?"),
            format!("Confirm: turn {name} off"),
        ),
        (Lang::Zh, true) => (format!("打开{name}？"), format!("确认：打开{name}")),
        (Lang::Zh, false) => (format!("关掉{name}？"), format!("确认：关掉{name}")),
    };
    let downtime = nat_off.then(|| {
        l.pick(
            "Devices on the U60 may have no internet until NAT is turned back on.",
            "在 NAT 重新打开之前，连着 U60 的设备可能都上不了网。",
        )
        .to_string()
    });
    confirm(
        remote.remote() || nat_off,
        title,
        what.into(),
        action,
        downtime,
        lan,
        l,
    )
}

fn not_read(l: Lang) -> write::Outcome {
    write::Outcome::NotYet(
        l.pick(
            "Firewall settings not read yet (or not current); try again shortly",
            "还没读到最新的防火墙设置，稍后再试",
        )
        .into(),
    )
}

pub fn vendor(
    method: &'static str,
    args: Value,
    readback: write::Readback,
    applied: String,
) -> write::Write {
    write::Write {
        action: "vendor.call",
        params: json!({"object": OBJECT, "method": method, "args": args}),
        readback: Some(readback),
        applied,
        timeout: write::READBACK,
        typed: None,
        request_timeout: None,
    }
}

pub fn sw_write(s: Sw, up: Option<&Upnp>, l: Lang) -> write::Write {
    let (method, args) = s.args(up);
    vendor(method, args, Box::new(move |st| s.holds(st)), s.applied(l))
}

// ---- DMZ ----

#[derive(Clone, Debug, PartialEq)]
pub struct Dmz {
    pub on: bool,
    pub ip: String,
}

pub fn dmz_check(on: bool, ip: &str, st: &St, l: Lang) -> Result<Dmz, write::Outcome> {
    if !fresh(st) || firewall(st).and_then(|f| f.dmz).is_none() {
        return Err(not_read(l));
    }
    let ip = ip.trim();
    if !on {
        // the old page sent what was in the field (the current host) with dmz_enabled "0"
        return Ok(Dmz {
            on,
            ip: ip.to_owned(),
        });
    }
    let n = subnet(st, l)?;
    match host_check(ip, Some(n), l) {
        Ok(a) => Ok(Dmz {
            on,
            ip: a.to_string(),
        }),
        Err(e) => Err(write::Outcome::NotYet(format!(
            "{}{e}",
            l.pick("DMZ host: ", "DMZ 主机：")
        ))),
    }
}

pub fn dmz_confirm(d: &Dmz, lan: Option<Lan>, l: Lang) -> write::Confirm {
    let ip = &d.ip;
    let downtime = Some(
        l.pick(
            "Devices on the U60 may lose internet. If they do, undo the change here.",
            "连着 U60 的设备可能会断网；如果断了，在这里改回来。",
        )
        .to_string(),
    );
    match (l, d.on) {
        (Lang::En, true) => confirm(
            true,
            format!("Expose {ip} through DMZ?"),
            format!(
                "Every connection from the internet that no other rule handles goes to {ip}, bypassing the firewall. Only do this for a device that protects itself."
            ),
            "Confirm: enable DMZ".into(),
            downtime,
            lan,
            l,
        ),
        (Lang::Zh, true) => confirm(
            true,
            format!("通过 DMZ 暴露 {ip}？"),
            format!(
                "没有被其他规则处理的所有来自互联网的连接都会转给 {ip}，绕过防火墙。只对能保护自己的设备这样做。"
            ),
            "确认：启用 DMZ".into(),
            downtime,
            lan,
            l,
        ),
        (Lang::En, false) => confirm(
            true,
            "Turn DMZ off?".into(),
            "The DMZ host stops receiving unsolicited traffic from the internet.".into(),
            "Confirm: turn DMZ off".into(),
            downtime,
            lan,
            l,
        ),
        (Lang::Zh, false) => confirm(
            true,
            "关闭 DMZ？".into(),
            "DMZ 主机不再收到来自互联网的主动连接。".into(),
            "确认：关闭 DMZ".into(),
            downtime,
            lan,
            l,
        ),
    }
}

pub fn dmz_write(d: &Dmz, l: Lang) -> write::Write {
    let (on, ip) = (d.on, d.ip.clone());
    vendor(
        "router_set_dmz",
        json!({"dmz_enabled": if on { "1" } else { "0" }, "dmz_ip": d.ip}),
        Box::new(move |st| {
            firewall(st).is_some_and(|f| f.dmz == Some(on) && (!on || f.dmz_ip == ip))
        }),
        if on {
            match l {
                Lang::En => format!("DMZ on: {}", d.ip),
                Lang::Zh => format!("已启用 DMZ：{}", d.ip),
            }
        } else {
            l.pick("DMZ off", "已关闭 DMZ").into()
        },
    )
}

// ---- port forwarding ----

/// The add form's signals.
#[derive(Deserialize, Clone, Debug, Default)]
pub struct PfIn {
    #[serde(default, rename = "_pn")]
    pub name: String,
    #[serde(default, rename = "_pp")]
    pub protocol: String,
    #[serde(default, rename = "_pw")]
    pub wan: String,
    #[serde(default, rename = "_pi")]
    pub ip: String,
    #[serde(default, rename = "_pl")]
    pub port: String,
    #[serde(default = "yes", rename = "_pe")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

pub fn pf_check(f: &PfIn, st: &St, l: Lang) -> Result<Rule, write::Outcome> {
    if !fresh(st) || rules(st).is_none() {
        return Err(write::Outcome::NotYet(l.pick("The rule list isn't read yet (or not current): a new rule could not be checked; try again shortly", "还没读到最新的规则列表，没法确认新规则；稍后再试").into()));
    }
    let bad = |s: &str| Err(write::Outcome::NotYet(s.into()));
    let r = Rule {
        id: String::new(),
        name: f.name.trim().into(),
        protocol: PROTOCOLS
            .iter()
            .find(|p| **p == f.protocol)
            .unwrap_or(&"TCP")
            .to_string(),
        wan_port: f.wan.trim().into(),
        lan_ip: f.ip.trim().into(),
        lan_port: f.port.trim().into(),
        enabled: Some(f.enabled),
    };
    if r.name.is_empty() || r.wan_port.is_empty() || r.lan_ip.is_empty() || r.lan_port.is_empty() {
        return bad(l.pick(
            "Fill in the name, both ports and the LAN IP",
            "名称、两个端口和 LAN IP 都要填",
        ));
    }
    if r.name.chars().count() > 32 || r.name.chars().any(char::is_control) {
        return bad(l.pick("The name is at most 32 characters", "名称最多 32 个字符"));
    }
    if !port_ok(&r.wan_port) || !port_ok(&r.lan_port) {
        return bad(l.pick(
            "Ports are numbers from 1 to 65535 (or a range like 8000-8010)",
            "端口是 1–65535 的数字（或像 8000-8010 这样的范围）",
        ));
    }
    let n = subnet(st, l)?;
    match host_check(&r.lan_ip, Some(n), l) {
        Ok(a) => Ok(Rule {
            lan_ip: a.to_string(),
            ..r
        }),
        Err(e) => bad(&format!("{}{e}", l.pick("LAN IP: ", "LAN IP："))),
    }
}

pub fn pf_confirm(r: &Rule, remote: Via, lan: Option<Lan>, l: Lang) -> write::Confirm {
    let (n, p, w, i, lp) = (&r.name, &r.protocol, &r.wan_port, &r.lan_ip, &r.lan_port);
    match l {
        Lang::En => confirm(
            remote.remote(),
            format!("Add port forwarding rule “{n}”?"),
            format!("Traffic from the internet to {p} port {w} goes to {i}:{lp}."),
            format!("Confirm: add rule {n}"),
            None,
            lan,
            l,
        ),
        Lang::Zh => confirm(
            remote.remote(),
            format!("添加端口转发规则「{n}」？"),
            format!("来自互联网、发往 {p} 端口 {w} 的流量会转给 {i}:{lp}。"),
            format!("确认：添加规则「{n}」"),
            None,
            lan,
            l,
        ),
    }
}

pub fn pf_write(r: &Rule, before: usize, l: Lang) -> write::Write {
    let want = r.clone();
    vendor(
        "router_set_portforward",
        json!({"action": "add", "name": r.name, "protocol": r.protocol, "wan_port": r.wan_port, "lan_ip": r.lan_ip, "lan_port": r.lan_port, "enabled": if r.enabled == Some(false) { "0" } else { "1" }}),
        Box::new(move |st| {
            rules(st).is_some_and(|v| v.iter().filter(|x| x.same(&want)).count() > before)
        }),
        match l {
            Lang::En => format!("Rule “{}” added", r.name),
            Lang::Zh => format!("已添加规则「{}」", r.name),
        },
    )
}

pub fn del_confirm(r: &Rule, remote: Via, lan: Option<Lan>, l: Lang) -> write::Confirm {
    let d = |s: &str| {
        if s.is_empty() {
            "—".to_string()
        } else {
            s.to_string()
        }
    };
    let (n, w, i) = (r.label(), d(&r.wan_port), d(&r.lan_ip));
    match l {
        Lang::En => confirm(
            remote.remote(),
            format!("Delete rule “{n}”?"),
            format!("Traffic to port {w} stops reaching {i}. To undo, add the rule again."),
            format!("Confirm: delete rule {n}"),
            None,
            lan,
            l,
        ),
        Lang::Zh => confirm(
            remote.remote(),
            format!("删除规则「{n}」？"),
            format!("发往端口 {w} 的流量不再转给 {i}。要恢复，重新添加这条规则。"),
            format!("确认：删除规则「{n}」"),
            None,
            lan,
            l,
        ),
    }
}

pub fn del_write(r: &Rule, l: Lang) -> write::Write {
    let gone = r.clone();
    vendor(
        "router_set_portforward",
        json!({"action": "delete", "id": r.id}),
        Box::new(move |st| {
            rules(st).is_some_and(|v| !v.iter().any(|x| x.id == gone.id && x.same(&gone)))
        }),
        match l {
            Lang::En => format!("Rule “{}” deleted", r.label()),
            Lang::Zh => format!("已删除规则「{}」", r.label()),
        },
    )
}

// ---- checked forms waiting for their confirm ----

#[derive(Clone, Debug)]
enum Held {
    Dmz(Dmz),
    Pf(Rule),
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
}

impl Polled for Poll {
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        // nothing of the agent's: the hub's changes re-render (`render`)
        (leaves(app, self.l), Duration::from_secs(3600))
    }
    fn watches(&self, block: &str) -> bool {
        matches!(block, "router" | "dhcp")
    }
    fn render(&mut self, app: &Shared) -> Option<Vec<(String, String)>> {
        Some(leaves(app, self.l))
    }
}

#[derive(Template)]
#[template(path = "router_firewall.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    leaves: Vec<(String, String)>,
    sw_op: String,
    dmz_op: String,
    pf_op: String,
    del_op: String,
}

impl Page {
    fn leaf(&self, id: &str) -> &str {
        self.leaves
            .iter()
            .find(|(k, _)| k == id)
            .map_or("", |(_, h)| h.as_str())
    }
}

/// The write areas: `fw-sw`, `fw-dmz`, `fw-pf` (add), `fw-del`.
fn area(id: &str, l: Lang, remote: Via, set: &str, phase: &write::Phase) -> String {
    let cancel = format!("{PATH}/{}/cancel", &id[3..]);
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
        leaves: {
            let mut v = leaves(&app, l);
            v.extend(app.hub.read(|st| {
                split_leaves(
                    &(form_leaves("dmz", false, st, l)
                        + "<!--leaf-->"
                        + &form_leaves("pf", false, st, l)),
                )
            }));
            v
        },
        sw_op: area("fw-sw", l, ctx.remote, "", &idle),
        dmz_op: area("fw-dmz", l, ctx.remote, "", &idle),
        pf_op: area("fw-pf", l, ctx.remote, "", &idle),
        del_op: area("fw-del", l, ctx.remote, "", &idle),
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
    let l = ctx.lang;
    crate::agent::stream(app, ctx, q.g, PATH, Poll { l })
}

/// Retry: reconnect to u60d now.
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

/// Run `w` (`None`: not sent, `o` instead) and answer with the area's result, after "正在…".
fn run(
    app: Shared,
    pending: String,
    done: impl FnOnce(write::Outcome) -> String + Send + 'static,
    w: Result<write::Write, write::Outcome>,
    l: Lang,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    write::respond(pending, async move {
        let o = match w {
            Err(o) => o,
            Ok(w) => write::execute(&app.http, &app.control_url, &app.hub, &w, l).await,
        };
        done(o)
    })
}

#[derive(Deserialize, Default)]
pub struct SwQuery {
    #[serde(default)]
    k: String,
    #[serde(default)]
    on: Option<bool>,
}

/// A switch (`k` = fw / nat / upnp / pf; the target is the other state) or a level (`k` = the
/// level): the confirm, or why not.
pub async fn sw_ask(
    State(app): State<Shared>,
    Query(q): Query<SwQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let (phase, set) = app.hub.read(|st| {
        let Some(probe) = Sw::parse(&q.k, true) else {
            return (write::Phase::Idle, String::new());
        };
        let cur = probe.current(st).filter(|_| fresh(st));
        let s = match (probe, cur) {
            (_, None) => return (write::Phase::Done(not_read(l)), String::new()),
            (Sw::Level(_), Some(true)) => return (write::Phase::Idle, String::new()),
            (Sw::Level(_), _) => probe,
            (_, Some(on)) => Sw::parse(&q.k, !on).unwrap(),
        };
        (
            write::Phase::Ask(sw_confirm(s, ctx.remote, lan(st), l)),
            format!("{PATH}/sw/set?{}", s.query()),
        )
    });
    one(area("fw-sw", l, ctx.remote, &set, &phase))
}

pub async fn sw_set(
    State(app): State<Shared>,
    Query(q): Query<SwQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    let s = Sw::parse(&q.k, q.on.unwrap_or(false));
    let w = match s {
        None => Err(write::Outcome::Failed(
            l.pick("unknown switch", "不认识的开关").into(),
        )),
        Some(s) => app.hub.read(|st| {
            if s.current(st).is_some() && fresh(st) {
                Ok(sw_write(s, upnp(st).as_ref(), l))
            } else {
                Err(not_read(l))
            }
        }),
    };
    let pending = area(
        "fw-sw",
        l,
        Via::Local,
        "",
        &write::Phase::Pending(s.map(|s| s.pending(l)).unwrap_or_default()),
    );
    run(
        app,
        pending,
        move |o| area("fw-sw", l, Via::Local, "", &write::Phase::Done(o)),
        w,
        l,
    )
}

pub async fn sw_cancel(
    Query(q): Query<write::CancelQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    one(area(
        "fw-sw",
        ctx.lang,
        ctx.remote,
        "",
        &write::cancel_phase(&q),
    ))
}

// ---- the forms ----

#[derive(Template)]
#[template(path = "router_firewall_form.html")]
struct Form {
    l: Lang,
    /// "dmz" or "pf".
    which: &'static str,
    open: bool,
    /// The switch's first state (`_don` / `_pe`), before Datastar runs.
    on: bool,
    /// The `data-signals` object (JSON).
    sigs: String,
    hint: String,
    lan: Option<String>,
}

/// The form's toggle button (`fw-{which}btn`) and its container (`fw-{which}form`), open or
/// closed, as two leaves (the page places them apart).
fn form_leaves(which: &'static str, open: bool, st: &St, l: Lang) -> String {
    let n = lan(st);
    let (on, sigs) = match which {
        "dmz" => {
            let f = firewall(st);
            let on = f.as_ref().and_then(|f| f.dmz).unwrap_or(false);
            (
                on,
                json!({"_don": on, "_dip": f.map(|f| f.dmz_ip).unwrap_or_default()}),
            )
        }
        _ => (
            true,
            json!({"_pn": "", "_pp": "TCP", "_pw": "", "_pi": "", "_pl": "", "_pe": true}),
        ),
    };
    Form {
        l,
        which,
        open,
        on,
        sigs: sigs.to_string(),
        hint: n.map_or_else(|| "192.168.0.x".into(), |n| n.hint()),
        lan: n.map(|n| n.text()),
    }
    .render()
    .unwrap()
}

/// The same as one patch.
fn form(which: &'static str, open: bool, st: &St, l: Lang) -> String {
    form_leaves(which, open, st, l).replace("<!--leaf-->", "\n")
}

fn closed(app: &Shared, which: &'static str, l: Lang) -> String {
    app.hub.read(|st| form(which, false, st, l))
}

pub async fn dmz_form(State(app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    one(app.hub.read(|st| form("dmz", true, st, l)))
}
pub async fn dmz_close(
    State(app): State<Shared>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    one(closed(&app, "dmz", l))
}
pub async fn pf_form(State(app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    one(app.hub.read(|st| form("pf", true, st, l)))
}
pub async fn pf_close(State(app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    one(closed(&app, "pf", l))
}

async fn body(req: axum::extract::Request) -> Vec<u8> {
    axum::body::to_bytes(req.into_body(), 16 * 1024)
        .await
        .map(|b| b.to_vec())
        .unwrap_or_default()
}

#[derive(Deserialize, Default)]
struct DmzIn {
    #[serde(default, rename = "_don")]
    on: bool,
    #[serde(default, rename = "_dip")]
    ip: String,
}

/// DMZ "Apply": check the draft, keep it under a token, ask (tier 3).
pub async fn dmz_ask(State(app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let f: DmzIn = serde_json::from_slice(&body(req).await).unwrap_or_default();
    let html = app.hub.read(|st| match dmz_check(f.on, &f.ip, st, l) {
        Err(o) => area("fw-dmz", l, ctx.remote, "", &write::Phase::Done(o)),
        Ok(d) => {
            let c = dmz_confirm(&d, lan(st), l);
            let set = format!("{PATH}/dmz/set?t={}", put(Held::Dmz(d)));
            area("fw-dmz", l, ctx.remote, &set, &write::Phase::Ask(c))
        }
    });
    one(html)
}

#[derive(Deserialize, Default)]
pub struct TokenQuery {
    #[serde(default)]
    t: String,
}

fn expired(l: Lang) -> write::Outcome {
    write::Outcome::NotYet(
        l.pick("The form expired; press it again", "表单已过期，请再按一次")
            .into(),
    )
}

pub async fn dmz_set(
    State(app): State<Shared>,
    Query(q): Query<TokenQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    let w = match take(&q.t) {
        Some(Held::Dmz(d)) => app
            .hub
            .read(|st| dmz_check(d.on, &d.ip, st, l))
            .map(|d| dmz_write(&d, l)),
        _ => Err(expired(l)),
    };
    let pending = area(
        "fw-dmz",
        l,
        Via::Local,
        "",
        &write::Phase::Pending(l.pick("Applying DMZ…", "正在应用 DMZ…").into()),
    );
    let a2 = app.clone();
    run(
        app,
        pending,
        move |o| {
            let close = if matches!(o, write::Outcome::Applied(_)) {
                closed(&a2, "dmz", l) + "\n"
            } else {
                String::new()
            };
            format!(
                "{close}{}",
                area("fw-dmz", l, Via::Local, "", &write::Phase::Done(o))
            )
        },
        w,
        l,
    )
}

pub async fn dmz_cancel(
    Query(q): Query<write::CancelQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    one(area(
        "fw-dmz",
        ctx.lang,
        ctx.remote,
        "",
        &write::cancel_phase(&q),
    ))
}

/// "Save Rule": check the form, keep it under a token, ask.
pub async fn pf_ask(State(app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let f: PfIn = serde_json::from_slice(&body(req).await).unwrap_or_default();
    let html = app.hub.read(|st| match pf_check(&f, st, l) {
        Err(o) => area("fw-pf", l, ctx.remote, "", &write::Phase::Done(o)),
        Ok(r) => {
            let c = pf_confirm(&r, ctx.remote, lan(st), l);
            let set = format!("{PATH}/pf/set?t={}", put(Held::Pf(r)));
            area("fw-pf", l, ctx.remote, &set, &write::Phase::Ask(c))
        }
    });
    one(html)
}

pub async fn pf_set(
    State(app): State<Shared>,
    Query(q): Query<TokenQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    let w = match take(&q.t) {
        Some(Held::Pf(r)) => app.hub.read(|st| {
            // checked again; "before" counts identical rows now, so a duplicate still has to show up as a new row
            let before = rules(st).map_or(0, |v| v.iter().filter(|x| x.same(&r)).count());
            if fresh(st) && rules(st).is_some() {
                Ok(pf_write(&r, before, l))
            } else {
                Err(not_read(l))
            }
        }),
        _ => Err(expired(l)),
    };
    let pending = area(
        "fw-pf",
        l,
        Via::Local,
        "",
        &write::Phase::Pending(l.pick("Adding the rule…", "正在添加规则…").into()),
    );
    let a2 = app.clone();
    run(
        app,
        pending,
        move |o| {
            let close = if matches!(o, write::Outcome::Applied(_)) {
                closed(&a2, "pf", l) + "\n"
            } else {
                String::new()
            };
            format!(
                "{close}{}",
                area("fw-pf", l, Via::Local, "", &write::Phase::Done(o))
            )
        },
        w,
        l,
    )
}

pub async fn pf_cancel(
    Query(q): Query<write::CancelQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    one(area(
        "fw-pf",
        ctx.lang,
        ctx.remote,
        "",
        &write::cancel_phase(&q),
    ))
}

#[derive(Deserialize, Default)]
pub struct IdQuery {
    #[serde(default)]
    id: String,
}

fn find(st: &St, id: &str, l: Lang) -> Result<Rule, write::Outcome> {
    if !fresh(st) {
        return Err(not_read(l));
    }
    let Some(v) = rules(st) else {
        return Err(not_read(l));
    };
    v.into_iter()
        .find(|r| r.id == id && r.deletable())
        .ok_or_else(|| {
            write::Outcome::NotYet(
                l.pick(
                    "That rule is not in the list any more",
                    "这条规则已经不在列表里了",
                )
                .into(),
            )
        })
}

/// A rule's "Delete": the confirm, or why not.
pub async fn del_ask(
    State(app): State<Shared>,
    Query(q): Query<IdQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let html = app.hub.read(|st| match find(st, &q.id, l) {
        Err(o) => area("fw-del", l, ctx.remote, "", &write::Phase::Done(o)),
        Ok(r) => {
            let set = format!("{PATH}/del/set?id={}", r.id);
            area(
                "fw-del",
                l,
                ctx.remote,
                &set,
                &write::Phase::Ask(del_confirm(&r, ctx.remote, lan(st), l)),
            )
        }
    });
    one(html)
}

pub async fn del_set(
    State(app): State<Shared>,
    Query(q): Query<IdQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    let w = app
        .hub
        .read(|st| find(st, &q.id, l))
        .map(|r| del_write(&r, l));
    let pending = area(
        "fw-del",
        l,
        Via::Local,
        "",
        &write::Phase::Pending(l.pick("Deleting the rule…", "正在删除规则…").into()),
    );
    run(
        app,
        pending,
        move |o| area("fw-del", l, Via::Local, "", &write::Phase::Done(o)),
        w,
        l,
    )
}

pub async fn del_cancel(
    Query(q): Query<write::CancelQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    one(area(
        "fw-del",
        ctx.lang,
        ctx.remote,
        "",
        &write::cancel_phase(&q),
    ))
}

#[cfg(test)]
mod tests;
