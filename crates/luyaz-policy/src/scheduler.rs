//! Scheduled jobs, moved from zte-agent `src/scheduler.rs` (manager 84f3404): the job types
//! (the shape of `scheduler.json`), `validate_job`, `forbidden`, `disable_forbidden`, and the
//! due-job choice the agent's `tick` makes every 30 s, as `due`. Running the actions (and
//! refusing a forbidden path again at that moment), saving the file and the journal stay in the
//! host; [`record`] is the bookkeeping `tick` does with each result.
//!
//! Deliberate differences from the agent: `due` takes the local time (`hh`, `mm`, `dow`) as
//! inputs; the host computes it with `localtime_r` (the device clock is local time labelled
//! UTC, so the digits are right). Decisions are the same, except that `HH:MM` is compared as
//! numbers ([`at_minute`]): the agent used to compare it as text against the zero-padded local
//! time, so a job saved as `9:05` (which `validate_time` accepts) never fired.

pub use crate::action::{Action, Json, is_method};

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Job {
    pub id: u32,
    pub name: String,
    pub enabled: bool,
    pub schedule: Schedule,
    pub action: Action,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub restore: Option<Restore>,
    pub last_run: Option<i64>,
    pub last_status: Option<u16>,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub last_error: Option<String>,
    pub last_restore: Option<i64>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub last_restore_status: Option<u16>,
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub last_restore_error: Option<String>,
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "type"))]
pub enum Schedule {
    #[cfg_attr(feature = "serde", serde(rename = "once"))]
    Once { at: i64 },
    /// `days`: 0 = Monday … 6 = Sunday.
    #[cfg_attr(feature = "serde", serde(rename = "recurring"))]
    Recurring { time: String, days: Vec<u8> },
}

/// Runs the job's action again with this body at `time`, if the action ran since the last restore.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Restore {
    pub time: String,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub body: Option<Json>,
}

pub fn validate_time(s: &str) -> Result<(), String> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 2 {
        return Err("time must be HH:MM".into());
    }
    let h: u8 = parts[0].parse().map_err(|_| "invalid hour".to_string())?;
    let m: u8 = parts[1].parse().map_err(|_| "invalid minute".to_string())?;
    if h > 23 {
        return Err("hour must be 0-23".into());
    }
    if m > 59 {
        return Err("minute must be 0-59".into());
    }
    Ok(())
}

/// Whether a saved `HH:MM` (with or without leading zeros, as `validate_time` accepts) is the
/// local minute `hh:mm`. Text that isn't a time never matches.
pub fn at_minute(time: &str, hh: u8, mm: u8) -> bool {
    match time.split_once(':') {
        Some((h, m)) => h.parse::<u8>() == Ok(hh) && m.parse::<u8>() == Ok(mm),
        None => false,
    }
}

/// Paths a scheduled job may never call (D20): eSIM, factory reset, kill-bloat, power off and
/// the AT terminal. Matched on the path as the router sees it (query and trailing slashes
/// dropped, case folded, slashes collapsed); a `%` or a dot segment is refused outright.
pub fn forbidden(path: &str) -> Option<&'static str> {
    let mut p = path
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches('/')
        .to_ascii_lowercase();
    // every run of slashes: one pass left `///` as `//` (batch 5c)
    while p.contains("//") {
        p = p.replace("//", "/");
    }
    // no route of ours has a `%` or a dot segment: a job that hides a path behind one is refused
    if p.contains('%') || p.split('/').any(|s| s == "." || s == "..") {
        return Some("an encoded or dotted path");
    }
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

/// (Chinese, English) refusal text.
pub fn forbidden_msg(what: &str) -> (String, String) {
    (
        format!("定时任务不能调{what}（会改到这台设备唯一的上网方式或清掉设置）"),
        format!("A scheduled job may not call {what}"),
    )
}

pub fn validate_job(
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
    if !is_method(&action.method) {
        return Err("action.method must be GET, POST, PUT, or DELETE".into());
    }
    match schedule {
        Schedule::Recurring { time, days } => {
            validate_time(time)?;
            if days.is_empty() {
                return Err("days must not be empty".into());
            }
            if days.iter().any(|&d| d > 6) {
                return Err("days must be 0-6 (Mon-Sun)".into());
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

/// Jobs saved before D20 that call a forbidden path: switched off, with the reason in
/// `last_error`. Returns (id, name, path) of the ones that were enabled.
pub fn disable_forbidden(jobs: &mut [Job]) -> Vec<(u32, String, String)> {
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

/// Earliest plausible device epoch (2026-01-01 00:00:00 UTC, matching `CLOCK_YEAR_MIN`): before
/// SNTP sets the clock after boot, no scheduled job or restore may fire on the 1970/build-time RTC.
pub const CLOCK_SET: i64 = 1_767_225_600;

/// Jobs that fire now, in job order: `(id, is_restore)`. A restore runs the job's action with
/// `restore.body`. `now_ts` is the device clock (unix seconds as the device keeps them),
/// `hh:mm` and `dow` (0 = Monday) its local time. The 60 s guards keep a 30 s tick (or a small
/// backward clock adjustment in the same minute) from firing twice in the same minute without
/// wedging recurring jobs after a large backward wall-clock jump (e.g. -8 h). Forbidden paths are
/// not filtered here: the host refuses them when it runs them (403), which `record` then stores.
pub fn due(jobs: &[Job], now_ts: i64, hh: u8, mm: u8, dow: u8) -> Vec<(u32, bool)> {
    if now_ts < CLOCK_SET || hh > 23 || mm > 59 || dow > 6 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for job in jobs.iter().filter(|j| j.enabled) {
        let run = match &job.schedule {
            Schedule::Recurring { time, days } => {
                at_minute(time, hh, mm)
                    && days.contains(&dow)
                    && job.last_run.is_none_or(|lr| now_ts.abs_diff(lr) > 60)
            }
            Schedule::Once { at } => now_ts >= *at && job.last_run.is_none(),
        };
        if run {
            out.push((job.id, false));
        }
        // Restore fires at restore.time if the action ran since the last restore (judged on
        // the state before this tick: an action due now is not counted yet).
        if let Some(restore) = &job.restore
            && at_minute(&restore.time, hh, mm)
        {
            let should_restore = match (job.last_run, job.last_restore) {
                (Some(lr), Some(lrest)) => lr > lrest,
                (Some(_), None) => true,
                _ => false,
            };
            let guard = job.last_restore.is_none_or(|lr| now_ts.abs_diff(lr) > 60);
            if should_restore && guard {
                out.push((job.id, true));
            }
        }
    }
    out
}

/// What `tick` writes back after running a due action (`status` `None` = never ran).
pub fn record(
    job: &mut Job,
    is_restore: bool,
    status: Option<u16>,
    error: Option<String>,
    now_ts: i64,
) {
    let once = matches!(job.schedule, Schedule::Once { .. });
    if is_restore {
        job.last_restore = Some(now_ts);
        job.last_restore_status = status;
        job.last_restore_error = error;
        // a once job is done after its restore
        if once {
            job.enabled = false;
        }
    } else {
        job.last_run = Some(now_ts);
        job.last_status = status;
        job.last_error = error;
        // If the wall clock stepped backward since the previous restore (`last_restore >= now_ts`),
        // clear `last_restore` so `last_run > last_restore` does not wedge the upcoming restore.
        if job.last_restore.is_some_and(|lrest| lrest >= now_ts) {
            job.last_restore = None;
        }
        // a once job without a restore is done after running
        if once && job.restore.is_none() {
            job.enabled = false;
        }
    }
}

#[cfg(test)]
mod tests;
