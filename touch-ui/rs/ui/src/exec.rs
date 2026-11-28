//! Replace this process with a fresh copy of itself: a theme or language switch.
//! Same pid, so the screen's owner (the sentinel, which checks /proc/<pid>/comm starts with
//! luyaz-ui) sees no restart. Exec'd by the binary's real path, not /proc/self/exe: the
//! kernel names the new image after the path's basename, and "exe" would fail the check and get
//! a second UI started next to this one (two UIs on /dev/dri/card0 reboot the device,
//! 2026-09-17).
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use crate::logic::{self, Launch};
use std::os::unix::process::CommandExt;

/// Every fd above stderr close-on-exec but `keep` (the DRM card handed over): the new process
/// opens touch, key and sockets itself.
fn cloexec_all(keep: Option<i32>) {
    let fds: Vec<i32> = match std::fs::read_dir("/proc/self/fd") {
        Ok(d) => d.filter_map(Result::ok).filter_map(|e| e.file_name().to_str().and_then(|n| n.parse().ok())).collect(),
        Err(_) => (3..1024).collect(),
    };
    for fd in fds.into_iter().filter(|&fd| fd > 2 && Some(fd) != keep) {
        // SAFETY: F_GETFD/F_SETFD on an fd number; a closed one just fails
        unsafe {
            let fl = libc::fcntl(fd, libc::F_GETFD);
            if fl >= 0 {
                libc::fcntl(fd, libc::F_SETFD, fl | libc::FD_CLOEXEC);
            }
        }
    }
}

/// Exec the running binary with argv[0] kept and `l` as its arguments. `l.drm`: that fd stays
/// open across the exec (FD_CLOEXEC cleared: std opens everything close-on-exec), set again if
/// the exec fails. Returns only on failure (-1, the reason logged, this process unchanged): the
/// binary was replaced on disk since it started (" (deleted)"), or readlink / exec failed.
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
    let mut l = *l;
    if let Some(h) = l.drm
        && !plat::drm::set_cloexec(h.fd, false)
    {
        eprintln!("ui: exec: DRM fd {} cannot stay open; the new image opens the card itself", h.fd);
        l.drm = None;
    }
    let args = logic::launch_argv(&l);
    eprintln!("ui: exec {ps} {}", args.join(" "));
    cloexec_all(l.drm.map(|h| h.fd));
    let e = std::process::Command::new(&path).arg0(if argv0.is_empty() { &ps } else { argv0 }).args(&args).exec();
    eprintln!("ui: exec failed: {e}");
    if let Some(h) = l.drm {
        plat::drm::set_cloexec(h.fd, true);
    }
    -1
}

/// FD_CLOEXEC set on `fd` (false: closed, or not set).
fn is_cloexec(fd: i32) -> bool {
    // SAFETY: an fd probe
    let fl = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    fl >= 0 && fl & libc::FD_CLOEXEC != 0
}

/// A close-on-exec fd standing in for the DRM card (no card here), passed as `--drm=`.
fn stand_in() -> (std::fs::File, plat::drm::Handoff) {
    use std::os::fd::AsRawFd;
    let f = std::fs::File::open("/dev/null").expect("/dev/null");
    plat::drm::set_cloexec(f.as_raw_fd(), true);
    let h = plat::drm::Handoff { fd: f.as_raw_fd(), handle: 1, fb: 40, pitch: 640, size: 307200 };
    (f, h)
}

/// LUYAZ_UI_EXEC_SELFTEST=1 luyaz-ui (a self-test mode of the real binary;
/// the variable survives the exec): the first image opens a pipe and execs itself with launch
/// arguments through exec_self, one close-on-exec fd handed over as the DRM card; the second
/// checks it kept the pid and comm, kept that fd under its number and lost every other fd above
/// stderr, got the arguments, and can make the kept fd close-on-exec again. =delete: the binary
/// removes itself first, the exec must be refused and the handed fd stay close-on-exec. =noexec:
/// the binary loses its x bits, so execve itself fails (EACCES, root too) after the handed fd was
/// made to survive it: that fd must be close-on-exec again. Exit 0 when all held.
pub fn selftest(argv: &[String]) -> i32 {
    if std::env::var("LUYAZ_UI_EXEC_SELFTEST").is_ok_and(|v| v == "noexec") {
        use std::os::unix::fs::PermissionsExt;
        let Ok(p) = std::fs::read_link("/proc/self/exe") else {
            println!("  FAIL readlink /proc/self/exe");
            return 1;
        };
        let mode = std::fs::metadata(&p).map(|m| m.permissions().mode()).unwrap_or(0o755);
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode & !0o111));
        let (f, h) = stand_in();
        let l = Launch { tab: 1, drm: Some(h), ..Default::default() };
        let failed = exec_self(&argv[0], &l) == -1;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode));
        println!("  {} execve failed", if failed { "ok  " } else { "FAIL" });
        let kept = is_cloexec(h.fd);
        println!("  {} the handed fd close-on-exec again after the failed execve", if kept { "ok  " } else { "FAIL" });
        drop(f);
        return i32::from(!(failed && kept));
    }
    if std::env::var("LUYAZ_UI_EXEC_SELFTEST").is_ok_and(|v| v == "delete") {
        if let Ok(p) = std::fs::read_link("/proc/self/exe") {
            let _ = std::fs::remove_file(p);
        }
        let (f, h) = stand_in();
        let l = Launch { tab: 1, drm: Some(h), ..Default::default() };
        let ok = exec_self(&argv[0], &l) == -1;
        println!("  {} replaced binary: exec refused", if ok { "ok  " } else { "FAIL" });
        let kept = is_cloexec(h.fd);
        println!("  {} the handed fd still close-on-exec", if kept { "ok  " } else { "FAIL" });
        drop(f);
        return i32::from(!(ok && kept));
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
        let (_f, h) = stand_in();
        let l = Launch { tab: 2, scroll_y: 340, screen_off: 1, autooff_ms: 30000, bright: 200, hist: logic::ExecHist { first_s: 100, last_s: 100, count: 1 }, drm: Some(h) };
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
    let kept = l.drm.map_or(-1, |h| h.fd);
    // SAFETY: F_GETFD probes
    let open = |fd: i32| unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0;
    let leaked = (3..64).filter(|&fd| fd != kept && open(fd)).count();
    check("the handed DRM fd kept under its number", kept > 2 && open(kept) && !is_cloexec(kept));
    check("no other fd above stderr survived", leaked == 0);
    check("the kept fd made close-on-exec again", plat::drm::set_cloexec(kept, true) && is_cloexec(kept));
    check(
        "launch args delivered",
        l.tab == 2 && l.scroll_y == 340 && l.screen_off == 1 && l.autooff_ms == 30000 && l.bright == 200 && l.hist.count == 1 && l.hist.first_s == 100,
    );
    check("boot clock ticks", crate::data::util::boot_s() > 0);
    println!("exec selftest: failed {fail}");
    i32::from(fail != 0)
}
