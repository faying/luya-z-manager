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
//! IPv4/IPv6 addresses), the sim block's `iccid`, and the home screen's `story.state` (no SIM,
//! airplane). It probes only when the passive reading has said down for [`DOWN_HOLD_SECS`] in a
//! row, a client is connected, and the last probing check is [`RECHECK_SECS`] ago (design:
//! rechecks at most every 10 minutes).

use serde_json::Value;
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use luyaz_api::clock::BootTime;
use luyaz_api::diagnose::Gate;
use luyaz_api::finding::{DataIn, NetworkEvidence, Premises, Reg, Verdict};
use luyaz_netwatch::finding::store::{Change, Store};
use luyaz_netwatch::finding::{self as nf, FindingHost};
use luyaz_netwatch::{Watch, record};

/// The store between restarts (see the module doc).
pub const FILE: &str = "/data/luyaz/core/findings.json";
/// Sampler rounds (5 s) between checks.
pub const EVERY_ROUNDS: u32 = 6;
pub const RECHECK_SECS: u64 = 600;
pub(crate) mod budget;
pub(crate) mod sampling;
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
    /// first seen, unix seconds (the underlay gate's hold limit; not shown)
    pub first: u64,
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
    pub observed: Option<BootTime>,
    pub root: String,
    pub network: Mutex<Option<NetworkEvidence>>,
    pub current_connection: Arc<dyn Fn() -> bool + Send + Sync>,
    pub reserve_probe: Arc<dyn Fn() -> bool + Send + Sync>,
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

impl Inputs {
    fn fresh(&self) -> bool {
        self.observed
            .as_ref()
            .zip(BootTime::read().as_ref())
            .and_then(|(then, now)| now.elapsed_since(then))
            .is_some_and(|age| age <= nf::path::MAX_AGE)
    }
}

impl FindingHost for Inputs {
    fn premises(&self) -> Premises {
        if !self.fresh() {
            return Premises::default();
        }
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
        self.readings_current() && self.clients && self.probe_due && self.held_down
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
        if !self.fresh() {
            return DataIn::default();
        }
        let cell = cellular(self.state.as_ref());
        let s = |k: &str| cell.and_then(|c| c[k].as_str()).map(str::to_string);
        DataIn {
            connect_status: s("connect_status"),
            fail_count: cell.and_then(|c| c["connect_fail_count"].as_i64()),
            has_ip: self.state.as_ref().and_then(nf::path::has_ip),
            pdp_active: None,
            ceer: None,
        }
    }

    fn readings_current(&self) -> bool {
        self.fresh() && (self.current_connection)()
    }

    fn network_probe(&self) -> NetworkEvidence {
        self.network_probe_with(&nf::path::BoundSockets)
    }
}

/// Recheck identity/freshness before each request. No fallback once a connection changes.
struct Guarded<'a> {
    inputs: &'a Inputs,
    io: &'a dyn nf::path::Transport,
    invalid: AtomicBool,
}
impl Guarded<'_> {
    fn current(&self) -> bool {
        if self.invalid.load(Ordering::SeqCst) {
            return false;
        }
        if !self.inputs.readings_current() {
            self.invalid.store(true, Ordering::SeqCst);
            return false;
        }
        true
    }
}
impl nf::path::Transport for Guarded<'_> {
    fn http(
        &self,
        path: &nf::path::Path,
        target: std::net::SocketAddr,
    ) -> luyaz_api::finding::ResultState {
        if self.current() {
            self.io.http(path, target)
        } else {
            luyaz_api::finding::ResultState::Unknown
        }
    }
    fn dns(
        &self,
        path: &nf::path::Path,
        target: std::net::SocketAddr,
    ) -> luyaz_api::finding::ResultState {
        if self.current() {
            self.io.dns(path, target)
        } else {
            luyaz_api::finding::ResultState::Unknown
        }
    }
}

impl Inputs {
    fn network_probe_with(&self, io: &dyn nf::path::Transport) -> NetworkEvidence {
        let unknown = |reason: &str| NetworkEvidence {
            reason: reason.into(),
            observed_at: self.observed.clone(),
            ..NetworkEvidence::default()
        };
        let n = if !self.readings_current() {
            unknown("snapshot_or_connection_changed")
        } else if !(self.reserve_probe)() {
            unknown("probe_budget_unavailable")
        } else {
            // Reservation is durable before any packet; panic/crash also consumes the budget.
            self.probed.store(true, Ordering::SeqCst);
            let paths = self.state.as_ref().map(nf::path::paths).unwrap_or_default();
            let redirected = redirected(&self.root);
            let guarded = Guarded {
                inputs: self,
                io,
                invalid: AtomicBool::new(false),
            };
            let mut n = nf::path::observe(
                &paths,
                self.observed.clone(),
                BootTime::read(),
                redirected,
                &guarded,
            );
            n.observed_at = self.observed.clone();
            if !guarded.current() {
                n = unknown("snapshot_or_connection_changed_during_probe");
            }
            n
        };
        *lock(&self.network) = Some(n.clone());
        n
    }
}

/// CHILL may redirect the device's own traffic: anything but a readable "stopped" (or no state
/// file at all) counts as redirected. Shared with the deep diagnosis' path probe.
pub(super) fn redirected(root: &str) -> bool {
    match std::fs::read_to_string(format!("{root}{}", luyaz_api::chill::CHILL_STATE)) {
        Ok(s) => serde_json::from_str::<Value>(&s)
            .ok()
            .is_none_or(|v| v["state"] != "stopped"),
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    }
}

/// This load's random identity for the store (decision-execution-loop §4.1 D2); unreadable =
/// "" = nothing executable.
fn new_epoch() -> String {
    std::fs::read_to_string("/proc/sys/kernel/random/uuid")
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
}

/// The open findings and the suppressed table, and when the last probing check ran.
pub struct Findings {
    store: Mutex<Store>,
    path: PathBuf,
    budget: Mutex<budget::Budget>,
    passive: Mutex<sampling::Passive>,
    network: Mutex<Option<StoredNetwork>>,
}

struct StoredNetwork {
    evidence: NetworkEvidence,
    current: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl Findings {
    /// The store from `path` (missing or unreadable: empty).
    pub fn load(path: PathBuf) -> Findings {
        let store = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .map(|v| Store::from_value(&v))
            .unwrap_or_default()
            .with_epoch(new_epoch());
        let f = Findings {
            store: Mutex::new(store),
            path: path.clone(),
            budget: Mutex::new(budget::Budget::load(
                path.with_extension("probe-budget.json"),
                BootTime::read().as_ref(),
            )),
            passive: Mutex::new(sampling::Passive::default()),
            network: Mutex::new(None),
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
            // Old unbound HTTP findings must not reappear as a sure internet diagnosis.
            .filter(|e| {
                !matches!(e.finding["layer"].as_str(), Some("internet" | "dns"))
                    || e.finding["evidence"].as_array().is_some_and(|ev| {
                        ev.iter()
                            .any(|v| v["network"]["internet_usable"] == "passed")
                    })
            })
            .max_by_key(|e| e.last)?;
        let f = &e.finding;
        let t = |k: &str| f[k].as_str().unwrap_or("").to_string();
        Some(Home {
            kind: e.kind.clone(),
            layer: t("layer"),
            symptom: t("symptom"),
            symptom_en: t("symptom_en"),
            first: e.first,
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
        if matches!(f["layer"].as_str(), Some("internet" | "dns"))
            && !f["evidence"]
                .as_array()
                .is_some_and(|ev| ev.iter().any(|v| v.get("network").is_some()))
        {
            f["confidence"] = "guess".into();
            f["verification"] = "legacy_path_unverified".into();
            f["symptom"] = "旧版探测的蜂窝出口未验证，需重新检查".into();
            f["symptom_en"] = "Legacy probe cellular path unverified; recheck needed".into();
        }
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

    /// The latest observation while it is fresh and its connection current (what
    /// [`Findings::network_view`] marks `fresh`): the deep diagnosis reuses it (deep-diag-v2 §2.1).
    pub fn fresh_network(&self) -> Option<NetworkEvidence> {
        let stored = lock(&self.network);
        let stored = stored.as_ref()?;
        let n = &stored.evidence;
        let fresh = n
            .observed_at
            .as_ref()
            .zip(BootTime::read().as_ref())
            .and_then(|(a, b)| b.elapsed_since(a))
            .is_some_and(|age| age <= nf::path::MAX_AGE);
        (fresh && (stored.current)()).then(|| n.clone())
    }

    /// Latest scoped observation, with elapsed freshness checked at read time.
    pub fn network_view(&self) -> Value {
        let stored = lock(&self.network);
        let Some(stored) = stored.as_ref() else {
            return Value::Null;
        };
        let n = &stored.evidence;
        let fresh = n
            .observed_at
            .as_ref()
            .zip(BootTime::read().as_ref())
            .and_then(|(a, b)| b.elapsed_since(a))
            .is_some_and(|age| age <= nf::path::MAX_AGE);
        let fresh = fresh && (stored.current)();
        serde_json::json!({"fresh": fresh, "observation": n})
    }

    pub fn probe_due(&self, now: Option<&BootTime>) -> bool {
        lock(&self.budget).due(now)
    }

    pub fn reserve_probe(&self, now: Option<&BootTime>) -> bool {
        lock(&self.budget).reserve(now)
    }

    pub fn sample_passive(&self, readings: Option<&sampling::Readings>, now: Option<&BootTime>) {
        lock(&self.passive).sample(readings, now);
    }

    /// [`sampling::Passive::down_secs`].
    pub fn down_secs(
        &self,
        readings: Option<&sampling::Readings>,
        now: Option<&BootTime>,
    ) -> Option<u64> {
        lock(&self.passive).down_secs(readings, now)
    }

    /// The open finding's binding (store epoch + revision), or None.
    pub fn identity(
        &self,
        kind: &str,
        iccid: &str,
    ) -> Option<luyaz_netwatch::finding::store::Identity> {
        lock(&self.store).identity(kind, iccid)
    }

    /// The decision loop's re-read under the engine lock (memory only).
    #[allow(dead_code)] // `propose::now_for`
    pub fn snapshot(&self, kind: &str, iccid: &str) -> luyaz_api::execution::FindingSnapshot {
        match self.identity(kind, iccid) {
            Some(id) => luyaz_api::execution::FindingSnapshot {
                open: true,
                epoch: Some(id.epoch),
                revision: Some(id.revision),
            },
            None => luyaz_api::execution::FindingSnapshot::default(),
        }
    }

    pub fn passive_admission(
        &self,
        readings: Option<&sampling::Readings>,
        now: Option<&BootTime>,
    ) -> (bool, bool) {
        lock(&self.passive).admission(readings, now)
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
        if let Some(n) = lock(&i.network).clone() {
            *lock(&self.network) = Some(StoredNetwork {
                evidence: n,
                current: i.current_connection.clone(),
            });
        }
        if !i.readings_current() {
            // Even Unverified carries a card to Store::observe; a stale card must not expire
            // findings belonging to the new connection. Do not mutate the store at all.
            return Some(Verdict::Unverified {
                why: "snapshot_or_connection_changed",
            });
        }
        let boot = BootTime::read();
        self.observe_at(
            &v,
            &i.iccid,
            watch,
            now,
            boot.as_ref().or(i.observed.as_ref()),
        );
        Some(v)
    }

    #[cfg(test)]
    pub(super) fn observe(&self, v: &Verdict, iccid: &str, watch: &Watch, now: u64) {
        let boot = BootTime::read();
        self.observe_at(v, iccid, watch, now, boot.as_ref());
    }

    pub(super) fn observe_at(
        &self,
        v: &Verdict,
        iccid: &str,
        watch: &Watch,
        now: u64,
        boot: Option<&BootTime>,
    ) {
        let changes = lock(&self.store).observe_at(v, iccid, now, boot);
        self.apply(&changes, watch, now);
        // the confidence of an open one can change without a transition
        self.publish();
    }

    /// The user's "ignore" on what [`Findings::view`] shows (the web, T6): the open finding of
    /// `kind` seen most recently, checked against the ICCID's last 4 digits when given (the
    /// view never carries the whole ICCID). False: nothing open matches.
    pub fn dismiss_shown(&self, kind: &str, tail: Option<&str>, watch: &Watch, now: u64) -> bool {
        let boot = BootTime::read();
        self.dismiss_shown_at(kind, tail, watch, now, boot.as_ref())
    }

    pub fn dismiss_shown_at(
        &self,
        kind: &str,
        tail: Option<&str>,
        watch: &Watch,
        now: u64,
        boot: Option<&BootTime>,
    ) -> bool {
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
        let changes = lock(&self.store).dismiss_at(kind, &iccid, now, boot);
        self.apply(&changes, watch, now);
        !changes.is_empty()
    }

    /// The decision loop proved its repair (confirmed + bound HTTPS recheck passed): close the
    /// finding it was proposed for now instead of waiting for the passive ups. True: closed.
    pub fn resolve(&self, kind: &str, iccid: &str, epoch: &str, watch: &Watch, now: u64) -> bool {
        let changes = lock(&self.store).resolve(kind, iccid, epoch);
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
