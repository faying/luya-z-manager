//! Phase 10 X9 (manager docs/designs/u60-phase10.md §3 X9, §6): the duties of touch-ui
//! `scripts/supervise.sh` taken over by the core, so the procd inits run the programs directly
//! (`sh -c 'exec … >>/tmp/<name>.log 2>&1'`: no wrapper stays, procd tracks the program's pid).
//!
//! What supervise.sh did, and who does it now:
//!
//! - output to `/tmp/<name>.log`: the init's `>>` (O_APPEND, so the sentinel's copy-then-truncate
//!   cap keeps working; it caps both logs);
//! - a crash keeps the last 200 lines of that log in `/data/crashlog/<name>/` (5 per program,
//!   1 MB in all) and raises an alert (`/data/alerts`, the same queue alert-lib.sh writes):
//!   - **luyaz-web, luyaz-sentinel** ([`watch`]): the core waits on a pidfd of each procd
//!     instance's pid; when it ends, it asks procd (`service list`) once the exit is reaped. The
//!     instance still listed but not running (procd's respawn is pending: `respawn 300 5 5` waits
//!     5 s) is a crash; the service gone, or running again under a new pid that soon, is a stop
//!     or restart through the init ([`judge`]). Exit status: procd's `exit_code` when its dump
//!     has one, otherwise "status unknown".
//!   - **the core itself** ([`Marker`]): a marker in its state dir, written once both listeners are
//!     bound (a second copy that lost the bind never touches it) and removed on the TERM/INT path
//!     only. Found at start with this boot's id: the last run crashed; the record is the log's
//!     tail as it was before this start ([`log_len_at_start`]). Another boot's marker is a reboot,
//!     not a crash. Side-by-side builds (`luyaz-core.test`) never write one ([`marks`]).
//! - a requested stop is not a crash: above;
//! - the leftover child of a SIGKILLed wrapper: there is no wrapper; the core refuses a second
//!   copy (port bind, serve.rs) and so does luyaz-web (its only `--bind`).

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const CRASH_DIR: &str = "/data/crashlog";
pub const CRASH_KEEP: usize = 5;
pub const CRASH_TOTAL: u64 = 1_048_576;
pub const TAIL_LINES: usize = 200;
/// luyaz-core.init's log.
pub const CORE_LOG: &str = "/tmp/luyaz-core.log";
pub const CORE_KIND: &str = "core-crash";
/// In the core's state dir (/data/luyaz/core).
pub const MARKER: &str = "/data/luyaz/core/core.running";

/// The procd services the core watches: (service = program = crashlog dir, alert kind, log).
pub const WATCHED: [(&str, &str, &str); 2] = [
    ("luyaz-web", "web-crash", "/tmp/luyaz-web.log"),
    (
        "luyaz-sentinel",
        "sentinel-crash",
        "/tmp/luyaz-sentinel.log",
    ),
];

/// The sentinel's own log is its line-by-line writer's; its init sends stdout/stderr (a panic's last
/// words) to this one. A crash record of the sentinel carries both tails.
pub const SENTINEL_OUT: &str = "/tmp/luyaz-sentinel.out";
const OUT_LINES: usize = 50;

/// A watched program's second log (its stderr), when its init keeps one apart.
pub fn stderr_log(svc: &str) -> Option<&'static str> {
    (svc == "luyaz-sentinel").then_some(SENTINEL_OUT)
}

/// The record's body: the log's tail, then the stderr log's tail (when there is one and it has lines).
pub fn evidence(log_text: &str, out: Option<(&str, &str)>) -> String {
    let mut e = tail(log_text, TAIL_LINES);
    if let Some((name, text)) = out.filter(|(_, t)| !t.trim().is_empty()) {
        e.push_str(&format!(
            "\n--- last {OUT_LINES} lines of {name} ---\n{}",
            tail(text, OUT_LINES)
        ));
    }
    e
}

/// A watched service not running is looked for again this often.
const RESCAN: Duration = Duration::from_secs(60);
/// After an exit: first look at procd this much later (it has reaped by then).
const REAP_WAIT: Duration = Duration::from_millis(500);
/// …and again this often while it still shows the old pid, at most [`REAP_TRIES`] times.
const REAP_TRIES: u32 = 10;
/// After a crash: procd's respawn wait (5 s) and some.
const RESPAWN_WAIT: Duration = Duration::from_secs(7);

// ── sans-IO ─────────────────────────────────────────────────────────────────

/// One service as `ubus call service list '{"name":…}'` shows it.
#[derive(Debug, PartialEq)]
pub enum Seen {
    Running(u32),
    /// listed, not running (a respawn pending, or given up); procd's `exit_code` if it says
    Down(Option<i64>),
    /// not listed: stopped, or never started
    Gone,
}

/// The first instance of `svc` in a `service list` reply.
pub fn seen(list: &Value, svc: &str) -> Seen {
    let Some(inst) = list
        .get(svc)
        .and_then(|s| s.get("instances"))
        .and_then(Value::as_object)
        .and_then(|m| m.values().next())
    else {
        return Seen::Gone;
    };
    let pid = inst.get("pid").and_then(Value::as_u64).filter(|&p| p > 0);
    match (inst.get("running").and_then(Value::as_bool), pid) {
        (Some(true), Some(p)) => Seen::Running(p as u32),
        _ => Seen::Down(inst.get("exit_code").and_then(Value::as_i64)),
    }
}

#[derive(Debug, PartialEq)]
pub enum Verdict {
    Crash(Option<i64>),
    /// stopped or restarted through the init (`service delete` / stop + start)
    Stopped,
    /// procd still shows the old pid: ask again
    NotYet,
}

/// `old` ended; what procd shows now says why.
pub fn judge(old: u32, now: &Seen) -> Verdict {
    match now {
        Seen::Running(p) if *p == old => Verdict::NotYet,
        Seen::Running(_) | Seen::Gone => Verdict::Stopped,
        Seen::Down(code) => Verdict::Crash(*code),
    }
}

/// Exit status in words (supervise.sh `describe`); procd gives signals as 128 + n.
pub fn describe(code: Option<i64>) -> String {
    match code {
        None => "exited, status unknown".into(),
        Some(c) if c > 128 => match c - 128 {
            6 => "killed by SIGABRT".into(),
            9 => "killed by SIGKILL".into(),
            11 => "killed by SIGSEGV".into(),
            15 => "killed by SIGTERM".into(),
            n => format!("killed by signal {n}"),
        },
        Some(c) => format!("exit {c}"),
    }
}

/// The last `n` lines of `text`.
pub fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// The core's running marker: `<boot_id> <pid>`.
#[derive(Debug, PartialEq)]
pub struct Marker {
    pub boot: String,
    pub pid: u32,
}

impl Marker {
    pub fn parse(s: &str) -> Option<Marker> {
        let mut f = s.split_whitespace();
        let boot = f.next()?.to_string();
        let pid = f.next()?.parse().ok()?;
        Some(Marker { boot, pid })
    }
    pub fn line(&self) -> String {
        format!("{} {}\n", self.boot, self.pid)
    }
    /// A marker left by the last run says it crashed only when it is from this boot (and not ours).
    pub fn crashed(prev: Option<&Marker>, boot: &str, me: u32) -> bool {
        prev.is_some_and(|m| !boot.is_empty() && m.boot == boot && m.pid != me)
    }
}

/// Only the installed program (argv0 file name exactly `luyaz-core`) keeps a marker: a
/// side-by-side `luyaz-core.test` that a trial kills must not leave a crash for the next start.
pub fn marks(argv0: &str) -> bool {
    Path::new(argv0).file_name().and_then(|n| n.to_str()) == Some("luyaz-core")
}

// ── files ───────────────────────────────────────────────────────────────────

/// Writes `<dir>/<prog>/<YYYYMMDD-HHMMSS>-up<N>.log` as supervise.sh did (header, then the log tail;
/// u60-api health.rs reads `status:`), keeps the newest [`CRASH_KEEP`] of the program and drops the
/// oldest anywhere while the tree is over [`CRASH_TOTAL`]. Best effort.
pub fn record(
    dir: &Path,
    prog: &str,
    why: &str,
    log: &str,
    log_tail: &str,
    wall: u64,
    up: u64,
) -> std::io::Result<PathBuf> {
    let pd = dir.join(prog);
    std::fs::create_dir_all(&pd)?;
    let civil = crate::ops::record::civil(wall); // YYYY-MM-DD HH:MM:SS
    let compact: String = civil.chars().filter(char::is_ascii_digit).collect();
    let mut f = pd.join(format!("{}-{}-up{up}.log", &compact[..8], &compact[8..]));
    let mut n = 1;
    while f.exists() {
        f = pd.join(format!(
            "{}-{}-up{up}.{n}.log",
            &compact[..8],
            &compact[8..]
        ));
        n += 1;
    }
    let body = format!(
        "program: {prog}\nstatus:  {why}\ntime:    {civil} device local (uptime {up}s; unreliable before the network sets the clock)\n--- last {TAIL_LINES} lines of {log} ---\n{log_tail}\n"
    );
    std::fs::write(&f, body)?;
    prune(dir, &pd, &f);
    Ok(f)
}

fn logs_in(d: &Path) -> Vec<(std::time::SystemTime, PathBuf, u64)> {
    let Ok(rd) = std::fs::read_dir(d) else {
        return Vec::new();
    };
    rd.flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "log"))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((m.modified().ok()?, e.path(), m.len()))
        })
        .collect()
}

fn prune(dir: &Path, pd: &Path, keep: &Path) {
    let mut mine = logs_in(pd);
    mine.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    for (_, p, _) in mine.iter().skip(CRASH_KEEP) {
        if p != keep {
            let _ = std::fs::remove_file(p);
        }
    }
    let mut all: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .flat_map(|e| logs_in(&e.path()))
        .collect();
    all.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1))); // oldest first
    let mut total: u64 = all.iter().map(|e| e.2).sum();
    for (_, p, sz) in all {
        if total <= CRASH_TOTAL {
            break;
        }
        if p != keep && std::fs::remove_file(&p).is_ok() {
            total = total.saturating_sub(sz);
        }
    }
}

/// Where the files are (the device's through `state::host_path`, or a test tree).
pub struct Files {
    pub crash: PathBuf,
    pub alerts: PathBuf,
    pub root: String,
}

impl Files {
    pub fn device() -> Files {
        Files {
            crash: crate::state::host_path(CRASH_DIR).into(),
            alerts: crate::state::host_path(luyaz_api::alerts::DIR).into(),
            root: crate::state::host_path(""),
        }
    }
    fn at(&self, p: &str) -> PathBuf {
        PathBuf::from(format!("{}{p}", self.root))
    }

    /// One crash: the record and the alert (`<prog> <why>`, as supervise.sh). Best effort.
    pub fn crash(&self, prog: &str, kind: &str, why: &str, log: &str, log_tail: &str) {
        let (wall, up) = (wall_s(), uptime_s(&self.at("/proc/uptime")));
        match record(&self.crash, prog, why, log, log_tail, wall, up) {
            Ok(f) => eprintln!("[svcwatch] {prog} {why}: kept {}", f.display()),
            Err(e) => eprintln!("[svcwatch] {prog} {why}: no crash record: {e}"),
        }
        let a = luyaz_api::alerts::Alerts::at(&self.alerts);
        if let Err(e) = a.add(kind, &format!("{prog} {why}"), wall as i64, up) {
            eprintln!("[svcwatch] {prog}: no alert: {e}");
        }
    }

    /// The core's start (after both binds): a marker from this boot's last run is its crash.
    /// Then this run's marker.
    pub fn core_started(&self, marker: &Path, boot: &str, me: u32, log_before: &str) {
        let prev = std::fs::read_to_string(marker)
            .ok()
            .and_then(|s| Marker::parse(&s));
        if Marker::crashed(prev.as_ref(), boot, me) {
            self.crash(
                crate::paths::CORE,
                CORE_KIND,
                &describe(None),
                CORE_LOG,
                &tail(before_this_start(log_before), TAIL_LINES),
            );
        }
        if let Some(d) = marker.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let tmp = marker.with_extension("running.tmp");
        let m = Marker {
            boot: boot.into(),
            pid: me,
        };
        if std::fs::write(&tmp, m.line())
            .and_then(|()| std::fs::rename(&tmp, marker))
            .is_err()
        {
            eprintln!("[svcwatch] cannot write {}", marker.display());
        }
    }
}

fn wall_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn uptime_s(p: &Path) -> u64 {
    std::fs::read_to_string(p)
        .ok()
        .and_then(|s| s.split('.').next()?.trim().parse().ok())
        .unwrap_or(0)
}

/// The init's `=== <time> start <bin>` line for this start (written just before the exec) is this
/// run's, not the last one's evidence.
pub fn before_this_start(log: &str) -> &str {
    let t = log.trim_end_matches('\n');
    match t.rsplit_once('\n') {
        Some((head, last)) if last.starts_with("=== ") && last.contains(" start ") => head,
        None if t.starts_with("=== ") && t.contains(" start ") => "",
        _ => log,
    }
}

/// The core's log as it was before this start; taken first thing, so the new run's own lines are
/// not mixed into the last one's evidence. 1 MB at most from the end (the log is capped far below).
pub fn log_len_at_start(log: &Path) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(log) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let from = len.saturating_sub(1_048_576);
    let mut buf = Vec::new();
    if f.seek(SeekFrom::Start(from)).is_ok() {
        let _ = f.take(len - from).read_to_end(&mut buf);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

// ── the core's own marker, process-wide ─────────────────────────────────────

static LOG_BEFORE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static MARK: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// First thing in serve mode.
pub fn note_start() {
    let _ = LOG_BEFORE.set(log_len_at_start(Path::new(&crate::state::host_path(CORE_LOG))));
}

/// Once both listeners are bound: the last run's crash, this run's marker, the watchers.
pub fn start(argv0: &str) {
    if marks(argv0) {
        let f = Files::device();
        let marker = PathBuf::from(crate::state::host_path(MARKER));
        f.core_started(
            &marker,
            &crate::evlog::boot_id(),
            std::process::id(),
            LOG_BEFORE.get().map_or("", String::as_str),
        );
        let _ = MARK.set(marker);
    }
    for (svc, kind, log) in WATCHED {
        tokio::spawn(watch(svc, kind, log));
    }
}

/// A requested stop (TERM/INT): the marker goes; any other end leaves it for the next start.
pub fn clean_stop() {
    if let Some(m) = MARK.get() {
        let _ = std::fs::remove_file(m);
    }
}

// ── watching procd's instances ──────────────────────────────────────────────

async fn service(ubus: &mut crate::ubus::backend::Backend, svc: &str) -> Option<Seen> {
    use crate::ubus::backend::UbusBackend;
    let args = serde_json::json!({ "name": svc });
    match ubus.call("service", "list", &args).await {
        Ok(v) => Some(seen(&v, svc)),
        Err(_) => None,
    }
}

/// Until `pid` has ended: a pidfd when the kernel has them, otherwise /proc every 5 s.
async fn exited(pid: u32) {
    // SAFETY: plain syscall; a valid fd is owned from here on.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::c_int, 0) };
    if fd >= 0 {
        use std::os::unix::io::{FromRawFd, OwnedFd};
        let owned = unsafe { OwnedFd::from_raw_fd(fd as i32) };
        if let Ok(a) = tokio::io::unix::AsyncFd::with_interest(owned, tokio::io::Interest::READABLE)
        {
            let _ = a.readable().await;
            return;
        }
    }
    let p = PathBuf::from(format!("/proc/{pid}"));
    while p.exists() {
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn watch(svc: &'static str, kind: &'static str, log: &'static str) {
    let mut ubus = crate::ubus::backend::Backend::from_env();
    let files = Files::device();
    loop {
        let Some(Seen::Running(pid)) = service(&mut ubus, svc).await else {
            tokio::time::sleep(RESCAN).await;
            continue;
        };
        exited(pid).await;
        let mut verdict = Verdict::NotYet;
        for _ in 0..REAP_TRIES {
            tokio::time::sleep(REAP_WAIT).await;
            if let Some(s) = service(&mut ubus, svc).await {
                verdict = judge(pid, &s);
                if verdict != Verdict::NotYet {
                    break;
                }
            }
        }
        if let Verdict::Crash(code) = verdict {
            let read =
                |p: &str| std::fs::read_to_string(crate::state::host_path(p)).unwrap_or_default();
            let out = stderr_log(svc).map(|o| (o, read(o)));
            let body = evidence(&read(log), out.as_ref().map(|(o, t)| (*o, t.as_str())));
            files.crash(svc, kind, &describe(code), log, &body);
            tokio::time::sleep(RESPAWN_WAIT).await;
        }
    }
}

#[cfg(test)]
#[path = "svcwatch/tests.rs"]
mod tests;
