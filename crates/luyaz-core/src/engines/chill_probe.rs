//! CHILL end to end (docs/research/network-reliability-underlay-20261021.md §9): mihomo's API can
//! answer while nothing gets through it, and chill.sh only watches the API. Here, at most once per
//! [`RECHECK_SECS`](crate::api::finding::RECHECK_SECS) (its own probe budget file, so the user's
//! 网络诊断 never waits on it) and only while somebody can use it (CHILL on, not paused or busy, a
//! fresh client count above 0), one `generate_204` goes through mihomo ([`crate::ops::chill::probe`]).
//! Two misses in a row while the API answers = forwarding broken, shown in `network.health`; one
//! success clears it. Detection only: a restart drops every client's connections and does nothing
//! for dead upstream nodes, so repairing stays with the owner.

use crate::ops::chill::Probe;
use luyaz_api::alerts::clock::BootTime;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const BUDGET: &str = "/data/luyaz/core/chill-probe-budget.json";
/// Local reads between probes: cheap, no packets.
const TICK_S: u64 = 60;
/// A verdict older than this is not shown (the probe stopped: off, idle, nobody there).
const STALE_S: u64 = 1800;

/// What the probe found lately.
#[derive(Debug, Default)]
pub struct Track {
    misses: u32,
    /// last verdict and when (unix seconds)
    last: Option<(bool, u64)>,
}

impl Track {
    /// One result in. Unknown (API or entry down) is chill.sh's to handle: it says nothing here.
    pub fn note(&mut self, p: Probe, now: u64) {
        match p {
            Probe::Up => {
                self.misses = 0;
                self.last = Some((true, now));
            }
            Probe::Down => {
                self.misses += 1;
                if self.misses >= 2 {
                    self.last = Some((false, now));
                }
            }
            Probe::Unknown => {}
        }
    }

    pub fn now(&self, now: u64) -> Option<bool> {
        self.last
            .filter(|(_, at)| now.saturating_sub(*at) < STALE_S)
            .map(|(up, _)| up)
    }
}

/// Somebody can use CHILL now and it is in a state to be measured.
pub fn wanted(meant_on: bool, idle_paused: bool, busy: bool, clients: Option<i64>) -> bool {
    meant_on && !idle_paused && !busy && clients.is_some_and(|n| n > 0)
}

fn track() -> &'static Mutex<Track> {
    static T: std::sync::OnceLock<Mutex<Track>> = std::sync::OnceLock::new();
    T.get_or_init(Default::default)
}

/// For `network.health`: Some(false) = forwarding broken, Some(true) = got through lately.
pub fn now() -> Option<bool> {
    track()
        .lock()
        .ok()?
        .now(luyaz_netwatch::now_unix())
}

pub fn spawn(hub: Arc<crate::block::Hub>) {
    tokio::spawn(async move {
        let path = PathBuf::from(crate::state::host_path(BUDGET));
        let mut budget =
            crate::api::finding::budget::Budget::load(path, BootTime::read().as_ref());
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(TICK_S)).await;
            let clients = hub
                .view("clients")
                .filter(|v| !v.stale)
                .and_then(|v| v.data["total"].as_i64());
            let go = wanted(
                super::chill_idle::meant_on(),
                crate::ops::chill::idle_paused(),
                crate::ops::chill::busy(),
                clients,
            );
            if !go || !budget.reserve(BootTime::read().as_ref()) {
                continue;
            }
            let p = crate::ops::chill::probe().await;
            let before = now();
            if let Ok(mut t) = track().lock() {
                t.note(p, luyaz_netwatch::now_unix());
            }
            let after = now();
            if before != after {
                eprintln!("[chill-probe] {p:?}: forwarding {after:?} (was {before:?})");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_misses_make_it_down_one_success_clears() {
        let mut t = Track::default();
        t.note(Probe::Down, 100);
        assert_eq!(t.now(100), None, "one miss is not enough");
        t.note(Probe::Unknown, 700);
        t.note(Probe::Down, 1300);
        assert_eq!(t.now(1300), Some(false), "Unknown in between neither counts nor resets");
        t.note(Probe::Up, 1900);
        assert_eq!(t.now(1900), Some(true));
        t.note(Probe::Down, 2500);
        assert_eq!(t.now(2500), Some(true), "a single miss after a success keeps the success");
    }

    #[test]
    fn an_old_verdict_is_not_shown() {
        let mut t = Track::default();
        t.note(Probe::Down, 0);
        t.note(Probe::Down, 600);
        assert_eq!(t.now(600 + STALE_S - 1), Some(false));
        assert_eq!(t.now(600 + STALE_S), None);
    }

    #[test]
    fn only_with_somebody_there() {
        assert!(wanted(true, false, false, Some(2)));
        assert!(!wanted(true, false, false, Some(0)));
        assert!(!wanted(true, false, false, None), "unknown clients: no packets");
        assert!(!wanted(false, false, false, Some(2)));
        assert!(!wanted(true, true, false, Some(2)));
        assert!(!wanted(true, false, true, Some(2)));
    }
}
