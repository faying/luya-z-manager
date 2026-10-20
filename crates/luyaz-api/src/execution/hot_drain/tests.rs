use super::*;

/// A screen-off hour at home: 41.0 °C against a normal 36.0, 900 mW against 400, three matched
/// hours each, every factor read and absent.
fn base() -> Evidence {
    Evidence {
        fresh: Some(true),
        window_s: Some(3600),
        charging: Some(false),
        batt_dc: Some(410),
        batt_normal_dc: Some(360),
        temp_hours: MIN_HOURS,
        drain_mw: Some(900),
        drain_normal_mw: Some(400),
        drain_hours: MIN_HOURS,
        factors: Factors {
            clients: Some(false),
            cellular: Some(false),
            cpu: Some(false),
            throttled: Some(false),
            chip_hot: Some(false),
            battery_health: Some(false),
        },
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
        ("old window", |e| e.fresh = Some(false), "window_stale"),
        ("no ledger", |e| e.fresh = None, "window_stale"),
        (
            "mixed hour",
            |e| e.window_s = Some(MIN_WINDOW_S - 1),
            "window_incomplete",
        ),
        ("window unread", |e| e.window_s = None, "window_incomplete"),
        ("charging", |e| e.charging = Some(true), "charging"),
        ("charging unread", |e| e.charging = None, "charging_unknown"),
        ("no sensor", |e| e.batt_dc = None, "no_temperature"),
        (
            "no temp baseline",
            |e| e.batt_normal_dc = None,
            "no_temperature_baseline",
        ),
        (
            "two temp hours",
            |e| e.temp_hours = MIN_HOURS - 1,
            "no_temperature_baseline",
        ),
        (
            "just under margin",
            |e| e.batt_dc = Some(360 + HOT_MARGIN_DC - 1),
            "not_hot",
        ),
        (
            "drain unscreened",
            |e| e.drain_mw = None,
            "drain_unqualified",
        ),
        (
            "no drain baseline",
            |e| e.drain_normal_mw = None,
            "no_drain_baseline",
        ),
        (
            "two drain hours",
            |e| e.drain_hours = MIN_HOURS - 1,
            "no_drain_baseline",
        ),
        ("drain normal", |e| e.drain_mw = Some(599), "drain_normal"),
        ("drain at ratio", |e| e.drain_mw = Some(600), "explain"),
        (
            "at margin",
            |e| e.batt_dc = Some(360 + HOT_MARGIN_DC),
            "explain",
        ),
        ("factor unread", |e| e.factors.cpu = None, "explain"),
    ];
    for (name, edit, want) in cases {
        let mut e = base();
        edit(&mut e);
        assert_eq!(why(&replay(&e)), *want, "{name}");
    }
}

#[test]
fn lists_factors_alongside_and_unread_and_never_acts() {
    let mut e = base();
    e.factors.clients = Some(true);
    e.factors.chip_hot = Some(true);
    e.factors.cellular = None;
    let a = replay(&e);
    assert_eq!(
        a,
        Advice::Explain {
            batt_dc: 410,
            batt_normal_dc: 360,
            drain_mw: 900,
            drain_normal_mw: 400,
            alongside: vec!["clients", "chip_hot"],
            unread: vec!["cellular"],
            ambient: "unread",
        }
    );
    assert_eq!(a.action(), None);
    assert_eq!(replay(&Evidence::default()).action(), None);
}

#[test]
fn serializes_tagged() {
    let v = serde_json::to_value(replay(&base())).unwrap();
    assert_eq!(v["advice"], "explain");
    assert_eq!(v["batt_dc"], 410);
    assert_eq!(v["ambient"], "unread");
    let v = serde_json::to_value(replay(&Evidence::default())).unwrap();
    assert_eq!(v["advice"], "unknown");
    assert_eq!(v["why"], "window_stale");
}
