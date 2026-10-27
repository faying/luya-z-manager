//! /clients (已连设备): status first (count · live / "list stopped at
//! hh:mm" · retry), then the lease list, expired leases behind a toggle. Read-only.
//!
//! Both reads are the agent's (`/api/network/clients` = luci-rpc host hints + DHCP leases + iw
//! stations; `/api/netinfo?lite=1&clients=1` = per-client traffic, kept fresh only while someone
//! asks), every 5 s per tab with the tab's own session (agent.rs). A reply without
//! `dhcp_leases` is a failure (R9: getDHCPLeases failed, the agent still says ok), so it can't
//! read as "no devices". Lease expiry is on the device clock; "now" is netinfo's `now` (same clock).
//! The expired toggle is a Datastar signal in the page (no server state), so pushes keep it.
//!
//! LAN client identification (lan-client-id.md T5, core 1.21.0): `/api/lanid` (u60d lanid.rs)
//! is read with the other two and is extra like the traffic (an older core without it, or a
//! failed read, leaves the page as before). Per row: the winning name in the core's order
//! (alias > mDNS > DHCP: lease host name / host hint, then lanid's > NetBIOS), vendor, kind, a
//! 「私有地址」 tag (locally administered MAC while lanid is loaded, the core's rule), and a
//! details panel (`$cld` = the open MAC, a page signal so pushes keep it) with every source.
//! Alias, tier 2: Save ─▶ `alias/ask` (the draft `_cla` in the body, kept under a random token
//! server-side) ─▶ inline confirm ─▶ `alias/set?t=` ─▶ `POST /api/lanid/alias {mac, alias}`
//! ("" removes) ─▶ read back `aliases[mac]` from `/api/lanid`. The area is outside the pushed list.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use askama::Template;
use axum::extract::{Query, State};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::Stream;
use serde_json::{Value, json};

use crate::agent::{Caller, Fail, Polled, bytes, mbps, stopped};
use crate::i18n::Lang;
use crate::pages::{Shared, StreamQuery, patch, split_leaves};
use crate::topbar::Via;
use crate::write::{self, Outcome, Phase};

const PATH: &str = "/clients";
const EVERY: Duration = Duration::from_secs(5);

/// One value of one field and where it came from (`GET /api/lanid` `sources`), for the details.
pub struct Src {
    pub field: &'static str,
    pub source: String,
    pub value: String,
}

pub struct Row {
    pub name: String,
    /// lowercase `aa:bb:…` (the core's key), "" without a MAC
    pub mac: String,
    pub alias: String,
    /// the old MAC the alias was taken over from (lanid watch.rs: same device, new random MAC)
    pub alias_from: String,
    pub vendor: Option<String>,
    pub kind: Option<&'static str>,
    pub private: bool,
    pub sources: Vec<Src>,
    pub wifi: Option<(String, String)>,
    /// The details panel's Wi-Fi link, every field always there (「读不到」 when the driver didn't
    /// say): (label, value, read).
    pub link: Vec<(&'static str, String, bool)>,
    pub not_on_wifi: bool,
    pub traffic: Option<String>,
    pub addr: String,
    pub left: String,
    pub expired: bool,
}

pub enum List {
    Loading,
    Failed,
    Empty,
    Rows { active: Vec<Row>, expired: Vec<Row> },
}

pub struct View {
    pub l: Lang,
    pub tone: &'static str,
    pub state: String,
    pub reason: Option<String>,
    /// Some(None) = live; Some(Some(line)) = reconnecting, list stopped at …
    pub meta: Option<Option<String>>,
    pub retry: bool,
    pub signed_out: bool,
    pub stale: bool,
    pub list: List,
}

impl Row {
    /// The MAC without colons, for element ids.
    pub fn slug(&self) -> String {
        self.mac.replace(':', "")
    }
}

impl View {
    pub fn symbol(&self) -> &'static str {
        match self.tone {
            "warn" => "▲",
            "bad" => "■",
            _ => "●",
        }
    }
}

#[derive(Default)]
struct Got {
    clients: Option<Value>,
    netinfo: Option<Value>,
    last_ok: Option<SystemTime>,
    err: Option<Fail>,
    no_table: bool,
    /// the last good `/api/lanid` data
    lanid: Option<Value>,
}

pub struct Poll {
    who: Caller,
    l: Lang,
    got: Got,
}

fn sig_word(tier: &str, l: Lang) -> Option<&'static str> {
    Some(match tier {
        "great" => l.pick("Strong signal", "信号很好"),
        "good" => l.pick("Good signal", "信号好"),
        "fair" => l.pick("Fair signal", "信号一般"),
        "weak" => l.pick("Weak signal", "信号弱"),
        _ => return None,
    })
}

fn wifi_line(st: &Value, l: Lang) -> (String, String) {
    let mut parts: Vec<String> = Vec::new();
    if let Some(b) = st["band"].as_str().filter(|b| !b.is_empty()) {
        parts.push(b.into());
    }
    if let Some(g) = st["wifi_gen"].as_u64().filter(|g| *g > 0) {
        parts.push(format!("Wi-Fi {g}"));
    }
    match (pos(st, "link_down_mbps"), pos(st, "link_up_mbps")) {
        (Some(d), Some(u)) => parts.push(format!("↓{d} ↑{u} Mbps")),
        (Some(d), None) => parts.push(format!("↓{d} Mbps")),
        (None, Some(u)) => parts.push(format!("↑{u} Mbps")),
        (None, None) => {}
    }
    if let Some(w) = st["signal_tier"].as_str().and_then(|t| sig_word(t, l)) {
        parts.push(w.into());
    }
    let line = if parts.is_empty() {
        l.pick("On Wi-Fi", "连着 Wi-Fi").into()
    } else {
        parts.join(" · ")
    };
    let title = match st["signal"].as_i64() {
        Some(s) => {
            let c = st["channel"]
                .as_u64()
                .map_or("—".to_string(), |c| c.to_string());
            match l {
                Lang::En => format!("{s} dBm · channel {c}"),
                Lang::Zh => format!("{s} dBm · 信道 {c}"),
            }
        }
        None => String::new(),
    };
    (line, title)
}

/// A field the core reads from `iw`; 0 is a driver that printed nothing useful, not a value.
fn pos(st: &Value, k: &str) -> Option<u64> {
    st[k].as_u64().filter(|n| *n > 0)
}

/// The details panel's four Wi-Fi fields (luyaz-api station_wifi_json). Nothing is guessed: a
/// rate line without MCS gives no generation, so the protocol then reads 「读不到」 too.
fn link_rows(st: &Value, l: Lang) -> Vec<(&'static str, String, bool)> {
    let none = || l.pick("Not reported", "读不到").to_string();
    let rate = |k| pos(st, k).map_or_else(none, |n| format!("{n} Mbps"));
    let (down, up) = (rate("link_down_mbps"), rate("link_up_mbps"));
    let rates_read = pos(st, "link_down_mbps").is_some() || pos(st, "link_up_mbps").is_some();
    let band = st["band"].as_str().filter(|b| !b.is_empty()).map(|b| {
        let mut s = b.to_string();
        if let Some(c) = pos(st, "channel") {
            s += &match l {
                Lang::En => format!(" · channel {c}"),
                Lang::Zh => format!(" · 信道 {c}"),
            };
        }
        if let Some(w) = pos(st, "width_mhz") {
            // the hotspot's width (iw dev … info), not this client's own
            s += &match l {
                Lang::En => format!(" · hotspot {w} MHz wide"),
                Lang::Zh => format!(" · 热点频宽 {w} MHz"),
            };
        }
        s
    });
    let signal = st["signal"].as_i64().map(|d| {
        match st["signal_tier"].as_str().and_then(|t| sig_word(t, l)) {
            Some(w) => format!("{d} dBm · {w}"),
            None => format!("{d} dBm"),
        }
    });
    let gen_ = pos(st, "wifi_gen").map(|g| {
        let std = match g {
            7 => " (802.11be)",
            6 => " (802.11ax)",
            5 => " (802.11ac)",
            4 => " (802.11n)",
            _ => "",
        };
        format!("Wi-Fi {g}{std}")
    });
    let opt = |v: Option<String>| (v.is_some(), v.unwrap_or_else(none));
    let (b_ok, band) = opt(band);
    let (s_ok, signal) = opt(signal);
    let (g_ok, gen_) = opt(gen_);
    vec![
        (
            l.pick("Link rate (down / up)", "协商速率（下行 / 上行）"),
            format!("↓ {down} · ↑ {up}"),
            rates_read,
        ),
        (l.pick("Band", "频段"), band, b_ok),
        (l.pick("Signal", "信号强度"), signal, s_ok),
        (l.pick("Wi-Fi standard", "Wi-Fi 协议"), gen_, g_ok),
    ]
}

fn traffic_line(tr: &Value, l: Lang) -> Option<String> {
    let mut parts = Vec::new();
    if let (Some(d), Some(u)) = (mbps(tr["down_rate"].as_f64()), mbps(tr["up_rate"].as_f64())) {
        parts.push(format!("↓ {d} ↑ {u} Mbps"));
    }
    let (db, ub) = (tr["down_bytes"].as_f64(), tr["up_bytes"].as_f64());
    if db.is_some() || ub.is_some() {
        if let Some(t) = bytes(Some(db.unwrap_or(0.0) + ub.unwrap_or(0.0))) {
            parts.push(match l {
                Lang::En => format!("{t} since joined"),
                Lang::Zh => format!("连上后共 {t}"),
            });
        }
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn lease_left(secs: i64, l: Lang) -> String {
    if secs <= 0 {
        return l.pick("Expired", "已过期").into();
    }
    let (h, m) = (secs / 3600, secs % 3600 / 60);
    match (l, h) {
        (Lang::En, 0) => format!("{m}m"),
        (Lang::En, _) => format!("{h}h {m}m"),
        (Lang::Zh, 0) => format!("{m} 分"),
        (Lang::Zh, _) => format!("{h} 小时 {m} 分"),
    }
}

fn same_mac(a: &Value, mac: &str) -> bool {
    a["mac"]
        .as_str()
        .is_some_and(|m| m.eq_ignore_ascii_case(mac))
}

fn kind_word(k: &str, l: Lang) -> Option<&'static str> {
    Some(match k {
        "phone" => l.pick("Phone", "手机"),
        "tablet" => l.pick("Tablet", "平板"),
        "laptop" => l.pick("Laptop", "笔记本电脑"),
        "computer" => l.pick("Computer", "电脑"),
        "tv" => l.pick("TV", "电视"),
        "speaker" => l.pick("Speaker", "音箱"),
        "watch" => l.pick("Watch", "手表"),
        "printer" => l.pick("Printer", "打印机"),
        "iot" => l.pick("Smart home device", "智能家居设备"),
        _ => return None,
    })
}

fn field_word(f: &str, l: Lang) -> &'static str {
    match f {
        "name" => l.pick("Name", "名称"),
        "vendor" => l.pick("Maker", "厂商"),
        _ => l.pick("Type", "类型"),
    }
}

/// How a source is called in the details.
pub fn source_word(s: &str, l: Lang) -> String {
    match s {
        "alias" => l.pick("Your alias", "你设的别名").into(),
        "dhcp" => "DHCP".into(),
        "mdns" => "mDNS".into(),
        "ssdp" => "SSDP".into(),
        "netbios" => "NetBIOS".into(),
        "oui" => l.pick("MAC prefix (OUI)", "MAC 前缀（OUI）").into(),
        o => o.into(),
    }
}

/// `aa:bb:cc:dd:ee:ff` from any casing or `-`; None when it is not a MAC.
pub fn mac_key(s: &str) -> Option<String> {
    let h: String = s.chars().filter(|c| *c != ':' && *c != '-').collect();
    (h.len() == 12 && h.chars().all(|c| c.is_ascii_hexdigit())).then(|| {
        h.to_ascii_lowercase()
            .as_bytes()
            .chunks(2)
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect::<Vec<_>>()
            .join(":")
    })
}

/// The core's check of an alias (lanid.rs `valid_value`, after trim): at most 64 bytes, no
/// control characters; "" (remove) is fine.
pub fn alias_ok(a: &str) -> bool {
    a.len() <= 64 && !a.chars().any(char::is_control)
}

/// Name, alias, vendor, kind, private, details of one MAC, from the lease's own name and lanid.
#[allow(clippy::type_complexity)]
fn identify(
    lanid: Option<&Value>,
    mac: &str,
    lease_name: Option<String>,
    l: Lang,
) -> (
    Option<String>,
    String,
    Option<String>,
    Option<&'static str>,
    bool,
    Vec<Src>,
) {
    let key = mac_key(mac);
    let loaded = lanid.is_some_and(|d| d["status"]["loaded"].as_bool() == Some(true));
    let private = loaded
        && key
            .as_deref()
            .and_then(|k| u8::from_str_radix(&k[..2], 16).ok())
            .is_some_and(|b| b & 2 != 0);
    let m = key
        .as_deref()
        .and_then(|k| lanid.map(|d| &d["merged"][k]))
        .filter(|m| m.is_object());
    let mut raw: Vec<(String, String, String)> = m
        .and_then(|m| m["sources"].as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| {
                    Some((
                        s["field"].as_str()?.to_owned(),
                        s["source"].as_str()?.to_owned(),
                        s["value"].as_str().filter(|v| !v.is_empty())?.to_owned(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    // the lease's own name is a DHCP name too: in the core's order, after alias and mDNS
    if let Some(n) = &lease_name
        && !raw
            .iter()
            .any(|(f, s, v)| f == "name" && s == "dhcp" && v == n)
    {
        let at = raw
            .iter()
            .position(|(f, s, _)| f == "name" && s != "alias" && s != "mdns")
            .or_else(|| raw.iter().position(|(f, _, _)| f != "name"))
            .unwrap_or(raw.len());
        raw.insert(at, ("name".into(), "dhcp".into(), n.clone()));
    }
    let alias = m
        .and_then(|m| m["client"]["alias"].as_str())
        .unwrap_or("")
        .to_owned();
    let name = raw
        .iter()
        .find(|(f, _, _)| f == "name")
        .map(|(_, _, v)| v.clone());
    let first = |f: &str| {
        raw.iter()
            .find(|(x, _, _)| x == f)
            .map(|(_, _, v)| v.clone())
    };
    let vendor = first("vendor");
    let kind = first("kind").and_then(|k| kind_word(&k, l));
    let sources = raw
        .iter()
        .map(|(f, s, v)| Src {
            field: field_word(f, l),
            source: source_word(s, l),
            value: if f == "kind" {
                kind_word(v, l).map_or_else(|| v.clone(), str::to_owned)
            } else {
                v.clone()
            },
        })
        .collect();
    (name, alias, vendor, kind, private, sources)
}

/// The old MAC whose alias `mac` took over ("" when it did not).
fn alias_from(lanid: Option<&Value>, mac: &str) -> String {
    mac_key(mac)
        .and_then(|k| {
            lanid?["merged"][k.as_str()]["client"]["alias_from"]
                .as_str()
                .map(str::to_owned)
        })
        .unwrap_or_default()
}

fn view(g: &Got, l: Lang) -> View {
    let signed_out = g.err == Some(Fail::SignedOut);
    let stale = g.err.is_some() || g.no_table;
    let Some(d) = &g.clients else {
        let failed = g.err.is_some() || g.no_table;
        return View {
            l,
            tone: if failed { "bad" } else { "neutral" },
            state: if failed {
                l.pick("Couldn't read the device list", "读取失败").into()
            } else {
                l.pick("Reading the device list…", "正在读取设备列表…")
                    .into()
            },
            reason: failed.then(|| reason(g, l)),
            meta: None,
            retry: failed && !signed_out,
            signed_out,
            stale: false,
            list: if failed { List::Failed } else { List::Loading },
        };
    };
    let leases = d["dhcp_leases"].as_array().cloned().unwrap_or_default();
    let ni = g.netinfo.as_ref();
    // the device clock; without netinfo, our own (luyaz-web runs on the device: same clock)
    let now = ni.and_then(|n| n["now"].as_i64()).unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64)
    });
    let wifi = d["wifi"].as_array();
    let traffic = ni.and_then(|n| n["clients"]["list"].as_array());
    let hosts = &d["hosts"];
    let mut active = Vec::new();
    let mut expired = Vec::new();
    for le in &leases {
        let mac = le["macaddr"].as_str().unwrap_or("");
        let exp = le["expires"].as_i64().unwrap_or(0);
        // 0 = no expiry known: this dnsmasq writes the lease length, not the end time
        // (lease file reads 86400 on CN B31), so luci-rpc reports 0 for every lease
        let is_active = exp == 0 || exp > now;
        let hint = [mac.to_string(), mac.to_lowercase(), mac.to_uppercase()]
            .iter()
            .find_map(|k| hosts[k.as_str()]["name"].as_str().filter(|n| !n.is_empty()))
            .map(str::to_owned);
        let lease_name = le["hostname"]
            .as_str()
            .filter(|n| !n.is_empty())
            .map(str::to_owned)
            .or(hint);
        let (name, alias, vendor, kind, private, sources) =
            identify(g.lanid.as_ref(), mac, lease_name, l);
        let name = name.unwrap_or_else(|| l.pick("Unknown device", "未知设备").into());
        let st = (!mac.is_empty())
            .then(|| wifi.and_then(|w| w.iter().find(|s| same_mac(s, mac))))
            .flatten();
        let tr = (!mac.is_empty())
            .then(|| traffic.and_then(|t| t.iter().find(|s| same_mac(s, mac))))
            .flatten();
        let ip = le["ipaddr"].as_str().unwrap_or("—");
        let row = Row {
            name,
            mac: mac_key(mac).unwrap_or_default(),
            alias_from: alias_from(g.lanid.as_ref(), mac),
            alias,
            vendor,
            kind,
            private,
            sources,
            wifi: st.map(|s| wifi_line(s, l)),
            link: st.map(|s| link_rows(s, l)).unwrap_or_default(),
            not_on_wifi: st.is_none() && is_active && wifi.is_some(),
            traffic: tr.and_then(|t| traffic_line(t, l)),
            addr: if mac.is_empty() {
                ip.into()
            } else {
                format!("{ip} · {mac}")
            },
            left: if exp == 0 {
                "—".into()
            } else {
                lease_left(exp.saturating_sub(now), l)
            },
            expired: !is_active,
        };
        if is_active {
            active.push(row);
        } else {
            expired.push(row);
        }
    }
    let state = if leases.is_empty() {
        l.pick("No devices connected right now", "现在没有设备连着")
            .into()
    } else {
        match l {
            Lang::En => format!("{} total · {} active leases", leases.len(), active.len()),
            Lang::Zh => format!("共 {} 台 · {} 个有效租约", leases.len(), active.len()),
        }
    };
    View {
        l,
        tone: if stale { "stale" } else { "ok" },
        state,
        reason: stale.then(|| reason(g, l)),
        meta: Some(stale.then(|| stopped(l, "List", "列表", g.last_ok))),
        retry: stale && !signed_out,
        signed_out,
        stale,
        list: if leases.is_empty() {
            List::Empty
        } else {
            List::Rows { active, expired }
        },
    }
}

fn reason(g: &Got, l: Lang) -> String {
    match &g.err {
        Some(e) => e.text(l),
        None => l
            .pick(
                "The device didn't return its DHCP lease table.",
                "设备没有返回 DHCP 租约表。",
            )
            .into(),
    }
}

#[derive(Template)]
#[template(path = "clients_leaves.html")]
struct Leaves<'a> {
    v: &'a View,
}

impl Poll {
    pub fn new(who: Caller, l: Lang) -> Poll {
        Poll {
            who,
            l,
            got: Got::default(),
        }
    }

    async fn fetch(&mut self, app: &Shared) {
        let (c, n, d) = tokio::join!(
            crate::agent::get(app, &self.who, "/api/network/clients"),
            crate::agent::get(app, &self.who, "/api/netinfo?lite=1&clients=1"),
            crate::agent::get(app, &self.who, "/api/lanid"),
        );
        // identification is extra too: keep the last answer (an older core has no /api/lanid)
        if let Ok(d) = d
            && d["status"].is_object()
        {
            self.got.lanid = Some(d);
        }
        match c {
            Ok(v) if v["dhcp_leases"].is_array() => {
                self.got.clients = Some(v);
                self.got.last_ok = Some(SystemTime::now());
                self.got.err = None;
                self.got.no_table = false;
            }
            Ok(_) => {
                self.got.err = None;
                self.got.no_table = true;
            }
            Err(e) => self.got.err = Some(e),
        }
        // traffic is extra: keep the last answer when it fails
        if let Ok(n) = n {
            self.got.netinfo = Some(n);
        }
    }

    fn leaves(&self) -> Vec<(String, String)> {
        let v = view(&self.got, self.l);
        split_leaves(&Leaves { v: &v }.render().unwrap())
    }
}

impl Polled for Poll {
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        self.fetch(app).await;
        (self.leaves(), EVERY)
    }
}

#[derive(Template)]
#[template(path = "clients.html")]
struct Page {
    shell: crate::shell::Shell,
    leaves: Vec<(String, String)>,
    alias_op: String,
}

impl Page {
    fn leaf(&self, id: &str) -> &str {
        self.leaves
            .iter()
            .find(|(k, _)| k == id)
            .map_or("", |(_, h)| h.as_str())
    }
}

pub async fn page(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let mut p = Poll::new(Caller::of(&app, &req), ctx.lang);
    p.fetch(&app).await;
    let shell = app.hub.read(|st| {
        crate::shell::Shell::new(&headers, PATH).live(
            st,
            &ctx,
            Some(format!("{PATH}/events?g={}", st.generation)),
        )
    });
    let html = Page {
        shell,
        leaves: p.leaves(),
        alias_op: alias_area(ctx.lang, ctx.remote, "", &Phase::Idle),
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
    let p = Poll::new(Caller::of(&app, &req), ctx.lang);
    crate::agent::stream(app, ctx, q.g, PATH, p)
}

/// Refresh / Retry: every /clients stream reads now.
pub async fn refresh(State(app): State<Shared>) -> axum::http::StatusCode {
    let _ = app.kick.send(PATH);
    axum::http::StatusCode::NO_CONTENT
}

// ---- alias (tier 2) ----

const ALIAS_CANCEL: &str = "/clients/alias/cancel";
const STASH_TTL: Duration = Duration::from_secs(600);

fn alias_area(l: Lang, remote: Via, set: &str, phase: &Phase) -> String {
    write::Area {
        id: "cl-alias",
        l,
        remote,
        set,
        cancel: ALIAS_CANCEL,
        phase,
    }
    .html()
}

fn one(html: String) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(futures_util::stream::once(async move { Ok(patch(&html)) }))
}

/// What an ask confirmed: (MAC, alias, the device's shown name), under a random token, so the
/// alias never travels in a URL.
type Stash = Mutex<HashMap<String, (Instant, String, String, String)>>;

fn stash() -> &'static Stash {
    static S: std::sync::OnceLock<Stash> = std::sync::OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

fn put(mac: String, alias: String, name: String) -> String {
    let t = crate::auth::random_id();
    let mut m = stash().lock().unwrap();
    m.retain(|_, (at, ..)| at.elapsed() < STASH_TTL);
    m.insert(t.clone(), (Instant::now(), mac, alias, name));
    t
}

fn take(t: &str) -> Option<(String, String)> {
    stash()
        .lock()
        .unwrap()
        .remove(t)
        .filter(|(at, ..)| at.elapsed() < STASH_TTL)
        .map(|(_, m, a, _)| (m, a))
}

fn bad_alias(l: Lang) -> Outcome {
    Outcome::Failed(
        l.pick(
            "An alias is at most 64 bytes (about 21 Chinese characters), without control characters.",
            "别名最多 64 字节（约 21 个汉字），不能有控制字符。",
        )
        .into(),
    )
}

pub fn alias_confirm(alias: &str, name: &str, l: Lang) -> write::Confirm {
    let (title, what, action) = if alias.is_empty() {
        (
            l.pick("Remove the alias", "删除别名"),
            match l {
                Lang::En => format!("“{name}” goes back to the name the device reports itself."),
                Lang::Zh => format!("「{name}」改回设备自己报的名字。"),
            },
            l.pick("Confirm: remove the alias", "确认：删除别名"),
        )
    } else {
        (
            l.pick("Save the alias", "保存别名"),
            match l {
                Lang::En => format!(
                    "“{name}” is shown as “{alias}” here and on the touch screen. Only this router keeps it; the device itself is not renamed."
                ),
                Lang::Zh => format!(
                    "「{name}」在这里和触屏上显示为「{alias}」。只存在本机，不会改设备自己的名字。"
                ),
            },
            l.pick("Confirm: save the alias", "确认：保存别名"),
        )
    };
    write::Confirm {
        tier: write::Tier::Inline,
        title: title.into(),
        what,
        downtime: None,
        recovery: None,
        action: action.into(),
        cuts_uplink: false,
        word: None,
        danger: false,
        revert: write::Revert::No,
    }
}

#[derive(serde::Deserialize, Default)]
pub struct AliasIn {
    #[serde(default, rename = "_cla")]
    alias: String,
    /// the name shown now (for the confirm's words only)
    #[serde(default, rename = "_cln")]
    name: String,
}

#[derive(serde::Deserialize)]
pub struct AskQuery {
    #[serde(default)]
    mac: String,
}

/// 「保存」: the inline confirm (or why the draft cannot be saved).
pub async fn alias_ask(
    Query(q): Query<AskQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let (l, r) = (ctx.lang, ctx.remote);
    let body = axum::body::to_bytes(req.into_body(), 16 * 1024)
        .await
        .unwrap_or_default();
    let f: AliasIn = serde_json::from_slice(&body).unwrap_or_default();
    let alias = f.alias.trim().to_owned();
    let Some(mac) = mac_key(&q.mac) else {
        return one(alias_area(l, r, "", &Phase::Idle));
    };
    if !alias_ok(&alias) {
        return one(alias_area(l, r, "", &Phase::Done(bad_alias(l))));
    }
    let name: String = f.name.trim().chars().take(64).collect();
    let c = alias_confirm(&alias, if name.is_empty() { &mac } else { &name }, l);
    let t = put(mac, alias, name);
    one(alias_area(
        l,
        r,
        &format!("/clients/alias/set?t={t}"),
        &Phase::Ask(c),
    ))
}

#[derive(serde::Deserialize)]
pub struct SetQuery {
    #[serde(default)]
    t: String,
}

/// The stored alias of `mac` in `/api/lanid` ("" = none); None when it could not be read.
async fn read_alias(app: &Shared, who: &Caller, mac: &str) -> Option<String> {
    let d = crate::agent::get(app, who, "/api/lanid").await.ok()?;
    let a = d["aliases"].as_object()?;
    Some(
        a.iter()
            .find(|(k, _)| mac_key(k).as_deref() == Some(mac))
            .and_then(|(_, v)| v.as_str())
            .unwrap_or("")
            .to_owned(),
    )
}

async fn alias_outcome(app: &Shared, who: &Caller, l: Lang, mac: &str, alias: &str) -> Outcome {
    let sent = crate::agent::post_raw(
        app,
        who,
        "/api/lanid/alias",
        &json!({"mac": mac, "alias": alias}),
    )
    .await;
    let stored_mac = match sent {
        Ok((c, v)) if (200..300).contains(&c) && v["ok"].as_bool() != Some(false) => v["mac"]
            .as_str()
            .and_then(mac_key)
            .unwrap_or_else(|| mac.to_owned()),
        Ok((c, v)) => return Outcome::Failed(crate::agent::refused(c, &v).text(l)),
        Err(Fail::Unreachable) => return Outcome::Unknown,
        Err(f) => return Outcome::Failed(f.text(l)),
    };
    match read_alias(app, who, &stored_mac).await {
        Some(a) if a == alias => Outcome::Applied(
            if alias.is_empty() {
                l.pick("Alias removed", "别名已删除")
            } else {
                l.pick("Alias saved", "别名已保存")
            }
            .into(),
        ),
        _ => Outcome::Accepted,
    }
}

pub async fn alias_set(
    State(app): State<Shared>,
    Query(q): Query<SetQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let (l, r) = (ctx.lang, ctx.remote);
    let who = Caller::of(&app, &req);
    let pending = alias_area(
        l,
        r,
        "",
        &Phase::Pending(l.pick("Saving…", "正在保存…").into()),
    );
    let got = take(&q.t);
    write::respond(pending, async move {
        let o = match got {
            Some((mac, alias)) => alias_outcome(&app, &who, l, &mac, &alias).await,
            None => Outcome::NotYet(
                l.pick(
                    "This confirm has expired; press Save again.",
                    "这个确认已过期，请重新点「保存」。",
                )
                .into(),
            ),
        };
        let _ = app.kick.send(PATH);
        alias_area(l, r, "", &Phase::Done(o))
    })
}

pub async fn alias_cancel(
    Query(q): Query<write::CancelQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    one(alias_area(
        ctx.lang,
        ctx.remote,
        "",
        &write::cancel_phase(&q),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn got() -> Got {
        Got {
            clients: Some(json!({
                "hosts": {"AA:BB:CC:DD:EE:01": {"name": "hint-name"}},
                "dhcp_leases": [
                    {"expires": 1000 + 3 * 3600 + 120, "hostname": "", "macaddr": "aa:bb:cc:dd:ee:01", "ipaddr": "192.168.0.10"},
                    {"expires": 1000 + 600, "hostname": "phone", "macaddr": "aa:bb:cc:dd:ee:02", "ipaddr": "192.168.0.11"},
                    {"expires": 900, "hostname": "old", "macaddr": "aa:bb:cc:dd:ee:03", "ipaddr": "192.168.0.12"}],
                "wifi": [{"mac": "AA:BB:CC:DD:EE:01", "band": "5 GHz", "wifi_gen": 6, "link_down_mbps": 2402,
                          "signal": -39, "signal_tier": "great", "channel": 44}]})),
            netinfo: Some(json!({"now": 1000, "clients": {"list": [
                {"mac": "aa:bb:cc:dd:ee:01", "down_bytes": 1_900_000_000u64, "up_bytes": 40_000_000u64,
                 "down_rate": 1_450_000, "up_rate": 37_500}]}})),
            last_ok: Some(SystemTime::now()),
            ..Default::default()
        }
    }

    #[test]
    fn rows_like_the_old_page() {
        let v = view(&got(), Lang::Zh);
        assert_eq!(v.state, "共 3 台 · 2 个有效租约");
        let List::Rows { active, expired } = &v.list else {
            panic!()
        };
        assert_eq!(active[0].name, "hint-name", "host hint by any MAC casing");
        assert_eq!(
            active[0].wifi.as_ref().unwrap().0,
            "5 GHz · Wi-Fi 6 · ↓2402 Mbps · 信号很好"
        );
        assert_eq!(
            active[0].traffic.as_deref(),
            Some("↓ 11.6 ↑ 0.30 Mbps · 连上后共 1.94 GB")
        );
        assert_eq!(active[0].left, "3 小时 2 分");
        assert_eq!(
            active[0].link,
            vec![
                ("协商速率（下行 / 上行）", "↓ 2402 Mbps · ↑ 读不到".to_string(), true),
                ("频段", "5 GHz · 信道 44".to_string(), true),
                ("信号强度", "-39 dBm · 信号很好".to_string(), true),
                ("Wi-Fi 协议", "Wi-Fi 6 (802.11ax)".to_string(), true),
            ]
        );
        assert!(active[1].not_on_wifi);
        assert!(active[1].link.is_empty(), "no Wi-Fi block off Wi-Fi");
        assert_eq!(active[1].left, "10 分");
        assert_eq!(expired[0].name, "old");
        assert!(!expired[0].not_on_wifi, "only active leases say so");
    }

    #[test]
    fn wifi_link_all_fields_and_none() {
        let full = json!({"band": "5 GHz", "channel": 149, "width_mhz": 80, "wifi_gen": 7,
            "link_down_mbps": 2882, "link_up_mbps": 1441, "signal": -71, "signal_tier": "fair"});
        assert_eq!(wifi_line(&full, Lang::Zh).0, "5 GHz · Wi-Fi 7 · ↓2882 ↑1441 Mbps · 信号一般");
        let v: Vec<String> = link_rows(&full, Lang::Zh).into_iter().map(|r| r.1).collect();
        assert_eq!(
            v,
            [
                "↓ 2882 Mbps · ↑ 1441 Mbps",
                "5 GHz · 信道 149 · 热点频宽 80 MHz",
                "-71 dBm · 信号一般",
                "Wi-Fi 7 (802.11be)"
            ]
        );
        // a driver that prints zeros / nothing: every field still there, each says so
        let bare = json!({"band": null, "channel": null, "wifi_gen": null, "link_down_mbps": 0,
            "link_up_mbps": null, "signal": null});
        let rows = link_rows(&bare, Lang::Zh);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].1, "↓ 读不到 · ↑ 读不到");
        assert!(rows.iter().all(|r| !r.2));
        assert!(rows[1..].iter().all(|r| r.1 == "读不到"));
        assert_eq!(link_rows(&bare, Lang::En)[3].1, "Not reported");
        assert_eq!(wifi_line(&bare, Lang::Zh).0, "连着 Wi-Fi");
    }

    #[test]
    fn expires_zero_is_unknown_not_expired() {
        // what the device sends (CN B31): every lease has expires 0
        let g = Got {
            clients: Some(json!({"dhcp_leases": [
                {"expires": 0, "hostname": "tv", "macaddr": "aa:bb:cc:dd:ee:04", "ipaddr": "192.168.0.5"}]})),
            netinfo: Some(json!({"now": 1000})),
            last_ok: Some(SystemTime::now()),
            ..Default::default()
        };
        let v = view(&g, Lang::Zh);
        assert_eq!(v.state, "共 1 台 · 1 个有效租约");
        let List::Rows { active, expired } = &v.list else {
            panic!()
        };
        assert!(expired.is_empty());
        assert_eq!(active[0].left, "—");
        assert!(!active[0].expired);
    }

    #[test]
    fn a_reply_without_the_lease_table_is_a_failure_not_an_empty_list() {
        let g = Got {
            no_table: true,
            ..Default::default()
        };
        let v = view(&g, Lang::Zh);
        assert_eq!(v.tone, "bad");
        assert!(matches!(v.list, List::Failed));
        assert!(v.reason.unwrap().contains("DHCP"));
        // with an older good answer: stale, the list stays
        let mut g = got();
        g.no_table = true;
        let v = view(&g, Lang::Zh);
        assert_eq!(v.tone, "stale");
        assert!(matches!(v.list, List::Rows { .. }));
        assert!(v.meta.unwrap().unwrap().starts_with("列表停在 "));
    }

    #[test]
    fn signed_out_says_so_and_offers_no_retry() {
        let mut g = got();
        g.err = Some(Fail::SignedOut);
        let v = view(&g, Lang::En);
        assert!(v.signed_out && !v.retry);
        let html = Leaves { v: &v }.render().unwrap();
        assert!(html.contains("/login?next=/clients"), "{html}");
    }

    #[test]
    fn the_english_page_has_no_chinese() {
        let mut g = got();
        let html = Leaves {
            v: &view(&g, Lang::En),
        }
        .render()
        .unwrap();
        g.clients = None;
        g.err = Some(Fail::Unreachable);
        let html2 = Leaves {
            v: &view(&g, Lang::En),
        }
        .render()
        .unwrap();
        for h in [html, html2] {
            assert!(
                !h.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)
                    || ('\u{3000}'..='\u{303f}').contains(&c)
                    || ('\u{ff00}'..='\u{ffef}').contains(&c)),
                "{h}"
            );
        }
    }

    fn lanid() -> Value {
        json!({"status": {"loaded": true}, "aliases": {"aa:bb:cc:dd:ee:02": "Mum's phone"},
            "merged": {
                "aa:bb:cc:dd:ee:01": {"client": {"mac": "aa:bb:cc:dd:ee:01"}, "sources": [
                    {"field": "name", "source": "mdns", "value": "Living room TV"},
                    {"field": "name", "source": "netbios", "value": "TV-01"},
                    {"field": "vendor", "source": "oui", "value": "Sony"},
                    {"field": "kind", "source": "mdns", "value": "tv"}]},
                "aa:bb:cc:dd:ee:02": {"client": {"mac": "aa:bb:cc:dd:ee:02", "alias": "Mum's phone"}, "sources": [
                    {"field": "name", "source": "alias", "value": "Mum's phone"}]}}})
    }

    #[test]
    fn identification_in_the_cores_order_with_every_source() {
        let mut g = got();
        g.lanid = Some(lanid());
        let v = view(&g, Lang::Zh);
        let List::Rows { active, expired } = &v.list else {
            panic!()
        };
        // mDNS beats the host hint; the hint is listed as DHCP, before NetBIOS
        assert_eq!(active[0].name, "Living room TV");
        assert_eq!(active[0].vendor.as_deref(), Some("Sony"));
        assert_eq!(active[0].kind, Some("电视"));
        let src: Vec<String> = active[0]
            .sources
            .iter()
            .map(|s| format!("{}/{}/{}", s.field, s.source, s.value))
            .collect();
        assert_eq!(
            src,
            [
                "名称/mDNS/Living room TV",
                "名称/DHCP/hint-name",
                "名称/NetBIOS/TV-01",
                "厂商/MAC 前缀（OUI）/Sony",
                "类型/mDNS/电视"
            ]
        );
        // the alias beats the lease's host name
        assert_eq!(active[1].name, "Mum's phone");
        assert_eq!(active[1].alias, "Mum's phone");
        assert!(active[0].private, "0xaa has the locally administered bit");
        assert_eq!(expired[0].name, "old", "no lanid record: as before");
        let html = Leaves { v: &v }.render().unwrap();
        assert!(html.contains("cl-d-aabbccddee01") && html.contains("Living room TV"));
    }

    #[test]
    fn alias_taken_over_from_an_old_mac_is_tagged() {
        let mut g = got();
        let mut l = lanid();
        l["merged"]["aa:bb:cc:dd:ee:02"]["client"]["alias_from"] = json!("aa:bb:cc:dd:ee:77");
        g.lanid = Some(l);
        let v = view(&g, Lang::Zh);
        let List::Rows { active, .. } = &v.list else {
            panic!()
        };
        assert_eq!(active[1].alias_from, "aa:bb:cc:dd:ee:77");
        assert!(active[0].alias_from.is_empty());
        let html = Leaves { v: &v }.render().unwrap();
        assert!(html.contains("新地址，沿用别名") && html.contains("aa:bb:cc:dd:ee:77"));
    }

    #[test]
    fn private_only_while_lanid_is_loaded() {
        let mut g = got();
        g.clients.as_mut().unwrap()["dhcp_leases"][0]["macaddr"] = json!("06:11:22:33:44:55");
        g.clients.as_mut().unwrap()["dhcp_leases"][1]["macaddr"] = json!("04:11:22:33:44:55");
        g.lanid = Some(json!({"status": {"loaded": true}, "merged": {}, "aliases": {}}));
        let List::Rows { active, .. } = view(&g, Lang::Zh).list else {
            panic!()
        };
        assert!(active[0].private && active[0].sources.is_empty());
        assert!(!active[1].private, "04 = a universal MAC");
        g.lanid = Some(json!({"status": {"loaded": false}, "merged": {}, "aliases": {}}));
        let List::Rows { active, .. } = view(&g, Lang::Zh).list else {
            panic!()
        };
        assert!(!active[0].private);
    }

    #[test]
    fn alias_checks_like_the_core() {
        assert!(alias_ok("") && alias_ok("客厅电视") && alias_ok(&"a".repeat(64)));
        assert!(!alias_ok(&"a".repeat(65)) && !alias_ok("a\tb") && !alias_ok(&"电".repeat(22)));
        assert_eq!(
            mac_key("AA-BB-CC-DD-EE-0F").as_deref(),
            Some("aa:bb:cc:dd:ee:0f")
        );
        assert_eq!(
            mac_key("aabbccddee0f").as_deref(),
            Some("aa:bb:cc:dd:ee:0f")
        );
        assert!(mac_key("aa:bb").is_none() && mac_key("zz:bb:cc:dd:ee:ff").is_none());
    }

    #[test]
    fn the_alias_confirms_say_what_changes() {
        let c = alias_confirm("客厅电视", "TV-01", Lang::Zh);
        assert_eq!(c.tier, write::Tier::Inline);
        assert!(c.what.contains("「TV-01」") && c.what.contains("「客厅电视」"));
        assert_eq!(c.action, "确认：保存别名");
        let c = alias_confirm("", "TV-01", Lang::En);
        assert_eq!(c.action, "Confirm: remove the alias");
    }

    #[test]
    fn the_english_identification_has_no_chinese() {
        let mut g = got();
        g.lanid = Some(lanid());
        let h = Leaves {
            v: &view(&g, Lang::En),
        }
        .render()
        .unwrap();
        assert!(h.contains("MAC prefix (OUI)") && h.contains(">TV<"), "{h}");
        assert!(
            !h.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
            "{h}"
        );
    }
}
