//! /clients (已连设备), the old `clients/page.tsx`: status first (count · live / "list stopped at
//! hh:mm" · retry), then the lease list, expired leases behind a toggle. Read-only.
//!
//! Both reads are the agent's (`/api/network/clients` = luci-rpc host hints + DHCP leases + iw
//! stations; `/api/netinfo?lite=1&clients=1` = per-client traffic, kept fresh only while someone
//! asks), every 5 s per tab with the tab's own session (agent.rs). A reply without
//! `dhcp_leases` is a failure (R9: getDHCPLeases failed, the agent still says ok), so it can't
//! read as "no devices". Lease expiry is on the device clock; "now" is netinfo's `now` (same clock).
//! The expired toggle is a Datastar signal in the page (no server state), so pushes keep it.

use std::time::{Duration, SystemTime};

use askama::Template;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use crate::agent::{Caller, Fail, Polled, bytes, mbps, stopped};
use crate::i18n::Lang;
use crate::pages::{Shared, StreamQuery, split_leaves};

const PATH: &str = "/clients";
const EVERY: Duration = Duration::from_secs(5);

pub struct Row {
    pub name: String,
    pub wifi: Option<(String, String)>,
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
    if let Some(n) = st["link_down_mbps"].as_u64().filter(|n| *n > 0) {
        parts.push(format!("{n} Mbps"));
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
        let name = le["hostname"]
            .as_str()
            .filter(|n| !n.is_empty())
            .map(str::to_owned)
            .or(hint)
            .unwrap_or_else(|| l.pick("Unknown device", "未知设备").into());
        let st = (!mac.is_empty())
            .then(|| wifi.and_then(|w| w.iter().find(|s| same_mac(s, mac))))
            .flatten();
        let tr = (!mac.is_empty())
            .then(|| traffic.and_then(|t| t.iter().find(|s| same_mac(s, mac))))
            .flatten();
        let ip = le["ipaddr"].as_str().unwrap_or("—");
        let row = Row {
            name,
            wifi: st.map(|s| wifi_line(s, l)),
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
        let (c, n) = tokio::join!(
            crate::agent::get(app, &self.who, "/api/network/clients"),
            crate::agent::get(app, &self.who, "/api/netinfo?lite=1&clients=1"),
        );
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
            "5 GHz · Wi-Fi 6 · 2402 Mbps · 信号很好"
        );
        assert_eq!(
            active[0].traffic.as_deref(),
            Some("↓ 11.6 ↑ 0.30 Mbps · 连上后共 1.94 GB")
        );
        assert_eq!(active[0].left, "3 小时 2 分");
        assert!(active[1].not_on_wifi);
        assert_eq!(active[1].left, "10 分");
        assert_eq!(expired[0].name, "old");
        assert!(!expired[0].not_on_wifi, "only active leases say so");
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
}
