//! The scenario engine's routes; the engine is [`crate::engines::scenario`].

use axum::{body::Bytes, extract::{Request, State}, middleware::Next, response::Response};
use serde_json::{Value, json};
use luyaz_scenario::Engine as ScenarioEngine;

use super::{Compat, reply};
use crate::engines::scenario::*;

/// Served once the engine runs (it starts a moment after u60d); 503 before.
pub async fn owned(State(c): State<Compat>, request: Request, next: Next) -> Response {
    if c.scenario.engine().is_none() {
        return reply(503, json!({"ok": false, "error": "情景引擎还没起来", "error_en": "the scenario engine has not started"}));
    }
    next.run(request).await
}

/// Run a route of the engine off the runtime thread (some block for long: disabling restores
/// away, the scan runs `iw`).
async fn with_engine(c: &Compat, f: impl FnOnce(&ScenarioEngine) -> (u16, Value) + Send + 'static) -> Response {
    let Some(e) = c.scenario.engine() else {
        return reply(503, json!({"ok": false, "error": "情景引擎还没起来", "error_en": "the scenario engine has not started"}));
    };
    let (status, body) = crate::state::blocking(move || f(&e)).await;
    reply(status, body)
}

pub async fn get(State(c): State<Compat>) -> Response {
    with_engine(&c, |e| e.http_get()).await
}

pub async fn put(State(c): State<Compat>, body: Bytes) -> Response {
    with_engine(&c, move |e| e.http_put(&body)).await
}

pub async fn template(State(c): State<Compat>) -> Response {
    with_engine(&c, |e| e.http_template()).await
}

pub async fn pin(State(c): State<Compat>, body: Bytes) -> Response {
    with_engine(&c, move |e| e.http_pin(&body)).await
}

pub async fn enabled(State(c): State<Compat>, body: Bytes) -> Response {
    with_engine(&c, move |e| e.http_enabled(&body)).await
}

pub async fn scan(State(c): State<Compat>) -> Response {
    with_engine(&c, |e| e.http_scan()).await
}

pub async fn log(State(c): State<Compat>) -> Response {
    with_engine(&c, |e| e.http_log()).await
}

// ── the actions only the engine needs here ─────────────────────────────────

/// PUT /api/wifi/guest (the agent's `wifi::guest_set`): the uci keys through `/control` under the
/// Wi-Fi lock, then the reload and its check on a thread of their own, which keeps the lock.
pub async fn guest(State(c): State<Compat>, body: Bytes) -> Response {
    let set = match luyaz_api::wifi::guest_set(&body) {
        Ok(s) => s,
        Err((s, b)) => return reply(s, b),
    };
    let lock_path = c.path(luyaz_wifi::LOCK_FILE).to_string_lossy().into_owned();
    let lk = match crate::state::blocking(move || luyaz_wifi::lock_at(&lock_path, luyaz_wifi::HTTP_WAIT)).await {
        Ok(l) => l,
        Err(e) => return reply(503, json!({"ok": false, "error": e})),
    };
    if set.is_empty() {
        return reply(200, json!({"ok": true, "data": {"status": "ok", "note": "no changes"}}));
    }
    // Guest interfaces may not exist: best effort, the skipped ones listed by /control
    let r = c
        .src
        .control
        .send("scenario", None, "wifi.apply", &json!({"set": set, "reload": false, "best_effort": true}))
        .await;
    let changed = match r {
        luyaz_api::control::Reply::Done { result, .. } => result["committed"].as_array().is_some_and(|c| !c.is_empty()),
        r => {
            let (s, b) = r.into_http();
            return reply(s, b);
        }
    };
    if !changed {
        return reply(200, json!({"ok": true, "data": {"status": "ok", "note": "no changes"}}));
    }
    let (url, token) = c.src.control.target();
    let spawned = std::thread::Builder::new().name("wifi-guest".into()).spawn(move || {
        let host = WifiHost { control: url, token };
        let data = luyaz_wifi::reload_and_verify(&host, &lk);
        eprintln!("[compat] guest Wi-Fi: reload finished {data}");
        drop(lk);
    });
    if let Err(e) = spawned {
        eprintln!("[compat] guest Wi-Fi: could not start the reload thread: {e}");
    }
    reply(200, json!({"ok": true, "data": {"status": "ok"}}))
}

/// PUT /api/device/power-save (the agent's `device_power_save_set`): the stock
/// `zwrt_mc.device.manager set_device_info` with the body as it is.
pub async fn power_save(State(c): State<Compat>, body: Bytes) -> Response {
    let Ok(args) = serde_json::from_slice::<Value>(&body) else {
        return reply(400, json!({"ok": false, "error": "invalid JSON"}));
    };
    let r = c
        .src
        .control
        .send(
            "scenario",
            None,
            "vendor.call",
            &json!({"object": "zwrt_mc.device.manager", "method": "set_device_info", "args": args}),
        )
        .await;
    match r.into_result() {
        Ok(data) => reply(200, json!({"ok": true, "data": data})),
        Err(e) => reply(503, json!({"ok": false, "error": e})),
    }
}
