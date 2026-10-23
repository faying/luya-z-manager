//! response parsing, and round trips against local servers (timeout,
//! refused, truncation, unix socket, 401 → log in again).
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use super::*;
use crate::data::core_api::CoreApi;
use crate::data::testsrv::{listen, read_req, serve};
use std::io::Write;
use std::os::unix::net::UnixListener;
use std::time::{Duration, Instant};

fn p(s: &str) -> Resp {
    parse(s.as_bytes())
}

fn body(r: &Resp) -> &[u8] {
    r.body.as_deref().unwrap_or(b"")
}

#[test]
fn parse_responses() {
    let r = p("HTTP/1.0 200 OK\r\nContent-Length: 2\r\n\r\n{}");
    assert!(r.status == 200 && body(&r) == b"{}");
    let r = p("HTTP/1.1 401 Unauthorized\r\n\r\n");
    assert!(r.status == 401 && body(&r).is_empty());
    // chunked, header name in odd case, two chunks + extension + trailer
    let r = p("HTTP/1.1 200 OK\r\nTransfer-ENCODING: chunked\r\n\r\n4\r\n{\"a\"\r\n3;x=y\r\n:1}\r\n0\r\nX-T: 1\r\n\r\n");
    assert!(r.status == 200 && body(&r) == b"{\"a\":1}");
    // uppercase hex size
    let r = p("HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\nA\r\n0123456789\r\n0\r\n\r\n");
    assert_eq!(body(&r), b"0123456789");
    // short last chunk: keep what arrived
    let r = p("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n10\r\nabc");
    assert_eq!(body(&r), b"abc");
    // malformed size: stop, no crash
    let r = p("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\nabc");
    assert!(r.status == 200 && body(&r).is_empty());
    // absurd size larger than the buffer
    let r = p("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nffffffffffffffffff\r\nabc");
    assert!(r.status == 200 && body(&r).is_empty());
    // "chunked" in another header does not trigger de-chunking
    let r = p("HTTP/1.1 200 OK\r\nTransfer-Encoding: identity\r\nX-Note: chunked\r\n\r\n4\r\nab");
    assert_eq!(body(&r), b"4\r\nab");
    // not HTTP / no header end / garbage status
    assert_eq!(p("SSH-2.0-dropbear\r\n\r\n").status, 0);
    assert_eq!(p("HTTP/1.0 200 OK\r\nContent-Length: 2\r\n").status, 0);
    assert_eq!(p("HTTP/1.0 abc\r\n\r\n").status, 0);
    assert_eq!(p("").status, 0);
}

#[test]
fn dechunk_direct() {
    assert!(dechunk(b"0\r\n\r\n").is_empty());
    assert_eq!(dechunk(b"3\r\nabc"), b"abc"); // no CRLF after the data
    assert!(dechunk(b"3").is_empty()); // a size only
}

#[test]
fn build_requests() {
    let r = build("POST", "/x", "127.0.0.1:9090", "A: b\r\n", Some("{}"), 256).unwrap();
    assert!(r.contains("POST /x HTTP/1.0\r\nHost: 127.0.0.1:9090\r\nA: b\r\n"));
    assert!(r.contains("Content-Length: 2\r\n") && r.contains("\r\n\r\n{}"));
    let r = build("GET", "/y", "h", "", None, 256).unwrap();
    assert!(!r.contains("Content-Length") && r.contains("Connection: close\r\n\r\n"));
    assert!(build("GET", "/a-very-long-path-that-does-not-fit", "h", "", None, 20).is_none());
}

fn h_ok(c: &mut std::net::TcpStream, _i: usize) {
    read_req(c);
    let _ = c.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n");
    std::thread::sleep(Duration::from_millis(20)); // the chunk arrives in a later read
    let _ = c.write_all(b"5\r\nhello\r\n0\r\n\r\n");
}

#[test]
fn round_trips() {
    let req = build("GET", "/", "x", "", None, 256).unwrap();
    // chunked answer split over two reads
    let (l, port) = listen();
    let h = serve(l, 1, h_ok);
    let fd = connect_tcp("127.0.0.1", port, 500).expect("connect");
    let r = exchange(fd, req.as_bytes(), 4096, 500);
    assert!(r.status == 200 && body(&r) == b"hello" && !r.truncated);
    h.join().unwrap();

    // the peer never answers: bounded by io_ms, status 0
    let (l, port) = listen();
    let h = serve(l, 1, |c, _| {
        read_req(c);
        std::thread::sleep(Duration::from_millis(600));
    });
    let fd = connect_tcp("127.0.0.1", port, 500).unwrap();
    let t = Instant::now();
    assert_eq!(exchange(fd, req.as_bytes(), 4096, 200).status, 0);
    let ms = t.elapsed().as_millis();
    assert!((150..1000).contains(&ms), "{ms} ms");
    h.join().unwrap();

    // larger than the buffer: truncated, still parsed
    let (l, port) = listen();
    let h = serve(l, 1, |c, _| {
        read_req(c);
        let _ = c.write_all(b"HTTP/1.0 200 OK\r\n\r\n");
        for _ in 0..8 {
            if c.write_all(&[b'x'; 1024]).is_err() {
                break;
            }
        }
    });
    let fd = connect_tcp("127.0.0.1", port, 500).unwrap();
    let r = exchange(fd, req.as_bytes(), 2048, 500);
    assert!(r.status == 200 && r.truncated && body(&r).len() == 2047 - 19);
    h.join().unwrap();

    // refused: nothing listens on the port just freed
    let (l, port) = listen();
    drop(l);
    assert!(connect_tcp("127.0.0.1", port, 300).is_none());
    assert!(connect_tcp("not-an-ip", 80, 100).is_none());
}

#[test]
fn unix_socket() {
    let path = format!("/tmp/http_test.{}.sock", std::process::id());
    let _ = std::fs::remove_file(&path);
    let l = UnixListener::bind(&path).unwrap();
    let h = std::thread::spawn(move || {
        let (mut c, _) = l.accept().unwrap();
        let mut b = [0u8; 1024];
        let _ = std::io::Read::read(&mut c, &mut b);
        let _ = c.write_all(b"HTTP/1.0 200 OK\r\n\r\nhello");
    });
    let req = build("GET", "/", "x", "", None, 256).unwrap();
    let fd = connect_unix(&path).expect("unix connect");
    let r = exchange(fd, req.as_bytes(), 4096, 500);
    assert!(r.status == 200 && body(&r) == b"hello");
    h.join().unwrap();
    std::fs::remove_file(&path).unwrap();
    assert!(connect_unix(&path).is_none());
}

#[test]
fn agent_goes_to_u60ds_v2_api_without_a_login() {
    let (l, port) = listen();
    let h = serve(l, 1, |c, _| {
        let rq = read_req(c);
        let r = if rq.starts_with("POST /v2/api/x HTTP/1.0\r\n") && !rq.contains("Authorization") && rq.contains("\r\n\r\n{\"x\":1}") {
            "HTTP/1.0 200 OK\r\n\r\n{\"done\":1}"
        } else {
            "HTTP/1.0 500 X\r\n\r\n"
        };
        let _ = c.write_all(r.as_bytes());
    });
    let mut a = CoreApi::default();
    a.config(port);
    let (code, b) = a.api("POST", "/api/x", Some("{\"x\":1}"), 4096);
    assert_eq!(code, 200);
    assert_eq!(b.as_deref(), Some(&b"{\"done\":1}"[..]));
    h.join().unwrap();

    // unreachable: 0 and no body
    let (l, port) = listen();
    drop(l);
    a.config(port);
    assert_eq!(a.api("GET", "/api/y", None, 4096), (0, None));
}

#[test]
fn a_dripping_peer_cannot_extend_the_total_read_budget() {
    let (listener, port) = listen();
    let h = serve(listener, 1, |c, _| {
        read_req(c);
        for _ in 0..20 {
            if c.write_all(b"x").is_err() { break; }
            std::thread::sleep(Duration::from_millis(25));
        }
    });
    let fd = connect_tcp("127.0.0.1", port, 500).unwrap();
    let at = Instant::now();
    let _ = exchange(fd, b"GET / HTTP/1.0\r\n\r\n", 4096, 120);
    assert!(at.elapsed() < Duration::from_millis(350), "drip extended the read budget");
    h.join().unwrap();
}
