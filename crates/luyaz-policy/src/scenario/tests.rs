//! Moved from zte-agent `src/scenario.rs` (manager 20c3e35), same names and expectations; only
//! types changed (`serde_json::Value` bodies became `Json`, `Instant` became seconds,
//! `simulated_trip_abroad_and_back` uses `in_abroad_scenario` instead of an `Engine`). Not moved:
//! the picker/English/scene-list tests (presentation, stayed in the agent) and
//! `uptime_is_whole_seconds_of_the_first_field` (heartbeat file). The `step_*` and
//! `after_switch_*` tests are new.

use super::*;
use serde_json::json;

fn j(v: serde_json::Value) -> Json {
    Json::from(v)
}

/// A3: the scenario engine can't touch the home exit.
#[test]
fn home_exit_not_scenario_reachable() {
    assert!(!ALLOWED_PATHS.iter().any(|p| p.contains("/chill/home")));
}

fn net(ssid: &str, bssid: &str, signal: f64) -> Network {
    Network {
        ssid: ssid.into(),
        bssid: bssid.into(),
        signal,
    }
}

fn cfg_with(entries: Vec<SsidEntry>) -> Config {
    let mut c = template();
    if let Some(home) = c.scenarios.iter_mut().find(|s| s.id == "home") {
        home.detect = Detect::Ssid { entries };
    }
    c
}

#[test]
fn ssid_entries_only_for_wifi_scenarios() {
    let cfg = cfg_with(vec![SsidEntry {
        ssid: "MyHome".into(),
        bssid: None,
    }]);
    assert_eq!(ssid_entries(&cfg, "home").map(|e| e.len()), Some(1));
    assert!(
        ssid_entries(&cfg, AWAY_ID).is_none(),
        "fallback is not Wi-Fi detected"
    );
    assert!(ssid_entries(&cfg, "nope").is_none());
    let empty = cfg_with(Vec::new());
    assert!(
        ssid_entries(&empty, "home").is_none(),
        "no entries, nothing to recheck"
    );
}

#[test]
fn miss_summary_names_each_entry() {
    let entries = vec![
        SsidEntry {
            ssid: "Home".into(),
            bssid: Some("02:00:00:00:00:aa".into()),
        },
        SsidEntry {
            ssid: "Home_IoT".into(),
            bssid: Some("02:00:00:00:00:BB".into()),
        },
        SsidEntry {
            ssid: "Attic".into(),
            bssid: None,
        },
    ];
    let nets = [
        net("Other", "02:00:00:00:00:01", -50.0),
        net("Home_IoT", "02:00:00:00:00:bb", -80.0),
        net("Attic", "02:00:00:00:00:02", -75.0),
        net("Attic", "02:00:00:00:00:03", -72.0),
    ];
    let got = miss_summary(&entries, &nets, 1, -70.0);
    assert_eq!(
        got,
        "scans saw 1+3 networks; Home 02:00:00:00:00:aa absent, \
         Home_IoT 02:00:00:00:00:BB -80 dBm, Attic -72 dBm (below -70)"
    );
}

#[test]
fn unconfigured_engine_matches_nothing_but_fallback() {
    let cfg = cfg_with(Vec::new());
    let got = detect(&cfg, &[net("Someone", "02:00:00:00:00:aa", -30.0)], None);
    assert_eq!(got.as_deref(), Some(AWAY_ID));
}

#[test]
fn weak_signal_does_not_count_as_home() {
    let cfg = cfg_with(vec![SsidEntry {
        ssid: "MyHome".into(),
        bssid: None,
    }]);
    let got = detect(&cfg, &[net("MyHome", "02:00:00:00:00:aa", -85.0)], None);
    assert_eq!(got.as_deref(), Some(AWAY_ID));
}

#[test]
fn strong_signal_counts_as_home_when_no_bssid_pinned() {
    let cfg = cfg_with(vec![SsidEntry {
        ssid: "MyHome".into(),
        bssid: None,
    }]);
    let got = detect(&cfg, &[net("MyHome", "02:00:00:00:00:aa", -45.0)], None);
    assert_eq!(got.as_deref(), Some("home"));
}

#[test]
fn pinned_bssid_beats_a_strong_impostor() {
    let cfg = cfg_with(vec![SsidEntry {
        ssid: "MyHome".into(),
        bssid: Some("02:00:00:00:00:AA".into()),
    }]);
    let got = detect(&cfg, &[net("MyHome", "02:00:00:00:00:bb", -20.0)], None);
    assert_eq!(got.as_deref(), Some(AWAY_ID));
    let got = detect(&cfg, &[net("MyHome", "02:00:00:00:00:aa", -88.0)], None);
    assert_eq!(got.as_deref(), Some("home"));
}

#[test]
fn hysteresis_needs_two_consistent_readings_to_enter() {
    let p = Params::default();
    let mut st = RunState::default();
    assert_eq!(decide(&mut st, "home", &p), None);
    assert_eq!(decide(&mut st, "home", &p).as_deref(), Some("home"));
}

#[test]
fn a_single_stray_reading_does_not_move_the_device() {
    let p = Params::default();
    let mut st = RunState {
        current: "home".into(),
        ..Default::default()
    };
    assert_eq!(decide(&mut st, "away", &p), None);
    assert_eq!(decide(&mut st, "home", &p), None);
    assert_eq!(st.hits, 0, "a confirming reading resets the candidate");
    assert_eq!(decide(&mut st, "away", &p), None);
}

#[test]
fn leaving_home_takes_two_consecutive_misses_then_switches() {
    let p = Params::default();
    let mut st = RunState {
        current: "home".into(),
        ..Default::default()
    };
    assert_eq!(decide(&mut st, "away", &p), None);
    assert_eq!(decide(&mut st, "away", &p).as_deref(), Some("away"));
}

#[test]
fn rejects_paths_outside_the_allow_list() {
    let mut c = template();
    c.scenarios[0].actions[0].action.path = "/api/device/reboot".into();
    let err = validate(&c).unwrap_err();
    assert!(err.contains("not allowed"), "{err}");
    // allowed until step 9h: a config that still has it is reported, not silently run
    c.scenarios[0].actions[0].action.path = "/api/device/thermal".into();
    assert!(validate(&c).unwrap_err().contains("not allowed"));
}

#[test]
fn rejects_two_fallback_scenarios() {
    let mut c = template();
    c.scenarios[0].detect = Detect::Fallback;
    assert!(validate(&c).is_err());
}

#[test]
fn template_is_valid_and_inert() {
    let c = template();
    validate(&c).unwrap();
    match &c.scenarios[0].detect {
        Detect::Ssid { entries } => assert!(entries.is_empty()),
        _ => panic!("first scenario should detect by ssid"),
    }
}

#[test]
fn home_network_beats_a_foreign_sim() {
    let cfg = cfg_with(vec![SsidEntry {
        ssid: "MyHome".into(),
        bssid: Some("02:00:00:00:00:aa".into()),
    }]);
    let nets = [net("MyHome", "02:00:00:00:00:aa", -40.0)];
    assert_eq!(detect(&cfg, &nets, Some("440")).as_deref(), Some("home"));
}

fn mcc_scenario(id: &str, mccs: &[&str]) -> Scenario {
    Scenario {
        id: id.into(),
        name: id.into(),
        detect: Detect::Mcc {
            mccs: mccs.iter().map(|m| m.to_string()).collect(),
        },
        inhibit_sleep: false,
        actions: vec![wifi_action(true)],
    }
}

#[test]
fn any_foreign_sim_is_abroad() {
    let cfg = template();
    assert_eq!(detect(&cfg, &[], Some("440")).as_deref(), Some("abroad"));
    assert_eq!(detect(&cfg, &[], Some("520")).as_deref(), Some("abroad"));
}

#[test]
fn a_hand_added_mcc_scenario_beats_the_catch_all() {
    let mut cfg = template();
    cfg.scenarios
        .insert(0, mcc_scenario("japan", &["440", "441"]));
    let i = cfg.scenarios.iter().position(|s| s.id == "abroad").unwrap();
    let catch_all = cfg.scenarios.remove(i);
    cfg.scenarios.insert(0, catch_all);
    validate(&cfg).unwrap();
    assert_eq!(detect(&cfg, &[], Some("441")).as_deref(), Some("japan"));
    assert_eq!(detect(&cfg, &[], Some("520")).as_deref(), Some("abroad"));
}

#[test]
fn home_mcc_or_unknown_sim_is_away() {
    let cfg = template();
    assert_eq!(detect(&cfg, &[], Some("460")).as_deref(), Some(AWAY_ID));
    assert_eq!(detect(&cfg, &[], None).as_deref(), Some(AWAY_ID));
}

#[test]
fn empty_or_unready_sim_has_no_mcc() {
    let ok = j(json!({"sim_states": "sim ready", "sim_imsi": "440101234567890"}));
    assert_eq!(mcc_from_sim_info(&ok).as_deref(), Some("440"));
    let empty = j(json!({"sim_states": "sim ready", "sim_imsi": ""}));
    assert_eq!(mcc_from_sim_info(&empty), None);
    let not_ready = j(json!({"sim_states": "sim absent", "sim_imsi": "440101234567890"}));
    assert_eq!(mcc_from_sim_info(&not_ready), None);
    let junk = j(json!({"sim_states": "sim ready", "sim_imsi": "44x10"}));
    assert_eq!(mcc_from_sim_info(&junk), None);
}

#[test]
fn core_sim_block_maps_like_the_ubus_reply() {
    let ok = j(json!({"state": "sim ready", "imsi": "440101234567890", "iccid": "8981"}));
    assert_eq!(mcc_from_core_sim(&ok).as_deref(), Some("440"));
    for bad in [
        json!({"state": "sim ready", "imsi": ""}),
        json!({"state": "sim absent", "imsi": "440101234567890"}),
        json!({"state": "", "imsi": "440101234567890"}),
        json!({"state": "sim ready", "imsi": "44x10"}),
        json!({"state": "sim ready", "imsi": "4401"}),
        json!({"state": "sim ready"}),
        json!({}),
    ] {
        assert_eq!(mcc_from_core_sim(&j(bad.clone())), None, "{bad}");
    }
}

#[test]
fn fresh_block_wins_and_stale_falls_back_to_ubus() {
    let ready = j(json!({"state": "sim ready", "imsi": "460001234567890"}));
    assert_eq!(
        sim_mcc_from(Some(ready), || panic!("no ubus read with a fresh block")).as_deref(),
        Some("460")
    );
    let switching = j(json!({"state": "sim ready", "imsi": ""}));
    assert_eq!(
        sim_mcc_from(Some(switching), || panic!(
            "no ubus read with a fresh block"
        )),
        None
    );
    let direct = j(json!({"sim_states": "sim ready", "sim_imsi": "440101234567890"}));
    assert_eq!(sim_mcc_from(None, || Some(direct)).as_deref(), Some("440"));
    assert_eq!(
        sim_mcc_from(None, || None),
        None,
        "read failed: unknown, never abroad"
    );
}

#[test]
fn abroad_goes_direct_best_effort_and_remembers_the_old_value() {
    let cfg = template();
    let abroad = cfg.scenarios.iter().find(|s| s.id == "abroad").unwrap();
    assert_eq!(abroad.actions[0].action.path, "/api/wifi/radio");
    assert!(!abroad.actions[0].best_effort);
    let group = &abroad.actions[1];
    assert_eq!(group.action.path, "/api/services/chill/regions");
    assert_eq!(
        group.action.body.as_ref().unwrap().get("member"),
        Some(&Json::from("DIRECT"))
    );
    assert!(group.best_effort);
    assert!(group.verify_job.is_none(), "a region switch starts no job");
    assert_eq!(
        group.verify_field.as_ref().unwrap().equals,
        Json::from("DIRECT")
    );
    assert!(group.restore_on_exit.is_some());
}

#[test]
fn restore_puts_the_saved_member_back_and_verifies_it() {
    let a = chill_main_group_action("DIRECT");
    let back = restore_action(&a, j(json!("🇹🇼 台湾"))).unwrap();
    let body = back.action.body.as_ref().unwrap();
    assert_eq!(body.get("member"), Some(&Json::from("🇹🇼 台湾")));
    assert_eq!(body.get("group"), Some(&Json::from(CHILL_MAIN_GROUP)));
    assert_eq!(
        back.verify_field.as_ref().unwrap().equals,
        Json::from("🇹🇼 台湾")
    );
    assert!(
        back.restore_on_exit.is_none(),
        "a restore must not save again"
    );
    assert!(back.best_effort);
}

#[test]
fn same_group_different_member_shares_a_restore_key() {
    let k1 = restore_key(&chill_main_group_action("DIRECT"));
    let k2 = restore_key(&chill_main_group_action("🇯🇵 日本"));
    assert!(k1.is_some());
    assert_eq!(k1, k2);
    assert_eq!(restore_key(&wifi_action(true)), None);
    // Byte for byte the key the agent writes into restore.json (serde_json's text).
    let agent = format!(
        "/api/services/chill/regions {}",
        json!({"group": CHILL_MAIN_GROUP})
    );
    assert_eq!(k1.as_deref(), Some(agent.as_str()));
}

#[test]
fn restore_is_due_on_leaving_abroad_but_not_while_in_it() {
    let cfg = template();
    let a = chill_main_group_action("DIRECT");
    let pending = vec![PendingRestore {
        key: restore_key(&a).unwrap(),
        action: restore_action(&a, j(json!("🇹🇼 台湾"))).unwrap(),
        saved_at: 0,
        while_abroad: false,
    }];
    let by = |id: &str| cfg.scenarios.iter().find(|s| s.id == id);
    assert!(due_restores(&pending, by("abroad")).is_empty());
    assert_eq!(due_restores(&pending, by("away")).len(), 1);
    assert_eq!(due_restores(&pending, by("home")).len(), 1);
    assert_eq!(
        due_restores(&pending, None).len(),
        1,
        "engine off restores too"
    );
}

#[test]
fn chill_switched_off_abroad_comes_back_only_when_home_and_first() {
    let cfg = template();
    let by = |id: &str| cfg.scenarios.iter().find(|s| s.id == id);
    let a = chill_main_group_action("DIRECT");
    let pending = vec![
        PendingRestore {
            key: restore_key(&a).unwrap(),
            action: restore_action(&a, j(json!("🇹🇼 台湾"))).unwrap(),
            saved_at: 0,
            while_abroad: false,
        },
        chill_on_restore(0),
    ];
    assert!(
        due_restores(&pending, by("abroad")).is_empty(),
        "still abroad: CHILL stays off"
    );
    let due = due_restores(&pending, by("away"));
    assert_eq!(due.len(), 2);
    assert_eq!(due[0], CHILL_ON_KEY, "CHILL on before its group is set");
    assert_eq!(due_restores(&pending, by("home"))[0], CHILL_ON_KEY);
    assert_eq!(
        due_restores(&pending, None)[0],
        CHILL_ON_KEY,
        "engine off restores too"
    );
    let mut with_exit = pending.clone();
    with_exit.insert(0, chill_exit_restore(0));
    let due = due_restores(&with_exit, by("away"));
    assert_eq!(due.first().map(String::as_str), Some(CHILL_ON_KEY));
    assert_eq!(
        due.last().map(String::as_str),
        Some(CHILL_EXIT_KEY),
        "mode last, after the node"
    );
    assert!(due_restores(&with_exit, by("abroad")).is_empty());
    let mcc = Scenario {
        id: "jp".into(),
        name: "日本".into(),
        detect: Detect::Mcc {
            mccs: vec!["440".into()],
        },
        actions: Vec::new(),
        inhibit_sleep: false,
    };
    assert_eq!(
        due_restores(&pending, Some(&mcc)),
        vec![restore_key(&a).unwrap()]
    );
}

#[test]
fn simulated_trip_abroad_and_back() {
    let cfg = template();
    let by = |id: &str| cfg.scenarios.iter().find(|s| s.id == id);
    assert!(!in_abroad_scenario(&cfg, true, "away"));
    assert!(!in_abroad_scenario(&cfg, true, "home"));
    assert!(
        !in_abroad_scenario(&cfg, true, ""),
        "just booted, nothing decided yet"
    );
    assert!(!in_abroad_scenario(&cfg, false, "abroad"), "engine off");
    assert!(in_abroad_scenario(&cfg, true, "abroad"));
    let abroad = by("abroad").unwrap();
    let group = abroad
        .actions
        .iter()
        .find(|a| a.restore_on_exit.is_some())
        .unwrap();
    assert_eq!(
        group.action.body.as_ref().unwrap().get("member"),
        Some(&Json::from("DIRECT"))
    );
    let mut pending = vec![PendingRestore {
        key: restore_key(group).unwrap(),
        action: restore_action(group, j(json!("🇹🇼 台湾"))).unwrap(),
        saved_at: 1,
        while_abroad: false,
    }];
    pending.push(chill_exit_restore(2));
    pending.push(chill_on_restore(3));
    assert!(due_restores(&pending, Some(abroad)).is_empty());
    for back in ["away", "home"] {
        let due = due_restores(&pending, by(back));
        assert_eq!(due.len(), 3, "{back}");
        assert_eq!(due[0], CHILL_ON_KEY);
        let node = pending.iter().find(|p| p.key == due[1]).unwrap();
        assert_eq!(
            node.action.action.body.as_ref().unwrap().get("member"),
            Some(&Json::from("🇹🇼 台湾"))
        );
        let exit = pending.iter().find(|p| p.key == due[2]).unwrap();
        assert_eq!(exit.action.action.path, "/api/services/chill/exit");
        assert_eq!(
            exit.action.action.body.as_ref().unwrap().get("state"),
            Some(&Json::from("proxy"))
        );
        assert!(
            pending.iter().all(|p| p.action.best_effort),
            "a failed restore is retried, never rolls back Wi-Fi"
        );
    }
}

#[test]
fn chill_on_restore_waits_for_the_job_and_survives_a_round_trip() {
    let r = chill_on_restore(5);
    assert_eq!(r.action.action.path, "/api/services/chill/enable");
    assert!(ALLOWED_PATHS.contains(&r.action.action.path.as_str()));
    assert_eq!(
        r.action.verify_job.as_deref(),
        Some("/api/services/chill/job")
    );
    assert!(r.action.best_effort);
    #[cfg(feature = "serde")]
    {
        let text = serde_json::to_string(&vec![r]).unwrap();
        let back: Vec<PendingRestore> = serde_json::from_str(&text).unwrap();
        assert!(back[0].while_abroad);
        let old: Vec<PendingRestore> =
            serde_json::from_str(&text.replace(",\"while_abroad\":true", "")).unwrap();
        assert!(!old[0].while_abroad);
    }
}

#[test]
fn rejects_best_effort_wifi() {
    let mut c = template();
    c.scenarios[0].actions[0].best_effort = true;
    assert!(validate(&c).unwrap_err().contains("best_effort"));
}

#[test]
fn rejects_an_mcc_scenario_that_includes_home() {
    let mut c = template();
    c.scenarios.push(Scenario {
        id: "oops".into(),
        name: "oops".into(),
        detect: Detect::Mcc {
            mccs: vec!["460".into()],
        },
        inhibit_sleep: false,
        actions: Vec::new(),
    });
    assert!(validate(&c).unwrap_err().contains("home MCC"));
}

/// The agent's version also checks the English picker list; that part stayed with the picker.
#[cfg(feature = "serde")]
#[test]
fn config_written_before_abroad_support_still_loads() {
    let old = r#"{
      "version": 1,
      "params": {"scan_interval_home_secs": 60, "scan_interval_away_battery_secs": 300,
                 "scan_interval_away_charging_secs": 60, "enter_hits": 2,
                 "exit_misses": 2, "min_rssi_dbm": -70.0},
      "scenarios": [
        {"id": "home", "name": "在家", "inhibit_sleep": true,
         "detect": {"type": "ssid", "entries": [{"ssid": "MyHome", "bssid": "02:00:00:00:00:aa"}]},
         "actions": [{"method": "PUT", "path": "/api/wifi/radio",
                      "body": {"ap_2g": false, "ap_5g": false},
                      "snapshot": ["wireless.main_2g.disabled", "wireless.main_5g.disabled"]}]},
        {"id": "away", "name": "外出", "inhibit_sleep": false,
         "detect": {"type": "fallback"},
         "actions": [{"method": "PUT", "path": "/api/wifi/radio",
                      "body": {"ap_2g": true, "ap_5g": true},
                      "snapshot": ["wireless.main_2g.disabled", "wireless.main_5g.disabled"]}]}
      ]
    }"#;
    let cfg: Config = serde_json::from_str(old).unwrap();
    validate(&cfg).unwrap();
    assert_eq!(cfg.home_mcc, "460");
    assert_eq!(cfg.scenarios.len(), 2);
    assert!(!cfg.scenarios[0].actions[0].best_effort);
    assert_eq!(detect(&cfg, &[], Some("440")).as_deref(), Some(AWAY_ID));
    // and it writes back the shape the agent writes
    let again: Config = serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
    assert_eq!(again, cfg);
    assert_eq!(cfg.scenarios[0].actions[0], wifi_action(false));
}

#[test]
fn takeover_holds_for_the_full_period_then_releases_once() {
    let hold = 600.0;
    let t0 = 1000.0;
    let mut seen = None;

    assert_eq!(takeover_state(None, &mut seen, t0, hold), Takeover::None);
    assert!(seen.is_none());

    assert_eq!(
        takeover_state(Some("100"), &mut seen, t0, hold),
        Takeover::Active
    );
    assert_eq!(
        takeover_state(Some("100"), &mut seen, t0 + 599.0, hold),
        Takeover::Active
    );
    let done = t0 + hold;
    assert_eq!(
        takeover_state(Some("100"), &mut seen, done, hold),
        Takeover::Release
    );
    assert_eq!(
        takeover_state(Some("100"), &mut seen, done, hold),
        Takeover::Active
    );
}

#[test]
fn a_new_takeover_restarts_the_clock() {
    let hold = 600.0;
    let t0 = 1000.0;
    let mut seen = None;
    takeover_state(Some("100"), &mut seen, t0, hold);
    let t1 = t0 + 500.0;
    assert_eq!(
        takeover_state(Some("900"), &mut seen, t1, hold),
        Takeover::Active
    );
    assert_eq!(
        takeover_state(Some("900"), &mut seen, t0 + hold, hold),
        Takeover::Active
    );
    assert_eq!(
        takeover_state(Some("900"), &mut seen, t1 + hold, hold),
        Takeover::Release
    );
}

#[test]
fn marker_removed_by_hand_ends_the_takeover() {
    let hold = 600.0;
    let t0 = 1000.0;
    let mut seen = None;
    takeover_state(Some("100"), &mut seen, t0, hold);
    assert_eq!(takeover_state(None, &mut seen, t0, hold), Takeover::None);
    assert!(seen.is_none());
}

#[test]
fn fallback_may_not_turn_wifi_off() {
    let mut c = template();
    let away = c.scenarios.iter_mut().find(|s| s.id == AWAY_ID).unwrap();
    away.actions = vec![wifi_action(false)];
    assert!(validate(&c).unwrap_err().contains("fallback"));
}

// ── new: scan_interval, step, after_switch ──────────────────────────────────

const HOME_BSSID: &str = "02:00:00:00:00:aa";

fn home_cfg() -> Config {
    cfg_with(vec![SsidEntry {
        ssid: "MyHome".into(),
        bssid: Some(HOME_BSSID.into()),
    }])
}

fn home_nets() -> Vec<Network> {
    vec![net("MyHome", HOME_BSSID, -40.0)]
}

fn input<'a>(cfg: &'a Config, st: &'a RunState, now: i64, scan: Scan<'a>) -> TickInput<'a> {
    TickInput {
        cfg,
        state: st,
        enabled: true,
        takeover: false,
        pin: None,
        now,
        pin_retry_at: 0,
        charging: false,
        scan,
        recheck: None,
        mcc: Some("460"),
        beaconing: true,
    }
}

fn in_scenario(current: &str, last_scan: i64) -> RunState {
    RunState {
        current: current.into(),
        last_scan,
        ..Default::default()
    }
}

#[test]
fn scan_interval_follows_the_scenario_and_the_charger() {
    let cfg = template();
    assert_eq!(scan_interval(&cfg, "home", "", false), 60, "APs down");
    assert_eq!(
        scan_interval(&cfg, "", "", false),
        60,
        "nothing decided yet"
    );
    assert_eq!(
        scan_interval(&cfg, AWAY_ID, "home", false),
        60,
        "switch in the making"
    );
    assert_eq!(
        scan_interval(&cfg, AWAY_ID, "", false),
        300,
        "away on battery"
    );
    assert_eq!(
        scan_interval(&cfg, AWAY_ID, "", true),
        60,
        "away on charger"
    );
    assert_eq!(
        scan_interval(&cfg, "abroad", "", false),
        300,
        "abroad scans like away"
    );
}

#[test]
fn step_asks_for_a_scan_only_when_due() {
    let cfg = home_cfg();
    let st = in_scenario(AWAY_ID, 1000);
    let o = step(&input(&cfg, &st, 1299, Scan::NotScanned));
    assert_eq!((o.branch, &o.intent), (Branch::NotDue, &Intent::None));
    assert!(!o.keep_awake);
    assert_eq!(o.state, st);
    let o = step(&input(&cfg, &st, 1300, Scan::NotScanned));
    assert_eq!((o.branch, &o.intent), (Branch::Due, &Intent::NeedScan));
    assert_eq!(
        o.restores,
        Some(Restores {
            next: Some(AWAY_ID.into())
        })
    );
    // at home, not due: refresh the sleep defences
    let st = in_scenario("home", 1000);
    let o = step(&input(&cfg, &st, 1010, Scan::NotScanned));
    assert!(o.branch == Branch::NotDue && o.keep_awake);
    // just booted: no restores before the first decision
    let st = RunState::default();
    let o = step(&input(&cfg, &st, 1000, Scan::NotScanned));
    assert_eq!((o.intent, o.restores), (Intent::NeedScan, None));
}

#[test]
fn step_stays_when_the_scan_agrees() {
    let cfg = home_cfg();
    let nets = home_nets();
    let st = RunState {
        candidate: AWAY_ID.into(),
        hits: 1,
        misses: 1,
        scan_failures: 2,
        last_error: Some("scan: busy".into()),
        ..in_scenario("home", 1000)
    };
    let o = step(&input(
        &cfg,
        &st,
        1060,
        Scan::Networks {
            at: 1062,
            networks: &nets,
        },
    ));
    assert_eq!((o.branch, &o.intent), (Branch::Scanned, &Intent::None));
    assert_eq!(o.state.last_scan, 1062);
    assert_eq!(
        (o.state.hits, o.state.misses, o.state.candidate.as_str()),
        (0, 0, "")
    );
    assert_eq!(o.state.scan_failures, 0);
    assert_eq!(
        o.state.last_error, st.last_error,
        "a good scan does not clear last_error"
    );
}

#[test]
fn step_builds_a_candidate_up_to_a_switch() {
    let cfg = home_cfg();
    let nets = home_nets();
    let mut st = in_scenario(AWAY_ID, 0);
    let mut now = 1000;
    let o = step(&input(
        &cfg,
        &st,
        now,
        Scan::Networks {
            at: now,
            networks: &nets,
        },
    ));
    assert_eq!(o.intent, Intent::None);
    assert_eq!((o.state.candidate.as_str(), o.state.hits), ("home", 1));
    st = o.state;
    // the candidate shortens the interval to the charging one
    now += 60;
    let o = step(&input(
        &cfg,
        &st,
        now,
        Scan::Networks {
            at: now,
            networks: &nets,
        },
    ));
    assert_eq!(o.intent, Intent::Switch { to: "home".into() });
    assert_eq!(o.state.hits, 2);
    let after = after_switch(&o.state, &cfg, "home", now + 20, Ok(()));
    assert_eq!(after.current, "home");
    assert_eq!(after.last_switch, Some(now + 20));
    assert_eq!(
        (after.candidate.as_str(), after.hits, after.misses),
        ("", 0, 0)
    );
}

#[test]
fn step_fails_open_after_three_scan_failures() {
    let cfg = home_cfg();
    let mut st = in_scenario("home", 0);
    for n in 1..=3 {
        let now = n * 60;
        let o = step(&TickInput {
            beaconing: false,
            ..input(
                &cfg,
                &st,
                now,
                Scan::Failed {
                    at: now,
                    error: "busy",
                },
            )
        });
        assert_eq!(o.branch, Branch::ScanFailed);
        assert_eq!(o.state.scan_failures, n as u32);
        assert_eq!(o.state.last_error.as_deref(), Some("scan: busy"));
        assert_eq!(o.state.last_scan, now);
        let want = if n == 3 {
            Intent::Switch { to: AWAY_ID.into() }
        } else {
            Intent::None
        };
        assert_eq!(o.intent, want, "failure {n}");
        st = o.state;
    }
    // Wi-Fi is beaconing anyway: nothing to rescue
    let o = step(&input(
        &cfg,
        &st,
        240,
        Scan::Failed {
            at: 240,
            error: "busy",
        },
    ));
    assert_eq!(o.intent, Intent::None);
    // already away: nothing to do either
    let away = RunState {
        scan_failures: 5,
        ..in_scenario(AWAY_ID, 0)
    };
    let o = step(&TickInput {
        beaconing: false,
        ..input(
            &cfg,
            &away,
            1000,
            Scan::Failed {
                at: 1000,
                error: "busy",
            },
        )
    });
    assert_eq!(o.intent, Intent::None);
    // a failed apply of away lands on away with the error
    let after = after_switch(&st, &cfg, AWAY_ID, 300, Err("wifi: timeout"));
    assert_eq!(
        (after.current.as_str(), after.last_error.as_deref()),
        (AWAY_ID, Some("wifi: timeout"))
    );
}

#[test]
fn step_recheck_saves_home() {
    let cfg = home_cfg();
    let empty: Vec<Network> = Vec::new();
    let nets = home_nets();
    let st = in_scenario("home", 1000);
    let first = Scan::Networks {
        at: 1061,
        networks: &empty,
    };
    let o = step(&input(&cfg, &st, 1060, first));
    assert_eq!(o.intent, Intent::NeedRecheck);
    assert_eq!(o.state, st, "nothing written before the recheck");
    // seen on the recheck: stays home, no miss counted
    let o = step(&TickInput {
        recheck: Some(Recheck {
            at: 1065,
            networks: &nets,
        }),
        ..input(&cfg, &st, 1060, first)
    });
    assert_eq!((o.branch, &o.intent), (Branch::Scanned, &Intent::None));
    assert_eq!((o.state.misses, o.state.last_scan), (0, 1065));
    // unseen twice (the recheck scan failed: no networks): a miss
    let o = step(&TickInput {
        recheck: Some(Recheck {
            at: 1065,
            networks: &empty,
        }),
        ..input(&cfg, &st, 1060, first)
    });
    assert_eq!((o.state.candidate.as_str(), o.state.misses), (AWAY_ID, 1));
    assert_eq!(o.intent, Intent::None);
    // leaving away for home needs no recheck: away is not Wi-Fi detected
    let away = in_scenario(AWAY_ID, 0);
    let o = step(&input(
        &cfg,
        &away,
        1000,
        Scan::Networks {
            at: 1000,
            networks: &nets,
        },
    ));
    assert_eq!(o.state.candidate, "home");
}

#[test]
fn step_pin_overrides_detection() {
    let cfg = home_cfg();
    let st = in_scenario(AWAY_ID, 0);
    let pinned = |pin, st: &RunState, now, retry| {
        step(&TickInput {
            pin: Some(pin),
            pin_retry_at: retry,
            ..input(&cfg, st, now, Scan::NotScanned)
        })
    };
    let o = pinned("home", &st, 1000, 0);
    assert_eq!(
        (o.branch, &o.intent),
        (Branch::Pinned, &Intent::Switch { to: "home".into() })
    );
    let home = in_scenario("home", 0);
    let o = pinned("home", &home, 1000, 0);
    assert_eq!(o.intent, Intent::None);
    assert_eq!(
        (o.pin_retry_at, o.restores),
        (
            1000,
            Some(Restores {
                next: Some("home".into())
            })
        )
    );
    let o = pinned("home", &home, 1059, 1000);
    assert_eq!(
        (o.pin_retry_at, o.restores),
        (1000, None),
        "retry once a minute"
    );
    // a pin to a scenario that is not configured: switch_to would do nothing
    let o = pinned("office", &home, 1000, 0);
    assert_eq!((o.branch, o.intent), (Branch::Pinned, Intent::None));
    // a takeover beats the pin
    let o = step(&TickInput {
        pin: Some("home"),
        takeover: true,
        ..input(&cfg, &home, 1000, Scan::NotScanned)
    });
    assert_eq!(
        (o.branch, o.intent),
        (Branch::Takeover, Intent::Switch { to: AWAY_ID.into() })
    );
}

#[test]
fn step_disabled_goes_back_to_away_and_restores_everything() {
    let cfg = home_cfg();
    let home = RunState {
        hits: 1,
        ..in_scenario("home", 500)
    };
    let off = |st| {
        step(&TickInput {
            enabled: false,
            ..input(&cfg, st, 1000, Scan::NotScanned)
        })
    };
    let o = off(&home);
    assert_eq!((o.branch, &o.intent), (Branch::Disabled, &Intent::Bootsafe));
    assert_eq!(
        o.state,
        RunState::default(),
        "bootsafe clears the run state"
    );
    assert_eq!(o.restores, Some(Restores { next: None }));
    let away = in_scenario(AWAY_ID, 500);
    let o = off(&away);
    assert_eq!((o.intent, o.state), (Intent::None, away.clone()));
    assert_eq!(o.restores, Some(Restores { next: None }));
    // disabled during a takeover: the takeover branch does nothing
    let o = step(&TickInput {
        enabled: false,
        takeover: true,
        ..input(&cfg, &home, 1000, Scan::NotScanned)
    });
    assert_eq!((o.branch, o.intent), (Branch::Takeover, Intent::None));
    // unconfigured: nothing at all
    let empty = Config::default();
    let o = step(&input(&empty, &home, 1000, Scan::NotScanned));
    assert_eq!(
        (o.branch, o.intent, o.restores),
        (Branch::Unconfigured, Intent::None, None)
    );
}

#[test]
fn step_detects_abroad_by_the_sim() {
    let cfg = home_cfg();
    let st = in_scenario(AWAY_ID, 0);
    let o = step(&TickInput {
        mcc: Some("440"),
        ..input(
            &cfg,
            &st,
            1000,
            Scan::Networks {
                at: 1000,
                networks: &[],
            },
        )
    });
    assert_eq!(o.state.candidate, "abroad");
}

#[test]
fn after_switch_ignores_an_unknown_scenario() {
    let cfg = home_cfg();
    let st = RunState {
        candidate: "x".into(),
        hits: 3,
        ..in_scenario(AWAY_ID, 0)
    };
    assert_eq!(after_switch(&st, &cfg, "office", 5, Ok(())), st);
}
