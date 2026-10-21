//! The gate in front of every request (u60-web.md W14, 8d-2). Pages that are migrated (`--pages`,
//! read at start so a page can be switched off without a rebuild) are served here; since the agent
//! retired (8d-2) nothing is forwarded anywhere: any other page is a 404 page, any other `/api/*`
//! a JSON 404.
//!
//! ```text
//!  request ─▶ gate ─┬─ public (/a/*, /health, /login, /lang) ─▶ as is
//!                   ├─ migrated page or its endpoints ─▶ needs session (+ Origin on writes) ─▶ here
//!                   │     (no session: a Datastar stream/button gets the "session expired" patch,
//!                   │      a page load the /login redirect; an open stream ends with the patch)
//!                   ├─ /api/* ─▶ 404 {"ok":false,"error":"not_found"}
//!                   └─ anything else ─▶ 404 page (a link home)
//! ```

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use serde_json::json;

use crate::auth::same_origin;
use crate::pages::Shared;

/// Paths served here when their page is migrated: the page and what it calls.
fn own(path: &str, pages: &[String]) -> bool {
    if path == "/logout" {
        return true;
    }
    // the frame of every migrated page: its stream and "Got it" on the alert bar
    if path == "/shell/events" || path.starts_with("/oplogo/") || path == "/alerts/read" || path == "/ops/notice" {
        return !pages.is_empty();
    }
    let on = |p: &str| pages.iter().any(|x| x == p);
    match path {
        "/charts" | "/functions" | "/system" => on(path),
        "/health" => false,
        // the home page (5b); its stream and buttons under /home
        "/" => on("/"),
        _ if under(path, "/home") => on("/"),
        // folded into /scheduler (5a): its redirect comes with it
        _ if under(path, "/router/schedule") => on("/scheduler"),
        "/signal" | "/signal/events" | "/retry" => on("/signal"),
        _ if path.starts_with("/roam/") => on("/signal"),
        // /sms, /sms/compose and /sms/forward each by their own entry
        _ if under(path, "/sms/forward") => on("/sms/forward"),
        _ if under(path, "/sms/compose") => on("/sms/compose"),
        _ if under(path, "/sms") => on("/sms"),
        // the 8c-A step 5 pages: the page and everything under it (stream, buttons)
        _ => OWN_PREFIX.iter().any(|p| {
            on(p) && (path == *p || path.strip_prefix(p).is_some_and(|r| r.starts_with('/')))
        }),
    }
}

/// `/health` from a browser once its page is migrated (5a). Anything else asking `/health` (the
/// ship script's curl, tests) keeps luyaz-web's own link status, public as before.
pub fn health_page(path: &str, h: &HeaderMap, pages: &[String]) -> bool {
    path == "/health" && pages.iter().any(|p| p == "/health") && wants_html(h)
}

pub fn wants_html(h: &HeaderMap) -> bool {
    h.get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("text/html"))
}

/// `path` is `p` or below it (`/sms/x`, not `/smsx`).
fn under(path: &str, p: &str) -> bool {
    path == p || path.strip_prefix(p).is_some_and(|r| r.starts_with('/'))
}

/// Pages whose stream and buttons live under the page's own path.
const OWN_PREFIX: &[&str] = &[
    "/trends",
    "/clients",
    "/tools/speedtest",
    "/router/signal-detect",
    "/tools/diagnose",
    "/device-info",
    "/router/qci",
    "/tools/cpu",
    "/changes",
    "/usb",
    "/bandlock",
    "/router/celllock",
    "/router/apn",
    "/router/network-mode",
    "/router/mobile-network",
    // batch 4: owned only once listed in --pages
    "/router/qos",
    "/router/vpn",
    "/router/telemetry",
    "/router/firewall",
    "/router/lan",
    "/router/wifi",
    "/router/wifi-guest",
    // batch 5a; plain `/health` is decided in `gate` (curl gets the link status, a browser the
    // page), `/alerts/read` above (the alert bar's)
    "/alerts",
    "/scheduler",
    "/router/schedule",
    "/router/esim",
    "/health",
    "/settings",
    "/config",
    // 5b
    "/services/tailscale",
    "/services/chill",
    "/router/scenario",
    // 5c
    "/router/device",
    "/router/sim",
    "/router/dualsim",
    "/router/stc",
    "/router/stk",
    "/tools/at",
    "/tools/processes",
    "/tools/enable-adb",
    "/router/dns",
];

/// Paths anyone may load: assets, the login page, the language switch, the link status.
fn public(path: &str) -> bool {
    path.starts_with("/a/") || matches!(path, "/health" | "/login" | "/lang")
}

pub async fn gate(State(app): State<Shared>, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();
    let method = req.method().clone();
    let session = app.sessions.of(req.headers());
    // the CHILL dashboard and its controller API: served here, own session check, before the slash redirect
    if crate::chill_proxy::ours(&path) {
        return next.run(req).await;
    }
    // /agent-api: a program's bearer token and source check (agent_api.rs), never a cookie session
    if crate::agent_api::ours(&path) {
        return next.run(req).await;
    }

    // the old web links with a trailing slash (next.config trailingSlash): /signal/ → /signal
    if path.len() > 1
        && path.ends_with('/')
        && (own(path.trim_end_matches('/'), &app.pages)
            || health_page(path.trim_end_matches('/'), req.headers(), &app.pages))
    {
        let to = path.trim_end_matches('/');
        let to = if query.is_empty() {
            to.to_string()
        } else {
            format!("{to}?{query}")
        };
        return Redirect::permanent(&to).into_response();
    }
    if own(&path, &app.pages) || health_page(&path, req.headers(), &app.pages) {
        let Some((sid, _)) = session else {
            // a stream or a button of a page that is already open: say so in the page (2-0)
            if datastar(req.headers()) {
                return expired_reply(req.headers());
            }
            return to_login(&method, &path, &query);
        };
        if !same_origin(&method, req.headers()) {
            return StatusCode::FORBIDDEN.into_response();
        }
        let ds = datastar(req.headers());
        let h = req.headers().clone();
        // a write from a page opened on another device: refuse before it reaches u60d (pin.rs)
        let req = if ds && method == axum::http::Method::POST && !crate::pin::id().is_empty() {
            let (parts, body) = req.into_parts();
            let Ok(bytes) = axum::body::to_bytes(body, PIN_BODY_MAX).await else {
                return StatusCode::PAYLOAD_TOO_LARGE.into_response();
            };
            if crate::pin::other_device(crate::pin::id(), &bytes) {
                return other_device_reply(&h);
            }
            Request::from_parts(parts, Body::from(bytes))
        } else {
            req
        };
        let resp = next.run(req).await;
        // a push stream ends with the same notice when its session does
        if ds && method == axum::http::Method::GET && event_stream(&resp) {
            return end_with_session(&app, sid, &h, resp);
        }
        return resp;
    }
    if public(&path) {
        if path == "/lang" && !same_origin(&method, req.headers()) {
            return StatusCode::FORBIDDEN.into_response();
        }
        return next.run(req).await;
    }
    not_found(&path, req.headers())
}

/// Nothing here serves `path` (8d-2: there is no agent behind luyaz-web any more).
fn not_found(path: &str, h: &HeaderMap) -> Response {
    if path == "/api" || path.starts_with("/api/") {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(json!({"ok": false, "error": "not_found"})),
        )
            .into_response();
    }
    let l = crate::i18n::Lang::from_headers(h);
    let html = format!(
        "<!doctype html><meta charset=utf-8><meta name=viewport content=\"width=device-width\"><title>404</title><p>{} <a href=\"/\">{}</a></p>",
        l.pick("There is no such page.", "没有这个页面。"),
        l.pick("Home", "回首页"),
    );
    (
        StatusCode::NOT_FOUND,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        html,
    )
        .into_response()
}

fn to_login(method: &axum::http::Method, path: &str, query: &str) -> Response {
    if *method != axum::http::Method::GET {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let next = if query.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{query}")
    };
    Redirect::to(&login_url(&next)).into_response()
}

/// `/login?next=<next, percent-encoded>`.
fn login_url(next: &str) -> String {
    let enc: String = next
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    format!("/login?next={enc}")
}

// ---- session expired (8c-A 2-0) ----
//
// The session lasts 1 h, not sliding (auth.rs). A page already open keeps its push stream and its
// buttons; without this, a reconnecting stream got a 303 to /login (Datastar drops it silently)
// and a button a 401, and the page just stopped. Now both get one Datastar patch: the old web's
// "session expired" dialog with a link to log in and come back to this page. An open stream also
// ends with it when its session goes (expiry or logout elsewhere), checked every few seconds.
// Plain page loads still redirect to /login.

/// A Datastar request (`@get`/`@post`): its answer is read as patches, not followed.
fn datastar(h: &HeaderMap) -> bool {
    h.contains_key("datastar-request")
}

fn event_stream(r: &Response) -> bool {
    r.headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/event-stream"))
}

#[derive(askama::Template)]
#[template(path = "expired.html")]
struct Expired {
    l: crate::i18n::Lang,
    login: String,
}

/// The page the request came from (same-origin `Referer`), to come back to after login; a
/// stream or button path is not a page.
fn page_of(h: &HeaderMap) -> String {
    let host = h.get(header::HOST).and_then(|v| v.to_str().ok());
    let path = h
        .get(header::REFERER)
        .and_then(|v| v.to_str().ok())
        .and_then(|r| r.split_once("://"))
        .and_then(|(_, rest)| {
            let i = rest.find('/')?;
            let (auth, path) = rest.split_at(i);
            host.filter(|h| h.eq_ignore_ascii_case(auth))?;
            Some(path.split('#').next().unwrap_or(path))
        })
        .unwrap_or("/");
    crate::auth::safe_next(path).to_string()
}

/// The patch: replace `#lw-expired` (an empty placeholder in base.html) with the dialog. `replace`,
/// not a morph, so a second notice opens a fresh dialog even after the first was closed.
fn expired_event(h: &HeaderMap) -> String {
    use askama::Template;
    let html = Expired {
        l: crate::i18n::Lang::from_headers(h),
        login: login_url(&page_of(h)),
    }
    .render()
    .unwrap();
    patch_event(&html)
}

/// One `datastar-patch-elements` event replacing the element `html` names by id.
fn patch_event(html: &str) -> String {
    let data = format!("mode replace\n{}", crate::sse::patch_data(html));
    let mut ev = String::from("event: datastar-patch-elements\n");
    for line in data.lines() {
        ev.push_str("data: ");
        ev.push_str(line);
        ev.push('\n');
    }
    ev.push('\n');
    ev
}

/// A Datastar POST body read for the device pin is never larger than this (signals only).
const PIN_BODY_MAX: usize = 1 << 20;

#[derive(askama::Template)]
#[template(path = "other_device.html")]
struct OtherDevice {
    l: crate::i18n::Lang,
    page: String,
}

/// The wrong-device notice in `#lw-expired`, like the expired one (200: Datastar drops a non-2xx).
fn other_device_reply(h: &HeaderMap) -> Response {
    use askama::Template;
    let html = OtherDevice {
        l: crate::i18n::Lang::from_headers(h),
        page: page_of(h),
    }
    .render()
    .unwrap();
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(patch_event(&html)))
        .unwrap()
}

/// 200 with the one patch: a non-2xx would be dropped by Datastar without a word.
fn expired_reply(h: &HeaderMap) -> Response {
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(expired_event(h)))
        .unwrap()
}

/// How often an open stream checks that its session is still there.
const SESSION_CHECK: std::time::Duration = std::time::Duration::from_secs(3);

/// Pass the stream through until its session is gone, then send the notice and end (a clean end:
/// Datastar does not reconnect, and dropping the inner stream closes the page's task).
fn end_with_session(app: &Shared, sid: String, h: &HeaderMap, resp: Response) -> Response {
    use futures_util::StreamExt;
    let (app, app2, sid2) = (app.clone(), app.clone(), sid.clone());
    let gone = async move {
        while app.sessions.agent_token(&sid).is_some() {
            tokio::time::sleep(SESSION_CHECK).await;
        }
    };
    let notice = expired_event(h);
    let (parts, body) = resp.into_parts();
    // the notice only when the session is what ended it: a stream that ends by itself (a page's
    // one-off check, 5a) ends quietly
    let s = body
        .into_data_stream()
        .take_until(Box::pin(gone))
        .chain(futures_util::stream::once(async move {
            Ok(match app2.sessions.agent_token(&sid2) {
                None => axum::body::Bytes::from(notice),
                Some(_) => axum::body::Bytes::new(),
            })
        }));
    Response::from_parts(parts, Body::from_stream(s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn health_is_a_page_only_for_a_browser() {
        let pages = vec!["/health".to_string(), "/sms/forward".to_string(), "/scheduler".to_string()];
        let mut h = HeaderMap::new();
        assert!(!own("/health", &pages) && !health_page("/health", &h, &pages), "curl: */*");
        h.insert(header::ACCEPT, HeaderValue::from_static("text/html,*/*"));
        assert!(health_page("/health", &h, &pages));
        assert!(!health_page("/health", &h, &[]), "not migrated: the link status");
        assert!(own("/health/events", &pages));
        assert!(own("/oplogo/chunghwa", &pages) && !own("/oplogo/chunghwa", &[]));
        assert!(own("/sms/forward", &pages) && !own("/sms/forward", &[]));
        assert!(!own("/alerts", &pages) && own("/alerts/read", &pages));
        assert!(own("/router/schedule", &pages) && own("/router/schedule/x", &pages));
        assert!(!own("/router/schedule", &[]));
        assert!(!own("/router/schedulex", &pages));
    }

    #[test]
    fn routing_decisions() {
        let pages = vec!["/signal".to_string(), "/charts".to_string()];
        assert!(
            own("/signal", &pages) && own("/signal/events", &pages) && own("/roam/set", &pages)
        );
        assert!(own("/charts", &pages) && !own("/functions", &pages));
        assert!(
            !own("/signal", &[]),
            "switched off: not served (404)"
        );
        assert_eq!(
            to_login(&axum::http::Method::GET, "/tools/at", "x=a b").headers()["location"],
            "/login?next=/tools/at%3Fx%3Da%20b"
        );
    }

    #[test]
    fn changes_is_ours_with_its_stream_and_undo() {
        let on = vec!["/changes".to_string()];
        for p in ["/changes", "/changes/events", "/changes/undo/ask", "/changes/undo", "/changes/refresh"] {
            assert!(own(p, &on), "{p}");
            assert!(!own(p, &[]), "{p}: not migrated → not served");
        }
        assert!(!own("/changesx", &on));
    }

    #[test]
    fn sms_and_compose_each_by_their_own_entry() {
        let both = vec!["/sms".to_string(), "/sms/compose".to_string()];
        for p in ["/sms", "/sms/events", "/sms/read", "/sms/delete/ask", "/sms/open"] {
            assert!(own(p, &both), "{p}");
        }
        for p in ["/sms/compose", "/sms/compose/ask", "/sms/compose/send"] {
            assert!(own(p, &both), "{p}");
        }
        for p in [
            "/sms/forward",
            "/sms/forward/index.txt",
            "/sms/forward/__next._full.txt",
            "/smsx",
        ] {
            assert!(!own(p, &both), "{p}");
        }
        // each by its own entry
        let inbox = vec!["/sms".to_string()];
        assert!(own("/sms/read", &inbox) && !own("/sms/compose", &inbox));
        let compose = vec!["/sms/compose".to_string()];
        assert!(own("/sms/compose/send", &compose) && !own("/sms", &compose));
    }

    #[tokio::test]
    async fn anything_not_served_here_is_a_404() {
        let r = not_found("/api/wifi/apply", &HeaderMap::new());
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
        let b = axum::body::to_bytes(r.into_body(), 1024).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&b).unwrap(),
            json!({"ok": false, "error": "not_found"})
        );
        let r = not_found("/router/apn", &hm(&[("cookie", "u60_lang=en")]));
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
        assert!(r.headers()[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/html"));
        let b = axum::body::to_bytes(r.into_body(), 4096).await.unwrap();
        let b = String::from_utf8(b.to_vec()).unwrap();
        assert!(b.contains("no such page") && b.contains("href=\"/\""), "{b}");
        assert!(public("/login") && public("/a/x.css") && !public("/api/public/status"));
        assert!(!public("/_next/static/x.js") && !public("/favicon.ico"));
    }

    fn hm(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.insert(*k, v.parse().unwrap());
        }
        m
    }

    #[test]
    fn the_expired_notice_comes_back_to_the_page() {
        let h = hm(&[
            ("host", "192.168.0.1:8080"),
            ("referer", "http://192.168.0.1:8080/tools/speedtest?x=1#a"),
        ]);
        assert_eq!(page_of(&h), "/tools/speedtest?x=1");
        let other = hm(&[("host", "192.168.0.1:8080"), ("referer", "http://evil.example/x")]);
        assert_eq!(page_of(&other), "/", "another site's page is not a way back");
        assert_eq!(page_of(&hm(&[("host", "a")])), "/");
        let ev = expired_event(&h);
        assert!(ev.starts_with("event: datastar-patch-elements\ndata: mode replace\ndata: elements <div id=\"lw-expired\">"), "{ev}");
        assert!(ev.contains("登录已过期") && ev.contains("/login?next=/tools/speedtest%3Fx%3D1"), "{ev}");
        assert!(ev.ends_with("\n\n") && !ev.trim_end().contains("\n\n"), "one event");
        let en = hm(&[("cookie", "u60_lang=en")]);
        assert!(expired_event(&en).contains("Session expired"));
        let r = expired_reply(&h);
        assert_eq!(r.status(), StatusCode::OK);
        assert!(event_stream(&r));
    }
}
