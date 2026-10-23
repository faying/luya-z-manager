//! Restore the last verified connection (`docs/designs/restore-verified-connection.md`): the
//! rules, no device. When a set of cellular settings counts as verified, what differs from it
//! now, and the steps back in order. The core reads and writes; this decides.
//!
//! One set per card × country (serving MCC, user decision 10-20), plus the one before it for
//! display. Recorded only after the user's deep check also ran the bound HTTPS + DNS check
//! (user decision 10-20) and it passed on one connection, one card, a trusted clock, with no
//! transaction running or just ended.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{Check, Epochs, Recheck};
use serde_json::Value;

/// The items of a set. Restore order is [`diff`]'s: unlocks, bands, mode, locks, APN.
pub const ITEMS: [&str; 6] = [
    "cell.lte",
    "cell.nr",
    "band.lte",
    "band.nr",
    "network.mode",
    "apn",
];

/// The write that sets each item back (its `Spec::params` turns the value into parameters).
pub fn action_of(item: &str) -> Option<&'static str> {
    Some(match item {
        "cell.lte" => "cell.lock_lte",
        "cell.nr" => "cell.lock_nr",
        "band.lte" => "band.set_lte",
        "band.nr" => "band.set_nr",
        "network.mode" => "network.set_mode",
        "apn" => "apn.use",
        _ => return None,
    })
}

/// A transaction must have been created at least this long ago: the design's 10 minutes after
/// its end, plus the longest deadline (3 min). The engine keeps no end time, so this counts from
/// creation, which is never shorter than the real wait.
pub const SETTLE_MS: u64 = 13 * 60 * 1000;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Prev {
    pub at: i64,
    pub items: BTreeMap<String, Value>,
}

/// One verified set (`/data/luyaz/core/verified/<card>-<mcc>.json`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Set {
    /// Device clock, Unix seconds.
    pub at: i64,
    pub mcc: i64,
    pub conn_epoch: String,
    pub items: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev: Option<Prev>,
}

/// The transaction state when the check ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Txn {
    None,
    Active,
    /// The newest one is final, created this many ms ago.
    Final(u64),
}

/// Everything the record decision looks at.
pub struct Proof<'a> {
    pub txn: Txn,
    pub recheck: &'a Recheck,
    /// Before and after the check (the check itself demands they're equal and complete).
    pub then: &'a Epochs,
    pub now: &'a Epochs,
    /// The card before and after reading the settings.
    pub iccid_then: Option<&'a str>,
    pub iccid_now: Option<&'a str>,
    pub mcc: Option<i64>,
}

/// None = record; otherwise why not.
pub fn refuse_record(p: &Proof) -> Option<&'static str> {
    match p.txn {
        Txn::Active => return Some("txn_active"),
        Txn::Final(age) if age < SETTLE_MS => return Some("settling"),
        _ => {}
    }
    let pass = p.recheck.valid
        && !p.recheck.layers.is_empty()
        && p.recheck.layers.iter().all(|(_, c)| *c == Check::Pass);
    if !pass {
        return Some("check_not_passed");
    }
    if p.then.conn.as_deref().is_none_or(str::is_empty) || p.then != p.now {
        return Some("connection_changed");
    }
    match (p.iccid_then, p.iccid_now) {
        (Some(a), Some(b)) if !a.is_empty() && a == b => {}
        (Some(a), Some(_)) if !a.is_empty() => return Some("sim_changed"),
        _ => return Some("no_sim"),
    }
    if p.mcc.is_none_or(|m| m <= 0) {
        return Some("no_country");
    }
    None
}

/// The new set, keeping the old one's items as `prev`; None when nothing usable was read.
/// Only items with a value are kept: an unread item is left out, not recorded as empty.
pub fn next(
    old: Option<&Set>,
    at: i64,
    mcc: i64,
    conn_epoch: &str,
    items: &BTreeMap<String, Value>,
) -> Option<Set> {
    let items: BTreeMap<String, Value> = items
        .iter()
        .filter(|(k, v)| ITEMS.contains(&k.as_str()) && !v.is_null())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if items.is_empty() {
        return None;
    }
    let prev = old.and_then(|o| {
        (o.items != items).then(|| Prev {
            at: o.at,
            items: o.items.clone(),
        })
    });
    Some(Set {
        at,
        mcc,
        conn_epoch: conn_epoch.to_owned(),
        items,
        // the same settings again: keep the older prev instead of a copy of this set
        prev: prev.or_else(|| old.and_then(|o| o.prev.clone())),
    })
}

/// One step back to the set.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Step {
    pub item: &'static str,
    pub action: &'static str,
    /// What it is now (`null` = could not be read).
    pub now: Value,
    pub to: Value,
}

/// Restore order: unlocking a cell opens the network up, so it goes first; locking one only
/// works once the bands and mode allow that cell, so it goes after them; the APN last.
fn rank(s: &Step) -> u8 {
    match s.item {
        "cell.lte" | "cell.nr" if s.to.get("off") == Some(&Value::Bool(true)) => 0,
        "band.lte" | "band.nr" => 1,
        "network.mode" => 2,
        "cell.lte" | "cell.nr" => 3,
        _ => 4,
    }
}

/// The items that differ, in restore order ([`rank`], then [`ITEMS`]). An item the set doesn't
/// have is left alone. APN is compared on mode and fingerprint only: the profile number may
/// differ.
pub fn diff(set: &Set, now: &BTreeMap<String, Value>) -> Vec<Step> {
    let mut steps: Vec<Step> = ITEMS
        .iter()
        .filter_map(|&item| {
            let to = set.items.get(item)?;
            let action = action_of(item)?;
            let cur = now.get(item).cloned().unwrap_or(Value::Null);
            let same = if item == "apn" {
                apn_key(&cur) == apn_key(to)
            } else {
                &cur == to
            };
            (!same).then(|| Step {
                item,
                action,
                now: cur,
                to: to.clone(),
            })
        })
        .collect();
    steps.sort_by_key(rank);
    steps
}

fn apn_key(v: &Value) -> Option<(i64, &str)> {
    Some((v.get("mode")?.as_i64()?, v.get("fp")?.as_str()?))
}

/// The APN step's target: automatic as it is; manual = the profile on the device now whose
/// content has the recorded fingerprint (`list` = (id, fp)). None = that profile is gone: the
/// step is refused, never recreated.
pub fn apn_target(to: &Value, list: &[(String, String)]) -> Option<Value> {
    let (mode, fp) = apn_key(to)?;
    if mode == 0 {
        return Some(serde_json::json!({"mode": 0, "manual": "", "fp": ""}));
    }
    let (id, _) = list.iter().find(|(_, f)| f == fp)?;
    Some(serde_json::json!({"mode": mode, "manual": id, "fp": fp}))
}

/// Why a restore can't start, checked in this order; None = it may.
pub fn refuse_restore(
    mcc: Option<i64>,
    set: Option<&Set>,
    txn_active: bool,
    steps: &[Step],
) -> Option<&'static str> {
    if mcc.is_none_or(|m| m <= 0) {
        return Some("no_country");
    }
    if set.is_none() {
        return Some("no_record");
    }
    if txn_active {
        return Some("txn_active");
    }
    if steps.is_empty() {
        return Some("already_there");
    }
    None
}

#[cfg(test)]
mod tests;
