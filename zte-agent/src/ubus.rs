use std::process::Command;

use serde_json::Value;

/// Read-only `ubus call <object> <method> [<params>]`, JSON output parsed.
/// Refuses any method that does not look like a read ([`is_read_method`]):
/// every device write goes through datad (`datad_write`; write-op-layer.md
/// T7). There is no direct ubus write here any more; `tests/no_direct_writes.rs`
/// keeps it that way.
pub fn read(object: &str, method: &str, params: Option<&str>) -> Result<Value, String> {
    read_with_timeout(object, method, params, None)
}

/// [`read`], giving up after `secs` (`ubus -t`; the CLI's own default is 30 s).
pub fn read_with_timeout(object: &str, method: &str, params: Option<&str>, secs: Option<u32>) -> Result<Value, String> {
    if !is_read_method(method) {
        return Err(format!("ubus {object} {method}: not a read; writes go through datad"));
    }
    run(object, method, params, secs)
}

/// Methods that only read, by the vendor's naming: `get`/`list`/`status`/
/// `info`/`report` and the netselect polls (`…_status`, `…_result`, `…_contents`).
pub fn is_read_method(method: &str) -> bool {
    let m = method.to_ascii_lowercase();
    matches!(m.as_str(), "list" | "status" | "info" | "report")
        || m.starts_with("get")
        || m.contains("_get_")
        || m.contains("get_")
        || ["_status", "_result", "_contents"].iter().any(|s| m.ends_with(s))
}

fn run(object: &str, method: &str, params: Option<&str>, secs: Option<u32>) -> Result<Value, String> {
    let mut cmd = Command::new("ubus");
    if let Some(t) = secs {
        cmd.args(["-t", &t.to_string()]);
    }
    cmd.args(["call", object, method]);
    if let Some(p) = params {
        cmd.arg(p);
    }
    let output = cmd.output().map_err(|e| format!("ubus exec: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = match stderr.find("\nUsage:") {
            Some(pos) => &stderr[..pos],
            None => &stderr,
        };
        return Err(format!(
            "ubus call {object} {method} failed: {}",
            stderr.trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(trimmed).map_err(|e| format!("ubus JSON parse: {e}"))
}

/// Does `object` offer `method`? From `ubus -v list <object>`, which only
/// describes, it calls nothing. For setters a firmware dropped (B31 removed
/// `zwrt_bsp.usb set`), so a page can say so before anyone presses the button.
/// None when the list could not be read.
pub fn has_method(object: &str, method: &str) -> Option<bool> {
    let output = Command::new("ubus").args(["-v", "list", object]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(lists_method(&String::from_utf8_lossy(&output.stdout), method))
}

/// `ubus -v list` prints one `"<method>":{<args>}` line per method.
fn lists_method(listing: &str, method: &str) -> bool {
    let want = format!("\"{method}\":");
    listing.lines().any(|l| l.trim_start().starts_with(&want))
}

/// Run `uci get <key>` and return the value.
pub fn uci_get(key: &str) -> Result<String, String> {
    let output = Command::new("uci")
        .args(["get", key])
        .output()
        .map_err(|e| format!("uci exec: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("uci get {key}: {stderr}"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_writes_by_name() {
        for m in ["list", "status", "info", "report", "get_wwaniface", "nwinfo_get_netinfo", "router_get_qos", "getHostHints",
            "zte_libwms_get_sms_data", "wlan_get_guest_access_left_time", "nwinfo_m_netselect_status", "nwinfo_m_netselect_result",
            "nwinfo_m_netselect_contents", "get_apn_at_cid"] {
            assert!(is_read_method(m), "{m}");
        }
        for m in ["set", "reload", "set_wwaniface", "nwinfo_set_netselect", "nwinfo_rest_band_rat", "nwinfo_reset_band_cell_setting",
            "enable_manu_apn_id", "add_manu_apn", "zte_libwms_send_sms", "zwrt_wms_delete_sms", "zwrt_wms_modify_tag", "device_reboot",
            "nwinfo_manual_scan", "nwinfo_manual_register", "set_qcliiface", "enableAutoSleep", "router_set_lan_para", "reboot"] {
            assert!(!is_read_method(m), "{m}");
        }
        assert!(read("zwrt_data", "set_wwaniface", None).unwrap_err().contains("not a read"));
    }

    #[test]
    fn finds_methods_in_a_listing() {
        let b31 = "'zwrt_bsp.usb' @8577ea77\n\t\"list\":{}\n";
        let b27 = "'zwrt_bsp.usb' @21414706\n        \"list\":{}\n        \"set\":{\"mode\":\"String\"}\n";
        assert!(lists_method(b31, "list"));
        assert!(!lists_method(b31, "set"));
        assert!(lists_method(b27, "set"));
        assert!(!lists_method(b27, "se"));
    }
}
