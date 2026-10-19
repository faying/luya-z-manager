//! Diagnosis → execution loop contracts (decision-execution-loop.md §1.1/§1.2, §4 B1, §4.1 D3).
//! Types and pure rules only: nothing here writes, persists or grants a permission. The engine
//! wiring (second batch) must call [`check_execution`] inside its **second** locked section —
//! after reading the current values, with a Finding snapshot re-read in that same section — and
//! only then hand the write to the existing transaction engine.
//!
//! Invariants the tests hold:
//! - a transaction's `Confirmed` / `UserKeep` never alone makes `recovered`: that needs a
//!   `fault_recheck` pass on the original fault ([`outcome`]);
//! - unknown never becomes success or an action;
//! - `human_kept` is kept apart from the fault's own state;
//! - one proposal binds at most one transaction; asking again returns the same op ([`Ledger`]);
//! - another card, another connection, another Finding epoch/revision, a human write or expiry
//!   voids the proposal;
//! - an approval only carries the permission decision made elsewhere (`ops/agent.rs` for the
//!   MCP's restricted APN/CHILL self-approval, Web/Screen for a human); it adds none.

use serde::{Deserialize, Serialize};

pub mod no_internet;

/// The card: full ICCID + slot, kept on the device only (screens show the tail).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimId {
    pub iccid: String,
    pub slot: u8,
}

/// Which Finding, as the store had it (epoch random per load, revision per change).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingRef {
    pub finding_id: String,
    pub kind: String,
    pub epoch: String,
    pub revision: u64,
}

/// The Finding as re-read under the execution lock. None fields = unknown.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FindingSnapshot {
    pub open: bool,
    pub epoch: Option<String>,
    pub revision: Option<u64>,
}

/// An authoritative connection generation (P1-01 lifecycle). IP + estimated up-since
/// (`txn::Conn`) is not one; until a producer exists this is always None = unknown.
pub type ConnGen = Option<String>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RollbackPolicy {
    /// Only for `captured_and_proven` actions (`ops/spec.rs`).
    Auto,
    /// Stop and fix by hand.
    None,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub proposal_id: String,
    pub finding: FindingRef,
    pub boot_id: String,
    pub sim: SimId,
    /// None = unknown: never executable.
    pub conn: ConnGen,
    pub action_id: String,
    /// Digest of the normalised parameters.
    pub params_digest: String,
    pub reason: String,
    pub excluded: Vec<String>,
    pub rollback_policy: RollbackPolicy,
    /// BOOTTIME ms on `boot_id`.
    pub created_at_boot_ms: u64,
    pub expires_at_boot_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovedBy {
    /// A real confirm on the admin web or the touch screen.
    Human,
    /// The MCP agent's restricted self-approval, as `ops/agent.rs` decided it.
    AgentRestricted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    pub proposal_id: String,
    pub params_digest: String,
    pub device_id: String,
    pub by: ApprovedBy,
    /// Where it came from (web / screen / mcp), for the record.
    pub via: String,
    /// The permission check's own verdict elsewhere; this module never decides it.
    pub permitted: bool,
    pub expires_at_boot_ms: u64,
}

/// What the execution lock sees at the moment of the check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Now {
    pub boot_id: String,
    /// None = boot clock unreadable.
    pub boot_ms: Option<u64>,
    pub device_id: String,
    /// None = the card can't be read.
    pub sim: Option<SimId>,
    pub conn: ConnGen,
    /// The admission gate is held, granted, by this proposal.
    pub gate_granted: bool,
    /// A human wrote the same item since the proposal.
    pub human_wrote: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Refuse {
    NotPermitted,
    ApprovalMismatch,
    Expired,
    OtherBoot,
    NoClock,
    SimUnknown,
    SimChanged,
    ConnUnknown,
    ConnChanged,
    FindingClosed,
    FindingUnknown,
    FindingChanged,
    HumanWrote,
    GateNotHeld,
}

/// May this proposal run now. The order puts what a person can fix last; every unknown refuses.
pub fn check_execution(
    p: &Proposal,
    a: &Approval,
    f: &FindingSnapshot,
    now: &Now,
) -> Result<(), Refuse> {
    if !a.permitted {
        return Err(Refuse::NotPermitted);
    }
    if a.proposal_id != p.proposal_id
        || a.params_digest != p.params_digest
        || a.device_id != now.device_id
    {
        return Err(Refuse::ApprovalMismatch);
    }
    if now.boot_id.is_empty() || now.boot_id != p.boot_id {
        return Err(Refuse::OtherBoot);
    }
    let t = now.boot_ms.ok_or(Refuse::NoClock)?;
    if t >= p.expires_at_boot_ms || t >= a.expires_at_boot_ms || t < p.created_at_boot_ms {
        return Err(Refuse::Expired);
    }
    match &now.sim {
        None => return Err(Refuse::SimUnknown),
        Some(s) if *s != p.sim => return Err(Refuse::SimChanged),
        _ => {}
    }
    match (&p.conn, &now.conn) {
        (None, _) | (_, None) => return Err(Refuse::ConnUnknown),
        (Some(a), Some(b)) if a != b => return Err(Refuse::ConnChanged),
        _ => {}
    }
    if !f.open {
        return Err(Refuse::FindingClosed);
    }
    match (&f.epoch, f.revision) {
        (Some(e), _) if e.is_empty() || p.finding.epoch.is_empty() => {
            return Err(Refuse::FindingUnknown);
        }
        (Some(e), Some(r)) if *e == p.finding.epoch && r == p.finding.revision => {}
        (Some(_), Some(_)) => return Err(Refuse::FindingChanged),
        _ => return Err(Refuse::FindingUnknown),
    }
    if now.human_wrote {
        return Err(Refuse::HumanWrote);
    }
    if !now.gate_granted {
        return Err(Refuse::GateNotHeld);
    }
    Ok(())
}

// ---- one proposal, at most one transaction ----------------------------------------------

/// proposal id → op id. Persisting it (refuse the write when that fails) is the second batch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ledger {
    bound: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Bind {
    New,
    /// Asked again: the op it already has.
    Existing(String),
}

impl Ledger {
    pub fn bind(&mut self, proposal_id: &str, op_id: &str) -> Bind {
        if let Some((_, op)) = self.bound.iter().find(|(p, _)| p == proposal_id) {
            return Bind::Existing(op.clone());
        }
        self.bound.push((proposal_id.to_owned(), op_id.to_owned()));
        Bind::New
    }
    pub fn op_of(&self, proposal_id: &str) -> Option<&str> {
        self.bound
            .iter()
            .find(|(p, _)| p == proposal_id)
            .map(|(_, o)| o.as_str())
    }
}

// ---- states and the outcome ---------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Open,
    Proposed,
    Approved,
    Applying,
    TxnVerifying,
    FaultRechecking,
    Recovered,
    StillFaulty,
    Unknown,
    Cancelled,
}

impl State {
    pub fn is_final(self) -> bool {
        matches!(
            self,
            State::Recovered | State::StillFaulty | State::Unknown | State::Cancelled
        )
    }
}

/// The only forward steps (§1.2); any non-final state may also go to `Cancelled`.
pub fn advance(from: State, to: State) -> Option<State> {
    use State::*;
    let ok = match (from, to) {
        (s, Cancelled) => !s.is_final(),
        (Open, Proposed)
        | (Proposed, Approved)
        | (Approved, Applying)
        | (Applying, TxnVerifying)
        | (TxnVerifying, FaultRechecking)
        // the transaction ended without a confirmed write: nothing to recheck
        | (TxnVerifying, Unknown)
        | (FaultRechecking, Recovered | StillFaulty | Unknown) => true,
        _ => false,
    };
    ok.then_some(to)
}

/// The transaction's own final word (from `ops/txn.rs`), unchanged by the loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TxnEnd {
    Confirmed,
    UserKeep,
    UserRevert,
    RolledBack,
    RollbackFailed,
    Unverified,
    NotApplied,
    Cancelled,
}

/// One layer of the original fault, measured again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Check {
    Pass,
    Fail,
    Unknown,
}

/// `fault_recheck`: per layer of the fault as defined at proposal time; valid only when the
/// evidence is from the same card, connection, path and a trusted clock.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recheck {
    pub layers: Vec<(String, Check)>,
    /// Same SIM / connection / source / exit as the proposal, clock trusted, within budget.
    pub valid: bool,
}

impl Recheck {
    fn verdict(&self) -> Check {
        if !self.valid || self.layers.is_empty() {
            return Check::Unknown;
        }
        if self.layers.iter().any(|(_, c)| *c == Check::Fail) {
            return Check::Fail;
        }
        if self.layers.iter().all(|(_, c)| *c == Check::Pass) {
            return Check::Pass;
        }
        Check::Unknown
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Recovered,
    StillFaulty,
    Unknown,
    RolledBack,
    RollbackFailed,
    NotApplied,
    Cancelled,
    HumanKept,
}

/// The record: the loop's outcome, the fault's own state apart from it, and the transaction's
/// end kept as it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub outcome: Outcome,
    pub fault: Check,
    pub txn: TxnEnd,
}

pub fn outcome(txn: TxnEnd, recheck: Option<&Recheck>) -> Record {
    let fault = recheck.map_or(Check::Unknown, Recheck::verdict);
    let outcome = match txn {
        TxnEnd::Confirmed => match fault {
            Check::Pass => Outcome::Recovered,
            Check::Fail => Outcome::StillFaulty,
            Check::Unknown => Outcome::Unknown,
        },
        // the person chose; the fault is reported on its own
        TxnEnd::UserKeep => Outcome::HumanKept,
        TxnEnd::UserRevert | TxnEnd::Cancelled => Outcome::Cancelled,
        TxnEnd::RolledBack => Outcome::RolledBack,
        TxnEnd::RollbackFailed => Outcome::RollbackFailed,
        TxnEnd::NotApplied => Outcome::NotApplied,
        TxnEnd::Unverified => Outcome::Unknown,
    };
    Record {
        outcome,
        fault,
        txn,
    }
}

/// Whether the Finding may close as resolved from this record: only a valid passing recheck
/// after the change stayed (confirmed, or kept by the person). A rollback that "worked" is never
/// written up as the fault fixed.
pub fn resolves_finding(r: &Record) -> bool {
    r.fault == Check::Pass && matches!(r.outcome, Outcome::Recovered | Outcome::HumanKept)
}

#[cfg(test)]
mod tests;
