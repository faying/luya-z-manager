//! SMS forwarding, run by the core: new received SMS are matched against the user's rules and
//! sent on (Telegram, webhook, SMS, ntfy, Discord, Slack). Everything that touches the device
//! (the core's SMS feed and pages, ubus reads, the vendor send, core actions, alerts, shadow
//! records, sleeping) goes through [`Host`]; HTTP destinations are called from here. Tests
//! use a fake host.
//!
//! One owner at a time: the event loop waits for another live owner's lease
//! ([`wait_lease_free`]), re-reads the state file when it starts, and stops (holding the current
//! SMS, as a transient failure) once [`Host::active`] says the host no longer owns forwarding.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{atomic::AtomicBool, atomic::Ordering, mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
// derive macros for the state file types below
use serde::{Deserialize, Serialize};

mod api;
mod lease;
mod model;
mod queue;
mod send;
mod text;

pub use lease::{remove_lease, wait_lease_free, write_lease, LEASE_FILE, LEASE_WAIT};
pub use model::{ForwardDestination, ForwardLogEntry, ForwardRule, HttpHeader, SmsFilter, SmsForwardConfig};
pub(crate) use model::DecodedSms;
pub use queue::{backoff_secs, fingerprint, Job, JobState, Queue, QUEUE_PATH};
use send::{
    destination_type_name, forward_to, http_agent, is_forward_loop, is_garbled_echo,
    is_transient_error, matches_filter,
};
use text::{decode_ucs2_hex, is_ucs2_hex, now_ts, preview};

/// Log lines kept in the state file (oldest dropped first).
const MAX_LOG_ENTRIES: usize = 200;
/// Pause between two sends of one drain.
const RULE_GAP: Duration = Duration::from_millis(500);

pub const CONFIG_PATH: &str = "/data/local/tmp/sms_forward.json";
pub const STATE_PATH: &str = "/data/local/tmp/sms_forward_state.json";

/// What the engine does to the device. Errors are the host's own text (logged as they come).
pub trait Host: Send + Sync + 'static {
    /// the core's `sms` block summary (max id, count), if subscribed and fresh; `None` = read ubus
    /// directly.
    fn sms_summary(&self) -> Option<(u64, u64)>;
    /// One core `sms.list_after` page (busy retried by the host).
    fn list_after(&self, after: u64, limit: u64) -> Result<Page, String>;
    /// A ubus read; `long_timeout` = the long read timeout (SMS lists), else the default.
    fn ubus_read(&self, obj: &str, method: &str, params: Option<&str>, long_timeout: bool) -> Result<Value, String>;
    /// A vendor setter through core (sending an SMS), as the calling thread's source.
    fn vendor(&self, obj: &str, method: &str, params: Option<&str>) -> Result<Value, String>;
    /// A core action (`sms.delete`, `sms.mark_read`) as source auto.
    fn control(&self, action: &str, params: &Value) -> Result<Value, String>;
    fn alert(&self, key: &str, text: &str);
    /// A shadow record for u60d (the host keeps the ring).
    fn tick(&self, record: Value);
    /// The device's SMS zone in hours (the clock is local time labelled UTC).
    fn sms_zone_hours(&self) -> i64;
    /// The host still owns forwarding; false = handed over (the loop stops).
    fn active(&self) -> bool;
    fn sleep(&self, d: Duration);
    /// Runs the event loop on its thread (the host attributes that thread's writes to source
    /// auto here).
    fn run_worker(&self, f: Box<dyn FnOnce() + '_>) {
        f()
    }
    /// Hold the loop's checks for now (a SIM switch in progress): nothing is read, sent or
    /// deleted and the watermark stays; the host wakes the loop when the hold ends.
    fn hold(&self) -> bool {
        false
    }
}
/// The state file: how far forwarding got, what it did, and what it is waiting on.
#[derive(Clone, Default, Deserialize, Serialize)]
pub struct ForwardState {
    pub last_forwarded_id: u64, // watermark: highest device SMS id handled, 0 = not set yet
    /// Time of the newest SMS the forwarder has handled, as parsed from the
    /// SMS's own `date` field by `sms_time_key` (seconds, UTC-normalised with
    /// the SMS's zone). Both sides of every comparison come from that same
    /// field and parser — never from the device clock. Missing in state files
    /// written before this field existed; see `plan_forward`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_forwarded_time: Option<i64>,
    /// Delivery attempts, oldest first, at most [`MAX_LOG_ENTRIES`].
    #[serde(default = "Vec::new")]
    pub log: Vec<model::ForwardLogEntry>,
    /// The SMS the watermark is held at by a transient failure, if any.
    /// Missing in older state files (= nothing held).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<HeldSms>,
}

/// A deferred SMS: when it was first held, how long it has waited while
/// online, and which rules already got it (not re-sent after a restart).
#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
pub struct HeldSms {
    pub sms_id: u64,
    /// Wall clock of the first deferral (informational).
    pub first_deferred_at: i64,
    /// Seconds waited while online (cellular service + WAN) since then.
    #[serde(default)]
    pub online_secs: u64,
    #[serde(default)]
    pub delivered_rules: Vec<u32>,
}

/// Counts time only while online. `observe(now, online)` is called with the
/// link state from `now` on; it returns the seconds since the previous
/// observation if the link was up since then (an interval that started
/// offline counts nothing). The first observation
/// after a restart counts nothing.
#[derive(Default)]
struct OnlineClock {
    last: Option<(u64, bool)>,
}

impl OnlineClock {
    fn observe(&mut self, now: u64, online: bool) -> u64 {
        let add = match self.last {
            Some((t, true)) => now.saturating_sub(t),
            _ => 0,
        };
        self.last = Some((now, online));
        add
    }
}

/// Whole seconds of `CLOCK_MONOTONIC` (immune to wall-clock steps).
fn mono_secs() -> u64 {
    let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: fills the struct we own.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    now.tv_sec as u64
}

// ── wake-ups ─────────────────────────────────────────────────────────

/// Why the forwarder woke: the core's `sms` block summary (max id / count)
/// changed, or the source switched (订阅中 with an `sms` block ⇄ 退路 / no
/// block). Replaces the `zwrt_wms_status_event` ubus listen (T10/D2).
struct Wake;

/// While the core's `sms` block drives us: a check every 5 min as a safety net.
/// Reading ubus directly (退路, no feed, or a core without the block):
/// every [`FALLBACK_SMS_POLL`] — nothing else wakes us then.
const IDLE_SMS_POLL: Duration = Duration::from_secs(300);
const FALLBACK_SMS_POLL: Duration = Duration::from_secs(60);

/// When to run a check. Pure over a monotonic `now`.
#[derive(Debug)]
struct PollSchedule {
    last_poll: Duration,
    /// The last check went through core (`sms` block fresh, subscribed).
    via_core: bool,
}

impl PollSchedule {
    fn new(now: Duration, via_core: bool) -> Self {
        PollSchedule { last_poll: now, via_core }
    }

    fn period(&self) -> Duration {
        if self.via_core {
            IDLE_SMS_POLL
        } else {
            FALLBACK_SMS_POLL
        }
    }

    /// The source for this check; true if it switched (then page through
    /// either way — into direct reads to cover the gap, back to core to
    /// catch anything the last direct read missed).
    fn source_changed(&mut self, via_core: bool) -> bool {
        let changed = self.via_core != via_core;
        self.via_core = via_core;
        changed
    }

    fn due(&self, now: Duration) -> bool {
        now.saturating_sub(self.last_poll) >= self.period()
    }

    fn polled(&mut self, now: Duration) {
        self.last_poll = now;
    }

    fn wait(&self, now: Duration) -> Duration {
        (self.last_poll + self.period())
            .saturating_sub(now)
            .clamp(Duration::from_millis(100), IDLE_SMS_POLL)
    }
}

/// What one complete (both stores OK) read of the device's SMS list gave us.
#[derive(Debug, Clone, Default)]
struct DeviceSnapshot {
    /// Highest id seen in any row (received or not).
    max_id: u64,
    /// Received messages (tag 0/1): (id, `sms_time_key` of its date).
    received: Vec<(u64, Option<i64>)>,
}

#[derive(Debug, PartialEq)]
struct ForwardPlan {
    /// Ids to forward, ascending.
    forward_ids: Vec<u64>,
    new_watermark: u64,
    new_last_time: Option<i64>,
    /// Device ids went below the watermark (counter reset / storage wiped).
    rollback: bool,
}

/// Decide what to forward. Pure (R4/R15).
///
/// * Read failed (either store errored) → nothing forwarded, nothing moves.
/// * Normal: forward received ids > watermark; watermark = max of those.
/// * Rollback (device max id < watermark, device not empty): forward received
///   messages whose time is newer than `last_time`, then watermark = device
///   max id. Without a `last_time` (old state file) nothing is forwarded — we
///   would rather miss a message than re-send old ones — and the baseline is
///   re-established from what is on the device.
/// * An empty device is not treated as a rollback (can't tell; keep state).
/// * `last_time` missing with no rollback: backfilled from messages the
///   watermark already covers, so old state files gain a time baseline.
fn plan_forward(watermark: u64, last_time: Option<i64>, snapshot: Result<&DeviceSnapshot, &str>) -> ForwardPlan {
    let unchanged = ForwardPlan {
        forward_ids: Vec::new(),
        new_watermark: watermark,
        new_last_time: last_time,
        rollback: false,
    };
    let snap = match snapshot {
        Ok(s) => s,
        Err(_) => return unchanged,
    };
    let max_opt = |a: Option<i64>, b: Option<i64>| match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (x, None) => x,
        (None, y) => y,
    };

    if snap.max_id > 0 && snap.max_id < watermark {
        let mut forward: Vec<(u64, Option<i64>)> = match last_time {
            Some(lt) => snap
                .received
                .iter()
                .copied()
                .filter(|&(_, t)| matches!(t, Some(t) if t > lt))
                .collect(),
            None => Vec::new(),
        };
        forward.sort_by_key(|&(id, _)| id);
        let new_last_time = match last_time {
            Some(_) => forward.iter().fold(last_time, |acc, &(_, t)| max_opt(acc, t)),
            None => snap.received.iter().fold(None, |acc, &(_, t)| max_opt(acc, t)),
        };
        return ForwardPlan {
            forward_ids: forward.into_iter().map(|(id, _)| id).collect(),
            new_watermark: snap.max_id,
            new_last_time,
            rollback: true,
        };
    }

    let mut forward: Vec<(u64, Option<i64>)> =
        snap.received.iter().copied().filter(|&(id, _)| id > watermark).collect();
    forward.sort_by_key(|&(id, _)| id);
    let mut new_last_time = last_time;
    if new_last_time.is_none() {
        new_last_time = snap
            .received
            .iter()
            .filter(|&&(id, _)| id <= watermark)
            .fold(None, |acc, &(_, t)| max_opt(acc, t));
    }
    let new_last_time = forward.iter().fold(new_last_time, |acc, &(_, t)| max_opt(acc, t));
    ForwardPlan {
        new_watermark: forward.last().map(|&(id, _)| id).unwrap_or(watermark),
        forward_ids: forward.into_iter().map(|(id, _)| id).collect(),
        new_last_time,
        rollback: false,
    }
}

/// Parse the ZTE SMS `date` field (`YY,MM,DD,HH,MM,SS,+TZ`, `;` also
/// accepted) into seconds since 2000-01-01 UTC, using the zone the SMS
/// carries (hours; values beyond ±14 are read as GSM quarter-hours). Only ever
/// compared with other values from this same function.
fn sms_time_key(raw: &str) -> Option<i64> {
    let parts: Vec<&str> = raw.split([',', ';']).collect();
    if parts.len() < 6 {
        return None;
    }
    let n = |i: usize| -> Option<i64> { parts[i].trim().parse::<i64>().ok() };
    let (yy, mo, dd, hh, mi, ss) = (n(0)?, n(1)?, n(2)?, n(3)?, n(4)?, n(5)?);
    if !(0..=99).contains(&yy)
        || !(1..=12).contains(&mo)
        || !(1..=31).contains(&dd)
        || !(0..=23).contains(&hh)
        || !(0..=59).contains(&mi)
        || !(0..=60).contains(&ss)
    {
        return None;
    }
    // Days since 2000-01-01 (civil calendar, all years 2000..2099).
    let y = 2000 + yy;
    let (y2, m2) = if mo <= 2 { (y - 1, mo + 9) } else { (y, mo - 3) };
    let era_days = |y: i64, m: i64, d: i64| 365 * y + y / 4 - y / 100 + y / 400 + (153 * m + 2) / 5 + d;
    let days = era_days(y2, m2, dd) - era_days(1999, 10, 1); // 2000-01-01 == Mar-based (1999, 10, 1)
    let tz_secs = match parts.get(6).map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(tz) => {
            let v: i64 = tz.trim_start_matches('+').parse().ok()?;
            if v.abs() > 14 { v * 15 * 60 } else { v * 3600 }
        }
        None => 0,
    };
    Some(days * 86_400 + hh * 3600 + mi * 60 + ss - tz_secs)
}

// ── the forwarder ──

/// The running engine: settings, state, link status and the wake channel, shared between the
/// event loop thread and the API handlers.
pub struct SmsForwarder {
    host: Arc<dyn Host>,
    /// Where settings and state are saved.
    config_path: PathBuf,
    state_path: PathBuf,
    /// Settings and rules as last saved.
    config: Mutex<SmsForwardConfig>,
    /// Watermark, log and held SMS as last saved (plus unsaved online time).
    state: Mutex<ForwardState>,
    /// Link status as last reported: cellular service (needed by SMS targets) and WAN (needed
    /// by the HTTP ones).
    link: Link,
    /// `(sms_id, rule_id)` already delivered for the SMS the watermark is
    /// held at (a deferred SMS is tried again later: rules that already got
    /// it, automatically or by a manual retry, are not sent it twice).
    delivered: Mutex<HashSet<(u64, u32)>>,
    /// Where the delivery queue is saved, and the queue as last saved (plus unsaved online time).
    queue_path: PathBuf,
    queue: Mutex<Queue>,
    /// Online-only clock for the queued jobs' give-up time.
    online_clock: Mutex<OnlineClock>,
    /// Wakes the forwarder (connectivity came back, new SMS).
    wake: Mutex<Option<mpsc::Sender<Wake>>>,
    /// The core `sms` summary the last check saw: seen again with nothing to do, the block may
    /// have stopped following the device, so that check also reads ubus directly.
    last_summary: Mutex<Option<(u64, u64)>>,
}

/// Two link flags, both down until something says otherwise.
#[derive(Default)]
struct Link {
    service: AtomicBool,
    wan: AtomicBool,
}

impl Link {
    fn service(&self) -> bool {
        self.service.load(Ordering::Relaxed)
    }
    fn wan(&self) -> bool {
        self.wan.load(Ordering::Relaxed)
    }
}

/// A `network_type` with cellular service.
fn in_service(network_type: &str) -> bool {
    !network_type.is_empty() && network_type != "NO_SERVICE" && !network_type.starts_with("LIMITED_SERVICE")
}

/// A WAN status (`connect_status` / `wan_status`) that means an IPv4 connection is up.
fn wan_up(status: &str) -> bool {
    status.starts_with("ipv4")
}

/// A JSON file parsed as `T`; missing or unreadable = `T::default()`.
fn load_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> T {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => T::default(),
    }
}

fn read_state(path: &Path) -> ForwardState {
    load_json(path)
}

fn delivered_of(state: &ForwardState) -> HashSet<(u64, u32)> {
    state.held.iter().flat_map(|h| h.delivered_rules.iter().map(move |r| (h.sms_id, *r))).collect()
}

/// The queue file opened; a damaged one is recovered and reported once (`sms-forward-queue`).
fn open_queue(host: &dyn Host, path: &Path) -> Queue {
    let (q, note) = Queue::open(path);
    if let Some(note) = note {
        host.alert("sms-forward-queue", &note);
    }
    q
}

/// Seed attempts at start (the vendor services may not be registered yet), 2 s apart.
const SEED_TRIES: u32 = 5;

impl SmsForwarder {
    /// The config and state files: [`CONFIG_PATH`] and [`STATE_PATH`] on the device (the core
    /// may put them under its compat root).
    pub fn new(host: Arc<dyn Host>, config_path: impl Into<PathBuf>, state_path: impl Into<PathBuf>) -> Self {
        let config_path: PathBuf = config_path.into();
        let state_path: PathBuf = state_path.into();
        let saved: ForwardState = read_state(&state_path);
        let queue_path = state_path.with_file_name("sms-forward-queue.json");
        let queue = open_queue(&*host, &queue_path);
        SmsForwarder {
            queue: Mutex::new(queue),
            queue_path,
            delivered: Mutex::new(delivered_of(&saved)),
            state: Mutex::new(saved),
            config: Mutex::new(load_json(&config_path)),
            link: Link::default(),
            online_clock: Mutex::default(),
            wake: Mutex::new(None),
            last_summary: Mutex::new(None),
            config_path,
            state_path,
            host,
        }
    }

    /// The queue saved at `path` instead (the core: [`QUEUE_PATH`] under its data dir).
    pub fn queue_at(mut self, path: impl Into<PathBuf>) -> Self {
        self.queue_path = path.into();
        self.queue = Mutex::new(open_queue(&*self.host, &self.queue_path));
        self
    }

    /// One try at whichever link flag is still down; true when both are known up.
    fn seed_link(&self) -> (bool, bool) {
        let service = self.link.service()
            || match self.host.ubus_read("zte_nwinfo_api", "nwinfo_get_netinfo", Some("{}"), false) {
                Err(_) => false,
                Ok(info) => {
                    let kind = info["network_type"].as_str().unwrap_or_default();
                    let up = in_service(kind);
                    self.link.service.store(up, Ordering::Relaxed);
                    eprintln!("[sms_forward] initial service state: {kind} (has_service={up})");
                    up
                }
            };
        let wan_args = r#"{"source_module":"zte_topsw_data","cid":1}"#;
        let wan = self.link.wan()
            || match self.host.ubus_read("zwrt_data", "get_wwaniface", Some(wan_args), false) {
                Err(_) => false,
                Ok(info) => {
                    let up = info["connect_status"].as_str().is_some_and(wan_up);
                    self.link.wan.store(up, Ordering::Relaxed);
                    eprintln!("[sms_forward] initial WAN state: connected={up}");
                    up
                }
            };
        (service, wan)
    }

    /// Seed the connectivity state (retried while the services are not registered yet), then
    /// run the event loop on a thread of its own. Wake it with [`wake`](Self::wake) (new SMS)
    /// and feed it [`set_service`](Self::set_service) / [`set_wan`](Self::set_wan); without
    /// wakes only its timer checks. `lease`: wait for another live owner's lease before the
    /// first pass ([`wait_lease_free`]).
    pub fn start(self: &Arc<Self>, lease: Option<PathBuf>) {
        let mut tries = 0;
        loop {
            tries += 1;
            let (service, wan) = self.seed_link();
            if (service && wan) || tries == SEED_TRIES {
                break;
            }
            eprintln!(
                "[sms_forward] connectivity seed attempt {tries}/{SEED_TRIES} incomplete (service={service}, wan={wan}), retrying in 2s"
            );
            self.host.sleep(Duration::from_secs(2));
        }

        // The sender kept here keeps the channel open: with no other waker
        // only the timer wakes the loop.
        let (tx, rx) = mpsc::channel();
        *self.wake.lock().unwrap() = Some(tx);

        let me = Arc::clone(self);
        std::thread::spawn(move || {
            let host = me.host.clone();
            host.run_worker(Box::new(|| me.event_loop(rx, lease.as_deref())));
        });
    }

    /// Stores a new link flag; the online clock closes its interval under the old state first.
    /// A flag that comes up wakes the loop (once per transition, so no storm).
    fn set_link(&self, flag: &AtomicBool, up: bool, what: &str, raw: &str) {
        let was = flag.swap(up, Ordering::Relaxed);
        self.tick_online();
        if was == up {
            return;
        }
        eprintln!("[sms_forward] {what} state changed: {raw} ({}={up})", if what == "WAN" { "connected" } else { "has_service" });
        if up {
            self.wake();
        }
    }

    /// A `zwrt_servicestatus` event's `network_type`.
    pub fn set_service(&self, network_type: &str) {
        self.set_link(&self.link.service, in_service(network_type), "service", network_type);
    }

    /// A `router_event_wan_connect_status` event's `wan_status`.
    pub fn set_wan(&self, wan_status: &str) {
        self.set_link(&self.link.wan, wan_up(wan_status), "WAN", wan_status);
    }

    fn online(&self) -> bool {
        self.link.service() && self.link.wan()
    }

    /// Add the online time since the last tick to every pending job (in memory; saved with the
    /// next transition).
    fn tick_online(&self) {
        let gained = self.online_clock.lock().unwrap().observe(mono_secs(), self.online());
        if gained == 0 {
            return;
        }
        for j in self.queue.lock().unwrap().jobs.iter_mut().filter(|j| j.state == JobState::Pending) {
            j.online_secs += gained;
        }
    }

    /// Check now (new SMS; connectivity came back: one wake per transition, so no storm).
    /// False once the loop has stopped (or before [`start`](Self::start)).
    pub fn wake(&self) -> bool {
        self.wake.lock().unwrap().as_ref().is_some_and(|tx| tx.send(Wake).is_ok())
    }

    /// The state file as the loop starts: another owner may have moved the watermark or the
    /// held SMS since [`new`](Self::new). Online time counted in memory for the same held SMS
    /// and not saved yet is kept.
    fn reload_state(&self) {
        let mut fresh = read_state(&self.state_path);
        let mut state = self.state.lock().unwrap();
        if let (Some(f), Some(m)) = (fresh.held.as_mut(), state.held.as_ref()) {
            if f.sms_id == m.sms_id {
                f.online_secs = f.online_secs.max(m.online_secs);
            }
        }
        *self.delivered.lock().unwrap() = delivered_of(&fresh);
        *state = fresh;
    }

    /// No watermark yet (first run, or forwarding just switched on): start from the newest SMS
    /// on the device, so only later ones go out.
    fn init_watermark(&self) {
        if self.state.lock().unwrap().last_forwarded_id != 0 {
            return;
        }
        let (top, newest) = match fetch_baseline(&*self.host) {
            Err(e) => return eprintln!("[sms_forward] watermark init failed: {e}"),
            Ok((0, _)) => return eprintln!("[sms_forward] no SMS on device, watermark stays at 0"),
            Ok(found) => found,
        };
        let mut st = self.state.lock().unwrap();
        st.last_forwarded_id = top;
        st.last_forwarded_time = sane_time(max_time(st.last_forwarded_time, newest));
        self.save_state(&st);
        eprintln!("[sms_forward] watermark initialized to {top}");
    }

    /// The loop thread: wait for the lease, pick up the saved state, check once at once (a
    /// restart mid-burst resumes from the watermark straight away), then check on every wake
    /// and whenever the poll timer says so. Ends when ownership moves away.
    fn event_loop(&self, rx: mpsc::Receiver<Wake>, lease: Option<&Path>) {
        if let Some(path) = lease {
            wait_lease_free(path, &*self.host, LEASE_WAIT);
        }
        self.reload_state();
        if !self.host.active() {
            return eprintln!("[sms_forward] not the owner: forwarder stopped");
        }
        self.init_watermark();

        let started = Instant::now();
        let first = self.host.sms_summary();
        let mut sched = PollSchedule::new(Duration::ZERO, first.is_some());
        self.check(first);
        sched.polled(started.elapsed());
        'wakes: loop {
            let wait = match self.queue_wait() {
                Some(q) => sched.wait(started.elapsed()).min(q),
                None => sched.wait(started.elapsed()),
            };
            let woke = rx.recv_timeout(wait);
            match woke {
                // a burst queues many wakes; one pass pages through all of them
                Ok(Wake) => while rx.try_recv().is_ok() {},
                // a queued job came due before the next poll: just the queue
                Err(mpsc::RecvTimeoutError::Timeout) if !sched.due(started.elapsed()) => {
                    if !self.host.active() {
                        return eprintln!("[sms_forward] handed over: forwarder stopped");
                    }
                    let cfg = self.config.lock().unwrap().clone();
                    if is_active(&cfg) && !self.host.hold() {
                        self.drain(&cfg, &http_agent());
                    }
                    continue 'wakes;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return eprintln!("[sms_forward] wake channel closed");
                }
            }
            if !self.host.active() {
                return eprintln!("[sms_forward] handed over: forwarder stopped");
            }
            let summary = self.host.sms_summary();
            if sched.source_changed(summary.is_some()) {
                let via = if summary.is_some() { "core" } else { "direct ubus" };
                eprintln!("[sms_forward] SMS source now {via}: checking SMS");
            }
            self.check(summary);
            sched.polled(started.elapsed());
        } // 'wakes
    }

    /// One check: everything above the watermark, paged. `summary` is the core's `sms` block
    /// when that is subscribed and fresh; `None` means ubus is read directly.
    fn check(&self, summary: Option<(u64, u64)>) {
        let cfg = self.config.lock().unwrap().clone();
        let live: Vec<&ForwardRule> = cfg.rules.iter().filter(|r| r.enabled).collect();
        if live.is_empty() || !cfg.enabled {
            return;
        }
        if self.host.hold() {
            return eprintln!("[sms_forward] held: SIM switch in progress");
        }
        self.init_watermark();
        self.seed(summary.is_some());
        self.scan(summary, &cfg, &live);
        self.drain(&cfg, &http_agent());
    }

    /// The pass: everything above the watermark goes into the queue.
    fn scan(&self, summary: Option<(u64, u64)>, cfg: &SmsForwardConfig, live: &[&ForwardRule]) {
        let host = &*self.host;
        let mut forward = |sms: &DecodedSms| self.enqueue(sms, cfg, live);
        let mut commit = |watermark: u64, last_time: Option<i64>| {
            let mut st = self.state.lock().unwrap();
            (st.last_forwarded_id, st.last_forwarded_time) = (watermark, sane_time(last_time));
            self.save_state(&st);
        };
        let position = || {
            let st = self.state.lock().unwrap();
            (st.last_forwarded_id, sane_time(st.last_forwarded_time))
        };

        if let Some((top, _)) = summary {
            let repeated = self.last_summary.lock().unwrap().replace((top, summary.unwrap().1)) == summary;
            let (from, since) = position();
            let mut via_core = |after: u64, limit: u64| host.list_after(after, limit);
            let res = recorded_pass(host, "core", from, since, top, &mut via_core, &mut forward, &mut commit);
            match res {
                // The derived block only moves on a ubus event; one missed (or rate limited)
                // leaves it at the watermark while new SMS wait. Same summary as last time and
                // nothing to do: confirm with a direct read (cheap: the idle poll is 5 min).
                Ok(0) if repeated && top == from => {}
                Ok(0) => return,
                Ok(n) => return eprintln!("[sms_forward] forwarded {n} message(s) after id {from} (via core)"),
                // whatever got through is committed; the direct read below finishes the job (D2)
                Err(e) => eprintln!("[sms_forward] core sms.list_after failed: {e}; reading ubus directly"),
            }
        }

        // direct: the device max id needs both stores read OK (R15)
        let top = match fetch_baseline(host) {
            Ok((top, _)) => top,
            Err(e) => return eprintln!("[sms_forward] failed to read SMS max id: {e}"),
        };
        let (from, since) = position();
        let mut direct =
            |after: u64, limit: u64| list_after_direct(after, limit, &mut |store, page| ubus_sms_page(host, store, page));
        match recorded_pass(host, "direct", from, since, top, &mut direct, &mut forward, &mut commit) {
            Err(e) => eprintln!("[sms_forward] failed to read SMS after id {from}: {e}"),
            Ok(0) => {}
            Ok(n) => eprintln!("[sms_forward] forwarded {n} message(s) after id {from} (direct)"),
        }
    }

    /// Queues one SMS for every matching rule (saved before the watermark moves past it) and
    /// returns at once: the sends are [`drain`](Self::drain)'s. An SMS whose fingerprint was
    /// seen before (a SIM re-read gave it a new id) is not queued again. A rule that already got
    /// it under the old single hold (`delivered`) is queued as sent.
    fn enqueue(&self, sms: &DecodedSms, config: &SmsForwardConfig, enabled_rules: &[&ForwardRule]) -> Delivery {
        // no longer the owner: this SMS stays above the watermark for the next one
        if !self.host.active() {
            return Delivery::Deferred(HANDED_OVER.into());
        }
        let key = fingerprint(&sms.sender, &sms.date, &sms.content);
        let now = now_ts();
        let mut q = self.queue.lock().unwrap();
        if q.is_seen(&key) {
            eprintln!("[sms_forward] SMS {} already queued before (same sender, date and text): not again", sms.id);
            return Delivery::Delivered;
        }
        let delivered = self.delivered.lock().unwrap().clone();
        let mut steps: Vec<Value> = Vec::new();
        let mut jobs = Vec::new();
        for rule in enabled_rules {
            let dest = destination_type_name(&rule.destination);
            // the firmware's garbled echo of our own SMS forward, or a non-match: nothing to send
            let step = if is_forward_loop(&rule.destination, &sms.sender) && is_garbled_echo(&sms.content) {
                "echo"
            } else if !matches_filter(&rule.filter, &sms.sender, &sms.content) {
                "no_match"
            } else if delivered.contains(&(sms.id, rule.id)) {
                "already"
            } else {
                "queued"
            };
            steps.push(json!({"rule": rule.id, "dest": dest, "step": step}));
            if step == "echo" || step == "no_match" {
                continue;
            }
            jobs.push(Job {
                key: key.clone(),
                rule_id: rule.id,
                sms_id: sms.id,
                sender: sms.sender.clone(),
                content: sms.content.clone(),
                date: sms.date.clone(),
                state: if step == "already" { JobState::Sent } else { JobState::Pending },
                attempts: 0,
                next_at: now,
                created_at: now,
                online_secs: 0,
                last_error: None,
            });
        }
        let nothing_to_send = jobs.is_empty();
        let mut saved = q.clone();
        saved.see(&key, now);
        saved.jobs.extend(jobs);
        if let Err(e) = saved.save(&self.queue_path) {
            // not durable: hold the watermark here and try again on the next pass
            eprintln!("[sms_forward] could not queue SMS {}: {e}", sms.id);
            return Delivery::Deferred(format!("queue: {e}"));
        }
        *q = saved;
        drop(q);
        {
            let mut st = self.state.lock().unwrap();
            if st.held.as_ref().is_some_and(|h| h.sms_id == sms.id) {
                st.held = None;
                self.delivered.lock().unwrap().clear();
            }
        }
        self.host.tick(json!({"kind": "enqueue", "sms_id": sms.id, "wall": now, "rules": steps}));
        // nothing matched: done with it at once (as when every rule succeeded)
        if nothing_to_send {
            self.after_forward(sms.id, &key, config);
        }
        Delivery::Delivered
    }

    /// A rule's jobs can be tried now: it is on and its link is up (a removed rule runs, to be
    /// marked failed).
    fn runnable(&self, cfg: &SmsForwardConfig, rule_id: u32) -> bool {
        match cfg.rules.iter().find(|r| r.id == rule_id) {
            None => true,
            Some(r) if !r.enabled => false,
            Some(r) => match r.destination {
                ForwardDestination::Sms { .. } => self.link.service(),
                _ => self.link.wan(),
            },
        }
    }

    /// How long until a queued job comes due (`None`: nothing waits on a timer).
    fn queue_wait(&self) -> Option<Duration> {
        let cfg = self.config.lock().unwrap().clone();
        if !is_active(&cfg) {
            return None;
        }
        let q = self.queue.lock().unwrap();
        let secs = q.wait_secs(now_ts(), &|r| self.runnable(&cfg, r))?;
        Some(Duration::from_secs(secs.max(1) as u64))
    }

    /// The SMS already on the device at or below the watermark are fingerprinted once (first
    /// start with the queue, or forwarding just switched on), so a SIM re-read that renumbers
    /// them above the watermark does not send them. A failed read tries again next check.
    fn seed(&self, via_core: bool) {
        if self.queue.lock().unwrap().seeded {
            return;
        }
        let wm = self.state.lock().unwrap().last_forwarded_id;
        let host = &*self.host;
        let mut keys = Vec::new();
        let (mut after, mut pages) = (0, 0);
        loop {
            let page = if via_core {
                host.list_after(after, LIST_PAGE)
            } else {
                list_after_direct(after, LIST_PAGE, &mut |store, page| ubus_sms_page(host, store, page))
            };
            let page = match page {
                Ok(p) => p,
                Err(e) => return eprintln!("[sms_forward] could not fingerprint the SMS on the device: {e}"),
            };
            let last = page.items.iter().map(item_id).max().unwrap_or(after);
            for item in &page.items {
                if let (id, Some(m)) = decode_item(item) {
                    if id <= wm {
                        keys.push(fingerprint(&m.sender, &m.date, &m.content));
                    }
                }
            }
            pages += 1;
            if !page.has_more || last <= after || last > wm || pages >= 2 * LIST_MAX_PAGES {
                break;
            }
            after = last;
        }
        let now = now_ts();
        let mut q = self.queue.lock().unwrap();
        for k in &keys {
            q.see(k, now);
        }
        q.seeded = true;
        q.prune(now);
        self.save_queue(&q);
        eprintln!("[sms_forward] fingerprinted {} SMS already on the device", keys.len());
    }

    /// One try for every due job (per rule the oldest pending one), again while a rule's head
    /// went out; a transient failure backs that job off ([`backoff_secs`]) and its rule waits,
    /// the others go on. Size and age bounds afterwards (an alert for each job given up).
    fn drain(&self, cfg: &SmsForwardConfig, agent: &ureq::Agent) {
        self.tick_online();
        loop {
            let now = now_ts();
            let picks: Vec<(String, u32)> = {
                let mut q = self.queue.lock().unwrap();
                q.clamp(now);
                let due = q.due(now, &|r| self.runnable(cfg, r));
                due.into_iter().map(|i| (q.jobs[i].key.clone(), q.jobs[i].rule_id)).collect()
            };
            if picks.is_empty() {
                break;
            }
            for (n, (key, rule_id)) in picks.iter().enumerate() {
                if !self.host.active() || self.host.hold() {
                    return;
                }
                if n > 0 {
                    self.host.sleep(RULE_GAP);
                }
                self.attempt(key, *rule_id, cfg, agent);
            }
        }
        let given_up = {
            let mut q = self.queue.lock().unwrap();
            let before = q.clone();
            let given_up = q.prune(now_ts());
            if *q != before {
                self.save_queue(&q);
            }
            given_up
        };
        for j in given_up {
            let text = format!("SMS {} not forwarded to rule {}, given up: {}", j.sms_id, j.rule_id, j.last_error.clone().unwrap_or_default());
            eprintln!("[sms_forward] {text}");
            {
                let mut st = self.state.lock().unwrap();
                for e in st.log.iter_mut().filter(|e| e.sms_id == j.sms_id && e.rule_id == j.rule_id && !e.success) {
                    if let Some(err) = e.error.as_mut() {
                        if !err.contains(queue::GIVE_UP_NOTE) {
                            *err = format!("{err}（{}）", queue::GIVE_UP_NOTE);
                        }
                    }
                }
                self.save_state(&st);
            }
            self.host.alert("sms-forward-stuck", &text);
        }
    }

    /// One send of one job; its new state saved, its log line written; when this was the SMS's
    /// last job and every one went out, the device copy is marked read / deleted as configured.
    fn attempt(&self, key: &str, rule_id: u32, cfg: &SmsForwardConfig, agent: &ureq::Agent) {
        let find = |q: &Queue| q.jobs.iter().position(|j| j.key == key && j.rule_id == rule_id && j.state == JobState::Pending);
        let job = {
            let q = self.queue.lock().unwrap();
            match find(&q) {
                Some(i) => q.jobs[i].clone(),
                None => return,
            }
        };
        let rule = cfg.rules.iter().find(|r| r.id == rule_id);
        let msg = DecodedSms { id: job.sms_id, sender: job.sender.clone(), content: job.content.clone(), date: job.date.clone(), late_secs: (now_ts() - job.created_at >= queue::LATE_AFTER_SECS).then(|| now_ts() - job.created_at) };
        let result = match rule {
            None => Err(format!("rule {rule_id} removed {}", send::PERMANENT)),
            Some(r) => forward_to(&*self.host, &r.destination, &msg, agent),
        };
        let dest = rule.map_or("removed", |r| destination_type_name(&r.destination));
        let now = now_ts();
        let finished = {
            let mut q = self.queue.lock().unwrap();
            let Some(i) = find(&q) else { return };
            let j = &mut q.jobs[i];
            match &result {
                Ok(()) => {
                    j.state = JobState::Sent;
                    j.last_error = None;
                }
                Err(e) if !is_transient_error(e) => {
                    j.state = JobState::Failed;
                    j.last_error = Some(e.clone());
                }
                Err(e) => {
                    j.attempts += 1;
                    j.next_at = now + backoff_secs(j.attempts);
                    j.last_error = Some(e.clone());
                    eprintln!("[sms_forward] SMS {} -> {dest} try {} failed: {e}; next in {} s", j.sms_id, j.attempts, backoff_secs(j.attempts));
                }
            }
            self.save_queue(&q);
            (!q.has_pending(key)).then(|| q.all_sent(key))
        };
        if let Some(rule) = rule {
            let entry = ForwardLogEntry {
                timestamp: now,
                sms_id: job.sms_id,
                sender: job.sender.clone(),
                content_preview: preview(&job.content, 80),
                rule_name: rule.name.clone(),
                destination_type: dest.to_string(),
                success: result.is_ok(),
                error: result.as_ref().err().cloned(),
                rule_id,
                content: job.content.clone(),
                date: job.date.clone(),
            };
            let mut state = self.state.lock().unwrap();
            push_log(&mut state.log, entry);
            if state.log.len() > MAX_LOG_ENTRIES {
                let excess = state.log.len() - MAX_LOG_ENTRIES;
                state.log.drain(..excess);
            }
            self.save_state(&state);
        }
        if finished == Some(true) {
            self.after_forward(job.sms_id, key, cfg);
        }
    }

    /// Mark read / delete the device copy (as configured), only if that id still holds the same
    /// SMS: by now a SIM re-read may have given the id to another one.
    fn after_forward(&self, sms_id: u64, key: &str, config: &SmsForwardConfig) {
        if !config.mark_read_after_forward && !config.delete_after_forward {
            return;
        }
        let same = match self.host.list_after(sms_id.saturating_sub(1), 1) {
            Ok(p) => p.items.first().is_some_and(|item| match decode_item(item) {
                (id, Some(m)) => id == sms_id && fingerprint(&m.sender, &m.date, &m.content) == key,
                _ => false,
            }),
            Err(e) => {
                eprintln!("[sms_forward] could not re-read SMS {sms_id}: {e}; left as it is");
                return;
            }
        };
        if !same {
            return eprintln!("[sms_forward] SMS {sms_id} is no longer the forwarded one: left as it is");
        }
        if config.mark_read_after_forward {
            if let Err(e) = self.host.control("sms.mark_read", &json!({"ids": sms_id.to_string(), "tag": 0})) {
                eprintln!("[sms_forward] mark-read failed for SMS {sms_id}: {e}");
            }
        }
        if config.delete_after_forward {
            if let Err(e) = self.host.control("sms.delete", &json!({"ids": sms_id.to_string()})) {
                eprintln!("[sms_forward] delete failed for SMS {sms_id}: {e}");
            }
        }
    }
}

// ── shadow records ──

// A host may replay each pass and each delivery through luyaz-policy's copy. Ids, time keys and outcome
// classes only: sender and body are secret and never written here, so the rule matching (which
// reads them) is not replayed. Only while forwarding is on (`check` returns before otherwise).
// Published through `Host::tick`; what the host does with them is its own business.

fn delivery_kind(d: &Delivery) -> &'static str {
    match d {
        Delivery::Delivered => "delivered",
        Delivery::Skipped(_) => "skipped",
        Delivery::Deferred(_) => "deferred",
    }
}

/// An SMS row as u60-policy sees it: id, received or not, the time key of its date.
fn item_json(item: &Value) -> Value {
    let (id, sms) = decode_item(item);
    json!({"id": id, "received": sms.is_some(), "time": sms.and_then(|m| sms_time_key(&m.date))})
}

/// [`run_pass`] with every page, forward and commit written down; published when the pass read or
/// moved anything.
#[allow(clippy::too_many_arguments)]
fn recorded_pass(
    host: &dyn Host,
    via: &str,
    watermark: u64,
    last_time: Option<i64>,
    max_id: u64,
    fetch: &mut dyn FnMut(u64, u64) -> Result<Page, String>,
    forward: &mut dyn FnMut(&DecodedSms) -> Delivery,
    commit: &mut dyn FnMut(u64, Option<i64>),
) -> Result<usize, String> {
    let (res, rec) = record_pass(via, watermark, last_time, max_id, fetch, forward, commit);
    if let Some(rec) = rec {
        host.tick(rec);
    }
    res
}

fn record_pass(
    via: &str,
    watermark: u64,
    last_time: Option<i64>,
    max_id: u64,
    fetch: &mut dyn FnMut(u64, u64) -> Result<Page, String>,
    forward: &mut dyn FnMut(&DecodedSms) -> Delivery,
    commit: &mut dyn FnMut(u64, Option<i64>),
) -> (Result<usize, String>, Option<Value>) {
    let (mut pages, mut forwards, mut commits) = (Vec::new(), Vec::new(), Vec::new());
    let res = run_pass(
        watermark,
        last_time,
        max_id,
        &mut |after, limit| {
            let r = fetch(after, limit);
            pages.push(match &r {
                Ok(p) => json!({"after": after, "limit": limit, "page": {"items": p.items.iter().map(item_json).collect::<Vec<_>>(), "has_more": p.has_more}}),
                Err(e) => json!({"after": after, "limit": limit, "error": e}),
            });
            r
        },
        &mut |sms| {
            let d = forward(sms);
            forwards.push(json!({"id": sms.id, "outcome": delivery_kind(&d)}));
            d
        },
        &mut |wm, lt| {
            commits.push(json!([wm, lt]));
            commit(wm, lt);
        },
    );
    let rec = (!pages.is_empty() || !commits.is_empty()).then(|| {
        json!({
            "kind": "pass", "via": via, "watermark": watermark, "last_time": last_time, "max_id": max_id,
            "pages": pages, "forwards": forwards, "commits": commits,
            "result": match &res { Ok(n) => json!({"ok": n}), Err(e) => json!({"err": e}) },
        })
    });
    (res, rec)
}

/// Pre-forward connectivity failures (the tests' model of a pass).
#[cfg(test)]
const NO_SERVICE_ERR: &str = "no cellular service (will retry)";
#[cfg(test)]
const NO_WAN_ERR: &str = "no WAN connectivity (will retry)";
/// The host stopped owning forwarding mid-delivery: transient, the next owner resumes.
const HANDED_OVER: &str = "handed over";

/// What happened to one SMS.
#[derive(Debug, Clone, PartialEq)]
enum Delivery {
    /// Every matching rule succeeded (or none matched): move past it.
    Delivered,
    /// Some rule failed permanently (4xx / config): logged, move past it.
    #[allow(dead_code)] // run_pass's contract with luyaz-policy keeps it
    Skipped(String),
    /// Some rule failed transiently (no link, connection error, timeout,
    /// 5xx, retries used up): stop here, resume from this SMS next time.
    Deferred(String),
}

/// Rule errors of one SMS → its outcome (the tests' model of a pass). Any transient error wins:
/// the SMS is held.
#[cfg(test)]
fn delivery_outcome(errors: &[String]) -> Delivery {
    if let Some(e) = errors.iter().find(|e| is_transient_error(e)) {
        return Delivery::Deferred(e.clone());
    }
    match errors.first() {
        Some(e) => Delivery::Skipped(e.clone()),
        None => Delivery::Delivered,
    }
}

/// Append a log entry; a retry of the same SMS + rule whose last entry
/// failed updates that entry instead (a deferred SMS is tried on every
/// wake-up — one line per SMS and rule, still retryable by hand).
fn push_log(log: &mut Vec<ForwardLogEntry>, entry: ForwardLogEntry) {
    if let Some(prev) = log.iter_mut().rev().find(|e| e.sms_id == entry.sms_id && e.rule_id == entry.rule_id) {
        if !prev.success {
            *prev = entry;
            return;
        }
    }
    log.push(entry);
}

// ── paging (T10 / R16) ──────────────────────────────────────────────

/// Page size for `sms.list_after` and the direct reads (the core's limit cap).
const LIST_PAGE: u64 = 50;
/// Pages per store before a direct read gives up (as core: 20 × 50).
const LIST_MAX_PAGES: u64 = 20;

/// One `sms.list_after` page: items with id > after_id, ascending.
#[derive(Debug, Clone, Default)]
pub struct Page {
    pub items: Vec<Value>,
    pub has_more: bool,
}

/// One item → `(id, received SMS if tag 0/1)`. Field names as
/// `zte_libwms_get_sms_data` (core returns the same, with plaintext).
fn decode_item(item: &Value) -> (u64, Option<DecodedSms>) {
    let id = item_id(item);
    // tags 0 (unread) and 1 (read) are received; 2 (sent) and 3 (draft) are not forwarded
    let tag_num = item["tag"]
        .as_u64()
        .or_else(|| item["tag"].as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0);
    if tag_num >= 2 {
        return (id, None);
    }
    let sender_raw = item["number"].as_str().unwrap_or("");
    let content_raw = item["content"].as_str().unwrap_or("");
    let date = item["date"]
        .as_str()
        .or_else(|| item["received_time"].as_str())
        .or_else(|| item["sms_time"].as_str())
        .unwrap_or("")
        .to_string();
    let sender = if is_ucs2_hex(sender_raw) { decode_ucs2_hex(sender_raw) } else { sender_raw.to_string() };
    let content = if is_ucs2_hex(content_raw) { decode_ucs2_hex(content_raw) } else { content_raw.to_string() };
    (id, Some(DecodedSms { id, sender, content, date, late_secs: None }))
}

/// How far before the last handled SMS a received one may be dated and still
/// count as new. SMS times come from the SMS centre, so mail held while the
/// device was off still dates after the last one we forwarded; a little
/// reordering is all this has to absorb.
const REIMPORT_MARGIN_SECS: i64 = 7 * 86_400;

/// A received SMS with a fresh id but an old date: the firmware renumbers the
/// SIM card's stored SMS when it reads a card again (card swap, eSIM switch),
/// so years-old messages show up above the watermark. Skipped (the watermark
/// still moves past them), not forwarded.
fn is_reimported(sms: &DecodedSms, last_time: Option<i64>) -> bool {
    let skip = matches!((sms_time_key(&sms.date), last_time),
        (Some(t), Some(lt)) if t < lt - REIMPORT_MARGIN_SECS);
    if skip {
        eprintln!("[sms_forward] skipping SMS {} dated {}: older than the last forwarded one (re-read from SIM)", sms.id, sms.date);
    }
    skip
}

/// The later of two optional times; a missing one never wins.
fn max_time(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    a.into_iter().chain(b).max()
}

/// How far past the system clock an SMS time may be and still count: the device clock is
/// local time labelled UTC (up to 14 h off) plus SMS-centre skew.
const FUTURE_MARGIN_SECS: i64 = 2 * 86_400;

/// A time (the [`sms_time_key`] scale, seconds since 2000-01-01 UTC) beyond now + margin: a bad
/// SMS date. Kept as the last forwarded time it would make every later SMS look re-imported, so
/// [`SmsForwarder::check`] neither saves nor uses one (the pure [`run_pass`] stays clock-free).
fn sane_time(t: Option<i64>) -> Option<i64> {
    t.filter(|&t| !in_future(t))
}

fn in_future(t: i64) -> bool {
    const UNIX_2000: i64 = 946_684_800;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64 - UNIX_2000)
        .unwrap_or(i64::MAX);
    t > now.saturating_add(FUTURE_MARGIN_SECS)
}

/// Page through the device's SMS above the watermark and forward them. Pure:
/// `fetch(after_id, limit)` is the core's `sms.list_after` or the direct read
/// ([`list_after_direct`]) — the same paging either way; `forward` delivers
/// one SMS; `commit(watermark, last_time)` persists.
///
/// * `max_id` (device max, both stores) == watermark or 0 → nothing to do.
/// * Normal: `after_id` = watermark; each returned item (sent/draft too, the
///   id counter is shared) moves the watermark to its id and is committed at
///   once, so an interruption resumes right after the last handled item.
/// * Rollback (`max_id` < watermark, R15): page the whole list from 0; only
///   after every page read OK, [`plan_forward`] picks what is newer than the
///   last forwarded time. A failed page moves nothing.
///
/// Returns how many SMS were handled; an error after partial progress
/// leaves that progress committed. A [`Delivery::Deferred`] SMS ends the
/// pass without moving past it (nothing after it is sent either, so the
/// order holds); a skipped one moves on like a delivered one.
fn run_pass(
    watermark: u64,
    last_time: Option<i64>,
    max_id: u64,
    fetch: &mut dyn FnMut(u64, u64) -> Result<Page, String>,
    forward: &mut dyn FnMut(&DecodedSms) -> Delivery,
    commit: &mut dyn FnMut(u64, Option<i64>),
) -> Result<usize, String> {
    if max_id == 0 || max_id == watermark {
        return Ok(0);
    }
    let mut sent = 0;
    if max_id < watermark {
        let mut after = 0;
        let mut all = Vec::<Value>::new();
        loop {
            let page = fetch(after, LIST_PAGE)?;
            let last = page.items.iter().map(item_id).max().unwrap_or(after);
            if page.has_more && last <= after {
                return Err(format!("sms.list_after made no progress after id {after}"));
            }
            after = last;
            all.extend(page.items);
            if !page.has_more {
                break;
            }
        }
        let mut snap = DeviceSnapshot { max_id, received: Vec::new() };
        let mut decoded: Vec<DecodedSms> = Vec::new();
        for item in &all {
            let (id, sms) = decode_item(item);
            snap.max_id = snap.max_id.max(id);
            if let Some(sms) = sms {
                snap.received.push((id, sms_time_key(&sms.date)));
                decoded.push(sms);
            }
        }
        let plan = plan_forward(watermark, last_time, Ok(&snap));
        let mut lt = last_time;
        for id in &plan.forward_ids {
            if let Some(sms) = decoded.iter().find(|m| m.id == *id) {
                if let Delivery::Deferred(_) = forward(sms) {
                    // Last time stays before this SMS: the next pass plans it again.
                    return Ok(sent);
                }
                sent += 1;
                lt = max_time(lt, sms_time_key(&sms.date));
                commit(watermark, lt);
            }
        }
        commit(plan.new_watermark, plan.new_last_time);
        return Ok(sent);
    }
    let (mut wm, mut lt) = (watermark, last_time);
    loop {
        let page = fetch(wm, LIST_PAGE)?;
        let mut progressed = false;
        for item in &page.items {
            let (id, sms) = decode_item(item);
            if id <= wm {
                continue;
            }
            progressed = true;
            if let Some(sms) = sms.filter(|m| !is_reimported(m, lt)) {
                if let Delivery::Deferred(_) = forward(&sms) {
                    // Watermark stays before this SMS: resume from it.
                    return Ok(sent);
                }
                sent += 1;
                lt = max_time(lt, sms_time_key(&sms.date));
            }
            wm = id;
            commit(wm, lt);
        }
        if !page.has_more {
            return Ok(sent);
        }
        if !progressed {
            return Err(format!("sms.list_after made no progress after id {wm}"));
        }
    }
}

/// `sms.list_after` without core: the firmware only takes
/// `"order by id desc"`, so each store (NV = 1, SIM = 0; one shared id
/// counter) is read page by page from the top until an id ≤ `after_id`, a
/// short page, or a page with nothing new; merged, deduplicated, ascending,
/// first `limit`. Either store failing fails the whole read (R15). Same
/// rules as the core's own `sms.list_after`.
fn list_after_direct(
    after_id: u64,
    limit: u64,
    fetch: &mut dyn FnMut(u64, u64) -> Result<Value, String>,
) -> Result<Page, String> {
    let mut found: BTreeMap<u64, Value> = BTreeMap::new();
    for store in [1u64, 0] {
        let mut page = 0;
        loop {
            if page >= LIST_MAX_PAGES {
                return Err(format!("mem_store {store}: more than {} SMS above id {after_id}", LIST_MAX_PAGES * LIST_PAGE));
            }
            let reply = fetch(store, page).map_err(|e| format!("mem_store {store}: {e}"))?;
            let items = reply["messages"].as_array().cloned().unwrap_or_default();
            let (mut reached, mut fresh) = (false, false);
            for item in items.iter() {
                let id = item_id(item);
                if id <= after_id {
                    reached = true;
                } else if let std::collections::btree_map::Entry::Vacant(slot) = found.entry(id) {
                    fresh = true;
                    slot.insert(item.clone());
                }
            }
            if reached || !fresh || (items.len() as u64) < LIST_PAGE {
                break;
            }
            page += 1;
        }
    }
    let has_more = found.len() as u64 > limit;
    Ok(Page { items: found.into_values().take(limit as usize).collect(), has_more })
}

/// One `zte_libwms_get_sms_data` read of one store (NV = 1, SIM = 0); `tags` 10 = every kind,
/// 2 = sent only. Long timeout: big lists are slow.
fn sms_query(host: &dyn Host, store: u64, tags: u64, page: u64, per_page: u64, order: &str) -> Result<Value, String> {
    let mut q = serde_json::Map::new();
    q.insert("mem_store".into(), store.into());
    q.insert("tags".into(), tags.into());
    q.insert("page".into(), page.into());
    q.insert("data_per_page".into(), per_page.into());
    q.insert("order_by".into(), order.into());
    host.ubus_read("zwrt_wms", "zte_libwms_get_sms_data", Some(&Value::Object(q).to_string()), true)
}

/// One direct page for [`list_after_direct`] (descending; the firmware rejects ascending).
fn ubus_sms_page(host: &dyn Host, store: u64, page: u64) -> Result<Value, String> {
    sms_query(host, store, 10, page, LIST_PAGE, "order by id desc")
}

// ── both stores ──

/// Rows from the NV store and then the SIM store, one id once (the firmware's "all" store, 2,
/// leaves the SIM rows out, hence two reads). One store failing fails the read: half a list
/// could look like the ids went backwards (R15).
fn fetch_sms_both_stores(host: &dyn Host, tags: u64, page: u64, count: u64, order: &str) -> Result<Vec<Value>, String> {
    let nv = (1, sms_query(host, 1, tags, page, count, order));
    let sim = (0, sms_query(host, 0, tags, page, count, order));
    merge_store_results(vec![nv, sim])
}

/// An SMS row's id, given as a number or a numeric string; 0 when it is neither.
pub fn item_id(item: &Value) -> u64 {
    let raw = &item["id"];
    match raw.as_str() {
        Some(text) => text.parse().unwrap_or(0),
        None => raw.as_u64().unwrap_or(0),
    }
}

/// The merge behind [`fetch_sms_both_stores`], given each store's reply in order.
fn merge_store_results(results: Vec<(u64, Result<Value, String>)>) -> Result<Vec<Value>, String> {
    let mut ids = HashSet::new();
    let mut rows = Vec::new();
    for (store, reply) in results {
        let reply = reply.map_err(|e| format!("mem_store {store}: {e}"))?;
        let listed = reply["messages"].as_array().map(Vec::as_slice).unwrap_or_default();
        rows.extend(listed.iter().filter(|row| ids.insert(item_id(row))).cloned());
    }
    Ok(rows)
}

/// Where a fresh watermark starts: the highest id among the newest rows of both stores (any
/// kind) and the newest received SMS's own time.
fn fetch_baseline(host: &dyn Host) -> Result<(u64, Option<i64>), String> {
    fetch_sms_both_stores(host, 10, 0, 5, "order by id desc").map(|rows| baseline_of(&rows))
}

fn baseline_of(items: &[Value]) -> (u64, Option<i64>) {
    let mut top = (0, None);
    for row in items {
        let (id, sms) = decode_item(row);
        top.0 = top.0.max(id);
        top.1 = max_time(top.1, sms.and_then(|m| sms_time_key(&m.date)));
    }
    top
}

// ── Persistence ─────────────────────────────────────────────────────

impl SmsForwarder {
    /// Settings are saved indented (people read that file); failures are ignored, the next save
    /// tries again.
    fn save_config(&self, config: &SmsForwardConfig) {
        let Ok(text) = serde_json::to_string_pretty(config) else { return };
        let _ = atomic_write(&self.config_path, text.as_bytes());
    }

    /// The queue, durably; a failure is logged (the next transition tries again).
    fn save_queue(&self, q: &Queue) {
        if let Err(e) = q.save(&self.queue_path) {
            eprintln!("[sms_forward] could not save the queue: {e}");
        }
    }

    /// State is saved compact, often.
    fn save_state(&self, state: &ForwardState) {
        if let Ok(text) = serde_json::to_string(state) {
            let _ = atomic_write(&self.state_path, text.as_bytes());
        }
    }
}


/// Forwarding actually runs: master switch on and at least one rule on.
fn is_active(config: &SmsForwardConfig) -> bool {
    config.enabled && config.rules.iter().any(|r| r.enabled)
}

/// Save a changed config. When forwarding goes from not running to running,
/// drop the old baseline: while it was off the watermark stood still, and
/// everything that arrived meanwhile would otherwise go out at once. The
/// next check re-baselines from what is on the device ([`SmsForwarder::init_watermark`]),
/// so only SMS that arrive after switching on are forwarded.
fn commit_config(fwd: &SmsForwarder, was_active: bool, config: &SmsForwardConfig) {
    fwd.save_config(config);
    if !was_active && is_active(config) {
        let mut state = fwd.state.lock().unwrap();
        state.last_forwarded_id = 0;
        state.last_forwarded_time = None;
        state.held = None;
        fwd.save_state(&state);
        drop(state);
        // what is on the device now is fingerprinted again before the first pass
        let mut q = fwd.queue.lock().unwrap();
        q.seeded = false;
        fwd.save_queue(&q);
        eprintln!("[sms_forward] forwarding switched on: baseline reset");
    }
}

/// Write `path.tmp`, then rename it over `path` (a crash mid-write leaves the old file whole);
/// the old file's mode is carried over, a new file gets the default.
fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let keep = fs::metadata(path).ok().map(|m| m.permissions().mode() & 0o7777);
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).open(&tmp)?;
        // After open, not via OpenOptions::mode: that one is masked by the
        // umask and ignored for a .tmp left over from a crash.
        if let Some(m) = keep {
            f.set_permissions(fs::Permissions::from_mode(m))?;
        }
        f.write_all(data)?;
    }
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests;

