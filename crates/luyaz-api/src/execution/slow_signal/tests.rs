use super::*;

/// A fresh check on this cell: signal fine, Wi-Fi bad, cell load warn, the rest fine.
fn base() -> Evidence {
    let l = |id: &str, v| (id.to_string(), v);
    Evidence {
        done: Some(true),
        age_s: Some(30),
        same_cell: Some(true),
        layers: vec![
            l("wifi", Level::Bad),
            l("sim", Level::Ok),
            l("signal", Level::Ok),
            l("limit", Level::Ok),
            l("bearer", Level::Ok),
            l("link", Level::Ok),
            l("dns", Level::Ok),
            l("e2e", Level::Ok),
            l("clock", Level::Bad),
            l("crowd", Level::Warn),
            l("proxy", Level::Na),
        ],
        busy_s: None,
    }
}

fn set(e: &mut Evidence, id: &str, v: Level) {
    for (l, x) in &mut e.layers {
        if l == id {
            *x = v;
        }
    }
}

fn why(a: &Advice) -> &'static str {
    match a {
        Advice::None { why } | Advice::Unknown { why } => why,
        Advice::Explain { .. } => "explain",
        Advice::Suggest { .. } => "suggest",
    }
}

#[test]
fn the_evidence_matrix() {
    type Edit = fn(&mut Evidence);
    let cases: &[(&str, Edit, &str)] = &[
        ("base", |_| {}, "explain"),
        ("no check", |e| e.done = None, "no_check"),
        ("running", |e| e.done = Some(false), "check_running"),
        ("old", |e| e.age_s = Some(MAX_AGE_S + 1), "check_stale"),
        ("at the limit", |e| e.age_s = Some(MAX_AGE_S), "explain"),
        ("age unread", |e| e.age_s = None, "check_stale"),
        ("moved", |e| e.same_cell = Some(false), "cell_changed"),
        ("cell unread", |e| e.same_cell = None, "cell_unknown"),
        (
            "weak",
            |e| set(e, "signal", Level::Bad),
            "signal_not_strong",
        ),
        (
            "noisy",
            |e| set(e, "signal", Level::Warn),
            "signal_not_strong",
        ),
        (
            "signal na",
            |e| set(e, "signal", Level::Na),
            "signal_unknown",
        ),
        (
            "signal missing",
            |e| e.layers.retain(|(l, _)| l != "signal"),
            "signal_unknown",
        ),
        (
            "nothing found",
            |e| {
                set(e, "wifi", Level::Ok);
                set(e, "crowd", Level::Ok);
            },
            "no_layer_explains",
        ),
    ];
    for (name, edit, want) in cases {
        let mut e = base();
        edit(&mut e);
        assert_eq!(why(&replay(&e)), *want, "{name}");
    }
}

#[test]
fn proven_and_possible_and_never_acts() {
    let a = replay(&base());
    // the clock layer is not a slowness cause; Na, Blocked and Info never count
    assert_eq!(
        a,
        Advice::Explain {
            proven: vec!["wifi"],
            possible: vec!["crowd"]
        }
    );
    assert_eq!(a.action(), None);
    // cell load is never proven, even when the check says bad
    let mut e = base();
    set(&mut e, "crowd", Level::Bad);
    set(&mut e, "dns", Level::Warn);
    set(&mut e, "proxy", Level::Bad);
    set(&mut e, "e2e", Level::Blocked);
    assert_eq!(
        replay(&e),
        Advice::Explain {
            proven: vec!["wifi", "proxy"],
            possible: vec!["dns", "crowd"]
        }
    );
    assert_eq!(replay(&Evidence::default()).action(), None);
}

#[test]
fn serializes_tagged() {
    let v = serde_json::to_value(replay(&base())).unwrap();
    assert_eq!(v["advice"], "explain");
    assert_eq!(v["proven"][0], "wifi");
}

#[test]
fn busy_suggests_a_check_only_where_there_is_none_to_read() {
    type Edit = fn(&mut Evidence);
    let cases: &[(&str, Edit, &str)] = &[
        ("no check", |e| e.done = None, "suggest"),
        ("old", |e| e.age_s = Some(MAX_AGE_S + 1), "suggest"),
        ("moved", |e| e.same_cell = Some(false), "suggest"),
        ("cell unread", |e| e.same_cell = None, "suggest"),
        ("running", |e| e.done = Some(false), "check_running"),
        ("a fresh check speaks", |_| {}, "explain"),
        (
            "nothing explains",
            |e| {
                for id in ["wifi", "crowd"] {
                    set(e, id, Level::Ok)
                }
            },
            "no_layer_explains",
        ),
        (
            "too short",
            |e| {
                e.done = None;
                e.busy_s = Some(BUSY_S - 1)
            },
            "no_check",
        ),
    ];
    for (name, edit, want) in cases {
        let mut e = base();
        e.busy_s = Some(BUSY_S);
        edit(&mut e);
        assert_eq!(why(&replay(&e)), *want, "{name}");
    }
}

#[test]
fn busy_runs_while_strong_and_moving() {
    let mut b = Busy::default();
    let fast = Some(BUSY_MIN_BPS);
    for t in (0..=60).step_by(5) {
        b.observe(t, Some(Level::Ok), fast);
    }
    assert_eq!(b.seconds(60), Some(60));
    assert_eq!(b.seconds(60 + BUSY_GAP_S + 1), None, "stopped being fed");
    // a gap starts over
    b.observe(100, Some(Level::Ok), fast);
    assert_eq!(b.seconds(100), Some(0));
    // weak, slow traffic, or unread: over
    for (sig, rx) in [
        (Some(Level::Warn), fast),
        (Some(Level::Ok), Some(BUSY_MIN_BPS - 1)),
        (None, fast),
        (Some(Level::Ok), None),
    ] {
        let mut b = Busy::default();
        b.observe(0, Some(Level::Ok), fast);
        b.observe(5, sig, rx);
        assert_eq!(b.seconds(5), None);
    }
}

#[test]
fn the_suggestion_never_acts() {
    let a = Advice::Suggest {
        busy_s: 90,
        step: "deep_check",
    };
    assert_eq!(a.action(), None);
}
