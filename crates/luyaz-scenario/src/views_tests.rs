//! The views moved with the engine from the agent (the picker and the names; their wording is
//! u60-api's): the agent's tests of them, unchanged but for the types.

use super::*;
use luyaz_policy::scenario as sc;
use crate::views::{names_en, picker, when_view};
use serde_json::json;
use luyaz_policy::scenario::{template, Detect, Scenario, ScenarioAction, SsidEntry, AWAY_ID};
use crate::Json;

fn mcc_scenario(id: &str, mccs: &[&str]) -> Scenario {
    Scenario {
        id: id.into(),
        name: id.into(),
        detect: Detect::Mcc { mccs: mccs.iter().map(|m| m.to_string()).collect() },
        inhibit_sleep: false,
        actions: vec![sc::wifi_action(true)],
    }
}

fn is_abroad(s: &Scenario) -> bool {
    sc::is_abroad(s)
}

fn describe_detect(d: &Detect, home_mcc: &str) -> String {
    describe_detect_both(d, home_mcc).0
}

fn describe_detect_both(d: &Detect, home_mcc: &str) -> (String, String) {
    luyaz_api::scenario::describe_when(&when_view(d), home_mcc)
}

fn describe_actions(s: &Scenario) -> String {
    describe_actions_both(s).0
}

fn describe_actions_both(s: &Scenario) -> (String, String) {
    let bodies: Vec<Option<Value>> = s.actions.iter().map(|a| a.action.body.as_ref().map(Value::from)).collect();
    let does: Vec<luyaz_api::scenario::Does> = s
        .actions
        .iter()
        .zip(&bodies)
        .map(|(a, b)| luyaz_api::scenario::Does { path: &a.action.path, body: b.as_ref() })
        .collect();
    luyaz_api::scenario::describe_does(&does, s.inhibit_sleep)
}

fn scenes_json(enabled: bool, current: &str, pin: Option<String>, guard_takeover: bool, cfg: &Config) -> Value {
    picker(enabled, current, pin, guard_takeover, cfg)
}

fn picker_list(cfg: &Config) -> Vec<Value> {
    scenes_json(true, "", None, false, cfg)["list"].as_array().cloned().unwrap_or_default()
}

fn picker_list_en(cfg: &Config) -> Vec<Value> {
    scenes_json(true, "", None, false, cfg)["list_en"].as_array().cloned().unwrap_or_default()
}

#[test]
fn picker_explains_every_template_scenario() {
    let cfg = template();
    let home = &cfg.scenarios[0];
    assert_eq!(describe_detect(&home.detect, "460"), "还没设家里的 Wi-Fi，不会自动进入");
    assert_eq!(describe_actions(home), "关 Wi-Fi · 不休眠，Tailscale 一直连得上");
    let away = cfg.scenarios.iter().find(|s| s.id == AWAY_ID).unwrap();
    assert_eq!(describe_detect(&away.detect, "460"), "其他情景都不符合时（默认）");
    assert_eq!(describe_actions(away), "开 Wi-Fi");
    let abroad = cfg.scenarios.iter().find(|s| s.id == "abroad").unwrap();
    assert_eq!(describe_detect(&abroad.detect, "460"), "插的不是中国的卡时（当地卡、境外 eSIM）");
    assert_eq!(describe_actions(abroad), "开 Wi-Fi · CHILL 改成直连");
    let ssid = Detect::Ssid {
        entries: ["A", "B", "C"].iter().map(|n| SsidEntry { ssid: n.to_string(), bssid: None }).collect(),
    };
    assert_eq!(describe_detect(&ssid, "460"), "附近有 Wi-Fi「A」「B」等 3 个时");
    assert_eq!(describe_detect(&Detect::Mcc { mccs: vec!["440".into()] }, "460"), "插的是日本的卡时");
}

#[test]
fn picker_explains_every_template_scenario_in_english() {
    let cfg = template();
    let home = &cfg.scenarios[0];
    assert_eq!(describe_detect_both(&home.detect, "460").1, "Home Wi-Fi not set yet; never entered automatically");
    assert_eq!(describe_actions_both(home).1, "Wi-Fi off · No sleep; Tailscale stays reachable");
    let away = cfg.scenarios.iter().find(|s| s.id == AWAY_ID).unwrap();
    assert_eq!(describe_detect_both(&away.detect, "460").1, "When no other scenario matches (default)");
    assert_eq!(describe_actions_both(away).1, "Wi-Fi on");
    let abroad = cfg.scenarios.iter().find(|s| s.id == "abroad").unwrap();
    assert_eq!(
        describe_detect_both(&abroad.detect, "460").1,
        "SIM not from China (local or foreign eSIM)"
    );
    assert_eq!(describe_actions_both(abroad).1, "Wi-Fi on · CHILL goes direct");
    let ssid = Detect::Ssid {
        entries: ["A", "B", "C"].iter().map(|n| SsidEntry { ssid: n.to_string(), bssid: None }).collect(),
    };
    assert_eq!(describe_detect_both(&ssid, "460").1, "When Wi-Fi \"A\", \"B\" (3 in all) is nearby");
    let two = Detect::Mcc { mccs: vec!["440".into(), "450".into(), "999".into()] };
    assert_eq!(describe_detect_both(&two, "460").1, "When the SIM is from Japan, South Korea, MCC 999");
    let empty = Scenario { actions: Vec::new(), inhibit_sleep: false, ..mcc_scenario("x", &["440"]) };
    assert_eq!(describe_actions_both(&empty), ("什么都不改".to_string(), "Changes nothing".to_string()));
}

/// Every action and detect kind, so every English arm is checked.
fn every_kind_scenarios() -> Vec<Scenario> {
    let act = |path: &str, body: Value| ScenarioAction {
        action: sc::Action { method: "PUT".into(), path: path.into(), body: Some(Json::from(body)) },
        ..sc::wifi_action(true)
    };
    let mut all = template().scenarios;
    let mut a = mcc_scenario("japan", &["440", "441"]);
    a.actions = vec![
        act("/api/wifi/radio", json!({"ap_2g": true, "ap_5g": false})),
        act("/api/wifi/guest", json!({"enabled": false})),
        act("/api/wifi/guest", json!({"enabled": true})),
        act("/api/device/power-save", json!({})),
        act("/api/device/thermal", json!({})),
        act("/api/services/chill/enable", json!({})),
        act("/api/services/chill/disable", json!({})),
        act("/api/services/chill/regions", json!({"member": "JP 01"})),
        act("/api/services/chill/exit", json!({})),
        act("/api/services/chill/bypass", json!({})),
    ];
    a.inhibit_sleep = true;
    all.push(a);
    all[0].detect = Detect::Ssid {
        entries: ["Home", "Home-5G", "Attic"].iter().map(|n| SsidEntry { ssid: n.to_string(), bssid: None }).collect(),
    };
    all
}

#[test]
fn every_english_field_is_free_of_cjk() {
    use luyaz_api::health::has_cjk;
    let cfg = Config { scenarios: every_kind_scenarios(), ..template() };
    for item in picker_list_en(&cfg) {
        for k in ["when_en", "does_en"] {
            let v = item[k].as_str().unwrap();
            assert!(!v.is_empty() && !has_cjk(v) && !v.ends_with('.') && !v.contains("Please"), "{k}: {v}");
        }
        if let Some(n) = item["name_en"].as_str() {
            assert!(!has_cjk(n), "{n}");
        }
    }
}

#[test]
fn factory_names_get_english_and_renamed_ones_do_not() {
    // 1. Still the factory name: English is added.
    let cfg = template();
    let list = picker_list_en(&cfg);
    let en: Vec<(&str, &str)> =
        list.iter().map(|s| (s["id"].as_str().unwrap(), s["name_en"].as_str().unwrap_or("-"))).collect();
    assert_eq!(en, vec![("home", "Home"), (AWAY_ID, "Away"), ("abroad", "Abroad")]);
    assert_eq!(picker_list(&cfg)[0]["name"], "在家", "the Chinese name is unchanged");
    assert_eq!(names_en(&cfg), json!({"home": "Home", "away": "Away", "abroad": "Abroad"}));

    // 2. Renamed by the owner: their name in both languages, no name_en.
    let mut renamed = template();
    renamed.scenarios[0].name = "我家".into();
    renamed.scenarios[1].name = "Out".into();
    let list = picker_list_en(&renamed);
    assert!(list[0]["name_en"].is_null() && list[1]["name_en"].is_null());
    assert_eq!(list[2]["name_en"], "Abroad");
    assert_eq!(names_en(&renamed), json!({"abroad": "Abroad"}));

    // 3. A scenario of the owner's that happens to be called 在家 is not
    //    the built-in one.
    let mut own = template();
    own.scenarios.push(Scenario { name: "在家".into(), ..mcc_scenario("office", &["440"]) });
    assert!(picker_list_en(&own)[3]["name_en"].is_null());

    // Never saved: the stored configuration carries no English.
    let saved = serde_json::to_string(&template()).unwrap();
    assert!(!saved.contains("name_en") && !saved.contains("Home"), "{saved}");
}

/// `scenes.list` exactly as the agent wrote it before L2 (copied from
/// picker_json at HEAD 2026-10-01), to prove the list did not change.
fn list_before_l2(cfg: &Config) -> Vec<Value> {
    cfg.scenarios
        .iter()
        .map(|s| {
            json!({
                "id": s.id,
                "name": s.name,
                "wifi_off": s.inhibit_sleep,
                "abroad": is_abroad(s),
                "when": describe_detect(&s.detect, &cfg.home_mcc),
                "does": describe_actions(s),
            })
        })
        .collect()
}

#[test]
fn scene_list_is_byte_for_byte_what_it_was() {
    let t = template();
    let abroad_does = "开 Wi-Fi · CHILL 改成直连";
    let want = format!(
        r#"[{{"abroad":false,"does":"关 Wi-Fi · 不休眠，Tailscale 一直连得上","id":"home","name":"在家","when":"还没设家里的 Wi-Fi，不会自动进入","wifi_off":true}},{{"abroad":false,"does":"开 Wi-Fi","id":"away","name":"外出","when":"其他情景都不符合时（默认）","wifi_off":false}},{{"abroad":true,"does":"{abroad_does}","id":"abroad","name":"国外","when":"插的不是中国的卡时（当地卡、境外 eSIM）","wifi_off":false}}]"#
    );
    assert_eq!(serde_json::to_string(&picker_list(&t)).unwrap(), want);
    let cfg = Config { scenarios: every_kind_scenarios(), ..template() };
    assert_eq!(serde_json::to_string(&picker_list(&cfg)).unwrap(), serde_json::to_string(&list_before_l2(&cfg)).unwrap());
    for item in picker_list(&cfg) {
        assert!(item.as_object().unwrap().keys().all(|k| !k.ends_with("_en")), "{item}");
    }
}

/// A reader may keep `scenes` in fixed buffers (8192 bytes overall,
/// 2048 for `list`, 768 per scene) and truncate silently (L2 review O1). The
/// English is a separate `list_en`, after `list`, so it only uses what
/// `sub` has spare.
#[test]
fn scenes_fit_the_old_touch_screen() {
    let long = |c: char| SsidEntry { ssid: std::iter::repeat_n(c, 32).collect(), bssid: None };
    // Worst case: six scenes, three full-length (32-byte) network names
    // each, every kind of action.
    let mut worst = every_kind_scenarios();
    for s in worst.iter_mut() {
        s.detect = Detect::Ssid { entries: vec![long('a'), long('b'), long('c')] };
    }
    worst.truncate(4);
    while worst.len() < 6 {
        worst.push(Scenario { id: format!("s{}", worst.len()), ..worst[0].clone() });
    }
    let cfg = Config { scenarios: worst, ..template() };
    let scenes = scenes_json(true, "japan", Some("japan".into()), true, &cfg);
    let text = scenes.to_string();
    let list = serde_json::to_string(&scenes["list"]).unwrap();
    let list_en = serde_json::to_string(&scenes["list_en"]).unwrap();
    eprintln!("worst case: list {} bytes, list_en {}, scenes {}", list.len(), list_en.len(), text.len());
    assert_eq!(list, serde_json::to_string(&list_before_l2(&cfg)).unwrap(), "list unchanged");
    assert!(list.len() <= 2047, "list {} bytes", list.len());
    assert!(text.len() < 8191, "scenes {} bytes", text.len());
    assert!(text.find(r#""list":"#).unwrap() < text.find(r#""list_en":"#).unwrap(), "list_en after list");
    for (i, e) in scenes["list_en"].as_array().unwrap().iter().enumerate() {
        assert!(e.to_string().len() < 767, "{e}");
        assert_eq!(e["id"], scenes["list"][i]["id"]);
        let keys: Vec<&str> = e.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["does_en", "id", "name_en", "when_en"]);
    }
    for e in scenes["list"].as_array().unwrap() {
        assert!(e.to_string().len() < 767, "{e}");
    }
}

#[test]
fn u60d_writes_the_same_restore_entries() {
    let same = |shared: Value, ours: sc::PendingRestore| {
        let back: sc::PendingRestore = serde_json::from_value(shared).unwrap();
        assert_eq!(serde_json::to_value(&back).unwrap(), serde_json::to_value(&ours).unwrap());
    };
    same(luyaz_api::chill::chill_on_restore(5), sc::chill_on_restore(5));
    same(luyaz_api::chill::chill_exit_restore(7), sc::chill_exit_restore(7));
}
