//! Scheduled jobs: the record kept in `scheduler.json`, the checks a new or edited job must
//! pass (`validate_job`, `forbidden`, `disable_forbidden`), the 30 s tick's choice of what is due
//! ([`due`]) and what the tick writes back afterwards ([`record`]). Running an action (and
//! refusing a forbidden path once more at that moment), saving the file and the journal are the
//! host's job.
//!
//! `due` is handed the local wall time (`hh`, `mm`, `dow`); the host gets it from
//! `localtime_r` (the device keeps local time labelled UTC, so the digits are right). `HH:MM` is
//! matched as numbers ([`at_minute`]), so `9:05` (which `validate_time` accepts) fires at 09:05.

pub use crate::action::{Action, Json, is_method};

/// One saved job. Field names are the keys of `scheduler.json`.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Job {
    /// Stable number handed out on create.
    pub id: u32,
    /// Label shown in the UI.
    pub name: String,
    /// Off = kept but never fired.
    pub enabled: bool,
    /// When the action fires.
    pub schedule: Schedule,
    /// The local API call made when it fires.
    pub action: Action,
    /// Optional second call that undoes the action later the same day.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub restore: Option<Restore>,
    /// Device-clock second of the latest firing.
    pub last_run: Option<i64>,
    /// HTTP status of that firing (`None` = it never got an answer).
    pub last_status: Option<u16>,
    /// Why the latest firing failed, or why the job was switched off.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub last_error: Option<String>,
    /// Device-clock second of the latest restore.
    pub last_restore: Option<i64>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub last_restore_status: Option<u16>,
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub last_restore_error: Option<String>,
    /// Device-clock second the job was saved.
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

/// `H:MM` / `HH:MM`, hour 0–23, minute 0–59.
pub fn validate_time(s: &str) -> Result<(), String> {
    let Some((hour, minute)) = s.split_once(':').filter(|(_, m)| !m.contains(':')) else {
        return Err("time must be HH:MM".into());
    };
    let hour: u8 = hour.parse().map_err(|_| String::from("invalid hour"))?;
    let minute: u8 = minute.parse().map_err(|_| String::from("invalid minute"))?;
    match (hour, minute) {
        (24.., _) => Err("hour must be 0-23".into()),
        (_, 60..) => Err("minute must be 0-59".into()),
        _ => Ok(()),
    }
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

/// Path prefixes a job may not target at all, with the refusal text.
const OFF_LIMITS: [(&str, &str); 2] = [
    ("/api/scheduler/", "jobs may not call the scheduler API"),
    ("/api/auth/", "jobs may not call the login API"),
];

/// Checks a job before it is saved; the `Err` text goes back to the caller as is.
pub fn validate_job(act: &Action, when: &Schedule, undo: &Option<Restore>) -> Result<(), String> {
    let path = act.path.as_str();
    if !path.starts_with("/api/") {
        return Err("action.path: only /api/ paths can be scheduled".into());
    }
    if let Some((_, why)) = OFF_LIMITS.iter().find(|(pre, _)| path.starts_with(pre)) {
        return Err((*why).to_string());
    }
    if let Some(what) = forbidden(path) {
        return Err(forbidden_msg(what).0);
    }
    let bad_when: Option<&str> = if !is_method(&act.method) {
        Some("action.method: use GET, POST, PUT or DELETE")
    } else {
        match when {
            Schedule::Recurring {
                time: hhmm,
                days: weekdays,
            } => {
                validate_time(hhmm)?;
                if weekdays.is_empty() {
                    Some("days must not be empty")
                } else if weekdays.iter().any(|&d| d > 6) {
                    Some("days: each day is 0 (Monday) to 6 (Sunday)")
                } else {
                    None
                }
            }
            // at or before 2001-09-09 is a typo or a seconds/ms mix-up, not a moment
            Schedule::Once { at } if *at <= 1_000_000_000 => {
                Some("once.at: not a unix time in seconds")
            }
            Schedule::Once { .. } => None,
        }
    };
    if let Some(why) = bad_when {
        return Err(why.to_string());
    }
    undo.as_ref().map_or(Ok(()), |r| validate_time(&r.time))
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
            // pending = the action has fired and no restore has followed it yet
            let pending = job
                .last_run
                .is_some_and(|ran| job.last_restore.is_none_or(|undone| ran > undone));
            let guard = job.last_restore.is_none_or(|lr| now_ts.abs_diff(lr) > 60);
            if pending && guard {
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
