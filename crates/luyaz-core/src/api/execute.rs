//! The decision loop's execution entry (decision-execution-loop.md §1.1, §1.4, §4.8).
//!
//! An approved proposal goes through here and nowhere else:
//! 1. any entry condition still missing (§3) → read-only, nothing reserved;
//! 2. one proposal binds at most one transaction: asked again, the same op comes back;
//! 3. one attempt per kind + card + action + connection at a time; 30 min after any end,
//!    two failures in 24 h stop it (§1.4);
//! 4. the binding and the attempt are written to disk **before** the engine is called, and a
//!    failed write refuses (a core restart can never run the proposal twice);
//! 5. `check_execution` runs inside the engine's second locked section ([`Precheck`]);
//! 6. after the transaction ends, one bounded recheck turns it into an [`Outcome`].
//!
//! Since 10-20 nothing is missing (step 1 passes). A non-human approval still runs only an
//! action on [`ALLOW`], which is empty until the owner adds one (§1.1).

// No production caller: nothing produces a proposal while every action for a found fault is
// unverified (B31) or needs a person's own confirm, and `missing()` is non-empty.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Mutex;

use luyaz_api::execution::no_internet::Missing;
use luyaz_api::execution::{
    Approval, ApprovedBy, FindingSnapshot, Now, Outcome, Proposal, Recheck, Record, TxnEnd, check_execution,
    outcome,
};
use serde::{Deserialize, Serialize};

use crate::ops::engine::{Device, Engine, Precheck, Request, Submit};

pub const FILE: &str = "/data/luyaz/core/loop.json";
/// §1.4: no new attempt of the same key this soon after one ended.
pub const COOLDOWN_S: u64 = 30 * 60;
/// §1.4: this many failed/unknown ends in a day stop the key until a person clears it.
pub const MAX_FAILS_PER_DAY: usize = 2;
const DAY_S: u64 = 24 * 3600;
/// Kept records (the newest).
const KEEP: usize = 50;

/// §1.1 auto-execution allow-list: the actions an approval that isn't a person's may run.
/// Empty: the owner adds each one (the first candidate is `data.reconnect`).
pub const ALLOW: &[&str] = &[];

/// The entry conditions still missing. Every one keeps the loop read-only.
pub fn missing() -> Vec<Missing> {
    luyaz_api::execution::no_internet::ALL_MISSING.to_vec()
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Tried {
    key: String,
    proposal_id: String,
    op_id: String,
    started: u64,
    #[serde(default)]
    ended: Option<u64>,
    #[serde(default)]
    outcome: Option<Outcome>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct LoopFile {
    #[serde(default)]
    tried: Vec<Tried>,
    #[serde(default)]
    records: Vec<serde_json::Value>,
    /// Every proposal id that reached the engine, kept after its attempt is pruned: a
    /// proposal never runs twice, whatever its expiry says.
    #[serde(default)]
    spent: Vec<String>,
}

/// Spent ids kept (the newest).
const SPENT_KEEP: usize = 500;

pub struct Loop {
    path: PathBuf,
    file: Mutex<LoopFile>,
}

#[derive(Debug, PartialEq)]
pub enum Exec {
    /// §3: still read-only.
    ReadOnly(Vec<Missing>),
    /// §1.1: not approved by a person and the action is not on [`ALLOW`].
    NotAllowed,
    /// This proposal already has its transaction.
    Existing(String),
    /// §1.4 cooldown or daily stop for this key.
    Cooldown(&'static str),
    /// Nothing written: the binding could not be saved, or the engine refused/was busy.
    Refused(String),
    /// Handed to the engine.
    Started(String),
}

/// kind + full card + action: the daily stop. Without the connection, so an action that
/// redials (a new connection epoch every time) still runs out of tries.
pub fn day_key(p: &Proposal) -> String {
    format!(
        "{}|{}:{}|{}",
        p.finding.kind, p.sim.slot, p.sim.iccid, p.action_id
    )
}

/// [`day_key`] + connection: one attempt at a time and the cooldown (§1.4).
pub fn key(p: &Proposal) -> String {
    format!(
        "{}|{}:{}|{}|{}",
        p.finding.kind,
        p.sim.slot,
        p.sim.iccid,
        p.action_id,
        p.epochs.conn.as_deref().unwrap_or("")
    )
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// §1.1: a person's approval, or an action on [`ALLOW`].
pub fn allowed(a: &Approval, p: &Proposal) -> bool {
    a.by == ApprovedBy::Human || ALLOW.contains(&p.action_id.as_str())
}

/// A failed or unknown end counts toward the daily stop; recovered / kept / cancelled don't.
fn failed(o: Option<Outcome>) -> bool {
    !matches!(
        o,
        Some(Outcome::Recovered | Outcome::HumanKept | Outcome::Cancelled)
    )
}

impl Loop {
    pub fn load(path: PathBuf) -> Loop {
        let file = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Loop {
            path,
            file: Mutex::new(file),
        }
    }

    fn save(&self, f: &LoopFile) -> bool {
        let Ok(text) = serde_json::to_vec(f) else {
            return false;
        };
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &self.path).is_ok()
    }

    /// Steps 2–4 under one lock: the existing op, the cooldown, or a saved reservation.
    fn reserve(&self, p: &Proposal, now: u64) -> Result<String, Exec> {
        let mut f = lock(&self.file);
        let op_id = format!("loop-{}", p.proposal_id);
        if f.spent.contains(&p.proposal_id) {
            return Err(Exec::Existing(op_id));
        }
        let k = key(p);
        let dk = day_key(p);
        let same: Vec<&Tried> = f.tried.iter().filter(|t| t.key == k).collect();
        if same.iter().any(|t| t.ended.is_none()) {
            return Err(Exec::Cooldown("attempt_running"));
        }
        if f.tried
            .iter()
            .filter(|t| t.key.starts_with(&format!("{dk}|")))
            .filter(|t| t.ended.is_some_and(|e| now.saturating_sub(e) < DAY_S))
            .filter(|t| failed(t.outcome))
            .count()
            >= MAX_FAILS_PER_DAY
        {
            return Err(Exec::Cooldown("stopped_after_two_failures"));
        }
        if same
            .iter()
            .any(|t| t.ended.is_some_and(|e| now.saturating_sub(e) < COOLDOWN_S))
        {
            return Err(Exec::Cooldown("cooldown"));
        }
        if !crate::ops::engine::valid_op_id(&op_id) {
            return Err(Exec::Refused("bad proposal id".into()));
        }
        f.tried.push(Tried {
            key: k,
            proposal_id: p.proposal_id.clone(),
            op_id: op_id.clone(),
            started: now,
            ended: None,
            outcome: None,
        });
        f.spent.push(p.proposal_id.clone());
        let n = f.spent.len();
        if n > SPENT_KEEP {
            f.spent.drain(..n - SPENT_KEEP);
        }
        if !self.save(&f) {
            f.tried.pop();
            f.spent.pop();
            return Err(Exec::Refused(
                "cannot save the attempt on the device; not changing anything".into(),
            ));
        }
        Ok(op_id)
    }

    /// Run one approved proposal. `current` re-reads the Finding and the device under the
    /// engine lock (argument: the admission gate is held) for `check_execution`. It runs with
    /// the engine's lock held: it must only copy in-memory snapshots (the Finding store, one
    /// collector `Scope`, the boot clock), never call ubus or start a process.
    pub async fn execute<D: Device, F>(
        &self,
        engine: &Engine<D>,
        p: &Proposal,
        a: &Approval,
        req: Request,
        now_unix: u64,
        current: F,
    ) -> Exec
    where
        F: FnOnce(bool) -> (FindingSnapshot, Now) + Send + 'static,
    {
        let m = missing();
        if !m.is_empty() {
            return Exec::ReadOnly(m);
        }
        if !allowed(a, p) {
            return Exec::NotAllowed;
        }
        self.execute_unchecked(engine, p, a, req, now_unix, current)
            .await
    }

    /// [`Loop::execute`] past the entry conditions. Only tests call it directly.
    pub(crate) async fn execute_unchecked<D: Device, F>(
        &self,
        engine: &Engine<D>,
        p: &Proposal,
        a: &Approval,
        mut req: Request,
        now_unix: u64,
        current: F,
    ) -> Exec
    where
        F: FnOnce(bool) -> (FindingSnapshot, Now) + Send + 'static,
    {
        let op_id = match self.reserve(p, now_unix) {
            Ok(op) => op,
            Err(e) => return e,
        };
        req.op_id = Some(op_id.clone());
        let (p2, a2) = (p.clone(), a.clone());
        let check: Precheck = Box::new(move |held| {
            let (f, mut n) = current(held);
            n.gate_granted = n.gate_granted && held;
            check_execution(&p2, &a2, &f, &n).map_err(|r| {
                serde_json::to_value(r)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "refused".into())
            })
        });
        match engine.submit_from_loop(req, check).await {
            Submit::Applied { .. } => Exec::Started(op_id),
            Submit::Existing(_) => Exec::Existing(op_id),
            other => {
                let why = match other {
                    Submit::Refused(w) | Submit::NoCapture(w) => w,
                    Submit::Busy(_) => "busy".into(),
                    _ => "not a transaction".into(),
                };
                self.refused(&op_id, now_unix, &why);
                Exec::Refused(why)
            }
        }
    }

    /// The transaction `op_id` ended (`phase` as the engine reports it); with the recheck when
    /// it was confirmed. Returns the record, or None for an op this loop never started.
    pub fn end(
        &self,
        op_id: &str,
        now_unix: u64,
        txn: TxnEnd,
        recheck: Option<&Recheck>,
    ) -> Option<Record> {
        let r = outcome(txn, recheck);
        let mut f = lock(&self.file);
        let t = f.tried.iter_mut().find(|t| t.op_id == op_id)?;
        if t.ended.is_some() {
            return None;
        }
        t.ended = Some(now_unix);
        t.outcome = Some(r.outcome);
        let line = serde_json::json!({"op_id": op_id, "at": now_unix, "record": r});
        f.records.push(line);
        let n = f.records.len();
        if n > KEEP {
            f.records.drain(..n - KEEP);
        }
        // ended attempts older than a day no longer matter to §1.4
        f.tried
            .retain(|t| t.ended.is_none_or(|e| now_unix.saturating_sub(e) < DAY_S));
        self.save(&f);
        Some(r)
    }

    /// Nothing was written: the attempt doesn't count toward §1.4, but the proposal stays spent
    /// (§1.1: a voided proposal is never replayed; a new observation makes a new one).
    fn refused(&self, op_id: &str, now_unix: u64, why: &str) {
        let mut f = lock(&self.file);
        f.tried.retain(|t| t.op_id != op_id);
        f.records
            .push(serde_json::json!({"op_id": op_id, "at": now_unix, "refused": why}));
        let n = f.records.len();
        if n > KEEP {
            f.records.drain(..n - KEEP);
        }
        self.save(&f);
    }

    /// This proposal already reached the engine (it never runs again).
    pub fn is_spent(&self, proposal_id: &str) -> bool {
        lock(&self.file).spent.iter().any(|s| s == proposal_id)
    }

    /// Attempts with no end yet (op id, start): after a core restart nobody follows them.
    pub fn open(&self) -> Vec<(String, u64)> {
        lock(&self.file)
            .tried
            .iter()
            .filter(|t| t.ended.is_none())
            .map(|t| (t.op_id.clone(), t.started))
            .collect()
    }

    pub fn records(&self) -> Vec<serde_json::Value> {
        lock(&self.file).records.clone()
    }
}

/// The engine's final phase as the loop's [`TxnEnd`]; None while it runs.
pub fn txn_end(status: &serde_json::Value) -> Option<TxnEnd> {
    Some(
        match (status["phase"].as_str()?, status["reason"].as_str()) {
            ("confirmed", Some("user_keep")) => TxnEnd::UserKeep,
            ("confirmed", _) => TxnEnd::Confirmed,
            ("rolled_back", Some("user_revert")) => TxnEnd::UserRevert,
            ("rolled_back", _) => TxnEnd::RolledBack,
            ("rollback_failed", _) => TxnEnd::RollbackFailed,
            ("unverified", _) => TxnEnd::Unverified,
            ("not_applied", _) => TxnEnd::NotApplied,
            ("cancelled", _) => TxnEnd::Cancelled,
            _ => return None,
        },
    )
}

#[cfg(test)]
#[path = "tests_execute.rs"]
mod tests;
