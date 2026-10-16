//! The AT port's routes (the agent's `at_terminal.rs` and `telephony.rs`): the AT terminal, USSD
//! and the SIM toolkit, through u60d's `at.rs` (one owner of the port: its lock, its port pick).
//! The terminal's line is typed by a person ([`crate::at::raw`]); USSD and STK are built from
//! checked parts ([`crate::at::Phone`]). The decoders and parsers are the agent's, as they were.
//!
//! Differences from the agent: the terminal stops at OK or ERROR instead of always waiting the
//! whole `timeout` (an unsolicited line after the answer is not shown); a line with a control
//! character in it is refused (the agent's `echo -e` would have sent two commands); a `+CFUN=` set
//! is refused ([`radio_set`]).

use std::time::{Duration, Instant};

use axum::{
    body::Bytes,
    extract::{Request, State},
    response::Response,
};
use serde_json::{Value, json};

use super::super::{Compat, charge, reply};
use crate::at::{self, Phone};

fn target(c: &Compat) -> at::Target {
    c.admin.at.clone().unwrap_or_else(at::Target::from_env)
}

// ---- AT terminal ----

/// A `+CFUN=` set (anything but the `=?` test form): refused (owner, 10-13). The vendor's ONLINE
/// does not bring the modem back from LPM and `AT+CFUN=1` blocks on the port (CLAUDE.md, B31):
/// only a restart does, and this device is someone's only uplink abroad. Unlike the agent.
pub(crate) fn radio_set(cmd: &str) -> bool {
    let s: String = cmd
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_uppercase();
    s.match_indices("CFUN=").any(|(i, _)| !s[i + 5..].starts_with('?'))
}

/// POST /api/at/send `{"command": "AT+CSQ", "timeout": 3}`
pub async fn at_send(State(c): State<Compat>, req: Request) -> Response {
    let source = charge::source(&req);
    let Ok(body) = axum::body::to_bytes(req.into_body(), 64 * 1024).await else {
        return reply(400, json!({"ok": false, "error": "invalid JSON"}));
    };
    let Ok(parsed) = serde_json::from_slice::<Value>(&body) else {
        return reply(400, json!({"ok": false, "error": "invalid JSON"}));
    };
    let Some(command) = parsed["command"].as_str() else {
        return reply(
            400,
            json!({"ok": false, "error": "missing 'command' field"}),
        );
    };
    if !command.to_ascii_uppercase().starts_with("AT") {
        return reply(
            400,
            json!({"ok": false, "error": "command must start with 'AT'"}),
        );
    }
    let timeout = parsed["timeout"].as_u64().unwrap_or(3).clamp(1, 30);
    // the agent's filter (it went through a shell); kept, so the same line sends the same command
    let sanitized: String = command
        .chars()
        .filter(|c| !matches!(c, '\'' | '`' | '$' | ';' | '|' | '&'))
        .collect();
    if sanitized.is_empty() {
        return reply(
            400,
            json!({"ok": false, "error": "command is empty after sanitization"}),
        );
    }
    if radio_set(command) || radio_set(&sanitized) {
        return reply(
            403,
            json!({"ok": false,
                "error": "AT+CFUN 设置不能从终端发：调制解调器停在低功耗状态只有重启设备才能恢复",
                "error_en": "AT+CFUN sets are refused: low-power state only comes back with a device restart"}),
        );
    }
    // Any AT command may change the network behind u60d's back: a network-mode change still
    // confirming is cancelled, not rolled back over this one (D40). Best effort. A person's
    // command: the agent's forward says `web`, luyaz-web's direct call nothing (`screen`); the
    // scheduler and the scenario never reach this route.
    if !matches!(source, "scheduler" | "scenario") {
        let r = c
            .src
            .control
            .send_with(
                source,
                None,
                "op.interrupt",
                &json!({"what": "at"}),
                Duration::from_secs(3),
            )
            .await;
        if !r.ok() {
            eprintln!("[compat] op.interrupt at: {r:?}");
        }
    }
    let start = Instant::now();
    match at::raw(target(&c), &sanitized, Duration::from_secs(timeout)).await {
        Ok(a) => reply(
            200,
            json!({"ok": true, "data": {"command": sanitized, "response": a.text, "port": a.port,
                "elapsed_ms": start.elapsed().as_millis() as u64}}),
        ),
        Err(e) => reply(503, json!({"ok": false, "error": e})),
    }
}

/// GET /api/at/port: the port the commands use (probed under the lock: it writes "AT").
pub async fn at_port(State(c): State<Compat>) -> Response {
    let data = match at::probe(target(&c)).await {
        Ok(port) => json!({"port": port, "available": true}),
        Err(_) => json!({"port": null, "available": false}),
    };
    reply(200, json!({"ok": true, "data": data}))
}

// ---- USSD: the agent's decoders and parsers ----
//
// u60d's `sms.rs` has a UCS-2 decoder (surrogate pairs joined) and no GSM 7-bit one; these are the
// agent's, copied as they were, so a USSD answer or an STK menu reads the same.

// GSM 7-bit default alphabet → UTF-8

const GSM7_TABLE: &[&str] = &[
    "@", "\u{00A3}", "$", "\u{00A5}", "\u{00E8}", "\u{00E9}", "\u{00F9}", "\u{00EC}", "\u{00F2}",
    "\u{00C7}", "\n", "\u{00D8}", "\u{00F8}", "\r", "\u{00C5}", "\u{00E5}", "_", "_", " ", "!",
    "\"", "#", "\u{00A4}", "%", "&", "'", "(", ")", "*", "+", ",", "-", ".", "/", "0", "1", "2",
    "3", "4", "5", "6", "7", "8", "9", ":", ";", "<", "=", ">", "?", "\u{00A1}", "A", "B", "C",
    "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R", "S", "T", "U", "V",
    "W", "X", "Y", "Z", "\u{00C4}", "\u{00D6}", "\u{00D1}", "\u{00DC}", "\u{00A7}", "\u{00BF}",
    "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r", "s",
    "t", "u", "v", "w", "x", "y", "z", "\u{00E4}", "\u{00F6}", "\u{00F1}", "\u{00FC}", "\u{00E0}",
];

fn hex_to_bytes(hex: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    let mut i = 0;
    while i + 1 < hex.len() {
        if let Ok(b) = u8::from_str_radix(&hex[i..i + 2], 16) {
            bytes.push(b);
        }
        i += 2;
    }
    bytes
}

fn decode_gsm7(hex_str: &str) -> String {
    if hex_str.is_empty() {
        return String::new();
    }
    let bytes = hex_to_bytes(hex_str);
    // Unpack 7-bit septets from 8-bit octets
    let mut septets = Vec::new();
    let mut shift: u32 = 0;
    for (bi, &byte) in bytes.iter().enumerate() {
        let val = if bi > 0 {
            ((byte as u32) << shift) + ((bytes[bi - 1] as u32) >> (8 - shift))
        } else {
            byte as u32
        };
        septets.push((val % 128) as usize);
        shift += 1;
        if shift == 7 {
            septets.push((byte as u32 >> 1) as usize);
            shift = 0;
        }
    }
    let mut out = String::new();
    for s in septets {
        if let Some(&ch) = GSM7_TABLE.get(s) {
            out.push_str(ch);
        }
    }
    out
}

fn decode_ucs2(hex_str: &str) -> String {
    if hex_str.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let mut i = 0;
    while i + 3 < hex_str.len() {
        if let Ok(cp) = u32::from_str_radix(&hex_str[i..i + 4], 16) {
            if let Some(ch) = char::from_u32(cp) {
                out.push(ch);
            }
        }
        i += 4;
    }
    out
}

fn decode_ussd_response(hex_str: &str, dcs: u32) -> String {
    if hex_str.is_empty() {
        return String::new();
    }
    let coding = dcs % 16;
    if coding == 8 {
        return decode_ucs2(hex_str);
    }
    if coding == 0 || coding == 15 {
        let decoded = decode_gsm7(hex_str);
        if !decoded.is_empty() {
            return decoded;
        }
    }
    // Fallback: plain ASCII hex
    let mut out = String::new();
    let bytes = hex_to_bytes(hex_str);
    for b in &bytes {
        if *b >= 32 && *b < 127 {
            out.push(*b as char);
        }
    }
    if !out.is_empty() {
        return out;
    }
    hex_str.to_string()
}

// ---------------------------------------------------------------------------
// Parsers
// ---------------------------------------------------------------------------

fn parse_cusd(raw: &str) -> Option<(i64, String, u32)> {
    // Look for +CUSD: status[,"body"[,dcs]]
    for line in raw.lines() {
        let Some(rest) = line.strip_prefix("+CUSD:") else {
            continue;
        };
        let rest = rest.trim();
        // Parse status
        let status_end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        let status: i64 = rest[..status_end].parse().unwrap_or(-1);
        let after_status = &rest[status_end..];

        // Try to extract body between quotes
        let mut body = String::new();
        let mut dcs: u32 = 15;
        if let Some(q1) = after_status.find('"') {
            let after_q1 = &after_status[q1 + 1..];
            if let Some(q2) = after_q1.find('"') {
                body = after_q1[..q2].to_string();
                // Look for dcs after closing quote
                let after_body = &after_q1[q2 + 1..];
                if let Some(comma) = after_body.find(',') {
                    let dcs_str = after_body[comma + 1..].trim();
                    let dcs_end = dcs_str
                        .find(|c: char| !c.is_ascii_digit())
                        .unwrap_or(dcs_str.len());
                    dcs = dcs_str[..dcs_end].parse().unwrap_or(15);
                }
            }
        }
        return Some((status, body, dcs));
    }
    None
}

// ---------------------------------------------------------------------------
// STK TLV parser
// ---------------------------------------------------------------------------

struct StkMenu {
    title: String,
    items: Vec<Value>,
}

fn parse_stk_menu_tlv(hex: &str) -> Option<StkMenu> {
    if hex.is_empty() {
        return None;
    }
    let mut title = String::new();
    let mut items = Vec::new();
    let mut i = 0;
    while i + 4 <= hex.len() {
        let tag = u8::from_str_radix(&hex[i..i + 2], 16).ok()?;
        let len = usize::from_str_radix(&hex[i + 2..i + 4], 16).ok()?;
        let val_start = i + 4;
        let val_end = val_start + len * 2;
        if val_end > hex.len() {
            break;
        }
        let val_hex = &hex[val_start..val_end];
        match tag {
            0x85 => {
                // Alpha identifier (title)
                title = decode_ucs2(val_hex);
                if title.is_empty() {
                    title = decode_hex_ascii(val_hex);
                }
            }
            0x8F => {
                // Item: first byte = item ID, rest = text
                if val_hex.len() >= 4 {
                    let item_id = u8::from_str_radix(&val_hex[..2], 16).unwrap_or(0);
                    let text_hex = &val_hex[2..];
                    let mut label = decode_ucs2(text_hex);
                    if label.is_empty() {
                        label = decode_hex_ascii(text_hex);
                    }
                    items.push(json!({"id": item_id, "label": label}));
                }
            }
            _ => {}
        }
        i = val_end;
    }
    if !items.is_empty() || !title.is_empty() {
        Some(StkMenu { title, items })
    } else {
        None
    }
}

fn decode_hex_ascii(hex: &str) -> String {
    let bytes = hex_to_bytes(hex);
    let mut out = String::new();
    for b in bytes {
        if b >= 32 {
            out.push(b as char);
        }
    }
    out
}

fn format_ussd_response(parsed: (i64, String, u32)) -> Value {
    let (status, body, dcs) = parsed;
    let decoded = decode_ussd_response(&body, dcs);
    let response = if decoded.is_empty() { &body } else { &decoded };
    let session_active = status == 1;
    json!({
        "response": response,
        "raw_response": body,
        "status": status,
        "dcs": dcs,
        "session_active": session_active,
    })
}

fn ussd_reply(r: Result<String, String>, error_too: bool) -> Response {
    match r {
        Ok(resp) => {
            if error_too && resp.contains("ERROR") {
                return reply(500, json!({"ok": false, "error": "USSD failed"}));
            }
            match parse_cusd(&resp) {
                Some(parsed) => reply(
                    200,
                    json!({"ok": true, "data": format_ussd_response(parsed)}),
                ),
                None => {
                    let clean: String = resp.chars().filter(|c| !c.is_control()).collect();
                    reply(
                        200,
                        json!({"ok": true, "data": {"response": clean, "raw_response": clean,
                            "status": -1, "dcs": 15, "session_active": false}}),
                    )
                }
            }
        }
        Err(e) => reply(503, json!({"ok": false, "error": e})),
    }
}

fn field<'a>(parsed: &'a Value, name: &str) -> Option<&'a str> {
    parsed[name].as_str().filter(|s| !s.is_empty())
}

/// POST /api/ussd/send `{"code": "*123#"}`
pub async fn ussd_send(State(c): State<Compat>, body: Bytes) -> Response {
    let Ok(parsed) = serde_json::from_slice::<Value>(&body) else {
        return reply(400, json!({"ok": false, "error": "invalid JSON"}));
    };
    let Some(code) = field(&parsed, "code") else {
        return reply(400, json!({"ok": false, "error": "missing code"}));
    };
    ussd_reply(at::phone(target(&c), Phone::ussd(code)).await, true)
}

/// POST /api/ussd/respond `{"reply": "1"}`
pub async fn ussd_respond(State(c): State<Compat>, body: Bytes) -> Response {
    let Ok(parsed) = serde_json::from_slice::<Value>(&body) else {
        return reply(400, json!({"ok": false, "error": "invalid JSON"}));
    };
    let Some(answer) = field(&parsed, "reply") else {
        return reply(400, json!({"ok": false, "error": "missing reply"}));
    };
    ussd_reply(at::phone(target(&c), Phone::ussd(answer)).await, false)
}

/// POST /api/ussd/cancel
pub async fn ussd_cancel(State(c): State<Compat>) -> Response {
    match at::phone(target(&c), Phone::UssdCancel).await {
        Ok(_) => reply(200, json!({"ok": true, "data": {"status": "ok"}})),
        Err(e) => reply(503, json!({"ok": false, "error": e})),
    }
}

// ---- STK ----

/// The SIM supports STK: an STK/USAT AID in EF_DIR (AT+CUAD), else any AT+STIN? answer.
async fn stk_support(t: &at::Target) -> (bool, String) {
    if let Ok(resp) = at::phone(t.clone(), Phone::Cuad).await
        && !resp.contains("ERROR")
        && resp.to_uppercase().contains("A000000009")
    {
        return (true, "STK AID found in EF_DIR (AT+CUAD)".into());
    }
    if let Ok(resp) = at::phone(t.clone(), Phone::Stin(2)).await
        && resp.contains("STIN")
        && !resp.contains("ERROR")
    {
        return (true, "STK indicated by AT+STIN? response".into());
    }
    (
        false,
        "No STK AID in EF_DIR and no STIN response — SIM does not support STK".into(),
    )
}

/// GET /api/stk/menu
pub async fn stk_menu(State(c): State<Compat>) -> Response {
    let t = target(&c);
    let (supported, diag) = stk_support(&t).await;
    if !supported {
        return reply(
            200,
            json!({"ok": true, "data": {"supported": false, "items": [], "reason": diag}}),
        );
    }
    let mut diagnostics = vec![diag];
    if let Ok(resp) = at::phone(t.clone(), Phone::Cusatd).await {
        if !resp.contains("ERROR") {
            if let Some(menu) = extract_hex(&resp, "CUSATD:").and_then(|h| parse_stk_menu_tlv(&h)) {
                return reply(
                    200,
                    json!({"ok": true, "data": {"supported": true, "title": menu.title,
                        "items": menu.items, "source": "at_cusatd"}}),
                );
            }
            diagnostics.push("AT+CUSATD=1 responded but no parseable menu".into());
        } else {
            diagnostics.push("AT+CUSATD=1 returned ERROR".into());
        }
    }
    if let Ok(resp) = at::phone(t.clone(), Phone::Stin(3)).await
        && resp.contains("STIN")
    {
        let stin = extract_number_after(&resp, "STIN:");
        if let Some(n @ (37 | 25)) = stin
            && let Ok(stgi) = at::phone(t.clone(), Phone::Stgi(n as u8)).await
            && !stgi.contains("ERROR")
        {
            let (title, items) = parse_stgi_response(&stgi);
            if !items.is_empty() {
                return reply(
                    200,
                    json!({"ok": true, "data": {"supported": true, "title": title,
                        "items": items, "source": "at_stgi"}}),
                );
            }
        }
        diagnostics.push(format!(
            "AT+STIN? returned type {stin:?}, no menu from STGI"
        ));
    }
    reply(
        200,
        json!({"ok": true, "data": {"supported": true, "items": [],
            "reason": "no proactive command pending", "diagnostics": diagnostics}}),
    )
}

/// POST /api/stk/select `{"item_id": 1}`
pub async fn stk_select(State(c): State<Compat>, body: Bytes) -> Response {
    let t = target(&c);
    let (supported, diag) = stk_support(&t).await;
    if !supported {
        return reply(
            200,
            json!({"ok": true, "data": {"supported": false, "reason": diag}}),
        );
    }
    let Ok(parsed) = serde_json::from_slice::<Value>(&body) else {
        return reply(400, json!({"ok": false, "error": "invalid JSON"}));
    };
    let Some(item) = parsed["item_id"].as_u64() else {
        return reply(400, json!({"ok": false, "error": "missing item_id"}));
    };
    if let Ok(resp) = at::phone(t, Phone::Cusate(item as u8)).await
        && !resp.contains("ERROR")
    {
        if let Some(menu) = extract_hex(&resp, "CUSATE:").and_then(|h| parse_stk_menu_tlv(&h))
            && !menu.items.is_empty()
        {
            return reply(
                200,
                json!({"ok": true, "data": {"type": "menu", "title": menu.title,
                    "items": menu.items, "source": "at_cusate"}}),
            );
        }
        let text: String = resp.chars().filter(|c| !c.is_control()).collect();
        return reply(
            200,
            json!({"ok": true, "data": {"type": "display", "data": text}}),
        );
    }
    reply(500, json!({"ok": false, "error": "item selection failed"}))
}

// ---- STK helpers (the agent's) ----

fn extract_hex(resp: &str, prefix: &str) -> Option<String> {
    for line in resp.lines() {
        if let Some(rest) = line.strip_prefix(prefix) {
            let hex: String = rest
                .trim()
                .chars()
                .filter(|c| c.is_ascii_hexdigit())
                .collect();
            if hex.len() >= 4 {
                return Some(hex);
            }
        }
    }
    // Fallback: find any long hex string in response
    for line in resp.lines() {
        let trimmed = line.trim();
        if trimmed.len() >= 8 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(trimmed.to_string());
        }
    }
    None
}

fn extract_number_after(resp: &str, prefix: &str) -> Option<i64> {
    for line in resp.lines() {
        if let Some(rest) = line.strip_prefix(prefix) {
            let num_str: String = rest
                .trim()
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            return num_str.parse().ok();
        }
    }
    None
}

fn parse_stgi_response(stgi: &str) -> (String, Vec<Value>) {
    let mut title = "SIM Menu".to_string();
    let mut items = Vec::new();
    let mut first = true;
    for line in stgi.lines() {
        let Some(rest) = line.strip_prefix("STGI:") else {
            continue;
        };
        let rest = rest.trim();
        if first {
            // First STGI line is the title
            if let Some(t) = rest.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                title = t.to_string();
            }
            first = false;
            continue;
        }
        // Item lines: id,type,"label"
        let parts: Vec<&str> = rest.splitn(3, ',').collect();
        if parts.len() >= 3 {
            let id: u64 = parts[0].trim().parse().unwrap_or(0);
            let label = parts[2].trim().trim_matches('"').to_string();
            items.push(json!({"id": id, "label": label}));
        }
    }
    (title, items)
}
