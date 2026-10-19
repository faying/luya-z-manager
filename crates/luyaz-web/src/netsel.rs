//! /router/network-mode (网络模式) and /router/mobile-network (移动网络), the old
//! `router/network-mode/page.tsx` and `router/mobile-network/page.tsx`. Batch 3, step 3-4
//! (u60-web.md).
//!
//! Network mode — u60d only: the reported mode is the `signal` block's `net_select` (radio
//! preference; "TCHGWL_5G" / "NETWORK_auto" count as Auto like the old page), `net_select_mode`
//! (automatic / manual operator selection) adds a note. The six choices, order and names are the
//! old page's (ZTE's own web UI); firmware values outside them are named (`OTHER`). Apply = tier 3,
//! `network.set_mode {mode}` (u60d's transaction), followed in the `op` block (follow.rs); the
//! confirm adds the old page's warnings for "5G SA only" / "3G only" and says when u60d's automatic
//! revert is off (`op.rollback_enabled`). "Last change" links to that row in /changes.
//!
//! Mobile network:
//! - connection, mobile data, roaming = u60d's `interfaces.cellular` (`connect_status`, `enable`,
//!   `roam_enable`; `enable` 0 with the call up counts as on, as core / the old page do); data and
//!   roaming are written with u60d `cellular.set` (tier 2, tier 3 over Tailscale as the old page;
//!   data on also sends `connect_mode` 1 as the agent did);
//! - radio off ("airplane") = the agent's `/api/modem/status` `operate_mode` (u60d has no field);
//! - operator scan, register, back to automatic = the agent's jobs (`/api/netinfo/scan`,
//!   `/api/modem/register` + its guard, `/api/modem/netselect/auto`) and their state in
//!   `/api/netinfo?lite=1` (`scan`, `guard`), with the tab's session. The agent forwards each
//!   step to u60d `/control` inside u60d's search session (D17); the guard that goes back to
//!   automatic when a register does not take stays in the agent (u60d's port of it is not switched
//!   on yet), so these three go through the agent, not straight to u60d;
//! - reboot = u60d `device.reboot` (tier 3).

use std::convert::Infallible;
use std::time::Duration;

use askama::Template;
use axum::extract::{Query, State};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::topbar::Via;
use crate::agent::{Caller, Fail, Polled};
use crate::hub::{Link, St};
use crate::i18n::Lang;
use crate::pages::{Shared, StreamQuery, async_stream, patch, split_leaves};
use crate::write;

pub const MODE_PATH: &str = "/router/network-mode";
pub const NET_PATH: &str = "/router/mobile-network";
const FOLLOW: Duration = Duration::from_secs(130);
const SEND_TIMEOUT: Duration = Duration::from_secs(30);
/// The agent's scan may hold the modem 2 × 180 s; a register waits up to 90 s and may go back.
const SCAN_WAIT: Duration = Duration::from_secs(380);
const GUARD_WAIT: Duration = Duration::from_secs(300);
const JOB_POLL: Duration = Duration::from_secs(3);

fn stuck_reason(l: Lang) -> &'static str {
    l.pick(
        "Data service not responding · settings can't be changed for now",
        "数据服务没响应 · 暂时不能改设置",
    )
}

fn one(html: String) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(futures_util::stream::once(async move { Ok(patch(&html)) }))
}

fn signal(st: &St) -> Option<(&Value, bool)> {
    st.blocks
        .get("signal")
        .map(|b| (&b.data, b.stale || st.link != Link::Live))
}

// ================= network mode =================

/// (value, en, zh, en description, zh description), the old page's six.
pub const MODES: [(&str, &str, &str, &str, &str); 6] = [
    ("WL_AND_5G", "5G/4G/3G (Auto)", "5G/4G/3G（自动）", "5G, 4G and 3G; the modem picks the best one", "5G、4G、3G 都可以用，由模组挑最好的"),
    ("LTE_AND_5G", "5G NSA only", "只用 5G NSA", "5G carried by a 4G anchor (NSA), never SA. Connected, it shows both a 4G and a 5G carrier; where there is no NSA it stays on 4G.", "5G 由 4G 带着连（NSA），不用 SA。连上时会同时看到 4G 和 5G 两个载波；当地没有 NSA 就停在 4G。"),
    ("Only_5G", "5G SA only", "只用 5G SA", "Standalone 5G only. Many countries have no SA yet, and roaming SIMs often can't use it: abroad this can mean no signal.", "只连 5G SA。很多国家还没有 SA，漫游卡也常常用不了：在国外可能直接没信号。"),
    ("WCDMA_AND_LTE", "4G/3G", "4G/3G", "No 5G: LTE, falling back to WCDMA", "不用 5G：优先 LTE，没有时用 WCDMA"),
    ("Only_LTE", "4G only", "只用 4G", "LTE only", "只连 LTE"),
    ("Only_WCDMA", "3G only", "只用 3G", "WCDMA only. Many countries have switched 3G off: there this means no signal.", "只连 WCDMA。很多国家已经关了 3G：在那里就是没信号。"),
];

const AUTO: &str = "WL_AND_5G";

pub fn is_auto(v: &str) -> bool {
    matches!(v, "WL_AND_5G" | "TCHGWL_5G" | "NETWORK_auto")
}

/// Firmware values that are not among the six, in words (the old page's `OTHER`).
fn other(v: &str, l: Lang) -> Option<&'static str> {
    Some(match v {
        "TCHGWL_5G" => l.pick("Auto (every network type)", "自动（全部制式）"),
        "NETWORK_auto" => l.pick("Auto (firmware default)", "自动（固件默认）"),
        "WL_AND_NSA" => "5G NSA + 4G + 3G",
        "4G_AND_5G" => "4G + 5G",
        "GSM_AND_LTE" => "4G + 2G",
        "TDSCDMA_AND_LTE" => "4G + TD-SCDMA",
        "Only_GSM_WCDMA" => "3G + 2G",
        "Only_TDSCDMA" => l.pick("TD-SCDMA only", "只用 TD-SCDMA"),
        "Only_GSM" => l.pick("2G only", "只用 2G"),
        _ => return None,
    })
}

pub fn mode_label(v: &str, l: Lang) -> String {
    if v.is_empty() {
        return "—".into();
    }
    if let Some(m) = MODES.iter().find(|m| m.0 == v) {
        return l.pick(m.1, m.2).into();
    }
    other(v, l).map_or_else(|| v.to_string(), str::to_string)
}

fn risky(v: &str, l: Lang) -> Option<&'static str> {
    match v {
        "Only_5G" => Some(l.pick(
            "Many countries have no 5G SA yet, and roaming SIMs often can't use it where it exists. Abroad, pick Auto unless you know this network has SA for your SIM.",
            "很多国家还没有 5G SA；有 SA 的地方，漫游卡也常常用不了。在国外请选「自动」，除非你确定当地网络给你的卡开了 SA。",
        )),
        "Only_WCDMA" => Some(l.pick(
            "3G has been switched off in many countries (the US, much of Europe, Japan and others), and this device has no 2G. There, this mode means no mobile network at all.",
            "很多国家已经关了 3G（美国、欧洲大部分、日本等），这台设备又没有 2G。在那里选这个就是完全没有移动网络。",
        )),
        _ => None,
    }
}

/// The reported mode (raw) and the choice it is (Auto for the auto values).
pub fn reported(st: &St) -> Option<(String, String)> {
    let (d, _) = signal(st)?;
    let raw = d["net_select"].as_str().unwrap_or("").to_string();
    let now = if is_auto(&raw) { AUTO.to_string() } else { raw.clone() };
    Some((raw, now))
}

pub struct ModeView {
    pub l: Lang,
    pub stale: bool,
    pub now: String,
    /// `_nmnow`: the current choice ("" = not reported).
    pub cur: String,
    pub manual_sel: bool,
    pub last: Option<(String, String)>,
    pub dis: bool,
    pub why: Option<String>,
    pub txn: Option<String>,
}

/// The newest change-log row of an item: (href to its detail, "10-03 14:32 · 触屏").
pub fn last_change(item: &str, l: Lang) -> Option<(String, String)> {
    let entries = crate::changes::cached()?;
    let e = entries.iter().find(|e| e["item"].as_str() == Some(item) && e["hide"].as_bool() != Some(true))?;
    let id = e["op_id"].as_str().filter(|s| crate::changes::valid_id(s))?;
    let t = e["t"].as_str().unwrap_or("");
    let short = t.get(5..16).unwrap_or(t);
    let src = crate::changes::pick(e, "source", l);
    Some((format!("/changes?op={id}"), [short.to_string(), src].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ")))
}

pub fn mode_view(st: &St, l: Lang) -> ModeView {
    let r = reported(st);
    let txn = crate::follow::running(st, &["network."], l);
    ModeView {
        l,
        stale: signal(st).is_some_and(|(_, s)| s),
        now: match &r {
            None => "—".into(),
            Some((raw, _)) if raw.is_empty() => l.pick("Not reported", "设备没有报告").into(),
            Some((raw, _)) => mode_label(raw, l),
        },
        cur: r.as_ref().map(|(_, c)| c.clone()).unwrap_or_default(),
        manual_sel: signal(st).is_some_and(|(d, _)| d["net_select_mode"].as_str() == Some("manual_select")),
        last: last_change("network.mode", l),
        dis: st.stuck || txn.is_some() || r.is_none(),
        why: st.stuck.then(|| stuck_reason(l).to_string()),
        txn,
    }
}

#[derive(Template)]
#[template(path = "netmode_leaves.html")]
struct ModeLeaves<'a> {
    v: &'a ModeView,
}

pub fn mode_leaves(app: &Shared, l: Lang) -> Vec<(String, String)> {
    crate::changes::ensure_read(app);
    app.hub
        .read(|st| split_leaves(&ModeLeaves { v: &mode_view(st, l) }.render().unwrap()))
}

/// The draft back to what the device reads (a write's own reply; `_dn` changes every time).
fn mode_draft(app: &Shared) -> String {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let cur = app.hub.read(|st| reported(st).map(|r| r.1).unwrap_or_default());
    format!("<div id=\"nm-draft\" hidden data-signals=\"{{_dn: {n}, _nm: '{}'}}\"></div>", safe_mode(&cur))
}

/// A mode value as u60d takes it (letters, digits, `_`): safe in a URL and a JS string.
fn safe_mode(v: &str) -> &str {
    if v.len() <= 32 && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') { v } else { "" }
}

pub fn mode_check(want: &str, st: &St, l: Lang) -> Result<String, write::Outcome> {
    if st.stuck {
        return Err(write::Outcome::NotYet(stuck_reason(l).into()));
    }
    if !MODES.iter().any(|m| m.0 == want) {
        return Err(write::Outcome::Failed(l.pick("not one of the six modes", "不是这六种模式之一").into()));
    }
    Ok(want.to_string())
}

pub fn mode_confirm(want: &str, rollback_off: bool, l: Lang) -> write::Confirm {
    let label = mode_label(want, l);
    let mut what = if want == AUTO {
        l.pick("The modem picks the best available network again.", "调制解调器重新自动选择最好的网络。").to_string()
    } else {
        match l {
            Lang::En => format!("The modem only uses {label}. If that network isn't available here, there is no mobile connection."),
            Lang::Zh => format!("调制解调器只使用「{label}」。如果这里没有这种网络，就没有移动网络。"),
        }
    };
    if let Some(r) = risky(want, l) {
        what = format!("{what} {r}");
    }
    if rollback_off {
        what = format!("{what} {}", l.pick("Automatic revert is off: it stays even without a connection.", "自动退回没开：没通也会保持。"));
    }
    write::Confirm {
        tier: write::Tier::Dialog,
        title: match l {
            Lang::En => format!("Switch to {label}?"),
            Lang::Zh => format!("切换到「{label}」？"),
        },
        what,
        downtime: Some(l.pick("The mobile connection drops for about 30 seconds while the modem re-attaches.", "调制解调器重新附着，移动网络断开约 30 秒。").into()),
        recovery: Some(l.pick("Come back to this page and choose “Auto”. If the page can't be reached, set the network mode back on the device's touchscreen.", "回到这个页面选「自动」。如果页面打不开，在设备触屏上把网络模式改回来。").into()),
        action: l.pick("Switch network mode", "切换网络模式").into(),
        cuts_uplink: true,
        word: None,
        danger: false,
        revert: crate::write::Revert::No,
    }
}

fn mode_area(l: Lang, remote: Via, want: &str, phase: &write::Phase) -> String {
    let set = format!("{MODE_PATH}/set?mode={}", safe_mode(want));
    let cancel = format!("{MODE_PATH}/cancel");
    write::Area { id: "nm", l, remote, set: &set, cancel: &cancel, phase }.html()
}

#[derive(Deserialize, Default)]
pub struct ModeQuery {
    #[serde(default)]
    mode: String,
}

pub async fn mode_ask(State(app): State<Shared>, Query(q): Query<ModeQuery>, req: axum::extract::Request) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, MODE_PATH);
    let l = ctx.lang;
    let html = app.hub.read(|st| match mode_check(&q.mode, st, l) {
        Err(o) => mode_area(l, ctx.remote, "", &write::Phase::Done(o)),
        Ok(m) => {
            let revert = write::Revert::txn(st.blocks.get("op").map(|b| &b.data));
            let c = write::Confirm { revert, ..mode_confirm(&m, revert == write::Revert::Off, l) };
            mode_area(l, ctx.remote, &m, &write::Phase::Ask(c))
        }
    });
    one(html)
}

pub async fn mode_set(State(app): State<Shared>, Query(q): Query<ModeQuery>, req: axum::extract::Request) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, MODE_PATH).lang;
    Sse::new(async_stream(move |tx| async move {
        let want = match app.hub.read(|st| mode_check(&q.mode, st, l)) {
            Ok(m) => m,
            Err(o) => {
                let _ = tx.send(patch(&mode_area(l, Via::Local, "", &write::Phase::Done(o)))).await;
                return;
            }
        };
        let pending = |t: String| mode_area(l, Via::Local, &want, &write::Phase::Pending(t));
        if tx.send(patch(&pending(l.pick("Switching…", "正在切换…").into()))).await.is_err() {
            return;
        }
        let op_id = write::next_op_id();
        let body = json!({"action": "network.set_mode", "params": {"mode": want}, "source": "web", "op_id": op_id});
        let changes = app.hub.watch();
        let sent = match write::send_body(&app.http, &app.control_url, &body, Some(SEND_TIMEOUT), l).await {
            Ok(s) => s,
            Err(o) => {
                let _ = tx.send(patch(&mode_area(l, Via::Local, &want, &write::Phase::Done(o)))).await;
                return;
            }
        };
        let w2 = want.clone();
        let Some(f) = crate::follow::follow(&app, &tx, &op_id, changes, FOLLOW, Duration::from_secs(5), l, pending, move |st| {
            reported(st).is_some_and(|r| r.1 == w2)
        })
        .await
        else {
            return;
        };
        let label = mode_label(&want, l);
        let applied = match l {
            Lang::En => format!("Network mode set to {label}"),
            Lang::Zh => format!("网络模式已改成「{label}」"),
        };
        let o = crate::follow::outcome(&f, sent, applied, l);
        let _ = tx.send(patch(&format!("{}\n{}", mode_area(l, Via::Local, &want, &write::Phase::Done(o)), mode_draft(&app)))).await;
    }))
}

pub async fn mode_cancel(Query(q): Query<write::CancelQuery>, req: axum::extract::Request) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, MODE_PATH);
    one(mode_area(ctx.lang, ctx.remote, "", &write::cancel_phase(&q)))
}

pub struct ModePoll {
    l: Lang,
}

impl Polled for ModePoll {
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        (mode_leaves(app, self.l), Duration::from_secs(30))
    }
    fn watches(&self, block: &str) -> bool {
        matches!(block, "signal" | "op" | "exec")
    }
    fn render(&mut self, app: &Shared) -> Option<Vec<(String, String)>> {
        Some(mode_leaves(app, self.l))
    }
}

#[derive(Template)]
#[template(path = "netmode.html")]
struct ModePage {
    shell: crate::shell::Shell,
    l: Lang,
    leaves: Vec<(String, String)>,
    cur: String,
    op: String,
}

impl ModePage {
    fn leaf(&self, id: &str) -> &str {
        self.leaves.iter().find(|(k, _)| k == id).map_or("", |(_, h)| h.as_str())
    }
    fn modes(&self) -> Vec<(&'static str, &'static str, &'static str)> {
        MODES.iter().map(|m| (m.0, self.l.pick(m.1, m.2), self.l.pick(m.3, m.4))).collect()
    }
}

pub async fn mode_page(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, MODE_PATH);
    let l = ctx.lang;
    let (shell, cur) = app.hub.read(|st| {
        (
            crate::shell::Shell::new(&headers, MODE_PATH).live(st, &ctx, Some(format!("{MODE_PATH}/events?g={}", st.generation))),
            reported(st).map(|r| r.1).unwrap_or_default(),
        )
    });
    let html = ModePage {
        shell,
        l,
        leaves: mode_leaves(&app, l),
        cur: safe_mode(&cur).to_string(),
        op: mode_area(l, ctx.remote, "", &write::Phase::Idle),
    }
    .render()
    .unwrap();
    crate::assets::maybe_gzip(&headers, "text/html; charset=utf-8", html.into_bytes(), "no-store")
}

pub async fn mode_events(State(app): State<Shared>, Query(q): Query<StreamQuery>, req: axum::extract::Request) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, MODE_PATH);
    let p = ModePoll { l: ctx.lang };
    crate::agent::stream(app, ctx, q.g, MODE_PATH, p)
}

// ================= mobile network =================

/// connected / connecting / disconnected / unknown (connectState.ts `connectKind`).
pub fn connect_kind(s: &str) -> &'static str {
    let v = s.to_lowercase();
    if v.is_empty() {
        "unknown"
    } else if v.contains("disconnect") {
        "disconnected"
    } else if v.contains("connecting") {
        "connecting"
    } else if v.contains("connected") {
        "connected"
    } else {
        "disconnected"
    }
}

/// "IPv4 + IPv6" / "IPv4" / "IPv6" / "".
pub fn families(s: &str) -> String {
    let v = s.to_lowercase();
    [v.contains("ipv4").then_some("IPv4"), v.contains("ipv6").then_some("IPv6")]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" + ")
}

/// What `interfaces.cellular` says.
#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    pub status: String,
    /// The mobile data switch (`enable` 0 with the call up counts as on); None = can't tell.
    pub data: Option<bool>,
    pub roam: Option<bool>,
}

pub fn cell(st: &St) -> Option<Cell> {
    let c = &st.blocks.get("interfaces")?.data["cellular"];
    if !c.is_object() {
        return None;
    }
    let status = c["connect_status"].as_str().unwrap_or("").to_string();
    let flag = |v: &Value| v.as_i64().map(|n| n != 0).or_else(|| v.as_bool()).or_else(|| v.as_str().map(|s| s == "1"));
    let data = match flag(&c["enable"]) {
        Some(true) => Some(true),
        Some(false) => match connect_kind(&status) {
            "unknown" => None,
            k => Some(k == "connected" || k == "connecting"),
        },
        None => None,
    };
    Some(Cell { status, data, roam: flag(&c["roam_enable"]) })
}

/// The agent's jobs and radio state.
#[derive(Clone, Debug, Default)]
pub struct Jobs {
    /// `scan.state`: idle / scanning / done / error.
    pub scan: String,
    pub ops: Vec<Op>,
    pub scan_done_at: i64,
    pub scan_err: String,
    /// `guard.phase`: idle / registering / ok / reverting / reverted.
    pub guard: String,
    pub guard_target: String,
    pub guard_at: i64,
    /// `operate_mode` is not ONLINE.
    pub airplane: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Op {
    pub name: String,
    pub plmn: String,
    pub rat: String,
    pub status: String,
}

impl Jobs {
    pub fn of(ni: &Value, l: Lang) -> Jobs {
        let s = &ni["scan"];
        let g = &ni["guard"];
        let pick = |zh: &Value, en: &Value| {
            let (z, e) = (zh.as_str().unwrap_or(""), en.as_str().unwrap_or(""));
            if l == Lang::En && !e.is_empty() { e.to_string() } else { z.to_string() }
        };
        Jobs {
            scan: s["state"].as_str().unwrap_or("").into(),
            ops: s["operators"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|o| Op {
                            name: pick(&o["name"], &o["operator_en"]),
                            // they go into a URL and a JS string: digits / letters only
                            plmn: o["plmn"].as_str().unwrap_or("").chars().filter(char::is_ascii_digit).collect(),
                            rat: o["rat"].as_str().unwrap_or("").chars().filter(char::is_ascii_alphanumeric).collect(),
                            status: o["status"].as_str().unwrap_or("").into(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            scan_done_at: s["finished_at"].as_i64().unwrap_or(0),
            scan_err: pick(&s["error"], &s["error_en"]),
            guard: g["phase"].as_str().unwrap_or("").into(),
            guard_target: g["target"].as_str().unwrap_or("").into(),
            guard_at: g["started_at"].as_i64().unwrap_or(0),
            airplane: None,
        }
    }
    pub fn scanning(&self) -> bool {
        self.scan == "scanning"
    }
    pub fn guarding(&self) -> bool {
        matches!(self.guard.as_str(), "registering" | "reverting")
    }
    /// The finished scan's list, unless a register since then made it stale (the old page cleared it).
    pub fn shown(&self) -> Option<&[Op]> {
        (self.scan == "done" && !(self.guard == "ok" && self.guard_at >= self.scan_done_at)).then_some(&self.ops[..])
    }
}

/// Scan result m_rat → generation, as the stock web maps it (ZTE's own codes).
pub fn rat_name(r: &str) -> String {
    match r {
        "9" | "11" | "12" => "5G".into(),
        "7" | "13" => "4G".into(),
        "2" => "3G".into(),
        "0" => "2G".into(),
        "" => "—".into(),
        r => r.into(),
    }
}

pub struct NetView {
    pub l: Lang,
    pub stale: bool,
    pub tone: &'static str,
    pub state: String,
    pub reason: String,
    pub conn: String,
    pub data: Option<bool>,
    pub roam: Option<bool>,
    pub airplane: bool,
    /// Data / roaming switches off (not read, stale, a job running, stuck).
    pub radio_dis: bool,
    /// Scan / register / auto off (a job running here or in the agent, or u60d's search session).
    pub sel_dis: bool,
    pub job: Option<String>,
    pub ops: Option<Vec<Op>>,
    pub why: Option<String>,
    pub ni_err: Option<String>,
}

pub fn net_view(st: &St, jobs: Option<&Result<Jobs, Fail>>, l: Lang) -> NetView {
    let c = cell(st);
    let stale = st.blocks.get("interfaces").is_some_and(|b| b.stale) || st.link != Link::Live;
    let j = jobs.and_then(|r| r.as_ref().ok());
    let airplane = j.and_then(|j| j.airplane) == Some(true);
    let kind = c.as_ref().map(|c| connect_kind(&c.status)).unwrap_or("unknown");
    let fams = c.as_ref().map(|c| families(&c.status)).unwrap_or_default();
    let (tone, state, reason) = match &c {
        None => ("neutral", l.pick("Reading the mobile connection…", "正在读取移动网络…").to_string(), String::new()),
        Some(_) if airplane => ("bad", l.pick("Airplane mode on", "飞行模式已开").into(), l.pick("The cellular radio is off. Reboot the device to bring it back.", "蜂窝无线电已关闭，重启设备才能恢复。").into()),
        Some(c) if c.data == Some(false) => ("bad", l.pick("Mobile data off", "移动数据已关").into(), l.pick("Devices on the U60 have no internet. Turn mobile data on below.", "连着 U60 的设备上不了网。在下面打开移动数据。").into()),
        Some(c) if kind == "connected" => (
            "ok",
            l.pick("Connected", "已连接").into(),
            [fams.clone(), if c.roam == Some(true) { l.pick("Data roaming is allowed.", "允许数据漫游。").into() } else { String::new() }]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" · "),
        ),
        Some(_) if kind == "connecting" => ("neutral", l.pick("Connecting…", "正在连接…").into(), String::new()),
        Some(c) => (
            "warn",
            l.pick("Not connected", "没有连接").into(),
            if c.status.is_empty() {
                l.pick("The modem doesn't report a connection state.", "调制解调器没有报告连接状态。").into()
            } else {
                match l {
                    Lang::En => format!("The modem reports “{}”. Check the signal and the APN.", c.status),
                    Lang::Zh => format!("调制解调器报告「{}」。检查信号和 APN。", c.status),
                }
            },
        ),
    };
    let session = st.blocks.get("op").and_then(|b| b.data["active"]["item"].as_str().map(str::to_owned)).filter(|i| i.starts_with("netselect"));
    let job = match j {
        Some(j) if j.scanning() => Some(l.pick("One to three minutes; mobile data is off while it searches…", "约 1–3 分钟，搜索期间移动数据会断开…").to_string()),
        Some(j) if j.guard == "registering" => Some(match l {
            Lang::En => format!("Registering to {}… If it does not take, the device goes back to automatic selection.", j.guard_target),
            Lang::Zh => format!("正在注册到 {}…注册不上会自动回到自动选网。", j.guard_target),
        }),
        Some(j) if j.guard == "reverting" => Some(l.pick("Going back to automatic selection…", "正在回到自动选网…").into()),
        _ if session.is_some() => crate::follow::running(st, &["netselect"], l),
        _ => None,
    };
    let busy = job.is_some();
    NetView {
        l,
        stale: c.is_some() && stale,
        tone: if c.is_some() && stale { "stale" } else { tone },
        state,
        reason,
        conn: match kind {
            "connected" => [l.pick("Connected", "已连接").to_string(), fams].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · "),
            "connecting" => l.pick("Connecting…", "正在连接…").into(),
            _ => c.as_ref().map(|c| c.status.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| "—".into()),
        },
        data: c.as_ref().and_then(|c| c.data),
        roam: c.as_ref().and_then(|c| c.roam),
        airplane,
        radio_dis: c.is_none() || stale || busy || st.stuck,
        sel_dis: busy || jobs.is_none_or(|r| r.is_err()),
        job,
        ops: j.and_then(|j| j.shown().map(<[Op]>::to_vec)),
        why: st.stuck.then(|| stuck_reason(l).to_string()),
        ni_err: jobs.and_then(|r| r.as_ref().err()).map(|e| e.text(l)),
    }
}

impl NetView {
    pub fn rat(&self, r: &str) -> String {
        rat_name(r)
    }
}

#[derive(Template)]
#[template(path = "mobilenet_leaves.html")]
struct NetLeaves<'a> {
    v: &'a NetView,
}

pub fn net_leaves(app: &Shared, jobs: Option<&Result<Jobs, Fail>>, l: Lang) -> Vec<(String, String)> {
    app.hub
        .read(|st| split_leaves(&NetLeaves { v: &net_view(st, jobs, l) }.render().unwrap()))
}

/// The agent's jobs + radio state, with the tab's session.
async fn fetch_jobs(app: &Shared, who: &Caller, l: Lang) -> Result<Jobs, Fail> {
    let (ni, ms) = tokio::join!(
        crate::agent::get(app, who, "/api/netinfo?lite=1"),
        crate::agent::get(app, who, "/api/modem/status"),
    );
    let mut j = Jobs::of(&ni?, l);
    j.airplane = ms.ok().and_then(|m| m["operate_mode"].as_str().map(|s| s != "ONLINE"));
    Ok(j)
}

// ---- writes ----

#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    Data(bool),
    Roam(bool),
    Scan,
    Register { plmn: String, rat: String },
    Auto,
    Reboot,
}

impl Act {
    fn id(&self) -> &'static str {
        match self {
            Act::Data(_) => "mn-data",
            Act::Roam(_) => "mn-roam",
            Act::Scan => "mn-scan",
            Act::Register { .. } => "mn-reg",
            Act::Auto => "mn-auto",
            Act::Reboot => "mn-boot",
        }
    }
    fn urls(&self) -> (String, String) {
        let p = NET_PATH;
        match self {
            Act::Data(on) => (format!("{p}/data/set?on={on}"), format!("{p}/data/cancel")),
            Act::Roam(on) => (format!("{p}/roam/set?on={on}"), format!("{p}/roam/cancel")),
            Act::Scan => (format!("{p}/scan/set"), format!("{p}/scan/cancel")),
            Act::Register { plmn, rat } => (format!("{p}/reg/set?plmn={plmn}&rat={rat}"), format!("{p}/reg/cancel")),
            Act::Auto => (format!("{p}/auto/set"), format!("{p}/auto/cancel")),
            Act::Reboot => (format!("{p}/reboot/set"), format!("{p}/reboot/cancel")),
        }
    }
}

fn net_area(act: &Act, l: Lang, remote: Via, phase: &write::Phase) -> String {
    let (set, cancel) = act.urls();
    write::Area { id: act.id(), l, remote, set: &set, cancel: &cancel, phase }.html()
}

/// A PLMN (5–6 digits) and a short RAT code, as u60d's `netselect.register` takes them.
fn reg_ok(plmn: &str, rat: &str) -> bool {
    (5..=6).contains(&plmn.len()) && plmn.bytes().all(|b| b.is_ascii_digit()) && rat.len() <= 8 && rat.bytes().all(|b| b.is_ascii_alphanumeric())
}

pub fn net_confirm(act: &Act, remote: Via, name: &str, l: Lang) -> write::Confirm {
    let inline = |title: &str, what: &str, action: &str, cuts: bool| write::Confirm {
        tier: if remote.remote() && cuts { write::Tier::Dialog } else { write::Tier::Inline },
        title: title.into(),
        what: what.into(),
        downtime: None,
        recovery: None,
        action: action.into(),
        cuts_uplink: cuts,
        word: None,
        danger: false,
        revert: crate::write::Revert::No,
    };
    match act {
        // tier 2; tier 3 over Tailscale (turning data off can cut the page's own uplink)
        Act::Data(true) => inline(l.pick("Turn mobile data on?", "打开移动数据？"), l.pick("The U60 connects to the mobile network again.", "U60 重新连上移动网络。"), l.pick("Confirm: turn mobile data on", "确认：打开移动数据"), false),
        Act::Data(false) => {
            let mut c = inline(l.pick("Turn mobile data off?", "关闭移动数据？"), l.pick("Devices on the U60 lose internet until mobile data is turned back on. Wi-Fi and this page keep working.", "连着 U60 的设备会断网，直到重新打开移动数据。Wi-Fi 和这个页面照常可用。"), l.pick("Confirm: turn mobile data off", "确认：关闭移动数据"), true);
            if c.tier == write::Tier::Dialog {
                c.downtime = Some(l.pick("Until it is turned back on. Over Tailscale this page goes away with it.", "直到重新打开为止。经 Tailscale 访问时，这个页面也会一起断开。").into());
                c.recovery = Some(l.pick("Someone on the U60's Wi-Fi opens this page and turns it back on, or uses the device's touchscreen.", "请连着 U60 Wi-Fi 的人打开这个页面重新打开移动数据，或在设备触屏上打开。").into());
            }
            c
        }
        Act::Roam(true) => inline(l.pick("Allow data roaming?", "允许数据漫游？"), l.pick("The U60 may use partner networks abroad; your carrier may charge roaming fees.", "在国外 U60 可能连到合作运营商的网络，运营商可能收漫游费。"), l.pick("Confirm: allow roaming", "确认：允许漫游"), false),
        Act::Roam(false) => {
            let mut c = inline(l.pick("Turn data roaming off?", "关闭数据漫游？"), l.pick("If the U60 is roaming right now, it loses its mobile connection.", "如果 U60 现在正在漫游，会断开移动网络。"), l.pick("Confirm: turn roaming off", "确认：关闭漫游"), true);
            if c.tier == write::Tier::Dialog {
                c.downtime = Some(l.pick("Until it is turned back on. Over Tailscale this page goes away with it.", "直到重新打开为止。经 Tailscale 访问时，这个页面也会一起断开。").into());
                c.recovery = Some(l.pick("Someone on the U60's Wi-Fi opens this page and turns it back on, or uses the device's touchscreen.", "请连着 U60 Wi-Fi 的人打开这个页面重新打开，或在设备触屏上打开。").into());
            }
            c
        }
        Act::Scan => write::Confirm {
            tier: write::Tier::Dialog,
            title: l.pick("Scan for carriers?", "扫描运营商？").into(),
            what: l.pick("The modem searches every carrier nearby.", "模组会搜索附近所有运营商。").into(),
            downtime: Some(l.pick("One to three minutes (about 110 seconds measured): mobile data is off while it searches.", "约 1–3 分钟（实测约 110 秒），搜索期间移动数据断开。").into()),
            recovery: Some(l.pick("Mobile data comes back by itself when the search ends.", "搜索结束后移动数据会自己恢复。").into()),
            action: l.pick("Scan for Carriers", "扫描运营商").into(),
            cuts_uplink: true,
            word: None,
            danger: false,
            revert: crate::write::Revert::No,
        },
        Act::Register { plmn, .. } => write::Confirm {
            tier: write::Tier::Dialog,
            title: match l {
                Lang::En => format!("Register to {name}?"),
                Lang::Zh => format!("注册到 {name}？"),
            },
            what: match l {
                Lang::En => format!("The modem leaves automatic network selection and registers only to {name} ({plmn})."),
                Lang::Zh => format!("调制解调器退出自动选网，只注册到 {name}（{plmn}）。"),
            },
            downtime: Some(l.pick("The mobile connection drops for about 30 seconds while the modem registers. If it is refused, the device goes back to automatic selection.", "注册时移动网络断开约 30 秒；如果被拒绝，设备会自己回到自动选网。").into()),
            recovery: Some(l.pick("If it does not take, the device goes back to automatic selection by itself within about two minutes. You can also press Back to Automatic below.", "注册不上时，设备会在约 2 分钟内自己回到自动选网；也可以点下面的「回到自动选网」。").into()),
            action: l.pick("Register", "注册").into(),
            cuts_uplink: true,
            word: None,
            danger: false,
            revert: crate::write::Revert::No,
        },
        Act::Auto => write::Confirm {
            tier: write::Tier::Inline,
            title: l.pick("Back to Automatic", "回到自动选网").into(),
            what: l.pick("The modem picks the network by itself again. Mobile data may drop for about half a minute while it re-registers.", "让模组重新自动选择网络，重新注册时移动数据可能断开半分钟左右。").into(),
            downtime: None,
            recovery: None,
            action: l.pick("Confirm: back to automatic", "确认：回到自动选网").into(),
            cuts_uplink: false,
            word: None,
            danger: false,
            revert: crate::write::Revert::No,
        },
        Act::Reboot => write::Confirm {
            tier: write::Tier::Dialog,
            title: l.pick("Reboot the U60?", "重启 U60？").into(),
            what: l.pick("Reboot the router now?", "现在重启路由器？").into(),
            downtime: Some(l.pick("About 90 seconds: Wi-Fi, the mobile connection and this page all go away.", "约 90 秒：Wi-Fi、移动网络和这个页面都会断开。").into()),
            recovery: Some(l.pick("Wait another minute, then reconnect to the U60's Wi-Fi. If the screen stays dark, hold the power button to start it.", "再等一分钟，然后重新连上 U60 的 Wi-Fi。如果屏幕一直不亮，长按电源键开机。").into()),
            action: l.pick("Reboot", "重启").into(),
            cuts_uplink: true,
            word: None,
            danger: false,
            revert: crate::write::Revert::No,
        },
    }
}

/// Why an action can't be asked / sent now.
fn net_gate(act: &Act, app: &Shared, l: Lang) -> Result<(), write::Outcome> {
    let (c, stuck, session) = app.hub.read(|st| {
        (
            cell(st),
            st.stuck,
            st.blocks.get("op").is_some_and(|b| b.data["active"]["item"].as_str().is_some_and(|i| i.starts_with("netselect"))),
        )
    });
    match act {
        Act::Data(_) | Act::Roam(_) | Act::Reboot if stuck => Err(write::Outcome::NotYet(stuck_reason(l).into())),
        Act::Data(_) | Act::Roam(_) if c.is_none() => Err(write::Outcome::NotYet(l.pick("Mobile data settings not read yet; try again shortly", "还没读到移动数据设置，稍后再试").into())),
        Act::Data(_) | Act::Roam(_) | Act::Scan | Act::Register { .. } | Act::Auto if session => Err(write::Outcome::Busy),
        Act::Register { plmn, rat } if !reg_ok(plmn, rat) => Err(write::Outcome::Failed(l.pick("not a carrier from the scan", "不是扫描到的运营商").into())),
        _ => Ok(()),
    }
}

#[derive(Deserialize, Default)]
pub struct NetQuery {
    #[serde(default)]
    on: bool,
    #[serde(default)]
    plmn: String,
    #[serde(default)]
    rat: String,
}

/// The register target's name from the agent's last scan (the PLMN when it is not there).
async fn op_name(app: &Shared, who: &Caller, plmn: &str, rat: &str, l: Lang) -> String {
    crate::agent::get(app, who, "/api/netinfo?lite=1")
        .await
        .ok()
        .and_then(|ni| Jobs::of(&ni, l).ops.into_iter().find(|o| o.plmn == plmn && o.rat == rat).map(|o| o.name))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| plmn.to_string())
}

async fn net_ask(app: Shared, act: Act, req: axum::extract::Request) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let ctx = crate::topbar::Ctx::of_request(&req, NET_PATH);
    let l = ctx.lang;
    let who = Caller::of(&app, &req);
    let phase = match net_gate(&act, &app, l) {
        Err(o) => write::Phase::Done(o),
        Ok(()) => {
            let name = match &act {
                Act::Register { plmn, rat } => op_name(&app, &who, plmn, rat, l).await,
                _ => String::new(),
            };
            write::Phase::Ask(net_confirm(&act, ctx.remote, &name, l))
        }
    };
    one(net_area(&act, l, ctx.remote, &phase))
}

/// Poll the agent's netinfo every 3 s until `done` says how it ended (None = still running, with
/// the progress line), the deadline, or the tab goes away (None).
async fn wait_agent(
    app: &Shared,
    who: &Caller,
    tx: &tokio::sync::mpsc::Sender<Event>,
    wait: Duration,
    l: Lang,
    pending: impl Fn(String) -> String,
    done: impl Fn(&Jobs) -> Result<write::Outcome, Option<String>>,
    late: write::Outcome,
) -> Option<write::Outcome> {
    let deadline = tokio::time::Instant::now() + wait;
    let mut said = String::new();
    loop {
        tokio::select! {
            _ = tokio::time::sleep(JOB_POLL) => {}
            _ = tx.closed() => return None,
        }
        if let Ok(ni) = crate::agent::get(app, who, "/api/netinfo?lite=1").await {
            match done(&Jobs::of(&ni, l)) {
                Ok(o) => return Some(o),
                Err(Some(t)) if t != said => {
                    said = t.clone();
                    if tx.send(patch(&pending(t))).await.is_err() {
                        return None;
                    }
                }
                Err(_) => {}
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Some(late);
        }
    }
}

async fn net_run(app: Shared, act: Act, who: Caller, l: Lang, tx: tokio::sync::mpsc::Sender<Event>) {
    if let Err(o) = net_gate(&act, &app, l) {
        let _ = tx.send(patch(&net_area(&act, l, Via::Local, &write::Phase::Done(o)))).await;
        return;
    }
    let pending = |t: String| net_area(&act, l, Via::Local, &write::Phase::Pending(t));
    let first = match &act {
        Act::Scan => l.pick("Starting the scan…", "正在开始扫描…"),
        Act::Register { .. } => l.pick("Registering…", "正在注册…"),
        Act::Auto => l.pick("Going back to automatic selection…", "正在回到自动选网…"),
        Act::Reboot => l.pick("Rebooting…", "正在重启…"),
        _ => "",
    };
    if tx.send(patch(&pending(first.into()))).await.is_err() {
        return;
    }
    let refused = |e: Fail| match e {
        Fail::Refused(409, ..) => write::Outcome::Failed(e.text(l)),
        e => write::Outcome::Failed(e.text(l)),
    };
    let o = match &act {
        Act::Data(on) | Act::Roam(on) => {
            let on = *on;
            let data = matches!(act, Act::Data(_));
            let params = match (data, on) {
                (true, true) => json!({"enabled": 1, "connect_mode": "1"}),
                (true, false) => json!({"enabled": 0}),
                (false, r) => json!({"roaming": r}),
            };
            let w = write::Write {
                action: "cellular.set",
                params,
                readback: Some(Box::new(move |st| cell(st).is_some_and(|c| if data { c.data == Some(on) } else { c.roam == Some(on) }))),
                applied: match (data, on, l) {
                    (true, true, _) => l.pick("Mobile data on", "已打开移动数据"),
                    (true, false, _) => l.pick("Mobile data off", "已关闭移动数据"),
                    (false, true, _) => l.pick("Data roaming on", "已打开数据漫游"),
                    (false, false, _) => l.pick("Data roaming off", "已关掉数据漫游"),
                }
                .into(),
                timeout: write::READBACK,
                typed: None,
                request_timeout: None,
            };
            write::execute(&app.http, &app.control_url, &app.hub, &w, l).await
        }
        Act::Reboot => {
            let body = json!({"action": "device.reboot", "params": {}, "source": "web", "op_id": write::next_op_id()});
            match write::send_body(&app.http, &app.control_url, &body, Some(SEND_TIMEOUT), l).await {
                Ok(_) => write::Outcome::AcceptedSaid(l.pick("Accepted by the device · rebooting; this page comes back in about 90 seconds", "设备已接受 · 正在重启，约 90 秒后再打开本页").into()),
                Err(o) => o,
            }
        }
        Act::Scan => match crate::agent::post(&app, &who, "/api/netinfo/scan", &json!({})).await {
            Err(e) => refused(e),
            Ok(_) => {
                let _ = app.kick.send(NET_PATH);
                let r = wait_agent(&app, &who, &tx, SCAN_WAIT, l, &pending, |j| match j.scan.as_str() {
                    "done" if j.ops.is_empty() => Ok(write::Outcome::NotYet(l.pick("No carriers found · check the antenna position and scan again.", "没有找到运营商 · 调整设备位置后再扫描一次。").into())),
                    "done" => Ok(write::Outcome::Applied(match l {
                        Lang::En => format!("{} carriers found", j.ops.len()),
                        Lang::Zh => format!("找到 {} 个运营商", j.ops.len()),
                    })),
                    "error" => Ok(write::Outcome::Failed(if j.scan_err.is_empty() { l.pick("The scan failed", "搜索失败").into() } else { j.scan_err.clone() })),
                    _ => Err(Some(l.pick("One to three minutes; mobile data is off while it searches…", "约 1–3 分钟，搜索期间移动数据会断开…").into())),
                }, write::Outcome::Ended(l.pick("Carrier scan timed out", "扫描运营商超时").into(), "warn"))
                .await;
                let Some(r) = r else { return };
                r
            }
        },
        Act::Register { plmn, rat } => {
            let name = op_name(&app, &who, plmn, rat, l).await;
            match crate::agent::post(&app, &who, "/api/modem/register", &json!({"m_mcc_mnc": plmn, "m_rat": rat})).await {
                Err(e) => refused(e),
                Ok(_) => {
                    let _ = app.kick.send(NET_PATH);
                    let r = wait_agent(&app, &who, &tx, GUARD_WAIT, l, &pending, |j| match j.guard.as_str() {
                        "ok" => Ok(write::Outcome::Applied(match l {
                            Lang::En => format!("Registered to {name}"),
                            Lang::Zh => format!("已注册到 {name}"),
                        })),
                        "reverted" => Ok(write::Outcome::Ended(l.pick("Could not register; back to automatic selection", "没注册上，已回到自动选网").into(), "bad")),
                        _ => Err(Some(match l {
                            Lang::En => format!("Registering to {name}… If it does not take, the device goes back to automatic selection."),
                            Lang::Zh => format!("正在注册到 {name}…注册不上会自动回到自动选网。"),
                        })),
                    }, write::Outcome::Ended(l.pick("Registration timed out", "注册超时").into(), "warn"))
                    .await;
                    let Some(r) = r else { return };
                    r
                }
            }
        }
        Act::Auto => match crate::agent::post(&app, &who, "/api/modem/netselect/auto", &json!({})).await {
            Err(e) => refused(e),
            Ok(_) => {
                let _ = app.kick.send(NET_PATH);
                let r = wait_agent(&app, &who, &tx, GUARD_WAIT, l, &pending, |j| match j.guard.as_str() {
                    "reverted" | "ok" => Ok(write::Outcome::Applied(l.pick("Back to automatic selection", "已回到自动选网").into())),
                    _ => Err(Some(l.pick("Going back to automatic selection…", "正在回到自动选网…").into())),
                }, write::Outcome::Accepted)
                .await;
                let Some(r) = r else { return };
                r
            }
        },
    };
    let _ = app.kick.send(NET_PATH);
    let _ = tx.send(patch(&net_area(&act, l, Via::Local, &write::Phase::Done(o)))).await;
}

async fn net_set(app: Shared, act: Act, req: axum::extract::Request) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let l = crate::topbar::Ctx::of_request(&req, NET_PATH).lang;
    let who = Caller::of(&app, &req);
    Sse::new(async_stream(move |tx| net_run(app, act, who, l, tx)))
}

fn net_cancel(act: Act, q: &write::CancelQuery, req: &axum::extract::Request) -> String {
    let ctx = crate::topbar::Ctx::of_request(req, NET_PATH);
    net_area(&act, ctx.lang, ctx.remote, &write::cancel_phase(q))
}

pub async fn data_ask(State(app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    let on = app.hub.read(|st| cell(st).and_then(|c| c.data)).is_none_or(|v| !v);
    net_ask(app, Act::Data(on), req).await
}
pub async fn data_set(State(app): State<Shared>, Query(q): Query<NetQuery>, req: axum::extract::Request) -> impl IntoResponse {
    net_set(app, Act::Data(q.on), req).await
}
pub async fn data_cancel(Query(q): Query<write::CancelQuery>, req: axum::extract::Request) -> impl IntoResponse {
    one(net_cancel(Act::Data(false), &q, &req))
}
pub async fn roam_ask(State(app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    let on = app.hub.read(|st| cell(st).and_then(|c| c.roam)).is_none_or(|v| !v);
    net_ask(app, Act::Roam(on), req).await
}
pub async fn roam_set(State(app): State<Shared>, Query(q): Query<NetQuery>, req: axum::extract::Request) -> impl IntoResponse {
    net_set(app, Act::Roam(q.on), req).await
}
pub async fn roam_cancel(Query(q): Query<write::CancelQuery>, req: axum::extract::Request) -> impl IntoResponse {
    one(net_cancel(Act::Roam(false), &q, &req))
}
pub async fn scan_ask(State(app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    net_ask(app, Act::Scan, req).await
}
pub async fn scan_set(State(app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    net_set(app, Act::Scan, req).await
}
pub async fn scan_cancel(Query(q): Query<write::CancelQuery>, req: axum::extract::Request) -> impl IntoResponse {
    one(net_cancel(Act::Scan, &q, &req))
}
pub async fn reg_ask(State(app): State<Shared>, Query(q): Query<NetQuery>, req: axum::extract::Request) -> impl IntoResponse {
    net_ask(app, Act::Register { plmn: q.plmn, rat: q.rat }, req).await
}
pub async fn reg_set(State(app): State<Shared>, Query(q): Query<NetQuery>, req: axum::extract::Request) -> impl IntoResponse {
    net_set(app, Act::Register { plmn: q.plmn, rat: q.rat }, req).await
}
pub async fn reg_cancel(Query(q): Query<write::CancelQuery>, req: axum::extract::Request) -> impl IntoResponse {
    one(net_cancel(Act::Register { plmn: String::new(), rat: String::new() }, &q, &req))
}
pub async fn auto_ask(State(app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    net_ask(app, Act::Auto, req).await
}
pub async fn auto_set(State(app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    net_set(app, Act::Auto, req).await
}
pub async fn auto_cancel(Query(q): Query<write::CancelQuery>, req: axum::extract::Request) -> impl IntoResponse {
    one(net_cancel(Act::Auto, &q, &req))
}
pub async fn reboot_ask(State(app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    net_ask(app, Act::Reboot, req).await
}
pub async fn reboot_set(State(app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    net_set(app, Act::Reboot, req).await
}
pub async fn reboot_cancel(Query(q): Query<write::CancelQuery>, req: axum::extract::Request) -> impl IntoResponse {
    one(net_cancel(Act::Reboot, &q, &req))
}

pub struct NetPoll {
    l: Lang,
    who: Caller,
    jobs: Option<Result<Jobs, Fail>>,
}

impl Polled for NetPoll {
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        let j = fetch_jobs(app, &self.who, self.l).await;
        let busy = j.as_ref().is_ok_and(|j| j.scanning() || j.guarding());
        self.jobs = Some(j);
        (net_leaves(app, self.jobs.as_ref(), self.l), if busy { JOB_POLL } else { Duration::from_secs(30) })
    }
    fn watches(&self, block: &str) -> bool {
        matches!(block, "interfaces" | "op" | "exec")
    }
    fn render(&mut self, app: &Shared) -> Option<Vec<(String, String)>> {
        Some(net_leaves(app, self.jobs.as_ref(), self.l))
    }
}

#[derive(Template)]
#[template(path = "mobilenet.html")]
struct NetPage {
    shell: crate::shell::Shell,
    l: Lang,
    leaves: Vec<(String, String)>,
    ops: Vec<String>,
}

impl NetPage {
    fn leaf(&self, id: &str) -> &str {
        self.leaves.iter().find(|(k, _)| k == id).map_or("", |(_, h)| h.as_str())
    }
}

pub async fn net_page(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, NET_PATH);
    let l = ctx.lang;
    let shell = app.hub.read(|st| {
        crate::shell::Shell::new(&headers, NET_PATH).live(st, &ctx, Some(format!("{NET_PATH}/events?g={}", st.generation)))
    });
    let idle = write::Phase::Idle;
    let ops = [Act::Data(false), Act::Roam(false), Act::Scan, Act::Register { plmn: String::new(), rat: String::new() }, Act::Auto, Act::Reboot]
        .iter()
        .map(|a| net_area(a, l, ctx.remote, &idle))
        .collect();
    let html = NetPage { shell, l, leaves: net_leaves(&app, None, l), ops }.render().unwrap();
    crate::assets::maybe_gzip(&headers, "text/html; charset=utf-8", html.into_bytes(), "no-store")
}

pub async fn net_events(State(app): State<Shared>, Query(q): Query<StreamQuery>, req: axum::extract::Request) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, NET_PATH);
    let p = NetPoll { l: ctx.lang, who: Caller::of(&app, &req), jobs: None };
    crate::agent::stream(app, ctx, q.g, NET_PATH, p)
}

#[cfg(test)]
mod tests;
