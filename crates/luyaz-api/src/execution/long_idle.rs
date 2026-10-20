//! `usage.long_idle` replay (decision-execution-loop.md §2): from the last closed ledger hours to
//! a suggestion for the person, or unknown. Read-only for good: it never carries an action. No
//! automatic LPM, never the only Wi-Fi or remote way in, never the sentinel or stock daemons.
//!
//! Idle = at least [`MIN_HOURS`] adjacent, fully covered hours with the screen off, not charging,
//! no clients and next to no cellular traffic, nothing of ours running now, and the SMS and
//! watchdog states known. Then the only suggestion is the existing, verified saving step: the
//! CHILL idle pause (u60-radio.md 6e), which wakes by itself when the screen or a client comes
//! back. Already on, or no CHILL to pause = nothing to suggest.

use serde::Serialize;

/// Quiet closed hours needed (§2: at least two complete quiet hours).
pub const MIN_HOURS: usize = 2;
/// "Next to no traffic" (screen off at home): at most this many cellular packets in the hour…
pub const MAX_PKTS: i64 = 10_000;
/// …and at least this share (per mille) of the hour in zero-packet runs.
pub const MIN_QUIET_PM: i64 = 400;

/// One closed hour. None = not read / can't tell.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hour {
    /// Every watched source was up for the whole hour and its seconds add up.
    pub covered: Option<bool>,
    /// It starts where the hour before it ended (no restart or clock gap in between).
    pub adjacent: Option<bool>,
    pub charging: Option<bool>,
    pub screen_on: Option<bool>,
    /// No clients all hour (screen off at home only: roaming hours have no client count).
    pub no_clients: Option<bool>,
    /// At most [`MAX_PKTS`] packets and at least [`MIN_QUIET_PM`] quiet.
    pub low_traffic: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Evidence {
    /// The last hour is this boot's and ended recently enough.
    pub fresh: Option<bool>,
    /// The last closed hours, oldest first (the replay looks at the last [`MIN_HOURS`]).
    pub hours: Vec<Hour>,
    /// How many closed hours in a row, back from the last, were quiet (for the words only).
    pub quiet_run: u32,
    /// One of our transactions is running now.
    pub txn_running: Option<bool>,
    /// The SMS list is readable (not stale).
    pub sms_known: Option<bool>,
    /// The sentinel's heartbeat is fresh.
    pub watchdog_ok: Option<bool>,
    /// CHILL is meant on (something the idle pause could stop).
    pub chill_on: Option<bool>,
    /// The CHILL idle pause is on.
    pub idle_pause_on: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "advice", rename_all = "snake_case")]
pub enum Advice {
    /// Nothing to say (in use, already saving, nothing to save).
    None { why: &'static str },
    /// Can't tell; says why.
    Unknown { why: &'static str },
    /// Idle for `hours`: suggest the person turn on the CHILL idle pause themselves.
    Suggest { hours: u32, step: &'static str },
}

impl Advice {
    /// Never an action, not even with an approval (§2: suggest first; only the person's own
    /// pre-configured, reversible CHILL/scenario policy may ever act).
    pub fn action(&self) -> Option<&'static str> {
        None
    }
}

/// Whether one hour was quiet: Ok(()) quiet, Err((unknown?, why)).
pub fn quiet(h: &Hour) -> Result<(), (bool, &'static str)> {
    // (reading, present is still unknown, why unread, why present)
    for (v, present_unknown, unread, present) in [
        // a gap in coverage is not knowledge of use: unknown, not "in use"
        (
            h.covered.map(|c| !c),
            true,
            "coverage_unknown",
            "coverage_gap",
        ),
        (h.charging, false, "charging_unknown", "charging"),
        (h.screen_on, false, "screen_unknown", "in_use"),
        (
            h.no_clients.map(|c| !c),
            false,
            "clients_unknown",
            "clients",
        ),
        (
            h.low_traffic.map(|c| !c),
            false,
            "traffic_unknown",
            "traffic",
        ),
    ] {
        match v {
            None => return Err((true, unread)),
            Some(true) => return Err((present_unknown, present)),
            Some(false) => {}
        }
    }
    Ok(())
}

pub fn replay(e: &Evidence) -> Advice {
    let unknown = |why| Advice::Unknown { why };
    if e.fresh != Some(true) {
        return unknown("window_stale");
    }
    if e.hours.len() < MIN_HOURS {
        return unknown("window_incomplete");
    }
    let last = &e.hours[e.hours.len() - MIN_HOURS..];
    // the first hour's link to the one before it doesn't matter; the rest must chain
    if last[1..].iter().any(|h| h.adjacent != Some(true)) {
        return unknown("clock_gap");
    }
    for h in last {
        match quiet(h) {
            Ok(()) => {}
            Err((true, why)) => return unknown(why),
            Err((false, why)) => return Advice::None { why },
        }
    }
    for (v, unread, present) in [
        (e.txn_running, "txn_unknown", Some("txn_running")),
        (e.sms_known.map(|k| !k), "sms_unknown", None),
        (e.watchdog_ok.map(|k| !k), "watchdog_unknown", None),
    ] {
        match (v, present) {
            (None, _) | (Some(true), None) => return unknown(unread),
            (Some(true), Some(why)) => return Advice::None { why },
            (Some(false), _) => {}
        }
    }
    match e.chill_on {
        None => return unknown("chill_unknown"),
        Some(false) => {
            return Advice::None {
                why: "nothing_to_save",
            };
        }
        Some(true) => {}
    }
    match e.idle_pause_on {
        None => unknown("saving_unknown"),
        Some(true) => Advice::None {
            why: "already_saving",
        },
        Some(false) => Advice::Suggest {
            hours: e.quiet_run.max(MIN_HOURS as u32),
            step: "chill_idle_pause",
        },
    }
}

#[cfg(test)]
mod tests;
