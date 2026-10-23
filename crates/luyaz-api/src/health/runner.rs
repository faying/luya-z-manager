//! Running the doctor : whoever owns the
//! `health` part keeps a [`Runner`] — the agent until it hands the part over, then u60d.
//!
//! The doctor is a script that starts its own children (ubus, wget, …): on a timeout the whole
//! process group goes, not just the `sh` (a hung grandchild used to stay behind). stdout is read
//! while it runs, so a long report can't fill the pipe and stall the doctor until the timeout.

use super::{Check, Snapshot, has_tsv2_marker, parse, parse_tsv2};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

// ---- the script and its schedule ----

/// Where the install puts it; the core maps it to /data/luyaz/scripts once the file is there
/// (Luya-Z rename D1).
pub const DOCTOR: &str = "/data/luyaz/scripts/doctor.sh";
/// The background run's period.
pub const EVERY: Duration = Duration::from_secs(60);
/// The first background run waits this long, so boot (or a takeover) is not slowed.
pub const FIRST_WAIT: Duration = Duration::from_secs(20);
pub const DOCTOR_TIMEOUT: Duration = Duration::from_secs(20);

/// Phase 10 X11: the core's own `doctor --tsv2` (`core` = `/proc/self/exe` in u60d), in its own
/// process group like the script. Anything but a `#tsv2` answer (a spawn error, an exit without
/// output, a core without the subcommand) falls back to the script; a timeout does not (it would
/// cost a second run).
pub fn run_checks_core(core: &str, doctor: &str, limit: Duration) -> Result<Vec<Check>, String> {
    match output_within_group(Command::new(core).args(["doctor", "--tsv2"]), limit) {
        Ok(out) => {
            let out = String::from_utf8_lossy(&out.stdout);
            if has_tsv2_marker(&out) {
                return Ok(parse_tsv2(&out));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => return Err("doctor timed out".into()),
        Err(_) => {}
    }
    run_checks(doctor, limit)
}

/// Run the doctor the new way, falling back to the old one: `--tsv2` first; if its first line is
/// not the marker (an older doctor.sh), run `--tsv` and leave the English empty. A timeout or a
/// missing script is an error either way — never read as "old doctor", so it cannot cost a second
/// 20 s run.
pub fn run_checks(doctor: &str, limit: Duration) -> Result<Vec<Check>, String> {
    let out = run_doctor_within(doctor, "--tsv2", limit)?;
    if has_tsv2_marker(&out) {
        return Ok(parse_tsv2(&out));
    }
    Ok(parse(&run_doctor_within(doctor, "--tsv", limit)?))
}

/// doctor.sh with a deadline. Its exit status is "no bad checks", not success/failure of the run
/// itself, so only the output matters.
pub fn run_doctor_within(doctor: &str, flag: &str, limit: Duration) -> Result<String, String> {
    if !std::path::Path::new(doctor).exists() {
        return Err(format!("{doctor} is not installed"));
    }
    match output_within_group(Command::new("sh").args([doctor, flag]), limit) {
        Ok(out) => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Err("doctor timed out".into()),
        Err(e) => Err(format!("run doctor: {e}")),
    }
}

/// A command in its own process group, killed as a group past `limit` (the agent's
/// `ubus::output_within_group`).
pub fn output_within_group(cmd: &mut Command, limit: Duration) -> std::io::Result<Output> {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().map(drain);
    let stderr = child.stderr.take().map(drain);
    let t0 = Instant::now();
    let mut nap = Duration::from_millis(1);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if t0.elapsed() < limit => {
                thread::sleep(nap);
                nap = (nap * 2).min(Duration::from_millis(20));
            }
            other => {
                // Before wait(): once reaped, the id could be someone else's. Never kill(-0):
                // that is our own group.
                let pid = child.id() as libc::pid_t;
                if pid > 0 {
                    unsafe { libc::kill(-pid, libc::SIGKILL) };
                }
                let _ = child.kill();
                let _ = child.wait();
                // The readers end when the pipes close; not joined, in case something else still
                // holds them.
                return Err(match other {
                    Err(e) => e,
                    _ => std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!("timed out after {} ms", limit.as_millis()),
                    ),
                });
            }
        }
    };
    let join = |h: Option<JoinHandle<Vec<u8>>>| h.and_then(|h| h.join().ok()).unwrap_or_default();
    Ok(Output {
        status,
        stdout: join(stdout),
        stderr: join(stderr),
    })
}

fn drain(mut pipe: impl Read + Send + 'static) -> JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        buf
    })
}

fn now_device() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The last run, kept in memory and written to the snapshot file after each run.
pub struct Runner {
    doctor: String,
    file: PathBuf,
    limit: Duration,
    last: Mutex<Snapshot>,
    /// X11: run this program's `doctor --tsv2` first ([`run_checks_core`])
    core: Option<String>,
    /// a background run is on its way ([`Runner::refresh_soon`])
    busy: AtomicBool,
}

impl Runner {
    /// `file`: where the snapshot goes ([`super::SNAPSHOT_FILE`] on the device).
    pub fn new(doctor: &str, file: impl Into<PathBuf>, limit: Duration) -> Runner {
        Runner {
            core: None,
            doctor: doctor.to_string(),
            file: file.into(),
            limit,
            last: Mutex::new(Snapshot::default()),
            busy: AtomicBool::new(false),
        }
    }

    /// X11: ask `core doctor --tsv2` first, the script only when that gives no answer.
    pub fn with_core(mut self, core: &str) -> Runner {
        self.core = Some(core.to_string());
        self
    }

    /// Start from what the file says (u60d taking over from the agent: its last run, until ours).
    pub fn seed(&self, s: Snapshot) {
        *self.lock() = s;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Snapshot> {
        self.last.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn snapshot(&self) -> Snapshot {
        self.lock().clone()
    }

    /// Run the doctor now (blocks up to the limit, twice for an old doctor), keep and write the
    /// result. A failed run keeps the previous checks and says why.
    pub fn refresh(&self) {
        let result = match &self.core {
            Some(core) => run_checks_core(core, &self.doctor, self.limit),
            None => run_checks(&self.doctor, self.limit),
        };
        let text = {
            let mut s = self.lock();
            match result {
                Ok(checks) => {
                    s.checks = checks;
                    s.error = None;
                }
                Err(e) => s.error = Some(e),
            }
            s.ran = true;
            s.at_device = now_device();
            s.to_json().to_string()
        };
        let tmp = self.file.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &self.file);
        }
    }

    /// Re-run the check now, in the background, after something that changes its answer (alerts
    /// marked read, SMS number set). Without this the touch screen's 健康 row kept saying 注意 for
    /// up to a minute after 全部已读. One extra run at a time; a second request while one is
    /// running is dropped. Returns whether a run was started.
    pub fn refresh_soon(self: &std::sync::Arc<Self>) -> bool {
        if self.busy.swap(true, Ordering::AcqRel) {
            return false;
        }
        let me = self.clone();
        let spawned = thread::Builder::new()
            .name("health-now".into())
            .spawn(move || {
                me.refresh();
                me.busy.store(false, Ordering::Release);
            });
        if spawned.is_err() {
            self.busy.store(false, Ordering::Release);
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests;
