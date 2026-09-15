//! Read-only status + log endpoints for sidecar services
//! the user installed on the device:
//!
//!   • Tailscale  — daemon at /data/tailscale/tailscaled, socket
//!                  /tmp/tailscaled.sock, log /data/tailscaled.log.
//!   • ShellClash — mihomo runtime renamed `CrashCore`, REST API on
//!                  127.0.0.1:9999, log /tmp/ShellCrash/ShellCrash.log.
//!
//! Endpoints never mutate state — purely observability.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde_json::{json, Value};

use crate::handlers::AppState;

const TS_BIN: &str = "/data/tailscale/tailscale";
const TS_SOCKET: &str = "/tmp/tailscaled.sock";
const TS_LOG: &str = "/data/tailscaled.log";

const SC_API: &str = "http://127.0.0.1:9999";
const SC_PID_FILE: &str = "/tmp/ShellCrash/shellcrash.pid";
const SC_LOG: &str = "/tmp/ShellCrash/ShellCrash.log";
const SC_CONFIG: &str = "/tmp/ShellCrash/config.yaml";

const HTTP_TIMEOUT: Duration = Duration::from_secs(3);
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
 *  ShellClash (mihomo)
 * -------------------------------------------------------------- */

/// GET /api/services/shellcrash
pub fn shellcrash_status(_state: &AppState) -> (u16, Value) {
    let pid = read_pid_file(SC_PID_FILE);
    let alive_from_pid = pid
        .map(|p| Path::new(&format!("/proc/{p}")).exists())
        .unwrap_or(false);
    let running = alive_from_pid || pgrep("CrashCore");

    if !running {
        return (
            200,
            json!({"ok": true, "data": {
                "installed": Path::new(SC_CONFIG).exists() || Path::new("/etc/ShellCrash").exists(),
                "running": false,
                "pid": pid,
            }}),
        );
    }

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .build()
        .into();

    let version = http_get_json(&agent, &format!("{SC_API}/version"));
    let memory = http_get_json(&agent, &format!("{SC_API}/memory"));
    let configs = http_get_json(&agent, &format!("{SC_API}/configs"));
    let connections = http_get_json(&agent, &format!("{SC_API}/connections"));
    let proxies_raw = http_get_json(&agent, &format!("{SC_API}/proxies"));

    // Boil the proxies map down to per-group summaries; full payload can
    // be very large with hundreds of nodes.
    let groups = summarize_proxies(&proxies_raw);

    let conn_count = connections
        .as_ref()
        .and_then(|v| v.get("connections"))
        .and_then(Value::as_array)
        .map(|a| a.len())
        .unwrap_or(0);
    let conn_total_up = connections
        .as_ref()
        .and_then(|v| v.get("uploadTotal"))
        .and_then(Value::as_i64);
    let conn_total_down = connections
        .as_ref()
        .and_then(|v| v.get("downloadTotal"))
        .and_then(Value::as_i64);

    let mode = configs.as_ref().and_then(|v| v.get("mode")).cloned();
    let log_level = configs.as_ref().and_then(|v| v.get("log-level")).cloned();
    let port = configs.as_ref().and_then(|v| v.get("port")).cloned();
    let socks_port = configs
        .as_ref()
        .and_then(|v| v.get("socks-port"))
        .cloned();
    let mixed_port = configs
        .as_ref()
        .and_then(|v| v.get("mixed-port"))
        .cloned();
    let tun_enable = configs
        .as_ref()
        .and_then(|v| v.get("tun"))
        .and_then(|t| t.get("enable"))
        .and_then(Value::as_bool);

    let started = read_first_line("/tmp/ShellCrash/crash_start_time");

    (
        200,
        json!({"ok": true, "data": {
            "installed": true,
            "running": true,
            "pid": pid,
            "version": version.as_ref().and_then(|v| v.get("version")).and_then(Value::as_str),
            "premium": version.as_ref().and_then(|v| v.get("premium")).and_then(Value::as_bool),
            "meta": version.as_ref().and_then(|v| v.get("meta")).and_then(Value::as_bool),
            "started_at_unix": started.and_then(|s| s.trim().parse::<i64>().ok()),
            "memory": memory.as_ref().and_then(|v| v.get("inuse")).and_then(Value::as_i64),
            "memory_oslimit": memory.as_ref().and_then(|v| v.get("oslimit")).and_then(Value::as_i64),
            "config": {
                "mode": mode,
                "log_level": log_level,
                "http_port": port,
                "socks_port": socks_port,
                "mixed_port": mixed_port,
                "tun_enable": tun_enable,
                "external_controller": SC_API.trim_start_matches("http://"),
            },
            "connections": {
                "active": conn_count,
                "upload_total": conn_total_up,
                "download_total": conn_total_down,
            },
            "groups": groups,
        }}),
    )
}

/// GET /api/services/shellcrash/log?lines=N
pub fn shellcrash_log(query: &str) -> (u16, Value) {
    let n = parse_lines(query);
    let lines = tail_file(SC_LOG, n).unwrap_or_default();
    (
        200,
        json!({"ok": true, "data": {
            "path": SC_LOG,
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

fn read_pid_file(path: &str) -> Option<i64> {
    let s = std::fs::read_to_string(path).ok()?;
    s.trim().parse::<i64>().ok()
}

fn read_first_line(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
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

fn http_get_json(agent: &ureq::Agent, url: &str) -> Option<Value> {
    let mut resp = agent.get(url).call().ok()?;
    if resp.status().as_u16() >= 400 {
        return None;
    }
    resp.body_mut().read_json::<Value>().ok()
}

/// Reduce mihomo's `/proxies` response (which can list every individual
/// proxy node) into a small per-group summary the UI can render.
fn summarize_proxies(raw: &Option<Value>) -> Vec<Value> {
    let Some(proxies) = raw.as_ref().and_then(|v| v.get("proxies")).and_then(Value::as_object)
    else {
        return Vec::new();
    };

    let mut groups = Vec::new();
    for (name, info) in proxies.iter() {
        let kind = info.get("type").and_then(Value::as_str).unwrap_or("");
        // Only return "selectable" group types — skip raw nodes.
        if !matches!(
            kind,
            "Selector" | "URLTest" | "Fallback" | "LoadBalance" | "Relay"
        ) {
            continue;
        }
        let now = info.get("now").and_then(Value::as_str);
        let all_count = info
            .get("all")
            .and_then(Value::as_array)
            .map(|a| a.len())
            .unwrap_or(0);
        groups.push(json!({
            "name": name,
            "type": kind,
            "now": now,
            "size": all_count,
            "udp": info.get("udp").and_then(Value::as_bool).unwrap_or(false),
        }));
    }

    // Stable order by name so the UI doesn't jump.
    groups.sort_by(|a, b| {
        let an = a.get("name").and_then(Value::as_str).unwrap_or("");
        let bn = b.get("name").and_then(Value::as_str).unwrap_or("");
        an.cmp(bn)
    });
    groups
}
