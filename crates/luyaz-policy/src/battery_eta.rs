//! Battery time estimate (manager docs/battery-estimate.md), moved from zte-agent
//! `src/battery_eta.rs` (manager 7d2ca07). `average`/`estimate` are the policy (what the
//! battery-eta component exports, `wit/battery-eta.wit`); `Tracker` is the host's sample buffer
//! and report, the agent's `BatteryEta` without its own sampler thread: u60d feeds it on the
//! battery block's read (every 5 s, the agent's `SAMPLE_EVERY`), so no new wakeup.
//!
//! Deliberate differences from the agent: time is seconds on the host's monotonic clock (`f64`)
//! instead of `Instant`, and the estimate is a parameter of `feed` so the host can pass the
//! component's. Outputs are the same.

use std::collections::VecDeque;

pub const WINDOW_SECS: f64 = 180.0;
pub const MIN_CURRENT_UA: f64 = 50_000.0;
/// A report older than this means the sampler stopped.
pub const STALE_AFTER_SECS: f64 = 20.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    /// Seconds, monotonic.
    pub t: f64,
    /// Battery current in µA, positive = into the battery.
    pub ua: i64,
    /// Charger plugged in.
    pub online: bool,
}

#[derive(Clone, Debug)]
pub struct Input<'a> {
    pub samples: &'a [Sample],
    pub soc: i64,
    pub charge_full_uah: Option<i64>,
    pub charge_counter_uah: Option<i64>,
    pub target_pct: i64,
    pub paused_at_limit: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum Kind {
    ChargingEta,
    DischargingEta,
    ReachedTarget,
    PausedAtLimit,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Estimate {
    pub kind: Kind,
    /// Only for `ChargingEta` / `DischargingEta`.
    pub minutes: Option<i64>,
}

/// Time-weighted average over the current window; `None` without samples.
pub fn average(samples: &[Sample]) -> Option<f64> {
    let last = *samples.last()?;
    let positive = last.ua >= 0;
    let mut start = samples.len() - 1;
    while start > 0 {
        let s = samples[start - 1];
        if s.t < last.t - WINDOW_SECS || (s.ua >= 0) != positive || s.online != last.online {
            break;
        }
        start -= 1;
    }
    let (mut sum, mut weight) = (0.0, 0.0);
    for k in start..samples.len() - 1 {
        let w = samples[k + 1].t - samples[k].t;
        sum += samples[k].ua as f64 * w;
        weight += w;
    }
    Some(if weight > 0.0 {
        sum / weight
    } else {
        last.ua as f64
    })
}

/// Half away from zero for the positive minutes we produce (JS `Math.round`,
/// C `floor(x + 0.5)`).
fn round_minutes(x: f64) -> i64 {
    (x + 0.5).floor() as i64
}

pub fn estimate(input: &Input) -> Estimate {
    let unknown = Estimate {
        kind: Kind::Unknown,
        minutes: None,
    };
    let Some(avg) = average(input.samples) else {
        return unknown;
    };
    let online = input.samples.last().map(|s| s.online).unwrap_or(false);
    if input.paused_at_limit {
        return Estimate {
            kind: Kind::PausedAtLimit,
            minutes: None,
        };
    }
    if online && input.soc >= input.target_pct && (avg >= 0.0 || avg.abs() < MIN_CURRENT_UA) {
        return Estimate {
            kind: Kind::ReachedTarget,
            minutes: None,
        };
    }
    if avg.abs() < MIN_CURRENT_UA {
        return unknown;
    }
    if avg > 0.0 {
        let Some(full) = input.charge_full_uah else {
            return unknown;
        };
        let need = full as f64 * (input.target_pct - input.soc) as f64 / 100.0;
        return Estimate {
            kind: Kind::ChargingEta,
            minutes: Some(round_minutes(need / avg * 60.0)),
        };
    }
    let left = match (input.charge_counter_uah, input.charge_full_uah) {
        (Some(c), _) => c as f64,
        (None, Some(full)) => full as f64 * input.soc as f64 / 100.0,
        (None, None) => return unknown,
    };
    Estimate {
        kind: Kind::DischargingEta,
        minutes: Some(round_minutes(left / -avg * 60.0)),
    }
}

/// Appends a sample and drops the ones the estimate can no longer use.
fn push(buf: &mut VecDeque<Sample>, s: Sample) {
    buf.push_back(s);
    let cut = s.t - WINDOW_SECS;
    while buf.len() > 1 && buf.front().is_some_and(|f| f.t < cut) {
        buf.pop_front();
    }
}

/// What the endpoints return (agent `/api/battery`'s `estimate`, `/api/screen`'s `battery`).
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Report {
    /// `ok` · `estimating` (no sample yet) · `stale` (sampler stopped) ·
    /// `unavailable` (no battery in sysfs).
    pub state: &'static str,
    pub kind: Kind,
    pub minutes: Option<i64>,
    /// 100, or the charge limit while it is on.
    pub target_pct: i64,
    /// Device clock, seconds (same base as the core's `ts`).
    pub observed_at: u64,
    /// Samples in the buffer.
    pub samples: usize,
}

/// One tick's readings; `None` = could not read.
#[derive(Clone, Debug, Default)]
pub struct Reading {
    pub soc: Option<i64>,
    pub current_ua: Option<i64>,
    pub charge_full_uah: Option<i64>,
    pub charge_counter_uah: Option<i64>,
    pub charger_connected: Option<bool>,
    pub limit_enabled: bool,
    pub limit_pct: i64,
    pub charging_stopped: Option<bool>,
    pub manual_override: bool,
}

/// The sample buffer and the latest report.
#[derive(Default)]
pub struct Tracker {
    samples: VecDeque<Sample>,
    /// (monotonic seconds, report)
    last: Option<(f64, Report)>,
}

impl Tracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one reading taken at `now` (monotonic seconds) / `wall` (device clock); `eval` is the
    /// policy (`estimate`, or the component's).
    pub fn feed(
        &mut self,
        r: &Reading,
        now: f64,
        wall: u64,
        eval: &mut dyn FnMut(&Input) -> Estimate,
    ) -> Report {
        // a limit outside 50–100 is not a limit (as the touch screen treated it)
        let target_pct = if r.limit_enabled && (50..=100).contains(&r.limit_pct) {
            r.limit_pct
        } else {
            100
        };
        let report = match (r.soc, r.current_ua) {
            (Some(soc), Some(ua)) => {
                let online = r
                    .charger_connected
                    .unwrap_or_else(|| self.samples.back().is_some_and(|s| s.online));
                push(&mut self.samples, Sample { t: now, ua, online });
                let samples: Vec<Sample> = self.samples.iter().copied().collect();
                // unknown plug state counts as plugged, like the web did: the
                // limit only stops charging while a charger is there
                let paused = r.limit_enabled
                    && r.charging_stopped == Some(true)
                    && !r.manual_override
                    && r.charger_connected != Some(false);
                let e = eval(&Input {
                    samples: &samples,
                    soc,
                    charge_full_uah: r.charge_full_uah,
                    charge_counter_uah: r.charge_counter_uah,
                    target_pct,
                    paused_at_limit: paused,
                });
                Report {
                    state: "ok",
                    kind: e.kind,
                    minutes: e.minutes,
                    target_pct,
                    observed_at: wall,
                    samples: samples.len(),
                }
            }
            _ => Report {
                state: "unavailable",
                kind: Kind::Unknown,
                minutes: None,
                target_pct,
                observed_at: wall,
                samples: self.samples.len(),
            },
        };
        self.last = Some((now, report.clone()));
        report
    }

    /// Latest report, marked `stale` when the sampler stopped.
    pub fn report(&self, now: f64) -> Report {
        match &self.last {
            None => Report {
                state: "estimating",
                kind: Kind::Unknown,
                minutes: None,
                target_pct: 100,
                observed_at: 0,
                samples: 0,
            },
            Some((at, r)) if now - *at > STALE_AFTER_SECS => Report {
                state: "stale",
                ..r.clone()
            },
            Some((_, r)) => r.clone(),
        }
    }

    pub fn samples(&self) -> impl Iterator<Item = &Sample> {
        self.samples.iter()
    }
}

#[cfg(test)]
mod tests;
