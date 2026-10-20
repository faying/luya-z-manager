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
//!
//! The other way in (§2: passive transfer ≥ 60 s): with no deep check to read, a strong signal
//! carrying real traffic for [`BUSY_S`] only earns the suggestion to run one. Whether that
//! transfer is slow is never judged here: there is no same-cell rate baseline, and the
//! suggestion sends nothing by itself.

use serde::Serialize;

use crate::diagnose::Level;

/// A deep check older than this describes another moment.
pub const MAX_AGE_S: u64 = 600;
/// Strong signal with real traffic this long, in a row…
pub const BUSY_S: u64 = 60;
/// …at least this many bytes a second (1 Mbit/s) at every sample.
pub const BUSY_MIN_BPS: i64 = 125_000;
/// Samples further apart than this break the run.
pub const BUSY_GAP_S: u64 = 15;

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
    /// Seconds the signal has been strong with real traffic, up to now ([`Busy`]).
    pub busy_s: Option<u64>,
}

/// The passive run: fed by the core's sampler, every few seconds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Busy {
    since: Option<u64>,
    last: Option<u64>,
}

impl Busy {
    pub const fn new() -> Self {
        Self {
            since: None,
            last: None,
        }
    }

    /// One sample at `now` (monotonic s): the signal layer's level on the live reading and the
    /// downlink bytes a second. Anything else, unread, or a gap starts over.
    pub fn observe(&mut self, now: u64, signal: Option<Level>, rx_bps: Option<i64>) {
        let on = signal == Some(Level::Ok) && rx_bps.is_some_and(|b| b >= BUSY_MIN_BPS);
        let gap = self.last.is_none_or(|l| now.saturating_sub(l) > BUSY_GAP_S);
        if !on {
            *self = Self::default();
            return;
        }
        if gap {
            self.since = Some(now);
        }
        self.last = Some(now);
    }

    /// How long the run has lasted, if it is still going at `now`.
    pub fn seconds(&self, now: u64) -> Option<u64> {
        let (s, l) = (self.since?, self.last?);
        (now.saturating_sub(l) <= BUSY_GAP_S).then(|| l - s)
    }
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
    /// No deep check to read, but a strong signal has carried real traffic for `busy_s`: if it
    /// feels slow, run one. Nothing is judged slow and nothing is sent.
    Suggest { busy_s: u64, step: &'static str },
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
    // no check of this moment and cell to read: the passive way in, or unknown
    let unknown = |why| match e.busy_s {
        Some(s) if s >= BUSY_S => Advice::Suggest {
            busy_s: s,
            step: "deep_check",
        },
        _ => Advice::Unknown { why },
    };
    match e.done {
        None => return unknown("no_check"),
        Some(false) => {
            return Advice::Unknown {
                why: "check_running",
            };
        }
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
        _ => {
            return Advice::Unknown {
                why: "signal_unknown",
            };
        }
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
        return Advice::Unknown {
            why: "no_layer_explains",
        };
    }
    Advice::Explain { proven, possible }
}

#[cfg(test)]
mod tests;
