//! Charge limit policy, moved from zte-agent `src/charge_policy.rs` (manager 7d2ca07).
//!
//! `enforce` and `on_unplug` are the policy (what the charge-policy component exports,
//! `wit/charge-policy.wit`): given the battery and the limit, whether to stop or resume charging.
//! `Driver` is when to ask (plug transitions, periodic and change-driven passes); it stays in the
//! host. An answer is `Some(allow)`: `false` = stop charging, `true` = resume; the host turns it
//! into `power.direct_supply.set {enabled: !allow}` (direct supply on = charging stopped).
//!
//! Deliberate differences from the agent: none in the decisions. The agent re-reads the charger
//! over ubus right before writing; the host does that when it writes (the shadow never writes).

use std::time::Duration;

pub const POLL_ACTIVE_SECS: u64 = 60;
pub const POLL_IDLE_SECS: u64 = 300;
pub const DEFAULT_HYSTERESIS: u8 = 5;
/// The agent's core feed fallback: a direct read at most this often (`core_feed::FALLBACK_POLL`).
pub const FALLBACK_POLL: Duration = Duration::from_secs(30);
/// Longest wait between passes (`core_feed::MAX_WAIT`).
pub const MAX_WAIT: Duration = Duration::from_secs(60);

/// What `enforce` looks at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Battery {
    /// sysfs `capacity`; 0 when unreadable (as the agent).
    pub capacity: u8,
    /// sysfs `status` is `Discharging`.
    pub discharging: bool,
    /// Charging is stopped by direct supply.
    pub stopped: bool,
    pub limit: u8,
    pub hysteresis: u8,
}

/// Stop at the limit, resume at limit − hysteresis; nothing while discharging on its own.
pub fn enforce(b: &Battery) -> Option<bool> {
    if b.discharging && !b.stopped {
        return None;
    }
    if b.capacity >= b.limit && !b.stopped {
        return Some(false);
    }
    if b.capacity <= b.limit.saturating_sub(b.hysteresis) && b.stopped {
        return Some(true);
    }
    None
}

/// Unplugged: make sure charging is re-enabled for the next plug-in (limit on or off).
pub fn on_unplug(stopped: bool) -> Option<bool> {
    stopped.then_some(true)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlugStep {
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
pub fn plug_step(prev: &mut Option<bool>, observed: Option<bool>) -> PlugStep {
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
pub struct Driver {
    connected: Option<bool>,
    last_direct: Option<Duration>,
    last_enforce: Option<Duration>,
}

/// What the loop should do this pass.
#[derive(Debug, Default, PartialEq)]
pub struct Pass {
    pub plug: Option<PlugStep>,
    /// Periodic / change-driven enforcement while connected.
    pub enforce: bool,
    /// Longest the loop may wait before the next pass.
    pub wait: Duration,
}

impl Driver {
    /// `feed` = charger_connect from a fresh feed (`None` when unknown). `direct` is only called
    /// when the feed has no answer and the last direct read is [`FALLBACK_POLL`] old.
    /// `changed` = woken by a feed change (battery/charger moved).
    pub fn pass(
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
                let due = self
                    .last_direct
                    .is_none_or(|t| now.saturating_sub(t) >= FALLBACK_POLL);
                if due {
                    self.last_direct = Some(now);
                    direct()
                } else {
                    None
                }
            }
        };
        let step = plug_step(&mut self.connected, observed);
        let mut out = Pass {
            plug: (step != PlugStep::Nothing).then_some(step),
            ..Pass::default()
        };
        let period = Duration::from_secs(if active {
            POLL_ACTIVE_SECS
        } else {
            POLL_IDLE_SECS
        });
        if step == PlugStep::PluggedIn {
            self.last_enforce = Some(now);
        } else if self.connected == Some(true) && active {
            let due = self
                .last_enforce
                .is_none_or(|t| now.saturating_sub(t) >= period);
            if due || (changed && from_feed) {
                out.enforce = true;
                self.last_enforce = Some(now);
            }
        }
        let mut wait = MAX_WAIT;
        if !from_feed {
            let next = self
                .last_direct
                .map_or(Duration::ZERO, |t| (t + FALLBACK_POLL).saturating_sub(now));
            wait = wait.min(next);
        }
        out.wait = wait.max(Duration::from_millis(100));
        out
    }
}

#[cfg(test)]
mod tests;
