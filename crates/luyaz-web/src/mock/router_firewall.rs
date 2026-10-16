//! /router/firewall's part of the mock u60d: the `router` block's `firewall`, `upnp`,
//! `port_forward` and `filter_rules` keys, and the seven `zwrt_router.api` writes the page makes.
//! `POST /mock/fw` (JSON) sets any of those keys (and `dhcp` fields under `"dhcp"`) as given;
//! `POST /mock/fw` with no body puts the defaults back.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};

use super::{Mock, set_field};

const METHODS: [&str; 7] = [
    "router_set_firewall_switch",
    "router_set_firewall_level",
    "router_set_nat_switch",
    "router_set_dmz",
    "router_set_upnp_switch",
    "router_set_portforward",
    "router_set_portforward_switch",
];

fn router_key(m: &Mock, key: &str) -> Value {
    m.blocks
        .lock()
        .unwrap()
        .get("router")
        .map_or(Value::Null, |b| b["data"][key].clone())
}

fn flag(v: &Value) -> Option<bool> {
    match v.as_str() {
        Some("1") => Some(true),
        Some("0") => Some(false),
        _ => None,
    }
}

/// `/control` bodies this page's writes send; None = not this page's.
pub(super) fn control(m: &Arc<Mock>, body: &Value, noreadback: bool) -> Option<Response> {
    let p = &body["params"];
    if body["action"] != "vendor.call" || p["object"] != "zwrt_router.api" {
        return None;
    }
    let method = p["method"].as_str().filter(|x| METHODS.contains(x))?;
    m.page_ops.lock().unwrap().push(body.clone());
    let a = &p["args"];
    let bad = |msg: &str| {
        let e = json!({"ok":false,"action":"vendor.call","error":{"code":"invalid_parameter","message":msg}});
        Some((StatusCode::BAD_REQUEST, axum::Json(e)).into_response())
    };
    let mut fw = router_key(m, "firewall");
    let (key, v) = match method {
        "router_set_upnp_switch" => {
            let Some(on) = a["enable_upnp"].as_i64() else {
                return bad("args.enable_upnp");
            };
            let mut u = router_key(m, "upnp");
            u["enabled"] = json!(on == 1);
            ("upnp", u)
        }
        "router_set_portforward" => {
            let mut list = router_key(m, "port_forward")
                .as_array()
                .cloned()
                .unwrap_or_default();
            match a["action"].as_str() {
                Some("add") => {
                    let n = list
                        .iter()
                        .filter_map(|r| r["id"].as_str()?.parse::<u64>().ok())
                        .max()
                        .unwrap_or(0)
                        + 1;
                    list.push(json!({"id": n.to_string(), "name": a["name"], "protocol": a["protocol"], "wan_port": a["wan_port"],
                        "lan_ip": a["lan_ip"], "lan_port": a["lan_port"], "enabled": flag(&a["enabled"])}));
                }
                Some("delete") => {
                    let Some(id) = a["id"].as_str() else {
                        return bad("args.id");
                    };
                    list.retain(|r| r["id"] != id);
                }
                _ => return bad("args.action"),
            }
            ("port_forward", json!(list))
        }
        _ => {
            let (k, v) = match method {
                "router_set_firewall_switch" => ("enabled", json!(flag(&a["firewall_switch"]))),
                "router_set_nat_switch" => ("nat", json!(flag(&a["nat_switch"]))),
                "router_set_portforward_switch" => {
                    ("port_forward", json!(flag(&a["port_forward_switch"])))
                }
                "router_set_firewall_level" => ("level", a["firewall_level"].clone()),
                _ => {
                    fw["dmz_ip"] = a["dmz_ip"].clone();
                    ("dmz", json!(flag(&a["dmz_enabled"])))
                }
            };
            if v.is_null() {
                return bad("args");
            }
            fw[k] = v;
            ("firewall", fw)
        }
    };
    if !noreadback {
        let m = m.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1200)).await;
            set_field(&m, "router", key, v);
        });
    }
    Some(
        axum::Json(json!({"ok":true,"action":"vendor.call","result":{"result":"success"}}))
            .into_response(),
    )
}

fn defaults(m: &Mock) {
    set_field(
        m,
        "router",
        "firewall",
        json!({"enabled": true, "level": "medium", "nat": true, "dmz": false, "dmz_ip": "",
        "port_forward": true, "wan_ping": false, "remote_web": false}),
    );
    set_field(
        m,
        "router",
        "upnp",
        json!({"enabled": true, "natpmp": 1, "notify_interval": 30, "ttl": 4}),
    );
    set_field(
        m,
        "router",
        "port_forward",
        json!([{"id": "1", "name": "NAS HTTPS", "protocol": "TCP", "wan_port": "8443",
        "lan_ip": "192.168.0.20", "lan_port": "443", "enabled": true}]),
    );
    set_field(m, "router", "filter_rules", json!([]));
}

/// Publish this page's default data before anyone connects. The recording's `dhcp` has no
/// `netmask` (older than batch 4): a /24 is added when missing (the e2e sets its own LAN).
pub(super) fn seed(m: &Arc<Mock>) {
    defaults(m);
    let has = m
        .blocks
        .lock()
        .unwrap()
        .get("dhcp")
        .is_some_and(|b| b["data"]["netmask"].is_string());
    if !has {
        set_field(m, "dhcp", "netmask", json!("255.255.255.0"));
    }
}

async fn set(State(m): State<Arc<Mock>>, body: Bytes) -> StatusCode {
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let Some(o) = v.as_object() else {
        defaults(&m);
        return StatusCode::NO_CONTENT;
    };
    for (k, x) in o {
        if k == "dhcp" {
            for (dk, dv) in x.as_object().into_iter().flatten() {
                set_field(&m, "dhcp", dk, dv.clone());
            }
        } else {
            set_field(&m, "router", k, x.clone());
        }
    }
    StatusCode::NO_CONTENT
}

/// This page's `/mock/*` test hooks.
pub(super) fn routes(r: Router<Arc<Mock>>) -> Router<Arc<Mock>> {
    r.route("/mock/fw", post(set))
}
