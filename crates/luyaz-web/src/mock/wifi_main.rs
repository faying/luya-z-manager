//! /router/wifi's part of the mock u60d: the `wifi` block's `on`, `wifi6`, `country`, `band_2g`,
//! `band_5g` (each its own `set_field`, so the guest page's `guest` key stays), and the page's
//! writes: `wifi.settings` as a transaction on item `wifi.main` (op block active → last, like the
//! mock's network mode / band / APN ones), `wifi.power_save` and `nfc.set`.
//!
//! `POST /mock/wifi` (JSON, all optional): `{"reset": true}` puts the defaults back;
//! `{"next": "lock"}` makes the next `wifi.settings` answer 503 busy (the Wi-Fi lock), `"doing"`
//! 409 busy with `doing`, `"refused"` 409 refused; `{"end": "rolled_back"}` ends the following
//! transactions rolled back (`"confirmed"` again by default); `{"ms": n}` stretches a transaction
//! to about n ms; `{"set": {...}}` merges keys into the block (`band_2g` etc. as whole objects);
//! `{"gone": true}` makes the bands unreadable; `{"stale": true|false}` republishes `wifi` marked so.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};

use super::{Mock, active_op, set_field};

const KEYS: [&str; 5] = ["on", "wifi6", "country", "band_2g", "band_5g"];

#[derive(Default)]
struct Hooks {
    next: String,
    end: String,
    ms: u64,
}

fn hooks() -> &'static Mutex<Hooks> {
    static H: OnceLock<Mutex<Hooks>> = OnceLock::new();
    H.get_or_init(|| Mutex::new(Hooks::default()))
}

fn defaults() -> Value {
    json!({
        "on": true,
        "wifi6": true,
        "country": "CN",
        "band_2g": {"ap_on": true, "radio_on": true, "ssid": "U60-Home", "encryption": "psk2+ccmp", "has_password": true,
                    "hidden": false, "channel": "auto", "txpower": "100", "htmode": "HT20"},
        "band_5g": {"ap_on": true, "radio_on": true, "ssid": "U60-Home-5G", "encryption": "psk2+psk3+ccmp", "has_password": true,
                    "hidden": false, "channel": "149", "txpower": "100", "htmode": "VHT80"},
    })
}

fn current(m: &Mock) -> Value {
    m.blocks
        .lock()
        .unwrap()
        .get("wifi")
        .map_or(Value::Null, |b| b["data"].clone())
}

fn publish_keys(m: &Mock, v: &Value) {
    for k in KEYS {
        if let Some(x) = v.get(k) {
            set_field(m, "wifi", k, x.clone());
        }
    }
}

/// Publish this page's default data before anyone connects.
pub(super) fn seed(m: &Arc<Mock>) {
    publish_keys(m, &defaults());
    if !m.blocks.lock().unwrap().contains_key("nfc") {
        m.publish("nfc", json!({"switch": 1}), 0, false);
    }
}

fn reply_err(status: StatusCode, action: &str, code: &str, msg: &str, extra: Option<(&str, Value)>) -> Response {
    let mut e = json!({"ok": false, "action": action, "error": {"code": code, "message": msg}});
    if let Some((k, v)) = extra {
        e[k] = v;
    }
    (status, axum::Json(e)).into_response()
}

/// The block after the agent's write of `p` (keys of PUT /api/wifi/settings).
fn applied(cur: &Value, p: &Value) -> Value {
    let mut w = cur.clone();
    let Some(p) = p.as_object() else { return w };
    for (k, v) in p {
        let s = v.as_str().unwrap_or_default();
        let band = if k.ends_with("2g") || k == "radio2_disabled" { "band_2g" } else { "band_5g" };
        match k.split('_').next().unwrap_or_default() {
            "country" => w["country"] = json!(s.to_ascii_uppercase()),
            "radio2" | "radio5" => w[band]["radio_on"] = json!(s == "0"),
            "ssid" => w[band]["ssid"] = json!(s),
            "key" => w[band]["has_password"] = json!(!s.is_empty()),
            "encryption" => w[band]["encryption"] = json!(s),
            "channel" => w[band]["channel"] = json!(s),
            "htmode" => w[band]["htmode"] = json!(s),
            "txpower" => w[band]["txpower"] = json!(s),
            "hidden" => w[band]["hidden"] = json!(s == "1"),
            _ => {}
        }
    }
    // the master switch last, as the agent writes it
    if let Some(on) = p.get("wifi_onoff").and_then(Value::as_str) {
        let on = on != "0";
        w["on"] = json!(on);
        for b in ["band_2g", "band_5g"] {
            w[b]["ap_on"] = json!(on);
            if on {
                w[b]["radio_on"] = json!(true);
            }
        }
    }
    w
}

fn publish_op(m: &Mock, active: Value, last: Value) {
    let cur = m.blocks.lock().unwrap().get("op").cloned();
    let (mut data, at) = match cur {
        Some(b) => (b["data"].clone(), b["observed_at"].as_u64().unwrap_or(0)),
        None => (json!({"rollback_enabled": true}), 0),
    };
    if !data.is_object() {
        data = json!({"rollback_enabled": true});
    }
    data["active"] = active;
    if !last.is_null() {
        data["last"] = last;
    }
    m.publish("op", data, at + 1, false);
}

fn settings(m: &Arc<Mock>, body: &Value) -> Response {
    const A: &str = "wifi.settings";
    m.page_ops.lock().unwrap().push(body.clone());
    let p = &body["params"];
    let next = std::mem::take(&mut hooks().lock().unwrap().next);
    match next.as_str() {
        "lock" => {
            return reply_err(StatusCode::SERVICE_UNAVAILABLE, A, "busy", "Wi-Fi is being changed by another writer; try again", None);
        }
        "doing" => {
            let doing = json!({"op_id": "mock-guard", "action": "wifi.guest", "source": "guard", "phase": "verifying",
                "age_ms": 12000, "say_zh": "正在改访客 Wi-Fi（看门狗发起，12 秒），稍等", "say_en": "Busy: guest wi-fi (watchdog)"});
            return reply_err(StatusCode::CONFLICT, A, "busy", "another change is in progress", Some(("doing", doing)));
        }
        "refused" => {
            return reply_err(StatusCode::CONFLICT, A, "refused", "the old Wi-Fi password was not kept on the device", None);
        }
        _ => {}
    }
    if !p.is_object() || body["source"].as_str().is_none() {
        return reply_err(StatusCode::BAD_REQUEST, A, "invalid_parameter", "params must be an object; source required", None);
    }
    if let Some(a) = active_op(m) {
        let doing = json!({"op_id": a["op_id"], "item": a["item"], "phase": a["phase"],
            "say_zh": "正在改另一项设置（网页发起），稍等", "say_en": "Busy: another change (web)"});
        return reply_err(StatusCode::CONFLICT, A, "busy", "another change is in progress", Some(("doing", doing)));
    }
    let cur = current(m);
    if let Some(w6) = p.get("wifi6_switch").and_then(Value::as_str)
        && (w6 != "0") != cur["wifi6"].as_bool().unwrap_or(true)
    {
        return reply_err(StatusCode::CONFLICT, A, "refused",
            "this firmware changes Wi-Fi 6 together with the radio mode; switching it here is not supported yet", None);
    }
    for k in ["key_2g", "key_5g"] {
        if let Some(s) = p[k].as_str()
            && !s.is_empty()
            && !(8..=64).contains(&s.chars().count())
        {
            return reply_err(StatusCode::BAD_REQUEST, A, "invalid_parameter", &format!("{k} must be 8-64 characters"), None);
        }
    }
    let want = applied(&cur, p);
    if want == cur {
        return axum::Json(json!({"ok": true, "action": A, "result": {"status": "ok", "note": "no changes"}})).into_response();
    }
    let hot = p.as_object().is_some_and(|o| o.keys().all(|k| k.starts_with("txpower_")));
    let id = body["op_id"].as_str().unwrap_or("mock-wifi").to_owned();
    let (end, ms) = {
        let h = hooks().lock().unwrap();
        (h.end.clone(), if h.ms > 0 { h.ms } else if hot { 400 } else { 2500 })
    };
    let base = json!({"op_id": id, "action": A, "item": "wifi.main", "source": "web", "undo": false,
        "source_zh": "网页", "source_en": "Web", "what_zh": "Wi-Fi 设置", "what_en": "Wi-Fi settings",
        "reason": null, "can_revert": false, "can_keep": false, "steps": []});
    let with = |o: Value| {
        let mut v = base.clone();
        for (k, x) in o.as_object().unwrap() {
            v[k] = x.clone();
        }
        v
    };
    let applying = with(json!({"phase": "applying", "say_zh": "正在应用 Wi-Fi 设置", "say_en": "Applying Wi-Fi settings",
        "mark": null, "stay": "live", "next_zh": null, "next_en": null, "remaining_ms": null}));
    let verifying = with(json!({"phase": "verifying", "say_zh": "正在确认 Wi-Fi 起来了", "say_en": "Checking Wi-Fi is up",
        "mark": null, "stay": "live", "next_zh": "{t} 后没起来就退回", "next_en": "Back in {t} if it doesn't come up",
        "remaining_ms": 90000}));
    let receipt = body["receipt"].as_bool() == Some(true);
    let ok = end != "rolled_back";
    let mut waiting = verifying.clone();
    waiting["receipt"] = super::receipt::view(super::receipt::wait_ms());
    let mut last = with(if ok {
        json!({"phase": "confirmed", "reason": "verified", "mark": "ok", "stay": "brief", "acked": false, "needs_ack": false,
               "say_zh": "Wi-Fi 设置已生效", "say_en": "Wi-Fi settings in effect"})
    } else {
        json!({"phase": "rolled_back", "reason": "timeout", "mark": "warn", "stay": "sticky", "acked": false, "needs_ack": true,
               "say_zh": "Wi-Fi 没起来 · 已退回原来的设置", "say_en": "Wi-Fi didn't come up · settings put back"})
    });
    publish_op(m, applying, Value::Null);
    let m2 = m.clone();
    let id2 = id.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(ms / 3)).await;
        if ok {
            publish_keys(&m2, &want);
        }
        publish_op(&m2, verifying, Value::Null);
        tokio::time::sleep(Duration::from_millis(ms - ms / 3)).await;
        // O3: a remote change also waits for the page's receipt (receipt.rs); none: put back
        if ok && receipt {
            publish_op(&m2, waiting, Value::Null);
            if super::receipt::wait(&id2).await {
                last["reason"] = json!("receipt");
            } else {
                publish_keys(&m2, &cur);
                last = super::no_receipt(last, ("原来的设置", "the old settings"));
            }
        }
        publish_op(&m2, Value::Null, last);
    });
    let mut result = json!({"status": "ok"});
    if hot {
        result["hot"] = json!(true);
    }
    axum::Json(json!({"ok": true, "action": A, "result": result, "op": {"op_id": id, "phase": "applying"}})).into_response()
}

/// `/control` bodies this page's writes send; None = not this page's.
pub(super) fn control(m: &Arc<Mock>, body: &Value, noreadback: bool) -> Option<Response> {
    let action = body["action"].as_str()?;
    match action {
        "wifi.settings" => Some(settings(m, body)),
        "wifi.power_save" => {
            m.page_ops.lock().unwrap().push(body.clone());
            let Some(on) = body["params"]["enabled"].as_bool() else {
                return Some(reply_err(StatusCode::BAD_REQUEST, action, "invalid_parameter", "enabled must be a boolean", None));
            };
            Some(axum::Json(json!({"ok": true, "action": action, "result": {"enabled": on, "applied": 4}})).into_response())
        }
        "nfc.set" => {
            m.page_ops.lock().unwrap().push(body.clone());
            let Some(on) = body["params"]["enabled"].as_i64() else {
                return Some(reply_err(StatusCode::BAD_REQUEST, action, "invalid_parameter", "enabled", None));
            };
            if !noreadback {
                let m2 = m.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(800)).await;
                    let at = m2.blocks.lock().unwrap().get("nfc").and_then(|b| b["observed_at"].as_u64()).unwrap_or(0);
                    m2.publish("nfc", json!({"switch": on}), at + 1, false);
                });
            }
            Some(axum::Json(json!({"ok": true, "action": action, "result": {"result": "success"}})).into_response())
        }
        _ => None,
    }
}

async fn set_wifi(State(m): State<Arc<Mock>>, body: Bytes) -> StatusCode {
    let q: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    if q["reset"].as_bool() == Some(true) {
        *hooks().lock().unwrap() = Hooks::default();
        publish_keys(&m, &defaults());
    }
    {
        let mut h = hooks().lock().unwrap();
        if let Some(n) = q["next"].as_str() {
            h.next = n.into();
        }
        if let Some(e) = q["end"].as_str() {
            h.end = e.into();
        }
        if let Some(ms) = q["ms"].as_u64() {
            h.ms = ms;
        }
    }
    if q["set"].is_object() {
        publish_keys(&m, &q["set"]);
    }
    if q["gone"].as_bool() == Some(true) {
        set_field(&m, "wifi", "band_2g", Value::Null);
    }
    if let Some(s) = q["stale"].as_bool() {
        let b = m.blocks.lock().unwrap().get("wifi").cloned();
        if let Some(b) = b {
            let at = b["observed_at"].as_u64().unwrap_or(0) + 1;
            m.publish("wifi", b["data"].clone(), at, s);
        }
    }
    StatusCode::NO_CONTENT
}

/// This page's `/mock/*` test hooks.
pub(super) fn routes(r: Router<Arc<Mock>>) -> Router<Arc<Mock>> {
    r.route("/mock/wifi", post(set_wifi))
}
