//! The two threads (O3) and shadow mode (§9).
//!
//! The screen thread is event driven: it sleeps in poll(2) on a signalfd (TERM/INT), the control FIFO,
//! a pidfd of the UI on screen (ours or adopted, O4) and — only while our UI is not on screen — the
//! touch device; the timeout is [`Screen::next_wake_ms`] (at most 60 s, the heartbeat). It never waits
//! for a lock. The worker thread runs [`Worker::round`] every 60 s and does everything that may block.
//!
//! Shadow mode (`--shadow`) runs the same decisions against the live device next to u60-uid and
//! u60-guard and changes nothing: [`ShadowWorker`] wraps the real device and records every change
//! instead of making it, and the screen thread records its acts instead of carrying them out. It
//! never opens the control FIFO (reading it would steal u60-uid's commands, and creating it would
//! unlink u60-uid's), card0 or the touch device, never writes a state file, takes no lock. Everything
//! goes to `/tmp/luyaz-sentinel.shadow` (capped at 512 KB, one `.old`).

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::{AsRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use crate::screen::{self, Act, Input, Persist, Screen};
use crate::sys;
use crate::worker::{Health, Paths, Worker, WorkerIo};

/// The touch UI's old home (Phase 10 R5): start.sh and the `*.orig-backup` stay here for good.
pub const DEVUI_DIR: &str = "/data/plugins/u60pro-devui";
/// Its home since R5: the only program the sentinel starts (round ③).
pub const UI_DIR: &str = "/data/luyaz/ui";
pub const UI_BIN: &str = "/data/luyaz/ui/luyaz-ui";
pub const DEVUI_LOG: &str = "/tmp/luyaz-ui.log";
pub const START_SH: &str = "/data/plugins/u60pro-devui/start.sh";
/// The screen's state (Phase 10 R4). The copy from the old `/data/u60-uid` went with Luya-Z
/// rename E3: every device has this one (and its `.migrated`, which only older readers look at).
pub const STATE_DIR: &str = "/data/luyaz/screen";
pub const F_ATTEMPTS: &str = "/data/luyaz/screen/attempts";
pub const F_GAVE_UP: &str = "/data/luyaz/screen/gave-up";
/// On tmpfs on purpose: a reboot always comes back to our UI. Only the give-up and the attempt count
/// survive a reboot.
pub const F_WANT: &str = "/tmp/luyaz/screen.want";
pub const CRASH_DIR: &str = "/data/crashlog/u60pro-devui";
pub const DRM_DEV: &str = "/dev/dri/card0";
pub const VENDOR_INIT: &str = "/etc/init.d/zte_topsw_devui";
pub const VENDOR_COMM: &str = "zte_topsw_devui";
pub const DEVUI_COMM: &str = "u60pro-devui";
pub const UI_COMM: &str = "luyaz-ui";

/// Which program to start, and its working directory: luyaz-ui only since round ③ (the old
/// u60pro-devui is deleted from the device; a missing program is a failed launch, counted).
pub fn pick_ui(_new_exists: bool) -> (&'static str, &'static str) {
    (UI_BIN, UI_DIR)
}

pub const HEARTBEAT: &str = "/tmp/luyaz-sentinel.hb";
const STOP_GRACE_MS: i64 = 5000;

// ── shadow: the real device, read; every change recorded ───────────────────

/// Lines recorded in the last 10 s, so a decision that repeats until u60-uid/the guard acts on it is
/// written once.
#[derive(Default)]
struct Dedup(HashMap<String, i64>);

impl Dedup {
    fn fresh(&mut self, line: &str, now_s: i64) -> bool {
        self.0.retain(|_, t| now_s - *t < 10);
        if self.0.contains_key(line) {
            return false;
        }
        self.0.insert(line.to_string(), now_s);
        true
    }
}

pub struct ShadowWorker<I: WorkerIo> {
    pub real: I,
    seen: Dedup,
    /// What shadow mode "wrote", so it reads its own state back like the real run would.
    files: HashMap<String, String>,
    /// Count of changes recorded (tests: must be 0 on the real device's side).
    pub recorded: usize,
}

impl<I: WorkerIo> ShadowWorker<I> {
    pub fn new(real: I) -> Self {
        ShadowWorker {
            real,
            seen: Dedup::default(),
            files: HashMap::new(),
            recorded: 0,
        }
    }
    fn would(&mut self, what: String) {
        self.recorded += 1;
        let now = self.real.mono_s();
        if self.seen.fresh(&what, now) {
            sys::shadow_log("worker", &format!("would {what}"));
        }
    }
}

impl<I: WorkerIo> WorkerIo for ShadowWorker<I> {
    fn uptime(&mut self) -> u64 {
        self.real.uptime()
    }
    fn wall(&mut self) -> i64 {
        self.real.wall()
    }
    fn mono_s(&mut self) -> i64 {
        self.real.mono_s()
    }
    fn read(&mut self, path: &str) -> Option<String> {
        if path.starts_with(&format!("{}/", Paths::default().state)) {
            return self.files.get(path).cloned();
        }
        self.real.read(path)
    }
    fn exists(&mut self, path: &str) -> bool {
        self.real.exists(path)
    }
    fn size(&mut self, path: &str) -> Option<u64> {
        self.real.size(path)
    }
    fn cmdline_has(&mut self, pid: i32, needle: &str) -> bool {
        self.real.cmdline_has(pid, needle)
    }
    fn appenders_only(&mut self, path: &str) -> bool {
        self.real.appenders_only(path)
    }
    fn beaconing(&mut self) -> bool {
        self.real.beaconing()
    }
    fn healthz(&mut self) -> Health {
        self.real.healthz()
    }
    fn dropbear_listening(&mut self) -> bool {
        self.real.dropbear_listening()
    }
    fn write(&mut self, path: &str, content: &str) {
        if path.starts_with(&format!("{}/", Paths::default().state)) {
            self.files.insert(path.to_string(), content.to_string()); // own state: kept in memory, not recorded
            return;
        }
        self.would(format!("write {path}: {}", content.trim_end()));
    }
    fn poke(&mut self, path: &str, content: &str) -> bool {
        self.would(format!("poke {path}: {}", content.trim_end()));
        true
    }
    fn remove(&mut self, path: &str) {
        self.would(format!("remove {path}"));
    }
    fn log(&mut self, msg: &str) {
        let now = self.real.mono_s();
        if self.seen.fresh(msg, now) {
            sys::shadow_log("worker", &format!("log: {msg}"));
        }
    }
    fn alert(&mut self, kind: &str, text: &str) {
        self.would(format!("alert {kind}: {text}"));
    }
    fn restore_wifi(&mut self) -> Result<(), String> {
        // Assumed to work: whether it really did is the guard's to find out, and the next round
        // judges by what really beacons (a made-up failure would back off and alert where the
        // guard did not).
        self.would("restore Wi-Fi".into());
        Ok(())
    }
    fn set_autosleep(&mut self, on: bool) {
        self.would(format!("set auto-sleep {on}"));
    }
    fn recover_live(&mut self) -> bool {
        self.would("start luyaz-ship.sh recover-live".into());
        false
    }
    fn cap_log(&mut self, path: &str) -> bool {
        self.would(format!("cap {path}"));
        false
    }
    fn start_dropbear(&mut self) -> bool {
        self.would("start dropbear".into());
        false
    }
    fn core_being_replaced(&mut self) -> bool {
        self.real.core_being_replaced()
    }
    fn alert_sms(&mut self) {
        self.would("send alert SMS (core unhealthy)".into());
    }
    fn guard_running(&mut self) -> bool {
        self.real.guard_running()
    }
    fn boot_is_normal(&mut self) -> bool {
        self.real.boot_is_normal()
    }
}

// ── the worker thread ───────────────────────────────────────────────────────

pub fn worker_thread(shadow: bool, last_round: Arc<AtomicI64>) {
    let mut w = Worker::new(Paths::default());
    if shadow {
        let mut io = ShadowWorker::new(sys::RealWorker::default());
        loop {
            w.round(&mut io);
            last_round.store(w.last_round_mono, Ordering::Relaxed);
            std::thread::sleep(Duration::from_secs(crate::worker::INTERVAL_S));
        }
    }
    let mut io = sys::RealWorker::default();
    w.load(&mut io);
    loop {
        w.round(&mut io);
        last_round.store(w.last_round_mono, Ordering::Relaxed);
        std::thread::sleep(Duration::from_secs(crate::worker::INTERVAL_S));
    }
}

// ── the events thread (§6 C-R9) ─────────────────────────────────────────────

/// Real mode only: from here on every alert of this process is an event with an id, delivered to
/// the core (or queued here when it does not answer). A POST may wait 5 s, so neither the screen
/// thread nor the worker ever does it.
pub fn start_events() {
    let (tx, rx) = std::sync::mpsc::channel::<(String, String)>();
    sys::set_event_sender(tx);
    let _ = std::thread::Builder::new()
        .name("events".into())
        .spawn(move || {
            let mut io = sys::RealEvents;
            let mut ev = crate::events::Events::default();
            loop {
                match rx.recv_timeout(Duration::from_secs(5)) {
                    Ok((kind, text)) => {
                        ev.raise(&mut io, &kind, &text);
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                    Err(_) => {}
                }
                ev.deliver(&mut io);
            }
        });
}

// ── the screen thread ───────────────────────────────────────────────────────


/// Make [`STATE_DIR`] (0700) before anything writes it (main, before the events thread, whose
/// sequence file lives there). An existing one keeps its mode.
pub fn state_dir_at_start() {
    use std::os::unix::fs::DirBuilderExt;
    let _ = fs::DirBuilder::new().recursive(true).mode(0o700).create(STATE_DIR);
}

pub fn load_persist() -> Persist {
    let attempts = screen::parse_attempts(sys::read(F_ATTEMPTS).as_deref());
    let gave_up = std::path::Path::new(F_GAVE_UP).exists();
    let want_devui = !sys::read(F_WANT)
        .map(|w| w.starts_with("vendor"))
        .unwrap_or(false);
    Persist {
        want_devui,
        gave_up,
        attempts,
    }
}

/// A ship transaction of `touch` or the sentinel with a live executor: the empty-screen check yields
/// (its executor judges an empty screen itself, and rolls back).
pub fn ship_yield() -> bool {
    sys::txn_live(&["touch", "sentinel"])
}

fn sh(script: &str) {
    let _ = sys::run(
        Command::new("/bin/sh").args(["-c", script]),
        Duration::from_secs(30),
    );
}

fn wait_drm_free(log: &dyn Fn(&str)) {
    let t0 = sys::mono_ms();
    while sys::held_open(DRM_DEV) && sys::mono_ms() - t0 < screen::DRM_WAIT_MS {
        std::thread::sleep(Duration::from_millis(200));
    }
    if sys::held_open(DRM_DEV) {
        log(&format!(
            "{DRM_DEV} still held after {} ms; going ahead",
            screen::DRM_WAIT_MS
        ));
    }
}

fn record_crash(why: &str) {
    let _ = fs::create_dir_all(CRASH_DIR);
    let wall = sys::wall_s();
    let st = sys::stamp(wall); // YYYY-MM-DDTHH:MM:SS
    let compact: String = st.chars().filter(|c| c.is_ascii_digit()).collect(); // YYYYMMDDHHMMSS
    let name = format!(
        "{CRASH_DIR}/{}-{}-up{}.log",
        &compact[..8],
        &compact[8..],
        sys::uptime_s()
    );
    let tail: String = {
        let t = sys::read(DEVUI_LOG).unwrap_or_default();
        let lines: Vec<&str> = t.lines().collect();
        lines[lines.len().saturating_sub(200)..].join("\n")
    };
    let body = format!(
        "program: u60pro-devui\nstatus:  {why}\ntime:    {} device local\n--- last 200 lines of {DEVUI_LOG} ---\n{tail}\n",
        st.replace('T', " ")
    );
    let _ = fs::write(&name, body);
    // keep the newest 5
    if let Ok(rd) = fs::read_dir(CRASH_DIR) {
        let mut v: Vec<(std::time::SystemTime, std::path::PathBuf)> = rd
            .flatten()
            .filter(|e| e.path().extension().map(|x| x == "log").unwrap_or(false))
            .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
            .collect();
        v.sort_by_key(|e| std::cmp::Reverse(e.0));
        for (_, p) in v.into_iter().skip(5) {
            let _ = fs::remove_file(p);
        }
    }
}

fn spawn_devui() -> Result<i32, String> {
    if fs::metadata(DEVUI_LOG)
        .map(|m| m.len() > 1024 * 1024)
        .unwrap_or(false)
    {
        let _ = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(DEVUI_LOG);
    }
    let out = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o644)
        .open(DEVUI_LOG)
        .map_err(|e| e.to_string())?;
    let err = out.try_clone().map_err(|e| e.to_string())?;
    let (bin, dir) = pick_ui(std::path::Path::new(UI_BIN).exists());
    let mut cmd = Command::new(bin);
    cmd.current_dir(dir)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    // SAFETY: setsid is async-signal-safe. Its own session: stopping the sentinel must not take it
    // down. Every other fd of ours is O_CLOEXEC.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            // As under u60-uid, whose children inherited its SIG_IGN (std resets it to default).
            libc::signal(libc::SIGPIPE, libc::SIG_IGN);
            Ok(())
        });
    }
    let child = cmd.spawn().map_err(|e| e.to_string())?;
    let pid = child.id() as i32;
    std::mem::forget(child); // reaped by waitpid(pid) below, never by std
    Ok(pid)
}

/// waitpid(pid, WNOHANG): `Some(Some(status))` ended, `Some(None)` not our child any more, `None`
/// still running.
fn reap(pid: i32) -> Option<Option<i32>> {
    let mut st = 0;
    // SAFETY: plain syscall on our own child's pid.
    let r = unsafe { libc::waitpid(pid, &mut st, libc::WNOHANG) };
    if r == pid {
        Some(Some(st))
    } else if r < 0 {
        Some(None)
    } else {
        None
    }
}

/// TERM, up to 5 s, then KILL (u60-uid's stop_devui). What ended, as for [`Input::ui_exit`].
fn stop_ui(ui: &screen::Ui) -> Option<Option<i32>> {
    // SAFETY: signals to a pid we track.
    unsafe { libc::kill(ui.pid, libc::SIGTERM) };
    let gone = |ui: &screen::Ui| -> Option<Option<i32>> {
        match ui.kind {
            screen::Kind::Child => reap(ui.pid),
            screen::Kind::Adopted => (!sys::pid_is_ui(ui.pid)).then_some(None),
        }
    };
    let t0 = sys::mono_ms();
    while sys::mono_ms() - t0 < STOP_GRACE_MS {
        if let Some(e) = gone(ui) {
            return Some(e);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    unsafe { libc::kill(ui.pid, libc::SIGKILL) };
    match ui.kind {
        screen::Kind::Child => {
            let mut st = 0;
            let r = unsafe { libc::waitpid(ui.pid, &mut st, 0) };
            Some(if r == ui.pid { Some(st) } else { None })
        }
        screen::Kind::Adopted => {
            std::thread::sleep(Duration::from_millis(300));
            gone(ui)
        }
    }
}

struct Rt {
    shadow: bool,
    ctl: Vec<(fs::File, fs::File)>,
    touch: Option<sys::Touch>,
    ui_fd: Option<(i32, OwnedFd)>,
    /// UIs dropped by a hold while still running, with their pidfd (None: no pidfd), polled so
    /// their exit wakes the loop and they are reaped at once: a zombie still has /proc/<pid>, and
    /// ship's executor waits for the old UI's pid to go (10-11: it timed out on the zombie)
    orphans: Vec<(i32, Option<OwnedFd>)>,
    launches_base: u32,
    last_hb_ms: i64,
    last_hb: String,
    seen: Dedup,
    attempts_set: u32,
}

impl Rt {
    fn log(&mut self, msg: &str) {
        if self.shadow {
            if self.seen.fresh(msg, sys::mono_ms() / 1000) {
                sys::shadow_log("screen", &format!("log: {msg}"));
            }
        } else {
            sys::uid_log(msg);
        }
    }

    fn would(&mut self, a: &Act) {
        let line = format!("would {a:?}");
        if self.seen.fresh(&line, sys::mono_ms() / 1000) {
            sys::shadow_log("screen", &line);
        }
    }

    /// Carry out the acts; feedback (spawned/stopped) goes back into the screen and may add acts.
    fn exec(&mut self, s: &mut Screen, acts: Vec<Act>) {
        let mut queue: std::collections::VecDeque<Act> = acts.into();
        while let Some(a) = queue.pop_front() {
            if let Act::Log(m) = &a {
                self.log(m);
                continue;
            }
            if self.shadow {
                self.would(&a);
                continue;
            }
            match a {
                Act::Log(_) => {}
                Act::Alert(kind, text) => sys::alert(kind, &text),
                Act::SetAttempts(n) => {
                    self.attempts_set = n;
                    if let Err(e) = sys::write_synced(F_ATTEMPTS, &format!("{n}\n")) {
                        self.log(&format!("cannot write {F_ATTEMPTS}: {e}"));
                    }
                }
                Act::SetGaveUp => {
                    let _ = sys::write_synced(F_GAVE_UP, "1\n");
                }
                Act::ClearGaveUp => {
                    let _ = fs::remove_file(F_GAVE_UP);
                }
                Act::WantVendor => {
                    let _ = sys::write_atomic(F_WANT, "vendor\n"); // tmpfs: nothing to sync
                }
                Act::ClearWant => {
                    let _ = fs::remove_file(F_WANT);
                }
                Act::RecordCrash(why) => record_crash(&why),
                Act::StopVendor => {
                    if sys::find_comm(VENDOR_COMM).is_some() {
                        sh(&format!(
                            "{VENDOR_INIT} stop >/dev/null 2>&1; killall -9 {VENDOR_COMM} 2>/dev/null"
                        ));
                        std::thread::sleep(Duration::from_millis(500));
                    }
                }
                Act::Explain => {
                    wait_drm_free(&|m| sys::uid_log(m));
                    let en = sys::read(sys::DEVUI_CONF).is_some_and(|c| {
                        c.lines()
                            .filter_map(|l| l.strip_prefix("lang="))
                            .next_back()
                            .is_some_and(|v| v.trim_end_matches('\r') == "en")
                    });
                    match sys::show_frame(en) {
                        Ok(()) => self.log("explanation frame shown"),
                        Err(e) => self.log(&format!("explanation frame skipped: {e}")),
                    }
                }
                Act::StartVendor => {
                    wait_drm_free(&|m| sys::uid_log(m));
                    self.log("starting the vendor UI");
                    sh(&format!("{VENDOR_INIT} start >/dev/null 2>&1"));
                }
                Act::Spawn => {
                    let r = spawn_devui();
                    queue.extend(s.spawned(sys::mono_ms(), r, self.attempts_set));
                }
                Act::Stop(pid) => {
                    if let Some(ui) = s.cur.filter(|u| u.pid == pid) {
                        let ended = stop_ui(&ui);
                        queue.extend(s.stopped(ended));
                    }
                }
                Act::Touch(on) => {
                    if on {
                        self.touch = sys::Touch::open();
                        if self.touch.is_some() {
                            self.log("vendor UI on screen: corner long-press listener on");
                        }
                    } else {
                        self.touch = None;
                    }
                }
            }
        }
    }

    fn heartbeat(&mut self, s: &Screen, worker: i64, force: bool) {
        let now = sys::mono_ms();
        let state = s.on_screen().word();
        if !force && state == self.last_hb && now - self.last_hb_ms < screen::HEARTBEAT_MS {
            return;
        }
        let line = format!(
            "{} {} {state} {} {worker}\n",
            now / 1000,
            std::process::id(),
            self.launches_base + s.launches
        );
        if self.shadow {
            if state != self.last_hb {
                sys::shadow_log("screen", &format!("state {state}"));
            }
        } else if let Err(e) = sys::write_atomic(HEARTBEAT, &line) {
            eprintln!("luyaz-sentinel: heartbeat: {e}");
        }
        self.last_hb = state.to_string();
        self.last_hb_ms = now;
    }
}

static TERM_PIPE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

extern "C" fn on_term(_sig: libc::c_int) {
    let fd = TERM_PIPE.load(Ordering::Relaxed);
    if fd >= 0 {
        // SAFETY: write(2) is async-signal-safe; a full pipe just drops the byte.
        unsafe { libc::write(fd, b"t".as_ptr() as *const libc::c_void, 1) };
    }
}

/// TERM/INT wake the screen thread through a pipe. Not a signalfd: that needs the signals blocked in
/// every thread, and a blocked mask is inherited by every child — the UI would ignore the TERM that
/// stops it (found running real mode against stubs) and so would whatever the init scripts start.
/// Handlers are reset to default in a child at exec; the mask is left clean. Call before any thread.
pub fn install_signals() -> Option<OwnedFd> {
    let mut fds = [0; 2];
    // SAFETY: pipe2 into a 2-int array; sigaction with a zeroed struct and a plain handler.
    unsafe {
        if libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) != 0 {
            return None;
        }
        TERM_PIPE.store(fds[1], Ordering::Relaxed);
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_term as *const () as usize;
        sa.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(libc::SIGTERM, &sa, std::ptr::null_mut());
        libc::sigaction(libc::SIGINT, &sa, std::ptr::null_mut());
        Some(std::os::unix::io::FromRawFd::from_raw_fd(fds[0]))
    }
}

pub fn screen_thread(shadow: bool, worker_round: Arc<AtomicI64>, sig: Option<OwnedFd>) {
    let launches_base = if shadow {
        0
    } else {
        sys::read(HEARTBEAT)
            .and_then(|t| t.split_whitespace().nth(3).and_then(|x| x.parse().ok()))
            .unwrap_or(0)
    };
    let mut rt = Rt {
        shadow,
        ctl: Vec::new(),
        touch: None,
        ui_fd: None,
        orphans: Vec::new(),
        launches_base,
        last_hb_ms: 0,
        last_hb: String::new(),
        seen: Dedup::default(),
        attempts_set: 0,
    };
    rt.log(&format!(
        "starting{} (pid {})",
        if shadow { " in shadow mode" } else { "" },
        std::process::id()
    ));
    if !shadow {
        {
            use std::os::unix::fs::DirBuilderExt;
            let _ = fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(STATE_DIR);
        }
        // corner-wake is retired: it would start our UI behind our back.
        sh("killall corner-wake 2>/dev/null");
        // One-time boot chores that used to live in start.sh's launch path.
        if std::path::Path::new(START_SH).exists() {
            sh(&format!("sh {START_SH} prep >/dev/null 2>&1"));
        }
        // The new FIFO only (R4 round ③); a stale old one from round ① is removed.
        let _ = fs::remove_file("/tmp/u60-uid.ctl");
        rt.ctl = screen::CTL_FIFOS.iter().filter_map(|p| sys::ctl_open(p)).collect();
    }
    let (mut s, acts) = Screen::new(sys::mono_ms(), sys::find_ui());
    rt.exec(&mut s, acts);
    let mut ui_exit: Option<Option<i32>> = None;
    let mut first = true; // the first step right away, not after a full wait

    loop {
        // (re)watch the UI on screen
        let cur_pid = s.cur.map(|u| u.pid);
        if rt.ui_fd.as_ref().map(|(p, _)| Some(*p)) != Some(cur_pid) {
            if let Some((old, fd)) = rt.ui_fd.take()
                && !rt.shadow
                && cur_pid != Some(old)
                && reap(old).is_none()
            {
                rt.orphans.push((old, Some(fd))); // dropped by a hold while still running
            }
            if let Some(p) = cur_pid {
                rt.ui_fd = sys::pidfd_open(p).map(|fd| (p, fd));
            }
        }
        rt.heartbeat(&s, worker_round.load(Ordering::Relaxed), false);

        let p = load_persist();
        let now = sys::mono_ms();
        let mut wake = s.next_wake_ms(now, &p);
        if cur_pid.is_some() && rt.ui_fd.is_none() {
            wake = wake.min(now + 5000); // no pidfd (ENOSYS): check every 5 s
        }
        if rt.orphans.iter().any(|(_, fd)| fd.is_none()) {
            wake = wake.min(now + 500); // an orphan without a pidfd: look twice a second
        }
        let mut fds: Vec<libc::pollfd> = Vec::new();
        let mut push = |fd: i32| {
            fds.push(libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            })
        };
        if let Some(f) = &sig {
            push(f.as_raw_fd());
        }
        for (r, _) in &rt.ctl {
            push(r.as_raw_fd());
        }
        if let Some((_, fd)) = &rt.ui_fd {
            push(fd.as_raw_fd());
        }
        for (_, fd) in &rt.orphans {
            if let Some(fd) = fd {
                push(fd.as_raw_fd());
            }
        }
        if let Some(t) = &rt.touch {
            push(t.file.as_raw_fd());
        }
        let timeout = if first {
            0
        } else {
            (wake - now).clamp(0, screen::HEARTBEAT_MS) as i32
        };
        first = false;
        // SAFETY: fds is a valid array of pollfd.
        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };

        if let Some(f) = &sig {
            let mut buf = [0u8; 128];
            // SAFETY: reading the signal pipe into a small buffer.
            if unsafe {
                libc::read(
                    f.as_raw_fd(),
                    buf.as_mut_ptr() as *mut libc::c_void,
                    buf.len(),
                )
            } > 0
            {
                break;
            }
        }
        let mut cmds = Vec::new();
        for (r, _) in rt.ctl.iter_mut() {
            let mut b = String::new();
            let _ = std::io::Read::read_to_string(r, &mut b);
            cmds.extend(screen::parse_cmds(&b));
        }
        let now = sys::mono_ms();
        let mut fired = false;
        if let Some(t) = rt.touch.as_mut() {
            for (x, y, pressed) in t.read() {
                fired |= s.corner.feed(
                    screen::in_corner_box(x, y),
                    pressed,
                    now,
                    screen::CORNER_HOLD_MS,
                );
            }
        }
        rt.orphans.retain(|(o, _)| reap(*o).is_none());
        if let Some(ui) = s.cur {
            ui_exit = match ui.kind {
                screen::Kind::Child if !rt.shadow => reap(ui.pid),
                _ => (!sys::pid_is_ui(ui.pid)).then_some(None),
            };
        }
        let inp = Input {
            now_ms: now,
            persist: load_persist(),
            ui_exit: ui_exit.take(),
            found_devui: if s.wants_scan() {
                sys::find_ui()
            } else {
                None
            },
            vendor_alive: sys::find_comm(VENDOR_COMM).is_some(),
            card0_held: s.needs_card0() && sys::held_open(DRM_DEV),
            cmds,
            corner_fired: fired,
            ship_yield: ship_yield(),
        };
        let before = s.on_screen();
        let acts = s.step(&inp);
        rt.exec(&mut s, acts);
        if s.on_screen() != before {
            rt.heartbeat(&s, worker_round.load(Ordering::Relaxed), true);
        }
    }

    // Stopping leaves the UI on screen; the next sentinel adopts it. Remove the FIFO: with no reader
    // left, a shell `echo … > ctl` would block forever (2026-09-23).
    if !shadow {
        for p in screen::CTL_FIFOS {
            let _ = fs::remove_file(p);
        }
    }
    rt.log("stopping (u60pro-devui left running)");
}
