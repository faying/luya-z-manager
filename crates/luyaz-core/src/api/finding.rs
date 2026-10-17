//! "Registered but no internet" in the core (u60-mcp.md step 1 phase 1; T3): every
//! [`EVERY_ROUNDS`] sampler rounds the readings go through netwatch's finding runner and the
//! verdict into the store (`luyaz_netwatch::finding::store`); transitions become `finding.*`
//! events in the signal log (on disk, 7 days) and the store goes to [`FILE`].
//!
//! Storage (owner 2026-10-17): design D5/D11 put the open findings and the suppressed table in
//! an evlog block, but the evlog does not reach the disk on the device yet (Phase 3), so a small
//! file stands in: the store's one JSON value, written (tmp + rename) only on a transition.
//! It moves into the block when the evlog is on disk.
//!
//! What it reads (no AT: the AT port has one lock and the data layer's `AT+CGACT?`/`AT+CEER`
//! readings are left out for now): the signal block (`type`, `roaming`), the interfaces
//! block's `cellular` (`enable` read like the home screen does, `roam_enable`, `connect_status`, `connect_fail_count`,
//! `ipv4_address`), the sim block's `iccid`, and the home screen's `story.state` (no SIM,
//! airplane). It probes only when the passive reading has said down for [`DOWN_HOLD_SECS`] in a
//! row, a client is connected, and the last probing check is [`RECHECK_SECS`] ago (design:
//! rechecks at most every 10 minutes).

use serde_json::Value;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use luyaz_api::diagnose::Gate;
use luyaz_api::finding::{DataIn, Premises, Probe, Reg, Verdict};
use luyaz_netwatch::finding::store::{Change, Store};
use luyaz_netwatch::finding::{self as nf, FindingHost};
use luyaz_netwatch::{Watch, record};

/// The store between restarts (see the module doc).
pub const FILE: &str = "/data/luyaz/core/findings.json";
/// Sampler rounds (5 s) between checks.
pub const EVERY_ROUNDS: u32 = 6;
pub const RECHECK_SECS: u64 = 600;
/// How long the passive reading must say down, without a break, before a probe may run (design
/// Open Question 2, owner 2026-10-17). Why 60 s: the signal log 2026-10-10..10-17 (7.5 days)
/// shows reconnects while registered back within 10 s, and the one long gap (100 s) came back by
/// itself. The pilot only explains, so a false finding costs little; raise this in phase 2 when
/// a finding can trigger an automatic action.
pub const DOWN_HOLD_SECS: u64 = 60;

/// The "sure" finding the home screens show (T6): set on load and on every transition, read by
/// `/v2/screen` (over `net.home`) and `/api/public/status`. A slot because the screen view is
/// built outside the watch group.
static HOME: Mutex<Option<Home>> = Mutex::new(None);

/// What the home screens get of an open finding: never the ICCID.
#[derive(Clone, Debug, PartialEq)]
pub struct Home {
    pub kind: String,
    pub layer: String,
    pub symptom: String,
    pub symptom_en: String,
}

impl Home {
    pub fn to_value(&self) -> Value {
        serde_json::json!({
            "kind": self.kind,
            "layer": self.layer,
            "symptom": self.symptom,
            "symptom_en": self.symptom_en,
        })
    }
}

/// The finding the home screens show now, if any.
pub fn home_now() -> Option<Home> {
    lock(&HOME).clone()
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// One round's readings, taken by the sampler.
pub struct Inputs {
    /// `luyaz_netwatch::state_from_blocks`
    pub state: Option<Value>,
    /// the `/v2/screen` view
    pub screen: Option<Value>,
    pub iccid: String,
    pub passive_down: bool,
    pub clients: bool,
    /// the last probing check is long enough ago
    pub probe_due: bool,
    /// the passive reading has said down for [`DOWN_HOLD_SECS`] in a row
    pub held_down: bool,
    pub busy: Option<&'static str>,
    /// set when the internet probe ran
    pub probed: AtomicBool,
}

fn cellular(state: Option<&Value>) -> Option<&Value> {
    state?.get("interfaces")?.get("cellular")
}

/// The signal block's `roaming`, read like the home screen does: empty = unknown; `Home`,
/// `home`, `0` = not roaming.
fn roaming(net: &Value) -> Option<bool> {
    let r = net["roaming"].as_str().unwrap_or("");
    (!r.is_empty()).then(|| r != "Home" && r != "home" && r != "0")
}

/// `1`/`0` as a number or a string (firmware differ).
fn flag(v: Option<&Value>, key: &str) -> Option<bool> {
    match v?.get(key)? {
        Value::Number(n) => n.as_i64().map(|n| n == 1),
        Value::String(s) => s.trim().parse::<i64>().ok().map(|n| n == 1),
        _ => None,
    }
}

impl FindingHost for Inputs {
    fn premises(&self) -> Premises {
        let story = self
            .screen
            .as_ref()
            .and_then(|s| s["net"]["story"]["state"].as_str());
        let cell = cellular(self.state.as_ref());
        Premises {
            sim_present: story.map(|s| s != "nosim"),
            airplane: story.map(|s| s == "airplane"),
            // `enable` reads 0 after a boot while auto-dial connects: the home screen's rule
            data_on: cell.and_then(crate::ops::data_switch),
            roaming: self.state.as_ref().and_then(|s| roaming(&s["net"])),
            roam_allowed: flag(cell, "roam_enable"),
        }
    }

    fn passive_down(&self) -> bool {
        self.passive_down
    }

    fn can_probe(&self) -> bool {
        self.clients && self.probe_due && self.held_down
    }

    fn iccid(&self) -> String {
        self.iccid.clone()
    }

    fn others_busy(&self) -> Option<&'static str> {
        self.busy
    }

    fn registration(&self) -> Reg {
        let Some(net) = self.state.as_ref().map(|s| &s["net"]) else {
            return Reg::Unknown;
        };
        let t = net["type"].as_str().unwrap_or("").trim();
        if t.is_empty() {
            Reg::Unknown
        } else if !crate::ops::registered(t) {
            Reg::NotRegistered
        } else if roaming(net) == Some(true) {
            Reg::Roaming
        } else {
            Reg::Home
        }
    }

    fn data(&self) -> DataIn {
        let cell = cellular(self.state.as_ref());
        let s = |k: &str| cell.and_then(|c| c[k].as_str()).map(str::to_string);
        DataIn {
            connect_status: s("connect_status"),
            fail_count: cell.and_then(|c| c["connect_fail_count"].as_i64()),
            has_ip: cell.map(|c| c["ipv4_address"].as_str().is_some_and(|a| !a.is_empty())),
            pdp_active: None,
            ceer: None,
        }
    }

    fn operator_dns(&self) -> Vec<String> {
        self.state
            .as_ref()
            .map(luyaz_netwatch::operator_dns)
            .unwrap_or_default()
    }

    fn internet_probe(&self) -> Option<Probe> {
        self.probed.store(true, Ordering::SeqCst);
        Some(nf::internet_either())
    }
}

/// The open findings and the suppressed table, and when the last probing check ran.
pub struct Findings {
    store: Mutex<Store>,
    path: PathBuf,
    last_probe: AtomicU64,
    /// when the passive reading first said down in the current run of downs; 0 = up
    down_since: AtomicU64,
}

impl Findings {
    /// The store from `path` (missing or unreadable: empty).
    pub fn load(path: PathBuf) -> Findings {
        let store = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .map(|v| Store::from_value(&v))
            .unwrap_or_default();
        let f = Findings {
            store: Mutex::new(store),
            path,
            last_probe: AtomicU64::new(0),
            down_since: AtomicU64::new(0),
        };
        f.publish();
        f
    }

    /// Design: only "sure" goes on the home screens; "likely" and "guess" stay on the web's
    /// evidence page. The most recently seen one.
    pub fn home(&self) -> Option<Home> {
        let s = lock(&self.store);
        let e = s
            .open
            .iter()
            .filter(|e| e.finding["confidence"] == "sure")
            .max_by_key(|e| e.last)?;
        let f = &e.finding;
        let t = |k: &str| f[k].as_str().unwrap_or("").to_string();
        Some(Home {
            kind: e.kind.clone(),
            layer: t("layer"),
            symptom: t("symptom"),
            symptom_en: t("symptom_en"),
        })
    }

    fn publish(&self) {
        *lock(&HOME) = self.home();
    }

    /// The store as JSON (what [`FILE`] holds).
    #[cfg(test)]
    pub fn value(&self) -> Value {
        lock(&self.store).to_value()
    }

    /// `/api/diagnose`'s `finding` (T5, D6): the open finding seen most recently, or null.
    /// The ICCID leaves as its last 4 digits only (the contract marks it personal).
    pub fn view(&self) -> Value {
        let s = lock(&self.store);
        let Some(e) = s.open.iter().max_by_key(|e| e.last) else {
            return Value::Null;
        };
        let mut f = e.finding.clone();
        if let Some(o) = f.as_object_mut() {
            o.remove("iccid");
            o.insert(
                "iccid_tail".into(),
                e.iccid[e.iccid.len().saturating_sub(4)..].into(),
            );
            o.insert("first".into(), e.first.into());
            o.insert("last".into(), e.last.into());
            o.insert("count".into(), e.count.into());
            o.insert("open".into(), s.open.len().into());
        }
        f
    }

    pub fn probe_due(&self, now: u64) -> bool {
        now.saturating_sub(self.last_probe.load(Ordering::SeqCst)) >= RECHECK_SECS
    }

    /// Tracks the run of passive downs (same clock as [`Findings::probe_due`]): the first down
    /// starts it, an up clears it. True once it has lasted [`DOWN_HOLD_SECS`]. A short outage
    /// never probes, so it never starts the [`RECHECK_SECS`] wait.
    pub fn held_down(&self, passive_down: bool, now: u64) -> bool {
        if !passive_down {
            self.down_since.store(0, Ordering::SeqCst);
            return false;
        }
        let since = match self.down_since.load(Ordering::SeqCst) {
            0 => {
                self.down_since.store(now, Ordering::SeqCst);
                now
            }
            t => t,
        };
        now.saturating_sub(since) >= DOWN_HOLD_SECS
    }

    /// One check. Busy (another operation holds the gate): nothing changes, try next time.
    pub fn round(
        &self,
        i: &Inputs,
        gate: &Mutex<Gate>,
        watch: &Watch,
        now: u64,
    ) -> Option<Verdict> {
        let v = nf::check(i, gate).ok()?;
        if i.probed.load(Ordering::SeqCst) {
            self.last_probe.store(now, Ordering::SeqCst);
        }
        self.observe(&v, &i.iccid, watch, now);
        Some(v)
    }

    pub(super) fn observe(&self, v: &Verdict, iccid: &str, watch: &Watch, now: u64) {
        let changes = lock(&self.store).observe(v, iccid, now);
        self.apply(&changes, watch, now);
        // the confidence of an open one can change without a transition
        self.publish();
    }

    /// The user's "ignore" on what [`Findings::view`] shows (the web, T6): the open finding of
    /// `kind` seen most recently, checked against the ICCID's last 4 digits when given (the
    /// view never carries the whole ICCID). False: nothing open matches.
    pub fn dismiss_shown(&self, kind: &str, tail: Option<&str>, watch: &Watch, now: u64) -> bool {
        let iccid = {
            let s = lock(&self.store);
            let Some(e) = s
                .open
                .iter()
                .filter(|e| e.kind == kind && tail.is_none_or(|t| e.iccid.ends_with(t)))
                .max_by_key(|e| e.last)
            else {
                return false;
            };
            e.iccid.clone()
        };
        let changes = lock(&self.store).dismiss(kind, &iccid, now);
        self.apply(&changes, watch, now);
        !changes.is_empty()
    }

    fn apply(&self, changes: &[Change], watch: &Watch, now: u64) {
        if changes.is_empty() {
            return;
        }
        for c in changes {
            let (kind, data) = c.event();
            watch.push_event(record::Event::new(now, kind, data));
        }
        self.save();
        self.publish();
    }

    fn save(&self) {
        let text = lock(&self.store).to_value().to_string();
        let tmp = self.path.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }
}

#[cfg(test)]
#[path = "tests_finding.rs"]
mod tests;
