use serde_json::{json, Value};

use crate::handlers::AppState;
use crate::ubus;

pub fn cell_lock_nr(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match ubus::call("zte_nwinfo_api", "nwinfo_lock_nr_cell", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn cell_lock_lte(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match ubus::call("zte_nwinfo_api", "nwinfo_lock_lte_cell", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn cell_lock_reset(_state: &AppState) -> (u16, Value) {
    match ubus::call("zte_nwinfo_api", "nwinfo_reset_band_cell_setting", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn cell_neighbors_nr(_state: &AppState) -> (u16, Value) {
    match ubus::call("zte_nwinfo_api", "nwinfo_get_nr5g_nbr_contents", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn cell_neighbors_lte(_state: &AppState) -> (u16, Value) {
    match ubus::call("zte_nwinfo_api", "nwinfo_get_lte_nbr_contents", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

/// 原厂网页（developer_options.js）锁 NR 频段时 `nr5g_type` 传 SA "0"、NSA "1"。
/// 管理网页一直传的是 "sa"/"nsa"，固件不认；这里统一换成原厂的值。
fn normalize_nr5g_type(v: &mut Value) {
    let Some(t) = v.get("nr5g_type").and_then(Value::as_str) else { return };
    let fixed = match t.to_ascii_lowercase().as_str() {
        "sa" => "0",
        "nsa" => "1",
        _ => return,
    };
    v["nr5g_type"] = json!(fixed);
}

pub fn cell_band_nr(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let mut parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    normalize_nr5g_type(&mut parsed);
    match ubus::call("zte_nwinfo_api", "nwinfo_set_nrbandlock", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

/// LTE 锁频按原厂网页（developer_options.js 的 saveLte）走 `nwinfo_set_lte_ext_band`，
/// 参数是逗号分隔的频段号 `{"lte_band":"1,3,7"}`；触屏和 datad 的 band.set_lte 也是这条。
/// 以前这里调 `nwinfo_set_gwl_bandlock`（要位掩码），网页却传逗号列表。旧的请求体
/// `{"lte_band_mask":"1,3,7",...}` 仍然认。
fn lte_band_list(v: &Value) -> Result<String, &'static str> {
    let list = v
        .get("lte_band")
        .or_else(|| v.get("lte_band_mask"))
        .and_then(Value::as_str)
        .ok_or("missing lte_band")?;
    if list.is_empty()
        || list.starts_with(',')
        || list.ends_with(',')
        || list.contains(",,")
        || !list.bytes().all(|b| b.is_ascii_digit() || b == b',')
    {
        return Err("lte_band must be band numbers separated by commas");
    }
    Ok(list.to_string())
}

pub fn cell_band_lte(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let list = match lte_band_list(&parsed) {
        Ok(l) => l,
        Err(e) => return (400, json!({"ok": false, "error": e})),
    };
    let args = json!({ "lte_band": list }).to_string();
    match ubus::call("zte_nwinfo_api", "nwinfo_set_lte_ext_band", Some(&args)) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn cell_band_reset(_state: &AppState) -> (u16, Value) {
    match ubus::call("zte_nwinfo_api", "nwinfo_rest_band_rat", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn cell_stc_params_get(_state: &AppState) -> (u16, Value) {
    match ubus::call("zte_nwinfo_api", "nwinfo_get_stc_white_list_par", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn cell_stc_params_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match ubus::call("zte_nwinfo_api", "nwinfo_set_stc_white_list_par", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn cell_stc_status(_state: &AppState) -> (u16, Value) {
    match ubus::call("zte_nwinfo_api", "nwinfo_get_stc_white_list_status", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn cell_stc_enable(_state: &AppState) -> (u16, Value) {
    match ubus::call("zte_nwinfo_api", "nwinfo_stc_cell_lock_enable", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn cell_stc_disable(_state: &AppState) -> (u16, Value) {
    match ubus::call("zte_nwinfo_api", "nwinfo_stc_cell_lock_disable", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn cell_stc_reset(_state: &AppState) -> (u16, Value) {
    match ubus::call("zte_nwinfo_api", "nwinfo_stc_cell_lock_reset", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn cell_signal_detect_start(_state: &AppState) -> (u16, Value) {
    match ubus::call("zte_nwinfo_api", "nwinfo_start_detect_signal_quality", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn cell_signal_detect_stop(_state: &AppState) -> (u16, Value) {
    match ubus::call("zte_nwinfo_api", "nwinfo_end_detect_signal_quality", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn cell_signal_detect_results(_state: &AppState) -> (u16, Value) {
    match ubus::call("zte_nwinfo_api", "nwinfo_get_detect_quality_recorder", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn cell_signal_detect_progress(_state: &AppState) -> (u16, Value) {
    match ubus::call("zte_nwinfo_api", "nwinfo_get_progress_and_quality", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nr5g_type_uses_vendor_values() {
        for (input, want) in [("sa", "0"), ("nsa", "1"), ("SA", "0"), ("0", "0"), ("1", "1")] {
            let mut v = json!({"nr5g_type": input, "nr5g_band": "78"});
            normalize_nr5g_type(&mut v);
            assert_eq!(v["nr5g_type"], want, "{input}");
            assert_eq!(v["nr5g_band"], "78");
        }
        let mut v = json!({"nr5g_band": "78"});
        normalize_nr5g_type(&mut v);
        assert_eq!(v, json!({"nr5g_band": "78"}));
    }

    #[test]
    fn lte_band_list_takes_vendor_list_and_old_body() {
        assert_eq!(lte_band_list(&json!({"lte_band": "1,3,7"})).as_deref(), Ok("1,3,7"));
        assert_eq!(
            lte_band_list(&json!({"is_lte_band": "1", "lte_band_mask": "3", "is_gw_band": "0", "gw_band_mask": ""})).as_deref(),
            Ok("3")
        );
        for bad in [json!({}), json!({"lte_band": ""}), json!({"lte_band": "1;reboot"}), json!({"lte_band": "B3"}),
                    json!({"lte_band": ",3"}), json!({"lte_band": "3,"}), json!({"lte_band": "1,,3"}), json!({"lte_band": 3})] {
            assert!(lte_band_list(&bad).is_err(), "{bad}");
        }
    }
}
