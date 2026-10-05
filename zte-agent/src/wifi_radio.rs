// ─────────────────────────────────────────────────────────────────────────────
// wifi_radio — turn the Wi-Fi APs on and off, unconditionally, and prove it.
//
// This exists because `wifi::wifi_set` cannot be used to drive Wi-Fi from
// automation, for three separate reasons found on the device:
//
//  1. It diffs before writing. When the value it would write already matches
//     uci it returns `200 {"note":"no changes"}` and never calls reload. If uci
//     says `disabled=1` but the AP is somehow still up, retrying through that
//     endpoint is a permanent no-op — exactly the state a retry needs to fix.
//  2. It returns before the reload has finished — the reload and its verify run
//     on a background thread (see `finish_in_background`) so that one Wi-Fi
//     save cannot pin one of the agent's two HTTP workers for a minute.
//  3. It reports nothing about whether the radios actually changed.
//
// What we toggle: the **AP interfaces** (`wireless.main_2g/main_5g.disabled`),
// NOT the radios (`wireless.wifi0/wifi1.disabled`). Measured 2026-09-22:
//
//   * Disabling the radios destroys the whole wiphy, not just the vdevs. The
//     phy disappears from `iw phy` entirely, so nothing can scan without first
//     bringing the radios back — and bringing them back re-creates the AP vdevs
//     and restarts hostapd, i.e. re-broadcasts the SSID. A scan every minute
//     would mean the SSID reappearing every minute and clients flapping onto a
//     network that is about to vanish again.
//   * Disabling only the AP interfaces leaves the phy up with zero hostapd
//     processes: no beacon, clients roam away, and a temporary `managed` vdev
//     can be added for scanning without ever broadcasting. It also avoids the
//     ACS channel re-selection that a radio restart triggers.
//
// The cost is that the radios stay powered. That trade is deliberate and the
// power draw is still unmeasured — see SCENARIO_ENGINE_PLAN.md.
// ─────────────────────────────────────────────────────────────────────────────

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::process::Command;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::handlers::AppState;
use crate::ubus;

const HOSTAPD_CTRL: &str = "/data/vendor/wifi/hostapd";
const AP_2G: &str = "wireless.main_2g.disabled";
const AP_5G: &str = "wireless.main_5g.disabled";
const IFACE_2G: &str = "wlan0";
const IFACE_5G: &str = "wlan2";

/// Deadlines are asymmetric because the device is. Tearing the APs down settles
/// in well under 10s; bringing both APs *and* both radios back was still
/// reporting nothing at 12s and only complete around 30s. A fixed sleep on the
/// "on" path is how an earlier probe script wrongly concluded the device had
/// failed to recover, so both paths poll instead.
const OFF_DEADLINE: Duration = Duration::from_secs(20);
const ON_DEADLINE: Duration = Duration::from_secs(45);
const POLL_EVERY: Duration = Duration::from_millis(1500);

// ── The Wi-Fi lock ──────────────────────────────────────────────────────────
//
// Every writer of the `wireless` uci package must hold a `WifiLock` from the
// first `uci set` until the reload it triggers has been verified. In the agent
// that is: `apply` (scenario engine, and PUT /api/wifi/radio), the engine's
// rollback, `wifi::wifi_set`, `wifi::guest_set` and `homemode`'s scan wake.
// Outside it: u60-guard (touch-ui `scripts/u60-guard.sh`, the watchdog that
// forces Wi-Fi on when the agent stops heartbeating) and the legacy
// `scripts/homemode*.sh`. Without it a scenario's multi-step apply can be
// interleaved with an admin-UI save — or with the watchdog — and land
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
//    written into it: the device has no `fuser`, so this is how u60-guard finds
//    who to kill when the lock is held by a wedged agent.

/// Shared with u60-guard and the homemode scripts. Do not rename one side only.
pub const LOCK_FILE: &str = "/tmp/u60-wifi.lock";

/// Longest the agent legitimately holds the lock in one go. The worst case is
/// `homemode_scan`'s wake path: reload (ubus times out at 30 s) + settle + five
/// scan attempts + reload + OFF_DEADLINE. A normal `apply` is reload +
/// ON_DEADLINE (≤ 75 s).
///
/// u60-guard's lock wait MUST be longer than this, or it will treat an agent
/// that is merely mid-reload as wedged and kill it. (The device's busybox
/// `flock` has no `-w`: the wait is a `flock -n` poll loop with a deadline.)
#[allow(dead_code)] // documented contract for u60-guard; referenced by tests
pub const MAX_HOLD: Duration = Duration::from_secs(120);

/// HTTP handlers: fail fast with 503 rather than park one of the two workers
/// behind an engine apply.
pub const HTTP_WAIT: Duration = Duration::from_secs(5);

/// The engine: outwait any single legitimate holder, including u60-guard's own
/// recovery attempt.
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

fn lock_at(path: &str, wait: Duration) -> Result<WifiLock, String> {
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
    // Best effort: the pid is a diagnostic for u60-guard, not part of the lock.
    let _ = file.set_len(0);
    let _ = writeln!(file, "{}", std::process::id());
    Ok(file)
}

/// Number of running hostapd processes. hostapd is what emits beacons, so zero
/// processes is positive proof that nothing is broadcasting — and unlike
/// `hostapd_cli`, "zero" cannot be confused with "we asked the wrong socket".
fn hostapd_count() -> usize {
    let out = match Command::new("ps").arg("w").output() {
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
    ubus::output_within(Command::new("hostapd_cli").args(["-i", iface, "-p", HOSTAPD_CTRL, "status"]), ubus::CMD_TIMEOUT)
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
pub fn fully_on() -> bool {
    snapshot() == (true, true)
        && beaconing()
        && hostapd_enabled(IFACE_2G)
        && hostapd_enabled(IFACE_5G)
}

/// Snapshot of what the AP interfaces are currently configured as, for rollback.
pub fn snapshot() -> (bool, bool) {
    let on = |key: &str| ubus::uci_get(key).unwrap_or_default() != "1";
    (on(AP_2G), on(AP_5G))
}

/// Observed state, for reporting. `beaconing` is the honest answer to "is
/// anything broadcasting right now".
fn observe() -> Value {
    let (cfg_2g, cfg_5g) = snapshot();
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
pub fn apply(ap_2g: bool, ap_5g: bool) -> Result<Value, String> {
    let lk = lock(ENGINE_WAIT)?;
    apply_locked(&lk, ap_2g, ap_5g)
}

/// `apply` for a caller that already holds the lock.
pub fn apply_locked(lk: &WifiLock, ap_2g: bool, ap_5g: bool) -> Result<Value, String> {
    let flag = |on: bool| if on { "0" } else { "1" };
    // The stock UI's "Wi-Fi off" is its own master switch, not these flags:
    // under it, enabling the APs does nothing. Turning on means that one too.
    let vendor_on = vendor_on_if_off(lk, ap_2g || ap_5g);
    // Through datad: it writes and reloads even when uci already says so (a
    // retry repairs that way). Synchronous, unlike wifi_set's backgrounded
    // fire-and-forget. A reload failure is not fatal on its own — the verify
    // below is the authority. datad gone: the emergency script (D19, Wi-Fi
    // on/off is one of the writes a person would otherwise have to fix).
    let reply = crate::datad_write::send("wifi.apply", &json!({"set": {AP_2G: flag(ap_2g), AP_5G: flag(ap_5g)}, "reload": true}));
    let reload: Result<(), String> = match reply {
        crate::datad_write::Reply::Done { result, .. } => match result["reload_error"].as_str() {
            Some(e) => Err(e.to_string()),
            None => Ok(()),
        },
        crate::datad_write::Reply::Unreachable { maybe_done: false, .. } if !crate::datad_feed::executor_stalled() => {
            match crate::fallback_write::run(crate::datad_write::current(), "wifi.radio", &json!({"ap_2g": ap_2g as u8, "ap_5g": ap_5g as u8})) {
                r if r.ok() => Ok(()),
                r => return Err(r.into_result().err().unwrap_or_default()),
            }
        }
        r => return Err(r.into_result().err().unwrap_or_default()),
    };

    let want_any_on = ap_2g || ap_5g;
    let verified = if want_any_on {
        poll_until(ON_DEADLINE, || {
            hostapd_count() > 0
                && (!ap_2g || hostapd_enabled(IFACE_2G))
                && (!ap_5g || hostapd_enabled(IFACE_5G))
        })
    } else {
        poll_until(OFF_DEADLINE, || hostapd_count() == 0)
    };

    let mut data = observe();
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
// down. datad's `wifi.set_module` makes the stock call (with the current `lbd`),
// the same thing u60-guard's `vendor_wifi_on` does.

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
/// the datad write went. Callers go on with the AP write either way, and their
/// verify is the authority (as in u60-guard). datad gone: an error here — the
/// agent has no vendor write of its own, and the emergency script's
/// `wifi.radio` does not know this switch.
pub fn vendor_on_if_off(_lk: &WifiLock, want_on: bool) -> Option<Result<(), String>> {
    if !want_on {
        return None;
    }
    let onoff = ubus::uci_get(VENDOR_ONOFF).ok();
    if !vendor_switch_needs_on(want_on, onoff.as_deref()) {
        return None;
    }
    eprintln!("[wifi] vendor Wi-Fi switch ({VENDOR_ONOFF}) is off; turning it on through datad");
    let r = crate::datad_write::send("wifi.set_module", &json!({"enabled": 1})).into_result().map(|_| ());
    if let Err(e) = &r {
        eprintln!("[wifi] vendor Wi-Fi switch: {e}; writing the APs anyway");
    }
    Some(r)
}

/// GET /api/wifi/radio — what the APs are doing right now.
pub fn radio_get(_state: &AppState) -> (u16, Value) {
    (200, json!({"ok": true, "data": observe()}))
}

/// PUT /api/wifi/radio — body: `{"ap_2g": bool, "ap_5g": bool}`.
///
/// A thin wrapper over `apply_locked`, with the short HTTP lock wait. The
/// scenario applier calls `apply` directly instead of routing through here:
/// going through `server::route` while holding the lock would deadlock (see
/// action.rs rule 1).
pub fn radio_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let (cur_2g, cur_5g) = snapshot();
    let want = |key: &str, cur: bool| parsed.get(key).and_then(|v| v.as_bool()).unwrap_or(cur);
    let ap_2g = want("ap_2g", cur_2g);
    let ap_5g = want("ap_5g", cur_5g);

    let lk = match lock(HTTP_WAIT) {
        Ok(l) => l,
        Err(e) => return (503, json!({"ok": false, "error": e})),
    };
    match apply_locked(&lk, ap_2g, ap_5g) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

// ── Reload + verify for the general-purpose writers ─────────────────────────
//
// `apply` knows its target. `wifi_set`, `guest_set` and homemode write
// arbitrary keys, so what "done" looks like is derived from uci afterwards.

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

fn current_expectation() -> Expect {
    let get = |k: &str| ubus::uci_get(k).unwrap_or_default();
    // Guest sections may not exist at all; only an explicit "0" counts as on.
    let guest_on = get("wireless.guest_2g.disabled") == "0" || get("wireless.guest_5g.disabled") == "0";
    expectation(
        &get("wireless.wifi0.disabled"),
        &get(AP_2G),
        &get("wireless.wifi1.disabled"),
        &get(AP_5G),
        guest_on,
    )
}

/// Reload synchronously, without waiting for the result to show. Only for
/// callers that verify some other way (homemode's wake is followed by scans).
pub fn reload(_lk: &WifiLock) -> Result<Value, String> {
    crate::datad_write::send("wifi.reload", &json!({})).into_result()
}

/// Reload and wait until the radios match what uci now says. Never fails: the
/// returned object carries `verified` and any `reload_error`, like `apply`'s.
pub fn reload_and_verify(lk: &WifiLock) -> Value {
    let reload = reload(lk);
    let verified = match current_expectation() {
        Expect::On { ap_2g, ap_5g } => Some(poll_until(ON_DEADLINE, || {
            hostapd_count() > 0
                && (!ap_2g || hostapd_enabled(IFACE_2G))
                && (!ap_5g || hostapd_enabled(IFACE_5G))
        })),
        Expect::Off => Some(poll_until(OFF_DEADLINE, || hostapd_count() == 0)),
        Expect::Unknown => None,
    };
    let mut data = observe();
    if let Some(obj) = data.as_object_mut() {
        obj.insert("verified".into(), json!(verified));
        if let Err(e) = &reload {
            obj.insert("reload_error".into(), json!(e));
        }
    }
    data
}

/// Hand the lock to a worker thread that reloads, verifies, logs, and only then
/// releases it — so the lock covers the reload, but the HTTP worker that did the
/// `uci set`s returns immediately (the agent only has two of them).
pub fn finish_in_background(lk: WifiLock, what: &'static str) {
    let source = crate::datad_write::current();
    let spawned = std::thread::Builder::new()
        .name(format!("wifi-{what}"))
        .spawn(move || {
            let data = crate::datad_write::with_source(source, || reload_and_verify(&lk));
            eprintln!("[wifi] {what}: reload finished {data}");
            drop(lk);
        });
    if let Err(e) = spawned {
        // The closure (and the lock in it) was dropped with the error; the
        // config is committed but not live. Say so rather than pretend.
        eprintln!("[wifi] {what}: could not start reload thread: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send<T: Send>() {}

    #[test]
    fn guard_is_send() {
        // wifi_set moves its lock into the reload thread.
        assert_send::<WifiLock>();
    }

    #[test]
    fn vendor_switch_only_when_exactly_off_and_turning_on() {
        assert!(vendor_switch_needs_on(true, Some("0")));
        // On, missing (= on), unreadable, or anything else: leave it.
        assert!(!vendor_switch_needs_on(true, Some("1")));
        assert!(!vendor_switch_needs_on(true, Some("")));
        assert!(!vendor_switch_needs_on(true, None));
        assert!(!vendor_switch_needs_on(true, Some(" 0")));
        // Turning off (home) never touches it.
        assert!(!vendor_switch_needs_on(false, Some("0")));
        assert!(!vendor_switch_needs_on(false, Some("1")));
    }

    #[test]
    fn expectation_follows_radio_and_ap_flags() {
        assert_eq!(expectation("0", "0", "0", "0", false), Expect::On { ap_2g: true, ap_5g: true });
        // Missing keys mean enabled.
        assert_eq!(expectation("", "", "", "", false), Expect::On { ap_2g: true, ap_5g: true });
        // Either layer disables a band.
        assert_eq!(expectation("1", "0", "0", "0", false), Expect::On { ap_2g: false, ap_5g: true });
        assert_eq!(expectation("0", "0", "0", "1", false), Expect::On { ap_2g: true, ap_5g: false });
        assert_eq!(expectation("0", "1", "0", "1", false), Expect::Off);
        assert_eq!(expectation("1", "1", "1", "1", false), Expect::Off);
        assert_eq!(expectation("0", "1", "0", "1", true), Expect::Unknown);
    }

    fn try_flock(path: &str) -> bool {
        let f = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path).unwrap();
        // SAFETY: valid fd; the lock goes away when `f` is dropped.
        unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
    }

    // One test, not several: the in-process layer is a process-wide static, so
    // parallel tests taking it would time each other out.
    #[test]
    fn lock_takes_both_layers_and_releases_both() {
        let dir = std::env::temp_dir().join(format!("u60-wifi-lock-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("u60-wifi.lock");
        let path = path.to_str().unwrap();

        let lk = lock_at(path, Duration::from_secs(1)).expect("first lock");

        // Cross-process layer: a separate open file description cannot flock it
        // (flock conflicts between descriptions even within one process — the
        // same thing `flock` in a shell would see).
        assert!(!try_flock(path), "flock should be held");
        // The holder's pid is in the file for u60-guard.
        let pid: u32 = std::fs::read_to_string(path).unwrap().trim().parse().unwrap();
        assert_eq!(pid, std::process::id());

        // In-process layer: a second acquisition times out instead of hanging.
        let t0 = Instant::now();
        let second = std::thread::spawn({
            let p = path.to_string();
            move || lock_at(&p, Duration::from_millis(300)).is_err()
        })
        .join()
        .unwrap();
        assert!(second, "second in-process lock must time out");
        assert!(t0.elapsed() >= Duration::from_millis(300));

        // A waiter gets it once the holder lets go — including a holder that
        // released it from another thread, the way finish_in_background does.
        let waiter = std::thread::spawn({
            let p = path.to_string();
            move || lock_at(&p, Duration::from_secs(5)).map(drop).is_ok()
        });
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            drop(lk);
        })
        .join()
        .unwrap();
        assert!(waiter.join().unwrap(), "waiter should acquire after release");

        // Both layers are free again.
        assert!(try_flock(path), "flock should be free");
        drop(lock_at(path, Duration::from_millis(100)).expect("in-process layer free"));

        // A process-external holder (what u60-guard looks like to us) blocks the
        // flock layer until the deadline, and the in-process flag is given back.
        let outside = OpenOptions::new().read(true).write(true).open(path).unwrap();
        // SAFETY: valid fd.
        assert_eq!(unsafe { libc::flock(outside.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0);
        assert!(lock_at(path, Duration::from_millis(300)).is_err());
        drop(outside);
        drop(lock_at(path, Duration::from_millis(300)).expect("free after outside holder left"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn max_hold_covers_a_normal_apply() {
        // reload (ubus 30 s timeout) + ON_DEADLINE must fit, with room.
        assert!(Duration::from_secs(30) + ON_DEADLINE < MAX_HOLD);
        assert!(MAX_HOLD < ENGINE_WAIT);
    }
}
