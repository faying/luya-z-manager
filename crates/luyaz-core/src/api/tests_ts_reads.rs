//! Tests for ts_reads.rs (batch 5b): Tailscale's status and log at the agent's paths.

use super::tests::{call, start};
use super::ts_reads::*;
use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

fn scratch(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("u60d-tsreads-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// A fake `tailscale` CLI at `<root>/data/tailscale/tailscale` running `body`.
fn fake_cli(root: &Path, body: &str) {
    let bin = root.join("data/tailscale/tailscale");
    fs::create_dir_all(bin.parent().unwrap()).unwrap();
    fs::write(&bin, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
}

fn fake_socket_and_daemon(root: &Path) {
    fs::create_dir_all(root.join("tmp")).unwrap();
    fs::write(root.join("tmp/tailscaled.sock"), "").unwrap();
    fs::create_dir_all(root.join("proc/42")).unwrap();
    fs::write(root.join("proc/42/comm"), "tailscaled\n").unwrap();
    fs::create_dir_all(root.join("proc/self")).unwrap();
    fs::write(root.join("proc/self/comm"), "tailscaled\n").unwrap();
}

#[test]
fn status_shape() {
    let mut peers = serde_json::Map::new();
    for i in 0..40 {
        peers.insert(
            format!("k{i:02}"),
            json!({"ID": format!("{i}"), "HostName": format!("h{i}"), "Online": i % 2 == 0,
                "Active": i < 3, "CurAddr": if i == 0 { "1.2.3.4:41641" } else { "" },
                "Relay": "", "ExitNode": i == 5, "TailscaleIPs": ["100.64.0.5"]}),
        );
    }
    let v = status_data(
        &json!({"BackendState": "Running", "Version": "1.102.4", "AuthURL": "",
            "Self": {"HostName": "u60", "DNSName": "u60.ts.net.", "TailscaleIPs": ["100.64.0.1"],
                "Online": true, "Relay": "", "ExitNodeOption": true,
                "KeyExpiry": "0001-01-01T00:00:00Z", "PrimaryRoutes": ["192.168.0.0/24"]},
            "Peer": peers}),
        true,
    );
    assert_eq!(v["installed"], true);
    assert_eq!(v["running"], true);
    assert_eq!(v["backend_state"], "Running");
    assert_eq!(v["auth_url"], Value::Null, "an empty AuthURL is null");
    assert_eq!(v["self"]["relay"], "", "self relay as the CLI says");
    assert_eq!(
        v["self"]["key_expiry"],
        Value::Null,
        "0001-… = expiry disabled"
    );
    assert_eq!(v["self"]["primary_routes"], json!(["192.168.0.0/24"]));
    assert_eq!(v["peer_count"], 40);
    assert_eq!(v["peer_online"], 20);
    assert_eq!(
        (v["peer_active"].clone(), v["peer_direct"].clone()),
        (json!(3), json!(1))
    );
    assert_eq!(
        v["peers"].as_array().unwrap().len(),
        32,
        "the list is capped"
    );
    assert_eq!(
        v["peers"][1]["relay"],
        Value::Null,
        "an empty peer relay is null"
    );
    assert_eq!(v["peers"][1]["cur_addr"], Value::Null);
    assert_eq!(v["exit_node"]["hostname"], "h5");
    let bare = status_data(&json!({}), false);
    assert_eq!(bare["backend_state"], "Unknown");
    assert_eq!(bare["self"]["ips"], json!([]));
    assert_eq!(bare["exit_node"], Value::Null);
}

#[test]
fn not_installed_failing_and_unparsable() {
    let root = scratch("cli");
    let (bin, sock, proc) = (
        root.join("data/tailscale/tailscale"),
        root.join("tmp/tailscaled.sock"),
        root.join("proc"),
    );
    let t = Duration::from_secs(5);
    assert_eq!(
        status_blocking(&bin, &sock, &proc, t),
        json!({"installed": false, "running": false})
    );
    fake_cli(
        &root,
        "echo 'failed to connect to local tailscaled' >&2; exit 1",
    );
    assert_eq!(
        status_blocking(&bin, &sock, &proc, t),
        json!({"installed": true, "running": false,
            "error": "failed to connect to local tailscaled"})
    );
    fake_socket_and_daemon(&root);
    fake_cli(&root, "echo 'not json'");
    assert_eq!(
        status_blocking(&bin, &sock, &proc, t),
        json!({"installed": true, "running": true,
            "error": "could not parse tailscale status output"})
    );
    fake_cli(
        &root,
        r#"echo '{"BackendState":"NeedsLogin","AuthURL":"https://login"}'"#,
    );
    let v = status_blocking(&bin, &sock, &proc, t);
    assert_eq!(
        (v["backend_state"].as_str(), v["auth_url"].as_str()),
        (Some("NeedsLogin"), Some("https://login"))
    );
}

#[test]
fn a_wedged_cli_is_killed() {
    let root = scratch("wedged");
    fake_cli(&root, "exec sleep 30");
    let t0 = Instant::now();
    let v = status_blocking(
        &root.join("data/tailscale/tailscale"),
        &root.join("tmp/tailscaled.sock"),
        &root.join("proc"),
        Duration::from_millis(300),
    );
    assert!(t0.elapsed() < Duration::from_secs(5), "{:?}", t0.elapsed());
    assert_eq!(v["installed"], true);
    assert!(v["error"].as_str().unwrap().contains("timed out"), "{v}");
}

#[test]
fn process_check_reads_comm() {
    let root = scratch("proc");
    assert!(!running(&root.join("proc"), "tailscaled"));
    fake_socket_and_daemon(&root);
    assert!(running(&root.join("proc"), "tailscaled"));
    assert!(!running(&root.join("proc"), "tailscale"));
}

#[test]
fn lines_query_and_tail_cap() {
    assert_eq!(parse_lines(""), 200);
    assert_eq!(parse_lines("lines=50"), 50);
    assert_eq!(parse_lines("?n=7"), 7);
    assert_eq!(parse_lines("lines=0"), 1);
    assert_eq!(parse_lines("lines=999999"), 2000);
    assert_eq!(parse_lines("lines=abc"), 200);
    let root = scratch("tail");
    let f = root.join("log");
    let text: String = (0..100).map(|i| format!("line {i}\n")).collect();
    fs::write(&f, &text).unwrap();
    assert_eq!(
        tail_file(&f, 3, 1 << 20),
        vec!["line 97", "line 98", "line 99"]
    );
    assert_eq!(tail_file(&f, 500, 1 << 20).len(), 100);
    // only the last 30 bytes: the cut first line is dropped
    let short = tail_file(&f, 500, 30);
    assert_eq!(short.last().unwrap(), "line 99");
    assert!(short.iter().all(|l| l.starts_with("line ")), "{short:?}");
    assert!(short.len() < 5, "{short:?}");
    assert!(tail_file(&root.join("missing"), 5, 1024).is_empty());
}

#[tokio::test]
async fn routes_at_the_agents_paths() {
    let root = scratch("routes");
    fs::create_dir_all(root.join("data")).unwrap();
    fs::write(root.join("data/tailscaled.log"), "a\nb\nc\n").unwrap();
    let addr = start(Compat::new(root.to_str().unwrap(), Sources::none())).await;
    let (st, b) = call(addr, "GET", "/api/services/tailscale", None, "").await;
    assert_eq!(
        (st, b["data"].clone()),
        (200, json!({"installed": false, "running": false}))
    );
    let (st, b) = call(addr, "GET", "/api/services/tailscale/log?lines=2", None, "").await;
    assert_eq!(st, 200);
    assert_eq!(
        b["data"],
        json!({"path": "/data/tailscaled.log", "lines": ["b", "c"], "limit": 2})
    );
}
