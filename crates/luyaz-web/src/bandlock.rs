//! /bandlock (频段锁定), the old `bandlock/page.tsx`: the bands in use, the locks the firmware
//! keeps, NR and LTE band choices, and reset. Batch 3, step 3-1 (u60-web.md).
//!
//! Where each value comes from (all u60d's `signal` block; the agent is not asked):
//! - bands in use = the carriers (`view::carriers`, the same as /signal; the old page's `carriers`);
//! - locks = `lte_bands`, `sa_bands`, `nsa_bands` (comma lists; not locked = the whole default list);
//! - choices = `lte_supported_bands`; NR = `nr_sa_supported_bands` ∩ `nr_nsa_supported_bands`
//!   (one NR choice is written to both, and u60d wants both lists non-empty; on the recorded device
//!   the two lists are the same). The old page had a fixed 14 + 14 and did not preselect anything
//!   until T9c; here the chips start at the current lock.
//!
//! Writes, all tier 3 (controls-inventory §/bandlock), only through u60d `/control` transactions:
//! `band.set_lte {bands}`, `band.set_nr {sa, nsa}` (the same list: SA and NSA are one item, user
//! decision 10-12; the old page's SA/NSA scope is gone) and `band.reset {}` (also clears cell locks,
//! D22). Each send has a new `op_id`; the reply then follows that transaction in the `op` block up
//! to u60d's 120 s window, showing u60d's own words while it runs and when it ends. The op bar is not
//! shown on this page for these items (`opbar::item_route`), so the area says it all. A 409 busy is
//! a normal outcome (another change holds the device). The list in the confirm's URL is what is
//! sent (checked again: digits, in the choices, not empty).
//!
//! The chips are a draft in page-local signals (`_lte`, `_nr`, never sent to the server): pushes
//! only change the "now" rows and `_lnow`/`_nnow`/`_dis`, so a half-made selection survives them;
//! only a write's own reply resets the draft to what the device reads back.

use std::convert::Infallible;
use std::time::Duration;

use askama::Template;
use axum::extract::{Query, State};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;

use crate::agent::Polled;
use crate::changes::Seen;
use crate::hub::{Link, St};
use crate::i18n::Lang;
use crate::pages::{Shared, StreamQuery, async_stream, patch, split_leaves};
use crate::write;

pub const PATH: &str = "/bandlock";
/// u60d confirms within 120 s (readback + registered + data probe); a little more for the last frame.
const FOLLOW: Duration = Duration::from_secs(130);
/// The `/control` request itself (the agent gives a transaction 30 s).
const SEND_TIMEOUT: Duration = Duration::from_secs(30);

// ---- bands ----

/// A comma list in u60d's canonical form: numbers ascending, deduped. None: not only digits.
pub fn canon(s: &str) -> Option<Vec<u32>> {
    let mut v = Vec::new();
    for p in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        if !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        v.push(p.parse::<u32>().ok()?);
    }
    v.sort_unstable();
    v.dedup();
    Some(v)
}

pub fn join(v: &[u32]) -> String {
    v.iter().map(u32::to_string).collect::<Vec<_>>().join(",")
}

fn field(sig: &Value, k: &str) -> Vec<u32> {
    sig[k].as_str().and_then(canon).unwrap_or_default()
}

/// "B1 B3" / "n41 n78".
fn words(prefix: &str, v: &[u32]) -> String {
    v.iter()
        .map(|b| format!("{prefix}{b}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// What the `signal` block says about bands.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Bands {
    pub lte: Vec<u32>,
    pub sa: Vec<u32>,
    pub nsa: Vec<u32>,
    pub lte_sup: Vec<u32>,
    pub sa_sup: Vec<u32>,
    pub nsa_sup: Vec<u32>,
}

impl Bands {
    pub fn of(sig: &Value) -> Bands {
        Bands {
            lte: field(sig, "lte_bands"),
            sa: field(sig, "sa_bands"),
            nsa: field(sig, "nsa_bands"),
            lte_sup: field(sig, "lte_supported_bands"),
            sa_sup: field(sig, "nr_sa_supported_bands"),
            nsa_sup: field(sig, "nr_nsa_supported_bands"),
        }
    }
    /// NR choices: both lists (one choice goes to SA and NSA), or the one there is.
    pub fn nr_choices(&self) -> Vec<u32> {
        match (self.sa_sup.is_empty(), self.nsa_sup.is_empty()) {
            (false, false) => self
                .sa_sup
                .iter()
                .copied()
                .filter(|b| self.nsa_sup.contains(b))
                .collect(),
            (true, _) => self.nsa_sup.clone(),
            (false, true) => self.sa_sup.clone(),
        }
    }
    /// The NR draft to start from: SA's lock (within the choices).
    pub fn nr_now(&self) -> Vec<u32> {
        let c = self.nr_choices();
        self.sa.iter().copied().filter(|b| c.contains(b)).collect()
    }
    pub fn lte_now(&self) -> Vec<u32> {
        self.lte
            .iter()
            .copied()
            .filter(|b| self.lte_sup.contains(b))
            .collect()
    }
    /// SA and NSA locked to different lists (the touch screen still sets them one by one).
    pub fn split(&self) -> bool {
        self.sa != self.nsa
    }
    /// Nothing locked: each list is its default.
    pub fn unlocked(&self) -> bool {
        self.lte == self.lte_sup && self.sa == self.sa_sup && self.nsa == self.nsa_sup
    }
    /// "SA n78 · NSA 全部 · LTE B1 B3" or 「没锁（全部频段）」.
    pub fn locked_text(&self, l: Lang) -> String {
        if self.unlocked() {
            return l
                .pick("Not locked (all bands)", "没锁（全部频段）")
                .into();
        }
        let all = l.pick("all", "全部");
        let part = |label: &str, prefix: &str, v: &[u32], sup: &[u32]| {
            let w = if v == sup || v.is_empty() {
                all.to_string()
            } else {
                words(prefix, v)
            };
            format!("{label} {w}")
        };
        [
            part("SA", "n", &self.sa, &self.sa_sup),
            part("NSA", "n", &self.nsa, &self.nsa_sup),
            part("LTE", "B", &self.lte, &self.lte_sup),
        ]
        .join(" · ")
    }
}

fn signal(st: &St) -> Option<(&Value, bool)> {
    st.blocks
        .get("signal")
        .map(|b| (&b.data, b.stale || st.link != Link::Live))
}

pub fn bands(st: &St) -> Option<Bands> {
    signal(st).map(|(d, _)| Bands::of(d))
}

/// A band change running in u60d now (any source): its words, else None.
pub fn running(st: &St, l: Lang) -> Option<String> {
    let a = &st.blocks.get("op")?.data["active"];
    let item = a["item"].as_str()?;
    if !item.starts_with("band.") {
        return None;
    }
    Some(progress(a, l))
}

/// u60d's `say` (and its `next` with the time left): "正在确认 · 1:42 后没通就退回…".
pub(crate) fn progress(a: &Value, l: Lang) -> String {
    let say = crate::changes::pick(a, "say", l);
    let next = crate::changes::pick(a, "next", l);
    if next.is_empty() {
        return say;
    }
    let t = a["remaining_ms"]
        .as_u64()
        .map(|ms| {
            let s = ms.div_ceil(1000);
            format!("{}:{:02}", s / 60, s % 60)
        })
        .unwrap_or_default();
    format!("{say} · {}", next.replace("{t}", &t))
}

fn stuck_reason(l: Lang) -> &'static str {
    l.pick(
        "Data service not responding · settings can't be changed for now",
        "数据服务没响应 · 暂时不能改设置",
    )
}

// ---- the view ----

pub struct Chip {
    pub key: String,
    pub label: String,
    pub on: bool,
}

pub struct View {
    pub l: Lang,
    pub stale: bool,
    pub in_use: String,
    pub locked: String,
    pub split: bool,
    pub lte: Vec<Chip>,
    pub nr: Vec<Chip>,
    /// `_lnow` / `_nnow`: the current lock in the draft's form (`-` = never equal: SA ≠ NSA).
    pub lnow: String,
    pub nnow: String,
    pub dis: bool,
    pub why: Option<String>,
    pub txn: Option<String>,
}

fn chips(prefix: &str, choices: &[u32], now: &[u32]) -> Vec<Chip> {
    choices
        .iter()
        .map(|b| Chip {
            key: format!("b{b}"),
            label: format!("{prefix}{b}"),
            on: now.contains(b),
        })
        .collect()
}

impl View {
    /// `{b1: true, b3: false}` for `data-signals`.
    pub fn draft(c: &[Chip]) -> String {
        let inner = c
            .iter()
            .map(|c| format!("{}: {}", c.key, c.on))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{{{inner}}}")
    }
    /// The canonical selection of a draft signal, as a Datastar expression.
    pub fn sel(sig: &str, c: &[Chip]) -> String {
        let parts = c
            .iter()
            .map(|c| format!("${sig}.{} && '{}'", c.key, &c.key[1..]))
            .collect::<Vec<_>>()
            .join(", ");
        format!("[{parts}].filter(Boolean).join(',')")
    }
    pub fn draft_lte(&self) -> String {
        View::draft(&self.lte)
    }
    pub fn draft_nr(&self) -> String {
        View::draft(&self.nr)
    }
    pub fn sel_lte(&self) -> String {
        View::sel("_lte", &self.lte)
    }
    pub fn sel_nr(&self) -> String {
        View::sel("_nr", &self.nr)
    }
}

pub fn view(st: &St, l: Lang) -> View {
    let sig = signal(st);
    let b = sig.map(|(d, _)| Bands::of(d)).unwrap_or_default();
    let in_use = match sig {
        None => "—".to_string(),
        Some((d, _)) => {
            let cs: Vec<String> = crate::view::carriers(d)
                .iter()
                .map(|c| format!("{}{}", if c.nr { "n" } else { "B" }, c.band))
                .collect();
            if cs.is_empty() {
                l.pick("No active carriers", "没有激活的载波").into()
            } else {
                cs.join(" · ")
            }
        }
    };
    let loaded = sig.is_some() && !b.lte_sup.is_empty() && !b.nr_choices().is_empty();
    let txn = running(st, l);
    let why = if st.stuck {
        Some(stuck_reason(l).to_string())
    } else {
        None
    };
    let nr_now = b.nr_now();
    View {
        l,
        stale: sig.is_some_and(|(_, s)| s),
        in_use,
        locked: if sig.is_some() {
            b.locked_text(l)
        } else {
            "—".into()
        },
        split: sig.is_some() && b.split(),
        lte: chips("B", &b.lte_sup, &b.lte_now()),
        nr: chips("n", &b.nr_choices(), &nr_now),
        lnow: join(&b.lte_now()),
        nnow: if b.split() { "-".into() } else { join(&nr_now) },
        dis: st.stuck || txn.is_some() || !loaded,
        why,
        txn,
    }
}

#[derive(Template)]
#[template(path = "bandlock_leaves.html")]
struct Leaves<'a> {
    v: &'a View,
}

pub fn leaves(app: &Shared, l: Lang) -> Vec<(String, String)> {
    app.hub.read(|st| {
        split_leaves(
            &Leaves {
                v: &view(st, l),
            }
            .render()
            .unwrap(),
        )
    })
}

/// The hidden element whose `data-signals` puts the chips back to what the device reads now
/// (only the page render and a write's own reply send it). `_dn` changes every time: a patch with
/// the same attribute would not set the signals again (an unchanged lock after a rollback).
fn draft(app: &Shared, l: Lang) -> String {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    app.hub.read(|st| {
        let v = view(st, l);
        // nothing read yet: no empty draft (the chips' own `__ifmissing` sets it when they come)
        let mut sigs = format!("_dn: {n}");
        if !v.lte.is_empty() {
            sigs += &format!(", _lte: {}", v.draft_lte());
        }
        if !v.nr.is_empty() {
            sigs += &format!(", _nr: {}", v.draft_nr());
        }
        format!("<div id=\"bl-draft\" hidden data-signals=\"{{{sigs}}}\"></div>")
    })
}

// ---- writes ----

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Which {
    Lte,
    Nr,
    Reset,
}

impl Which {
    fn id(self) -> &'static str {
        match self {
            Which::Lte => "bl-lte",
            Which::Nr => "bl-nr",
            Which::Reset => "bl-reset",
        }
    }
    fn path(self) -> &'static str {
        match self {
            Which::Lte => "lte",
            Which::Nr => "nr",
            Which::Reset => "reset",
        }
    }
    pub fn action(self) -> &'static str {
        match self {
            Which::Lte => "band.set_lte",
            Which::Nr => "band.set_nr",
            Which::Reset => "band.reset",
        }
    }
}

fn downtime(l: Lang) -> String {
    l.pick(
        "The mobile connection drops for about 30 seconds while the modem re-attaches.",
        "调制解调器重新附着，移动网络断开约 30 秒。",
    )
    .into()
}

fn recovery(l: Lang) -> String {
    l.pick(
        "Press “Reset / unlock all” on this page. If the page can't be reached, open band lock on the device's touchscreen and restore the default.",
        "在本页点「重置 / 全部解锁」。如果打不开本页，到设备触屏的「锁频」页恢复默认。",
    )
    .into()
}

pub fn confirm(w: Which, list: &[u32], l: Lang) -> write::Confirm {
    let (title, what, action) = match w {
        Which::Lte => {
            let b = words("B", list);
            (
                match l {
                    Lang::En => format!("Lock LTE to {b}?"),
                    Lang::Zh => format!("把 LTE 锁定到 {b}？"),
                },
                l.pick(
                    "4G only uses these bands. Other LTE bands stop being used.",
                    "4G 只用这些频段，其他 LTE 频段不再使用。",
                ),
                l.pick("Lock LTE bands", "锁定 LTE 频段"),
            )
        }
        Which::Nr => {
            let b = words("n", list);
            (
                match l {
                    Lang::En => format!("Lock NR to {b}?"),
                    Lang::Zh => format!("把 NR 锁定到 {b}？"),
                },
                l.pick(
                    "5G only uses these bands, for both SA and NSA. Other NR bands stop being used.",
                    "5G 只用这些频段（SA 和 NSA 都是），其他 NR 频段不再使用。",
                ),
                l.pick("Lock NR bands", "锁定 NR 频段"),
            )
        }
        Which::Reset => (
            l.pick("Reset all band locks?", "重置所有频段锁定？").into(),
            l.pick(
                "Bands and cell lock are both reset: every band lock and every cell lock is removed, and the device picks automatically again.",
                "频段和小区锁一起恢复：所有频段锁定和小区锁定都会去掉，设备改回自动选择。",
            ),
            l.pick("Reset / unlock all", "重置 / 全部解锁"),
        ),
    };
    write::Confirm {
        tier: write::Tier::Dialog,
        title,
        what: what.into(),
        downtime: Some(downtime(l)),
        recovery: (w != Which::Reset).then(|| recovery(l)),
        action: action.into(),
        cuts_uplink: true,
        word: None,
        danger: false,
    }
}

/// The list of a confirm or send, checked against the choices; Err = why it is not sent.
pub fn check(w: Which, raw: &str, b: Option<&Bands>, stuck: bool, l: Lang) -> Result<Vec<u32>, write::Outcome> {
    if stuck {
        return Err(write::Outcome::NotYet(stuck_reason(l).into()));
    }
    let Some(b) = b else {
        return Err(write::Outcome::NotYet(
            l.pick(
                "Bands not read yet; try again shortly",
                "还没读到频段，稍后再试",
            )
            .into(),
        ));
    };
    if w == Which::Reset {
        return Ok(Vec::new());
    }
    let list = canon(raw).ok_or_else(|| {
        write::Outcome::Failed(l.pick("not a band list", "不是频段列表").into())
    })?;
    if list.is_empty() {
        return Err(write::Outcome::NotYet(
            match w {
                Which::Nr => l.pick("Select at least one NR band", "请至少选择一个 NR 频段"),
                _ => l.pick("Select at least one LTE band", "请至少选择一个 LTE 频段"),
            }
            .into(),
        ));
    }
    let choices = if w == Which::Nr {
        b.nr_choices()
    } else {
        b.lte_sup.clone()
    };
    if let Some(x) = list.iter().find(|x| !choices.contains(x)) {
        return Err(write::Outcome::Failed(match l {
            Lang::En => format!("band {x} is not one this device supports"),
            Lang::Zh => format!("频段 {x} 不在这台设备支持的列表里"),
        }));
    }
    Ok(list)
}

/// The `/control` params.
pub fn params(w: Which, list: &[u32]) -> Value {
    let s = join(list);
    match w {
        Which::Lte => json!({ "bands": s }),
        Which::Nr => json!({ "sa": s, "nsa": s }),
        Which::Reset => json!({}),
    }
}

/// The signal block shows the change (when u60d's `op` block says nothing about it).
pub fn read_back(w: Which, list: &[u32], b: &Bands) -> bool {
    match w {
        Which::Lte => b.lte == list,
        Which::Nr => b.sa == list && b.nsa == list,
        Which::Reset => b.unlocked(),
    }
}

fn applied(w: Which, list: &[u32], l: Lang) -> String {
    match (w, l) {
        (Which::Lte, Lang::En) => format!("LTE locked to {}", words("B", list)),
        (Which::Lte, Lang::Zh) => format!("LTE 已锁定到 {}", words("B", list)),
        (Which::Nr, Lang::En) => format!("NR locked to {}", words("n", list)),
        (Which::Nr, Lang::Zh) => format!("NR 已锁定到 {}", words("n", list)),
        (Which::Reset, _) => l.pick("All bands unlocked", "已解锁所有频段").into(),
    }
}

/// How the follow ended, in the page's words (u60d's own for a finished transaction).
pub fn outcome(s: &Seen, sent: write::Sent, read: bool, w: Which, list: &[u32], l: Lang) -> write::Outcome {
    match s {
        Seen::Final(_) => crate::changes::outcome(s, sent, l),
        Seen::Live(v) => {
            let say = crate::changes::pick(v, "say", l);
            write::Outcome::AcceptedSaid(match l {
                Lang::En => format!("Accepted by the device · {say}; not confirmed yet"),
                Lang::Zh => format!("设备已接受 · {say}，还没确认"),
            })
        }
        Seen::Nothing if read => write::Outcome::Applied(applied(w, list, l)),
        Seen::Nothing => sent.not_read(),
    }
}

fn area(w: Which, l: Lang, remote: bool, list: &[u32], phase: &write::Phase) -> String {
    let p = w.path();
    let set = match w {
        Which::Reset => format!("/bandlock/{p}/set"),
        _ => format!("/bandlock/{p}/set?bands={}", join(list)),
    };
    let cancel = format!("/bandlock/{p}/cancel");
    write::Area {
        id: w.id(),
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

#[derive(Deserialize, Default)]
pub struct BandsQuery {
    #[serde(default)]
    bands: String,
}

async fn ask(app: Shared, w: Which, q: BandsQuery, req: axum::extract::Request) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let (b, stuck) = app.hub.read(|st| (bands(st), st.stuck));
    let phase = match check(w, &q.bands, b.as_ref(), stuck, l) {
        Err(o) => write::Phase::Done(o),
        Ok(list) => {
            let html = area(w, l, ctx.remote, &list, &write::Phase::Ask(confirm(w, &list, l)));
            return one(html);
        }
    };
    one(area(w, l, ctx.remote, &[], &phase))
}

async fn set(app: Shared, w: Which, q: BandsQuery, req: axum::extract::Request) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    Sse::new(async_stream(move |tx| async move {
        let (b, stuck) = app.hub.read(|st| (bands(st), st.stuck));
        let list = match check(w, &q.bands, b.as_ref(), stuck, l) {
            Ok(list) => list,
            Err(o) => {
                let _ = tx.send(patch(&area(w, l, false, &[], &write::Phase::Done(o)))).await;
                return;
            }
        };
        let pending = |t: String| area(w, l, false, &list, &write::Phase::Pending(t));
        let first = match w {
            Which::Reset => l.pick("Resetting…", "正在恢复默认…"),
            _ => l.pick("Locking…", "正在锁定…"),
        };
        if tx.send(patch(&pending(first.into()))).await.is_err() {
            // the tab went away before anything was sent: don't write
            return;
        }
        let op_id = write::next_op_id();
        let body = json!({
            "action": w.action(),
            "params": params(w, &list),
            "source": "web",
            "op_id": op_id,
        });
        let mut changes = app.hub.watch();
        let sent = match write::send_body(&app.http, &app.control_url, &body, Some(SEND_TIMEOUT), l).await {
            Ok(s) => s,
            Err(o) => {
                let _ = tx.send(patch(&area(w, l, false, &list, &write::Phase::Done(o)))).await;
                return;
            }
        };
        // follow the transaction in the op block, saying what u60d says while it runs
        let t0 = tokio::time::Instant::now();
        let deadline = t0 + FOLLOW;
        let mut last = Seen::Nothing;
        let mut said = String::new();
        loop {
            match app.hub.read(|st| crate::changes::seen(st, &op_id)) {
                f @ Seen::Final(_) => {
                    last = f;
                    break;
                }
                Seen::Live(v) => {
                    let now = progress(&v, l);
                    if now != said {
                        said = now.clone();
                        if tx.send(patch(&pending(now))).await.is_err() {
                            return;
                        }
                    }
                    last = Seen::Live(v);
                }
                Seen::Nothing => {
                    // no transaction seen (an older u60d): the signal block is the readback
                    // (after a few seconds: the op block normally comes first)
                    if matches!(last, Seen::Nothing)
                        && t0.elapsed() > Duration::from_secs(5)
                        && app.hub.read(|st| bands(st).is_some_and(|b| read_back(w, &list, &b)))
                    {
                        break;
                    }
                }
            }
            let wake = (tokio::time::Instant::now() + Duration::from_secs(1)).min(deadline);
            tokio::select! {
                r = tokio::time::timeout_at(wake, changes.recv()) => match r {
                    Ok(Ok(_)) | Ok(Err(RecvError::Lagged(_))) | Err(_) => {}
                    Ok(Err(RecvError::Closed)) => break,
                },
                _ = tx.closed() => return,
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
        }
        let read = app
            .hub
            .read(|st| bands(st).is_some_and(|b| read_back(w, &list, &b)));
        let o = outcome(&last, sent, read, w, &list, l);
        let html = format!(
            "{}\n{}",
            area(w, l, false, &list, &write::Phase::Done(o)),
            draft(&app, l)
        );
        let _ = tx.send(patch(&html)).await;
    }))
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
        matches!(block, "signal" | "op" | "exec")
    }
    fn render(&mut self, app: &Shared) -> Option<Vec<(String, String)>> {
        Some(leaves(app, self.l))
    }
}

#[derive(Template)]
#[template(path = "bandlock.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    leaves: Vec<(String, String)>,
    draft: String,
    lte_op: String,
    nr_op: String,
    reset_op: String,
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
    let idle = write::Phase::Idle;
    let html = Page {
        shell,
        l,
        leaves: leaves(&app, l),
        draft: draft(&app, l),
        lte_op: area(Which::Lte, l, ctx.remote, &[], &idle),
        nr_op: area(Which::Nr, l, ctx.remote, &[], &idle),
        reset_op: area(Which::Reset, l, ctx.remote, &[], &idle),
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
    let p = Poll { l: ctx.lang };
    crate::agent::stream(app, ctx, q.g, PATH, p)
}

pub async fn lte_ask(State(app): State<Shared>, Query(q): Query<BandsQuery>, req: axum::extract::Request) -> impl IntoResponse {
    ask(app, Which::Lte, q, req).await
}
pub async fn nr_ask(State(app): State<Shared>, Query(q): Query<BandsQuery>, req: axum::extract::Request) -> impl IntoResponse {
    ask(app, Which::Nr, q, req).await
}
pub async fn reset_ask(State(app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    ask(app, Which::Reset, BandsQuery::default(), req).await
}
pub async fn lte_set(State(app): State<Shared>, Query(q): Query<BandsQuery>, req: axum::extract::Request) -> impl IntoResponse {
    set(app, Which::Lte, q, req).await
}
pub async fn nr_set(State(app): State<Shared>, Query(q): Query<BandsQuery>, req: axum::extract::Request) -> impl IntoResponse {
    set(app, Which::Nr, q, req).await
}
pub async fn reset_set(State(app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    set(app, Which::Reset, BandsQuery::default(), req).await
}

fn cancel(w: Which, q: &write::CancelQuery, req: &axum::extract::Request) -> String {
    let ctx = crate::topbar::Ctx::of_request(req, PATH);
    area(w, ctx.lang, ctx.remote, &[], &write::cancel_phase(q))
}
pub async fn lte_cancel(Query(q): Query<write::CancelQuery>, req: axum::extract::Request) -> impl IntoResponse {
    one(cancel(Which::Lte, &q, &req))
}
pub async fn nr_cancel(Query(q): Query<write::CancelQuery>, req: axum::extract::Request) -> impl IntoResponse {
    one(cancel(Which::Nr, &q, &req))
}
pub async fn reset_cancel(Query(q): Query<write::CancelQuery>, req: axum::extract::Request) -> impl IntoResponse {
    one(cancel(Which::Reset, &q, &req))
}

#[cfg(test)]
mod tests;
