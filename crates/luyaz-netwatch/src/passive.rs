//! Passive link evidence: raw wall labels never establish freshness. No IO or probes.
use super::{operator_dns, wan_connected, RX_FRESH_SECS};
use luyaz_api::clock::BootTime;
use serde_json::Value;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    /// Raw wall label for history only; never use it for freshness.
    pub at: u64,
    pub observed_at: Option<BootTime>,
    /// the core's `wan_status` says a data call is up. `None` = core unreadable.
    pub connected: Option<bool>,
    pub rx_bytes: Option<u64>,
    /// Last proven receive-counter increase, on the same boot's elapsed clock.
    pub rx_moved_at: Option<BootTime>,
    /// Operator DNS servers (IPv4), from the core's `wan_dns`.
    pub dns: Vec<String>,
    /// The IPv4 cellular path of this same observation (Finding's [`crate::finding::path::paths`]):
    /// the direct DNS query binds to it (R18-P0-02). Never carried over from an older sample;
    /// `None` = interface unknown, and then no query is sent.
    pub path: Option<crate::finding::path::Path>,
}

/// Observation uncertainty must not become either proof of a working link or a
/// reason to issue an automatic DNS probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PassiveLink {
    Unknown,
    Down,
    Receiving,
    NeedsProbe,
}

pub(super) fn fresh(observed: Option<&BootTime>, now: Option<&BootTime>, max_age: u64) -> bool {
    observed
        .zip(now)
        .and_then(|(a, b)| b.elapsed_since(a))
        .is_some_and(|age| age <= max_age)
}

pub fn alive_without_probe(s: &Snapshot, now: Option<&BootTime>) -> PassiveLink {
    if !fresh(s.observed_at.as_ref(), now, RX_FRESH_SECS) || s.connected.is_none() {
        return PassiveLink::Unknown;
    }
    if s.connected == Some(false) {
        return PassiveLink::Down;
    }
    if s.rx_bytes.is_none() {
        return PassiveLink::Unknown;
    }
    if fresh(s.rx_moved_at.as_ref(), now, RX_FRESH_SECS) {
        return PassiveLink::Receiving;
    }
    PassiveLink::NeedsProbe
}

/// Wall time labels the history; elapsed time alone establishes freshness.
/// A missing read, reset, reconnect, boot change or sampling gap breaks the pair.
pub fn sample(
    prev: &Snapshot,
    now: u64,
    elapsed: Option<&BootTime>,
    state: Option<&Value>,
    rx: Option<u64>,
) -> Snapshot {
    let net = state.map(|v| v.get("net").unwrap_or(v));
    let connected = net
        .and_then(|n| n.get("wan_status"))
        .and_then(Value::as_str)
        .map(wan_connected);
    let dns = state
        .map(operator_dns)
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| prev.dns.clone());
    let continuous = connected == Some(true)
        && prev.connected == Some(true)
        && prev
            .observed_at
            .as_ref()
            .zip(elapsed)
            .and_then(|(a, b)| b.elapsed_since(a))
            .is_some_and(|age| age > 0 && age <= RX_FRESH_SECS);
    let paired = continuous && prev.rx_bytes.zip(rx).is_some_and(|(a, b)| b >= a);
    let moved = paired && prev.rx_bytes.zip(rx).is_some_and(|(a, b)| b > a);
    Snapshot {
        at: now,
        observed_at: elapsed.filter(|t| !t.boot_id.is_empty()).cloned(),
        connected,
        rx_bytes: rx,
        rx_moved_at: if moved {
            elapsed.cloned()
        } else if paired {
            prev.rx_moved_at.clone()
        } else {
            None
        },
        dns,
        path: state.and_then(|v| {
            crate::finding::path::paths(v)
                .into_iter()
                .find(|p| p.source.is_ipv4())
        }),
    }
}

#[cfg(test)]
#[path = "passive_tests.rs"]
mod tests;
