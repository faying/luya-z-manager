//! /sms/compose (写短信, hidden: reached from /sms): recipient,
//! text, length, send. Send is tier 2 (DESIGN.md §5; controls-inventory §/sms/compose): it goes
//! out over the mobile network and the carrier may charge per part, so the inline confirm names
//! the recipient, the length and the number of parts.
//!
//! ```text
//!  发送 ─▶ POST ask (signals {to, body} in the body) ─▶ checked here (the old page's words)
//!       ─▶ confirm: 「把 N 个字符发给 X，分 M 条…」 (+ a line when `signal.true_roaming`)
//!       ─▶ POST send?h=<hash of what the confirm named> (same signals) ─▶ same hash or not sent
//!       ─▶ u60d sms.send_raw {number, message_hex, sms_time} (fresh op_id, never retried)
//!       ─▶ 「设备已接受：发给 X 的短信」, the form cleared; never "成功"
//! ```
//! - **u60d sends every SMS as UCS-2** (`send_raw` passes the hex as is and fixes `encode_type:
//!   UNICODE`), so a part holds 70 UTF-16 units, 67 when split, ASCII too. The old page counted
//!   ASCII text in GSM-7 parts (160 / 153), which the device never used through this path; the
//!   count and the confirm now say what is actually sent.
//! - No readback: the outbox is not something the page can read back. send_raw itself waits for
//!   the firmware's command status (sent / failed, up to 10 s), so the request gets 40 s, not the
//!   shared client's 10 (a slow success would read as 「连接中断」 and invite a paid resend).
//! - The number and text travel in the POST body (Datastar signals), never in a URL, and are not
//!   logged.

use std::hash::{Hash, Hasher};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use askama::Template;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use crate::hub::St;
use crate::i18n::Lang;
use crate::pages::Shared;
use crate::write;

pub const PATH: &str = "/sms/compose";
/// u60d's limit: `message_hex` ≤ 4096 hex digits = 1024 UTF-16 units.
pub const MAX_UNITS: usize = 1024;
/// send_raw: up to 10 s of command-status polling.
const SEND_TIMEOUT: Duration = Duration::from_secs(40);

/// Spaces, dashes and brackets out (the old `cleanNumber`).
pub fn clean_number(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '-' | '(' | ')'))
        .collect()
}

/// Digits with an optional leading +, 3 to 20 digits (the old `isNumber`).
pub fn is_number(s: &str) -> bool {
    let d = s.strip_prefix('+').unwrap_or(s);
    (3..=20).contains(&d.len()) && d.bytes().all(|b| b.is_ascii_digit())
}

/// UTF-16 units (what a part holds, and what JS `.length` counts in the page's counter).
pub fn units(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Parts on the air, UCS-2: one part holds 70, split parts 67.
pub fn parts(units: usize) -> usize {
    match units {
        0 => 0,
        1..=70 => 1,
        n => n.div_ceil(67),
    }
}

/// UTF-16BE hex, upper case (u60-sms-forward's `encode_ucs2_hex`).
pub fn ucs2_hex(text: &str) -> String {
    text.encode_utf16().map(|c| format!("{c:04X}")).collect()
}

/// What the form holds, as the server checks it.
#[derive(Debug, PartialEq)]
pub struct Draft {
    pub number: String,
    pub text: String,
}

/// The old page's checks and words; one more: the length u60d takes.
pub fn check(to: &str, body: &str, l: Lang) -> Result<Draft, String> {
    let number = clean_number(to);
    let text = body.trim().to_owned();
    if number.is_empty() {
        return Err(l
            .pick("Enter a number to send to.", "先填收件号码。")
            .into());
    }
    if !is_number(&number) {
        return Err(l
            .pick(
                "Enter digits only, optionally starting with +, 3 to 20 digits.",
                "号码只能是数字，可以用 + 开头，3 到 20 位。",
            )
            .into());
    }
    if text.is_empty() {
        return Err(l
            .pick("Write something to send.", "短信还没写内容。")
            .into());
    }
    if units(&text) > MAX_UNITS {
        return Err(match l {
            Lang::En => format!("The message is too long: at most {MAX_UNITS} characters."),
            Lang::Zh => format!("内容太长：最多 {MAX_UNITS} 个字符。"),
        });
    }
    Ok(Draft { number, text })
}

/// What the confirm named, as a token for the send: the send goes only if the form still holds
/// exactly that.
pub fn hash(d: &Draft) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    d.number.hash(&mut h);
    d.text.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// The device is in another country (`signal.true_roaming`; the setting `roam_enable` is not it).
pub fn roaming(st: &St) -> bool {
    st.blocks
        .get("signal")
        .filter(|b| !b.stale)
        .and_then(|b| b.data["true_roaming"].as_bool())
        == Some(true)
}

pub fn confirm(d: &Draft, roaming: bool, l: Lang) -> write::Confirm {
    let (chars, n) = (units(&d.text), parts(units(&d.text)));
    let to = &d.number;
    let mut what = match l {
        Lang::En => format!(
            "Sends {chars} characters to {to} as {n} SMS part(s) over the mobile network. Your carrier may charge for each part. A sent SMS can't be recalled."
        ),
        Lang::Zh => format!(
            "把 {chars} 个字符发给 {to}，分 {n} 条短信经移动网络发出，运营商可能按条收费。发出后没法撤回。"
        ),
    };
    if roaming {
        what.push_str(l.pick(
            " The device is roaming abroad now; your carrier may charge roaming rates for SMS.",
            "设备现在在国外漫游，短信可能按漫游资费收费。",
        ));
    }
    let title = match l {
        Lang::En => format!("send to {to}"),
        Lang::Zh => format!("发给 {to}"),
    };
    write::Confirm {
        tier: write::Tier::Inline,
        action: match l {
            Lang::En => format!("Confirm: {title}"),
            Lang::Zh => format!("确认：{title}"),
        },
        title,
        what,
        downtime: None,
        recovery: None,
        cuts_uplink: false,
        word: None,
        danger: false,
        revert: crate::write::Revert::No,
    }
}

/// "YY;MM;DD;HH;MM;SS;+Z" (u60-sms-forward's `format_sms_time`): the device clock's digits (local
/// time labelled UTC, so the UTC digits are the local ones) and the zone in whole hours.
pub fn sms_time(at: SystemTime, zone_hours: i64) -> String {
    let secs = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // civil from days (H. Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{:02};{:02};{:02};{:02};{:02};{:02};{}{}",
        y % 100,
        m,
        d,
        rem / 3600,
        rem / 60 % 60,
        rem % 60,
        if zone_hours >= 0 { "+" } else { "" },
        zone_hours
    )
}

/// ZTE's SNTP zone (`zwrt_zte_sntp.settings.time_from_utc`, then `.timezone`, "8.00"), in whole
/// hours like u60d's `sms_zone_hours`; 0 when uci is not there (tests, a dev box). 5 min cache.
async fn zone_hours() -> i64 {
    static CACHE: Mutex<Option<(Instant, i64)>> = Mutex::new(None);
    if let Some((at, v)) = *CACHE.lock().unwrap()
        && at.elapsed() < Duration::from_secs(300)
    {
        return v;
    }
    let v = tokio::task::spawn_blocking(|| {
        let get = |k: &str| {
            std::process::Command::new("uci")
                .args(["-q", "get", k])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .and_then(|s| s.trim().parse::<f64>().ok())
                .filter(|h| (-14.0..=14.0).contains(h))
        };
        get("zwrt_zte_sntp.settings.time_from_utc")
            .or_else(|| get("zwrt_zte_sntp.settings.timezone"))
            .map_or(0, |h| h.trunc() as i64)
    })
    .await
    .unwrap_or(0);
    *CACHE.lock().unwrap() = Some((Instant::now(), v));
    v
}

// ---- page ----

#[derive(Template)]
#[template(path = "sms_compose.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    op: String,
}

impl Page {
    /// The counter under the text (UTF-16 units, as sent; parts at 70 / 67).
    fn count_js(&self) -> &'static str {
        self.l.pick(
            "(n => n + ' chars · ' + (n === 0 ? 0 : n <= 70 ? 1 : Math.ceil(n / 67)) + (n > 0 && n <= 70 ? ' segment' : ' segments'))($body.trim().length)",
            "(n => n + ' 个字符 · ' + (n === 0 ? 0 : n <= 70 ? 1 : Math.ceil(n / 67)) + ' 条')($body.trim().length)",
        )
    }
    fn count0(&self) -> &'static str {
        self.l.pick("0 chars · 0 segments", "0 个字符 · 0 条")
    }
}

fn area(l: Lang, set: &str, phase: &write::Phase) -> String {
    write::Area {
        id: "smsc",
        l,
        remote: crate::topbar::Via::Local,
        set,
        cancel: "/sms/compose/cancel",
        phase,
    }
    .html()
}

fn err_html(e: Option<&str>) -> String {
    match e {
        None => "<div id=\"smsc-err\"></div>".into(),
        Some(e) => format!(
            "<div id=\"smsc-err\"><p role=\"alert\" class=\"nd-error\">{}</p></div>",
            crate::agent::esc(e)
        ),
    }
}

pub async fn page(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let shell = app
        .hub
        .read(|st| crate::shell::Shell::new(&headers, PATH).live(st, &ctx, None));
    let html = Page {
        shell,
        l: ctx.lang,
        op: area(ctx.lang, "/sms/compose/send", &write::Phase::Idle),
    }
    .render()
    .unwrap();
    crate::assets::maybe_gzip(
        &headers,
        "text/html; charset=utf-8",
        html.into_bytes(),
        "no-store",
    )
}

/// The page's signals as Datastar posts them.
#[derive(Deserialize, Default)]
pub struct Form {
    #[serde(default)]
    to: String,
    #[serde(default)]
    body: String,
}

fn form(body: &str) -> Form {
    serde_json::from_str(body).unwrap_or_default()
}

fn patches(
    html: String,
) -> axum::response::sse::Sse<
    impl futures_util::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>,
> {
    axum::response::sse::Sse::new(futures_util::stream::once(async move {
        Ok(crate::pages::patch(&html))
    }))
}

/// 发送: check the form; good → the confirm (tier 2), else the old page's sentence.
pub async fn ask(State(app): State<Shared>, headers: HeaderMap, body: String) -> impl IntoResponse {
    let l = Lang::from_headers(&headers);
    let f = form(&body);
    let html = match check(&f.to, &f.body, l) {
        Err(e) => format!(
            "{}\n{}",
            err_html(Some(&e)),
            area(l, "/sms/compose/send", &write::Phase::Idle)
        ),
        Ok(d) => {
            let set = format!("/sms/compose/send?h={}", hash(&d));
            let roam = app.hub.read(roaming);
            format!(
                "{}\n{}",
                err_html(None),
                area(l, &set, &write::Phase::Ask(confirm(&d, roam, l)))
            )
        }
    };
    patches(html)
}

pub async fn cancel(Query(q): Query<write::CancelQuery>, headers: HeaderMap) -> impl IntoResponse {
    patches(area(
        Lang::from_headers(&headers),
        "/sms/compose/send",
        &write::cancel_phase(&q),
    ))
}

#[derive(Deserialize, Default)]
pub struct H {
    #[serde(default)]
    h: String,
}

/// The confirm's button: sends exactly what the confirm named, once.
pub async fn send(
    State(app): State<Shared>,
    Query(q): Query<H>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    let l = Lang::from_headers(&headers);
    let f = form(&body);
    let draft = check(&f.to, &f.body, l);
    let pending = area(
        l,
        "/sms/compose/send",
        &write::Phase::Pending(l.pick("Sending…", "提交中…").into()),
    );
    write::respond(pending, async move {
        let d = match draft {
            Ok(d) if hash(&d) == q.h => d,
            Ok(_) => {
                return area(
                    l,
                    "/sms/compose/send",
                    &write::Phase::Done(write::Outcome::NotYet(
                        l.pick(
                            "Not sent: the number or text changed after you confirmed. Press Send again.",
                            "没有发出：确认之后号码或内容又改了，请再点一次「发送」。",
                        )
                        .into(),
                    )),
                );
            }
            Err(e) => {
                return format!(
                    "{}\n{}",
                    err_html(Some(&e)),
                    area(l, "/sms/compose/send", &write::Phase::Idle)
                );
            }
        };
        let w = write::Write {
            action: "sms.send_raw",
            params: json!({
                "number": d.number,
                "message_hex": ucs2_hex(&d.text),
                "sms_time": sms_time(SystemTime::now(), zone_hours().await),
            }),
            readback: None,
            applied: String::new(),
            timeout: write::READBACK,
            typed: None,
            request_timeout: Some(SEND_TIMEOUT),
        };
        let o = match write::send(&app.http, &app.control_url, &w, l).await {
            Ok(write::Sent::Ok) => write::Outcome::AcceptedSaid(match l {
                Lang::En => format!("Accepted by the device: SMS to {}", d.number),
                Lang::Zh => format!("设备已接受：发给 {} 的短信", d.number),
            }),
            Ok(s) => s.not_read(),
            Err(o) => o,
        };
        let sent = matches!(o, write::Outcome::AcceptedSaid(_));
        let mut html = area(l, "/sms/compose/send", &write::Phase::Done(o));
        if sent {
            // the form empties, so the same text can't go out twice by accident (the old page
            // left for the inbox)
            html.push_str(&format!(
                "\n<span id=\"smsc-reset\" hidden data-init=\"$to = ''; $body = ''; $_x = '{}'\"></span>",
                write::next_op_id()
            ));
        }
        html
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_text_are_checked_with_the_old_words() {
        let l = Lang::Zh;
        assert_eq!(check("", "x", l).unwrap_err(), "先填收件号码。");
        assert_eq!(
            check("12a", "x", l).unwrap_err(),
            "号码只能是数字，可以用 + 开头，3 到 20 位。"
        );
        assert_eq!(
            check("+886 912-345 (678)", "  ", l).unwrap_err(),
            "短信还没写内容。"
        );
        let d = check("+886 912-345 (678)", "  hi \n", l).unwrap();
        assert_eq!(
            d,
            Draft {
                number: "+886912345678".into(),
                text: "hi".into()
            }
        );
        assert!(
            check("10086", &"字".repeat(1025), l)
                .unwrap_err()
                .contains("1024")
        );
        assert!(check("10086", &"字".repeat(1024), l).is_ok());
        assert!(check("++10086", "x", l).is_err());
    }

    #[test]
    fn parts_are_ucs2_parts() {
        assert_eq!(parts(0), 0);
        assert_eq!(parts(70), 1);
        assert_eq!(parts(71), 2);
        assert_eq!(parts(134), 2);
        assert_eq!(parts(135), 3);
        // an emoji is two units (as JS counts it)
        assert_eq!(units("a😀"), 3);
        assert_eq!(ucs2_hex("测A😀"), "6D4B0041D83DDE00");
    }

    #[test]
    fn the_confirm_names_recipient_length_parts_and_roaming() {
        let d = Draft {
            number: "10086".into(),
            text: "x".repeat(71),
        };
        let c = confirm(&d, false, Lang::Zh);
        assert_eq!(c.tier, write::Tier::Inline);
        assert_eq!(c.action, "确认：发给 10086");
        assert!(
            c.what.contains("把 71 个字符发给 10086，分 2 条短信"),
            "{}",
            c.what
        );
        assert!(!c.what.contains("漫游"));
        assert!(
            confirm(&d, true, Lang::Zh)
                .what
                .contains("设备现在在国外漫游")
        );
        let en = confirm(&d, true, Lang::En);
        for s in [&en.what, &en.action, &en.title] {
            assert!(!s.chars().any(|c| c > '\u{2fff}'), "{s}");
        }
    }

    #[test]
    fn the_hash_follows_what_was_confirmed() {
        let a = Draft {
            number: "10086".into(),
            text: "hi".into(),
        };
        let b = Draft {
            number: "10086".into(),
            text: "hi!".into(),
        };
        assert_eq!(
            hash(&a),
            hash(&Draft {
                number: "10086".into(),
                text: "hi".into()
            })
        );
        assert_ne!(hash(&a), hash(&b));
    }

    #[test]
    fn sms_time_is_the_firmware_format() {
        // 2026-10-12 14:03:09 (device clock: local digits labelled UTC)
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_791_813_789);
        assert_eq!(sms_time(t, 8), "26;10;12;14;03;09;+8");
        assert_eq!(sms_time(t, -3), "26;10;12;14;03;09;-3");
    }

    #[test]
    fn the_english_page_has_no_chinese() {
        let l = Lang::En;
        let d = Draft {
            number: "10086".into(),
            text: "hi".into(),
        };
        let mut all = vec![
            area(l, "/x", &write::Phase::Ask(confirm(&d, true, l))),
            area(
                l,
                "/x",
                &write::Phase::Pending(l.pick("Sending…", "提交中…").into()),
            ),
            err_html(check("", "", l).err().as_deref()),
            err_html(check("1", "x", l).err().as_deref()),
            err_html(check("10086", &"a".repeat(2000), l).err().as_deref()),
        ];
        for o in [
            write::Outcome::AcceptedSaid("Accepted by the device: SMS to 10086".into()),
            write::Outcome::Unknown,
            write::Outcome::Busy,
        ] {
            all.push(area(l, "/x", &write::Phase::Done(o)));
        }
        let html = all.join("\n");
        assert!(
            !html.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)
                || ('\u{3000}'..='\u{303f}').contains(&c)
                || ('\u{ff00}'..='\u{ffef}').contains(&c)),
            "{html}"
        );
    }
}
