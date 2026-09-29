//! Battery time estimate (docs/battery-estimate.md). The only implementation:
//! the admin web and the touch screen both show what this computes
//! (`/api/battery`'s `estimate`, `/api/screen`'s `battery`).
//!
//! A sampler thread reads the battery every [`SAMPLE_EVERY`] whether or not
//! anyone is looking, so the estimate is ready the moment a page opens.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::charge_policy::ChargeLimitEnforcer;

pub const WINDOW_SECS: f64 = 180.0;
pub const MIN_CURRENT_UA: f64 = 50_000.0;
const SAMPLE_EVERY: Duration = Duration::from_secs(5);
/// A report older than this means the sampler stopped.
const STALE_AFTER: Duration = Duration::from_secs(20);

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
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
    Some(if weight > 0.0 { sum / weight } else { last.ua as f64 })
}

/// Half away from zero for the positive minutes we produce (JS `Math.round`,
/// C `floor(x + 0.5)`).
fn round_minutes(x: f64) -> i64 {
    (x + 0.5).floor() as i64
}

pub fn estimate(input: &Input) -> Estimate {
    let unknown = Estimate { kind: Kind::Unknown, minutes: None };
    let Some(avg) = average(input.samples) else { return unknown };
    let online = input.samples.last().map(|s| s.online).unwrap_or(false);
    if input.paused_at_limit {
        return Estimate { kind: Kind::PausedAtLimit, minutes: None };
    }
    if online && input.soc >= input.target_pct && (avg >= 0.0 || avg.abs() < MIN_CURRENT_UA) {
        return Estimate { kind: Kind::ReachedTarget, minutes: None };
    }
    if avg.abs() < MIN_CURRENT_UA {
        return unknown;
    }
    if avg > 0.0 {
        let Some(full) = input.charge_full_uah else { return unknown };
        let need = full as f64 * (input.target_pct - input.soc) as f64 / 100.0;
        return Estimate { kind: Kind::ChargingEta, minutes: Some(round_minutes(need / avg * 60.0)) };
    }
    let left = match (input.charge_counter_uah, input.charge_full_uah) {
        (Some(c), _) => c as f64,
        (None, Some(full)) => full as f64 * input.soc as f64 / 100.0,
        (None, None) => return unknown,
    };
    Estimate { kind: Kind::DischargingEta, minutes: Some(round_minutes(left / -avg * 60.0)) }
}

/// Appends a sample and drops the ones the estimate can no longer use.
fn push(buf: &mut VecDeque<Sample>, s: Sample) {
    buf.push_back(s);
    let cut = s.t - WINDOW_SECS;
    while buf.len() > 1 && buf.front().is_some_and(|f| f.t < cut) {
        buf.pop_front();
    }
}

/// What the endpoints return.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Report {
    /// `ok` · `estimating` (no sample yet) · `stale` (sampler stopped) ·
    /// `unavailable` (no battery in sysfs).
    pub state: &'static str,
    pub kind: Kind,
    pub minutes: Option<i64>,
    /// 100, or the charge limit while it is on.
    pub target_pct: i64,
    /// Device clock, seconds (same base as datad's `ts`).
    pub observed_at: u64,
    /// Samples in the buffer.
    pub samples: usize,
}

/// One sampler tick's readings; `None` = could not read.
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

struct Inner {
    samples: VecDeque<Sample>,
    last: Option<(Instant, Report)>,
}

pub struct BatteryEta {
    t0: Instant,
    inner: Mutex<Inner>,
}

impl Default for BatteryEta {
    fn default() -> Self {
        Self::new()
    }
}

impl BatteryEta {
    pub fn new() -> Self {
        BatteryEta { t0: Instant::now(), inner: Mutex::new(Inner { samples: VecDeque::new(), last: None }) }
    }

    /// Feed one reading taken at `now` (monotonic) / `wall` (device clock).
    pub fn feed(&self, r: &Reading, now: Instant, wall: u64) -> Report {
        let mut g = self.inner.lock().unwrap();
        // a limit outside 50–100 is not a limit (as the touch screen treated it)
        let target_pct = if r.limit_enabled && (50..=100).contains(&r.limit_pct) { r.limit_pct } else { 100 };
        let report = match (r.soc, r.current_ua) {
            (Some(soc), Some(ua)) => {
                let online = r.charger_connected.unwrap_or_else(|| g.samples.back().is_some_and(|s| s.online));
                push(&mut g.samples, Sample { t: now.duration_since(self.t0).as_secs_f64(), ua, online });
                let samples: Vec<Sample> = g.samples.iter().copied().collect();
                // unknown plug state counts as plugged, like the web did: the
                // limit only stops charging while a charger is there
                let paused = r.limit_enabled
                    && r.charging_stopped == Some(true)
                    && !r.manual_override
                    && r.charger_connected != Some(false);
                let e = estimate(&Input {
                    samples: &samples,
                    soc,
                    charge_full_uah: r.charge_full_uah,
                    charge_counter_uah: r.charge_counter_uah,
                    target_pct,
                    paused_at_limit: paused,
                });
                Report { state: "ok", kind: e.kind, minutes: e.minutes, target_pct, observed_at: wall, samples: samples.len() }
            }
            _ => Report {
                state: "unavailable",
                kind: Kind::Unknown,
                minutes: None,
                target_pct,
                observed_at: wall,
                samples: g.samples.len(),
            },
        };
        g.last = Some((now, report.clone()));
        report
    }

    /// Latest report, marked `stale` when the sampler stopped.
    pub fn report(&self, now: Instant) -> Report {
        let g = self.inner.lock().unwrap();
        match &g.last {
            None => Report {
                state: "estimating",
                kind: Kind::Unknown,
                minutes: None,
                target_pct: 100,
                observed_at: 0,
                samples: 0,
            },
            Some((at, r)) if now.duration_since(*at) > STALE_AFTER => Report { state: "stale", ..r.clone() },
            Some((_, r)) => r.clone(),
        }
    }

    pub fn start(self: &Arc<Self>, limit: Arc<ChargeLimitEnforcer>) {
        let me = Arc::clone(self);
        std::thread::Builder::new()
            .name("battery_eta".into())
            .spawn(move || loop {
                let r = read_now(&limit);
                me.feed(&r, Instant::now(), wall_now());
                std::thread::sleep(SAMPLE_EVERY);
            })
            .ok();
    }
}

fn wall_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// sysfs for the battery itself; plug and stop state from the datad feed
/// (direct ubus only when the feed can't say), as the charge policy does.
fn read_now(limit: &ChargeLimitEnforcer) -> Reading {
    let b = crate::system::read_battery();
    let (limit_enabled, limit_pct, _, manual_override) = limit.get();
    let charger_connected =
        crate::datad_feed::charger_connected_cached(crate::charge_policy::direct_charger_connected);
    let charging_stopped = crate::datad_feed::global()
        .and_then(|f| f.view().charging_stopped())
        .or_else(|| limit_enabled.then(crate::charge_policy::is_charging_stopped));
    Reading {
        soc: b.as_ref().map(|b| b.capacity),
        current_ua: b.as_ref().map(|b| b.current_ua),
        charge_full_uah: b.as_ref().and_then(|b| b.charge_full_uah),
        charge_counter_uah: b.as_ref().and_then(|b| b.charge_counter_uah),
        charger_connected,
        limit_enabled,
        limit_pct: i64::from(limit_pct),
        charging_stopped,
        manual_override,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn kind_name(k: Kind) -> &'static str {
        match k {
            Kind::ChargingEta => "charging_eta",
            Kind::DischargingEta => "discharging_eta",
            Kind::ReachedTarget => "reached_target",
            Kind::PausedAtLimit => "paused_at_limit",
            Kind::Unknown => "unknown",
        }
    }

    /// The shared cases from docs/battery-estimate/fixtures.json (the rules'
    /// examples, formerly run by both the web and the touch screen).
    #[test]
    fn fixtures() {
        let v: Value = serde_json::from_str(include_str!("../../docs/battery-estimate/fixtures.json")).unwrap();
        let cases = v["cases"].as_array().unwrap();
        assert!(cases.len() >= 10);
        for c in cases {
            let i = &c["input"];
            let samples: Vec<Sample> = i["samples"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| Sample {
                    t: s[0].as_f64().unwrap(),
                    ua: s[1].as_f64().unwrap() as i64,
                    online: s[2].as_bool().unwrap(),
                })
                .collect();
            let e = estimate(&Input {
                samples: &samples,
                soc: i["soc"].as_i64().unwrap(),
                charge_full_uah: i["charge_full_uah"].as_i64(),
                charge_counter_uah: i["charge_counter_uah"].as_i64(),
                target_pct: i["target_pct"].as_i64().unwrap(),
                paused_at_limit: i["paused_at_limit"].as_bool().unwrap(),
            });
            let name = c["name"].as_str().unwrap();
            assert_eq!(kind_name(e.kind), c["expect"]["kind"].as_str().unwrap(), "{name}");
            assert_eq!(e.minutes, c["expect"]["minutes"].as_i64(), "{name}");
        }
    }

    fn reading(soc: i64, ua: i64, plugged: Option<bool>) -> Reading {
        Reading {
            soc: Some(soc),
            current_ua: Some(ua),
            charge_full_uah: Some(11_000_000),
            charge_counter_uah: Some(5_500_000),
            charger_connected: plugged,
            limit_pct: 100,
            ..Default::default()
        }
    }

    #[test]
    fn states_over_time() {
        let eta = BatteryEta::new();
        let t0 = eta.t0;
        assert_eq!(eta.report(t0).state, "estimating");

        // discharging 1.1 A steadily, counter 5.5 Ah → 300 min
        for k in 0..10u64 {
            eta.feed(&reading(50, -1_100_000, Some(false)), t0 + Duration::from_secs(5 * k), 1000 + 5 * k);
        }
        let r = eta.report(t0 + Duration::from_secs(46));
        assert_eq!((r.state, r.kind, r.minutes), ("ok", Kind::DischargingEta, Some(300)));
        assert_eq!(r.observed_at, 1045);

        // sampler stopped: same numbers, marked stale
        let r = eta.report(t0 + Duration::from_secs(45 + 21));
        assert_eq!((r.state, r.minutes), ("stale", Some(300)));

        // battery unreadable
        let r = eta.feed(&Reading::default(), t0 + Duration::from_secs(70), 1070);
        assert_eq!((r.state, r.kind), ("unavailable", Kind::Unknown));
    }

    #[test]
    fn window_keeps_only_180s() {
        let eta = BatteryEta::new();
        let t0 = eta.t0;
        let mut last = None;
        for k in 0..100u64 {
            last = Some(eta.feed(&reading(50, -1_100_000, Some(false)), t0 + Duration::from_secs(5 * k), k));
        }
        // 180 s / 5 s + the one at the cut
        assert!(last.unwrap().samples <= 38);
    }

    #[test]
    fn charge_limit_and_pause() {
        let eta = BatteryEta::new();
        let t0 = eta.t0;
        let mut r = reading(80, -350_000, Some(true));
        r.limit_enabled = true;
        r.limit_pct = 80;
        r.charging_stopped = Some(true);
        let out = eta.feed(&r, t0, 1);
        assert_eq!((out.kind, out.target_pct), (Kind::PausedAtLimit, 80));

        // a manual stop is not "at the limit"
        r.manual_override = true;
        let out = eta.feed(&r, t0 + Duration::from_secs(5), 2);
        assert_ne!(out.kind, Kind::PausedAtLimit);

        // unplugged: never paused
        r.manual_override = false;
        r.charger_connected = Some(false);
        let out = eta.feed(&r, t0 + Duration::from_secs(10), 3);
        assert_ne!(out.kind, Kind::PausedAtLimit);

        // an out-of-range limit counts as no limit
        r.limit_pct = 20;
        assert_eq!(eta.feed(&r, t0 + Duration::from_secs(15), 4).target_pct, 100);
    }

    #[test]
    fn unknown_plug_state_keeps_the_last_one() {
        let eta = BatteryEta::new();
        let t0 = eta.t0;
        eta.feed(&reading(50, 1_000_000, Some(true)), t0, 1);
        eta.feed(&reading(50, 1_000_000, None), t0 + Duration::from_secs(5), 2);
        let g = eta.inner.lock().unwrap();
        assert!(g.samples.iter().all(|s| s.online));
    }
}
