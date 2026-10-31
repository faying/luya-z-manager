//! CHILL dashboard (zashboard) served by luyaz-web (8d-2):
//! `/chill-ui/…` serves the files in `/data/chill/ui` (`--chill-ui-dir`; index.html for the
//! root), `/chill-api/…` forwards REST and WebSocket to mihomo's controller on `127.0.0.1:9999`
//! (chill.sh binds it there, or on 0.0.0.0 with `CHILL_API_LAN=1`; loopback reaches both).
//!
//! Auth: a luyaz-web session, nothing else. The agent also took its dashboard secret
//! (`/data/chill/dashboard.secret`) or an agent token as `Bearer`/`?token=`; here the CHILL
//! page's link carries no secret (chill_page/view.rs) and zashboard is on this origin, so the
//! `lw_sid` cookie rides on its fetches and WebSockets. The secret is neither read nor created,
//! and whatever zashboard sends as its own password (`Authorization`, `token=`) is dropped, so
//! the controller is never reachable without a login. Writes must come from this site
//! (`same_origin`); WebSocket handshakes are GETs, and `SameSite=Strict` keeps the cookie off
//! cross-site ones. mihomo itself has no secret (the touchscreen talks to it on loopback).
//!
//! proxy.rs's gate hands both prefixes straight here (before its slash redirect, which would
//! turn `/chill-ui/` into `/chill-ui` and break zashboard's relative asset URLs).

use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::any;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::auth::Sessions;
use crate::pages::Shared;

pub const UI_PREFIX: &str = "/chill-ui";
pub const API_PREFIX: &str = "/chill-api";
const UI_DIR: &str = "/data/chill/ui";
const UPSTREAM: &str = "127.0.0.1:9999";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// Request bodies (config uploads, rule edits) are read up to this, as in the agent.
const MAX_BODY: usize = 4 * 1024 * 1024;

/// Where the files are and where mihomo listens.
pub struct Conf {
    pub ui_dir: PathBuf,
    pub upstream: String,
}

static UI_DIR_FLAG: OnceLock<PathBuf> = OnceLock::new();
static CONF: OnceLock<Conf> = OnceLock::new();

/// `--chill-ui-dir`; call before the first request.
pub fn set_ui_dir(dir: PathBuf) {
    let _ = UI_DIR_FLAG.set(dir);
}

/// `--chill-sh`: the file whose absence means CHILL is not installed (install.sh status reads
/// the same `/data/chill/chill.sh`). `None`: not checked, taken as installed (off the device).
static CHILL_SH: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Call before the first request; an empty path is the same as none.
pub fn set_chill_sh(p: Option<PathBuf>) {
    let _ = CHILL_SH.set(p.filter(|p| !p.as_os_str().is_empty()));
}

/// CHILL is on this device: without it home, the sidebar and search leave CHILL out. Only "no
/// such file" counts as absent; any other error keeps CHILL shown.
pub fn installed() -> bool {
    match CHILL_SH.get() {
        Some(Some(p)) => present(p),
        _ => true,
    }
}

fn present(p: &std::path::Path) -> bool {
    !matches!(std::fs::metadata(p), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
}

fn conf() -> &'static Conf {
    CONF.get_or_init(|| Conf {
        ui_dir: UI_DIR_FLAG
            .get()
            .cloned()
            .unwrap_or_else(|| PathBuf::from(UI_DIR)),
        upstream: UPSTREAM.to_string(),
    })
}

/// The paths served here, whatever `--pages` says.
pub fn ours(path: &str) -> bool {
    [UI_PREFIX, API_PREFIX]
        .iter()
        .any(|p| path == *p || path.strip_prefix(p).is_some_and(|r| r.starts_with('/')))
}

pub fn routes() -> Router<Shared> {
    let h = |State(app): State<Shared>, req: Request| async move {
        handle(&app.sessions, &app.proxy_http, conf(), req).await
    };
    Router::new()
        .route(UI_PREFIX, any(h))
        .route("/chill-ui/", any(h))
        .route("/chill-ui/{*rest}", any(h))
        .route(API_PREFIX, any(h))
        .route("/chill-api/", any(h))
        .route("/chill-api/{*rest}", any(h))
}

/// Both prefixes: the session check, then the files or mihomo.
pub async fn handle(
    sessions: &Sessions,
    http: &reqwest::Client,
    conf: &Conf,
    req: Request,
) -> Response {
    // the raw path: the `{*rest}` extractor percent-decodes, which would bring `%2e%2e` back
    let path = req.uri().path().to_string();
    let method = req.method().clone();
    if sessions.of(req.headers()).is_none() {
        if method == Method::GET && path.starts_with(UI_PREFIX) {
            let next = match req.uri().query() {
                Some(q) => format!("{path}?{q}"),
                None => path.clone(),
            };
            return Redirect::to(&format!("/login?next={}", enc(&next))).into_response();
        }
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }
    if !crate::auth::same_origin(&method, req.headers()) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if let Some(rest) = path
        .strip_prefix(UI_PREFIX)
        .filter(|r| r.is_empty() || r.starts_with('/'))
    {
        if !matches!(method, Method::GET | Method::HEAD) {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        let root = conf.ui_dir.clone();
        let rest = rest.to_string();
        return tokio::task::spawn_blocking(move || ui(&root, &rest))
            .await
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    api(http, conf, req).await
}

/// For the login redirect's `next=`.
fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn text(code: StatusCode, msg: &'static str) -> Response {
    (
        code,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        msg,
    )
        .into_response()
}

// ---- /chill-ui ----

/// The file under `root` for `rest` (the path after `/chill-ui`), or None for anything that is
/// not plain names: `..`, `.`, a root or a prefix.
fn ui_file(root: &Path, rest: &str) -> Option<PathBuf> {
    let rel = rest.trim_start_matches('/');
    if rel.is_empty() {
        return Some(root.join("index.html"));
    }
    if rel.split('/').any(|s| s == ".." || s == ".") || rel.contains('\\') || rel.contains('\0') {
        return None;
    }
    let p = Path::new(rel);
    if !p.components().all(|c| matches!(c, Component::Normal(_))) {
        return None;
    }
    let f = root.join(p);
    Some(if f.is_dir() { f.join("index.html") } else { f })
}

fn mime(file: &Path) -> &'static str {
    match file.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "webmanifest" => "application/manifest+json",
        // images
        "jpeg" | "jpg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "txt" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// `/chill-ui/…` → a file in the ui dir. HEAD is answered by the server from the same response.
fn ui(root: &Path, rest: &str) -> Response {
    let Some(file) = ui_file(root, rest) else {
        return text(StatusCode::NOT_FOUND, "not found");
    };
    let Ok(data) = std::fs::read(&file) else {
        return text(StatusCode::NOT_FOUND, "not found");
    };
    let data = if file.file_name().and_then(|n| n.to_str()) == Some("index.html") {
        strip_bootstrap(data)
    } else {
        data
    };
    let cache = if rest.trim_start_matches('/').starts_with("assets/") {
        "public, max-age=31536000, immutable" // RESIDUE-OK: standard Cache-Control value
    } else {
        "no-cache"
    };
    (
        [
            (header::CONTENT_TYPE, mime(&file)),
            (header::CACHE_CONTROL, cache),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        data,
    )
        .into_response()
}

/// The same /data/chill/ui is also mihomo's own `:9999/ui`, and install-chill.sh (stage.sh)
/// injects a script there that seeds zashboard with `<host>:9999`. Behind this proxy that seed is
/// wrong (the backend is `/chill-api` on this port, and :9999 may be closed to the LAN), so cut it
/// out of index.html when serving here. The CHILL page's link carries the real setup in the hash.
fn strip_bootstrap(data: Vec<u8>) -> Vec<u8> {
    const START: &[u8] = b"<script>;(function(){try{if(localStorage.getItem(\"setup/api-list\"))";
    const END: &[u8] = b"</script>";
    let find = |hay: &[u8], pat: &[u8], from: usize| {
        hay[from..]
            .windows(pat.len())
            .position(|w| w == pat)
            .map(|i| i + from)
    };
    let Some(a) = find(&data, START, 0) else {
        return data;
    };
    let Some(b) = find(&data, END, a) else {
        return data;
    };
    let mut out = data[..a].to_vec();
    out.extend_from_slice(&data[b + END.len()..]);
    out
}

// ---- /chill-api ----

/// Query string without `token=` (zashboard's own password; mihomo has no secret).
fn strip_token(query: &str) -> String {
    query
        .split('&')
        .filter(|kv| !kv.is_empty() && !kv.starts_with("token="))
        .collect::<Vec<_>>()
        .join("&")
}

/// The upstream path and query for `/chill-api…`, or the refusal.
fn target(path: &str, query: Option<&str>) -> Result<String, Response> {
    let rest = path.strip_prefix(API_PREFIX).unwrap_or("");
    let rest = if rest.is_empty() { "/" } else { rest };
    if rest.split('/').any(|s| s == "..") {
        return Err(text(StatusCode::BAD_REQUEST, "bad path"));
    }
    // mihomo's /upgrade, /upgrade/ui, /upgrade/geo would pull a new core or panel from GitHub. The
    // core is pinned by sha256 in the kit, so the dashboard may not replace it.
    if rest == "/upgrade" || rest.starts_with("/upgrade/") {
        return Err(text(
            StatusCode::FORBIDDEN,
            "updates come from the install kit",
        ));
    }
    let q = strip_token(query.unwrap_or(""));
    Ok(if q.is_empty() {
        rest.to_string()
    } else {
        format!("{rest}?{q}")
    })
}

const PASS_REQ: [header::HeaderName; 3] = [
    header::CONTENT_TYPE,
    header::ACCEPT,
    header::ACCEPT_LANGUAGE,
];
const HOP: [&str; 8] = [
    "connection",
    "keep-alive",
    "transfer-encoding",
    "upgrade",
    "server",
    "date",
    "te",
    "trailer",
];

fn websocket_req(h: &HeaderMap) -> bool {
    h.get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
}

async fn api(http: &reqwest::Client, conf: &Conf, req: Request) -> Response {
    let target = match target(req.uri().path(), req.uri().query()) {
        Ok(t) => t,
        Err(r) => return r,
    };
    if websocket_req(req.headers()) {
        return websocket(conf, req, &target).await;
    }
    let (parts, body) = req.into_parts();
    let Ok(body) = axum::body::to_bytes(body, MAX_BODY).await else {
        return text(StatusCode::PAYLOAD_TOO_LARGE, "body too large");
    };
    let mut rb = http.request(
        parts.method.clone(),
        format!("http://{}{target}", conf.upstream),
    );
    for name in &PASS_REQ {
        if let Some(v) = parts.headers.get(name) {
            rb = rb.header(name, v);
        }
    }
    if !body.is_empty() || !matches!(parts.method, Method::GET | Method::HEAD) {
        rb = rb.body(body);
    }
    let up = match rb.send().await {
        Ok(r) => r,
        Err(_) => return text(StatusCode::BAD_GATEWAY, "CHILL is not running"),
    };
    // streamed as it comes: /logs, /traffic, /connections over plain HTTP never end
    let mut r = Response::builder().status(up.status());
    for (k, v) in up.headers() {
        if !HOP.contains(&k.as_str()) {
            r = r.header(k, v);
        }
    }
    r.body(Body::from_stream(up.bytes_stream()))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

/// Read an HTTP/1.x response head. Returns (status, headers, bytes read past the head).
async fn read_head(
    s: &mut tokio::net::TcpStream,
) -> std::io::Result<(u16, Vec<(String, String)>, Vec<u8>)> {
    use std::io::{Error, ErrorKind};
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let end = loop {
        let n = s.read(&mut chunk).await?;
        if n == 0 {
            return Err(Error::new(ErrorKind::UnexpectedEof, "upstream closed"));
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > 32 * 1024 {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "upstream head too large",
            ));
        }
    };
    let head = String::from_utf8_lossy(&buf[..end]).to_string();
    let rest = buf[end + 4..].to_vec();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap_or(502);
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    Ok((status, headers, rest))
}

/// The handshake goes to mihomo first; on its 101 the browser gets ours, and both sockets are
/// spliced byte for byte (frames pass untouched, so no WebSocket library is needed). Own task: a
/// dashboard keeps several sockets open while it is on screen. A closed browser ends the splice,
/// which closes the upstream socket too.
async fn websocket(conf: &Conf, mut req: Request, target: &str) -> Response {
    let on_upgrade = hyper::upgrade::on(&mut req);
    let mut up = match tokio::time::timeout(
        CONNECT_TIMEOUT,
        tokio::net::TcpStream::connect(&conf.upstream),
    )
    .await
    {
        Ok(Ok(s)) => s,
        _ => return text(StatusCode::BAD_GATEWAY, "CHILL is not running"),
    };
    let mut head = format!(
        "GET {target} HTTP/1.1\r\nHost: {}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n",
        conf.upstream
    );
    for name in [
        "sec-websocket-key",
        "sec-websocket-version",
        "sec-websocket-protocol",
        "sec-websocket-extensions",
    ] {
        if let Some(v) = req.headers().get(name).and_then(|v| v.to_str().ok()) {
            head.push_str(&format!("{name}: {v}\r\n"));
        }
    }
    head.push_str("\r\n");
    if up.write_all(head.as_bytes()).await.is_err() {
        return text(StatusCode::BAD_GATEWAY, "upstream write failed");
    }
    let (status, headers, pre) =
        match tokio::time::timeout(CONNECT_TIMEOUT, read_head(&mut up)).await {
            Ok(Ok(h)) => h,
            _ => return text(StatusCode::BAD_GATEWAY, "upstream read failed"),
        };
    if status != 101 {
        let code = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
        return text(code, "upstream refused the websocket");
    }
    let mut r = Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(header::CONNECTION, HeaderValue::from_static("upgrade"))
        .header(header::UPGRADE, HeaderValue::from_static("websocket"));
    for (k, v) in &headers {
        if k.to_ascii_lowercase().starts_with("sec-websocket-") {
            r = r.header(k.as_str(), v.as_str());
        }
    }
    tokio::spawn(async move {
        let Ok(client) = on_upgrade.await else { return };
        let mut client = hyper_util::rt::TokioIo::new(client);
        if !pre.is_empty() && client.write_all(&pre).await.is_err() {
            return;
        }
        let _ = tokio::io::copy_bidirectional(&mut client, &mut up).await;
    });
    r.body(Body::empty())
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn chill_is_absent_only_when_its_script_is_missing() {
        let d = std::env::temp_dir().join(format!("lweb-chill-sh-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let sh = d.join("chill.sh");
        assert!(!present(&sh));
        std::fs::write(&sh, "#!/bin/sh\n").unwrap();
        assert!(present(&sh));
        // not a "no such file" (a path through a file): kept as installed
        assert!(present(&sh.join("x")));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn strips_only_the_seed_script() {
        let html = b"<head><script>;(function(){try{if(localStorage.getItem(\"setup/api-list\"))return;x()}catch(e){}})()</script><script type=\"module\" src=\"a.js\"></script></head>".to_vec();
        assert_eq!(
            strip_bootstrap(html),
            b"<head><script type=\"module\" src=\"a.js\"></script></head>".to_vec()
        );
        let plain = b"<head><script type=\"module\"></script></head>".to_vec();
        assert_eq!(strip_bootstrap(plain.clone()), plain);
    }

    #[test]
    fn token_is_removed_and_upgrade_refused() {
        assert_eq!(strip_token("token=abc&x=1"), "x=1");
        assert_eq!(strip_token("x=1&token=abc"), "x=1");
        assert_eq!(strip_token("token=abc"), "");
        assert_eq!(target("/chill-api", None).ok().unwrap(), "/");
        assert_eq!(
            target("/chill-api/logs", Some("token=s&level=info"))
                .ok()
                .unwrap(),
            "/logs?level=info"
        );
        assert_eq!(
            target("/chill-api/upgrade/ui", None)
                .err()
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            target("/chill-api/a/../b", None).err().unwrap().status(),
            StatusCode::BAD_REQUEST
        );
        assert!(ours("/chill-ui") && ours("/chill-ui/") && ours("/chill-api/version"));
        assert!(!ours("/chill-uix") && !ours("/services/chill"));
    }

    struct T {
        sessions: Sessions,
        http: reqwest::Client,
        conf: Arc<Conf>,
    }

    /// This module's routes on a port, with its own state; returns (base URL, a session id).
    async fn serve(ui_dir: PathBuf, upstream: String) -> (String, String) {
        let t = Arc::new(T {
            sessions: Sessions::with_file(None),
            http: reqwest::Client::new(),
            conf: Arc::new(Conf { ui_dir, upstream }),
        });
        let sid = t.sessions.create(None);
        let h = move |req: Request| {
            let t = t.clone();
            async move { handle(&t.sessions, &t.http, &t.conf, req).await }
        };
        let app = Router::new()
            .route("/chill-ui/", any(h.clone()))
            .route("/chill-ui/{*rest}", any(h.clone()))
            .route("/chill-api/{*rest}", any(h));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        (base, sid)
    }

    /// One raw request (no client-side path normalization), the whole answer back.
    async fn raw(base: &str, path: &str, extra: &str) -> String {
        let addr = base.trim_start_matches("http://");
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        let req =
            format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n{extra}\r\n");
        s.write_all(req.as_bytes()).await.unwrap();
        let mut out = Vec::new();
        s.read_to_end(&mut out).await.unwrap();
        String::from_utf8_lossy(&out).to_string()
    }

    fn ui_tree() -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!(
            "lw-chill-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let ui = base.join("ui");
        std::fs::create_dir_all(ui.join("assets")).unwrap();
        std::fs::write(
            ui.join("index.html"),
            "<head><script>;(function(){try{if(localStorage.getItem(\"setup/api-list\"))return})()</script>ZB</head>",
        )
        .unwrap();
        std::fs::write(ui.join("assets/app.js"), "console.log(1)").unwrap();
        std::fs::write(base.join("secret.txt"), "TOPSECRET").unwrap();
        (base, ui)
    }

    #[tokio::test]
    async fn ui_files_with_a_session_and_no_way_out() {
        let (base_dir, ui) = ui_tree();
        let (base, sid) = serve(ui, "127.0.0.1:9".into()).await;
        let ck = format!("Cookie: lw_sid={sid}\r\n");

        let r = raw(&base, "/chill-ui/", &ck).await;
        assert!(
            r.starts_with("HTTP/1.1 200") && r.contains("ZB</head>"),
            "{r}"
        );
        assert!(!r.contains("setup/api-list"), "the :9999 seed is cut out");
        assert!(
            r.to_ascii_lowercase().contains("cache-control: no-cache"),
            "{r}"
        );
        let r = raw(&base, "/chill-ui/assets/app.js", &ck).await;
        assert!(
            r.starts_with("HTTP/1.1 200")
                && r.contains("text/javascript")
                && r.contains("immutable"),
            "{r}"
        );

        for bad in [
            "/chill-ui/../secret.txt",
            "/chill-ui/assets/../../secret.txt",
            "/chill-ui/%2e%2e/secret.txt",
            "/chill-ui/%2e%2e%2fsecret.txt",
            "/chill-ui/assets/%2F..%2F..%2Fsecret.txt",
            "/chill-ui//etc/passwd",
            "/chill-ui/./index.html",
        ] {
            let r = raw(&base, bad, &ck).await;
            assert!(
                !r.contains("TOPSECRET") && !r.contains("root:"),
                "{bad}: {r}"
            );
            assert!(!r.starts_with("HTTP/1.1 200"), "{bad}: {r}");
        }

        // no session: the page goes to the login, the API is refused
        let r = raw(&base, "/chill-ui/", "").await;
        assert!(
            r.starts_with("HTTP/1.1 303") && r.contains("/login?next=/chill-ui/"),
            "{r}"
        );
        let r = raw(&base, "/chill-api/version", "").await;
        assert!(r.starts_with("HTTP/1.1 401"), "{r}");
        let r = raw(
            &base,
            "/chill-api/version?token=x",
            "Authorization: Bearer x\r\nCookie: lw_sid=nope\r\n",
        )
        .await;
        assert!(
            r.starts_with("HTTP/1.1 401"),
            "a Bearer or token is not a login: {r}"
        );
        let _ = std::fs::remove_dir_all(base_dir);
    }

    /// A stand-in mihomo: answers every request with what it got (method, path, auth, body).
    async fn mihomo() -> String {
        let app = Router::new().fallback(|req: Request| async move {
            let auth = req.headers().get(header::AUTHORIZATION).is_some();
            let cookie = req.headers().get(header::COOKIE).is_some();
            let line = format!("{} {}", req.method(), req.uri());
            let body = axum::body::to_bytes(req.into_body(), 1 << 20)
                .await
                .unwrap();
            axum::Json(serde_json::json!({
                "line": line, "auth": auth, "cookie": cookie,
                "body": String::from_utf8_lossy(&body),
            }))
        });
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        addr
    }

    #[tokio::test]
    async fn rest_goes_to_mihomo_without_secrets() {
        let up = mihomo().await;
        let (base, sid) = serve(PathBuf::from("/nonexistent"), up).await;
        let c = reqwest::Client::new();
        let r = c
            .get(format!("{base}/chill-api/proxies?token=zz&x=1"))
            .header(header::COOKIE, format!("lw_sid={sid}"))
            .header(header::AUTHORIZATION, "Bearer zz")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let v: serde_json::Value = r.json().await.unwrap();
        assert_eq!(v["line"], "GET /proxies?x=1");
        assert_eq!(v["auth"], false, "zashboard's password is not passed on");
        assert_eq!(v["cookie"], false, "nor our session");

        let host = base.trim_start_matches("http://");
        let put = || {
            c.put(format!("{base}/chill-api/configs"))
                .header(header::COOKIE, format!("lw_sid={sid}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body("{\"mode\":\"rule\"}")
        };
        let r = put()
            .header(header::ORIGIN, format!("http://{host}"))
            .send()
            .await
            .unwrap();
        let v: serde_json::Value = r.json().await.unwrap();
        assert_eq!(v["line"], "PUT /configs");
        assert_eq!(v["body"], "{\"mode\":\"rule\"}");
        let r = put()
            .header(header::ORIGIN, "http://evil.example")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 403, "a write from another site");

        let r = c
            .get(format!("{base}/chill-api/upgrade/ui"))
            .header(header::COOKIE, format!("lw_sid={sid}"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 403);
    }

    #[tokio::test]
    async fn mihomo_down_is_a_502() {
        let (base, sid) = serve(PathBuf::from("/nonexistent"), "127.0.0.1:9".into()).await;
        let r = raw(
            &base,
            "/chill-api/version",
            &format!("Cookie: lw_sid={sid}\r\n"),
        )
        .await;
        assert!(r.starts_with("HTTP/1.1 502"), "{r}");
    }

    /// A raw upstream that accepts one WebSocket handshake, then echoes bytes back.
    async fn ws_echo() -> String {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let mut got = Vec::new();
            while !got.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = s.read(&mut buf).await.unwrap();
                got.extend_from_slice(&buf[..n]);
            }
            let head = String::from_utf8_lossy(&got).to_string();
            assert!(head.starts_with("GET /traffic HTTP/1.1"), "{head}");
            assert!(head.contains("sec-websocket-key: k=="), "{head}");
            s.write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: acc\r\n\r\nHELLO")
                .await
                .unwrap();
            loop {
                let n = s.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                s.write_all(&buf[..n]).await.unwrap();
            }
        });
        addr
    }

    #[tokio::test]
    async fn websocket_is_spliced_both_ways() {
        let up = ws_echo().await;
        let (base, sid) = serve(PathBuf::from("/nonexistent"), up).await;
        let addr = base.trim_start_matches("http://");
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        let req = format!(
            "GET /chill-api/traffic?token= HTTP/1.1\r\nHost: {addr}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: k==\r\nSec-WebSocket-Version: 13\r\nCookie: lw_sid={sid}\r\n\r\n"
        );
        s.write_all(req.as_bytes()).await.unwrap();
        let mut got = Vec::new();
        let mut buf = [0u8; 1024];
        while !String::from_utf8_lossy(&got).contains("HELLO") {
            let n = tokio::time::timeout(Duration::from_secs(5), s.read(&mut buf))
                .await
                .unwrap()
                .unwrap();
            assert!(n > 0, "closed early: {}", String::from_utf8_lossy(&got));
            got.extend_from_slice(&buf[..n]);
        }
        let head = String::from_utf8_lossy(&got).to_ascii_lowercase();
        assert!(
            head.starts_with("http/1.1 101") && head.contains("sec-websocket-accept: acc"),
            "{head}"
        );
        s.write_all(b"PING").await.unwrap();
        let n = tokio::time::timeout(Duration::from_secs(5), s.read(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buf[..n], b"PING");
    }
}
