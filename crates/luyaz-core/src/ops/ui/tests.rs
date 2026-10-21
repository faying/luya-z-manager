//! 状态文案表（DD7）逐行、三行进度、撤销（STATE_V2.md V2-35、V2-36、V2-38、V2-39）。

use super::*;
use crate::ops::txn::{Applied, Confirm, DataPath, NewTxn, Reading, SimId};

fn sim() -> SimId {
    SimId {
        iccid: "89860000000000000001".into(),
        slot: 1,
    }
}

/// 自动 → 只用 4G，网页发起，在等确认。
fn txn(rollback: bool) -> Txn {
    let mut t = Txn::new(
        NewTxn {
            op_id: "op1".into(),
            action: "network.set_mode".into(),
            item: NETWORK_MODE.into(),
            source: Source::Web,
            approved_by: Source::Web,
            undo: false,
            target: "Only_LTE".into(),
            old: "WL_AND_5G".into(),
            rollback_to: "WL_AND_5G".into(),
            sim: sim(),
            conn: None,
            confirm: Confirm::Registered,
            rollback_enabled: rollback,
            deadline_ms: 120_000,
            boot_id: "b".into(),
            receipt: false,
        },
        1_000,
    );
    t.begin_apply();
    t.applied(Applied::Done, 1_000);
    t
}

fn ended(phase: Phase, reason: Reason) -> Txn {
    let mut t = txn(true);
    t.phase = phase;
    t.reason = Some(reason);
    t
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str()
        .unwrap_or_else(|| panic!("{k} missing in {v}"))
}

/// manager docs/ui-glossary.md 的英文规则：ASCII（加两边共用的 ·），没有句号，没有 Please。
fn check_en(what: &str, en: &str) {
    assert!(
        en.chars().all(|c| c.is_ascii() || c == '·'),
        "{what}: non-English text in {en:?}"
    );
    assert!(!en.ends_with('.'), "{what}: trailing period in {en:?}");
    assert!(
        !en.to_ascii_lowercase().contains("please"),
        "{what}: {en:?}"
    );
}

#[test]
fn every_text_row_has_zh_and_en_within_budget() {
    use Phase::*;
    use Reason::*;
    // 进行中（首页大字 ≤ 10 个字符，DD15）
    let mut t = txn(true);
    t.phase = Applying;
    let v = view(&t, 2_000, Ctx::default());
    assert_eq!(
        (s(&v, "say_zh"), s(&v, "say_en")),
        ("正在换制式", "Switching")
    );
    assert_eq!(s(&v, "stay"), "live");
    assert!(v["mark"].is_null() && v["undo"].is_null() && v["next_zh"].is_null());
    let v = view(&txn(true), 19_000, Ctx::default());
    assert_eq!((s(&v, "say_zh"), s(&v, "say_en")), ("正在确认", "Checking"));
    assert_eq!(s(&v, "next_zh"), "{t} 后没通就退回到自动");
    assert_eq!(s(&v, "next_en"), "Back to Auto in {t} if no data");
    assert_eq!(v["remaining_ms"], 102_000);
    assert_eq!(v["can_revert"], true);
    assert_eq!(v["can_keep"], true);
    assert_eq!(
        (s(&v, "revert_label_zh"), s(&v, "revert_label_en")),
        ("退回自动", "Revert to Auto")
    );
    assert_eq!(
        (s(&v, "keep_label_zh"), s(&v, "keep_label_en")),
        ("保留只用 4G", "Keep 4G only")
    );
    // DD6：自动退回关着
    let v = view(&txn(false), 2_000, Ctx::default());
    assert_eq!(s(&v, "next_zh"), "还剩 {t} · 自动退回没开");
    assert_eq!(s(&v, "next_en"), "{t} left · auto revert off");
    let mut t = txn(true);
    t.phase = RollingBack;
    let v = view(&t, 2_000, Ctx::default());
    assert_eq!(
        (s(&v, "say_zh"), s(&v, "say_en")),
        ("正在退回", "Reverting")
    );
    assert_eq!(s(&v, "next_zh"), "退回到自动 · 还剩 {t}");
    assert_eq!(v["can_revert"], false);
    for w in ["Switching", "Checking", "Reverting"] {
        assert!(w.chars().count() <= 10, "{w}");
    }

    // 终态，一行一个（中、英、符号、停留）
    let rows: &[(Phase, Reason, &str, &str, &str, &str)] = &[
        (
            Confirmed,
            Verified,
            "已切到只用 4G",
            "Now 4G only",
            "ok",
            "brief",
        ),
        (
            Confirmed,
            UserKeep,
            "保留只用 4G · 没确认通",
            "Kept 4G only · unconfirmed",
            "warn",
            "sticky",
        ),
        (
            Confirmed,
            NoVerify,
            "已改 · 没有可确认的读数",
            "Saved · not checked",
            "ok",
            "brief",
        ),
        (
            Unverified,
            ApnNoData,
            "APN 已存 · 数据关着没法试",
            "APN saved · untested",
            "warn",
            "sticky",
        ),
        (
            Unverified,
            NoRollback,
            "没通 · 还是只用 4G",
            "No data · still 4G only",
            "bad",
            "sticky",
        ),
        (
            RolledBack,
            Timeout,
            "没通 · 已退回自动",
            "No data · back to Auto",
            "warn",
            "sticky",
        ),
        (
            RolledBack,
            UserRevert,
            "已退回自动",
            "Back to Auto",
            "ok",
            "brief",
        ),
        (
            RolledBack,
            RebootLoop,
            "重启了两次 · 已退回自动",
            "2 reboots · back to Auto",
            "warn",
            "sticky",
        ),
        (
            NotApplied,
            Ignored,
            "没切成 · 还是自动",
            "Didn't apply · still Auto",
            "warn",
            "sticky",
        ),
        (
            RollbackFailed,
            RollbackTimeout,
            "退回也没通",
            "Revert failed",
            "bad",
            "alert",
        ),
        (
            Cancelled,
            Superseded,
            "被新的改动接替",
            "Replaced by a newer change",
            "ok",
            "none",
        ),
        (
            Cancelled,
            Preempted,
            "已关数据 · 不再自动退回",
            "Data off · no auto revert",
            "warn",
            "sticky",
        ),
        (
            Cancelled,
            ManualChange,
            "别处改过 · 不再自动退回",
            "Changed elsewhere",
            "warn",
            "sticky",
        ),
        (
            Cancelled,
            Takeover,
            "手动接管 · 不再自动退回",
            "Manual override",
            "warn",
            "sticky",
        ),
        (
            Cancelled,
            SimChanged,
            "换过卡 · 不再自动退回",
            "SIM changed",
            "warn",
            "sticky",
        ),
        (
            Cancelled,
            OtherChange,
            "改了别的设置，不再自动退回",
            "Other change; no auto revert",
            "warn",
            "sticky",
        ),
    ];
    for &(phase, reason, zh, en, mark, stay) in rows {
        let v = view(&ended(phase, reason), 5_000, Ctx::default());
        let what = format!("{phase:?}/{reason:?}");
        assert_eq!(s(&v, "say_zh"), zh, "{what}");
        assert_eq!(s(&v, "say_en"), en, "{what}");
        assert_eq!(s(&v, "mark"), mark, "{what}");
        assert_eq!(s(&v, "stay"), stay, "{what}");
        assert_eq!(
            sticky(&ended(phase, reason)),
            matches!(stay, "sticky" | "alert")
        );
        check_en(&what, en);
        // 事务行：值用剩余宽度、末尾 …；这里只防明显过长。
        assert!(en.len() <= 32 && zh.chars().count() <= 20, "{what}");
        assert!(
            v["next_zh"].is_null() && v["remaining_ms"].is_null(),
            "{what}"
        );
    }

    // 来源显示名（DD14）、值的显示名
    let v = view(&txn(true), 2_000, Ctx::default());
    assert_eq!((s(&v, "source_zh"), s(&v, "source_en")), ("网页", "Web"));
    assert_eq!(
        (s(&v, "what_zh"), s(&v, "what_en")),
        ("制式", "Network mode")
    );
    assert_eq!((s(&v, "old_zh"), s(&v, "target_en")), ("自动", "4G only"));
    assert!(v["readback_zh"].is_null());
    for src in [
        Source::Screen,
        Source::Web,
        Source::Legacy,
        Source::Guard,
        Source::Scenario,
        Source::Scheduler,
        Source::Auto,
    ] {
        let (zh, en) = source_words(src);
        assert!(!zh.is_ascii());
        check_en("source", en);
    }
    assert_eq!(source_words(Source::Legacy), source_words(Source::Screen));

    // 重启过（DD12）
    let mut t = txn(true);
    t.boots = 1;
    let v = view(&t, 2_000, Ctx::default());
    assert_eq!(s(&v, "note_zh"), "重启过 · 重新确认");
    check_en("note", s(&v, "note_en"));
    // 拼接：中文挨中文不空格，挨数字、英文空一格
    assert_eq!(cat("退回到", "自动"), "退回到自动");
    assert_eq!(cat("退回到", "4G + 5G"), "退回到 4G + 5G");
    assert_eq!(
        cat("{t} 后没通就退回到", "只用 5G SA"),
        "{t} 后没通就退回到只用 5G SA"
    );
    assert_eq!(clock(102_000), "1:42");
    assert_eq!(clock(48_001), "0:49");
    assert_eq!(clock(0), "0:00");
}

#[test]
fn steps_follow_readings() {
    let read = |v: &str, registered: bool| Reading {
        value: Some(v.into()),
        registered,
        sim: sim(),
        data: Some(DataPath {
            expected: Some(false),
            ..DataPath::default()
        }),
        probe: None,
    };
    let done = |t: &Txn| -> Vec<bool> {
        view(t, 3_000, Ctx::default())["steps"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["done"].as_bool().unwrap())
            .collect()
    };
    let mut t = txn(true);
    assert_eq!(done(&t), [false, false, false]);
    let keys: Vec<_> = view(&t, 3_000, Ctx::default())["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| {
            (
                x["key"].as_str().unwrap().to_string(),
                x["zh"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        keys,
        [
            ("applied".to_string(), "设置已生效".to_string()),
            ("registered".into(), "已注册".into()),
            ("data".into(), "数据".into())
        ]
    );
    // 读回对上、还没注册
    t.on_reading(&read("Only_LTE", false), 3_000);
    assert_eq!(done(&t), [true, false, false]);
    // 注册上，不应当有数据：数据这一关也算过，确认
    t.on_reading(&read("Only_LTE", true), 5_000);
    assert_eq!(t.phase, Phase::Confirmed);
    assert_eq!(done(&t), [true, true, true]);
    let v = view(&t, 5_000, Ctx::default());
    assert_eq!(s(&v, "readback_zh"), "只用 4G");

    // 退回中看的是退回目标
    let mut t = txn(true);
    t.on_reading(&read("Only_LTE", true), 3_000);
    t.phase = Phase::RollingBack;
    assert!(!done(&t)[0]);
    t.readback = Some("WL_AND_5G".into());
    assert!(done(&t)[0]);
}

#[test]
fn undo_by_phase_and_reason() {
    use Phase::*;
    use Reason::*;
    let undo = |t: &Txn| view(t, 5_000, Ctx::default())["undo"].clone();
    for (p, r) in [
        (Confirmed, Verified),
        (Confirmed, UserKeep),
        (Unverified, NoRollback),
        (Unverified, ApnNoData),
    ] {
        let u = undo(&ended(p, r));
        assert_eq!(u["ok"], true, "{p:?}/{r:?}");
        assert!(u["why_zh"].is_null());
        assert_eq!((s(&u, "label_zh"), s(&u, "label_en")), ("撤销", "Undo"));
        assert_eq!(s(&u, "value"), "WL_AND_5G");
    }
    for (p, r, zh, en) in [
        (
            RolledBack,
            Timeout,
            "设置没变 · 不用撤销",
            "Nothing to undo",
        ),
        (
            NotApplied,
            Ignored,
            "设置没变 · 不用撤销",
            "Nothing to undo",
        ),
        (
            RollbackFailed,
            RollbackTimeout,
            "先再试一次退回",
            "Retry the revert instead",
        ),
        (Cancelled, SimChanged, "换过卡", "SIM changed"),
        (Cancelled, Superseded, "之后又改过", "Changed since"),
    ] {
        let u = undo(&ended(p, r));
        assert_eq!(u["ok"], false, "{p:?}/{r:?}");
        assert_eq!((s(&u, "why_zh"), s(&u, "why_en")), (zh, en), "{p:?}/{r:?}");
        check_en("why", en);
    }
    // 被打断的：写进去的目标值还在才能撤
    let mut t = ended(Cancelled, Preempted);
    t.readback = Some("Only_LTE".into());
    assert_eq!(undo(&t)["ok"], true);
    t.readback = Some("WL_AND_5G".into());
    assert_eq!(s(&undo(&t), "why_zh"), "之后又改过");
    t.readback = None;
    assert_eq!(undo(&t)["ok"], false);
    // 撤销过的再点叫「重做」
    let mut t = ended(Confirmed, Verified);
    t.undo = true;
    assert_eq!(s(&undo(&t), "label_zh"), "重做");
    assert_eq!(s(&undo(&t), "label_en"), "Redo");
}

#[test]
fn later_change_blocks_undo() {
    let t = ended(Phase::Confirmed, Reason::Verified);
    let u = view(&t, 5_000, Ctx { later_change: true })["undo"].clone();
    assert_eq!(u["ok"], false);
    assert_eq!(s(&u, "why_zh"), "之后又改过");
}

#[test]
fn busy_reply_says_who_and_what() {
    assert_eq!(
        busy_words(NETWORK_MODE, Source::Screen, 32_400),
        (
            "正在换制式（触屏发起，32 秒），稍等".to_string(),
            "Busy: network mode (Screen)".to_string()
        )
    );
    let (zh, en) = busy_words("netselect.session", Source::Web, 5_000);
    assert_eq!(zh, "正在搜网（网页发起，5 秒），稍等");
    assert_eq!(en, "Busy: network search (Web)");
}

#[test]
fn screen_op_lines() {
    // 进行中：倒计时换好
    let t = txn(true);
    let op = screen_op(Some(&t), None, 19_000).unwrap();
    assert_eq!(op.kind, ScreenOpKind::Live);
    assert_eq!(op.head, ("正在确认".into(), "Checking".into()));
    assert_eq!(op.hint.0, "1:42 后没通就退回到自动");
    assert_eq!(op.hint.1, "Back to Auto in 1:42 if no data");
    // 重启过的放前面
    let mut r = txn(true);
    r.boots = 1;
    let op = screen_op(Some(&r), None, 19_000).unwrap();
    assert_eq!(op.hint.0, "重启过 · 重新确认 · 1:42 后没通就退回到自动");
    // 常驻结果：没点「知道了」才有
    let done = ended(Phase::RolledBack, Reason::Timeout);
    let op = screen_op(None, Some((&done, false)), 9_000).unwrap();
    assert_eq!(op.kind, ScreenOpKind::Sticky);
    assert_eq!(op.hint.0, "没通 · 已退回自动");
    assert!(screen_op(None, Some((&done, true)), 9_000).is_none());
    // 3 秒类的不叠
    let ok = ended(Phase::Confirmed, Reason::Verified);
    assert!(screen_op(None, Some((&ok, false)), 9_000).is_none());
    // 退回也没通：占状态块，写现在的值、上次确认的值和下一步
    let mut bad = ended(Phase::RollbackFailed, Reason::RollbackTimeout);
    let op = screen_op(None, Some((&bad, false)), 9_000).unwrap();
    assert_eq!(op.kind, ScreenOpKind::Alert);
    assert_eq!(op.head, ("退回也没通".into(), "Failed".into()));
    assert_eq!(
        op.hint.0,
        "当前设置未知 · 上次确认是自动 · 再试一次退回或重启设备"
    );
    bad.readback = Some("Only_LTE".into());
    let op = screen_op(None, Some((&bad, false)), 9_000).unwrap();
    assert_eq!(
        op.hint.0,
        "现在是只用 4G · 上次确认是自动 · 再试一次退回或重启设备"
    );
    assert_eq!(
        op.hint.1,
        "Now 4G only · last good Auto · retry revert or restart"
    );
    check_en("alert hint", &op.hint.1);
}

#[test]
fn confirmed_proposal_words() {
    let w = |s, by| who_words(Who::new(s, Some(by)).unwrap());
    assert_eq!(
        w(Source::Mcp, Source::Screen),
        (
            "MCP 提议 · 你在触屏确认".to_string(),
            "Proposed by MCP · confirmed on the screen".to_string()
        )
    );
    assert_eq!(
        w(Source::Scenario, Source::Web),
        (
            "情景提议 · 你在网页确认".to_string(),
            "Proposed by Scene · confirmed on the web".to_string()
        )
    );
    // 相同时和以前一样
    assert_eq!(
        w(Source::Screen, Source::Screen),
        ("触屏".into(), "Screen".into())
    );
    assert_eq!(
        busy_words(
            NETWORK_MODE,
            Who::new(Source::Mcp, Some(Source::Web)).unwrap(),
            2_000
        )
        .0,
        "正在换制式（MCP 提议 · 你在网页确认，2 秒），稍等"
    );
}

#[test]
fn band_value_words() {
    assert_eq!(
        value_words(BAND_LTE, &json!("1,3")),
        ("B1 B3".into(), "B1 B3".into())
    );
    assert_eq!(
        value_words(BAND_NR, &json!({"sa":"78","nsa":"41,78"})),
        ("SA n78 · NSA n41 n78".into(), "SA n78 · NSA n41 n78".into())
    );
    let many = "1,2,3,4,5,7,8,18,19,20,26,28,29,32,34,38,39,40,41,42,43,48,66,71";
    let w = value_words(BAND_ALL, &json!({"lte": many, "sa":"78", "nsa":"78"}));
    assert_eq!(w.0, "4G 24 个频段 · SA n78 · NSA n78");
    assert_eq!(w.1, "4G 24 bands · SA n78 · NSA n78");
    assert_eq!(
        value_words(BAND_ALL, &crate::ops::spec::reset_marker()).0,
        "默认"
    );
}

#[test]
fn cell_value_words() {
    let nr = json!({"pci":"603","arfcn":"627264","band":"78"});
    assert_eq!(value_words(CELL_NR, &nr).0, "PCI 603 · 627264 · n78");
    assert_eq!(
        value_words(CELL_LTE, &json!({"pci":"12","earfcn":"1650"})).1,
        "PCI 12 · 1650"
    );
    assert_eq!(
        value_words(CELL_LTE, &crate::ops::spec::cell_off()).0,
        "不锁小区"
    );
}

#[test]
fn chill_and_non_rollbackable_ui_words() {
    let mut t = Txn::new(
        NewTxn {
            op_id: "c1".into(),
            action: "chill.exit".into(),
            item: crate::ops::chill::EXIT.into(),
            source: Source::Mcp,
            approved_by: Source::Mcp,
            undo: false,
            target: "direct_keep_ai".into(),
            old: "proxy".into(),
            rollback_to: "proxy".into(),
            sim: sim(),
            conn: None,
            confirm: Confirm::Chill,
            rollback_enabled: true,
            deadline_ms: 90_000,
            boot_id: "b".into(),
            receipt: false,
        },
        1_000,
    );
    t.begin_apply();
    let v = view(&t, 1_500, Ctx::default());
    assert_eq!(
        (s(&v, "say_zh"), s(&v, "what_zh"), s(&v, "what_en")),
        ("正在换 CHILL 出口", "CHILL 出口", "CHILL exit")
    );
    assert!(s(&v, "say_zh").len() < 32);
    assert_eq!(
        (s(&v, "old_zh"), s(&v, "target_zh"), s(&v, "target_en")),
        ("代理", "直连 · AI 不动", "Direct · AI stays")
    );
    let steps = v["steps"].as_array().unwrap();
    assert_eq!(steps[1]["zh"], "CHILL 在运行");
    assert_eq!(steps[2]["zh"], "出口连通");
    assert_eq!(
        value_words(crate::ops::chill::PROFILE, &json!("eco")),
        ("省电".into(), "Eco".into())
    );

    // 没有旧值可退回：验证期不能点退回，终态也不能点撤销
    t.applied(Applied::Done, 1_500);
    t.rollback_to = Value::Null;
    t.old = Value::Null;
    let v = view(&t, 2_000, Ctx::default());
    assert_eq!(v["can_revert"], false);
    assert_eq!(v["can_keep"], true);
    t.phase = Phase::Confirmed;
    t.reason = Some(Reason::Verified);
    let v = view(&t, 3_000, Ctx::default());
    assert_eq!(v["undo"]["ok"], false);
    assert_eq!(s(&v["undo"], "why_zh"), "没有旧值可退回");
}

#[test]
fn dual_sim_words_say_mode_and_slot() {
    use serde_json::json;
    let w = |v| value_words(super::super::spec::DUAL_SIM, &v);
    assert_eq!(w(json!({"mode":"single","active_slot":1})).0, "单卡 · 卡槽 1");
    assert_eq!(w(json!({"mode":"dual","smart":false,"active_slot":2})).1, "Dual SIM · main slot 2");
    assert_eq!(w(json!({"mode":"dual","smart":true})).0, "双卡 · 智能切换");
}
