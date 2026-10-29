//! The core's `plugin.settings.get` / `plugin.settings.set` in the mock u60d (core plug/settings.rs):
//! the same reply shapes and error codes, a made-up plugin kept in memory. Its text carries
//! markup and Datastar-looking strings on purpose: the page must show them as plain text.
//!
//! `POST /mock/plugin-settings` (JSON, any of):
//! - `{"reset": true}`: back to the start (not installed);
//! - `{"state": "none" | "nosettings" | "online" | "offline" | "offline_empty" | "invalid"}`;
//! - `{"next": "stale" | "unknown" | "unknown_applied"}`: how the next write answers
//!   (`stale_revision`; `result_unknown` with the value unchanged / changed underneath);
//! - `{"bump": true}`: someone else changed `beta` (rev + 1).
//!
//! `GET /mock/plugin-settings`: `{rev, rows, writes}` (each write's params as sent).

use std::sync::{Arc, Mutex, OnceLock};

use axum::Router;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};

use super::Mock;

struct St {
    state: String,
    next: String,
    rev: u64,
    alpha: bool,
    beta: bool,
    writes: Vec<Value>,
}

impl Default for St {
    fn default() -> Self {
        St { state: "none".into(), next: String::new(), rev: 1, alpha: true, beta: false, writes: Vec::new() }
    }
}

fn st() -> &'static Mutex<St> {
    static S: OnceLock<Mutex<St>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(St::default()))
}

fn descriptor() -> Value {
    json!({"title": "<b>Demo</b> 插件", "groups": [
        {"key": "main", "title": "基本", "rows": [
            {"key": "alpha", "kind": "toggle", "label": "<script>window.__pwn=1</script>甲",
             "note": "$pwn = 1; @post('/x')", "confirm": "<img src=x onerror=window.__pwn=1> data-on:click=\"@post('/x')\" 确定关掉甲？"},
            {"key": "beta", "kind": "toggle", "label": "乙", "note": "直接生效"},
            {"key": "more", "kind": "page", "label": "更多", "page": "more"},
            {"key": "state", "kind": "status", "label": "状态"}]},
        {"key": "more", "title": "更多", "sub": true, "rows": [
            {"key": "more.wait", "kind": "value", "label": "等待时间"}]}]})
}

fn values(s: &St) -> Value {
    let mut rows = json!({"alpha": {"value": s.alpha}, "beta": {"value": s.beta},
        "state": {"value": "正常 ${x}"}, "more.wait": {"value": "25 秒"}});
    if !s.alpha {
        rows["beta"] = json!({"value": s.beta, "disabled": true, "note": "甲关着时乙不能改"});
    }
    json!({"rev": s.rev, "rows": rows})
}

fn ok(action: &str, r: Value) -> Response {
    axum::Json(json!({"ok": true, "action": action, "result": r})).into_response()
}

fn err(action: &str, status: StatusCode, code: &str) -> Response {
    (status, axum::Json(json!({"ok": false, "action": action, "error": {"code": code, "message": code}}))).into_response()
}

/// The cached view a plugin that went away leaves behind (confirmed 10-28 09:30 device time).
fn view(s: &St, online: bool) -> Value {
    let mut v = json!({"installed": true, "online": online, "descriptor": descriptor(), "confirmed_at": 1_793_179_800});
    let vals = values(s);
    v["rev"] = vals["rev"].clone();
    v["rows"] = vals["rows"].clone();
    v
}

pub(super) fn control(_m: &Arc<Mock>, body: &Value, _noreadback: bool) -> Option<Response> {
    let action = body["action"].as_str()?;
    if !action.starts_with("plugin.settings.") {
        return None;
    }
    let mut s = st().lock().unwrap();
    if action == "plugin.settings.get" {
        return Some(match s.state.as_str() {
            "none" => ok(action, json!({"installed": false})),
            "nosettings" => ok(action, json!({"installed": true, "online": true, "settings": false})),
            "invalid" => err(action, StatusCode::BAD_GATEWAY, "plugin_invalid"),
            "offline_empty" => ok(action, json!({"installed": true, "online": false})),
            "offline" => ok(action, view(&s, false)),
            _ => ok(action, view(&s, true)),
        });
    }
    if action != "plugin.settings.set" {
        return Some(err(action, StatusCode::NOT_FOUND, "unknown_action"));
    }
    let p = &body["params"];
    s.writes.push(p.clone());
    match s.state.as_str() {
        "online" => {}
        "invalid" => return Some(err(action, StatusCode::BAD_GATEWAY, "plugin_invalid")),
        "nosettings" | "none" => return Some(err(action, StatusCode::NOT_FOUND, "no_settings")),
        _ => return Some(err(action, StatusCode::SERVICE_UNAVAILABLE, "plugin_offline")),
    }
    let (Some(key), Some(to), Some(rev)) = (p["key"].as_str(), p["value"].as_bool(), p["rev"].as_u64()) else {
        return Some(err(action, StatusCode::BAD_REQUEST, "invalid_parameter"));
    };
    let next = std::mem::take(&mut s.next);
    if next == "stale" || rev != s.rev {
        return Some(err(action, StatusCode::CONFLICT, "stale_revision"));
    }
    match key {
        "alpha" => {}
        "beta" if s.alpha => {}
        "beta" => return Some(err(action, StatusCode::BAD_GATEWAY, "plugin_refused")),
        _ => return Some(err(action, StatusCode::BAD_REQUEST, "invalid_parameter")),
    }
    let apply = |s: &mut St| {
        if key == "alpha" {
            s.alpha = to;
        } else {
            s.beta = to;
        }
        s.rev += 1;
    };
    match next.as_str() {
        "unknown" => Some(err(action, StatusCode::GATEWAY_TIMEOUT, "result_unknown")),
        "unknown_applied" => {
            apply(&mut s);
            Some(err(action, StatusCode::GATEWAY_TIMEOUT, "result_unknown"))
        }
        _ => {
            apply(&mut s);
            Some(ok(action, values(&s)))
        }
    }
}

async fn hook(body: axum::body::Bytes) -> StatusCode {
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let mut s = st().lock().unwrap();
    if v["reset"] == true {
        *s = St::default();
    }
    if let Some(x) = v["state"].as_str() {
        s.state = x.into();
    }
    if let Some(x) = v["next"].as_str() {
        s.next = x.into();
    }
    if v["bump"] == true {
        s.beta = !s.beta;
        s.rev += 1;
    }
    StatusCode::NO_CONTENT
}

async fn show() -> Response {
    let s = st().lock().unwrap();
    let vals = values(&s);
    axum::Json(json!({"rev": s.rev, "rows": vals["rows"], "writes": s.writes})).into_response()
}

pub(super) fn seed(_m: &Arc<Mock>) {}

pub(super) fn routes(r: Router<Arc<Mock>>) -> Router<Arc<Mock>> {
    r.route("/mock/plugin-settings", post(hook).get(show))
}
