//! Alerts for the SIM/eSIM path (TODOS P1 警报种类, 10-22): a card change, a transition stuck
//! without the bearer, a failed eSIM switch, and the reboot an eSIM switch asked for.
//!
//! Each is raised once per SIM identity epoch (`sim_identity`): the event id is this boot's
//! id (its first 8 hex) and `epoch * 10 + kind`, so [`luyaz_api::alerts::Alerts::add_once`]
//! drops a repeat even across a core restart. The epoch starts again at 0 each boot, hence
//! the boot id in the key. The reboot itself fires 2 s after the switch job ends, so it is
//! told on the next boot, from the planned-reboot marker (`longterm::boot::plan("esim-identity")`).

use std::path::Path;
use std::sync::Mutex;

pub const CHANGED: &str = "sim-changed";
pub const STALLED: &str = "sim-stalled";
pub const ESIM_FAILED: &str = "esim-failed";
pub const ESIM_REBOOTED: &str = "esim-rebooted";

/// The event id for `kind` (1–4) in `epoch` of the boot `boot_id`; `None` = no boot id.
pub fn event_id(boot_id: &str, epoch: u64, kind: u64) -> Option<String> {
    let b = boot_id.get(..8)?;
    b.bytes()
        .all(|c| c.is_ascii_hexdigit())
        .then(|| format!("{}-{}", b.to_ascii_lowercase(), epoch * 10 + kind))
}

fn raise(epoch: u64, code: u64, kind: &str, text: &str) {
    let Some(id) = event_id(&crate::longterm::boot::proc_boot_id(), epoch, code) else {
        return;
    };
    raise_id(&id, kind, text);
}

/// An event not tied to the data card's epoch (the card table's, dual-slot D2/D3): its id is
/// this boot's id and `key`, so a repeat with the same key in this boot is dropped.
pub fn note(key: &str, kind: &str, text: &str) {
    let boot = crate::longterm::boot::proc_boot_id();
    let Some(b) = boot.get(..8).filter(|b| b.bytes().all(|c| c.is_ascii_hexdigit())) else {
        return;
    };
    raise_id(&format!("{}-{key}", b.to_ascii_lowercase()), kind, text);
}

fn raise_id(id: &str, kind: &str, text: &str) {
    let dir = crate::state::host_path(luyaz_api::alerts::DIR);
    let a = luyaz_api::alerts::Alerts::at(Path::new(&dir));
    let up = std::fs::read_to_string(crate::state::host_path("/proc/uptime"))
        .ok()
        .and_then(|s| s.split('.').next()?.parse::<u64>().ok())
        .unwrap_or(0);
    match a.add_once(id, kind, text, luyaz_api::alerts::now(), up) {
        Ok((_, true)) => eprintln!("[sim_alerts] {kind}: {text}"),
        Ok(_) => {}
        Err(e) => eprintln!("[sim_alerts] {kind}: {e}"),
    }
}

/// The last phase seen, so the queue is only looked at when it moves.
static LAST: Mutex<&str> = Mutex::new("online");

/// Each snapshot round: the epoch and phase `sim_identity` is in now.
pub fn phase(epoch: u64, phase: &'static str) {
    {
        let mut l = LAST.lock().unwrap_or_else(|e| e.into_inner());
        if *l == phase {
            return;
        }
        *l = phase;
    }
    if let Some((code, kind, text)) = phase_alert(epoch, phase) {
        raise(epoch, code, kind, &text);
    }
}

/// The alert a phase raises: (event code, kind, text); `None` for the quiet phases.
fn phase_alert(epoch: u64, phase: &str) -> Option<(u64, &'static str, String)> {
    match phase {
        "changed" => Some((1, CHANGED, format!("SIM identity changed (epoch {epoch})"))),
        "stalled" => Some((
            2,
            STALLED,
            format!("SIM change stuck without data (epoch {epoch})"),
        )),
        _ => None,
    }
}

/// A switch job ended (`SwitchEvent::Ended`). A cancel is the user's own; a job that
/// scheduled the reboot is told on the next boot ([`boot`]).
pub fn switch_ended(epoch: u64, reason: &str, rebooting: bool) {
    if rebooting || reason.is_empty() || reason == "cancelled" {
        return;
    }
    raise(
        epoch,
        3,
        ESIM_FAILED,
        &format!("eSIM switch failed: {reason}"),
    );
}

/// Once at start: this boot is the one an eSIM switch asked for.
pub fn boot() {
    if let crate::longterm::boot::Planned::Ours(w) = crate::longterm::boot::planned_now()
        && w == "esim-identity"
    {
        raise(
            0,
            4,
            ESIM_REBOOTED,
            "Rebooted automatically to finish an eSIM switch",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_id_keys_boot_epoch_and_kind() {
        let b = "3F2A9C01-1111-2222-3333-444455556666";
        assert_eq!(event_id(b, 4, 1).as_deref(), Some("3f2a9c01-41"));
        assert_ne!(event_id(b, 4, 1), event_id(b, 5, 1));
        assert_ne!(event_id(b, 4, 1), event_id(b, 4, 2));
        assert_ne!(
            event_id(b, 4, 1),
            event_id("0badf00d-1111-2222-3333-444455556666", 4, 1)
        );
        assert!(luyaz_api::alerts::valid_event_id(
            &event_id(b, 4, 3).unwrap()
        ));
        assert_eq!(event_id("", 1, 1), None);
        assert_eq!(event_id("not-hex!-x", 1, 1), None);
    }

    /// Dual-slot regression contract (dual-slot-removable.md R6/D8 ④): a single-slot device
    /// that has its card pulled and another put in raises `sim-changed` once, with this exact
    /// text (the SMS/inbox wording is pinned in luyaz-alerts). The card table must not change it.
    #[test]
    fn a_single_slot_card_swap_raises_sim_changed_as_before() {
        use crate::sim_identity::{Identity, SimIdentity};
        let t0 = std::time::Instant::now();
        let at = |s| t0 + std::time::Duration::from_secs(s);
        let mut s = SimIdentity::default();
        let a = Identity::new("89860100000000000011", "460010000000001");
        let b = Identity::new("89860300000000000022", "460110000000002");
        s.observe(a.clone(), true, at(0));
        s.observe(Identity::new("", ""), false, at(10));
        assert_eq!((s.phase().as_str(), s.epoch()), ("leaving", 1));
        assert_eq!(phase_alert(s.epoch(), s.phase().as_str()), None);
        s.observe(b, false, at(30));
        assert_eq!((s.phase().as_str(), s.epoch()), ("changed", 1));
        assert_eq!(
            phase_alert(s.epoch(), s.phase().as_str()),
            Some((
                1,
                "sim-changed",
                "SIM identity changed (epoch 1)".to_string()
            ))
        );
        assert_eq!(
            phase_alert(2, "stalled"),
            Some((
                2,
                "sim-stalled",
                "SIM change stuck without data (epoch 2)".to_string()
            ))
        );
        assert_eq!(phase_alert(1, "online"), None);
    }
}
