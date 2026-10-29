//! Heat care (`docs/designs/thermal-care.md`): how hot the device is, and what that asks for.
//!
//! The stock battery driver already cuts the charge current at 45 °C and stops charging at 55 °C;
//! this adds the load side (CHILL to eco, then direct) and a pause that a manual resume can beat.
//! Thresholds sit close to the stock bands (owner 10-29: the loose set).
//!
//! [`Tracker::step`] takes one reading (about every 30 s): a level is entered after
//! [`Limits::enter_reads`] readings in a row at it or above, and left only after every reading for
//! [`Limits::cool_hold`] has been under that level's exit line. A reading that cannot be had
//! (`None`) never enters a level and never counts as cool.

use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Level {
    #[default]
    Normal,
    /// The battery is warm while plugged in: pause charging.
    Warm,
    /// Hot battery or chip: pause charging, CHILL to eco.
    Hot,
    /// Close to the stock limits: CHILL direct, an incident (texted).
    Critical,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Normal => "normal",
            Level::Warm => "warm",
            Level::Hot => "hot",
            Level::Critical => "critical",
        }
    }
}

/// Degrees Celsius throughout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub warm_batt: i32,
    pub warm_batt_exit: i32,
    pub hot_batt: i32,
    pub hot_chip: i32,
    pub hot_batt_exit: i32,
    pub hot_chip_exit: i32,
    pub critical_batt: i32,
    pub critical_chip: i32,
    pub critical_batt_exit: i32,
    pub critical_chip_exit: i32,
    /// At or under this charge a warm/hot pause gives way (pausing drains the battery: no bypass).
    pub low_battery: u8,
    pub enter_reads: u8,
    pub cool_hold: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            warm_batt: 44,
            warm_batt_exit: 40,
            hot_batt: 50,
            hot_chip: 80,
            hot_batt_exit: 46,
            hot_chip_exit: 73,
            critical_batt: 55,
            critical_chip: 90,
            critical_batt_exit: 51,
            critical_chip_exit: 83,
            low_battery: 30,
            enter_reads: 2,
            cool_hold: Duration::from_secs(300),
        }
    }
}

/// One look at the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Reading {
    /// Battery °C.
    pub batt: Option<i32>,
    /// The hottest of the CPU, modem and PMIC zones, °C.
    pub chip: Option<i32>,
    /// A charger is connected (`None`: not known, taken as unplugged).
    pub plugged: Option<bool>,
}

/// The highest level this one reading reaches.
pub fn reached(r: &Reading, l: &Limits) -> Level {
    let b = r.batt.unwrap_or(i32::MIN);
    let c = r.chip.unwrap_or(i32::MIN);
    if b >= l.critical_batt || c >= l.critical_chip {
        Level::Critical
    } else if b >= l.hot_batt || c >= l.hot_chip {
        Level::Hot
    } else if b >= l.warm_batt && r.plugged == Some(true) {
        Level::Warm
    } else {
        Level::Normal
    }
}

/// Under `level`'s exit line: `None` readings are not cool. Warm also ends on unplugging.
fn cool(level: Level, r: &Reading, l: &Limits) -> bool {
    let under = |v: Option<i32>, line: i32| v.is_some_and(|v| v <= line);
    match level {
        Level::Normal => true,
        Level::Warm => r.plugged != Some(true) || under(r.batt, l.warm_batt_exit),
        Level::Hot => under(r.batt, l.hot_batt_exit) && under(r.chip, l.hot_chip_exit),
        Level::Critical => {
            under(r.batt, l.critical_batt_exit) && under(r.chip, l.critical_chip_exit)
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tracker {
    pub level: Level,
    /// Readings in a row above `level`, and the lowest level they reached.
    rising: u8,
    rising_to: Level,
    /// When the readings went under `level`'s exit line (monotonic).
    cool_since: Option<Duration>,
}

impl Tracker {
    /// One reading at monotonic `now`; returns the level after it.
    pub fn step(&mut self, now: Duration, r: &Reading, l: &Limits) -> Level {
        let hit = reached(r, l);
        if hit > self.level {
            self.rising_to = if self.rising == 0 {
                hit
            } else {
                self.rising_to.min(hit)
            };
            self.rising = self.rising.saturating_add(1);
            self.cool_since = None;
            if self.rising >= l.enter_reads.max(1) {
                self.level = self.rising_to;
                self.rising = 0;
            }
            return self.level;
        }
        self.rising = 0;
        if self.level == Level::Normal {
            return self.level;
        }
        // Warm ends at once on unplugging: there is no charging left to pause.
        if self.level == Level::Warm && r.plugged != Some(true) && hit == Level::Normal {
            self.level = Level::Normal;
            self.cool_since = None;
            return self.level;
        }
        if !cool(self.level, r, l) {
            self.cool_since = None;
            return self.level;
        }
        let since = *self.cool_since.get_or_insert(now);
        if now.saturating_sub(since) >= l.cool_hold {
            self.level = hit;
            self.cool_since = None;
        }
        self.level
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Chill {
    #[default]
    Leave,
    Eco,
    Direct,
}

impl Chill {
    /// What the core writes for chill.sh (`/tmp/luyaz/heat`); empty = leave CHILL alone.
    pub fn as_str(self) -> &'static str {
        match self {
            Chill::Leave => "",
            Chill::Eco => "hot",
            Chill::Direct => "critical",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Actions {
    pub pause_charge: bool,
    pub chill: Chill,
    /// `regress-heat` open (texted).
    pub incident: bool,
}

/// What `level` asks for. `capacity`: battery % (`None`: unknown, no floor). `manual_resume`:
/// the owner resumed charging during this plug-in (owner 10-29: it wins until unplugged, except
/// at critical, where the stock driver stops charging too).
pub fn actions(level: Level, capacity: Option<u8>, manual_resume: bool, l: &Limits) -> Actions {
    let low = capacity.is_some_and(|c| c <= l.low_battery);
    match level {
        Level::Normal => Actions::default(),
        Level::Warm => Actions {
            pause_charge: !manual_resume && !low,
            ..Actions::default()
        },
        Level::Hot => Actions {
            pause_charge: !manual_resume && !low,
            chill: Chill::Eco,
            incident: false,
        },
        Level::Critical => Actions {
            pause_charge: true,
            chill: Chill::Direct,
            incident: true,
        },
    }
}

#[cfg(test)]
mod tests;
