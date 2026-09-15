use std::fs;

use serde_json::{json, Value};

use crate::handlers::AppState;
use crate::ubus;

pub fn device_thermal(_state: &AppState) -> (u16, Value) {
    match ubus::call("zwrt_bsp.thermal", "get_cpu_temp", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn device_charger(_state: &AppState) -> (u16, Value) {
    match ubus::call("zwrt_bsp.charger", "list", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn device_system(_state: &AppState) -> (u16, Value) {
    match ubus::call("system", "info", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn device_reboot(_state: &AppState) -> (u16, Value) {
    match ubus::call("system", "reboot", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

// factory_reset is ZTE-specific (zwrt_bsp.power) — may require re-enabling that daemon
pub fn device_factory_reset(_state: &AppState) -> (u16, Value) {
    match ubus::call("zwrt_bsp.power", "factory_reset", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn charge_control_get(state: &AppState) -> (u16, Value) {
    let read_sysfs = |path: &str| -> String {
        fs::read_to_string(path)
            .unwrap_or_default()
            .trim()
            .to_string()
    };

    let battery_status = read_sysfs("/sys/class/power_supply/battery/status");
    let capacity: i64 = read_sysfs("/sys/class/power_supply/battery/capacity")
        .parse()
        .unwrap_or(0);

    let charging_stopped = ubus::call("zwrt_bsp.charger", "list", Some("{}"))
        .ok()
        .and_then(|v| v["direct_power_supply_mode"].as_str().map(|s| s == "enable"))
        .unwrap_or(false);

    let (limit_enabled, limit_pct, hysteresis, manual_override) = state.charge_limit.get();

    (
        200,
        json!({
            "ok": true,
            "data": {
                "charging_stopped": charging_stopped,
                "battery_status": battery_status,
                "capacity": capacity,
                "charge_limit_enabled": limit_enabled,
                "charge_limit": limit_pct,
                "hysteresis": hysteresis,
                "manual_override": manual_override,
            }
        }),
    )
}

pub fn charge_control_set(state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };

    // Manual charge stop/resume via ubus (inverted: "enable" = stop, "disable" = start)
    if let Some(stopped) = parsed["charging_stopped"].as_bool() {
        let mode = if stopped { "enable" } else { "disable" };
        let params = format!(r#"{{"direct_power_supply_mode":"{mode}"}}"#);
        if let Err(e) = ubus::call("zwrt_bsp.charger", "set", Some(&params)) {
            return (
                500,
                json!({"ok": false, "error": format!("charger ubus: {e}")}),
            );
        }
        // Set manual override so enforcer doesn't fight the user
        state.charge_limit.set_manual_override(stopped);
    }

    // Charge limit settings
    if parsed.get("charge_limit_enabled").is_some()
        || parsed.get("charge_limit").is_some()
        || parsed.get("hysteresis").is_some()
    {
        let (cur_enabled, cur_limit, cur_hysteresis, _) = state.charge_limit.get();
        let enabled = parsed["charge_limit_enabled"].as_bool().unwrap_or(cur_enabled);
        let limit = parsed["charge_limit"]
            .as_u64()
            .map(|v| v as u8)
            .unwrap_or(cur_limit);
        let hysteresis = parsed["hysteresis"]
            .as_u64()
            .map(|v| v as u8)
            .unwrap_or(cur_hysteresis);

        if let Err(e) = state.charge_limit.set(enabled, limit, hysteresis) {
            return (400, json!({"ok": false, "error": e}));
        }
    }

    // Return updated state
    charge_control_get(state)
}

pub fn device_power_save_get(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match ubus::call(
        "zwrt_mc.device.manager",
        "get_device_info",
        Some(&parsed.to_string()),
    ) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn device_power_save_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match ubus::call(
        "zwrt_mc.device.manager",
        "set_device_info",
        Some(&parsed.to_string()),
    ) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn device_fast_boot_get(_state: &AppState) -> (u16, Value) {
    let params = r#"{"deviceInfoList":["quicken_power_on"]}"#;
    match ubus::call("zwrt_mc.device.manager", "get_device_info", Some(params)) {
        Ok(data) => {
            let val = data["quicken_power_on"].as_str().unwrap_or("0");
            (200, json!({"ok": true, "data": {"fast_boot": val}}))
        }
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn device_fast_boot_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let val = match parsed["fast_boot"].as_str() {
        Some(v @ ("0" | "1")) => v,
        _ => return (400, json!({"ok": false, "error": "fast_boot must be \"0\" or \"1\""})),
    };
    let params = format!(r#"{{"deviceInfoList":{{"quicken_power_on":"{val}"}}}}"#);
    match ubus::call("zwrt_mc.device.manager", "set_device_info", Some(&params)) {
        Ok(_) => (200, json!({"ok": true, "data": {"fast_boot": val}})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}
