//! The home exit's routes; the engine's host is [`crate::engines::home_exit`].

use axum::{body::Bytes, extract::State, response::Response};

use super::{Compat, reply};

/// GET /api/services/chill/home, from u60d's own machine.
pub async fn get(State(c): State<Compat>) -> Response {
    let Some(e) = c.home_exit.clone().filter(|e| e.owner()) else {
        return super::home::get(State(c)).await;
    };
    match tokio::task::spawn_blocking(move || e.get()).await {
        Ok((s, v)) => reply(s, v),
        Err(_) => reply(500, serde_json::json!({"ok": false, "error": "internal error"})),
    }
}

/// PUT /api/services/chill/home (only routed when u60d runs the home exit).
pub async fn put(State(c): State<Compat>, body: Bytes) -> Response {
    let Some(e) = c.home_exit.clone() else {
        return reply(404, serde_json::json!({"ok": false, "error": "not_in_compat_yet"}));
    };
    match tokio::task::spawn_blocking(move || e.put(&body)).await {
        Ok((s, v)) => reply(s, v),
        Err(_) => reply(500, serde_json::json!({"ok": false, "error": "internal error"})),
    }
}

