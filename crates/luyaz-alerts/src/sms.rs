//! Alert SMS: the rules, the texts and the send states (u60-sentinel.md §6, D9, D15), moved from
//! touch-ui `scripts/u60-guard.sh` (`sms_round` and its helpers). Two senders use this one copy:
//! the core (normally) and the sentinel (only while the core is unhealthy); until the core takes
//! the `sms` duty over, the guard keeps sending with its shell copy, so the files stay the guard's:
//!
//! - `sms-done`: the highest queue seq that is finished (sent, failed, suppressed, skipped).
//! - `sms-log`: `wall \t seq \t kind \t result`, last 100 lines (the web shows the last 10).
//! - `sms-sending` (new): the one alert being sent right now —
//!   `seq \t sender \t pid \t boot_id \t uptime \t wall`. pending = seq above `sms-done` with no
//!   claim; sending = this file; sent/failed = `sms-log` and the cursor past it.
//!
//! Claims and finishes happen under `lock` (the flock `add` uses); the send itself does not hold
//! it. A claim from another boot, older than [`UNCERTAIN_AFTER_S`], or whose process is gone is
//! "uncertain": the next sender sends that alert again (D15: a duplicate over a lost alert), and
//! every SMS ends in `#<seq>` (last three digits) so a duplicate is recognisable. The cursor and the log only move
//! after the send returned, so a sender dying mid-send never loses an alert.

use std::fs;
use std::io::Write;
use std::os::unix::io::AsRawFd;

use crate::{Alerts, parse_queue, parse_sms_log, valid_number};

/// A `sending` claim this old is taken as "the sender died mid-send".
pub const UNCERTAIN_AFTER_S: u64 = 120;
/// At most one sent SMS per kind per hour, five per 24 h (guard `rate_ok`).
const RATE_KIND_S: i64 = 3600;
const RATE_DAY_S: i64 = 86400;
const RATE_DAY_MAX: usize = 5;
const SMS_LOG_KEEP: usize = 100;
/// Kinds never sent as an SMS: a failed SMS, the screen's colours, what the agent changed (MCP
/// second phase: the agent reads it back, the owner's alert forwarding shows it), the tailnet
/// poller (owner 10-21: web and Hermes only).
const NEVER: &[&str] = &["sms-failed", "devui-theme-paused", "agent-change", "tailnet-poller"];

/// What a sender needs from the device. The core implements `send` through its reconciler, the
/// sentinel straight over ubus.
pub trait SmsIo {
    /// Wall clock (device local time marked UTC, as the alert queue stores it).
    fn wall(&mut self) -> i64;
    /// Seconds since boot (`/proc/uptime`).
    fn uptime(&mut self) -> u64;
    fn boot_id(&mut self) -> String;
    fn pid_alive(&mut self, pid: u32) -> bool;
    /// Fresh, same-boot wall estimate with no observed step, as the sentinel computes it.
    /// This preserves local-wall rate limiting; it does not mean verified UTC/NTP.
    fn clock_ok(&mut self) -> bool;
    /// The screen's language is English (`lang=en`, last line, in devui.conf).
    fn lang_en(&mut self) -> bool;
    /// ZTE's SNTP zone in whole hours (`zwrt_zte_sntp.settings.time_from_utc`).
    fn tz_hours(&mut self) -> i32;
    /// `zwrt_wms zte_libwms_send_sms` with these args ([`send_args`]); Ok when the device took it.
    fn send(&mut self, args: &serde_json::Value) -> Result<(), SendError>;
    /// Delete the copy the firmware keeps in its sent box (the newest to `number`).
    fn delete_sent_copy(&mut self, number: &str);
    fn log(&mut self, msg: &str);
}

/// Why a send did not work.
#[derive(Debug, PartialEq)]
pub enum SendError {
    /// The device answered and refused it (a result code, a failed ubus call): logged `failed`,
    /// the cursor moves on, `sms-failed` is raised.
    Rejected(String),
    /// It never reached the device, or we cannot tell (busy, no answer, timed out): the alert
    /// stays pending and goes again next round (D15: a duplicate over a lost alert).
    NotSent(String),
}

/// What the rules say about one pending alert.
#[derive(Debug, PartialEq)]
pub enum Verdict {
    /// Done without a trace (kinds never sent).
    Skip,
    /// Done, logged with this result, nothing sent.
    Record(&'static str),
    /// Leave it and everything after it pending (no trusted clock yet: rate limits need one).
    Hold,
    Send,
}

pub struct Facts<'a> {
    pub kind: &'a str,
    pub number: Option<&'a str>,
    pub abroad: bool,
    pub sms_abroad: bool,
    pub clock_ok: bool,
    pub rate_ok: bool,
}

/// guard `sms_round`, in its order.
pub fn verdict(f: &Facts) -> Verdict {
    if NEVER.contains(&f.kind) {
        Verdict::Skip
    } else if !f.number.is_some_and(valid_number) {
        Verdict::Record("no-number")
    } else if f.abroad && !f.sms_abroad {
        Verdict::Record("suppressed-abroad")
    } else if !f.clock_ok {
        Verdict::Hold
    } else if !f.rate_ok {
        Verdict::Record("suppressed-rate")
    } else {
        Verdict::Send
    }
}

/// guard `rate_ok`: counts only what was sent; a record stamped in the future (the clock jumped
/// back) counts as just sent.
pub fn rate_ok(log: &str, kind: &str, now: i64) -> bool {
    let mut day = 0;
    let mut hour = 0;
    for r in parse_sms_log(log) {
        if r.result != "sent" {
            continue;
        }
        if r.wall > now || r.wall > now - RATE_DAY_S {
            day += 1;
        }
        if r.kind == kind && (r.wall > now || r.wall > now - RATE_KIND_S) {
            hour += 1;
        }
    }
    hour < 1 && day < RATE_DAY_MAX
}

// ── texts ───────────────────────────────────────────────────────────────────

/// What the owner reads on the phone, in Chinese (guard `sms_body`, word for word): what happened,
/// whether it affects getting online, whether to do anything. One SMS (≤ 70 chars with the time
/// and the short code).
fn body_zh(kind: &str) -> String {
    let m = match kind {
        "wifi-takeover" => "管理后台没反应了。为了让你能上网，已自动打开U60的Wi-Fi。不用管。",
        "wifi-restore-failed" => {
            "想自动打开U60的Wi-Fi没成功，还在重试。如果手机连不上U60，请重启它。"
        }
        "core-crash" => "屏幕的数据服务意外退出，已自动重启。不用管。",
        "devui-crash" => "屏幕界面闪退了，已自动重新打开。不用管。",
        "devui-gave-up" => {
            "屏幕界面连续打不开，已换成原厂界面，上网不受影响。长按屏幕右下角3秒可换回。"
        }
        "sms-test" => "这是测试短信。收到了，说明告警短信能正常发到你手机。",
        // luyaz-sentinel (Phase 5): new in this copy, not in the guard's
        "screen-two-uis" => "屏幕上原厂界面和我们的界面同时起来了，已留下原厂界面，稍后再换回。",
        "screen-hold-expired" => "更新屏幕时暂管屏幕超时，已自动收回。不用管。",
        "dropbear-restarted" => "SSH远程登录停了，已自动重新打开。不用管。",
        "dropbear-down" => "SSH远程登录反复停掉，这一小时已不再重开。上网不受影响。",
        "sentinel-silent" => "U60的看门程序超过2分钟没反应。屏幕和上网一般不受影响。",
        "screen-empty" => "屏幕上1分钟没有界面，已打开原厂界面。长按右下角3秒可换回。",
        // Phase 10 X9: the core's crash watch
        "web-crash" => "管理网页意外退出，已自动重启。不用管。",
        "sentinel-crash" => "U60的看门程序意外退出，已自动重启。不用管。",
        _ => return format!("有新告警（{kind}），请到管理网页「系统→告警」查看。"),
    };
    m.to_string()
}

/// The same in English (guard `sms_body_en`; docs/ui-glossary.md §9): printable ASCII only. Four
/// are a few words shorter than the guard's so the short code still fits one SMS
/// (wifi-restore-failed, core-crash, devui-gave-up, dropbear-down).
fn body_en(kind: &str) -> String {
    let m = match kind {
        "wifi-takeover" => "Admin down; Wi-Fi turned on; no action",
        "wifi-restore-failed" => "Wi-Fi won't start; retrying; stuck? Reboot",
        "core-crash" => "Data service crashed; restarted; no action",
        "devui-crash" => "Screen UI crashed; reopened; no action",
        "devui-gave-up" => "Stock UI on; hold bottom-right 3s to undo",
        "sms-test" => "Test SMS: alert texts reach your phone",
        "screen-two-uis" => "Two screen UIs; stock UI kept; retry later",
        "screen-hold-expired" => "Screen hold expired; taken back; no action",
        "dropbear-restarted" => "SSH was down; restarted; no action needed",
        "dropbear-down" => "SSH keeps dropping; restarts paused 1 hour",
        "sentinel-silent" => "Watchdog silent 2+ min; screen and net OK",
        "screen-empty" => "Screen blank; stock UI on; hold corner 3s",
        "web-crash" => "Admin pages crashed; restarted; no action",
        "sentinel-crash" => "Watchdog crashed; restarted; no action",
        // a kind is ASCII by alert-lib's rules; kept to 17 safe chars so the SMS stays one
        // message whatever comes in
        _ => {
            let k: String = kind
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || "_.-".contains(*c))
                .take(17)
                .collect();
            return format!("Alert {k}; see Alerts on web");
        }
    };
    m.to_string()
}

/// The SMS for one alert: language, the device-local time now, and the short code `#<seq>`
/// (D15: the guard's text, unchanged, plus the code at the end). The code is the last three digits
/// of the queue seq: enough to tell a resent duplicate from a new alert, and the longest English
/// text stays one 70-character SMS. A device name (`sms-name`, 10-20: two U60s alerting one
/// phone) goes after the signature, `【Luya-Z】名字：…`; the longer texts then go out as a long
/// SMS (UCS-2 over 70 characters: the firmware splits it, the phone shows one; CN B31, 10-20).
pub fn sms_text(kind: &str, en: bool, wall: i64, seq: u64, name: Option<&str>) -> String {
    let (_, mo, d, h, mi, _) = civil(wall);
    let seq = seq % 1000;
    let (name_zh, name_en) = match name {
        Some(n) => (format!("{n}："), format!("{n}: ")),
        None => (String::new(), String::new()),
    };
    if en {
        const MON: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        format!(
            "[Luya-Z] {name_en}{} ({d:02} {} {h:02}:{mi:02})#{seq}",
            body_en(kind),
            MON[(mo - 1) as usize]
        )
    } else {
        format!(
            "【Luya-Z】{name_zh}{}（{mo:02}-{d:02} {h:02}:{mi:02}）#{seq}",
            body_zh(kind)
        )
    }
}

/// UTF-8 → UCS-2 big-endian hex, uppercase (guard `ucs2_hex`, as sms_forward.rs encodes);
/// characters outside the BMP become U+FFFD.
pub fn ucs2_hex(s: &str) -> String {
    s.chars()
        .map(|c| {
            let cp = c as u32;
            format!("{:04X}", if cp > 0xFFFF { 0xFFFD } else { cp })
        })
        .collect()
}

/// ZTE's `YY;MM;DD;HH;MM;SS;+TZ`; the wall clock already reads local time (guard `sms_time`).
pub fn sms_time(wall: i64, tz_hours: i32) -> String {
    let (y, mo, d, h, mi, s) = civil(wall);
    let sign = if tz_hours < 0 { '-' } else { '+' };
    format!(
        "{:02};{mo:02};{d:02};{h:02};{mi:02};{s:02};{sign}{}",
        y % 100,
        tz_hours.abs()
    )
}

/// The `zte_libwms_send_sms` arguments (guard `send_sms`).
pub fn send_args(number: &str, text: &str, wall: i64, tz_hours: i32) -> serde_json::Value {
    serde_json::json!({
        "number": number,
        "message_body": ucs2_hex(text),
        "encode_type": "UNICODE",
        "sms_time": sms_time(wall, tz_hours),
        "id": "-1",
    })
}

/// The reply of `zte_libwms_send_sms`: missing or 3 is success (guard `send_sms`).
pub fn send_result_ok(result: Option<&serde_json::Value>) -> bool {
    match result {
        None | Some(serde_json::Value::Null) => true,
        Some(v) => v.as_i64() == Some(3) || v.as_str() == Some("3") || v.as_str() == Some(""),
    }
}

/// The screen's language from devui.conf: English only when the last `lang=` line is exactly
/// `lang=en` (guard `sms_lang`, touch-ui `lang_parse`). Split on `\n` only, so `lang=en\r` stays
/// Chinese as it does on the screen.
pub fn lang_en(conf: &str) -> bool {
    conf.split('\n')
        .filter_map(|l| l.strip_prefix("lang="))
        .next_back()
        .is_some_and(|v| v == "en")
}

/// ZTE SNTP's `time_from_utc` → whole hours (guard `sms_time`): `"8.00"` → 8, `"-5.30"` → -5,
/// anything else 0.
pub fn tz_hours(v: &str) -> i32 {
    let v = v.trim();
    let (neg, n) = v.strip_prefix('-').map_or((false, v), |r| (true, r));
    let whole = n.split('.').next().unwrap_or("");
    match whole.parse::<i32>() {
        Ok(h) if whole.bytes().all(|b| b.is_ascii_digit()) => {
            if neg {
                -h
            } else {
                h
            }
        }
        _ => 0,
    }
}

/// The id of the newest message to `number` in a `zte_libwms_get_sms_data` reply asked for with
/// `order by id desc` (the first match is the newest). Only plain non-negative whole ids.
pub fn newest_to(list: &serde_json::Value, number: &str) -> Option<String> {
    list["messages"]
        .as_array()?
        .iter()
        .find(|m| m["number"].as_str() == Some(number))
        .and_then(|m| match &m["id"] {
            serde_json::Value::String(s)
                if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) =>
            {
                Some(s.clone())
            }
            serde_json::Value::Number(n) => n.as_u64().map(|n| n.to_string()),
            _ => None,
        })
}

/// Seconds since 1970 → (year, month, day, hour, minute, second), proleptic Gregorian.
fn civil(t: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = t.div_euclid(86400);
    let rem = t.rem_euclid(86400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m, d, rem / 3600, rem % 3600 / 60, rem % 60)
}

// ── the send states ─────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Clone)]
pub struct Claim {
    pub seq: u64,
    pub sender: String,
    pub pid: u32,
    pub boot_id: String,
    pub uptime: u64,
    pub wall: i64,
}

pub fn parse_claim(text: &str) -> Option<Claim> {
    let f: Vec<&str> = text.trim_end().split('\t').collect();
    if f.len() != 6 {
        return None;
    }
    Some(Claim {
        seq: f[0].parse().ok()?,
        sender: f[1].to_string(),
        pid: f[2].parse().ok()?,
        boot_id: f[3].to_string(),
        uptime: f[4].parse().ok()?,
        wall: f[5].parse().ok()?,
    })
}

/// What one sender's round did.
#[derive(Debug, PartialEq)]
pub enum Round {
    /// Nothing pending (or no queue).
    Idle,
    /// Another sender has a fresh claim; try again next round.
    Busy(String),
    /// Pending alerts wait for a trusted clock.
    Held,
    /// This many alerts finished (sent, failed, recorded or skipped).
    Done(usize),
}

enum Next {
    Busy(String),
    Idle,
    /// The claim is ours now; `resend` = it was another sender's uncertain claim.
    Go(crate::Event, bool),
}

/// Holds the flock until dropped.
struct Locked {
    _f: fs::File,
}

impl Alerts {
    fn lock(&self) -> std::io::Result<Locked> {
        self.ensure_dir()?;
        let f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path("lock"))?;
        // SAFETY: flock on an open file descriptor.
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Locked { _f: f })
    }

    /// The claim now in `sms-sending`, if any.
    pub fn sms_claim(&self) -> Option<Claim> {
        parse_claim(&fs::read_to_string(self.path("sms-sending")).ok()?)
    }

    fn claim_next(&self, sender: &str, pid: u32, io: &mut dyn SmsIo) -> std::io::Result<Next> {
        let _l = self.lock()?;
        let done = self.read_u64("sms-done");
        let boot = io.boot_id();
        let up = io.uptime();
        let mut resend = false;
        if let Some(c) = self.sms_claim() {
            let same_boot = c.boot_id == boot;
            let fresh = same_boot
                && up >= c.uptime
                && up - c.uptime < UNCERTAIN_AFTER_S
                && io.pid_alive(c.pid);
            if fresh && !(c.sender == sender && c.pid == pid) {
                return Ok(Next::Busy(c.sender));
            }
            // uncertain (or our own leftover): sent again below if still above the cursor
            resend = c.seq > done;
            if resend {
                io.log(&format!(
                    "sms: alert {} was being sent by {} (pid {}) {}; sending it again",
                    c.seq,
                    c.sender,
                    c.pid,
                    if !same_boot {
                        "before a reboot".to_string()
                    } else {
                        format!("{} s ago", up.saturating_sub(c.uptime))
                    }
                ));
            }
        }
        let queue = fs::read_to_string(self.path("queue")).unwrap_or_default();
        let Some(ev) = parse_queue(&queue).into_iter().find(|e| e.seq > done) else {
            let _ = fs::remove_file(self.path("sms-sending"));
            return Ok(Next::Idle);
        };
        let wall = io.wall();
        self.write_whole(
            "sms-sending",
            &format!("{}\t{sender}\t{pid}\t{boot}\t{up}\t{wall}\n", ev.seq),
        )?;
        Ok(Next::Go(ev, resend))
    }

    /// Move the cursor past `seq` and log `result` (None: skipped without a trace), only if the
    /// claim is still ours (another sender may have taken an uncertain claim over meanwhile).
    fn finish(
        &self,
        seq: u64,
        kind: &str,
        result: Option<&str>,
        sender: &str,
        pid: u32,
        wall: i64,
    ) -> std::io::Result<bool> {
        let _l = self.lock()?;
        let ours = self
            .sms_claim()
            .is_some_and(|c| c.seq == seq && c.sender == sender && c.pid == pid);
        if !ours || self.read_u64("sms-done") >= seq {
            return Ok(false);
        }
        if let Some(r) = result {
            let mut f = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.path("sms-log"))?;
            writeln!(f, "{wall}\t{seq}\t{kind}\t{r}")?;
            drop(f);
            let all = fs::read_to_string(self.path("sms-log")).unwrap_or_default();
            let lines: Vec<&str> = all.lines().collect();
            if lines.len() > SMS_LOG_KEEP {
                let mut kept = lines[lines.len() - SMS_LOG_KEEP..].join("\n");
                kept.push('\n');
                self.write_whole("sms-log", &kept)?;
            }
        }
        self.write_whole("sms-done", &format!("{seq}\n"))?;
        let _ = fs::remove_file(self.path("sms-sending"));
        Ok(true)
    }

    /// Drop our claim without finishing (held for the clock).
    fn unclaim(&self, seq: u64, sender: &str, pid: u32) {
        if let Ok(_l) = self.lock()
            && self
                .sms_claim()
                .is_some_and(|c| c.seq == seq && c.sender == sender && c.pid == pid)
        {
            let _ = fs::remove_file(self.path("sms-sending"));
        }
    }

    /// One round of the alert SMS (guard `sms_round`): every pending alert in order, until one
    /// waits for the clock or another sender holds a fresh claim. `sender` names this sender in
    /// the claim ("core", "sentinel").
    pub fn sms_round(&self, sender: &str, pid: u32, io: &mut dyn SmsIo) -> Round {
        // Nothing pending and no claim: done without the lock (an idle round writes nothing).
        if !self.exists("sms-sending") {
            let done = self.read_u64("sms-done");
            let queue = fs::read_to_string(self.path("queue")).unwrap_or_default();
            if !parse_queue(&queue).iter().any(|e| e.seq > done) {
                return Round::Idle;
            }
        }
        let mut n = 0;
        loop {
            let (ev, resend) = match self.claim_next(sender, pid, io) {
                Ok(Next::Go(ev, r)) => (ev, r),
                Ok(Next::Idle) => return if n > 0 { Round::Done(n) } else { Round::Idle },
                Ok(Next::Busy(who)) => {
                    return if n > 0 {
                        Round::Done(n)
                    } else {
                        Round::Busy(who)
                    };
                }
                Err(e) => {
                    io.log(&format!("sms: alert files: {e}"));
                    return Round::Done(n);
                }
            };
            let wall = io.wall();
            let number = self.number();
            let log = fs::read_to_string(self.path("sms-log")).unwrap_or_default();
            let clock_ok = io.clock_ok();
            let v = verdict(&Facts {
                kind: &ev.kind,
                number: number.as_deref(),
                abroad: self.exists("abroad"),
                sms_abroad: self.exists("sms-abroad"),
                clock_ok,
                // a resend of an uncertain claim is not rate-limited: it may never have gone out
                rate_ok: resend || rate_ok(&log, &ev.kind, wall),
            });
            let result = match v {
                Verdict::Hold => {
                    self.unclaim(ev.seq, sender, pid);
                    return if n > 0 { Round::Done(n) } else { Round::Held };
                }
                Verdict::Skip => None,
                Verdict::Record(r) => Some(r),
                Verdict::Send => {
                    let number = number.unwrap_or_default();
                    let name = self.sms_name();
                    let text = sms_text(&ev.kind, io.lang_en(), wall, ev.seq, name.as_deref());
                    let tz = io.tz_hours();
                    match io.send(&send_args(&number, &text, wall, tz)) {
                        Ok(()) => {
                            io.delete_sent_copy(&number);
                            Some("sent")
                        }
                        Err(SendError::Rejected(e)) => {
                            io.log(&format!("sms: {e}"));
                            Some("failed")
                        }
                        Err(SendError::NotSent(e)) => {
                            io.log(&format!("sms: alert {} not sent ({e}); next round", ev.seq));
                            self.unclaim(ev.seq, sender, pid);
                            return Round::Done(n);
                        }
                    }
                }
            };
            match self.finish(ev.seq, &ev.kind, result, sender, pid, wall) {
                Ok(true) => {
                    if result == Some("failed") {
                        let _ = self.add(
                            "sms-failed",
                            &format!("alert {} ({}) not sent", ev.seq, ev.kind),
                            wall,
                            io.uptime(),
                        );
                    }
                    n += 1;
                }
                // another sender took our claim over as uncertain: it sends this one again
                Ok(false) => return Round::Done(n),
                Err(e) => {
                    io.log(&format!("sms: alert files: {e}"));
                    return Round::Done(n);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
