//! The mock u60d's /router/telemetry part: `router.blocked_domains` and the domain filter's
//! `vendor.call zwrt_router.api router_set_domain_filter {action, domain}`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};

use super::{Mock, set_field};

/// A domain whose writes fail (the partial quick block).
static FAIL_ON: Mutex<Option<String>> = Mutex::new(None);
/// One read-modify-publish at a time (the delayed readbacks of a quick block overlap).
static UPDATE: Mutex<()> = Mutex::new(());

const DEFAULT: [&str; 2] = ["iot.zte.com.cn", "ads.example.com"];

fn is_mine(body: &Value) -> bool {
    body["action"] == "vendor.call"
        && body["params"]["object"] == "zwrt_router.api"
        && body["params"]["method"] == "router_set_domain_filter"
}

fn domains(m: &Mock) -> Vec<String> {
    m.blocks
        .lock()
        .unwrap()
        .get("router")
        .and_then(|b| b["data"]["blocked_domains"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}

/// `/control` bodies this page's writes send; None = not this page's.
pub(super) fn control(m: &Arc<Mock>, body: &Value, noreadback: bool) -> Option<Response> {
    if !is_mine(body) {
        return None;
    }
    m.page_ops.lock().unwrap().push(body.clone());
    let args = &body["params"]["args"];
    let (Some(action), Some(domain)) = (args["action"].as_str(), args["domain"].as_str()) else {
        let e = json!({"ok":false,"action":"vendor.call","error":{"code":"invalid_parameter","message":"args.action, args.domain"}});
        return Some((StatusCode::BAD_REQUEST, axum::Json(e)).into_response());
    };
    if !matches!(action, "add" | "delete") {
        let e = json!({"ok":false,"action":"vendor.call","error":{"code":"invalid_parameter","message":"args.action"}});
        return Some((StatusCode::BAD_REQUEST, axum::Json(e)).into_response());
    }
    if FAIL_ON.lock().unwrap().as_deref() == Some(domain) {
        let e = json!({"ok":false,"action":"vendor.call","error":{"code":"device_call_failed","message":"zwrt_router.api router_set_domain_filter: timeout"}});
        return Some((StatusCode::BAD_GATEWAY, axum::Json(e)).into_response());
    }
    if !noreadback {
        let (m, add, d) = (m.clone(), action == "add", domain.to_owned());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(800)).await;
            let _g = UPDATE.lock().unwrap();
            let mut list = domains(&m);
            list.retain(|x| *x != d);
            if add {
                list.push(d);
            }
            set_field(&m, "router", "blocked_domains", json!(list));
        });
    }
    Some(
        axum::Json(json!({"ok":true,"action":"vendor.call","result":{"result":"success"}}))
            .into_response(),
    )
}

/// Publish this page's default data before anyone connects.
pub(super) fn seed(m: &Arc<Mock>) {
    set_field(m, "router", "blocked_domains", json!(DEFAULT));
}

#[derive(serde::Deserialize, Default)]
struct SetFilter {
    /// Comma list (empty = nothing blocked); `null` = unreadable; absent = the default.
    list: Option<String>,
    /// Writes of this domain fail; empty = none.
    #[serde(default)]
    fail: String,
}

/// `POST /mock/filter?list=a,b|null&fail=d`.
async fn set_filter(State(m): State<Arc<Mock>>, Query(q): Query<SetFilter>) -> StatusCode {
    *FAIL_ON.lock().unwrap() = (!q.fail.is_empty()).then_some(q.fail);
    let v = match q.list.as_deref() {
        None => json!(DEFAULT),
        Some("null") => Value::Null,
        Some("") => json!([]),
        Some(s) => json!(s.split(',').collect::<Vec<_>>()),
    };
    let _g = UPDATE.lock().unwrap();
    set_field(&m, "router", "blocked_domains", v);
    StatusCode::NO_CONTENT
}

/// This page's `/mock/*` test hooks.
pub(super) fn routes(r: Router<Arc<Mock>>) -> Router<Arc<Mock>> {
    r.route("/mock/filter", post(set_filter))
}
