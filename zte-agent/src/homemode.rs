// ─────────────────────────────────────────────────────────────────────────────
// Home Mode — manage the SSID watch-list used by /data/homemode.sh.
//
// The on-device cron worker (scripts/homemode.sh) disables Wi-Fi when any of a
// configured set of "home" SSIDs is seen nearby. This module exposes that
// configuration to the admin UI:
//
//   GET    /api/homemode          → { enabled, ssids, state, wifi_off, default }
//   PUT    /api/homemode          → set { enabled?, ssids? }
//   GET    /api/homemode/scan     → nearby SSIDs (to pick from), strongest first
//
// Contract with the shell worker (must stay in sync):
//   SSID list      : $SSID_FILE,    one SSID per line (# comments / blanks ok)
//   suspend switch : $DISABLE_FLAG, file present ⇒ home mode suspended
//   runtime state  : $STATE_FILE,   "<mode> <tick> <miss>" (mode = normal|home)
// ─────────────────────────────────────────────────────────────────────────────

use crate::handlers::AppState;
use crate::ubus;
use serde_json::{json, Value};
use std::fs;
use std::process::Command;
use std::time::Duration;

const STATE_DIR: &str = "/data/homemode";
const SSID_FILE: &str = "/data/homemode/ssids";
const CONFIG_FILE: &str = "/data/homemode/config";
const DISABLE_FLAG: &str = "/data/homemode/disabled";
const STATE_FILE: &str = "/data/homemode/state";
const CHECK_EVERY_DEFAULT: u32 = 2;
const EXIT_MISSES_DEFAULT: u32 = 2;
const LOG_FILE: &str = "/data/log/homemode.log"; // switch events (Wi-Fi on/off)
const SCAN_LOG_FILE: &str = "/data/log/homemode-scan.log"; // periodic scan rechecks
const LOG_TAIL_LINES: usize = 200;
const DEFAULT_SSIDS: [&str; 3] = ["EXAMPLE-HOME", "EXAMPLE-HOME-5G", "EXAMPLE-HOME-IOT"];
const SCAN_IFACE: &str = "wlan0";
const RADIO_2G: &str = "wireless.wifi0.disabled";
const WAKE_SETTLE_SECS: u64 = 4; // initial wait after waking 2.4G before scanning

/// Whether the user has an explicit SSID file (absent ⇒ built-in defaults).
fn ssids_file_exists() -> bool {
    std::path::Path::new(SSID_FILE).exists()
}

/// Read the configured SSID list. '#' is a comment only at line start, so
/// SSIDs may legitimately contain it (mirrors the shell worker exactly).
fn read_ssids() -> Vec<String> {
    fs::read_to_string(SSID_FILE)
        .map(|s| {
            s.lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .collect()
        })
        .unwrap_or_default()
}

/// Persist the SSID list (one per line). De-dupes while preserving order.
fn write_ssids(ssids: &[String]) -> std::io::Result<()> {
    let _ = fs::create_dir_all(STATE_DIR);
    let mut seen: Vec<String> = Vec::new();
    for s in ssids {
        let t = s.trim();
        if !t.is_empty() && !seen.iter().any(|x| x == t) {
            seen.push(t.to_string());
        }
    }
    fs::write(SSID_FILE, format!("{}\n", seen.join("\n")))
}

/// Current worker mode: "home" (Wi-Fi off) or "normal".
fn read_mode() -> String {
    fs::read_to_string(STATE_FILE)
        .ok()
        .and_then(|s| s.split_whitespace().next().map(|m| m.to_string()))
        .unwrap_or_else(|| "normal".to_string())
}

/// Read an integer tunable from CONFIG_FILE (`key=value` lines).
fn read_cfg_int(key: &str, default: u32) -> u32 {
    fs::read_to_string(CONFIG_FILE)
        .ok()
        .and_then(|s| {
            s.lines().find_map(|l| {
                l.trim()
                    .strip_prefix(&format!("{key}="))
                    .and_then(|v| v.trim().parse::<u32>().ok())
            })
        })
        .unwrap_or(default)
}

/// Persist both tunables (the file holds the full set).
fn write_cfg(check_every: u32, exit_misses: u32) -> std::io::Result<()> {
    let _ = fs::create_dir_all(STATE_DIR);
    fs::write(
        CONFIG_FILE,
        format!("check_every={check_every}\nexit_misses={exit_misses}\n"),
    )
}

// GET /api/homemode
pub fn homemode_get(_state: &AppState) -> (u16, Value) {
    // Absent file ⇒ built-in defaults are in effect. A present file (even
    // empty) is the user's explicit list and is shown as-is.
    let using_default = !ssids_file_exists();
    let ssids: Vec<String> = if using_default {
        DEFAULT_SSIDS.iter().map(|s| s.to_string()).collect()
    } else {
        read_ssids()
    };
    let mode = read_mode();
    (
        200,
        json!({
            "ok": true,
            "data": {
                "enabled": !std::path::Path::new(DISABLE_FLAG).exists(),
                "ssids": ssids,
                "using_default": using_default,
                "default_ssids": DEFAULT_SSIDS,
                "mode": mode,                 // "home" | "normal"
                "wifi_off": mode == "home",
                "check_every": read_cfg_int("check_every", CHECK_EVERY_DEFAULT),
                "exit_misses": read_cfg_int("exit_misses", EXIT_MISSES_DEFAULT),
            }
        }),
    )
}

// PUT /api/homemode — body: { enabled?, ssids?, check_every?, exit_misses? }
pub fn homemode_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let obj = match parsed.as_object() {
        Some(o) => o,
        None => return (400, json!({"ok": false, "error": "expected JSON object"})),
    };

    // Update SSID list if provided.
    if let Some(arr) = obj.get("ssids") {
        let list = match arr.as_array() {
            Some(a) => a,
            None => return (400, json!({"ok": false, "error": "'ssids' must be an array"})),
        };
        let mut ssids = Vec::with_capacity(list.len());
        for v in list {
            let s = match v.as_str() {
                Some(s) => s.trim(),
                None => return (400, json!({"ok": false, "error": "'ssids' must be strings"})),
            };
            // Newlines would corrupt the one-per-line file format.
            if s.is_empty() || s.contains('\n') || s.contains('\r') {
                continue;
            }
            ssids.push(s.to_string());
        }
        if let Err(e) = write_ssids(&ssids) {
            return (500, json!({"ok": false, "error": format!("write ssids: {e}")}));
        }
    }

    // Update enable/suspend switch if provided.
    if let Some(enabled) = obj.get("enabled") {
        let enabled = enabled.as_bool().unwrap_or(true);
        let _ = fs::create_dir_all(STATE_DIR);
        if enabled {
            let _ = fs::remove_file(DISABLE_FLAG); // ok if missing
        } else {
            let _ = fs::write(DISABLE_FLAG, "");
        }
    }

    // Update detection tunables if provided (accept number or numeric string).
    let as_u32 = |v: &Value| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok()));
    let want_ce = obj.get("check_every").and_then(as_u32);
    let want_em = obj.get("exit_misses").and_then(as_u32);
    if want_ce.is_some() || want_em.is_some() {
        let ce = want_ce
            .map(|v| (v as u32).clamp(1, 60))
            .unwrap_or_else(|| read_cfg_int("check_every", CHECK_EVERY_DEFAULT));
        let em = want_em
            .map(|v| (v as u32).clamp(1, 30))
            .unwrap_or_else(|| read_cfg_int("exit_misses", EXIT_MISSES_DEFAULT));
        if let Err(e) = write_cfg(ce, em) {
            return (500, json!({"ok": false, "error": format!("write config: {e}")}));
        }
    }

    homemode_get(_state)
}

/// Run `iw dev wlanX scan`; Some(output) on success, None if the radio is down.
/// The `dev` keyword matters: the legacy `iw wlan0 scan` shorthand returns exit
/// 0 even when the interface is gone, which would defeat the wake logic.
fn run_iw_scan() -> Option<String> {
    let out = Command::new("iw")
        .args(["dev", SCAN_IFACE, "scan"])
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        None
    }
}

/// A freshly-woken radio often fails or returns nothing on the first scan, so
/// retry until results appear (or attempts run out).
fn scan_until_results(attempts: u32, delay: Duration) -> Option<String> {
    let mut last = None;
    for i in 0..attempts {
        if i > 0 {
            std::thread::sleep(delay);
        }
        if let Some(t) = run_iw_scan() {
            if t.contains("SSID:") {
                return Some(t);
            }
            last = Some(t);
        }
    }
    last
}

/// Apply wifi0's disabled flag at runtime (commit + daemon reload).
fn set_radio_2g(disabled: bool) {
    // Fourth writer of the `wireless` package — same lock as wifi_set and the
    // scenario applier, so a scan wake can't interleave with either.
    let _wifi_guard = crate::wifi_radio::WIFI_APPLY_LOCK.lock();
    let _ = ubus::uci_set_no_commit(RADIO_2G, if disabled { "1" } else { "0" });
    let _ = ubus::uci_commit("wireless");
    let _ = Command::new("ubus")
        .args(["call", "zwrt_wlan", "reload"])
        .output();
}

// GET /api/homemode/scan — nearby SSIDs to pick from, strongest signal first.
pub fn homemode_scan(_state: &AppState) -> (u16, Value) {
    // Normally radios are up and we scan directly. But when home mode has turned
    // Wi-Fi off (you're home), wlan0 is gone — so briefly wake ONLY 2.4G (no 5G
    // DFS delay), scan, then put it back, exactly like the cron worker does.
    let mut woke = false;
    let mut text = run_iw_scan();
    if text.is_none() && ubus::uci_get(RADIO_2G).unwrap_or_default() == "1" {
        set_radio_2g(false); // wake 2.4G
        woke = true;
        std::thread::sleep(Duration::from_secs(WAKE_SETTLE_SECS));
        text = scan_until_results(5, Duration::from_millis(1500));
    }

    let text = text.unwrap_or_default();

    // Parse `SSID:` and the preceding `signal:` line; keep the strongest per SSID.
    let mut cur_signal: Option<f64> = None;
    let mut best: Vec<(String, f64)> = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("signal:") {
            cur_signal = rest.trim().split_whitespace().next().and_then(|n| n.parse().ok());
        } else if let Some(rest) = l.strip_prefix("SSID:") {
            let ssid = rest.trim().to_string();
            let sig = cur_signal.take().unwrap_or(-100.0);
            if ssid.is_empty() {
                continue; // hidden SSID
            }
            match best.iter_mut().find(|(s, _)| *s == ssid) {
                Some(entry) => {
                    if sig > entry.1 {
                        entry.1 = sig;
                    }
                }
                None => best.push((ssid, sig)),
            }
        }
    }
    best.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let networks: Vec<Value> = best
        .into_iter()
        .map(|(ssid, signal)| json!({ "ssid": ssid, "signal": signal }))
        .collect();

    // If we woke 2.4G just to scan, put it back off (home mode 5G is already off).
    if woke {
        set_radio_2g(true);
    }

    (
        200,
        json!({"ok": true, "data": { "networks": networks, "woke_radio": woke }}),
    )
}

/// Last LOG_TAIL_LINES lines of a log file (empty string if absent).
fn tail_log(path: &str) -> String {
    let text = fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(LOG_TAIL_LINES);
    lines[start..].join("\n")
}

// GET /api/homemode/log — worker activity, split into switch events and scan rechecks.
pub fn homemode_log(_state: &AppState) -> (u16, Value) {
    (
        200,
        json!({
            "ok": true,
            "data": {
                "events": tail_log(LOG_FILE),   // Wi-Fi on/off transitions (kept long)
                "scans": tail_log(SCAN_LOG_FILE) // periodic rechecks (high volume)
            }
        }),
    )
}
