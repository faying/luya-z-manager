use super::*;

fn read(q: Option<i64>) -> Read {
    Read {
        q,
        v: Some(4_000_000),
        charging_known: Some(false),
        screen_known: Some(false),
        clients: Some(0),
        ..Default::default()
    }
}
fn clock(up: i64, mono: i64) -> Clock {
    Clock {
        boot: "boot-a".into(),
        boottime: up,
        mono,
        ..Default::default()
    }
}
fn sample(a: &mut Acc, q: Option<i64>, up: i64, mono: i64) {
    a.observe(&read(q), &clock(up, mono), Some(false));
}
fn cell(a: &Acc) -> Value {
    a.value()["cells"]["off_home"].clone()
}

#[test]
fn counter_units_zero_and_sleep_use_actual_boottime() {
    let mut a = Acc::default();
    sample(&mut a, Some(10_000), 0, 0);
    sample(&mut a, Some(9_000), 600, 60);
    assert_eq!(cell(&a), json!({"observed_s":600,"uwh":4000.0}));
    assert_eq!(a.value()["sleep_observed_s"], 540);
    assert_eq!(a.value()["max_discharge_ua"], 6000.0);
    sample(&mut a, Some(9_000), 660, 120);
    assert_eq!(cell(&a)["observed_s"], 660);
    assert!(Acc::default().value()["cells"]["off_home"]["uwh"].is_null());
}

#[test]
fn increase_and_extreme_reset_reject_without_overflow_or_erasing_valid_totals() {
    let mut a = Acc::default();
    sample(&mut a, Some(1000), 0, 0);
    sample(&mut a, Some(900), 60, 60);
    sample(&mut a, Some(2000), 120, 120);
    sample(&mut a, Some(i64::MIN), 180, 180);
    assert_eq!(cell(&a), json!({"observed_s":60,"uwh":400.0}));
    assert_eq!(a.value()["rejected"]["counter_increase"], 1);
    assert_eq!(a.value()["rejected"]["counter_arithmetic_overflow"], 1);
    assert_eq!(a.value()["rejected_s"], 120);
    // The legacy path must also survive malformed extreme values in debug/release.
    let mut legacy = super::super::power::Acc::default();
    legacy.add(&read(Some(i64::MIN)), Some(i64::MAX), 60, 60, false, 60);
    let mut r = read(None);
    r.i = Some(i64::MIN);
    legacy.add(&r, None, 60, 60, false, 120);
}

#[test]
fn missing_counter_does_not_bridge_even_when_vi_is_available() {
    let mut a = Acc::default();
    sample(&mut a, Some(1000), 0, 0);
    sample(&mut a, None, 60, 60);
    sample(&mut a, Some(800), 120, 120);
    assert!(cell(&a)["uwh"].is_null());
    sample(&mut a, Some(700), 180, 180);
    assert_eq!(cell(&a)["observed_s"], 60);
    assert_eq!(a.value()["rejected"]["counter_missing"], 2);
}

#[test]
fn charge_unknown_conflict_and_transitions_do_not_become_discharge() {
    for (status, i, expected) in [
        ("", Some(-100), None),
        ("Unknown", Some(0), None),
        ("Discharging", Some(1), None),
        ("Charging", Some(-1), None),
        ("Full", Some(0), Some(true)),
        ("Charging", None, Some(true)),
        ("Discharging", None, Some(false)),
        ("Not charging", Some(-1), Some(false)),
    ] {
        assert_eq!(charging(status, i), expected);
    }
    let mut a = Acc::default();
    sample(&mut a, Some(1000), 0, 0);
    for (n, state) in [None, Some(false), Some(true), Some(true), Some(false)]
        .into_iter()
        .enumerate()
    {
        let mut r = read(Some(900 - n as i64 * 100));
        r.charging_known = state;
        a.observe(
            &r,
            &clock((n as i64 + 1) * 60, (n as i64 + 1) * 60),
            Some(false),
        );
    }
    assert_eq!(a.value()["rejected"]["charging_unknown"], 2);
    assert_eq!(a.value()["rejected"]["charging_transition"], 2);
    assert_eq!(a.value()["rejected"]["charging"], 1);
    assert!(cell(&a)["uwh"].is_null());
}

#[test]
fn gaps_invalid_clocks_and_boot_change_never_bridge() {
    let mut a = Acc::default();
    sample(&mut a, Some(1000), 0, 0);
    sample(&mut a, Some(900), 180, 180);
    assert_eq!(a.value()["rejected"]["awake_gap"], 1);
    sample(&mut a, Some(800), 240, 240);
    assert_eq!(cell(&a)["observed_s"], 60);
    a.observe(
        &read(Some(700)),
        &Clock {
            boot: "boot-b".into(),
            ..clock(300, 300)
        },
        Some(false),
    );
    assert_eq!(cell(&a)["observed_s"], 60);
    sample(&mut a, Some(600), 360, 400); // impossible MONOTONIC > BOOTTIME
    sample(&mut a, Some(500), 420, 420); // baseline only
    assert_eq!(cell(&a)["observed_s"], 60);
    assert_eq!(a.value()["rejected"]["invalid_clock"], 1);
}

#[test]
fn scenario_missing_and_changes_are_excluded_and_cells_remain_separate() {
    for mutate in 0..6 {
        let mut a = Acc::default();
        sample(&mut a, Some(1000), 0, 0);
        let mut r = read(Some(900));
        let mut abroad = Some(false);
        match mutate {
            0 => r.screen_known = None,
            1 => r.screen_known = Some(true),
            2 => abroad = None,
            3 => abroad = Some(true),
            4 => r.clients = None,
            _ => r.clients = Some(1),
        }
        a.observe(&r, &clock(60, 60), abroad);
        assert!(cell(&a)["uwh"].is_null(), "case {mutate}");
        assert_eq!(a.value()["rejected_s"], 60);
    }
    let mut a = Acc::default();
    let mut r = read(Some(1000));
    r.screen_known = Some(true);
    a.observe(&r, &clock(0, 0), Some(true));
    r.q = Some(900);
    a.observe(&r, &clock(60, 60), Some(true));
    assert_eq!(a.value()["cells"]["on_abroad"]["uwh"], 400.0);
    assert!(cell(&a)["uwh"].is_null());
}

#[test]
fn invalid_voltage_and_downward_jump_keep_evidence_without_invented_hardware_limit() {
    let mut a = Acc::default();
    sample(&mut a, Some(1_000_000), 0, 0);
    let mut r = read(Some(900_000));
    r.v = Some(0);
    a.observe(&r, &clock(60, 60), Some(false));
    sample(&mut a, Some(0), 120, 120);
    assert_eq!(cell(&a)["observed_s"], 60);
    assert_eq!(a.value()["max_discharge_ua"], 54_000_000.0);
    assert_eq!(a.value()["max_interval"]["q0_uah"], 900_000);
    assert_eq!(a.value()["rejected"]["voltage_missing_or_invalid"], 1);
    assert_eq!(
        a.value()["jump_check"],
        "requires_independently_validated_discharge_bound"
    );
}

#[test]
fn saved_totals_survive_but_restart_and_old_state_require_new_baseline() {
    let mut a = Acc::default();
    sample(&mut a, Some(1000), 0, 0);
    sample(&mut a, Some(900), 60, 60);
    let raw = serde_json::to_value(&a).unwrap();
    assert!(raw.get("prev").is_none());
    let mut restored: Acc = serde_json::from_value(raw).unwrap();
    sample(&mut restored, Some(100), 120, 120);
    assert_eq!(cell(&restored)["uwh"], 400.0);
    sample(&mut restored, Some(0), 180, 180);
    assert_eq!(cell(&restored)["uwh"], 800.0);
    let legacy: super::super::power::Acc = serde_json::from_str("{}").unwrap();
    assert_eq!(legacy.power()["counter_quality"]["samples"], 0);
}

#[test]
fn fold_routes_quality_to_existing_hour_power_and_resumes_without_bridging() {
    use crate::longterm::{Fold, Out};
    let mut fold = Fold::new("fixture");
    let mut out = Out::default();
    for (up, q) in [(3480, 1000), (3540, 900), (3600, 800), (3660, 700)] {
        let c = clock(up, up);
        fold.tick(&c, &mut out);
        fold.seen.serving_mcc = Some(310);
        fold.seen.home_mcc = Some(310);
        fold.power_with(&c, &read(Some(q)));
    }
    assert_eq!(out.hours.len(), 3, "no new record or write schedule");
    let q = &out.hours.iter().find(|r| r["k"] == "hour_power").unwrap()["counter_quality"];
    assert_eq!(q["cells"]["off_home"]["observed_s"], 60);
    assert_eq!(q["cells"]["off_home"]["uwh"], 400.0);
    assert!(serde_json::to_string(q).unwrap().len() < 2048);
    let mut restored = Fold::resume(&fold.save(), "fixture", "boot-a").unwrap();
    let c = clock(3720, 3720);
    restored.tick(&c, &mut Out::default());
    restored.power_with(&c, &read(Some(10)));
    let rows = Fold::close_old(&restored.save(), "next-boot");
    let q = &rows.iter().find(|r| r["k"] == "hour_power").unwrap()["counter_quality"];
    assert_eq!(q["cells"]["off_home"]["observed_s"], 60);
    assert_eq!(q["cells"]["off_home"]["uwh"], 400.0);
    let mut old: Value = serde_json::from_str(&fold.save()).unwrap();
    old["hour"]["pw"].as_object_mut().unwrap().remove("energy");
    let mut legacy = Fold::resume(&old.to_string(), "fixture", "boot-a").unwrap();
    legacy.tick(&c, &mut Out::default());
    legacy.power_with(&c, &read(Some(10)));
    let rows = Fold::close_old(&legacy.save(), "next-boot");
    assert!(rows.iter().find(|r| r["k"] == "hour_power").unwrap()["counter_quality"]["cells"]["off_home"]["uwh"].is_null());
}
