//! CHILL dashboard through the agent: `/chill-ui/` serves zashboard from
//! `/data/chill/ui`, `/chill-api/…` forwards REST and WebSocket to mihomo's
//! controller on `127.0.0.1:9999`.
//!
//! Why: the controller used to be reachable from the LAN (`CHILL_API_LAN=1`,
//! guarded only by a source-IP allowlist, no secret) so browsers could use
//! zashboard. With this proxy the controller can stay on loopback, and access
//! goes through `:9090` behind a secret — also over Tailscale.
//!
//! Auth for `/chill-api/`: zashboard sends its "secret" as `Authorization:
//! Bearer …` on REST calls and as `?token=…` on WebSockets. Accepted: the
//! dashboard secret (`/data/chill/dashboard.secret`, created on first use,
//! handed to the admin web by `GET /api/services/chill/dashboard` after login)
//! or a valid admin-web session token. mihomo itself still has no secret, so
//! the touchscreen (on the device, loopback) needs no change.
//!
//! Upstream requests go out as HTTP/1.0 so mihomo answers without chunking
//! and closes when done: long-lived streams (`/logs`, `/traffic`) pass
//! straight through as the response body.

use std::fs;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, StatusCode};

use crate::handlers::AppState;

const UPSTREAM: &str = "127.0.0.1:9999";
const SECRET_PATH: &str = "/data/chill/dashboard.secret";
const UI_DIR: &str = "/data/chill/ui";
pub const API_PREFIX: &str = "/chill-api";
pub const UI_PREFIX: &str = "/chill-ui";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

fn secret_path() -> PathBuf {
    PathBuf::from(std::env::var("ZTE_AGENT_CHILL_SECRET").unwrap_or_else(|_| SECRET_PATH.to_string()))
}

/// The dashboard secret, created (32 hex chars from /dev/urandom, mode 600) on first use.
pub fn secret() -> Option<String> {
    let p = secret_path();
    if let Ok(s) = fs::read_to_string(&p) {
        let s = s.trim().to_string();
        if s.len() >= 16 && s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Some(s);
        }
    }
    let mut buf = [0u8; 16];
    fs::File::open("/dev/urandom").ok()?.read_exact(&mut buf).ok()?;
    let s: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    let tmp = p.with_extension("tmp");
    fs::write(&tmp, format!("{s}\n")).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
    }
    fs::rename(&tmp, &p).ok()?;
    Some(s)
}

/// `GET /api/services/chill/dashboard` (behind the normal login): what the
/// admin web needs to open zashboard already connected.
pub fn dashboard_info(_state: &AppState) -> (u16, Value) {
    match secret() {
        Some(s) => (200, json!({"ok": true, "data": {"secret": s, "ui": format!("{UI_PREFIX}/"), "api": API_PREFIX}})),
        None => (500, json!({"ok": false, "error": "cannot create the dashboard secret"})),
    }
}

fn ct_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn header<'a>(req: &'a Request, name: &str) -> Option<&'a str> {
    req.headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str())
}

fn query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        (k == key).then_some(v)
    })
}

/// Query string without `token=` (mihomo has no secret; don't pass it upstream).
fn strip_token(query: &str) -> String {
    query
        .split('&')
        .filter(|kv| !kv.is_empty() && !kv.starts_with("token="))
        .collect::<Vec<_>>()
        .join("&")
}

fn authorized(req: &Request, query: &str, state: &AppState) -> bool {
    if !state.auth.has_password() {
        return true; // same rule as the rest of the agent: no password set, no auth
    }
    let presented = header(req, "Authorization")
        .and_then(|v| v.strip_prefix("Bearer "))
        .or_else(|| query_param(query, "token"))
        .unwrap_or("");
    if presented.is_empty() {
        return false;
    }
    if let Some(s) = secret() {
        if ct_eq(presented, &s) {
            return true;
        }
    }
    state.auth.validate(presented)
}

fn text(req: Request, code: u16, msg: &str) {
    let _ = req.respond(
        Response::from_string(msg)
            .with_status_code(code)
            .with_header(Header::from_bytes("Content-Type", "text/plain; charset=utf-8").unwrap()),
    );
}

fn method_str(m: &Method) -> &str {
    m.as_str()
}

/// Read an HTTP/1.x response head from `s`. Returns (status, headers, bytes
/// read past the head).
fn read_head(s: &mut TcpStream) -> io::Result<(u16, Vec<(String, String)>, Vec<u8>)> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let end = loop {
        let n = s.read(&mut chunk)?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "upstream closed"));
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > 32 * 1024 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "upstream head too large"));
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

/// Everything not already read, as one reader: the bytes read with the head, then the socket.
struct Body {
    pre: io::Cursor<Vec<u8>>,
    sock: TcpStream,
}
impl Read for Body {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = self.pre.read(out)?;
        if n > 0 {
            return Ok(n);
        }
        self.sock.read(out)
    }
}

const PASS_REQ: &[&str] = &["content-type", "accept", "accept-language"];
const HOP: &[&str] = &["connection", "keep-alive", "transfer-encoding", "upgrade", "content-length", "server", "date"];

/// `/chill-api/…` → mihomo.
pub fn proxy(mut req: Request, url: &str, state: &AppState) {
    let rest = url.strip_prefix(API_PREFIX).unwrap_or("");
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    if !authorized(&req, query, state) {
        text(req, 401, "unauthorized");
        return;
    }
    let path = if path.is_empty() { "/" } else { path };
    if path.split('/').any(|seg| seg == "..") {
        text(req, 400, "bad path");
        return;
    }
    // mihomo's /upgrade, /upgrade/ui, /upgrade/geo would pull a new core or
    // panel from GitHub. The core is pinned by sha256 in the kit and the kit's
    // update path compares against it, so don't let the dashboard replace it.
    if path == "/upgrade" || path.starts_with("/upgrade/") {
        text(req, 403, "updates come from the install kit");
        return;
    }
    let q = strip_token(query);
    let target = if q.is_empty() { path.to_string() } else { format!("{path}?{q}") };
    let ws = header(&req, "Upgrade").map(|u| u.eq_ignore_ascii_case("websocket")).unwrap_or(false);

    let addr = UPSTREAM.parse().unwrap();
    let mut up = match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
        Ok(s) => s,
        Err(_) => {
            text(req, 502, "CHILL is not running");
            return;
        }
    };

    if ws {
        websocket(req, up, &target);
        return;
    }

    let mut body = Vec::new();
    if let Err(e) = req.as_reader().take(4 * 1024 * 1024).read_to_end(&mut body) {
        text(req, 400, &format!("read body: {e}"));
        return;
    }
    let mut head = format!("{} {} HTTP/1.0\r\nHost: {UPSTREAM}\r\n", method_str(req.method()), target);
    for h in req.headers() {
        let name = h.field.as_str().as_str().to_ascii_lowercase();
        if PASS_REQ.contains(&name.as_str()) {
            head.push_str(&format!("{}: {}\r\n", h.field.as_str(), h.value.as_str()));
        }
    }
    head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    if up.write_all(head.as_bytes()).and_then(|_| up.write_all(&body)).is_err() {
        text(req, 502, "upstream write failed");
        return;
    }
    let _ = up.set_read_timeout(None);
    let (status, headers, pre) = match read_head(&mut up) {
        Ok(h) => h,
        Err(_) => {
            text(req, 502, "upstream read failed");
            return;
        }
    };
    let length = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse::<usize>().ok());
    let mut resp_headers = Vec::new();
    for (k, v) in &headers {
        if !HOP.contains(&k.to_ascii_lowercase().as_str()) {
            if let Ok(h) = Header::from_bytes(k.as_bytes(), v.as_bytes()) {
                resp_headers.push(h);
            }
        }
    }
    let mut reader = Body { pre: io::Cursor::new(pre), sock: up };
    if length.is_some() {
        let _ = req.respond(Response::new(StatusCode(status), resp_headers, reader, length, None));
        return;
    }
    // No length: mihomo (answering HTTP/1.0) sends big bodies like /proxies and
    // the endless /logs, /traffic streams close-delimited. tiny_http would
    // chunk them through an 8 KB buffer and hold stream lines back, so chunk
    // by hand: one chunk per upstream read, flushed. (Close-delimited is no
    // option: tiny_http keeps the socket open for the next request, so the
    // browser would never see the body end.)
    let chunked = *req.http_version() >= tiny_http::HTTPVersion(1, 1);
    let mut out = req.into_writer();
    let mut head = format!("HTTP/1.1 {} {}\r\n", status, StatusCode(status).default_reason_phrase());
    for h in &resp_headers {
        head.push_str(&format!("{}: {}\r\n", h.field.as_str(), h.value.as_str()));
    }
    head.push_str(if chunked { "Transfer-Encoding: chunked\r\n\r\n" } else { "Connection: close\r\n\r\n" });
    if out.write_all(head.as_bytes()).and_then(|_| out.flush()).is_err() {
        return;
    }
    // Own thread, like the websockets: a stream stays open as long as someone
    // watches it and must not hold one of the request workers.
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        let done = loop {
            let n = match reader.read(&mut buf) {
                Ok(0) | Err(_) => break true,
                Ok(n) => n,
            };
            let w = if chunked {
                write!(out, "{n:x}\r\n").and_then(|_| out.write_all(&buf[..n])).and_then(|_| out.write_all(b"\r\n"))
            } else {
                out.write_all(&buf[..n])
            };
            if w.and_then(|_| out.flush()).is_err() {
                break false;
            }
        };
        if done && chunked {
            let _ = out.write_all(b"0\r\n\r\n").and_then(|_| out.flush());
        }
        let _ = reader.sock.shutdown(Shutdown::Both);
    });
}

fn websocket(req: Request, mut up: TcpStream, target: &str) {
    let mut head = format!("GET {target} HTTP/1.1\r\nHost: {UPSTREAM}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n");
    for name in ["Sec-WebSocket-Key", "Sec-WebSocket-Version", "Sec-WebSocket-Protocol", "Sec-WebSocket-Extensions"] {
        if let Some(v) = header(&req, name) {
            head.push_str(&format!("{name}: {v}\r\n"));
        }
    }
    head.push_str("\r\n");
    if up.write_all(head.as_bytes()).is_err() {
        text(req, 502, "upstream write failed");
        return;
    }
    let (status, headers, pre) = match read_head(&mut up) {
        Ok(h) => h,
        Err(_) => {
            text(req, 502, "upstream read failed");
            return;
        }
    };
    if status != 101 {
        text(req, status, "upstream refused the websocket");
        return;
    }
    let mut resp = Response::empty(StatusCode(101));
    for (k, v) in &headers {
        let lk = k.to_ascii_lowercase();
        if lk.starts_with("sec-websocket-") {
            if let Ok(h) = Header::from_bytes(k.as_bytes(), v.as_bytes()) {
                resp.add_header(h);
            }
        }
    }
    let mut client = req.upgrade("websocket", resp);
    // mihomo's sockets (/traffic, /memory, /logs, /connections) only push from
    // the server, so one pump mihomo → browser is enough. (tiny_http's
    // upgraded stream can't be split into a reader and a writer; reading it
    // on another thread would block the writes.) A closed browser shows up as
    // a failed write, which also closes the upstream socket. Own thread: a
    // dashboard keeps several sockets open as long as it is on screen, which
    // must not tie up the agent's request workers.
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        if !pre.is_empty() && client.write_all(&pre).and_then(|_| client.flush()).is_err() {
            let _ = up.shutdown(Shutdown::Both);
            return;
        }
        // A quiet socket (/logs at a low level) would park this thread until
        // mihomo's next line even after the browser is gone: after 30 s of
        // silence send a ping, whose failure notices the closed browser. Safe
        // between reads because mihomo writes each frame in one go.
        let _ = up.set_read_timeout(Some(Duration::from_secs(30)));
        loop {
            let n = match up.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                    if client.write_all(&[0x89, 0x00]).and_then(|_| client.flush()).is_err() {
                        break;
                    }
                    continue;
                }
                Err(_) => break,
            };
            if client.write_all(&buf[..n]).and_then(|_| client.flush()).is_err() {
                break;
            }
        }
        let _ = up.shutdown(Shutdown::Both);
    });
}

/// `/chill-ui/…` → files in /data/chill/ui (zashboard; index.html for the root).
pub fn ui(req: Request, path: &str, head_only: bool) {
    let rel = path.strip_prefix(UI_PREFIX).unwrap_or("").trim_start_matches('/');
    if rel.split('/').any(|s| s == "..") {
        text(req, 404, "not found");
        return;
    }
    let root = PathBuf::from(std::env::var("ZTE_AGENT_CHILL_UI").unwrap_or_else(|_| UI_DIR.to_string()));
    let mut file = if rel.is_empty() { root.join("index.html") } else { root.join(rel) };
    if file.is_dir() {
        file = file.join("index.html");
    }
    let data = match fs::read(&file) {
        Ok(d) => d,
        Err(_) => {
            text(req, 404, "not found");
            return;
        }
    };
    let data = if file.file_name().and_then(|n| n.to_str()) == Some("index.html") { strip_bootstrap(data) } else { data };
    let ext = Path::new(&file).extension().and_then(|e| e.to_str()).unwrap_or("");
    let mime = match ext {
        "webmanifest" => "application/manifest+json",
        e => crate::static_files::mime_for(e),
    };
    let cache = if rel.starts_with("assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
    let len = data.len();
    let body: Vec<u8> = if head_only { Vec::new() } else { data };
    let mut resp = Response::from_data(body)
        .with_header(Header::from_bytes("Content-Type", mime).unwrap())
        .with_header(Header::from_bytes("Cache-Control", cache).unwrap());
    if head_only {
        resp = resp.with_header(Header::from_bytes("Content-Length", len.to_string()).unwrap());
    }
    let _ = req.respond(resp);
}

/// The same /data/chill/ui is also mihomo's own `:9999/ui`, and install-chill.sh
/// injects a script there that seeds zashboard with `<host>:9999`. Behind this
/// proxy that seed is wrong (the backend is `/chill-api` on this port, and :9999
/// may be closed to the LAN), so cut it out of index.html when serving here.
/// The admin page's link carries the real setup in the URL hash instead.
fn strip_bootstrap(data: Vec<u8>) -> Vec<u8> {
    const START: &[u8] = b"<script>;(function(){try{if(localStorage.getItem(\"setup/api-list\"))";
    const END: &[u8] = b"</script>";
    let find = |hay: &[u8], pat: &[u8], from: usize| hay[from..].windows(pat.len()).position(|w| w == pat).map(|i| i + from);
    let Some(a) = find(&data, START, 0) else { return data };
    let Some(b) = find(&data, END, a) else { return data };
    let mut out = data[..a].to_vec();
    out.extend_from_slice(&data[b + END.len()..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_only_the_seed_script() {
        let html = b"<head><script>;(function(){try{if(localStorage.getItem(\"setup/api-list\"))return;x()}catch(e){}})()</script><script type=\"module\" src=\"a.js\"></script></head>".to_vec();
        assert_eq!(strip_bootstrap(html), b"<head><script type=\"module\" src=\"a.js\"></script></head>".to_vec());
        let plain = b"<head><script type=\"module\"></script></head>".to_vec();
        assert_eq!(strip_bootstrap(plain.clone()), plain);
    }

    #[test]
    fn token_is_removed_from_query() {
        assert_eq!(strip_token("token=abc&x=1"), "x=1");
        assert_eq!(strip_token("x=1&token=abc"), "x=1");
        assert_eq!(strip_token("token=abc"), "");
    }
}
