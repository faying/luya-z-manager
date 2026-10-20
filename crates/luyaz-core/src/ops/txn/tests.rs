//! 状态机用例矩阵（write-op-layer.md 测试计划 Critical Paths 第 1 条，纯逻辑部分）。

use super::*;

const D: u64 = 120_000;

fn sim() -> SimId {
    SimId {
        iccid: "89860000000000000001".into(),
        slot: 1,
    }
}

fn txn(rollback: bool) -> Txn {
    let mut t = Txn::new(
        NewTxn {
            op_id: "op1".into(),
            action: "network.set_mode".into(),
            item: "network.mode".into(),
            source: Source::Screen,
            approved_by: Source::Screen,
            undo: false,
            target: "Only_LTE".into(),
            old: "WL_AND_5G".into(),
            rollback_to: "WL_AND_5G".into(),
            sim: sim(),
            conn: Some(conn("10.0.0.1", 500)),
            confirm: Confirm::Registered,
            rollback_enabled: rollback,
            deadline_ms: D,
            boot_id: "boot-a".into(),
            receipt: false,
        },
        1_000,
    );
    t.begin_apply();
    assert_eq!(t.intent, Some(Intent::Apply));
    t.applied(Applied::Done, 1_000);
    t
}

fn conn(ip: &str, up_since: u64) -> Conn {
    Conn {
        ipv4: ip.into(),
        up_since_ms: Some(up_since),
        ..Conn::default()
    }
}

/// 只有 IPv6 的连接（D41）。
fn conn6(ip: &str, up_since: u64) -> Conn {
    Conn {
        ipv6: ip.into(),
        up_since_ms: Some(up_since),
        ..Conn::default()
    }
}

/// 不应当有数据的读数（数据或漫游关着）：只看读回和注册，和 T2 一样。
fn read(v: &str, registered: bool) -> Reading {
    Reading {
        value: Some(v.into()),
        registered,
        sim: sim(),
        data: Some(DataPath {
            expected: Some(false),
            ..DataPath::default()
        }),
        probe: None,
    }
}

/// 应当有数据、已连接的读数。
fn live(v: &str, c: Conn) -> Reading {
    Reading {
        data: Some(DataPath {
            expected: Some(true),
            connected: true,
            conn: c,
            iface: "rmnet_data0".into(),
            dns: vec!["192.0.2.53".into()],
        }),
        ..read(v, true)
    }
}

/// 驱动的做法：要探测就填结果，再喂进去。
fn feed(t: &mut Txn, mut r: Reading, probe_ok: bool, now: u64) -> (Next, bool) {
    let probed = t.wants_probe(&r, now).is_some();
    if probed {
        r.probe = Some(probe_ok);
    }
    (t.on_reading(&r, now), probed)
}

fn end(t: &Txn) -> (Phase, Option<Reason>) {
    (t.phase, t.reason)
}

#[test]
fn readback_and_registered_confirms() {
    let mut t = txn(false);
    assert_eq!(t.phase, Phase::Verifying);
    assert_eq!(t.on_reading(&read("WL_AND_5G", true), 3_000), Next::Wait);
    assert_eq!(t.on_reading(&read("Only_LTE", false), 5_000), Next::Wait);
    assert!(t.ever_matched);
    assert_eq!(t.on_reading(&read("Only_LTE", true), 7_000), Next::Done);
    assert_eq!(end(&t), (Phase::Confirmed, Some(Reason::Verified)));
    assert!(t.phase.is_final());
    // T11：开始确认（1 s 下发完）到数据通用了 6 s
    assert_eq!(t.took_ms, Some(6_000));
}

#[test]
fn never_matched_at_deadline_is_not_applied() {
    let mut t = txn(true);
    for now in (3_000..1_000 + D).step_by(2_000) {
        assert_eq!(t.on_reading(&read("WL_AND_5G", true), now), Next::Wait);
    }
    assert_eq!(
        t.on_reading(&read("WL_AND_5G", true), 1_000 + D),
        Next::Done
    );
    // 写没生效：不退回。
    assert_eq!(end(&t), (Phase::NotApplied, Some(Reason::Ignored)));
}

#[test]
fn apply_error_with_old_readback_is_not_applied_at_once() {
    let mut t = Txn::new(
        NewTxn {
            op_id: "op1".into(),
            action: "network.set_mode".into(),
            item: "network.mode".into(),
            source: Source::Web,
            approved_by: Source::Web,
            undo: false,
            target: "Only_LTE".into(),
            old: "WL_AND_5G".into(),
            rollback_to: "WL_AND_5G".into(),
            sim: sim(),
            conn: None,
            confirm: Confirm::Registered,
            rollback_enabled: true,
            deadline_ms: D,
            boot_id: "boot-a".into(),
            receipt: false,
        },
        0,
    );
    t.begin_apply();
    t.applied(Applied::Failed, 0);
    assert_eq!(t.on_reading(&read("WL_AND_5G", true), 2_000), Next::Done);
    assert_eq!(end(&t), (Phase::NotApplied, Some(Reason::Ignored)));
}

#[test]
fn apply_error_but_readback_moved_still_confirms() {
    let mut t = txn(false);
    t.apply_failed = true;
    assert_eq!(t.on_reading(&read("Only_LTE", true), 3_000), Next::Done);
    assert_eq!(end(&t), (Phase::Confirmed, Some(Reason::Verified)));
}

#[test]
fn matched_then_old_is_manual_change() {
    let mut t = txn(true);
    t.on_reading(&read("Only_LTE", false), 3_000);
    assert_eq!(t.on_reading(&read("WL_AND_5G", true), 5_000), Next::Done);
    assert_eq!(end(&t), (Phase::Cancelled, Some(Reason::ManualChange)));
}

#[test]
fn third_value_is_manual_change() {
    let mut t = txn(true);
    assert_eq!(t.on_reading(&read("Only_5G", true), 3_000), Next::Done);
    assert_eq!(end(&t), (Phase::Cancelled, Some(Reason::ManualChange)));
}

#[test]
fn empty_readback_is_not_a_third_value() {
    let mut t = txn(true);
    assert_eq!(t.on_reading(&read("", false), 3_000), Next::Wait);
    assert_eq!(t.phase, Phase::Verifying);
}

#[test]
fn deadline_with_rollback_off_is_unverified() {
    let mut t = txn(false);
    t.on_reading(&read("Only_LTE", false), 3_000);
    assert_eq!(
        t.on_reading(&read("Only_LTE", false), 1_000 + D),
        Next::Done
    );
    assert_eq!(end(&t), (Phase::Unverified, Some(Reason::NoRollback)));
}

#[test]
fn deadline_with_rollback_on_rolls_back_once() {
    let mut t = txn(true);
    t.on_reading(&read("Only_LTE", false), 3_000);
    let next = t.on_reading(&read("Only_LTE", false), 1_000 + D);
    assert_eq!(next, Next::Rollback("WL_AND_5G".into()));
    assert_eq!(t.phase, Phase::RollingBack);
    assert_eq!(t.intent, Some(Intent::Rollback));
    // 退回的写还没回来：读数一律不算。
    assert_eq!(
        t.on_reading(&read("WL_AND_5G", true), 2_000 + D),
        Next::Wait
    );
    t.rollback_sent(2_000 + D);
    assert_eq!(t.intent, None);
    // 还是目标值：等。
    assert_eq!(t.on_reading(&read("Only_LTE", true), 4_000 + D), Next::Wait);
    assert_eq!(
        t.on_reading(&read("WL_AND_5G", false), 6_000 + D),
        Next::Wait
    );
    assert_eq!(
        t.on_reading(&read("WL_AND_5G", true), 8_000 + D),
        Next::Done
    );
    assert_eq!(end(&t), (Phase::RolledBack, Some(Reason::Timeout)));
}

#[test]
fn rollback_not_through_is_rollback_failed() {
    let mut t = txn(true);
    t.on_reading(&read("Only_LTE", false), 3_000);
    t.on_tick(1_000 + D);
    t.rollback_sent(2_000 + D);
    assert_eq!(
        t.on_reading(&read("WL_AND_5G", false), 2_000 + 2 * D),
        Next::Done
    );
    assert_eq!(
        end(&t),
        (Phase::RollbackFailed, Some(Reason::RollbackTimeout))
    );
}

#[test]
fn third_value_while_rolling_back_is_manual_change() {
    let mut t = txn(true);
    t.on_reading(&read("Only_LTE", false), 3_000);
    t.on_tick(1_000 + D);
    t.rollback_sent(2_000 + D);
    assert_eq!(t.on_reading(&read("Only_5G", true), 4_000 + D), Next::Done);
    assert_eq!(end(&t), (Phase::Cancelled, Some(Reason::ManualChange)));
}

#[test]
fn no_readings_at_all_is_not_judged_not_applied() {
    let mut t = txn(false);
    assert_eq!(t.on_tick(1_000 + D - 1), Next::Wait);
    assert_eq!(t.on_tick(1_000 + D), Next::Done);
    assert_eq!(end(&t), (Phase::Unverified, Some(Reason::NoRollback)));
    let mut t = txn(true);
    assert_eq!(t.on_tick(1_000 + D), Next::Rollback("WL_AND_5G".into()));
}

#[test]
fn sim_change_cancels_in_any_wait() {
    let other = Reading {
        value: Some("Only_LTE".into()),
        registered: true,
        sim: SimId {
            iccid: "89860000000000000002".into(),
            slot: 1,
        },
        ..read("Only_LTE", true)
    };
    let mut t = txn(true);
    assert_eq!(t.on_reading(&other, 3_000), Next::Done);
    assert_eq!(end(&t), (Phase::Cancelled, Some(Reason::SimChanged)));

    let mut t = txn(true);
    t.on_tick(1_000 + D);
    t.rollback_sent(1_000 + D);
    let slot2 = Reading {
        sim: SimId {
            iccid: sim().iccid,
            slot: 2,
        },
        ..read("WL_AND_5G", true)
    };
    assert_eq!(t.on_reading(&slot2, 3_000 + D), Next::Done);
    assert_eq!(end(&t), (Phase::Cancelled, Some(Reason::SimChanged)));
}

#[test]
fn user_revert_works_with_auto_rollback_off() {
    let mut t = txn(false);
    assert_eq!(t.revert(), Ok(Next::Rollback("WL_AND_5G".into())));
    assert_eq!(t.revert(), Err("already rolling back"));
    t.rollback_sent(3_000);
    assert_eq!(t.on_reading(&read("WL_AND_5G", true), 5_000), Next::Done);
    assert_eq!(end(&t), (Phase::RolledBack, Some(Reason::UserRevert)));
    assert_eq!(t.revert(), Err("change already finished"));
}

#[test]
fn keep_confirms_by_user() {
    let mut t = txn(true);
    assert_eq!(t.keep(), Ok(()));
    assert_eq!(end(&t), (Phase::Confirmed, Some(Reason::UserKeep)));
    let mut t = txn(true);
    t.revert().unwrap();
    assert_eq!(t.keep(), Err("already rolling back"));
}

#[test]
fn cancel_only_touches_live_changes() {
    let mut t = txn(true);
    t.cancel(Reason::Superseded);
    assert_eq!(end(&t), (Phase::Cancelled, Some(Reason::Superseded)));
    t.cancel(Reason::Preempted);
    assert_eq!(t.reason, Some(Reason::Superseded));
}

#[test]
fn resume_same_boot_keeps_the_clock() {
    let mut t = txn(true);
    t.on_reading(&read("Only_LTE", false), 60_000);
    assert_eq!(t.resume("boot-a", 70_000), Next::Wait);
    assert_eq!(t.boots, 0);
    assert_eq!(t.wait_start_ms, 1_000);
    assert_eq!(t.on_tick(1_000 + D), Next::Rollback("WL_AND_5G".into()));
}

#[test]
fn first_reboot_restarts_the_clock() {
    let mut t = txn(true);
    t.on_reading(&read("Only_LTE", false), 61_000);
    // 新开机，时钟从头数
    assert_eq!(t.resume("boot-b", 5_000), Next::Wait);
    assert_eq!(t.boots, 1);
    assert_eq!(t.elapsed_prev_ms, 60_000);
    assert_eq!(t.wait_start_ms, 5_000);
    assert_eq!(t.on_tick(5_000 + D - 1), Next::Wait);
    assert_eq!(t.on_tick(5_000 + D), Next::Rollback("WL_AND_5G".into()));
}

#[test]
fn second_reboot_rolls_back_at_once() {
    let mut t = txn(true);
    t.on_reading(&read("Only_LTE", false), 11_000);
    t.resume("boot-b", 5_000);
    t.on_reading(&read("Only_LTE", false), 15_000);
    let next = t.resume("boot-c", 4_000);
    assert_eq!(next, Next::Rollback("WL_AND_5G".into()));
    assert_eq!(t.rollback_reason, Some(Reason::RebootLoop));
    t.rollback_sent(5_000);
    t.on_reading(&read("WL_AND_5G", true), 7_000);
    assert_eq!(end(&t), (Phase::RolledBack, Some(Reason::RebootLoop)));
}

#[test]
fn reboot_after_whole_deadline_rolls_back_at_once() {
    let mut t = txn(true);
    // 上一次开机最后一次看到它时已经等满了时限（那一拍还没来得及判就重启了）
    t.seen_ms = 1_000 + D;
    let next = t.resume("boot-b", 3_000);
    assert_eq!(t.boots, 1);
    assert_eq!(next, Next::Rollback("WL_AND_5G".into()));
    assert_eq!(t.rollback_reason, Some(Reason::RebootLoop));
}

#[test]
fn reboot_loop_with_rollback_off_is_unverified() {
    let mut t = txn(false);
    t.resume("boot-b", 1_000);
    assert_eq!(t.resume("boot-c", 1_000), Next::Done);
    assert_eq!(end(&t), (Phase::Unverified, Some(Reason::NoRollback)));
}

#[test]
fn crash_during_apply_is_never_reapplied() {
    let mut t = Txn::new(
        NewTxn {
            op_id: "op1".into(),
            action: "network.set_mode".into(),
            item: "network.mode".into(),
            source: Source::Screen,
            approved_by: Source::Screen,
            undo: false,
            target: "Only_LTE".into(),
            old: "WL_AND_5G".into(),
            rollback_to: "WL_AND_5G".into(),
            sim: sim(),
            conn: None,
            confirm: Confirm::Registered,
            rollback_enabled: true,
            deadline_ms: D,
            boot_id: "boot-a".into(),
            receipt: false,
        },
        0,
    );
    t.begin_apply();
    // 意图已落盘，结果没落盘就崩了
    let mut t: Txn = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
    assert_eq!(t.resume("boot-a", 9_000), Next::Wait);
    assert_eq!(t.phase, Phase::Verifying);
    assert_eq!(t.intent, None);
    // 读回 = 目标：视为已做
    assert_eq!(t.on_reading(&read("Only_LTE", true), 11_000), Next::Done);
    assert_eq!(end(&t), (Phase::Confirmed, Some(Reason::Verified)));

    // 读回一直是旧值：视为没做，到点 not_applied，不退回
    let mut t: Txn = serde_json::from_str(
        &serde_json::to_string(&{
            let mut x = txn(true);
            x.phase = Phase::Applying;
            x.intent = Some(Intent::Apply);
            x
        })
        .unwrap(),
    )
    .unwrap();
    t.resume("boot-b", 0);
    t.on_reading(&read("WL_AND_5G", true), 2_000);
    assert_eq!(t.on_reading(&read("WL_AND_5G", true), D), Next::Done);
    assert_eq!(end(&t), (Phase::NotApplied, Some(Reason::Ignored)));
}

#[test]
fn crash_during_rollback_keeps_the_rollback_intent() {
    let mut t = txn(true);
    t.on_tick(1_000 + D);
    assert_eq!(t.intent, Some(Intent::Rollback));
    let mut t: Txn = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
    // 同一次开机和整机重启都一样：意图留着，驱动按读回决定发不发（O7）；一直发不出去，到点放弃
    let mut u = t.clone();
    assert_eq!(t.resume("boot-a", 2_000 + D), Next::Wait);
    assert_eq!(t.phase, Phase::RollingBack);
    assert_eq!(t.intent, Some(Intent::Rollback));
    assert_eq!(u.resume("boot-b", 1_000), Next::Wait);
    assert_eq!(u.phase, Phase::RollingBack);
    assert_eq!(u.on_tick(1_000 + D), Next::Done);
    assert_eq!(
        end(&u),
        (Phase::RollbackFailed, Some(Reason::RollbackTimeout))
    );
}

#[test]
fn status_reports_remaining_time() {
    let t = txn(false);
    let s = t.status(31_000);
    assert_eq!(s["phase"], "verifying");
    assert_eq!(s["source"], "screen");
    assert_eq!(s["remaining_ms"], D - 30_000);
    assert_eq!(s["reason"], Value::Null);
}

#[test]
fn sources() {
    for (s, user) in [
        ("screen", true),
        ("web", true),
        ("legacy", true),
        ("guard", false),
        ("scenario", false),
        ("scheduler", false),
        ("auto", false),
    ] {
        let src = Source::parse(s).unwrap();
        assert_eq!(src.is_user(), user, "{s}");
        assert_eq!(serde_json::to_value(src).unwrap(), s);
    }
    assert!(Source::Guard.may_override());
    assert!(!Source::Scenario.may_override());
    assert_eq!(Source::parse("touch"), None);
}

#[test]
fn sim_check_only_a_different_identity_is_a_change() {
    let blank = SimId {
        iccid: String::new(),
        slot: 0,
    };
    let no_slot = SimId {
        iccid: sim().iccid,
        slot: 0,
    };
    let other = SimId {
        iccid: "89860000000000000002".into(),
        slot: 1,
    };
    assert_eq!(sim_check(&sim(), &sim()), SimCheck::Same);
    assert_eq!(sim_check(&sim(), &other), SimCheck::Changed);
    assert_eq!(sim_check(&sim(), &blank), SimCheck::Unknown);
    assert_eq!(sim_check(&sim(), &no_slot), SimCheck::Unknown);
    assert_eq!(sim_check(&blank, &blank), SimCheck::Same);
    // 写之前没卡、现在插了卡
    assert_eq!(sim_check(&blank, &sim()), SimCheck::Changed);
}

#[test]
fn blank_sim_reading_only_counts_time() {
    let mut t = txn(true);
    let blank = Reading {
        sim: SimId {
            iccid: String::new(),
            slot: 0,
        },
        ..read("WL_AND_5G", true)
    };
    assert_eq!(t.on_reading(&blank, 3_000), Next::Wait);
    assert!(!t.read_ok);
    assert_eq!(t.phase, Phase::Verifying);
}

#[test]
fn timed_out_write_is_judged_only_by_readback() {
    let mut t = txn(true);
    t.phase = Phase::Applying;
    t.intent = Some(Intent::Apply);
    t.applied(Applied::Unknown, 1_000);
    assert!(!t.apply_failed);
    // 原厂还在做：读回仍是旧值也不马上判没写进去
    assert_eq!(t.on_reading(&read("WL_AND_5G", true), 3_000), Next::Wait);
    assert_eq!(t.on_reading(&read("Only_LTE", true), 9_000), Next::Done);
    assert_eq!(end(&t), (Phase::Confirmed, Some(Reason::Verified)));
}

// ---- T4：数据通（D25、D33、D34） ----

fn apn_txn(rollback: bool) -> Txn {
    let mut t = txn(rollback);
    t.confirm = Confirm::Apn;
    t
}

#[test]
fn connected_but_probe_fails_is_not_confirmed() {
    let mut t = txn(false);
    let c = conn("10.0.0.1", 500);
    assert_eq!(
        feed(&mut t, live("Only_LTE", c.clone()), false, 3_000),
        (Next::Wait, true)
    );
    assert!(t.ever_matched);
    assert!(!t.data_ok);
    // 隔不到 5 秒不再探测
    assert_eq!(
        feed(&mut t, live("Only_LTE", c.clone()), false, 5_000),
        (Next::Wait, false)
    );
    assert_eq!(
        feed(&mut t, live("Only_LTE", c.clone()), false, 8_000),
        (Next::Wait, true)
    );
    assert_eq!(
        feed(&mut t, live("Only_LTE", c.clone()), false, 13_000),
        (Next::Wait, true)
    );
    // 一轮失败满 3 次：同一条连接上 30 秒内不再探测
    assert_eq!(t.probe_fails, PROBE_TRIES);
    assert_eq!(
        feed(&mut t, live("Only_LTE", c.clone()), true, 30_000),
        (Next::Wait, false)
    );
    // 对上过、数据一直不通：到点按没通处理，不算 not_applied
    assert_eq!(t.on_tick(1_000 + D), Next::Done);
    assert_eq!(end(&t), (Phase::Unverified, Some(Reason::NoRollback)));
}

#[test]
fn pdp_kept_and_data_late_still_confirms() {
    // 换制式时 PDP 没断（连接身份一直不变），前十几秒基带在重新附着、探测不通，过一会才通
    let mut t = txn(false);
    let kept = conn("10.0.0.1", 500);
    for now in [3_000, 8_000, 13_000] {
        assert_eq!(
            feed(&mut t, live("Only_LTE", kept.clone()), false, now),
            (Next::Wait, true)
        );
    }
    assert_eq!(
        feed(&mut t, live("Only_LTE", kept.clone()), true, 20_000),
        (Next::Wait, false)
    );
    assert_eq!(
        feed(&mut t, live("Only_LTE", kept.clone()), true, 43_000),
        (Next::Done, true)
    );
    assert_eq!(end(&t), (Phase::Confirmed, Some(Reason::Verified)));
}

#[test]
fn a_new_connection_rearms_the_probe() {
    let mut t = txn(false);
    for now in [3_000, 8_000, 13_000] {
        feed(&mut t, live("Only_LTE", conn("10.0.0.1", 500)), false, now);
    }
    assert_eq!(t.probe_fails, PROBE_TRIES);
    // 重新拨号（IP 变了）：再探测，通了就确认
    assert_eq!(
        feed(
            &mut t,
            live("Only_LTE", conn("10.0.0.9", 20_000)),
            true,
            25_000
        ),
        (Next::Done, true)
    );
    assert_eq!(end(&t), (Phase::Confirmed, Some(Reason::Verified)));
    assert!(t.data_ok);
}

#[test]
fn uptime_jitter_is_the_same_connection() {
    let mut t = txn(false);
    for (now, up) in [(3_000, 500), (8_000, 1_400), (13_000, 300)] {
        feed(&mut t, live("Only_LTE", conn("10.0.0.1", up)), false, now);
    }
    assert_eq!(t.probe_fails, PROBE_TRIES);
}

#[test]
fn probe_only_when_everything_else_is_there() {
    let t = txn(false);
    let c = conn("10.0.0.1", 500);
    // 读回还是旧值、没注册、没连上、没 IP、读不到数据通路：都不探测
    assert!(
        t.wants_probe(&live("WL_AND_5G", c.clone()), 3_000)
            .is_none()
    );
    let mut r = live("Only_LTE", c.clone());
    r.registered = false;
    assert!(t.wants_probe(&r, 3_000).is_none());
    let mut r = live("Only_LTE", c.clone());
    r.data.as_mut().unwrap().connected = false;
    assert!(t.wants_probe(&r, 3_000).is_none());
    let r = live("Only_LTE", conn("", 500));
    assert!(t.wants_probe(&r, 3_000).is_none());
    let mut r = live("Only_LTE", c.clone());
    r.data = None;
    assert!(t.wants_probe(&r, 3_000).is_none());
    let mut r = live("Only_LTE", c.clone());
    r.data.as_mut().unwrap().expected = None;
    assert!(t.wants_probe(&r, 3_000).is_none());
    // 齐了：绑定 get_wwaniface 报的接口
    assert_eq!(
        t.wants_probe(&live("Only_LTE", c), 3_000),
        Some(ProbeTarget {
            iface: "rmnet_data0".into(),
            dns: vec!["192.0.2.53".into()],
            v6: false,
            chill: false,
        })
    );
}

#[test]
fn ipv6_only_connection_probes_over_ipv6() {
    let mut t = txn(false);
    let mut r = live("Only_LTE", conn6("2001:db8::1", 500));
    r.data.as_mut().unwrap().dns = vec!["2001:db8::53".into()];
    assert_eq!(
        t.wants_probe(&r, 3_000),
        Some(ProbeTarget {
            iface: "rmnet_data0".into(),
            dns: vec!["2001:db8::53".into()],
            v6: true,
            chill: false,
        })
    );
    // 有 IPv4 时照旧走 IPv4，IPv6 地址不算进身份
    let both = Conn {
        ipv6: "2001:db8::1".into(),
        ..conn("10.0.0.1", 500)
    };
    assert_eq!(both.addr(), "10.0.0.1");
    assert!(!both.v6_only());
    assert!(!t.wants_probe(&live("Only_LTE", both), 3_000).unwrap().v6);
    let (next, probed) = feed(&mut t, r, true, 3_000);
    assert!(probed);
    assert_eq!(next, Next::Done);
    assert_eq!(end(&t), (Phase::Confirmed, Some(Reason::Verified)));
}

#[test]
fn ipv6_address_change_is_a_new_connection() {
    let mut t = txn(false);
    for (now, ip) in [(3_000, "2001:db8::1"), (8_000, "2001:db8::1")] {
        feed(&mut t, live("Only_LTE", conn6(ip, 500)), false, now);
    }
    assert_eq!(t.probe_fails, 2);
    // 换了地址（重新拨号）：重新计数
    feed(
        &mut t,
        live("Only_LTE", conn6("2001:db8::2", 500)),
        false,
        13_000,
    );
    assert_eq!(t.probe_fails, 1);
    // 没有地址：不探测
    assert!(
        t.wants_probe(&live("Only_LTE", Conn::default()), 20_000)
            .is_none()
    );
}

#[test]
fn old_pending_file_without_ipv6_still_reads() {
    let c: Conn = serde_json::from_str(r#"{"ipv4":"10.0.0.1","up_since_ms":5}"#).unwrap();
    assert_eq!(c, conn("10.0.0.1", 5));
    // 有 IPv4 的连接落盘的样子不变
    assert_eq!(
        serde_json::to_string(&c).unwrap(),
        r#"{"ipv4":"10.0.0.1","up_since_ms":5}"#
    );
}

#[test]
fn no_data_expected_never_probes_and_confirms_on_registration() {
    let mut t = txn(false);
    let r = read("Only_LTE", true);
    assert!(t.wants_probe(&r, 3_000).is_none());
    assert_eq!(feed(&mut t, r, true, 3_000), (Next::Done, false));
    assert_eq!(end(&t), (Phase::Confirmed, Some(Reason::Verified)));
}

#[test]
fn unknown_data_path_waits() {
    let mut t = txn(false);
    let mut r = read("Only_LTE", true);
    r.data = None;
    assert_eq!(t.on_reading(&r, 3_000), Next::Wait);
    let mut r = read("Only_LTE", true);
    r.data.as_mut().unwrap().expected = None;
    assert_eq!(t.on_reading(&r, 5_000), Next::Wait);
    assert!(t.ever_matched);
}

#[test]
fn pdp_kept_confirms_a_mode_switch_but_not_an_apn() {
    // 写之前的连接一直没断（IP、起点都没变）
    let kept = conn("10.0.0.1", 500);
    let mut t = txn(false);
    assert_eq!(
        feed(&mut t, live("Only_LTE", kept.clone()), true, 3_000),
        (Next::Done, true)
    );
    assert_eq!(end(&t), (Phase::Confirmed, Some(Reason::Verified)));

    let mut t = apn_txn(false);
    assert_eq!(
        feed(&mut t, live("Only_LTE", kept.clone()), true, 3_000),
        (Next::Wait, false)
    );
    // 写之后的新连接（起点在事务开始之后）才算
    assert_eq!(
        feed(
            &mut t,
            live("Only_LTE", conn("10.0.0.1", 4_000)),
            true,
            6_000
        ),
        (Next::Done, true)
    );
    assert_eq!(end(&t), (Phase::Confirmed, Some(Reason::Verified)));

    // IP 变了也算新连接
    let mut t = apn_txn(false);
    assert_eq!(
        feed(&mut t, live("Only_LTE", conn("10.9.9.9", 500)), true, 3_000),
        (Next::Done, true)
    );
}

#[test]
fn apn_without_data_expected_is_unverified() {
    let mut t = apn_txn(true);
    assert_eq!(t.on_reading(&read("Only_LTE", false), 3_000), Next::Wait);
    assert_eq!(t.on_reading(&read("Only_LTE", true), 5_000), Next::Done);
    assert_eq!(end(&t), (Phase::Unverified, Some(Reason::ApnNoData)));
}

#[test]
fn apn_after_a_reboot_any_connection_is_new() {
    let mut t = apn_txn(false);
    assert_eq!(t.resume("boot-b", 2_000), Next::Wait);
    assert_eq!(
        feed(&mut t, live("Only_LTE", conn("10.0.0.1", 500)), true, 4_000),
        (Next::Done, true)
    );
}

#[test]
fn rollback_is_confirmed_by_the_same_rule() {
    let mut t = txn(true);
    feed(
        &mut t,
        live("Only_LTE", conn("10.0.0.1", 500)),
        false,
        3_000,
    );
    assert_eq!(t.on_tick(1_000 + D), Next::Rollback("WL_AND_5G".into()));
    t.rollback_sent(1_000 + D);
    // 旧值回来了、已注册，但数据不通：不算退回成功
    let c = conn("10.0.0.1", 500);
    for i in 0..3u64 {
        assert_eq!(
            feed(
                &mut t,
                live("WL_AND_5G", c.clone()),
                false,
                3_000 + D + i * 6_000
            ),
            (Next::Wait, true)
        );
    }
    assert_eq!(t.on_tick(1_000 + 2 * D), Next::Done);
    assert_eq!(
        end(&t),
        (Phase::RollbackFailed, Some(Reason::RollbackTimeout))
    );

    let mut t = txn(true);
    t.on_tick(1_000 + D);
    t.rollback_sent(1_000 + D);
    assert_eq!(
        feed(&mut t, live("WL_AND_5G", c.clone()), true, 3_000 + D),
        (Next::Done, true)
    );
    assert_eq!(end(&t), (Phase::RolledBack, Some(Reason::Timeout)));
}

#[test]
fn apn_rollback_needs_a_connection_after_the_rollback() {
    let mut t = apn_txn(true);
    let first = conn("10.0.0.2", 2_000);
    feed(&mut t, live("Only_LTE", first.clone()), false, 3_000);
    t.on_tick(1_000 + D);
    t.rollback_sent(1_000 + D);
    // 还是退回之前那条连接：不算
    assert_eq!(
        feed(&mut t, live("WL_AND_5G", first), true, 3_000 + D),
        (Next::Wait, false)
    );
    assert_eq!(
        feed(
            &mut t,
            live("WL_AND_5G", conn("10.0.0.2", 2_000 + D)),
            true,
            5_000 + D
        ),
        (Next::Done, true)
    );
    assert_eq!(end(&t), (Phase::RolledBack, Some(Reason::Timeout)));
}

#[test]
fn old_pending_files_still_load() {
    // T3 版本落的盘没有 T4 的字段
    let mut v = serde_json::to_value(txn(false)).unwrap();
    for k in [
        "confirm",
        "ref_conn",
        "ref_ms",
        "last_conn",
        "probe_conn",
        "probe_fails",
        "probe_last_ms",
        "data_ok",
    ] {
        v.as_object_mut().unwrap().remove(k);
    }
    let t: Txn = serde_json::from_value(v).unwrap();
    assert_eq!(t.confirm, Confirm::Registered);
    assert_eq!(t.ref_ms, 0);
}

// ── T22（D23）──────────────────────────────────────────────────────────

#[test]
fn approved_by_rules() {
    use Source::*;
    // 默认和来源相同
    assert_eq!(Who::new(Mcp, None).unwrap(), Who::from(Mcp));
    // Agent 自己批准 = 和不写一样（第二期 D7，放不放行由 ops::agent 按动作定）；别的来源不能写 mcp
    assert_eq!(Who::new(Mcp, Some(Mcp)).unwrap(), Who::from(Mcp));
    assert!(Who::new(Web, Some(Mcp)).is_err());
    // 人确认：只能是触屏或网页
    assert_eq!(
        Who::new(Mcp, Some(Screen)).unwrap().confirmed_by(),
        Some(Screen)
    );
    assert_eq!(
        Who::new(Scenario, Some(Web)).unwrap().confirmed_by(),
        Some(Web)
    );
    for by in [Guard, Legacy, Auto, Scheduler, Scenario] {
        assert!(Who::new(Mcp, Some(by)).is_err(), "{by:?}");
    }
    assert!(!Mcp.is_user() && !Mcp.may_override());
    assert_eq!(Source::parse("mcp"), Some(Mcp));
    assert_eq!(Mcp.as_str(), "mcp");
}

#[test]
fn status_and_serialization_only_carry_approved_by_when_it_differs() {
    let t = txn(true);
    assert!(t.approved_by.is_none());
    assert!(t.status(2_000).get("approved_by").is_none());
    let v = serde_json::to_value(&t).unwrap();
    assert!(v.get("approved_by").is_none());
    // 旧版落的盘（没有这个键）读回来 = 和来源相同
    let back: Txn = serde_json::from_value(v).unwrap();
    assert_eq!(back.who(), Who::from(Source::Screen));
    let mut m = t.clone();
    m.source = Source::Mcp;
    m.approved_by = Some(Source::Screen);
    assert_eq!(m.status(2_000)["approved_by"], "screen");
    let v = serde_json::to_value(&m).unwrap();
    assert_eq!(v["approved_by"], "screen");
    assert_eq!(serde_json::from_value::<Txn>(v).unwrap(), m);
}

/// 3-0：网络模式的目标值仍是裸字符串，落盘形状和旧版一样；旧版落的盘（字符串字段）照样读回。
#[test]
fn string_targets_keep_the_old_on_disk_shape() {
    let t = txn(true);
    let v = serde_json::to_value(&t).unwrap();
    assert_eq!(v["target"], "Only_LTE");
    assert_eq!(v["old"], "WL_AND_5G");
    assert_eq!(v["rollback_to"], "WL_AND_5G");
    let back: Txn = serde_json::from_value(v).unwrap();
    assert_eq!(back.target, json!("Only_LTE"));
    assert_eq!(back, t);
}

/// 3-0：多参数的写目标值是对象；对上、第三个值、退回都按整个值比较。
#[test]
fn object_targets_confirm_cancel_and_roll_back() {
    let new = json!({"bands": [1, 3], "rat": "lte"});
    let old = json!({"bands": [], "rat": "lte"});
    let obj = |target: &Value, rollback: bool| {
        let mut t = txn(rollback);
        t.target = target.clone();
        t.old = old.clone();
        t.rollback_to = old.clone();
        t
    };
    let at = |v: &Value, registered: bool| Reading {
        value: Some(v.clone()),
        ..read("", registered)
    };

    let mut t = obj(&new, false);
    assert_eq!(t.on_reading(&at(&new, true), 2_000), Next::Done);
    assert_eq!(
        (t.phase, t.reason),
        (Phase::Confirmed, Some(Reason::Verified))
    );
    assert_eq!(t.readback, Some(new.clone()));

    let mut t = obj(&new, false);
    let third = json!({"bands": [3], "rat": "lte"});
    assert_eq!(t.on_reading(&at(&third, true), 2_000), Next::Done);
    assert_eq!(
        (t.phase, t.reason),
        (Phase::Cancelled, Some(Reason::ManualChange))
    );

    // 读回是空对象（读数器没读到）不算第三个值
    let mut t = obj(&new, false);
    assert_eq!(t.on_reading(&at(&json!({}), true), 2_000), Next::Wait);

    let mut t = obj(&new, true);
    assert_eq!(t.on_reading(&at(&new, false), 2_000), Next::Wait);
    assert_eq!(t.on_tick(1_000 + D), Next::Rollback(old.clone()));
    t.rollback_sent(1_000 + D);
    assert_eq!(t.on_reading(&at(&old, true), 2_000 + D), Next::Done);
    assert_eq!(t.phase, Phase::RolledBack);
}

// ── CHILL（第二期 D2、D10）──────────────────────────────────────────

fn chill_txn() -> Txn {
    let blank = SimId {
        iccid: String::new(),
        slot: 0,
    };
    let mut t = Txn::new(
        NewTxn {
            op_id: "c1".into(),
            action: "chill.exit".into(),
            item: "chill.exit".into(),
            source: Source::Mcp,
            approved_by: Source::Mcp,
            undo: false,
            target: "direct_keep_ai".into(),
            old: "proxy".into(),
            rollback_to: "proxy".into(),
            sim: blank,
            conn: None,
            confirm: Confirm::Chill,
            rollback_enabled: true,
            deadline_ms: D,
            boot_id: "boot-a".into(),
            receipt: false,
        },
        1_000,
    );
    t.begin_apply();
    t.applied(Applied::Done, 1_000);
    t
}

fn chill_read(v: &str, probe: Option<bool>) -> Reading {
    Reading {
        value: Some(v.into()),
        registered: true,
        sim: SimId {
            iccid: String::new(),
            slot: 0,
        },
        data: None,
        probe,
    }
}

/// 真路径探测连续两次通才算过；中间失败一次重新数；两次之间隔 5 秒；不看蜂窝数据。
#[test]
fn chill_needs_two_passes_in_a_row() {
    let mut t = chill_txn();
    let r = chill_read("direct_keep_ai", None);
    let target = t.wants_probe(&r, 2_000).expect("asks for the CHILL probe");
    assert!(target.chill);
    assert_eq!(t.on_reading(&chill_read("direct_keep_ai", Some(true)), 2_000), Next::Wait);
    assert!(t.wants_probe(&r, 4_000).is_none(), "5 s between probes");
    assert_eq!(t.on_reading(&chill_read("direct_keep_ai", Some(false)), 7_500), Next::Wait);
    assert_eq!(t.on_reading(&chill_read("direct_keep_ai", Some(true)), 13_000), Next::Wait);
    assert!(t.wants_probe(&r, 18_500).is_some());
    assert_eq!(t.on_reading(&chill_read("direct_keep_ai", Some(true)), 18_500), Next::Done);
    assert_eq!(end(&t), (Phase::Confirmed, Some(Reason::Verified)));
}

/// 到点没连续通：退回原来的出口；读回是原值就算退回完成（不等探测，也不要 mihomo 在跑：闲置暂停时它停着）。
/// 人在中间改成第三个值：不退回。
#[test]
fn chill_rolls_back_and_a_person_wins() {
    let mut t = chill_txn();
    t.on_reading(&chill_read("direct_keep_ai", Some(false)), 2_000);
    assert_eq!(
        t.on_tick(1_000 + D),
        Next::Rollback(json!("proxy"))
    );
    t.rollback_sent(1_000 + D);
    let n = 1_000 + D;
    let mut paused = chill_read("proxy", None);
    paused.registered = false;
    assert_eq!(t.on_reading(&paused, n + 2_000), Next::Done);
    assert_eq!(end(&t), (Phase::RolledBack, Some(Reason::Timeout)));

    let mut t = chill_txn();
    assert_eq!(t.on_reading(&chill_read("direct_all", None), 3_000), Next::Done);
    assert_eq!(end(&t), (Phase::Cancelled, Some(Reason::ManualChange)));
}

#[test]
fn revert_without_rollback_target_is_refused() {
    let mut t = txn(false);
    t.rollback_to = Value::Null;
    assert_eq!(t.revert(), Err("no previous setting to revert to"));
    assert_eq!(t.phase, Phase::Verifying);
}

/// O3：远端发起的 Wi-Fi 改动（`receipt`）。全局退回开关按 `rollback` 给。
fn wifi_remote(rollback: bool) -> Txn {
    let mut t = Txn::new(
        NewTxn {
            op_id: "w1".into(),
            action: "wifi.settings".into(),
            item: "wifi.main".into(),
            source: Source::Web,
            approved_by: Source::Web,
            undo: false,
            target: "new".into(),
            old: "old".into(),
            rollback_to: "old".into(),
            sim: no_sim(),
            conn: None,
            confirm: Confirm::Wifi,
            rollback_enabled: rollback,
            deadline_ms: 90_000,
            boot_id: "boot-a".into(),
            receipt: true,
        },
        1_000,
    );
    t.begin_apply();
    t.applied(Applied::Done, 1_000);
    t
}

fn ap(v: &str) -> Reading {
    Reading {
        value: Some(v.into()),
        registered: true,
        sim: no_sim(),
        data: None,
        probe: None,
    }
}

#[test]
fn remote_wifi_waits_for_a_receipt_after_the_ap_is_up() {
    let mut t = wifi_remote(true);
    assert_eq!(t.receipt(), Err("Wi-Fi not confirmed yet"), "AP not up yet");
    assert_eq!(t.on_reading(&ap("new"), 10_000), Next::Wait);
    assert_eq!(t.phase, Phase::Verifying, "AP up is not the end");
    let s = t.status(10_000);
    assert_eq!(s["receipt"]["waiting"], true);
    assert_eq!(s["receipt"]["remaining_ms"], RECEIPT_MS);
    assert_eq!(t.remaining_ms(40_000), Some(RECEIPT_MS - 30_000));
    // The main 90 s deadline no longer applies while waiting for the receipt.
    assert_eq!(t.on_tick(69_999), Next::Wait);
    assert_eq!(t.receipt(), Ok(()));
    assert_eq!((t.phase, t.reason), (Phase::Confirmed, Some(Reason::Receipt)));
}

#[test]
fn no_receipt_reverts_even_with_auto_rollback_off() {
    let mut t = wifi_remote(false);
    t.on_reading(&ap("new"), 10_000);
    // Later readings keep it waiting until the window is over.
    assert_eq!(t.on_reading(&ap("new"), 69_999), Next::Wait);
    assert_eq!(t.on_reading(&ap("new"), 70_000), Next::Rollback("old".into()));
    assert_eq!(t.phase, Phase::RollingBack);
    assert_eq!(t.rollback_reason, Some(Reason::NoReceipt));
    t.rollback_sent(71_000);
    assert_eq!(t.on_reading(&ap("old"), 80_000), Next::Done);
    assert_eq!((t.phase, t.reason), (Phase::RolledBack, Some(Reason::NoReceipt)));
}

#[test]
fn no_receipt_on_a_tick_and_nothing_to_revert_to_is_no_rollback() {
    let mut t = wifi_remote(true);
    t.on_reading(&ap("new"), 10_000);
    assert_eq!(t.on_tick(70_000), Next::Rollback("old".into()));
    let mut t = wifi_remote(true);
    t.rollback_to = Value::Null;
    t.on_reading(&ap("new"), 10_000);
    assert_eq!(t.on_tick(70_000), Next::Done);
    assert_eq!((t.phase, t.reason), (Phase::Unverified, Some(Reason::NoRollback)));
}

#[test]
fn a_reboot_while_waiting_restarts_the_receipt_window() {
    let mut t = wifi_remote(true);
    t.on_reading(&ap("new"), 50_000);
    assert_eq!(t.resume("boot-b", 2_000), Next::Wait);
    assert_eq!(t.on_tick(61_999), Next::Wait);
    assert_eq!(t.on_tick(62_000), Next::Rollback("old".into()));
}

#[test]
fn local_wifi_and_other_actions_take_no_receipt() {
    let mut t = wifi_remote(true);
    t.receipt = false;
    assert_eq!(t.on_reading(&ap("new"), 10_000), Next::Done);
    assert_eq!(t.reason, Some(Reason::Verified));
    assert!(t.status(10_000).get("receipt").is_none());
    let mut t = txn(true);
    assert_eq!(t.receipt(), Err("this change does not take a receipt"));
}

/// R18-P0-02 ②：本机没发出去的探测是 unknown：不计失败、不确认，只按 5 秒间隔再试。
#[test]
fn unknown_probe_neither_counts_nor_confirms() {
    let mut t = txn(false);
    let r = live("Only_LTE", conn("10.0.0.1", 500));
    assert!(t.wants_probe(&r, 3_000).is_some());
    t.probe_unknown(3_000);
    assert!(t.wants_probe(&r, 4_000).is_none(), "5 s between probes");
    assert_eq!(t.on_reading(&r, 4_000), Next::Wait);
    assert_eq!(t.probe_fails, 0);
    assert_eq!(t.phase, Phase::Verifying);
    assert!(t.wants_probe(&r, 8_000).is_some());
}

/// R18-P0-02 ③：喂回前的复核比 `same` 严：起点只有一边读到、地址变了、没地址都算变了。
#[test]
fn still_is_stricter_than_same() {
    let a = conn("10.0.0.1", 500);
    assert!(a.still(&conn("10.0.0.1", 1_500)));
    assert!(!a.still(&conn("10.0.0.1", 5_000)));
    assert!(!a.still(&conn("10.0.0.2", 500)));
    let mut none = a.clone();
    none.up_since_ms = None;
    assert!(a.same(&none) && !a.still(&none));
    assert!(!conn("", 500).still(&conn("", 500)));
}

/// Wi-Fi 事务的 SIM 记空（换卡不影响 Wi-Fi）。
fn no_sim() -> SimId {
    SimId {
        iccid: String::new(),
        slot: 0,
    }
}
