//! Read-only status + log endpoints for sidecar services
//! the user installed on the device:
//!
//!   • Tailscale  — daemon at /data/tailscale/tailscaled, socket
//!                  /tmp/tailscaled.sock, log /data/tailscaled.log.
//!
//! CHILL (the native-mihomo proxy service) has its own module, chill.rs — it
//! mutates state (region/provider/bypass switches) so it doesn't fit this
//! read-only-only file.
//!
//! Endpoints never mutate state — purely observability.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::Command;

use serde_json::{json, Value};

use crate::handlers::AppState;

const TS_BIN: &str = "/data/tailscale/tailscale";
const TS_SOCKET: &str = "/tmp/tailscaled.sock";
const TS_LOG: &str = "/data/tailscaled.log";

const LOG_TAIL_DEFAULT: usize = 200;
const LOG_TAIL_MAX: usize = 2000;

/* -------------------------------------------------------------- *
 *  Tailscale
 * -------------------------------------------------------------- */

/// GET /api/services/tailscale
pub fn tailscale_status(_state: &AppState) -> (u16, Value) {
    if !Path::new(TS_BIN).exists() {
        return (
            200,
            json!({"ok": true, "data": {
                "installed": false,
                "running": false,
            }}),
        );
    }

    let running = Path::new(TS_SOCKET).exists() && pgrep("tailscaled");

    let raw = match Command::new(TS_BIN)
        .args(["--socket", TS_SOCKET, "status", "--json"])
        .output()
    {
        Ok(o) if o.status.success() => o.stdout,
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr).into_owned();
            return (
                200,
                json!({"ok": true, "data": {
                    "installed": true,
                    "running": running,
                    "error": stderr.trim(),
                }}),
            );
        }
        Err(e) => {
            return (
                200,
                json!({"ok": true, "data": {
                    "installed": true,
                    "running": running,
                    "error": format!("exec failed: {e}"),
                }}),
            )
        }
    };

    let parsed: Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(_) => {
            return (
                200,
                json!({"ok": true, "data": {
                    "installed": true,
                    "running": running,
                    "error": "could not parse tailscale status output",
                }}),
            )
        }
    };

    let backend_state = parsed
        .get("BackendState")
        .and_then(Value::as_str)
        .unwrap_or("Unknown");
    let version = parsed.get("Version").and_then(Value::as_str);
    let auth_url = parsed.get("AuthURL").and_then(Value::as_str);

    let self_node = parsed.get("Self").cloned().unwrap_or(Value::Null);
    let hostname = self_node.get("HostName").and_then(Value::as_str);
    let dns_name = self_node.get("DNSName").and_then(Value::as_str);
    let ips = self_node
        .get("TailscaleIPs")
        .cloned()
        .unwrap_or_else(|| Value::Array(vec![]));
    let online = self_node
        .get("Online")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let exit_node_option = self_node
        .get("ExitNodeOption")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let relay = self_node.get("Relay").and_then(Value::as_str);

    // Peers — summarize counts and a small list (limit so payload stays small)
    let mut peer_count = 0usize;
    let mut peer_online = 0usize;
    let mut peers_summary: Vec<Value> = Vec::new();
    let mut exit_node: Option<Value> = None;

    if let Some(peers) = parsed.get("Peer").and_then(Value::as_object) {
        for (_, peer) in peers.iter() {
            peer_count += 1;
            let is_online = peer
                .get("Online")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if is_online {
                peer_online += 1;
            }

            if peer
                .get("ExitNode")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                exit_node = Some(json!({
                    "hostname": peer.get("HostName").and_then(Value::as_str),
                    "ips": peer.get("TailscaleIPs").cloned().unwrap_or(Value::Array(vec![])),
                    "online": is_online,
                }));
            }

            if peers_summary.len() < 32 {
                peers_summary.push(json!({
                    "id": peer.get("ID").and_then(Value::as_str),
                    "hostname": peer.get("HostName").and_then(Value::as_str),
                    "dns_name": peer.get("DNSName").and_then(Value::as_str),
                    "os": peer.get("OS").and_then(Value::as_str),
                    "ips": peer.get("TailscaleIPs").cloned().unwrap_or(Value::Array(vec![])),
                    "online": is_online,
                    "exit_node": peer.get("ExitNode").and_then(Value::as_bool).unwrap_or(false),
                    "rx_bytes": peer.get("RxBytes").and_then(Value::as_i64),
                    "tx_bytes": peer.get("TxBytes").and_then(Value::as_i64),
                    "last_seen": peer.get("LastSeen").and_then(Value::as_str),
                    "last_handshake": peer.get("LastHandshake").and_then(Value::as_str),
                }));
            }
        }
    }

    (
        200,
        json!({"ok": true, "data": {
            "installed": true,
            "running": running,
            "backend_state": backend_state,
            "version": version,
            "auth_url": if auth_url == Some("") { None } else { auth_url },
            "self": {
                "hostname": hostname,
                "dns_name": dns_name,
                "ips": ips,
                "online": online,
                "relay": relay,
                "exit_node_option": exit_node_option,
            },
            "exit_node": exit_node,
            "peer_count": peer_count,
            "peer_online": peer_online,
            "peers": peers_summary,
        }}),
    )
}

/// GET /api/services/tailscale/log?lines=N
pub fn tailscale_log(query: &str) -> (u16, Value) {
    let n = parse_lines(query);
    let lines = tail_file(TS_LOG, n).unwrap_or_default();
    (
        200,
        json!({"ok": true, "data": {
            "path": TS_LOG,
            "lines": lines,
            "limit": n,
        }}),
    )
}

/* -------------------------------------------------------------- *
 *  Helpers
 * -------------------------------------------------------------- */

fn pgrep(name: &str) -> bool {
    Command::new("pidof")
        .arg(name)
        .output()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false)
}

fn parse_lines(query: &str) -> usize {
    for kv in query.trim_start_matches('?').split('&') {
        let mut it = kv.splitn(2, '=');
        if let (Some(k), Some(v)) = (it.next(), it.next()) {
            if k == "lines" || k == "n" {
                if let Ok(n) = v.parse::<usize>() {
                    return n.clamp(1, LOG_TAIL_MAX);
                }
            }
        }
    }
    LOG_TAIL_DEFAULT
}

/// Read up to `n` trailing lines from a text file.
///
/// Naive but fine for our log sizes (sub-megabyte). If the file is huge
/// this still works; it just streams from the start.
fn tail_file(path: &str, n: usize) -> std::io::Result<Vec<String>> {
    let f = File::open(path)?;
    let reader = BufReader::new(f);
    let mut buf: std::collections::VecDeque<String> = std::collections::VecDeque::with_capacity(n);
    for line in reader.lines() {
        let line = line.unwrap_or_default();
        if buf.len() == n {
            buf.pop_front();
        }
        buf.push_back(line);
    }
    Ok(buf.into_iter().collect())
}
