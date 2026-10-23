//! The scheduled jobs' routes (they edit the file); running them is
//! [`crate::engines::scheduler`].

use axum::{
    body::Bytes,
    extract::State,
    response::Response,
};
use serde::Deserialize;
use serde_json::{Value, json};
use luyaz_policy::scheduler::{self as sp, Action, Job, Restore, Schedule};

use super::{Compat, reply};
use crate::engines::scheduler::*;

/// Load, change, save under the lock; `f` answers the reply and whether to save.
fn edit(c: &Compat, f: impl FnOnce(&mut Vec<Job>) -> (u16, Value, bool)) -> Response {
    let _g = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let p = file(c);
    let mut jobs = match load(&p) {
        Ok(j) => j,
        Err(e) => {
            return reply(500, json!({"ok": false, "error": format!("读不了任务文件：{e}")}));
        }
    };
    let (status, body, changed) = f(&mut jobs);
    if changed && let Err(e) = save(&p, &jobs) {
        return reply(500, json!({"ok": false, "error": format!("存不了任务文件：{e}")}));
    }
    reply(status, body)
}

fn now_ts() -> i64 {
    now_local().0
}

fn bad(e: impl Into<String>) -> (u16, Value, bool) {
    (400, json!({"ok": false, "error": e.into()}), false)
}

fn not_found() -> (u16, Value, bool) {
    (404, json!({"ok": false, "error": "job not found"}), false)
}

/// GET /api/scheduler/jobs
pub async fn list(State(c): State<Compat>) -> Response {
    edit(&c, |jobs| (200, json!({"ok": true, "data": jobs}), false))
}

/// Body of a create: everything the caller chooses; bookkeeping starts empty.
#[derive(Deserialize)]
struct Create {
    /// label for the list
    name: String,
    /// once / recurring
    schedule: Schedule,
    /// the API call to make
    action: Action,
    /// optional undo call
    restore: Option<Restore>,
}

/// POST /api/scheduler/jobs
pub async fn create(State(c): State<Compat>, body: Bytes) -> Response {
    let req: Create = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return reply(400, json!({"ok": false, "error": format!("invalid JSON: {e}")})),
    };
    if let Err(e) = sp::validate_job(&req.action, &req.schedule, &req.restore) {
        return reply(400, json!({"ok": false, "error": e}));
    }
    edit(&c, |jobs| {
        let next_id = 1 + jobs.iter().map(|j| j.id).max().unwrap_or(0);
        let Create {
            name,
            schedule,
            action,
            restore,
        } = req;
        let job = Job {
            id: next_id,
            created_at: now_ts(),
            enabled: true,
            name,
            action,
            schedule,
            restore,
            // nothing has fired yet
            last_run: None,
            last_restore: None,
            last_status: None,
            last_restore_status: None,
            last_error: None,
            last_restore_error: None,
        };
        let body = json!({"ok": true, "data": job});
        jobs.push(job);
        (201, body, true)
    })
}

/// Body of an edit: the job to change and its new caller-chosen fields (bookkeeping is kept).
#[derive(Deserialize)]
struct Update {
    /// which job
    id: u32,
    /// switched on or off
    enabled: bool,
    /// new label
    name: String,
    /// new timing
    schedule: Schedule,
    /// new call
    action: Action,
    /// new undo call, or none
    restore: Option<Restore>,
}

/// PUT /api/scheduler/jobs
pub async fn update(State(c): State<Compat>, body: Bytes) -> Response {
    let req: Update = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return reply(400, json!({"ok": false, "error": format!("invalid JSON: {e}")})),
    };
    if let Err(e) = sp::validate_job(&req.action, &req.schedule, &req.restore) {
        return reply(400, json!({"ok": false, "error": e}));
    }
    edit(&c, |jobs| {
        let Update {
            id,
            enabled,
            name,
            schedule,
            action,
            restore,
        } = req;
        let Some(job) = jobs.iter_mut().find(|j| j.id == id) else {
            return not_found();
        };
        (job.enabled, job.name) = (enabled, name);
        (job.schedule, job.action, job.restore) = (schedule, action, restore);
        (200, json!({"ok": true, "data": job.clone()}), true)
    })
}

fn id_of(v: &Value) -> Result<u32, (u16, Value, bool)> {
    v["id"]
        .as_u64()
        .and_then(|i| u32::try_from(i).ok())
        .ok_or_else(|| bad("missing 'id' field"))
}

/// DELETE /api/scheduler/jobs `{"id"}`
pub async fn delete(State(c): State<Compat>, body: Bytes) -> Response {
    let Ok(v) = serde_json::from_slice::<Value>(&body) else {
        return reply(400, json!({"ok": false, "error": "invalid JSON"}));
    };
    edit(&c, |jobs| {
        let id = match id_of(&v) {
            Ok(i) => i,
            Err(e) => return e,
        };
        let before = jobs.len();
        jobs.retain(|j| j.id != id);
        if jobs.len() == before {
            return not_found();
        }
        (200, json!({"ok": true}), true)
    })
}

/// PUT /api/scheduler/jobs/toggle `{"id", "enabled"}`: a forbidden job cannot be switched on.
pub async fn toggle(State(c): State<Compat>, body: Bytes) -> Response {
    let Ok(v) = serde_json::from_slice::<Value>(&body) else {
        return reply(400, json!({"ok": false, "error": "invalid JSON"}));
    };
    edit(&c, |jobs| {
        let id = match id_of(&v) {
            Ok(i) => i,
            Err(e) => return e,
        };
        let Some(enabled) = v["enabled"].as_bool() else {
            return bad("missing 'enabled' field");
        };
        let Some(job) = jobs.iter_mut().find(|j| j.id == id) else {
            return not_found();
        };
        if enabled && let Some(what) = sp::forbidden(&job.action.path) {
            let (zh, en) = sp::forbidden_msg(what);
            return (400, json!({"ok": false, "error": zh, "error_en": en}), false);
        }
        job.enabled = enabled;
        (200, json!({"ok": true, "data": job.clone()}), true)
    })
}
