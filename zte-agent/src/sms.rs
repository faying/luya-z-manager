use std::process::Command;

use serde_json::{json, Value};

use crate::handlers::AppState;
use crate::ubus;

const SMS_DB_PATH: &str = "/etc_rw/ztembb/ztesms/sms_db/sms.db";

pub fn sms_list(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };

    // The firmware's zte_libwms_get_sms_data wants STRICT types: mem_store/page/
    // data_per_page/tags must be integers, or it returns "Invalid argument".
    // Clients (web/app) may send a friendly mem_store string ("all"/"sim"/
    // "device") — coerce it, and fill sane defaults so the call never 503s on shape.
    let mut obj = parsed.as_object().cloned().unwrap_or_default();
    let mem_int: i64 = match obj.get("mem_store") {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(1),
        Some(Value::String(s)) => match s.as_str() {
            "sim" => 2,
            _ => 1, // "all" / "device" / "native" / unknown → ME/native store
        },
        _ => 1,
    };
    obj.insert("mem_store".into(), json!(mem_int));
    obj.entry("page").or_insert(json!(0));
    obj.entry("data_per_page").or_insert(json!(500));
    obj.entry("tags").or_insert(json!(10));
    obj.entry("order_by").or_insert(json!("order by id desc"));
    obj.remove("store"); // legacy/no-op key some callers send

    match ubus::call("zwrt_wms", "zte_libwms_get_sms_data", Some(&Value::Object(obj).to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn sms_capacity(_state: &AppState) -> (u16, Value) {
    match ubus::call("zwrt_wms", "zwrt_wms_get_wms_capacity", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn sms_send(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match ubus::call("zwrt_wms", "zte_libwms_send_sms", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

/// Delete one or more SMS by id.
///
/// Body shape (legacy ZTE format): `{"id": "3681;3682;"}` — semicolon-joined ids with trailing `;`.
///
/// Firmware bug: `zwrt_wms_delete_sms` works for NV-stored messages but silently returns
/// `{"result": 3}` without deleting SIM-stored rows. The daemon's listing reads from
/// `/etc_rw/ztembb/ztesms/sms_db/sms.db`, so we fall back to a direct SQLite DELETE for any
/// id that survived the ubus call.
pub fn sms_delete(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };

    let ids = match parse_ids(parsed.get("id")) {
        Ok(v) if !v.is_empty() => v,
        Ok(_) => return (400, json!({"ok": false, "error": "no ids in 'id' field"})),
        Err(e) => return (400, json!({"ok": false, "error": e})),
    };

    let ubus_result = ubus::call("zwrt_wms", "zwrt_wms_delete_sms", Some(&parsed.to_string()));

    let survivors = match db_filter_existing(&ids) {
        Ok(v) => v,
        Err(e) => {
            return match ubus_result {
                Ok(data) => (200, json!({"ok": true, "data": data, "warning": format!("db check skipped: {e}")})),
                Err(ubus_err) => (503, json!({"ok": false, "error": format!("ubus: {ubus_err}; db: {e}")})),
            };
        }
    };

    if survivors.is_empty() {
        return (
            200,
            json!({"ok": true, "data": ubus_result.unwrap_or(Value::Null), "deleted_via": "ubus"}),
        );
    }

    match db_delete_ids(&survivors) {
        Ok(()) => (
            200,
            json!({"ok": true, "deleted_via": "sqlite", "ids": survivors}),
        ),
        Err(e) => (503, json!({"ok": false, "error": format!("sqlite delete failed: {e}")})),
    }
}

/// Parse the legacy ZTE id format (semicolon-joined with trailing `;`).
fn parse_ids(field: Option<&Value>) -> Result<Vec<i64>, String> {
    let raw = field.ok_or_else(|| "missing 'id' field".to_string())?;
    let s = raw.as_str().ok_or_else(|| "'id' must be a string".to_string())?;
    let mut out = Vec::new();
    for part in s.split(';') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        let n: i64 = p
            .parse()
            .map_err(|_| format!("invalid id '{p}' (must be integer)"))?;
        out.push(n);
    }
    Ok(out)
}

/// Return the subset of `ids` still present in the WMS sms table.
fn db_filter_existing(ids: &[i64]) -> Result<Vec<i64>, String> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let in_clause = ids
        .iter()
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("SELECT id FROM sms WHERE id IN ({in_clause});");
    let output = Command::new("/usr/bin/sqlite3")
        .args(["-cmd", ".timeout 2000", "-readonly", SMS_DB_PATH, &sql])
        .output()
        .map_err(|e| format!("spawn sqlite3: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let mut out = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        if let Ok(n) = l.parse::<i64>() {
            out.push(n);
        }
    }
    Ok(out)
}

/// Direct DELETE bypassing the broken ubus path.
fn db_delete_ids(ids: &[i64]) -> Result<(), String> {
    if ids.is_empty() {
        return Ok(());
    }
    let in_clause = ids
        .iter()
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("DELETE FROM sms WHERE id IN ({in_clause});");
    let output = Command::new("/usr/bin/sqlite3")
        .args(["-cmd", ".timeout 2000", SMS_DB_PATH, &sql])
        .output()
        .map_err(|e| format!("spawn sqlite3: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(())
}

pub fn sms_mark_read(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match ubus::call("zwrt_wms", "zwrt_wms_modify_tag", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}
