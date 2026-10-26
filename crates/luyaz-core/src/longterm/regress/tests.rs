use super::detect::{Cell, Check};
use super::*;
use serde_json::json;

const W0: i64 = 1_792_800_000;
const BOOT: &str = "aaaaaaaa-1111-2222-3333-444444444444";

fn at(up: i64) -> At {
    At {
        boot: "aaaaaaaa".into(),
        up,
        wall: W0 + up,
        ok: true,
    }
}

fn fresh() -> Regress {
    Regress::load(None, base::Base::default(), "1.45.0", &at(100), 0, BOOT)
}

fn chk(up: i64) -> Check {
    Check {
        at: at(up),
        core_age: up,
        ..Default::default()
    }
}

fn kinds(r: &Regress) -> Vec<String> {
    r.file.open.iter().map(|i| i.kind.clone()).collect()
}

fn ev(seq: u64, wall: i64, up: u64, kind: &str, text: &str) -> luyaz_api::alerts::Event {
    luyaz_api::alerts::Event {
        seq,
        wall,
        uptime: up,
        kind: kind.into(),
        text: text.into(),
    }
}

/// Implementation spec §1: the first run takes the queue as read and this boot as judged.
#[test]
fn the_first_run_reports_nothing_old() {
    let mut r = Regress::load(None, base::Base::default(), "b", &at(100), 42, BOOT);
    assert_eq!(r.file.ingested_seq, 42);
    r.boot(&json!({"code": 1119}), &at(160), BOOT, None);
    assert!(r.file.open.is_empty());
    r.ingest(
        &[
            ev(40, W0, 50, "core-crash", "x"),
            ev(43, W0 + 90, 90, "core-crash", "y"),
        ],
        &at(100),
    );
    assert_eq!(r.file.open.len(), 1, "only the one after the read point");
    assert_eq!(r.file.open[0].id, "q43");
}

/// Eng review R5: planned or manual/scheduled codes are not alerts; 1134 without a mark, 1119,
/// 1155 are.
#[test]
fn unexpected_reboots_by_mark_and_code() {
    for (facts, want) in [
        (json!({"code": 1112}), false),
        (json!({"code": 1150}), false),
        (json!({"code": 1152}), false),
        (json!({"code": 1134, "planned": "web"}), false),
        (json!({"code": 1134}), true),
        (json!({"code": 1119}), true),
        (json!({"code": 1155}), true),
        (json!({}), true),
    ] {
        let mut r = fresh();
        r.boot(&facts, &at(160), "bbbbbbbb-new", None);
        assert_eq!(kinds(&r) == ["regress-reboot"], want, "{facts}");
    }
}

#[test]
fn a_reboot_row_escalates_at_2_4_8_and_ends_quiet() {
    let mut r = fresh();
    r.boot(&json!({"code": 1119}), &at(160), "b1", None);
    assert_eq!(r.pending().len(), 1);
    let id = r.file.open[0].id.clone();
    r.queued(&format!("rg-{id}-open"), 7);
    for (n, b) in (2..=4).zip(["b2", "b3", "b4"]) {
        let t = At {
            boot: format!("{b}xxxxxx")[..8].to_string(),
            up: 60,
            wall: W0 + 160 + n * 600,
            ok: true,
        };
        r.boot(&json!({"code": 1119}), &t, b, None);
    }
    let i = &r.file.open[0];
    assert_eq!(i.count, 4);
    let p: Vec<String> = r
        .pending()
        .iter()
        .map(|p| p.id.rsplit('-').next().unwrap().to_string())
        .collect();
    assert_eq!(p, ["esc2", "esc4"]);
    // 6 h quiet: ends without a close alert
    let late = At {
        boot: "b4xxxxxx".into(),
        up: 60 + 6 * 3600 + 61,
        wall: W0 + 160 + 2400 + 6 * 3600 + 61,
        ok: true,
    };
    sync::expire(&mut r, &late);
    assert!(r.file.open.is_empty());
    assert_eq!(r.file.closed[0].end.as_deref(), Some("quiet"));
    assert!(!r.pending().iter().any(|p| p.id.ends_with("-close")));
}

/// A reboot cuts interval incidents (the reboot is reported by itself).
#[test]
fn a_new_boot_cuts_open_interval_incidents() {
    let mut r = fresh();
    r.open("regress-cellular", None, &at(200), "x".into(), None, None);
    let f = r.file.clone();
    let r2 = Regress::load(
        Some(f),
        base::Base::default(),
        "b",
        &At {
            boot: "cccccccc".into(),
            ..at(50)
        },
        0,
        "cccccccc-x",
    );
    assert!(r2.file.open.is_empty());
    assert_eq!(r2.file.closed[0].end.as_deref(), Some("reboot"));
}

/// Eng review R8: saved with the transition pending, then queued: a restart in between queues it
/// once.
#[test]
fn a_restart_between_save_and_queue_queues_once() {
    let d = std::env::temp_dir().join(format!("u60d-regress-r8-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let a = luyaz_api::alerts::Alerts::at(&d);
    let mut r = fresh();
    r.open(
        "regress-mem-abs",
        None,
        &at(2000),
        "anon 100MiB".into(),
        None,
        None,
    );
    // the save happened, the process died before queueing: the file still has it pending
    let saved = serde_json::to_string(&r.file).unwrap();
    for _ in 0..2 {
        let f: File = serde_json::from_str(&saved).unwrap();
        let mut r = Regress::load(Some(f), base::Base::default(), "b", &at(2100), 0, BOOT);
        for p in r.pending() {
            let (seq, _) = a.add_once(&p.id, &p.kind, &p.text, W0, 1).unwrap();
            r.queued(&p.id, seq);
        }
        assert!(r.pending().is_empty());
        assert_eq!(r.file.open[0].alert_seqs, vec![1]);
    }
    assert_eq!(a.events().len(), 1);
    assert!(a.events()[0].text.starts_with("[rg-mem-abs-"));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn memory_over_the_absolute_bar_opens_and_recovers() {
    let mut r = fresh();
    let mut k = chk(0);
    k.heap_kb = Some(10_000);
    k.anon_kb = Some(100 * 1024);
    for up in (0..=1800).step_by(60) {
        k.at = at(up);
        r.check(&k);
    }
    assert_eq!(kinds(&r), ["regress-mem-abs"]);
    assert!(
        r.file.open[0].detail.contains("anon 100MiB > 96MiB"),
        "{}",
        r.file.open[0].detail
    );
    // just under the bar is not under 80 %
    k.anon_kb = Some(90 * 1024);
    for up in (1860..=5000).step_by(60) {
        k.at = at(up);
        r.check(&k);
    }
    assert_eq!(kinds(&r), ["regress-mem-abs"]);
    k.anon_kb = Some(60 * 1024);
    for up in (5060..=6860).step_by(60) {
        k.at = at(up);
        r.check(&k);
    }
    assert!(r.file.open.is_empty());
    assert_eq!(r.file.closed[0].end.as_deref(), Some("recovered"));
    assert!(r.pending().iter().any(|p| p.id.ends_with("-close")));
}

#[test]
fn relative_memory_waits_for_the_core_to_settle_and_a_baseline() {
    let mut r = fresh();
    let mut k = chk(0);
    k.heap_kb = Some(20_000);
    k.anon_kb = Some(40_000);
    for up in (0..=3000).step_by(60) {
        k.at = at(up);
        k.core_age = up;
        r.check(&k);
    }
    assert!(
        r.file.open.is_empty(),
        "no baseline: only the absolute bars"
    );
    r.base.heap = Some(10_000.0);
    r.base.anon = Some(30_000.0);
    let mut r2 = fresh();
    r2.base = r.base.clone();
    for up in (0..=1700).step_by(60) {
        k.at = at(up);
        k.core_age = up;
        r2.check(&k);
    }
    assert!(
        r2.file.open.is_empty(),
        "the first 30 min of a core do not count"
    );
    for up in (1760..=2800).step_by(60) {
        k.at = at(up);
        k.core_age = up;
        r2.check(&k);
    }
    assert_eq!(kinds(&r2), ["regress-mem"]);
}

/// Eng review R11, R3-7: no service or no data route for 5 min opens; cannot-tell does not;
/// three good checks close.
#[test]
fn cellular_down_five_minutes_opens_three_good_close() {
    let mut r = fresh();
    let bad = Cell {
        registered: true,
        route_up: false,
    };
    let good = Cell {
        registered: true,
        route_up: true,
    };
    let mut k = chk(0);
    k.cell = Some(bad);
    for up in (0..=240).step_by(60) {
        k.at = at(up);
        r.check(&k);
    }
    assert!(r.file.open.is_empty());
    k.cell = None; // data roaming off: cannot tell, the hold starts over
    k.at = at(300);
    r.check(&k);
    k.cell = Some(bad);
    for up in (360..=600).step_by(60) {
        k.at = at(up);
        r.check(&k);
    }
    assert!(r.file.open.is_empty());
    k.at = at(660);
    r.check(&k);
    assert_eq!(kinds(&r), ["regress-cellular"]);
    assert!(
        r.file.open[0]
            .detail
            .starts_with("registered, no data route")
    );
    k.cell = Some(good);
    for up in [720, 780] {
        k.at = at(up);
        r.check(&k);
    }
    assert_eq!(kinds(&r), ["regress-cellular"]);
    k.at = at(840);
    r.check(&k);
    assert!(r.file.open.is_empty());
}

/// Eng review R14: CHILL failing (forwarding broken) twice over 5 min opens, 3 through close; a
/// stale probe is "cannot tell"; failures while cellular is down are cellular's.
#[test]
fn an_exit_failing_opens_and_cellular_takes_its_failures() {
    assert_eq!(io::chill_state(false, false, None), None);
    assert_eq!(io::chill_state(true, false, Some(true)), Some(false));
    assert_eq!(io::chill_state(true, true, None), None);
    assert_eq!(io::chill_state(true, true, Some(false)), Some(false));
    let mut r = fresh();
    let mut k = chk(0);
    k.chill = Some(false);
    k.ts = Some(true);
    for up in (0..=300).step_by(60) {
        k.at = at(up);
        r.check(&k);
    }
    assert_eq!(kinds(&r), ["regress-egress"]);
    assert_eq!(r.file.open[0].sub.as_deref(), Some("chill"));
    k.chill = Some(true);
    for up in (360..=480).step_by(60) {
        k.at = at(up);
        r.check(&k);
    }
    assert!(r.file.open.is_empty());
    let mut r = fresh();
    r.open(
        "regress-cellular",
        None,
        &at(0),
        "no service".into(),
        None,
        None,
    );
    let mut k = chk(0);
    k.chill = Some(false);
    for up in (0..=900).step_by(60) {
        k.at = at(up);
        r.check(&k);
    }
    assert_eq!(kinds(&r), ["regress-cellular"]);
}

fn hour_power(ft: i64, mw: f64, chg: i64, pkts: i64) -> serde_json::Value {
    let uwh = mw * 3600.0 / 3600.0 * 1000.0 * 3500.0 / 3600.0;
    json!({"k": "hour_power", "ft": ft, "tt": ft + 3600, "chg_s": chg, "off_home_s": 3600 - chg,
           "off_abroad_s": 0, "on_home_s": 0, "on_abroad_s": 0, "off_home_wan_pkts": pkts,
           "counter_quality": {"cells": {"off_home": {"observed_s": 3500, "uwh": uwh}}}})
}

#[test]
fn standby_power_two_qualifying_hours_over_600() {
    let l = hour_power(W0, 700.0, 0, 1000);
    let q = detect::qualifying(&l);
    assert_eq!(q.len(), 1);
    assert!((q[0].1 - 700.0).abs() < 1.0, "{q:?}");
    assert!(
        detect::qualifying(&hour_power(W0, 700.0, 60, 1000)).is_empty(),
        "charging"
    );
    assert!(
        detect::qualifying(&hour_power(W0, 700.0, 0, 20_000)).is_empty(),
        "busy"
    );
    let mut r = fresh();
    r.hour(&hour_power(W0, 700.0, 0, 1000), &at(3600));
    assert!(r.file.open.is_empty());
    r.hour(&hour_power(W0 + 3600, 650.0, 0, 1000), &at(7200));
    assert_eq!(kinds(&r), ["regress-power-abs"]);
    r.hour(&hour_power(W0 + 7200, 400.0, 0, 1000), &at(10_800));
    r.hour(&hour_power(W0 + 10_800, 420.0, 0, 1000), &at(14_400));
    assert!(r.file.open.is_empty());
}

/// Eng review R1: charging all week does not apply; unplugged 6 h with no qualifying hour is
/// blind.
#[test]
fn power_blind_only_when_unplugged_and_never_qualifying() {
    let now = W0 + 86_400;
    let mut b = base::Base::default();
    let charging: Vec<_> = (0..24)
        .map(|h| hour_power(W0 + h * 3600, 500.0, 3600, 0))
        .collect();
    b.recompute(charging.iter(), now, &[], &[]);
    assert_eq!(b.power_known, Some(true));
    let busy: Vec<_> = (0..8)
        .map(|h| hour_power(W0 + h * 3600, 500.0, 0, 90_000))
        .collect();
    b.recompute(busy.iter(), now, &[], &[]);
    assert_eq!(b.power_known, Some(false));
}

#[test]
fn baselines_median_anchor_and_left_out_spans() {
    let now = W0 + 7 * 86_400;
    let proc_ = |ft: i64, heap: f64| json!({"k": "hour_proc", "ft": ft, "tt": ft + 3600, "cpu_core": 3, "heap_live": heap, "anon_core": 30000});
    let mut lines: Vec<_> = (0..12)
        .map(|h| proc_(now - 86_400 + h * 3600, 10_000.0))
        .collect();
    // a restart hour (no cpu_core) does not count
    lines.push(json!({"k": "hour_proc", "ft": now - 3600, "tt": now, "cpu_core": null, "heap_live": 99999, "anon_core": 99999}));
    let mut b = base::Base::default();
    b.recompute(lines.iter(), now, &[], &[]);
    assert_eq!(b.heap, Some(10_000.0));
    assert_eq!(b.heap_anchor, Some(10_000.0));
    // a slow leak cannot pass the anchor by more than a quarter
    let high: Vec<_> = (0..12)
        .map(|h| proc_(now - 86_400 + h * 3600, 20_000.0))
        .collect();
    b.recompute(high.iter(), now, &[], &[]);
    assert_eq!(b.heap, Some(12_500.0));
    // hours inside an incident's span are left out
    let mut c = base::Base::default();
    c.recompute(high.iter(), now, &[(now - 2 * 86_400, now)], &[]);
    assert_eq!(c.heap, None);
}

#[test]
fn accept_needs_three_days_and_enough_hours() {
    let mut r = fresh();
    let lines: Vec<serde_json::Value> = Vec::new();
    assert!(r.accept("regress-mem-abs", lines.iter(), &at(10)).is_err());
    assert!(r.accept("regress-mem", lines.iter(), &at(10)).is_err());
    r.open("regress-mem", None, &at(0), "heap".into(), None, None);
    let e = r
        .accept("regress-mem", lines.iter(), &at(3600))
        .unwrap_err();
    assert!(e.contains("3 days"), "{e}");
    let now = at(3 * 86_400 + 60);
    let e = r.accept("regress-mem", lines.iter(), &now).unwrap_err();
    assert!(e.contains("10 more"), "{e}");
    let lines: Vec<_> = (0..10)
        .map(|h| json!({"k": "hour_proc", "ft": now.wall - 36_000 + h * 3600, "tt": now.wall - 32_400 + h * 3600, "cpu_core": 2, "heap_live": 25_600, "anon_core": 50_000}))
        .collect();
    assert!(r.accept("regress-mem", lines.iter(), &now).is_ok());
    assert_eq!(r.base.heap, Some(25_600.0));
    assert!(r.file.open.is_empty());
}

/// CEO review G3, OV-C2; round 3 D9: a crash row is one incident, its alerts kept; replay and a
/// restart keep it; a trimmed queue is a gap.
#[test]
fn the_queue_read_back_merges_rows_and_keeps_alerts() {
    let mut r = fresh();
    let q = vec![
        ev(1, W0 + 100, 100, "core-crash", "a"),
        ev(2, W0 + 900, 900, "core-crash", "b"),
        ev(3, W0 + 1500, 1500, "sms-test", "c"),
        ev(4, W0 + 2000, 2000, "core-crash", "d"),
    ];
    r.ingest(&q, &at(2100));
    r.ingest(&q, &at(2160));
    assert_eq!(r.file.open.len(), 1);
    let i = &r.file.open[0];
    assert_eq!((i.id.as_str(), i.count), ("q1", 3));
    assert_eq!(i.alert_seqs, vec![1, 2, 4]);
    // the mapping survives a restart
    let f: File = serde_json::from_str(&serde_json::to_string(&r.file).unwrap()).unwrap();
    let r2 = Regress::load(Some(f), base::Base::default(), "b", &at(2200), 0, BOOT);
    assert_eq!(r2.file.incident_of(4).map(|i| i.id.as_str()), Some("q1"));
    assert!(r2.file.incident_of(3).is_none());
    // an hour without one ends it quiet
    let mut r = r2;
    sync::expire(&mut r, &at(2000 + 3601));
    assert_eq!(r.file.closed[0].end.as_deref(), Some("quiet"));
    // trimmed past the read point: a gap
    r.ingest(&[ev(9, W0 + 5000, 5000, "sms-test", "x")], &at(5000));
    assert!(r.file.gap_at.is_some());
}

#[test]
fn the_sentinels_incidents_open_and_close_from_the_queue() {
    let mut r = fresh();
    r.ingest(
        &[ev(
            1,
            W0 + 700,
            700,
            "regress-core-down",
            "[rg-core-down-aaaaaaaa-100-open] attribution unknown",
        )],
        &at(760),
    );
    assert_eq!(kinds(&r), ["regress-core-down"]);
    assert_eq!(r.file.open[0].id, "core-down-aaaaaaaa-100");
    r.ingest(
        &[ev(
            2,
            W0 + 900,
            900,
            "regress-core-down",
            "[rg-core-down-aaaaaaaa-100-close] back",
        )],
        &at(960),
    );
    assert!(r.file.open.is_empty());
    assert_eq!(r.file.closed[0].alert_seqs, vec![1, 2]);
    assert_eq!(r.file.closed[0].end.as_deref(), Some("recovered"));
}

/// Round 3 D11: the touch screen keeps an ended incident 24 h, by BOOTTIME in the boot, by
/// trusted wall clocks across boots; an untrusted or backward clock hides it.
#[test]
fn an_ended_incident_stays_24_hours_on_the_screen() {
    let to = at(1000);
    assert!(shown(&to, &at(1000 + 86_400)));
    assert!(!shown(&to, &at(1000 + 86_401)));
    let other = |up, wall, ok| At {
        boot: "dddddddd".into(),
        up,
        wall,
        ok,
    };
    assert!(shown(&to, &other(50, to.wall + 23 * 3600, true)));
    assert!(!shown(&to, &other(50, to.wall + 23 * 3600, false)));
    assert!(
        !shown(&to, &other(50, to.wall - 3600, true)),
        "the clock went back"
    );
    // in the same boot a clock step back does not bring an expired one back
    let mut now = at(1000 + 86_401);
    now.wall = to.wall + 10;
    assert!(!shown(&to, &now));
}

#[test]
fn the_block_has_five_open_three_ended_and_nothing_that_ticks() {
    let mut r = fresh();
    for k in 0..7 {
        r.open(
            "regress-egress",
            Some(&format!("p{k}")),
            &at(100 + k),
            "x".into(),
            None,
            None,
        );
    }
    for k in 0..4 {
        let idx = r.open(
            "regress-cellular",
            None,
            &at(200 + k),
            "x".into(),
            None,
            None,
        );
        r.close(idx, &at(300 + k), "recovered", "y");
    }
    let b1 = block(&r, &at(400));
    assert_eq!(b1["open_total"], 7);
    assert_eq!(b1["open"].as_array().unwrap().len(), 5);
    assert_eq!(b1["closed"].as_array().unwrap().len(), 3);
    assert_eq!(b1["closed"][0]["end"], "recovered");
    // a minute later, nothing moved: the same block (published only on change)
    assert_eq!(block(&r, &at(460)), b1);
    // 24 h after the last end: they leave, once
    assert_eq!(
        block(&r, &at(303 + 86_401))["closed"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn every_kind_has_short_names() {
    for k in [
        "regress-mem",
        "regress-mem-abs",
        "regress-reboot",
        "regress-cellular",
        "regress-egress",
        "regress-power",
        "regress-power-abs",
        "regress-core-down",
        "regress-recorder",
        "core-crash",
        "ui-crash",
        "web-crash",
        "sentinel-crash",
        "ui-gave-up",
        "sentinel-silent",
    ] {
        let (zh, en) = name(k, None);
        assert_ne!(zh, "设备异常", "{k}");
        assert!(zh.chars().count() <= 6, "{k} {zh}");
        assert!(en.len() <= 18, "{k} {en}");
    }
    assert_eq!(
        name("something-new", None),
        ("设备异常".into(), "Device issue".into())
    );
    assert_eq!(name("regress-blind", Some("power")).0, "看不见：耗电");
    assert_eq!(name("core-crash", None).0, "数据服务崩溃");
    assert_eq!(name("ui-gave-up", None).1, "Stock UI fallback");
}

#[test]
fn ship_by_promote_in_this_boot_uncertain_across() {
    let manifest = format!(
        "{}\n{}\n",
        json!({"kind": "ship", "comp": "core", "txn": "t1", "boot_id": "aaaaaaaa-x", "uptime": 500, "mac_time": 1}),
        json!({"kind": "ship", "comp": "sentinel", "txn": "t2", "boot_id": "aaaaaaaa-x", "uptime": 9000, "act_boot": "aaaaaaaa-x", "act_uptime": 1000, "mac_time": 2})
    );
    let rows = ship::rows(&manifest, "");
    assert_eq!(rows.len(), 2);
    let (s, c) = ship::find(&rows, &at(2000), 2000, None).unwrap();
    assert_eq!(
        (s.comp.as_str(), s.ago_s, c.as_str()),
        ("sentinel", Some(1000), "same_boot")
    );
    assert!(
        ship::find(&rows, &at(5000), 5000, None).is_none(),
        "over an hour after"
    );
    // an old line without act_* only in this boot and not after now
    let old = format!(
        "{}\n",
        json!({"kind": "ship", "comp": "core", "txn": "t1", "boot_id": "aaaaaaaa-x", "uptime": 3000})
    );
    assert!(ship::find(&ship::rows(&old, ""), &at(3100), 2900, None).is_none());
    assert!(ship::find(&ship::rows(&old, ""), &at(3100), 3200, None).is_some());
    // across a boot: early in the new one, time uncertain
    let nb = At {
        boot: "eeeeeeee".into(),
        up: 90,
        wall: W0,
        ok: false,
    };
    let (s, c) = ship::find(&rows, &nb, 90, None).unwrap();
    assert_eq!(
        (s.comp.as_str(), s.ago_s, c.as_str()),
        ("sentinel", None, "uncertain")
    );
    // the transaction in flight counts with its promote
    let txn = "txn=t3\ncomp=lweb\nphase=check\nact_boot=aaaaaaaa-x\nact_uptime=1900\n";
    let (s, _) = ship::find(&ship::rows(&manifest, txn), &at(2000), 2000, None).unwrap();
    assert_eq!(s.comp, "lweb");
}

/// CEO review S6: holds run on BOOTTIME; a wall clock step back changes nothing.
#[test]
fn a_wall_clock_step_does_not_move_holds() {
    let mut r = fresh();
    let mut k = chk(0);
    k.cell = Some(Cell {
        registered: false,
        route_up: false,
    });
    for up in (0..=300).step_by(60) {
        k.at = at(up);
        k.at.wall -= if up > 120 { 3600 } else { 0 };
        r.check(&k);
    }
    assert_eq!(kinds(&r), ["regress-cellular"]);
}

#[test]
fn transition_ids_fit_the_queue_and_split_back() {
    let i = interval_id("regress-power-abs", Some("abroad"), &at(5));
    let tid = format!("rg-{i}-close");
    assert!(luyaz_api::alerts::valid_regress_id(&tid), "{tid}");
    assert_eq!(split_tid(&tid), Some((i.as_str(), "close")));
    assert_eq!(split_tid("rg-q12-esc4"), Some(("q12", "esc4")));
    let untrusted = At {
        boot: "abcdef01".into(),
        up: 123_456,
        wall: 5,
        ok: false,
    };
    let tid = format!(
        "rg-{}-open",
        interval_id("regress-core-down", None, &untrusted)
    );
    assert!(
        tid.len() <= 64 && luyaz_api::alerts::valid_regress_id(&tid),
        "{tid}"
    );
}

/// Design §3: a Wi-Fi the watchdog could not bring back stays open until a takeover works.
#[test]
fn wifi_restore_failed_is_open_until_wifi_comes_back() {
    let mut r = fresh();
    r.ingest(
        &[
            ev(1, W0 + 100, 100, "wifi-restore-failed", "x"),
            ev(2, W0 + 400, 400, "wifi-restore-failed", "y"),
        ],
        &at(500),
    );
    assert_eq!(kinds(&r), ["wifi-restore-failed"]);
    assert_eq!(r.file.open[0].alert_seqs, vec![1, 2]);
    r.ingest(&[ev(3, W0 + 900, 900, "wifi-takeover", "ok")], &at(1000));
    assert!(r.file.open.is_empty());
    assert_eq!(r.file.closed[0].end.as_deref(), Some("recovered"));
    assert_eq!(r.file.closed[0].alert_seqs, vec![1, 2, 3]);
}

fn tmp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("u60d-regress-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Eng review OV7: every detector has a positive. Relative standby power: over the baseline's
/// trigger, under the absolute bar.
#[test]
fn relative_standby_power_opens_under_the_absolute_bar() {
    let mut r = fresh();
    let cell = detect::qualifying(&hour_power(W0, 400.0, 0, 1000))[0]
        .0
        .clone();
    r.base.power.insert(cell.clone(), 200.0);
    r.hour(&hour_power(W0, 400.0, 0, 1000), &at(3600));
    assert!(r.file.open.is_empty(), "one hour is not two");
    r.hour(&hour_power(W0 + 3600, 420.0, 0, 1000), &at(7200));
    assert_eq!(kinds(&r), ["regress-power"]);
    assert_eq!(r.file.open[0].sub.as_deref(), Some(cell.as_str()));
    r.hour(&hour_power(W0 + 7200, 210.0, 0, 1000), &at(10_800));
    r.hour(&hour_power(W0 + 10_800, 220.0, 0, 1000), &at(14_400));
    assert!(r.file.open.is_empty());
    assert_eq!(r.file.closed[0].end.as_deref(), Some("recovered"));
}

/// Every way an incident ends queues its evidence to be written again (recovery time, the end).
#[test]
fn every_end_queues_the_evidence_again() {
    // Regress::close (expire and accept go through it)
    let mut r = fresh();
    r.ingest(&[ev(1, W0 + 200, 200, "core-crash", "x")], &at(200));
    let id = r.file.open[0].id.clone();
    super::sync::expire(&mut r, &at(200 + 3700));
    assert_eq!(r.ended, [id]);
    // the sentinel's close from the queue
    let mut r = fresh();
    r.ingest(
        &[
            ev(
                1,
                W0 + 700,
                700,
                "regress-core-down",
                "[rg-core-down-aaaaaaaa-100-open] x",
            ),
            ev(
                2,
                W0 + 900,
                900,
                "regress-core-down",
                "[rg-core-down-aaaaaaaa-100-close] back",
            ),
        ],
        &at(960),
    );
    assert_eq!(
        r.fresh,
        ["core-down-aaaaaaaa-100"],
        "opened and closed in one batch"
    );
    assert_eq!(r.ended, ["core-down-aaaaaaaa-100"]);
    // Wi-Fi back
    let mut r = fresh();
    r.ingest(
        &[
            ev(1, W0 + 200, 200, "wifi-restore-failed", "x"),
            ev(2, W0 + 300, 300, "wifi-takeover", "y"),
        ],
        &at(300),
    );
    assert_eq!(r.ended, ["q1"]);
    // a reboot cutting an interval incident
    let mut r = fresh();
    r.open("regress-cellular", None, &at(500), "x".into(), None, None);
    let f = r.file.clone();
    let r2 = Regress::load(
        Some(f),
        base::Base::default(),
        "1.45.0",
        &at(10),
        0,
        "bbbbbbbb-0",
    );
    assert_eq!(r2.ended.len(), 1);
    assert_eq!(r2.file.closed[0].end.as_deref(), Some("reboot"));
}

/// Design §4: three days open, a relative incident asks once (inbox and MCP only); an absolute
/// one never does.
#[test]
fn three_days_open_asks_once_whether_it_is_the_new_normal() {
    let mut r = fresh();
    r.open(
        "regress-mem",
        None,
        &at(0),
        "heap".into(),
        Some(41_000.0),
        None,
    );
    r.open(
        "regress-mem-abs",
        None,
        &at(0),
        "heap".into(),
        Some(41_000.0),
        None,
    );
    for i in &mut r.file.open {
        i.pending.clear();
    }
    r.ask(&at(3 * 86_400 - 60));
    assert!(r.pending().is_empty(), "not yet");
    r.build = "1.47.0".into();
    r.ask(&at(3 * 86_400 + 60));
    let p = r.pending();
    assert_eq!(p.len(), 1, "{p:?}");
    assert!(
        p[0].id.starts_with("rg-mem-") && p[0].id.ends_with("-ask"),
        "{}",
        p[0].id
    );
    assert!(luyaz_api::alerts::valid_regress_id(&p[0].id));
    assert_eq!(split_tid(&p[0].id).map(|t| t.1), Some("ask"));
    assert!(
        p[0].text.contains("1.45.0>1.47.0") && p[0].text.contains("40MiB"),
        "{}",
        p[0].text
    );
    assert!(p[0].text.len() <= 72 && p[0].text.is_ascii());
    r.queued(&p[0].id.clone(), 9);
    r.ask(&at(6 * 86_400));
    assert!(r.pending().is_empty(), "once");
    assert!(r.file.open[0].alert_seqs.contains(&9));
}

/// Design §4 「前 60 分鐘 ship/演練」: a drill is related like a ship.
#[test]
fn a_drill_is_related_like_a_ship() {
    let mut r = fresh();
    r.drill("rx-quiet", &at(1000));
    let d = r.drill_for(&at(1000 + 1800)).unwrap();
    assert_eq!(
        (d.what.as_str(), d.ago_s, d.clock.as_str()),
        ("rx-quiet", Some(1800), "same_boot")
    );
    assert!(r.drill_for(&at(1000 + 3700)).is_none(), "over an hour");
    assert!(r.drill_for(&at(500)).is_none(), "before the drill");
    // the next boot, early: uncertain, by the wall clocks when trusted
    let next = At {
        boot: "bbbbbbbb".into(),
        up: 60,
        wall: W0 + 1000 + 600,
        ok: true,
    };
    let d = r.drill_for(&next).unwrap();
    assert_eq!((d.ago_s, d.clock.as_str()), (Some(600), "uncertain"));
    let late = At {
        up: 4000,
        ..next.clone()
    };
    assert!(r.drill_for(&late).is_none());
    // kept in the file, the last DRILLS_MAX
    for k in 0..30 {
        r.drill("rx-quiet", &at(2000 + k));
    }
    assert_eq!(r.file.drills.len(), DRILLS_MAX);
    let back: File = serde_json::from_str(&serde_json::to_string(&r.file).unwrap()).unwrap();
    assert_eq!(back.drills.len(), DRILLS_MAX);
    // an older file without drills still loads
    let old = json!({"v": 1, "boot": BOOT, "ingested_seq": 0, "open": [], "closed": []});
    assert!(
        serde_json::from_value::<File>(old)
            .unwrap()
            .drills
            .is_empty()
    );
}

fn hours_file(dir: &std::path::Path, lines: &[serde_json::Value]) {
    let t: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(dir.join("hours.jsonl"), t).unwrap();
}

fn proc_line(ft: i64) -> serde_json::Value {
    json!({"k": "hour_proc", "ft": ft, "tt": ft + 3600, "cpu_core": 3, "heap_live": 9000,
           "anon_core": 30000, "rss_core": 40000, "rss_mihomo": 60000,
           "chill_on_s": 3600, "chill_ok_s": 3500, "ts_on_s": 3600, "ts_chk_s": 3000, "ts_ok_s": 2900})
}

/// X5: the evidence names the end, the recovery time, the exits, the boot's code and the drill;
/// an incident begun before the clock was trusted gets its hours once it is; 8 KB at most.
#[test]
fn evidence_has_the_end_the_exits_and_stays_under_8kb() {
    let d = tmp("evidence");
    let start = W0 + 7 * 3600;
    hours_file(
        &d,
        &(0..7)
            .map(|h| proc_line(W0 + h * 3600 - 3600))
            .collect::<Vec<_>>(),
    );
    let mut r = fresh();
    r.drill("rx-quiet", &at(start - W0 - 600));
    let k = r.open(
        "regress-cellular",
        None,
        &at(start - W0),
        "x".into(),
        None,
        None,
    );
    let from = r.file.open[k].from.clone();
    let drill = r.drill_for(&from);
    r.file.open[k].drill = drill;
    r.close(k, &at(start - W0 + 900), "recovered", "back");
    let i = &r.file.closed[0];
    let facts = io::Facts {
        boot_code: Some(1119),
        planned: None,
    };
    let v: serde_json::Value =
        serde_json::from_str(&io::evidence(i, &d, &at(start - W0 + 900), &facts)).unwrap();
    assert_eq!(v["recovery_s"], 900);
    assert_eq!(v["end"], "recovered");
    assert_eq!(v["boot_code"], 1119);
    // a window that just passed is not a recovery
    let mut q = i.clone();
    q.end = Some("quiet".into());
    let vq: serde_json::Value =
        serde_json::from_str(&io::evidence(&q, &d, &at(start - W0 + 900), &facts)).unwrap();
    assert!(vq["recovery_s"].is_null());
    assert_eq!(v["drill"]["what"], "rx-quiet");
    let h = v["hours"].as_array().unwrap();
    assert!(!h.is_empty());
    assert_eq!(h[0]["chill"], json!([3600, 3500]));
    assert_eq!(h[0]["ts"], json!([3600, 3000, 2900]));
    // begun before the clock was trusted: counted back from a trusted clock in the same boot
    let mut j = i.clone();
    j.from.ok = false;
    j.from.wall = 5;
    let untrusted: serde_json::Value =
        serde_json::from_str(&io::evidence(&j, &d, &at(start - W0 + 900), &facts)).unwrap();
    assert_eq!(untrusted["from_wall"], start);
    assert_eq!(untrusted["hours"].as_array().unwrap().len(), h.len());
    let no_clock = At {
        ok: false,
        ..at(start - W0 + 900)
    };
    let none: serde_json::Value =
        serde_json::from_str(&io::evidence(&j, &d, &no_clock, &facts)).unwrap();
    assert!(none["hours"].as_array().unwrap().is_empty());
    // many hours: the oldest go until it fits
    hours_file(
        &d,
        &(0..200)
            .map(|m| proc_line(start - 6 * 3600 + m * 60))
            .collect::<Vec<_>>(),
    );
    let big = io::evidence(i, &d, &at(start - W0 + 900), &facts);
    assert!(big.len() <= io::EVIDENCE_MAX, "{}", big.len());
    let v: serde_json::Value = serde_json::from_str(&big).unwrap();
    let last = v["hours"].as_array().unwrap().last().unwrap()["ft"]
        .as_i64()
        .unwrap();
    assert_eq!(last, start - 6 * 3600 + 199 * 60, "the newest hours stay");
    let _ = std::fs::remove_dir_all(&d);
}

/// X5 keeping: the newest 50, none older than 30 days; others' files untouched.
#[test]
fn evidence_files_keep_50_and_30_days() {
    let d = tmp("prune");
    let now = std::time::SystemTime::now();
    for k in 0..55u64 {
        let p = d.join(format!("incident-q{k}.json"));
        std::fs::write(&p, "{}").unwrap();
        let f = std::fs::File::options().write(true).open(&p).unwrap();
        f.set_modified(now - std::time::Duration::from_secs(3600 * (60 - k)))
            .unwrap();
    }
    let old = d.join("incident-old.json");
    std::fs::write(&old, "{}").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(now - std::time::Duration::from_secs(31 * 86_400))
        .unwrap();
    std::fs::write(d.join("hours.jsonl"), "").unwrap();
    io::prune_files(
        &d,
        "incident-",
        ".json",
        io::EVIDENCE_KEEP,
        Some(io::EVIDENCE_DAYS),
    );
    let left: Vec<String> = std::fs::read_dir(&d)
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("incident-"))
        .collect();
    assert_eq!(left.len(), 50);
    assert!(!old.exists(), "older than 30 days");
    assert!(!d.join("incident-q0.json").exists() && d.join("incident-q54.json").exists());
    assert!(d.join("hours.jsonl").exists());
    let _ = std::fs::remove_dir_all(&d);
}

/// X2: without smaps_rollup the anonymous sizes are summed from smaps; 4 KB at most.
#[test]
fn memsnap_falls_back_to_summed_smaps() {
    let d = tmp("memsnap");
    let p = d.join("proc");
    std::fs::create_dir_all(&p).unwrap();
    std::fs::write(
        p.join("smaps"),
        "a\nAnonymous:   100 kB\nb\nAnonymous: 23 kB\n",
    )
    .unwrap();
    std::fs::write(
        p.join("status"),
        format!(
            "Name: x\nThreads:\t9\nVmRSS:\t1 kB\n{}",
            "Pad: x\n".repeat(900)
        ),
    )
    .unwrap();
    io::memsnap(&d, &p, "mem-1").unwrap();
    let t = std::fs::read_to_string(d.join("memsnap-mem-1.txt")).unwrap();
    assert!(t.contains("Anonymous (smaps sum): 123 kB"), "{t}");
    assert!(t.contains("Threads:\t9") && t.contains("heap live:"));
    std::fs::write(p.join("smaps_rollup"), "Rss: 5 kB\n".repeat(1000)).unwrap();
    io::memsnap(&d, &p, "mem-2").unwrap();
    let t = std::fs::read_to_string(d.join("memsnap-mem-2.txt")).unwrap();
    assert!(t.starts_with("Rss: 5 kB") && t.chars().count() <= io::MEMSNAP_MAX);
    let _ = std::fs::remove_dir_all(&d);
}

/// Another version or a broken file is renamed aside and the layer starts over.
#[test]
fn another_version_is_renamed_aside() {
    let d = tmp("bak");
    std::fs::write(d.join(FILE), r#"{"v": 99, "open": []}"#).unwrap();
    assert!(io::load_file(&d).is_none());
    assert!(d.join(format!("{FILE}.v99.bak")).exists());
    assert!(!d.join(FILE).exists());
    std::fs::write(d.join(FILE), "not json").unwrap();
    assert!(io::load_file(&d).is_none());
    assert!(d.join(format!("{FILE}.v0.bak")).exists());
    let f = File {
        v: VERSION,
        boot: BOOT.into(),
        ..Default::default()
    };
    std::fs::write(d.join(FILE), serde_json::to_string(&f).unwrap()).unwrap();
    assert_eq!(io::load_file(&d), Some(f));
    let _ = std::fs::remove_dir_all(&d);
}

/// `/v2/incidents/<id>`: a bad id is a 404 before any file is read; one past the 8 days is read
/// back from its evidence (archived), with its times as the list gives them.
#[test]
fn incident_details_refuse_bad_ids_and_read_archived_evidence() {
    let d = tmp("detail");
    let q = d.join("alerts");
    std::fs::create_dir_all(&q).unwrap();
    std::fs::write(d.join("core.env"), "secret").unwrap();
    let file = File::default();
    let long_id = "a".repeat(65);
    for bad in ["../core.env", "", long_id.as_str(), "Q1", "a/b", "a.json"] {
        assert!(!io::valid_id(bad), "{bad}");
        assert!(io::incident_in(&file, &d, bad, &q).is_err(), "{bad}");
    }
    assert!(io::valid_id(&"a".repeat(64)));
    assert!(io::incident_in(&file, &d, "q1", &q).is_err(), "never was");
    let mut r = fresh();
    let k = r.open(
        "regress-cellular",
        None,
        &at(500),
        "down".into(),
        None,
        None,
    );
    r.close(k, &at(1400), "recovered", "back");
    let i = r.file.closed[0].clone();
    let facts = io::Facts {
        boot_code: Some(1185),
        planned: None,
    };
    std::fs::write(
        d.join(format!("incident-{}.json", i.id)),
        io::evidence(&i, &d, &at(1400), &facts),
    )
    .unwrap();
    let v = io::incident_in(&file, &d, &i.id, &q).unwrap();
    assert_eq!(v["archived"], true);
    assert_eq!(v["closed"], true);
    assert_eq!(v["from"], W0 + 500);
    assert_eq!(v["to"], W0 + 1400);
    assert_eq!(v["recovery_s"], 900);
    assert_eq!(v["boot_code"], 1185);
    assert_eq!(v["name_zh"], "蜂窝断线");
    // still kept: from the file, not archived
    let v = io::incident_in(&r.file, &d, &i.id, &q).unwrap();
    assert_eq!(v["archived"], false);
    let _ = std::fs::remove_dir_all(&d);
}
