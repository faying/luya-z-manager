//! The real device behind the [`crate::worker::WorkerIo`] / [`crate::wifi::WifiOps`] traits and the
//! screen thread: /proc scans, card0, the core's HTTP port, the locks, logs, the touch device.
//! Linux only. Every fd is opened with O_CLOEXEC (std does), so nothing leaks into the UI we spawn.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::wifi::{self, Core, WifiOps};
use crate::worker::{Health, WorkerIo};

// ── time ────────────────────────────────────────────────────────────────────

#[allow(clippy::unnecessary_cast)] // time_t is not i64 on every target
fn clock(id: libc::clockid_t) -> (i64, i64) {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: valid pointer to a timespec.
    unsafe { libc::clock_gettime(id, &mut ts) };
    (ts.tv_sec, ts.tv_nsec)
}

/// CLOCK_MONOTONIC milliseconds: stops in suspend (§4).
pub fn mono_ms() -> i64 {
    let (s, ns) = clock(libc::CLOCK_MONOTONIC);
    s * 1000 + ns / 1_000_000
}

/// Whole seconds of `/proc/uptime` (boottime: counts through suspend), 0 when unreadable.
pub fn uptime_s() -> u64 {
    fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|t| t.split('.').next().and_then(|x| x.trim().parse().ok()))
        .unwrap_or(0)
}

pub fn wall_s() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `YYYY-MM-DDTHH:MM:SS` of a wall clock that is local time labelled UTC (the device's): formatted as
/// UTC, which gives the local digits (`localtime_r` under TZ=UTC prints the same).
pub fn stamp(wall: i64) -> String {
    let days = wall.div_euclid(86_400);
    let secs = wall.rem_euclid(86_400);
    // civil from days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

// ── files ───────────────────────────────────────────────────────────────────

pub fn read(path: &str) -> Option<String> {
    fs::read(path)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/')
        .map(|(d, _)| d)
        .filter(|d| !d.is_empty())
        .unwrap_or("/")
}

/// Whole file, tmp + rename (tmpfs state files).
pub fn write_atomic(path: &str, content: &str) -> std::io::Result<()> {
    let _ = fs::create_dir_all(parent(path));
    let tmp = format!("{path}.tmp");
    fs::write(&tmp, content)?;
    fs::rename(&tmp, path)
}

/// Whole file + fsync + rename + fsync of the directory (the attempts
/// count must survive a crash that takes the device down right after the launch).
pub fn write_synced(path: &str, content: &str) -> std::io::Result<()> {
    write_synced_bytes(path, content.as_bytes())
}

pub fn write_synced_bytes(path: &str, content: &[u8]) -> std::io::Result<()> {
    // The directory is the caller's: never
    // change the mode of one that exists — this is also called for files right under /tmp.
    let dir = parent(path);
    let _ = fs::create_dir_all(dir);
    let tmp = format!("{path}.tmp");
    {
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(content)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

pub fn poke(path: &str, content: &str) -> bool {
    OpenOptions::new()
        .write(true)
        .open(path)
        .and_then(|mut f| f.write_all(content.as_bytes()))
        .is_ok()
}

// ── processes ───────────────────────────────────────────────────────────────

fn comm(pid: &str) -> Option<String> {
    fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|s| s.trim_end_matches('\n').to_string())
}

/// Is `pid` alive and still a process whose comm starts with `prefix`? (kill(pid, 0) cannot tell
/// ours from a new one that reused the pid.)
pub fn pid_is(pid: i32, prefix: &str) -> bool {
    comm(&pid.to_string())
        .map(|c| c.starts_with(prefix))
        .unwrap_or(false)
}

/// Is `pid` our touch UI (either name, [`crate::screen::is_ui_comm`])?
pub fn pid_is_ui(pid: i32) -> bool {
    comm(&pid.to_string())
        .map(|c| crate::screen::is_ui_comm(&c))
        .unwrap_or(false)
}

/// First pid that is our touch UI, under either name (never this process).
pub fn find_ui() -> Option<i32> {
    let me = std::process::id() as i32;
    let rd = fs::read_dir("/proc").ok()?;
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(n) = name.to_str() else { continue };
        let Ok(pid) = n.parse::<i32>() else { continue };
        if pid != me
            && comm(n)
                .map(|c| crate::screen::is_ui_comm(&c))
                .unwrap_or(false)
        {
            return Some(pid);
        }
    }
    None
}

/// First pid whose comm starts with `prefix` (never this process).
pub fn find_comm(prefix: &str) -> Option<i32> {
    let me = std::process::id() as i32;
    let rd = fs::read_dir("/proc").ok()?;
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(n) = name.to_str() else { continue };
        let Ok(pid) = n.parse::<i32>() else { continue };
        if pid == me {
            continue;
        }
        if comm(n).map(|c| c.starts_with(prefix)).unwrap_or(false) {
            return Some(pid);
        }
    }
    None
}

/// Does any process hold `dev` open? (readlink over every /proc/<pid>/fd.)
pub fn held_open(dev: &str) -> bool {
    !openers(dev).is_empty()
}

/// Pids with `path` open, and each fd's `flags:` from fdinfo (octal).
fn openers(path: &str) -> Vec<(i32, Option<u32>)> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir("/proc") else {
        return out;
    };
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(n) = name.to_str() else { continue };
        let Ok(pid) = n.parse::<i32>() else { continue };
        let Ok(fds) = fs::read_dir(format!("/proc/{n}/fd")) else {
            continue;
        };
        for f in fds.flatten() {
            let Ok(target) = fs::read_link(f.path()) else {
                continue;
            };
            if target.as_os_str() != path {
                continue;
            }
            let fdn = f.file_name();
            let flags = fs::read_to_string(format!("/proc/{n}/fdinfo/{}", fdn.to_string_lossy()))
                .ok()
                .and_then(|t| {
                    t.lines()
                        .find_map(|l| l.strip_prefix("flags:"))
                        .and_then(|v| u32::from_str_radix(v.trim(), 8).ok())
                });
            out.push((pid, flags));
        }
    }
    out
}

pub fn appenders_only(path: &str) -> bool {
    openers(path)
        .iter()
        .all(|(_, fl)| matches!(fl, Some(f) if f & libc::O_APPEND as u32 != 0))
}

pub fn cmdline_has(pid: i32, needle: &str) -> bool {
    fs::read(format!("/proc/{pid}/cmdline"))
        .map(|b| {
            String::from_utf8_lossy(&b)
                .replace('\0', " ")
                .contains(needle)
        })
        .unwrap_or(false)
}

/// A LISTEN socket on `port` in /proc/net/tcp or tcp6.
pub fn listening(port: u16) -> bool {
    let want = format!(":{port:04X}");
    ["/proc/net/tcp", "/proc/net/tcp6"].iter().any(|p| {
        read(p)
            .map(|t| {
                t.lines().skip(1).any(|l| {
                    let f: Vec<&str> = l.split_whitespace().collect();
                    f.len() > 3 && f[1].ends_with(&want) && f[3] == "0A"
                })
            })
            .unwrap_or(false)
    })
}

/// Run a command to completion within `limit`; its exit success.
pub fn run(cmd: &mut Command, limit: Duration) -> Result<String, String> {
    cmd.stdin(Stdio::null());
    match luyaz_wifi::output_within(cmd, limit) {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
        Ok(o) => Err(format!(
            "{}: {}",
            o.status,
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => Err(e.to_string()),
    }
}

pub fn pidfd_open(pid: i32) -> Option<OwnedFd> {
    // SAFETY: plain syscall; a valid fd is owned by us from here on.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        None
    } else {
        Some(unsafe { OwnedFd::from_raw_fd(fd as RawFd) })
    }
}

// ── the core's port ─────────────────────────────────────────────────────────

pub const CORE: &str = "127.0.0.1:9460";

pub enum HttpErr {
    Refused,
    Other(String),
}

/// One HTTP/1.1 request to the core with `Connection: close`; (status, whole body).
pub fn http(
    method: &str,
    path: &str,
    body: Option<&str>,
    timeout: Duration,
) -> Result<(u16, String), HttpErr> {
    let addr: SocketAddr = CORE.parse().map_err(|e| HttpErr::Other(format!("{e}")))?;
    let mut s = match TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
            return Err(HttpErr::Refused);
        }
        Err(e) => return Err(HttpErr::Other(e.to_string())),
    };
    let _ = s.set_read_timeout(Some(timeout));
    let _ = s.set_write_timeout(Some(timeout));
    let b = body.unwrap_or("");
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{b}",
        b.len()
    );
    s.write_all(req.as_bytes())
        .map_err(|e| HttpErr::Other(e.to_string()))?;
    let mut buf = Vec::new();
    let deadline = Instant::now() + timeout;
    let mut chunk = [0u8; 4096];
    loop {
        match s.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) => return Err(HttpErr::Other(e.to_string())),
        }
        if Instant::now() >= deadline || buf.len() > 1 << 20 {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buf).into_owned();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    Ok((status, body))
}

pub fn healthz() -> Health {
    match http("GET", "/healthz", None, Duration::from_secs(2)) {
        Ok((200, _)) => Health::Ok,
        Ok((_, body)) if body.contains("starting") => Health::Starting,
        Ok((code, body)) => Health::Bad(format!(
            "{code} {}",
            body.trim().chars().take(60).collect::<String>()
        )),
        Err(HttpErr::Refused) => Health::Down("refused".into()),
        Err(HttpErr::Other(e)) => Health::Down(e),
    }
}

// ── locks ───────────────────────────────────────────────────────────────────

/// The write lock, the same file as the core's and `luyaz-fallback.sh`'s (`/var/run/u60-write.lock`
/// before Luya-Z rename E2).
pub const WRITE_LOCK: &str = "/var/run/luyaz-write.lock";

/// flock(LOCK_EX) on `path`, polled every 200 ms for at most `wait` (the write lock; u60-wifi's own
/// lock function is for the Wi-Fi lock — its in-process flag is per process, one lock at a time).
pub fn flock_wait(path: &str, wait: Duration) -> Result<File, String> {
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| format!("open {path}: {e}"))?;
    let deadline = Instant::now() + wait;
    loop {
        // SAFETY: flock on a valid, owned fd.
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(f);
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EWOULDBLOCK) {
            return Err(format!("flock {path}: {err}"));
        }
        if Instant::now() >= deadline {
            return Err(format!("{path} held over {}s", wait.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// The one-sentinel lock (Phase 10 X9 follow-up): the real sentinel holds `flock(LOCK_EX)` on this
/// file for its whole life. A second copy — any name, `luyaz-sentinel.test` too — fails to take it and
/// exits before it touches the FIFO, the heartbeat or the screen. `--shadow` takes none (it runs next
/// to the real one). The fd is O_CLOEXEC (std's default), so the UI it launches never inherits it.
pub const INSTANCE_LOCK: &str = "/tmp/luyaz/luyaz-sentinel.lock";

/// Ok(file): ours while the file lives. Err: held by another process (or cannot be opened).
pub fn single_instance(path: &str) -> Result<File, String> {
    if let Some(d) = std::path::Path::new(path).parent() {
        let _ = fs::create_dir_all(d);
    }
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("open {path}: {e}"))?;
    // SAFETY: flock on a valid, owned fd.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let err = std::io::Error::last_os_error();
        return Err(if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
            format!("another luyaz-sentinel holds {path}")
        } else {
            format!("flock {path}: {err}")
        });
    }
    Ok(f)
}

// ── logs ────────────────────────────────────────────────────────────────────

/// The screen log.
pub const UID_LOG: &str = "/tmp/luyaz-screen.log";
pub const SENTINEL_LOG: &str = "/tmp/luyaz-sentinel.log";
pub const SHADOW_LOG: &str = "/tmp/luyaz-sentinel.shadow";
const SHADOW_CAP: u64 = 512 * 1024;

static LOG_LOCK: Mutex<()> = Mutex::new(());

/// Append one line (opened O_APPEND per line: the guard's log cap may truncate it).
pub fn append(path: &str, line: &str) {
    let _g = LOG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if path == SHADOW_LOG
        && fs::metadata(path)
            .map(|m| m.len() > SHADOW_CAP)
            .unwrap_or(false)
    {
        let _ = fs::rename(path, format!("{path}.old"));
    }
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o644)
        .open(path)
    {
        let _ = f.write_all(format!("{line}\n").as_bytes());
    }
}

/// Both logs, kept on flash too: /tmp goes with the reboot that most needs them read
/// (10-25 u60g: the screen went dark after a failed touch ship, the firmware rebooted,
/// and why the sentinel brought no UI back went with /tmp). A few lines an hour, so
/// each one synced; `<boot8> <screen|worker> <line>`, 64 KB then one `.old`.
pub const KEPT_LOG: &str = "/data/luyaz/screen/log";
const KEPT_CAP: u64 = 64 * 1024;

fn keep(tag: &str, line: &str) {
    let b: String = boot_id()
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .take(8)
        .collect();
    let _g = LOG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    keep_at(KEPT_LOG, KEPT_CAP, &format!("{b} {tag} {line}"));
}

pub(crate) fn keep_at(path: &str, cap: u64, line: &str) {
    if fs::metadata(path).map(|m| m.len() >= cap).unwrap_or(false) {
        let _ = fs::rename(path, format!("{path}.old"));
    }
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o644)
        .open(path)
    {
        let _ = f.write_all(format!("{line}\n").as_bytes());
        let _ = f.sync_data();
    }
}

/// The screen log line: `<stamp> <msg>` (no prefix: ship's UID_BAD matches the bare message).
pub fn uid_log(msg: &str) {
    eprintln!("luyaz-sentinel: {msg}");
    let line = format!("{} {msg}", stamp(wall_s()));
    append(UID_LOG, &line);
    keep("screen", &line);
}

/// The guard's log line shape: `<stamp> up=<uptime> <msg>`.
pub fn worker_log(msg: &str) {
    let line = format!("{} up={} {msg}", stamp(wall_s()), uptime_s());
    append(SENTINEL_LOG, &line);
    keep("worker", &line);
}

pub fn shadow_log(who: &str, msg: &str) {
    append(
        SHADOW_LOG,
        &format!(
            "{} up={} mono={} {who} {msg}",
            stamp(wall_s()),
            uptime_s(),
            mono_ms() / 1000
        ),
    );
}

// ── events (u60-sentinel.md §6) ────────────────────────────────────────────

type EventTx = std::sync::mpsc::Sender<(String, String)>;
static EVENTS: OnceLock<Mutex<EventTx>> = OnceLock::new();

/// From here on, [`alert`] hands events to the events thread (real mode).
pub fn set_event_sender(tx: EventTx) {
    let _ = EVENTS.set(Mutex::new(tx));
}

/// An alert from the sentinel: an event with an id through the events thread when it runs (real
/// mode), else straight into the queue.
pub fn alert(kind: &str, text: &str) {
    if let Some(tx) = EVENTS.get()
        && tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send((kind.to_string(), text.to_string()))
            .is_ok()
    {
        return;
    }
    let a = luyaz_alerts::Alerts::at(std::path::Path::new(luyaz_alerts::DIR));
    if let Err(e) = a.add(kind, text, wall_s(), uptime_s()) {
        eprintln!("luyaz-sentinel: alert {kind}: {e}");
    }
}

// ── the worker's device ─────────────────────────────────────────────────────

pub const DROPBEAR_START: &str = "/data/local/tmp/start_dropbear.sh";

/// The real Wi-Fi operations (`wifi.rs`).
#[derive(Default)]
pub struct RealWifi {
    wifi: Option<luyaz_wifi::WifiLock>,
    write: Option<File>,
}

impl WifiOps for RealWifi {
    fn core_listening(&mut self) -> bool {
        let addr: SocketAddr = CORE.parse().expect("core address");
        TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok()
    }
    fn core(&mut self, body: &str) -> Core {
        match http("POST", "/control", Some(body), Duration::from_secs(30)) {
            Err(HttpErr::Refused) => wifi::classify(Err("refused".into()), true),
            Err(HttpErr::Other(e)) => wifi::classify(Err(e), false),
            Ok((_, b)) => wifi::classify(Ok(b), false),
        }
    }
    fn lock_wifi(&mut self, wait: Duration) -> Result<(), String> {
        self.wifi = Some(luyaz_wifi::lock_at(luyaz_wifi::LOCK_FILE, wait)?);
        Ok(())
    }
    fn unlock_wifi(&mut self) {
        self.wifi = None;
    }
    fn lock_write(&mut self, wait: Duration) -> Result<(), String> {
        self.write = Some(flock_wait(WRITE_LOCK, wait)?);
        Ok(())
    }
    fn unlock_write(&mut self) {
        self.write = None;
    }
    fn uci_get(&mut self, key: &str) -> Option<String> {
        run(
            Command::new("uci").args(["-q", "get", key]),
            Duration::from_secs(5),
        )
        .ok()
        .map(|s| s.trim().to_string())
    }
    fn uci_set_commit(&mut self, pairs: &[(&str, &str)]) -> Result<(), String> {
        for (k, v) in pairs {
            run(
                Command::new("uci").args(["set", &format!("{k}={v}")]),
                Duration::from_secs(5),
            )?;
        }
        run(
            Command::new("uci").args(["commit", "wireless"]),
            Duration::from_secs(10),
        )
        .map(|_| ())
    }
    fn ubus(&mut self, object: &str, method: &str, args: &str) -> Result<(), String> {
        run(
            Command::new("ubus").args(["call", object, method, args]),
            Duration::from_secs(35),
        )
        .map(|_| ())
    }
    fn beaconing(&mut self) -> bool {
        luyaz_wifi::beaconing()
    }
    fn sleep_s(&mut self, s: u64) {
        std::thread::sleep(Duration::from_secs(s));
    }
    fn log(&mut self, msg: &str) {
        worker_log(msg);
    }
}

/// A recover-live still running after this is stuck: killed with its group, the slot freed.
const RECOVER_CAP: Duration = Duration::from_secs(15 * 60);

#[derive(Default)]
pub struct RealWorker {
    /// The recover-live we started and when.
    recover: luyaz_wifi::Slot,
}

/// Open once, reject symlinks/non-root/writable files, and bound reads before JSON parsing.
pub(crate) fn trusted_utc_file(path: &std::path::Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let meta = f.metadata().ok()?;
    if !meta.is_file()
        || meta.uid() != 0
        || meta.mode() & 0o022 != 0
        || meta.nlink() != 1
        || meta.len() > 4096
    {
        return None;
    }
    let mut text = String::new();
    f.take(4097).read_to_string(&mut text).ok()?;
    (text.len() <= 4096).then_some(text)
}

impl WorkerIo for RealWorker {
    fn utc_anchor(&mut self) -> Option<String> {
        trusted_utc_file(std::path::Path::new(luyaz_alerts::utc::ANCHOR_PATH))
    }
    fn boot_time(&mut self) -> Option<luyaz_alerts::clock::BootTime> {
        luyaz_alerts::clock::BootTime::read()
    }
    fn uptime(&mut self) -> u64 {
        uptime_s()
    }
    fn wall(&mut self) -> i64 {
        wall_s()
    }
    fn mono_s(&mut self) -> i64 {
        mono_ms() / 1000
    }
    fn read(&mut self, path: &str) -> Option<String> {
        read(path)
    }
    fn exists(&mut self, path: &str) -> bool {
        std::path::Path::new(path).exists()
    }
    fn size(&mut self, path: &str) -> Option<u64> {
        fs::metadata(path)
            .ok()
            .filter(|m| m.is_file())
            .map(|m| m.len())
    }
    fn cmdline_has(&mut self, pid: i32, needle: &str) -> bool {
        cmdline_has(pid, needle)
    }
    fn appenders_only(&mut self, path: &str) -> bool {
        appenders_only(path)
    }
    fn beaconing(&mut self) -> bool {
        luyaz_wifi::beaconing()
    }
    fn healthz(&mut self) -> Health {
        healthz()
    }
    fn dropbear_listening(&mut self) -> bool {
        listening(2222)
    }
    fn write(&mut self, path: &str, content: &str) {
        if let Err(e) = write_atomic(path, content) {
            worker_log(&format!("cannot write {path}: {e}"));
        }
    }
    fn persist(&mut self, path: &str, content: &str) {
        if let Err(e) = write_synced(path, content) {
            worker_log(&format!("cannot write {path}: {e}"));
        }
    }
    fn poke(&mut self, path: &str, content: &str) -> bool {
        poke(path, content)
    }
    fn remove(&mut self, path: &str) {
        let _ = fs::remove_file(path);
    }
    fn log(&mut self, msg: &str) {
        worker_log(msg);
    }
    fn alert(&mut self, kind: &str, text: &str) {
        alert(kind, text);
    }
    fn restore_wifi(&mut self) -> Result<(), String> {
        wifi::restore(&mut RealWifi::default())
    }
    fn set_autosleep(&mut self, on: bool) {
        wifi::set_autosleep(&mut RealWifi::default(), on);
    }
    fn recover_live(&mut self) -> bool {
        match luyaz_wifi::check_slot(&mut self.recover, RECOVER_CAP) {
            luyaz_wifi::SlotState::Running(_) => return false, // ours is still at it
            luyaz_wifi::SlotState::Killed(pid) => worker_log(&format!(
                "recover-live (pid {pid}) still running after {} s: killed",
                RECOVER_CAP.as_secs()
            )),
            _ => {}
        }
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(SENTINEL_LOG);
        let mut cmd = Command::new("sh");
        let ship = crate::worker::Paths::default().ship;
        cmd.args([ship.as_str(), "recover-live"])
            .stdin(Stdio::null());
        // its own group, so the cap above takes its children too
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        if let Ok(f) = log
            && let Ok(f2) = f.try_clone()
        {
            cmd.stdout(f).stderr(f2);
        }
        match cmd.spawn() {
            Ok(c) => {
                self.recover = Some((c, Instant::now()));
                true
            }
            Err(e) => {
                worker_log(&format!("cannot start recover-live: {e}"));
                false
            }
        }
    }
    fn cap_log(&mut self, path: &str) -> bool {
        // copy-then-truncate: the writers keep their fds (all O_APPEND, checked before)
        fs::copy(path, format!("{path}.old")).is_ok()
            && OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(path)
                .is_ok()
    }
    fn start_dropbear(&mut self) -> bool {
        run(
            Command::new("sh").arg(DROPBEAR_START),
            Duration::from_secs(20),
        )
        .is_ok()
    }
    fn core_being_replaced(&mut self) -> bool {
        core_being_replaced()
    }
    fn boot_is_normal(&mut self) -> bool {
        boot_is_normal()
    }
    fn mtime(&mut self, path: &str) -> Option<i64> {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(path).ok().map(|m| m.mtime())
    }
    fn alert_id(&mut self, id: &str, kind: &str, text: &str) {
        // straight into the queue (the core may be the one that is down); the core reads it back
        let a = luyaz_alerts::Alerts::at(std::path::Path::new(luyaz_alerts::DIR));
        if let Err(e) = a.add_once(id, kind, text, wall_s(), uptime_s()) {
            worker_log(&format!("regress: queue {id}: {e}"));
        }
    }
    fn proc_blind(&mut self) -> bool {
        fs::read_dir("/proc").is_err()
    }
    fn disk_free(&mut self, path: &str) -> Option<crate::worker::Disk> {
        disk_free(path)
    }
    fn largest(&mut self, path: &str) -> String {
        largest(path, 5)
    }
    fn restart_core(&mut self) -> Result<(), String> {
        run(
            Command::new(CORE_INITD).arg("restart"),
            Duration::from_secs(45),
        )
        .map(|_| ())
    }
    fn alert_sms(&mut self) {
        let a = luyaz_alerts::Alerts::at(std::path::Path::new(luyaz_alerts::DIR));
        let r = a.sms_round("sentinel", std::process::id(), &mut SentinelSms);
        if !matches!(r, luyaz_alerts::sms::Round::Idle) {
            worker_log(&format!("alert sms (core unhealthy): {r:?}"));
        }
    }
}

/// A ship transaction of one of `comps` with a live executor (heartbeat ≤ 90 s, its pid running
/// the executor).
pub fn txn_live(comps: &[&str]) -> bool {
    let crate::worker::Paths {
        ship_txn, ship_hb, ..
    } = crate::worker::Paths::default();
    let Some(txn) = read(&ship_txn) else {
        return false;
    };
    let mut comp = "";
    let mut phase = "";
    for l in txn.lines() {
        if let Some(v) = l.strip_prefix("comp=") {
            comp = v;
        } else if let Some(v) = l.strip_prefix("phase=") {
            phase = v;
        }
    }
    if !comps.contains(&comp)
        || !matches!(
            phase,
            "staged" | "trial" | "promote" | "check" | "manifest" | "rollback"
        )
    {
        return false;
    }
    let hb = read(&ship_hb).unwrap_or_default();
    let mut f = hb.split_whitespace();
    let (Some(hu), Some(hp)) = (
        f.next().and_then(|x| x.parse::<u64>().ok()),
        f.next().and_then(|x| x.parse::<i32>().ok()),
    ) else {
        return false;
    };
    uptime_s().saturating_sub(hu) <= crate::worker::SHIP_STALE_S
        && cmdline_has(hp, crate::worker::SHIP_CMDLINE)
}

/// Power-off charging is `mode_power_off_*` in zwrt_zte_mc_tmp; power-on
/// modes are normal; with neither, a non-silent boot on the kernel command line is.
pub fn boot_is_normal() -> bool {
    boot_mode_normal(
        &mc_tmp("mode_main_state"),
        &read("/proc/cmdline").unwrap_or_default(),
    )
}

/// One `option <name> '<value>'` of zwrt_zte_mc_tmp ("" when missing).
fn mc_tmp(name: &str) -> String {
    let key = format!("option {name}");
    read("/etc/config/zwrt_zte_mc_tmp")
        .and_then(|t| {
            t.lines()
                .find(|l| l.contains(&key))
                .and_then(|l| l.split('\'').nth(1).map(str::to_string))
        })
        .unwrap_or_default()
}

pub fn boot_mode_normal(state: &str, cmdline: &str) -> bool {
    if state.starts_with("mode_power_off_") {
        return false;
    }
    if state == "mode_power_on" || state == "mode_power_on_charger" {
        return true;
    }
    cmdline.contains("silent_boot.mode=nonsilent")
}

/// The boot conditions, one block per sentinel start.
pub const BOOT_TRACE: &str = "/data/luyaz/ui/boot-trace.log";
const BOOT_TRACE_KEEP: usize = 160;
const BOOT_TRACE_FILES: &[&str] = &[
    "/sys/class/power_supply/usb/online",
    "/sys/class/power_supply/usb/voltage_now",
    "/sys/class/power_supply/battery/status",
    "/sys/class/power_supply/battery/capacity",
    "/sys/class/power_supply/charger_zte/present_mbb",
    "/sys/class/power_supply/charger_zte/status_mbb",
    "/sys/class/power_supply/type-c_zte/present_mbb",
    "/sys/class/power_supply/type-c_zte/real_type_mbb",
    "/sys/class/power_supply/statistics_zte/batt_status",
    "/sys/class/power_supply/statistics_zte/batt_online",
    "/sys/class/power_supply/battery_zte/status_mbb",
    "/sys/class/power_supply/battery_zte/online_mbb",
];

/// Appends this boot's block to `path` and keeps the last 160 lines (start.sh's format).
pub fn boot_trace(path: &str) {
    let state = mc_tmp("mode_main_state");
    let cmdline = read("/proc/cmdline").unwrap_or_default();
    let mut b = format!(
        "=== {} ===\nmode_main_state={state}\nreboot_reason_code={}\nbootmode={}\ncmdline={}",
        stamp(wall_s()).replace('T', " "),
        mc_tmp("reboot_reason_code"),
        // power-off charging boots do not expose silent_boot.mode=nonsilent
        if boot_mode_normal(&state, &cmdline) {
            "normal"
        } else {
            "charge"
        },
        cmdline
    );
    if !b.ends_with('\n') {
        b.push('\n');
    }
    for f in BOOT_TRACE_FILES {
        if std::path::Path::new(f).exists() {
            b.push_str(&format!("{f}={}\n", read(f).unwrap_or_default().trim_end()));
        }
    }
    let mut events: Vec<String> = fs::read_dir("/dev/input")
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("event"))
                .collect()
        })
        .unwrap_or_default();
    events.sort();
    for e in events {
        let name = read(&format!("/sys/class/input/{e}/device/name")).unwrap_or_default();
        b.push_str(&format!("/dev/input/{e}={}\n", name.trim_end()));
    }
    b.push('\n');
    let _ = write_atomic(path, &boot_trace_keep(&read(path).unwrap_or_default(), &b));
}

/// `old` + `block`, cut to the last 160 lines.
pub fn boot_trace_keep(old: &str, block: &str) -> String {
    let all = format!("{old}{block}");
    let lines: Vec<&str> = all.lines().collect();
    let from = lines.len().saturating_sub(BOOT_TRACE_KEEP);
    let mut out = lines[from..].join("\n");
    out.push('\n');
    out
}

pub const CORE_INITD: &str = "/etc/init.d/luyaz-core";

/// The room on the filesystem holding `path`.
pub fn disk_free(path: &str) -> Option<crate::worker::Disk> {
    let c = std::ffi::CString::new(path).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: a valid C string and a zeroed out-struct of the right type.
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let f = st.f_frsize as u64;
    Some(crate::worker::Disk {
        free: st.f_bavail as u64 * f,
        total: st.f_blocks as u64 * f,
        ifree: st.f_favail as u64,
        itotal: st.f_files as u64,
    })
}

/// Bytes under `path`, this filesystem only, symlinks not followed; stops at `budget` entries.
fn tree_size(path: &std::path::Path, dev: u64, budget: &mut u32) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let Ok(m) = fs::symlink_metadata(path) else {
        return 0;
    };
    if m.dev() != dev {
        return 0;
    }
    if !m.is_dir() {
        return m.blocks() * 512;
    }
    let mut sum = m.blocks() * 512;
    if let Ok(rd) = fs::read_dir(path) {
        for e in rd.flatten() {
            if *budget == 0 {
                break;
            }
            *budget -= 1;
            sum += tree_size(&e.path(), dev, budget);
        }
    }
    sum
}

/// The `n` biggest entries two levels under `path` (`luyaz/core 312M, logfs 41M`), for the record
/// of a filesystem running full: what filled it is the question afterwards.
pub fn largest(path: &str, n: usize) -> String {
    use std::os::unix::fs::MetadataExt;
    let root = std::path::Path::new(path);
    let Ok(dev) = fs::metadata(root).map(|m| m.dev()) else {
        return String::new();
    };
    let mut budget = 200_000u32;
    let mut sizes: Vec<(u64, String)> = Vec::new();
    let Ok(top) = fs::read_dir(root) else {
        return String::new();
    };
    for e in top.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        let is_dir = fs::symlink_metadata(&p)
            .map(|m| m.is_dir() && m.dev() == dev)
            .unwrap_or(false);
        let subs: Vec<_> = if is_dir {
            fs::read_dir(&p)
                .map(|r| r.flatten().collect())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        if subs.is_empty() {
            sizes.push((tree_size(&p, dev, &mut budget), name));
            continue;
        }
        for s in subs {
            let sub = format!("{name}/{}", s.file_name().to_string_lossy());
            sizes.push((tree_size(&s.path(), dev, &mut budget), sub));
        }
    }
    sizes.sort_by_key(|s| std::cmp::Reverse(s.0));
    sizes
        .iter()
        .take(n)
        .filter(|(b, _)| *b > 0)
        .map(|(b, p)| format!("{p} {}M", b >> 20))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The core is being replaced (D6): a core transaction with a live executor.
pub fn core_being_replaced() -> bool {
    txn_live(&["core"])
}

fn boot_id() -> String {
    read("/proc/sys/kernel/random/boot_id")
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

#[derive(Default)]
pub struct RealEvents;

impl crate::events::EventIo for RealEvents {
    fn wall(&mut self) -> i64 {
        wall_s()
    }
    fn uptime(&mut self) -> u64 {
        uptime_s()
    }
    fn boot8(&mut self) -> String {
        let b: String = boot_id()
            .chars()
            .filter(|c| c.is_ascii_hexdigit())
            .take(8)
            .collect();
        if b.len() == 8 { b } else { "00000000".into() }
    }
    fn read(&mut self, path: &str) -> Option<String> {
        read(path)
    }
    fn write_synced(&mut self, path: &str, content: &str) {
        if let Err(e) = write_synced(path, content) {
            worker_log(&format!("events: cannot write {path}: {e}"));
        }
    }
    fn append_synced(&mut self, path: &str, line: &str) {
        let r = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
            .and_then(|mut f| {
                f.write_all(format!("{line}\n").as_bytes())?;
                f.sync_all()
            });
        if let Err(e) = r {
            worker_log(&format!("events: cannot append to {path}: {e}"));
        }
    }
    fn post(&mut self, ev: &crate::events::Event) -> crate::events::Post {
        use crate::events::Post;
        let body = serde_json::json!({"id": ev.id, "kind": ev.kind, "text": ev.text,
            "wall": ev.wall, "uptime": ev.uptime})
        .to_string();
        match http(
            "POST",
            "/v2/sentinel/event",
            Some(&body),
            Duration::from_secs(5),
        ) {
            Ok((200, b)) if b.contains("\"ok\":true") => Post::Acked,
            Ok((code, b)) => {
                Post::NoAck(format!("{code} {}", b.chars().take(60).collect::<String>()))
            }
            Err(HttpErr::Refused) => Post::NoAck("refused".into()),
            Err(HttpErr::Other(e)) => Post::NoAck(e),
        }
    }
    fn enqueue(&mut self, ev: &crate::events::Event) -> bool {
        let a = luyaz_alerts::Alerts::at(std::path::Path::new(luyaz_alerts::DIR));
        a.add_once(&ev.id, &ev.kind, &ev.text, ev.wall, ev.uptime)
            .map_err(|e| worker_log(&format!("events: queue {}: {e}", ev.id)))
            .is_ok()
    }
    fn core_being_replaced(&mut self) -> bool {
        core_being_replaced()
    }
    fn log(&mut self, msg: &str) {
        worker_log(msg);
    }
}

// ── alert SMS while the core is unhealthy (§6, D15) ─────────────────────────

const CLOCK_OK: &str = "/tmp/luyaz/clock-ok";

/// The touch UI's settings: ui.conf.
pub fn ui_conf() -> Option<String> {
    read(luyaz_alerts::sms::UI_CONF)
}

/// The sentinel as an alert SMS sender: straight over ubus (the core is the thing that is down).
#[derive(Default)]
pub struct SentinelSms;

fn ubus_json(
    obj: &str,
    method: &str,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let out = run(
        Command::new("ubus").args(["call", obj, method, &args.to_string()]),
        Duration::from_secs(30),
    )?;
    if out.trim().is_empty() {
        return Ok(serde_json::Value::Null);
    }
    serde_json::from_str(&out).map_err(|e| format!("{obj} {method}: {e}"))
}

impl luyaz_alerts::sms::SmsIo for SentinelSms {
    fn wall(&mut self) -> i64 {
        wall_s()
    }
    fn uptime(&mut self) -> u64 {
        uptime_s()
    }
    fn boot_id(&mut self) -> String {
        boot_id()
    }
    fn pid_alive(&mut self, pid: u32) -> bool {
        // SAFETY: signal 0 only checks that the process exists.
        let r = unsafe { libc::kill(pid as i32, 0) };
        r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    fn clock_ok(&mut self) -> bool {
        luyaz_alerts::clock::evidence_status(
            &read(CLOCK_OK).unwrap_or_default(),
            luyaz_alerts::clock::BootTime::read().as_ref(),
            wall_s(),
        )
        .wall_usable()
    }
    fn wall_settled(&mut self) -> bool {
        luyaz_alerts::clock::evidence_settled(
            &read(CLOCK_OK).unwrap_or_default(),
            luyaz_alerts::clock::BootTime::read().as_ref(),
            wall_s(),
        )
    }
    fn lang_en(&mut self) -> bool {
        ui_conf().is_some_and(|c| luyaz_alerts::sms::lang_en(&c))
    }
    fn tz_hours(&mut self) -> i32 {
        let v = run(
            Command::new("uci").args(["-q", "get", "zwrt_zte_sntp.settings.time_from_utc"]),
            Duration::from_secs(5),
        )
        .unwrap_or_default();
        luyaz_alerts::sms::tz_hours(&v)
    }
    fn send(&mut self, args: &serde_json::Value) -> Result<(), luyaz_alerts::sms::SendError> {
        use luyaz_alerts::sms::SendError::{NotSent, Rejected};
        // a ubus call that failed or timed out: the service may be down, or it may have gone out
        // — sent again next round (D15)
        let r = ubus_json("zwrt_wms", "zte_libwms_send_sms", args).map_err(NotSent)?;
        if luyaz_alerts::sms::send_result_ok(r.get("result")) {
            Ok(())
        } else {
            Err(Rejected(format!(
                "device rejected (result={})",
                r["result"]
            )))
        }
    }
    fn delete_sent_copy(&mut self, number: &str) {
        std::thread::sleep(Duration::from_secs(1));
        for store in [1, 0] {
            let args = serde_json::json!({"tags": 2, "page": 0, "data_per_page": 5,
                "mem_store": store, "order_by": "order by id desc"});
            let Ok(list) = ubus_json("zwrt_wms", "zte_libwms_get_sms_data", &args) else {
                continue;
            };
            let Some(id) = luyaz_alerts::sms::newest_to(&list, number) else {
                continue;
            };
            match ubus_json(
                "zwrt_wms",
                "zwrt_wms_delete_sms",
                &serde_json::json!({"id": id}),
            ) {
                Ok(_) => worker_log(&format!("sms: deleted the sent copy (id {id})")),
                Err(e) => worker_log(&format!("sms: deleting the sent copy {id}: {e}")),
            }
            return;
        }
        worker_log("sms: sent copy not found to delete");
    }
    fn log(&mut self, msg: &str) {
        worker_log(msg);
    }
}

// ── the touch device (what the corner gesture needs of it) ──

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;
const SYN_REPORT: u16 = 0x00;
const BTN_TOUCH: u16 = 0x14a;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;

const fn ioc_read(nr: u64, size: u64) -> u64 {
    (2 << 30) | (size << 16) | ((b'E' as u64) << 8) | nr
}

#[repr(C)]
#[derive(Default)]
struct AbsInfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

/// arm64 `struct input_event`: two longs + u16 + u16 + i32 = 24 bytes.
const EVENT_SIZE: usize = 24;

pub struct Touch {
    pub file: File,
    mt: bool,
    min_x: i32,
    max_x: i32,
    min_y: i32,
    max_y: i32,
    raw_x: i32,
    raw_y: i32,
    pressed: bool,
    pub x: i32,
    pub y: i32,
}

fn test_bit(bits: &[u64; 1], b: u16) -> bool {
    (bits[0] >> b) & 1 == 1
}

impl Touch {
    /// Probe /dev/input/event* for a touchscreen; never EVIOCGRAB (the vendor UI must keep every event).
    pub fn open() -> Option<Touch> {
        let mut names: Vec<String> = fs::read_dir("/dev/input")
            .ok()?
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
            .filter(|n| n.starts_with("event"))
            .collect();
        names.sort();
        for n in names {
            let path = format!("/dev/input/{n}");
            let Ok(file) = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&path)
            else {
                continue;
            };
            let fd = file.as_raw_fd();
            let mut bits = [0u64; 1];
            // SAFETY: EVIOCGBIT(EV_ABS, 8) into an 8-byte buffer.
            if unsafe {
                libc::ioctl(
                    fd,
                    ioc_read(0x20 + EV_ABS as u64, 8) as _,
                    bits.as_mut_ptr(),
                )
            } < 0
            {
                continue;
            }
            let mt = test_bit(&bits, ABS_MT_POSITION_X) && test_bit(&bits, ABS_MT_POSITION_Y);
            if !mt && !(test_bit(&bits, ABS_X) && test_bit(&bits, ABS_Y)) {
                continue;
            }
            let mut t = Touch {
                file,
                mt,
                min_x: 0,
                max_x: crate::screen::SCREEN_W - 1,
                min_y: 0,
                max_y: crate::screen::SCREEN_H - 1,
                raw_x: 0,
                raw_y: 0,
                pressed: false,
                x: 0,
                y: 0,
            };
            let axis = |code: u16| {
                let mut a = AbsInfo::default();
                // SAFETY: EVIOCGABS into a 24-byte input_absinfo.
                let ok = unsafe {
                    libc::ioctl(
                        fd,
                        ioc_read(0x40 + code as u64, 24) as _,
                        &mut a as *mut AbsInfo,
                    )
                } == 0;
                (ok && a.maximum > a.minimum).then_some(a)
            };
            if let Some(a) = axis(if mt { ABS_MT_POSITION_X } else { ABS_X }) {
                (t.min_x, t.max_x, t.raw_x) = (a.minimum, a.maximum, a.value);
            }
            if let Some(a) = axis(if mt { ABS_MT_POSITION_Y } else { ABS_Y }) {
                (t.min_y, t.max_y, t.raw_y) = (a.minimum, a.maximum, a.value);
            }
            if mt {
                // multitouch positions are per slot, not in the axis info: read the current slot's
                let mut slot = 0usize;
                let mut sl = AbsInfo::default();
                // SAFETY: as above.
                if unsafe {
                    libc::ioctl(
                        fd,
                        ioc_read(0x40 + ABS_MT_SLOT as u64, 24) as _,
                        &mut sl as *mut AbsInfo,
                    )
                } == 0
                    && (0..16).contains(&sl.value)
                {
                    slot = sl.value as usize;
                }
                for (code, dst) in [(ABS_MT_POSITION_X, 0), (ABS_MT_POSITION_Y, 1)] {
                    let mut q = [0i32; 17];
                    q[0] = code as i32;
                    // SAFETY: EVIOCGMTSLOTS(68) into a 68-byte buffer (code + 16 values).
                    if unsafe { libc::ioctl(fd, ioc_read(0x0a, 68) as _, q.as_mut_ptr()) } == 0 {
                        if dst == 0 {
                            t.raw_x = q[1 + slot]
                        } else {
                            t.raw_y = q[1 + slot]
                        }
                    }
                }
            }
            return Some(t);
        }
        None
    }

    fn scale(v: i32, lo: i32, hi: i32, out_max: i32) -> i32 {
        if hi <= lo {
            return 0;
        }
        let v = v.clamp(lo, hi);
        ((v - lo) as i64 * out_max as i64 / (hi - lo) as i64) as i32
    }

    /// Drain pending events; every SYN_REPORT gives one (x, y, pressed) sample in display pixels,
    /// after the panel's 180° turn (the U60 Pro mounts it upside down).
    pub fn read(&mut self) -> Vec<(i32, i32, bool)> {
        let mut out = Vec::new();
        let mut buf = [0u8; EVENT_SIZE * 32];
        let mut new_press = self.pressed;
        loop {
            let n = match self.file.read(&mut buf) {
                Ok(n) if n >= EVENT_SIZE => n,
                _ => break,
            };
            for ev in buf[..n - n % EVENT_SIZE].as_chunks::<EVENT_SIZE>().0 {
                let ty = u16::from_ne_bytes([ev[16], ev[17]]);
                let code = u16::from_ne_bytes([ev[18], ev[19]]);
                let value = i32::from_ne_bytes([ev[20], ev[21], ev[22], ev[23]]);
                match (ty, code) {
                    (EV_ABS, ABS_MT_POSITION_X) if self.mt => self.raw_x = value,
                    (EV_ABS, ABS_MT_POSITION_Y) if self.mt => self.raw_y = value,
                    (EV_ABS, ABS_X) if !self.mt => self.raw_x = value,
                    (EV_ABS, ABS_Y) if !self.mt => self.raw_y = value,
                    (EV_ABS, ABS_MT_TRACKING_ID) => new_press = value >= 0,
                    (EV_KEY, BTN_TOUCH) => new_press = value != 0,
                    (EV_SYN, SYN_REPORT) => {
                        let w = crate::screen::SCREEN_W;
                        let h = crate::screen::SCREEN_H;
                        let sx = Self::scale(self.raw_x, self.min_x, self.max_x, w - 1);
                        let sy = Self::scale(self.raw_y, self.min_y, self.max_y, h - 1);
                        self.x = (w - 1) - sx;
                        self.y = (h - 1) - sy;
                        self.pressed = new_press;
                        out.push((self.x, self.y, self.pressed));
                    }
                    _ => {}
                }
            }
        }
        if out.is_empty() {
            out.push((self.x, self.y, self.pressed)); // a poll with no event: the state as it stands
        }
        out
    }
}

/// Whether `path` is still the FIFO `reader` reads (same device and inode).
pub fn ctl_current(path: &str, reader: &File) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::metadata(path), reader.metadata()) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

pub fn ctl_open(path: &str) -> Option<(File, File)> {
    let _ = fs::remove_file(path);
    let c = std::ffi::CString::new(path).ok()?;
    // SAFETY: mkfifo on a valid C string.
    if unsafe { libc::mkfifo(c.as_ptr(), 0o600) } != 0 {
        uid_log(&format!(
            "mkfifo {path}: {}",
            std::io::Error::last_os_error()
        ));
        return None;
    }
    let r = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    // Hold a writer ourselves so the FIFO never reports EOF between writers.
    let w = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    Some((r, w))
}

// ── the explanation frame on DRM ───────────────────────────────────────────
// The kernel's drm_mode.h structs (aarch64 layout, repr(C)) for a dumb buffer,
// plus SET_MASTER/DROP_MASTER: the frame is drawn only when this process
// gets master; a UI that still holds card0 (hung, not yet exited) makes SET_MASTER fail and the
// frame is skipped.

pub mod drm {
    #[repr(C)]
    #[derive(Default)]
    pub struct CardRes {
        pub fb_id_ptr: u64,
        pub crtc_id_ptr: u64,
        pub connector_id_ptr: u64,
        pub encoder_id_ptr: u64,
        pub count_fbs: u32,
        pub count_crtcs: u32,
        pub count_connectors: u32,
        pub count_encoders: u32,
        pub min_width: u32,
        pub max_width: u32,
        pub min_height: u32,
        pub max_height: u32,
    }
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    pub struct ModeInfo {
        pub clock: u32,
        pub hdisplay: u16,
        pub hsync_start: u16,
        pub hsync_end: u16,
        pub htotal: u16,
        pub hskew: u16,
        pub vdisplay: u16,
        pub vsync_start: u16,
        pub vsync_end: u16,
        pub vtotal: u16,
        pub vscan: u16,
        pub vrefresh: u32,
        pub flags: u32,
        pub type_: u32,
        pub name: [u8; 32],
    }
    #[repr(C)]
    #[derive(Default)]
    pub struct GetConnector {
        pub encoders_ptr: u64,
        pub modes_ptr: u64,
        pub props_ptr: u64,
        pub prop_values_ptr: u64,
        pub count_modes: u32,
        pub count_props: u32,
        pub count_encoders: u32,
        pub encoder_id: u32,
        pub connector_id: u32,
        pub connector_type: u32,
        pub connector_type_id: u32,
        pub connection: u32,
        pub mm_width: u32,
        pub mm_height: u32,
        pub subpixel: u32,
        pub pad: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    pub struct GetEncoder {
        pub encoder_id: u32,
        pub encoder_type: u32,
        pub crtc_id: u32,
        pub possible_crtcs: u32,
        pub possible_clones: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    pub struct CreateDumb {
        pub height: u32,
        pub width: u32,
        pub bpp: u32,
        pub flags: u32,
        pub handle: u32,
        pub pitch: u32,
        pub size: u64,
    }
    #[repr(C)]
    #[derive(Default)]
    pub struct FbCmd {
        pub fb_id: u32,
        pub width: u32,
        pub height: u32,
        pub pitch: u32,
        pub bpp: u32,
        pub depth: u32,
        pub handle: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    pub struct MapDumb {
        pub handle: u32,
        pub pad: u32,
        pub offset: u64,
    }
    #[repr(C)]
    #[derive(Default)]
    pub struct Crtc {
        pub set_connectors_ptr: u64,
        pub count_connectors: u32,
        pub crtc_id: u32,
        pub fb_id: u32,
        pub x: u32,
        pub y: u32,
        pub gamma_size: u32,
        pub mode_valid: u32,
        pub mode: ModeInfo,
    }

    const fn iowr(nr: u64, size: usize) -> u64 {
        (3 << 30) | ((size as u64) << 16) | ((b'd' as u64) << 8) | nr
    }
    pub const SET_MASTER: u64 = ((b'd' as u64) << 8) | 0x1e;
    pub const DROP_MASTER: u64 = ((b'd' as u64) << 8) | 0x1f;
    pub const GETRESOURCES: u64 = iowr(0xA0, size_of::<CardRes>());
    pub const SETCRTC: u64 = iowr(0xA2, size_of::<Crtc>());
    pub const GETENCODER: u64 = iowr(0xA6, size_of::<GetEncoder>());
    pub const GETCONNECTOR: u64 = iowr(0xA7, size_of::<GetConnector>());
    pub const ADDFB: u64 = iowr(0xAE, size_of::<FbCmd>());
    pub const RMFB: u64 = iowr(0xAF, size_of::<u32>());
    pub const CREATE_DUMB: u64 = iowr(0xB2, size_of::<CreateDumb>());
    pub const MAP_DUMB: u64 = iowr(0xB3, size_of::<MapDumb>());
    pub const DESTROY_DUMB: u64 = iowr(0xB4, size_of::<u32>());
    pub const CONNECTED: u32 = 1;

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn layouts_match_the_kernel() {
            assert_eq!(size_of::<CardRes>(), 64);
            assert_eq!(size_of::<ModeInfo>(), 68);
            assert_eq!(size_of::<GetConnector>(), 80);
            assert_eq!(size_of::<GetEncoder>(), 20);
            assert_eq!(size_of::<CreateDumb>(), 32);
            assert_eq!(size_of::<FbCmd>(), 28);
            assert_eq!(size_of::<MapDumb>(), 16);
            assert_eq!(size_of::<Crtc>(), 104);
            // the numbers the C headers give (aarch64)
            assert_eq!(GETRESOURCES, 0xC04064A0);
            assert_eq!(GETCONNECTOR, 0xC05064A7);
            assert_eq!(SETCRTC, 0xC06864A2);
            assert_eq!(CREATE_DUMB, 0xC02064B2);
            assert_eq!(ADDFB, 0xC01C64AE);
            assert_eq!(MAP_DUMB, 0xC01064B3);
            assert_eq!(RMFB, 0xC00464AF);
            assert_eq!(DESTROY_DUMB, 0xC00464B4);
            assert_eq!(SET_MASTER, 0x641E);
        }
    }
}

fn drm_io<T>(fd: RawFd, req: u64, arg: &mut T) -> std::io::Result<()> {
    loop {
        // SAFETY: `arg` is the repr(C) struct this request takes.
        let r = unsafe { libc::ioctl(fd, req as _, arg as *mut T) };
        if r == 0 {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

/// The first connected connector, its preferred mode and a CRTC for it (touch-ui
/// `pick_connector_crtc`).
fn drm_pick(fd: RawFd) -> Result<(u32, u32, drm::ModeInfo), String> {
    let mut res = drm::CardRes::default();
    drm_io(fd, drm::GETRESOURCES, &mut res).map_err(|e| format!("GETRESOURCES: {e}"))?;
    let mut crtcs = vec![0u32; res.count_crtcs as usize];
    let mut conns = vec![0u32; res.count_connectors as usize];
    let mut encs = vec![0u32; res.count_encoders as usize];
    res.crtc_id_ptr = crtcs.as_mut_ptr() as u64;
    res.connector_id_ptr = conns.as_mut_ptr() as u64;
    res.encoder_id_ptr = encs.as_mut_ptr() as u64;
    res.fb_id_ptr = 0;
    res.count_fbs = 0;
    drm_io(fd, drm::GETRESOURCES, &mut res).map_err(|e| format!("GETRESOURCES: {e}"))?;
    for &cid in &conns {
        let mut c = drm::GetConnector {
            connector_id: cid,
            ..Default::default()
        };
        if drm_io(fd, drm::GETCONNECTOR, &mut c).is_err()
            || c.connection != drm::CONNECTED
            || c.count_modes == 0
        {
            continue;
        }
        let mut modes = vec![drm::ModeInfo::default(); c.count_modes as usize];
        let mut cencs = vec![0u32; c.count_encoders as usize];
        c.modes_ptr = modes.as_mut_ptr() as u64;
        c.encoders_ptr = cencs.as_mut_ptr() as u64;
        c.props_ptr = 0;
        c.prop_values_ptr = 0;
        c.count_props = 0;
        if drm_io(fd, drm::GETCONNECTOR, &mut c).is_err() || c.count_modes == 0 {
            continue;
        }
        let mut crtc = 0;
        if c.encoder_id != 0 {
            let mut e = drm::GetEncoder {
                encoder_id: c.encoder_id,
                ..Default::default()
            };
            if drm_io(fd, drm::GETENCODER, &mut e).is_ok() {
                crtc = e.crtc_id;
            }
        }
        for &eid in cencs.iter().take(c.count_encoders as usize) {
            if crtc != 0 {
                break;
            }
            let mut e = drm::GetEncoder {
                encoder_id: eid,
                ..Default::default()
            };
            if drm_io(fd, drm::GETENCODER, &mut e).is_err() {
                continue;
            }
            if let Some(i) = (0..crtcs.len()).find(|i| e.possible_crtcs & (1 << i) != 0) {
                crtc = crtcs[i];
            }
        }
        if crtc == 0 {
            crtc = crtcs.first().copied().unwrap_or(0);
        }
        if crtc != 0 {
            return Ok((cid, crtc, modes[0]));
        }
    }
    Err("no connected connector".into())
}

/// Show the explanation frame for [`crate::frame::SHOW_MS`], then let go of everything (the
/// framebuffer, master, card0) so the vendor UI can take the panel. Err = skipped (another
/// process holds the panel, or a DRM call failed); nothing is left behind either way.
pub fn show_frame(en: bool) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    let card = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(crate::run::DRM_DEV)
        .map_err(|e| format!("open: {e}"))?;
    let fd = card.as_raw_fd();
    // SAFETY: SET_MASTER takes no argument.
    if unsafe { libc::ioctl(fd, drm::SET_MASTER as _, 0) } != 0 {
        return Err(format!(
            "panel held by another process ({})",
            std::io::Error::last_os_error()
        ));
    }
    let r = draw(fd, en);
    // SAFETY: as above.
    unsafe { libc::ioctl(fd, drm::DROP_MASTER as _, 0) };
    drop(card);
    r
}

fn draw(fd: RawFd, en: bool) -> Result<(), String> {
    let (conn, crtc, mode) = drm_pick(fd)?;
    let (w, h) = (mode.hdisplay as usize, mode.vdisplay as usize);
    if w == 0 || h == 0 {
        return Err("mode has no size".into());
    }
    let mut dumb = drm::CreateDumb {
        width: w as u32,
        height: h as u32,
        bpp: 16,
        ..Default::default()
    };
    drm_io(fd, drm::CREATE_DUMB, &mut dumb).map_err(|e| format!("CREATE_DUMB: {e}"))?;
    let handle = dumb.handle;
    let result = (|| {
        let mut fb = drm::FbCmd {
            width: w as u32,
            height: h as u32,
            pitch: dumb.pitch,
            bpp: 16,
            depth: 16,
            handle,
            ..Default::default()
        };
        drm_io(fd, drm::ADDFB, &mut fb).map_err(|e| format!("ADDFB: {e}"))?;
        let fb_id = fb.fb_id;
        let shown = (|| {
            let mut map = drm::MapDumb {
                handle,
                ..Default::default()
            };
            drm_io(fd, drm::MAP_DUMB, &mut map).map_err(|e| format!("MAP_DUMB: {e}"))?;
            // SAFETY: mapping the dumb buffer the kernel just sized for us.
            let addr = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    dumb.size as usize,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    fd,
                    map.offset as libc::off_t,
                )
            };
            if addr == libc::MAP_FAILED {
                return Err(format!("mmap: {}", std::io::Error::last_os_error()));
            }
            let mut px = crate::frame::compose(en, w, h);
            crate::frame::rotate180(&mut px);
            let pitch_px = dumb.pitch as usize / 2;
            // SAFETY: `size` bytes mapped; rows of `pitch` bytes, `w` <= pitch_px pixels each.
            unsafe {
                let dst = addr as *mut u16;
                for y in 0..h {
                    std::ptr::copy_nonoverlapping(px.as_ptr().add(y * w), dst.add(y * pitch_px), w);
                }
                libc::munmap(addr, dumb.size as usize);
            }
            let mut cid = conn;
            let mut set = drm::Crtc {
                set_connectors_ptr: &mut cid as *mut u32 as u64,
                count_connectors: 1,
                crtc_id: crtc,
                fb_id,
                mode_valid: 1,
                mode,
                ..Default::default()
            };
            drm_io(fd, drm::SETCRTC, &mut set).map_err(|e| format!("SETCRTC: {e}"))?;
            std::thread::sleep(Duration::from_millis(crate::frame::SHOW_MS));
            Ok(())
        })();
        let mut id = fb_id;
        let _ = drm_io(fd, drm::RMFB, &mut id);
        shown
    })();
    let mut h_ = handle;
    let _ = drm_io(fd, drm::DESTROY_DUMB, &mut h_);
    result
}
