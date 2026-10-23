//! The change in progress, on every page (write-op-layer.md
//! DD4, DD8, DD15, DD16, DD18): one strip above the alert bar, "正在改网络模式 · 1:42 ›" while it
//! runs, then the result until someone presses "Got it" on the item's page or the touch screen.
//! Also says when u60d is stuck (writes are off) and when we lost u60d while a change ran.
//! With nothing running or to report: the one-time "auto revert is on" notice with "Got it".
//!
//! ```text
//!  u60d `op` block (hub.rs, op_at = when it arrived) + heartbeat exec_age_ms (hub.stuck)
//!    └▶ view(st, seen, now) ─▶ #op-bar (+ its clock #op-bar-clock, sent alone once a second)
//!  POST /ops/notice ─▶ u60d /control op.notice_ack ─▶ #op-bar
//! ```
//! Every sentence is u60d's (`say_*`, `what_*`, `old_*`, `target_*`, `source_*`); this file only
//! picks the language, runs the countdown and decides what is still worth showing. A 3-second
//! result shows only if this tab's stream saw the change end ([`Seen`], one per stream): pages are
//! separate loads here, so a result that ended while navigating between them is not replayed.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::time::{Duration, Instant};

use askama::Template;
use axum::extract::{Query, Request, State};
use axum::response::sse::{Event, Sse};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::hub::{Link, St};
use crate::i18n::Lang;
use crate::pages::Shared;
use crate::topbar::Ctx;

/// A "brief" result (● 现在是 X) shows this long after the change ended.
const BRIEF: Duration = Duration::from_secs(3);
/// No frame from u60d for this long: the countdown can't be trusted (DD12).
const STALL: Duration = Duration::from_secs(20);

/// Where each item's page is (the bar links there and is not shown on it).
fn item_route(item: &str) -> &'static str {
    match item {
        "network.mode" => "/router/network-mode",
        "band.lte" | "band.nr" | "band.all" => "/bandlock",
        "cell.lte" | "cell.nr" => "/router/celllock",
        "apn" | "apn.use" => "/router/apn",
        "netselect.session" => "/router/mobile-network",
        "wifi.main" => "/router/wifi",
        "wifi.guest" => "/router/wifi-guest",
        "chill.profile" | "chill.exit" => "/services/chill",
        _ => "/changes",
    }
}

/// Another page whose own write area shows the item: "unlock all" on the cell lock page is
/// `band.reset` (item `band.all`), whose page is /bandlock.
fn also_on(item: &str, path: &str) -> bool {
    item == "band.all" && path == "/router/celllock"
}

/// What this tab's stream has seen of each change.
#[derive(Default)]
pub struct Seen {
    active: HashSet<String>,
    ended: HashMap<String, Instant>,
    /// `last.op_id` at the first read; `None` = nothing read yet.
    at_load: Option<Option<String>>,
    /// The countdown when it froze: (op_id, ms), kept while frozen so it doesn't jump.
    frozen_at: Option<(String, u64)>,
    /// The link was live at some read: before that, what the hub holds may be left over from
    /// the last connection (blocks survive the idle drop), so nothing is shown from it.
    live_seen: bool,
}

impl Seen {
    fn observe(&mut self, op: &Value, now: Instant) {
        if let Some(id) = op["active"]["op_id"].as_str() {
            self.active.insert(id.to_owned());
        }
        let last = op["last"]["op_id"].as_str().map(str::to_owned);
        let Some(at_load) = &self.at_load else {
            if let Some(id) = &last
                && self.active.contains(id)
                && !op["active"].is_object()
            {
                self.ended.insert(id.clone(), now);
            }
            self.at_load = Some(last);
            return;
        };
        if let Some(id) = last
            && !self.ended.contains_key(&id)
            && (Some(&id) != at_load.as_ref() || self.active.contains(&id))
        {
            self.ended.insert(id, now);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    /// Lost u60d while a change was running: result unknown.
    Disconnected,
    /// u60d's executor has not moved for 20 s: writes are off.
    Stuck,
    /// The one-time "auto revert is on" notice.
    Notice,
    Live,
    Brief,
    Sticky,
    Alert,
}

/// What the bar shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Bar {
    pub kind: Kind,
    /// `ok` / `warn` / `bad` / `neutral` (shell.css `nd-announce__sym--*`).
    pub tone: &'static str,
    pub say: String,
    pub detail: String,
    /// "Details ›" goes here (`None`: no link).
    pub href: Option<&'static str>,
    pub error: Option<String>,
}

impl Bar {
    pub fn sym(&self) -> &'static str {
        match self.tone {
            "warn" => "▲",
            "bad" => "■",
            _ => "●",
        }
    }
    pub fn alert(&self) -> bool {
        self.kind == Kind::Alert
    }
}

/// The countdown next to a running change's sentence.
#[derive(Debug, Clone, PartialEq)]
pub struct Clock {
    pub text: String,
    pub frozen: bool,
}

/// `m:ss`, rounded up, never negative.
pub fn fmt_clock(ms: u64) -> String {
    let s = ms.div_ceil(1000);
    format!("{}:{:02}", s / 60, s % 60)
}

fn tone(mark: &Value, stay: &str) -> &'static str {
    if stay == "alert" {
        return "bad";
    }
    match mark.as_str() {
        Some("ok") => "ok",
        Some("warn") => "warn",
        Some("bad") => "bad",
        _ => "neutral",
    }
}

/// One of u60d's paired texts in the page language (the other one if it is missing).
fn text(v: &Value, field: &str, l: Lang) -> String {
    let get = |suffix: &str| {
        v[format!("{field}_{suffix}")]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let (first, second) = match l {
        Lang::En => ("en", "zh"),
        Lang::Zh => ("zh", "en"),
    };
    get(first).or_else(|| get(second)).unwrap_or_default()
}

fn op_block(st: &St) -> Option<&crate::hub::Block> {
    st.blocks.get("op").filter(|b| b.data.is_object())
}

/// The countdown can't be trusted: u60d stuck, the block stale, the link not live, or no frame
/// for 20 s.
fn frozen(st: &St, now: Instant) -> bool {
    st.stuck
        || st.link != Link::Live
        || op_block(st).is_none_or(|b| b.stale)
        || st
            .last_ok
            .is_none_or(|t| now.saturating_duration_since(t) > STALL)
}

/// Reads the state through `seen` (call once per render, in order) and says what to show.
pub fn view(st: &St, seen: &mut Seen, ctx: &Ctx, now: Instant) -> (Option<Bar>, Option<Clock>) {
    let l = ctx.lang;
    seen.live_seen |= st.link == Link::Live;
    if !seen.live_seen {
        // a first visit after the idle drop: the last connection's op block until the snapshot
        return (None, None);
    }
    let op = op_block(st).map(|b| &b.data);
    if let Some(op) = op {
        seen.observe(op, now);
    }
    let active = op.map(|o| &o["active"]).filter(|a| a.is_object());
    let plain = |kind, tone, say: &str, detail: &str| {
        Some(Bar {
            kind,
            tone,
            say: say.into(),
            detail: detail.into(),
            href: None,
            error: None,
        })
    };
    // lost u60d after this stream saw it live (Connecting = retrying after Down)
    if st.link != Link::Live && active.is_some() {
        let say = l.pick(
            "Disconnected · result unknown",
            "和设备断开了 · 操作结果未知",
        );
        let detail = l.pick(
            "Nothing is resent. The result shows here once the device answers again.",
            "不会自动重发；连回来后结果会显示在这里。",
        );
        return (plain(Kind::Disconnected, "warn", say, detail), None);
    }
    if st.stuck && st.link == Link::Live {
        let say = l.pick("Data service not responding", "数据服务没响应");
        let detail = l.pick("Settings can't be changed for now.", "暂时不能改设置。");
        return (plain(Kind::Stuck, "bad", say, detail), None);
    }
    let Some(op) = op else {
        return (None, None);
    };
    let shown = if let Some(a) = active {
        Some((Kind::Live, a))
    } else {
        let last = &op["last"];
        let stay = last["stay"].as_str().unwrap_or("none");
        let waiting = last["needs_ack"]
            .as_bool()
            .unwrap_or(last["acked"].as_bool() == Some(false));
        let brief = || {
            last["op_id"]
                .as_str()
                .and_then(|id| seen.ended.get(id))
                .is_some_and(|t| now.saturating_duration_since(*t) < BRIEF)
        };
        match stay {
            "alert" if waiting => Some((Kind::Alert, last)),
            "sticky" if waiting => Some((Kind::Sticky, last)),
            "brief" if brief() => Some((Kind::Brief, last)),
            _ => None,
        }
    };
    let Some((kind, v)) = shown else {
        seen.frozen_at = None;
        if op["notice"].as_str() == Some("rollback_on") {
            let say = l.pick(
                "Auto revert on: reverts a mode change that can't connect",
                "自动退回已开：切模式没通会退回",
            );
            return (plain(Kind::Notice, "neutral", say, ""), None);
        }
        return (None, None);
    };
    let href = item_route(v["item"].as_str().unwrap_or_default());
    let here = ctx.path.trim_end_matches('/');
    if here == href || also_on(v["item"].as_str().unwrap_or_default(), here) {
        return (None, None);
    }
    let (old, target) = (text(v, "old", l), text(v, "target", l));
    let change = if old.is_empty() || target.is_empty() {
        String::new()
    } else {
        format!("{old} → {target}")
    };
    let detail = [text(v, "what", l), change, text(v, "source", l)]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    let bar = Bar {
        kind,
        tone: if kind == Kind::Live {
            "neutral"
        } else {
            tone(&v["mark"], v["stay"].as_str().unwrap_or(""))
        },
        say: text(v, "say", l),
        detail,
        href: Some(href),
        error: None,
    };
    let clock = (kind == Kind::Live)
        .then(|| v["remaining_ms"].as_u64())
        .flatten()
        .map(|ms| {
            let id = v["op_id"].as_str().unwrap_or_default().to_owned();
            let is_frozen = frozen(st, now) || v["frozen"].as_bool() == Some(true);
            let left = match &seen.frozen_at {
                Some((i, left)) if is_frozen && *i == id => *left,
                _ => {
                    let age = st
                        .op_at
                        .map_or(0, |t| now.saturating_duration_since(t).as_millis() as u64);
                    ms.saturating_sub(age)
                }
            };
            seen.frozen_at = is_frozen.then_some((id, left));
            Clock {
                text: fmt_clock(left),
                frozen: is_frozen,
            }
        });
    if clock.is_none() {
        seen.frozen_at = None;
    }
    (Some(bar), clock)
}

/// When this tab's bar changes by the clock alone: once a second while a change runs, and when
/// a 3-second result runs out.
pub fn wake(st: &St, seen: &Seen, now: Instant) -> Option<Instant> {
    let op = &op_block(st)?.data;
    if op["active"].is_object() {
        // right after the shown second changes (m:ss rounds up), so the clock doesn't lag
        let left = op["active"]["remaining_ms"].as_u64().map(|ms| {
            let age = st
                .op_at
                .map_or(0, |t| now.saturating_duration_since(t).as_millis() as u64);
            ms.saturating_sub(age)
        });
        let step = match left {
            Some(l) if l > 0 && !frozen(st, now) => match l % 1000 {
                0 => 1000,
                r => r,
            },
            _ => 1000,
        };
        return Some(now + Duration::from_millis(step + 5));
    }
    let id = op["last"]["op_id"].as_str()?;
    let end = *seen.ended.get(id)? + BRIEF;
    (end > now).then_some(end)
}

#[derive(Template)]
#[template(path = "opbar.html")]
struct OpbarFrag<'a> {
    l: Lang,
    bar: Option<&'a Bar>,
}

#[derive(Template)]
#[template(path = "opclock.html")]
struct ClockFrag<'a> {
    clock: Option<&'a Clock>,
}

/// The bar, with the clock's place left as `<!--slot:op-bar-clock-->` (topbar::fill).
pub fn bar_html(bar: Option<&Bar>, l: Lang) -> String {
    OpbarFrag { l, bar }.render().unwrap()
}

pub fn clock_html(clock: Option<&Clock>) -> String {
    ClockFrag { clock }.render().unwrap()
}

#[derive(Deserialize)]
pub struct NoticeQuery {
    #[serde(default)]
    p: String,
}

/// "Got it" on the auto-revert notice: u60d records it once for the screen and the web (DD18).
/// Straight to u60d's `/control` with the agent's body (`ops.rs` `act`).
pub async fn notice_ack(
    State(app): State<Shared>,
    Query(q): Query<NoticeQuery>,
    req: Request,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let path = if q.p.starts_with('/') {
        q.p
    } else {
        "/".into()
    };
    let ctx = Ctx::of_request(&req, &path);
    let body =
        json!({"action": "op.notice_ack", "source": "web", "params": {"notice": "rollback_on"}});
    let r = app.http.post(&app.control_url).json(&body).send().await;
    let err = match r {
        Ok(r) if r.status().is_success() => None,
        Ok(r) => Some(format!("HTTP {}", r.status().as_u16())),
        Err(_) => Some(
            ctx.lang
                .pick("Can't reach the data service", "连不上数据服务")
                .to_string(),
        ),
    };
    let html = app.hub.read(|st| {
        let (bar, clock) = view(st, &mut Seen::default(), &ctx, Instant::now());
        let bar = match (bar, err) {
            // u60d's new block may not be here yet: don't show the notice we just cleared
            (Some(b), None) if b.kind == Kind::Notice => None,
            (Some(mut b), Some(e)) => {
                b.error = Some(e);
                Some(b)
            }
            (b, _) => b,
        };
        crate::topbar::fill(
            &bar_html(bar.as_ref(), ctx.lang),
            &clock_html(clock.as_ref()),
        )
    });
    Sse::new(futures_util::stream::once(async move {
        Ok(crate::pages::patch(&html))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sse::Frame;
    use std::sync::Arc;

    fn ctx(path: &str) -> Ctx {
        Ctx {
            lang: Lang::Zh,
            path: path.into(),
            remote: crate::topbar::Via::Local,
        }
    }

    fn hub_op(op: Value) -> Arc<crate::hub::Hub> {
        let hub = crate::hub::Hub::new();
        hub.apply(&Frame {
            event: "snapshot".into(),
            data: json!({"epoch":"e","blocks":{"op":{"revision":1,"observed_at":1,"stale":false,"data":op}}})
                .to_string(),
        });
        hub
    }

    fn set_op(hub: &crate::hub::Hub, rev: u64, op: Value) {
        hub.apply(&Frame {
            event: "block".into(),
            data: json!({"epoch":"e","name":"op","revision":rev,"observed_at":1,"stale":false,"data":op})
                .to_string(),
        });
    }

    fn live(id: &str, ms: u64) -> Value {
        json!({"op_id": id, "item": "network.mode", "say_zh": "正在改网络模式", "say_en": "Changing network mode",
               "what_zh": "网络模式", "old_zh": "自动", "target_zh": "只用 4G", "source_zh": "网页",
               "remaining_ms": ms, "mark": null, "stay": "live"})
    }

    fn done(id: &str, stay: &str, mark: &str) -> Value {
        json!({"op_id": id, "item": "network.mode", "say_zh": "现在是只用 4G", "say_en": "Now 4G only",
               "remaining_ms": null, "mark": mark, "stay": stay, "acked": false, "needs_ack": stay != "brief"})
    }

    #[test]
    fn clock_rounds_up_and_never_goes_negative() {
        assert_eq!(fmt_clock(102_000), "1:42");
        assert_eq!(fmt_clock(101_001), "1:42");
        assert_eq!(fmt_clock(0), "0:00");
        assert_eq!(fmt_clock(59_999), "1:00");
    }

    #[test]
    fn a_running_change_counts_down_from_when_the_block_arrived() {
        let hub =
            hub_op(json!({"rollback_enabled": true, "active": live("a", 90_000), "last": null}));
        let mut seen = Seen::default();
        let (bar, clock) = hub.read(|st| view(st, &mut seen, &ctx("/charts"), Instant::now()));
        let bar = bar.unwrap();
        assert_eq!((bar.kind, bar.say.as_str()), (Kind::Live, "正在改网络模式"));
        assert_eq!(bar.detail, "网络模式 · 自动 → 只用 4G · 网页");
        assert_eq!(bar.href, Some("/router/network-mode"));
        assert_eq!(clock.as_ref().unwrap().text, "1:30");
        let later = Instant::now() + Duration::from_millis(5_500);
        let (_, clock) = hub.read(|st| view(st, &mut seen, &ctx("/charts"), later));
        // no frame for 5.5 s is still fine (< 20 s)
        assert_eq!(
            clock.unwrap(),
            Clock {
                text: "1:25".into(),
                frozen: false
            }
        );
        let (bar, _) = hub.read(|st| view(st, &mut seen, &ctx("/router/network-mode"), later));
        assert!(bar.is_none(), "not on the item's own page");
    }

    #[test]
    fn stuck_says_so_and_the_clock_holds_when_it_comes_back() {
        let hub = hub_op(json!({"active": live("a", 60_000), "last": null}));
        let mut seen = Seen::default();
        let hb = |age: u64| Frame {
            event: "heartbeat".into(),
            data: json!({"epoch":"e","seq":2,"blocks":{},"exec_age_ms":age}).to_string(),
        };
        hub.apply(&hb(25_000));
        let (bar, clock) = hub.read(|st| view(st, &mut seen, &ctx("/"), Instant::now()));
        assert_eq!(bar.unwrap().kind, Kind::Stuck);
        assert!(clock.is_none());
        hub.apply(&hb(10));
        let (bar, _) = hub.read(|st| view(st, &mut seen, &ctx("/"), Instant::now()));
        assert_eq!(bar.unwrap().kind, Kind::Live);
    }

    #[test]
    fn a_frozen_clock_stays_where_it_stopped() {
        let hub = hub_op(json!({"active": live("a", 60_000), "last": null}));
        let mut seen = Seen::default();
        let t0 = Instant::now();
        hub.read(|st| view(st, &mut seen, &ctx("/"), t0 + Duration::from_secs(5)));
        // nothing from u60d for 21 s: frozen at the first frozen read, then it holds
        let (_, c1) = hub.read(|st| view(st, &mut seen, &ctx("/"), t0 + Duration::from_secs(21)));
        let (_, c2) = hub.read(|st| view(st, &mut seen, &ctx("/"), t0 + Duration::from_secs(40)));
        assert!(c1.as_ref().unwrap().frozen);
        assert_eq!(c1, c2);
    }

    #[test]
    fn a_brief_result_only_after_seeing_it_end_and_only_for_3_s() {
        // opened the page after it ended: not replayed
        let hub = hub_op(json!({"active": null, "last": done("a", "brief", "ok")}));
        let mut seen = Seen::default();
        let t0 = Instant::now();
        assert!(
            hub.read(|st| view(st, &mut seen, &ctx("/"), t0))
                .0
                .is_none()
        );

        // saw it run, then end
        let hub = hub_op(json!({"active": live("b", 30_000), "last": null}));
        let mut seen = Seen::default();
        hub.read(|st| view(st, &mut seen, &ctx("/"), t0));
        set_op(
            &hub,
            2,
            json!({"active": null, "last": done("b", "brief", "ok")}),
        );
        let (bar, clock) =
            hub.read(|st| view(st, &mut seen, &ctx("/"), t0 + Duration::from_secs(1)));
        let bar = bar.unwrap();
        assert_eq!((bar.kind, bar.tone, bar.sym()), (Kind::Brief, "ok", "●"));
        assert!(clock.is_none());
        let w = hub
            .read(|st| wake(st, &seen, t0 + Duration::from_secs(1)))
            .unwrap();
        assert!(w > t0 + Duration::from_secs(1));
        let (bar, _) = hub.read(|st| view(st, &mut seen, &ctx("/"), t0 + Duration::from_secs(5)));
        assert!(bar.is_none());
    }

    #[test]
    fn sticky_and_alert_stay_until_acked_then_the_notice() {
        let hub = hub_op(
            json!({"active": null, "last": done("a", "sticky", "warn"), "notice": "rollback_on"}),
        );
        let mut seen = Seen::default();
        let bar = hub
            .read(|st| view(st, &mut seen, &ctx("/"), Instant::now()))
            .0
            .unwrap();
        assert_eq!((bar.kind, bar.sym()), (Kind::Sticky, "▲"));
        set_op(
            &hub,
            2,
            json!({"active": null, "last": done("a", "alert", "bad")}),
        );
        let bar = hub
            .read(|st| view(st, &mut seen, &ctx("/"), Instant::now()))
            .0
            .unwrap();
        assert!(bar.alert() && bar.tone == "bad");
        let mut acked = done("a", "sticky", "warn");
        acked["acked"] = json!(true);
        acked["needs_ack"] = json!(false);
        set_op(
            &hub,
            3,
            json!({"active": null, "last": acked, "notice": "rollback_on"}),
        );
        let bar = hub
            .read(|st| view(st, &mut seen, &ctx("/"), Instant::now()))
            .0
            .unwrap();
        assert_eq!(bar.kind, Kind::Notice);
        assert!(bar_html(Some(&bar), Lang::Zh).contains("/ops/notice"));
    }

    #[test]
    fn lost_u60d_during_a_change_says_result_unknown() {
        let hub = hub_op(json!({"active": live("a", 60_000), "last": null}));
        let mut seen = Seen::default();
        hub.read(|st| view(st, &mut seen, &ctx("/"), Instant::now()));
        hub.set_link_for_test(Link::Down);
        let bar = hub
            .read(|st| view(st, &mut seen, &ctx("/"), Instant::now()))
            .0
            .unwrap();
        assert_eq!(bar.kind, Kind::Disconnected);
    }

    #[test]
    fn english_falls_back_to_chinese_per_field() {
        let mut c = ctx("/");
        c.lang = Lang::En;
        let hub = hub_op(json!({"active": live("a", 60_000), "last": null}));
        let bar = hub
            .read(|st| view(st, &mut Seen::default(), &c, Instant::now()))
            .0
            .unwrap();
        assert_eq!(bar.say, "Changing network mode");
        assert!(bar.detail.starts_with("网络模式"), "{}", bar.detail);
    }

    #[test]
    fn a_running_change_sends_only_its_clock_and_a_resent_bar_carries_it() {
        use crate::topbar::{diff, leaves};
        let hub = hub_op(json!({"active": live("a", 90_000), "last": null}));
        let c = ctx("/charts");
        let mut seen = Seen::default();
        let a = hub.read(|st| leaves(st, &c, &mut seen));
        std::thread::sleep(Duration::from_millis(1_100));
        let b = hub.read(|st| leaves(st, &c, &mut seen));
        let out = diff(&a, &b);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(
            out[0].starts_with("<span id=\"op-bar-clock\""),
            "{}",
            out[0]
        );
        // the sentence changes: one patch, the bar with the current clock in it
        let mut next = live("a", 80_000);
        next["say_zh"] = json!("正在核对");
        set_op(&hub, 2, json!({"active": next, "last": null}));
        let d = hub.read(|st| leaves(st, &c, &mut seen));
        let out = diff(&b, &d);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(
            out[0].contains("正在核对")
                && out[0].contains("id=\"op-bar-clock\"")
                && out[0].contains("1:20"),
            "{}",
            out[0]
        );
        assert!(!out[0].contains("slot:"));
    }

    #[test]
    fn nothing_from_a_previous_connection_before_this_one_is_live() {
        let hub = hub_op(json!({"active": live("a", 60_000), "last": null}));
        // all tabs closed, the upstream dropped; a new tab arrives while it reconnects
        hub.set_link_for_test(Link::Connecting);
        let mut seen = Seen::default();
        assert_eq!(
            hub.read(|st| view(st, &mut seen, &ctx("/"), Instant::now())),
            (None, None)
        );
        hub.set_link_for_test(Link::Live);
        let bar = hub
            .read(|st| view(st, &mut seen, &ctx("/"), Instant::now()))
            .0
            .unwrap();
        assert_eq!(bar.kind, Kind::Live);
        // then lost: retrying counts as disconnected, no flapping between Down and Connecting
        for link in [Link::Down, Link::Connecting] {
            hub.set_link_for_test(link);
            let bar = hub
                .read(|st| view(st, &mut seen, &ctx("/"), Instant::now()))
                .0
                .unwrap();
            assert_eq!(bar.kind, Kind::Disconnected, "{link:?}");
        }
    }

    #[test]
    fn item_routes_cover_apn_use_and_chill() {
        assert_eq!(item_route("apn.use"), "/router/apn");
        assert_eq!(item_route("chill.profile"), "/services/chill");
        assert_eq!(item_route("chill.exit"), "/services/chill");
    }
}
