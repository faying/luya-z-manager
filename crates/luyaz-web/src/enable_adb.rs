//! /tools/enable-adb (启用 ADB): the current USB mode, the
//! security warning, the one action (tier 3, a dialog).
//!
//! ```text
//!  on open / 刷新 / after the change ─▶ agent /api/usb/status (tab's session; u60d first)
//!        shared USB write policy ─▶ the button is disabled in the very first HTML, with the reason
//!  hub `qos.usb_mode` ─▶ the mode shown (the agent's `mode` only while the block is missing)
//!  启用 ─▶ ask (gate again) ─▶ dialog ─▶ set (gate again) ─▶ usb::mode_write("debug") ─▶ readback
//! ```
//! - No polling: the old page read nothing; this one reads on open, on 刷新 and after its own
//!   change, and the mode follows the hub's `qos` block by itself.
//! - One writer: [`crate::usb::mode_write`] and [`crate::usb::mode_gate`] (the /usb page's own);
//!   The Core executor checks the same assessment again before dispatch.
//! - Unlike /usb, a tab already in debug mode may press it again (the old page's 「重新启用」).
//! - The dialog's texts are the old page's (its recovery line points to the /usb page).

use std::convert::Infallible;
use std::time::Duration;

use askama::Template;
use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::Stream;

use crate::topbar::Via;
use crate::agent::{Caller, Polled};
use crate::i18n::Lang;
use crate::pages::{Shared, StreamQuery, split_leaves};
use crate::usb::{self, Agent};
use crate::write;

pub const PATH: &str = "/tools/enable-adb";
/// The page does not poll; the stream only re-reads this rarely (a kick reads at once).
const EVERY: Duration = Duration::from_secs(900);

pub fn routes() -> Router<Shared> {
    Router::new()
        .route(PATH, get(page))
        .route("/tools/enable-adb/events", get(events))
        .route("/tools/enable-adb/refresh", post(refresh))
        .route("/tools/enable-adb/ask", post(ask))
        .route("/tools/enable-adb/set", post(set))
        .route("/tools/enable-adb/cancel", post(cancel))
}

// ---- the view ----

pub struct View {
    pub l: Lang,
    pub tone: &'static str,
    pub state: String,
    pub reason: Option<String>,
    pub retry: bool,
    pub stale: bool,
    /// "Re-enable" instead of "Enable".
    pub on: bool,
    /// `mode_settable: false`: the button is off and says why.
    pub fixed: bool,
    pub blocked: bool,
    pub capability_note: Option<&'static str>,
}

impl View {
    pub fn symbol(&self) -> &'static str {
        crate::sms::symbol(self.tone)
    }
}

/// The mode now: the hub's `qos.usb_mode`, else the agent's.
fn mode_of(app: &Shared, a: &Agent) -> Option<String> {
    app.hub
        .read(usb::usb_mode)
        .or_else(|| a.port.as_ref().and_then(|p| p.mode.clone()))
}

pub fn view(mode: Option<&str>, a: &Agent, l: Lang) -> View {
    let (tone, state, reason): (&str, String, Option<String>) = match (&a.port, &a.fail, mode) {
        (None, Some(f), None) => (
            "warn",
            l.pick("Couldn't read the USB mode", "读不到 USB 模式").into(),
            Some(f.text(l)),
        ),
        (None, None, None) => (
            "neutral",
            l.pick("Reading USB mode…", "正在读取 USB 模式…").into(),
            None,
        ),
        (_, _, Some("debug")) => (
            "warn",
            l.pick("ADB is on", "ADB 已开启").into(),
            Some(
                l.pick(
                    "USB is in debug mode: anyone with the cable has a root shell. Switch USB to another mode when you're done.",
                    "USB 处于调试模式：任何接上数据线的人都能拿到 root shell。用完请把 USB 切到别的模式。",
                )
                .into(),
            ),
        ),
        (_, _, Some(m)) => (
            "neutral",
            l.pick("ADB is off", "ADB 未开启").into(),
            Some(match l {
                Lang::En => format!("USB mode: {}", m.to_uppercase()),
                Lang::Zh => format!("USB 模式：{}", m.to_uppercase()),
            }),
        ),
        (_, _, None) => (
            "neutral",
            l.pick("USB mode not reported", "设备没报告 USB 模式").into(),
            Some(
                l.pick(
                    "The device didn't say which USB mode is active.",
                    "设备没有说明当前是哪种 USB 模式。",
                )
                .into(),
            ),
        ),
    };
    View {
        l,
        tone,
        state,
        reason,
        retry: a.port.is_none() && a.fail.is_some(),
        stale: a.port.is_some() && a.fail.is_some(),
        on: mode == Some("debug"),
        blocked: usb::mode_gate(a, l).is_some(),
        capability_note: usb::policy_note(a, "usb.mode.set", l),
        fixed: a
            .port
            .as_ref()
            .and_then(|p| p.policies.get("usb.mode.set"))
            .is_some_and(|p| p.assessment.status == luyaz_api::firmware::Support::Unsupported),
    }
}

#[derive(Template)]
#[template(path = "enable_adb_leaves.html")]
struct Leaves<'a> {
    v: &'a View,
}

fn leaves(app: &Shared, a: &Agent, l: Lang) -> Vec<(String, String)> {
    let v = view(mode_of(app, a).as_deref(), a, l);
    split_leaves(&Leaves { v: &v }.render().unwrap())
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
        (leaves(app, &self.agent, self.l), EVERY)
    }
    fn watches(&self, block: &str) -> bool {
        block == "qos"
    }
    fn render(&mut self, app: &Shared) -> Option<Vec<(String, String)>> {
        Some(leaves(app, &self.agent, self.l))
    }
}

#[derive(Template)]
#[template(path = "enable_adb.html")]
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

fn area(l: Lang, remote: Via, set: &str, phase: &write::Phase) -> String {
    write::Area {
        id: "adb",
        l,
        remote,
        set,
        cancel: "/tools/enable-adb/cancel",
        phase,
    }
    .html()
}

pub async fn page(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    // read before rendering: a firmware that can't set the mode has the button off in the HTML
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
        l,
        leaves: leaves(&app, &a, l),
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
    let p = Poll {
        who: Caller::of(&app, &req),
        l: ctx.lang,
        agent: Agent::default(),
    };
    crate::agent::stream(app, ctx, q.g, PATH, p)
}

/// 刷新 / 重试: every open tab reads again.
pub async fn refresh(State(app): State<Shared>) -> StatusCode {
    app.hub.retry();
    let _ = app.kick.send(PATH);
    StatusCode::NO_CONTENT
}

fn one(html: String) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(futures_util::stream::once(async move {
        Ok(crate::pages::patch(&html))
    }))
}

// ---- the change ----

fn confirm(l: Lang) -> write::Confirm {
    write::Confirm {
        tier: write::Tier::Dialog,
        title: l
            .pick("Turn on ADB over USB?", "打开 USB 的 ADB 调试？")
            .into(),
        what: l
            .pick(
                "Whoever plugs a cable into the USB port gets a root shell on the device. Turn it on only with the device in your hands and nobody else around, and switch it off when you are done.",
                "谁把线插到 USB 口上，谁就拿到设备的 root shell。只在设备在你手上、旁边没有别人时打开，用完就关。",
            )
            .into(),
        downtime: Some(
            l.pick(
                "If you are connected to this page over the USB cable (USB network), that connection drops. Wi-Fi and the mobile connection are not affected.",
                "如果你是通过 USB 数据线（USB 网络）打开这个页面的，这条连接会断开。Wi-Fi 和移动网络不受影响。",
            )
            .into(),
        ),
        recovery: Some(
            l.pick(
                "Connect over Wi-Fi and switch USB back to another mode on the USB mode page.",
                "改用 Wi-Fi 连接，在「USB 模式」页面把 USB 切回别的模式。",
            )
            .into(),
        ),
        action: l.pick("Enable ADB", "启用 ADB").into(),
        cuts_uplink: true,
        word: None,
        danger: true,
        revert: crate::write::Revert::No,
    }
}

/// What a finished change says: ADB's own sentence once the mode was read back.
fn said(o: write::Outcome, l: Lang) -> write::Outcome {
    match o {
        write::Outcome::Applied(_) => write::Outcome::Applied(
            l.pick(
                "ADB is on. Plug in the USB cable and run adb devices to check.",
                "ADB 已打开。插上 USB 线，运行 adb devices 看看。",
            )
            .into(),
        ),
        o => o,
    }
}

/// 启用 ADB: the dialog, or why not (the firmware can't, the state wasn't read).
pub async fn ask(State(app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let a = Agent::read(&app, &Caller::of(&app, &req)).await;
    let phase = match usb::mode_gate(&a, l) {
        Some(o) => write::Phase::Done(o),
        None => write::Phase::Ask(confirm(l)),
    };
    let set = format!("{PATH}/set");
    one(area(l, ctx.remote, &set, &phase))
}

/// The confirmed change: gate again, `usb.set` once, read back `qos.usb_mode` (up to 30 s).
pub async fn set(State(app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    let who = Caller::of(&app, &req);
    let pending = area(
        l,
        Via::Local,
        "",
        &write::Phase::Pending(l.pick("Enabling ADB…", "正在启用 ADB…").into()),
    );
    write::respond(pending, async move {
        let a = Agent::read(&app, &who).await;
        let o = match usb::mode_gate(&a, l) {
            Some(o) => o,
            None => said(
                write::execute(
                    &app.http,
                    &app.control_url,
                    &app.hub,
                    &usb::mode_write("debug", l),
                    l,
                )
                .await,
                l,
            ),
        };
        let _ = app.kick.send(PATH);
        area(l, Via::Local, "", &write::Phase::Done(o))
    })
}

#[derive(serde::Deserialize, Default)]
pub struct CancelQ {
    #[serde(default)]
    lost: bool,
}

/// Cancel / Esc; `lost=true` when the send's reply dropped (a computer on USB loses its link):
/// look at the mode once more before saying "not confirmed".
pub async fn cancel(
    State(app): State<Shared>,
    Query(q): Query<CancelQ>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let phase = if !q.lost {
        write::Phase::Idle
    } else if usb::wait_for(&app.hub, write::READBACK, |st| {
        usb::usb_mode(st).as_deref() == Some("debug")
    })
    .await
    {
        write::Phase::Done(said(write::Outcome::Applied(String::new()), l))
    } else {
        write::Phase::Done(write::Outcome::Unknown)
    };
    one(area(l, ctx.remote, "", &phase))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(settable: Option<bool>, mode: Option<&str>) -> Agent {
        let mut a = crate::usb::tests::agent(settable);
        a.port.as_mut().unwrap().mode = mode.map(Into::into);
        a
    }

    #[test]
    fn a_firmware_that_cannot_set_the_mode_has_the_button_off() {
        let a = agent(Some(false), Some("user"));
        let v = view(Some("user"), &a, Lang::En);
        assert!(v.fixed && !v.on);
        let html = Leaves { v: &v }.render().unwrap();
        assert!(
            html.contains("disabled") && html.contains("This firmware can&#39;t turn on ADB mode"),
            "{html}"
        );
        let ok = view(Some("mtp"), &agent(Some(true), Some("mtp")), Lang::En);
        let html = Leaves { v: &ok }.render().unwrap();
        assert!(
            !html.contains("disabled") && html.contains("/tools/enable-adb/ask"),
            "{html}"
        );
        assert!(usb::mode_gate(&a, Lang::En).is_some(), "never sent");
        assert!(usb::mode_gate(&agent(Some(true), None), Lang::En).is_none());
    }

    #[test]
    fn the_status_words_follow_the_mode() {
        let a = agent(None, None);
        assert_eq!(view(Some("debug"), &a, Lang::En).state, "ADB is on");
        assert!(view(Some("debug"), &a, Lang::En).on);
        let off = view(Some("rndis"), &a, Lang::Zh);
        assert_eq!(
            (off.state.as_str(), off.reason.as_deref()),
            ("ADB 未开启", Some("USB 模式：RNDIS"))
        );
        assert_eq!(view(None, &a, Lang::En).state, "USB mode not reported");
        let down = Agent {
            fail: Some(crate::agent::Fail::Unreachable),
            ..Agent::default()
        };
        let v = view(None, &down, Lang::En);
        assert!(v.retry && v.state == "Couldn't read the USB mode");
    }

    #[test]
    fn only_a_read_back_mode_is_reported_as_enabled() {
        assert!(matches!(
            said(write::Outcome::Accepted, Lang::En),
            write::Outcome::Accepted
        ));
        let o = said(write::Outcome::Applied("x".into()), Lang::En);
        assert_eq!(
            o.text(Lang::En),
            "ADB is on. Plug in the USB cable and run adb devices to check."
        );
    }
}
