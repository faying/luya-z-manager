//! Feed from zwrt-datad — for now only the "degraded" marker.
//!
//! When datad's `/v2` stops answering, the agent falls back to reading ubus
//! itself. The owner should hear about it if that lasts, but the agent cannot
//! be trusted to text about its own troubles, so it only leaves a marker and
//! u60-guard (touch-ui `scripts/u60-guard.sh`, `datad_round`) sends the SMS.
//!
//! Contract for `/data/u60-guard/datad-degraded` (decision R5):
//! - The agent is its ONLY writer and remover. u60-guard only reads it.
//! - Content: line 1 = epoch seconds when the fallback started, line 2 = a
//!   one-line printable-ASCII reason (<= 120 chars). Plain lines so busybox
//!   `read` can take it apart.
//! - Written on entering the fallback; rewritten (same start, new mtime) at
//!   most every [`REFRESH_EVERY`] seconds while it lasts; deleted on recovery.
//!   On startup any old marker is deleted — the previous agent's episode is
//!   not ours to vouch for — and the state is judged again from `/v2`.
//! - u60-guard alerts once per start time when mtime is < 3 min old and the
//!   start is > 5 min ago. A stale mtime means the agent stopped refreshing:
//!   that is the heartbeat alert's job, so no datad alert then.
//!
//! The state machine that decides *when* to fall back (T7) is not here yet:
//! it drives this through [`DegradedMarker::startup`], [`DegradedMarker::enter`],
//! [`DegradedMarker::tick`] and [`DegradedMarker::recover`].

// Wired up by the datad feed state machine (T7); until then only tests call it.
#![allow(dead_code)]

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Where u60-guard looks for the marker (touch-ui `u60-guard.sh` DATAD_MARKER).
pub const MARKER_PATH: &str = "/data/u60-guard/datad-degraded";
/// Rewrite the marker this often while degraded. u60-guard treats it as
/// stale after 180 s, so this leaves room for two missed refreshes.
pub const REFRESH_EVERY: u64 = 60;
const REASON_MAX: usize = 120;

pub fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The agent's side of the marker. Times are wall-clock epoch seconds passed
/// in by the caller (tests inject them; production uses [`now_epoch`]).
#[derive(Debug)]
pub struct DegradedMarker {
    path: PathBuf,
    /// `Some` while degraded: (start epoch, reason, last write epoch).
    active: Option<Active>,
}

#[derive(Debug, Clone)]
struct Active {
    start: u64,
    reason: String,
    written: u64,
}

impl DegradedMarker {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            active: None,
        }
    }

    pub fn is_degraded(&self) -> bool {
        self.active.is_some()
    }

    /// Start of the current episode, if degraded.
    pub fn since(&self) -> Option<u64> {
        self.active.as_ref().map(|a| a.start)
    }

    /// Call once at agent start, before judging `/v2` health: drops whatever
    /// a previous agent left behind. The caller (T7) then calls [`enter`]
    /// if `/v2` is unhealthy, which starts a fresh episode.
    pub fn startup(&mut self) -> io::Result<()> {
        self.active = None;
        remove_if_present(&self.path)
    }

    /// Entering the fallback. Writes the marker with `now` as the start.
    /// Already degraded: keeps the original start (the episode goes on);
    /// only the reason is updated and the marker refreshed if due.
    pub fn enter(&mut self, reason: &str, now: u64) -> io::Result<()> {
        let reason = clean_reason(reason);
        match &mut self.active {
            Some(a) => {
                a.reason = reason;
                self.tick(now).map(|_| ())
            }
            None => {
                let a = Active {
                    start: now,
                    reason,
                    written: now,
                };
                write_marker(&self.path, a.start, &a.reason)?;
                self.active = Some(a);
                Ok(())
            }
        }
    }

    /// Call every loop pass while degraded. Rewrites the marker (same start)
    /// when [`REFRESH_EVERY`] has passed since the last write, or the clock
    /// went backwards. Also recreates it if something deleted it.
    /// Returns whether it wrote. No-op when not degraded.
    pub fn tick(&mut self, now: u64) -> io::Result<bool> {
        let Some(a) = &mut self.active else {
            return Ok(false);
        };
        let due = now < a.written || now - a.written >= REFRESH_EVERY;
        if !due {
            return Ok(false);
        }
        write_marker(&self.path, a.start, &a.reason)?;
        a.written = now;
        Ok(true)
    }

    /// Back on `/v2`: delete the marker.
    pub fn recover(&mut self) -> io::Result<()> {
        self.active = None;
        remove_if_present(&self.path)
    }
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// One line of printable ASCII, at most [`REASON_MAX`] chars.
fn clean_reason(reason: &str) -> String {
    let s: String = reason
        .chars()
        .map(|c| if c == '\t' || c == '\n' || c == '\r' { ' ' } else { c })
        .filter(|c| c.is_ascii() && !c.is_ascii_control())
        .take(REASON_MAX)
        .collect();
    let s = s.trim();
    if s.is_empty() {
        "unknown".to_string()
    } else {
        s.to_string()
    }
}

/// Write via a temp file in the same directory + rename, so u60-guard never
/// reads half a marker.
fn write_marker(path: &Path, start: u64, reason: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        let mut f = fs::File::create(&tmp)?;
        write!(f, "{start}\n{reason}\n")?;
    }
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("u60-datad-feed-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn read(p: &Path) -> String {
        fs::read_to_string(p).unwrap()
    }

    fn mtime(p: &Path) -> SystemTime {
        fs::metadata(p).unwrap().modified().unwrap()
    }

    #[test]
    fn enter_writes_start_and_reason() {
        let d = dir("enter");
        let p = d.join("sub/datad-degraded"); // parent created on demand
        let mut m = DegradedMarker::new(&p);
        m.enter("v2 unreachable", 1_758_600_000).unwrap();
        assert_eq!(read(&p), "1758600000\nv2 unreachable\n");
        assert!(m.is_degraded());
        assert_eq!(m.since(), Some(1_758_600_000));
        assert!(!p.with_extension("tmp").exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn enter_again_keeps_start() {
        let d = dir("reenter");
        let p = d.join("datad-degraded");
        let mut m = DegradedMarker::new(&p);
        m.enter("first", 1000).unwrap();
        m.enter("second", 1030).unwrap(); // not due: file untouched
        assert_eq!(read(&p), "1000\nfirst\n");
        m.enter("second", 1100).unwrap(); // due: rewritten, same start
        assert_eq!(read(&p), "1000\nsecond\n");
        assert_eq!(m.since(), Some(1000));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn tick_refreshes_every_60s_with_same_start() {
        let d = dir("tick");
        let p = d.join("datad-degraded");
        let mut m = DegradedMarker::new(&p);
        assert!(!m.tick(1000).unwrap(), "not degraded: nothing to do");
        assert!(!p.exists());
        m.enter("v2 down", 1000).unwrap();
        assert!(!m.tick(1059).unwrap());
        // Backdate the file so the rewrite is visible in the mtime.
        let old = UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        fs::File::options().write(true).open(&p).unwrap().set_modified(old).unwrap();
        assert!(m.tick(1060).unwrap());
        assert!(mtime(&p) > old);
        assert_eq!(read(&p), "1000\nv2 down\n");
        assert!(!m.tick(1100).unwrap());
        assert!(m.tick(1120).unwrap());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn tick_recreates_deleted_marker() {
        let d = dir("recreate");
        let p = d.join("datad-degraded");
        let mut m = DegradedMarker::new(&p);
        m.enter("v2 down", 1000).unwrap();
        fs::remove_file(&p).unwrap();
        assert!(m.tick(1060).unwrap());
        assert_eq!(read(&p), "1000\nv2 down\n");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn tick_after_clock_went_back_rewrites() {
        let d = dir("clockback");
        let p = d.join("datad-degraded");
        let mut m = DegradedMarker::new(&p);
        m.enter("v2 down", 5000).unwrap();
        assert!(m.tick(100).unwrap(), "clock jumped back: refresh, don't stall");
        assert!(!m.tick(120).unwrap());
        assert_eq!(read(&p), "5000\nv2 down\n");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn recover_deletes_and_tolerates_missing() {
        let d = dir("recover");
        let p = d.join("datad-degraded");
        let mut m = DegradedMarker::new(&p);
        m.enter("v2 down", 1000).unwrap();
        m.recover().unwrap();
        assert!(!p.exists());
        assert!(!m.is_degraded());
        m.recover().unwrap(); // already gone: fine
        assert!(!m.tick(2000).unwrap(), "recovered: no refresh");
        assert!(!p.exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn startup_drops_old_marker() {
        let d = dir("startup");
        let p = d.join("datad-degraded");
        fs::write(&p, "123\nleft by the previous agent\n").unwrap();
        let mut m = DegradedMarker::new(&p);
        m.startup().unwrap();
        assert!(!p.exists());
        assert!(!m.is_degraded());
        m.startup().unwrap(); // nothing there: fine
        // Still unhealthy after restart: a fresh episode with a new start.
        m.enter("v2 down", 9000).unwrap();
        assert_eq!(read(&p), "9000\nv2 down\n");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn reason_is_one_line_printable_ascii_capped() {
        assert_eq!(clean_reason("a\tb\nc\r"), "a b c");
        assert_eq!(clean_reason("连接 refused \u{7}x"), "refused x");
        assert_eq!(clean_reason("  \n "), "unknown");
        assert_eq!(clean_reason(&"x".repeat(300)).len(), REASON_MAX);
    }
}
