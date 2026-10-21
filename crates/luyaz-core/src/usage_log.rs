//! Per-day, per-card data usage (E3 travel assistant; docs/designs/travel-assist.md §4).
//!
//! The firmware keeps one counter set for the data connection and does not split it by card
//! (`get_wwandst` ignores `subid`: both slots read the same numbers, u60g 10-21). So we split it
//! ourselves: every [`EVERY`] the session counters of the snapshot (`traffic.rx_bytes/tx_bytes/
//! session_time` = `get_wwandst type 1`, read each round anyway) are diffed against the last
//! reading, and the delta goes to the card that is the data card now (`sim.iccid`). A card is
//! keyed by the first 16 hex of sha256(ICCID), as `verified.rs`; the number itself is never
//! written, only its last 4 digits for display.
//!
//! - Session counters going down (or `session_time` going down): a new session, the delta is
//!   the new reading itself (it counted from zero).
//! - A card change inside one interval: the whole delta goes to the card seen at the end.
//! - Days are device dates (`dsds_log::date`), gated on the sentinel's clock evidence.
//!
//! Files in [`DIR`]: `today.json` (the open day + the baseline, rewritten at most every
//! [`SAVE_EVERY`] and at a day change) and `days.jsonl` (one closed line per day per card,
//! appended at a day change, older than [`KEEP_DAYS`] dropped then). A core restart picks the
//! baseline up from `today.json`, so the traffic in between is not lost.
//!
//! Landing (§5, the travel card): `landing.json` in the same dir keeps the serving country
//! (`net.serving_mcc`) of the last arrival, its time and the bytes used since (all cards). A new
//! country seen [`LAND_AFTER`] rounds in a row starts a new record with the old one's country as
//! `from`; the very first record has no `from` (installing at home is not a landing).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Mutex, Once};
use std::time::Duration;

pub const DIR: &str = "/data/luyaz/core/usage";
pub const EVERY: Duration = Duration::from_secs(60);
pub const SAVE_EVERY: i64 = 3600;
pub const KEEP_DAYS: i64 = 400;
/// Rounds in a row in the new country before it counts (a border cell flicker is not a landing).
pub const LAND_AFTER: u32 = 3;

/// The last landing (`landing.json`, `GET /v2/usage/landing`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Landing {
    /// Serving MCC now (0: no record yet).
    pub mcc: i64,
    /// The country before (`None`: the first record, not a landing).
    #[serde(default)]
    pub from: Option<i64>,
    /// Device time of the landing.
    pub at: i64,
    pub rx: u64,
    pub tx: u64,
    /// A different country seen this many rounds in a row (not saved).
    #[serde(skip)]
    pub seen: Option<(i64, u32)>,
}

/// The serving MCC (`signal.serving_mcc`; the old `net` block has only the raw `mcc`, used when
/// it names a country). `None`: not registered or unknown.
pub fn serving_mcc(fields: &serde_json::Map<String, Value>) -> Option<i64> {
    let known = |m: &i64| {
        u16::try_from(*m)
            .ok()
            .and_then(luyaz_api::netinfo::mcc_iso)
            .is_some()
    };
    fields
        .get("signal")
        .and_then(|n| n["serving_mcc"].as_i64())
        .or_else(|| fields.get("net").and_then(|n| n["mcc"].as_i64()))
        .filter(known)
}

/// One country (310–316 are all the US, 440/441 Japan, …).
fn same_country(a: i64, b: i64) -> bool {
    let iso = |m: i64| u16::try_from(m).ok().and_then(luyaz_api::netinfo::mcc_iso);
    a == b || iso(a).is_some_and(|x| iso(b) == Some(x))
}

/// Fold one round into the landing record: the bytes of this round (`rx`, `tx`) count towards
/// the record in force; a new country confirmed [`LAND_AFTER`] times starts a new one at `t`.
/// Returns true when the record changed in a way worth saving now (a new landing).
pub fn land(l: &mut Landing, mcc: Option<i64>, t: i64, rx: u64, tx: u64) -> bool {
    if l.mcc != 0 {
        l.rx += rx;
        l.tx += tx;
    }
    let Some(m) = mcc else {
        return false;
    };
    if l.mcc == 0 {
        *l = Landing {
            mcc: m,
            from: None,
            at: t,
            ..Landing::default()
        };
        return true;
    }
    if same_country(m, l.mcc) {
        l.seen = None;
        return false;
    }
    let n = match l.seen {
        Some((s, n)) if same_country(s, m) => n + 1,
        _ => 1,
    };
    if n < LAND_AFTER {
        l.seen = Some((m, n));
        return false;
    }
    *l = Landing {
        mcc: m,
        from: Some(l.mcc),
        at: t,
        ..Landing::default()
    };
    true
}

/// One reading taken from the snapshot's fields.
#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    pub t: i64,
    pub rx: u64,
    pub tx: u64,
    pub session: u64,
    /// `None` when no ICCID is known (no SIM, SIM not ready).
    pub card: Option<Card>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Card {
    pub id: String,
    pub tail: String,
    pub op: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Bytes {
    pub tail: String,
    pub op: String,
    pub rx: u64,
    pub tx: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Base {
    pub rx: u64,
    pub tx: u64,
    pub session: u64,
}

/// The open day: what `today.json` holds.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Today {
    pub day: String,
    pub cards: BTreeMap<String, Bytes>,
    #[serde(default)]
    pub base: Option<Base>,
}

pub fn card_id(iccid: &str) -> String {
    let h = Sha256::digest(iccid.as_bytes());
    h.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// The reading in `fields`, `None` when the traffic counters are missing.
pub fn reading(fields: &serde_json::Map<String, Value>, t: i64) -> Option<Reading> {
    let tr = fields.get("traffic")?;
    let n = |k: &str| tr[k].as_i64().filter(|v| *v >= 0).map(|v| v as u64);
    let (rx, tx, session) = (
        n("rx_bytes")?,
        n("tx_bytes")?,
        n("session_time").unwrap_or(0),
    );
    let iccid = fields
        .get("sim")
        .and_then(|s| s["iccid"].as_str())
        .unwrap_or("")
        .trim();
    let card = (!iccid.is_empty()).then(|| Card {
        id: card_id(iccid),
        tail: iccid
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect(),
        op: fields
            .get("net")
            .and_then(|n| n["operator"].as_str())
            .unwrap_or("")
            .to_owned(),
    });
    Some(Reading {
        t,
        rx,
        tx,
        session,
        card,
    })
}

/// Bytes since `base` (a new session when anything went down).
pub fn delta(base: &Base, r: &Reading) -> (u64, u64) {
    if r.rx < base.rx || r.tx < base.tx || r.session < base.session {
        (r.rx, r.tx)
    } else {
        (r.rx - base.rx, r.tx - base.tx)
    }
}

/// Fold one reading into the open day. Returns the closed day when the date moved on (its
/// cards are the lines for `days.jsonl`). The delta of the interval that crosses midnight goes
/// to the new day.
pub fn step(today: &mut Today, r: &Reading, day: &str) -> Option<Today> {
    let closed = if today.day != day {
        let base = today.base.take();
        let old = std::mem::replace(
            today,
            Today {
                day: day.to_owned(),
                cards: BTreeMap::new(),
                base,
            },
        );
        (!old.day.is_empty() && !old.cards.is_empty()).then_some(old)
    } else {
        None
    };
    if let (Some(base), Some(card)) = (&today.base, &r.card) {
        let (rx, tx) = delta(base, r);
        if rx > 0 || tx > 0 {
            let e = today.cards.entry(card.id.clone()).or_default();
            e.rx += rx;
            e.tx += tx;
            e.tail.clone_from(&card.tail);
            if !card.op.is_empty() {
                e.op.clone_from(&card.op);
            }
        }
    }
    today.base = Some(Base {
        rx: r.rx,
        tx: r.tx,
        session: r.session,
    });
    closed
}

/// The `days.jsonl` lines of a closed day.
pub fn lines(d: &Today) -> Vec<Value> {
    d.cards
        .iter()
        .map(|(id, b)| json!({"day": d.day, "card": id, "tail": b.tail, "op": b.op, "rx": b.rx, "tx": b.tx}))
        .collect()
}

/// `days.jsonl` text without the lines older than [`KEEP_DAYS`] at `t`.
pub fn pruned(text: &str, t: i64) -> String {
    let oldest = crate::dsds_log::date(t - (KEEP_DAYS - 1) * 86_400);
    text.lines()
        .filter(|l| {
            serde_json::from_str::<Value>(l)
                .ok()
                .and_then(|v| v["day"].as_str().map(|d| d >= oldest.as_str()))
                .unwrap_or(false)
        })
        .map(|l| format!("{l}\n"))
        .collect()
}

// ---- IO ----

static LATEST: Mutex<Option<serde_json::Map<String, Value>>> = Mutex::new(None);
static START: Once = Once::new();

/// Called from each collection round with the snapshot's fields; starts the thread once.
pub fn offer(fields: &serde_json::Map<String, Value>) {
    let keep: serde_json::Map<String, Value> = ["traffic", "sim", "net", "signal"]
        .iter()
        .filter_map(|k| fields.get(*k).map(|v| ((*k).to_owned(), v.clone())))
        .collect();
    *LATEST.lock().unwrap_or_else(|e| e.into_inner()) = Some(keep);
    if cfg!(not(test)) {
        START.call_once(|| {
            let _ = std::thread::Builder::new()
                .name("usage-log".into())
                .spawn(|| run(Path::new(DIR)));
        });
    }
}

fn load(dir: &Path) -> Today {
    std::fs::read_to_string(dir.join("today.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save(dir: &Path, today: &Today) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join("today.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(today).unwrap_or_default())?;
    std::fs::rename(tmp, dir.join("today.json"))
}

fn close(dir: &Path, d: &Today, t: i64) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let p = dir.join("days.jsonl");
    let mut text = pruned(&std::fs::read_to_string(&p).unwrap_or_default(), t);
    for l in lines(d) {
        text.push_str(&format!("{l}\n"));
    }
    let tmp = dir.join("days.jsonl.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, p)
}

fn run(dir: &Path) {
    let mut today = load(dir);
    let mut saved = 0i64;
    let mut landing = load_landing(dir);
    let mut landing_saved = 0i64;
    loop {
        std::thread::sleep(EVERY);
        let fields = LATEST.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let t = crate::qmi_nas::now() as i64;
        let Some(r) = fields.as_ref().and_then(|f| reading(f, t)) else {
            continue;
        };
        if !crate::dsds_log::clock_ok(t) {
            continue;
        }
        let day = crate::dsds_log::date(t);
        let (rx, tx) = today.base.as_ref().map_or((0, 0), |b| delta(b, &r));
        let landed = land(
            &mut landing,
            fields.as_ref().and_then(serving_mcc),
            t,
            rx,
            tx,
        );
        if (landed || t - landing_saved >= SAVE_EVERY) && save_landing(dir, &landing).is_ok() {
            landing_saved = t;
        }
        *LANDING.lock().unwrap_or_else(|e| e.into_inner()) = Some(landing.clone());
        let closed = step(&mut today, &r, &day);
        if let Some(d) = &closed {
            let _ = close(dir, d, t);
        }
        if (closed.is_some() || t - saved >= SAVE_EVERY || saved == 0) && save(dir, &today).is_ok()
        {
            saved = t;
        }
        *TODAY.lock().unwrap_or_else(|e| e.into_inner()) = Some(today.clone());
    }
}

static TODAY: Mutex<Option<Today>> = Mutex::new(None);
static LANDING: Mutex<Option<Landing>> = Mutex::new(None);

fn load_landing(dir: &Path) -> Landing {
    std::fs::read_to_string(dir.join("landing.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_landing(dir: &Path, l: &Landing) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join("landing.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(l).unwrap_or_default())?;
    std::fs::rename(tmp, dir.join("landing.json"))
}

/// `GET /v2/usage/landing`: `{mcc, from, at, rx, tx}` (from memory, or `landing.json` before
/// the first round), `null` before any record.
pub fn landing(dir: &Path) -> Value {
    let l = LANDING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_else(|| load_landing(dir));
    if l.mcc == 0 {
        return Value::Null;
    }
    json!({"mcc": l.mcc, "from": l.from, "at": l.at, "rx": l.rx, "tx": l.tx})
}

// ---- the read side (`GET /v2/usage/days`) ----

/// `{days: [{day, cards: [{card, tail, op, rx, tx}]}]}`, newest first, the open day included
/// (from memory, or `today.json` before the first round), at most `n` (1..=[`KEEP_DAYS`]) days.
pub fn days(dir: &Path, n: i64) -> Value {
    let n = n.clamp(1, KEEP_DAYS) as usize;
    let today = TODAY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_else(|| load(dir));
    let mut by_day: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for l in std::fs::read_to_string(dir.join("days.jsonl"))
        .unwrap_or_default()
        .lines()
    {
        if let Ok(v) = serde_json::from_str::<Value>(l)
            && let Some(d) = v["day"].as_str()
        {
            let d = d.to_owned();
            let mut c = v;
            if let Some(o) = c.as_object_mut() {
                o.remove("day");
            }
            by_day.entry(d).or_default().push(c);
        }
    }
    if !today.day.is_empty() {
        let mut open = lines(&today);
        for c in &mut open {
            if let Some(o) = c.as_object_mut() {
                o.remove("day");
            }
        }
        by_day.insert(today.day.clone(), open);
    }
    let out: Vec<Value> = by_day
        .into_iter()
        .rev()
        .take(n)
        .map(|(day, cards)| json!({"day": day, "cards": cards}))
        .collect();
    json!({"days": out})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(t: i64, rx: u64, tx: u64, session: u64, card: Option<&str>) -> Reading {
        Reading {
            t,
            rx,
            tx,
            session,
            card: card.map(|c| Card {
                id: c.into(),
                tail: format!("{c}-t"),
                op: "OP".into(),
            }),
        }
    }

    #[test]
    fn reading_from_fields() {
        let f = json!({
            "traffic": {"rx_bytes": 100, "tx_bytes": 20, "session_time": 5},
            "sim": {"iccid": "8986001234567890123"},
            "net": {"operator": "CUCC"},
        });
        let x = reading(f.as_object().unwrap(), 9).unwrap();
        assert_eq!((x.rx, x.tx, x.session), (100, 20, 5));
        let c = x.card.unwrap();
        assert_eq!(
            (c.tail.as_str(), c.op.as_str(), c.id.len()),
            ("0123", "CUCC", 16)
        );
        assert_eq!(c.id, card_id("8986001234567890123"));
        let none = json!({"traffic": {"rx_bytes": 1, "tx_bytes": 1}, "sim": {"iccid": ""}});
        assert!(
            reading(none.as_object().unwrap(), 9)
                .unwrap()
                .card
                .is_none()
        );
        assert!(reading(json!({}).as_object().unwrap(), 9).is_none());
    }

    #[test]
    fn first_reading_is_only_a_baseline() {
        let mut d = Today::default();
        assert!(step(&mut d, &r(0, 500, 50, 10, Some("a")), "2026-10-21").is_none());
        assert!(d.cards.is_empty());
        step(&mut d, &r(60, 800, 70, 70, Some("a")), "2026-10-21");
        assert_eq!((d.cards["a"].rx, d.cards["a"].tx), (300, 20));
    }

    #[test]
    fn new_session_counts_from_zero() {
        let mut d = Today::default();
        step(&mut d, &r(0, 500, 50, 100, Some("a")), "2026-10-21");
        step(&mut d, &r(60, 40, 4, 30, Some("a")), "2026-10-21");
        assert_eq!((d.cards["a"].rx, d.cards["a"].tx), (40, 4));
    }

    #[test]
    fn card_switch_splits() {
        let mut d = Today::default();
        step(&mut d, &r(0, 0, 0, 0, Some("a")), "2026-10-21");
        step(&mut d, &r(60, 100, 10, 60, Some("a")), "2026-10-21");
        // data card moved to b: firmware reconnects, a new session
        step(&mut d, &r(120, 30, 3, 20, Some("b")), "2026-10-21");
        step(&mut d, &r(180, 90, 9, 80, Some("b")), "2026-10-21");
        assert_eq!((d.cards["a"].rx, d.cards["b"].rx), (100, 90));
        assert_eq!(lines(&d).len(), 2);
    }

    #[test]
    fn no_card_keeps_baseline_only() {
        let mut d = Today::default();
        step(&mut d, &r(0, 0, 0, 0, None), "2026-10-21");
        step(&mut d, &r(60, 100, 10, 60, None), "2026-10-21");
        assert!(d.cards.is_empty());
        step(&mut d, &r(120, 150, 15, 120, Some("a")), "2026-10-21");
        assert_eq!(d.cards["a"].rx, 50);
    }

    #[test]
    fn day_change_closes() {
        let mut d = Today::default();
        step(&mut d, &r(0, 0, 0, 0, Some("a")), "2026-10-21");
        step(&mut d, &r(60, 100, 10, 60, Some("a")), "2026-10-21");
        let closed = step(&mut d, &r(120, 130, 13, 120, Some("a")), "2026-10-22").unwrap();
        assert_eq!(
            (closed.day.as_str(), closed.cards["a"].rx),
            ("2026-10-21", 100)
        );
        assert_eq!((d.day.as_str(), d.cards["a"].rx), ("2026-10-22", 30));
        assert_eq!(lines(&closed)[0]["day"], "2026-10-21");
    }

    #[test]
    fn restart_resumes_from_saved_baseline() {
        let mut d = Today::default();
        step(&mut d, &r(0, 0, 0, 0, Some("a")), "2026-10-21");
        step(&mut d, &r(60, 100, 10, 60, Some("a")), "2026-10-21");
        let mut back: Today = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
        step(&mut back, &r(4000, 900, 90, 4000, Some("a")), "2026-10-21");
        assert_eq!(back.cards["a"].rx, 900);
    }

    #[test]
    fn serving_mcc_from_the_snapshot() {
        let f = |v: Value| v.as_object().cloned().unwrap();
        // the real shape: `net` without serving_mcc, `signal` with it
        assert_eq!(
            serving_mcc(&f(
                json!({"net": {"mcc": 460}, "signal": {"mcc": 460, "serving_mcc": 460}})
            )),
            Some(460)
        );
        assert_eq!(serving_mcc(&f(json!({"net": {"mcc": 440}}))), Some(440));
        assert_eq!(
            serving_mcc(&f(
                json!({"net": {"mcc": 0}, "signal": {"serving_mcc": null}})
            )),
            None
        );
    }

    #[test]
    fn landing_needs_three_rounds_and_counts_since() {
        let mut l = Landing::default();
        // first record at home: not a landing, nothing counted before it
        assert!(land(&mut l, Some(460), 100, 5, 5));
        assert_eq!((l.mcc, l.from, l.rx), (460, None, 0));
        assert!(!land(&mut l, None, 160, 10, 1));
        // 310 and 311 are one country: never a landing
        let mut us = Landing {
            mcc: 310,
            from: Some(460),
            at: 1,
            ..Landing::default()
        };
        for t in 0..5 {
            assert!(!land(&mut us, Some(311), t, 0, 0));
        }
        assert_eq!((l.rx, l.tx), (10, 1));
        // a flicker to 440 and back resets
        assert!(!land(&mut l, Some(440), 220, 0, 0));
        assert!(!land(&mut l, Some(460), 280, 0, 0));
        assert!(!land(&mut l, Some(440), 340, 0, 0));
        assert!(!land(&mut l, Some(440), 400, 0, 0));
        assert!(land(&mut l, Some(440), 460, 7, 0));
        assert_eq!((l.mcc, l.from, l.at, l.rx), (440, Some(460), 460, 0));
        assert!(!land(&mut l, Some(440), 520, 3, 2));
        assert_eq!((l.rx, l.tx), (3, 2));
        let dir = std::env::temp_dir().join(format!("usage-land-{}", std::process::id()));
        save_landing(&dir, &l).unwrap();
        assert_eq!(load_landing(&dir).from, Some(460));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_and_read() {
        let t = 1_792_584_000; // 2026-10-21 12:00 device clock
        let old = json!({"day": "2025-01-01", "card": "a", "rx": 1, "tx": 1});
        let keep =
            json!({"day": "2026-10-20", "card": "a", "tail": "1", "op": "", "rx": 5, "tx": 1});
        assert_eq!(
            pruned(&format!("{old}\n{keep}\nbad\n"), t),
            format!("{keep}\n")
        );
        let dir = std::env::temp_dir().join(format!("usage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut d = Today {
            day: "2026-10-20".into(),
            ..Default::default()
        };
        d.cards.insert(
            "a".into(),
            Bytes {
                tail: "1".into(),
                op: "".into(),
                rx: 5,
                tx: 1,
            },
        );
        close(&dir, &d, t).unwrap();
        let mut open = Today {
            day: "2026-10-21".into(),
            ..Default::default()
        };
        open.cards.insert(
            "b".into(),
            Bytes {
                rx: 7,
                ..Default::default()
            },
        );
        save(&dir, &open).unwrap();
        let v = days(&dir, 30);
        assert_eq!(v["days"][0]["day"], "2026-10-21");
        assert_eq!(v["days"][0]["cards"][0]["rx"], 7);
        assert_eq!(v["days"][1]["cards"][0]["card"], "a");
        assert_eq!(days(&dir, 1)["days"].as_array().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
