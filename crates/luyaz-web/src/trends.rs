//! /trends (近 5 分钟): mobile speed, CPU, memory and battery as four small line charts, like the
//! old page (`trends/page.tsx` + `MiniChart.tsx`). The old page sampled four agent endpoints every
//! 3 s in the tab; here every value is already in u60d's `live` and `battery` blocks on the one
//! upstream connection, so the hub keeps one sample per `live` block for 5 minutes and the page
//! just draws them.
//!
//! ```text
//!  live block ─▶ hub: record(Sample{at: now, …}) ─▶ Change::Block("live") ─▶ each /trends stream
//!     re-renders and sends only the leaves (a chart's line, its "now" value) whose HTML changed
//! ```
//! Samples are by arrival on our clock, and every render drops what is older than 5 minutes: after
//! the idle disconnect the hub still holds old samples, and those must read "collecting", not be
//! drawn as the last 5 minutes.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::time::{Duration, Instant};

use askama::Template;
use axum::extract::{Query, State};
use axum::response::Response;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::Stream;
use serde_json::Value;
use tokio::sync::broadcast::error::RecvError;

use crate::hub::{Change, Hub, Link, St};
use crate::i18n::Lang;
use crate::pages::{Shared, StreamQuery, async_stream, send_out, split_leaves};

pub const WINDOW: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy)]
pub struct Sample {
    pub at: Instant,
    /// Mobile download / upload, Mbit/s.
    pub down: Option<f64>,
    pub up: Option<f64>,
    /// Percent.
    pub cpu: Option<f64>,
    pub mem: Option<f64>,
    pub bat: Option<f64>,
    /// Battery power, W (sysfs `current_now` sign, like the old page: + charging).
    pub watt: Option<f64>,
}

/// The sample a `live` block (and the battery block as it is then) gives.
pub fn sample(live: &Value, battery: Option<&Value>, at: Instant) -> Sample {
    let rt = &live["runtime"];
    // the agent's speed (`read_rmnet_bytes`) counts every rmnet interface; u60d's `rx_bps` is
    // bytes per second despite the name
    let rates: Vec<&Value> = rt["link_rates"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|r| {
                    r["interface"]
                        .as_str()
                        .is_some_and(|i| i.starts_with("rmnet"))
                })
                .collect()
        })
        .unwrap_or_default();
    let sum = |k: &str| -> Option<f64> {
        let v: Vec<f64> = rates.iter().filter_map(|r| r[k].as_f64()).collect();
        (!v.is_empty()).then(|| v.iter().sum::<f64>() * 8.0 / 1e6)
    };
    let mem = &rt["memory_kb"];
    let mem = match (mem["total"].as_f64(), mem["available"].as_f64()) {
        (Some(t), Some(a)) if t > 0.0 => Some((t - a) / t * 100.0),
        _ => None,
    };
    let b = battery.unwrap_or(&Value::Null);
    let watt = match (b["bat_uv"].as_f64(), b["bat_ua"].as_f64()) {
        (Some(uv), Some(ua)) if uv > 0.0 => Some(uv / 1e6 * ua / 1e6),
        _ => None,
    };
    Sample {
        at,
        down: sum("rx_bps"),
        up: sum("tx_bps"),
        cpu: rt["cpu_usage_tenths"].as_f64().map(|t| t / 10.0),
        mem,
        bat: b["percent"].as_f64(),
        watt,
    }
}

/// Called by the hub when a `live` block (or a snapshot) arrives.
pub fn record(st: &mut St, now: Instant) {
    let Some(live) = st.blocks.get("live").filter(|b| !b.stale) else {
        return;
    };
    let bat = st
        .blocks
        .get("battery")
        .filter(|b| !b.stale)
        .map(|b| &b.data);
    let s = sample(&live.data, bat, now);
    st.samples.push_back(s);
    prune(&mut st.samples, now);
}

fn prune(q: &mut VecDeque<Sample>, now: Instant) {
    while q
        .front()
        .is_some_and(|s| now.saturating_duration_since(s.at) > WINDOW)
    {
        q.pop_front();
    }
}

// ---- the view ----

const W: f64 = 600.0;
const H: f64 = 120.0;
const PAD_R: f64 = 8.0;
const PAD_Y: f64 = 8.0;

pub struct Line {
    pub points: String,
    pub dashed: bool,
}

pub struct Plot {
    /// Gridlines: (y in the viewBox, label, label top as % of the height).
    pub ticks: Vec<(String, String, String)>,
    pub lines: Vec<Line>,
}

pub struct Chart {
    pub key: &'static str,
    pub title: &'static str,
    pub now: String,
    pub aria: &'static str,
    pub plot: Option<Plot>,
}

pub struct View {
    pub l: Lang,
    /// Greyed lines: the data service's numbers are not current.
    pub stale: bool,
    pub full: bool,
    pub charts: [Chart; 4],
}

fn fmt(v: Option<f64>, digits: usize) -> String {
    match v {
        None => "—".into(),
        Some(v) if v >= 100.0 => format!("{v:.0}"),
        Some(v) => format!("{v:.digits$}"),
    }
}

fn tick_label(v: f64) -> String {
    if v >= 10.0 {
        format!("{}", v.round())
    } else {
        format!("{}", (v * 10.0).round() / 10.0)
    }
}

/// MiniChart's geometry: x across the window ending at the newest sample, y from 0 to `max` (or
/// the largest value × 1.1, at least 1). Whole-number points: the line is resent every push, and
/// a pixel of the 600×120 box is finer than the card shows it.
fn plot(
    samples: &[Sample],
    series: &[(fn(&Sample) -> Option<f64>, bool)],
    max: Option<f64>,
) -> Option<Plot> {
    let vals: Vec<f64> = samples
        .iter()
        .flat_map(|s| series.iter().filter_map(move |(f, _)| f(s)))
        .collect();
    // two points of one line, not one point of two lines
    let drawn = samples
        .iter()
        .filter(|s| series.iter().any(|(f, _)| f(s).is_some()))
        .count();
    if drawn < 2 {
        return None;
    }
    let top = max.unwrap_or_else(|| vals.iter().cloned().fold(1.0, f64::max) * 1.1);
    let end = samples.last()?.at;
    let win = WINDOW.as_secs_f64();
    let x = |t: Instant| (1.0 - end.saturating_duration_since(t).as_secs_f64() / win) * (W - PAD_R);
    let y = |v: f64| PAD_Y + (top - v.clamp(0.0, top)) / top * (H - PAD_Y * 2.0);
    let ticks = [0.0, top / 2.0, top]
        .iter()
        .map(|&v| {
            (
                format!("{:.1}", y(v)),
                tick_label(v),
                format!("{:.2}", y(v) / H * 100.0),
            )
        })
        .collect();
    let lines = series
        .iter()
        .map(|(f, dashed)| Line {
            points: samples
                .iter()
                .filter_map(|s| f(s).map(|v| format!("{:.0},{:.0}", x(s.at), y(v))))
                .collect::<Vec<_>>()
                .join(" "),
            dashed: *dashed,
        })
        .collect();
    Some(Plot { ticks, lines })
}

pub fn view(st: &St, now: Instant, l: Lang) -> View {
    let s: Vec<Sample> = st
        .samples
        .iter()
        .filter(|s| now.saturating_duration_since(s.at) <= WINDOW)
        .copied()
        .collect();
    let last = s.last();
    let stale = st.link != Link::Live || st.blocks.get("live").is_none_or(|b| b.stale);
    let span = match (s.first(), last) {
        (Some(a), Some(b)) => b.at.saturating_duration_since(a.at).as_secs(),
        _ => 0,
    };
    let bat_now = match last.and_then(|x| x.bat) {
        None => "—".into(),
        Some(p) => match last.and_then(|x| x.watt) {
            None => format!("{p:.0}%"),
            Some(w) => format!(
                "{p:.0}% · {}{:.1} W",
                if w >= 0.0 { "+" } else { "−" },
                w.abs()
            ),
        },
    };
    View {
        l,
        stale,
        full: span >= 290,
        charts: [
            Chart {
                key: "speed",
                title: l.pick("Mobile speed", "蜂窝网速"),
                now: format!(
                    "↓ {} ↑ {} Mbps",
                    fmt(last.and_then(|x| x.down), 1),
                    fmt(last.and_then(|x| x.up), 1)
                ),
                aria: l.pick(
                    "Mobile download (solid) and upload (dashed), last 5 minutes",
                    "近 5 分钟蜂窝下行（实线）和上行（虚线）",
                ),
                plot: plot(&s, &[(|x| x.down, false), (|x| x.up, true)], None),
            },
            Chart {
                key: "cpu",
                title: "CPU",
                now: format!("{}%", fmt(last.and_then(|x| x.cpu), 0)),
                aria: l.pick("CPU use, last 5 minutes", "近 5 分钟 CPU 占用"),
                plot: plot(&s, &[(|x| x.cpu, false)], Some(100.0)),
            },
            Chart {
                key: "mem",
                title: l.pick("Memory", "内存"),
                now: format!("{}%", fmt(last.and_then(|x| x.mem), 0)),
                aria: l.pick("Memory in use, last 5 minutes", "近 5 分钟内存占用"),
                plot: plot(&s, &[(|x| x.mem, false)], Some(100.0)),
            },
            Chart {
                key: "bat",
                title: l.pick("Battery", "电池"),
                now: bat_now,
                aria: l.pick("Battery level, last 5 minutes", "近 5 分钟电量"),
                plot: plot(&s, &[(|x| x.bat, false)], Some(100.0)),
            },
        ],
    }
}

// ---- templates ----

#[derive(Template)]
#[template(path = "trends_leaves.html")]
struct Leaves<'a> {
    v: &'a View,
    /// Draw the lines. The page itself leaves them out (a full 5-minute hub is ~10 KB gzip of
    /// points, which put the first screen over 40 KiB); the stream's first push brings them.
    lines: bool,
}

#[derive(Template)]
#[template(path = "trends.html")]
struct Page<'a> {
    shell: crate::shell::Shell,
    v: &'a View,
    leaves: std::collections::HashMap<String, String>,
}

impl Page<'_> {
    fn leaf(&self, id: &str) -> &str {
        self.leaves.get(id).map(String::as_str).unwrap_or("")
    }
}

fn leaves(hub: &Hub, l: Lang, lines: bool) -> (u64, Vec<(String, String)>) {
    hub.read(|st| {
        let v = view(st, Instant::now(), l);
        (
            st.generation,
            split_leaves(&Leaves { v: &v, lines }.render().unwrap()),
        )
    })
}

pub async fn page(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, "/trends");
    let (html, g) = app.hub.read(|st| {
        let v = view(st, Instant::now(), ctx.lang);
        let leaves = split_leaves(&Leaves { v: &v, lines: false }.render().unwrap())
            .into_iter()
            .collect();
        let shell = crate::shell::Shell::new(&headers, "/trends").live(
            st,
            &ctx,
            Some(format!("/trends/events?g={}", st.generation)),
        );
        (
            Page {
                shell,
                v: &v,
                leaves,
            }
            .render()
            .unwrap(),
            st.generation,
        )
    });
    let _ = g;
    crate::assets::maybe_gzip(
        &headers,
        "text/html; charset=utf-8",
        html.into_bytes(),
        "no-store",
    )
}

/// One tab's stream: the charts (on each `live` block, and every 10 s so old samples age out
/// while nothing arrives) and the frame. The page came without the lines, so the first push
/// always carries them (as if the page had sent empty ones); after that only what changed.
pub async fn events(
    State(app): State<Shared>,
    Query(q): Query<StreamQuery>,
    req: axum::extract::Request,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let ctx = crate::topbar::Ctx::of_request(&req, "/trends");
    let mut sub = app.hub.subscribe();
    let stream = async_stream(move |tx| async move {
        let (g, mut sent) = leaves(&app.hub, ctx.lang, true);
        let mut seen = crate::opbar::Seen::default();
        let mut frame = app
            .hub
            .read(|st| crate::topbar::leaves(st, &ctx, &mut seen));
        let first = if g != q.g {
            let mut all: Vec<String> = sent.iter().map(|(_, h)| h.clone()).collect();
            all.extend(crate::topbar::diff(&[], &frame));
            all
        } else {
            // what the page has: the same render without the lines
            let (_, page) = leaves(&app.hub, ctx.lang, false);
            crate::topbar::diff(&page, &sent)
        };
        if !send_out(&tx, first).await {
            return;
        }
        let mut tick = tokio::time::interval(Duration::from_secs(10));
        tick.tick().await;
        loop {
            let at = app
                .hub
                .read(|st| crate::opbar::wake(st, &seen, Instant::now()));
            tokio::select! {
                r = sub.rx.recv() => match r {
                    Ok(Change::Block(name))
                        if name != "live" && !crate::topbar::reads(&name) => continue,
                    Ok(_) | Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => return,
                },
                _ = tick.tick() => {}
                _ = crate::topbar::wake(at) => {}
                _ = tx.closed() => return,
            }
            let (_, now) = leaves(&app.hub, ctx.lang, true);
            let mut out = crate::topbar::diff(&sent, &now);
            sent = now;
            let frame_now = app
                .hub
                .read(|st| crate::topbar::leaves(st, &ctx, &mut seen));
            out.extend(crate::topbar::diff(&frame, &frame_now));
            frame = frame_now;
            if !send_out(&tx, out).await {
                return;
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sse::Frame;
    use serde_json::json;

    fn live(rev: u64, rx: u64, cpu: u64) -> Value {
        json!({"epoch":"e","name":"live","revision":rev,"observed_at":rev,"stale":false,
            "data":{"runtime":{"cpu_usage_tenths":cpu,
                "memory_kb":{"total":1000,"available":250},
                "link_rates":[{"interface":"rmnet_data0","rx_bps":rx,"tx_bps":1000},
                              {"interface":"br-lan","rx_bps":99999999,"tx_bps":0}]}}})
    }

    fn feed(hub: &Hub, event: &str, v: Value) {
        hub.apply(&Frame {
            event: event.into(),
            data: v.to_string(),
        });
    }

    fn hub_with_samples(n: u64) -> std::sync::Arc<Hub> {
        let hub = Hub::new();
        feed(
            &hub,
            "snapshot",
            json!({"epoch":"e","blocks":{
                "battery":{"revision":1,"observed_at":1,"stale":false,
                    "data":{"percent":85,"bat_uv":4_000_000,"bat_ua":-500_000}}}}),
        );
        for i in 0..n {
            feed(&hub, "block", live(i + 1, 1_250_000, 100 + i));
        }
        hub
    }

    #[test]
    fn a_sample_reads_units_like_the_old_page() {
        let s = sample(
            &live(1, 1_250_000, 125)["data"],
            Some(&json!({"percent":85,"bat_uv":4_000_000,"bat_ua":-500_000})),
            Instant::now(),
        );
        assert_eq!(s.down, Some(10.0), "bytes/s × 8 / 1e6, rmnet only");
        assert_eq!(s.up, Some(0.008));
        assert_eq!(s.cpu, Some(12.5));
        assert_eq!(s.mem, Some(75.0));
        assert_eq!(s.watt, Some(-2.0));
    }

    #[test]
    fn one_live_block_is_collecting_two_draw_lines() {
        let hub = hub_with_samples(1);
        let v = hub.read(|st| view(st, Instant::now(), Lang::Zh));
        assert!(v.charts.iter().all(|c| c.plot.is_none()));
        assert_eq!(v.charts[3].now, "85% · −2.0 W");
        let hub = hub_with_samples(3);
        let v = hub.read(|st| view(st, Instant::now(), Lang::Zh));
        let p = v.charts[1].plot.as_ref().unwrap();
        assert_eq!(p.lines[0].points.split(' ').count(), 3);
        assert_eq!(v.charts[0].plot.as_ref().unwrap().lines.len(), 2);
    }

    #[test]
    fn samples_older_than_five_minutes_are_not_drawn() {
        let hub = hub_with_samples(3);
        let later = Instant::now() + WINDOW + Duration::from_secs(1);
        let v = hub.read(|st| view(st, later, Lang::Zh));
        assert!(
            v.charts.iter().all(|c| c.plot.is_none()),
            "collecting again"
        );
        assert_eq!(v.charts[1].now, "—%");
    }

    #[test]
    fn a_new_sample_sends_only_the_leaves_that_changed() {
        let hub = hub_with_samples(3);
        let (_, a) = leaves(&hub, Lang::Zh, true);
        feed(&hub, "block", live(10, 1_250_000, 102)); // cpu as the last one, speed the same
        let (_, b) = leaves(&hub, Lang::Zh, true);
        let out = crate::topbar::diff(&a, &b).join("\n");
        assert!(out.contains("id=\"tr-cpu-line\""), "{out}");
        assert!(
            !out.contains("id=\"tr-cpu-plot\""),
            "same scale: no grid: {out}"
        );
        assert!(!out.contains("id=\"tr-cpu-now\""), "same value: {out}");
        assert!(!out.contains("id=\"tr-bat-now\""), "{out}");
    }

    #[test]
    fn the_english_page_has_no_chinese() {
        let hub = hub_with_samples(4);
        let html = hub.read(|st| {
            let v = view(st, Instant::now(), Lang::En);
            Leaves { v: &v, lines: true }.render().unwrap()
        });
        assert!(
            !html.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)
                || ('\u{3000}'..='\u{303f}').contains(&c)
                || ('\u{ff00}'..='\u{ffef}').contains(&c)),
            "{html}"
        );
    }

    #[test]
    fn the_page_has_no_lines_and_the_first_push_brings_exactly_them() {
        // a full hub: 5 minutes of samples, one a second (the mock at SPEED=6 is about that)
        let hub = hub_with_samples(1);
        let now = Instant::now();
        hub.change_for_test(|st| {
            let s = st.samples[0];
            st.samples.clear();
            for i in (0..300).rev() {
                st.samples.push_back(Sample {
                    at: now - Duration::from_secs(i),
                    cpu: Some((i % 50) as f64),
                    down: Some((i % 7) as f64),
                    ..s
                });
            }
        });
        let (_, page) = leaves(&hub, Lang::Zh, false);
        let (_, full) = leaves(&hub, Lang::Zh, true);
        let html: String = page.iter().map(|(_, h)| h.as_str()).collect();
        assert!(!html.contains("<polyline"), "the page has no lines");
        assert!(html.contains("id=\"tr-cpu-line\""), "but the line elements are there");
        let first = crate::topbar::diff(&page, &full);
        assert_eq!(first.len(), 4, "the four lines, nothing else");
        for k in ["speed", "cpu", "mem", "bat"] {
            assert!(
                first.iter().any(|h| h.contains(&format!("id=\"tr-{k}-line\""))),
                "{k}"
            );
        }
        assert!(full.iter().map(|(_, h)| h.len()).sum::<usize>() > html.len() + 5_000);
    }
}
