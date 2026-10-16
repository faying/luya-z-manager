//! The agent's doctor-runner tests, moved with it (9d), and the runner's own.

use super::*;
use crate::health::counts;
use std::fs;
use std::sync::Arc;

fn script(name: &str, body: &str) -> String {
    let p = std::env::temp_dir().join(format!("u60-doctor-{name}-{}.sh", std::process::id()));
    fs::write(&p, body).unwrap();
    p.to_string_lossy().into_owned()
}

#[test]
fn old_doctor_falls_back_to_tsv_in_chinese() {
    // What doctor.sh did before --tsv2: only `--tsv` is special, any other argument gets the
    // human-readable report (R2).
    let d = script(
        "old",
        "if [ \"$1\" = \"--tsv\" ]; then printf 'ok\\tfota\\tZTE 自动升级\\t已关闭\\nwarn\\tsms\\t告警短信\\t没设号码\\n'; \
         else echo '== U60 体检 =='; echo '  [正常] ZTE 自动升级：已关闭'; fi\n",
    );
    let c = run_checks(&d, DOCTOR_TIMEOUT).unwrap();
    assert_eq!(
        c.len(),
        2,
        "health page must not go empty with an old doctor"
    );
    assert_eq!(c[1].label, "告警短信");
    assert!(
        c.iter()
            .all(|x| x.label_en.is_empty() && x.detail_en.is_empty())
    );
    assert_eq!(counts(&c), (0, 1));
    let _ = fs::remove_file(&d);
}

#[test]
fn new_doctor_is_read_once_with_english() {
    let d = script(
        "new",
        "if [ \"$1\" = \"--tsv2\" ]; then printf '#tsv2\\nok\\tfota\\tZTE 自动升级\\t已关闭\\tZTE auto-update\\tOff\\n'; \
         else printf 'bad\\twrong\\tread --tsv\\tx\\n'; fi\n",
    );
    let c = run_checks(&d, DOCTOR_TIMEOUT).unwrap();
    assert_eq!(c.len(), 1);
    assert_eq!(
        (c[0].id.as_str(), c[0].label_en.as_str()),
        ("fota", "ZTE auto-update")
    );
    let _ = fs::remove_file(&d);
}

/// Well past a 64 KB pipe buffer, then exit at once: the old code only read stdout after the
/// exit, so this doctor sat blocked until the timeout and the run failed.
#[test]
fn a_long_report_does_not_stall_the_doctor() {
    let d = script(
        "flood",
        "i=0; while [ $i -lt 4000 ]; do printf 'ok\\tid%s\\t%080d\\tx\\n' $i 0; i=$((i+1)); done\n",
    );
    let t0 = Instant::now();
    let out = run_doctor_within(&d, "--tsv", Duration::from_secs(20)).unwrap();
    assert!(out.len() > 300_000, "{} bytes", out.len());
    assert_eq!(parse(&out).len(), 4000);
    assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
    let _ = fs::remove_file(&d);
}

/// A doctor stuck on a child of its own: the timeout takes the child too.
#[test]
fn a_timeout_kills_the_doctors_children() {
    let pidfile = std::env::temp_dir().join(format!("u60-doctor-child-{}.pid", std::process::id()));
    let _ = fs::remove_file(&pidfile);
    let d = script(
        "hang",
        &format!("sleep 30 & echo $! > {}\nwait\n", pidfile.display()),
    );
    let t0 = Instant::now();
    assert_eq!(
        run_doctor_within(&d, "--tsv2", Duration::from_millis(500)).unwrap_err(),
        "doctor timed out"
    );
    assert!(t0.elapsed() < Duration::from_secs(5), "{:?}", t0.elapsed());
    let pid = fs::read_to_string(&pidfile).unwrap().trim().to_string();
    // Gone, or a zombie nobody reaps (a container's PID 1 may not): either way no longer running.
    let dead = || match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => true,
        Ok(st) => st
            .rsplit(')')
            .next()
            .map(|r| r.trim_start().starts_with('Z'))
            .unwrap_or(false),
    };
    let t1 = Instant::now();
    while !dead() && t1.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(dead(), "sleep {pid} outlived the doctor");
    let _ = fs::remove_file(&d);
    let _ = fs::remove_file(&pidfile);
}

#[test]
fn missing_doctor_is_an_error_not_an_empty_list() {
    assert!(run_checks("/nonexistent/doctor.sh", DOCTOR_TIMEOUT).is_err());
}

#[test]
fn the_runner_keeps_writes_and_refreshes_once_at_a_time() {
    let dir = std::env::temp_dir().join(format!("u60-runner-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let count = dir.join("runs");
    // every run appends a line to `runs`; the check says how many runs there were
    let d = script(
        "count",
        &format!(
            "echo x >> {c}; sleep 0.3; printf '#tsv2\\nwarn\\truns\\t次数\\t%s\\tRuns\\t%s\\n' \
             $(wc -l < {c}) $(wc -l < {c})\n",
            c = count.display()
        ),
    );
    let file = dir.join("health.json");
    let r = Arc::new(Runner::new(&d, &file, DOCTOR_TIMEOUT));
    assert!(!r.snapshot().ran);
    r.refresh();
    let s = r.snapshot();
    assert!(s.ran && s.error.is_none());
    assert_eq!(s.checks[0].detail.trim(), "1");
    assert_eq!(Snapshot::read(&file), s, "the file says what memory says");
    // two asks while one runs: one run
    assert!(r.refresh_soon());
    assert!(!r.refresh_soon());
    let t0 = Instant::now();
    while fs::read_to_string(&count).unwrap().lines().count() < 2
        || r.snapshot().checks[0].detail.trim() != "2"
    {
        assert!(t0.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(fs::read_to_string(&count).unwrap().lines().count(), 2);
    // the doctor gone: the old checks stay, with the reason
    fs::remove_file(&d).unwrap();
    r.refresh();
    let s = r.snapshot();
    assert_eq!(s.checks[0].detail.trim(), "2");
    assert!(s.error.as_deref().unwrap().contains("not installed"));
    let _ = fs::remove_dir_all(&dir);
}

fn exe(name: &str, body: &str) -> String {
    let p = script(name, &format!("#!/bin/sh\n{body}"));
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    p
}

#[test]
fn the_core_answers_first_and_the_script_only_without_an_answer() {
    let sh = script(
        "x11-sh",
        "printf '#tsv2\\nok\\tfrom-script\\tx\\ty\\tX\\tY\\n'\n",
    );
    let core = exe(
        "x11-core",
        "[ \"$1 $2\" = \"doctor --tsv2\" ] && printf '#tsv2\\nok\\tfrom-core\\tx\\ty\\tX\\tY\\n'\n",
    );
    let id = |c: Vec<Check>| c[0].id.clone();
    assert_eq!(
        id(run_checks_core(&core, &sh, DOCTOR_TIMEOUT).unwrap()),
        "from-core"
    );
    // a pre-X11 core (no subcommand, nothing on stdout) or exit 64: the script
    let old = exe(
        "x11-old",
        "echo 'error: unexpected argument' >&2; exit 64\n",
    );
    assert_eq!(
        id(run_checks_core(&old, &sh, DOCTOR_TIMEOUT).unwrap()),
        "from-script"
    );
    assert_eq!(
        id(run_checks_core("/nonexistent/core", &sh, DOCTOR_TIMEOUT).unwrap()),
        "from-script"
    );
    // a core that hangs: an error, not a second 20 s run
    let slow = exe("x11-slow", "sleep 5\n");
    assert_eq!(
        run_checks_core(&slow, &sh, Duration::from_millis(300)).unwrap_err(),
        "doctor timed out"
    );
    for f in [sh, core, old, slow] {
        let _ = fs::remove_file(f);
    }
}
