//! /router/dualsim (双卡), new with the global edition (manager `docs/designs/dual-sim-global.md`):
//! the two SIM slots, single / dual mode, the main slot and smart switching. Only reached from
//! /router/sim when the device reports dual SIM (the route is hidden: search only); on a
//! single-slot device the page says so and offers nothing.
//!
//! Where each value comes from (u60d's `sim` block, `dual`; the agent is not asked):
//! `mode`, `active_slot`, `smart`, `switching` and per slot `provisioned`, `present`, `ready`,
//! `operator`, `mcc`/`mnc`, `iccid_tail`. The serving country for the roaming note is the
//! `signal` block's `serving_mcc`.
//!
//! One write, tier 3, only through a u60d `/control` transaction: `sim.dual {mode, active_slot,
//! smart}` (u60d checks the slot has a card and refuses while the firmware is switching on its
//! own; data comes back within 120 s or u60d puts the old setting back). Followed in the `op`
//! block (follow.rs). The dialog warns about roaming when the card that will carry data is from
//! another country than the network the device is on, or when that can't be told (a slot that is
//! off reports no MCC) — D4 of the eng review.
//!
//! The form is page-local signals (`_dm`, `_ds`, `_dsm`), outside the pushed leaves.

use std::convert::Infallible;
use std::time::Duration;

use askama::Template;
use axum::extract::{Query, State};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::Polled;
use crate::hub::{Link, St};
use crate::i18n::Lang;
use crate::pages::{Shared, StreamQuery, async_stream, patch, split_leaves};
use crate::topbar::Via;
use crate::write;

pub const PATH: &str = "/router/dualsim";
const FOLLOW: Duration = Duration::from_secs(130);
const SEND_TIMEOUT: Duration = Duration::from_secs(30);
const AREA: &str = "ds";

// ---- the setting ----

/// What the form asks for; the same shape as u60d's `sim.dual` value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Want {
    pub dual: bool,
    /// None only with smart on (the firmware picks).
    pub slot: Option<u8>,
    pub smart: bool,
}

impl Want {
    pub fn params(&self) -> Value {
        match (self.dual, self.smart) {
            (false, _) => json!({"mode": "single", "active_slot": self.slot}),
            (true, true) => json!({"mode": "dual", "smart": true}),
            (true, false) => json!({"mode": "dual", "smart": false, "active_slot": self.slot}),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Slot {
    pub n: u8,
    pub provisioned: bool,
    pub present: bool,
    pub ready: Option<bool>,
    pub operator: String,
    pub mcc: String,
    pub mnc: String,
    pub tail: String,
    /// 本地 / 漫游到X in the core's words ("" unknown)
    pub place: String,
    pub place_en: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Dual {
    pub dual: bool,
    pub active: u8,
    pub smart: bool,
    pub switching: bool,
    pub slots: Vec<Slot>,
}

fn sim(st: &St) -> Option<(&Value, bool)> {
    st.blocks
        .get("sim")
        .map(|b| (&b.data, b.stale || st.link != Link::Live))
}

/// The `dual` view; None = no sim block yet, Some(None) = single-slot device.
pub fn dual(st: &St) -> Option<Option<Dual>> {
    let (d, _) = sim(st)?;
    Some(parse(&d["dual"]))
}

pub fn parse(d: &Value) -> Option<Dual> {
    let o = d.as_object()?;
    let s = |v: &Value, k: &str| v[k].as_str().unwrap_or("").to_string();
    Some(Dual {
        dual: o.get("mode").and_then(Value::as_str) == Some("dual"),
        active: d["active_slot"].as_u64().unwrap_or(0) as u8,
        smart: d["smart"] == true,
        switching: d["switching"] == true,
        slots: d["slots"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|v| Slot {
                        n: v["slot"].as_u64().unwrap_or(0) as u8,
                        provisioned: v["provisioned"] == true,
                        present: v["present"] == true,
                        ready: v["ready"].as_bool(),
                        operator: s(v, "operator"),
                        mcc: s(v, "mcc"),
                        mnc: s(v, "mnc"),
                        tail: s(v, "iccid_tail"),
                        place: s(v, "place"),
                        place_en: s(v, "place_en"),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    })
}

impl Dual {
    pub fn want(&self) -> Want {
        Want {
            dual: self.dual,
            slot: (!(self.dual && self.smart)).then_some(self.active),
            smart: self.dual && self.smart,
        }
    }
    fn slot(&self, n: u8) -> Option<&Slot> {
        self.slots.iter().find(|s| s.n == n)
    }
}

pub fn slot_name(n: u8, l: Lang) -> &'static str {
    match n {
        1 => l.pick("Slot 1 (SIM card)", "卡槽 1（插拔卡）"),
        2 => l.pick("Slot 2 (built-in)", "卡槽 2（内置卡）"),
        _ => "—",
    }
}

fn stuck_reason(l: Lang) -> &'static str {
    l.pick(
        "Data service not responding · settings can't be changed for now",
        "数据服务没响应 · 暂时不能改设置",
    )
}

// ---- the view ----

pub struct SlotRow {
    pub name: &'static str,
    pub state: String,
    pub carrier: String,
    pub tail: String,
}

pub struct View {
    pub l: Lang,
    pub stale: bool,
    /// None: not read yet or single-slot (`note` says which).
    pub mode: String,
    pub main: String,
    pub smart: String,
    pub slots: Vec<SlotRow>,
    pub note: Option<String>,
    pub dis: bool,
    /// Not a single-slot device (not read yet counts as yes): the form shows.
    pub supported: bool,
    pub why: Option<String>,
    pub txn: Option<String>,
}

pub fn view(st: &St, l: Lang) -> View {
    let dash = || "—".to_string();
    let read = dual(st);
    let d = read.clone().flatten();
    let txn = crate::follow::running(st, &["sim.dual"], l);
    let note = match &read {
        None => Some(l.pick("SIM not read yet", "还没读到 SIM").to_string()),
        Some(None) => Some(l.pick("This device has one SIM slot.", "这台设备只有一个卡槽。").to_string()),
        Some(Some(d)) if d.switching => Some(
            l.pick("The firmware is switching SIMs on its own; wait for it to finish.", "原厂正在自动切换主卡，等它切完再改。").to_string(),
        ),
        _ => None,
    };
    let slots = d
        .as_ref()
        .map(|d| {
            d.slots
                .iter()
                .map(|s| SlotRow {
                    name: slot_name(s.n, l),
                    state: match (s.present, s.provisioned, s.ready) {
                        (false, _, _) => l.pick("No SIM", "没有卡"),
                        (_, false, _) => l.pick("Off", "未启用"),
                        (_, true, Some(true)) => l.pick("On · ready", "已启用 · 就绪"),
                        (_, true, _) => l.pick("On · not ready", "已启用 · 未就绪"),
                    }
                    .into(),
                    carrier: {
                        let c = match (s.operator.is_empty(), s.mcc.is_empty()) {
                            (true, true) => dash(),
                            (false, true) => s.operator.clone(),
                            (true, false) => format!("{}-{}", s.mcc, s.mnc),
                            (false, false) => format!("{} · {}-{}", s.operator, s.mcc, s.mnc),
                        };
                        let place = if matches!(l, Lang::En) { &s.place_en } else { &s.place };
                        if place.is_empty() { c } else { format!("{c} · {place}") }
                    },
                    tail: if s.tail.is_empty() { dash() } else { format!("…{}", s.tail) },
                })
                .collect()
        })
        .unwrap_or_default();
    View {
        l,
        stale: sim(st).is_some_and(|(_, s)| s),
        mode: d.as_ref().map_or_else(dash, |d| {
            if d.dual { l.pick("Dual SIM", "双卡") } else { l.pick("Single SIM", "单卡") }.into()
        }),
        main: d.as_ref().map_or_else(dash, |d| slot_name(d.active, l).into()),
        smart: d.as_ref().map_or_else(dash, |d| {
            if !d.dual {
                dash()
            } else if d.smart {
                l.pick("On", "开").into()
            } else {
                l.pick("Off", "关").into()
            }
        }),
        slots,
        note,
        dis: st.stuck || txn.is_some() || d.as_ref().is_none_or(|d| d.switching),
        supported: !matches!(read, Some(None)),
        why: st.stuck.then(|| stuck_reason(l).to_string()),
        txn,
    }
}

#[derive(Template)]
#[template(path = "dualsim_leaves.html")]
struct Leaves<'a> {
    v: &'a View,
}

pub fn leaves(app: &Shared, l: Lang) -> Vec<(String, String)> {
    app.hub
        .read(|st| split_leaves(&Leaves { v: &view(st, l) }.render().unwrap()))
}

// ---- writes ----

#[derive(Deserialize, Default)]
pub struct DualQuery {
    #[serde(default)]
    mode: String,
    #[serde(default)]
    slot: String,
    #[serde(default)]
    smart: String,
}

/// The form's ask, checked against what the page read; Err = why it is not sent.
pub fn check(q: &DualQuery, d: Option<&Dual>, stuck: bool, l: Lang) -> Result<Want, write::Outcome> {
    let no = |zh: &'static str, en: &'static str| Err(write::Outcome::NotYet(l.pick(en, zh).into()));
    if stuck {
        return Err(write::Outcome::NotYet(stuck_reason(l).into()));
    }
    let Some(d) = d else {
        return no("还没读到双卡状态，稍后再试", "Dual SIM not read yet; try again shortly");
    };
    if d.switching {
        return no("原厂正在自动切换主卡，等它切完再改", "The firmware is switching SIMs; try again when it's done");
    }
    let dual = match q.mode.as_str() {
        "dual" => true,
        "single" => false,
        _ => return no("选单卡或双卡", "Choose single or dual"),
    };
    let smart = dual && q.smart == "1";
    let slot = match q.slot.as_str() {
        "1" => Some(1),
        "2" => Some(2),
        _ if smart => None,
        _ => return no("选主卡", "Choose the main slot"),
    };
    let w = Want { dual, slot: if smart { None } else { slot }, smart };
    if let Some(n) = w.slot
        && !d.slot(n).is_some_and(|s| s.present)
    {
        return no("这个卡槽没有卡", "There is no SIM in that slot");
    }
    if dual && !d.slots.iter().all(|s| s.present) {
        return no("双卡要两个卡槽都有卡", "Dual SIM needs a SIM in both slots");
    }
    if w == d.want() {
        return no("已经是这样了", "Already set this way");
    }
    Ok(w)
}

pub fn read_back(w: &Want, st: &St) -> bool {
    dual(st).flatten().is_some_and(|d| d.want() == *w)
}

/// The roaming note (D4): the card that will carry data is from another country than the network,
/// or one of the two can't be read. With smart on either card may carry data.
pub fn roaming_note(w: &Want, d: &Dual, serving_mcc: Option<i64>, l: Lang) -> Option<String> {
    let cards: Vec<&Slot> = match w.slot {
        Some(n) => d.slot(n).into_iter().collect(),
        None => d.slots.iter().collect(),
    };
    let unsure = serving_mcc.is_none() || cards.iter().any(|s| s.mcc.is_empty());
    let away = serving_mcc.is_some_and(|m| cards.iter().any(|s| s.mcc.parse::<i64>().is_ok_and(|c| c != m)));
    if away {
        Some(
            l.pick(
                "That card is from another country than the network here: it will roam, which can cost a lot.",
                "这张卡和这里的网络不是同一个国家：会漫游，可能很贵。",
            )
            .into(),
        )
    } else if unsure {
        Some(
            l.pick(
                "Can't tell whether that card roams here (a slot that is off reports no country). Check its roaming charges first.",
                "看不出这张卡在这里算不算漫游（没启用的卡槽读不到国家）。先确认它的漫游费用。",
            )
            .into(),
        )
    } else {
        None
    }
}

fn describe(w: &Want, l: Lang) -> String {
    match (w.dual, w.smart, w.slot) {
        (false, _, Some(n)) => match l {
            Lang::En => format!("single SIM on {}", slot_name(n, l)),
            Lang::Zh => format!("单卡，用{}", slot_name(n, l)),
        },
        (true, true, _) => l.pick("dual SIM with smart switching", "双卡，智能切换").into(),
        (true, false, Some(n)) => match l {
            Lang::En => format!("dual SIM, main {}", slot_name(n, l)),
            Lang::Zh => format!("双卡，主卡{}", slot_name(n, l)),
        },
        _ => String::new(),
    }
}

fn applied(w: &Want, l: Lang) -> String {
    match l {
        Lang::En => format!("Now {}", describe(w, l)),
        Lang::Zh => format!("已切换：{}", describe(w, l)),
    }
}

pub fn confirm(w: &Want, roam: Option<String>, l: Lang) -> write::Confirm {
    let mut what = match l {
        Lang::En => format!("Switch to {}.", describe(w, l)),
        Lang::Zh => format!("切换成{}。", describe(w, l)),
    };
    if w.smart {
        what.push(' ');
        what.push_str(l.pick(
            "The firmware then picks the main card by itself.",
            "之后由原厂自己选主卡。",
        ));
    }
    if let Some(r) = roam {
        what.push(' ');
        what.push_str(&r);
    }
    write::Confirm {
        tier: write::Tier::Dialog,
        title: l.pick("Switch SIM?", "切换 SIM 卡？").into(),
        what,
        downtime: Some(
            l.pick(
                "Mobile data drops for about 10 seconds while the modem switches cards.",
                "切卡时移动数据会断开约 10 秒。",
            )
            .into(),
        ),
        recovery: Some(
            l.pick(
                "If data isn't back within 2 minutes, the device puts the old setting back by itself. You can also switch back on this page.",
                "如果 2 分钟内数据没恢复，设备会自己改回原来的设置；也可以在这个页面切回去。",
            )
            .into(),
        ),
        action: l.pick("Switch", "切换").into(),
        cuts_uplink: true,
        word: None,
        danger: false,
        revert: write::Revert::No,
    }
}

fn area(l: Lang, remote: Via, w: Option<&Want>, phase: &write::Phase) -> String {
    let set = match w {
        Some(w) => format!(
            "{PATH}/set?mode={}&slot={}&smart={}",
            if w.dual { "dual" } else { "single" },
            w.slot.map_or(String::new(), |n| n.to_string()),
            if w.smart { "1" } else { "0" }
        ),
        None => format!("{PATH}/set"),
    };
    let cancel = format!("{PATH}/cancel");
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

fn state(app: &Shared) -> (Option<Dual>, bool, Option<i64>) {
    app.hub.read(|st| {
        (
            dual(st).flatten(),
            st.stuck,
            st.blocks.get("signal").and_then(|b| b.data["serving_mcc"].as_i64()),
        )
    })
}

pub async fn ask(State(app): State<Shared>, Query(q): Query<DualQuery>, req: axum::extract::Request) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let (d, stuck, mcc) = state(&app);
    let phase = match check(&q, d.as_ref(), stuck, l) {
        Err(o) => write::Phase::Done(o),
        Ok(w) => {
            let roam = d.as_ref().and_then(|d| roaming_note(&w, d, mcc, l));
            let revert = app.hub.read(|st| write::Revert::txn(st.blocks.get("op").map(|b| &b.data)));
            let c = write::Confirm { revert, ..confirm(&w, roam, l) };
            return one(area(l, ctx.remote, Some(&w), &write::Phase::Ask(c)));
        }
    };
    one(area(l, ctx.remote, None, &phase))
}

pub async fn set(State(app): State<Shared>, Query(q): Query<DualQuery>, req: axum::extract::Request) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    Sse::new(async_stream(move |tx| async move {
        let (d, stuck, _) = state(&app);
        let w = match check(&q, d.as_ref(), stuck, l) {
            Ok(w) => w,
            Err(o) => {
                let _ = tx.send(patch(&area(l, Via::Local, None, &write::Phase::Done(o)))).await;
                return;
            }
        };
        let pending = |t: String| area(l, Via::Local, Some(&w), &write::Phase::Pending(t));
        if tx.send(patch(&pending(l.pick("Switching…", "正在切换…").into()))).await.is_err() {
            return;
        }
        let op_id = write::next_op_id();
        let body = json!({"action": "sim.dual", "params": w.params(), "source": "web", "op_id": op_id});
        let changes = app.hub.watch();
        let sent = match write::send_body(&app.http, &app.control_url, &body, Some(SEND_TIMEOUT), l).await {
            Ok(s) => s,
            Err(o) => {
                let _ = tx.send(patch(&area(l, Via::Local, Some(&w), &write::Phase::Done(o)))).await;
                return;
            }
        };
        let w2 = w.clone();
        let Some(f) = crate::follow::follow(
            &app,
            &tx,
            &op_id,
            changes,
            FOLLOW,
            Duration::from_secs(5),
            l,
            pending,
            move |st| read_back(&w2, st),
        )
        .await
        else {
            return;
        };
        let o = crate::follow::outcome(&f, sent, applied(&w, l), l);
        let _ = tx.send(patch(&area(l, Via::Local, Some(&w), &write::Phase::Done(o)))).await;
    }))
}

pub async fn cancel(Query(q): Query<write::CancelQuery>, req: axum::extract::Request) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    one(area(ctx.lang, ctx.remote, None, &write::cancel_phase(&q)))
}

// ---- the page and its stream ----

pub struct Poll {
    l: Lang,
}

impl Polled for Poll {
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        (leaves(app, self.l), Duration::from_secs(24 * 3600))
    }
    fn watches(&self, block: &str) -> bool {
        matches!(block, "sim" | "op" | "exec")
    }
    fn render(&mut self, app: &Shared) -> Option<Vec<(String, String)>> {
        Some(leaves(app, self.l))
    }
}

#[derive(Template)]
#[template(path = "dualsim.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    leaves: Vec<(String, String)>,
    /// The form starts at the current setting.
    dm: &'static str,
    ds: String,
    dsm: bool,
    supported: bool,
    op: String,
}

impl Page {
    fn leaf(&self, id: &str) -> &str {
        self.leaves.iter().find(|(k, _)| k == id).map_or("", |(_, h)| h.as_str())
    }
    fn slot_name(&self, n: u8) -> &'static str {
        slot_name(n, self.l)
    }
}

pub async fn page(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let (shell, d, supported) = app.hub.read(|st| {
        let read = dual(st);
        (
            crate::shell::Shell::new(&headers, PATH).live(st, &ctx, Some(format!("{PATH}/events?g={}", st.generation))),
            read.clone().flatten(),
            // not read yet counts as supported: the form stays disabled until the push arrives
            !matches!(read, Some(None)),
        )
    });
    let html = Page {
        shell,
        l,
        leaves: leaves(&app, l),
        dm: if d.as_ref().is_some_and(|d| d.dual) { "dual" } else { "single" },
        ds: d.as_ref().map_or("1".into(), |d| d.active.max(1).to_string()),
        dsm: d.as_ref().is_some_and(|d| d.dual && d.smart),
        supported,
        op: area(l, ctx.remote, None, &write::Phase::Idle),
    }
    .render()
    .unwrap();
    crate::assets::maybe_gzip(&headers, "text/html; charset=utf-8", html.into_bytes(), "no-store")
}

pub async fn events(State(app): State<Shared>, Query(q): Query<StreamQuery>, req: axum::extract::Request) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let p = Poll { l: ctx.lang };
    crate::agent::stream(app, ctx, q.g, PATH, p)
}

#[cfg(test)]
mod tests;
