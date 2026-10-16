//! The charge limit and the battery estimate (u60-platform.md Phase 4 step 9c; the agent's until
//! Phase 10). They go together (the estimate reads the limit's settings and override).
//!
//! u60d runs them while its writes and the policies (`LUYAZ_POLICIES`) are on:
//! - `/api/device/charge-control` is served here (with the caller's source): settings and the
//!   manual override are u60d's (`plugin::charge`), saved to the agent's file in its format.
//! - The charge policy loop (`plugin::charge_policy`, on every battery/charger read) hands its
//!   actions to [`execute`], which reads the stop flag from ubus first and writes
//!   `power.direct_supply.set` through u60d's own `/control`, source auto. An unknown stop flag
//!   (ubus not answering) writes nothing.
//! - The agent's three habits stay: turning the limit off always resumes charging (a manual stop
//!   too), turning it on enforces at once, both under the caller's source; a plug-in waits 500 ms.
//!
//! Policies off: charge-control answers 409 `not_owner` (nobody enforces the limit then).

use axum::{
    extract::{ConnectInfo, Request, State},
    middleware::Next,
    response::Response,
};
use serde_json::{Value, json};
use std::net::SocketAddr;
use luyaz_api::charge::{self as shared, Settings};
use luyaz_policy::charge_policy::Battery;

use super::{Compat, reply};
use crate::plugin::{charge, charge_policy as policy};

/// How often the policies' switch is looked at.
const LOOK_SECS: u64 = 2;

/// Needs the policies on (`LUYAZ_POLICIES=0` installs no loop: serving the route then would never
/// enforce).
pub(super) fn owns(c: &Compat) -> bool {
    c.writes && crate::plugin::enabled()
}

/// In front of charge-control: 409 with the policies off.
pub(super) async fn owned(State(c): State<Compat>, req: Request, next: Next) -> Response {
    if owns(&c) {
        return next.run(req).await;
    }
    reply(
        409,
        json!({"ok": false, "error": "充电策略没开（LUYAZ_POLICIES=0）", "error_en": "the charge policies are off",
            "not_owner": true}),
    )
}

/// Who a write is for: the source luyaz-web or the scheduler names (loopback only), else the
/// screen (the only other caller on loopback).
pub(super) fn source(req: &Request) -> &'static str {
    let loopback = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .is_none_or(|ci| ci.0.ip().is_loopback());
    let named = req
        .headers()
        .get(luyaz_api::headers::SOURCE_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(crate::ops::Source::parse);
    match named {
        Some(s) if loopback => s.as_str(),
        _ => "screen",
    }
}

/// The stop flag as ubus has it; None when it does not answer.
async fn stopped_now(c: &Compat) -> Option<bool> {
    let v = c
        .ubus_res("zwrt_bsp.charger", "list", json!({}))
        .await
        .ok()?;
    shared::stopped_in(&v)
}

fn sysfs(c: &Compat, f: &str) -> String {
    std::fs::read_to_string(c.path(&format!("/sys/class/power_supply/battery/{f}")))
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// One `power.direct_supply.set`; `allow` false = stop charging.
async fn set_charging(c: &Compat, source: &'static str, allow: bool, why: &'static str) -> bool {
    let r = c
        .writer(source)
        .send("power.direct_supply.set", &json!({"enabled": !allow}))
        .await;
    let ok = r.ok();
    if ok {
        policy::owner_wrote(allow, why);
    } else {
        eprintln!("[compat] charge: set charging {allow} ({why}): {r:?}");
    }
    ok
}

/// The agent's `enforce`: stop at the limit, resume at the band's bottom; nothing when the stop
/// flag is unknown.
async fn enforce(c: &Compat, source: &'static str, s: &Settings, why: &'static str) {
    let Some(stopped) = stopped_now(c).await else {
        eprintln!("[compat] charge: stop flag unknown, not enforcing ({why})");
        return;
    };
    let b = Battery {
        capacity: sysfs(c, "capacity").parse().unwrap_or(0),
        discharging: sysfs(c, "status") == "Discharging",
        stopped,
        limit: s.limit,
        hysteresis: s.hysteresis,
    };
    if let Some(allow) = policy::decide_now(policy::Ask::Enforce(b)) {
        set_charging(c, source, allow, why).await;
    }
}

/// One action of the policy loop (owned).
async fn execute(c: &Compat, job: policy::Job) {
    let (s, manual_override) = charge::current();
    let active = s.enabled && !manual_override;
    match job {
        policy::Job::PluggedIn => {
            // the agent waited 500 ms after a plug-in before enforcing
            tokio::time::sleep(c.clock.secs(1) / 2).await;
            if active {
                enforce(c, "auto", &s, "plugged_in").await;
            }
        }
        policy::Job::Limit if active => enforce(c, "auto", &s, "limit").await,
        policy::Job::Limit => {}
        policy::Job::Unplugged => {
            if stopped_now(c).await == Some(true) {
                set_charging(c, "auto", true, "unplugged").await;
            }
        }
    }
}

/// Follows the policies' switch (every [`LOOK_SECS`]) and runs the policy loop's actions.
pub async fn run(c: Compat, mut jobs: tokio::sync::mpsc::UnboundedReceiver<policy::Job>) {
    let file = c.path(shared::FILE);
    let mut look = tokio::time::interval(c.clock.secs(LOOK_SECS));
    // only what this loop saw change is applied (one touch layer per u60d; tests run several)
    let mut was = false;
    loop {
        tokio::select! {
            _ = look.tick() => {
                let on = owns(&c);
                if on != was {
                    was = on;
                    policy::set_owned(on, &file);
                    eprintln!("[compat] charge: {}", if on { "u60d enforces the limit" } else { "the policies are off" });
                }
            }
            Some(job) = jobs.recv() => {
                if charge::owned() {
                    execute(&c, job).await;
                }
            }
        }
    }
}

/// GET /api/device/charge-control (the agent's `charge_control_get`)
pub async fn get(State(c): State<Compat>) -> Response {
    let (s, v) = current_reply(&c).await;
    reply(s, v)
}

async fn current_reply(c: &Compat) -> (u16, Value) {
    let stopped = stopped_now(c).await.unwrap_or(false);
    let (s, manual_override) = charge::current();
    let capacity = sysfs(c, "capacity").parse().unwrap_or(0);
    (
        200,
        shared::reply(stopped, &sysfs(c, "status"), capacity, &s, manual_override),
    )
}

/// PUT /api/device/charge-control (the agent's `charge_control_set`): a manual stop/resume first,
/// then new settings; answers the state after.
pub async fn put(State(c): State<Compat>, req: Request) -> Response {
    let src = source(&req);
    let Ok(body) = axum::body::to_bytes(req.into_body(), 64 * 1024).await else {
        return reply(400, json!({"ok": false, "error": "invalid JSON"}));
    };
    let r = match shared::parse_request(&body) {
        Ok(r) => r,
        Err((s, v)) => return reply(s, v),
    };
    if let Some(stopped) = r.charging_stopped {
        let res = c
            .writer(src)
            .send("power.direct_supply.set", &json!({"enabled": stopped}))
            .await;
        if !res.ok() {
            let (s, v) = res.into_http();
            return reply(s, v);
        }
        policy::owner_wrote(!stopped, "manual");
        charge::set_override(stopped);
    }
    if let Some(change) = r.settings {
        let new = change.apply(&charge::current().0);
        if let Err(e) = shared::validate(&new) {
            return reply(400, json!({"ok": false, "error": e}));
        }
        charge::save(&new, &c.path(shared::FILE));
        if new.enabled {
            enforce(&c, src, &new, "settings").await;
        } else {
            set_charging(&c, src, true, "settings").await;
        }
    }
    let (s, v) = current_reply(&c).await;
    reply(s, v)
}
