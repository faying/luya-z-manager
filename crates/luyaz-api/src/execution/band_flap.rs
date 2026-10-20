//! `radio.band_flap` replay (decision-execution-loop.md §2): from one window of drop history to
//! an explanation for the person, or unknown. Read-only for good: it never carries an action.
//! Locking a band or cell needs the person's own confirmation and a network transaction each
//! time, and a drop never escalates to a modem reboot.

use serde::Serialize;

/// The evidence for one window. None = not read / can't tell.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Evidence {
    /// Independent drops in the last [`WINDOW_S`] on this band (one redial = one drop; drops
    /// within [`MERGE_S`] of each other count once).
    pub drops: Option<u32>,
    /// The band the drops happened on (`B3`, `n78`); None when the drops disagree.
    pub band: Option<String>,
    /// Every drop in the window is on the same card.
    pub same_sim: Option<bool>,
    /// The connection epochs the drops are counted from are authoritative (not reset, not
    /// guessed from a counter).
    pub epochs_authoritative: Option<bool>,
    /// Seconds the window actually covers (gaps in the history make it shorter).
    pub covered_s: Option<u64>,
    /// The device moved between cells in the window (several serving cells / handovers).
    pub moving: Option<bool>,
    /// SINR was weak around the drops.
    pub weak_sinr: Option<bool>,
    /// A drop overlaps an expected outage (no SIM, airplane, data off) or our own transaction.
    pub expected_overlap: Option<bool>,
    /// The stock daemon's own recovery (modem restart, counter reset) explains a drop.
    pub stock_recovery: Option<bool>,
}

pub const WINDOW_S: u64 = 30 * 60;
pub const MIN_DROPS: u32 = 3;
pub const MERGE_S: u64 = 10;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "advice", rename_all = "snake_case")]
pub enum Advice {
    /// Nothing to say (too few drops, another explanation).
    None { why: &'static str },
    /// Can't tell; says why.
    Unknown { why: &'static str },
    /// Explain the pattern and suggest the person review the mode/band policy themselves.
    Explain { band: String, drops: u32 },
}

impl Advice {
    /// Never an action, not even with an approval (§2: no automatic band or cell lock).
    pub fn action(&self) -> Option<&'static str> {
        None
    }
}

pub fn replay(e: &Evidence) -> Advice {
    let unknown = |why| Advice::Unknown { why };
    match e.epochs_authoritative {
        Some(true) => {}
        _ => return unknown("epochs_not_authoritative"),
    }
    match e.same_sim {
        None => return unknown("sim_unreadable"),
        Some(false) => return Advice::None { why: "sim_changed" },
        _ => {}
    }
    let drops = match e.drops {
        None => return unknown("no_drop_history"),
        Some(d) if d < MIN_DROPS => {
            return Advice::None {
                why: "too_few_drops",
            };
        }
        Some(d) => d,
    };
    match e.covered_s {
        Some(c) if c >= WINDOW_S => {}
        _ => return unknown("window_incomplete"),
    }
    // Each alternative cause: unread = unknown, present = not this scenario.
    for (v, unread, present) in [
        (e.expected_overlap, "expected_unknown", "expected_outage"),
        (e.stock_recovery, "stock_recovery_unknown", "stock_recovery"),
        (e.moving, "movement_unknown", "moving"),
        (e.weak_sinr, "sinr_unknown", "weak_sinr"),
    ] {
        match v {
            None => return unknown(unread),
            Some(true) => return Advice::None { why: present },
            Some(false) => {}
        }
    }
    match &e.band {
        Some(b) if !b.is_empty() => Advice::Explain {
            band: b.clone(),
            drops,
        },
        _ => unknown("band_mixed_or_unknown"),
    }
}

#[cfg(test)]
mod tests;
