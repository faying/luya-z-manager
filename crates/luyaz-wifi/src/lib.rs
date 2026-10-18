//! wifi_radio — turn the Wi-Fi APs on and off, unconditionally, and prove it.
//!
//! This exists because `wifi::wifi_set` cannot be used to drive Wi-Fi from
//! automation, for three separate reasons found on the device:
//!
//!  1. It diffs before writing. When the value it would write already matches
//!     uci it returns `200 {"note":"no changes"}` and never calls reload. If uci
//!     says `disabled=1` but the AP is somehow still up, retrying through that
//!     endpoint is a permanent no-op — exactly the state a retry needs to fix.
//!  2. It returns before the reload has finished — the reload and its verify run
//!     on a background thread (see `finish_in_background`) so that one Wi-Fi
//!     save cannot pin one of the agent's two HTTP workers for a minute.
//!  3. It reports nothing about whether the radios actually changed.
//!
//! What we toggle: the **AP interfaces** (`wireless.main_2g/main_5g.disabled`),
//! NOT the radios (`wireless.wifi0/wifi1.disabled`). Measured 2026-09-22:
//!
//!   * Disabling the radios destroys the whole wiphy, not just the vdevs. The
//!     phy disappears from `iw phy` entirely, so nothing can scan without first
//!     bringing the radios back — and bringing them back re-creates the AP vdevs
//!     and restarts hostapd, i.e. re-broadcasts the SSID. A scan every minute
//!     would mean the SSID reappearing every minute and clients flapping onto a
//!     network that is about to vanish again.
//!   * Disabling only the AP interfaces leaves the phy up with zero hostapd
//!     processes: no beacon, clients roam away, and a temporary `managed` vdev
//!     can be added for scanning without ever broadcasting. It also avoids the
//!     ACS channel re-selection that a radio restart triggers.
//!
//! The cost is that the radios stay powered. That trade is deliberate and the
//! power draw is still unmeasured — see SCENARIO_ENGINE_PLAN.md.
//!
//! Moved from zte-agent `wifi_radio.rs` and `wifi_scan.rs` (u60-platform.md Phase 4 step 9h) so the
//! agent and u60d run the same code. The writes go through the caller's [`Host`]: the agent sends
//! them to datad's `/control` (with its emergency script behind it), u60d to its own `/control`.
//! The lock is the same in both processes: an in-process flag here (one per process) and the flock on
//! [`LOCK_FILE`] that the sentinel and the core's `/control` (D3) take too.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::process::Command;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

mod run;
pub mod layout;
pub mod scan;

pub use run::output_within;

pub const AP_2G: &str = "wireless.main_2g.disabled";
pub const AP_5G: &str = "wireless.main_5g.disabled";
const HOSTAPD_CTRL: &str = "/data/vendor/wifi/hostapd";
const IFACE_2G: &str = "wlan0";
const IFACE_5G: &str = "wlan2";
/// `hostapd_cli status` gives up after this (the agent's `ubus::CMD_TIMEOUT`).
const CMD_TIMEOUT: Duration = Duration::from_secs(5);

/// Deadlines are asymmetric because the device is. Tearing the APs down settles
/// in well under 10s; bringing both APs *and* both radios back was still
/// reporting nothing at 12s and only complete around 30s. A fixed sleep on the
/// "on" path is how an earlier probe script wrongly concluded the device had
/// failed to recover, so both paths poll instead.
pub const OFF_DEADLINE: Duration = Duration::from_secs(20);
pub const ON_DEADLINE: Duration = Duration::from_secs(45);
const POLL_EVERY: Duration = Duration::from_millis(1500);

/// What the switch needs from the process it runs in: uci reads and the three writes, which each
/// side sends its own way (the agent to datad with the emergency script behind it, u60d to its own
/// `/control`). Only ever called under the lock.
pub trait Host {
    /// One uci value; `None` when it cannot be read.
    fn uci_get(&self, key: &str) -> Option<String>;
    /// Write both AP flags and reload (`wifi.apply` `{set: {AP_2G, AP_5G}, reload: true}`): `Ok(None)`
    /// done, `Ok(Some(e))` written but the reload reported `e` (not fatal: the verify decides),
    /// `Err` not written.
    fn write_aps(&self, ap_2g: bool, ap_5g: bool) -> Result<Option<String>, String>;
    /// The vendor master switch on (`wifi.set_module` `{enabled: 1}`).
    fn vendor_on(&self) -> Result<(), String>;
    /// Reload the `wireless` package as it is (`wifi.reload`).
    fn reload(&self) -> Result<Value, String>;
}

// ── The Wi-Fi lock ──────────────────────────────────────────────────────────
//
// Every writer of the `wireless` uci package must hold a `WifiLock` from the
// first `uci set` until the reload it triggers has been verified: the core's
// scenario engine (`apply` and its rollback), the core's `/control` for a
// caller that does not hold it (D3), and the sentinel (`luyaz-sentinel`
// `wifi.rs`, the watchdog that forces Wi-Fi on when the scenario heartbeat
// stops); touch-ui `scripts/chaos.sh` in tests. Without it a scenario's multi-step apply
// can be interleaved with an admin-UI save — or with the watchdog — and land
// half-applied.
//
// Two layers, taken in this order and released together:
//  * an in-process flag + condvar. `flock` is per open file description, so
//    two threads that each open the file would exclude each other anyway; the
//    flag is there so a waiting thread can time out without polling, and so the
//    guard is `Send` (a std `MutexGuard` is not) — `wifi_set` hands its lock to
//    the thread that runs the reload.
//  * `flock(LOCK_EX)` on `LOCK_FILE`, which the shell writers take with the
//    busybox `flock` applet. The file is opened per acquisition and closed on
//    release, never kept open and never unlinked, and the holder's pid is
//    written into it: the device has no `fuser`, so this is how the core's
//    `/control` sees that its caller holds it already. Nobody kills a holder
//    (the old guard's kill of a wedged agent went with them).

/// Shared with the core's `/control` and touch-ui `scripts/chaos.sh`. Do not rename one side
/// only. Luya-Z rename D2 (manager docs/designs/luyaz-rename.md): was `/tmp/u60-wifi.lock`; the
/// core and the sentinel took both while they were shipped one after the other.
pub const LOCK_FILE: &str = "/tmp/luyaz-wifi.lock";

/// Longest a holder legitimately keeps the lock in one go. A normal `apply` is
/// reload (ubus times out at 30 s) + ON_DEADLINE (≤ 75 s); the value keeps the
/// headroom it was sized with when a longer scan-wake path still took the lock.
///
/// Waiters that give up (the sentinel's and `/control`'s 150 s) wait longer
/// than this, so a holder that is merely mid-reload never fails their round.
/// (The device's busybox `flock` has no `-w`: shell waits are a `flock -n`
/// poll loop with a deadline.)
pub const MAX_HOLD: Duration = Duration::from_secs(120);

/// HTTP handlers: fail fast with 503 rather than park a worker behind an
/// engine apply.
pub const HTTP_WAIT: Duration = Duration::from_secs(5);

/// The engine: outwait any single legitimate holder, including the sentinel's
/// own recovery attempt.
pub const ENGINE_WAIT: Duration = Duration::from_secs(150);

const FLOCK_POLL: Duration = Duration::from_millis(200);

static HELD: Mutex<bool> = Mutex::new(false);
static RELEASED: Condvar = Condvar::new();

/// Proof of holding the Wi-Fi lock. Functions that must only run under the lock
/// take `&WifiLock`, so the compiler enforces it.
pub struct WifiLock {
    file: Option<File>,
}

impl Drop for WifiLock {
    fn drop(&mut self) {
        // Close the file (releasing the flock) before letting the next thread in,
        // so it does not spin on a flock this process still holds.
        drop(self.file.take());
        release_in_process();
    }
}

fn release_in_process() {
    let mut held = HELD.lock().unwrap_or_else(|e| e.into_inner());
    *held = false;
    RELEASED.notify_one();
}

/// Take the Wi-Fi lock, waiting at most `wait` in total.
pub fn lock(wait: Duration) -> Result<WifiLock, String> {
    lock_at(LOCK_FILE, wait)
}

/// [`lock`] on another file (tests, a test root).
pub fn lock_at(path: &str, wait: Duration) -> Result<WifiLock, String> {
    let deadline = Instant::now() + wait;
    {
        let mut held = HELD.lock().unwrap_or_else(|e| e.into_inner());
        while *held {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err("Wi-Fi is being reconfigured; try again shortly".into());
            }
            held = RELEASED.wait_timeout(held, left).unwrap_or_else(|e| e.into_inner()).0;
        }
        *held = true;
    }
    // From here on every early return must give the in-process flag back.
    match flock_file(path, deadline) {
        Ok(file) => Ok(WifiLock { file: Some(file) }),
        Err(e) => {
            release_in_process();
            Err(e)
        }
    }
}

fn flock_file(path: &str, deadline: Instant) -> Result<File, String> {
    // std opens with O_CLOEXEC, so the uci/ubus children we spawn while holding
    // the lock do not inherit it — a backgrounded child must never outlive us
    // holding the lock.
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| format!("open {path}: {e}"))?;
    loop {
        // SAFETY: flock on a valid, owned fd.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            break;
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EWOULDBLOCK) {
            return Err(format!("flock {path}: {err}"));
        }
        if Instant::now() >= deadline {
            return Err(format!("{path} is held by another process (see the pid in it)"));
        }
        std::thread::sleep(FLOCK_POLL);
    }
    // Best effort: the pid is a diagnostic (and `/control`'s own-caller check), not part of the lock.
    let _ = file.set_len(0);
    let _ = writeln!(file, "{}", std::process::id());
    Ok(file)
}

/// Number of running hostapd processes. hostapd is what emits beacons, so zero
/// processes is positive proof that nothing is broadcasting — and unlike
/// `hostapd_cli`, "zero" cannot be confused with "we asked the wrong socket".
fn hostapd_count() -> usize {
    // Bounded: this runs under the lock, and a hung `ps` would hold it until the sentinel gives up.
    let out = match output_within(Command::new("ps").arg("w"), CMD_TIMEOUT) {
        Ok(o) => o,
        Err(_) => return usize::MAX, // unknown — never let this read as "off"
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains("/hostapd ") || l.ends_with("/hostapd"))
        .count()
}

/// `true` only when this interface's hostapd answers and reports ENABLED.
///
/// Use this to prove an AP *is* beaconing, never to prove one is not: when the
/// interface is down the control socket is simply absent, which is
/// indistinguishable from a wrong `-p` path or a hostapd that has not come up
/// yet. Proving presence is safe; proving absence is not.
fn hostapd_enabled(iface: &str) -> bool {
    output_within(Command::new("hostapd_cli").args(["-i", iface, "-p", HOSTAPD_CTRL, "status"]), CMD_TIMEOUT)
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("state=ENABLED"))
        .unwrap_or(false)
}

fn poll_until(deadline: Duration, mut done: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    loop {
        if done() {
            return true;
        }
        if start.elapsed() >= deadline {
            return false;
        }
        std::thread::sleep(POLL_EVERY);
    }
}

/// Is anything broadcasting right now? Cheap, read-only, no side effects.
///
/// Exists so callers can ask "does this need repairing?" before reaching for
/// `apply`, which always writes and reloads — appropriate for a deliberate
/// change, wasteful and disruptive as a health check.
pub fn beaconing() -> bool {
    let n = hostapd_count();
    n > 0 && n != usize::MAX
}

/// Both APs configured on *and* both answering ENABLED — exactly what `apply`'s
/// own verify would accept after `apply(true, true)`. When this holds, applying
/// "both on" again would only tear the wiphy down and rebuild it, dropping every
/// connected client for ~30 s to arrive where it started.
///
/// Only ever used to skip turning Wi-Fi *on*. It proves presence, which is safe;
/// there is no equivalent shortcut for "off" (see `hostapd_enabled`).
pub fn fully_on(h: &dyn Host) -> bool {
    snapshot(h) == (true, true) && beaconing() && hostapd_enabled(IFACE_2G) && hostapd_enabled(IFACE_5G)
}

/// Snapshot of what the AP interfaces are currently configured as, for rollback.
pub fn snapshot(h: &dyn Host) -> (bool, bool) {
    let on = |key: &str| h.uci_get(key).unwrap_or_default() != "1";
    (on(AP_2G), on(AP_5G))
}

/// Observed state, for reporting. `beaconing` is the honest answer to "is
/// anything broadcasting right now".
pub fn observe(h: &dyn Host) -> Value {
    let (cfg_2g, cfg_5g) = snapshot(h);
    json!({
        "configured": { "ap_2g": cfg_2g, "ap_5g": cfg_5g },
        "hostapd_processes": hostapd_count(),
        "beaconing": { IFACE_2G: hostapd_enabled(IFACE_2G), IFACE_5G: hostapd_enabled(IFACE_5G) },
    })
}

/// Set both AP interfaces and block until the change is observable.
///
/// Unconditional: it writes and reloads even when uci already holds the target
/// value, because "uci already says off" is precisely the state a retry exists
/// to repair. Takes the Wi-Fi lock itself (engine wait); callers that already
/// hold it use `apply_locked`.
pub fn apply(h: &dyn Host, ap_2g: bool, ap_5g: bool) -> Result<Value, String> {
    let lk = lock(ENGINE_WAIT)?;
    apply_locked(h, &lk, ap_2g, ap_5g)
}

/// `apply` for a caller that already holds the lock.
pub fn apply_locked(h: &dyn Host, lk: &WifiLock, ap_2g: bool, ap_5g: bool) -> Result<Value, String> {
    // The stock UI's "Wi-Fi off" is its own master switch, not these flags:
    // under it, enabling the APs does nothing. Turning on means that one too.
    let vendor_on = vendor_on_if_off(h, lk, ap_2g || ap_5g);
    // Writes and reloads even when uci already says so (a retry repairs that
    // way). Synchronous, unlike wifi_set's backgrounded fire-and-forget. A
    // reload failure is not fatal on its own — the verify below is the authority.
    let reload: Result<(), String> = match h.write_aps(ap_2g, ap_5g)? {
        Some(e) => Err(e),
        None => Ok(()),
    };

    let want_any_on = ap_2g || ap_5g;
    let verified = if want_any_on {
        poll_until(ON_DEADLINE, || {
            hostapd_count() > 0 && (!ap_2g || hostapd_enabled(IFACE_2G)) && (!ap_5g || hostapd_enabled(IFACE_5G))
        })
    } else {
        poll_until(OFF_DEADLINE, || hostapd_count() == 0)
    };

    let mut data = observe(h);
    if let Some(obj) = data.as_object_mut() {
        obj.insert("verified".into(), json!(verified));
        obj.insert("target".into(), json!({"ap_2g": ap_2g, "ap_5g": ap_5g}));
        if let Err(e) = &reload {
            obj.insert("reload_error".into(), json!(e));
        }
        match &vendor_on {
            Some(Ok(())) => {
                obj.insert("vendor_switch".into(), json!("turned_on"));
            }
            Some(Err(e)) => {
                obj.insert("vendor_switch".into(), json!("failed"));
                obj.insert("vendor_switch_error".into(), json!(e));
            }
            None => {}
        }
    }

    if verified {
        Ok(data)
    } else {
        Err(format!(
            "AP state not observable within deadline; target ap_2g={ap_2g} ap_5g={ap_5g}, observed {data}"
        ))
    }
}

// ── The vendor master switch ────────────────────────────────────────────────
//
// The stock web UI and screen turn Wi-Fi off with `wireless.zte_mbb.wifi_onoff
// = 0` (vendor `zwrt_wlan set {zte_mbb:{wifi_onoff}}`), not with the AP flags.
// While it reads "0", writing `main_*.disabled=0` and reloading leaves the APs
// down. `wifi.set_module` makes the stock call (with the current `lbd`), the
// same thing the old guard script's `vendor_wifi_on` did.

/// The vendor switch's uci key. A missing key means on.
pub const VENDOR_ONOFF: &str = "wireless.zte_mbb.wifi_onoff";

/// Whether turning Wi-Fi on must first turn the vendor switch on: only when
/// something is wanted on and the switch reads exactly "0". Missing, unreadable
/// (`None`) or any other value: leave it alone.
fn vendor_switch_needs_on(want_on: bool, onoff: Option<&str>) -> bool {
    want_on && onoff == Some("0")
}

/// Turn the vendor master switch on if it is off and `want_on` (turning Wi-Fi
/// off — the home scenario — never touches it). Under the Wi-Fi lock, before
/// the AP flags are written. `None` when nothing needed doing; otherwise how
/// the write went. Callers go on with the AP write either way, and their
/// verify is the authority.
pub fn vendor_on_if_off(h: &dyn Host, _lk: &WifiLock, want_on: bool) -> Option<Result<(), String>> {
    if !want_on {
        return None;
    }
    let onoff = h.uci_get(VENDOR_ONOFF);
    if !vendor_switch_needs_on(want_on, onoff.as_deref()) {
        return None;
    }
    eprintln!("[wifi] vendor Wi-Fi switch ({VENDOR_ONOFF}) is off; turning it on");
    let r = h.vendor_on();
    if let Err(e) = &r {
        eprintln!("[wifi] vendor Wi-Fi switch: {e}; writing the APs anyway");
    }
    Some(r)
}

#[derive(Debug, PartialEq)]
enum Expect {
    /// At least one main AP should be beaconing; wait for exactly these.
    On { ap_2g: bool, ap_5g: bool },
    /// Nothing at all configured on: wait for zero hostapd processes.
    Off,
    /// Only guest APs on. Their interface names are not known here, so there is
    /// nothing honest to wait for; the reload result is all we report.
    Unknown,
}

/// A band is on when neither its radio nor its main AP is disabled. Arguments
/// are the raw `disabled` values; a missing key reads as "" (= enabled), as in
/// OpenWrt.
fn expectation(radio_2g: &str, main_2g: &str, radio_5g: &str, main_5g: &str, guest_on: bool) -> Expect {
    let ap_2g = radio_2g != "1" && main_2g != "1";
    let ap_5g = radio_5g != "1" && main_5g != "1";
    if ap_2g || ap_5g {
        Expect::On { ap_2g, ap_5g }
    } else if guest_on {
        Expect::Unknown
    } else {
        Expect::Off
    }
}

fn current_expectation(h: &dyn Host) -> Expect {
    let get = |k: &str| h.uci_get(k).unwrap_or_default();
    // Guest sections may not exist at all; only an explicit "0" counts as on.
    let guest_on = get("wireless.guest_2g.disabled") == "0" || get("wireless.guest_5g.disabled") == "0";
    expectation(&get("wireless.wifi0.disabled"), &get(AP_2G), &get("wireless.wifi1.disabled"), &get(AP_5G), guest_on)
}

/// One look, no waiting: do the radios match what uci now says? The same test
/// [`reload_and_verify`] polls for; `None` when there is nothing honest to check
/// (only guest APs on). For a caller that lost track of a reload it started
/// (u60d restarted while a Wi-Fi change was being confirmed).
pub fn settled(h: &dyn Host) -> Option<bool> {
    match current_expectation(h) {
        Expect::On { ap_2g, ap_5g } => {
            let n = hostapd_count();
            Some(n > 0 && n != usize::MAX && (!ap_2g || hostapd_enabled(IFACE_2G)) && (!ap_5g || hostapd_enabled(IFACE_5G)))
        }
        Expect::Off => Some(hostapd_count() == 0),
        Expect::Unknown => None,
    }
}

/// Reload synchronously, without waiting for the result to show. Only for
/// callers that verify some other way.
pub fn reload(h: &dyn Host, _lk: &WifiLock) -> Result<Value, String> {
    h.reload()
}

/// Reload and wait until the radios match what uci now says. Never fails: the
/// returned object carries `verified` and any `reload_error`, like `apply`'s.
pub fn reload_and_verify(h: &dyn Host, lk: &WifiLock) -> Value {
    let reload = reload(h, lk);
    let verified = match current_expectation(h) {
        Expect::On { ap_2g, ap_5g } => Some(poll_until(ON_DEADLINE, || {
            hostapd_count() > 0 && (!ap_2g || hostapd_enabled(IFACE_2G)) && (!ap_5g || hostapd_enabled(IFACE_5G))
        })),
        Expect::Off => Some(poll_until(OFF_DEADLINE, || hostapd_count() == 0)),
        Expect::Unknown => None,
    };
    let mut data = observe(h);
    if let Some(obj) = data.as_object_mut() {
        obj.insert("verified".into(), json!(verified));
        if let Err(e) = &reload {
            obj.insert("reload_error".into(), json!(e));
        }
    }
    data
}

#[cfg(test)]
mod tests;
