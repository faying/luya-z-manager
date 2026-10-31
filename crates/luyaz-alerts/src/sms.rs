//! Alert SMS: the rules, the texts and the send states (u60-sentinel.md §6, D9, D15), the one
//! copy both senders use: the core (normally) and the sentinel (only while the core is
//! unhealthy). The files, all under the alert directory:
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

use crate::{Alerts, Event, parse_queue, parse_sms_log, valid_number};

/// A `sending` claim this old is taken as "the sender died mid-send".
pub const UNCERTAIN_AFTER_S: u64 = 120;
/// At most one sent SMS per kind per hour, five per 24 h (guard `rate_ok`).
const RATE_KIND_S: i64 = 3600;
const RATE_DAY_S: i64 = 86400;
const RATE_DAY_MAX: usize = 5;
const SMS_LOG_KEEP: usize = 100;
/// Kinds never sent as an SMS: a failed SMS, the screen's colours, what an MCP agent changed (MCP
/// second phase: the agent reads it back, the owner's alert forwarding shows it), the tailnet
/// poller (owner 10-21: web and Hermes only), a new device on the LAN (guests on the hotspot would
/// each cost an SMS; the screen and the web show it).
const NEVER: &[&str] = &[
    "sms-failed",
    "ui-theme-paused",
    "agent-change",
    "tailnet-poller",
    "lan-new-device",
    // heat care (owner 10-29): warm and hot are inbox only; critical is regress-heat, texted
    "heat-protect",
    // R18-P2-01 stock anomalies (owner 10-23): inbox and MCP only while false alarms are watched
    "wifi-post-stuck",
    "no-service-long",
    // regress (10-24): a Wi-Fi the watchdog brought back is no regression; cellular outages stay
    // inbox-only as no-service-long was (eng review R2); "this kind is blind" is inbox-only
    "wifi-takeover",
    "regress-cellular",
    "regress-blind",
    // the 5 GHz channel guard (wifi-dfs-guard): the fallback worked, inbox only; still down is texted
    "wifi-5g-fallback",
];

/// A regress or point-event alert the device refused (eng review R9) goes again this long after
/// the first refusal (1st, 2nd, 3rd retry), outside the rate limits; refused a 4th time it is
/// `failed` as any other.
const RETRY_AFTER_S: [i64; 3] = [300, 600, 900];

/// How long a regress SMS may be, in UCS-2 characters (two parts of a long SMS).
pub const REGRESS_MAX: usize = 134;

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
    /// The wall is fit to print in an SMS: [`Self::clock_ok`] and not just the wall the device
    /// booted with (`last_wall`), which can be hours off until SNTP steps it (10-26: a reboot SMS
    /// said 01:58 at 09:58). Without it the SMS goes out with no time.
    fn wall_settled(&mut self) -> bool {
        self.clock_ok()
    }
    /// The screen's language is English (`lang=en`, last line, in ui.conf).
    fn lang_en(&mut self) -> bool;
    /// ZTE's SNTP zone in whole hours (`zwrt_zte_sntp.settings.time_from_utc`).
    fn tz_hours(&mut self) -> i32;
    /// `zwrt_wms zte_libwms_send_sms` with these args ([`send_args`]); Ok when the device took it.
    fn send(&mut self, args: &serde_json::Value) -> Result<(), SendError>;
    /// Delete the copy the firmware keeps in its sent box (the newest to `number`).
    fn delete_sent_copy(&mut self, number: &str);
    fn log(&mut self, msg: &str);
    /// The owner turned regress SMS off ([`crate::REGRESS_SMS_OFF`] exists).
    fn regress_sms_off(&mut self) -> bool {
        std::path::Path::new(crate::REGRESS_SMS_OFF).exists()
    }
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

/// The day limit alone (a point event's 2nd/4th/8th in a row, a regress escalation: eng review
/// R12): five sent in 24 h, a future stamp counting as just sent.
pub fn day_ok(log: &str, now: i64) -> bool {
    parse_sms_log(log)
        .iter()
        .filter(|r| r.result == "sent" && (r.wall > now || r.wall > now - RATE_DAY_S))
        .count()
        < RATE_DAY_MAX
}

/// Which one in its row a point event is: 1 + the events of its kind before it, each within
/// [`crate::POINT_WINDOW_S`] of the next (the queue holds what both senders and the core wrote).
pub fn row_n(events: &[Event], ev: &Event) -> usize {
    let mut n = 1;
    let mut t = ev.wall;
    for e in events
        .iter()
        .rev()
        .filter(|e| e.seq < ev.seq && e.kind == ev.kind)
    {
        if e.wall > t || t - e.wall > crate::POINT_WINDOW_S {
            break;
        }
        n += 1;
        t = e.wall;
    }
    n
}

/// What the regress rules make of one alert on top of the guard's verdict (eng review R4, R6, R12,
/// CEO OV-C4); every other kind is left exactly as [`verdict`] says (R6: the regression contract).
/// `nth`: a point event's place in its row; `esc`: a regress escalation (`rg-…-esc<n>`).
pub fn regress_verdict(v: Verdict, kind: &str, nth: Option<usize>, off: bool) -> Verdict {
    if v != Verdict::Send {
        return v;
    }
    if crate::is_regress(kind) && off {
        return Verdict::Record("suppressed-off");
    }
    if nth.is_some_and(|n| !matches!(n, 1 | 2 | 4 | 8)) {
        return Verdict::Record("suppressed-repeat");
    }
    Verdict::Send
}

/// A transition for the inbox only, no SMS: one that closes an incident (`rg-…-close`; design §5:
/// a recovery never costs the quota) or asks whether a level is the new normal (`rg-…-ask`;
/// design §4: inbox and MCP).
pub fn is_inbox_only(text: &str) -> bool {
    crate::text_id(text)
        .is_some_and(|id| id.starts_with("rg-") && (id.ends_with("-close") || id.ends_with("-ask")))
}

/// A regress escalation (`rg-…-esc<n>`).
pub fn is_escalation(text: &str) -> bool {
    crate::text_id(text).is_some_and(|id| {
        id.starts_with("rg-")
            && id
                .rsplit_once('-')
                .is_some_and(|(_, t)| t.strip_prefix("esc").is_some_and(|n| !n.is_empty()))
    })
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
        "ui-crash" => "屏幕界面闪退了，已自动重新打开。不用管。",
        "ui-gave-up" => {
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
        "agent-silent" => "自动管理Wi-Fi的程序没反应了，Wi-Fi会保持打开，上网不受影响。",
        "wifi-5g-down" => "U60的5G Wi-Fi停了5分钟还没恢复，2.4G不受影响。可在管理网页重开Wi-Fi。",
        _ => return format!("有新告警（{kind}），请到管理网页「系统→告警」查看。"),
    };
    m.to_string()
}

/// The same in English (guard `sms_body_en`; docs/ui-glossary.md §9): printable ASCII only. Four
/// are a few words shorter than the guard's so the short code still fits one SMS
/// (wifi-restore-failed, core-crash, ui-gave-up, dropbear-down).
fn body_en(kind: &str) -> String {
    let m = match kind {
        "wifi-takeover" => "Admin down; Wi-Fi turned on; no action",
        "wifi-restore-failed" => "Wi-Fi won't start; retrying; stuck? Reboot",
        "core-crash" => "Data service crashed; restarted; no action",
        "ui-crash" => "Screen UI crashed; reopened; no action",
        "ui-gave-up" => "Stock UI on; hold bottom-right 3s to undo",
        "sms-test" => "Test SMS: alert texts reach your phone",
        "screen-two-uis" => "Two screen UIs; stock UI kept; retry later",
        "screen-hold-expired" => "Screen hold expired; taken back; no action",
        "dropbear-restarted" => "SSH was down; restarted; no action needed",
        "dropbear-down" => "SSH keeps dropping; restarts paused 1 hour",
        "sentinel-silent" => "Watchdog silent 2+ min; screen and net OK",
        "screen-empty" => "Screen blank; stock UI on; hold corner 3s",
        "web-crash" => "Admin pages crashed; restarted; no action",
        "sentinel-crash" => "Watchdog crashed; restarted; no action",
        "agent-silent" => "Wi-Fi automation silent; Wi-Fi kept on",
        "wifi-5g-down" => "5 GHz Wi-Fi down 5+ min; 2.4 GHz still on",
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
/// `wall` 0 = no time to print (the clock is not settled, [`SmsIo::wall_settled`]).
pub fn sms_text(kind: &str, en: bool, wall: i64, seq: u64, name: Option<&str>) -> String {
    let body = if en { body_en(kind) } else { body_zh(kind) };
    stamp(&body, en, wall, seq, name)
}

/// The fixed part of a regress SMS (eng review O3): what happened, in the owner's words; the
/// queue text's details go after it, then where the recovery shows (design review 7A). `None` for
/// kinds that are never texted or not regress.
fn regress_zh(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "regress-reboot" => "U60意外重启了，现已恢复运行。",
        "regress-mem" => "数据服务内存比平时高很多，可能在泄漏。",
        "regress-mem-abs" => "数据服务内存超过上限，可能在泄漏。",
        "regress-egress" => "出口不通超过5分钟。",
        "regress-heat" => "温度过高（电池≥55°C或芯片≥90°C），放到阴凉处。",
        "regress-power" => "待机耗电比平时高。",
        "regress-power-abs" => "待机耗电超过上限。",
        "regress-recorder" => "长期记录停了10分钟以上。",
        "regress-core-down" => "数据服务停了10分钟以上，屏幕数据可能不更新。",
        "regress-disk" => "存储空间快满了，程序可能写不进去而崩溃。",
        _ => return None,
    })
}

fn regress_en(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "regress-reboot" => "Unexpected reboot; back up now.",
        "regress-mem" => "Data service memory well above normal.",
        "regress-mem-abs" => "Data service memory over limit.",
        "regress-egress" => "Route down 5+ min.",
        "regress-heat" => "Too hot (battery 55+ °C or chip 90+ °C); move it somewhere cool.",
        "regress-power" => "Standby power above normal.",
        "regress-power-abs" => "Standby power over limit.",
        "regress-recorder" => "Recorder stalled 10+ min.",
        "regress-core-down" => "Data service down 10+ min.",
        "regress-disk" => "Storage almost full.",
        _ => return None,
    })
}

const TAIL_ZH: &str = "恢复消息见网页「告警」页 / Discord";
const TAIL_EN: &str = "recovery: web Alerts / Discord";

/// The SMS for one queued alert: a regress kind's template with the details from its queue text
/// (cut first, never the template, to [`REGRESS_MAX`] characters); every other kind exactly as
/// [`sms_text`] words it.
pub fn sms_text_for(
    kind: &str,
    text: &str,
    en: bool,
    wall: i64,
    seq: u64,
    name: Option<&str>,
) -> String {
    let fixed = if en {
        regress_en(kind)
    } else {
        regress_zh(kind)
    };
    let Some(fixed) = fixed else {
        return sms_text(kind, en, wall, seq, name);
    };
    let raw = crate::text_body(text).trim();
    let hint;
    let detail = if en {
        raw
    } else {
        hint = if kind == "regress-disk" {
            // paths and sizes read the same in either language
            raw.to_string()
        } else {
            regress_hint_zh(raw)
        };
        hint.as_str()
    };
    let build = |d: &str| {
        let (mid, tail) = if en { (" ", TAIL_EN) } else { ("", TAIL_ZH) };
        let d = if d.is_empty() {
            String::new()
        } else if en {
            format!("{d}. ")
        } else {
            format!("{d}。")
        };
        let body = format!("{fixed}{mid}{d}{tail}");
        stamp(&body, en, wall, seq, name)
    };
    let whole = build(detail);
    if whole.chars().count() <= REGRESS_MAX {
        return whole;
    }
    let bare = build("").chars().count();
    // room for the details and their separators (". " / "。") and the ellipsis
    let room = REGRESS_MAX.saturating_sub(bare + if en { 3 } else { 2 });
    let cut: String = detail.chars().take(room).collect();
    if cut.is_empty() {
        return build("");
    }
    build(&format!("{}…", cut.trim_end()))
}

/// Signature, device name, body, the device-local time and the short code, as [`sms_text`] puts
/// them around its body.
fn stamp(body: &str, en: bool, wall: i64, seq: u64, name: Option<&str>) -> String {
    let (_, mo, d, h, mi, _) = civil(wall);
    let seq = seq % 1000;
    if en {
        const MON: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        let name = name.map(|n| format!("{n}: ")).unwrap_or_default();
        if wall <= 0 {
            return format!("[Luya-Z] {name}{body}#{seq}");
        }
        format!(
            "[Luya-Z] {name}{body} ({d:02} {} {h:02}:{mi:02})#{seq}",
            MON[(mo - 1) as usize]
        )
    } else {
        let name = name.map(|n| format!("{n}：")).unwrap_or_default();
        if wall <= 0 {
            return format!("【Luya-Z】{name}{body}#{seq}");
        }
        format!("【Luya-Z】{name}{body}（{mo:02}-{d:02} {h:02}:{mi:02}）#{seq}")
    }
}

/// What a Chinese regress SMS says of the queue text's details: the owner's words only. The
/// details themselves (codes, builds, /healthz) are English for the web and MCP; the one fact
/// worth a text is a ship just before it (`ship <comp>`), and it only "may be related".
fn regress_hint_zh(detail: &str) -> String {
    detail
        .split(';')
        .find_map(|p| p.trim().strip_prefix("ship "))
        .and_then(|r| r.split_whitespace().next())
        .map(|comp| format!("之前刚换过{comp}，可能有关"))
        .unwrap_or_default()
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

/// The touch UI's settings (with the program).
pub const UI_CONF: &str = "/data/luyaz/ui/ui.conf";

/// The screen's language from ui.conf: English only when the last `lang=` line is exactly
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

    /// A refused alert's retries (`sms-retry`: `seq \t tries \t first refusal wall`, eng review
    /// R9): how many retries were set up and when it was first refused.
    fn retry_of(&self, seq: u64) -> Option<(u32, i64)> {
        let t = fs::read_to_string(self.path("sms-retry")).ok()?;
        let f: Vec<&str> = t.trim_end().split('\t').collect();
        if f.len() != 3 || f[0].parse::<u64>().ok()? != seq {
            return None;
        }
        Some((f[1].parse().ok()?, f[2].parse().ok()?))
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
        if self.retry_of(seq).is_some() {
            let _ = fs::remove_file(self.path("sms-retry"));
        }
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
            // regress and point events (10-24): the row count, escalations, refusals retried
            let regress = crate::is_regress(&ev.kind);
            let point = crate::is_point(&ev.kind);
            let nth = point.then(|| {
                let queue = fs::read_to_string(self.path("queue")).unwrap_or_default();
                row_n(&parse_queue(&queue), &ev)
            });
            let retry = if regress || point {
                self.retry_of(ev.seq)
            } else {
                None
            };
            if let Some((tries, first)) = retry
                && clock_ok
                && wall >= first
                && wall < first + RETRY_AFTER_S[(tries.clamp(1, 3) - 1) as usize]
            {
                // refused before, its next try not due yet: everything after it waits too (order)
                self.unclaim(ev.seq, sender, pid);
                return if n > 0 { Round::Done(n) } else { Round::Held };
            }
            let v = if is_inbox_only(&ev.text) {
                Verdict::Skip
            } else {
                let escalation = nth.is_some_and(|k| k > 1) || is_escalation(&ev.text);
                regress_verdict(
                    verdict(&Facts {
                        kind: &ev.kind,
                        number: number.as_deref(),
                        abroad: self.exists("abroad"),
                        sms_abroad: self.exists("sms-abroad"),
                        clock_ok,
                        // a resend of an uncertain claim is not rate-limited: it may never have
                        // gone out; nor is a retry of a refused one (R9); an escalation counts
                        // against the day only (R12)
                        rate_ok: resend
                            || retry.is_some()
                            || if escalation {
                                day_ok(&log, wall)
                            } else {
                                rate_ok(&log, &ev.kind, wall)
                            },
                    }),
                    &ev.kind,
                    nth,
                    regress && io.regress_sms_off(),
                )
            };
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
                    let shown = if io.wall_settled() { wall } else { 0 };
                    let text = sms_text_for(
                        &ev.kind,
                        &ev.text,
                        io.lang_en(),
                        shown,
                        ev.seq,
                        name.as_deref(),
                    );
                    let tz = io.tz_hours();
                    match io.send(&send_args(&number, &text, wall, tz)) {
                        Ok(()) => {
                            io.delete_sent_copy(&number);
                            Some("sent")
                        }
                        Err(SendError::Rejected(e))
                            if (regress || point)
                                && retry.map_or(0, |r| r.0) < RETRY_AFTER_S.len() as u32 =>
                        {
                            let (tries, first) = retry.map_or((1, wall), |(t, f)| (t + 1, f));
                            io.log(&format!(
                                "sms: alert {} refused ({e}); try {} of {} in a few minutes",
                                ev.seq,
                                tries + 1,
                                RETRY_AFTER_S.len() + 1
                            ));
                            if let Err(e) = self.write_whole(
                                "sms-retry",
                                &format!("{}\t{tries}\t{first}\n", ev.seq),
                            ) {
                                io.log(&format!("sms: alert files: {e}"));
                            }
                            self.unclaim(ev.seq, sender, pid);
                            return Round::Done(n);
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
