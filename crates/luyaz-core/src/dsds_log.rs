//! Passive dual-SIM log (全球版, 10-21; docs/designs/dual-sim-global.md「观察记录」).
//!
//! The stock firmware moves the data card by itself (rule in
//! docs/research/dsds-smart-switch-20261021.md); we only watch. While `sim.dual.mode` is
//! "dual", every [`EVERY`] one sample line goes to `DIR/YYYY-MM-DD.jsonl` (device date: the
//! clock is local time labelled UTC, so the UTC digits of the epoch are the local ones, as
//! `ops::record::civil`). When the data slot changes, or `switching` flips, an event line
//! `{"ev":"switch",from,to,before,after,smart}` goes in too. Files older than [`KEEP_DAYS`]
//! days are deleted at each date change; a file past [`CAP`] keeps only event lines, past
//! [`HARD_CAP`] nothing. Nothing here reads the radio or the network: the inputs are the
//! snapshot's own fields. A single-slot device (no `sim.dual`) never starts the thread.
//!
//! Flash: one ~350-byte line every 30 s = ~1 MB a day, ~14 MB for the 14 days; not fsynced.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, Once};
use std::time::Duration;

pub const DIR: &str = "/data/luyaz/core/dsds";
pub const EVERY: Duration = Duration::from_secs(30);
pub const KEEP_DAYS: i64 = 14;
/// Past this, a day's file takes only event lines.
pub const CAP: u64 = 2 << 20;
/// Past this, nothing.
pub const HARD_CAP: u64 = 3 << 20;
/// A previous sample older than this is not compared (core restart, mode change, sleep).
pub const GAP_S: i64 = 120;
/// The marker the reader filters on before parsing.
const EV_MARK: &str = "\"ev\":\"switch\"";

/// One sample from the snapshot's fields, `None` unless `sim.dual.mode == "dual"`.
/// - per slot: `sim.dual.slots[]` (signal from nwinfo for the data slot, the QMI sampler for
///   the standby one; `age_s` = `t - signal.at`);
/// - `net_type`: the data card's serving type, the signal block's `type` (`fields.net.type`);
/// - `data_up`: `interfaces.wan4.up` = netifd `network.interface.zte_wan status` `up`, the
///   cellular IPv4 interface (state.rs `wan4_if`) — read each round anyway, no new traffic.
pub fn sample(fields: &serde_json::Map<String, Value>, t: i64) -> Option<Value> {
    let d = fields.get("sim")?.get("dual")?;
    if d["mode"].as_str() != Some("dual") {
        return None;
    }
    let slots: Vec<Value> = d["slots"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|s| {
                    let g = &s["signal"];
                    json!({
                        "slot": s["slot"], "operator": s["operator"], "state": s["state"], "role": s["role"],
                        "rat": g["rat"], "rsrp": g["rsrp"], "snr": g["snr"], "bars": g["bars"],
                        "age_s": g["at"].as_i64().map(|at| (t - at).max(0)),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(json!({
        "t": t,
        "active_slot": d["active_slot"],
        "smart": d["smart"] == true,
        "switching": d["switching"] == true,
        "net_type": fields.get("net").map_or(Value::Null, |n| n["type"].clone()),
        "data_up": fields.get("interfaces").and_then(|i| i["wan4"]["up"].as_bool()),
        "slots": slots,
    }))
}

/// What the recorder remembers between samples.
#[derive(Default)]
pub struct Prev {
    last: Option<Value>,
    /// The last sample not in the middle of a switch (the "before" of a slot change).
    calm: Option<Value>,
}

/// The lines for `cur` after `prev`: the sample, then an event when the data slot moved or
/// `switching` flipped (a previous sample older than [`GAP_S`] is forgotten first).
pub fn step(prev: &mut Prev, cur: Value) -> Vec<Value> {
    let t = cur["t"].as_i64().unwrap_or(0);
    if prev.last.as_ref().is_some_and(|p| t - p["t"].as_i64().unwrap_or(0) > GAP_S) {
        *prev = Prev::default();
    }
    let mut out = vec![cur.clone()];
    if let Some(p) = &prev.last {
        let (from, to) = (p["active_slot"].clone(), cur["active_slot"].clone());
        if from != to || p["switching"] != cur["switching"] {
            let before = if from != to {
                prev.calm.clone().filter(|c| c["active_slot"] == from).unwrap_or_else(|| p.clone())
            } else {
                p.clone()
            };
            out.push(json!({
                "t": t, "ev": "switch", "from": from, "to": to, "smart": cur["smart"],
                "before": before, "after": cur,
            }));
        }
    }
    if cur["switching"] != true {
        prev.calm = Some(cur.clone());
    }
    prev.last = Some(cur);
    out
}

/// `YYYY-MM-DD` of a device epoch.
pub fn date(t: i64) -> String {
    crate::ops::record::civil(t.max(0) as u64)[..10].to_owned()
}

fn is_day_file(n: &str) -> bool {
    let b = n.as_bytes();
    n.len() == 16
        && n.ends_with(".jsonl")
        && b[4] == b'-'
        && b[7] == b'-'
        && b[..10].iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

/// The day files to delete at `t`: older than [`KEEP_DAYS`] days (today counts as one).
pub fn doomed<'a>(names: impl IntoIterator<Item = &'a str>, t: i64) -> Vec<String> {
    let oldest = date(t - (KEEP_DAYS - 1) * 86_400);
    names
        .into_iter()
        .filter(|n| is_day_file(n) && n[..10] < *oldest)
        .map(str::to_owned)
        .collect()
}

/// Whether a line goes into a file that is `size` bytes now.
pub fn fits(size: u64, line: &Value) -> bool {
    size < if line.get("ev").is_some() { HARD_CAP } else { CAP }
}

// ---- IO ----

static LATEST: Mutex<Option<serde_json::Map<String, Value>>> = Mutex::new(None);
static START: Once = Once::new();

/// Called from each collection round with the snapshot's fields. Keeps the latest dual-mode
/// fields for the recorder; starts its thread the first time dual mode is seen.
pub fn offer(fields: &serde_json::Map<String, Value>) {
    let dual = fields
        .get("sim")
        .and_then(|s| s.get("dual"))
        .is_some_and(|d| d["mode"].as_str() == Some("dual"));
    let keep = dual.then(|| {
        ["sim", "net", "interfaces"]
            .iter()
            .filter_map(|k| fields.get(*k).map(|v| ((*k).to_owned(), v.clone())))
            .collect()
    });
    let mut l = LATEST.lock().unwrap_or_else(|e| e.into_inner());
    *l = keep;
    drop(l);
    if dual && cfg!(not(test)) {
        START.call_once(|| {
            let _ = std::thread::Builder::new().name("dsds-log".into()).spawn(|| run(Path::new(DIR)));
        });
    }
}

/// The wall clock can be trusted for dates (the sentinel's clock evidence, as the scheduler).
pub(crate) fn clock_ok(t: i64) -> bool {
    let read = |p: &str| std::fs::read_to_string(p).unwrap_or_default();
    let boot = luyaz_api::clock::BootTime::from_uptime(&read("/proc/sys/kernel/random/boot_id"), &read("/proc/uptime"));
    crate::engines::scheduler::clock_usable(&read(crate::engines::scheduler::CLOCK_OK), boot.as_ref(), t)
}

fn run(dir: &Path) {
    let mut prev = Prev::default();
    let mut pruned = String::new();
    loop {
        std::thread::sleep(EVERY);
        let fields = LATEST.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let t = crate::qmi_nas::now() as i64;
        let Some(cur) = fields.as_ref().and_then(|f| sample(f, t)) else {
            prev = Prev::default();
            continue;
        };
        if !clock_ok(t) {
            prev = Prev::default();
            continue;
        }
        let day = date(t);
        if day != pruned {
            prune(dir, t);
            pruned = day.clone();
        }
        let _ = write(dir, &day, &step(&mut prev, cur));
    }
}

fn prune(dir: &Path, t: i64) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let names: Vec<String> = rd.flatten().filter_map(|e| e.file_name().into_string().ok()).collect();
    for n in doomed(names.iter().map(String::as_str), t) {
        let _ = std::fs::remove_file(dir.join(n));
    }
}

fn write(dir: &Path, day: &str, lines: &[Value]) -> std::io::Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(dir)?;
    let p = dir.join(format!("{day}.jsonl"));
    let mut size = std::fs::metadata(&p).map_or(0, |m| m.len());
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&p)?;
    for l in lines {
        if fits(size, l) {
            let s = format!("{l}\n");
            f.write_all(s.as_bytes())?;
            size += s.len() as u64;
        }
    }
    Ok(())
}

// ---- the read side (`GET /v2/dual/switches`) ----

type Cached = (u64, Option<std::time::SystemTime>, Vec<Value>);
static CACHE: Mutex<Vec<(PathBuf, Cached)>> = Mutex::new(Vec::new());

/// A file's real slot changes (from != to), cached on (length, mtime).
fn file_switches(p: &Path) -> Vec<Value> {
    let Ok(m) = std::fs::metadata(p) else { return Vec::new() };
    let key = (m.len(), m.modified().ok());
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, (len, mt, v))) = c.iter().find(|(q, _)| q == p)
        && (*len, *mt) == key
    {
        return v.clone();
    }
    let v = switches_in(&std::fs::read_to_string(p).unwrap_or_default());
    c.retain(|(q, _)| q != p && q.exists());
    c.push((p.to_owned(), (key.0, key.1, v.clone())));
    v
}

/// The event lines of one file that moved the data slot (a `switching` flip alone is not one).
pub fn switches_in(text: &str) -> Vec<Value> {
    text.lines()
        .filter(|l| l.contains(EV_MARK))
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["from"] != v["to"])
        .collect()
}

/// `{days, count, switches}`: the slot changes of the last `days` (1..=14) days, newest first,
/// at most `limit` of them (`count` is all of them).
pub fn switches(dir: &Path, days: i64, limit: usize, t: i64) -> Value {
    let days = days.clamp(1, KEEP_DAYS);
    let oldest = date(t - (days - 1) * 86_400);
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().filter_map(|e| e.file_name().into_string().ok()).collect())
        .unwrap_or_default();
    names.retain(|n| is_day_file(n) && n[..10] >= *oldest);
    let mut all: Vec<Value> = names.iter().flat_map(|n| file_switches(&dir.join(n))).collect();
    all.sort_by_key(|v| std::cmp::Reverse(v["t"].as_i64().unwrap_or(0)));
    let count = all.len();
    all.truncate(limit);
    json!({"days": days, "count": count, "switches": all})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(active: i64, switching: bool, rsrp1: i64) -> serde_json::Map<String, Value> {
        let role = |s| if s == active { "data" } else { "standby" };
        json!({
            "sim": {"dual": {"mode": "dual", "active_slot": active, "smart": true, "switching": switching,
                "slots": [
                    {"slot": 1, "operator": "CUCC", "state": "ready", "role": role(1),
                     "signal": {"rat": "nr", "rsrp": rsrp1, "rsrq": -11, "snr": "-1.0", "bars": 1, "at": 990}},
                    {"slot": 2, "operator": "CMCC", "state": "ready", "role": role(2),
                     "signal": {"rat": "nr", "rsrp": -98, "rsrq": -10, "snr": "7.0", "bars": 4, "at": 970}}]}},
            "net": {"type": "SA"},
            "interfaces": {"wan4": {"up": true}},
        })
        .as_object()
        .unwrap()
        .clone()
    }

    #[test]
    fn line_format() {
        let s = sample(&fields(1, false, -116), 1000).unwrap();
        assert_eq!(
            s.to_string(),
            r#"{"active_slot":1,"data_up":true,"net_type":"SA","slots":[{"age_s":10,"bars":1,"operator":"CUCC","rat":"nr","role":"data","rsrp":-116,"slot":1,"snr":"-1.0","state":"ready"},{"age_s":30,"bars":4,"operator":"CMCC","rat":"nr","role":"standby","rsrp":-98,"slot":2,"snr":"7.0","state":"ready"}],"smart":true,"switching":false,"t":1000}"#
        );
    }

    #[test]
    fn single_mode_and_single_slot_give_nothing() {
        let mut f = fields(1, false, -90);
        f["sim"]["dual"]["mode"] = json!("single");
        assert!(sample(&f, 1).is_none());
        f.insert("sim".into(), json!({}));
        assert!(sample(&f, 1).is_none());
    }

    #[test]
    fn switch_detection() {
        let mut p = Prev::default();
        assert_eq!(step(&mut p, sample(&fields(1, false, -116), 1000).unwrap()).len(), 1);
        assert_eq!(step(&mut p, sample(&fields(1, false, -117), 1030).unwrap()).len(), 1);
        // switching flips on: an event, same slot
        let on = step(&mut p, sample(&fields(1, true, -117), 1060).unwrap());
        assert_eq!((on.len(), &on[1]["from"], &on[1]["to"]), (2, &json!(1), &json!(1)));
        // slot moves (still switching): before = the last calm sample
        let mv = step(&mut p, sample(&fields(2, true, -117), 1090).unwrap());
        assert_eq!(mv[1]["ev"], "switch");
        assert_eq!((&mv[1]["from"], &mv[1]["to"]), (&json!(1), &json!(2)));
        assert_eq!(mv[1]["before"]["t"], 1030);
        assert_eq!(mv[1]["after"]["t"], 1090);
        // switching off: event again; the reader counts only from != to
        let off = step(&mut p, sample(&fields(2, false, -117), 1120).unwrap());
        assert_eq!(off.len(), 2);
        let text: String = [on, mv, off].concat().iter().map(|l| format!("{l}\n")).collect();
        assert_eq!(switches_in(&text).len(), 1);
    }

    #[test]
    fn gap_forgets() {
        let mut p = Prev::default();
        step(&mut p, sample(&fields(1, false, -90), 1000).unwrap());
        assert_eq!(step(&mut p, sample(&fields(2, false, -90), 1000 + GAP_S + 1).unwrap()).len(), 1);
    }

    #[test]
    fn pruning() {
        // 2026-10-21 12:00 (device clock)
        let t = 1_792_584_000;
        assert_eq!(date(t), "2026-10-21");
        let names = ["2026-10-08.jsonl", "2026-10-07.jsonl", "2026-09-30.jsonl", "2026-10-21.jsonl", "notes.txt", "2026-1-01.jsonl"];
        assert_eq!(doomed(names, t), vec!["2026-10-07.jsonl", "2026-09-30.jsonl"]);
    }

    #[test]
    fn caps() {
        let s = json!({"t": 1});
        let e = json!({"t": 1, "ev": "switch"});
        assert!(fits(CAP - 1, &s) && !fits(CAP, &s));
        assert!(fits(CAP, &e) && !fits(HARD_CAP, &e));
    }

    #[test]
    fn read_side() {
        let dir = std::env::temp_dir().join(format!("dsds-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let t = 1_792_584_000;
        let ev = |at: i64, from: i64, to: i64| format!("{}\n", json!({"t": at, "ev": "switch", "from": from, "to": to}));
        write(&dir, "2026-10-21", &[json!({"t": t})]).unwrap();
        std::fs::write(dir.join("2026-10-21.jsonl"), format!("{}{}", ev(t, 1, 2), ev(t + 5, 2, 2))).unwrap();
        std::fs::write(dir.join("2026-10-20.jsonl"), ev(t - 86_400, 2, 1)).unwrap();
        std::fs::write(dir.join("2026-10-01.jsonl"), ev(t - 20 * 86_400, 2, 1)).unwrap();
        let v = switches(&dir, 14, 10, t);
        assert_eq!(v["count"], 2);
        assert_eq!(v["switches"][0]["t"], t);
        assert_eq!(switches(&dir, 1, 10, t)["count"], 1);
        assert_eq!(switches(&dir, 99, 1, t)["switches"].as_array().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
