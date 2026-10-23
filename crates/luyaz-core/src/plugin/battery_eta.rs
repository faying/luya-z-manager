//! battery_eta in u60d: fed on every successful read of the `battery`
//! block (5 s, the agent's sampler interval), so it adds no timer of its own. Readings come from
//! the same places the charge limit reads them:
//!
//! - soc, current, charge_full, charge_counter: sysfs `/sys/class/power_supply/battery`;
//! - charger plugged: the battery block's `charger_connect`; charging stopped: the charger block's
//!   `direct_supply.enabled` (what the agent's core feed reads);
//! - the charge limit and the agent's manual override: `charge.rs` (the agent's limit file, and
//!   the override mirrored from manual charge writes in `/control`).
//!
//! The report is only on `GET /debug/policy`; nothing in `/v2` changes.

use super::Slot;
use crate::block::Hub;
use serde_json::{Value, json};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use luyaz_policy::battery_eta::{self as p, Estimate, Input, Kind, Reading, Tracker};

/// The WIT package this u60d was built against (`plugins.json` `abi`).
pub const ABI: &str = "luyaz:policy@0.1.0";
pub const NAME: &str = "battery-eta";

mod wit {
    wasmtime::component::bindgen!({ path: "../luyaz-policy/wit/battery-eta.wit", world: "battery-eta" });
}

/// A loaded component, ready to instantiate per call.
pub struct Component {
    engine: wasmtime::Engine,
    pre: wit::BatteryEtaPre<super::Limits>,
}

impl Component {
    /// Deserialize checked bytes (`super::load_checked`).
    pub fn new(engine: &wasmtime::Engine, bytes: &[u8]) -> Result<Self, String> {
        // SAFETY: the bytes matched the sha256 in the manifest, which plugc wrote for a .cwasm it
        // compiled with this Config; wasmtime also refuses another version's or Config's output.
        let c = unsafe { wasmtime::component::Component::deserialize(engine, bytes) }
            .map_err(|e| format!("deserialize: {}", super::short(&e)))?;
        let linker = wasmtime::component::Linker::new(engine);
        let pre = linker
            .instantiate_pre(&c)
            .and_then(wit::BatteryEtaPre::new)
            .map_err(|e| format!("link: {}", super::short(&e)))?;
        Ok(Self {
            engine: engine.clone(),
            pre,
        })
    }

    /// One call in a fresh instance with the fuel and memory caps.
    pub fn call(&self, i: &Input) -> Result<Estimate, String> {
        let input = wit::Input {
            samples: i
                .samples
                .iter()
                .map(|s| wit::Sample {
                    t: s.t,
                    ua: s.ua,
                    online: s.online,
                })
                .collect(),
            soc: i.soc,
            charge_full_uah: i.charge_full_uah,
            charge_counter_uah: i.charge_counter_uah,
            target_pct: i.target_pct,
            paused_at_limit: i.paused_at_limit,
        };
        let run = || -> wasmtime::Result<wit::Estimate> {
            let mut store = super::store(&self.engine)?;
            let inst = self.pre.instantiate(&mut store)?;
            inst.call_estimate(&mut store, &input)
        };
        let e = run().map_err(|e| super::short(&e))?;
        let kind = match e.kind {
            wit::Kind::ChargingEta => Kind::ChargingEta,
            wit::Kind::DischargingEta => Kind::DischargingEta,
            wit::Kind::ReachedTarget => Kind::ReachedTarget,
            wit::Kind::PausedAtLimit => Kind::PausedAtLimit,
            wit::Kind::Unknown => Kind::Unknown,
        };
        Ok(Estimate {
            kind,
            minutes: e.minutes,
        })
    }
}

struct State {
    t0: Instant,
    tracker: Tracker,
    slot: Slot<Component>,
    /// plug state and stop flag of the last reading fed
    switches: (Option<bool>, Option<bool>),
}

static STATE: OnceLock<Mutex<State>> = OnceLock::new();

fn state() -> &'static Mutex<State> {
    STATE.get_or_init(|| Mutex::new(load(&super::dir())))
}

fn load(dir: &std::path::Path) -> State {
    State {
        t0: Instant::now(),
        tracker: Tracker::new(),
        slot: Slot::load(dir, NAME, ABI, Component::new),
        switches: (None, None),
    }
}

/// Called after every battery block read.
pub fn tick(hub: &Hub) {
    run(hub, false);
}

/// After a charger block read: one more sample only when the plug state or the stop flag moved, so
/// the estimate's kind follows a stop/resume at once instead of up to 5 s later (the agent's
/// sampler sees the feed change sooner). The sample rhythm stays the battery block's.
pub fn charger_read(hub: &Hub) {
    run(hub, true);
}

fn run(hub: &Hub, only_on_switch: bool) {
    let r = reading(hub);
    let wall = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut g = state().lock().unwrap_or_else(|e| e.into_inner());
    let switches = (r.charger_connected, r.charging_stopped);
    if only_on_switch && g.switches == switches {
        return;
    }
    g.switches = switches;
    let now = g.t0.elapsed().as_secs_f64();
    feed(&mut g, &r, now, wall);
}

fn feed(g: &mut State, r: &Reading, now: f64, wall: u64) -> p::Report {
    let State { tracker, slot, .. } = g;
    tracker.feed(r, now, wall, &mut |i| {
        slot.run(|c| c.call(i), || p::estimate(i))
    })
}

fn reading(hub: &Hub) -> Reading {
    let bat = |f: &str| read_num(&format!("/sys/class/power_supply/battery/{f}"));
    let block = |name: &str| hub.view(name).filter(|v| !v.stale).map(|v| v.data);
    // the agent's core feed: plug state only while the charger block is fresh too
    let charger = block("charger");
    let charger_connected = charger
        .is_some()
        .then(|| {
            block("battery")
                .and_then(|d| d["charger_connect"].as_i64())
                .map(|v| v != 0)
        })
        .flatten();
    let charging_stopped = charger.and_then(|d| d["direct_supply"]["enabled"].as_bool());
    let (limit, manual_override) = super::charge::current();
    Reading {
        soc: bat("capacity"),
        current_ua: bat("current_now"),
        charge_full_uah: bat("charge_full"),
        charge_counter_uah: bat("charge_counter"),
        charger_connected,
        limit_enabled: limit.enabled,
        limit_pct: i64::from(limit.limit),
        charging_stopped,
        manual_override,
    }
}

fn read_num(path: &str) -> Option<i64> {
    std::fs::read_to_string(crate::state::host_path(path))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// The estimate as the agent's `/api/screen` and `/api/battery` give it (the touch compatibility
/// layer).
pub fn report() -> Value {
    let g = state().lock().unwrap_or_else(|e| e.into_inner());
    let now = g.t0.elapsed().as_secs_f64();
    json!(g.tracker.report(now))
}

#[cfg(test)]
pub(crate) mod tests;
