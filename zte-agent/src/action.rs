// ─────────────────────────────────────────────────────────────────────────────
// Action — "send one request to our own HTTP API, in-process".
//
// This is the execution primitive the scheduler has used since it was written,
// extracted here so the scenario engine can share it instead of growing a
// second copy. `exec()` dispatches straight into `server::route()` without
// going over a socket, so it bypasses `server.rs`'s bearer-token check by
// construction — every caller is therefore responsible for validating the path
// itself (see `scheduler::validate_job` and `scenario`'s allow-list).
//
// Two rules that callers must honour, both learned the hard way:
//
//  1. `route()` takes the same locks the HTTP handlers take. Never call `exec()`
//     while holding a lock a handler could want — `Mutex` is not re-entrant and
//     the deadlock also wedges an HTTP worker thread. Collect the work, drop the
//     lock, execute, then re-acquire to record results (the scheduler's
//     phase1/2/3 shape).
//  2. An HTTP 200 from `exec()` does NOT mean the device changed. CHILL and eSIM
//     return `{"status":"running"}` immediately and finish on a worker thread;
//     so does `wifi::wifi_set`'s reload, whose result only reaches the log.
//     Anything that must actually have taken effect needs
//     its own verification step after `exec()` returns.
// ─────────────────────────────────────────────────────────────────────────────

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tiny_http::Method;

use crate::handlers::AppState;

/// One request to our own API. Wire-compatible with the shape the scheduler has
/// been persisting to `scheduler.json` since v1 — do not reorder or rename.
#[derive(Serialize, Deserialize, Clone)]
pub struct Action {
    pub method: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

/// What actually happened. `status` is `None` when the action never ran at all
/// (an unparseable method), which is deliberately distinct from "ran and
/// returned an error" — the old code dropped that case on the floor and left no
/// trace anywhere, so a typo'd method looked exactly like a job that had never
/// come due.
pub struct Outcome {
    pub status: Option<u16>,
    pub error: Option<String>,
}

impl Outcome {
    /// Used by the scenario applier to decide whether to keep going or roll
    /// back. `#[allow]` because the scheduler records outcomes rather than
    /// branching on them.
    #[allow(dead_code)]
    pub fn ok(&self) -> bool {
        matches!(self.status, Some(s) if s < 400)
    }
}

pub fn parse_method(s: &str) -> Option<Method> {
    match s {
        "GET" => Some(Method::Get),
        "POST" => Some(Method::Post),
        "PUT" => Some(Method::Put),
        "DELETE" => Some(Method::Delete),
        _ => None,
    }
}

/// Serialise an action body for `exec`. Absent body ⇒ empty slice.
pub fn body_bytes(body: &Option<Value>) -> Vec<u8> {
    match body {
        Some(v) => serde_json::to_vec(v).unwrap_or_default(),
        None => Vec::new(),
    }
}

/// Execute one action in-process. See the module comment for the locking and
/// "200 != done" rules before calling this.
pub fn exec(state: &AppState, method: &str, path: &str, body: &[u8]) -> Outcome {
    let parsed = match parse_method(method) {
        Some(m) => m,
        None => {
            return Outcome {
                status: None,
                error: Some(format!("unsupported method {method:?}")),
            }
        }
    };
    let (status, resp) = crate::server::route(&parsed, path, state, body);
    let error = if status >= 400 {
        Some(
            resp.get("error")
                .and_then(|e| e.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("HTTP {status}")),
        )
    } else {
        None
    };
    Outcome {
        status: Some(status),
        error,
    }
}
