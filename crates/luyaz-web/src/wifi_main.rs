//! /router/wifi (Wi-Fi 设置), the old `router/wifi/page.tsx` + `Extras.tsx`: the status (on,
//! broadcasting, per band channel · width · devices), the guest summary row, the master switch,
//! Wi-Fi 6, region, per band radio / SSID / password / encryption / channel / bandwidth / transmit
//! power / hidden, and the power save and NFC switches. Batch 4 (u60-web.md).
//!
//! Where each value comes from:
//! - u60d's `wifi` block: `on`, `wifi6`, `country`, `band_2g` / `band_5g` {ap_on, radio_on, ssid,
//!   encryption, has_password, hidden, channel, txpower, htmode} and `guest{ssid, on_2g, on_5g}` (the
//!   guest row). Passwords never come back (user decision): the form's password fields are
//!   write-only (empty = keep), the page says only 「已设密码 / 没有密码」.
//! - u60d's `clients.wifi`: the device count in the status.
//! - The agent's `GET /api/wifi/status` with the tab's session (batch 1 `agent.rs`), only for what
//!   u60d has no field for: the channel and width the radios really use (`actual_channel_*`,
//!   `actual_bw_*`, from iw) and the devices per band (`clients_*`). Without them the page can't
//!   say "switched on but not broadcasting". The reply also carries both passwords in plain text:
//!   [`Live::of`] keeps only those six fields, nothing else is stored or rendered.
//! - Power save: the agent's `GET /api/wifi/power-save` (u60d has no block for it); NFC: u60d's
//!   `nfc` block (`switch`).
//!
//! Writes, only through u60d `/control`:
//! - `wifi.settings` with the agent's PUT /api/wifi/settings keys, **only the keys the user
//!   changed** (the form posts its draft and the seed it was opened with; the diff is taken against
//!   that seed, so an untouched field is never rewritten). u60d runs it as a transaction on item
//!   `wifi.main` (reply `op`): followed in the `op` block (follow.rs, up to u60d's 90 s); `note: "no
//!   changes"` = nothing to do; no `op` (older u60d) = read back from the `wifi` block. Busy (409 with
//!   `doing`, or 503 for the Wi-Fi lock) and refusals are shown, not hidden.
//! - Tiers: anything that reloads Wi-Fi is tier 3 (dialog, also on the LAN: devices on this Wi-Fi
//!   drop for ~10–60 s; Tailscale goes over the mobile link and is not affected, so no Tailscale
//!   warning); transmit power alone is applied hot by u60d: tier 2.
//! - The Wi-Fi 6 switch stays, but a change is refused before sending (the firmware changes it
//!   together with the radio mode; u60d refuses it too).
//! - Power save `wifi.power_save {enabled}` (read back from the agent) and NFC `nfc.set {enabled,
//!   flag: 2}` (read back from the `nfc` block): tier 1, as the old page.
//! - A browser on the U60's own Wi-Fi loses the page during the reload: the area's lost-reply post
//!   (`cancel?op=…&lost=true`, retried by Datastar until the link is back) follows the same op_id
//!   and shows u60d's result; the status also shows the last `wifi.main` result from the `op` block.
//!
//! The form comes as a patch when 「更改设置」 is pressed (not in the first load). Its fields are
//! page-local signals (`_f…` draft, `_g…` seed); 「应用」 posts only those to the ask, which checks
//! them and keeps the patch server-side under a random token for the confirm: a password is in that
//! one POST body and the `/control` body, never in a URL, the dialog's HTML or any push.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use askama::Template;
use axum::Router;
use axum::extract::{Query, State};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::topbar::Via;
use crate::agent::{Caller, Polled};
use crate::hub::{Link, St};
use crate::i18n::Lang;
use crate::pages::{Shared, StreamQuery, async_stream, patch, split_leaves};
use crate::write;

pub const PATH: &str = "/router/wifi";
/// u60d's item for these settings (the guest network is `wifi.guest`, another page).
const ITEM: &str = "wifi.main";
const ACTION: &str = "wifi.settings";
/// u60d confirms a Wi-Fi transaction within 90 s.
const FOLLOW: Duration = Duration::from_secs(100);
/// The submit waits up to 5 s for the Wi-Fi lock, then writes uci.
const SEND_TIMEOUT: Duration = Duration::from_secs(30);
const AGENT_POLL: Duration = Duration::from_secs(30);
const STASH_TTL: Duration = Duration::from_secs(600);

pub const BANDS: [&str; 2] = ["2.4 GHz", "5 GHz"];
const SUFFIX: [&str; 2] = ["2g", "5g"];
pub const CHANNELS_2G: [&str; 14] = [
    "auto", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13",
];
pub const CHANNELS_5G: [&str; 22] = [
    "auto", "36", "40", "44", "48", "52", "56", "60", "64", "100", "104", "108", "112", "116",
    "132", "136", "140", "149", "153", "157", "161", "165",
];
pub const HTMODES: [&str; 5] = ["auto", "HT20", "HT40", "VHT80", "VHT160"];
pub const TXPOWERS: [&str; 3] = ["25", "50", "100"];
pub const ENCS: [&str; 4] = ["psk2+ccmp", "psk3+ccmp", "psk2+psk3+ccmp", "none"];
pub const REGIONS: [&str; 11] = [
    "CN", "US", "HK", "TW", "JP", "KR", "SG", "AU", "GB", "DE", "CA",
];
/// Characters the agent / u60d drop from a uci value without a word (`sanitize_uci_value`).
const STRIPPED: &[char] = &['\'', '"', ';', '$', '`', '\\', '|', '<', '>', '&'];

/// This page's routes (the page, its stream, its buttons).
pub fn routes() -> Router<Shared> {
    Router::new()
        .route(PATH, get(page))
        .route("/router/wifi/events", get(events))
        .route("/router/wifi/receipt", post(crate::follow::receipt))
        .route("/router/wifi/refresh", post(refresh))
        .route("/router/wifi/form", post(form_open))
        .route("/router/wifi/close", post(form_close))
        .route("/router/wifi/main/ask", post(main_ask))
        .route("/router/wifi/main/set", post(main_set))
        .route("/router/wifi/main/cancel", post(main_cancel))
        .route("/router/wifi/psm/set", post(psm_set))
        .route("/router/wifi/psm/cancel", post(psm_cancel))
        .route("/router/wifi/nfc/set", post(nfc_set))
        .route("/router/wifi/nfc/cancel", post(nfc_cancel))
}

// ---- what u60d says ----

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Band {
    pub ap_on: bool,
    pub radio_on: bool,
    pub ssid: String,
    pub encryption: String,
    pub has_password: bool,
    pub hidden: bool,
    pub channel: String,
    pub txpower: String,
    pub htmode: String,
}

impl Band {
    fn of(v: &Value) -> Option<Band> {
        v.is_object().then(|| Band {
            ap_on: v["ap_on"].as_bool().unwrap_or(true),
            radio_on: v["radio_on"].as_bool().unwrap_or(true),
            ssid: text(&v["ssid"]),
            encryption: text(&v["encryption"]),
            has_password: v["has_password"].as_bool().unwrap_or(false),
            hidden: v["hidden"].as_bool().unwrap_or(false),
            channel: text(&v["channel"]),
            txpower: text(&v["txpower"]),
            htmode: text(&v["htmode"]),
        })
    }
    /// The AP and its radio are both on.
    pub fn up(&self) -> bool {
        self.ap_on && self.radio_on
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Guest {
    pub ssid: String,
    pub on: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Wifi {
    pub on: bool,
    pub wifi6: bool,
    pub country: String,
    pub bands: [Band; 2],
    pub guest: Option<Guest>,
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_owned(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// The `wifi` block's data and whether it is current.
fn block(st: &St) -> Option<(&Value, bool)> {
    st.blocks
        .get("wifi")
        .map(|b| (&b.data, b.stale || st.link != Link::Live))
}

pub fn wifi_of(d: &Value) -> Option<Wifi> {
    let b2 = Band::of(&d["band_2g"])?;
    let b5 = Band::of(&d["band_5g"])?;
    let g = &d["guest"];
    Some(Wifi {
        on: d["on"].as_bool().unwrap_or(true),
        wifi6: d["wifi6"].as_bool().unwrap_or(true),
        country: text(&d["country"]).to_ascii_uppercase(),
        bands: [b2, b5],
        guest: g.is_object().then(|| Guest {
            ssid: text(&g["ssid"]),
            on: g["on_2g"].as_bool() == Some(true) || g["on_5g"].as_bool() == Some(true),
        }),
    })
}

pub fn wifi(st: &St) -> Option<Wifi> {
    wifi_of(block(st)?.0)
}

/// Readable and current (a change needs both), and u60d's executor is not stuck.
fn fresh(st: &St) -> bool {
    block(st).is_some_and(|(_, s)| !s) && !st.stuck
}

/// Devices on Wi-Fi (u60d `clients.wifi`).
fn on_wifi(st: &St) -> Option<i64> {
    st.blocks.get("clients")?.data["wifi"].as_i64()
}

/// NFC switch from u60d's `nfc` block: None = no block (no NFC, or not read), Some(None) = unknown.
fn nfc(st: &St) -> Option<Option<bool>> {
    let b = st.blocks.get("nfc")?;
    Some(b.data["switch"].as_i64().map(|n| n != 0))
}

// ---- what the agent says (tab's session) ----

/// The live radio state from the agent's `/api/wifi/status`: only these fields are kept (the reply
/// also has both passwords in plain text).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Live {
    pub ch: [String; 2],
    pub bw: [String; 2],
    pub n: [Option<i64>; 2],
}

impl Live {
    pub fn of(v: &Value) -> Live {
        let n = |k: &str| match &v[k] {
            Value::Number(x) => x.as_i64(),
            Value::String(s) => s.trim().parse().ok(),
            _ => None,
        };
        Live {
            ch: [text(&v["actual_channel_2g"]), text(&v["actual_channel_5g"])],
            bw: [text(&v["actual_bw_2g"]), text(&v["actual_bw_5g"])],
            n: [n("clients_2g"), n("clients_5g")],
        }
    }
}

/// Power save from the agent's `/api/wifi/power-save`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Psm {
    /// Live (iw) when Wi-Fi is up, else the saved choice; None = unknown.
    pub enabled: Option<bool>,
    pub live: Option<bool>,
}

impl Psm {
    pub fn of(v: &Value) -> Psm {
        Psm {
            enabled: v["enabled"].as_bool(),
            live: v["live"].as_bool(),
        }
    }
}

// ---- the form: seed, draft, diff ----

/// One band's comparable fields (the password is not one: write-only).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BandF {
    pub radio: bool,
    pub ssid: String,
    pub enc: String,
    pub ch: String,
    pub ht: String,
    pub tx: String,
    pub hidden: bool,
}

/// The form's comparable fields, as a seed (from u60d) or a draft (posted).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Fields {
    pub on: bool,
    pub w6: bool,
    pub cc: String,
    pub b: [BandF; 2],
}

/// Channel "0" / "" means automatic, like "auto".
fn norm_ch(s: &str) -> String {
    if s.is_empty() || s == "0" || s.eq_ignore_ascii_case("auto") {
        "auto".into()
    } else {
        s.to_owned()
    }
}

fn norm_ht(s: &str) -> String {
    if s.is_empty() || s.eq_ignore_ascii_case("auto") {
        "auto".into()
    } else {
        s.to_owned()
    }
}

impl Fields {
    pub fn seed(w: &Wifi) -> Fields {
        let band = |b: &Band| BandF {
            radio: b.radio_on,
            ssid: b.ssid.clone(),
            enc: b.encryption.clone(),
            ch: norm_ch(&b.channel),
            ht: norm_ht(&b.htmode),
            tx: b.txpower.clone(),
            hidden: b.hidden,
        };
        Fields {
            on: w.on,
            w6: w.wifi6,
            cc: w.country.clone(),
            b: [band(&w.bands[0]), band(&w.bands[1])],
        }
    }

    /// As page-local signals (`_{p}on`, `_{p}s2`…).
    pub fn signals(&self, p: char, out: &mut Map<String, Value>) {
        out.insert(format!("_{p}on"), json!(self.on));
        out.insert(format!("_{p}w6"), json!(self.w6));
        out.insert(format!("_{p}cc"), json!(self.cc));
        for (i, s) in ["2", "5"].iter().enumerate() {
            let b = &self.b[i];
            out.insert(format!("_{p}r{s}"), json!(b.radio));
            out.insert(format!("_{p}s{s}"), json!(b.ssid));
            out.insert(format!("_{p}e{s}"), json!(b.enc));
            out.insert(format!("_{p}c{s}"), json!(b.ch));
            out.insert(format!("_{p}h{s}"), json!(b.ht));
            out.insert(format!("_{p}t{s}"), json!(b.tx));
            out.insert(format!("_{p}x{s}"), json!(b.hidden));
        }
    }

    /// From posted signals; None when one is missing or of the wrong type.
    pub fn from_signals(v: &Value, p: char) -> Option<Fields> {
        let s = |k: &str| v[format!("_{p}{k}")].as_str().map(str::to_owned);
        let b = |k: &str| v[format!("_{p}{k}")].as_bool();
        let band = |x: &str| -> Option<BandF> {
            Some(BandF {
                radio: b(&format!("r{x}"))?,
                ssid: s(&format!("s{x}"))?,
                enc: s(&format!("e{x}"))?,
                ch: norm_ch(&s(&format!("c{x}"))?),
                ht: norm_ht(&s(&format!("h{x}"))?),
                tx: s(&format!("t{x}"))?,
                hidden: b(&format!("x{x}"))?,
            })
        };
        Some(Fields {
            on: b("on")?,
            w6: b("w6")?,
            cc: s("cc")?,
            b: [band("2")?, band("5")?],
        })
    }
}

/// A checked change: the `/control` params (the agent's keys, only what changed) and what it does.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub params: Map<String, Value>,
    /// The draft (for the confirm's words and the readback).
    pub draft: Fields,
    /// Which bands get a new password.
    pub keys: [bool; 2],
}

fn flag(b: bool) -> Value {
    json!(if b { "1" } else { "0" })
}

impl Change {
    /// Only the transmit power changes: u60d applies it hot, Wi-Fi does not reload.
    pub fn txpower_only(&self) -> bool {
        self.params.keys().all(|k| k.starts_with("txpower_"))
    }
    pub fn master_off(&self) -> bool {
        self.params.get("wifi_onoff") == Some(&json!("0"))
    }
    /// Devices must join again by hand: a new name, password or security, or a band switched off.
    pub fn rejoin(&self) -> bool {
        self.params.keys().any(|k| {
            k.starts_with("ssid_") || k.starts_with("key_") || k.starts_with("encryption_")
        }) || self.params.get("radio2_disabled") == Some(&json!("1"))
            || self.params.get("radio5_disabled") == Some(&json!("1"))
    }
    fn has(&self, k: &str) -> bool {
        self.params.contains_key(k)
    }
}

fn ssid_problem(s: &str, l: Lang) -> Option<&'static str> {
    if s.is_empty() {
        return Some(l.pick("enter a network name", "请填写网络名称"));
    }
    if s.len() > 32 {
        return Some(l.pick(
            "the network name is longer than 32 bytes",
            "网络名称超过 32 字节",
        ));
    }
    if s.contains(STRIPPED) || s.chars().any(char::is_control) {
        return Some(l.pick(
            "the network name can't contain ' \" ; $ ` \\ | < > & (the device would drop them)",
            "网络名称不能有 ' \" ; $ ` \\ | < > &（设备会把它们悄悄去掉）",
        ));
    }
    None
}

/// WPA passphrase: 8–63 printable ASCII, or 64 hex digits (a 64-character passphrase that is not
/// hex keeps hostapd from starting); none of the characters the device would drop.
fn key_problem(k: &str, l: Lang) -> Option<&'static str> {
    if k.len() == 64 && k.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    if !k.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        return Some(l.pick(
            "the password can only use letters, digits and ASCII symbols",
            "密码只能用字母、数字和 ASCII 符号",
        ));
    }
    if k.len() < 8 {
        return Some(l.pick(
            "the password needs at least 8 characters",
            "密码至少 8 个字符",
        ));
    }
    if k.len() > 64 {
        return Some(l.pick(
            "the password can have at most 64 characters",
            "密码最多 64 个字符",
        ));
    }
    if k.len() == 64 {
        return Some(l.pick(
            "a 64-character password must be 64 hex digits; use at most 63 characters",
            "64 个字符的密码必须全是十六进制数字；普通密码最多 63 个字符",
        ));
    }
    if k.contains(STRIPPED) {
        return Some(l.pick(
            "the password can't contain ' \" ; $ ` \\ | < > & (the device would drop them)",
            "密码不能有 ' \" ; $ ` \\ | < > &（设备会把它们悄悄去掉）",
        ));
    }
    None
}

fn not_read(l: Lang) -> write::Outcome {
    write::Outcome::NotYet(
        l.pick(
            "Wi-Fi settings not read yet (or not current); try again shortly",
            "还没读到最新的 Wi-Fi 设置，稍后再试",
        )
        .into(),
    )
}

pub fn wifi6_refused(l: Lang) -> String {
    l.pick(
        "Wi-Fi 6 can't be switched here: this firmware changes it together with the radio mode. Switch it back and apply again.",
        "Wi-Fi 6 在这里改不了：这个固件把它和无线模式一起改。把开关拨回去再应用。",
    )
    .into()
}

/// The draft against the seed it was opened with: the `/control` params, or why not.
/// `keys`: the passwords typed (empty = keep). `cur`: u60d now (None = not read / not current).
pub fn check(
    seed: &Fields,
    draft: &Fields,
    keys: [&str; 2],
    cur: Option<&Wifi>,
    l: Lang,
) -> Result<Change, write::Outcome> {
    let Some(cur) = cur else {
        return Err(not_read(l));
    };
    if draft.w6 != seed.w6 {
        return Err(write::Outcome::NotYet(wifi6_refused(l)));
    }
    let mut p = Map::new();
    let mut errs = Vec::new();
    let mut new_key = [false; 2];
    if draft.on != seed.on {
        p.insert("wifi_onoff".into(), flag(draft.on));
    }
    if draft.cc != seed.cc {
        let cc = draft.cc.trim().to_ascii_uppercase();
        if cc.len() != 2 || !cc.bytes().all(|b| b.is_ascii_alphabetic()) {
            errs.push(l.pick("Region: not a country code", "区域：不是国家代码").to_string());
        }
        p.insert("country".into(), json!(cc));
    }
    for i in 0..2 {
        let (s, d, b) = (&seed.b[i], &draft.b[i], BANDS[i]);
        let x = SUFFIX[i];
        let err = |e: &str| format!("{b}{}{e}", l.pick(": ", "："));
        if d.radio != s.radio {
            p.insert(
                format!("radio{}_disabled", &x[..1]),
                flag(!d.radio),
            );
        }
        if d.ssid != s.ssid {
            if let Some(e) = ssid_problem(&d.ssid, l) {
                errs.push(err(e));
            }
            p.insert(format!("ssid_{x}"), json!(d.ssid));
        }
        let open = d.enc == "none";
        if d.enc != s.enc {
            if !ENCS.contains(&d.enc.as_str()) {
                errs.push(err(l.pick("unknown security", "不认识的加密方式")));
            }
            p.insert(format!("encryption_{x}"), json!(d.enc));
        }
        let k = keys[i];
        if !k.is_empty() && !open {
            match key_problem(k, l) {
                Some(e) => errs.push(err(e)),
                None => {
                    p.insert(format!("key_{x}"), json!(k));
                    new_key[i] = true;
                }
            }
        } else if !open && (s.enc == "none" || !cur.bands[i].has_password) && d.enc != s.enc {
            errs.push(err(l.pick(
                "this security needs a password; type one",
                "这种加密方式要有密码，请填一个",
            )));
        }
        if d.ch != s.ch {
            let ok = if i == 0 {
                CHANNELS_2G.contains(&d.ch.as_str())
            } else {
                CHANNELS_5G.contains(&d.ch.as_str())
            };
            if !ok {
                errs.push(err(l.pick("not a channel of this band", "不是这个频段的信道")));
            }
            p.insert(format!("channel_{x}"), json!(d.ch));
        }
        if d.ht != s.ht {
            if !HTMODES.contains(&d.ht.as_str()) {
                errs.push(err(l.pick("unknown bandwidth", "不认识的带宽")));
            }
            p.insert(format!("htmode_{x}"), json!(d.ht));
        }
        if d.tx != s.tx {
            if !TXPOWERS.contains(&d.tx.as_str()) {
                errs.push(err(l.pick("unknown transmit power", "不认识的发射功率")));
            }
            p.insert(format!("txpower_{x}"), json!(d.tx));
        }
        if d.hidden != s.hidden {
            p.insert(format!("hidden_{x}"), flag(d.hidden));
        }
    }
    // US (FCC) has no 2.4 GHz channel 12 / 13
    if draft.cc.eq_ignore_ascii_case("US")
        && matches!(draft.b[0].ch.as_str(), "12" | "13")
        && (p.contains_key("country") || p.contains_key("channel_2g"))
    {
        errs.push(l.pick(
            "2.4 GHz: channels 12 and 13 are not allowed in the US; pick another or Auto",
            "2.4 GHz：美国不允许 12、13 信道；换一个或选自动",
        ).into());
    }
    // 2.4 GHz off with 5 GHz still up: the firmware runs no Wi-Fi at all (luyaz_wifi::layout; the
    // core refuses it too — said here before the dialog, in the page's language)
    let up = |i: usize| draft.b[i].radio && cur.bands[i].ap_on;
    if draft.on && (draft.b[0].radio != seed.b[0].radio || draft.b[1].radio != seed.b[1].radio) && up(1) && !up(0) {
        errs.push(l.pick(LAYOUT_WORDS[0].1, LAYOUT_WORDS[0].2).trim_start_matches("Not applied: ").trim_start_matches("没有生效：").to_owned());
    }
    if !errs.is_empty() {
        return Err(write::Outcome::NotYet(errs.join(l.pick("; ", "；"))));
    }
    if p.is_empty() {
        return Err(write::Outcome::NotYet(
            l.pick(
                "No changes yet. Change a setting, then apply.",
                "还没有改动。先改设置，再应用。",
            )
            .into(),
        ));
    }
    Ok(Change {
        params: p,
        draft: draft.clone(),
        keys: new_key,
    })
}

/// The readback when no transaction was seen: every sent value shows in the `wifi` block (the
/// passwords only as "has one").
pub fn holds(c: &Change, w: &Wifi) -> bool {
    c.params.iter().all(|(k, v)| {
        let v = v.as_str().unwrap_or_default();
        let band = |x: &str| if x.ends_with("2g") || x == "radio2_disabled" { 0 } else { 1 };
        let b = &w.bands[band(k)];
        match k.split('_').next().unwrap_or_default() {
            "wifi" => (v != "0") == w.on,
            "country" => w.country.eq_ignore_ascii_case(v),
            "radio2" | "radio5" => b.radio_on == (v == "0"),
            "ssid" => b.ssid == v,
            "key" => b.has_password,
            "encryption" => b.encryption == v,
            "channel" => norm_ch(&b.channel) == norm_ch(v),
            "htmode" => norm_ht(&b.htmode) == norm_ht(v),
            "txpower" => b.txpower == v,
            "hidden" => b.hidden == (v == "1"),
            _ => true,
        }
    })
}

// ---- words ----

pub fn enc_label(l: Lang, e: &str) -> String {
    match e {
        "psk2+ccmp" | "psk2" => "WPA2".into(),
        "psk3+ccmp" | "sae" | "psk3" => "WPA3".into(),
        "psk2+psk3+ccmp" | "psk-mixed+ccmp" => l.pick("WPA2/WPA3 mixed", "WPA2/WPA3 混合").into(),
        "none" => l.pick("Open (no password)", "开放（无密码）").into(),
        "" => "—".into(),
        o => o.into(),
    }
}

pub fn ch_label(l: Lang, c: &str) -> String {
    match norm_ch(c).as_str() {
        "auto" if c.is_empty() => "—".into(),
        "auto" => l.pick("Auto", "自动").into(),
        o => o.into(),
    }
}

pub fn ht_label(l: Lang, h: &str) -> String {
    if h.is_empty() {
        return "—".into();
    }
    match norm_ht(h).as_str() {
        "auto" => l.pick("Auto", "自动").into(),
        o => o.into(),
    }
}

pub fn tx_label(l: Lang, t: &str) -> String {
    match t {
        "25" => l.pick("Low", "低").into(),
        "50" => l.pick("Medium", "中").into(),
        "100" => l.pick("High", "高").into(),
        "" => "—".into(),
        o => format!("{o}%"),
    }
}

pub fn region_label(l: Lang, c: &str) -> String {
    let name = match c {
        "CN" => l.pick("China", "中国"),
        "US" => l.pick("United States", "美国"),
        "HK" => l.pick("Hong Kong", "香港"),
        "TW" => l.pick("Taiwan", "台湾"),
        "JP" => l.pick("Japan", "日本"),
        "KR" => l.pick("Korea", "韩国"),
        "SG" => l.pick("Singapore", "新加坡"),
        "AU" => l.pick("Australia", "澳大利亚"),
        "GB" => l.pick("United Kingdom", "英国"),
        "DE" => l.pick("Germany", "德国"),
        "CA" => l.pick("Canada", "加拿大"),
        "" => return "—".into(),
        _ => return c.into(),
    };
    format!("{name} ({c})")
}

fn quoted(l: Lang, s: &str) -> String {
    match l {
        Lang::En => format!("“{s}”"),
        Lang::Zh => format!("「{s}」"),
    }
}

/// The networks to join after the change: 「X」, or 「X」(2.4 GHz) 和「Y」(5 GHz).
fn names(c: &Change, l: Lang) -> String {
    let d = &c.draft;
    let on: Vec<usize> = (0..2).filter(|i| d.b[*i].radio).collect();
    if on.len() == 2 && d.b[0].ssid == d.b[1].ssid {
        return quoted(l, &d.b[0].ssid);
    }
    on.iter()
        .map(|i| format!("{} ({})", quoted(l, &d.b[*i].ssid), BANDS[*i]))
        .collect::<Vec<_>>()
        .join(l.pick(" and ", " 和 "))
}

/// The consequence, in plain words (the old page's `describe`).
pub fn what(c: &Change, l: Lang) -> String {
    let d = &c.draft;
    let mut lines: Vec<String> = Vec::new();
    if let Some(v) = c.params.get("wifi_onoff") {
        lines.push(if v == "0" {
            l.pick("All Wi-Fi goes off: every device on the U60's Wi-Fi drops.", "所有 Wi-Fi 都会关掉：连着 U60 Wi-Fi 的设备全部断开。").into()
        } else {
            l.pick("Wi-Fi comes on.", "Wi-Fi 打开。").into()
        });
    }
    for i in 0..2 {
        let (b, x) = (BANDS[i], SUFFIX[i]);
        let f = &d.b[i];
        if c.has(&format!("radio{}_disabled", &x[..1])) {
            lines.push(match (l, f.radio) {
                (Lang::En, false) => format!("{b} goes off; devices that only use it drop."),
                (Lang::En, true) => format!("{b} comes on."),
                (Lang::Zh, false) => format!("{b} 关掉；只用它的设备会断开。"),
                (Lang::Zh, true) => format!("{b} 打开。"),
            });
        }
        if c.has(&format!("ssid_{x}")) {
            lines.push(match l {
                Lang::En => format!("{b} network name becomes {}.", quoted(l, &f.ssid)),
                Lang::Zh => format!("{b} 的网络名称改为{}。", quoted(l, &f.ssid)),
            });
        }
        if c.has(&format!("encryption_{x}")) {
            lines.push(match (l, f.enc == "none") {
                (Lang::En, true) => format!("{b} becomes an open network: anyone nearby can join without a password."),
                (Lang::Zh, true) => format!("{b} 变成开放网络：附近任何人不用密码就能连。"),
                (Lang::En, false) => format!("{b} security becomes {}.", enc_label(l, &f.enc)),
                (Lang::Zh, false) => format!("{b} 的加密方式改为 {}。", enc_label(l, &f.enc)),
            });
        }
        if c.keys[i] {
            lines.push(match l {
                Lang::En => format!("{b} password changes."),
                Lang::Zh => format!("{b} 的密码会改。"),
            });
        }
        if c.has(&format!("hidden_{x}")) {
            lines.push(match (l, f.hidden) {
                (Lang::En, true) => format!("{b} stops showing its name; new devices must add it by hand."),
                (Lang::En, false) => format!("{b} shows its name again."),
                (Lang::Zh, true) => format!("{b} 不再广播名称；新设备要手动添加。"),
                (Lang::Zh, false) => format!("{b} 重新广播名称。"),
            });
        }
    }
    let mut others = Vec::new();
    if c.has("channel_2g") || c.has("channel_5g") {
        others.push(l.pick("channel", "信道"));
    }
    if c.has("htmode_2g") || c.has("htmode_5g") {
        others.push(l.pick("bandwidth", "带宽"));
    }
    if (c.has("txpower_2g") || c.has("txpower_5g")) && !c.txpower_only() {
        others.push(l.pick("transmit power", "发射功率"));
    }
    if c.has("country") {
        others.push(l.pick("region", "区域"));
    }
    if !others.is_empty() {
        lines.push(match l {
            Lang::En => format!("Also changes: {}.", others.join(", ")),
            Lang::Zh => format!("另外改：{}。", others.join("、")),
        });
    }
    if c.txpower_only() {
        lines.push(l.pick(
            "Only the transmit power changes: it applies at once and Wi-Fi does not restart.",
            "只改发射功率：马上生效，Wi-Fi 不重启。",
        ).into());
    } else if !c.master_off() {
        lines.push(l.pick(
            "Wi-Fi reloads: devices on this Wi-Fi drop for about 10–60 seconds.",
            "Wi-Fi 会重新加载：连着这个 Wi-Fi 的设备断开约 10–60 秒。",
        ).into());
    }
    lines.join(" ")
}

pub fn confirm(c: &Change, l: Lang) -> write::Confirm {
    let action = l.pick("Confirm: apply the Wi-Fi settings", "确认：应用 Wi-Fi 设置").to_string();
    let title = l.pick("Apply the Wi-Fi settings?", "应用 Wi-Fi 设置？").to_string();
    if c.txpower_only() {
        return write::Confirm {
            tier: write::Tier::Inline,
            title,
            what: what(c, l),
            downtime: None,
            recovery: None,
            action,
            cuts_uplink: false,
            word: None,
            danger: false,
            revert: crate::write::Revert::No,
        };
    }
    let downtime = if c.master_off() {
        l.pick(
            "Wi-Fi stays off until it is turned on again; every device on it drops.",
            "Wi-Fi 一直关着，直到再打开；连着它的设备全部断开。",
        )
        .to_string()
    } else {
        l.pick(
            "Devices on this Wi-Fi drop for about 10–60 seconds while it reloads.",
            "Wi-Fi 重新加载时，连着它的设备断开约 10–60 秒。",
        )
        .to_string()
    };
    let not_ts = l.pick(
        " Over a cable, USB or Tailscale (which goes over the mobile link) the page is not affected.",
        "经网线、USB 或 Tailscale（走移动网络）打开的这个页面不受影响。",
    );
    let recovery = if c.master_off() {
        l.pick(
            "Turn Wi-Fi back on from the touchscreen, or open this page over a cable, USB or Tailscale and turn it on here.",
            "在触屏上把 Wi-Fi 打开，或经网线、USB 或 Tailscale 打开这个页面再打开。",
        )
        .to_string()
    } else if c.rejoin() {
        let n = names(c, l);
        let key = c.keys.iter().any(|k| *k);
        match (l, key) {
            (Lang::En, true) => format!("Devices on the U60's Wi-Fi (this browser too, if it is on it) must join {n} again with the new password; then open this page again.{not_ts}"),
            (Lang::En, false) => format!("Devices on the U60's Wi-Fi (this browser too, if it is on it) must join {n} again; then open this page again.{not_ts}"),
            (Lang::Zh, true) => format!("连着 U60 Wi-Fi 的设备（包括这个浏览器，如果它也连着）要用新密码重新连到{n}，再打开这个页面。{not_ts}"),
            (Lang::Zh, false) => format!("连着 U60 Wi-Fi 的设备（包括这个浏览器，如果它也连着）要重新连到{n}，再打开这个页面。{not_ts}"),
        }
    } else {
        format!(
            "{}{not_ts}",
            l.pick(
                "If this browser is on the U60's Wi-Fi, the page loses its connection, reconnects by itself when Wi-Fi is back and shows the result.",
                "如果这个浏览器连的是 U60 的 Wi-Fi，页面会断开，Wi-Fi 回来后自己重连并显示结果。",
            )
        )
    };
    write::Confirm {
        tier: write::Tier::Dialog,
        title,
        what: what(c, l),
        downtime: Some(downtime),
        recovery: Some(recovery),
        action,
        // Tailscale goes over the mobile link: no "someone has to be at the device" warning
        cuts_uplink: false,
        word: None,
        danger: false,
        revert: crate::write::Revert::No,
    }
}

/// The core's AP layout refusals (`luyaz_wifi::layout`), matched by their start.
const LAYOUT_WORDS: [(&str, &str, &str); 3] = [
    (
        "turning 2.4 GHz off while 5 GHz stays on",
        "Not applied: turning 2.4 GHz off while 5 GHz stays on stops all Wi-Fi on this device; turn both off, or only 5 GHz",
        "没有生效：只关 2.4 GHz、5 GHz 还开着，这台设备的 Wi-Fi 会整个断掉；请两个都关，或只关 5 GHz",
    ),
    (
        "a guest network needs the main network",
        "Not applied: a guest network needs the main network of its band on",
        "没有生效：访客网络开着时，同频段的主网络也要开着",
    ),
    (
        "this device cannot run exactly three Wi-Fi networks",
        "Not applied: this device cannot run exactly three Wi-Fi networks; turn the guest network on for both bands, or off",
        "没有生效：这台设备不能刚好开三个 Wi-Fi 网络；访客网络请两个频段一起开或一起关",
    ),
];

/// u60d's English-only messages in the page's language.
pub(crate) fn u60d_words(m: &str, l: Lang) -> String {
    if let Some((_, en, zh)) = LAYOUT_WORDS.iter().find(|(p, ..)| m.starts_with(p)) {
        return l.pick(en, zh).into();
    }
    if m.starts_with("this firmware changes Wi-Fi 6") {
        return wifi6_refused(l);
    }
    if m.starts_with("the old Wi-Fi password was not kept") {
        return l
            .pick(
                "the old Wi-Fi password was not kept on the device",
                "设备上没有留着旧的 Wi-Fi 密码",
            )
            .into();
    }
    m.to_owned()
}

/// A refused `/control` reply in the page's words: busy (409 with `doing`, or 503 = the Wi-Fi lock)
/// says what holds the device; refusals and invalid bodies give u60d's reason.
pub fn refusal(status: u16, v: &Value, l: Lang) -> write::Outcome {
    let code = v["error"]["code"].as_str().unwrap_or("");
    let msg = v["error"]["message"].as_str().unwrap_or("");
    if matches!(code, "busy" | "op_busy") {
        let doing = crate::changes::pick(&v["doing"], "say", l);
        return write::Outcome::NotYet(if !doing.is_empty() {
            match l {
                Lang::En => format!("Not applied: another change is in progress · {doing}"),
                Lang::Zh => format!("没有生效：另一项改动正在进行 · {doing}"),
            }
        } else if status == 503 || msg.starts_with("Wi-Fi is being changed") {
            l.pick(
                "Not applied: Wi-Fi is being changed by something else (a scenario, the watchdog or the touchscreen); try again in a moment",
                "没有生效：Wi-Fi 正被别的程序修改（情景模式、看门狗或触屏），稍后再试",
            )
            .into()
        } else {
            write::Outcome::Busy.text(l)
        });
    }
    write::Outcome::Failed(if msg.is_empty() {
        l.pick("the core service refused", "核心服务拒绝了").into()
    } else {
        u60d_words(msg, l)
    })
}

// ---- the view ----

pub struct Row {
    pub label: String,
    pub value: String,
    /// Device text (SSIDs): kept out of the English-page check.
    pub data: bool,
}

pub struct BandView {
    pub title: &'static str,
    pub desc: &'static str,
    pub rows: Vec<Row>,
}

pub struct View {
    pub l: Lang,
    pub tone: &'static str,
    pub state: String,
    pub reason: Option<String>,
    pub meta: Vec<String>,
    pub retry: bool,
    pub stale: bool,
    /// The last `wifi.main` result (u60d's words, its tone), or the running one.
    pub op: Option<(String, &'static str)>,
    pub guest: Option<(String, String)>,
    pub global: Vec<Row>,
    pub bands: Vec<BandView>,
    pub psm: Option<Psm>,
    pub nfc: Option<Option<bool>>,
}

impl View {
    pub fn symbol(&self) -> &'static str {
        match self.tone {
            "bad" => "■",
            "warn" => "▲",
            _ => "●",
        }
    }
    pub fn psm_sub(&self) -> Option<&'static str> {
        let p = self.psm?;
        match (p.enabled, p.live) {
            (None, _) => Some(self.l.pick(
                "Not read yet (Wi-Fi off and never set here)",
                "还没读到（Wi-Fi 关着，也没在这里设过）",
            )),
            (Some(_), None) => Some(self.l.pick(
                "Saved; applies when Wi-Fi is on",
                "已保存；Wi-Fi 打开时生效",
            )),
            _ => None,
        }
    }
}

fn on_off(l: Lang, v: bool) -> String {
    if v { l.pick("On", "开") } else { l.pick("Off", "关") }.into()
}

fn row(label: &str, value: String) -> Row {
    Row {
        label: label.into(),
        value,
        data: false,
    }
}

pub fn view(st: &St, live: Option<&Live>, psm: Option<Psm>, l: Lang) -> View {
    let w = wifi(st);
    let rb = block(st);
    let stale = rb.is_some_and(|(_, s)| s);
    let known = matches!(st.link, Link::Live | Link::Down);
    let n = on_wifi(st);
    let mut meta = Vec::new();
    let (mut tone, state, reason, retry): (&'static str, String, Option<String>, bool) = match &w {
        None if rb.is_some() || known => (
            "bad",
            l.pick("Can't read the Wi-Fi settings", "读不到 Wi-Fi 设置").into(),
            Some(
                match (rb.is_some(), &st.link) {
                    (false, Link::Down) => l.pick("Can't reach the data service.", "连不上数据服务。"),
                    (false, _) => l.pick("The data service has no Wi-Fi settings.", "数据服务没有 Wi-Fi 设置。"),
                    _ => l.pick("The device didn't answer the Wi-Fi read.", "设备没有回答 Wi-Fi 设置的读取。"),
                }
                .into(),
            ),
            true,
        ),
        None => (
            "neutral",
            l.pick("Reading Wi-Fi…", "正在读取 Wi-Fi…").into(),
            None,
            false,
        ),
        Some(w) if !w.on => (
            "neutral",
            l.pick("Wi-Fi off", "Wi-Fi 已关闭").into(),
            Some(l.pick(
                "The master switch is off. Devices reach the U60 by cable, USB or Tailscale only.",
                "总开关关着。设备只能经网线、USB 或 Tailscale 连 U60。",
            ).into()),
            false,
        ),
        Some(w) if !w.bands.iter().any(Band::up) => (
            "neutral",
            l.pick("Both bands off", "两个频段都关着").into(),
            Some(l.pick(
                "The 2.4 GHz and 5 GHz radios are both switched off.",
                "2.4 GHz 和 5 GHz 都关着。",
            ).into()),
            false,
        ),
        Some(w) if live.is_some_and(|v| (0..2).all(|i| !w.bands[i].up() || v.ch[i].is_empty())) => (
            "warn",
            l.pick("Wi-Fi not broadcasting", "Wi-Fi 没在广播").into(),
            Some(l.pick(
                "It is switched on, but no band is up. Right after a change, give it a minute and look again.",
                "开着，但没有频段起来。刚改过的话，等一分钟再看。",
            ).into()),
            false,
        ),
        Some(_) => (
            "ok",
            l.pick("Wi-Fi on", "Wi-Fi 已开启").into(),
            n.map(|n| match l {
                Lang::En => format!("{n} device(s) connected"),
                Lang::Zh => format!("{n} 台设备连着"),
            }),
            false,
        ),
    };
    if let Some(w) = w.as_ref().filter(|w| w.on) {
        for i in 0..2 {
            let b = BANDS[i];
            let line = if !w.bands[i].up() {
                format!("{b}{}", l.pick(": off", "：已关"))
            } else {
                match live {
                    None => format!("{b}{}", l.pick(": on", "：开")),
                    Some(v) if v.ch[i].is_empty() => {
                        format!("{b}{}", l.pick(": not broadcasting", "：没在广播"))
                    }
                    Some(v) => {
                        let bw = if v.bw[i].is_empty() { "—" } else { &v.bw[i] };
                        let dev = v.n[i].map(|n| match l {
                            Lang::En => format!(" · {n} device(s)"),
                            Lang::Zh => format!(" · {n} 台设备"),
                        });
                        match l {
                            Lang::En => format!("{b}: channel {} · {bw}{}", v.ch[i], dev.unwrap_or_default()),
                            Lang::Zh => format!("{b}：信道 {} · {bw}{}", v.ch[i], dev.unwrap_or_default()),
                        }
                    }
                }
            };
            meta.push(line);
        }
    }
    if w.is_some() && stale {
        tone = "stale";
        meta.push(
            l.pick(
                "Settings not current — refresh before changing anything",
                "设置不是最新的，刷新后再改",
            )
            .into(),
        );
    }
    let op = match crate::follow::running(st, &[ITEM], l) {
        Some(s) => Some((
            match l {
                Lang::En => format!("Changing Wi-Fi settings · {s}"),
                Lang::Zh => format!("正在改 Wi-Fi 设置 · {s}"),
            },
            "neutral",
        )),
        None => st.blocks.get("op").and_then(|b| {
            let last = &b.data["last"];
            (last["item"].as_str() == Some(ITEM)).then(|| {
                let say = crate::changes::pick(last, "say", l);
                (
                    format!("{}{say}", l.pick("Last change: ", "上次改动：")),
                    crate::changes::tone(&last["mark"]),
                )
            })
        }),
    };
    let guest = w.as_ref().and_then(|w| w.guest.clone()).map(|g| (g.ssid, on_off(l, g.on)));
    let global = match &w {
        None => Vec::new(),
        Some(w) => vec![
            row(l.pick("Wi-Fi", "Wi-Fi"), on_off(l, w.on)),
            row(
                "Wi-Fi 6",
                if w.wifi6 { l.pick("Enabled", "已启用") } else { l.pick("Disabled", "已停用") }.into(),
            ),
            row(l.pick("Region", "区域"), region_label(l, &w.country)),
        ],
    };
    let bands = match &w {
        None => Vec::new(),
        Some(w) => (0..2)
            .map(|i| {
                let b = &w.bands[i];
                BandView {
                    title: BANDS[i],
                    desc: if i == 0 {
                        l.pick("Longer range and better through walls — slower speeds.", "覆盖更远、穿墙更好，速度较慢。")
                    } else {
                        l.pick("Faster speeds and less congestion — shorter range.", "速度更快、干扰更少，覆盖较近。")
                    },
                    rows: vec![
                        row(l.pick("Radio", "无线电"), on_off(l, b.radio_on)),
                        Row {
                            label: l.pick("Network name (SSID)", "网络名称（SSID）").into(),
                            value: if b.ssid.is_empty() { "—".into() } else { b.ssid.clone() },
                            data: true,
                        },
                        row(
                            l.pick("Password", "密码"),
                            if b.has_password { l.pick("Set", "已设密码") } else { l.pick("None", "没有密码") }.into(),
                        ),
                        row(l.pick("Security", "加密方式"), enc_label(l, &b.encryption)),
                        row(l.pick("Channel", "信道"), ch_label(l, &b.channel)),
                        row(l.pick("Bandwidth", "带宽"), ht_label(l, &b.htmode)),
                        row(l.pick("Transmit power", "发射功率"), tx_label(l, &b.txpower)),
                        row(
                            l.pick("Hide network name", "隐藏网络名称"),
                            if b.hidden { l.pick("Yes", "是") } else { l.pick("No", "否") }.into(),
                        ),
                    ],
                }
            })
            .collect(),
    };
    View {
        l,
        tone,
        state,
        reason,
        meta,
        retry,
        stale,
        op,
        guest,
        global,
        bands,
        psm,
        nfc: nfc(st),
    }
}

#[derive(Template)]
#[template(path = "wifi_main_leaves.html")]
struct Leaves<'a> {
    v: &'a View,
}

fn render(app: &Shared, live: Option<&Live>, psm: Option<Psm>, l: Lang) -> Vec<(String, String)> {
    app.hub
        .read(|st| split_leaves(&Leaves { v: &view(st, live, psm, l) }.render().unwrap()))
}

// ---- the form ----

pub struct Opt {
    pub value: String,
    pub label: String,
    pub sel: bool,
}

fn opts(list: &[&str], cur: &str, label: impl Fn(&str) -> String) -> Vec<Opt> {
    let mut v: Vec<Opt> = list
        .iter()
        .map(|x| Opt {
            value: (*x).into(),
            label: label(x),
            sel: *x == cur,
        })
        .collect();
    if !list.contains(&cur) {
        // the device's own value stays selectable (and is not a change)
        v.insert(
            0,
            Opt {
                value: cur.into(),
                label: if cur.is_empty() { "—".into() } else { label(cur) },
                sel: true,
            },
        );
    }
    v
}

pub struct BandForm {
    /// "2" / "5" (signal suffix).
    pub s: &'static str,
    pub title: &'static str,
    pub has_password: bool,
    pub radio: bool,
    pub hidden: bool,
    pub enc: Vec<Opt>,
    pub ch: Vec<Opt>,
    pub ht: Vec<Opt>,
    pub tx: Vec<Opt>,
}

impl BandForm {
    pub fn sig(&self, k: &str) -> String {
        format!("_f{k}{}", self.s)
    }
    pub fn lid(&self, k: &str) -> String {
        format!("wf-f{k}{}-l", self.s)
    }
    pub fn lab(&self, l: &Lang, en: &'static str, zh: &'static str) -> String {
        format!("{} {}", self.title, l.pick(en, zh))
    }
}

#[derive(Template)]
#[template(path = "wifi_main_form.html")]
struct Form {
    l: Lang,
    open: bool,
    on: bool,
    w6: bool,
    sigs: String,
    regions: Vec<Opt>,
    bands: Vec<BandForm>,
}

/// The 「更改设置」 button and the form container, open or closed, as two leaves.
fn form_leaves(open: bool, st: &St, l: Lang) -> String {
    let w = if open { wifi(st) } else { None };
    let mut sigs = Map::new();
    let (mut regions, mut bands) = (Vec::new(), Vec::new());
    if let Some(w) = &w {
        let seed = Fields::seed(w);
        seed.signals('f', &mut sigs);
        seed.signals('g', &mut sigs);
        for s in ["2", "5"] {
            sigs.insert(format!("_fk{s}"), json!(""));
            sigs.insert(format!("_fv{s}"), json!(false));
        }
        regions = opts(&REGIONS, &seed.cc, |c| region_label(l, c));
        bands = (0..2)
            .map(|i| {
                let b = &seed.b[i];
                BandForm {
                    s: ["2", "5"][i],
                    title: BANDS[i],
                    has_password: w.bands[i].has_password,
                    radio: b.radio,
                    hidden: b.hidden,
                    enc: opts(&ENCS, &b.enc, |e| enc_label(l, e)),
                    ch: opts(
                        if i == 0 { &CHANNELS_2G[..] } else { &CHANNELS_5G[..] },
                        &b.ch,
                        |c| ch_label(l, c),
                    ),
                    ht: opts(&HTMODES, &b.ht, |h| ht_label(l, h)),
                    tx: opts(&TXPOWERS, &b.tx, |t| tx_label(l, t)),
                }
            })
            .collect();
    }
    Form {
        l,
        open: w.is_some(),
        on: w.as_ref().is_some_and(|w| w.on),
        w6: w.as_ref().is_some_and(|w| w.wifi6),
        sigs: Value::Object(sigs).to_string(),
        regions,
        bands,
    }
    .render()
    .unwrap()
}

fn form(open: bool, st: &St, l: Lang) -> String {
    form_leaves(open, st, l).replace("<!--leaf-->", "\n")
}

// ---- checked changes waiting for their confirm ----

#[derive(Clone, Debug)]
struct Held {
    change: Change,
    op_id: String,
}

fn stash() -> &'static Mutex<HashMap<String, (Instant, Held)>> {
    static S: std::sync::OnceLock<Mutex<HashMap<String, (Instant, Held)>>> =
        std::sync::OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

fn put(h: Held) -> String {
    let t = crate::auth::random_id();
    let mut m = stash().lock().unwrap();
    m.retain(|_, (at, _)| at.elapsed() < STASH_TTL);
    m.insert(t.clone(), (Instant::now(), h));
    t
}

fn take(t: &str) -> Option<Held> {
    stash()
        .lock()
        .unwrap()
        .remove(t)
        .filter(|(at, _)| at.elapsed() < STASH_TTL)
        .map(|(_, h)| h)
}

// ---- the page and its stream ----

pub struct Poll {
    l: Lang,
    who: Caller,
    live: Option<Live>,
    psm: Option<Psm>,
}

impl Polled for Poll {
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        let (s, p) = tokio::join!(
            crate::agent::get(app, &self.who, "/api/wifi/status"),
            crate::agent::get(app, &self.who, "/api/wifi/power-save"),
        );
        // only the six live fields are kept (the reply has the passwords in it)
        self.live = s.ok().map(|v| Live::of(&v));
        self.psm = p.ok().map(|v| Psm::of(&v));
        (render(app, self.live.as_ref(), self.psm, self.l), AGENT_POLL)
    }
    fn watches(&self, block: &str) -> bool {
        matches!(block, "wifi" | "clients" | "nfc" | "op" | "exec")
    }
    fn render(&mut self, app: &Shared) -> Option<Vec<(String, String)>> {
        Some(render(app, self.live.as_ref(), self.psm, self.l))
    }
}

#[derive(Template)]
#[template(path = "wifi_main.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    leaves: Vec<(String, String)>,
    main_op: String,
    psm_op: String,
    nfc_op: String,
}

impl Page {
    fn leaf(&self, id: &str) -> &str {
        self.leaves
            .iter()
            .find(|(k, _)| k == id)
            .map_or("", |(_, h)| h.as_str())
    }
}

/// The write areas: `wf-main` (the settings; `op` = the op_id its lost reply follows), `wf-psm`,
/// `wf-nfc`.
fn area(id: &str, l: Lang, remote: Via, set: &str, op: &str, phase: &write::Phase) -> String {
    let mut cancel = format!("{PATH}/{}/cancel", &id[3..]);
    if !op.is_empty() {
        cancel.push_str("?op=");
        cancel.push_str(op);
    }
    write::Area {
        id,
        l,
        remote,
        set,
        cancel: &cancel,
        phase,
    }
    .html()
}

pub async fn page(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let shell = app.hub.read(|st| {
        crate::shell::Shell::new(&headers, PATH).live(
            st,
            &ctx,
            Some(format!("{PATH}/events?g={}", st.generation)),
        )
    });
    let idle = write::Phase::Idle;
    let html = Page {
        shell,
        l,
        leaves: {
            let mut v = render(&app, None, None, l);
            v.extend(app.hub.read(|st| split_leaves(&form_leaves(false, st, l))));
            v
        },
        main_op: area("wf-main", l, ctx.remote, "", "", &idle),
        psm_op: area("wf-psm", l, ctx.remote, "", "", &idle),
        nfc_op: area("wf-nfc", l, ctx.remote, "", "", &idle),
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

pub async fn events(
    State(app): State<Shared>,
    Query(q): Query<StreamQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let p = Poll {
        l: ctx.lang,
        who: Caller::of(&app, &req),
        live: None,
        psm: None,
    };
    crate::agent::stream(app, ctx, q.g, PATH, p)
}

/// Retry: reconnect to u60d and ask the agent again now.
pub async fn refresh(State(app): State<Shared>) -> axum::http::StatusCode {
    app.hub.retry();
    let _ = app.kick.send(PATH);
    axum::http::StatusCode::NO_CONTENT
}

fn one(html: String) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(futures_util::stream::once(async move { Ok(patch(&html)) }))
}

/// 「更改设置」: the form, seeded with u60d's values now.
pub async fn form_open(State(app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    let clear = area("wf-main", l, Via::Local, "", "", &write::Phase::Idle);
    let html = app.hub.read(|st| {
        if wifi(st).is_none() {
            format!(
                "{}\n{}",
                form(false, st, l),
                area("wf-main", l, Via::Local, "", "", &write::Phase::Done(not_read(l)))
            )
        } else {
            format!("{}\n{clear}", form(true, st, l))
        }
    });
    one(html)
}

pub async fn form_close(State(app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    one(app.hub.read(|st| form(false, st, l)))
}

/// 「应用」: check the draft against its seed, keep the change under a token, ask.
pub async fn main_ask(State(app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let body = axum::body::to_bytes(req.into_body(), 16 * 1024)
        .await
        .unwrap_or_default();
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let keys = [
        v["_fk2"].as_str().unwrap_or(""),
        v["_fk5"].as_str().unwrap_or(""),
    ];
    let checked = match (Fields::from_signals(&v, 'g'), Fields::from_signals(&v, 'f')) {
        (Some(seed), Some(draft)) => app.hub.read(|st| {
            let cur = wifi(st).filter(|_| fresh(st));
            if crate::follow::running(st, &[ITEM], l).is_some() {
                return Err(write::Outcome::NotYet(
                    l.pick(
                        "A Wi-Fi change is still in progress; wait for it to end",
                        "Wi-Fi 设置还在改，等它结束再改",
                    )
                    .into(),
                ));
            }
            check(&seed, &draft, keys, cur.as_ref(), l)
        }),
        _ => Err(write::Outcome::NotYet(
            l.pick(
                "The form expired; open it again",
                "表单已过期，请重新打开",
            )
            .into(),
        )),
    };
    let html = match checked {
        Err(o) => area("wf-main", l, ctx.remote, "", "", &write::Phase::Done(o)),
        Ok(c) => {
            let mut cf = confirm(&c, l);
            crate::follow::receipt_note(&mut cf, ctx.remote, l);
            let op_id = write::next_op_id();
            let t = put(Held {
                change: c,
                op_id: op_id.clone(),
            });
            let set = format!("{PATH}/main/set?t={t}");
            area("wf-main", l, ctx.remote, &set, &op_id, &write::Phase::Ask(cf))
        }
    };
    one(html)
}

#[derive(Deserialize, Default)]
pub struct TokenQuery {
    #[serde(default)]
    t: String,
}

/// Post the `/control` body once (never retried), keeping u60d's whole reply: a busy reply's
/// `doing` is shown (write.rs's shared send folds it into a generic "busy").
async fn send(app: &Shared, body: &Value, l: Lang) -> Result<(write::Sent, Value), write::Outcome> {
    let rb = app
        .http
        .post(&app.control_url)
        .json(body)
        .timeout(SEND_TIMEOUT);
    match rb.send().await {
        Err(e) if e.is_connect() => Err(write::Outcome::Failed(
            l.pick("can't reach the core service", "连不上核心服务").into(),
        )),
        Err(_) => Ok((write::Sent::NoReply, Value::Null)),
        Ok(r) => {
            let status = r.status().as_u16();
            match r.json::<Value>().await {
                Err(_) => Ok((write::Sent::NoReply, Value::Null)),
                Ok(v) if v["ok"].as_bool() == Some(true) => Ok((write::Sent::Ok, v)),
                Ok(v) => Err(refusal(status, &v, l)),
            }
        }
    }
}

fn applied(l: Lang) -> String {
    l.pick("Wi-Fi settings applied", "Wi-Fi 设置已改好").into()
}

/// Send the change and follow it to its end; the area's last words.
async fn run_main(
    app: &Shared,
    h: &Held,
    tx: &tokio::sync::mpsc::Sender<Event>,
    l: Lang,
    receipt: bool,
) -> Option<write::Outcome> {
    // O3: from off the LAN, the core waits for this page to come back (crate::follow) or reverts
    let body = json!({"action": ACTION, "params": h.change.params, "source": "web", "op_id": h.op_id, "receipt": receipt});
    let changes = app.hub.watch();
    let (sent, reply) = match send(app, &body, l).await {
        Ok(r) => r,
        Err(o) => return Some(o),
    };
    if reply["result"]["note"].as_str() == Some("no changes") && !reply["op"].is_object() {
        return Some(write::Outcome::Applied(
            l.pick(
                "The device already has these settings; nothing to change",
                "设备上已经是这些设置，不用改",
            )
            .into(),
        ));
    }
    // a transaction (or no reply: there may be one) is followed for up to u60d's 90 s
    let txn = reply["op"]["op_id"].is_string() || sent == write::Sent::NoReply;
    let c = h.change.clone();
    let pending = |t: String| area("wf-main", l, Via::Local, "", &h.op_id, &write::Phase::Pending(t));
    let f = crate::follow::follow(
        app,
        tx,
        &h.op_id,
        changes,
        if txn { FOLLOW } else { write::READBACK },
        Duration::ZERO,
        l,
        pending,
        move |st| wifi(st).is_some_and(|w| holds(&c, &w)),
    )
    .await?;
    Some(crate::follow::outcome(&f, sent, applied(l), l))
}

fn done_html(app: &Shared, o: write::Outcome, op: &str, l: Lang) -> String {
    let ok = matches!(o, write::Outcome::Applied(_))
        || matches!(&o, write::Outcome::Ended(_, t) if *t == "ok");
    let close = if ok {
        app.hub.read(|st| form(false, st, l)) + "\n"
    } else {
        String::new()
    };
    format!("{close}{}", area("wf-main", l, Via::Local, "", op, &write::Phase::Done(o)))
}

pub async fn main_set(
    State(app): State<Shared>,
    Query(q): Query<TokenQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let (l, receipt) = (ctx.lang, ctx.remote != Via::Local);
    let held = take(&q.t);
    Sse::new(async_stream(move |tx| async move {
        let Some(h) = held else {
            let o = write::Outcome::NotYet(
                l.pick("The form expired; press Apply again", "表单已过期，请再按一次「应用」").into(),
            );
            let _ = tx.send(patch(&area("wf-main", l, Via::Local, "", "", &write::Phase::Done(o)))).await;
            return;
        };
        // checked again at the send: still readable, nothing running
        let gate = app.hub.read(|st| {
            if !fresh(st) || wifi(st).is_none() {
                Some(not_read(l))
            } else {
                None
            }
        });
        if let Some(o) = gate {
            let _ = tx.send(patch(&area("wf-main", l, Via::Local, "", "", &write::Phase::Done(o)))).await;
            return;
        }
        let pending = area(
            "wf-main",
            l,
            Via::Local,
            "",
            &h.op_id,
            &write::Phase::Pending(l.pick("Sending…", "正在发送…").into()),
        );
        if tx.send(patch(&pending)).await.is_err() {
            // the tab went away before anything was sent: don't write
            return;
        }
        let Some(o) = run_main(&app, &h, &tx, l, receipt).await else {
            return;
        };
        let _ = tx.send(patch(&done_html(&app, o, &h.op_id, l))).await;
    }))
}

#[derive(Deserialize, Default)]
pub struct CancelOp {
    #[serde(default)]
    lost: bool,
    #[serde(default)]
    op: String,
}

fn valid_op(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// Cancel / Esc, or the send's reply was lost (this browser was on the Wi-Fi that reloaded): with
/// the op_id, follow that transaction now and show how it ended.
pub async fn main_cancel(
    State(app): State<Shared>,
    Query(q): Query<CancelOp>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let remote = ctx.remote;
    Sse::new(async_stream(move |tx| async move {
        if !q.lost || !valid_op(&q.op) {
            let ph = write::cancel_phase(&write::CancelQuery { lost: q.lost });
            let _ = tx.send(patch(&area("wf-main", l, remote, "", "", &ph))).await;
            return;
        }
        let op = q.op.clone();
        let pending = |t: String| area("wf-main", l, Via::Local, "", &op, &write::Phase::Pending(t));
        let first = app.hub.read(|st| crate::changes::seen(st, &op));
        let quiet = matches!(first, crate::changes::Seen::Nothing);
        if tx
            .send(patch(&pending(
                l.pick("The connection dropped · checking the result…", "连接断过 · 正在查看结果…").into(),
            )))
            .await
            .is_err()
        {
            return;
        }
        let changes = app.hub.watch();
        // nothing seen: give the hub a few seconds to catch up, then it is unknown
        let window = if quiet { Duration::from_secs(8) } else { FOLLOW };
        let Some(f) = crate::follow::follow(
            &app, &tx, &op, changes, window, Duration::from_secs(3600), l, pending, |_| false,
        )
        .await
        else {
            return;
        };
        let o = match f.seen {
            crate::changes::Seen::Nothing => write::Outcome::Unknown,
            _ => crate::follow::outcome(&f, write::Sent::NoReply, applied(l), l),
        };
        let _ = tx.send(patch(&done_html(&app, o, &op, l))).await;
    }))
}

// ---- power save and NFC (tier 1) ----

#[derive(Deserialize, Default)]
pub struct OnQuery {
    #[serde(default)]
    on: bool,
}

fn said(l: Lang, on: bool, en: &str, zh: &str) -> String {
    match (l, on) {
        (Lang::En, true) => format!("Turned {en} on"),
        (Lang::En, false) => format!("Turned {en} off"),
        (Lang::Zh, true) => format!("已打开{zh}"),
        (Lang::Zh, false) => format!("已关掉{zh}"),
    }
}

/// Power save: `wifi.power_save {enabled}`, read back from the agent (u60d has no block for it).
pub async fn psm_set(
    State(app): State<Shared>,
    Query(q): Query<OnQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    let who = Caller::of(&app, &req);
    let on = q.on;
    let pending = area("wf-psm", l, Via::Local, "", "", &write::Phase::Pending(String::new()));
    write::respond(pending, async move {
        let body = json!({"action": "wifi.power_save", "params": {"enabled": on}, "source": "web", "op_id": write::next_op_id()});
        let o = match write::send_body_reply(&app.http, &app.control_url, &body, None, l).await {
            Err(o) => o,
            Ok((sent, _)) => {
                let read = write::read_back(&app.hub, write::READBACK, Duration::from_secs(2), Duration::from_secs(3), || {
                    let (app, who) = (app.clone(), who.clone());
                    async move {
                        crate::agent::get(&app, &who, "/api/wifi/power-save")
                            .await
                            .is_ok_and(|v| Psm::of(&v).enabled == Some(on))
                    }
                })
                .await;
                if read {
                    write::Outcome::Applied(said(l, on, "Wi-Fi power save", "Wi-Fi 节能"))
                } else {
                    sent.not_read()
                }
            }
        };
        let _ = app.kick.send(PATH);
        area("wf-psm", l, Via::Local, "", "", &write::Phase::Done(o))
    })
}

pub async fn psm_cancel(
    Query(q): Query<write::CancelQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    one(area("wf-psm", ctx.lang, ctx.remote, "", "", &write::cancel_phase(&q)))
}

/// NFC: `nfc.set {enabled, flag: 2}` (the touch screen's flag), read back from the `nfc` block.
pub async fn nfc_set(
    State(app): State<Shared>,
    Query(q): Query<OnQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    let on = q.on;
    let w = write::Write {
        action: "nfc.set",
        params: json!({"enabled": i64::from(on), "flag": 2}),
        readback: Some(Box::new(move |st| nfc(st) == Some(Some(on)))),
        applied: said(l, on, "NFC tap to join", "NFC 碰一碰"),
        timeout: write::READBACK,
        typed: None,
        request_timeout: None,
    };
    let pending = area("wf-nfc", l, Via::Local, "", "", &write::Phase::Pending(String::new()));
    write::respond(pending, async move {
        let o = write::execute(&app.http, &app.control_url, &app.hub, &w, l).await;
        area("wf-nfc", l, Via::Local, "", "", &write::Phase::Done(o))
    })
}

pub async fn nfc_cancel(
    Query(q): Query<write::CancelQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    one(area("wf-nfc", ctx.lang, ctx.remote, "", "", &write::cancel_phase(&q)))
}

#[cfg(test)]
mod tests;
