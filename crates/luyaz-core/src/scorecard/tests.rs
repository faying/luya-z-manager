use super::awk::*;
use super::*;

#[test]
fn awk_fld_takes_the_first_key_and_the_longer_alternative() {
    let l = r#"{"v":1,"seq":2,"n":3,"up":4.5,"t":null,"k":"ssr","id":"ssr-a-1","line":"a,b","on_home_e":7,"e":9}"#;
    assert_eq!(fld(l, "t"), "null");
    assert_eq!(fld(l, "line"), "a,b");
    assert_eq!(fld(l, "e"), "9");
    assert_eq!(fld(l, "nope"), "");
    // escaped quotes and a missing closing quote, as busybox gives them (probed 2026-10-10)
    assert_eq!(fld(r#"{"x":"a\"b,c"}"#, "x"), r#"a\""#);
    assert_eq!(fld(r#"{"x":"a\"b"}"#, "x"), r#"a\"b"#);
    assert_eq!(fld(r#"{"x":"ab}"#, "x"), "a");
}

#[test]
fn awk_numbers_as_busybox() {
    assert_eq!(num("0x10"), 16.0);
    assert_eq!(num(" 12abc"), 12.0);
    assert_eq!(num("1e3"), 1000.0);
    assert_eq!(num("null"), 0.0);
    assert_eq!(num(""), 0.0);
    assert!(isn("-1.5") && isn("3") && !isn("1.") && !isn("") && !isn("null"));
    // printf rounding probed in the busybox container: 0 2 2 0.2 0.3
    let r: Vec<String> = [0.5, 1.5, 2.5].iter().map(|&x| fixed(x, 0)).collect();
    assert_eq!(r, ["0", "2", "2"]);
    assert_eq!(fixed(0.25, 1), "0.2");
    assert_eq!(fixed(0.35, 1), "0.3");
    assert_eq!(int(13.9), 13);
    assert_eq!(int(-2.5), -2);
    assert_eq!(mktime("2026 10 10 01 02 03"), 1_791_594_123.0);
    assert_eq!(mktime("null"), -1.0);
    assert_eq!(mktime("2026 10 10"), -1.0);
    assert_eq!(md_hm(0.0), "01-01 00:00");
}

#[test]
fn head_test_as_doctor() {
    assert!(head_ok(
        r#"{"v":1,"seq":1,"n":1,"up":5,"t":null,"k":"hour","id":"x"}"#
    ));
    assert!(!head_ok("not json"));
    assert!(!head_ok(
        r#"{"v":1,"seq":1,"n":1,"up":5,"t":"x","k":"hour"}"#
    ));
}

#[test]
fn core_layer_becomes_ledger_lines() {
    let hours = r#"{"k":"hour","id":"h-aaaaaaaa-0","boot":"aaaaaaaa","from":0,"to":3600,"ft":1790000000,"tt":1790003600,"awake":3600,"abroad":0}"#;
    let events = concat!(
        r#"{"t":null,"up":5,"boot":"aaaaaaaa","k":"boot","boot_id":"aaaaaaaa-1"}"#,
        "\n",
        r#"{"t":1790007200,"up":7200,"boot":"bbbbbbbb","k":"boot","boot_id":"bbbbbbbb-2"}"#
    );
    let l = core::lines(hours, events);
    assert_eq!(l.len(), 3);
    assert!(
        l[0].starts_with(r#"{"v":1,"seq":1,"n":1,"up":5,"t":null,"k":"boot""#),
        "{}",
        l[0]
    );
    assert!(
        l[1].starts_with(r#"{"v":1,"seq":1,"n":2,"up":3600,"t":1790003600,"k":"hour""#),
        "{}",
        l[1]
    );
    assert!(l[2].contains(r#""seq":2"#));
    let mut c = Card::new();
    for x in &l {
        c.line(x);
    }
    let (text, rc) = c.finish(&Inputs {
        w: 86_400.0,
        label: "24 小时".into(),
        ..Default::default()
    });
    assert_eq!(rc, 0, "{text}");
    assert!(text.contains("账本：2 次开机，1 小时的记录"), "{text}");
}

#[test]
fn a_nul_ends_a_line_as_in_busybox_awk() {
    // busybox: printf 'a\0\0\0b\nc\n' | awk '{print}' gives a, "", "", b, c
    let v: Vec<String> = guard_lines(b"a\0\0\0b\nc\n").collect();
    assert_eq!(v, ["a", "", "", "b", "c"]);
    assert_eq!(guard_lines(b"x\n\0\0").count(), 3);
    assert_eq!(guard_lines(b"").count(), 0);
    assert_eq!(guard_lines(b"\n").count(), 1);
}

#[test]
fn live_inputs_read_as_doctor_does() {
    use super::live::{cap_end, health, print_num, thr};
    // the last kmsg record of the last 40 lines decides; other lines in between do not
    assert_eq!(
        cap_end(b"6,1,0,-;a\n3,2,0,-;modem: FATAL ERROR RECEIVED\nnoise\n"),
        1
    );
    assert_eq!(cap_end(b"3,2,0,-;rproc recovery\n6,3,0,-;fine\n"), 0);
    assert_eq!(cap_end(b"3,2,0,-;err_fatal"), 1); // no final newline: still a line
    assert_eq!(cap_end(b"x,2,0;err_fatal\n"), 0);
    assert_eq!(cap_end(b""), 0);
    let mut old = String::from("3,1,0,-;watchdog received\n");
    for i in 0..40 {
        old.push_str(&format!("more {i}\n"));
    }
    assert_eq!(cap_end(old.as_bytes()), 0); // past tail -n 40
    // busybox: print 24+3*3 → 33, 1394.7+3*21.2 → 1458.3, 0.1+3e-7 → 0.1, 1e7 → 10000000
    assert_eq!(thr("2 24.0 3.0\n3 0 0\n"), "33");
    assert_eq!(thr("4 1 1\n2 1394.7 21.2\n"), "1458.3");
    assert_eq!(thr("02 5 1\n2 - 1\n"), "");
    assert_eq!(print_num(0.1000003), "0.1");
    assert_eq!(print_num(1e7), "10000000");
    assert_eq!(print_num(1234567.5), "1.23457e+06");
    assert_eq!(print_num(0.00001234), "1.234e-05");
    let mut inp = Inputs::default();
    health(&mut inp, "\nWi-Fi：没有在广播\n");
    assert_eq!(inp.standby.as_deref(), Some(""));
    assert_eq!(inp.warn.as_deref(), Some("Wi-Fi：没有在广播\n"));
    health(&mut inp, "x\t规则成立\n");
    assert_eq!(inp.standby.as_deref(), Some("x\t规则成立\n"));
    assert_eq!(inp.warn.as_deref(), Some(""));
}

#[test]
fn without_the_health_check_its_rows_read_not_measured() {
    let mut c = Card::new();
    c.line(r#"{"v":1,"seq":1,"n":1,"up":5,"t":1790000005,"k":"boot","boot":"aaaaaaaa"}"#);
    c.line(r#"{"v":1,"seq":1,"n":2,"up":3600,"t":1790003600,"k":"hour","id":"h1","from":5,"to":3600,"ft":1790000005,"tt":1790003600,"awake":3600}"#);
    let (text, _) = c.finish(&Inputs {
        w: 86_400.0,
        label: "24 小时".into(),
        ..Default::default()
    });
    assert!(text.contains("现在 ▲/■ 没测（没带体检）"), "{text}");
    let p3 = text.lines().find(|l| l.contains("P3")).unwrap();
    assert!(p3.contains("没带体检") && p3.contains("没测"), "{p3}");
}

#[test]
fn a_power_cuts_nuls_count_once_a_segment() {
    let mut c = Card::new();
    c.segment();
    c.line(r#"{"v":1,"seq":1,"n":1,"up":5,"t":1790000005,"k":"boot","boot":"aaaaaaaa"}"#);
    c.line(r#"{"v":1,"seq":1,"n":2,"up":3600,"t":1790003600,"k":"hour","id":"h1","from":5,"to":3600,"ft":1790000005,"tt":1790003600,"awake":3600}"#);
    for l in guard_lines(b"\0\0\0{\"v\":1,broken\n") {
        c.line(&l);
    }
    c.segment();
    c.line("");
    let (text, _) = c.finish(&Inputs {
        w: 86_400.0,
        label: "24 小时".into(),
        ..Default::default()
    });
    assert!(
        text.contains("；2 段有断电留下的空字节，已跳过；1 行读不懂，已跳过"),
        "{text}"
    );
}

/// The long-term fold's own lines, through the adapter, into the card (u60-sentinel.md §19
/// 「T9c 前定」): a restart in the same boot is not a reboot; the core's coverage is the card's
/// `guard`; sources it does not watch, and hour lines written before T9c, are not 完整; the core
/// gone across an hour turn is an hour with nothing covered.
#[test]
fn core_coverage_reaches_the_card() {
    use crate::longterm::{Clock, Fold, Out, Report};
    let rpt = |a: i64| Report {
        alive: Some(a),
        looks: ["crashlog", "uidlog"]
            .iter()
            .map(|s| (s.to_string(), true))
            .collect(),
        ..Default::default()
    };
    const W: i64 = 1_791_500_400;
    let c = |up: i64| Clock {
        boot: "b1b1b1b1-x".into(),
        boottime: up,
        mono: up,
        wall: W + up,
        wall_ok: true,
        clock_status: luyaz_api::clock::EvidenceStatus::Estimated,
        evidence: None,
        how: None,
    };
    let text = |v: &[serde_json::Value]| v.iter().map(|x| x.to_string() + "\n").collect::<String>();
    let card = |hours: &str, events: &str| {
        let mut k = Card::new();
        for l in core::lines(hours, events) {
            k.line(&l);
        }
        k.finish(&Inputs {
            w: 86_400.0,
            label: "24 小时".into(),
            ..Default::default()
        })
        .0
    };
    let row = |t: &str, id: &str| t.lines().find(|l| l.contains(id)).unwrap_or("").to_string();
    // two hours, checks answered, a restart (30 s away) in the middle of the second
    let mut f = Fold::new("v1");
    let mut out = Out::default();
    // the fuel gauge: 100 µAh a minute at 4 V = 400 µWh a minute = 24 mWh an hour, screen off
    let pw = |t: i64| crate::longterm::power::Read {
        v: Some(4_000_000),
        q: Some(10_000_000 - t / 60 * 100),
        ..Default::default()
    };
    for t in (0..=5400).step_by(60) {
        f.cover(&c(t), &rpt(t), &mut out);
        f.power_with(&c(t), &pw(t));
    }
    let mut f = Fold::resume(&f.save(), "v1", "b1b1b1b1-x").unwrap();
    for t in (5430..=10_800).step_by(60) {
        f.cover(&c(t), &rpt(t), &mut out);
    }
    f.tick(&c(10_800), &mut out);
    let (h, e) = (text(&out.hours), text(&out.events));
    let t = card(&h, &e);
    assert!(t.contains("账本：1 次开机，3 小时的记录"), "{t}");
    let p1 = row(&t, "P1");
    assert!(p1.contains("覆盖完整"), "{p1}\n{t}");
    assert!(
        p1.contains("平均 24 mW"),
        "the core's hour_power reaches the card: {p1}\n{t}"
    );
    let u = row(&t, "S5b");
    assert!(
        u.contains("覆盖完整"),
        "the minute look said uidlog worked: {u}\n{t}"
    );
    // the same hours as written before T9c (no g_*): not complete
    let old: String = h
        .lines()
        .map(|l| {
            let mut v: serde_json::Value = serde_json::from_str(l).unwrap();
            v.as_object_mut()
                .unwrap()
                .retain(|k, _| !k.starts_with("g_"));
            v.to_string() + "\n"
        })
        .collect();
    let p1 = row(&card(&old, &e), "P1");
    assert!(p1.contains("覆盖不完整"), "{p1}");
    // gone across an hour turn: one hour line with nothing covered, a long gap
    let mut f = Fold::new("v1");
    let mut out = Out::default();
    for t in (0..=3000).step_by(60) {
        f.cover(&c(t), &rpt(t), &mut out);
    }
    let mut f = Fold::resume(&f.save(), "v1", "b1b1b1b1-x").unwrap();
    for t in (6000..=7200).step_by(60) {
        f.cover(&c(t), &rpt(t), &mut out);
    }
    f.tick(&c(7200), &mut out);
    let t = card(&text(&out.hours), &text(&out.events));
    assert!(t.contains("账本：1 次开机，3 小时的记录"), "{t}");
    let p1 = row(&t, "P1");
    assert!(
        p1.contains("覆盖不完整") && p1.contains("超过 10 分钟"),
        "{p1}\n{t}"
    );
}

/// The stitch (u60-sentinel.md §19 T9d): the guard's hours up to the cut, the core's after it,
/// one boot under the guard's number across both, a boot only the core saw after it.
#[test]
fn the_stitch_takes_each_side_of_the_cut() {
    const W: i64 = 1_791_500_400;
    let d = std::env::temp_dir().join(format!("u60d-stitch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("ledger")).unwrap();
    std::fs::create_dir_all(d.join("long")).unwrap();
    let ghour = |n: i64, i: i64| {
        let (f, t) = (i * 3600, i * 3600 + 3603);
        format!(
            "{{\"v\":1,\"seq\":3,\"n\":{n},\"up\":{t},\"t\":{},\"k\":\"hour\",\"id\":\"h-b1b1b1b1-{f}\",\"from\":{f},\"to\":{t},\"ft\":{},\"tt\":{},\"awake\":3603,\"asleep\":0}}\n",
            W + t,
            W + f,
            W + t
        )
    };
    let mut g = String::from(
        "{\"v\":1,\"seq\":3,\"n\":0,\"up\":50,\"t\":null,\"k\":\"boot\",\"boot\":\"b1b1b1b1-x\",\"code\":1114}\n",
    );
    for i in 0..4 {
        g += &ghour(i + 1, i);
    }
    // the guard's backfilled last hour, written at the next boot without a clock in `t`: after
    // the cut by its `tt`, so not taken (the core has its own)
    g += &format!(
        "{{\"v\":1,\"seq\":3,\"n\":9,\"up\":18000,\"t\":null,\"k\":\"hour\",\"id\":\"h-b1b1b1b1-14400\",\"from\":14400,\"to\":18000,\"ft\":{},\"tt\":{},\"awake\":3600,\"asleep\":0}}\n",
        W + 14_400,
        W + 18_000
    );
    std::fs::write(d.join("ledger/boot-000003-b1b1b1b1-001.jsonl"), g).unwrap();
    let chour = |b: &str, base: i64, i: i64| {
        let (f, t) = (i * 3600, i * 3600 + 3600);
        format!(
            "{{\"k\":\"hour\",\"boot\":\"{b}\",\"id\":\"h-{b}-{f}\",\"from\":{f},\"to\":{t},\"ft\":{},\"tt\":{},\"awake\":3600,\"asleep\":0}}\n",
            base + f,
            base + t
        )
    };
    // the core from its go-live in hour 1; a new boot after the guard's last hour
    let mut h = String::new();
    for i in 1..5 {
        h += &chour("b1b1b1b1", W, i);
    }
    let w2 = W + 6 * 3600;
    h += &chour("c2c2c2c2", w2, 0);
    let e = format!(
        "{{\"k\":\"net_change\",\"boot\":\"b1b1b1b1\",\"up\":4000,\"t\":{},\"net\":\"460-11\"}}\n{{\"k\":\"boot\",\"boot\":\"c2c2c2c2\",\"up\":55,\"t\":{},\"code\":1112}}\n",
        W + 4000,
        w2 + 55
    );
    std::fs::write(d.join("long/hours.jsonl"), h).unwrap();
    std::fs::write(d.join("long/events.jsonl"), e).unwrap();
    let inp = Inputs {
        w: 604_800.0,
        label: "7 天".into(),
        ..Default::default()
    };
    let (t, _) = replay::stitch(
        &d.join("ledger"),
        &d.join("long"),
        (W + 2 * 3600) as f64,
        &inp,
    );
    // guard hours 0 and 1 (1 ends 3 s after the cut), core hours 2–4 and the new boot's
    assert!(t.contains("账本：2 次开机，6 小时的记录"), "{t}");
    // the core's net_change at 4000 s is before the cut: not taken
    let (all, _) = replay::stitch(&d.join("ledger"), &d.join("long"), W as f64 - 7200.0, &inp);
    assert!(all.contains("账本：2 次开机，5 小时的记录"), "{all}");
    let _ = std::fs::remove_dir_all(&d);
}

/// A reboot someone asked for (`planned` on the boot line it starts, boot.rs) is no unexpected
/// reboot whatever its code: CN B31 logs a shell reboot as 1134, the modem-crash code.
#[test]
fn a_planned_reboot_is_manual_whatever_its_code() {
    let card = |planned: &str| {
        let mut c = Card::new();
        c.line(r#"{"v":1,"seq":1,"n":1,"up":5,"t":1790000005,"k":"boot","boot":"aaaaaaaa","code":1150}"#);
        c.line(r#"{"v":1,"seq":1,"n":2,"up":3600,"t":1790003600,"k":"hour","id":"h1","from":5,"to":3600,"ft":1790000005,"tt":1790003600,"awake":3600}"#);
        c.line(&format!(
            r#"{{"v":1,"seq":2,"n":1,"up":60,"t":1790003700,"k":"boot","boot":"bbbbbbbb","code":1134{planned}}}"#
        ));
        c.line(r#"{"v":1,"seq":2,"n":2,"up":3600,"t":1790007240,"k":"hour","id":"h2","from":60,"to":3600,"ft":1790003700,"tt":1790007240,"awake":3540}"#);
        let (text, _) = c.finish(&Inputs {
            w: 86_400.0,
            label: "24 小时".into(),
            ..Default::default()
        });
        text.lines()
            .find(|l| l.contains("S1 "))
            .unwrap()
            .to_string()
    };
    let crash = card("");
    assert!(crash.contains("S1 意外整机重启：1 次"), "{crash}");
    let asked = card(r#","planned":"ssh:test""#);
    assert!(
        asked.contains("S1 意外整机重启：0 次（另有人为 1 次）"),
        "{asked}"
    );
}
