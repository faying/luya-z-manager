//! "Registered but no internet"'s runner (u60-mcp.md step 1 phase 1; T2): gathers the four
//! layers through a [`FindingHost`] and hands them to `luyaz_api::finding::judge`. The core
//! implements the host (its state, `at.rs` directly: never `/v2/api/at/send`, which sends
//! `op.interrupt` first) and keeps the open findings (T3).
//!
//! Gate (D4): the probes take the same gate as the deep diagnosis, speed test and search/register.
//! Unlike the deep diagnosis it does not wait: busy means try at the next round (rechecks are at
//! most every 10 minutes anyway). It claims `Gate::finding_since`, not `diag_since`: a deep
//! diagnosis asked meanwhile waits for it and reuses what it saw (deep-diag-v2 §2.1). Transactions join the gate in T4.
//!
//! With a client, held-down observation and cooldown satisfied: at most two bound HTTP
//! attempts and two bound operator DNS attempts, shared across families. No unbound fallback,
//! AT, TLS bypass or new periodic task. HTTP reachability is not internet usability.

use std::sync::Mutex;

use luyaz_api::diagnose::Gate;
use luyaz_api::finding::*;

use crate::now_unix;

pub mod path;
pub mod store;

pub trait FindingHost {
    fn premises(&self) -> Premises;
    /// The collection round's passive reading says down or can't tell.
    fn passive_down(&self) -> bool;
    fn can_probe(&self) -> bool;
    fn iccid(&self) -> String;
    /// A search, a manual register or a speed test going.
    fn others_busy(&self) -> Option<&'static str>;
    fn registration(&self) -> Reg;
    fn data(&self) -> DataIn;
    /// Recheck after blocking probes; an old snapshot cannot establish a current root cause.
    fn readings_current(&self) -> bool {
        true
    }
    fn network_probe(&self) -> NetworkEvidence;
}

/// Another operation holds the gate.
#[derive(Debug, PartialEq, Eq)]
pub struct Busy(pub &'static str);

/// One check. Reads first; probes only when the verdict could become "open".
pub fn check(host: &dyn FindingHost, gate: &Mutex<Gate>) -> Result<Verdict, Busy> {
    let now = now_unix();
    let mut c = Check {
        premises: host.premises(),
        passive_down: host.passive_down(),
        can_probe: host.can_probe(),
        iccid: host.iccid(),
        layers: Vec::new(),
        now,
    };
    if !c.passive_down || !c.can_probe || expected_outage(&c.premises) != Ok(None) {
        return Ok(judge(&c));
    }
    let reg = host.registration();
    c.layers.push(judge_signal(reg, now_unix()));
    c.layers.push(judge_data(&host.data(), now_unix()));
    if !reg.registered() {
        return Ok(judge(&c));
    }
    {
        let mut g = gate.lock().unwrap_or_else(|e| e.into_inner());
        if g.diag_since.is_some() {
            return Err(Busy("diagnose"));
        }
        if let Some(k) = host.others_busy() {
            return Err(Busy(k));
        }
        // its own field (deep-diag-v2 §2.1): never shown or timed as a diagnosis
        g.finding_since = Some(now);
    }
    let probed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let network = host.network_probe();
        judge_network(network, now_unix())
    }));
    gate.lock().unwrap_or_else(|e| e.into_inner()).finding_since = None;
    let (internet, dns) = probed.unwrap_or_else(|_| {
        let t = now_unix();
        judge_network(
            NetworkEvidence {
                reason: "probe_panicked".into(),
                ..NetworkEvidence::default()
            },
            t,
        )
    });
    if !host.readings_current() {
        return Ok(Verdict::Unverified {
            why: "snapshot_expired",
        });
    }
    c.layers.push(internet);
    c.layers.push(dns);
    c.now = now_unix();
    Ok(judge(&c))
}

fn any_status(head: &[u8]) -> bool {
    head.starts_with(b"HTTP/1.") && head.len() >= 12 && head[9..12].iter().all(u8::is_ascii_digit)
}

#[cfg(test)]
mod tests;
