use serde_json::{json, Value};

use crate::handlers::AppState;
use crate::ubus;

pub fn router_dns_get(_state: &AppState) -> (u16, Value) {
    match ubus::read("zwrt_router.api", "router_get_dns_para", Some("{}")) {
        Ok(data) => {
            // Firmware returns keys with "wan_" prefix (e.g. wan_dns_mode);
            // strip it so iOS client can use clean names (dns_mode, prefer_dns_manual, etc.)
            let mut cleaned = serde_json::Map::new();
            if let Some(obj) = data.as_object() {
                for (k, v) in obj {
                    let key = k.strip_prefix("wan_").unwrap_or(k).to_string();
                    cleaned.insert(key, v.clone());
                }
            }
            // Firmware bug: sometimes returns empty manual DNS values; fill from UCI
            if cleaned.get("dns_mode").and_then(|v| v.as_str()) == Some("manual") {
                if cleaned
                    .get("prefer_dns_manual")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .is_empty()
                {
                    if let Ok(v) = ubus::uci_get("network.wan.dns") {
                        let mut parts = v.split_whitespace();
                        if let Some(primary) = parts.next() {
                            cleaned.insert("prefer_dns_manual".into(), Value::String(primary.to_string()));
                        }
                        if let Some(secondary) = parts.next() {
                            cleaned.insert("standby_dns_manual".into(), Value::String(secondary.to_string()));
                        }
                    }
                }
            }
            (200, json!({"ok": true, "data": Value::Object(cleaned)}))
        }
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_dns_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match crate::datad_write::vendor("zwrt_router.api", "router_set_wan_dns", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_lan_get(_state: &AppState) -> (u16, Value) {
    let ip = ubus::uci_get("network.lan.ipaddr").unwrap_or_default();
    let mask = ubus::uci_get("network.lan.netmask").unwrap_or_default();
    let ignore = ubus::uci_get("dhcp.lan.ignore").unwrap_or_default();
    let start = ubus::uci_get("dhcp.lan.start").unwrap_or_default();
    let limit = ubus::uci_get("dhcp.lan.limit").unwrap_or_default();
    let lease = ubus::uci_get("dhcp.lan.leasetime").unwrap_or_default();
    let end = compute_dhcp_end(&ip, &start, &limit);
    (200, json!({"ok": true, "data": {
        "lan_ipaddr": ip, "lan_netmask": mask,
        "dhcp_enable": if ignore == "1" { "0" } else { "1" },
        "dhcp_start": start, "dhcp_end": end,
        "dhcp_lease_time": lease
    }}))
}

fn compute_dhcp_end(base_ip: &str, start: &str, limit: &str) -> String {
    let start_num: u32 = start.parse().unwrap_or(100);
    let limit_num: u32 = limit.parse().unwrap_or(50);
    let end_host = start_num + limit_num - 1;
    // Replace last octet of base IP with end_host
    if let Some(prefix) = base_ip.rfind('.') {
        format!("{}.{end_host}", &base_ip[..prefix])
    } else {
        format!("192.168.0.{end_host}")
    }
}

pub fn router_lan_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match crate::datad_write::vendor("zwrt_router.api", "router_set_lan_para", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_firewall_get(_state: &AppState) -> (u16, Value) {
    match ubus::read("zwrt_router.api", "router_get_firewall_para", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_firewall_switch_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match crate::datad_write::vendor("zwrt_router.api", "router_set_firewall_switch", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_firewall_level_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match crate::datad_write::vendor("zwrt_router.api", "router_set_firewall_level", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_firewall_nat_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match crate::datad_write::vendor("zwrt_router.api", "router_set_nat_switch", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_firewall_dmz_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match crate::datad_write::vendor("zwrt_router.api", "router_set_dmz", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

/// GET /api/router/firewall/upnp. The getter is `router_get_upnp` (B27
/// answers {enabled, enable_upnp, notify_interval, ttl, enable_natpmp});
/// `router_get_upnp_switch` does not exist on the firmware.
pub fn router_firewall_upnp_get(_state: &AppState) -> (u16, Value) {
    match ubus::read("zwrt_router.api", "router_get_upnp", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

/// PUT /api/router/firewall/upnp → `router_set_upnp_switch`, which takes
/// integers: {enable_upnp, notify_interval, ttl, natpmp}.
pub fn router_firewall_upnp_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match crate::datad_write::vendor("zwrt_router.api", "router_set_upnp_switch", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_firewall_port_forward_get(_state: &AppState) -> (u16, Value) {
    match ubus::read("zwrt_router.api", "router_get_portforward_rule", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_firewall_port_forward_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match crate::datad_write::vendor("zwrt_router.api", "router_set_portforward", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_firewall_port_forward_switch(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match crate::datad_write::vendor("zwrt_router.api", "router_set_portforward_switch", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_firewall_filter_rules(_state: &AppState) -> (u16, Value) {
    match ubus::read("zwrt_router.api", "router_get_macipport_filter_rule", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_vpn_get(_state: &AppState) -> (u16, Value) {
    match ubus::read("zwrt_router.api", "router_get_alg_para", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_vpn_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match crate::datad_write::vendor("zwrt_router.api", "router_set_alg_switch", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

/// GET /api/router/qos. The getter is `router_get_qos` (B27 answers {}
/// while nothing is configured); `router_get_qos_switch` does not exist.
pub fn router_qos_get(_state: &AppState) -> (u16, Value) {
    match ubus::read("zwrt_router.api", "router_get_qos", Some("{}")) {
        Ok(mut data) => {
            // The vendor "带宽分配" mode (zte_smart_manage). Every mode but
            // Nomal_mode (极速) sorts traffic by app, which needs xdpi, and
            // xdpi is off on purpose (≈25 % CPU, 2026-09-23). Read-only: the
            // owner decided on 2026-09-25 not to offer switching it.
            if let Some(o) = data.as_object_mut() {
                o.insert("smart_qos_mode".into(), json!(ubus::uci_get("zwrt_smart_mng.smart_qos.mode").ok()));
                o.insert("xdpi_support".into(), json!(ubus::uci_get("zwrt_smart_mng.smart_mng.xdpi_support").ok()));
            }
            (200, json!({"ok": true, "data": data}))
        }
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

/// PUT /api/router/qos. The firmware setter is `router_set_qos` and it takes
/// rate limits, not a switch: {upload_total_limit_rate, upload_total_limit_unit,
/// download_total_limit_rate, download_total_limit_unit, qos_smart_switch,
/// qos_smart_pri_type} (all integers). The old `router_set_qos_switch` does not
/// exist, so this still answers 503 until the web page is redesigned around
/// those fields (owner to decide); kept as-is on purpose.
pub fn router_qos_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match crate::datad_write::vendor("zwrt_router.api", "router_set_qos_switch", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_domain_filter_get(_state: &AppState) -> (u16, Value) {
    match ubus::read("zwrt_router.api", "router_get_domainfilter_rule", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_domain_filter_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match crate::datad_write::vendor("zwrt_router.api", "router_set_domain_filter", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_apn_mode_get(_state: &AppState) -> (u16, Value) {
    match ubus::read("zwrt_apn_object", "get_apn_mode", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_apn_mode_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let Some(mode) = parsed.get("apn_mode").and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok())).filter(|m| *m == 0 || *m == 1) else {
        return (400, json!({"ok": false, "error": "apn_mode must be 0 or 1"}));
    };
    crate::datad_write::send("apn.set_mode", &json!({"mode": mode})).into_http()
}

pub fn router_apn_profiles_get(_state: &AppState) -> (u16, Value) {
    match ubus::read("zwrt_apn_object", "get_manu_apn_list", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_apn_profiles_add(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match apn_params(&parsed, false) {
        Ok(p) => crate::datad_write::send("apn.add", &p).into_http(),
        Err(e) => (400, json!({"ok": false, "error": e})),
    }
}

pub fn router_apn_profiles_modify(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match apn_params(&parsed, true) {
        Ok(p) => crate::datad_write::send("apn.modify", &p).into_http(),
        Err(e) => (400, json!({"ok": false, "error": e})),
    }
}

pub fn router_apn_auto_profiles(_state: &AppState) -> (u16, Value) {
    match ubus::read("zwrt_apn_object", "get_auto_apn_list", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_apn_profiles_delete(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match profile_id(&parsed) {
        Some(id) => crate::datad_write::send("apn.delete", &json!({"profile_id": id})).into_http(),
        None => (400, json!({"ok": false, "error": "missing profileId"})),
    }
}

pub fn router_apn_profiles_activate(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match profile_id(&parsed) {
        Some(id) => crate::datad_write::send("apn.enable", &json!({"profile_id": id})).into_http(),
        None => (400, json!({"ok": false, "error": "missing profileId"})),
    }
}

/// GET /api/router/wan-ipv6 — whether the WAN data call requests IPv6 from the
/// operator. This is governed by the dialing APN's PDP type (3GPP: 1=IPv4,
/// 2=IPv6, 3=IPv4v6), regardless of auto/manual APN mode. `wan_has_ipv6`
/// reflects the live PDN, which can lag the config until a reconnect.
pub fn router_wan_ipv6_get(_state: &AppState) -> (u16, Value) {
    let apn = match ubus::read("zwrt_apn_object", "get_apn_at_cid", Some("{\"cid\":1}")) {
        Ok(v) => v,
        Err(e) => return (503, json!({"ok": false, "error": e})),
    };
    let pdp = apn["pdpType"].as_i64().unwrap_or(0);
    let wan_has_ipv6 = ubus::read("zwrt_data", "get_wwaniface",
        Some("{\"source_module\":\"zte_topsw_data\",\"cid\":1}"),
    )
    .ok()
    .and_then(|w| w["ipv6_address"].as_str().map(|s| !s.is_empty()))
    .unwrap_or(false);
    (200, json!({"ok": true, "data": {
        "ipv6_enabled": pdp == 2 || pdp == 3,
        "pdp_type": pdp,
        "wan_has_ipv6": wan_has_ipv6,
    }}))
}

/// PUT /api/router/wan-ipv6 — { enabled: bool }. Turns operator-pushed WAN IPv6
/// on/off by setting the dialing APN's PDP type to IPv4v6 (on) or IPv4-only
/// (off), preserving every other APN field, then applying to the live PDN:
/// disabling drops just the IPv6 leg (IPv4 stays up, no interruption); enabling
/// brings the IPv6 leg up. The PDP-type change is what makes it persist across
/// reconnects and reboots.
pub fn router_wan_ipv6_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let enabled = match parsed["enabled"].as_bool() {
        Some(b) => b,
        None => return (400, json!({"ok": false, "error": "missing 'enabled' boolean"})),
    };

    // datad changes only the dialing APN's PDP type (IPv4v6 / IPv4), keeps
    // every other field, then brings the live IPv6 leg up or down
    match crate::datad_write::send("apn.set_pdp_type", &json!({"ipv6": enabled})) {
        crate::datad_write::Reply::Done { result, .. } => (200, json!({"ok": true, "data": result})),
        r => r.into_http(),
    }
}

fn profile_id(v: &Value) -> Option<String> {
    match v.get("profileId").or_else(|| v.get("profile_id"))? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// A vendor-format APN profile (the web body, or an entry of
/// `get_auto_apn_list`) → datad `apn.add` / `apn.modify` params. B27 keeps
/// `pdpType`, `pppAuthMode`, `roamingPdpType` as integers; datad sends them so.
pub(crate) fn apn_params(v: &Value, modify: bool) -> Result<Value, String> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    let int = |k: &str| -> Result<Option<i64>, String> {
        match v.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Number(n)) => n.as_i64().map(Some).ok_or_else(|| format!("{k} must be a number")),
            Some(Value::String(x)) => x.trim().parse().map(Some).map_err(|_| format!("{k} must be a number (this firmware stores numbers)")),
            _ => Err(format!("{k} must be a number")),
        }
    };
    let mut p = serde_json::Map::new();
    if modify {
        p.insert("profile_id".into(), json!(profile_id(v).ok_or("missing profileId")?));
    }
    p.insert("name".into(), json!(s("profilename").ok_or("missing profilename")?));
    p.insert("apn".into(), json!(s("wanapn").ok_or("missing wanapn")?));
    for (from, to) in [("username", "username"), ("password", "password")] {
        if let Some(x) = s(from) {
            p.insert(to.into(), json!(x));
        }
    }
    for (from, to) in [("pppAuthMode", "auth_mode"), ("pdpType", "pdp_type"), ("roamingPdpType", "roaming_pdp_type")] {
        if let Some(x) = int(from)? {
            p.insert(to.into(), json!(x));
        }
    }
    Ok(Value::Object(p))
}

#[cfg(test)]
mod apn_tests {
    use super::*;

    #[test]
    fn vendor_profile_to_datad() {
        let web = json!({"profilename":"cm","wanapn":"cmnet","pdpType":3,"pppAuthMode":"0","roamingPdpType":3,"username":"","password":"x"});
        assert_eq!(
            apn_params(&web, false),
            Ok(json!({"name":"cm","apn":"cmnet","username":"","password":"x","auth_mode":0,"pdp_type":3,"roaming_pdp_type":3}))
        );
        let mut m = web.clone();
        m["profileId"] = json!("2");
        assert_eq!(apn_params(&m, true).unwrap()["profile_id"], "2");
        assert!(apn_params(&web, true).is_err());
        assert!(apn_params(&json!({"wanapn":"x"}), false).is_err());
        assert!(apn_params(&json!({"profilename":"a","wanapn":"x","pdpType":"IPV4V6"}), false).is_err());
        assert_eq!(profile_id(&json!({"profileId": 3})), Some("3".into()));
        assert_eq!(profile_id(&json!({})), None);
    }
}
