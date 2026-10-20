use super::*;
use luyaz_api::execution::{Check, Epochs, FindingRef, RollbackPolicy, SimId};

fn proposal(id: &str, conn: &str) -> Proposal {
    Proposal {
        proposal_id: id.into(),
        finding: FindingRef {
            finding_id: "f".into(),
            kind: "data.no_internet".into(),
            epoch: "e".into(),
            revision: 1,
        },
        boot_id: "b".into(),
        sim: SimId {
            iccid: "8986".into(),
            slot: 1,
        },
        epochs: Epochs {
            sim: Some("s".into()),
            conn: Some(conn.into()),
            radio: Some("r".into()),
        },
        action_id: "apn.use".into(),
        params_digest: "d".into(),
        reason: String::new(),
        excluded: vec![],
        rollback_policy: RollbackPolicy::None,
        created_at_boot_ms: 0,
        expires_at_boot_ms: 1,
    }
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("luyaz-loop-{name}-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    d.join("loop.json")
}

#[test]
fn no_entry_condition_is_missing_any_more() {
    // §3: O2/O3/T4/B31 closed on the device, O5 by the owner (10-20)
    assert!(missing().is_empty());
}

#[test]
fn one_proposal_one_op_and_it_survives_a_restart() {
    let path = tmp("restart");
    let l = Loop::load(path.clone());
    let op = l.reserve(&proposal("p1", "c1"), 1_000).unwrap();
    assert_eq!(op, "loop-p1");
    assert_eq!(
        l.reserve(&proposal("p1", "c1"), 1_001),
        Err(Exec::Existing(op.clone()))
    );
    // a core restart reads the binding back: still never a second transaction
    let again = Loop::load(path);
    assert_eq!(
        again.reserve(&proposal("p1", "c1"), 1_002),
        Err(Exec::Existing(op))
    );
}

#[test]
fn an_unsaveable_reservation_refuses() {
    let l = Loop::load(PathBuf::from("/nonexistent-dir/loop.json"));
    assert!(matches!(
        l.reserve(&proposal("p1", "c1"), 0),
        Err(Exec::Refused(_))
    ));
    assert!(lock(&l.file).tried.is_empty(), "nothing left reserved");
}

#[test]
fn one_attempt_per_key_then_cooldown_then_a_daily_stop() {
    let l = Loop::load(tmp("cool"));
    let op = l.reserve(&proposal("p1", "c1"), 0).unwrap();
    assert_eq!(
        l.reserve(&proposal("p2", "c1"), 10),
        Err(Exec::Cooldown("attempt_running"))
    );
    // another connection is another key
    assert!(l.reserve(&proposal("p3", "c2"), 10).is_ok());
    l.end(&op, 100, TxnEnd::Unverified, None).unwrap();
    assert_eq!(
        l.reserve(&proposal("p2", "c1"), 100 + COOLDOWN_S - 1),
        Err(Exec::Cooldown("cooldown"))
    );
    let op2 = l.reserve(&proposal("p2", "c1"), 100 + COOLDOWN_S).unwrap();
    l.end(&op2, 200 + COOLDOWN_S, TxnEnd::RolledBack, None)
        .unwrap();
    assert_eq!(
        l.reserve(&proposal("p4", "c1"), 300 + 2 * COOLDOWN_S),
        Err(Exec::Cooldown("stopped_after_two_failures"))
    );
    assert!(
        l.reserve(&proposal("p4", "c1"), 300 + COOLDOWN_S + DAY_S)
            .is_ok()
    );
}

#[test]
fn the_outcome_needs_the_recheck_and_is_recorded_once() {
    let l = Loop::load(tmp("outcome"));
    let op = l.reserve(&proposal("p1", "c1"), 0).unwrap();
    let pass = Recheck {
        layers: vec![("exit".into(), Check::Pass)],
        valid: true,
    };
    let r = l.end(&op, 5, TxnEnd::Confirmed, Some(&pass)).unwrap();
    assert_eq!(r.outcome, Outcome::Recovered);
    assert_eq!(
        l.end(&op, 6, TxnEnd::Confirmed, Some(&pass)),
        None,
        "only once"
    );
    assert_eq!(l.end("someone-else", 6, TxnEnd::Confirmed, None), None);
    let op = l.reserve(&proposal("p2", "c2"), 0).unwrap();
    let r = l.end(&op, 5, TxnEnd::Confirmed, None).unwrap();
    assert_eq!(
        r.outcome,
        Outcome::Unknown,
        "confirmed alone is not recovered"
    );
    assert_eq!(l.records().len(), 2);
}

#[test]
fn engine_phases_map_to_ends() {
    use serde_json::json;
    let t = |p: &str, r: &str| txn_end(&json!({"phase": p, "reason": r}));
    assert_eq!(t("confirmed", "verified"), Some(TxnEnd::Confirmed));
    assert_eq!(t("confirmed", "user_keep"), Some(TxnEnd::UserKeep));
    assert_eq!(t("rolled_back", "user_revert"), Some(TxnEnd::UserRevert));
    assert_eq!(t("rolled_back", "timeout"), Some(TxnEnd::RolledBack));
    assert_eq!(t("unverified", "no_rollback"), Some(TxnEnd::Unverified));
    assert_eq!(t("verifying", ""), None);
    assert_eq!(t("applying", ""), None);
}

#[test]
fn a_redialling_action_still_hits_the_daily_stop() {
    let l = Loop::load(tmp("redial"));
    let a = l.reserve(&proposal("p1", "c1"), 0).unwrap();
    l.end(&a, 10, TxnEnd::Unverified, None).unwrap();
    let b = l.reserve(&proposal("p2", "c2"), 20).unwrap();
    l.end(&b, 30, TxnEnd::RolledBack, None).unwrap();
    assert_eq!(
        l.reserve(&proposal("p3", "c3"), 40),
        Err(Exec::Cooldown("stopped_after_two_failures"))
    );
}

#[test]
fn a_proposal_stays_spent_after_its_attempt_is_pruned_or_refused() {
    let l = Loop::load(tmp("spent"));
    let a = l.reserve(&proposal("p1", "c1"), 0).unwrap();
    l.end(&a, 10, TxnEnd::Unverified, None).unwrap();
    // a day later the attempt is pruned by the next end
    let b = l.reserve(&proposal("p2", "c9"), 10 + DAY_S + 1).unwrap();
    l.end(&b, 10 + DAY_S + 2, TxnEnd::Unverified, None).unwrap();
    assert!(lock(&l.file).tried.iter().all(|t| t.proposal_id != "p1"));
    assert_eq!(
        l.reserve(&proposal("p1", "c1"), 10 + DAY_S + 3),
        Err(Exec::Existing(a))
    );
    // refused by the engine: no attempt counted, the proposal still spent
    let c = l.reserve(&proposal("p3", "c5"), 0).unwrap();
    l.refused(&c, 1, "busy");
    assert!(lock(&l.file).tried.iter().all(|t| t.op_id != c));
    assert_eq!(l.reserve(&proposal("p3", "c5"), 2), Err(Exec::Existing(c)));
    assert!(
        l.reserve(&proposal("p4", "c5"), 2).is_ok(),
        "no cooldown from a refusal"
    );
}
