use super::*;
use crate::hub::Hub;
use crate::sse::Frame;
use std::sync::Arc;

fn blk(data: Value) -> Value {
    json!({"revision": 1, "observed_at": 1, "stale": false, "data": data})
}

fn prof(id: &str, name: &str, apn: &str, pdp: i64, auth: i64, en: bool) -> Value {
    json!({"id": id, "name": name, "apn": apn, "username": "", "has_password": false, "auth": auth, "pdp": pdp, "roaming_pdp": 2, "enabled": en})
}

fn block(mode: i64) -> Value {
    json!({"mode": mode, "current": "manu1",
           "manual": [prof("manu1", "CM", "cmnet", 3, 0, true), prof("manu2", "Work", "corp", 1, 2, false), prof("manu3", "", "", 0, 0, false)],
           "auto": [prof("auto1", "CT", "ctiot", 3, 0, true), prof("auto2", "CT 4G", "ctnet", 3, 0, false)]})
}

fn hub_with(apn: Value, op: Option<Value>) -> Arc<Hub> {
    let hub = Hub::new();
    let mut blocks = json!({"apn": blk(apn)});
    if let Some(o) = op {
        blocks["op"] = blk(o);
    }
    hub.apply(&Frame { event: "snapshot".into(), data: json!({"epoch": "e", "blocks": blocks}).to_string() });
    hub.set_link_for_test(Link::Live);
    hub
}

fn cjk(s: &str) -> bool {
    s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c) || ('\u{3000}'..='\u{303f}').contains(&c) || ('\u{ff00}'..='\u{ffef}').contains(&c))
}

#[test]
fn block_reads_and_hides_empty_slots() {
    let a = Apn::of(&block(1));
    assert_eq!(a.manual.len(), 2);
    assert!(a.in_use("manu1") && !a.in_use("manu2"));
    assert_eq!(a.dialing().unwrap().id, "manu1");
    assert_eq!(a.manual[1].sub(), "corp · IPv4 · CHAP");
    let a = Apn::of(&block(0));
    assert!(!a.in_use("manu1"));
    assert_eq!(a.dialing().unwrap().id, "auto1");
}

#[test]
fn view_marks_rows_like_the_old_page() {
    let marks = Marks::of(&json!({"apn": {"picked_id": "manu2", "auto": [{"id": "auto1", "iot": true}], "notice": {"text": "换卡了", "text_en": "SIM changed"}}}));
    let hub = hub_with(block(0), None);
    let v = hub.read(|st| view(st, Some(&marks), None, Lang::Zh));
    assert_eq!(v.state, "数据连接用的是「CT」");
    assert_eq!(v.manual[0].marks, vec!["手动模式时用这条"]);
    assert_eq!(v.manual[1].marks, vec!["本卡"]);
    assert!(v.manual[0].can_delete && !v.manual[0].can_enable);
    assert_eq!(v.auto[0].marks, vec!["使用中", "物联网"]);
    assert_eq!(v.notice.as_deref(), Some("已自动调整：换卡了"));
    let v = hub_with(block(1), None).read(|st| view(st, None, None, Lang::Zh));
    assert_eq!(v.manual[0].marks, vec!["使用中"]);
    assert!(!v.manual[0].can_delete);
    assert!(v.auto[0].marks.is_empty());
    let op = json!({"active": {"op_id": "x", "item": "apn", "say_zh": "正在换 APN"}});
    let v = hub_with(block(1), Some(op)).read(|st| view(st, None, None, Lang::Zh));
    assert!(v.dis && v.txn.as_deref() == Some("正在换 APN"));
}

fn form(id: &str, name: &str, apn: &str, pw: &str, act: bool) -> FormIn {
    FormIn { id: id.into(), name: name.into(), apn: apn.into(), user: String::new(), pw: pw.into(), pdp: json!("3"), auth: json!("1"), act }
}

#[test]
fn save_checks_and_steps() {
    let a = Apn::of(&block(1));
    let s = check_save(&form("", " New ", " x.apn ", "", false), Some(&a), false, Lang::Zh).unwrap();
    assert_eq!(s.steps.len(), 1);
    assert_eq!(s.steps[0].0, "apn.add");
    assert_eq!(s.steps[0].1, json!({"name": "New", "apn": "x.apn", "auth_mode": 1, "pdp_type": 3, "roaming_pdp_type": 3}));
    assert!(!s.redials);
    // edit the one not in use and switch to it: modify then enable; redials in manual mode
    let s = check_save(&form("manu2", "Work", "corp2", "secret", true), Some(&a), false, Lang::Zh).unwrap();
    assert_eq!(s.steps.iter().map(|x| x.0).collect::<Vec<_>>(), ["apn.modify", "apn.enable"]);
    assert_eq!(s.steps[0].1["password"], "secret");
    assert_eq!(s.steps[0].1["roaming_pdp_type"], 2);
    assert_eq!(s.steps[0].1["username"], "");
    assert!(s.redials && !s.picks);
    // empty password = keep (not sent)
    let s = check_save(&form("manu1", "CM", "cmnet", "", false), Some(&a), false, Lang::Zh).unwrap();
    assert!(s.steps[0].1.get("password").is_none() && s.redials);
    assert!(matches!(check_save(&form("", "CM", "y", "", false), Some(&a), false, Lang::Zh), Err(write::Outcome::NotYet(t)) if t == "已存在同名 APN"));
    assert!(check_save(&form("", "a", " ", "", false), Some(&a), false, Lang::Zh).is_err());
    assert!(check_save(&form("manu9", "a", "b", "", false), Some(&a), false, Lang::Zh).is_err());
    assert!(check_save(&form("", "a", "b", "", false), Some(&a), true, Lang::Zh).is_err());
    // auto mode: switching is a pick, not a redial
    let a0 = Apn::of(&block(0));
    let s = check_save(&form("manu2", "Work", "corp", "", true), Some(&a0), false, Lang::Zh).unwrap();
    assert!(!s.redials && s.picks);
}

#[test]
fn the_password_is_in_no_html() {
    let a = Apn::of(&block(1));
    let s = check_save(&form("manu2", "Work", "corp", "hunter2pw", true), Some(&a), false, Lang::Zh).unwrap();
    let c = confirm(&Act::Save("tok".into()), Some(&a), Some(&s), Lang::Zh).unwrap();
    let html = area(&Act::Save("tok".into()), Lang::Zh, false, &write::Phase::Ask(c));
    assert!(!html.contains("hunter2pw") && html.contains("/router/apn/save/set?t=tok"));
    let t = put(s.clone());
    assert_eq!(take(&t), Some(s));
    assert_eq!(take(&t), None, "a token is used once");
    let f = super::form(a.find("manu2"), Lang::Zh);
    assert!(f.contains("&#34;_fpw&#34;:&#34;&#34;") && f.contains("&#34;_fname&#34;:&#34;Work&#34;"), "{f}");
}

#[test]
fn readback_per_step() {
    let a = Apn::of(&block(1));
    assert!(read_back("apn.set_mode", &json!({"mode": 1}), None, &a));
    assert!(!read_back("apn.enable", &json!({"profile_id": "manu2"}), None, &a));
    assert!(read_back("apn.delete", &json!({"profile_id": "manu9"}), None, &a));
    assert!(read_back("apn.modify", &json!({"profile_id": "manu2", "name": "Work", "apn": "corp"}), None, &a));
    assert!(read_back("apn.add", &json!({"name": "CM", "apn": "cmnet"}), None, &a));
}

#[test]
fn confirms_follow_the_old_page_and_english_has_no_chinese() {
    let a = Apn::of(&block(1));
    let c = confirm(&Act::Enable("manu2".into()), Some(&a), None, Lang::Zh).unwrap();
    assert!(c.title == "切换到「Work」？" && c.cuts_uplink && c.downtime.is_some());
    let a0 = Apn::of(&block(0));
    assert!(!confirm(&Act::Enable("manu2".into()), Some(&a0), None, Lang::Zh).unwrap().cuts_uplink);
    assert!(confirm(&Act::Delete("manu1".into()), Some(&a), None, Lang::Zh).is_err());
    assert!(confirm(&Act::Mode(true), Some(&a), None, Lang::Zh).unwrap().cuts_uplink);
    for act in [Act::Mode(true), Act::Mode(false), Act::V6(true), Act::V6(false), Act::Enable("manu2".into()), Act::Delete("manu2".into())] {
        let c = confirm(&act, Some(&a), None, Lang::En).unwrap();
        assert!(!cjk(&c.title) && !cjk(&c.what) && !cjk(&c.action), "{act:?}");
        assert!(!cjk(&applied(&act, Lang::En)));
    }
    let marks = Marks::of(&json!({"apn": {"picked_id": "manu2", "auto": [{"id": "auto1", "iot": true}]}}));
    let hub = hub_with(block(1), None);
    let v = hub.read(|st| view(st, Some(&marks), Some(&Ok(Wan { pdp: 3, live: true })), Lang::En));
    let html = Leaves { v: &v }.render().unwrap();
    assert!(!cjk(&html), "{html}");
    assert!(!cjk(&super::form(a.find("manu2"), Lang::En)));
}

fn ni(mode: &str, in_use: &str, picked: Option<&str>, switching: bool, err: &str) -> Marks {
    Marks::of(&json!({"apn": {"mode": mode, "in_use": {"apn": in_use}, "picked_id": picked, "switching": switching, "switch_error": err}}))
}

#[test]
fn use_for_this_sim_on_carrier_rows() {
    let hub = hub_with(block(0), None);
    // no netinfo yet: no button
    let v = hub.read(|st| view(st, None, None, Lang::Zh));
    assert!(v.auto.iter().all(|r| !r.can_pick));
    // auto mode, nothing picked: both carrier rows offer it
    let m = ni("auto", "ctiot", None, false, "");
    let v = hub.read(|st| view(st, Some(&m), None, Lang::Zh));
    assert!(v.auto.iter().all(|r| r.can_pick) && v.manual.iter().all(|r| !r.can_pick));
    let html = Leaves { v: &v }.render().unwrap();
    assert!(html.contains("/router/apn/pick/ask?id=auto2") && html.contains("aria-label=\"只给这张卡用 ctnet\""), "{html}");
    assert!(html.contains("运营商库里这张卡有好几条 APN"));
    // this SIM's pick dials ctnet: that row is marked, no button there
    let m = ni("manual", "ctnet", Some("manu9"), false, "");
    let v = hub.read(|st| view(st, Some(&m), None, Lang::Zh));
    assert!(v.auto[0].can_pick && !v.auto[1].can_pick);
    assert_eq!(v.auto[1].marks, vec!["本卡在用"]);
    // switching: off
    let v = hub.read(|st| view(st, Some(&ni("auto", "ctiot", None, true, "")), None, Lang::Zh));
    assert!(v.picking && Leaves { v: &v }.render().unwrap().contains("ctiot for this SIM\" disabled") == false);
    assert!(Leaves { v: &v }.render().unwrap().contains("只给这张卡用 ctiot\" disabled"));
    // one carrier profile: nothing to choose
    let mut one = block(0);
    one["auto"].as_array_mut().unwrap().truncate(1);
    let v = hub_with(one, None).read(|st| view(st, Some(&m), None, Lang::Zh));
    assert!(!v.auto[0].can_pick);
}

#[test]
fn pick_ids_confirm_and_readback() {
    assert!(auto_id("auto109600") && !auto_id("auto") && !auto_id("manu1") && !auto_id("auto1&x=1"));
    let a = Apn::of(&block(0));
    let c = confirm(&Act::Pick("auto2".into()), Some(&a), None, Lang::Zh).unwrap();
    assert!(c.title == "只给这张卡用 ctnet？" && c.cuts_uplink && c.downtime.is_some() && c.recovery.is_some() && c.action == "只给这张卡用");
    let c = confirm(&Act::Pick("auto2".into()), Some(&a), None, Lang::En).unwrap();
    assert!(!cjk(&c.title) && !cjk(&c.what) && !cjk(&c.action) && !cjk(&applied(&Act::Pick("auto2".into()), Lang::En)));
    assert!(confirm(&Act::Pick("auto9".into()), Some(&a), None, Lang::Zh).is_err());
    assert_eq!(pick_state(&ni("manual", "ctnet", Some("manu9"), true, ""), "ctnet"), None, "still switching");
    assert_eq!(pick_state(&ni("manual", "ctnet", Some("manu9"), false, ""), "ctnet"), Some(Ok(())));
    assert_eq!(pick_state(&ni("auto", "ctiot", None, false, "拨号失败"), "ctnet"), Some(Err("拨号失败".into())));
    assert_eq!(pick_state(&ni("auto", "ctiot", None, false, ""), "ctnet"), None);
    let html = area(&Act::Pick("auto2".into()), Lang::En, false, &write::Phase::Ask(c));
    assert!(html.contains("/router/apn/pick/set?id=auto2") && html.contains("ap-pick"));
}
