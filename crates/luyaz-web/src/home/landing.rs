//! The home page's landing card (docs/designs/travel-assist.md §5): after arriving in a new
//! country, one card with what matters on landing — where, which network, local time, the home
//! exit, data used since.
//!
//! ```text
//!  poll  GET /v2/usage/landing (core usage_log.rs, 60 s) → {mcc, from, at, rx, tx} | null
//!        GET /api/public/status (clock.utc_offset, only while the card shows)
//!  leaf  hm-land (hidden: no landing, `from` null = the first record, older than 24 h, dismissed)
//!  write POST /home/landing/dismiss?at= (tier 1, lweb only: kept in memory until a restart)
//! ```
//! Local time: the network's time zone (`uci_device_info.nitz_*`); without NITZ sync, a
//! single-zone country's own zone marked 「估计」; otherwise not shown.

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Instant;

use axum::Router;
use axum::extract::{Query, Request, State};
use axum::response::IntoResponse;
use axum::routing::post;
use serde::Deserialize;
use serde_json::Value;

use super::Src;
use crate::agent::{self, Caller, esc};
use crate::i18n::Lang;
use crate::pages::Shared;
use crate::write;

pub(crate) const STATUS: &str = "/usage/landing";
/// The card shows this long after the landing.
const SHOW_FOR: i64 = 86_400;

/// The landing (`at`) the user dismissed.
static DISMISSED: AtomicI64 = AtomicI64::new(0);

pub(super) fn routes(r: Router<Shared>) -> Router<Shared> {
    r.route("/home/landing/dismiss", post(dismiss))
}

pub(super) fn frame() -> String {
    hidden()
}

fn hidden() -> String {
    "<section id=\"hm-land\" hidden></section>".into()
}

/// What the card needs besides the landing record.
pub(crate) struct Now<'a> {
    /// Device clock (seconds; the same clock as the core's `at`).
    pub t: i64,
    /// `clock.utc_offset` of the device clock (`None`: not read).
    pub utc_offset: Option<i64>,
    pub signal: &'a Value,
    pub device: &'a Value,
    /// The core's `/api/services/chill` (`None`: not read).
    pub chill: Option<&'a Value>,
    /// CHILL is installed (without it the "Home exit" row, CHILL's, is left out).
    pub chill_installed: bool,
}

fn country(mcc: i64, l: Lang) -> String {
    let iso = u16::try_from(mcc).ok().and_then(luyaz_api::netinfo::mcc_iso);
    match iso.and_then(luyaz_api::netinfo::iso_names) {
        Some((zh, en)) => l.pick(en, zh).to_owned(),
        None => format!("MCC {mcc}"),
    }
}

/// Hours east of UTC of a single-zone country (an estimate when the network gives none).
fn zone_of(mcc: i64) -> Option<f64> {
    let iso = u16::try_from(mcc).ok().and_then(luyaz_api::netinfo::mcc_iso)?;
    Some(match iso {
        "CN" | "HK" | "MO" | "TW" | "SG" | "MY" | "PH" => 8.0,
        "JP" | "KR" => 9.0,
        "TH" | "VN" => 7.0,
        "IN" => 5.5,
        "AE" => 4.0,
        "SA" | "QA" | "TR" => 3.0,
        "IL" | "GR" | "FI" => 2.0,
        "DE" | "FR" | "IT" | "ES" | "NL" | "BE" | "CH" | "AT" | "DK" | "SE" | "NO" => 1.0,
        "GB" | "IE" | "PT" => 0.0,
        _ => return None,
    })
}

/// "14:05 (UTC+9)" for local time, and whether it is an estimate.
fn local_time(n: &Now, mcc: i64, l: Lang) -> Option<String> {
    let off = n.utc_offset?;
    let num = |k: &str| n.device[k].as_str().and_then(|v| v.trim().parse::<f64>().ok());
    let (zone, guess) = match (n.device["nitz_sync_flag"].as_str().map(str::trim), num("nitz_timezone")) {
        (Some("1"), Some(z)) => (z + num("nitz_time_dst").unwrap_or(0.0), false),
        _ => (zone_of(mcc)?, true),
    };
    let secs = n.t - off + (zone * 3600.0).round() as i64;
    let m = secs.rem_euclid(86_400) / 60;
    let z = if zone.fract() == 0.0 { format!("{zone:+}") } else { format!("{:+}:{:02}", zone.trunc(), (zone.fract().abs() * 60.0).round()) };
    let mut s = format!("{:02}:{:02} (UTC{z})", m / 60, m % 60);
    if guess {
        s.push_str(l.pick(" · estimate", " · 估计"));
    }
    Some(s)
}

/// The home exit in one word (chill_page/data.rs's reading of the same status).
fn home_exit(h: Option<&Value>, l: Lang) -> (&'static str, &'static str) {
    let Some(h) = h.filter(|h| h.is_object()) else {
        return (l.pick("Not read", "读不到"), "neutral");
    };
    if h["home"].is_null() && h["tailscale"].is_null() {
        return (l.pick("Not set up", "未配置"), "neutral");
    }
    if h["home"]["ok"].as_bool() == Some(false) || h["tailscale"] == "offline" {
        return (l.pick("Down", "不可用"), "bad");
    }
    (l.pick("Up", "可用"), "ok")
}

fn ago(secs: i64, l: Lang) -> String {
    let m = (secs / 60).max(0);
    match (m, l) {
        (0..=59, Lang::En) => format!("{m} min ago"),
        (0..=59, _) => format!("{m} 分钟前"),
        (_, Lang::En) => format!("{} h ago", m / 60),
        _ => format!("{} 小时前", m / 60),
    }
}

fn row(label: &str, value: &str, tone: Option<&str>) -> String {
    let v = match tone {
        Some(t) => format!("<span class=\"nd-mark nd-mark--{t}\"><span>{}</span></span>", esc(value)),
        None => esc(value),
    };
    format!(
        "<div class=\"nd-row\"><span class=\"nd-row__text\"><span class=\"nd-row__label\">{}</span></span><span class=\"nd-row__value\" data-data>{v}</span></div>",
        esc(label)
    )
}

/// The card for the core's landing record (`None`: not read yet).
pub(crate) fn card(l: Lang, land: Option<&Value>, n: &Now) -> String {
    let Some(d) = land.filter(|d| d.is_object()) else {
        return hidden();
    };
    let (Some(mcc), Some(from), Some(at)) = (d["mcc"].as_i64(), d["from"].as_i64(), d["at"].as_i64()) else {
        return hidden();
    };
    if n.t - at >= SHOW_FOR || n.t < at - 300 || DISMISSED.load(Ordering::Relaxed) == at {
        return hidden();
    }
    let title = match l {
        Lang::En => format!("Arrived in {}", country(mcc, l)),
        _ => format!("到了{}", country(mcc, l)),
    };
    let sub = match l {
        Lang::En => format!("From {} · {}", country(from, l), ago(n.t - at, l)),
        _ => format!("从{}来 · {}", country(from, l), ago(n.t - at, l)),
    };
    let sig = n.signal;
    let op = sig["operator"].as_str().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("—");
    let roam = match sig["true_roaming"].as_bool() {
        Some(true) => l.pick("roaming", "漫游"),
        Some(false) => l.pick("local SIM", "当地卡"),
        None => "",
    };
    let net = [op, roam, sig["type"].as_str().unwrap_or("")]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" · ");
    let used = d["rx"].as_f64().unwrap_or(0.0) + d["tx"].as_f64().unwrap_or(0.0);
    let (exit, tone) = home_exit(n.chill, l);
    let mut rows = row(l.pick("Network", "网络"), &net, None);
    if let Some(t) = local_time(n, mcc, l) {
        rows.push_str(&row(l.pick("Local time", "当地时间"), &t, None));
    }
    if n.chill_installed {
        rows.push_str(&row(l.pick("Home exit", "家里出口"), exit, Some(tone)));
    }
    rows.push_str(&row(
        l.pick("Data since landing", "落地以来流量"),
        &agent::bytes(Some(used)).unwrap_or_else(|| "—".into()),
        None,
    ));
    format!(
        "<section id=\"hm-land\" class=\"hm-o1 hm-land\" aria-labelledby=\"hm-land-t\"><div class=\"nd-group\">\
<div class=\"nd-row hm-land__head\"><span class=\"nd-row__text\"><span id=\"hm-land-t\" class=\"nd-row__label\" data-data>{}</span><span class=\"nd-row__sub\" data-data>{}</span></span>\
<button type=\"button\" class=\"nd-btn nd-btn--ghost nd-btn--sm\" data-on:click=\"{}\">{}</button></div>{rows}</div></section>",
        esc(&title),
        esc(&sub),
        write::post_once(&format!("/home/landing/dismiss?at={at}")),
        l.pick("Dismiss", "收起"),
    )
}

pub(super) struct Landing {
    src: Src<Value>,
    status: Src<Value>,
}

impl Landing {
    pub(super) fn new() -> Landing {
        Landing { src: Src::new(60), status: Src::new(300) }
    }

    pub(super) fn next(&self) -> Instant {
        self.src.next()
    }

    fn landed(&self) -> bool {
        self.src.val.as_ref().is_some_and(|v| v["from"].is_i64())
    }

    pub(super) async fn poll(&mut self, app: &Shared, who: &Caller, now: Instant, force: bool) {
        if force || self.src.due(now) {
            let r = agent::get(app, who, STATUS).await.ok();
            self.src.set(Instant::now(), r);
        }
        if self.landed() && (force || self.status.due(now)) {
            let r = agent::get(app, who, "/api/public/status").await.ok();
            self.status.set(Instant::now(), r);
        }
    }

    pub(super) fn leaves(&self, app: &Shared, l: Lang, chill: Option<&Value>, chill_installed: bool) -> Vec<(String, String)> {
        let html = app.hub.read(|st| {
            let data = |k: &str| st.blocks.get(k).map_or(&Value::Null, |b| &b.data);
            let n = Now {
                t: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64),
                utc_offset: self.status.val.as_ref().and_then(|s| s["clock"]["utc_offset"].as_i64()),
                signal: data("signal"),
                device: data("uci_device_info"),
                chill,
                chill_installed,
            };
            card(l, self.src.val.as_ref(), &n)
        });
        vec![("hm-land".into(), html)]
    }
}

#[derive(Deserialize)]
struct DismissQuery {
    at: i64,
}

async fn dismiss(State(app): State<Shared>, Query(q): Query<DismissQuery>, _req: Request) -> impl IntoResponse {
    DISMISSED.store(q.at, Ordering::Relaxed);
    let _ = app.kick.send("/");
    let html = hidden();
    axum::response::sse::Sse::new(futures_util::stream::once(async move {
        Ok::<_, std::convert::Infallible>(crate::pages::patch(&html))
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn now<'a>(t: i64, sig: &'a Value, dev: &'a Value, chill: Option<&'a Value>) -> Now<'a> {
        Now { t, utc_offset: Some(28_800), signal: sig, device: dev, chill, chill_installed: true }
    }

    #[test]
    fn card_after_a_landing() {
        let sig = json!({"operator": "KDDI", "true_roaming": true, "type": "SA"});
        let dev = json!({"nitz_timezone": "9.00", "nitz_time_dst": "0.00", "nitz_sync_flag": "1"});
        let chill = json!({"home": {"ok": true, "ms": 40}, "tailscale": "direct"});
        let land = json!({"mcc": 440, "from": 460, "at": 1_000_000, "rx": 3_000_000, "tx": 500_000});
        // device clock 8 h ahead of UTC: 1_007_200 device = 978_400 UTC (07:46) → 16:46 JST
        let h = card(Lang::Zh, Some(&land), &now(1_007_200, &sig, &dev, Some(&chill)));
        assert!(h.contains("到了日本") && h.contains("从中国来 · 2 小时前"), "{h}");
        assert!(h.contains("KDDI · 漫游 · SA"), "{h}");
        assert!(h.contains("16:46 (UTC+9)") && !h.contains("估计"), "{h}");
        assert!(h.contains("可用") && h.contains("3.50 MB"), "{h}");
        // no NITZ: Japan's own zone, marked
        let h = card(Lang::En, Some(&land), &now(1_007_200, &sig, &json!({}), None));
        assert!(h.contains("16:46 (UTC+9) · estimate") && h.contains("Not read"), "{h}");
        // half-hour zones
        let dev_in = json!({"nitz_timezone": "5.50", "nitz_sync_flag": "1"});
        assert!(local_time(&now(1_007_200, &sig, &dev_in, None), 404, Lang::En).unwrap().starts_with("13:16 (UTC+5:30)"));
    }

    #[test]
    fn hidden_cases() {
        let (sig, dev) = (json!({}), json!({}));
        let first = json!({"mcc": 460, "from": null, "at": 100, "rx": 0, "tx": 0});
        assert!(card(Lang::Zh, Some(&first), &now(200, &sig, &dev, None)).contains("hidden"));
        let land = json!({"mcc": 440, "from": 460, "at": 100, "rx": 0, "tx": 0});
        assert!(!card(Lang::Zh, Some(&land), &now(200, &sig, &dev, None)).contains("hidden"));
        assert!(card(Lang::Zh, Some(&land), &now(100 + SHOW_FOR, &sig, &dev, None)).contains("hidden"));
        assert!(card(Lang::Zh, Some(&Value::Null), &now(200, &sig, &dev, None)).contains("hidden"));
        assert_eq!(home_exit(Some(&json!({"home": {"ok": false}})), Lang::Zh).0, "不可用");
        // no CHILL on the device: no "Home exit" row, the rest unchanged
        let mut n = now(200, &sig, &dev, None);
        n.chill_installed = false;
        let h = card(Lang::En, Some(&land), &n);
        assert!(!h.contains("Home exit") && h.contains("Data since landing"), "{h}");
        assert_eq!(home_exit(Some(&json!({"enabled": false})), Lang::Zh).0, "未配置");
    }
}
