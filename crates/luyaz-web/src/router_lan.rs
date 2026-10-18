//! /router/lan (局域网 / DHCP), the old `router/lan/page.tsx`: the U60's LAN address and netmask,
//! the DHCP server switch, the address range it hands out and the lease. Batch 4 (u60-web.md).
//!
//! Where each value comes from (the old page read `GET /api/router/lan`, six `uci get`s):
//! - u60d's `dhcp` block: `ip` (`network.lan.ipaddr`), `netmask`, `dhcp_enabled`
//!   (`dhcp.lan.ignore` != "1"), `start` + `limit` (OpenWrt: start is an offset from the network
//!   address, not the last octet), `leasetime` (seconds on the device; "12h" style also read).
//!   A missing key (older u60d, the recording) shows "—"; an empty `ip` locks the change.
//! - The range end is computed here: network + start + limit − 1 (the agent replaced the last
//!   octet, wrong outside a /24 and past .255).
//!
//! Write: u60d `lan.set {ip, netmask, dhcp_disabled, dhcp_start, dhcp_end, lease_seconds}`, which
//! u60d maps to `zwrt_router.api router_set_lan_para {ipaddr, netmask, ignore, zte_start, zte_end,
//! leasetime}`. Chosen over the agent's passthrough (`lan_ipaddr`, `dhcp_enable`, …): those keys
//! were the old page's own and were never seen working, while `zte_start` / `zte_end` are the
//! firmware's uci options (`dhcp.lan.zte_start='192.168.0.2'`, full addresses), which upstream
//! datad read and wrote. Still unconfirmed on a device: the readback is the only proof.
//!
//! Tiers (controls-inventory.md §/router/lan, brief): a new LAN address or netmask is tier 3
//! everywhere (devices get new addresses, this page must be reopened at the new address); DHCP
//! on/off, range and lease are tier 2, tier 3 over Tailscale. The form opens as a patch; its fields
//! are page-local signals posted to the ask, which checks them and keeps the result under a random
//! token for the confirm. After a new address the old one goes dead mid-write over the LAN: the
//! link to `http://<new ip>:9090` is in the dialog and in the "正在…" patch, worded "if this page
//! stops answering", because nothing after the send can reach this tab.

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

use crate::agent::Polled;
use crate::hub::{Link, St};
use crate::i18n::Lang;
use crate::pages::{Shared, StreamQuery, split_leaves};
use crate::write;

pub const PATH: &str = "/router/lan";
/// luyaz-web's port on the device (`luyaz-web.init`: `--bind 0.0.0.0:9090`).
pub const PORT: u16 = 9090;
/// The LAN restarts; u60d re-reads uci at most every 30 s after the write cleared its cache.
const READBACK: Duration = Duration::from_secs(45);
const AREA: &str = "lan-ap";

/// This page's routes (the page, its stream, its buttons).
pub fn routes() -> Router<Shared> {
    Router::new()
        .route(PATH, get(page))
        .route("/router/lan/events", get(events))
        .route("/router/lan/refresh", post(refresh))
        .route("/router/lan/form", post(form_open))
        .route("/router/lan/close", post(form_close))
        .route("/router/lan/ask", post(ask))
        .route("/router/lan/set", post(set))
        .route("/router/lan/cancel", post(cancel))
}

// ---- what u60d says ----

/// The `dhcp` block, parsed. Empty / missing = unknown.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cur {
    pub ip_raw: String,
    pub ip: Option<Ipv4Addr>,
    pub mask_raw: String,
    pub mask: Option<Ipv4Addr>,
    pub enabled: Option<bool>,
    pub start: Option<u32>,
    pub limit: Option<u32>,
    pub lease_raw: String,
    pub stale: bool,
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_owned(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

pub fn cur(st: &St) -> Option<Cur> {
    let b = st.blocks.get("dhcp")?;
    let d = &b.data;
    if !d.is_object() {
        return None;
    }
    let ip_raw = text(&d["ip"]);
    let mask_raw = text(&d["netmask"]);
    Some(Cur {
        ip: ip_raw.parse().ok(),
        mask: mask_raw.parse().ok().filter(|m| prefix(*m).is_some()),
        ip_raw,
        mask_raw,
        enabled: d["dhcp_enabled"].as_bool(),
        start: text(&d["start"]).parse().ok(),
        limit: text(&d["limit"]).parse().ok(),
        lease_raw: text(&d["leasetime"]),
        stale: b.stale || st.link != Link::Live,
    })
}

/// Contiguous netmask → its prefix length (0 is not a LAN mask).
pub fn prefix(m: Ipv4Addr) -> Option<u32> {
    let m = u32::from(m);
    (m != 0 && m.leading_ones() + m.trailing_zeros() == 32).then(|| m.leading_ones())
}

/// (network, broadcast) of `ip` in `mask`.
pub fn net(ip: Ipv4Addr, mask: Ipv4Addr) -> (u32, u32) {
    let m = u32::from(mask);
    let n = u32::from(ip) & m;
    (n, n | !m)
}

impl Cur {
    /// The range handed out: network + start … network + start + limit − 1.
    pub fn range(&self) -> Option<(Ipv4Addr, Ipv4Addr)> {
        let (n, b) = net(self.ip?, self.mask?);
        let s = n.checked_add(self.start?)?;
        let e = s.checked_add(self.limit?.checked_sub(1)?)?;
        (self.limit? > 0 && e <= b).then(|| (Ipv4Addr::from(s), Ipv4Addr::from(e)))
    }
    pub fn lease(&self) -> Option<u64> {
        lease_secs(&self.lease_raw)
    }
    /// Can a change be asked for: current and with a LAN address.
    fn editable(&self) -> bool {
        !self.stale && self.ip.is_some()
    }
}

/// OpenWrt lease time ("86400", "12h", "30m", "1d", "1w") → seconds.
pub fn lease_secs(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, mult) = match s.char_indices().last()? {
        (i, 's') => (&s[..i], 1),
        (i, 'm') => (&s[..i], 60),
        (i, 'h') => (&s[..i], 3600),
        (i, 'd') => (&s[..i], 86400),
        (i, 'w') => (&s[..i], 604_800),
        _ => (s, 1),
    };
    if num.is_empty() || num.len() > 9 || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    num.parse::<u64>().ok().map(|n| n * mult)
}

/// "24 小时", "30 分钟", "90 秒".
pub fn lease_text(secs: u64, l: Lang) -> String {
    let (n, en, zh) = if secs >= 86400 && secs.is_multiple_of(86400) {
        (
            secs / 86400,
            if secs == 86400 { "day" } else { "days" },
            "天",
        )
    } else if secs >= 3600 && secs.is_multiple_of(3600) {
        (
            secs / 3600,
            if secs == 3600 { "hour" } else { "hours" },
            "小时",
        )
    } else if secs >= 60 && secs.is_multiple_of(60) {
        (
            secs / 60,
            if secs == 60 { "minute" } else { "minutes" },
            "分钟",
        )
    } else {
        (secs, if secs == 1 { "second" } else { "seconds" }, "秒")
    };
    match l {
        Lang::En => format!("{n} {en}"),
        Lang::Zh => format!("{n} {zh}"),
    }
}

pub fn url(ip: Ipv4Addr) -> String {
    format!("http://{ip}:{PORT}")
}

// ---- the view ----

pub struct Row {
    pub label: &'static str,
    pub value: String,
}

pub struct View {
    pub l: Lang,
    pub tone: &'static str,
    pub state: String,
    pub reason: Option<String>,
    pub meta: Option<String>,
    pub retry: bool,
    pub stale: bool,
    pub rows: Vec<Row>,
}

impl View {
    pub fn symbol(&self) -> &'static str {
        match self.tone {
            "bad" => "■",
            "warn" => "▲",
            _ => "●",
        }
    }
}

fn dash(s: &str) -> String {
    if s.is_empty() { "—".into() } else { s.into() }
}

pub fn view(st: &St, l: Lang) -> View {
    let c = cur(st);
    let known = matches!(st.link, Link::Live | Link::Down);
    let range = c.as_ref().and_then(Cur::range);
    let (mut tone, state, reason, retry) = match &c {
        None if known => (
            "bad",
            l.pick("Can't read the LAN settings", "读不到局域网设置")
                .to_string(),
            Some(
                match st.link {
                    Link::Down => l.pick("Can't reach the data service.", "连不上数据服务。"),
                    _ => l.pick(
                        "The data service has no LAN settings.",
                        "数据服务没有局域网设置。",
                    ),
                }
                .to_string(),
            ),
            true,
        ),
        None => (
            "neutral",
            l.pick("Reading the LAN settings…", "正在读取局域网设置…")
                .to_string(),
            None,
            false,
        ),
        Some(c) if c.ip.is_none() => (
            "warn",
            l.pick("LAN address unreadable", "读不到局域网地址")
                .to_string(),
            Some(
                l.pick(
                    "The device returned no LAN address. Refresh before changing anything.",
                    "设备没有返回局域网地址，刷新后再改。",
                )
                .to_string(),
            ),
            true,
        ),
        Some(c) => match c.enabled {
            Some(true) => (
                "ok",
                l.pick("DHCP on", "DHCP 已开启").to_string(),
                Some(match range {
                    Some((s, e)) => format!("{}{s} – {e}", l.pick("Hands out ", "分配 ")),
                    None => l
                        .pick("The range it hands out is unknown.", "分配的地址范围不明。")
                        .into(),
                }),
                false,
            ),
            Some(false) => (
                "neutral",
                l.pick("DHCP off", "DHCP 已关闭").to_string(),
                Some(
                    l.pick(
                        "Devices need an address set by hand.",
                        "设备要手动设置地址。",
                    )
                    .to_string(),
                ),
                false,
            ),
            None => (
                "neutral",
                l.pick("DHCP state unknown", "DHCP 状态不明").to_string(),
                Some(
                    l.pick(
                        "The device didn't say whether DHCP is on.",
                        "设备没有返回 DHCP 是否开启。",
                    )
                    .to_string(),
                ),
                false,
            ),
        },
    };
    let stale = c.as_ref().is_some_and(|c| c.stale);
    let mut meta = c.as_ref().filter(|c| c.ip.is_some()).map(|c| {
        format!(
            "{}{} / {}",
            l.pick("U60 at ", "U60 在 "),
            c.ip_raw,
            dash(&c.mask_raw)
        )
    });
    if stale {
        tone = "stale";
        meta = Some(
            l.pick(
                "Settings not current — refresh before changing anything",
                "设置不是最新的，刷新后再改",
            )
            .into(),
        );
    }
    let rows = match &c {
        None => Vec::new(),
        Some(c) => {
            let on = match c.enabled {
                None => "—",
                Some(true) => l.pick("On", "开"),
                Some(false) => l.pick("Off", "关"),
            };
            let (s, e) = match range {
                Some((s, e)) => (s.to_string(), e.to_string()),
                None => ("—".into(), "—".into()),
            };
            let lease = match c.lease() {
                Some(n) => lease_text(n, l),
                None => dash(&c.lease_raw),
            };
            vec![
                Row {
                    label: l.pick("LAN IP Address", "LAN IP 地址"),
                    value: dash(&c.ip_raw),
                },
                Row {
                    label: l.pick("Subnet Mask", "子网掩码"),
                    value: dash(&c.mask_raw),
                },
                Row {
                    label: l.pick("DHCP Server", "DHCP 服务器"),
                    value: on.into(),
                },
                Row {
                    label: l.pick("Start Address", "起始地址"),
                    value: s,
                },
                Row {
                    label: l.pick("End Address", "结束地址"),
                    value: e,
                },
                Row {
                    label: l.pick("Lease time", "租约时间"),
                    value: lease,
                },
            ]
        }
    };
    View {
        l,
        tone,
        state,
        reason,
        meta,
        retry,
        stale,
        rows,
    }
}

#[derive(Template)]
#[template(path = "router_lan_leaves.html")]
struct Leaves<'a> {
    v: &'a View,
}

fn leaves(app: &Shared, l: Lang) -> Vec<(String, String)> {
    app.hub
        .read(|st| split_leaves(&Leaves { v: &view(st, l) }.render().unwrap()))
}

// ---- the form and its check ----

/// The form's signals.
#[derive(Deserialize, Clone, Debug, Default)]
pub struct Draft {
    #[serde(default, rename = "_nip")]
    pub ip: String,
    #[serde(default, rename = "_nmask")]
    pub mask: String,
    #[serde(default, rename = "_non")]
    pub on: bool,
    #[serde(default, rename = "_nst")]
    pub start: String,
    #[serde(default, rename = "_nend")]
    pub end: String,
    #[serde(default, rename = "_nlease")]
    pub lease: String,
}

/// A checked change.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub ip: Ipv4Addr,
    pub mask: Ipv4Addr,
    pub on: bool,
    pub start: Ipv4Addr,
    pub end: Ipv4Addr,
    pub lease: u64,
    /// The address or netmask changes (tier 3, devices renumber).
    pub renumber: bool,
    pub ip_changed: bool,
    pub dhcp_changed: bool,
    pub range_changed: bool,
}

/// Writes are off (10-18, on the device): `router_set_lan_para` answered ok, then the firmware
/// rebooted the whole device; the values landed only in `zwrt_router.dhcp`, not in `dhcp.lan`
/// (what dnsmasq and the `dhcp` block read). Off until that sync and the reboot are understood;
/// `LUYAZ_WEB_LAN_WRITES=1` turns them back on (the browser checks against the mock).
fn writes_off_now() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| std::env::var("LUYAZ_WEB_LAN_WRITES").as_deref() != Ok("1"))
}

fn writes_off(l: Lang) -> write::Outcome {
    write::Outcome::NotYet(
        l.pick(
            "Changing LAN settings is off for now: on this device it reboots the whole U60 and the change does not take effect",
            "暂不开放更改局域网设置：在这台设备上会整机重启，而且改动不生效",
        )
        .into(),
    )
}

fn not_read(l: Lang) -> write::Outcome {
    write::Outcome::NotYet(
        l.pick(
            "LAN settings not read yet (or not current); try again shortly",
            "还没读到最新的局域网设置，稍后再试",
        )
        .into(),
    )
}

/// A range end: a host number ("100" = network + 100) or a full address, in the subnet, not the
/// network / broadcast address, not the U60 itself.
fn endpoint(
    s: &str,
    which: &str,
    ip: Ipv4Addr,
    mask: Ipv4Addr,
    l: Lang,
) -> Result<Ipv4Addr, String> {
    let (n, b) = net(ip, mask);
    let s = s.trim();
    let a = if !s.is_empty() && s.len() <= 10 && s.bytes().all(|c| c.is_ascii_digit()) {
        let h: u32 = s.parse().unwrap_or(0);
        match n.checked_add(h).filter(|x| h > 0 && *x < b) {
            Some(x) => Ipv4Addr::from(x),
            None => {
                return Err(match l {
                    Lang::En => format!("{which}: host number {s} is outside the network"),
                    Lang::Zh => format!("{which}：主机号 {s} 超出了网段"),
                });
            }
        }
    } else {
        s.parse::<Ipv4Addr>().map_err(|_| match l {
            Lang::En => {
                format!("{which}: enter a host number such as 100, or a full address")
            }
            Lang::Zh => format!("{which}：填主机号（例如 100）或完整地址"),
        })?
    };
    let x = u32::from(a);
    let net_text = format!("{}/{}", Ipv4Addr::from(n), prefix(mask).unwrap_or(0));
    if x & u32::from(mask) != n {
        return Err(match l {
            Lang::En => format!(
                "{which} {a} is not in the LAN's network ({net_text}); a host number such as 100 also works"
            ),
            Lang::Zh => {
                format!("{which} {a} 不在局域网网段（{net_text}）里；也可以只填主机号，例如 100")
            }
        });
    }
    if x == n || x == b {
        return Err(match l {
            Lang::En => format!("{which} {a} is the network's network or broadcast address"),
            Lang::Zh => format!("{which} {a} 是网段的网络地址或广播地址"),
        });
    }
    if a == ip {
        return Err(match l {
            Lang::En => format!("{which} {a} is the U60's own address"),
            Lang::Zh => format!("{which} {a} 是 U60 自己的地址"),
        });
    }
    Ok(a)
}

/// Check the draft against the current settings: a full, valid change, or why not.
pub fn check(d: &Draft, c: Option<&Cur>, l: Lang) -> Result<Plan, write::Outcome> {
    let Some(c) = c.filter(|c| c.editable()) else {
        return Err(not_read(l));
    };
    let bad = |s: String| Err(write::Outcome::NotYet(s));
    let Ok(ip) = d.ip.trim().parse::<Ipv4Addr>() else {
        return bad(l
            .pick(
                "LAN IP address: not an IPv4 address (like 192.168.0.1)",
                "LAN IP 地址：不是 IPv4 地址（例如 192.168.0.1）",
            )
            .into());
    };
    if !ip.is_private() {
        return bad(l
            .pick(
                "LAN IP address: use a private address (10.x.x.x, 172.16–31.x.x or 192.168.x.x)",
                "LAN IP 地址：要用私有地址（10.x.x.x、172.16–31.x.x 或 192.168.x.x）",
            )
            .into());
    }
    let Some(mask) = d
        .mask
        .trim()
        .parse::<Ipv4Addr>()
        .ok()
        .filter(|m| prefix(*m).is_some())
    else {
        return bad(l
            .pick(
                "Subnet mask: not a netmask (like 255.255.255.0)",
                "子网掩码：不是有效的掩码（例如 255.255.255.0）",
            )
            .into());
    };
    if !(8..=30).contains(&prefix(mask).unwrap_or(0)) {
        return bad(l
            .pick(
                "Subnet mask: use one from 255.0.0.0 to 255.255.255.252",
                "子网掩码：要在 255.0.0.0 到 255.255.255.252 之间",
            )
            .into());
    }
    let (n, b) = net(ip, mask);
    if u32::from(ip) == n || u32::from(ip) == b {
        return bad(l
            .pick(
                "LAN IP address: that is the network or broadcast address of this subnet",
                "LAN IP 地址：这是网段的网络地址或广播地址",
            )
            .into());
    }
    let start = endpoint(&d.start, l.pick("Start Address", "起始地址"), ip, mask, l);
    let end = endpoint(&d.end, l.pick("End Address", "结束地址"), ip, mask, l);
    let (start, end) = match (start, end) {
        (Err(e), _) | (_, Err(e)) => return bad(e),
        (Ok(s), Ok(e)) => (s, e),
    };
    if u32::from(start) > u32::from(end) {
        return bad(l
            .pick(
                "The start address comes after the end address",
                "起始地址在结束地址后面",
            )
            .into());
    }
    let Some(lease) = lease_secs(&d.lease).filter(|s| *s >= 120) else {
        return bad(l
            .pick(
                "Lease time: at least 2 minutes, in seconds or with a unit such as 30m, 12h, 1d",
                "租约时间：至少 2 分钟，填秒数或带单位，例如 30m、12h、1d",
            )
            .into());
    };
    let ip_changed = c.ip != Some(ip);
    let renumber = ip_changed || c.mask != Some(mask);
    let range_changed = c.range() != Some((start, end));
    // a range that holds the U60's address: only refused when this change makes it so
    let holds = (u32::from(start)..=u32::from(end)).contains(&u32::from(ip));
    if holds && (renumber || range_changed) {
        return bad(match l {
            Lang::En => format!(
                "The range {start} – {end} includes the U60's own address {ip}; start after it or end before it"
            ),
            Lang::Zh => format!(
                "范围 {start} – {end} 包含了 U60 自己的地址 {ip}；起始地址放到它后面，或结束地址放到它前面"
            ),
        });
    }
    let dhcp_changed = c.enabled != Some(d.on);
    let lease_changed = c.lease() != Some(lease);
    if !renumber && !dhcp_changed && !range_changed && !lease_changed {
        return bad(l
            .pick(
                "Nothing changed: these are the current settings",
                "没有改动：这就是现在的设置",
            )
            .into());
    }
    Ok(Plan {
        ip,
        mask,
        on: d.on,
        start,
        end,
        lease,
        renumber,
        ip_changed,
        dhcp_changed,
        range_changed: range_changed || lease_changed,
    })
}

/// `lan.set` params (u60d maps them onto `router_set_lan_para`).
pub fn params(p: &Plan) -> Value {
    json!({
        "ip": p.ip.to_string(),
        "netmask": p.mask.to_string(),
        "dhcp_disabled": i64::from(!p.on),
        "dhcp_start": p.start.to_string(),
        "dhcp_end": p.end.to_string(),
        "lease_seconds": p.lease.to_string(),
    })
}

/// The `dhcp` block shows the plan.
pub fn holds(p: &Plan, st: &St) -> bool {
    let Some(c) = cur(st) else { return false };
    c.ip == Some(p.ip)
        && c.mask == Some(p.mask)
        && c.enabled == Some(p.on)
        && (!p.on || (c.range() == Some((p.start, p.end)) && c.lease() == Some(p.lease)))
}

fn applied(p: &Plan, l: Lang) -> String {
    match (l, p.ip_changed) {
        (Lang::En, true) => format!("LAN settings applied: the U60 is now at {}", p.ip),
        (Lang::Zh, true) => format!("局域网设置已生效：U60 现在的地址是 {}", p.ip),
        _ => l.pick("LAN settings applied", "局域网设置已生效").into(),
    }
}

pub fn lan_write(p: &Plan, l: Lang) -> write::Write {
    let want = p.clone();
    write::Write {
        action: "lan.set",
        params: params(p),
        readback: Some(Box::new(move |st| holds(&want, st))),
        applied: applied(p, l),
        timeout: READBACK,
        typed: None,
        request_timeout: None,
    }
}

/// The confirm: what changes, sentence by sentence (the old page's `describe`).
pub fn confirm(p: &Plan, c: &Cur, remote: bool, l: Lang) -> write::Confirm {
    let mut what = Vec::new();
    let en = l == Lang::En;
    if p.ip_changed {
        what.push(if en {
            format!(
                "The U60's address becomes {}. This page then opens at {}.",
                p.ip,
                url(p.ip)
            )
        } else {
            format!(
                "U60 的地址改为 {}，之后这个页面在 {} 打开。",
                p.ip,
                url(p.ip)
            )
        });
    }
    if c.mask != Some(p.mask) {
        what.push(if en {
            format!("The netmask becomes {}.", p.mask)
        } else {
            format!("子网掩码改为 {}。", p.mask)
        });
    }
    if p.dhcp_changed {
        what.push(
            match (en, p.on) {
                (true, false) => {
                    "DHCP goes off: devices that join get no address and need one set by hand."
                }
                (false, false) => "DHCP 关闭：新连上的设备拿不到地址，要手动设置。",
                (true, true) => "DHCP comes on.",
                (false, true) => "DHCP 打开。",
            }
            .into(),
        );
    }
    if p.on && (p.range_changed || p.renumber || p.dhcp_changed) {
        let lease = lease_text(p.lease, l);
        what.push(if en {
            format!(
                "Addresses handed out: {} – {}, lease {lease}.",
                p.start, p.end
            )
        } else {
            format!("分配的地址：{} – {}，租约 {lease}。", p.start, p.end)
        });
    }
    what.push(
        if p.renumber {
            l.pick(
                "Every connected device drops and gets an address in the new network.",
                "所有连着的设备都会断开，再拿到新网段的地址。",
            )
        } else {
            l.pick(
                "The U60 reboots (about a minute): every connected device drops and fetches a new address.",
                "U60 会整机重启（约 1 分钟）：所有连着的设备断开，再重新拿地址。",
            )
        }
        .into(),
    );
    let recovery = if p.ip_changed {
        if en {
            format!(
                "Open {} and sign in again. If it doesn't load, disconnect and reconnect your device's Wi-Fi (or replug the cable) so it gets an address in the new network.",
                url(p.ip)
            )
        } else {
            format!(
                "打开 {} 并重新登录。打不开的话，断开再重连 Wi-Fi（或重新插网线），让电脑拿到新网段的地址。",
                url(p.ip)
            )
        }
    } else {
        l.pick(
            "Wait a moment and reload this page. If it still doesn't load, disconnect and reconnect your device's Wi-Fi (or replug the cable).",
            "稍等片刻再刷新这个页面。还打不开的话，断开再重连 Wi-Fi（或重新插网线）。",
        )
        .into()
    };
    let dialog = p.renumber || remote;
    write::Confirm {
        tier: if dialog {
            write::Tier::Dialog
        } else {
            write::Tier::Inline
        },
        title: l.pick("Apply the LAN settings?", "应用局域网设置？").into(),
        what: what.join(" "),
        downtime: dialog.then(|| {
            l.pick(
                "About 30 seconds while the LAN restarts; connected devices drop.",
                "局域网重启约 30 秒，连着的设备会断开。",
            )
            .into()
        }),
        recovery: dialog.then_some(recovery),
        action: l
            .pick("Confirm: apply the LAN settings", "确认：应用局域网设置")
            .into(),
        cuts_uplink: true,
        word: None,
        danger: false,
    }
}

// ---- checked forms waiting for their confirm ----

const STASH_TTL: Duration = Duration::from_secs(600);

fn stash() -> &'static Mutex<HashMap<String, (Instant, Plan)>> {
    static S: std::sync::OnceLock<Mutex<HashMap<String, (Instant, Plan)>>> =
        std::sync::OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

fn put(p: Plan) -> String {
    let t = crate::auth::random_id();
    let mut m = stash().lock().unwrap();
    m.retain(|_, (at, _)| at.elapsed() < STASH_TTL);
    m.insert(t.clone(), (Instant::now(), p));
    t
}

fn take(t: &str) -> Option<Plan> {
    stash()
        .lock()
        .unwrap()
        .remove(t)
        .filter(|(at, _)| at.elapsed() < STASH_TTL)
        .map(|(_, p)| p)
}

// ---- the new-address note (patched only by the write's replies) ----

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Note {
    None,
    /// Sent, waiting: if this page stops answering, open the new address.
    Pending,
    /// Read back: the U60 is at the new address.
    Moved,
    /// The reply came back on the old address without a readback: not confirmed.
    StillHere,
    /// The reply stream dropped (expected when the address changed).
    Lost,
    /// Read back, and this tab came by another address (Tailscale): it keeps working.
    Elsewhere,
}

#[derive(Template)]
#[template(path = "router_lan_note.html")]
pub struct NoteHtml {
    pub l: Lang,
    pub note: Note,
    pub ip: Option<Ipv4Addr>,
}

impl NoteHtml {
    pub fn url(&self) -> String {
        self.ip.map(url).unwrap_or_default()
    }
    pub fn lead(&self) -> String {
        let ip = self.ip.map(|i| i.to_string()).unwrap_or_default();
        let en = self.l == Lang::En;
        match self.note {
            Note::None => String::new(),
            Note::Pending => {
                if en {
                    format!("The U60 is moving to {ip}. If this page stops answering, open")
                } else {
                    format!("U60 正在换到 {ip}。如果这个页面不再回应，打开")
                }
            }
            Note::Moved => {
                if en {
                    format!("The U60 is now at {ip}. Open")
                } else {
                    format!("U60 现在的地址是 {ip}。打开")
                }
            }
            Note::StillHere => {
                if en {
                    "The U60 still answers at this address, so the new address is not confirmed. If this page stops answering, open".into()
                } else {
                    "U60 还在这个地址回应，新地址没有确认。如果这个页面不再回应，打开".into()
                }
            }
            Note::Lost => {
                if en {
                    "The connection dropped, as expected when the address changes. Open".into()
                } else {
                    "连接断了，换地址时这是预期的。打开".into()
                }
            }
            Note::Elsewhere => {
                if en {
                    format!("The U60 is now at {ip}. Devices on the LAN use")
                } else {
                    format!("U60 现在的地址是 {ip}。局域网里的设备之后用")
                }
            }
        }
    }
    pub fn tail(&self) -> &'static str {
        if self.note == Note::Elsewhere {
            return self.l.pick(
                " from now on; this page keeps working here.",
                " 打开；这个页面照常可用。",
            );
        }
        self.l.pick(
            " and sign in again (reconnect Wi-Fi first if it doesn't load).",
            " 并重新登录（打不开就先重连 Wi-Fi）。",
        )
    }
}

fn note(l: Lang, note: Note, ip: Option<Ipv4Addr>) -> String {
    NoteHtml {
        l,
        note: if ip.is_some() { note } else { Note::None },
        ip,
    }
    .render()
    .unwrap()
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
        block == "dhcp"
    }
    fn render(&mut self, app: &Shared) -> Option<Vec<(String, String)>> {
        Some(leaves(app, self.l))
    }
}

#[derive(Template)]
#[template(path = "router_lan.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    leaves: Vec<(String, String)>,
    op: String,
    note: String,
}

impl Page {
    fn leaf(&self, id: &str) -> &str {
        self.leaves
            .iter()
            .find(|(k, _)| k == id)
            .map_or("", |(_, h)| h.as_str())
    }
}

/// The write area; `ip`: the new address, carried to `cancel` so a lost reply can name it.
fn area(l: Lang, remote: bool, set: &str, ip: Option<Ipv4Addr>, phase: &write::Phase) -> String {
    let cancel = match ip {
        Some(ip) => format!("{PATH}/cancel?ip={ip}"),
        None => format!("{PATH}/cancel"),
    };
    write::Area {
        id: AREA,
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
    let html = Page {
        shell,
        l,
        leaves: {
            let mut v = leaves(&app, l);
            v.extend(app.hub.read(|st| split_leaves(&form_leaves(false, st, l))));
            v
        },
        op: area(l, ctx.remote, "", None, &write::Phase::Idle),
        note: note(l, Note::None, None),
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

#[derive(Template)]
#[template(path = "router_lan_form.html")]
struct Form {
    l: Lang,
    open: bool,
    on: bool,
    /// The `data-signals` object (JSON).
    sigs: String,
}

/// The "Change" button (`lan-btn`) and the form (`lan-form`), open or closed, as two leaves (the
/// page places them apart). Open: the fields start at the current values (range as addresses).
fn form_leaves(open: bool, st: &St, l: Lang) -> String {
    let c = cur(st).unwrap_or_default();
    let on = c.enabled.unwrap_or(false);
    let (s, e) = c
        .range()
        .map(|(s, e)| (s.to_string(), e.to_string()))
        .unwrap_or_default();
    let lease = c
        .lease()
        .map_or_else(|| c.lease_raw.clone(), |n| n.to_string());
    let sigs = json!({"_nip": c.ip_raw, "_nmask": c.mask_raw, "_non": on, "_nst": s, "_nend": e, "_nlease": lease});
    Form {
        l,
        open,
        on,
        sigs: sigs.to_string(),
    }
    .render()
    .unwrap()
}

/// The same as one patch.
fn form(open: bool, st: &St, l: Lang) -> String {
    form_leaves(open, st, l).replace("<!--leaf-->", "\n")
}

/// "Change": the form, or (settings not readable / not current) why not, in the area.
pub async fn form_open(
    State(app): State<Shared>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    one(app.hub.read(|st| {
        if writes_off_now() {
            area(l, ctx.remote, "", None, &write::Phase::Done(writes_off(l)))
        } else if cur(st).is_some_and(|c| c.editable()) {
            form(true, st, l) + "\n" + &area(l, ctx.remote, "", None, &write::Phase::Idle)
        } else {
            area(l, ctx.remote, "", None, &write::Phase::Done(not_read(l)))
        }
    }))
}

pub async fn form_close(
    State(app): State<Shared>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    one(app.hub.read(|st| form(false, st, l))
        + "\n"
        + &area(l, ctx.remote, "", None, &write::Phase::Idle))
}

async fn body(req: axum::extract::Request) -> Vec<u8> {
    axum::body::to_bytes(req.into_body(), 16 * 1024)
        .await
        .map(|b| b.to_vec())
        .unwrap_or_default()
}

/// "Apply": check the draft, keep it under a token, ask.
pub async fn ask(State(app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let d: Draft = serde_json::from_slice(&body(req).await).unwrap_or_default();
    let html = app.hub.read(|st| {
        let c = cur(st);
        match check(&d, c.as_ref(), l).and_then(|p| if writes_off_now() { Err(writes_off(l)) } else { Ok(p) }) {
            Err(o) => area(l, ctx.remote, "", None, &write::Phase::Done(o)),
            Ok(p) => {
                let cf = confirm(&p, c.as_ref().unwrap(), ctx.remote, l);
                let ip = p.ip_changed.then_some(p.ip);
                let set = format!("{PATH}/set?t={}", put(p));
                area(l, ctx.remote, &set, ip, &write::Phase::Ask(cf))
            }
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
        l.pick(
            "The form expired; press Apply again",
            "表单已过期，请再按一次「应用」",
        )
        .into(),
    )
}

/// What the note says after the send ended (the reply reached this tab). `here`: the tab came by
/// the old LAN address, which dies with the change, so a reply without a readback means the U60
/// still answers there. Otherwise (Tailscale, another name) the tab survives: no inference.
pub fn done_note(o: &write::Outcome, here: bool) -> Note {
    match (o, here) {
        (write::Outcome::Applied(_), true) => Note::Moved,
        (write::Outcome::Applied(_), false) => Note::Elsewhere,
        (
            write::Outcome::Accepted | write::Outcome::AcceptedSaid(_) | write::Outcome::Unknown,
            true,
        ) => Note::StillHere,
        _ => Note::None,
    }
}

/// The browser reached this page by the U60's LAN address `ip` (`Host`'s host part), i.e. this tab
/// goes dead when that address changes. Not `topbar::remote`: a routed LAN client counts as
/// remote there, yet its tab dies like a LAN one.
pub fn by_lan_address(headers: &axum::http::HeaderMap, ip: Option<Ipv4Addr>) -> bool {
    let Some(ip) = ip else { return false };
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default();
    let host = host.rsplit_once(':').map_or(host, |(h, _)| h);
    host.parse::<Ipv4Addr>().ok() == Some(ip)
}

pub async fn set(
    State(app): State<Shared>,
    Query(q): Query<TokenQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    let here = by_lan_address(req.headers(), app.hub.read(cur).and_then(|c| c.ip));
    let w = match take(&q.t).filter(|_| !writes_off_now()) {
        // checked again against what u60d says now
        Some(p) => app.hub.read(|st| {
            let d = Draft {
                ip: p.ip.to_string(),
                mask: p.mask.to_string(),
                on: p.on,
                start: p.start.to_string(),
                end: p.end.to_string(),
                lease: p.lease.to_string(),
            };
            check(&d, cur(st).as_ref(), l).map(|p| (lan_write(&p, l), p))
        }),
        None => Err(expired(l)),
    };
    let ip = w
        .as_ref()
        .ok()
        .and_then(|(_, p)| p.ip_changed.then_some(p.ip));
    let pending = area(
        l,
        false,
        "",
        ip,
        &write::Phase::Pending(
            l.pick("Applying the LAN settings…", "正在应用局域网设置…")
                .into(),
        ),
    ) + "\n"
        + &note(l, Note::Pending, ip);
    let a2 = app.clone();
    write::respond(pending, async move {
        let o = match w {
            Err(o) => o,
            Ok((w, _)) => write::execute(&a2.http, &a2.control_url, &a2.hub, &w, l).await,
        };
        let close = if matches!(o, write::Outcome::Applied(_)) {
            a2.hub.read(|st| form(false, st, l)) + "\n"
        } else {
            String::new()
        };
        format!(
            "{close}{}\n{}",
            area(l, false, "", ip, &write::Phase::Done(o.clone())),
            note(l, done_note(&o, here), ip)
        )
    })
}

#[derive(Deserialize, Default)]
pub struct CancelQuery {
    #[serde(default)]
    lost: bool,
    #[serde(default)]
    ip: Option<Ipv4Addr>,
}

/// Cancel / Esc, or the send's reply dropped (`lost`): with a new address that is expected.
pub async fn cancel(
    Query(q): Query<CancelQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let html =
        match (q.lost, q.ip) {
            (true, Some(ip)) => area(
                l,
                ctx.remote,
                "",
                None,
                &write::Phase::Done(write::Outcome::NotYet(
                    l.pick(
                        "Connection dropped; not confirmed yet whether the new address took effect",
                        "连接中断，还没确认新地址是否生效",
                    )
                    .into(),
                )),
            ) + "\n"
                + &note(l, Note::Lost, Some(ip)),
            _ => area(
                l,
                ctx.remote,
                "",
                None,
                &write::cancel_phase(&write::CancelQuery { lost: q.lost }),
            ),
        };
    one(html)
}

#[cfg(test)]
mod tests;
