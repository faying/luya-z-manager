//! /tools/speedtest/ladder: the load ladder's kept runs. One run in full with how its delay was
//! spread, a comparison with another run of the same plan, the list, and the run as SVG, PNG,
//! PDF (print) or JSON. Read only.
//!
//!  GET /tools/speedtest/ladder?id=&vs=  ─▶ core /api/ladder/result?id= (+ the other run), /history
//!  GET /tools/speedtest/ladder.svg?id=  ─▶ the run as a standalone SVG (PNG is drawn from it in
//!                                          the browser; nothing leaves the LAN)
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use askama::Template;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use serde_json::Value;
use std::collections::HashMap;

use crate::agent::Caller;
use crate::i18n::Lang;
use crate::pages::Shared;
use crate::speedtest::{ladder_headline, ladder_kind, ladder_of, LadderFrag, LADDER_CAP_MS};

const PATH: &str = "/tools/speedtest/ladder";

/// A step's name in a table, by what it loaded.
fn step_name(k: &str, l: Lang) -> &'static str {
    match k {
        "idle" => l.pick("Idle", "空闲"),
        "down" => l.pick("Video load", "看影片的负载"),
        "both" => l.pick("Plus upload", "再加上传"),
        "up" => l.pick("Upload only", "只上传"),
        _ => l.pick("After stopping", "停止后"),
    }
}

/// What the device saw where the run was: never a place name, it does not know one.
fn context_text(c: &Value, l: Lang) -> String {
    let mut out: Vec<String> = Vec::new();
    let rat = c["rat"].as_str().unwrap_or("");
    let band = c["band"].as_str().unwrap_or("");
    if !rat.is_empty() || !band.is_empty() {
        // the band may already carry the RAT ("LTE BAND 5"): 11-14 the record said "LTE LTE BAND 5"
        if !rat.is_empty() && band.starts_with(rat) {
            out.push(band.to_string());
        } else {
            out.push(format!("{rat} {band}").trim().to_string());
        }
    }
    if let Some(p) = c["pci"].as_i64().filter(|p| *p > 0) {
        out.push(format!("PCI {p}"));
    }
    if let Some(v) = c["rsrp"].as_f64() {
        out.push(format!("RSRP {v:.0} dBm"));
    }
    if let Some(v) = c["sinr"].as_f64() {
        out.push(format!("SINR {v:.0} dB"));
    }
    if let Some(n) = c["carriers"].as_i64().filter(|n| *n > 0) {
        out.push(match l {
            Lang::En => format!("{n} carrier{}", if n == 1 { "" } else { "s" }),
            Lang::Zh => format!("{n} 载波"),
        });
    }
    if c["roaming"] == true {
        out.push(l.pick("roaming", "漫游").into());
    }
    if let Some(t) = c["chip_c"].as_f64() {
        out.push(match l {
            Lang::En => format!("chip {t:.0} °C"),
            Lang::Zh => format!("芯片 {t:.0} °C"),
        });
    }
    out.join(" · ")
}

fn plan_name(p: &str, l: Lang) -> &'static str {
    match p {
        "roaming" => l.pick("Light (roaming)", "轻量（漫游）"),
        _ => l.pick("Full", "完整"),
    }
}

/// The probes of one step that counted, as `(sent, delay)`.
fn step_probes(r: &Value, i: usize) -> Vec<f64> {
    let s = &r["steps"][i];
    let (Some(a), Some(b)) = (s["from_ms"].as_f64(), s["to_ms"].as_f64()) else { return Vec::new() };
    r["samples"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|x| x[2].as_u64().is_some_and(|o| o <= 1))
        .filter_map(|x| Some((x[0].as_f64()?, x[1].as_f64()?)))
        .filter(|(t, _)| *t >= a && *t < b)
        .map(|(_, ms)| ms)
        .collect()
}

pub(crate) struct CdfLine {
    kind: &'static str,
    name: &'static str,
    points: String,
}
/// Each step's delays sorted: how much of the step stayed under each delay. A steep line on the
/// left is a step that stayed quick; one that leans right spent time waiting.
fn cdf(r: &Value, l: Lang) -> Vec<CdfLine> {
    let n = r["steps"].as_array().map_or(0, Vec::len);
    (0..n)
        .filter_map(|i| {
            let mut v = step_probes(r, i);
            if v.len() < 5 {
                return None;
            }
            v.sort_by(f64::total_cmp);
            let total = v.len() as f64;
            // at most about a point every 1%, so a long step does not bloat the page
            let every = (v.len() / 100).max(1);
            let mut pts: Vec<String> = Vec::new();
            for (j, ms) in v.iter().enumerate() {
                if j % every == 0 || j + 1 == v.len() {
                    let x = ms.min(LADDER_CAP_MS) / LADDER_CAP_MS * 600.0;
                    let y = 120.0 - (j + 1) as f64 / total * 120.0;
                    pts.push(format!("{x:.1},{y:.1}"));
                }
            }
            let k = ladder_kind(r, i as u64 + 1);
            let kind = match k {
                "idle" => "idle",
                "down" => "down",
                "both" => "both",
                "up" => "up",
                _ => "recovery",
            };
            Some(CdfLine { kind, name: step_name(k, l), points: pts.join(" ") })
        })
        .collect()
}

pub(crate) struct CompareRow {
    name: &'static str,
    this: String,
    that: String,
    change: String,
}
pub(crate) struct Compare {
    this_when: String,
    that_when: String,
    this_ctx: String,
    that_ctx: String,
    this_head: String,
    that_head: String,
    rows: Vec<CompareRow>,
    /// a different cell or band: the change may come from there
    note: Option<&'static str>,
    this_pts: String,
    that_pts: String,
    /// the two are of different plans or steps: shown side by side, not as changes
    unlike: bool,
}
/// Each run's probes over its own measured span, so two runs of the same plan line up.
fn delay_points(r: &Value) -> String {
    let steps = r["steps"].as_array().map(Vec::as_slice).unwrap_or_default();
    let from = steps.first().and_then(|s| s["from_ms"].as_f64());
    let to = steps.iter().filter_map(|s| s["to_ms"].as_f64()).fold(None, |m: Option<f64>, b| Some(m.map_or(b, |m| m.max(b))));
    let samples = r["samples"].as_array().map(Vec::as_slice).unwrap_or_default();
    let (from, to) = match (from, to) {
        (Some(a), Some(b)) if b > a => (a, b),
        _ => (0.0, samples.iter().filter_map(|s| s[0].as_f64()).fold(1.0, f64::max)),
    };
    samples
        .iter()
        .filter_map(|s| Some((s[0].as_f64()?, s[1].as_f64()?)))
        .filter(|(t, _)| *t >= from && *t <= to)
        .map(|(t, ms)| format!("{:.1},{:.1}", (t - from) / (to - from) * 600.0, 120.0 - ms.min(LADDER_CAP_MS) / LADDER_CAP_MS * 120.0))
        .collect::<Vec<_>>()
        .join(" ")
}
fn kinds(r: &Value) -> Vec<String> {
    let n = r["steps"].as_array().map_or(0, Vec::len) as u64;
    (1..=n).map(|i| ladder_kind(r, i).to_string()).collect()
}
fn compare(this: &Value, that: &Value, l: Lang) -> Compare {
    let (a, b) = (&this["report"], &that["report"]);
    let unlike = a["plan"] != b["plan"] || kinds(a) != kinds(b);
    let ms = |s: &Value| s["stats"]["p50_ms"].as_f64();
    let rate = |s: &Value| {
        let mut v = Vec::new();
        if let Some(d) = s["down_mbps"].as_f64() {
            v.push(format!("↓{d:.0}"));
        }
        if let Some(u) = s["up_mbps"].as_f64() {
            v.push(format!("↑{u:.0}"));
        }
        if v.is_empty() { String::new() } else { format!(" · {} Mbps", v.join(" ")) }
    };
    let cell = |s: &Value, rec: Option<f64>, k: &str| -> String {
        if k == "recovery" {
            return rec.map_or("--".into(), |v| match l {
                Lang::En => format!("{:.1} s", v / 1000.0),
                Lang::Zh => format!("{:.1} 秒", v / 1000.0),
            });
        }
        ms(s).map_or("--".into(), |v| format!("{v:.0} ms{}", rate(s)))
    };
    let rec = |r: &Value| r["findings"]["recovery_ms"].as_f64();
    let n = a["steps"].as_array().map_or(0, Vec::len).max(b["steps"].as_array().map_or(0, Vec::len));
    let rows = (0..n)
        .map(|i| {
            let k = ladder_kind(a, i as u64 + 1);
            let k = if k.is_empty() { ladder_kind(b, i as u64 + 1) } else { k };
            let (sa, sb) = (&a["steps"][i], &b["steps"][i]);
            let change = if unlike {
                String::new()
            } else if k == "recovery" {
                match (rec(a), rec(b)) {
                    (Some(x), Some(y)) => format!("{:+.1} s", (x - y) / 1000.0),
                    _ => String::new(),
                }
            } else {
                match (ms(sa), ms(sb)) {
                    (Some(x), Some(y)) => format!("{:+.0} ms", x - y),
                    _ => String::new(),
                }
            };
            CompareRow { name: step_name(k, l), this: cell(sa, rec(a), k), that: cell(sb, rec(b), k), change }
        })
        .collect();
    let (ca, cb) = (&this["context"], &that["context"]);
    let moved = ca.is_object() && cb.is_object() && (ca["pci"] != cb["pci"] || ca["band"] != cb["band"]);
    let when = |v: &Value| v["id"].as_i64().map(crate::alerts_page::mdhm).unwrap_or_default();
    Compare {
        this_when: when(this),
        that_when: when(that),
        this_ctx: context_text(ca, l),
        that_ctx: context_text(cb, l),
        this_head: ladder_headline(a, l),
        that_head: ladder_headline(b, l),
        rows,
        note: moved.then(|| {
            l.pick(
                "The two runs were on different cells or bands: the change may come from the cell, not only the spot.",
                "两次不在同一个小区或频段：差别可能来自小区，不只是位置。",
            )
        }),
        this_pts: delay_points(a),
        that_pts: delay_points(b),
        unlike,
    }
}

pub(crate) struct HistRow {
    href: String,
    vs_href: String,
    when: String,
    plan: &'static str,
    ctx: String,
    head: String,
    current: bool,
    compared: bool,
}

#[derive(Template)]
#[template(path = "speedtest_ladder_page.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    status: String,
    run: String,
    id: Option<u64>,
    cdf: Vec<CdfLine>,
    compare: Option<Compare>,
    history: Vec<HistRow>,
    svg_href: String,
    json_href: String,
}

/// The verdict's tone: unanswering is bad, slowing or queueing is warn, no conclusion (stopped,
/// no baseline) is neutral, holding up is ok.
fn tone(r: &Value) -> &'static str {
    let f = &r["findings"];
    if r["aborted"].is_string() || f["no_baseline"] == true {
        "neutral"
    } else if f["unresponsive_steps"][0].is_u64() {
        "bad"
    } else if f["first_slow_step"].is_u64() {
        "warn"
    } else {
        "ok"
    }
}

/// The page-top status block (11-14 ③): the shown run's verdict, its time and cell as meta.
fn status(body: Option<&Value>, l: Lang) -> String {
    let Some(b) = body else {
        return crate::status_block::html("ld-status", "bad", l.pick("Could not read the runs", "读不到负载体检记录"), None, None);
    };
    let Some(w) = Some(&b["result"]).filter(|w| w.is_object()) else {
        return crate::status_block::html("ld-status", "neutral", l.pick("No run yet", "还没有记录"), None, None);
    };
    let r = &w["report"];
    let meta = [
        w["id"].as_u64().map(|i| crate::alerts_page::mdhm(i as i64)).unwrap_or_default(),
        context_text(&w["context"], l),
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join(" · ");
    crate::status_block::html(
        "ld-status",
        tone(r),
        &crate::speedtest::ladder_headline(r, l),
        Some(esc(l.pick("This moment only.", "只代表此刻。")).as_str()),
        Some(esc(&meta).as_str()),
    )
}

async fn result(app: &Shared, who: &Caller, id: Option<u64>) -> Option<Value> {
    let path = id.map_or("/api/ladder/result".to_string(), |i| format!("/api/ladder/result?id={i}"));
    crate::agent::get(app, who, &path).await.ok()
}
fn num(q: &HashMap<String, String>, k: &str) -> Option<u64> {
    q.get(k).and_then(|v| v.parse().ok())
}

pub async fn page(
    State(app): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
    req: axum::extract::Request,
) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let who = Caller::of(&app, &req);
    let shell = app.hub.read(|st| crate::shell::Shell::new(&headers, PATH).live(st, &ctx, None));
    let body = result(&app, &who, num(&q, "id")).await;
    let w = body.as_ref().map(|b| &b["result"]).filter(|w| w.is_object());
    let id = w.and_then(|w| w["id"].as_u64());
    // the other run: the one asked for, or the last one like this
    let vs = num(&q, "vs").or_else(|| w.and_then(|w| w["prev"]["id"].as_u64()));
    let other = match vs.filter(|v| Some(*v) != id) {
        Some(v) => result(&app, &who, Some(v)).await,
        None => None,
    };
    let compare = match (w, other.as_ref().map(|o| &o["result"]).filter(|o| o.is_object())) {
        (Some(a), Some(b)) => Some(compare(a, b, l)),
        _ => None,
    };
    let compared = compare.is_some().then_some(vs).flatten();
    let list = crate::agent::get(&app, &who, "/api/ladder/history").await.ok();
    let history = list
        .as_ref()
        .and_then(|v| v["runs"].as_array())
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|r| {
            let rid = r["id"].as_u64()?;
            let report = serde_json::json!({
                "aborted": r["aborted"], "findings": r["findings"],
                "notes": r["notes"].as_array().map(|a| a.iter().map(|c| serde_json::json!({"code": c})).collect::<Vec<_>>()),
                "steps": r["steps"].as_array().map(|a| a.iter().map(|s| serde_json::json!({"kind": s["kind"], "stats": {"p50_ms": s["p50_ms"]}, "down_mbps": s["down_mbps"], "up_mbps": s["up_mbps"]})).collect::<Vec<_>>()),
            });
            Some(HistRow {
                href: format!("{PATH}?id={rid}"),
                vs_href: id.map_or(String::new(), |cur| format!("{PATH}?id={cur}&vs={rid}")),
                when: crate::alerts_page::mdhm(rid as i64),
                plan: plan_name(r["plan"].as_str().unwrap_or(""), l),
                ctx: context_text(&r["context"], l),
                head: ladder_headline(&report, l),
                current: Some(rid) == id,
                compared: Some(rid) == compared,
            })
        })
        .collect();
    let mut v = ladder_of(body.as_ref(), l);
    v.here = true;
    let run = LadderFrag { v: &v }.render().unwrap_or_default();
    let q_id = id.map_or(String::new(), |i| format!("?id={i}"));
    let html = Page {
        shell,
        l,
        status: status(body.as_ref(), l),
        run,
        id,
        cdf: w.map(|w| cdf(&w["report"], l)).unwrap_or_default(),
        compare,
        history,
        svg_href: format!("{PATH}.svg{q_id}"),
        json_href: format!("/tools/speedtest/ladder.json{q_id}"),
    }
    .render()
    .unwrap();
    crate::assets::maybe_gzip(&headers, "text/html; charset=utf-8", html.into_bytes(), "no-store")
}

/// Text for SVG: the five characters XML needs escaped.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

/// The run as one standalone SVG, colours inline (the page's CSS variables do not travel).
pub(crate) fn svg(w: &Value, l: Lang) -> String {
    const INK: &str = "#17171c";
    const T3: &str = "#5b5b66";
    const HAIR: &str = "#deded9";
    const ACC: &str = "#1558c7";
    const OK: &str = "#1a7a3a";
    const WARN: &str = "#a35300";
    const BAD: &str = "#c8101a";
    let r = &w["report"];
    let mut o = String::new();
    let width = 800.0;
    let mut y = 40.0;
    let text = |o: &mut String, x: f64, y: f64, size: u32, col: &str, weight: u32, t: &str| {
        o.push_str(&format!(
            "<text x=\"{x:.0}\" y=\"{y:.0}\" font-size=\"{size}\" fill=\"{col}\" font-weight=\"{weight}\">{}</text>",
            esc(t)
        ));
    };
    text(&mut o, 24.0, y, 22, INK, 600, &ladder_headline(r, l));
    y += 24.0;
    let when = w["id"].as_i64().map(crate::alerts_page::mdhm).unwrap_or_default();
    let ctx = context_text(&w["context"], l);
    let sub = [when, ctx].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
    text(&mut o, 24.0, y, 13, T3, 400, &sub);
    y += 30.0;
    let steps = r["steps"].as_array().map(Vec::as_slice).unwrap_or_default();
    for (i, s) in steps.iter().enumerate() {
        let k = ladder_kind(r, i as u64 + 1);
        text(&mut o, 24.0, y, 14, INK, 500, step_name(k, l));
        let st = &s["stats"];
        let mut v = Vec::new();
        if k == "recovery"
            && let Some(ms) = r["findings"]["recovery_ms"].as_f64()
        {
            v.push(match l {
                Lang::En => format!("recovered in {:.1} s", ms / 1000.0),
                Lang::Zh => format!("{:.1} 秒恢复", ms / 1000.0),
            });
        }
        for (name, key) in [("P50", "p50_ms"), ("P90", "p90_ms"), ("P95", "p95_ms")] {
            if let Some(x) = st[key].as_f64() {
                v.push(format!("{name} {x:.0}"));
            }
        }
        if let Some(x) = st["increase_ms"].as_f64() {
            v.push(format!("+{x:.0}"));
        }
        let mut line = v.join(" · ");
        if st["p50_ms"].is_number() {
            line.push_str(" ms");
        }
        for (arrow, got, want) in [("↓", "down_mbps", "target_down_mbps"), ("↑", "up_mbps", "target_up_mbps")] {
            if let (Some(g), Some(t)) = (s[got].as_f64(), s[want].as_f64()) {
                line.push_str(&format!("  {arrow}{g:.1}/{t:.0} Mbps"));
            }
        }
        text(&mut o, 200.0, y, 14, INK, 400, &line);
        y += 22.0;
    }
    // the delay over the run, steps shaded alternately, then the rates
    y += 12.0;
    let (cx, cw, ch) = (60.0, width - 84.0, 160.0);
    text(&mut o, 24.0, y, 13, T3, 400, l.pick("Reaction time over the run (ms)", "全程反应时间（毫秒）"));
    y += 10.0;
    let top = y;
    let from = steps.first().and_then(|s| s["from_ms"].as_f64()).unwrap_or(0.0);
    let samples = r["samples"].as_array().map(Vec::as_slice).unwrap_or_default();
    let to = steps
        .iter()
        .filter_map(|s| s["to_ms"].as_f64())
        .fold(None, |m: Option<f64>, b| Some(m.map_or(b, |m| m.max(b))))
        .unwrap_or_else(|| samples.iter().filter_map(|s| s[0].as_f64()).fold(1.0, f64::max));
    let span = (to - from).max(1.0);
    let x = |t: f64| cx + ((t - from) / span).clamp(0.0, 1.0) * cw;
    for (i, s) in steps.iter().enumerate() {
        if let (Some(a), Some(b)) = (s["from_ms"].as_f64(), s["to_ms"].as_f64()) {
            if i % 2 == 1 {
                o.push_str(&format!("<rect x=\"{:.1}\" y=\"{top:.1}\" width=\"{:.1}\" height=\"{ch}\" fill=\"{HAIR}\" opacity=\"0.6\"/>", x(a), x(b) - x(a)));
            }
            let mid = (x(a) + x(b)) / 2.0;
            o.push_str(&format!(
                "<text x=\"{mid:.0}\" y=\"{:.0}\" font-size=\"11\" fill=\"{T3}\" text-anchor=\"middle\">{}</text>",
                top + ch + 14.0,
                esc(step_name(ladder_kind(r, i as u64 + 1), l))
            ));
        }
    }
    for ms in [250.0, 500.0, 750.0, 1000.0] {
        let gy = top + ch - ms / LADDER_CAP_MS * ch;
        o.push_str(&format!("<line x1=\"{cx}\" x2=\"{:.0}\" y1=\"{gy:.1}\" y2=\"{gy:.1}\" stroke=\"{HAIR}\"/>", cx + cw));
        text(&mut o, 24.0, gy + 4.0, 11, T3, 400, &format!("{ms:.0}"));
    }
    let pts: Vec<String> = samples
        .iter()
        .filter_map(|s| Some((s[0].as_f64()?, s[1].as_f64()?)))
        .filter(|(t, _)| *t >= from && *t <= to)
        .map(|(t, ms)| format!("{:.1},{:.1}", x(t), top + ch - ms.min(LADDER_CAP_MS) / LADDER_CAP_MS * ch))
        .collect();
    o.push_str(&format!("<polyline points=\"{}\" fill=\"none\" stroke=\"{ACC}\" stroke-width=\"1.5\"/>", pts.join(" ")));
    for s in samples.iter().filter(|s| s[2].as_u64() == Some(1)) {
        if let Some(t) = s[0].as_f64().filter(|t| *t >= from && *t <= to) {
            o.push_str(&format!("<line x1=\"{0:.1}\" x2=\"{0:.1}\" y1=\"{top:.1}\" y2=\"{1:.1}\" stroke=\"{BAD}\" stroke-width=\"3\"/>", x(t), top + 10.0));
        }
    }
    y = top + ch + 36.0;
    let rates: Vec<(f64, f64, f64)> = r["rates"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|v| Some((v[0].as_f64()?, v[1].as_f64()? / 1e3, v[2].as_f64()? / 1e3)))
        .filter(|v| v.0 >= from && v.0 <= to)
        .collect();
    if !rates.is_empty() {
        let peak = rates.iter().map(|v| v.1.max(v.2)).fold(1.0, f64::max);
        let rtop = (peak / 10.0).ceil() * 10.0;
        text(&mut o, 24.0, y, 13, T3, 400, &match l {
            Lang::En => format!("Rate (Mbps, top {rtop:.0}): download green, upload orange"),
            Lang::Zh => format!("速率（Mbps，图顶 {rtop:.0}）：绿色下载，橙色上传"),
        });
        y += 10.0;
        let rh = 70.0;
        o.push_str(&format!("<line x1=\"{cx}\" x2=\"{:.0}\" y1=\"{y:.1}\" y2=\"{y:.1}\" stroke=\"{HAIR}\"/><line x1=\"{cx}\" x2=\"{:.0}\" y1=\"{:.1}\" y2=\"{:.1}\" stroke=\"{HAIR}\"/>", cx + cw, cx + cw, y + rh, y + rh));
        for (pick, col) in [(1usize, OK), (2, WARN)] {
            let p: Vec<String> = rates
                .iter()
                .map(|v| {
                    let m = if pick == 1 { v.1 } else { v.2 };
                    format!("{:.1},{:.1}", x(v.0), y + rh - m.min(rtop) / rtop * rh)
                })
                .collect();
            o.push_str(&format!("<polyline points=\"{}\" fill=\"none\" stroke=\"{col}\" stroke-width=\"1.5\"/>", p.join(" ")));
        }
        y += rh + 20.0;
    }
    text(&mut o, 24.0, y + 6.0, 11, T3, 400, l.pick(
        "This moment only. Sent from the device; Wi-Fi, call loss and the CHILL path were not tested. Red marks: probes unanswered in 2 s.",
        "只代表此刻。从设备发起，没测 Wi-Fi、通话丢包、CHILL 路径。红色短线：2 秒内没回应的探测。",
    ));
    let height = y + 24.0;
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width:.0}\" height=\"{height:.0}\" viewBox=\"0 0 {width:.0} {height:.0}\" font-family=\"-apple-system, 'PingFang SC', 'Noto Sans CJK SC', 'Microsoft YaHei', sans-serif\"><rect width=\"100%\" height=\"100%\" fill=\"#ffffff\"/>{o}</svg>"
    )
}

/// GET /tools/speedtest/ladder.svg?id=: the run as a file to keep or send.
pub async fn svg_file(
    State(app): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
    req: axum::extract::Request,
) -> Response {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    let who = Caller::of(&app, &req);
    let Some(body) = result(&app, &who, num(&q, "id")).await else {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    };
    let w = &body["result"];
    let id = w["id"].as_u64().unwrap_or(0);
    (
        [
            (axum::http::header::CONTENT_TYPE, "image/svg+xml; charset=utf-8".to_string()),
            (axum::http::header::CONTENT_DISPOSITION, format!("attachment; filename=\"ladder-{id}.svg\"")),
            (axum::http::header::CACHE_CONTROL, "no-store".to_string()),
        ],
        svg(w, l),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(id: u64, pci: i64, p50: [f64; 5]) -> Value {
        let kinds = ["idle", "down", "both", "up", "recovery"];
        let steps: Vec<Value> = (0..5)
            .map(|i| json!({"kind": kinds[i], "from_ms": i * 1000, "to_ms": (i + 1) * 1000,
                "stats": {"p50_ms": p50[i], "p90_ms": p50[i] + 5.0, "p95_ms": p50[i] + 9.0}}))
            .collect();
        let samples: Vec<Value> = (0..50).map(|j| json!([j * 100, p50[j / 10] as u64, if j == 25 { 1 } else { 0 }])).collect();
        json!({"id": id, "context": {"rat": "SA", "band": "n78", "pci": pci, "rsrp": -88.0, "sinr": 14.0, "carriers": 2},
            "report": {"plan": "home", "aborted": null, "findings": {"first_slow_step": 3, "slow_steps": [3], "recovery_ms": 500},
                "notes": [], "steps": steps, "samples": samples}})
    }

    #[test]
    fn two_runs_compare_step_by_step_and_name_a_cell_change() {
        let c = compare(&run(2, 837, [40.0, 45.0, 190.0, 60.0, 41.0]), &run(1, 900, [42.0, 44.0, 120.0, 50.0, 40.0]), Lang::Zh);
        assert!(!c.unlike);
        assert_eq!(c.rows.len(), 5);
        assert_eq!((c.rows[2].name, c.rows[2].change.as_str()), ("再加上传", "+70 ms"));
        assert_eq!(c.rows[4].this, "0.5 秒");
        assert!(c.note.is_some());
        assert!(c.this_ctx.contains("SA n78 · PCI 837 · RSRP -88 dBm · SINR 14 dB · 2 载波"), "{}", c.this_ctx);
        let same = compare(&run(2, 837, [40.0; 5]), &run(1, 837, [40.0; 5]), Lang::Zh);
        assert!(same.note.is_none());
    }
    #[test]
    fn the_spread_has_a_line_per_step_ending_at_all() {
        let lines = cdf(&run(1, 1, [40.0, 45.0, 190.0, 60.0, 41.0])["report"], Lang::En);
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[3].kind, "up");
        assert!(lines[0].points.ends_with(",0.0"), "{}", lines[0].points);
    }
    /// 11-14 ③: the shown run's verdict is the page-top status block, with its tone.
    #[test]
    fn the_verdict_is_the_status_block() {
        let st = |report: Value| status(Some(&json!({"result": {"id": 1_794_680_000u64, "report": report, "context": {}}})), Lang::Zh);
        let bad = st(json!({"findings": {"unresponsive_steps": [3]}, "steps": [{}, {}, {"kind": "both"}]}));
        assert!(bad.starts_with("<div id=\"ld-status\"") && bad.contains("nd-status--bad") && bad.contains("网络就没反应了"), "{bad}");
        assert!(bad.contains("11-14"), "its time as meta: {bad}");
        assert!(st(json!({"aborted": "stopped", "findings": {}})).contains("nd-status--neutral"));
        assert!(st(json!({"findings": {}})).contains("nd-status--ok"));
        assert!(status(Some(&json!({"result": null})), Lang::Zh).contains("还没有记录"));
        assert!(status(None, Lang::Zh).contains("nd-status--bad"));
    }

    #[test]
    fn the_svg_stands_alone() {
        let s = svg(&run(1, 1, [40.0, 45.0, 190.0, 60.0, 41.0]), Lang::Zh);
        assert!(s.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""));
        assert!(!s.contains("var(--"));
        assert!(s.contains("只上传") && s.contains("SA n78"));
        assert!(s.contains("#c8101a"), "the unanswered probe is marked");
    }
}
