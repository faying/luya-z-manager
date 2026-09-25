use std::collections::HashSet;
use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::datad_feed;
use crate::handlers::AppState;
use crate::ubus;

const CONFIG_PATH: &str = "/data/local/tmp/sms_forward.json";
const STATE_PATH: &str = "/data/local/tmp/sms_forward_state.json";
const MAX_LOG_ENTRIES: usize = 200;
const HTTP_TIMEOUT_SECS: u64 = 15;
const MAX_RETRIES: u32 = 3;
const RETRY_DELAYS: [u64; 3] = [5, 15, 60];
const INTER_RULE_DELAY_MS: u64 = 500;
const INTER_SMS_DELAY_MS: u64 = 1000;

// ── Data types ──────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone)]
pub struct SmsForwardConfig {
    pub enabled: bool,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
    #[serde(default)]
    pub mark_read_after_forward: bool,
    #[serde(default)]
    pub delete_after_forward: bool,
    #[serde(default)]
    pub rules: Vec<ForwardRule>,
}

fn default_poll_interval() -> u64 {
    30
}

impl Default for SmsForwardConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            poll_interval_secs: 30,
            mark_read_after_forward: false,
            delete_after_forward: false,
            rules: Vec::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ForwardRule {
    pub id: u32,
    pub name: String,
    pub enabled: bool,
    pub filter: SmsFilter,
    pub destination: ForwardDestination,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(tag = "type")]
pub enum SmsFilter {
    #[serde(rename = "all")]
    All,
    #[serde(rename = "sender")]
    Sender { patterns: Vec<String> },
    #[serde(rename = "content")]
    Content { keywords: Vec<String> },
    #[serde(rename = "sender_and_content")]
    SenderAndContent {
        patterns: Vec<String>,
        keywords: Vec<String>,
    },
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(tag = "type")]
pub enum ForwardDestination {
    #[serde(rename = "telegram")]
    Telegram {
        bot_token: String,
        chat_id: String,
        #[serde(default)]
        silent: bool,
    },
    #[serde(rename = "webhook")]
    Webhook {
        url: String,
        #[serde(default = "default_method")]
        method: String,
        #[serde(default)]
        headers: Vec<HttpHeader>,
    },
    #[serde(rename = "sms")]
    Sms { forward_number: String },
    #[serde(rename = "ntfy")]
    Ntfy {
        url: String,
        topic: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        token: Option<String>,
    },
    #[serde(rename = "discord")]
    Discord { webhook_url: String },
    #[serde(rename = "slack")]
    Slack { webhook_url: String },
}

fn default_method() -> String {
    "POST".into()
}

#[derive(Serialize, Deserialize, Clone)]
pub struct HttpHeader {
    pub name: String,
    pub value: String,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct ForwardState {
    pub last_forwarded_id: u64,
    /// Time of the newest SMS the forwarder has handled, as parsed from the
    /// SMS's own `date` field by `sms_time_key` (seconds, UTC-normalised with
    /// the SMS's zone). Both sides of every comparison come from that same
    /// field and parser — never from the device clock. Missing in state files
    /// written before this field existed; see `plan_forward`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_forwarded_time: Option<i64>,
    #[serde(default)]
    pub log: Vec<ForwardLogEntry>,
    /// The SMS the watermark is held at by a transient failure, if any.
    /// Missing in older state files (= nothing held).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<HeldSms>,
}

/// A deferred SMS: when it was first held, how long it has waited while
/// online, and which rules already got it (not re-sent after a restart).
#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
pub struct HeldSms {
    pub sms_id: u64,
    /// Wall clock of the first deferral (informational).
    pub first_deferred_at: i64,
    /// Seconds waited while online (cellular service + WAN) since then.
    #[serde(default)]
    pub online_secs: u64,
    #[serde(default)]
    pub delivered_rules: Vec<u32>,
}

/// A held SMS that still has not gone out after this much online time is
/// skipped for the rules still failing (one broken target must not block
/// every later SMS forever).
const HOLD_TIMEOUT_SECS: u64 = 6 * 3600;
const HOLD_TIMEOUT_NOTE: &str = "等待超时跳过";

/// Counts time only while online. `observe(now, online)` is called with the
/// link state from `now` on; it returns the seconds since the previous
/// observation if the link was up since then (an interval that started
/// offline counts nothing). The first observation
/// after a restart counts nothing.
#[derive(Default)]
struct OnlineClock {
    last: Option<(u64, bool)>,
}

impl OnlineClock {
    fn observe(&mut self, now: u64, online: bool) -> u64 {
        let add = match self.last {
            Some((t, true)) => now.saturating_sub(t),
            _ => 0,
        };
        self.last = Some((now, online));
        add
    }
}

/// Settle the hold after one delivery attempt of `sms_id`. Not deferred →
/// hold cleared. Deferred → hold started (or kept) with the rules delivered
/// so far; once it has waited `HOLD_TIMEOUT_SECS` online, the outcome turns
/// into `Skipped` and the alert text is returned (the hold is then cleared,
/// so this happens once per SMS).
fn settle_hold(held: &mut Option<HeldSms>, sms_id: u64, outcome: Delivery, delivered: &[u32], wall: i64) -> (Delivery, Option<String>) {
    let Delivery::Deferred(err) = outcome else {
        *held = None;
        return (outcome, None);
    };
    let h = match held {
        Some(h) if h.sms_id == sms_id => h,
        _ => held.insert(HeldSms { sms_id, first_deferred_at: wall, ..Default::default() }),
    };
    let mut rules = delivered.to_vec();
    rules.sort_unstable();
    rules.dedup();
    h.delivered_rules = rules;
    if h.online_secs < HOLD_TIMEOUT_SECS {
        return (Delivery::Deferred(err), None);
    }
    let alert = format!("SMS {sms_id} not forwarded after {}h online, skipped: {err}", h.online_secs / 3600);
    *held = None;
    (Delivery::Skipped(format!("{HOLD_TIMEOUT_NOTE}: {err}")), Some(alert))
}

fn mono_secs() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ForwardLogEntry {
    pub timestamp: i64,
    pub sms_id: u64,
    pub sender: String,
    pub content_preview: String,
    pub rule_name: String,
    pub destination_type: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub rule_id: u32,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub date: String,
}

// ── wake-ups ─────────────────────────────────────────────────────────

/// Why the forwarder woke: datad's `sms` block summary (max id / count)
/// changed, or the source switched (订阅中 with an `sms` block ⇄ 退路 / no
/// block). Replaces the `zwrt_wms_status_event` ubus listen (T10/D2).
struct Wake;

/// While datad's `sms` block drives us: a check every 5 min as a safety net.
/// Reading ubus directly (退路, no feed, or a datad without the block):
/// every [`FALLBACK_SMS_POLL`] — nothing else wakes us then.
const IDLE_SMS_POLL: Duration = Duration::from_secs(300);
const FALLBACK_SMS_POLL: Duration = Duration::from_secs(60);

/// When to run a check. Pure over a monotonic `now`.
#[derive(Debug)]
struct PollSchedule {
    last_poll: Duration,
    /// The last check went through datad (`sms` block fresh, subscribed).
    via_datad: bool,
}

impl PollSchedule {
    fn new(now: Duration, via_datad: bool) -> Self {
        PollSchedule { last_poll: now, via_datad }
    }

    fn period(&self) -> Duration {
        if self.via_datad {
            IDLE_SMS_POLL
        } else {
            FALLBACK_SMS_POLL
        }
    }

    /// The source for this check; true if it switched (then page through
    /// either way — into direct reads to cover the gap, back to datad to
    /// catch anything the last direct read missed).
    fn source_changed(&mut self, via_datad: bool) -> bool {
        let changed = self.via_datad != via_datad;
        self.via_datad = via_datad;
        changed
    }

    fn due(&self, now: Duration) -> bool {
        now.saturating_sub(self.last_poll) >= self.period()
    }

    fn polled(&mut self, now: Duration) {
        self.last_poll = now;
    }

    fn wait(&self, now: Duration) -> Duration {
        (self.last_poll + self.period())
            .saturating_sub(now)
            .clamp(Duration::from_millis(100), IDLE_SMS_POLL)
    }
}

// ── SMS message from ubus ───────────────────────────────────────────

struct DecodedSms {
    id: u64,
    sender: String,
    content: String,
    date: String,
}

/// What one complete (both stores OK) read of the device's SMS list gave us.
#[derive(Debug, Clone, Default)]
struct DeviceSnapshot {
    /// Highest id seen in any row (received or not).
    max_id: u64,
    /// Received messages (tag 0/1): (id, `sms_time_key` of its date).
    received: Vec<(u64, Option<i64>)>,
}

#[derive(Debug, PartialEq)]
struct ForwardPlan {
    /// Ids to forward, ascending.
    forward_ids: Vec<u64>,
    new_watermark: u64,
    new_last_time: Option<i64>,
    /// Device ids went below the watermark (counter reset / storage wiped).
    rollback: bool,
}

/// Decide what to forward. Pure; see R4/R15 in the datad plan.
///
/// * Read failed (either store errored) → nothing forwarded, nothing moves.
/// * Normal: forward received ids > watermark; watermark = max of those.
/// * Rollback (device max id < watermark, device not empty): forward received
///   messages whose time is newer than `last_time`, then watermark = device
///   max id. Without a `last_time` (old state file) nothing is forwarded — we
///   would rather miss a message than re-send old ones — and the baseline is
///   re-established from what is on the device.
/// * An empty device is not treated as a rollback (can't tell; keep state).
/// * `last_time` missing with no rollback: backfilled from messages the
///   watermark already covers, so old state files gain a time baseline.
fn plan_forward(watermark: u64, last_time: Option<i64>, snapshot: Result<&DeviceSnapshot, &str>) -> ForwardPlan {
    let unchanged = ForwardPlan {
        forward_ids: Vec::new(),
        new_watermark: watermark,
        new_last_time: last_time,
        rollback: false,
    };
    let snap = match snapshot {
        Ok(s) => s,
        Err(_) => return unchanged,
    };
    let max_opt = |a: Option<i64>, b: Option<i64>| match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (x, None) => x,
        (None, y) => y,
    };

    if snap.max_id > 0 && snap.max_id < watermark {
        let mut forward: Vec<(u64, Option<i64>)> = match last_time {
            Some(lt) => snap
                .received
                .iter()
                .copied()
                .filter(|&(_, t)| matches!(t, Some(t) if t > lt))
                .collect(),
            None => Vec::new(),
        };
        forward.sort_by_key(|&(id, _)| id);
        let new_last_time = match last_time {
            Some(_) => forward.iter().fold(last_time, |acc, &(_, t)| max_opt(acc, t)),
            None => snap.received.iter().fold(None, |acc, &(_, t)| max_opt(acc, t)),
        };
        return ForwardPlan {
            forward_ids: forward.into_iter().map(|(id, _)| id).collect(),
            new_watermark: snap.max_id,
            new_last_time,
            rollback: true,
        };
    }

    let mut forward: Vec<(u64, Option<i64>)> =
        snap.received.iter().copied().filter(|&(id, _)| id > watermark).collect();
    forward.sort_by_key(|&(id, _)| id);
    let mut new_last_time = last_time;
    if new_last_time.is_none() {
        new_last_time = snap
            .received
            .iter()
            .filter(|&&(id, _)| id <= watermark)
            .fold(None, |acc, &(_, t)| max_opt(acc, t));
    }
    let new_last_time = forward.iter().fold(new_last_time, |acc, &(_, t)| max_opt(acc, t));
    ForwardPlan {
        new_watermark: forward.last().map(|&(id, _)| id).unwrap_or(watermark),
        forward_ids: forward.into_iter().map(|(id, _)| id).collect(),
        new_last_time,
        rollback: false,
    }
}

// ── UCS-2 hex decoding ──────────────────────────────────────────────

fn decode_ucs2_hex(hex: &str) -> String {
    let hex = hex.trim();
    if hex.is_empty() {
        return String::new();
    }
    let mut chars = Vec::new();
    let mut i = 0;
    let bytes = hex.as_bytes();
    while i + 4 <= bytes.len() {
        if let Ok(code) = u16::from_str_radix(&hex[i..i + 4], 16) {
            if let Some(ch) = char::from_u32(code as u32) {
                chars.push(ch);
            }
        }
        i += 4;
    }
    chars.into_iter().collect()
}

/// Check if a string looks like UCS-2 hex (all hex chars, length multiple of 4).
fn is_ucs2_hex(s: &str) -> bool {
    let s = s.trim();
    !s.is_empty() && s.len() % 4 == 0 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Parse ZTE timestamp `YY,MM,DD,HH,MM,SS,+TZ` (or `;` separators) into human-readable string.
fn humanize_zte_date(raw: &str) -> String {
    let parts: Vec<&str> = raw.split(|c| c == ',' || c == ';').collect();
    if parts.len() < 6 {
        return raw.to_string();
    }
    let ok = || -> Option<String> {
        let yy: u32 = parts[0].trim().parse().ok()?;
        let mm: u32 = parts[1].trim().parse().ok()?;
        let dd: u32 = parts[2].trim().parse().ok()?;
        let hh: u32 = parts[3].trim().parse().ok()?;
        let min: u32 = parts[4].trim().parse().ok()?;
        let month = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ]
        .get((mm as usize).wrapping_sub(1))?;
        let (h12, ampm) = match hh {
            0 => (12, "AM"),
            1..=11 => (hh, "AM"),
            12 => (12, "PM"),
            _ => (hh - 12, "PM"),
        };
        let tz = parts.get(6).map(|s| s.trim()).unwrap_or("");
        let tz_str = if tz.is_empty() {
            String::new()
        } else {
            format!(" UTC{tz}")
        };
        Some(format!(
            "{month} {dd}, 20{yy:02} {h12}:{min:02} {ampm}{tz_str}"
        ))
    }();
    ok.unwrap_or_else(|| raw.to_string())
}

/// Parse the ZTE SMS `date` field (`YY,MM,DD,HH,MM,SS,+TZ`, `;` also
/// accepted) into seconds since 2000-01-01 UTC, using the zone the SMS
/// carries (hours; values beyond ±14 are read as GSM quarter-hours). Only ever
/// compared with other values from this same function.
fn sms_time_key(raw: &str) -> Option<i64> {
    let parts: Vec<&str> = raw.split(|c| c == ',' || c == ';').collect();
    if parts.len() < 6 {
        return None;
    }
    let n = |i: usize| -> Option<i64> { parts[i].trim().parse::<i64>().ok() };
    let (yy, mo, dd, hh, mi, ss) = (n(0)?, n(1)?, n(2)?, n(3)?, n(4)?, n(5)?);
    if !(0..=99).contains(&yy)
        || !(1..=12).contains(&mo)
        || !(1..=31).contains(&dd)
        || !(0..=23).contains(&hh)
        || !(0..=59).contains(&mi)
        || !(0..=60).contains(&ss)
    {
        return None;
    }
    // Days since 2000-01-01 (civil calendar, all years 2000..2099).
    let y = 2000 + yy;
    let (y2, m2) = if mo <= 2 { (y - 1, mo + 9) } else { (y, mo - 3) };
    let era_days = |y: i64, m: i64, d: i64| 365 * y + y / 4 - y / 100 + y / 400 + (153 * m + 2) / 5 + d;
    let days = era_days(y2, m2, dd) - era_days(1999, 10, 1); // 2000-01-01 == Mar-based (1999, 10, 1)
    let tz_secs = match parts.get(6).map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(tz) => {
            let v: i64 = tz.trim_start_matches('+').parse().ok()?;
            if v.abs() > 14 { v * 15 * 60 } else { v * 3600 }
        }
        None => 0,
    };
    Some(days * 86_400 + hh * 3600 + mi * 60 + ss - tz_secs)
}

/// Encode text as UTF-16BE hex (UCS-2), matching the apps' `encodeUCS2Hex`.
fn encode_ucs2_hex(text: &str) -> String {
    text.encode_utf16()
        .map(|c| format!("{c:04X}"))
        .collect()
}

/// Generate ZTE-format SMS timestamp "YY;MM;DD;HH;MM;SS;+TZ".
fn format_sms_time() -> String {
    let mut t: i64 = 0;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        libc::time(&mut t);
        libc::localtime_r(&t, &mut tm);
    }
    // tm_gmtoff is 0 on this firmware (TZ=UTC over a local-time clock); the
    // real zone comes from ZTE's SNTP settings. See clock.rs.
    let tz_offset = crate::clock::sms_zone_hours();
    let tz_sign = if tz_offset >= 0 { "+" } else { "" };
    format!(
        "{:02};{:02};{:02};{:02};{:02};{:02};{}{}",
        tm.tm_year % 100,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        tz_sign,
        tz_offset,
    )
}

// ── Filter matching ─────────────────────────────────────────────────

fn normalize_phone(num: &str) -> String {
    let num = num.trim();
    if let Some(rest) = num.strip_prefix("+63") {
        return format!("0{rest}");
    }
    num.to_string()
}

fn is_forward_loop(dest: &ForwardDestination, sender: &str) -> bool {
    match dest {
        ForwardDestination::Sms { forward_number } => {
            normalize_phone(sender) == normalize_phone(forward_number)
        }
        _ => false,
    }
}

/// Detect garbled echo content (firmware artifact: mostly `@` / NUL chars).
fn is_garbled_echo(content: &str) -> bool {
    if content.is_empty() {
        return true;
    }
    let junk = content.chars().filter(|&c| c == '@' || c == '\0').count();
    junk * 2 > content.len() // >50% junk chars
}

fn matches_filter(filter: &SmsFilter, sender: &str, content: &str) -> bool {
    match filter {
        SmsFilter::All => true,
        SmsFilter::Sender { patterns } => sender_matches(sender, patterns),
        SmsFilter::Content { keywords } => content_matches(content, keywords),
        SmsFilter::SenderAndContent { patterns, keywords } => {
            sender_matches(sender, patterns) && content_matches(content, keywords)
        }
    }
}

fn sender_matches(sender: &str, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return true;
    }
    let sender_lower = sender.to_lowercase();
    patterns.iter().any(|p| {
        let p_lower = p.to_lowercase();
        if p_lower.ends_with('*') {
            sender_lower.starts_with(&p_lower[..p_lower.len() - 1])
        } else if p_lower.starts_with('*') {
            sender_lower.ends_with(&p_lower[1..])
        } else {
            sender_lower == p_lower
        }
    })
}

fn content_matches(content: &str, keywords: &[String]) -> bool {
    if keywords.is_empty() {
        return true;
    }
    let content_lower = content.to_lowercase();
    keywords
        .iter()
        .any(|kw| content_lower.contains(&kw.to_lowercase()))
}

// ── Destination formatting + dispatch ───────────────────────────────

fn destination_type_name(dest: &ForwardDestination) -> &'static str {
    match dest {
        ForwardDestination::Telegram { .. } => "telegram",
        ForwardDestination::Webhook { .. } => "webhook",
        ForwardDestination::Sms { .. } => "sms",
        ForwardDestination::Ntfy { .. } => "ntfy",
        ForwardDestination::Discord { .. } => "discord",
        ForwardDestination::Slack { .. } => "slack",
    }
}

fn format_message(sms: &DecodedSms) -> String {
    format!(
        "SMS from {}\n{}\n\n{}",
        sms.sender,
        humanize_zte_date(&sms.date),
        sms.content
    )
}

fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(HTTP_TIMEOUT_SECS)))
        .build()
        .into()
}

fn forward_to(
    dest: &ForwardDestination,
    sms: &DecodedSms,
    agent: &ureq::Agent,
) -> Result<(), String> {
    match dest {
        ForwardDestination::Telegram {
            bot_token,
            chat_id,
            silent,
        } => {
            let url = format!("https://api.telegram.org/bot{bot_token}/sendMessage");
            let body = json!({
                "chat_id": chat_id,
                "text": format_message(sms),
                "disable_notification": silent,
            });
            let resp = agent
                .post(&url)
                .header("Content-Type", "application/json")
                .send(body.to_string().as_bytes())
                .map_err(|e| format!("telegram: {e}"))?;
            check_http_status(resp.status().into(), "telegram")
        }
        ForwardDestination::Webhook {
            url,
            method,
            headers,
        } => {
            let body = json!({
                "event": "sms_received",
                "sms": {
                    "id": sms.id,
                    "sender": sms.sender,
                    "content": sms.content,
                    "date": sms.date,
                },
                "timestamp": now_ts(),
            });
            let body_bytes = body.to_string();
            let mut req = match method.to_uppercase().as_str() {
                "PUT" => agent.put(url),
                _ => agent.post(url),
            };
            req = req.header("Content-Type", "application/json");
            for h in headers {
                req = req.header(&h.name, &h.value);
            }
            let resp = req
                .send(body_bytes.as_bytes())
                .map_err(|e| format!("webhook: {e}"))?;
            check_http_status(resp.status().into(), "webhook")
        }
        ForwardDestination::Sms { forward_number } => {
            let text = format_message(sms);
            let encode_type = "UNICODE";
            let message_body = encode_ucs2_hex(&text);
            let params = json!({
                "number": forward_number,
                "message_body": message_body,
                "encode_type": encode_type,
                "sms_time": format_sms_time(),
                "id": "-1",
            });
            let resp = ubus::call(
                "zwrt_wms",
                "zte_libwms_send_sms",
                Some(&params.to_string()),
            )
            .map_err(|e| format!("sms forward: {e}"))?;
            check_sms_send_result(&resp)?;

            // Auto-delete the forwarded outgoing SMS to prevent conversation clutter
            cleanup_forwarded_sms(forward_number);

            Ok(())
        }
        ForwardDestination::Ntfy { url, topic, token } => {
            let full_url = format!("{}/{}", url.trim_end_matches('/'), topic);
            let mut req = agent.post(&full_url);
            req = req.header("Title", &format!("SMS from {}", sms.sender));
            if let Some(t) = token {
                req = req.header("Authorization", &format!("Bearer {t}"));
            }
            let resp = req
                .send(sms.content.as_bytes())
                .map_err(|e| format!("ntfy: {e}"))?;
            check_http_status(resp.status().into(), "ntfy")
        }
        ForwardDestination::Discord { webhook_url } => {
            let text = format_message(sms);
            // Discord has 2000 char limit
            let text = if text.len() > 2000 {
                format!("{}...", &text[..text.floor_char_boundary(1997)])
            } else {
                text
            };
            let body = json!({ "content": text });
            let resp = agent
                .post(webhook_url)
                .header("Content-Type", "application/json")
                .send(body.to_string().as_bytes())
                .map_err(|e| format!("discord: {e}"))?;
            check_http_status(resp.status().into(), "discord")
        }
        ForwardDestination::Slack { webhook_url } => {
            let body = json!({ "text": format_message(sms) });
            let resp = agent
                .post(webhook_url)
                .header("Content-Type", "application/json")
                .send(body.to_string().as_bytes())
                .map_err(|e| format!("slack: {e}"))?;
            check_http_status(resp.status().into(), "slack")
        }
    }
}

fn check_http_status(status: u16, dest: &str) -> Result<(), String> {
    if (200..300).contains(&status) {
        Ok(())
    } else if status == 429 {
        Err(format!("{dest}: HTTP 429 rate limited"))
    } else if (400..500).contains(&status) {
        Err(format!("{dest}: HTTP {status} (permanent error)"))
    } else {
        Err(format!("{dest}: HTTP {status}"))
    }
}

/// Check ZTE SMS send response. result=3 means success, anything else is failure.
fn check_sms_send_result(resp: &Value) -> Result<(), String> {
    let result = resp
        .get("result")
        .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())));
    match result {
        Some(3) | None => Ok(()),
        Some(code) => Err(format!("sms forward: device rejected (result={code})")),
    }
}

/// Best-effort cleanup of forwarded outgoing SMS.
/// The firmware stores sent SMS on the device; we delete it so it doesn't appear
/// in the conversation list. Only appears in the forward log.
fn cleanup_forwarded_sms(forward_number: &str) {
    // Small delay to let firmware store the outgoing SMS
    std::thread::sleep(Duration::from_millis(200));

    // Fetch latest sent messages (tag=2)
    let messages = match fetch_sms_both_stores(2, 0, 5, "order by id desc") {
        Ok(m) => m,
        Err(e) => {
            eprintln!("[sms_forward] cleanup: failed to list sent SMS: {e}");
            return;
        }
    };

    // Find the most recent sent SMS to the forward number
    let normalized_dest = normalize_phone(forward_number);
    for item in &messages {
        let number = item["number"].as_str().unwrap_or("");
        let decoded_number = if is_ucs2_hex(number) {
            decode_ucs2_hex(number)
        } else {
            number.to_string()
        };

        if normalize_phone(&decoded_number) == normalized_dest {
            let id = item["id"].as_u64()
                .or_else(|| item["id"].as_str().and_then(|s| s.parse().ok()))
                .unwrap_or(0);
            if id > 0 {
                match ubus::call(
                    "zwrt_wms",
                    "zwrt_wms_delete_sms",
                    Some(&json!({"id": id.to_string()}).to_string()),
                ) {
                    Ok(_) => eprintln!("[sms_forward] cleanup: deleted forwarded outgoing SMS id={id}"),
                    Err(e) => eprintln!("[sms_forward] cleanup: failed to delete SMS id={id}: {e}"),
                }
            }
            break; // Only delete the most recent match
        }
    }
}

fn is_transient_error(err: &str) -> bool {
    // HTTP 4xx are permanent — everything else is transient
    !err.contains("(permanent error)")
}

// ── Time helper ─────────────────────────────────────────────────────

fn now_ts() -> i64 {
    unsafe { libc::time(std::ptr::null_mut()) as i64 }
}

// ── Core forwarder ──────────────────────────────────────────────────

pub struct SmsForwarder {
    config: Mutex<SmsForwardConfig>,
    state: Mutex<ForwardState>,
    has_service: AtomicBool,
    wan_connected: AtomicBool,
    /// `(sms_id, rule_id)` already delivered for the SMS the watermark is
    /// held at (a deferred SMS is tried again later: rules that already got
    /// it, automatically or by a manual retry, are not sent it twice).
    delivered: Mutex<HashSet<(u64, u32)>>,
    /// Online-only clock for the held SMS's timeout.
    online_clock: Mutex<OnlineClock>,
    /// Wakes the forwarder (connectivity came back).
    wake: Mutex<Option<mpsc::Sender<Wake>>>,
}

impl SmsForwarder {
    pub fn new() -> Self {
        let config = fs::read_to_string(CONFIG_PATH)
            .ok()
            .and_then(|s| serde_json::from_str::<SmsForwardConfig>(&s).ok())
            .unwrap_or_default();

        let state = fs::read_to_string(STATE_PATH)
            .ok()
            .and_then(|s| serde_json::from_str::<ForwardState>(&s).ok())
            .unwrap_or_default();

        let delivered: HashSet<(u64, u32)> =
            state.held.iter().flat_map(|h| h.delivered_rules.iter().map(move |r| (h.sms_id, *r))).collect();
        SmsForwarder {
            config: Mutex::new(config),
            state: Mutex::new(state),
            has_service: AtomicBool::new(false),
            wan_connected: AtomicBool::new(false),
            delivered: Mutex::new(delivered),
            online_clock: Mutex::new(OnlineClock::default()),
            wake: Mutex::new(None),
        }
    }

    /// Start the SMS forwarder with event-driven reception.
    /// Falls back to polling if the event channel disconnects.
    pub fn start(self: &Arc<Self>, service_rx: mpsc::Receiver<Value>, wan_rx: mpsc::Receiver<Value>) {
        // Seed initial connectivity state (retry if services aren't registered yet)
        for attempt in 1..=5 {
            let got_service = if !self.has_service.load(Ordering::Relaxed) {
                if let Ok(data) = ubus::call("zte_nwinfo_api", "nwinfo_get_netinfo", Some("{}")) {
                    let network_type = data["network_type"].as_str().unwrap_or("");
                    let has = !network_type.is_empty() && network_type != "NO_SERVICE" && !network_type.starts_with("LIMITED_SERVICE");
                    self.has_service.store(has, Ordering::Relaxed);
                    eprintln!("[sms_forward] initial service state: {network_type} (has_service={has})");
                    has
                } else {
                    false
                }
            } else {
                true
            };

            let got_wan = if !self.wan_connected.load(Ordering::Relaxed) {
                if let Ok(data) = ubus::call("zwrt_data", "get_wwaniface", Some(r#"{"source_module":"zte_topsw_data","cid":1}"#)) {
                    let connected = data["connect_status"].as_str().map(|s| s.starts_with("ipv4")).unwrap_or(false);
                    self.wan_connected.store(connected, Ordering::Relaxed);
                    eprintln!("[sms_forward] initial WAN state: connected={connected}");
                    connected
                } else {
                    false
                }
            } else {
                true
            };

            if got_service && got_wan {
                break;
            }
            if attempt < 5 {
                eprintln!("[sms_forward] connectivity seed attempt {attempt}/5 incomplete (service={got_service}, wan={got_wan}), retrying in 2s");
                std::thread::sleep(Duration::from_secs(2));
            }
        }

        // Wake on the datad `sms` block: max id / count changed, or it
        // appeared / went away (source switch). Without a feed the sender
        // stays alive in the forwarder thread and only the timer wakes it.
        // Connectivity coming back also wakes it (a deferred SMS goes out).
        let (tx, rx) = mpsc::channel();
        *self.wake.lock().unwrap() = Some(tx.clone());

        let conn_self = Arc::clone(self);
        std::thread::spawn(move || {
            conn_self.connectivity_monitor(service_rx, wan_rx);
        });
        let keep_tx = match datad_feed::global() {
            Some(feed) => {
                std::thread::spawn(move || {
                    let first = feed.view();
                    let (mut seen, mut last) = (first.version, first.sms_summary());
                    loop {
                        let v = feed.wait_change(seen, datad_feed::MAX_WAIT);
                        seen = v.version;
                        let now = v.sms_summary();
                        if now != last {
                            last = now;
                            if tx.send(Wake).is_err() {
                                return;
                            }
                        }
                    }
                });
                None
            }
            None => Some(tx),
        };

        let forwarder = Arc::clone(self);
        std::thread::spawn(move || {
            let _keep_tx = keep_tx;
            forwarder.event_loop(rx);
        });
    }

    fn connectivity_monitor(&self, service_rx: mpsc::Receiver<Value>, wan_rx: mpsc::Receiver<Value>) {
        loop {
            let mut got_event = false;

            // Check service events
            match service_rx.try_recv() {
                Ok(event) => {
                    let network_type = event["network_type"].as_str().unwrap_or("");
                    let has = !network_type.is_empty() && network_type != "NO_SERVICE" && !network_type.starts_with("LIMITED_SERVICE");
                    let prev = self.has_service.swap(has, Ordering::Relaxed);
                    self.tick_online(); // closes the interval under the old state
                    if prev != has {
                        eprintln!("[sms_forward] service state changed: {network_type} (has_service={has})");
                        if has {
                            self.wake_up();
                        }
                    }
                    got_event = true;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    eprintln!("[sms_forward] service_rx disconnected");
                    return;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }

            // Check WAN events
            match wan_rx.try_recv() {
                Ok(event) => {
                    let wan_status = event["wan_status"].as_str().unwrap_or("");
                    let connected = wan_status.starts_with("ipv4");
                    let prev = self.wan_connected.swap(connected, Ordering::Relaxed);
                    self.tick_online(); // closes the interval under the old state
                    if prev != connected {
                        eprintln!("[sms_forward] WAN state changed: {wan_status} (connected={connected})");
                        if connected {
                            self.wake_up();
                        }
                    }
                    got_event = true;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    eprintln!("[sms_forward] wan_rx disconnected");
                    return;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }

            if !got_event {
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }

    fn online(&self) -> bool {
        self.has_service.load(Ordering::Relaxed) && self.wan_connected.load(Ordering::Relaxed)
    }

    /// Add the online time since the last tick to the held SMS (in memory;
    /// persisted with the next save).
    fn tick_online(&self) {
        let add = self.online_clock.lock().unwrap().observe(mono_secs(), self.online());
        if add > 0 {
            if let Some(h) = self.state.lock().unwrap().held.as_mut() {
                h.online_secs += add;
            }
        }
    }

    /// Connectivity came back: check now (one wake per transition, so no storm).
    fn wake_up(&self) {
        if let Some(tx) = self.wake.lock().unwrap().as_ref() {
            let _ = tx.send(Wake);
        }
    }

    fn init_watermark(&self) {
        let last_id = self.state.lock().unwrap().last_forwarded_id;
        if last_id == 0 {
            match fetch_max_sms_id() {
                Ok(max_id) if max_id > 0 => {
                    let mut state = self.state.lock().unwrap();
                    state.last_forwarded_id = max_id;
                    save_state(&state);
                    eprintln!("[sms_forward] watermark initialized to {max_id}");
                }
                Ok(_) => eprintln!("[sms_forward] no SMS on device, watermark stays at 0"),
                Err(e) => eprintln!("[sms_forward] watermark init failed: {e}"),
            }
        }
    }

    fn event_loop(&self, rx: mpsc::Receiver<Wake>) {
        self.init_watermark();

        let t0 = Instant::now();
        // Check once at start: a restart in the middle of a burst resumes
        // from the watermark at once, not after a whole poll period.
        let summary = sms_summary();
        let mut sched = PollSchedule::new(Duration::ZERO, summary.is_some());
        self.check(summary);
        sched.polled(t0.elapsed());
        loop {
            match rx.recv_timeout(sched.wait(t0.elapsed())) {
                Ok(Wake) => {
                    // A burst queues many wakes; one pass pages through all.
                    while rx.try_recv().is_ok() {}
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if !sched.due(t0.elapsed()) {
                        continue;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    eprintln!("[sms_forward] wake channel closed");
                    return;
                }
            }
            let summary = sms_summary();
            if sched.source_changed(summary.is_some()) {
                eprintln!(
                    "[sms_forward] SMS source now {}: checking SMS",
                    if summary.is_some() { "datad" } else { "direct ubus" }
                );
            }
            self.check(summary);
            sched.polled(t0.elapsed());
        }
    }

    /// One check: page through everything above the watermark. `summary` =
    /// datad's `sms` block (subscribed, fresh); `None` = read ubus directly.
    fn check(&self, summary: Option<(u64, u64)>) {
        let config = self.config.lock().unwrap().clone();
        let enabled_rules: Vec<&ForwardRule> = config.rules.iter().filter(|r| r.enabled).collect();
        if !config.enabled || enabled_rules.is_empty() {
            return;
        }
        self.init_watermark();

        let agent = http_agent();
        let mut forward = |sms: &DecodedSms| self.deliver(sms, &config, &enabled_rules, &agent);
        let mut commit = |wm: u64, lt: Option<i64>| {
            let mut state = self.state.lock().unwrap();
            state.last_forwarded_id = wm;
            state.last_forwarded_time = lt;
            save_state(&state);
        };
        let load = || {
            let st = self.state.lock().unwrap();
            (st.last_forwarded_id, st.last_forwarded_time)
        };

        if let Some((max_id, _)) = summary {
            let (wm, lt) = load();
            let addr = datad_feed::datad_addr();
            let mut fetch = |after: u64, limit: u64| datad_page(addr, after, limit, &datad_feed::BUSY_RETRY);
            match run_pass(wm, lt, max_id, &mut fetch, &mut forward, &mut commit) {
                Ok(n) => {
                    if n > 0 {
                        eprintln!("[sms_forward] forwarded {n} message(s) after id {wm} (via datad)");
                    }
                    return;
                }
                // Progress so far is committed; finish by reading ubus (D2's 退路).
                Err(e) => eprintln!("[sms_forward] datad sms.list_after failed: {e}; reading ubus directly"),
            }
        }

        // Direct: both stores must read OK to know the device max id (R15).
        let max_id = match fetch_max_sms_id() {
            Ok(m) => m,
            Err(e) => {
                eprintln!("[sms_forward] failed to read SMS max id: {e}");
                return;
            }
        };
        let (wm, lt) = load();
        let mut fetch = |after: u64, limit: u64| list_after_direct(after, limit, &mut ubus_sms_page);
        match run_pass(wm, lt, max_id, &mut fetch, &mut forward, &mut commit) {
            Ok(n) if n > 0 => eprintln!("[sms_forward] forwarded {n} message(s) after id {wm} (direct)"),
            Ok(_) => {}
            Err(e) => eprintln!("[sms_forward] failed to read SMS after id {wm}: {e}"),
        }
    }

    /// Forward one SMS through every matching rule, log, then mark-read /
    /// delete if all succeeded. [`Delivery::Deferred`] (a transient failure
    /// on some rule) holds the watermark at this SMS; rules that already got
    /// it are remembered and skipped next time.
    fn deliver(&self, sms: &DecodedSms, config: &SmsForwardConfig, enabled_rules: &[&ForwardRule], agent: &ureq::Agent) -> Delivery {
        let mut errors: Vec<String> = Vec::new();
        self.tick_online();

        for (i, rule) in enabled_rules.iter().enumerate() {
            // Skip firmware echo: sender matches destination AND content is garbled
            if is_forward_loop(&rule.destination, &sms.sender) && is_garbled_echo(&sms.content) {
                continue;
            }

            if !matches_filter(&rule.filter, &sms.sender, &sms.content) {
                continue;
            }
            if self.delivered.lock().unwrap().contains(&(sms.id, rule.id)) {
                continue;
            }

            // Pre-forward connectivity check
            let connectivity_err = match &rule.destination {
                ForwardDestination::Sms { .. } => {
                    if !self.has_service.load(Ordering::Relaxed) {
                        Some(NO_SERVICE_ERR.to_string())
                    } else {
                        None
                    }
                }
                _ => {
                    if !self.wan_connected.load(Ordering::Relaxed) {
                        Some(NO_WAN_ERR.to_string())
                    } else {
                        None
                    }
                }
            };

            let result = match connectivity_err {
                Some(err) => {
                    eprintln!("[sms_forward] skipping forward for SMS {} -> {}: {err}", sms.id, destination_type_name(&rule.destination));
                    Err(err)
                }
                None => forward_with_retry(&rule.destination, sms, agent),
            };

            match &result {
                Ok(()) => {
                    self.delivered.lock().unwrap().insert((sms.id, rule.id));
                }
                Err(e) => errors.push(e.clone()),
            }

            let entry = ForwardLogEntry {
                timestamp: now_ts(),
                sms_id: sms.id,
                sender: sms.sender.clone(),
                content_preview: preview(&sms.content, 80),
                rule_name: rule.name.clone(),
                destination_type: destination_type_name(&rule.destination).to_string(),
                success: result.is_ok(),
                error: result.err(),
                rule_id: rule.id,
                content: sms.content.clone(),
                date: sms.date.clone(),
            };

            let mut state = self.state.lock().unwrap();
            push_log(&mut state.log, entry);
            if state.log.len() > MAX_LOG_ENTRIES {
                let excess = state.log.len() - MAX_LOG_ENTRIES;
                state.log.drain(..excess);
            }
            drop(state);

            // Delay between rules for the same SMS
            if i + 1 < enabled_rules.len() {
                std::thread::sleep(Duration::from_millis(INTER_RULE_DELAY_MS));
            }
        }

        let delivered_now: Vec<u32> =
            self.delivered.lock().unwrap().iter().filter(|(id, _)| *id == sms.id).map(|(_, r)| *r).collect();
        let (outcome, alert) = {
            let mut state = self.state.lock().unwrap();
            let (outcome, alert) = settle_hold(&mut state.held, sms.id, delivery_outcome(&errors), &delivered_now, now_ts());
            if alert.is_some() {
                for e in state.log.iter_mut().filter(|e| e.sms_id == sms.id && !e.success) {
                    if let Some(err) = e.error.as_mut() {
                        if !err.contains(HOLD_TIMEOUT_NOTE) {
                            *err = format!("{err}（{HOLD_TIMEOUT_NOTE}）");
                        }
                    }
                }
            }
            save_state(&state);
            (outcome, alert)
        };
        if let Some(text) = alert {
            eprintln!("[sms_forward] SMS {} {HOLD_TIMEOUT_NOTE} (held over {HOLD_TIMEOUT_SECS}s online)", sms.id);
            crate::alerts::raise("sms-forward-stuck", &text);
        }
        match &outcome {
            Delivery::Deferred(e) => {
                eprintln!("[sms_forward] SMS {} deferred ({e}); will resume from it on the next wake-up", sms.id);
                std::thread::sleep(Duration::from_millis(INTER_SMS_DELAY_MS));
                return outcome;
            }
            Delivery::Skipped(e) => eprintln!("[sms_forward] SMS {} skipped after a permanent error: {e}", sms.id),
            Delivery::Delivered => {}
        }
        self.delivered.lock().unwrap().clear();
        let all_succeeded = errors.is_empty();

        // Post-forward actions — only when ALL rules succeeded
        if all_succeeded && config.mark_read_after_forward {
            if let Err(e) = ubus::call(
                "zwrt_wms",
                "zwrt_wms_modify_tag",
                Some(&json!({"id": sms.id.to_string(), "tag": 0}).to_string()),
            ) {
                eprintln!("[sms_forward] mark-read failed for SMS {}: {e}", sms.id);
            }
        }
        if all_succeeded && config.delete_after_forward {
            if let Err(e) = ubus::call(
                "zwrt_wms",
                "zwrt_wms_delete_sms",
                Some(&json!({"id": sms.id.to_string()}).to_string()),
            ) {
                eprintln!("[sms_forward] delete failed for SMS {}: {e}", sms.id);
            }
        }

        // Delay between different SMS messages
        std::thread::sleep(Duration::from_millis(INTER_SMS_DELAY_MS));
        outcome
    }
}

/// Pre-forward connectivity failures: transient, the SMS waits for the link.
const NO_SERVICE_ERR: &str = "no cellular service (will retry)";
const NO_WAN_ERR: &str = "no WAN connectivity (will retry)";

/// What happened to one SMS.
#[derive(Debug, Clone, PartialEq)]
enum Delivery {
    /// Every matching rule succeeded (or none matched): move past it.
    Delivered,
    /// Some rule failed permanently (4xx / config): logged, move past it.
    Skipped(String),
    /// Some rule failed transiently (no link, connection error, timeout,
    /// 5xx, retries used up): stop here, resume from this SMS next time.
    Deferred(String),
}

/// Rule errors of one SMS → its outcome. Any transient error wins: the SMS
/// is held so that rule gets it later.
fn delivery_outcome(errors: &[String]) -> Delivery {
    if let Some(e) = errors.iter().find(|e| is_transient_error(e)) {
        return Delivery::Deferred(e.clone());
    }
    match errors.first() {
        Some(e) => Delivery::Skipped(e.clone()),
        None => Delivery::Delivered,
    }
}

/// Append a log entry; a retry of the same SMS + rule whose last entry
/// failed updates that entry instead (a deferred SMS is tried on every
/// wake-up — one line per SMS and rule, still retryable by hand).
fn push_log(log: &mut Vec<ForwardLogEntry>, entry: ForwardLogEntry) {
    if let Some(prev) = log.iter_mut().rev().find(|e| e.sms_id == entry.sms_id && e.rule_id == entry.rule_id) {
        if !prev.success {
            *prev = entry;
            return;
        }
    }
    log.push(entry);
}

/// datad's `sms` block summary, if subscribed and fresh.
fn sms_summary() -> Option<(u64, u64)> {
    datad_feed::global().and_then(|f| f.view().sms_summary())
}

// ── paging (T10 / R16) ──────────────────────────────────────────────

/// Page size for `sms.list_after` and the direct reads (datad's limit cap).
const LIST_PAGE: u64 = 50;
/// Pages per store before a direct read gives up (as datad: 20 × 50).
const LIST_MAX_PAGES: u64 = 20;

/// One `sms.list_after` page: items with id > after_id, ascending.
#[derive(Debug, Clone, Default)]
struct Page {
    items: Vec<Value>,
    has_more: bool,
}

/// One item → `(id, received SMS if tag 0/1)`. Field names as
/// `zte_libwms_get_sms_data` (datad returns the same, with plaintext).
fn decode_item(item: &Value) -> (u64, Option<DecodedSms>) {
    let id = item_id(item);
    // tag: 0=unread, 1=read, 2=sent, 3=draft — only forward received (0/1)
    let tag_num = item["tag"]
        .as_u64()
        .or_else(|| item["tag"].as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0);
    if tag_num >= 2 {
        return (id, None);
    }
    let sender_raw = item["number"].as_str().unwrap_or("");
    let content_raw = item["content"].as_str().unwrap_or("");
    let date = item["date"]
        .as_str()
        .or_else(|| item["received_time"].as_str())
        .or_else(|| item["sms_time"].as_str())
        .unwrap_or("")
        .to_string();
    let sender = if is_ucs2_hex(sender_raw) { decode_ucs2_hex(sender_raw) } else { sender_raw.to_string() };
    let content = if is_ucs2_hex(content_raw) { decode_ucs2_hex(content_raw) } else { content_raw.to_string() };
    (id, Some(DecodedSms { id, sender, content, date }))
}

fn max_time(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (x, None) => x,
        (None, y) => y,
    }
}

/// Page through the device's SMS above the watermark and forward them. Pure:
/// `fetch(after_id, limit)` is datad's `sms.list_after` or the direct read
/// ([`list_after_direct`]) — the same paging either way; `forward` delivers
/// one SMS; `commit(watermark, last_time)` persists.
///
/// * `max_id` (device max, both stores) == watermark or 0 → nothing to do.
/// * Normal: `after_id` = watermark; each returned item (sent/draft too, the
///   id counter is shared) moves the watermark to its id and is committed at
///   once, so an interruption resumes right after the last handled item.
/// * Rollback (`max_id` < watermark, R15): page the whole list from 0; only
///   after every page read OK, [`plan_forward`] picks what is newer than the
///   last forwarded time. A failed page moves nothing.
///
/// Returns how many SMS were handled; an error after partial progress
/// leaves that progress committed. A [`Delivery::Deferred`] SMS ends the
/// pass without moving past it (nothing after it is sent either, so the
/// order holds); a skipped one moves on like a delivered one.
fn run_pass(
    watermark: u64,
    last_time: Option<i64>,
    max_id: u64,
    fetch: &mut dyn FnMut(u64, u64) -> Result<Page, String>,
    forward: &mut dyn FnMut(&DecodedSms) -> Delivery,
    commit: &mut dyn FnMut(u64, Option<i64>),
) -> Result<usize, String> {
    if max_id == 0 || max_id == watermark {
        return Ok(0);
    }
    let mut sent = 0;
    if max_id < watermark {
        let mut after = 0;
        let mut all: Vec<Value> = Vec::new();
        loop {
            let page = fetch(after, LIST_PAGE)?;
            let last = page.items.iter().map(item_id).max().unwrap_or(after);
            if page.has_more && last <= after {
                return Err(format!("sms.list_after made no progress after id {after}"));
            }
            after = last;
            all.extend(page.items);
            if !page.has_more {
                break;
            }
        }
        let mut snap = DeviceSnapshot { max_id, received: Vec::new() };
        let mut decoded: Vec<DecodedSms> = Vec::new();
        for item in &all {
            let (id, sms) = decode_item(item);
            snap.max_id = snap.max_id.max(id);
            if let Some(sms) = sms {
                snap.received.push((id, sms_time_key(&sms.date)));
                decoded.push(sms);
            }
        }
        let plan = plan_forward(watermark, last_time, Ok(&snap));
        let mut lt = last_time;
        for id in &plan.forward_ids {
            if let Some(sms) = decoded.iter().find(|m| m.id == *id) {
                if let Delivery::Deferred(_) = forward(sms) {
                    // Last time stays before this SMS: the next pass plans it again.
                    return Ok(sent);
                }
                sent += 1;
                lt = max_time(lt, sms_time_key(&sms.date));
                commit(watermark, lt);
            }
        }
        commit(plan.new_watermark, plan.new_last_time);
        return Ok(sent);
    }
    let (mut wm, mut lt) = (watermark, last_time);
    loop {
        let page = fetch(wm, LIST_PAGE)?;
        let mut progressed = false;
        for item in &page.items {
            let (id, sms) = decode_item(item);
            if id <= wm {
                continue;
            }
            progressed = true;
            if let Some(sms) = sms {
                if let Delivery::Deferred(_) = forward(&sms) {
                    // Watermark stays before this SMS: resume from it.
                    return Ok(sent);
                }
                sent += 1;
                lt = max_time(lt, sms_time_key(&sms.date));
            }
            wm = id;
            commit(wm, lt);
        }
        if !page.has_more {
            return Ok(sent);
        }
        if !progressed {
            return Err(format!("sms.list_after made no progress after id {wm}"));
        }
    }
}

/// `sms.list_after` without datad: the firmware only takes
/// `"order by id desc"`, so each store (NV = 1, SIM = 0; one shared id
/// counter) is read page by page from the top until an id ≤ `after_id`, a
/// short page, or a page with nothing new; merged, deduplicated, ascending,
/// first `limit`. Either store failing fails the whole read (R15). Same
/// rules as datad's (`data-service` `sms::list_after_with`).
fn list_after_direct(
    after_id: u64,
    limit: u64,
    fetch: &mut dyn FnMut(u64, u64) -> Result<Value, String>,
) -> Result<Page, String> {
    let mut found: std::collections::BTreeMap<u64, Value> = std::collections::BTreeMap::new();
    for store in [1u64, 0] {
        let mut page = 0;
        loop {
            if page >= LIST_MAX_PAGES {
                return Err(format!("mem_store {store}: more than {} SMS above id {after_id}", LIST_MAX_PAGES * LIST_PAGE));
            }
            let reply = fetch(store, page).map_err(|e| format!("mem_store {store}: {e}"))?;
            let items = reply["messages"].as_array().cloned().unwrap_or_default();
            let (mut reached, mut fresh) = (false, false);
            for item in items.iter() {
                let id = item_id(item);
                if id <= after_id {
                    reached = true;
                } else if let std::collections::btree_map::Entry::Vacant(slot) = found.entry(id) {
                    fresh = true;
                    slot.insert(item.clone());
                }
            }
            if reached || !fresh || (items.len() as u64) < LIST_PAGE {
                break;
            }
            page += 1;
        }
    }
    let has_more = found.len() as u64 > limit;
    Ok(Page { items: found.into_values().take(limit as usize).collect(), has_more })
}

/// One direct `zte_libwms_get_sms_data` page (descending; ascending is rejected).
fn ubus_sms_page(store: u64, page: u64) -> Result<Value, String> {
    let params = json!({
        "tags": 10,
        "page": page,
        "data_per_page": LIST_PAGE,
        "mem_store": store,
        "order_by": "order by id desc",
    });
    ubus::call("zwrt_wms", "zte_libwms_get_sms_data", Some(&params.to_string()))
}

/// One `sms.list_after` page from datad; `503 busy` retried after `delays`.
fn datad_page(addr: std::net::SocketAddr, after: u64, limit: u64, delays: &[Duration]) -> Result<Page, String> {
    let params = json!({"after_id": after, "limit": limit});
    let r = datad_feed::control_retry(addr, "sms.list_after", &params, Duration::from_secs(60), delays)
        .map_err(|e| e.to_string())?;
    let items = r["items"].as_array().cloned().ok_or("sms.list_after: no items")?;
    let has_more = r["has_more"].as_bool().ok_or("sms.list_after: no has_more")?;
    Ok(Page { items, has_more })
}

fn preview(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..s.floor_char_boundary(max)])
    }
}

fn forward_with_retry(
    dest: &ForwardDestination,
    sms: &DecodedSms,
    agent: &ureq::Agent,
) -> Result<(), String> {
    let mut last_err = String::new();

    for attempt in 0..MAX_RETRIES {
        match forward_to(dest, sms, agent) {
            Ok(()) => return Ok(()),
            Err(e) => {
                eprintln!(
                    "[sms_forward] attempt {}/{MAX_RETRIES} failed for SMS {} -> {}: {e}",
                    attempt + 1,
                    sms.id,
                    destination_type_name(dest),
                );
                if !is_transient_error(&e) {
                    return Err(e);
                }
                last_err = e;
                if attempt + 1 < MAX_RETRIES {
                    std::thread::sleep(Duration::from_secs(RETRY_DELAYS[attempt as usize]));
                }
            }
        }
    }

    Err(format!("{last_err} (failed after {MAX_RETRIES} retries)"))
}

// ── ubus helpers ────────────────────────────────────────────────────

/// Query SMS from both NV and SIM storage, merged and deduplicated by ID.
/// ZTE firmware bug: mem_store=2 ("all") silently omits SIM messages.
/// Either store failing fails the whole read — a half list must never be
/// mistaken for "the device's ids went backwards" (R15).
fn fetch_sms_both_stores(tags: u64, page: u64, count: u64, order: &str) -> Result<Vec<Value>, String> {
    // NV (1) first — more common, then SIM (0)
    let results: Vec<(u64, Result<Value, String>)> = [1u64, 0]
        .into_iter()
        .map(|store| {
            let params = json!({
                "tags": tags,
                "page": page,
                "data_per_page": count,
                "mem_store": store,
                "order_by": order,
            });
            (store, ubus::call("zwrt_wms", "zte_libwms_get_sms_data", Some(&params.to_string())))
        })
        .collect();
    merge_store_results(results)
}

fn item_id(item: &Value) -> u64 {
    item["id"]
        .as_u64()
        .or_else(|| item["id"].as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0)
}

/// Pure half of `fetch_sms_both_stores`.
fn merge_store_results(results: Vec<(u64, Result<Value, String>)>) -> Result<Vec<Value>, String> {
    let mut all: Vec<Value> = Vec::new();
    let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
    for (store, res) in results {
        let data = res.map_err(|e| format!("mem_store {store}: {e}"))?;
        if let Some(arr) = data["messages"].as_array() {
            for item in arr {
                if seen.insert(item_id(item)) {
                    all.push(item.clone());
                }
            }
        }
    }
    Ok(all)
}

/// Get the maximum SMS id currently on the device.
fn fetch_max_sms_id() -> Result<u64, String> {
    let all = fetch_sms_both_stores(10, 0, 5, "order by id desc")?;

    let max_id = all
        .iter()
        .filter_map(|item| {
            item["id"]
                .as_u64()
                .or_else(|| item["id"].as_str().and_then(|s| s.parse().ok()))
        })
        .max()
        .unwrap_or(0);

    Ok(max_id)
}

// ── Persistence ─────────────────────────────────────────────────────

fn save_config(config: &SmsForwardConfig) {
    if let Ok(json) = serde_json::to_string_pretty(config) {
        let _ = fs::write(CONFIG_PATH, json);
    }
}

fn save_state(state: &ForwardState) {
    if let Ok(json) = serde_json::to_string(state) {
        let _ = fs::write(STATE_PATH, json);
    }
}

// ── HTTP handlers ───────────────────────────────────────────────────

/// GET /api/sms/forward/config
pub fn config_get(state: &AppState) -> (u16, Value) {
    let config = state.sms_forward.config.lock().unwrap();
    let fwd_state = state.sms_forward.state.lock().unwrap();
    (
        200,
        json!({
            "ok": true,
            "data": {
                "config": *config,
                "last_forwarded_id": fwd_state.last_forwarded_id,
            }
        }),
    )
}

/// PUT /api/sms/forward/config
pub fn config_set(state: &AppState, body: &[u8]) -> (u16, Value) {
    #[derive(Deserialize)]
    struct Req {
        #[serde(default)]
        enabled: Option<bool>,
        #[serde(default)]
        poll_interval_secs: Option<u64>,
        #[serde(default)]
        mark_read_after_forward: Option<bool>,
        #[serde(default)]
        delete_after_forward: Option<bool>,
    }

    let req: Req = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return (400, json!({"ok": false, "error": format!("invalid JSON: {e}")})),
    };

    let mut config = state.sms_forward.config.lock().unwrap();

    if let Some(enabled) = req.enabled {
        config.enabled = enabled;
    }
    if let Some(interval) = req.poll_interval_secs {
        if interval < 10 {
            return (400, json!({"ok": false, "error": "poll_interval_secs must be >= 10"}));
        }
        config.poll_interval_secs = interval;
    }
    if let Some(v) = req.mark_read_after_forward {
        config.mark_read_after_forward = v;
    }
    if let Some(v) = req.delete_after_forward {
        config.delete_after_forward = v;
    }

    save_config(&config);
    (200, json!({"ok": true, "data": *config}))
}

/// POST /api/sms/forward/rules — Create a new rule
pub fn rules_create(state: &AppState, body: &[u8]) -> (u16, Value) {
    #[derive(Deserialize)]
    struct Req {
        name: String,
        #[serde(default = "default_true")]
        enabled: bool,
        filter: SmsFilter,
        destination: ForwardDestination,
    }

    let req: Req = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return (400, json!({"ok": false, "error": format!("invalid JSON: {e}")})),
    };

    let mut config = state.sms_forward.config.lock().unwrap();
    let next_id = config.rules.iter().map(|r| r.id).max().unwrap_or(0) + 1;

    let rule = ForwardRule {
        id: next_id,
        name: req.name,
        enabled: req.enabled,
        filter: req.filter,
        destination: req.destination,
    };

    let result = json!({"ok": true, "data": rule});
    config.rules.push(rule);
    save_config(&config);
    (201, result)
}

fn default_true() -> bool {
    true
}

/// PUT /api/sms/forward/rules — Update a rule
pub fn rules_update(state: &AppState, body: &[u8]) -> (u16, Value) {
    #[derive(Deserialize)]
    struct Req {
        id: u32,
        name: String,
        enabled: bool,
        filter: SmsFilter,
        destination: ForwardDestination,
    }

    let req: Req = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return (400, json!({"ok": false, "error": format!("invalid JSON: {e}")})),
    };

    let mut config = state.sms_forward.config.lock().unwrap();
    let rule = match config.rules.iter_mut().find(|r| r.id == req.id) {
        Some(r) => r,
        None => return (404, json!({"ok": false, "error": "rule not found"})),
    };

    rule.name = req.name;
    rule.enabled = req.enabled;
    rule.filter = req.filter;
    rule.destination = req.destination;

    let result = json!({"ok": true, "data": rule.clone()});
    save_config(&config);
    (200, result)
}

/// DELETE /api/sms/forward/rules — Delete a rule
pub fn rules_delete(state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let id = match parsed["id"].as_u64() {
        Some(id) => id as u32,
        None => return (400, json!({"ok": false, "error": "missing 'id' field"})),
    };

    let mut config = state.sms_forward.config.lock().unwrap();
    let len_before = config.rules.len();
    config.rules.retain(|r| r.id != id);

    if config.rules.len() == len_before {
        return (404, json!({"ok": false, "error": "rule not found"}));
    }

    save_config(&config);
    (200, json!({"ok": true}))
}

/// PUT /api/sms/forward/rules/toggle — Enable/disable a rule
pub fn rules_toggle(state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let id = match parsed["id"].as_u64() {
        Some(id) => id as u32,
        None => return (400, json!({"ok": false, "error": "missing 'id' field"})),
    };
    let enabled = match parsed["enabled"].as_bool() {
        Some(e) => e,
        None => return (400, json!({"ok": false, "error": "missing 'enabled' field"})),
    };

    let mut config = state.sms_forward.config.lock().unwrap();
    let rule = match config.rules.iter_mut().find(|r| r.id == id) {
        Some(r) => r,
        None => return (404, json!({"ok": false, "error": "rule not found"})),
    };

    rule.enabled = enabled;
    let result = json!({"ok": true, "data": rule.clone()});
    save_config(&config);
    (200, result)
}

/// POST /api/sms/forward/test — Send a test message
pub fn test_forward(state: &AppState, body: &[u8]) -> (u16, Value) {
    #[derive(Deserialize)]
    struct Req {
        destination: ForwardDestination,
    }

    let req: Req = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return (400, json!({"ok": false, "error": format!("invalid JSON: {e}")})),
    };

    let _ = state; // keep signature consistent

    let test_sms = DecodedSms {
        id: 0,
        sender: "+1234567890".to_string(),
        content: "This is a test message from zte-agent SMS forwarder.".to_string(),
        date: "2025-01-01 12:00:00".to_string(),
    };

    let agent = http_agent();
    match forward_to(&req.destination, &test_sms, &agent) {
        Ok(()) => (200, json!({"ok": true, "data": {"status": "sent"}})),
        Err(e) => (502, json!({"ok": false, "error": e})),
    }
}

/// GET /api/sms/forward/log — returns newest-first
pub fn log_get(state: &AppState) -> (u16, Value) {
    let fwd_state = state.sms_forward.state.lock().unwrap();
    let mut log = fwd_state.log.clone();
    log.reverse();
    (200, json!({"ok": true, "data": log}))
}

/// POST /api/sms/forward/log/clear
pub fn log_clear(state: &AppState) -> (u16, Value) {
    let mut fwd_state = state.sms_forward.state.lock().unwrap();
    fwd_state.log.clear();
    save_state(&fwd_state);
    (200, json!({"ok": true}))
}

/// POST /api/sms/forward/retry — Retry a failed forward log entry
pub fn retry_forward(state: &AppState, body: &[u8]) -> (u16, Value) {
    #[derive(Deserialize)]
    struct Req {
        index: usize,
    }

    let req: Req = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return (400, json!({"ok": false, "error": format!("invalid JSON: {e}")})),
    };

    // Convert reversed index (newest-first) to internal chronological index
    let (entry_clone, rule_id, internal_index) = {
        let fwd_state = state.sms_forward.state.lock().unwrap();
        let internal_index = match fwd_state.log.len().checked_sub(1 + req.index) {
            Some(i) => i,
            None => return (404, json!({"ok": false, "error": "log entry not found"})),
        };
        let entry = match fwd_state.log.get(internal_index) {
            Some(e) => e,
            None => return (404, json!({"ok": false, "error": "log entry not found"})),
        };
        if entry.success {
            return (400, json!({"ok": false, "error": "entry already succeeded"}));
        }
        (entry.clone(), entry.rule_id, internal_index)
    };

    // Look up rule by id from current config
    let dest = {
        let config = state.sms_forward.config.lock().unwrap();
        match config.rules.iter().find(|r| r.id == rule_id) {
            Some(rule) => rule.destination.clone(),
            None => return (404, json!({"ok": false, "error": "rule no longer exists"})),
        }
    };

    // Reconstruct the SMS from stored log data
    let sms = DecodedSms {
        id: entry_clone.sms_id,
        sender: entry_clone.sender.clone(),
        content: entry_clone.content.clone(),
        date: entry_clone.date.clone(),
    };

    // Single attempt — no auto-retry for manual retries
    let agent = http_agent();
    let result = forward_to(&dest, &sms, &agent);

    // Update the log entry
    let mut guard = state.sms_forward.state.lock().unwrap();
    let fwd_state = &mut *guard;
    if let Some(entry) = fwd_state.log.get_mut(internal_index) {
        match &result {
            Ok(()) => {
                entry.success = true;
                entry.error = None;
                entry.timestamp = now_ts();
                // A held SMS must not go to this rule again automatically.
                state.sms_forward.delivered.lock().unwrap().insert((entry.sms_id, rule_id));
                if let Some(h) = fwd_state.held.as_mut().filter(|h| h.sms_id == entry.sms_id) {
                    if !h.delivered_rules.contains(&rule_id) {
                        h.delivered_rules.push(rule_id);
                    }
                }
            }
            Err(e) => {
                entry.error = Some(e.clone());
                entry.timestamp = now_ts();
            }
        }
    }
    save_state(fwd_state);

    match result {
        Ok(()) => (200, json!({"ok": true, "data": {"status": "sent"}})),
        Err(e) => (502, json!({"ok": false, "error": e})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(max_id: u64, received: &[(u64, Option<i64>)]) -> DeviceSnapshot {
        DeviceSnapshot { max_id, received: received.to_vec() }
    }

    /// 短信同类场景: datad drops, an SMS arrives, the direct poll (within
    /// 60 s) forwards it; datad comes back, the source-switch pass finds
    /// nothing new — no second send.
    #[test]
    fn sms_during_fallback_forwarded_once() {
        let s = |x: u64| Duration::from_secs(x);
        let mut sched = PollSchedule::new(s(0), true);
        let mut dev = FakeDev::new(1..=100);
        let mut st = Persisted::new(100, Some(1_000_000));
        let mut sent: Vec<u64> = Vec::new();
        // datad drops at 100 → source switch, checks once (nothing new).
        assert!(sched.source_changed(false));
        st.pass(&dev, None, &mut sent).unwrap();
        sched.polled(s(100));
        // SMS 101 arrives at 110 (no datad wake).
        dev.add(101..=101);
        let mut now = 110;
        while !sched.due(s(now)) {
            let w = sched.wait(s(now));
            assert!(w <= Duration::from_secs(60));
            now += w.as_secs().max(1);
        }
        assert!(now - 110 <= 60, "direct poll within 60 s, got {}", now - 110);
        st.pass(&dev, None, &mut sent).unwrap();
        sched.polled(s(now));
        assert_eq!(sent, vec![101]);
        // datad back: source switch, pass via datad, nothing new.
        assert!(sched.source_changed(true));
        assert!(!sched.source_changed(true), "same source: no extra wake");
        st.pass(&dev, None, &mut sent).unwrap();
        sched.polled(s(now + 30));
        assert_eq!(sent, vec![101], "no re-send after recovery");
        assert!(!sched.due(s(now + 30 + 299)), "via datad: back to the 5-min net");
    }

    // ── T10 paging ──

    /// A device: ids in two stores (multiples of 5 on SIM), one shared
    /// counter; `zte_libwms_get_sms_data` pages descending only.
    struct FakeDev {
        ids: Vec<u64>,
        sent_tags: Vec<u64>,
    }

    impl FakeDev {
        fn new(ids: impl IntoIterator<Item = u64>) -> Self {
            FakeDev { ids: ids.into_iter().collect(), sent_tags: Vec::new() }
        }
        fn add(&mut self, ids: impl IntoIterator<Item = u64>) {
            self.ids.extend(ids);
        }
        fn max_id(&self) -> u64 {
            self.ids.iter().copied().max().unwrap_or(0)
        }
        fn store_page(&self, args: &Value) -> Result<Value, String> {
            if args["order_by"] != "order by id desc" {
                return Err("Invalid argument".into());
            }
            let sim = args["mem_store"] == 0;
            let mut ids: Vec<u64> = self.ids.iter().copied().filter(|i| (i % 5 == 0) == sim).collect();
            ids.sort_unstable_by(|a, b| b.cmp(a));
            let per = args["data_per_page"].as_u64().unwrap() as usize;
            let page = args["page"].as_u64().unwrap() as usize;
            let rows: Vec<Value> = ids
                .iter()
                .skip(page * per)
                .take(per)
                .map(|&id| {
                    let tag = if self.sent_tags.contains(&id) { "2" } else { "1" };
                    // 00:00 + id minutes, so later ids are later in time.
                    let date = format!("26,08,27,{:02},{:02},00,+32", (id / 60) % 24, id % 60);
                    json!({"id": id.to_string(), "number": "0031003000300038003600", "content": "0041", "tag": tag, "date": date})
                })
                .collect();
            Ok(json!({"messages": rows}))
        }
        /// datad's `sms.list_after` over this device (same algorithm).
        fn list_after(&self, after: u64, limit: u64, calls: &mut usize) -> Result<Page, String> {
            *calls += 1;
            list_after_direct(after, limit, &mut |store, page| {
                self.store_page(&json!({"tags": 10, "page": page, "data_per_page": LIST_PAGE, "mem_store": store, "order_by": "order by id desc"}))
            })
        }
    }

    /// The forwarder's state file, round-tripped through JSON on every
    /// commit like `save_state` / `SmsForwarder::new`.
    struct Persisted {
        json: String,
    }

    impl Persisted {
        fn new(wm: u64, lt: Option<i64>) -> Self {
            let st = ForwardState { last_forwarded_id: wm, last_forwarded_time: lt, log: Vec::new(), held: None };
            Persisted { json: serde_json::to_string(&st).unwrap() }
        }
        fn load(&self) -> (u64, Option<i64>) {
            let st: ForwardState = serde_json::from_str(&self.json).unwrap();
            (st.last_forwarded_id, st.last_forwarded_time)
        }
        /// One pass; `fail_on_call` makes that fetch call fail (interruption).
        fn pass(&mut self, dev: &FakeDev, fail_on_call: Option<usize>, sent: &mut Vec<u64>) -> Result<usize, String> {
            let (wm, lt) = self.load();
            let mut calls = 0;
            let mut fetch = |after: u64, limit: u64| {
                if fail_on_call == Some(calls + 1) {
                    return Err("datad went away".to_string());
                }
                dev.list_after(after, limit, &mut calls)
            };
            let mut forward = |sms: &DecodedSms| {
                sent.push(sms.id);
                Delivery::Delivered
            };
            let json = &mut self.json;
            let mut commit = |wm: u64, lt: Option<i64>| {
                let st = ForwardState { last_forwarded_id: wm, last_forwarded_time: lt, log: Vec::new(), held: None };
                *json = serde_json::to_string(&st).unwrap();
            };
            run_pass(wm, lt, dev.max_id(), &mut fetch, &mut forward, &mut commit)
        }
    }

    /// One pass where `outcome(id)` decides each delivery.
    fn pass_with(st: &mut Persisted, dev: &FakeDev, sent: &mut Vec<u64>, outcome: &dyn Fn(u64) -> Delivery) -> usize {
        let (wm, lt) = st.load();
        let mut calls = 0;
        let json = &mut st.json;
        run_pass(
            wm,
            lt,
            dev.max_id(),
            &mut |a, l| dev.list_after(a, l, &mut calls),
            &mut |s: &DecodedSms| {
                let o = outcome(s.id);
                if !matches!(o, Delivery::Deferred(_)) {
                    sent.push(s.id);
                }
                o
            },
            &mut |w, t| *json = serde_json::to_string(&ForwardState { last_forwarded_id: w, last_forwarded_time: t, log: Vec::new(), held: None }).unwrap(),
        )
        .unwrap()
    }

    /// No link: 3 SMS arrive, nothing moves; link back: all 3 in order, once.
    #[test]
    fn sms_offline_held_then_forwarded_in_order() {
        let mut dev = FakeDev::new(1..=10);
        let mut st = Persisted::new(10, Some(0));
        dev.add(11..=13);
        let mut sent = Vec::new();
        let offline = |_| delivery_outcome(&[NO_WAN_ERR.to_string()]);
        for _ in 0..3 {
            assert_eq!(pass_with(&mut st, &dev, &mut sent, &offline), 0);
            assert_eq!(st.load(), (10, Some(0)), "watermark and last time held");
        }
        assert!(sent.is_empty());
        assert_eq!(pass_with(&mut st, &dev, &mut sent, &|_| Delivery::Delivered), 3);
        once_each(&sent, 11..=13);
        assert_eq!(st.load().0, 13);
        assert_eq!(pass_with(&mut st, &dev, &mut sent, &|_| Delivery::Delivered), 0, "no repeat");
        once_each(&sent, 11..=13);
    }

    /// A permanent (4xx) failure skips that SMS and moves on.
    #[test]
    fn sms_permanent_error_skipped() {
        let mut dev = FakeDev::new(1..=10);
        let mut st = Persisted::new(10, Some(0));
        dev.add(11..=13);
        let mut sent = Vec::new();
        let perm = delivery_outcome(&["telegram: HTTP 400 (permanent error)".to_string()]);
        assert!(matches!(perm, Delivery::Skipped(_)));
        let n = pass_with(&mut st, &dev, &mut sent, &|id| if id == 12 { perm.clone() } else { Delivery::Delivered });
        assert_eq!(n, 3);
        assert_eq!(sent, vec![11, 12, 13]);
        assert_eq!(st.load().0, 13, "moved past the skipped one");
    }

    /// Link drops in the middle: held at the failing SMS, resumes from it.
    #[test]
    fn sms_deferred_midway_resumes() {
        let mut dev = FakeDev::new(1..=10);
        let mut st = Persisted::new(10, Some(0));
        dev.add(11..=13);
        let mut sent = Vec::new();
        let n = pass_with(&mut st, &dev, &mut sent, &|id| {
            if id >= 12 { delivery_outcome(&["slack: io: connection refused".to_string()]) } else { Delivery::Delivered }
        });
        assert_eq!(n, 1);
        assert_eq!(st.load().0, 11, "held before 12");
        assert_eq!(pass_with(&mut st, &dev, &mut sent, &|_| Delivery::Delivered), 2);
        once_each(&sent, 11..=13);
    }

    /// Two rules: 1 = a target that may fail with 5xx, 2 = always fine.
    /// Mirrors `deliver`: tick the online clock, skip rules already
    /// delivered, settle the hold, persist. `state` is the JSON state file.
    struct HoldSim {
        state: String,
        clock: OnlineClock,
        now: u64,
        online: bool,
        /// Rule 1 answers 503 for this SMS id.
        broken_for: u64,
        sends: Vec<(u64, u32)>,
        alerts: Vec<String>,
    }

    impl HoldSim {
        fn new(wm: u64) -> Self {
            let st = ForwardState { last_forwarded_id: wm, last_forwarded_time: Some(0), ..Default::default() };
            HoldSim { state: serde_json::to_string(&st).unwrap(), clock: OnlineClock::default(), now: 1000, online: true, broken_for: 11, sends: Vec::new(), alerts: Vec::new() }
        }
        fn st(&self) -> ForwardState {
            serde_json::from_str(&self.state).unwrap()
        }
        /// zte-agent restart: in-memory clock gone, state file kept.
        fn restart(&mut self) {
            self.clock = OnlineClock::default();
        }
        /// Time passes; `online` changes are observed like connectivity_monitor.
        fn advance(&mut self, secs: u64, online_after: bool) {
            self.now += secs;
            let mut st = self.st();
            let add = self.clock.observe(self.now, online_after);
            if let Some(h) = st.held.as_mut() {
                h.online_secs += add;
            }
            self.online = online_after;
            self.state = serde_json::to_string(&st).unwrap();
        }
        fn pass(&mut self, dev: &FakeDev) -> usize {
            let st0 = self.st();
            let mut calls = 0;
            let cell = std::cell::RefCell::new(st0.clone());
            let (clock, now, online, broken_for) = (&mut self.clock, self.now, self.online, self.broken_for);
            let (sends, alerts) = (&mut self.sends, &mut self.alerts);
            let n = run_pass(
                st0.last_forwarded_id,
                st0.last_forwarded_time,
                dev.max_id(),
                &mut |a, l| dev.list_after(a, l, &mut calls),
                &mut |sms: &DecodedSms| {
                    let mut st = cell.borrow_mut();
                    let add = clock.observe(now, online);
                    if let Some(h) = st.held.as_mut() {
                        h.online_secs += add;
                    }
                    let mut done: Vec<u32> = st.held.iter().filter(|h| h.sms_id == sms.id).flat_map(|h| h.delivered_rules.clone()).collect();
                    let mut errors = Vec::new();
                    for rule in [1u32, 2] {
                        if done.contains(&rule) {
                            continue;
                        }
                        if !online {
                            errors.push(NO_WAN_ERR.to_string());
                        } else if rule == 1 && sms.id == broken_for {
                            errors.push("webhook: HTTP 503".to_string());
                        } else {
                            sends.push((sms.id, rule));
                            done.push(rule);
                        }
                    }
                    let (o, alert) = settle_hold(&mut st.held, sms.id, delivery_outcome(&errors), &done, 0);
                    alerts.extend(alert);
                    o
                },
                &mut |w, t| {
                    let mut st = cell.borrow_mut();
                    st.last_forwarded_id = w;
                    st.last_forwarded_time = t;
                },
            )
            .unwrap();
            self.state = serde_json::to_string(&*cell.borrow()).unwrap();
            n
        }
    }

    /// A target stuck on 5xx holds its SMS for 6 h online, then that SMS is
    /// skipped (alert once) and later SMS flow again.
    #[test]
    fn sms_hold_times_out_after_6h_online() {
        let mut dev = FakeDev::new(1..=10);
        dev.add(11..=12);
        let mut sim = HoldSim::new(10);
        assert_eq!(sim.pass(&dev), 0);
        assert_eq!(sim.st().held.as_ref().unwrap().delivered_rules, vec![2]);
        for _ in 0..5 {
            sim.advance(3600, true);
            assert_eq!(sim.pass(&dev), 0);
        }
        assert_eq!(sim.st().last_forwarded_id, 10, "5 h: still held");
        sim.advance(3600, true);
        assert_eq!(sim.pass(&dev), 2);
        assert_eq!(sim.st().last_forwarded_id, 12);
        assert!(sim.st().held.is_none());
        assert_eq!(sim.alerts.len(), 1, "one alert");
        assert!(sim.alerts[0].contains("SMS 11"));
        assert_eq!(sim.sends, vec![(11, 2), (12, 1), (12, 2)], "rule 2 got 11 once");
        assert_eq!(sim.pass(&dev), 0);
        assert_eq!(sim.alerts.len(), 1, "still one alert");
    }

    /// Offline time does not count toward the 6 h.
    #[test]
    fn sms_hold_offline_does_not_count() {
        let mut dev = FakeDev::new(1..=10);
        dev.add(11..=11);
        let mut sim = HoldSim::new(10);
        assert_eq!(sim.pass(&dev), 0);
        sim.advance(3600, false); // 1 h online, then the link drops
        for _ in 0..24 {
            sim.advance(3600, false); // a day offline
            assert_eq!(sim.pass(&dev), 0);
        }
        sim.advance(0, true);
        sim.advance(4 * 3600, true);
        assert_eq!(sim.pass(&dev), 0);
        assert_eq!(sim.st().held.as_ref().unwrap().online_secs, 5 * 3600);
        sim.advance(3600, true);
        assert_eq!(sim.pass(&dev), 1);
        assert_eq!(sim.alerts.len(), 1);
    }

    /// Restart keeps the waited time and the rules already delivered;
    /// an old state file without `held` still loads.
    #[test]
    fn sms_hold_survives_restart() {
        let old: ForwardState = serde_json::from_str(r#"{"last_forwarded_id":10,"log":[]}"#).unwrap();
        assert!(old.held.is_none());
        let mut dev = FakeDev::new(1..=10);
        dev.add(11..=11);
        let mut sim = HoldSim::new(10);
        assert_eq!(sim.pass(&dev), 0);
        sim.advance(4 * 3600, true);
        assert_eq!(sim.pass(&dev), 0);
        sim.restart();
        assert_eq!(sim.pass(&dev), 0, "first tick after restart counts nothing");
        assert_eq!(sim.st().held.as_ref().unwrap().online_secs, 4 * 3600);
        sim.advance(2 * 3600, true);
        assert_eq!(sim.pass(&dev), 1);
        assert_eq!(sim.sends, vec![(11, 2)], "rule 2 not re-sent after restart");
        assert_eq!(sim.alerts.len(), 1);
    }

    #[test]
    fn delivery_outcome_classifies() {
        assert_eq!(delivery_outcome(&[]), Delivery::Delivered);
        for e in [NO_WAN_ERR, NO_SERVICE_ERR, "discord: HTTP 502", "telegram: HTTP 429 rate limited", "x (failed after 3 retries)"] {
            assert!(matches!(delivery_outcome(&[e.to_string()]), Delivery::Deferred(_)), "{e}");
        }
        let mixed = ["a: HTTP 404 (permanent error)".to_string(), "b: HTTP 500".to_string()];
        assert!(matches!(delivery_outcome(&mixed), Delivery::Deferred(_)), "any transient holds");
    }

    #[test]
    fn deferred_retries_update_one_log_line() {
        let e = |ok: bool, rule: u32| ForwardLogEntry {
            timestamp: 0, sms_id: 7, sender: String::new(), content_preview: String::new(), rule_name: String::new(),
            destination_type: String::new(), success: ok, error: None, rule_id: rule, content: String::new(), date: String::new(),
        };
        let mut log = Vec::new();
        push_log(&mut log, e(false, 1));
        push_log(&mut log, e(false, 1));
        push_log(&mut log, e(false, 2));
        assert_eq!(log.len(), 2);
        push_log(&mut log, e(true, 1));
        assert_eq!(log.len(), 2);
        assert!(log[0].success);
        push_log(&mut log, e(true, 1));
        assert_eq!(log.len(), 3, "a success is never overwritten");
    }

    fn once_each(sent: &[u64], want: std::ops::RangeInclusive<u64>) {
        let mut sorted = sent.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted, want.collect::<Vec<_>>(), "each exactly once");
        assert!(sent.windows(2).all(|w| w[0] < w[1]), "in id order");
    }

    /// Verify 1: a burst of 600 is paged 50 at a time, all forwarded, no dup.
    #[test]
    fn sms_burst_600_paged_forwarded_once() {
        let mut dev = FakeDev::new(1..=20);
        let mut st = Persisted::new(20, None);
        dev.add(21..=620);
        let mut sent = Vec::new();
        let mut calls = 0;
        let (wm, lt) = st.load();
        let n = run_pass(
            wm,
            lt,
            dev.max_id(),
            &mut |a, l| dev.list_after(a, l, &mut calls),
            &mut |s: &DecodedSms| {
                sent.push(s.id);
                Delivery::Delivered
            },
            &mut |w, t| st.json = serde_json::to_string(&ForwardState { last_forwarded_id: w, last_forwarded_time: t, log: Vec::new(), held: None }).unwrap(),
        )
        .unwrap();
        assert_eq!(n, 600);
        once_each(&sent, 21..=620);
        assert_eq!(calls, 12, "600 / 50 pages");
        assert_eq!(st.load().0, 620);
        assert!(st.load().1.is_some(), "last forwarded time recorded");
        // Next pass: nothing new, no fetch at all.
        let mut again = Vec::new();
        assert_eq!(st.pass(&dev, None, &mut again).unwrap(), 0);
        assert!(again.is_empty());
    }

    /// Verify 2: the pass dies on page 3; the restart resumes from the
    /// persisted watermark — nothing lost, nothing sent twice.
    #[test]
    fn sms_interrupted_on_page_3_resumes() {
        let dev = FakeDev::new(1..=600);
        let mut st = Persisted::new(0, None);
        // The forwarder starts with the watermark at 0 only on an empty device;
        // here the device is full of unforwarded SMS on purpose.
        let mut sent = Vec::new();
        assert!(st.pass(&dev, Some(3), &mut sent).is_err());
        assert_eq!(sent.len(), 100, "pages 1–2 done");
        assert_eq!(st.load().0, 100, "watermark moved item by item");
        // Restart (fresh state read from the file).
        let mut st = Persisted { json: st.json.clone() };
        st.pass(&dev, None, &mut sent).unwrap();
        once_each(&sent, 1..=600);

        // Sent/draft rows move the cursor too (shared id counter), so a run
        // of them can never pin the watermark.
        let mut dev = FakeDev::new(1..=10);
        dev.add(11..=130);
        dev.sent_tags = (11..=120).collect();
        let mut st = Persisted::new(10, None);
        let mut sent = Vec::new();
        st.pass(&dev, None, &mut sent).unwrap();
        once_each(&sent, 121..=130);
        assert_eq!(st.load().0, 130);
    }

    /// Verify 3: datad is down while 120 SMS arrive and the direct read also
    /// fails part way (one store); back on datad, the source-switch pass
    /// forwards all 120, once.
    #[test]
    fn sms_datad_outage_120_backfilled() {
        let mut dev = FakeDev::new(1..=100);
        let mut st = Persisted::new(100, Some(0));
        let mut sched = PollSchedule::new(Duration::ZERO, true);
        assert!(sched.source_changed(false));
        dev.add(101..=220);
        let mut sent = Vec::new();
        // Direct read with the SIM store failing: nothing moves (R15).
        let (wm, lt) = st.load();
        let r = run_pass(
            wm,
            lt,
            dev.max_id(),
            &mut |a, l| {
                list_after_direct(a, l, &mut |store, page| {
                    if store == 0 {
                        return Err("timeout".into());
                    }
                    dev.store_page(&json!({"page": page, "data_per_page": LIST_PAGE, "mem_store": store, "order_by": "order by id desc"}))
                })
            },
            &mut |s: &DecodedSms| {
                sent.push(s.id);
                Delivery::Delivered
            },
            &mut |_, _| panic!("a failed read must not commit"),
        );
        assert!(r.unwrap_err().contains("mem_store 0"));
        assert!(sent.is_empty());
        // datad back: switch to datad and page through.
        assert!(sched.source_changed(true));
        st.pass(&dev, None, &mut sent).unwrap();
        once_each(&sent, 101..=220);
        assert_eq!(st.load().0, 220);
    }

    /// Rollback through the pager: the whole list is read (every page OK)
    /// before plan_forward decides; newer-than-last-time only.
    #[test]
    fn sms_rollback_pages_whole_list() {
        let dev = FakeDev::new(1..=120);
        let t = |id: u64| sms_time_key(&format!("26,08,27,{:02},{:02},00,+32", (id / 60) % 24, id % 60));
        let mut st = Persisted::new(5000, t(110));
        let mut sent = Vec::new();
        assert!(st.pass(&dev, Some(2), &mut sent).is_err(), "page 2 fails");
        assert!(sent.is_empty());
        assert_eq!(st.load(), (5000, t(110)), "nothing moved");
        st.pass(&dev, None, &mut sent).unwrap();
        once_each(&sent, 111..=120);
        assert_eq!(st.load().0, 120);
    }

    /// datad answers `503 busy` twice, then the page: the HTTP client
    /// retries (bounded) and parses `{items, has_more}`.
    #[test]
    fn sms_list_after_busy_retried_over_http() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut bodies = Vec::new();
            for n in 0..3 {
                let (mut c, _) = listener.accept().unwrap();
                let mut buf = Vec::new();
                let mut b = [0u8; 1024];
                loop {
                    let k = c.read(&mut b).unwrap();
                    buf.extend_from_slice(&b[..k]);
                    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..p]).to_ascii_lowercase();
                        let len: usize = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .map(|v| v.trim().parse().unwrap())
                            .unwrap();
                        if buf.len() >= p + 4 + len {
                            bodies.push(String::from_utf8_lossy(&buf[p + 4..p + 4 + len]).to_string());
                            break;
                        }
                    }
                }
                let (status, body) = if n < 2 {
                    ("503 Service Unavailable", r#"{"ok":false,"action":"sms.list_after","error":{"code":"busy","message":"control queue full"}}"#.to_string())
                } else {
                    ("200 OK", r#"{"ok":true,"action":"sms.list_after","result":{"items":[{"id":8,"number":"0031","content":"0041","tag":"0","date":"d"}],"has_more":false}}"#.to_string())
                };
                let resp = format!("HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}", body.len());
                c.write_all(resp.as_bytes()).unwrap();
            }
            bodies
        });
        let delays = [Duration::from_millis(10); 3];
        let page = datad_page(addr, 7, 50, &delays).unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(item_id(&page.items[0]), 8);
        assert!(!page.has_more);
        let bodies = server.join().unwrap();
        assert_eq!(bodies.len(), 3);
        for b in &bodies {
            let v: Value = serde_json::from_str(b).unwrap();
            assert_eq!(v, json!({"action": "sms.list_after", "params": {"after_id": 7, "limit": 50}}));
        }
        // Busy every time: gives up after the bounded retries.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut n = 0;
            for c in listener.incoming().take(2) {
                let mut c = c.unwrap();
                let mut b = [0u8; 4096];
                let _ = c.read(&mut b);
                let body = r#"{"ok":false,"error":{"code":"busy","message":"control queue full"}}"#;
                let _ = c.write_all(format!("HTTP/1.1 503 Service Unavailable\r\ncontent-length: {}\r\n\r\n{body}", body.len()).as_bytes());
                n += 1;
            }
            n
        });
        let err = datad_page(addr, 0, 50, &[Duration::from_millis(10)]).unwrap_err();
        assert!(err.contains("busy"), "{err}");
        assert_eq!(server.join().unwrap(), 2, "one try + one retry");
    }

    const T0: i64 = 1_000_000;

    #[test]
    fn normal_increasing_ids_forward_only_new() {
        let s = snap(103, &[(99, Some(T0 - 20)), (100, Some(T0)), (101, Some(T0 + 10)), (103, Some(T0 + 30))]);
        let p = plan_forward(100, Some(T0), Ok(&s));
        assert!(!p.rollback);
        assert_eq!(p.forward_ids, vec![101, 103]);
        assert_eq!(p.new_watermark, 103);
        assert_eq!(p.new_last_time, Some(T0 + 30));
    }

    #[test]
    fn nothing_new_changes_nothing() {
        let s = snap(100, &[(99, Some(T0 - 20)), (100, Some(T0))]);
        let p = plan_forward(100, Some(T0), Ok(&s));
        assert_eq!(p, ForwardPlan { forward_ids: vec![], new_watermark: 100, new_last_time: Some(T0), rollback: false });
    }

    #[test]
    fn rollback_forwards_ids_1_and_2() {
        // watermark 100, device numbering reset, new SMS 1 and 2 arrive
        let s = snap(2, &[(2, Some(T0 + 60)), (1, Some(T0 + 30))]);
        let p = plan_forward(100, Some(T0), Ok(&s));
        assert!(p.rollback);
        assert_eq!(p.forward_ids, vec![1, 2]);
        assert_eq!(p.new_watermark, 2);
        assert_eq!(p.new_last_time, Some(T0 + 60));
        // next round: nothing is re-sent
        let p2 = plan_forward(p.new_watermark, p.new_last_time, Ok(&s));
        assert!(!p2.rollback);
        assert!(p2.forward_ids.is_empty());
        // and SMS 3 goes through the normal path
        let s3 = snap(3, &[(1, Some(T0 + 30)), (2, Some(T0 + 60)), (3, Some(T0 + 90))]);
        let p3 = plan_forward(p.new_watermark, p.new_last_time, Ok(&s3));
        assert_eq!(p3.forward_ids, vec![3]);
        assert_eq!(p3.new_watermark, 3);
    }

    #[test]
    fn rollback_does_not_resend_old_sms() {
        // after the reset the device still holds older messages (time <= last forwarded)
        let s = snap(4, &[(1, Some(T0 - 500)), (2, Some(T0)), (3, None), (4, Some(T0 + 5))]);
        let p = plan_forward(100, Some(T0), Ok(&s));
        assert!(p.rollback);
        assert_eq!(p.forward_ids, vec![4]);
        assert_eq!(p.new_watermark, 4);
        assert_eq!(p.new_last_time, Some(T0 + 5));

        let only_old = snap(2, &[(1, Some(T0 - 500)), (2, Some(T0 - 1))]);
        let p = plan_forward(100, Some(T0), Ok(&only_old));
        assert!(p.rollback);
        assert!(p.forward_ids.is_empty());
        assert_eq!(p.new_watermark, 2);
        assert_eq!(p.new_last_time, Some(T0));
    }

    #[test]
    fn one_store_error_no_rollback_no_advance_no_forward() {
        // NV store answers with only low ids, SIM store fails
        let res = merge_store_results(vec![
            (1, Ok(json!({"messages": [{"id": "1", "tag": "0"}, {"id": 2, "tag": 1}]}))),
            (0, Err("ubus timeout".to_string())),
        ]);
        assert!(res.is_err());
        let err = res.unwrap_err();
        assert!(err.contains("mem_store 0"), "{err}");
        let p = plan_forward(100, Some(T0), Err(&err));
        assert_eq!(p, ForwardPlan { forward_ids: vec![], new_watermark: 100, new_last_time: Some(T0), rollback: false });

        let res = merge_store_results(vec![(1, Err("boom".into())), (0, Ok(json!({"messages": []})))]);
        assert!(res.is_err());
    }

    #[test]
    fn both_stores_ok_merges_and_dedups() {
        let res = merge_store_results(vec![
            (1, Ok(json!({"messages": [{"id": "5"}, {"id": 3}]}))),
            (0, Ok(json!({"messages": [{"id": 5}, {"id": "4"}]}))),
            (0, Ok(json!({}))),
        ])
        .unwrap();
        let ids: Vec<u64> = res.iter().map(item_id).collect();
        assert_eq!(ids, vec![5, 3, 4]);
    }

    #[test]
    fn rollback_without_last_time_is_conservative() {
        // old state file: no time recorded -> forward nothing, rebase on device
        let s = snap(2, &[(1, Some(T0 + 30)), (2, Some(T0 + 60))]);
        let p = plan_forward(100, None, Ok(&s));
        assert!(p.rollback);
        assert!(p.forward_ids.is_empty());
        assert_eq!(p.new_watermark, 2);
        assert_eq!(p.new_last_time, Some(T0 + 60));
    }

    #[test]
    fn missing_last_time_backfilled_from_covered_messages() {
        let s = snap(102, &[(99, Some(T0 - 10)), (100, Some(T0)), (102, Some(T0 + 20))]);
        let p = plan_forward(100, None, Ok(&s));
        assert!(!p.rollback);
        assert_eq!(p.forward_ids, vec![102]);
        assert_eq!(p.new_last_time, Some(T0 + 20));
        let s = snap(100, &[(99, Some(T0 - 10)), (100, Some(T0))]);
        assert_eq!(plan_forward(100, None, Ok(&s)).new_last_time, Some(T0));
    }

    #[test]
    fn empty_device_is_not_a_rollback() {
        let p = plan_forward(100, Some(T0), Ok(&snap(0, &[])));
        assert!(!p.rollback);
        assert_eq!(p.new_watermark, 100);
        assert!(p.forward_ids.is_empty());
    }

    #[test]
    fn old_state_file_without_time_still_loads() {
        let st: ForwardState = serde_json::from_str(r#"{"last_forwarded_id":42,"log":[]}"#).unwrap();
        assert_eq!(st.last_forwarded_id, 42);
        assert_eq!(st.last_forwarded_time, None);
        let st: ForwardState = serde_json::from_str(r#"{"last_forwarded_id":42}"#).unwrap();
        assert_eq!(st.last_forwarded_time, None);
        let back = serde_json::to_string(&ForwardState { last_forwarded_id: 7, last_forwarded_time: Some(5), log: vec![], held: None }).unwrap();
        let st: ForwardState = serde_json::from_str(&back).unwrap();
        assert_eq!((st.last_forwarded_id, st.last_forwarded_time), (7, Some(5)));
    }

    #[test]
    fn sms_time_key_orders_and_uses_zone() {
        let a = sms_time_key("26,09,25,13,20,00,+8").unwrap();
        let b = sms_time_key("26;09;25;13;20;01;+8").unwrap();
        assert_eq!(b - a, 1);
        // same instant in another zone
        assert_eq!(sms_time_key("26,09,25,05,20,00,+0").unwrap(), a);
        // quarter-hour zone form (+32 = +8h)
        assert_eq!(sms_time_key("26,09,25,13,20,00,+32").unwrap(), a);
        // day / month / year boundaries keep ordering
        assert!(sms_time_key("26,10,01,00,00,00,+8").unwrap() > sms_time_key("26,09,30,23,59,59,+8").unwrap());
        assert!(sms_time_key("27,01,01,00,00,00,+8").unwrap() > sms_time_key("26,12,31,23,59,59,+8").unwrap());
        assert_eq!(sms_time_key("00,01,01,00,00,00"), Some(0));
        assert_eq!(sms_time_key("00,03,01,00,00,00"), Some((31 + 29) * 86_400));
        assert_eq!(sms_time_key("garbage"), None);
        assert_eq!(sms_time_key("26,13,01,00,00,00"), None);
    }
}
