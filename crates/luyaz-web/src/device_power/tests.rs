use super::*;

fn charge(v: Value) -> Got {
    let mut g = Got::default();
    g.charge.put(Ok(v));
    g
}

#[test]
fn flags_read_the_firmware_strings() {
    assert_eq!(flag(&json!("1")), Some(true));
    assert_eq!(flag(&json!("0")), Some(false));
    assert_eq!(flag(&json!(1)), Some(true));
    assert_eq!(flag(&json!(null)), None);
    assert_eq!(flag(&json!("yes")), None);
}

#[test]
fn the_draft_must_be_in_the_agents_range() {
    let f = |on: Value, l: Value, h: Value| ChargeIn { on, limit: l, hyst: h };
    assert_eq!(
        check(&f(json!(true), json!(80), json!(5))),
        Some(Draft { on: true, limit: 80, hyst: 5 })
    );
    assert!(check(&f(json!(true), json!("90"), json!("20"))).is_some(), "strings are read too");
    assert!(check(&f(json!(true), json!(49), json!(5))).is_none());
    assert!(check(&f(json!(true), json!(101), json!(5))).is_none());
    assert!(check(&f(json!(true), json!(80), json!(0))).is_none());
    assert!(check(&f(json!(true), json!(80), json!(21))).is_none());
    assert!(check(&f(json!(null), json!(80), json!(5))).is_none());
    assert!(check(&ChargeIn::default()).is_none());
}

#[test]
fn the_status_says_what_the_device_says() {
    let g = charge(json!({"capacity":62,"battery_status":"Charging","charge_limit_enabled":true,
        "charge_limit":80,"hysteresis":5,"charging_stopped":false}));
    let (tone, state, reason, meta, retry) = status(&g, Lang::En);
    assert_eq!((tone, state.as_str()), ("ok", "Battery 62%"));
    assert_eq!(reason.as_deref(), Some("Status: Charging · Charge limit 80%"));
    assert!(meta.is_none() && !retry);
    let stopped = charge(json!({"capacity":80,"battery_status":"Not charging","charge_limit_enabled":true,
        "charge_limit":80,"hysteresis":5,"charging_stopped":true}));
    let (tone, state, reason, ..) = status(&stopped, Lang::Zh);
    assert_eq!((tone, state.as_str()), ("warn", "电量 80% · 已停止充电"));
    assert!(reason.unwrap().contains("75%"));
    // sysfs unreadable: capacity 0 and no status is "not readable", not 0%
    let (tone, state, ..) = status(&charge(json!({"capacity":0,"battery_status":""})), Lang::En);
    assert_eq!((tone, state.as_str()), ("warn", "Battery level not readable"));
    // no answer yet / refused
    assert_eq!(status(&Got::default(), Lang::En).1, "Reading battery…");
    let mut down = Got::default();
    down.charge.put(Err(Fail::Unreachable));
    let (tone, state, _, _, retry) = status(&down, Lang::En);
    assert_eq!((tone, state.as_str(), retry), ("bad", "Can't read the battery state", true));
    // an older answer kept after a failure is stale, said so
    let mut old = charge(json!({"capacity":50,"battery_status":"Full"}));
    old.charge.put(Err(Fail::Unreachable));
    let (tone, _, _, meta, _) = status(&old, Lang::En);
    assert_eq!(tone, "stale");
    assert!(meta.unwrap().starts_with("Numbers stopped at"));
}

#[test]
fn the_form_is_sent_with_its_values_and_a_skeleton_before() {
    let f = form_html(Lang::En, Some(Draft { on: true, limit: 80, hyst: 5 }));
    assert!(f.starts_with("<div id=\"dc-form\" data-signals="), "{f}");
    for s in ["&#34;_cle&#34;:true", "&#34;_cll&#34;:80", "&#34;_c0h&#34;:5", "include: /^_cl/", "id=\"dc-limit\"", "id=\"dc-hyst\"", "aria-valuetext=\"80%\""] {
        assert!(f.contains(s), "{s}: {f}");
    }
    assert!(!f.contains("display:none\"><div class=\"dc-slider__head"), "sliders shown when the limit is on");
    let off = form_html(Lang::En, Some(Draft { on: false, limit: 90, hyst: 3 }));
    assert!(off.contains("<div class=\"dc-slider\" data-show=\"$_cle\" style=\"display:none\">"), "{off}");
    let sk = form_html(Lang::En, None);
    assert!(sk.starts_with("<div id=\"dc-form\"><div class=\"nd-group\">") && !sk.contains("data-signals") && !sk.contains("<button"));
    // the same render twice is the same HTML: the stream never pushes it again
    assert_eq!(f, form_html(Lang::En, Some(Draft { on: true, limit: 80, hyst: 5 })));
}

#[test]
fn a_switch_the_device_did_not_report_is_off_with_a_dash() {
    let mut g = charge(json!({"charging_stopped":false}));
    g.ps.put(Ok(json!({})));
    g.fb.put(Ok(json!({"fast_boot":"1"})));
    let (rows, err) = mode_rows(&g, Lang::En);
    assert!(err.is_none());
    let by = |k: &str| rows.iter().find(|r| r.key == k).unwrap();
    assert_eq!((by("dps").on, by("dps").disabled), (Some(false), false));
    assert_eq!((by("ps").on, by("ps").disabled), (None, true));
    assert_eq!(by("ps").sub, "The device didn't report this setting.");
    assert_eq!((by("fb").on, by("fb").disabled, by("fb").want()), (Some(true), false, false));
    // not read yet: reading, disabled
    let (rows, _) = mode_rows(&Got::default(), Lang::Zh);
    assert!(rows.iter().all(|r| r.disabled && r.sub.contains("正在读取")));
    // a failed read says why
    let mut bad = Got::default();
    bad.ps.put(Err(Fail::Unreachable));
    assert!(mode_rows(&bad, Lang::En).1.unwrap().starts_with("Couldn't read power-save mode"));
}

#[test]
fn battery_details_keep_every_old_row() {
    let b = json!({"status":"Discharging","capacity":70,"voltage_uv":4_120_000,"current_ua":-350_000,
        "temperature":315,"charge_full_uah":11_011_000,"charge_full_design_uah":10_214_000,"cycle_count":66,
        "charger":{"online":false,"voltage_uv":31_000,"current_ua":0},
        "estimate":{"state":"ok","kind":"discharging_eta","minutes":272,"target_pct":100}});
    let rows = battery_rows(&b, Some(false), Lang::En);
    let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(labels, ["Estimate", "Battery", "Charger input", "Voltage", "Current", "Temperature", "Health", "Cycles", "Full / design capacity"]);
    let v = |l: &str| rows.iter().find(|r| r.label == l).unwrap().value.clone();
    assert_eq!(v("Estimate"), "About 4 h 32 min left");
    assert_eq!(v("Battery"), "Out 1.4 W");
    assert_eq!(v("Charger input"), "Not plugged in");
    assert_eq!(v("Voltage"), "4.12 V");
    assert_eq!(v("Current"), "-350 mA");
    assert_eq!(v("Temperature"), "31.5 °C");
    assert_eq!(v("Health"), "108%".replace("108%", "100%+ (fuel-gauge estimate)"));
    assert_eq!(v("Cycles"), "66");
    assert_eq!(v("Full / design capacity"), "11011 / 10214 mAh");
    // plugged in and charging: input watts and the device's own draw
    let c = json!({"voltage_uv":4_000_000,"current_ua":1_000_000,"charger":{"online":true,"voltage_uv":9_000_000,"current_ua":900_000},
        "estimate":{"state":"estimating"}});
    let rows = battery_rows(&c, Some(true), Lang::Zh);
    let v = |l: &str| rows.iter().find(|r| r.label == l).unwrap().value.clone();
    assert_eq!(v("预估"), "—");
    assert_eq!(v("电池"), "充入 4.0 W");
    assert_eq!(v("充电器输入"), "8.1 W");
    assert_eq!(v("整机"), "整机约 4.1 W（含转换损耗）");
    // plugged unknown: the charger's own `online` decides
    let rows = battery_rows(&json!({"charger":{"online":true,"current_ua":0}}), None, Lang::En);
    assert_eq!(rows.iter().find(|r| r.label == "Charger input").unwrap().value, "No input");
}

#[test]
fn the_dialogs_say_what_how_long_and_how_to_recover() {
    for k in [Rst::Reboot, Rst::Off, Rst::Reset] {
        for l in [Lang::En, Lang::Zh] {
            let c = rst_confirm(k, l);
            assert_eq!(c.tier, write::Tier::Dialog);
            assert!(c.cuts_uplink && c.downtime.is_some() && c.recovery.is_some() && !c.what.is_empty());
            assert_eq!(c.word.is_some(), k == Rst::Reset);
            assert_eq!(c.danger, k == Rst::Reset);
        }
    }
    assert_eq!(rst_confirm(Rst::Reset, Lang::Zh).word.as_deref(), Some("恢复出厂"));
    assert_eq!(rst_confirm(Rst::Reset, Lang::En).word.as_deref(), Some("factory reset"));
    assert!(rst_confirm(Rst::Off, Lang::En).recovery.unwrap().contains("power button"));
    assert!(write::word_ok(reset_word(Lang::En), " factory reset "));
    assert!(!write::word_ok(reset_word(Lang::En), "factory"));
    assert_eq!(rst_key("poweroff"), None);
    assert_eq!(Rst::Off.path(), "/api/device/poweroff");
}

#[test]
fn one_write_at_a_time() {
    let a = Guard::take().expect("free");
    assert!(Guard::take().is_none());
    drop(a);
    assert!(Guard::take().is_some());
}

#[test]
fn a_reply_is_judged_by_the_readback_never_by_itself() {
    let ok = || Ok(json!(null));
    assert_eq!(judged(ok(), Some(true), "x".into(), Lang::En), write::Outcome::Applied("x".into()));
    assert_eq!(judged(ok(), Some(false), "x".into(), Lang::En), write::Outcome::Accepted);
    assert_eq!(judged(ok(), None, "x".into(), Lang::En), write::Outcome::Accepted);
    assert_eq!(judged(Err(Fail::Unreachable), None, "x".into(), Lang::En), write::Outcome::Unknown);
    assert_eq!(judged(Err(Fail::Unreachable), Some(true), "x".into(), Lang::En), write::Outcome::Applied("x".into()));
    assert!(matches!(
        judged(Err(Fail::Refused(400, "limit must be 50-100".into(), String::new())), Some(false), "x".into(), Lang::En),
        write::Outcome::Failed(_)
    ));
}

/// Heat care's row reads the core's `heat` block (thermal-care §4.3).
#[test]
fn heat_row_says_level_and_what_it_holds() {
    assert!(heat_row(&json!(null), Lang::Zh).is_none());
    let n = heat_row(&json!({"level":"normal","since":null,"charge_paused":false,"chill":""}), Lang::Zh).unwrap();
    assert_eq!(n.value, "正常");
    // 1793246400 = 10-29 04:00 on the device clock (local labelled UTC)
    let h = heat_row(
        &json!({"level":"hot","since":1793246400,"charge_paused":true,"chill":"hot","chill_now":"hot","stock_protect":1}),
        Lang::Zh,
    )
    .unwrap();
    assert_eq!(h.value, "热 · 暂停充电 · CHILL 省电档 · 04:00 起 · 原厂保护在限流");
    let m = heat_row(
        &json!({"level":"warm","since":1793246400,"charge_paused":false,"manual_resume":true,"chill":""}),
        Lang::En,
    )
    .unwrap();
    assert_eq!(m.value, "Warm · charging on (you resumed it) · since 04:00");
}
