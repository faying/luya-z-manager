//! /router/vpn's mock: `router.alg` like B31 (only SIP ALG reported), `router_set_alg_switch`.
//! `POST /mock/router-vpn?reported=true` makes the three passthrough fields reported (all off),
//! `reported=false` goes back to B31, `failed=true` publishes `alg: null` (the read failed).
//! A write to a reported field lands after 1.2 s (`noreadback`: never); an unreported one never
//! changes (as on B31).

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};

use super::{Mock, set_field};

const ARGS: [(&str, &str); 3] = [
    ("l2tp_passthrough", "l2tp"),
    ("pptp_passthrough", "pptp"),
    ("ipsec_passthrough", "ipsec"),
];

fn b31() -> Value {
    json!({"sip": true, "l2tp": null, "pptp": null, "ipsec": null})
}

fn cur_alg(m: &Mock) -> Value {
    m.blocks
        .lock()
        .unwrap()
        .get("router")
        .map(|b| b["data"]["alg"].clone())
        .unwrap_or(Value::Null)
}

/// `/control` bodies this page's writes send; None = not this page's.
pub(super) fn control(m: &Arc<Mock>, body: &Value, noreadback: bool) -> Option<Response> {
    let p = &body["params"];
    if body["action"] != "vendor.call"
        || p["object"] != "zwrt_router.api"
        || p["method"] != "router_set_alg_switch"
    {
        return None;
    }
    m.page_ops.lock().unwrap().push(body.clone());
    let sets: Vec<(&str, bool)> = ARGS
        .iter()
        .filter_map(|(arg, key)| match p["args"][*arg].as_str() {
            Some("1") => Some((*key, true)),
            Some("0") => Some((*key, false)),
            _ => None,
        })
        .collect();
    if sets.is_empty() {
        let e = json!({"ok":false,"action":"vendor.call","error":{"code":"invalid_parameter","message":"args"}});
        return Some((StatusCode::BAD_REQUEST, axum::Json(e)).into_response());
    }
    if !noreadback {
        let m = m.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1200)).await;
            let mut a = cur_alg(&m);
            if let Some(o) = a.as_object_mut() {
                for (k, v) in sets {
                    // B31: a field the firmware doesn't report stays unreported
                    if o.get(k).is_some_and(|x| !x.is_null()) {
                        o.insert(k.into(), json!(v));
                    }
                }
                set_field(&m, "router", "alg", a);
            }
        });
    }
    Some(axum::Json(json!({"ok":true,"action":"vendor.call","result":{"result":"success"}})).into_response())
}

/// Publish this page's default data before anyone connects.
pub(super) fn seed(m: &Arc<Mock>) {
    set_field(m, "router", "alg", b31());
}

#[derive(serde::Deserialize)]
struct Set {
    #[serde(default)]
    reported: bool,
    #[serde(default)]
    failed: bool,
}

async fn set(State(m): State<Arc<Mock>>, Query(q): Query<Set>) -> StatusCode {
    let v = if q.failed {
        Value::Null
    } else if q.reported {
        json!({"sip": true, "l2tp": false, "pptp": false, "ipsec": false})
    } else {
        b31()
    };
    set_field(&m, "router", "alg", v);
    StatusCode::NO_CONTENT
}

/// This page's `/mock/*` test hooks.
pub(super) fn routes(r: Router<Arc<Mock>>) -> Router<Arc<Mock>> {
    r.route("/mock/router-vpn", post(set))
}
