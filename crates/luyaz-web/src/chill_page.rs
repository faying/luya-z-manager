//! /services/chill (batch 5b), the old `services/chill/page.tsx` + `lib/chill.ts`.
//!
//! ```text
//!  agent (tab's own session; u60d first once it has them)        one push stream per tab
//!   /api/services/chill ........ 4 s   state, exit, regions, groups, vitals, notice ─┐
//!   /api/services/chill/home .... 1 s while busy, else 5 s (not on an unstarted CHILL)├─▶ leaves
//!   /api/services/chill/providers 15 s (only while running)                           │   (only
//!   /api/services/chill/bypass .. 8 s       /api/network/clients 15 s                 │   what
//!   /api/services/chill/log ..... 4 s, not while this tab paused it                  ─┘   changed)
//!  buttons ─▶ writes.rs: ask (tier 2) ─▶ set ─▶ agent ─▶ job followed / readback ─▶ the section's area
//! ```
//! - One [`Poll`] per tab keeps a due time per source and reads only what is due (all at once on
//!   刷新, after a write, or when another tab kicks it). `/job` is read only inside a write that
//!   starts a job. Nothing is read once the stream is closed.
//! - Log pause is a flag per tab (`PAUSED`, keyed by the tab id in the events URL), because the
//!   stream, not the browser, does the reading.
//! - The dashboard link goes to luyaz-web's own `/chill-ui/` on this origin (chill_proxy.rs: the
//!   files, and `/chill-api/` to mihomo, behind the login session), so the dashboard secret is
//!   never read, never in HTML, a push or a URL. Notes on what that does and does not cover are in
//!   the batch report.

mod data;
mod view;
mod writes;

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use askama::Template;
use axum::Router;
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::{get, post};
use serde::Deserialize;

use crate::agent::{Caller, Polled};
use crate::i18n::Lang;
use crate::pages::Shared;
use crate::write::Phase;
use view::{AREAS, Got, area, leaves};

pub(crate) const PATH: &str = "/services/chill";

/// Bumped by 刷新 and by every write: each stream reads everything on its next turn.
static FORCE: AtomicU64 = AtomicU64::new(0);

pub(crate) fn bump(app: &Shared) {
    FORCE.fetch_add(1, Ordering::Relaxed);
    let _ = app.kick.send(PATH);
}

fn paused() -> &'static Mutex<HashMap<String, (bool, Instant)>> {
    static P: std::sync::OnceLock<Mutex<HashMap<String, (bool, Instant)>>> = std::sync::OnceLock::new();
    P.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn set_paused(tab: &str, on: bool) {
    if tab.is_empty() || tab.len() > 64 {
        return;
    }
    let mut m = paused().lock().unwrap();
    m.retain(|_, (_, at)| at.elapsed() < Duration::from_secs(6 * 3600));
    m.insert(tab.to_owned(), (on, Instant::now()));
}

fn is_paused(tab: &str) -> bool {
    paused().lock().unwrap().get(tab).is_some_and(|(on, _)| *on)
}

pub fn routes() -> Router<Shared> {
    use writes::*;
    Router::new()
        .route(PATH, get(page))
        .route("/services/chill/events", get(events))
        .route("/services/chill/refresh", post(refresh))
        .route("/services/chill/log/pause", post(pause))
        .route("/services/chill/cancel", post(cancel))
        .route("/services/chill/power/ask", post(power_ask))
        .route("/services/chill/power/set", post(power_set))
        .route("/services/chill/exit/ask", post(exit_ask))
        .route("/services/chill/exit/set", post(exit_set))
        .route("/services/chill/pick/set", post(pick_set))
        .route("/services/chill/profile/set", post(profile_set))
        .route("/services/chill/home/ask", post(home_ask))
        .route("/services/chill/home/set", post(home_set))
        .route("/services/chill/home/extend", post(home_extend))
        .route("/services/chill/prov/refresh/ask", post(prov_refresh_ask))
        .route("/services/chill/prov/refresh/set", post(prov_refresh_set))
        .route("/services/chill/prov/edit", post(prov_edit))
        .route("/services/chill/prov/edit/close", post(prov_edit_close))
        .route("/services/chill/prov/save/ask", post(prov_save_ask))
        .route("/services/chill/prov/save/set", post(prov_save_set))
        .route("/services/chill/byp/ask", post(byp_ask))
        .route("/services/chill/byp/set", post(byp_set))
}

// ---- the stream ----

const STATUS_EVERY: Duration = Duration::from_secs(4);
const PROV_EVERY: Duration = Duration::from_secs(15);
const BYPASS_EVERY: Duration = Duration::from_secs(8);
const CLIENTS_EVERY: Duration = Duration::from_secs(15);
const LOG_EVERY: Duration = Duration::from_secs(4);

/// When a source is read next (None = now).
#[derive(Default)]
struct Due {
    status: Option<Instant>,
    home: Option<Instant>,
    prov: Option<Instant>,
    bypass: Option<Instant>,
    clients: Option<Instant>,
    log: Option<Instant>,
}

pub struct Poll {
    who: Caller,
    l: Lang,
    tab: String,
    got: Got,
    due: Due,
    forced: u64,
    was_paused: bool,
}

impl Drop for Poll {
    fn drop(&mut self) {
        paused().lock().unwrap().remove(&self.tab);
    }
}

impl Poll {
    fn new(who: Caller, l: Lang, tab: String) -> Poll {
        Poll {
            who,
            l,
            tab,
            got: Got::default(),
            due: Due::default(),
            forced: FORCE.load(Ordering::Relaxed),
            was_paused: false,
        }
    }

    fn leaves(&self) -> Vec<(String, String)> {
        leaves(&self.got, self.l, &self.tab, data::now())
    }
}

fn is_due(d: Option<Instant>, all: bool, now: Instant) -> bool {
    all || d.is_none_or(|d| d <= now)
}

async fn get_if(app: &Shared, who: &Caller, due: bool, path: &str) -> Option<Result<serde_json::Value, crate::agent::Fail>> {
    if due { Some(crate::agent::get(app, who, path).await) } else { None }
}

impl Polled for Poll {
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        let now = Instant::now();
        let f = FORCE.load(Ordering::Relaxed);
        let all = f != self.forced;
        self.forced = f;
        let paused_now = is_paused(&self.tab);
        let log_on = !paused_now && (all || self.was_paused);
        self.was_paused = paused_now;

        if let Some(r) = get_if(app, &self.who, is_due(self.due.status, all, now), "/api/services/chill").await {
            self.got.status.set(r);
            self.due.status = Some(now + STATUS_EVERY);
        }
        let st = self.got.status.v.as_ref().map_or("", |v| data::s(v, "state"));
        let running = st == "running";
        let known = !st.is_empty() && st != "unknown";
        let d_home = known && is_due(self.due.home, all, now);
        let d_prov = running && is_due(self.due.prov, all, now);
        let d_byp = is_due(self.due.bypass, all, now);
        let d_cli = is_due(self.due.clients, all, now);
        let d_log = !paused_now && (log_on || is_due(self.due.log, all, now));
        let (home, prov, byp, cli, log) = tokio::join!(
            get_if(app, &self.who, d_home, "/api/services/chill/home"),
            get_if(app, &self.who, d_prov, "/api/services/chill/providers"),
            get_if(app, &self.who, d_byp, "/api/services/chill/bypass"),
            get_if(app, &self.who, d_cli, "/api/network/clients"),
            get_if(app, &self.who, d_log, "/api/services/chill/log?lines=200"),
        );
        let after = Instant::now();
        if let Some(r) = home {
            self.got.home.set(r);
            let busy = self.got.home.v.as_ref().is_some_and(|h| h["busy"].is_object());
            self.due.home = Some(after + if busy { Duration::from_secs(1) } else { Duration::from_secs(5) });
        }
        if let Some(r) = prov {
            self.got.prov.set(r);
            self.due.prov = Some(after + PROV_EVERY);
        }
        if let Some(r) = byp {
            self.got.bypass.set(r);
            self.due.bypass = Some(after + BYPASS_EVERY);
        }
        if let Some(r) = cli {
            self.got.clients.set(r);
            self.due.clients = Some(after + CLIENTS_EVERY);
        }
        if let Some(r) = log {
            self.got.log.set(r);
            self.due.log = Some(after + LOG_EVERY);
        }
        // the next turn: the nearest source that is wanted now
        let mut next = self.due.status;
        let mut near = |d: Option<Instant>, wanted: bool| {
            if wanted && let Some(d) = d {
                next = Some(next.map_or(d, |n| n.min(d)));
            }
        };
        near(self.due.home, known);
        near(self.due.prov, running);
        near(self.due.bypass, true);
        near(self.due.clients, true);
        near(self.due.log, !paused_now);
        let wait = next.map_or(STATUS_EVERY, |n| n.saturating_duration_since(Instant::now()));
        (self.leaves(), wait.max(Duration::from_millis(200)))
    }
}

#[derive(Deserialize, Default)]
pub struct EventsQuery {
    #[serde(default)]
    g: u64,
    #[serde(default)]
    t: String,
}

#[derive(Template)]
#[template(path = "chill.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    leaves: Vec<(String, String)>,
    ops: [String; 8],
}

impl Page {
    fn leaf(&self, id: &str) -> &str {
        self.leaves.iter().find(|(k, _)| k == id).map_or("", |(_, h)| h.as_str())
    }
}

async fn page(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let tab = crate::auth::random_id();
    let p = Poll::new(Caller::of(&app, &req), l, tab.clone());
    let shell = app.hub.read(|st| {
        crate::shell::Shell::new(&headers, PATH).live(st, &ctx, Some(format!("{PATH}/events?g={}&t={tab}", st.generation)))
    });
    let idle = Phase::Idle;
    let ops = AREAS.map(|id| area(id, l, ctx.remote, "", &idle));
    let html = Page { shell, l, leaves: p.leaves(), ops }.render().unwrap();
    crate::assets::maybe_gzip(&headers, "text/html; charset=utf-8", html.into_bytes(), "no-store")
}

async fn events(State(app): State<Shared>, Query(q): Query<EventsQuery>, req: axum::extract::Request) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let tab = if q.t.is_empty() || q.t.len() > 64 { crate::auth::random_id() } else { q.t };
    let p = Poll::new(Caller::of(&app, &req), ctx.lang, tab);
    crate::agent::stream(app, ctx, q.g, PATH, p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::split_leaves;
    use crate::agent::Fail;
    use serde_json::json;

    fn got() -> Got {
        let mut g = Got::default();
        g.status.set(Ok(json!({"state": "running", "cpuss_c": 47, "mem_avail_mb": 612, "core_pid": 4127, "started_at": "2026-10-13T08:00:00Z",
            "version": "v1.19.14", "mode": "rule", "exit": "proxy", "profile": "standard", "profile_effective": "standard",
            "region": {"active": "🇹🇼 台湾", "options": ["🇯🇵 日本", "🇹🇼 台湾", "DIRECT"]},
            "ai_exit": {"active": "🇺🇸 美国", "options": ["🇺🇸 美国"]},
            "groups": [{"name": "🚀 节点选择", "now": "🇹🇼 台湾", "size": 6}]})));
        g.prov.set(Ok(json!({"providers": [{"name": "sub", "vehicle_type": "HTTP", "updated_at": "2026-10-13T08:00:00.4Z", "node_count": 24,
            "subscription": {"Upload": 1, "Download": 2, "Total": 100, "Expire": 1798732800}, "editable": true}]})));
        g.bypass.set(Ok(json!({"ips": ["192.168.0.105"], "stale": ["192.168.0.105"]})));
        g.clients.set(Ok(json!({"dhcp_leases": [{"hostname": "tv", "macaddr": "aa", "ipaddr": "192.168.0.105"}]})));
        g.log.set(Ok(json!({"lines": ["a <b>", "c"]})));
        g
    }

    #[test]
    fn every_leaf_has_its_id_first() {
        for (g, l) in [(got(), Lang::En), (got(), Lang::Zh), (Got::default(), Lang::En)] {
            let ls = leaves(&g, l, "tab1", 1_790_000_000);
            let html: String = ls.iter().map(|(_, h)| format!("{h}<!--leaf-->")).collect();
            let ids: Vec<String> = split_leaves(&html).into_iter().map(|(k, _)| k).collect();
            let want: Vec<&str> = ls.iter().map(|(k, _)| k.as_str()).collect();
            assert_eq!(ids, want);
        }
    }

    #[test]
    fn running_page_shows_its_controls_and_escapes_device_text() {
        let ls = leaves(&got(), Lang::En, "tab1", 1_790_000_000);
        let all: String = ls.iter().map(|(_, h)| h.as_str()).collect();
        assert!(all.contains("/services/chill/exit/set?v=proxy") && all.contains("/services/chill/exit/ask"));
        assert!(all.contains("pick/set?g=main&i=1&h=") && all.contains("pick/set?g=ai&i=0&h="));
        assert!(all.contains("byp/ask?ip=192.168.0.105&on=0") && all.contains("prov/edit?i=0&h="));
        assert!(all.contains("a &lt;b&gt;\nc") && !all.contains("a <b>"));
        // a device name never in a URL, an expression or an aria-label
        assert!(!all.contains("%E") && !all.contains("aria-label=\"🇹🇼"));
        // the dashboard secret is never read
        assert!(!all.contains("secret"));
        assert!(all.contains("Running") && all.contains("mihomo v1.19.14 · mode rule"));
    }

    #[test]
    fn unknown_state_can_still_start() {
        let mut g = Got::default();
        g.status.set(Ok(json!({"state": "unknown"})));
        let ls = leaves(&g, Lang::En, "t", 0);
        let st = &ls[0].1;
        assert!(st.contains("Not started") && st.contains("power/ask?act=start"), "{st}");
        assert_eq!(ls[1].1, "<div id=\"ch-exit\"></div>");
    }

    #[test]
    fn unreadable_status_says_so_and_offers_retry() {
        let mut g = Got::default();
        g.status.set(Err(Fail::Unreachable));
        let st = &leaves(&g, Lang::En, "t", 0)[0].1;
        assert!(st.contains("Can't read CHILL status") && st.contains("refresh"), "{st}");
    }

    #[test]
    fn a_failed_later_read_keeps_the_last_data_and_greys_it() {
        let mut g = got();
        g.status.set(Err(Fail::Unreachable));
        let st = &leaves(&g, Lang::En, "t", 0)[0].1;
        assert!(st.contains("nd-stale") && st.contains("Status stopped at"), "{st}");
    }

    #[test]
    fn pause_flag_is_per_tab() {
        set_paused("tab-a", true);
        assert!(is_paused("tab-a") && !is_paused("tab-b"));
        set_paused("tab-a", false);
        assert!(!is_paused("tab-a"));
    }
}
