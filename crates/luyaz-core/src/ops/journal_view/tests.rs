//! 改动记录的显示字段（STATE_V2.md V2-41）。

use super::*;

fn txn(op: &str, old: &str, new: &str, result: &str, reason: &str) -> Value {
    json!({"op_id": op, "action": "network.set_mode", "item": "network.mode", "source": "web",
           "undo": false, "sim": "0001/1", "old": old, "new": new, "rollback_to": old,
           "readback": new, "result": result, "reason": reason, "ts": 1, "t": "2026-10-03 14:32:00"})
}

fn get<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or_else(|| panic!("{k} in {v}"))
}

fn en_ok(s: &str) {
    assert!(
        s.chars()
            .all(|c| c.is_ascii() || matches!(c, '·' | '→' | '×')),
        "{s:?}"
    );
    assert!(!s.ends_with('.'), "{s:?}");
}

#[test]
fn every_entry_kind_reads_as_a_sentence() {
    let mut e = vec![
        json!({"source": "screen", "action": "op.ack", "op_id": "a", "item": "network.mode", "result": "ok"}),
        txn("b", "WL_AND_5G", "Only_LTE", "confirmed", "verified"),
        json!({"action": "cellular.set", "item": null, "source": "screen", "params": {"enabled": 0},
               "result": "ok", "status": 200}),
        json!({"source": "scenario", "item": "wifi", "result": "skipped", "reason": "user_hold", "skip": "end", "count": 5}),
        json!({"source": "scenario", "item": "wifi", "result": "skipped", "reason": "user_hold", "skip": "start"}),
        json!({"source": "web", "action": "esim.switch", "result": "ok", "old": "CMHK", "new": "Ubigi",
               "journal_append": true}),
        json!({"source": "auto", "item": "apn", "result": "skipped", "reason": "busy", "skip": "start"}),
        json!({"action": "device.reboot", "source": "web", "params": {}, "result": "requested"}),
        json!({"action": "wifi.apply", "source": "screen",
               "params": {"set": {"wireless.main_2g.disabled": "1", "wireless.main_5g.disabled": "1"}, "reload": true},
               "result": "failed", "status": 502}),
        json!({"action": "usb.attach_mode", "source": "screen", "params": {"mode": "share"}, "result": "ok", "status": 200}),
    ];
    decorate(&mut e);
    // 「知道了」只记账，不单独成行
    assert_eq!(e[0]["hide"], true);
    // 事务
    let t = &e[1];
    assert_eq!(
        (get(t, "what_zh"), get(t, "change_zh")),
        ("制式", "自动 → 只用 4G")
    );
    assert_eq!(
        (get(t, "result_zh"), get(t, "mark")),
        ("已切到只用 4G", "ok")
    );
    assert_eq!((get(t, "source_zh"), get(t, "source_en")), ("网页", "Web"));
    assert_eq!(t["undo_view"]["ok"], true);
    assert_eq!(get(&t["undo_view"], "label_zh"), "撤销");
    assert_eq!(
        t["undo_view"]["request"],
        json!({"action": "network.set_mode", "undo": true, "params": {"mode": "WL_AND_5G"}})
    );
    assert_eq!(t["undo"], false, "the line's own undo flag stays");
    // 不走事务的写
    let c = &e[2];
    assert_eq!(
        (get(c, "what_zh"), get(c, "change_zh"), get(c, "result_zh")),
        ("移动数据", "关掉数据", "已改")
    );
    assert_eq!(
        (get(c, "change_en"), get(c, "result_en")),
        ("data off", "Done")
    );
    assert!(c["undo_view"].is_null());
    // 跳过段：结束行带次数，它的开始行收起
    assert_eq!(get(&e[3], "result_zh"), "情景跳过 ×5（你手动改过）");
    assert_eq!(get(&e[3], "result_en"), "Scene skipped ×5 (you changed it)");
    assert_eq!(
        (e[3]["hide"].clone(), e[4]["hide"].clone()),
        (json!(false), json!(true))
    );
    // 只记账的 eSIM
    assert_eq!(
        (get(&e[5], "what_zh"), get(&e[5], "change_zh")),
        ("eSIM", "CMHK → Ubigi")
    );
    // 还在跳过的段
    assert_eq!(get(&e[6], "result_zh"), "自动跳过中（设备正忙）");
    assert_eq!(e[6]["hide"], false);
    assert_eq!(
        (get(&e[7], "what_zh"), get(&e[7], "result_zh")),
        ("重启", "已发出")
    );
    assert_eq!(
        (
            get(&e[8], "change_zh"),
            get(&e[8], "result_zh"),
            get(&e[8], "mark")
        ),
        ("关掉", "没改成", "bad")
    );
    assert_eq!(
        (
            get(&e[9], "what_zh"),
            get(&e[9], "change_zh"),
            get(&e[9], "change_en")
        ),
        ("插线用法", "充电 + 上网", "charge + internet")
    );
    for x in &e {
        for k in ["what_en", "change_en", "result_en", "source_en"] {
            en_ok(get(x, k));
        }
    }
}

#[test]
fn only_the_latest_change_of_an_item_can_be_undone() {
    let mut e = vec![
        txn("new", "Only_LTE", "Only_5G", "confirmed", "user_keep"),
        txn("old", "WL_AND_5G", "Only_LTE", "confirmed", "verified"),
        txn("rb", "WL_AND_5G", "Only_LTE", "rolled_back", "timeout"),
    ];
    e[0]["undo"] = json!(true);
    decorate(&mut e);
    assert_eq!(e[0]["undo_view"]["ok"], true);
    assert_eq!(get(&e[0]["undo_view"], "label_zh"), "重做");
    assert_eq!(get(&e[0], "result_zh"), "保留只用 5G SA · 没确认通");
    assert_eq!(e[1]["undo_view"]["ok"], false);
    assert_eq!(get(&e[1]["undo_view"], "why_zh"), "之后又改过");
    assert_eq!(get(&e[2]["undo_view"], "why_zh"), "之后又改过");
    // 单独一条退回了的：设置没变
    let mut one = vec![txn("rb", "WL_AND_5G", "Only_LTE", "rolled_back", "timeout")];
    decorate(&mut one);
    assert_eq!(get(&one[0]["undo_view"], "why_zh"), "设置没变 · 不用撤销");
    assert_eq!(get(&one[0], "result_zh"), "没通 · 已退回自动");
    // 不走事务的写成功了，也算「之后又改过」
    let mut e = vec![
        json!({"action": "network.set_mode", "item": "network.mode", "source": "legacy",
               "params": {"mode": "Only_LTE"}, "result": "ok"}),
        txn("t", "WL_AND_5G", "Only_5G", "confirmed", "verified"),
    ];
    decorate(&mut e);
    assert_eq!(get(&e[0], "change_zh"), "改成只用 4G");
    assert_eq!(get(&e[1]["undo_view"], "why_zh"), "之后又改过");
}

#[test]
fn confirmed_proposals_say_both() {
    let mut a = txn("m", "WL_AND_5G", "Only_LTE", "confirmed", "verified");
    a["source"] = json!("mcp");
    a["approved_by"] = json!("screen");
    // 读不懂的批准者（不该出现）：只按来源写
    let mut b = txn("n", "Only_LTE", "WL_AND_5G", "confirmed", "verified");
    b["approved_by"] = json!("guard");
    let mut e = vec![
        a,
        b,
        json!({"action":"wifi.apply","source":"screen","result":"lock_wait","reason":"wifi_lock"}),
    ];
    decorate(&mut e);
    assert_eq!(get(&e[0], "source_zh"), "MCP 提议 · 你在触屏确认");
    assert_eq!(
        get(&e[0], "source_en"),
        "Proposed by MCP · confirmed on the screen"
    );
    assert_eq!(get(&e[1], "source_zh"), "网页");
    assert_eq!(get(&e[2], "result_zh"), "等 Wi-Fi 锁超时，重试");
    en_ok(get(&e[2], "result_en"));
}

#[test]
fn chill_and_blank_old_entries() {
    let mut e = vec![
        json!({"op_id": "c1", "action": "chill.exit", "item": "chill.exit", "source": "mcp",
               "approved_by": "mcp", "undo": false, "old": "proxy", "new": "direct_keep_ai",
               "rollback_to": "proxy", "readback": "direct_keep_ai",
               "result": "confirmed", "reason": "verified"}),
        json!({"op_id": "n1", "action": "network.set_mode", "item": "network.mode", "source": "web",
               "undo": false, "old": null, "new": "Only_LTE", "rollback_to": null,
               "readback": "Only_LTE", "result": "confirmed", "reason": "verified"}),
    ];
    decorate(&mut e);
    assert_eq!(
        (get(&e[0], "what_zh"), get(&e[0], "change_zh")),
        ("CHILL 出口", "代理 → 直连 · AI 不动")
    );
    assert_eq!(e[1]["undo_view"]["ok"], false);
    assert_eq!(get(&e[1]["undo_view"], "why_zh"), "没有旧值可退回");
}
