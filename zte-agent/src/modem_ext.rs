use serde_json::{json, Value};

use crate::handlers::AppState;
use crate::ubus;

pub fn modem_data_get(_state: &AppState) -> (u16, Value) {
    match ubus::call("zwrt_data", "get_wwaniface", Some(r#"{"cid":1}"#)) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn modem_data_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match ubus::call("zwrt_data", "set_wwaniface", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn modem_airplane(state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    // Use AT+CFUN=1 for ONLINE (ubus nwinfo_set_mode ONLINE is broken for LPM→ONLINE recovery)
    if parsed["operate_mode"].as_str() == Some("ONLINE") {
        return crate::handlers::modem_online(state);
    }
    match ubus::call("zte_nwinfo_api", "nwinfo_set_mode", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

/// `net_select` values the firmware knows (strings of B27's
/// /usr/bin/zte_topsw_nwinfo). The touch screen sends WL_AND_5G / Only_5G /
/// LTE_AND_5G / Only_LTE; the web page also WCDMA_AND_LTE and Only_WCDMA; B27 has been seen reporting both
/// "WL_AND_5G" and "TCHGWL_5G" (every RAT + 5G) as automatic. Anything else
/// is refused here rather than handed to the modem unchecked.
const NET_SELECT_VALUES: &[&str] = &[
    "WL_AND_5G", "TCHGWL_5G", "Only_5G", "LTE_AND_5G", "4G_AND_5G", "WL_AND_NSA", "Only_LTE",
    "WCDMA_AND_LTE", "GSM_AND_LTE", "TDSCDMA_AND_LTE", "Only_WCDMA", "Only_GSM_WCDMA", "Only_TDSCDMA", "Only_GSM",
];

pub fn modem_network_mode_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let v = parsed.get("net_select").and_then(Value::as_str).unwrap_or("");
    if !NET_SELECT_VALUES.contains(&v) {
        return (400, json!({"ok": false, "error": format!("net_select must be one of {}", NET_SELECT_VALUES.join(", "))}));
    }
    let parsed = json!({ "net_select": v });
    match ubus::call("zte_nwinfo_api", "nwinfo_set_netselect", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn modem_scan_status(_state: &AppState) -> (u16, Value) {
    match ubus::call("zte_nwinfo_api", "nwinfo_m_netselect_status", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn modem_scan_results(_state: &AppState) -> (u16, Value) {
    match ubus::call("zte_nwinfo_api", "nwinfo_m_netselect_contents", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn modem_register_result(_state: &AppState) -> (u16, Value) {
    match ubus::call("zte_nwinfo_api", "nwinfo_m_netselect_result", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

