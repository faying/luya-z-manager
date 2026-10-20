//! `power.hot_drain` replay (decision-execution-loop.md §2): from one closed ledger hour and the
//! same device's matched history to an explanation for the person, or unknown. Read-only for
//! good: it never carries an action. Heat never leads to LPM, stopping stock services, cutting an
//! exit or a Wi-Fi change; any charging or network write keeps its own confirmation.
//!
//! Both halves are needed: the battery is hotter than its normal range in the same condition
//! (screen on/off × home/abroad, not charging), **and** a qualified counter discharge rate
//! (`counter_quality` cells, never the hour's unscreened `e`) is well above the same condition's
//! normal. The factors that ran alongside are listed as correlations, not as the cause. Ambient
//! temperature is never read, so a hot room or a bag can't be excluded: every explanation says so.

use serde::Serialize;

/// The window's main condition must cover at least this much of the hour.
pub const MIN_WINDOW_S: u64 = 30 * 60;
/// Matched hours needed for a normal range (§1.3: at least three).
pub const MIN_HOURS: u32 = 3;
/// Hot = at least this far (tenths of °C) above the matched hours' 90th percentile.
pub const HOT_MARGIN_DC: i64 = 30;
/// Fast drain = at least this many times the matched hours' median.
pub const DRAIN_RATIO: f64 = 1.5;

/// What else ran in the window, each against the same condition's matched hours.
/// None = not read / no baseline to compare with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Factors {
    /// Wi-Fi clients were connected (screen off at home only: the ledger has no other count).
    pub clients: Option<bool>,
    /// Cellular packets above the matched hours' 90th percentile (screen-off cells only).
    pub cellular: Option<bool>,
    /// System CPU above the matched hours' 90th percentile.
    pub cpu: Option<bool>,
    /// A cpufreq policy was capped this hour.
    pub throttled: Option<bool>,
    /// The application-side thermal zones above the matched hours' 90th percentile.
    pub chip_hot: Option<bool>,
    /// The fuel gauge says the battery's health is not `Good`.
    pub battery_health: Option<bool>,
}

/// The evidence for one window. None = not read / can't tell.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Evidence {
    /// The window is this boot's last closed hour and ended recently enough.
    pub fresh: Option<bool>,
    /// Seconds of the window in its main condition.
    pub window_s: Option<u64>,
    /// Charging or USB power during the window (heat while charging is not this scenario).
    pub charging: Option<bool>,
    /// The window's highest battery temperature, tenths of °C.
    pub batt_dc: Option<i64>,
    /// The matched hours' 90th percentile, tenths of °C.
    pub batt_normal_dc: Option<i64>,
    pub temp_hours: u32,
    /// Qualified counter discharge in the window's condition (≥ [`MIN_WINDOW_S`] screened), mW.
    pub drain_mw: Option<u32>,
    /// The matched qualified hours' median, mW.
    pub drain_normal_mw: Option<u32>,
    pub drain_hours: u32,
    pub factors: Factors,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "advice", rename_all = "snake_case")]
pub enum Advice {
    /// Nothing to say (charging, not hot, drain normal).
    None { why: &'static str },
    /// Can't tell; says why.
    Unknown { why: &'static str },
    /// Hot and draining fast; what ran alongside, and what could not be read.
    Explain {
        batt_dc: i64,
        batt_normal_dc: i64,
        drain_mw: u32,
        drain_normal_mw: u32,
        alongside: Vec<&'static str>,
        unread: Vec<&'static str>,
        /// Always `unread`: no ambient sensor, so the surroundings are never excluded.
        ambient: &'static str,
    },
}

impl Advice {
    /// Never an action, not even with an approval (§2: explain first, any energy-saving setting
    /// is the person's own, confirmed and reversible).
    pub fn action(&self) -> Option<&'static str> {
        None
    }
}

pub fn replay(e: &Evidence) -> Advice {
    let unknown = |why| Advice::Unknown { why };
    if e.fresh != Some(true) {
        return unknown("window_stale");
    }
    match e.charging {
        None => return unknown("charging_unknown"),
        Some(true) => return Advice::None { why: "charging" },
        Some(false) => {}
    }
    // charging seconds are in no condition: a charging hour has no window to speak of
    match e.window_s {
        Some(s) if s >= MIN_WINDOW_S => {}
        _ => return unknown("window_incomplete"),
    }
    let Some(batt) = e.batt_dc else {
        return unknown("no_temperature");
    };
    let normal = match e.batt_normal_dc {
        Some(n) if e.temp_hours >= MIN_HOURS => n,
        _ => return unknown("no_temperature_baseline"),
    };
    if batt < normal + HOT_MARGIN_DC {
        return Advice::None { why: "not_hot" };
    }
    let Some(drain) = e.drain_mw else {
        return unknown("drain_unqualified");
    };
    let drain_normal = match e.drain_normal_mw {
        Some(n) if e.drain_hours >= MIN_HOURS => n,
        _ => return unknown("no_drain_baseline"),
    };
    if f64::from(drain) < f64::from(drain_normal) * DRAIN_RATIO {
        return Advice::None {
            why: "drain_normal",
        };
    }
    let f = &e.factors;
    let (mut alongside, mut unread) = (Vec::new(), Vec::new());
    for (v, name) in [
        (f.clients, "clients"),
        (f.cellular, "cellular"),
        (f.cpu, "cpu"),
        (f.throttled, "throttled"),
        (f.chip_hot, "chip_hot"),
        (f.battery_health, "battery_health"),
    ] {
        match v {
            Some(true) => alongside.push(name),
            None => unread.push(name),
            Some(false) => {}
        }
    }
    Advice::Explain {
        batt_dc: batt,
        batt_normal_dc: normal,
        drain_mw: drain,
        drain_normal_mw: drain_normal,
        alongside,
        unread,
        ambient: "unread",
    }
}

#[cfg(test)]
mod tests;
