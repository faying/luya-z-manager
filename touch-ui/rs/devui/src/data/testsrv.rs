//! Tiny local servers for the data modules' tests (the C tests fork; these are threads).
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::JoinHandle;

/// Read one request: the headers and a Content-Length body.
pub fn read_req(c: &mut TcpStream) -> String {
    let mut rq = Vec::new();
    let mut b = [0u8; 4096];
    loop {
        let n = match c.read(&mut b) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        rq.extend_from_slice(&b[..n]);
        if let Some(h) = rq.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&rq[..h]).to_string();
            let want = head
                .lines()
                .find_map(|l| l.strip_prefix("Content-Length: "))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if rq.len() >= h + 4 + want {
                break;
            }
        }
    }
    String::from_utf8_lossy(&rq).into_owned()
}

/// A listener on a free loopback port.
pub fn listen() -> (TcpListener, u16) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let p = l.local_addr().unwrap().port();
    (l, p)
}

/// Serve `conns` connections with `h(conn, i)`, then stop.
pub fn serve(l: TcpListener, conns: usize, h: impl Fn(&mut TcpStream, usize) + Send + 'static) -> JoinHandle<()> {
    std::thread::spawn(move || {
        for i in 0..conns {
            let Ok((mut c, _)) = l.accept() else { return };
            h(&mut c, i);
            let _ = c.shutdown(std::net::Shutdown::Both);
        }
    })
}

/// One canned answer per connection, by index (the last repeats); records the requests.
pub fn canned(answers: Vec<String>, conns: usize) -> (u16, std::sync::Arc<std::sync::Mutex<Vec<String>>>, JoinHandle<()>) {
    let (l, port) = listen();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let s2 = seen.clone();
    let h = serve(l, conns, move |c, i| {
        let rq = read_req(c);
        s2.lock().unwrap().push(rq);
        let a = &answers[i.min(answers.len() - 1)];
        let _ = c.write_all(a.as_bytes());
    });
    (port, seen, h)
}

/// "HTTP/1.0 <status> X\r\n\r\n<body>"
pub fn resp(status: i32, body: &str) -> String {
    format!("HTTP/1.0 {status} X\r\n\r\n{body}")
}
