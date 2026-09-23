// ─────────────────────────────────────────────────────────────────────────────
// public.rs — UNAUTHENTICATED, read-only status summary for the login screen.
//
// Mirrors the kind of pre-login info the stock ZTE web UI shows (network / Wi-Fi
// / battery) and adds our service health (Tailscale / CHILL / Home Mode).
// LAN-only, no secrets — never include keys, IMEI, client lists, etc.
//
//   GET /api/public/status   (allow-listed past auth in server.rs)
// ─────────────────────────────────────────────────────────────────────────────

use crate::handlers::AppState;
use crate::ubus;
use serde_json::{json, Value};
use std::fs;
use std::process::Command;

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("")
}

fn sh(cmd: &str) -> String {
    Command::new("sh")
        .args(["-c", cmd])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

pub fn public_status(state: &AppState) -> (u16, Value) {
    // ── Network (signal / type / operator / connection) ──
    let net = ubus::call("zte_nwinfo_api", "nwinfo_get_netinfo", Some("{}")).unwrap_or(json!({}));
    let nettype = s(&net, "network_type"); // "SA" | "NSA" | "LTE" | ""
    let operator = {
        let f = s(&net, "network_provider_fullname");
        if f.is_empty() { s(&net, "network_provider") } else { f }
    };
    let bar: i64 = s(&net, "signalbar").parse().unwrap_or(-1);
    let rsrp: i64 = if nettype == "LTE" {
        net.get("lte_rsrp").and_then(|v| v.as_i64()).unwrap_or(0)
    } else {
        net.get("nr5g_rsrp").and_then(|v| v.as_i64()).unwrap_or(0)
    };
    let wwan = ubus::call(
        "zwrt_data",
        "get_wwaniface",
        Some(r#"{"source_module":"zte_topsw_data","cid":1}"#),
    )
    .unwrap_or(json!({}));
    let connect_status = s(&wwan, "connect_status"); // e.g. ipv4_ipv6_connected
    let connected = connect_status.contains("connected");

    // ── Wi-Fi ──
    let wlan = ubus::call("zwrt_wlan", "report", Some("{}")).unwrap_or(json!({}));
    let wifi_on = s(&wlan, "wifi_onoff") == "1";
    let ssid = {
        let a = s(&wlan, "main2g_ssid");
        if a.is_empty() { s(&wlan, "main5g_ssid") } else { a }
    };

    // ── Battery ──
    let bpct: i64 = fs::read_to_string("/sys/class/power_supply/battery/capacity")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(-1);
    let bstat = fs::read_to_string("/sys/class/power_supply/battery/status")
        .unwrap_or_default()
        .trim()
        .to_string();
    let charging = bstat.eq_ignore_ascii_case("Charging");

    // ── Services ──
    let ts_running = !sh("pidof tailscaled").is_empty();
    let ts_node = if ts_running {
        // first self line of `tailscale status`: "<100.x.y.z> <hostname> ..."
        let line = sh("/data/tailscale/tailscale --socket=/tmp/tailscaled.sock status 2>/dev/null | head -1");
        let mut it = line.split_whitespace();
        match (it.next(), it.next()) {
            (Some(ip), Some(name)) => format!("{ip} {name}"),
            _ => String::new(),
        }
    } else {
        String::new()
    };
    let ts_installed = std::path::Path::new("/data/tailscale/tailscale").exists();
    // CHILL publishes its own state as JSON (state/reason/...) — see chill.rs and
    // chill.sh's write_state().
    let chill_state = fs::read_to_string("/tmp/chill.state")
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok());
    let chill_state_str = chill_state
        .as_ref()
        .and_then(|v| v.get("state"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let chill_reason = chill_state
        .as_ref()
        .and_then(|v| v.get("reason"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let hm_state = fs::read_to_string("/data/homemode/state").unwrap_or_default();
    let hm_mode = hm_state.split_whitespace().next().unwrap_or("normal").to_string();
    let hm_present = std::path::Path::new("/data/homemode.sh").exists();
    let hm_enabled = hm_present && !std::path::Path::new("/data/homemode/disabled").exists();

    // ── SMS (unread count, device + SIM storage) ──
    let cap = ubus::call("zwrt_wms", "zwrt_wms_get_wms_capacity", Some("{}")).unwrap_or(json!({}));
    let unread_i = |k: &str| -> i64 {
        cap.get(k)
            .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
            .unwrap_or(0)
    };
    let sms_unread = unread_i("sms_dev_unread_num") + unread_i("sms_sim_unread_num");

    (
        200,
        json!({
            "ok": true,
            "data": {
                "network": {
                    "connected": connected,
                    "type": nettype,
                    "operator": operator,
                    "bar": bar,
                    "rsrp": rsrp,
                },
                "wifi": { "on": wifi_on, "ssid": ssid },
                "battery": { "percent": bpct, "charging": charging },
                "sms": { "unread": sms_unread },
                "services": {
                    "tailscale": { "running": ts_running, "installed": ts_installed, "node": ts_node },
                    "chill": { "state": chill_state_str, "reason": chill_reason, "on": crate::chill::switched_on() },
                    "home_mode": { "present": hm_present, "enabled": hm_enabled, "mode": hm_mode },
                },
                "scenario": crate::scenario::public_summary(&state.scenario),
                // Count only — the events themselves need a login.
                "alerts": { "unread": crate::alerts::public_unread() },
                // Seconds the device clock runs ahead of UTC (clock.rs). The web
                // needs it to compare device times with the browser's clock.
                "clock": { "utc_offset": crate::clock::utc_offset() },
                // Counts only; the checks themselves need a login (/api/health).
                "health": crate::health::public_summary(),
            }
        }),
    )
}
