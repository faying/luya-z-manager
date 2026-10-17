//! Tailscale's page reads moved from the agent (`services.rs`): its status and its log, at the
//! agent's paths with the agent's reply shape (`{ok, data}`). On demand, off the runtime thread
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

use super::home::{TS_BIN, TS_SOCKET, TS_TIMEOUT, ts_exec};
use super::{Compat, reply};

const TS_LOG: &str = "/data/tailscaled.log";
const LOG_TAIL_DEFAULT: usize = 200;
const LOG_TAIL_MAX: usize = 2000;
/// At most this much of a log's end is read (the agent streamed the whole file): a long-lined log
/// can answer fewer lines than asked.
const LOG_TAIL_BYTES: u64 = 512 * 1024;
/// The whole read, beyond the CLI's own limit.
const READ_TIMEOUT: Duration = Duration::from_secs(8);

/// The agent's `parse_lines`: `lines=N` (or `n=N`), clamped to 1..=2000; else 200.
pub(super) fn parse_lines(query: &str) -> usize {
    for kv in query.trim_start_matches('?').split('&') {
        let mut it = kv.splitn(2, '=');
        if let (Some(k), Some(v)) = (it.next(), it.next())
            && (k == "lines" || k == "n")
            && let Ok(n) = v.parse::<usize>()
        {
            return n.clamp(1, LOG_TAIL_MAX);
        }
    }
    LOG_TAIL_DEFAULT
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

/// Whether a process named `name` runs (the agent's `pidof`), from `<proc>/*/comm`.
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

/// The agent's `tailscale_status` summary of `tailscale status --json`.
pub(super) fn status_data(parsed: &Value, running: bool) -> Value {
    let backend_state = parsed
        .get("BackendState")
        .and_then(Value::as_str)
        .unwrap_or("Unknown");
    let version = parsed.get("Version").and_then(Value::as_str);
    let auth_url = parsed.get("AuthURL").and_then(Value::as_str);

    let self_node = parsed.get("Self").cloned().unwrap_or(Value::Null);
    let ips = self_node
        .get("TailscaleIPs")
        .cloned()
        .unwrap_or_else(|| json!([]));
    // "0001-…" or missing: key expiry is disabled for this node
    let key_expiry = self_node
        .get("KeyExpiry")
        .and_then(Value::as_str)
        .filter(|k| k.len() >= 10 && !k.starts_with("0001"));
    let primary_routes = self_node
        .get("PrimaryRoutes")
        .cloned()
        .unwrap_or_else(|| json!([]));

    let (mut peer_count, mut peer_online, mut peer_active, mut peer_direct) = (0, 0, 0, 0);
    let mut peers_summary: Vec<Value> = Vec::new();
    let mut exit_node: Option<Value> = None;
    if let Some(peers) = parsed.get("Peer").and_then(Value::as_object) {
        for peer in peers.values() {
            peer_count += 1;
            let is_online = peer.get("Online").and_then(Value::as_bool).unwrap_or(false);
            if is_online {
                peer_online += 1;
            }
            let active = peer.get("Active").and_then(Value::as_bool).unwrap_or(false);
            // CurAddr set = hole-punched; empty = through a DERP relay
            let cur_addr = peer
                .get("CurAddr")
                .and_then(Value::as_str)
                .filter(|a| !a.is_empty());
            if active {
                peer_active += 1;
                if cur_addr.is_some() {
                    peer_direct += 1;
                }
            }
            let is_exit = peer
                .get("ExitNode")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let peer_ips = peer
                .get("TailscaleIPs")
                .cloned()
                .unwrap_or_else(|| json!([]));
            if is_exit {
                exit_node = Some(json!({
                    "hostname": peer.get("HostName").and_then(Value::as_str),
                    "ips": peer_ips,
                    "online": is_online,
                }));
            }
            if peers_summary.len() < 32 {
                peers_summary.push(json!({
                    "id": peer.get("ID").and_then(Value::as_str),
                    "hostname": peer.get("HostName").and_then(Value::as_str),
                    "dns_name": peer.get("DNSName").and_then(Value::as_str),
                    "os": peer.get("OS").and_then(Value::as_str),
                    "ips": peer_ips,
                    "online": is_online,
                    "exit_node": is_exit,
                    "rx_bytes": peer.get("RxBytes").and_then(Value::as_i64),
                    "tx_bytes": peer.get("TxBytes").and_then(Value::as_i64),
                    "last_seen": peer.get("LastSeen").and_then(Value::as_str),
                    "last_handshake": peer.get("LastHandshake").and_then(Value::as_str),
                    "active": active,
                    "cur_addr": cur_addr,
                    "relay": peer.get("Relay").and_then(Value::as_str).filter(|r| !r.is_empty()),
                    "primary_routes": peer.get("PrimaryRoutes").cloned().unwrap_or_else(|| json!([])),
                }));
            }
        }
    }

    json!({
        "installed": true,
        "running": running,
        "backend_state": backend_state,
        "version": version,
        "auth_url": if auth_url == Some("") { None } else { auth_url },
        "self": {
            "hostname": self_node.get("HostName").and_then(Value::as_str),
            "dns_name": self_node.get("DNSName").and_then(Value::as_str),
            "ips": ips,
            "online": self_node.get("Online").and_then(Value::as_bool).unwrap_or(false),
            "relay": self_node.get("Relay").and_then(Value::as_str),
            "exit_node_option": self_node.get("ExitNodeOption").and_then(Value::as_bool).unwrap_or(false),
            "key_expiry": key_expiry,
            "primary_routes": primary_routes,
        },
        "peer_active": peer_active,
        "peer_direct": peer_direct,
        "exit_node": exit_node,
        "peer_count": peer_count,
        "peer_online": peer_online,
        "peers": peers_summary,
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
            "error": "could not parse tailscale status output"}),
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
