//! eSIM for the touch screen and luyaz-web (Phase 4 steps 8e, 9b): `u60-esim`, the agent's own
//! engine. Only with u60d's writes on, every route included: even the profile list runs lpac on
//! the card, and two lpac processes on one eUICC conflict. Before a switch, u60d's own `/control` is
//! told as `screen` (D40 `op.interrupt`), as the agent tells core.

use axum::{body::Bytes, extract::State, response::Response};
use serde_json::{Value, json};
use std::time::Duration;

use super::{Compat, reply};

fn answer(r: Result<(u16, Value), tokio::task::JoinError>) -> Response {
    match r {
        Ok((s, v)) => reply(s, v),
        Err(_) => reply(500, json!({"ok": false, "error": "internal"})),
    }
}

/// The device's host: lpac, the shell and the reboot as the agent's; the data check through u60d's
/// ubus (from the engine's own threads, so it blocks on the runtime handle).
pub fn real_host(
    ubus: std::sync::Arc<
        dyn Fn(&'static str, &'static str, Value) -> super::UbusReply + Send + Sync,
    >,
) -> std::sync::Arc<dyn luyaz_esim::Host> {
    let handle = tokio::runtime::Handle::current();
    std::sync::Arc::new(luyaz_esim::RealHost {
        data_up: Box::new(move || {
            let w = handle
                .block_on(ubus(
                    "zwrt_data",
                    "get_wwaniface",
                    json!({"source_module": "zte_topsw_data", "cid": 1}),
                ))
                .unwrap_or(Value::Null);
            luyaz_api::netinfo::data_connected(&w)
        }),
    })
}

/// GET /api/esim/profiles
pub async fn profiles(State(c): State<Compat>) -> Response {
    answer(tokio::task::spawn_blocking(move || c.esim.profiles()).await)
}

/// GET /api/esim/job
pub async fn job(State(c): State<Compat>) -> Response {
    let (s, v) = c.esim.job();
    reply(s, v)
}

/// POST /api/esim/switch `{"iccid"}`
pub async fn switch(State(c): State<Compat>, body: Bytes) -> Response {
    let handle = tokio::runtime::Handle::current();
    let client = c.src.control.clone();
    let interrupt = Box::new(move || {
        let r = handle.block_on(client.send_with(
            "screen",
            None,
            "op.interrupt",
            &json!({"what": "esim"}),
            Duration::from_secs(3),
        ));
        if !r.ok() {
            eprintln!("[compat] op.interrupt esim: {r:?}");
        }
    });
    answer(tokio::task::spawn_blocking(move || c.esim.switch(&body, interrupt)).await)
}

/// GET /api/esim/status
pub async fn status(State(c): State<Compat>) -> Response {
    answer(tokio::task::spawn_blocking(move || c.esim.status()).await)
}

/// GET /api/esim/notifications
pub async fn notifications(State(c): State<Compat>) -> Response {
    answer(tokio::task::spawn_blocking(move || c.esim.notifications()).await)
}

/// POST /api/esim/download
pub async fn download(State(c): State<Compat>, body: Bytes) -> Response {
    answer(tokio::task::spawn_blocking(move || c.esim.download(&body)).await)
}

/// POST /api/esim/nickname
pub async fn nickname(State(c): State<Compat>, body: Bytes) -> Response {
    answer(tokio::task::spawn_blocking(move || c.esim.nickname(&body)).await)
}

/// POST /api/esim/delete
pub async fn delete(State(c): State<Compat>, body: Bytes) -> Response {
    answer(tokio::task::spawn_blocking(move || c.esim.delete(&body)).await)
}

/// POST /api/esim/notifications/process
pub async fn notifications_process(State(c): State<Compat>, body: Bytes) -> Response {
    answer(tokio::task::spawn_blocking(move || c.esim.notifications_process(&body)).await)
}
