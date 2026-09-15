use serde_json::{json, Value};

use crate::handlers::AppState;
use crate::ubus;

pub fn network_wan(_state: &AppState) -> (u16, Value) {
    match ubus::call("network.interface.zte_wan", "status", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn network_wan6(_state: &AppState) -> (u16, Value) {
    match ubus::call("network.interface.zte_wan6", "status", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn network_lan_status(_state: &AppState) -> (u16, Value) {
    match ubus::call("network.interface.lan", "status", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn network_clients(_state: &AppState) -> (u16, Value) {
    let hints = ubus::call("luci-rpc", "getHostHints", Some("{}"))
        .unwrap_or(Value::Null);
    let dhcp = ubus::call("luci-rpc", "getDHCPLeases", Some(r#"{"family":4}"#))
        .ok()
        .and_then(|v| v.get("dhcp_leases").cloned());
    let mut result = serde_json::Map::new();
    result.insert("hosts".into(), hints);
    if let Some(leases) = dhcp {
        result.insert("dhcp_leases".into(), leases);
    }
    (200, json!({"ok": true, "data": result}))
}

pub fn network_speeds(_state: &AppState) -> (u16, Value) {
    match ubus::call("zwrt_data", "get_wwandst", Some(r#"{"cid":1,"type":1}"#)) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn network_rmnet(_state: &AppState) -> (u16, Value) {
    match ubus::call("network.device", "status", Some(r#"{"name":"rmnet_data0"}"#)) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn network_battery_ubus(_state: &AppState) -> (u16, Value) {
    match ubus::call("zwrt_bsp.battery", "list", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}
