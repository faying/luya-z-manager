//! The one small HTTP/1.x client the modules share (src/http.c): synchronous with a timeout per
//! step, the caller's buffer size, chunked bodies decoded. the core's /v2 stream and /control keep
//! their own code (core.rs), as in C.
// SPDX-License-Identifier: MIT

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

/// A parsed response. status 0 = no usable response.
#[derive(Debug, Default, Clone)]
pub struct Resp {
    pub status: i32,
    /// the body (after de-chunking), None when there was no usable response
    pub body: Option<Vec<u8>>,
    /// the buffer filled up before the peer closed
    pub truncated: bool,
}

/// poll() on one fd: 1 ready, 0 timeout, -1 error (http_wait).
pub fn wait(fd: RawFd, write: bool, ms: i32) -> i32 {
    if fd < 0 {
        return -1;
    }
    let mut p = libc::pollfd { fd, events: if write { libc::POLLOUT } else { libc::POLLIN }, revents: 0 };
    loop {
        // SAFETY: one pollfd on the stack
        let r = unsafe { libc::poll(&mut p, 1, ms) };
        if r < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return r.clamp(-1, 1);
    }
}

fn set_nonblock(fd: RawFd, on: bool) -> bool {
    // SAFETY: fcntl on our own fd
    unsafe {
        let f = libc::fcntl(fd, libc::F_GETFL, 0);
        f >= 0 && libc::fcntl(fd, libc::F_SETFL, if on { f | libc::O_NONBLOCK } else { f & !libc::O_NONBLOCK }) >= 0
    }
}

/// Connect to a dotted-quad IPv4 address: non-blocking with `ms` as the limit, then back to
/// blocking (http_connect_tcp).
pub fn connect_tcp(ip: &str, port: u16, ms: i32) -> Option<OwnedFd> {
    let addr: std::net::Ipv4Addr = ip.parse().ok()?;
    // SAFETY: a new socket we own
    let raw = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    if raw < 0 {
        return None;
    }
    // SAFETY: raw is a valid fd nobody else owns
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    if !set_nonblock(raw, true) {
        return None;
    }
    let sa = libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: port.to_be(),
        sin_addr: libc::in_addr { s_addr: u32::from_ne_bytes(addr.octets()) },
        sin_zero: [0; 8],
    };
    // SAFETY: sa is a sockaddr_in of the right size
    let rc = unsafe { libc::connect(raw, (&sa as *const libc::sockaddr_in).cast(), size_of::<libc::sockaddr_in>() as u32) };
    if rc < 0 {
        if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINPROGRESS) {
            return None;
        }
        let mut err: libc::c_int = 0;
        let mut len = size_of::<libc::c_int>() as libc::socklen_t;
        if wait(raw, true, ms) <= 0 {
            return None;
        }
        // SAFETY: SO_ERROR into an int
        let g = unsafe { libc::getsockopt(raw, libc::SOL_SOCKET, libc::SO_ERROR, (&mut err as *mut libc::c_int).cast(), &mut len) };
        if g < 0 || err != 0 {
            return None;
        }
    }
    set_nonblock(raw, false).then_some(fd)
}

/// Connect to a unix socket; fails at once when the peer's backlog is full (http_connect_unix).
pub fn connect_unix(path: &str) -> Option<OwnedFd> {
    let mut sa = libc::sockaddr_un { sun_family: libc::AF_UNIX as libc::sa_family_t, sun_path: [0; 108] };
    if path.len() >= sa.sun_path.len() {
        return None;
    }
    for (d, &s) in sa.sun_path.iter_mut().zip(path.as_bytes()) {
        *d = s as libc::c_char;
    }
    // SAFETY: a new socket we own
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    if raw < 0 {
        return None;
    }
    // SAFETY: raw is ours
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    if !set_nonblock(raw, true) {
        return None;
    }
    // SAFETY: sa is a sockaddr_un
    let rc = unsafe { libc::connect(raw, (&sa as *const libc::sockaddr_un).cast(), size_of::<libc::sockaddr_un>() as u32) };
    (rc == 0 && set_nonblock(raw, false)).then_some(fd)
}

/// Write it all; MSG_NOSIGNAL (a peer hanging up must not kill the screen with SIGPIPE).
pub fn send(fd: RawFd, req: &[u8]) -> bool {
    let mut off = 0;
    while off < req.len() {
        // SAFETY: the slice is valid for its length
        let w = unsafe { libc::send(fd, req[off..].as_ptr().cast(), req.len() - off, libc::MSG_NOSIGNAL) };
        if w < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        if w <= 0 {
            return false;
        }
        off += w as usize;
    }
    true
}

/// read(2) into the spare room of `buf` (at most `room` bytes).
pub fn read_into(fd: RawFd, buf: &mut Vec<u8>, room: usize) -> isize {
    let at = buf.len();
    buf.resize(at + room, 0);
    // SAFETY: room bytes are reserved past `at`
    let n = unsafe { libc::read(fd, buf[at..].as_mut_ptr().cast(), room) };
    buf.truncate(at + n.max(0) as usize);
    n
}

/// Read until the peer closes, a read waits longer than io_ms, or cap - 1 bytes are in
/// (http_read_all). Returns (bytes, truncated).
pub fn read_all(fd: RawFd, cap: usize, io_ms: i32) -> (Vec<u8>, bool) {
    let mut buf = Vec::new();
    if cap == 0 {
        return (buf, false);
    }
    loop {
        if buf.len() + 1 >= cap {
            return (buf, true);
        }
        if wait(fd, false, io_ms) <= 0 {
            break;
        }
        let room = cap - 1 - buf.len();
        let n = read_into(fd, &mut buf, room);
        if n < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        if n <= 0 {
            break;
        }
    }
    (buf, false)
}

/// Case-insensitive search in hay (header names are not normalised by every server).
fn ci_find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()].eq_ignore_ascii_case(needle))
}

/// Decode an HTTP/1.1 chunked body (http_dechunk): stops at the last chunk or at the first
/// malformed or short one, keeping what arrived.
pub fn dechunk(body: &[u8]) -> Vec<u8> {
    let len = body.len();
    let (mut r, mut out) = (0usize, Vec::with_capacity(len));
    loop {
        let (mut sz, mut digits) = (0usize, 0);
        while r < len {
            let d = match body[r] {
                c @ b'0'..=b'9' => (c - b'0') as usize,
                c @ b'a'..=b'f' => (c - b'a' + 10) as usize,
                c @ b'A'..=b'F' => (c - b'A' + 10) as usize,
                _ => break,
            };
            if sz > (usize::MAX >> 4) {
                digits = 0;
                break;
            }
            sz = sz * 16 + d;
            digits += 1;
            r += 1;
        }
        if digits == 0 {
            break;
        }
        while r < len && body[r] != b'\r' {
            r += 1; // ;chunk-ext
        }
        if r + 1 >= len || body[r + 1] != b'\n' {
            break;
        }
        r += 2;
        if sz == 0 {
            break; // the last chunk
        }
        let sz = sz.min(len - r); // a short read: keep what arrived
        out.extend_from_slice(&body[r..r + sz]);
        r += sz;
        if r + 1 < len && body[r] == b'\r' && body[r + 1] == b'\n' {
            r += 2;
        }
    }
    out
}

/// Parse a whole response (http_parse): status line, header end, chunked body decoded.
pub fn parse(buf: &[u8]) -> Resp {
    // C works on the NUL-terminated text
    let buf = &buf[..buf.iter().position(|&c| c == 0).unwrap_or(buf.len())];
    let mut r = Resp::default();
    if buf.len() < 12 || !buf.starts_with(b"HTTP/1.") || buf[8] != b' ' {
        return r;
    }
    let Some(hend) = super::util::find(buf, b"\r\n\r\n", 0) else { return r };
    let status = crate::json::atol(&buf[9..]);
    if !(100..=999).contains(&status) {
        return r;
    }
    r.status = status as i32;
    let body = &buf[hend + 4..];
    let head = &buf[..hend];
    let chunked = ci_find(head, b"\r\ntransfer-encoding:").is_some_and(|te| {
        let rest = &head[te + 2..];
        let eol = super::util::find(rest, b"\r\n", 0).unwrap_or(rest.len());
        ci_find(&rest[..eol], b"chunked").is_some()
    });
    r.body = Some(if chunked { dechunk(body) } else { body.to_vec() });
    r
}

/// Send req, read, parse, close (http_exchange). Returns the response (status 0 = none).
pub fn exchange(fd: OwnedFd, req: &[u8], cap: usize, io_ms: i32) -> Resp {
    if !send(fd.as_raw_fd(), req) {
        return Resp::default();
    }
    let (buf, trunc) = read_all(fd.as_raw_fd(), cap, io_ms);
    drop(fd);
    let mut r = parse(&buf);
    r.truncated = trunc;
    r
}

/// An HTTP/1.0 request (http_build); json None = no body. None if it would not fit in cap.
pub fn build(method: &str, path: &str, host: &str, extra: &str, json: Option<&str>, cap: usize) -> Option<String> {
    let s = match json {
        Some(j) => format!(
            "{method} {path} HTTP/1.0\r\nHost: {host}\r\n{extra}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{j}",
            j.len()
        ),
        None => format!("{method} {path} HTTP/1.0\r\nHost: {host}\r\n{extra}Connection: close\r\n\r\n"),
    };
    (s.len() < cap).then_some(s)
}

#[cfg(test)]
mod tests;
