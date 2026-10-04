use std::process::Command;

use serde_json::{json, Value};

use crate::handlers::AppState;
use crate::ubus;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn uci_get_wireless(key: &str) -> String {
    ubus::uci_get(&format!("wireless.{key}")).unwrap_or_default()
}

/// The firmware's own switches live in `wireless.zte_mbb` (B31: `wifi_onoff`,
/// `lbd`, …; `wifi6_switch` only once the vendor page has written it). There is
/// no `zte_mbb` uci package: the `zte_mbb.wifi.*` this file used to read and
/// write never existed on this device.
fn uci_get_mbb(key: &str) -> String {
    uci_get_wireless(&format!("zte_mbb.{key}"))
}

/// Pure part of [`master_on`]: the vendor switch is not off, and not both main
/// APs are disabled (how the master switch turns Wi-Fi off, see `wifi_set`).
fn master_on_from(vendor_onoff: &str, ap_2g_disabled: &str, ap_5g_disabled: &str) -> bool {
    vendor_onoff != "0" && !(ap_2g_disabled == "1" && ap_5g_disabled == "1")
}

/// The master switch as the page shows it.
fn master_on() -> bool {
    master_on_from(&uci_get_mbb("wifi_onoff"), &uci_get_wireless("main_2g.disabled"), &uci_get_wireless("main_5g.disabled"))
}

/// The vendor page treats a missing `wifi6_switch` as on, and only `"0"` as off.
fn wifi6_from(raw: &str) -> &'static str {
    if raw == "0" { "0" } else { "1" }
}

/// What the master switch writes when the page asks for `want` ("0"/"1") and
/// the switch currently reads `current_on`. Off: both main APs disabled. On:
/// both APs and both radios enabled, as u60-guard's restore does (APs under a
/// disabled radio never come up). Nothing when it already reads as wanted, so
/// the full body the page always sends doesn't touch the radios.
fn master_writes(want: &str, current_on: bool) -> Vec<(&'static str, &'static str)> {
    let want_on = want != "0";
    if want_on == current_on {
        return Vec::new();
    }
    if want_on {
        vec![
            ("wireless.wifi0.disabled", "0"),
            ("wireless.wifi1.disabled", "0"),
            ("wireless.main_2g.disabled", "0"),
            ("wireless.main_5g.disabled", "0"),
        ]
    } else {
        vec![("wireless.main_2g.disabled", "1"), ("wireless.main_5g.disabled", "1")]
    }
}

fn iw_info(iface: &str) -> (String, String) {
    let output = Command::new("iw")
        .args([iface, "info"])
        .output()
        .ok();
    let out = output
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    let channel = out
        .lines()
        .find_map(|l| {
            let l = l.trim();
            if l.starts_with("channel ") {
                l.split_whitespace().nth(1).map(|s| s.to_string())
            } else {
                None
            }
        })
        .unwrap_or_default();
    let bw = out
        .lines()
        .find_map(|l| {
            let l = l.trim();
            if let Some(pos) = l.find("width:") {
                let rest = l[pos + 6..].trim();
                let end = rest.find("MHz").map(|i| i + 3).unwrap_or(rest.len());
                Some(rest[..end].trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_default();
    (channel, bw)
}

fn station_count(iface: &str) -> u64 {
    let output = Command::new("sh")
        .args(["-c", &format!("iw {iface} station dump 2>/dev/null | grep -c Station")])
        .output()
        .ok();
    output
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse::<u64>()
                .ok()
        })
        .unwrap_or(0)
}

fn sanitize_uci_value(v: &str) -> String {
    v.chars()
        .filter(|c| !matches!(c, '\'' | '"' | ';' | '$' | '`' | '\\' | '|' | '<' | '>' | '&'))
        .collect()
}

// ---------------------------------------------------------------------------
// GET /api/wifi/status
// ---------------------------------------------------------------------------

pub fn wifi_status(_state: &AppState) -> (u16, Value) {
    let mut result = serde_json::Map::new();

    // Global switches: master = vendor switch and the two main APs; Wi-Fi 6
    // as the vendor page reads it
    result.insert("wifi_onoff".into(), json!(if master_on() { "1" } else { "0" }));
    result.insert("wifi6_switch".into(), json!(wifi6_from(&uci_get_mbb("wifi6_switch"))));

    // Radio config
    result.insert("radio2_disabled".into(), json!(uci_get_wireless("wifi0.disabled")));
    result.insert("radio5_disabled".into(), json!(uci_get_wireless("wifi1.disabled")));
    result.insert("channel_2g".into(), json!(uci_get_wireless("wifi0.channel")));
    result.insert("channel_5g".into(), json!(uci_get_wireless("wifi1.channel")));
    result.insert("txpower_2g".into(), json!(uci_get_wireless("wifi0.txpowerpercent")));
    result.insert("txpower_5g".into(), json!(uci_get_wireless("wifi1.txpowerpercent")));
    result.insert("htmode_2g".into(), json!(uci_get_wireless("wifi0.htmode")));
    result.insert("htmode_5g".into(), json!(uci_get_wireless("wifi1.htmode")));
    result.insert("country_code".into(), json!(uci_get_wireless("wifi0.country")));

    // Interface config
    result.insert("ssid_2g".into(), json!(uci_get_wireless("main_2g.ssid")));
    result.insert("ssid_5g".into(), json!(uci_get_wireless("main_5g.ssid")));
    result.insert("key_2g".into(), json!(uci_get_wireless("main_2g.key")));
    result.insert("key_5g".into(), json!(uci_get_wireless("main_5g.key")));
    result.insert("encryption_2g".into(), json!(uci_get_wireless("main_2g.encryption")));
    result.insert("encryption_5g".into(), json!(uci_get_wireless("main_5g.encryption")));
    result.insert("hidden_2g".into(), json!(uci_get_wireless("main_2g.hidden")));
    result.insert("hidden_5g".into(), json!(uci_get_wireless("main_5g.hidden")));

    // Runtime info from iw
    let (ch2, bw2) = iw_info("wlan0");
    let (ch5, bw5) = iw_info("wlan2");
    result.insert("actual_channel_2g".into(), json!(ch2));
    result.insert("actual_bw_2g".into(), json!(bw2));
    result.insert("actual_channel_5g".into(), json!(ch5));
    result.insert("actual_bw_5g".into(), json!(bw5));

    // Client counts
    let c2g = station_count("wlan0");
    let c5g = station_count("wlan2");
    result.insert("clients_2g".into(), json!(c2g));
    result.insert("clients_5g".into(), json!(c5g));
    result.insert("clients_total".into(), json!(c2g + c5g));

    // Guest WiFi summary
    result.insert("guest_disabled_2g".into(), json!(uci_get_wireless("guest_2g.disabled")));
    result.insert("guest_disabled_5g".into(), json!(uci_get_wireless("guest_5g.disabled")));
    result.insert("guest_ssid".into(), json!(uci_get_wireless("guest_2g.ssid")));

    (200, json!({"ok": true, "data": result}))
}

// ---------------------------------------------------------------------------
// PUT /api/wifi/settings
// ---------------------------------------------------------------------------

pub fn wifi_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let obj = match parsed.as_object() {
        Some(o) => o,
        None => return (400, json!({"ok": false, "error": "expected JSON object"})),
    };
    // Wi-Fi 6: the vendor page changes it together with each radio's hwmode
    // (Wi-Fi 5 mode forces 5 GHz to 11ac), in one `zwrt_wlan set`. Writing the
    // flag alone is not that, so say so instead of answering "no changes".
    if let Some(v) = obj.get("wifi6_switch").and_then(|v| v.as_str()) {
        if wifi6_from(v) != wifi6_from(&uci_get_mbb("wifi6_switch")) {
            return (501, json!({"ok": false, "error": "this firmware changes Wi-Fi 6 together with the radio mode; switching it here is not supported yet"}));
        }
    }
    // Serialise against every other writer of the `wireless` package (the
    // scenario engine, u60-guard, homemode). Held until the reload has been
    // verified, not just until commit — see wifi_radio's lock notes.
    let lk = match crate::wifi_radio::lock(crate::wifi_radio::HTTP_WAIT) {
        Ok(l) => l,
        Err(e) => return (503, json!({"ok": false, "error": e})),
    };

    let uci_map: &[(&str, &str)] = &[
        ("ssid_2g", "wireless.main_2g.ssid"),
        ("ssid_5g", "wireless.main_5g.ssid"),
        ("key_2g", "wireless.main_2g.key"),
        ("key_5g", "wireless.main_5g.key"),
        ("encryption_2g", "wireless.main_2g.encryption"),
        ("encryption_5g", "wireless.main_5g.encryption"),
        ("hidden_2g", "wireless.main_2g.hidden"),
        ("hidden_5g", "wireless.main_5g.hidden"),
        ("channel_2g", "wireless.wifi0.channel"),
        ("channel_5g", "wireless.wifi1.channel"),
        ("txpower_2g", "wireless.wifi0.txpowerpercent"),
        ("txpower_5g", "wireless.wifi1.txpowerpercent"),
        ("htmode_2g", "wireless.wifi0.htmode"),
        ("htmode_5g", "wireless.wifi1.htmode"),
        ("radio2_disabled", "wireless.wifi0.disabled"),
        ("radio5_disabled", "wireless.wifi1.disabled"),
    ];
    let txpower_keys: &[&str] = &["txpower_2g", "txpower_5g"];

    let mut wireless_changed = false;
    // what changed, written through datad in one go (E4 T7b)
    let mut wireless_set = serde_json::Map::new();
    let mut only_txpower = true;
    let mut txpower_2g_val: Option<u32> = None;
    let mut txpower_5g_val: Option<u32> = None;

    // Region/country code applies to BOTH radios (wifi0 + wifi1). It is the
    // self-managed phy's regulatory domain, so it forces a full wlan reload
    // (re-insmod) — never a txpower hot-apply.
    if let Some(country) = obj.get("country").and_then(|v| v.as_str()) {
        let cc = sanitize_uci_value(country).to_uppercase();
        if cc.len() == 2 && cc.chars().all(|c| c.is_ascii_alphabetic()) {
            for path in ["wireless.wifi0.country", "wireless.wifi1.country"] {
                let current = ubus::uci_get(path).unwrap_or_default();
                if current != cc {
                    wireless_set.insert(path.into(), json!(cc));
                    wireless_changed = true;
                    only_txpower = false;
                }
            }
        }
    }

    for (key, value) in obj {
        let val_str = match value {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => if *b { "1" } else { "0" }.to_string(),
            _ => continue,
        };
        let val_str = sanitize_uci_value(&val_str);

        // Check wireless UCI map
        if let Some(&(_, path)) = uci_map.iter().find(|&&(k, _)| k == key) {
            let current = ubus::uci_get(path).unwrap_or_default();
            if current != val_str {
                wireless_set.insert(path.into(), json!(val_str));
                wireless_changed = true;
                if !txpower_keys.contains(&key.as_str()) {
                    only_txpower = false;
                } else if key == "txpower_2g" {
                    txpower_2g_val = val_str.parse().ok();
                } else if key == "txpower_5g" {
                    txpower_5g_val = val_str.parse().ok();
                }
            }
            continue;
        }
    }

    // Master switch: the two main APs, the same writes as /api/wifi/radio, the
    // scenario engine and u60-guard's restore (which only knows `disabled`).
    // Applied after the per-key writes so it wins over the radio keys the body
    // always carries.
    if let Some(want) = obj.get("wifi_onoff").and_then(|v| v.as_str()) {
        for (path, val) in master_writes(want, master_on()) {
            if ubus::uci_get(path).unwrap_or_default() != val {
                wireless_set.insert(path.into(), json!(val));
                wireless_changed = true;
                only_txpower = false;
            }
        }
    }

    // Write and commit through datad; the reload comes after (hot txpower, or
    // the background reload + verify below)
    if wireless_changed {
        let r = crate::datad_write::send("wifi.apply", &json!({"set": wireless_set, "reload": false}));
        if !r.ok() {
            return r.into_http();
        }
    }

    if !wireless_changed {
        return (200, json!({"ok": true, "data": {"status": "ok", "note": "no changes"}}));
    }

    // Hot-apply txpower if that's the only change
    if wireless_changed && only_txpower {
        if let Some(val) = txpower_2g_val {
            let _ = Command::new("iw")
                .args(["dev", "wlan0", "set", "txpower", "limit", &(val * 30).to_string()])
                .output();
        }
        if let Some(val) = txpower_5g_val {
            let _ = Command::new("iw")
                .args(["dev", "wlan2", "set", "txpower", "limit", &(val * 30).to_string()])
                .output();
        }
        return (200, json!({"ok": true, "data": {"status": "ok", "hot": true}}));
    }

    // Full reload needed. It runs on its own thread, which keeps the lock until
    // the reload is verified; the response does not wait for it.
    if wireless_changed {
        crate::wifi_radio::finish_in_background(lk, "settings");
    }

    (200, json!({"ok": true, "data": {"status": "ok"}}))
}

// ---------------------------------------------------------------------------
// GET /api/wifi/guest
// ---------------------------------------------------------------------------

pub fn guest_status(_state: &AppState) -> (u16, Value) {
    let mut result = serde_json::Map::new();

    result.insert("ssid".into(), json!(uci_get_wireless("guest_2g.ssid")));
    result.insert("key".into(), json!(uci_get_wireless("guest_2g.key")));
    result.insert("encryption".into(), json!(uci_get_wireless("guest_2g.encryption")));
    result.insert("disabled_2g".into(), json!(uci_get_wireless("guest_2g.disabled")));
    result.insert("disabled_5g".into(), json!(uci_get_wireless("guest_5g.disabled")));
    result.insert("hidden".into(), json!(uci_get_wireless("guest_2g.hidden")));
    result.insert("isolate".into(), json!(uci_get_wireless("guest_2g.isolate")));
    result.insert("guest_active_time".into(), json!(uci_get_wireless("guest_2g.guest_active_time")));

    // Runtime remaining time
    let remaining = ubus::read("zwrt_wlan", "wlan_get_guest_access_left_time", Some("{}"))
        .ok()
        .and_then(|v| v["guest_left_time"].as_str().and_then(|s| s.parse::<i64>().ok()))
        .unwrap_or(-1);
    result.insert("remaining_seconds".into(), json!(remaining));

    (200, json!({"ok": true, "data": result}))
}

// ---------------------------------------------------------------------------
// PUT /api/wifi/guest
// ---------------------------------------------------------------------------

pub fn guest_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let obj = match parsed.as_object() {
        Some(o) => o,
        None => return (400, json!({"ok": false, "error": "expected JSON object"})),
    };

    let guest_map: &[(&str, &[&str])] = &[
        ("guest_ssid", &["wireless.guest_2g.ssid", "wireless.guest_5g.ssid"]),
        ("guest_key", &["wireless.guest_2g.key", "wireless.guest_5g.key"]),
        ("guest_encryption", &["wireless.guest_2g.encryption", "wireless.guest_5g.encryption"]),
        ("guest_disabled", &["wireless.guest_2g.disabled", "wireless.guest_5g.disabled"]),
        ("guest_disabled_2g", &["wireless.guest_2g.disabled"]),
        ("guest_disabled_5g", &["wireless.guest_5g.disabled"]),
        ("guest_hidden", &["wireless.guest_2g.hidden", "wireless.guest_5g.hidden"]),
        ("guest_isolate", &["wireless.guest_2g.isolate", "wireless.guest_5g.isolate"]),
        ("guest_active_time", &["wireless.guest_2g.guest_active_time", "wireless.guest_5g.guest_active_time"]),
    ];

    let lk = match crate::wifi_radio::lock(crate::wifi_radio::HTTP_WAIT) {
        Ok(l) => l,
        Err(e) => return (503, json!({"ok": false, "error": e})),
    };

    let mut set = serde_json::Map::new();
    for (key, value) in obj {
        let val_str = match value {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => if *b { "1" } else { "0" }.to_string(),
            _ => continue,
        };
        let val_str = sanitize_uci_value(&val_str);

        if let Some(&(_, paths)) = guest_map.iter().find(|&&(k, _)| k == key) {
            for &path in paths {
                set.insert(path.into(), json!(val_str));
            }
        }
    }

    if set.is_empty() {
        return (200, json!({"ok": true, "data": {"status": "ok", "note": "no changes"}}));
    }
    // Guest interfaces may not exist: best effort, skipped ones listed by datad
    let changed = match crate::datad_write::send("wifi.apply", &json!({"set": set, "reload": false, "best_effort": true})) {
        crate::datad_write::Reply::Done { result, .. } => result["committed"].as_array().is_some_and(|c| !c.is_empty()),
        r => return r.into_http(),
    };
    if !changed {
        return (200, json!({"ok": true, "data": {"status": "ok", "note": "no changes"}}));
    }

    crate::wifi_radio::finish_in_background(lk, "guest");

    (200, json!({"ok": true, "data": {"status": "ok"}}))
}

// ---------------------------------------------------------------------------
// Wi-Fi power save (E4, 10-04): the same switch as the touch screen. The write
// goes through datad (`wifi.power_save`: hotplug script + iw on wlan0-3 +
// readback, recorded in the change log); here we only read.
// ---------------------------------------------------------------------------

const PSM_HOTPLUG: &str = "/etc/hotplug.d/iface/99-disable-powersave";

/// `iw dev X get power_save` → on/off; anything else (Wi-Fi off) → None.
fn psm_from_iw(out: &str) -> Option<bool> {
    if out.contains("Power save: on") {
        Some(true)
    } else if out.contains("Power save: off") {
        Some(false)
    } else {
        None
    }
}

/// The saved choice in the hotplug script (missing: never set, driver default).
fn psm_from_script(text: &str) -> Option<bool> {
    let line = text.lines().find(|l| l.contains("set power_save"))?;
    psm_from_iw(&line.replace("set power_save", "Power save:"))
}

fn psm_live() -> Option<bool> {
    ["wlan0", "wlan2"].iter().find_map(|w| {
        let out = Command::new("iw").args(["dev", w, "get", "power_save"]).output().ok()?;
        psm_from_iw(&String::from_utf8_lossy(&out.stdout))
    })
}

pub fn power_save_get(_state: &AppState) -> (u16, Value) {
    let live = psm_live();
    let saved = std::fs::read_to_string(PSM_HOTPLUG).ok().and_then(|t| psm_from_script(&t));
    (
        200,
        json!({"ok": true, "data": {"enabled": live.or(saved), "live": live, "saved": saved}}),
    )
}

pub fn power_save_set(state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let Some(enabled) = parsed["enabled"].as_bool() else {
        return (400, json!({"ok": false, "error": "enabled must be true or false"}));
    };
    let r = crate::datad_write::send("wifi.power_save", &json!({"enabled": enabled}));
    if !r.ok() {
        return r.into_http();
    }
    power_save_get(state)
}

// ---------------------------------------------------------------------------
// NFC tap-to-join (audit C, 10-04): the touch screen's 「NFC 碰一碰」 on the
// Wi-Fi page. Read: the vendor read itself (the web reads back once right
// after a write, and the feed can still hold the value from before it), then
// datad's `nfc.switch`; write: datad nfc.set with the touch screen's flag.
// ---------------------------------------------------------------------------

fn nfc_switch(v: &Value) -> Option<bool> {
    match v.get("switch")? {
        Value::Number(n) => n.as_i64().map(|n| n == 1),
        Value::String(s) => s.trim().parse::<i64>().ok().map(|n| n == 1),
        _ => None,
    }
}

pub fn nfc_get(_state: &AppState) -> (u16, Value) {
    let on = ubus::read("zwrt_nfc", "zwrt_nfc_wifi_get", Some("{}"))
        .ok()
        .as_ref()
        .and_then(nfc_switch)
        .or_else(|| crate::datad_feed::global().and_then(|f| f.view().block("nfc")).as_ref().and_then(nfc_switch));
    (200, json!({"ok": true, "data": {"supported": on.is_some(), "enabled": on}}))
}

pub fn nfc_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let Some(enabled) = parsed["enabled"].as_bool() else {
        return (400, json!({"ok": false, "error": "enabled must be true or false"}));
    };
    let r = crate::datad_write::send("nfc.set", &json!({"enabled": enabled as i64, "flag": 2}));
    if !r.ok() {
        return r.into_http();
    }
    (200, json!({"ok": true, "data": {"supported": true, "enabled": enabled}}))
}

#[cfg(test)]
mod power_save_tests {
    use super::*;

    #[test]
    fn reads_iw_and_the_saved_script() {
        assert_eq!(psm_from_iw("wlan0\tPower save: on\n"), Some(true));
        assert_eq!(psm_from_iw("Power save: off"), Some(false));
        assert_eq!(psm_from_iw("command failed: No such device (-19)"), None);
        let script = "#!/bin/sh\n[ \"$ACTION\" = ifup ] && {\n  iw dev wlan0 set power_save off 2>/dev/null\n}\n";
        assert_eq!(psm_from_script(script), Some(false));
        assert_eq!(psm_from_script(&script.replace("off", "on")), Some(true));
        assert_eq!(psm_from_script(""), None);
    }

    #[test]
    fn nfc_switch_reads_numbers_and_strings() {
        assert_eq!(nfc_switch(&json!({"switch": 1})), Some(true));
        assert_eq!(nfc_switch(&json!({"switch": "0"})), Some(false));
        assert_eq!(nfc_switch(&json!({})), None);
    }
}

#[cfg(test)]
mod switch_tests {
    use super::*;

    #[test]
    fn master_reads_vendor_switch_and_both_aps() {
        assert!(master_on_from("1", "0", "0"));
        assert!(master_on_from("", "", ""));
        assert!(master_on_from("1", "1", "0"), "one band off is not the master switch");
        assert!(!master_on_from("1", "1", "1"));
        assert!(!master_on_from("0", "0", "0"));
    }

    #[test]
    fn master_writes_only_on_change() {
        assert!(master_writes("1", true).is_empty());
        assert!(master_writes("0", false).is_empty());
        assert_eq!(master_writes("0", true), vec![("wireless.main_2g.disabled", "1"), ("wireless.main_5g.disabled", "1")]);
        let on = master_writes("1", false);
        assert_eq!(on.len(), 4);
        assert!(on.iter().all(|&(_, v)| v == "0"));
        assert!(on.contains(&("wireless.wifi0.disabled", "0")) && on.contains(&("wireless.wifi1.disabled", "0")));
    }

    #[test]
    fn wifi6_missing_reads_as_on() {
        assert_eq!(wifi6_from(""), "1");
        assert_eq!(wifi6_from("1"), "1");
        assert_eq!(wifi6_from("0"), "0");
    }
}
