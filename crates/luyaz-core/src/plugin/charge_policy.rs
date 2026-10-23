//! charge_policy in u60d: decides on every successful read of the
//! `battery` and `charger` blocks (5 s each) what the agent's charge limit thread would write, and
//! records it as a proposal instead of writing. The agent's real writes (source auto, seen in
//! `/control`, `charge::observe_control`) are recorded next to them; `GET /debug/policy` pairs the
//! two (same direction within [`MATCH_WINDOW_SECS`]).
//!
//! Inputs, the same as the charge limit's own loop reads:
//! plugged = the battery block's `charger_connect` while the charger block is fresh; stopped = the
//! charger block's `direct_supply.enabled`; capacity and status from sysfs; the limit and the
//! manual override from `charge.rs`. The agent passes on every core feed change (about every
//! second); here it is every battery/charger read, so a decision can come up to 5 s later.
//! (That second is only the agent being woken by other blocks: its feed is these same 5 s reads.)
//!
//! Once u60d owns the part ([`set_owned`]), the same loop hands what it would have
//! proposed to the touch layer's executor ([`Job`], `compat/charge.rs`), which reads the stop flag
//! from ubus and writes under source auto; the decisions it carried out are kept in `writes`.

use super::Slot;
use super::charge;
use crate::block::Hub;
use luyaz_policy::charge_policy::{self as p, Battery, Driver, PlugStep};
use serde::Serialize;
#[cfg(test)]
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The WIT package this u60d was built against (`plugins.json` `abi`).
pub const ABI: &str = "luyaz:charge-policy@0.1.0";
pub const NAME: &str = "charge-policy";
/// A proposal and an agent write in the same direction this close together are the same decision.
#[cfg(test)]
pub const MATCH_WINDOW_SECS: f64 = 30.0;
/// The same proposal again within this long is not recorded twice (the charger block catches up
/// with the agent's write a few seconds after it).
const REPEAT_SECS: f64 = 60.0;
const KEEP: usize = 200;

mod wit {
    wasmtime::component::bindgen!({ path: "../luyaz-policy/wit/charge-policy.wit", world: "charge-policy" });
}

/// A loaded component, ready to instantiate per call.
pub struct Component {
    engine: wasmtime::Engine,
    pre: wit::ChargePolicyPre<super::Limits>,
}

/// One policy question.
#[derive(Clone, Copy, Debug)]
pub enum Ask {
    Enforce(Battery),
    Unplug { stopped: bool },
}

impl Component {
    pub fn new(engine: &wasmtime::Engine, bytes: &[u8]) -> Result<Self, String> {
        // SAFETY: as battery_eta::Component::new (manifest sha256, same wasmtime and Config).
        let c = unsafe { wasmtime::component::Component::deserialize(engine, bytes) }
            .map_err(|e| format!("deserialize: {}", super::short(&e)))?;
        let linker = wasmtime::component::Linker::new(engine);
        let pre = linker
            .instantiate_pre(&c)
            .and_then(wit::ChargePolicyPre::new)
            .map_err(|e| format!("link: {}", super::short(&e)))?;
        Ok(Self {
            engine: engine.clone(),
            pre,
        })
    }

    pub fn call(&self, ask: Ask) -> Result<Option<bool>, String> {
        let run = || -> wasmtime::Result<Option<bool>> {
            let mut store = super::store(&self.engine)?;
            let inst = self.pre.instantiate(&mut store)?;
            match ask {
                Ask::Enforce(b) => inst.call_enforce(
                    &mut store,
                    wit::Battery {
                        capacity: b.capacity,
                        discharging: b.discharging,
                        stopped: b.stopped,
                        limit: b.limit,
                        hysteresis: b.hysteresis,
                    },
                ),
                Ask::Unplug { stopped } => inst.call_on_unplug(&mut store, stopped),
            }
        };
        run().map_err(|e| super::short(&e))
    }
}

pub fn builtin(ask: Ask) -> Option<bool> {
    match ask {
        Ask::Enforce(b) => p::enforce(&b),
        Ask::Unplug { stopped } => p::on_unplug(stopped),
    }
}

/// A decision: the shadow's proposal or the agent's write. `allow` false = stop charging.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Event {
    /// seconds on u60d's monotonic clock
    #[serde(skip)]
    pub at: f64,
    /// device clock
    pub wall: u64,
    pub allow: bool,
    /// why the shadow proposed it (`limit`, `plugged_in`, `unplugged`); empty for the agent's
    pub why: &'static str,
}

struct State {
    t0: Instant,
    driver: Driver,
    revisions: (u64, u64),
    slot: Slot<Component>,
    proposals: VecDeque<Event>,
    agent: VecDeque<Event>,
    /// owned: what u60d wrote itself
    writes: VecDeque<Event>,
}

/// What the loop asks the owner's executor to do (owned mode): the agent loop's three actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Job {
    /// just plugged in, limit active: wait 500 ms, then enforce
    PluggedIn,
    /// the periodic / change-driven enforcement
    Limit,
    /// unplugged: resume charging if stopped
    Unplugged,
}

static EXECUTOR: OnceLock<tokio::sync::mpsc::UnboundedSender<Job>> = OnceLock::new();

/// The touch layer's executor (once, at start).
pub fn set_executor(tx: tokio::sync::mpsc::UnboundedSender<Job>) {
    let _ = EXECUTOR.set(tx);
}

/// u60d takes the part over or gives it back: the loop starts again from "plug state unknown",
/// as a restarted agent does, so the first look at a plugged-in charger enforces at once.
pub fn set_owned(on: bool, file: &std::path::Path) -> bool {
    if !charge::set_owned(on, file) {
        return false;
    }
    let mut g = lock();
    g.driver = Driver::default();
    true
}

static STATE: OnceLock<Mutex<State>> = OnceLock::new();

fn state() -> &'static Mutex<State> {
    STATE.get_or_init(|| Mutex::new(load(&super::dir())))
}

fn lock() -> std::sync::MutexGuard<'static, State> {
    state().lock().unwrap_or_else(|e| e.into_inner())
}

fn load(dir: &std::path::Path) -> State {
    State {
        t0: Instant::now(),
        driver: Driver::default(),
        revisions: (0, 0),
        slot: Slot::load(dir, NAME, ABI, Component::new),
        proposals: VecDeque::new(),
        agent: VecDeque::new(),
        writes: VecDeque::new(),
    }
}

fn wall_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// What one tick saw.
#[derive(Clone, Copy, Debug, Default)]
pub struct Observed {
    /// plugged in; `None` = unknown
    pub connected: Option<bool>,
    /// battery or charger block published something new since the last tick
    pub changed: bool,
    pub stopped: bool,
    pub capacity: u8,
    pub discharging: bool,
    /// u60d owns the part: hand the actions to the executor instead of proposing
    pub owned: bool,
}

/// Called after every battery or charger block read.
pub fn tick(hub: &Hub) {
    let fresh = |name: &str| hub.view(name).filter(|v| !v.stale);
    let (battery, charger) = (fresh("battery"), fresh("charger"));
    let revisions = (
        hub.view("battery").map_or(0, |v| v.revision),
        hub.view("charger").map_or(0, |v| v.revision),
    );
    let connected = charger
        .is_some()
        .then(|| {
            battery
                .as_ref()
                .and_then(|b| b.data["charger_connect"].as_i64())
                .map(|v| v != 0)
        })
        .flatten();
    // the agent: the charger block's value, a direct ubus read (false when that fails) otherwise
    let stopped = charger
        .as_ref()
        .and_then(|c| c.data["direct_supply"]["enabled"].as_bool())
        .unwrap_or(false);
    let sys = |f: &str| {
        std::fs::read_to_string(crate::state::host_path(&format!(
            "/sys/class/power_supply/battery/{f}"
        )))
    };
    let capacity = sys("capacity")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let discharging = sys("status")
        .map(|s| s.trim() == "Discharging")
        .unwrap_or(false);
    let (limit, manual_override) = charge::current();
    let mut g = lock();
    let changed = g.revisions != revisions;
    g.revisions = revisions;
    let now = g.t0.elapsed().as_secs_f64();
    let jobs = step(
        &mut g,
        Observed {
            connected,
            changed,
            stopped,
            capacity,
            discharging,
            owned: charge::owned(),
        },
        limit,
        manual_override,
        now,
        wall_now(),
    );
    drop(g);
    if let Some(tx) = EXECUTOR.get() {
        for j in jobs {
            let _ = tx.send(j);
        }
    }
}

/// One pass of the agent's loop: proposals instead of writes, or (owned) the executor's jobs.
fn step(
    g: &mut State,
    o: Observed,
    limit: charge::Limit,
    manual_override: bool,
    now: f64,
    wall: u64,
) -> Vec<Job> {
    let active = limit.enabled && !manual_override;
    let pass = g.driver.pass(
        Duration::from_secs_f64(now),
        o.connected,
        o.changed,
        active,
        &mut || None,
    );
    let battery = Battery {
        capacity: o.capacity,
        discharging: o.discharging,
        stopped: o.stopped,
        limit: limit.limit,
        hysteresis: limit.hysteresis,
    };
    if o.owned {
        let mut jobs = Vec::new();
        match pass.plug {
            Some(PlugStep::PluggedIn) if active => jobs.push(Job::PluggedIn),
            Some(PlugStep::Unplugged) => jobs.push(Job::Unplugged),
            _ => {}
        }
        if pass.enforce {
            jobs.push(Job::Limit);
        }
        return jobs;
    }
    let mut asks: Vec<(Ask, &'static str)> = Vec::new();
    match pass.plug {
        Some(PlugStep::PluggedIn) if active => asks.push((Ask::Enforce(battery), "plugged_in")),
        Some(PlugStep::Unplugged) => asks.push((Ask::Unplug { stopped: o.stopped }, "unplugged")),
        _ => {}
    }
    if pass.enforce {
        asks.push((Ask::Enforce(battery), "limit"));
    }
    for (ask, why) in asks {
        if let Some(allow) = decide(g, ask) {
            propose(
                g,
                Event {
                    at: now,
                    wall,
                    allow,
                    why,
                },
            );
        }
    }
    Vec::new()
}

fn decide(g: &mut State, ask: Ask) -> Option<bool> {
    g.slot.run(|c| c.call(ask), || builtin(ask))
}

/// The policy's answer now (the component, else the built-in): for the owner's executor.
pub fn decide_now(ask: Ask) -> Option<bool> {
    decide(&mut lock(), ask)
}

/// The owner wrote `power.direct_supply.set` (`allow` false = stopped charging); `why` as a
/// proposal's, or `settings` for a settings save.
pub fn owner_wrote(allow: bool, why: &'static str) {
    let mut g = lock();
    let at = g.t0.elapsed().as_secs_f64();
    push(
        &mut g.writes,
        Event {
            at,
            wall: wall_now(),
            allow,
            why,
        },
    );
}

fn propose(g: &mut State, e: Event) {
    if g.proposals
        .back()
        .is_some_and(|l| l.allow == e.allow && e.at - l.at < REPEAT_SECS)
    {
        return;
    }
    push(&mut g.proposals, e);
}

fn push(q: &mut VecDeque<Event>, e: Event) {
    if q.len() == KEEP {
        q.pop_front();
    }
    q.push_back(e);
}

/// The agent's charge limit thread wrote `power.direct_supply.set` (source auto).
pub fn agent_wrote(allow: bool) {
    let mut g = lock();
    let at = g.t0.elapsed().as_secs_f64();
    push(
        &mut g.agent,
        Event {
            at,
            wall: wall_now(),
            allow,
            why: "",
        },
    );
}

/// Pair proposals and agent writes: same direction within the window, each used once.
/// (matched, agent only, shadow only); entries younger than the window are not judged yet.
#[cfg(test)]
fn compare(
    proposals: &VecDeque<Event>,
    agent: &VecDeque<Event>,
    now: f64,
) -> (u64, Vec<Event>, Vec<Event>) {
    let mut used = vec![false; proposals.len()];
    let (mut matched, mut agent_only) = (0, Vec::new());
    for a in agent {
        let hit = proposals.iter().enumerate().find(|(i, s)| {
            !used[*i] && s.allow == a.allow && (s.at - a.at).abs() <= MATCH_WINDOW_SECS
        });
        match hit {
            Some((i, _)) => {
                used[i] = true;
                matched += 1;
            }
            None if now - a.at > MATCH_WINDOW_SECS => agent_only.push(a.clone()),
            None => {}
        }
    }
    let shadow_only = proposals
        .iter()
        .zip(&used)
        .filter(|(s, u)| !**u && now - s.at > MATCH_WINDOW_SECS)
        .map(|(s, _)| s.clone())
        .collect();
    (matched, agent_only, shadow_only)
}

/// Every input `enforce` can get that matters (capacity 0–100, both flags, a spread of limits and
/// hysteresis values), plus `on_unplug` both ways.
#[cfg(test)]
pub fn all_asks() -> Vec<Ask> {
    let mut v = Vec::new();
    for capacity in 0..=100u8 {
        for (discharging, stopped) in [(false, false), (false, true), (true, false), (true, true)] {
            for (limit, hysteresis) in [(50, 1), (80, 5), (100, 20), (60, 20)] {
                v.push(Ask::Enforce(Battery {
                    capacity,
                    discharging,
                    stopped,
                    limit,
                    hysteresis,
                }));
            }
        }
    }
    v.push(Ask::Unplug { stopped: true });
    v.push(Ask::Unplug { stopped: false });
    v
}

/// Load from `dir`, ask every case in [`all_asks`] and compare with the built-in (the component
/// parity test).
#[cfg(test)]
pub fn check(dir: &std::path::Path) -> (Value, bool) {
    let t = Instant::now();
    let mut g = load(dir);
    let load_us = t.elapsed().as_micros() as u64;
    let asks = all_asks();
    let t = Instant::now();
    let differ = asks
        .iter()
        .filter(|a| decide(&mut g, **a) != builtin(**a))
        .count();
    let us = t.elapsed().as_micros() as u64 / asks.len() as u64;
    let ok = g.slot.runs_component() && differ == 0;
    let mut v = g.slot.json();
    v["cases"] = json!(asks.len());
    v["differ"] = json!(differ);
    v["load_us"] = json!(load_us);
    v["us_per_call"] = json!(us);
    (v, ok)
}

/// The policy's status and what it did (the charge test reads it).
#[cfg(test)]
pub fn debug() -> Value {
    let (limit, manual_override) = charge::current();
    let g = lock();
    let now = g.t0.elapsed().as_secs_f64();
    let (matched, agent_only, shadow_only) = compare(&g.proposals, &g.agent, now);
    let mut v = g.slot.json();
    let more = json!({
        "owner": if charge::owned() { "u60d" } else { "agent" },
        "writes": g.writes.iter().rev().take(20).collect::<Vec<_>>(),
        "limit": {"enabled": limit.enabled, "limit": limit.limit, "hysteresis": limit.hysteresis, "manual_override": manual_override},
        "proposals": g.proposals.len(),
        "agent_writes": g.agent.len(),
        "matched": matched,
        "agent_only": agent_only,
        "shadow_only": shadow_only,
        "last": g.proposals.iter().rev().take(5).collect::<Vec<_>>(),
    });
    if let (Some(v), Value::Object(more)) = (v.as_object_mut(), more) {
        v.extend(more);
    }
    v
}

#[cfg(test)]
mod tests;
