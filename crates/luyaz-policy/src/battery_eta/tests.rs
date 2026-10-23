//! Battery estimate cases (D7); times are seconds on the monotonic clock.

use super::*;
use serde_json::Value;

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::ChargingEta => "charging_eta",
        Kind::DischargingEta => "discharging_eta",
        Kind::ReachedTarget => "reached_target",
        Kind::PausedAtLimit => "paused_at_limit",
        Kind::Unknown => "unknown",
    }
}

/// The shared cases from docs/battery-estimate/fixtures.json (the rules'
/// examples, formerly run by both the web and the touch screen).
#[test]
fn fixtures() {
    let v: Value = serde_json::from_str(include_str!(
        "../../../../docs/battery-estimate/fixtures.json"
    ))
    .unwrap();
    let cases = v["cases"].as_array().unwrap();
    assert!(cases.len() >= 10);
    for c in cases {
        let i = &c["input"];
        let samples: Vec<Sample> = i["samples"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| Sample {
                t: s[0].as_f64().unwrap(),
                ua: s[1].as_f64().unwrap() as i64,
                online: s[2].as_bool().unwrap(),
            })
            .collect();
        let e = estimate(&Input {
            samples: &samples,
            soc: i["soc"].as_i64().unwrap(),
            charge_full_uah: i["charge_full_uah"].as_i64(),
            charge_counter_uah: i["charge_counter_uah"].as_i64(),
            target_pct: i["target_pct"].as_i64().unwrap(),
            paused_at_limit: i["paused_at_limit"].as_bool().unwrap(),
        });
        let name = c["name"].as_str().unwrap();
        assert_eq!(
            kind_name(e.kind),
            c["expect"]["kind"].as_str().unwrap(),
            "{name}"
        );
        assert_eq!(e.minutes, c["expect"]["minutes"].as_i64(), "{name}");
    }
}

fn reading(soc: i64, ua: i64, plugged: Option<bool>) -> Reading {
    Reading {
        soc: Some(soc),
        current_ua: Some(ua),
        charge_full_uah: Some(11_000_000),
        charge_counter_uah: Some(5_500_000),
        charger_connected: plugged,
        limit_pct: 100,
        ..Default::default()
    }
}

fn feed(t: &mut Tracker, r: &Reading, now: f64, wall: u64) -> Report {
    t.feed(r, now, wall, &mut |i| estimate(i))
}

#[test]
fn states_over_time() {
    let mut eta = Tracker::new();
    assert_eq!(eta.report(0.0).state, "estimating");

    // discharging 1.1 A steadily, counter 5.5 Ah → 300 min
    for k in 0..10u64 {
        feed(
            &mut eta,
            &reading(50, -1_100_000, Some(false)),
            5.0 * k as f64,
            1000 + 5 * k,
        );
    }
    let r = eta.report(46.0);
    assert_eq!(
        (r.state, r.kind, r.minutes),
        ("ok", Kind::DischargingEta, Some(300))
    );
    assert_eq!(r.observed_at, 1045);

    // sampler stopped: same numbers, marked stale
    let r = eta.report(45.0 + 21.0);
    assert_eq!((r.state, r.minutes), ("stale", Some(300)));

    // battery unreadable
    let r = feed(&mut eta, &Reading::default(), 70.0, 1070);
    assert_eq!((r.state, r.kind), ("unavailable", Kind::Unknown));
}

#[test]
fn window_keeps_only_180s() {
    let mut eta = Tracker::new();
    let mut last = None;
    for k in 0..100u64 {
        last = Some(feed(
            &mut eta,
            &reading(50, -1_100_000, Some(false)),
            5.0 * k as f64,
            k,
        ));
    }
    // 180 s / 5 s + the one at the cut
    assert!(last.unwrap().samples <= 38);
}

#[test]
fn charge_limit_and_pause() {
    let mut eta = Tracker::new();
    let mut r = reading(80, -350_000, Some(true));
    r.limit_enabled = true;
    r.limit_pct = 80;
    r.charging_stopped = Some(true);
    let out = feed(&mut eta, &r, 0.0, 1);
    assert_eq!((out.kind, out.target_pct), (Kind::PausedAtLimit, 80));

    // a manual stop is not "at the limit"
    r.manual_override = true;
    let out = feed(&mut eta, &r, 5.0, 2);
    assert_ne!(out.kind, Kind::PausedAtLimit);

    // unplugged: never paused
    r.manual_override = false;
    r.charger_connected = Some(false);
    let out = feed(&mut eta, &r, 10.0, 3);
    assert_ne!(out.kind, Kind::PausedAtLimit);

    // an out-of-range limit counts as no limit
    r.limit_pct = 20;
    assert_eq!(feed(&mut eta, &r, 15.0, 4).target_pct, 100);
}

#[test]
fn unknown_plug_state_keeps_the_last_one() {
    let mut eta = Tracker::new();
    feed(&mut eta, &reading(50, 1_000_000, Some(true)), 0.0, 1);
    feed(&mut eta, &reading(50, 1_000_000, None), 5.0, 2);
    assert!(eta.samples().all(|s| s.online));
}
