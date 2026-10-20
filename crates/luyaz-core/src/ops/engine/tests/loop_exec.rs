//! The loop's execution entry (api/execute.rs) through the real engine and a fake device:
//! read-only while anything is missing; check_execution under the engine lock; the binding
//! saved before the write; the outcome only from a recheck.

use super::*;
use crate::api::execute::{Exec, Loop, txn_end};
use luyaz_api::execution::{
    Approval, ApprovedBy, Check, Epochs, FindingRef, FindingSnapshot, Now, Outcome, Proposal,
    Recheck, RollbackPolicy, SimId,
};

fn epochs() -> Epochs {
    Epochs {
        sim: Some("s".into()),
        conn: Some("c".into()),
        radio: Some("r".into()),
    }
}

fn proposal() -> Proposal {
    Proposal {
        proposal_id: "p1".into(),
        finding: FindingRef {
            finding_id: "f".into(),
            kind: "data.no_internet".into(),
            epoch: "e".into(),
            revision: 3,
        },
        boot_id: "b".into(),
        sim: SimId {
            iccid: "8986".into(),
            slot: 1,
        },
        epochs: epochs(),
        action_id: spec().action.into(),
        params_digest: "d".into(),
        reason: String::new(),
        excluded: vec![],
        rollback_policy: RollbackPolicy::None,
        created_at_boot_ms: 0,
        expires_at_boot_ms: 600_000,
    }
}

fn approval() -> Approval {
    Approval {
        proposal_id: "p1".into(),
        params_digest: "d".into(),
        device_id: "dev".into(),
        by: ApprovedBy::Human,
        via: "web".into(),
        permitted: true,
        expires_at_boot_ms: 600_000,
    }
}

fn current(revision: u64) -> impl FnOnce(bool) -> (FindingSnapshot, Now) + Send + 'static {
    move |held| {
        (
            FindingSnapshot {
                open: true,
                epoch: Some("e".into()),
                revision: Some(revision),
            },
            Now {
                boot_id: "b".into(),
                boot_ms: Some(1_000),
                device_id: "dev".into(),
                sim: Some(SimId {
                    iccid: "8986".into(),
                    slot: 1,
                }),
                epochs: epochs(),
                gate_granted: held,
                human_wrote: false,
            },
        )
    }
}

/// An engine with an admission gate, as on the device (`Compat::from_host`).
fn gated(dev: &Dev) -> Engine<Dev> {
    let e = engine(dev, true, None);
    e.set_gate(TxnGate {
        gate: Arc::default(),
        others: Arc::new(|| false),
        wait_ms: 1_000,
    });
    e
}

fn file(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("luyaz-loopx-{name}-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    d.join("loop.json")
}

#[tokio::test(start_paused = true)]
async fn production_entry_runs_nothing_the_allow_list_does_not_name() {
    let dev = Dev::new();
    let e = engine(&dev, true, None);
    let l = Loop::load(file("ro"));
    let mut a = approval();
    a.by = ApprovedBy::AgentRestricted;
    let got = l
        .execute(
            &e,
            &proposal(),
            &a,
            req("Only_LTE", Source::Web),
            0,
            current(3),
        )
        .await;
    assert_eq!(got, Exec::NotAllowed);
    assert!(dev.s().writes.is_empty());
    assert!(l.records().is_empty(), "nothing reserved or spent");
}

#[tokio::test(start_paused = true)]
async fn a_finding_that_changed_under_the_lock_writes_nothing() {
    let dev = Dev::new();
    let e = engine(&dev, true, None);
    let l = Loop::load(file("changed"));
    let got = l
        .execute_unchecked(
            &e,
            &proposal(),
            &approval(),
            req("Only_LTE", Source::Web),
            0,
            current(4),
        )
        .await;
    assert_eq!(got, Exec::Refused("finding_changed".into()));
    assert!(dev.s().writes.is_empty());
    assert_eq!(l.records().len(), 1, "recorded as refused");
    // spent: the producer won't offer this id again (api/propose.rs)
    assert!(l.is_spent("p1"));
    // the proposal is spent: asking again gives the same (dead) op, never a new write
    let again = l
        .execute_unchecked(
            &e,
            &proposal(),
            &approval(),
            req("Only_LTE", Source::Web),
            1,
            current(3),
        )
        .await;
    assert_eq!(again, Exec::Existing("loop-p1".into()));
    assert!(dev.s().writes.is_empty());
}

#[tokio::test(start_paused = true)]
async fn an_approved_proposal_runs_once_and_ends_by_its_recheck() {
    let dev = Dev::new();
    let e = gated(&dev);
    let l = Loop::load(file("ok"));
    let got = l
        .execute_unchecked(
            &e,
            &proposal(),
            &approval(),
            req("Only_LTE", Source::Web),
            0,
            current(3),
        )
        .await;
    assert_eq!(got, Exec::Started("loop-p1".into()));
    assert_eq!(dev.s().writes, ["Only_LTE"]);
    assert_eq!(
        dev.s().strict,
        [true],
        "the loop uses the strict write lock"
    );
    // a restart before the end: the reloaded loop still lists it for the startup sweep
    assert_eq!(Loop::load(file("ok")).open(), [("loop-p1".to_string(), 0)]);
    let fin = settle(&e, "loop-p1").await;
    let t = txn_end(&fin).expect("final");
    let still = Recheck {
        layers: vec![("exit".into(), Check::Fail)],
        valid: true,
    };
    let r = l.end("loop-p1", 10, t, Some(&still)).unwrap();
    if t == luyaz_api::execution::TxnEnd::Confirmed {
        assert_eq!(r.outcome, Outcome::StillFaulty);
    }
    assert!(l.open().is_empty());
    let again = l
        .execute_unchecked(
            &e,
            &proposal(),
            &approval(),
            req("Only_LTE", Source::Web),
            11,
            current(3),
        )
        .await;
    assert_eq!(again, Exec::Existing("loop-p1".into()));
    assert_eq!(dev.s().writes.first().map(String::as_str), Some("Only_LTE"));
}

#[tokio::test(start_paused = true)]
async fn without_an_admission_gate_the_loop_does_not_write() {
    let dev = Dev::new();
    let e = engine(&dev, true, None);
    let l = Loop::load(file("nogate"));
    let got = l
        .execute_unchecked(
            &e,
            &proposal(),
            &approval(),
            req("Only_LTE", Source::Web),
            0,
            current(3),
        )
        .await;
    assert_eq!(got, Exec::Refused("gate_not_held".into()));
    assert!(dev.s().writes.is_empty());
}
