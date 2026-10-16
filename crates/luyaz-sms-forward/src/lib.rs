//! SMS forwarding (moved from zte-agent `sms_forward.rs`, Phase 4 step 9e), shared by the agent
//! and u60d. Every side effect (datad's SMS feed and pages, ubus reads, the vendor send, datad
//! actions, alerts, shadow records, sleeping) goes through [`Host`]; the HTTP destinations are
//! sent from here. The agent's host is the device; tests use a fake.
//!
//! One owner at a time: the event loop waits for another live owner's lease
//! ([`wait_lease_free`]), re-reads the state file when it starts, and stops (holding the current
//! SMS, as a transient failure) once [`Host::active`] says the host no longer owns forwarding.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

mod api;
mod lease;

pub use lease::{remove_lease, wait_lease_free, write_lease, LEASE_FILE, LEASE_WAIT};

pub const CONFIG_PATH: &str = "/data/local/tmp/sms_forward.json";
pub const STATE_PATH: &str = "/data/local/tmp/sms_forward_state.json";

/// What the engine does to the device. Errors are the host's own text (logged as they come).
pub trait Host: Send + Sync + 'static {
    /// datad's `sms` block summary (max id, count), if subscribed and fresh; `None` = read ubus
    /// directly.
    fn sms_summary(&self) -> Option<(u64, u64)>;
    /// One datad `sms.list_after` page (busy retried by the host).
    fn list_after(&self, after: u64, limit: u64) -> Result<Page, String>;
    /// A ubus read; `long_timeout` = the long read timeout (SMS lists), else the default.
    fn ubus_read(&self, obj: &str, method: &str, params: Option<&str>, long_timeout: bool) -> Result<Value, String>;
    /// A vendor setter through datad (sending an SMS), as the calling thread's source.
    fn vendor(&self, obj: &str, method: &str, params: Option<&str>) -> Result<Value, String>;
    /// A datad action (`sms.delete`, `sms.mark_read`) as source auto.
    fn control(&self, action: &str, params: &Value) -> Result<Value, String>;
    fn alert(&self, key: &str, text: &str);
    /// A shadow record for u60d (the host keeps the ring).
    fn tick(&self, record: Value);
    /// The device's SMS zone in hours (the clock is local time labelled UTC).
    fn sms_zone_hours(&self) -> i64;
    /// The host still owns forwarding; false = handed over (the loop stops).
    fn active(&self) -> bool;
    fn sleep(&self, d: Duration);
    /// Runs the event loop on its thread (the agent attributes that thread's writes to source
    /// auto here).
    fn run_worker(&self, f: Box<dyn FnOnce() + '_>) {
        f()
    }
}
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
    !s.is_empty() && s.len().is_multiple_of(4) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Parse ZTE timestamp `YY,MM,DD,HH,MM,SS,+TZ` (or `;` separators) into human-readable string.
fn humanize_zte_date(raw: &str) -> String {
    let parts: Vec<&str> = raw.split([',', ';']).collect();
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
    let parts: Vec<&str> = raw.split([',', ';']).collect();
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
fn format_sms_time(tz_offset: i64) -> String {
    let mut t: i64 = 0;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        libc::time(&mut t);
        libc::localtime_r(&t, &mut tm);
    }
    // tm_gmtoff is 0 on this firmware (TZ=UTC over a local-time clock); the
    // real zone comes from ZTE's SNTP settings (the agent's clock.rs).
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
        } else if let Some(suffix) = p_lower.strip_prefix('*') {
            sender_lower.ends_with(suffix)
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
    host: &dyn Host,
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
                "sms_time": format_sms_time(host.sms_zone_hours()),
                "id": "-1",
            });
            let resp = host.vendor("zwrt_wms", "zte_libwms_send_sms",
                Some(&params.to_string()),
            )
            .map_err(|e| format!("sms forward: {e}"))?;
            check_sms_send_result(&resp)?;

            // Auto-delete the forwarded outgoing SMS to prevent conversation clutter
            cleanup_forwarded_sms(host, forward_number);

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
fn cleanup_forwarded_sms(host: &dyn Host, forward_number: &str) {
    // Small delay to let firmware store the outgoing SMS
    host.sleep(Duration::from_millis(200));

    // Fetch latest sent messages (tag=2)
    let messages = match fetch_sms_both_stores(host, 2, 0, 5, "order by id desc") {
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
                match host.control("sms.delete", &json!({"ids": id.to_string()})) {
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
    host: Arc<dyn Host>,
    config_path: PathBuf,
    state_path: PathBuf,
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
    /// Wakes the forwarder (connectivity came back, new SMS).
    wake: Mutex<Option<mpsc::Sender<Wake>>>,
}

/// A `network_type` with cellular service.
fn in_service(network_type: &str) -> bool {
    !network_type.is_empty() && network_type != "NO_SERVICE" && !network_type.starts_with("LIMITED_SERVICE")
}

fn read_state(path: &Path) -> ForwardState {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<ForwardState>(&s).ok())
        .unwrap_or_default()
}

fn delivered_of(state: &ForwardState) -> HashSet<(u64, u32)> {
    state.held.iter().flat_map(|h| h.delivered_rules.iter().map(move |r| (h.sms_id, *r))).collect()
}

impl SmsForwarder {
    /// The config and state files: [`CONFIG_PATH`] and [`STATE_PATH`] on the device (u60d may
    /// put them under its compat root).
    pub fn new(host: Arc<dyn Host>, config_path: impl Into<PathBuf>, state_path: impl Into<PathBuf>) -> Self {
        let (config_path, state_path) = (config_path.into(), state_path.into());
        let config = fs::read_to_string(&config_path)
            .ok()
            .and_then(|s| serde_json::from_str::<SmsForwardConfig>(&s).ok())
            .unwrap_or_default();

        let state = read_state(&state_path);

        let delivered = delivered_of(&state);
        SmsForwarder {
            host,
            config_path,
            state_path,
            config: Mutex::new(config),
            state: Mutex::new(state),
            has_service: AtomicBool::new(false),
            wan_connected: AtomicBool::new(false),
            delivered: Mutex::new(delivered),
            online_clock: Mutex::new(OnlineClock::default()),
            wake: Mutex::new(None),
        }
    }

    /// Seed the connectivity state (retried while the services are not registered yet), then
    /// run the event loop on a thread of its own. Wake it with [`wake`](Self::wake) (new SMS)
    /// and feed it [`set_service`](Self::set_service) / [`set_wan`](Self::set_wan); without
    /// wakes only its timer checks. `lease`: wait for another live owner's lease before the
    /// first pass ([`wait_lease_free`]).
    pub fn start(self: &Arc<Self>, lease: Option<PathBuf>) {
        // Seed initial connectivity state (retry if services aren't registered yet)
        for attempt in 1..=5 {
            let got_service = if !self.has_service.load(Ordering::Relaxed) {
                if let Ok(data) = self.host.ubus_read("zte_nwinfo_api", "nwinfo_get_netinfo", Some("{}"), false) {
                    let network_type = data["network_type"].as_str().unwrap_or("");
                    let has = in_service(network_type);
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
                if let Ok(data) = self.host.ubus_read("zwrt_data", "get_wwaniface", Some(r#"{"source_module":"zte_topsw_data","cid":1}"#), false) {
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
                self.host.sleep(Duration::from_secs(2));
            }
        }

        // The sender kept here keeps the channel open: with no other waker
        // only the timer wakes the loop.
        let (tx, rx) = mpsc::channel();
        *self.wake.lock().unwrap() = Some(tx);

        let forwarder = Arc::clone(self);
        std::thread::spawn(move || {
            let host = Arc::clone(&forwarder.host);
            host.run_worker(Box::new(|| forwarder.event_loop(rx, lease.as_deref())));
        });
    }

    /// A `zwrt_servicestatus` event's `network_type`.
    pub fn set_service(&self, network_type: &str) {
        let has = in_service(network_type);
        let prev = self.has_service.swap(has, Ordering::Relaxed);
        self.tick_online(); // closes the interval under the old state
        if prev != has {
            eprintln!("[sms_forward] service state changed: {network_type} (has_service={has})");
            if has {
                self.wake();
            }
        }
    }

    /// A `router_event_wan_connect_status` event's `wan_status`.
    pub fn set_wan(&self, wan_status: &str) {
        let connected = wan_status.starts_with("ipv4");
        let prev = self.wan_connected.swap(connected, Ordering::Relaxed);
        self.tick_online(); // closes the interval under the old state
        if prev != connected {
            eprintln!("[sms_forward] WAN state changed: {wan_status} (connected={connected})");
            if connected {
                self.wake();
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

    /// Check now (new SMS; connectivity came back: one wake per transition, so no storm).
    /// False once the loop has stopped (or before [`start`](Self::start)).
    pub fn wake(&self) -> bool {
        match self.wake.lock().unwrap().as_ref() {
            Some(tx) => tx.send(Wake).is_ok(),
            None => false,
        }
    }

    /// The state file as the loop starts: another owner may have moved the watermark or the
    /// held SMS since [`new`](Self::new). Online time counted in memory for the same held SMS
    /// and not saved yet is kept.
    fn reload_state(&self) {
        let mut fresh = read_state(&self.state_path);
        let mut state = self.state.lock().unwrap();
        if let (Some(f), Some(m)) = (fresh.held.as_mut(), state.held.as_ref()) {
            if f.sms_id == m.sms_id {
                f.online_secs = f.online_secs.max(m.online_secs);
            }
        }
        *self.delivered.lock().unwrap() = delivered_of(&fresh);
        *state = fresh;
    }

    fn init_watermark(&self) {
        let last_id = self.state.lock().unwrap().last_forwarded_id;
        if last_id == 0 {
            match fetch_baseline(&*self.host) {
                Ok((max_id, newest)) if max_id > 0 => {
                    let mut state = self.state.lock().unwrap();
                    state.last_forwarded_id = max_id;
                    state.last_forwarded_time = max_time(state.last_forwarded_time, newest);
                    self.save_state(&state);
                    eprintln!("[sms_forward] watermark initialized to {max_id}");
                }
                Ok(_) => eprintln!("[sms_forward] no SMS on device, watermark stays at 0"),
                Err(e) => eprintln!("[sms_forward] watermark init failed: {e}"),
            }
        }
    }

    fn event_loop(&self, rx: mpsc::Receiver<Wake>, lease: Option<&Path>) {
        if let Some(path) = lease {
            wait_lease_free(path, &*self.host, LEASE_WAIT);
        }
        self.reload_state();
        if !self.host.active() {
            eprintln!("[sms_forward] not the owner: forwarder stopped");
            return;
        }
        self.init_watermark();

        let t0 = Instant::now();
        // Check once at start: a restart in the middle of a burst resumes
        // from the watermark at once, not after a whole poll period.
        let summary = self.host.sms_summary();
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
            if !self.host.active() {
                eprintln!("[sms_forward] handed over: forwarder stopped");
                return;
            }
            let summary = self.host.sms_summary();
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
            self.save_state(&state);
        };
        let load = || {
            let st = self.state.lock().unwrap();
            (st.last_forwarded_id, st.last_forwarded_time)
        };

        if let Some((max_id, _)) = summary {
            let (wm, lt) = load();
            let mut fetch = |after: u64, limit: u64| self.host.list_after(after, limit);
            match recorded_pass(&*self.host, "datad", wm, lt, max_id, &mut fetch, &mut forward, &mut commit) {
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
        let max_id = match fetch_baseline(&*self.host) {
            Ok((m, _)) => m,
            Err(e) => {
                eprintln!("[sms_forward] failed to read SMS max id: {e}");
                return;
            }
        };
        let (wm, lt) = load();
        let mut fetch = |after: u64, limit: u64| list_after_direct(after, limit, &mut |store, page| ubus_sms_page(&*self.host, store, page));
        match recorded_pass(&*self.host, "direct", wm, lt, max_id, &mut fetch, &mut forward, &mut commit) {
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
        let held_before = self.state.lock().unwrap().held.clone();
        let mut steps: Vec<Value> = Vec::new();

        for (i, rule) in enabled_rules.iter().enumerate() {
            // Skip firmware echo: sender matches destination AND content is garbled
            let dest = destination_type_name(&rule.destination);
            if is_forward_loop(&rule.destination, &sms.sender) && is_garbled_echo(&sms.content) {
                steps.push(json!({"rule": rule.id, "dest": dest, "step": "echo"}));
                continue;
            }

            if !matches_filter(&rule.filter, &sms.sender, &sms.content) {
                steps.push(json!({"rule": rule.id, "dest": dest, "step": "no_match"}));
                continue;
            }
            if self.delivered.lock().unwrap().contains(&(sms.id, rule.id)) {
                steps.push(json!({"rule": rule.id, "dest": dest, "step": "already"}));
                continue;
            }

            // Handed over: hold this SMS here (a transient failure), try no further rule.
            if !self.host.active() {
                steps.push(json!({"rule": rule.id, "dest": dest, "step": "transient"}));
                errors.push(HANDED_OVER.to_string());
                break;
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
                None => forward_with_retry(&*self.host, &rule.destination, sms, agent),
            };
            let handed_over = matches!(&result, Err(e) if e == HANDED_OVER);

            let step = match &result {
                Ok(()) => "ok",
                Err(e) if is_transient_error(e) => "transient",
                Err(_) => "permanent",
            };
            steps.push(json!({"rule": rule.id, "dest": dest, "step": step}));
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
            if handed_over {
                break;
            }

            // Delay between rules for the same SMS
            if i + 1 < enabled_rules.len() {
                self.host.sleep(Duration::from_millis(INTER_RULE_DELAY_MS));
            }
        }

        let delivered_now: Vec<u32> =
            self.delivered.lock().unwrap().iter().filter(|(id, _)| *id == sms.id).map(|(_, r)| *r).collect();
        let wall = now_ts();
        let (outcome, alert) = {
            let mut state = self.state.lock().unwrap();
            let (outcome, alert) = settle_hold(&mut state.held, sms.id, delivery_outcome(&errors), &delivered_now, wall);
            self.host.tick(json!({
                "kind": "deliver",
                "sms_id": sms.id,
                "wall": wall,
                "held_before": held_before,
                "rules": steps,
                "delivered_now": delivered_now,
                "outcome": delivery_kind(&outcome),
                "held_after": state.held,
                "alert": alert.is_some(),
                "mark_read": config.mark_read_after_forward,
                "delete": config.delete_after_forward,
            }));
            if alert.is_some() {
                for e in state.log.iter_mut().filter(|e| e.sms_id == sms.id && !e.success) {
                    if let Some(err) = e.error.as_mut() {
                        if !err.contains(HOLD_TIMEOUT_NOTE) {
                            *err = format!("{err}（{HOLD_TIMEOUT_NOTE}）");
                        }
                    }
                }
            }
            self.save_state(&state);
            (outcome, alert)
        };
        if let Some(text) = alert {
            eprintln!("[sms_forward] SMS {} {HOLD_TIMEOUT_NOTE} (held over {HOLD_TIMEOUT_SECS}s online)", sms.id);
            self.host.alert("sms-forward-stuck", &text);
        }
        match &outcome {
            Delivery::Deferred(e) => {
                eprintln!("[sms_forward] SMS {} deferred ({e}); will resume from it on the next wake-up", sms.id);
                self.host.sleep(Duration::from_millis(INTER_SMS_DELAY_MS));
                return outcome;
            }
            Delivery::Skipped(e) => eprintln!("[sms_forward] SMS {} skipped after a permanent error: {e}", sms.id),
            Delivery::Delivered => {}
        }
        self.delivered.lock().unwrap().clear();
        let all_succeeded = errors.is_empty();

        // Post-forward actions — only when ALL rules succeeded
        if all_succeeded && config.mark_read_after_forward {
            if let Err(e) = self.host.control("sms.mark_read", &json!({"ids": sms.id.to_string(), "tag": 0})) {
                eprintln!("[sms_forward] mark-read failed for SMS {}: {e}", sms.id);
            }
        }
        if all_succeeded && config.delete_after_forward {
            if let Err(e) = self.host.control("sms.delete", &json!({"ids": sms.id.to_string()})) {
                eprintln!("[sms_forward] delete failed for SMS {}: {e}", sms.id);
            }
        }

        // Delay between different SMS messages
        self.host.sleep(Duration::from_millis(INTER_SMS_DELAY_MS));
        outcome
    }
}

// ── shadow records for u60d (u60-platform.md Phase 4 step 6b) ───────

// u60d replays each pass and each delivery through u60-policy's copy. Ids, time keys and outcome
// classes only: sender and body are secret and never written here, so the rule matching (which
// reads them) is not replayed. Only while forwarding is on (`check` returns before otherwise).
// Published through `Host::tick` (the agent's ring is `/tmp/sms-forward.tick.json`).

fn delivery_kind(d: &Delivery) -> &'static str {
    match d {
        Delivery::Delivered => "delivered",
        Delivery::Skipped(_) => "skipped",
        Delivery::Deferred(_) => "deferred",
    }
}

/// An SMS row as u60-policy sees it: id, received or not, the time key of its date.
fn item_json(item: &Value) -> Value {
    let (id, sms) = decode_item(item);
    json!({"id": id, "received": sms.is_some(), "time": sms.and_then(|m| sms_time_key(&m.date))})
}

/// [`run_pass`] with every page, forward and commit written down; published when the pass read or
/// moved anything.
#[allow(clippy::too_many_arguments)]
fn recorded_pass(
    host: &dyn Host,
    via: &str,
    watermark: u64,
    last_time: Option<i64>,
    max_id: u64,
    fetch: &mut dyn FnMut(u64, u64) -> Result<Page, String>,
    forward: &mut dyn FnMut(&DecodedSms) -> Delivery,
    commit: &mut dyn FnMut(u64, Option<i64>),
) -> Result<usize, String> {
    let (res, rec) = record_pass(via, watermark, last_time, max_id, fetch, forward, commit);
    if let Some(rec) = rec {
        host.tick(rec);
    }
    res
}

fn record_pass(
    via: &str,
    watermark: u64,
    last_time: Option<i64>,
    max_id: u64,
    fetch: &mut dyn FnMut(u64, u64) -> Result<Page, String>,
    forward: &mut dyn FnMut(&DecodedSms) -> Delivery,
    commit: &mut dyn FnMut(u64, Option<i64>),
) -> (Result<usize, String>, Option<Value>) {
    let (mut pages, mut forwards, mut commits) = (Vec::new(), Vec::new(), Vec::new());
    let res = run_pass(
        watermark,
        last_time,
        max_id,
        &mut |after, limit| {
            let r = fetch(after, limit);
            pages.push(match &r {
                Ok(p) => json!({"after": after, "limit": limit, "page": {"items": p.items.iter().map(item_json).collect::<Vec<_>>(), "has_more": p.has_more}}),
                Err(e) => json!({"after": after, "limit": limit, "error": e}),
            });
            r
        },
        &mut |sms| {
            let d = forward(sms);
            forwards.push(json!({"id": sms.id, "outcome": delivery_kind(&d)}));
            d
        },
        &mut |wm, lt| {
            commits.push(json!([wm, lt]));
            commit(wm, lt);
        },
    );
    let rec = (!pages.is_empty() || !commits.is_empty()).then(|| {
        json!({
            "kind": "pass", "via": via, "watermark": watermark, "last_time": last_time, "max_id": max_id,
            "pages": pages, "forwards": forwards, "commits": commits,
            "result": match &res { Ok(n) => json!({"ok": n}), Err(e) => json!({"err": e}) },
        })
    });
    (res, rec)
}

/// Pre-forward connectivity failures: transient, the SMS waits for the link.
const NO_SERVICE_ERR: &str = "no cellular service (will retry)";
const NO_WAN_ERR: &str = "no WAN connectivity (will retry)";
/// The host stopped owning forwarding mid-delivery: transient, the next owner resumes.
const HANDED_OVER: &str = "handed over";

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

// ── paging (T10 / R16) ──────────────────────────────────────────────

/// Page size for `sms.list_after` and the direct reads (datad's limit cap).
const LIST_PAGE: u64 = 50;
/// Pages per store before a direct read gives up (as datad: 20 × 50).
const LIST_MAX_PAGES: u64 = 20;

/// One `sms.list_after` page: items with id > after_id, ascending.
#[derive(Debug, Clone, Default)]
pub struct Page {
    pub items: Vec<Value>,
    pub has_more: bool,
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

/// How far before the last handled SMS a received one may be dated and still
/// count as new. SMS times come from the SMS centre, so mail held while the
/// device was off still dates after the last one we forwarded; a little
/// reordering is all this has to absorb.
const REIMPORT_MARGIN_SECS: i64 = 7 * 86_400;

/// A received SMS with a fresh id but an old date: the firmware renumbers the
/// SIM card's stored SMS when it reads a card again (card swap, eSIM switch),
/// so years-old messages show up above the watermark. Skipped (the watermark
/// still moves past them), not forwarded.
fn is_reimported(sms: &DecodedSms, last_time: Option<i64>) -> bool {
    let skip = matches!((sms_time_key(&sms.date), last_time),
        (Some(t), Some(lt)) if t < lt - REIMPORT_MARGIN_SECS);
    if skip {
        eprintln!("[sms_forward] skipping SMS {} dated {}: older than the last forwarded one (re-read from SIM)", sms.id, sms.date);
    }
    skip
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
            if let Some(sms) = sms.filter(|m| !is_reimported(m, lt)) {
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
fn ubus_sms_page(host: &dyn Host, store: u64, page: u64) -> Result<Value, String> {
    let params = json!({
        "tags": 10,
        "page": page,
        "data_per_page": LIST_PAGE,
        "mem_store": store,
        "order_by": "order by id desc",
    });
    host.ubus_read("zwrt_wms", "zte_libwms_get_sms_data", Some(&params.to_string()), true)
}

fn preview(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..s.floor_char_boundary(max)])
    }
}

fn forward_with_retry(
    host: &dyn Host,
    dest: &ForwardDestination,
    sms: &DecodedSms,
    agent: &ureq::Agent,
) -> Result<(), String> {
    let mut last_err = String::new();

    for attempt in 0..MAX_RETRIES {
        match forward_to(host, dest, sms, agent) {
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
                    host.sleep(Duration::from_secs(RETRY_DELAYS[attempt as usize]));
                    if !host.active() {
                        return Err(HANDED_OVER.to_string());
                    }
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
fn fetch_sms_both_stores(host: &dyn Host, tags: u64, page: u64, count: u64, order: &str) -> Result<Vec<Value>, String> {
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
            (store, host.ubus_read("zwrt_wms", "zte_libwms_get_sms_data", Some(&params.to_string()), true))
        })
        .collect();
    merge_store_results(results)
}

/// An SMS row's id (number or string; 0 if neither).
pub fn item_id(item: &Value) -> u64 {
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
/// Baseline from the newest rows of both stores: the highest id (any tag)
/// and the newest received SMS's own time.
fn fetch_baseline(host: &dyn Host) -> Result<(u64, Option<i64>), String> {
    let all = fetch_sms_both_stores(host, 10, 0, 5, "order by id desc")?;
    Ok(baseline_of(&all))
}

fn baseline_of(items: &[Value]) -> (u64, Option<i64>) {
    items.iter().fold((0, None), |(max_id, lt), item| {
        let (id, sms) = decode_item(item);
        let t = sms.and_then(|m| sms_time_key(&m.date));
        (max_id.max(id), max_time(lt, t))
    })
}

// ── Persistence ─────────────────────────────────────────────────────

impl SmsForwarder {
    fn save_config(&self, config: &SmsForwardConfig) {
        if let Ok(json) = serde_json::to_string_pretty(config) {
            let _ = atomic_write(&self.config_path, json.as_bytes());
        }
    }

    fn save_state(&self, state: &ForwardState) {
        if let Ok(json) = serde_json::to_string(state) {
            let _ = atomic_write(&self.state_path, json.as_bytes());
        }
    }
}

/// Forwarding actually runs: master switch on and at least one rule on.
fn is_active(config: &SmsForwardConfig) -> bool {
    config.enabled && config.rules.iter().any(|r| r.enabled)
}

/// Save a changed config. When forwarding goes from not running to running,
/// drop the old baseline: while it was off the watermark stood still, and
/// everything that arrived meanwhile would otherwise go out at once. The
/// next check re-baselines from what is on the device ([`SmsForwarder::init_watermark`]),
/// so only SMS that arrive after switching on are forwarded.
fn commit_config(fwd: &SmsForwarder, was_active: bool, config: &SmsForwardConfig) {
    fwd.save_config(config);
    if !was_active && is_active(config) {
        let mut state = fwd.state.lock().unwrap();
        state.last_forwarded_id = 0;
        state.last_forwarded_time = None;
        state.held = None;
        fwd.save_state(&state);
        eprintln!("[sms_forward] forwarding switched on: baseline reset");
    }
}

/// Write `path.tmp`, then rename it over `path` (a crash mid-write leaves the old file whole);
/// the old file's mode is carried over, a new file gets the default. As the agent's
/// `fsutil::atomic_write`.
fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let keep = fs::metadata(path).ok().map(|m| m.permissions().mode() & 0o7777);
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).open(&tmp)?;
        // After open, not via OpenOptions::mode: that one is masked by the
        // umask and ignored for a .tmp left over from a crash.
        if let Some(m) = keep {
            f.set_permissions(fs::Permissions::from_mode(m))?;
        }
        f.write_all(data)?;
    }
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests;

