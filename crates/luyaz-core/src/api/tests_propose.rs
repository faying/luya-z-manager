use super::*;
use luyaz_api::execution::no_internet::Layer;
use luyaz_api::execution::{Approval, ApprovedBy, Refuse, check_execution};

fn premises() -> Premises {
    Premises {
        sim_present: Some(true),
        airplane: Some(false),
        data_on: Some(true),
        roaming: Some(false),
        roam_allowed: Some(false),
    }
}

/// Down 90 s, registered, same card, fresh; rmnet as given.
fn ev(rmnet: Option<bool>) -> Evidence {
    evidence(&premises(), Reg::Home, Some(true), Some(90), Some(5), rmnet)
}

fn epochs() -> Epochs {
    Epochs {
        sim: Some("s".into()),
        conn: Some("c".into()),
        radio: Some("r".into()),
    }
}

fn binding(epochs: Epochs) -> Binding<'static> {
    Binding {
        finding_epoch: "fe",
        finding_revision: 3,
        iccid: "89860000000000001234",
        epochs,
        boot_id: "boot",
        boot_ms: Some(1_000),
        user_writes: 7,
    }
}

#[test]
fn no_rmnet_address_proposes_a_reconnect_whatever_wwaniface_says() {
    // wwaniface isn't read at all: the session is the rmnet address (B31, 10-20)
    let a = replay(&ev(Some(false)));
    assert_eq!(
        a,
        Advice::ReadOnly {
            layer: Layer::DataSession,
            missing: vec![]
        }
    );
    let p = build(&a, &binding(epochs())).expect("a proposal");
    assert_eq!(p.action_id, "data.reconnect");
    assert_eq!(p.rollback_policy, RollbackPolicy::None);
    assert_eq!(p.expires_at_boot_ms, 1_000 + TTL_MS);
    assert_eq!(p.params_digest, params_digest("data.reconnect", &json!({})));
    assert!(crate::ops::engine::valid_op_id(&format!(
        "loop-{}",
        p.proposal_id
    )));
}

#[test]
fn an_address_or_an_unreadable_one_proposes_nothing() {
    // with an address the later layers decide, and they carry no action
    assert_eq!(build(&replay(&ev(Some(true))), &binding(epochs())), None);
    assert_eq!(build(&replay(&ev(None)), &binding(epochs())), None);
}

#[test]
fn expected_outage_short_down_other_card_or_stale_propose_nothing() {
    let mut p = premises();
    p.data_on = Some(false);
    let cases = [
        evidence(&p, Reg::Home, Some(true), Some(90), Some(5), Some(false)),
        evidence(
            &premises(),
            Reg::Home,
            Some(true),
            Some(30),
            Some(5),
            Some(false),
        ),
        evidence(
            &premises(),
            Reg::Home,
            Some(false),
            Some(90),
            Some(5),
            Some(false),
        ),
        evidence(
            &premises(),
            Reg::Home,
            Some(true),
            Some(90),
            Some(60),
            Some(false),
        ),
        evidence(
            &premises(),
            Reg::Home,
            Some(true),
            None,
            Some(5),
            Some(false),
        ),
        // not registered: a registration fault, no action
        evidence(
            &premises(),
            Reg::NotRegistered,
            Some(true),
            Some(90),
            Some(5),
            Some(false),
        ),
        evidence(
            &premises(),
            Reg::Unknown,
            Some(true),
            Some(90),
            Some(5),
            Some(false),
        ),
    ];
    for e in cases {
        assert_eq!(build(&replay(&e), &binding(epochs())), None, "{e:?}");
    }
}

#[test]
fn a_sim_switch_leaving_or_changed_proposes_nothing() {
    use crate::sim_identity::Phase;
    for (phase, want) in [
        (Phase::Leaving, false),
        (Phase::Changed, false),
        (Phase::Online, true),
        (Phase::Stalled, true),
    ] {
        let mut e = ev(Some(false));
        sim_switching(&mut e, phase);
        let p = build(&replay(&e), &binding(epochs()));
        assert_eq!(p.is_some(), want, "{phase:?}");
    }
}

#[test]
fn any_unknown_epoch_or_binding_proposes_nothing() {
    let a = replay(&ev(Some(false)));
    for f in [
        |e: &mut Epochs| e.sim = None,
        |e: &mut Epochs| e.conn = Some(String::new()),
        // a stale or non-ONLINE mode read leaves the radio epoch None (`loop_epochs`)
        |e: &mut Epochs| e.radio = None,
    ] {
        let mut e = epochs();
        f(&mut e);
        assert_eq!(build(&a, &binding(e)), None);
    }
    for f in [
        |b: &mut Binding| b.finding_epoch = "",
        |b: &mut Binding| b.iccid = "",
        |b: &mut Binding| b.boot_id = "",
        |b: &mut Binding| b.boot_ms = None,
    ] {
        let mut b = binding(epochs());
        f(&mut b);
        assert_eq!(build(&a, &b), None);
    }
}

#[test]
fn the_id_is_stable_per_binding_and_changes_with_revision_or_connection() {
    let a = replay(&ev(Some(false)));
    let id = |b: Binding| build(&a, &b).unwrap().proposal_id;
    let base = id(binding(epochs()));
    assert_eq!(base, id(binding(epochs())));
    let mut b = binding(epochs());
    b.finding_revision = 4;
    assert_ne!(base, id(b));
    let mut e = epochs();
    e.conn = Some("c2".into());
    assert_ne!(base, id(binding(e)));
}

fn approval(p: &Proposal, device: &str) -> Approval {
    Approval {
        proposal_id: p.proposal_id.clone(),
        params_digest: p.params_digest.clone(),
        device_id: device.into(),
        by: ApprovedBy::Human,
        via: "web".into(),
        permitted: true,
        expires_at_boot_ms: p.expires_at_boot_ms,
    }
}

fn now(p: &Proposal) -> Now {
    Now {
        boot_id: p.boot_id.clone(),
        boot_ms: Some(p.created_at_boot_ms + 1),
        device_id: "dev".into(),
        sim: Some(p.sim.clone()),
        epochs: p.epochs.clone(),
        gate_granted: true,
        human_wrote: false,
    }
}

fn snap(revision: u64) -> FindingSnapshot {
    FindingSnapshot {
        open: true,
        epoch: Some("fe".into()),
        revision: Some(revision),
    }
}

#[test]
fn a_built_proposal_passes_the_precheck_and_a_revision_bump_refuses() {
    let p = build(&replay(&ev(Some(false))), &binding(epochs())).unwrap();
    let a = approval(&p, "dev");
    assert_eq!(check_execution(&p, &a, &snap(3), &now(&p)), Ok(()));
    assert_eq!(
        check_execution(&p, &a, &snap(4), &now(&p)),
        Err(Refuse::FindingChanged)
    );
    let mut n = now(&p);
    n.epochs.radio = None;
    assert_eq!(
        check_execution(&p, &a, &snap(3), &n),
        Err(Refuse::RadioUnknown)
    );
}

#[test]
fn the_producer_keeps_one_and_drops_it_on_expiry_or_other_boot() {
    let prod = Producer::default();
    let mut b = binding(epochs());
    b.boot_id = boot_id();
    let p = build(&replay(&ev(Some(false))), &b).unwrap();
    assert!(!prod.has("fe", 3, Some("c"), 7));
    prod.offer(Pending {
        proposal: p.clone(),
        user_writes: 7,
    });
    assert!(prod.has("fe", 3, Some("c"), 7));
    assert!(!prod.has("fe", 4, Some("c"), 7));
    assert!(!prod.has("fe", 3, Some("c2"), 7));
    assert!(!prod.has("fe", 3, Some("c"), 8));
    // same id offered again keeps the first (its user-write mark too)
    prod.offer(Pending {
        proposal: p.clone(),
        user_writes: 9,
    });
    assert_eq!(prod.current(Some(1_001)).unwrap().user_writes, 7);
    assert!(prod.current(Some(1_000 + TTL_MS)).is_none());
    // the view carries the card's tail only
    let v = prod.current(Some(1_001)).unwrap().view();
    assert_eq!(v["iccid_tail"], "1234");
    assert!(!v.to_string().contains("89860000"));
    prod.clear();
    assert!(prod.current(Some(1_001)).is_none());
}

#[test]
fn a_human_write_makes_a_new_id_and_the_old_approval_no_longer_matches() {
    let a = replay(&ev(Some(false)));
    let old = build(&a, &binding(epochs())).unwrap();
    let mut b = binding(epochs());
    b.user_writes = 8;
    let new = build(&a, &b).unwrap();
    assert_ne!(old.proposal_id, new.proposal_id);
    assert_eq!(
        check_execution(&new, &approval(&old, "dev"), &snap(3), &now(&new)),
        Err(Refuse::ApprovalMismatch)
    );
}

#[test]
fn an_unreadable_rmnet_is_unknown_not_down() {
    let gone = || Err(std::io::Error::from(std::io::ErrorKind::NotFound));
    let denied = || Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
    // `ip` failed (no such device): unknown, so no proposal
    assert_eq!(rmnet_state(None, Ok(String::new())), None);
    assert_eq!(
        build(&replay(&ev(rmnet_state(None, gone()))), &binding(epochs())),
        None
    );
    assert_eq!(rmnet_state(Some(""), denied()), None);
    // no IPv6 file = no IPv6
    assert_eq!(rmnet_state(Some(""), gone()), Some(false));
    let v4 = "5: rmnet_data0    inet 10.1.2.3/30 scope global rmnet_data0";
    assert_eq!(rmnet_state(Some(v4), gone()), Some(true));
}
