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
            if let luyaz_esim::SwitchEvent::Leaving { slot, .. } = &ev {
                // the card table's event for this slot says it was ours (D2)
                crate::card_watch::own_switch(*slot, crate::cards::uptime_now());
            }
            let ended = track_switch(
                &mut crate::sim_identity::global()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()),
                ev,
                std::time::Instant::now(),
            );
            if let Some((epoch, reason, rebooting)) = ended {
                crate::sim_alerts::switch_ended(epoch, &reason, rebooting);
            }
            crate::state::invalidate_cache();
            // the profile changed on a card: the card table reads both slots again now
            crate::cards::kick();
        }),
        card_slots: Box::new(|| {
            crate::cards::current()
                .rows
                .iter()
                .filter(|r| r.present)
                .map(|r| (r.slot, r.generation))
                .collect()
        }),
    })
}

/// A switch's step into the SIM identity tracking; a job's end gives back what the alert
/// needs (epoch, reason, rebooting).
fn track_switch(
    s: &mut crate::sim_identity::SimIdentity,
    ev: luyaz_esim::SwitchEvent,
    now: std::time::Instant,
) -> Option<(u64, String, bool)> {
    match ev {
        // a standby / off slot's card: the data card's identity does not move (dual-slot §2)
        ev @ luyaz_esim::SwitchEvent::Leaving { .. } => {
            if moves_data_card(&ev) {
                s.intent(now)
            }
            None
        }
        luyaz_esim::SwitchEvent::CardSwitched => None,
        luyaz_esim::SwitchEvent::Ended { reason, rebooting } => {
            s.job_ended(now);
            Some((s.epoch(), reason, rebooting))
        }
    }
}

/// A switch that moves the data card's identity: only one on the slot data is on (a standby or
/// off slot's card leaves the data card alone, dual-slot §2).
fn moves_data_card(ev: &luyaz_esim::SwitchEvent) -> bool {
    matches!(ev, luyaz_esim::SwitchEvent::Leaving { data_slot: true, .. })
}

/// `?eid=<EID>` or `?slot=<n>` of a GET (the card it is for; none = the only eSIM card).
fn eid_of(q: Option<String>) -> Option<String> {
    let q = q?;
    let eid = q
        .split('&')
        .find_map(|kv| kv.strip_prefix("eid=").map(str::to_owned))
        .filter(|e| !e.is_empty() && e.chars().all(|c| c.is_ascii_hexdigit()));
    eid.or_else(|| {
        q.split('&')
            .find_map(|kv| kv.strip_prefix("slot=")?.parse::<u8>().ok())
            .filter(|s| matches!(s, 1 | 2))
            .map(luyaz_esim::slot_ref)
    })
}

/// GET /api/esim/profiles
pub async fn profiles(State(c): State<Compat>, axum::extract::RawQuery(q): axum::extract::RawQuery) -> Response {
    let eid = eid_of(q);
    answer(tokio::task::spawn_blocking(move || c.esim.profiles_of(eid.as_deref())).await)
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

/// POST /api/esim/reread `{"slot"}` — read an off slot's card again (any card); the card
/// table then reads it at once (its UIM indication, or the kick here when indications are off)
pub async fn reread(State(c): State<Compat>, body: Bytes) -> Response {
    let r = tokio::task::spawn_blocking(move || c.esim.reread(&body)).await;
    if matches!(r, Ok((200, _))) {
        crate::cards::kick();
    }
    answer(r)
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
pub async fn status(State(c): State<Compat>, axum::extract::RawQuery(q): axum::extract::RawQuery) -> Response {
    let eid = eid_of(q);
    answer(tokio::task::spawn_blocking(move || c.esim.status_of(eid.as_deref())).await)
}

/// GET /api/esim/notifications
pub async fn notifications(State(c): State<Compat>, axum::extract::RawQuery(q): axum::extract::RawQuery) -> Response {
    let eid = eid_of(q);
    answer(tokio::task::spawn_blocking(move || c.esim.notifications_of(eid.as_deref())).await)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_data_slot_switch_moves_the_data_card() {
        let ev = |data_slot| luyaz_esim::SwitchEvent::Leaving { target: "8985".into(), slot: 2, data_slot };
        assert!(moves_data_card(&ev(true)));
        assert!(!moves_data_card(&ev(false)));
        assert!(!moves_data_card(&luyaz_esim::SwitchEvent::CardSwitched));
    }

    /// T5 verify: a switch on the standby slot's eUICC leaves the data card's tracking online,
    /// with no epoch (no `sim-changed`) and no stall clock (no `sim-stalled`).
    #[test]
    fn a_standby_switch_does_not_leave_the_data_card() {
        use crate::sim_identity::{Identity, Phase, SimIdentity};
        let t0 = std::time::Instant::now();
        let at = |n: u64| t0 + std::time::Duration::from_secs(n);
        let card = Identity::new("89860100000000001111", "46001");
        let mut s = SimIdentity::default();
        s.observe(card.clone(), true, at(0));
        let leaving = luyaz_esim::SwitchEvent::Leaving { target: "8985".into(), slot: 2, data_slot: false };
        assert_eq!(track_switch(&mut s, leaving, at(1)), None);
        assert_eq!((s.phase(), s.epoch()), (Phase::Online, 0));
        let ended = luyaz_esim::SwitchEvent::Ended { reason: String::new(), rebooting: false };
        assert_eq!(track_switch(&mut s, ended, at(40)), Some((0, String::new(), false)));
        for n in [41, 200, 900] {
            s.observe(card.clone(), true, at(n));
            assert_eq!((s.phase(), s.epoch()), (Phase::Online, 0));
        }
        // the same switch on the data slot does leave
        let leaving = luyaz_esim::SwitchEvent::Leaving { target: "8985".into(), slot: 2, data_slot: true };
        track_switch(&mut s, leaving, at(901));
        assert_eq!((s.phase(), s.epoch()), (Phase::Leaving, 1));
    }

    #[test]
    fn the_eid_of_a_get() {
        assert_eq!(eid_of(Some("eid=89049032ABCDEF".into())).as_deref(), Some("89049032ABCDEF"));
        assert_eq!(eid_of(Some("x=1&eid=8904".into())).as_deref(), Some("8904"));
        assert_eq!(eid_of(Some("eid=".into())), None);
        assert_eq!(eid_of(Some("eid=../etc".into())), None);
        assert_eq!(eid_of(None), None);
        assert_eq!(eid_of(Some("slot=2".into())).as_deref(), Some("@2"));
        assert_eq!(eid_of(Some("slot=9".into())), None);
        assert_eq!(eid_of(Some("eid=8904&slot=2".into())).as_deref(), Some("8904"));
    }
}
