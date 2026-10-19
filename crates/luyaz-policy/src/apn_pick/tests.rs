//! zte-agent `apn_pick.rs`'s tests, moved as they are, except: `op_busy_is_retried_and_not_counted`
//! lost its two lines about the core's reply type (host side). Left in the agent:
//! `same_profile_matches_numbers_and_strings`, `notice_only_while_fresh_unless_sticky`,
//! `store_round_trips` (profile matching, the notice and the file are host side).
use super::*;

// Fake ICCIDs: these repos sync to a public mirror.
const A: &str = "8900000000000000001";
const B: &str = "8900000000000000002";

type Apn = Option<(bool, Option<String>)>;

fn apn(manual: bool, sel: Option<&str>) -> Apn {
    Some((manual, sel.map(str::to_string)))
}

/// Two looks at `iccid`; the first never acts. Commits like the watcher
/// does when the action is None.
fn twice(s: &mut Store, w: &mut Watch, iccid: &str, a: Apn) -> Action {
    assert_eq!(
        decide(s, w, iccid, false, || a.clone()),
        NOTHING,
        "first read never acts"
    );
    let d = decide(s, w, iccid, false, || a);
    if let (Action::None, Some(c)) = (&d.action, &d.card) {
        s.last_iccid = c.clone();
    }
    d.action
}

fn store_with_pick() -> Store {
    let mut s = Store {
        last_iccid: A.into(),
        ..Default::default()
    };
    s.picks.insert(
        A.into(),
        Pick {
            manual_id: "manu4".into(),
            apn: "ctnet".into(),
        },
    );
    s
}

/// core answered op_busy (D40): the watcher leaves the Watch as it is,
/// so the next look acts again, and it never counts toward giving up.
#[test]
fn op_busy_is_retried_and_not_counted() {
    let mut s = store_with_pick();
    s.last_iccid = B.into();
    let mut w = Watch::default();
    assert_eq!(
        twice(&mut s, &mut w, A, apn(false, None)),
        Action::Use("manu4".into())
    );
    for _ in 0..(MAX_FAILS + 2) {
        let d = decide(&s, &mut w, A, false, || apn(false, None));
        assert_eq!(d.action, Action::Use("manu4".into()));
        assert!(w.eager(), "keeps looking soon");
    }
    assert_eq!(w.fails, 0);
}

#[test]
fn iccid_pad_and_tail() {
    assert_eq!(norm_iccid("8900000000000000072F"), "8900000000000000072");
    assert_eq!(tail4("8900000000000000001"), "0001");
    assert_eq!(tail4("12"), "12");
}

#[test]
fn iot_names() {
    for a in ["ctiot", "CMIOT5G", "m2m.example", "iot.1nce.net"] {
        assert!(is_iot(a), "{a}");
    }
    for a in ["ctnet", "plus.4g", "uad5gn.au-net.ne.jp", "cmnet"] {
        assert!(!is_iot(a), "{a}");
    }
}

#[test]
fn same_card_does_nothing_and_reads_no_apn() {
    let s = store_with_pick();
    let mut w = Watch::default();
    for _ in 0..3 {
        assert_eq!(
            decide(&s, &mut w, A, false, || panic!(
                "no APN read without a card change"
            )),
            NOTHING
        );
    }
}

#[test]
fn new_card_while_on_our_copy_goes_auto() {
    let mut s = store_with_pick();
    let mut w = Watch::default();
    assert_eq!(
        twice(&mut s, &mut w, B, apn(true, Some("manu4"))),
        Action::Auto
    );
    assert_eq!(s.last_iccid, A, "not committed until the switch worked");
}

#[test]
fn picking_card_back_uses_its_pick() {
    let mut s = store_with_pick();
    s.last_iccid = B.into();
    let mut w = Watch::default();
    assert_eq!(
        twice(&mut s, &mut w, A, apn(false, None)),
        Action::Use("manu4".into())
    );
    let mut w = Watch::default();
    assert_eq!(
        twice(&mut s, &mut w, A, apn(true, Some("manu4"))),
        Action::None,
        "already dialling it"
    );
    assert_eq!(s.last_iccid, A);
}

#[test]
fn going_from_one_pick_to_another_is_one_switch() {
    let mut s = store_with_pick();
    s.picks.insert(
        B.into(),
        Pick {
            manual_id: "manu5".into(),
            apn: "plus.4g".into(),
        },
    );
    let mut w = Watch::default();
    assert_eq!(
        twice(&mut s, &mut w, B, apn(true, Some("manu4"))),
        Action::Use("manu5".into())
    );
}

#[test]
fn owners_own_manual_apn_is_never_touched() {
    let mut s = store_with_pick();
    let mut w = Watch::default();
    assert_eq!(
        twice(&mut s, &mut w, B, apn(true, Some("manu2"))),
        Action::None
    );
    let mut w = Watch::default();
    assert_eq!(
        twice(&mut s, &mut w, A, apn(true, Some("manu2"))),
        Action::None
    );
}

#[test]
fn new_card_on_auto_with_no_pick_does_nothing() {
    let mut s = store_with_pick();
    let mut w = Watch::default();
    assert_eq!(
        twice(&mut s, &mut w, B, apn(false, Some("manu4"))),
        Action::None
    );
    assert_eq!(s.last_iccid, B, "learned");
}

#[test]
fn empty_flicker_and_busy_never_act() {
    let s = store_with_pick();
    let mut w = Watch::default();
    let a = || apn(true, Some("manu4"));
    assert_eq!(decide(&s, &mut w, B, false, a), NOTHING);
    assert_eq!(decide(&s, &mut w, "", false, a), NOTHING);
    assert_eq!(
        decide(&s, &mut w, B, false, a),
        NOTHING,
        "the empty read restarted the count"
    );
    assert_eq!(decide(&s, &mut w, B, true, a), NOTHING);
    assert_eq!(decide(&s, &mut w, B, true, a), NOTHING);
}

#[test]
fn unreadable_apn_state_retries() {
    let s = store_with_pick();
    let mut w = Watch::default();
    assert_eq!(decide(&s, &mut w, B, false, || None), NOTHING);
    assert_eq!(
        decide(&s, &mut w, B, false, || None),
        NOTHING,
        "can't read the APN: no card commit"
    );
    let d = decide(&s, &mut w, B, false, || apn(true, Some("manu4")));
    assert_eq!(
        d,
        Decision {
            action: Action::Auto,
            card: Some(B.into())
        }
    );
}

#[test]
fn failed_claim_or_switch_is_retried_on_the_next_look() {
    let s = store_with_pick();
    let mut w = Watch::default();
    let a = || apn(true, Some("manu4"));
    assert_eq!(decide(&s, &mut w, B, false, a), NOTHING);
    assert_eq!(decide(&s, &mut w, B, false, a).action, Action::Auto);
    // The watcher could not claim / the switch failed: nothing committed,
    // the very next look acts again (no new two-read wait).
    assert!(w.eager());
    w.fails += 1;
    assert_eq!(decide(&s, &mut w, B, false, a).action, Action::Auto);
    assert_eq!(w.fails, 1, "fail count kept for the same card");
}

#[test]
fn first_run_only_learns_the_card() {
    let mut s = Store::default();
    let mut w = Watch::default();
    assert_eq!(
        twice(&mut s, &mut w, A, apn(true, Some("manu4"))),
        Action::None
    );
    assert_eq!(s.last_iccid, A);
}

/// Replay = the same decision on recorded inputs, and it tells whether the APN state was asked.
#[test]
fn replay_matches_decide() {
    let s = store_with_pick();
    let seen = Watch {
        pending: B.into(),
        seen: 1,
        fails: 0,
    };
    let (d, w, asked) = replay(
        &s,
        &seen,
        B,
        false,
        &ApnRead::Read {
            manual: true,
            selected: Some("manu4".into()),
        },
    );
    assert_eq!(
        d,
        Decision {
            action: Action::Auto,
            card: Some(B.into())
        }
    );
    assert_eq!((w.seen, asked), (2, true));
    let (d, w, asked) = replay(&s, &Watch::default(), B, false, &ApnRead::NotAsked);
    assert_eq!((d, w.seen, asked), (NOTHING, 1, false));
    let (d, _, asked) = replay(&s, &seen, B, false, &ApnRead::Unread);
    assert_eq!((d, asked), (NOTHING, true));
}
