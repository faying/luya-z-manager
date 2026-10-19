//! `data.no_internet` replay (decision-execution-loop.md §2, §4 B5): from one evidence set to a
//! read-only suggestion or unknown, plus which entry conditions still keep it read-only. It never
//! produces an executable proposal in this batch: the B31 data-session reconnect is unverified,
//! there is no authoritative connection generation, no O2 recheck and no wired gate.

use super::Check;
use serde::Serialize;

/// The evidence the replay looks at. None = not read / can't tell.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Evidence {
    /// Seconds of consecutive qualifying passive-down samples.
    pub passive_down_s: Option<u64>,
    /// Expected outage (no SIM, airplane, data off, roaming off).
    pub expected_outage: Option<bool>,
    /// Same card as when the fault was first seen.
    pub same_sim: Option<bool>,
    pub registered: Option<bool>,
    pub data_session: Option<bool>,
    pub has_ip: Option<bool>,
    pub dns: Option<Dns>,
    /// Multi-target HTTPS over the cellular path.
    pub https: Option<Check>,
    /// The clock is trusted (certificates can be judged).
    pub clock_trusted: Option<bool>,
    /// The probe path (family / interface / source / route) is the cellular one.
    pub path_bound: Option<bool>,
    /// Newest reading's age in seconds (BOOTTIME, same boot).
    pub age_s: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Dns {
    Ok,
    NxDomain,
    Fail,
}

/// The first bad layer, when the evidence shows one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    Registration,
    DataSession,
    Ip,
    Dns,
    Exit,
}

/// Entry conditions (§3) still missing for any automatic action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Missing {
    /// Multi-target HTTPS + trusted clock + verified policy routing recheck.
    O2,
    /// Per-action rollback capability and completion evidence.
    O3,
    /// Atomic Finding revision + SIM + authoritative connection generation.
    O5,
    /// The admission gate wired into the engine's write path.
    T4,
    /// B31's safe data-session reconnect interface, unverified.
    B31Reconnect,
}

const ALL_MISSING: [Missing; 5] = [
    Missing::O2,
    Missing::O3,
    Missing::O5,
    Missing::T4,
    Missing::B31Reconnect,
];

/// Same as the pilot (`finding.rs`): 60 s of passive-down, readings at most 30 s old.
pub const HOLD_S: u64 = 60;
pub const FRESH_S: u64 = 30;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "advice", rename_all = "snake_case")]
pub enum Advice {
    /// Nothing to propose (expected outage, not down long enough, another card).
    None { why: &'static str },
    /// Can't tell; says why.
    Unknown {
        why: &'static str,
        missing: Vec<Missing>,
    },
    /// A read-only suggestion for the person; never executed by itself.
    ReadOnly { layer: Layer, missing: Vec<Missing> },
}

impl Advice {
    /// Always false in this batch.
    pub fn executable(&self) -> bool {
        false
    }
}

pub fn replay(e: &Evidence) -> Advice {
    // no producer of an authoritative connection generation yet (P1-01): O5 always missing
    let missing = || ALL_MISSING.to_vec();
    let unknown = |why| Advice::Unknown {
        why,
        missing: missing(),
    };
    match e.expected_outage {
        None => return unknown("premises_unreadable"),
        Some(true) => {
            return Advice::None {
                why: "expected_outage",
            };
        }
        _ => {}
    }
    match e.same_sim {
        None => return unknown("sim_unreadable"),
        Some(false) => return Advice::None { why: "sim_changed" },
        _ => {}
    }
    match e.age_s {
        Some(a) if a <= FRESH_S => {}
        _ => return unknown("stale_or_untimed"),
    }
    match e.passive_down_s {
        None => return unknown("no_passive_samples"),
        Some(s) if s < HOLD_S => {
            return Advice::None {
                why: "not_down_long_enough",
            };
        }
        _ => {}
    }
    let read_only = |layer| Advice::ReadOnly {
        layer,
        missing: missing(),
    };
    for (v, layer, why) in [
        (e.registered, Layer::Registration, "registration_unknown"),
        (e.data_session, Layer::DataSession, "data_session_unknown"),
        (e.has_ip, Layer::Ip, "ip_unknown"),
    ] {
        match v {
            None => return unknown(why),
            Some(false) => return read_only(layer),
            Some(true) => {}
        }
    }
    if e.path_bound != Some(true) {
        return unknown("path_unbound");
    }
    match e.dns {
        None => return unknown("dns_unknown"),
        // a name that does not exist says nothing about the link
        Some(Dns::NxDomain) => return unknown("nxdomain"),
        Some(Dns::Fail) => return read_only(Layer::Dns),
        Some(Dns::Ok) => {}
    }
    if e.clock_trusted != Some(true) {
        return unknown("clock_untrusted");
    }
    match e.https {
        Some(Check::Fail) => read_only(Layer::Exit),
        Some(Check::Pass) => Advice::None {
            why: "app_reachable",
        },
        _ => unknown("https_unknown"),
    }
}

#[cfg(test)]
mod tests;
