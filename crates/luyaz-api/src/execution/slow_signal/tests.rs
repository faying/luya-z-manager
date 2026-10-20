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
