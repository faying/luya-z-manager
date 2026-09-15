//! ShellCrash profile manager — mutating endpoints layered on top of the
//! read-only status in `services.rs`.
//!
//! ShellCrash itself is single-active-config: the live subscription lives at
//! `/etc/ShellCrash/yamls/config.yaml` and is regenerated through the
//! `clash_modify.sh` pipeline (DoH + region_filter) on every restart. There is
//! no native multi-profile store, so this module keeps one of its own under
//! `/etc/ShellCrash/profiles/`:
//!
//!   profiles/<id>.yaml   cached RAW subscription (pre-modify), one per profile
//!   profiles/index.json  { profiles: [...], active: "<id>" }
//!
//! Switching = copy a cached profile over yamls/config.yaml + restart (offline,
//! reuses the modify pipeline). Adding from a URL reuses ShellCrash's own robust
//! downloader (`task.sh update_config`) so UA / redirects / fake-ip are handled.
//!
//! Slow operations (download, apply) run on a detached thread guarded by a
//! single-slot job (mirrors the speedtest pattern); the UI polls `…/profiles/job`.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::handlers::AppState;

const SC_DIR: &str = "/etc/ShellCrash";
const PROFILES_DIR: &str = "/etc/ShellCrash/profiles";
const INDEX_PATH: &str = "/etc/ShellCrash/profiles/index.json";
const ACTIVE_CONFIG: &str = "/etc/ShellCrash/yamls/config.yaml";
const CFG_PATH: &str = "/etc/ShellCrash/configs/ShellCrash.cfg";
const TASK_SH: &str = "/etc/ShellCrash/task/task.sh";
const INITD: &str = "/etc/init.d/shellcrash";
const MIHOMO_API: &str = "http://127.0.0.1:9999";
/// Per-profile `proxy-server-nameserver` override, consumed by the patched
/// clash_modify.sh. One resolver per line; absent/empty → clash_modify default.
const PROXY_DNS_LIST: &str = "/etc/ShellCrash/configs/proxy_dns.list";
/// Historical raw subscription ShellCrash keeps; adopted on first run.
const LEGACY_BAK: &str = "/etc/ShellCrash/yamls/config.yaml.bak";
/// The generated runtime config the core actually runs.
const SC_CONFIG_RUNTIME: &str = "/tmp/ShellCrash/config.yaml";

const MAX_CONFIG_BYTES: usize = 8 * 1024 * 1024; // 8 MB
const MAX_URL_LEN: usize = 4096;
const MAX_DNS_SERVERS: usize = 8;

/// Profile-apply resource guard. A switch regenerates the config and (on the
/// restart fallback) rebuilds the firewall — a memory/CPU spike that has hard-
/// hung this device (PMIC warm-reset, cause=32) when it was already low on RAM
/// or thermally stressed. Refuse below/above these thresholds; a clear refusal
/// beats a ~30-min watchdog outage. Healthy idle is ~600 MB free, ~50°C; the
/// crash captures sat at ~27 MB / 80–82°C, so these sit safely between.
const MIN_MEM_AVAIL_KB: u64 = 100 * 1024; // 100 MB
const MAX_TEMP_MILLIC: i64 = 78_000; // 78°C (sysfs reports milli-°C)

/* -------------------------------------------------------------- *
 *  State
 * -------------------------------------------------------------- */

#[derive(Clone, Serialize, Deserialize)]
struct Profile {
    id: String,
    name: String,
    kind: String, // "url" | "upload"
    #[serde(default)]
    source_url: String,
    #[serde(default)]
    added_unix: u64,
    #[serde(default)]
    updated_unix: u64,
    #[serde(default)]
    node_count: u64,
    /// Airline-specific `proxy-server-nameserver` resolvers for this profile.
    /// Empty → the global clash_modify default is used. Extracted from the
    /// subscription on add, editable in the UI. See PROXY_DNS_LIST.
    #[serde(default)]
    dns_servers: Vec<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct Index {
    #[serde(default)]
    profiles: Vec<Profile>,
    #[serde(default)]
    active: String,
}

#[derive(Clone, Serialize)]
pub struct JobState {
    id: u64,
    kind: String,       // "download" | "refresh" | "apply"
    status: String,     // "idle" | "running" | "done" | "error"
    message: String,
    profile_id: String,
    started_unix: u64,
    finished_unix: u64,
}

pub struct ScAdmin {
    job: Arc<Mutex<JobState>>,
    running: Arc<AtomicBool>,
}

impl ScAdmin {
    pub fn new() -> Self {
        Self {
            job: Arc::new(Mutex::new(JobState {
                id: 0,
                kind: String::new(),
                status: "idle".into(),
                message: String::new(),
                profile_id: String::new(),
                started_unix: 0,
                finished_unix: 0,
            })),
            running: Arc::new(AtomicBool::new(false)),
        }
    }
}

/* -------------------------------------------------------------- *
 *  Endpoints
 * -------------------------------------------------------------- */

/// GET /api/services/shellcrash/profiles
pub fn profiles_list(state: &AppState) -> (u16, Value) {
    let idx = load_index();
    let busy = state.sc_admin.running.load(Ordering::Relaxed);
    // Skip the mihomo lookup while a job runs — the core may be mid-restart, so
    // the call would block a worker thread for the full timeout. The node is
    // only shown for the active profile and refreshes on the next idle poll.
    let now_node = if busy { None } else { current_node() };

    let profiles: Vec<Value> = idx
        .profiles
        .iter()
        .map(|p| {
            let active = p.id == idx.active;
            json!({
                "id": p.id,
                "name": p.name,
                "kind": p.kind,
                "source_url": p.source_url,
                "added_unix": p.added_unix,
                "updated_unix": p.updated_unix,
                "node_count": p.node_count,
                "active": active,
                "current_node": if active { now_node.clone() } else { None },
                "dns_servers": p.dns_servers,
            })
        })
        .collect();

    (
        200,
        json!({"ok": true, "data": {
            "profiles": profiles,
            "active": idx.active,
            "busy": busy,
        }}),
    )
}

/// POST /api/services/shellcrash/profiles/upload  — body {name, content}
///
/// Synchronous: validates and stores the config as a new profile. Does NOT
/// activate it (the user applies it afterwards).
pub fn profile_upload(state: &AppState, body: &[u8]) -> (u16, Value) {
    if state.sc_admin.running.load(Ordering::Relaxed) {
        return (409, json!({"ok": false, "error": "operation in progress"}));
    }
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let name = parsed["name"].as_str().unwrap_or("").trim();
    let name = if name.is_empty() { "Uploaded" } else { name };
    let content = match parsed["content"].as_str() {
        Some(c) => c,
        None => return (400, json!({"ok": false, "error": "missing 'content'"})),
    };

    if content.len() > MAX_CONFIG_BYTES {
        return (413, json!({"ok": false, "error": "config too large (max 8 MB)"}));
    }
    if !looks_like_clash_config(content) {
        return (
            400,
            json!({"ok": false, "error": "not a clash config (no 'proxies:' / 'proxy-providers:')"}),
        );
    }

    if fs::create_dir_all(PROFILES_DIR).is_err() {
        return (500, json!({"ok": false, "error": "cannot create profiles dir"}));
    }
    let id = make_id(name);
    let dest = format!("{PROFILES_DIR}/{id}.yaml");
    if atomic_write(&dest, content.as_bytes()).is_err() {
        return (500, json!({"ok": false, "error": "cannot write profile"}));
    }

    let now = now_unix();
    let profile = Profile {
        id: id.clone(),
        name: name.to_string(),
        kind: "upload".into(),
        source_url: String::new(),
        added_unix: now,
        updated_unix: now,
        node_count: count_nodes(content) as u64,
        dns_servers: extract_proxy_dns(content),
    };
    let mut idx = load_index();
    upsert(&mut idx, profile.clone());
    if write_index(&idx).is_err() {
        return (500, json!({"ok": false, "error": "cannot write index"}));
    }

    (200, json!({"ok": true, "data": profile_json(&profile, false, None)}))
}

/// POST /api/services/shellcrash/profiles/url  — body {name, url}
///
/// Background: downloads via ShellCrash's own updater (handles UA/redirects),
/// activates it, then snapshots the result as a new profile.
pub fn profile_add_url(state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let name = parsed["name"].as_str().unwrap_or("").trim();
    let name = if name.is_empty() { "Subscription" } else { name }.to_string();
    let url = parsed["url"].as_str().unwrap_or("").trim().to_string();
    if !valid_url(&url) {
        return (400, json!({"ok": false, "error": "invalid URL (http(s) only, no quotes/spaces)"}));
    }
    let id = make_id(&name);
    spawn_download(state, "download", id, name, url)
}

/// POST /api/services/shellcrash/profiles/refresh  — body {id}
///
/// Background: re-pulls an existing URL profile's subscription and activates it.
pub fn profile_refresh(state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let id = parsed["id"].as_str().unwrap_or("").trim().to_string();
    if id.is_empty() {
        return (400, json!({"ok": false, "error": "missing 'id'"}));
    }
    let idx = load_index();
    let profile = match idx.profiles.iter().find(|p| p.id == id) {
        Some(p) => p.clone(),
        None => return (404, json!({"ok": false, "error": "profile not found"})),
    };
    if profile.kind != "url" || !valid_url(&profile.source_url) {
        return (400, json!({"ok": false, "error": "profile has no refreshable URL"}));
    }
    spawn_download(state, "refresh", profile.id, profile.name, profile.source_url)
}

/// POST /api/services/shellcrash/profiles/apply  — body {id}
///
/// Background: copies a cached profile over the active config and restarts.
pub fn profile_apply(state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let id = parsed["id"].as_str().unwrap_or("").trim().to_string();
    if id.is_empty() {
        return (400, json!({"ok": false, "error": "missing 'id'"}));
    }
    let idx = load_index();
    let profile = match idx.profiles.iter().find(|p| p.id == id) {
        Some(p) => p.clone(),
        None => return (404, json!({"ok": false, "error": "profile not found"})),
    };

    let job_id = match try_start_job(state, "apply", &id) {
        Some(j) => j,
        None => return (409, json!({"ok": false, "error": "operation in progress"})),
    };
    let job = Arc::clone(&state.sc_admin.job);
    let running = Arc::clone(&state.sc_admin.running);
    std::thread::spawn(move || {
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| do_apply(&profile)))
            .unwrap_or_else(|_| Err("internal panic".into()));
        finish_job(&job, &running, res);
    });
    (200, json!({"ok": true, "data": {"job_id": job_id, "status": "running"}}))
}

/// DELETE /api/services/shellcrash/profiles  — body {id}
pub fn profile_delete(state: &AppState, body: &[u8]) -> (u16, Value) {
    if state.sc_admin.running.load(Ordering::Relaxed) {
        return (409, json!({"ok": false, "error": "operation in progress"}));
    }
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let id = parsed["id"].as_str().unwrap_or("").trim().to_string();
    if id.is_empty() {
        return (400, json!({"ok": false, "error": "missing 'id'"}));
    }
    let mut idx = load_index();
    if idx.active == id {
        return (400, json!({"ok": false, "error": "cannot delete the active profile"}));
    }
    if !idx.profiles.iter().any(|p| p.id == id) {
        return (404, json!({"ok": false, "error": "profile not found"}));
    }
    idx.profiles.retain(|p| p.id != id);
    let _ = fs::remove_file(format!("{PROFILES_DIR}/{id}.yaml"));
    if write_index(&idx).is_err() {
        return (500, json!({"ok": false, "error": "cannot write index"}));
    }
    (200, json!({"ok": true, "data": {"deleted": id}}))
}

/// PUT /api/services/shellcrash/profiles/dns  — body {id, dns_servers: [..]}
///
/// Stores the profile's airline-specific resolvers. Editing the ACTIVE profile
/// re-applies it (background restart) so the change takes effect now; editing an
/// inactive profile just saves it (applies on switch).
pub fn profile_dns_set(state: &AppState, body: &[u8]) -> (u16, Value) {
    // Reject while a job runs (consistent with upload/delete) — otherwise an
    // edit to the ACTIVE profile would persist new DNS to the index but fail to
    // re-apply it, leaving stored state ahead of the running config.
    if state.sc_admin.running.load(Ordering::Relaxed) {
        return (409, json!({"ok": false, "error": "operation in progress"}));
    }
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let id = parsed["id"].as_str().unwrap_or("").trim().to_string();
    if id.is_empty() {
        return (400, json!({"ok": false, "error": "missing 'id'"}));
    }
    let servers: Vec<String> = match parsed["dns_servers"].as_array() {
        Some(a) => a
            .iter()
            .filter_map(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        None => return (400, json!({"ok": false, "error": "missing 'dns_servers' array"})),
    };
    if servers.len() > MAX_DNS_SERVERS {
        return (400, json!({"ok": false, "error": "too many DNS servers"}));
    }
    if let Some(bad) = servers.iter().find(|s| !valid_resolver(s)) {
        return (400, json!({"ok": false, "error": format!("invalid resolver: {bad}")}));
    }

    let mut idx = load_index();
    if !idx.profiles.iter().any(|p| p.id == id) {
        return (404, json!({"ok": false, "error": "profile not found"}));
    }
    let is_active = idx.active == id;

    // For the ACTIVE profile, claim the job slot BEFORE persisting so we never
    // leave the stored DNS ahead of the running config (closes the narrow race
    // with the entry gate above). Inactive edits just persist — no restart.
    let job_id = if is_active {
        match try_start_job(state, "apply", &id) {
            Some(j) => Some(j),
            None => return (409, json!({"ok": false, "error": "operation in progress"})),
        }
    } else {
        None
    };

    if let Some(p) = idx.profiles.iter_mut().find(|p| p.id == id) {
        p.dns_servers = servers;
    }
    if write_index(&idx).is_err() {
        // Release the slot we may have claimed so the device isn't wedged busy.
        if job_id.is_some() {
            state.sc_admin.running.store(false, Ordering::Relaxed);
        }
        return (500, json!({"ok": false, "error": "cannot write index"}));
    }

    if let Some(job_id) = job_id {
        let profile = idx.profiles.iter().find(|p| p.id == id).cloned().unwrap();
        let job = Arc::clone(&state.sc_admin.job);
        let running = Arc::clone(&state.sc_admin.running);
        std::thread::spawn(move || {
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| do_apply(&profile)))
                .unwrap_or_else(|_| Err("internal panic".into()));
            finish_job(&job, &running, res);
        });
        return (200, json!({"ok": true, "data": {"applied": true, "job_id": job_id}}));
    }
    (200, json!({"ok": true, "data": {"applied": false}}))
}

/// PUT /api/services/shellcrash/profiles/rename  — body {id, name}
///
/// Display-name only; the profile id is immutable, so no files move and no
/// restart is needed. Still gated on `running`: a download/apply job also does a
/// load-modify-write of index.json, so a concurrent rename could clobber (or be
/// clobbered by) the node_count/active/dns the job is about to persist.
pub fn profile_rename(state: &AppState, body: &[u8]) -> (u16, Value) {
    if state.sc_admin.running.load(Ordering::Relaxed) {
        return (409, json!({"ok": false, "error": "operation in progress"}));
    }
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let id = parsed["id"].as_str().unwrap_or("").trim().to_string();
    if id.is_empty() {
        return (400, json!({"ok": false, "error": "missing 'id'"}));
    }
    let name = sanitize_name(parsed["name"].as_str().unwrap_or(""));
    if name.is_empty() {
        return (400, json!({"ok": false, "error": "name cannot be empty"}));
    }

    let mut idx = load_index();
    match idx.profiles.iter_mut().find(|p| p.id == id) {
        Some(p) => p.name = name.clone(),
        None => return (404, json!({"ok": false, "error": "profile not found"})),
    }
    if write_index(&idx).is_err() {
        return (500, json!({"ok": false, "error": "cannot write index"}));
    }
    (200, json!({"ok": true, "data": {"id": id, "name": name}}))
}

/// GET /api/services/shellcrash/profiles/job
pub fn profile_job(state: &AppState) -> (u16, Value) {
    let j = state.sc_admin.job.lock().unwrap().clone();
    (200, json!({"ok": true, "data": j}))
}

/* -------------------------------------------------------------- *
 *  Background workers
 * -------------------------------------------------------------- */

fn spawn_download(
    state: &AppState,
    kind: &str,
    id: String,
    name: String,
    url: String,
) -> (u16, Value) {
    let job_id = match try_start_job(state, kind, &id) {
        Some(j) => j,
        None => return (409, json!({"ok": false, "error": "operation in progress"})),
    };
    let job = Arc::clone(&state.sc_admin.job);
    let running = Arc::clone(&state.sc_admin.running);
    std::thread::spawn(move || {
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            do_download(&id, &name, &url)
        }))
        .unwrap_or_else(|_| Err("internal panic".into()));
        finish_job(&job, &running, res);
    });
    (200, json!({"ok": true, "data": {"job_id": job_id, "status": "running"}}))
}

/// Download a subscription via ShellCrash's own updater, then snapshot it.
fn do_download(id: &str, name: &str, url: &str) -> Result<String, String> {
    let prev_https = read_cfg_https();
    let prev_node = current_node();
    set_cfg_subscription(url).map_err(|e| format!("cfg write failed: {e}"))?;

    // Stop the proxy BEFORE fetching the subscription. Pulling a sub must go over
    // the direct (domestic) network: the airline's sub server is domestic-routed,
    // and — crucially — the currently-selected node is often dead (the very reason
    // you're switching), so fetching through the live proxy would fail. Stopping
    // drops the nft redirect rules so update_config's download goes out directly.
    run_cmd(&format!("{INITD} stop"));
    // If the core is still up the stop didn't take — the fetch would then route
    // through the (maybe dead) proxy; surfaced in the error message on failure.
    let stop_effective = !core_running();

    // get_core_config (download into yamls/config.yaml) && start.sh start (regen+restart)
    let (ok, out) = run_cmd(&format!("{TASK_SH} update_config"));

    let cfg = fs::read_to_string(ACTIVE_CONFIG).unwrap_or_default();
    let nodes = count_nodes(&cfg);
    // A valid result has inline nodes OR proxy-providers (same rule the upload
    // path uses). Treat anything else as a failed fetch — ShellCrash leaves the
    // previous active config in place, so we roll back the subscription URL and
    // bring the proxy back up on it (update_config's own `start` runs only on
    // success, so after a failed fetch the core is still stopped).
    if !ok || (nodes == 0 && !cfg.contains("proxy-providers:")) {
        let _ = set_cfg_subscription(&prev_https);
        let recovered = restart_and_wait().is_ok();
        let _ = restore_node(&prev_node);
        let mut msg = format!("download failed (nodes={nodes})");
        if !stop_effective {
            msg.push_str(" [proxy didn't stop — fetch may have routed through a dead node]");
        }
        if !recovered {
            msg.push_str(" [WARNING: proxy did not come back up — open the page / restart it]");
        }
        msg.push_str(&format!(": {}", tail(&out, 6)));
        return Err(msg);
    }

    let dest = format!("{PROFILES_DIR}/{id}.yaml");
    fs::create_dir_all(PROFILES_DIR).map_err(|e| format!("mkdir failed: {e}"))?;
    atomic_write(&dest, cfg.as_bytes()).map_err(|e| format!("snapshot failed: {e}"))?;

    let dns_servers = extract_proxy_dns(&cfg);

    let now = now_unix();
    let mut idx = load_index();
    let added = idx
        .profiles
        .iter()
        .find(|p| p.id == id)
        .map(|p| p.added_unix)
        .unwrap_or(now);
    upsert(
        &mut idx,
        Profile {
            id: id.to_string(),
            name: name.to_string(),
            kind: "url".into(),
            source_url: url.to_string(),
            added_unix: added,
            updated_unix: now,
            node_count: nodes as u64,
            dns_servers: dns_servers.clone(),
        },
    );
    idx.active = id.to_string();
    write_index(&idx).map_err(|e| format!("index write failed: {e}"))?;

    // task.sh update_config already restarted, but with whatever proxy_dns.list
    // the PREVIOUS profile left. Sync it to this profile's DNS; if it changed,
    // restart once more so the airline's own resolver takes effect.
    if write_proxy_dns(&dns_servers) {
        let _ = restart_and_wait();
    }

    // A refresh of the same airline keeps the user's node; a brand-new airline
    // won't contain it, so the group stays on its default (reported to the UI).
    let node = restore_node(&prev_node);
    Ok(match node {
        Some(n) => format!("Downloaded and applied — {nodes} nodes, on {n}"),
        None => format!("Downloaded and applied — {nodes} nodes"),
    })
}

/// Apply a cached profile: copy over the active config and restart.
fn do_apply(profile: &Profile) -> Result<String, String> {
    // Refuse to switch when the box is already near the edge — the config regen
    // (and the firewall rebuild on the restart fallback) is the spike that has
    // hard-hung this device when low on RAM or hot (warm-reset, cause=32).
    precheck_resources()?;

    let src = format!("{PROFILES_DIR}/{}.yaml", profile.id);
    let cfg = fs::read_to_string(&src).map_err(|_| "cached profile missing".to_string())?;
    if cfg.trim().is_empty() {
        return Err("cached profile is empty".into());
    }

    // Remember the node selected before the restart so we can restore it.
    let prev_node = current_node();

    atomic_write(ACTIVE_CONFIG, cfg.as_bytes())
        .map_err(|e| format!("cannot write active config: {e}"))?;
    // Keep the cfg subscription URL in sync so a later "refresh active" pulls
    // the right source (empty for uploaded profiles).
    let _ = set_cfg_subscription(if profile.kind == "url" { &profile.source_url } else { "" });
    // Point clash_modify at this profile's airline-specific DNS resolver (or
    // clear it → global default). Done BEFORE the restart so it takes effect in
    // a single restart. NB: do NOT delete cache.db — wiping it loses the fake-ip
    // cache and the restart already drops the node selection (restored below).
    write_proxy_dns(&profile.dns_servers);

    // Hot-reload the new config in-process instead of a full ShellCrash restart.
    // The restart tears down + rebuilds the transparent-proxy firewall, a spike
    // that has hard-hung the device (cause=32) even when healthy. The firewall
    // redirect is unchanged across a same-mode swap, so leaving it up is correct.
    // Fall back to the full restart only if the in-process reload fails, so a
    // switch never leaves the proxy down.
    if let Err(e) = reload_core() {
        restart_and_wait()
            .map_err(|re| format!("reload failed ({e}); restart fallback also failed: {re}"))?;
    }

    let nodes = count_nodes(&cfg);
    let mut idx = load_index();
    idx.active = profile.id.clone();
    if let Some(p) = idx.profiles.iter_mut().find(|p| p.id == profile.id) {
        p.node_count = nodes as u64;
    }
    let _ = write_index(&idx);

    let node = restore_node(&prev_node);
    Ok(apply_message(&profile.name, nodes, &node))
}

/// Compose a friendly job message including the resulting node selection.
fn apply_message(name: &str, nodes: usize, node: &Option<String>) -> String {
    match node {
        Some(n) => format!("Applied '{name}' — {nodes} nodes, on {n}"),
        None => format!("Applied '{name}' — {nodes} nodes"),
    }
}

/// Re-select the previously-active node after a restart if it still exists in
/// the (region-filtered) Proxies group. ShellCrash's restart otherwise drops
/// the selector back to the group's first member, which may be a dead node.
/// Returns the node the group ends up on (best-effort; None if API unreachable).
fn restore_node(prev: &Option<String>) -> Option<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(3)))
        .build()
        .into();
    let url = format!("{MIHOMO_API}/proxies/Proxies");
    let mut resp = agent.get(&url).call().ok()?;
    if resp.status().as_u16() >= 400 {
        return None;
    }
    let group: Value = resp.body_mut().read_json::<Value>().ok()?;
    let now = group.get("now").and_then(Value::as_str).map(str::to_string);

    if let Some(want) = prev {
        let in_group = group
            .get("all")
            .and_then(Value::as_array)
            .map(|a| a.iter().any(|n| n.as_str() == Some(want.as_str())))
            .unwrap_or(false);
        if in_group && now.as_deref() != Some(want.as_str()) {
            let body = json!({ "name": want });
            if agent.put(&url).send_json(&body).is_ok() {
                return Some(want.clone());
            }
        }
    }
    now
}

/// Restart ShellCrash and wait for the core to come back up.
fn restart_and_wait() -> Result<(), String> {
    // `ok` is ignored on purpose — restart prints a benign 'service delete (Not
    // found)' to stderr on first run yet still succeeds.
    let (_ok, out) = run_cmd(&format!("{INITD} restart"));
    for _ in 0..8 {
        std::thread::sleep(Duration::from_secs(1));
        if core_running() {
            return Ok(());
        }
    }
    Err(format!("core not running after restart: {}", tail(&out, 8)))
}

/// Apply a freshly-written active config WITHOUT a full ShellCrash restart.
///
/// `/etc/init.d/shellcrash restart` runs `fw_stop`+`fw_start` (tears down and
/// rebuilds the whole transparent-proxy firewall) and respawns the core — a
/// memory/CPU spike that has hard-hung this device (PMIC warm-reset, cause=32)
/// even when it was otherwise healthy. Instead, two steps that touch neither the
/// firewall nor the running core: first run ShellCrash's own `bfstart.sh` to
/// regenerate the runtime config (`SC_CONFIG_RUNTIME`) through the real
/// clash_modify + region_filter pipeline — the exact env a start uses; then
/// hot-reload that file in-process via mihomo `PUT /configs?force=true`.
/// The firewall redirect is identical across a same-mode profile swap, so
/// leaving it in place is both correct and what avoids the hang.
fn reload_core() -> Result<(), String> {
    // Nothing to reload into — let the caller fall back to a full start.
    if !core_running() {
        return Err("core not running".into());
    }
    // Regenerate SC_CONFIG_RUNTIME via ShellCrash's own prep script. bfstart skips
    // its network check once crash_start_time exists (set on boot), so on a live
    // box this only re-runs the config pipeline + a few file writes — no respawn,
    // no firewall, no core kill.
    let (ok, out) = run_cmd(&format!("{SC_DIR}/starts/bfstart.sh"));
    if !ok {
        return Err(format!("config regen failed: {}", tail(&out, 6)));
    }
    if !Path::new(SC_CONFIG_RUNTIME).exists() {
        return Err("runtime config missing after regen".into());
    }
    // force=true reloads even though the path is unchanged from the core's -f arg.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .into();
    let url = format!("{MIHOMO_API}/configs?force=true");
    let body = json!({ "path": SC_CONFIG_RUNTIME });
    match agent.put(&url).send_json(&body) {
        Ok(resp) if resp.status().as_u16() < 400 => {}
        Ok(resp) => {
            return Err(format!("core rejected the reload (HTTP {})", resp.status().as_u16()))
        }
        Err(e) => return Err(format!("reload request failed: {e}")),
    }
    if core_running() {
        Ok(())
    } else {
        Err("core died during reload".into())
    }
}

/// Guardrail for profile apply: refuse when the device is already low on memory
/// or thermally stressed — the two states in which a config-regen/firewall spike
/// has tipped it into a hard hang. Best-effort: a missing/unparseable reading
/// never blocks the apply, only an explicit over-threshold one does.
fn precheck_resources() -> Result<(), String> {
    if let Some(avail_kb) = mem_available_kb() {
        if avail_kb < MIN_MEM_AVAIL_KB {
            return Err(format!(
                "device low on memory ({} MB available) — switching now risks a hang; \
                 retry once it settles",
                avail_kb / 1024
            ));
        }
    }
    if let Some(milli_c) = max_temp_millic() {
        if milli_c >= MAX_TEMP_MILLIC {
            return Err(format!(
                "device too hot ({}°C) — switching now risks a hang; \
                 plug in / let it cool, then retry",
                milli_c / 1000
            ));
        }
    }
    Ok(())
}

fn mem_available_kb() -> Option<u64> {
    let s = fs::read_to_string("/proc/meminfo").ok()?;
    s.lines().find_map(|l| {
        let rest = l.strip_prefix("MemAvailable:")?;
        rest.trim().trim_end_matches("kB").trim().parse::<u64>().ok()
    })
}

/// Hottest thermal zone in milli-°C (zones report e.g. `48900` = 48.9°C). Scans
/// a fixed range and skips gaps so non-contiguous zone numbering is handled.
fn max_temp_millic() -> Option<i64> {
    let mut max: Option<i64> = None;
    for i in 0..64 {
        let p = format!("/sys/class/thermal/thermal_zone{i}/temp");
        if let Ok(s) = fs::read_to_string(&p) {
            if let Ok(v) = s.trim().parse::<i64>() {
                max = Some(max.map_or(v, |m| m.max(v)));
            }
        }
    }
    max
}

/// Write the profile's airline-specific resolvers to PROXY_DNS_LIST (one per
/// line). Empty → remove the file so clash_modify falls back to its built-in
/// default. Returns true if the on-disk content actually changed.
fn write_proxy_dns(servers: &[String]) -> bool {
    let desired = servers.join("\n");
    let current = fs::read_to_string(PROXY_DNS_LIST).unwrap_or_default();
    let current_norm = current.trim();
    if servers.is_empty() {
        if Path::new(PROXY_DNS_LIST).exists() {
            let _ = fs::remove_file(PROXY_DNS_LIST);
            return true;
        }
        return false;
    }
    if current_norm == desired.trim() {
        return false;
    }
    let _ = atomic_write(PROXY_DNS_LIST, format!("{desired}\n").as_bytes());
    true
}

/// Extract the airline-specific `proxy-server-nameserver` resolvers from a raw
/// subscription config. Conservative on purpose (so it never replaces a
/// working default with a guess):
///   1. an explicit `proxy-server-nameserver` block, else
///   2. resolvers from `nameserver-policy` entries whose key matches the node
///      servers' domain suffix (e.g. cloud-nodes' `+.cloud-nodes.com`),
/// normalizing a bare `IP:port` to `udp://IP:port` (the form mihomo needs).
/// Returns empty when nothing airline-specific is found (→ global default).
fn extract_proxy_dns(cfg: &str) -> Vec<String> {
    // 1. explicit proxy-server-nameserver
    let explicit = collect_resolvers_after(cfg, "proxy-server-nameserver");
    if !explicit.is_empty() {
        return cap_dns(explicit);
    }
    // 2. nameserver-policy entries matching the node domains' suffix
    let suffixes = node_domain_suffixes(cfg);
    if !suffixes.is_empty() {
        let mut out = Vec::new();
        for raw in policy_lines(cfg) {
            // raw is like:  +.cloud-nodes.com: '124.221.68.73:1053'
            // or            +.cloud-nodes.com: [ udp://1.2.3.4:53 ]
            let (key, val) = match raw.split_once(':') {
                Some((k, v)) => (k.trim().trim_matches(['\'', '"', '{', ' ']), v),
                None => continue,
            };
            let key_host = key.trim_start_matches("+.").trim_start_matches('.');
            if key_host.is_empty() || !suffixes.iter().any(|s| key_host.ends_with(s.as_str())) {
                continue;
            }
            for r in split_resolver_values(val) {
                out.push(normalize_resolver(&r));
            }
        }
        if !out.is_empty() {
            return cap_dns(out);
        }
    }
    Vec::new()
}

fn cap_dns(mut v: Vec<String>) -> Vec<String> {
    // Drop anything that wouldn't survive the YAML/heredoc round-trip — this is
    // the single choke point for BOTH extraction paths, so a malformed entry
    // parsed out of a subscription (e.g. a comma-split array fragment like
    // `53]`) never reaches proxy_dns.list.
    v.retain(|s| valid_resolver(s));
    v.dedup();
    v.truncate(MAX_DNS_SERVERS);
    v
}

/// Collect resolver entries that appear right after `key:` — handles both the
/// flow form `key: [ a, b ]` and the block form `key:\n  - a\n  - b`.
fn collect_resolvers_after(cfg: &str, key: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut lines = cfg.lines();
    while let Some(line) = lines.next() {
        let t = line.trim_start();
        if !t.starts_with(key) {
            continue;
        }
        let after = match t.split_once(':') {
            Some((k, v)) if k.trim_end().trim_end_matches(' ') == key => v.trim(),
            _ => continue,
        };
        if after.starts_with('[') {
            // flow: [ a, b ]
            for r in split_resolver_values(after) {
                out.push(normalize_resolver(&r));
            }
        } else if after.is_empty() {
            // block list on following indented `- ` lines
            for bl in lines.by_ref() {
                let bt = bl.trim_start();
                if let Some(item) = bt.strip_prefix("- ") {
                    out.push(normalize_resolver(item.trim()));
                } else if bt.is_empty() {
                    continue;
                } else {
                    break;
                }
            }
        } else {
            out.push(normalize_resolver(after));
        }
        break;
    }
    out
}

/// The `nameserver-policy:` body lines (flow `{a: b, c: d}` split into entries,
/// or block `  key: val` lines).
fn policy_lines(cfg: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut lines = cfg.lines();
    while let Some(line) = lines.next() {
        let t = line.trim_start();
        if !t.starts_with("nameserver-policy") {
            continue;
        }
        let after = t.split_once(':').map(|(_, v)| v.trim()).unwrap_or("");
        if after.starts_with('{') {
            // flow map: split on commas at top level (resolvers here rarely
            // contain commas; arrays use a single element in practice)
            let inner = after.trim_matches(['{', '}', ' ']);
            for part in inner.split(',') {
                if part.contains(':') {
                    out.push(part.trim().to_string());
                }
            }
        } else if after.is_empty() {
            for bl in lines.by_ref() {
                let bt = bl.trim_start();
                if bt.is_empty() {
                    continue;
                }
                // a less-indented top-level key ends the block
                if !bl.starts_with(' ') {
                    break;
                }
                if bt.contains(':') {
                    out.push(bt.to_string());
                }
            }
        }
        break;
    }
    out
}

/// Split the value side of a resolver entry into individual resolvers, handling
/// `[ a, b ]`, quotes, and bare scalars.
fn split_resolver_values(val: &str) -> Vec<String> {
    val.trim()
        .trim_matches(['[', ']', '{', '}'])
        .split(',')
        .map(|s| s.trim().trim_matches(['\'', '"', ' ']).to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// A bare `IP:port` or `IP` resolver is dialed over UDP by mihomo only with an
/// explicit scheme; add `udp://` so it actually works (matches the known-good
/// cloud-nodes fix). Scheme-qualified entries (https/tls/quic/udp/tcp/dhcp/system)
/// pass through unchanged.
fn normalize_resolver(r: &str) -> String {
    let r = r.trim().trim_matches(['\'', '"']);
    let has_scheme = r.contains("://")
        || r == "system"
        || r == "dhcp"
        || r.starts_with("system://")
        || r.starts_with("dhcp://");
    if r.is_empty() || has_scheme {
        return r.to_string();
    }
    // looks like an IP / IP:port / host:port without scheme → udp
    format!("udp://{r}")
}

/// Registrable-ish domain suffixes of the proxy node `server:` values, used to
/// match nameserver-policy keys. Takes the last two labels (e.g. host
/// `ae982dcae.4z6iniwsy.sbs` → `4z6iniwsy.sbs`).
fn node_domain_suffixes(cfg: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut in_block = false;
    let mut item_indent: Option<usize> = None;
    for line in cfg.lines() {
        if !in_block {
            if line.starts_with("proxies:") {
                in_block = true;
            }
            continue;
        }
        let trimmed = line.trim_start();
        let lead = line.len() - trimmed.len();
        if !trimmed.is_empty() && lead == 0 && !line.starts_with('-') {
            break;
        }
        let is_item = trimmed.starts_with("- ") || trimmed == "-";
        if is_item {
            match item_indent {
                None => item_indent = Some(lead),
                Some(i) if i == lead => {}
                _ => continue,
            }
        }
        if let Some(pos) = line.find("server:") {
            let rest = line[pos + "server:".len()..].trim();
            let host: String = rest
                .trim_start_matches(['{', ' '])
                .chars()
                .take_while(|c| !matches!(c, ',' | '}' | ' ' | '"' | '\''))
                .collect();
            // only domains (contain a letter), not IPs
            if host.contains(|c: char| c.is_ascii_alphabetic()) {
                let labels: Vec<&str> = host.trim_end_matches('.').split('.').collect();
                if labels.len() >= 2 {
                    let suffix = labels[labels.len() - 2..].join(".");
                    if !out.contains(&suffix) {
                        out.push(suffix);
                    }
                }
            }
        }
        if out.len() >= 4 {
            break;
        }
    }
    out
}

/// Read the live `proxy-server-nameserver` from the running config (used to
/// back-fill the adopted "Current" profile with its known-working resolvers).
fn live_proxy_dns() -> Vec<String> {
    let cfg = fs::read_to_string(SC_CONFIG_RUNTIME).unwrap_or_default();
    cap_dns(collect_resolvers_after(&cfg, "proxy-server-nameserver"))
}

/* -------------------------------------------------------------- *
 *  Job bookkeeping
 * -------------------------------------------------------------- */

fn try_start_job(state: &AppState, kind: &str, profile_id: &str) -> Option<u64> {
    if state
        .sc_admin
        .running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::Relaxed)
        .is_err()
    {
        return None;
    }
    let mut j = state.sc_admin.job.lock().unwrap();
    let next = j.id.wrapping_add(1);
    *j = JobState {
        id: next,
        kind: kind.to_string(),
        status: "running".into(),
        message: String::new(),
        profile_id: profile_id.to_string(),
        started_unix: now_unix(),
        finished_unix: 0,
    };
    Some(next)
}

fn finish_job(job: &Arc<Mutex<JobState>>, running: &Arc<AtomicBool>, res: Result<String, String>) {
    {
        let mut j = job.lock().unwrap();
        match res {
            Ok(m) => {
                j.status = "done".into();
                j.message = m;
            }
            Err(e) => {
                j.status = "error".into();
                j.message = e;
            }
        }
        j.finished_unix = now_unix();
    }
    running.store(false, Ordering::Relaxed);
}

/* -------------------------------------------------------------- *
 *  Index persistence
 * -------------------------------------------------------------- */

fn load_index() -> Index {
    if let Ok(s) = fs::read_to_string(INDEX_PATH) {
        if let Ok(idx) = serde_json::from_str::<Index>(&s) {
            return idx;
        }
    }
    bootstrap_index()
}

/// First run: adopt the currently-active config as the initial profile so the
/// list is never empty on a device that already runs ShellCrash, and import the
/// historical `config.yaml.bak` ShellCrash keeps (if it's a distinct config).
fn bootstrap_index() -> Index {
    let _ = fs::create_dir_all(PROFILES_DIR);
    let mut idx = Index::default();

    if let Ok(cfg) = fs::read_to_string(ACTIVE_CONFIG) {
        if !cfg.trim().is_empty() {
            let https = read_cfg_https();
            let kind = if https.is_empty() { "upload" } else { "url" };
            let id = make_id("current");
            let dest = format!("{PROFILES_DIR}/{id}.yaml");
            if atomic_write(&dest, cfg.as_bytes()).is_ok() {
                let now = now_unix();
                // Back-fill DNS from the LIVE running config (known-working
                // resolvers), not from the subscription's own block which may be
                // stale — keeps the adopted profile's egress intact when applied.
                let mut dns = live_proxy_dns();
                if dns.is_empty() {
                    dns = extract_proxy_dns(&cfg);
                }
                idx.profiles.push(Profile {
                    id: id.clone(),
                    name: "Current".into(),
                    kind: kind.into(),
                    source_url: https,
                    added_unix: now,
                    updated_unix: now,
                    node_count: count_nodes(&cfg) as u64,
                    dns_servers: dns,
                });
                idx.active = id;
            }

            // Import the historical backup as a second, switchable profile if it
            // looks like a real config and differs from the current one.
            if let Ok(bak) = fs::read_to_string(LEGACY_BAK) {
                if looks_like_clash_config(&bak) && bak.trim() != cfg.trim() && count_nodes(&bak) > 0 {
                    let id = make_id("previous");
                    let dest = format!("{PROFILES_DIR}/{id}.yaml");
                    if atomic_write(&dest, bak.as_bytes()).is_ok() {
                        let now = now_unix();
                        idx.profiles.push(Profile {
                            id,
                            name: "Previous".into(),
                            kind: "upload".into(),
                            source_url: String::new(),
                            added_unix: now,
                            updated_unix: now,
                            node_count: count_nodes(&bak) as u64,
                            dns_servers: extract_proxy_dns(&bak),
                        });
                    }
                }
            }
        }
    }
    let _ = write_index(&idx);
    idx
}

fn write_index(idx: &Index) -> std::io::Result<()> {
    let _ = fs::create_dir_all(PROFILES_DIR);
    let data = serde_json::to_vec_pretty(idx).unwrap_or_else(|_| b"{}".to_vec());
    atomic_write(INDEX_PATH, &data)
}

fn upsert(idx: &mut Index, p: Profile) {
    if let Some(existing) = idx.profiles.iter_mut().find(|e| e.id == p.id) {
        *existing = p;
    } else {
        idx.profiles.push(p);
    }
}

/* -------------------------------------------------------------- *
 *  Helpers
 * -------------------------------------------------------------- */

fn profile_json(p: &Profile, active: bool, current_node: Option<String>) -> Value {
    json!({
        "id": p.id,
        "name": p.name,
        "kind": p.kind,
        "source_url": p.source_url,
        "added_unix": p.added_unix,
        "updated_unix": p.updated_unix,
        "node_count": p.node_count,
        "active": active,
        "current_node": current_node,
        "dns_servers": p.dns_servers,
    })
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Slug from the name plus a nanosecond suffix for uniqueness. ASCII-only, so
/// byte slicing is safe; never contains a path separator.
fn make_id(name: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
    }
    let slug = slug.trim_matches('-');
    let base = if slug.is_empty() { "profile" } else { &slug[..slug.len().min(24)] };
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    format!("{base}-{nanos:x}")
}

/// Count proxy nodes in a clash config's top-level `proxies:` block. Only list
/// items at the block's own indent are counted (so a node's nested arrays —
/// `alpn:` etc. — are not miscounted).
fn count_nodes(yaml: &str) -> usize {
    let mut in_block = false;
    let mut item_indent: Option<usize> = None;
    let mut count = 0;
    for line in yaml.lines() {
        if !in_block {
            if line.starts_with("proxies:") {
                in_block = true;
            }
            continue;
        }
        let trimmed = line.trim_start();
        let lead = line.len() - trimmed.len();
        // A non-indented, non-list line is the next top-level key → block ended.
        if !trimmed.is_empty() && lead == 0 && !line.starts_with('-') {
            break;
        }
        let is_item = trimmed.starts_with("- ") || trimmed == "-";
        if is_item {
            match item_indent {
                None => {
                    item_indent = Some(lead);
                    count += 1;
                }
                Some(i) if i == lead => count += 1,
                _ => {}
            }
        }
    }
    count
}

fn looks_like_clash_config(content: &str) -> bool {
    content.contains("proxies:") || content.contains("proxy-providers:")
}

/// A profile display name: trim, drop control chars, cap length. Stored only in
/// index.json and echoed as JSON, so the only real constraints are sanity ones.
fn sanitize_name(raw: &str) -> String {
    raw.trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(64)
        .collect::<String>()
        .trim()
        .to_string()
}

fn valid_url(u: &str) -> bool {
    (u.starts_with("http://") || u.starts_with("https://"))
        && u.len() <= MAX_URL_LEN
        && !u.contains('\'')
        && !u.contains('"')
        && !u.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// A DNS resolver entry is written one-per-line to proxy_dns.list and then
/// interpolated by clash_modify.sh into an UNQUOTED YAML flow array `[ $psn ]`
/// inside a shell heredoc. Whitelist only the characters a resolver URL
/// (`scheme://host[:port][/path]`) actually needs — this rejects `#` (would
/// start a YAML comment and silently truncate the array), `,`/`[`/`]` (break
/// the array), and every shell metacharacter in one stroke. IPv6-literal
/// resolvers (need `[]`) are intentionally unsupported — they can't live in an
/// unquoted YAML flow array anyway.
fn valid_resolver(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 256
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, ':' | '/' | '.' | '-' | '_' | '%' | '+')
        })
}

/// Read the live subscription URL from ShellCrash.cfg (`Https='...'`).
fn read_cfg_https() -> String {
    let content = fs::read_to_string(CFG_PATH).unwrap_or_default();
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("Https=") {
            return rest.trim().trim_matches('\'').trim_matches('"').to_string();
        }
    }
    String::new()
}

/// Rewrite the `Url=`/`Https=` lines in ShellCrash.cfg (mirrors `setconfig`).
/// `url` is validated single-quote-free by the caller, so single-quoting is safe.
fn set_cfg_subscription(url: &str) -> std::io::Result<()> {
    let content = fs::read_to_string(CFG_PATH).unwrap_or_default();
    let mut out = String::with_capacity(content.len() + 64);
    for line in content.lines() {
        if line.starts_with("Https=") || line.starts_with("Url=") {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("Url=\n");
    if url.is_empty() {
        out.push_str("Https=\n");
    } else {
        out.push_str(&format!("Https='{url}'\n"));
    }
    atomic_write(CFG_PATH, out.as_bytes())
}

fn atomic_write(path: &str, data: &[u8]) -> std::io::Result<()> {
    let tmp = format!("{path}.tmp");
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)
}

fn run_cmd(cmd: &str) -> (bool, String) {
    match Command::new("sh").arg("-c").arg(cmd).output() {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            s.push_str(&String::from_utf8_lossy(&o.stderr));
            (o.status.success(), s)
        }
        Err(e) => (false, format!("exec failed: {e}")),
    }
}

fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join(" | ")
}

fn core_running() -> bool {
    Command::new("pidof")
        .arg("CrashCore")
        .output()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false)
}

/// Currently-selected node of the main `Proxies` group, via mihomo's API.
fn current_node() -> Option<String> {
    if !Path::new(SC_DIR).exists() {
        return None;
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_millis(1200)))
        .build()
        .into();
    let mut resp = agent.get(&format!("{MIHOMO_API}/proxies/Proxies")).call().ok()?;
    if resp.status().as_u16() >= 400 {
        return None;
    }
    let v: Value = resp.body_mut().read_json::<Value>().ok()?;
    v.get("now").and_then(Value::as_str).map(|s| s.to_string())
}
