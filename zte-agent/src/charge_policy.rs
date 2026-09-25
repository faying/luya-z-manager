use std::fs;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::datad_feed::{self, FALLBACK_POLL, MAX_WAIT};
use crate::ubus;

const STORAGE_PATH: &str = "/data/local/tmp/charge_limit.json";
const POLL_ACTIVE_SECS: u64 = 60;
const POLL_IDLE_SECS: u64 = 300;
const DEFAULT_HYSTERESIS: u8 = 5;

const SYSFS_CAPACITY: &str = "/sys/class/power_supply/battery/capacity";
const SYSFS_STATUS: &str = "/sys/class/power_supply/battery/status";

fn default_hysteresis() -> u8 {
    DEFAULT_HYSTERESIS
}

#[derive(Serialize, Deserialize)]
struct Persisted {
    enabled: bool,
    limit: u8,
    #[serde(default = "default_hysteresis")]
    hysteresis: u8,
}

struct LimitState {
    enabled: bool,
    limit: u8,
    hysteresis: u8,
    manual_override: bool,
}

pub struct ChargeLimitEnforcer {
    inner: Mutex<LimitState>,
}

/// Check if charging is currently stopped via ubus (ground truth).
/// `direct_power_supply_mode: "enable"` = charging STOPPED (inverted naming).
fn is_charging_stopped() -> bool {
    ubus::call("zwrt_bsp.charger", "list", Some("{}"))
        .ok()
        .and_then(|v| {
            v["direct_power_supply_mode"]
                .as_str()
                .map(|s| s == "enable")
        })
        .unwrap_or(false)
}

/// Set charging state via ubus (inverted: "enable" = stop, "disable" = start)
fn set_charging(allow: bool) {
    let mode = if allow { "disable" } else { "enable" };
    let params = format!(r#"{{"direct_power_supply_mode":"{mode}"}}"#);
    let _ = ubus::call("zwrt_bsp.charger", "set", Some(&params));
}

/// Direct read of `charger_connect` (fallback path and first look).
pub fn direct_charger_connected() -> Option<bool> {
    let v = ubus::call("zwrt_bsp.charger", "list", Some("{}")).ok()?;
    match &v["charger_connect"] {
        Value::Number(n) => Some(n.as_u64() != Some(0)),
        Value::String(s) => Some(s != "0"),
        _ => None,
    }
}

/// What a plug-state observation calls for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlugStep {
    Nothing,
    /// Plugged in (or first seen plugged in): enforce the limit now.
    PluggedIn,
    /// Unplugged: make sure charging is re-enabled for the next plug-in.
    Unplugged,
}

/// The one "last known connected" state, shared by both modes and never reset
/// on a mode change: a recovery snapshot saying "connected" after the
/// fallback already saw the plug-in is not a second plug-in. `None` observed
/// = don't know — never taken as "unplugged".
fn plug_step(prev: &mut Option<bool>, observed: Option<bool>) -> PlugStep {
    let Some(now) = observed else {
        return PlugStep::Nothing;
    };
    if *prev == Some(now) {
        return PlugStep::Nothing;
    }
    *prev = Some(now);
    if now {
        PlugStep::PluggedIn
    } else {
        PlugStep::Unplugged
    }
}

/// Loop bookkeeping, pure over a monotonic `now` so tests can drive it.
#[derive(Debug, Default)]
struct Driver {
    connected: Option<bool>,
    last_direct: Option<Duration>,
    last_enforce: Option<Duration>,
}

/// What the loop should do this pass.
#[derive(Debug, Default, PartialEq)]
struct Pass {
    plug: Option<PlugStep>,
    /// Periodic / change-driven enforcement while connected.
    enforce: bool,
    /// Longest the loop may wait before the next pass.
    wait: Duration,
}

impl Driver {
    /// `feed` = charger_connect from a subscribed, fresh feed (`None` when
    /// not subscribed or unknown). `direct` is only called when the feed has
    /// no answer and the last direct read is [`FALLBACK_POLL`] old.
    /// `changed` = woken by a feed change (battery/charger moved).
    fn pass(
        &mut self,
        now: Duration,
        feed: Option<bool>,
        changed: bool,
        active: bool,
        direct: &mut dyn FnMut() -> Option<bool>,
    ) -> Pass {
        let from_feed = feed.is_some();
        let observed = match feed {
            Some(c) => Some(c),
            None => {
                let due = self.last_direct.is_none_or(|t| now.saturating_sub(t) >= FALLBACK_POLL);
                if due {
                    self.last_direct = Some(now);
                    direct()
                } else {
                    None
                }
            }
        };
        let step = plug_step(&mut self.connected, observed);
        let mut out = Pass { plug: (step != PlugStep::Nothing).then_some(step), ..Pass::default() };
        let period = Duration::from_secs(if active { POLL_ACTIVE_SECS } else { POLL_IDLE_SECS });
        if step == PlugStep::PluggedIn {
            self.last_enforce = Some(now);
        } else if self.connected == Some(true) && active {
            let due = self.last_enforce.is_none_or(|t| now.saturating_sub(t) >= period);
            if due || (changed && from_feed) {
                out.enforce = true;
                self.last_enforce = Some(now);
            }
        }
        let mut wait = MAX_WAIT;
        if !from_feed {
            let next = self.last_direct.map_or(Duration::ZERO, |t| (t + FALLBACK_POLL).saturating_sub(now));
            wait = wait.min(next);
        }
        out.wait = wait.max(Duration::from_millis(100));
        out
    }
}

impl ChargeLimitEnforcer {
    pub fn new() -> Self {
        let persisted = fs::read_to_string(STORAGE_PATH)
            .ok()
            .and_then(|s| serde_json::from_str::<Persisted>(&s).ok());

        let (enabled, limit, hysteresis) = match persisted {
            Some(p) => (p.enabled, p.limit.clamp(50, 100), p.hysteresis.clamp(1, 20)),
            None => (false, 100, DEFAULT_HYSTERESIS),
        };

        ChargeLimitEnforcer {
            inner: Mutex::new(LimitState {
                enabled,
                limit,
                hysteresis,
                manual_override: false,
            }),
        }
    }

    /// Start the policy thread. Plug state comes from the datad feed while
    /// subscribed, from a direct ubus read every [`FALLBACK_POLL`] otherwise;
    /// the thread wakes on every feed change and at least every 60 s.
    pub fn start(self: &Arc<Self>) {
        let enforcer = Arc::clone(self);
        std::thread::spawn(move || enforcer.event_loop());
    }

    fn event_loop(&self) {
        let t0 = Instant::now();
        let mut driver = Driver::default();
        let mut seen = 0u64;
        let mut wait = Duration::ZERO;
        loop {
            let view = datad_feed::wait(seen, wait);
            let changed = view.version != seen;
            seen = view.version;
            let (enabled, manual_override, limit, hysteresis) = {
                let st = self.inner.lock().unwrap();
                (st.enabled, st.manual_override, st.limit, st.hysteresis)
            };
            let active = enabled && !manual_override;
            let pass = driver.pass(t0.elapsed(), view.charger_connected(), changed, active, &mut direct_charger_connected);
            match pass.plug {
                Some(PlugStep::PluggedIn) => {
                    eprintln!("[charge_policy] charger connected — enforcing");
                    if active {
                        std::thread::sleep(Duration::from_millis(500));
                        self.enforce(limit, hysteresis);
                    }
                }
                Some(PlugStep::Unplugged) => {
                    eprintln!("[charge_policy] charger disconnected — suspending enforcement");
                    // Re-enable charging so the next plug-in starts normally.
                    if is_charging_stopped() {
                        set_charging(true);
                        eprintln!("[charge_policy] re-enabled charging (was stopped by limit)");
                    }
                }
                _ => {}
            }
            if pass.enforce {
                self.enforce(limit, hysteresis);
            }
            wait = pass.wait;
        }
    }

    /// Core enforcement logic: read capacity and stop/resume charging as needed.
    ///
    /// While subscribed, "stopped" comes from the charger block; before
    /// writing anything the ubus value is re-read, so a charger block that has
    /// not caught up with our own last write never causes a second write.
    fn enforce(&self, limit: u8, hysteresis: u8) {
        let feed_stopped = datad_feed::global().and_then(|f| f.view().charging_stopped());
        let stopped = feed_stopped.unwrap_or_else(is_charging_stopped);

        let status = fs::read_to_string(SYSFS_STATUS).unwrap_or_default();
        if status.trim() == "Discharging" && !stopped {
            return;
        }

        let capacity: u8 = fs::read_to_string(SYSFS_CAPACITY)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);

        let want_stop = capacity >= limit && !stopped;
        let want_start = capacity <= limit.saturating_sub(hysteresis) && stopped;
        if !want_stop && !want_start {
            return;
        }
        let stopped = if feed_stopped.is_some() { is_charging_stopped() } else { stopped };
        if want_stop && !stopped {
            set_charging(false);
        } else if want_start && stopped {
            set_charging(true);
        }
    }

    pub fn get(&self) -> (bool, u8, u8, bool) {
        let state = self.inner.lock().unwrap();
        (state.enabled, state.limit, state.hysteresis, state.manual_override)
    }

    pub fn set(&self, enabled: bool, limit: u8, hysteresis: u8) -> Result<(), String> {
        if limit < 50 || limit > 100 {
            return Err("limit must be 50-100".into());
        }
        if hysteresis < 1 || hysteresis > 20 {
            return Err("hysteresis must be 1-20".into());
        }
        let mut state = self.inner.lock().unwrap();
        state.enabled = enabled;
        state.limit = limit;
        state.hysteresis = hysteresis;
        state.manual_override = false;
        self.save_locked(&state);
        drop(state);

        if enabled {
            self.enforce(limit, hysteresis);
        } else {
            set_charging(true);
        }

        Ok(())
    }

    pub fn set_manual_override(&self, override_on: bool) {
        let mut state = self.inner.lock().unwrap();
        state.manual_override = override_on;
    }

    fn save_locked(&self, state: &LimitState) {
        let p = Persisted {
            enabled: state.enabled,
            limit: state.limit,
            hysteresis: state.hysteresis,
        };
        if let Ok(json) = serde_json::to_string(&p) {
            let _ = fs::write(STORAGE_PATH, json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(x: u64) -> Duration {
        Duration::from_secs(x)
    }

    /// 拔电 → datad 断开 → 插电 → datad 恢复: the plug-in during fallback is
    /// acted on within 60 s, and the recovery does not act on it again.
    #[test]
    fn plug_in_during_fallback_enforced_once() {
        let mut d = Driver::default();
        let mut direct_calls = 0;
        let mut plugged = false;
        let mut acts: Vec<(u64, PlugStep)> = Vec::new();

        // Subscribed, unplugged. Start: one "unplugged" (re-enable check).
        let mut now = 0u64;
        let p = d.pass(s(now), Some(false), true, true, &mut || panic!("no direct read while subscribed"));
        assert_eq!(p.plug, Some(PlugStep::Unplugged));
        // Unplug is old news: nothing more while subscribed.
        now = 30;
        assert_eq!(d.pass(s(now), Some(false), false, true, &mut || unreachable!()).plug, None);

        // datad drops at 100: the feed has no answer; the loop polls directly.
        // Plug-in lands at 101, just after a direct read — the worst case.
        let plug_at = 101;
        now = 100;
        let mut wait;
        loop {
            if now >= plug_at {
                plugged = true;
            }
            let mut direct = || {
                direct_calls += 1;
                Some(plugged)
            };
            let p = d.pass(s(now), None, now == 100, true, &mut direct);
            if let Some(step) = p.plug {
                acts.push((now, step));
            }
            wait = p.wait;
            assert!(wait <= MAX_WAIT);
            if !acts.is_empty() {
                break;
            }
            now += wait.as_secs().max(1);
            assert!(now < 400, "never acted");
        }
        let (at, step) = acts[0];
        assert_eq!(step, PlugStep::PluggedIn);
        assert!(at - plug_at <= 60, "acted {}s after plug-in", at - plug_at);
        assert!(direct_calls <= 3, "low-frequency direct reads, got {direct_calls}");

        // datad back at 200: the snapshot says connected. Not a new plug-in.
        for t in [200, 230, 260, 300, 400] {
            let p = d.pass(s(t), Some(true), t == 200, true, &mut || panic!("subscribed: no direct"));
            assert_eq!(p.plug, None, "recovery at {t} must not repeat the plug-in action");
        }
    }

    #[test]
    fn unknown_never_counts_as_unplugged() {
        let mut d = Driver::default();
        assert_eq!(d.pass(s(0), Some(true), true, true, &mut || None).plug, Some(PlugStep::PluggedIn));
        // Fallback, direct read fails: no transition, no re-enable.
        for t in [10, 40, 80, 120] {
            assert_eq!(d.pass(s(t), None, false, true, &mut || None).plug, None);
        }
    }

    #[test]
    fn periodic_enforce_while_connected_and_waits_capped() {
        let mut d = Driver::default();
        d.pass(s(0), Some(true), true, true, &mut || None);
        let p = d.pass(s(30), Some(true), false, true, &mut || None);
        assert!(!p.enforce);
        assert_eq!(p.wait, MAX_WAIT);
        assert!(d.pass(s(60), Some(true), false, true, &mut || None).enforce);
        // A battery/charger change while connected enforces at once.
        assert!(d.pass(s(70), Some(true), true, true, &mut || None).enforce);
        // Fallback: next pass no later than the next direct read.
        let p = d.pass(s(80), None, true, true, &mut || Some(true));
        assert!(p.wait <= FALLBACK_POLL);
    }
}
