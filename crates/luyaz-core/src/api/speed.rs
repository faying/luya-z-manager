//! The speed test for the touch screen (Phase 4 step 8e): `u60-speedtest`, the agent's own engine,
//! claimed through u60d's deep-diagnosis gate (D10). Only with u60d's writes on (step 9; one
//! engine on the device).

use axum::{body::Bytes, extract::State, response::Response};
use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;

use super::{reply, Compat};

fn answer(r: Result<(u16, Value), tokio::task::JoinError>) -> Response {
    match r {
        Ok((s, v)) => reply(s, v),
        Err(_) => reply(500, json!({"ok": false, "error": "internal"})),
    }
}

/// The claim: refused while a diagnosis runs, then "already running" (the agent's `admit`).
pub(super) fn admit(c: &Compat, running: &AtomicBool) -> Result<(), (u16, Value)> {
    let gate = c.gate();
    if let Some(busy) = luyaz_api::diagnose::refusal_at(&gate, c.clock.now() as u64) {
        return Err(busy);
    }
    luyaz_speedtest::claim_running(running)
}

/// GET /api/speedtest/servers: cache-only, with single-flight background discovery.
pub async fn servers(
    State(c): State<Compat>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let (s, v) = if q.get("refresh").is_some_and(|v| v == "1") {
        c.speed.refresh_servers()
    } else {
        c.speed.servers()
    };
    reply(s, v)
}

/// POST /api/speedtest/start `{"provider"?, "server_id"?}`: immediate job admission.
pub async fn start(State(c): State<Compat>, body: Bytes) -> Response {
    answer(
        tokio::task::spawn_blocking(move || c.speed.start(&body, &|r: &AtomicBool| admit(&c, r)))
            .await,
    )
}

/// GET /api/speedtest/progress
pub async fn progress(State(c): State<Compat>) -> Response {
    let (s, v) = c.speed.progress();
    reply(s, v)
}

/// POST /api/speedtest/stop
pub async fn stop(State(c): State<Compat>) -> Response {
    let (s, v) = c.speed.stop();
    reply(s, v)
}

/// Local LibreSpeed endpoints; saving does not run a measurement or contact any server.
pub async fn config(State(c): State<Compat>) -> Response {
    let (s, v) = c.speed.config();
    reply(s, v)
}
pub async fn configure(State(c): State<Compat>, body: Bytes) -> Response {
    answer(tokio::task::spawn_blocking(move || c.speed.configure(&body)).await)
}
