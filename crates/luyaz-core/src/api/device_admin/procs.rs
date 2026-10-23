//! The process monitor (the agent's `system.rs` `ProcessTracker` and `kill_bloat`): GET
//! `/api/system/top` (a read: CPU is the delta since the previous call, as `/api/cpu`) and POST
//! `/api/system/kill-bloat` (SIGKILL, only to the vendor daemons on [`BLOAT`]).

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{LazyLock, Mutex};

use axum::{body::Bytes, extract::State, response::Response};
use serde_json::{Value, json};

use super::super::{Compat, reply};

/// Daemons listed in /etc/config/zte_topsw_daemon.conf: zte_topsw_daemon waits for every one of
/// them before it releases boot. Never killed (CLAUDE.md "ZTE Daemon Sync Barrier").
#[cfg(test)]
pub(crate) const SYNC_BARRIER: &[&str] = &[
    "zte_topsw_mc",
    "zte_router",
    "zte_topsw_data",
    "zte_topsw_nwinfo",
    "zte_topsw_mdm",
    "zte_topsw_sleep_faw",
    "zte_topsw_apn",
    "zte_topsw_wms",
    "zte_topsw_key",
    "zte_topsw_led",
    "zte_topsw_tr098db",
    "zte_dm",
    "zte_topsw_fota_result",
    "zte_topsw_devui",
    "zte_topsw_wlan",
    "zte_smart_manage",
];

/// Vendor daemons that are safe to kill: none of them is in daemon.conf, so the boot gate does not
/// wait for them.
pub(crate) const BLOAT: &[&str] = &[
    // remote management and phone-home
    "zte_dua",
    "zte_mqtt_sdk_st",
    "zte-topsw-tunnel",
    "zte_topsw_tr069",
    "zte_topsw_tr069_sub",
    // features this device does not use
    "zte_topsw_diag",
    "zte_topsw_get_brand",
    "zte_topsw_jwxk_query",
    "zte_topsw_nfc",
    "zte_topsw_samba",
];

/// `/proc/<pid>/stat`'s one-letter state as the page shows it.
fn state_word(letter: &str) -> &'static str {
    const WORDS: [(&str, &str); 5] = [
        ("D", "disk"),
        ("R", "running"),
        ("S", "sleeping"),
        ("T", "stopped"),
        ("Z", "zombie"),
    ];
    WORDS
        .iter()
        .find(|(l, _)| *l == letter)
        .map_or("other", |(_, w)| w)
}

fn is_bloat(name: &str) -> bool {
    BLOAT.contains(&name)
}

/// Per-pid CPU ticks and the total at the previous call.
static PREV: LazyLock<Mutex<(HashMap<u32, u64>, u64)>> = LazyLock::new(Mutex::default);

fn total_ticks(proc: &Path) -> u64 {
    fs::read_to_string(proc.join("stat"))
        .ok()
        .and_then(|s| {
            s.lines().find(|l| l.starts_with("cpu ")).map(|l| {
                l.split_whitespace()
                    .skip(1)
                    .filter_map(|v| v.parse::<u64>().ok())
                    .sum()
            })
        })
        .unwrap_or(0)
}

/// The basename of argv[0], else None.
fn cmdline_name(proc: &Path, pid: u32) -> Option<String> {
    let s = fs::read_to_string(proc.join(format!("{pid}/cmdline"))).ok()?;
    let clean = s.replace('\0', " ");
    let first = clean.split_whitespace().next()?;
    let base = first.rsplit('/').next().unwrap_or(first);
    (!base.is_empty()).then(|| base.to_string())
}

/// `(comm, state, utime + stime, rss kB)` from /proc/PID/stat.
fn stat(proc: &Path, pid: u32) -> Option<(String, String, u64, u64)> {
    let s = fs::read_to_string(proc.join(format!("{pid}/stat"))).ok()?;
    let (start, end) = (s.find('(')? + 1, s.rfind(')')?);
    let comm = s.get(start..end)?.to_string();
    let f: Vec<&str> = s.get(end + 2..)?.split_whitespace().collect();
    if f.len() < 22 {
        return None;
    }
    let n = |i: usize| f[i].parse::<u64>().unwrap_or(0);
    Some((comm, f[0].to_string(), n(11) + n(12), n(21) * 4))
}

fn pids(proc: &Path) -> Vec<u32> {
    fs::read_dir(proc)
        .map(|d| {
            d.flatten()
                .filter_map(|e| e.file_name().to_str()?.parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// The agent's `ProcessTracker::sample`, `/proc` at `proc`.
pub(super) fn sample(proc: &Path) -> Value {
    let total = total_ticks(proc);
    let mut prev = PREV.lock().unwrap_or_else(|e| e.into_inner());
    let dt = total.saturating_sub(prev.1);
    let mut now: HashMap<u32, u64> = HashMap::new();
    let mut rows: Vec<(f64, Value, bool, u64)> = Vec::new();
    for pid in pids(proc) {
        let Some((comm, state, ticks, rss_kb)) = stat(proc, pid) else {
            continue;
        };
        let cpu = if dt > 0 {
            let before = prev.0.get(&pid).copied().unwrap_or(ticks);
            round1(ticks.saturating_sub(before) as f64 / dt as f64 * 100.0)
        } else {
            0.0
        };
        now.insert(pid, ticks);
        let name = cmdline_name(proc, pid).unwrap_or(comm);
        let bloat = is_bloat(&name);
        let state = state_word(&state);
        rows.push((
            cpu,
            json!({"pid": pid, "name": name, "cpu_pct": cpu, "rss_kb": rss_kb, "state": state,
                "is_bloat": bloat}),
            bloat,
            rss_kb,
        ));
    }
    *prev = (now, total);
    rows.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let bloat: Vec<_> = rows.iter().filter(|r| r.2).collect();
    let bloat_cpu = round1(bloat.iter().map(|r| r.0).sum::<f64>());
    let bloat_rss: u64 = bloat.iter().map(|r| r.3).sum();
    let (total_count, bloat_count) = (rows.len(), bloat.len());
    let processes: Vec<Value> = rows.into_iter().take(50).map(|r| r.1).collect();
    json!({"processes": processes, "total_count": total_count, "bloat_count": bloat_count,
        "bloat_cpu_pct": bloat_cpu, "bloat_rss_kb": bloat_rss})
}

/// GET /api/system/top
pub async fn top(State(c): State<Compat>) -> Response {
    let proc = c.path("/proc");
    match tokio::task::spawn_blocking(move || sample(&proc)).await {
        Ok(v) => reply(200, json!({"ok": true, "data": v})),
        Err(_) => reply(503, json!({"ok": false, "error": "process list failed"})),
    }
}

/// The agent's `kill_bloat`: `pids` None = every running bloat daemon; a pid not on [`BLOAT`]
/// (by its name now) is skipped, never killed.
pub(super) fn kill(proc: &Path, pids_asked: Option<&[u32]>, signal: &dyn Fn(u32) -> bool) -> Value {
    let name_rss = |pid: u32| {
        let (comm, _, _, rss) = stat(proc, pid)?;
        Some((cmdline_name(proc, pid).unwrap_or(comm), rss))
    };
    let targets: Vec<(u32, String, u64)> = match pids_asked {
        Some(list) => list
            .iter()
            .filter_map(|&p| name_rss(p).map(|(n, r)| (p, n, r)))
            .collect(),
        None => pids(proc)
            .into_iter()
            .filter_map(|p| name_rss(p).map(|(n, r)| (p, n, r)))
            .filter(|(_, n, _)| is_bloat(n))
            .collect(),
    };
    let (mut killed, mut skipped, mut freed) = (Vec::new(), Vec::new(), 0u64);
    for (pid, name, rss) in targets {
        if is_bloat(&name) && signal(pid) {
            freed += rss;
            killed.push(json!({"pid": pid, "name": name}));
        } else {
            skipped.push(json!({"pid": pid, "name": name}));
        }
    }
    json!({"killed": killed, "skipped": skipped, "freed_rss_kb": freed})
}

/// SIGKILL to `pid`.
pub(super) fn sigkill(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // SAFETY: kill(2) with a positive pid and a constant signal
    pid > 0 && unsafe { libc::kill(pid, libc::SIGKILL) } == 0
}

/// POST /api/system/kill-bloat `{"all": true}` or `{"pids": [..]}`
pub async fn kill_bloat(State(c): State<Compat>, body: Bytes) -> Response {
    let Ok(parsed) = serde_json::from_slice::<Value>(&body) else {
        return reply(400, json!({"ok": false, "error": "invalid JSON"}));
    };
    let asked: Option<Vec<u32>> = if parsed["all"].as_bool() == Some(true) {
        None
    } else if let Some(arr) = parsed["pids"].as_array() {
        let ids: Vec<u32> = arr
            .iter()
            .filter_map(|v| v.as_u64().map(|n| n as u32))
            .collect();
        if ids.is_empty() {
            return reply(400, json!({"ok": false, "error": "pids array is empty"}));
        }
        Some(ids)
    } else {
        return reply(
            400,
            json!({"ok": false, "error": "say which: all, or a pids list"}),
        );
    };
    let (proc, signal) = (c.path("/proc"), c.admin.kill.clone());
    match tokio::task::spawn_blocking(move || kill(&proc, asked.as_deref(), &*signal)).await {
        Ok(v) => reply(200, json!({"ok": true, "data": v})),
        Err(_) => reply(500, json!({"ok": false, "error": "internal"})),
    }
}
