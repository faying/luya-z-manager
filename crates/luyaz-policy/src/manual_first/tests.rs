//! zte-agent `manual_first.rs`'s tests, moved as they are (only `has_cjk` is local here). The
//! runtime's (`exit_switch_remembers_manual_while_on_backup`, `provider_map_skips_groups`,
//! `connection_filters`, the record contract) are in `crates/luyaz-manual-first` (step 9g).
use super::*;

mod replay;

const JP: &str = "🇯🇵 日本";
const NODE: &str = "JP 03";

#[derive(Default)]
struct Fake {
    in_use: bool,
    probes: Vec<Option<Probe>>,
    cell: bool,
    direct: Option<u32>,
    region: Option<String>,
    retest: Option<(String, bool)>,
    selected: Vec<String>,
    dropped: u32,
    saved: u32,
    probed: u32,
}

impl Io for Fake {
    fn in_use(&mut self) -> bool {
        self.in_use
    }
    fn probe(&mut self, _n: &str) -> Option<Probe> {
        self.probed += 1;
        if self.probes.is_empty() {
            Some(Probe::Ms(100))
        } else {
            self.probes.remove(0)
        }
    }
    fn cell_alive(&mut self) -> bool {
        self.cell
    }
    fn direct_ms(&mut self) -> Option<u32> {
        self.direct
    }
    fn region_of(&mut self, _n: &str) -> Option<String> {
        self.region.clone()
    }
    fn retest(&mut self, _g: &str) -> Option<(String, bool)> {
        self.retest.clone()
    }
    fn select_main(&mut self, m: &str) -> bool {
        self.selected.push(m.to_string());
        true
    }
    fn drop_manual_conns(&mut self) {
        self.dropped += 1;
    }
    fn save(&mut self, _p: &Persist) {
        self.saved += 1;
    }
}

fn healthy() -> Fake {
    Fake {
        in_use: true,
        cell: true,
        direct: Some(60),
        region: Some(JP.into()),
        retest: Some(("JP 01".into(), true)),
        ..Default::default()
    }
}

fn obs(now: u64, main: &str) -> Obs {
    Obs {
        now,
        active: true,
        main_now: main.into(),
        manual_now: NODE.into(),
    }
}

/// Runs one tick per minute from `t`, returning the time after.
fn run(m: &mut Machine, io: &mut Fake, t: u64, n: u32, main: &str) -> u64 {
    let mut t = t;
    for _ in 0..n {
        m.tick(&obs(t, main), io);
        t += 60;
    }
    t
}

fn left(m: &Machine) -> bool {
    m.p.backup == JP
}

#[test]
fn detail_only_when_the_tick_can_use_it() {
    let mut m = Machine::default();
    assert!(m.needs_detail(MANUAL_GROUP));
    assert!(m.needs_detail(JP), "a region not yet remembered");
    assert!(!m.needs_detail(""));
    assert!(!m.needs_detail("DIRECT"));
    m.p.last_region = JP.into();
    assert!(!m.needs_detail(JP), "the remembered region: nothing to do");
    assert!(m.needs_detail("🇺🇸 美国"));
    m.p.backup = JP.into();
    assert!(m.needs_detail(JP), "on our backup: always watch");
    assert!(
        m.needs_detail("DIRECT"),
        "owner moved off the backup: hand off"
    );
}

#[test]
fn tick_without_detail_only_resets_counts() {
    // What observe() hands over when needs_detail is false: no mode, no 🎯.
    let mut m = Machine::default();
    m.p.last_region = JP.into();
    m.timeouts = 1;
    m.bad = 2;
    let mut io = healthy();
    let before = m.p.clone();
    m.tick(
        &Obs {
            now: 1000,
            active: true,
            main_now: JP.into(),
            manual_now: String::new(),
        },
        &mut io,
    );
    assert_eq!(m.p, before);
    assert_eq!((m.timeouts, m.bad), (0, 0));
    assert_eq!((io.probed, io.saved), (0, 0));
    assert!(io.selected.is_empty());
}

#[test]
fn two_timeouts_switch_to_region_and_drop_conns() {
    let mut m = Machine::default();
    let mut io = healthy();
    io.probes = vec![Some(Probe::Timeout), Some(Probe::Timeout)];
    run(&mut m, &mut io, 1000, 2, MANUAL_GROUP);
    assert!(left(&m));
    assert_eq!(io.selected, vec![JP]);
    assert_eq!(io.dropped, 1);
    assert_eq!(io.saved, 1);
    assert!(m.p.notice.contains("JP 03") && m.p.notice.contains(JP));
}

#[test]
fn three_slow_switch_but_two_do_not() {
    let mut m = Machine::default();
    let mut io = healthy();
    io.probes = vec![
        Some(Probe::Ms(900)),
        Some(Probe::Ms(1200)),
        Some(Probe::Ms(100)),
        Some(Probe::Ms(900)),
    ];
    run(&mut m, &mut io, 1000, 4, MANUAL_GROUP);
    assert!(!left(&m), "a good probe in between resets the count");
    io.probes = vec![Some(Probe::Ms(900)), Some(Probe::Ms(950))];
    run(&mut m, &mut io, 2000, 2, MANUAL_GROUP);
    assert!(left(&m));
}

#[test]
fn unreachable_mihomo_is_not_a_failure() {
    let mut m = Machine::default();
    let mut io = healthy();
    io.probes = vec![
        Some(Probe::Timeout),
        None,
        None,
        Some(Probe::Ms(100)),
        Some(Probe::Timeout),
    ];
    run(&mut m, &mut io, 1000, 5, MANUAL_GROUP);
    assert!(!left(&m));
}

#[test]
fn idle_means_no_probe() {
    let mut m = Machine::default();
    let mut io = healthy();
    io.in_use = false;
    run(&mut m, &mut io, 1000, 10, MANUAL_GROUP);
    assert_eq!(io.probed, 0);
}

#[test]
fn probes_at_most_once_a_minute() {
    let mut m = Machine::default();
    let mut io = healthy();
    for t in (1000..1060).step_by(10) {
        m.tick(&obs(t, MANUAL_GROUP), &mut io);
    }
    assert_eq!(io.probed, 1);
}

#[test]
fn no_switch_when_cell_down_or_direct_slow() {
    for (cell, direct) in [(false, Some(50)), (true, Some(2000)), (true, None)] {
        let mut m = Machine::default();
        let mut io = healthy();
        io.cell = cell;
        io.direct = direct;
        io.probes = vec![Some(Probe::Timeout); 6];
        run(&mut m, &mut io, 1000, 6, MANUAL_GROUP);
        assert!(!left(&m), "cell={cell} direct={direct:?}");
        assert!(io.selected.is_empty());
    }
}

#[test]
fn backup_that_picks_the_dead_node_or_fails_is_not_used() {
    for r in [
        Some((NODE.to_string(), true)),
        Some(("JP 01".to_string(), false)),
        None,
    ] {
        let mut m = Machine::default();
        let mut io = healthy();
        io.retest = r.clone();
        io.probes = vec![Some(Probe::Timeout); 2];
        run(&mut m, &mut io, 1000, 2, MANUAL_GROUP);
        assert!(!left(&m), "{r:?}");
        assert!(m.p.notice.contains("都不通"));
    }
}

#[test]
fn unknown_region_uses_last_auto_region() {
    let mut m = Machine::default();
    let mut io = healthy();
    m.tick(&obs(500, "🇸🇬 新加坡"), &mut io);
    io.region = None;
    io.probes = vec![Some(Probe::Timeout); 2];
    run(&mut m, &mut io, 1000, 2, MANUAL_GROUP);
    assert_eq!(m.p.backup, "🇸🇬 新加坡");
}

#[test]
fn returns_after_ten_good_minutes_only() {
    let mut m = Machine::default();
    let mut io = healthy();
    io.probes = vec![Some(Probe::Timeout); 2];
    let t = run(&mut m, &mut io, 1000, 2, MANUAL_GROUP);
    assert!(left(&m));
    // On backup: probes every 2 min. One failure restarts the 10 minutes.
    io.probes = vec![
        Some(Probe::Ms(100)),
        Some(Probe::Ms(100)),
        Some(Probe::Timeout),
    ];
    let t = run(&mut m, &mut io, t + 120, 6, JP);
    assert!(left(&m));
    io.probes = vec![];
    let t = run(&mut m, &mut io, t, 10, JP); // 9 minutes of good probes
    assert!(left(&m), "not 10 minutes yet");
    run(&mut m, &mut io, t, 4, JP);
    assert!(!left(&m));
    assert_eq!(io.selected.last().map(String::as_str), Some(MANUAL_GROUP));
    assert!(m.p.notice.contains("恢复"));
}

#[test]
fn owner_changing_main_stops_takeover() {
    let mut m = Machine::default();
    let mut io = healthy();
    io.probes = vec![Some(Probe::Timeout); 2];
    let t = run(&mut m, &mut io, 1000, 2, MANUAL_GROUP);
    assert!(left(&m));
    m.tick(&obs(t, "DIRECT"), &mut io);
    assert!(m.p.backup.is_empty());
    assert!(m.p.notice.is_empty());
    let n = io.selected.len();
    run(&mut m, &mut io, t + 60, 20, "DIRECT");
    assert_eq!(
        io.selected.len(),
        n,
        "never switches back on its own afterwards"
    );
}

/// Every notice has an English twin, set and cleared with it. Node and
/// group names (🇯🇵 日本) are proper nouns and are the only CJK allowed.
#[test]
fn notices_come_in_both_languages() {
    let en_ok = |m: &Machine| {
        let rest =
            m.p.notice_en
                .replace(JP, "")
                .replace(NODE, "")
                .replace("JP 07", "");
        assert!(
            !m.p.notice_en.is_empty() && !has_cjk(&rest) && !rest.ends_with('.'),
            "{}",
            m.p.notice_en
        );
    };
    let mut m = Machine::default();
    let mut io = healthy();
    io.probes = vec![Some(Probe::Timeout); 2];
    let t = run(&mut m, &mut io, 1000, 2, MANUAL_GROUP);
    assert!(left(&m));
    en_ok(&m);
    assert!(m.p.notice_en.contains(NODE) && m.p.notice_en.contains(JP));
    assert_eq!(m.p.notice_kind, "backup");
    // back after ten good minutes
    io.probes = vec![];
    run(&mut m, &mut io, t + 120, 14, JP);
    assert!(!left(&m) && m.p.notice.contains("恢复"));
    en_ok(&m);
    assert_eq!(m.p.notice_kind, "back");
    // a new pick while on the backup
    let mut m = Machine::default();
    io.probes = vec![Some(Probe::Timeout); 2];
    let t = run(&mut m, &mut io, 1000, 2, MANUAL_GROUP);
    m.tick(
        &Obs {
            now: t,
            active: true,
            main_now: JP.into(),
            manual_now: "JP 07".into(),
        },
        &mut io,
    );
    en_ok(&m);
    assert_eq!(m.p.notice_kind, "repick");
    // both down; capped
    for (retest, want) in [
        (Some(("x".to_string(), false)), "both down"),
        (None, "both down"),
    ] {
        let mut m = Machine::default();
        let mut io = healthy();
        io.retest = retest;
        io.probes = vec![Some(Probe::Timeout); 2];
        run(&mut m, &mut io, 1000, 2, MANUAL_GROUP);
        en_ok(&m);
        assert!(m.p.notice_en.contains(want), "{}", m.p.notice_en);
        assert_eq!(m.p.notice_kind, "alldown");
    }
    let mut m = Machine::default();
    m.p.switches = vec![990, 995];
    let mut io = healthy();
    io.probes = vec![Some(Probe::Timeout); 2];
    run(&mut m, &mut io, 1000, 2, MANUAL_GROUP);
    en_ok(&m);
    assert!(m.p.notice_en.contains("staying put"));
    assert_eq!(m.p.notice_kind, "unstable");
    // the owner taking over clears both
    let mut m = Machine::default();
    let mut io = healthy();
    io.probes = vec![Some(Probe::Timeout); 2];
    let t = run(&mut m, &mut io, 1000, 2, MANUAL_GROUP);
    m.tick(&obs(t, "DIRECT"), &mut io);
    assert!(m.p.notice.is_empty() && m.p.notice_en.is_empty() && m.p.notice_kind.is_empty());
}

#[cfg(feature = "serde")]
#[test]
fn state_file_from_before_l2_still_loads() {
    let old = r#"{"backup":"🇯🇵 日本","manual":"JP 03","switched_at":5,"switches":[5],
                  "last_region":"","notice":"手选的 JP 03 不通，已临时换到 🇯🇵 日本（自动）","notice_at":5}"#;
    let p: Persist = serde_json::from_str(old).unwrap();
    assert_eq!(p.backup, JP);
    assert!(p.notice_en.is_empty() && p.notice_kind.is_empty());
}

#[test]
fn new_hand_pick_while_on_backup_goes_back_to_manual() {
    let mut m = Machine::default();
    let mut io = healthy();
    io.probes = vec![Some(Probe::Timeout); 2];
    let t = run(&mut m, &mut io, 1000, 2, MANUAL_GROUP);
    let o = Obs {
        now: t,
        active: true,
        main_now: JP.into(),
        manual_now: "JP 07".into(),
    };
    m.tick(&o, &mut io);
    assert!(m.p.backup.is_empty());
    assert_eq!(io.selected.last().map(String::as_str), Some(MANUAL_GROUP));
    assert!(m.p.notice.contains("JP 07"));
}

#[test]
fn inactive_chill_does_nothing_and_keeps_state() {
    let mut m = Machine::default();
    let mut io = healthy();
    io.probes = vec![Some(Probe::Timeout); 2];
    let t = run(&mut m, &mut io, 1000, 2, MANUAL_GROUP);
    m.tick(
        &Obs {
            now: t,
            ..Default::default()
        },
        &mut io,
    );
    assert!(left(&m));
}

#[test]
fn at_most_two_switches_an_hour() {
    let mut m = Machine::default();
    let mut io = healthy();
    let mut t = 1000;
    for _ in 0..2 {
        io.probes = vec![Some(Probe::Timeout); 2];
        t = run(&mut m, &mut io, t, 2, MANUAL_GROUP);
        assert!(left(&m));
        m.tick(&obs(t, MANUAL_GROUP), &mut io); // owner put it back by hand
        t += 60;
    }
    io.probes = vec![Some(Probe::Timeout); 2];
    run(&mut m, &mut io, t, 2, MANUAL_GROUP);
    assert!(!left(&m));
    assert!(m.p.notice.contains("这次不换"));
}

#[test]
fn restart_resumes_waiting_to_return() {
    let p = Persist {
        backup: JP.into(),
        manual: NODE.into(),
        switched_at: 900,
        ..Default::default()
    };
    let mut m = Machine::new(p);
    let mut io = healthy();
    run(&mut m, &mut io, 1000, 14, JP);
    assert!(!left(&m));
    assert_eq!(io.selected, vec![MANUAL_GROUP]);
}

/// zte-agent `health.rs`'s.
fn has_cjk(s: &str) -> bool {
    s.chars().any(|c| {
        matches!(c as u32,
            0x2E80..=0x2FFF | 0x3000..=0x303F | 0x3040..=0x30FF | 0x3100..=0x9FFF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF)
    })
}
