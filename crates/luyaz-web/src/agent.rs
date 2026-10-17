//! The agent's `/api/*` calls of the pages (u60-web.md 8c-A: what u60d had no block or `/control`
//! action for), called by luyaz-web on the server side for a signed-in tab. The pages render the
//! answers and push them like the u60d ones.
//!
//! 8d-1b: u60d's compat layer serves these paths itself, at the agent's path and reply shape
//! ([`ON_U60D`]), so they go to u60d's `/v2/api/*` (loopback, no token). 8d-2: the agent is retired
//! and a session has no agent token (auth.rs), so that is the only way: u60d answering "not mine"
//! (a route it does not have, a part it does not own) or a path not on u60d is a refusal
//! ([`not_here`]), never a call to the agent (Phase 10 X4 removed the development fallback that
//! still sent them to an agent when there was no `--password-file`).
//!
//! The caller's address goes along (u60d decides some things by who asks, e.g. `/api/diagnose`
//! picks the Wi-Fi rules for the asking client): `X-U60-Client`, trusted only from 127.0.0.1.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use axum::extract::{ConnectInfo, Request};
use serde_json::Value;

use crate::i18n::Lang;
use crate::pages::Shared;

/// Why a call has no answer.
#[derive(Debug, Clone, PartialEq)]
pub enum Fail {
    /// No session, or it ran out.
    SignedOut,
    /// u60d did not answer.
    Unreachable,
    /// The agent answered, with an error (status, its message, its English message if any).
    Refused(u16, String, String),
}

impl Fail {
    pub fn text(&self, l: Lang) -> String {
        match self {
            Fail::SignedOut => l
                .pick("Signed out — log in again", "已退出登录，请重新登录")
                .into(),
            Fail::Unreachable => l
                .pick("The device service did not answer", "设备服务没有回应")
                .into(),
            Fail::Refused(_, _, en) if l == Lang::En && !en.is_empty() => en.clone(),
            Fail::Refused(code, m, _) if m.is_empty() => format!("HTTP {code}"),
            Fail::Refused(_, m, _) => m.clone(),
        }
    }
}

/// Who is asking: the session (None = signed out), its agent token (None = the usual case since
/// 8d-2; Some only for a development login through the agent) and the browser's address.
#[derive(Clone)]
pub struct Caller {
    pub token: Option<Option<String>>,
    pub peer: Option<IpAddr>,
    /// The session, to read its agent token fresh.
    pub sid: Option<String>,
}

impl Caller {
    pub fn of(app: &Shared, req: &Request) -> Caller {
        let s = app.sessions.of(req.headers());
        Caller {
            sid: s.as_ref().map(|(id, _)| id.clone()),
            token: s.map(|(_, t)| t),
            peer: req
                .extensions()
                .get::<ConnectInfo<SocketAddr>>()
                .map(|c| c.0.ip()),
        }
    }
}

/// Paths u60d's compat layer serves with the agent's reply (exact, or a prefix ending in `/`).
const ON_U60D: &[&str] = &[
    // component versions, installed and newest (u60d versions.rs; /v2/versions, not under /api)
    "/versions",
    "/versions/check",
    "/api/netinfo",
    "/api/netinfo/scan",
    // 8d-2: "use for this SIM" on /router/apn (compat ops::apn_use)
    "/api/netinfo/apn",
    "/api/agent/roam-cheap",
    "/api/modem/register",
    "/api/modem/netselect/auto",
    "/api/speedtest/",
    "/api/diagnose",
    "/api/diagnose/",
    "/api/cpu",
    "/api/device",
    "/api/modem/status",
    "/api/usb/status",
    "/api/sms/capacity",
    "/api/router/wan-ipv6",
    "/api/network/clients",
    "/api/network/qos",
    "/api/wifi/status",
    "/api/wifi/power-save",
    "/api/cell/signal-detect/",
    "/api/alerts/read",
    // 5a: u60d serves these (alerts and the health check always; SMS forwarding, the scheduler and
    // eSIM once handed over, else "not mine" sends them to the agent)
    "/api/alerts",
    "/api/alerts/sms",
    "/api/health",
    "/api/health/crashlog",
    "/api/sms/forward/",
    "/api/scheduler/",
    "/api/esim/",
    // 5b: u60d serves these (public status and the battery estimate always; CHILL and the
    // scenarios once handed over, else "not mine" sends them to the agent; Tailscale and the
    // CHILL extras once u60d has them)
    "/api/public/status",
    "/api/screen",
    "/api/services/tailscale",
    "/api/services/tailscale/log",
    "/api/services/chill",
    "/api/services/chill/",
    "/api/scenario",
    "/api/scenario/",
    // 5c: u60d serves the stateless reads always; the writes and the STK/USSD session once the
    // agent hands over `device_admin` (else "not mine" sends them to the agent)
    "/api/device/fast-boot",
    "/api/device/power-save",
    "/api/device/poweroff",
    // 8d-2: u60d serves the reboot (compat.rs, writes on); nothing behind luyaz-web any more
    "/api/device/reboot",
    "/api/device/factory-reset",
    "/api/sim/",
    "/api/cell/stc/",
    "/api/stk/",
    "/api/ussd/",
    "/api/at/",
    "/api/system/",
    "/api/router/dns",
    "/api/usb/mode",
    // 8d-2: the charge control and the battery estimate (device_power.rs)
    "/api/device/charge-control",
    "/api/battery",
];

const ESIM: &str = "/api/esim/";
const ESIM_TIMEOUT: Duration = Duration::from_secs(60);
/// 5c paths that wait on the modem or the network as long as eSIM does.
const SLOW: &[&str] = &["/api/at/", "/api/sim/", "/api/stk/", "/api/ussd/"];

fn on_u60d(path: &str) -> bool {
    let p = path.split('?').next().unwrap_or(path);
    ON_U60D
        .iter()
        .any(|&x| p == x || (x.ends_with('/') && p.starts_with(x)))
}

/// The answer for a call nobody here serves: u60d said "not mine", or the path is not on u60d.
fn not_here() -> Fail {
    Fail::Refused(
        404,
        "设备服务还不支持这项".into(),
        "The device service does not offer this yet".into(),
    )
}

/// u60d said the path is not its own (yet).
fn not_mine(code: u16, v: &Value) -> bool {
    (matches!(code, 404 | 501) && v["error"] == "not_in_compat_yet")
        || (code == 409 && v["not_owner"] == true)
}

/// One call: u60d for [`ON_U60D`] paths; the rest, and whatever u60d does not take, is
/// [`not_here`].
async fn send(
    app: &Shared,
    who: &Caller,
    method: reqwest::Method,
    path: &str,
    body: Option<&Value>,
    timeout: Option<Duration>,
) -> Result<(u16, Value), Fail> {
    // eSIM calls run lpac on the card first (tens of seconds): the agent's own forward gives them
    // 60 s, so do the same whoever answers. The modem's slow writes (AT terminal up to ~47 s, a
    // PIN check up to ~30 s, USSD/STK waiting for the network) get 60 s too: a write that times
    // out here and is tried again can cost a second PIN attempt.
    let timeout = if path.starts_with(ESIM) || SLOW.iter().any(|x| path.starts_with(x)) {
        Some(ESIM_TIMEOUT)
    } else {
        timeout
    };
    if who.token.is_none() {
        return Err(Fail::SignedOut);
    }
    // the session as it is now (it may have been logged out since the page opened)
    if let Some(sid) = &who.sid {
        app.sessions.agent_token(sid).ok_or(Fail::SignedOut)?;
    }
    if !on_u60d(path) {
        return Err(not_here());
    }
    let mut rb = app
        .http
        .request(method, format!("{}/v2{path}", app.u60d_url))
        // whose write it is (u60d's journal; without it u60d records the touch screen's `screen`)
        .header(luyaz_api::headers::SOURCE_HEADER, "web");
    if let Some(t) = timeout {
        rb = rb.timeout(t);
    }
    if let Some(b) = body {
        rb = rb.json(b);
    }
    if let Some(ip) = who.peer {
        rb = rb.header(luyaz_api::headers::CLIENT_HEADER, ip.to_string());
    }
    let r = rb.send().await.map_err(|_| Fail::Unreachable)?;
    let code = r.status().as_u16();
    let v: Value = r.json().await.unwrap_or(Value::Null);
    if not_mine(code, &v) {
        return Err(not_here());
    }
    Ok((code, v))
}

fn data(code: u16, v: Value) -> Result<Value, Fail> {
    if (200..300).contains(&code) && v["ok"].as_bool() != Some(false) {
        Ok(v.get("data").cloned().unwrap_or(v))
    } else {
        Err(refused(code, &v))
    }
}

/// An error reply as a [`Fail`]: `error` (or `error.message`) and `error_en`.
pub fn refused(code: u16, v: &Value) -> Fail {
    Fail::Refused(
        code,
        v["error"]
            .as_str()
            .or(v["error"]["message"].as_str())
            .unwrap_or("")
            .to_owned(),
        v["error_en"].as_str().unwrap_or("").to_owned(),
    )
}

/// GET `path` (with query) on the agent; `data` of `{ok, data}`.
pub async fn get(app: &Shared, who: &Caller, path: &str) -> Result<Value, Fail> {
    let (code, v) = send(app, who, reqwest::Method::GET, path, None, None).await?;
    data(code, v)
}

/// A GET for `/agent-api` (agent_api.rs, u60-mcp.md): no browser session — the caller checked
/// the token and the source itself — but the same client and the same [`ON_U60D`] list.
/// The status and the whole reply.
pub async fn get_internal(app: &Shared, path: &str) -> Result<(u16, Value), Fail> {
    if !on_u60d(path) {
        return Err(not_here());
    }
    let r = app
        .http
        .get(format!("{}/v2{path}", app.u60d_url))
        .send()
        .await
        .map_err(|_| Fail::Unreachable)?;
    let code = r.status().as_u16();
    Ok((code, r.json().await.unwrap_or(Value::Null)))
}

/// [`get`] with its own timeout, for the slow reads (`/api/network/qos` asks the modem's AT port
/// and takes about 12 s on the device; the shared client gives up after 10).
pub async fn get_slow(
    app: &Shared,
    who: &Caller,
    path: &str,
    timeout: Duration,
) -> Result<Value, Fail> {
    let (code, v) = send(app, who, reqwest::Method::GET, path, None, Some(timeout)).await?;
    data(code, v)
}

/// POST a JSON body; the status and the whole reply, for callers that read 409s and the like.
pub async fn post_raw(
    app: &Shared,
    who: &Caller,
    path: &str,
    body: &Value,
) -> Result<(u16, Value), Fail> {
    send(
        app,
        who,
        reqwest::Method::POST,
        path,
        Some(body),
        Some(Duration::from_secs(20)),
    )
    .await
}

/// PUT a JSON body; the status and the whole reply (the alert-SMS settings are a PUT).
pub async fn put_raw(
    app: &Shared,
    who: &Caller,
    path: &str,
    body: &Value,
) -> Result<(u16, Value), Fail> {
    send(
        app,
        who,
        reqwest::Method::PUT,
        path,
        Some(body),
        Some(Duration::from_secs(20)),
    )
    .await
}

pub async fn post(app: &Shared, who: &Caller, path: &str, body: &Value) -> Result<Value, Fail> {
    let (code, v) = post_raw(app, who, path, body).await?;
    data(code, v)
}

/// Any method with a JSON body (PUT / DELETE of the agent's list routes, e.g. `/api/scheduler/jobs`);
/// `data` of `{ok, data}`, an error reply as [`Fail::Refused`]. Sent once, like [`post`].
pub async fn send_json(
    app: &Shared,
    who: &Caller,
    method: reqwest::Method,
    path: &str,
    body: &Value,
) -> Result<Value, Fail> {
    let (code, v) = send(app, who, method, path, Some(body), Some(Duration::from_secs(20))).await?;
    data(code, v)
}

/// Text into HTML (for the few one-line answers built without a template).
pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// "hh:mm" of a device-clock moment, as the device shows it. luyaz-web runs on the device, whose
/// clock is local wall time labelled UTC (CLAUDE.md), so the UTC digits are the local ones.
pub fn hhmm(at: std::time::SystemTime) -> String {
    let s = at
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{:02}:{:02}", s / 3600 % 24, s / 60 % 60)
}

/// The freshness line of a list that stopped: "列表停在 14:03".
pub fn stopped(l: Lang, what_en: &str, what_zh: &str, at: Option<std::time::SystemTime>) -> String {
    match (at, l) {
        (None, _) => l.pick("No data yet", "尚未取得数据").into(),
        (Some(t), Lang::En) => format!("{what_en} stopped at {}", hhmm(t)),
        (Some(t), Lang::Zh) => format!("{what_zh}停在 {}", hhmm(t)),
    }
}

/// Bytes/s → Mbit/s like the old `mbps()` (2 digits under 10, 1 under 100).
pub fn mbps(bps: Option<f64>) -> Option<String> {
    let v = bps.filter(|b| b.is_finite() && *b >= 0.0)? * 8.0 / 1e6;
    Some(if v >= 100.0 {
        format!("{v:.0}")
    } else if v >= 10.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    })
}

/// Bytes → "1.23 GB" like the old `bytes()`.
pub fn bytes(n: Option<f64>) -> Option<String> {
    let mut x = n.filter(|b| b.is_finite() && *b >= 0.0)?;
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut i = 0;
    while x >= 1000.0 && i < units.len() - 1 {
        x /= 1000.0;
        i += 1;
    }
    let digits = if i == 0 {
        0
    } else if x >= 100.0 {
        0
    } else if x >= 10.0 {
        1
    } else {
        2
    };
    Some(format!("{x:.digits$} {}", units[i]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_paths_go_to_u60d() {
        for p in [
            "/api/netinfo?lite=1&apn=1",
            "/api/speedtest/progress",
            "/api/diagnose",
            "/api/diagnose/feedback",
            "/api/cell/signal-detect/start",
            "/api/device/reboot",
            "/api/wifi/power-save",
            "/api/alerts",
            "/api/health?refresh=1",
            "/api/esim/profiles",
            "/api/sms/forward/rules/toggle",
            "/api/device/charge-control",
            "/api/battery",
            "/api/netinfo/apn",
        ] {
            assert!(on_u60d(p), "{p}");
        }
        for p in [
            "/api/speedtest",
            "/api/netinfo/apnx",
            "/api/wifi/apply",
            "/api/alertsx",
            "/api/esim",
            "/api/cpux",
            "/api/batteryx",
            "/api/device/charge-controlx",
        ] {
            assert!(!on_u60d(p), "{p}");
        }
        assert!(not_mine(
            501,
            &serde_json::json!({"ok":false,"error":"not_in_compat_yet"})
        ));
        assert!(not_mine(
            404,
            &serde_json::json!({"ok":false,"error":"not_in_compat_yet","path":"/api/x"})
        ));
        assert!(not_mine(
            409,
            &serde_json::json!({"ok":false,"error":"这项还在 agent 那边","not_owner":true})
        ));
        assert!(
            !not_mine(409, &serde_json::json!({"ok":false,"error":"正在诊断"})),
            "a real refusal is u60d's answer"
        );
        assert!(!not_mine(
            404,
            &serde_json::json!({"ok":false,"error":"no such job"})
        ));
    }

    #[test]
    fn a_refusal_reads_in_the_page_language() {
        let f = refused(
            409,
            &serde_json::json!({"ok":false,"error":"正在诊断，约 5 秒后再试","error_en":"Diagnosing; try again in about 5 s"}),
        );
        assert_eq!(f.text(Lang::En), "Diagnosing; try again in about 5 s");
        assert_eq!(f.text(Lang::Zh), "正在诊断，约 5 秒后再试");
        assert_eq!(
            refused(500, &serde_json::json!({})).text(Lang::En),
            "HTTP 500"
        );
    }

    #[test]
    fn number_formats_match_the_old_web() {
        assert_eq!(mbps(Some(1_450_000.0)).as_deref(), Some("11.6"));
        assert_eq!(mbps(Some(37_500.0)).as_deref(), Some("0.30"));
        assert_eq!(bytes(Some(1_940_000_000.0)).as_deref(), Some("1.94 GB"));
        assert_eq!(bytes(Some(999.0)).as_deref(), Some("999 B"));
        assert_eq!(
            hhmm(std::time::UNIX_EPOCH + Duration::from_secs(3600 * 26 + 60 * 7)),
            "02:07"
        );
    }
}

// ---- the per-tab stream of an agent-backed page ----

/// A page whose live parts come from polling the agent: one `step` = fetch + render.
pub trait Polled: Send + 'static {
    /// Fetch what the page shows and render its leaves (`(id, html)`, each one element); also how
    /// long until the next fetch.
    fn step(
        &mut self,
        app: &Shared,
    ) -> impl std::future::Future<Output = (Vec<(String, String)>, Duration)> + Send;

    /// The page also shows this u60d block: its changes re-render the page (without a fetch).
    fn watches(&self, _block: &str) -> bool {
        false
    }

    /// The page's leaves from what it already has and the hub as it is now; `None` = the page
    /// shows nothing from the hub (only the agent's answers, re-rendered by `step`).
    fn render(&mut self, _app: &Shared) -> Option<Vec<(String, String)>> {
        None
    }
}

/// One tab's stream: the page's leaves (first one sent whole, then only what changed) and the
/// frame. Re-polls early when a button on this page kicks it (`App::kick`); a page that also
/// shows u60d blocks ([`Polled::watches`]) re-renders when they change, between its polls.
pub fn stream<P: Polled>(
    app: Shared,
    ctx: crate::topbar::Ctx,
    g: u64,
    path: &'static str,
    mut page: P,
) -> axum::response::sse::Sse<
    impl futures_util::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>,
> {
    use crate::hub::Change;
    use tokio::sync::broadcast::error::RecvError;
    let mut sub = app.hub.subscribe();
    let mut kicks = app.kick.subscribe();
    let s = crate::pages::async_stream(move |tx| async move {
        let mut seen = crate::opbar::Seen::default();
        let (mut frame, gen_now) = app
            .hub
            .read(|st| (crate::topbar::leaves(st, &ctx, &mut seen), st.generation));
        let (mut sent, delay) = page.step(&app).await;
        // a deadline, not a sleep per loop: frame pushes come more often than the page's poll
        let mut next = tokio::time::Instant::now() + delay;
        let mut out: Vec<String> = sent.iter().map(|(_, h)| h.clone()).collect();
        if gen_now != g {
            out.extend(crate::topbar::diff(&[], &frame));
        }
        if !crate::pages::send_out(&tx, out).await {
            return;
        }
        loop {
            let at = app
                .hub
                .read(|st| crate::opbar::wake(st, &seen, std::time::Instant::now()));
            let (poll, hub) = tokio::select! {
                _ = tokio::time::sleep_until(next) => (true, false),
                k = kicks.recv() => match k {
                    Ok(p) if p == path => (true, false),
                    Ok(_) => continue,
                    Err(RecvError::Lagged(_)) => (true, false),
                    Err(RecvError::Closed) => return,
                },
                r = sub.rx.recv() => match r {
                    Ok(Change::Block(name)) if page.watches(&name) => (false, true),
                    Ok(Change::Block(name)) if !crate::topbar::reads(&name) => continue,
                    Ok(Change::Block(_)) => (false, false),
                    Ok(_) | Err(RecvError::Lagged(_)) => (false, true),
                    Err(RecvError::Closed) => return,
                },
                _ = crate::topbar::wake(at) => (false, false),
                _ = tx.closed() => return,
            };
            let mut out = Vec::new();
            if poll {
                let (now, d) = page.step(&app).await;
                next = tokio::time::Instant::now() + d;
                out = crate::topbar::diff(&sent, &now);
                sent = now;
            } else if hub && let Some(now) = page.render(&app) {
                out = crate::topbar::diff(&sent, &now);
                sent = now;
            }
            let frame_now = app
                .hub
                .read(|st| crate::topbar::leaves(st, &ctx, &mut seen));
            out.extend(crate::topbar::diff(&frame, &frame_now));
            frame = frame_now;
            if !crate::pages::send_out(&tx, out).await {
                return;
            }
        }
    });
    axum::response::sse::Sse::new(s)
        .keep_alive(axum::response::sse::KeepAlive::new().interval(Duration::from_secs(15)))
}
