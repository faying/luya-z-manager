//! The delivery queue: one job per (SMS × rule), each with its own state, attempts and next try,
//! saved on every transition (tmp + fsync + rename + dir fsync, mode 0600: it holds SMS bodies).
//!
//! The pass only enqueues and moves the watermark on; [`crate::SmsForwarder`] drains the queue
//! (one attempt per due job, then back off). A channel that is down holds only its own jobs: the
//! others keep going, and a job that went out is never sent again (also across restarts). SMS
//! are known by a fingerprint of sender, date and text, not by id: the firmware renumbers SIM
//! messages when it reads a card again, so an SMS seen once is not queued again at a new id.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Next to the core's state (the core passes its own data dir).
pub const QUEUE_PATH: &str = "/data/luyaz/core/sms-forward-queue.json";

/// First retry after this, doubling up to [`BACKOFF_CAP_SECS`].
pub(crate) const BACKOFF_BASE_SECS: i64 = 30;
pub(crate) const BACKOFF_CAP_SECS: i64 = 15 * 60;
/// A job still pending after this much online time is given up (alert once).
/// A job that goes out this long after it was queued is marked late in the chat text.
pub(crate) const LATE_AFTER_SECS: i64 = 120;
pub(crate) const GIVE_UP_ONLINE_SECS: u64 = 24 * 3600;
/// Jobs kept; past it the oldest finished ones go first, then the oldest pending (given up).
pub(crate) const MAX_JOBS: usize = 300;
/// Finished jobs are kept this long (the API shows them), then dropped.
pub(crate) const DONE_KEEP_SECS: i64 = 3 * 86_400;
/// Fingerprints remembered (newest kept) and for how long.
pub(crate) const MAX_SEEN: usize = 1000;
pub(crate) const SEEN_KEEP_SECS: i64 = 60 * 86_400;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Pending,
    Sent,
    /// A permanent error, or given up (age / queue size).
    Failed,
}

/// One SMS to one rule.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Job {
    /// [`fingerprint`] of the SMS.
    pub key: String,
    pub rule_id: u32,
    /// Device id when queued (for the log, mark-read and delete; checked against `key` first).
    pub sms_id: u64,
    pub sender: String,
    pub content: String,
    pub date: String,
    pub state: JobState,
    /// Tries that reached the target (a link that is down is not a try).
    #[serde(default)]
    pub attempts: u32,
    /// Device clock (unix seconds) of the next try.
    #[serde(default)]
    pub next_at: i64,
    /// Device clock when queued.
    pub created_at: i64,
    /// Online time waited so far (the give-up clock).
    #[serde(default)]
    pub online_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// A fingerprint and when it was first seen.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Seen {
    pub key: String,
    pub at: i64,
}

/// The queue file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Queue {
    #[serde(default)]
    pub jobs: Vec<Job>,
    #[serde(default)]
    pub seen: Vec<Seen>,
    /// The SMS already on the device were fingerprinted (first start, or forwarding switched on).
    #[serde(default)]
    pub seeded: bool,
}

/// FNV-1a 64 over sender, date and text: stable across renumbering, the same on every build.
pub fn fingerprint(sender: &str, date: &str, content: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for part in [sender, date, content] {
        for b in part.bytes().chain([0x1f]) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

/// Wait after the `attempts`-th failed try: 30 s, 60 s, 2, 4, 8 min, then 15 min.
pub fn backoff_secs(attempts: u32) -> i64 {
    let n = attempts.max(1) - 1;
    if n >= 5 {
        return BACKOFF_CAP_SECS;
    }
    (BACKOFF_BASE_SECS << n).min(BACKOFF_CAP_SECS)
}

impl Queue {
    pub fn load(path: &Path) -> Option<Queue> {
        let text = fs::read_to_string(path).ok()?;
        Some(serde_json::from_str(&text).unwrap_or_default())
    }

    /// Saved durably; an error is the caller's to log.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = serde_json::to_vec(self).map_err(std::io::Error::other)?;
        durable_write(path, &text)
    }

    pub fn is_seen(&self, key: &str) -> bool {
        self.seen.iter().any(|s| s.key == key)
    }

    pub fn see(&mut self, key: &str, now: i64) {
        if !self.is_seen(key) {
            self.seen.push(Seen {
                key: key.to_string(),
                at: now,
            });
        }
    }

    pub fn pending(&self) -> impl Iterator<Item = &Job> {
        self.jobs.iter().filter(|j| j.state == JobState::Pending)
    }

    /// Next try times beyond `now + cap` pulled in (the device clock stepped back).
    pub fn clamp(&mut self, now: i64) {
        for j in self.jobs.iter_mut().filter(|j| j.state == JobState::Pending) {
            j.next_at = j.next_at.min(now + BACKOFF_CAP_SECS);
        }
    }

    /// Indexes of the jobs to try now: per rule the oldest pending job only if it is due
    /// (one channel's order holds; another channel never waits for it). `runnable(rule)` says
    /// whether the rule's link is up and the rule is on.
    pub fn due(&self, now: i64, runnable: &dyn Fn(u32) -> bool) -> Vec<usize> {
        let mut heads: HashSet<u32> = HashSet::new();
        let mut out = Vec::new();
        for (i, j) in self.jobs.iter().enumerate() {
            if j.state != JobState::Pending || !heads.insert(j.rule_id) {
                continue;
            }
            if runnable(j.rule_id) && j.next_at <= now {
                out.push(i);
            }
        }
        out
    }

    /// Seconds until the earliest head job that can run comes due (`None`: nothing to wait for;
    /// a channel whose link is down waits for the link-up wake instead of a timer).
    pub fn wait_secs(&self, now: i64, runnable: &dyn Fn(u32) -> bool) -> Option<i64> {
        let mut heads: HashSet<u32> = HashSet::new();
        self.jobs
            .iter()
            .filter(|j| j.state == JobState::Pending && heads.insert(j.rule_id))
            .filter(|j| runnable(j.rule_id))
            .map(|j| (j.next_at - now).clamp(0, BACKOFF_CAP_SECS))
            .min()
    }

    /// Size and age bounds. Returns the jobs given up here (pending ones dropped for size or
    /// age), for the alert.
    pub fn prune(&mut self, now: i64) -> Vec<Job> {
        let mut given_up = Vec::new();
        for j in self.jobs.iter_mut() {
            if j.state == JobState::Pending && j.online_secs >= GIVE_UP_ONLINE_SECS {
                j.state = JobState::Failed;
                j.last_error = Some(format!(
                    "{}: {}",
                    GIVE_UP_NOTE,
                    j.last_error.clone().unwrap_or_default()
                ));
                given_up.push(j.clone());
            }
        }
        self.jobs
            .retain(|j| j.state == JobState::Pending || now - j.created_at < DONE_KEEP_SECS);
        while self.jobs.len() > MAX_JOBS {
            let at = self.jobs.iter().position(|j| j.state != JobState::Pending).unwrap_or(0);
            let mut j = self.jobs.remove(at);
            if j.state == JobState::Pending {
                j.state = JobState::Failed;
                j.last_error = Some(format!("{GIVE_UP_NOTE}: queue full"));
                given_up.push(j);
            }
        }
        self.seen.retain(|s| now - s.at < SEEN_KEEP_SECS);
        if self.seen.len() > MAX_SEEN {
            let cut = self.seen.len() - MAX_SEEN;
            self.seen.drain(..cut);
        }
        given_up
    }

    /// Every job of this SMS is finished and all went out.
    pub fn all_sent(&self, key: &str) -> bool {
        let mut of = self.jobs.iter().filter(|j| j.key == key).peekable();
        of.peek().is_some() && of.all(|j| j.state == JobState::Sent)
    }

    pub fn has_pending(&self, key: &str) -> bool {
        self.pending().any(|j| j.key == key)
    }

    /// For the API: counts and the earliest next try.
    pub fn status(&self) -> serde_json::Value {
        let count = |s: JobState| self.jobs.iter().filter(|j| j.state == s).count();
        serde_json::json!({
            "pending": count(JobState::Pending),
            "sent": count(JobState::Sent),
            "failed": count(JobState::Failed),
            "next_at": self.pending().map(|j| j.next_at).min(),
            // how long the oldest pending one has waited (device clock), null when none
            "oldest_pending_secs": self.pending().map(|j| j.created_at).min().map(|t| (crate::text::now_ts() - t).max(0)),
        })
    }
}

pub(crate) const GIVE_UP_NOTE: &str = "等待超时跳过";

/// `path.tmp` written and fsynced, renamed over `path`, the directory fsynced; mode 0600.
fn durable_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    if let Some(dir) = path.parent() {
        if let Ok(d) = fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}
