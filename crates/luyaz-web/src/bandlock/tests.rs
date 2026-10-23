use super::*;
use crate::hub::Hub;
use crate::sse::Frame;
use std::sync::Arc;

const ALL_LTE: &str = "1,2,3,4,5,7,8,18,19,20,26,28,29,32,34,38,39,40,41,42,43,48,66,71";
const ALL_NR: &str = "1,2,3,5,7,8,18,20,26,28,29,38,40,41,48,66,71,75,77,78,79";

fn blk(data: Value) -> Value {
    json!({"revision": 1, "observed_at": 1, "stale": false, "data": data})
}

/// The recorded device (not locked), with the given locks.
fn sig(lte: &str, sa: &str, nsa: &str) -> Value {
    json!({"lte_bands": lte, "sa_bands": sa, "nsa_bands": nsa,
           "lte_supported_bands": ALL_LTE, "nr_sa_supported_bands": ALL_NR, "nr_nsa_supported_bands": ALL_NR,
           "nr_rsrp": -90, "nr_band": "n78", "nr_pci": 1, "nr_channel": 2, "lteca": "", "nrca": ""})
}

fn hub_with(signal: Value, op: Option<Value>) -> Arc<Hub> {
    let hub = Hub::new();
    let mut blocks = json!({"signal": blk(signal)});
    if let Some(o) = op {
        blocks["op"] = blk(o);
    }
    hub.apply(&Frame {
        event: "snapshot".into(),
        data: json!({"epoch": "e", "blocks": blocks}).to_string(),
    });
    hub.set_link_for_test(Link::Live);
    hub
}

fn cjk(s: &str) -> bool {
    s.chars().any(|c| {
        ('\u{4e00}'..='\u{9fff}').contains(&c)
            || ('\u{3000}'..='\u{303f}').contains(&c)
            || ('\u{ff00}'..='\u{ffef}').contains(&c)
    })
}

#[test]
fn canonical_lists_like_u60d() {
    assert_eq!(canon(" 78, 41,78,1 "), Some(vec![1, 41, 78]));
    assert_eq!(canon(""), Some(vec![]));
    assert_eq!(canon("n78"), None);
    assert_eq!(join(&[1, 3]), "1,3");
}

#[test]
fn unlocked_reads_as_not_locked_and_preselects_everything() {
    let hub = hub_with(sig(ALL_LTE, ALL_NR, ALL_NR), None);
    let v = hub.read(|st| view(st, Lang::Zh));
    assert!(!v.dis && !v.split);
    assert_eq!(v.locked, "没锁（全部频段）");
    assert_eq!(v.in_use, "n78");
    assert_eq!(v.lte.len(), 24);
    assert_eq!(v.nr.len(), 21);
    assert!(v.lte.iter().all(|c| c.on) && v.nr.iter().all(|c| c.on));
    assert_eq!(v.lnow, ALL_LTE);
    assert_eq!(v.nnow, ALL_NR);
    let html = Leaves { v: &v }.render().unwrap();
    assert!(html.contains("role=\"toolbar\"") && html.contains("aria-label=\"LTE 频段\""));
    assert!(html.contains(">B66</button>") && html.contains(">n78</button>"));
    assert!(html.contains("data-signals__ifmissing"));
}

#[test]
fn current_locks_are_shown_and_preselected() {
    let hub = hub_with(sig("3,1", "78", "78"), None);
    let v = hub.read(|st| view(st, Lang::Zh));
    assert_eq!(v.locked, "SA n78 · NSA n78 · LTE B1 B3");
    assert_eq!(v.lnow, "1,3");
    assert_eq!(v.nnow, "78");
    let on: Vec<&str> = v.lte.iter().filter(|c| c.on).map(|c| c.label.as_str()).collect();
    assert_eq!(on, ["B1", "B3"]);
    assert_eq!(v.nr.iter().filter(|c| c.on).count(), 1);
    // the draft and the selection expression agree on keys
    assert!(v.draft_lte().starts_with("{b1: true, b2: false, b3: true"));
    assert!(v.sel_lte().starts_with("[$_lte.b1 && '1', $_lte.b2 && '2'"));
    let en = hub.read(|st| view(st, Lang::En));
    assert_eq!(en.locked, "SA n78 · NSA n78 · LTE B1 B3");
    // a default part reads 「全部」
    let hub = hub_with(sig(ALL_LTE, "78", ALL_NR), None);
    assert_eq!(hub.read(|st| view(st, Lang::Zh)).locked, "SA n78 · NSA 全部 · LTE 全部");
}

#[test]
fn sa_and_nsa_apart_start_from_sa_and_say_so() {
    let hub = hub_with(sig(ALL_LTE, "78", "41,78"), None);
    let v = hub.read(|st| view(st, Lang::Zh));
    assert!(v.split);
    assert_eq!(v.nnow, "-", "never equal: Apply stays on");
    assert_eq!(v.nr.iter().filter(|c| c.on).count(), 1);
    let html = Leaves { v: &v }.render().unwrap();
    assert!(html.contains("SA 和 NSA 现在锁的频段不一样"));
    let en = Leaves { v: &hub.read(|st| view(st, Lang::En)) }.render().unwrap();
    assert!(!cjk(&en), "{en}");
}

#[test]
fn nr_choices_are_both_lists() {
    let b = Bands::of(&json!({"nr_sa_supported_bands": "41,78,79", "nr_nsa_supported_bands": "1,41,78"}));
    assert_eq!(b.nr_choices(), [41, 78]);
    let b = Bands::of(&json!({"nr_sa_supported_bands": "", "nr_nsa_supported_bands": "1,41"}));
    assert_eq!(b.nr_choices(), [1, 41]);
}

#[test]
fn buttons_off_while_stuck_or_a_band_change_runs() {
    let hub = hub_with(sig(ALL_LTE, ALL_NR, ALL_NR), Some(json!({"active": {"op_id": "x", "item": "band.nr",
        "say_zh": "正在确认", "say_en": "Checking", "next_zh": "{t} 内确认", "next_en": "confirming within {t}",
        "remaining_ms": 61000}, "last": null})));
    let v = hub.read(|st| view(st, Lang::Zh));
    assert!(v.dis);
    assert_eq!(v.txn.as_deref(), Some("正在确认 · 1:01 内确认"));
    // another item's change does not count
    let hub = hub_with(sig(ALL_LTE, ALL_NR, ALL_NR), Some(json!({"active": {"op_id": "x", "item": "network.mode"}})));
    assert!(!hub.read(|st| view(st, Lang::Zh)).dis);
    // stuck: off, with the reason, and not sent
    let hub = hub_with(sig(ALL_LTE, ALL_NR, ALL_NR), None);
    hub.apply(&Frame { event: "heartbeat".into(), data: json!({"epoch": "e", "seq": 1, "blocks": {}, "exec_age_ms": 60000}).to_string() });
    let v = hub.read(|st| view(st, Lang::Zh));
    assert!(v.dis && v.why.as_deref() == Some("数据服务没响应 · 暂时不能改设置"), "{:?}", v.why);
    let b = hub.read(bands);
    assert!(matches!(check(Which::Lte, "1", b.as_ref(), true, Lang::Zh), Err(write::Outcome::NotYet(_))));
}

#[test]
fn nothing_read_yet_is_not_sent() {
    let v = Hub::new().read(|st| view(st, Lang::Zh));
    assert!(v.dis);
    assert_eq!(v.locked, "—");
    assert!(matches!(check(Which::Reset, "", None, false, Lang::Zh), Err(write::Outcome::NotYet(_))));
}

#[test]
fn what_is_sent_is_checked_again() {
    let b = Bands::of(&sig(ALL_LTE, ALL_NR, ALL_NR));
    let l = Lang::Zh;
    assert_eq!(check(Which::Lte, "3,1,3", Some(&b), false, l), Ok(vec![1, 3]));
    assert!(matches!(check(Which::Lte, "", Some(&b), false, l), Err(write::Outcome::NotYet(s)) if s == "至少勾一个 LTE 频段"));
    assert!(matches!(check(Which::Lte, "1;3", Some(&b), false, l), Err(write::Outcome::Failed(_))));
    assert!(matches!(check(Which::Lte, "6", Some(&b), false, l), Err(write::Outcome::Failed(s)) if s.contains("频段 6")));
    assert!(matches!(check(Which::Nr, "4", Some(&b), false, l), Err(write::Outcome::Failed(_))));
    assert_eq!(check(Which::Reset, "anything", Some(&b), false, l), Ok(vec![]));
    assert_eq!(params(Which::Lte, &[1, 3]), json!({"bands": "1,3"}));
    assert_eq!(params(Which::Nr, &[41, 78]), json!({"sa": "41,78", "nsa": "41,78"}), "one choice for SA and NSA");
    assert_eq!(params(Which::Reset, &[]), json!({}));
}

#[test]
fn confirms_are_tier_3_and_reset_names_the_cell_lock() {
    for l in [Lang::Zh, Lang::En] {
        for w in [Which::Lte, Which::Nr, Which::Reset] {
            let c = confirm(w, &[1, 3], l);
            assert_eq!(c.tier, write::Tier::Dialog);
            assert!(c.cuts_uplink && c.downtime.is_some());
            if l == Lang::En {
                assert!(!cjk(&format!("{}{}{:?}{}", c.title, c.what, c.recovery, c.action)));
            }
        }
    }
    assert_eq!(confirm(Which::Lte, &[1, 3], Lang::Zh).title, "把 LTE 锁定到 B1 B3？");
    assert!(confirm(Which::Nr, &[78], Lang::Zh).what.contains("SA 和 NSA 都是"));
    assert!(confirm(Which::Reset, &[], Lang::Zh).what.starts_with("频段和小区锁一起恢复"));
    assert!(confirm(Which::Reset, &[], Lang::En).what.contains("Bands and cell lock are both reset"));
}

#[test]
fn readback_and_outcomes() {
    let b = Bands::of(&sig("1,3", "78", "78"));
    assert!(read_back(Which::Lte, &[1, 3], &b));
    assert!(read_back(Which::Nr, &[78], &b));
    assert!(!read_back(Which::Reset, &[], &b));
    assert!(read_back(Which::Reset, &[], &Bands::of(&sig(ALL_LTE, ALL_NR, ALL_NR))));
    let fin = Seen::Final(json!({"say_zh": "没通 · 已退回 4G 24 个频段", "mark": "warn"}));
    let o = outcome(&fin, write::Sent::Ok, true, Which::Lte, &[1], Lang::Zh);
    assert_eq!(o, write::Outcome::Ended("没通 · 已退回 4G 24 个频段".into(), "warn"));
    let fin = Seen::Final(json!({"say_zh": "已改成B1", "mark": "ok"}));
    assert_eq!(outcome(&fin, write::Sent::Ok, false, Which::Lte, &[1], Lang::Zh).tone(), "ok");
    let live = Seen::Live(json!({"say_zh": "正在确认", "say_en": "Checking"}));
    let t = outcome(&live, write::Sent::Ok, false, Which::Lte, &[1], Lang::Zh).text(Lang::Zh);
    assert!(t.starts_with("设备已接受") && !t.contains("横条") && !t.contains("成功"), "{t}");
    assert_eq!(
        outcome(&Seen::Nothing, write::Sent::Ok, true, Which::Nr, &[78], Lang::Zh),
        write::Outcome::Applied("NR 已锁定到 n78".into())
    );
    assert_eq!(outcome(&Seen::Nothing, write::Sent::NoReply, false, Which::Nr, &[78], Lang::Zh), write::Outcome::Unknown);
}

#[test]
fn band_rows_can_be_undone_on_changes_with_the_band_wording() {
    use crate::changes::{UNDO_ACTIONS, undo_body};
    for a in ["band.set_lte", "band.set_nr", "band.reset"] {
        assert!(UNDO_ACTIONS.contains(&a));
        assert_eq!(crate::changes::tier(a), write::Tier::Dialog);
    }
    let e = json!({"op_id": "web-40", "action": "band.reset", "item": "band.all", "what_zh": "频段和小区锁",
        "change_zh": "SA n78 → 默认", "undo_view": {"ok": true, "label_zh": "撤销",
        "request": {"action": "band.reset", "undo": true, "params": {"lte": "1", "sa": "78", "nsa": "78"}}}});
    let b = undo_body(&e, "lw-1-1", false, Lang::Zh).unwrap();
    assert_eq!(b["action"], "band.reset");
    assert_eq!(b["params"]["sa"], "78");
    let c = crate::changes::confirm(&e, Lang::Zh);
    assert!(c.recovery.unwrap().contains("频段锁定"));
}
