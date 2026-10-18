use super::*;

fn reading(n: u64) -> Read {
    Read {
        process: Some(Process {
            pid: 10,
            start: 5,
            cpu: n * 100,
            children: Some(n * 10),
        }),
        system: System {
            busy: Some(n * 500),
            total: Some(n * 24_000),
            tasks: Some(n * 120),
            ctxt: Some(n * 600),
        },
        hz: Some(100),
    }
}

fn stat(pid: u32, comm: &str, cpu: u64, children: &str, start: u64) -> String {
    let mut fields = vec!["0".to_string(); 20];
    fields[0] = "S".into();
    fields[11] = cpu.to_string();
    fields[13] = children.into();
    fields[19] = start.to_string();
    format!("{pid} ({comm}) {}", fields.join(" "))
}

#[test]
fn proc_parsers_preserve_unknown_and_avoid_guest_double_counting() {
    let p = process(&stat(10, "name ) with spaces", 23, "7", 9)).unwrap();
    assert_eq!((p.pid, p.cpu, p.children, p.start), (10, 23, Some(7), 9));
    assert_eq!(
        process(&stat(10, "core", 23, "-1", 9)).unwrap().children,
        None
    );
    assert_eq!(process("10 (truncated) S 1"), None);
    let s = system("cpu 100 20 30 400 50 60 70 80 90 10\nprocesses 123\nctxt 0\n");
    assert_eq!(
        (s.busy, s.total, s.tasks, s.ctxt),
        (Some(360), Some(810), Some(123), Some(0))
    );
    assert_eq!(system("cpu 1 2 broken\nprocesses invalid\n").total, None);
    assert_eq!(system("").tasks, None);
    assert_eq!(system("cpu 18446744073709551615 1 0 0 0 0 0 0").total, None);
}

#[test]
fn units_valid_zero_and_separate_children_are_preserved() {
    let mut a = Acc::default();
    a.observe(Some(&reading(1)), "boot", 60, 60);
    a.observe(Some(&reading(2)), "boot", 120, 120);
    let v = a.value();
    assert_eq!(v["task_creation"]["count"], 120);
    assert_eq!(v["task_creation"]["per_second"], 2.0);
    assert_eq!(v["core_cpu"]["cpu_s"], 1.0);
    assert_eq!(v["reaped_children_cpu"]["cpu_s"], 0.1);
    assert_eq!(v["system_cpu"]["cpu_s"], 5.0);
    assert!(
        (v["system_cpu"]["all_core_percent"].as_f64().unwrap() - 100.0 * 500.0 / 24000.0).abs()
            < 1e-9
    );
    assert!(v["true_wakeups"].is_null());
    a.observe(Some(&reading(2)), "boot", 180, 180);
    assert_eq!(a.value()["task_creation"]["observed_s"], 120);
    assert_eq!(a.value()["task_creation"]["per_second"], 1.0);
    assert!(Acc::default().value()["core_cpu"]["ticks"].is_null());
}

#[test]
fn runtime_hz_and_multicore_cpu_do_not_assume_100_or_cap_at_one_core() {
    let mut a = Acc::default();
    let mut first = reading(1);
    first.hz = Some(250);
    first.process.as_mut().unwrap().cpu = 250;
    a.observe(Some(&first), "boot", 60, 60);
    let mut next = reading(2);
    next.hz = Some(250);
    next.process.as_mut().unwrap().cpu = 45_250;
    a.observe(Some(&next), "boot", 120, 120);
    assert_eq!(a.value()["core_cpu"]["cpu_s"], 180.0);
    assert_eq!(a.value()["core_cpu"]["single_core_percent"], 300.0);
}

#[test]
fn missing_field_breaks_only_its_own_coverage_and_does_not_bridge() {
    let mut a = Acc::default();
    a.observe(Some(&reading(1)), "boot", 60, 60);
    let mut missing = reading(2);
    missing.system.tasks = None;
    a.observe(Some(&missing), "boot", 120, 120);
    a.observe(Some(&reading(3)), "boot", 180, 180);
    assert!(a.value()["task_creation"]["count"].is_null());
    assert_eq!(a.value()["context_switches"]["observed_s"], 120);
    a.observe(Some(&reading(4)), "boot", 240, 240);
    assert_eq!(a.value()["task_creation"]["count"], 120);
    assert_eq!(a.value()["task_creation"]["observed_s"], 60);
}

#[test]
fn missing_sample_and_counter_reset_rebaseline() {
    let mut a = Acc::default();
    a.observe(Some(&reading(1)), "boot", 60, 60);
    a.observe(None, "boot", 120, 120);
    a.observe(Some(&reading(3)), "boot", 180, 180);
    assert!(a.value()["core_cpu"]["ticks"].is_null());
    a.observe(Some(&reading(4)), "boot", 240, 240);
    a.observe(Some(&reading(1)), "boot", 300, 300);
    assert_eq!(a.value()["core_cpu"]["observed_s"], 60);
    assert_eq!(a.value()["rejected"]["core_cpu_counter_reset"], 1);
    a.observe(Some(&reading(2)), "boot", 360, 360);
    assert_eq!(a.value()["core_cpu"]["observed_s"], 120);
}

#[test]
fn restart_pid_reuse_and_boot_change_never_bridge() {
    for (pid, start, boot) in [(11, 5, "boot"), (10, 6, "boot"), (10, 5, "other")] {
        let mut a = Acc::default();
        a.observe(Some(&reading(1)), "boot", 60, 60);
        let mut next = reading(2);
        let p = next.process.as_mut().unwrap();
        p.pid = pid;
        p.start = start;
        a.observe(Some(&next), boot, 120, 120);
        assert!(a.value()["task_creation"]["count"].is_null());
    }
}

#[test]
fn suspend_uses_boottime_but_unobserved_awake_gap_is_rejected() {
    let mut a = Acc::default();
    a.observe(Some(&reading(1)), "boot", 60, 60);
    a.observe(Some(&reading(2)), "boot", 3660, 120);
    assert_eq!(a.value()["task_creation"]["observed_s"], 3600);
    assert!(
        (a.value()["task_creation"]["per_second"].as_f64().unwrap() - 120.0 / 3600.0).abs() < 1e-9
    );
    a.observe(Some(&reading(3)), "boot", 3841, 301);
    assert_eq!(a.value()["rejected"]["awake_gap"], 1);
    assert_eq!(a.value()["task_creation"]["observed_s"], 3600);
}

#[test]
fn invalid_clocks_and_hz_are_not_zero_measurements() {
    for (up, mono) in [(60, 60), (59, 61), (120, 121), (0, 0), (-1, -1)] {
        let mut a = Acc::default();
        a.observe(Some(&reading(1)), "boot", 60, 60);
        a.observe(Some(&reading(2)), "boot", up, mono);
        assert!(a.value()["task_creation"]["count"].is_null());
    }
    for hz in [None, Some(0), Some(1000)] {
        let mut a = Acc::default();
        a.observe(Some(&reading(1)), "boot", 60, 60);
        let next = Read { hz, ..reading(2) };
        a.observe(Some(&next), "boot", 120, 120);
        assert!(a.value()["core_cpu"]["ticks"].is_null());
    }
}

#[test]
fn saved_totals_survive_but_previous_sample_is_never_restored() {
    let mut a = Acc::default();
    a.observe(Some(&reading(1)), "boot", 60, 60);
    a.observe(Some(&reading(2)), "boot", 120, 120);
    let saved = serde_json::to_string(&a).unwrap();
    assert!(!saved.contains("prev"));
    let mut b: Acc = serde_json::from_str(&saved).unwrap();
    b.observe(Some(&reading(3)), "boot", 180, 180);
    assert_eq!(b.value()["task_creation"]["observed_s"], 60);
    b.observe(Some(&reading(4)), "boot", 240, 240);
    assert_eq!(b.value()["task_creation"]["observed_s"], 120);
    let old: Acc = serde_json::from_str("{}").unwrap();
    assert!(old.value()["core_cpu"]["ticks"].is_null());
}

#[test]
fn io_reads_only_two_small_counter_files_and_pid_identity_is_checked() {
    let root = std::env::temp_dir().join(format!("luyaz-activity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("10")).unwrap();
    std::fs::write(
        root.join("stat"),
        "cpu 1 2 3 4 5 6 7 8\nprocesses 9\nctxt 10",
    )
    .unwrap();
    std::fs::write(root.join("10/stat"), stat(10, "core", 23, "7", 9)).unwrap();
    let r = read(&root, 10);
    assert_eq!(r.process.unwrap().cpu, 23);
    assert_eq!(r.system.tasks, Some(9));
    assert!(r.hz.is_some_and(|hz| hz > 0));
    assert!(read(&root, 11).process.is_none());
    std::fs::write(root.join("10/stat"), stat(11, "wrong", 23, "7", 9)).unwrap();
    assert!(read(&root, 10).process.is_none());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn fold_emits_in_existing_hour_proc_and_restarts_hour_coverage() {
    use crate::longterm::{Clock, Fold, Out, power};
    let mut fold = Fold::new("fixture");
    let mut out = Out::default();
    let clock = |up| Clock {
        boot: "abc12345-full-boot".into(),
        boottime: up,
        mono: up,
        wall: 1000 + up,
        ..Default::default()
    };
    for (up, n) in [(3480, 1), (3540, 2), (3600, 3), (3660, 4)] {
        let c = clock(up);
        fold.tick(&c, &mut out);
        fold.power_with(
            &c,
            &power::Read {
                activity: Some(reading(n)),
                ..Default::default()
            },
        );
    }
    assert_eq!(
        out.hours.len(),
        3,
        "only the existing hour/hour_power/hour_proc lines"
    );
    let row = out.hours.iter().find(|r| r["k"] == "hour_proc").unwrap();
    assert_eq!(row["activity"]["core_cpu"]["observed_s"], 60);
    assert_eq!(row["activity"]["task_creation"]["count"], 120);
    let saved = fold.save();
    let closed = Fold::close_old(&saved, "next-boot");
    let row = closed.iter().find(|r| r["k"] == "hour_proc").unwrap();
    assert_eq!(row["activity"]["core_cpu"]["observed_s"], 60);
    assert!(
        serde_json::to_string(&row["activity"]).unwrap().len() < 2048,
        "bounded hourly diagnostic addition, not a per-minute record"
    );
}

#[test]
fn fold_resumes_legacy_state_without_inventing_activity_coverage() {
    use crate::longterm::{Clock, Fold, Out, power};
    let mut fold = Fold::new("fixture");
    let c = Clock {
        boot: "abc12345".into(),
        boottime: 60,
        mono: 60,
        ..Default::default()
    };
    fold.tick(&c, &mut Out::default());
    fold.power_with(&c, &power::Read::default());
    let mut old: Value = serde_json::from_str(&fold.save()).unwrap();
    old["hour"]["pw"]
        .as_object_mut()
        .unwrap()
        .remove("activity");
    let mut restored = Fold::resume(&old.to_string(), "fixture", &c.boot).unwrap();
    let c = Clock {
        boottime: 120,
        mono: 120,
        ..c
    };
    restored.power_with(
        &c,
        &power::Read {
            activity: Some(reading(2)),
            ..Default::default()
        },
    );
    let rows = Fold::close_old(&restored.save(), "next-boot");
    assert!(
        rows.iter().find(|r| r["k"] == "hour_proc").unwrap()["activity"]["core_cpu"]["ticks"]
            .is_null()
    );
}
