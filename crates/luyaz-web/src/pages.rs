//! The signal page (8b trial): full page on GET, then one Datastar SSE stream per tab that sends
//! only the fragments whose HTML changed. The roaming switch is a tier-2 write through write.rs
//! (page-inline confirm, u60d `/control` with a fresh op_id, readback through the `interfaces`
//! block, DESIGN.md §5).
//!
//! ```text
//!  GET /signal ─▶ page (fragments rendered at hub generation g)
//!  GET /signal/events?g ─▶ Sub ─▶ catch up if hub moved past g ─▶ on Change: re-render, send
//!        only the values whose HTML differs (each has its own id); whole fragments only when
//!        their structure changed (stale, chart shown, rows added). Help icons are never resent.
//!  POST /roam/ask|cancel ─▶ the #roam fragment opened/closed (no server state per tab)
//!  POST /roam/set?on ─▶ "正在…" ─▶ write::execute(cellular.set) ─▶ ≤15 s interfaces readback
//! ```
//! The streams send only the switch (#roam-sw), and only when `roam_enable` itself changed, so an
//! open confirm or a write's result line survives every push.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};

use askama::Template;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;

use crate::hub::{Change, Hub, Link};
use crate::i18n::Lang;
use crate::sse::patch_data;
use crate::write;
use crate::view::{Anchor, Readout, Reason, SignalView, Trend, roaming, signal_view, trend_y};

pub struct App {
    pub hub: Arc<Hub>,
    pub control_url: String,
    /// u60d's base URL (`/v2/api/...` reads for the frame, topbar.rs).
    pub u60d_url: String,
    /// Asks the frame's side poller for a round now (after "Got it").
    pub side_kick: Arc<tokio::sync::Notify>,
    pub http: reqwest::Client,
    /// Streaming client (no overall timeout, no redirects) for a proxied panel (chill_proxy.rs).
    pub proxy_http: reqwest::Client,
    pub sessions: crate::auth::Sessions,
    /// The admin password file (`--password-file`); None = every login is refused (auth.rs).
    pub password_file: Option<std::path::PathBuf>,
    /// Pages served here (`--pages`); any other page is a 404 (proxy.rs).
    pub pages: Vec<String>,
    /// A button on an agent-backed page did something: every stream of that page (by path)
    /// polls now instead of at its next tick (agent.rs).
    pub kick: tokio::sync::broadcast::Sender<&'static str>,
    /// Operator logo files (`--logo-dir`, oplogo.rs).
    pub logo_dir: std::path::PathBuf,
}

pub type Shared = Arc<App>;

// ---- templates (askama checks every field at compile time: pass line 6) ----

#[derive(Template)]
#[template(path = "status.html")]
struct StatusFrag<'a> {
    v: &'a SignalView,
}

#[derive(Template)]
#[template(path = "readouts.html")]
struct ReadoutsFrag<'a> {
    v: &'a SignalView,
}

#[derive(Template)]
#[template(path = "trend.html")]
struct TrendFrag<'a> {
    v: &'a SignalView,
}

#[derive(Template)]
#[template(path = "cell.html")]
struct CellFrag<'a> {
    v: &'a SignalView,
}

#[derive(Template)]
#[template(path = "roam.html")]
struct RoamFrag {
    l: Lang,
    on: Option<bool>,
    /// The switch's confirm is open / a write is in flight, towards `target`.
    ask: bool,
    busy: bool,
    target: bool,
    /// The write area under the row (write.rs).
    op: String,
}

/// Only the switch: what a push sends when `roam_enable` changed elsewhere. The confirm and the
/// result line belong to the write's own replies, so a push never wipes them.
#[derive(Template)]
#[template(path = "roam_switch.html")]
struct RoamSwitchFrag {
    l: Lang,
    on: Option<bool>,
}

#[derive(Template)]
#[template(path = "signal.html")]
struct SignalPage {
    shell: crate::shell::Shell,
    status: String,
    readouts: String,
    trend: String,
    cell: String,
    roam: String,
}

/// The status block alone, for pages that show the signal verdict next to other things (home).
pub(crate) fn status_frag(v: &SignalView) -> String {
    StatusFrag { v }.render().unwrap()
}

/// The ids the stream may patch, in page order.
const FRAGS: [&str; 4] = ["sig-status", "sig-readouts", "sig-trend", "sig-cell"];

#[derive(Template)]
#[template(path = "leaves.html")]
struct LeavesFrag<'a> {
    v: &'a SignalView,
}

/// One render of the page's live parts: the big fragments, the small pieces inside them that
/// change on every push (each one element with its own id), and the shape that decides which of
/// the two a push sends.
pub(crate) struct Render {
    pub(crate) generation: u64,
    pub(crate) frags: HashMap<&'static str, String>,
    leaves: Vec<(String, String)>,
    /// Whatever changes the structure of the fragments (stale styling, skeletons, chart shown,
    /// rows present). Same shape: send only the leaves that changed. New shape: the fragments.
    shape: (bool, bool, bool, usize, bool),
    roam: Option<bool>,
}

pub(crate) fn split_leaves(html: &str) -> Vec<(String, String)> {
    html.split("<!--leaf-->")
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter_map(|l| {
            let i = l.find("id=\"")? + 4;
            let j = l[i..].find('"')? + i;
            Some((l[i..j].to_owned(), l.to_owned()))
        })
        .collect()
}

pub(crate) fn render_frags(hub: &Hub, l: Lang) -> Render {
    hub.read(|st| {
        let v = signal_view(st, Instant::now(), l);
        let mut m = HashMap::new();
        m.insert("sig-status", StatusFrag { v: &v }.render().unwrap());
        m.insert("sig-readouts", ReadoutsFrag { v: &v }.render().unwrap());
        m.insert("sig-trend", TrendFrag { v: &v }.render().unwrap());
        m.insert("sig-cell", CellFrag { v: &v }.render().unwrap());
        Render {
            generation: st.generation,
            frags: m,
            leaves: split_leaves(&LeavesFrag { v: &v }.render().unwrap()),
            shape: (
                v.stale,
                v.rsrp.loading(),
                v.trend.is_some(),
                v.cell.len(),
                v.anchor.is_some(),
            ),
            roam: roaming(st),
        }
    })
}

/// What a push sends to a tab that has `sent`: the status block when it changed (small, no
/// icons), then either the changed leaves (same shape) or the changed fragments (new shape).
fn diff(sent: &Render, now: &Render) -> Vec<String> {
    let mut out = Vec::new();
    if sent.frags.get("sig-status") != now.frags.get("sig-status") {
        out.push(now.frags["sig-status"].clone());
    }
    if sent.shape == now.shape {
        let old: HashMap<&str, &str> = sent
            .leaves
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        for (id, html) in &now.leaves {
            if old.get(id.as_str()) != Some(&html.as_str()) {
                out.push(html.clone());
            }
        }
    } else {
        for id in &FRAGS[1..] {
            if sent.frags.get(id) != now.frags.get(id) {
                out.push(now.frags[id].clone());
            }
        }
    }
    out
}

/// The roaming row and its write area (tier 2, write.rs). The switch shows the confirm's target
/// while it is open, and "正在打开…" while a write is in flight.
fn roam_html(l: Lang, on: Option<bool>, target: bool, phase: &write::Phase) -> String {
    let set = format!("/roam/set?on={target}");
    let op = write::Area {
        id: "roam",
        l,
        remote: false,
        set: &set,
        cancel: "/roam/cancel",
        phase,
    }
    .html();
    RoamFrag {
        l,
        on,
        ask: matches!(phase, write::Phase::Ask(_)),
        busy: matches!(phase, write::Phase::Pending(_)),
        target,
        op,
    }
    .render()
    .unwrap()
}

fn roam_confirm(l: Lang, target: bool) -> write::Confirm {
    write::Confirm {
        tier: write::Tier::Inline,
        title: if target {
            l.pick("Turn data roaming on", "打开数据漫游")
        } else {
            l.pick("Turn data roaming off", "关掉数据漫游")
        }
        .into(),
        what: if target {
            l.pick(
                "Mobile data will work abroad too; your carrier may charge roaming fees.",
                "打开后在国外也会用移动数据，运营商可能按漫游收费。",
            )
        } else {
            l.pick(
                "Mobile data won't work abroad; at home nothing changes.",
                "关掉后在国外不能用移动数据，回到本国不受影响。",
            )
        }
        .into(),
        downtime: None,
        recovery: None,
        action: if target {
            l.pick("Confirm: turn data roaming on", "确认：打开数据漫游")
        } else {
            l.pick("Confirm: turn data roaming off", "确认：关掉数据漫游")
        }
        .into(),
        cuts_uplink: false,
        word: None,
        danger: false,
    }
}

pub async fn signal(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, "/signal");
    let Render {
        generation,
        frags: mut f,
        roam: on,
        ..
    } = render_frags(&app.hub, ctx.lang);
    let shell = app.hub.read(|st| {
        crate::shell::Shell::new(&headers, "/signal").live(
            st,
            &ctx,
            Some(format!("/signal/events?g={generation}")),
        )
    });
    let page = SignalPage {
        shell,
        status: f.remove("sig-status").unwrap(),
        readouts: f.remove("sig-readouts").unwrap(),
        trend: f.remove("sig-trend").unwrap(),
        cell: f.remove("sig-cell").unwrap(),
        roam: roam_html(ctx.lang, on, false, &write::Phase::Idle),
    }
    .render()
    .unwrap();
    crate::assets::maybe_gzip(
        &headers,
        "text/html; charset=utf-8",
        page.into_bytes(),
        "no-store",
    )
}

#[derive(Deserialize)]
pub struct StreamQuery {
    #[serde(default)]
    pub g: u64,
}

/// One tab's push stream: the page's live parts and the frame's (topbar.rs).
pub async fn signal_events(
    State(app): State<Shared>,
    Query(q): Query<StreamQuery>,
    req: axum::extract::Request,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let ctx = crate::topbar::Ctx::of_request(&req, "/signal");
    let mut sub = app.hub.subscribe();
    let stream = async_stream(move |tx| async move {
        let mut sent = render_frags(&app.hub, ctx.lang);
        let mut seen = crate::opbar::Seen::default();
        let mut frame = app
            .hub
            .read(|st| crate::topbar::leaves(st, &ctx, &mut seen));
        if sent.generation != q.g {
            // The hub moved between the page render and this stream: send it all once.
            let mut all: Vec<&str> = FRAGS.iter().map(|id| sent.frags[id].as_str()).collect();
            let frame_all = crate::topbar::diff(&[], &frame);
            all.extend(frame_all.iter().map(String::as_str));
            if tx.send(patch(&all.join("\n"))).await.is_err() {
                return;
            }
        }
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        tick.tick().await;
        loop {
            let roam_changed;
            let at = app
                .hub
                .read(|st| crate::opbar::wake(st, &seen, std::time::Instant::now()));
            tokio::select! {
                r = sub.rx.recv() => {
                    match r {
                        Ok(Change::Block(name))
                            if !matches!(name.as_str(), "signal" | "qos" | "interfaces")
                                && !crate::topbar::reads(&name) => continue,
                        Ok(Change::Block(name)) if name != "interfaces" => roam_changed = false,
                        Ok(_) | Err(RecvError::Lagged(_)) => roam_changed = true,
                        Err(RecvError::Closed) => return,
                    }
                }
                // The freshness line ("数字停在 N 分钟前") moves with the clock.
                _ = tick.tick() => roam_changed = false,
                // the op bar's countdown and 3-second results
                _ = crate::topbar::wake(at) => roam_changed = false,
                // a closed tab, also while nothing changes (the upstream is down or idle)
                _ = tx.closed() => return,
            }
            let now = render_frags(&app.hub, ctx.lang);
            let mut out = diff(&sent, &now);
            let frame_now = app
                .hub
                .read(|st| crate::topbar::leaves(st, &ctx, &mut seen));
            out.extend(crate::topbar::diff(&frame, &frame_now));
            frame = frame_now;
            if roam_changed && now.roam != sent.roam {
                out.push(
                    RoamSwitchFrag {
                        l: ctx.lang,
                        on: now.roam,
                    }
                    .render()
                    .unwrap(),
                );
            }
            let roam_sent = if roam_changed { now.roam } else { sent.roam };
            sent = Render {
                roam: roam_sent,
                ..now
            };
            if !send_out(&tx, out).await {
                return; // tab gone; dropping `sub` tells the hub
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

pub(crate) fn patch(html: &str) -> Event {
    Event::default()
        .event("datastar-patch-elements")
        .data(patch_data(html))
}

/// One push in one event. The trend line is its own small `<svg>` (not a bare `<polyline>`), so
/// it parses as SVG in an HTML patch: Datastar 1.0.4's svg-namespace morph turned a re-patched
/// `<polyline>` into an HTML element that draws nothing.
pub(crate) async fn send_out(tx: &tokio::sync::mpsc::Sender<Event>, out: Vec<String>) -> bool {
    out.is_empty() || tx.send(patch(&out.join("\n"))).await.is_ok()
}

/// A stream fed by a task through a small channel; the task stops when the client goes away.
pub(crate) fn async_stream<F, Fut>(f: F) -> impl Stream<Item = Result<Event, Infallible>>
where
    F: FnOnce(tokio::sync::mpsc::Sender<Event>) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    tokio::spawn(f(tx));
    futures_util::StreamExt::map(tokio_stream::wrappers::ReceiverStream::new(rx), Ok)
}

pub async fn retry(State(app): State<Shared>) -> StatusCode {
    app.hub.retry();
    StatusCode::NO_CONTENT
}

fn one_patch(html: String) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(futures_util::stream::once(async move { Ok(patch(&html)) }))
}

pub async fn roam_ask(State(app): State<Shared>, headers: HeaderMap) -> impl IntoResponse {
    let l = Lang::from_headers(&headers);
    let on = app.hub.read(roaming);
    let html = match on {
        Some(on) => roam_html(l, Some(on), !on, &write::Phase::Ask(roam_confirm(l, !on))),
        None => roam_html(
            l,
            on,
            false,
            &write::Phase::Done(write::Outcome::NotYet(
                l.pick(
                    "Roaming setting not read yet; try again shortly",
                    "还没读到漫游设置，稍后再试",
                )
                .into(),
            )),
        ),
    };
    one_patch(html)
}

/// Cancel / Esc, and `lost=1` when the send's reply dropped (write.rs).
pub async fn roam_cancel(
    State(app): State<Shared>,
    Query(q): Query<write::CancelQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let l = Lang::from_headers(&headers);
    one_patch(roam_html(l, app.hub.read(roaming), false, &write::cancel_phase(&q)))
}

#[derive(Deserialize)]
pub struct SetQuery {
    on: bool,
}

/// Tier 2: `cellular.set` through write.rs, read back from the `interfaces` block.
pub async fn roam_set(
    State(app): State<Shared>,
    Query(q): Query<SetQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let l = Lang::from_headers(&headers);
    let target = q.on;
    let pending = roam_html(
        l,
        app.hub.read(roaming),
        target,
        &write::Phase::Pending(String::new()),
    );
    let w = write::Write {
        action: "cellular.set",
        params: json!({ "roaming": target }),
        readback: Some(Box::new(move |st| roaming(st) == Some(target))),
        applied: if target {
            l.pick("Data roaming on", "已打开数据漫游")
        } else {
            l.pick("Data roaming off", "已关掉数据漫游")
        }
        .into(),
        timeout: write::READBACK,
        typed: None,
        request_timeout: None,
    };
    write::respond(pending, async move {
        let o = write::execute(&app.http, &app.control_url, &app.hub, &w, l).await;
        roam_html(l, app.hub.read(roaming), target, &write::Phase::Done(o))
    })
}

/// For tests and the trial report: upstream state in one line.
pub async fn health(State(app): State<Shared>) -> impl IntoResponse {
    let (link, connects) = app.hub.read(|st| (st.link, st.connects));
    let link = match link {
        Link::Idle => "idle",
        Link::Connecting => "connecting",
        Link::Live => "live",
        Link::Down => "down",
    };
    (
        [(header::CACHE_CONTROL, "no-store")],
        axum::Json(json!({"link": link, "connects": connects, "tabs": app.hub.subscribers()})),
    )
}

// ---- template helpers ----

impl Readout {
    pub fn loading(&self) -> bool {
        self.value.is_none()
    }
    pub fn text(&self) -> &str {
        self.value.as_deref().unwrap_or("")
    }
}

impl Trend {
    pub fn aria(&self) -> String {
        match self.l {
            Lang::En => format!(
                "RSRP over the last 2 minutes, from {} to {} dBm",
                self.lo, self.hi
            ),
            Lang::Zh => format!("过去 2 分钟的 RSRP，{} 到 {} dBm", self.lo, self.hi),
        }
    }
}

/// Band rectangles and gridline labels of TrendChart.tsx: (y, height, fill).
pub fn trend_bands() -> [(String, String, &'static str); 3] {
    let r = |a: f64, b: f64, fill| {
        (
            format!("{:.1}", trend_y(a)),
            format!("{:.1}", trend_y(b) - trend_y(a)),
            fill,
        )
    };
    [
        r(-70.0, -100.0, "var(--nd-wash)"),
        r(-100.0, -110.0, "var(--nd-washW)"),
        r(-110.0, -130.0, "var(--nd-washB)"),
    ]
}

pub fn trend_labels() -> [(i64, String); 4] {
    [-80, -100, -110, -120].map(|v| (v, format!("{:.1}", trend_y(v as f64) + 4.0)))
}

impl SignalView {
    pub fn bands(&self) -> [(String, String, &'static str); 3] {
        trend_bands()
    }
    pub fn labels(&self) -> [(i64, String); 4] {
        trend_labels()
    }
    pub fn anchor_ref(&self) -> Option<&Anchor> {
        self.anchor.as_ref()
    }
    pub fn reason_ref(&self) -> &Reason {
        &self.reason
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sse::Frame;
    use serde_json::Value;

    fn feed(hub: &Hub, event: &str, v: Value) {
        hub.apply(&Frame {
            event: event.into(),
            data: v.to_string(),
        });
    }

    fn sig(rev: u64, rsrp: i64, stale: bool) -> Value {
        json!({"epoch":"e","name":"signal","revision":rev,"observed_at":rev,"stale":stale,
               "data":{"type":"SA","bars":4,"nr_rsrp":rsrp,"nr_snr":10,"nr_rsrq":-11,"nr_pci":5,"nr_channel":7}})
    }

    #[test]
    fn a_value_change_sends_only_that_value_and_never_the_help_icons() {
        let hub = Hub::new();
        feed(
            &hub,
            "snapshot",
            json!({"epoch":"e","blocks":{"signal":sig(1, -97, false)}}),
        );
        let a = render_frags(&hub, Lang::Zh);
        feed(&hub, "block", sig(2, -99, false));
        let b = render_frags(&hub, Lang::Zh);
        let out = diff(&a, &b).join("\n");
        assert!(
            out.contains("id=\"ro-rsrp\"") && out.contains("-99"),
            "{out}"
        );
        assert!(
            !out.contains("sig-readouts")
                && !out.contains("nd-help")
                && !out.contains("id=\"ro-sinr\""),
            "{out}"
        );
        assert!(out.len() < 400, "{} bytes", out.len());
    }

    #[test]
    fn a_shape_change_sends_the_fragments() {
        let hub = Hub::new();
        feed(
            &hub,
            "snapshot",
            json!({"epoch":"e","blocks":{"signal":sig(1, -97, false)}}),
        );
        let a = render_frags(&hub, Lang::Zh);
        feed(&hub, "block", sig(2, -97, true));
        let out = diff(&a, &render_frags(&hub, Lang::Zh)).join("\n");
        assert!(
            out.contains("id=\"sig-readouts\"") && out.contains("nd-stale"),
            "{out}"
        );
        assert!(
            out.contains("id=\"sig-status\""),
            "the status turns stale too"
        );
    }

    #[test]
    fn leaves_are_split_by_id() {
        let l = split_leaves(
            "<div id=\"a\">1</div>\n<!--leaf-->\n<span id=\"b\" class=\"x\">2</span>\n",
        );
        assert_eq!(
            l,
            vec![
                ("a".into(), "<div id=\"a\">1</div>".into()),
                ("b".into(), "<span id=\"b\" class=\"x\">2</span>".into())
            ]
        );
    }

    fn cjk(s: &str) -> Vec<char> {
        s.chars()
            .filter(|c| {
                ('\u{4e00}'..='\u{9fff}').contains(c)
                    || ('\u{3000}'..='\u{303f}').contains(c)
                    || ('\u{ff00}'..='\u{ffef}').contains(c)
            })
            .collect()
    }

    #[test]
    fn the_english_page_body_has_no_chinese_left() {
        let hub = Hub::new();
        let mut nsa = sig(1, -97, false);
        nsa["data"]["type"] = json!("NSA");
        nsa["data"]["lte_rsrp"] = json!(-101);
        nsa["data"]["band"] = json!("B3");
        nsa["data"]["net_select_mode"] = json!("auto_select");
        feed(
            &hub,
            "snapshot",
            json!({"epoch":"e","blocks":{"signal":nsa,
                "qos":{"revision":1,"observed_at":1,"stale":false,"data":{"qci":9,"ambr_dl":300,"ambr_ul":50}}}}),
        );
        for i in 0..12 {
            feed(&hub, "block", {
                let mut b = sig(2 + i, -97 - i as i64, false);
                b["data"] = nsa["data"].clone();
                b["observed_at"] = json!(10 * (i + 1));
                b
            });
        }
        let r = render_frags(&hub, Lang::En);
        let mut all: Vec<String> = r.frags.values().cloned().collect();
        all.extend(r.leaves.iter().map(|(_, h)| h.clone()));
        all.push(roam_html(
            Lang::En,
            Some(true),
            false,
            &write::Phase::Ask(roam_confirm(Lang::En, false)),
        ));
        all.push(roam_html(
            Lang::En,
            Some(false),
            true,
            &write::Phase::Pending(String::new()),
        ));
        for o in [
            write::Outcome::Accepted,
            write::Outcome::Unknown,
            write::Outcome::Busy,
            write::Outcome::Failed("x".into()),
        ] {
            all.push(roam_html(Lang::En, Some(false), true, &write::Phase::Done(o)));
        }
        let html = all.join("\n");
        assert!(
            html.contains("Signal trend") && html.contains("4G anchor"),
            "trend and anchor rendered"
        );
        assert_eq!(cjk(&html), Vec::<char>::new(), "{html}");
        // and stale: the reason line
        feed(&hub, "block", sig(40, -97, true));
        let r = render_frags(&hub, Lang::En);
        assert!(
            r.frags["sig-status"].contains("Numbers stopped"),
            "{}",
            r.frags["sig-status"]
        );
        assert!(cjk(&r.frags["sig-status"]).is_empty());
    }
}
