//! /router/telemetry (遥测拦截), the old `router/telemetry/page.tsx`: the domain block list, the
//! quick block of the four known ZTE telemetry domains, add, remove.
//!
//! Where each value comes from (the old page read `/api/router/domain-filter`, the agent's
//! pass-through of `router_get_domainfilter_rule`):
//! - u60d block `router.blocked_domains` (the same ubus read; u60d takes `blocked_domains[]` or
//!   `rule_list[].domain`, and B31's `{}` as an empty list). `null` = the read failed or the reply
//!   had neither: shown as "not readable" with every change locked, never as "nothing blocked".
//! - The four known domains, the "n of 4" status and the per-row 「已知 ZTE 遥测」 tag: written into
//!   the page, as before.
//!
//! Writes (web/docs/controls-inventory.md §/router/telemetry): `vendor.call` `zwrt_router.api
//! router_set_domain_filter {action: "add"|"delete", domain}` (the args the agent passed through
//! as they came), tier 2 on the LAN and tier 3 over Tailscale (treated like firewall rules), read
//! back from `router.blocked_domains` (u60d re-reads after any write).
//! - Quick block: one send per known domain not blocked yet, one after another, each once (the
//!   old page swallowed a failed one and still said "done"). The result names what was blocked
//!   and what wasn't; pressing it again sends only the ones still missing.
//! - Add refuses, before sending, a domain that would block the name the browser used to reach
//!   this panel (the request's Host): the host contains the rule text (the user's conservative
//!   choice, 10-13: covers exact, suffix and keyword matching by the firmware). An IP address
//!   host can't be blocked by a domain rule. Same check at the ask, at the confirm, and for the
//!   quick block.

use std::convert::Infallible;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use askama::Template;
use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, header};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;

use crate::agent::Polled;
use crate::hub::{Hub, Link, St};
use crate::i18n::Lang;
use crate::pages::{Shared, StreamQuery, split_leaves};
use crate::write::{self, Outcome, Phase};

pub const PATH: &str = "/router/telemetry";
pub const KNOWN: [&str; 4] = ["iot.zte.com.cn", "ztems.com", "zte.com.cn", "ztemt.com.cn"];

/// A quick block is sending (one device, one page: no second one alongside it).
static QUICK_BUSY: AtomicBool = AtomicBool::new(false);

/// This page's routes (the page, its stream, its buttons).
pub fn routes() -> Router<Shared> {
    Router::new()
        .route(PATH, get(page))
        .route("/router/telemetry/events", get(events))
        .route("/router/telemetry/refresh", post(refresh))
        .route("/router/telemetry/quick/ask", post(quick_ask))
        .route("/router/telemetry/quick/set", post(quick_set))
        .route("/router/telemetry/quick/cancel", post(quick_cancel))
        .route("/router/telemetry/add/ask", post(add_ask))
        .route("/router/telemetry/add/set", post(add_set))
        .route("/router/telemetry/add/cancel", post(add_cancel))
        .route("/router/telemetry/del/ask", post(del_ask))
        .route("/router/telemetry/del/set", post(del_set))
        .route("/router/telemetry/del/cancel", post(del_cancel))
}

// ---- what u60d says ----

#[derive(Debug, PartialEq)]
pub enum List {
    /// The hub is still connecting.
    Loading,
    /// No `router` block: u60d is down (true) or has none (false).
    Missing(bool),
    /// `blocked_domains` null: the firmware's reply was not read.
    Unreadable {
        stale: bool,
    },
    Read {
        domains: Vec<String>,
        stale: bool,
    },
}

pub fn list(st: &St) -> List {
    let Some(b) = st.blocks.get("router") else {
        return match st.link {
            Link::Live => List::Missing(false),
            Link::Down => List::Missing(true),
            _ => List::Loading,
        };
    };
    let stale = b.stale || st.link != Link::Live;
    match b.data.get("blocked_domains").and_then(Value::as_array) {
        None => List::Unreadable { stale },
        Some(a) => List::Read {
            domains: a
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            stale,
        },
    }
}

/// The list when it can be changed now (read and current).
pub fn current(st: &St) -> Option<Vec<String>> {
    match list(st) {
        List::Read {
            domains,
            stale: false,
        } => Some(domains),
        _ => None,
    }
}

/// `domain` is on the list (read back).
fn listed(st: &St, domain: &str) -> Option<bool> {
    match list(st) {
        List::Read { domains, .. } => Some(domains.iter().any(|d| d == domain)),
        _ => None,
    }
}

// ---- input, host ----

#[derive(Debug, PartialEq)]
pub enum Bad {
    Empty,
    Shape,
}

/// A typed domain, lowercased, without spaces around or a final dot; letters, digits, `-`, `_`,
/// at least two labels (the old page's "looks like example.com").
pub fn domain(raw: &str) -> Result<String, Bad> {
    let d = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    if d.is_empty() {
        return Err(Bad::Empty);
    }
    let ok_label = |s: &str| {
        !s.is_empty()
            && s.len() <= 63
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    };
    if d.len() > 253 || !d.contains('.') || !d.split('.').all(ok_label) {
        return Err(Bad::Shape);
    }
    Ok(d)
}

/// The host the browser used (the Host header): no port, no brackets, no final dot, lowercase.
pub fn host_of(h: &HeaderMap) -> Option<String> {
    let raw = h.get(header::HOST)?.to_str().ok()?.trim();
    let host = if let Some(rest) = raw.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        match raw.rsplit_once(':') {
            Some((a, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => a,
            _ => raw,
        }
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

/// Blocking `rule` could cut off `host` (the user's rule, 10-13: conservative, since how the
/// firmware matches is not known): the host contains the rule text, which covers an exact,
/// suffix or keyword match. An IP address host can't be blocked by a domain rule (only one
/// spelled exactly like it is refused).
pub fn cuts_panel(rule: &str, host: &str) -> bool {
    if rule == host {
        return true;
    }
    host.split('%')
        .next()
        .unwrap_or(host)
        .parse::<IpAddr>()
        .is_err()
        && host.contains(rule)
}

/// Percent-encoded for a query value inside `@post('…')` (device text never reaches the JS raw).
pub fn enc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

// ---- the view ----

pub struct Row {
    pub d: String,
    pub enc: String,
    pub known: bool,
}

pub struct View {
    pub l: Lang,
    pub tone: &'static str,
    pub state: String,
    pub reason: Option<String>,
    pub meta: Option<&'static str>,
    pub retry: bool,
    /// Nothing can be changed (not read, not current, a quick block sending).
    pub dis: bool,
    pub quick_dis: bool,
    pub quick_busy: bool,
    pub all_blocked: bool,
    pub rows: Vec<Row>,
    /// Instead of rows.
    pub note: Option<String>,
    pub skel: bool,
    pub stale: bool,
}

impl View {
    pub fn symbol(&self) -> &'static str {
        match self.tone {
            "bad" => "■",
            "warn" => "▲",
            _ => "●",
        }
    }
    pub fn trash(&self) -> String {
        crate::shell::icon("Trash", false, 20, "")
    }
}

fn no_read(l: Lang) -> String {
    l.pick(
        "The device answered, but without a list this page recognises. Changes are disabled until it can be read.",
        "设备有回应，但里面没有这个页面认得的列表。读出来之前不能修改。",
    )
    .into()
}

pub fn view(st: &St, l: Lang) -> View {
    let busy = QUICK_BUSY.load(Ordering::Relaxed);
    let lst = list(st);
    let (mut tone, state, reason, retry) = match &lst {
        List::Loading => (
            "neutral",
            l.pick("Reading the block list…", "正在读取拦截列表…")
                .to_string(),
            None,
            false,
        ),
        List::Missing(down) => (
            "bad",
            l.pick("Can't read the block list", "读不到拦截列表")
                .to_string(),
            Some(
                if *down {
                    l.pick("can't reach the data service", "连不上数据服务")
                } else {
                    l.pick(
                        "the data service has no router data",
                        "数据服务没有路由器数据",
                    )
                }
                .to_string(),
            ),
            true,
        ),
        List::Unreadable { .. } => (
            "warn",
            l.pick("Block list not readable", "拦截列表读不出来")
                .to_string(),
            Some(no_read(l)),
            true,
        ),
        List::Read { domains, .. } => {
            let missing: Vec<&str> = KNOWN
                .iter()
                .copied()
                .filter(|k| !domains.iter().any(|d| d == k))
                .collect();
            let n = KNOWN.len() - missing.len();
            if missing.is_empty() {
                (
                    "ok",
                    l.pick("ZTE telemetry blocked", "ZTE 遥测已拦截")
                        .to_string(),
                    Some(match l {
                        Lang::En => {
                            format!("All {} known domains are on the block list.", KNOWN.len())
                        }
                        Lang::Zh => format!("已知的 {} 个域名都在拦截列表里。", KNOWN.len()),
                    }),
                    false,
                )
            } else {
                (
                    "warn",
                    match l {
                        Lang::En => {
                            format!("{n} of {} known telemetry domains blocked", KNOWN.len())
                        }
                        Lang::Zh => format!("已知遥测域名拦截了 {n} / {} 个", KNOWN.len()),
                    },
                    Some(match l {
                        Lang::En => format!("Not blocked: {}", missing.join(", ")),
                        Lang::Zh => format!("没拦截：{}", missing.join("、")),
                    }),
                    false,
                )
            }
        }
    };
    let stale = matches!(
        lst,
        List::Read { stale: true, .. } | List::Unreadable { stale: true }
    );
    let mut meta = None;
    let mut retry = retry;
    if stale {
        tone = "stale";
        retry = true;
        meta = Some(l.pick(
            "Not current: refresh before changing anything.",
            "不是最新的：刷新后再改。",
        ));
    }
    let readable = matches!(lst, List::Read { stale: false, .. });
    let dis = !readable || busy;
    let (rows, note, skel, all_blocked) = match &lst {
        List::Loading => (Vec::new(), None, true, false),
        List::Missing(_) | List::Unreadable { .. } => (
            Vec::new(),
            Some(
                l.pick(
                    "The list can't be shown: the device's reply has no domain list.",
                    "列表显示不了：设备的回复里没有域名列表。",
                )
                .to_string(),
            ),
            false,
            false,
        ),
        List::Read { domains, .. } => {
            let all = KNOWN.iter().all(|k| domains.iter().any(|d| d == k));
            let note = domains.is_empty().then(|| {
                l.pick(
                    "No domains blocked. Use “Block Known Telemetry” or add one above.",
                    "还没有拦截任何域名。可以用「拦截已知遥测」，或在上面添加。",
                )
                .to_string()
            });
            (
                domains
                    .iter()
                    .map(|d| Row {
                        d: d.clone(),
                        enc: enc(d),
                        known: KNOWN.contains(&d.as_str()),
                    })
                    .collect(),
                note,
                false,
                all,
            )
        }
    };
    View {
        l,
        tone,
        state,
        reason,
        meta,
        retry,
        dis,
        quick_dis: dis || all_blocked,
        quick_busy: busy,
        all_blocked,
        rows,
        note,
        skel,
        stale,
    }
}

#[derive(Template)]
#[template(path = "router_filter_leaves.html")]
struct Leaves<'a> {
    v: &'a View,
}

fn leaves(app: &Shared, l: Lang) -> Vec<(String, String)> {
    app.hub
        .read(|st| split_leaves(&Leaves { v: &view(st, l) }.render().unwrap()))
}

fn leaf(app: &Shared, l: Lang, id: &str) -> String {
    leaves(app, l)
        .into_iter()
        .find(|(k, _)| k == id)
        .map_or_else(String::new, |(_, h)| h)
}

// ---- confirms, gates, writes ----

fn confirm(remote: bool, title: String, what: String, action: String, l: Lang) -> write::Confirm {
    write::Confirm {
        tier: if remote {
            write::Tier::Dialog
        } else {
            write::Tier::Inline
        },
        title,
        what,
        downtime: Some(
            l.pick(
                "None; the filter reloads in a moment.",
                "不断网；过滤规则马上重新加载。",
            )
            .into(),
        ),
        recovery: Some(
            l.pick(
                "Undo it on this page. If this page stops loading over Tailscale, someone on the U60's Wi-Fi can change it.",
                "在这个页面改回来。如果经 Tailscale 打不开这个页面了，请连着 U60 Wi-Fi 的人来改。",
            )
            .into(),
        ),
        action,
        cuts_uplink: true,
        word: None,
        danger: false,
    }
}

fn not_read(l: Lang) -> Outcome {
    Outcome::NotYet(
        l.pick(
            "The block list isn't read (or isn't current) yet; try again shortly",
            "还没读到最新的拦截列表，稍后再试",
        )
        .into(),
    )
}

fn host_refusal(rule: &str, host: &str, l: Lang) -> Outcome {
    let text = match l {
        Lang::En => format!(
            "Not sent: blocking {rule} would also block {host}, the address this browser uses to reach this panel. Open the panel by its IP address to block it."
        ),
        Lang::Zh => format!(
            "没有发送：拦截 {rule} 也会拦住 {host}，这个浏览器正是用它打开管理页的。要拦它，先用 IP 地址打开管理页。"
        ),
    };
    Outcome::Ended(text, "bad")
}

/// The missing known domains when a quick block can be sent now.
pub fn quick_gate(st: &St, host: Option<&str>, l: Lang) -> Result<Vec<&'static str>, Outcome> {
    if QUICK_BUSY.load(Ordering::Relaxed) {
        return Err(Outcome::NotYet(
            l.pick("Already sending; wait for it", "正在发送，请等它结束")
                .into(),
        ));
    }
    let list = current(st).ok_or_else(|| not_read(l))?;
    let todo: Vec<&'static str> = KNOWN
        .iter()
        .copied()
        .filter(|k| !list.iter().any(|d| d == k))
        .collect();
    if let Some(h) = host
        && let Some(d) = todo.iter().find(|d| cuts_panel(d, h))
    {
        return Err(host_refusal(d, h, l));
    }
    Ok(todo)
}

/// The domain to add when it can be sent now.
pub fn add_gate(st: &St, raw: &str, host: Option<&str>, l: Lang) -> Result<String, Outcome> {
    let d = domain(raw).map_err(|b| {
        Outcome::NotYet(
            match b {
                Bad::Empty => l.pick("Enter a domain name", "请输入域名"),
                Bad::Shape => l.pick(
                    "That doesn't look like a domain name, e.g. example.com.",
                    "这不像域名，例如 example.com。",
                ),
            }
            .into(),
        )
    })?;
    let list = current(st).ok_or_else(|| not_read(l))?;
    if list.contains(&d) {
        return Err(Outcome::NotYet(match l {
            Lang::En => format!("{d} is already blocked."),
            Lang::Zh => format!("{d} 已经在拦截列表里。"),
        }));
    }
    if let Some(h) = host
        && cuts_panel(&d, h)
    {
        return Err(host_refusal(&d, h, l));
    }
    Ok(d)
}

/// The domain to remove when it can be sent now (it is on the list as the device has it).
pub fn del_gate(st: &St, d: &str, l: Lang) -> Result<String, Outcome> {
    let list = current(st).ok_or_else(|| not_read(l))?;
    if !list.iter().any(|x| x == d) {
        return Err(Outcome::NotYet(match l {
            Lang::En => format!("{d} is no longer on the list."),
            Lang::Zh => format!("{d} 已经不在列表里了。"),
        }));
    }
    Ok(d.to_owned())
}

/// The `/control` params of one rule change (the agent's pass-through body as args).
pub fn params(action: &str, domain: &str) -> Value {
    json!({"object": "zwrt_router.api", "method": "router_set_domain_filter",
           "args": {"action": action, "domain": domain}})
}

fn one_write(add: bool, d: &str, l: Lang) -> write::Write {
    let want = d.to_owned();
    write::Write {
        action: "vendor.call",
        params: params(if add { "add" } else { "delete" }, d),
        readback: Some(Box::new(move |st| listed(st, &want) == Some(add))),
        applied: match (add, l) {
            (true, Lang::En) => format!("Blocked {d}"),
            (true, Lang::Zh) => format!("已拦截 {d}"),
            (false, Lang::En) => format!("Unblocked {d}"),
            (false, Lang::Zh) => format!("已解除拦截 {d}"),
        },
        timeout: write::READBACK,
        typed: None,
        request_timeout: None,
    }
}

/// Wait until the hub shows `check`, or `timeout`.
async fn wait_for(hub: &Hub, timeout: Duration, check: impl Fn(&St) -> bool) -> bool {
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

fn list_words(v: &[&str], l: Lang) -> String {
    v.join(l.pick(", ", "、"))
}

/// The steps of a quick block: each domain sent once, one after another (a busy device stops
/// the rest), then the sent ones read back. Says what was blocked and what wasn't.
pub async fn quick_run(
    http: &reqwest::Client,
    control_url: &str,
    hub: &Hub,
    todo: &[&'static str],
    timeout: Duration,
    l: Lang,
) -> Outcome {
    let mut sent: Vec<&str> = Vec::new();
    let mut no_reply = false;
    let mut failed: Vec<(&str, String)> = Vec::new();
    for (i, d) in todo.iter().enumerate() {
        let w = one_write(true, d, l);
        match write::send(http, control_url, &w, l).await {
            Ok(s) => {
                no_reply |= s == write::Sent::NoReply;
                sent.push(d);
            }
            Err(Outcome::Busy) => {
                let why = l
                    .pick("another change is in progress", "另一项改动正在进行")
                    .to_string();
                for d in &todo[i..] {
                    failed.push((d, why.clone()));
                }
                break;
            }
            Err(Outcome::Failed(e)) => failed.push((d, e)),
            Err(o) => failed.push((d, o.text(l))),
        }
    }
    let all_in = |st: &St| sent.iter().all(|d| listed(st, d) == Some(true));
    let read = sent.is_empty() || wait_for(hub, timeout, all_in).await;
    let (shown, unseen): (Vec<&str>, Vec<&str>) =
        hub.read(|st| sent.iter().partition(|d| listed(st, d) == Some(true)));
    if failed.is_empty() && read {
        return Outcome::Applied(match l {
            Lang::En => format!("Blocked {} telemetry domain(s)", shown.len()),
            Lang::Zh => format!("已拦截 {} 个遥测域名", shown.len()),
        });
    }
    if failed.is_empty() {
        return if no_reply && shown.is_empty() {
            Outcome::Unknown
        } else if shown.is_empty() {
            Outcome::Accepted
        } else {
            Outcome::AcceptedSaid(match l {
                Lang::En => format!(
                    "Blocked: {}. Accepted by the device, not read back yet: {}",
                    list_words(&shown, l),
                    list_words(&unseen, l)
                ),
                Lang::Zh => format!(
                    "已拦截：{}。设备已接受，还没读回：{}",
                    list_words(&shown, l),
                    list_words(&unseen, l)
                ),
            })
        };
    }
    let mut parts = Vec::new();
    if !shown.is_empty() {
        parts.push(match l {
            Lang::En => format!("Blocked: {}.", list_words(&shown, l)),
            Lang::Zh => format!("已拦截：{}。", list_words(&shown, l)),
        });
    }
    if !unseen.is_empty() {
        parts.push(match l {
            Lang::En => format!("Sent, not read back yet: {}.", list_words(&unseen, l)),
            Lang::Zh => format!("已发送，还没读回：{}。", list_words(&unseen, l)),
        });
    }
    let fails: Vec<String> = failed
        .iter()
        .map(|(d, e)| match l {
            Lang::En => format!("{d} ({e})"),
            Lang::Zh => format!("{d}（{e}）"),
        })
        .collect();
    parts.push(match l {
        Lang::En => format!(
            "Not blocked: {}. Press “Block Known Telemetry” again to send only the rest.",
            fails.join(", ")
        ),
        Lang::Zh => format!(
            "没拦截：{}。再按「拦截已知遥测」只补发剩下的。",
            fails.join("、")
        ),
    });
    let tone = if shown.is_empty() && unseen.is_empty() {
        "bad"
    } else {
        "warn"
    };
    Outcome::Ended(parts.join(" "), tone)
}

// ---- the page and its stream ----

pub struct Poll {
    l: Lang,
}

impl Polled for Poll {
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        (leaves(app, self.l), Duration::from_secs(300))
    }
    fn watches(&self, block: &str) -> bool {
        block == "router"
    }
    fn render(&mut self, app: &Shared) -> Option<Vec<(String, String)>> {
        Some(leaves(app, self.l))
    }
}

#[derive(Template)]
#[template(path = "router_filter.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    leaves: Vec<(String, String)>,
    quick_op: String,
    add_op: String,
    del_op: String,
}

impl Page {
    fn leaf(&self, id: &str) -> &str {
        self.leaves
            .iter()
            .find(|(k, _)| k == id)
            .map_or("", |(_, h)| h.as_str())
    }
    fn known(&self) -> String {
        KNOWN.join(", ")
    }
}

fn area(id: &'static str, l: Lang, remote: bool, set: &str, phase: &Phase) -> String {
    let cancel = format!("{PATH}/{}/cancel", &id[3..]);
    write::Area {
        id,
        l,
        remote,
        set,
        cancel: &cancel,
        phase,
    }
    .html()
}

fn quick_area(l: Lang, remote: bool, phase: &Phase) -> String {
    area("tf-quick", l, remote, "/router/telemetry/quick/set", phase)
}

fn add_area(l: Lang, remote: bool, d: &str, phase: &Phase) -> String {
    let set = format!("{PATH}/add/set?d={}", enc(d));
    area("tf-add", l, remote, &set, phase)
}

fn del_area(l: Lang, remote: bool, d: &str, phase: &Phase) -> String {
    let set = format!("{PATH}/del/set?d={}", enc(d));
    area("tf-del", l, remote, &set, phase)
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
    let html = Page {
        shell,
        l,
        leaves: leaves(&app, l),
        quick_op: quick_area(l, ctx.remote, &Phase::Idle),
        add_op: add_area(l, ctx.remote, "", &Phase::Idle),
        del_op: del_area(l, ctx.remote, "", &Phase::Idle),
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

/// Retry: reconnect to u60d now.
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

#[derive(Deserialize, Default)]
pub struct DQuery {
    #[serde(default)]
    d: String,
}

// quick block

pub async fn quick_ask(
    State(app): State<Shared>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let (l, host) = (ctx.lang, host_of(req.headers()));
    let phase = match app.hub.read(|st| quick_gate(st, host.as_deref(), l)) {
        Err(o) => Phase::Done(o),
        Ok(todo) if todo.is_empty() => Phase::Idle,
        Ok(todo) => Phase::Ask(confirm(
            ctx.remote,
            l.pick(
                "Block the known ZTE telemetry domains?",
                "拦截已知的 ZTE 遥测域名？",
            )
            .into(),
            match l {
                Lang::En => format!(
                    "Devices behind the U60 can no longer reach {}.",
                    list_words(&todo, l)
                ),
                Lang::Zh => format!("U60 后面的设备将无法访问 {}。", list_words(&todo, l)),
            },
            match l {
                Lang::En => format!("Confirm: block {} telemetry domain(s)", todo.len()),
                Lang::Zh => format!("确认：拦截 {} 个遥测域名", todo.len()),
            },
            l,
        )),
    };
    one(quick_area(l, ctx.remote, &phase))
}

/// Clears the busy flag however the send ends (also when the tab goes away mid-way).
struct QuickGuard;
impl Drop for QuickGuard {
    fn drop(&mut self) {
        QUICK_BUSY.store(false, Ordering::Relaxed);
    }
}

pub async fn quick_set(
    State(app): State<Shared>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    let host = host_of(req.headers());
    let gate = app.hub.read(|st| quick_gate(st, host.as_deref(), l));
    let guard = if gate.as_ref().is_ok_and(|t| !t.is_empty()) {
        QUICK_BUSY
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .ok()
            .map(|_| QuickGuard)
    } else {
        None
    };
    let pending = format!(
        "{}\n{}",
        leaf(&app, l, "tf-quickbtn"),
        quick_area(
            l,
            false,
            &Phase::Pending(
                l.pick("Blocking the telemetry domains…", "正在拦截遥测域名…")
                    .into()
            )
        )
    );
    write::respond(pending, async move {
        let o = match gate {
            Err(o) => o,
            Ok(todo) if todo.is_empty() => Outcome::Applied(
                l.pick(
                    "All telemetry domains already blocked",
                    "已知遥测域名都已拦截",
                )
                .into(),
            ),
            Ok(_) if guard.is_none() => Outcome::NotYet(
                l.pick("Already sending; wait for it", "正在发送，请等它结束")
                    .into(),
            ),
            Ok(todo) => {
                let o = quick_run(
                    &app.http,
                    &app.control_url,
                    &app.hub,
                    &todo,
                    write::READBACK,
                    l,
                )
                .await;
                drop(guard);
                o
            }
        };
        let _ = app.kick.send(PATH);
        format!(
            "{}\n{}",
            leaf(&app, l, "tf-quickbtn"),
            quick_area(l, false, &Phase::Done(o))
        )
    })
}

pub async fn quick_cancel(
    Query(q): Query<write::CancelQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    one(quick_area(ctx.lang, ctx.remote, &write::cancel_phase(&q)))
}

// add

pub async fn add_ask(
    State(app): State<Shared>,
    Query(q): Query<DQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let (l, host) = (ctx.lang, host_of(req.headers()));
    let (d, phase) = match app.hub.read(|st| add_gate(st, &q.d, host.as_deref(), l)) {
        Err(o) => (String::new(), Phase::Done(o)),
        Ok(d) => {
            let c = confirm(
                ctx.remote,
                match l {
                    Lang::En => format!("Block {d}?"),
                    Lang::Zh => format!("拦截 {d}？"),
                },
                match l {
                    Lang::En => format!("Devices behind the U60 can no longer reach {d}."),
                    Lang::Zh => format!("U60 后面的设备将无法访问 {d}。"),
                },
                match l {
                    Lang::En => format!("Confirm: block {d}"),
                    Lang::Zh => format!("确认：拦截 {d}"),
                },
                l,
            );
            (d, Phase::Ask(c))
        }
    };
    one(add_area(l, ctx.remote, &d, &phase))
}

/// Empties the input after an add went through (a new value re-applies `data-signals`).
fn clear_input() -> String {
    format!(
        "<div id=\"tf-clr\" hidden data-signals=\"{{_tfdom: '', _tfn: {}}}\"></div>",
        write::next_op_id().rsplit('-').next().unwrap_or("0")
    )
}

pub async fn add_set(
    State(app): State<Shared>,
    Query(q): Query<DQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    let host = host_of(req.headers());
    let pending = add_area(
        l,
        false,
        &q.d,
        &Phase::Pending(match l {
            Lang::En => format!("Blocking {}…", q.d),
            Lang::Zh => format!("正在拦截 {}…", q.d),
        }),
    );
    write::respond(pending, async move {
        let o = match app.hub.read(|st| add_gate(st, &q.d, host.as_deref(), l)) {
            Err(o) => o,
            Ok(d) => {
                write::execute(
                    &app.http,
                    &app.control_url,
                    &app.hub,
                    &one_write(true, &d, l),
                    l,
                )
                .await
            }
        };
        let clr = if matches!(o, Outcome::Applied(_) | Outcome::Accepted) {
            clear_input()
        } else {
            String::new()
        };
        format!("{clr}\n{}", add_area(l, false, &q.d, &Phase::Done(o)))
    })
}

pub async fn add_cancel(
    Query(q): Query<write::CancelQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    one(add_area(ctx.lang, ctx.remote, "", &write::cancel_phase(&q)))
}

// remove

pub async fn del_ask(
    State(app): State<Shared>,
    Query(q): Query<DQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let (d, phase) = match app.hub.read(|st| del_gate(st, &q.d, l)) {
        Err(o) => (String::new(), Phase::Done(o)),
        Ok(d) => {
            let c = confirm(
                ctx.remote,
                match l {
                    Lang::En => format!("Remove “{d}”?"),
                    Lang::Zh => format!("移除「{d}」？"),
                },
                match l {
                    Lang::En => format!("Devices behind the U60 can reach {d} again."),
                    Lang::Zh => format!("U60 后面的设备又能访问 {d} 了。"),
                },
                match l {
                    Lang::En => format!("Confirm: remove {d}"),
                    Lang::Zh => format!("确认：移除 {d}"),
                },
                l,
            );
            (d, Phase::Ask(c))
        }
    };
    one(del_area(l, ctx.remote, &d, &phase))
}

pub async fn del_set(
    State(app): State<Shared>,
    Query(q): Query<DQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    let pending = del_area(
        l,
        false,
        &q.d,
        &Phase::Pending(match l {
            Lang::En => format!("Removing {}…", q.d),
            Lang::Zh => format!("正在移除 {}…", q.d),
        }),
    );
    write::respond(pending, async move {
        let o = match app.hub.read(|st| del_gate(st, &q.d, l)) {
            Err(o) => o,
            Ok(d) => {
                write::execute(
                    &app.http,
                    &app.control_url,
                    &app.hub,
                    &one_write(false, &d, l),
                    l,
                )
                .await
            }
        };
        del_area(l, false, &q.d, &Phase::Done(o))
    })
}

pub async fn del_cancel(
    Query(q): Query<write::CancelQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    one(del_area(ctx.lang, ctx.remote, "", &write::cancel_phase(&q)))
}

#[cfg(test)]
mod tests;
