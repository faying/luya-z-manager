//! `data.no_internet` replay (decision-execution-loop.md §2, §4 B5): from one evidence set to a
//! read-only suggestion or unknown, plus which entry conditions still keep it read-only. It never
//! produces an executable proposal in this batch: the B31 data-session reconnect is unverified,
//! the SIM/connection/radio epochs exist but are not authoritative, there is no O2 recheck and
//! no wired gate.

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
    /// Multi-target HTTPS + trusted clock + verified policy routing recheck. Rules in `https`;
    /// not yet run on the device.
    O2,
    /// Per-action rollback capability and completion evidence.
    O3,
    /// Atomic Finding revision + SIM + connection generation. The binding is in
    /// `check_execution`; the epoch producers are still non-authoritative.
    O5,
    /// The admission gate wired into the engine's write path.
    T4,
    /// B31's safe data-session reconnect interface, unverified.
    B31Reconnect,
}

/// Still missing (10-20). O3 closed 10-19 (receipt verified both ways on the device); O2
/// closed 10-20 (`https` rules + bound o2-check on the device, both families, clock trusted);
/// T4 closed 10-20 (a real band transaction waited for a running diagnosis, and a diagnosis
/// asked during the transaction waited for its end, on the device); B31 closed 10-20 (IPv4,
/// IPv6 and dual-stack redials all came back within 15 s on the device); O5 closed 10-20 by the
/// owner: SIM and connection epochs as observed (redials, IPv6 loss, steady while idle, core
/// 1.6.9), the radio epoch stood in for by a fresh ONLINE mode read before the engine.
pub const ALL_MISSING: [Missing; 0] = [];

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
    /// Never by itself: a proposal runs only with an approval (a person, or the allow-list).
    pub fn executable(&self) -> bool {
        false
    }

    /// The one action a proposal may carry: a lost data session or address → redial (B31,
    /// `ops/reconnect.rs`). DNS and exit faults, registration: no action, read-only advice.
    pub fn action(&self) -> Option<&'static str> {
        match self {
            Advice::ReadOnly {
                layer: Layer::DataSession | Layer::Ip,
                missing,
            } if missing.is_empty() => Some("data.reconnect"),
            _ => None,
        }
    }
}

pub fn replay(e: &Evidence) -> Advice {
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
