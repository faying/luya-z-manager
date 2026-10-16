//! The home exit's reads for the touch screen (Phase 4 step 8b), rendered by u60-api as
//! the agent renders it. The machine: on/off, rest and the notice from the agent's file (requests
//! change them without a record), the probe results from u60d's replay of the agent's records.
//! `busy` is null and `ready` always true until the requests move here (8e); while the agent runs
//! a request the two replies differ for those seconds.
//!

use serde_json::Value;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub(super) const TS_BIN: &str = "/data/tailscale/tailscale";
pub(super) const TS_SOCKET: &str = "/tmp/tailscaled.sock";
pub(super) const TS_TIMEOUT: Duration = Duration::from_secs(5);

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}


/// The last `tailscale status` answer's path to home, refreshed in the background at most once a
/// minute and only when GET asks (the agent's `ts_now`): never on a timer, never waited for.
#[derive(Default)]
pub struct TsCache {
    /// (when, path)
    last: Mutex<Option<(Instant, Option<&'static str>)>>,
    refreshing: std::sync::atomic::AtomicBool,
}


/// `tailscale status --json` (the agent's `services::tailscale_json`).
pub fn tailscale_status() -> Option<Value> {
    let bin = crate::state::host_path(TS_BIN);
    let sock = crate::state::host_path(TS_SOCKET);
    if !std::path::Path::new(&sock).exists() {
        return None;
    }
    let out = ts_exec(
        std::path::Path::new(&bin),
        std::path::Path::new(&sock),
        TS_TIMEOUT,
    )
    .ok()?;
    serde_json::from_slice(&out).ok()
}

/// `<bin> --socket <sock> status --json` within `limit` (blocking: run off the async threads).
/// Ok(stdout) on success; Err(stderr, trimmed) when it fails, "exec failed: …" when it cannot
/// start or does not answer in time (then it is killed).
pub(super) fn ts_exec(
    bin: &std::path::Path,
    sock: &std::path::Path,
    limit: Duration,
) -> Result<Vec<u8>, String> {
    if !bin.exists() {
        return Err("exec failed: not installed".into());
    }
    let mut child = std::process::Command::new(bin)
        .arg("--socket")
        .arg(sock)
        .args(["status", "--json"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("exec failed: {e}"))?;
    let drain = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        })
    };
    let out = drain(child.stdout.take().map(|p| Box::new(p) as _));
    let err = drain(child.stderr.take().map(|p| Box::new(p) as _));
    let t0 = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if t0.elapsed() < limit => std::thread::sleep(Duration::from_millis(50)),
            Ok(None) => {
                // the readers end when the pipes close; not waited for
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("exec failed: timed out after {}s", limit.as_secs()));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("exec failed: {e}"));
            }
        }
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    if status.success() {
        Ok(stdout)
    } else {
        Err(String::from_utf8_lossy(&stderr).trim().to_string())
    }
}

