//! /router/qos: `router.qos` (read-only page, no writes). Default: 极速, xdpi off (the device's
//! state since 09-23). `POST /mock/qos?mode=<vendor mode>&xdpi=on|off|null` changes it
//! (`mode` empty = unread); `?gone=true` makes the part null (the vendor read failed);
//! `?stale=true` republishes the whole `router` block marked stale (`false` clears it).

use std::sync::Arc;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::post;
use serde_json::{Value, json};

use super::{Mock, set_field};

/// `/control` bodies this page's writes send; None = not this page's (it has none).
pub(super) fn control(_m: &Arc<Mock>, _body: &Value, _noreadback: bool) -> Option<Response> {
    None
}

/// Publish this page's default data before anyone connects.
pub(super) fn seed(m: &Arc<Mock>) {
    set_field(
        m,
        "router",
        "qos",
        json!({"mode":"Nomal_mode","xdpi":false}),
    );
}

#[derive(serde::Deserialize)]
struct SetQos {
    mode: Option<String>,
    xdpi: Option<String>,
    gone: Option<bool>,
    stale: Option<bool>,
}

async fn set_qos(State(m): State<Arc<Mock>>, Query(q): Query<SetQos>) -> StatusCode {
    if let Some(s) = q.stale {
        let b = m.blocks.lock().unwrap().get("router").cloned();
        if let Some(b) = b {
            let at = b["observed_at"].as_u64().unwrap_or(0) + 1;
            m.publish("router", b["data"].clone(), at, s);
        }
        return StatusCode::NO_CONTENT;
    }
    let v = if q.gone == Some(true) {
        Value::Null
    } else {
        json!({
            "mode": q.mode.unwrap_or_else(|| "Nomal_mode".into()),
            "xdpi": match q.xdpi.as_deref() {
                Some("on") => json!(true),
                Some("null") => Value::Null,
                _ => json!(false),
            },
        })
    };
    set_field(&m, "router", "qos", v);
    StatusCode::NO_CONTENT
}

/// This page's `/mock/*` test hooks.
pub(super) fn routes(r: Router<Arc<Mock>>) -> Router<Arc<Mock>> {
    r.route("/mock/qos", post(set_qos))
}
