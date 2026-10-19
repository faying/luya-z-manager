//! The firmware's signal detection (zte_nwinfo_api) moved from the agent: progress, results, start, stop.
//!
//! u60-web.md 8d-1b: the agent's reads luyaz-web still used, served here at the agent's path with
//! the agent's reply shape (`{ok, data}`), so luyaz-web only changes where it asks. On demand,
//! off the collection tick (spawn_blocking / a timeout), never cached across requests unless said.
//!
//! The vendor's answers go out untouched (CN B31 answers `{"progress":"","quality":""}` and `{}`
//! when nothing ran); luyaz-web reads what it knows. Start and stop are the agent's
//! `vendor.call`s, sent to u60d's own `/control` as `web` (journal, write lock, its table).

use axum::{extract::State, response::Response};
use serde_json::{Value, json};
use std::time::Duration;

use super::{Compat, reply};

const OBJECT: &str = "zte_nwinfo_api";
/// A read the firmware does not answer in this long is an error, not a hang.
const READ_TIMEOUT: Duration = Duration::from_secs(8);

/// The agent's mapping for all four: the vendor's answer as `data`, any error a 503.
pub(super) fn answer(r: Result<Value, String>) -> (u16, Value) {
    match r {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

async fn read(c: &Compat, method: &'static str) -> Response {
    let r = match tokio::time::timeout(READ_TIMEOUT, (c.src.ubus)(OBJECT, method, json!({}))).await
    {
        Ok(r) => r,
        Err(_) => Err(format!("{OBJECT} {method}: timed out")),
    };
    let (s, v) = answer(r);
    reply(s, v)
}

async fn vendor(c: &Compat, method: &str) -> Response {
    let params = json!({"object": OBJECT, "method": method, "args": {}});
    let (s, v) = answer(
        c.writer("web")
            .send("vendor.call", &params)
            .await
            .into_result(),
    );
    reply(s, v)
}

/// GET /api/cell/signal-detect/progress (`nwinfo_get_progress_and_quality`)
pub async fn progress(State(c): State<Compat>) -> Response {
    read(&c, "nwinfo_get_progress_and_quality").await
}

/// GET /api/cell/signal-detect/results (`nwinfo_get_detect_quality_recorder`)
pub async fn results(State(c): State<Compat>) -> Response {
    read(&c, "nwinfo_get_detect_quality_recorder").await
}

/// POST /api/cell/signal-detect/start (`nwinfo_start_detect_signal_quality`). Unlike the agent,
/// refused while a deep diagnosis runs, as the speed test is: the detection works the modem and
/// would skew what the diagnosis measures. Nothing else holds it off (core never counted it as
/// a network-affecting write, `ops::spec`).
pub async fn start(State(c): State<Compat>) -> Response {
    let busy = luyaz_api::diagnose::refusal_at(&c.gate(), c.clock.now() as u64);
    if let Some((s, v)) = busy {
        return reply(s, v);
    }
    vendor(&c, "nwinfo_start_detect_signal_quality").await
}

/// POST /api/cell/signal-detect/stop (`nwinfo_end_detect_signal_quality`): never held off.
pub async fn stop(State(c): State<Compat>) -> Response {
    vendor(&c, "nwinfo_end_detect_signal_quality").await
}
