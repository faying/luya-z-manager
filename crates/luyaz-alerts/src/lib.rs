//! The alert files under `/data/alerts` (moved from zte-agent `src/alerts.rs`, Phase 4 step 8a):
//! reading the queue and SMS log, the read cursor, the flags, appending an event the way
//! `alert-lib.sh` does, the summary the web and the touch screen show, and the kind labels.
//! zte-agent (admin web), u60d (touch compatibility layer) and luyaz-sentinel all use this one
//! copy; its own crate since Phase 5 (u60-sentinel.md D9, was `luyaz_api::alerts`, still re-exported
//! there).
//!
//! The only change from the agent: [`Alerts::summary`] takes the clock offset as an argument
//! (the agent reads it from uci; u60d the same way through its own ubus).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

pub mod sms;

pub const DIR: &str = "/data/alerts";

/// Wall-clock seconds below this are "clock not set yet" — the RTC comes up
/// around 1971 until the network provides the time.
const CLOCK_SANE_AFTER: i64 = 1_704_067_200; // 2024-01-01

const EVENTS_SHOWN: usize = 50;
/// Queue trimming, as touch-ui `scripts/alert-lib.sh` (ALERT_TRIM_AT / ALERT_KEEP).
const QUEUE_TRIM_AT: usize = 300;
const QUEUE_KEEP: usize = 200;
const SMS_LOG_SHOWN: usize = 10;

#[derive(Debug, PartialEq)]
pub struct Event {
    pub seq: u64,
    pub wall: i64,
    pub uptime: u64,
    pub kind: String,
    pub text: String,
}

#[derive(Debug, PartialEq)]
pub struct SmsRecord {
    pub wall: i64,
    pub seq: u64,
    pub kind: String,
    pub result: String,
}

/// A sentinel event id: the boot id's first 8 hex digits, `-`, a decimal sequence number.
pub fn valid_event_id(id: &str) -> bool {
    id.split_once('-').is_some_and(|(b, n)| {
        b.len() == 8
            && b.bytes().all(|c| c.is_ascii_hexdigit())
            && !n.is_empty()
            && n.len() <= 12
            && n.bytes().all(|c| c.is_ascii_digit())
    })
}

pub fn valid_kind(k: &str) -> bool {
    !k.is_empty()
        && k.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Parse `queue`. Lines that do not have the exact shape are skipped: a writer
/// may be mid-append, and one bad line must not hide the rest.
pub fn parse_queue(text: &str) -> Vec<Event> {
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

pub fn parse_sms_log(text: &str) -> Vec<SmsRecord> {
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
pub fn valid_number(n: &str) -> bool {
    let digits = n.strip_prefix('+').unwrap_or(n);
    n.len() <= 20 && digits.len() >= 3 && digits.bytes().all(|b| b.is_ascii_digit())
}

/// `PUT /api/alerts/sms` (the agent's, and u60d's since 8d-1c): `{"number": "+86…" | "",
/// "abroad": bool}`, either field may be omitted; an empty number turns SMS alerts off.
/// Err = (HTTP status, message).
pub fn set_sms(a: &Alerts, body: &[u8]) -> Result<(), (u16, String)> {
    let parsed: Value =
        serde_json::from_slice(body).map_err(|_| (400, "invalid JSON".to_string()))?;
    if let Some(n) = parsed.get("number") {
        let Some(n) = n.as_str().map(|s| s.trim().replace([' ', '-'], "")) else {
            return Err((400, "number must be a string".into()));
        };
        let res = if n.is_empty() {
            a.set_flag("sms-to", false)
        } else if valid_number(&n) {
            a.write_whole("sms-to", &format!("{n}\n"))
        } else {
            return Err((
                400,
                "number: digits only, optional leading +, at most 20 characters".into(),
            ));
        };
        res.map_err(|e| (500, format!("save number: {e}")))?;
    }
    if let Some(b) = parsed.get("abroad") {
        let Some(b) = b.as_bool() else {
            return Err((400, "abroad must be true or false".into()));
        };
        a.set_flag("sms-abroad", b)
            .map_err(|e| (500, format!("save abroad switch: {e}")))?;
    }
    Ok(())
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub struct Alerts {
    pub dir: PathBuf,
}

impl Alerts {
    pub fn at(dir: &Path) -> Self {
        Alerts {
            dir: dir.to_path_buf(),
        }
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    pub fn read_u64(&self, name: &str) -> u64 {
        fs::read_to_string(self.path(name))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }

    pub fn exists(&self, name: &str) -> bool {
        self.path(name).exists()
    }

    /// The directory, 0700. Touches nothing when it is already so: a chmod updates the inode's
    /// ctime even when the mode does not change, and callers run this every round (flash writes).
    pub fn ensure_dir(&self) -> std::io::Result<()> {
        if let Ok(m) = fs::metadata(&self.dir)
            && m.is_dir()
            && m.permissions().mode() & 0o7777 == 0o700
        {
            return Ok(());
        }
        fs::create_dir_all(&self.dir)?;
        fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700))
    }

    pub fn write_whole(&self, name: &str, content: &str) -> std::io::Result<()> {
        self.ensure_dir()?;
        let tmp = self.path(&format!(".{name}.tmp"));
        fs::write(&tmp, content)?;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
        fs::rename(&tmp, self.path(name))
    }

    pub fn set_flag(&self, name: &str, on: bool) -> std::io::Result<()> {
        if on {
            self.write_whole(name, "")
        } else {
            match fs::remove_file(self.path(name)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            }
        }
    }

    /// Append one event the way touch-ui `scripts/alert-lib.sh` does
    /// (RELIABILITY.md §4): under `lock`, bump `seq`, append, trim past
    /// 300 lines to the last 200. `text` is cleaned to printable ASCII, 120
    /// chars. u60-guard picks it up for the SMS like any other kind.
    pub fn add(&self, kind: &str, text: &str, wall: i64, uptime: u64) -> std::io::Result<u64> {
        if !valid_kind(kind) {
            return Err(std::io::Error::other("bad kind"));
        }
        let _lock = self.flock()?;
        self.append_locked(kind, text, wall, uptime)
    }

    /// An event with an id (the sentinel's, u60-sentinel.md §6 C-R9): queued once whoever adds it
    /// first, the core on the sentinel's POST or the sentinel itself when the core did not answer.
    /// The text starts with `[<id>] `. Returns the queue seq and whether this call added it.
    pub fn add_once(
        &self,
        id: &str,
        kind: &str,
        text: &str,
        wall: i64,
        uptime: u64,
    ) -> std::io::Result<(u64, bool)> {
        if !valid_kind(kind) || !valid_event_id(id) {
            return Err(std::io::Error::other("bad kind or id"));
        }
        let _lock = self.flock()?;
        if let Some(seq) = self.find_event(id) {
            return Ok((seq, false));
        }
        Ok((
            self.append_locked(kind, &format!("[{id}] {text}"), wall, uptime)?,
            true,
        ))
    }

    /// The queue seq of the event with this id, if it is (still) in the queue.
    pub fn find_event(&self, id: &str) -> Option<u64> {
        let tag = format!("[{id}] ");
        self.events()
            .into_iter()
            .find(|e| e.text.starts_with(&tag))
            .map(|e| e.seq)
    }

    fn flock(&self) -> std::io::Result<fs::File> {
        use std::os::unix::io::AsRawFd;
        self.ensure_dir()?;
        let lock = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path("lock"))?;
        // SAFETY: flock on an open file descriptor.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(lock)
    }

    fn append_locked(
        &self,
        kind: &str,
        text: &str,
        wall: i64,
        uptime: u64,
    ) -> std::io::Result<u64> {
        use std::io::Write;
        let text: String = text
            .chars()
            .filter(|c| c.is_ascii() && !c.is_ascii_control())
            .take(120)
            .collect();
        let seq = self.read_u64("seq") + 1;
        self.write_whole("seq", &format!("{seq}\n"))?;
        let mut q = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path("queue"))?;
        writeln!(q, "{seq}\t{wall}\t{uptime}\t{kind}\t{text}")?;
        drop(q);
        let all = fs::read_to_string(self.path("queue")).unwrap_or_default();
        let lines: Vec<&str> = all.lines().collect();
        if lines.len() > QUEUE_TRIM_AT {
            let mut kept = lines[lines.len() - QUEUE_KEEP..].join("\n");
            kept.push('\n');
            self.write_whole("queue", &kept)?;
        }
        Ok(seq)
    }

    pub fn events(&self) -> Vec<Event> {
        parse_queue(&fs::read_to_string(self.path("queue")).unwrap_or_default())
    }

    pub fn unread(&self) -> usize {
        let read = self.read_u64("read");
        self.events().iter().filter(|e| e.seq > read).count()
    }

    pub fn number(&self) -> Option<String> {
        fs::read_to_string(self.path("sms-to"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| valid_number(s))
    }

    pub fn summary(&self, now: i64, uptime_now: Option<u64>, utc_offset: i64) -> Value {
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
                    // the touch screen's wording (it no longer keeps its own table)
                    "label": kind_label(&e.kind),
                    "label_en": kind_label_en(&e.kind),
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
            .map(|r| {
                json!({
                    "time": r.wall, "seq": r.seq, "kind": r.kind, "result": r.result,
                    "label": kind_label(&r.kind), "label_en": kind_label_en(&r.kind),
                })
            })
            .collect();

        json!({
            "events": shown,
            "unread": unread,
            "read": read,
            "uptime_now": uptime_now,
            "clock_set": clock_ok,
            "utc_offset": utc_offset,
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

/// What each kind means, as the touch screen words it (moved from touch-ui
/// alerts.c, 2026-09-26). The admin web shows these too (with
/// [`kind_label_en`]); it no longer keeps its own copy (L2 review R8).
pub fn kind_label(kind: &str) -> String {
    const T: &[(&str, &str)] = &[
        ("agent-crash", "管理后台意外退出，已自动重启"),
        ("agent-silent", "管理后台失去响应"),
        ("agent-hung", "管理后台卡死，已被强制重启"),
        ("datad-crash", "数据服务意外退出，已自动重启"),
        ("devui-crash", "触屏界面闪退，已自动重新打开"),
        // Phase 10 X9: the core watches these two procd services
        ("web-crash", "管理网页意外退出，已自动重启"),
        ("sentinel-crash", "哨兵意外退出，已自动重启"),
        (
            "devui-gave-up",
            "触屏界面反复打不开，已换回原厂界面（长按屏幕右下角 3 秒重试）",
        ),
        ("devui-theme-paused", "自动切换深浅色已暂停，重启后恢复"),
        ("wifi-takeover", "Wi-Fi 看门狗重新打开了 Wi-Fi"),
        ("wifi-restore-failed", "Wi-Fi 看门狗没能打开 Wi-Fi"),
        ("sms-failed", "告警短信发送失败"),
        ("sms-test", "测试短信"),
        ("datad-degraded", "数据服务持续没响应，后台改为直接读取"),
        // luyaz-sentinel (u60-sentinel.md Phase 5)
        (
            "screen-two-uis",
            "触屏界面刚打开时原厂界面也起来了，已关掉触屏界面、保留原厂界面",
        ),
        ("screen-hold-expired", "上机时暂管屏幕超时，哨兵已收回屏幕"),
        ("dropbear-restarted", "SSH（:2222）没在听，已重新打开"),
        (
            "sentinel-silent",
            "哨兵超过 150 秒没心跳（屏幕照常，只告警）",
        ),
        (
            "screen-empty",
            "屏幕上 1 分钟没有任何界面，核心已打开原厂界面",
        ),
        (
            "dropbear-down",
            "SSH（:2222）一小时内反复掉线，已到重开上限",
        ),
        (
            "home-exit-down",
            "家里出口 15 分钟没回应，「其余都走家里」已关",
        ),
        ("home-exit-check-failed", "家里出口打开后自检没过，已关回去"),
    ];
    T.iter()
        .find(|t| t.0 == kind)
        .map_or_else(|| format!("其他告警（{kind}）"), |t| t.1.to_string())
}

/// English for [`kind_label`]. Taken from the admin web's labels
/// (web/src/lib/alerts.ts kindLabel), with the glossary's name for the touch
/// screen (docs/ui-glossary.md §3: Screen UI): the web is to read these
/// instead of keeping its own copy (L2 review R8).
pub fn kind_label_en(kind: &str) -> String {
    const T: &[(&str, &str)] = &[
        (
            "agent-crash",
            "Admin backend exited unexpectedly",
        ),
        (
            "agent-silent",
            "Admin backend stopped responding",
        ),
        ("agent-hung", "Admin backend was stuck and was restarted"),
        (
            "datad-crash",
            "Data service exited unexpectedly",
        ),
        ("devui-crash", "Screen UI exited unexpectedly"),
        ("web-crash", "Admin pages (luyaz-web) exited unexpectedly"),
        (
            "sentinel-crash",
            "Sentinel (luyaz-sentinel) exited unexpectedly",
        ),
        (
            "devui-gave-up",
            "Screen UI kept failing; stock UI is on screen (hold the bottom-right corner 3 s to retry)",
        ),
        (
            "devui-theme-paused",
            "Automatic light/dark switching on the screen paused until reboot",
        ),
        ("wifi-takeover", "Wi-Fi watchdog turned Wi-Fi back on"),
        (
            "wifi-restore-failed",
            "Wi-Fi watchdog could not turn Wi-Fi on",
        ),
        ("sms-failed", "Alert SMS could not be sent"),
        ("sms-test", "Test SMS"),
        (
            "datad-degraded",
            "Data service not answering; admin backend reads the modem directly",
        ),
        // luyaz-sentinel (u60-sentinel.md Phase 5)
        (
            "screen-two-uis",
            "Stock UI started next to the screen UI; screen UI stopped, stock UI kept",
        ),
        (
            "screen-hold-expired",
            "Screen held for an update past its deadline; the sentinel took it back",
        ),
        (
            "dropbear-restarted",
            "SSH (:2222) was not listening; started it again",
        ),
        (
            "sentinel-silent",
            "Sentinel heartbeat older than 150 s (screen untouched; alert only)",
        ),
        (
            "screen-empty",
            "No UI on screen for 1 min; the core started the stock UI",
        ),
        (
            "dropbear-down",
            "SSH (:2222) keeps going away; restart limit reached",
        ),
        (
            "home-exit-down",
            "Home exit silent 15 min; \"Rest via home\" turned off",
        ),
        (
            "home-exit-check-failed",
            "Home exit failed its check after turning on; turned back off",
        ),
    ];
    T.iter()
        .find(|t| t.0 == kind)
        .map_or_else(|| format!("Other alert ({kind})"), |t| t.1.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_event_id_is_queued_once() {
        use super::*;
        let d = std::env::temp_dir().join(format!("u60-alerts-once-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        let a = Alerts::at(&d);
        assert!(
            valid_event_id("8559f5dc-17")
                && !valid_event_id("8559f5d-1")
                && !valid_event_id("8559f5dc-")
        );
        assert!(a.add_once("x", "devui-gave-up", "t", 1, 1).is_err());
        let (s1, new1) = a
            .add_once("8559f5dc-1", "devui-gave-up", "gave up", 100, 5)
            .unwrap();
        let (s2, new2) = a
            .add_once("8559f5dc-1", "devui-gave-up", "gave up", 101, 6)
            .unwrap();
        assert_eq!((s1, new1, s2, new2), (1, true, 1, false));
        assert_eq!(a.events()[0].text, "[8559f5dc-1] gave up");
        assert_eq!(a.find_event("8559f5dc-1"), Some(1));
        assert_eq!(a.find_event("8559f5dc-2"), None);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn kind_labels_as_the_screen_had_them() {
        use super::kind_label;
        assert_eq!(kind_label("agent-crash"), "管理后台意外退出，已自动重启");
        assert_eq!(
            kind_label("devui-gave-up"),
            "触屏界面反复打不开，已换回原厂界面（长按屏幕右下角 3 秒重试）"
        );
        assert_eq!(kind_label("sms-test"), "测试短信");
        // the screen used to fall back to the raw kind for this one
        assert_eq!(
            kind_label("datad-degraded"),
            "数据服务持续没响应，后台改为直接读取"
        );
        assert_eq!(kind_label("something-new"), "其他告警（something-new）");
        assert_eq!(kind_label(""), "其他告警（）");
    }

    use super::*;

    #[test]
    fn every_kind_has_english_without_cjk() {
        use super::{kind_label, kind_label_en};

        let kinds = [
            "agent-crash",
            "agent-silent",
            "agent-hung",
            "datad-crash",
            "devui-crash",
            "web-crash",
            "sentinel-crash",
            "devui-gave-up",
            "devui-theme-paused",
            "wifi-takeover",
            "wifi-restore-failed",
            "sms-failed",
            "sms-test",
            "datad-degraded",
        ];
        for k in kinds {
            let en = kind_label_en(k);
            assert!(
                !kind_label(k).starts_with("其他告警"),
                "{k} has no Chinese label"
            );
            assert!(!en.starts_with("Other alert"), "{k} has no English label");
            assert!(
                !has_cjk(&en) && !en.ends_with('.') && !en.contains("Please"),
                "{k}: {en}"
            );
        }
        assert_eq!(
            kind_label_en("agent-crash"),
            "Admin backend exited unexpectedly"
        );
        assert_eq!(
            kind_label_en("something-new"),
            "Other alert (something-new)"
        );
    }

    /// The touch screen reads /api/alerts into a 32 KB buffer (touch-ui
    /// alerts.c AL_RESP_MAX) and truncates silently; 50 events of the longest
    /// label with a full 120-character text must still fit with room to spare.
    #[test]
    fn fifty_longest_events_fit_the_screen_buffer() {
        let a = scratch("size");
        let text = "x".repeat(120);
        let mut q = String::new();
        for i in 1..=60u64 {
            q.push_str(&format!(
                "{i}\t1758600000\t4294967295\tdevui-gave-up\t{text}\n"
            ));
        }
        fs::write(a.path("queue"), q).unwrap();
        a.write_whole("sms-to", "+12345678901234567890\n").unwrap();
        let sms: String = (1..=20u64)
            .map(|i| format!("1758600000\t{i}\tdevui-gave-up\tsuppressed-abroad\n"))
            .collect();
        fs::write(a.path("sms-log"), sms).unwrap();
        let body =
            json!({"ok": true, "data": a.summary(1_758_700_000, Some(1), 28800)}).to_string();
        assert_eq!(
            a.summary(1_758_700_000, Some(1), 28800)["events"]
                .as_array()
                .unwrap()
                .len(),
            EVENTS_SHOWN
        );
        let before = body.matches(r#","label_en":""#).count();
        let en_bytes: usize = body
            .split(r#""label_en":""#)
            .skip(1)
            .map(|r| r.find('"').unwrap() + 14)
            .sum();
        eprintln!(
            "alerts: {} bytes before L2, {} after ({before} events)",
            body.len() - en_bytes,
            body.len()
        );
        assert!(body.len() < 24 * 1024, "{} bytes", body.len());
        let _ = fs::remove_dir_all(&a.dir);
    }

    #[test]
    fn add_appends_with_seq_and_trims() {
        let d = std::env::temp_dir().join(format!("u60-alerts-add-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        let a = Alerts::at(&d);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("seq"), "41\n").unwrap();
        assert_eq!(
            a.add("sms-forward-stuck", "SMS 7\theld\u{4e2d}", 1_758_600_000, 5)
                .unwrap(),
            42
        );
        let ev = a.events();
        assert_eq!(ev.len(), 1);
        assert_eq!(
            (ev[0].seq, ev[0].kind.as_str(), ev[0].text.as_str()),
            (42, "sms-forward-stuck", "SMS 7held")
        );
        assert!(a.add("Bad Kind", "x", 0, 0).is_err());
        for _ in 0..300 {
            a.add("sms-forward-stuck", "x", 0, 0).unwrap();
        }
        let ev = a.events();
        assert!(ev.len() <= QUEUE_TRIM_AT && ev.len() >= QUEUE_KEEP);
        assert_eq!(ev.last().unwrap().seq, 342);
        let _ = fs::remove_dir_all(&d);
    }

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

        let s = a.summary(1_758_600_500, Some(500), 28800);
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
        let s = a.summary(now, None, 28800);
        assert_eq!(s["sms"]["sent_24h"], 1);
        assert_eq!(s["sms"]["configured"], true);
        assert_eq!(s["sms"]["recent"][0]["result"], "suppressed-rate");
        // the web shows the record by these, not by a table of its own
        assert_eq!(s["sms"]["recent"][0]["label"], kind_label("d"));
        assert_eq!(s["sms"]["recent"][0]["label_en"], "Other alert (d)");
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

    /// zte-agent `health.rs`'s.
    fn has_cjk(s: &str) -> bool {
        s.chars().any(|c| {
            matches!(c as u32,
                0x2E80..=0x2FFF | 0x3000..=0x303F | 0x3040..=0x30FF | 0x3100..=0x9FFF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF)
        })
    }

    #[test]
    fn summary_carries_the_offset_it_is_given() {
        let a = scratch("offset");
        assert_eq!(a.summary(1_758_600_500, None, 19800)["utc_offset"], 19800);
        let _ = fs::remove_dir_all(&a.dir);
    }
}
