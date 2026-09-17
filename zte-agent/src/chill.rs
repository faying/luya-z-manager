//! CHILL (native mihomo, `/data/chill`) status + control.
//!
//! CHILL's own `chill.sh` already does the hard parts safely: render + `mihomo
//! -t` validate + hot-reload (keeps the old config on failure), start/stop
//! bookkeeping (`/data/chill/disabled`), and the health/crash/backoff loop that
//! maintains `/tmp/chill.state`. This module is a thin control layer over that:
//! read the state file, talk to mihomo's own REST API at `127.0.0.1:9999`
//! (region/AI-exit switch, provider refresh), and shell out to `chill.sh` for
//! anything that needs a config re-render.
//!
//! Deliberately no Bearer token / mihomo `secret` here (matches the design
//! doc's phase-1 contract): setting one would 401 the touchscreen panel until
//! it's recompiled to send it, which is separate, harder-dependency work.
//! `:9999` stays LAN-bound with `CHILL_API_LAN`/`CHILL_API_ALLOW_IP`
//! (chill.env) as the access control instead of a secret.

use std::fs;
use std::io::BufRead;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{json, Value};

use crate::handlers::AppState;

const CHILL_ENV: &str = "/data/chill/chill.env";
const CHILL_SH: &str = "/data/chill/chill.sh";
const STATE_PATH: &str = "/tmp/chill.state";
const LOG_PATH: &str = "/tmp/chill.log";
const MIHOMO_API: &str = "http://127.0.0.1:9999";

const HTTP_TIMEOUT: Duration = Duration::from_secs(3);
// Provider refresh / reload can involve an actual subscription fetch, give it more room.
const SLOW_HTTP_TIMEOUT: Duration = Duration::from_secs(15);
const LOG_TAIL_DEFAULT: usize = 200;
const LOG_TAIL_MAX: usize = 2000;

// The five `type: select` groups defined in template.yaml — the only groups a
// region/AI-exit switch is allowed to touch. Whitelisted so a client can't PUT
// an arbitrary mihomo group name.
const REGION_GROUPS: [&str; 5] = ["🚀 节点选择", "🤖 AI", "📺 流媒体", "🍎 Apple", "🐟 漏网之鱼"];
const MAIN_GROUP: &str = "🚀 节点选择";
const AI_GROUP: &str = "🤖 AI";

// Must match chill.sh's own $BYPASS_PRIO — the `ip rule` priority its
// apply_bypass()/flush() manage. Changing this here without changing chill.sh
// would leave stale rules neither side cleans up.
const BYPASS_PRIO: &str = "8999";
const MAX_BYPASS_IPS: usize = 32;

// Of the four proxy-providers (oix/shouhou/nexi/tag), only shouhou is a live
// http subscription (see template.yaml comments) — the others are static file
// snapshots with no URL to edit.
const EDITABLE_PROVIDER: &str = "shouhou";
const MAX_URL_LEN: usize = 4096;

// Resource guardrail: a config-regen/restart spike has hard-hung this device
// when it was already low on memory or thermally stressed. Read straight out
// of chill.state instead of re-deriving from /proc, since chill.sh's own
// health loop already computes these every 60s.
const MIN_MEM_AVAIL_MB: i64 = 100;
const MAX_TEMP_C: i64 = 78;

/* -------------------------------------------------------------- *
 *  Job state (single in-flight background op — reload/enable/disable)
 * -------------------------------------------------------------- */

#[derive(Clone, Serialize)]
pub struct JobState {
    id: u64,
    kind: String,
    status: String, // idle | running | done | error
    message: String,
    started_unix: u64,
    finished_unix: u64,
}

pub struct ChillAdmin {
    job: Arc<Mutex<JobState>>,
    running: Arc<AtomicBool>,
}

impl ChillAdmin {
    pub fn new() -> Self {
        Self {
            job: Arc::new(Mutex::new(JobState {
                id: 0,
                kind: String::new(),
                status: "idle".into(),
                message: String::new(),
                started_unix: 0,
                finished_unix: 0,
            })),
            running: Arc::new(AtomicBool::new(false)),
        }
    }
}

/* -------------------------------------------------------------- *
 *  Status + log
 * -------------------------------------------------------------- */

/// GET /api/services/chill
pub fn status(_state: &AppState) -> (u16, Value) {
    let Some(st) = read_chill_state() else {
        // chill.sh hasn't written a state file yet (never started since boot).
        return (200, json!({"ok": true, "data": {"state": "unknown"}}));
    };
    let running = st.get("state").and_then(Value::as_str) == Some("running");
    let mut data = st;
    if running {
        let agent = mihomo_agent(HTTP_TIMEOUT);
        let secret = mihomo_secret();
        let version = http_get_json(&agent, &format!("{MIHOMO_API}/version"), &secret);
        let proxies_raw = http_get_json(&agent, &format!("{MIHOMO_API}/proxies"), &secret);
        data["version"] = json!(version
            .as_ref()
            .and_then(|v| v.get("version"))
            .and_then(Value::as_str));
        data["groups"] = json!(summarize_groups(&proxies_raw));
        data["region"] = json!(group_now(&proxies_raw, MAIN_GROUP));
        data["ai_exit"] = json!(group_now(&proxies_raw, AI_GROUP));
    }
    (200, json!({"ok": true, "data": data}))
}

/// GET /api/services/chill/log?lines=N
pub fn log(query: &str) -> (u16, Value) {
    let n = parse_lines(query);
    let lines = tail_file(LOG_PATH, n).unwrap_or_default();
    (
        200,
        json!({"ok": true, "data": {"path": LOG_PATH, "lines": lines, "limit": n}}),
    )
}

/* -------------------------------------------------------------- *
 *  Providers (subscriptions)
 * -------------------------------------------------------------- */

/// GET /api/services/chill/providers
pub fn providers_list(_state: &AppState) -> (u16, Value) {
    let agent = mihomo_agent(HTTP_TIMEOUT);
    let raw = http_get_json(&agent, &format!("{MIHOMO_API}/providers/proxies"), &mihomo_secret());
    let Some(providers) = raw
        .as_ref()
        .and_then(|v| v.get("providers"))
        .and_then(Value::as_object)
    else {
        return (200, json!({"ok": true, "data": {"providers": []}}));
    };

    let mut out: Vec<Value> = providers
        .iter()
        // mihomo auto-wraps DIRECT/REJECT and every proxy-group into a synthetic
        // "Compatible" provider too — those aren't real subscriptions, skip them.
        .filter(|(_, info)| {
            !info
                .get("vehicleType")
                .and_then(Value::as_str)
                .unwrap_or("")
                .eq_ignore_ascii_case("compatible")
        })
        .map(|(name, info)| {
            json!({
                "name": name,
                "vehicle_type": info.get("vehicleType").and_then(Value::as_str),
                "updated_at": info.get("updatedAt").and_then(Value::as_str),
                "node_count": info.get("proxies").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0),
                "subscription": info.get("subscriptionInfo").cloned(),
                "editable": name.as_str() == EDITABLE_PROVIDER,
            })
        })
        .collect();
    out.sort_by(|a, b| a["name"].as_str().unwrap_or("").cmp(b["name"].as_str().unwrap_or("")));

    (200, json!({"ok": true, "data": {"providers": out}}))
}

/// PUT /api/services/chill/providers — body {name, url}
///
/// Only `shouhou` has an editable URL. Rewrites `SUB_SHOUHOU` in chill.env,
/// then backgrounds `chill.sh reload` (its own render step validates via
/// `mihomo -t` and keeps the old config on failure — nothing to duplicate here).
pub fn providers_set_url(state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let name = parsed["name"].as_str().unwrap_or("").trim();
    if name != EDITABLE_PROVIDER {
        return (
            400,
            json!({"ok": false, "error": format!("provider '{name}' has no editable URL")}),
        );
    }
    let url = parsed["url"].as_str().unwrap_or("").trim().to_string();
    if !valid_url(&url) {
        return (400, json!({"ok": false, "error": "invalid URL (http(s) only, no quotes/spaces)"}));
    }
    if let Err(e) = precheck_resources() {
        return (409, json!({"ok": false, "error": e}));
    }
    if let Err(e) = set_env_var("SUB_SHOUHOU", &url) {
        return (500, json!({"ok": false, "error": e}));
    }
    start_job(state, "reload", do_reload)
}

/// POST /api/services/chill/providers/refresh — body {name}
///
/// Native mihomo op (re-fetch/re-read that one provider) — synchronous, no
/// config regen, no job needed.
pub fn providers_refresh(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let name = parsed["name"].as_str().unwrap_or("").trim().to_string();
    if name.is_empty() {
        return (400, json!({"ok": false, "error": "missing 'name'"}));
    }
    let agent = mihomo_agent(SLOW_HTTP_TIMEOUT);
    let url = format!("{MIHOMO_API}/providers/proxies/{}", urlencode(&name));
    match mihomo_put_json(&agent, &url, &mihomo_secret(), json!({})) {
        Ok(code) if code < 400 => (200, json!({"ok": true})),
        Ok(code) => (
            502,
            json!({"ok": false, "error": format!("mihomo rejected refresh (HTTP {code})")}),
        ),
        Err(e) => (502, json!({"ok": false, "error": format!("refresh request failed: {e}")})),
    }
}

fn do_reload() -> Result<String, String> {
    let (ok, out) = run_cmd(&format!("{CHILL_SH} reload"));
    if ok {
        Ok("Reloaded".into())
    } else {
        Err(format!("reload failed: {}", tail(&out, 6)))
    }
}

/* -------------------------------------------------------------- *
 *  Regions / AI exit (proxy-group member switch)
 * -------------------------------------------------------------- */

/// PUT /api/services/chill/regions — body {group, member}
///
/// Just a validated passthrough to mihomo's own `PUT /proxies/{group}` — no
/// reload, no config touch (design doc's "type 1" operation class).
pub fn regions_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let group = parsed["group"].as_str().unwrap_or("").trim().to_string();
    let member = parsed["member"].as_str().unwrap_or("").trim().to_string();
    if !REGION_GROUPS.contains(&group.as_str()) {
        return (400, json!({"ok": false, "error": "unknown group"}));
    }
    if member.is_empty() {
        return (400, json!({"ok": false, "error": "missing 'member'"}));
    }

    let agent = mihomo_agent(HTTP_TIMEOUT);
    let secret = mihomo_secret();
    let group_url = format!("{MIHOMO_API}/proxies/{}", urlencode(&group));
    let Some(info) = http_get_json(&agent, &group_url, &secret) else {
        return (502, json!({"ok": false, "error": "mihomo unreachable"}));
    };
    let in_group = info
        .get("all")
        .and_then(Value::as_array)
        .map(|a| a.iter().any(|n| n.as_str() == Some(member.as_str())))
        .unwrap_or(false);
    if !in_group {
        return (400, json!({"ok": false, "error": "member not in group"}));
    }

    match mihomo_put_json(&agent, &group_url, &secret, json!({"name": member})) {
        Ok(code) if code < 400 => (200, json!({"ok": true, "data": {"group": group, "active": member}})),
        Ok(code) => (
            502,
            json!({"ok": false, "error": format!("mihomo rejected switch (HTTP {code})")}),
        ),
        Err(e) => (502, json!({"ok": false, "error": format!("switch request failed: {e}")})),
    }
}

/* -------------------------------------------------------------- *
 *  Device bypass (per-device source-IP rule)
 * -------------------------------------------------------------- */

/// GET /api/services/chill/bypass
pub fn bypass_get(_state: &AppState) -> (u16, Value) {
    let ips = read_bypass_ips();
    let stale = read_chill_state()
        .and_then(|v| v.get("bypass_stale").cloned())
        .unwrap_or(Value::Array(vec![]));
    (200, json!({"ok": true, "data": {"ips": ips, "stale": stale}}))
}

/// PUT /api/services/chill/bypass — body {ips: [...]} (full desired list)
///
/// Only adds/removes `ip rule` entries at BYPASS_PRIO — no reload, no config
/// touch (design doc's "type 2" operation class) — then persists the list to
/// chill.env so a future restart's `apply_bypass()` reproduces the same rules
/// (that function only runs at core start, not on every edit).
pub fn bypass_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let Some(arr) = parsed["ips"].as_array() else {
        return (400, json!({"ok": false, "error": "missing 'ips' array"}));
    };
    let mut ips: Vec<String> = arr
        .iter()
        .filter_map(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if ips.len() > MAX_BYPASS_IPS {
        return (400, json!({"ok": false, "error": "too many IPs"}));
    }
    if let Some(bad) = ips.iter().find(|ip| !valid_ipv4(ip)) {
        return (400, json!({"ok": false, "error": format!("invalid IP: {bad}")}));
    }
    ips.sort();
    ips.dedup();

    remove_bypass_rules();
    for ip in &ips {
        let (ok, out) = run_cmd(&format!("ip rule add from {ip} priority {BYPASS_PRIO} lookup main"));
        if !ok {
            return (
                500,
                json!({"ok": false, "error": format!("failed to add rule for {ip}: {}", tail(&out, 3))}),
            );
        }
    }
    if let Err(e) = set_env_var("CHILL_BYPASS_IP", &ips.join(" ")) {
        return (500, json!({"ok": false, "error": e}));
    }
    (200, json!({"ok": true, "data": {"ips": ips}}))
}

/// Remove every `ip rule` at BYPASS_PRIO one at a time. Mirrors chill.sh's own
/// flush() loop — `ip rule del pref N` on this device's `ip` only removes one
/// matching entry per call even when several share a priority.
fn remove_bypass_rules() {
    for _ in 0..MAX_BYPASS_IPS + 8 {
        let (_, out) = run_cmd(&format!(
            "ip rule show | awk -F: '$1+0=={BYPASS_PRIO} {{print $1; exit}}'"
        ));
        let pref = out.trim();
        if pref.is_empty() {
            break;
        }
        let _ = run_cmd(&format!("ip rule del pref {pref}"));
    }
}

/* -------------------------------------------------------------- *
 *  Enable / disable (total on/off)
 * -------------------------------------------------------------- */

/// POST /api/services/chill/enable
pub fn enable(state: &AppState) -> (u16, Value) {
    if let Err(e) = precheck_resources() {
        return (409, json!({"ok": false, "error": e}));
    }
    start_job(state, "enable", || run_chill_sh("start"))
}

/// POST /api/services/chill/disable
pub fn disable(state: &AppState) -> (u16, Value) {
    start_job(state, "disable", || run_chill_sh("stop"))
}

fn run_chill_sh(verb: &str) -> Result<String, String> {
    let (ok, out) = run_cmd(&format!("{CHILL_SH} {verb}"));
    if ok {
        Ok(format!("chill.sh {verb} ok"))
    } else {
        Err(format!("chill.sh {verb} failed: {}", tail(&out, 6)))
    }
}

/// GET /api/services/chill/job
pub fn job(state: &AppState) -> (u16, Value) {
    let j = state.chill.job.lock().unwrap().clone();
    (200, json!({"ok": true, "data": j}))
}

/* -------------------------------------------------------------- *
 *  Job bookkeeping — single in-flight op, same shape as esim.rs
 *  (Arc<Mutex<JobState>> + Arc<AtomicBool>, catch_unwind around the worker
 *  so a panic still releases the slot).
 * -------------------------------------------------------------- */

fn start_job<F>(state: &AppState, kind: &str, f: F) -> (u16, Value)
where
    F: FnOnce() -> Result<String, String> + Send + 'static,
{
    let job_id = match try_start_job(state, kind) {
        Some(j) => j,
        None => return (409, json!({"ok": false, "error": "operation in progress"})),
    };
    let job = Arc::clone(&state.chill.job);
    let running = Arc::clone(&state.chill.running);
    std::thread::spawn(move || {
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
            .unwrap_or_else(|_| Err("internal panic".into()));
        finish_job(&job, &running, res);
    });
    (200, json!({"ok": true, "data": {"job_id": job_id, "status": "running"}}))
}

fn try_start_job(state: &AppState, kind: &str) -> Option<u64> {
    if state
        .chill
        .running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::Relaxed)
        .is_err()
    {
        return None;
    }
    let mut j = state.chill.job.lock().unwrap();
    let next = j.id.wrapping_add(1);
    *j = JobState {
        id: next,
        kind: kind.to_string(),
        status: "running".into(),
        message: String::new(),
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
 *  Resource precheck
 * -------------------------------------------------------------- */

fn precheck_resources() -> Result<(), String> {
    // No state file yet (never started since boot) — nothing to precheck against.
    let Some(st) = read_chill_state() else { return Ok(()) };
    if let Some(mb) = st.get("mem_avail_mb").and_then(Value::as_i64) {
        if mb < MIN_MEM_AVAIL_MB {
            return Err(format!(
                "device low on memory ({mb} MB available) — this risks a hang; retry once it settles"
            ));
        }
    }
    if let Some(c) = st.get("cpuss_c").and_then(Value::as_i64) {
        if c >= MAX_TEMP_C {
            return Err(format!("device too hot ({c}°C) — this risks a hang; let it cool, then retry"));
        }
    }
    Ok(())
}

/* -------------------------------------------------------------- *
 *  chill.env read/write
 * -------------------------------------------------------------- */

fn read_env_var(name: &str) -> String {
    let content = fs::read_to_string(CHILL_ENV).unwrap_or_default();
    let prefix = format!("{name}=");
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix(&prefix) {
            return rest.trim().trim_matches('\'').trim_matches('"').to_string();
        }
    }
    String::new()
}

fn read_bypass_ips() -> Vec<String> {
    read_env_var("CHILL_BYPASS_IP")
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// Atomically rewrite a single `KEY='...'` line in chill.env. chill.sh sources
/// this file as shell, so the value is single-quoted the same way it already
/// quotes SUB_SHOUHOU — callers (valid_url/valid_ipv4) never allow a `'`
/// through, but the check is repeated here as the last line of defense.
fn set_env_var(name: &str, value: &str) -> Result<(), String> {
    if value.contains('\'') {
        return Err(format!("{name} value cannot contain a single quote"));
    }
    let content = fs::read_to_string(CHILL_ENV).unwrap_or_default();
    let prefix = format!("{name}=");
    let mut out = String::with_capacity(content.len() + 64);
    for line in content.lines() {
        if line.starts_with(&prefix) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(&format!("{name}='{value}'\n"));
    atomic_write(CHILL_ENV, out.as_bytes()).map_err(|e| format!("cannot write chill.env: {e}"))
}

/* -------------------------------------------------------------- *
 *  Helpers
 * -------------------------------------------------------------- */

fn read_chill_state() -> Option<Value> {
    let s = fs::read_to_string(STATE_PATH).ok()?;
    serde_json::from_str::<Value>(&s).ok()
}

/// Reduce mihomo's `/proxies` response to the 5 known select-groups (skips the
/// hundreds of raw node entries — same idea as services.rs's summarize_proxies,
/// duplicated here since it's filtered to a different, CHILL-specific set).
fn summarize_groups(raw: &Option<Value>) -> Vec<Value> {
    let Some(proxies) = raw.as_ref().and_then(|v| v.get("proxies")).and_then(Value::as_object) else {
        return Vec::new();
    };
    REGION_GROUPS
        .iter()
        .filter_map(|name| {
            let info = proxies.get(*name)?;
            Some(json!({
                "name": name,
                "now": info.get("now").and_then(Value::as_str),
                "size": info.get("all").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0),
            }))
        })
        .collect()
}

fn group_now(raw: &Option<Value>, group: &str) -> Option<Value> {
    let proxies = raw.as_ref()?.get("proxies")?.as_object()?;
    let g = proxies.get(group)?;
    Some(json!({
        "active": g.get("now").and_then(Value::as_str),
        "options": g.get("all").cloned().unwrap_or(Value::Array(vec![])),
    }))
}

fn mihomo_agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .build()
        .into()
}

/// mihomo's own `CHILL_SECRET`, read from chill.env — empty when the
/// device hasn't had a secret configured yet (mihomo then ignores/doesn't
/// require the header, matching touch-ui's `chill.conf` `secret=` convention).
fn mihomo_secret() -> String {
    read_env_var("CHILL_SECRET")
}

/// `Some("Bearer <secret>")` when a secret is configured, else `None` — the
/// caller adds the header only in the `Some` case (an empty header would be
/// a needless deviation from "no secret configured ⇒ no auth at all").
fn bearer(secret: &str) -> Option<String> {
    (!secret.is_empty()).then(|| format!("Bearer {secret}"))
}

fn http_get_json(agent: &ureq::Agent, url: &str, secret: &str) -> Option<Value> {
    let mut req = agent.get(url);
    if let Some(h) = bearer(secret) {
        req = req.header("Authorization", h);
    }
    let mut resp = req.call().ok()?;
    if resp.status().as_u16() >= 400 {
        return None;
    }
    resp.body_mut().read_json::<Value>().ok()
}

/// PUT a JSON body to mihomo with the Bearer header when a secret is set.
/// Centralizes the two `agent.put(...).send_json(...)` call sites so the
/// auth header only needs writing once.
fn mihomo_put_json(agent: &ureq::Agent, url: &str, secret: &str, body: Value) -> Result<u16, String> {
    let mut req = agent.put(url);
    if let Some(h) = bearer(secret) {
        req = req.header("Authorization", h);
    }
    match req.send_json(body) {
        Ok(resp) => Ok(resp.status().as_u16()),
        Err(e) => Err(format!("{e}")),
    }
}

/// Percent-encode a URL path segment (mihomo group/provider names are emoji +
/// CJK + spaces — `ureq` doesn't encode these for us).
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(*b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn valid_url(u: &str) -> bool {
    (u.starts_with("http://") || u.starts_with("https://"))
        && u.len() <= MAX_URL_LEN
        && !u.contains('\'')
        && !u.contains('"')
        && !u.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn valid_ipv4(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 4
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.len() <= 3
                && p.chars().all(|c| c.is_ascii_digit())
                && p.parse::<u16>().map(|n| n <= 255).unwrap_or(false)
        })
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

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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

fn tail_file(path: &str, n: usize) -> std::io::Result<Vec<String>> {
    let f = fs::File::open(path)?;
    let reader = std::io::BufReader::new(f);
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
