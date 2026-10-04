use serde_json::{json, Value};

use crate::handlers::AppState;
use crate::ubus;

/// The mode setter the vendor offers; B31 dropped it (only `list` is left).
fn mode_settable() -> Option<bool> {
    ubus::has_method("zwrt_bsp.usb", "set")
}

const MODE_UNSUPPORTED: &str = "this firmware has no USB mode setter (zwrt_bsp.usb set is gone since B31)";

/// GET /api/usb/status: the vendor's `list`, plus `mode_settable` (false when
/// the firmware has no setter, absent when that could not be checked).
pub fn usb_status(_state: &AppState) -> (u16, Value) {
    match ubus::read("zwrt_bsp.usb", "list", Some("{}")) {
        Ok(mut data) => {
            if let (Some(obj), Some(settable)) = (data.as_object_mut(), mode_settable()) {
                obj.insert("mode_settable".into(), json!(settable));
            }
            (200, json!({"ok": true, "data": data}))
        }
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn usb_mode_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    // web {mode} (and the vendor's own key names) → datad usb.set
    let mut p = serde_json::Map::new();
    for (from, to) in [("mode", "mode"), ("port_switch", "port_switch"), ("usb_port_switch", "port_switch"), ("network_protocol", "network_protocol"), ("usb_network_protocal", "network_protocol")] {
        match parsed.get(from) {
            None => {}
            Some(Value::String(s)) => {
                p.insert(to.into(), json!(s));
            }
            Some(Value::Number(n)) => {
                p.insert(to.into(), json!(n.to_string()));
            }
            Some(_) => return (400, json!({"ok": false, "error": format!("{from} must be a string")})),
        }
    }
    if p.is_empty() {
        return (400, json!({"ok": false, "error": "nothing to set: mode"}));
    }
    if mode_settable() == Some(false) {
        return (501, json!({"ok": false, "error": MODE_UNSUPPORTED}));
    }
    crate::datad_write::send("usb.set", &Value::Object(p)).into_http()
}

pub fn usb_powerbank_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match crate::datad_write::vendor("zwrt_bsp.powerbank", "set", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}
