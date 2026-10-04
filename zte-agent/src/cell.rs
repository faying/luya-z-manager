use serde_json::{json, Value};

use crate::handlers::AppState;
use crate::ubus;

pub fn cell_lock_nr(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match lock_params(&parsed, true) {
        Ok(p) => crate::datad_write::send("cell.lock_nr", &p).into_http(),
        Err(e) => (400, json!({"ok": false, "error": e})),
    }
}

pub fn cell_lock_lte(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match lock_params(&parsed, false) {
        Ok(p) => crate::datad_write::send("cell.lock_lte", &p).into_http(),
        Err(e) => (400, json!({"ok": false, "error": e})),
    }
}

/// The web body `{pci, earfcn, band?}` → datad `cell.lock_lte {pci, earfcn}` /
/// `cell.lock_nr {pci, arfcn, band}` (strings). The vendor's own key names
/// (`lock_lte_pci`, …) are taken too. Until 10-03 the body went to the
/// vendor as it was, under keys it does not know.
fn lock_params(v: &Value, nr: bool) -> Result<Value, String> {
    let get = |names: &[&str]| -> Option<String> {
        names.iter().find_map(|n| match v.get(*n)? {
            Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
    };
    let num = |name: &str, x: Option<String>| -> Result<String, String> {
        let x = x.ok_or_else(|| format!("missing {name}"))?;
        if x.bytes().all(|b| b.is_ascii_digit()) { Ok(x) } else { Err(format!("{name} must be a number")) }
    };
    if nr {
        Ok(json!({
            "pci": num("pci", get(&["pci", "lock_nr_pci"]))?,
            "arfcn": num("earfcn", get(&["earfcn", "arfcn", "lock_nr_earfcn"]))?,
            "band": num("band", get(&["band", "lock_nr_cell_band"]).map(|b| b.trim_start_matches(['n', 'N']).to_string()))?,
        }))
    } else {
        Ok(json!({
            "pci": num("pci", get(&["pci", "lock_lte_pci"]))?,
            "earfcn": num("earfcn", get(&["earfcn", "lock_lte_earfcn"]))?,
        }))
    }
}

/// 「锁小区恢复」和「频段恢复默认」都是原厂的 `nwinfo_reset_band_cell_setting`
/// （datad band.reset，频段和小区锁一起恢复，D22）。
pub fn cell_lock_reset(_state: &AppState) -> (u16, Value) {
    crate::datad_write::send("band.reset", &json!({})).into_http()
}

pub fn cell_neighbors_nr(_state: &AppState) -> (u16, Value) {
    match ubus::read("zte_nwinfo_api", "nwinfo_get_nr5g_nbr_contents", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn cell_neighbors_lte(_state: &AppState) -> (u16, Value) {
    match ubus::read("zte_nwinfo_api", "nwinfo_get_lte_nbr_contents", Some("{}")) {
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
    let action = match parsed.get("nr5g_type").and_then(Value::as_str) {
        Some("0") => "band.set_nr_sa",
        Some("1") => "band.set_nr_nsa",
        _ => return (400, json!({"ok": false, "error": "nr5g_type must be sa or nsa"})),
    };
    let Some(bands) = parsed.get("nr5g_band").and_then(Value::as_str) else {
        return (400, json!({"ok": false, "error": "missing nr5g_band"}));
    };
    crate::datad_write::send(action, &json!({ "bands": bands })).into_http()
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
    crate::datad_write::send("band.set_lte", &json!({ "bands": list })).into_http()
}

/// 原厂网页的「恢复默认」只有 `nwinfo_reset_band_cell_setting`（D21/D22）；
/// 以前这里调的 `nwinfo_rest_band_rat` 原厂网页里没有，已删。
pub fn cell_band_reset(state: &AppState) -> (u16, Value) {
    cell_lock_reset(state)
}

pub fn cell_stc_params_get(_state: &AppState) -> (u16, Value) {
    match ubus::read("zte_nwinfo_api", "nwinfo_get_stc_white_list_par", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn cell_stc_params_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match crate::datad_write::vendor("zte_nwinfo_api", "nwinfo_set_stc_white_list_par", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn cell_stc_status(_state: &AppState) -> (u16, Value) {
    match ubus::read("zte_nwinfo_api", "nwinfo_get_stc_white_list_status", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn cell_stc_enable(_state: &AppState) -> (u16, Value) {
    match crate::datad_write::vendor("zte_nwinfo_api", "nwinfo_stc_cell_lock_enable", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn cell_stc_disable(_state: &AppState) -> (u16, Value) {
    match crate::datad_write::vendor("zte_nwinfo_api", "nwinfo_stc_cell_lock_disable", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn cell_stc_reset(_state: &AppState) -> (u16, Value) {
    match crate::datad_write::vendor("zte_nwinfo_api", "nwinfo_stc_cell_lock_reset", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn cell_signal_detect_start(_state: &AppState) -> (u16, Value) {
    match crate::datad_write::vendor("zte_nwinfo_api", "nwinfo_start_detect_signal_quality", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn cell_signal_detect_stop(_state: &AppState) -> (u16, Value) {
    match crate::datad_write::vendor("zte_nwinfo_api", "nwinfo_end_detect_signal_quality", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn cell_signal_detect_results(_state: &AppState) -> (u16, Value) {
    match ubus::read("zte_nwinfo_api", "nwinfo_get_detect_quality_recorder", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn cell_signal_detect_progress(_state: &AppState) -> (u16, Value) {
    match ubus::read("zte_nwinfo_api", "nwinfo_get_progress_and_quality", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

/// The band locks as the firmware keeps them, for the web page's readback
/// after a lock or a reset (E4 T9c): `zte_nwinfo.band_lock.*` is the current
/// set (after locking to n78 it reads "78", datad state.rs), and
/// `zwrt_zte_nwinfo.default_band_lock.*` the full set a reset goes back to.
/// Comma-separated band numbers; null when the firmware has no such option.
pub fn cell_band_lock_get(_state: &AppState) -> (u16, Value) {
    let get = |k: &str| ubus::uci_get(k).ok().filter(|v| !v.is_empty());
    (
        200,
        json!({"ok": true, "data": {
            "nr_sa": get("zte_nwinfo.band_lock.nr5g_sa_band_lock"),
            "nr_nsa": get("zte_nwinfo.band_lock.nr5g_nsa_band_lock"),
            "lte": get("zte_nwinfo.band_lock.lte_ext_band_lock"),
            "default": {
                "nr_sa": get("zwrt_zte_nwinfo.default_band_lock.default_nr5g_sa_band_lock"),
                "nr_nsa": get("zwrt_zte_nwinfo.default_band_lock.default_nr5g_nsa_band_lock"),
                "lte": get("zwrt_zte_nwinfo.default_band_lock.default_lte_ext_band_lock"),
            },
        }}),
    )
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

#[cfg(test)]
mod lock_tests {
    use super::*;

    #[test]
    fn web_bodies_to_datad() {
        assert_eq!(lock_params(&json!({"pci":"12","earfcn":"1850"}), false), Ok(json!({"pci":"12","earfcn":"1850"})));
        assert_eq!(lock_params(&json!({"lock_lte_pci":12,"lock_lte_earfcn":1850}), false), Ok(json!({"pci":"12","earfcn":"1850"})));
        assert_eq!(lock_params(&json!({"pci":"5","earfcn":"627264","band":"n78"}), true), Ok(json!({"pci":"5","arfcn":"627264","band":"78"})));
        assert!(lock_params(&json!({"pci":"5","earfcn":"627264"}), true).unwrap_err().contains("band"));
        assert!(lock_params(&json!({"pci":"5;reboot","earfcn":"1"}), false).is_err());
        assert!(lock_params(&json!({"earfcn":"1"}), false).is_err());
    }
}

/// GET /api/cell/extra — what the touch screen shows and the web lacked
/// (audit C, 10-04): QCI and the session AMBR (datad `qos`, Mbps, from the
/// modem's key log) and the SIM's own number (`sim.msisdn`; many SIMs don't
/// carry it → null). From the subscribed feed; otherwise one `/state` read.
/// High-speed-rail mode is not here: datad's `net.HSR` is always false (no
/// signalling source yet).
pub fn cell_extra_get(_state: &AppState) -> (u16, Value) {
    let view = crate::datad_feed::global().map(|f| f.view());
    let fed = |name: &str| view.as_ref().and_then(|v| v.block(name));
    let (qos, sim, source) = match (fed("qos"), fed("sim")) {
        (Some(q), Some(s)) => (Some(q), Some(s), "feed"),
        _ => match crate::deep_diag::datad_get("/state") {
            Some(st) => (st.get("qos").cloned(), st.get("sim").cloned(), "state"),
            None => return (503, json!({"ok": false, "error": "data service not responding", "error_en": "data service not responding"})),
        },
    };
    (200, json!({"ok": true, "data": extra_json(qos.as_ref(), sim.as_ref(), source)}))
}

fn extra_json(qos: Option<&Value>, sim: Option<&Value>, source: &str) -> Value {
    let num = |k: &str| crate::deep_diag::num(qos.and_then(|q| q.get(k))).filter(|x| *x > 0.0);
    let msisdn = sim
        .and_then(|s| s.get("msisdn"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|m| !m.is_empty());
    json!({
        "qci": qos.and_then(|q| q.get("qci")).and_then(Value::as_i64).filter(|q| *q > 0),
        "ambr_dl_mbps": num("ambr_dl"),
        "ambr_ul_mbps": num("ambr_ul"),
        "msisdn": msisdn,
        "source": source,
    })
}

#[cfg(test)]
mod extra_tests {
    use super::*;

    #[test]
    fn extra_reads_datads_strings_and_drops_empties() {
        let q = json!({"qci": 9, "ambr_dl": "150.000", "ambr_ul": "75.000"});
        let s = json!({"msisdn": " +8613800000000 "});
        let v = extra_json(Some(&q), Some(&s), "feed");
        assert_eq!(v["qci"], 9);
        assert_eq!(v["ambr_dl_mbps"], 150.0);
        assert_eq!(v["ambr_ul_mbps"], 75.0);
        assert_eq!(v["msisdn"], "+8613800000000");
        let v = extra_json(Some(&json!({"qci": 0, "ambr_dl": "0.000"})), Some(&json!({"msisdn": ""})), "state");
        assert!(v["qci"].is_null() && v["ambr_dl_mbps"].is_null() && v["msisdn"].is_null());
    }
}
