//! `u60d --scorecard-replay <dir>`: one fixture left by doctor (`DOC_REPORT_KEEP=<dir>`,
//! touch-ui scripts/doctor.sh): `vars` (W, LABEL, RECOV, UIDTK, SUMMARY, LEDGER_DIR, GAVEUP,
//! THR), `ledger/boot-*.jsonl`, `cap`, `warn`, `standby`, `alerts`; doctor's own answer is in
//! `out` and `rc`, which tools/scorecard-parity.sh compares with what this prints.
//!
//! `u60d --scorecard-core <dir> 24h|7d`: the same over the core's long-term layer (`hours.jsonl`,
//! `events.jsonl` in <dir>), with no side inputs (the T9b endpoint will supply them).
//!
//! `u60d --scorecard-replay-core <dir>`: a fixture's side inputs with the core's long-term layer
//! in `<dir>/long/` instead of its guard ledger (T9c's second leg).
//!
//! `u60d --scorecard-replay-stitch <dir> <cut>`: the guard ledger before `cut`, the core's from
//! it ([`stitch`], T9d).

use super::{Card, Inputs, guard_lines};
use std::collections::HashMap;
use std::io::BufRead;
use std::path::Path;

fn read(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).unwrap_or_default()
}

/// The fixture's inputs; the ledger directory doctor named.
pub fn inputs(dir: &Path) -> (Inputs, String) {
    let vars: HashMap<String, String> = read(dir, "vars")
        .lines()
        .filter_map(|l| {
            l.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
        })
        .collect();
    let v = |k: &str| vars.get(k).cloned().unwrap_or_default();
    let n = |k: &str| {
        let x = v(k);
        (!x.is_empty()).then(|| super::awk::num(&x))
    };
    let inp = Inputs {
        w: super::awk::num(&v("W")),
        label: v("LABEL"),
        recov: n("RECOV"),
        uidtk: n("UIDTK"),
        cap: read(dir, "cap"),
        warn: Some(read(dir, "warn")),
        standby: Some(read(dir, "standby")),
        alerts: read(dir, "alerts"),
        gaveup: super::awk::num(&v("GAVEUP")) != 0.0,
        thr: v("THR"),
        summary: v("SUMMARY") == "1",
    };
    (inp, v("LEDGER_DIR"))
}

/// The guard ledger's segments in a directory, in file name order (bytewise, as `sort` in the
/// busybox container and on the device).
pub fn segments(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("boot-") && n.ends_with(".jsonl"))
        })
        .collect();
    v.sort_by(|a, b| {
        a.as_os_str()
            .as_encoded_bytes()
            .cmp(b.as_os_str().as_encoded_bytes())
    });
    v
}

/// The scorecard of a guard ledger directory: text and exit code (doctor's `report_ledger`).
pub fn guard(ledger: &Path, shown: &str, inp: &Inputs) -> (String, i32) {
    let segs = segments(ledger);
    if segs.is_empty() {
        return (format!("账本还没有记录（{shown}）\n"), 1);
    }
    let mut card = Card::new();
    for p in &segs {
        let Ok(f) = std::fs::File::open(p) else {
            continue;
        };
        // line by line: a ledger is up to 16 MB and u60d is resident
        card.segment();
        let mut r = std::io::BufReader::new(f);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match r.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    for l in guard_lines(&buf) {
                        card.line(&l);
                    }
                }
            }
        }
    }
    card.finish(inp)
}

/// The scorecard of the core's long-term layer.
pub fn core(dir: &Path, inp: &Inputs) -> (String, i32) {
    let mut card = Card::new();
    for l in super::core::lines(&read(dir, "hours.jsonl"), &read(dir, "events.jsonl")) {
        card.line(&l);
    }
    card.finish(inp)
}

/// Hour-family lines go by their end (`t` is the hour's `tt`): an hour that ends up to half an
/// hour after the cut is the old side's (the guard's hour turns are a few seconds late).
fn hourly(k: &str) -> bool {
    matches!(k, "hour" | "hour_power" | "hour_proc")
}

/// Whether a line with a clock is before the cut.
fn before(k: &str, t: f64, cut: f64) -> bool {
    if hourly(k) {
        t <= cut + 1800.0
    } else {
        t < cut
    }
}

/// A line's wall clock for the cut: `t`; an hour-family line without one (the guard's backfilled
/// last hour of a boot) by its `tt`, else `ft`.
fn clock_of(l: &str) -> Option<f64> {
    use super::awk::{fld, num};
    let ok = |v: &str| !v.is_empty() && v != "null";
    let t = fld(l, "t");
    if ok(t) {
        return Some(num(t));
    }
    if hourly(fld(l, "k")) {
        let tt = fld(l, "tt");
        if ok(tt) {
            return Some(num(tt));
        }
        let ft = fld(l, "ft");
        if ok(ft) {
            // its end is no earlier than its start: on the old side only when that is too
            return Some(num(ft));
        }
    }
    None
}

/// The scorecard over both sources (u60-sentinel.md §19 T9d): the guard ledger before `cut`
/// (wall clock), the core's long-term layer (`long/`) from it, so the core has nothing to build
/// up before the guard's ledger stops. The core's boots take the guard's `seq` by boot id (the
/// segment's file name `boot-<seq>-<boot8>-…`); boots the guard never saw come after. A line
/// without a clock goes by when its boot started (from that boot's lines with one): before the
/// cut, or unknown, the guard's; else the core's, as are lines of boots the guard has no record of.
pub fn stitch(ledger: &Path, long: &Path, cut: f64, inp: &Inputs) -> (String, i32) {
    use super::awk::{fld, num};
    let segs = segments(ledger);
    let mut known: HashMap<String, i64> = HashMap::new();
    let mut start: HashMap<i64, f64> = HashMap::new();
    let each = |p: &Path, f: &mut dyn FnMut(&str)| {
        let Ok(fh) = std::fs::File::open(p) else {
            return;
        };
        let mut r = std::io::BufReader::new(fh);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match r.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    for l in guard_lines(&buf) {
                        f(&l);
                    }
                }
            }
        }
    };
    // pass 1: boot ids and when each boot started
    for p in &segs {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let mut it = name.split('-').skip(1);
        if let (Some(s), Some(b)) = (it.next(), it.next())
            && let Ok(s) = s.parse::<i64>()
        {
            known.insert(b.to_string(), s);
        }
        each(p, &mut |l| {
            let t = fld(l, "t");
            if !t.is_empty() && t != "null" && !fld(l, "seq").is_empty() {
                let b = num(t) - num(fld(l, "up")).trunc();
                let e = start.entry(num(fld(l, "seq")) as i64).or_insert(b);
                *e = e.min(b);
            }
        });
    }
    let mut card = Card::new();
    for p in &segs {
        card.segment();
        each(p, &mut |l| {
            let keep = match clock_of(l) {
                Some(t) => before(fld(l, "k"), t, cut),
                None => start
                    .get(&(num(fld(l, "seq")) as i64))
                    .is_none_or(|&b| b < cut),
            };
            if keep {
                card.line(l);
            }
        });
    }
    card.segment();
    let core = super::core::lines_seq(
        &read(long, "hours.jsonl"),
        &read(long, "events.jsonl"),
        &known,
    );
    // the core's own boot starts, for its lines without a clock
    let mut cstart: HashMap<i64, f64> = HashMap::new();
    for l in &core {
        let t = fld(l, "t");
        if t != "null" {
            let b = num(t) - num(fld(l, "up")).trunc();
            let e = cstart.entry(num(fld(l, "seq")) as i64).or_insert(b);
            *e = e.min(b);
        }
    }
    let guard_seqs: std::collections::HashSet<i64> = known.values().copied().collect();
    for l in &core {
        let s = num(fld(l, "seq")) as i64;
        let keep = if let Some(t) = clock_of(l) {
            !before(fld(l, "k"), t, cut)
        } else {
            !guard_seqs.contains(&s) || start.get(&s).or(cstart.get(&s)).is_some_and(|&b| b >= cut)
        };
        if keep {
            card.line(l);
        }
    }
    card.finish(inp)
}

/// `--scorecard-replay-core <dir>`: the fixture's side inputs over the core's long-term layer in
/// `<dir>/long/` (the second leg: tools/scorecard-parity/leg2.sh).
pub fn run_core(dir: &Path) -> (String, i32) {
    let (inp, _) = inputs(dir);
    core(&dir.join("long"), &inp)
}

/// `--scorecard-replay-stitch <dir> <cut>`: the fixture's side inputs over its guard ledger
/// before `cut` and the core's long-term layer in `<dir>/long/` from it.
pub fn run_stitch(dir: &Path, cut: f64) -> (String, i32) {
    let (inp, _) = inputs(dir);
    stitch(&dir.join("ledger"), &dir.join("long"), cut, &inp)
}

/// `--scorecard-replay <dir>`.
pub fn run(dir: &Path) -> (String, i32) {
    let (inp, shown) = inputs(dir);
    guard(&dir.join("ledger"), &shown, &inp)
}
