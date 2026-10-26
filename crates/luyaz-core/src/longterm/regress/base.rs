//! Baselines (design §4, eng review R7/R10/R13, CEO round 2): the median of the last 7 days'
//! qualifying hours from `hours.jsonl` (one point an hour, across restarts; nothing new is
//! written to flash for them), recomputed once a day. The first one is kept as the anchor and
//! later ones may not pass it by more than a quarter (a slow leak cannot raise its own bar). The
//! owner moves a relative bar with `accept`, an absolute one with `limit` (`baseline.json`).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use super::detect;

pub const FILE: &str = "baseline.json";
pub const WINDOW_S: i64 = 7 * 86_400;
pub const ANCHOR_ROOM: f64 = 1.25;
/// Qualifying hours a baseline needs: memory, power (about two nights).
pub const MEM_MIN: usize = 10;
pub const POWER_MIN: usize = 6;
/// The absolute bars' starting values (design §3).
pub const HEAP_LIMIT_MIB: f64 = 32.0;
pub const ANON_LIMIT_MIB: f64 = 96.0;
pub const POWER_LIMIT_MW: f64 = 600.0;
/// Power's blindness (eng review R1): this much time unplugged in 7 days with no qualifying hour.
pub const UNPLUGGED_MIN_S: i64 = 6 * 3600;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Limits {
    pub heap_mib: Option<f64>,
    pub anon_mib: Option<f64>,
    pub power_mw: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Base {
    pub v: u32,
    /// kB, from `hour_proc`'s heap_live / anon_core.
    pub heap: Option<f64>,
    pub anon: Option<f64>,
    pub heap_anchor: Option<f64>,
    pub anon_anchor: Option<f64>,
    /// mW per cell (`home`, `abroad`).
    pub power: BTreeMap<String, f64>,
    pub power_anchor: BTreeMap<String, f64>,
    /// The firmware and SIM home network the power baseline was built under: either moving
    /// empties it (design §4 reset).
    pub power_key: Option<String>,
    /// When it was last recomputed (wall).
    pub at: Option<i64>,
    /// The owner's bars, from `regress-limits.json` (not saved here).
    #[serde(skip)]
    pub limits: Limits,
    /// Power could be judged in the last 7 days (R1); None until computed.
    pub power_known: Option<bool>,
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    Some(if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    })
}

/// A value under its anchor's room; the first one becomes the anchor.
fn anchored(v: f64, anchor: &mut Option<f64>) -> f64 {
    match anchor {
        Some(a) => v.min(*a * ANCHOR_ROOM),
        None => {
            *anchor = Some(v);
            v
        }
    }
}

/// An hour line's span on the wall clock.
fn span(l: &Value) -> Option<(i64, i64)> {
    Some((l["ft"].as_i64()?, l["tt"].as_i64()?))
}

fn inside(s: (i64, i64), ex: &[(i64, i64)]) -> bool {
    ex.iter().any(|&(a, b)| s.0 < b && s.1 > a)
}

/// A memory hour: the same core process the whole hour (`cpu_core` is only there then).
fn mem_hour(l: &Value) -> Option<(f64, f64)> {
    if l["k"] != "hour_proc" || l["cpu_core"].is_null() {
        return None;
    }
    Some((l["heap_live"].as_f64()?, l["anon_core"].as_f64()?))
}

impl Base {
    pub fn mem(&self) -> Option<(f64, f64)> {
        Some((self.heap?, self.anon?))
    }

    /// The absolute memory bars in kB.
    pub fn mem_limits(&self) -> (f64, f64) {
        (
            self.limits.heap_mib.unwrap_or(HEAP_LIMIT_MIB) * 1024.0,
            self.limits.anon_mib.unwrap_or(ANON_LIMIT_MIB) * 1024.0,
        )
    }

    pub fn power(&self, cell: &str) -> Option<f64> {
        self.power.get(cell).copied()
    }

    pub fn power_limit(&self) -> f64 {
        self.limits.power_mw.unwrap_or(POWER_LIMIT_MW)
    }

    /// The firmware or the SIM's home network moved: the power baseline starts over.
    pub fn power_under(&mut self, key: &str) -> bool {
        if self.power_key.as_deref() == Some(key) {
            return false;
        }
        let had = self.power_key.is_some();
        self.power_key = Some(key.into());
        if had {
            self.power.clear();
            self.power_anchor.clear();
        }
        true
    }

    /// Recompute from the hour lines (any order), `now` the wall clock; hours inside `mem_ex` /
    /// `power_ex` (incident spans) are left out.
    pub fn recompute<'a>(
        &mut self,
        lines: impl Iterator<Item = &'a Value>,
        now: i64,
        mem_ex: &[(i64, i64)],
        power_ex: &[(i64, i64)],
    ) {
        let (mut heap, mut anon) = (Vec::new(), Vec::new());
        let mut power: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        let mut unplugged = 0;
        let mut qualifying = 0;
        for l in lines {
            let Some(s) = span(l) else { continue };
            if s.1 < now - WINDOW_S || s.0 > now {
                continue;
            }
            if let Some((h, a)) = mem_hour(l)
                && !inside(s, mem_ex)
            {
                heap.push(h);
                anon.push(a);
            }
            if l["k"] == "hour_power" {
                unplugged += ["on_home_s", "on_abroad_s", "off_home_s", "off_abroad_s"]
                    .iter()
                    .map(|k| l[*k].as_i64().unwrap_or(0))
                    .sum::<i64>();
                for (cell, mw) in detect::qualifying(l) {
                    qualifying += 1;
                    if !inside(s, power_ex) {
                        power.entry(cell).or_default().push(mw);
                    }
                }
            }
        }
        if heap.len() >= MEM_MIN {
            let h = median(heap).unwrap_or(0.0);
            let a = median(anon).unwrap_or(0.0);
            self.heap = Some(anchored(h, &mut self.heap_anchor));
            self.anon = Some(anchored(a, &mut self.anon_anchor));
        }
        for (cell, v) in power {
            if v.len() >= POWER_MIN {
                let m = median(v).unwrap_or(0.0);
                let mut a = self.power_anchor.get(&cell).copied();
                let b = anchored(m, &mut a);
                if let Some(a) = a {
                    self.power_anchor.insert(cell.clone(), a);
                }
                self.power.insert(cell, b);
            }
        }
        // charging all week is "does not apply", not blind (R1)
        self.power_known = Some(!(unplugged >= UNPLUGGED_MIN_S && qualifying == 0));
        self.at = Some(now);
    }

    /// The owner accepts a relative incident's level as the new normal (design §4, CEO round 2):
    /// the baseline and its anchor become the recent median, incident time included. Err = why
    /// not (too few hours).
    pub fn accept<'a>(
        &mut self,
        kind: &str,
        cell: Option<&str>,
        lines: impl Iterator<Item = &'a Value>,
        now: i64,
    ) -> Result<String, String> {
        match kind {
            "regress-mem" => {
                let (mut heap, mut anon) = (Vec::new(), Vec::new());
                for l in lines {
                    if span(l).is_some_and(|s| s.1 >= now - 86_400 && s.0 <= now)
                        && let Some((h, a)) = mem_hour(l)
                    {
                        heap.push(h);
                        anon.push(a);
                    }
                }
                if heap.len() < MEM_MIN {
                    return Err(format!(
                        "need {MEM_MIN} hours of one core process in the last 24 h, have {} ({} more)",
                        heap.len(),
                        MEM_MIN - heap.len()
                    ));
                }
                let h = median(heap).unwrap_or(0.0);
                let a = median(anon).unwrap_or(0.0);
                self.heap = Some(h);
                self.anon = Some(a);
                self.heap_anchor = Some(h);
                self.anon_anchor = Some(a);
                Ok(format!("memory baseline heap {h:.0} kB anon {a:.0} kB"))
            }
            "regress-power" => {
                let cell = cell.unwrap_or("home");
                let v: Vec<f64> = lines
                    .filter(|l| span(l).is_some_and(|s| s.1 >= now - 3 * 86_400 && s.0 <= now))
                    .flat_map(detect::qualifying)
                    .filter(|(c, _)| c == cell)
                    .map(|(_, m)| m)
                    .collect();
                if v.len() < POWER_MIN {
                    return Err(format!(
                        "need {POWER_MIN} qualifying hours ({cell}) in the last 3 nights, have {} ({} more)",
                        v.len(),
                        POWER_MIN - v.len()
                    ));
                }
                let m = median(v).unwrap_or(0.0);
                self.power.insert(cell.into(), m);
                self.power_anchor.insert(cell.into(), m);
                Ok(format!("standby baseline ({cell}) {m:.0} mW"))
            }
            _ => Err(format!(
                "{kind} has no baseline to accept (absolute bars move with limit)"
            )),
        }
    }
}
