//! /router/lan's part of the mock u60d: the `dhcp` block and the page's one write, `lan.set`
//! (u60d maps it onto `router_set_lan_para`). The readback follows OpenWrt: `start` is the offset
//! of `dhcp_start` from the network address, `limit` the count up to `dhcp_end`.
//! `POST /mock/lan` (JSON) sets `dhcp` fields as given (`"stale": true` republishes the block as
//! stale); with no body it puts the defaults back.

use std::net::Ipv4Addr;
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

/// `/control` bodies this page's writes send; None = not this page's.
pub(super) fn control(m: &Arc<Mock>, body: &Value, noreadback: bool) -> Option<Response> {
    if body["action"] != "lan.set" {
        return None;
    }
    m.page_ops.lock().unwrap().push(body.clone());
    let p = &body["params"];
    let bad = |msg: &str| {
        let e = json!({"ok":false,"action":"lan.set","error":{"code":"invalid_parameter","message":msg}});
        Some((StatusCode::BAD_REQUEST, axum::Json(e)).into_response())
    };
    let ip = p["ip"].as_str().and_then(|s| s.parse::<Ipv4Addr>().ok());
    let mask = p["netmask"]
        .as_str()
        .and_then(|s| s.parse::<Ipv4Addr>().ok());
    let s = p["dhcp_start"]
        .as_str()
        .and_then(|s| s.parse::<Ipv4Addr>().ok());
    let e = p["dhcp_end"]
        .as_str()
        .and_then(|s| s.parse::<Ipv4Addr>().ok());
    let (Some(ip), Some(mask), Some(s), Some(e)) = (ip, mask, s, e) else {
        return bad("params");
    };
    let Some(off) = p["dhcp_disabled"].as_i64() else {
        return bad("params.dhcp_disabled must be an integer");
    };
    let n = u32::from(ip) & u32::from(mask);
    let start = u32::from(s).wrapping_sub(n);
    let limit = u32::from(e).wrapping_sub(u32::from(s)) + 1;
    let fields = [
        ("ip", json!(ip.to_string())),
        ("netmask", json!(mask.to_string())),
        ("dhcp_enabled", json!(off == 0)),
        ("start", json!(start.to_string())),
        ("limit", json!(limit.to_string())),
        ("leasetime", p["lease_seconds"].clone()),
    ];
    if !noreadback {
        let m = m.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1200)).await;
            for (k, v) in fields {
                set_field(&m, "dhcp", k, v);
            }
        });
    }
    Some(axum::Json(json!({"ok":true,"action":"lan.set","result":{}})).into_response())
}

fn defaults(m: &Mock) {
    for (k, v) in [
        ("ip", json!("192.168.0.1")),
        ("netmask", json!("255.255.255.0")),
        ("dhcp_enabled", json!(true)),
        ("start", json!("100")),
        ("limit", json!("50")),
        ("leasetime", json!("43200")),
    ] {
        set_field(m, "dhcp", k, v);
    }
}

/// Publish this page's default data before anyone connects: only what the recording's `dhcp`
/// lacks (it is older than batch 4: no `netmask`, no `dhcp_enabled`).
pub(super) fn seed(m: &Arc<Mock>) {
    let d = m
        .blocks
        .lock()
        .unwrap()
        .get("dhcp")
        .map(|b| b["data"].clone());
    match d {
        None => defaults(m),
        Some(d) => {
            if !d["dhcp_enabled"].is_boolean() {
                set_field(m, "dhcp", "dhcp_enabled", json!(true));
            }
        }
    }
}

async fn set(State(m): State<Arc<Mock>>, body: Bytes) -> StatusCode {
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let Some(o) = v.as_object() else {
        defaults(&m);
        return StatusCode::NO_CONTENT;
    };
    for (k, x) in o {
        if k != "stale" {
            set_field(&m, "dhcp", k, x.clone());
        }
    }
    if o.get("stale") == Some(&json!(true)) {
        let b = m.blocks.lock().unwrap().get("dhcp").cloned();
        if let Some(b) = b {
            m.publish(
                "dhcp",
                b["data"].clone(),
                b["observed_at"].as_u64().unwrap_or(0) + 1,
                true,
            );
        }
    }
    StatusCode::NO_CONTENT
}

/// This page's `/mock/*` test hooks.
pub(super) fn routes(r: Router<Arc<Mock>>) -> Router<Arc<Mock>> {
    r.route("/mock/lan", post(set))
}
