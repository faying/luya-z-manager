//! Who changed CHILL's exit or profile. 11-10 17:00 the main device went to 性能 + 全部直连
//! and back on 11-11 09:30; nobody had done it by hand and nothing recorded the caller, so it
//! could not be traced. Each write through `/api/services/chill/exit` and `.../profile` now
//! records its caller:
//!
//! - the source header luyaz-web, the MCP route, the scenario engine and the scheduler set
//!   ([`luyaz_api::headers::SOURCE_HEADER`]), the browser's address luyaz-web forwards
//!   ([`luyaz_api::headers::CLIENT_HEADER`]) and the restore flag;
//! - the User-Agent when one is sent (curl and scripts send theirs);
//! - for a loopback connection, the process at the other end: its port → the socket inode in
//!   `/proc/net/tcp{,6}` → the `/proc/<pid>/fd` that holds it → `comm`. The touch screen sends no
//!   header, so before this "no header on loopback" meant "the screen" and a shell `curl` looked
//!   the same; now it reads `luyaz-ui` or `curl`. Only done on these writes (a few a day).
//!
//! Lines go to CHILL's event timeline (`/data/chill/events.log`, beside chill.sh's own; chill.sh
//! keeps its last 20) and to [`LOG`] (the last [`KEEP`]), and the core log.

use axum::http::HeaderMap;
use std::io::Write;
use std::net::SocketAddr;

/// The core's own record of CHILL writes (survives reboots; chill.sh's timeline is short).
pub const LOG: &str = "/data/luyaz/core/chill-writes.log";
/// chill.sh's event timeline (`EVENTS` there).
pub const EVENTS: &str = "/data/chill/events.log";
/// Lines kept in [`LOG`].
pub const KEEP: usize = 200;

/// Printable ASCII, no spaces at the ends, at most `n` characters (headers come from callers).
fn clean(s: &str, n: usize) -> String {
    s.chars()
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .take(n)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// The caller in one line, e.g. `web client 192.168.0.10 via luyaz-web[812]`,
/// `luyaz-ui[6669]`, `curl[2210] ua curl/8.9.1`, `lan 192.168.0.10`.
pub fn caller(root: &str, headers: &HeaderMap, peer: Option<SocketAddr>) -> String {
    let h = |k: &str| {
        headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .map(|v| clean(v, 48))
    };
    let mut parts: Vec<String> = Vec::new();
    if let Some(s) = h(luyaz_api::headers::SOURCE_HEADER).filter(|s| !s.is_empty()) {
        parts.push(s);
    }
    if headers.contains_key(luyaz_api::headers::RESTORE_HEADER) {
        parts.push("restore".into());
    }
    if let Some(c) = h(luyaz_api::headers::CLIENT_HEADER).filter(|s| !s.is_empty()) {
        parts.push(format!("client {c}"));
    }
    match peer {
        Some(p) if p.ip().is_loopback() => match process_on(root, p.port()) {
            Some(proc) if parts.is_empty() => parts.push(proc),
            Some(proc) => parts.push(format!("via {proc}")),
            None => parts.push(format!("loopback port {}", p.port())),
        },
        Some(p) => parts.push(format!("lan {}", p.ip())),
        None => parts.push("no peer".into()),
    }
    if let Some(ua) = h("user-agent").filter(|s| !s.is_empty()) {
        parts.push(format!("ua {ua}"));
    }
    parts.join(" ")
}

/// The inode of the established TCP socket whose local port is `port`, from `/proc/net/tcp`
/// and `tcp6` (the client end of a loopback connection to us).
fn inode_of(root: &str, port: u16) -> Option<String> {
    let want = format!(":{port:04X}");
    for f in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(t) = std::fs::read_to_string(format!("{root}{f}")) else {
            continue;
        };
        for line in t.lines().skip(1) {
            let c: Vec<&str> = line.split_whitespace().collect();
            // sl local rem st tx:rx tr:when retrnsmt uid timeout inode
            if c.len() > 9 && c[1].ends_with(&want) && c[3] == "01" && c[9] != "0" {
                return Some(c[9].to_owned());
            }
        }
    }
    None
}

/// `comm[pid]` of the process holding the client end of a loopback connection from `port`.
pub fn process_on(root: &str, port: u16) -> Option<String> {
    let inode = inode_of(root, port)?;
    let target = format!("socket:[{inode}]");
    let procs = std::fs::read_dir(format!("{root}/proc")).ok()?;
    for p in procs.flatten() {
        let name = p.file_name();
        let Some(pid) = name
            .to_str()
            .filter(|s| s.bytes().all(|b| b.is_ascii_digit()))
        else {
            continue;
        };
        let Ok(fds) = std::fs::read_dir(p.path().join("fd")) else {
            continue;
        };
        let holds = fds.flatten().any(|fd| {
            std::fs::read_link(fd.path()).is_ok_and(|l| l.as_os_str() == target.as_str())
        });
        if holds {
            let comm = std::fs::read_to_string(p.path().join("comm")).unwrap_or_default();
            return Some(format!("{}[{pid}]", clean(&comm, 16)));
        }
    }
    None
}

/// Record one write: `what` (`exit` / `profile`), the value asked, the result (`ok` or why not)
/// and [`caller`]'s line. Never fails the request.
pub fn record(root: &str, wall: i64, what: &str, value: &str, result: &str, who: &str) {
    let ts = crate::ops::record::civil(wall.max(0) as u64).replacen(' ', "T", 1) + "Z";
    let value = clean(value, 24);
    let line = format!("{ts} {what} -> {value} ({result}) by {who}");
    eprintln!("[chill-who] {line}");
    let append = |p: &str| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(format!("{root}{p}"))
            .and_then(|mut f| writeln!(f, "{line}"))
    };
    let _ = append(EVENTS);
    if append(LOG).is_ok() {
        let p = format!("{root}{LOG}");
        if let Ok(t) = std::fs::read_to_string(&p) {
            let n = t.lines().count();
            if n > KEEP + KEEP / 4 {
                let kept: Vec<&str> = t.lines().skip(n - KEEP).collect();
                let _ = crate::api::ops::atomic_write(
                    std::path::Path::new(&p),
                    (kept.join("\n") + "\n").as_bytes(),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn root(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("chill-who-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("proc/net")).unwrap();
        d
    }

    const TCP_HEAD: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n";

    #[test]
    fn a_loopback_caller_is_named_by_the_process_holding_its_socket() {
        let d = root("proc");
        // the client end 127.0.0.1:51234 -> :9460, established, inode 4242
        std::fs::write(
            d.join("proc/net/tcp"),
            format!("{TCP_HEAD}   0: 0100007F:C822 0100007F:24F4 01 00000000:00000000 00:00000000 00000000     0        0 4242 1\n"),
        )
        .unwrap();
        std::fs::create_dir_all(d.join("proc/6669/fd")).unwrap();
        std::fs::write(d.join("proc/6669/comm"), "luyaz-ui\n").unwrap();
        std::os::unix::fs::symlink("socket:[4242]", d.join("proc/6669/fd/7")).unwrap();
        std::fs::create_dir_all(d.join("proc/77/fd")).unwrap();
        std::os::unix::fs::symlink("socket:[1]", d.join("proc/77/fd/3")).unwrap();
        let r = d.to_str().unwrap();
        let peer: SocketAddr = "127.0.0.1:51234".parse().unwrap();
        assert_eq!(caller(r, &HeaderMap::new(), Some(peer)), "luyaz-ui[6669]");

        let mut h = HeaderMap::new();
        h.insert(
            luyaz_api::headers::SOURCE_HEADER,
            HeaderValue::from_static("web"),
        );
        h.insert(
            luyaz_api::headers::CLIENT_HEADER,
            HeaderValue::from_static("192.168.0.10"),
        );
        assert_eq!(
            caller(r, &h, Some(peer)),
            "web client 192.168.0.10 via luyaz-ui[6669]"
        );

        let mut h = HeaderMap::new();
        h.insert("user-agent", HeaderValue::from_static("curl/8.9.1"));
        let other: SocketAddr = "127.0.0.1:40000".parse().unwrap();
        assert_eq!(
            caller(r, &h, Some(other)),
            "loopback port 40000 ua curl/8.9.1"
        );
        let lan: SocketAddr = "192.168.0.10:5000".parse().unwrap();
        assert_eq!(caller(r, &HeaderMap::new(), Some(lan)), "lan 192.168.0.10");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_write_lands_in_both_logs_and_the_core_log_is_capped() {
        let d = root("log");
        std::fs::create_dir_all(d.join("data/chill")).unwrap();
        std::fs::create_dir_all(d.join("data/luyaz/core")).unwrap();
        let r = d.to_str().unwrap();
        // 2026-11-10 17:01:00 (the device clock reads local time)
        record(
            r,
            1_794_330_060,
            "exit",
            "direct_all",
            "ok",
            "luyaz-ui[6669]",
        );
        let ev = std::fs::read_to_string(d.join("data/chill/events.log")).unwrap();
        assert_eq!(
            ev,
            "2026-11-10T17:01:00Z exit -> direct_all (ok) by luyaz-ui[6669]\n"
        );
        for i in 0..(KEEP + KEEP / 4 + 1) {
            record(r, 1_794_330_060 + i as i64, "profile", "perf", "ok", "x");
        }
        let n = std::fs::read_to_string(d.join("data/luyaz/core/chill-writes.log"))
            .unwrap()
            .lines()
            .count();
        assert!((KEEP..=KEEP + KEEP / 4).contains(&n), "{n}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
