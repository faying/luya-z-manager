use super::super::propose;
use super::*;
use crate::ops::txn::Source;
use luyaz_api::execution::Check;
use luyaz_api::execution::https::{Attempt, ClockTrust, Sample, Verdict};

fn pending() -> Pending {
    use luyaz_api::finding::{Premises, Reg};
    let premises = Premises {
        sim_present: Some(true),
        airplane: Some(false),
        data_on: Some(true),
        roaming: Some(false),
        roam_allowed: Some(false),
    };
    let e = propose::evidence(
        &premises,
        Reg::Home,
        Some(true),
        Some(90),
        Some(5),
        Some(false),
    );
    let b = propose::Binding {
        finding_epoch: "fe",
        finding_revision: 3,
        iccid: "89860000000000001234",
        epochs: ep("s", "c", "r"),
        boot_id: propose::boot_id(),
        boot_ms: Some(1_000),
        user_writes: 7,
    };
    Pending {
        proposal: propose::build(&luyaz_api::execution::no_internet::replay(&e), &b).unwrap(),
        user_writes: 7,
    }
}

fn shared(p: Option<Pending>) -> Shared {
    let s = Shared {
        producer: Arc::default(),
        findings: Arc::new(super::super::finding::Findings::load(
            std::env::temp_dir().join(format!("luyaz-approve-{}.json", std::process::id())),
        )),
        watch: Arc::new(luyaz_netwatch::Watch::new()),
    };
    if let Some(p) = p {
        s.producer.offer(p);
    }
    s
}

#[test]
fn only_the_proposal_shown_is_picked() {
    let p = pending();
    let sh = shared(Some(p.clone()));
    let id = p.proposal.proposal_id.clone();
    let d = p.proposal.params_digest.clone();
    let at = Some(p.proposal.created_at_boot_ms + 1);
    assert!(pick(&sh, &json!({"proposal_id": id, "params_digest": d}), at).is_some());
    assert!(
        pick(
            &sh,
            &json!({"proposal_id": "other", "params_digest": d}),
            at
        )
        .is_none()
    );
    assert!(pick(&sh, &json!({"proposal_id": id, "params_digest": "x"}), at).is_none());
    assert!(pick(&sh, &json!({"proposal_id": id}), at).is_none());
    // expired
    assert!(
        pick(
            &sh,
            &json!({"proposal_id": id, "params_digest": d}),
            Some(p.proposal.expires_at_boot_ms)
        )
        .is_none()
    );
    assert!(
        pick(
            &shared(None),
            &json!({"proposal_id": id, "params_digest": d}),
            at
        )
        .is_none()
    );
}

#[test]
fn the_approval_and_request_are_a_persons_redial() {
    let p = pending();
    let who = Who::from(Source::Screen);
    let a = approval(&p, "dev", who);
    assert_eq!(a.by, ApprovedBy::Human);
    assert_eq!(a.via, "screen");
    assert_eq!(a.proposal_id, p.proposal.proposal_id);
    let r = request(&p, who).unwrap();
    assert_eq!(r.spec.action, "data.reconnect");
    assert_eq!(r.target, json!("up"));
    assert_eq!(r.approved_by, Source::Screen);
    assert_eq!(
        r.expect_iccid.as_deref(),
        Some(p.proposal.sim.iccid.as_str())
    );
}

fn ep(sim: &str, conn: &str, radio: &str) -> Epochs {
    Epochs {
        sim: Some(sim.into()),
        conn: Some(conn.into()),
        radio: Some(radio.into()),
    }
}

fn verdict() -> Verdict {
    Verdict {
        app: Check::Pass,
        clock: ClockTrust::Trusted,
        skew_s: Some(1),
        outliers: vec![],
        vendor_disagrees: false,
        why: "",
    }
}

#[test]
fn a_redial_recheck_expects_a_new_connection_but_not_another_card_or_radio() {
    let s = [Sample {
        org: "apple",
        attempt: Attempt::Unreachable,
    }];
    let then = ep("s", "c1", "r");
    assert!(redial_recheck(&s, &verdict(), &then, &ep("s", "c2", "r")).valid);
    assert!(!redial_recheck(&s, &verdict(), &then, &ep("s2", "c2", "r")).valid);
    assert!(!redial_recheck(&s, &verdict(), &then, &ep("s", "c2", "r2")).valid);
    let mut unknown = ep("s", "c2", "r");
    unknown.conn = None;
    assert!(!redial_recheck(&s, &verdict(), &then, &unknown).valid);
}

#[test]
fn replies_map_to_http() {
    assert_eq!(Reply::NotAPerson.http().0, 400);
    assert_eq!(Reply::Changed.http().0, 409);
    assert_eq!(Reply::Exec(Exec::Started("loop-x".into())).http().0, 200);
    assert_eq!(Reply::Exec(Exec::NotAllowed).http().0, 403);
    assert_eq!(Reply::Exec(Exec::Existing("loop-x".into())).http().0, 409);
    assert_eq!(Reply::Exec(Exec::Cooldown("cooldown")).http().0, 409);
}

#[test]
fn only_a_recovered_attempt_closes_its_finding() {
    use luyaz_api::diagnose::Level;
    use luyaz_api::execution::TxnEnd;
    use luyaz_api::finding::{Confidence, Evidence, Finding, KIND_NO_INTERNET, LayerId};
    const T: u64 = 1_900_000_000;
    let iccid = "89860000000000001234";
    let sh = shared(None);
    let open = luyaz_api::finding::Verdict::Open(Finding {
        kind: KIND_NO_INTERNET,
        iccid: iccid.into(),
        layer: LayerId::Internet,
        confidence: Confidence::Sure,
        symptom: "x".into(),
        symptom_en: "x".into(),
        evidence: vec![Evidence::new(LayerId::Internet, Level::Bad, "x", "x", T)],
        at: T,
    });
    sh.findings.observe(&open, iccid, &sh.watch, T);
    // the store's epoch comes from /proc (Linux); without one nothing is ever bound
    let epoch = sh
        .findings
        .identity(KIND_NO_INTERNET, iccid)
        .map(|i| i.epoch)
        .unwrap_or_default();
    let t = Target {
        kind: KIND_NO_INTERNET.into(),
        iccid: iccid.into(),
        epoch: epoch.clone(),
    };
    let rec = |outcome| Record {
        outcome,
        fault: Check::Pass,
        txn: TxnEnd::Confirmed,
    };
    for o in [Outcome::StillFaulty, Outcome::Unknown, Outcome::Cancelled] {
        assert!(!settle(&sh, &t, &rec(o), T + 1));
    }
    let other = Target {
        epoch: "another-load".into(),
        ..t.clone()
    };
    assert!(!settle(&sh, &other, &rec(Outcome::Recovered), T + 1));
    assert_eq!(
        settle(&sh, &t, &rec(Outcome::Recovered), T + 2),
        !epoch.is_empty()
    );
    if !epoch.is_empty() {
        assert!(sh.findings.identity(KIND_NO_INTERNET, iccid).is_none());
        // already closed: nothing more
        assert!(!settle(&sh, &t, &rec(Outcome::Recovered), T + 3));
    }
}
