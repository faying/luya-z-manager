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
//  2. Its reload is `sh -c "ubus call zwrt_wlan reload … &"` — backgrounded,
//     return value discarded — and then it returns 200 regardless.
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

use std::process::Command;
use std::sync::Mutex;
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

/// Every writer of the `wireless` uci package must hold this: this module, the
/// scenario applier (which calls `apply` directly rather than through
/// `server::route`, to avoid re-entering the lock), `wifi::wifi_set`, and
/// `homemode`'s scan wake. Without it a scenario's multi-step apply can be
/// interleaved with an admin-UI write and land half-applied.
pub static WIFI_APPLY_LOCK: Mutex<()> = Mutex::new(());

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
    Command::new("hostapd_cli")
        .args(["-i", iface, "-p", HOSTAPD_CTRL, "status"])
        .output()
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
/// to repair. Callers must NOT hold `WIFI_APPLY_LOCK` — this takes it.
pub fn apply(ap_2g: bool, ap_5g: bool) -> Result<Value, String> {
    let _guard = WIFI_APPLY_LOCK.lock().map_err(|_| "wifi lock poisoned")?;

    let flag = |on: bool| if on { "0" } else { "1" };
    ubus::uci_set_no_commit(AP_2G, flag(ap_2g))?;
    ubus::uci_set_no_commit(AP_5G, flag(ap_5g))?;
    ubus::uci_commit("wireless")?;

    // Synchronous, unlike wifi_set's backgrounded fire-and-forget. A failure
    // here is still not fatal on its own — the verify below is the authority.
    let reload = ubus::call("zwrt_wlan", "reload", None);

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
    }

    if verified {
        Ok(data)
    } else {
        Err(format!(
            "AP state not observable within deadline; target ap_2g={ap_2g} ap_5g={ap_5g}, observed {data}"
        ))
    }
}

/// GET /api/wifi/radio — what the APs are doing right now.
pub fn radio_get(_state: &AppState) -> (u16, Value) {
    (200, json!({"ok": true, "data": observe()}))
}

/// PUT /api/wifi/radio — body: `{"ap_2g": bool, "ap_5g": bool}`.
///
/// A thin wrapper over `apply`. The scenario applier calls `apply` directly
/// instead of routing through here: it already holds no lock, and going through
/// `server::route` while holding one would deadlock (see action.rs rule 1).
pub fn radio_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let (cur_2g, cur_5g) = snapshot();
    let want = |key: &str, cur: bool| parsed.get(key).and_then(|v| v.as_bool()).unwrap_or(cur);
    let ap_2g = want("ap_2g", cur_2g);
    let ap_5g = want("ap_5g", cur_5g);

    match apply(ap_2g, ap_5g) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}
