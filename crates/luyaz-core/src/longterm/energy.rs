//! Counter interval screening, independent of historical hour_power estimates.
//! No IO: uses the existing check's readings. Endpoint agreement cannot prove
//! there were no transitions between samples. A downward reset cannot be
//! distinguished from discharge without an independently validated bound.

use super::{Clock, power::Read};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const CELLS: [&str; 4] = ["on_home", "on_abroad", "off_home", "off_abroad"];

/// Do not infer discharge merely from missing status or from a negative current.
/// Conflicting current/status is unknown. Full is excluded conservatively.
pub fn charging(status: &str, current: Option<i64>) -> Option<bool> {
    match status {
        "Charging" | "Full" if current.is_none_or(|i| i >= 0) => Some(true),
        "Discharging" | "Not charging" if current.is_none_or(|i| i <= 0) => Some(false),
        _ => None,
    }
}

#[derive(Clone, Debug)]
struct Stamp {
    boot: String,
    up: i64,
    mono: i64,
    q: Option<i64>,
    charging: Option<bool>,
    screen: Option<bool>,
    abroad: Option<bool>,
    clients: Option<i64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Acc {
    samples: u64,
    pair_s: i64,
    rejected_s: i64,
    rejected: BTreeMap<String, u64>,
    valid_s: [i64; 4],
    uwh: [f64; 4],
    sleep_s: i64,
    max_discharge_ua: Option<f64>,
    max_interval: Option<Value>,
    last_rejected: Option<Value>,
    // Never bridge a restart, old saved fold, or an hour boundary.
    #[serde(skip)]
    prev: Option<Stamp>,
}

impl Acc {
    fn reject(&mut self, why: &str, seconds: i64, evidence: Value) {
        *self.rejected.entry(why.into()).or_default() += 1;
        self.rejected_s += seconds;
        self.last_rejected = Some(json!({"reason":why, "interval":evidence}));
    }

    pub fn observe(&mut self, r: &Read, c: &Clock, abroad: Option<bool>) {
        self.samples += 1;
        let now = Stamp {
            boot: c.boot.clone(),
            up: c.boottime,
            mono: c.mono,
            q: r.q,
            charging: r.charging_known,
            screen: r.screen_known,
            abroad,
            clients: r.clients,
        };
        if c.boot.is_empty() || c.boottime < 0 || c.mono < 0 || c.mono > c.boottime {
            self.prev = None;
            self.reject("invalid_clock", 0, Value::Null);
            return;
        }
        let Some(old) = self.prev.replace(now.clone()) else {
            *self.rejected.entry("baseline_only".into()).or_default() += 1;
            return;
        };
        let dt = c.boottime.checked_sub(old.up);
        let awake = c.mono.checked_sub(old.mono);
        let evidence = json!({"from":old.up,"to":c.boottime,"awake_s":awake,
            "q0_uah":old.q,"q1_uah":r.q,"v1_uv":r.v});
        if old.boot != now.boot {
            self.reject("boot_changed", 0, evidence);
            return;
        }
        let (Some(dt), Some(awake)) = (dt, awake) else {
            self.reject("invalid_clock", 0, evidence);
            return;
        };
        if dt <= 0 || awake < 0 || awake > dt {
            self.reject("invalid_clock", 0, evidence);
            return;
        }
        self.pair_s += dt;
        // A sleep is allowed; a missed awake check is not silently covered.
        let reason = if awake > 120 {
            Some("awake_gap")
        } else if old.charging.is_none() || now.charging.is_none() {
            Some("charging_unknown")
        } else if old.charging != now.charging {
            Some("charging_transition")
        } else if now.charging == Some(true) {
            Some("charging")
        } else if old.screen.is_none()
            || now.screen.is_none()
            || old.abroad.is_none()
            || now.abroad.is_none()
        {
            Some("scenario_unknown")
        } else if old.screen != now.screen || old.abroad != now.abroad {
            Some("scenario_transition")
        } else if old.clients.is_none_or(|n| n < 0) || now.clients.is_none_or(|n| n < 0) {
            Some("clients_unknown")
        } else if old.clients != now.clients
            || (now.screen == Some(false) && now.abroad == Some(false) && now.clients != Some(0))
        {
            Some("clients_present_or_transition")
        } else {
            None
        };
        if let Some(reason) = reason {
            self.reject(reason, dt, evidence);
            return;
        }
        let (Some(q0), Some(q1)) = (old.q, r.q) else {
            self.reject("counter_missing", dt, evidence);
            return;
        };
        let Some(dq) = q0.checked_sub(q1) else {
            self.reject("counter_arithmetic_overflow", dt, evidence);
            return;
        };
        if dq < 0 {
            self.reject("counter_increase", dt, evidence);
            return;
        }
        let Some(v) = r.v.filter(|v| *v > 0) else {
            self.reject("voltage_missing_or_invalid", dt, evidence);
            return;
        };
        let ua = dq as f64 * 3600.0 / dt as f64;
        if self.max_discharge_ua.is_none_or(|m| ua > m) {
            self.max_discharge_ua = Some(ua);
            self.max_interval = Some(evidence);
        }
        let cell =
            usize::from(now.screen == Some(false)) * 2 + usize::from(now.abroad == Some(true));
        self.valid_s[cell] += dt;
        self.uwh[cell] += dq as f64 * v as f64 / 1e6;
        self.sleep_s += dt - awake;
    }

    pub fn value(&self) -> Value {
        let cells: BTreeMap<_, _> = CELLS
            .iter()
            .enumerate()
            .map(|(n, c)| {
                (
                    *c,
                    json!({"observed_s":self.valid_s[n],
                "uwh":(self.valid_s[n] > 0).then_some(self.uwh[n])}),
                )
            })
            .collect();
        json!({"v":1,"clock":"boottime_seconds","samples":self.samples,
            "pair_s":self.pair_s,"rejected_s":self.rejected_s,"rejected":self.rejected,
            "cells":cells,"sleep_observed_s":self.sleep_s,
            "max_discharge_ua":self.max_discharge_ua,"max_interval":self.max_interval,
            "last_rejected":self.last_rejected,
            "jump_check":"requires_independently_validated_discharge_bound",
            "transition_check":"endpoints_only","voltage_method":"interval_end"})
    }
}

#[cfg(test)]
#[path = "energy_tests.rs"]
mod tests;
