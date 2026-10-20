use super::*;

fn quiet_hour() -> Hour {
    Hour {
        covered: Some(true),
        adjacent: Some(true),
        charging: Some(false),
        screen_on: Some(false),
        no_clients: Some(true),
        low_traffic: Some(true),
    }
}

/// Three quiet hours at home, nothing running, CHILL on, the idle pause off.
fn base() -> Evidence {
    Evidence {
        fresh: Some(true),
        hours: vec![quiet_hour(), quiet_hour(), quiet_hour()],
        quiet_run: 3,
        txn_running: Some(false),
        sms_known: Some(true),
        watchdog_ok: Some(true),
        chill_on: Some(true),
        idle_pause_on: Some(false),
    }
}

fn why(a: &Advice) -> &'static str {
    match a {
        Advice::None { why } | Advice::Unknown { why } => why,
        Advice::Suggest { .. } => "suggest",
    }
}

fn kind(a: &Advice) -> &'static str {
    match a {
        Advice::None { .. } => "none",
        Advice::Unknown { .. } => "unknown",
        Advice::Suggest { .. } => "suggest",
    }
}

#[test]
fn the_evidence_matrix() {
    type Edit = fn(&mut Evidence);
    let cases: &[(&str, Edit, &str, &str)] = &[
        ("base", |_| {}, "suggest", "suggest"),
        ("old", |e| e.fresh = Some(false), "unknown", "window_stale"),
        ("no ledger", |e| e.fresh = None, "unknown", "window_stale"),
        (
            "one hour",
            |e| e.hours.truncate(1),
            "unknown",
            "window_incomplete",
        ),
        (
            "restart between",
            |e| e.hours[2].adjacent = Some(false),
            "unknown",
            "clock_gap",
        ),
        (
            "link unread",
            |e| e.hours[2].adjacent = None,
            "unknown",
            "clock_gap",
        ),
        // the oldest counted hour's own link backwards doesn't matter
        (
            "gap before the window",
            |e| e.hours[1].adjacent = Some(false),
            "suggest",
            "suggest",
        ),
        (
            "gap in coverage",
            |e| e.hours[2].covered = Some(false),
            "unknown",
            "coverage_gap",
        ),
        (
            "coverage unread",
            |e| e.hours[1].covered = None,
            "unknown",
            "coverage_unknown",
        ),
        (
            "charging",
            |e| e.hours[2].charging = Some(true),
            "none",
            "charging",
        ),
        (
            "charging unread",
            |e| e.hours[2].charging = None,
            "unknown",
            "charging_unknown",
        ),
        (
            "screen on",
            |e| e.hours[1].screen_on = Some(true),
            "none",
            "in_use",
        ),
        (
            "screen unread",
            |e| e.hours[1].screen_on = None,
            "unknown",
            "screen_unknown",
        ),
        (
            "a client",
            |e| e.hours[2].no_clients = Some(false),
            "none",
            "clients",
        ),
        (
            "roaming: no count",
            |e| e.hours[2].no_clients = None,
            "unknown",
            "clients_unknown",
        ),
        (
            "traffic",
            |e| e.hours[1].low_traffic = Some(false),
            "none",
            "traffic",
        ),
        (
            "traffic unread",
            |e| e.hours[1].low_traffic = None,
            "unknown",
            "traffic_unknown",
        ),
        // an older busy hour is outside the window
        (
            "busy before",
            |e| e.hours[0].screen_on = Some(true),
            "suggest",
            "suggest",
        ),
        (
            "txn running",
            |e| e.txn_running = Some(true),
            "none",
            "txn_running",
        ),
        (
            "txn unread",
            |e| e.txn_running = None,
            "unknown",
            "txn_unknown",
        ),
        (
            "sms stale",
            |e| e.sms_known = Some(false),
            "unknown",
            "sms_unknown",
        ),
        (
            "sms unread",
            |e| e.sms_known = None,
            "unknown",
            "sms_unknown",
        ),
        (
            "watchdog stale",
            |e| e.watchdog_ok = Some(false),
            "unknown",
            "watchdog_unknown",
        ),
        (
            "watchdog unread",
            |e| e.watchdog_ok = None,
            "unknown",
            "watchdog_unknown",
        ),
        (
            "no chill",
            |e| e.chill_on = Some(false),
            "none",
            "nothing_to_save",
        ),
        (
            "chill unread",
            |e| e.chill_on = None,
            "unknown",
            "chill_unknown",
        ),
        (
            "pause on",
            |e| e.idle_pause_on = Some(true),
            "none",
            "already_saving",
        ),
        (
            "pause unread",
            |e| e.idle_pause_on = None,
            "unknown",
            "saving_unknown",
        ),
    ];
    for (name, edit, k, want) in cases {
        let mut e = base();
        edit(&mut e);
        let a = replay(&e);
        assert_eq!((kind(&a), why(&a)), (*k, *want), "{name}");
    }
}

#[test]
fn suggests_the_idle_pause_and_never_acts() {
    let a = replay(&base());
    assert_eq!(
        a,
        Advice::Suggest {
            hours: 3,
            step: "chill_idle_pause"
        }
    );
    assert_eq!(a.action(), None);
    let mut e = base();
    e.quiet_run = 0;
    assert!(matches!(replay(&e), Advice::Suggest { hours: 2, .. }));
    assert_eq!(replay(&Evidence::default()).action(), None);
}

#[test]
fn serializes_tagged() {
    let v = serde_json::to_value(replay(&base())).unwrap();
    assert_eq!(v["advice"], "suggest");
    assert_eq!(v["step"], "chill_idle_pause");
}
