//! SMS forwarding's routes; the engine is [`crate::engines::sms_forward`].

use axum::{
    extract::{Request, State},
    response::Response,
};
use serde_json::{Value, json};
use luyaz_sms_forward::SmsForwarder;

use super::{Compat, reply};
use crate::engines::sms_forward::*;

/// One route on the blocking pool, the caller's source on the thread (a web test or retry sends
/// as `web`).
async fn call(c: Compat, req: Request, f: fn(&SmsForwarder, &[u8]) -> (u16, Value)) -> Response {
    let src = super::charge::source(&req);
    let Ok(body) = axum::body::to_bytes(req.into_body(), 256 * 1024).await else {
        return reply(400, json!({"ok": false, "error": "invalid JSON"}));
    };
    let Some(fwd) = c.sms.current() else {
        return reply(
            503,
            json!({"ok": false, "error": "短信转发正在交接，稍后再试", "error_en": "SMS forwarding is changing hands, try again shortly"}),
        );
    };
    let r = tokio::task::spawn_blocking(move || {
        SOURCE.with(|s| s.set(src));
        f(&fwd, &body)
    })
    .await;
    match r {
        Ok((s, v)) => reply(s, v),
        Err(_) => reply(500, json!({"ok": false, "error": "internal error"})),
    }
}

macro_rules! route {
    ($name:ident, |$f:ident, $b:ident| $e:expr) => {
        pub async fn $name(State(c): State<Compat>, req: Request) -> Response {
            call(c, req, |$f, $b| $e).await
        }
    };
}

route!(config_get, |f, _b| f.config_get());
route!(config_set, |f, b| f.config_set(b));
route!(rules_create, |f, b| f.rules_create(b));
route!(rules_update, |f, b| f.rules_update(b));
route!(rules_delete, |f, b| f.rules_delete(b));
route!(rules_toggle, |f, b| f.rules_toggle(b));
route!(test, |f, b| f.test_forward(b));
route!(log_get, |f, _b| f.log_get());
route!(log_clear, |f, _b| f.log_clear());
route!(retry, |f, b| f.retry_forward(b));
