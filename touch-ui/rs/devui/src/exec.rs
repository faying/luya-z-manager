//! Replace this process with a fresh copy of itself (src/ui_exec.c): a theme or language switch.
//! Same pid, so the screen's owner (the sentinel, which checks /proc/<pid>/comm starts with
//! u60pro-devui) sees no restart. Exec'd by the binary's real path, not /proc/self/exe: the
//! kernel names the new image after the path's basename, and "exe" would fail the check and get
//! a second UI started next to this one (two UIs on /dev/dri/card0 reboot the device,
//! 2026-09-17).
// SPDX-License-Identifier: MIT

use crate::logic::{self, Launch};
use std::os::unix::process::CommandExt;

/// Every fd above stderr close-on-exec: the new process opens DRM, touch, key and sockets
/// itself.
fn cloexec_all() {
    let fds: Vec<i32> = match std::fs::read_dir("/proc/self/fd") {
        Ok(d) => d.filter_map(Result::ok).filter_map(|e| e.file_name().to_str().and_then(|n| n.parse().ok())).collect(),
        Err(_) => (3..1024).collect(),
    };
    for fd in fds.into_iter().filter(|&fd| fd > 2) {
        // SAFETY: F_GETFD/F_SETFD on an fd number; a closed one just fails
        unsafe {
            let fl = libc::fcntl(fd, libc::F_GETFD);
            if fl >= 0 {
                libc::fcntl(fd, libc::F_SETFD, fl | libc::FD_CLOEXEC);
            }
        }
    }
}

/// Exec the running binary with argv[0] kept and `l` as its arguments. Returns only on failure
/// (-1, the reason logged, this process unchanged): the binary was replaced on disk since it
/// started (" (deleted)"), or readlink / exec failed.
pub fn exec_self(argv0: &str, l: &Launch) -> i32 {
    let path = match std::fs::read_link("/proc/self/exe") {
        Ok(p) => p,
        Err(e) => {
            eprintln!("ui: exec skipped: readlink /proc/self/exe: {e}");
            return -1;
        }
    };
    let ps = path.to_string_lossy().into_owned();
    if ps.ends_with(" (deleted)") {
        // whatever sits at that path now has not been side-by-side tested
        eprintln!("ui: exec skipped: running binary was replaced on disk ({ps})");
        return -1;
    }
    let args = logic::launch_argv(l);
    eprintln!("ui: exec {ps} {}", args.join(" "));
    cloexec_all();
    let e = std::process::Command::new(&path).arg0(if argv0.is_empty() { &ps } else { argv0 }).args(&args).exec();
    eprintln!("ui: exec failed: {e}");
    -1
}

/// LUYAZ_UI_EXEC_SELFTEST=1 u60pro-devui (tests/ui_exec_test.c as a mode of the real binary;
/// the variable survives the exec): the first image opens a pipe and execs itself with launch
/// arguments through exec_self; the second checks it kept the pid and comm, lost every fd above
/// stderr and got the arguments. =delete: the binary removes itself first and the exec must be
/// refused. Exit 0 when all held.
pub fn selftest(argv: &[String]) -> i32 {
    if std::env::var("LUYAZ_UI_EXEC_SELFTEST").is_ok_and(|v| v == "delete") {
        if let Ok(p) = std::fs::read_link("/proc/self/exe") {
            let _ = std::fs::remove_file(p);
        }
        let l = Launch { tab: 1, ..Default::default() };
        let ok = exec_self(&argv[0], &l) == -1;
        println!("  {} replaced binary: exec refused", if ok { "ok  " } else { "FAIL" });
        return i32::from(!ok);
    }
    let l = logic::launch_parse(argv);
    let comm = std::fs::read_to_string("/proc/self/comm").unwrap_or_default().trim().to_string();
    if l.tab < 0 {
        // SAFETY: setenv before any thread (the test runs alone)
        unsafe {
            std::env::set_var("T_PID", std::process::id().to_string());
            std::env::set_var("T_COMM", &comm);
        }
        let mut fds = [0i32; 2];
        // SAFETY: a pipe for the check that fds do not survive
        unsafe { libc::pipe(fds.as_mut_ptr()) };
        let l = Launch { tab: 2, scroll_y: 340, screen_off: 1, autooff_ms: 30000, bright: 200, hist: logic::ExecHist { first_s: 100, last_s: 100, count: 1 } };
        exec_self(&argv[0], &l);
        println!("  FAIL exec returned");
        return 1;
    }
    let mut fail = 0;
    let mut check = |what: &str, ok: bool| {
        println!("  {} {what}", if ok { "ok  " } else { "FAIL" });
        fail += !ok as i32;
    };
    check("same pid", std::env::var("T_PID").ok() == Some(std::process::id().to_string()));
    check("comm unchanged (not \"exe\")", std::env::var("T_COMM").ok() == Some(comm.clone()));
    check("comm keeps the luyaz-ui prefix", !std::env::var("T_COMM").unwrap_or_default().starts_with("luyaz-ui") || comm.starts_with("luyaz-ui"));
    // SAFETY: F_GETFD probes
    let leaked = (3..64).filter(|&fd| unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0).count();
    check("no fd above stderr survived", leaked == 0);
    check(
        "launch args delivered",
        l.tab == 2 && l.scroll_y == 340 && l.screen_off == 1 && l.autooff_ms == 30000 && l.bright == 200 && l.hist.count == 1 && l.hist.first_s == 100,
    );
    check("boot clock ticks", crate::data::util::boot_s() > 0);
    println!("exec selftest: failed {fail}");
    i32::from(fail != 0)
}
