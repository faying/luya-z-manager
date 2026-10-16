//! zte-agent `sms_forward.rs`'s plan and outcome tests, moved as they are (the two error
//! constants written out). `run_pass` is compared with the agent's own on random lists in
//! zte-agent (contract test); the hold has its own cases below.

use super::*;

const T0: i64 = 1_000_000;

fn snap(max_id: u64, received: &[(u64, Option<i64>)]) -> DeviceSnapshot {
    DeviceSnapshot {
        max_id,
        received: received.to_vec(),
    }
}

#[test]
fn normal_increasing_ids_forward_only_new() {
    let s = snap(
        103,
        &[
            (99, Some(T0 - 20)),
            (100, Some(T0)),
            (101, Some(T0 + 10)),
            (103, Some(T0 + 30)),
        ],
    );
    let p = plan_forward(100, Some(T0), Ok(&s));
    assert!(!p.rollback);
    assert_eq!(p.forward_ids, vec![101, 103]);
    assert_eq!(p.new_watermark, 103);
    assert_eq!(p.new_last_time, Some(T0 + 30));
}

#[test]
fn nothing_new_changes_nothing() {
    let s = snap(100, &[(99, Some(T0 - 20)), (100, Some(T0))]);
    let p = plan_forward(100, Some(T0), Ok(&s));
    assert_eq!(
        p,
        ForwardPlan {
            forward_ids: vec![],
            new_watermark: 100,
            new_last_time: Some(T0),
            rollback: false
        }
    );
}

#[test]
fn rollback_forwards_ids_1_and_2() {
    // watermark 100, device numbering reset, new SMS 1 and 2 arrive
    let s = snap(2, &[(2, Some(T0 + 60)), (1, Some(T0 + 30))]);
    let p = plan_forward(100, Some(T0), Ok(&s));
    assert!(p.rollback);
    assert_eq!(p.forward_ids, vec![1, 2]);
    assert_eq!(p.new_watermark, 2);
    assert_eq!(p.new_last_time, Some(T0 + 60));
    // next round: nothing is re-sent
    let p2 = plan_forward(p.new_watermark, p.new_last_time, Ok(&s));
    assert!(!p2.rollback);
    assert!(p2.forward_ids.is_empty());
    // and SMS 3 goes through the normal path
    let s3 = snap(
        3,
        &[(1, Some(T0 + 30)), (2, Some(T0 + 60)), (3, Some(T0 + 90))],
    );
    let p3 = plan_forward(p.new_watermark, p.new_last_time, Ok(&s3));
    assert_eq!(p3.forward_ids, vec![3]);
    assert_eq!(p3.new_watermark, 3);
}

#[test]
fn rollback_does_not_resend_old_sms() {
    // after the reset the device still holds older messages (time <= last forwarded)
    let s = snap(
        4,
        &[
            (1, Some(T0 - 500)),
            (2, Some(T0)),
            (3, None),
            (4, Some(T0 + 5)),
        ],
    );
    let p = plan_forward(100, Some(T0), Ok(&s));
    assert!(p.rollback);
    assert_eq!(p.forward_ids, vec![4]);
    assert_eq!(p.new_watermark, 4);
    assert_eq!(p.new_last_time, Some(T0 + 5));

    let only_old = snap(2, &[(1, Some(T0 - 500)), (2, Some(T0 - 1))]);
    let p = plan_forward(100, Some(T0), Ok(&only_old));
    assert!(p.rollback);
    assert!(p.forward_ids.is_empty());
    assert_eq!(p.new_watermark, 2);
    assert_eq!(p.new_last_time, Some(T0));
}

#[test]
fn rollback_without_last_time_is_conservative() {
    // old state file: no time recorded -> forward nothing, rebase on device
    let s = snap(2, &[(1, Some(T0 + 30)), (2, Some(T0 + 60))]);
    let p = plan_forward(100, None, Ok(&s));
    assert!(p.rollback);
    assert!(p.forward_ids.is_empty());
    assert_eq!(p.new_watermark, 2);
    assert_eq!(p.new_last_time, Some(T0 + 60));
}

#[test]
fn missing_last_time_backfilled_from_covered_messages() {
    let s = snap(
        102,
        &[(99, Some(T0 - 10)), (100, Some(T0)), (102, Some(T0 + 20))],
    );
    let p = plan_forward(100, None, Ok(&s));
    assert!(!p.rollback);
    assert_eq!(p.forward_ids, vec![102]);
    assert_eq!(p.new_last_time, Some(T0 + 20));
    let s = snap(100, &[(99, Some(T0 - 10)), (100, Some(T0))]);
    assert_eq!(plan_forward(100, None, Ok(&s)).new_last_time, Some(T0));
}

#[test]
fn empty_device_is_not_a_rollback() {
    let p = plan_forward(100, Some(T0), Ok(&snap(0, &[])));
    assert!(!p.rollback);
    assert_eq!(p.new_watermark, 100);
    assert!(p.forward_ids.is_empty());
}

#[test]
fn delivery_outcome_classifies() {
    assert_eq!(delivery_outcome(&[]), Delivery::Delivered);
    for e in [
        "no WAN connectivity (will retry)",
        "no cellular service (will retry)",
        "discord: HTTP 502",
        "telegram: HTTP 429 rate limited",
        "x (failed after 3 retries)",
    ] {
        assert!(
            matches!(delivery_outcome(&[e.to_string()]), Delivery::Deferred(_)),
            "{e}"
        );
    }
    let mixed = [
        "a: HTTP 404 (permanent error)".to_string(),
        "b: HTTP 500".to_string(),
    ];
    assert!(
        matches!(delivery_outcome(&mixed), Delivery::Deferred(_)),
        "any transient holds"
    );
}

#[test]
fn hold_starts_keeps_rules_and_times_out_once() {
    let mut held = None;
    let d = |e: &str| Delivery::Deferred(e.to_string());
    let (o, a) = settle_hold(&mut held, 11, d("HTTP 502"), &[2, 1, 2], 100);
    assert_eq!((o.kind(), a), ("deferred", None));
    assert_eq!(
        held,
        Some(HeldSms {
            sms_id: 11,
            first_deferred_at: 100,
            online_secs: 0,
            delivered_rules: vec![1, 2]
        })
    );
    held.as_mut().unwrap().online_secs = HOLD_TIMEOUT_SECS - 1;
    let (o, _) = settle_hold(&mut held, 11, d("HTTP 502"), &[1, 2], 200);
    assert_eq!(o.kind(), "deferred");
    assert_eq!(
        held.as_ref().unwrap().first_deferred_at,
        100,
        "the same hold"
    );
    held.as_mut().unwrap().online_secs = HOLD_TIMEOUT_SECS;
    let (o, a) = settle_hold(&mut held, 11, d("HTTP 502"), &[1, 2], 300);
    assert!(
        matches!(&o, Delivery::Skipped(e) if e.starts_with(HOLD_TIMEOUT_NOTE)),
        "{o:?}"
    );
    assert!(a.unwrap().contains("SMS 11 not forwarded after 6h"));
    assert!(held.is_none());
    // another SMS deferred replaces the hold; delivered clears it
    let (_, _) = settle_hold(&mut held, 12, d("x"), &[], 400);
    let (_, _) = settle_hold(&mut held, 13, d("x"), &[], 500);
    assert_eq!(held.as_ref().map(|h| h.sms_id), Some(13));
    let (o, _) = settle_hold(&mut held, 13, Delivery::Delivered, &[], 600);
    assert_eq!((o, held), (Delivery::Delivered, None));
}

#[test]
fn online_clock_counts_only_online_intervals() {
    let mut c = OnlineClock::default();
    assert_eq!(c.observe(100, true), 0, "first observation counts nothing");
    assert_eq!(c.observe(160, false), 60);
    assert_eq!(c.observe(1000, true), 0, "the interval started offline");
    assert_eq!(c.observe(1010, true), 10);
}

#[test]
fn reimported_only_well_before_the_last_time() {
    assert!(is_reimported(Some(T0 - REIMPORT_MARGIN_SECS - 1), Some(T0)));
    assert!(!is_reimported(Some(T0 - REIMPORT_MARGIN_SECS), Some(T0)));
    assert!(!is_reimported(None, Some(T0)));
    assert!(!is_reimported(Some(0), None));
}
