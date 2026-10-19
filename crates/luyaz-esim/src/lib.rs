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
//! a reboot, which is always safe. APN auto-selects by the new profile's PLMN.
//! Whether the card switched at all is read back from its own profile list,
//! not taken from lpac's exit; a card that answers catBusy is reported as
//! needing a device restart (or reinsert), with no retries and no cooldown.
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
use serde_json::{Value, json};

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
/// every `IDENTITY_POLL`) the stack is taken as stuck and the device reboots.
/// Data: past `DATA_WAIT` (polled every `NOTIF_DATA_POLL`) the job ends
/// "switched, data not back" (`reason: "no_data"`), with no reboot. Neither is
/// measured on B31 yet: each step's time is logged so they can be calibrated.
const IDENTITY_WAIT: Duration = Duration::from_secs(45);
const IDENTITY_POLL: Duration = Duration::from_secs(2);
const DATA_WAIT: Duration = Duration::from_secs(60);
/// After a switch the card answers `profile list` with transitional errors for
/// a while (upstream B27: 5–25 s of `euicc_init` failures / QMI error 80
/// SimFileNotFound). For `LIST_STALE_WINDOW` after a switch ends, such an error
/// returns the last good list marked `stale`/`updating` instead of a 502 (R4).
/// Not measured on B31. There is no retry thread: lpac calls are not yet
/// serialised (R2), so the UIs' own 1–1.5 s polls are the retry and the window
/// is its bound.
const LIST_STALE_WINDOW: Duration = Duration::from_secs(30);

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
    /// "" otherwise.
    reason: String,
}

pub struct Esim {
    job: Arc<Mutex<JobState>>,
    running: Arc<AtomicBool>,
    host: Arc<dyn Host>,
    list: Arc<Mutex<ListCache>>,
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
            host,
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
            })),
            running: Arc::new(AtomicBool::new(false)),
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
}

/// The device. `data_up` comes from the host program (the agent's netinfo read, u60d's ubus).
pub struct RealHost {
    pub data_up: Box<dyn Fn() -> bool + Send + Sync>,
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
    let child = Command::new(LPAC_BIN)
        .args(args)
        .env("LD_LIBRARY_PATH", LIB_DIR)
        .env("LD_PRELOAD", COMPAT_SHIM)
        .env("LPAC_APDU", "qmi_qrtr")
        .env("LPAC_HTTP", "curl")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn lpac failed: {e}"))?;

    // Watchdog: kill the child if it wedges (e.g. modem unresponsive). A killed
    // child makes wait_with_output return promptly with whatever was written.
    let pid = child.id();
    let done = Arc::new(AtomicBool::new(false));
    {
        let done = Arc::clone(&done);
        std::thread::spawn(move || {
            let step = Duration::from_millis(250);
            let mut waited = Duration::ZERO;
            while waited < timeout {
                if done.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(step);
                waited += step;
            }
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).output();
        });
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("lpac wait failed: {e}"));
    done.store(true, Ordering::Relaxed);
    let out = out?;
    if !out.status.success() && out.status.code().is_none() {
        if let Some(c) = chan {
            run_cmd(&format!("{QMI_PROBE} close {c} 2>/dev/null"), PROBE_TIMEOUT);
        }
    }

    parse_lpac_output(&String::from_utf8_lossy(&out.stdout), out.status.success()).map_err(|e| {
        if e.is_empty() {
            format!(
                "lpac exited with {} and no result (killed after timeout?)",
                out.status
            )
        } else {
            e
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
    output_within_group(Command::new("sh").arg("-c").arg(cmd), limit)
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// A command in its own process group, killed as a group past `limit` (luyaz-api's
/// `health::runner::output_within_group`). stderr is discarded.
fn output_within_group(
    cmd: &mut Command,
    limit: Duration,
) -> std::io::Result<std::process::Output> {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let reader = child.stdout.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    });
    let t0 = std::time::Instant::now();
    let mut nap = Duration::from_millis(1);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if t0.elapsed() < limit => {
                std::thread::sleep(nap);
                nap = (nap * 2).min(Duration::from_millis(20));
            }
            other => {
                // Before wait(): once reaped, the id could be someone else's. Never kill(-0).
                let pid = child.id() as libc::pid_t;
                if pid > 0 {
                    unsafe { libc::kill(-pid, libc::SIGKILL) };
                }
                let _ = child.kill();
                let _ = child.wait();
                // The reader ends when the pipe closes; not joined, in case something escaped
                // the group and still holds it.
                return Err(match other {
                    Err(e) => e,
                    _ => std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!("timed out after {} ms", limit.as_millis()),
                    ),
                });
            }
        }
    };
    let stdout = reader.and_then(|h| h.join().ok()).unwrap_or_default();
    Ok(std::process::Output {
        status,
        stdout,
        stderr: Vec::new(),
    })
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
        match self.host.lpac(&["profile", "list"], LPAC_TIMEOUT_FAST) {
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
        match self.host.lpac(&["notification", "list"], LPAC_TIMEOUT_FAST) {
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
    /// cooldown. Retrying a busy card didn't help in the trials and the wait
    /// guessed at a timeout nobody measured; what clears catBusy is a power cycle
    /// of the card (device restart, or taking the card out and back), so the job
    /// fails with `reason: "card_busy"` and both UIs say exactly that. Whether
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
        let job = Arc::clone(&self.job);
        let running = Arc::clone(&self.running);
        let h = Arc::clone(&self.host);
        let list = Arc::clone(&self.list);
        std::thread::spawn(move || {
            let h: &dyn Host = &*h;
            h.wake(true);
            // The switch (and the simreset / zte_topsw_mdm restart after it) does
            // not go through core: tell it first, so a network-mode change still
            // confirming is not rolled back over the new card (D40). Best effort.
            interrupt();
            // Record the pre-switch identity so we can detect when the ZTE stack has
            // actually moved off it (the robust convergence signal).
            let (old_imsi, _) = ubus_sim_identity(h);
            // Highest pending notification before the enable: anything above it
            // afterwards is this switch's own.
            let seq_before = h
                .lpac(&["notification", "list"], LPAC_TIMEOUT_FAST)
                .ok()
                .and_then(|l| max_notif_seq(&l));

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
            let card_s = h.now().saturating_duration_since(card_t).as_secs();
            if let Err(e) = &res {
                eprintln!("esim: profile enable {iccid}: {e}; card says {card:?}");
            }
            if let EnableOutcome::Failed { busy, msg } = decide_enable(&res, card) {
                h.wake(false);
                let reason = if busy { "card_busy" } else { "not_switched" };
                finish_job_with(&job, &running, Err(msg), false, reason);
                return;
            }

            // Card-local and quick, so done on both paths below (the reboot one
            // can't come back to it).
            let to_send = prune_notifications(h, seq_before);

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
            let ident_s = h.now().saturating_duration_since(t).as_secs();
            if !converged {
                eprintln!(
                    "esim: switch {iccid}: card {card_s} s, identity not back after {ident_s} s: rebooting"
                );
                // Fast path didn't take; reboot recovers cleanly. Keep the wake lock
                // held so the device can't doze before it reboots.
                list.lock().unwrap().switched_at = Some(h.now());
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
        assert!(
            parse_lpac_output("", true)
                .unwrap_err()
                .contains("no result line")
        );
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
        /// the ZTE stack picks up the new card (IMSI changes, sync ok)
        converges: bool,
        /// …but only from this identity read on (0 = the first after the switch)
        converge_at: u32,
        /// the data call never comes back
        no_data: bool,
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
                    if self.busy {
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
            let mut s = self.state.lock().unwrap();
            s.log.push(format!("sh {cmd}"));
            if cmd.contains("get_sync_info") {
                return r#"{"noSyncModuleName":"sync success"}"#.into();
            }
            if cmd.contains("get_sim_info") {
                s.imsi_reads += 1;
                // read 1 is the pre-switch identity
                let imsi = if self.converges && s.imsi_reads > 1 + self.converge_at {
                    "440100000000002"
                } else {
                    "440100000000001"
                };
                return json!({"sim_imsi": imsi, "sim_iccid": ""}).to_string();
            }
            String::new()
        }
        fn exists(&self, _p: &str) -> bool {
            true
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
    }

    #[test]
    fn channel_check_holds_the_slot_and_never_runs_under_a_job() {
        let fake = Arc::new(Fake::default());
        let e = Esim::new(fake.clone());
        // the fake's probe says nothing it knows: unknown, slot freed again
        assert_eq!(e.channel_check(), ChannelCheck::Unknown);
        assert!(!e.busy());
        assert!(
            fake.state
                .lock()
                .unwrap()
                .log
                .iter()
                .any(|l| l.contains("qmi_uim_probe open"))
        );
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
    fn a_switch_that_does_not_converge_reboots() {
        let (j, fake) = switch_with(Fake {
            enable_works: true,
            converges: false,
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

    #[test]
    fn identity_past_the_deadline_reboots() {
        let (j, fake) = switch_with(Fake {
            enable_works: true,
            converges: true,
            converge_at: 30,
            ..Fake::default()
        });
        assert_eq!(j["rebooting"].as_bool(), Some(true), "{j}");
        let s = fake.state.lock().unwrap();
        assert!(s.rebooted);
        let waited =
            Duration::from_secs(2 * s.log.iter().filter(|l| *l == "sleep 2000").count() as u64);
        assert!(
            waited >= IDENTITY_WAIT && waited < IDENTITY_WAIT + IDENTITY_POLL,
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
        assert!(j["message"].as_str().unwrap().contains("data 60 s"), "{j}");
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
}
