// ─────────────────────────────────────────────────────────────────────────────
// scenario — "where am I, and what should the device look like here".
//
// A scenario is a detection rule plus an ordered list of actions. The engine
// samples the surroundings, decides which scenario holds, and applies that
// scenario's actions once on entry. `home` (Wi-Fi off, the phone roams onto the
// house network), `away` (the catch-all), and `abroad` (a foreign SIM in the
// slot: CHILL's main group goes DIRECT, and whatever it was before comes back
// when you return — see `restore_on_exit`).
//
// Design notes that are easy to get wrong and expensive to rediscover:
//
//  * **The device is someone's only internet connection.** Every failure path
//    ends at the away scenario with Wi-Fi on. Boot resets to away. An empty or
//    unreadable config means "do nothing", never "apply something".
//  * **A wall clock, not a sleep counter.** In the home scenario the device is
//    allowed to suspend; `thread::sleep` does not advance across suspend, so
//    counting ticks would silently stop time. Elapsed real seconds decide when
//    a scan is due.
//  * **The detector never writes configuration.** It scans through a throwaway
//    vdev (see wifi_scan). That matters for the override latch below: if the
//    detector changed uci, the engine would see its own edits and conclude the
//    user had taken over.
//  * **HTTP 200 does not mean the device changed** (see action.rs). Wireless
//    actions therefore go through `wifi_radio::apply`, which polls until the
//    change is observable, instead of through the route table.
//  * **Never call `server::route` while holding a lock a handler could want.**
//    `wifi_radio::apply` takes the Wi-Fi lock itself, so the engine must not
//    already hold it — hence the direct call rather than a routed one. Code
//    that does hold it (rollback) calls `apply_locked`.
// ─────────────────────────────────────────────────────────────────────────────

use std::collections::BTreeMap;
use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::action::Action;
use crate::handlers::AppState;
use crate::ubus;
use crate::wifi_scan;

const DIR: &str = "/data/scenario";
const CONFIG_FILE: &str = "/data/scenario/scenarios.json";
const STATE_FILE: &str = "/data/scenario/state.json";
const PIN_FILE: &str = "/data/scenario/pin";
const DISABLED_FLAG: &str = "/data/scenario/disabled";
const LOG_FILE: &str = "/data/scenario/log";
/// Values to put back on leaving a scenario. Kept apart from `state.json`
/// because boot clears that, and a reboot abroad must not turn "what it was
/// before the trip" into "what it is now".
const RESTORE_FILE: &str = "/data/scenario/restore.json";
const LOG_MAX_BYTES: u64 = 1_048_576;

// ── Contract with u60-guard (touch-ui scripts/u60-guard.sh) ─────────────────
//
// u60-guard is the watchdog that forces Wi-Fi on when this agent stops
// working while the APs are down. It knows the agent only through these two
// files, both on tmpfs so a reboot starts clean:
//
//  * HEARTBEAT_FILE — written by the engine thread before bootsafe and then on
//    every loop, whether or not the engine is enabled or configured: it proves
//    the agent is alive, not that it is switching. Content is whole seconds of
//    /proc/uptime (monotonic, immune to the clock jump when the network sets
//    the time). u60-guard only starts judging once it has seen it at least once.
//  * TAKEOVER_FILE — written by u60-guard when it steps in. While it exists the
//    engine does no detection and ignores the pin, and goes back to (and stays
//    in) away: a persistent pin of "home" would otherwise take the APs straight
//    back down. The engine removes it after TAKEOVER_HOLD of its own continuous
//    running, which is how control is handed back. If u60-guard rewrites it
//    (a second takeover), the clock starts again.
const HEARTBEAT_FILE: &str = "/tmp/scenario.heartbeat";
const TAKEOVER_FILE: &str = "/tmp/u60-wifiguard.took-over";
const TAKEOVER_HOLD: Duration = Duration::from_secs(600);
// Which is why u60-guard must write the marker ONCE per takeover, with content
// unique to that takeover (its uptime at the time) — rewriting it every check
// would restart the hold forever and control would never come back.

/// Set by the procd service script. Tells the agent a supervisor will restart
/// it, so exiting is the right response to a dead engine thread.
const SUPERVISED_ENV: &str = "ZTE_AGENT_SUPERVISED";
const LOG_TAIL_LINES: usize = 300;

/// The scenario every failure and every boot falls back to.
const AWAY_ID: &str = "away";

/// How often the engine wakes to *consider* working. The decision to actually
/// scan is made against the wall clock, so this only bounds responsiveness.
const TICK_SECS: u64 = 15;

/// Vendor sleep daemon. Writing `/sys/power/wake_lock` does NOT hold this
/// device awake — the daemon forces suspend with an explicit `echo mem >
/// /sys/power/state` that ignores the kernel wakelock set. Its own ubus switch
/// is the only thing it honours.
const SLEEP_UBUS_OBJ: &str = "zwrt_zte_sleep_faw.wakelock";
const RTC_WAKEALARM: &str = "/sys/class/rtc/rtc0/wakealarm";
const RTC_SINCE_EPOCH: &str = "/sys/class/rtc/rtc0/since_epoch";

/// Paths a scenario is allowed to drive.
///
/// An allow-list, not a deny-list. The scheduler's `validate_job` blocks only
/// `/api/scheduler/` and `/api/auth/`, which leaves `/api/device/reboot`,
/// `/api/device/factory-reset`, `/api/system/kill-bloat` and the whole eSIM
/// surface reachable. That is tolerable for a job a human typed; it is not
/// tolerable for something that fires by itself when the surroundings change.
///
/// eSIM is deliberately absent: `esim.rs` reboots the device when a profile
/// switch does not converge, and this is the phone's only uplink.
const ALLOWED_PATHS: &[&str] = &[
    "/api/wifi/radio",
    "/api/wifi/guest",
    "/api/services/chill/enable",
    "/api/services/chill/disable",
    "/api/services/chill/regions",
    "/api/services/chill/bypass",
    "/api/services/chill/exit",
    "/api/device/power-save",
    "/api/device/thermal",
    "/api/router/apn/profiles/activate",
];

// ── configuration ───────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone)]
pub struct Params {
    /// Home: the APs are down and nothing is connected, so scanning is free.
    pub scan_interval_home_secs: u64,
    /// Away on battery: each scan briefly occupies the radios, which a
    /// connected phone can feel. Deliberately slack.
    pub scan_interval_away_battery_secs: u64,
    /// Away on charger: plugged in usually means parked somewhere, so it is
    /// worth looking more often.
    pub scan_interval_away_charging_secs: u64,
    /// Consecutive detections before entering a scenario.
    pub enter_hits: u32,
    /// Consecutive non-detections before leaving it.
    pub exit_misses: u32,
    /// A network weaker than this does not count as "here", which is the cheap
    /// half of not being fooled by someone broadcasting the same name.
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
#[derive(Serialize, Deserialize, Clone)]
pub struct SsidEntry {
    pub ssid: String,
    /// Optional but strongly recommended. Name alone is trivially spoofed, and
    /// a false "home" reading turns the phone's only uplink off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bssid: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Detect {
    /// Matches when any listed network is nearby and strong enough.
    Ssid { entries: Vec<SsidEntry> },
    /// Matches when the SIM in the slot belongs to one of these MCCs.
    ///
    /// The SIM's *home* MCC (first three digits of the IMSI), never the serving
    /// cell's. A Chinese SIM roaming abroad therefore does not trigger this —
    /// only a local SIM or eUICC profile does. That is deliberate: judged by the
    /// serving cell, any action that changed the SIM would leave the device
    /// convinced it was still abroad after coming home.
    Mcc { mccs: Vec<String> },
    /// Matches any SIM whose MCC is known and is not `home_mcc`. Checked after
    /// every `Mcc` scenario, so it only catches countries nothing else maps.
    Abroad,
    /// Matches when nothing else does. Exactly one scenario should use this.
    Fallback,
}

/// Read a value back after an action and compare it. For actions that answer
/// synchronously but whose effect lives somewhere else (a CHILL region switch
/// is a passthrough to mihomo), so the HTTP status proves nothing.
#[derive(Serialize, Deserialize, Clone)]
pub struct FieldCheck {
    /// GET path, routed in-process.
    pub path: String,
    /// JSON pointer into the response, e.g. `/data/region/active`.
    pub pointer: String,
    pub equals: Value,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ScenarioAction {
    #[serde(flatten)]
    pub action: Action,
    /// uci keys to read before acting, so a later failure can put them back,
    /// and so the engine can tell its own writes from the user's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub snapshot: Vec<String>,
    /// Poll this job endpoint until it leaves `running`. For CHILL and eSIM,
    /// which answer 200 immediately and finish on a worker thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_job: Option<String>,
    /// Poll a GET until a field holds the expected value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_field: Option<FieldCheck>,
    /// A failure is logged and the scenario still counts as applied — no
    /// rollback, no fall back to away. For extras like the CHILL region, which
    /// fails whenever the user has CHILL stopped: without this, every retry
    /// would roll back, and rollback forces Wi-Fi through a full reload.
    /// Never allowed on `/api/wifi/radio` (see `validate`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub best_effort: bool,
    /// Remember the value this action is about to replace, and put it back
    /// when the device leaves for a scenario that does not manage it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore_on_exit: Option<RestoreOnExit>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct RestoreOnExit {
    /// GET path (routed in-process) that reports the value before the change.
    pub read_path: String,
    pub read_pointer: String,
    /// The body field that carries the saved value when putting it back.
    pub field: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Scenario {
    pub id: String,
    pub name: String,
    pub detect: Detect,
    #[serde(default)]
    pub actions: Vec<ScenarioAction>,
    /// Hold off the vendor sleep daemon while this scenario is active. Required
    /// for any scenario that turns the APs off: the device would otherwise
    /// suspend, the engine would stop running, and Wi-Fi would never come back
    /// when you left. Also what keeps Tailscale reachable, which is the only
    /// way in once the APs are down.
    #[serde(default)]
    pub inhibit_sleep: bool,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    pub version: u32,
    /// The MCC that means "not abroad". Only `Detect::Abroad` reads it.
    #[serde(default = "default_home_mcc")]
    pub home_mcc: String,
    #[serde(default)]
    pub params: Params,
    #[serde(default)]
    pub scenarios: Vec<Scenario>,
}

impl Default for Config {
    /// No scenarios. An unconfigured engine must do nothing at all — the
    /// alternative is shipping a default that turns someone's Wi-Fi off based
    /// on network names they never chose.
    fn default() -> Self {
        Config {
            version: 1,
            home_mcc: default_home_mcc(),
            params: Params::default(),
            scenarios: Vec::new(),
        }
    }
}

fn default_home_mcc() -> String {
    "460".into()
}

const WIFI_KEYS: [&str; 2] = ["wireless.main_2g.disabled", "wireless.main_5g.disabled"];

fn wifi_action(on: bool) -> ScenarioAction {
    ScenarioAction {
        action: Action {
            method: "PUT".into(),
            path: "/api/wifi/radio".into(),
            body: Some(json!({"ap_2g": on, "ap_5g": on})),
        },
        snapshot: WIFI_KEYS.iter().map(|k| k.to_string()).collect(),
        verify_job: None,
        verify_field: None,
        best_effort: false,
        restore_on_exit: None,
    }
}

/// The CHILL group the abroad scenario switches. Must match
/// `scripts/chill/template.yaml`, where `DIRECT` is one of its members.
const CHILL_MAIN_GROUP: &str = "🚀 节点选择";
const CHILL_STATUS: &str = "/api/services/chill";
const CHILL_ACTIVE: &str = "/data/region/active";

/// Set CHILL's main group, remembering what it was so it can be put back.
fn chill_main_group_action(member: &str) -> ScenarioAction {
    ScenarioAction {
        action: Action {
            method: "PUT".into(),
            path: "/api/services/chill/regions".into(),
            body: Some(json!({"group": CHILL_MAIN_GROUP, "member": member})),
        },
        snapshot: Vec::new(),
        // Not `verify_job`: a region switch is synchronous and starts no job,
        // so the job endpoint would only report some earlier reload.
        verify_job: None,
        verify_field: Some(FieldCheck {
            path: CHILL_STATUS.into(),
            pointer: CHILL_ACTIVE.into(),
            equals: json!(member),
        }),
        best_effort: true,
        restore_on_exit: Some(RestoreOnExit {
            read_path: CHILL_STATUS.into(),
            read_pointer: CHILL_ACTIVE.into(),
            field: "member".into(),
        }),
    }
}

/// The abroad scenario on its own, for adding to an existing configuration
/// without touching what is already there.
///
/// On a local SIM the traffic already leaves from that country, so the default
/// is DIRECT; the owner changes groups by hand from there if they want, and
/// the engine leaves that alone until the device comes home.
fn abroad_scenarios() -> Vec<Scenario> {
    vec![Scenario {
        id: "abroad".into(),
        name: "国外".into(),
        detect: Detect::Abroad,
        inhibit_sleep: false,
        // Wi-Fi first: arriving from `home` means the APs are down. When they
        // are already up this is skipped, not re-applied.
        actions: vec![wifi_action(true), chill_main_group_action("DIRECT")],
    }]
}

/// A starting point for the admin UI: the two scenarios, with no home networks
/// filled in, so it still never triggers until the user names one.
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

fn validate(cfg: &Config) -> Result<(), String> {
    if cfg.scenarios.iter().filter(|s| matches!(s.detect, Detect::Fallback)).count() > 1 {
        return Err("at most one scenario may use detect.type = fallback".into());
    }
    if cfg.scenarios.iter().filter(|s| matches!(s.detect, Detect::Abroad)).count() > 1 {
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
                return Err(format!("scenario {:?}: MCC {bad:?} must be three digits", s.id));
            }
            if mccs.contains(&cfg.home_mcc) {
                return Err(format!(
                    "scenario {:?} lists the home MCC {}; it would fire at home",
                    s.id, cfg.home_mcc
                ));
            }
        }
        // Every failure lands on the fallback scenario. If it turned the APs
        // off, "something went wrong" would mean "no network at all".
        if matches!(s.detect, Detect::Fallback)
            && s.actions.iter().any(|a| {
                a.action.path == "/api/wifi/radio"
                    && a.action.body.as_ref().is_some_and(|b| {
                        b.get("ap_2g") == Some(&json!(false)) || b.get("ap_5g") == Some(&json!(false))
                    })
            })
        {
            return Err(format!(
                "scenario {:?} is the fallback and must not turn Wi-Fi off",
                s.id
            ));
        }
        for a in &s.actions {
            // A failed Wi-Fi change must roll back: shrugging off a failed
            // "turn the APs off" could strand the device half-configured.
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
            if crate::action::parse_method(&a.action.method).is_none() {
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

/// Something to undo once the device leaves the scenario that changed it.
#[derive(Serialize, Deserialize, Clone)]
struct PendingRestore {
    key: String,
    /// The original action with the saved value in its body.
    action: ScenarioAction,
    saved_at: i64,
    /// Not due while the next scenario is itself an abroad one — only once the
    /// device is home again. For values the *owner* changed abroad (CHILL
    /// switched off by hand), which no scenario action manages, so the
    /// ownership rule in `due_restores` alone would put them back at once.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    while_abroad: bool,
}

/// Scenarios that mean "outside the home country".
fn is_abroad(s: &Scenario) -> bool {
    matches!(s.detect, Detect::Mcc { .. } | Detect::Abroad)
}

/// Key of the "switch CHILL back on when home" entry (see `chill_toggled`).
const CHILL_ON_KEY: &str = "chill-on-after-abroad";
/// Key of the "exit back to proxy when home" entry (see `chill_exit_changed`).
const CHILL_EXIT_KEY: &str = "chill-exit-after-abroad";

/// Serialises read-modify-write of RESTORE_FILE between the engine thread and
/// HTTP handlers (`chill_toggled`). Never held while running an action: the
/// action may route to a handler that takes it.
static RESTORE_LOCK: Mutex<()> = Mutex::new(());

thread_local! {
    /// Set while `run_restores` runs an action, so the CHILL-enable hook can
    /// tell "the restore turning CHILL back on" from "the owner doing it".
    static IN_RESTORE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// What an action manages, independent of the value it sets: its path plus its
/// body minus the restored field. Two scenarios setting the same CHILL group
/// to different members share a key.
fn restore_key(a: &ScenarioAction) -> Option<String> {
    let r = a.restore_on_exit.as_ref()?;
    let mut body = a.action.body.clone().unwrap_or(json!({}));
    if let Some(obj) = body.as_object_mut() {
        obj.remove(&r.field);
    }
    Some(format!("{} {}", a.action.path, body))
}

/// The action that puts `saved` back.
fn restore_action(a: &ScenarioAction, saved: Value) -> Option<ScenarioAction> {
    let r = a.restore_on_exit.as_ref()?;
    let mut back = a.clone();
    let mut body = back.action.body.take().unwrap_or(json!({}));
    body.as_object_mut()?.insert(r.field.clone(), saved.clone());
    back.action.body = Some(body);
    back.restore_on_exit = None;
    back.best_effort = true;
    back.verify_field = back.verify_field.map(|c| FieldCheck { equals: saved, ..c });
    Some(back)
}

/// Pending restores the next scenario does not manage itself, i.e. the ones
/// that are due when switching to it.
fn due_restores(pending: &[PendingRestore], next: Option<&Scenario>) -> Vec<String> {
    let owned: Vec<String> = next
        .map(|s| s.actions.iter().filter_map(restore_key).collect())
        .unwrap_or_default();
    let abroad_next = next.is_some_and(is_abroad);
    let mut due: Vec<String> = pending
        .iter()
        .filter(|p| !owned.contains(&p.key) && !(p.while_abroad && abroad_next))
        .map(|p| p.key.clone())
        .collect();
    // CHILL back on before anything that talks to it; the exit (mode) last,
    // after the main group has its pre-trip node back.
    due.sort_by_key(|k| match k.as_str() {
        CHILL_ON_KEY => 0,
        CHILL_EXIT_KEY => 2,
        _ => 1,
    });
    due
}

// ── runtime state ───────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Default, Clone)]
struct RunState {
    /// Scenario currently applied. Empty means "nothing applied yet".
    current: String,
    /// Scenario accumulating hits towards a switch.
    candidate: String,
    hits: u32,
    misses: u32,
    /// uci key → the value the engine last wrote. Anything that differs now was
    /// changed by someone else, and the engine leaves it alone.
    applied: BTreeMap<String, String>,
    last_switch: Option<i64>,
    last_error: Option<String>,
    /// Wall-clock second of the last completed scan.
    last_scan: i64,
    /// Consecutive scan failures. Reset on any successful scan.
    #[serde(default)]
    scan_failures: u32,
}

/// After this many consecutive scan failures, a scenario that has Wi-Fi turned
/// off is abandoned and the device falls back to `away`.
///
/// Without this the engine fails *closed*: `tick` returns early when a scan
/// errors, so no "miss" ever accumulates, so the exit condition never fires,
/// so Wi-Fi stays off indefinitely. That is survivable while you are at home
/// and using the house network — and catastrophic the moment you walk out,
/// because this device is the only way the phone gets online. Fail open.
const SCAN_FAILURES_BEFORE_FAIL_OPEN: u32 = 3;

fn now() -> i64 {
    unsafe { libc::time(std::ptr::null_mut()) as i64 }
}

fn read_json<T: for<'de> Deserialize<'de> + Default>(path: &str) -> T {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_json<T: Serialize>(path: &str, value: &T) {
    let _ = fs::create_dir_all(DIR);
    if let Ok(text) = serde_json::to_string_pretty(value) {
        let tmp = format!("{path}.tmp");
        if fs::write(&tmp, text).is_ok() {
            let _ = fs::rename(&tmp, path);
        }
    }
}

/// Append one line, rotating at a size cap. `/data` is 1.8 GB and shared with
/// everything else on the device, so an unbounded log is a slow leak.
fn log_line(msg: &str) {
    let _ = fs::create_dir_all(DIR);
    if fs::metadata(LOG_FILE).map(|m| m.len()).unwrap_or(0) > LOG_MAX_BYTES {
        let keep = fs::read_to_string(LOG_FILE).unwrap_or_default();
        let lines: Vec<&str> = keep.lines().collect();
        let start = lines.len().saturating_sub(LOG_TAIL_LINES);
        let _ = fs::write(LOG_FILE, lines[start..].join("\n"));
    }
    let stamp = {
        // Take the value straight from `libc::time` rather than casting an i64:
        // naming `libc::time_t` is deprecated on musl, where its width changed.
        unsafe {
            let tt = libc::time(std::ptr::null_mut());
            let mut tm: libc::tm = std::mem::zeroed();
            libc::localtime_r(&tt, &mut tm);
            format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
                tm.tm_year + 1900,
                tm.tm_mon + 1,
                tm.tm_mday,
                tm.tm_hour,
                tm.tm_min,
                tm.tm_sec
            )
        }
    };
    use std::io::Write as _;
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(LOG_FILE) {
        let _ = writeln!(f, "{stamp} {msg}");
    }
}

// ── SIM ─────────────────────────────────────────────────────────────────────

/// The home MCC of the SIM in the slot, or `None` when it cannot be known.
///
/// `None` must never read as "abroad": a SIM that is not ready, or an eUICC
/// mid-way through a profile switch, reports an empty IMSI, and an empty string
/// is also "not 460". Hysteresis would usually absorb that, but it should not
/// have to.
fn sim_mcc() -> Option<String> {
    let info = ubus::call("zwrt_zte_mdm.api", "get_sim_info", Some("{}")).ok()?;
    mcc_from_sim_info(&info)
}

fn mcc_from_sim_info(info: &Value) -> Option<String> {
    let ready = info
        .get("sim_states")
        .and_then(|v| v.as_str())
        .is_some_and(|s| s.contains("ready"));
    let imsi = info.get("sim_imsi").and_then(|v| v.as_str()).unwrap_or("");
    if !ready || imsi.len() < 5 || !imsi.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(imsi[..3].to_string())
}

// ── sleep survival ──────────────────────────────────────────────────────────

fn set_auto_sleep(enabled: bool) {
    let arg = format!(r#"{{"switch":{}}}"#, enabled);
    let _ = ubus::call(SLEEP_UBUS_OBJ, "enableAutoSleep", Some(&arg));
}

/// Belt to the wakelock's braces: if the device suspends anyway, the RTC brings
/// it back so the next tick can still notice you have left. Re-armed every
/// tick, so while awake it keeps being pushed out and never fires.
fn arm_wakealarm(secs: i64) {
    let Ok(text) = fs::read_to_string(RTC_SINCE_EPOCH) else {
        return;
    };
    let Ok(base) = text.trim().parse::<i64>() else {
        return;
    };
    // The kernel requires clearing a pending alarm before setting a new one.
    let _ = fs::write(RTC_WAKEALARM, "0");
    let _ = fs::write(RTC_WAKEALARM, (base + secs).to_string());
}

fn clear_wakealarm() {
    let _ = fs::write(RTC_WAKEALARM, "0");
}

// ── heartbeat and takeover ──────────────────────────────────────────────────

/// Whole seconds from the text of /proc/uptime ("12345.67 54321.00").
fn uptime_secs(text: &str) -> Option<u64> {
    text.split_whitespace().next()?.split('.').next()?.parse().ok()
}

fn write_heartbeat() {
    let Some(secs) = fs::read_to_string("/proc/uptime").ok().as_deref().and_then(uptime_secs) else {
        return; // no uptime, no heartbeat: u60-guard reads that as "not alive", the safe side
    };
    // Rename so u60-guard never reads a half-written number.
    let tmp = format!("{HEARTBEAT_FILE}.tmp");
    if fs::write(&tmp, format!("{secs}\n")).is_ok() {
        let _ = fs::rename(&tmp, HEARTBEAT_FILE);
    }
}

#[derive(Debug, PartialEq)]
enum Takeover {
    /// No marker: u60-guard is not involved.
    None,
    /// Marker present and not yet held long enough: stay in away, ignore pin.
    Active,
    /// Held for TAKEOVER_HOLD: remove the marker and resume.
    Release,
}

/// `seen` is this process's record of which marker it has been watching (by
/// content — u60-guard writes its uptime into it) and since when.
fn takeover_state(
    marker: Option<&str>,
    seen: &mut Option<(String, Instant)>,
    now: Instant,
    hold: Duration,
) -> Takeover {
    let Some(content) = marker else {
        *seen = None;
        return Takeover::None;
    };
    match seen {
        Some((c, since)) if c == content => {
            if now.duration_since(*since) >= hold {
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

// ── engine ──────────────────────────────────────────────────────────────────

pub struct Engine {
    cfg: Mutex<Config>,
    state: Mutex<RunState>,
    /// A scan plus an apply can take tens of seconds. Without this, a slow tick
    /// would overlap the next one and two appliers would fight.
    busy: AtomicBool,
    /// Which u60-guard takeover marker this process is honouring, and since when.
    takeover: Mutex<Option<(String, Instant)>>,
}

impl Engine {
    pub fn new() -> Self {
        Engine {
            cfg: Mutex::new(read_json::<Config>(CONFIG_FILE)),
            state: Mutex::new(read_json::<RunState>(STATE_FILE)),
            busy: AtomicBool::new(false),
            takeover: Mutex::new(None),
        }
    }

    /// Is u60-guard in charge of Wi-Fi? True while its marker exists; once this
    /// process has run for TAKEOVER_HOLD with the same marker, removes it and
    /// hands control back to detection and the pin.
    fn guard_takeover(&self) -> bool {
        let marker = fs::read_to_string(TAKEOVER_FILE).ok().map(|s| s.trim().to_string());
        let mut seen = self.takeover.lock().unwrap_or_else(|e| e.into_inner());
        // A marker we are not already honouring: none before, or a new takeover.
        let fresh = seen.as_ref().map_or(true, |(c, _)| Some(c.as_str()) != marker.as_deref());
        match takeover_state(marker.as_deref(), &mut seen, Instant::now(), TAKEOVER_HOLD) {
            Takeover::None => false,
            Takeover::Active => {
                if fresh {
                    log_line("u60-guard took over Wi-Fi: staying in away, pin paused");
                }
                true
            }
            Takeover::Release => {
                let _ = fs::remove_file(TAKEOVER_FILE);
                log_line("u60-guard takeover released after 10 min of steady running; pin and detection resume");
                false
            }
        }
    }

    /// For the status endpoints; no side effects.
    fn takeover_marked(&self) -> bool {
        std::path::Path::new(TAKEOVER_FILE).exists()
    }

    pub fn enabled(&self) -> bool {
        !std::path::Path::new(DISABLED_FLAG).exists()
    }

    fn pin(&self) -> Option<String> {
        fs::read_to_string(PIN_FILE)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Reset to a known-good state, then tick forever.
    ///
    /// Both happen on this thread, off the startup path: `bootsafe` can take
    /// tens of seconds if it has to repair Wi-Fi, and the admin UI must not be
    /// unreachable while it does.
    pub fn start(self: &Arc<Self>, app: Arc<AppState>) {
        let engine = Arc::clone(self);
        std::thread::spawn(move || {
            // A panic here ends only this thread: HTTP keeps answering, so the
            // process looks healthy to procd while the heartbeat has stopped and
            // nothing is left to release a u60-guard takeover. (A poisoned
            // state lock from a panicking handler is enough to cause it.)
            let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                // Before bootsafe, which can block for tens of seconds repairing
                // Wi-Fi: u60-guard should see us alive as early as possible.
                write_heartbeat();
                engine.bootsafe(&app);
                loop {
                    write_heartbeat();
                    std::thread::sleep(Duration::from_secs(TICK_SECS));
                    engine.tick(&app);
                }
            }));
            if died.is_err() {
                log_line("engine thread panicked; heartbeat stopped");
                // Under procd, exit so it restarts us whole. Started the old
                // way nothing would restart us, and a live admin UI beats a
                // dead one — u60-guard covers Wi-Fi either way.
                if std::env::var_os(SUPERVISED_ENV).is_some() {
                    eprintln!("[scenario] engine thread panicked; exiting for the supervisor");
                    std::process::exit(70);
                }
            }
        });
    }

    /// Put the device back to the away scenario and clear runtime state.
    ///
    /// Called at boot. Runtime state is cleared but the pin is not: the pin
    /// exists precisely for when detection is misbehaving, and a reboot is the
    /// most likely thing to happen right after that. A pinned scenario is
    /// re-applied on the next tick, through the full apply-and-verify path.
    pub fn bootsafe(&self, app: &AppState) {
        let _ = fs::create_dir_all(DIR);
        set_auto_sleep(true);
        clear_wakealarm();
        let away = self
            .cfg
            .lock()
            .ok()
            .and_then(|c| c.scenarios.iter().find(|s| s.id == AWAY_ID).cloned());
        {
            let mut st = self.state.lock().unwrap();
            *st = RunState::default();
            write_json(STATE_FILE, &*st);
        }
        // Repair, don't churn. The vendor daemon brings the APs up on boot by
        // itself, so the overwhelming majority of boots need nothing here —
        // and `apply` always writes and reloads, which tears the wiphy down and
        // rebuilds it. Doing that unconditionally would bounce Wi-Fi and
        // re-run ACS on every single boot to fix a problem that isn't there.
        //
        // Sampled twice, a few seconds apart, because a single reading can be
        // taken mid-teardown and report "up" about a radio that is on its way
        // down. That is not hypothetical: restarting the agent while a previous
        // instance had just written `disabled=1` and fired the reload produced
        // exactly that — "already up; nothing to repair", followed seconds later
        // by zero hostapd processes. The engine self-heals on its next scan, but
        // the boot path should not need rescuing.
        std::thread::sleep(Duration::from_secs(5));
        let up_once = crate::wifi_radio::beaconing();
        std::thread::sleep(Duration::from_secs(4));
        if up_once && crate::wifi_radio::beaconing() {
            log_line("[bootsafe] Wi-Fi already up; nothing to repair");
            return;
        }

        match away {
            Some(scen) => match self.apply(app, &scen) {
                Ok(()) => log_line("[bootsafe] Wi-Fi was down; restored away scenario"),
                Err(e) => log_line(&format!("[bootsafe] away scenario failed: {e}")),
            },
            None => {
                // No config yet: still make sure Wi-Fi comes back, because that
                // is the state a reboot must always be able to reach.
                match crate::wifi_radio::apply(true, true) {
                    Ok(_) => log_line("[bootsafe] Wi-Fi was down; forced it on"),
                    Err(e) => log_line(&format!("[bootsafe] forcing Wi-Fi on failed: {e}")),
                }
            }
        }
    }

    fn scan_interval(&self, cfg: &Config, current: &str, candidate: &str) -> u64 {
        // The cheap interval is for when the APs are down and nobody is
        // connected — which is what `inhibit_sleep` marks. An abroad scenario
        // keeps the APs up with clients on them, so it scans like away.
        if cfg.scenarios.iter().any(|s| s.id == current && s.inhibit_sleep) {
            return cfg.params.scan_interval_home_secs;
        }
        // Nothing decided yet (just booted or restarted): the screen says
        // "判定中" until two scans agree, and on battery that would take ten
        // minutes. Two quick scans are a small price for knowing where we are.
        //
        // Same for a switch already in the making: one scan saw somewhere else,
        // so confirm it quickly instead of waiting a full battery interval.
        // Walking in the door on battery otherwise took 5–10 minutes to turn
        // Wi-Fi off. A stray reading costs one extra scan, not a switch — the
        // hysteresis still needs the second one to agree.
        if current.is_empty() || !candidate.is_empty() {
            return cfg.params.scan_interval_away_charging_secs;
        }
        let charging = ubus::call("zwrt_bsp.charger", "list", Some("{}"))
            .ok()
            .and_then(|v| v.get("charger_connect").and_then(|c| c.as_i64()))
            .map(|c| c == 1)
            .unwrap_or(false);
        if charging {
            cfg.params.scan_interval_away_charging_secs
        } else {
            cfg.params.scan_interval_away_battery_secs
        }
    }

    /// Run one action. Wireless goes through `wifi_radio::apply` directly —
    /// routing it would wait on the Wi-Fi lock with the short HTTP timeout, and the routed
    /// handler's 200 would not prove anything anyway.
    fn run_action(&self, app: &AppState, a: &ScenarioAction) -> Result<(), String> {
        if a.action.path == "/api/wifi/radio" {
            let body = a.action.body.clone().unwrap_or(json!({}));
            let want = |k: &str| body.get(k).and_then(|v| v.as_bool());
            let (cur_2g, cur_5g) = crate::wifi_radio::snapshot();
            let ap_2g = want("ap_2g").unwrap_or(cur_2g);
            let ap_5g = want("ap_5g").unwrap_or(cur_5g);
            // `apply` always tears down and rebuilds. Moving between two
            // scenarios that both want Wi-Fi on (away ⇄ abroad) must not kick
            // every client off for half a minute to change nothing.
            if ap_2g && ap_5g && crate::wifi_radio::fully_on() {
                return Ok(());
            }
            crate::wifi_radio::apply(ap_2g, ap_5g).map(|_| ())
        } else {
            let body = crate::action::body_bytes(&a.action.body);
            let outcome = crate::action::exec(app, &a.action.method, &a.action.path, &body);
            if !outcome.ok() {
                return Err(outcome
                    .error
                    .unwrap_or_else(|| "action failed".into()));
            }
            if let Some(job_path) = &a.verify_job {
                wait_for_job(app, job_path)?;
            }
            if let Some(check) = &a.verify_field {
                wait_for_field(app, check)?;
            }
            Ok(())
        }
    }

    /// Apply a scenario's actions in order, rolling back on the first failure.
    fn apply(&self, app: &AppState, scen: &Scenario) -> Result<(), String> {
        let mut undo: Vec<(String, String)> = Vec::new();

        for a in &scen.actions {
            // Manual-override check. If a key the engine previously wrote no
            // longer holds the engine's value, someone changed it on purpose —
            // leave it and say so, rather than yanking it back every tick.
            let overridden = {
                let st = self.state.lock().unwrap();
                a.snapshot.iter().find(|key| {
                    match st.applied.get(*key) {
                        Some(prev) => ubus::uci_get(key).unwrap_or_default() != *prev,
                        None => false,
                    }
                }).cloned()
            };
            if let Some(key) = overridden {
                log_line(&format!(
                    "manual-override kept: {key} differs from what the engine last set; skipping {} {}",
                    a.action.method, a.action.path
                ));
                continue;
            }

            for key in &a.snapshot {
                undo.push((key.clone(), ubus::uci_get(key).unwrap_or_default()));
            }
            self.remember_for_restore(app, a);

            if let Err(e) = self.run_action(app, a) {
                if a.best_effort {
                    log_line(&format!(
                        "best-effort action failed ({} {}): {e} — continuing",
                        a.action.method, a.action.path
                    ));
                    continue;
                }
                log_line(&format!(
                    "action failed ({} {}): {e} — rolling back",
                    a.action.method, a.action.path
                ));
                self.rollback(&undo);
                return Err(e);
            }

            // Latch what we just wrote, so the override check above can tell
            // our edits from the user's next time round.
            let mut st = self.state.lock().unwrap();
            for key in &a.snapshot {
                st.applied
                    .insert(key.clone(), ubus::uci_get(key).unwrap_or_default());
            }
            write_json(STATE_FILE, &*st);
        }
        Ok(())
    }

    /// Save the value `a` is about to replace — unless something is already
    /// saved under its key. Re-entering abroad after a reboot there must keep
    /// the pre-trip value, not overwrite it with the DIRECT it set itself.
    fn remember_for_restore(&self, app: &AppState, a: &ScenarioAction) {
        let (Some(r), Some(key)) = (a.restore_on_exit.as_ref(), restore_key(a)) else {
            return;
        };
        if read_json::<Vec<PendingRestore>>(RESTORE_FILE).iter().any(|p| p.key == key) {
            return;
        }
        let (status, resp) =
            crate::server::route(&tiny_http::Method::Get, &r.read_path, app, b"");
        let current = resp.pointer(&r.read_pointer).cloned().unwrap_or(Value::Null);
        if status >= 400 || current.is_null() {
            log_line(&format!(
                "cannot read {}{} before changing it; nothing to restore later",
                r.read_path, r.read_pointer
            ));
            return;
        }
        let Some(back) = restore_action(a, current.clone()) else {
            return;
        };
        log_line(&format!("saved {current} to restore on leaving"));
        let _lk = RESTORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut pending: Vec<PendingRestore> = read_json(RESTORE_FILE);
        if pending.iter().any(|p| p.key == key) {
            return;
        }
        pending.push(PendingRestore {
            key,
            action: back,
            saved_at: now(),
            while_abroad: false,
        });
        write_json(RESTORE_FILE, &pending);
    }

    /// Put back whatever `next` does not manage itself. Best-effort: a failure
    /// (CHILL stopped, say) keeps the entry so a later tick can try again.
    /// `quiet` skips logging failures, for the periodic retry.
    fn run_restores(&self, app: &AppState, next: Option<&Scenario>, quiet: bool) {
        let pending: Vec<PendingRestore> = read_json(RESTORE_FILE);
        let due = due_restores(&pending, next);
        if due.is_empty() {
            return;
        }
        // Run without RESTORE_LOCK (an action can reach a handler that takes
        // it), then drop what succeeded from a fresh read, so an entry an HTTP
        // handler added meanwhile is not lost.
        let mut done: Vec<String> = Vec::new();
        for key in &due {
            let Some(p) = pending.iter().find(|p| &p.key == key) else { continue };
            IN_RESTORE.with(|f| f.set(true));
            let result = self.run_action(app, &p.action);
            IN_RESTORE.with(|f| f.set(false));
            match result {
                Ok(()) => {
                    let body = p.action.action.body.clone().unwrap_or(Value::Null);
                    log_line(&format!("restored {} {body}", p.action.action.path));
                    done.push(key.clone());
                }
                Err(e) => {
                    if !quiet {
                        log_line(&format!("restore failed, will retry: {e}"));
                    }
                }
            }
        }
        if done.is_empty() {
            return;
        }
        let _lk = RESTORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut now_pending: Vec<PendingRestore> = read_json(RESTORE_FILE);
        now_pending.retain(|p| !done.contains(&p.key));
        write_json(RESTORE_FILE, &now_pending);
    }

    /// Put snapshotted keys back, newest first, then make them take effect.
    fn rollback(&self, undo: &[(String, String)]) {
        if undo.is_empty() {
            return;
        }
        // Wi-Fi is the only thing snapshotted today, and it must end up on.
        let lk = match crate::wifi_radio::lock(crate::wifi_radio::ENGINE_WAIT) {
            Ok(l) => l,
            Err(e) => {
                // The snapshot is only the two AP flags, which `apply(true,
                // true)` overwrites anyway — so skip the restore and go
                // straight for "on", which retries the lock itself.
                log_line(&format!("rollback: {e}; forcing Wi-Fi on instead"));
                match crate::wifi_radio::apply(true, true) {
                    Ok(_) => log_line("rollback complete; Wi-Fi forced on"),
                    Err(e) => log_line(&format!("rollback: could not restore Wi-Fi: {e}")),
                }
                return;
            }
        };
        for (key, value) in undo.iter().rev() {
            let _ = ubus::uci_set_no_commit(key, value);
        }
        let _ = ubus::uci_commit("wireless");
        match crate::wifi_radio::apply_locked(&lk, true, true) {
            Ok(_) => log_line("rollback complete; Wi-Fi forced on"),
            Err(e) => log_line(&format!("rollback: could not restore Wi-Fi: {e}")),
        }
    }

    fn switch_to(&self, app: &AppState, cfg: &Config, id: &str) {
        let Some(scen) = cfg.scenarios.iter().find(|s| s.id == id) else {
            return;
        };
        log_line(&format!("entering scenario {:?} ({})", scen.name, scen.id));
        self.run_restores(app, Some(scen), false);

        // Hold off suspend *before* tearing the APs down: once they are off,
        // Tailscale over cellular is the only way back in, and a suspended
        // device answers nothing.
        if scen.inhibit_sleep {
            set_auto_sleep(false);
            arm_wakealarm(cfg.params.scan_interval_home_secs as i64 + 30);
        }

        let result = self.apply(app, scen);

        let mut st = self.state.lock().unwrap();
        match result {
            Ok(()) => {
                st.current = scen.id.clone();
                st.last_switch = Some(now());
                // u60-guard holds SMS alerts while abroad (roaming charges).
                crate::alerts::set_abroad(matches!(scen.detect, Detect::Mcc { .. } | Detect::Abroad));
                st.last_error = None;
                if !scen.inhibit_sleep {
                    set_auto_sleep(true);
                    clear_wakealarm();
                }
                log_line(&format!("scenario {:?} applied", scen.id));
            }
            Err(e) => {
                // Rollback already ran inside apply(); land on away so the next
                // tick starts from a known state rather than a half-applied one.
                st.current = AWAY_ID.to_string();
                st.last_error = Some(e.clone());
                set_auto_sleep(true);
                clear_wakealarm();
                log_line(&format!("scenario {:?} failed: {e}; fell back to away", scen.id));
            }
        }
        st.candidate.clear();
        st.hits = 0;
        st.misses = 0;
        write_json(STATE_FILE, &*st);
    }

    fn tick(&self, app: &AppState) {
        if self.busy.swap(true, Ordering::SeqCst) {
            return; // previous tick still working
        }
        let _guard = BusyGuard(&self.busy);

        // u60-guard stepped in while we were dead or stuck. Its Wi-Fi-on wins
        // over detection and over the pin until the takeover is released.
        if self.guard_takeover() {
            let current = { self.state.lock().unwrap().current.clone() };
            let cfg = { self.cfg.lock().unwrap().clone() };
            if self.enabled() && !cfg.scenarios.is_empty() && current != AWAY_ID {
                log_line("u60-guard takeover: returning to away");
                self.switch_to(app, &cfg, AWAY_ID);
            }
            return;
        }

        if !self.enabled() {
            // Not just "stop working": if the engine is switched off while a
            // scenario has Wi-Fi down, freezing there would leave the device
            // permanently without the network it exists to provide. The off
            // switch has to be an escape hatch, so it restores `away` first.
            // The HTTP endpoint does this too; doing it here as well makes
            // `touch /data/scenario/disabled` — the only escape available over
            // SSH — behave the same way.
            let current = { self.state.lock().unwrap().current.clone() };
            if !current.is_empty() && current != AWAY_ID {
                log_line("engine disabled while away from the away scenario — restoring it");
                self.bootsafe(app);
            }
            // Off means "as if the engine were not here": put back anything a
            // scenario changed, too.
            self.run_restores(app, None, true);
            return;
        }
        let cfg = { self.cfg.lock().unwrap().clone() };
        if cfg.scenarios.is_empty() {
            return; // unconfigured engines do nothing
        }

        let (current, candidate, last_scan) = {
            let st = self.state.lock().unwrap();
            (st.current.clone(), st.candidate.clone(), st.last_scan)
        };

        // A pin overrides detection entirely, but still goes through the whole
        // apply-and-verify path rather than being assumed.
        if let Some(pinned) = self.pin() {
            if pinned != current {
                self.switch_to(app, &cfg, &pinned);
            }
            return;
        }

        // Wall clock, not tick counting: the home scenario lets the device
        // suspend, and a monotonic sleep does not advance while suspended.
        let due = now() - last_scan;
        if due < self.scan_interval(&cfg, &current, &candidate) as i64 {
            // Keep the sleep defences fresh even when not scanning.
            if cfg
                .scenarios
                .iter()
                .any(|s| s.id == current && s.inhibit_sleep)
            {
                set_auto_sleep(false);
                arm_wakealarm(cfg.params.scan_interval_home_secs as i64 + 30);
            }
            return;
        }

        // A restore that failed on the way out (CHILL was stopped, say) gets
        // another go each scan, for as long as the current scenario does not
        // manage that value itself. Not before the first decision after boot:
        // with nothing applied yet the device may well still be abroad, and
        // restoring there would flip the route back and forth.
        if !current.is_empty() {
            let current_scen = cfg.scenarios.iter().find(|s| s.id == current);
            self.run_restores(app, current_scen, true);
        }

        let nets = match wifi_scan::scan() {
            Ok(n) => n,
            Err(e) => {
                let failures = {
                    let mut st = self.state.lock().unwrap();
                    st.last_scan = now();
                    st.scan_failures += 1;
                    st.last_error = Some(format!("scan: {e}"));
                    write_json(STATE_FILE, &*st);
                    st.scan_failures
                };
                log_line(&format!("scan failed ({failures} in a row): {e}"));

                // Fail open. If we cannot see where we are and the APs are
                // down, the safe guess is "not home" — leaving Wi-Fi off would
                // strand the only device that provides it.
                if failures >= SCAN_FAILURES_BEFORE_FAIL_OPEN
                    && current != AWAY_ID
                    && !crate::wifi_radio::beaconing()
                {
                    log_line(
                        "cannot scan and Wi-Fi is off — failing open to the away scenario",
                    );
                    self.switch_to(app, &cfg, AWAY_ID);
                }
                return;
            }
        };

        let mcc = sim_mcc();
        let detected =
            detect(&cfg, &nets, mcc.as_deref()).unwrap_or_else(|| AWAY_ID.to_string());

        let switch_to = {
            let mut st = self.state.lock().unwrap();
            st.last_scan = now();
            st.scan_failures = 0;
            let decision = decide(&mut st, &detected, &cfg.params);
            write_json(STATE_FILE, &*st);
            decision
        };

        if let Some(id) = switch_to {
            self.switch_to(app, &cfg, &id);
        }
    }
}

/// Which scenario the surroundings say we are in.
///
/// Three passes, so priority does not depend on how the list happens to be
/// ordered: named networks and specific MCCs first (in configuration order),
/// then the catch-all abroad, then the fallback. Home beats a foreign SIM on
/// purpose — with a multi-profile eUICC you can be at home on a foreign
/// profile, and the house network is still the right answer there.
fn detect(
    cfg: &Config,
    nets: &[wifi_scan::Network],
    mcc: Option<&str>,
) -> Option<String> {
    for scen in &cfg.scenarios {
        let hit = match &scen.detect {
            Detect::Ssid { entries } => entries.iter().any(|e| {
                nets.iter().any(|n| {
                    n.ssid == e.ssid
                        && match &e.bssid {
                            // A named BSSID is exact: it is the half of spoof
                            // resistance that signal strength cannot provide.
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
    if mcc.is_some_and(|m| m != cfg.home_mcc) {
        if let Some(s) = cfg.scenarios.iter().find(|s| matches!(s.detect, Detect::Abroad)) {
            return Some(s.id.clone());
        }
    }
    cfg.scenarios
        .iter()
        .find(|s| matches!(s.detect, Detect::Fallback))
        .map(|s| s.id.clone())
}

/// Hysteresis, as a pure function so both directions can be tested without a
/// radio. Returns the scenario to switch to, or `None` to stay put.
///
/// Entering and leaving are counted separately on purpose: a single stray
/// reading in either direction must not move the device, because both
/// directions are disruptive — one turns the phone's only uplink off, the other
/// bounces every client back onto it.
fn decide(st: &mut RunState, detected: &str, p: &Params) -> Option<String> {
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
    // Nothing applied yet (fresh boot): there is nothing to leave, so only the
    // entry threshold applies.
    let leaving_ok = st.current.is_empty() || st.misses >= p.exit_misses;
    if st.hits >= p.enter_hits && leaving_ok {
        Some(detected.to_string())
    } else {
        None
    }
}

struct BusyGuard<'a>(&'a AtomicBool);
impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Poll a `{job_id, status}` endpoint until it stops reporting `running`.
/// CHILL and eSIM both answer 200 immediately and finish on a worker thread, so
/// the HTTP status of the triggering call says nothing about the outcome.
fn wait_for_job(app: &AppState, path: &str) -> Result<(), String> {
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(1000));
        let (status, resp) = crate::server::route(&tiny_http::Method::Get, path, app, b"");
        if status >= 400 {
            continue; // transient; keep polling until the deadline
        }
        let still_running = resp
            .pointer("/data/status")
            .and_then(|v| v.as_str())
            .map(|s| s == "running")
            .unwrap_or(false);
        if still_running {
            continue;
        }
        if let Some(err) = resp.pointer("/data/error").and_then(|v| v.as_str()) {
            return Err(format!("job reported: {err}"));
        }
        // CHILL reports failure as `status: "error"` with the reason in
        // `message`, not in an `error` field.
        if resp.pointer("/data/status").and_then(|v| v.as_str()) == Some("error") {
            let msg = resp
                .pointer("/data/message")
                .and_then(|v| v.as_str())
                .unwrap_or("no message");
            return Err(format!("job reported: {msg}"));
        }
        return Ok(());
    }
    Err(format!("job at {path} did not settle within 40s"))
}

/// Poll a GET until `check.pointer` holds `check.equals`. A missing field
/// counts as "not yet" — for CHILL, `region` is only reported while running.
fn wait_for_field(app: &AppState, check: &FieldCheck) -> Result<(), String> {
    let mut last = Value::Null;
    for _ in 0..10 {
        let (status, resp) =
            crate::server::route(&tiny_http::Method::Get, &check.path, app, b"");
        if status < 400 {
            last = resp.pointer(&check.pointer).cloned().unwrap_or(Value::Null);
            if last == check.equals {
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(1000));
    }
    Err(format!(
        "{}{} is {last}, expected {}",
        check.path, check.pointer, check.equals
    ))
}

// ── HTTP ────────────────────────────────────────────────────────────────────

fn state_json(engine: &Engine) -> Value {
    // A ubus round-trip; take it before the locks, not while holding them.
    let sim_mcc = sim_mcc();
    let cfg = engine.cfg.lock().unwrap();
    let st = engine.state.lock().unwrap();
    json!({
        "enabled": engine.enabled(),
        "pin": engine.pin(),
        "guard_takeover": engine.takeover_marked(),
        "current": st.current,
        "candidate": st.candidate,
        "hits": st.hits,
        "misses": st.misses,
        "applied": st.applied,
        "last_switch": st.last_switch,
        "last_scan": st.last_scan,
        "last_error": st.last_error,
        "config": &*cfg,
        "sim_mcc": sim_mcc,
        "pending_restore": read_json::<Vec<PendingRestore>>(RESTORE_FILE)
            .iter()
            .map(|p| json!({"key": p.key, "body": p.action.action.body, "saved_at": p.saved_at}))
            .collect::<Vec<_>>(),
        "allowed_paths": ALLOWED_PATHS,
    })
}

/// CHILL was switched on or off through the agent (admin web, touch screen, or
/// a scenario action). Switched off while the device is in an abroad scenario:
/// remember to switch it back on once home — abroad the owner often has no use
/// for it, but at home it is expected to be running. Switched on by anyone but
/// that restore: nothing is left to put back.
///
/// `was_on` is whether CHILL was on before an "off": turning off something
/// already off must not arrange for it to come on later.
pub fn chill_toggled(engine: &Engine, on: bool, was_on: bool) {
    if on {
        if IN_RESTORE.with(|f| f.get()) {
            return; // the restore itself; run_restores removes the entry on success
        }
        let _lk = RESTORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut pending: Vec<PendingRestore> = read_json(RESTORE_FILE);
        let before = pending.len();
        pending.retain(|p| p.key != CHILL_ON_KEY);
        if pending.len() != before {
            write_json(RESTORE_FILE, &pending);
            log_line("CHILL switched on by hand; no longer switching it on when home");
        }
        return;
    }
    if !was_on {
        return;
    }
    if add_abroad_restore(engine, chill_on_restore(now())) {
        log_line("CHILL switched off abroad; will switch it back on when home");
    }
}

/// The owner changed CHILL's exit (proxy / direct except AI / all direct) by
/// hand. Abroad, whatever they pick, home means proxy again.
pub fn chill_exit_changed(engine: &Engine) {
    if IN_RESTORE.with(|f| f.get()) {
        return;
    }
    if add_abroad_restore(engine, chill_exit_restore(now())) {
        log_line("CHILL exit changed abroad; will go back to proxy when home");
    }
}

fn in_abroad_scenario(engine: &Engine) -> bool {
    if !engine.enabled() {
        return false;
    }
    let cfg = engine.cfg.lock().unwrap_or_else(|e| e.into_inner());
    let st = engine.state.lock().unwrap_or_else(|e| e.into_inner());
    cfg.scenarios.iter().any(|s| s.id == st.current && is_abroad(s))
}

/// Add `entry` unless the device is not abroad or one with its key is already
/// waiting. True when added.
fn add_abroad_restore(engine: &Engine, entry: PendingRestore) -> bool {
    if !in_abroad_scenario(engine) {
        return false;
    }
    let _lk = RESTORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut pending: Vec<PendingRestore> = read_json(RESTORE_FILE);
    if pending.iter().any(|p| p.key == entry.key) {
        return false;
    }
    pending.push(entry);
    write_json(RESTORE_FILE, &pending);
    true
}

fn chill_exit_restore(saved_at: i64) -> PendingRestore {
    PendingRestore {
        key: CHILL_EXIT_KEY.into(),
        action: ScenarioAction {
            action: Action {
                method: "PUT".into(),
                path: "/api/services/chill/exit".into(),
                body: Some(json!({"state": "proxy"})),
            },
            snapshot: Vec::new(),
            verify_job: None,
            verify_field: Some(FieldCheck {
                path: CHILL_STATUS.into(),
                pointer: "/data/exit".into(),
                equals: json!("proxy"),
            }),
            best_effort: true,
            restore_on_exit: None,
        },
        saved_at,
        while_abroad: true,
    }
}

fn chill_on_restore(saved_at: i64) -> PendingRestore {
    PendingRestore {
        key: CHILL_ON_KEY.into(),
        action: ScenarioAction {
            action: Action {
                method: "POST".into(),
                path: "/api/services/chill/enable".into(),
                body: None,
            },
            snapshot: Vec::new(),
            // Enable answers at once and starts chill.sh on a worker thread.
            verify_job: Some("/api/services/chill/job".into()),
            verify_field: None,
            best_effort: true,
            restore_on_exit: None,
        },
        saved_at,
        while_abroad: true,
    }
}

/// The slice of engine state the touch screen shows, for the unauthenticated
/// `/api/public/status`. Scenario names and times only — never the network
/// list, the SIM, or anything a LAN client could use to spoof "home".
pub fn public_summary(engine: &Engine) -> Value {
    let cfg = engine.cfg.lock().unwrap();
    let st = engine.state.lock().unwrap();
    let scen = cfg.scenarios.iter().find(|s| s.id == st.current);
    json!({
        "configured": !cfg.scenarios.is_empty(),
        "enabled": engine.enabled(),
        "current": st.current,
        "name": scen.map(|s| s.name.as_str()).unwrap_or(""),
        // inhibit_sleep marks the scenarios that take the APs down.
        "wifi_off": scen.is_some_and(|s| s.inhibit_sleep),
        // The touch screen offers to switch CHILL off while this is true.
        "abroad": engine.enabled() && scen.is_some_and(is_abroad),
        "chill_on_when_home": read_json::<Vec<PendingRestore>>(RESTORE_FILE)
            .iter()
            .any(|p| p.key == CHILL_ON_KEY),
        // Abroad, the main group goes DIRECT by itself (see abroad_scenarios).
        "auto_direct": scen.is_some_and(|s| {
            is_abroad(s)
                && s.actions.iter().any(|a| {
                    a.action.path == "/api/services/chill/regions"
                        && a.action.body.as_ref().and_then(|b| b.get("member")).and_then(|m| m.as_str())
                            == Some("DIRECT")
                })
        }),
        "pin": engine.pin(),
        "guard_takeover": engine.takeover_marked(),
        "last_switch": st.last_switch,
    })
}

/// GET /api/scenario
pub fn scenario_get(state: &AppState) -> (u16, Value) {
    (200, json!({"ok": true, "data": state_json(&state.scenario)}))
}

/// PUT /api/scenario — replace the whole configuration.
pub fn scenario_put(state: &AppState, body: &[u8]) -> (u16, Value) {
    let cfg: Config = match serde_json::from_slice(body) {
        Ok(c) => c,
        Err(e) => return (400, json!({"ok": false, "error": format!("invalid config: {e}")})),
    };
    if let Err(e) = validate(&cfg) {
        return (400, json!({"ok": false, "error": e}));
    }
    {
        let mut guard = state.scenario.cfg.lock().unwrap();
        *guard = cfg.clone();
    }
    write_json(CONFIG_FILE, &cfg);
    log_line("configuration replaced");
    (200, json!({"ok": true, "data": state_json(&state.scenario)}))
}

/// GET /api/scenario/template — a starting configuration, with no home
/// networks filled in so it cannot trigger until the user names one.
pub fn scenario_template(_state: &AppState) -> (u16, Value) {
    (200, json!({"ok": true, "data": template()}))
}

/// POST /api/scenario/pin — body `{"id": "home"}` or `{"id": null}`.
pub fn scenario_pin(state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let _ = fs::create_dir_all(DIR);
    match parsed.get("id").and_then(|v| v.as_str()) {
        Some(id) => {
            let known = state
                .scenario
                .cfg
                .lock()
                .unwrap()
                .scenarios
                .iter()
                .any(|s| s.id == id);
            if !known {
                return (400, json!({"ok": false, "error": format!("unknown scenario {id:?}")}));
            }
            let _ = fs::write(PIN_FILE, id);
            log_line(&format!("pinned to {id:?}"));
        }
        None => {
            let _ = fs::remove_file(PIN_FILE);
            log_line("pin cleared");
        }
    }
    (200, json!({"ok": true, "data": state_json(&state.scenario)}))
}

/// PUT /api/scenario/enabled — body `{"enabled": bool}`.
///
/// Disabling does not freeze the device where it stands: it restores the away
/// scenario first. Freezing would be a trap — disable it while the APs are off
/// and you would have locked yourself out of the network you were trying to get
/// back.
pub fn scenario_enabled(state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let enabled = parsed.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
    let _ = fs::create_dir_all(DIR);
    if enabled {
        let _ = fs::remove_file(DISABLED_FLAG);
        log_line("engine enabled");
    } else {
        let _ = fs::write(DISABLED_FLAG, "");
        log_line("engine disabled — restoring away scenario");
        state.scenario.bootsafe(state);
        state.scenario.run_restores(state, None, false);
    }
    (200, json!({"ok": true, "data": state_json(&state.scenario)}))
}

/// POST /api/scenario/apply — body `{"id": "home"}`. Apply one scenario now.
///
/// Human-triggered, synchronous, and it reports what actually happened. Exists
/// for two reasons: the admin UI needs an "apply now" that is not a guess, and
/// a new build can be exercised on the device without turning the autonomous
/// loop loose on it. Detection is bypassed; hysteresis is not consulted.
pub fn scenario_apply(state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let Some(id) = parsed.get("id").and_then(|v| v.as_str()) else {
        return (400, json!({"ok": false, "error": "expected {\"id\": \"...\"}"}));
    };
    let cfg = { state.scenario.cfg.lock().unwrap().clone() };
    if !cfg.scenarios.iter().any(|s| s.id == id) {
        return (404, json!({"ok": false, "error": format!("unknown scenario {id:?}")}));
    }
    log_line(&format!("manual apply requested: {id:?}"));
    state.scenario.switch_to(state, &cfg, id);
    let data = state_json(&state.scenario);
    let failed = data
        .get("last_error")
        .map(|e| !e.is_null())
        .unwrap_or(false)
        && data.get("current").and_then(|c| c.as_str()) != Some(id);
    let status = if failed { 503 } else { 200 };
    (status, json!({"ok": !failed, "data": data}))
}

/// GET /api/scenario/scan — what the detector can see right now. The helper the
/// admin UI needs to let someone pick their home network (with its BSSID)
/// instead of typing a name.
pub fn scenario_scan(_state: &AppState) -> (u16, Value) {
    match wifi_scan::scan() {
        Ok(nets) => {
            let list: Vec<Value> = nets.iter().map(|n| n.to_json()).collect();
            (200, json!({"ok": true, "data": {"networks": list}}))
        }
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

/// GET /api/scenario/log
pub fn scenario_log(_state: &AppState) -> (u16, Value) {
    let text = fs::read_to_string(LOG_FILE).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(LOG_TAIL_LINES);
    (200, json!({"ok": true, "data": {"log": lines[start..].join("\n")}}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(ssid: &str, bssid: &str, signal: f64) -> wifi_scan::Network {
        wifi_scan::Network {
            ssid: ssid.into(),
            bssid: bssid.into(),
            signal,
        }
    }

    fn cfg_with(entries: Vec<SsidEntry>) -> Config {
        let mut c = template();
        if let Some(home) = c.scenarios.iter_mut().find(|s| s.id == "home") {
            home.detect = Detect::Ssid { entries };
        }
        c
    }

    #[test]
    fn unconfigured_engine_matches_nothing_but_fallback() {
        let cfg = cfg_with(Vec::new());
        let got = detect(&cfg, &[net("Someone", "02:00:00:00:00:aa", -30.0)], None);
        assert_eq!(got.as_deref(), Some(AWAY_ID));
    }

    #[test]
    fn weak_signal_does_not_count_as_home() {
        let cfg = cfg_with(vec![SsidEntry {
            ssid: "MyHome".into(),
            bssid: None,
        }]);
        // Same name, far away — a neighbour, or someone spoofing it.
        let got = detect(&cfg, &[net("MyHome", "02:00:00:00:00:aa", -85.0)], None);
        assert_eq!(got.as_deref(), Some(AWAY_ID));
    }

    #[test]
    fn strong_signal_counts_as_home_when_no_bssid_pinned() {
        let cfg = cfg_with(vec![SsidEntry {
            ssid: "MyHome".into(),
            bssid: None,
        }]);
        let got = detect(&cfg, &[net("MyHome", "02:00:00:00:00:aa", -45.0)], None);
        assert_eq!(got.as_deref(), Some("home"));
    }

    #[test]
    fn pinned_bssid_beats_a_strong_impostor() {
        let cfg = cfg_with(vec![SsidEntry {
            ssid: "MyHome".into(),
            bssid: Some("02:00:00:00:00:AA".into()),
        }]);
        // Right name, loud, wrong radio: must not be treated as home.
        let got = detect(&cfg, &[net("MyHome", "02:00:00:00:00:bb", -20.0)], None);
        assert_eq!(got.as_deref(), Some(AWAY_ID));
        // Right radio, case-insensitively, even when weak.
        let got = detect(&cfg, &[net("MyHome", "02:00:00:00:00:aa", -88.0)], None);
        assert_eq!(got.as_deref(), Some("home"));
    }

    #[test]
    fn hysteresis_needs_two_consistent_readings_to_enter() {
        let p = Params::default(); // enter_hits = 2, exit_misses = 2
        let mut st = RunState::default();
        // Fresh boot, nothing applied: one sighting is not enough.
        assert_eq!(decide(&mut st, "home", &p), None);
        assert_eq!(decide(&mut st, "home", &p).as_deref(), Some("home"));
    }

    #[test]
    fn a_single_stray_reading_does_not_move_the_device() {
        let p = Params::default();
        let mut st = RunState::default();
        st.current = "home".into();
        // One "away" reading between two "home" ones must not turn Wi-Fi back
        // on — that would bounce every client for a momentary mis-scan.
        assert_eq!(decide(&mut st, "away", &p), None);
        assert_eq!(decide(&mut st, "home", &p), None);
        assert_eq!(st.hits, 0, "a confirming reading resets the candidate");
        assert_eq!(decide(&mut st, "away", &p), None);
    }

    #[test]
    fn leaving_home_takes_two_consecutive_misses_then_switches() {
        let p = Params::default();
        let mut st = RunState::default();
        st.current = "home".into();
        assert_eq!(decide(&mut st, "away", &p), None);
        assert_eq!(decide(&mut st, "away", &p).as_deref(), Some("away"));
    }

    #[test]
    fn rejects_paths_outside_the_allow_list() {
        let mut c = template();
        c.scenarios[0].actions[0].action.path = "/api/device/reboot".into();
        let err = validate(&c).unwrap_err();
        assert!(err.contains("not allowed"), "{err}");
    }

    #[test]
    fn rejects_two_fallback_scenarios() {
        let mut c = template();
        c.scenarios[0].detect = Detect::Fallback;
        assert!(validate(&c).is_err());
    }

    #[test]
    fn template_is_valid_and_inert() {
        let c = template();
        validate(&c).unwrap();
        // The home scenario ships with no networks, so it can never fire until
        // the user names one — nobody inherits someone else's SSID list.
        match &c.scenarios[0].detect {
            Detect::Ssid { entries } => assert!(entries.is_empty()),
            _ => panic!("first scenario should detect by ssid"),
        }
    }

    #[test]
    fn home_network_beats_a_foreign_sim() {
        // At home on a foreign eUICC profile: the house network still wins.
        let cfg = cfg_with(vec![SsidEntry {
            ssid: "MyHome".into(),
            bssid: Some("02:00:00:00:00:aa".into()),
        }]);
        let nets = [net("MyHome", "02:00:00:00:00:aa", -40.0)];
        assert_eq!(detect(&cfg, &nets, Some("440")).as_deref(), Some("home"));
    }

    fn mcc_scenario(id: &str, mccs: &[&str]) -> Scenario {
        Scenario {
            id: id.into(),
            name: id.into(),
            detect: Detect::Mcc {
                mccs: mccs.iter().map(|m| m.to_string()).collect(),
            },
            inhibit_sleep: false,
            actions: vec![wifi_action(true)],
        }
    }

    #[test]
    fn any_foreign_sim_is_abroad() {
        let cfg = template();
        assert_eq!(detect(&cfg, &[], Some("440")).as_deref(), Some("abroad"));
        assert_eq!(detect(&cfg, &[], Some("520")).as_deref(), Some("abroad"));
    }

    #[test]
    fn a_hand_added_mcc_scenario_beats_the_catch_all() {
        // Not shipped, but the config allows per-country scenarios, and the
        // catch-all must not shadow them whatever the order.
        let mut cfg = template();
        cfg.scenarios.insert(0, mcc_scenario("japan", &["440", "441"]));
        let i = cfg.scenarios.iter().position(|s| s.id == "abroad").unwrap();
        let catch_all = cfg.scenarios.remove(i);
        cfg.scenarios.insert(0, catch_all);
        validate(&cfg).unwrap();
        assert_eq!(detect(&cfg, &[], Some("441")).as_deref(), Some("japan"));
        assert_eq!(detect(&cfg, &[], Some("520")).as_deref(), Some("abroad"));
    }

    #[test]
    fn home_mcc_or_unknown_sim_is_away() {
        let cfg = template();
        assert_eq!(detect(&cfg, &[], Some("460")).as_deref(), Some(AWAY_ID));
        assert_eq!(detect(&cfg, &[], None).as_deref(), Some(AWAY_ID));
    }

    #[test]
    fn empty_or_unready_sim_has_no_mcc() {
        let ok = json!({"sim_states": "sim ready", "sim_imsi": "440101234567890"});
        assert_eq!(mcc_from_sim_info(&ok).as_deref(), Some("440"));
        let empty = json!({"sim_states": "sim ready", "sim_imsi": ""});
        assert_eq!(mcc_from_sim_info(&empty), None);
        let not_ready = json!({"sim_states": "sim absent", "sim_imsi": "440101234567890"});
        assert_eq!(mcc_from_sim_info(&not_ready), None);
        let junk = json!({"sim_states": "sim ready", "sim_imsi": "44x10"});
        assert_eq!(mcc_from_sim_info(&junk), None);
    }

    #[test]
    fn abroad_goes_direct_best_effort_and_remembers_the_old_value() {
        let cfg = template();
        let abroad = cfg.scenarios.iter().find(|s| s.id == "abroad").unwrap();
        assert_eq!(abroad.actions[0].action.path, "/api/wifi/radio");
        assert!(!abroad.actions[0].best_effort);
        let group = &abroad.actions[1];
        assert_eq!(group.action.path, "/api/services/chill/regions");
        assert_eq!(group.action.body.as_ref().unwrap()["member"], "DIRECT");
        assert!(group.best_effort);
        assert!(group.verify_job.is_none(), "a region switch starts no job");
        assert_eq!(group.verify_field.as_ref().unwrap().equals, json!("DIRECT"));
        assert!(group.restore_on_exit.is_some());
    }

    #[test]
    fn restore_puts_the_saved_member_back_and_verifies_it() {
        let a = chill_main_group_action("DIRECT");
        let back = restore_action(&a, json!("🇹🇼 台湾")).unwrap();
        assert_eq!(back.action.body.as_ref().unwrap()["member"], "🇹🇼 台湾");
        assert_eq!(back.action.body.as_ref().unwrap()["group"], CHILL_MAIN_GROUP);
        assert_eq!(back.verify_field.as_ref().unwrap().equals, json!("🇹🇼 台湾"));
        assert!(back.restore_on_exit.is_none(), "a restore must not save again");
        assert!(back.best_effort);
    }

    #[test]
    fn same_group_different_member_shares_a_restore_key() {
        let k1 = restore_key(&chill_main_group_action("DIRECT"));
        let k2 = restore_key(&chill_main_group_action("🇯🇵 日本"));
        assert!(k1.is_some());
        assert_eq!(k1, k2);
        assert_eq!(restore_key(&wifi_action(true)), None);
    }

    #[test]
    fn restore_is_due_on_leaving_abroad_but_not_while_in_it() {
        let cfg = template();
        let a = chill_main_group_action("DIRECT");
        let pending = vec![PendingRestore {
            key: restore_key(&a).unwrap(),
            action: restore_action(&a, json!("🇹🇼 台湾")).unwrap(),
            saved_at: 0,
            while_abroad: false,
        }];
        let by = |id: &str| cfg.scenarios.iter().find(|s| s.id == id);
        assert!(due_restores(&pending, by("abroad")).is_empty());
        assert_eq!(due_restores(&pending, by("away")).len(), 1);
        assert_eq!(due_restores(&pending, by("home")).len(), 1);
        assert_eq!(due_restores(&pending, None).len(), 1, "engine off restores too");
    }

    #[test]
    fn chill_switched_off_abroad_comes_back_only_when_home_and_first() {
        let cfg = template();
        let by = |id: &str| cfg.scenarios.iter().find(|s| s.id == id);
        let a = chill_main_group_action("DIRECT");
        let pending = vec![
            PendingRestore {
                key: restore_key(&a).unwrap(),
                action: restore_action(&a, json!("🇹🇼 台湾")).unwrap(),
                saved_at: 0,
                while_abroad: false,
            },
            chill_on_restore(0),
        ];
        assert!(due_restores(&pending, by("abroad")).is_empty(), "still abroad: CHILL stays off");
        let due = due_restores(&pending, by("away"));
        assert_eq!(due.len(), 2);
        assert_eq!(due[0], CHILL_ON_KEY, "CHILL on before its group is set");
        assert_eq!(due_restores(&pending, by("home"))[0], CHILL_ON_KEY);
        assert_eq!(due_restores(&pending, None)[0], CHILL_ON_KEY, "engine off restores too");
        let mut with_exit = pending.clone();
        with_exit.insert(0, chill_exit_restore(0));
        let due = due_restores(&with_exit, by("away"));
        assert_eq!(due.first().map(String::as_str), Some(CHILL_ON_KEY));
        assert_eq!(due.last().map(String::as_str), Some(CHILL_EXIT_KEY), "mode last, after the node");
        assert!(due_restores(&with_exit, by("abroad")).is_empty());
        // A per-country scenario is abroad too.
        let mcc = Scenario {
            id: "jp".into(),
            name: "日本".into(),
            detect: Detect::Mcc { mccs: vec!["440".into()] },
            actions: Vec::new(),
            inhibit_sleep: false,
        };
        assert_eq!(due_restores(&pending, Some(&mcc)), vec![restore_key(&a).unwrap()]);
    }

    /// A whole trip, as the engine and the handlers would record it, checked at
    /// every stage: what is waiting, and what comes back in which order.
    #[test]
    fn simulated_trip_abroad_and_back() {
        let cfg = template();
        let by = |id: &str| cfg.scenarios.iter().find(|s| s.id == id);
        let engine_in = |current: &str| Engine {
            cfg: Mutex::new(template()),
            state: Mutex::new(RunState { current: current.into(), ..Default::default() }),
            busy: AtomicBool::new(false),
            takeover: Mutex::new(None),
        };

        // At home / away: turning CHILL off or changing its exit records nothing.
        assert!(!in_abroad_scenario(&engine_in("away")));
        assert!(!in_abroad_scenario(&engine_in("home")));
        assert!(!in_abroad_scenario(&engine_in("")), "just booted, nothing decided yet");

        // Arrive on a local SIM: the abroad scenario sets the main group to
        // DIRECT and saves the node it replaces.
        assert!(in_abroad_scenario(&engine_in("abroad")));
        let abroad = by("abroad").unwrap();
        let group = abroad.actions.iter().find(|a| a.restore_on_exit.is_some()).unwrap();
        assert_eq!(group.action.body.as_ref().unwrap()["member"], "DIRECT");
        let mut pending = vec![PendingRestore {
            key: restore_key(group).unwrap(),
            action: restore_action(group, json!("🇹🇼 台湾")).unwrap(),
            saved_at: 1,
            while_abroad: false,
        }];

        // Abroad the owner picks "all direct", then switches CHILL off.
        pending.push(chill_exit_restore(2));
        pending.push(chill_on_restore(3));

        // Still abroad (also after a reboot there, once the engine decides
        // "abroad" again): nothing comes back.
        assert!(due_restores(&pending, Some(abroad)).is_empty());

        // Home SIM again: CHILL on, then the pre-trip node, then proxy mode.
        for back in ["away", "home"] {
            let due = due_restores(&pending, by(back));
            assert_eq!(due.len(), 3, "{back}");
            assert_eq!(due[0], CHILL_ON_KEY);
            let node = pending.iter().find(|p| p.key == due[1]).unwrap();
            assert_eq!(node.action.action.body.as_ref().unwrap()["member"], "🇹🇼 台湾");
            let exit = pending.iter().find(|p| p.key == due[2]).unwrap();
            assert_eq!(exit.action.action.path, "/api/services/chill/exit");
            assert_eq!(exit.action.action.body.as_ref().unwrap()["state"], "proxy");
            assert!(pending.iter().all(|p| p.action.best_effort), "a failed restore is retried, never rolls back Wi-Fi");
        }
    }

    #[test]
    fn chill_on_restore_waits_for_the_job_and_survives_a_round_trip() {
        let r = chill_on_restore(5);
        assert_eq!(r.action.action.path, "/api/services/chill/enable");
        assert!(ALLOWED_PATHS.contains(&r.action.action.path.as_str()));
        assert_eq!(r.action.verify_job.as_deref(), Some("/api/services/chill/job"));
        assert!(r.action.best_effort);
        let text = serde_json::to_string(&vec![r]).unwrap();
        let back: Vec<PendingRestore> = serde_json::from_str(&text).unwrap();
        assert!(back[0].while_abroad);
        // Entries written before this field existed still parse, as not-abroad.
        let old: Vec<PendingRestore> = serde_json::from_str(
            &text.replace(",\"while_abroad\":true", ""),
        )
        .unwrap();
        assert!(!old[0].while_abroad);
    }

    #[test]
    fn rejects_best_effort_wifi() {
        let mut c = template();
        c.scenarios[0].actions[0].best_effort = true;
        assert!(validate(&c).unwrap_err().contains("best_effort"));
    }

    #[test]
    fn rejects_an_mcc_scenario_that_includes_home() {
        let mut c = template();
        c.scenarios.push(Scenario {
            id: "oops".into(),
            name: "oops".into(),
            detect: Detect::Mcc { mccs: vec!["460".into()] },
            inhibit_sleep: false,
            actions: Vec::new(),
        });
        assert!(validate(&c).unwrap_err().contains("home MCC"));
    }

    #[test]
    fn config_written_before_abroad_support_still_loads() {
        // Shape of a phase-one config on a real device (names made up): no
        // home_mcc, no best_effort, no verify_field. If this stopped parsing,
        // read_json would silently fall back to an empty config and disarm
        // the engine — and the UI would then offer to overwrite it.
        let old = r#"{
          "version": 1,
          "params": {"scan_interval_home_secs": 60, "scan_interval_away_battery_secs": 300,
                     "scan_interval_away_charging_secs": 60, "enter_hits": 2,
                     "exit_misses": 2, "min_rssi_dbm": -70.0},
          "scenarios": [
            {"id": "home", "name": "在家", "inhibit_sleep": true,
             "detect": {"type": "ssid", "entries": [{"ssid": "MyHome", "bssid": "02:00:00:00:00:aa"}]},
             "actions": [{"method": "PUT", "path": "/api/wifi/radio",
                          "body": {"ap_2g": false, "ap_5g": false},
                          "snapshot": ["wireless.main_2g.disabled", "wireless.main_5g.disabled"]}]},
            {"id": "away", "name": "外出", "inhibit_sleep": false,
             "detect": {"type": "fallback"},
             "actions": [{"method": "PUT", "path": "/api/wifi/radio",
                          "body": {"ap_2g": true, "ap_5g": true},
                          "snapshot": ["wireless.main_2g.disabled", "wireless.main_5g.disabled"]}]}
          ]
        }"#;
        let cfg: Config = serde_json::from_str(old).unwrap();
        validate(&cfg).unwrap();
        assert_eq!(cfg.home_mcc, "460");
        assert_eq!(cfg.scenarios.len(), 2);
        assert!(!cfg.scenarios[0].actions[0].best_effort);
        // And a SIM from abroad on an old config simply means away.
        assert_eq!(detect(&cfg, &[], Some("440")).as_deref(), Some(AWAY_ID));
    }

    #[test]
    fn uptime_is_whole_seconds_of_the_first_field() {
        assert_eq!(uptime_secs("12345.67 54321.00\n"), Some(12345));
        assert_eq!(uptime_secs("7 1"), Some(7));
        assert_eq!(uptime_secs(""), None);
        assert_eq!(uptime_secs("garbage"), None);
    }

    #[test]
    fn takeover_holds_for_the_full_period_then_releases_once() {
        let hold = Duration::from_secs(600);
        let t0 = Instant::now();
        let mut seen = None;

        assert_eq!(takeover_state(None, &mut seen, t0, hold), Takeover::None);
        assert!(seen.is_none());

        assert_eq!(takeover_state(Some("100"), &mut seen, t0, hold), Takeover::Active);
        let later = t0 + Duration::from_secs(599);
        assert_eq!(takeover_state(Some("100"), &mut seen, later, hold), Takeover::Active);
        let done = t0 + hold;
        assert_eq!(takeover_state(Some("100"), &mut seen, done, hold), Takeover::Release);
        // The caller deletes the file; had that failed, the same marker starts a
        // fresh hold rather than releasing every tick.
        assert_eq!(takeover_state(Some("100"), &mut seen, done, hold), Takeover::Active);
    }

    #[test]
    fn a_new_takeover_restarts_the_clock() {
        let hold = Duration::from_secs(600);
        let t0 = Instant::now();
        let mut seen = None;
        takeover_state(Some("100"), &mut seen, t0, hold);
        let t1 = t0 + Duration::from_secs(500);
        // u60-guard stepped in again (we hung again) and rewrote the marker.
        assert_eq!(takeover_state(Some("900"), &mut seen, t1, hold), Takeover::Active);
        assert_eq!(takeover_state(Some("900"), &mut seen, t0 + hold, hold), Takeover::Active);
        assert_eq!(takeover_state(Some("900"), &mut seen, t1 + hold, hold), Takeover::Release);
    }

    #[test]
    fn marker_removed_by_hand_ends_the_takeover() {
        let hold = Duration::from_secs(600);
        let t0 = Instant::now();
        let mut seen = None;
        takeover_state(Some("100"), &mut seen, t0, hold);
        assert_eq!(takeover_state(None, &mut seen, t0, hold), Takeover::None);
        assert!(seen.is_none());
    }

    #[test]
    fn fallback_may_not_turn_wifi_off() {
        let mut c = template();
        let away = c.scenarios.iter_mut().find(|s| s.id == AWAY_ID).unwrap();
        away.actions = vec![wifi_action(false)];
        assert!(validate(&c).unwrap_err().contains("fallback"));
    }
}
