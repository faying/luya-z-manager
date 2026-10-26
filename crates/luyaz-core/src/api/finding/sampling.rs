//! Finding-only passive admission. Reuses the five-second sampler, never sends packets.
use luyaz_api::clock::BootTime;
use serde_json::{Value, json};

use super::{DOWN_HOLD_SECS, nf};
use crate::block::{Hub, Observation};

/// Gaps beyond two sampler periods break continuity, including time spent suspended.
pub const SAMPLE_GAP: u64 = 10;
const RX_FRESH: u64 = 30;

/// Real-device test switch for the decision loop (decision-execution-loop §4.10): while this
/// file exists and is under [`RX_QUIET_MAX_S`] old, moving counters do not count as receiving.
/// Deleting a data address leaves the counters moving, so a real outage cannot otherwise be
/// staged. On the RAM disk (gone at reboot) and self-expiring; everything else stays real.
pub(crate) const RX_QUIET_FILE: &str = "/tmp/luyaz-test-rx-quiet";
pub(crate) const RX_QUIET_MAX_S: u64 = 600;

fn rx_quiet_test() -> bool {
    rx_quiet_at(std::path::Path::new(RX_QUIET_FILE))
}

/// The switch at `p` is on (the regress layer records it as a drill).
pub(crate) fn rx_quiet_at(p: &std::path::Path) -> bool {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age.as_secs() <= RX_QUIET_MAX_S)
}

#[derive(Clone, PartialEq, Eq)]
pub struct Connection {
    pub iccid: String,
    // Includes failures and A→B→A changes observed by the collector, not just final equality.
    // Any SIM/interfaces change is conservative invalidation (traffic counters excluded, see
    // `block::Observation::generation`), even an unrelated LAN change.
    generations: [u64; 2],
}

#[derive(Clone)]
pub struct Readings {
    pub state: Value,
    pub connection: Connection,
    pub observed: BootTime,
    live_at: BootTime,
}

impl Readings {
    pub fn capture(hub: &Hub) -> Option<Self> {
        Self::from_observations(
            hub.observations(&["signal", "interfaces", "live", "sim"]),
            &BootTime::read()?,
        )
    }

    fn from_observations(blocks: Vec<Observation>, now: &BootTime) -> Option<Self> {
        let get = |name| blocks.iter().find(|b| b.name == name);
        let mut oldest = now.clone();
        for name in ["signal", "interfaces", "live", "sim"] {
            let at = get(name)?.observed.as_ref()?;
            if now.elapsed_since(at)? > nf::path::MAX_AGE {
                return None;
            }
            oldest.seconds = oldest.seconds.min(at.seconds);
        }
        let sim = get("sim")?;
        let iccid = sim.data["iccid"].as_str()?.trim();
        if iccid.is_empty() {
            return None;
        }
        Some(Self {
            state: json!({
                "net": get("signal")?.data,
                "interfaces": get("interfaces")?.data,
                "traffic": get("live")?.data.get("traffic").cloned(),
            }),
            connection: Connection {
                iccid: iccid.into(),
                generations: [sim.generation, get("interfaces")?.generation],
            },
            observed: oldest,
            live_at: get("live")?.observed.clone()?,
        })
    }
}

#[derive(Default)]
pub struct Passive {
    last: Option<BootTime>,
    connection: Option<Connection>,
    rx: Option<u64>,
    live_at: Option<BootTime>,
    moved: Option<BootTime>,
    down_since: Option<BootTime>,
    candidate: bool,
    known: bool,
}

fn connected(status: Option<&str>) -> Option<bool> {
    match status? {
        "connected" | "ipv4_connected" | "ipv6_connected" | "ipv4_ipv6_connected" => Some(true),
        "disconnected"
        | "connecting"
        | "ipv4_disconnected"
        | "ipv6_disconnected"
        | "ipv4_ipv6_disconnected" => Some(false),
        _ => None,
    }
}

impl Passive {
    pub fn sample(&mut self, readings: Option<&Readings>, now: Option<&BootTime>) {
        let Some((r, now)) = readings.zip(now).filter(|(r, now)| {
            now.elapsed_since(&r.observed)
                .is_some_and(|age| age <= nf::path::MAX_AGE)
        }) else {
            *self = Self::default();
            return;
        };
        let continuous = self
            .last
            .as_ref()
            .and_then(|t| now.elapsed_since(t))
            .is_some_and(|age| age > 0 && age <= SAMPLE_GAP)
            && self.connection.as_ref() == Some(&r.connection);
        if !continuous {
            *self = Self::default();
        }
        let rx = luyaz_netwatch::cell_rx_bytes(Some(&r.state));
        let status = connected(r.state["interfaces"]["cellular"]["connect_status"].as_str());
        let advanced = self
            .live_at
            .as_ref()
            .and_then(|t| r.live_at.elapsed_since(t))
            .is_some_and(|age| age > 0);
        let reset = matches!((self.rx, rx), (Some(a), Some(b)) if b < a);
        if advanced && matches!((self.rx, rx), (Some(a), Some(b)) if b > a) {
            self.moved = Some(r.live_at.clone());
        }
        let receiving = self
            .moved
            .as_ref()
            .and_then(|t| now.elapsed_since(t))
            .is_some_and(|age| age <= RX_FRESH)
            && !rx_quiet_test();
        // Quiet counters are a reason to check, never proof of an internet outage.
        // Missing counter/status or a counter reset starts a new observation, not a fault.
        self.known =
            status.is_some() && rx.is_some() && !reset && !(status == Some(false) && receiving);
        self.candidate = self.known && !receiving;
        if !self.known {
            self.moved = None;
        }
        if !self.candidate {
            self.down_since = None;
        } else if self.down_since.is_none() {
            self.down_since = Some(now.clone());
        }
        self.rx = rx;
        self.live_at = Some(r.live_at.clone());
        self.connection = Some(r.connection.clone());
        self.last = Some(now.clone());
    }

    /// The readings belong to the sampled connection, are fresh, and the samples are unbroken.
    fn current(&self, r: &Readings, now: &BootTime) -> bool {
        self.connection.as_ref() == Some(&r.connection)
            && now
                .elapsed_since(&r.observed)
                .is_some_and(|age| age <= nf::path::MAX_AGE)
            && self
                .last
                .as_ref()
                .and_then(|t| now.elapsed_since(t))
                .is_some_and(|age| age <= SAMPLE_GAP)
            && self.known
    }

    /// Seconds the passive reading has said down without a break (0 = up); None = can't tell.
    /// The decision loop's `passive_down_s` (decision-execution-loop §4.9).
    pub fn down_secs(&self, r: Option<&Readings>, now: Option<&BootTime>) -> Option<u64> {
        let (r, now) = r.zip(now)?;
        if !self.current(r, now) {
            return None;
        }
        if !self.candidate {
            return Some(0);
        }
        now.elapsed_since(self.down_since.as_ref()?)
    }

    pub fn admission(&self, r: Option<&Readings>, now: Option<&BootTime>) -> (bool, bool) {
        let Some((r, now)) = r.zip(now) else {
            return (true, false);
        };
        // Unknown must not be reported as an up observation (which resolves stored findings).
        if !self.current(r, now) {
            return (true, false);
        }
        let candidate = self.candidate;
        (
            candidate,
            candidate
                && self
                    .down_since
                    .as_ref()
                    .and_then(|t| now.elapsed_since(t))
                    .is_some_and(|age| age >= DOWN_HOLD_SECS),
        )
    }
}

#[cfg(test)]
mod tests;
