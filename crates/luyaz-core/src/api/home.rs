//! GET `/api/services/chill/home` for the touch screen, rendered by u60-api as
//! the agent renders it. The machine: on/off, rest and the notice from the agent's file (requests
//! change them without a record), the probe results from u60d's replay of the agent's records.
//! `busy` is null and `ready` always true until the requests move here (8e); while the agent runs
//! a request the two replies differ for those seconds.
//!
//! CHILL (not public in the agent); u60d's public scope is decided at Phase 10.

use serde_json::Value;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use axum::{extract::State, response::Response};
use serde_json::json;
use luyaz_api::chill as ch;
use luyaz_policy::home_exit as p;

use super::{Compat, reply};

const FISH_GROUP: &str = "🐟 漏网之鱼";
const MIHOMO_TIMEOUT: Duration = Duration::from_secs(3);
pub(super) const TS_BIN: &str = "/data/tailscale/tailscale";
pub(super) const TS_SOCKET: &str = "/tmp/tailscaled.sock";
/// The agent's TS_CACHE_SECS.
const TS_EVERY: Duration = Duration::from_secs(60);
pub(super) const TS_TIMEOUT: Duration = Duration::from_secs(5);

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The machine as the replies read it while the home exit's engine is not running: its file, no
/// probe.
pub(super) fn machine(c: &Compat) -> ch::Home {
    let file: p::Persist = std::fs::read_to_string(c.path(ch::HOME_EXIT_STATE))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    ch::Home {
        ready: true,
        media: file.items.media,
        ip: file.items.ip,
        rest: file.items.rest,
        rest_until: file.rest_until,
        home_ok: None,
        home_ms: None,
        probed_at: 0,
        busy: Value::Null,
        notice: json!(file.notice),
    }
}

/// The last `tailscale status` answer's path to home, refreshed in the background at most once a
/// minute and only when GET asks (the agent's `ts_now`): never on a timer, never waited for.
#[derive(Default)]
pub struct TsCache {
    /// (when, path)
    last: Mutex<Option<(Instant, Option<&'static str>)>>,
    refreshing: std::sync::atomic::AtomicBool,
}

impl TsCache {
    fn get(&self, c: &Compat, ip: std::net::Ipv4Addr) -> Option<&'static str> {
        use std::sync::atomic::Ordering;
        let cached = *self.last.lock().unwrap_or_else(|e| e.into_inner());
        let fresh = cached.is_some_and(|(at, _)| at.elapsed() < TS_EVERY);
        if !fresh && !self.refreshing.swap(true, Ordering::SeqCst) {
            let (ts, src) = (c.ts.clone(), c.src.tailscale.clone());
            tokio::task::spawn_blocking(move || {
                let v = src().and_then(|s| ch::ts_path(&s, ip));
                *ts.last.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), v));
                ts.refreshing.store(false, Ordering::SeqCst);
            });
        }
        cached.and_then(|(_, v)| v)
    }
}

/// `tailscale status --json` (the agent's `services::tailscale_json`).
pub fn tailscale_status() -> Option<Value> {
    let bin = crate::state::host_path(TS_BIN);
    let sock = crate::state::host_path(TS_SOCKET);
    if !std::path::Path::new(&sock).exists() {
        return None;
    }
    let out = ts_exec(
        std::path::Path::new(&bin),
        std::path::Path::new(&sock),
        TS_TIMEOUT,
    )
    .ok()?;
    serde_json::from_slice(&out).ok()
}

/// `<bin> --socket <sock> status --json` within `limit` (blocking: run off the async threads).
/// Ok(stdout) on success; Err(stderr, trimmed) when it fails, "exec failed: …" when it cannot
/// start or does not answer in time (then it is killed).
pub(super) fn ts_exec(
    bin: &std::path::Path,
    sock: &std::path::Path,
    limit: Duration,
) -> Result<Vec<u8>, String> {
    if !bin.exists() {
        return Err("exec failed: not installed".into());
    }
    let mut child = std::process::Command::new(bin)
        .arg("--socket")
        .arg(sock)
        .args(["status", "--json"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("exec failed: {e}"))?;
    let drain = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        })
    };
    let out = drain(child.stdout.take().map(|p| Box::new(p) as _));
    let err = drain(child.stderr.take().map(|p| Box::new(p) as _));
    let t0 = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if t0.elapsed() < limit => std::thread::sleep(Duration::from_millis(50)),
            Ok(None) => {
                // the readers end when the pipes close; not waited for
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("exec failed: timed out after {}s", limit.as_secs()));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("exec failed: {e}"));
            }
        }
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    if status.success() {
        Ok(stdout)
    } else {
        Err(String::from_utf8_lossy(&stderr).trim().to_string())
    }
}

pub(super) fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// What mihomo's group has selected now (the agent's `group_now`).
async fn group_now(c: &Compat, secret: &str, group: &str) -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(MIHOMO_TIMEOUT)
        .build()
        .ok()?;
    let mut req = client.get(format!("{}/proxies/{}", c.src.mihomo, enc(group)));
    if !secret.is_empty() {
        req = req.header("Authorization", format!("Bearer {secret}"));
    }
    let r = req.send().await.ok()?;
    if r.status().as_u16() >= 400 {
        return None;
    }
    let v: Value = serde_json::from_slice(&r.bytes().await.ok()?).ok()?;
    v.get("now").and_then(Value::as_str).map(str::to_string)
}

/// (serving_mcc, true_roaming) from u60d's own signal block; unknown when stale.
fn where_now(c: &Compat) -> (Option<i64>, Option<bool>) {
    let Some(sig) = c
        .src
        .hub
        .as_ref()
        .and_then(|h| h.view("signal"))
        .filter(|v| !v.stale)
    else {
        return (None, None);
    };
    (
        sig.data.get("serving_mcc").and_then(Value::as_i64),
        sig.data.get("true_roaming").and_then(Value::as_bool),
    )
}

/// GET /api/services/chill/home
pub async fn get(State(c): State<Compat>) -> Response {
    let c2 = c.clone();
    let read = tokio::task::spawn_blocking(move || {
        let env = std::fs::read_to_string(c2.path(ch::CHILL_ENV)).unwrap_or_default();
        let home = ch::parse_home_exit(&ch::env_var(&env, "CHILL_HOME_EXIT"))?;
        let state: Option<Value> = std::fs::read_to_string(c2.path(ch::CHILL_STATE))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok());
        let mode = std::fs::read_to_string(c2.path(ch::CHILL_MODE_FILE)).unwrap_or_default();
        let sites = std::fs::read_to_string(c2.path(ch::HOME_IP_LIST))
            .map(|s| ch::count_sites(&s))
            .unwrap_or(0);
        Some((
            home,
            ch::chill_mode(state.as_ref(), &mode),
            sites,
            machine(&c2),
            ch::env_var(&env, "CHILL_SECRET"),
        ))
    })
    .await
    .ok()
    .flatten();
    let Some(((ip, _), mode, sites, m, secret)) = read else {
        return reply(200, json!({"ok": true, "data": {"configured": false}}));
    };
    let (serving_mcc, true_roaming) = where_now(&c);
    let tailscale = c.ts.get(&c, ip);
    // Where the traffic goes while home is down (DD6 "暂走 X").
    let fallback_to = if m.ready && ch::wants_fallback(&m, mode) {
        group_now(&c, &secret, FISH_GROUP).await
    } else {
        None
    };
    let around = ch::Around {
        serving_mcc,
        true_roaming,
        blocked: mode.blocked(),
        sites,
        fallback_to,
        tailscale,
        now: now_unix(),
    };
    reply(200, json!({"ok": true, "data": ch::home_get(&m, &around)}))
}
