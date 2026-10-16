//! / (home, batch 5b) in the mock u60d: the `clients` block (the recording has none), the made-up
//! parts of u60d's public status the page reads (scenario, verdict; `mock.rs`
//! `screen_status` calls [`public_extra`]) and `/v2/api/screen` (the battery estimate).
//!
//! `POST /mock/home` (JSON, all optional): `{"reset": true}`; `{"verdict": "stall"}` (network.verdict);
//! `{"scenario": "none" | "off" | "pinned"}`;
//! `{"bat": "discharging" | "charging" | "limit" | "estimating" | "gone"}` (what /v2/api/screen says);
//! `{"clients": n}` (republishes the `clients` block).

use std::sync::{Arc, Mutex, OnceLock};

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Value, json};

use super::Mock;

#[derive(Clone, Default)]
struct Hooks {
    verdict: String,
    scenario: String,
    notice: String,
    bat: String,
}

fn hooks() -> &'static Mutex<Hooks> {
    static H: OnceLock<Mutex<Hooks>> = OnceLock::new();
    H.get_or_init(|| Mutex::new(Hooks::default()))
}

pub(super) fn control(_m: &Arc<Mock>, _body: &Value, _noreadback: bool) -> Option<Response> {
    None
}

fn publish_clients(m: &Mock, n: i64) {
    let list: Vec<Value> = (0..n)
        .map(|i| json!({"ip": format!("192.168.0.{}", 10 + i), "mac": format!("02:00:00:00:00:{i:02x}"), "name": format!("phone-{i}")}))
        .collect();
    m.publish(
        "clients",
        json!({"lan": 0, "wifi": n, "total": n, "list": list}),
        0,
        false,
    );
}

pub(super) fn seed(m: &Arc<Mock>) {
    if !m.blocks.lock().unwrap().contains_key("clients") {
        publish_clients(m, 3);
    }
}

/// The fields u60d's public status has besides what `screen_status` already says.
pub(crate) fn public_extra(d: &mut Value) {
    let h = hooks().lock().unwrap().clone();
    d["network"] = json!({"verdict": if h.verdict.is_empty() { "ok" } else { &h.verdict }});
    d["scenario"] = match h.scenario.as_str() {
        "none" => json!({"configured": false, "enabled": true, "current": null, "pin": null}),
        s => json!({"configured": true, "enabled": s != "off", "current": "abroad", "name": "国外",
                    "name_en": "Abroad", "pin": if s == "pinned" { json!("abroad") } else { Value::Null },
                    "last_switch": 1_760_000_000_u64 + 3600 * 14 + 60 * 25}),
    };
    d["services"]["tailscale"] = json!({"installed": true, "running": true});
}

async fn screen(State(_m): State<Arc<Mock>>) -> Response {
    let bat = hooks().lock().unwrap().bat.clone();
    let b = |state: &str, kind: &str, minutes: Value, target: i64| {
        json!({"state": state, "kind": kind, "minutes": minutes, "target_pct": target, "observed_at": 0, "samples": 12})
    };
    let battery = match bat.as_str() {
        "charging" => b("ok", "charging_eta", json!(95), 100),
        "limit" => b("ok", "charging_eta", json!(40), 80),
        "estimating" => b("estimating", "unknown", Value::Null, 100),
        "gone" => b("unavailable", "unknown", Value::Null, 100),
        _ => b("ok", "discharging_eta", json!(272), 100),
    };
    axum::Json(json!({"ok": true, "data": {"v": 1, "battery": battery}})).into_response()
}

async fn set(State(m): State<Arc<Mock>>, body: axum::body::Bytes) -> StatusCode {
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let mut h = hooks().lock().unwrap();
    if v["reset"].as_bool() == Some(true) {
        *h = Hooks::default();
    }
    if let Some(s) = v["verdict"].as_str() {
        h.verdict = s.into();
    }
    if let Some(s) = v["scenario"].as_str() {
        h.scenario = s.into();
    }
    if v.get("notice").is_some() {
        h.notice = v["notice"].as_str().unwrap_or_default().into();
    }
    if let Some(s) = v["bat"].as_str() {
        h.bat = s.into();
    }
    drop(h);
    if let Some(n) = v["clients"].as_i64() {
        publish_clients(&m, n.clamp(0, 30));
    }
    StatusCode::NO_CONTENT
}

pub(super) fn routes(r: Router<Arc<Mock>>) -> Router<Arc<Mock>> {
    r.route("/mock/home", post(set))
        .route("/v2/api/screen", get(screen))
}
