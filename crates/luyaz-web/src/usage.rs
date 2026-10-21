//! /usage (流量): data used per device date and per SIM card (docs/designs/travel-assist.md §4).
//! The core keeps the ledger (`GET /v2/usage/days?n=30`, core usage_log.rs: `{days: [{day,
//! cards: [{card, tail, op, rx, tx}]}]}`, newest first, today included); this page reads it once
//! per load and draws one bar a day, stacked by card, with the same numbers as a table below.
//! A card keeps its colour on the page by its place among the cards sorted by id.

use std::time::Duration;

use askama::Template;
use axum::extract::State;
use axum::response::Response;
use serde_json::Value;

use crate::agent::esc;
use crate::i18n::Lang;
use crate::pages::Shared;

pub const PATH: &str = "/usage";
const DAYS: usize = 30;
/// Series colours (tokens): the fifth card on reuses the last.
const COLOURS: [&str; 4] = ["--nd-accMark", "--nd-okMark", "--nd-warnMark", "--nd-t3"];

#[derive(Template)]
#[template(path = "usage.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    main: String,
}

pub async fn page(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let who = crate::agent::Caller::of(&app, &req);
    let got = tokio::time::timeout(
        Duration::from_secs(3),
        crate::agent::get(&app, &who, &format!("/usage/days?n={DAYS}")),
    )
    .await;
    let reply = got.ok().and_then(Result::ok);
    let shell = app.hub.read(|st| crate::shell::Shell::new(&headers, PATH).live(st, &ctx, None));
    let html = Page { shell, l: ctx.lang, main: main(reply.as_ref(), ctx.lang) }.render().unwrap();
    crate::assets::maybe_gzip(&headers, "text/html; charset=utf-8", html.into_bytes(), "no-store")
}

struct Card {
    id: String,
    name: String,
}

fn total(c: &Value) -> f64 {
    c["rx"].as_f64().unwrap_or(0.0) + c["tx"].as_f64().unwrap_or(0.0)
}

fn size(n: f64) -> String {
    crate::agent::bytes(Some(n)).unwrap_or_else(|| "—".into())
}

/// The cards in the reply, sorted by id (their colour order), with their label.
fn cards(days: &[Value], l: Lang) -> Vec<Card> {
    let mut out: Vec<Card> = Vec::new();
    for c in days.iter().flat_map(|d| d["cards"].as_array().map_or(&[][..], Vec::as_slice)) {
        let Some(id) = c["card"].as_str() else { continue };
        if out.iter().any(|k| k.id == id) {
            continue;
        }
        let op = op_word(c["op"].as_str().unwrap_or(""), l);
        let tail = c["tail"].as_str().unwrap_or("");
        let name = match (op, tail) {
            ("—", "") => l.pick("Unknown card", "未知的卡").to_owned(),
            ("—", t) => format!("···{t}"),
            (o, "") => o.to_owned(),
            (o, t) => format!("{o} · {t}"),
        };
        out.push(Card { id: id.to_owned(), name });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// The core writes the SIM's own operator name ("China Telecom"); the home page's words cover
/// the short codes.
fn op_word(op: &str, l: Lang) -> &str {
    let code = match op {
        "China Mobile" => "CMCC",
        "China Unicom" => "CUCC",
        "China Telecom" => "CTCC",
        "China Broadnet" => "CBN",
        o => o,
    };
    crate::home_dual::op_word(code, l)
}

fn colour(i: usize) -> &'static str {
    COLOURS[i.min(COLOURS.len() - 1)]
}

fn row(label: &str, value: &str) -> String {
    format!(
        "<div class=\"nd-row\"><span class=\"nd-row__text\"><span class=\"nd-row__label\" data-data>{}</span></span><span class=\"nd-row__value\" data-data>{}</span></div>",
        esc(label),
        esc(value)
    )
}

fn swatch(i: usize) -> String {
    format!("<i class=\"us-sw\" style=\"background:var({})\" aria-hidden=\"true\"></i>", colour(i))
}

/// The page body for the core's `data` (`None`: could not read it).
pub fn main(reply: Option<&Value>, l: Lang) -> String {
    let Some(r) = reply else {
        return format!("<div class=\"nd-group\">{}</div>", row(l.pick("Could not read the usage log", "读不到流量记录"), ""));
    };
    let days: &[Value] = r["days"].as_array().map_or(&[], Vec::as_slice);
    if days.is_empty() {
        return format!(
            "<div class=\"nd-group\">{}</div>",
            row(l.pick("Nothing recorded yet — the count starts once the device clock is trusted", "还没有记录：设备时间可信后开始记"), "")
        );
    }
    let cards = cards(days, l);
    let idx = |id: &str| cards.iter().position(|c| c.id == id).unwrap_or(0);
    // ---- the chart: oldest on the left ----
    let top = days.iter().map(|d| d["cards"].as_array().map_or(0.0, |a| a.iter().map(total).sum())).fold(1.0, f64::max);
    let bars: String = days
        .iter()
        .rev()
        .map(|d| {
            let day = d["day"].as_str().unwrap_or("");
            let list = d["cards"].as_array().map_or(&[][..], Vec::as_slice);
            let mut segs: Vec<(usize, f64)> = list.iter().filter_map(|c| Some((idx(c["card"].as_str()?), total(c)))).collect();
            segs.sort_by_key(|s| s.0);
            let sum: f64 = segs.iter().map(|s| s.1).sum();
            let inner: String = segs
                .iter()
                .filter(|s| s.1 > 0.0)
                .map(|&(i, v)| format!("<i style=\"height:{:.1}%;background:var({})\"></i>", v / top * 100.0, colour(i)))
                .collect();
            format!(
                "<span class=\"us-bar\" title=\"{} · {}\">{inner}</span>",
                esc(day),
                esc(&size(sum))
            )
        })
        .collect();
    let first = days.last().and_then(|d| d["day"].as_str()).unwrap_or("");
    let last = days.first().and_then(|d| d["day"].as_str()).unwrap_or("");
    let short = |d: &str| d.get(5..).unwrap_or(d).to_owned();
    let legend: String = cards
        .iter()
        .enumerate()
        .map(|(i, c)| format!("<span class=\"us-key\">{}<span data-data>{}</span></span>", swatch(i), esc(&c.name)))
        .collect();
    let chart = format!(
        "<section class=\"t-card us-card\" aria-labelledby=\"us-chart-t\"><div class=\"tr-head\"><h2 id=\"us-chart-t\" class=\"tr-title\">{}</h2><span class=\"tr-now\">{} {}</span></div>\
<div class=\"us-bars\" role=\"img\" aria-label=\"{}\">{bars}</div><div class=\"t-axis\"><span>{}</span><span>{}</span></div><div class=\"us-legend\">{legend}</div></section>",
        l.pick("Per day", "每天"),
        l.pick("Max", "最多"),
        esc(&size(top)),
        l.pick("Data used per day, by card; the numbers are in the table below", "每天用的流量，按卡分色；数字见下表"),
        esc(&short(first)),
        esc(&short(last)),
    );
    // ---- totals per card over the window, then the days ----
    let sums: String = cards
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let (rx, tx) = days
                .iter()
                .flat_map(|d| d["cards"].as_array().map_or(&[][..], Vec::as_slice))
                .filter(|k| k["card"].as_str() == Some(c.id.as_str()))
                .fold((0.0, 0.0), |(a, b), k| (a + k["rx"].as_f64().unwrap_or(0.0), b + k["tx"].as_f64().unwrap_or(0.0)));
            format!(
                "<div class=\"nd-row\"><span class=\"nd-row__text\"><span class=\"nd-row__label\">{}<span data-data>{}</span></span><span class=\"nd-row__sub\" data-data>↓ {} · ↑ {}</span></span><span class=\"nd-row__value\" data-data>{}</span></div>",
                swatch(i),
                esc(&c.name),
                esc(&size(rx)),
                esc(&size(tx)),
                esc(&size(rx + tx))
            )
        })
        .collect();
    let n = days.len();
    let sums_t = match l {
        Lang::En => format!("By card, last {n} days"),
        _ => format!("按卡合计（近 {n} 天）"),
    };
    let day_rows: String = days
        .iter()
        .map(|d| {
            let list = d["cards"].as_array().map_or(&[][..], Vec::as_slice);
            let sub: Vec<String> = list
                .iter()
                .map(|c| {
                    let name = cards.iter().find(|k| Some(k.id.as_str()) == c["card"].as_str()).map_or("", |k| k.name.as_str());
                    format!("{name} {}", size(total(c)))
                })
                .collect();
            let sum: f64 = list.iter().map(total).sum();
            let sub = if list.len() > 1 { format!("<span class=\"nd-row__sub\" data-data>{}</span>", esc(&sub.join(" · "))) } else { String::new() };
            let label = match (list.len(), list.first()) {
                (1, Some(c)) => {
                    let name = cards.iter().find(|k| Some(k.id.as_str()) == c["card"].as_str()).map_or("", |k| k.name.as_str());
                    format!("{} <span class=\"nd-aux\" data-data>{}</span>", esc(d["day"].as_str().unwrap_or("")), esc(name))
                }
                _ => esc(d["day"].as_str().unwrap_or("")),
            };
            format!(
                "<div class=\"nd-row\"><span class=\"nd-row__text\"><span class=\"nd-row__label\" data-data>{label}</span>{sub}</span><span class=\"nd-row__value\" data-data>{}</span></div>",
                esc(&size(sum))
            )
        })
        .collect();
    format!(
        "{chart}<section aria-labelledby=\"us-sum-t\"><h2 id=\"us-sum-t\" class=\"nd-group-title\">{}</h2><div class=\"nd-group us-list\">{sums}</div></section>\
<section aria-labelledby=\"us-days-t\"><h2 id=\"us-days-t\" class=\"nd-group-title\">{}</h2><div class=\"nd-group us-list\">{day_rows}</div></section>",
        esc(&sums_t),
        l.pick("Days", "逐日"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reply() -> Value {
        json!({"days": [
            {"day": "2026-10-21", "cards": [
                {"card": "bbbb", "tail": "3668", "op": "China Unicom", "rx": 2_000_000, "tx": 500_000},
                {"card": "aaaa", "tail": "9403", "op": "China Telecom", "rx": 1_000_000, "tx": 0}]},
            {"day": "2026-10-20", "cards": [
                {"card": "aaaa", "tail": "9403", "op": "China Telecom", "rx": 4_000_000, "tx": 1_000_000}]}
        ]})
    }

    #[test]
    fn colours_follow_the_card_not_the_day() {
        let r = reply();
        let c = cards(r["days"].as_array().unwrap(), Lang::Zh);
        assert_eq!(c.iter().map(|k| k.id.as_str()).collect::<Vec<_>>(), ["aaaa", "bbbb"]);
        assert_eq!(c[0].name, "中国电信 · 9403");
        assert_eq!(colour(0), "--nd-accMark");
        assert_eq!(colour(9), "--nd-t3");
    }

    #[test]
    fn page_body() {
        let h = main(Some(&reply()), Lang::Zh);
        // the 10-20 bar is the tallest (5 MB) = 100 %
        assert!(h.contains("height:100.0%;background:var(--nd-accMark)"), "{h}");
        assert!(h.contains("10-20</span><span>10-21"));
        assert!(h.contains("按卡合计（近 2 天）"));
        // telecom over two days: 5 MB + 1 MB
        assert!(h.contains("6.00 MB"));
        // the two-card day lists both
        assert!(h.contains("中国联通 · 3668 2.50 MB · 中国电信 · 9403 1.00 MB"), "{h}");
        assert!(main(None, Lang::En).contains("Could not read the usage log"));
        assert!(main(Some(&json!({"days": []})), Lang::Zh).contains("还没有记录"));
    }
}
