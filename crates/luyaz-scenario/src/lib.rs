//! scenario — "where am I, and what should the device look like here".
//!
//! A scenario is a detection rule plus an ordered list of actions. The engine
//! samples the surroundings, decides which scenario holds, and applies that
//! scenario's actions once on entry. `home` (Wi-Fi off, the phone roams onto the
//! house network), `away` (the catch-all), and `abroad` (a foreign SIM in the
//! slot: CHILL's main group goes DIRECT, and whatever it was before comes back
//! when you return — see `restore_on_exit`).
//!
//! The core runs this engine; whichever process owns it runs it (the handover list, the lease
//! [`LEASE_FILE`]). The decisions are u60-policy's `scenario::step`, called through
//! [`Io::step`] so the core can hand it to its policy component. Everything that touches the device goes through the host's [`Io`].
//!
//! Design notes that are easy to get wrong and expensive to rediscover:
//!
//!  * **The device is someone's only internet connection.** Every failure path
//!    ends at the away scenario with Wi-Fi on. Boot resets to away. An empty or
//!    unreadable config means "do nothing", never "apply something".
//!  * **A wall clock, not a sleep counter.** In the home scenario the device is
//!    allowed to suspend; `thread::sleep` does not advance across suspend, so
//!    counting ticks would silently stop time. Elapsed real seconds decide when
//!    a scan is due.
//!  * **The detector never writes configuration.** It scans through a throwaway
//!    vdev (`luyaz_wifi::scan`). That matters for the override latch below: if the
//!    detector changed uci, the engine would see its own edits and conclude the
//!    user had taken over.
//!  * **HTTP 200 does not mean the device changed.** Wireless actions therefore
//!    go through the Wi-Fi switch (`luyaz_wifi::apply`), which polls until the
//!    change is observable, instead of through the route table.
//!  * **Never route a call while holding a lock a handler could want.** The
//!    Wi-Fi switch takes the Wi-Fi lock itself, so the engine must not already
//!    hold it — hence the direct call rather than a routed one. Code that does
//!    hold it (rollback) uses the locked variant.
//!  * **Nothing here may panic.** u60d is built with `panic = "abort"`: a panic on
//!    the engine thread would take the whole data service with it. Poisoned locks
//!    are taken anyway ([`lk`]); no `unwrap` on anything that can fail.

use std::collections::VecDeque;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};
use luyaz_policy::scenario::{
    self as sc, Branch, Config, Intent, Network, PendingRestore, Recheck, RunState, Scan, Scenario, ScenarioAction,
    Takeover, TickInput, TickOutcome, AWAY_ID,
};

mod http;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
mod views;

pub use luyaz_policy::scenario::{Json, ALLOWED_PATHS};
/// The decisions (u60-policy `scenario`), for hosts that need its helpers (`sim_mcc_from`).
pub use luyaz_policy::scenario as policy;

pub const DIR: &str = "/data/scenario";
pub const CONFIG_FILE: &str = "/data/scenario/scenarios.json";
pub const STATE_FILE: &str = "/data/scenario/state.json";
pub const PIN_FILE: &str = "/data/scenario/pin";
pub const DISABLED_FLAG: &str = "/data/scenario/disabled";
pub const LOG_FILE: &str = "/data/scenario/log";
/// Values to put back on leaving a scenario. Kept apart from `state.json`
/// because boot clears that, and a reboot abroad must not turn "what it was
/// before the trip" into "what it is now".
pub const RESTORE_FILE: &str = "/data/scenario/restore.json";
const LOG_MAX_BYTES: u64 = 1_048_576;
const LOG_TAIL_LINES: usize = 300;

/// Held by u60d while its engine runs (u60-api `lease`, as SMS forwarding and manual-first): the
/// agent's engine waits for it to go before starting, so the two never tick at once.
pub const LEASE_FILE: &str = "/tmp/scenario.owner";

// ── Contract with the sentinel's Wi-Fi guard (luyaz-sentinel) ────────────────
//
// The Wi-Fi guard is the watchdog that forces Wi-Fi on when the engine stops
// working while the APs are down. It knows the engine only through these two
// files, both on tmpfs so a reboot starts clean:
//
//  * HEARTBEAT_FILE — written by the engine thread before bootsafe and then on
//    every loop, whether or not the engine is enabled or configured: it proves
//    the engine is alive, not that it is switching. Content is whole seconds of
//    /proc/uptime (monotonic, immune to the clock jump when the network sets
//    the time). The guard only starts judging once it has seen it at least once.
//    Whichever process runs the engine writes it (the core).
//  * TAKEOVER_FILE — written by the guard when it steps in. While it exists the
//    engine does no detection and ignores the pin, and goes back to (and stays
//    in) away: a persistent pin of "home" would otherwise take the APs straight
//    back down. The engine removes it after TAKEOVER_HOLD of its own continuous
//    running, which is how control is handed back. If the guard rewrites it
//    (a second takeover), the clock starts again.
//
// Which is why the guard must write the marker ONCE per takeover, with content
// unique to that takeover (its uptime at the time) — rewriting it every check
// would restart the hold forever and control would never come back.
pub const HEARTBEAT_FILE: &str = "/tmp/scenario.heartbeat";
pub const TAKEOVER_FILE: &str = "/tmp/luyaz/wifiguard.took-over";
/// Uptime past which bootsafe treats its start as a core restart, not a boot.
const RESTART_MIN_UPTIME: u64 = 300;
/// The agent's tick records (`/tmp/scenario.tick.json`), read by u60d's shadow.
pub const TICK_FILE: &str = "/tmp/scenario.tick.json";
const TICK_RING: usize = 8;

/// The longest an engine thread legitimately goes without a heartbeat: a switch's restores (each
/// up to ~60 s with its checks), the apply (150 s for the lock, 30 s reload, 45 s for the APs) and
/// a rollback after it (the same again), with room. u60d's watchdog exits past it.
pub const WEDGED: Duration = Duration::from_secs(900);
/// How long an engine starting after a hand-back waits for the other's lease: past [`WEDGED`] and
/// a look, the other has either stopped or been ended by its watchdog (its pid gone).
pub const HANDBACK_WAIT: Duration = Duration::from_secs(960);

/// How often the engine wakes to *consider* working. The decision to actually
/// scan is made against the wall clock, so this only bounds responsiveness.
pub const TICK_SECS: u64 = 15;
/// A scan that does not see the current Wi-Fi-detected scenario is checked
/// once more after this, before it counts as a miss. 10-07 on the device: the
/// home AP at -26 dBm went unseen by two scans a minute apart and the engine
/// left home, turning Wi-Fi on for six minutes. Scans on this chip sometimes
/// come back partial; a second look a few seconds later costs nothing.
const MISS_RECHECK_DELAY: Duration = Duration::from_secs(sc::MISS_RECHECK_DELAY_SECS);

/// Vendor sleep daemon. Writing `/sys/power/wake_lock` does NOT hold this
/// device awake — the daemon forces suspend with an explicit `echo mem >
/// /sys/power/state` that ignores the kernel wakelock set. Its own ubus switch
/// is the only thing it honours.
pub const SLEEP_UBUS_OBJ: &str = "zwrt_zte_sleep_faw.wakelock";
const RTC_WAKEALARM: &str = "/sys/class/rtc/rtc0/wakealarm";
const RTC_SINCE_EPOCH: &str = "/sys/class/rtc/rtc0/since_epoch";

/// Take a lock even when a panicking thread poisoned it (nothing here may panic: see the module
/// docs).
pub(crate) fn lk<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// What the engine needs from the process it runs in. Called from the engine's own thread (and,
/// for the web routes that act, from the request's): every method may block.
///
/// The Wi-Fi methods default to the real switch (`luyaz_wifi`) through [`Io::wifi`]; tests override
/// them. Paths go through [`Io::path`] and every wait through [`Io::sleep`], so a host can run the
/// engine under a test root on a fast clock.
pub trait Io: Send + Sync + 'static {
    /// A device path as this process sees it.
    fn path(&self, p: &str) -> String {
        p.to_string()
    }

    /// Wall-clock seconds.
    fn now(&self) -> i64 {
        // SAFETY: time(NULL) has no preconditions.
        unsafe { libc::time(std::ptr::null_mut()) as i64 }
    }

    /// Monotonic seconds (the takeover clock).
    fn mono(&self) -> f64 {
        static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        T0.get_or_init(Instant::now).elapsed().as_secs_f64()
    }

    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }

    /// False once the engine has been taken back: the loop ends between ticks (never inside a
    /// switch or a rollback, which always finish).
    fn active(&self) -> bool {
        true
    }

    /// The wait between ticks: the only one a host may cut short once taken back ([`Io::sleep`]
    /// is also every wait inside a switch, which must run its full length).
    fn idle(&self, d: Duration) {
        self.sleep(d);
    }

    /// Whole seconds of `/proc/uptime`, for the heartbeat.
    fn uptime_secs(&self) -> Option<u64> {
        uptime_secs(&fs::read_to_string("/proc/uptime").ok()?)
    }

    /// Nearby networks (`luyaz_wifi::scan`).
    fn scan(&self) -> Result<Vec<Network>, String> {
        luyaz_wifi::scan::scan().map(|nets| {
            nets.into_iter()
                .map(|n| Network { ssid: n.ssid, bssid: n.bssid, signal: n.signal })
                .collect()
        })
    }

    /// The Wi-Fi switch's writes and uci reads.
    fn wifi(&self) -> &dyn luyaz_wifi::Host;

    fn beaconing(&self) -> bool {
        luyaz_wifi::beaconing()
    }

    fn wifi_fully_on(&self) -> bool {
        luyaz_wifi::fully_on(self.wifi())
    }

    fn wifi_snapshot(&self) -> (bool, bool) {
        luyaz_wifi::snapshot(self.wifi())
    }

    /// Both AP flags, written, reloaded and verified (takes the Wi-Fi lock).
    fn wifi_apply(&self, ap_2g: bool, ap_5g: bool) -> Result<(), String> {
        luyaz_wifi::apply(self.wifi(), ap_2g, ap_5g).map(|_| ())
    }

    fn wifi_lock(&self, wait: Duration) -> Result<luyaz_wifi::WifiLock, String> {
        luyaz_wifi::lock(wait)
    }

    fn wifi_apply_locked(&self, lk: &luyaz_wifi::WifiLock, ap_2g: bool, ap_5g: bool) -> Result<(), String> {
        luyaz_wifi::apply_locked(self.wifi(), lk, ap_2g, ap_5g).map(|_| ())
    }

    /// Rollback: write the snapshotted uci values back without a reload, best effort
    /// (`wifi.apply` `{set, reload: false, best_effort: true}`); the forced "on" after it reloads.
    fn wifi_put_back(&self, set: serde_json::Map<String, Value>);

    fn uci_get(&self, key: &str) -> Option<String> {
        self.wifi().uci_get(key)
    }

    /// Run one scenario action (one of [`ALLOWED_PATHS`] other than the Wi-Fi switch), routed
    /// inside the process, its device writes under source `scenario`. `Err` = refused or failed
    /// (HTTP status ≥ 400, its `error`).
    fn exec(&self, method: &str, path: &str, body: &[u8]) -> Result<(), String>;

    /// A GET routed inside the process (`verify_job`, `verify_field`, `restore_on_exit.read_path`).
    fn get(&self, path: &str) -> (u16, Value);

    /// The vendor sleep daemon's switch ([`SLEEP_UBUS_OBJ`] `enableAutoSleep`).
    fn set_auto_sleep(&self, enabled: bool);

    /// Charger connected; `None` when unknown (counts as not charging).
    fn charging(&self) -> Option<bool>;

    /// The SIM's home MCC (`sc::sim_mcc_from`).
    fn sim_mcc(&self) -> Option<String>;

    /// The tick records after a new one, as [`TICK_FILE`] holds them (the last eight, newest
    /// last): the agent writes the file for u60d's shadow; nobody else needs them.
    fn publish(&self, _ring: &str) {}

    /// The decisions: u60-policy's built-in step, or a host's component of it.
    fn step(&self, i: &TickInput) -> TickOutcome {
        sc::step(i)
    }
}

/// Whole seconds from the text of /proc/uptime ("12345.67 54321.00").
pub fn uptime_secs(text: &str) -> Option<u64> {
    text.split_whitespace().next()?.split('.').next()?.parse().ok()
}

// ── files ───────────────────────────────────────────────────────────────────

/// Missing or unparsable file → the type's default.
fn read_json<T: DeserializeOwned + Default>(path: &str) -> T {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => T::default(),
    }
}
/// Write `path.tmp`, then rename it over `path`: a crash mid-write leaves the old file whole.
/// The old file's mode is kept (the agent's `fsutil::atomic_write`).
pub(crate) fn atomic_write(path: &str, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt;
    let keep = fs::metadata(path).ok().map(|m| m.permissions().mode() & 0o7777);
    let tmp = format!("{path}.tmp");
    {
        let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).open(&tmp)?;
        if let Some(m) = keep {
            f.set_permissions(fs::Permissions::from_mode(m))?;
        }
        f.write_all(data)?;
    }
    fs::rename(&tmp, path)
}

/// The last [`TICK_RING`] tick records, newest last, rewritten whole on each one (the agent's
/// publish; u60d's shadow reads the file).
#[derive(Default)]
pub struct TickRing {
    seq: u64,
    ring: VecDeque<Value>,
}

impl TickRing {
    /// Stamp `rec` (`seq` from 1, `at`) and return the file's new text.
    pub fn push(&mut self, mut rec: Value, at: i64) -> String {
        self.seq += 1;
        rec["seq"] = json!(self.seq);
        rec["at"] = json!(at);
        while self.ring.len() >= TICK_RING {
            self.ring.pop_front();
        }
        self.ring.push_back(rec);
        serde_json::to_string(&self.ring).unwrap_or_default()
    }
}

fn tick_state(st: &RunState) -> Value {
    json!({
        "current": st.current,
        "candidate": st.candidate,
        "hits": st.hits,
        "misses": st.misses,
        "last_scan": st.last_scan,
        "scan_failures": st.scan_failures,
    })
}

fn net_json(n: &Network) -> Value {
    json!({"ssid": n.ssid, "bssid": n.bssid, "signal": n.signal})
}

/// An action body as serde_json (the shape the routes take).
fn body_value(body: &Option<Json>) -> Option<Value> {
    body.as_ref().map(Value::from)
}

static RESTORE_LOCK: Mutex<()> = Mutex::new(());

thread_local! {
    // Set while run_restores puts something back, so a hook does not mistake
    // the restore for a change made by hand (CHILL switched on by its restore).
    static IN_RESTORE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Inside a restore on this thread (the agent forwards it as `X-U60-Restore`).
pub fn in_restore() -> bool {
    IN_RESTORE.with(|f| f.get())
}

pub fn set_in_restore(on: bool) {
    IN_RESTORE.with(|f| f.set(on));
}

// ── engine ──────────────────────────────────────────────────────────────────

pub struct Engine {
    io: Arc<dyn Io>,
    cfg: Mutex<Config>,
    state: Mutex<RunState>,
    /// A scan plus an apply can take tens of seconds. Without this, a slow tick
    /// would overlap the next one and two appliers would fight.
    busy: AtomicBool,
    /// Last restore retry while pinned (unix secs).
    pin_retry_at: AtomicI64,
    /// Which Wi-Fi guard takeover marker this process is honouring, and since when (monotonic).
    takeover: Mutex<Option<(String, f64)>>,
    tick_ring: Mutex<TickRing>,
    /// When the heartbeat was last written, on the monotonic clock (which stops in suspend, so
    /// a device that slept is not mistaken for a stuck engine; `/proc/uptime` keeps counting).
    beat: Mutex<Option<Instant>>,
    /// The config file as last read or written (mtime, length): a hand edit is picked up on the
    /// next tick, not only when the core restarts (10-20: a copied scenarios.json sat unread).
    cfg_seen: Mutex<FileStamp>,
}

type FileStamp = Option<(Option<std::time::SystemTime>, u64)>;

fn stamp(path: &str) -> FileStamp {
    fs::metadata(path).ok().map(|m| (m.modified().ok(), m.len()))
}

struct BusyGuard<'a>(&'a AtomicBool);
impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl Engine {
    /// The engine over its files. A config that no longer validates (an action this version does
    /// not allow any more) is kept as it is and said so: such an action fails when it runs, and
    /// the switch rolls back to away.
    pub fn new(io: Arc<dyn Io>) -> Engine {
        let cfg_seen = stamp(&io.path(CONFIG_FILE));
        let cfg: Config = read_json(&io.path(CONFIG_FILE));
        let state: RunState = read_json(&io.path(STATE_FILE));
        let e = Engine {
            io,
            cfg: Mutex::new(cfg),
            state: Mutex::new(state),
            busy: AtomicBool::new(false),
            pin_retry_at: AtomicI64::new(0),
            takeover: Mutex::new(None),
            tick_ring: Mutex::new(TickRing::default()),
            beat: Mutex::new(None),
            cfg_seen: Mutex::new(cfg_seen),
        };
        if let Err(err) = sc::validate(&lk(&e.cfg)) {
            e.log(&format!("configuration does not validate any more: {err}"));
        }
        e
    }

    pub fn io(&self) -> &Arc<dyn Io> {
        &self.io
    }

    fn p(&self, file: &str) -> String {
        self.io.path(file)
    }

    /// One line in the scenario log.
    pub fn log(&self, msg: &str) {
        log_line(&self.p(DIR), &self.p(LOG_FILE), msg);
    }

    fn write_json<T: Serialize>(&self, file: &str, value: &T) {
        let _ = fs::create_dir_all(self.p(DIR));
        if let Ok(text) = serde_json::to_string_pretty(value) {
            let _ = atomic_write(&self.p(file), text.as_bytes());
        }
    }

    fn write_state(&self, st: &RunState) {
        self.write_json(STATE_FILE, st);
    }

    fn pending(&self) -> Vec<PendingRestore> {
        read_json(&self.p(RESTORE_FILE))
    }

    /// Stamp one record and hand the ring to the host (the agent writes the file).
    fn publish(&self, rec: Value) {
        let text = lk(&self.tick_ring).push(rec, self.io.now());
        self.io.publish(&text);
    }

    /// Is the Wi-Fi guard in charge of Wi-Fi? True while its marker exists; once this
    /// process has run for the hold time with the same marker, removes it and
    /// hands control back to detection and the pin.
    fn guard_takeover(&self) -> bool {
        let marker = fs::read_to_string(self.p(TAKEOVER_FILE))
            .ok()
            .map(|s| s.trim().to_string());
        let mut seen = lk(&self.takeover);
        // A marker we are not already honouring: none before, or a new takeover.
        let fresh = seen.as_ref().map_or(true, |(c, _)| Some(c.as_str()) != marker.as_deref());
        match sc::takeover_state(marker.as_deref(), &mut seen, self.io.mono(), sc::TAKEOVER_HOLD_SECS) {
            Takeover::None => false,
            Takeover::Active => {
                if fresh {
                    self.log("Wi-Fi watchdog took over Wi-Fi: staying in away, pin paused");
                }
                true
            }
            Takeover::Release => {
                let _ = fs::remove_file(self.p(TAKEOVER_FILE));
                self.log("Wi-Fi watchdog takeover released after 10 min of steady running; pin and detection resume");
                false
            }
        }
    }

    /// For the status endpoints; no side effects.
    pub fn takeover_marked(&self) -> bool {
        Path::new(&self.p(TAKEOVER_FILE)).exists()
    }

    pub fn enabled(&self) -> bool {
        !Path::new(&self.p(DISABLED_FLAG)).exists()
    }

    pub fn pin(&self) -> Option<String> {
        fs::read_to_string(self.p(PIN_FILE))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// The config file changed since it was last read or written (by hand, or copied from the
    /// other device): take it if it reads and validates, else keep the one loaded and say so.
    /// Once per change either way. A file that went away keeps the loaded one too.
    pub fn reload_config(&self) {
        let path = self.p(CONFIG_FILE);
        let now = stamp(&path);
        {
            let mut seen = lk(&self.cfg_seen);
            if *seen == now {
                return;
            }
            *seen = now;
        }
        if now.is_none() {
            self.log("configuration file is gone; keeping the one loaded");
            return;
        }
        let parsed = fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| serde_json::from_str::<Config>(&t).map_err(|e| e.to_string()));
        match parsed {
            Err(e) => self.log(&format!("configuration file changed but does not read ({e}); keeping the one loaded")),
            Ok(cfg) => {
                if let Err(e) = sc::validate(&cfg) {
                    self.log(&format!("configuration file changed but does not validate ({e}); keeping the one loaded"));
                } else if *lk(&self.cfg) != cfg {
                    *lk(&self.cfg) = cfg;
                    self.log("configuration file changed; reloaded");
                }
            }
        }
    }

    pub fn config(&self) -> Config {
        lk(&self.cfg).clone()
    }

    pub fn run_state(&self) -> RunState {
        lk(&self.state).clone()
    }

    /// Whole seconds of uptime into the heartbeat file, renamed into place so the guard never
    /// reads a half-written number. No uptime, no heartbeat: the guard reads that as "not
    /// alive", the safe side.
    pub fn heartbeat(&self) {
        *lk(&self.beat) = Some(Instant::now());
        let Some(secs) = self.io.uptime_secs() else {
            return;
        };
        let _ = atomic_write(&self.p(HEARTBEAT_FILE), format!("{secs}\n").as_bytes());
    }

    /// Time since the last heartbeat on the monotonic clock (suspend not counted); `None` before
    /// the first.
    pub fn since_beat(&self) -> Option<Duration> {
        lk(&self.beat).map(|t| t.elapsed())
    }

    /// Bootsafe, then tick until taken back ([`Io::active`]). The host runs this on a thread of
    /// its own: `bootsafe` can take tens of seconds if it has to repair Wi-Fi, an apply up to a few
    /// minutes, and nothing else may wait for that. The heartbeat goes first, before bootsafe:
    /// The guard should see the engine alive as early as possible.
    pub fn run(&self) {
        self.heartbeat();
        self.bootsafe();
        loop {
            self.heartbeat();
            self.io.idle(Duration::from_secs(TICK_SECS));
            if !self.io.active() {
                return;
            }
            self.tick();
        }
    }

    /// Put the device back to the away scenario and clear runtime state.
    ///
    /// Called at boot (and when u60d takes the engine over). Runtime state is
    /// cleared but the pin is not: the pin exists precisely for when detection
    /// is misbehaving, and a reboot is the most likely thing to happen right
    /// after that. A pinned scenario is re-applied on the next tick, through the
    /// full apply-and-verify path.
    pub fn bootsafe(&self) {
        let _ = fs::create_dir_all(self.p(DIR));
        self.io.set_auto_sleep(true);
        self.clear_wakealarm();
        let away = lk(&self.cfg).scenario(AWAY_ID).cloned();
        // The core restarted (a ship, a crash), the device did not: the APs are down because the
        // applied scenario took them down, not because something broke. Keep it; "repairing"
        // brought Wi-Fi up for the ~100 s until home was seen again (device, 10-18).
        let prev: RunState = read_json(&self.p(STATE_FILE));
        if let Some(why) = self.kept_after_restart(&prev) {
            let mut st = lk(&self.state);
            *st = RunState { candidate: String::new(), hits: 0, misses: 0, ..prev };
            self.write_state(&st);
            self.log(&format!("[bootsafe] {why}; left as is"));
            return;
        }
        {
            let mut st = lk(&self.state);
            *st = RunState::default();
            self.write_state(&st);
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
        self.io.sleep(Duration::from_secs(5));
        let up_once = self.io.beaconing();
        self.io.sleep(Duration::from_secs(4));
        if up_once && self.io.beaconing() {
            self.log("[bootsafe] Wi-Fi already up; nothing to repair");
            return;
        }

        match away {
            Some(scen) => match self.apply(&scen) {
                Ok(()) => self.log("[bootsafe] Wi-Fi was down; restored away scenario"),
                Err(e) => self.log(&format!("[bootsafe] away scenario failed: {e}")),
            },
            None => {
                // No config yet: still make sure Wi-Fi comes back, because that
                // is the state a reboot must always be able to reach.
                match self.io.wifi_apply(true, true) {
                    Ok(()) => self.log("[bootsafe] Wi-Fi was down; forced it on"),
                    Err(e) => self.log(&format!("[bootsafe] forcing Wi-Fi on failed: {e}")),
                }
            }
        }
    }

    /// `Some(reason)` when bootsafe should leave Wi-Fi alone: up longer than `RESTART_MIN_UPTIME`
    /// (a core restart, not a boot: a boot keeps the full repair), no guard takeover, and the
    /// state the engine left says a scenario other than away is applied and it turned every AP
    /// it wrote off.
    fn kept_after_restart(&self, prev: &RunState) -> Option<String> {
        let up = self.io.uptime_secs()?;
        let off = |(k, v): (&String, &String)| k.ends_with(".disabled") && v == "1";
        (up >= RESTART_MIN_UPTIME
            && !Path::new(&self.p(TAKEOVER_FILE)).exists()
            && !prev.current.is_empty()
            && prev.current != AWAY_ID
            && !prev.applied.is_empty()
            && prev.applied.iter().all(off))
        .then(|| format!("Wi-Fi off by scenario \"{}\" (core restart, up {up} s)", prev.current))
    }

    // ── sleep survival ──────────────────────────────────────────────────────

    /// Belt to the wakelock's braces: if the device suspends anyway, the RTC brings
    /// it back so the next tick can still notice you have left. Re-armed every
    /// tick, so while awake it keeps being pushed out and never fires.
    fn arm_wakealarm(&self, secs: i64) {
        let Ok(text) = fs::read_to_string(self.p(RTC_SINCE_EPOCH)) else {
            return;
        };
        let Ok(base) = text.trim().parse::<i64>() else {
            return;
        };
        // The kernel requires clearing a pending alarm before setting a new one.
        let _ = fs::write(self.p(RTC_WAKEALARM), "0");
        let _ = fs::write(self.p(RTC_WAKEALARM), (base + secs).to_string());
    }

    fn clear_wakealarm(&self) {
        let _ = fs::write(self.p(RTC_WAKEALARM), "0");
    }

    /// Kept on /data so that the SMS sender (core or sentinel) still knows the last
    /// location after the engine has died. Best effort.
    fn set_abroad(&self, abroad: bool) {
        let alerts = luyaz_api::alerts::Alerts::at(Path::new(&self.p(luyaz_api::alerts::DIR)));
        if let Err(e) = alerts.set_flag("abroad", abroad) {
            eprintln!("[scenario] could not update the abroad flag: {e}");
        }
    }

    // ── actions ─────────────────────────────────────────────────────────────

    /// Run one action. Wireless goes through the Wi-Fi switch directly —
    /// routing it would wait on the Wi-Fi lock with the short HTTP timeout, and
    /// the routed handler's 200 would not prove anything anyway.
    fn run_action(&self, a: &ScenarioAction) -> Result<(), String> {
        if a.action.path == "/api/wifi/radio" {
            let body = body_value(&a.action.body).unwrap_or(json!({}));
            let want = |k: &str| body.get(k).and_then(|v| v.as_bool());
            let (cur_2g, cur_5g) = self.io.wifi_snapshot();
            let ap_2g = want("ap_2g").unwrap_or(cur_2g);
            let ap_5g = want("ap_5g").unwrap_or(cur_5g);
            // Applying always tears down and rebuilds. Moving between two
            // scenarios that both want Wi-Fi on (away ⇄ abroad) must not kick
            // every client off for half a minute to change nothing.
            if ap_2g && ap_5g && self.io.wifi_fully_on() {
                return Ok(());
            }
            self.io.wifi_apply(ap_2g, ap_5g)
        } else {
            let body = body_value(&a.action.body)
                .map(|v| serde_json::to_vec(&v).unwrap_or_default())
                .unwrap_or_default();
            self.io.exec(&a.action.method, &a.action.path, &body)?;
            if let Some(job_path) = &a.verify_job {
                self.wait_for_job(job_path)?;
            }
            if let Some(check) = &a.verify_field {
                self.wait_for_field(check)?;
            }
            Ok(())
        }
    }

    /// Poll a job endpoint until it leaves `running` (40 s at most).
    fn wait_for_job(&self, path: &str) -> Result<(), String> {
        for _ in 0..40 {
            self.io.sleep(Duration::from_millis(1000));
            let (status, resp) = self.io.get(path);
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
            if resp.pointer("/data/status").and_then(|v| v.as_str()) == Some("error") {
                let msg = resp.pointer("/data/message").and_then(|v| v.as_str()).unwrap_or("no message");
                return Err(format!("job reported: {msg}"));
            }
            return Ok(());
        }
        Err(format!("job at {path} did not settle within 40s"))
    }

    /// Poll a GET until a field holds the expected value (10 tries, a second apart).
    fn wait_for_field(&self, check: &sc::FieldCheck) -> Result<(), String> {
        let want = Value::from(&check.equals);
        let mut last = Value::Null;
        for _ in 0..10 {
            let (status, resp) = self.io.get(&check.path);
            if status < 400 {
                last = resp.pointer(&check.pointer).cloned().unwrap_or(Value::Null);
                if last == want {
                    return Ok(());
                }
            }
            self.io.sleep(Duration::from_millis(1000));
        }
        Err(format!("{}{} is {last}, expected {want}", check.path, check.pointer))
    }

    /// Apply a scenario's actions in order, rolling back on the first failure.
    fn apply(&self, scen: &Scenario) -> Result<(), String> {
        let mut undo: Vec<(String, String)> = Vec::new();

        for a in &scen.actions {
            // Manual-override check. If a key the engine previously wrote no
            // longer holds the engine's value, someone changed it on purpose —
            // leave it and say so, rather than yanking it back every tick.
            let overridden = {
                let st = lk(&self.state);
                a.snapshot
                    .iter()
                    .find(|key| match st.applied.get(*key) {
                        Some(prev) => self.io.uci_get(key).unwrap_or_default() != *prev,
                        None => false,
                    })
                    .cloned()
            };
            if let Some(key) = overridden {
                self.log(&format!(
                    "manual-override kept: {key} differs from what the engine last set; skipping {} {}",
                    a.action.method, a.action.path
                ));
                continue;
            }

            for key in &a.snapshot {
                undo.push((key.clone(), self.io.uci_get(key).unwrap_or_default()));
            }
            self.remember_for_restore(a);

            if let Err(e) = self.run_action(a) {
                if a.best_effort {
                    self.log(&format!(
                        "best-effort action failed ({} {}): {e} — continuing",
                        a.action.method, a.action.path
                    ));
                    continue;
                }
                self.log(&format!("action failed ({} {}): {e} — rolling back", a.action.method, a.action.path));
                self.rollback(&undo);
                return Err(e);
            }

            // Latch what we just wrote, so the override check above can tell
            // our edits from the user's next time round.
            let mut st = lk(&self.state);
            for key in &a.snapshot {
                st.applied.insert(key.clone(), self.io.uci_get(key).unwrap_or_default());
            }
            self.write_state(&st);
        }
        Ok(())
    }

    /// Save the value `a` is about to replace — unless something is already
    /// saved under its key. Re-entering abroad after a reboot there must keep
    /// the pre-trip value, not overwrite it with the DIRECT it set itself.
    fn remember_for_restore(&self, a: &ScenarioAction) {
        let (Some(r), Some(key)) = (a.restore_on_exit.as_ref(), sc::restore_key(a)) else {
            return;
        };
        if self.pending().iter().any(|p| p.key == key) {
            return;
        }
        let (status, resp) = self.io.get(&r.read_path);
        let current = resp.pointer(&r.read_pointer).cloned().unwrap_or(Value::Null);
        if status >= 400 || current.is_null() {
            self.log(&format!(
                "cannot read {}{} before changing it; nothing to restore later",
                r.read_path, r.read_pointer
            ));
            return;
        }
        let Some(back) = sc::restore_action(a, Json::from(current.clone())) else {
            return;
        };
        self.log(&format!("saved {current} to restore on leaving"));
        let _g = lk(&RESTORE_LOCK);
        let mut pending = self.pending();
        if pending.iter().any(|p| p.key == key) {
            return;
        }
        pending.push(PendingRestore { key, action: back, saved_at: self.io.now(), while_abroad: false });
        self.write_json(RESTORE_FILE, &pending);
    }

    /// Put back whatever `next` does not manage itself. Best-effort: a failure
    /// (its service stopped, say) keeps the entry so a later tick can try again.
    /// `quiet` skips logging failures, for the periodic retry.
    fn run_restores(&self, next: Option<&Scenario>, quiet: bool) {
        let pending = self.pending();
        let due = sc::due_restores(&pending, next);
        if due.is_empty() {
            return;
        }
        // Run without RESTORE_LOCK (an action can reach a handler that takes
        // it), then drop what succeeded from a fresh read, so an entry a
        // handler added meanwhile is not lost.
        let mut done: Vec<String> = Vec::new();
        // Set when this pass switched CHILL back on: mihomo takes a few
        // seconds to answer, and the entries after it (main group, exit) talk
        // to it. Without the wait they failed and sat until the next scan
        // (1–5 min; never, while pinned — 9-26 live test).
        let mut chill_wait: Option<f64> = None;
        for key in &due {
            let Some(p) = pending.iter().find(|p| &p.key == key) else { continue };
            set_in_restore(true);
            let mut result = self.run_action(&p.action);
            while result.is_err() && chill_wait.is_some_and(|t| self.io.mono() < t) {
                self.io.sleep(Duration::from_secs(2));
                result = self.run_action(&p.action);
            }
            if result.is_ok() && key == sc::CHILL_ON_KEY {
                chill_wait = Some(self.io.mono() + 20.0);
            }
            set_in_restore(false);
            match result {
                Ok(()) => {
                    let body = body_value(&p.action.action.body).unwrap_or(Value::Null);
                    self.log(&format!("restored {} {body}", p.action.action.path));
                    done.push(key.clone());
                }
                Err(e) => {
                    if !quiet {
                        self.log(&format!("restore failed, will retry: {e}"));
                    }
                }
            }
        }
        if done.is_empty() {
            return;
        }
        let _g = lk(&RESTORE_LOCK);
        let mut now_pending = self.pending();
        now_pending.retain(|p| !done.contains(&p.key));
        self.write_json(RESTORE_FILE, &now_pending);
    }

    /// Put snapshotted keys back, newest first, then make them take effect.
    fn rollback(&self, undo: &[(String, String)]) {
        if undo.is_empty() {
            return;
        }
        // Wi-Fi is the only thing snapshotted today, and it must end up on.
        let lock = match self.io.wifi_lock(luyaz_wifi::ENGINE_WAIT) {
            Ok(l) => l,
            Err(e) => {
                // The snapshot is only the two AP flags, which "on" overwrites
                // anyway — so skip the restore and go straight for "on", which
                // retries the lock itself.
                self.log(&format!("rollback: {e}; forcing Wi-Fi on instead"));
                match self.io.wifi_apply(true, true) {
                    Ok(()) => self.log("rollback complete; Wi-Fi forced on"),
                    Err(e) => self.log(&format!("rollback: could not restore Wi-Fi: {e}")),
                }
                return;
            }
        };
        // the snapshot back (best effort: the forced "on" below overwrites the
        // AP flags anyway)
        let set: serde_json::Map<String, Value> =
            undo.iter().rev().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect();
        self.io.wifi_put_back(set);
        match self.io.wifi_apply_locked(&lock, true, true) {
            Ok(()) => self.log("rollback complete; Wi-Fi forced on"),
            Err(e) => self.log(&format!("rollback: could not restore Wi-Fi: {e}")),
        }
    }

    fn switch_to(&self, cfg: &Config, id: &str) {
        let Some(scen) = cfg.scenario(id) else {
            return;
        };
        self.log(&format!("entering scenario {:?} ({})", scen.name, scen.id));
        self.run_restores(Some(scen), false);

        // Hold off suspend *before* tearing the APs down: once they are off,
        // Tailscale over cellular is the only way back in, and a suspended
        // device answers nothing.
        if scen.inhibit_sleep {
            self.io.set_auto_sleep(false);
            self.arm_wakealarm(cfg.params.scan_interval_home_secs as i64 + 30);
        }

        let result = self.apply(scen);
        let applied_ok = result.is_ok();

        let mut st = lk(&self.state);
        match result {
            Ok(()) => {
                st.current = scen.id.clone();
                st.last_switch = Some(self.io.now());
                // The SMS sender holds alerts while abroad (roaming charges).
                self.set_abroad(sc::is_abroad(scen));
                st.last_error = None;
                if !scen.inhibit_sleep {
                    self.io.set_auto_sleep(true);
                    self.clear_wakealarm();
                }
                self.log(&format!("scenario {:?} applied", scen.id));
            }
            Err(e) => {
                // Rollback already ran inside apply(); land on away so the next
                // tick starts from a known state rather than a half-applied one.
                st.current = AWAY_ID.to_string();
                st.last_error = Some(e.clone());
                self.io.set_auto_sleep(true);
                self.clear_wakealarm();
                self.log(&format!("scenario {:?} failed: {e}; fell back to away", scen.id));
            }
        }
        st.candidate.clear();
        st.hits = 0;
        st.misses = 0;
        self.write_state(&st);
        let rec = json!({
            "kind": "applied", "origin": "tick", "to": scen.id, "ok": applied_ok,
            "error": st.last_error, "after": tick_state(&st),
        });
        drop(st);
        // switch_to is only reached from tick (the HTTP paths go through bootsafe)
        self.publish(rec);
    }

    /// One tick: the decisions are [`Io::step`]'s (u60-policy `scenario::step`), called before the
    /// scan, with it, and with the recheck when it asks; this runs what they say, in the agent's
    /// order, and writes the agent's tick records.
    pub fn tick(&self) {
        if self.busy.swap(true, Ordering::SeqCst) {
            return; // previous tick still working
        }
        let _busy = BusyGuard(&self.busy);

        // The Wi-Fi guard stepped in while we were dead or stuck. Its Wi-Fi-on wins
        // over detection and over the pin until the takeover is released.
        self.reload_config();
        let takeover = self.guard_takeover();
        let cfg = self.config();
        let st = self.run_state();
        let enabled = self.enabled();
        let pin = self.pin();
        let charging = self.io.charging();
        let pin_retry_at = self.pin_retry_at.load(Ordering::Relaxed);
        let mut input = TickInput {
            cfg: &cfg,
            state: &st,
            enabled,
            takeover,
            pin: pin.as_deref(),
            now: self.io.now(),
            pin_retry_at,
            charging: charging.unwrap_or(false),
            scan: Scan::NotScanned,
            recheck: None,
            mcc: None,
            beaconing: true,
        };
        let out = self.io.step(&input);
        let current = st.current.clone();

        match out.branch {
            Branch::Takeover => {
                let to = match &out.intent {
                    Intent::Switch { to } => Some(to.clone()),
                    _ => None,
                };
                // the agent's record: switch_to whenever enabled, configured and not in away
                let would = enabled && !cfg.scenarios.is_empty() && current != AWAY_ID;
                self.publish(json!({"kind": "takeover", "current": current, "enabled": enabled, "switch_to": would.then_some(AWAY_ID)}));
                if let Some(to) = to {
                    self.log("Wi-Fi watchdog takeover: returning to away");
                    self.switch_to(&cfg, &to);
                }
                return;
            }
            Branch::Disabled => {
                // Not just "stop working": if the engine is switched off while a
                // scenario has Wi-Fi down, freezing there would leave the device
                // permanently without the network it exists to provide. The off
                // switch has to be an escape hatch, so it restores `away` first.
                // The HTTP endpoint does this too; doing it here as well makes
                // `touch /data/scenario/disabled` — the only escape available over
                // SSH — behave the same way.
                if out.intent == Intent::Bootsafe {
                    self.log("engine disabled while away from the away scenario — restoring it");
                    self.bootsafe();
                }
                // Off means "as if the engine were not here": put back anything a
                // scenario changed, too.
                self.run_restores(None, true);
                return;
            }
            Branch::Unconfigured => return, // unconfigured engines do nothing
            Branch::Pinned => {
                // A pin overrides detection entirely, but still goes through the
                // whole apply-and-verify path rather than being assumed.
                let pinned = pin.unwrap_or_default();
                self.publish(json!({
                    "kind": "pin", "pin": pinned, "current": current,
                    "switch_to": (pinned != current).then_some(&pinned),
                }));
                if let Intent::Switch { to } = &out.intent {
                    self.switch_to(&cfg, to);
                } else if let Some(r) = &out.restores {
                    // No scans while pinned, so failed restores get their retry
                    // here instead (same rule as the scan path below).
                    self.pin_retry_at.store(out.pin_retry_at, Ordering::Relaxed);
                    let next = r.next.as_deref().and_then(|id| cfg.scenario(id));
                    self.run_restores(next, true);
                }
                return;
            }
            Branch::NotDue => {
                // Keep the sleep defences fresh even when not scanning.
                if out.keep_awake {
                    self.io.set_auto_sleep(false);
                    self.arm_wakealarm(cfg.params.scan_interval_home_secs as i64 + 30);
                }
                return;
            }
            Branch::Due | Branch::ScanFailed | Branch::Scanned => {}
        }

        // A restore that failed on the way out (its service was stopped, say) gets
        // another go each scan, for as long as the current scenario does not
        // manage that value itself. Not before the first decision after boot:
        // with nothing applied yet the device may well still be abroad, and
        // restoring there would flip the route back and forth.
        if let Some(r) = &out.restores {
            let next = r.next.as_deref().and_then(|id| cfg.scenario(id));
            self.run_restores(next, true);
        }

        let before = tick_state(&st);
        let interval = sc::scan_interval(&cfg, &current, &st.candidate, charging.unwrap_or(false));
        let nets = match self.io.scan() {
            Ok(n) => n,
            Err(e) => {
                let failures = st.scan_failures + 1;
                // Fail open. If we cannot see where we are and the APs are
                // down, the safe guess is "not home" — leaving Wi-Fi off would
                // strand the only device that provides it.
                let beaconing =
                    (failures >= sc::SCAN_FAILURES_BEFORE_FAIL_OPEN && current != AWAY_ID).then(|| self.io.beaconing());
                input.scan = Scan::Failed { at: self.io.now(), error: &e };
                input.beaconing = beaconing.unwrap_or(true);
                let out = self.io.step(&input);
                *lk(&self.state) = out.state.clone();
                self.write_state(&out.state);
                self.log(&format!("scan failed ({failures} in a row): {e}"));
                let fail_open = beaconing == Some(false);
                self.publish(json!({
                    "kind": "scan_failed", "before": before, "interval": interval, "charging": charging,
                    "beaconing": beaconing,
                    "after": tick_state(&out.state),
                    "switch_to": fail_open.then_some(AWAY_ID),
                }));
                if let Intent::Switch { to } = &out.intent {
                    self.log("cannot scan and Wi-Fi is off — failing open to the away scenario");
                    self.switch_to(&cfg, to);
                }
                return;
            }
        };

        let first_scan = nets.len();
        let mcc = self.io.sim_mcc();
        input.mcc = mcc.as_deref();
        let at = self.io.now();
        input.scan = Scan::Networks { at, networks: &nets };
        let mut out = self.io.step(&input);
        let mut all = nets.clone();
        let mut rechecked = false;

        // Leaving a Wi-Fi-detected scenario rests on not seeing something, and
        // a partial scan also does not see it. Look again before believing it,
        // and say what was (not) seen, so a wrong exit can be explained later.
        if out.intent == Intent::NeedRecheck {
            self.io.sleep(MISS_RECHECK_DELAY);
            rechecked = true;
            let more = self.io.scan().unwrap_or_default();
            all.extend(more.iter().cloned());
            input.recheck = Some(Recheck { at: self.io.now(), networks: &more });
            out = self.io.step(&input);
            let detected = sc::detect(&cfg, &all, mcc.as_deref()).unwrap_or_else(|| AWAY_ID.to_string());
            if detected == current {
                self.log(&format!("{current:?} unseen in one scan ({first_scan} networks), seen on recheck"));
            } else if let Some(entries) = sc::ssid_entries(&cfg, &current) {
                self.log(&format!(
                    "{current:?} not seen: {}",
                    sc::miss_summary(entries, &all, first_scan, cfg.params.min_rssi_dbm)
                ));
            }
        }
        let detected = sc::detect(&cfg, &all, mcc.as_deref()).unwrap_or_else(|| AWAY_ID.to_string());

        *lk(&self.state) = out.state.clone();
        self.write_state(&out.state);
        let switch = match &out.intent {
            Intent::Switch { to } => Some(to.clone()),
            _ => None,
        };
        // nets[..first] is the first scan; the rest (if rechecked) the second
        self.publish(json!({
            "kind": "scan", "before": before, "interval": interval, "charging": charging,
            "nets": all.iter().map(net_json).collect::<Vec<_>>(),
            "first": first_scan, "rechecked": rechecked, "mcc": mcc,
            "detected": detected, "after": tick_state(&out.state), "switch_to": switch,
        }));

        if let Some(to) = switch {
            self.switch_to(&cfg, &to);
        }
    }

    // ── CHILL hooks ─────────────────────────────────────────────────────────

    /// CHILL was switched on or off by hand (not by a restore). Switched on: no
    /// more "switch it on when home". Switched off abroad (`was_on`): switch it
    /// back on when home.
    pub fn chill_toggled(&self, on: bool, was_on: bool) {
        if on {
            if in_restore() {
                return; // the restore itself; run_restores removes the entry on success
            }
            let _g = lk(&RESTORE_LOCK);
            let mut pending = self.pending();
            let before = pending.len();
            pending.retain(|p| p.key != sc::CHILL_ON_KEY);
            if pending.len() != before {
                self.write_json(RESTORE_FILE, &pending);
                self.log("CHILL switched on by hand; no longer switching it on when home");
            }
            return;
        }
        if !was_on {
            return;
        }
        if self.add_abroad_restore(sc::chill_on_restore(self.io.now())) {
            self.log("CHILL switched off abroad; will switch it back on when home");
        }
    }

    /// The CHILL exit was changed by hand abroad: go back to proxy when home.
    pub fn chill_exit_changed(&self) {
        if in_restore() {
            return;
        }
        if self.add_abroad_restore(sc::chill_exit_restore(self.io.now())) {
            self.log("CHILL exit changed abroad; will go back to proxy when home");
        }
    }

    fn in_abroad_scenario(&self) -> bool {
        let cfg = lk(&self.cfg);
        let st = lk(&self.state);
        sc::in_abroad_scenario(&cfg, self.enabled(), &st.current)
    }

    fn add_abroad_restore(&self, entry: PendingRestore) -> bool {
        if !self.in_abroad_scenario() {
            return false;
        }
        let _g = lk(&RESTORE_LOCK);
        let mut pending = self.pending();
        if pending.iter().any(|p| p.key == entry.key) {
            return false;
        }
        pending.push(entry);
        self.write_json(RESTORE_FILE, &pending);
        true
    }
}

/// The last `LOG_TAIL_LINES` lines of a log, newline-joined (what is kept on trim and served).
pub(crate) fn log_tail(text: &str) -> String {
    let all: Vec<&str> = text.lines().collect();
    let keep_from = all.len() - all.len().min(LOG_TAIL_LINES);
    all[keep_from..].join("\n")
}

/// Append one line to the scenario log, trimming it to its last lines once it passes 1 MiB.
fn log_line(dir: &str, file: &str, msg: &str) {
    let _ = fs::create_dir_all(dir);
    if fs::metadata(file).map(|m| m.len()).unwrap_or(0) > LOG_MAX_BYTES {
        let keep = fs::read_to_string(file).unwrap_or_default();
        let _ = fs::write(file, log_tail(&keep));
    }
    // Local time, as the rest of the device's logs (the clock is local time marked UTC).
    let stamp = {
        // SAFETY: time(NULL) and localtime_r into a zeroed tm have no preconditions.
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
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(file) {
        let _ = writeln!(f, "{stamp} {msg}");
    }
}

#[cfg(test)]
mod tests;
