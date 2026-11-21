//! CHILL's on/off switch and profile for the touch screen, ported from the
//! agent's `chill.rs`: `chill.sh start|stop|profile X` as one job at a time, run as a child process
//! off the request; then, abroad, the scenario engine's restore entries (the agent's
//! `chill_toggled`). Only with u60d's writes on, like every write.
//!
//! The screen's, luyaz-web's, the scheduler's and the scenario's calls all come here, so
//! the device has one job slot. A scenario restore's own "on" comes with
//! [`luyaz_api::headers::RESTORE_HEADER`]: it keeps its restore entry (the restore drops it).
//!
//! The home exit's switches (PUT `/api/services/chill/home`) are not here: they need the home exit's
//! machine and its 30 s loop, which stay with the agent until the whole of it moves.

use axum::{
    body::Bytes,
    extract::{ConnectInfo, Request, State},
    response::Response,
};
use luyaz_api::chill as ch;
use luyaz_policy::scenario as sc;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::{Compat, reply};

const RESTORE_FILE: &str = "/data/scenario/restore.json";

/// One job at a time (the agent's `ChillAdmin`): `{id, kind, status, message, started_unix,
/// finished_unix}`, status idle | running | done | error.
pub struct ChillJobs {
    job: Mutex<Value>,
    /// the job slot; u60d's CHILL transactions (the agent's, `ops::chill`) take the same one
    running: Arc<AtomicBool>,
    /// chill.sh's deadline (`ops::chill::SH_TIMEOUT`; tests shorten it)
    limit: std::time::Duration,
}

impl Default for ChillJobs {
    fn default() -> Self {
        Self::sharing(Arc::default())
    }
}

impl ChillJobs {
    /// With this job slot (the device's: `ops::chill::slot()`).
    pub fn sharing(running: Arc<AtomicBool>) -> Self {
        Self::sharing_within(running, crate::ops::chill::SH_TIMEOUT)
    }
    /// With this job slot and chill.sh deadline.
    pub fn sharing_within(running: Arc<AtomicBool>, limit: std::time::Duration) -> Self {
        ChillJobs {
            job: Mutex::new(json!({"id": 0, "kind": "", "status": "idle", "message": "",
                "started_unix": 0, "finished_unix": 0})),
            running,
            limit,
        }
    }
}

fn now() -> u64 {
    super::home::now_unix()
}

impl ChillJobs {
    fn lock(&self) -> std::sync::MutexGuard<'_, Value> {
        self.job.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// `chill.sh <args>`: (ok, stdout + stderr). Never killed with u60d: like the agent's, a job
/// whose parent restarts (every ship restarts u60d) finishes on its own, instead of leaving mihomo
/// half started or stopped. Killed (its process group) past `limit` instead (resilience audit R7):
/// a wedged chill.sh would otherwise hold the CHILL slot until u60d restarts.
async fn chill_sh(c: &Compat, args: &str, limit: std::time::Duration) -> (bool, String) {
    let cmd = format!("exec {} {args}", shell_quote(&c.path(ch::CHILL_SH).to_string_lossy()));
    match crate::ops::chill::sh_within(&cmd, limit).await {
        Ok(r) => r,
        Err(e) => (false, e),
    }
}

/// Start `args` as the job; 409 while one runs. `ok` words the success from the output.
fn start_job(c: &Compat, kind: &str, args: String, ok: fn(&str, &str) -> String) -> Option<u64> {
    let slot = reserve(c)?;
    let c2 = c.clone();
    Some(spawn(c, slot, kind, async move {
        let (success, out) = chill_sh(&c2, &args, c2.chill.limit).await;
        if success {
            Ok(ok(&args, ""))
        } else {
            Err(script_error(&out))
        }
    }))
}

pub(super) struct Slot(Arc<AtomicBool>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

pub(super) fn reserve(c: &Compat) -> Option<Slot> {
    if c.chill.running.swap(true, Ordering::SeqCst) {
        return None;
    }
    let slot = Slot(c.chill.running.clone());
    if c.root.is_empty() && crate::ops::chill::sh_running() {
        return None;
    }
    Some(slot)
}

pub(super) fn spawn(
    c: &Compat,
    slot: Slot,
    kind: &str,
    work: impl Future<Output = Result<String, String>> + Send + 'static,
) -> u64 {
    let id = {
        let mut j = c.chill.lock();
        let id = j["id"].as_u64().unwrap_or(0).wrapping_add(1);
        *j = json!({"id": id, "kind": kind, "status": "running", "message": "",
            "started_unix": now(), "finished_unix": 0});
        id
    };
    let c2 = c.clone();
    tokio::spawn(async move {
        let (status, message) = match work.await {
            Ok(message) => ("done", message),
            Err(message) => ("error", message),
        };
        {
            let mut j = c2.chill.lock();
            j["status"] = json!(status);
            j["message"] = json!(message);
            j["finished_unix"] = json!(now());
            drop(slot);
        }
    });
    id
}

pub(super) fn busy() -> Response {
    reply(409, json!({"ok": false, "error": "operation in progress"}))
}

fn started(id: u64) -> Response {
    reply(
        200,
        json!({"ok": true, "data": {"job_id": id, "status": "running"}}),
    )
}

pub(super) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn script_error(output: &str) -> String {
    // Configuration parsers and providers can print source URLs and secrets.
    // Surface only fixed reason codes, never their output.
    if let Some(reason) = output.lines().rev().map(str::trim).find(|line| matches!(
        *line,
        "ipv6_unavailable" | "provider_refresh_failed" | "profile_switch_failed"
            | "profile_activation_failed_rolled_back" | "profile_recovery_failed" | "profile_busy"
    )) {
        reason.into()
    } else if output.contains("timed out") {
        "CHILL operation timed out".into()
    } else {
        "CHILL operation failed".into()
    }
}

pub(super) async fn profile_command(
    c: &Compat,
    command: &str,
    id: &str,
) -> Result<(), String> {
    if !super::chill_profiles::valid_id(id)
        || !matches!(command, "profile-switch" | "profile-validate" | "profile-refresh")
    {
        return Err("invalid profile command".into());
    }
    let args = format!("{command} {}", shell_quote(id));
    let (success, output) = chill_sh(c, &args, c.chill.limit).await;
    if success { Ok(()) } else { Err(script_error(&output)) }
}

fn read_state(c: &Compat) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(c.path(ch::CHILL_STATE)).ok()?).ok()
}

/// A call the agent forwards from a scenario restore (loopback only).
fn from_restore(req: &Request) -> bool {
    let loopback = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .is_none_or(|ci| ci.0.ip().is_loopback());
    loopback
        && req
            .headers()
            .contains_key(luyaz_api::headers::RESTORE_HEADER)
}

/// POST /api/services/chill/enable
pub async fn enable(State(c): State<Compat>, req: Request) -> Response {
    let restore = from_restore(&req);
    if let Err(e) = ch::precheck(read_state(&c).as_ref()) {
        return reply(409, json!({"ok": false, "error": e}));
    }
    match start_job(&c, "enable", "start".into(), |a, _| {
        format!("chill.sh {a} ok")
    }) {
        Some(id) => {
            if !restore {
                toggled(&c, true, false);
            }
            started(id)
        }
        None => busy(),
    }
}

/// POST /api/services/chill/disable
pub async fn disable(State(c): State<Compat>) -> Response {
    let was_on = !c.path(ch::CHILL_DISABLED).exists();
    match start_job(&c, "disable", "stop".into(), |a, _| {
        format!("chill.sh {a} ok")
    }) {
        Some(id) => {
            toggled(&c, false, was_on);
            started(id)
        }
        None => busy(),
    }
}

/// PUT /api/services/chill/profile `{"profile": "eco"|"standard"|"perf"}`
pub async fn profile(State(c): State<Compat>, body: Bytes) -> Response {
    let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let want = match ch::profile_wanted(&parsed) {
        Ok(p) => p.to_string(),
        Err(r) => return reply(r.0, r.1),
    };
    match start_job(&c, "profile", format!("profile {want}"), |a, _| {
        format!("chill.sh {a} ok")
    }) {
        Some(id) => started(id),
        None => busy(),
    }
}

/// PUT /api/services/chill/providers `{"name": "sub", "url": …}`: the subscription URL into
/// chill.env, then `chill.sh reload` as the job (its render step checks with `mihomo -t` and keeps
/// the old config on failure). Only the agent calls it (the web's subscription page).
pub async fn providers(State(c): State<Compat>, body: Bytes) -> Response {
    let url = match ch::provider_url_wanted(&body) {
        Ok(u) => u,
        Err(r) => return reply(r.0, r.1),
    };
    if let Err(e) = ch::precheck(read_state(&c).as_ref()) {
        return reply(409, json!({"ok": false, "error": e}));
    }
    let Some(slot) = reserve(&c) else {
        return busy();
    };
    let p = c.path(ch::CHILL_ENV);
    let out = match ch::env_with(
        &std::fs::read_to_string(&p).unwrap_or_default(),
        &[("SUB_URL", &url)],
    ) {
        Ok(o) => o,
        Err(e) => return reply(500, json!({"ok": false, "error": e})),
    };
    // chill.env holds the subscription and mihomo's secret: keep its mode
    let mode = std::fs::metadata(&p).ok().map(|m| m.permissions());
    if let Err(e) = super::ops::atomic_write(&p, out.as_bytes()) {
        return reply(
            500,
            json!({"ok": false, "error": format!("cannot write chill.env: {e}")}),
        );
    }
    if let Some(m) = mode {
        let _ = std::fs::set_permissions(&p, m);
    }
    let work = c.clone();
    let id = spawn(&c, slot, "reload", async move {
        let (success, output) = chill_sh(&work, "reload", work.chill.limit).await;
        if success { Ok("Reloaded".into()) } else { Err(script_error(&output)) }
    });
    started(id)
}

/// GET /api/services/chill/job
pub async fn job(State(c): State<Compat>) -> Response {
    let j = c.chill.lock().clone();
    reply(200, json!({"ok": true, "data": j}))
}

/// The agent's `scenario::chill_toggled`: switched on by hand drops "switch it on when home";
/// switched off abroad (it was on) adds it.
fn toggled(c: &Compat, on: bool, was_on: bool) {
    use super::status::{SCENARIO_CONFIG, SCENARIO_DISABLED, SCENARIO_STATE};
    let read = |p: &str| std::fs::read_to_string(c.path(p)).ok();
    let mut pending: Vec<sc::PendingRestore> = read(RESTORE_FILE)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let write = |pending: &Vec<sc::PendingRestore>| {
        if let Ok(text) = serde_json::to_string_pretty(pending) {
            let _ = super::ops::atomic_write(&c.path(RESTORE_FILE), text.as_bytes());
        }
    };
    if on {
        let before = pending.len();
        pending.retain(|p| p.key != ch::CHILL_ON_KEY);
        if pending.len() != before {
            write(&pending);
        }
        return;
    }
    if !was_on
        || luyaz_scenario::disabled_state(c.path(SCENARIO_DISABLED))
            != luyaz_scenario::DisabledState::Active
    {
        return;
    }
    let cfg: sc::Config = read(SCENARIO_CONFIG)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let st: sc::RunState = read(SCENARIO_STATE)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let abroad = cfg
        .scenarios
        .iter()
        .any(|s| s.id == st.current && sc::is_abroad(s));
    if !abroad || pending.iter().any(|p| p.key == ch::CHILL_ON_KEY) {
        return;
    }
    if let Ok(e) = serde_json::from_value(ch::chill_on_restore(super::home::now_unix() as i64)) {
        pending.push(e);
        write(&pending);
    }
}
