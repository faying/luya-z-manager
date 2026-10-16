//! The built-in path, the fallback when a component does not load, and (with a component built
//! for this machine, `LUYAZ_TEST_PLUGINS=<dir>`: crates/luyaz-plugins/build.sh test) the component
//! against the built-in on the shared fixtures and the agent's tracker cases.

use super::*;
use crate::plugin::Status;
use serde_json::Value;
use sha2::Digest;

fn empty_dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("u60d-beta-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
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

#[test]
fn builtin_without_a_component() {
    let mut g = load(&empty_dir("none"));
    assert_eq!(g.slot.status, Status::Builtin { why: String::new() });
    for k in 0..10u64 {
        feed(
            &mut g,
            &reading(50, -1_100_000, Some(false)),
            5.0 * k as f64,
            1000 + 5 * k,
        );
    }
    let r = g.tracker.report(46.0);
    assert_eq!(
        (r.state, r.kind, r.minutes),
        ("ok", Kind::DischargingEta, Some(300))
    );
    assert_eq!((g.slot.calls, g.slot.fallbacks), (10, 0));
}

#[test]
fn a_component_that_does_not_deserialize_leaves_the_builtin() {
    let d = empty_dir("garbage");
    let bytes = b"not a cwasm";
    std::fs::write(d.join("b.cwasm"), bytes).unwrap();
    let sha: String = sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let m = json!({"plugins": [{"name": NAME, "abi": ABI, "file": "b.cwasm", "sha256": sha}]});
    std::fs::write(d.join("plugins.json"), m.to_string()).unwrap();
    let g = load(&d);
    match &g.slot.status {
        Status::Builtin { why } => assert!(why.starts_with("deserialize"), "{why}"),
        s => panic!("{s:?}"),
    }
    assert!(g.slot.component.is_none());
}

/// The component built for this machine, when the test run has one.
fn component() -> Option<State> {
    let Some(dir) = std::env::var_os("LUYAZ_TEST_PLUGINS") else {
        assert!(
            std::env::var_os("LUYAZ_REQUIRE_PLUGINS").is_none(),
            "LUYAZ_REQUIRE_PLUGINS set without LUYAZ_TEST_PLUGINS"
        );
        eprintln!("no LUYAZ_TEST_PLUGINS: component parity not run");
        return None;
    };
    let g = load(std::path::Path::new(&dir));
    assert!(
        matches!(g.slot.status, Status::Component { .. }),
        "{:?}",
        g.slot.status
    );
    Some(g)
}

#[test]
fn component_matches_the_builtin_on_the_fixtures() {
    let Some(g) = component() else { return };
    let c = g.slot.component.as_ref().unwrap();
    let v: Value = serde_json::from_str(include_str!(
        "../../../../../docs/battery-estimate/fixtures.json"
    ))
    .unwrap();
    let cases = v["cases"].as_array().unwrap();
    assert!(cases.len() >= 10);
    for case in cases {
        let i = &case["input"];
        let samples: Vec<p::Sample> = i["samples"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| p::Sample {
                t: s[0].as_f64().unwrap(),
                ua: s[1].as_f64().unwrap() as i64,
                online: s[2].as_bool().unwrap(),
            })
            .collect();
        let input = Input {
            samples: &samples,
            soc: i["soc"].as_i64().unwrap(),
            charge_full_uah: i["charge_full_uah"].as_i64(),
            charge_counter_uah: i["charge_counter_uah"].as_i64(),
            target_pct: i["target_pct"].as_i64().unwrap(),
            paused_at_limit: i["paused_at_limit"].as_bool().unwrap(),
        };
        assert_eq!(
            c.call(&input).unwrap(),
            p::estimate(&input),
            "{}",
            case["name"]
        );
    }
}

#[test]
fn component_runs_the_agent_tracker_cases() {
    let Some(mut g) = component() else { return };
    // states_over_time
    for k in 0..10u64 {
        feed(
            &mut g,
            &reading(50, -1_100_000, Some(false)),
            5.0 * k as f64,
            1000 + 5 * k,
        );
    }
    let r = g.tracker.report(46.0);
    assert_eq!(
        (r.state, r.kind, r.minutes, r.observed_at),
        ("ok", Kind::DischargingEta, Some(300), 1045)
    );
    // charge_limit_and_pause
    let mut r = reading(80, -350_000, Some(true));
    r.limit_enabled = true;
    r.limit_pct = 80;
    r.charging_stopped = Some(true);
    let out = feed(&mut g, &r, 60.0, 1);
    assert_eq!((out.kind, out.target_pct), (Kind::PausedAtLimit, 80));
    // charging toward the limit
    let mut g2 = component().unwrap();
    for k in 0..10u64 {
        feed(
            &mut g2,
            &reading(40, 2_000_000, Some(true)),
            5.0 * k as f64,
            k,
        );
    }
    let mut want = Tracker::new();
    for k in 0..10u64 {
        want.feed(
            &reading(40, 2_000_000, Some(true)),
            5.0 * k as f64,
            k,
            &mut |i| p::estimate(i),
        );
    }
    assert_eq!(g2.tracker.report(46.0), want.report(46.0));
    assert!(matches!(g.slot.status, Status::Component { .. }));
    assert_eq!(g.slot.fallbacks + g2.slot.fallbacks, 0);
}

/// A component that never returns (crates/luyaz-plugins/hostile-eta, `LUYAZ_TEST_HOSTILE`): the call
/// runs out of fuel, the tick still gets the built-in's answer, and the component stays off.
#[test]
fn a_component_that_fails_a_call_is_dropped_for_the_builtin() {
    let Some(dir) = std::env::var_os("LUYAZ_TEST_HOSTILE") else {
        assert!(
            std::env::var_os("LUYAZ_REQUIRE_PLUGINS").is_none(),
            "LUYAZ_REQUIRE_PLUGINS set without LUYAZ_TEST_HOSTILE"
        );
        return;
    };
    let mut g = load(std::path::Path::new(&dir));
    assert!(
        matches!(g.slot.status, Status::Component { .. }),
        "{:?}",
        g.slot.status
    );
    let t = Instant::now();
    let out = feed(&mut g, &reading(50, -1_100_000, Some(false)), 0.0, 1);
    // fuel for about 10 ms on the device; generous for a slow test machine
    assert!(t.elapsed().as_millis() < 1000, "{:?}", t.elapsed());
    let mut want = Tracker::new();
    let w = want.feed(&reading(50, -1_100_000, Some(false)), 0.0, 1, &mut |i| {
        p::estimate(i)
    });
    assert_eq!(out, w);
    match &g.slot.status {
        Status::Builtin { why } => assert!(why.starts_with("call failed: trap"), "{why}"),
        s => panic!("{s:?}"),
    }
    assert!(g.slot.component.is_none());
    feed(&mut g, &reading(50, -1_100_000, Some(false)), 5.0, 2);
    assert_eq!((g.slot.calls, g.slot.fallbacks), (2, 1));
}

/// The ABI u60d checks manifests against is the wit file's package line (build.sh takes it from
/// there too).
#[test]
fn abi_is_the_wit_package() {
    let wit = include_str!("../../../../luyaz-policy/wit/battery-eta.wit");
    let line = wit.lines().find(|l| l.starts_with("package ")).unwrap();
    assert_eq!(line, format!("package {ABI};"));
}
