//! Signal history: one record per minute and an event list, kept in memory for
//! 24 h and on disk for 7 days (`docs/designs/slow-diagnosis.md` §4.3, which
//! amends `docs/designs/signal-history.md`; eng review 10-02 D6/D12).
//!
//! Fed by the netwatch thread every 5 s with what it already read: datad's
//! `/state`, the verdict from `/v2/screen` (`net.story.state`; datad decides,
//! the agent does not) and the modem's receive counter. Nothing here
//! sends a packet.
//!
//! On disk, in [`DIR`] (`ZTE_AGENT_SIGNAL_LOG` overrides), one pair of files
//! per day of the device clock (local time labelled UTC, so the date is taken
//! from `t` as is): `YYYYMMDD.jsonl` for minutes, appended every 10 minutes,
//! and `YYYYMMDD.events.jsonl`, appended the moment an event happens — the
//! minutes before a whole-device reboot are the ones worth having. A line cut
//! short by power loss is skipped when read, and a newline is put after it
//! before the next append so it can't swallow the next event.
//!
//! Before the clock is set (year < 2024) nothing is written and nothing is
//! read back; what was recorded meanwhile is dropped on the first write after
//! the clock is set, rather than guessed at.
//!
//! `rx_peak_mbps` uses the same counter as `cell_alive`: the modem's own
//! `traffic.rx_bytes`, which (unlike `rmnet_data0`) includes client traffic
//! that goes through IPA hardware offload (checked on the device, ER1).

use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const DIR: &str = "/data/signal-log";
/// 2024-01-01 00:00 — earlier means the clock was never set.
pub const CLOCK_SET: u64 = 1_704_067_200;
pub const KEEP_MINUTES: usize = 1440;
pub const KEEP_EVENTS: usize = 2000;
pub const KEEP_DAYS: u64 = 7;
const FLUSH_EVERY: u64 = 600;
/// Two samples further apart than this (or going backwards) = a clock jump or a
/// stalled thread: the minute in progress and any running timers are dropped.
const MAX_STEP: u64 = 60;
const WEAK_ENTER: f64 = -110.0;
const WEAK_EXIT: f64 = -105.0;
const WEAK_HOLD: u64 = 30;

// ---- one 5 s reading --------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reading {
    pub at: u64,
    /// `/state` was read; everything below is meaningless when false.
    pub ok: bool,
    pub connected: bool,
    /// SA / NSA / LTE / 3G / 2G / none
    pub rat: &'static str,
    pub band: String,
    pub pci: Option<i64>,
    /// Full cell ID of the serving cell: `nr_cell_id` on SA, the (anchor) LTE
    /// `lte_cell_id` on LTE/NSA.
    pub cell: Option<i64>,
    pub channel: Option<i64>,
    /// "460-11"; None when either part is unknown.
    pub plmn: Option<String>,
    pub nr_rsrp: Option<f64>,
    pub nr_sinr: Option<f64>,
    pub nr_rsrq: Option<f64>,
    pub lte_rsrp: Option<f64>,
    pub lte_sinr: Option<f64>,
    pub lte_rsrq: Option<f64>,
    pub nrca: Option<u32>,
    pub lteca: Option<u32>,
    pub hsr: bool,
    pub qci: Option<i64>,
    pub ambr: Option<String>,
    pub rx_bytes: Option<u64>,
    /// datad's `story.state` from `/v2/screen`; None = not read.
    pub verdict: Option<String>,
}

fn num(v: Option<&Value>) -> Option<f64> {
    let x = match v? {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => s.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    x.is_finite().then_some(x)
}

/// Integer readings where 0 is datad's "not there".
fn nonzero(v: Option<&Value>) -> Option<f64> {
    num(v).filter(|x| *x != 0.0)
}

fn rat_of(t: &str) -> &'static str {
    let u = t.to_ascii_uppercase();
    if u.is_empty() || u.contains("LIMIT") || u.contains("EMERGENCY") || u.contains("NO_SERVICE") || u.contains("NOSERVICE") {
        "none"
    } else if u.contains("NSA") || u.contains("ENDC") || u.contains("EN-DC") {
        "NSA"
    } else if u == "SA" || u == "NR" || u.contains("5G") || u.starts_with("SA_") || u.starts_with("NR_") || u.contains("_SA") {
        "SA"
    } else if u.contains("LTE") || u.contains("4G") {
        "LTE"
    } else if ["WCDMA", "UMTS", "HSPA", "TD-SCDMA", "TDSCDMA", "CDMA2000", "EVDO", "EV-DO", "HRPD", "3G"].iter().any(|w| u.contains(w)) {
        "3G"
    } else if ["GSM", "GPRS", "EDGE", "2G", "CDMA", "1X"].iter().any(|w| u.contains(w)) {
        "2G"
    } else {
        "none"
    }
}

/// Carriers in datad's `nrca` / `lteca` (`;`-separated entries).
fn ca_count(v: Option<&Value>) -> Option<u32> {
    let s = v?.as_str()?;
    Some(s.split(';').filter(|p| !p.trim().is_empty() && p.trim() != "--").count() as u32)
}

impl Reading {
    pub fn parse(at: u64, state: Option<&Value>, screen: Option<&Value>, rx_bytes: Option<u64>) -> Reading {
        let verdict = screen
            .and_then(|s| s.get("net"))
            .and_then(|n| n.get("story"))
            .and_then(|s| s.get("state"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let net = state.map(|v| v.get("net").unwrap_or(v));
        let Some(wan) = net.and_then(|n| n.get("wan_status")).and_then(Value::as_str) else {
            return Reading { at, rat: "none", rx_bytes, verdict, ..Default::default() };
        };
        let net = net.unwrap();
        let g = |k: &str| net.get(k);
        let rat = rat_of(g("type").and_then(Value::as_str).unwrap_or(""));
        let nr = matches!(rat, "SA" | "NSA");
        let lte = matches!(rat, "LTE" | "NSA");
        let only = |on: bool, x: Option<f64>| if on { x } else { None };
        let int = |x: Option<f64>| x.map(|v| v as i64);
        let sa = rat == "SA";
        let mcc = int(nonzero(g("mcc")));
        let mnc = num(g("mnc")).map(|v| v as i64);
        let qos = state.and_then(|s| s.get("qos"));
        let qs = |k: &str| qos.and_then(|q| q.get(k)).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty() && *s != "--");
        let ambr = match (qs("ambr_dl"), qs("ambr_ul")) {
            (Some(d), Some(u)) => Some(format!("{d}/{u}")),
            (Some(d), None) => Some(d.to_string()),
            _ => None,
        };
        Reading {
            at,
            ok: true,
            connected: super::wan_connected(wan),
            rat,
            band: g("band").and_then(Value::as_str).unwrap_or("").to_string(),
            pci: int(if sa { nonzero(g("nr_pci")) } else if lte { nonzero(g("lte_pci")) } else { None }),
            cell: int(if sa { nonzero(g("nr_cell_id")) } else if lte { nonzero(g("lte_cell_id")) } else { None }),
            channel: int(if sa {
                nonzero(g("nr_channel"))
            } else if lte {
                nonzero(g("lte_channel")).or_else(|| nonzero(g("channel")))
            } else {
                None
            }),
            // MNC 0 is real (460-00); MCC 0 is not
            plmn: match (mcc, mnc) {
                (Some(c), Some(n)) if g("mnc").is_some() => Some(format!("{c}-{n:02}")),
                _ => None,
            },
            nr_rsrp: only(nr, nonzero(g("nr_rsrp"))),
            nr_sinr: only(nr, num(g("nr_snr"))),
            nr_rsrq: only(nr, nonzero(g("nr_rsrq"))),
            lte_rsrp: only(lte, nonzero(g("lte_rsrp"))),
            lte_sinr: only(lte, num(g("lte_snr"))),
            lte_rsrq: only(lte, nonzero(g("lte_rsrq"))),
            nrca: if nr { ca_count(g("nrca")) } else { None },
            lteca: if lte { ca_count(g("lteca")) } else { None },
            hsr: g("HSR").and_then(Value::as_bool).unwrap_or(false),
            qci: int(nonzero(qos.and_then(|q| q.get("qci")))),
            ambr,
            rx_bytes,
            verdict,
        }
    }

    /// RSRP of the radio in charge: NR on SA, LTE on LTE/NSA.
    fn main_rsrp(&self) -> Option<f64> {
        match self.rat {
            "SA" => self.nr_rsrp,
            "LTE" | "NSA" => self.lte_rsrp,
            _ => None,
        }
    }
}

// ---- records ----------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Minute {
    /// Start of the minute (device clock seconds).
    pub t: u64,
    pub rat: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub band: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pci: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ch: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plmn: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nr_rsrp_min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nr_rsrp_avg: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nr_rsrp_max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nr_sinr_min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nr_sinr_avg: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nr_rsrq: Option<f64>,
    pub nr_n: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lte_rsrp_min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lte_rsrp_avg: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lte_rsrp_max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lte_sinr_min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lte_sinr_avg: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lte_rsrq: Option<f64>,
    pub lte_n: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nrca: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lteca: Option<u32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hsr: bool,
    /// Successful `/state` reads (≤ 12).
    pub n: u32,
    /// Seconds without a data call.
    pub down_s: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rx_peak_mbps: Option<f64>,
    /// Most frequent `story.state`; "-" when `/v2/screen` was never read.
    pub verdict: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub t: u64,
    pub kind: String,
    /// Kind-specific fields (from/to, duration …), flattened into the line.
    #[serde(flatten)]
    pub data: BTreeMap<String, Value>,
}

impl Event {
    pub fn new(t: u64, kind: &str, data: Value) -> Event {
        let data = match data {
            Value::Object(m) => m.into_iter().filter(|(_, v)| !v.is_null()).collect(),
            _ => BTreeMap::new(),
        };
        Event { t, kind: kind.to_string(), data }
    }
}

#[derive(Default)]
struct Stats {
    n: u32,
    sum: f64,
    min: f64,
    max: f64,
}

impl Stats {
    fn add(&mut self, x: Option<f64>) {
        let Some(x) = x else { return };
        if self.n == 0 || x < self.min {
            self.min = x;
        }
        if self.n == 0 || x > self.max {
            self.max = x;
        }
        self.n += 1;
        self.sum += x;
    }
    fn avg(&self) -> Option<f64> {
        (self.n > 0).then(|| round1(self.sum / f64::from(self.n)))
    }
    fn min(&self) -> Option<f64> {
        (self.n > 0).then_some(self.min)
    }
    fn max(&self) -> Option<f64> {
        (self.n > 0).then_some(self.max)
    }
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

fn most(counts: &BTreeMap<String, u32>) -> Option<&str> {
    // ties: the later key in sort order loses, so the result is stable
    counts.iter().max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0))).map(|(k, _)| k.as_str())
}

#[derive(Default)]
struct Acc {
    t: u64,
    rats: BTreeMap<String, u32>,
    verdicts: BTreeMap<String, u32>,
    last: Option<Reading>,
    nr_rsrp: Stats,
    nr_sinr: Stats,
    nr_rsrq: Stats,
    lte_rsrp: Stats,
    lte_sinr: Stats,
    lte_rsrq: Stats,
    hsr: bool,
    n: u32,
    down_s: u32,
    rx_peak: Option<f64>,
}

impl Acc {
    fn finish(self) -> Minute {
        let l = self.last.unwrap_or_default();
        Minute {
            t: self.t,
            rat: most(&self.rats).unwrap_or("none").to_string(),
            band: l.band,
            pci: l.pci,
            cell: l.cell,
            ch: l.channel,
            plmn: l.plmn,
            nr_rsrp_min: self.nr_rsrp.min(),
            nr_rsrp_avg: self.nr_rsrp.avg(),
            nr_rsrp_max: self.nr_rsrp.max(),
            nr_sinr_min: self.nr_sinr.min(),
            nr_sinr_avg: self.nr_sinr.avg(),
            nr_rsrq: self.nr_rsrq.avg(),
            nr_n: self.nr_rsrp.n,
            lte_rsrp_min: self.lte_rsrp.min(),
            lte_rsrp_avg: self.lte_rsrp.avg(),
            lte_rsrp_max: self.lte_rsrp.max(),
            lte_sinr_min: self.lte_sinr.min(),
            lte_sinr_avg: self.lte_sinr.avg(),
            lte_rsrq: self.lte_rsrq.avg(),
            lte_n: self.lte_rsrp.n,
            nrca: l.nrca,
            lteca: l.lteca,
            hsr: self.hsr,
            n: self.n,
            down_s: self.down_s,
            rx_peak_mbps: self.rx_peak,
            verdict: most(&self.verdicts).unwrap_or("-").to_string(),
        }
    }
}

/// Hysteresis on the main radio's RSRP: below −110 for 30 s enters, above −105
/// for 30 s leaves. A reading without RSRP neither confirms nor breaks a run.
#[derive(Default)]
struct Weak {
    on: bool,
    since: Option<u64>,
    worst: Option<f64>,
}

// ---- the recorder (pure: time comes with each reading) ----------------------

#[derive(Default)]
pub struct Recorder {
    pub minutes: VecDeque<Minute>,
    pub events: VecDeque<Event>,
    acc: Option<Acc>,
    /// Last successful reading, what events compare against.
    prev_ok: Option<Reading>,
    /// Last reading of any kind, for step checks and rx rates.
    prev: Option<Reading>,
    down_since: Option<u64>,
    stalled_since: Option<u64>,
    weak: Weak,
    datad_lost: bool,
}

impl Recorder {
    /// Fold one reading in. Returns the events it caused and the minute it
    /// closed, if any (both already kept in memory).
    pub fn feed(&mut self, r: &Reading) -> (Vec<Event>, Option<Minute>) {
        let mut ev = Vec::new();
        let step = self.prev.as_ref().map(|p| r.at as i64 - p.at as i64);
        let jumped = step.is_some_and(|s| s <= 0 || s > MAX_STEP as i64);
        if jumped {
            // no duration or minute is computed across a clock jump
            self.acc = None;
            self.down_since = None;
            self.stalled_since = None;
            self.weak.since = None;
        }

        // minute boundary
        let mut closed = None;
        let mstart = r.at - r.at % 60;
        if self.acc.as_ref().is_some_and(|a| a.t != mstart) {
            let m = self.acc.take().unwrap().finish();
            self.minutes.push_back(m.clone());
            while self.minutes.len() > KEEP_MINUTES {
                self.minutes.pop_front();
            }
            closed = Some(m);
        }
        let acc = self.acc.get_or_insert_with(|| Acc { t: mstart, ..Default::default() });

        // receive rate, from the counter (bytes since the previous reading)
        if let (false, Some(p), Some(b)) = (jumped, self.prev.as_ref(), r.rx_bytes) {
            if let (Some(a), Some(s)) = (p.rx_bytes, step) {
                if b >= a && s > 0 {
                    let mbps = round1((b - a) as f64 * 8.0 / s as f64 / 1e6);
                    acc.rx_peak = Some(acc.rx_peak.map_or(mbps, |x: f64| x.max(mbps)));
                }
            }
        }
        if let Some(v) = &r.verdict {
            *acc.verdicts.entry(v.clone()).or_default() += 1;
        }

        if !r.ok {
            if !self.datad_lost && self.prev_ok.is_some() {
                ev.push(Event::new(r.at, "datad_lost", json!({})));
            }
            self.datad_lost = true;
            self.prev = Some(r.clone());
            return (self.keep(ev), closed);
        }

        acc.n += 1;
        *acc.rats.entry(r.rat.to_string()).or_default() += 1;
        acc.nr_rsrp.add(r.nr_rsrp);
        acc.nr_sinr.add(r.nr_sinr);
        acc.nr_rsrq.add(r.nr_rsrq);
        acc.lte_rsrp.add(r.lte_rsrp);
        acc.lte_sinr.add(r.lte_sinr);
        acc.lte_rsrq.add(r.lte_rsrq);
        acc.hsr |= r.hsr;
        if !r.connected {
            let s = step.filter(|_| !jumped).unwrap_or(5).clamp(0, MAX_STEP as i64) as u32;
            acc.down_s = (acc.down_s + s).min(60);
        }
        acc.last = Some(r.clone());

        if self.datad_lost {
            ev.push(Event::new(r.at, "datad_back", json!({})));
            self.datad_lost = false;
        }
        if let Some(p) = self.prev_ok.clone() {
            self.changes(&p, r, &mut ev);
        }
        self.weak(r, &mut ev);
        self.prev_ok = Some(r.clone());
        self.prev = Some(r.clone());
        (self.keep(ev), closed)
    }

    fn changes(&mut self, p: &Reading, r: &Reading, ev: &mut Vec<Event>) {
        let cell_of = |x: &Reading| json!({"band": x.band, "pci": x.pci, "cell": x.cell, "ch": x.channel, "plmn": x.plmn});
        if r.rat != p.rat {
            ev.push(Event::new(r.at, "rat_change", json!({"from": p.rat, "to": r.rat})));
        }
        let known = |x: &Reading| x.pci.is_some() && !x.band.is_empty();
        if known(p) && known(r) && (p.pci != r.pci || p.band != r.band) {
            ev.push(Event::new(r.at, "cell_change", json!({"from": cell_of(p), "to": cell_of(r)})));
        }
        let ca = |x: &Reading| match (x.nrca, x.lteca) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
        };
        if let (Some(a), Some(b)) = (ca(p), ca(r)) {
            if a != b && r.rat == p.rat {
                ev.push(Event::new(r.at, "ca_change", json!({"from": a, "to": b})));
            }
        }
        if p.connected && !r.connected {
            self.down_since = Some(r.at);
            ev.push(Event::new(r.at, "data_down", json!({"rat": r.rat})));
        } else if !p.connected && r.connected {
            let secs = self.down_since.take().map(|s| r.at.saturating_sub(s));
            ev.push(Event::new(r.at, "data_up", json!({"down_s": secs})));
        }
        let stall = |x: &Reading| x.verdict.as_deref() == Some("stall");
        let known_v = |x: &Reading| x.verdict.is_some();
        if known_v(p) && known_v(r) {
            if !stall(p) && stall(r) {
                self.stalled_since = Some(r.at);
                ev.push(Event::new(r.at, "stall", json!({"rat": r.rat, "cell": cell_of(r)})));
            } else if stall(p) && !stall(r) {
                let secs = self.stalled_since.take().map(|s| r.at.saturating_sub(s));
                ev.push(Event::new(r.at, "stall_end", json!({"secs": secs, "now": r.verdict})));
            }
        }
        if p.qci.is_some() && r.qci.is_some() && p.ambr.is_some() && r.ambr.is_some() && (p.qci != r.qci || p.ambr != r.ambr) {
            ev.push(Event::new(
                r.at,
                "qos_change",
                json!({"from": {"qci": p.qci, "ambr": p.ambr}, "to": {"qci": r.qci, "ambr": r.ambr}}),
            ));
        }
    }

    fn weak(&mut self, r: &Reading, ev: &mut Vec<Event>) {
        let Some(x) = r.main_rsrp() else { return };
        let w = &mut self.weak;
        let toward = if w.on { x > WEAK_EXIT } else { x < WEAK_ENTER };
        // the lowest reading from the start of the run that entered until exit
        if w.on || toward {
            w.worst = Some(w.worst.map_or(x, |m: f64| m.min(x)));
        }
        if !toward {
            w.since = None;
            if !w.on {
                w.worst = None;
            }
            return;
        }
        let since = *w.since.get_or_insert(r.at);
        if r.at.saturating_sub(since) < WEAK_HOLD {
            return;
        }
        w.since = None;
        if w.on {
            w.on = false;
            ev.push(Event::new(r.at, "weak_exit", json!({"rat": r.rat, "rsrp": x, "worst": w.worst})));
            w.worst = None;
        } else {
            w.on = true;
            ev.push(Event::new(r.at, "weak_enter", json!({"rat": r.rat, "rsrp": x})));
        }
    }

    /// Add events from elsewhere (deep diagnosis, proxy fallback …).
    pub fn push_event(&mut self, e: Event) {
        self.keep(vec![e]);
    }

    fn keep(&mut self, ev: Vec<Event>) -> Vec<Event> {
        for e in &ev {
            self.events.push_back(e.clone());
        }
        while self.events.len() > KEEP_EVENTS {
            self.events.pop_front();
        }
        ev
    }

    /// After a clock set: whatever was recorded with a bogus clock goes.
    pub fn drop_unset_clock(&mut self) {
        self.minutes.retain(|m| m.t >= CLOCK_SET);
        self.events.retain(|e| e.t >= CLOCK_SET);
    }
}

// ---- disk -------------------------------------------------------------------

/// Days since 1970-01-01 → (y, m, d) (proleptic Gregorian).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

pub fn day_name(t: u64) -> String {
    let (y, m, d) = civil((t / 86_400) as i64);
    format!("{y:04}{m:02}{d:02}")
}

pub struct Store {
    dir: PathBuf,
    /// Files this process has already appended to (their tail is known whole).
    checked: Vec<PathBuf>,
}

impl Store {
    pub fn new(dir: impl Into<PathBuf>) -> Store {
        Store { dir: dir.into(), checked: Vec::new() }
    }

    fn minutes_path(&self, day: &str) -> PathBuf {
        self.dir.join(format!("{day}.jsonl"))
    }

    fn events_path(&self, day: &str) -> PathBuf {
        self.dir.join(format!("{day}.events.jsonl"))
    }

    /// One `write_all` per call, in append mode. A tail without a newline
    /// (power cut mid-line) gets one first, once per file per process.
    fn append(&mut self, path: &Path, lines: &str) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let mut f = OpenOptions::new().create(true).append(true).read(true).open(path)?;
        let mut out = String::new();
        if !self.checked.iter().any(|p| p == path) {
            let len = f.metadata()?.len();
            if len > 0 {
                let mut last = [0u8; 1];
                f.seek(SeekFrom::Start(len - 1))?;
                f.read_exact(&mut last)?;
                if last[0] != b'\n' {
                    out.push('\n');
                }
            }
            self.checked.push(path.to_path_buf());
        }
        out.push_str(lines);
        f.write_all(out.as_bytes())
    }

    pub fn append_event(&mut self, e: &Event) -> io::Result<()> {
        let line = serde_json::to_string(e).map_err(io::Error::other)? + "\n";
        let p = self.events_path(&day_name(e.t));
        self.append(&p, &line)
    }

    pub fn append_minutes(&mut self, ms: &[Minute]) -> io::Result<()> {
        let mut by_day: BTreeMap<String, String> = BTreeMap::new();
        for m in ms {
            let line = serde_json::to_string(m).map_err(io::Error::other)?;
            let s = by_day.entry(day_name(m.t)).or_default();
            s.push_str(&line);
            s.push('\n');
        }
        for (day, lines) in by_day {
            let p = self.minutes_path(&day);
            self.append(&p, &lines)?;
        }
        Ok(())
    }

    /// Lines of one file that parse; anything else (a cut-off line) is skipped.
    fn read_lines<T: for<'de> Deserialize<'de>>(path: &Path) -> Vec<T> {
        let Ok(text) = fs::read_to_string(path) else { return Vec::new() };
        text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
    }

    /// Minutes and events with `from <= t < to`, oldest first.
    pub fn read_range(&self, from: u64, to: u64) -> (Vec<Minute>, Vec<Event>) {
        let (mut ms, mut es) = (Vec::new(), Vec::new());
        let mut day = from / 86_400;
        while day * 86_400 < to {
            let name = day_name(day * 86_400);
            ms.extend(Self::read_lines::<Minute>(&self.minutes_path(&name)).into_iter().filter(|m| m.t >= from && m.t < to));
            es.extend(Self::read_lines::<Event>(&self.events_path(&name)).into_iter().filter(|e| e.t >= from && e.t < to));
            day += 1;
        }
        ms.sort_by_key(|m| m.t);
        es.sort_by_key(|e| e.t);
        (ms, es)
    }

    /// Delete day files older than [`KEEP_DAYS`] before `now`'s day.
    pub fn prune(&self, now: u64) {
        let keep_from = day_name(now.saturating_sub(KEEP_DAYS * 86_400));
        let Ok(rd) = fs::read_dir(&self.dir) else { return };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let day = name.split('.').next().unwrap_or("");
            if day.len() == 8 && day.bytes().all(|b| b.is_ascii_digit()) && day < keep_from.as_str() {
                let _ = fs::remove_file(e.path());
            }
        }
    }
}

// ---- recorder + store, as the netwatch thread drives them --------------------

pub struct Log {
    pub rec: Recorder,
    store: Store,
    /// Minutes in `rec.minutes` not yet on disk (counted from the back).
    unsaved: usize,
    last_flush: u64,
    /// Disk read back and pruned (done once, after the clock is set).
    loaded: bool,
    pub write_errors: u64,
}

impl Log {
    pub fn new(dir: impl Into<PathBuf>) -> Log {
        Log { rec: Recorder::default(), store: Store::new(dir), unsaved: 0, last_flush: 0, loaded: false, write_errors: 0 }
    }

    fn io(&mut self, r: io::Result<()>) {
        if let Err(e) = r {
            self.write_errors += 1;
            if self.write_errors == 1 || self.write_errors.is_multiple_of(100) {
                eprintln!("netwatch: signal log write failed ({e}), {} so far", self.write_errors);
            }
        }
    }

    pub fn feed(&mut self, r: &Reading) {
        let set = r.at >= CLOCK_SET;
        if set && !self.loaded {
            self.loaded = true;
            self.rec.drop_unset_clock();
            self.unsaved = self.unsaved.min(self.rec.minutes.len());
            self.store.prune(r.at);
            let (ms, es) = self.store.read_range(r.at.saturating_sub(86_400), r.at + 1);
            let had: Vec<Minute> = self.rec.minutes.drain(..).collect();
            let evs: Vec<Event> = self.rec.events.drain(..).collect();
            self.rec.minutes.extend(ms.into_iter().chain(had));
            self.rec.events.extend(es.into_iter().chain(evs));
            while self.rec.minutes.len() > KEEP_MINUTES {
                self.rec.minutes.pop_front();
            }
            while self.rec.events.len() > KEEP_EVENTS {
                self.rec.events.pop_front();
            }
            self.last_flush = r.at;
        }
        let (events, closed) = self.rec.feed(r);
        if set {
            for e in events.iter().filter(|e| e.t >= CLOCK_SET) {
                let res = self.store.append_event(e);
                self.io(res);
            }
        }
        if closed.is_some() {
            self.unsaved = (self.unsaved + 1).min(self.rec.minutes.len());
        }
        if set && self.unsaved > 0 && r.at.saturating_sub(self.last_flush) >= FLUSH_EVERY {
            self.flush(r.at);
        }
    }

    fn flush(&mut self, now: u64) {
        let n = self.rec.minutes.len();
        let pending: Vec<Minute> = self.rec.minutes.iter().skip(n - self.unsaved).filter(|m| m.t >= CLOCK_SET).cloned().collect();
        let res = self.store.append_minutes(&pending);
        self.io(res);
        self.unsaved = 0;
        self.last_flush = now;
        if day_name(now) != day_name(now.saturating_sub(FLUSH_EVERY)) {
            self.store.prune(now);
        }
    }

    /// An event from another module: kept and written at once.
    pub fn push_event(&mut self, e: Event) {
        if e.t >= CLOCK_SET {
            let res = self.store.append_event(&e);
            self.io(res);
        }
        self.rec.push_event(e);
    }

    /// Minutes and events with `from <= t < to`: from memory when it covers
    /// the range, from disk otherwise (up to 7 days).
    pub fn range(&self, from: u64, to: u64) -> (Vec<Minute>, Vec<Event>) {
        let mem_from = self.rec.minutes.front().map(|m| m.t).unwrap_or(u64::MAX);
        if from >= mem_from || !self.loaded {
            let ms = self.rec.minutes.iter().filter(|m| m.t >= from && m.t < to).cloned().collect();
            let es = self.rec.events.iter().filter(|e| e.t >= from && e.t < to).cloned().collect();
            return (ms, es);
        }
        // disk has what was flushed; memory has the rest
        let (mut ms, mut es) = self.store.read_range(from, to);
        let disk_last_m = ms.last().map(|m| m.t);
        ms.extend(self.rec.minutes.iter().filter(|m| m.t >= from && m.t < to && disk_last_m.is_none_or(|l| m.t > l)).cloned());
        let disk_last_e = es.last().map(|e| e.t);
        es.extend(self.rec.events.iter().filter(|e| e.t >= from && e.t < to && disk_last_e.is_none_or(|l| e.t > l)).cloned());
        (ms, es)
    }
}

#[cfg(test)]
mod tests;
