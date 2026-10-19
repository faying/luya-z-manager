use super::*;

/// Down for 70 s on the same card, everything read, HTTPS fails at the exit.
fn base() -> Evidence {
    Evidence {
        passive_down_s: Some(70),
        expected_outage: Some(false),
        same_sim: Some(true),
        registered: Some(true),
        data_session: Some(true),
        has_ip: Some(true),
        dns: Some(Dns::Ok),
        https: Some(Check::Fail),
        clock_trusted: Some(true),
        path_bound: Some(true),
        age_s: Some(5),
    }
}

fn why(a: &Advice) -> &'static str {
    match a {
        Advice::None { why } | Advice::Unknown { why, .. } => why,
        Advice::ReadOnly { .. } => "read_only",
    }
}

#[test]
fn the_evidence_matrix() {
    type Edit = fn(&mut Evidence);
    let cases: &[(&str, Edit, &str)] = &[
        ("full evidence, exit down", |_| {}, "read_only"),
        (
            "missed reading",
            |e| e.registered = None,
            "registration_unknown",
        ),
        ("no clock / untimed", |e| e.age_s = None, "stale_or_untimed"),
        ("stale reading", |e| e.age_s = Some(31), "stale_or_untimed"),
        (
            "clock untrusted",
            |e| e.clock_trusted = None,
            "clock_untrusted",
        ),
        ("nxdomain", |e| e.dns = Some(Dns::NxDomain), "nxdomain"),
        ("another card", |e| e.same_sim = Some(false), "sim_changed"),
        ("card unreadable", |e| e.same_sim = None, "sim_unreadable"),
        (
            "expected outage",
            |e| e.expected_outage = Some(true),
            "expected_outage",
        ),
        (
            "premises unreadable",
            |e| e.expected_outage = None,
            "premises_unreadable",
        ),
        (
            "down < 60 s",
            |e| e.passive_down_s = Some(59),
            "not_down_long_enough",
        ),
        (
            "no passive samples",
            |e| e.passive_down_s = None,
            "no_passive_samples",
        ),
        (
            "path not bound",
            |e| e.path_bound = Some(false),
            "path_unbound",
        ),
        (
            "https unknown",
            |e| e.https = Some(Check::Unknown),
            "https_unknown",
        ),
        ("https ok", |e| e.https = Some(Check::Pass), "app_reachable"),
    ];
    for (name, edit, want) in cases {
        let mut e = base();
        edit(&mut e);
        let a = replay(&e);
        assert_eq!(why(&a), *want, "{name}");
        assert!(!a.executable(), "{name}");
    }
}

#[test]
fn the_first_bad_layer_is_named_and_stays_read_only() {
    let mut e = base();
    e.data_session = Some(false);
    let a = replay(&e);
    assert_eq!(
        a,
        Advice::ReadOnly {
            layer: Layer::DataSession,
            missing: ALL_MISSING.to_vec()
        }
    );
    // B31's reconnect is unverified: listed as missing, nothing executable
    if let Advice::ReadOnly { missing, .. } = &a {
        assert!(missing.contains(&Missing::B31Reconnect) && missing.contains(&Missing::O5));
    }
    let mut e = base();
    e.dns = Some(Dns::Fail);
    assert!(matches!(
        replay(&e),
        Advice::ReadOnly {
            layer: Layer::Dns,
            ..
        }
    ));
}

#[test]
fn every_unknown_says_what_is_missing() {
    let mut e = base();
    e.https = None;
    match replay(&e) {
        Advice::Unknown { missing, .. } => assert_eq!(missing.len(), 5),
        a => panic!("{a:?}"),
    }
}
