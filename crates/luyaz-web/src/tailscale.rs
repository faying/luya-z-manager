//! /services/tailscale (Tailscale), the old `services/tailscale/page.tsx`: read-only, no device
//! write on this page.
//!
//! Where each value comes from (through `agent.rs` with the tab's own session; u60d first, the
//! agent when u60d does not answer):
//! - status block (state, reason, dns name, "numbers stopped at", Retry), the login card
//!   (`auth_url`), 「本节点」 (hostname, Tailscale name, state, IP v4/v6, DERP relay, version,
//!   subnet routes, key expiry, exit-node available), 「Mesh」 (peers, online, talking now), 「正在
//!   使用出口节点」, the peer list (first 32): `GET /api/services/tailscale`, polled every 5 s
//!   ([`EVERY`]) and re-read on 刷新 / Retry (`App::kick`). A reply with a non-empty `error` is a
//!   failure, as `tailscaleValid` made it in the old page (the agent answers 200 there): the last
//!   good data stays, greyed, with the reason.
//! - daemon log (last 200 lines, `/data/tailscaled.log`): `GET /api/services/tailscale/log?lines=200`
//!   (the agent's path; u60d's compat list carries `/api/tailscale/log`, which the agent does not
//!   serve), polled by the tab every 4 s while 暂停 is off (`data-on-interval` in the template); the tab posts the
//!   hash of what it shows, an unchanged log is a 204. Lines are escaped and cut to [`LINE_MAX`].
//!
//! Dropped on purpose: nothing the old page showed. Still not shown (the old page did not):
//! `peers[].dns_name`, `last_seen`, `id`.

use std::convert::Infallible;
use std::hash::{Hash, Hasher};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::{Query, Request, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::Stream;
use serde_json::Value;

use askama::Template;

use crate::agent::{Caller, Fail, Polled, esc};
use crate::i18n::Lang;
use crate::pages::{Shared, StreamQuery, patch, split_leaves};

const PATH: &str = "/services/tailscale";
const EVERY: Duration = Duration::from_secs(5);
const LOG_LINES: usize = 200;
/// The agent sends at most this many peers.
const PEER_CAP: usize = 32;
/// A log line longer than this is cut (the old page had no cut; the log is device-written text).
const LINE_MAX: usize = 2000;
const KEY_WARN_DAYS: i64 = 14;

// ---- what the agent says ----

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Peer {
    pub hostname: Option<String>,
    pub os: Option<String>,
    pub ips: Vec<String>,
    pub online: bool,
    pub exit_node: bool,
    pub rx: Option<f64>,
    pub tx: Option<f64>,
    pub last_handshake: Option<String>,
    pub active: bool,
    pub cur_addr: Option<String>,
    pub relay: Option<String>,
    pub routes: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SelfNode {
    pub hostname: Option<String>,
    pub dns_name: Option<String>,
    pub ips: Vec<String>,
    pub relay: Option<String>,
    pub exit_node_option: bool,
    /// `Some(None)` = the field is there and null (key expiry off); `None` = not sent.
    pub key_expiry: Option<Option<String>>,
    pub routes: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExitNode {
    pub hostname: Option<String>,
    pub ip: Option<String>,
    pub online: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub installed: bool,
    pub running: bool,
    pub backend_state: Option<String>,
    pub version: Option<String>,
    pub auth_url: Option<String>,
    pub me: Option<SelfNode>,
    pub exit: Option<ExitNode>,
    pub peer_count: Option<u64>,
    pub peer_online: Option<u64>,
    pub peer_active: Option<u64>,
    pub peer_direct: Option<u64>,
    pub peers: Vec<Peer>,
}

fn s(v: &Value) -> Option<String> {
    v.as_str().filter(|x| !x.is_empty()).map(Into::into)
}
fn strs(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str().map(Into::into)).collect())
        .unwrap_or_default()
}

/// `Err(reason)` = the reply carries an `error` (the old `tailscaleValid`).
pub fn parse(v: &Value) -> Result<Status, String> {
    if let Some(e) = v["error"].as_str().filter(|e| !e.trim().is_empty()) {
        return Err(e.to_owned());
    }
    let me = v["self"].is_object().then(|| {
        let m = &v["self"];
        SelfNode {
            hostname: s(&m["hostname"]),
            dns_name: s(&m["dns_name"]),
            ips: strs(&m["ips"]),
            relay: s(&m["relay"]),
            exit_node_option: m["exit_node_option"].as_bool().unwrap_or(false),
            key_expiry: m
                .get("key_expiry")
                .map(|k| s(k)),
            routes: m["primary_routes"].is_array().then(|| strs(&m["primary_routes"])),
        }
    });
    let exit = v["exit_node"].is_object().then(|| {
        let x = &v["exit_node"];
        ExitNode {
            hostname: s(&x["hostname"]),
            ip: strs(&x["ips"]).into_iter().next(),
            online: x["online"].as_bool().unwrap_or(false),
        }
    });
    Ok(Status {
        installed: v["installed"].as_bool().unwrap_or(false),
        running: v["running"].as_bool().unwrap_or(false),
        backend_state: s(&v["backend_state"]),
        version: s(&v["version"]),
        auth_url: s(&v["auth_url"]),
        me,
        exit,
        peer_count: v["peer_count"].as_u64(),
        peer_online: v["peer_online"].as_u64(),
        peer_active: v["peer_active"].as_u64(),
        peer_direct: v["peer_direct"].as_u64(),
        peers: v["peers"]
            .as_array()
            .map(|a| {
                a.iter()
                    .take(PEER_CAP)
                    .map(|p| Peer {
                        hostname: s(&p["hostname"]),
                        os: s(&p["os"]),
                        ips: strs(&p["ips"]),
                        online: p["online"].as_bool().unwrap_or(false),
                        exit_node: p["exit_node"].as_bool().unwrap_or(false),
                        rx: p["rx_bytes"].as_f64(),
                        tx: p["tx_bytes"].as_f64(),
                        last_handshake: s(&p["last_handshake"]),
                        active: p["active"].as_bool().unwrap_or(false),
                        cur_addr: s(&p["cur_addr"]),
                        relay: s(&p["relay"]),
                        routes: strs(&p["primary_routes"]),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    })
}

// ---- small formatters (the old page's) ----

/// Unknown counters show "—", never 0.
fn fmt_bytes(n: Option<f64>) -> String {
    let Some(n) = n.filter(|n| n.is_finite() && *n >= 0.0) else {
        return "—".into();
    };
    if n < 1024.0 {
        format!("{n:.0} B")
    } else if n < 1024.0 * 1024.0 {
        format!("{:.1} KB", n / 1024.0)
    } else if n < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} MB", n / 1024.0 / 1024.0)
    } else {
        format!("{:.2} GB", n / 1024.0 / 1024.0 / 1024.0)
    }
}

/// Seconds since the epoch of an RFC 3339 time (`2026-09-24T14:02:11Z`, optional fraction and
/// `±HH:MM`).
fn parse_rfc3339(t: &str) -> Option<i64> {
    let b = t.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    let n = |a: usize, z: usize| t.get(a..z)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (n(0, 4)?, n(5, 7)?, n(8, 10)?, n(11, 13)?, n(14, 16)?, n(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let mut rest = &t[19..];
    if let Some(r) = rest.strip_prefix('.') {
        rest = r.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    let off = match rest {
        "Z" | "z" => 0,
        r if r.len() == 6 && matches!(&r[..1], "+" | "-") && &r[3..4] == ":" => {
            let v = r[1..3].parse::<i64>().ok()? * 3600 + r[4..6].parse::<i64>().ok()? * 60;
            if &r[..1] == "-" { -v } else { v }
        }
        _ => return None,
    };
    // days from civil (H. Hinnant)
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3600 + mi * 60 + se - off)
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Days until the node key expires; None = no expiry / unknown. (Whole days, rounded down.)
fn key_days_left(iso: &str, now: i64) -> Option<i64> {
    parse_rfc3339(iso).map(|at| (at - now).div_euclid(86_400))
}

/// "12s ago" … like the old `fmtRelative`: never = None; a stamp from the future (the device clock
/// is local time labelled UTC, tailscaled sometimes stamps the same way) within 12 h = just now.
fn relative(iso: &str, now: i64, l: Lang) -> Option<String> {
    if iso.starts_with("0001-01-01") {
        return None;
    }
    let Some(at) = parse_rfc3339(iso) else {
        return Some(iso.to_owned());
    };
    let diff = now - at;
    if diff < 0 {
        return (diff > -12 * 3600).then(|| l.pick("just now", "刚刚").to_owned());
    }
    Some(match (diff, l) {
        (0..=59, Lang::En) => format!("{diff}s ago"),
        (0..=59, Lang::Zh) => format!("{diff} 秒前"),
        (60..=3599, Lang::En) => format!("{}m ago", diff / 60),
        (60..=3599, Lang::Zh) => format!("{} 分钟前", diff / 60),
        (3600..=86_399, Lang::En) => format!("{}h ago", diff / 3600),
        (3600..=86_399, Lang::Zh) => format!("{} 小时前", diff / 3600),
        (_, Lang::En) => format!("{}d ago", diff / 86_400),
        (_, Lang::Zh) => format!("{} 天前", diff / 86_400),
    })
}

// ---- html bits ----

fn pk(l: Lang, en: &str, zh: &str) -> String {
    match l {
        Lang::En => en.to_owned(),
        Lang::Zh => zh.to_owned(),
    }
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

fn online_mark(on: bool, l: Lang) -> String {
    if on {
        mark("ok", l.pick("Online", "在线"))
    } else {
        mark("neutral", l.pick("Offline", "离线"))
    }
}

/// The (?) button and its popover (macros.html `help`).
fn help(l: Lang, id: &str, label: &str, text: &str) -> String {
    format!(
        "<button type=\"button\" class=\"nd-help\" popovertarget=\"h-{id}\" aria-label=\"{} {}\"><svg width=\"18\" height=\"18\" viewBox=\"0 0 256 256\" fill=\"currentColor\" aria-hidden=\"true\"><path d=\"M144,180a16,16,0,1,1-16-16A16,16,0,0,1,144,180Zm92-52A108,108,0,1,1,128,20,108.12,108.12,0,0,1,236,128Zm-24,0a84,84,0,1,0-84,84A84.09,84.09,0,0,0,212,128ZM128,64c-24.26,0-44,17.94-44,40v4a12,12,0,0,0,24,0v-4c0-8.82,9-16,20-16s20,7.18,20,16-9,16-20,16a12,12,0,0,0-12,12v8a12,12,0,0,0,23.73,2.56C158.31,137.88,172,122.37,172,104,172,81.94,152.26,64,128,64Z\"/></svg></button><div id=\"h-{id}\" popover class=\"nd-pop\">{}</div>",
        l.pick("About", "关于"),
        esc(label),
        esc(text)
    )
}

/// One label/value row; `label` and `value` are HTML.
fn row(label: &str, value: &str, mono: bool) -> String {
    format!(
        "<div class=\"nd-row\"><span class=\"nd-row__text\"><span class=\"nd-row__label\">{label}</span></span><span class=\"nd-row__value{}\">{value}</span></div>",
        if mono { " nd-mono" } else { "" }
    )
}

fn dash(v: &Option<String>) -> String {
    v.as_deref().map_or("—".into(), esc)
}

fn num(v: Option<u64>) -> String {
    v.map_or("—".into(), |n| n.to_string())
}

fn peers_summary(st: &Status, l: Lang) -> String {
    match l {
        Lang::En => format!("{} total · {} online", num(st.peer_count), num(st.peer_online)),
        Lang::Zh => format!("共 {} 个 · {} 在线", num(st.peer_count), num(st.peer_online)),
    }
}

// ---- the page's leaves ----

struct Poll {
    who: Caller,
    l: Lang,
    /// The last good reply.
    data: Option<Status>,
    fail: Option<Fail>,
    /// The last reply carried an `error`.
    invalid: Option<String>,
    at: Option<SystemTime>,
}

impl Poll {
    fn new(who: Caller, l: Lang) -> Poll {
        Poll {
            who,
            l,
            data: None,
            fail: None,
            invalid: None,
            at: None,
        }
    }

    async fn fetch(&mut self, app: &Shared) {
        match crate::agent::get(app, &self.who, "/api/services/tailscale").await {
            Ok(v) => match parse(&v) {
                Ok(d) => {
                    self.data = Some(d);
                    self.fail = None;
                    self.invalid = None;
                    self.at = Some(SystemTime::now());
                }
                Err(e) => {
                    self.invalid = Some(e);
                    self.fail = None;
                }
            },
            Err(f) => {
                self.fail = Some(f);
                self.invalid = None;
            }
        }
    }

    fn failing(&self) -> bool {
        self.fail.is_some() || self.invalid.is_some()
    }

    fn stale(&self) -> bool {
        self.data.is_some() && self.failing()
    }

    /// (tone, state, reason) of the status block, before the stale grey.
    fn state(&self, now: i64) -> (&'static str, String, Option<String>) {
        let l = self.l;
        let t = |en: &str, zh: &str| pk(l, en, zh);
        let Some(d) = &self.data else {
            return if self.failing() {
                ("bad", t("Can't read Tailscale status", "读不到 Tailscale 状态"), None)
            } else {
                ("neutral", t("Loading", "加载中"), None)
            };
        };
        if !d.installed {
            return (
                "neutral",
                t("Tailscale isn't installed", "未安装 Tailscale"),
                Some(t(
                    "No tailscaled binary was found in /data/tailscale/. Run the toolkit installer's Tailscale module on the device, then refresh this page.",
                    "未在 /data/tailscale/ 找到 tailscaled。请在设备上运行工具箱安装器的 Tailscale 模块，然后刷新本页。",
                )),
            );
        }
        if !d.running {
            return (
                "bad",
                t("Stopped", "已停止"),
                Some(t("tailscaled isn't running on the device.", "设备上的 tailscaled 没在运行。")),
            );
        }
        if d.auth_url.is_some() {
            return (
                "warn",
                t("Auth required", "需要认证"),
                Some(t(
                    "Open the login link below to connect this router.",
                    "打开下面的登录链接，把这台路由器连上。",
                )),
            );
        }
        match d.backend_state.as_deref() {
            Some("Running") => {
                let days = d
                    .me
                    .as_ref()
                    .and_then(|m| m.key_expiry.clone().flatten())
                    .and_then(|k| key_days_left(&k, now));
                let peers = peers_summary(d, l);
                match days {
                    Some(n) if n < 0 => (
                        "bad",
                        t("Key expired", "密钥已过期"),
                        Some(t(
                            "Re-authenticate this router in the Tailscale admin console, or it stays off the tailnet.",
                            "请在 Tailscale 控制台为这台路由器重新认证，否则它一直在 tailnet 之外。",
                        )),
                    ),
                    Some(n) if n < KEY_WARN_DAYS => (
                        "warn",
                        match l {
                            Lang::En => format!("Key expires in {n} days"),
                            Lang::Zh => format!("密钥 {n} 天后过期"),
                        },
                        Some(match l {
                            Lang::En => format!("Renew it (or turn off key expiry for this node) in the Tailscale admin console before it lapses. {peers}"),
                            Lang::Zh => format!("请在失效前到 Tailscale 控制台续期（或关闭此节点的密钥过期）。{peers}"),
                        }),
                    ),
                    _ => ("ok", t("Running", "运行中"), Some(peers)),
                }
            }
            Some("NeedsLogin") => ("warn", t("Needs login", "需要登录"), None),
            Some("Stopped") => ("bad", t("Stopped", "已停止"), None),
            Some(other) => ("neutral", other.to_owned(), None),
            None => ("neutral", t("Unknown", "未知"), None),
        }
    }

    fn leaves(&self) -> Vec<(String, String)> {
        self.leaves_at(now_secs())
    }

    fn leaves_at(&self, now: i64) -> Vec<(String, String)> {
        let l = self.l;
        let stale = self.stale();
        let mut out = String::new();

        // status block
        let (mut tone, state, mut reason) = self.state(now);
        let mut role_alert = tone == "bad";
        if let Some(e) = &self.invalid {
            role_alert = true;
            reason = Some(match l {
                Lang::En => format!("Tailscale: {e}"),
                Lang::Zh => format!("Tailscale：{e}"),
            });
        } else if let (Some(f), None) = (&self.fail, &self.data) {
            reason = Some(match l {
                Lang::En => format!("Couldn't read the status: {} · retry", f.text(l)),
                Lang::Zh => format!("读不到状态：{} · 重试", f.text(l)),
            });
        }
        let mut meta = String::new();
        if let Some(dns) = self
            .data
            .as_ref()
            .filter(|d| d.installed && !stale)
            .and_then(|d| d.me.as_ref())
            .and_then(|m| m.dns_name.as_deref())
        {
            meta.push_str(&format!("<span class=\"nd-mono\">{}</span> ", esc(dns)));
        }
        if stale {
            tone = "stale";
            meta.push_str(&format!(
                "<span class=\"nd-fresh\">{}</span>",
                esc(&crate::agent::stopped(l, "Numbers", "数字", self.at))
            ));
        } else if self.data.is_none() && self.failing() {
            meta.push_str(&format!(
                "<span class=\"nd-fresh\">{}</span>",
                l.pick("No data yet", "尚未取得数据")
            ));
        }
        let actions = if self.failing() {
            format!(
                "<button type=\"button\" class=\"nd-btn nd-btn--secondary nd-btn--sm\" data-on:click=\"@post('/services/tailscale/refresh')\">{}</button>",
                l.pick("Retry", "重试")
            )
        } else {
            String::new()
        };
        out.push_str(&format!(
            "<div id=\"ts-status\"><section class=\"nd-status nd-status--{tone}{}\"><div class=\"nd-status__main\"><div class=\"nd-status__state\" role=\"status\">{}</div>{}{}</div><div class=\"nd-status__actions\">{actions}</div></section></div>\n<!--leaf-->\n",
            if tone == "stale" { " nd-stale" } else { "" },
            mark(if tone == "stale" { "neutral" } else { tone }, &state),
            reason.map_or(String::new(), |r| format!(
                "<div class=\"nd-status__reason\"><span{}>{}</span></div>",
                if role_alert { " role=\"alert\"" } else { "" },
                esc(&r)
            )),
            if meta.is_empty() {
                String::new()
            } else {
                format!("<div class=\"nd-status__meta\">{meta}</div>")
            },
        ));

        let d = self.data.as_ref();
        // login card
        match d.and_then(|d| d.auth_url.as_deref()) {
            Some(url) => {
                // only an http(s) link is made clickable
                let link = if url.starts_with("https://") || url.starts_with("http://") {
                    format!(
                        "<a href=\"{u}\" target=\"_blank\" rel=\"noreferrer\" class=\"nd-mono ts-link\">{u}</a>",
                        u = esc(url)
                    )
                } else {
                    format!("<span class=\"nd-mono ts-link\">{}</span>", esc(url))
                };
                out.push_str(&format!(
                    "<section id=\"ts-login\" aria-labelledby=\"ts-login-t\"><h2 id=\"ts-login-t\" class=\"nd-group-title\">{}</h2><div class=\"nd-group ts-login\"><p class=\"nd-body\">{}</p>{link}</div></section>\n<!--leaf-->\n",
                    l.pick("Login required", "需要登录"),
                    l.pick(
                        "Tailscale needs authentication. Open this URL on a logged-in device:",
                        "Tailscale 需要认证。请在已登录的设备上打开此链接："
                    )
                ));
            }
            None => out.push_str("<div id=\"ts-login\"></div>\n<!--leaf-->\n"),
        }

        match d.filter(|d| d.installed) {
            None => out.push_str(
                "<div id=\"ts-node\"></div>\n<!--leaf-->\n<div id=\"ts-peers\"></div>\n<!--leaf-->\n<div id=\"ts-mesh\"></div>",
            ),
            Some(d) => {
                out.push_str(&self.node(d, stale, now));
                out.push_str("\n<!--leaf-->\n");
                out.push_str(&self.peers(d, stale, now));
                out.push_str("\n<!--leaf-->\n");
                out.push_str(&self.mesh(d, stale));
            }
        }
        split_leaves(&out)
    }

    fn node(&self, d: &Status, stale: bool, now: i64) -> String {
        let l = self.l;
        let me = d.me.as_ref();
        let ips = me.map(|m| m.ips.as_slice()).unwrap_or(&[]);
        let v4 = ips.iter().find(|i| !i.contains(':')).cloned();
        let v6 = ips.iter().find(|i| i.contains(':')).cloned();
        let lab = |en: &str, zh: &str| esc(pk(l, en, zh).as_str());
        let mut rows = String::new();
        rows.push_str(&row(&lab("Hostname", "主机名"), &dash(&me.and_then(|m| m.hostname.clone())), true));
        rows.push_str(&row(&lab("Tailscale name", "Tailscale 名称"), &dash(&me.and_then(|m| m.dns_name.clone())), true));
        rows.push_str(&row(
            &format!(
                "{} {}",
                lab("State", "状态"),
                help(l, "ts-state", l.pick("State", "状态"), l.pick("Connection state of the Tailscale daemon.", "Tailscale 守护进程的连接状态。"))
            ),
            &dash(&d.backend_state),
            false,
        ));
        rows.push_str(&row("IP (v4)", &dash(&v4), true));
        rows.push_str(&row("IP (v6)", &dash(&v6), true));
        rows.push_str(&row(
            &format!(
                "{} {}",
                lab("DERP relay", "DERP 中继"),
                help(l, "ts-derp", l.pick("DERP relay", "DERP 中继"), l.pick("The Tailscale relay region used when a direct peer-to-peer link isn't possible.", "无法建立直连时使用的 Tailscale 中继区域。"))
            ),
            &dash(&me.and_then(|m| m.relay.clone())),
            true,
        ));
        rows.push_str(&row(&lab("Version", "版本"), &dash(&d.version), true));
        if let Some(r) = me.and_then(|m| m.routes.as_ref()) {
            rows.push_str(&row(
                &lab("Subnet routes", "子网路由"),
                &if r.is_empty() {
                    esc(l.pick("None", "无"))
                } else {
                    esc(&r.join(", "))
                },
                !r.is_empty(),
            ));
        }
        if let Some(k) = me.and_then(|m| m.key_expiry.as_ref()) {
            let v = match k {
                Some(iso) => {
                    let n = key_days_left(iso, now);
                    let tone = match n {
                        None => "neutral",
                        Some(n) if n < 0 => "bad",
                        Some(n) if n < KEY_WARN_DAYS => "warn",
                        _ => "ok",
                    };
                    let date = iso.get(..10).unwrap_or(iso);
                    let left = n.unwrap_or(0).max(0);
                    mark(
                        tone,
                        &match l {
                            Lang::En => format!("{date} · {left} days left"),
                            Lang::Zh => format!("{date} · 还剩 {left} 天"),
                        },
                    )
                }
                None => esc(l.pick("Never (expiry off)", "永不过期（已关闭密钥过期）")),
            };
            rows.push_str(&row(&lab("Key expires", "密钥到期"), &v, false));
        }
        rows.push_str(&row(
            &format!(
                "{} {}",
                lab("Exit-node available", "可作出口节点"),
                help(l, "ts-exit", l.pick("Exit-node available", "可作出口节点"), l.pick("Whether other devices can route their internet through this router.", "其他设备能否通过本路由器路由其互联网流量。"))
            ),
            &match me {
                Some(m) if m.exit_node_option => esc(l.pick("Yes", "是")),
                Some(_) => esc(l.pick("No", "否")),
                None => "—".into(),
            },
            false,
        ));
        format!(
            "<section id=\"ts-node\" class=\"ts-sec\" aria-labelledby=\"ts-node-t\"><h2 id=\"ts-node-t\" class=\"nd-group-title\">{}</h2><p class=\"nd-aux ts-sub\">{}</p><div class=\"nd-group{}\">{rows}</div></section>",
            l.pick("This node", "本节点"),
            l.pick("How the router appears on your Tailscale network.", "路由器在你的 Tailscale 网络中的呈现。"),
            if stale { " nd-stale" } else { "" },
        )
    }

    fn peers(&self, d: &Status, stale: bool, now: i64) -> String {
        let l = self.l;
        let capped = d.peers.len() >= PEER_CAP && d.peer_count.unwrap_or(0) > PEER_CAP as u64;
        let mut rows = String::new();
        if d.peers.is_empty() {
            rows.push_str(&format!(
                "<div class=\"nd-row nd-row--two\"><span class=\"nd-row__text\"><span class=\"nd-row__label\">{}</span><span class=\"nd-row__sub block\">{}</span></span></div>",
                l.pick("No other devices on this tailnet yet", "这个 tailnet 里还没有其他设备"),
                l.pick("Sign in to Tailscale on another device and it will show up here.", "在另一台设备上登录 Tailscale，它就会出现在这里。"),
            ));
        }
        for p in &d.peers {
            rows.push_str(&self.peer_row(p, now));
        }
        format!(
            "<section id=\"ts-peers\" class=\"ts-sec\" aria-labelledby=\"ts-peers-t\"><h2 id=\"ts-peers-t\" class=\"nd-group-title\">{}</h2><p class=\"nd-aux ts-sub\">{}{}</p><div class=\"nd-group{}\">{rows}</div></section>",
            l.pick("Peers", "节点列表"),
            esc(&peers_summary(d, l)),
            if capped {
                match l {
                    Lang::En => format!(" · showing the first {PEER_CAP}"),
                    Lang::Zh => format!(" · 只显示前 {PEER_CAP} 个"),
                }
            } else {
                String::new()
            },
            if stale { " nd-stale" } else { "" },
        )
    }

    fn peer_row(&self, p: &Peer, now: i64) -> String {
        let l = self.l;
        let mut parts: Vec<String> = vec![];
        if let Some(ip) = p.ips.first() {
            parts.push(format!("<span class=\"nd-mono\">{}</span>", esc(ip)));
        }
        if let Some(os) = &p.os {
            parts.push(format!("<span>{}</span>", esc(os)));
        }
        parts.push(format!(
            "<span class=\"tabular-nums\">↑{} ↓{}</span>",
            fmt_bytes(p.tx),
            fmt_bytes(p.rx)
        ));
        if let Some(w) = p.last_handshake.as_deref().and_then(|h| relative(h, now, l)) {
            parts.push(format!(
                "<span>{}</span>",
                match l {
                    Lang::En => format!("handshake {}", esc(&w)),
                    Lang::Zh => format!("握手 {}", esc(&w)),
                }
            ));
        }
        if p.active {
            let path = if p.cur_addr.is_some() {
                pk(l, "direct", "直连")
            } else if let Some(r) = &p.relay {
                match l {
                    Lang::En => format!("relay {r}"),
                    Lang::Zh => format!("中继 {r}"),
                }
            } else {
                pk(l, "relay", "中继")
            };
            parts.push(format!("<span data-testid=\"ts-path\">{}</span>", esc(&path)));
        }
        if !p.routes.is_empty() {
            parts.push(format!("<span class=\"nd-mono\">{}</span>", esc(&p.routes.join(", "))));
        }
        format!(
            "<div class=\"nd-row nd-row--two ts-peer\"><span class=\"nd-row__text\"><span class=\"nd-row__label\">{}{}</span><span class=\"nd-row__sub ts-wrap\">{}</span></span><span class=\"nd-row__value\">{}</span></div>",
            p.hostname.as_deref().map_or("—".into(), esc),
            if p.exit_node {
                format!("<span class=\"nd-aux ts-chip\">{}</span>", l.pick("exit node", "出口节点"))
            } else {
                String::new()
            },
            parts.join(" · "),
            online_mark(p.online, l),
        )
    }

    fn mesh(&self, d: &Status, stale: bool) -> String {
        let l = self.l;
        let st = if stale { " nd-stale" } else { "" };
        let mut rows = row(&esc(l.pick("Peers", "节点")), &num(d.peer_count), false);
        rows.push_str(&row(&esc(l.pick("Online", "在线")), &num(d.peer_online), false));
        if let Some(n) = d.peer_active {
            let direct = d.peer_direct.unwrap_or(0);
            rows.push_str(&row(
                &format!(
                    "{} {}",
                    esc(l.pick("Talking now", "正在通信")),
                    help(l, "ts-direct", l.pick("Talking now", "正在通信"), l.pick("Direct = a peer-to-peer path; relay = through a Tailscale DERP server, slower. Behind strict NAT (some carriers abroad) relay is normal.", "直连 = 点对点路径；中继 = 经 Tailscale 的 DERP 服务器，较慢。在严格 NAT 后面（国外一些运营商）走中继是正常的。"))
                ),
                &match l {
                    Lang::En => format!("{n} · {direct} direct · {} relay", n.saturating_sub(direct)),
                    Lang::Zh => format!("{n} · {direct} 直连 · {} 中继", n.saturating_sub(direct)),
                },
                false,
            ));
        }
        let exit = match &d.exit {
            Some(x) => format!(
                "<div class=\"nd-row nd-row--two\"><span class=\"nd-row__text\"><span class=\"nd-row__label\">{}</span>{}</span><span class=\"nd-row__value\">{}</span></div>",
                x.hostname.as_deref().map_or("—".into(), esc),
                x.ip.as_deref().map_or(String::new(), |ip| format!("<span class=\"nd-row__sub block nd-mono\">{}</span>", esc(ip))),
                online_mark(x.online, l),
            ),
            None => format!(
                "<div class=\"nd-row\"><span class=\"nd-row__text\"><span class=\"nd-row__label\">{}</span></span></div>",
                l.pick("No exit node selected.", "未选择出口节点。")
            ),
        };
        format!(
            "<div id=\"ts-mesh\" class=\"ts-col\"><section class=\"ts-sec\" aria-labelledby=\"ts-mesh-t\"><h2 id=\"ts-mesh-t\" class=\"nd-group-title\">{}</h2><div class=\"nd-group{st}\">{rows}</div></section><section class=\"ts-sec\" aria-labelledby=\"ts-exit-t\"><h2 id=\"ts-exit-t\" class=\"nd-group-title\">{}</h2><div class=\"nd-group{st}\">{exit}</div></section></div>",
            l.pick("Mesh", "Mesh 网络"),
            l.pick("Exit node in use", "正在使用出口节点"),
        )
    }
}

impl Polled for Poll {
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        self.fetch(app).await;
        (self.leaves(), EVERY)
    }
}

// ---- the log ----

fn lines_of(v: &Value) -> Vec<String> {
    let all: Vec<&str> = v["lines"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    all[all.len().saturating_sub(LOG_LINES)..]
        .iter()
        .map(|x| {
            if x.len() > LINE_MAX {
                let mut e = LINE_MAX;
                while !x.is_char_boundary(e) {
                    e -= 1;
                }
                format!("{}…", &x[..e])
            } else {
                (*x).to_owned()
            }
        })
        .collect()
}

fn hash_of(lines: &[String]) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    lines.hash(&mut h);
    format!("{:x}", h.finish())
}

/// The log box (`#ts-logbox`): the lines escaped, or why not.
fn log_box(l: Lang, lines: Option<&[String]>, err: Option<&str>) -> String {
    let note = err.map_or(String::new(), |e| {
        format!(
            "<p class=\"nd-console__muted\" role=\"alert\">{}</p>",
            esc(&match l {
                Lang::En => format!("Couldn't read the log: {e}"),
                Lang::Zh => format!("读不到日志：{e}"),
            })
        )
    });
    let (body, h) = match lines {
        Some(ls) if !ls.is_empty() => (esc(&ls.join("\n")), hash_of(ls)),
        Some(_) => (
            format!("<span class=\"nd-console__muted\">{}</span>", l.pick("No log lines yet.", "暂无日志。")),
            hash_of(&[]),
        ),
        None => (String::new(), String::new()),
    };
    format!(
        "<div id=\"ts-logbox\">{note}<pre id=\"ts-log\" class=\"ts-pre\" tabindex=\"0\" data-h=\"{h}\">{body}</pre></div>"
    )
}

async fn read_log(app: &Shared, who: &Caller) -> Result<Vec<String>, Fail> {
    crate::agent::get(app, who, &format!("/api/services/tailscale/log?lines={LOG_LINES}"))
        .await
        .map(|v| lines_of(&v))
}

#[derive(serde::Deserialize)]
struct LogQuery {
    #[serde(default)]
    h: String,
    #[serde(default)]
    force: String,
}

/// The tab asks for the log (every 4 s unless paused, or 刷新): nothing new = 204.
async fn log(State(app): State<Shared>, Query(q): Query<LogQuery>, req: Request) -> Response {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let who = Caller::of(&app, &req);
    let had = !q.h.is_empty();
    match read_log(&app, &who).await {
        Ok(ls) if had && q.force.is_empty() && hash_of(&ls) == q.h => StatusCode::NO_CONTENT.into_response(),
        Ok(ls) => one(log_box(ctx.lang, Some(&ls), None)).into_response(),
        // a log shown stays when a read fails; nothing shown yet says why
        Err(_) if had => StatusCode::NO_CONTENT.into_response(),
        Err(f) => one(log_box(ctx.lang, None, Some(&f.text(ctx.lang)))).into_response(),
    }
}

fn one(html: String) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(futures_util::stream::once(async move { Ok(patch(&html)) }))
}

// ---- the page ----

#[derive(Template)]
#[template(path = "tailscale.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    leaves: Vec<(String, String)>,
    log: String,
}

impl Page {
    fn leaf(&self, id: &str) -> &str {
        self.leaves
            .iter()
            .find(|(k, _)| k == id)
            .map_or("", |(_, h)| h.as_str())
    }
}

async fn page(State(app): State<Shared>, req: Request) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let who = Caller::of(&app, &req);
    let mut p = Poll::new(who.clone(), ctx.lang);
    let (_, lg) = tokio::join!(p.fetch(&app), read_log(&app, &who));
    let log = match &lg {
        Ok(ls) => log_box(ctx.lang, Some(ls), None),
        Err(f) => log_box(ctx.lang, None, Some(&f.text(ctx.lang))),
    };
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
        leaves: p.leaves(),
        log,
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

/// 刷新 / Retry: read again now.
async fn refresh(State(app): State<Shared>) -> StatusCode {
    let _ = app.kick.send(PATH);
    StatusCode::NO_CONTENT
}

pub fn routes() -> Router<Shared> {
    Router::new()
        .route(PATH, get(page))
        .route("/services/tailscale/events", get(events))
        .route("/services/tailscale/refresh", post(refresh))
        .route("/services/tailscale/log", post(log))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_790_000_000;

    fn sample() -> Value {
        json!({
            "installed": true, "running": true, "backend_state": "Running", "version": "1.88.1",
            "auth_url": null,
            "self": {"hostname": "u60-pro", "dns_name": "u60-pro.example.ts.net.", "ips": ["100.101.12.34", "fd7a::1"],
                     "online": true, "relay": "tok", "exit_node_option": false, "key_expiry": null, "primary_routes": ["192.168.0.0/24"]},
            "exit_node": null, "peer_count": 2, "peer_online": 1, "peer_active": 1, "peer_direct": 1,
            "peers": [
                {"id": "x", "hostname": "<b>nas</b>", "os": "linux", "ips": ["100.88.14.2"], "online": true, "exit_node": true,
                 "rx_bytes": 2048, "tx_bytes": null, "last_handshake": iso(NOW - 30), "active": true, "cur_addr": "1.2.3.4:5", "relay": "tok", "primary_routes": []},
                {"hostname": "phone", "ips": [], "online": false, "exit_node": false, "last_handshake": "0001-01-01T00:00:00Z"},
            ],
        })
    }

    fn poll(l: Lang, v: Option<Value>) -> Poll {
        let mut p = Poll::new(
            Caller { token: Some(Some("t".into())), peer: None, sid: None },
            l,
        );
        p.data = v.map(|v| parse(&v).unwrap());
        p
    }

    fn leaf(p: &Poll, id: &str) -> String {
        p.leaves_at(NOW).into_iter().find(|(k, _)| k == id).unwrap().1
    }

    /// The ISO form of an epoch second (test helper: the inverse of `parse_rfc3339`).
    fn iso(t: i64) -> String {
        let (days, rem) = (t.div_euclid(86_400), t.rem_euclid(86_400));
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe + era * 400 + i64::from(m <= 2);
        format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem / 60 % 60, rem % 60)
    }

    #[test]
    fn rfc3339_and_relative_times() {
        assert_eq!(parse_rfc3339("1970-01-02T00:00:00Z"), Some(86_400));
        assert_eq!(parse_rfc3339(&iso(NOW)), Some(NOW));
        assert_eq!(parse_rfc3339("2026-09-24T14:02:11.123+08:00"), parse_rfc3339("2026-09-24T06:02:11Z"));
        assert_eq!(parse_rfc3339("nope"), None);
        let r = |d: i64| relative(&iso(NOW - d), NOW, Lang::En);
        assert_eq!(r(30).as_deref(), Some("30s ago"));
        assert_eq!(r(3600).as_deref(), Some("1h ago"));
        assert_eq!(r(2 * 86_400).as_deref(), Some("2d ago"));
        assert_eq!(r(-6 * 3600).as_deref(), Some("just now"));
        assert_eq!(r(-20 * 3600), None);
        assert_eq!(relative("0001-01-01T00:00:00Z", NOW, Lang::En), None);
        assert_eq!(fmt_bytes(None), "—");
        assert_eq!(fmt_bytes(Some(1536.0)), "1.5 KB");
    }

    #[test]
    fn an_error_reply_is_a_failure_not_data() {
        assert_eq!(parse(&json!({"installed": true, "running": false, "error": "no socket"})), Err("no socket".into()));
        assert!(parse(&json!({"installed": false, "running": false})).is_ok());
    }

    #[test]
    fn running_status_shows_peers_and_everything_escaped() {
        let p = poll(Lang::En, Some(sample()));
        let st = leaf(&p, "ts-status");
        assert!(st.contains("nd-status--ok") && st.contains("Running") && st.contains("2 total · 1 online"), "{st}");
        assert!(st.contains("u60-pro.example.ts.net."), "{st}");
        let n = leaf(&p, "ts-node");
        assert!(n.contains("100.101.12.34") && n.contains("fd7a::1") && n.contains("192.168.0.0/24") && n.contains("Never (expiry off)"), "{n}");
        let pe = leaf(&p, "ts-peers");
        assert!(pe.contains("&lt;b&gt;nas&lt;/b&gt;") && !pe.contains("<b>nas"), "{pe}");
        assert!(pe.contains("↑— ↓2.0 KB") && pe.contains("handshake") && pe.contains(">direct<") && pe.contains("exit node"), "{pe}");
        assert!(pe.contains("Offline") && pe.contains("phone"), "{pe}");
        let m = leaf(&p, "ts-mesh");
        assert!(m.contains("1 · 1 direct · 0 relay") && m.contains("No exit node selected."), "{m}");
        let z = leaf(&poll(Lang::Zh, Some(sample())), "ts-mesh");
        assert!(z.contains("1 直连") && z.contains("未选择出口节点。"), "{z}");
    }

    #[test]
    fn key_expiry_warns_and_the_failure_greys() {
        let mut v = sample();
        v["self"]["key_expiry"] = json!(iso(NOW + 5 * 86_400 + 3600));
        let p = poll(Lang::En, Some(v.clone()));
        let st = leaf(&p, "ts-status");
        assert!(st.contains("nd-status--warn") && st.contains("Key expires in 5 days"), "{st}");
        assert!(leaf(&p, "ts-node").contains("5 days left"));
        v["self"]["key_expiry"] = json!(iso(NOW - 3600));
        assert!(leaf(&poll(Lang::En, Some(v)), "ts-status").contains("Key expired"));
        let mut p = poll(Lang::En, Some(sample()));
        p.invalid = Some("failed to connect".into());
        p.at = Some(SystemTime::now());
        let st = leaf(&p, "ts-status");
        assert!(st.contains("nd-stale") && st.contains("Tailscale: failed to connect") && st.contains("Retry") && st.contains("role=\"alert\""), "{st}");
        assert!(leaf(&p, "ts-node").contains("nd-stale"));
        let mut n = poll(Lang::En, None);
        n.invalid = Some("e".into());
        assert!(leaf(&n, "ts-status").contains("Can&#39;t read") || leaf(&n, "ts-status").contains("Can't read"));
    }

    #[test]
    fn not_installed_and_login_card() {
        let p = poll(Lang::En, Some(json!({"installed": false, "running": false})));
        let st = leaf(&p, "ts-status");
        assert!(st.contains("isn&#39;t installed") || st.contains("isn't installed"), "{st}");
        assert_eq!(leaf(&p, "ts-node"), "<div id=\"ts-node\"></div>");
        let mut v = sample();
        v["auth_url"] = json!("https://login.tailscale.com/a/\"x\"");
        let l = leaf(&poll(Lang::En, Some(v)), "ts-login");
        assert!(l.contains("href=\"https://login.tailscale.com/a/&quot;x&quot;\"") && l.contains("Login required"), "{l}");
        let mut v = sample();
        v["auth_url"] = json!("javascript:alert(1)");
        assert!(!leaf(&poll(Lang::En, Some(v)), "ts-login").contains("href="));
    }

    #[test]
    fn the_log_is_escaped_capped_and_hashed() {
        let lines: Vec<Value> = (0..300).map(|i| json!(format!("l{i} <script>"))).collect();
        let ls = lines_of(&json!({"lines": lines}));
        assert_eq!(ls.len(), LOG_LINES);
        assert_eq!(ls[0], "l100 <script>");
        let h = log_box(Lang::En, Some(&ls), None);
        assert!(h.contains("&lt;script&gt;") && !h.contains("<script>") && h.contains("data-h=\""), "{}", &h[..200]);
        let long = lines_of(&json!({"lines": ["é".repeat(3000)]}));
        assert!(long[0].len() <= LINE_MAX + 4);
        assert!(log_box(Lang::En, Some(&[]), None).contains("No log lines yet."));
        assert!(log_box(Lang::Zh, None, Some("x")).contains("读不到日志：x"));
    }
}
