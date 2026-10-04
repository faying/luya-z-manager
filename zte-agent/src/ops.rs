//! The write-op layer for the web (E4 T9, write-op-layer.md DD4–DD16):
//! datad's transaction state, the revert / keep / "got it" buttons, the
//! change log and undo. All of it is datad's (STATE_V2.md §12): the texts,
//! the countdown, what can be undone. The agent only passes it through, with
//! `source: web` on the writes.

use std::time::Duration;

use serde_json::{json, Value};

use crate::datad_feed::{self, PostError};

/// The transaction actions a page may send straight to datad (undo, retry):
/// datad's described actions (`ops/spec.rs`). Anything else is refused here.
const WRITE_ACTIONS: [&str; 1] = ["network.set_mode"];

/// How long a read (state fallback, change log) may wait for datad.
const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// Revert / keep / ack: datad answers at once (no vendor call in line).
const ACT_TIMEOUT: Duration = Duration::from_secs(10);

/// `GET /api/ops`: `{op, datad, stalled, supported}`.
/// - `op`: datad's `op` block (`{rollback_enabled, active, last, notice?}`), with
///   `active.remaining_ms` brought to now; null when unknown.
/// - `datad`: `up`, `stuck` (executor has not moved for 20 s, D12) or `down`.
/// - `supported`: datad has the write-op layer (an older one has no `op`).
pub fn state() -> (u16, Value) {
    let feed = datad_feed::global().map(|f| f.view()).filter(|v| v.subscribed());
    if let Some(view) = feed {
        let stalled = datad_feed::executor_stalled();
        let datad = if stalled { "stuck" } else { "up" };
        return match view.blocks.get("op") {
            Some(b) if b.fresh() => {
                let op = aged(b.data.clone(), view.op_at.map(|t| t.elapsed()).unwrap_or_default());
                ok(json!({"op": op, "datad": datad, "stalled": stalled, "supported": true}))
            }
            // stale while the executor is stuck: the last we know, no countdown
            Some(b) => ok(json!({"op": frozen(b.data.clone()), "datad": datad, "stalled": stalled, "supported": true})),
            None => ok(json!({"op": null, "datad": datad, "stalled": stalled, "supported": false})),
        };
    }
    // not subscribed (starting, or fallback): ask datad's screen read
    match screen_cached() {
        Some(s) => {
            let stalled = s["exec_age_ms"].as_u64().is_some_and(|ms| ms >= datad_feed::EXEC_STALL.as_millis() as u64);
            let supported = s.get("op").is_some();
            ok(json!({
                "op": s.get("op").cloned().unwrap_or(Value::Null),
                "datad": if stalled { "stuck" } else { "up" },
                "stalled": stalled,
                "supported": supported,
            }))
        }
        None => ok(json!({"op": null, "datad": "down", "stalled": false, "supported": false})),
    }
}

/// `remaining_ms` was as of `age` ago.
fn aged(mut op: Value, age: Duration) -> Value {
    if let Some(ms) = op["active"]["remaining_ms"].as_u64() {
        op["active"]["remaining_ms"] = json!(ms.saturating_sub(age.as_millis() as u64));
    }
    op
}

/// A stale block: keep what it said, drop the countdown (it would be a guess).
fn frozen(mut op: Value) -> Value {
    if op["active"].is_object() {
        op["active"]["frozen"] = json!(true);
    }
    op
}

/// [`screen`], at most one in flight and reused for [`SCREEN_TTL`]: in
/// fallback datad may be hung, and every open tab polls; a 3 s read per poll
/// would tie up the agent's few workers.
fn screen_cached() -> Option<Value> {
    use std::sync::Mutex;
    use std::time::Instant;
    static LAST: Mutex<Option<(Instant, Option<Value>)>> = Mutex::new(None);
    static FETCH: Mutex<()> = Mutex::new(());
    let cached = || LAST.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some((at, v)) = cached() {
        if at.elapsed() < SCREEN_TTL {
            return v;
        }
    }
    let Ok(_one) = FETCH.try_lock() else {
        // someone is reading it now: the last answer (or "don't know")
        return cached().and_then(|(_, v)| v);
    };
    let v = screen();
    *LAST.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), v.clone()));
    v
}

const SCREEN_TTL: Duration = Duration::from_secs(4);

fn screen() -> Option<Value> {
    let base = std::env::var("ZTE_AGENT_DATAD").unwrap_or_else(|_| "http://127.0.0.1:9460".to_string());
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(3))).build().into();
    agent.get(&format!("{base}/v2/screen")).call().ok()?.body_mut().read_json::<Value>().ok()
}

/// `POST /api/ops/act` `{"act": "revert"|"keep"|"ack", "op_id"}`, or
/// `{"act": "notice_ack"}`: "Got it" on the one-time "auto revert is on"
/// notice (`op.notice` = `rollback_on`, DD18; datad records it once for both
/// sides).
pub fn act(body: &[u8]) -> (u16, Value) {
    let Ok(req) = serde_json::from_slice::<Value>(body) else {
        return bad("bad JSON");
    };
    let action = match req["act"].as_str() {
        Some("revert") => "op.revert",
        Some("keep") => "op.keep",
        Some("ack") => "op.ack",
        Some("notice_ack") => {
            let body = json!({"action": "op.notice_ack", "source": "web", "params": {"notice": "rollback_on"}});
            return reply(datad_feed::post_control(datad_feed::datad_addr(), &body, ACT_TIMEOUT));
        }
        _ => return bad("act must be revert, keep, ack or notice_ack"),
    };
    let Some(op_id) = req["op_id"].as_str().filter(|s| valid_id(s)) else {
        return bad("op_id");
    };
    let body = json!({"action": action, "source": "web", "params": {"op_id": op_id}});
    reply(datad_feed::post_control(datad_feed::datad_addr(), &body, ACT_TIMEOUT))
}

/// `GET /api/ops/journal?limit=N` (default 30, at most 200): datad's
/// `journal.list` as it is (`entries` newest first, `owners`).
pub fn journal(query: &str) -> (u16, Value) {
    let limit = query
        .split('&')
        .find_map(|kv| kv.strip_prefix("limit="))
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(30)
        .clamp(1, 200);
    let body = json!({"action": "journal.list", "params": {"limit": limit}});
    reply(datad_feed::post_control(datad_feed::datad_addr(), &body, READ_TIMEOUT))
}

/// `POST /api/ops/write` `{"request": {action, params, undo?}, "op_id"?}`:
/// a change log row's `undo_view.request` (V2-41, `undo: true`), or "try the
/// revert again" with the transaction's raw `rollback_to` (DD9) — datad's own
/// value vocabulary, which the network-mode route would not take (it may read
/// back as `NETWORK_auto`). Sent as it is with `source: web`. Only datad's
/// transaction actions; datad itself decides whether it can still be undone.
pub fn write(body: &[u8]) -> (u16, Value) {
    let Ok(req) = serde_json::from_slice::<Value>(body) else {
        return bad("bad JSON");
    };
    let r = &req["request"];
    let Some(action) = r["action"].as_str().filter(|a| WRITE_ACTIONS.contains(a)) else {
        return bad("not a transaction action");
    };
    let Some(params) = r["params"].as_object() else {
        return bad("params");
    };
    if datad_feed::executor_stalled() {
        return crate::datad_write::Reply::Unreachable { error: "datad executor stuck".into(), maybe_done: false }.into_http();
    }
    // the page's own id, so a resent request reads back instead of writing twice
    let op_id = match req["op_id"].as_str() {
        Some(id) if valid_id(id) => id.to_string(),
        Some(_) => return bad("op_id"),
        None => web_op_id(),
    };
    let undo = r["undo"].as_bool().unwrap_or(false);
    let body = json!({"action": action, "source": "web", "undo": undo, "op_id": op_id, "params": params});
    match datad_feed::post_control(datad_feed::datad_addr(), &body, crate::datad_write::TIMEOUT) {
        Ok((status, v)) => crate::datad_write::parse(status, &v).into_http(),
        // no emergency script: the page's own route has it; this is a choice
        Err(PostError::Connect(e)) => crate::datad_write::Reply::Unreachable { error: e, maybe_done: false }.into_http(),
        Err(PostError::NoReply(e)) => crate::datad_write::Reply::Unreachable { error: e, maybe_done: true }.into_http(),
    }
}

/// A fresh `web-<start>-<n>` id: a resend of the same request only reads back
/// the existing state at datad, never writes twice.
fn web_op_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::OnceLock;
    static START: OnceLock<u64> = OnceLock::new();
    static N: AtomicU64 = AtomicU64::new(0);
    let start = *START.get_or_init(datad_feed::now_epoch);
    format!("web-{start}-{}", N.fetch_add(1, Ordering::Relaxed) + 1)
}

fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// datad's reply to a read or an `op.*`: 200 → `{ok, data: result}`;
/// 409 (`invalid_state`, `busy`) stays 409 with datad's error; no answer →
/// 503 with the agent's usual datad-down words.
fn reply(r: Result<(u16, Value), PostError>) -> (u16, Value) {
    match r {
        Ok((200, v)) if v["ok"] == true => ok(v.get("result").cloned().unwrap_or(Value::Null)),
        Ok((status, v)) => {
            let code = v["error"]["code"].as_str().unwrap_or("").to_string();
            let msg = v["error"]["message"].as_str().or_else(|| v["error"].as_str()).unwrap_or("failed").to_string();
            let status = if status == 409 || status == 400 { status } else { 503 };
            (status, json!({"ok": false, "error": msg, "code": code}))
        }
        Err(PostError::Connect(e)) => crate::datad_write::Reply::Unreachable { error: e, maybe_done: false }.into_http(),
        Err(PostError::NoReply(e)) => crate::datad_write::Reply::Unreachable { error: e, maybe_done: true }.into_http(),
    }
}

fn ok(data: Value) -> (u16, Value) {
    (200, json!({"ok": true, "data": data}))
}

fn bad(e: &str) -> (u16, Value) {
    (400, json!({"ok": false, "error": e}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn countdown_is_brought_to_now() {
        let op = json!({"rollback_enabled": true, "active": {"op_id": "w-1", "remaining_ms": 90000}, "last": null});
        assert_eq!(aged(op, Duration::from_millis(1500))["active"]["remaining_ms"], 88500);
        let op = json!({"active": {"remaining_ms": 1000}});
        assert_eq!(aged(op, Duration::from_secs(5))["active"]["remaining_ms"], 0);
        let op = json!({"active": null, "last": {"op_id": "w-1"}});
        assert_eq!(aged(op.clone(), Duration::from_secs(5)), op);
    }

    #[test]
    fn stale_block_has_no_countdown() {
        assert_eq!(frozen(json!({"active": {"remaining_ms": 5}}))["active"]["frozen"], true);
        assert!(frozen(json!({"active": null}))["active"].is_null());
    }

    #[test]
    fn bad_requests_send_nothing() {
        assert_eq!(act(b"{\"act\":\"drop\",\"op_id\":\"x\"}").0, 400);
        assert_eq!(act(b"{\"act\":\"ack\",\"op_id\":\"a b\"}").0, 400);
        assert_eq!(act(b"nope").0, 400);
        assert_eq!(act(b"{\"act\":\"notice\"}").0, 400);
        assert_eq!(write(b"{\"request\":{\"action\":\"device.reboot\",\"undo\":true,\"params\":{}}}").0, 400);
        assert_eq!(write(b"{\"request\":{\"action\":\"network.set_mode\",\"undo\":true}}").0, 400);
        assert_eq!(write(b"{\"op_id\":\"a b\",\"request\":{\"action\":\"network.set_mode\",\"params\":{\"mode\":\"Only_LTE\"}}}").0, 400);
    }

    #[test]
    fn op_ids_are_fresh_and_valid() {
        let a = web_op_id();
        let b = web_op_id();
        assert_ne!(a, b);
        assert!(valid_id(&a) && a.starts_with("web-"));
    }

    #[test]
    fn replies_map_to_http() {
        assert_eq!(reply(Ok((200, json!({"ok": true, "result": {"entries": []}})))), (200, json!({"ok": true, "data": {"entries": []}})));
        let (s, v) = reply(Ok((409, json!({"ok": false, "error": {"code": "invalid_state", "message": "not waiting"}}))));
        assert_eq!((s, v["code"].as_str()), (409, Some("invalid_state")));
        assert_eq!(reply(Ok((500, json!({"ok": false, "error": "x"})))).0, 503);
        let (s, v) = reply(Err(PostError::Connect("refused".into())));
        assert_eq!((s, v["datad"].as_str()), (503, Some("down")));
    }
}
