use super::hot_drain::*;
use luyaz_api::execution::hot_drain::{Advice, replay};
use serde_json::{Value, json};

/// One closed hour screen off at home: `batt` tenths of °C, `q_s` screened seconds at `mw`,
/// `cli_s` with clients, `cpu` %, `chg` charging seconds, `usb` the `hour` line's power seconds.
#[allow(clippy::too_many_arguments)]
fn hour(
    boot: &str,
    n: i64,
    batt: i64,
    q_s: i64,
    mw: f64,
    cli_s: i64,
    cpu: f64,
    chg: i64,
    usb: i64,
) -> Vec<String> {
    let (from, to) = (n * 3600, n * 3600 + 3600);
    let cell = |s: i64, u: Option<f64>| json!({"observed_s": s, "uwh": u});
    let pw = json!({
        "k": "hour_power", "boot": boot, "from": from, "to": to,
        "on_home_s": 0, "on_abroad_s": 0, "off_home_s": 3600 - chg, "off_abroad_s": 0,
        "chg_s": chg, "batt_tmax": batt, "zone_tmax": 45000, "throttle_s": 0,
        "off_home_cli_s": cli_s, "off_home_wan_pkts": 1000, "off_abroad_wan_pkts": null,
        "e": 99999.0,
        "counter_quality": {"v": 1, "cells": {
            "on_home": cell(0, None), "on_abroad": cell(0, None),
            "off_home": cell(q_s, (q_s > 0).then(|| mw * q_s as f64 / 3.6)),
            "off_abroad": cell(0, None),
        }},
    });
    let h = json!({"k": "hour", "boot": boot, "from": from, "to": to, "chg_s": usb});
    let pr = json!({"k": "hour_proc", "boot": boot, "from": from, "to": to,
        "activity": {"system_cpu": {"all_core_percent": cpu}}});
    vec![h.to_string(), pw.to_string(), pr.to_string()]
}

/// Five quiet nights at 30.0 °C, 400 mW, then a hot hour at 41.0 °C, 900 mW, 3 % → 30 % CPU.
fn ledger(last: Vec<String>) -> Vec<Row> {
    let mut l: Vec<String> = (0..5)
        .flat_map(|n| hour("old", n, 300, 3600, 400.0, 0, 3.0, 0, 0))
        .collect();
    l.push("{\"k\":\"boot\"}".into());
    l.push("not json".into());
    l.extend(last);
    rows(l.iter())
}

fn hot() -> Vec<String> {
    hour("0c178ee6", 10, 410, 3600, 900.0, 0, 30.0, 0, 0)
}

const NOW: i64 = 11 * 3600 + 600;

#[test]
fn a_hot_fast_hour_against_quiet_nights_explains() {
    let r = ledger(hot());
    assert_eq!(r.len(), 6);
    let (e, w) = evidence(&r, "0c178ee6", NOW, Some("Good\n"));
    assert_eq!(e.batt_normal_dc, Some(300));
    assert_eq!(e.temp_hours, 5);
    assert_eq!(e.drain_mw, Some(900));
    assert_eq!(e.drain_normal_mw, Some(400));
    assert_eq!(e.drain_hours, 5);
    assert_eq!(w["cell"], "off_home");
    assert_eq!(w["age_s"], 600);
    let f = &e.factors;
    assert_eq!(
        (
            f.clients,
            f.cpu,
            f.cellular,
            f.chip_hot,
            f.throttled,
            f.battery_health
        ),
        (
            Some(false),
            Some(true),
            Some(false),
            Some(false),
            Some(false),
            Some(false)
        )
    );
    match replay(&e) {
        Advice::Explain { alongside, .. } => assert_eq!(alongside, vec!["cpu"]),
        a => panic!("{a:?}"),
    }
}

#[test]
fn units_tenths_and_milli_degrees() {
    let r = ledger(hot());
    let w = r.last().unwrap();
    assert_eq!(w.batt_dc, Some(410));
    assert_eq!(w.zone_mc, Some(45000));
}

#[test]
fn never_the_unscreened_hour_energy() {
    // screened only 20 min: unknown even though `e` says a lot
    let r = ledger(hour("0c178ee6", 10, 410, 1200, 900.0, 0, 3.0, 0, 0));
    let (e, _) = evidence(&r, "0c178ee6", NOW, Some("Good"));
    assert_eq!(e.drain_mw, None);
    assert_eq!(
        replay(&e),
        Advice::Unknown {
            why: "drain_unqualified"
        }
    );
}

#[test]
fn the_battery_blocks_charging_flag_is_not_charging() {
    // the `hour` line says charging all hour, the fuel gauge says discharging
    let r = ledger(hour("0c178ee6", 10, 410, 3600, 900.0, 0, 3.0, 0, 3000));
    let (e, _) = evidence(&r, "0c178ee6", NOW, None);
    assert_eq!(e.charging, Some(false));
    assert!(matches!(replay(&e), Advice::Explain { .. }));
}

#[test]
fn charging_is_not_this_scenario_and_not_a_baseline() {
    let r = ledger(hour("0c178ee6", 10, 410, 3600, 900.0, 0, 3.0, 1000, 0));
    let (e, _) = evidence(&r, "0c178ee6", NOW, None);
    assert_eq!(e.charging, Some(true));
    assert_eq!(replay(&e), Advice::None { why: "charging" });
    // charging nights don't count as normal
    let mut l: Vec<String> = (0..5)
        .flat_map(|n| hour("old", n, 300, 3600, 400.0, 0, 3.0, 100, 100))
        .collect();
    l.extend(hot());
    let (e, _) = evidence(&rows(l.iter()), "0c178ee6", NOW, None);
    assert_eq!(e.temp_hours, 0);
    assert_eq!(
        replay(&e),
        Advice::Unknown {
            why: "no_temperature_baseline"
        }
    );
}

#[test]
fn stale_or_other_boot_window_is_unknown() {
    let r = ledger(hot());
    for (boot, now) in [
        ("ffffffff", NOW),
        ("0c178ee6", 10 * 3600 + 3600 + FRESH_S + 1),
    ] {
        let (e, _) = evidence(&r, boot, now, None);
        assert_eq!(e.fresh, Some(false));
        assert_eq!(
            replay(&e),
            Advice::Unknown {
                why: "window_stale"
            }
        );
    }
    let (e, w) = evidence(&[], "0c178ee6", NOW, None);
    assert_eq!(e.fresh, None);
    assert_eq!(w, Value::Null);
}

#[test]
fn clients_and_battery_health_factors() {
    let r = ledger(hour("0c178ee6", 10, 410, 3600, 900.0, 1200, 3.0, 0, 0));
    let (e, _) = evidence(&r, "0c178ee6", NOW, Some("Overheat"));
    assert_eq!(e.factors.clients, Some(true));
    assert_eq!(e.factors.battery_health, Some(true));
    assert_eq!(e.factors.cpu, Some(false));
}

#[test]
fn too_few_matched_hours_leave_the_comparisons_unread() {
    let mut l: Vec<String> = (0..2)
        .flat_map(|n| hour("old", n, 300, 3600, 400.0, 0, 3.0, 0, 0))
        .collect();
    l.extend(hot());
    let (e, _) = evidence(&rows(l.iter()), "0c178ee6", NOW, None);
    assert_eq!(e.batt_normal_dc, None);
    assert_eq!(e.factors.cpu, None);
    assert_eq!(e.factors.chip_hot, None);
    assert_eq!(e.factors.battery_health, None);
}

#[test]
fn keeps_the_last_thirty_days() {
    let l: Vec<String> = (0..(KEEP as i64 + 5))
        .flat_map(|n| hour("old", n, 300, 0, 0.0, 0, 3.0, 0, 0))
        .collect();
    let r = rows(l.iter());
    assert_eq!(r.len(), KEEP);
    assert_eq!(r[0].from, 5 * 3600);
}
