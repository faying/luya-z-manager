//! `remote.wifi_change` replay (decision-execution-loop.md §2): before the person changes an AP,
//! say who may be cut off and whether a remote trial is allowed. It never starts a change; the
//! answer goes on the confirm page. A remote trial is allowed only for a change whose receipt +
//! revert path has been measured (O3, §4.5); otherwise the person must be on site.

use serde::Serialize;

/// What the change touches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    /// Turning an AP on or off.
    OnOff,
    /// Password, SSID, band, channel, hiding: clients have to reconnect or re-enter.
    Settings,
}

/// Where the request comes from, as the web sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// A LAN client not on the AP being changed (wired or the other AP).
    LanOther,
    /// A LAN client on the AP being changed.
    LanSameAp,
    /// Tailnet or a subnet route: the page comes back only if the uplink and route survive.
    Remote,
}

/// None = not read / can't tell.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Evidence {
    pub change: Option<Change>,
    /// None when the source can't be told apart (e.g. a subnet route looking local).
    pub origin: Option<Origin>,
    /// Clients on the AP being changed, the requester included.
    pub clients_on_ap: Option<u32>,
    /// Another way in that the change doesn't touch (wired, the other AP, Tailscale on the
    /// cellular side).
    pub other_entry: Option<bool>,
    /// The old values were read, so a revert has something to go back to.
    pub old_values: Option<bool>,
    /// This kind of change has a measured receipt + revert path (O3 verified on the device).
    pub revert_measured: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "risk", rename_all = "snake_case")]
pub enum Advice {
    /// Can't tell who gets cut off; the confirm page says so and treats it as remote.
    Unknown { why: &'static str },
    /// Nobody on the AP and the requester is elsewhere: a plain confirm.
    Low,
    /// Clients will drop; the requester is not one of them, or is on site.
    Clients { clients: u32, requester_drops: bool },
    /// Remote: allowed only with the receipt + timed revert. None = client count not read.
    RemoteTrial { clients: Option<u32> },
    /// Remote without a measured revert, or nothing to revert to: be on site.
    OnSiteOnly { why: &'static str },
}

impl Advice {
    /// Never an action: the person starts the change themselves.
    pub fn action(&self) -> Option<&'static str> {
        None
    }

    /// The write must carry a receipt (O3) and revert when none arrives.
    pub fn needs_receipt(&self) -> bool {
        matches!(self, Advice::RemoteTrial { .. } | Advice::Unknown { .. })
    }
}

pub fn replay(e: &Evidence) -> Advice {
    let Some(change) = e.change else {
        return Advice::Unknown {
            why: "change_unknown",
        };
    };
    // Off the LAN (or can't tell): whether the page comes back decides first; the client count
    // only colours the answer.
    let remote = match e.origin {
        None | Some(Origin::Remote) => true,
        Some(_) => false,
    };
    if remote {
        if e.old_values != Some(true) {
            return Advice::OnSiteOnly {
                why: "no_old_values",
            };
        }
        if e.revert_measured != Some(true) {
            return Advice::OnSiteOnly {
                why: if e.origin.is_none() {
                    "origin_unknown_revert_unmeasured"
                } else {
                    "revert_unmeasured"
                },
            };
        }
        if e.origin.is_none() {
            return Advice::Unknown {
                why: "origin_unknown",
            };
        }
        // a settings change can strand the clients even when the revert lands
        if change == Change::Settings && e.other_entry != Some(true) {
            return Advice::OnSiteOnly {
                why: "no_other_entry",
            };
        }
        return Advice::RemoteTrial {
            clients: e.clients_on_ap,
        };
    }
    let Some(clients) = e.clients_on_ap else {
        return Advice::Unknown {
            why: "clients_unknown",
        };
    };
    match e.origin {
        Some(Origin::LanSameAp) => Advice::Clients {
            clients: clients.max(1),
            requester_drops: true,
        },
        _ if clients == 0 => Advice::Low,
        _ => Advice::Clients {
            clients,
            requester_drops: false,
        },
    }
}

#[cfg(test)]
mod tests;
