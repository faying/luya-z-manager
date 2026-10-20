//! A person approves the pending proposal (decision-execution-loop.md §4.10): `/control`
//! `loop.approve` from the touch screen or the admin web, never the agent.
//!
//! The approval names the proposal the person saw (`proposal_id` + `params_digest`); a different
//! pending one is refused, never approved in its place. Before the engine: one real mode read
//! (the producer's is minutes old by now, and the precheck counts the radio only within
//! [`super::propose::RADIO_EXEC_S`]). Then [`Loop::execute`] with [`super::propose::now_for`] as
//! the precheck under the engine lock. When the transaction ends: one bound HTTPS recheck over
//! the cellular interface, and [`Loop::end`] records the outcome (§1.4 counts it). Recovered
//! closes the finding the proposal was for right away (§4.10), not after the passive ups.

use std::sync::Arc;
use std::time::Duration;

use luyaz_api::clock::BootTime;
use luyaz_api::execution::{Approval, ApprovedBy, Epochs, Outcome, Recheck, Record};
use serde_json::{Value, json};

use super::execute::{Exec, Loop, txn_end};
use super::propose::{Pending, RADIO_EXEC_S, Shared, fresh_mode_read, now_for};
use crate::ops::engine::{Device, Engine, Request};
use crate::ops::txn::Who;

/// The device's loop state (`/data/luyaz/core/loop.json`).
pub fn device_loop() -> &'static Loop {
    static L: std::sync::OnceLock<Loop> = std::sync::OnceLock::new();
    L.get_or_init(|| Loop::load(super::execute::FILE.into()))
}

/// How the approval ended, for the reply.
#[derive(Debug, PartialEq)]
pub enum Reply {
    /// Not a person on the screen or the web.
    NotAPerson,
    /// Nothing pending, or another proposal than the one shown.
    Changed,
    /// The loop's answer.
    Exec(Exec),
}

impl Reply {
    pub fn http(&self) -> (u16, Value) {
        match self {
            Reply::NotAPerson => (
                400,
                json!({"code": "invalid_parameter", "message": "loop.approve needs source screen or web"}),
            ),
            Reply::Changed => (
                409,
                json!({"code": "proposal_changed", "message": "the proposal shown is no longer pending"}),
            ),
            Reply::Exec(Exec::Started(op)) => (200, json!({"op_id": op, "started": true})),
            // spent: not a success, nothing runs until a new observation makes a new proposal
            Reply::Exec(Exec::Existing(op)) => (
                409,
                json!({"code": "already_used", "message": "this proposal was used already; wait for a new one", "op_id": op}),
            ),
            Reply::Exec(Exec::NotAllowed) => (
                403,
                json!({"code": "not_allowed", "message": "not approved by a person"}),
            ),
            Reply::Exec(Exec::ReadOnly(m)) => (
                409,
                json!({"code": "read_only", "message": "the loop is still read-only", "missing": m}),
            ),
            Reply::Exec(Exec::Cooldown(why)) => (409, json!({"code": "cooldown", "message": why})),
            Reply::Exec(Exec::Refused(why)) => (409, json!({"code": "refused", "message": why})),
        }
    }
}

/// The pending proposal if it is the one the person saw.
pub fn pick(sh: &Shared, params: &Value, boot_ms: Option<u64>) -> Option<Pending> {
    let p = sh.producer.current(boot_ms)?;
    let same = params["proposal_id"].as_str() == Some(p.proposal.proposal_id.as_str())
        && params["params_digest"].as_str() == Some(p.proposal.params_digest.as_str());
    same.then_some(p)
}

pub fn approval(p: &Pending, device_id: &str, who: Who) -> Approval {
    Approval {
        proposal_id: p.proposal.proposal_id.clone(),
        params_digest: p.proposal.params_digest.clone(),
        device_id: device_id.into(),
        by: ApprovedBy::Human,
        via: who.approved_by.as_str().into(),
        permitted: true,
        expires_at_boot_ms: p.proposal.expires_at_boot_ms,
    }
}

pub fn request(p: &Pending, who: Who) -> Option<Request> {
    let spec = crate::ops::spec::find_any(&p.proposal.action_id)?;
    let params = json!({});
    Some(Request {
        spec,
        target: spec.target(&params).ok()?,
        params,
        source: who.source,
        approved_by: who.approved_by,
        op_id: None,
        undo: false,
        rollback: None,
        expect_iccid: Some(p.proposal.sim.iccid.clone()),
        receipt: false,
    })
}

/// `loop.approve {proposal_id, params_digest}`. On `Started` the follow-up (recheck + record)
/// runs on its own; `status` = the engine's `op.status` for one op.
pub async fn run<D: Device + 'static>(
    engine: &Engine<D>,
    lp: &'static Loop,
    sh: &'static Shared,
    hub: Arc<crate::block::Hub>,
    who: Option<Who>,
    params: &Value,
) -> Reply {
    let Some(who) = who.filter(|w| w.approved_by.can_confirm()) else {
        return Reply::NotAPerson;
    };
    let Some(p) = pick(sh, params, Some(crate::evlog::boottime_ms())) else {
        return Reply::Changed;
    };
    // the producer's mode read is minutes old: one real read now, off the async threads
    let h = hub.clone();
    let _ = tokio::task::spawn_blocking(move || fresh_mode_read(&h)).await;
    let (then, device_id) = hub
        .firmware()
        .loop_epochs(BootTime::read().as_ref(), RADIO_EXEC_S);
    let Some(req) = request(&p, who) else {
        return Reply::Exec(Exec::Refused("no such action".into()));
    };
    let a = approval(&p, &device_id, who);
    let (h, pend) = (hub.clone(), p.clone());
    let current = move |_held: bool| now_for(&h, &sh.findings, &pend);
    let now_unix = luyaz_netwatch::now_unix();
    let exec = lp
        .execute(engine, &p.proposal, &a, req, now_unix, current)
        .await;
    match &exec {
        Exec::Started(op) => {
            sh.producer.clear();
            let (e, op, hub) = (engine.clone(), op.clone(), hub.clone());
            let target = Target::of(&p);
            tokio::spawn(async move {
                let r = follow(&e, lp, &op, Some(then), hub, FOLLOW_MAX).await;
                if let Some(r) = r {
                    let _ = tokio::task::spawn_blocking(move || {
                        settle(sh, &target, &r, luyaz_netwatch::now_unix())
                    })
                    .await;
                }
            });
        }
        Exec::Existing(_) | Exec::Refused(_) => sh.producer.clear(),
        _ => {}
    }
    Reply::Exec(exec)
}

/// The transaction runs at most its deadline plus confirmation; past this it counts as unknown.
const FOLLOW_MAX: Duration = Duration::from_secs(10 * 60);
const FOLLOW_EVERY: Duration = Duration::from_secs(2);

/// After a restart the engine has to load its transactions first: an attempt past its time
/// still gets this long before it counts as unknown.
const RESUME_MIN: Duration = Duration::from_secs(60);

/// Startup: follow every attempt the last core left open to its end, so it can't block its
/// key for good. The epochs from before the restart are gone, so a confirmed one records
/// unknown (no recheck) instead of recovered.
pub fn resume<D: Device + 'static>(engine: Engine<D>, hub: Arc<crate::block::Hub>) {
    let lp = device_loop();
    let now = luyaz_netwatch::now_unix();
    for (op, started) in lp.open() {
        let left = Duration::from_secs((started + FOLLOW_MAX.as_secs()).saturating_sub(now));
        let (e, hub) = (engine.clone(), hub.clone());
        tokio::spawn(async move {
            follow(&e, lp, &op, None, hub, left.max(RESUME_MIN)).await;
        });
    }
}

/// The finding a proposal was for: the store load it was bound to, the card, the kind.
#[derive(Clone, Debug)]
pub struct Target {
    kind: String,
    iccid: String,
    epoch: String,
}

impl Target {
    pub fn of(p: &Pending) -> Target {
        Target {
            kind: p.proposal.finding.kind.clone(),
            iccid: p.proposal.sim.iccid.clone(),
            epoch: p.proposal.finding.epoch.clone(),
        }
    }
}

/// A recovered attempt closes its finding as resolved now (the recheck already proved the
/// internet works over the cellular path); anything else leaves it to the passive checks.
/// True: a finding closed.
pub fn settle(sh: &Shared, t: &Target, r: &Record, now: u64) -> bool {
    r.outcome == Outcome::Recovered
        && sh
            .findings
            .resolve(&t.kind, &t.iccid, &t.epoch, &sh.watch, now)
}

/// Wait for the end (at most `max`), recheck when it was confirmed and the epochs from the
/// start are known, record. The record, or None when this loop never started `op`.
async fn follow<D: Device + 'static>(
    engine: &Engine<D>,
    lp: &'static Loop,
    op: &str,
    then: Option<Epochs>,
    hub: Arc<crate::block::Hub>,
    max: Duration,
) -> Option<Record> {
    let start = tokio::time::Instant::now();
    let end = loop {
        if let Some(t) = txn_end(&engine.status(Some(op))) {
            break t;
        }
        if start.elapsed() > max {
            break luyaz_api::execution::TxnEnd::Unverified;
        }
        tokio::time::sleep(FOLLOW_EVERY).await;
    };
    let recheck = match then {
        Some(then) if end == luyaz_api::execution::TxnEnd::Confirmed => {
            tokio::task::spawn_blocking(move || recheck(&hub, &then))
                .await
                .ok()
                .flatten()
        }
        _ => None,
    };
    let r = lp.end(op, luyaz_netwatch::now_unix(), end, recheck.as_ref());
    eprintln!("loop: {op} ended {end:?}: {r:?}");
    r
}

/// One bound run over the cellular interface (blocking). A redial makes a new connection on
/// purpose, so the connection epoch is taken from now; the card and the radio must hold.
fn recheck(hub: &crate::block::Hub, then: &Epochs) -> Option<Recheck> {
    fresh_mode_read(hub);
    let (now, _) = hub
        .firmware()
        .loop_epochs(BootTime::read().as_ref(), RADIO_EXEC_S);
    let (samples, v) = crate::o2::check(crate::ops::reconnect::IFACE)?;
    Some(redial_recheck(&samples, &v, then, &now))
}

/// [`luyaz_api::execution::https::recheck`] for a redial: the new connection is expected.
pub fn redial_recheck(
    samples: &[luyaz_api::execution::https::Sample],
    v: &luyaz_api::execution::https::Verdict,
    then: &Epochs,
    now: &Epochs,
) -> Recheck {
    let then = Epochs {
        conn: now.conn.clone(),
        ..then.clone()
    };
    luyaz_api::execution::https::recheck(samples, v, &then, now)
}

#[cfg(test)]
#[path = "tests_approve.rs"]
mod tests;
