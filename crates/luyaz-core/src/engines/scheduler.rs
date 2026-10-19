//! Scheduled jobs (8d-1d; the agent's until then).
//!
//! With u60d's writes on:
//! - luyaz-web's routes (`/api/scheduler/jobs`, `…/toggle`) edit the agent's file
//!   (`/data/local/tmp/scheduler.json`, same shape);
//! - a thread of its own looks every [`TICK_SECS`] which jobs are due (`luyaz_policy::scheduler::due`)
//!   and runs each one's method + path against u60d's own agent-shaped routes (`/v2/api/...`,
//!   source `scheduler`), as the scenario engine does. A path u60d does not serve is recorded as
//!   not supported; the forbidden list (eSIM, factory reset, power off, AT, kill-bloat) is checked
//!   again before every run.
//!
//! The run mark is saved **before** the call: a once job that reboots the device (or a u60d that
//! dies mid-call) is not run again after the restart. Its status says the result never came back
//! until the call returns and fills it in.

use luyaz_api::clock::BootTime;
use luyaz_policy::scheduler::{self as sp, Job};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::api::Compat;

pub const JOBS_FILE: &str = "/data/local/tmp/scheduler.json";
pub const CLOCK_OK: &str = "/tmp/luyaz/clock-ok";
const TICK_SECS: u64 = 30;
/// How often the thread looks whether a tick is due.
const LOOK_SECS: u64 = 2;
/// One job's call (a reboot answers at once; a Wi-Fi change waits for its lock and readback).
const CALL_SECS: u64 = 240;
/// What a run says until its call has come back.
pub const NO_RESULT: &str = "已开始执行，没等到结果（设备可能重启了，或 u60d 中途退出）";

/// One writer of the file inside u60d (the routes and the thread).
pub(crate) static FILE_LOCK: Mutex<()> = Mutex::new(());

pub(crate) fn file(c: &Compat) -> PathBuf {
    c.path(JOBS_FILE)
}

/// The jobs; none when the file is missing. An unreadable file is an error (never overwritten).
pub(crate) fn load(p: &std::path::Path) -> Result<Vec<Job>, String> {
    match std::fs::read_to_string(p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.to_string()),
        Ok(t) => parse(&t),
    }
}

/// `SchedulerData` in the agent.
fn parse(text: &str) -> Result<Vec<Job>, String> {
    #[derive(serde::Deserialize)]
    struct Data {
        jobs: Vec<Job>,
    }
    serde_json::from_str::<Data>(text)
        .map(|d| d.jobs)
        .map_err(|e| e.to_string().chars().take(120).collect())
}

/// The device clock and its local time: (unix seconds, hour, minute, weekday 0 = Monday).
pub(crate) fn now_local() -> (i64, u8, u8, u8) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (hh, mm, dow) = luyaz_api::clock::local_hm_dow(
        secs,
        luyaz_api::alerts::current_offset(),
        luyaz_api::clock::system_gmtoff(),
    )
    .unwrap_or((0, 0, 0));
    (secs, hh, mm, dow)
}

/// Whether the wall clock is usable for firing calendar jobs: requires `now_ts >= CLOCK_YEAR_MIN`
/// and a fresh, unrevoked current-boot `ClockEvidence` v2 record from `luyaz-sentinel`.
pub(crate) fn clock_usable(evidence: &str, boot: Option<&BootTime>, now_ts: i64) -> bool {
    now_ts >= luyaz_api::clock::CLOCK_YEAR_MIN
        && luyaz_api::clock::evidence_status(evidence, boot, now_ts).wall_usable()
}

pub(crate) fn save(p: &std::path::Path, jobs: &[Job]) -> Result<(), String> {
    let text = serde_json::to_string_pretty(&json!({"jobs": jobs})).map_err(|e| e.to_string())?;
    crate::api::ops::atomic_write(p, text.as_bytes()).map_err(|e| e.to_string())
}

// ── running them ────────────────────────────────────────────────────────────

/// How a job's call reaches u60d (the device's routes; a fake in tests).
pub trait Caller: Send + Sync {
    /// `(status, reply)`; 502 when nothing answered.
    fn call(&self, method: &str, path: &str, body: &[u8]) -> (u16, Value);
}

/// u60d's own agent-shaped routes at `base` (`http://127.0.0.1:<port>/v2`), as source `scheduler`.
pub struct Http {
    pub base: String,
    pub token: Option<String>,
}

impl Caller for Http {
    fn call(&self, method: &str, path: &str, body: &[u8]) -> (u16, Value) {
        let a: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(CALL_SECS)))
            .http_status_as_error(false)
            .build()
            .into();
        let url = format!("{}{path}", self.base);
        let auth = self.token.as_ref().map(|t| format!("Bearer {t}"));
        let r = if method == "GET" {
            let mut q = a
                .get(&url)
                .header(luyaz_api::headers::SOURCE_HEADER, "scheduler");
            if let Some(h) = &auth {
                q = q.header("Authorization", h);
            }
            q.call()
        } else {
            let q = match method {
                "PUT" => a.put(&url),
                "DELETE" => a.delete(&url).force_send_body(),
                _ => a.post(&url),
            };
            let mut q = q
                .header("Content-Type", "application/json")
                .header(luyaz_api::headers::SOURCE_HEADER, "scheduler");
            if let Some(h) = &auth {
                q = q.header("Authorization", h);
            }
            q.send(body)
        };
        match r {
            Ok(mut resp) => (
                resp.status().as_u16(),
                resp.body_mut().read_json().unwrap_or(Value::Null),
            ),
            Err(e) => (
                502,
                json!({"ok": false, "error": format!("luyaz-core: {e}")}),
            ),
        }
    }
}

/// A call's outcome as the job records it: `(status, error)`.
fn outcome(status: u16, v: &Value) -> (Option<u16>, Option<String>) {
    if status < 400 {
        return (Some(status), None);
    }
    let said = v.get("error").and_then(Value::as_str).unwrap_or_default();
    let error = if (status == 404 || status == 501) && said == "not_in_compat_yet" {
        "不支持：u60d 没有这个路径".to_string()
    } else if said.is_empty() {
        format!("HTTP {status}")
    } else {
        said.to_string()
    };
    (Some(status), Some(error))
}

/// One tick at the device's `(now_ts, hh, mm, dow)`: marks what is due, saves, runs each, then
/// fills in the results. Returns how many ran.
#[cfg(test)]
pub fn tick(path: &std::path::Path, io: &dyn Caller, now: (i64, u8, u8, u8)) -> usize {
    let mut cooldown = HashMap::new();
    tick_at(
        path,
        io,
        now,
        now.0 >= luyaz_api::clock::CLOCK_YEAR_MIN,
        None,
        &mut cooldown,
    )
}

/// Boot- and evidence-gated tick: always disables forbidden jobs, but only fires due jobs when
/// `clock_ok` holds and at least 60 s of `BootTime` have elapsed since the same `(id, restore)`
/// last fired on this boot.
pub(crate) fn tick_at(
    path: &std::path::Path,
    io: &dyn Caller,
    now: (i64, u8, u8, u8),
    clock_ok: bool,
    boot_now: Option<&BootTime>,
    cooldown: &mut HashMap<(u32, bool), BootTime>,
) -> usize {
    let (now_ts, hh, mm, dow) = now;
    // 1: what is due, marked as run (and saved) before anything runs
    let due: Vec<(u32, bool, String, String, Vec<u8>)> = {
        let _g = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let Ok(mut jobs) = load(path) else {
            return 0;
        };
        let stopped = sp::disable_forbidden(&mut jobs);
        let fires = if clock_ok {
            sp::due(&jobs, now_ts, hh, mm, dow)
        } else {
            Vec::new()
        };
        let fires: Vec<(u32, bool)> = fires
            .into_iter()
            .filter(|&(id, restore)| {
                let Some(now_b) = boot_now else {
                    return true;
                };
                cooldown
                    .get(&(id, restore))
                    .and_then(|prev| now_b.elapsed_since(prev))
                    .is_none_or(|age| age > 60)
            })
            .collect();
        if fires.is_empty() {
            if !stopped.is_empty() {
                let _ = save(path, &jobs);
            }
            return 0;
        }
        let mut out = Vec::new();
        for (id, restore) in fires {
            let Some(job) = jobs.iter_mut().find(|j| j.id == id) else {
                continue;
            };
            let body = if restore {
                job.restore.as_ref().and_then(|r| r.body.clone())
            } else {
                job.action.body.clone()
            };
            let body = body
                .map(|b| serde_json::to_vec(&b).unwrap_or_default())
                .unwrap_or_default();
            out.push((
                id,
                restore,
                job.action.method.clone(),
                job.action.path.clone(),
                body,
            ));
            sp::record(job, restore, None, Some(NO_RESULT.to_string()), now_ts);
            if let Some(now_b) = boot_now.filter(|b| !b.boot_id.is_empty()) {
                cooldown.insert((id, restore), now_b.clone());
            }
        }
        if save(path, &jobs).is_err() {
            // not marked: running now could run it again after a restart, so not at all
            return 0;
        }
        out
    };
    // 2: the calls, no lock held (a job may call these routes)
    let results: Vec<(u32, bool, Option<u16>, Option<String>)> = due
        .iter()
        .map(|(id, restore, method, path_, body)| {
            let (status, error) = match sp::forbidden(path_) {
                Some(what) => (Some(403), Some(sp::forbidden_msg(what).0)),
                None if !sp::is_method(method) => (None, Some(format!("不认识的方法 {method}"))),
                None => {
                    let (s, v) = io.call(method, path_, body);
                    outcome(s, &v)
                }
            };
            (*id, *restore, status, error)
        })
        .collect();
    // 3: the results, into the file as it is now (a route may have changed it meanwhile)
    let _g = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Ok(mut jobs) = load(path) {
        for (id, restore, status, error) in &results {
            if let Some(job) = jobs.iter_mut().find(|j| j.id == *id) {
                if *restore {
                    job.last_restore_status = *status;
                    job.last_restore_error = error.clone();
                } else {
                    job.last_status = *status;
                    job.last_error = error.clone();
                }
            }
        }
        let _ = save(path, &jobs);
    }
    results.len()
}

/// The thread: while u60d owns the jobs, a tick every [`TICK_SECS`] (blocking calls, so never on
/// the runtime's thread).
pub fn spawn(c: Compat, io: std::sync::Arc<dyn Caller>) {
    let _ = std::thread::Builder::new()
        .name("u60d-scheduler".into())
        .spawn(move || {
            let mut last: Option<std::time::Instant> = None;
            let mut cooldown: HashMap<(u32, bool), BootTime> = HashMap::new();
            let clock_path = c.path(CLOCK_OK);
            loop {
                std::thread::sleep(Duration::from_secs(LOOK_SECS));
                if last.is_some_and(|t| t.elapsed() < Duration::from_secs(TICK_SECS)) {
                    continue;
                }
                last = Some(std::time::Instant::now());
                let now = now_local();
                let boot = BootTime::read();
                let evidence = std::fs::read_to_string(&clock_path).unwrap_or_default();
                let clock_ok = clock_usable(&evidence, boot.as_ref(), now.0);
                let ran = tick_at(
                    &file(&c),
                    io.as_ref(),
                    now,
                    clock_ok,
                    boot.as_ref(),
                    &mut cooldown,
                );
                if ran > 0 {
                    eprintln!("[scheduler] ran {ran} job(s)");
                }
            }
        });
}
