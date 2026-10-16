//! The speed test for the touch screen (Phase 4 step 8e): `u60-speedtest`, the agent's own engine,
//! claimed through u60d's deep-diagnosis gate (D10). Only with u60d's writes on (step 9; one
//! engine on the device).

use axum::{body::Bytes, extract::State, response::Response};
use serde_json::{Value, json};
use std::sync::atomic::AtomicBool;

use super::{Compat, reply};

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

/// GET /api/speedtest/servers (fetches the list at most every 5 minutes; blocking)
pub async fn servers(State(c): State<Compat>) -> Response {
    answer(tokio::task::spawn_blocking(move || c.speed.servers()).await)
}

/// POST /api/speedtest/start `{"server_id"?}`
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
