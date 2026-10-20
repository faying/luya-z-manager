use super::*;

fn sim() -> SimId {
    SimId {
        iccid: "8986001234567890123".into(),
        slot: 1,
    }
}

fn epochs() -> Epochs {
    Epochs::of(&crate::firmware::Context {
        sim_epoch: Some("s-1".into()),
        connection_epoch: Some("c-3".into()),
        radio_epoch: Some("r-2".into()),
        ..Default::default()
    })
}

fn proposal() -> Proposal {
    Proposal {
        proposal_id: "p1".into(),
        finding: FindingRef {
            finding_id: "f1".into(),
            kind: "data.no_internet".into(),
            epoch: "e1".into(),
            revision: 7,
        },
        boot_id: "b1".into(),
        sim: sim(),
        epochs: epochs(),
        action_id: "apn.use".into(),
        params_digest: "d1".into(),
        reason: "r".into(),
        excluded: vec![],
        rollback_policy: RollbackPolicy::None,
        created_at_boot_ms: 1_000,
        expires_at_boot_ms: 61_000,
    }
}

fn approval() -> Approval {
    Approval {
        proposal_id: "p1".into(),
        params_digest: "d1".into(),
        device_id: "dev".into(),
        by: ApprovedBy::Human,
        via: "web".into(),
        permitted: true,
        expires_at_boot_ms: 31_000,
    }
}

fn snap() -> FindingSnapshot {
    FindingSnapshot {
        open: true,
        epoch: Some("e1".into()),
        revision: Some(7),
    }
}

fn now() -> Now {
    Now {
        boot_id: "b1".into(),
        boot_ms: Some(2_000),
        device_id: "dev".into(),
        sim: Some(sim()),
        epochs: epochs(),
        gate_granted: true,
        human_wrote: false,
    }
}

fn check(p: Proposal, a: Approval, f: FindingSnapshot, n: Now) -> Result<(), Refuse> {
    check_execution(&p, &a, &f, &n)
}

#[test]
fn everything_matching_may_run() {
    assert_eq!(check(proposal(), approval(), snap(), now()), Ok(()));
}

type Edit = Box<dyn Fn(&mut Proposal, &mut Approval, &mut FindingSnapshot, &mut Now)>;

#[test]
fn every_mismatch_or_unknown_refuses() {
    use Refuse::*;
    let cases: Vec<(Refuse, Edit)> = vec![
        (NotPermitted, Box::new(|_, a, _, _| a.permitted = false)),
        (
            ApprovalMismatch,
            Box::new(|_, a, _, _| a.proposal_id = "p2".into()),
        ),
        (
            ApprovalMismatch,
            Box::new(|_, a, _, _| a.params_digest = "d2".into()),
        ),
        (
            ApprovalMismatch,
            Box::new(|_, _, _, n| n.device_id = "other".into()),
        ),
        (OtherBoot, Box::new(|_, _, _, n| n.boot_id = "b2".into())),
        (
            OtherBoot,
            Box::new(|p, _, _, n| {
                p.boot_id = String::new();
                n.boot_id = String::new();
            }),
        ),
        (NoClock, Box::new(|_, _, _, n| n.boot_ms = None)),
        (Expired, Box::new(|_, _, _, n| n.boot_ms = Some(31_000))),
        (
            Expired,
            Box::new(|p, _, _, n| {
                p.expires_at_boot_ms = 5_000;
                n.boot_ms = Some(5_000)
            }),
        ),
        (Expired, Box::new(|_, _, _, n| n.boot_ms = Some(999))),
        (SimUnknown, Box::new(|_, _, _, n| n.sim = None)),
        (
            SimChanged,
            Box::new(|_, _, _, n| n.sim.as_mut().unwrap().slot = 2),
        ),
        // the same tail, another card
        (
            SimChanged,
            Box::new(|_, _, _, n| n.sim.as_mut().unwrap().iccid = "8986000000000009999".into()),
        ),
        // the same card re-seated: same ICCID and slot, a new card event
        (
            SimChanged,
            Box::new(|_, _, _, n| n.epochs.sim = Some("s-2".into())),
        ),
        (SimUnknown, Box::new(|p, _, _, _| p.epochs.sim = None)),
        (SimUnknown, Box::new(|_, _, _, n| n.epochs.sim = None)),
        (
            SimUnknown,
            Box::new(|p, _, _, n| {
                p.epochs.sim = Some(String::new());
                n.epochs.sim = Some(String::new());
            }),
        ),
        (ConnUnknown, Box::new(|p, _, _, _| p.epochs.conn = None)),
        (ConnUnknown, Box::new(|_, _, _, n| n.epochs.conn = None)),
        (
            ConnUnknown,
            Box::new(|_, _, _, n| n.epochs.conn = Some(String::new())),
        ),
        (
            ConnChanged,
            Box::new(|_, _, _, n| n.epochs.conn = Some("c-4".into())),
        ),
        (RadioUnknown, Box::new(|p, _, _, _| p.epochs.radio = None)),
        (RadioUnknown, Box::new(|_, _, _, n| n.epochs.radio = None)),
        (
            RadioUnknown,
            Box::new(|_, _, _, n| n.epochs.radio = Some(String::new())),
        ),
        (
            RadioChanged,
            Box::new(|_, _, _, n| n.epochs.radio = Some("r-3".into())),
        ),
        (FindingClosed, Box::new(|_, _, f, _| f.open = false)),
        (FindingUnknown, Box::new(|_, _, f, _| f.revision = None)),
        (FindingUnknown, Box::new(|_, _, f, _| f.epoch = None)),
        (
            FindingUnknown,
            Box::new(|p, _, f, _| {
                p.finding.epoch = String::new();
                f.epoch = Some(String::new());
            }),
        ),
        (FindingChanged, Box::new(|_, _, f, _| f.revision = Some(8))),
        (
            FindingChanged,
            Box::new(|_, _, f, _| f.epoch = Some("e2".into())),
        ),
        (HumanWrote, Box::new(|_, _, _, n| n.human_wrote = true)),
        (GateNotHeld, Box::new(|_, _, _, n| n.gate_granted = false)),
    ];
    for (want, edit) in cases {
        let (mut p, mut a, mut f, mut n) = (proposal(), approval(), snap(), now());
        edit(&mut p, &mut a, &mut f, &mut n);
        assert_eq!(check(p, a, f, n), Err(want));
    }
}

#[test]
fn the_agents_restricted_approval_is_carried_not_decided() {
    let a = Approval {
        by: ApprovedBy::AgentRestricted,
        via: "mcp".into(),
        ..approval()
    };
    assert_eq!(check(proposal(), a.clone(), snap(), now()), Ok(()));
    let a = Approval {
        permitted: false,
        ..a
    };
    assert_eq!(
        check(proposal(), a, snap(), now()),
        Err(Refuse::NotPermitted)
    );
}

#[test]
fn one_proposal_one_transaction() {
    let mut l = Ledger::default();
    assert_eq!(l.bind("p1", "op-a"), Bind::New);
    assert_eq!(l.bind("p1", "op-b"), Bind::Existing("op-a".into()));
    assert_eq!(l.op_of("p1"), Some("op-a"));
    assert_eq!(l.bind("p2", "op-b"), Bind::New);
}

#[test]
fn states_only_move_forward_and_finals_stay() {
    use State::*;
    let path = [
        Open,
        Proposed,
        Approved,
        Applying,
        TxnVerifying,
        FaultRechecking,
        Recovered,
    ];
    for w in path.windows(2) {
        assert_eq!(advance(w[0], w[1]), Some(w[1]));
    }
    assert_eq!(
        advance(Applying, Recovered),
        None,
        "no skipping the recheck"
    );
    assert_eq!(advance(TxnVerifying, Recovered), None);
    assert_eq!(
        advance(Unknown, Recovered),
        None,
        "unknown never turns into success"
    );
    assert_eq!(advance(Approved, Cancelled), Some(Cancelled));
    for f in [Recovered, StillFaulty, Unknown, Cancelled] {
        assert_eq!(advance(f, Cancelled), None);
    }
}

fn rc(layers: &[Check], valid: bool) -> Recheck {
    Recheck {
        layers: layers
            .iter()
            .enumerate()
            .map(|(i, c)| (format!("l{i}"), *c))
            .collect(),
        valid,
    }
}

#[test]
fn a_confirmed_transaction_alone_is_not_recovered() {
    let r = outcome(TxnEnd::Confirmed, None);
    assert_eq!((r.outcome, r.fault), (Outcome::Unknown, Check::Unknown));
    assert!(!resolves_finding(&r));
    // an invalid recheck (another card / exit / untrusted clock) counts for nothing
    let r = outcome(
        TxnEnd::Confirmed,
        Some(&rc(&[Check::Pass, Check::Pass], false)),
    );
    assert_eq!(r.outcome, Outcome::Unknown);
    // one unknown layer keeps it unknown
    let r = outcome(
        TxnEnd::Confirmed,
        Some(&rc(&[Check::Pass, Check::Unknown], true)),
    );
    assert_eq!(r.outcome, Outcome::Unknown);
    let r = outcome(TxnEnd::Confirmed, Some(&rc(&[], true)));
    assert_eq!(r.outcome, Outcome::Unknown);
    let r = outcome(
        TxnEnd::Confirmed,
        Some(&rc(&[Check::Pass, Check::Fail], true)),
    );
    assert_eq!(r.outcome, Outcome::StillFaulty);
    let r = outcome(
        TxnEnd::Confirmed,
        Some(&rc(&[Check::Pass, Check::Pass], true)),
    );
    assert_eq!((r.outcome, r.txn), (Outcome::Recovered, TxnEnd::Confirmed));
    assert!(resolves_finding(&r));
}

#[test]
fn human_kept_and_rollbacks_keep_the_fault_apart() {
    let r = outcome(TxnEnd::UserKeep, None);
    assert_eq!((r.outcome, r.fault), (Outcome::HumanKept, Check::Unknown));
    assert!(!resolves_finding(&r));
    let r = outcome(TxnEnd::UserKeep, Some(&rc(&[Check::Fail], true)));
    assert_eq!((r.outcome, r.fault), (Outcome::HumanKept, Check::Fail));
    let r = outcome(TxnEnd::RolledBack, Some(&rc(&[Check::Pass], true)));
    assert_eq!(r.outcome, Outcome::RolledBack);
    assert!(!resolves_finding(&r), "a rollback is never the fault fixed");
    for (t, o) in [
        (TxnEnd::RollbackFailed, Outcome::RollbackFailed),
        (TxnEnd::NotApplied, Outcome::NotApplied),
        (TxnEnd::Unverified, Outcome::Unknown),
        (TxnEnd::UserRevert, Outcome::Cancelled),
        (TxnEnd::Cancelled, Outcome::Cancelled),
    ] {
        let r = outcome(t, Some(&rc(&[Check::Pass], true)));
        assert_eq!((r.outcome, r.txn), (o, t));
        assert!(!resolves_finding(&r));
    }
}
