use super::*;
use crate::hub::Hub;
use crate::sse::Frame;
use std::sync::Arc;

fn blk(data: Value) -> Value {
    json!({"revision": 1, "observed_at": 1, "stale": false, "data": data})
}

fn sig(nr: &str, lte: &str) -> Value {
    json!({"nr_cell_lock": nr, "lte_cell_lock": lte, "nr_rsrp": -90, "nr_band": "n78", "nr_pci": 603,
           "nr_channel": 627264, "nr_cell_id": 12345, "lteca": "", "nrca": ""})
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
    s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c) || ('\u{3000}'..='\u{303f}').contains(&c) || ('\u{ff00}'..='\u{ffef}').contains(&c))
}

#[test]
fn locks_parse_like_u60d() {
    assert_eq!(parse("", true), None);
    assert_eq!(parse("0,0,0", true), None);
    assert_eq!(parse("603,627264,78", true), Some(Lock { pci: "603".into(), arfcn: "627264".into(), band: "78".into() }));
    assert_eq!(parse("603, 627264, n78", true).unwrap().band, "78");
    assert_eq!(parse("1,1300", false), Some(Lock { pci: "1".into(), arfcn: "1300".into(), band: String::new() }));
    assert_eq!(parse("1,1300", true), None);
    assert_eq!(parse("x,1", false), None);
    assert_eq!(num("007"), Some("7".into()));
    assert_eq!(num(" "), None);
}

#[test]
fn view_shows_cell_locks_and_whether_the_modem_is_on_it() {
    let hub = hub_with(sig("", ""), None);
    let v = hub.read(|st| view(st, Lang::Zh));
    assert_eq!((v.rat.as_str(), v.pci.as_str(), v.arfcn.as_str(), v.band.as_str(), v.cell_id.as_str()), ("NR", "603", "627264", "n78", "12345"));
    assert_eq!((v.nr_lock.as_str(), v.lte_lock.as_str()), ("没锁", "没锁"));
    assert!(v.on_cell.is_none() && !v.dis);
    let hub = hub_with(sig("604,627264,78", "1,1300"), None);
    let v = hub.read(|st| view(st, Lang::Zh));
    assert_eq!(v.nr_lock, "PCI 604 · ARFCN 627264 · n78");
    assert_eq!(v.lte_lock, "PCI 1 · EARFCN 1300");
    assert_eq!(v.on_cell, Some(("调制解调器还在 PCI 603 上（锁定的是 604）".into(), false)));
    let hub = hub_with(sig("603,627264,78", ""), None);
    assert!(hub.read(|st| view(st, Lang::En)).on_cell.unwrap().1);
    assert_eq!(hub.read(band0), "78");
}

#[test]
fn a_cell_or_band_change_running_or_stuck_turns_it_off() {
    let op = json!({"active": {"op_id": "x", "item": "band.all", "say_zh": "正在恢复默认", "say_en": "Resetting"}});
    let hub = hub_with(sig("", ""), Some(op));
    let v = hub.read(|st| view(st, Lang::Zh));
    assert!(v.dis && v.txn.as_deref() == Some("正在恢复默认"));
    let op = json!({"active": {"op_id": "x", "item": "network.mode", "say_zh": "正在确认"}});
    assert!(!hub_with(sig("", ""), Some(op)).read(|st| view(st, Lang::Zh)).dis);
}

#[test]
fn check_wants_numbers_and_the_nr_band() {
    let q = |pci: &str, arfcn: &str, band: &str| CellQuery { pci: pci.into(), arfcn: arfcn.into(), band: band.into() };
    let k = check(Which::Nr, &q(" 603", "627264", "n78"), true, false, Lang::Zh).unwrap();
    assert_eq!(params(Which::Nr, &k), json!({"pci": "603", "arfcn": "627264", "band": "78"}));
    assert!(matches!(check(Which::Nr, &q("603", "627264", ""), true, false, Lang::Zh), Err(write::Outcome::NotYet(_))));
    assert!(check(Which::Nr, &q("1008", "1", "78"), true, false, Lang::Zh).is_err());
    assert!(check(Which::Lte, &q("1;x", "1", ""), true, false, Lang::Zh).is_err());
    let k = check(Which::Lte, &q("1", "1300", "ignored"), true, false, Lang::Zh).unwrap();
    assert_eq!(params(Which::Lte, &k), json!({"pci": "1", "earfcn": "1300"}));
    assert!(check(Which::Lte, &q("1", "1300", ""), true, true, Lang::Zh).is_err());
    assert!(check(Which::Reset, &q("", "", ""), false, false, Lang::Zh).is_err());
    assert_eq!(params(Which::Reset, &Lock::default()), json!({}));
}

#[test]
fn readback_by_the_signal_fields() {
    let k = Lock { pci: "603".into(), arfcn: "627264".into(), band: "78".into() };
    let hub = hub_with(sig("603,627264,78", ""), None);
    assert!(hub.read(|st| read_back(Which::Nr, &k, st)));
    assert!(!hub.read(|st| read_back(Which::Reset, &k, st)));
    let hub = hub_with(sig("0,0,0", ""), None);
    assert!(hub.read(|st| read_back(Which::Reset, &k, st)) && !hub.read(|st| read_back(Which::Nr, &k, st)));
}

#[test]
fn confirms_say_what_and_english_has_no_chinese() {
    let k = Lock { pci: "603".into(), arfcn: "627264".into(), band: "78".into() };
    let c = confirm(Which::Nr, &k, Lang::Zh);
    assert_eq!(c.title, "把 NR 锁定到 PCI 603？");
    assert!(c.cuts_uplink && c.tier == write::Tier::Dialog && c.recovery.is_some());
    let r = confirm(Which::Reset, &k, Lang::Zh);
    assert!(r.what.contains("频段和小区锁一起恢复") && r.recovery.is_none());
    for w in [Which::Nr, Which::Lte, Which::Reset] {
        let c = confirm(w, &k, Lang::En);
        assert!(!cjk(&c.title) && !cjk(&c.what) && !cjk(c.downtime.as_deref().unwrap()) && !cjk(&c.action), "{w:?}");
        assert!(!cjk(&applied(w, &k, Lang::En)));
    }
    let hub = hub_with(sig("604,627264,78", "1,1300"), None);
    let v = hub.read(|st| view(st, Lang::En));
    let html = Leaves { v: &v }.render().unwrap();
    assert!(!cjk(&html), "{html}");
}

#[test]
fn neighbours_in_the_cores_order_or_why_not() {
    let ok = json!({"state": "ok", "cells": [
        {"rat": "LTE", "band": "B3", "pci": 182, "arfcn": 1750, "rsrp": -99, "rsrq": -16, "sinr": null},
        {"rat": "LTE", "pci": 7, "arfcn": 1650, "rsrp": null, "rsrq": -19},
    ]});
    let r = neighbours(&ok, Lang::Zh).unwrap();
    assert_eq!(r[0], Nbr { pci: "182".into(), band: "B3".into(), arfcn: "1750".into(), rsrp: "-99".into(), rsrq: "-16".into() });
    assert!(r[1].band == "LTE" && r[1].rsrp == "-");
    let html = nbr_leaf(Some(&ok), Lang::En).1;
    assert!(html.starts_with(r#"<div id="cl-nbr">"#) && html.contains(r#"<td class="nd-mono">182</td>"#), "{html}");
    let sa = json!({"state": "not_reported", "error": "5G 独立组网（SA）下基带不报邻区", "error_en": "In 5G SA the modem reports no neighbours", "cells": []});
    assert_eq!(neighbours(&sa, Lang::En).unwrap_err(), "In 5G SA the modem reports no neighbours");
    assert_eq!(neighbours(&sa, Lang::Zh).unwrap_err(), "5G 独立组网（SA）下基带不报邻区");
    // an older core: unsupported with its reason; nothing at all
    assert_eq!(neighbours(&json!({"state": "unsupported", "error": "x"}), Lang::Zh).unwrap_err(), "x");
    assert_eq!(neighbours(&Value::Null, Lang::Zh).unwrap_err(), "基带现在没报邻区");
}
