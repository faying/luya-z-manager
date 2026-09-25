// ─────────────────────────────────────────────────────────────────────────────
// health — the device check, for the admin web's health page and the touch
// screen's summary.
//
// The checks themselves live in one shell script, touch-ui scripts/doctor.sh
// (installed at /data/u60-guard/doctor.sh), so that `install.sh doctor` over
// SSH and this page can never disagree — and so the check still works when
// this agent is the thing that is broken. Here we only run it on a timer,
// cache the result, and list the crash logs supervise.sh / u60-uid keep.
//
// Unauthenticated `/api/public/status` gets counts only; the details (and the
// crash logs, which contain program output) need a login.
// ─────────────────────────────────────────────────────────────────────────────

use std::fs;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::handlers::AppState;

const DOCTOR: &str = "/data/u60-guard/doctor.sh";
const CRASH_DIR: &str = "/data/crashlog";
const EVERY: Duration = Duration::from_secs(60);
const DOCTOR_TIMEOUT: Duration = Duration::from_secs(20);
const CRASHLOG_MAX: u64 = 256 * 1024;

#[derive(Clone, Debug, PartialEq)]
struct Check {
    level: String, // ok | warn | bad
    id: String,
    label: String,
    detail: String,
}

struct Snapshot {
    checks: Vec<Check>,
    at_device: i64,      // device clock (local wall time labelled UTC; see clock.rs)
    at: Option<Instant>, // None = never ran
    error: Option<String>,
}

static LAST: Mutex<Snapshot> = Mutex::new(Snapshot { checks: Vec::new(), at_device: 0, at: None, error: None });

fn parse(tsv: &str) -> Vec<Check> {
    tsv.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.splitn(4, '\t').collect();
            if f.len() != 4 || !matches!(f[0], "ok" | "warn" | "bad") {
                return None;
            }
            Some(Check { level: f[0].into(), id: f[1].into(), label: f[2].into(), detail: f[3].into() })
        })
        .collect()
}

/// Run doctor.sh with a deadline. Its exit status is "no bad checks", not
/// success/failure of the run itself, so only the output matters.
fn run_doctor() -> Result<String, String> {
    if !std::path::Path::new(DOCTOR).exists() {
        return Err(format!("{DOCTOR} is not installed"));
    }
    let mut child = Command::new("sh")
        .args([DOCTOR, "--tsv"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("run doctor: {e}"))?;
    let t0 = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if t0.elapsed() < DOCTOR_TIMEOUT => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("doctor timed out".into());
            }
        }
    }
    let mut out = String::new();
    if let Some(mut s) = child.stdout.take() {
        let _ = s.read_to_string(&mut out);
    }
    Ok(out)
}

fn now_device() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn refresh() {
    let result = run_doctor();
    let mut s = LAST.lock().unwrap_or_else(|e| e.into_inner());
    match result {
        Ok(out) => {
            s.checks = parse(&out);
            s.error = None;
        }
        Err(e) => s.error = Some(e),
    }
    s.at = Some(Instant::now());
    s.at_device = now_device();
}

/// Re-run the check now, in the background, after something that changes its
/// answer (alerts marked read, SMS number set). Without this the touch
/// screen's 健康 row kept saying 注意 for up to a minute after 全部已读.
/// One extra run at a time; a second request while one is running is dropped.
pub fn refresh_soon() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static BUSY: AtomicBool = AtomicBool::new(false);
    if BUSY.swap(true, Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("health-now".into())
        .spawn(|| {
            refresh();
            BUSY.store(false, Ordering::Release);
        });
    if spawned.is_err() {
        BUSY.store(false, Ordering::Release);
    }
}

/// Background refresher. The first run waits a little so boot is not slowed.
pub fn start() {
    std::thread::Builder::new()
        .name("health".into())
        .spawn(|| {
            std::thread::sleep(Duration::from_secs(20));
            loop {
                refresh();
                std::thread::sleep(EVERY);
            }
        })
        .ok();
}

fn counts(checks: &[Check]) -> (usize, usize) {
    let bad = checks.iter().filter(|c| c.level == "bad").count();
    let warn = checks.iter().filter(|c| c.level == "warn").count();
    (bad, warn)
}

/// For `/api/public/status`: counts only.
pub fn public_summary() -> Value {
    let s = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let (bad, warn) = counts(&s.checks);
    json!({ "checked": s.at.is_some() && s.error.is_none(), "bad": bad, "warn": warn })
}

/// Name safe to use as one path component: no separators, no dot-dot.
fn safe_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 96
        && !n.starts_with('.')
        && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

fn crash_logs() -> Vec<Value> {
    let mut out = Vec::new();
    let Ok(progs) = fs::read_dir(CRASH_DIR) else { return out };
    for p in progs.flatten() {
        let prog = p.file_name().to_string_lossy().to_string();
        if !safe_name(&prog) {
            continue;
        }
        let Ok(files) = fs::read_dir(p.path()) else { continue };
        for f in files.flatten() {
            let name = f.file_name().to_string_lossy().to_string();
            if !name.ends_with(".log") || !safe_name(&name) {
                continue;
            }
            let meta = f.metadata().ok();
            let mtime = meta
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            // The status line, so the list says what happened without opening it.
            let status = fs::read_to_string(f.path())
                .ok()
                .and_then(|t| t.lines().find_map(|l| l.strip_prefix("status:").map(|s| s.trim().to_string())))
                .unwrap_or_default();
            out.push(json!({
                "program": prog, "file": name, "time": mtime,
                "size": meta.map(|m| m.len()).unwrap_or(0), "status": status,
            }));
        }
    }
    out.sort_by(|a, b| b["time"].as_i64().cmp(&a["time"].as_i64()));
    out
}

/// GET /api/health[?refresh=1]
pub fn health_get(_state: &AppState, query: &str) -> (u16, Value) {
    if query.split('&').any(|kv| kv == "refresh=1") {
        refresh();
    }
    let s = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let (bad, warn) = counts(&s.checks);
    let checks: Vec<Value> = s
        .checks
        .iter()
        .map(|c| json!({"level": c.level, "id": c.id, "label": c.label, "detail": c.detail}))
        .collect();
    (
        200,
        json!({"ok": true, "data": {
            "checks": checks,
            "bad": bad,
            "warn": warn,
            "checked_at": if s.at.is_some() { Some(s.at_device) } else { None },
            "error": s.error,
            "crashlogs": crash_logs(),
        }}),
    )
}

/// GET /api/health/crashlog?program=<p>&file=<f>
pub fn crashlog_get(_state: &AppState, query: &str) -> (u16, Value) {
    let get = |k: &str| {
        query.split('&').find_map(|kv| kv.strip_prefix(k).and_then(|v| v.strip_prefix('=')).map(str::to_string))
    };
    let (Some(prog), Some(file)) = (get("program"), get("file")) else {
        return (400, json!({"ok": false, "error": "program and file are required"}));
    };
    if !safe_name(&prog) || !safe_name(&file) || !file.ends_with(".log") {
        return (400, json!({"ok": false, "error": "bad name"}));
    }
    let path = format!("{CRASH_DIR}/{prog}/{file}");
    let Ok(f) = fs::File::open(&path) else {
        return (404, json!({"ok": false, "error": "no such crash log"}));
    };
    let mut text = String::new();
    let _ = f.take(CRASHLOG_MAX).read_to_string(&mut text);
    (200, json!({"ok": true, "data": {"program": prog, "file": file, "text": text}}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_doctor_tsv_and_skips_junk() {
        let c = parse("ok\tfota\tZTE 自动升级\t已关闭\nbad\tscreen\t触屏界面\t没在运行\nnonsense\nwarn\tx\ty\tdetail\twith tab\n");
        assert_eq!(c.len(), 3);
        assert_eq!(c[1].level, "bad");
        assert_eq!(c[2].detail, "detail\twith tab");
        assert_eq!(counts(&c), (1, 1));
    }

    #[test]
    fn names_cannot_escape_the_crash_dir() {
        assert!(safe_name("zte-agent"));
        assert!(safe_name("20260923-150424-up2325.log"));
        assert!(!safe_name(".."));
        assert!(!safe_name("../etc"));
        assert!(!safe_name("a/b"));
        assert!(!safe_name(".hidden"));
        assert!(!safe_name(""));
    }
}
