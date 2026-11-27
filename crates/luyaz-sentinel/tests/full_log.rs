//! A log line that cannot be written (the sentinel's stderr is /tmp/luyaz-sentinel.out on the
//! device) is dropped, never a reason to stop. std's `eprintln!` panics when the write fails, and
//! the sentinel aborts on any panic (its hook, and the device build): a full /tmp would leave the
//! screen and the core's restarts with no one (core P-7, 10-26).
#![cfg(target_os = "linux")]

/// The program: a line on stderr (here the usage) with stderr unwritable, then its own exit.
#[test]
fn the_program_says_its_line_into_a_full_log_and_carries_on() {
    let full = std::fs::OpenOptions::new().write(true).open("/dev/full").unwrap();
    let st = std::process::Command::new(env!("CARGO_BIN_EXE_luyaz-sentinel"))
        .arg("--no-such-flag")
        .stdout(std::process::Stdio::null())
        .stderr(full)
        .status()
        .unwrap();
    assert_eq!(st.code(), Some(2), "{st}");
}

/// The library: the screen log line goes to stderr too (`uid_log`), and must not panic when that
/// write fails. The test reruns itself with `--nocapture` (the harness otherwise catches what
/// `eprintln!` prints before it reaches fd 2) and stderr on /dev/full.
#[test]
fn a_screen_log_line_into_a_full_stderr_does_not_panic() {
    const CHILD: &str = "LUYAZ_FULL_LOG_CHILD";
    if std::env::var_os(CHILD).is_some() {
        luyaz_sentinel::sys::uid_log("full-log test line");
        return;
    }
    let full = std::fs::OpenOptions::new().write(true).open("/dev/full").unwrap();
    let st = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "a_screen_log_line_into_a_full_stderr_does_not_panic", "--nocapture"])
        .env(CHILD, "1")
        .stdout(std::process::Stdio::null())
        .stderr(full)
        .status()
        .unwrap();
    assert!(st.success(), "uid_log with stderr unwritable: {st}");
}
