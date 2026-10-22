//! `network.health` (docs/research/network-reliability-underlay-20261021.md §9): one summary of the
//! layers under the clients, the repairs the underlay gate is holding and what to do next, for the
//! web, the touch screen and MCP alike. Pure: the core gathers the inputs, [`build`] answers, the
//! words come from [`super::say::health`].

use serde::Serialize;
use serde_json::{Value, json};

/// A repair the underlay gate held the last time it asked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Paused {
    /// who asked: "home-exit", "manual-first"
    pub who: String,
    /// "no_bearer" or "lower"
    pub reason: &'static str,
    /// the lower finding's layer ("lower" only)
    pub layer: Option<String>,
    /// unix seconds, first held
    pub since: u64,
    /// held past the gate's limit and let through: the automatic repair did not help
    pub released: bool,
}

/// The open sure finding (the home screens' one).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Finding {
    pub kind: String,
    /// "signal", "data", "internet", "dns"
    pub layer: String,
    pub symptom: String,
    pub symptom_en: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Input {
    /// a default route out of the data interface (`ni::bearer_from_proc`); None = unread
    pub bearer: Option<bool>,
    pub finding: Option<Finding>,
    pub paused: Vec<Paused>,
    /// the last held repair cleared this many seconds ago
    pub recovered_secs: Option<u64>,
    /// CHILL's forwarding, from the core's background probe: Some(false) = two misses in a row
    /// through mihomo while its API answers; None = not probed lately (off, idle, no clients)
    pub proxy: Option<bool>,
}

/// How long "recovered, checking the proxy" stays up after the last hold clears.
pub const RECOVERED_SECS: u64 = 120;

/// What to do next: "wait" (it repairs itself), "check_apn", or none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    Wait,
    CheckApn,
}

impl Next {
    pub fn code(self) -> &'static str {
        match self {
            Next::Wait => "wait",
            Next::CheckApn => "check_apn",
        }
    }
}

/// "ok", "down" or "unknown", from the bearer and the finding.
pub fn state(i: &Input) -> &'static str {
    if i.bearer == Some(false) || i.finding.is_some() {
        "down"
    } else if i.bearer.is_none() {
        "unknown"
    } else {
        "ok"
    }
}

pub fn next(i: &Input) -> Option<Next> {
    if i.bearer == Some(false) {
        // data off on purpose (airplane, roaming off, no SIM, a switch, boot) holds nothing:
        // only a held repair makes it something to wait for
        return held(i).then_some(Next::Wait);
    }
    let f = i.finding.as_ref()?;
    // a repair let through past the gate's limit and the finding still open: not repairing itself
    let gave_up = i.paused.iter().any(|p| p.released);
    Some(if gave_up && matches!(f.layer.as_str(), "data" | "internet") {
        Next::CheckApn
    } else {
        Next::Wait
    })
}

/// A repair is held now (not one already let through).
pub fn held(i: &Input) -> bool {
    i.paused.iter().any(|p| !p.released)
}

/// The block. `say` is null when there is nothing worth saying (healthy: no notice).
pub fn build(i: &Input) -> Value {
    let bearer = match i.bearer {
        Some(true) => "up",
        Some(false) => "down",
        None => "unknown",
    };
    json!({
        "state": state(i),
        "layers": {
            "bearer": bearer,
            "finding": i.finding.as_ref().map(|f| f.layer.as_str()),
            "proxy": i.proxy.map(|up| if up { "up" } else { "down" }),
        },
        "finding": i.finding.as_ref().map(|f| f.kind.as_str()),
        "paused": i.paused,
        "next": next(i).map(Next::code),
        "say": super::say::health(i),
    })
}

#[cfg(test)]
mod tests;
