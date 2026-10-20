//! `power.hot_drain` evidence (decision-execution-loop.md §4.11): the long-term layer's closed
//! hours (`hours.jsonl`: `hour`, `hour_power`, `hour_proc`) turned into
//! `luyaz_api::execution::hot_drain::Evidence` for `/api/diagnose`. Read-only: the replay never
//! carries an action.
//!
//! - The window is this boot's last closed hour, at most [`FRESH_S`] old: slow history, explain
//!   only (§1.3 lets it show a trend, never gate a write). Its condition is the screen × home
//!   cell it spent the most seconds in.
//! - Matched hours: the same cell for at least half an hour, not charging. Charging is
//!   `hour_power.chg_s` (the fuel gauge's status and current), never the `hour` line's `chg_s`:
//!   that one follows the battery block's flag, which on this device stays on while the battery
//!   drains (10-20: 165 of 277 hours, e.g. 87 → 82 % in an hour it called 3149 s charging).
//!   Temperatures are `hour_power.batt_tmax` (tenths of °C) and `zone_tmax` (m°C).
//! - Drain only from `counter_quality` cells with ≥ 30 screened minutes; never the hour's `e`
//!   (power-measurement.md: unverified, not to be mixed). On a device with clients most of the
//!   day that is mostly unknown: by design.
//! - No new polling: the file is parsed again only when its size or mtime changed (once an hour);
//!   the battery's `health` is one sysfs read per request.

use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::Path;
use std::sync::Mutex;
use std::time::SystemTime;

use luyaz_api::execution::hot_drain::{Evidence, Factors, MIN_HOURS, MIN_WINDOW_S, replay};
use serde_json::{Value, json};

/// The window may have ended at most this long ago (BOOTTIME s).
pub const FRESH_S: i64 = 2 * 3600;
/// Hours kept for the baselines (30 days).
pub const KEEP: usize = 30 * 24;
const CELLS: [&str; 4] = ["on_home", "on_abroad", "off_home", "off_abroad"];
const OFF_HOME: usize = 2;

/// One closed hour, from its `hour_power`, `hour_proc` and `hour` lines.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Row {
    pub boot: String,
    pub from: i64,
    pub to: i64,
    /// `hour_power` was seen (the other fields below come from it unless said).
    pub power: bool,
    pub cell_s: [i64; 4],
    pub chg_s: i64,
    pub batt_dc: Option<i64>,
    pub zone_mc: Option<i64>,
    pub throttle_s: Option<i64>,
    pub off_home_cli_s: Option<i64>,
    /// `off_home_wan_pkts`, `off_abroad_wan_pkts`.
    pub wan_pkts: [Option<i64>; 2],
    /// `counter_quality.cells`: screened seconds and µWh.
    pub q_s: [i64; 4],
    pub q_uwh: [Option<f64>; 4],
    /// `hour_proc.activity.system_cpu.all_core_percent`.
    pub cpu: Option<f64>,
    /// `off_home_quiet_pm`, `off_abroad_quiet_pm`.
    pub quiet_pm: [Option<i64>; 2],
    /// The `hour` line (coverage only, for `long_idle`; its `chg_s` is never read, see above):
    /// awake + asleep seconds, and `g_core` (seconds the core was away, so its readings are
    /// missing). None = no `hour` line.
    pub ledger_s: Option<i64>,
    pub ledger_core_gap: Option<i64>,
}

impl Row {
    fn main_cell(&self) -> usize {
        (0..4)
            .max_by_key(|&c| (self.cell_s[c], -(c as i64)))
            .unwrap_or(0)
    }

    fn charging(&self) -> Option<bool> {
        self.power.then_some(self.chg_s > 0)
    }

    fn drain_mw(&self, c: usize) -> Option<f64> {
        let s = self.q_s[c];
        (s >= MIN_WINDOW_S as i64)
            .then_some(self.q_uwh[c]?)
            .map(|uwh| uwh * 3.6 / s as f64)
    }
}

fn i(v: &Value, k: &str) -> Option<i64> {
    v.get(k)?.as_i64()
}

/// Rows from the ledger's lines, oldest first, at most [`KEEP`].
pub fn rows<S: AsRef<str>>(lines: impl Iterator<Item = S>) -> Vec<Row> {
    let mut by: BTreeMap<(String, i64), usize> = BTreeMap::new();
    let mut out: Vec<Row> = Vec::new();
    for l in lines {
        let l = l.as_ref();
        if !l.contains("\"k\":\"hour") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(l) else {
            continue;
        };
        let (Some(boot), Some(from)) = (v.get("boot").and_then(Value::as_str), i(&v, "from"))
        else {
            continue;
        };
        let n = *by.entry((boot.to_string(), from)).or_insert_with(|| {
            out.push(Row {
                boot: boot.into(),
                from,
                ..Row::default()
            });
            out.len() - 1
        });
        let r = &mut out[n];
        r.to = r.to.max(i(&v, "to").unwrap_or(0));
        match v.get("k").and_then(Value::as_str) {
            Some("hour") => {
                r.ledger_s = Some(i(&v, "awake").unwrap_or(0) + i(&v, "asleep").unwrap_or(0));
                r.ledger_core_gap = i(&v, "g_core");
            }
            Some("hour_proc") => {
                r.cpu = v
                    .pointer("/activity/system_cpu/all_core_percent")
                    .and_then(Value::as_f64);
            }
            Some("hour_power") => {
                r.power = true;
                for (c, name) in CELLS.iter().enumerate() {
                    r.cell_s[c] = i(&v, &format!("{name}_s")).unwrap_or(0);
                    let q = v.pointer(&format!("/counter_quality/cells/{name}"));
                    r.q_s[c] = q.and_then(|q| i(q, "observed_s")).unwrap_or(0);
                    r.q_uwh[c] = q.and_then(|q| q.get("uwh")?.as_f64());
                }
                r.chg_s = i(&v, "chg_s").unwrap_or(0);
                r.batt_dc = i(&v, "batt_tmax");
                r.zone_mc = i(&v, "zone_tmax");
                r.throttle_s = i(&v, "throttle_s");
                r.off_home_cli_s = i(&v, "off_home_cli_s");
                r.wan_pkts = [i(&v, "off_home_wan_pkts"), i(&v, "off_abroad_wan_pkts")];
                r.quiet_pm = [i(&v, "off_home_quiet_pm"), i(&v, "off_abroad_quiet_pm")];
            }
            _ => {}
        }
    }
    let skip = out.len().saturating_sub(KEEP);
    out.drain(..skip);
    out
}

/// Nearest-rank 90th percentile.
fn p90(mut xs: Vec<f64>) -> Option<f64> {
    if xs.len() < MIN_HOURS as usize {
        return None;
    }
    xs.sort_by(f64::total_cmp);
    let k = (xs.len() * 9).div_ceil(10).max(1) - 1;
    xs.get(k).copied()
}

fn median(mut xs: Vec<f64>) -> Option<f64> {
    if xs.is_empty() {
        return None;
    }
    xs.sort_by(f64::total_cmp);
    Some(xs[xs.len() / 2])
}

/// Above the matched hours' 90th percentile; None when unread or too few matched.
fn above(now: Option<f64>, base: Vec<f64>) -> Option<bool> {
    Some(now? > p90(base)?)
}

/// The window, the matched hours and the evidence. `boot8`: this boot's id prefix (as the ledger
/// writes it); `now_up`: BOOTTIME s; `health`: the fuel gauge's word.
pub fn evidence(rows: &[Row], boot8: &str, now_up: i64, health: Option<&str>) -> (Evidence, Value) {
    let Some((wi, w)) = rows.iter().enumerate().rev().find(|(_, r)| r.power) else {
        return (Evidence::default(), Value::Null);
    };
    let c = w.main_cell();
    let age = (w.boot == boot8).then(|| now_up - w.to);
    let matched: Vec<&Row> = rows
        .iter()
        .enumerate()
        .filter(|&(n, r)| {
            n != wi
                && r.power
                && r.main_cell() == c
                && r.cell_s[c] >= MIN_WINDOW_S as i64
                && r.charging() == Some(false)
        })
        .map(|(_, r)| r)
        .collect();
    let col = |f: &dyn Fn(&Row) -> Option<f64>| matched.iter().filter_map(|r| f(r)).collect();
    let temps: Vec<f64> = col(&|r| r.batt_dc.map(|t| t as f64));
    let drains: Vec<f64> = col(&|r| r.drain_mw(c));
    let off = c >= 2;
    let a = c - 2 * usize::from(off);
    let factors = Factors {
        clients: (c == OFF_HOME)
            .then_some(w.off_home_cli_s)
            .flatten()
            .map(|s| s > 0),
        cellular: off
            .then(|| {
                above(
                    w.wan_pkts[a].map(|p| p as f64),
                    col(&|r| r.wan_pkts[a].map(|p| p as f64)),
                )
            })
            .flatten(),
        cpu: above(w.cpu, col(&|r| r.cpu)),
        throttled: w.throttle_s.map(|s| s > 0),
        chip_hot: above(
            w.zone_mc.map(|z| z as f64),
            col(&|r| r.zone_mc.map(|z| z as f64)),
        ),
        battery_health: health.map(|h| h.trim() != "Good"),
    };
    let e = Evidence {
        fresh: Some(age.is_some_and(|a| (0..=FRESH_S).contains(&a))),
        window_s: Some(w.cell_s[c].max(0) as u64),
        charging: w.charging(),
        batt_dc: w.batt_dc,
        batt_normal_dc: p90(temps.clone()).map(|t| t as i64),
        temp_hours: temps.len() as u32,
        drain_mw: w.drain_mw(c).map(|m| m.round() as u32),
        drain_normal_mw: (drains.len() >= MIN_HOURS as usize)
            .then(|| median(drains.clone()))
            .flatten()
            .map(|m| m.round() as u32),
        drain_hours: drains.len() as u32,
        factors,
    };
    let window = json!({"cell": CELLS[c], "window_s": w.cell_s[c], "age_s": age, "from": w.from});
    (e, window)
}

struct Cache {
    key: Option<(u64, SystemTime)>,
    rows: Vec<Row>,
}

static CACHE: Mutex<Cache> = Mutex::new(Cache {
    key: None,
    rows: Vec::new(),
});

/// The ledger's rows, parsed again only when the file changed (shared with `long_idle`).
fn cached(path: &Path) -> Vec<Row> {
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let key = std::fs::metadata(path)
        .ok()
        .and_then(|m| Some((m.len(), m.modified().ok()?)));
    if key.is_none() {
        c.key = None;
        c.rows.clear();
    } else if key != c.key {
        c.rows = std::fs::File::open(path)
            .map(|f| rows(std::io::BufReader::new(f).lines().map_while(Result::ok)))
            .unwrap_or_default();
        c.key = key;
    }
    c.rows.clone()
}

pub(crate) fn ledger() -> Vec<Row> {
    let dir = std::path::PathBuf::from(crate::state::host_path(crate::longterm::DIR));
    cached(&dir.join("hours.jsonl"))
}

pub(crate) fn boot8() -> String {
    crate::evlog::boot_id()
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .take(8)
        .collect()
}

/// `/api/diagnose`'s `hot_drain`: the replay's answer and the window it saw.
pub fn view() -> Value {
    let rows = ledger();
    let health = std::fs::read_to_string(crate::state::host_path(
        "/sys/class/power_supply/battery/health",
    ))
    .ok();
    let now = (super::band_flap::boot_ms() / 1000) as i64;
    let (e, window) = evidence(&rows, &boot8(), now, health.as_deref());
    json!({
        "advice": replay(&e),
        "window": window,
        "batt_dc": e.batt_dc,
        "batt_normal_dc": e.batt_normal_dc,
        "temp_hours": e.temp_hours,
        "drain_mw": e.drain_mw,
        "drain_normal_mw": e.drain_normal_mw,
        "drain_hours": e.drain_hours,
    })
}
