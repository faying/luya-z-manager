//! The core's long-term layer as ledger lines (u60-sentinel.md §19 T9a, the second adapter):
//! `/data/u60d/long/hours.jsonl` and `events.jsonl` (longterm.rs, crashwatch) → the guard
//! ledger's line shape, so [`super::Card`] reads both the same way.
//!
//! The core's lines carry `boot` (the boot id's first 8 hex) instead of the ledger's boot number
//! `seq`, and no `v`/`n`; hour lines have no `up`/`t` (they are `to`/`tt`). Boots are numbered in
//! the order they happened: those only left in hours.jsonl (their events aged out first: events
//! are kept 30 days, hours 90) come first in that file's order, then the boots of events.jsonl in
//! the order each first appears (a boot's first line is any of its events: `ver`, the crash
//! watcher's, or `boot`, which waits for its facts). Within a boot, events and hours are
//! merged by uptime. Kinds and fields the core does not write yet stay absent, so their rows
//! read 没测 or 没有记录 until T9c adds the producers.
//!
//! Coverage (u60-sentinel.md §19 「T9c 前定」): the core's `g_core` is the card's `g_guard` (the
//! process that keeps the books), and its `gap src=core` the card's `src=guard`. A `g_*` the hour
//! line does not carry (hours written before T9c, sources not watched) counts as not working the
//! whole awake hour: a missing field must not read 完整. A `gap … no_hour=1` (the core was gone
//! across an hour turn) becomes an hour line over its span with every source down.

use serde_json::Value;
use std::collections::HashMap;

/// (boot8, uptime, the line's fields).
type Rec = (String, f64, Value);

/// The card's sources and where the core's hour line has them.
const G: [(&str, &str); 6] = [
    ("guard", "g_core"),
    ("watcher", "g_watcher"),
    ("capture", "g_capture"),
    ("uidlog", "g_uidlog"),
    ("crashlog", "g_crashlog"),
    ("datad", "g_datad"),
];

/// An hour line's coverage in the card's names; missing = down all the awake hour.
fn hour_cov(o: &mut Value) {
    let awake = o["awake"].clone();
    if let Some(m) = o.as_object_mut() {
        for (card, core) in G {
            let v = m.get(core).filter(|v| v.is_number()).cloned();
            m.insert(format!("g_{card}"), v.unwrap_or_else(|| awake.clone()));
        }
    }
}

/// The hour line for a span the core was not there for (`gap … no_hour=1`).
fn gap_hour(g: &Value) -> Option<Value> {
    let (from, to) = (g["from"].as_i64()?, g["to"].as_i64()?);
    let awake = g["awake"]
        .as_i64()
        .unwrap_or(to - from)
        .clamp(0, (to - from).max(0));
    let mut h = serde_json::json!({
        "k": "hour",
        "id": format!("h-{}-{from}", g["boot"].as_str().unwrap_or("")),
        "boot": g["boot"],
        "from": from,
        "to": to,
        "ft": g["ft"],
        "tt": g["tt"],
        "awake": awake,
        "asleep": (to - from - awake).max(0),
    });
    let m = h.as_object_mut()?;
    for (card, _) in G {
        m.insert(format!("g_{card}"), awake.into());
    }
    Some(h)
}

fn parse(text: &str, hour: bool) -> Vec<Rec> {
    let mut v = Vec::new();
    for l in text.lines() {
        let Ok(o) = serde_json::from_str::<Value>(l) else {
            continue;
        };
        let Some(boot) = o["boot"].as_str().map(str::to_string) else {
            continue;
        };
        let up = if hour { &o["to"] } else { &o["up"] };
        let Some(up) = up.as_f64() else { continue };
        let mut o = o;
        if hour && o["k"] == "hour" {
            hour_cov(&mut o);
        } else if o["k"] == "gap" && o["src"] == "core" {
            o["src"] = "guard".into();
        }
        v.push((boot, up, o));
    }
    v
}

/// The ledger lines for the two files' text.
pub fn lines(hours: &str, events: &str) -> Vec<String> {
    lines_seq(hours, events, &HashMap::new())
}

/// The same, with the boots in `known` (boot8 → the guard ledger's `seq`) numbered as there and
/// the others after the highest of them, in the order above (the stitch: replay.rs).
pub fn lines_seq(hours: &str, events: &str, known: &HashMap<String, i64>) -> Vec<String> {
    let (mut h, e) = (parse(hours, true), parse(events, false));
    for (b, _, o) in &e {
        if o["k"] == "gap"
            && o["no_hour"] == 1
            && let Some(x) = gap_hour(o)
            && let Some(to) = x["to"].as_f64()
        {
            h.push((b.clone(), to, x));
        }
    }
    let mut order: Vec<String> = Vec::new();
    let in_events: std::collections::HashSet<&str> = e.iter().map(|r| r.0.as_str()).collect();
    for (b, _, _) in &h {
        if !in_events.contains(b.as_str()) && !order.contains(b) {
            order.push(b.clone());
        }
    }
    for (b, _, _) in &e {
        if !order.contains(b) {
            order.push(b.clone());
        }
    }
    let mut next = known.values().copied().max().unwrap_or(0);
    let seq: HashMap<&str, i64> = order
        .iter()
        .map(|b| {
            let s = known.get(b).copied().unwrap_or_else(|| {
                next += 1;
                next
            });
            (b.as_str(), s)
        })
        .collect();
    let mut all: Vec<(i64, f64, usize, bool, &Value)> = Vec::new();
    for (i, (b, up, o)) in e.iter().enumerate() {
        all.push((seq[b.as_str()], *up, i, false, o));
    }
    for (i, (b, up, o)) in h.iter().enumerate() {
        // an hour line goes after the events of the same second
        all.push((seq[b.as_str()], *up, e.len() + i, true, o));
    }
    all.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .then(a.2.cmp(&b.2))
    });
    let mut out = Vec::with_capacity(all.len());
    for (n, (s, up, _, hour, o)) in all.into_iter().enumerate() {
        let Some(k) = o["k"].as_str() else { continue };
        let t = if hour { &o["tt"] } else { &o["t"] };
        let t = t.as_i64().map_or("null".to_string(), |t| t.to_string());
        let mut line = format!(
            "{{\"v\":1,\"seq\":{s},\"n\":{},\"up\":{},\"t\":{t},\"k\":{}",
            n + 1,
            up.max(0.0),
            Value::from(k)
        );
        if let Some(m) = o.as_object() {
            for (key, v) in m {
                if matches!(key.as_str(), "k" | "t" | "up" | "v" | "seq" | "n") {
                    continue;
                }
                line.push(',');
                line.push_str(&Value::from(key.as_str()).to_string());
                line.push(':');
                line.push_str(&v.to_string());
            }
        }
        line.push('}');
        out.push(line);
    }
    out
}
