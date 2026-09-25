// ─────────────────────────────────────────────────────────────────────────────
// alerts — show what went wrong while nobody was looking, and configure where
// the SMS about it goes.
//
// The agent is a reader here, not the alert service. Events are written by the
// things that notice failures — the supervise.sh wrapper when a program dies,
// u60-guard when it has to force Wi-Fi on — because the case that matters most
// is the agent itself being dead. For the same reason the only SMS sender is
// u60-guard: it is running whether or not we are, and one implementation of
// the cursor, rate limit and abroad rule is better than two that must agree.
//
// What the agent owns: the `read` cursor (web banner), the SMS number and the
// "send abroad too" switch, and the `abroad` flag (kept up to date by the
// scenario engine, so the last known location survives the agent dying).
//
// File formats and who writes what: docs/RELIABILITY.md §4. Every file the
// agent writes is written whole via rename, and nothing the agent writes is
// written by anyone else, so the agent never needs the directory's flock.
// ─────────────────────────────────────────────────────────────────────────────

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::handlers::AppState;

const DIR: &str = "/data/alerts";

/// Wall-clock seconds below this are "clock not set yet" — the RTC comes up
/// around 1971 until the network provides the time.
const CLOCK_SANE_AFTER: i64 = 1_704_067_200; // 2024-01-01

const EVENTS_SHOWN: usize = 50;
const SMS_LOG_SHOWN: usize = 10;

#[derive(Debug, PartialEq)]
struct Event {
    seq: u64,
    wall: i64,
    uptime: u64,
    kind: String,
    text: String,
}

#[derive(Debug, PartialEq)]
struct SmsRecord {
    wall: i64,
    seq: u64,
    kind: String,
    result: String,
}

fn valid_kind(k: &str) -> bool {
    !k.is_empty() && k.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Parse `queue`. Lines that do not have the exact shape are skipped: a writer
/// may be mid-append, and one bad line must not hide the rest.
fn parse_queue(text: &str) -> Vec<Event> {
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() != 5 || !valid_kind(f[3]) {
                return None;
            }
            Some(Event {
                seq: f[0].parse().ok()?,
                wall: f[1].parse().ok()?,
                uptime: f[2].parse().ok()?,
                kind: f[3].to_string(),
                text: f[4].to_string(),
            })
        })
        .collect()
}

fn parse_sms_log(text: &str) -> Vec<SmsRecord> {
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() != 4 || !valid_kind(f[2]) || f[3].is_empty() {
                return None;
            }
            Some(SmsRecord {
                wall: f[0].parse().ok()?,
                seq: f[1].parse().ok()?,
                kind: f[2].to_string(),
                result: f[3].to_string(),
            })
        })
        .collect()
}

/// `+` optional, then digits only, at most 20 characters in all. The number
/// ends up inside a JSON string built by a shell script, so this is also what
/// keeps it from being anything but a number.
fn valid_number(n: &str) -> bool {
    let digits = n.strip_prefix('+').unwrap_or(n);
    n.len() <= 20 && digits.len() >= 3 && digits.bytes().all(|b| b.is_ascii_digit())
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

struct Alerts {
    dir: PathBuf,
}

impl Alerts {
    fn at(dir: &Path) -> Self {
        Alerts { dir: dir.to_path_buf() }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn read_u64(&self, name: &str) -> u64 {
        fs::read_to_string(self.path(name))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }

    fn exists(&self, name: &str) -> bool {
        self.path(name).exists()
    }

    fn ensure_dir(&self) -> std::io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700))
    }

    fn write_whole(&self, name: &str, content: &str) -> std::io::Result<()> {
        self.ensure_dir()?;
        let tmp = self.path(&format!(".{name}.tmp"));
        fs::write(&tmp, content)?;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
        fs::rename(&tmp, self.path(name))
    }

    fn set_flag(&self, name: &str, on: bool) -> std::io::Result<()> {
        if on {
            self.write_whole(name, "")
        } else {
            match fs::remove_file(self.path(name)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            }
        }
    }

    fn events(&self) -> Vec<Event> {
        parse_queue(&fs::read_to_string(self.path("queue")).unwrap_or_default())
    }

    fn unread(&self) -> usize {
        let read = self.read_u64("read");
        self.events().iter().filter(|e| e.seq > read).count()
    }

    fn number(&self) -> Option<String> {
        fs::read_to_string(self.path("sms-to"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| valid_number(s))
    }

    fn summary(&self, now: i64, uptime_now: Option<u64>) -> Value {
        let read = self.read_u64("read");
        let events = self.events();
        let unread = events.iter().filter(|e| e.seq > read).count();
        let shown: Vec<Value> = events
            .iter()
            .rev()
            .take(EVENTS_SHOWN)
            .map(|e| {
                let clock_ok = e.wall >= CLOCK_SANE_AFTER;
                json!({
                    "seq": e.seq,
                    // null when the clock was not set; the UI then shows
                    // "N s after boot" from `uptime` instead.
                    "time": if clock_ok { Some(e.wall) } else { None },
                    "uptime": e.uptime,
                    "kind": e.kind,
                    "text": e.text,
                    "unread": e.seq > read,
                })
            })
            .collect();

        let log = parse_sms_log(&fs::read_to_string(self.path("sms-log")).unwrap_or_default());
        let clock_ok = now >= CLOCK_SANE_AFTER;
        let sent_24h = log
            .iter()
            .filter(|r| r.result == "sent" && (!clock_ok || r.wall > now - 86_400))
            .count();
        let recent: Vec<Value> = log
            .iter()
            .rev()
            .take(SMS_LOG_SHOWN)
            .map(|r| json!({"time": r.wall, "seq": r.seq, "kind": r.kind, "result": r.result}))
            .collect();

        json!({
            "events": shown,
            "unread": unread,
            "read": read,
            "uptime_now": uptime_now,
            "clock_set": clock_ok,
            "utc_offset": crate::clock::utc_offset(),
            "sms": {
                "configured": self.number().is_some(),
                "number": self.number(),
                "abroad_allowed": self.exists("sms-abroad"),
                "abroad_now": self.exists("abroad"),
                "processed": self.read_u64("sms-done"),
                "sent_24h": sent_24h,
                "recent": recent,
            },
        })
    }
}

fn alerts() -> Alerts {
    Alerts::at(Path::new(DIR))
}

fn uptime_now() -> Option<u64> {
    fs::read_to_string("/proc/uptime")
        .ok()?
        .split_whitespace()
        .next()?
        .split('.')
        .next()?
        .parse()
        .ok()
}

/// Called by the scenario engine after every successful switch. Kept on /data
/// so that u60-guard, which sends the SMS, still knows the last location after
/// the agent has died. Best effort: a failure only means SMS abroad follows the
/// previous location.
pub fn set_abroad(abroad: bool) {
    if let Err(e) = alerts().set_flag("abroad", abroad) {
        eprintln!("[alerts] could not update abroad flag: {e}");
    }
}

/// Unread count only, for the unauthenticated `/api/public/status`.
pub fn public_unread() -> usize {
    alerts().unread()
}

/// GET /api/alerts
pub fn alerts_get(_state: &AppState) -> (u16, Value) {
    (200, json!({"ok": true, "data": alerts().summary(now(), uptime_now())}))
}

/// POST /api/alerts/read — `{"seq": N}`: everything up to N has been seen.
/// Never moves backwards, never past the newest event.
pub fn alerts_read(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let Some(seq) = parsed.get("seq").and_then(|v| v.as_u64()) else {
        return (400, json!({"ok": false, "error": "expected {\"seq\": number}"}));
    };
    let a = alerts();
    let newest = a.events().last().map(|e| e.seq).unwrap_or(0);
    let read = seq.min(newest).max(a.read_u64("read"));
    match a.write_whole("read", &format!("{read}\n")) {
        Ok(()) => {
            crate::health::refresh_soon();
            (200, json!({"ok": true, "data": {"read": read}}))
        }
        Err(e) => (500, json!({"ok": false, "error": format!("write read cursor: {e}")})),
    }
}

/// PUT /api/alerts/sms — `{"number": "+86…" | "", "abroad": bool}`. Either
/// field may be omitted. An empty number turns SMS alerts off.
pub fn alerts_sms_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let a = alerts();
    if let Some(n) = parsed.get("number") {
        let Some(n) = n.as_str().map(|s| s.trim().replace([' ', '-'], "")) else {
            return (400, json!({"ok": false, "error": "number must be a string"}));
        };
        let res = if n.is_empty() {
            a.set_flag("sms-to", false)
        } else if valid_number(&n) {
            a.write_whole("sms-to", &format!("{n}\n"))
        } else {
            return (
                400,
                json!({"ok": false, "error": "number: digits only, optional leading +, at most 20 characters"}),
            );
        };
        if let Err(e) = res {
            return (500, json!({"ok": false, "error": format!("save number: {e}")}));
        }
    }
    if let Some(b) = parsed.get("abroad") {
        let Some(b) = b.as_bool() else {
            return (400, json!({"ok": false, "error": "abroad must be true or false"}));
        };
        if let Err(e) = a.set_flag("sms-abroad", b) {
            return (500, json!({"ok": false, "error": format!("save abroad switch: {e}")}));
        }
    }
    crate::health::refresh_soon();
    (200, json!({"ok": true, "data": a.summary(now(), uptime_now())["sms"].clone()}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_parser_keeps_good_lines_and_skips_the_rest() {
        let text = "1\t1758600000\t42\tagent-crash\texit 139 (SIGSEGV)\n\
                    garbage line\n\
                    2\t35392498\t7\twifi-takeover\tagent silent 300s, AP off; forcing Wi-Fi on\n\
                    3\t1758600100\t99\tBad_Kind\tx\n\
                    4\t1758600200\t120\tdatad-crash\ttoo\tmany\n\
                    5\t1758600300\t130\tsms-failed\tresult=5\n\
                    6\t17586";
        let ev = parse_queue(text);
        assert_eq!(ev.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 2, 5]);
        assert_eq!(ev[1].kind, "wifi-takeover");
        assert_eq!(ev[1].uptime, 7);
    }

    #[test]
    fn sms_log_parser() {
        let log = parse_sms_log("1758600000\t1\tagent-crash\tsent\nbad\n1758600001\t2\tx\t\n");
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].result, "sent");
    }

    #[test]
    fn numbers() {
        assert!(valid_number("+8612300000000"));
        assert!(valid_number("12300000000"));
        assert!(!valid_number(""));
        assert!(!valid_number("+"));
        assert!(!valid_number("138\"},{\"x"));
        assert!(!valid_number("+123456789012345678901"));
        assert!(!valid_number("12a45"));
    }

    fn scratch(name: &str) -> Alerts {
        let dir = std::env::temp_dir().join(format!("u60-alerts-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let a = Alerts::at(&dir);
        a.ensure_dir().unwrap();
        a
    }

    #[test]
    fn unread_follows_the_read_cursor_and_clock_is_flagged() {
        let a = scratch("unread");
        fs::write(
            a.path("queue"),
            "7\t1758600000\t42\tagent-crash\tA\n8\t35392498\t7\twifi-takeover\tB\n",
        )
        .unwrap();
        assert_eq!(a.unread(), 2);
        a.write_whole("read", "7\n").unwrap();
        assert_eq!(a.unread(), 1);

        let s = a.summary(1_758_600_500, Some(500));
        assert_eq!(s["unread"], 1);
        // Newest first; the pre-NTP event has no wall time.
        assert_eq!(s["events"][0]["seq"], 8);
        assert!(s["events"][0]["time"].is_null());
        assert_eq!(s["events"][1]["time"], 1_758_600_000i64);
        assert_eq!(s["sms"]["configured"], false);
        let _ = fs::remove_dir_all(&a.dir);
    }

    #[test]
    fn sent_in_the_last_day_counts_only_sent() {
        let a = scratch("sent");
        let now = 1_758_700_000i64;
        fs::write(
            a.path("sms-log"),
            format!(
                "{}\t1\ta\tsent\n{}\t2\tb\tsent\n{}\t3\tc\tfailed\n{}\t4\td\tsuppressed-rate\n",
                now - 90_000,
                now - 100,
                now - 50,
                now - 10
            ),
        )
        .unwrap();
        a.write_whole("sms-to", "+8612300000000\n").unwrap();
        let s = a.summary(now, None);
        assert_eq!(s["sms"]["sent_24h"], 1);
        assert_eq!(s["sms"]["configured"], true);
        assert_eq!(s["sms"]["recent"][0]["result"], "suppressed-rate");
        let _ = fs::remove_dir_all(&a.dir);
    }

    #[test]
    fn flags_toggle_and_removing_a_missing_flag_is_fine() {
        let a = scratch("flags");
        a.set_flag("abroad", false).unwrap();
        a.set_flag("abroad", true).unwrap();
        assert!(a.exists("abroad"));
        a.set_flag("abroad", false).unwrap();
        assert!(!a.exists("abroad"));
        let _ = fs::remove_dir_all(&a.dir);
    }
}
