use serde_json::{json, Value};

use crate::handlers::AppState;
use crate::ubus;

pub fn modem_data_get(_state: &AppState) -> (u16, Value) {
    match ubus::read("zwrt_data", "get_wwaniface", Some(r#"{"cid":1}"#)) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn modem_data_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let Value::Object(asked) = &parsed else {
        return (400, json!({"ok": false, "error": "body must be an object"}));
    };
    // datad's cellular.set changes only what is named (it reads the rest from
    // the modem first, which also keeps the PDP settings stored in this
    // object). `cid` must be 1; `connect_status` (the page sends
    // "disconnected" with data off) is left to the modem.
    let params = match cellular_params(asked) {
        Ok(p) => p,
        Err(e) => return (400, json!({"ok": false, "error": e})),
    };
    match crate::datad_write::send("cellular.set", &params) {
        crate::datad_write::Reply::Failed { error, op } => {
            // B27 answers "Unknown error" while it re-dials (seen 2026-09-25
            // turning roaming on after an eSIM switch) yet keeps the setting.
            // Read back before calling it a failure.
            std::thread::sleep(std::time::Duration::from_millis(800));
            match ubus::read("zwrt_data", "get_wwaniface", Some(r#"{"cid":1}"#)) {
                Ok(now) if applied(&parsed, &now) => (200, json!({"ok": true, "data": now})),
                _ => crate::datad_write::Reply::Failed { error, op }.into_http(),
            }
        }
        r => r.into_http(),
    }
}

/// The web body (`enable` / `roam_enable` / `connect_mode`, `cid` 1,
/// `connect_status`) → datad `cellular.set` params.
fn cellular_params(asked: &serde_json::Map<String, Value>) -> Result<Value, String> {
    let mut p = serde_json::Map::new();
    for (k, v) in asked {
        match k.as_str() {
            "enable" | "roam_enable" => {
                let on = match v {
                    Value::Bool(b) => *b,
                    Value::Number(n) if n.as_i64() == Some(0) || n.as_i64() == Some(1) => n.as_i64() == Some(1),
                    _ => return Err(format!("{k} must be 0 or 1")),
                };
                // datad maps these as integers (control.rs `mapped`)
                p.insert(if k == "enable" { "enabled" } else { "roaming" }.into(), json!(on as u8));
            }
            "connect_mode" => {
                let m = match v {
                    Value::Number(n) if n.as_u64().is_some() => n.to_string(),
                    Value::String(s) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => s.clone(),
                    _ => return Err("connect_mode must be a number".into()),
                };
                p.insert("connect_mode".into(), json!(m));
            }
            "cid" if v.as_i64() == Some(1) => {}
            "cid" => return Err("only cid 1".into()),
            "connect_status" => {}
            _ => return Err(format!("unknown field {k}")),
        }
    }
    if p.is_empty() {
        return Err("nothing to change: enable, roam_enable or connect_mode".into());
    }
    Ok(Value::Object(p))
}

/// Every switch the request set (enable / roam_enable / connect_mode) now reads back the same.
fn applied(want: &Value, now: &Value) -> bool {
    let keys = ["enable", "roam_enable", "connect_mode"];
    let asked: Vec<_> = keys.iter().filter(|k| want.get(**k).is_some()).collect();
    !asked.is_empty() && asked.iter().all(|k| want[**k].as_i64().is_some() && want[**k].as_i64() == now[**k].as_i64())
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
    let Some(mode) = parsed["operate_mode"].as_str() else {
        return (400, json!({"ok": false, "error": "missing operate_mode"}));
    };
    crate::datad_write::send("modem.airplane", &json!({"operate_mode": mode})).into_http()
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
    crate::datad_write::send("network.set_mode", &json!({ "mode": v })).into_http()
}

pub fn modem_scan_status(_state: &AppState) -> (u16, Value) {
    match ubus::read("zte_nwinfo_api", "nwinfo_m_netselect_status", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn modem_scan_results(_state: &AppState) -> (u16, Value) {
    match ubus::read("zte_nwinfo_api", "nwinfo_m_netselect_contents", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn modem_register_result(_state: &AppState) -> (u16, Value) {
    match ubus::read("zte_nwinfo_api", "nwinfo_m_netselect_result", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applied_compares_only_requested_switches() {
        let now = json!({"enable": 1, "roam_enable": 1, "connect_mode": 1, "cid": 1});
        assert!(applied(&json!({"cid": 1, "roam_enable": 1}), &now));
        assert!(!applied(&json!({"cid": 1, "roam_enable": 0}), &now));
        assert!(!applied(&json!({"cid": 1}), &now));
    }
}

#[cfg(test)]
mod cellular_tests {
    use super::*;

    fn p(v: Value) -> Result<Value, String> {
        cellular_params(v.as_object().unwrap())
    }

    #[test]
    fn web_body_to_datad() {
        assert_eq!(p(json!({"cid":1,"enable":0,"connect_status":"disconnected"})), Ok(json!({"enabled":0})));
        assert_eq!(p(json!({"cid":1,"roam_enable":1})), Ok(json!({"roaming":1})));
        assert_eq!(p(json!({"connect_mode":1})), Ok(json!({"connect_mode":"1"})));
        assert_eq!(p(json!({"enable":true,"roam_enable":false})), Ok(json!({"enabled":1,"roaming":0})));
        for bad in [json!({"cid":1}), json!({"enable":2}), json!({"cid":2,"enable":1}), json!({"apn":"x"}), json!({"connect_mode":"a"})] {
            assert!(p(bad.clone()).is_err(), "{bad}");
        }
    }
}
