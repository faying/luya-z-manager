//! O3's remote Wi-Fi receipt in the mock u60d. A `wifi.settings` / `wifi.guest` body with
//! `"receipt": true` keeps its transaction verifying with `receipt: {waiting: true, remaining_ms}`
//! until `op.receipt` names it in `params.op_id` (as the core reads it: a top-level `op_id` is
//! refused 409, so is a receipt nobody waits for); then it ends `confirmed` / `receipt`. Without a
//! receipt in time it ends `rolled_back` / `no_receipt` (the core's 60 s; the mock's is shorter).
//!
//! `POST /mock/receipt` (JSON, all optional): `{"lost": true}` drops every receipt as if the page
//! was cut off (answered 409, never counted), `{"ms": n}` waits n ms for one (default 20000),
//! `{"reset": true}` back to the defaults and clears the log. `GET /mock/stats` has `receipts`:
//! every `op.receipt` body with `accepted`.

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};
use tokio::sync::Notify;

use super::Mock;

const DEFAULT_MS: u64 = 20_000;

#[derive(Default)]
struct State {
    /// The op waiting for its receipt.
    waiting: Option<String>,
    got: bool,
    lost: bool,
    ms: u64,
    log: Vec<Value>,
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(State::default()))
}

fn notify() -> &'static Notify {
    static N: OnceLock<Notify> = OnceLock::new();
    N.get_or_init(Notify::new)
}

/// How long the mock waits for a receipt.
pub(super) fn wait_ms() -> u64 {
    let ms = state().lock().unwrap().ms;
    if ms > 0 { ms } else { DEFAULT_MS }
}

/// The `receipt` view the core adds to a waiting op.
pub(super) fn view(ms: u64) -> Value {
    json!({"waiting": true, "remaining_ms": ms})
}

/// Wait for `op_id`'s receipt; true if it came in time.
pub(super) async fn wait(op_id: &str) -> bool {
    let ms = wait_ms();
    {
        let mut s = state().lock().unwrap();
        s.waiting = Some(op_id.to_owned());
        s.got = false;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
    loop {
        if state().lock().unwrap().got {
            break;
        }
        if tokio::time::timeout_at(deadline, notify().notified()).await.is_err() {
            break;
        }
    }
    let mut s = state().lock().unwrap();
    s.waiting = None;
    std::mem::take(&mut s.got)
}

/// `op.receipt`: the op from `params.op_id` only.
pub(super) fn control(_m: &Mock, body: &Value) -> Response {
    let op = body["params"]["op_id"].as_str().map(str::to_owned);
    let mut s = state().lock().unwrap();
    let accepted = !s.lost && op.is_some() && s.waiting == op;
    s.log.push(json!({"body": body, "accepted": accepted}));
    if !accepted {
        let msg = if op.is_none() { "params.op_id required" } else { "no receipt awaited for this op" };
        let e = json!({"ok": false, "action": "op.receipt", "error": {"code": "conflict", "message": msg}});
        return (StatusCode::CONFLICT, axum::Json(e)).into_response();
    }
    s.got = true;
    drop(s);
    notify().notify_waiters();
    axum::Json(json!({"ok": true, "action": "op.receipt", "result": {"op_id": op}})).into_response()
}

pub(super) fn log() -> Vec<Value> {
    state().lock().unwrap().log.clone()
}

async fn hook(body: Bytes) -> StatusCode {
    let q: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let mut s = state().lock().unwrap();
    if q["reset"].as_bool() == Some(true) {
        *s = State::default();
    }
    if let Some(l) = q["lost"].as_bool() {
        s.lost = l;
    }
    if let Some(ms) = q["ms"].as_u64() {
        s.ms = ms;
    }
    StatusCode::NO_CONTENT
}

pub(super) fn routes(r: Router<std::sync::Arc<Mock>>) -> Router<std::sync::Arc<Mock>> {
    r.route("/mock/receipt", post(hook))
}
