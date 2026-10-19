use super::*;
use crate::hub::Hub;
use crate::sse::Frame;
use std::sync::Arc;

fn blk(data: Value) -> Value {
    json!({"revision": 1, "observed_at": 1, "stale": false, "data": data})
}

fn hub_with(blocks: Value) -> Arc<Hub> {
    let hub = Hub::new();
    let mut b = serde_json::Map::new();
    for (k, v) in blocks.as_object().unwrap() {
        b.insert(k.clone(), blk(v.clone()));
    }
    hub.apply(&Frame { event: "snapshot".into(), data: json!({"epoch": "e", "blocks": b}).to_string() });
    hub.set_link_for_test(Link::Live);
    hub
}

fn cjk(s: &str) -> bool {
    s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c) || ('\u{3000}'..='\u{303f}').contains(&c) || ('\u{ff00}'..='\u{ffef}').contains(&c))
}

#[test]
fn modes_like_the_old_page() {
    assert!(is_auto("TCHGWL_5G") && is_auto("NETWORK_auto") && !is_auto("Only_LTE"));
    assert_eq!(mode_label("TCHGWL_5G", Lang::Zh), "自动（全部制式）");
    assert_eq!(mode_label("Only_5G", Lang::Zh), "只用 5G SA");
    assert_eq!(mode_label("XYZ", Lang::En), "XYZ");
    let hub = hub_with(json!({"signal": {"net_select": "TCHGWL_5G", "net_select_mode": "manual_select"}}));
    let v = hub.read(|st| mode_view(st, Lang::Zh));
    assert_eq!((v.now.as_str(), v.cur.as_str()), ("自动（全部制式）", "WL_AND_5G"));
    assert!(v.manual_sel && !v.dis);
    let op = json!({"active": {"op_id": "x", "item": "network.mode", "say_zh": "正在确认"}});
    let hub = hub_with(json!({"signal": {"net_select": "Only_LTE"}, "op": op}));
    let v = hub.read(|st| mode_view(st, Lang::Zh));
    assert!(v.dis && v.txn.as_deref() == Some("正在确认"));
}

#[test]
fn mode_confirm_warns_and_says_rollback_off() {
    let c = mode_confirm("Only_5G", true, Lang::Zh);
    assert_eq!(c.title, "切换到「只用 5G SA」？");
    assert!(c.what.contains("很多国家还没有 5G SA") && c.what.contains("自动退回没开") && c.cuts_uplink);
    assert!(!mode_confirm("Only_LTE", false, Lang::Zh).what.contains("自动退回"));
    for m in MODES {
        let c = mode_confirm(m.0, true, Lang::En);
        assert!(!cjk(&c.title) && !cjk(&c.what) && !cjk(&c.action));
    }
    let hub = hub_with(json!({"signal": {"net_select": "Only_LTE"}}));
    assert!(hub.read(|st| mode_check("Only_GSM", st, Lang::Zh)).is_err());
    assert!(hub.read(|st| mode_check("WL_AND_5G", st, Lang::Zh)).is_ok());
}

#[test]
fn data_switch_reads_like_core() {
    let c = |enable: Value, status: &str| {
        hub_with(json!({"interfaces": {"cellular": {"enable": enable, "connect_status": status, "roam_enable": 1}}})).read(cell).unwrap()
    };
    assert_eq!(c(json!(1), "disconnected").data, Some(true));
    assert_eq!(c(json!(0), "ipv4_ipv6_connected").data, Some(true), "0 after boot with the call up is on");
    assert_eq!(c(json!(0), "disconnected").data, Some(false));
    assert_eq!(c(json!(0), "").data, None);
    assert_eq!(c(json!(1), "").roam, Some(true));
    assert_eq!(families("ipv4_ipv6_connected"), "IPv4 + IPv6");
    assert_eq!(connect_kind("ipv4_connecting"), "connecting");
}

#[test]
fn net_view_status_jobs_and_results() {
    let hub = hub_with(json!({"interfaces": {"cellular": {"enable": 1, "connect_status": "ipv4_ipv6_connected", "roam_enable": 1}}}));
    let ni = json!({"scan": {"state": "done", "finished_at": 100, "operators": [
        {"status": "2", "name": "中华电信", "operator_en": "Chunghwa", "plmn": "46692", "rat": "11"},
        {"status": "3", "name": "X", "operator_en": "X", "plmn": "466'97", "rat": "7;"}]},
        "guard": {"phase": "idle"}});
    let j = Ok(Jobs::of(&ni, Lang::En));
    let v = hub.read(|st| net_view(st, Some(&j), Lang::En));
    assert_eq!((v.tone, v.state.as_str()), ("ok", "Connected"));
    assert_eq!(v.reason, "IPv4 + IPv6 · Data roaming is allowed.");
    let ops = v.ops.clone().unwrap();
    assert_eq!(ops[0].name, "Chunghwa");
    assert_eq!((ops[1].plmn.as_str(), ops[1].rat.as_str()), ("46697", "7"), "only digits / letters go into the URL");
    assert!(!v.sel_dis && !v.radio_dis);
    let html = NetLeaves { v: &v }.render().unwrap();
    assert!(!cjk(&html), "{html}");
    // a register since the scan hides the stale list; a running job turns things off
    let ni = json!({"scan": {"state": "done", "finished_at": 100, "operators": []}, "guard": {"phase": "ok", "started_at": 120}});
    assert!(Jobs::of(&ni, Lang::Zh).shown().is_none());
    let ni = json!({"scan": {"state": "scanning"}, "guard": {"phase": "idle"}});
    let j = Ok(Jobs::of(&ni, Lang::Zh));
    let v = hub.read(|st| net_view(st, Some(&j), Lang::Zh));
    assert!(v.sel_dis && v.radio_dis && v.job.as_deref().is_some_and(|t| t.contains("搜索期间移动数据会断开")));
    // no agent answer: scan off, data/roaming still on
    let v = hub.read(|st| net_view(st, Some(&Err(Fail::Unreachable)), Lang::Zh));
    assert!(v.sel_dis && !v.radio_dis && v.ni_err.as_deref() == Some("设备服务没有回应"));
}

#[test]
fn net_confirms_by_tier() {
    assert_eq!(net_confirm(&Act::Data(false), Via::Local, "", Lang::Zh).tier, write::Tier::Inline);
    let c = net_confirm(&Act::Data(false), Via::Tailnet, "", Lang::Zh);
    assert!(c.tier == write::Tier::Dialog && c.cuts_uplink && c.downtime.is_some());
    assert_eq!(net_confirm(&Act::Data(true), Via::Tailnet, "", Lang::Zh).tier, write::Tier::Inline);
    let r = net_confirm(&Act::Register { plmn: "46692".into(), rat: "11".into() }, Via::Local, "中华电信", Lang::Zh);
    assert!(r.title == "注册到 中华电信？" && r.what.contains("（46692）") && r.tier == write::Tier::Dialog);
    assert_eq!(net_confirm(&Act::Scan, Via::Local, "", Lang::Zh).tier, write::Tier::Dialog);
    assert_eq!(net_confirm(&Act::Auto, Via::Tailnet, "", Lang::Zh).tier, write::Tier::Inline);
    assert!(reg_ok("46692", "11") && !reg_ok("4669", "11") && !reg_ok("46692", "1;1"));
    for a in [Act::Data(true), Act::Data(false), Act::Roam(true), Act::Roam(false), Act::Scan, Act::Auto, Act::Reboot] {
        let c = net_confirm(&a, Via::Tailnet, "X", Lang::En);
        assert!(!cjk(&c.title) && !cjk(&c.what) && !cjk(&c.action), "{a:?}");
    }
}
