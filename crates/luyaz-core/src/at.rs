//! The few AT commands datad sends (write-op-layer.md T7b, D17): going back to
//! automatic network selection and bringing the radio online; and the two bearer
//! reads of `/api/network/qos` (8d-1b). A fixed list, never a string from a client.
//! Batch 5c adds the USSD and SIM-toolkit commands ([`Phone`], built from checked parts) and
//! the AT terminal's one command typed by a person ([`raw`]): the same lock, port pick and
//! reads, with the wait and the end of the answer each needs.
//!
//! This is the AT port's one executor (zte-agent is retired): two readers on one tty take each
//! other's replies, so every command holds a cross-process lock (`LUYAZ_AT_LOCK`, default
//! `/var/run/luyaz-at.lock`, flock; `luyaz-fallback.sh` takes it too) for the whole command.
//! Port: `LUYAZ_AT_PORT`, else the first candidate no other process holds open that answers
//! "AT" with OK. An answer ends at its final result code (`OK`, `ERROR`, `+CME ERROR:`,
//! `+CMS ERROR:`, each a whole line); a failure says which [`ErrorKind`] it is.

use fs2::FileExt;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// The candidates. The order is only a preference: a node another process holds open is skipped
/// ([`held_by`]; on B31 the stock port-bridge / port-bridge-at hold at_mdm0/1 and never let an
/// answer through, at_mdm2 is free (b31-community §1), other firmware or USB modes may differ),
/// and a node that does not exist is passed over.
const PORTS: &[&str] = &[
    "/dev/at_mdm2",
    "/dev/at_mdm0",
    "/dev/at_mdm1",
    // no at_usb0/1: those are the USB bridge, and with no USB host attached open()
    // blocks in the kernel for good (O_NONBLOCK or not): a probe hung there holding
    // the AT lock (2026-10-11)
    "/dev/smd7",
    "/dev/smd11",
];
/// What a failed command ran into, from its error text ([`error_kind`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// the lock: another command is on the port
    Busy,
    /// every candidate that exists is held open by another process
    Held,
    /// no answer in time, or the port is stuck on an earlier command
    Timeout,
    /// the modem said ERROR / +CME ERROR / +CMS ERROR, or opening, writing or reading failed
    Error,
    /// no candidate exists, or none answers "AT"
    NoPort,
}

impl ErrorKind {
    pub fn name(self) -> &'static str {
        match self {
            ErrorKind::Busy => "busy",
            ErrorKind::Held => "held",
            ErrorKind::Timeout => "timeout",
            ErrorKind::Error => "error",
            ErrorKind::NoPort => "no-port",
        }
    }
}

const BUSY: &str = "AT port busy";
const HELD: &str = "every AT port is held by another process";
const NO_PORT: &str = "no AT port found";

/// The kind of an error this module returned (its texts are stable: the callers show them).
pub fn error_kind(e: &str) -> ErrorKind {
    if e.starts_with(BUSY) {
        ErrorKind::Busy
    } else if e.starts_with(HELD) {
        ErrorKind::Held
    } else if e.starts_with(NO_PORT) {
        ErrorKind::NoPort
    } else if e.contains("stuck") || e.ends_with(": no answer") {
        ErrorKind::Timeout
    } else {
        ErrorKind::Error
    }
}

/// A final result code (V.250 / TS 27.007 §9.2): the answer is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Final {
    Ok,
    Error,
}

/// The final result code in `s`, if a line of it is one: `OK`, `ERROR`, `+CME ERROR: …` or
/// `+CMS ERROR: …`. A word inside other text (a USSD message saying "ERROR") is not one; the
/// `+CME`/`+CMS` line must have ended (its number may still be on the way).
pub fn final_code(s: &str) -> Option<Final> {
    codes(s).next()
}

/// Every final result code line in `s`, in order (see [`final_code`]).
fn codes(s: &str) -> impl Iterator<Item = Final> + '_ {
    let mut lines = s.split(['\r', '\n']).peekable();
    std::iter::from_fn(move || {
        while let Some(line) = lines.next() {
            let ended = lines.peek().is_some();
            match line.trim() {
                "OK" => return Some(Final::Ok),
                "ERROR" => return Some(Final::Error),
                l if ended && (l.starts_with("+CME ERROR:") || l.starts_with("+CMS ERROR:")) => {
                    return Some(Final::Error);
                }
                _ => {}
            }
        }
        None
    })
}

/// How long to wait for another command to finish on the port.
const LOCK_WAIT: Duration = Duration::from_secs(10);
/// An answer ends with OK or ERROR; the agent waits a fixed 8 s for COPS=0,
/// we stop at the answer and give up a little earlier.
const ANSWER_WAIT: Duration = Duration::from_secs(6);
/// Bound for the direct operate_mode read before any AT lock or port.
const MODE_READ_WAIT: Duration = Duration::from_secs(3);
/// One read's buffer: bigger than any reply on the list (`AT+CGCONTRDP` with several contexts).
const READ_BUF: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
    /// `AT+COPS=0`: automatic network selection.
    CopsAuto,
    /// Compatibility spelling only: the shared policy rejects CFUN setters before
    /// opening a port. LPM recovery requires a device restart.
    CfunOnline,
    /// `AT+CGCONTRDP`: the active PDP contexts (read).
    Cgcontrdp,
    /// `AT+CGEQOSRDP=<cid>`: one context's QoS (read).
    Cgeqosrdp(u8),
}

impl Cmd {
    fn text(self) -> String {
        match self {
            Cmd::CopsAuto => "AT+COPS=0".into(),
            Cmd::CfunOnline => "AT+CFUN=1".into(),
            Cmd::Cgcontrdp => "AT+CGCONTRDP".into(),
            Cmd::Cgeqosrdp(cid) => format!("AT+CGEQOSRDP={cid}"),
        }
    }
}

/// When an answer is complete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Until {
    /// at the final result: OK or ERROR
    Final,
    /// at a whole line starting with this (an answer that comes after the OK, `+CUSD:`), or ERROR
    Line(&'static str),
    /// never: the whole wait, what comes after the OK too (the agent's fixed `sleep`)
    Wait,
}

/// The USSD and SIM-toolkit pages' commands (the agent's `telephony.rs`): built from checked
/// parts, read for as long as the agent read them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phone {
    /// `AT+CUSD=1,"<code>",15`: a code, or a reply inside the session; only digits and `*#+`
    Ussd(String),
    /// `AT+CUSD=2`: end the session
    UssdCancel,
    /// `AT+CUAD`: the SIM's application list (EF_DIR)
    Cuad,
    /// `AT+STIN?`, read this many seconds
    Stin(u64),
    /// `AT+STGI=<n>`
    Stgi(u8),
    /// `AT+CUSATD=1`
    Cusatd,
    /// `AT+CUSATE="<menu selection of item n>"`
    Cusate(u8),
}

impl Phone {
    /// A USSD code or reply with everything but digits and `*#+` dropped (the agent's filter).
    pub fn ussd(code: &str) -> Phone {
        Phone::Ussd(
            code.chars()
                .filter(|c| c.is_ascii_digit() || "*#+".contains(*c))
                .collect(),
        )
    }

    fn job(&self) -> Job {
        let secs = Duration::from_secs;
        let (text, wait, until) = match self {
            Phone::Ussd(code) => (
                format!("AT+CUSD=1,\"{code}\",15"),
                secs(8),
                Until::Line("+CUSD:"),
            ),
            Phone::UssdCancel => ("AT+CUSD=2".into(), secs(3), Until::Final),
            Phone::Cuad => ("AT+CUAD".into(), secs(3), Until::Final),
            Phone::Stin(s) => ("AT+STIN?".into(), secs(*s), Until::Final),
            Phone::Stgi(n) => (format!("AT+STGI={n}"), secs(5), Until::Final),
            Phone::Cusatd => ("AT+CUSATD=1".into(), secs(5), Until::Wait),
            Phone::Cusate(item) => (
                format!("AT+CUSATE=\"D30782020181900101{item:02X}\""),
                secs(8),
                Until::Wait,
            ),
        };
        Job::loose(text, wait, until)
    }
}

/// Where the commands go: the lock file and the port (`None`: probe the list).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Target {
    pub lock: Option<String>,
    pub port: Option<String>,
}

impl Target {
    /// `LUYAZ_AT_LOCK` and `LUYAZ_AT_PORT`, as [`send`] uses them.
    pub fn from_env() -> Target {
        Target {
            lock: lock_path(),
            port: std::env::var("LUYAZ_AT_PORT")
                .ok()
                .filter(|p| !p.is_empty()),
        }
    }
}

/// What a command got back, and on which port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    pub port: String,
    pub text: String,
}

/// One command and how to read its answer.
struct Job {
    /// `None`: only find the port
    text: Option<String>,
    wait: Duration,
    until: Until,
    /// the answer must end in OK (the fixed list); else whatever came back is the answer
    strict: bool,
}

impl Job {
    fn fixed(cmd: Cmd) -> Job {
        Job {
            text: Some(cmd.text()),
            wait: ANSWER_WAIT,
            until: Until::Final,
            strict: true,
        }
    }

    fn loose(text: String, wait: Duration, until: Until) -> Job {
        Job {
            text: Some(text),
            wait,
            until,
            strict: false,
        }
    }

    fn name(&self) -> &str {
        self.text.as_deref().unwrap_or("AT port probe")
    }

    /// The whole command's limit: the lock's wait, one probe per port, the answer's wait.
    fn total(&self) -> Duration {
        LOCK_WAIT + Duration::from_secs(PORTS.len() as u64) + self.wait + Duration::from_secs(2)
    }
}

/// The AT terminal's command (`POST /api/at/send`): one line as typed, read until OK or ERROR
/// or `wait`; whatever came back (an ERROR, nothing) is the answer.
pub async fn raw(t: Target, text: &str, wait: Duration) -> Result<Answer, String> {
    if text.is_empty() || text.chars().any(char::is_control) {
        return Err("one command, no control characters".into());
    }
    let job = Job::loose(text.to_string(), wait, Until::Final);
    let total = job.total();
    run(job, t.lock, t.port, total).await
}

/// One USSD or SIM-toolkit command; its answer as it came (an ERROR in it is the caller's to read).
pub async fn phone(t: Target, p: Phone) -> Result<String, String> {
    let job = p.job();
    let total = job.total();
    run(job, t.lock, t.port, total).await.map(|a| a.text)
}

/// The port the commands use (`LUYAZ_AT_PORT`, or the first that answers "AT"), under the lock.
pub async fn probe(t: Target) -> Result<String, String> {
    let job = Job {
        text: None,
        wait: Duration::ZERO,
        until: Until::Final,
        strict: true,
    };
    let total = job.total();
    run(job, t.lock, t.port, total).await.map(|a| a.port)
}

fn lock_path() -> Option<String> {
    match std::env::var("LUYAZ_AT_LOCK") {
        Ok(v) if v.is_empty() => None,
        Ok(v) => Some(v),
        Err(_) => Some("/var/run/luyaz-at.lock".into()),
    }
}

/// Send `cmd`, return the modem's answer (must contain OK).
pub async fn send(cmd: Cmd) -> Result<String, String> {
    let port = std::env::var("LUYAZ_AT_PORT")
        .ok()
        .filter(|p| !p.is_empty());
    send_with(cmd, lock_path(), port).await
}

/// `send` with the lock path and port given (`port` None = probe the list).
/// Runs on its own thread; on the executor, every bounded stage that ends
/// (lock taken, a port probed, the answer read) counts as progress for the
/// watchdog and `exec_age_ms` (STATE_V2.md V2-32). Each stage has its own
/// limit, but a stage can still block inside the kernel: on B31 with the modem
/// in LPM, a call on the AT port did not return for over 30 s and the watchdog
/// killed datad twice (10-05). So the whole command gets `TOTAL_WAIT`, below
/// the watchdog's 30 s; past it the thread is left behind, and until it
/// returns every new command fails at once instead of piling up more threads.
pub(crate) async fn send_with(
    cmd: Cmd,
    lock: Option<String>,
    port: Option<String>,
) -> Result<String, String> {
    send_within(cmd, lock, port, TOTAL_WAIT).await
}

/// Whole-command limit (lock wait 10 s + probes + answer 6 s fit inside).
const TOTAL_WAIT: Duration = Duration::from_secs(25);
/// Commands left behind by `TOTAL_WAIT` and still blocked, by lock + port.
static STUCK: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn stuck() -> std::sync::MutexGuard<'static, Vec<String>> {
    STUCK.lock().unwrap_or_else(|e| e.into_inner())
}

async fn send_within(
    cmd: Cmd,
    lock: Option<String>,
    port: Option<String>,
    total: Duration,
) -> Result<String, String> {
    run(Job::fixed(cmd), lock, port, total)
        .await
        .map(|a| a.text)
}

async fn run(
    job: Job,
    lock: Option<String>,
    port: Option<String>,
    total: Duration,
) -> Result<Answer, String> {
    // Before any lock, probe, port open or thread: no manual/terminal/internal bypass.
    if job
        .text
        .as_deref()
        .is_some_and(crate::device_profile::policy::radio_set)
    {
        let policy = crate::device_profile::policy::availability("modem.recover_from_lpm", None)
            .expect("fixed recovery policy");
        return Err(format!(
            "{}: CFUN setters are blocked; recovery requires a device restart",
            policy.reason
        ));
    }
    let key = format!("{lock:?} {port:?}");
    let name = job.name().to_string();
    if stuck().contains(&key) {
        return Err(format!(
            "{name}: the AT port is still stuck on an earlier command"
        ));
    }
    // All production callers share this boundary, including terminal/USSD/probe/internal.
    // A current read permits this explicit attempt; it does not mint a radio epoch.
    // The pre-read shares the caller's total budget and has its own short bound.
    let started = Instant::now();
    let binding = if let Some(hub) = crate::executor::profile_hub() {
        let mode = tokio::time::timeout(
            MODE_READ_WAIT.min(total),
            crate::executor::call(
                "uci",
                "get",
                &serde_json::json!({
                    "config":"zte_nwinfo", "section":"sys_info", "option":"operate_mode"
                }),
            ),
        )
        .await;
        let token = hub.firmware().token();
        if !mode.as_ref().is_ok_and(|m| {
            m.as_ref()
                .is_ok_and(|v| v["value"].as_str().map(str::trim) == Some("ONLINE"))
        }) || !hub
            .firmware()
            .at_ready(&token, luyaz_api::clock::BootTime::read().as_ref())
        {
            return Err(
                "radio_not_online: a fresh ONLINE observation is required before opening AT".into(),
            );
        }
        let mark = hub.firmware().radio_mark();
        Some((hub, token, mark))
    } else {
        None
    };
    let total = total.saturating_sub(started.elapsed());
    // Port selection returns only a name, not the complete probe exchange. Neither
    // configured nor discovered names can be promoted into a synthetic OK response.
    let command = job.text.clone().unwrap_or_default();
    let original = binding.clone();
    let progress = crate::executor::progress_handle();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let done = Arc::new(AtomicBool::new(false));
    let (thread_done, thread_key) = (done.clone(), key.clone());
    std::thread::Builder::new()
        .name("at".into())
        .spawn(move || {
            let r = send_blocking(job, lock, port, &|| progress.tick(), &|| {
                // Only an observed radio change stops the attempt; lock waits and unrelated
                // ubus/USB invalidations do not (they still keep the reply from being proof).
                if original
                    .as_ref()
                    .is_none_or(|(hub, _, mark)| hub.firmware().radio_unchanged(*mark))
                {
                    Ok(())
                } else {
                    Err("radio_not_online: the radio left ONLINE before the AT command".into())
                }
            });
            // under the same mutex as the timeout path below: either it sees
            // `done`, or its entry is already there for us to remove
            let mut s = stuck();
            thread_done.store(true, Ordering::SeqCst);
            s.retain(|k| *k != thread_key);
            drop(s);
            let _ = tx.send(r);
        })
        .map_err(|e| format!("AT thread: {e}"))?;
    match tokio::time::timeout(total, rx).await {
        Ok(Ok(r)) => {
            // The command already reached the modem: always return its answer. A stale or
            // late answer only loses its value as capability evidence.
            if let Some((hub, token, _)) = &binding
                && hub
                    .firmware()
                    .at_reply_current(token, luyaz_api::clock::BootTime::read().as_ref())
            {
                let reply = r.as_ref().ok().map(|a| a.text.as_str());
                hub.firmware().at_read(
                    token.clone(),
                    luyaz_api::clock::BootTime::read(),
                    &command,
                    reply,
                );
            }
            r
        }
        Ok(Err(_)) => Err(format!("{name}: AT thread ended without an answer")),
        Err(_) => {
            let mut s = stuck();
            if !done.load(Ordering::SeqCst) {
                s.push(key);
            }
            Err(format!(
                "{name}: no answer within {} s (the AT port is stuck)",
                total.as_secs()
            ))
        }
    }
}

fn take_lock(path: Option<String>) -> Result<Option<File>, String> {
    let Some(path) = path else {
        return Ok(None);
    };
    let open = |p: &str| {
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(p)
    };
    let f = open(&path).map_err(|e| format!("AT lock {path}: {e}"))?;
    let start = Instant::now();
    loop {
        if f.try_lock_exclusive().is_ok() {
            return Ok(Some(f));
        }
        if start.elapsed() >= LOCK_WAIT {
            return Err(format!("{BUSY} (another command is running)"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn send_blocking(
    job: Job,
    lock: Option<String>,
    port: Option<String>,
    progress: &dyn Fn(),
    context: &dyn Fn() -> Result<(), String>,
) -> Result<Answer, String> {
    let _lock = take_lock(lock)?;
    context()?;
    progress();
    let port = match port {
        Some(p) => p,
        None => find_port(progress, context)?,
    };
    context()?;
    let Some(text) = &job.text else {
        return Ok(Answer {
            port,
            text: String::new(),
        });
    };
    if !job.strict {
        let answer = exchange_until(&port, text, job.wait, job.until);
        progress();
        return answer.map(|(text, _)| Answer { port, text });
    }
    let answer = exchange(&port, text, job.wait);
    progress();
    answer.and_then(|a| {
        if final_code(&a) == Some(Final::Ok) {
            Ok(Answer { port, text: a })
        } else {
            Err(format!("{text}: {}", a.trim()))
        }
    })
}

fn find_port(
    progress: &dyn Fn(),
    context: &dyn Fn() -> Result<(), String>,
) -> Result<String, String> {
    find_port_in_checked(
        PORTS,
        &|p| held_by(Path::new("/proc"), p),
        progress,
        context,
    )
}

#[cfg(test)]
fn find_port_in(
    ports: &[&str],
    held: &dyn Fn(&Path) -> Option<String>,
    progress: &dyn Fn(),
) -> Result<String, String> {
    find_port_in_checked(ports, held, progress, &|| Ok(()))
}

/// The first of `ports` that exists, that `held` says no other process has open, and that
/// answers "AT" with OK. Never writes to a held node: a probe there would take another
/// program's answer (and its "AT" would reach that program's reader).
fn find_port_in_checked(
    ports: &[&str],
    held: &dyn Fn(&Path) -> Option<String>,
    progress: &dyn Fn(),
    context: &dyn Fn() -> Result<(), String>,
) -> Result<String, String> {
    let mut holders = Vec::new();
    let mut existing = 0;
    for p in ports {
        let path = Path::new(p);
        if !path.exists() {
            continue;
        }
        existing += 1;
        if let Some(who) = held(path) {
            holders.push(format!("{p} ({who})"));
            continue;
        }
        context()?;
        let ok = exchange(p, "AT", Duration::from_secs(1))
            .is_ok_and(|a| final_code(&a) == Some(Final::Ok));
        progress();
        if ok {
            return Ok((*p).to_string());
        }
    }
    if existing > 0 && holders.len() == existing {
        return Err(format!("{HELD}: {}", holders.join(", ")));
    }
    Err(NO_PORT.into())
}

/// Which other process (its comm) holds the character device `port` open, from `<proc>/*/fd`.
/// Compares the device number (`st_rdev`), so a symlinked /dev entry still matches; skips this
/// process, and any process or descriptor that vanishes or cannot be read meanwhile.
fn held_by(proc_root: &Path, port: &Path) -> Option<String> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let meta = std::fs::metadata(port).ok()?;
    if !meta.file_type().is_char_device() {
        return None;
    }
    let rdev = meta.rdev();
    let me = std::process::id().to_string();
    for pid in std::fs::read_dir(proc_root).ok()?.flatten() {
        let name = pid.file_name();
        let name = name.to_string_lossy();
        if !name.bytes().all(|b| b.is_ascii_digit()) || name == me {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(pid.path().join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            let Ok(m) = std::fs::metadata(fd.path()) else {
                continue;
            };
            if m.file_type().is_char_device() && m.rdev() == rdev {
                let comm = std::fs::read_to_string(pid.path().join("comm")).unwrap_or_default();
                return Some(format!("{} pid {name}", comm.trim()));
            }
        }
    }
    None
}

fn set_nonblocking(f: &std::fs::File, on: bool) {
    use std::os::unix::io::AsRawFd;
    let fd = f.as_raw_fd();
    // SAFETY: fcntl on a descriptor we own, flags only.
    unsafe {
        let fl = libc::fcntl(fd, libc::F_GETFL);
        if fl >= 0 {
            let fl = if on {
                fl | libc::O_NONBLOCK
            } else {
                fl & !libc::O_NONBLOCK
            };
            libc::fcntl(fd, libc::F_SETFL, fl);
        }
    }
}

/// Write `text\r`, read until OK / ERROR or `wait`.
fn exchange(port: &str, text: &str, wait: Duration) -> Result<String, String> {
    let (s, ended) = exchange_until(port, text, wait, Until::Final)?;
    if ended {
        return Ok(s);
    }
    let s = s.trim();
    Err(if s.is_empty() {
        format!("{text}: no answer")
    } else {
        format!("{text}: {s}")
    })
}

/// Whether `s` is a whole answer by `until`.
fn complete(s: &str, until: Until) -> bool {
    match until {
        Until::Final => final_code(s).is_some(),
        Until::Line(prefix) => {
            // an error after an OK too (`AT+CUSD` says OK, then +CME ERROR)
            codes(s).any(|c| c == Final::Error)
                || s.find(prefix)
                    .is_some_and(|i| s[i..].contains(['\r', '\n']))
        }
        Until::Wait => false,
    }
}

/// The most of one answer kept (a terminal command that never stops talking).
const ANSWER_MAX: usize = 64 * 1024;

/// Write `text\r`, read until `until` or `wait`: what came, and whether `until` ended it.
fn exchange_until(
    port: &str,
    text: &str,
    wait: Duration,
    until: Until,
) -> Result<(String, bool), String> {
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(port)
        .map_err(|e| format!("open {port}: {e}"))?;
    // drop whatever an earlier command left unread
    let mut junk = [0u8; READ_BUF];
    while matches!(f.read(&mut junk), Ok(n) if n > 0) {}
    // The write is blocking: on B31 a non-blocking write to at_mdm2 always says EBUSY (measured
    // 10-13, 50 tries over 1 s), while a blocking one goes through at once, as the agent's `echo`
    // does. Only the reads stay non-blocking. A write that blocks for good is the whole
    // command's `TOTAL_WAIT` to deal with, like any other stage.
    set_nonblocking(&f, false);
    let sent = f.write_all(format!("{text}\r").as_bytes());
    set_nonblocking(&f, true);
    sent.map_err(|e| format!("write {port}: {e}"))?;
    let start = Instant::now();
    let mut answer = Vec::new();
    // one read per message: the smd port hands a reply over whole and drops what does not fit
    // (10-13: a 549-byte +CGCONTRDP read into 512 bytes lost its tail and the OK)
    let mut buf = [0u8; READ_BUF];
    while start.elapsed() < wait {
        match f.read(&mut buf) {
            Ok(n) if n > 0 => {
                answer.extend_from_slice(&buf[..n]);
                let s = String::from_utf8_lossy(&answer);
                if complete(&s, until) {
                    return Ok((s.into_owned(), true));
                }
                if answer.len() >= ANSWER_MAX {
                    break;
                }
            }
            Ok(_) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50))
            }
            Err(e) => return Err(format!("read {port}: {e}")),
        }
    }
    Ok((String::from_utf8_lossy(&answer).into_owned(), false))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::ffi::CStr;

    #[test]
    fn changed_context_is_checked_after_lock_before_port_access() {
        let e = send_blocking(
            Job::fixed(Cmd::Cgcontrdp),
            None,
            Some("/does-not-exist/at-port".into()),
            &|| {},
            &|| Err("synthetic context changed".into()),
        )
        .err()
        .unwrap();
        assert_eq!(e, "synthetic context changed");
    }

    /// A pseudo-terminal standing in for the modem: answers `answer` to every
    /// command line it reads; returns the slave path and what it was sent.
    fn fake_modem(answer: &'static str) -> (String, std::sync::mpsc::Receiver<String>) {
        fake_modem_after(answer, Duration::ZERO)
    }

    /// `fake_modem` that takes `delay` to answer each line (a slow COPS=0).
    pub(crate) fn fake_modem_after(
        answer: &'static str,
        delay: Duration,
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        // SAFETY: plain libc pty calls on a fresh descriptor.
        unsafe {
            let m = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
            assert!(m >= 0);
            assert_eq!(libc::grantpt(m), 0);
            assert_eq!(libc::unlockpt(m), 0);
            let mut buf = [0 as libc::c_char; 128];
            // ptsname_r: tests run in parallel and ptsname's buffer is shared
            assert_eq!(libc::ptsname_r(m, buf.as_mut_ptr(), buf.len()), 0);
            let name = CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned();
            // raw slave: no echo, no line editing, like the modem's port
            let s = libc::open(
                std::ffi::CString::new(name.clone()).unwrap().as_ptr(),
                libc::O_RDWR | libc::O_NOCTTY,
            );
            let mut t: libc::termios = std::mem::zeroed();
            libc::tcgetattr(s, &mut t);
            libc::cfmakeraw(&mut t);
            libc::tcsetattr(s, libc::TCSANOW, &t);
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _keep_slave_open = s;
                let mut line = Vec::new();
                let mut b = [0u8; 1];
                while libc::read(m, b.as_mut_ptr().cast(), 1) == 1 {
                    if b[0] == b'\r' {
                        let _ = tx.send(String::from_utf8_lossy(&line).into_owned());
                        line.clear();
                        std::thread::sleep(delay);
                        libc::write(m, answer.as_ptr().cast(), answer.len());
                    } else {
                        line.push(b[0]);
                    }
                }
            });
            (name, rx)
        }
    }

    /// `fake_modem` that answers each line in parts: after each delay, its text (`OK` at once,
    /// a `+CUSD:` seconds later, as the modem does).
    pub(crate) fn fake_modem_parts(
        parts: &'static [(u64, &'static str)],
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        // SAFETY: plain libc pty calls on a fresh descriptor (as `fake_modem_after`).
        unsafe {
            let m = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
            assert!(m >= 0);
            assert_eq!(libc::grantpt(m), 0);
            assert_eq!(libc::unlockpt(m), 0);
            let mut buf = [0 as libc::c_char; 128];
            assert_eq!(libc::ptsname_r(m, buf.as_mut_ptr(), buf.len()), 0);
            let name = CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned();
            let s = libc::open(
                std::ffi::CString::new(name.clone()).unwrap().as_ptr(),
                libc::O_RDWR | libc::O_NOCTTY,
            );
            let mut t: libc::termios = std::mem::zeroed();
            libc::tcgetattr(s, &mut t);
            libc::cfmakeraw(&mut t);
            libc::tcsetattr(s, libc::TCSANOW, &t);
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _keep_slave_open = s;
                let mut line = Vec::new();
                let mut b = [0u8; 1];
                while libc::read(m, b.as_mut_ptr().cast(), 1) == 1 {
                    if b[0] == b'\r' {
                        let _ = tx.send(String::from_utf8_lossy(&line).into_owned());
                        line.clear();
                        for (ms, text) in parts {
                            std::thread::sleep(Duration::from_millis(*ms));
                            libc::write(m, text.as_ptr().cast(), text.len());
                        }
                    } else {
                        line.push(b[0]);
                    }
                }
            });
            (name, rx)
        }
    }

    fn target(port: &str) -> Target {
        Target {
            lock: None,
            port: Some(port.into()),
        }
    }

    #[tokio::test]
    async fn a_ussd_answer_after_the_ok_is_waited_for() {
        let (port, sent) =
            fake_modem_parts(&[(0, "\r\nOK\r\n"), (400, "\r\n+CUSD: 1,\"0042\",72\r\n")]);
        let t = Instant::now();
        let a = phone(target(&port), Phone::ussd("*1x0#; rm"))
            .await
            .unwrap();
        assert!(a.contains("+CUSD: 1,\"0042\",72"), "{a:?}");
        assert!(
            t.elapsed() < Duration::from_secs(3),
            "stops at the +CUSD line"
        );
        assert_eq!(sent.recv().unwrap(), "AT+CUSD=1,\"*10#\",15");
    }

    #[tokio::test]
    async fn the_terminal_gets_errors_and_silence_as_answers() {
        let (port, sent) = fake_modem_parts(&[(0, "\r\n+CME ERROR: 4\r\n")]);
        let a = raw(target(&port), "AT+FOO", Duration::from_secs(2))
            .await
            .unwrap();
        assert!(a.text.contains("ERROR") && a.port == port, "{a:?}");
        assert_eq!(sent.recv().unwrap(), "AT+FOO");
        let (port, _) = fake_modem_parts(&[]);
        let t = Instant::now();
        let a = raw(target(&port), "AT", Duration::from_millis(400))
            .await
            .unwrap();
        assert_eq!(a.text, "");
        assert!(t.elapsed() >= Duration::from_millis(350));
        // one command per call: no line breaks into a second one
        for bad in ["", "AT\rAT+CFUN=0", "AT\n", "AT\u{1b}"] {
            assert!(
                raw(target(&port), bad, Duration::from_secs(1))
                    .await
                    .is_err(),
                "{bad:?}"
            );
        }
    }

    #[tokio::test]
    async fn stk_reads_the_whole_wait_and_the_probe_names_the_port() {
        assert_eq!(
            Phone::Cusate(3).job().text.unwrap(),
            "AT+CUSATE=\"D3078202018190010103\""
        );
        assert_eq!(Phone::Cusatd.job().until, Until::Wait);
        assert_eq!(probe(target("/dev/nope")).await.unwrap(), "/dev/nope");
        let (port, _) = fake_modem_parts(&[(0, "\r\nOK\r\n"), (200, "\r\n+CUSATP: D00A\r\n")]);
        let job = Job::loose(
            "AT+CUSATD=1".into(),
            Duration::from_millis(700),
            Until::Wait,
        );
        let a = run(job, None, Some(port), Duration::from_secs(3))
            .await
            .unwrap();
        assert!(
            a.text.contains("OK") && a.text.contains("+CUSATP: D00A"),
            "{a:?}"
        );
    }

    #[test]
    fn sends_only_the_whitelisted_text_and_reads_the_answer() {
        let (port, sent) = fake_modem("\r\nOK\r\n");
        let a = exchange(&port, &Cmd::CopsAuto.text(), Duration::from_secs(2)).unwrap();
        assert!(a.contains("OK"));
        assert_eq!(
            sent.recv_timeout(Duration::from_secs(1)).unwrap(),
            "AT+COPS=0"
        );
        let t = Instant::now();
        exchange(&port, &Cmd::CfunOnline.text(), Duration::from_secs(2)).unwrap();
        assert!(t.elapsed() < Duration::from_secs(1), "stops at the answer");
        assert_eq!(
            sent.recv_timeout(Duration::from_secs(1)).unwrap(),
            "AT+CFUN=1"
        );
    }

    #[test]
    fn error_and_silence_fail() {
        let (port, _) = fake_modem("\r\n+CME ERROR: 3\r\n");
        let a = exchange(&port, "AT+COPS=0", Duration::from_secs(1)).unwrap();
        assert!(a.contains("ERROR"), "{a:?}");
        let (port, _) = fake_modem("");
        let e = exchange(&port, "AT+COPS=0", Duration::from_millis(300)).unwrap_err();
        assert!(e.contains("no answer"), "{e}");
    }

    #[test]
    fn final_result_codes_are_whole_lines() {
        assert_eq!(final_code("\r\n+CSQ: 20,99\r\n\r\nOK\r\n"), Some(Final::Ok));
        assert_eq!(final_code("OK"), Some(Final::Ok));
        assert_eq!(final_code("\r\nERROR\r\n"), Some(Final::Error));
        assert_eq!(final_code("\r\n+CME ERROR: 10\r\n"), Some(Final::Error));
        assert_eq!(final_code("\r\n+CMS ERROR: 500\r\n"), Some(Final::Error));
        // the number may still be on the way
        assert_eq!(final_code("\r\n+CME ERROR: 1"), None);
        // a word inside other text is not a result code
        assert_eq!(final_code("\r\n+CUSD: 0,\"NO ERROR, OK\",15\r\n"), None);
        assert_eq!(final_code("\r\n+COPS: 0,0,\"OKTEL\"\r\n"), None);
        assert_eq!(final_code("AT\r"), None);
        assert!(!complete(
            "\r\n+CUSD: 0,\"ERROR in text",
            Until::Line("+CUSD:")
        ));
        assert!(complete("\r\n+CME ERROR: 4\r\n", Until::Line("+CUSD:")));
        assert!(!complete("\r\nOK\r\n", Until::Line("+CUSD:")));
        assert!(complete(
            "\r\nOK\r\n\r\n+CME ERROR: 30\r\n",
            Until::Line("+CUSD:")
        ));
    }

    #[test]
    fn errors_say_their_kind() {
        let k = |e: &str| error_kind(e).name();
        assert_eq!(k("AT port busy (another command is running)"), "busy");
        assert_eq!(
            k("every AT port is held by another process: /dev/at_mdm0 (port-bridge pid 9)"),
            "held"
        );
        assert_eq!(k("no AT port found"), "no-port");
        assert_eq!(k("AT+COPS=0: no answer"), "timeout");
        assert_eq!(
            k("AT+COPS=0: no answer within 25 s (the AT port is stuck)"),
            "timeout"
        );
        assert_eq!(
            k("AT+COPS=0: the AT port is still stuck on an earlier command"),
            "timeout"
        );
        assert_eq!(k("AT+COPS=0: +CME ERROR: 3"), "error");
        assert_eq!(
            k("open /dev/at_mdm2: Permission denied (os error 13)"),
            "error"
        );
    }

    #[test]
    fn held_and_missing_nodes_are_skipped_in_any_order() {
        let (free, sent) = fake_modem("\r\nOK\r\n");
        let (held, held_sent) = fake_modem("\r\nOK\r\n");
        let held_p = held.clone();
        let is_held =
            move |p: &Path| (p == Path::new(&held_p)).then(|| "port-bridge pid 1".to_string());
        let gone = "/dev/at_mdm2-not-here";
        // at_mdm2 missing, the next held: the free one is found, the held one never written to
        let got = find_port_in(&[gone, &held, &free], &is_held, &|| ()).unwrap();
        assert_eq!(got, free);
        assert_eq!(sent.recv_timeout(Duration::from_secs(1)).unwrap(), "AT");
        assert!(held_sent.recv_timeout(Duration::from_millis(200)).is_err());
        let got = find_port_in(&[&free, &held], &is_held, &|| ()).unwrap();
        assert_eq!(got, free);
        // every existing one held: said so, with who holds it
        let e = find_port_in(&[gone, &held], &is_held, &|| ()).unwrap_err();
        assert_eq!(error_kind(&e), ErrorKind::Held, "{e}");
        assert!(e.contains("port-bridge"), "{e}");
        assert!(held_sent.recv_timeout(Duration::from_millis(200)).is_err());
        // none exists / none answers: no port
        let e = find_port_in(&[gone], &is_held, &|| ()).unwrap_err();
        assert_eq!(error_kind(&e), ErrorKind::NoPort);
        let (mute, _) = fake_modem("");
        let e = find_port_in(&[&mute], &|_| None, &|| ()).unwrap_err();
        assert_eq!(error_kind(&e), ErrorKind::NoPort);
    }

    #[test]
    fn a_node_open_in_another_process_is_seen_in_proc() {
        if !Path::new("/proc/self/fd").exists() {
            return; // not Linux
        }
        // this process holds the fake modem's slave itself: that does not count
        let (port, _) = fake_modem("\r\nOK\r\n");
        assert_eq!(held_by(Path::new("/proc"), Path::new(&port)), None);
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .stdin(File::open(&port).unwrap())
            .spawn()
            .unwrap();
        let who = held_by(Path::new("/proc"), Path::new(&port));
        child.kill().unwrap();
        child.wait().unwrap();
        let who = who.expect("held by the child");
        assert!(who.starts_with("sleep pid "), "{who}");
        // not a character device: never held
        assert_eq!(
            held_by(Path::new("/proc"), Path::new("/proc/self/stat")),
            None
        );
    }

    #[test]
    fn the_lock_is_shared_with_the_agent() {
        let dir = std::env::temp_dir().join(format!("datad-at-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("at.lock");
        let held = File::create(&path).unwrap();
        held.lock_exclusive().unwrap();
        // SAFETY: tests in this module do not read the variable concurrently with the set
        unsafe { std::env::set_var("LUYAZ_AT_LOCK", &path) };
        let t = Instant::now();
        let r = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            FileExt::unlock(&held).unwrap();
        });
        let got = take_lock(lock_path()).unwrap();
        assert!(got.is_some() && t.elapsed() >= Duration::from_millis(250));
        r.join().unwrap();
    }

    #[tokio::test]
    async fn a_stuck_stage_is_left_behind_and_blocks_later_commands_until_it_ends() {
        let dir = std::env::temp_dir().join(format!("datad-at-stuck-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lock = dir.join("at.lock").to_string_lossy().into_owned();
        let held = File::create(&lock).unwrap();
        held.lock_exclusive().unwrap();
        let (port, _) = fake_modem("\r\nOK\r\n");
        let total = Duration::from_millis(300);
        let t = Instant::now();
        let e = send_within(Cmd::CopsAuto, Some(lock.clone()), Some(port.clone()), total)
            .await
            .unwrap_err();
        assert!(
            e.contains("stuck") && t.elapsed() < Duration::from_secs(2),
            "{e}"
        );
        let e = send_within(Cmd::CopsAuto, Some(lock.clone()), Some(port.clone()), total)
            .await
            .unwrap_err();
        assert!(e.contains("still stuck"), "{e}");
        // the stage ends: the left-behind thread finishes and frees the key
        FileExt::unlock(&held).unwrap();
        let t = Instant::now();
        loop {
            match send_within(Cmd::CopsAuto, Some(lock.clone()), Some(port.clone()), total).await {
                Ok(a) => break assert!(a.contains("OK")),
                Err(e) if t.elapsed() < Duration::from_secs(5) => {
                    assert!(e.contains("stuck"), "{e}");
                    tokio::time::sleep(Duration::from_millis(50)).await
                }
                Err(e) => panic!("{e}"),
            }
        }
    }
}
