use super::*;
use crate::hub::Hub;
use crate::sse::Frame;
use std::sync::Arc;

fn blk(data: Value) -> Value {
    json!({"revision": 1, "observed_at": 1, "stale": false, "data": data})
}

/// u60g 10-20 的形状：单卡、槽 1，内置卡没启用（没有 MCC）。识别码是假的。
fn single1() -> Value {
    json!({"active_slot": 1, "mode": "single", "smart": false, "switching": false, "slots": [
        {"slot": 1, "provisioned": true, "present": true, "ready": true, "operator": "CUCC", "mcc": "460", "mnc": "01", "iccid_tail": "1111"},
        {"slot": 2, "provisioned": false, "present": true, "ready": null, "operator": "CMCC", "mcc": "", "mnc": "", "iccid_tail": "2222"}]})
}

fn hub_with(dual: Option<Value>, serving: i64) -> Arc<Hub> {
    let hub = Hub::new();
    let mut sim = json!({"state": "sim ready", "current_slot": 1});
    if let Some(d) = dual {
        sim["dual"] = d;
    }
    hub.apply(&Frame {
        event: "snapshot".into(),
        data: json!({"epoch": "e", "blocks": {"sim": blk(sim), "signal": blk(json!({"serving_mcc": serving}))}}).to_string(),
    });
    hub.set_link_for_test(Link::Live);
    hub
}

fn q(mode: &str, slot: &str, smart: &str) -> DualQuery {
    DualQuery { mode: mode.into(), slot: slot.into(), smart: smart.into() }
}

#[test]
fn view_reads_the_dual_block() {
    let hub = hub_with(Some(single1()), 460);
    let v = hub.read(|st| view(st, Lang::Zh));
    assert_eq!((v.mode.as_str(), v.main.as_str(), v.smart.as_str()), ("单卡", "卡槽 1（插拔卡）", "—"));
    assert_eq!(v.slots[0].state, "已启用 · 就绪");
    assert_eq!(v.slots[1].state, "未启用");
    assert_eq!(v.slots[1].carrier, "CMCC");
    assert_eq!(v.slots[0].tail, "…1111");
    assert!(!v.dis && v.note.is_none());
    // 单卡槽机器：只说明，不能改
    let hub = hub_with(None, 460);
    let v = hub.read(|st| view(st, Lang::Zh));
    assert_eq!(v.note.as_deref(), Some("这台设备只有一个卡槽。"));
    assert!(v.dis);
}

#[test]
fn check_and_params() {
    let d = parse(&single1()).unwrap();
    let ok = |q: DualQuery| check(&q, Some(&d), false, Lang::En);
    assert_eq!(ok(q("dual", "2", "0")).unwrap().params(), json!({"mode":"dual","smart":false,"active_slot":2}));
    assert_eq!(ok(q("dual", "", "1")).unwrap().params(), json!({"mode":"dual","smart":true}));
    assert_eq!(ok(q("single", "2", "1")).unwrap().params(), json!({"mode":"single","active_slot":2}));
    // 现状、没选主卡、卡槽、卡住、自动切换中都不发
    assert!(ok(q("single", "1", "0")).is_err());
    assert!(ok(q("single", "", "0")).is_err());
    assert!(ok(q("x", "1", "0")).is_err());
    assert!(check(&q("dual", "2", "0"), Some(&d), true, Lang::En).is_err());
    assert!(check(&q("dual", "2", "0"), None, false, Lang::En).is_err());
    let mut busy = d.clone();
    busy.switching = true;
    assert!(check(&q("dual", "2", "0"), Some(&busy), false, Lang::En).is_err());
    let mut empty2 = d.clone();
    empty2.slots[1].present = false;
    assert!(check(&q("dual", "1", "0"), Some(&empty2), false, Lang::En).is_err());
    assert!(check(&q("single", "2", "0"), Some(&empty2), false, Lang::En).is_err());
}

#[test]
fn read_back_matches_the_target_shape() {
    let hub = hub_with(Some(single1()), 460);
    let w = Want { dual: false, slot: Some(1), smart: false };
    assert!(hub.read(|st| read_back(&w, st)));
    let smart = Want { dual: true, slot: None, smart: true };
    assert!(!hub.read(|st| read_back(&smart, st)));
    // 智能开：原厂换到哪个槽都算对上（D3）
    let mut d = single1();
    d["mode"] = json!("dual");
    d["smart"] = json!(true);
    d["active_slot"] = json!(2);
    let hub = hub_with(Some(d), 460);
    assert!(hub.read(|st| read_back(&smart, st)));
}

#[test]
fn roaming_note_d4() {
    let mut d = parse(&single1()).unwrap();
    let to1 = Want { dual: false, slot: Some(1), smart: false };
    let to2 = Want { dual: true, slot: Some(2), smart: false };
    let smart = Want { dual: true, slot: None, smart: true };
    assert_eq!(roaming_note(&to1, &d, Some(460), Lang::En), None);
    // 槽 2 没启用，读不到 MCC → 提醒
    assert!(roaming_note(&to2, &d, Some(460), Lang::Zh).unwrap().contains("看不出"));
    assert!(roaming_note(&smart, &d, Some(460), Lang::Zh).is_some());
    assert!(roaming_note(&to1, &d, None, Lang::Zh).unwrap().contains("看不出"));
    // 在国外：槽 1 是 460，网络是 454 → 会漫游
    assert!(roaming_note(&to1, &d, Some(454), Lang::Zh).unwrap().contains("会漫游"));
    d.slots[1].mcc = "460".into();
    assert_eq!(roaming_note(&to2, &d, Some(460), Lang::En), None);
}

#[test]
fn confirm_is_a_dialog_that_cuts_the_uplink() {
    let w = Want { dual: true, slot: Some(2), smart: false };
    let c = confirm(&w, Some("R".into()), Lang::Zh);
    assert_eq!(c.tier, write::Tier::Dialog);
    assert!(c.cuts_uplink);
    assert!(c.what.contains("双卡，主卡卡槽 2（内置卡）") && c.what.ends_with('R'));
    assert!(c.recovery.unwrap().contains("2 分钟"));
}
