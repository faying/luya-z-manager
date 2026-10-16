//! The APN candidate pick's card-change decision, moved from zte-agent `src/apn_pick.rs`
//! (Phase 4 step 5a; D39: the one automatic APN write the owner authorised).
//!
//! When the card changes while our copy of a candidate is the active manual APN, go back to auto
//! (or straight to the new card's own pick); when the card that made a pick comes back while the
//! device is on auto or another card's copy, use its pick again. A manual APN the owner made is
//! never touched. [`decide`] is the agent's, unchanged; the watcher thread, the ubus reads, the
//! writes, the notice and the profile matching (`find_same`, on vendor JSON) stay in the host.
//!
//! Deliberate differences from the agent:
//! - [`Store`] has no `notice` (the decision never reads it; serde skips it when reading the
//!   agent's file).
//! - [`Watch`]'s fields are public, so a shadow record can carry it before and after a look.

use std::collections::BTreeMap;

/// Failed switches for one card before it gives up (and leaves a sticky notice).
pub const MAX_FAILS: u8 = 3;

#[derive(Clone, PartialEq, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Pick {
    /// The manual profile we dial (our copy, or an identical one that existed).
    pub manual_id: String,
    pub apn: String,
}

/// `/data/apn-pick.json` without the notice.
#[derive(Clone, PartialEq, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Store {
    /// Full ICCID (pad nibble trimmed) → its pick.
    #[cfg_attr(feature = "serde", serde(default))]
    pub picks: BTreeMap<String, Pick>,
    /// The card seen last; a change is what the watcher acts on.
    #[cfg_attr(feature = "serde", serde(default))]
    pub last_iccid: String,
}

/// ICCID as the card has it: the ZTE stack appends a BCD pad nibble ("…F").
pub fn norm_iccid(s: &str) -> String {
    s.trim().trim_end_matches(['F', 'f']).to_string()
}

/// Last four digits, for logs and the UIs (full ICCIDs stay in the store).
pub fn tail4(iccid: &str) -> &str {
    &iccid[iccid.len().saturating_sub(4)..]
}

/// IoT / M2M APNs: they bring up a bearer but not the normal internet.
pub fn is_iot(apn: &str) -> bool {
    let a = apn.to_ascii_lowercase();
    a.contains("iot") || a.starts_with("m2m")
}

#[derive(Debug, PartialEq, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum Action {
    None,
    /// Go to automatic APN.
    Auto,
    /// Dial this manual profile.
    Use(String),
}

/// Two reads in a row of the same new ICCID before acting: the stack reads
/// empty, then the old card, for a moment around an eSIM switch.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Watch {
    pub pending: String,
    pub seen: u8,
    /// Failed switches for `pending`.
    pub fails: u8,
}

impl Watch {
    /// Looking at a new card, or retrying: look again soon.
    pub fn eager(&self) -> bool {
        self.seen > 0
    }
}

/// What to do now. `card` is set once a new card is confirmed: commit it to
/// `last_iccid` when `action` is None or after the action worked.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Decision {
    pub action: Action,
    pub card: Option<String>,
}

pub const NOTHING: Decision = Decision {
    action: Action::None,
    card: None,
};

/// One look. `iccid` normalised ("" = unreadable); `busy` = an APN switch, a
/// network search/register or an eSIM job running. `apn` reads (manual mode,
/// selected manual id) — called only for a confirmed change, `None` = could
/// not read (try again next time).
pub fn decide(
    store: &Store,
    w: &mut Watch,
    iccid: &str,
    busy: bool,
    apn: impl FnOnce() -> Option<(bool, Option<String>)>,
) -> Decision {
    if busy || iccid.is_empty() || iccid == store.last_iccid {
        *w = Watch::default();
        return NOTHING;
    }
    if w.pending == iccid {
        w.seen = w.seen.saturating_add(1);
    } else {
        *w = Watch {
            pending: iccid.to_string(),
            seen: 1,
            fails: 0,
        };
    }
    if w.seen < 2 {
        return NOTHING;
    }
    let card = Some(iccid.to_string());
    if store.last_iccid.is_empty() {
        // Nothing to compare with (first run): just learn the card.
        return Decision {
            action: Action::None,
            card,
        };
    }
    let Some((manual_mode, selected)) = apn() else {
        return NOTHING;
    };
    let ours = |id: &str| store.picks.values().any(|p| p.manual_id == id);
    let on_our_copy = manual_mode && selected.as_deref().is_some_and(ours);
    let action = match store.picks.get(iccid) {
        Some(p) if manual_mode && selected.as_deref() == Some(p.manual_id.as_str()) => Action::None,
        Some(p) if !manual_mode || on_our_copy => Action::Use(p.manual_id.clone()),
        Some(_) => Action::None, // on the owner's own manual APN: leave it
        None if on_our_copy => Action::Auto,
        None => Action::None,
    };
    Decision { action, card }
}

/// What the agent's `apn()` closure gave on a look: not asked, could not read, or the state.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum ApnRead {
    NotAsked,
    Unread,
    Read {
        manual: bool,
        selected: Option<String>,
    },
}

/// Replay one recorded look: the same `decide` with the recorded inputs. Returns the decision,
/// the watch after, and whether this copy asked for the APN state (the record says whether the
/// agent did; asking when the agent did not, or the reverse, is a difference on its own).
pub fn replay(
    store: &Store,
    before: &Watch,
    iccid: &str,
    busy: bool,
    apn: &ApnRead,
) -> (Decision, Watch, bool) {
    let mut w = before.clone();
    let mut asked = false;
    let d = decide(store, &mut w, iccid, busy, || {
        asked = true;
        match apn {
            ApnRead::Read { manual, selected } => Some((*manual, selected.clone())),
            _ => None,
        }
    });
    (d, w, asked)
}

#[cfg(test)]
mod tests;
