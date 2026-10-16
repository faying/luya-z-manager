//! Scenario engine decisions, moved from zte-agent `src/scenario.rs` (manager 20c3e35).
//!
//! The agent samples the surroundings (a Wi-Fi scan through a throwaway vdev, the SIM's MCC),
//! decides which scenario holds, and applies its actions on entry. This module is the deciding
//! part: the config types, `validate`, `detect`, `decide` (hysteresis), `scan_interval`, the
//! restore-on-exit bookkeeping (`restore_key`, `restore_action`, `due_restores`), the MCC rule,
//! the u60-guard takeover clock, and `step`, which is one tick of the agent's `Engine::tick`
//! after its inputs have been read. Applying (uci snapshots, `wifi_radio::apply`, routed HTTP
//! actions, rollback, the sleep daemon, the RTC alarm, files) stays in the host.
//!
//! Deliberate differences from the agent:
//! - Action bodies are [`Json`] instead of `serde_json::Value`; same text, same serde shape.
//! - `takeover_state` takes monotonic seconds (`f64`) instead of `Instant`.
//! - `scan_interval` takes `charging` instead of reading the charger itself.
//! - `step` returns what the tick decides instead of doing it; see its doc for what the host
//!   does at each point. The state change `switch_to` makes once `apply` has run is
//!   [`after_switch`], since only the host knows whether apply succeeded.
//! - The picker/description text (`describe_*`, `picker_*`, `name_en`) is presentation and
//!   stayed in the agent.

use std::collections::BTreeMap;

pub use crate::action::{Action, Json};

/// The scenario every failure and every boot falls back to.
pub const AWAY_ID: &str = "away";
/// While pinned there are no scans; failed restores are retried this often.
pub const PIN_RETRY_SECS: i64 = 60;
/// u60-guard's takeover is released after this much continuous running.
pub const TAKEOVER_HOLD_SECS: f64 = 600.0;
/// A scan that misses the current Wi-Fi-detected scenario is checked once more after this.
pub const MISS_RECHECK_DELAY_SECS: u64 = 3;
/// After this many consecutive scan failures, a scenario with Wi-Fi off is abandoned for away
/// (fail open: the device is the phone's only uplink).
pub const SCAN_FAILURES_BEFORE_FAIL_OPEN: u32 = 3;

/// Paths a scenario is allowed to drive. An allow-list: eSIM, reboot, factory reset and
/// everything about the cellular network itself are absent on purpose (see the agent).
/// `/api/device/thermal` was here until step 9h (10-09, the owner's decision): a read, not an
/// action, and u60d has no route for it; a config that still names it fails `validate`.
pub const ALLOWED_PATHS: &[&str] = &[
    "/api/wifi/radio",
    "/api/wifi/guest",
    "/api/device/power-save",
];

// ── configuration ───────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Params {
    pub scan_interval_home_secs: u64,
    pub scan_interval_away_battery_secs: u64,
    pub scan_interval_away_charging_secs: u64,
    /// Consecutive detections before entering a scenario.
    pub enter_hits: u32,
    /// Consecutive non-detections before leaving it.
    pub exit_misses: u32,
    /// A name-only network weaker than this does not count as "here".
    pub min_rssi_dbm: f64,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            scan_interval_home_secs: 60,
            scan_interval_away_battery_secs: 300,
            scan_interval_away_charging_secs: 60,
            enter_hits: 2,
            exit_misses: 2,
            min_rssi_dbm: -70.0,
        }
    }
}

/// One network that means "I am here".
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SsidEntry {
    pub ssid: String,
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub bssid: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "type", rename_all = "snake_case"))]
pub enum Detect {
    /// Any listed network nearby and strong enough.
    Ssid { entries: Vec<SsidEntry> },
    /// The SIM's home MCC (IMSI), never the serving cell's, is one of these.
    Mcc { mccs: Vec<String> },
    /// Any known MCC that is not `home_mcc`; checked after every `Mcc` scenario.
    Abroad,
    /// When nothing else matches.
    Fallback,
}

/// Read a value back after an action and compare it.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FieldCheck {
    pub path: String,
    pub pointer: String,
    pub equals: Json,
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ScenarioAction {
    #[cfg_attr(feature = "serde", serde(flatten))]
    pub action: Action,
    /// uci keys read before acting (rollback, and telling the engine's writes from the user's).
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Vec::is_empty")
    )]
    pub snapshot: Vec<String>,
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub verify_job: Option<String>,
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub verify_field: Option<FieldCheck>,
    /// A failure is logged and the scenario still counts as applied. Never on Wi-Fi.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "std::ops::Not::not")
    )]
    pub best_effort: bool,
    /// Remember the value this action replaces and put it back on leaving.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub restore_on_exit: Option<RestoreOnExit>,
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RestoreOnExit {
    pub read_path: String,
    pub read_pointer: String,
    /// The body field that carries the saved value when putting it back.
    pub field: String,
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Scenario {
    pub id: String,
    pub name: String,
    pub detect: Detect,
    #[cfg_attr(feature = "serde", serde(default))]
    pub actions: Vec<ScenarioAction>,
    /// Hold off the vendor sleep daemon while active (any scenario that turns the APs off).
    #[cfg_attr(feature = "serde", serde(default))]
    pub inhibit_sleep: bool,
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Config {
    pub version: u32,
    #[cfg_attr(feature = "serde", serde(default = "default_home_mcc"))]
    pub home_mcc: String,
    #[cfg_attr(feature = "serde", serde(default))]
    pub params: Params,
    #[cfg_attr(feature = "serde", serde(default))]
    pub scenarios: Vec<Scenario>,
}

impl Default for Config {
    /// No scenarios: an unconfigured engine does nothing at all.
    fn default() -> Self {
        Config {
            version: 1,
            home_mcc: default_home_mcc(),
            params: Params::default(),
            scenarios: Vec::new(),
        }
    }
}

impl Config {
    pub fn scenario(&self, id: &str) -> Option<&Scenario> {
        self.scenarios.iter().find(|s| s.id == id)
    }
}

fn default_home_mcc() -> String {
    "460".into()
}

pub const WIFI_KEYS: [&str; 2] = ["wireless.main_2g.disabled", "wireless.main_5g.disabled"];

pub fn wifi_action(on: bool) -> ScenarioAction {
    ScenarioAction {
        action: Action {
            method: "PUT".into(),
            path: "/api/wifi/radio".into(),
            body: Some(Json::obj([
                ("ap_2g", Json::from(on)),
                ("ap_5g", Json::from(on)),
            ])),
        },
        snapshot: WIFI_KEYS.iter().map(|k| k.to_string()).collect(),
        verify_job: None,
        verify_field: None,
        best_effort: false,
        restore_on_exit: None,
    }
}


/// The abroad scenario on its own, for adding to an existing configuration.
pub fn abroad_scenarios() -> Vec<Scenario> {
    vec![Scenario {
        id: "abroad".into(),
        name: "国外".into(),
        detect: Detect::Abroad,
        inhibit_sleep: false,
        actions: vec![wifi_action(true)],
    }]
}

/// The admin UI's starting point: no home networks filled in, so it never triggers until the
/// owner names one.
pub fn template() -> Config {
    let mut scenarios = vec![
        Scenario {
            id: "home".into(),
            name: "在家".into(),
            detect: Detect::Ssid {
                entries: Vec::new(),
            },
            inhibit_sleep: true,
            actions: vec![wifi_action(false)],
        },
        Scenario {
            id: AWAY_ID.into(),
            name: "外出".into(),
            detect: Detect::Fallback,
            inhibit_sleep: false,
            actions: vec![wifi_action(true)],
        },
    ];
    scenarios.extend(abroad_scenarios());
    Config {
        version: 1,
        home_mcc: default_home_mcc(),
        params: Params::default(),
        scenarios,
    }
}

fn is_mcc(v: &str) -> bool {
    v.len() == 3 && v.bytes().all(|b| b.is_ascii_digit())
}

pub fn validate(cfg: &Config) -> Result<(), String> {
    if cfg
        .scenarios
        .iter()
        .filter(|s| matches!(s.detect, Detect::Fallback))
        .count()
        > 1
    {
        return Err("at most one scenario may use detect.type = fallback".into());
    }
    if cfg
        .scenarios
        .iter()
        .filter(|s| matches!(s.detect, Detect::Abroad))
        .count()
        > 1
    {
        return Err("at most one scenario may use detect.type = abroad".into());
    }
    if !is_mcc(&cfg.home_mcc) {
        return Err(format!("home_mcc {:?} must be three digits", cfg.home_mcc));
    }
    let mut seen: Vec<&str> = Vec::new();
    for s in &cfg.scenarios {
        if s.id.trim().is_empty() {
            return Err("scenario id must not be empty".into());
        }
        if seen.contains(&s.id.as_str()) {
            return Err(format!("duplicate scenario id {:?}", s.id));
        }
        seen.push(&s.id);
        if let Detect::Mcc { mccs } = &s.detect {
            if mccs.is_empty() {
                return Err(format!("scenario {:?}: mccs must not be empty", s.id));
            }
            if let Some(bad) = mccs.iter().find(|m| !is_mcc(m)) {
                return Err(format!(
                    "scenario {:?}: MCC {bad:?} must be three digits",
                    s.id
                ));
            }
            if mccs.contains(&cfg.home_mcc) {
                return Err(format!(
                    "scenario {:?} lists the home MCC {}; it would fire at home",
                    s.id, cfg.home_mcc
                ));
            }
        }
        // Every failure lands on the fallback: it must never turn the APs off.
        if matches!(s.detect, Detect::Fallback)
            && s.actions.iter().any(|a| {
                a.action.path == "/api/wifi/radio"
                    && a.action.body.as_ref().is_some_and(|b| {
                        b.get("ap_2g") == Some(&Json::Bool(false))
                            || b.get("ap_5g") == Some(&Json::Bool(false))
                    })
            })
        {
            return Err(format!(
                "scenario {:?} is the fallback and must not turn Wi-Fi off",
                s.id
            ));
        }
        for a in &s.actions {
            if a.best_effort && a.action.path == "/api/wifi/radio" {
                return Err("best_effort is not allowed on /api/wifi/radio".into());
            }
            if !ALLOWED_PATHS.contains(&a.action.path.as_str()) {
                return Err(format!(
                    "path {:?} is not allowed in a scenario; permitted: {}",
                    a.action.path,
                    ALLOWED_PATHS.join(", ")
                ));
            }
            if !crate::action::is_method(&a.action.method) {
                return Err(format!("unsupported method {:?}", a.action.method));
            }
        }
    }
    if cfg.params.enter_hits == 0 || cfg.params.exit_misses == 0 {
        return Err("enter_hits and exit_misses must be at least 1".into());
    }
    Ok(())
}

// ── restore on exit ─────────────────────────────────────────────────────────

/// Something to undo once the device leaves the scenario that changed it (`restore.json`).
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PendingRestore {
    pub key: String,
    /// The original action with the saved value in its body.
    pub action: ScenarioAction,
    pub saved_at: i64,
    /// Not due while the next scenario is itself an abroad one.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "std::ops::Not::not")
    )]
    pub while_abroad: bool,
}

/// Scenarios that mean "outside the home country".
pub fn is_abroad(s: &Scenario) -> bool {
    matches!(s.detect, Detect::Mcc { .. } | Detect::Abroad)
}

/// The agent's `in_abroad_scenario`: whether a change the owner makes by hand now
/// (a service switched off, say) is recorded to be undone when home.
pub fn in_abroad_scenario(cfg: &Config, enabled: bool, current: &str) -> bool {
    enabled
        && cfg
            .scenarios
            .iter()
            .any(|s| s.id == current && is_abroad(s))
}

/// What an action manages, independent of the value it sets: its path plus its body minus the
/// restored field.
pub fn restore_key(a: &ScenarioAction) -> Option<String> {
    let r = a.restore_on_exit.as_ref()?;
    let mut body = a
        .action
        .body
        .clone()
        .unwrap_or(Json::Object(BTreeMap::new()));
    if let Some(obj) = body.as_object_mut() {
        obj.remove(&r.field);
    }
    Some(format!("{} {}", a.action.path, body))
}

/// The action that puts `saved` back.
pub fn restore_action(a: &ScenarioAction, saved: Json) -> Option<ScenarioAction> {
    let r = a.restore_on_exit.as_ref()?;
    let mut back = a.clone();
    let mut body = back
        .action
        .body
        .take()
        .unwrap_or(Json::Object(BTreeMap::new()));
    body.as_object_mut()?.insert(r.field.clone(), saved.clone());
    back.action.body = Some(body);
    back.restore_on_exit = None;
    back.best_effort = true;
    back.verify_field = back.verify_field.map(|c| FieldCheck { equals: saved, ..c });
    Some(back)
}

/// Pending restores the next scenario does not manage itself, in the order to run them.
/// `next = None` is the engine switched off (everything is due).
pub fn due_restores(pending: &[PendingRestore], next: Option<&Scenario>) -> Vec<String> {
    let owned: Vec<String> = next
        .map(|s| s.actions.iter().filter_map(restore_key).collect())
        .unwrap_or_default();
    let abroad_next = next.is_some_and(is_abroad);
        let due: Vec<String> = pending
        .iter()
        .filter(|p| !owned.contains(&p.key) && !(p.while_abroad && abroad_next))
        .map(|p| p.key.clone())
        .collect();
    due
}

// ── runtime state ───────────────────────────────────────────────────────────

/// The agent's `state.json`.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RunState {
    /// Scenario currently applied. Empty means "nothing applied yet".
    pub current: String,
    /// Scenario accumulating hits towards a switch.
    pub candidate: String,
    pub hits: u32,
    pub misses: u32,
    /// uci key -> the value the engine last wrote (kept by the host's apply).
    pub applied: BTreeMap<String, String>,
    pub last_switch: Option<i64>,
    pub last_error: Option<String>,
    /// Wall-clock second of the last completed scan.
    pub last_scan: i64,
    /// Consecutive scan failures. Reset on any successful scan.
    #[cfg_attr(feature = "serde", serde(default))]
    pub scan_failures: u32,
}

// ── SIM ─────────────────────────────────────────────────────────────────────

/// A fresh datad `sim` block is the answer, "unknown" included; without one, `direct` (the
/// host's `get_sim_info` read) decides.
pub fn sim_mcc_from(block: Option<Json>, direct: impl FnOnce() -> Option<Json>) -> Option<String> {
    match block {
        Some(sim) => mcc_from_datad_sim(&sim),
        None => mcc_from_sim_info(&direct()?),
    }
}

/// `zwrt_zte_mdm.api get_sim_info` as ubus returns it (`sim_states`, `sim_imsi`).
pub fn mcc_from_sim_info(info: &Json) -> Option<String> {
    mcc_from(str_field(info, "sim_states"), str_field(info, "sim_imsi"))
}

/// datad's `sim` block: the same two values, renamed (`state`, `imsi`).
pub fn mcc_from_datad_sim(sim: &Json) -> Option<String> {
    mcc_from(str_field(sim, "state"), str_field(sim, "imsi"))
}

fn str_field<'a>(v: &'a Json, k: &str) -> &'a str {
    v.get(k).and_then(Json::as_str).unwrap_or("")
}

/// A ready SIM with a plausible all-digit IMSI, else `None`.
pub fn mcc_from(state: &str, imsi: &str) -> Option<String> {
    if !state.contains("ready") || imsi.len() < 5 || !imsi.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(imsi[..3].to_string())
}

// ── takeover ────────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Takeover {
    /// No marker: u60-guard is not involved.
    None,
    /// Marker present and not yet held long enough: stay in away, ignore pin.
    Active,
    /// Held for the hold time: the host removes the marker and the tick goes on as usual.
    Release,
}

/// `seen` is the host's record of which marker (by content) it is honouring and since when
/// (monotonic seconds).
pub fn takeover_state(
    marker: Option<&str>,
    seen: &mut Option<(String, f64)>,
    now: f64,
    hold: f64,
) -> Takeover {
    let Some(content) = marker else {
        *seen = None;
        return Takeover::None;
    };
    match seen {
        Some((c, since)) if c == content => {
            if now - *since >= hold {
                *seen = None;
                Takeover::Release
            } else {
                Takeover::Active
            }
        }
        _ => {
            *seen = Some((content.to_string(), now));
            Takeover::Active
        }
    }
}

// ── detection ───────────────────────────────────────────────────────────────

/// One network from a scan (the agent's `wifi_scan::Network`).
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Network {
    pub ssid: String,
    pub bssid: String,
    /// dBm.
    pub signal: f64,
}

/// Which scenario the surroundings say we are in: named networks and specific MCCs first (in
/// configuration order), then the catch-all abroad, then the fallback.
pub fn detect(cfg: &Config, nets: &[Network], mcc: Option<&str>) -> Option<String> {
    for scen in &cfg.scenarios {
        let hit = match &scen.detect {
            Detect::Ssid { entries } => entries.iter().any(|e| {
                nets.iter().any(|n| {
                    n.ssid == e.ssid
                        && match &e.bssid {
                            Some(b) => n.bssid.eq_ignore_ascii_case(b.trim()),
                            None => n.signal >= cfg.params.min_rssi_dbm,
                        }
                })
            }),
            Detect::Mcc { mccs } => mcc.is_some_and(|m| mccs.iter().any(|x| x == m)),
            Detect::Abroad | Detect::Fallback => false,
        };
        if hit {
            return Some(scen.id.clone());
        }
    }
    if mcc.is_some_and(|m| m != cfg.home_mcc)
        && let Some(s) = cfg
            .scenarios
            .iter()
            .find(|s| matches!(s.detect, Detect::Abroad))
    {
        return Some(s.id.clone());
    }
    cfg.scenarios
        .iter()
        .find(|s| matches!(s.detect, Detect::Fallback))
        .map(|s| s.id.clone())
}

/// The Wi-Fi entries that detect scenario `id`, if it is detected by Wi-Fi.
pub fn ssid_entries<'a>(cfg: &'a Config, id: &str) -> Option<&'a [SsidEntry]> {
    cfg.scenario(id).and_then(|s| match &s.detect {
        Detect::Ssid { entries } if !entries.is_empty() => Some(entries.as_slice()),
        _ => None,
    })
}

/// The log line on a miss: how much the scans saw, and each entry's reading. `first` is the
/// size of the first scan (`nets` holds both scans).
pub fn miss_summary(
    entries: &[SsidEntry],
    nets: &[Network],
    first: usize,
    min_rssi: f64,
) -> String {
    let parts: Vec<String> = entries
        .iter()
        .map(|e| {
            let best = nets
                .iter()
                .filter(|n| match &e.bssid {
                    Some(b) => n.bssid.eq_ignore_ascii_case(b.trim()),
                    None => n.ssid == e.ssid,
                })
                .map(|n| n.signal)
                .fold(None, |m: Option<f64>, x| Some(m.map_or(x, |m| m.max(x))));
            let what = match &e.bssid {
                Some(b) => format!("{} {}", e.ssid, b.trim()),
                None => e.ssid.clone(),
            };
            match best {
                Some(sig) if e.bssid.is_none() && sig < min_rssi => {
                    format!("{what} {sig:.0} dBm (below {min_rssi:.0})")
                }
                Some(sig) => format!("{what} {sig:.0} dBm"),
                None => format!("{what} absent"),
            }
        })
        .collect();
    format!(
        "scans saw {first}+{} networks; {}",
        nets.len() - first,
        parts.join(", ")
    )
}

/// Hysteresis. Returns the scenario to switch to, or `None` to stay put. Entering and leaving
/// are counted separately: a single stray reading in either direction must not move the device.
pub fn decide(st: &mut RunState, detected: &str, p: &Params) -> Option<String> {
    if detected == st.current {
        st.misses = 0;
        st.hits = 0;
        st.candidate.clear();
        return None;
    }
    if st.candidate == detected {
        st.hits += 1;
        st.misses += 1;
    } else {
        st.candidate = detected.to_string();
        st.hits = 1;
        st.misses = 1;
    }
    // Nothing applied yet (fresh boot): only the entry threshold applies.
    let leaving_ok = st.current.is_empty() || st.misses >= p.exit_misses;
    if st.hits >= p.enter_hits && leaving_ok {
        Some(detected.to_string())
    } else {
        None
    }
}

/// Seconds between scans (the agent's `Engine::scan_interval`). `charging` is only looked at
/// on the away-on-battery/charger branch.
pub fn scan_interval(cfg: &Config, current: &str, candidate: &str, charging: bool) -> u64 {
    // APs down and nobody connected (what inhibit_sleep marks): scanning is free.
    if cfg
        .scenarios
        .iter()
        .any(|s| s.id == current && s.inhibit_sleep)
    {
        return cfg.params.scan_interval_home_secs;
    }
    // Nothing decided yet, or a switch in the making: confirm quickly.
    if current.is_empty() || !candidate.is_empty() {
        return cfg.params.scan_interval_away_charging_secs;
    }
    if charging {
        cfg.params.scan_interval_away_charging_secs
    } else {
        cfg.params.scan_interval_away_battery_secs
    }
}

// ── one tick ────────────────────────────────────────────────────────────────

/// What the host's scan returned. `at` is the wall-clock second the agent stamps into
/// `last_scan` (its `now()` right after the scan).
#[derive(Clone, Copy, Debug)]
pub enum Scan<'a> {
    /// The host has not scanned (yet).
    NotScanned,
    Failed {
        at: i64,
        error: &'a str,
    },
    Networks {
        at: i64,
        networks: &'a [Network],
    },
}

/// The second look when the first scan missed the current Wi-Fi-detected scenario, taken
/// [`MISS_RECHECK_DELAY_SECS`] later. A failed recheck scan adds no networks: pass an empty
/// slice (the agent ignores the error). `at` replaces the first scan's time in `last_scan`.
#[derive(Clone, Copy, Debug)]
pub struct Recheck<'a> {
    pub at: i64,
    pub networks: &'a [Network],
}

/// Everything one tick looks at.
#[derive(Clone, Copy, Debug)]
pub struct TickInput<'a> {
    pub cfg: &'a Config,
    pub state: &'a RunState,
    /// No `/data/scenario/disabled`.
    pub enabled: bool,
    /// `takeover_state(..) == Takeover::Active`, evaluated by the host first (on `Release` it
    /// removes the marker and passes `false`).
    pub takeover: bool,
    /// `/data/scenario/pin`, trimmed, `None` when empty.
    pub pin: Option<&'a str>,
    /// Wall-clock seconds at the start of the tick (same clock as `last_scan`; real seconds
    /// keep passing across suspend, which a sleep counter would not).
    pub now: i64,
    /// Last restore retry while pinned (wall-clock seconds; the agent's `pin_retry_at`).
    pub pin_retry_at: i64,
    /// Charger connected (only read for the away scan interval).
    pub charging: bool,
    pub scan: Scan<'a>,
    pub recheck: Option<Recheck<'a>>,
    /// The SIM's MCC (`sim_mcc_from`), read after a successful scan.
    pub mcc: Option<&'a str>,
    /// `wifi_radio::beaconing()`; only matters for fail-open after scan failures.
    pub beaconing: bool,
}

/// Which way through the tick it went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Branch {
    Takeover,
    Disabled,
    Unconfigured,
    Pinned,
    NotDue,
    /// A scan is due (with `Scan::NotScanned`: the host should scan now).
    Due,
    ScanFailed,
    Scanned,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Intent {
    None,
    /// The host runs `switch_to(to)`: restores due for `to`, sleep defences, apply, then
    /// [`after_switch`] with apply's result.
    Switch {
        to: String,
    },
    /// Disabled while in a scenario other than away: the host runs `bootsafe` (sleep allowed,
    /// RTC alarm cleared, Wi-Fi repaired if down); the state is already reset in the outcome.
    Bootsafe,
    /// A scan is due: the host scans and calls `step` again with the result.
    NeedScan,
    /// The scan missed the current Wi-Fi-detected scenario: the host waits
    /// [`MISS_RECHECK_DELAY_SECS`], scans again and calls `step` again with `recheck`.
    NeedRecheck,
}

/// Restores the host runs this tick, quietly (failures not logged), for `next` (`None` = all).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Restores {
    pub next: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TickOutcome {
    pub branch: Branch,
    /// What the tick writes to `state.json` before any switch.
    pub state: RunState,
    pub intent: Intent,
    /// Pending restores to run (before the scan on the scan path).
    pub restores: Option<Restores>,
    /// The current scenario inhibits sleep and no scan is due: the host refreshes the sleep
    /// defences (auto sleep off, RTC alarm at home interval + 30 s).
    pub keep_awake: bool,
    pub pin_retry_at: i64,
}

/// One tick of the agent's `Engine::tick` (its busy flag, file reads and logging stay in the
/// host). Called with `Scan::NotScanned` it answers up to the scan; called with the scan (and
/// the recheck, when asked for) it describes the whole tick, restores included.
pub fn step(i: &TickInput) -> TickOutcome {
    let cfg = i.cfg;
    let st = i.state;
    let mut out = TickOutcome {
        branch: Branch::Takeover,
        state: st.clone(),
        intent: Intent::None,
        restores: None,
        keep_awake: false,
        pin_retry_at: i.pin_retry_at,
    };
    let switch = |to: &str| {
        // switch_to does nothing for an id that is not in the config
        if cfg.scenario(to).is_some() {
            Intent::Switch { to: to.to_string() }
        } else {
            Intent::None
        }
    };

    // u60-guard's Wi-Fi-on wins over detection and the pin until released.
    if i.takeover {
        if i.enabled && !cfg.scenarios.is_empty() && st.current != AWAY_ID {
            out.intent = switch(AWAY_ID);
        }
        return out;
    }

    if !i.enabled {
        out.branch = Branch::Disabled;
        // The off switch is an escape hatch: back to away, then put back what scenarios changed.
        if !st.current.is_empty() && st.current != AWAY_ID {
            out.intent = Intent::Bootsafe;
            out.state = RunState::default();
        }
        out.restores = Some(Restores { next: None });
        return out;
    }
    if cfg.scenarios.is_empty() {
        out.branch = Branch::Unconfigured;
        return out;
    }

    let current = st.current.as_str();
    let current_known = cfg.scenario(current).map(|s| s.id.clone());

    if let Some(pinned) = i.pin {
        out.branch = Branch::Pinned;
        if pinned != current {
            out.intent = switch(pinned);
        } else if i.now - i.pin_retry_at >= PIN_RETRY_SECS {
            out.pin_retry_at = i.now;
            out.restores = Some(Restores {
                next: current_known,
            });
        }
        return out;
    }

    if i.now - st.last_scan < scan_interval(cfg, current, &st.candidate, i.charging) as i64 {
        out.branch = Branch::NotDue;
        out.keep_awake = cfg
            .scenarios
            .iter()
            .any(|s| s.id == current && s.inhibit_sleep);
        return out;
    }

    // Failed restores get another go each scan, but not before the first decision after boot.
    out.branch = Branch::Due;
    if !current.is_empty() {
        out.restores = Some(Restores {
            next: current_known,
        });
    }

    let nets = match i.scan {
        Scan::NotScanned => {
            out.intent = Intent::NeedScan;
            return out;
        }
        Scan::Failed { at, error } => {
            out.branch = Branch::ScanFailed;
            out.state.last_scan = at;
            out.state.scan_failures += 1;
            out.state.last_error = Some(format!("scan: {error}"));
            // Fail open: cannot see where we are and the APs are down, so "not home".
            if out.state.scan_failures >= SCAN_FAILURES_BEFORE_FAIL_OPEN
                && current != AWAY_ID
                && !i.beaconing
            {
                out.intent = switch(AWAY_ID);
            }
            return out;
        }
        Scan::Networks { at, networks } => {
            out.branch = Branch::Scanned;
            out.state.last_scan = at;
            networks
        }
    };

    let detect_any = |n: &[Network]| detect(cfg, n, i.mcc).unwrap_or_else(|| AWAY_ID.to_string());
    let mut detected = detect_any(nets);
    // Leaving a Wi-Fi-detected scenario rests on not seeing something; a partial scan does not
    // see it either. Look again before believing it.
    if detected != current && ssid_entries(cfg, current).is_some() {
        let Some(r) = i.recheck else {
            out.state.last_scan = st.last_scan;
            out.intent = Intent::NeedRecheck;
            return out;
        };
        let all: Vec<Network> = nets.iter().chain(r.networks).cloned().collect();
        detected = detect_any(&all);
        out.state.last_scan = r.at;
    }

    out.state.scan_failures = 0;
    if let Some(id) = decide(&mut out.state, &detected, &cfg.params) {
        out.intent = switch(&id);
    }
    out
}

/// The state `switch_to` leaves once the host's apply has run (`result`), for a scenario in the
/// config (`switch_to` ignores other ids, and so does this). On failure the host has already
/// rolled back and lands on away.
pub fn after_switch(
    st: &RunState,
    cfg: &Config,
    id: &str,
    now: i64,
    result: Result<(), &str>,
) -> RunState {
    let mut st = st.clone();
    if cfg.scenario(id).is_none() {
        return st;
    }
    match result {
        Ok(()) => {
            st.current = id.to_string();
            st.last_switch = Some(now);
            st.last_error = None;
        }
        Err(e) => {
            st.current = AWAY_ID.to_string();
            st.last_error = Some(e.to_string());
        }
    }
    st.candidate.clear();
    st.hits = 0;
    st.misses = 0;
    st
}

#[cfg(test)]
mod tests;
