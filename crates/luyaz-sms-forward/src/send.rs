//! Rule matching and the actual sending: one SMS to one destination, one try (the queue backs
//! off and tries again). Errors are plain text; a 4xx is tagged `(permanent error)` and that tag
//! is what [`is_transient_error`] reads.

use std::time::Duration;

use serde_json::{json, Value};

use crate::text::{
    decode_ucs2_hex, encode_ucs2_hex, format_sms_time, humanize_zte_date, is_ucs2_hex, now_ts,
};
use crate::{
    fetch_sms_both_stores, item_id, DecodedSms, ForwardDestination, Host, SmsFilter,
};

/// Whole-request timeout for every HTTP destination.
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
/// Discord rejects longer messages.
const DISCORD_MAX: usize = 2000;
pub(crate) const PERMANENT: &str = "(permanent error)";

// ── matching ──

/// Number as compared for loop detection: trimmed, `+63…` written as `0…`.
fn normalize_phone(num: &str) -> String {
    let n = num.trim();
    n.strip_prefix("+63")
        .map_or_else(|| n.to_owned(), |local| format!("0{local}"))
}

/// The SMS came from the very number this rule sends to.
pub(crate) fn is_forward_loop(to: &ForwardDestination, sender: &str) -> bool {
    if let ForwardDestination::Sms { forward_number } = to {
        return normalize_phone(forward_number) == normalize_phone(sender);
    }
    false
}

/// The firmware's echo of a forwarded SMS: empty, or more than half `@`/NUL.
pub(crate) fn is_garbled_echo(content: &str) -> bool {
    let noise = content.chars().filter(|c| matches!(c, '@' | '\0')).count();
    content.is_empty() || 2 * noise > content.len()
}

/// Sender pattern list hit: empty list = any; `abc*` prefix, `*abc` suffix, else exact; no case.
fn sender_hit(sender: &str, patterns: &[String]) -> bool {
    let who = sender.to_lowercase();
    patterns.is_empty()
        || patterns.iter().map(|p| p.to_lowercase()).any(|p| {
            if let Some(head) = p.strip_suffix('*') {
                who.starts_with(head)
            } else if let Some(tail) = p.strip_prefix('*') {
                who.ends_with(tail)
            } else {
                who == p
            }
        })
}

/// Keyword list hit: empty list = any; case-insensitive substring.
fn content_hit(content: &str, keywords: &[String]) -> bool {
    let text = content.to_lowercase();
    keywords.is_empty() || keywords.iter().any(|k| text.contains(&k.to_lowercase()))
}

pub(crate) fn matches_filter(filter: &SmsFilter, sender: &str, content: &str) -> bool {
    let (by_sender, by_text) = match filter {
        SmsFilter::All => (None, None),
        SmsFilter::Sender { patterns } => (Some(patterns), None),
        SmsFilter::Content { keywords } => (None, Some(keywords)),
        SmsFilter::SenderAndContent { patterns, keywords } => (Some(patterns), Some(keywords)),
    };
    by_sender.is_none_or(|p| sender_hit(sender, p))
        && by_text.is_none_or(|k| content_hit(content, k))
}

// ── sending ──

/// The `type` tag of a destination, as logged.
pub(crate) fn destination_type_name(to: &ForwardDestination) -> &'static str {
    use ForwardDestination as D;
    match to {
        D::Sms { .. } => "sms",
        D::Webhook { .. } => "webhook",
        D::Telegram { .. } => "telegram",
        D::Discord { .. } => "discord",
        D::Slack { .. } => "slack",
        D::Ntfy { .. } => "ntfy",
    }
}

/// The text chat-style destinations get: sender, readable date, blank line, body.
pub(crate) fn chat_text(msg: &DecodedSms) -> String {
    let when = humanize_zte_date(&msg.date);
    let late = msg.late_secs.map_or(String::new(), |s| format!(" (late: forwarded {} after it came in)", wait_text(s)));
    format!("SMS from {}\n{when}{late}\n\n{}", msg.sender, msg.content)
}

/// `45 min`, `3 h 05 min`, `2 d 4 h`.
pub(crate) fn wait_text(secs: i64) -> String {
    let m = secs.max(0) / 60;
    match m {
        0..60 => format!("{} min", m.max(1)),
        60..1440 => format!("{} h {:02} min", m / 60, m % 60),
        _ => format!("{} d {} h", m / 1440, m % 1440 / 60),
    }
}

pub(crate) fn http_agent() -> ureq::Agent {
    let cfg = ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .build();
    ureq::Agent::from(cfg)
}

/// 2xx = fine; 429 and other 4xx get their own wording (4xx are permanent, retrying is useless).
fn http_verdict(label: &str, code: u16) -> Result<(), String> {
    match code {
        200..=299 => Ok(()),
        429 => Err(format!("{label}: HTTP 429 rate limited")),
        400..=499 => Err(format!("{label}: HTTP {code} {PERMANENT}")),
        _ => Err(format!("{label}: HTTP {code}")),
    }
}

/// POST a JSON body, judge the status.
fn post_json(http: &ureq::Agent, label: &str, url: &str, body: &Value) -> Result<(), String> {
    let raw = body.to_string();
    let req = http.post(url).header("Content-Type", "application/json");
    let reply = req
        .send(raw.as_bytes())
        .map_err(|e| format!("{label}: {e}"))?;
    http_verdict(label, reply.status().into())
}

/// The send call's `result`: 3 (or none at all) = accepted by the modem.
fn sms_send_verdict(reply: &Value) -> Result<(), String> {
    let field = reply.get("result");
    let code = field
        .and_then(Value::as_u64)
        .or_else(|| field.and_then(Value::as_str).and_then(|s| s.parse().ok()));
    match code {
        None | Some(3) => Ok(()),
        Some(other) => Err(format!("sms forward: device rejected (result={other})")),
    }
}

/// Sends `msg` once to `to`.
pub(crate) fn forward_to(
    host: &dyn Host,
    to: &ForwardDestination,
    msg: &DecodedSms,
    http: &ureq::Agent,
) -> Result<(), String> {
    use ForwardDestination as D;
    match to {
        D::Slack { webhook_url } => post_json(
            http,
            "slack",
            webhook_url,
            &json!({ "text": chat_text(msg) }),
        ),
        D::Discord { webhook_url } => {
            let mut text = chat_text(msg);
            if text.len() > DISCORD_MAX {
                let cut = text.floor_char_boundary(DISCORD_MAX - 3);
                text.truncate(cut);
                text.push_str("...");
            }
            post_json(http, "discord", webhook_url, &json!({ "content": text }))
        }
        D::Telegram {
            bot_token,
            chat_id,
            silent,
        } => {
            let body = json!({
                "chat_id": chat_id,
                "disable_notification": silent,
                "text": chat_text(msg),
            });
            post_json(
                http,
                "telegram",
                &format!("https://api.telegram.org/bot{bot_token}/sendMessage"),
                &body,
            )
        }
        D::Ntfy { url, topic, token } => {
            let target = format!("{}/{topic}", url.trim_end_matches('/'));
            let mut req = http
                .post(&target)
                .header("Title", &format!("SMS from {}", msg.sender));
            if let Some(tok) = token {
                req = req.header("Authorization", &format!("Bearer {tok}"));
            }
            let reply = req
                .send(msg.content.as_bytes())
                .map_err(|e| format!("ntfy: {e}"))?;
            http_verdict("ntfy", reply.status().into())
        }
        D::Webhook {
            url,
            method,
            headers,
        } => {
            let payload = json!({
                "event": "sms_received",
                "timestamp": now_ts(),
                "sms": { "id": msg.id, "sender": msg.sender, "content": msg.content, "date": msg.date },
                "late_secs": msg.late_secs,
            })
            .to_string();
            let base = if method.eq_ignore_ascii_case("PUT") {
                http.put(url)
            } else {
                http.post(url)
            };
            let req = headers
                .iter()
                .fold(base.header("Content-Type", "application/json"), |r, h| {
                    r.header(&h.name, &h.value)
                });
            let reply = req
                .send(payload.as_bytes())
                .map_err(|e| format!("webhook: {e}"))?;
            http_verdict("webhook", reply.status().into())
        }
        D::Sms { forward_number } => {
            let params = json!({
                "id": "-1",
                "number": forward_number,
                "encode_type": "UNICODE",
                "message_body": encode_ucs2_hex(&chat_text(msg)),
                "sms_time": format_sms_time(host.sms_zone_hours()),
            });
            let reply = host
                .vendor("zwrt_wms", "zte_libwms_send_sms", Some(&params.to_string()))
                .map_err(|e| format!("sms forward: {e}"))?;
            sms_send_verdict(&reply)?;
            // the sent copy would sit in the device's conversation list: remove it
            delete_sent_copy(host, forward_number);
            Ok(())
        }
    }
}

/// Best effort: find the newest sent SMS (tag 2) to `number` and delete it; the forward log
/// is the record of it.
fn delete_sent_copy(host: &dyn Host, number: &str) {
    // give the firmware a moment to file the sent copy
    host.sleep(Duration::from_millis(200));
    let rows = match fetch_sms_both_stores(host, 2, 0, 5, "order by id desc") {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("[sms_forward] cleanup: failed to list sent SMS: {e}");
            return;
        }
    };
    let want = normalize_phone(number);
    let to_of = |row: &Value| {
        let raw = row["number"].as_str().unwrap_or_default();
        if is_ucs2_hex(raw) {
            decode_ucs2_hex(raw)
        } else {
            raw.to_owned()
        }
    };
    // only the newest match; id 0 means the row had none
    let Some(row) = rows.iter().find(|row| normalize_phone(&to_of(row)) == want) else {
        return;
    };
    let id = item_id(row);
    if id == 0 {
        return;
    }
    match host.control("msg.delete", &json!({ "ids": id.to_string() })) {
        Err(e) => eprintln!("[sms_forward] cleanup: could not delete sent copy id={id}: {e}"),
        Ok(_) => eprintln!("[sms_forward] cleanup: deleted sent copy id={id}"),
    }
}

/// Everything but a 4xx is worth trying again later.
pub(crate) fn is_transient_error(err: &str) -> bool {
    !err.contains(PERMANENT)
}
