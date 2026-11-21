//! /router/wifi-guest (访客 Wi-Fi): whether the guest
//! network is on (2.4 / 5 GHz), its name, security, whether a password is set, hidden SSID, AP
//! isolation, the active time and the time left. Batch 4 (u60-web.md).
//!
//! Where each value comes from (the old page read the agent's `/api/wifi/guest` every 30 s):
//! - u60d's `wifi` block: `guest{ssid, encryption, has_password, on_2g, on_5g, hidden, isolate,
//!   active_time, left_s}`, `module_on`, and the corresponding radio and main AP settings.
//!   `left_s` is only read while the guest network is on; the page counts it down
//!   locally (a page-local signal), as the old page did.
//! - Passwords never come back (user decision): the page says only 已设密码 / 没有密码; the form's
//!   password is write-only (empty = keep, else 8–64 characters). The old eye toggle is gone.
//!
//! The one write: u60d `/control {"action":"wifi.guest","source":"web","params":{…}}`, params the
//! agent's PUT /api/wifi/guest keys (u60-api `wifi::guest_set`: `guest_ssid`, `guest_key`,
//! `guest_encryption`, `guest_disabled_2g`, `guest_disabled_5g`, `guest_hidden`, `guest_isolate`,
//! `guest_active_time`), only the keys changed from the form seed, and `expect` for those keys
//! other than the password. u60d runs it as a transaction on item `wifi.guest`
//! (reply `op`; confirmed within 90 s: readback + APs up), or answers `note:"no changes"` (plain).
//! 409 busy carries `doing` (said as is), 503 busy = the Wi-Fi lock is held elsewhere, 409 refused
//! and 400 carry u60d's message. Tier 3 for every change: the reload drops the main network too.
//!
//! The form comes as a patch behind 「更改」 (not in the first load). Its fields are page-local
//! signals (`_wg…`) posted to the ask, which checks them and keeps the body under a random token
//! for the confirm: the password is in that one POST body and the `/control` body, never in a URL,
//! the dialog or any push. A browser on the U60's own Wi-Fi loses this page during the reload: the
//! write area's cancel URL carries the token, so its lost-reply post (Datastar retries it until the
//! browser is back) finds the sent `op_id` and shows how the transaction ended.

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
use crate::agent::Polled;
use crate::changes::Seen;
use crate::hub::{Link, St};
use crate::i18n::Lang;
use crate::pages::{Shared, StreamQuery, async_stream, patch, split_leaves};
use crate::write;

pub const PATH: &str = "/router/wifi-guest";
/// u60d confirms a Wi-Fi change within 90 s (reload + verify); a little more.
const FOLLOW: Duration = Duration::from_secs(100);
const SEND_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a filled form waits for its confirm, and a sent one for its lost-reply post.
const STASH_TTL: Duration = Duration::from_secs(600);
/// The write area (`wg-save-op`).
const AREA: &str = "wg-save";

/// The encryption choices (uci value, English, Chinese), as the old page offered them.
pub const ENCS: [(&str, &str, &str); 4] = [
    ("psk2+ccmp", "WPA2", "WPA2"),
    ("psk3+ccmp", "WPA3", "WPA3"),
    ("psk2+psk3+ccmp", "WPA2/WPA3 Mixed", "WPA2/WPA3 混合"),
    ("none", "Open (no password)", "开放（无密码）"),
];

/// What `sanitize_uci_value` drops silently (u60-api `wifi.rs`): refused here instead.
const STRIPPED: [char; 10] = ['\'', '"', ';', '$', '`', '\\', '|', '<', '>', '&'];

pub fn routes() -> Router<Shared> {
    Router::new()
        .route(PATH, get(page))
        .route("/router/wifi-guest/events", get(events))
        .route("/router/wifi-guest/refresh", post(refresh))
        .route("/router/wifi-guest/form", post(form_open))
        .route("/router/wifi-guest/close", post(form_close))
        .route("/router/wifi-guest/ask", post(ask))
        .route("/router/wifi-guest/set", post(set))
        .route("/router/wifi-guest/cancel", post(cancel))
}

// ---- the wifi block ----

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Guest {
    pub ssid: String,
    pub encryption: String,
    pub has_password: bool,
    pub on_2g: bool,
    pub on_5g: bool,
    pub hidden: bool,
    pub isolate: bool,
    pub active_time: i64,
    pub left_s: Option<i64>,
}

impl Guest {
    fn of(v: &Value) -> Option<Guest> {
        let b = |k: &str| v[k].as_bool();
        Some(Guest {
            ssid: v["ssid"].as_str()?.to_owned(),
            encryption: v["encryption"].as_str()?.to_owned(),
            has_password: b("has_password")?,
            on_2g: b("on_2g")?,
            on_5g: b("on_5g")?,
            hidden: b("hidden")?,
            isolate: b("isolate")?,
            active_time: v["active_time"].as_i64()?,
            left_s: v["left_s"].as_i64(),
        })
    }
    pub fn on(&self) -> bool {
        self.on_2g || self.on_5g
    }

    fn signals(&self) -> Value {
        let mut signals = Map::new();
        for (key, value) in [
            ("on2", json!(self.on_2g)),
            ("on5", json!(self.on_5g)),
            ("ssid", json!(self.ssid)),
            ("enc", json!(self.encryption)),
            ("hid", json!(self.hidden)),
            ("iso", json!(self.isolate)),
            ("time", json!(self.active_time.to_string())),
        ] {
            signals.insert(format!("_wg{key}"), value.clone());
            signals.insert(format!("_wgs_{key}"), value);
        }
        signals.insert("_wgpw".into(), json!(""));
        Value::Object(signals)
    }
}

/// Guest settings and the Wi-Fi configuration that allows them to run.
pub struct Read {
    pub guest: Option<Guest>,
    pub module_on: Option<bool>,
    pub radios: [Option<bool>; 2],
    pub main_aps: [Option<bool>; 2],
    pub current: bool,
}

pub fn read(st: &St) -> Option<Read> {
    let b = st.blocks.get("wifi")?;
    Some(Read {
        guest: Guest::of(&b.data["guest"]),
        module_on: b.data["module_on"].as_bool(),
        radios: [b.data["band_2g"]["radio_on"].as_bool(), b.data["band_5g"]["radio_on"].as_bool()],
        main_aps: [b.data["band_2g"]["ap_on"].as_bool(), b.data["band_5g"]["ap_on"].as_bool()],
        current: !b.stale && st.link == Link::Live,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Availability {
    Eligible,
    ModuleOff,
    RadioOff,
    MainOff,
    Unknown,
}

impl Read {
    fn availability(&self, band: usize) -> Availability {
        use Availability::*;
        if !self.current {
            return Unknown;
        }
        match (self.module_on, self.radios[band], self.main_aps[band]) {
            (Some(false), _, _) => ModuleOff,
            (_, Some(false), _) => RadioOff,
            (_, _, Some(false)) => MainOff,
            (Some(true), Some(true), Some(true)) => Eligible,
            _ => Unknown,
        }
    }

    fn configured(&self, g: &Guest) -> impl Iterator<Item = (&'static str, Availability)> + '_ {
        [("2.4 GHz", g.on_2g), ("5 GHz", g.on_5g)]
            .into_iter()
            .enumerate()
            .filter(|(_, (_, on))| *on)
            .map(|(i, (band, _))| (band, self.availability(i)))
    }

    fn notes(&self, g: &Guest, l: Lang) -> Option<String> {
        use Availability::*;
        if !self.current {
            return Some(l.pick("Availability unknown", "可用性未知").into());
        }
        if self.module_on == Some(false) {
            return Some(l.pick("Wi-Fi module off", "Wi-Fi 模块关闭").into());
        }
        let mut notes = String::new();
        for (band, a) in self.configured(g) {
            let reason = match a {
                Eligible | ModuleOff => continue,
                RadioOff => l.pick("radio off", "射频关闭"),
                MainOff => l.pick("main network off", "主网络关闭"),
                Unknown => l.pick("availability unknown", "可用性未知"),
            };
            if !notes.is_empty() {
                notes.push_str(l.pick("; ", "；"));
            }
            notes.push_str(band);
            notes.push_str(l.pick(": ", "："));
            notes.push_str(reason);
        }
        (!notes.is_empty()).then_some(notes)
    }
}

/// Short status shared by both links to the guest settings. Configuration doesn't prove broadcast.
pub(crate) fn summary(st: &St, l: Lang) -> Option<(String, String)> {
    let mut r = read(st)?;
    let g = r.guest.take()?;
    let state = if g.on() {
        let on = l.pick("Set to on", "已设置开启");
        match r.notes(&g, l) {
            Some(reason) => format!("{on} · {reason}"),
            None => on.into(),
        }
    } else if r.current {
        l.pick("Off", "关").into()
    } else {
        l.pick("Availability unknown", "可用性未知").into()
    };
    Some((g.ssid, state))
}

/// The guest part, only when current (a write needs it).
pub fn guest(st: &St) -> Option<Guest> {
    read(st).filter(|r| r.current).and_then(|r| r.guest)
}

pub fn enc_label(l: Lang, v: &str) -> String {
    match ENCS.iter().find(|e| e.0 == v) {
        Some(e) => l.pick(e.1, e.2).into(),
        None if v.is_empty() => "—".into(),
        None => v.into(),
    }
}

fn bands_text(g: &Guest) -> &'static str {
    match (g.on_2g, g.on_5g) {
        (true, true) => "2.4 GHz + 5 GHz",
        (true, false) => "2.4 GHz",
        (false, true) => "5 GHz",
        (false, false) => "—",
    }
}

/// "1 h 05 min 09 s left" (the same as the page's countdown).
pub fn left_text(l: Lang, s: i64) -> String {
    if s <= 0 {
        return l.pick("Expired", "已到期").into();
    }
    let (h, m, s) = (s / 3600, s % 3600 / 60, s % 60);
    match (l, h > 0) {
        (Lang::En, true) => format!("{h} h {m:02} min {s:02} s left"),
        (Lang::En, false) => format!("{m} min {s:02} s left"),
        (Lang::Zh, true) => format!("剩余 {h} 小时 {m:02} 分 {s:02} 秒"),
        (Lang::Zh, false) => format!("剩余 {m} 分 {s:02} 秒"),
    }
}

/// The same as a Datastar expression over `$_wgl` (seconds), for the local countdown.
fn left_js(l: Lang) -> &'static str {
    match l {
        Lang::En => {
            "$_wgl <= 0 ? 'Expired' : ($_wgl >= 3600 ? Math.floor($_wgl / 3600) + ' h ' + String(Math.floor($_wgl % 3600 / 60)).padStart(2, '0') + ' min ' : Math.floor($_wgl / 60) + ' min ') + String($_wgl % 60).padStart(2, '0') + ' s left'"
        }
        Lang::Zh => {
            "$_wgl <= 0 ? '已到期' : '剩余 ' + ($_wgl >= 3600 ? Math.floor($_wgl / 3600) + ' 小时 ' + String(Math.floor($_wgl % 3600 / 60)).padStart(2, '0') + ' 分 ' : Math.floor($_wgl / 60) + ' 分 ') + String($_wgl % 60).padStart(2, '0') + ' 秒'"
        }
    }
}

// ---- the view ----

pub struct Row {
    pub label: &'static str,
    pub value: String,
    /// User text (the SSID): kept out of the "English page has no Chinese" check.
    pub data: bool,
}

pub struct View {
    pub l: Lang,
    pub tone: &'static str,
    pub state: String,
    pub reason: Option<String>,
    /// Second line: the time left (counted down locally when `left` is set) or "no time limit".
    pub meta: Option<String>,
    pub left: Option<i64>,
    pub retry: bool,
    pub stale: bool,
    pub rows: Vec<Row>,
    /// A Wi-Fi transaction running now: its words.
    pub busy: Option<String>,
}

impl View {
    pub fn symbol(&self) -> &'static str {
        match self.tone {
            "bad" => "■",
            "warn" => "▲",
            _ => "●",
        }
    }
    pub fn left_js(&self) -> &'static str {
        left_js(self.l)
    }
    pub fn sigs(&self) -> String {
        json!({"_wgl": self.left.unwrap_or(0)}).to_string()
    }
}

fn yes_no(l: Lang, on: bool) -> String {
    if on {
        l.pick("On", "开")
    } else {
        l.pick("Off", "关")
    }
    .into()
}

pub fn view(st: &St, l: Lang) -> View {
    let r = read(st);
    let known = matches!(st.link, Link::Live | Link::Down);
    let stale = r.as_ref().is_some_and(|r| !r.current && r.guest.is_some());
    let busy = crate::follow::running(st, &["wifi"], l);
    let mut v = View {
        l,
        tone: "neutral",
        state: l.pick("Reading guest Wi-Fi…", "正在读取访客 Wi-Fi…").into(),
        reason: None,
        meta: None,
        left: None,
        retry: false,
        stale,
        rows: Vec::new(),
        busy,
    };
    let (r, g) = match &r {
        None if !known => return v,
        None => {
            v.tone = "bad";
            v.state = l
                .pick(
                    "Can't read the guest Wi-Fi settings",
                    "读不到访客 Wi-Fi 设置",
                )
                .into();
            v.reason = Some(
                match st.link {
                    Link::Down => l.pick("Can't reach the data service.", "连不上数据服务。"),
                    _ => l.pick(
                        "The data service has no Wi-Fi settings.",
                        "数据服务没有 Wi-Fi 设置。",
                    ),
                }
                .into(),
            );
            v.retry = true;
            return v;
        }
        Some(Read { guest: None, .. }) => {
            v.tone = "bad";
            v.state = l
                .pick(
                    "Can't read the guest Wi-Fi settings",
                    "读不到访客 Wi-Fi 设置",
                )
                .into();
            v.reason = Some(
                l.pick(
                    "The device returned no Wi-Fi settings.",
                    "设备没有返回 Wi-Fi 设置。",
                )
                .into(),
            );
            v.retry = true;
            return v;
        }
        Some(r @ Read { guest: Some(g), .. }) => (r, g),
    };
    if g.ssid.is_empty() && !g.on() {
        v.tone = "warn";
        v.state = l.pick("No guest network found", "没有找到访客网络").into();
        v.reason = Some(
            l.pick(
                "The device has no guest network name. Settings saved here may not take effect.",
                "设备上没有访客网络名称，在这里保存的设置可能不生效。",
            )
            .into(),
        );
    } else if !g.on() {
        v.state = l.pick("Guest Wi-Fi off", "访客 Wi-Fi 已关闭").into();
        v.reason = Some(
            l.pick(
                "Turn on 2.4 GHz or 5 GHz under “Change” to let guests join.",
                "在「更改」里打开 2.4 GHz 或 5 GHz，访客才能连接。",
            )
            .into(),
        );
    } else {
        let notes = r.notes(g, l);
        v.tone = if notes.is_some() { "warn" } else { "ok" };
        v.state = l.pick("Guest Wi-Fi set to on", "访客 Wi-Fi 已设置开启").into();
        let bands = bands_text(g);
        v.reason = Some(match notes {
            Some(reason) => format!("{bands} · {reason}"),
            None => bands.into(),
        });
        if r.configured(g).any(|(_, a)| a == Availability::Eligible) {
            if g.active_time == 0 {
                v.meta = Some(l.pick("No time limit", "不限时").into());
            } else if let Some(s) = g.left_s.filter(|s| *s > 0) {
                v.meta = Some(left_text(l, s));
                v.left = Some(s);
            }
        }
    }
    if stale {
        v.tone = "stale";
        v.left = None;
        v.meta = Some(
            l.pick(
                "Settings not current — refresh before changing anything",
                "设置不是最新的，刷新后再改",
            )
            .into(),
        );
    }
    v.rows = vec![
        Row {
            label: l.pick("Guest SSID", "访客 SSID"),
            value: if g.ssid.is_empty() {
                "—".into()
            } else {
                g.ssid.clone()
            },
            data: true,
        },
        Row {
            label: "2.4 GHz",
            value: yes_no(l, g.on_2g),
            data: false,
        },
        Row {
            label: "5 GHz",
            value: yes_no(l, g.on_5g),
            data: false,
        },
        Row {
            label: l.pick("Security", "加密方式"),
            value: enc_label(l, &g.encryption),
            data: false,
        },
        Row {
            label: l.pick("Password", "密码"),
            value: if g.encryption == "none" {
                l.pick("None (open)", "无（开放）").into()
            } else if g.has_password {
                l.pick("Password set", "已设密码").into()
            } else {
                l.pick("No password", "没有密码").into()
            },
            data: false,
        },
        Row {
            label: l.pick("Hide SSID", "隐藏 SSID"),
            value: yes_no(l, g.hidden),
            data: false,
        },
        Row {
            label: l.pick("AP Isolation", "AP 隔离"),
            value: if g.isolate {
                l.pick(
                    "On, guests can't reach each other",
                    "开，访客之间不能互访",
                )
            } else {
                l.pick("Off, guests can reach each other", "关，访客之间可以互访")
            }
            .into(),
            data: false,
        },
        Row {
            label: l.pick("Active time", "有效时长"),
            value: if g.active_time == 0 {
                l.pick("No limit", "不限").into()
            } else {
                match l {
                    Lang::En => format!("{} min", g.active_time),
                    Lang::Zh => format!("{} 分钟", g.active_time),
                }
            },
            data: false,
        },
    ];
    v
}

#[derive(Template)]
#[template(path = "wifi_guest_leaves.html")]
struct Leaves<'a> {
    v: &'a View,
}

fn leaves(app: &Shared, l: Lang) -> Vec<(String, String)> {
    app.hub
        .read(|st| split_leaves(&Leaves { v: &view(st, l) }.render().unwrap()))
}

// ---- the form and its checks ----

/// A checked change: the `/control` params (may hold the password), what the guest part must read
/// back as, and the dialog's words.
#[derive(Clone, Debug, PartialEq)]
pub struct Save {
    pub params: Map<String, Value>,
    pub want: Guest,
    /// A password was sent (readback: `has_password`).
    pub key_sent: bool,
    pub what: String,
    pub reconnect: Option<String>,
}

impl Save {
    /// The guest part shows this change.
    pub fn holds(&self, g: &Guest) -> bool {
        let w = &self.want;
        let has = |key| self.params.contains_key(key);
        (!has("guest_ssid") || g.ssid == w.ssid)
            && (!has("guest_encryption") || g.encryption == w.encryption)
            && (!has("guest_disabled_2g") || g.on_2g == w.on_2g)
            && (!has("guest_disabled_5g") || g.on_5g == w.on_5g)
            && (!has("guest_hidden") || g.hidden == w.hidden)
            && (!has("guest_isolate") || g.isolate == w.isolate)
            && (!has("guest_active_time") || g.active_time == w.active_time)
            && (!self.key_sent || g.has_password)
    }
}

fn not_read(l: Lang) -> write::Outcome {
    write::Outcome::NotYet(
        l.pick(
            "Guest Wi-Fi settings not read yet; try again shortly",
            "还没读到访客 Wi-Fi 设置，稍后再试",
        )
        .into(),
    )
}

fn bad(s: &str) -> write::Outcome {
    write::Outcome::NotYet(s.into())
}

fn time_of(v: &Value) -> Option<i64> {
    let n = match v {
        Value::Number(n) => n.as_i64()?,
        Value::String(s) => {
            let s = s.trim();
            if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) || s.len() > 5 {
                return None;
            }
            s.parse().ok()?
        }
        _ => return None,
    };
    (0..=1440).contains(&n).then_some(n)
}

fn flag(on: bool) -> Value {
    json!(if on { "1" } else { "0" })
}

/// Check changes from the form seed, preserving settings edited elsewhere.
fn check(f: &Value, cur: &Guest, l: Lang) -> Result<Save, write::Outcome> {
    let mut want = cur.clone();
    let mut params = Map::new();
    let mut expect = Map::new();
    let incomplete = || {
        bad(l.pick("Reopen the guest Wi-Fi form and try again", "请重新打开访客 Wi-Fi 表单再试"))
    };
    let conflict = || bad(&crate::wifi_main::u60d_words(
        "Wi-Fi settings changed since the form was opened", l,
    ));
    let time = |v: &Value| time_of(v).ok_or_else(|| bad(l.pick(
        "Active time must be a whole number of minutes from 0 to 1440",
        "有效时长须为 0 到 1440 的整数分钟",
    )));
    macro_rules! edit {
        ($field:ident, $sig:literal, $key:literal, $snapshot:literal, $reader:expr, $send:expr) => {{
            let seed = ($reader)(&f[concat!("_wgs_", $sig)])?;
            let draft = ($reader)(&f[concat!("_wg", $sig)])?;
            if seed != draft {
                if seed != cur.$field {
                    return Err(conflict());
                }
                expect.insert(concat!("guest.", $snapshot).into(), json!(seed));
                params.insert(concat!("guest_", $key).into(), ($send)(&draft));
                want.$field = draft;
            }
        }};
    }
    let boolean = |v: &Value| v.as_bool().ok_or_else(incomplete);
    let text = |v: &Value| v.as_str().map(str::to_owned).ok_or_else(incomplete);
    edit!(ssid, "ssid", "ssid", "ssid", text, |v: &String| json!(v));
    edit!(encryption, "enc", "encryption", "encryption", text, |v: &String| json!(v));
    edit!(on_2g, "on2", "disabled_2g", "on_2g", boolean, |v: &bool| flag(!*v));
    edit!(on_5g, "on5", "disabled_5g", "on_5g", boolean, |v: &bool| flag(!*v));
    edit!(hidden, "hid", "hidden", "hidden", boolean, |v: &bool| flag(*v));
    edit!(isolate, "iso", "isolate", "isolate", boolean, |v: &bool| flag(*v));
    edit!(active_time, "time", "active_time", "active_time", time, |v: &i64| json!(v.to_string()));

    let ssid = &want.ssid;
    if ssid.contains(STRIPPED) {
        return Err(bad(l.pick(
            "The guest network name can't contain ' \" ; $ ` \\ | < > & — the device would silently drop them",
            "访客网络名称不能含 ' \" ; $ ` \\ | < > &，设备会悄悄删掉这些字符",
        )));
    }
    if ssid.trim().is_empty() {
        return Err(bad(
            l.pick("Enter a guest network name", "请填写访客网络名称")
        ));
    }
    if ssid.len() > 32 {
        return Err(bad(l.pick(
            "The guest network name is longer than 32 bytes",
            "访客网络名称超过 32 字节",
        )));
    }
    if !ENCS.iter().any(|e| e.0 == want.encryption) && want.encryption != cur.encryption {
        return Err(bad(l.pick("Pick a security type", "请选择加密方式")));
    }
    let open = want.encryption == "none";
    let pw = if open { "" } else { f["_wgpw"].as_str().unwrap_or("") };
    if !pw.is_empty() {
        if pw.contains(STRIPPED) {
            return Err(bad(l.pick(
                "The guest password can't contain ' \" ; $ ` \\ | < > & — the device would silently drop them",
                "访客密码不能含 ' \" ; $ ` \\ | < > &，设备会悄悄删掉这些字符",
            )));
        }
        if !pw.bytes().all(|b| (0x20..0x7f).contains(&b)) {
            return Err(bad(l.pick(
                "The guest password can only use letters, digits and ASCII symbols",
                "访客密码只能用字母、数字和 ASCII 符号",
            )));
        }
        let n = pw.len();
        if n < 8 || n > 64 || (n == 64 && !pw.bytes().all(|b| b.is_ascii_hexdigit())) {
            return Err(bad(l.pick(
                "The guest password needs 8–63 characters (or 64 hex digits)",
                "访客密码要 8–63 个字符（或 64 位十六进制）",
            )));
        }
    } else if !open && !cur.has_password {
        return Err(bad(l.pick(
            "Set a password for the guest network, or pick Open",
            "请给访客网络设密码，或选「开放」",
        )));
    }
    // One band's guest beside both main networks makes exactly three APs, which this firmware
    // cannot run (luyaz_wifi::layout, upstream #181): Wi-Fi goes down. Both bands or neither, as
    // the stock page does; a layout already like that is left alone.
    if want.on_2g != want.on_5g && (want.on_2g, want.on_5g) != (cur.on_2g, cur.on_5g) {
        return Err(bad(l.pick(
            "Turn the guest network on for both bands, or off: with one band only, this device runs three networks and Wi-Fi goes down",
            "访客网络请两个频段一起开或一起关：只开一个频段，设备就要跑三个网络，Wi-Fi 会整个断掉",
        )));
    }
    if !pw.is_empty() {
        params.insert("guest_key".into(), json!(pw));
    }
    if params.is_empty() {
        return Err(bad(l.pick(
            "No changes yet. Change a setting, then apply.",
            "还没有改动。改了设置再按「应用」。",
        )));
    }
    if !expect.is_empty() {
        params.insert("expect".into(), Value::Object(expect));
    }
    let (what, reconnect) = describe(cur, &want, !pw.is_empty(), l);
    Ok(Save {
        params,
        want,
        key_sent: !pw.is_empty(),
        what,
        reconnect,
    })
}

/// The dialog's "what happens" and the reconnect line (the old page's words).
fn describe(b: &Guest, f: &Guest, key: bool, l: Lang) -> (String, Option<String>) {
    let name = match l {
        Lang::En => format!("“{}”", f.ssid),
        Lang::Zh => format!("「{}」", f.ssid),
    };
    let mut lines: Vec<String> = Vec::new();
    let (was, is) = (b.on(), f.on());
    if !was && is {
        lines.push(match l {
            Lang::En => format!("The guest network {name} will be enabled in settings."),
            Lang::Zh => format!("访客网络{name}将设置为开启。"),
        });
    } else if was && !is {
        lines.push(
            l.pick(
                "The guest network goes off; devices on it drop.",
                "访客网络关闭，连着它的设备会断开。",
            )
            .into(),
        );
    } else if b.on_2g != f.on_2g || b.on_5g != f.on_5g {
        lines.push(match l {
            Lang::En => format!("The guest network now uses {}.", bands_text(f)),
            Lang::Zh => format!("访客网络改用 {}。", bands_text(f)),
        });
    }
    let mut name_or_key = false;
    if b.ssid != f.ssid {
        name_or_key = true;
        lines.push(match l {
            Lang::En => format!("The guest network name becomes {name}."),
            Lang::Zh => format!("访客网络名称改为{name}。"),
        });
    }
    if b.encryption != f.encryption {
        name_or_key = true;
        lines.push(if f.encryption == "none" {
            l.pick(
                "The guest network becomes open: when it is available, anyone nearby can join without a password.",
                "访客网络改为开放：网络可用时，附近任何人不用密码就能连。",
            )
            .into()
        } else {
            match l {
                Lang::En => format!("Guest security becomes {}.", enc_label(l, &f.encryption)),
                Lang::Zh => format!("访客加密方式改为 {}。", enc_label(l, &f.encryption)),
            }
        });
    }
    if key {
        name_or_key = true;
        lines.push(
            l.pick("The guest password changes.", "访客密码会改。")
                .into(),
        );
    }
    if b.isolate && !f.isolate {
        lines.push(
            l.pick(
                "Guest devices will be able to reach each other.",
                "访客设备之间将能互相访问。",
            )
            .into(),
        );
    }
    let mut others: Vec<String> = Vec::new();
    if b.hidden != f.hidden {
        others.push(
            if f.hidden {
                l.pick("hide the SSID", "隐藏 SSID")
            } else {
                l.pick("show the SSID", "显示 SSID")
            }
            .into(),
        );
    }
    if !b.isolate && f.isolate {
        others.push(l.pick("AP isolation on", "打开 AP 隔离").into());
    }
    if b.active_time != f.active_time {
        others.push(if f.active_time == 0 {
            l.pick("no time limit", "不限时").into()
        } else {
            match l {
                Lang::En => format!("active for {} min", f.active_time),
                Lang::Zh => format!("有效 {} 分钟", f.active_time),
            }
        });
    }
    if !others.is_empty() {
        lines.push(match l {
            Lang::En => format!("Also changes: {}.", others.join(", ")),
            Lang::Zh => format!("另外：{}。", others.join("、")),
        });
    }
    lines.push(
        l.pick(
            "All of Wi-Fi reloads: devices on the main network and the guest network drop briefly.",
            "整个 Wi-Fi 会重新加载：主网络和访客网络上的设备都会短暂断开。",
        )
        .into(),
    );
    let reconnect = (is && name_or_key).then(|| match (l, key) {
        (Lang::En, true) => {
            format!("Guest devices must reconnect to {name} with the new password.")
        }
        (Lang::En, false) => format!("Guest devices must reconnect to {name}."),
        (Lang::Zh, true) => format!("访客设备要用新密码重新连接{name}。"),
        (Lang::Zh, false) => format!("访客设备要重新连接{name}。"),
    });
    if let Some(r) = &reconnect {
        lines.push(r.clone());
    }
    (lines.join(if l == Lang::En { " " } else { "" }), reconnect)
}

fn confirm(s: &Save, l: Lang) -> write::Confirm {
    let mut rec = String::new();
    if let Some(r) = &s.reconnect {
        rec.push_str(r);
        rec.push_str(if l == Lang::En { " " } else { "" });
    }
    rec.push_str(l.pick(
        "If you are on the U60's Wi-Fi this page drops too: reconnect to the main Wi-Fi and open the page again.",
        "如果你连的是 U60 的 Wi-Fi，这个页面也会断开：重新连上主 Wi-Fi，再打开这个页面。",
    ));
    write::Confirm {
        tier: write::Tier::Dialog,
        title: l
            .pick("Apply the guest Wi-Fi settings?", "应用访客 Wi-Fi 设置？")
            .into(),
        what: s.what.clone(),
        downtime: Some(
            l.pick(
                "About 15–60 seconds while all of Wi-Fi reloads, main network included.",
                "整个 Wi-Fi（包括主网络）重新加载，约 15–60 秒。",
            )
            .into(),
        ),
        recovery: Some(rec),
        action: l.pick("Apply", "应用").into(),
        cuts_uplink: true,
        word: None,
        danger: false,
        revert: crate::write::Revert::No,
    }
}

/// Filled forms waiting for their confirm, by a random token (only the server knows the password).
fn stash() -> &'static Mutex<HashMap<String, (Instant, Save)>> {
    static S: std::sync::OnceLock<Mutex<HashMap<String, (Instant, Save)>>> =
        std::sync::OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

fn put(s: Save) -> String {
    let t = crate::auth::random_id();
    let mut m = stash().lock().unwrap();
    m.retain(|_, (at, _)| at.elapsed() < STASH_TTL);
    m.insert(t.clone(), (Instant::now(), s));
    t
}

fn take(t: &str) -> Option<Save> {
    let mut m = stash().lock().unwrap();
    m.remove(t)
        .filter(|(at, _)| at.elapsed() < STASH_TTL)
        .map(|(_, s)| s)
}

/// Sent changes by token: the `op_id` and the readback (no password), for the lost-reply post.
fn sent() -> &'static Mutex<HashMap<String, (Instant, String, Save)>> {
    static S: std::sync::OnceLock<Mutex<HashMap<String, (Instant, String, Save)>>> =
        std::sync::OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

fn remember(t: &str, op_id: &str, s: &Save) {
    let mut s = s.clone();
    s.params.remove("guest_key");
    let mut m = sent().lock().unwrap();
    m.retain(|_, (at, _, _)| at.elapsed() < STASH_TTL);
    m.insert(t.into(), (Instant::now(), op_id.into(), s));
}

fn recall(t: &str) -> Option<(String, Save)> {
    let m = sent().lock().unwrap();
    m.get(t)
        .filter(|(at, _, _)| at.elapsed() < STASH_TTL)
        .map(|(_, id, s)| (id.clone(), s.clone()))
}

// ---- the send ----

/// How u60d answered `wifi.guest`.
#[derive(Debug, PartialEq)]
pub enum Reply {
    /// Took it: a transaction (`op`) or not; `Sent::NoReply` when the answer never came.
    Took(write::Sent, bool),
    /// Nothing to change (plain, no transaction).
    NoChanges,
}

/// u60d's reply as the page says it: Err = not sent / refused, with u60d's words.
pub fn reply_of(status: u16, v: &Value, l: Lang) -> Result<Reply, write::Outcome> {
    if v["ok"].as_bool() == Some(true) {
        if v["op"]["op_id"].is_string() {
            return Ok(Reply::Took(write::Sent::Ok, true));
        }
        if v["result"]["note"].as_str() == Some("no changes") {
            return Ok(Reply::NoChanges);
        }
        return Ok(Reply::Took(write::Sent::Ok, false));
    }
    let code = v["error"]["code"].as_str().unwrap_or("");
    let msg = v["error"]["message"].as_str().filter(|s| !s.is_empty());
    if matches!(code, "busy" | "op_busy") {
        let doing = &v["doing"];
        let say = crate::changes::pick(doing, "say", l);
        if doing.is_object() && !say.is_empty() {
            return Err(write::Outcome::NotYet(match l {
                Lang::En => format!(
                    "Not applied: another change is in progress ({say}); try again when it ends"
                ),
                Lang::Zh => format!("没有生效：另一项改动正在进行（{say}），等它结束再试"),
            }));
        }
        if status == 503 {
            return Err(write::Outcome::NotYet(
                l.pick(
                    "Not applied: Wi-Fi is being changed elsewhere; try again shortly",
                    "没有生效：Wi-Fi 正在被别处修改，稍后再试",
                )
                .into(),
            ));
        }
        return Err(write::Outcome::Busy);
    }
    Err(write::Outcome::Failed(match msg {
        Some(m) => crate::wifi_main::u60d_words(m, l),
        None => l.pick("the core service refused", "核心服务拒绝了").to_owned(),
    }))
}

async fn send(app: &Shared, body: &Value, l: Lang) -> Result<Reply, write::Outcome> {
    let r = app
        .http
        .post(&app.control_url)
        .json(&crate::pin::stamp(body))
        .timeout(SEND_TIMEOUT)
        .send()
        .await;
    match r {
        Err(e) if e.is_connect() => Err(write::Outcome::Failed(
            l.pick("can't reach the core service", "连不上核心服务")
                .into(),
        )),
        Err(_) => Ok(Reply::Took(write::Sent::NoReply, true)),
        Ok(r) => {
            let status = r.status().as_u16();
            match r.json::<Value>().await {
                Err(_) => Ok(Reply::Took(write::Sent::NoReply, true)),
                Ok(v) => reply_of(status, &v, l),
            }
        }
    }
}

fn applied(l: Lang) -> String {
    l.pick("Guest Wi-Fi settings applied", "访客 Wi-Fi 设置已生效")
        .into()
}

fn area(l: Lang, remote: Via, t: &str, phase: &write::Phase) -> String {
    let (set, cancel) = if t.is_empty() {
        (String::new(), format!("{PATH}/cancel"))
    } else {
        (format!("{PATH}/set?t={t}"), format!("{PATH}/cancel?t={t}"))
    };
    write::Area {
        id: AREA,
        l,
        remote,
        set: &set,
        cancel: &cancel,
        phase,
    }
    .html()
}

fn one(html: String) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(futures_util::stream::once(async move { Ok(patch(&html)) }))
}

/// Why a change can't be asked / sent now (stuck core, not read, a Wi-Fi change running).
fn gate(st: &St, l: Lang) -> Result<Guest, write::Outcome> {
    if st.stuck {
        return Err(write::Outcome::NotYet(
            l.pick(
                "The core service is stuck; changes are paused",
                "核心服务卡住了，暂停修改",
            )
            .into(),
        ));
    }
    if let Some(say) = crate::follow::running(st, &["wifi"], l) {
        return Err(write::Outcome::NotYet(match l {
            Lang::En => format!("A Wi-Fi change is in progress ({say}); try again when it ends"),
            Lang::Zh => format!("Wi-Fi 正在改（{say}），等它结束再试"),
        }));
    }
    guest(st).ok_or_else(|| not_read(l))
}

async fn run(app: Shared, t: String, s: Save, l: Lang, tx: tokio::sync::mpsc::Sender<Event>, receipt: bool) {
    let done = |o: write::Outcome| area(l, Via::Local, &t, &write::Phase::Done(o));
    if let Err(o) = app.hub.read(|st| gate(st, l)) {
        let _ = tx.send(patch(&done(o))).await;
        return;
    }
    let pending = |w: String| area(l, Via::Local, &t, &write::Phase::Pending(w));
    if tx
        .send(patch(&pending(l.pick("Sending…", "正在发送…").into())))
        .await
        .is_err()
    {
        return;
    }
    let op_id = write::next_op_id();
    remember(&t, &op_id, &s);
    // O3: from off the LAN, the core waits for this page to come back (crate::follow) or reverts
    let body = json!({"action": "wifi.guest", "source": "web", "params": s.params, "op_id": op_id, "receipt": receipt});
    let changes = app.hub.watch();
    let o = match send(&app, &body, l).await {
        Err(o) => o,
        Ok(Reply::NoChanges) => write::Outcome::AcceptedSaid(
            l.pick(
                "No changes: the device already has these settings",
                "没有要改的：设备上已经是这些设置",
            )
            .into(),
        ),
        Ok(Reply::Took(sent, txn)) => {
            let s2 = s.clone();
            let Some(f) = crate::follow::follow(
                &app,
                &tx,
                &op_id,
                changes,
                if txn { FOLLOW } else { write::READBACK },
                Duration::ZERO,
                l,
                &pending,
                move |st| guest(st).is_some_and(|g| s2.holds(&g)),
            )
            .await
            else {
                return;
            };
            crate::follow::outcome(&f, sent, applied(l), l)
        }
    };
    let ok = matches!(
        o,
        write::Outcome::Applied(_) | write::Outcome::AcceptedSaid(_)
    ) || matches!(&o, write::Outcome::Ended(_, t) if *t == "ok");
    let mut html = done(o);
    if ok {
        html.push('\n');
        html.push_str(&form_html(false, None, l));
    }
    let _ = tx.send(patch(&html)).await;
}

// ---- the page and its stream ----

pub struct Poll {
    l: Lang,
}

impl Polled for Poll {
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        // nothing of the agent's: the hub's changes re-render (`render`)
        (leaves(app, self.l), Duration::from_secs(3600))
    }
    fn watches(&self, block: &str) -> bool {
        matches!(block, "wifi" | "op")
    }
    fn render(&mut self, app: &Shared) -> Option<Vec<(String, String)>> {
        Some(leaves(app, self.l))
    }
}

#[derive(Template)]
#[template(path = "wifi_guest.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    leaves: Vec<(String, String)>,
    op: String,
}

impl Page {
    fn leaf(&self, id: &str) -> &str {
        self.leaves
            .iter()
            .find(|(k, _)| k == id)
            .map_or("", |(_, h)| h.as_str())
    }
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
    let html = Page {
        shell,
        l,
        leaves: {
            let mut v = leaves(&app, l);
            v.extend(split_leaves(&form_leaves(false, None, l)));
            v
        },
        op: area(l, ctx.remote, "", &write::Phase::Idle),
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
    let l = ctx.lang;
    crate::agent::stream(app, ctx, q.g, PATH, Poll { l })
}

/// Retry: reconnect to u60d now.
pub async fn refresh(State(app): State<Shared>) -> axum::http::StatusCode {
    app.hub.retry();
    let _ = app.kick.send(PATH);
    axum::http::StatusCode::NO_CONTENT
}

#[derive(Template)]
#[template(path = "wifi_guest_form.html")]
struct Form<'a> {
    l: Lang,
    open: bool,
    g: Option<&'a Guest>,
    /// The `data-signals` object (JSON; askama escapes it for the attribute).
    sigs: String,
    /// The current encryption is none of [`ENCS`]: offered as it is.
    odd_enc: Option<String>,
}

impl Form<'_> {
    fn on(&self, k: &str) -> bool {
        self.g.is_some_and(|g| match k {
            "2" => g.on_2g,
            "5" => g.on_5g,
            "hid" => g.hidden,
            _ => g.isolate,
        })
    }
    fn has_pw(&self) -> bool {
        self.g.is_some_and(|g| g.has_password)
    }
    fn encs(&self) -> Vec<(&'static str, &'static str)> {
        ENCS.iter().map(|e| (e.0, self.l.pick(e.1, e.2))).collect()
    }
}

/// The toggle button (`wg-btn`) and the form (`wg-form`), open (at the guest part's values) or
/// closed, as two leaves (the page places them apart).
fn form_leaves(open: bool, g: Option<&Guest>, l: Lang) -> String {
    let sigs = g.map_or_else(String::new, |g| g.signals().to_string());
    let odd_enc = g
        .filter(|g| !ENCS.iter().any(|e| e.0 == g.encryption))
        .map(|g| g.encryption.clone());
    Form {
        l,
        open: open && g.is_some(),
        g,
        sigs,
        odd_enc,
    }
    .render()
    .unwrap()
}

/// The same as one patch.
fn form_html(open: bool, g: Option<&Guest>, l: Lang) -> String {
    form_leaves(open, g, l).replace("<!--leaf-->", "\n")
}

/// 「更改」: the form at the current values (or why not, in the area).
pub async fn form_open(
    State(app): State<Shared>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let html = app.hub.read(|st| match gate(st, l) {
        Ok(g) => format!(
            "{}\n{}",
            form_html(true, Some(&g), l),
            area(l, ctx.remote, "", &write::Phase::Idle)
        ),
        Err(o) => area(l, ctx.remote, "", &write::Phase::Done(o)),
    });
    one(html)
}

pub async fn form_close(req: axum::extract::Request) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    one(form_html(false, None, l))
}

/// 「应用」: check the form (its signals are the body), keep it under a token, ask (tier 3).
pub async fn ask(State(app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let body = axum::body::to_bytes(req.into_body(), 16 * 1024)
        .await
        .unwrap_or_default();
    let f: Value = serde_json::from_slice(&body).unwrap_or_default();
    let html = app.hub.read(
        |st| match gate(st, l).and_then(|g| check(&f, &g, l)) {
            Err(o) => area(l, ctx.remote, "", &write::Phase::Done(o)),
            Ok(s) => {
                let mut c = confirm(&s, l);
                crate::follow::receipt_note(&mut c, ctx.remote, l);
                let kind = if s.reconnect.is_some() {
                    luyaz_api::execution::wifi_change::Change::Settings
                } else {
                    luyaz_api::execution::wifi_change::Change::OnOff
                };
                crate::follow::remote_risk_note(
                    &mut c,
                    ctx.remote,
                    luyaz_api::execution::wifi_change::Ap::Guest,
                    kind,
                    None,
                    l,
                );
                let t = put(s);
                area(l, ctx.remote, &t, &write::Phase::Ask(c))
            }
        },
    );
    one(html)
}

#[derive(Deserialize, Default)]
pub struct TokenQuery {
    #[serde(default)]
    t: String,
    #[serde(default)]
    lost: bool,
}

pub async fn set(
    State(app): State<Shared>,
    Query(q): Query<TokenQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let (l, receipt) = (ctx.lang, ctx.remote != Via::Local);
    let save = take(&q.t);
    let t = q.t.clone();
    Sse::new(async_stream(move |tx| async move {
        let Some(s) = save else {
            let o = write::Outcome::NotYet(
                l.pick(
                    "The form expired; press Apply again",
                    "表单已过期，请再按一次「应用」",
                )
                .into(),
            );
            let _ = tx
                .send(patch(&area(l, Via::Local, "", &write::Phase::Done(o))))
                .await;
            return;
        };
        run(app, t, s, l, tx, receipt).await;
    }))
}

/// Cancel / Esc, and the lost-reply post (`lost=true`, retried by Datastar until the browser is
/// back): for a change that was sent, how its transaction went (following it if still running).
pub async fn cancel(
    State(app): State<Shared>,
    Query(q): Query<TokenQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let remote = ctx.remote;
    let t = q.t.clone();
    let lost = q.lost;
    Sse::new(async_stream(move |tx| async move {
        if !lost {
            take(&t);
            let _ = tx
                .send(patch(&area(l, remote, "", &write::Phase::Idle)))
                .await;
            return;
        }
        let Some((op_id, s)) = recall(&t) else {
            let _ = tx
                .send(patch(&area(
                    l,
                    remote,
                    "",
                    &write::Phase::Done(write::Outcome::Unknown),
                )))
                .await;
            return;
        };
        let changes = app.hub.watch();
        let pending = |w: String| area(l, remote, &t, &write::Phase::Pending(w));
        let window = match app.hub.read(|st| crate::changes::seen(st, &op_id)) {
            Seen::Live(_) => FOLLOW,
            _ => Duration::ZERO,
        };
        let s2 = s.clone();
        let Some(f) = crate::follow::follow(
            &app,
            &tx,
            &op_id,
            changes,
            window,
            Duration::ZERO,
            l,
            &pending,
            move |st| guest(st).is_some_and(|g| s2.holds(&g)),
        )
        .await
        else {
            return;
        };
        let o = crate::follow::outcome(&f, write::Sent::NoReply, applied(l), l);
        let mut html = area(l, remote, "", &write::Phase::Done(o.clone()));
        if matches!(o, write::Outcome::Applied(_))
            || matches!(&o, write::Outcome::Ended(_, t) if *t == "ok")
        {
            html.push('\n');
            html.push_str(&form_html(false, None, l));
        }
        let _ = tx.send(patch(&html)).await;
    }))
}

#[cfg(test)]
mod tests;
