//! Login (u60-web.md W2/W10, 8d-1a, 8d-2): the same admin password as today. With `--password-file`
//! (the device: `/data/zte-agent.env`, read at every login so a changed password needs no restart)
//! luyaz-web checks it itself (`u60-api::auth`) and nothing else: the agent is retired (8d-2), so a
//! session carries no agent token and everything it reads or does goes to u60d (agent.rs). Without
//! a password file (development against a fake agent) the agent's own login decides and its token
//! is kept in the session, so agent.rs can still send what u60d does not take to that agent.
//!
//! A good login gets a cookie session here (`lw_sid`: HttpOnly, SameSite=Strict, no `Secure`: plain
//! HTTP on the LAN), 1 h, not sliding (the agent token's old lifetime). Writes are
//! cookie-authenticated, so every non-GET that carries the cookie must come from this site:
//! `Origin` (or `Referer`) host:port equal to `Host` (W10).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use askama::Template;
use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::i18n::{Lang, cookie};
use crate::pages::Shared;

pub const COOKIE: &str = "lw_sid";
/// The agent's token lifetime (`u60-api::auth` TOKEN_TTL_SECS).
pub const TTL: Duration = Duration::from_secs(3600);

struct Session {
    /// Some only for a development login through the agent (no password file); never written to
    /// the file.
    agent_token: Option<String>,
    expires: Instant,
    /// `expires` on the wall clock, for the file.
    expires_unix: u64,
}

/// Sessions by the SHA-256 of their id (the cookie value is never kept, in memory or on disk).
/// With a file (the device: /data/luyaz/web-sessions) they outlive a restart of luyaz-web or of
/// the device (5a, owner 10-13: an eSIM switch reboots the device): one line per session, its
/// hash and wall-clock expiry, written on every login and logout.
#[derive(Default)]
pub struct Sessions {
    map: Mutex<HashMap<String, Session>>,
    file: Option<PathBuf>,
}

fn key(id: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(id.as_bytes())
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Sessions {
    /// Sessions kept in `file` (None: in memory only), with the live ones it already has. A clock
    /// that went back (the device before its time sync) gives a session at most [`TTL`] more.
    pub fn with_file(file: Option<PathBuf>) -> Sessions {
        let mut map = HashMap::new();
        if let Some(text) = file.as_ref().and_then(|f| std::fs::read_to_string(f).ok()) {
            let (now, wall) = (Instant::now(), unix_now());
            for l in text.lines() {
                let mut it = l.split(' ');
                let (Some(k), Some(exp)) = (it.next(), it.next().and_then(|e| e.parse::<u64>().ok()))
                else {
                    continue;
                };
                if k.len() != 64 || exp <= wall {
                    continue;
                }
                let left = Duration::from_secs(exp - wall).min(TTL);
                map.insert(
                    k.to_string(),
                    Session {
                        agent_token: None,
                        expires: now + left,
                        expires_unix: wall + left.as_secs(),
                    },
                );
            }
        }
        Sessions {
            map: Mutex::new(map),
            file,
        }
    }
    fn save(&self, m: &HashMap<String, Session>) {
        let Some(f) = &self.file else { return };
        let text: String = m
            .iter()
            .map(|(k, s)| format!("{k} {}\n", s.expires_unix))
            .collect();
        let tmp = f.with_extension("tmp");
        if let Err(e) = std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, f)) {
            eprintln!("[auth] {}: {e}; sessions will not outlive a restart", f.display());
        }
    }
    /// A new session, with the agent token if there is one; returns its id.
    pub fn create(&self, agent_token: Option<&str>) -> String {
        let id = random_id();
        let now = Instant::now();
        let mut m = self.map.lock().unwrap();
        m.retain(|_, s| s.expires > now);
        m.insert(
            key(&id),
            Session {
                agent_token: agent_token.map(str::to_string),
                expires: now + TTL,
                expires_unix: unix_now() + TTL.as_secs(),
            },
        );
        self.save(&m);
        id
    }
    /// The agent token of a live session (inner None: a session without one).
    pub fn agent_token(&self, id: &str) -> Option<Option<String>> {
        let m = self.map.lock().unwrap();
        m.get(&key(id))
            .filter(|s| s.expires > Instant::now())
            .map(|s| s.agent_token.clone())
    }
    pub fn remove(&self, id: &str) {
        let mut m = self.map.lock().unwrap();
        m.remove(&key(id));
        self.save(&m);
    }
    /// The live session of a request, as (id, agent token).
    pub fn of(&self, h: &HeaderMap) -> Option<(String, Option<String>)> {
        let id = cookie(h, COOKIE)?;
        let tok = self.agent_token(&id)?;
        Some((id, tok))
    }
}

pub(crate) fn random_id() -> String {
    let mut b = [0u8; 16];
    // urandom on the device; there is no fallback worth having for a login secret
    std::io::Read::read_exact(
        &mut std::fs::File::open("/dev/urandom").expect("/dev/urandom"),
        &mut b,
    )
    .expect("urandom");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn set_cookie(id: &str) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{COOKIE}={id}; Path=/; Max-Age={}; HttpOnly; SameSite=Strict",
        TTL.as_secs()
    ))
    .unwrap()
}

fn clear_cookie() -> HeaderValue {
    HeaderValue::from_static("lw_sid=; Path=/; Max-Age=0; HttpOnly; SameSite=Strict")
}

/// W10: a non-GET request must name this site as its origin. `Origin`, else `Referer`; compared as
/// host:port against `Host`, so it works on the LAN IP, the Tailscale name and any port.
pub fn same_origin(method: &Method, h: &HeaderMap) -> bool {
    if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return true;
    }
    let Some(host) = h.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let from = h
        .get(header::ORIGIN)
        .or_else(|| h.get(header::REFERER))
        .and_then(|v| v.to_str().ok())
        .and_then(authority);
    from.is_some_and(|a| a.eq_ignore_ascii_case(host))
}

/// "http://192.168.0.1:8080/x" → "192.168.0.1:8080".
fn authority(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    Some(&rest[..end]).filter(|a| !a.is_empty())
}

/// A same-site path to go back to after login (no `//host`, no scheme).
pub fn safe_next(next: &str) -> &str {
    if next.starts_with('/')
        && !next.starts_with("//")
        && !next.starts_with("/\\")
        && next != "/login"
    {
        next
    } else {
        "/"
    }
}

// ---- pages ----

#[derive(Template)]
#[template(path = "login.html")]
struct LoginPage {
    lang: Lang,
    next: String,
    error: &'static str,
}

#[derive(Template)]
#[template(path = "login_done.html")]
struct LoginDone {
    next_js: String,
}

#[derive(Deserialize)]
pub struct NextQuery {
    #[serde(default)]
    next: String,
}

pub async fn login_page(Query(q): Query<NextQuery>, headers: HeaderMap) -> Response {
    let lang = Lang::from_headers(&headers);
    page(
        &headers,
        LoginPage {
            lang,
            next: safe_next(&q.next).to_string(),
            error: "",
        }
        .render()
        .unwrap(),
        StatusCode::OK,
    )
}

fn page(headers: &HeaderMap, html: String, status: StatusCode) -> Response {
    let mut r = crate::assets::maybe_gzip(
        headers,
        "text/html; charset=utf-8",
        html.into_bytes(),
        "no-store",
    );
    *r.status_mut() = status;
    r
}

#[derive(Deserialize)]
pub struct LoginForm {
    password: String,
    #[serde(default)]
    next: String,
}

/// Phase 10 R6: the password file is moving from `/data/zte-agent.env` to `/data/luyaz/web.env`
/// (10e, one `mv`). Given the old one, the new one wins when it is there; checked at every login,
/// so the move needs no restart. Any other path is used as given.
pub fn password_path(given: &std::path::Path) -> PathBuf {
    if let Some(data) = given
        .to_str()
        .and_then(|s| s.strip_suffix("/zte-agent.env"))
        .filter(|d| d.ends_with("/data"))
    {
        let new = PathBuf::from(format!("{data}/luyaz/web.env"));
        if new.is_file() {
            return new;
        }
    }
    given.to_path_buf()
}

/// The login check: the password file alone decides (Err(true) = wrong password, Err(false) =
/// no file given, or it is unreadable or has no password: refuse, never run open).
async fn check(app: &Shared, password: &str) -> Result<(), bool> {
    let Some(path) = &app.password_file else {
        eprintln!("[auth] no --password-file; refusing logins");
        return Err(false);
    };
    let path = &password_path(path);
    let text = tokio::fs::read_to_string(path).await.map_err(|e| {
        eprintln!("[auth] {}: {e}; refusing logins", path.display());
        false
    })?;
    let stored = luyaz_api::auth::password_from_env_file(&text).ok_or_else(|| {
        eprintln!("[auth] {}: no password; refusing logins", path.display());
        false
    })?;
    if !luyaz_api::auth::password_matches(stored, password) {
        return Err(true);
    }
    Ok(())
}

pub async fn login(
    State(app): State<Shared>,
    headers: HeaderMap,
    Form(f): Form<LoginForm>,
) -> Response {
    let lang = Lang::from_headers(&headers);
    let next = safe_next(&f.next).to_string();
    if !same_origin(&Method::POST, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match check(&app, &f.password).await {
        Ok(()) => {
            let id = app.sessions.create(None);
            let html = LoginDone {
                next_js: js_str(&next),
            }
            .render()
            .unwrap();
            let mut r = page(&headers, html, StatusCode::OK);
            r.headers_mut().insert(header::SET_COOKIE, set_cookie(&id));
            r
        }
        Err(wrong) => {
            let error = if wrong {
                lang.pick("Wrong password.", "密码不对。")
            } else {
                lang.pick(
                    "The device did not answer. Try again in a moment.",
                    "设备没有回应，稍后再试。",
                )
            };
            page(
                &headers,
                LoginPage { lang, next, error }.render().unwrap(),
                if wrong {
                    StatusCode::UNAUTHORIZED
                } else {
                    StatusCode::BAD_GATEWAY
                },
            )
        }
    }
}

/// POST /logout: end the session here (and any token the old web left behind).
pub async fn logout(State(app): State<Shared>, headers: HeaderMap) -> Response {
    if !same_origin(&Method::POST, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if let Some(id) = cookie(&headers, COOKIE) {
        app.sessions.remove(&id);
    }
    let html = "<!doctype html><meta charset=utf-8><script>try{localStorage.removeItem(\"u60.token\")}catch(e){}location.replace(\"/login\")</script>";
    let mut r = page(&headers, html.to_string(), StatusCode::OK);
    r.headers_mut().insert(header::SET_COOKIE, clear_cookie());
    r
}

/// A JSON string literal, safe inside `<script>` (no `</script>` break-out).
fn js_str(s: &str) -> String {
    serde_json::to_string(s).unwrap().replace('<', "\\u003c")
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_new_password_file_wins_when_it_is_there() {
        use super::password_path;
        let d = std::env::temp_dir().join(format!("lw-pwpath-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("data/luyaz")).unwrap();
        let old = d.join("data/zte-agent.env");
        let new = d.join("data/luyaz/web.env");
        std::fs::write(&old, "ZTE_AGENT_PASSWORD=a\n").unwrap();
        assert_eq!(password_path(&old), old, "only the old one");
        std::fs::write(&new, "ZTE_AGENT_PASSWORD=b\n").unwrap();
        assert_eq!(password_path(&old), new, "both: the new one");
        std::fs::remove_file(&old).unwrap();
        assert_eq!(password_path(&old), new, "only the new one");
        let other = d.join("pw.env");
        assert_eq!(password_path(&other), other, "any other path as given");
        let _ = std::fs::remove_dir_all(&d);
    }

    use super::*;

    #[test]
    fn sessions_outlive_a_restart_by_hash_only() {
        let dir = std::env::temp_dir().join(format!("lw-sess-{}", random_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("web-sessions");
        let a = Sessions::with_file(Some(f.clone()));
        let (id, gone) = (a.create(Some("agent-tok")), a.create(None));
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(!text.contains(&id) && !text.contains("agent-tok"), "no id, no agent token: {text}");
        assert!(text.contains(&key(&id)));
        a.remove(&gone);
        // a restart: the session is back, without an agent token
        let b = Sessions::with_file(Some(f.clone()));
        assert_eq!(b.agent_token(&id), Some(None));
        assert_eq!(b.agent_token(&gone), None, "logged out stays out");
        // expired lines are dropped; a far future one (clock went back) gets at most TTL
        let now = unix_now();
        let k = key("x");
        std::fs::write(&f, format!("{} {}\n{k} {}\nbad\n", key("old"), now - 1, now + 999_999)).unwrap();
        let c = Sessions::with_file(Some(f.clone()));
        assert_eq!(c.agent_token("old"), None);
        assert_eq!(c.agent_token("x"), Some(None));
        assert!(c.map.lock().unwrap()[&k].expires_unix <= now + TTL.as_secs() + 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn h(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.insert(*k, v.parse().unwrap());
        }
        m
    }

    #[test]
    fn origin_check() {
        let post = Method::POST;
        assert!(same_origin(&Method::GET, &h(&[])));
        assert!(same_origin(
            &post,
            &h(&[
                ("host", "192.168.0.1:8080"),
                ("origin", "http://192.168.0.1:8080")
            ])
        ));
        assert!(same_origin(
            &post,
            &h(&[
                ("host", "u60.tail:8080"),
                ("referer", "http://u60.tail:8080/signal?x=1")
            ])
        ));
        assert!(!same_origin(
            &post,
            &h(&[
                ("host", "192.168.0.1:8080"),
                ("origin", "http://evil.example")
            ])
        ));
        assert!(
            !same_origin(
                &post,
                &h(&[
                    ("host", "192.168.0.1:8080"),
                    ("origin", "http://192.168.0.1:9090")
                ])
            ),
            "other port is another site"
        );
        assert!(
            !same_origin(&post, &h(&[("host", "192.168.0.1:8080")])),
            "no origin, no referer"
        );
        assert!(!same_origin(
            &post,
            &h(&[("host", "192.168.0.1:8080"), ("origin", "null")])
        ));
    }

    #[test]
    fn sessions_and_next() {
        let s = Sessions::default();
        let id = s.create(Some("agenttok"));
        assert_eq!(id.len(), 32);
        assert_eq!(s.agent_token(&id), Some(Some("agenttok".into())));
        assert_ne!(s.create(Some("agenttok")), id);
        let bare = s.create(None);
        assert_eq!(s.agent_token(&bare), Some(None), "a session without an agent token");
        s.remove(&id);
        assert!(s.agent_token(&id).is_none());
        assert_eq!(safe_next("/tools/at?x=1"), "/tools/at?x=1");
        assert_eq!(safe_next("//evil.example"), "/");
        assert_eq!(safe_next("https://evil.example"), "/");
        assert_eq!(safe_next("/\\evil"), "/");
        assert_eq!(js_str("</script>"), "\"\\u003c/script>\"");
    }
}
