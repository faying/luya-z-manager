//! `perf.slow_strong_signal` replay (decision-execution-loop.md §2): from the person's own deep
//! check (they felt it slow and asked) to which layers are proven bad and which are only
//! possible, or unknown. Read-only for good: it never carries an action. Changing RAT, locking a
//! band or switching the proxy off stays the person's, and a band lock needs their confirmation
//! each time; a speed test is never started from here.
//!
//! A strong signal is only the way in: the deep check's signal layer (RSRP and SINR together,
//! plus the carrier width) must be fine. One SINR reading never names a cause. Cell load is
//! never proven from here (no baseline of real traffic on the same cell), so it is always only
//! "possible", as is every layer the check left at warn.

use serde::Serialize;

use crate::diagnose::Level;

/// A deep check older than this describes another moment.
pub const MAX_AGE_S: u64 = 600;
/// Layers that can explain slowness besides the radio, in the deep check's order.
pub const LAYERS: [&str; 9] = [
    "wifi", "sim", "limit", "bearer", "link", "dns", "e2e", "crowd", "proxy",
];

/// The evidence. None = not read / can't tell.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Evidence {
    /// The last deep check: None = none kept, Some(false) = still running.
    pub done: Option<bool>,
    /// Seconds since it finished.
    pub age_s: Option<u64>,
    /// It ran on the cell serving now.
    pub same_cell: Option<bool>,
    /// The deep check's layers: id → level.
    pub layers: Vec<(String, Level)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "advice", rename_all = "snake_case")]
pub enum Advice {
    /// Nothing to say (the signal is not strong: the deep check's own headline covers it).
    None { why: &'static str },
    /// Can't tell; says why.
    Unknown { why: &'static str },
    /// Signal strong; `proven` layers are bad on the check's own evidence, `possible` ones are
    /// only suspected. Each gets its own manual advice on the page.
    Explain {
        proven: Vec<&'static str>,
        possible: Vec<&'static str>,
    },
}

impl Advice {
    /// Never an action, not even with an approval (§2).
    pub fn action(&self) -> Option<&'static str> {
        None
    }
}

fn level(e: &Evidence, id: &str) -> Option<Level> {
    e.layers.iter().find(|(l, _)| l == id).map(|(_, v)| *v)
}

pub fn replay(e: &Evidence) -> Advice {
    let unknown = |why| Advice::Unknown { why };
    match e.done {
        None => return unknown("no_check"),
        Some(false) => return unknown("check_running"),
        Some(true) => {}
    }
    match e.age_s {
        Some(a) if a <= MAX_AGE_S => {}
        _ => return unknown("check_stale"),
    }
    match e.same_cell {
        None => return unknown("cell_unknown"),
        Some(false) => return unknown("cell_changed"),
        Some(true) => {}
    }
    match level(e, "signal") {
        Some(Level::Ok) => {}
        Some(Level::Warn | Level::Bad) => {
            return Advice::None {
                why: "signal_not_strong",
            };
        }
        _ => return unknown("signal_unknown"),
    }
    let (mut proven, mut possible) = (Vec::new(), Vec::new());
    for id in LAYERS {
        match level(e, id) {
            Some(Level::Bad) if id != "crowd" => proven.push(id),
            Some(Level::Bad | Level::Warn) => possible.push(id),
            _ => {}
        }
    }
    if proven.is_empty() && possible.is_empty() {
        return unknown("no_layer_explains");
    }
    Advice::Explain { proven, possible }
}

#[cfg(test)]
mod tests;
