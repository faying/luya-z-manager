//! /router/sim (SIM / PIN): the card's state, its attempts left and
//! the five things that use those attempts up: verify PIN, verify PUK (sets a new PIN), PIN lock
//! on / off, change PIN, network unlock (NCK). Batch 5c.
//!
//! Where each value comes from (the old page read these once and again after a write; neither
//! poll nor push: the stream re-reads on load, on 「刷新」 and after every write):
//! - status block (SIM ready / locked by PIN / locked by PUK, the reason, attempts left in the meta
//!   line, the 「数字停在」 line when a re-read failed), 状态, 基带状态, PIN 锁, IMSI, ICCID,
//!   PIN / PUK attempts (each only when the card reports one): `GET /api/sim/info`
//!   (`sim_states`, `modem_main_state`, `pin_status` "1"/"0", `sim_imsi`, `sim_iccid`,
//!   `pinnumber`, `puknumber`);
//! - NCK attempts left (the row, and next to the unlock form): `GET /api/sim/lock-trials`
//!   (`available_trials`); when only this read fails the page says so and the rest stays.
//!
//! Every write is **tier 3 (dialog)**, as the old page had it. The inventory lists the verify /
//! mode / change / unlock rows as tier 2; that would weaken the old dialogs, and every one of them
//! can use up an attempt on the card (a wrong PIN → PUK lock, a wrong PUK → card dead for good,
//! a wrong NCK → no more tries), so the dialog stays. PUK and NCK are `danger`; switching the PIN
//! lock on says that a restart will then keep the SIM (and mobile data, and Tailscale) down until
//! somebody enters the PIN.
//!
//! ```text
//!  button (opens the form; which one is open is a page-local signal `_panel`)
//!   └─ Submit ─▶ POST ask?k=…  (signals {pin, oldpin, newpin, puk, nck} in the BODY)
//!                 ├ checked with the old words (PIN ≥ 4, PUK ≥ 8, new PIN ≥ 4, NCK required)
//!                 ├ the card read fresh: state fits the action, attempts left → the dialog quotes them
//!                 └ dialog ─▶ POST set?k=…  (same signals again; nothing secret in the URL)
//!                       ├ one SIM write at a time (a second press: 「另一项 SIM 改动正在进行」)
//!                       ├ card read fresh again (can't read it → nothing is sent), `before`
//!                       ├ POST /api/sim/pin/{verify,change,mode} | /api/sim/unlock  (sent once)
//!                       └ readback: sim_states / pin_status where there is one; else (and when
//!                         the readback does not show it) the attempts: fewer than `before` →
//!                         「没有生效：… · 还剩 N 次」; same → 「设备已接受」, never "成功"
//! ```
//! - The PIN, PUK and unlock code travel only in POST bodies (Datastar signals), are never part of
//!   a URL, a patched element, an error message or a log line. They are cleared from the form after
//!   every send (a wrong code is not resent by a second press of the same dialog), and the form
//!   closes when the device took the change.
//! - The agent forwards a write that may have reached u60d only once (`agent.rs`), so one press is
//!   at most one send. A reply that does not come is read back like any other; nothing newer →
//!   「连接中断，还没确认是否生效」.
//! - Stricter than the old page in one place: a PIN is 4 to 8 digits (3GPP), the old page only
//!   checked the minimum and would have sent 20 digits to the card.

use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use askama::Template;
use axum::Router;
use axum::extract::{Query, Request, State};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::topbar::Via;
use crate::agent::{Caller, Fail, Polled, esc};
use crate::i18n::Lang;
use crate::pages::{Shared, StreamQuery, patch, split_leaves};
use crate::write::{self, Confirm, Outcome, Phase, Sent, Tier};

const PATH: &str = "/router/sim";
/// The old page did not poll; a stream that only re-reads on a kick or a write.
const IDLE: Duration = Duration::from_secs(24 * 3600);
/// Until the first read works.
const RETRY: Duration = Duration::from_secs(10);
const PIN_STATES: [&str; 2] = ["wait pin", "modem_waitpin"];
const PUK_STATES: [&str; 2] = ["wait puk", "modem_waitpuk"];
/// One SIM write at a time, from any tab: a second press would be a second try on the card.
static WRITING: AtomicBool = AtomicBool::new(false);

struct Free;
impl Drop for Free {
    fn drop(&mut self) {
        WRITING.store(false, Ordering::SeqCst);
    }
}

pub fn routes() -> Router<Shared> {
    Router::new()
        .route(PATH, get(page))
        .route("/router/sim/events", get(events))
        .route("/router/sim/refresh", post(refresh))
        .route("/router/sim/ask", post(ask))
        .route("/router/sim/set", post(set))
        .route("/router/sim/cancel", post(cancel))
}

// ---- what the agent says ----

/// `GET /api/sim/info`, every field as text ("" = not there).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Info {
    pub states: String,
    pub modem: String,
    pub pin_status: String,
    pub imsi: String,
    pub iccid: String,
    pub pin_left: String,
    pub puk_left: String,
}

fn txt(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_owned(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

pub fn parse_info(v: &Value) -> Info {
    Info {
        states: txt(&v["sim_states"]),
        modem: txt(&v["modem_main_state"]),
        pin_status: txt(&v["pin_status"]),
        imsi: txt(&v["sim_imsi"]),
        iccid: txt(&v["sim_iccid"]),
        pin_left: txt(&v["pinnumber"]),
        puk_left: txt(&v["puknumber"]),
    }
}

fn count(s: &str) -> Option<u32> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok())?
}

impl Info {
    fn in_state(&self, states: &[&str]) -> bool {
        let (a, b) = (self.states.to_lowercase(), self.modem.to_lowercase());
        states.iter().any(|s| a == *s || b == *s)
    }
    pub fn pin_locked(&self) -> bool {
        self.in_state(&PIN_STATES)
    }
    pub fn puk_locked(&self) -> bool {
        self.in_state(&PUK_STATES)
    }
    pub fn pin_on(&self) -> bool {
        self.pin_status == "1"
    }
    pub fn pin_tries(&self) -> Option<u32> {
        count(&self.pin_left)
    }
    pub fn puk_tries(&self) -> Option<u32> {
        count(&self.puk_left)
    }
}

/// The card as read: the info, and the NCK attempts (`None`: not readable or not reported).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Card {
    pub info: Info,
    pub nck: Option<u32>,
}

async fn read_card(app: &Shared, who: &Caller) -> Result<Card, Fail> {
    let info = parse_info(&crate::agent::get(app, who, "/api/sim/info").await?);
    let nck = crate::agent::get(app, who, "/api/sim/lock-trials")
        .await
        .ok()
        .and_then(|v| count(&txt(&v["available_trials"])));
    Ok(Card { info, nck })
}

// ---- the five actions ----

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    VerifyPin,
    VerifyPuk,
    PinOn,
    PinOff,
    ChangePin,
    Nck,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "verify-pin" => Kind::VerifyPin,
            "verify-puk" => Kind::VerifyPuk,
            "pin-on" => Kind::PinOn,
            "pin-off" => Kind::PinOff,
            "change-pin" => Kind::ChangePin,
            "nck" => Kind::Nck,
            _ => return None,
        })
    }
    pub fn id(self) -> &'static str {
        match self {
            Kind::VerifyPin => "verify-pin",
            Kind::VerifyPuk => "verify-puk",
            Kind::PinOn => "pin-on",
            Kind::PinOff => "pin-off",
            Kind::ChangePin => "change-pin",
            Kind::Nck => "nck",
        }
    }
    /// Which attempts this action can use up.
    fn uses(self) -> Counter {
        match self {
            Kind::VerifyPuk => Counter::Puk,
            Kind::Nck => Counter::Nck,
            _ => Counter::Pin,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Counter {
    Pin,
    Puk,
    Nck,
}

fn tries(c: Counter, card: &Card) -> Option<u32> {
    match c {
        Counter::Pin => card.info.pin_tries(),
        Counter::Puk => card.info.puk_tries(),
        Counter::Nck => card.nck,
    }
}

/// The form's signals as Datastar posts them. No `Debug`: these are the secrets.
#[derive(Deserialize, Default)]
pub struct Form {
    #[serde(default)]
    pin: String,
    #[serde(default)]
    oldpin: String,
    #[serde(default)]
    newpin: String,
    #[serde(default)]
    puk: String,
    #[serde(default)]
    nck: String,
}

/// What a checked form will send. No `Debug`, no `Clone`.
pub struct Act {
    kind: Kind,
    pin: String,
    old: String,
    new: String,
    puk: String,
    nck: String,
}

fn digits(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_digit())
}

/// A PIN field: 4 to 8 digits.
fn pin_ok(s: &str, what_en: &'static str, what_zh: &'static str, l: Lang) -> Result<(), String> {
    let (en, zh) = (what_en, what_zh);
    if s.len() < 4 {
        return Err(match l {
            Lang::En => format!("{en}: use 4 to 8 digits"),
            Lang::Zh => format!("{zh}要填 4 到 8 位数字"),
        });
    }
    if !digits(s) {
        return Err(match l {
            Lang::En => format!("{en} can only contain digits"),
            Lang::Zh => format!("{zh}只能是数字"),
        });
    }
    if s.len() > 8 {
        return Err(match l {
            Lang::En => format!("{en} is at most 8 digits"),
            Lang::Zh => format!("{zh}最多 8 位数字"),
        });
    }
    Ok(())
}

/// The form for `kind`, with the old page's checks. Err = the sentence to show, nothing is sent.
pub fn check(kind: Kind, f: &Form, l: Lang) -> Result<Act, String> {
    let act = |pin: &str, old: &str, new: &str, puk: &str, nck: &str| Act {
        kind,
        pin: pin.into(),
        old: old.into(),
        new: new.into(),
        puk: puk.into(),
        nck: nck.into(),
    };
    match kind {
        Kind::VerifyPin | Kind::PinOn | Kind::PinOff => {
            pin_ok(&f.pin, "PIN", "PIN ", l)?;
            Ok(act(&f.pin, "", "", "", ""))
        }
        Kind::VerifyPuk => {
            let p = f.puk.as_str();
            if p.len() < 8 {
                return Err(l.pick("PUK: use 8 digits or more", "PUK 要填 8 位或更多数字").into());
            }
            if !digits(p) || p.len() > 20 {
                return Err(l.pick("PUK can only contain digits", "PUK 只能是数字").into());
            }
            pin_ok(&f.newpin, "New PIN", "新 PIN ", l)?;
            Ok(act("", "", &f.newpin, p, ""))
        }
        Kind::ChangePin => {
            pin_ok(&f.oldpin, "Current PIN", "当前 PIN ", l)?;
            pin_ok(&f.newpin, "New PIN", "新 PIN ", l)?;
            Ok(act("", &f.oldpin, &f.newpin, "", ""))
        }
        Kind::Nck => {
            let c = f.nck.trim();
            if c.is_empty() {
                return Err(l.pick("Unlock code is required", "需要解锁码").into());
            }
            if c.len() > 64 {
                return Err(l.pick("Unlock code is too long", "解锁码太长").into());
            }
            Ok(act("", "", "", "", c))
        }
    }
}

/// Does the card's state fit the action? Err = why not (a changed state is read again, not sent into).
pub fn fits(kind: Kind, i: &Info, l: Lang) -> Result<(), String> {
    let (pin, puk) = (i.pin_locked(), i.puk_locked());
    let ok = match kind {
        Kind::VerifyPin => pin && !puk,
        Kind::VerifyPuk => puk,
        Kind::PinOn => !pin && !puk && !i.pin_on(),
        Kind::PinOff => !pin && !puk && i.pin_on(),
        Kind::ChangePin => !pin && !puk,
        Kind::Nck => true,
    };
    if ok {
        Ok(())
    } else {
        Err(l
            .pick(
                "The SIM is in a different state than this page showed. Nothing was sent; the page is read again.",
                "SIM 卡的状态和页面显示的不一样。没有发送任何东西，页面正在重新读取。",
            )
            .into())
    }
}

/// The agent path and body of the action.
fn request(a: &Act) -> (&'static str, Value) {
    match a.kind {
        Kind::VerifyPin => (
            "/api/sim/pin/verify",
            json!({"pin_num": a.pin, "puk_num": "", "pin_encode_flag": "0"}),
        ),
        Kind::VerifyPuk => (
            "/api/sim/pin/verify",
            json!({"pin_num": a.new, "puk_num": a.puk, "pin_encode_flag": "0"}),
        ),
        Kind::PinOn | Kind::PinOff => (
            "/api/sim/pin/mode",
            json!({"pin_num_m": a.pin, "pin_mode": if a.kind == Kind::PinOn { "1" } else { "0" }, "pin_encode_flag": "0"}),
        ),
        Kind::ChangePin => (
            "/api/sim/pin/change",
            json!({"pin_num": a.old, "new_pin_num": a.new, "pin_encode_flag": "0"}),
        ),
        Kind::Nck => ("/api/sim/unlock", json!({"nck": a.nck})),
    }
}

/// What the readback looks for; none for a change of PIN and a network unlock (nothing to read).
fn held(kind: Kind, i: &Info) -> Option<bool> {
    match kind {
        Kind::VerifyPin => Some(!i.pin_locked()),
        Kind::VerifyPuk => Some(!i.puk_locked()),
        Kind::PinOn => Some(i.pin_on()),
        Kind::PinOff => Some(!i.pin_on()),
        Kind::ChangePin | Kind::Nck => None,
    }
}

// ---- words ----

fn left_words(c: Counter, n: u32, l: Lang) -> String {
    let what = match c {
        Counter::Pin => "PIN",
        Counter::Puk => "PUK",
        Counter::Nck => "",
    };
    match (c, l) {
        (Counter::Nck, Lang::En) => format!("{n} unlock attempt(s) left"),
        (Counter::Nck, Lang::Zh) => format!("还剩 {n} 次解锁尝试机会"),
        (_, Lang::En) => format!("{n} {what} attempt(s) left"),
        (_, Lang::Zh) => format!("还剩 {n} 次 {what} 尝试机会"),
    }
}

fn unknown_words(c: Counter, l: Lang) -> &'static str {
    match c {
        Counter::Pin => l.pick("The device did not report how many PIN attempts are left", "设备没有报告 PIN 还剩几次尝试机会"),
        Counter::Puk => l.pick("The device did not report how many PUK attempts are left", "设备没有报告 PUK 还剩几次尝试机会"),
        Counter::Nck => l.pick("The device did not report how many unlock attempts are left", "设备没有报告还剩几次解锁尝试机会"),
    }
}

/// After a failed or unconfirmed try: how many attempts are left, and what ran out.
fn after_words(kind: Kind, c: &Card, before: &Card, l: Lang) -> String {
    let counter = kind.uses();
    let mut out: Vec<String> = Vec::new();
    if counter == Counter::Pin && c.info.puk_locked() && !before.info.puk_locked() {
        out.push(
            l.pick(
                "PIN attempts used up: the SIM is now locked by PUK",
                "PIN 尝试次数用完了：SIM 现在被 PUK 锁定",
            )
            .into(),
        );
        if let Some(n) = c.info.puk_tries() {
            out.push(left_words(Counter::Puk, n, l));
        }
    } else {
        match tries(counter, c) {
            Some(0) if counter == Counter::Puk => out.push(
                l.pick(
                    "no PUK attempts left: the carrier has to replace the SIM",
                    "PUK 没有剩余尝试机会：要向运营商换卡",
                )
                .into(),
            ),
            Some(0) if counter == Counter::Nck => out.push(
                l.pick(
                    "no unlock attempts left",
                    "没有剩余的解锁尝试机会",
                )
                .into(),
            ),
            Some(n) => out.push(left_words(counter, n, l)),
            None => {}
        }
    }
    out.join(" · ")
}

fn wrong(kind: Kind, l: Lang) -> &'static str {
    match kind {
        Kind::VerifyPin | Kind::PinOn | Kind::PinOff => l.pick("the PIN was not accepted", "PIN 不对，卡没有接受"),
        Kind::ChangePin => l.pick("the current PIN was not accepted", "当前 PIN 不对，卡没有接受"),
        Kind::VerifyPuk => l.pick("the PUK was not accepted", "PUK 不对，卡没有接受"),
        Kind::Nck => l.pick("the unlock code was not accepted", "解锁码不对，卡没有接受"),
    }
}

fn applied(kind: Kind, l: Lang) -> &'static str {
    match kind {
        Kind::VerifyPin => l.pick("PIN accepted; the SIM is unlocked", "PIN 已通过，SIM 已解锁"),
        Kind::VerifyPuk => l.pick("PUK accepted; the new PIN is set and the SIM is unlocked", "PUK 已通过，新 PIN 已设置，SIM 已解锁"),
        Kind::PinOn => l.pick("PIN lock is on", "PIN 锁已开启"),
        Kind::PinOff => l.pick("PIN lock is off", "PIN 锁已关闭"),
        Kind::ChangePin | Kind::Nck => "",
    }
}

fn said(kind: Kind, l: Lang) -> &'static str {
    match kind {
        Kind::ChangePin => l.pick(
            "Accepted by the device: PIN change (a PIN can't be read back; no attempt was used)",
            "设备已接受：修改 PIN（PIN 读不回来；没有用掉尝试次数）",
        ),
        _ => l.pick(
            "Accepted by the device: network unlock (no attempt was used; the result can't be read back)",
            "设备已接受：网络解锁（没有用掉尝试次数；结果读不回来）",
        ),
    }
}

// ---- the confirm ----

pub fn confirm(kind: Kind, c: &Card, l: Lang) -> Confirm {
    let counter = kind.uses();
    let n = tries(counter, c);
    let mut left = match n {
        Some(n) => left_words(counter, n, l),
        None => unknown_words(counter, l).to_owned(),
    };
    left.push('.');
    if n == Some(1) {
        left.push(' ');
        left.push_str(l.pick("This is the last attempt.", "这是最后一次机会。"));
    }
    let with = |what: &str| format!("{what} {left}");
    let (title, what, recovery, action) = match kind {
        Kind::VerifyPin => (
            l.pick("Send this PIN to the SIM?", "把这个 PIN 发给 SIM 卡？").to_owned(),
            with(l.pick(
                "A wrong PIN uses up one attempt; when none are left the SIM locks and needs the PUK.",
                "输错 PIN 会用掉一次机会；用完后 SIM 被锁，要用 PUK 解锁。",
            )),
            Some(l.pick("If the PIN attempts run out, unlock with the PUK from your carrier.", "PIN 次数用完的话，用运营商给的 PUK 解锁。")),
            l.pick("Verify PIN", "验证 PIN"),
        ),
        Kind::VerifyPuk => (
            l.pick("Send this PUK and set a new PIN?", "发送这个 PUK 并设置新 PIN？").to_owned(),
            with(l.pick(
                "A wrong PUK uses up one attempt. When none are left the SIM is locked for good and has to be replaced by the carrier.",
                "输错 PUK 会用掉一次机会。用完后 SIM 永久锁死，要向运营商换卡。",
            )),
            Some(l.pick("Check the PUK with your carrier before trying again.", "再试之前先向运营商确认 PUK。")),
            l.pick("Verify PUK", "验证 PUK"),
        ),
        Kind::ChangePin => (
            l.pick("Change the SIM PIN?", "修改 SIM 的 PIN？").to_owned(),
            with(l.pick(
                "The SIM's PIN changes to the new one. A wrong current PIN uses up one attempt.",
                "SIM 的 PIN 改成新的。当前 PIN 输错会用掉一次机会。",
            )),
            Some(l.pick("Note the new PIN. If it's forgotten, the PUK resets it.", "记下新 PIN。忘了的话，用 PUK 重置。")),
            l.pick("Change PIN", "修改 PIN"),
        ),
        Kind::PinOn => (
            l.pick("Turn the PIN lock on?", "开启 PIN 锁？").to_owned(),
            with(l.pick(
                "From the next start-up the SIM stays locked until someone enters the PIN: no mobile data, and no remote access over Tailscale, until then. A wrong PIN uses up one attempt.",
                "下次开机起，SIM 一直锁着，直到有人输入 PIN：在此之前没有移动网络，也不能经 Tailscale 远程访问。PIN 输错会用掉一次机会。",
            )),
            Some(l.pick(
                "After a restart, join the U60's Wi-Fi and enter the PIN on this page, or turn the PIN lock off here first.",
                "重启后，连上设备的 Wi-Fi，在这个页面输入 PIN；或者先在这里把 PIN 锁关掉。",
            )),
            l.pick("Enable PIN Lock", "开启 PIN 锁"),
        ),
        Kind::PinOff => (
            l.pick("Turn the PIN lock off?", "关闭 PIN 锁？").to_owned(),
            with(l.pick(
                "The SIM no longer asks for a PIN at start-up. A wrong PIN uses up one attempt.",
                "SIM 开机时不再要 PIN。PIN 输错会用掉一次机会。",
            )),
            None,
            l.pick("Disable PIN Lock", "关闭 PIN 锁"),
        ),
        Kind::Nck => (
            l.pick("Send this network unlock code?", "发送这个网络解锁码？").to_owned(),
            with(l.pick(
                "This can't be undone. A wrong code uses up one attempt; when none are left the device can't be unlocked with a code any more.",
                "这一步撤不回。输错解锁码会用掉一次机会；用完后不能再用解锁码解锁。",
            )),
            Some(l.pick("Double-check the code with whoever issued it before sending.", "发送前先向发码的人核对解锁码。")),
            l.pick("Unlock SIM", "解锁 SIM"),
        ),
    };
    Confirm {
        tier: Tier::Dialog,
        title,
        what,
        downtime: None,
        recovery: recovery.map(Into::into),
        action: action.into(),
        cuts_uplink: kind == Kind::PinOn,
        word: None,
        danger: matches!(kind, Kind::VerifyPuk | Kind::Nck),
        revert: crate::write::Revert::No,
    }
}

// ---- the page's leaves ----

struct Poll {
    who: Caller,
    l: Lang,
    info: Option<Info>,
    info_fail: Option<Fail>,
    nck: Option<Option<u32>>,
    nck_raw: String,
    nck_fail: Option<Fail>,
    at: Option<SystemTime>,
}

fn mark(tone: &str, text: &str) -> String {
    let sym = match tone {
        "bad" => "■",
        "warn" => "▲",
        _ => "●",
    };
    format!(
        "<span class=\"nd-mark nd-mark--{tone}\"><span class=\"nd-mark__sym\" aria-hidden=\"true\">{sym}</span><span>{}</span></span>",
        esc(text)
    )
}

fn row(label: &str, value: &str, mono: bool) -> String {
    format!(
        "<div class=\"nd-row\"><span class=\"nd-row__text\"><span class=\"nd-row__label\">{}</span></span><span class=\"nd-row__value{}\">{value}</span></div>",
        esc(label),
        if mono { " nd-mono" } else { "" }
    )
}

impl Poll {
    fn new(who: Caller, l: Lang) -> Poll {
        Poll {
            who,
            l,
            info: None,
            info_fail: None,
            nck: None,
            nck_raw: String::new(),
            nck_fail: None,
            at: None,
        }
    }

    async fn fetch(&mut self, app: &Shared) {
        match crate::agent::get(app, &self.who, "/api/sim/info").await {
            Ok(v) => {
                self.info = Some(parse_info(&v));
                self.info_fail = None;
                self.at = Some(SystemTime::now());
            }
            Err(f) => self.info_fail = Some(f),
        }
        match crate::agent::get(app, &self.who, "/api/sim/lock-trials").await {
            Ok(v) => {
                self.nck_raw = txt(&v["available_trials"]);
                self.nck = Some(count(&self.nck_raw));
                self.nck_fail = None;
            }
            Err(f) => self.nck_fail = Some(f),
        }
    }

    fn stale(&self) -> bool {
        self.info.is_some() && self.info_fail.is_some()
    }

    fn card(&self) -> Option<Card> {
        self.info.clone().map(|info| Card {
            info,
            nck: self.nck.flatten(),
        })
    }

    fn leaves(&self) -> Vec<(String, String)> {
        let l = self.l;
        let stale = self.stale();
        let st_cls = if stale { " nd-stale" } else { "" };
        let i = self.info.as_ref();
        let mut out = String::new();

        // status block
        let (mut tone, state, reason): (&str, String, Option<String>) = match (i, &self.info_fail) {
            (None, Some(f)) => ("bad", l.pick("Can't read the SIM", "读不到 SIM 卡").into(), Some(f.text(l))),
            (None, None) => ("neutral", l.pick("Reading the SIM…", "正在读取 SIM 卡…").into(), None),
            (Some(i), _) if i.puk_locked() => (
                "bad",
                l.pick("SIM locked by PUK", "SIM 被 PUK 锁定").into(),
                Some(l.pick("The SIM wants its PUK. Enter it below.", "SIM 要 PUK 才能用，在下面输入。").into()),
            ),
            (Some(i), _) if i.pin_locked() => (
                "bad",
                l.pick("SIM locked by PIN", "SIM 被 PIN 锁定").into(),
                Some(l.pick("The SIM wants its PIN. Enter it below.", "SIM 要 PIN 才能用，在下面输入。").into()),
            ),
            (Some(i), _) => {
                let raw = i.states.to_lowercase();
                let ready = raw.contains("ready") || raw.contains("complete");
                (
                    if ready { "ok" } else { "neutral" },
                    if ready {
                        l.pick("SIM ready", "SIM 已就绪").into()
                    } else {
                        match l {
                            Lang::En => format!("SIM state: {}", if i.states.is_empty() { "—" } else { &i.states }),
                            Lang::Zh => format!("SIM 状态：{}", if i.states.is_empty() { "—" } else { &i.states }),
                        }
                    },
                    if i.pin_on() {
                        Some(l.pick("PIN lock is on: the PIN is asked for at every start-up.", "PIN 锁已开启：每次开机都要输入 PIN。").into())
                    } else if i.pin_status == "0" {
                        Some(l.pick("PIN lock is off.", "PIN 锁已关闭。").into())
                    } else {
                        None
                    },
                )
            }
        };
        let mut meta: Vec<String> = Vec::new();
        if stale {
            tone = "stale";
            meta.push(crate::agent::stopped(l, "Details", "详情", self.at));
            meta.push(l.pick("Refresh before changing anything.", "改任何东西之前先刷新。").into());
        } else if let Some(i) = i {
            if i.puk_locked() {
                if let Some(n) = i.puk_tries() {
                    meta.push(left_words(Counter::Puk, n, l));
                }
            } else if i.pin_locked()
                && let Some(n) = i.pin_tries()
            {
                meta.push(left_words(Counter::Pin, n, l));
            }
        }
        let retry = if i.is_none() || stale {
            format!(
                "<button type=\"button\" class=\"nd-btn nd-btn--secondary nd-btn--sm\" data-on:click=\"@post('{PATH}/refresh')\">{}</button>",
                l.pick("Retry", "重试")
            )
        } else {
            String::new()
        };
        out.push_str(&format!(
            "<div id=\"sim-status\"><section class=\"nd-status nd-status--{tone}{}\"><div class=\"nd-status__main\"><div class=\"nd-status__state\" role=\"status\">{}</div>{}{}</div><div class=\"nd-status__actions\">{retry}</div></section></div>\n<!--leaf-->\n",
            if stale { " nd-stale" } else { "" },
            mark(tone, &state),
            reason.map_or(String::new(), |r| format!(
                "<div class=\"nd-status__reason\"><span{}>{}</span></div>",
                if tone == "bad" { " role=\"alert\"" } else { "" },
                esc(&r)
            )),
            if meta.is_empty() {
                String::new()
            } else {
                format!("<div class=\"nd-status__meta\"><span class=\"nd-fresh\">{}</span></div>", esc(&meta.join(" · ")))
            },
        ));

        // details
        out.push_str(&format!("<div id=\"sim-info\" class=\"nd-group{st_cls}\">"));
        let skel = "<span class=\"nd-skel\" style=\"width:12ch\"></span>";
        let val = |s: &str| if s.is_empty() { "—".to_owned() } else { esc(s) };
        match i {
            None => {
                for label in [
                    l.pick("Status", "状态"),
                    l.pick("Modem", "基带状态"),
                    l.pick("PIN Lock", "PIN 锁"),
                    "IMSI",
                    "ICCID",
                ] {
                    out.push_str(&row(label, skel, false));
                }
            }
            Some(i) => {
                out.push_str(&row(l.pick("Status", "状态"), &val(&i.states), true));
                out.push_str(&row(l.pick("Modem", "基带状态"), &val(&i.modem), true));
                out.push_str(&row(
                    l.pick("PIN Lock", "PIN 锁"),
                    &match i.pin_status.as_str() {
                        "1" => l.pick("Enabled", "已启用").to_owned(),
                        "0" => l.pick("Disabled", "已禁用").to_owned(),
                        s => val(s),
                    },
                    false,
                ));
                out.push_str(&row("IMSI", &val(&i.imsi), true));
                out.push_str(&row("ICCID", &val(&i.iccid), true));
                if !i.pin_left.is_empty() {
                    out.push_str(&row(l.pick("PIN tries left", "PIN 还能试几次"), &esc(&i.pin_left), true));
                }
                if !i.puk_left.is_empty() {
                    out.push_str(&row(l.pick("PUK tries left", "PUK 还能试几次"), &esc(&i.puk_left), true));
                }
            }
        }
        if i.is_some() || self.nck_fail.is_some() || self.nck.is_some() {
            let v = match (&self.nck, &self.nck_fail) {
                (Some(_), _) => val(&self.nck_raw),
                (None, Some(_)) => l.pick("Can't read", "读不到").to_owned(),
                (None, None) => skel.to_owned(),
            };
            out.push_str(&row(l.pick("NCK unlock tries left", "NCK 解锁还能试几次"), &v, true));
        } else {
            out.push_str(&row(l.pick("NCK unlock tries left", "NCK 解锁还能试几次"), skel, true));
        }
        out.push_str("</div>\n<!--leaf-->\n");
        match (&self.nck_fail, i) {
            (Some(f), Some(_)) if self.nck.is_none() => out.push_str(&format!(
                "<p id=\"sim-trialserr\" class=\"nd-aux sim-note\">{}</p>\n<!--leaf-->\n",
                esc(&match l {
                    Lang::En => format!("NCK attempts couldn't be read: {}", f.text(l)),
                    Lang::Zh => format!("读不到 NCK 剩余次数：{}", f.text(l)),
                })
            )),
            _ => out.push_str("<p id=\"sim-trialserr\" hidden></p>\n<!--leaf-->\n"),
        }

        // the buttons
        let locked = i.is_none() || stale;
        let dis = if locked { " disabled" } else { "" };
        let open = |k: &str| {
            format!(
                "$_panel = ($_panel === '{k}' ? '' : '{k}'); $pin = ''; $oldpin = ''; $newpin = ''; $puk = ''; $nck = ''"
            )
        };
        let btn = |k: &str, icon: &str, secondary: bool, text: &str| {
            format!(
                "<button type=\"button\" class=\"nd-btn nd-btn--{}\"{dis} aria-expanded=\"false\" data-attr:aria-expanded=\"$_panel === '{k}' ? 'true' : 'false'\" data-on:click=\"{}\">{}{}</button>",
                if secondary { "secondary" } else { "primary" },
                open(k),
                crate::shell::icon(icon, false, 20, ""),
                esc(text)
            )
        };
        let mut b = String::new();
        if let Some(i) = i {
            if i.pin_locked() {
                b.push_str(&btn("verify-pin", "LockSimple", false, l.pick("Verify PIN", "验证 PIN")));
            }
            if i.puk_locked() {
                b.push_str(&btn("verify-puk", "LockSimple", false, l.pick("Verify PUK", "验证 PUK")));
            }
            if !i.pin_locked() && !i.puk_locked() {
                if i.pin_on() {
                    b.push_str(&btn("pin-off", "LockSimple", true, l.pick("Disable PIN Lock", "关闭 PIN 锁")));
                } else {
                    b.push_str(&btn("pin-on", "LockSimple", true, l.pick("Enable PIN Lock", "开启 PIN 锁")));
                }
                b.push_str(&btn("change-pin", "SimCard", true, l.pick("Change PIN", "修改 PIN")));
            }
        }
        b.push_str(&btn("nck", "Shield", true, l.pick("Network Unlock", "网络解锁")));
        out.push_str(&format!("<div id=\"sim-actions\" class=\"sim-acts{st_cls}\">{b}</div>\n<!--leaf-->\n"));

        // the attempts next to each form
        let card = self.card();
        for (k, c) in [
            ("verify-pin", Counter::Pin),
            ("pin-on", Counter::Pin),
            ("pin-off", Counter::Pin),
            ("change-pin", Counter::Pin),
            ("verify-puk", Counter::Puk),
            ("nck", Counter::Nck),
        ] {
            let n = card.as_ref().and_then(|c2| tries(c, c2));
            let (text, low) = match (n, card.is_some()) {
                (Some(n), _) => (left_words(c, n, l), n <= 1),
                (None, true) => (unknown_words(c, l).to_owned(), false),
                (None, false) => (String::new(), false),
            };
            out.push_str(&format!(
                "<p id=\"sim-t-{k}\" class=\"nd-aux sim-tries{}\" aria-live=\"polite\">{}</p>\n<!--leaf-->\n",
                if low { " sim-tries--low" } else { "" },
                esc(&text)
            ));
        }
        split_leaves(&out)
    }
}

impl Polled for Poll {
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        self.fetch(app).await;
        let next = if self.info.is_none() { RETRY } else { IDLE };
        (self.leaves(), next)
    }
}

// ---- the page ----

fn field(id: &str, sig: &str, label: &str, hint: &str, tries: &str) -> String {
    format!(
        "<div class=\"sim-field\"><label class=\"sim-label\" for=\"sim-f-{id}\">{}</label>\
<input id=\"sim-f-{id}\" type=\"password\" inputmode=\"numeric\" pattern=\"[0-9]*\" autocomplete=\"off\" maxlength=\"20\" class=\"nd-field nd-mono\" data-bind:{sig} data-on:input=\"${sig} = ${sig}.replace(/\\D/g, '')\" aria-describedby=\"sim-f-{id}-h {tries}\">\
<span id=\"sim-f-{id}-h\" class=\"nd-aux\">{}</span></div>",
        esc(label),
        esc(hint)
    )
}

/// The forms. Outside every pushed leaf, so a push never touches what is being typed.
fn panels(l: Lang, leaf: &dyn Fn(&str) -> String) -> String {
    let digits48 = l.pick("4-8 digits", "4-8 位数字");
    let clear = "$_panel = ''; $pin = ''; $oldpin = ''; $newpin = ''; $puk = ''; $nck = ''";
    let panel = |k: &str, title: &str, fields: String, submit: &str, danger: bool| {
        format!(
            "<section id=\"sim-p-{k}\" class=\"nd-group sim-panel\" style=\"display:none\" data-show=\"$_panel === '{k}'\" aria-labelledby=\"sim-p-{k}-t\">\
<h3 id=\"sim-p-{k}-t\">{}</h3>\
<form class=\"sim-form\" data-on:submit__prevent=\"@post('{PATH}/ask?k={k}')\">{fields}{}\
<div class=\"sim-btns\"><button type=\"submit\" class=\"nd-btn nd-btn--{}\">{}</button>\
<button type=\"button\" class=\"nd-btn nd-btn--secondary\" data-on:click=\"{clear}\">{}</button></div></form></section>",
            esc(title),
            leaf(&format!("sim-t-{k}")),
            if danger { "danger" } else { "primary" },
            esc(submit),
            l.pick("Cancel", "取消"),
        )
    };
    let tr = |k: &str| format!("sim-t-{k}");
    let mut s = String::new();
    s.push_str(&panel(
        "verify-pin",
        l.pick("Verify PIN", "验证 PIN"),
        field("vp", "pin", l.pick("PIN", "PIN"), digits48, &tr("verify-pin")),
        l.pick("Submit", "提交"),
        false,
    ));
    s.push_str(&panel(
        "verify-puk",
        l.pick("Verify PUK", "验证 PUK"),
        field("vk", "puk", l.pick("PUK", "PUK"), l.pick("8+ digits", "8 位以上数字"), &tr("verify-puk"))
            + &field("vkn", "newpin", l.pick("New PIN", "新 PIN"), digits48, &tr("verify-puk")),
        l.pick("Submit", "提交"),
        true,
    ));
    s.push_str(&panel(
        "pin-on",
        l.pick("Enable PIN Lock", "开启 PIN 锁"),
        field("on", "pin", l.pick("Confirm current PIN", "确认当前 PIN"), digits48, &tr("pin-on")),
        l.pick("Confirm", "确认"),
        false,
    ));
    s.push_str(&panel(
        "pin-off",
        l.pick("Disable PIN Lock", "关闭 PIN 锁"),
        field("off", "pin", l.pick("Confirm current PIN", "确认当前 PIN"), digits48, &tr("pin-off")),
        l.pick("Confirm", "确认"),
        false,
    ));
    s.push_str(&panel(
        "change-pin",
        l.pick("Change PIN", "修改 PIN"),
        field("co", "oldpin", l.pick("Current PIN", "当前 PIN"), digits48, &tr("change-pin"))
            + &field("cn", "newpin", l.pick("New PIN", "新 PIN"), digits48, &tr("change-pin")),
        l.pick("Change PIN", "修改 PIN"),
        false,
    ));
    let nck = format!(
        "<p><span class=\"nd-mark nd-mark--warn\"><span class=\"nd-mark__sym\" aria-hidden=\"true\">▲</span><span>{}</span></span></p>\
<div class=\"sim-field\"><label class=\"sim-label\" for=\"sim-f-nck\">{}</label>\
<input id=\"sim-f-nck\" type=\"password\" autocomplete=\"off\" autocapitalize=\"off\" spellcheck=\"false\" maxlength=\"64\" class=\"nd-field nd-mono\" data-bind:nck aria-describedby=\"sim-t-nck\" placeholder=\"{}\"></div>",
        l.pick(
            "There is no undo. Go on only with an unlock code you know is right.",
            "做了就退不回来。只有确定解锁码是对的才继续。"
        ),
        l.pick("Unlock code (NCK)", "解锁码 (NCK)"),
        l.pick("Unlock code", "解锁码"),
    );
    s.push_str(&panel(
        "nck",
        l.pick("Network Unlock (NCK)", "网络解锁 (NCK)"),
        nck,
        l.pick("Unlock SIM", "解锁 SIM"),
        true,
    ));
    s
}

#[derive(Template)]
#[template(path = "sim.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    leaves: Vec<(String, String)>,
    panels: String,
    op: String,
    /// The device has two slots (`sim.dual`): link to /router/dualsim.
    dual: bool,
}

impl Page {
    fn leaf(&self, id: &str) -> &str {
        self.leaves.iter().find(|(k, _)| k == id).map_or("", |(_, h)| h.as_str())
    }
}

fn leaf_of(leaves: &[(String, String)], id: &str) -> String {
    leaves.iter().find(|(k, _)| k == id).map_or(String::new(), |(_, h)| h.clone())
}

fn area(l: Lang, remote: Via, kind: Option<Kind>, phase: &Phase) -> String {
    let set = kind.map_or(String::new(), |k| format!("{PATH}/set?k={}", k.id()));
    write::Area {
        id: "sim",
        l,
        remote,
        set: &set,
        cancel: "/router/sim/cancel",
        phase,
    }
    .html()
}

async fn page(State(app): State<Shared>, req: Request) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let mut p = Poll::new(Caller::of(&app, &req), ctx.lang);
    p.fetch(&app).await;
    let leaves = p.leaves();
    let (shell, dual) = app.hub.read(|st| {
        (
            crate::shell::Shell::new(&headers, PATH).live(st, &ctx, Some(format!("{PATH}/events?g={}", st.generation))),
            crate::dualsim::dual(st).flatten().is_some(),
        )
    });
    let html = Page {
        shell,
        l: ctx.lang,
        panels: panels(ctx.lang, &|id| leaf_of(&leaves, id)),
        leaves,
        op: area(ctx.lang, ctx.remote, None, &Phase::Idle),
        dual,
    }
    .render()
    .unwrap();
    crate::assets::maybe_gzip(&headers, "text/html; charset=utf-8", html.into_bytes(), "no-store")
}

async fn events(State(app): State<Shared>, Query(q): Query<StreamQuery>, req: Request) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let p = Poll::new(Caller::of(&app, &req), ctx.lang);
    crate::agent::stream(app, ctx, q.g, PATH, p)
}

async fn refresh(State(app): State<Shared>) -> axum::http::StatusCode {
    let _ = app.kick.send(PATH);
    axum::http::StatusCode::NO_CONTENT
}

// ---- the writes ----

#[derive(Deserialize, Default)]
struct K {
    #[serde(default)]
    k: String,
}

fn one(html: String) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(futures_util::stream::once(async move { Ok(patch(&html)) }))
}

async fn split(app: &Shared, req: Request) -> (Caller, crate::topbar::Ctx, Form) {
    let who = Caller::of(app, &req);
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let body = axum::body::to_bytes(req.into_body(), 8192).await.unwrap_or_default();
    let form = serde_json::from_slice(&body).unwrap_or_default();
    (who, ctx, form)
}

fn cant_read(f: &Fail, l: Lang) -> Outcome {
    Outcome::NotYet(match l {
        Lang::En => format!("Can't read the SIM now, so nothing was sent: {}", f.text(l)),
        Lang::Zh => format!("现在读不到 SIM 卡，所以什么都没发送：{}", f.text(l)),
    })
}

fn bad_kind(l: Lang) -> Outcome {
    Outcome::NotYet(l.pick("Unknown action", "不认识的操作").into())
}

/// Submit: check the form, read the card, then the dialog (which quotes the attempts left).
async fn ask(State(app): State<Shared>, Query(q): Query<K>, req: Request) -> impl IntoResponse {
    let (who, ctx, form) = split(&app, req).await;
    let (l, r) = (ctx.lang, ctx.remote);
    let Some(kind) = Kind::parse(&q.k) else {
        return one(area(l, r, None, &Phase::Done(bad_kind(l))));
    };
    if let Err(e) = check(kind, &form, l) {
        return one(area(l, r, None, &Phase::Done(Outcome::NotYet(e))));
    }
    drop(form);
    if WRITING.load(Ordering::SeqCst) {
        return one(area(l, r, None, &Phase::Done(busy(l))));
    }
    let card = match read_card(&app, &who).await {
        Ok(c) => c,
        Err(f) => return one(area(l, r, None, &Phase::Done(cant_read(&f, l)))),
    };
    if let Err(e) = fits(kind, &card.info, l) {
        let _ = app.kick.send(PATH);
        return one(area(l, r, None, &Phase::Done(Outcome::NotYet(e))));
    }
    one(area(l, r, Some(kind), &Phase::Ask(confirm(kind, &card, l))))
}

fn busy(l: Lang) -> Outcome {
    Outcome::NotYet(
        l.pick(
            "Another SIM change is in progress; wait for it to finish",
            "另一项 SIM 改动正在进行，等它结束",
        )
        .into(),
    )
}

async fn cancel(Query(q): Query<write::CancelQuery>, State(app): State<Shared>, req: Request) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    if q.lost {
        let _ = app.kick.send(PATH);
    }
    one(area(ctx.lang, ctx.remote, None, &write::cancel_phase(&q)))
}

/// The dialog's button.
async fn set(State(app): State<Shared>, Query(q): Query<K>, req: Request) -> impl IntoResponse {
    let (who, ctx, form) = split(&app, req).await;
    let (l, r) = (ctx.lang, ctx.remote);
    let pending = area(l, r, None, &Phase::Pending(l.pick("Sending…", "提交中…").into()));
    write::respond(pending, async move {
        let done = |o: Outcome| area(l, r, None, &Phase::Done(o));
        let Some(kind) = Kind::parse(&q.k) else {
            return done(bad_kind(l));
        };
        let act = match check(kind, &form, l) {
            Ok(a) => a,
            Err(e) => return done(Outcome::NotYet(e)),
        };
        drop(form);
        if WRITING.swap(true, Ordering::SeqCst) {
            return done(busy(l));
        }
        let _free = Free;
        let (o, cleared) = run(&app, &who, l, act).await;
        let _ = app.kick.send(PATH);
        let close = matches!(o, Outcome::Applied(_) | Outcome::Accepted | Outcome::AcceptedSaid(_));
        let mut html = done(o);
        if cleared {
            html.push('\n');
            html.push_str(&reset(close));
        }
        html
    })
}

/// A hidden element whose `data-init` empties the form (and closes it when the device took the
/// change). The fresh id makes Datastar run it again each time.
fn reset(close: bool) -> String {
    format!(
        "<span id=\"sim-reset\" hidden data-init=\"$pin = ''; $oldpin = ''; $newpin = ''; $puk = ''; $nck = ''; {}$_x = '{}'\"></span>",
        if close { "$_panel = ''; " } else { "" },
        write::next_op_id()
    )
}

/// Read until `ok` holds, three tries two seconds apart (the card can lag a write by a moment).
async fn settle(app: &Shared, who: &Caller, kind: Kind) -> (Option<Card>, bool) {
    let mut last = None;
    for n in 0..3 {
        if n > 0 {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        if let Ok(c) = read_card(app, who).await {
            let hit = held(kind, &c.info) == Some(true);
            last = Some(c);
            if hit {
                return (last, true);
            }
        }
    }
    (last, false)
}

/// Fewer attempts than before (or the PIN attempts ran out): the code was wrong.
fn dropped(kind: Kind, before: &Card, after: &Card) -> bool {
    matches!((tries(kind.uses(), before), tries(kind.uses(), after)), (Some(b), Some(n)) if n < b)
        || (after.info.puk_locked() && !before.info.puk_locked())
}

/// The send and what came of it. The bool: a send was made (so the form is emptied).
async fn run(app: &Shared, who: &Caller, l: Lang, act: Act) -> (Outcome, bool) {
    let kind = act.kind;
    let before = match read_card(app, who).await {
        Ok(c) => c,
        Err(f) => return (cant_read(&f, l), false),
    };
    if let Err(e) = fits(kind, &before.info, l) {
        return (Outcome::NotYet(e), false);
    }
    let (path, body) = request(&act);
    drop(act);
    let sent = crate::agent::post(app, who, path, &body).await;
    drop(body);
    let how = match &sent {
        Ok(_) => Sent::Ok,
        Err(Fail::Unreachable) => Sent::NoReply,
        Err(f) => {
            // refused (or signed out): it was not taken; say how many attempts are left now. A
            // count that went down says the code was wrong, in plain words; the firmware's own
            // ("Unknown error") only when it did not.
            let after = read_card(app, who).await.ok();
            let spent = after.as_ref().is_some_and(|a| dropped(kind, &before, a));
            let left = after
                .as_ref()
                .map(|c| after_words(kind, c, &before, l))
                .filter(|s| !s.is_empty());
            let head = if spent {
                wrong(kind, l).to_owned()
            } else {
                f.text(l)
            };
            return (
                Outcome::Failed(match left {
                    Some(s) => format!("{head} \u{b7} {s}"),
                    None => head,
                }),
                true,
            );
        }
    };
    let (after, hit) = if held(kind, &before.info).is_some() {
        settle(app, who, kind).await
    } else {
        (read_card(app, who).await.ok(), false)
    };
    if hit {
        return (Outcome::Applied(applied(kind, l).into()), true);
    }
    let fewer = after.as_ref().is_some_and(|a| dropped(kind, &before, a));
    if fewer && let Some(a) = &after {
        let w = wrong(kind, l);
        let s = after_words(kind, a, &before, l);
        let e = if s.is_empty() { w.to_owned() } else { format!("{w} · {s}") };
        return (Outcome::Failed(e), true);
    }
    let o = if held(kind, &before.info).is_none() && how == Sent::Ok {
        Outcome::AcceptedSaid(said(kind, l).into())
    } else {
        how.not_read()
    };
    (o, true)
}

#[cfg(test)]
mod tests;
