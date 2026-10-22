//! The eSIM engine (moved from zte-agent `esim.rs`, Phase 4 step 8e), used by the
//! core's touch compatibility layer. Every side effect (lpac, the shell, the reboot, the
//! data check, the wake lock, sleeping) goes through [`Host`] (the write layer's interrupt before a
//! switch is the caller's, passed to [`Esim::switch`]): the device is
//! [`RealHost`], tests use a fake that never touches a card or reboots.
//!
//! eSIM (removable eUICC) manager — wraps lpac to manage profiles on an
//! eSTK.me / 5ber-style eUICC card in the physical SIM slot.
//!
//! lpac lives under `/data/esim/` (Alpine musl build with the `qmi_qrtr` APDU
//! driver compiled in, plus its shared-lib closure and a `statx` compat shim
//! for the device's older musl — see scripts/esim/). It talks straight to the
//! modem's QMI UIM service over QRTR, coexisting with the ZTE daemons.
//!
//! The platform quirk (verified on-device): `profile enable` switches the CARD
//! immediately, but the ZTE userspace only reads the SIM at daemon init — no
//! runtime QMI/ubus poke makes it re-read. The switch path resyncs it without a
//! full reboot: power-cycle the UIM session (modem re-reads), then restart just
//! `zte_topsw_mdm` (userspace re-reads; nwinfo/data re-dial off its signal).
//! This keeps the daemon sync barrier healthy and reattaches data in under a
//! minute. If identity doesn't converge or the barrier breaks, it falls back to
//! a reboot — at most one per 24 h (`AUTO_REBOOT_EVERY`); past that the job ends
//! `identity_stuck` and the user restarts. APN auto-selects by the new profile's PLMN.
//! Whether the card switched at all is read back from its own profile list,
//! not taken from lpac's exit; a card that answers catBusy is tried again within
//! `BUSY_TOTAL` (cancellable), then reported as needing a device restart (or reinsert).
//! On success, superseded enable/disable notifications are removed and the
//! switch's own are sent once data is back (rules above `plan_notifications`).
//!
//! Slow / network-touching operations (download, delete, notification
//! processing, switch) run on a detached thread guarded by a single-slot job,
//! mirroring the ShellCrash profile manager; the UI polls `/api/esim/job`.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{json, Value};

const LPAC_BIN: &str = "/data/esim/lpac";
const LIB_DIR: &str = "/data/esim/lib";
const COMPAT_SHIM: &str = "/data/esim/lib/libmusl-compat.so";
const WAKE_LOCK: &str = "/sys/power/wake_lock";
const WAKE_UNLOCK: &str = "/sys/power/wake_unlock";
const WAKE_TAG: &str = "esim_op";

/// Quick card-local ops (chip info, profile list) — no network involved.
const LPAC_TIMEOUT_FAST: Duration = Duration::from_secs(30);
/// Ops that talk to SM-DP+ servers (download, notifications).
const LPAC_TIMEOUT_NET: Duration = Duration::from_secs(240);

const MAX_CODE_LEN: usize = 512;
const MAX_NICKNAME_LEN: usize = 64;

/// Reading the profile list back after `profile enable` (the card is the judge
/// of whether the switch happened, not lpac's exit): a few tries, because a
/// timed-out enable can leave the card answering slowly for a moment.
const READBACK_TRIES: usize = 3;
const READBACK_GAP: Duration = Duration::from_millis(1500);
/// Sending this switch's enable/disable notifications happens only once the
/// switch saw the data call back (polled every `NOTIF_DATA_POLL`, see
/// `DATA_WAIT`) and gives lpac `LPAC_TIMEOUT_NOTIF`: while it runs it holds the
/// job slot, and the user may want to switch straight back if the new profile
/// has no service — so it is kept short, and skipped when data is not up.
const NOTIF_DATA_POLL: Duration = Duration::from_secs(5);
const LPAC_TIMEOUT_NOTIF: Duration = Duration::from_secs(45);
/// The switch is checked in three timed steps (resilience audit R5): the card
/// (`read_profiles_retry`), then the ZTE stack's identity, then the data call.
/// Identity: upstream measured up to 28 s on B27; past `IDENTITY_WAIT` (polled
/// every `IDENTITY_POLL`) plus `IDENTITY_GRACE` with neither identity nor data
/// back, the stack is taken as stuck and the device reboots.
/// Data: past `DATA_WAIT` (polled every `NOTIF_DATA_POLL`) the job ends
/// "switched, data not back" (`reason: "no_data"`), with no reboot. B31, 10-20
/// (eUICC, CMLink ⇄ CTM roaming): card 0 s, identity 3 s, data 10 s and 55–60 s —
/// the roaming profile only just made 60 s, so data waits 90 s (identity keeps
/// 45 s: it is the reboot trigger and 3 s is far inside it).
const IDENTITY_WAIT: Duration = Duration::from_secs(45);
/// Slow is not stuck: past `IDENTITY_WAIT` the switch gets this much longer,
/// and the reboot only happens if both identity and the data call are still
/// missing at the end. u60g 10-22: identity came back a few seconds past 45 s
/// with data fine, and the 45 s rule had already rebooted the device for it.
const IDENTITY_GRACE: Duration = Duration::from_secs(75);
const IDENTITY_POLL: Duration = Duration::from_secs(2);
const DATA_WAIT: Duration = Duration::from_secs(90);
/// After a switch the card answers `profile list` with transitional errors for
/// a while (upstream B27: 5–25 s of `euicc_init` failures / QMI error 80
/// SimFileNotFound). For `LIST_STALE_WINDOW` after a switch ends, such an error
/// returns the last good list marked `stale`/`updating` instead of a 502 (R4).
/// B31, 10-20 (eUICC, four CMLink ⇄ CTM switches): the list read fine
/// throughout and no transitional error was logged, so 30 s is headroom only.
/// There is no retry thread: the UIs' own 1–1.5 s polls are the retry (their
/// reads never queue behind the card lock, see [`Card`]) and the window is its
/// bound.
const LIST_STALE_WINDOW: Duration = Duration::from_secs(30);
/// catBusy (P0-3, 10-22): the enable is retried, the card's list re-read before
/// each try (it may have switched after all). Gaps grow from `BUSY_FIRST` by half
/// up to `BUSY_MAX_GAP`, each ±20 %, and no try starts past `BUSY_TOTAL` from the
/// first; `POST /api/esim/cancel` stops the waiting. Upstream (B27) saw catBusy
/// clear after up to 131 s / 13 tries. lpac 2.3.0 prints catBusy (5) and
/// undefinedError (127) both as "unknown", so both are retried: each retry is
/// one more enable on a card that refused, nothing else is touched.
const BUSY_FIRST: Duration = Duration::from_secs(10);
const BUSY_MAX_GAP: Duration = Duration::from_secs(30);
const BUSY_TOTAL: Duration = Duration::from_secs(240);
/// The identity-timeout reboot (the only reboot on the eSIM path, owner's
/// decision 10-22): at most one per `AUTO_REBOOT_EVERY`, the time kept in
/// `AUTO_REBOOT_STAMP` on /data so it survives the reboot. Past the cap the
/// switch ends `reason: "identity_stuck"` and the user restarts by hand: a
/// second loop would also leave the screen without the touch UI (the sentinel
/// gives it up after reboots < 10 min apart).
const AUTO_REBOOT_EVERY: u64 = 24 * 3600;
pub const AUTO_REBOOT_STAMP: &str = "/data/luyaz/core/esim-auto-reboot";
/// With this file present the identity-timeout reboot is off (owner, 10-22: kept,
/// on by default, but switchable): the switch ends `identity_stuck` as past the cap.
pub const AUTO_REBOOT_OFF: &str = "/data/luyaz/core/esim-auto-reboot-off";
/// The card lock across processes: the core's lpac runs and any script's (the
/// kit's `install.sh` takes it with `flock`). tmpfs, so a reboot frees it.
pub const CARD_FLOCK: &str = "/var/run/luyaz-esim.lock";
/// How long the UIs should wait for a switch job before calling it lost: every
/// step's bound (busy retries + the last enable, readback, simreset + mdm
/// restart, identity, data, the notifications). The job carries it as
/// `deadline_s`; UIs that predate it keep their own 240 s.
pub const SWITCH_DEADLINE: Duration = Duration::from_secs(
    BUSY_TOTAL.as_secs()
        + LPAC_TIMEOUT_FAST.as_secs()
        + 10
        + 15 // SIMRESET_TIMEOUT
        + 30 // MDM_RESTART_TIMEOUT
        + IDENTITY_WAIT.as_secs()
        + DATA_WAIT.as_secs()
        + LPAC_TIMEOUT_NOTIF.as_secs(),
);

/* -------------------------------------------------------------- *
 *  State
 * -------------------------------------------------------------- */

#[derive(Clone, Serialize)]
pub struct JobState {
    id: u64,
    kind: String,   // "switch" | "download" | "delete" | "notifications"
    status: String, // "idle" | "running" | "done" | "error"
    message: String,
    iccid: String,
    started_unix: u64,
    finished_unix: u64,
    /// Set when the finished job scheduled a device reboot (switch).
    rebooting: bool,
    /// Machine-readable cause of an error, so the UIs need not parse
    /// `message`: "card_busy" (the card answered catBusy and the profile did
    /// not change — only a device restart or reinserting the card clears it),
    /// "not_switched" (lpac or the card refused; the card still has the old
    /// profile), "no_data" (a `done` switch: the card and the modem are on the
    /// new profile but the data call did not come back in time; no reboot),
    /// "identity_stuck" (`done` with no reboot: the card is on the new
    /// profile, the ZTE stack never picked it up and the 24 h reboot cap was
    /// used — restart by hand), "cancelled" (the busy retries were stopped),
    /// "" otherwise.
    reason: String,
    /// Seconds from `started_unix` the UIs wait before calling the job lost
    /// (switch: `SWITCH_DEADLINE`; 0 = their own default).
    deadline_s: u64,
    /// Enable tries so far (switch; > 1 = the card answered busy and was retried).
    attempts: u32,
}

pub struct Esim {
    job: Arc<Mutex<JobState>>,
    running: Arc<AtomicBool>,
    /// Set by `cancel`: a switch waiting out a busy card stops at its next check.
    cancel: Arc<AtomicBool>,
    host: Arc<Card>,
    list: Arc<Mutex<ListCache>>,
}

/// The host with the card behind one lock (resilience audit R2): every lpac run
/// (its `free_channel` probe, the run, the channel close after a kill) and every
/// `qmi_uim_probe` call holds it, so two never meet on the card and a guessed
/// free channel can't be another lpac's. Jobs wait for it; the UIs' reads use
/// [`Card::try_lpac`] and answer from what they had instead of queueing.
pub struct Card {
    inner: Arc<dyn Host>,
    lock: Mutex<()>,
}

impl Card {
    /// The card for this process (the mutex) and against other processes (the
    /// host's file lock, see `CARD_FLOCK`), released together on drop.
    fn hold(&self) -> Held<'_> {
        let g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        Held(g, self.inner.lock_card(true).unwrap_or(None))
    }
    /// `None` while another call (or another process) has the card.
    fn try_lpac(&self, args: &[&str], timeout: Duration) -> Option<Result<Value, String>> {
        let g = match self.lock.try_lock() {
            Ok(g) => g,
            Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        };
        let _h = Held(g, self.inner.lock_card(false).ok()?);
        Some(self.inner.lpac(args, timeout))
    }
}

/// Both card locks; the file lock goes when its file closes.
struct Held<'a>(
    #[allow(dead_code)] std::sync::MutexGuard<'a, ()>,
    #[allow(dead_code)] Option<std::fs::File>,
);

impl Host for Card {
    fn installed(&self) -> bool {
        self.inner.installed()
    }
    fn lpac(&self, args: &[&str], timeout: Duration) -> Result<Value, String> {
        let _g = self.hold();
        self.inner.lpac(args, timeout)
    }
    fn sh(&self, cmd: &str, limit: Duration) -> String {
        if cmd.starts_with(QMI_PROBE) {
            let _g = self.hold();
            return self.inner.sh(cmd, limit);
        }
        self.inner.sh(cmd, limit)
    }
    fn exists(&self, path: &str) -> bool {
        self.inner.exists(path)
    }
    fn reboot(&self) {
        self.inner.reboot()
    }
    fn data_up(&self) -> bool {
        self.inner.data_up()
    }
    fn wake(&self, hold: bool) {
        self.inner.wake(hold)
    }
    fn sleep(&self, d: Duration) {
        self.inner.sleep(d)
    }
    fn now(&self) -> Instant {
        self.inner.now()
    }
    fn lock_card(&self, wait: bool) -> Result<Option<std::fs::File>, ()> {
        self.inner.lock_card(wait)
    }
    fn last_auto_reboot(&self) -> Option<u64> {
        self.inner.last_auto_reboot()
    }
    fn note_auto_reboot(&self, unix: u64) {
        self.inner.note_auto_reboot(unix)
    }
    fn wall_unix(&self) -> u64 {
        self.inner.wall_unix()
    }
    fn switch_event(&self, ev: SwitchEvent) {
        self.inner.switch_event(ev)
    }
}

/// The last profile list the card answered, and when the last switch ended
/// (see `LIST_STALE_WINDOW`).
#[derive(Default)]
struct ListCache {
    last: Option<(Value, Instant)>,
    switched_at: Option<Instant>,
}

impl Esim {
    pub fn new(host: Arc<dyn Host>) -> Self {
        Self {
            host: Arc::new(Card {
                inner: host,
                lock: Mutex::new(()),
            }),
            job: Arc::new(Mutex::new(JobState {
                id: 0,
                kind: String::new(),
                status: "idle".into(),
                message: String::new(),
                iccid: String::new(),
                started_unix: 0,
                finished_unix: 0,
                rebooting: false,
                reason: String::new(),
                deadline_s: 0,
                attempts: 0,
            })),
            running: Arc::new(AtomicBool::new(false)),
            cancel: Arc::new(AtomicBool::new(false)),
            list: Arc::new(Mutex::new(ListCache::default())),
        }
    }
}

/* -------------------------------------------------------------- *
 *  The host: every side effect goes through it (tests use a fake)
 * -------------------------------------------------------------- */

/// What the engine does to the device. [`RealHost`] is the device; a test host never runs lpac or
/// reboots.
pub trait Host: Send + Sync + 'static {
    /// lpac is deployed (`/data/esim/lpac` and its libraries).
    fn installed(&self) -> bool;
    /// Run lpac; the `data` of its final `{"type":"lpa"}` line.
    fn lpac(&self, args: &[&str], timeout: Duration) -> Result<Value, String>;
    /// A shell command's stdout (ubus reads, the UIM power-cycle, the mdm restart), its process
    /// group killed past `limit` (empty output then). See the `*_TIMEOUT` constants.
    fn sh(&self, cmd: &str, limit: Duration) -> String;
    fn exists(&self, path: &str) -> bool;
    /// Reboot the device (the switch's fallback when the stack does not converge).
    fn reboot(&self);
    /// The data call is up.
    fn data_up(&self) -> bool;
    /// Hold (true) or release the wake lock.
    fn wake(&self, hold: bool);
    fn sleep(&self, d: Duration);
    /// Monotonic now (tests move it with `sleep`).
    fn now(&self) -> Instant {
        Instant::now()
    }
    /// The card lock other processes see (`CARD_FLOCK`): `Ok(Some)` held until
    /// the file drops, `Ok(None)` no such lock here, `Err` taken (only when
    /// `wait` is false).
    #[allow(clippy::result_unit_err)]
    fn lock_card(&self, _wait: bool) -> Result<Option<std::fs::File>, ()> {
        Ok(None)
    }
    /// Wall-clock seconds of the last identity-timeout reboot, if any.
    fn last_auto_reboot(&self) -> Option<u64> {
        None
    }
    /// About to reboot on the identity timeout: keep `unix` for the cap and
    /// mark the reboot as planned.
    fn note_auto_reboot(&self, _unix: u64) {}
    fn wall_unix(&self) -> u64 {
        now_unix()
    }
    /// Where a switch is (the core's SIM identity tracking; its caches).
    fn switch_event(&self, _ev: SwitchEvent) {}
}

/// A switch's steps as the host hears them.
#[derive(Debug, Clone, PartialEq)]
pub enum SwitchEvent {
    /// Starting: the data call is about to go away for a profile `target`.
    Leaving { target: String },
    /// The card took the profile; the modem stack is being resynced.
    CardSwitched,
    /// The job ended, whichever way (also just before an automatic reboot): its `reason`
    /// (`JobState::reason`, "" = none) and whether it scheduled the reboot.
    Ended { reason: String, rebooting: bool },
}

/// May the identity timeout reboot now? Not within `AUTO_REBOOT_EVERY` of the
/// last one; a stamp in the future (the clock stepped back at boot) counts as
/// recent.
fn auto_reboot_allowed(last: Option<u64>, now: u64) -> bool {
    match last {
        None => true,
        Some(t) => t <= now && now - t >= AUTO_REBOOT_EVERY,
    }
}

/// The wait before busy retry `n` (1 = the first): `BUSY_FIRST` growing by half
/// to `BUSY_MAX_GAP`, ±20 % from `seed`.
fn busy_gap(n: u32, seed: u64) -> Duration {
    let mut ms = BUSY_FIRST.as_millis() as u64;
    for _ in 1..n {
        ms = (ms * 3 / 2).min(BUSY_MAX_GAP.as_millis() as u64);
    }
    let x = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(u64::from(n).wrapping_mul(1442695040888963407));
    let pct = 80 + (x >> 33) % 41; // 80..=120
    Duration::from_millis(ms * pct / 100)
}

/// The device. `data_up` comes from the host program (core: rmnet address + route);
/// `planned_reboot` marks the coming reboot as asked for (core's boot ledger).
pub struct RealHost {
    pub data_up: Box<dyn Fn() -> bool + Send + Sync>,
    pub planned_reboot: Box<dyn Fn() + Send + Sync>,
    pub switch_event: Box<dyn Fn(SwitchEvent) + Send + Sync>,
}

impl Host for RealHost {
    fn installed(&self) -> bool {
        installed()
    }
    fn lpac(&self, args: &[&str], timeout: Duration) -> Result<Value, String> {
        run_lpac(args, timeout)
    }
    fn sh(&self, cmd: &str, limit: Duration) -> String {
        run_cmd(cmd, limit)
    }
    fn exists(&self, path: &str) -> bool {
        Path::new(path).exists()
    }
    fn reboot(&self) {
        let _ = Command::new("sh").args(["-c", "sleep 2; reboot"]).spawn();
    }
    fn data_up(&self) -> bool {
        (self.data_up)()
    }
    fn wake(&self, hold: bool) {
        let _ = std::fs::write(if hold { WAKE_LOCK } else { WAKE_UNLOCK }, WAKE_TAG);
    }
    fn sleep(&self, d: Duration) {
        std::thread::sleep(d)
    }
    fn lock_card(&self, wait: bool) -> Result<Option<std::fs::File>, ()> {
        let Ok(f) = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(CARD_FLOCK)
        else {
            return Ok(None);
        };
        if wait {
            Ok(f.lock().ok().map(|_| f))
        } else {
            match f.try_lock() {
                Ok(()) => Ok(Some(f)),
                Err(std::fs::TryLockError::WouldBlock) => Err(()),
                Err(_) => Ok(None),
            }
        }
    }
    fn last_auto_reboot(&self) -> Option<u64> {
        std::fs::read_to_string(AUTO_REBOOT_STAMP)
            .ok()?
            .trim()
            .parse()
            .ok()
    }
    fn note_auto_reboot(&self, unix: u64) {
        let tmp = format!("{AUTO_REBOOT_STAMP}.tmp");
        if std::fs::write(&tmp, format!("{unix}\n")).is_ok() {
            let _ = std::fs::rename(&tmp, AUTO_REBOOT_STAMP);
        }
        (self.planned_reboot)();
    }
    fn switch_event(&self, ev: SwitchEvent) {
        (self.switch_event)(ev)
    }
}

/* -------------------------------------------------------------- *
 *  lpac invocation
 * -------------------------------------------------------------- */

fn installed() -> bool {
    Path::new(LPAC_BIN).exists() && Path::new(LIB_DIR).is_dir()
}

/// Run lpac and return the `data` field of its final `{"type":"lpa"}` line.
/// lpac emits one JSON object per stdout line: progress lines first, then the
/// lpa result line with code / message / data.
fn run_lpac(args: &[&str], timeout: Duration) -> Result<Value, String> {
    if !installed() {
        return Err("lpac not installed under /data/esim".into());
    }
    // The channel lpac is about to get (the lowest free one). If lpac is
    // killed, the modem keeps that channel open for good (measured 10-18: not
    // reclaimed after 3 min, and two kills used up every free channel), so the
    // watchdog closes it after the kill.
    let chan = free_channel();
    let ran = run_group(
        Command::new(LPAC_BIN)
            .args(args)
            .env("LD_LIBRARY_PATH", LIB_DIR)
            .env("LD_PRELOAD", COMPAT_SHIM)
            .env("LPAC_APDU", "qmi_qrtr")
            .env("LPAC_HTTP", "curl"),
        timeout,
    )
    .map_err(|e| format!("spawn lpac failed: {e}"))?;
    lpac_result(ran, timeout, || {
        if let Some(c) = chan {
            run_cmd(&format!("{QMI_PROBE} close {c} 2>/dev/null"), PROBE_TIMEOUT);
        }
    })
}

/// lpac's answer from how its run ended. `release` runs when lpac was killed (by us or a
/// signal): the modem keeps a killed lpac's channel open for good.
fn lpac_result(ran: Ran, timeout: Duration, release: impl FnOnce()) -> Result<Value, String> {
    let killed = ran.status.is_none_or(|s| s.code().is_none());
    if killed {
        release();
    }
    let why = if ran.err_tail.is_empty() {
        String::new()
    } else {
        format!(" — stderr: {}", ran.err_tail)
    };
    let Some(status) = ran.status else {
        return Err(if ran.overflow {
            format!("lpac output over {MAX_OUTPUT} bytes: killed")
        } else {
            format!("lpac killed after {} s timeout", timeout.as_secs())
        });
    };
    parse_lpac_output(&String::from_utf8_lossy(&ran.stdout), status.success()).map_err(|e| {
        if e.is_empty() {
            format!("lpac exited with {status} and no result{why}")
        } else {
            format!("{e}{why}")
        }
    })
}

/// Lowest free logical channel on the card, read by opening and closing one
/// (`qmi_uim_probe open` prints `[open] channel=N` on stderr).
pub fn free_channel() -> Option<u8> {
    channel_of(&run_cmd(&format!("{QMI_PROBE} open 2>&1"), PROBE_TIMEOUT))
}

/// What the deep diagnosis' channel check found (deep-diag-v2 Q1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelCheck {
    Free(u8),
    /// The modem refused to open one (qmi_error=68): every channel is taken.
    Full,
    /// An eSIM job holds the card: not checked.
    Busy,
    /// No probe deployed, or it said something else (not known to be an eUICC).
    Unknown,
}

/// `qmi_uim_probe open`'s output → free / full (only on qmi_error=68) / unknown.
pub fn channel_check_of(probe_out: &str) -> ChannelCheck {
    match channel_of(probe_out) {
        Some(c) => ChannelCheck::Free(c),
        None if probe_out.contains("qmi_error=68") => ChannelCheck::Full,
        None => ChannelCheck::Unknown,
    }
}

fn channel_of(probe_out: &str) -> Option<u8> {
    let rest = &probe_out[probe_out.find("[open] channel=")? + 15..];
    let n: String = rest.chars().take_while(char::is_ascii_digit).collect();
    n.parse().ok().filter(|c| (1..=19).contains(c))
}

/// lpac's stdout → the `data` of its final `{"type":"lpa"}` line, or the error
/// text. `Err("")` = no result line and lpac did not exit cleanly (the caller
/// words that, it knows the exit status).
fn parse_lpac_output(stdout: &str, exited_ok: bool) -> Result<Value, String> {
    for line in stdout.lines() {
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v["type"].as_str() != Some("lpa") {
            continue;
        }
        let payload = &v["payload"];
        let code = payload["code"].as_i64().unwrap_or(-1);
        if code == 0 {
            return Ok(payload["data"].clone());
        }
        let msg = payload["message"].as_str().unwrap_or("unknown");
        let detail = payload["data"].as_str().unwrap_or("");
        return Err(if detail.is_empty() {
            format!("lpac: {msg} (code {code})")
        } else {
            format!("lpac: {msg}: {detail} (code {code})")
        });
    }
    if !exited_ok {
        return Err(String::new());
    }
    Err("lpac produced no result line".into())
}

/* -------------------------------------------------------------- *
 *  ZTE stack resync (the no-reboot switch path)
 * -------------------------------------------------------------- */

/// QMI UIM power-cycle helper, deployed alongside lpac. Used to make the modem
/// re-read the card after a profile switch (see qmi_uim_probe.c `simreset`).
const QMI_PROBE: &str = "/data/esim/qmi_uim_probe";
/// The one daemon.conf daemon we restart on a switch. It caches the SIM
/// identity; restarting it makes the ZTE userspace re-read, and nwinfo/data
/// re-dial off its signal on their own — verified on-device to keep the daemon
/// sync barrier healthy. Restarting the whole set is unnecessary.
const MDM_INITD: &str = "/etc/init.d/zte_topsw_mdm";

/* Deadlines for the side commands (resilience audit R1). Each is killed as a process group past
 * its limit, so a wedged probe or init script cannot hold the job slot for good.
 * - PROBE_TIMEOUT: `qmi_uim_probe open/close` is one QMI round trip, well under a second when the
 *   modem answers; `channel_check` used 3 s since 10-18. 5 s leaves room on a busy modem.
 * - SIMRESET_TIMEOUT: the UIM power-cycle waits for the card to come back (a few seconds).
 * - UBUS_TIMEOUT: `ubus -t 5`; the outer limit only catches a ubus CLI that ignores its own.
 * - MDM_RESTART_TIMEOUT: `zte_topsw_mdm restart` (procd stop + start) took a few seconds on
 *   device; 30 s is generous, the switch then falls back to the reboot if nothing converged. */
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const SIMRESET_TIMEOUT: Duration = Duration::from_secs(15);
const UBUS_TIMEOUT: Duration = Duration::from_secs(10);
const MDM_RESTART_TIMEOUT: Duration = Duration::from_secs(30);

/// `sh -c cmd`'s stdout, or "" if it fails to start or outlives `limit` (then its whole process
/// group is killed: the shell and whatever it started).
fn run_cmd(cmd: &str, limit: Duration) -> String {
    match run_group(Command::new("sh").arg("-c").arg(cmd), limit) {
        Ok(Ran {
            status: Some(_),
            stdout,
            ..
        }) => String::from_utf8_lossy(&stdout).into_owned(),
        _ => String::new(),
    }
}

/// The most of a command's stdout kept (core `command::MAX_OUTPUT`); past it the group is
/// killed and the run reports `overflow`. lpac's answers are a few KB.
const MAX_OUTPUT: usize = 1 << 20;
/// The end of stderr kept for error messages.
const ERR_TAIL: usize = 2048;

/// How a [`run_group`] run ended.
struct Ran {
    /// `None`: killed (past its limit, or its output too long).
    status: Option<std::process::ExitStatus>,
    stdout: Vec<u8>,
    /// the last `ERR_TAIL` bytes of stderr
    err_tail: String,
    overflow: bool,
}

/// A command in its own process group (luyaz-api's `health::runner::output_within_group`):
/// killed as a group past `limit` or once stdout passes `MAX_OUTPUT`.
fn run_group(cmd: &mut Command, limit: Duration) -> std::io::Result<Ran> {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    use std::sync::atomic::AtomicBool;
    cmd.process_group(0);
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let overflow = Arc::new(AtomicBool::new(false));
    let out = child.stdout.take().map(|mut pipe| {
        let overflow = Arc::clone(&overflow);
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            while let Ok(n) = pipe.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                if buf.len() + n > MAX_OUTPUT {
                    overflow.store(true, Ordering::SeqCst);
                    break; // the group is killed; dropping the pipe ends its writes too
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            buf
        })
    });
    let err = child.stderr.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let mut tail = Vec::new();
            let mut chunk = [0u8; 4096];
            while let Ok(n) = pipe.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                tail.extend_from_slice(&chunk[..n]);
                if tail.len() > ERR_TAIL {
                    tail.drain(..tail.len() - ERR_TAIL);
                }
            }
            tail
        })
    });
    let t0 = std::time::Instant::now();
    let mut nap = Duration::from_millis(1);
    let status = loop {
        match child.try_wait() {
            // overflowing output can end the run itself (SIGPIPE once the reader stops)
            Ok(Some(_)) if overflow.load(Ordering::SeqCst) => break None,
            Ok(Some(status)) => break Some(status),
            Ok(None) if t0.elapsed() < limit && !overflow.load(Ordering::SeqCst) => {
                std::thread::sleep(nap);
                nap = (nap * 2).min(Duration::from_millis(20));
            }
            Err(e) => {
                kill_group(&mut child);
                return Err(e);
            }
            Ok(_) => {
                kill_group(&mut child);
                break None;
            }
        }
    };
    // The readers end when the pipes close. After a kill they are not joined, in case
    // something escaped the group and still holds a pipe.
    let (stdout, tail) = match status {
        Some(_) => (
            out.and_then(|h| h.join().ok()).unwrap_or_default(),
            err.and_then(|h| h.join().ok()).unwrap_or_default(),
        ),
        None => (Vec::new(), Vec::new()),
    };
    Ok(Ran {
        status,
        stdout,
        err_tail: String::from_utf8_lossy(&tail).trim().to_string(),
        overflow: overflow.load(Ordering::SeqCst),
    })
}

fn kill_group(child: &mut std::process::Child) {
    // Before wait(): once reaped, the id could be someone else's. Never kill(-0).
    let pid = child.id() as libc::pid_t;
    if pid > 0 {
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// (imsi, iccid) the ZTE modem stack currently believes is active — its cached
/// identity, which lags the card's enabled profile until the stack re-reads.
/// The ZTE `sim_iccid` carries a trailing BCD pad nibble ("…072F") and reads
/// empty for a beat right after a re-read, so callers lean on the IMSI.
fn ubus_sim_identity(h: &dyn Host) -> (String, String) {
    let v = serde_json::from_str::<Value>(&h.sh(
        "ubus -t 5 call zwrt_zte_mdm.api get_sim_info '{}'",
        UBUS_TIMEOUT,
    ))
    .unwrap_or(Value::Null);
    let imsi = v["sim_imsi"].as_str().unwrap_or("").to_string();
    let iccid = v["sim_iccid"].as_str().unwrap_or("").to_string();
    (imsi, iccid)
}

/// Has the ZTE stack converged onto the switched-in profile? Primary signal is
/// the IMSI changing away from the pre-switch value (IMSI always populates);
/// a normalized ICCID match is accepted too once `sim_iccid` fills in.
fn switch_converged(h: &dyn Host, target_iccid: &str, old_imsi: &str) -> bool {
    if !ubus_sync_ok(h) {
        return false;
    }
    let (imsi, iccid) = ubus_sim_identity(h);
    // An empty pre-switch IMSI read would make any IMSI look "changed": then
    // only the ICCID counts.
    let imsi_changed = !imsi.is_empty() && !old_imsi.is_empty() && imsi != old_imsi;
    let iccid_matches = iccid_eq(&iccid, target_iccid);
    imsi_changed || iccid_matches
}

/// Is the ZTE daemon sync barrier healthy? A broken barrier after the mdm
/// restart means the stack is wedged and we must fall back to a reboot.
fn ubus_sync_ok(h: &dyn Host) -> bool {
    serde_json::from_str::<Value>(&h.sh(
        "ubus -t 5 call zwrt_topsw_daemon.sync get_sync_info '{}'",
        UBUS_TIMEOUT,
    ))
    .ok()
    .and_then(|v| v["noSyncModuleName"].as_str().map(|s| s == "sync success"))
    .unwrap_or(false)
}

/* -------------------------------------------------------------- *
 *  Checks and the card's answers (the routes are `impl Esim` below)
 * -------------------------------------------------------------- */

fn valid_iccid(s: &str) -> bool {
    (18..=20).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
}

fn body_str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v[key].as_str().map(str::trim).filter(|s| !s.is_empty())
}

/// lpac 2.3.0 maps enableResult 1–4 to names and everything else (5 = catBusy,
/// 127 = undefinedError) to "unknown"; newer builds may spell it out.
fn is_card_busy(err: &str) -> bool {
    err.contains("es10c_enable_profile") && (err.contains(": unknown") || err.contains("catBusy"))
}

/// The card's answers while it settles after a switch: lpac's `euicc_init`
/// failing, or QMI error 80 (SimFileNotFound). catBusy is never one of them.
fn is_transitional(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    !is_card_busy(err)
        && (e.contains("euicc_init")
            || e.contains("simfilenotfound")
            || (e.contains("qmi") && e.contains("80")))
}

/// ICCIDs compared the way the card and the ZTE stack spell them: lpac and
/// `sim_iccid` can carry the BCD pad nibble ("…072F").
/// Is slot 1 (the eSIM card's) out of use on a dual-SIM device: switched off
/// (`sim1_provision_state` "0") or not the slot data is on (`current_sim_slot`)?
/// `get_sim_info`'s `sim_*` fields are always slot 1, `sim2_*` slot 2. Missing
/// fields (a single-slot device, a failed read) mean in use.
fn esim_slot_idle(h: &dyn Host) -> bool {
    let v = serde_json::from_str::<Value>(&h.sh(
        "ubus -t 5 call zwrt_zte_mdm.api get_sim_info '{}' # slots",
        UBUS_TIMEOUT,
    ))
    .unwrap_or(Value::Null);
    let off = v["sim1_provision_state"].as_str() == Some("0");
    let elsewhere = v["current_sim_slot"]
        .as_str()
        .is_some_and(|s| !s.is_empty() && s != "1");
    off || elsewhere
}

fn iccid_eq(a: &str, b: &str) -> bool {
    let a = a.trim().trim_end_matches(['F', 'f']);
    !a.is_empty() && a == b.trim().trim_end_matches(['F', 'f'])
}

/// What the card's own profile list says about the switch target.
#[derive(Debug, Clone, Copy, PartialEq)]
enum CardSays {
    /// The target is the enabled profile.
    Enabled,
    /// The list was read and the target is not enabled (or not on the card).
    NotEnabled,
    /// The list could not be read.
    Unknown,
}

/// `lpac profile list` data → is `iccid` the enabled profile?
fn target_state(list: &Value, iccid: &str) -> CardSays {
    match list.as_array() {
        None => CardSays::Unknown,
        Some(a) => {
            let on = a.iter().any(|p| {
                iccid_eq(p["iccid"].as_str().unwrap_or(""), iccid)
                    && p["profileState"].as_str() == Some("enabled")
            });
            if on {
                CardSays::Enabled
            } else {
                CardSays::NotEnabled
            }
        }
    }
}

#[derive(Debug, PartialEq)]
enum EnableOutcome {
    /// The card is on the target: resync the ZTE stack (simreset + mdm restart,
    /// convergence wait, reboot fallback).
    Resync,
    /// The card did not switch. `busy` = it answered catBusy.
    Failed { busy: bool, msg: String },
}

/// Did the switch happen? The card decides, not lpac's exit: a killed
/// (timed-out) or erroring enable may still have switched the card, and "not
/// in disabled state" means the target was already on. Only when the list
/// can't be read does lpac's own answer stand.
fn decide_enable(lpac: &Result<(), String>, card: CardSays) -> EnableOutcome {
    match (card, lpac) {
        (CardSays::Enabled, _) => EnableOutcome::Resync,
        (CardSays::Unknown, Ok(())) => EnableOutcome::Resync,
        (CardSays::NotEnabled, Ok(())) => EnableOutcome::Failed {
            busy: false,
            msg: "lpac reported success but the card still has the old profile enabled".into(),
        },
        (_, Err(e)) if is_card_busy(e) => EnableOutcome::Failed {
            busy: true,
            // "card is busy" is matched by touch-ui builds before `reason`.
            msg: format!(
                "card is busy (catBusy) — restart the device (or take the card out and put it back), then switch again ({e})"
            ),
        },
        (_, Err(e)) => EnableOutcome::Failed {
            busy: false,
            msg: e.clone(),
        },
    }
}

/// Read the profile list a few times until it answers.
fn read_profiles_retry(h: &dyn Host) -> Option<Value> {
    for i in 0..READBACK_TRIES {
        if i > 0 {
            h.sleep(READBACK_GAP);
        }
        if let Ok(v) = h.lpac(&["profile", "list"], LPAC_TIMEOUT_FAST) {
            if v.is_array() {
                return Some(v);
            }
        }
    }
    None
}

/* ---- pending notifications after a switch ----
 *
 * Every enable on the card queues two notifications for the SM-DP+ servers:
 * "enable" for the profile switched in and "disable" for the one switched out.
 * Until 10-04 the switch never touched them (only download/delete sent theirs),
 * so every back-and-forth piled up more on the card. Rules, conservative:
 *
 * - Superseded = an enable/disable notification for a profile that has a newer
 *   (higher seqNumber) enable/disable notification for the same ICCID. Its
 *   state change was overtaken by a later one; the server only cares about the
 *   latest, and sending it late would report an old state. These are REMOVED
 *   (card-local, no network) on every successful switch, on both the converged
 *   and the reboot path. lpac's `notification remove` loses them for good —
 *   that is the point, and the newer one for the same profile stays.
 * - This switch's own (seqNumber above the highest seen just before enabling,
 *   enable/disable only) are SENT with `process -r`, which removes each one
 *   only after the server accepted it (lpac stops at the first failure and
 *   keeps the rest). Only on the converged path, only once the data call is
 *   back, and with a short timeout; otherwise they stay pending for the eSIM
 *   page's "send notifications".
 * - The latest enable/disable of a profile this switch didn't touch, and every
 *   install/delete notification, are never removed or sent here.
 */

/// seqNumber of one `notification list` entry (lpac prints a number).
fn notif_seq(n: &Value) -> Option<i64> {
    n["seqNumber"]
        .as_i64()
        .or_else(|| n["seqNumber"].as_str().and_then(|s| s.parse().ok()))
}

fn notif_is_state_change(n: &Value) -> bool {
    matches!(
        n["profileManagementOperation"].as_str(),
        Some("enable") | Some("disable")
    )
}

#[derive(Debug, Default, PartialEq)]
struct NotifPlan {
    /// superseded enable/disable — remove without sending
    remove: Vec<i64>,
    /// this switch's enable/disable — send (and remove once delivered)
    send: Vec<i64>,
}

/// `notification list` data (+ the highest seqNumber seen before the enable,
/// None = unknown, then nothing is sent) → what to remove and what to send.
fn plan_notifications(list: &Value, seq_before: Option<i64>) -> NotifPlan {
    let items: Vec<(i64, String)> = list
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|n| notif_is_state_change(n))
                .filter_map(|n| {
                    let iccid = n["iccid"]
                        .as_str()?
                        .trim()
                        .trim_end_matches(['F', 'f'])
                        .to_string();
                    if iccid.is_empty() {
                        return None;
                    }
                    Some((notif_seq(n)?, iccid))
                })
                .collect()
        })
        .unwrap_or_default();
    let mut plan = NotifPlan::default();
    for (seq, iccid) in &items {
        let newest = items
            .iter()
            .filter(|(_, i)| i == iccid)
            .map(|(s, _)| *s)
            .max()
            .unwrap_or(*seq);
        if *seq < newest {
            plan.remove.push(*seq);
        } else if seq_before.is_some_and(|b| *seq > b) {
            plan.send.push(*seq);
        }
    }
    plan.remove.sort_unstable();
    plan.send.sort_unstable();
    plan
}

/// Highest seqNumber of any pending notification (None = list unreadable;
/// Some(-1) = none pending).
fn max_notif_seq(list: &Value) -> Option<i64> {
    list.as_array()
        .map(|a| a.iter().filter_map(notif_seq).max().unwrap_or(-1))
}

/// Remove the superseded notifications now (card-local). Returns the ones to
/// send later. Best effort: any lpac failure just leaves them pending.
fn prune_notifications(h: &dyn Host, seq_before: Option<i64>) -> Vec<i64> {
    let list = match h.lpac(&["notification", "list"], LPAC_TIMEOUT_FAST) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("esim: notification list after switch failed: {e}");
            return Vec::new();
        }
    };
    let plan = plan_notifications(&list, seq_before);
    if !plan.remove.is_empty() {
        let seqs: Vec<String> = plan.remove.iter().map(i64::to_string).collect();
        let mut args = vec!["notification", "remove"];
        args.extend(seqs.iter().map(String::as_str));
        match h.lpac(&args, LPAC_TIMEOUT_FAST) {
            Ok(_) => eprintln!("esim: removed superseded notifications {seqs:?}"),
            Err(e) => eprintln!("esim: removing superseded notifications {seqs:?} failed: {e}"),
        }
    }
    plan.send
}

/// After a converged switch: once data is back, send this switch's
/// notifications as a short "notifications" job. Skipped (left pending) when
/// data doesn't come back in time or another eSIM job holds the slot.
fn send_switch_notifications(
    h: &dyn Host,
    job: &Arc<Mutex<JobState>>,
    running: &Arc<AtomicBool>,
    seqs: Vec<i64>,
) {
    if seqs.is_empty() {
        return;
    }
    // The caller saw data back. This pause lets the UIs (1–1.5 s polls) see
    // the switch job's own result before this job takes the slot.
    h.sleep(NOTIF_DATA_POLL);
    if !h.data_up() {
        eprintln!("esim: data dropped after the switch; notifications {seqs:?} left pending");
        return;
    }
    if running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::Relaxed)
        .is_err()
    {
        return;
    }
    {
        let mut j = job.lock().unwrap();
        let next = j.id.wrapping_add(1);
        *j = JobState {
            id: next,
            kind: "notifications".into(),
            status: "running".into(),
            message: String::new(),
            iccid: String::new(),
            started_unix: now_unix(),
            finished_unix: 0,
            rebooting: false,
            reason: String::new(),
            deadline_s: 0,
            attempts: 0,
        };
    }
    let strs: Vec<String> = seqs.iter().map(i64::to_string).collect();
    let mut args = vec!["notification", "process", "-r"];
    args.extend(strs.iter().map(String::as_str));
    let n = seqs.len();
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        h.lpac(&args, LPAC_TIMEOUT_NOTIF)
            .map(|_| format!("sent {n} notification(s) for the switch"))
    }))
    .unwrap_or_else(|_| Err("internal panic".into()));
    if let Err(e) = &res {
        eprintln!("esim: sending switch notifications {strs:?} failed: {e}");
    }
    finish_job(job, running, res, false);
}

/* -------------------------------------------------------------- *
 *  Routes
 * -------------------------------------------------------------- */

impl Esim {
    /// GET /api/esim/status — chip identity + storage. `installed:false` means the
    /// lpac bundle isn't deployed (UI shows setup instructions).
    pub fn status(&self) -> (u16, Value) {
        if !self.host.installed() {
            return (200, json!({"ok": true, "data": {"installed": false}}));
        }
        match self.host.lpac(&["chip", "info"], LPAC_TIMEOUT_FAST) {
            Ok(d) => {
                let info2 = &d["EUICCInfo2"];
                (
                    200,
                    json!({"ok": true, "data": {
                        "installed": true,
                        "eid": d["eidValue"],
                        "default_smdp": d["EuiccConfiguredAddresses"]["defaultDpAddress"],
                        "root_smds": d["EuiccConfiguredAddresses"]["rootDsAddress"],
                        "sgp22_version": info2["profileVersion"],
                        "firmware_version": info2["euiccFirmwareVer"],
                        "free_nvm": info2["extCardResource"]["freeNonVolatileMemory"],
                    }}),
                )
            }
            Err(e) => (502, json!({"ok": false, "error": e})),
        }
    }
    /// GET /api/esim/profiles
    pub fn profiles(&self) -> (u16, Value) {
        if !self.host.installed() {
            return (
                200,
                json!({"ok": true, "data": {"installed": false, "profiles": []}}),
            );
        }
        // While a job runs the card may be mid-operation (or the device about to
        // reboot); skip the APDU round-trip and let the UI keep its last list.
        if self.running.load(Ordering::Relaxed) {
            return (
                200,
                json!({"ok": true, "data": {"installed": true, "busy": true, "profiles": Value::Null}}),
            );
        }
        let Some(res) = self.host.try_lpac(&["profile", "list"], LPAC_TIMEOUT_FAST) else {
            // another read has the card: the last list if there is one, else "busy" as above
            let now = self.host.now();
            return match &self.list.lock().unwrap().last {
                Some((l, at)) => (
                    200,
                    json!({"ok": true, "data": {
                        "installed": true, "busy": false, "profiles": l,
                        "stale": true, "updating": true,
                        "age_s": now.saturating_duration_since(*at).as_secs(),
                    }}),
                ),
                None => (
                    200,
                    json!({"ok": true, "data": {"installed": true, "busy": true, "profiles": Value::Null}}),
                ),
            };
        };
        match res {
            Ok(d) => {
                if d.is_array() {
                    self.list.lock().unwrap().last = Some((d.clone(), self.host.now()));
                }
                (
                    200,
                    json!({"ok": true, "data": {"installed": true, "busy": false, "profiles": d}}),
                )
            }
            Err(e) => {
                let now = self.host.now();
                let c = self.list.lock().unwrap();
                let settling = c
                    .switched_at
                    .is_some_and(|t| now.saturating_duration_since(t) < LIST_STALE_WINDOW);
                match &c.last {
                    Some((l, at)) if settling && is_transitional(&e) => {
                        eprintln!(
                            "esim: profile list while the card settles: {e}; last good list served"
                        );
                        (
                            200,
                            json!({"ok": true, "data": {
                                "installed": true, "busy": false, "profiles": l,
                                "stale": true, "updating": true,
                                "age_s": now.saturating_duration_since(*at).as_secs(),
                            }}),
                        )
                    }
                    _ => (502, json!({"ok": false, "error": e})),
                }
            }
        }
    }
    /// GET /api/esim/notifications
    pub fn notifications(&self) -> (u16, Value) {
        if !self.host.installed() {
            return (200, json!({"ok": true, "data": []}));
        }
        if self.running.load(Ordering::Relaxed) {
            return (200, json!({"ok": true, "data": Value::Null}));
        }
        // another call has the card: null, the same answer as under a job
        let Some(res) = self
            .host
            .try_lpac(&["notification", "list"], LPAC_TIMEOUT_FAST)
        else {
            return (200, json!({"ok": true, "data": Value::Null}));
        };
        match res {
            Ok(d) => (200, json!({"ok": true, "data": d})),
            Err(e) => (502, json!({"ok": false, "error": e})),
        }
    }
    /// An eSIM job (switch, download, …) is running: the ICCID may flip mid-way.
    pub fn busy(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }
    /// The deep diagnosis' check for a free logical channel (open + close one, no lpac). It
    /// holds the job slot meanwhile, so no switch can start on the card under it, and never
    /// runs while one is going.
    pub fn channel_check(&self) -> ChannelCheck {
        if !self.host.installed() || !self.host.exists(QMI_PROBE) {
            return ChannelCheck::Unknown;
        }
        if self
            .running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::Relaxed)
            .is_err()
        {
            return ChannelCheck::Busy;
        }
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // bounded: a hung probe would keep the job slot (killed past PROBE_TIMEOUT).
            self.host
                .sh(&format!("{QMI_PROBE} open 2>&1"), PROBE_TIMEOUT)
        }))
        .unwrap_or_default();
        self.running.store(false, Ordering::SeqCst);
        channel_check_of(&out)
    }
    /// GET /api/esim/job
    pub fn job(&self) -> (u16, Value) {
        let j = self.job.lock().unwrap().clone();
        (200, json!({"ok": true, "data": j}))
    }
    /// POST /api/esim/cancel — stop a switch that is waiting out a busy card.
    /// Only that wait: once the card took the profile the switch runs to its end.
    pub fn cancel(&self) -> (u16, Value) {
        let j = self.job.lock().unwrap();
        if !self.running.load(Ordering::SeqCst) || j.kind != "switch" || j.attempts == 0 {
            return (409, json!({"ok": false, "error": "no switch to stop"}));
        }
        if !j.message.starts_with("card busy") {
            return (
                409,
                json!({"ok": false, "error": "the switch is past the card step"}),
            );
        }
        self.cancel.store(true, Ordering::SeqCst);
        (200, json!({"ok": true}))
    }
    /// POST /api/esim/nickname — { iccid, nickname } (empty nickname clears).
    /// Synchronous: it's a single local APDU exchange.
    pub fn nickname(&self, body: &[u8]) -> (u16, Value) {
        if self.running.load(Ordering::Relaxed) {
            return (409, json!({"ok": false, "error": "operation in progress"}));
        }
        let v: Value = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
        };
        let iccid = match body_str(&v, "iccid") {
            Some(i) if valid_iccid(i) => i.to_string(),
            _ => return (400, json!({"ok": false, "error": "invalid iccid"})),
        };
        let nickname = v["nickname"].as_str().unwrap_or("").trim().to_string();
        if nickname.len() > MAX_NICKNAME_LEN {
            return (400, json!({"ok": false, "error": "nickname too long"}));
        }
        match self.host.lpac(
            &["profile", "nickname", &iccid, &nickname],
            LPAC_TIMEOUT_FAST,
        ) {
            Ok(_) => (200, json!({"ok": true})),
            Err(e) => (502, json!({"ok": false, "error": e})),
        }
    }
    /// POST /api/esim/switch — { iccid }. Enables the profile on the card, then
    /// moves the ZTE stack onto it (see module docs), rebooting only if that
    /// doesn't converge. The UI warns about the outage before calling.
    ///
    /// History: 9-17 every attempt started a 5–10 min cooldown; 9-26 only a real
    /// catBusy did, after 3 retries a second apart. 10-04: no retries and no
    /// cooldown. 10-22 (P0-3): bounded retries again (`BUSY_TOTAL`, growing gaps,
    /// the list re-read before each, `cancel` stops them) — upstream saw catBusy
    /// clear on its own within ~2 min; past the bound the job still fails with
    /// `reason: "card_busy"` and a restart is what clears it. Whether
    /// the switch happened is read back from the card's profile list — see
    /// `decide_enable` — and on success superseded enable/disable notifications
    /// are removed and this switch's own are sent (see the notifications block).
    ///
    /// `interrupt` runs first on the switch's thread: the caller's D40 `op.interrupt` (only for a
    /// user's switch; the switch and the resync after it do not go through the write layer).
    pub fn switch(&self, body: &[u8], interrupt: Box<dyn FnOnce() + Send>) -> (u16, Value) {
        let v: Value = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
        };
        let iccid = match body_str(&v, "iccid") {
            Some(i) if valid_iccid(i) => i.to_string(),
            _ => return (400, json!({"ok": false, "error": "invalid iccid"})),
        };
        let job_id = match self.try_start_job("switch", &iccid) {
            Some(j) => j,
            None => return (409, json!({"ok": false, "error": "operation in progress"})),
        };
        self.cancel.store(false, Ordering::SeqCst);
        self.job.lock().unwrap().deadline_s = SWITCH_DEADLINE.as_secs();
        let job = Arc::clone(&self.job);
        let running = Arc::clone(&self.running);
        let cancel = Arc::clone(&self.cancel);
        let h = Arc::clone(&self.host);
        let list = Arc::clone(&self.list);
        std::thread::spawn(move || {
            let h: &dyn Host = &*h;
            h.wake(true);
            let run = || {
                // The switch (and the simreset / zte_topsw_mdm restart after it) does
                // not go through core: tell it first, so a network-mode change still
                // confirming is not rolled back over the new card (D40). Best effort.
                interrupt();
                h.switch_event(SwitchEvent::Leaving {
                    target: iccid.clone(),
                });
                // Record the pre-switch identity so we can detect when the ZTE stack has
                // actually moved off it (the robust convergence signal).
                let (old_imsi, _) = ubus_sim_identity(h);
                // Dual-SIM (DSDS, the global device): the eSIM card sits in slot 1 (the
                // removable slot). When the modem has slot 1 switched off or data on the
                // other slot, it does not re-read the card, so the identity never moves
                // and waiting for it ends in a reboot for nothing (10-22, twice).
                let other_slot = esim_slot_idle(h);
                // Highest pending notification before the enable: anything above it
                // afterwards is this switch's own.
                let seq_before = h
                    .lpac(&["notification", "list"], LPAC_TIMEOUT_FAST)
                    .ok()
                    .and_then(|l| max_notif_seq(&l));

                // Step 1, the card: enable, read the list back; a busy card is tried
                // again (see `BUSY_TOTAL`), the list re-read first in case it moved.
                let busy_t = h.now();
                let mut attempt: u32 = 0;
                let mut card_s;
                loop {
                    attempt += 1;
                    job.lock().unwrap().attempts = attempt;
                    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        h.lpac(&["profile", "enable", &iccid], LPAC_TIMEOUT_FAST)
                            .map(|_| ())
                    }))
                    .unwrap_or_else(|_| Err("internal panic".into()));
                    let card_t = h.now();
                    let readback = read_profiles_retry(h);
                    let card = readback
                        .as_ref()
                        .map_or(CardSays::Unknown, |l| target_state(l, &iccid));
                    if let Some(l) = readback {
                        // the list the UIs fall back on while the card settles (R4)
                        list.lock().unwrap().last = Some((l, h.now()));
                    }
                    card_s = h.now().saturating_duration_since(card_t).as_secs();
                    if let Err(e) = &res {
                        eprintln!(
                            "esim: profile enable {iccid} (try {attempt}): {e}; card says {card:?}"
                        );
                    }
                    let EnableOutcome::Failed { busy, msg } = decide_enable(&res, card) else {
                        break;
                    };
                    let gap = busy_gap(attempt, job_id);
                    let past = h.now().saturating_duration_since(busy_t) + gap >= BUSY_TOTAL;
                    if !busy || past || cancel.load(Ordering::SeqCst) {
                        h.wake(false);
                        let (reason, msg) = match (busy, cancel.load(Ordering::SeqCst)) {
                            (true, true) => (
                                "cancelled",
                                format!("{msg} — retries stopped after {attempt} tries"),
                            ),
                            (true, false) => (
                                "card_busy",
                                format!("{msg} — still busy after {attempt} tries"),
                            ),
                            _ => ("not_switched", msg),
                        };
                        finish_job_with(&job, &running, Err(msg), false, reason);
                        return;
                    }
                    job.lock().unwrap().message = format!(
                        "card busy — retrying (try {} in {} s)",
                        attempt + 1,
                        gap.as_secs()
                    );
                    // in 1 s steps, so a cancel is seen at once
                    let until = h.now() + gap;
                    while h.now() < until && !cancel.load(Ordering::SeqCst) {
                        h.sleep(
                            until
                                .saturating_duration_since(h.now())
                                .min(Duration::from_secs(1)),
                        );
                    }
                    if cancel.load(Ordering::SeqCst) {
                        h.wake(false);
                        finish_job_with(
                            &job,
                            &running,
                            Err(format!(
                                "card is busy (catBusy) — retries stopped after {attempt} tries"
                            )),
                            false,
                            "cancelled",
                        );
                        return;
                    }
                }
                job.lock().unwrap().message = "switching".into();
                h.switch_event(SwitchEvent::CardSwitched);

                // Card-local and quick, so done on both paths below (the reboot one
                // can't come back to it).
                let to_send = prune_notifications(h, seq_before);

                if other_slot {
                    eprintln!(
                        "esim: switch {iccid}: card {card_s} s; slot 1 is off or data is on the other slot, nothing to wait for"
                    );
                    list.lock().unwrap().switched_at = Some(h.now());
                    finish_job_with(
                        &job,
                        &running,
                        Ok(format!(
                            "switched on the card ({card_s} s) — the eSIM slot is off or not the data slot, so data stays where it is"
                        )),
                        false,
                        "inactive_slot",
                    );
                    send_switch_notifications(h, &job, &running, to_send);
                    h.wake(false);
                    return;
                }

                // The card is now on the new profile, but the ZTE userspace still holds
                // the old identity — it only re-reads the SIM at daemon init, not on any
                // runtime QMI indication. The fast path avoids a full reboot: (1) power-
                // cycle the UIM session so the *modem* re-reads the new profile, then
                // (2) restart just zte_topsw_mdm so the *userspace* re-reads; nwinfo and
                // data re-dial off its signal on their own. Verified on-device to keep
                // the daemon sync barrier healthy and reattach data in well under a
                // minute. If identity doesn't converge or the barrier breaks, fall back
                // to the always-safe reboot.
                if h.exists(QMI_PROBE) {
                    let _ = h.sh(&format!("{QMI_PROBE} simreset"), SIMRESET_TIMEOUT);
                }
                let _ = h.sh(&format!("{MDM_INITD} restart"), MDM_RESTART_TIMEOUT);

                // Step 2, identity: the stack moves off the old IMSI (or onto the ICCID).
                // Wall time (the ubus reads count too): these are the B31 calibration numbers.
                let t = h.now();
                let mut converged = false;
                while h.now().saturating_duration_since(t) < IDENTITY_WAIT {
                    h.sleep(IDENTITY_POLL);
                    if switch_converged(h, &iccid, &old_imsi) {
                        converged = true;
                        break;
                    }
                }
                if !converged {
                    let g = h.now();
                    while h.now().saturating_duration_since(g) < IDENTITY_GRACE {
                        if switch_converged(h, &iccid, &old_imsi) {
                            converged = true;
                            break;
                        }
                        if h.data_up() {
                            eprintln!(
                                "esim: switch {iccid}: identity slow but data is up: not rebooting"
                            );
                            converged = true;
                            break;
                        }
                        h.sleep(IDENTITY_POLL);
                    }
                }
                let ident_s = h.now().saturating_duration_since(t).as_secs();
                if !converged {
                    list.lock().unwrap().switched_at = Some(h.now());
                    let now = h.wall_unix();
                    if h.exists(AUTO_REBOOT_OFF) {
                        eprintln!(
                            "esim: switch {iccid}: card {card_s} s, identity not back after {ident_s} s: not rebooting (automatic reboot is off: {AUTO_REBOOT_OFF})"
                        );
                        h.wake(false);
                        finish_job_with(
                            &job,
                            &running,
                            Ok(format!(
                                "switched on the card, but the modem has not picked it up after {ident_s} s — restart the device (automatic reboot is off)"
                            )),
                            false,
                            "identity_stuck",
                        );
                        return;
                    }
                    if !auto_reboot_allowed(h.last_auto_reboot(), now) {
                        eprintln!(
                            "esim: switch {iccid}: card {card_s} s, identity not back after {ident_s} s: not rebooting (one automatic reboot per 24 h, already used)"
                        );
                        h.wake(false);
                        finish_job_with(
                            &job,
                            &running,
                            Ok(format!(
                                "switched on the card, but the modem has not picked it up after {ident_s} s — restart the device (no automatic reboot: one already in the last 24 h)"
                            )),
                            false,
                            "identity_stuck",
                        );
                        return;
                    }
                    eprintln!(
                        "esim: switch {iccid}: card {card_s} s, identity not back after {ident_s} s: rebooting"
                    );
                    // Fast path didn't take; reboot recovers cleanly. Keep the wake lock
                    // held so the device can't doze before it reboots.
                    h.note_auto_reboot(now);
                    finish_job(
                        &job,
                        &running,
                        Ok("switched — rebooting to finish".into()),
                        true,
                    );
                    h.reboot();
                    return;
                }

                // Step 3, data: the switch is only done once the data call is back.
                // No reboot when it isn't — that is the user's call (or a later rule).
                let t = h.now();
                let data = loop {
                    if h.data_up() {
                        break true;
                    }
                    if h.now().saturating_duration_since(t) >= DATA_WAIT {
                        break false;
                    }
                    h.sleep(NOTIF_DATA_POLL);
                };
                let data_s = h.now().saturating_duration_since(t).as_secs();
                let steps = format!("card {card_s} s, identity {ident_s} s, data {data_s} s");
                eprintln!(
                    "esim: switch {iccid}: {steps}{}",
                    if data { "" } else { " (not back)" }
                );
                list.lock().unwrap().switched_at = Some(h.now());
                if data {
                    finish_job(
                        &job,
                        &running,
                        Ok(format!("switched — no reboot needed ({steps})")),
                        false,
                    );
                    send_switch_notifications(h, &job, &running, to_send);
                } else {
                    // this switch's notifications stay pending (the eSIM page can send them)
                    finish_job_with(
                        &job,
                        &running,
                        Ok(format!("switched — data not back yet ({steps})")),
                        false,
                        "no_data",
                    );
                }
                h.wake(false);
            };
            run();
            let (reason, rebooting) = {
                let j = job.lock().unwrap();
                (j.reason.clone(), j.rebooting)
            };
            h.switch_event(SwitchEvent::Ended { reason, rebooting });
        });
        (
            200,
            json!({"ok": true, "data": {"job_id": job_id, "status": "running"}}),
        )
    }
    /// POST /api/esim/download — { code, confirmation_code? }. Downloads a new
    /// profile via the activation code (LPA:1$…), then delivers the install
    /// notification. Does NOT enable it — switching stays an explicit step.
    pub fn download(&self, body: &[u8]) -> (u16, Value) {
        let v: Value = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
        };
        let code = match body_str(&v, "code") {
            Some(c) => c.to_string(),
            None => {
                return (
                    400,
                    json!({"ok": false, "error": "missing activation code"}),
                );
            }
        };
        if code.len() > MAX_CODE_LEN || code.chars().any(|c| c.is_control()) {
            return (
                400,
                json!({"ok": false, "error": "invalid activation code"}),
            );
        }
        let conf = v["confirmation_code"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();
        if conf.len() > MAX_CODE_LEN || conf.chars().any(|c| c.is_control()) {
            return (
                400,
                json!({"ok": false, "error": "invalid confirmation code"}),
            );
        }
        let job_id = match self.try_start_job("download", "") {
            Some(j) => j,
            None => return (409, json!({"ok": false, "error": "operation in progress"})),
        };
        let job = Arc::clone(&self.job);
        let running = Arc::clone(&self.running);
        let h = Arc::clone(&self.host);
        std::thread::spawn(move || {
            let h: &dyn Host = &*h;
            h.wake(true);
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut args = vec!["profile", "download", "-a", code.as_str()];
                if !conf.is_empty() {
                    args.push("-c");
                    args.push(conf.as_str());
                }
                h.lpac(&args, LPAC_TIMEOUT_NET)?;
                // Deliver the install notification (spec-required). Best effort:
                // the profile is already on the card even if this leg fails.
                // Superseded enable/disable ones go first, so `-a` doesn't send
                // stale state changes late.
                let _ = prune_notifications(h, None);
                let _ = h.lpac(&["notification", "process", "-a", "-r"], LPAC_TIMEOUT_NET);
                Ok("profile downloaded".to_string())
            }))
            .unwrap_or_else(|_| Err("internal panic".into()));
            h.wake(false);
            finish_job(&job, &running, res, false);
        });
        (
            200,
            json!({"ok": true, "data": {"job_id": job_id, "status": "running"}}),
        )
    }
    /// POST /api/esim/delete — { iccid }. Refuses to delete the enabled profile.
    pub fn delete(&self, body: &[u8]) -> (u16, Value) {
        let v: Value = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
        };
        let iccid = match body_str(&v, "iccid") {
            Some(i) if valid_iccid(i) => i.to_string(),
            _ => return (400, json!({"ok": false, "error": "invalid iccid"})),
        };
        // Check profile state up-front (also serializes against concurrent jobs).
        if self.running.load(Ordering::Relaxed) {
            return (409, json!({"ok": false, "error": "operation in progress"}));
        }
        match self.host.lpac(&["profile", "list"], LPAC_TIMEOUT_FAST) {
            Ok(list) => {
                let found = list
                    .as_array()
                    .and_then(|a| {
                        a.iter()
                            .find(|p| p["iccid"].as_str() == Some(iccid.as_str()))
                    })
                    .cloned();
                match found {
                    None => return (404, json!({"ok": false, "error": "profile not found"})),
                    Some(p) if p["profileState"].as_str() == Some("enabled") => {
                        return (
                            400,
                            json!({"ok": false, "error": "cannot delete the active profile — switch away first"}),
                        );
                    }
                    Some(_) => {}
                }
            }
            Err(e) => return (502, json!({"ok": false, "error": e})),
        }
        let job_id = match self.try_start_job("delete", &iccid) {
            Some(j) => j,
            None => return (409, json!({"ok": false, "error": "operation in progress"})),
        };
        let job = Arc::clone(&self.job);
        let running = Arc::clone(&self.running);
        let h = Arc::clone(&self.host);
        std::thread::spawn(move || {
            let h: &dyn Host = &*h;
            h.wake(true);
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                h.lpac(&["profile", "delete", &iccid], LPAC_TIMEOUT_FAST)?;
                let _ = prune_notifications(h, None);
                let _ = h.lpac(&["notification", "process", "-a", "-r"], LPAC_TIMEOUT_NET);
                Ok("profile deleted".to_string())
            }))
            .unwrap_or_else(|_| Err("internal panic".into()));
            h.wake(false);
            finish_job(&job, &running, res, false);
        });
        (
            200,
            json!({"ok": true, "data": {"job_id": job_id, "status": "running"}}),
        )
    }
    /// POST /api/esim/notifications/process — deliver all pending notifications to
    /// their SM-DP+ servers and remove them from the card.
    pub fn notifications_process(&self, _body: &[u8]) -> (u16, Value) {
        let job_id = match self.try_start_job("notifications", "") {
            Some(j) => j,
            None => return (409, json!({"ok": false, "error": "operation in progress"})),
        };
        let job = Arc::clone(&self.job);
        let running = Arc::clone(&self.running);
        let h = Arc::clone(&self.host);
        std::thread::spawn(move || {
            let h: &dyn Host = &*h;
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                h.lpac(&["notification", "process", "-a", "-r"], LPAC_TIMEOUT_NET)
                    .map(|_| "notifications processed".to_string())
            }))
            .unwrap_or_else(|_| Err("internal panic".into()));
            finish_job(&job, &running, res, false);
        });
        (
            200,
            json!({"ok": true, "data": {"job_id": job_id, "status": "running"}}),
        )
    }
}

/* -------------------------------------------------------------- *
 *  Job lifecycle
 * -------------------------------------------------------------- */

impl Esim {
    fn try_start_job(&self, kind: &str, iccid: &str) -> Option<u64> {
        if self
            .running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::Relaxed)
            .is_err()
        {
            return None;
        }
        let mut j = self.job.lock().unwrap();
        let next = j.id.wrapping_add(1);
        *j = JobState {
            id: next,
            kind: kind.to_string(),
            status: "running".into(),
            message: String::new(),
            iccid: iccid.to_string(),
            started_unix: now_unix(),
            finished_unix: 0,
            rebooting: false,
            reason: String::new(),
            deadline_s: 0,
            attempts: 0,
        };
        Some(next)
    }
}

fn finish_job(
    job: &Arc<Mutex<JobState>>,
    running: &Arc<AtomicBool>,
    res: Result<String, String>,
    rebooting: bool,
) {
    finish_job_with(job, running, res, rebooting, "");
}

fn finish_job_with(
    job: &Arc<Mutex<JobState>>,
    running: &Arc<AtomicBool>,
    res: Result<String, String>,
    rebooting: bool,
    reason: &str,
) {
    {
        let mut j = job.lock().unwrap();
        match res {
            Ok(m) => {
                j.status = "done".into();
                j.message = m;
            }
            Err(e) => {
                j.status = "error".into();
                j.message = e;
            }
        }
        j.rebooting = rebooting;
        j.reason = reason.to_string();
        j.finished_unix = now_unix();
    }
    running.store(false, Ordering::Relaxed);
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hung_side_command_is_killed_with_its_group() {
        // R1: a wedged probe / init script returns "" within its limit, and what it started dies
        // too (the shell's background child would otherwise keep the pipe and run on).
        let mark = format!("/tmp/luyaz-esim-r1-{}", std::process::id());
        let t0 = std::time::Instant::now();
        let out = run_cmd(
            &format!("echo started; (sleep 2; touch {mark}) & sleep 3600"),
            Duration::from_millis(300),
        );
        assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
        assert_eq!(out, "");
        std::thread::sleep(Duration::from_millis(2500));
        assert!(!Path::new(&mark).exists(), "the group was not killed");
        assert_eq!(run_cmd("echo ok", Duration::from_secs(5)), "ok\n");
    }

    fn sh_group(script: &str, limit: Duration) -> Ran {
        run_group(Command::new("sh").arg("-c").arg(script), limit).unwrap()
    }

    #[test]
    fn a_hung_lpac_dies_with_its_children_and_frees_its_channel() {
        // R3: lpac with a child (an external curl, say) that outlives it
        let mark = format!("/tmp/luyaz-esim-r3-{}", std::process::id());
        let ran = sh_group(
            &format!("(sleep 1; touch {mark}) & sleep 600"),
            Duration::from_millis(200),
        );
        assert!(ran.status.is_none());
        let mut released = false;
        let e = lpac_result(ran, Duration::from_secs(30), || released = true).unwrap_err();
        assert!(released && e.contains("timeout"), "{e}");
        std::thread::sleep(Duration::from_millis(1500));
        assert!(!Path::new(&mark).exists(), "the child outlived the kill");
    }

    #[test]
    fn endless_lpac_output_is_cut_off_not_kept() {
        let t0 = std::time::Instant::now();
        let ran = sh_group("head -c 10000000 /dev/zero", Duration::from_secs(20));
        assert!(ran.status.is_none() && ran.overflow && ran.stdout.is_empty());
        assert!(t0.elapsed() < Duration::from_secs(10));
        let e = lpac_result(ran, Duration::from_secs(30), || {}).unwrap_err();
        assert!(e.contains("output over"), "{e}");
    }

    #[test]
    fn lpac_stderr_tail_goes_into_the_error() {
        let ran = sh_group(
            "head -c 10000 /dev/zero | tr '\\0' x >&2; echo 'apdu: no channel' >&2; exit 3",
            Duration::from_secs(5),
        );
        assert!(ran.err_tail.len() <= ERR_TAIL && ran.err_tail.ends_with("apdu: no channel"));
        let mut released = false;
        let e = lpac_result(ran, Duration::from_secs(30), || released = true).unwrap_err();
        assert!(!released, "an lpac that exited on its own keeps no channel");
        assert!(
            e.contains("exited with") && e.contains("stderr:") && e.contains("no channel"),
            "{e}"
        );
        // a normal answer still parses
        let ran = sh_group(
            r#"echo '{"type":"lpa","payload":{"code":0,"message":"success","data":[]}}'"#,
            Duration::from_secs(5),
        );
        assert_eq!(
            lpac_result(ran, Duration::from_secs(30), || {}),
            Ok(json!([]))
        );
    }

    #[test]
    fn the_free_channel_is_read_from_the_probe_output() {
        let out = "  0010: 00 21 6f 1f\n[open] channel=2 SW=9000\n[close] channel 2 released\n";
        assert_eq!(channel_of(out), Some(2));
        assert_eq!(channel_of("[open] channel=3\n"), Some(3));
        assert_eq!(
            channel_of("[!] open channel rejected by modem (qmi_error=68)\n"),
            None
        );
        assert_eq!(
            channel_check_of("[!] open channel rejected by modem (qmi_error=68)\n"),
            ChannelCheck::Full
        );
        assert_eq!(
            channel_check_of("[open] channel=3\n"),
            ChannelCheck::Free(3)
        );
        assert_eq!(
            channel_check_of("sh: qmi_uim_probe: not found\n"),
            ChannelCheck::Unknown
        );
        assert_eq!(channel_of("[open] channel=0\n"), None);
        assert_eq!(channel_of(""), None);
    }

    /// One lpac 2.3.0 run as it prints it: progress lines, then the lpa line.
    fn lpa(code: i64, message: &str, data: Value) -> String {
        format!(
            "{}\n{}\n",
            json!({"type": "progress", "payload": {"code": 0, "message": "es10c_enable_profile", "data": null}}),
            json!({"type": "lpa", "payload": {"code": code, "message": message, "data": data}})
        )
    }

    fn enable_err(stdout: &str) -> Result<(), String> {
        parse_lpac_output(stdout, false).map(|_| ())
    }

    fn profile(iccid: &str, state: &str) -> Value {
        json!({"iccid": iccid, "isdpAid": "A0000005591010FFFFFFFF8900001000", "profileState": state,
               "profileNickname": null, "serviceProviderName": "X", "profileName": "X",
               "iconType": null, "icon": null, "profileClass": "operational"})
    }

    const A: &str = "89852000000000000011";
    const B: &str = "8985200000000000002";

    #[test]
    fn parses_lpac_lines() {
        assert_eq!(
            parse_lpac_output(&lpa(0, "success", json!([profile(A, "enabled")])), true).unwrap()[0]
                ["iccid"],
            A
        );
        assert_eq!(
            enable_err(&lpa(-1, "es10c_enable_profile", json!("unknown"))).unwrap_err(),
            "lpac: es10c_enable_profile: unknown (code -1)"
        );
        // killed by the watchdog: no lpa line
        assert_eq!(
            parse_lpac_output("{\"type\":\"progress\"}\n", false),
            Err(String::new())
        );
        assert!(parse_lpac_output("", true)
            .unwrap_err()
            .contains("no result line"));
    }

    #[test]
    fn busy_only_for_enable_unknown_or_catbusy() {
        // lpac 2.3.0: enableResult 5 falls through to "unknown"
        assert!(is_card_busy(
            &enable_err(&lpa(-1, "es10c_enable_profile", json!("unknown"))).unwrap_err()
        ));
        assert!(is_card_busy(
            "lpac: es10c_enable_profile: catBusy (code -1)"
        ));
        // the reasons lpac 2.3.0 does name (strings in the binary)
        for r in [
            "profile not in disabled state",
            "iccid or aid not found",
            "disallowed by policy",
        ] {
            assert!(
                !is_card_busy(&enable_err(&lpa(-1, "es10c_enable_profile", json!(r))).unwrap_err()),
                "{r}"
            );
        }
        assert!(!is_card_busy(
            "lpac exited with exit status: 9 and no result (killed after timeout?)"
        ));
        assert!(!is_card_busy(
            "lpac: es10b_list_notification: unknown (code -1)"
        ));
    }

    #[test]
    fn card_state_from_profile_list() {
        let list = json!([profile(A, "disabled"), profile(B, "enabled")]);
        assert_eq!(target_state(&list, B), CardSays::Enabled);
        assert_eq!(target_state(&list, A), CardSays::NotEnabled);
        assert_eq!(
            target_state(&list, "89852000000000000099"),
            CardSays::NotEnabled
        );
        // BCD pad nibble on either side
        assert_eq!(
            target_state(&json!([profile("8985200000000000002F", "enabled")]), B),
            CardSays::Enabled
        );
        assert_eq!(target_state(&Value::Null, B), CardSays::Unknown);
    }

    #[test]
    fn the_card_decides_whether_the_switch_happened() {
        let busy = enable_err(&lpa(-1, "es10c_enable_profile", json!("unknown")));
        let timeout: Result<(), String> =
            Err("lpac exited with signal: 9 and no result (killed after timeout?)".into());
        let already = enable_err(&lpa(
            -1,
            "es10c_enable_profile",
            json!("profile not in disabled state"),
        ));
        // an error, but the card is on the target: carry on with the resync
        assert_eq!(
            decide_enable(&timeout, CardSays::Enabled),
            EnableOutcome::Resync
        );
        assert_eq!(
            decide_enable(&busy, CardSays::Enabled),
            EnableOutcome::Resync
        );
        assert_eq!(
            decide_enable(&already, CardSays::Enabled),
            EnableOutcome::Resync
        );
        // lpac said ok but the card still has the old profile
        assert!(matches!(
            decide_enable(&Ok(()), CardSays::NotEnabled),
            EnableOutcome::Failed { busy: false, .. }
        ));
        // catBusy and the card did not move: say restart, no retry
        match decide_enable(&busy, CardSays::NotEnabled) {
            EnableOutcome::Failed { busy: true, msg } => {
                assert!(
                    msg.starts_with("card is busy"),
                    "old touch-ui matches this: {msg}"
                );
                assert!(msg.contains("restart"));
            }
            o => panic!("{o:?}"),
        }
        // the list can't be read: lpac's own answer stands
        assert_eq!(
            decide_enable(&Ok(()), CardSays::Unknown),
            EnableOutcome::Resync
        );
        assert!(matches!(
            decide_enable(&busy, CardSays::Unknown),
            EnableOutcome::Failed { busy: true, .. }
        ));
        assert!(matches!(
            decide_enable(&timeout, CardSays::Unknown),
            EnableOutcome::Failed { busy: false, .. }
        ));
    }

    fn notif(seq: i64, op: &str, iccid: &str) -> Value {
        json!({"seqNumber": seq, "profileManagementOperation": op,
               "notificationAddress": "rsp.example.com", "iccid": iccid})
    }

    #[test]
    fn notifications_superseded_are_removed_this_switch_sent() {
        const C: &str = "89860000000000000033";
        // A→B, B→A earlier (never sent), install of C, delete of a gone one;
        // then this switch A→B queued 9 (enable B) and 10 (disable A).
        let list = json!([
            notif(3, "install", C),
            notif(4, "enable", B),
            notif(5, "disable", A),
            notif(6, "enable", A),
            notif(7, "disable", B),
            notif(8, "delete", "89860000000000000044"),
            notif(9, "enable", B),
            notif(10, "disable", A),
        ]);
        let p = plan_notifications(&list, Some(8));
        assert_eq!(p.remove, vec![4, 5, 6, 7]);
        assert_eq!(p.send, vec![9, 10]);
        // seq before unknown: still prune, send nothing
        let p = plan_notifications(&list, None);
        assert_eq!(p.remove, vec![4, 5, 6, 7]);
        assert!(p.send.is_empty());
    }

    #[test]
    fn notifications_left_alone_when_not_superseded() {
        const C: &str = "89860000000000000033";
        // latest state change of a profile this switch didn't touch (C) stays;
        // install/delete never count; padded ICCIDs match.
        let list = json!([
            notif(1, "disable", C),
            notif(2, "install", B),
            notif(3, "enable", "8985200000000000002F"),
            notif(4, "enable", B),
            notif(5, "delete", A),
        ]);
        let p = plan_notifications(&list, Some(3));
        assert_eq!(p.remove, vec![3]);
        assert_eq!(p.send, vec![4]);
        assert_eq!(
            plan_notifications(&json!([]), Some(-1)),
            NotifPlan::default()
        );
        assert_eq!(
            plan_notifications(&Value::Null, Some(0)),
            NotifPlan::default()
        );
        assert_eq!(max_notif_seq(&list), Some(5));
        assert_eq!(max_notif_seq(&json!([])), Some(-1));
        assert_eq!(max_notif_seq(&Value::Null), None);
    }

    #[test]
    fn job_reports_reason() {
        let job = Arc::new(Mutex::new(
            Esim::new(Arc::new(Fake::default()))
                .job
                .lock()
                .unwrap()
                .clone(),
        ));
        let running = Arc::new(AtomicBool::new(true));
        finish_job_with(
            &job,
            &running,
            Err("card is busy".into()),
            false,
            "card_busy",
        );
        let v = serde_json::to_value(job.lock().unwrap().clone()).unwrap();
        assert_eq!(v["reason"], "card_busy");
        assert_eq!(v["status"], "error");
        finish_job(&job, &running, Ok("x".into()), false);
        assert_eq!(job.lock().unwrap().reason, "");
    }

    /// A card and a ZTE stack that answer as scripted; records what was run.
    #[derive(Default)]
    pub(super) struct Fake {
        /// the profile lpac enables ends up enabled on the card
        enable_works: bool,
        /// lpac's enable answers catBusy (and the card stays put)
        busy: bool,
        /// …only for the first this many enables (0 = always)
        busy_for: u32,
        /// the last automatic reboot's wall time
        stamp: Option<u64>,
        /// `AUTO_REBOOT_OFF` is present
        reboot_off: bool,
        /// the ZTE stack picks up the new card (IMSI changes, sync ok)
        converges: bool,
        /// …but only from this identity read on (0 = the first after the switch)
        converge_at: u32,
        /// the data call never comes back
        no_data: bool,
        /// `get_sim_info`'s `sim1_provision_state` / `current_sim_slot` ("" = absent)
        sim1_provision: &'static str,
        data_slot: &'static str,
        /// each lpac run takes this long (real time), to make calls overlap
        lpac_ms: u64,
        /// lpac runs and probe calls going on right now, and the most seen at once
        on_card: std::sync::atomic::AtomicU32,
        most_on_card: std::sync::atomic::AtomicU32,
        state: Mutex<FakeState>,
    }

    #[derive(Default)]
    pub(super) struct FakeState {
        enabled: String,
        imsi_reads: u32,
        log: Vec<String>,
        rebooted: bool,
        interrupted: bool,
        /// the next this many `profile list` answer a transitional error
        list_fails: u32,
        /// fake clock: what `sleep` was asked for
        slept: Duration,
        enables: u32,
        noted_reboot: Option<u64>,
    }

    pub(super) struct OnCard<'a>(&'a Fake);

    impl Drop for OnCard<'_> {
        fn drop(&mut self) {
            self.0.on_card.fetch_sub(1, Ordering::SeqCst);
        }
    }

    impl Fake {
        /// Count this call as on the card for `lpac_ms`.
        fn on_card(&self) -> OnCard<'_> {
            let n = self.on_card.fetch_add(1, Ordering::SeqCst) + 1;
            self.most_on_card.fetch_max(n, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(self.lpac_ms));
            OnCard(self)
        }
    }

    fn fake_epoch() -> Instant {
        static T: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        *T.get_or_init(Instant::now)
    }

    impl Host for Fake {
        fn installed(&self) -> bool {
            true
        }
        fn lpac(&self, args: &[&str], _t: Duration) -> Result<Value, String> {
            let _on = self.on_card();
            let mut s = self.state.lock().unwrap();
            s.log.push(format!("lpac {}", args.join(" ")));
            match args {
                ["profile", "list"] if s.list_fails > 0 => {
                    s.list_fails -= 1;
                    Err("lpac: euicc_init: QMI error 80 (code -1)".into())
                }
                ["profile", "list"] => Ok(json!([
                    {"iccid": "89852000000000000001", "profileState": if s.enabled == "89852000000000000001" { "enabled" } else { "disabled" }},
                    {"iccid": "89852000000000000002", "profileState": if s.enabled == "89852000000000000002" { "enabled" } else { "disabled" }},
                ])),
                ["profile", "enable", iccid] => {
                    s.enables += 1;
                    if self.busy && (self.busy_for == 0 || s.enables <= self.busy_for) {
                        return Err("lpac: es10c_enable_profile: unknown (code -1)".into());
                    }
                    if self.enable_works {
                        s.enabled = iccid.to_string();
                    }
                    Ok(Value::Null)
                }
                ["notification", "list"] => Ok(json!([])),
                _ => Ok(Value::Null),
            }
        }
        fn sh(&self, cmd: &str, _limit: Duration) -> String {
            let _on = cmd.starts_with(QMI_PROBE).then(|| self.on_card());
            let mut s = self.state.lock().unwrap();
            s.log.push(format!("sh {cmd}"));
            if cmd.contains("get_sync_info") {
                return r#"{"noSyncModuleName":"sync success"}"#.into();
            }
            if cmd.contains("get_sim_info") {
                // the slot read before the switch is not an identity read
                if !cmd.ends_with("# slots") {
                    s.imsi_reads += 1;
                }
                // read 1 is the pre-switch identity
                let imsi = if self.converges && s.imsi_reads > 1 + self.converge_at {
                    "440100000000002"
                } else {
                    "440100000000001"
                };
                let mut v = json!({"sim_imsi": imsi, "sim_iccid": ""});
                if !self.sim1_provision.is_empty() {
                    v["sim1_provision_state"] = self.sim1_provision.into();
                }
                if !self.data_slot.is_empty() {
                    v["current_sim_slot"] = self.data_slot.into();
                }
                return v.to_string();
            }
            String::new()
        }
        fn exists(&self, p: &str) -> bool {
            p != AUTO_REBOOT_OFF || self.reboot_off
        }
        fn reboot(&self) {
            self.state.lock().unwrap().rebooted = true;
        }
        fn data_up(&self) -> bool {
            !self.no_data
        }
        fn wake(&self, _hold: bool) {}
        fn sleep(&self, d: Duration) {
            let mut s = self.state.lock().unwrap();
            s.slept += d;
            s.log.push(format!("sleep {}", d.as_millis()));
        }
        fn now(&self) -> Instant {
            fake_epoch() + self.state.lock().unwrap().slept
        }
        fn last_auto_reboot(&self) -> Option<u64> {
            self.stamp
        }
        fn note_auto_reboot(&self, unix: u64) {
            self.state.lock().unwrap().noted_reboot = Some(unix);
        }
        fn wall_unix(&self) -> u64 {
            1_000_000
        }
    }

    #[test]
    fn channel_check_holds_the_slot_and_never_runs_under_a_job() {
        let fake = Arc::new(Fake::default());
        let e = Esim::new(fake.clone());
        // the fake's probe says nothing it knows: unknown, slot freed again
        assert_eq!(e.channel_check(), ChannelCheck::Unknown);
        assert!(!e.busy());
        assert!(fake
            .state
            .lock()
            .unwrap()
            .log
            .iter()
            .any(|l| l.contains("qmi_uim_probe open")));
        e.running.store(true, Ordering::SeqCst);
        let before = fake.state.lock().unwrap().log.len();
        assert_eq!(e.channel_check(), ChannelCheck::Busy);
        assert_eq!(
            fake.state.lock().unwrap().log.len(),
            before,
            "no probe under a job"
        );
        assert!(e.busy(), "the job's slot is left alone");
    }

    fn switch_with(f: Fake) -> (Value, Arc<Fake>) {
        let (j, fake, _) = switch_on(f);
        (j, fake)
    }

    fn switch_on(f: Fake) -> (Value, Arc<Fake>, Esim) {
        let fake = Arc::new(f);
        let e = Esim::new(fake.clone());
        let f2 = fake.clone();
        let (code, v) = e.switch(
            br#"{"iccid":"89852000000000000002"}"#,
            Box::new(move || f2.state.lock().unwrap().interrupted = true),
        );
        assert_eq!(code, 200, "{v}");
        for _ in 0..500 {
            let (_, j) = e.job();
            if j["data"]["status"] != "running" {
                return (j["data"].clone(), fake, e);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("switch never ended");
    }

    #[test]
    fn a_switch_that_converges_does_not_reboot() {
        let (j, fake) = switch_with(Fake {
            enable_works: true,
            converges: true,
            ..Fake::default()
        });
        assert_eq!(
            (j["status"].as_str(), j["rebooting"].as_bool()),
            (Some("done"), Some(false)),
            "{j}"
        );
        let s = fake.state.lock().unwrap();
        assert!(s.interrupted && !s.rebooted);
        assert!(
            s.log.iter().any(|l| l.ends_with("simreset"))
                && s.log.iter().any(|l| l.ends_with("zte_topsw_mdm restart"))
        );
    }

    #[test]
    fn a_slow_identity_with_data_up_does_not_reboot() {
        let (j, fake) = switch_with(Fake {
            enable_works: true,
            converges: false,
            ..Fake::default()
        });
        assert_eq!(
            (j["status"].as_str(), j["rebooting"].as_bool()),
            (Some("done"), Some(false)),
            "{j}"
        );
        assert!(!fake.state.lock().unwrap().rebooted);
    }

    #[test]
    fn a_switch_that_does_not_converge_reboots() {
        let (j, fake) = switch_with(Fake {
            enable_works: true,
            converges: false,
            no_data: true,
            ..Fake::default()
        });
        assert_eq!(
            (j["status"].as_str(), j["rebooting"].as_bool()),
            (Some("done"), Some(true)),
            "{j}"
        );
        assert!(fake.state.lock().unwrap().rebooted);
    }

    #[test]
    fn a_busy_card_is_not_switched_and_not_rebooted() {
        let (j, fake) = switch_with(Fake {
            busy: true,
            ..Fake::default()
        });
        assert_eq!(
            (j["status"].as_str(), j["reason"].as_str()),
            (Some("error"), Some("card_busy")),
            "{j}"
        );
        let s = fake.state.lock().unwrap();
        assert!(!s.rebooted && !s.log.iter().any(|l| l.contains("zte_topsw_mdm restart")));
    }

    #[test]
    fn a_busy_card_is_retried_within_the_bound() {
        let (j, fake) = switch_with(Fake {
            busy: true,
            ..Fake::default()
        });
        let s = fake.state.lock().unwrap();
        let tries = s.enables;
        assert!(tries > 3, "{tries}");
        assert_eq!(j["attempts"].as_u64(), Some(u64::from(tries)));
        assert!(s.slept < BUSY_TOTAL + LPAC_TIMEOUT_FAST, "{:?}", s.slept);
        // the message touch builds match on before `reason`
        assert!(
            j["message"].as_str().unwrap().contains("card is busy"),
            "{j}"
        );
    }

    #[test]
    fn a_card_busy_for_a_while_switches_in_the_end() {
        let (j, fake) = switch_with(Fake {
            busy: true,
            busy_for: 2,
            enable_works: true,
            converges: true,
            ..Fake::default()
        });
        assert_eq!(
            (
                j["status"].as_str(),
                j["reason"].as_str(),
                j["attempts"].as_u64()
            ),
            (Some("done"), Some(""), Some(3)),
            "{j}"
        );
        let s = fake.state.lock().unwrap();
        // the list is read back after every try
        let lists = s.log.iter().filter(|l| *l == "lpac profile list").count();
        assert!(lists >= 3, "{lists}");
        assert!(!s.rebooted);
    }

    #[test]
    fn busy_retries_can_be_stopped() {
        let fake = Arc::new(Fake {
            busy: true,
            lpac_ms: 20,
            ..Fake::default()
        });
        let e = Esim::new(fake.clone());
        assert_eq!(e.cancel().0, 409, "nothing to stop yet");
        let (code, _) = e.switch(br#"{"iccid":"89852000000000000002"}"#, Box::new(|| {}));
        assert_eq!(code, 200);
        let mut stopped = false;
        for _ in 0..2000 {
            if e.cancel().0 == 200 {
                stopped = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(stopped);
        while e.busy() {
            std::thread::sleep(Duration::from_millis(5));
        }
        let j = e.job().1["data"].clone();
        assert_eq!(
            (j["status"].as_str(), j["reason"].as_str()),
            (Some("error"), Some("cancelled")),
            "{j}"
        );
        assert!(fake.state.lock().unwrap().slept < BUSY_TOTAL);
    }

    #[test]
    fn busy_gaps_grow_and_stay_in_bounds() {
        for seed in 0..50 {
            let g1 = busy_gap(1, seed);
            assert!(
                g1 >= BUSY_FIRST * 8 / 10 && g1 <= BUSY_FIRST * 12 / 10,
                "{g1:?}"
            );
            let g9 = busy_gap(9, seed);
            assert!(
                g9 >= BUSY_MAX_GAP * 8 / 10 && g9 <= BUSY_MAX_GAP * 12 / 10,
                "{g9:?}"
            );
        }
        assert_ne!(busy_gap(1, 1), busy_gap(1, 2), "jittered");
    }

    #[test]
    fn the_switch_tells_the_uis_how_long_to_wait() {
        let (j, _) = switch_with(Fake {
            enable_works: true,
            converges: true,
            ..Fake::default()
        });
        assert_eq!(j["deadline_s"].as_u64(), Some(SWITCH_DEADLINE.as_secs()));
        assert!(SWITCH_DEADLINE > Duration::from_secs(240));
    }

    #[test]
    fn one_automatic_reboot_per_day() {
        assert!(auto_reboot_allowed(None, 1_000_000));
        assert!(!auto_reboot_allowed(Some(1_000_000 - 3600), 1_000_000));
        assert!(auto_reboot_allowed(
            Some(1_000_000 - AUTO_REBOOT_EVERY),
            1_000_000
        ));
        // a stamp from the future: the clock stepped back, count it as recent
        assert!(!auto_reboot_allowed(Some(2_000_000), 1_000_000));
    }

    #[test]
    fn identity_stuck_within_a_day_of_the_last_reboot_does_not_reboot() {
        let (j, fake) = switch_with(Fake {
            enable_works: true,
            converges: false,
            no_data: true,
            stamp: Some(1_000_000 - 600),
            ..Fake::default()
        });
        assert_eq!(
            (
                j["status"].as_str(),
                j["reason"].as_str(),
                j["rebooting"].as_bool()
            ),
            (Some("done"), Some("identity_stuck"), Some(false)),
            "{j}"
        );
        let s = fake.state.lock().unwrap();
        assert!(!s.rebooted && s.noted_reboot.is_none());
    }

    #[test]
    fn identity_stuck_with_the_reboot_off_does_not_reboot() {
        let (j, fake) = switch_with(Fake {
            enable_works: true,
            converges: false,
            no_data: true,
            reboot_off: true,
            ..Fake::default()
        });
        assert_eq!(
            (
                j["status"].as_str(),
                j["reason"].as_str(),
                j["rebooting"].as_bool()
            ),
            (Some("done"), Some("identity_stuck"), Some(false)),
            "{j}"
        );
        let s = fake.state.lock().unwrap();
        assert!(!s.rebooted && s.noted_reboot.is_none());
    }

    #[test]
    fn the_reboot_is_noted_before_it_happens() {
        let (_, fake) = switch_with(Fake {
            enable_works: true,
            converges: false,
            no_data: true,
            ..Fake::default()
        });
        let s = fake.state.lock().unwrap();
        assert!(s.rebooted);
        assert_eq!(s.noted_reboot, Some(1_000_000));
    }

    #[test]
    fn a_slow_identity_within_the_deadline_does_not_reboot() {
        // the stack moves on the 20th read, ≈ 40 s: the old 15 × 2 s loop rebooted here
        let (j, fake) = switch_with(Fake {
            enable_works: true,
            converges: true,
            converge_at: 19,
            ..Fake::default()
        });
        assert_eq!(
            (
                j["status"].as_str(),
                j["rebooting"].as_bool(),
                j["reason"].as_str()
            ),
            (Some("done"), Some(false), Some("")),
            "{j}"
        );
        assert!(
            j["message"].as_str().unwrap().contains("identity 40 s"),
            "{j}"
        );
        assert!(!fake.state.lock().unwrap().rebooted);
    }

    fn card_only(f: Fake) {
        let (j, fake) = switch_with(f);
        assert_eq!(
            (
                j["status"].as_str(),
                j["reason"].as_str(),
                j["rebooting"].as_bool()
            ),
            (Some("done"), Some("inactive_slot"), Some(false)),
            "{j}"
        );
        let s = fake.state.lock().unwrap();
        assert!(!s.rebooted);
        assert_eq!(s.enabled, "89852000000000000002");
        assert!(
            !s.log
                .iter()
                .any(|l| l.contains("restart") || l.contains("simreset")),
            "{:?}",
            s.log
        );
    }

    #[test]
    fn a_switch_with_the_esim_slot_off_only_switches_the_card() {
        card_only(Fake {
            enable_works: true,
            sim1_provision: "0",
            data_slot: "2",
            ..Fake::default()
        });
    }

    #[test]
    fn a_switch_with_data_on_the_other_slot_only_switches_the_card() {
        card_only(Fake {
            enable_works: true,
            sim1_provision: "1",
            data_slot: "2",
            ..Fake::default()
        });
    }

    #[test]
    fn a_switch_on_the_esim_slot_in_use_still_waits_for_identity() {
        let (j, fake) = switch_with(Fake {
            enable_works: true,
            sim1_provision: "1",
            data_slot: "1",
            converges: true,
            converge_at: 100,
            no_data: true,
            ..Fake::default()
        });
        assert_eq!(j["rebooting"].as_bool(), Some(true), "{j}");
        assert!(fake.state.lock().unwrap().rebooted);
    }

    #[test]
    fn identity_past_the_deadline_reboots() {
        let (j, fake) = switch_with(Fake {
            enable_works: true,
            converges: true,
            converge_at: 100,
            no_data: true,
            ..Fake::default()
        });
        assert_eq!(j["rebooting"].as_bool(), Some(true), "{j}");
        let s = fake.state.lock().unwrap();
        assert!(s.rebooted);
        let waited =
            Duration::from_secs(2 * s.log.iter().filter(|l| *l == "sleep 2000").count() as u64);
        assert!(
            waited >= IDENTITY_WAIT + IDENTITY_GRACE
                && waited < IDENTITY_WAIT + IDENTITY_GRACE + 2 * IDENTITY_POLL,
            "{waited:?}"
        );
    }

    #[test]
    fn a_switch_without_data_is_not_a_plain_success_and_does_not_reboot() {
        let (j, fake) = switch_with(Fake {
            enable_works: true,
            converges: true,
            no_data: true,
            ..Fake::default()
        });
        assert_eq!(
            (
                j["status"].as_str(),
                j["reason"].as_str(),
                j["rebooting"].as_bool()
            ),
            (Some("done"), Some("no_data"), Some(false)),
            "{j}"
        );
        assert!(j["message"].as_str().unwrap().contains("data 90 s"), "{j}");
        let s = fake.state.lock().unwrap();
        assert!(!s.rebooted);
        assert!(
            !s.log.iter().any(|l| l.contains("notification process")),
            "notifications stay pending"
        );
    }

    #[test]
    fn the_list_while_the_card_settles_is_the_last_good_one() {
        let (_, fake, e) = switch_on(Fake {
            enable_works: true,
            converges: true,
            ..Fake::default()
        });
        // the notifications job may still hold the slot for a beat
        for _ in 0..500 {
            if !e.busy() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        fake.state.lock().unwrap().list_fails = 2;
        for _ in 0..2 {
            let (code, v) = e.profiles();
            assert_eq!(code, 200, "{v}");
            assert_eq!(
                (
                    v["data"]["stale"].as_bool(),
                    v["data"]["updating"].as_bool()
                ),
                (Some(true), Some(true))
            );
            // the readback after the enable: the new profile already shows enabled
            assert_eq!(v["data"]["profiles"][1]["profileState"], "enabled", "{v}");
        }
        let (code, v) = e.profiles();
        assert_eq!(code, 200);
        assert!(
            v["data"]["stale"].is_null(),
            "fresh list once the card answers: {v}"
        );

        // past the window, the same error is a real failure again
        fake.state.lock().unwrap().slept += LIST_STALE_WINDOW;
        fake.state.lock().unwrap().list_fails = 1;
        assert_eq!(e.profiles().0, 502);
    }

    #[test]
    fn without_a_switch_a_list_error_is_a_502() {
        let fake = Arc::new(Fake::default());
        let e = Esim::new(fake.clone());
        assert_eq!(e.profiles().0, 200);
        fake.state.lock().unwrap().list_fails = 1;
        assert_eq!(e.profiles().0, 502);
    }

    #[test]
    fn transitional_errors_are_told_apart() {
        assert!(is_transitional("lpac: euicc_init: QMI error 80 (code -1)"));
        assert!(is_transitional("lpac: SimFileNotFound"));
        assert!(!is_transitional(
            "lpac: es10c_enable_profile: unknown (code -1)"
        ));
        assert!(!is_transitional(
            "lpac: es10c_enable_profile: catBusy euicc_init"
        ));
        assert!(!is_transitional(
            "lpac: es9p_initiate_authentication: timeout"
        ));
    }

    #[test]
    fn the_card_sees_one_call_at_a_time() {
        let fake = Arc::new(Fake {
            enable_works: true,
            converges: true,
            lpac_ms: 30,
            ..Fake::default()
        });
        let e = Arc::new(Esim::new(fake.clone()));
        let (code, _) = e.switch(br#"{"iccid":"89852000000000000002"}"#, Box::new(|| {}));
        assert_eq!(code, 200);
        let readers: Vec<_> = (0..3)
            .map(|i| {
                let e = e.clone();
                std::thread::spawn(move || {
                    for _ in 0..10 {
                        let (code, _) = match i {
                            0 => e.profiles(),
                            1 => e.notifications(),
                            _ => e.status(),
                        };
                        assert_eq!(code, 200);
                    }
                    let _ = e.channel_check();
                })
            })
            .collect();
        for r in readers {
            r.join().unwrap();
        }
        while e.busy() {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            fake.most_on_card.load(Ordering::SeqCst),
            1,
            "two calls met on the card"
        );
    }

    #[test]
    fn a_read_while_the_card_is_held_answers_from_the_last_list() {
        let fake = Arc::new(Fake::default());
        let e = Esim::new(fake.clone());
        assert!(e.profiles().1["data"]["stale"].is_null());
        let _held = e.host.hold();
        let (code, v) = e.profiles();
        assert_eq!(code, 200);
        assert_eq!(v["data"]["stale"], true, "{v}");
        assert!(v["data"]["profiles"].is_array(), "{v}");
        assert!(e.notifications().1["data"].is_null());
    }
}
