//! /router/dns (DNS 设置), the upstream servers: auto (the
//! operator's servers) or manual (a primary and an optional secondary IPv4 server), the agent's
//! `/api/router/dns` GET / PUT.
//!
//! - **Dropped on purpose (owner's call, batch 5c):** the DoH part (`/api/doh/*`: the DoH mode,
//!   its upstream URL, the proxy numbers, the cache list, "clear cache"). It went away with the
//!   agent; so did the old page's "turn DoH off after applying" step (owner, 8d-2).
//! - A wrong upstream breaks name resolution, so the old checks stay (manual: primary required,
//!   both IPv4; checked again on the server before the write) and so does the tier: tier 2
//!   (inline confirm), tier 3 (dialog with the Tailscale warning) when the tab came over Tailscale.
//! - Readback: the page reads `/api/router/dns` again (up to ~8 s) until mode and servers are what
//!   was sent; a PUT the agent took but the readback did not show says "accepted, not read back".
//! - The form is the tab's draft (Datastar signals `_dm`, `_dp`, `_ds`, set once from the device's
//!   values when the page loads); the stream pushes only the status block and the "operator DNS
//!   now" hint, polled every 30 s like the old page.

use std::convert::Infallible;
use std::time::{Duration, SystemTime};

use askama::Template;
use axum::Router;
use axum::extract::{Query, State};
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

const PATH: &str = "/router/dns";
const API: &str = "/api/router/dns";
const EVERY: Duration = Duration::from_secs(30);

pub fn routes() -> Router<Shared> {
    Router::new()
        .route(PATH, get(page))
        .route("/router/dns/events", get(events))
        .route("/router/dns/refresh", post(refresh))
        .route("/router/dns/ask", post(ask))
        .route("/router/dns/set", post(set))
        .route("/router/dns/cancel", post(cancel))
}

// ---- the data ----

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Dns {
    pub manual: bool,
    pub primary: String,
    pub secondary: String,
    /// The operator's own servers (shown, not editable).
    pub auto1: String,
    pub auto2: String,
}

pub fn parse(v: &Value) -> Option<Dns> {
    let mode = v["dns_mode"].as_str()?;
    let s = |k: &str| v[k].as_str().unwrap_or("").trim().to_owned();
    Some(Dns {
        manual: mode == "manual",
        primary: s("prefer_dns_manual"),
        secondary: s("standby_dns_manual"),
        auto1: s("prefer_dns_auto"),
        auto2: s("standby_dns_auto"),
    })
}

/// The old page's `isValidIPv4`: four dotted numbers, 0–255.
pub fn valid_ipv4(s: &str) -> bool {
    let p: Vec<&str> = s.split('.').collect();
    p.len() == 4
        && p.iter().all(|x| {
            !x.is_empty()
                && x.bytes().all(|b| b.is_ascii_digit())
                && x.parse::<u64>().is_ok_and(|n| n <= 255)
        })
}

#[derive(Deserialize, Default, Debug, Clone)]
pub struct Draft {
    #[serde(default, rename = "_dm")]
    pub mode: String,
    #[serde(default, rename = "_dp")]
    pub primary: String,
    #[serde(default, rename = "_ds")]
    pub secondary: String,
}

/// What will be sent: the draft after the old page's checks.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub manual: bool,
    pub primary: String,
    pub secondary: String,
}

impl Plan {
    fn body(&self) -> Value {
        if self.manual {
            json!({"dns_mode": "manual", "prefer_dns_manual": self.primary, "standby_dns_manual": self.secondary})
        } else {
            json!({"dns_mode": "auto", "prefer_dns_manual": "", "standby_dns_manual": ""})
        }
    }
    fn holds(&self, d: &Dns) -> bool {
        if self.manual {
            d.manual && d.primary == self.primary && d.secondary == self.secondary
        } else {
            !d.manual
        }
    }
    fn servers(&self) -> String {
        [self.primary.as_str(), self.secondary.as_str()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

pub fn check(d: &Draft, l: Lang) -> Result<Plan, write::Outcome> {
    let no = |s: &'static str, z: &'static str| Err(write::Outcome::NotYet(l.pick(s, z).into()));
    match d.mode.as_str() {
        "auto" => Ok(Plan { manual: false, primary: String::new(), secondary: String::new() }),
        "manual" => {
            let (p, s) = (d.primary.trim(), d.secondary.trim());
            if p.is_empty() {
                return no("Fill in the primary DNS", "主 DNS 要填");
            }
            if !valid_ipv4(p) {
                return no("The primary DNS isn't an IPv4 address", "主 DNS 不是 IPv4 地址");
            }
            if !s.is_empty() && !valid_ipv4(s) {
                return no("The second DNS isn't an IPv4 address", "第二个 DNS 不是 IPv4 地址");
            }
            Ok(Plan { manual: true, primary: p.into(), secondary: s.into() })
        }
        _ => no("Choose a DNS mode", "请选择 DNS 模式"),
    }
}

// ---- the view ----

#[derive(Default)]
pub struct Got {
    pub dns: Option<Dns>,
    pub err: Option<Fail>,
    pub at: Option<SystemTime>,
}

pub struct View {
    pub l: Lang,
    pub tone: &'static str,
    pub state: String,
    pub reason: Option<String>,
    pub meta: Option<String>,
    pub retry: bool,
    /// "Now: 1.1.1.1 · 1.0.0.1" under the auto choice.
    pub operator: Option<String>,
}

impl View {
    pub fn symbol(&self) -> &'static str {
        match self.tone {
            "bad" => "■",
            "stale" => "▲",
            _ => "●",
        }
    }
}

pub fn view(got: &Got, l: Lang) -> View {
    let stale = got.dns.is_some() && got.err.is_some();
    let join = |a: &str, b: &str| {
        let s = [a, b].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
        (!s.is_empty()).then_some(s)
    };
    let (mut tone, state, reason) = match (&got.dns, &got.err) {
        (None, Some(e)) => ("bad", l.pick("Can't read DNS settings", "读不到 DNS 设置").to_owned(), Some(e.text(l))),
        (None, None) => ("neutral", l.pick("Reading DNS settings…", "正在读取 DNS 设置…").to_owned(), None),
        (Some(d), _) if d.manual => (
            "ok",
            l.pick("Manual DNS", "手动 DNS").to_owned(),
            Some(join(&d.primary, &d.secondary).unwrap_or_else(|| "—".into())),
        ),
        (Some(d), _) => (
            "ok",
            l.pick("Auto (the carrier's servers)", "自动（用运营商的服务器）").to_owned(),
            join(&d.auto1, &d.auto2),
        ),
    };
    if stale {
        tone = "stale";
    }
    View {
        l,
        tone,
        state,
        reason,
        meta: stale.then(|| crate::agent::stopped(l, "Settings", "设置", got.at)),
        retry: got.err.is_some(),
        operator: got.dns.as_ref().and_then(|d| join(&d.auto1, &d.auto2)),
    }
}

#[derive(Template)]
#[template(path = "dns_leaves.html")]
struct Leaves<'a> {
    v: &'a View,
}

pub struct Poll {
    who: Caller,
    l: Lang,
    got: Got,
}

impl Poll {
    async fn fetch(&mut self, app: &Shared) {
        match crate::agent::get(app, &self.who, API).await {
            Ok(v) => match parse(&v) {
                Some(d) => {
                    self.got.dns = Some(d);
                    self.got.err = None;
                    self.got.at = Some(SystemTime::now());
                }
                None => {
                    self.got.err =
                        Some(Fail::Refused(200, "unexpected reply".into(), "unexpected reply".into()))
                }
            },
            Err(e) => self.got.err = Some(e),
        }
    }
    fn leaves(&self) -> Vec<(String, String)> {
        let v = view(&self.got, self.l);
        split_leaves(&Leaves { v: &v }.render().unwrap())
    }
}

impl Polled for Poll {
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        self.fetch(app).await;
        (self.leaves(), EVERY)
    }
}

// ---- the page ----

#[derive(Template)]
#[template(path = "dns.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    leaves: Vec<(String, String)>,
    /// `data-signals` object: the device's values as the draft's start.
    sigs: String,
    manual: bool,
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
        id: "dns-w",
        l,
        remote,
        set,
        cancel: "/router/dns/cancel",
        phase,
    }
    .html()
}

pub async fn page(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let mut p = Poll { who: Caller::of(&app, &req), l: ctx.lang, got: Got::default() };
    p.fetch(&app).await;
    let d = p.got.dns.clone().unwrap_or_default();
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
        leaves: p.leaves(),
        sigs: json!({"_dm": if d.manual {"manual"} else {"auto"}, "_dp": d.primary, "_ds": d.secondary}).to_string(),
        manual: d.manual,
        op: area(ctx.lang, ctx.remote, "", &write::Phase::Idle),
    }
    .render()
    .unwrap();
    crate::assets::maybe_gzip(&headers, "text/html; charset=utf-8", html.into_bytes(), "no-store")
}

pub async fn events(
    State(app): State<Shared>,
    Query(q): Query<StreamQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let p = Poll { who: Caller::of(&app, &req), l: ctx.lang, got: Got::default() };
    crate::agent::stream(app, ctx, q.g, PATH, p)
}

pub async fn refresh(State(app): State<Shared>) -> axum::http::StatusCode {
    let _ = app.kick.send(PATH);
    axum::http::StatusCode::NO_CONTENT
}

// ---- applying ----

fn one(html: String) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(futures_util::stream::once(async move {
        Ok(crate::pages::patch(&html))
    }))
}

pub fn confirm(p: &Plan, remote: Via, l: Lang) -> write::Confirm {
    let what = match (l, p.manual) {
        (Lang::En, true) => format!(
            "The router's DNS becomes {}. DNS restarts; names may not resolve for a few seconds.",
            p.servers()
        ),
        (Lang::Zh, true) => format!(
            "路由器的 DNS 改为 {}。DNS 会重启，几秒内可能解析不了域名。",
            p.servers()
        ),
        (Lang::En, false) => "The router goes back to the DNS servers the operator hands out. DNS restarts; names may not resolve for a few seconds.".to_owned(),
        (Lang::Zh, false) => "路由器改回用运营商分配的 DNS。DNS 会重启，几秒内可能解析不了域名。".to_owned(),
    };
    write::Confirm {
        tier: if remote.remote() { write::Tier::Dialog } else { write::Tier::Inline },
        title: l.pick("Apply the DNS settings?", "应用 DNS 设置？").into(),
        what,
        downtime: Some(l.pick("A few seconds while DNS restarts.", "DNS 重启期间的几秒钟。").into()),
        recovery: Some(
            l.pick(
                "If names stop resolving, set the mode back to Auto and apply again.",
                "如果域名解析不了，把模式改回「自动」再应用一次。",
            )
            .into(),
        ),
        action: l.pick("Confirm: apply", "确认：应用").into(),
        // a bad upstream can cut the remote session
        cuts_uplink: true,
        word: None,
        danger: false,
        revert: crate::write::Revert::No,
    }
}

async fn body(req: axum::extract::Request) -> Vec<u8> {
    axum::body::to_bytes(req.into_body(), 16 * 1024)
        .await
        .map(|b| b.to_vec())
        .unwrap_or_default()
}

pub async fn ask(State(_app): State<Shared>, req: axum::extract::Request) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let d: Draft = serde_json::from_slice(&body(req).await).unwrap_or_default();
    one(match check(&d, l) {
        Err(o) => area(l, ctx.remote, "", &write::Phase::Done(o)),
        Ok(p) => {
            let set = if p.manual {
                format!("{PATH}/set?mode=manual&p={}&s={}", p.primary, p.secondary)
            } else {
                format!("{PATH}/set?mode=auto")
            };
            area(l, ctx.remote, &set, &write::Phase::Ask(confirm(&p, ctx.remote, l)))
        }
    })
}

#[derive(Deserialize, Default)]
pub struct SetQ {
    #[serde(default)]
    mode: String,
    #[serde(default)]
    p: String,
    #[serde(default)]
    s: String,
}

/// Read the settings until `p` holds (looks 1.5 s apart, ~8 s): the agent restarts dnsmasq after
/// the write. None = never read.
async fn read_back(app: &Shared, who: &Caller, p: &Plan) -> Option<bool> {
    let mut read_once = false;
    for i in 0..5 {
        if i > 0 {
            tokio::time::sleep(Duration::from_millis(1500)).await;
        }
        if let Ok(v) = crate::agent::get(app, who, API).await
            && let Some(d) = parse(&v)
        {
            read_once = true;
            if p.holds(&d) {
                return Some(true);
            }
        }
    }
    read_once.then_some(false)
}

pub async fn apply(app: &Shared, who: &Caller, p: &Plan, l: Lang) -> write::Outcome {
    let no_reply = match crate::agent::send_json(app, who, reqwest::Method::PUT, API, &p.body()).await {
        Ok(_) => false,
        Err(Fail::Unreachable) => true,
        Err(e) => return write::Outcome::Failed(e.text(l)),
    };
    let _ = app.kick.send(PATH);
    match read_back(app, who, p).await {
        Some(true) => write::Outcome::Applied(if p.manual {
            match l {
                Lang::En => format!("DNS servers set to {}", p.servers()),
                Lang::Zh => format!("DNS 已改为 {}", p.servers()),
            }
        } else {
            l.pick("Using the operator's DNS servers", "已改回运营商分配的 DNS").into()
        }),
        _ if no_reply => write::Outcome::Unknown,
        _ => write::Outcome::Accepted,
    }
}

pub async fn set(
    State(app): State<Shared>,
    Query(q): Query<SetQ>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    let who = Caller::of(&app, &req);
    // checked again here: the confirm's URL is the tab's to edit
    let plan = check(&Draft { mode: q.mode, primary: q.p, secondary: q.s }, l);
    let pending = area(l, Via::Local, "", &write::Phase::Pending(l.pick("Applying the DNS settings…", "正在应用 DNS 设置…").into()));
    write::respond(pending, async move {
        let o = match plan {
            Err(o) => o,
            Ok(p) => apply(&app, &who, &p, l).await,
        };
        area(l, Via::Local, "", &write::Phase::Done(o))
    })
}

pub async fn cancel(Query(q): Query<write::CancelQuery>, req: axum::extract::Request) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    one(area(ctx.lang, ctx.remote, "", &write::cancel_phase(&q)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(mode: &str, p: &str, s: &str) -> Draft {
        Draft { mode: mode.into(), primary: p.into(), secondary: s.into() }
    }

    #[test]
    fn the_old_checks_stay() {
        assert!(valid_ipv4("1.1.1.1") && valid_ipv4("255.0.0.0") && valid_ipv4("001.2.3.4"));
        for bad in ["", "1.1.1", "1.1.1.1.1", "256.1.1.1", "a.b.c.d", "1..1.1", "1.1.1.-1", "99999999999999999999.1.1.1"] {
            assert!(!valid_ipv4(bad), "{bad}");
        }
        let msg = |r: Result<Plan, write::Outcome>| match r {
            Err(write::Outcome::NotYet(s)) => s,
            o => panic!("{o:?}"),
        };
        assert_eq!(msg(check(&d("manual", "", ""), Lang::En)), "Fill in the primary DNS");
        assert_eq!(msg(check(&d("manual", "1.1.1", ""), Lang::En)), "The primary DNS isn't an IPv4 address");
        assert_eq!(msg(check(&d("manual", "1.1.1.1", "x"), Lang::En)), "The second DNS isn't an IPv4 address");
        assert_eq!(msg(check(&d("doh", "1.1.1.1", ""), Lang::En)), "Choose a DNS mode");
        let p = check(&d("manual", " 1.1.1.1 ", ""), Lang::En).unwrap();
        assert_eq!(p.body(), json!({"dns_mode":"manual","prefer_dns_manual":"1.1.1.1","standby_dns_manual":""}));
        let a = check(&d("auto", "junk", "junk"), Lang::En).unwrap();
        assert_eq!(a.body(), json!({"dns_mode":"auto","prefer_dns_manual":"","standby_dns_manual":""}));
    }

    #[test]
    fn readback_compares_what_was_sent() {
        let p = check(&d("manual", "1.1.1.1", "1.0.0.1"), Lang::En).unwrap();
        let v = json!({"dns_mode":"manual","prefer_dns_manual":"1.1.1.1","standby_dns_manual":"1.0.0.1","prefer_dns_auto":"8.8.8.8"});
        assert!(p.holds(&parse(&v).unwrap()));
        let v2 = json!({"dns_mode":"manual","prefer_dns_manual":"1.1.1.1","standby_dns_manual":""});
        assert!(!p.holds(&parse(&v2).unwrap()));
        let a = check(&d("auto", "", ""), Lang::En).unwrap();
        assert!(!a.holds(&parse(&v).unwrap()));
        assert!(a.holds(&parse(&json!({"dns_mode":"auto"})).unwrap()));
    }

    #[test]
    fn the_status_says_what_resolves_names() {
        let manual = Got { dns: parse(&json!({"dns_mode":"manual","prefer_dns_manual":"1.1.1.1","standby_dns_manual":"1.0.0.1"})), ..Default::default() };
        let v = view(&manual, Lang::En);
        assert_eq!((v.state.as_str(), v.reason.as_deref()), ("Manual DNS", Some("1.1.1.1 · 1.0.0.1")));
        let auto = Got { dns: parse(&json!({"dns_mode":"auto","prefer_dns_auto":"168.95.1.1","standby_dns_auto":"168.95.192.1"})), ..Default::default() };
        let v = view(&auto, Lang::En);
        assert_eq!(v.reason.as_deref(), Some("168.95.1.1 · 168.95.192.1"));
        assert_eq!(v.operator.as_deref(), Some("168.95.1.1 · 168.95.192.1"));
        let down = Got { err: Some(Fail::Unreachable), ..Default::default() };
        let v = view(&down, Lang::En);
        assert_eq!((v.tone, v.state.as_str()), ("bad", "Can't read DNS settings"));
        assert!(v.retry);
        let stale = Got { dns: auto.dns.clone(), err: Some(Fail::Unreachable), at: None };
        assert_eq!(view(&stale, Lang::En).tone, "stale");
    }

    #[test]
    fn remote_gets_the_dialog_and_the_english_page_is_english() {
        let p = check(&d("manual", "1.1.1.1", ""), Lang::En).unwrap();
        assert_eq!(confirm(&p, Via::Local, Lang::En).tier, write::Tier::Inline);
        let c = confirm(&p, Via::Tailnet, Lang::En);
        assert!(c.tier == write::Tier::Dialog && c.cuts_uplink);
        let cjk = |s: &str| s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c) || ('\u{3000}'..='\u{303f}').contains(&c));
        for g in [Got::default(), Got { err: Some(Fail::Unreachable), ..Default::default() }] {
            let v = view(&g, Lang::En);
            assert!(!cjk(&Leaves { v: &v }.render().unwrap()));
        }
        assert!(!cjk(&c.what) && !cjk(&c.recovery.unwrap()));
    }
}
