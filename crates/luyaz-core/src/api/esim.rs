//! eSIM for the touch screen and luyaz-web: `u60-esim`, the agent's own
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

/// The device's host: lpac, the shell and the reboot as the agent's; the data check is the
/// bearer itself (rmnet address + route, P0-1: B31's `get_wwaniface` says connected when it is
/// not); a switch's steps go to the SIM identity tracking and drop the cached `get_sim_info`.
pub fn real_host() -> std::sync::Arc<dyn luyaz_esim::Host> {
    std::sync::Arc::new(luyaz_esim::RealHost {
        data_up: Box::new(crate::ops::reconnect::read_bearer_up),
        net_probe: Box::new(|| crate::o2::net_probe(crate::ops::reconnect::IFACE)),
        planned_reboot: Box::new(|| {
            if let Err(e) = crate::longterm::boot::plan("esim-identity") {
                eprintln!("[esim] planned-reboot marker: {e}");
            }
        }),
        switch_event: Box::new(|ev| {
            let now = std::time::Instant::now();
            let mut s = crate::sim_identity::global()
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let mut ended = None;
            match ev {
                luyaz_esim::SwitchEvent::Leaving { .. } => s.intent(now),
                luyaz_esim::SwitchEvent::CardSwitched => {}
                luyaz_esim::SwitchEvent::Ended { reason, rebooting } => {
                    s.job_ended(now);
                    ended = Some((s.epoch(), reason, rebooting));
                }
            }
            drop(s);
            if let Some((epoch, reason, rebooting)) = ended {
                crate::sim_alerts::switch_ended(epoch, &reason, rebooting);
            }
            crate::state::invalidate_cache();
        }),
    })
}

/// GET /api/esim/profiles
pub async fn profiles(State(c): State<Compat>) -> Response {
    answer(tokio::task::spawn_blocking(move || c.esim.profiles()).await)
}

/// GET /api/esim/job
pub async fn job(State(c): State<Compat>) -> Response {
    let (s, mut v) = c.esim.job();
    // the shared words for a switch (`sim_say`): the UIs show these, not the reason codes
    let say = crate::sim_say::for_job(&v["data"]);
    if let Some(o) = v["data"].as_object_mut() {
        crate::sim_say::stamp(o, say);
    }
    reply(s, v)
}

/// POST /api/esim/cancel — stop a switch's busy-card retries
pub async fn cancel(State(c): State<Compat>) -> Response {
    let (s, v) = c.esim.cancel();
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
