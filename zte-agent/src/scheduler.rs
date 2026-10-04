use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::handlers::AppState;

const STORAGE_PATH: &str = "/data/local/tmp/scheduler.json";
const TICK_SECS: u64 = 30;

#[derive(Serialize, Deserialize, Clone)]
pub struct Job {
    pub id: u32,
    pub name: String,
    pub enabled: bool,
    pub schedule: Schedule,
    pub action: Action,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restore: Option<Restore>,
    pub last_run: Option<i64>,
    pub last_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub last_restore: Option<i64>,
    // Restore used to record only its timestamp — status and error were read
    // from the ExecResult and then dropped, so a restore that failed looked
    // exactly like one that succeeded. Both are Option so jobs persisted by
    // older builds still deserialise.
    #[serde(default)]
    pub last_restore_status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_restore_error: Option<String>,
    pub created_at: i64,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(tag = "type")]
pub enum Schedule {
    #[serde(rename = "once")]
    Once { at: i64 },
    #[serde(rename = "recurring")]
    Recurring { time: String, days: Vec<u8> },
}

// Action moved to crate::action so the scenario engine can share the same
// execution primitive. Re-exported here because the serialised shape in
// scheduler.json is unchanged and callers still refer to scheduler::Action.
pub use crate::action::Action;

#[derive(Serialize, Deserialize, Clone)]
pub struct Restore {
    pub time: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

#[derive(Serialize, Deserialize)]
struct SchedulerData {
    jobs: Vec<Job>,
}

struct PendingAction {
    job_id: u32,
    method: String,
    path: String,
    body: Vec<u8>,
    is_restore: bool,
}

struct ExecResult {
    job_id: u32,
    is_restore: bool,
    /// `None` = the action never ran (unparseable method). Distinct from
    /// `Some(4xx)` = it ran and failed.
    status: Option<u16>,
    error: Option<String>,
}

pub struct Scheduler {
    data: Mutex<SchedulerData>,
}

fn now_local() -> (i64, u8, u8, u8) {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        let dow = ((tm.tm_wday + 6) % 7) as u8;
        (t as i64, tm.tm_hour as u8, tm.tm_min as u8, dow)
    }
}

fn validate_time(s: &str) -> Result<(), String> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 2 {
        return Err("time must be HH:MM".into());
    }
    let h: u8 = parts[0]
        .parse()
        .map_err(|_| "invalid hour".to_string())?;
    let m: u8 = parts[1]
        .parse()
        .map_err(|_| "invalid minute".to_string())?;
    if h > 23 {
        return Err("hour must be 0-23".into());
    }
    if m > 59 {
        return Err("minute must be 0-59".into());
    }
    Ok(())
}

/// Paths a scheduled job may never call (D20, write-op-layer.md E-A9): eSIM
/// (a switch that does not converge reboots the device), factory reset,
/// kill-bloat, power off (10-04: only a hand at the device turns it back on),
/// and the AT terminal (10-04, D40: any AT command can change the network
/// behind datad, and only the user's own may cancel a confirming change). Matched on the path as the router sees it (query and trailing
/// slashes dropped, case folded), so spelling variants do not slip through.
pub fn forbidden(path: &str) -> Option<&'static str> {
    let p = path.split(['?', '#']).next().unwrap_or("").trim_end_matches('/').to_ascii_lowercase();
    let p = p.replace("//", "/");
    if p == "/api/esim" || p.starts_with("/api/esim/") {
        Some("eSIM")
    } else if p == "/api/device/factory-reset" {
        Some("factory reset")
    } else if p == "/api/system/kill-bloat" {
        Some("kill-bloat")
    } else if p == "/api/device/poweroff" {
        Some("power off")
    } else if p == "/api/at/send" {
        Some("AT terminal")
    } else {
        None
    }
}

fn forbidden_msg(what: &str) -> (String, String) {
    (format!("定时任务不能调{what}（会改到这台设备唯一的上网方式或清掉设置）"), format!("A scheduled job may not call {what}"))
}

fn validate_job(
    action: &Action,
    schedule: &Schedule,
    restore: &Option<Restore>,
) -> Result<(), String> {
    if !action.path.starts_with("/api/") {
        return Err("action.path must start with /api/".into());
    }
    if action.path.starts_with("/api/scheduler/") {
        return Err("cannot schedule scheduler endpoints".into());
    }
    if action.path.starts_with("/api/auth/") {
        return Err("cannot schedule auth endpoints".into());
    }
    if let Some(what) = forbidden(&action.path) {
        return Err(forbidden_msg(what).0);
    }
    if crate::action::parse_method(&action.method).is_none() {
        return Err("action.method must be GET, POST, PUT, or DELETE".into());
    }
    match schedule {
        Schedule::Recurring { time, days } => {
            validate_time(time)?;
            if days.is_empty() {
                return Err("days must not be empty".into());
            }
            for &d in days {
                if d > 6 {
                    return Err("days must be 0-6 (Mon-Sun)".into());
                }
            }
        }
        Schedule::Once { at } => {
            if *at <= 1_000_000_000 {
                return Err("once.at must be a valid unix timestamp".into());
            }
        }
    }
    if let Some(r) = restore {
        validate_time(&r.time)?;
    }
    Ok(())
}

/// Jobs saved before D20 that call a forbidden path: switched off, with the
/// reason in `last_error` (the web page shows it). Returns (id, name, path).
fn disable_forbidden(jobs: &mut [Job]) -> Vec<(u32, String, String)> {
    let mut out = Vec::new();
    for j in jobs.iter_mut() {
        if let Some(what) = forbidden(&j.action.path) {
            let (zh, _) = forbidden_msg(what);
            let msg = format!("已停用：{zh}");
            if j.enabled || j.last_error.as_deref() != Some(msg.as_str()) {
                if j.enabled {
                    out.push((j.id, j.name.clone(), j.action.path.clone()));
                }
                j.enabled = false;
                j.last_error = Some(msg);
            }
        }
    }
    out
}

/// One journal line per job switched off (datad may still be starting: a few tries).
fn journal_disabled(stopped: Vec<(u32, String, String)>) {
    let _ = std::thread::Builder::new().name("sched-journal".into()).spawn(move || {
        for (id, name, path) in stopped {
            let line = json!({"item": "scheduler.job", "result": "disabled", "reason": "forbidden_path", "detail": {"id": id, "name": name, "path": path}});
            for _ in 0..10 {
                match crate::datad_write::send_as(crate::datad_write::Source::Scheduler, "journal.append", &line) {
                    r if r.ok() => break,
                    _ => std::thread::sleep(std::time::Duration::from_secs(30)),
                }
            }
        }
    });
}

fn save(data: &SchedulerData) {
    if let Ok(json) = serde_json::to_string_pretty(data) {
        let _ = crate::fsutil::atomic_write(STORAGE_PATH, json.as_bytes());
    }
}

impl Scheduler {
    pub fn new() -> Self {
        let mut data = std::fs::read_to_string(STORAGE_PATH)
            .ok()
            .and_then(|s| serde_json::from_str::<SchedulerData>(&s).ok())
            .unwrap_or(SchedulerData { jobs: Vec::new() });
        let stopped = disable_forbidden(&mut data.jobs);
        if !stopped.is_empty() {
            save(&data);
            journal_disabled(stopped);
        }
        Scheduler {
            data: Mutex::new(data),
        }
    }

    pub fn start(&self, state: Arc<AppState>) {
        let state = Arc::clone(&state);
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(TICK_SECS));
                state.scheduler.tick(&state);
            }
        });
    }

    fn tick(&self, state: &AppState) {
        let (now_ts, hour, minute, dow) = now_local();
        let now_hm = format!("{:02}:{:02}", hour, minute);

        // Phase 1: collect pending actions
        let pending: Vec<PendingAction> = {
            let data = self.data.lock().unwrap();
            let mut pending = Vec::new();

            for job in &data.jobs {
                if !job.enabled {
                    continue;
                }

                match &job.schedule {
                    Schedule::Recurring { time, days } => {
                        if *time == now_hm && days.contains(&dow) {
                            let guard = job
                                .last_run
                                .map(|lr| now_ts - lr > 60)
                                .unwrap_or(true);
                            if guard {
                                let body = crate::action::body_bytes(&job.action.body);
                                pending.push(PendingAction {
                                    job_id: job.id,
                                    method: job.action.method.clone(),
                                    path: job.action.path.clone(),
                                    body,
                                    is_restore: false,
                                });
                            }
                        }
                    }
                    Schedule::Once { at } => {
                        if now_ts >= *at && job.last_run.is_none() {
                            let body = crate::action::body_bytes(&job.action.body);
                            pending.push(PendingAction {
                                job_id: job.id,
                                method: job.action.method.clone(),
                                path: job.action.path.clone(),
                                body,
                                is_restore: false,
                            });
                        }
                    }
                }

                // Unified restore — fires at restore.time if action ran since last restore
                if let Some(restore) = &job.restore {
                    if restore.time == now_hm {
                        let should_restore = match (job.last_run, job.last_restore) {
                            (Some(lr), Some(lrest)) => lr > lrest,
                            (Some(_), None) => true,
                            _ => false,
                        };
                        let guard = job
                            .last_restore
                            .map(|lr| now_ts - lr > 60)
                            .unwrap_or(true);
                        if should_restore && guard {
                            let body = crate::action::body_bytes(&restore.body);
                            pending.push(PendingAction {
                                job_id: job.id,
                                method: job.action.method.clone(),
                                path: job.action.path.clone(),
                                body,
                                is_restore: true,
                            });
                        }
                    }
                }
            }

            pending
        };

        if pending.is_empty() {
            return;
        }

        // Phase 2: execute actions (no lock held — see action.rs rule 1).
        // `map`, not `filter_map`: an action with an unparseable method used to
        // be dropped silently, leaving last_run untouched so it was
        // indistinguishable from a job that had never come due. Now it produces
        // a result with status = None and an error string.
        let results: Vec<ExecResult> = pending
            .into_iter()
            .map(|pa| {
                // a job saved by hand into the file still cannot reach these
                let outcome = match forbidden(&pa.path) {
                    Some(what) => crate::action::Outcome { status: Some(403), error: Some(forbidden_msg(what).0) },
                    None => crate::action::exec(state, crate::datad_write::Source::Scheduler, &pa.method, &pa.path, &pa.body),
                };
                ExecResult {
                    job_id: pa.job_id,
                    is_restore: pa.is_restore,
                    status: outcome.status,
                    error: outcome.error,
                }
            })
            .collect();

        // Phase 3: update job state
        let mut data = self.data.lock().unwrap();
        for result in results {
            if let Some(job) = data.jobs.iter_mut().find(|j| j.id == result.job_id) {
                if result.is_restore {
                    job.last_restore = Some(now_ts);
                    // Record how the restore went. This used to drop status and
                    // error entirely, so a restore that 500'd left no trace.
                    job.last_restore_status = result.status;
                    job.last_restore_error = result.error;
                    // Disable once jobs after restore completes
                    if matches!(job.schedule, Schedule::Once { .. }) {
                        job.enabled = false;
                    }
                } else {
                    job.last_run = Some(now_ts);
                    job.last_status = result.status;
                    job.last_error = result.error;

                    // Auto-disable once jobs only if no restore pending
                    if matches!(job.schedule, Schedule::Once { .. }) && job.restore.is_none() {
                        job.enabled = false;
                    }
                }
            }
        }
        save(&data);
    }
}

// --- HTTP handlers ---

pub fn jobs_list(state: &AppState) -> (u16, Value) {
    let data = state.scheduler.data.lock().unwrap();
    (200, json!({"ok": true, "data": data.jobs}))
}

#[derive(Deserialize)]
struct CreateJobRequest {
    name: String,
    schedule: Schedule,
    action: Action,
    restore: Option<Restore>,
}

pub fn jobs_create(state: &AppState, body: &[u8]) -> (u16, Value) {
    let req: CreateJobRequest = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return (400, json!({"ok": false, "error": format!("invalid JSON: {e}")})),
    };

    if let Err(e) = validate_job(&req.action, &req.schedule, &req.restore) {
        return (400, json!({"ok": false, "error": e}));
    }

    let (now_ts, _, _, _) = now_local();
    let mut data = state.scheduler.data.lock().unwrap();

    let next_id = data.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;

    let job = Job {
        id: next_id,
        name: req.name,
        enabled: true,
        schedule: req.schedule,
        action: req.action,
        restore: req.restore,
        last_run: None,
        last_status: None,
        last_restore_status: None,
        last_restore_error: None,
        last_error: None,
        last_restore: None,
        created_at: now_ts,
    };

    let result = json!({"ok": true, "data": job});
    data.jobs.push(job);
    save(&data);
    (201, result)
}

#[derive(Deserialize)]
struct UpdateJobRequest {
    id: u32,
    name: String,
    enabled: bool,
    schedule: Schedule,
    action: Action,
    restore: Option<Restore>,
}

pub fn jobs_update(state: &AppState, body: &[u8]) -> (u16, Value) {
    let req: UpdateJobRequest = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return (400, json!({"ok": false, "error": format!("invalid JSON: {e}")})),
    };

    if let Err(e) = validate_job(&req.action, &req.schedule, &req.restore) {
        return (400, json!({"ok": false, "error": e}));
    }

    let mut data = state.scheduler.data.lock().unwrap();
    let job = match data.jobs.iter_mut().find(|j| j.id == req.id) {
        Some(j) => j,
        None => return (404, json!({"ok": false, "error": "job not found"})),
    };

    job.name = req.name;
    job.enabled = req.enabled;
    job.schedule = req.schedule;
    job.action = req.action;
    job.restore = req.restore;

    let result = json!({"ok": true, "data": job.clone()});
    save(&data);
    (200, result)
}

pub fn jobs_delete(state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let id = match parsed["id"].as_u64() {
        Some(id) => id as u32,
        None => return (400, json!({"ok": false, "error": "missing 'id' field"})),
    };

    let mut data = state.scheduler.data.lock().unwrap();
    let len_before = data.jobs.len();
    data.jobs.retain(|j| j.id != id);

    if data.jobs.len() == len_before {
        return (404, json!({"ok": false, "error": "job not found"}));
    }

    save(&data);
    (200, json!({"ok": true}))
}

pub fn jobs_toggle(state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let id = match parsed["id"].as_u64() {
        Some(id) => id as u32,
        None => return (400, json!({"ok": false, "error": "missing 'id' field"})),
    };
    let enabled = match parsed["enabled"].as_bool() {
        Some(e) => e,
        None => return (400, json!({"ok": false, "error": "missing 'enabled' field"})),
    };

    let mut data = state.scheduler.data.lock().unwrap();
    let job = match data.jobs.iter_mut().find(|j| j.id == id) {
        Some(j) => j,
        None => return (404, json!({"ok": false, "error": "job not found"})),
    };
    if enabled {
        if let Some(what) = forbidden(&job.action.path) {
            let (zh, en) = forbidden_msg(what);
            return (400, json!({"ok": false, "error": zh, "error_en": en}));
        }
    }

    job.enabled = enabled;
    let result = json!({"ok": true, "data": job.clone()});
    save(&data);
    (200, result)
}

#[cfg(test)]
mod forbidden_tests {
    use super::*;

    #[test]
    fn forbidden_paths() {
        for (p, want) in [
            ("/api/esim/switch", Some("eSIM")),
            ("/api/esim/download", Some("eSIM")),
            ("/api/ESIM/switch", Some("eSIM")),
            ("/api/esim/switch/", Some("eSIM")),
            ("/api/esim/switch?x=1", Some("eSIM")),
            ("/api//esim/switch", Some("eSIM")),
            ("/api/esim", Some("eSIM")),
            ("/api/device/factory-reset", Some("factory reset")),
            ("/api/device/factory-reset/", Some("factory reset")),
            ("/api/system/kill-bloat", Some("kill-bloat")),
            ("/api/device/reboot", None),
            ("/api/device/poweroff", Some("power off")),
            ("/api/device/poweroff/", Some("power off")),
            ("/api/modem/network-mode", None),
            ("/api/wifi/radio", None),
            ("/api/esimx", None),
            ("/api/at/send", Some("AT terminal")),
            ("/api/AT/Send/", Some("AT terminal")),
            ("/api//at/send?x=1", Some("AT terminal")),
            ("/api/at/port", None),
        ] {
            assert_eq!(forbidden(p), want, "{p}");
        }
    }

    fn job(id: u32, path: &str, enabled: bool) -> Job {
        Job {
            id,
            name: format!("j{id}"),
            enabled,
            schedule: Schedule::Once { at: 2_000_000_000 },
            action: Action { method: "POST".into(), path: path.into(), body: None },
            restore: None,
            last_run: None,
            last_status: None,
            last_error: None,
            last_restore: None,
            last_restore_status: None,
            last_restore_error: None,
            created_at: 0,
        }
    }

    #[test]
    fn old_jobs_are_switched_off_with_the_reason() {
        let mut jobs = vec![job(1, "/api/esim/switch", true), job(2, "/api/device/reboot", true), job(3, "/api/system/kill-bloat", false), job(4, "/api/at/send", true)];
        let stopped = disable_forbidden(&mut jobs);
        assert_eq!(stopped.iter().map(|s| s.0).collect::<Vec<_>>(), vec![1, 4]);
        assert!(!jobs[3].enabled && jobs[3].last_error.as_deref().unwrap().contains("AT terminal"));
        assert!(!jobs[0].enabled && jobs[0].last_error.as_deref().unwrap().starts_with("已停用"));
        assert!(jobs[1].enabled && jobs[1].last_error.is_none());
        assert!(!jobs[2].enabled && jobs[2].last_error.is_some());
        // a second start changes nothing more
        assert!(disable_forbidden(&mut jobs).is_empty());
    }

    #[test]
    fn creating_one_is_refused() {
        let s = Schedule::Once { at: 2_000_000_000 };
        for p in ["/api/esim/switch", "/api/device/factory-reset", "/api/system/kill-bloat", "/api/at/send"] {
            let a = Action { method: "POST".into(), path: p.into(), body: None };
            assert!(validate_job(&a, &s, &None).is_err(), "{p}");
        }
        let a = Action { method: "POST".into(), path: "/api/device/reboot".into(), body: None };
        assert!(validate_job(&a, &s, &None).is_ok());
    }
}
