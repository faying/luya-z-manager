//! /usb (USB 模式), the old `usb/page.tsx`: the USB state, the mode switch (tier 3), fast
//! charging (tier 2), the port and charger details. Not `/tools/enable-adb` (still the old page).
//!
//! Where each value comes from (the old page read `/api/usb/status` every 3 s and
//! `/api/device/charger` every 5 s):
//! - u60d blocks: mode = `qos.usb_mode` (the same `zwrt_bsp.usb list` mode; the agent's `mode`
//!   only while the block is missing); fast charging = `powerbank.state` (`zwrt_bsp.powerbank get`,
//!   the setter's own getter; the old page read `otg_powerbank_state` from the charger list, never
//!   checked on the device); "nothing plugged in" = `typec.cc_attch_state` 0 (what u60d's own
//!   `usb.attach_mode` checks; the agent's `typec_cc == no_cc` while the block is missing or
//!   stale); charger connected / type / raw charge status = `battery.charger_connect`,
//!   `charger_type`, `charging` (the charger list's fields, which u60d folds into the battery
//!   block; u60d writes 0 for a field the firmware leaves out, so "—" can read as 「否」/「0」);
//!   direct power = `charger.direct_supply.mode` (only enable/disable; another raw value is "—").
//! - The agent's `/api/usb/status` with the tab's own session, every 3 s: `connect` (cable),
//!   `typec_cc`, `usb2rj45`, and `mode_settable` (false when the firmware has no `zwrt_bsp.usb set`,
//!   as on CN B31; absent when unknown). `/api/device/charger` is not read any more.
//!
//! Writes (web/docs/controls-inventory.md §/usb):
//! - Mode, tier 3: `usb.set {mode}`, read back from `qos.usb_mode` for up to 30 s (the old page's
//!   `waitDevice` 30 s). luyaz-web runs on the device, so the readback does not wait for the
//!   device: only the browser's own link (a computer on USB) can drop. A dropped reply posts
//!   `cancel?want=<mode>&lost=true` (Datastar retries it until the link is back), which looks at
//!   the mode once more before saying 「连接中断…」. u60d's `usb.set` has no firmware check, so the
//!   agent's 501 gate is repeated here: `mode_settable: false` → never sent (ask and send both
//!   re-read it). Unknown → like the old page.
//! - Fast charging, tier 2: `vendor.call` `zwrt_bsp.powerbank set {state}` (u60d's allow-list, the
//!   same call the agent's `/api/usb/powerbank` made), read back from `powerbank.state`. Not sent
//!   while nothing is plugged in (the firmware drops it on unplug).
//! - `usb.attach_mode` (「充电 + 上网」) is chosen on the touch screen only (inventory): not here.

use std::convert::Infallible;
use std::time::{Duration, SystemTime};

use askama::Template;
use axum::extract::{Query, State};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;

use crate::agent::{Caller, Fail, Polled};
use crate::hub::{Hub, Link, St};
use crate::i18n::Lang;
use crate::pages::{Shared, StreamQuery, split_leaves};
use crate::write;

const PATH: &str = "/usb";
const AGENT_EVERY: Duration = Duration::from_secs(3);
/// The old page's `waitDevice` (30 s).
const MODE_READBACK: Duration = Duration::from_secs(30);
pub const MODES: [&str; 3] = ["debug", "mtp", "rndis"];

// ---- what the agent says ----

/// `/api/usb/status` as the page uses it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Port {
    pub connect: Option<bool>,
    pub cc: Option<String>,
    pub mode: Option<String>,
    pub rj45: Option<bool>,
    /// false: the firmware has no setter; None: not known.
    pub settable: Option<bool>,
}

/// 1/"1"/true → true, 0/"0"/false → false, anything else → unknown (the old page's `tri`).
pub fn tri(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => n.as_f64().map(|x| x != 0.0),
        Value::String(s) => match s.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" => Some(true),
            "0" | "false" | "no" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn text(v: &Value) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(Into::into)
}

pub fn port(v: &Value) -> Port {
    Port {
        connect: tri(&v["connect"]),
        cc: text(&v["typec_cc"]),
        mode: text(&v["mode"]),
        rj45: tri(&v["usb2rj45"]),
        settable: v["mode_settable"].as_bool(),
    }
}

/// The last answer, and whether the latest read failed.
#[derive(Clone, Default)]
pub struct Agent {
    pub port: Option<Port>,
    pub fail: Option<Fail>,
    pub at: Option<SystemTime>,
}

impl Agent {
    pub fn update(&mut self, r: Result<Value, Fail>) {
        match r {
            Ok(v) => {
                self.port = Some(port(&v));
                self.fail = None;
                self.at = Some(SystemTime::now());
            }
            Err(f) => self.fail = Some(f),
        }
    }
    pub async fn read(app: &Shared, who: &Caller) -> Agent {
        let mut a = Agent::default();
        a.update(crate::agent::get(app, who, "/api/usb/status").await);
        a
    }
    /// Stopped: an older answer is shown, the latest read failed.
    fn stale(&self) -> bool {
        self.port.is_some() && self.fail.is_some()
    }
}

// ---- what u60d says ----

/// A block's data and whether it is current.
fn block<'a>(st: &'a St, name: &str) -> Option<(&'a Value, bool)> {
    st.blocks
        .get(name)
        .map(|b| (&b.data, b.stale || st.link != Link::Live))
}

/// `qos.usb_mode` (`zwrt_bsp.usb list` mode).
pub fn usb_mode(st: &St) -> Option<String> {
    block(st, "qos").and_then(|(d, _)| text(&d["usb_mode"]))
}

/// `powerbank.state`: 1 on, 0 off.
pub fn powerbank(st: &St) -> Option<bool> {
    match block(st, "powerbank")?.0["state"].as_i64()? {
        1 => Some(true),
        0 => Some(false),
        _ => None,
    }
}

/// Nothing in the USB-C port: u60d `typec.cc_attch_state` 0 when current, else the agent's CC.
pub fn no_cc(st: &St, a: &Agent) -> bool {
    match block(st, "typec") {
        Some((d, false)) if d["cc_attch_state"].is_i64() => d["cc_attch_state"].as_i64() != Some(1),
        _ => a.port.as_ref().and_then(|p| p.cc.as_deref()) == Some("no_cc"),
    }
}

// ---- the view ----

pub struct Row {
    pub id: &'static str,
    pub label: &'static str,
    pub value: String,
    pub sub: Option<&'static str>,
    pub mono: bool,
    /// Device-supplied text.
    pub data: bool,
}

/// A write in flight on this tab's reply (the stream never shows one).
#[derive(Default)]
pub struct Busy {
    pub mode: bool,
    pub pb: Option<bool>,
}

pub struct View {
    pub l: Lang,
    pub tone: &'static str,
    pub state: String,
    pub reason: Option<String>,
    pub meta: Option<String>,
    pub retry: bool,
    /// (mode, label, selected)
    pub modes: Vec<(&'static str, String, bool)>,
    pub mode_dis: bool,
    pub mode_stale: bool,
    /// `mode_settable: false`.
    pub fixed: bool,
    /// A mode that is none of the three ("user" on the B31 device).
    pub other: Option<String>,
    pub pb: Option<bool>,
    pub pb_dis: bool,
    pub pb_stale: bool,
    pub pb_sub: &'static str,
    pub pb_busy: Option<bool>,
    pub port: Vec<Row>,
    pub port_stale: bool,
    pub chg: Vec<Row>,
    pub chg_stale: bool,
    pub chg_err: Option<String>,
}

impl View {
    pub fn symbol(&self) -> &'static str {
        match self.tone {
            "bad" => "■",
            "warn" => "▲",
            _ => "●",
        }
    }
}

fn dash() -> String {
    "—".into()
}

fn yes_no(
    l: Lang,
    v: Option<bool>,
    yes: (&'static str, &'static str),
    no: (&'static str, &'static str),
) -> String {
    match v {
        None => dash(),
        Some(true) => l.pick(yes.0, yes.1).into(),
        Some(false) => l.pick(no.0, no.1).into(),
    }
}

pub fn view(st: &St, a: &Agent, busy: &Busy, l: Lang) -> View {
    let p = a.port.as_ref();
    let a_stale = a.stale();
    let u_mode = usb_mode(st);
    let mode_stale = match block(st, "qos") {
        Some((_, s)) if u_mode.is_some() => s,
        _ => a_stale,
    };
    // the agent's mode only when u60d answered (or is down) without a qos block, not while the
    // hub is still connecting (the page's first render): no flicker from its "rndis" to "user"
    let hub_known = matches!(st.link, Link::Live | Link::Down);
    let mode = u_mode.or_else(|| p.and_then(|p| p.mode.clone()).filter(|_| hub_known));
    let known = mode
        .as_deref()
        .and_then(|m| MODES.iter().find(|k| **k == m).copied());
    let no_cc = no_cc(st, a);
    let cable = p.and_then(|p| p.connect).or(no_cc.then_some(false));
    let pb_block = block(st, "powerbank");
    let pb = powerbank(st);
    let mt = mode.as_deref().map_or_else(dash, str::to_uppercase);

    // status (the old StatusBlock)
    let (mut tone, state, mut reason) = match (p, &a.fail) {
        (None, Some(f)) => (
            "bad",
            l.pick("Can't read the USB state", "读不到 USB 状态")
                .to_string(),
            Some(f.text(l)),
        ),
        (None, None) => (
            "neutral",
            l.pick("Reading USB…", "正在读取 USB…").to_string(),
            None,
        ),
        (Some(_), _) => match cable {
            Some(true) => (
                "ok",
                match l {
                    Lang::En => format!("USB cable attached · {mt}"),
                    Lang::Zh => format!("USB 线已连接 · {mt}"),
                },
                None,
            ),
            Some(false) => (
                "neutral",
                match l {
                    Lang::En => format!("No USB cable · mode {mt}"),
                    Lang::Zh => format!("没插 USB 线 · 模式 {mt}"),
                },
                None,
            ),
            None => (
                "warn",
                match l {
                    Lang::En => format!("USB link state unknown · mode {mt}"),
                    Lang::Zh => format!("USB 连接状态不明 · 模式 {mt}"),
                },
                Some(
                    l.pick(
                        "The device didn't report whether a cable is attached.",
                        "设备没有返回是否插着线。",
                    )
                    .to_string(),
                ),
            ),
        },
    };
    if p.is_some() && pb == Some(true) {
        let note = l.pick(
            "Fast charging is on: the U60 is charging whatever is plugged in.",
            "快速充电宝开着：U60 正在给插在上面的设备充电。",
        );
        reason = Some(match reason {
            Some(r) => format!("{r} {note}"),
            None => note.into(),
        });
    }
    let mut meta = None;
    if a_stale {
        tone = "stale";
        meta = Some(crate::agent::stopped(l, "Numbers", "数字", a.at));
    }
    let retry = a.fail.is_some();

    let fixed = p.and_then(|p| p.settable) == Some(false);
    let pb_stale = pb_block.is_some_and(|(_, s)| s);

    // the USB port group
    let port = vec![
        Row {
            id: "usb-cable",
            label: l.pick("Cable", "线缆"),
            value: if p.is_none() {
                dash()
            } else {
                yes_no(l, cable, ("Attached", "已连接"), ("Detached", "已断开"))
            },
            sub: None,
            mono: false,
            data: false,
        },
        Row {
            id: "usb-cc",
            label: "USB-C CC",
            value: p.and_then(|p| p.cc.clone()).unwrap_or_else(dash),
            sub: None,
            mono: true,
            data: true,
        },
        Row {
            id: "usb-moderow",
            label: l.pick("Mode", "模式"),
            value: mt.clone(),
            sub: None,
            mono: true,
            data: true,
        },
        Row {
            id: "usb-rj45",
            label: l.pick("USB-to-Ethernet adapter", "USB 转网口适配器"),
            value: yes_no(
                l,
                p.and_then(|p| p.rj45),
                ("Present", "有"),
                ("None", "没有"),
            ),
            sub: None,
            mono: false,
            data: false,
        },
    ];

    // the charger group (battery + charger blocks)
    let bat = block(st, "battery");
    let chg = block(st, "charger");
    let num = |k: &str| {
        bat.and_then(|(d, _)| d[k].as_i64())
            .map_or_else(dash, |n| n.to_string())
    };
    let direct = chg.and_then(|(d, _)| text(&d["direct_supply"]["mode"]));
    let chg_rows = vec![
        Row {
            id: "usb-chgconn",
            label: l.pick("Charger connected", "充电器已接入"),
            value: yes_no(
                l,
                bat.and_then(|(d, _)| tri(&d["charger_connect"])),
                ("Yes", "是"),
                ("No", "否"),
            ),
            sub: None,
            mono: false,
            data: false,
        },
        Row {
            id: "usb-chgtype",
            label: l.pick("Charger Type", "充电器类型"),
            value: num("charger_type"),
            sub: None,
            mono: true,
            data: false,
        },
        Row {
            id: "usb-chgstat",
            label: l.pick("Charge status (raw)", "充电状态（原始值）"),
            value: num("charging"),
            sub: None,
            mono: true,
            data: false,
        },
        Row {
            id: "usb-direct",
            label: l.pick("Direct power", "直接供电"),
            sub: match direct.as_deref() {
                Some("enable") => Some(l.pick("charging stopped", "已停止充电")),
                Some("disable") => Some(l.pick("charging allowed", "允许充电")),
                _ => None,
            },
            value: direct.clone().unwrap_or_else(dash),
            mono: true,
            data: false,
        },
    ];
    let chg_err = (bat.is_none() && matches!(st.link, Link::Live | Link::Down)).then(|| {
        let why = match st.link {
            Link::Down => l.pick("can't reach the data service", "连不上数据服务"),
            _ => l.pick(
                "the data service has no charger data",
                "数据服务没有充电器数据",
            ),
        };
        match l {
            Lang::En => format!("Couldn't read the charger: {why}"),
            Lang::Zh => format!("读不到充电器信息：{why}"),
        }
    });

    View {
        l,
        tone,
        state,
        reason,
        meta,
        retry,
        modes: MODES
            .iter()
            .map(|m| (*m, m.to_uppercase(), known == Some(*m)))
            .collect(),
        mode_dis: p.is_none() || a_stale || fixed || busy.mode,
        mode_stale,
        fixed,
        other: mode.filter(|_| known.is_none()),
        pb,
        pb_dis: pb.is_none() || pb_stale || busy.pb.is_some() || no_cc,
        pb_stale,
        pb_sub: if pb.is_none() && pb_block.is_some() {
            l.pick(
                "The device didn't report the power bank state.",
                "设备没有返回充电宝状态。",
            )
        } else if no_cc {
            l.pick(
                "Plug a phone into the USB-C port first: this only works while one is plugged in.",
                "先在 USB-C 口插上手机：只在插着设备时有效。",
            )
        } else {
            l.pick(
                "18 W fast charging for the phone on the USB-C port, no internet. Ends when it is unplugged. Charge + internet is chosen on the touch screen.",
                "给 USB-C 口上的手机 18W 快充，不上网；拔掉就结束。「充电 + 上网」在触屏上选。",
            )
        },
        pb_busy: busy.pb,
        port,
        port_stale: a_stale,
        chg: chg_rows,
        chg_stale: bat.is_some_and(|(_, s)| s),
        chg_err,
    }
}

#[derive(Template)]
#[template(path = "usb_leaves.html")]
struct Leaves<'a> {
    v: &'a View,
}

fn leaves(app: &Shared, a: &Agent, busy: &Busy, l: Lang) -> Vec<(String, String)> {
    app.hub.read(|st| {
        split_leaves(
            &Leaves {
                v: &view(st, a, busy, l),
            }
            .render()
            .unwrap(),
        )
    })
}

fn leaf(app: &Shared, a: &Agent, busy: &Busy, l: Lang, id: &str) -> String {
    leaves(app, a, busy, l)
        .into_iter()
        .find(|(k, _)| k == id)
        .map_or_else(String::new, |(_, h)| h)
}

// ---- confirms and results ----

fn mode_label(m: &str) -> String {
    m.to_uppercase()
}

pub fn mode_confirm(target: &str, current: Option<&str>, l: Lang) -> write::Confirm {
    let t = mode_label(target);
    let cur = current.map_or_else(dash, mode_label);
    write::Confirm {
        tier: write::Tier::Dialog,
        title: match l {
            Lang::En => format!("Switch USB to {t}?"),
            Lang::Zh => format!("把 USB 切换到 {t}？"),
        },
        what: match target {
            "debug" => l.pick(
                "USB switches to ADB debugging. Anyone who can plug into the USB port gets a full root shell on the device. Only do this if you have the device with you and trust where it is.",
                "USB 切换到 ADB 调试。任何能插上这个 USB 口的人都能拿到设备的完整 root shell。只在设备在你手边、所处环境可信时才这样做。",
            ),
            "mtp" => l.pick(
                "USB switches to file transfer (MTP). A computer on the cable stops using the U60 as a network adapter.",
                "USB 切换到文件传输（MTP）。USB 线上的电脑不再把 U60 当网卡用。",
            ),
            _ => l.pick(
                "USB switches to a network adapter (RNDIS): a computer on the cable gets its connection through the U60.",
                "USB 切换到网卡模式（RNDIS）：USB 线上的电脑通过 U60 上网。",
            ),
        }
        .into(),
        downtime: Some(
            l.pick(
                "The USB link drops and comes back in a few seconds. Wi-Fi and the mobile connection are not affected.",
                "USB 连接会断开，几秒后恢复。Wi-Fi 和移动网络不受影响。",
            )
            .into(),
        ),
        recovery: Some(match l {
            Lang::En => format!("If a computer on the USB cable lost its connection, unplug and replug the cable. To undo, pick the previous mode ({cur}) here from a device on the U60's Wi-Fi."),
            Lang::Zh => format!("如果 USB 线上的电脑断了网，拔下线再插上。要改回去，用连着 U60 Wi-Fi 的设备在这里选回原来的模式（{cur}）。"),
        }),
        action: match l {
            Lang::En => format!("Switch to {t}"),
            Lang::Zh => format!("切换到 {t}"),
        },
        cuts_uplink: true,
        word: None,
        danger: target == "debug",
    }
}

fn mode_applied(target: &str, l: Lang) -> String {
    let t = mode_label(target);
    match l {
        Lang::En => format!("USB switched to {t}"),
        Lang::Zh => format!("USB 已切到 {t}"),
    }
}

pub fn pb_confirm(on: bool, l: Lang) -> write::Confirm {
    write::Confirm {
        tier: write::Tier::Inline,
        title: if on {
            l.pick("Turn fast charging on", "打开快速充电宝")
        } else {
            l.pick("Turn fast charging off", "关掉快速充电宝")
        }
        .into(),
        what: if on {
            l.pick(
                "The phone on the USB port fast-charges from the U60's battery (18 W) and has no internet through the cable.",
                "USB 口上的手机改用 U60 的电池快充（18W），这根线不再给它上网。",
            )
        } else {
            l.pick(
                "The phone on the USB port goes back to normal charging.",
                "USB 口上的手机回到普通充电。",
            )
        }
        .into(),
        downtime: None,
        recovery: None,
        action: if on {
            l.pick("Confirm: turn fast charging on", "确认：打开快速充电宝")
        } else {
            l.pick("Confirm: turn fast charging off", "确认：关掉快速充电宝")
        }
        .into(),
        cuts_uplink: false,
        word: None,
        danger: false,
    }
}

/// Why the mode can't be sent now (None: it can). `mode_settable: false` is never sent: u60d's
/// `usb.set` doesn't check the firmware, the agent did (501).
pub fn mode_gate(a: &Agent, l: Lang) -> Option<write::Outcome> {
    match (&a.port, &a.fail) {
        (_, Some(f)) => Some(write::Outcome::Failed(f.text(l))),
        (None, None) => Some(write::Outcome::NotYet(
            l.pick(
                "USB state not read yet; try again shortly",
                "还没读到 USB 状态，稍后再试",
            )
            .into(),
        )),
        (Some(p), None) if p.settable == Some(false) => Some(write::Outcome::NotYet(
            l.pick(
                "This firmware can't change the USB mode",
                "这版固件不能改 USB 模式",
            )
            .into(),
        )),
        _ => None,
    }
}

/// The current fast-charging state if it can be changed now, else why not.
pub fn pb_gate(st: &St, a: &Agent, l: Lang) -> Result<bool, write::Outcome> {
    let Some(on) = powerbank(st) else {
        return Err(write::Outcome::NotYet(
            l.pick(
                "Fast charging state not read yet; try again shortly",
                "还没读到快速充电宝的状态，稍后再试",
            )
            .into(),
        ));
    };
    if no_cc(st, a) {
        return Err(write::Outcome::NotYet(
            l.pick(
                "Plug a phone into the USB-C port first: this only works while one is plugged in.",
                "先在 USB-C 口插上手机：只在插着设备时有效。",
            )
            .into(),
        ));
    }
    Ok(on)
}

pub fn mode_write(target: &str, l: Lang) -> write::Write {
    let want = target.to_owned();
    write::Write {
        action: "usb.set",
        params: json!({ "mode": target }),
        readback: Some(Box::new(move |st| {
            usb_mode(st).as_deref() == Some(want.as_str())
        })),
        applied: mode_applied(target, l),
        timeout: MODE_READBACK,
        typed: None,
        request_timeout: None,
    }
}

pub fn pb_write(on: bool, l: Lang) -> write::Write {
    write::Write {
        action: "vendor.call",
        params: json!({"object": "zwrt_bsp.powerbank", "method": "set", "args": {"state": i64::from(on)}}),
        readback: Some(Box::new(move |st| powerbank(st) == Some(on))),
        applied: if on {
            l.pick("Fast charging on", "已打开快速充电宝")
        } else {
            l.pick("Fast charging off", "已关掉快速充电宝")
        }
        .into(),
        timeout: write::READBACK,
        typed: None,
        request_timeout: None,
    }
}

/// Wait until the hub shows `check`, or `timeout`.
pub async fn wait_for(hub: &Hub, timeout: Duration, check: impl Fn(&St) -> bool) -> bool {
    let mut changes = hub.watch();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if hub.read(&check) {
            return true;
        }
        match tokio::time::timeout_at(deadline, changes.recv()).await {
            Ok(Ok(_)) | Ok(Err(RecvError::Lagged(_))) => continue,
            _ => return false,
        }
    }
}

// ---- the page and its stream ----

pub struct Poll {
    who: Caller,
    l: Lang,
    agent: Agent,
}

impl Polled for Poll {
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        self.agent
            .update(crate::agent::get(app, &self.who, "/api/usb/status").await);
        (
            leaves(app, &self.agent, &Busy::default(), self.l),
            AGENT_EVERY,
        )
    }
    fn watches(&self, block: &str) -> bool {
        matches!(block, "qos" | "typec" | "powerbank" | "battery" | "charger")
    }
    fn render(&mut self, app: &Shared) -> Option<Vec<(String, String)>> {
        Some(leaves(app, &self.agent, &Busy::default(), self.l))
    }
}

#[derive(Template)]
#[template(path = "usb.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    leaves: Vec<(String, String)>,
    mode_op: String,
    pb_op: String,
}

impl Page {
    fn leaf(&self, id: &str) -> &str {
        self.leaves
            .iter()
            .find(|(k, _)| k == id)
            .map_or("", |(_, h)| h.as_str())
    }
}

fn mode_area(l: Lang, remote: bool, target: &str, phase: &write::Phase) -> String {
    let set = format!("/usb/mode/set?mode={target}");
    let cancel = format!("/usb/mode/cancel?want={target}");
    write::Area {
        id: "usb-mode",
        l,
        remote,
        set: &set,
        cancel: &cancel,
        phase,
    }
    .html()
}

fn pb_area(l: Lang, target: bool, phase: &write::Phase) -> String {
    let set = format!("/usb/pb/set?on={target}");
    write::Area {
        id: "usb-pb",
        l,
        remote: false,
        set: &set,
        cancel: "/usb/pb/cancel",
        phase,
    }
    .html()
}

pub async fn page(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let a = Agent::read(&app, &Caller::of(&app, &req)).await;
    let shell = app.hub.read(|st| {
        crate::shell::Shell::new(&headers, PATH).live(
            st,
            &ctx,
            Some(format!("{PATH}/events?g={}", st.generation)),
        )
    });
    let html = Page {
        shell,
        l: ctx.lang,
        leaves: leaves(&app, &a, &Busy::default(), ctx.lang),
        mode_op: mode_area(ctx.lang, ctx.remote, "", &write::Phase::Idle),
        pb_op: pb_area(ctx.lang, false, &write::Phase::Idle),
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
        who: Caller::of(&app, &req),
        l: ctx.lang,
        agent: Agent::default(),
    };
    crate::agent::stream(app, ctx, q.g, PATH, p)
}

/// Retry: reconnect to u60d now, and ask the agent again.
pub async fn refresh(State(app): State<Shared>) -> axum::http::StatusCode {
    app.hub.retry();
    let _ = app.kick.send(PATH);
    axum::http::StatusCode::NO_CONTENT
}

fn one(html: String) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(futures_util::stream::once(async move {
        Ok(crate::pages::patch(&html))
    }))
}

fn valid_mode(m: &str) -> Option<&'static str> {
    MODES.iter().find(|k| **k == m).copied()
}

#[derive(Deserialize, Default)]
pub struct ModeQuery {
    #[serde(default)]
    mode: String,
}

/// A mode button: the dialog (tier 3), or why not. Re-reads `mode_settable`.
pub async fn mode_ask(
    State(app): State<Shared>,
    Query(q): Query<ModeQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let a = Agent::read(&app, &Caller::of(&app, &req)).await;
    let Some(m) = valid_mode(&q.mode) else {
        return one(mode_area(l, ctx.remote, "", &write::Phase::Idle));
    };
    let current = app
        .hub
        .read(usb_mode)
        .or_else(|| a.port.as_ref().and_then(|p| p.mode.clone()));
    let phase = match mode_gate(&a, l) {
        Some(o) => write::Phase::Done(o),
        None if current.as_deref() == Some(m) => write::Phase::Idle,
        None => write::Phase::Ask(mode_confirm(m, current.as_deref(), l)),
    };
    let ctl = leaf(&app, &a, &Busy::default(), l, "usb-modes");
    one(format!("{ctl}\n{}", mode_area(l, ctx.remote, m, &phase)))
}

/// The confirmed mode switch: gate again, `usb.set` once, read back `qos.usb_mode`.
pub async fn mode_set(
    State(app): State<Shared>,
    Query(q): Query<ModeQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    let who = Caller::of(&app, &req);
    let target = valid_mode(&q.mode).unwrap_or("");
    let a0 = Agent::default();
    let pending = format!(
        "{}\n{}",
        leaf(
            &app,
            &a0,
            &Busy {
                mode: true,
                pb: None
            },
            l,
            "usb-modes"
        ),
        mode_area(
            l,
            false,
            target,
            &write::Phase::Pending(match l {
                Lang::En => format!("Switching to {}…", mode_label(target)),
                Lang::Zh => format!("正在切换到 {}…", mode_label(target)),
            }),
        )
    );
    write::respond(pending, async move {
        let a = Agent::read(&app, &who).await;
        let o = if target.is_empty() {
            write::Outcome::Failed(l.pick("unknown mode", "不认识的模式").into())
        } else if let Some(o) = mode_gate(&a, l) {
            o
        } else {
            write::execute(
                &app.http,
                &app.control_url,
                &app.hub,
                &mode_write(target, l),
                l,
            )
            .await
        };
        format!(
            "{}\n{}",
            leaf(&app, &a, &Busy::default(), l, "usb-modes"),
            mode_area(l, false, target, &write::Phase::Done(o))
        )
    })
}

#[derive(Deserialize, Default)]
pub struct ModeCancel {
    #[serde(default)]
    want: String,
    #[serde(default)]
    lost: bool,
}

/// Cancel / Esc; `lost=true` when the send's reply dropped (a computer on USB loses its link):
/// look at the mode once more (up to the usual readback) before saying "not confirmed".
pub async fn mode_cancel(
    State(app): State<Shared>,
    Query(q): Query<ModeCancel>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let want = valid_mode(&q.want);
    let phase = match (q.lost, want) {
        (false, _) => write::Phase::Idle,
        (true, None) => write::Phase::Done(write::Outcome::Unknown),
        (true, Some(m)) => {
            if wait_for(&app.hub, write::READBACK, |st| {
                usb_mode(st).as_deref() == Some(m)
            })
            .await
            {
                write::Phase::Done(write::Outcome::Applied(mode_applied(m, l)))
            } else {
                write::Phase::Done(write::Outcome::Unknown)
            }
        }
    };
    one(mode_area(l, ctx.remote, want.unwrap_or(""), &phase))
}

/// The fast-charging switch: the inline confirm (tier 2) for the other state, or why not.
pub async fn pb_ask(State(app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    let a = Agent::read(&app, &Caller::of(&app, &req)).await;
    let (target, phase) = match app.hub.read(|st| pb_gate(st, &a, l)) {
        Ok(on) => (!on, write::Phase::Ask(pb_confirm(!on, l))),
        Err(o) => (false, write::Phase::Done(o)),
    };
    one(pb_area(l, target, &phase))
}

#[derive(Deserialize)]
pub struct PbQuery {
    on: bool,
}

/// The confirmed switch: gate again, `vendor.call` powerbank set once, read back `powerbank.state`.
pub async fn pb_set(
    State(app): State<Shared>,
    Query(q): Query<PbQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    let who = Caller::of(&app, &req);
    let target = q.on;
    let pending = format!(
        "{}\n{}",
        leaf(
            &app,
            &Agent::default(),
            &Busy {
                mode: false,
                pb: Some(target)
            },
            l,
            "usb-pbrow"
        ),
        pb_area(l, target, &write::Phase::Pending(String::new()))
    );
    write::respond(pending, async move {
        let a = Agent::read(&app, &who).await;
        let o = match app.hub.read(|st| pb_gate(st, &a, l)) {
            Err(o) => o,
            Ok(_) => {
                write::execute(
                    &app.http,
                    &app.control_url,
                    &app.hub,
                    &pb_write(target, l),
                    l,
                )
                .await
            }
        };
        format!(
            "{}\n{}",
            leaf(&app, &a, &Busy::default(), l, "usb-pbrow"),
            pb_area(l, target, &write::Phase::Done(o))
        )
    })
}

pub async fn pb_cancel(
    Query(q): Query<write::CancelQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    one(pb_area(l, false, &write::cancel_phase(&q)))
}

#[cfg(test)]
mod tests;
