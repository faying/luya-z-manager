//! Tailscale's page reads: its status and its log, on the long-standing paths with the
//! `{ok, data}` reply shape. On demand, off the runtime thread
//! (spawn_blocking with a timeout), nothing cached.
//!
//! The log tail is shared with CHILL's log (`chill_page.rs`), as in the agent.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

use axum::{
    extract::{RawQuery, State},
    response::Response,
};
use serde_json::{Value, json};

use super::{
    Compat,
    home::{TS_BIN, TS_SOCKET, TS_TIMEOUT, ts_exec},
    reply,
};

/// tailscaled's own log (appended, not rotated).
const TS_LOG: &str = "/data/tailscaled.log";
/// Log lines answered: `lines=` asks for 1 to [`LOG_LINES_MAX`], without it [`LOG_LINES_DEFAULT`].
const LOG_LINES_DEFAULT: usize = 200;
const LOG_LINES_MAX: usize = 2000;
/// At most this much of a log's end is read : a long-lined log
/// can answer fewer lines than asked.
const LOG_TAIL_BYTES: u64 = 512 * 1024;
/// The whole read, beyond the CLI's own limit.
const READ_TIMEOUT: Duration = Duration::from_secs(8);

/// The line count a log query asks for: the first `lines=N` or `n=N` that is a number, clamped
/// to 1..=[`LOG_LINES_MAX`]; none = [`LOG_LINES_DEFAULT`].
pub(super) fn parse_lines(query: &str) -> usize {
    query
        .trim_start_matches('?')
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .filter(|(k, _)| matches!(*k, "lines" | "n"))
        .find_map(|(_, v)| v.parse::<usize>().ok())
        .map_or(LOG_LINES_DEFAULT, |n| n.clamp(1, LOG_LINES_MAX))
}

/// The last `n` lines of `path`, read from at most its last `max_bytes` (a partial first line
/// dropped). Missing file: empty.
pub(super) fn tail_file(path: &Path, n: usize, max_bytes: u64) -> Vec<String> {
    let Ok(mut f) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(max_bytes);
    if start > 0 && f.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    if f.take(max_bytes).read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<&str> = text.lines().collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    let skip = lines.len().saturating_sub(n);
    lines[skip..].iter().map(|l| l.to_string()).collect()
}

/// `{path, lines, limit}` for a log at device path `device`, read under the compat root.
pub(super) async fn log_reply(c: &Compat, device: &'static str, query: Option<String>) -> Response {
    let n = parse_lines(query.as_deref().unwrap_or(""));
    let p = c.path(device);
    let job = tokio::task::spawn_blocking(move || tail_file(&p, n, LOG_TAIL_BYTES));
    let lines = match tokio::time::timeout(READ_TIMEOUT, job).await {
        Ok(Ok(l)) => l,
        _ => Vec::new(),
    };
    reply(
        200,
        json!({"ok": true, "data": {"path": device, "lines": lines, "limit": n}}),
    )
}

/// GET /api/services/tailscale/log?lines=N
pub async fn log(State(c): State<Compat>, RawQuery(q): RawQuery) -> Response {
    log_reply(&c, TS_LOG, q).await
}

/// Whether a process named `name` runs, from `<proc>/*/comm`.
pub(super) fn running(proc: &Path, name: &str) -> bool {
    std::fs::read_dir(proc).is_ok_and(|rd| {
        rd.filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|s| s.bytes().all(|b| b.is_ascii_digit()))
            })
            .any(|e| std::fs::read_to_string(e.path().join("comm")).is_ok_and(|s| s.trim() == name))
    })
}

fn text<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn flag(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// A list field as it is, `[]` when missing.
fn list(v: &Value, key: &str) -> Value {
    v.get(key).cloned().unwrap_or_else(|| json!([]))
}

/// Peers listed in full; the counts cover all of them.
const PEERS_LISTED: usize = 32;

/// `tailscale status --json` boiled down for the page: this node, the peer counts, the exit node
/// in use and the first [`PEERS_LISTED`] peers.
pub(super) fn status_data(parsed: &Value, running: bool) -> Value {
    let me = parsed.get("Self").unwrap_or(&Value::Null);
    // a "0001-…" date (or none) means key expiry is off for this node
    let key_expiry = text(me, "KeyExpiry").filter(|k| k.len() >= 10 && !k.starts_with("0001"));

    let mut counts = (0u64, 0u64, 0u64, 0u64); // (all, online, active, active and direct)
    let mut exit_node = Value::Null;
    let mut listed = Vec::new();
    let peers = parsed.get("Peer").and_then(Value::as_object);
    for p in peers.into_iter().flat_map(|m| m.values()) {
        let online = flag(p, "Online");
        let active = flag(p, "Active");
        let is_exit = flag(p, "ExitNode");
        // an address = a direct path; none = relayed through DERP
        let cur_addr = text(p, "CurAddr").filter(|a| !a.is_empty());
        counts.0 += 1;
        counts.1 += u64::from(online);
        counts.2 += u64::from(active);
        counts.3 += u64::from(active && cur_addr.is_some());
        if is_exit {
            exit_node = json!({"hostname": text(p, "HostName"), "ips": list(p, "TailscaleIPs"),
                "online": online});
        }
        if listed.len() >= PEERS_LISTED {
            continue;
        }
        listed.push(json!({
            "id": text(p, "ID"),
            "hostname": text(p, "HostName"),
            "dns_name": text(p, "DNSName"),
            "os": text(p, "OS"),
            "ips": list(p, "TailscaleIPs"),
            "online": online,
            "exit_node": is_exit,
            "active": active,
            "cur_addr": cur_addr,
            "relay": text(p, "Relay").filter(|r| !r.is_empty()),
            "rx_bytes": p.get("RxBytes").and_then(Value::as_i64),
            "tx_bytes": p.get("TxBytes").and_then(Value::as_i64),
            "last_seen": text(p, "LastSeen"),
            "last_handshake": text(p, "LastHandshake"),
            "primary_routes": list(p, "PrimaryRoutes"),
        }));
    }

    let (peer_count, peer_online, peer_active, peer_direct) = counts;
    json!({
        "installed": true,
        "running": running,
        "version": text(parsed, "Version"),
        "backend_state": text(parsed, "BackendState").unwrap_or("Unknown"),
        "auth_url": text(parsed, "AuthURL").filter(|u| !u.is_empty()),
        "self": {
            "hostname": text(me, "HostName"),
            "dns_name": text(me, "DNSName"),
            "ips": list(me, "TailscaleIPs"),
            "online": flag(me, "Online"),
            "relay": text(me, "Relay"),
            "exit_node_option": flag(me, "ExitNodeOption"),
            "key_expiry": key_expiry,
            "primary_routes": list(me, "PrimaryRoutes"),
        },
        "exit_node": exit_node,
        "peers": listed,
        "peer_count": peer_count,
        "peer_online": peer_online,
        "peer_active": peer_active,
        "peer_direct": peer_direct,
    })
}

/// The whole reply's `data` (blocking: the CLI runs here).
pub(super) fn status_blocking(bin: &Path, sock: &Path, proc: &Path, limit: Duration) -> Value {
    if !bin.exists() {
        return json!({"installed": false, "running": false});
    }
    let running = sock.exists() && running(proc, "tailscaled");
    let raw = match ts_exec(bin, sock, limit) {
        Ok(raw) => raw,
        Err(e) => return json!({"installed": true, "running": running, "error": e}),
    };
    match serde_json::from_slice::<Value>(&raw) {
        Ok(parsed) => status_data(&parsed, running),
        Err(_) => json!({"installed": true, "running": running,
            "error": "tailscale status gave output we cannot read"}),
    }
}

/// GET /api/services/tailscale
pub async fn status(State(c): State<Compat>) -> Response {
    let (bin, sock, proc) = (c.path(TS_BIN), c.path(TS_SOCKET), c.path("/proc"));
    let job = tokio::task::spawn_blocking(move || status_blocking(&bin, &sock, &proc, TS_TIMEOUT));
    match tokio::time::timeout(TS_TIMEOUT + Duration::from_secs(2), job).await {
        Ok(Ok(data)) => reply(200, json!({"ok": true, "data": data})),
        _ => reply(
            200,
            json!({"ok": true, "data": {"installed": true, "running": false,
                "error": "exec failed: timed out"}}),
        ),
    }
}
