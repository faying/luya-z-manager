//! /router/celllock (小区锁定), the old `router/celllock/page.tsx`: the cell the modem is on, the
//! cell locks the firmware keeps (new: the old page could not read them), NR / LTE lock forms and
//! "unlock all". Batch 3, step 3-2 (u60-web.md).
//!
//! Where each value comes from (all u60d's `signal` block; the agent is not asked):
//! - current cell = the serving carrier (`view::carriers`, same as /signal and /bandlock): network,
//!   PCI, ARFCN, band; cell ID = `nr_cell_id` / `lte_cell_id`;
//! - locks = `nr_cell_lock` ("pci,arfcn,band") and `lte_cell_lock` ("pci,earfcn", order inferred by
//!   u60d, not checked on the device); "" or all zeros = not locked.
//!
//! Writes, all tier 3, only through u60d `/control` transactions: `cell.lock_nr {pci, arfcn, band}`
//! (u60d wants the band: the old form's "optional" is gone, it starts at the serving NR band),
//! `cell.lock_lte {pci, earfcn}`, and "unlock all" = `band.reset {}` (the firmware has no cell
//! unlock: bands and cell locks are both reset, D22). Each send has a new `op_id` and is followed in
//! the `op` block (follow.rs). Neighbour scan stays off (the agent answers 410): the button is shown
//! disabled with the reason, nothing is requested.
//!
//! The form fields are page-local signals (`_npci` …), outside the pushed leaves, so pushes never
//! touch what is being typed; the values go in the ask/confirm URL and are checked again.

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
use crate::write;

pub const PATH: &str = "/router/celllock";
const FOLLOW: Duration = Duration::from_secs(130);
const SEND_TIMEOUT: Duration = Duration::from_secs(30);

// ---- locks ----

/// A cell lock as u60d normalizes it: decimal strings, no leading zeros.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Lock {
    pub pci: String,
    /// ARFCN (NR) / EARFCN (LTE).
    pub arfcn: String,
    /// NR only.
    pub band: String,
}

/// One number as u60d's `cell_num` takes it: digits only (an NR band may start with n).
pub fn num(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<u64>().ok().map(|n| n.to_string())
}

/// The raw signal field: None = not locked ("" or all zeros) or unreadable.
pub fn parse(raw: &str, nr: bool) -> Option<Lock> {
    let parts: Vec<&str> = raw.split(',').map(str::trim).collect();
    let want = if nr { 3 } else { 2 };
    if parts.len() != want {
        return None;
    }
    let v: Vec<String> = parts.iter().map(|p| num(p.trim_start_matches(['n', 'N']))).collect::<Option<_>>()?;
    if v.iter().all(|x| x == "0") {
        return None;
    }
    Some(Lock {
        pci: v[0].clone(),
        arfcn: v[1].clone(),
        band: v.get(2).cloned().unwrap_or_default(),
    })
}

impl Lock {
    /// "PCI 603 · ARFCN 627264 · n78" / "PCI 1 · EARFCN 1300".
    pub fn text(&self, nr: bool) -> String {
        if nr {
            format!("PCI {} · ARFCN {} · n{}", self.pci, self.arfcn, self.band)
        } else {
            format!("PCI {} · EARFCN {}", self.pci, self.arfcn)
        }
    }
}

fn signal(st: &St) -> Option<(&Value, bool)> {
    st.blocks
        .get("signal")
        .map(|b| (&b.data, b.stale || st.link != Link::Live))
}

pub fn locks(st: &St) -> Option<(Option<Lock>, Option<Lock>)> {
    signal(st).map(|(d, _)| {
        (
            parse(d["nr_cell_lock"].as_str().unwrap_or(""), true),
            parse(d["lte_cell_lock"].as_str().unwrap_or(""), false),
        )
    })
}

fn stuck_reason(l: Lang) -> &'static str {
    l.pick(
        "Data service not responding · settings can't be changed for now",
        "数据服务没响应 · 暂时不能改设置",
    )
}

// ---- the view ----

pub struct View {
    pub l: Lang,
    pub stale: bool,
    pub rat: String,
    pub pci: String,
    pub arfcn: String,
    pub band: String,
    pub cell_id: String,
    pub nr_lock: String,
    pub lte_lock: String,
    /// "the modem is still on PCI x (locked y)" / "now on PCI y".
    pub on_cell: Option<(String, bool)>,
    pub dis: bool,
    pub why: Option<String>,
    pub txn: Option<String>,
}

pub fn view(st: &St, l: Lang) -> View {
    let sig = signal(st);
    let serving = sig.and_then(|(d, _)| crate::view::carriers(d).into_iter().next());
    let dash = || "—".to_string();
    let (nr_l, lte_l) = locks(st).unwrap_or((None, None));
    let not_locked = || l.pick("Not locked", "没锁").to_string();
    let lock_text = |x: &Option<Lock>, nr: bool| match (sig, x) {
        (None, _) => dash(),
        (_, Some(k)) => k.text(nr),
        (_, None) => not_locked(),
    };
    let on_cell = serving.as_ref().and_then(|c| {
        let k = if c.nr { nr_l.as_ref() } else { lte_l.as_ref() }?;
        let cur = c.pci?.to_string();
        Some(if cur == k.pci {
            (
                match l {
                    Lang::En => format!("The modem is on the locked cell (PCI {cur})"),
                    Lang::Zh => format!("调制解调器已在锁定的小区上（PCI {cur}）"),
                },
                true,
            )
        } else {
            (
                match l {
                    Lang::En => format!("The modem is still on PCI {cur} (locked {})", k.pci),
                    Lang::Zh => format!("调制解调器还在 PCI {cur} 上（锁定的是 {}）", k.pci),
                },
                false,
            )
        })
    });
    let txn = crate::follow::running(st, &["cell.", "band."], l);
    View {
        l,
        stale: sig.is_some_and(|(_, s)| s),
        rat: match (sig, &serving) {
            (None, _) => dash(),
            (_, None) => l.pick("No serving cell", "没有服务小区").into(),
            (_, Some(c)) => if c.nr { "NR" } else { "LTE" }.into(),
        },
        pci: serving.as_ref().and_then(|c| c.pci).map_or_else(dash, |v| v.to_string()),
        arfcn: serving.as_ref().and_then(|c| c.arfcn).map_or_else(dash, |v| v.to_string()),
        band: serving
            .as_ref()
            .filter(|c| !c.band.is_empty())
            .map_or_else(dash, |c| format!("{}{}", if c.nr { "n" } else { "B" }, c.band)),
        cell_id: match (sig, &serving) {
            (Some((d, _)), Some(c)) => {
                let v = &d[if c.nr { "nr_cell_id" } else { "lte_cell_id" }];
                v.as_i64().filter(|n| *n > 0).map_or_else(dash, |n| n.to_string())
            }
            _ => dash(),
        },
        nr_lock: lock_text(&nr_l, true),
        lte_lock: lock_text(&lte_l, false),
        on_cell,
        dis: st.stuck || txn.is_some() || sig.is_none(),
        why: st.stuck.then(|| stuck_reason(l).to_string()),
        txn,
    }
}

/// The NR band field's first value: the NR lock's band, else the serving NR carrier's.
fn band0(st: &St) -> String {
    if let Some((Some(k), _)) = locks(st) {
        return k.band;
    }
    signal(st)
        .and_then(|(d, _)| crate::view::carriers(d).into_iter().find(|c| c.nr))
        .map(|c| c.band.trim_start_matches(['n', 'N']).to_string())
        .unwrap_or_default()
}

#[derive(Template)]
#[template(path = "celllock_leaves.html")]
struct Leaves<'a> {
    v: &'a View,
}

pub fn leaves(app: &Shared, l: Lang) -> Vec<(String, String)> {
    app.hub
        .read(|st| split_leaves(&Leaves { v: &view(st, l) }.render().unwrap()))
}

// ---- writes ----

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Which {
    Nr,
    Lte,
    Reset,
}

impl Which {
    fn id(self) -> &'static str {
        match self {
            Which::Nr => "cl-nr",
            Which::Lte => "cl-lte",
            Which::Reset => "cl-reset",
        }
    }
    fn path(self) -> &'static str {
        match self {
            Which::Nr => "nr",
            Which::Lte => "lte",
            Which::Reset => "reset",
        }
    }
    pub fn action(self) -> &'static str {
        match self {
            Which::Nr => "cell.lock_nr",
            Which::Lte => "cell.lock_lte",
            Which::Reset => "band.reset",
        }
    }
}

#[derive(Deserialize, Default)]
pub struct CellQuery {
    #[serde(default)]
    pci: String,
    #[serde(default)]
    arfcn: String,
    #[serde(default)]
    band: String,
}

/// The lock of a confirm or send, checked; Err = why it is not sent.
pub fn check(w: Which, q: &CellQuery, loaded: bool, stuck: bool, l: Lang) -> Result<Lock, write::Outcome> {
    if stuck {
        return Err(write::Outcome::NotYet(stuck_reason(l).into()));
    }
    if !loaded {
        return Err(write::Outcome::NotYet(
            l.pick("Cell not read yet; try again shortly", "还没读到小区，稍后再试").into(),
        ));
    }
    if w == Which::Reset {
        return Ok(Lock::default());
    }
    let need = |s: &str, what: &str| {
        num(s).ok_or_else(|| {
            write::Outcome::NotYet(match l {
                Lang::En => format!("{what} must be a number"),
                Lang::Zh => format!("{what} 要填数字"),
            })
        })
    };
    let pci = need(&q.pci, "PCI")?;
    if pci.parse::<u32>().map_or(true, |p| p > 1007) {
        return Err(write::Outcome::NotYet(
            l.pick("PCI is 0–1007", "PCI 是 0～1007").into(),
        ));
    }
    let arfcn = need(&q.arfcn, if w == Which::Nr { "ARFCN" } else { "EARFCN" })?;
    let band = if w == Which::Nr {
        need(q.band.trim().trim_start_matches(['n', 'N']), l.pick("Band", "频段"))?
    } else {
        String::new()
    };
    Ok(Lock { pci, arfcn, band })
}

pub fn params(w: Which, k: &Lock) -> Value {
    match w {
        Which::Nr => json!({"pci": k.pci, "arfcn": k.arfcn, "band": k.band}),
        Which::Lte => json!({"pci": k.pci, "earfcn": k.arfcn}),
        Which::Reset => json!({}),
    }
}

pub fn read_back(w: Which, k: &Lock, st: &St) -> bool {
    match (w, locks(st)) {
        (_, None) => false,
        (Which::Nr, Some((n, _))) => n.as_ref() == Some(k),
        (Which::Lte, Some((_, t))) => t.as_ref() == Some(k),
        (Which::Reset, Some((n, t))) => n.is_none() && t.is_none(),
    }
}

fn applied(w: Which, k: &Lock, l: Lang) -> String {
    match (w, l) {
        (Which::Nr, Lang::En) => format!("NR locked to {}", k.text(true)),
        (Which::Nr, Lang::Zh) => format!("NR 已锁定到 {}", k.text(true)),
        (Which::Lte, Lang::En) => format!("LTE locked to {}", k.text(false)),
        (Which::Lte, Lang::Zh) => format!("LTE 已锁定到 {}", k.text(false)),
        (Which::Reset, _) => l.pick("Bands and cell locks reset", "频段和小区锁已恢复").into(),
    }
}

pub fn confirm(w: Which, k: &Lock, l: Lang) -> write::Confirm {
    let downtime = l.pick(
        "The mobile connection drops for about 30 seconds while the modem re-attaches. If the cell isn't reachable, it stays off until the lock is removed.",
        "调制解调器重新附着，移动网络断开约 30 秒；如果连不上这个小区，在解锁之前会一直断开。",
    );
    let recovery = l.pick(
        "Press “Unlock all” on this page. If the page can't be reached, open cell lock on the device's touchscreen and restore the default, or reboot the device.",
        "在这个页面按「全部解锁」。如果页面打不开，在设备触屏的小区锁定里恢复默认，或重启设备。",
    );
    let (title, what, action) = match (w, l) {
        (Which::Nr, Lang::En) => (
            format!("Lock NR to PCI {}?", k.pci),
            format!("5G only uses the cell PCI {} on ARFCN {} (n{}). Other NR cells stop being used.", k.pci, k.arfcn, k.band),
            "Lock NR",
        ),
        (Which::Nr, Lang::Zh) => (
            format!("把 NR 锁定到 PCI {}？", k.pci),
            format!("5G 只用 ARFCN {}（n{}）上的 PCI {} 小区，不再用其他 NR 小区。", k.arfcn, k.band, k.pci),
            "锁定 NR",
        ),
        (Which::Lte, Lang::En) => (
            format!("Lock LTE to PCI {}?", k.pci),
            format!("4G only uses the cell PCI {} on EARFCN {}. Other LTE cells stop being used.", k.pci, k.arfcn),
            "Lock LTE",
        ),
        (Which::Lte, Lang::Zh) => (
            format!("把 LTE 锁定到 PCI {}？", k.pci),
            format!("4G 只用 EARFCN {} 上的 PCI {} 小区，不再用其他 LTE 小区。", k.arfcn, k.pci),
            "锁定 LTE",
        ),
        (Which::Reset, _) => (
            l.pick("Reset all cell locks?", "重置所有小区锁定？").into(),
            l.pick(
                "Bands and cell lock are both reset: every cell lock and every band lock is removed.",
                "频段和小区锁一起恢复：所有小区锁定和频段锁定都会去掉。",
            )
            .into(),
            l.pick("Unlock all", "全部解锁"),
        ),
    };
    write::Confirm {
        tier: write::Tier::Dialog,
        title,
        what,
        downtime: Some(if w == Which::Reset {
            l.pick(
                "The mobile connection drops for about 30 seconds while the modem re-attaches.",
                "调制解调器重新附着，移动网络断开约 30 秒。",
            )
            .into()
        } else {
            downtime.into()
        }),
        recovery: (w != Which::Reset).then(|| recovery.into()),
        action: action.into(),
        cuts_uplink: true,
        word: None,
        danger: false,
    }
}

fn area(w: Which, l: Lang, remote: bool, k: &Lock, phase: &write::Phase) -> String {
    let p = w.path();
    let set = match w {
        Which::Reset => format!("{PATH}/{p}/set"),
        Which::Nr => format!("{PATH}/{p}/set?pci={}&arfcn={}&band={}", k.pci, k.arfcn, k.band),
        Which::Lte => format!("{PATH}/{p}/set?pci={}&arfcn={}", k.pci, k.arfcn),
    };
    let cancel = format!("{PATH}/{p}/cancel");
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

fn state(app: &Shared) -> (bool, bool) {
    app.hub.read(|st| (signal(st).is_some(), st.stuck))
}

async fn ask(app: Shared, w: Which, q: CellQuery, req: axum::extract::Request) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let (loaded, stuck) = state(&app);
    let phase = match check(w, &q, loaded, stuck, l) {
        Err(o) => write::Phase::Done(o),
        Ok(k) => return one(area(w, l, ctx.remote, &k, &write::Phase::Ask(confirm(w, &k, l)))),
    };
    one(area(w, l, ctx.remote, &Lock::default(), &phase))
}

async fn set(app: Shared, w: Which, q: CellQuery, req: axum::extract::Request) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    Sse::new(async_stream(move |tx| async move {
        let (loaded, stuck) = state(&app);
        let k = match check(w, &q, loaded, stuck, l) {
            Ok(k) => k,
            Err(o) => {
                let _ = tx.send(patch(&area(w, l, false, &Lock::default(), &write::Phase::Done(o)))).await;
                return;
            }
        };
        let pending = |t: String| area(w, l, false, &k, &write::Phase::Pending(t));
        let first = match w {
            Which::Reset => l.pick("Resetting…", "正在恢复默认…"),
            _ => l.pick("Locking…", "正在锁定…"),
        };
        if tx.send(patch(&pending(first.into()))).await.is_err() {
            return;
        }
        let op_id = write::next_op_id();
        let body = json!({"action": w.action(), "params": params(w, &k), "source": "web", "op_id": op_id});
        let changes = app.hub.watch();
        let sent = match write::send_body(&app.http, &app.control_url, &body, Some(SEND_TIMEOUT), l).await {
            Ok(s) => s,
            Err(o) => {
                let _ = tx.send(patch(&area(w, l, false, &k, &write::Phase::Done(o)))).await;
                return;
            }
        };
        let k2 = k.clone();
        let Some(f) = crate::follow::follow(
            &app,
            &tx,
            &op_id,
            changes,
            FOLLOW,
            Duration::from_secs(5),
            l,
            pending,
            move |st| read_back(w, &k2, st),
        )
        .await
        else {
            return;
        };
        let o = crate::follow::outcome(&f, sent, applied(w, &k, l), l);
        let _ = tx.send(patch(&area(w, l, false, &k, &write::Phase::Done(o)))).await;
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
#[template(path = "celllock.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    leaves: Vec<(String, String)>,
    band0: String,
    nr_op: String,
    lte_op: String,
    reset_op: String,
}

impl Page {
    fn leaf(&self, id: &str) -> &str {
        self.leaves.iter().find(|(k, _)| k == id).map_or("", |(_, h)| h.as_str())
    }
}

pub async fn page(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let (shell, band0) = app.hub.read(|st| {
        (
            crate::shell::Shell::new(&headers, PATH).live(st, &ctx, Some(format!("{PATH}/events?g={}", st.generation))),
            band0(st),
        )
    });
    let idle = write::Phase::Idle;
    let none = Lock::default();
    let html = Page {
        shell,
        l,
        leaves: leaves(&app, l),
        band0,
        nr_op: area(Which::Nr, l, ctx.remote, &none, &idle),
        lte_op: area(Which::Lte, l, ctx.remote, &none, &idle),
        reset_op: area(Which::Reset, l, ctx.remote, &none, &idle),
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

pub async fn nr_ask(State(app): State<Shared>, Query(q): Query<CellQuery>, req: axum::extract::Request) -> impl IntoResponse {
    ask(app, Which::Nr, q, req).await
}
pub async fn lte_ask(State(app): State<Shared>, Query(q): Query<CellQuery>, req: axum::extract::Request) -> impl IntoResponse {
    ask(app, Which::Lte, q, req).await
}
pub async fn reset_ask(State(app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    ask(app, Which::Reset, CellQuery::default(), req).await
}
pub async fn nr_set(State(app): State<Shared>, Query(q): Query<CellQuery>, req: axum::extract::Request) -> impl IntoResponse {
    set(app, Which::Nr, q, req).await
}
pub async fn lte_set(State(app): State<Shared>, Query(q): Query<CellQuery>, req: axum::extract::Request) -> impl IntoResponse {
    set(app, Which::Lte, q, req).await
}
pub async fn reset_set(State(app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    set(app, Which::Reset, CellQuery::default(), req).await
}

fn cancel(w: Which, q: &write::CancelQuery, req: &axum::extract::Request) -> String {
    let ctx = crate::topbar::Ctx::of_request(req, PATH);
    area(w, ctx.lang, ctx.remote, &Lock::default(), &write::cancel_phase(q))
}
pub async fn nr_cancel(Query(q): Query<write::CancelQuery>, req: axum::extract::Request) -> impl IntoResponse {
    one(cancel(Which::Nr, &q, &req))
}
pub async fn lte_cancel(Query(q): Query<write::CancelQuery>, req: axum::extract::Request) -> impl IntoResponse {
    one(cancel(Which::Lte, &q, &req))
}
pub async fn reset_cancel(Query(q): Query<write::CancelQuery>, req: axum::extract::Request) -> impl IntoResponse {
    one(cancel(Which::Reset, &q, &req))
}

#[cfg(test)]
mod tests;
