//! The event log's long-term layer (u60-platform.md D9, u60-sentinel.md §8 D16; T8): what is kept
//! after the raw events' 24 hours, so the 7-day scorecard and "what happened last week" have data.
//!
//! - **Hour summaries**, kept 90 days: one line per hour (on the wall clock's hour once it can be
//!   trusted, else every hour of uptime, as the guard's ledger does), `/data/luyaz/core/long/hours.jsonl`.
//! - **Important events**, kept 30 days and at most [`EVENTS_CAP`] bytes: boot, our version, the
//!   cellular link going down/up, the clock becoming trusted, blocks going stale and back,
//!   `/data/luyaz/core/long/events.jsonl`.
//!
//! Sans-IO: [`Fold`] takes the published block events (the same ones the raw log stores) and
//! clock readings and gives the lines; [`Store`] appends and prunes. So the layer can be rebuilt
//! from raw events by running the same fold over a replay ([`rebuild`]); live and rebuilt agree
//! by construction (test). Field names follow the guard's ledger (touch-ui docs/LEDGER.md §4
//! `hour`, `hour_power`) where the meaning is the same, so the parallel week (5e) can compare them;
//! where u60d sees less (no energy counter, home MCC without MNC), the field says so by its name.
//!
//! Flash: one line an hour (~300 bytes) plus a few events a day, each appended and fsynced: well
//! under 0.1 MB a day. Off unless `LUYAZ_LONGTERM=1` (the device turns it on in datad.env).
//!
//! **Coverage** (u60-sentinel.md §19 「T9c 前定」): each hour line carries `g_<src>`, the awake
//! seconds that source was not working, in CLOCK_MONOTONIC (so sleep is neither covered nor a
//! gap). `core` is this process: a restart in the same boot resumes the fold from
//! [`STATE`] (tmpfs) and charges the seconds it was away to every source; a restart in another
//! hour closes the old hour where it was last saved and writes one spanning `gap src=core
//! no_hour=1`, which the scorecard's adapter turns into an hour line that was not covered at all.
//! `watcher`, `capture`, `link` are the crash watcher's three routes (kmsg, the dump directory's
//! inotify, rtnetlink): a probe every [`PROBE_S`] wakes it ([`Shared`]); no wake since the
//! previous probe = all three down since its last wake; lost records (EPIPE, ENOBUFS,
//! IN_Q_OVERFLOW) = that route down from its last clean read. `crashlog`, `uidlog` come from the
//! minute look ([`watch`]): a check whose look did not say they worked charges them since the
//! previous check (a look that never runs reads as down, never as a missing field that reads 0).
//! `datad` is this process's own data (the agent's marker rules, §11), so it is down only when
//! the core is.
//! A span found after an hour closed is charged to the hour it is found in (at most one probe
//! period late); every `g_*` is capped at the hour's awake seconds. A run of one source's down
//! time longer than 10 min (across hours and restarts) also goes out as a `gap` event.

pub mod activity;
pub mod boot;
pub mod energy;
pub mod places;
pub mod power;
pub mod standby;
pub mod watch;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const DIR: &str = "/data/luyaz/core/long";
pub const HOURS_KEEP_S: i64 = 90 * 86_400;
pub const EVENTS_KEEP_S: i64 = 30 * 86_400;
pub const EVENTS_CAP: u64 = 1 << 20;
/// Blocks the fold looks at; everything else passes by.
pub const BLOCKS: &[&str] = &["signal", "battery", "interfaces", "sim", "clients"];
/// The fold between two processes of the same boot (tmpfs).
pub const STATE: &str = "/tmp/luyaz/core/longterm.json";
/// The fold's copy on flash, rewritten at every check (the guard's `state/acc.last`): after a
/// reboot, the last boot's open hour is closed where it was last saved instead of lost with tmpfs.
pub const OPEN: &str = "open.json";
/// Sources the core watches: itself, the crash watcher's three routes, the minute look's two,
/// and datad (this process).
pub const WATCHED: &[&str] = &[
    "core", "watcher", "capture", "link", "crashlog", "uidlog", "datad",
];
/// The crash watcher's routes (all down when its thread does not answer the probe).
const CW: &[&str] = &["watcher", "capture", "link"];
/// Sources the minute look reports (absent from a check's report = down since the previous one).
const LOOKED: &[&str] = &["crashlog", "uidlog"];
/// How often the crash watcher is probed and coverage is charged (and the fold saved).
pub const PROBE_S: i64 = 60;
/// A down run longer than this also goes out as a `gap` event (§8: "有一段超过 10 分钟").
const LONG_S: i64 = 600;

/// One clock reading: boot id, CLOCK_BOOTTIME and CLOCK_MONOTONIC in s, wall clock, trusted.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Clock {
    pub boot: String,
    pub boottime: i64,
    pub mono: i64,
    pub wall: i64,
    pub wall_ok: bool,
    /// Fresh local-wall estimate, not proof of UTC synchronization.
    #[serde(default)]
    pub clock_status: luyaz_api::clock::EvidenceStatus,
    /// Keep raw observations and the explicit unknown clock model for audit/replay.
    #[serde(default)]
    pub evidence: Option<luyaz_api::clock::ClockEvidence>,
    /// How clock-ok decided, when it says.
    #[serde(default)]
    pub how: Option<String>,
}

/// What the fold took from the blocks.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Seen {
    /// Serving MCC-MNC as the guard writes it (3-digit MNC where the country uses them).
    net: Option<String>,
    /// The radio-mode preference (signal's `net_select`).
    #[serde(default)]
    net_select: Option<String>,
    /// The SIM's home MCC-MNC (the IMSI's prefix; the IMSI itself is never kept).
    #[serde(default)]
    home: Option<String>,
    home_mcc: Option<i64>,
    serving_mcc: Option<i64>,
    battery: Option<i64>,
    batt_temp: Option<i64>,
    charging: bool,
    link: Option<String>,
    /// Clients connected (the `clients` block's total; None while stale or not seen).
    #[serde(default)]
    clients: Option<i64>,
    /// The serving band (NR when there is one), for the experience library.
    #[serde(default)]
    band: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Hour {
    from: Clock,
    batt_from: Option<i64>,
    batt_tmax: Option<i64>,
    abroad_s: i64,
    chg_s: i64,
    link_down: u32,
    stale_flips: u32,
    /// Awake seconds each watched source was down this hour.
    #[serde(default)]
    g: BTreeMap<String, i64>,
    /// `hour_power` / `hour_proc` sums.
    #[serde(default)]
    pw: power::Acc,
}

/// What the IO side saw of the sources since the last check (all CLOCK_MONOTONIC seconds).
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// The crash watcher's last wake; None if it never woke (off, or its thread is gone).
    pub alive: Option<i64>,
    /// Closed spans a route lost records in: (src, from, to).
    pub spans: Vec<(String, i64, i64)>,
    /// Routes down now (not set up, subscription gone): (src, since).
    pub down: Vec<(String, i64)>,
    /// The minute look's sources and whether they worked at this check.
    pub looks: Vec<(String, bool)>,
}

#[derive(Default, Serialize, Deserialize)]
pub struct Fold {
    boot: String,
    seen: Seen,
    hour: Option<Hour>,
    /// The last clock reading, to count seconds between readings.
    last: Option<Clock>,
    stale: BTreeMap<String, bool>,
    clock_said: bool,
    /// This boot's `boot` line is written (its facts come [`boot::AFTER_S`] in). A state saved
    /// before the field existed had written it at the first reading.
    #[serde(default = "yes")]
    boot_said: bool,
    /// The facts and the backfill went out (with a `boot` line of their own when the late one
    /// without facts was written first: readers keep a boot's last `boot` line).
    #[serde(default = "yes")]
    facts_said: bool,
    /// The minute look's state for this boot.
    #[serde(default)]
    look: watch::State,
    /// The power readings' state for this boot, and the last reading: (boot s, mono s, counter).
    #[serde(default)]
    pw_read: power::ReadState,
    #[serde(default)]
    pw_last: Option<(i64, i64, Option<i64>)>,
    /// MONOTONIC s of this boot's first reading by any of our processes (the late mark's start).
    #[serde(default)]
    first_mono: i64,
    /// The offset (wall − boot seconds) the last trusted `clock` event said.
    #[serde(default)]
    clock_off: Option<i64>,
    #[serde(skip)]
    version: String,
    /// The version the last `ver` event said (a restart says it again only when it changed).
    #[serde(default)]
    ver_said: String,
    /// Per source: down seconds are charged up to here (MONOTONIC s), never twice.
    #[serde(default)]
    counted: BTreeMap<String, i64>,
    /// Per source: the current down run (from, to; MONOTONIC s), for the 10-minute gap events.
    #[serde(default)]
    runs: BTreeMap<String, (i64, i64)>,
    /// When the crash watcher was last probed (MONOTONIC s).
    #[serde(default)]
    probe_at: Option<i64>,
    /// Set by [`Fold::resume`]: the next reading accounts for the time this process was away.
    #[serde(skip)]
    resumed: bool,
    /// `<net_select> <serving MCC>` at the last `net_change` (kept across boots by the IO side,
    /// `net.last`, as the guard's `state/net.last`).
    #[serde(default)]
    pub net_last: Option<String>,
}

/// What the fold gives: lines for the two files.
#[derive(Debug, Default, PartialEq)]
pub struct Out {
    pub hours: Vec<Value>,
    pub events: Vec<Value>,
}

fn yes() -> bool {
    true
}

/// After this many seconds from the boot's first reading without the facts, the `boot` line goes
/// out without them.
const BOOT_LATE_S: i64 = 600;

fn boot8(b: &str) -> String {
    b.chars()
        .filter(|c| c.is_ascii_hexdigit())
        .take(8)
        .collect()
}

fn wall_or_null(c: &Clock) -> Value {
    if c.wall_ok {
        json!(c.wall)
    } else {
        Value::Null
    }
}

/// The hour a reading belongs to: the wall clock's hour once trusted, else whole hours of uptime.
fn hour_key(c: &Clock) -> (bool, i64) {
    if c.wall_ok {
        (true, c.wall.div_euclid(3600))
    } else {
        (false, c.boottime.div_euclid(3600))
    }
}

impl Fold {
    pub fn new(version: &str) -> Fold {
        Fold {
            version: version.to_string(),
            ..Default::default()
        }
    }

    /// The fold a previous process of this boot saved; None when it is another boot's or
    /// unreadable (then a fresh fold: a new boot, or the state was lost).
    pub fn resume(json: &str, version: &str, boot: &str) -> Option<Fold> {
        let mut f: Fold = serde_json::from_str(json).ok()?;
        if f.boot != boot || f.last.is_none() {
            return None;
        }
        f.version = version.to_string();
        f.resumed = true;
        Some(f)
    }

    /// Another boot's open hour from its [`OPEN`] copy, closed at its last check (its lines go
    /// under that boot; empty when the copy is this boot's, has no hour, or is unreadable).
    pub fn close_old(json: &str, boot: &str) -> Vec<Value> {
        let Ok(mut f) = serde_json::from_str::<Fold>(json) else {
            return Vec::new();
        };
        if f.boot == boot || f.hour.is_none() {
            return Vec::new();
        }
        match f.last.clone() {
            Some(last) => f.close(&last),
            None => Vec::new(),
        }
    }

    /// The fold as saved for the next process ([`STATE`]).
    pub fn save(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Whether a check is due (the crash watcher probed, coverage charged, the fold saved).
    pub fn check_due(&self, c: &Clock) -> bool {
        self.probe_at.is_none_or(|p| c.mono - p >= PROBE_S)
    }

    /// Down seconds `[a, b]` of `src` into this hour (only what was not counted before), and its
    /// down run.
    fn charge(&mut self, src: &str, a: i64, b: i64) {
        let from = a.max(self.counted.get(src).copied().unwrap_or(i64::MIN));
        if b <= from {
            return;
        }
        if let Some(h) = self.hour.as_mut() {
            *h.g.entry(src.to_string()).or_default() += b - from;
        }
        self.counted.insert(src.to_string(), b);
        match self.runs.get_mut(src) {
            Some(r) if from <= r.1 + 1 => r.1 = b,
            _ => {
                self.runs.insert(src.to_string(), (from, b));
            }
        }
    }

    /// Ends `src`'s down run; a long one becomes a `gap` event (boot seconds from the clock's
    /// sleep offset now).
    fn end_run(&mut self, src: &str, c: &Clock, out: &mut Out) {
        let Some((a, b)) = self.runs.remove(src) else {
            return;
        };
        if b - a > LONG_S {
            let off = c.boottime - c.mono;
            out.events.push(self.event(
                c,
                "gap",
                json!({"src": src, "from": a + off, "to": b + off, "awake": b - a}),
            ));
        }
    }

    /// The minute look ([`watch`]): its lines into `out`; returns the file steps to do once they
    /// are written and the look's sources for [`Report::looks`].
    pub fn look(&mut self, c: &Clock, p: &watch::Paths, out: &mut Out) -> watch::Look {
        if self.boot != c.boot {
            self.tick(c, out);
        }
        let mut lk = self.look.look(p, &c.boot, c.boottime, c.wall);
        for (k, f) in std::mem::take(&mut lk.lines) {
            out.events.push(self.event(c, k, f));
        }
        lk
    }

    /// This check's power and process readings into the hour (`hour_power`, `hour_proc`).
    pub fn power(&mut self, c: &Clock, p: &power::Paths, out: &mut Out) {
        if self.boot != c.boot {
            self.tick(c, out);
        }
        let mut r = self.pw_read.read(p, &self.look.pids(), c.wall);
        r.clients = self.seen.clients;
        r.wan = Some(crate::cell_window::take_quiet());
        self.power_with(c, &r);
    }

    /// [`Fold::power`] with the readings given.
    pub fn power_with(&mut self, c: &Clock, r: &power::Read) {
        let abroad = matches!((self.seen.serving_mcc, self.seen.home_mcc), (Some(s), Some(h)) if s > 0 && h > 0 && s != h);
        if let Some(h) = self.hour.as_mut() {
            let known_abroad = match (self.seen.serving_mcc, self.seen.home_mcc) {
                (Some(s), Some(h)) if s > 0 && h > 0 => Some(s != h),
                _ => None,
            };
            h.pw.energy.observe(r, c, known_abroad);
            h.pw.activity
                .observe(r.activity.as_ref(), &c.boot, c.boottime, c.mono);
            match self.pw_last {
                Some((b0, m0, q0)) => {
                    h.pw.add(r, q0, c.boottime - b0, c.mono - m0, abroad, c.boottime)
                }
                None => h.pw.note(r, c.boottime),
            }
        }
        self.pw_last = Some((c.boottime, c.mono, r.q));
    }

    /// What the experience library needs of what the fold has seen.
    pub fn places_now(&self) -> places::Now {
        places::Now {
            net: self.seen.net.clone(),
            net_select: self.seen.net_select.clone(),
            band: self.seen.band.clone(),
        }
    }

    /// The periodic check: what the IO side saw of the sources since the last one. Call when
    /// [`Fold::check_due`]; probe the crash watcher right after.
    pub fn cover(&mut self, c: &Clock, rep: &Report, out: &mut Out) {
        self.tick(c, out);
        let now = c.mono;
        if let Some(p) = self.probe_at
            && rep.alive.is_none_or(|a| a < p)
        {
            // no wake since the last probe: the watcher's thread is stuck or gone
            let from = rep.alive.unwrap_or(i64::MIN);
            for s in CW {
                self.charge(s, from, now);
            }
        }
        for (s, a, b) in &rep.spans {
            self.charge(s, *a, (*b).min(now));
        }
        for (s, since) in &rep.down {
            self.charge(s, *since, now);
        }
        if let Some(p) = self.probe_at {
            for s in LOOKED {
                if !rep.looks.iter().any(|(x, ok)| x == s && *ok) {
                    self.charge(s, p, now);
                }
            }
        }
        let ended: Vec<String> = self
            .runs
            .iter()
            .filter(|(_, r)| r.1 < now)
            .map(|(s, _)| s.clone())
            .collect();
        for s in ended {
            self.end_run(&s, c, out);
        }
        self.probe_at = Some(now);
    }

    fn event(&self, c: &Clock, k: &str, mut fields: Value) -> Value {
        let mut v = json!({"t": wall_or_null(c), "raw_wall": c.wall, "clock_status": c.clock_status,
            "up": c.boottime, "boot": boot8(&c.boot), "k": k});
        if let (Some(o), Some(f)) = (v.as_object_mut(), fields.as_object_mut()) {
            o.append(f);
        }
        v
    }

    /// A block was published (`name`, its data, stale or not).
    pub fn block(&mut self, c: &Clock, name: &str, stale: bool, data: &Value, out: &mut Out) {
        self.tick(c, out);
        if name == "clients" {
            // only for hour_power's clients seconds: no stale events, no flips
            self.seen.clients = if stale { None } else { data["total"].as_i64() };
            return;
        }
        if self.stale.get(name).copied().unwrap_or(false) != stale && self.stale.contains_key(name)
        {
            out.events
                .push(self.event(c, "stale", json!({"block": name, "stale": stale})));
            if let Some(h) = self.hour.as_mut() {
                h.stale_flips += 1;
            }
        }
        self.stale.insert(name.to_string(), stale);
        if stale {
            return;
        }
        match name {
            "signal" => {
                let s = &mut self.seen;
                let n = crate::crashwatch::Net::from_blocks(Some(data), None, None);
                s.net = n.net;
                s.band = n.nrband.or(n.band);
                s.net_select = data["net_select"]
                    .as_str()
                    .filter(|x| !x.is_empty())
                    .map(|x| x.chars().take(24).collect());
                s.home_mcc = data["home_mcc"].as_i64();
                s.serving_mcc = data["serving_mcc"].as_i64();
                // net_change (LEDGER.md §4): the preference or the serving country moved
                if let (Some(ns), Some(net)) = (s.net_select.clone(), s.net.clone()) {
                    let key = format!("{ns} {}", net.split('-').next().unwrap_or(""));
                    if self.net_last.as_deref() != Some(key.as_str()) {
                        let ev = self.event(
                            c,
                            "net_change",
                            json!({"net_select": ns, "net": net, "home": self.seen.home}),
                        );
                        out.events.push(ev);
                        self.net_last = Some(key);
                    }
                }
            }
            "sim" => {
                self.seen.home = crate::crashwatch::Net::from_blocks(None, Some(data), None).home;
            }
            "battery" => {
                let s = &mut self.seen;
                s.battery = data["percent"].as_i64();
                s.batt_temp = data["temp"].as_i64();
                s.charging = data["charging"].as_i64().unwrap_or(0) != 0;
                if let Some(h) = self.hour.as_mut() {
                    if h.batt_from.is_none() {
                        h.batt_from = s.battery;
                    }
                    if let Some(t) = s.batt_temp {
                        h.batt_tmax = Some(h.batt_tmax.map_or(t, |m| m.max(t)));
                    }
                }
            }
            "interfaces" => {
                let st = data
                    .pointer("/cellular/connect_status")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let up = |s: &Option<String>| {
                    s.as_deref()
                        .is_some_and(|x| x.contains("connected") && !x.contains("disconnect"))
                };
                if self.seen.link.is_some() && up(&self.seen.link) != up(&st) {
                    out.events.push(self.event(
                        c,
                        "link",
                        json!({"state": if up(&st) { "up" } else { "down" }, "status": st}),
                    ));
                    if !up(&st)
                        && let Some(h) = self.hour.as_mut()
                    {
                        h.link_down += 1;
                    }
                }
                self.seen.link = st;
            }
            _ => {}
        }
    }

    /// A clock reading with nothing published (each round's heartbeat): closes the hour when it
    /// turned, counts seconds.
    pub fn tick(&mut self, c: &Clock, out: &mut Out) {
        if self.boot != c.boot {
            // a new boot (or the first reading): the previous hour, if any, is lost with the
            // process — what the guard's ledger also does for an hour cut by a reboot
            self.boot = c.boot.clone();
            self.hour = None;
            self.last = None;
            self.clock_said = false;
            self.runs.clear();
            self.probe_at = None;
            self.counted = WATCHED.iter().map(|s| (s.to_string(), c.mono)).collect();
            self.boot_said = false;
            self.facts_said = false;
            self.first_mono = c.mono;
            self.look = watch::State::default();
            self.pw_read = power::ReadState::default();
            self.pw_last = None;
            self.ver_said = self.version.clone();
            out.events
                .push(self.event(c, "ver", json!({"prog": "u60d", "version": self.version})));
        }
        if std::mem::take(&mut self.resumed)
            && let Some(last) = self.last.take()
        {
            self.resume_at(&last, c, out);
        }
        // the late mark counts from this boot's first reading by any of our processes, so a
        // core that starts long into a boot still waits for the facts
        if !self.boot_said && c.mono - self.first_mono >= BOOT_LATE_S {
            self.boot_said = true;
            out.events
                .push(self.event(c, "boot", json!({"boot_id": c.boot})));
        }
        // Clock estimate: first usable reading, revocation/reacquisition, or an offset change
        // beyond rounding/slew tolerance. None of these events asserts verified UTC.
        let off = c.wall - c.boottime;
        if c.wall_ok {
            if !self.clock_said
                || self
                    .clock_off
                    .is_none_or(|o| off.abs_diff(o) > luyaz_api::clock::WALL_STEP_TOLERANCE_S)
            {
                self.clock_said = true;
                self.clock_off = Some(off);
                out.events.push(self.event(
                    c,
                    "clock",
                    json!({"offset": off, "how": c.how, "evidence": c.evidence}),
                ));
            }
        } else if self.clock_said {
            self.clock_said = false;
            self.clock_off = None;
            out.events.push(self.event(
                c,
                "clock",
                json!({"offset": null, "how": "revoked", "evidence": c.evidence}),
            ));
        }
        if let (Some(last), Some(h)) = (self.last.clone(), self.hour.as_mut()) {
            let dt = (c.mono - last.mono).max(0);
            if self.seen.charging {
                h.chg_s += dt;
            }
            if let (Some(s), Some(hm)) = (self.seen.serving_mcc, self.seen.home_mcc)
                && s > 0
                && hm > 0
                && s != hm
            {
                h.abroad_s += dt;
            }
        }
        match &self.hour {
            Some(h) if hour_key(&h.from) != hour_key(c) => {
                // the clock became trusted mid-hour: the boot-relative hour ends here too
                let lines = self.close(c);
                out.hours.extend(lines);
                self.open(c);
            }
            None => self.open(c),
            _ => {}
        }
        self.last = Some(c.clone());
    }

    /// This boot's `boot` line with its facts ([`boot::gather`]) and the key.log backfill lines
    /// before it. Only once a boot (again after a late `boot` line without facts); returns
    /// whether it wrote them (then the anchor may move).
    pub fn boot_facts(
        &mut self,
        c: &Clock,
        facts: &Value,
        backfill: &[Value],
        out: &mut Out,
    ) -> bool {
        if self.boot != c.boot {
            self.tick(c, out);
        }
        if self.facts_said {
            return false;
        }
        self.boot_said = true;
        self.facts_said = true;
        for l in backfill {
            out.events.push(self.event(c, "boot_backfill", l.clone()));
        }
        let mut f = json!({"boot_id": c.boot});
        if let (Some(o), Some(x)) = (f.as_object_mut(), facts.as_object()) {
            for (k, v) in x {
                o.insert(k.clone(), v.clone());
            }
        }
        out.events.push(self.event(c, "boot", f));
        true
    }

    /// The first reading of a process that resumed a fold saved at `last`: the time between
    /// was not covered by anything (§19 rules 8, 9).
    fn resume_at(&mut self, last: &Clock, c: &Clock, out: &mut Out) {
        if self.ver_said != self.version {
            self.ver_said = self.version.clone();
            out.events
                .push(self.event(c, "ver", json!({"prog": "u60d", "version": self.version})));
        }
        let same_hour = self
            .hour
            .as_ref()
            .is_some_and(|h| hour_key(&h.from) == hour_key(c));
        if same_hour {
            for s in WATCHED {
                self.charge(s, last.mono, c.mono);
            }
            return;
        }
        // another hour: close the old one where it was saved, one spanning gap for the rest
        if self.hour.is_some() {
            let lines = self.close(last);
            out.hours.extend(lines);
        }
        if c.mono > last.mono {
            out.events.push(self.event(
                c,
                "gap",
                json!({
                    "src": "core", "no_hour": 1, "from": last.boottime, "to": c.boottime,
                    "ft": wall_or_null(last), "tt": wall_or_null(c),
                    "awake": c.mono - last.mono,
                }),
            ));
        }
        for s in WATCHED {
            self.counted.insert(s.to_string(), c.mono);
        }
        // its runs do not carry over the spanning gap (that is reported by itself)
        let runs: Vec<String> = self.runs.keys().cloned().collect();
        for s in runs {
            self.end_run(&s, last, out);
        }
    }

    fn open(&mut self, c: &Clock) {
        self.hour = Some(Hour {
            from: c.clone(),
            batt_from: self.seen.battery,
            batt_tmax: self.seen.batt_temp,
            ..Default::default()
        });
    }

    /// The hour's three lines: `hour`, `hour_power`, `hour_proc`.
    fn close(&mut self, c: &Clock) -> Vec<Value> {
        let h = self.hour.take().unwrap_or_default();
        let span = c.boottime - h.from.boottime;
        let awake = (c.mono - h.from.mono).clamp(0, span.max(0));
        let mut line = json!({
            "k": "hour",
            "id": format!("h-{}-{}", boot8(&c.boot), h.from.boottime),
            "boot": boot8(&c.boot),
            "from": h.from.boottime,
            "to": c.boottime,
            "ft": wall_or_null(&h.from),
            "tt": wall_or_null(c),
            "awake": awake,
            "asleep": span.max(0) - awake,
            "net": self.seen.net,
            "home_mcc": self.seen.home_mcc,
            "abroad": h.abroad_s,
            "batt_from": h.batt_from,
            "batt_to": self.seen.battery,
            "batt_tmax": h.batt_tmax,
            "chg_s": h.chg_s,
            "link_down": h.link_down,
            "stale_flips": h.stale_flips,
        });
        let (free, dump) = h.pw.free_dump();
        if let Some(o) = line.as_object_mut() {
            for s in WATCHED {
                let g = h.g.get(*s).copied().unwrap_or(0).clamp(0, awake);
                o.insert(format!("g_{s}"), json!(g));
            }
            // the core has no daily write budget of the guard's kind: null, not 0
            o.insert("free_mb".into(), json!(free));
            o.insert("dump_mb".into(), json!(dump));
            o.insert("budget".into(), Value::Null);
        }
        let head = |k: &str, pre: &str| {
            json!({
                "k": k, "id": format!("{pre}-{}-{}", boot8(&c.boot), h.from.boottime),
                "boot": boot8(&c.boot), "from": h.from.boottime, "to": c.boottime,
                "ft": wall_or_null(&h.from), "tt": wall_or_null(c),
            })
        };
        let mut out = vec![line];
        for (k, pre, body) in [
            ("hour_power", "hp", h.pw.power()),
            ("hour_proc", "hq", h.pw.proc_()),
        ] {
            let mut v = head(k, pre);
            if let (Some(o), Some(b)) = (v.as_object_mut(), body.as_object()) {
                for (x, y) in b {
                    o.insert(x.clone(), y.clone());
                }
            }
            out.push(v);
        }
        out
    }
}

/// The same fold over a replay of raw events (each: clock, block name, stale, data; `None` for
/// a heartbeat): what the live layer would have written.
pub type RawEvent = (Clock, Option<(String, bool, Value)>);

/// (Used by tests now; the 5e parallel week rebuilds the layer from a raw-event replay with it.)
#[allow(dead_code)]
pub fn rebuild(version: &str, raw: &[RawEvent]) -> Out {
    let mut f = Fold::new(version);
    let mut out = Out::default();
    for (c, b) in raw {
        match b {
            Some((name, stale, data)) => f.block(c, name, *stale, data, &mut out),
            None => f.tick(c, &mut out),
        }
    }
    out
}

/// The two files.
pub struct Store {
    dir: PathBuf,
}

/// The layer's directory is 0700 like the raw log's (evlog.rs); changed only when it is not
/// (a chmod on every append would write the inode each time).
fn private_dir(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(m) = std::fs::metadata(dir)
        && m.permissions().mode() & 0o777 != 0o700
    {
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
}

/// One writer at a time for the two files: the crash watcher (`crashwatch`, T10) appends to
/// `events.jsonl` from its own thread, and a prune in between would rename its line away.
static FILES: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl Store {
    pub fn at(dir: &Path) -> Store {
        Store {
            dir: dir.to_path_buf(),
        }
    }

    fn append(&self, name: &str, lines: &[Value]) -> std::io::Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        std::fs::create_dir_all(&self.dir)?;
        private_dir(&self.dir);
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(name))?;
        let mut buf = String::new();
        for l in lines {
            buf.push_str(&l.to_string());
            buf.push('\n');
        }
        f.write_all(buf.as_bytes())?;
        f.sync_data()
    }

    /// The last `net_change`'s key, kept across boots.
    pub fn net_last(&self) -> Option<String> {
        std::fs::read_to_string(self.dir.join("net.last"))
            .ok()
            .map(|t| t.trim_end_matches('\n').to_string())
            .filter(|t| !t.is_empty())
    }

    pub fn set_net_last(&self, key: &str) -> std::io::Result<()> {
        let p = self.dir.join("net.last");
        let tmp = p.with_extension("tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(format!("{key}\n").as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &p)
    }

    /// Lines from another writer (the crash watcher) into `events.jsonl`, fsynced.
    pub fn append_events(&self, lines: &[Value]) -> std::io::Result<()> {
        let _g = FILES.lock().unwrap_or_else(|e| e.into_inner());
        self.append("events.jsonl", lines)
    }

    pub fn write(&self, out: &Out) -> std::io::Result<()> {
        let _g = FILES.lock().unwrap_or_else(|e| e.into_inner());
        self.append("hours.jsonl", &out.hours)?;
        self.append("events.jsonl", &out.events)
    }

    /// Drop lines older than the keep window (by `tt`/`t`; lines without a trusted time stay
    /// until the byte cap), and the events file's oldest lines past [`EVENTS_CAP`]. Only rewrites
    /// a file that changes (tmp + rename). Needs a trusted `now`; else nothing.
    pub fn prune(&self, now: Option<i64>) -> std::io::Result<()> {
        let Some(now) = now else { return Ok(()) };
        let _g = FILES.lock().unwrap_or_else(|e| e.into_inner());
        for (name, keep, key, cap) in [
            ("hours.jsonl", HOURS_KEEP_S, "tt", u64::MAX),
            ("events.jsonl", EVENTS_KEEP_S, "t", EVENTS_CAP),
        ] {
            let p = self.dir.join(name);
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            let mut lines: Vec<&str> = text
                .lines()
                .filter(|l| {
                    serde_json::from_str::<Value>(l)
                        .ok()
                        .and_then(|v| v[key].as_i64())
                        .is_none_or(|t| t >= now - keep)
                })
                .collect();
            let mut size: u64 = lines.iter().map(|l| l.len() as u64 + 1).sum();
            while size > cap && !lines.is_empty() {
                size -= lines[0].len() as u64 + 1;
                lines.remove(0);
            }
            if lines.len() == text.lines().count() {
                continue;
            }
            let tmp = p.with_extension("tmp");
            let mut body = lines.join("\n");
            if !body.is_empty() {
                body.push('\n');
            }
            {
                let mut f = std::fs::File::create(&tmp)?;
                f.write_all(body.as_bytes())?;
                f.sync_all()?;
            }
            std::fs::rename(&tmp, &p)?;
        }
        Ok(())
    }
}

// ── wiring: a tee on the hub's event sink and a thread that folds and writes ──

/// What the hub hands over: a published block of interest, or a round's heartbeat (a tick).
pub enum Msg {
    Block(String, bool, Value),
    Tick,
    /// The boot line's facts and the key.log backfill ([`boot::gather`]).
    Boot(Value, boot::Backfill),
}

static TX: std::sync::OnceLock<std::sync::Mutex<std::sync::mpsc::Sender<Msg>>> =
    std::sync::OnceLock::new();

/// To the long-term thread from elsewhere (no-op when the layer is off).
pub fn send(m: Msg) {
    if let Some(tx) = TX.get() {
        let _ = tx.lock().unwrap_or_else(|e| e.into_inner()).send(m);
    }
}

/// The boot facts, once this process's boot is [`boot::AFTER_S`] old (needs the executor).
pub async fn boot_task() {
    let up = crate::evlog::boottime_ms() / 1000;
    if up < boot::AFTER_S {
        tokio::time::sleep(std::time::Duration::from_secs(boot::AFTER_S - up)).await;
    }
    let dir = PathBuf::from(crate::state::host_path(DIR));
    let (facts, bf) = boot::gather(&dir).await;
    send(Msg::Boot(facts, bf));
}

/// The hub's sink plus the long-term layer: everything goes on to `inner` as before; the blocks
/// in [`BLOCKS`] and the heartbeats are also queued for the long-term thread (an unbounded
/// channel: `emit` runs in the hub's lock and must not wait).
pub struct Tee {
    pub inner: Box<dyn crate::block::EventSink>,
    pub tx: std::sync::Mutex<std::sync::mpsc::Sender<Msg>>,
}

impl crate::block::EventSink for Tee {
    fn emit(&self, event: &crate::block::Event) {
        self.inner.emit(event);
        let m = match event {
            crate::block::Event::Block(b) if BLOCKS.contains(&b.name) => {
                Msg::Block(b.name.to_string(), b.stale, b.data.clone())
            }
            crate::block::Event::Heartbeat(_) => Msg::Tick,
            _ => return,
        };
        let _ = self.tx.lock().unwrap_or_else(|e| e.into_inner()).send(m);
    }
}

pub fn enabled() -> bool {
    std::env::var("LUYAZ_LONGTERM").is_ok_and(|v| v == "1")
}

fn mono_s() -> i64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime into a timespec we own.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec
}

/// Snapshot without probing the network or changing system time. Expired or stepped clock
/// evidence cannot label ledger records with a usable wall timestamp.
pub(crate) fn clock_now(boot: &str) -> Clock {
    let at = luyaz_api::clock::BootTime::read();
    let wall = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let ck = std::fs::read_to_string(crate::state::host_path(luyaz_api::clock::CLOCK_OK))
        .unwrap_or_default();
    clock_snapshot(boot, at, mono_s(), wall, &ck)
}

fn clock_snapshot(
    boot: &str,
    at: Option<luyaz_api::clock::BootTime>,
    mono: i64,
    wall: i64,
    text: &str,
) -> Clock {
    use luyaz_api::clock::{ClockEvidence, EvidenceStatus};
    let evidence = ClockEvidence::decode(text);
    let clock_status = if at.as_ref().is_some_and(|t| t.boot_id != boot) {
        EvidenceStatus::BootMismatch
    } else {
        evidence
            .as_ref()
            .map_or(EvidenceStatus::Unknown, |e| e.status(at.as_ref(), wall))
    };
    Clock {
        boot: boot.into(),
        boottime: at.and_then(|t| i64::try_from(t.seconds).ok()).unwrap_or(0),
        mono,
        wall,
        wall_ok: clock_status.wall_usable(),
        how: evidence.as_ref().map(|e| e.source.how().into()),
        evidence,
        clock_status,
    }
}

// ── coverage: the crash watcher's probe and what it reports (§19 「T9c 前定」 2–4) ──

/// Between the long-term thread and the crash watcher's thread. Times are CLOCK_MONOTONIC ms.
pub struct Shared {
    /// The probe: the long-term thread writes, the crash watcher polls it and drains it.
    efd: Option<std::os::fd::OwnedFd>,
    /// The crash watcher's last wake (after it read kmsg clean); 0 = never.
    alive_ms: std::sync::atomic::AtomicU64,
    /// (route, from, to): records lost between a clean read and the read that saw the loss.
    spans: std::sync::Mutex<Vec<(&'static str, u64, u64)>>,
    /// Routes down now, since when.
    down: std::sync::Mutex<BTreeMap<&'static str, u64>>,
}

pub fn shared() -> &'static Shared {
    static S: std::sync::OnceLock<Shared> = std::sync::OnceLock::new();
    S.get_or_init(|| {
        use std::os::fd::FromRawFd;
        // SAFETY: eventfd with plain flags; the fd is owned below.
        let fd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        Shared {
            // SAFETY: a fresh fd we own.
            efd: (fd >= 0).then(|| unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) }),
            alive_ms: std::sync::atomic::AtomicU64::new(0),
            spans: std::sync::Mutex::new(Vec::new()),
            down: std::sync::Mutex::new(BTreeMap::new()),
        }
    })
}

impl Shared {
    /// The fd the crash watcher polls.
    pub fn probe_fd(&self) -> Option<std::os::fd::RawFd> {
        use std::os::fd::AsRawFd;
        self.efd.as_ref().map(|f| f.as_raw_fd())
    }

    pub fn probe(&self) {
        if let Some(fd) = self.probe_fd() {
            let one: u64 = 1;
            // SAFETY: an 8-byte write from a u64 we own.
            unsafe { libc::write(fd, (&one as *const u64).cast(), 8) };
        }
    }

    /// The crash watcher took the probe (the counter back to 0, so poll sleeps again).
    pub fn take_probe(&self) {
        if let Some(fd) = self.probe_fd() {
            let mut v: u64 = 0;
            // SAFETY: an 8-byte read into a u64 we own.
            unsafe { libc::read(fd, (&mut v as *mut u64).cast(), 8) };
        }
    }

    pub fn alive(&self, mono_ms: u64) {
        self.alive_ms
            .store(mono_ms.max(1), std::sync::atomic::Ordering::Relaxed);
    }

    pub fn lost(&self, src: &'static str, from_ms: u64, to_ms: u64) {
        self.spans
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((src, from_ms, to_ms));
    }

    pub fn set_down(&self, src: &'static str, since_ms: Option<u64>) {
        let mut d = self.down.lock().unwrap_or_else(|e| e.into_inner());
        match since_ms {
            Some(t) => {
                d.entry(src).or_insert(t);
            }
            None => {
                d.remove(src);
            }
        }
    }

    /// What was seen since the last report (spans are taken), in seconds: a span's ends widen
    /// to whole seconds.
    pub fn report(&self) -> Report {
        let a = self.alive_ms.load(std::sync::atomic::Ordering::Relaxed);
        let spans = std::mem::take(&mut *self.spans.lock().unwrap_or_else(|e| e.into_inner()));
        let down = self.down.lock().unwrap_or_else(|e| e.into_inner());
        Report {
            alive: (a > 0).then_some((a / 1000) as i64),
            spans: spans
                .into_iter()
                .map(|(s, f, t)| (s.to_string(), (f / 1000) as i64, t.div_ceil(1000) as i64))
                .collect(),
            down: down
                .iter()
                .map(|(s, t)| (s.to_string(), (*t / 1000) as i64))
                .collect(),
            looks: Vec::new(),
        }
    }
}

fn save_state(path: &Path, text: &str) {
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// doctor's `--calibrate-standby` wrote a new standby baseline (`rows` records): one `calib` event
/// (docs/LEDGER.md §4 calib), now that the guard's ledger spool is no longer read (§19 T9d, §20).
/// `id` as doctor makes it (`calib-<boot 8>-<uptime>`), only `[a-z0-9-]`, at most 40.
pub fn calib_event(dir: &Path, id: &str, rows: u64) -> std::io::Result<Value> {
    if id.is_empty()
        || id.len() > 40
        || !id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "bad id",
        ));
    }
    let c = clock_now(&crate::evlog::boot_id());
    let v = json!({"t": wall_or_null(&c), "up": c.boottime, "boot": boot8(&c.boot), "k": "calib", "id": id, "rows": rows});
    Store::at(dir).append_events(std::slice::from_ref(&v))?;
    Ok(v)
}

/// `u60d --longterm-once <scratch dir>`: one minute look, one power reading and the key.log
/// backfill against the real device, printed; nothing under `/data/luyaz/core/long` is read or written
/// (the look's own files go to the scratch dir). For a side run before a ship: the ship trial
/// does not read datad.env, so none of this runs until the core is live.
pub fn once(scratch: &Path) -> String {
    let t0 = std::time::Instant::now();
    let _ = std::fs::create_dir_all(scratch);
    let boot = crate::evlog::boot_id();
    let c = clock_now(&boot);
    let mut wp = watch::Paths::device();
    wp.long = scratch.to_path_buf();
    let mut st = watch::State::default();
    let lk = st.look(&wp, &boot, c.boottime, c.wall);
    let mut out = format!(
        "boot {boot} up {} wall {} wall_usable {} status {:?} how {:?}\n",
        c.boottime, c.wall, c.wall_ok, c.clock_status, c.how
    );
    for (k, v) in &lk.lines {
        out.push_str(&format!("{k} {v}\n"));
    }
    out.push_str(&format!(
        "cov {:?}\nafter {:?}\npids {:?}\n",
        lk.cov,
        lk.after,
        st.pids()
    ));
    let mut rs = power::ReadState::default();
    let r = rs.read(&power::Paths::device(), &st.pids(), c.wall);
    out.push_str(&format!("power {r:?}\n"));
    let host = |p: &str| PathBuf::from(crate::state::host_path(p));
    let cur = boot::KeyFile::read(&host(boot::KEYLOG));
    // anchor lost on purpose: every reason-code line of key.log, to compare with the guard's ids
    let bf = boot::backfill(cur.as_ref(), || None, None, true);
    for l in &bf.lines {
        out.push_str(&format!("boot_backfill {l}\n"));
    }
    out.push_str(&format!(
        "code {:?} anchor {:?}\n",
        cur.as_ref().and_then(boot::KeyFile::code),
        bf.anchor
    ));
    // G10/G11/G8 (§20) into the scratch dir: two standby samples 5 s apart (the line regardless
    // of the backlight), the fingerprints, and the kernel log from a fresh /dev/kmsg reader
    let mut sp = standby::Paths::device();
    sp.stat = scratch.join("standby.stat");
    sp.prev = scratch.join("standby.prev");
    sp.ltmp = scratch.join("ledger");
    let pids = st.pids();
    let a = standby::sample(&sp, c.boottime, &pids);
    std::thread::sleep(std::time::Duration::from_secs(5));
    let b = standby::sample(&sp, c.boottime + 5, &pids);
    out.push_str(&format!(
        "standby {a}\nstandby {b}\nstandby line {:?}\n",
        standby::line(&a, &b, standby::WAKE_GAP)
    ));
    match standby::fp_round(&sp, c.boottime, &pids) {
        Ok(()) => out.push_str(&format!(
            "fp\n{}",
            std::fs::read_to_string(sp.ltmp.join("fp")).unwrap_or_default()
        )),
        Err(e) => out.push_str(&format!("fp error {e}\n")),
    }
    out.push_str(&crate::crashwatch::io::capture_once(
        &scratch.join("crashcap"),
        &boot,
    ));
    let hwm = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .map(str::to_string)
        })
        .unwrap_or_default();
    out.push_str(&format!("took {} ms; {hwm}\n", t0.elapsed().as_millis()));
    out
}

/// The long-term thread: fold, append, prune once at start and once a day; every
/// [`PROBE_S`] charge coverage, probe the crash watcher and save the fold for a restart.
pub fn start(dir: PathBuf) -> std::sync::mpsc::Sender<Msg> {
    let (tx, rx) = std::sync::mpsc::channel::<Msg>();
    let _ = TX.set(std::sync::Mutex::new(tx.clone()));
    let _ = std::thread::Builder::new()
        .name("longterm".into())
        .spawn(move || {
            let boot = crate::evlog::boot_id();
            let store = Store::at(&dir);
            let state = PathBuf::from(crate::state::host_path(STATE));
            // the commit it was built from (tools/luyaz ship passes it), else the crate version
            let version = option_env!("LUYAZ_COMMIT")
                .filter(|c| !c.is_empty())
                .unwrap_or(env!("CARGO_PKG_VERSION"));
            let open = dir.join(OPEN);
            let mut fold = match std::fs::read_to_string(&state)
                .ok()
                .and_then(|s| Fold::resume(&s, version, &boot))
            {
                Some(f) => f,
                None => {
                    // a new boot (or this boot's state lost): the last boot's open hour first
                    let old = std::fs::read_to_string(&open).unwrap_or_default();
                    let hours = Fold::close_old(&old, &boot);
                    if !hours.is_empty() {
                        let out = Out {
                            hours,
                            events: Vec::new(),
                        };
                        match store.write(&out) {
                            Ok(_) => {
                                let _ = std::fs::remove_file(&open);
                            }
                            Err(e) => eprintln!("[longterm] last boot's hour: {e}"),
                        }
                    }
                    let mut f = Fold::new(version);
                    f.net_last = store.net_last();
                    f
                }
            };
            let mut places = places::Places::load(&dir);
            if !places.backfilled {
                places.backfill(Path::new(&crate::state::host_path(places::LEDGER)));
                if let Err(e) = places.save(&dir, clock_now(&boot).boottime) {
                    eprintln!("[longterm] places: {e}");
                }
            }
            let sh = shared();
            let paths = watch::Paths::device();
            let ppaths = power::Paths::device();
            let spaths = standby::Paths::device();
            let mut last_prune: Option<i64> = None;
            for m in rx {
                let c = clock_now(&boot);
                let mut out = Out::default();
                let mut backfill = None;
                match m {
                    Msg::Block(name, stale, data) => fold.block(&c, &name, stale, &data, &mut out),
                    Msg::Tick => fold.tick(&c, &mut out),
                    Msg::Boot(facts, bf) => {
                        if fold.boot_facts(&c, &facts, &bf.lines, &mut out) {
                            backfill = Some(bf);
                        }
                    }
                }
                let due = fold.check_due(&c);
                let mut after = Vec::new();
                if due {
                    // a planned-reboot marker whose reboot never came goes (boot.rs `planned`)
                    let _ = boot::planned_now();
                    let lk = fold.look(&c, &paths, &mut out);
                    after = lk.after;
                    // G10, G11 once the guard handed them over (u60-sentinel.md §20)
                    let (sb, fp) = standby::owned(&spaths);
                    if sb || fp {
                        let pids = fold.look.pids();
                        if sb && let Err(e) = standby::standby_round(&spaths, c.boottime, &pids) {
                            eprintln!("[longterm] standby: {e}");
                        }
                        if fp && let Err(e) = standby::fp_round(&spaths, c.boottime, &pids) {
                            eprintln!("[longterm] fp: {e}");
                        }
                    }
                    fold.power(&c, &ppaths, &mut out);
                    let mut rep = sh.report();
                    rep.looks = lk.cov;
                    fold.cover(&c, &rep, &mut out);
                    sh.probe();
                }
                let now = fold.places_now();
                places.step(
                    &boot8(&c.boot),
                    c.boottime,
                    c.wall_ok.then_some(c.wall),
                    &now,
                    &out.events,
                    due,
                );
                if places.save_due(c.boottime)
                    && let Err(e) = places.save(&dir, c.boottime)
                {
                    eprintln!("[longterm] places: {e}");
                }
                places::set_advice(places.advice(&now));
                // the lines first: a kill in between leaves at worst a line written twice,
                // never a saved state that thinks a line is on disk when it is not
                let written = store.write(&out);
                if due || !out.hours.is_empty() || backfill.is_some() {
                    let saved = fold.save();
                    save_state(&state, &saved);
                    save_state(&open, &saved);
                }
                if written.is_ok()
                    && !after.is_empty()
                    && let Err(e) = watch::commit(&dir, &after)
                {
                    eprintln!("[longterm] look files: {e}");
                }
                if written.is_ok()
                    && let Some(bf) = &backfill
                    && let Err(e) = boot::commit(&dir, bf)
                {
                    eprintln!("[longterm] key.log anchor: {e}");
                }
                if let Err(e) = written {
                    eprintln!("[longterm] write: {e}");
                } else if out.events.iter().any(|e| e["k"] == "net_change")
                    && let Some(k) = &fold.net_last
                    && let Err(e) = store.set_net_last(k)
                {
                    eprintln!("[longterm] net.last: {e}");
                }
                if c.wall_ok && last_prune.is_none_or(|t| c.mono - t >= 86_400) {
                    last_prune = Some(c.mono);
                    if let Err(e) = store.prune(Some(c.wall)) {
                        eprintln!("[longterm] prune: {e}");
                    }
                }
            }
        });
    tx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clk(boot: &str, up: i64, sleep: i64, wall: Option<i64>) -> Clock {
        Clock {
            boot: boot.into(),
            boottime: up,
            mono: up - sleep,
            wall: wall.unwrap_or(1000 + up),
            wall_ok: wall.is_some(),
            clock_status: if wall.is_some() {
                luyaz_api::clock::EvidenceStatus::Estimated
            } else {
                Default::default()
            },
            evidence: None,
            how: wall.map(|_| "vendor".into()),
        }
    }

    fn sig(mcc: i64, mnc: i64, home: i64) -> Value {
        json!({"mcc": mcc, "mnc": mnc, "home_mcc": home, "serving_mcc": mcc})
    }

    #[test]
    fn hours_on_the_wall_clock_with_sleep_abroad_charging_and_link() {
        let mut f = Fold::new("v1");
        let mut out = Out::default();
        // the first reading at 38:20 past an hour, the hour turns at uptime 1400
        let w0 = 1_791_500_400 + 2300;
        assert_eq!((w0 - 100 + 1400) % 3600, 0);
        let c = |up: i64, sleep: i64| clk("8559f5dcaaaa", up, sleep, Some(w0 - 100 + up));
        f.block(&c(100, 0), "signal", false, &sig(440, 10, 460), &mut out);
        f.block(
            &c(100, 0),
            "battery",
            false,
            &json!({"percent": 80, "temp": 300, "charging": 0}),
            &mut out,
        );
        f.block(
            &c(100, 0),
            "interfaces",
            false,
            &json!({"cellular": {"connect_status": "ipv4_connected"}}),
            &mut out,
        );
        f.tick(&c(700, 0), &mut out);
        f.block(
            &c(700, 0),
            "interfaces",
            false,
            &json!({"cellular": {"connect_status": "disconnected"}}),
            &mut out,
        );
        f.block(
            &c(800, 0),
            "battery",
            false,
            &json!({"percent": 70, "temp": 350, "charging": 1}),
            &mut out,
        );
        f.tick(&c(1300, 300), &mut out); // slept 300 s in between
        assert!(hrs(&out).is_empty());
        f.tick(&c(1400, 300), &mut out); // the wall clock's hour turned
        assert_eq!(hrs(&out).len(), 1, "{out:?}");
        let h = hrs(&out)[0];
        assert_eq!(h["from"], 100);
        assert_eq!(h["to"], 1400);
        assert_eq!(h["asleep"], 300);
        assert_eq!(h["awake"], 1000);
        assert_eq!(h["net"], "440-10");
        assert_eq!(h["home_mcc"], 460);
        assert_eq!(h["batt_from"], 80);
        assert_eq!(h["batt_to"], 70);
        assert_eq!(h["batt_tmax"], 350);
        assert_eq!(h["link_down"], 1);
        assert!(h["abroad"].as_i64().unwrap() >= 900);
        assert!(h["chg_s"].as_i64().unwrap() > 0);
        let kinds: Vec<&str> = out
            .events
            .iter()
            .map(|e| e["k"].as_str().unwrap())
            .collect();
        // the boot line waits for its facts; without them it goes out 600 s into the boot
        assert_eq!(kinds, ["ver", "clock", "boot", "link"]);
    }

    #[test]
    fn untrusted_clock_hours_by_uptime_then_switch() {
        let mut f = Fold::new("v1");
        let mut out = Out::default();
        f.tick(&clk("b1", 10, 0, None), &mut out);
        f.tick(&clk("b1", 3599, 0, None), &mut out);
        assert!(hrs(&out).is_empty());
        f.tick(&clk("b1", 3600, 0, None), &mut out);
        assert_eq!(hrs(&out).len(), 1);
        assert_eq!(hrs(&out)[0]["ft"], Value::Null);
        // trusted mid-hour: the uptime hour closes there
        f.tick(&clk("b1", 4000, 0, Some(1_791_500_000)), &mut out);
        assert_eq!(hrs(&out).len(), 2);
        assert!(out.events.iter().any(|e| e["k"] == "clock"));
    }

    #[test]
    fn stale_flips_and_reboot() {
        let mut f = Fold::new("v1");
        let mut out = Out::default();
        f.block(
            &clk("b1", 10, 0, None),
            "signal",
            false,
            &sig(460, 0, 460),
            &mut out,
        );
        f.block(
            &clk("b1", 20, 0, None),
            "signal",
            true,
            &Value::Null,
            &mut out,
        );
        f.block(
            &clk("b1", 30, 0, None),
            "signal",
            false,
            &sig(460, 0, 460),
            &mut out,
        );
        f.tick(&clk("b2", 5, 0, None), &mut out);
        let kinds: Vec<&str> = out
            .events
            .iter()
            .map(|e| e["k"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, ["ver", "stale", "stale", "ver"]);
    }

    #[test]
    fn rebuild_gives_what_live_gave() {
        let raw: Vec<RawEvent> = (0..200)
            .map(|i| {
                let c = clk("b1", i * 60, i / 50 * 30, Some(1_791_500_000 + i * 60));
                let b = match i % 4 {
                    0 => Some(("signal".to_string(), false, sig(460, 1, 460))),
                    1 => Some(("battery".to_string(), false, json!({"percent": 100 - i / 4, "temp": 300, "charging": i % 3}))),
                    2 => Some((
                        "interfaces".to_string(),
                        false,
                        json!({"cellular": {"connect_status": if i % 40 < 20 { "ipv4_connected" } else { "disconnected" }}}),
                    )),
                    _ => None,
                };
                (c, b)
            })
            .collect();
        let mut live = Fold::new("v1");
        let mut out = Out::default();
        for (c, b) in &raw {
            match b {
                Some((n, s, d)) => live.block(c, n, *s, d, &mut out),
                None => live.tick(c, &mut out),
            }
        }
        assert_eq!(rebuild("v1", &raw), out);
        assert!(hrs(&out).len() >= 3);
    }

    /// A check where the crash watcher woke at `a` and the minute look's sources worked.
    fn rpt(a: i64) -> Report {
        Report {
            alive: Some(a),
            looks: LOOKED.iter().map(|s| (s.to_string(), true)).collect(),
            ..Default::default()
        }
    }

    /// The `hour` lines (each hour also writes `hour_power`, `hour_proc`).
    fn hrs(out: &Out) -> Vec<&Value> {
        out.hours.iter().filter(|h| h["k"] == "hour").collect()
    }

    fn kinds(out: &Out) -> Vec<&str> {
        out.events
            .iter()
            .map(|e| e["k"].as_str().unwrap())
            .collect()
    }

    /// A clock on the wall's hour grid: uptime `up`, slept `sleep`, the wall at `W + up`.
    const W: i64 = 1_791_500_400; // on the hour
    fn wc(up: i64, sleep: i64) -> Clock {
        clk("b1", up, sleep, Some(W + up))
    }

    #[test]
    fn a_reboot_closes_the_last_boots_open_hour_at_its_last_check() {
        let mut f = Fold::new("v1");
        let mut out = Out::default();
        for t in (100..=1000).step_by(60) {
            f.cover(&wc(t, 0), &rpt(t), &mut out);
        }
        let saved = f.save();
        assert!(
            Fold::close_old(&saved, "b1").is_empty(),
            "this boot's: resumed instead"
        );
        assert!(Fold::close_old("not json", "b2").is_empty());
        let v = Fold::close_old(&saved, "b2");
        let k: Vec<_> = v.iter().map(|x| x["k"].as_str().unwrap()).collect();
        assert_eq!(k, ["hour", "hour_power", "hour_proc"]);
        assert_eq!(
            (
                v[0]["boot"].clone(),
                v[0]["from"].clone(),
                v[0]["to"].clone()
            ),
            (json!("b1"), json!(100), json!(1000))
        );
        assert_eq!(v[0]["tt"], W + 1000);
        assert_eq!(v[0]["g_core"], 0);
        // nothing open (the hour just closed and the fold is between hours): nothing
        let mut g = Fold::new("v1");
        g.last = Some(wc(5, 0));
        g.boot = "b1".into();
        assert!(Fold::close_old(&g.save(), "b2").is_empty());
    }

    #[test]
    fn restart_in_the_same_hour_continues_it_and_charges_every_source() {
        let mut f = Fold::new("v1");
        let mut out = Out::default();
        f.cover(&wc(100, 0), &Report::default(), &mut out);
        f.cover(&wc(160, 0), &rpt(150), &mut out);
        let saved = f.save();
        // the process is gone 160..400; another one of the same boot, same version
        let mut f = Fold::resume(&saved, "v1", "b1").unwrap();
        assert!(
            Fold::resume(&saved, "v1", "b2").is_none(),
            "another boot starts fresh"
        );
        let mut out2 = Out::default();
        f.tick(&wc(400, 0), &mut out2);
        assert!(
            kinds(&out2).is_empty(),
            "no second boot/ver/clock: {out2:?}"
        );
        f.cover(&wc(400, 0), &rpt(399), &mut out2);
        f.cover(&wc(460, 0), &rpt(455), &mut out2);
        f.tick(&wc(3600, 0), &mut out2);
        assert_eq!(hrs(&out2).len(), 1);
        let h = hrs(&out2)[0];
        assert_eq!(h["from"], 100);
        for s in WATCHED {
            assert_eq!(h[format!("g_{s}")], 240, "{s}: {h}");
        }

        // the 240 s run was under 10 min: no gap event
        assert!(!kinds(&out2).contains(&"gap"));
        // a new version says so once
        let mut f = Fold::resume(&f.save(), "v2", "b1").unwrap();
        let mut out3 = Out::default();
        f.tick(&wc(3700, 0), &mut out3);
        assert_eq!(kinds(&out3), ["ver"]);
    }

    #[test]
    fn restart_across_hours_closes_where_saved_and_writes_one_spanning_gap() {
        let mut f = Fold::new("v1");
        let mut out = Out::default();
        f.cover(&wc(100, 0), &Report::default(), &mut out);
        f.cover(&wc(3000, 0), &rpt(2990), &mut out);
        let saved = f.save();
        // gone 3000..9000 with 1000 s asleep in it: two hour turns missed
        let mut f = Fold::resume(&saved, "v1", "b1").unwrap();
        let mut out2 = Out::default();
        f.tick(&wc(9000, 1000), &mut out2);
        assert_eq!(hrs(&out2).len(), 1);
        assert_eq!(hrs(&out2)[0]["from"], 100);
        assert_eq!(hrs(&out2)[0]["to"], 3000, "closed where it was saved");
        assert_eq!(hrs(&out2)[0]["g_core"], 0);
        let g = out2.events.iter().find(|e| e["k"] == "gap").expect("gap");
        assert_eq!(g["src"], "core");
        assert_eq!(g["no_hour"], 1);
        assert_eq!(
            (g["from"].as_i64(), g["to"].as_i64()),
            (Some(3000), Some(9000))
        );
        assert_eq!(g["awake"], 5000, "awake only: the sleep is neither");
        assert_eq!(
            (g["ft"].as_i64(), g["tt"].as_i64()),
            (Some(W + 3000), Some(W + 9000))
        );
        // the next hour starts at the restart and owes nothing for the time before it
        f.cover(&wc(9000, 1000), &rpt(8999), &mut out2);
        f.tick(&wc(10_800, 1000), &mut out2);
        let h = *hrs(&out2).last().unwrap();
        assert_eq!(h["from"], 9000);
        assert_eq!(h["g_core"], 0);
        assert_eq!(h["g_watcher"], 0);
    }

    #[test]
    fn the_watchers_routes_down_lost_and_unanswered() {
        let mut f = Fold::new("v1");
        let mut out = Out::default();
        let ok = rpt;
        f.cover(&wc(0, 0), &Report::default(), &mut out);
        // the dump directory could not be watched from the start
        let down = Report {
            alive: Some(55),
            down: vec![("capture".into(), 0)],
            ..Default::default()
        };
        f.cover(&wc(60, 0), &down, &mut out);
        // kmsg lost records between its clean read at 100 and the read at 103
        let lost = Report {
            alive: Some(115),
            spans: vec![("watcher".into(), 100, 103)],
            down: vec![("capture".into(), 0)],
            ..Default::default()
        };
        f.cover(&wc(120, 0), &lost, &mut out);
        // the thread stops answering after its wake at 130 until 1205 (asleep 300 s in it)
        for t in (180..=3540).step_by(60) {
            let mono = t - (t - 180).min(300);
            let mut r = ok(if t <= 1200 { 130 } else { mono - 5 });
            r.down = vec![("capture".into(), 0)];
            f.cover(&wc(t, t - mono), &r, &mut out);
        }
        f.tick(&wc(3600, 300), &mut out);
        let h = hrs(&out)[0];
        assert_eq!(h["awake"], 3300);
        assert_eq!(h["g_core"], 0);
        // charged up to the last check (mono 3240); the rest goes to the next hour
        assert_eq!(h["g_capture"], 3240);
        // 3 lost + 130..(1200−300) unanswered by MONOTONIC = 770
        assert_eq!(h["g_watcher"], 3 + 770);
        assert_eq!(h["g_link"], 770);
        // the two checks whose report had no minute look: its sources down for those minutes
        assert_eq!(h["g_crashlog"], 120);
        assert_eq!(h["g_uidlog"], 120);
        assert_eq!(h["g_datad"], 0);
        let gaps: Vec<&Value> = out.events.iter().filter(|e| e["k"] == "gap").collect();
        assert_eq!(gaps.len(), 2, "watcher and link ran past 10 min: {gaps:?}");
        for g in &gaps {
            assert_eq!(g["awake"], 770);
            assert_eq!(g["to"], 1200);
        }
    }

    #[test]
    fn the_boot_line_once_with_its_facts_and_the_backfill_before_it() {
        let mut f = Fold::new("v1");
        let mut out = Out::default();
        f.tick(&wc(30, 0), &mut out);
        let facts = json!({"code": 1185, "mode": "power_on", "pon": 3});
        let bf = [
            json!({"id": "kl-7-31", "code": 1134, "at": "2026-10-02 01:00:00", "started": null, "gap": 0}),
        ];
        assert!(f.boot_facts(&wc(61, 0), &facts, &bf, &mut out));
        assert_eq!(kinds(&out), ["ver", "clock", "boot_backfill", "boot"]);
        let b = out.events.last().unwrap();
        assert_eq!(b["boot_id"], "b1");
        assert_eq!(b["boot"], "b1", "boot8, as every core line");
        assert_eq!(
            (b["code"].clone(), b["mode"].clone(), b["pon"].clone()),
            (json!(1185), json!("power_on"), json!(3))
        );
        assert_eq!(out.events[2]["id"], "kl-7-31");
        // a restart of the same boot does not write it again, even past the late mark
        let mut f = Fold::resume(&f.save(), "v1", "b1").unwrap();
        let mut out2 = Out::default();
        assert!(!f.boot_facts(&wc(90, 0), &facts, &bf, &mut out2));
        f.tick(&wc(700, 0), &mut out2);
        assert!(!kinds(&out2).contains(&"boot"));
        // a state saved before boot_said existed had written it already
        let old = f
            .save()
            .replace(",\"boot_said\":true", "")
            .replace(",\"facts_said\":true", "");
        assert!(!old.contains("boot_said") && !old.contains("facts_said"));
        let mut f = Fold::resume(&old, "v1", "b1").unwrap();
        f.tick(&wc(800, 0), &mut out2);
        assert!(!kinds(&out2).contains(&"boot"));
    }

    /// A core that first starts long into a boot (the first ship of this version: no saved
    /// state) waits for the facts; facts later than the late mark still go out once.
    #[test]
    fn a_late_start_still_gets_its_facts() {
        let mut f = Fold::new("v1");
        let mut out = Out::default();
        f.block(
            &wc(5000, 0),
            "signal",
            false,
            &json!({"mcc": 460, "mnc": 1}),
            &mut out,
        );
        f.tick(&wc(5030, 0), &mut out);
        assert!(
            !kinds(&out).contains(&"boot"),
            "no boot line before the facts: {out:?}"
        );
        let facts = json!({"code": 1185});
        assert!(f.boot_facts(&wc(5060, 0), &facts, &[], &mut out));
        assert_eq!(out.events.iter().filter(|e| e["k"] == "boot").count(), 1);
        // facts that never come: the line without them 600 s after the first reading
        let mut f = Fold::new("v1");
        let mut out = Out::default();
        f.tick(&wc(5000, 0), &mut out);
        f.tick(&wc(5599, 0), &mut out);
        assert!(!kinds(&out).contains(&"boot"));
        f.tick(&wc(5600, 0), &mut out);
        assert_eq!(kinds(&out).iter().filter(|k| **k == "boot").count(), 1);
        // then they come: once more with them, the backfill before it, and the anchor may move
        let bf = [json!({"id": "kl-7-31", "code": 1134})];
        assert!(f.boot_facts(&wc(5700, 0), &facts, &bf, &mut out));
        assert_eq!(
            &kinds(&out)[kinds(&out).len() - 2..],
            ["boot_backfill", "boot"]
        );
        assert_eq!(out.events.last().unwrap()["code"], 1185);
        assert!(!f.boot_facts(&wc(5800, 0), &facts, &bf, &mut out));
    }

    #[test]
    fn clock_trusted_revoked_again_and_moved() {
        let mut f = Fold::new("v1");
        let mut out = Out::default();
        f.tick(&clk("b1", 10, 0, None), &mut out);
        f.tick(&clk("b1", 20, 0, Some(W + 20)), &mut out);
        f.tick(&clk("b1", 30, 0, Some(W + 30 + 3600)), &mut out); // even an hour must now produce a clock event
        f.tick(&clk("b1", 40, 0, None), &mut out);
        f.tick(&clk("b1", 50, 0, Some(W + 50)), &mut out);
        f.tick(&clk("b1", 60, 0, Some(W + 60 + 2 * 86_400)), &mut out);
        let ck: Vec<(Value, Value)> = out
            .events
            .iter()
            .filter(|e| e["k"] == "clock")
            .map(|e| (e["offset"].clone(), e["how"].clone()))
            .collect();
        assert_eq!(
            ck,
            [
                (json!(W), json!("vendor")),
                (json!(W + 3600), json!("vendor")),
                (Value::Null, json!("revoked")),
                (json!(W), json!("vendor")),
                (json!(W + 2 * 86_400), json!("vendor")),
            ]
        );
    }

    #[test]
    fn net_change_when_the_preference_or_the_country_moves() {
        let mut f = Fold::new("v1");
        f.net_last = Some("TCHGWL_5G 460".into()); // from net.last: the same as now
        let mut out = Out::default();
        let sg = |ns: &str, mcc: i64, mnc: i64| json!({"net_select": ns, "mcc": mcc, "mnc": mnc, "home_mcc": 460, "serving_mcc": mcc});
        f.block(
            &wc(10, 0),
            "sim",
            false,
            &json!({"imsi": "460011234567890"}),
            &mut out,
        );
        f.block(
            &wc(10, 0),
            "signal",
            false,
            &sg("TCHGWL_5G", 460, 1),
            &mut out,
        );
        f.block(
            &wc(20, 0),
            "signal",
            false,
            &sg("TCHGWL_5G", 460, 0),
            &mut out,
        );
        assert!(
            !kinds(&out).contains(&"net_change"),
            "same preference and country"
        );
        f.block(
            &wc(30, 0),
            "signal",
            false,
            &sg("WL_AND_5G", 460, 0),
            &mut out,
        );
        f.block(
            &wc(40, 0),
            "signal",
            false,
            &sg("WL_AND_5G", 310, 260),
            &mut out,
        );
        f.block(&wc(50, 0), "signal", true, &Value::Null, &mut out);
        f.block(&wc(60, 0), "signal", false, &sg("", 310, 260), &mut out);
        let nc: Vec<&Value> = out
            .events
            .iter()
            .filter(|e| e["k"] == "net_change")
            .collect();
        assert_eq!(nc.len(), 2, "{nc:?}");
        assert_eq!(nc[0]["net_select"], "WL_AND_5G");
        assert_eq!(nc[0]["net"], "460-00");
        assert_eq!(nc[0]["home"], "460-01");
        assert_eq!(nc[1]["net"], "310-260");
        assert_eq!(f.net_last.as_deref(), Some("WL_AND_5G 310"));
        // a restart of the same boot carries it
        let f = Fold::resume(&f.save(), "v1", "b1").unwrap();
        assert_eq!(f.net_last.as_deref(), Some("WL_AND_5G 310"));
        // the store keeps it across boots
        let d = std::env::temp_dir().join(format!("u60d-netlast-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let st = Store::at(&d);
        assert_eq!(st.net_last(), None);
        st.set_net_last("WL_AND_5G 310").unwrap();
        assert_eq!(st.net_last().as_deref(), Some("WL_AND_5G 310"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn store_appends_and_prunes_by_age_and_size() {
        let d = std::env::temp_dir().join(format!("u60d-long-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let s = Store::at(&d);
        let now = 1_800_000_000;
        let hours: Vec<Value> = (0..5)
            .map(|i| json!({"k": "hour", "tt": now - i * 30 * 86_400}))
            .collect();
        let events: Vec<Value> = (0..3)
            .map(|i| json!({"k": "link", "t": if i == 2 { Value::Null } else { json!(now - i * 40 * 86_400) }}))
            .collect();
        s.write(&Out { hours, events }).unwrap();
        s.prune(None).unwrap();
        assert_eq!(
            std::fs::read_to_string(d.join("hours.jsonl"))
                .unwrap()
                .lines()
                .count(),
            5
        );
        s.prune(Some(now)).unwrap();
        let h = std::fs::read_to_string(d.join("hours.jsonl")).unwrap();
        assert_eq!(h.lines().count(), 4, "older than 90 days gone");
        let e = std::fs::read_to_string(d.join("events.jsonl")).unwrap();
        assert_eq!(
            e.lines().count(),
            2,
            "older than 30 days gone, untimed kept"
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod calib_tests {
    #[test]
    fn calib_event_one_line_and_only_doctors_ids() {
        let d = std::env::temp_dir().join(format!("u60d-calib-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let v = super::calib_event(&d, "calib-c6f6ed41-5000", 42).unwrap();
        assert_eq!(v["k"], "calib");
        assert_eq!(v["rows"], 42);
        let t = std::fs::read_to_string(d.join("events.jsonl")).unwrap();
        assert_eq!(t.lines().count(), 1);
        assert!(t.contains(r#""id":"calib-c6f6ed41-5000""#));
        for bad in ["", "Calib-x", "a b", "x\"y", &"a".repeat(41)] {
            assert!(super::calib_event(&d, bad, 1).is_err(), "{bad:?}");
        }
        assert_eq!(std::fs::read_to_string(d.join("events.jsonl")).unwrap(), t);
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod clock_evidence_tests {
    use super::*;
    use luyaz_api::clock::{BootTime, ClockEvidence, ClockSource, EvidenceStatus};
    const WALL: i64 = 1_790_000_000;
    fn at(boot: &str, seconds: u64) -> BootTime {
        BootTime {
            boot_id: boot.into(),
            seconds,
        }
    }

    #[test]
    fn snapshot_retains_raw_wall_but_drops_expired_wrong_boot_and_stepped_wall_labels() {
        let e = ClockEvidence::observation(WALL, at("a", 100), Some(100), ClockSource::Year);
        for (seconds, step, boot, status) in [
            (100, 0, "a", EvidenceStatus::Estimated),
            (280, 0, "a", EvidenceStatus::Estimated),
            (281, 0, "a", EvidenceStatus::Stale),
            (160, -28_800, "a", EvidenceStatus::WallChanged),
            (160, 28_800, "a", EvidenceStatus::WallChanged),
            (160, 0, "b", EvidenceStatus::BootMismatch),
        ] {
            let wall = WALL + (seconds as i64 - 100) + step;
            let c = clock_snapshot(boot, Some(at(boot, seconds)), 100, wall, &e.encode());
            assert_eq!(c.clock_status, status);
            assert_eq!(c.wall, wall);
            assert_eq!(c.evidence, Some(e.clone()));
            let v = Fold::new("test").event(&c, "reading", json!({}));
            assert_eq!(v["raw_wall"], wall);
            assert_eq!(v["up"], seconds);
            assert_eq!(v["t"].is_null(), !status.wall_usable());
        }
        for text in ["", "1 1790000000 100 year\n", "broken"] {
            assert!(!clock_snapshot("a", Some(at("a", 100)), 100, WALL, text).wall_ok);
        }
        assert!(!clock_snapshot("a", None, 100, WALL, &e.encode()).wall_ok);
    }

    #[test]
    fn fold_records_revocation_and_keeps_elapsed_energy_and_events_after_a_clock_step() {
        let mut f = Fold::new("test");
        let mut out = Out::default();
        let e = ClockEvidence::observation(WALL, at("a", 100), Some(100), ClockSource::Year);
        let c1 = clock_snapshot("a", Some(at("a", 100)), 100, WALL, &e.encode());
        f.tick(&c1, &mut out);
        let c2 = clock_snapshot(
            "a",
            Some(at("a", 160)),
            160,
            WALL + 60 + 28_800,
            &e.encode(),
        );
        f.tick(&c2, &mut out);
        assert!(out.events.iter().any(|v| v["k"] == "clock"
            && v["offset"].is_null()
            && v["clock_status"] == "wall_changed"));
        assert!(f.hour.is_some(), "accounting continues using boot time");
        let serialized = serde_json::to_string(&c2).unwrap();
        assert_eq!(serde_json::from_str::<Clock>(&serialized).unwrap(), c2);
        // Older persisted snapshots remain readable with explicit unknown evidence.
        let old: Clock = serde_json::from_str(
            r#"{"boot":"a","boottime":1,"mono":1,"wall":1790000000,"wall_ok":true}"#,
        )
        .unwrap();
        assert_eq!(old.clock_status, EvidenceStatus::Unknown);
        assert!(old.evidence.is_none());
    }
}
