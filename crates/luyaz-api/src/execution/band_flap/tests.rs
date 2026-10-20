use super::*;

/// Four drops on n78 over a full window, same card, every other cause read and absent.
fn base() -> Evidence {
    Evidence {
        drops: Some(4),
        band: Some("n78".into()),
        same_sim: Some(true),
        epochs_authoritative: Some(true),
        covered_s: Some(WINDOW_S),
        moving: Some(false),
        weak_sinr: Some(false),
        expected_overlap: Some(false),
        stock_recovery: Some(false),
    }
}

fn why(a: &Advice) -> &'static str {
    match a {
        Advice::None { why } | Advice::Unknown { why } => why,
        Advice::Explain { .. } => "explain",
    }
}

#[test]
fn the_evidence_matrix() {
    type Edit = fn(&mut Evidence);
    let cases: &[(&str, Edit, &str)] = &[
        ("base", |_| {}, "explain"),
        (
            "epochs guessed",
            |e| e.epochs_authoritative = Some(false),
            "epochs_not_authoritative",
        ),
        (
            "epochs unread",
            |e| e.epochs_authoritative = None,
            "epochs_not_authoritative",
        ),
        ("sim unread", |e| e.same_sim = None, "sim_unreadable"),
        ("sim swapped", |e| e.same_sim = Some(false), "sim_changed"),
        ("no history", |e| e.drops = None, "no_drop_history"),
        ("two drops", |e| e.drops = Some(2), "too_few_drops"),
        ("single redial", |e| e.drops = Some(1), "too_few_drops"),
        (
            "gap in history",
            |e| e.covered_s = Some(WINDOW_S - 1),
            "window_incomplete",
        ),
        (
            "window unknown",
            |e| e.covered_s = None,
            "window_incomplete",
        ),
        (
            "expected outage",
            |e| e.expected_overlap = Some(true),
            "expected_outage",
        ),
        (
            "expected unread",
            |e| e.expected_overlap = None,
            "expected_unknown",
        ),
        (
            "stock restart",
            |e| e.stock_recovery = Some(true),
            "stock_recovery",
        ),
        (
            "stock unread",
            |e| e.stock_recovery = None,
            "stock_recovery_unknown",
        ),
        ("moving", |e| e.moving = Some(true), "moving"),
        ("movement unread", |e| e.moving = None, "movement_unknown"),
        ("weak sinr", |e| e.weak_sinr = Some(true), "weak_sinr"),
        ("sinr unread", |e| e.weak_sinr = None, "sinr_unknown"),
        ("bands disagree", |e| e.band = None, "band_mixed_or_unknown"),
    ];
    for (name, edit, want) in cases {
        let mut e = base();
        edit(&mut e);
        assert_eq!(why(&replay(&e)), *want, "{name}");
    }
}

#[test]
fn explains_band_and_count_and_never_acts() {
    let a = replay(&base());
    assert_eq!(
        a,
        Advice::Explain {
            band: "n78".into(),
            drops: 4
        }
    );
    assert_eq!(a.action(), None);
    let mut e = base();
    e.drops = Some(MIN_DROPS);
    assert!(matches!(replay(&e), Advice::Explain { drops: 3, .. }));
}

#[test]
fn serializes_tagged() {
    let v = serde_json::to_value(replay(&base())).unwrap();
    assert_eq!(v["advice"], "explain");
    assert_eq!(v["band"], "n78");
}
