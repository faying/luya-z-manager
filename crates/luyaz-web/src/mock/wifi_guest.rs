//! /router/wifi-guest's part of the mock u60d: the `guest` key of the `wifi` block (and `on` when
//! nobody published it) and the `wifi.guest` transaction (`/mock/txn?end=` picks how it ends).
//! `POST /mock/wguest` (JSON) takes `{"guest": {…}}` (merged into the guest part), `{"on": bool}`
//! and `{"next": "busy503" | "refused" | "bad"}` (the next `wifi.guest` reply); no body puts the
//! default guest part back.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Map, Value, json};

use super::{Mock, Txn, active_op, run_txn, set_field};

/// The next `wifi.guest` reply, forced by `/mock/wguest {"next": …}`.
fn next() -> &'static Mutex<Option<String>> {
    static N: std::sync::OnceLock<Mutex<Option<String>>> = std::sync::OnceLock::new();
    N.get_or_init(|| Mutex::new(None))
}

fn default_guest() -> Value {
    json!({"ssid": "U60-Guest", "encryption": "psk2+ccmp", "has_password": true, "on_2g": true, "on_5g": false,
           "hidden": false, "isolate": true, "active_time": 240, "left_s": 7385})
}

fn wifi(m: &Mock) -> Value {
    m.blocks
        .lock()
        .unwrap()
        .get("wifi")
        .map_or(Value::Null, |b| b["data"].clone())
}

/// The guest part as u60d would read it back after `params` (agent body keys).
fn apply(g: &mut Value, p: &Map<String, Value>) {
    for (k, v) in p {
        let s = v.as_str().unwrap_or("");
        match k.as_str() {
            "guest_ssid" => g["ssid"] = json!(s),
            "guest_key" => g["has_password"] = json!(!s.is_empty()),
            "guest_encryption" => g["encryption"] = json!(s),
            "guest_disabled_2g" => g["on_2g"] = json!(s != "1"),
            "guest_disabled_5g" => g["on_5g"] = json!(s != "1"),
            "guest_hidden" => g["hidden"] = json!(s == "1"),
            "guest_isolate" => g["isolate"] = json!(s != "0"),
            "guest_active_time" => g["active_time"] = json!(s.parse::<i64>().unwrap_or(0)),
            _ => {}
        }
    }
    let ssid_ok = g["ssid"].as_str().is_some_and(|s| !s.is_empty());
    for b in ["on_2g", "on_5g"] {
        if !ssid_ok {
            g[b] = json!(false);
        }
    }
    let on = g["on_2g"] == json!(true) || g["on_5g"] == json!(true);
    g["left_s"] = match g["active_time"].as_i64() {
        Some(n) if on && n > 0 => json!(n * 60),
        _ => Value::Null,
    };
}

fn words(p: &Map<String, Value>) -> (String, String) {
    let keys: Vec<&str> = p
        .keys()
        .filter(|k| *k != "guest_key")
        .map(|k| k.trim_start_matches("guest_"))
        .collect();
    let mut s = keys.join(", ");
    if p.contains_key("guest_key") {
        s.push_str(if s.is_empty() { "key" } else { ", key" });
    }
    (s.clone(), s)
}

/// `/control` bodies this page's writes send; None = not this page's.
pub(super) fn control(m: &Arc<Mock>, body: &Value, noreadback: bool) -> Option<Response> {
    if body["action"] != "wifi.guest" {
        return None;
    }
    m.page_ops.lock().unwrap().push(body.clone());
    let err = |status: StatusCode, code: &str, msg: &str, doing: Option<Value>| {
        let mut e =
            json!({"ok": false, "action": "wifi.guest", "error": {"code": code, "message": msg}});
        if let Some(d) = doing {
            e["doing"] = d;
        }
        Some((status, axum::Json(e)).into_response())
    };
    if body["source"].as_str().is_none() {
        return err(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "source required",
            None,
        );
    }
    match next().lock().unwrap().take().as_deref() {
        Some("busy503") => {
            return err(
                StatusCode::SERVICE_UNAVAILABLE,
                "busy",
                "Wi-Fi is being changed by another writer; try again",
                None,
            );
        }
        Some("refused") => {
            return err(
                StatusCode::CONFLICT,
                "refused",
                "wifi.guest refused by the mock",
                None,
            );
        }
        Some("bad") => {
            return err(
                StatusCode::BAD_REQUEST,
                "invalid_parameter",
                "params: expected JSON object",
                None,
            );
        }
        _ => {}
    }
    if let Some(mut doing) = active_op(m) {
        let a = m
            .blocks
            .lock()
            .unwrap()
            .get("op")
            .map_or(Value::Null, |b| b["data"]["active"].clone());
        doing["say_zh"] = a["say_zh"].clone();
        doing["say_en"] = a["say_en"].clone();
        doing["action"] = a["action"].clone();
        return err(
            StatusCode::CONFLICT,
            "busy",
            "another change is in progress",
            Some(doing),
        );
    }
    let Some(p) = body["params"].as_object().cloned() else {
        return err(
            StatusCode::BAD_REQUEST,
            "invalid_parameter",
            "params: expected JSON object",
            None,
        );
    };
    let old = wifi(m)["guest"].clone();
    let mut new = old.clone();
    apply(&mut new, &p);
    // u60d diffs against the wireless config: a password is always a change (only its fingerprint shows)
    if new == old && !p.contains_key("guest_key") {
        return Some(axum::Json(json!({"ok": true, "action": "wifi.guest", "result": {"status": "ok", "note": "no changes"}})).into_response());
    }
    if noreadback {
        return Some(
            axum::Json(
                json!({"ok": true, "action": "wifi.guest", "result": {"result": "success"}}),
            )
            .into_response(),
        );
    }
    let target = Value::Object(p.clone());
    let (tw, ow) = (words(&p), ("before".to_owned(), "before".to_owned()));
    let request = json!({"action": "wifi.guest", "undo": true, "params": {}});
    let t = Txn {
        item: "wifi.guest",
        what: ("访客 Wi-Fi", "Guest Wi-Fi"),
        target,
        old: old.clone(),
        words: (tw, ow),
        first: "正在改访客 Wi-Fi",
        request,
    };
    Some(run_txn(
        m,
        body,
        t,
        Box::new(move |m: &Mock| set_field(m, "wifi", "guest", new)),
    ))
}

/// Publish this page's default data before anyone connects: only the keys not there yet.
pub(super) fn seed(m: &Arc<Mock>) {
    let w = wifi(m);
    if !w["guest"].is_object() {
        set_field(m, "wifi", "guest", default_guest());
    }
    if !wifi(m)["on"].is_boolean() {
        set_field(m, "wifi", "on", json!(true));
    }
}

async fn hook(State(m): State<Arc<Mock>>, body: Bytes) -> StatusCode {
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if !v.is_object() {
        *next().lock().unwrap() = None;
        set_field(&m, "wifi", "guest", default_guest());
        set_field(&m, "wifi", "on", json!(true));
        return StatusCode::NO_CONTENT;
    }
    if let Some(n) = v["next"].as_str() {
        *next().lock().unwrap() = Some(n.into());
    }
    if let Some(on) = v["on"].as_bool() {
        set_field(&m, "wifi", "on", json!(on));
    }
    if let Some(g) = v["guest"].as_object() {
        let mut cur = wifi(&m)["guest"].clone();
        if !cur.is_object() {
            cur = default_guest();
        }
        for (k, x) in g {
            cur[k] = x.clone();
        }
        set_field(&m, "wifi", "guest", cur);
    }
    StatusCode::NO_CONTENT
}

/// This page's `/mock/*` test hooks.
pub(super) fn routes(r: Router<Arc<Mock>>) -> Router<Arc<Mock>> {
    r.route("/mock/wguest", post(hook))
}
