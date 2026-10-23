//! The AT port's routes: the AT terminal, USSD and the SIM toolkit, all through `crate::at` (the
//! port's one owner: its lock, its port pick). The terminal sends a line a person typed
//! ([`crate::at::raw`]); USSD and STK send commands built from checked parts
//! ([`crate::at::Phone`]), and their hex answers are decoded below.
//!
//! The terminal returns at OK or ERROR rather than waiting out the whole `timeout` (a line the
//! modem volunteers after the answer is not shown), refuses a line with a control character in it
//! (it would reach the modem as two commands), and refuses `+CFUN=` sets ([`radio_set`]) and call
//! control ([`call_control`]).

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
/// only a restart does, and this device is someone's only uplink abroad.
pub(crate) fn radio_set(cmd: &str) -> bool {
    crate::device_profile::policy::radio_set(cmd)
}

/// A call: dial (`ATD…`), answer (`ATA`) or hang up (`+CHUP`, anything but the `=?` test form):
/// refused from the terminal (b31-community A2), a call placed or dropped there is outside every
/// call record. Checked on the line as typed and as sanitized: the shell filter turns `AT&D2` (a
/// DTR setting) into `ATD2`, a dial, so that one is refused too, on purpose. Basic commands chain
/// (V.250: `ATE0D10086` dials), so every basic command before the first extended one is looked at.
pub(crate) fn call_control(cmd: &str) -> bool {
    let s: String = cmd
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_uppercase();
    let basic = s.strip_prefix("AT").unwrap_or("");
    let mut after_amp = false;
    for c in basic.chars() {
        if "+$^%*#!@".contains(c) {
            break;
        }
        if (c == 'D' || c == 'A') && !after_amp {
            return true;
        }
        after_amp = c == '&';
    }
    s.match_indices("+CHUP")
        .any(|(i, _)| !s[i + 5..].starts_with("=?"))
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
            json!({"ok": false, "error": "not an AT command (it has to begin with AT)"}),
        );
    }
    let timeout = parsed["timeout"].as_u64().unwrap_or(3).clamp(1, 30);
    // Shell metacharacters are dropped before sending, as the terminal always has; the checks
    // below look at both forms, since dropping `&` can turn `AT&D2` into `ATD2`.
    const DROPPED: [char; 6] = ['\'', '`', '$', ';', '|', '&'];
    let sanitized = command.replace(DROPPED, "");
    if sanitized.is_empty() {
        return reply(
            400,
            json!({"ok": false, "error": "nothing left to send once unsafe characters are removed"}),
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
    if call_control(command) || call_control(&sanitized) {
        return reply(
            403,
            json!({"ok": false,
                "error": "拨号、接听、挂断不能从终端发",
                "error_en": "ATD/ATA/AT+CHUP are refused from the terminal"}),
        );
    }
    // Any AT command may change the network behind u60d's back: a network-mode change still
    // confirming is cancelled, not rolled back over this one (D40). Best effort. A person's
    // command: luyaz-web's call carries no source (`screen`); the
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
        Err(e) => reply(
            503,
            json!({"ok": false, "error": e, "kind": at::error_kind(&e).name()}),
        ),
    }
}

/// GET /api/at/port: the port the commands use (probed under the lock: it writes "AT").
pub async fn at_port(State(c): State<Compat>) -> Response {
    let data = match at::probe(target(&c)).await {
        Ok(port) => json!({"port": port, "available": true}),
        Err(e) => json!({"port": null, "available": false, "error": e,
            "kind": at::error_kind(&e).name()}),
    };
    reply(200, json!({"ok": true, "data": data}))
}

// ---- USSD and STK text: how the modem's hex comes back as words ----
//
// `sms.rs` only knows UCS-2; a USSD answer can also arrive packed as GSM 03.38 septets, and an STK
// menu as a TLV list. Everything here is pure and has its tests at the bottom of the file.

mod text {
    /// GSM 03.38 default alphabet, by septet value. 0x1B is the escape to [`extension`].
    const GSM_DEFAULT: [char; 128] = [
        '@', '£', '$', '¥', 'è', 'é', 'ù', 'ì', 'ò', 'Ç', '\n', 'Ø', 'ø', '\r', 'Å',
        'å', // 0x00
        'Δ', '_', 'Φ', 'Γ', 'Λ', 'Ω', 'Π', 'Ψ', 'Σ', 'Θ', 'Ξ', '\u{1b}', 'Æ', 'æ', 'ß',
        'É', // 0x10
        ' ', '!', '"', '#', '¤', '%', '&', '\'', '(', ')', '*', '+', ',', '-', '.',
        '/', // 0x20
        '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', ':', ';', '<', '=', '>',
        '?', // 0x30
        '¡', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N',
        'O', // 0x40
        'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z', 'Ä', 'Ö', 'Ñ', 'Ü',
        '§', // 0x50
        '¿', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n',
        'o', // 0x60
        'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z', 'ä', 'ö', 'ñ', 'ü',
        'à', // 0x70
    ];

    /// The default alphabet's character for a septet value.
    pub fn default_char(septet: u8) -> char {
        GSM_DEFAULT[usize::from(septet & 0x7f)]
    }

    /// The default extension table (the septet after 0x1B). A code it does not define reads as
    /// the default table's character, as GSM 03.38 asks of a receiver.
    pub fn extension(septet: u8) -> char {
        match septet {
            0x0A => '\u{0c}',
            0x14 => '^',
            0x28 => '{',
            0x29 => '}',
            0x2F => '\\',
            0x3C => '[',
            0x3D => '~',
            0x3E => ']',
            0x40 => '|',
            0x65 => '€',
            other => default_char(other),
        }
    }

    /// Septet values packed low bit first (GSM 03.38 §6.1.2.1). When the octets end exactly on
    /// a septet boundary with 7 spare bits, a final 0x00 or CR is padding (GSM 03.38 §6.1.2.3.1)
    /// and dropped.
    pub fn septets(hex: &str) -> Vec<u8> {
        let raw = octets(hex);
        let mut out = Vec::with_capacity(raw.len() * 8 / 7);
        let (mut bits, mut held) = (0u32, 0u32);
        for o in &raw {
            bits |= u32::from(*o) << held;
            held += 8;
            while held >= 7 {
                out.push((bits & 0x7f) as u8);
                bits >>= 7;
                held -= 7;
            }
        }
        if raw.len().is_multiple_of(7) && matches!(out.last(), Some(0x00 | 0x0D)) {
            out.pop();
        }
        out
    }

    /// Hex digits two at a time; a pair that is not hex is skipped, an odd last digit dropped.
    pub fn octets(hex: &str) -> Vec<u8> {
        hex.as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .filter_map(|pair| std::str::from_utf8(pair).ok())
            .filter_map(|pair| u8::from_str_radix(pair, 16).ok())
            .collect()
    }

    /// GSM 7-bit text: the default alphabet with its extension table. A lone escape at the
    /// end is dropped.
    pub fn gsm7(hex: &str) -> String {
        let mut out = String::new();
        let mut it = septets(hex).into_iter();
        while let Some(s) = it.next() {
            match s {
                0x1B => out.extend(it.next().map(extension)),
                _ => out.push(default_char(s)),
            }
        }
        out
    }

    /// Big-endian UTF-16 units, four hex digits each, decoded one unit at a time (a lone surrogate
    /// half is dropped, not joined).
    pub fn ucs2(hex: &str) -> String {
        hex.as_bytes()
            .as_chunks::<4>()
            .0
            .iter()
            .filter_map(|quad| std::str::from_utf8(quad).ok())
            .filter_map(|quad| u32::from_str_radix(quad, 16).ok())
            .filter_map(char::from_u32)
            .collect()
    }

    /// Octets as Latin-1, control codes left out.
    pub fn latin1(hex: &str) -> String {
        octets(hex)
            .into_iter()
            .filter(|&b| b >= 0x20)
            .map(char::from)
            .collect()
    }

    /// A USSD string by its data coding scheme's low nibble: 8 = UCS-2; 0 and 15 = GSM 7-bit
    /// (when that yields anything); otherwise, or as the fallback, printable ASCII; and when even
    /// that is empty, the hex as it came.
    pub fn ussd(hex: &str, dcs: u32) -> String {
        if hex.is_empty() {
            return String::new();
        }
        match dcs & 0x0f {
            8 => return ucs2(hex),
            0 | 15 => {
                let s = gsm7(hex);
                if !s.is_empty() {
                    return s;
                }
            }
            _ => {}
        }
        let printable: String = octets(hex)
            .into_iter()
            .filter(|b| (0x20..0x7f).contains(b))
            .map(char::from)
            .collect();
        if printable.is_empty() {
            hex.to_string()
        } else {
            printable
        }
    }

    /// The leading run of ASCII digits of `s`.
    pub fn digits(s: &str) -> &str {
        let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
        &s[..end]
    }
}

/// One `+CUSD: <m>[,"<str>"[,<dcs>]]` line: status (-1 when unreadable), the quoted string as
/// sent, and its coding scheme (15 when absent).
struct Cusd {
    status: i64,
    body: String,
    dcs: u32,
}

impl Cusd {
    fn find(answer: &str) -> Option<Cusd> {
        let rest = answer
            .lines()
            .find_map(|l| l.strip_prefix("+CUSD:"))?
            .trim();
        let status_txt = text::digits(rest);
        let mut cusd = Cusd {
            status: status_txt.parse().unwrap_or(-1),
            body: String::new(),
            dcs: 15,
        };
        let tail = &rest[status_txt.len()..];
        let quoted = tail
            .split_once('"')
            .and_then(|(_, after_open)| after_open.split_once('"'));
        if let Some((body, after_close)) = quoted {
            cusd.body = body.to_string();
            if let Some((_, dcs_txt)) = after_close.split_once(',') {
                cusd.dcs = text::digits(dcs_txt.trim()).parse().unwrap_or(15);
            }
        }
        Some(cusd)
    }

    fn to_json(&self) -> Value {
        let decoded = text::ussd(&self.body, self.dcs);
        json!({
            "response": if decoded.is_empty() { self.body.as_str() } else { decoded.as_str() },
            "raw_response": self.body,
            "status": self.status,
            "dcs": self.dcs,
            "session_active": self.status == 1,
        })
    }
}

/// A SIM toolkit menu: its title and `{"id","label"}` items.
struct StkMenu {
    title: String,
    items: Vec<Value>,
}

/// UCS-2 first, Latin-1 when that gives nothing.
fn stk_text(hex: &str) -> String {
    let s = text::ucs2(hex);
    if s.is_empty() { text::latin1(hex) } else { s }
}

impl StkMenu {
    /// SET UP MENU's simple-TLV list in hex: tag 0x85 (alpha identifier) is the title, each 0x8F
    /// (item: one id octet, then the text) an entry. A header that is not hex gives `None`; a
    /// length running past the end stops the walk. `None` too when neither part was found.
    fn from_tlv(hex: &str) -> Option<StkMenu> {
        let mut menu = StkMenu {
            title: String::new(),
            items: Vec::new(),
        };
        let mut rest = hex;
        while rest.len() >= 4 {
            let tag = u8::from_str_radix(&rest[..2], 16).ok()?;
            let octets = usize::from_str_radix(&rest[2..4], 16).ok()?;
            let Some(value) = rest.get(4..4 + octets * 2) else {
                break;
            };
            if tag == 0x85 {
                menu.title = stk_text(value);
            } else if tag == 0x8F && value.len() >= 4 {
                let id = u8::from_str_radix(&value[..2], 16).unwrap_or(0);
                menu.items
                    .push(json!({"id": id, "label": stk_text(&value[2..])}));
            }
            rest = &rest[4 + octets * 2..];
        }
        (!menu.title.is_empty() || !menu.items.is_empty()).then_some(menu)
    }

    /// The `+STGI` listing: the first `STGI:` line is the quoted title, every later one
    /// `id,type,"label"`.
    fn from_stgi(answer: &str) -> (String, Vec<Value>) {
        let mut lines = answer
            .lines()
            .filter_map(|l| l.strip_prefix("STGI:"))
            .map(str::trim);
        let title = lines
            .next()
            .and_then(|t| t.strip_prefix('"')?.strip_suffix('"'))
            .unwrap_or("SIM Menu")
            .to_string();
        let items = lines
            .filter_map(|l| {
                let mut f = l.splitn(3, ',');
                let id = f.next()?.trim().parse::<u64>().unwrap_or(0);
                let label = f.nth(1)?.trim().trim_matches('"');
                Some(json!({"id": id, "label": label}))
            })
            .collect();
        (title, items)
    }
}

/// The hex payload after `prefix` (at least two octets of it), else the first bare line of at
/// least eight hex digits.
fn hex_payload(answer: &str, prefix: &str) -> Option<String> {
    let after_prefix = answer
        .lines()
        .filter_map(|l| l.strip_prefix(prefix))
        .find_map(|rest| {
            let h: String = rest.chars().filter(char::is_ascii_hexdigit).collect();
            (h.len() >= 4).then_some(h)
        });
    after_prefix.or_else(|| {
        answer
            .lines()
            .map(str::trim)
            .find(|l| l.len() >= 8 && l.chars().all(|c| c.is_ascii_hexdigit()))
            .map(String::from)
    })
}

/// The number at the start of the first line beginning with `prefix`.
fn number_after(answer: &str, prefix: &str) -> Option<i64> {
    let rest = answer.lines().find_map(|l| l.strip_prefix(prefix))?;
    text::digits(rest.trim()).parse().ok()
}

fn printable(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

fn port_error(e: String) -> Response {
    let kind = at::error_kind(&e).name();
    reply(503, json!({"ok": false, "error": e, "kind": kind}))
}

/// A USSD exchange's answer: the decoded `+CUSD`, or the text as it came when there is none.
/// `fail_on_error`: a fresh request answered with ERROR is a 500 (a reply inside a session is not
/// checked, as before).
fn ussd_answer(r: Result<String, String>, fail_on_error: bool) -> Response {
    let answer = match r {
        Ok(a) => a,
        Err(e) => return port_error(e),
    };
    if fail_on_error && answer.contains("ERROR") {
        return reply(500, json!({"ok": false, "error": "USSD failed"}));
    }
    let data = match Cusd::find(&answer) {
        Some(cusd) => cusd.to_json(),
        None => {
            let flat = printable(&answer);
            json!({"response": flat, "raw_response": flat, "status": -1, "dcs": 15,
                "session_active": false})
        }
    };
    reply(200, json!({"ok": true, "data": data}))
}

fn bad_request(msg: &str) -> Response {
    reply(400, json!({"ok": false, "error": msg}))
}

/// The body's non-empty string field `name`; `Err` = the 400's message.
fn string_field(body: &[u8], name: &str) -> Result<String, String> {
    let v: Value = serde_json::from_slice(body).map_err(|_| "invalid JSON".to_string())?;
    v[name]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(String::from)
        .ok_or_else(|| format!("missing {name}"))
}

/// POST /api/ussd/send `{"code": "*123#"}`
pub async fn ussd_send(State(c): State<Compat>, body: Bytes) -> Response {
    match string_field(&body, "code") {
        Ok(code) => ussd_answer(at::phone(target(&c), Phone::ussd(&code)).await, true),
        Err(msg) => bad_request(&msg),
    }
}

/// POST /api/ussd/respond `{"reply": "1"}`
pub async fn ussd_respond(State(c): State<Compat>, body: Bytes) -> Response {
    match string_field(&body, "reply") {
        Ok(answer) => ussd_answer(at::phone(target(&c), Phone::ussd(&answer)).await, false),
        Err(msg) => bad_request(&msg),
    }
}

/// POST /api/ussd/cancel
pub async fn ussd_cancel(State(c): State<Compat>) -> Response {
    match at::phone(target(&c), Phone::UssdCancel).await {
        Ok(_) => reply(200, json!({"ok": true, "data": {"status": "ok"}})),
        Err(e) => port_error(e),
    }
}

// ---- SIM toolkit ----

/// USAT's registered application id prefix, looked for in `AT+CUAD`'s EF_DIR listing.
const USAT_AID: &str = "A000000009";

// The reasons `stk_support` gives, as the page shows them.
const WHY_AID: &str = "the SIM lists a USAT application (AT+CUAD)";
const WHY_STIN: &str = "the SIM answered AT+STIN?";

/// Whether the SIM speaks STK, and the line saying why: a USAT AID in EF_DIR, failing that any
/// `+STIN?` answer.
async fn stk_support(t: &at::Target) -> (bool, String) {
    let ok = |r: &Result<String, String>| matches!(r, Ok(a) if !a.contains("ERROR"));
    let cuad = at::phone(t.clone(), Phone::Cuad).await;
    if ok(&cuad)
        && cuad
            .as_deref()
            .unwrap_or("")
            .to_uppercase()
            .contains(USAT_AID)
    {
        return (true, WHY_AID.into());
    }
    let stin = at::phone(t.clone(), Phone::Stin(2)).await;
    if ok(&stin) && stin.as_deref().unwrap_or("").contains("STIN") {
        return (true, WHY_STIN.into());
    }
    (
        false,
        "this SIM has no SIM menu: no USAT application listed, no AT+STIN? answer".into(),
    )
}

fn menu_reply(kind: Option<&str>, title: &str, items: Vec<Value>, source: &str) -> Response {
    let mut data = json!({"title": title, "items": items, "source": source});
    match kind {
        Some(k) => data["type"] = json!(k),
        None => data["supported"] = json!(true),
    }
    reply(200, json!({"ok": true, "data": data}))
}

/// GET /api/stk/menu: the menu from `+CUSATD`, else from `+STIN?`/`+STGI` (a pending SET UP MENU
/// or SELECT ITEM), else an empty list with what each try said.
pub async fn stk_menu(State(c): State<Compat>) -> Response {
    let t = target(&c);
    let (supported, why) = stk_support(&t).await;
    if !supported {
        return reply(
            200,
            json!({"ok": true, "data": {"supported": false, "items": [], "reason": why}}),
        );
    }
    let mut notes = vec![why];

    if let Ok(a) = at::phone(t.clone(), Phone::Cusatd).await {
        if a.contains("ERROR") {
            notes.push("AT+CUSATD=1: modem said ERROR".into());
        } else if let Some(m) = hex_payload(&a, "CUSATD:").and_then(|h| StkMenu::from_tlv(&h)) {
            return menu_reply(None, &m.title, m.items, "at_cusatd");
        } else {
            notes.push("AT+CUSATD=1: answered, but no menu could be read from it".into());
        }
    }

    if let Ok(a) = at::phone(t.clone(), Phone::Stin(3)).await
        && a.contains("STIN")
    {
        // 37 = SET UP MENU, 25 = SELECT ITEM: the two proactive commands that carry a list
        let pending = number_after(&a, "STIN:");
        if let Some(cmd @ (37 | 25)) = pending
            && let Ok(listing) = at::phone(t.clone(), Phone::Stgi(cmd as u8)).await
            && !listing.contains("ERROR")
        {
            let (title, items) = StkMenu::from_stgi(&listing);
            if !items.is_empty() {
                return menu_reply(None, &title, items, "at_stgi");
            }
        }
        notes.push(format!(
            "AT+STIN? returned type {pending:?}, no menu from STGI"
        ));
    }

    reply(
        200,
        json!({"ok": true, "data": {"supported": true, "items": [],
            "reason": "the SIM has no menu waiting right now", "diagnostics": notes}}),
    )
}

/// POST /api/stk/select `{"item_id": 1}`: a sub-menu when the SIM answers with one, else the
/// answer as text.
pub async fn stk_select(State(c): State<Compat>, body: Bytes) -> Response {
    let t = target(&c);
    let (supported, why) = stk_support(&t).await;
    if !supported {
        return reply(
            200,
            json!({"ok": true, "data": {"supported": false, "reason": why}}),
        );
    }
    let Ok(v) = serde_json::from_slice::<Value>(&body) else {
        return bad_request("invalid JSON");
    };
    let Some(item) = v["item_id"].as_u64() else {
        return bad_request("missing item_id");
    };
    let answer = match at::phone(t, Phone::Cusate(item as u8)).await {
        Ok(a) if !a.contains("ERROR") => a,
        _ => return reply(500, json!({"ok": false, "error": "item selection failed"})),
    };
    match hex_payload(&answer, "CUSATE:").and_then(|h| StkMenu::from_tlv(&h)) {
        Some(m) if !m.items.is_empty() => menu_reply(Some("menu"), &m.title, m.items, "at_cusate"),
        _ => reply(
            200,
            json!({"ok": true, "data": {"type": "display", "data": printable(&answer)}}),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test-only encoder: text → septets (default table, else escape + extension) → packed hex.
    fn pack(t: &str) -> String {
        let mut sept = Vec::new();
        for ch in t.chars() {
            let default = (0u8..128).find(|&i| i != 0x1B && text::default_char(i) == ch);
            match default {
                Some(i) => sept.push(i),
                None => {
                    let e = (0u8..128)
                        .find(|&i| text::extension(i) == ch && text::default_char(i) != ch)
                        .unwrap_or_else(|| panic!("{ch:?} has no GSM 7-bit code"));
                    sept.extend([0x1B, e]);
                }
            }
        }
        let (mut bits, mut held, mut hex) = (0u32, 0u32, String::new());
        for s in sept {
            bits |= u32::from(s) << held;
            held += 7;
            while held >= 8 {
                hex += &format!("{:02X}", bits & 0xff);
                bits >>= 8;
                held -= 8;
            }
        }
        if held > 0 {
            hex += &format!("{:02X}", bits & 0xff);
        }
        hex
    }

    #[test]
    fn gsm7_known_vectors() {
        assert_eq!(text::gsm7("C8329BFD06"), "Hello");
        assert_eq!(text::gsm7("E8329BFD4697D9EC37"), "hellohello");
        assert_eq!(text::ussd("C8329BFD06", 15), "Hello");
        // 7 octets with a zero last septet: padding, not '@'
        assert_eq!(text::gsm7(&pack("1234567")), "1234567");
        // escape sequences: ESC 0x65 = €, ESC 0x0A = form feed
        assert_eq!(text::gsm7(&pack("5 €")), "5 €");
        assert_eq!(text::gsm7(&pack("a\u{0c}b")), "a\u{0c}b");
        assert_eq!(text::gsm7(&pack("{[~]}|^\\")), "{[~]}|^\\");
        // a balance reply as a carrier sends it
        let reply = "Your balance is $12.50, valid until 2026-12-31. Reply 1 for offers [Δ]";
        assert_eq!(text::ussd(&pack(reply), 15), reply);
    }

    #[test]
    fn gsm7_round_trips_printable_ascii() {
        // every printable ASCII character GSM 7-bit defines (all but the backtick)
        let all: String = (0x20u8..0x7f)
            .map(char::from)
            .filter(|&c| c != '`')
            .collect();
        assert_eq!(text::gsm7(&pack(&all)), all);
    }

    #[test]
    fn ucs2_and_fallbacks() {
        assert_eq!(text::ussd("4F60597D", 72), "你好");
        assert_eq!(text::ussd("414243", 4), "ABC");
        assert_eq!(text::ussd("0102", 4), "0102");
        assert_eq!(text::ussd("", 8), "");
    }

    #[test]
    fn cusd_line() {
        let c = Cusd::find("OK\n+CUSD: 1,\"4F60\",72\n").unwrap();
        assert_eq!((c.status, c.body.as_str(), c.dcs), (1, "4F60", 72));
        assert_eq!(c.to_json()["response"], "你");
        assert_eq!(c.to_json()["session_active"], true);
        let bare = Cusd::find("+CUSD: 2").unwrap();
        assert_eq!((bare.status, bare.body.as_str(), bare.dcs), (2, "", 15));
        assert!(Cusd::find("OK").is_none());
    }

    #[test]
    fn stk_tlv_and_stgi() {
        let m = StkMenu::from_tlv("850200418F03010042").unwrap();
        assert_eq!(m.title, "A");
        assert_eq!(m.items, vec![json!({"id": 1, "label": "B"})]);
        assert!(StkMenu::from_tlv("").is_none());
        assert!(StkMenu::from_tlv("ZZ01").is_none());
        let (t, items) =
            StkMenu::from_stgi("STGI: \"Menu\"\nSTGI: 1,0,\"One\"\nSTGI: 2,0,\"Two\"\nOK");
        assert_eq!(t, "Menu");
        assert_eq!(items[1], json!({"id": 2, "label": "Two"}));
        assert_eq!(StkMenu::from_stgi("OK").0, "SIM Menu");
        assert_eq!(number_after("STIN: 37\nOK", "STIN:"), Some(37));
        assert_eq!(
            hex_payload("CUSATD: 0,\"D00A\"", "CUSATD:").as_deref(),
            // every hex digit after the prefix counts, the status digit too
            Some("0D00A")
        );
        assert_eq!(
            hex_payload("x\nD0112233\n", "CUSATD:").as_deref(),
            Some("D0112233")
        );
    }
}
