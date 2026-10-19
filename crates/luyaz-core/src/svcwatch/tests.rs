use super::*;
use serde_json::json;

struct Tree(PathBuf);
impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn tree() -> Tree {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("u60d-svcwatch-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    Tree(d)
}
fn files(t: &Tree) -> Files {
    std::fs::create_dir_all(t.0.join("proc")).unwrap();
    std::fs::write(t.0.join("proc/uptime"), "1234.56 99.0\n").unwrap();
    Files {
        crash: t.0.join("crashlog"),
        alerts: t.0.join("alerts"),
        root: t.0.to_str().unwrap().into(),
    }
}
fn queue(f: &Files) -> Vec<(String, String)> {
    luyaz_api::alerts::Alerts::at(&f.alerts)
        .events()
        .into_iter()
        .map(|e| (e.kind, e.text))
        .collect()
}

// procd's `service list '{"name":…}'` replies, one per shape
fn running(pid: u32) -> Value {
    json!({"luyaz-web": {"instances": {"instance1": {"running": true, "pid": pid, "command": ["/bin/sh", "-c", "exec"], "term_timeout": 10}}}})
}

#[test]
fn procd_shapes() {
    assert_eq!(seen(&running(41), "luyaz-web"), Seen::Running(41));
    assert_eq!(seen(&json!({}), "luyaz-web"), Seen::Gone);
    assert_eq!(
        seen(&json!({"luyaz-web": {"instances": {}}}), "luyaz-web"),
        Seen::Gone
    );
    let pending =
        json!({"luyaz-web": {"instances": {"instance1": {"running": false, "command": []}}}});
    assert_eq!(seen(&pending, "luyaz-web"), Seen::Down(None));
    let with_code =
        json!({"luyaz-web": {"instances": {"instance1": {"running": false, "exit_code": 139}}}});
    assert_eq!(seen(&with_code, "luyaz-web"), Seen::Down(Some(139)));
    // another service's entry is not this one
    assert_eq!(seen(&running(41), "luyaz-sentinel"), Seen::Gone);
}

#[test]
fn crash_or_requested_stop() {
    // respawn pending: a crash
    assert_eq!(judge(41, &Seen::Down(None)), Verdict::Crash(None));
    assert_eq!(judge(41, &Seen::Down(Some(134))), Verdict::Crash(Some(134)));
    // `/etc/init.d/x stop`: the service is deleted — not a crash
    assert_eq!(judge(41, &Seen::Gone), Verdict::Stopped);
    // restart through the init: running again under a new pid within the reap wait
    assert_eq!(judge(41, &Seen::Running(42)), Verdict::Stopped);
    // not reaped yet
    assert_eq!(judge(41, &Seen::Running(41)), Verdict::NotYet);
}

#[test]
fn status_words() {
    assert_eq!(describe(None), "exited, status unknown");
    assert_eq!(describe(Some(3)), "exit 3");
    assert_eq!(describe(Some(139)), "killed by SIGSEGV");
    assert_eq!(describe(Some(134)), "killed by SIGABRT");
    assert_eq!(describe(Some(137)), "killed by SIGKILL");
    assert_eq!(describe(Some(129)), "killed by signal 1");
}

#[test]
fn record_header_tail_and_alert() {
    let t = tree();
    let f = files(&t);
    let log: String = (1..=300).map(|i| format!("line {i}\n")).collect();
    f.crash(
        "luyaz-web",
        "web-crash",
        "killed by SIGSEGV",
        "/tmp/luyaz-web.log",
        &tail(&log, TAIL_LINES),
    );
    let d = f.crash.join("luyaz-web");
    let names: Vec<_> = std::fs::read_dir(&d)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().into_string().unwrap())
        .collect();
    assert_eq!(names.len(), 1);
    assert!(names[0].ends_with("-up1234.log"), "{names:?}");
    let body = std::fs::read_to_string(d.join(&names[0])).unwrap();
    assert!(body.starts_with("program: luyaz-web\nstatus:  killed by SIGSEGV\ntime:    "));
    assert!(body.contains("--- last 200 lines of /tmp/luyaz-web.log ---\nline 101\n"));
    assert!(body.contains("line 300\n") && !body.contains("line 100\n"));
    // health.rs reads the status line
    let list = luyaz_api::health::crash_logs(&f.crash);
    assert_eq!(list[0]["status"], "killed by SIGSEGV");
    assert_eq!(
        queue(&f),
        vec![("web-crash".into(), "luyaz-web killed by SIGSEGV".into())]
    );
}

#[test]
fn five_per_program_and_one_mb_in_all() {
    let t = tree();
    let f = files(&t);
    for i in 0..7 {
        record(
            &f.crash,
            "luyaz-sentinel",
            &format!("exit {i}"),
            "l",
            "x",
            1_760_000_000,
            10,
        )
        .unwrap();
    }
    assert_eq!(
        std::fs::read_dir(f.crash.join("luyaz-sentinel"))
            .unwrap()
            .count(),
        CRASH_KEEP
    );
    // the newest is kept
    let all: String = std::fs::read_dir(f.crash.join("luyaz-sentinel"))
        .unwrap()
        .flatten()
        .map(|e| std::fs::read_to_string(e.path()).unwrap())
        .collect();
    assert!(all.contains("exit 6"));
    // 1 MB across programs: an old big one goes first
    let big = "y".repeat(700_000);
    record(&f.crash, "zwrt-datad", "old", "l", &big, 1_760_000_000, 10).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    let newest = record(&f.crash, "luyaz-web", "new", "l", &big, 1_760_000_100, 11).unwrap();
    assert!(newest.exists());
    assert_eq!(
        std::fs::read_dir(f.crash.join("zwrt-datad"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn core_marker_from_this_boot_is_a_crash() {
    let t = tree();
    let f = files(&t);
    let m = t.0.join("data/luyaz/core/core.running");
    // first start ever: no record, a marker
    f.core_started(&m, "boot-a", 100, "old output\n");
    assert!(!f.crash.exists());
    assert_eq!(std::fs::read_to_string(&m).unwrap(), "boot-a 100\n");
    // a clean stop removed it: the next start is quiet
    std::fs::remove_file(&m).unwrap();
    f.core_started(&m, "boot-a", 101, "x\n");
    assert!(!f.crash.exists());
    // killed (marker left): the next start keeps the log as it was before it
    let before: String = (1..=250).map(|i| format!("old {i}\n")).collect();
    f.core_started(&m, "boot-a", 102, &before);
    let d = f.crash.join("luyaz-core");
    let body = std::fs::read_to_string(
        std::fs::read_dir(&d)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path(),
    )
    .unwrap();
    assert!(body.contains("status:  exited, status unknown"));
    assert!(body.contains("old 51\n") && body.contains("old 250") && !body.contains("old 50\n"));
    assert_eq!(
        queue(&f),
        vec![(
            "datad-crash".into(),
            "luyaz-core exited, status unknown".into()
        )]
    );
    assert_eq!(std::fs::read_to_string(&m).unwrap(), "boot-a 102\n");
}

#[test]
fn another_boots_marker_is_a_reboot_not_a_crash() {
    let t = tree();
    let f = files(&t);
    let m = t.0.join("core.running");
    std::fs::write(&m, "boot-a 100\n").unwrap();
    f.core_started(&m, "boot-b", 7, "");
    assert!(!f.crash.exists() && queue(&f).is_empty());
    assert_eq!(std::fs::read_to_string(&m).unwrap(), "boot-b 7\n");
    // unreadable boot id, garbage, or our own pid: never a crash
    assert!(!Marker::crashed(Marker::parse("boot-b 7").as_ref(), "", 8));
    assert!(Marker::parse("junk").is_none());
    assert!(!Marker::crashed(
        Marker::parse("boot-b 7").as_ref(),
        "boot-b",
        7
    ));
}

#[test]
fn only_the_installed_core_keeps_a_marker() {
    for y in [
        "zwrt-datad",
        "/data/plugins/zwrt-datad/zwrt-datad",
        "/data/luyaz/luyaz-core",
    ] {
        assert!(marks(y), "{y}");
    }
    for n in ["zwrt-datad.test", "/data/luyaz/luyaz-core.test", "u60d", ""] {
        assert!(!marks(n), "{n}");
    }
}

#[test]
fn log_before_start() {
    let t = tree();
    let p = t.0.join("log");
    assert_eq!(log_len_at_start(&p), "");
    std::fs::write(&p, "a\nb\n").unwrap();
    assert_eq!(log_len_at_start(&p), "a\nb\n");
}

#[tokio::test]
async fn exit_seen_through_the_pidfd() {
    let mut c = std::process::Command::new("sleep")
        .arg("0.3")
        .spawn()
        .unwrap();
    let pid = c.id();
    let t0 = std::time::Instant::now();
    tokio::time::timeout(Duration::from_secs(10), exited(pid))
        .await
        .unwrap();
    assert!(t0.elapsed() >= Duration::from_millis(200));
    let _ = c.wait();
}

#[test]
fn every_watched_kind_has_labels() {
    for (_, kind, log) in WATCHED {
        assert!(
            !luyaz_api::alerts::kind_label(kind).starts_with("其他告警"),
            "{kind}"
        );
        assert!(
            !luyaz_api::alerts::kind_label_en(kind).starts_with("Other alert"),
            "{kind}"
        );
        assert!(log.starts_with("/tmp/") && log.ends_with(".log"));
    }
}

#[test]
fn the_inits_start_line_is_this_runs() {
    let l = "old 1\nold 2\n=== 2026-10-14T10:00:00 start /data/luyaz/luyaz-core\n";
    assert_eq!(before_this_start(l), "old 1\nold 2");
    assert_eq!(before_this_start("=== 2026-10-14T10:00:00 start x\n"), "");
    assert_eq!(before_this_start("old 1\nold 2\n"), "old 1\nold 2\n");
}

#[test]
fn the_sentinels_record_carries_its_stderr_tail() {
    assert_eq!(stderr_log("luyaz-sentinel"), Some(SENTINEL_OUT));
    assert_eq!(stderr_log("luyaz-web"), None);
    let out: String = (0..80).map(|i| format!("e{i}\n")).collect();
    let b = evidence("a\nb\n", Some((SENTINEL_OUT, &out)));
    assert!(
        b.starts_with("a\nb\n--- last 50 lines of /tmp/luyaz-sentinel.out ---\ne30\n"),
        "{b}"
    );
    assert!(b.ends_with("e79"));
    // an empty stderr log adds nothing
    assert_eq!(evidence("a\n", Some((SENTINEL_OUT, "\n"))), "a");
    assert_eq!(evidence("a\n", None), "a");
}
