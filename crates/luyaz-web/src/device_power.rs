//! /router/device (设备控制): battery state,
//! battery details, the charge limit, "stop charging" / power-save / fast boot, reboot, power off
//! and factory reset.
//!
//! ```text
//!  agent (tab's own session; u60d first for the 5c paths, else "not mine" → the agent)
//!   /api/battery               every 5 s   ─▶ dc-battery (details + estimate; plugged = hub battery block)
//!   /api/device/charge-control every 30 s  ─▶ dc-status, dc-cap, dc-modes (stop charging), dc-form (once)
//!   POST /api/device/power-save, /api/device/fast-boot
//!                              on open, after a write, on 重试 ─▶ dc-modes
//!  charge limit 「应用」 ─▶ charge/ask (from the signals _cl*) ─▶ tier 2 inline ─▶ charge/set ─▶ PUT, read back
//!  停止充电 / 省电模式 / 快速启动 ─▶ mode/ask ─▶ tier 2 inline ─▶ mode/set ─▶ PUT, read back (no optimistic flip)
//!  重启 / 关机 / 恢复出厂 ─▶ rst/ask ─▶ tier 3 dialog (reset: typed word) ─▶ rst/set ─▶ POST
//! ```
//! - The charge-limit form is a draft in the tab (signals `_cl*`, baseline `_c0*`); the stream sends
//!   it once (skeleton → form) and never again, so a push cannot wipe what is typed. It follows the
//!   device only on open, and after its own 「应用」. The status block always shows the device value.
//! - One change at a time: a second confirmed write while one runs says "another change is in
//!   progress" (the old page disabled every control while one ran).
//! - luyaz-web runs on the device, so reboot / power off / factory reset cannot be read back: the
//!   result is "accepted by the device" with how to get back, or "connection dropped", never "success".
//!   The typed word of the reset is checked here again before anything is sent.
//! - Power-save is written in the object form (`deviceInfoList: {power_saver_mode}`), the one the
//!   firmware took on 9-26; the inventory's array form never did.
//! - Deliberately absent: any ZTE firmware update (FOTA) control.

use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use askama::Template;
use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::topbar::Via;
use crate::agent::{Caller, Fail, Polled};
use crate::i18n::Lang;
use crate::pages::{Shared, StreamQuery, split_leaves};
use crate::write;

pub const PATH: &str = "/router/device";
const CHARGE: &str = "/api/device/charge-control";
const POWER_SAVE: &str = "/api/device/power-save";
const FAST_BOOT: &str = "/api/device/fast-boot";
const BATTERY_EVERY: Duration = Duration::from_secs(5);
const CHARGE_EVERY: Duration = Duration::from_secs(30);

/// Bumped by every write and every 重试: a stream re-reads the one-shot sources when it moved.
static GEN: AtomicU64 = AtomicU64::new(0);
/// A confirmed write is running.
static BUSY: AtomicBool = AtomicBool::new(false);

struct Guard;
impl Guard {
    fn take() -> Option<Guard> {
        BUSY.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
            .then_some(Guard)
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::SeqCst);
    }
}

pub fn routes() -> Router<Shared> {
    Router::new()
        .route(PATH, get(page))
        .route("/router/device/events", get(events))
        .route("/router/device/refresh", post(refresh))
        .route("/router/device/charge/ask", post(charge_ask))
        .route("/router/device/charge/set", post(charge_set))
        .route("/router/device/charge/cancel", post(charge_cancel))
        .route("/router/device/mode/ask", post(mode_ask))
        .route("/router/device/mode/set", post(mode_set))
        .route("/router/device/mode/cancel", post(mode_cancel))
        .route("/router/device/rst/ask", post(rst_ask))
        .route("/router/device/rst/set", post(rst_set))
        .route("/router/device/rst/cancel", post(rst_cancel))
}

// ---- what the agent says ----

#[derive(Default, Clone)]
pub struct Src {
    pub v: Option<Value>,
    pub err: Option<Fail>,
    pub at: Option<SystemTime>,
}

impl Src {
    fn put(&mut self, r: Result<Value, Fail>) {
        match r {
            Ok(v) => {
                self.v = Some(v);
                self.err = None;
                self.at = Some(SystemTime::now());
            }
            Err(e) => self.err = Some(e),
        }
    }
    /// An older answer is shown, the latest read failed.
    fn stale(&self) -> bool {
        self.v.is_some() && self.err.is_some()
    }
}

#[derive(Default, Clone)]
pub struct Got {
    pub charge: Src,
    pub bat: Src,
    pub ps: Src,
    pub fb: Src,
}

/// "1"/"0" (or 1/0/true/false) → bool; a missing key or an odd shape → unknown.
pub fn flag(v: &Value) -> Option<bool> {
    match v {
        Value::String(s) if s == "1" => Some(true),
        Value::String(s) if s == "0" => Some(false),
        Value::Number(n) => match n.as_i64() {
            Some(1) => Some(true),
            Some(0) => Some(false),
            _ => None,
        },
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

async fn read_ps(app: &Shared, who: &Caller) -> Result<Value, Fail> {
    crate::agent::post(app, who, POWER_SAVE, &json!({"deviceInfoList": ["power_saver_mode"]})).await
}

// ---- the view ----

pub struct Row {
    pub label: String,
    pub sub: Option<String>,
    pub value: String,
    pub mono: bool,
}

pub struct ModeRow {
    pub key: &'static str,
    pub label: &'static str,
    pub sub: String,
    pub on: Option<bool>,
    pub disabled: bool,
}

impl ModeRow {
    /// The value a press asks for.
    pub fn want(&self) -> bool {
        !self.on.unwrap_or(false)
    }
}

pub struct View {
    pub l: Lang,
    // status
    pub tone: &'static str,
    pub state: String,
    pub reason: Option<String>,
    pub meta: Option<String>,
    pub retry: bool,
    // current capacity
    pub cap: Option<Row>,
    // battery details: None = nothing yet; Some(Err(text, retry)) = said; Some(Ok(rows))
    pub bat: Option<Result<Vec<Row>, (String, bool)>>,
    pub bat_stale: bool,
    pub form: String,
    pub modes: Vec<ModeRow>,
    pub mode_err: Option<String>,
}

impl View {
    pub fn symbol(&self) -> &'static str {
        crate::sms::symbol(self.tone)
    }
}

fn int(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_f64().map(|x| x.round() as i64))
}

fn fmt1(x: f64) -> String {
    format!("{:.1}", (x * 10.0).round() / 10.0)
}

fn status_word(s: &str, l: Lang) -> String {
    match s {
        "Charging" => l.pick("Charging", "充电中"),
        "Discharging" => l.pick("Discharging", "放电中"),
        "Not charging" => l.pick("Not charging", "未充电"),
        "Full" => l.pick("Full", "已充满"),
        other => return other.to_owned(),
    }
    .to_owned()
}

fn cap_known(c: &Value) -> bool {
    !(int(&c["capacity"]) == Some(0)
        && c["battery_status"].as_str().is_none_or(str::is_empty))
}

/// The status block from `charge-control`.
fn status(g: &Got, l: Lang) -> (&'static str, String, Option<String>, Option<String>, bool) {
    let s = &g.charge;
    let Some(c) = &s.v else {
        return match &s.err {
            Some(e) => (
                "bad",
                l.pick("Can't read the battery state", "读不到电池状态").into(),
                Some(e.text(l)),
                None,
                true,
            ),
            None => (
                "neutral",
                l.pick("Reading battery…", "正在读取电池…").into(),
                None,
                None,
                false,
            ),
        };
    };
    let known = cap_known(c);
    let cap = if known {
        format!("{}%", int(&c["capacity"]).unwrap_or(0))
    } else {
        "—".into()
    };
    let limit_on = c["charge_limit_enabled"].as_bool() == Some(true);
    let limit = int(&c["charge_limit"]).unwrap_or(0);
    let hyst = int(&c["hysteresis"]).unwrap_or(0);
    let (mut tone, state, reason) = if c["charging_stopped"].as_bool() == Some(true) {
        (
            "warn",
            match l {
                Lang::En => format!("Battery {cap} · charging stopped"),
                Lang::Zh => format!("电量 {cap} · 已停止充电"),
            },
            if limit_on {
                match l {
                    Lang::En => format!("The hardware has paused charging. The limit starts it again below {}%.", (limit - hyst).max(0)),
                    Lang::Zh => format!("硬件已暂停充电。电量低于 {}% 时按上限重新开始充。", (limit - hyst).max(0)),
                }
            } else {
                l.pick(
                    "The hardware has paused charging.",
                    "硬件已暂停充电。",
                )
                .into()
            },
        )
    } else {
        let state = if known {
            match l {
                Lang::En => format!("Battery {cap}"),
                Lang::Zh => format!("电量 {cap}"),
            }
        } else {
            l.pick("Battery level not readable", "读不到电量").into()
        };
        let mut parts = Vec::new();
        if let Some(b) = c["battery_status"].as_str().filter(|b| !b.is_empty()) {
            parts.push(match l {
                Lang::En => format!("Status: {}", status_word(b, l)),
                Lang::Zh => format!("状态：{}", status_word(b, l)),
            });
        }
        parts.push(if limit_on {
            match l {
                Lang::En => format!("Charge limit {limit}%"),
                Lang::Zh => format!("充电上限 {limit}%"),
            }
        } else {
            l.pick("No charge limit", "没有充电限制").into()
        });
        (if known { "ok" } else { "warn" }, state, parts.join(" · "))
    };
    let stale = s.stale();
    if stale {
        tone = "stale";
    }
    (
        tone,
        state,
        Some(reason),
        stale.then(|| crate::agent::stopped(l, "Numbers", "数字", s.at)),
        s.err.is_some(),
    )
}

fn battery_rows(b: &Value, plugged: Option<bool>, l: Lang) -> Vec<Row> {
    let f = |k: &str| b[k].as_f64();
    let watts = |uv: Option<f64>, ua: Option<f64>| Some(uv? / 1e6 * (ua? / 1e6));
    let batt_w = watts(f("voltage_uv"), f("current_ua"));
    let ch = &b["charger"];
    let input_w = (ch["online"].as_bool() == Some(true) && ch["current_ua"].as_f64().unwrap_or(0.0) > 0.0)
        .then(|| watts(ch["voltage_uv"].as_f64(), ch["current_ua"].as_f64()))
        .flatten();
    let system_w = match (input_w, batt_w) {
        (Some(i), Some(b)) => Some((i - b).max(0.0)),
        _ => None,
    };
    let est = crate::home::estimate_text(&b["estimate"], l).unwrap_or_else(|| "—".into());
    let batt = match batt_w {
        Some(w) if w.abs() >= 0.05 => {
            if w > 0.0 {
                match l {
                    Lang::En => format!("In {} W", fmt1(w)),
                    Lang::Zh => format!("充入 {} W", fmt1(w)),
                }
            } else {
                match l {
                    Lang::En => format!("Out {} W", fmt1(-w)),
                    Lang::Zh => format!("放出 {} W", fmt1(-w)),
                }
            }
        }
        _ => "—".into(),
    };
    let unplugged = plugged == Some(false) || (plugged.is_none() && ch["online"].as_bool() != Some(true));
    let input = if unplugged {
        l.pick("Not plugged in", "未接").into()
    } else {
        match input_w {
            Some(w) => format!("{} W", fmt1(w)),
            None => l.pick("No input", "没有输入").into(),
        }
    };
    let full = f("charge_full_uah");
    let design = f("charge_full_design_uah");
    let health = match (full, design) {
        (Some(f), Some(d)) if d > 0.0 => {
            let p = (f / d * 100.0).round() as i64;
            Some(p)
        }
        _ => None,
    };
    let mah = |x: Option<f64>| x.map_or("—".into(), |u| format!("{}", (u / 1000.0).round() as i64));
    let r = |label: &str, sub: Option<&str>, value: String, mono: bool| Row {
        label: label.to_owned(),
        sub: sub.map(str::to_owned),
        value,
        mono,
    };
    let mut rows = vec![
        r(
            l.pick("Estimate", "预估"),
            Some(l.pick(
                "Average current over the last 3 minutes; restarts when the charger is plugged or unplugged",
                "按最近 3 分钟的平均电流算，插拔充电器后重新开始",
            )),
            est,
            false,
        ),
        r(l.pick("Battery", "电池"), None, batt, true),
        r(l.pick("Charger input", "充电器输入"), None, input, true),
    ];
    if let Some(w) = system_w {
        rows.push(r(
            l.pick("Device", "整机"),
            None,
            match l {
                Lang::En => format!("About {} W (incl. conversion loss)", fmt1(w)),
                Lang::Zh => format!("整机约 {} W（含转换损耗）", fmt1(w)),
            },
            true,
        ));
    }
    rows.push(r(
        l.pick("Voltage", "电压"),
        None,
        f("voltage_uv").map_or("—".into(), |v| format!("{:.2} V", v / 1e6)),
        true,
    ));
    rows.push(r(
        l.pick("Current", "电流"),
        None,
        f("current_ua").map_or("—".into(), |v| format!("{} mA", (v / 1000.0).round() as i64)),
        true,
    ));
    rows.push(r(
        l.pick("Temperature", "温度"),
        None,
        f("temperature").map_or("—".into(), |t| format!("{} °C", fmt1(t / 10.0))),
        true,
    ));
    rows.push(r(
        l.pick("Health", "健康度"),
        Some(l.pick("Current full capacity ÷ design capacity", "当前满充容量 ÷ 设计容量")),
        match health {
            None => "—".into(),
            Some(p) if p > 100 => l
                .pick("100%+ (fuel-gauge estimate)", "100%+（电量计估算）")
                .into(),
            Some(p) => format!("{p}%"),
        },
        false,
    ));
    rows.push(r(
        l.pick("Cycles", "循环次数"),
        None,
        int(&b["cycle_count"]).map_or("—".into(), |n| n.to_string()),
        true,
    ));
    rows.push(r(
        l.pick("Full / design capacity", "满充 / 设计容量"),
        None,
        format!("{} / {} mAh", mah(full), mah(design)),
        true,
    ));
    rows
}

/// The charge-limit values the form starts from.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Draft {
    pub on: bool,
    pub limit: i64,
    pub hyst: i64,
}

fn draft_of(c: &Value) -> Draft {
    Draft {
        on: c["charge_limit_enabled"].as_bool() == Some(true),
        limit: int(&c["charge_limit"]).unwrap_or(80),
        hyst: int(&c["hysteresis"]).unwrap_or(5),
    }
}

#[derive(Template)]
#[template(path = "device_power_form.html")]
struct Form {
    l: Lang,
    d: Option<Draft>,
    sigs: String,
}

pub fn form_html(l: Lang, d: Option<Draft>) -> String {
    let sigs = d.map_or_else(String::new, |d| {
        json!({"_cle": d.on, "_cll": d.limit, "_clh": d.hyst,
               "_c0e": d.on, "_c0l": d.limit, "_c0h": d.hyst})
        .to_string()
    });
    Form { l, d, sigs }.render().unwrap()
}

fn mode_rows(g: &Got, l: Lang) -> (Vec<ModeRow>, Option<String>) {
    let c = g.charge.v.as_ref();
    let reading = l.pick("Reading…", "正在读取…");
    let ps = g.ps.v.as_ref().map(|v| flag(&v["power_saver_mode"]));
    let fb = g.fb.v.as_ref().map(|v| flag(&v["fast_boot"]));
    let rows = vec![
        ModeRow {
            key: "dps",
            label: l.pick("Stop charging", "停止充电"),
            sub: if c.is_none() {
                reading.into()
            } else {
                l.pick(
                    "Cuts the charger input: the battery runs the device, even when plugged in",
                    "切断充电输入，改用电池供电；插着电也会掉电",
                )
                .into()
            },
            on: c.map(|c| c["charging_stopped"].as_bool() == Some(true)),
            disabled: c.is_none() || g.charge.stale(),
        },
        ModeRow {
            key: "ps",
            label: l.pick("Power-save mode", "省电模式"),
            sub: match (&ps, &g.ps.err) {
                (None, None) => reading.into(),
                (Some(None), _) => l
                    .pick("The device didn't report this setting.", "设备没有返回这项设置。")
                    .into(),
                _ => String::new(),
            },
            on: ps.flatten(),
            disabled: ps.flatten().is_none() || g.ps.stale(),
        },
        ModeRow {
            key: "fb",
            label: l.pick("Fast boot", "快速启动"),
            sub: match (&fb, &g.fb.err) {
                (None, None) => reading.into(),
                (Some(None), _) => l
                    .pick("The device didn't report this setting.", "设备没有返回这项设置。")
                    .into(),
                _ => l
                    .pick("Restarts faster by leaving out some startup steps", "重启时省掉一些启动步骤，开得更快")
                    .into(),
            },
            on: fb.flatten(),
            disabled: fb.flatten().is_none() || g.fb.stale(),
        },
    ];
    let err = if let Some(e) = &g.ps.err {
        Some(match l {
            Lang::En => format!("Couldn't read power-save mode: {}", e.text(l)),
            Lang::Zh => format!("读不到省电模式：{}", e.text(l)),
        })
    } else {
        g.fb.err.as_ref().map(|e| match l {
            Lang::En => format!("Couldn't read fast boot: {}", e.text(l)),
            Lang::Zh => format!("读不到快速启动：{}", e.text(l)),
        })
    };
    (rows, err)
}

/// Plugged in = the hub's battery block `charger_connect` (no poll of its own).
fn plugged(app: &Shared) -> Option<bool> {
    app.hub.read(|st| {
        let b = st.blocks.get("battery")?;
        let v = &b.data["charger_connect"];
        int(v).map(|n| n != 0)
    })
}

pub fn view(app: &Shared, g: &Got, form: &str, l: Lang) -> View {
    let (tone, state, reason, meta, retry) = status(g, l);
    let cap = g.charge.v.as_ref().map(|c| {
        let known = cap_known(c);
        let t = if known {
            format!("{}%", int(&c["capacity"]).unwrap_or(0))
        } else {
            "—".into()
        };
        Row {
            label: l.pick("Current capacity", "当前电量").into(),
            sub: None,
            value: match c["battery_status"].as_str().filter(|s| !s.is_empty()) {
                Some(s) => format!("{t} · {}", status_word(s, l)),
                None => t,
            },
            mono: false,
        }
    });
    let bat = match (&g.bat.v, &g.bat.err) {
        (Some(b), _) => Some(Ok(battery_rows(b, plugged(app), l))),
        (None, Some(Fail::Refused(503, ..))) => Some(Err((
            l.pick(
                "This device doesn't report battery details",
                "这台设备读不到电池详情",
            )
            .into(),
            false,
        ))),
        (None, Some(_)) => Some(Err((
            l.pick("Can't read battery details right now", "暂时读不到电池详情")
                .into(),
            true,
        ))),
        (None, None) => None,
    };
    let (modes, mode_err) = mode_rows(g, l);
    View {
        l,
        tone,
        state,
        reason,
        meta,
        retry,
        cap,
        bat,
        bat_stale: g.bat.stale(),
        form: form.to_owned(),
        modes,
        mode_err,
    }
}

#[derive(Template)]
#[template(path = "device_power_leaves.html")]
struct Leaves<'a> {
    v: &'a View,
}

fn leaves(app: &Shared, g: &Got, form: &str, l: Lang) -> Vec<(String, String)> {
    let v = view(app, g, form, l);
    split_leaves(&Leaves { v: &v }.render().unwrap())
}

// ---- the page and its stream ----

pub struct Poll {
    who: Caller,
    l: Lang,
    got: Got,
    seen: u64,
    first: bool,
    last_charge: Option<Instant>,
    last_bat: Option<Instant>,
    /// The form is sent once, from the first answer.
    form: Option<String>,
}

impl Poll {
    fn new(who: Caller, l: Lang) -> Poll {
        Poll {
            who,
            l,
            got: Got::default(),
            seen: GEN.load(Ordering::SeqCst),
            first: true,
            last_charge: None,
            last_bat: None,
            form: None,
        }
    }
}

impl Polled for Poll {
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        let now = Instant::now();
        let gen_now = GEN.load(Ordering::SeqCst);
        let moved = gen_now != self.seen;
        self.seen = gen_now;
        let due = |last: Option<Instant>, every: Duration| {
            last.is_none_or(|t| now.duration_since(t) + Duration::from_millis(50) >= every)
        };
        let (charge, bat, once) = (
            moved || due(self.last_charge, CHARGE_EVERY),
            due(self.last_bat, BATTERY_EVERY),
            moved || self.first,
        );
        let who = &self.who;
        let (c, b, p, f) = tokio::join!(
            async {
                if charge {
                    Some(crate::agent::get(app, who, CHARGE).await)
                } else {
                    None
                }
            },
            async {
                if bat {
                    Some(crate::agent::get(app, who, "/api/battery").await)
                } else {
                    None
                }
            },
            async {
                if once {
                    Some(read_ps(app, who).await)
                } else {
                    None
                }
            },
            async {
                if once {
                    Some(crate::agent::get(app, who, FAST_BOOT).await)
                } else {
                    None
                }
            },
        );
        if let Some(r) = c {
            self.last_charge = Some(now);
            self.got.charge.put(r);
        }
        if let Some(r) = b {
            self.last_bat = Some(now);
            self.got.bat.put(r);
        }
        if let Some(r) = p {
            self.got.ps.put(r);
        }
        if let Some(r) = f {
            self.got.fb.put(r);
        }
        self.first = false;
        if self.form.is_none()
            && let Some(c) = &self.got.charge.v
        {
            self.form = Some(form_html(self.l, Some(draft_of(c))));
        }
        let form = self
            .form
            .clone()
            .unwrap_or_else(|| form_html(self.l, None));
        (leaves(app, &self.got, &form, self.l), BATTERY_EVERY)
    }
}

#[derive(Template)]
#[template(path = "device_power.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    leaves: Vec<(String, String)>,
    charge_op: String,
    mode_op: String,
    rst_op: String,
}

impl Page {
    fn leaf(&self, id: &str) -> &str {
        self.leaves
            .iter()
            .find(|(k, _)| k == id)
            .map_or("", |(_, h)| h.as_str())
    }
}

/// The write areas: `dc-charge` (the limit), `dc-mode` (the three switches), `dc-rst` (the dialogs).
fn area(id: &str, l: Lang, remote: Via, set: &str, phase: &write::Phase) -> String {
    let cancel = format!("{PATH}/{}/cancel", &id[3..]);
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
        // the first values come with the stream (all agent reads run in parallel there)
        leaves: leaves(&app, &Got::default(), &form_html(l, None), l),
        charge_op: area("dc-charge", l, ctx.remote, "", &idle),
        mode_op: area("dc-mode", l, ctx.remote, "", &idle),
        rst_op: area("dc-rst", l, ctx.remote, "", &idle),
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
    let p = Poll::new(Caller::of(&app, &req), ctx.lang);
    crate::agent::stream(app, ctx, q.g, PATH, p)
}

fn kick(app: &Shared) {
    GEN.fetch_add(1, Ordering::SeqCst);
    let _ = app.kick.send(PATH);
}

/// 重试: every open tab reads again.
pub async fn refresh(State(app): State<Shared>) -> StatusCode {
    kick(&app);
    StatusCode::NO_CONTENT
}

fn one(html: String) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(futures_util::stream::once(async move {
        Ok(crate::pages::patch(&html))
    }))
}

// ---- writes ----

/// Look until `ok` holds (three looks a second apart); None = nothing could be read.
async fn read_back<F, Fut>(mut read: F, ok: impl Fn(&Value) -> bool) -> Option<bool>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Value, Fail>>,
{
    let mut seen = false;
    for i in 0..3 {
        if i > 0 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        if let Ok(v) = read().await {
            seen = true;
            if ok(&v) {
                return Some(true);
            }
        }
    }
    seen.then_some(false)
}

/// One write's reply and its readback → the outcome.
fn judged(sent: Result<Value, Fail>, held: Option<bool>, applied: String, l: Lang) -> write::Outcome {
    match (sent, held) {
        (Err(Fail::Unreachable), Some(true)) | (Ok(_), Some(true)) => write::Outcome::Applied(applied),
        (Err(Fail::Unreachable), _) => write::Outcome::Unknown,
        (Err(e), _) => write::Outcome::Failed(e.text(l)),
        (Ok(_), _) => write::Outcome::Accepted,
    }
}

fn busy_area(id: &str, l: Lang) -> String {
    area(id, l, Via::Local, "", &write::Phase::Done(write::Outcome::Busy))
}

fn on_off(l: Lang, on: bool, en_on: &str, en_off: &str, zh_on: &str, zh_off: &str) -> String {
    match (l, on) {
        (Lang::En, true) => en_on,
        (Lang::En, false) => en_off,
        (Lang::Zh, true) => zh_on,
        (Lang::Zh, false) => zh_off,
    }
    .into()
}

// -- the charge limit --

#[derive(Deserialize, Default)]
pub struct ChargeIn {
    #[serde(default, rename = "_cle")]
    on: Value,
    #[serde(default, rename = "_cll")]
    limit: Value,
    #[serde(default, rename = "_clh")]
    hyst: Value,
}

fn num(v: &Value) -> Option<i64> {
    match v {
        Value::Number(_) => int(v),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// The draft as sent, if it is in range (50–100 / 1–20, the agent's).
pub fn check(f: &ChargeIn) -> Option<Draft> {
    let (limit, hyst) = (num(&f.limit)?, num(&f.hyst)?);
    let on = match &f.on {
        Value::Bool(b) => *b,
        Value::String(s) => s == "true",
        _ => return None,
    };
    ((50..=100).contains(&limit) && (1..=20).contains(&hyst)).then_some(Draft { on, limit, hyst })
}

async fn body_of(req: axum::extract::Request) -> Vec<u8> {
    axum::body::to_bytes(req.into_body(), 16 * 1024)
        .await
        .map(|b| b.to_vec())
        .unwrap_or_default()
}

pub async fn charge_ask(State(app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let who = Caller::of(&app, &req);
    let f: ChargeIn = serde_json::from_slice(&body_of(req).await).unwrap_or_default();
    let done = |o| area("dc-charge", l, ctx.remote, "", &write::Phase::Done(o));
    let Some(d) = check(&f) else {
        return one(done(write::Outcome::NotYet(
            l.pick(
                "The limit must be 50–100% and the hysteresis 1–20%.",
                "充电上限要在 50–100%，回差要在 1–20%。",
            )
            .into(),
        )));
    };
    if let Err(e) = crate::agent::get(&app, &who, CHARGE).await {
        return one(done(write::Outcome::Failed(e.text(l))));
    }
    let c = write::Confirm {
        tier: write::Tier::Inline,
        title: l.pick("Apply the charge limit", "应用充电限制").into(),
        what: if d.on {
            match l {
                Lang::En => format!("Charging stops at {}% and starts again below {}%.", d.limit, d.limit - d.hyst),
                Lang::Zh => format!("充到 {}% 停止充电，低于 {}% 再开始充。", d.limit, d.limit - d.hyst),
            }
        } else {
            l.pick(
                "The limit is removed; the battery charges to 100%.",
                "取消充电限制，电池会充到 100%。",
            )
            .into()
        },
        downtime: None,
        recovery: None,
        action: l.pick("Confirm: apply", "确认：应用").into(),
        cuts_uplink: false,
        word: None,
        danger: false,
        revert: crate::write::Revert::No,
    };
    let set = format!("{PATH}/charge/set?e={}&l={}&h={}", d.on, d.limit, d.hyst);
    one(area("dc-charge", l, ctx.remote, &set, &write::Phase::Ask(c)))
}

#[derive(Deserialize, Default)]
pub struct ChargeQ {
    #[serde(default)]
    e: bool,
    #[serde(default)]
    l: i64,
    #[serde(default)]
    h: i64,
}

pub async fn charge_set(
    State(app): State<Shared>,
    Query(q): Query<ChargeQ>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let who = Caller::of(&app, &req);
    let pending = area(
        "dc-charge",
        l,
        Via::Local,
        "",
        &write::Phase::Pending(l.pick("Applying…", "正在应用…").into()),
    );
    write::respond(pending, async move {
        let Some(_g) = Guard::take() else {
            return busy_area("dc-charge", l);
        };
        let d = Draft {
            on: q.e,
            limit: q.l,
            hyst: q.h,
        };
        if !(50..=100).contains(&d.limit) || !(1..=20).contains(&d.hyst) {
            return area("dc-charge", l, Via::Local, "", &write::Phase::Done(write::Outcome::Failed(
                l.pick("limit out of range", "数值超出范围").into(),
            )));
        }
        let o = match crate::agent::get(&app, &who, CHARGE).await {
            Err(e) => write::Outcome::Failed(e.text(l)),
            Ok(_) => {
                let sent = crate::agent::send_json(
                    &app,
                    &who,
                    reqwest::Method::PUT,
                    CHARGE,
                    &json!({"charge_limit_enabled": d.on, "charge_limit": d.limit, "hysteresis": d.hyst}),
                )
                .await;
                let held = read_back(
                    || crate::agent::get(&app, &who, CHARGE),
                    |c| {
                        c["charge_limit_enabled"].as_bool() == Some(d.on)
                            && (!d.on
                                || (int(&c["charge_limit"]) == Some(d.limit)
                                    && int(&c["hysteresis"]) == Some(d.hyst)))
                    },
                )
                .await;
                judged(
                    sent,
                    held,
                    if d.on {
                        match l {
                            Lang::En => format!("Charge limit set: {}%, back on below {}%", d.limit, d.limit - d.hyst),
                            Lang::Zh => format!("已设置充电上限 {}%，低于 {}% 再充", d.limit, d.limit - d.hyst),
                        }
                    } else {
                        l.pick("Charge limit removed", "已取消充电限制").into()
                    },
                    l,
                )
            }
        };
        kick(&app);
        let applied = matches!(o, write::Outcome::Applied(_));
        let res = area("dc-charge", l, Via::Local, "", &write::Phase::Done(o));
        if applied {
            // the form starts again from what the device now says (draft = device)
            format!("{}\n{res}", form_html(l, Some(d)))
        } else {
            res
        }
    })
}

pub async fn charge_cancel(
    Query(q): Query<write::CancelQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    one(area("dc-charge", ctx.lang, ctx.remote, "", &write::cancel_phase(&q)))
}

// -- stop charging / power-save / fast boot --

#[derive(Deserialize, Default)]
pub struct ModeQ {
    #[serde(default)]
    k: String,
    #[serde(default)]
    on: bool,
}

fn mode_key(k: &str) -> Option<&'static str> {
    ["dps", "ps", "fb"].into_iter().find(|x| *x == k)
}

pub async fn mode_ask(
    State(app): State<Shared>,
    Query(q): Query<ModeQ>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let who = Caller::of(&app, &req);
    let done = |o| area("dc-mode", l, ctx.remote, "", &write::Phase::Done(o));
    let Some(k) = mode_key(&q.k) else {
        return one(area("dc-mode", l, ctx.remote, "", &write::Phase::Idle));
    };
    // the state as it is now: a switch the device didn't report is never written
    let known = match k {
        "dps" => crate::agent::get(&app, &who, CHARGE).await.map(|c| Some(c)),
        "ps" => read_ps(&app, &who).await.map(|v| flag(&v["power_saver_mode"]).map(|_| v)),
        _ => crate::agent::get(&app, &who, FAST_BOOT)
            .await
            .map(|v| flag(&v["fast_boot"]).map(|_| v)),
    };
    let cur = match known {
        Err(e) => return one(done(write::Outcome::Failed(e.text(l)))),
        Ok(None) => {
            return one(done(write::Outcome::NotYet(
                l.pick("The device didn't report this setting.", "设备没有返回这项设置。")
                    .into(),
            )));
        }
        Ok(Some(v)) => v,
    };
    let (action, what) = match k {
        "dps" => (
            on_off(l, q.on, "stop charging", "charge again", "停止充电", "恢复充电"),
            if q.on {
                l.pick(
                    "The charger input is cut and the battery runs the device, so the level drops even when plugged in. The charge limit stays out of it until you change the limit.",
                    "切断充电输入，设备改用电池供电，插着电电量也会往下掉。改充电上限之前，上限不会来接管。",
                )
                .into()
            } else if cur["charge_limit_enabled"].as_bool() == Some(true) {
                let lim = int(&cur["charge_limit"]).unwrap_or(0);
                match l {
                    Lang::En => format!("The charger input comes back. The charge limit is on: above {lim}% it stops charging again by itself."),
                    Lang::Zh => format!("恢复充电输入。充电上限开着：电量在 {lim}% 以上时它会自己再停充。"),
                }
            } else {
                l.pick(
                    "The charger input comes back and the battery charges again.",
                    "恢复充电输入，电池接着充电。",
                )
                .into()
            },
        ),
        "ps" => (
            on_off(l, q.on, "turn power-save on", "turn power-save off", "打开省电模式", "关闭省电模式"),
            on_off(
                l,
                q.on,
                "The device switches to the firmware's power-save mode.",
                "The device leaves power-save mode.",
                "设备切换到固件自带的省电模式。",
                "设备退出省电模式。",
            ),
        ),
        _ => (
            on_off(l, q.on, "turn fast boot on", "turn fast boot off", "打开快速启动", "关闭快速启动"),
            on_off(
                l,
                q.on,
                "The next start skips some init steps. Takes effect at the next reboot.",
                "The next start runs the full init. Takes effect at the next reboot.",
                "下次开机跳过部分初始化步骤，重启后生效。",
                "下次开机走完整初始化，重启后生效。",
            ),
        ),
    };
    let c = write::Confirm {
        tier: write::Tier::Inline,
        title: action.clone(),
        what,
        downtime: None,
        recovery: None,
        action: match l {
            Lang::En => format!("Confirm: {action}"),
            Lang::Zh => format!("确认：{action}"),
        },
        cuts_uplink: false,
        word: None,
        danger: false,
        revert: crate::write::Revert::No,
    };
    let set = format!("{PATH}/mode/set?k={k}&on={}", q.on);
    one(area("dc-mode", l, ctx.remote, &set, &write::Phase::Ask(c)))
}

pub async fn mode_set(
    State(app): State<Shared>,
    Query(q): Query<ModeQ>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let who = Caller::of(&app, &req);
    let pending = area(
        "dc-mode",
        l,
        Via::Local,
        "",
        &write::Phase::Pending(l.pick("Applying…", "正在应用…").into()),
    );
    write::respond(pending, async move {
        let Some(_g) = Guard::take() else {
            return busy_area("dc-mode", l);
        };
        let on = q.on;
        let o = match mode_key(&q.k) {
            None => write::Outcome::Failed(l.pick("unknown switch", "不认识的开关").into()),
            Some("dps") => {
                let sent = crate::agent::send_json(
                    &app,
                    &who,
                    reqwest::Method::PUT,
                    CHARGE,
                    &json!({"charging_stopped": on}),
                )
                .await;
                let held = read_back(
                    || crate::agent::get(&app, &who, CHARGE),
                    |c| c["charging_stopped"].as_bool() == Some(on),
                )
                .await;
                judged(
                    sent,
                    held,
                    on_off(l, on, "Charging stopped", "Charging resumed", "已停止充电", "已恢复充电"),
                    l,
                )
            }
            Some("ps") => {
                // the object form (9-26); the array form never took on the device
                let sent = crate::agent::send_json(
                    &app,
                    &who,
                    reqwest::Method::PUT,
                    POWER_SAVE,
                    &json!({"deviceInfoList": {"power_saver_mode": if on { "1" } else { "0" }}}),
                )
                .await;
                let held = read_back(
                    || read_ps(&app, &who),
                    |v| flag(&v["power_saver_mode"]) == Some(on),
                )
                .await;
                judged(
                    sent,
                    held,
                    on_off(l, on, "Power-save mode on", "Power-save mode off", "已打开省电模式", "已关闭省电模式"),
                    l,
                )
            }
            Some(_) => {
                let sent = crate::agent::send_json(
                    &app,
                    &who,
                    reqwest::Method::PUT,
                    FAST_BOOT,
                    &json!({"fast_boot": if on { "1" } else { "0" }}),
                )
                .await;
                let held = read_back(
                    || crate::agent::get(&app, &who, FAST_BOOT),
                    |v| flag(&v["fast_boot"]) == Some(on),
                )
                .await;
                judged(
                    sent,
                    held,
                    on_off(l, on, "Fast boot on", "Fast boot off", "已打开快速启动", "已关闭快速启动"),
                    l,
                )
            }
        };
        kick(&app);
        area("dc-mode", l, Via::Local, "", &write::Phase::Done(o))
    })
}

pub async fn mode_cancel(
    Query(q): Query<write::CancelQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    one(area("dc-mode", ctx.lang, ctx.remote, "", &write::cancel_phase(&q)))
}

// -- reboot / power off / factory reset (tier 3) --

#[derive(Deserialize, Default)]
pub struct RstQ {
    #[serde(default)]
    k: String,
    #[serde(default)]
    word: String,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Rst {
    Reboot,
    Off,
    Reset,
}

fn rst_key(k: &str) -> Option<Rst> {
    match k {
        "reboot" => Some(Rst::Reboot),
        "off" => Some(Rst::Off),
        "reset" => Some(Rst::Reset),
        _ => None,
    }
}

impl Rst {
    fn key(self) -> &'static str {
        match self {
            Rst::Reboot => "reboot",
            Rst::Off => "off",
            Rst::Reset => "reset",
        }
    }
    fn path(self) -> &'static str {
        match self {
            Rst::Reboot => "/api/device/reboot",
            Rst::Off => "/api/device/poweroff",
            Rst::Reset => "/api/device/factory-reset",
        }
    }
}

/// The word typed to confirm a factory reset, in the page language.
pub fn reset_word(l: Lang) -> &'static str {
    l.pick("factory reset", "恢复出厂")
}

fn reboot_recovery(l: Lang) -> &'static str {
    l.pick(
        "Wait a little longer, then reload this page. If it still doesn't load, check on the touchscreen that the device has started, and join its Wi-Fi again.",
        "再等一会儿，然后刷新这个页面。还是打不开的话，在触屏上看设备是否已经开机，再重新连上它的 Wi-Fi。",
    )
}

fn reset_recovery(l: Lang) -> &'static str {
    l.pick(
        "This page will not come back: the reset removes this manager. The device returns with its factory Wi-Fi name, password and address (printed on its label). Join that Wi-Fi and run the install kit again to get this project's software back.",
        "这个页面不会再回来：恢复出厂会删掉这个管理后台。设备重启后用出厂的 Wi-Fi 名称、密码和地址（印在机身标签上）。连上那个 Wi-Fi，重新运行装机包，才能装回本项目的软件。",
    )
}

fn rst_confirm(k: Rst, l: Lang) -> write::Confirm {
    match k {
        Rst::Reboot => write::Confirm {
            tier: write::Tier::Dialog,
            title: l.pick("Reboot the device?", "重启设备？").into(),
            what: l
                .pick(
                    "The U60 restarts. Wi-Fi, the mobile connection and this page drop until it is back.",
                    "U60 会重新启动。重启期间 Wi-Fi、移动网络和这个页面都会断开。",
                )
                .into(),
            downtime: Some(
                l.pick(
                    "About 90 seconds. This page reconnects by itself when it is back; you may need to sign in again.",
                    "约 90 秒。页面会在设备回来后自动重连；可能需要重新登录。",
                )
                .into(),
            ),
            recovery: Some(reboot_recovery(l).into()),
            action: l.pick("Confirm Reboot", "确认重启").into(),
            cuts_uplink: true,
            word: None,
            danger: false,
            revert: crate::write::Revert::No,
        },
        Rst::Off => write::Confirm {
            tier: write::Tier::Dialog,
            title: l.pick("Power off the device?", "要关机吗？").into(),
            what: l
                .pick(
                    "The U60 shuts down. Wi-Fi, the mobile connection and this page are gone until someone turns it on.",
                    "U60 会关掉。Wi-Fi、蜂窝网络和这个页面都没了，直到有人把它打开。",
                )
                .into(),
            downtime: Some(
                l.pick(
                    "Until someone presses and holds the power button on the device. It can't be turned on from here or remotely.",
                    "一直到有人在设备上长按电源键。这里和远程都开不了机。",
                )
                .into(),
            ),
            recovery: Some(
                l.pick(
                    "Press and hold the power button on the device for about 3 seconds.",
                    "在设备上长按电源键约 3 秒。",
                )
                .into(),
            ),
            action: l.pick("Power Off", "关机").into(),
            cuts_uplink: true,
            word: None,
            danger: false,
            revert: crate::write::Revert::No,
        },
        Rst::Reset => {
            let what = l.pick(
                "Every setting is erased — Wi-Fi, APN, locks, SMS forwarding — and so is this project's software: this manager, the touchscreen interface, CHILL and Tailscale. This cannot be undone.",
                "所有设置都会被清除（Wi-Fi、APN、锁频锁小区、短信转发等），本项目的软件也会一起删掉：这个管理后台、触屏界面、CHILL 和 Tailscale。此操作无法撤销。",
            );
            write::Confirm {
                tier: write::Tier::Dialog,
                title: l
                    .pick(
                        "Wipe everything back to the factory state?",
                        "全部清空，恢复出厂？",
                    )
                    .into(),
                what: what.into(),
                downtime: Some(
                    l.pick(
                        "The device restarts; this page does not come back.",
                        "设备会重启；这个页面不会再回来。",
                    )
                    .into(),
                ),
                recovery: Some(reset_recovery(l).into()),
                action: l.pick("Reset Now", "立即重置").into(),
                cuts_uplink: true,
                word: Some(reset_word(l).into()),
                danger: true,
                revert: crate::write::Revert::No,
            }
        }
    }
}

pub async fn rst_ask(
    State(_app): State<Shared>,
    Query(q): Query<RstQ>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let Some(k) = rst_key(&q.k) else {
        return one(area("dc-rst", l, ctx.remote, "", &write::Phase::Idle));
    };
    let set = format!("{PATH}/rst/set?k={}", k.key());
    one(area("dc-rst", l, ctx.remote, &set, &write::Phase::Ask(rst_confirm(k, l))))
}

pub async fn rst_set(
    State(app): State<Shared>,
    Query(q): Query<RstQ>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let who = Caller::of(&app, &req);
    let k = rst_key(&q.k);
    let pending = area(
        "dc-rst",
        l,
        Via::Local,
        "",
        &write::Phase::Pending(
            match k {
                Some(Rst::Reboot) => l.pick("Rebooting… about 90 seconds", "正在重启…约 90 秒"),
                Some(Rst::Off) => l.pick("Powering off…", "正在关机…"),
                _ => l.pick("Resetting…", "正在恢复出厂…"),
            }
            .into(),
        ),
    );
    write::respond(pending, async move {
        let done = |o| area("dc-rst", l, Via::Local, "", &write::Phase::Done(o));
        let Some(k) = k else {
            return done(write::Outcome::Failed(l.pick("unknown action", "不认识的操作").into()));
        };
        // the word is checked again here: a request that skipped the dialog sends nothing
        if k == Rst::Reset && !write::word_ok(reset_word(l), &q.word) {
            return done(write::Outcome::Failed(
                l.pick("the confirmation word did not match", "输入的确认词不对").into(),
            ));
        }
        let Some(_g) = Guard::take() else {
            return busy_area("dc-rst", l);
        };
        let sent = crate::agent::post(&app, &who, k.path(), &json!({})).await;
        kick(&app);
        done(match sent {
            Ok(_) => write::Outcome::AcceptedSaid(match k {
                Rst::Reboot => format!(
                    "{} {}",
                    l.pick(
                        "The device accepted the reboot; it can't be read back from here.",
                        "设备已接受重启，这里读不回结果。",
                    ),
                    reboot_recovery(l)
                ),
                Rst::Off => l
                    .pick(
                        "The device accepted power off. Only the power button on the device turns it back on.",
                        "设备已接受关机。只能在设备上按电源键开机。",
                    )
                    .into(),
                Rst::Reset => format!(
                    "{} {}",
                    l.pick(
                        "The device accepted the factory reset; it can't be read back from here.",
                        "设备已接受恢复出厂，这里读不回结果。",
                    ),
                    reset_recovery(l)
                ),
            }),
            // the link went down with the device: it may well have happened
            Err(Fail::Unreachable) => write::Outcome::Unknown,
            Err(e) => write::Outcome::Failed(e.text(l)),
        })
    })
}

pub async fn rst_cancel(
    Query(q): Query<write::CancelQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    one(area("dc-rst", ctx.lang, ctx.remote, "", &write::cancel_phase(&q)))
}

#[cfg(test)]
mod tests;
