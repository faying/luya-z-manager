//! The core's `power.tailnet_poller` in the mock u60d (core poller.rs): `GET /v2/api/poller` and
//! the `poller.allow` / `poller.unallow` / `poller.ack` writes, kept in memory.
//!
//! `POST /mock/poller` (JSON): `{"reset": true}` (no event, empty list); `{"event": true}` (one
//! active event: 192.168.50.88 → :80 through the home gateway, 12 minutes, 550 packets a minute) and
//! its alert (seq 3, kind `tailnet-poller`, on `/v2/api/alerts` beside the two fake ones);
//! `POST /v2/api/alerts/read {"seq"}` moves that alert's read cursor (`GET /mock/poller` shows it).

use std::sync::{Arc, Mutex, OnceLock};

use axum::Router;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Value, json};

use super::Mock;

#[derive(Default)]
struct St {
    events: Vec<Value>,
    allow: Vec<Value>,
    alert: bool,
    read: u64,
}

/// The poller alert into the mock's alert summary (`mock.rs` `screen_alerts`, `screen_status`).
pub(crate) fn alerts_extra(d: &mut Value) {
    let s = st().lock().unwrap();
    if !s.alert {
        return;
    }
    let unread = s.read < 3;
    if let Some(evs) = d["events"].as_array_mut() {
        evs.insert(0, json!({"seq": 3, "time": null, "uptime": 300, "kind": "tailnet-poller",
            "label": "有设备经 Tailscale 持续访问管理接口，息屏更耗电",
            "label_en": "A device keeps polling the admin ports over Tailscale (more drain with the screen off)",
            "text": "192.168.50.88（经网关转发）经 Tailscale 持续访问 :80", "unread": unread}));
    }
    if unread {
        let n = d["unread"].as_u64().unwrap_or(0);
        d["unread"] = json!(n + 1);
    }
}

pub(crate) fn status_extra(d: &mut Value) {
    let s = st().lock().unwrap();
    if s.alert && s.read < 3 {
        let n = d["alerts"]["unread"].as_u64().unwrap_or(0);
        d["alerts"]["unread"] = json!(n + 1);
    }
}

async fn read_alert(body: axum::body::Bytes) -> Response {
    // with /mock/alerts-to-agent on, the list comes from the fake agent: so does marking it read,
    // or the page's read-back would look at a cursor this write never moved
    if super::ALERTS_TO_AGENT.load(std::sync::atomic::Ordering::SeqCst) {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(json!({"ok": false, "error": "not_in_compat_yet"})),
        )
            .into_response();
    }
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let mut s = st().lock().unwrap();
    if let Some(q) = v["seq"].as_u64() {
        s.read = s.read.max(q);
    }
    axum::Json(json!({"ok": true, "data": {"read": s.read}})).into_response()
}

async fn hooks() -> Response {
    let s = st().lock().unwrap();
    axum::Json(json!({"alert": s.alert, "read": s.read})).into_response()
}

fn st() -> &'static Mutex<St> {
    static S: OnceLock<Mutex<St>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(St::default()))
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

pub(super) fn control(_m: &Arc<Mock>, body: &Value, _noreadback: bool) -> Option<Response> {
    let action = body["action"].as_str()?;
    if !action.starts_with("poller.") {
        return None;
    }
    let p = &body["params"];
    let mut s = st().lock().unwrap();
    let ok = |r: Value| {
        Some(axum::Json(json!({"ok": true, "action": action, "result": r})).into_response())
    };
    let (peer, port) = (
        p["peer"].as_str().unwrap_or(""),
        p["port"].as_u64().unwrap_or(0),
    );
    match action {
        "poller.allow" => {
            let until = if p["forever"] == true {
                Value::Null
            } else {
                json!(now() + 86_400)
            };
            s.allow
                .retain(|a| !(a["peer"] == peer && a["port"] == port));
            s.allow.push(json!({"peer": peer, "port": port, "label": p["label"].as_str().unwrap_or(""), "until": until}));
            for e in s
                .events
                .iter_mut()
                .filter(|e| e["peer"] == peer && e["port"] == port)
            {
                e["allowed"] = json!(true);
            }
            ok(json!({"peer": peer, "port": port, "until": until}))
        }
        "poller.unallow" => {
            let n = s.allow.len();
            s.allow
                .retain(|a| !(a["peer"] == peer && a["port"] == port));
            let removed = s.allow.len() != n;
            ok(json!({"peer": peer, "port": port, "removed": removed}))
        }
        "poller.ack" => {
            let id = p["id"].as_str().unwrap_or("");
            match s.events.iter_mut().find(|e| e["id"] == id) {
                Some(e) => {
                    e["acked"] = json!(true);
                    ok(json!({"id": id, "acked": true}))
                }
                None => Some(
                    (
                        StatusCode::BAD_REQUEST,
                        axum::Json(json!({"ok": false, "action": action, "error": {"code": "invalid_parameter", "message": "no such event"}})),
                    )
                        .into_response(),
                ),
            }
        }
        _ => None,
    }
}

async fn view() -> Response {
    let s = st().lock().unwrap();
    axum::Json(
        json!({"ok": true, "data": {"events": s.events, "allow": s.allow, "ports": [80, 9090],
        "min_pkts": 20, "run_min": 10, "allow_default_s": 86_400}}),
    )
    .into_response()
}

async fn set(body: axum::body::Bytes) -> StatusCode {
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let mut s = st().lock().unwrap();
    if v["reset"] == true {
        *s = St::default();
    }
    if v["event"] == true {
        s.alert = true;
        s.read = 0;
        let since = now() - 12 * 60;
        s.events = vec![
            json!({"id": format!("0123abcd-{}", since * 100), "kind": "power.tailnet_poller",
            "peer": "192.168.50.88", "peer_name": null, "via_gateway": true, "port": 80, "since": since,
            "last": now(), "minutes": 12, "pkts_per_min": 550, "state": "active", "ended_at": null,
            "acked": false, "allowed": false}),
        ];
    }
    StatusCode::NO_CONTENT
}

pub(super) fn seed(_m: &Arc<Mock>) {}

pub(super) fn routes(r: Router<Arc<Mock>>) -> Router<Arc<Mock>> {
    r.route("/mock/poller", post(set).get(hooks))
        .route("/v2/api/alerts/read", post(read_alert))
        .route("/v2/api/poller", get(view))
}
