//! CHILL pieces of the agent's replies (moved from zte-agent `public.rs`, `manual_first.rs`,
//! `home_exit.rs` and `chill.rs`, Phase 4 step 8b): `services.chill` in `/api/public/status`,
//! the manual-first notice, the home exit's line and GET `/api/services/chill/home`, and the pure
//! readers they need. The caller gathers the state (the agent from its threads, u60d from its
//! shadow and the files) and these render it, so both reply alike.
//!
//! Not public (sync-public.sh deletes this file, as the agent's CHILL files).

use serde_json::{Value, json};
use std::net::Ipv4Addr;

pub const CHILL_ENV: &str = "/data/chill/chill.env";
pub const CHILL_STATE: &str = "/tmp/chill.state";
pub const CHILL_DISABLED: &str = "/data/chill/disabled";
pub const CHILL_MODE_FILE: &str = "/data/chill/mode";
pub const HOME_EXIT_STATE: &str = "/data/chill/home-exit.json";
pub const HOME_IP_LIST: &str = "/data/chill/home_ip.list";
pub const MANUAL_FIRST_STATE: &str = "/data/chill/manual-first.json";

/// A notice stays on the screens this long.
pub const NOTICE_KEEP: u64 = 600;
/// "Off in 10 min · Extend 2h" shows this long before the end (DD8).
pub const EXTEND_WINDOW: u64 = 600;

/// `NAME=value` from chill.env's text (the agent's `read_env_var`): quotes trimmed, "" when absent.
/// chill.env with these `KEY='…'` lines replaced (moved to the end), for one atomic rewrite
/// (the home exit flips three switches together; a half-written set would render something nobody
/// chose). chill.sh sources the file as shell, so a `'` or a newline in a value is refused.
pub fn env_with(content: &str, vars: &[(&str, &str)]) -> Result<String, String> {
    for (name, value) in vars {
        if value.contains('\'') || value.contains('\n') {
            return Err(format!("{name} value cannot contain a single quote or newline"));
        }
    }
    let prefixes: Vec<String> = vars.iter().map(|(n, _)| format!("{n}=")).collect();
    let mut out = String::with_capacity(content.len() + 64);
    for line in content.lines() {
        if prefixes.iter().any(|p| line.starts_with(p.as_str())) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    for (name, value) in vars {
        out.push_str(&format!("{name}='{value}'\n"));
    }
    Ok(out)
}

pub fn env_var(content: &str, name: &str) -> String {
    let prefix = format!("{name}=");
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix(&prefix) {
            return rest.trim().trim_matches('\'').trim_matches('"').to_string();
        }
    }
    String::new()
}

/// `services.chill`. The manual-first notice is one line for the home screen,
/// e.g. "手选的 JP 03 不通，已临时换到 🇯🇵 日本（自动）", with its English, when it was
/// said, and its kind (backup / back / alldown / repick / unstable) for the
/// symbol and colour. Fields are only ever added here (old screens read
/// `state`, `reason`, `on`, `notice`).
pub fn public(state: &str, reason: Option<&str>, on: bool, mf: &Value, home: Value) -> Value {
    json!({
        "state": state, "reason": reason, "on": on,
        "notice": mf["notice"],
        "notice_en": mf["notice_en"],
        "notice_at": mf["notice_at"],
        "notice_kind": mf["notice_kind"],
        // Home exit (E3 DD2): null, or {media, ip, rest, rest_until, fallback}
        "home_exit": home,
    })
}

/// `/tmp/chill.state` (chill.sh's `write_state`): (state, reason); "unknown" when unreadable.
pub fn state_and_reason(state: Option<&Value>) -> (String, Option<String>) {
    let s = state
        .and_then(|v| v.get("state"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let r = state
        .and_then(|v| v.get("reason"))
        .and_then(Value::as_str)
        .map(str::to_string);
    (s, r)
}

// ── manual first ─────────────────────────────────────────────────────────────

/// The parts of manual-first's `Persist` its summary reads.
#[derive(Clone, Copy, Debug, Default)]
pub struct ManualFirst<'a> {
    pub backup: &'a str,
    pub manual: &'a str,
    pub notice: &'a str,
    pub notice_en: &'a str,
    pub notice_at: u64,
    pub notice_kind: &'a str,
}

/// For `/api/services/chill` and `/api/public/status`.
pub fn manual_first_summary(p: &ManualFirst, now: u64) -> Value {
    let on_backup = !p.backup.is_empty();
    let fresh = on_backup || now.saturating_sub(p.notice_at) < NOTICE_KEEP;
    json!({
        "on_backup": on_backup,
        "manual": (!p.manual.is_empty()).then_some(p.manual),
        "backup": on_backup.then_some(p.backup),
        "notice": (fresh && !p.notice.is_empty()).then_some(p.notice),
        // null when there is none, or the notice predates L2 (no English)
        "notice_en": (fresh && !p.notice.is_empty() && !p.notice_en.is_empty()).then_some(p.notice_en),
        "notice_at": (fresh && !p.notice.is_empty()).then_some(p.notice_at),
        "notice_kind": (fresh && !p.notice.is_empty() && !p.notice_kind.is_empty()).then_some(p.notice_kind),
    })
}

// ── home exit ────────────────────────────────────────────────────────────────

/// CHILL as far as the switches care (DD4).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ChillMode {
    Off,
    Hot,
    Global,
    Direct,
    Rule,
}

impl ChillMode {
    /// Why a switch can't be turned on now (DD4's reason codes); `None` = OK.
    pub fn blocked(self) -> Option<&'static str> {
        match self {
            ChillMode::Off => Some("chill_off"),
            ChillMode::Hot => Some("hot"),
            ChillMode::Global => Some("mode_global"),
            ChillMode::Direct => Some("mode_direct"),
            ChillMode::Rule => None,
        }
    }
}

/// `CHILL_HOME_EXIT` as chill.sh accepts it (Q2): IPv4:port, port 1–65535.
pub fn parse_home_exit(s: &str) -> Option<(Ipv4Addr, u16)> {
    let (ip, port) = s.trim().rsplit_once(':')?;
    if ip
        .split('.')
        .any(|o| o.is_empty() || o.len() > 3 || !o.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    if port.is_empty() || port.len() > 5 || !port.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let port: u16 = port.parse().ok().filter(|p| *p > 0)?;
    Some((ip.parse().ok()?, port))
}

/// Sites in `home_ip.list`: non-empty lines that are not comments.
pub fn count_sites(list: &str) -> usize {
    list.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .count()
}

/// How Tailscale reaches the peer that routes `ip` (DD7): "direct", "relay"
/// or "offline"; `None` when no peer routes it (or status unreadable).
pub fn ts_path(status: &Value, ip: Ipv4Addr) -> Option<&'static str> {
    let peers = status.get("Peer")?.as_object()?;
    let router = peers.values().find(|p| {
        p.get("PrimaryRoutes")
            .and_then(Value::as_array)
            .is_some_and(|r| r.iter().filter_map(Value::as_str).any(|c| cidr_has(c, ip)))
    })?;
    if !router
        .get("Online")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Some("offline");
    }
    let direct = router
        .get("CurAddr")
        .and_then(Value::as_str)
        .is_some_and(|a| !a.is_empty());
    Some(if direct { "direct" } else { "relay" })
}

fn cidr_has(cidr: &str, ip: Ipv4Addr) -> bool {
    let Some((net, bits)) = cidr.split_once('/') else {
        return false;
    };
    let (Ok(net), Ok(bits)) = (net.parse::<Ipv4Addr>(), bits.parse::<u32>()) else {
        return false;
    };
    if bits > 32 {
        return false;
    }
    let mask = if bits == 0 {
        0
    } else {
        u32::MAX << (32 - bits)
    };
    u32::from(net) & mask == u32::from(ip) & mask
}

/// CHILL's state file and `/data/chill/mode` → the mode the switches see.
pub fn chill_mode(state: Option<&Value>, mode: &str) -> ChillMode {
    let Some(st) = state else {
        return ChillMode::Off;
    };
    match st.get("state").and_then(Value::as_str) {
        Some("running") => match mode.trim() {
            "global" => ChillMode::Global,
            "direct" => ChillMode::Direct,
            _ => ChillMode::Rule,
        },
        _ if st.get("reason").and_then(Value::as_str) == Some("overheat") => ChillMode::Hot,
        _ => ChillMode::Off,
    }
}

/// What the home exit's replies read from its machine.
#[derive(Clone, Debug, Default)]
pub struct Home {
    /// started (the screens show "updating" until then)
    pub ready: bool,
    pub media: bool,
    pub ip: bool,
    pub rest: bool,
    pub rest_until: u64,
    pub home_ok: Option<bool>,
    pub home_ms: Option<u32>,
    pub probed_at: u64,
    /// null, or {item, on, phase, until} while a request runs
    pub busy: Value,
    /// the saved notice as stored ({kind, item, text, text_en, at, code, detail})
    pub notice: Value,
}

/// The home exit's line in `services.chill` (DD2): what is on, rest's end, and whether home is on
/// its fallback; null when nothing is on.
pub fn home_summary(m: &Home) -> Value {
    if !m.ready || !(m.media || m.ip || m.rest) && m.busy.is_null() {
        return Value::Null;
    }
    json!({
        "media": m.media,
        "ip": m.ip,
        "rest": m.rest,
        "rest_until": (m.rest).then_some(m.rest_until),
        "fallback": m.home_ok == Some(false),
    })
}

/// Whether GET should say where traffic goes while home is down (DD6 "暂走 X"): only then is
/// mihomo asked.
pub fn wants_fallback(m: &Home, mode: ChillMode) -> bool {
    m.home_ok == Some(false) && (m.ip || m.rest) && mode == ChillMode::Rule
}

/// The rest of what GET reads, from outside the machine.
#[derive(Clone, Debug, Default)]
pub struct Around {
    pub serving_mcc: Option<i64>,
    pub true_roaming: Option<bool>,
    pub blocked: Option<&'static str>,
    pub sites: usize,
    pub fallback_to: Option<String>,
    pub tailscale: Option<&'static str>,
    pub now: u64,
}

/// `data` of GET /api/services/chill/home when configured.
pub fn home_get(m: &Home, a: &Around) -> Value {
    if !m.ready {
        // Not started yet: the screens show "updating".
        return json!({"configured": true, "updating": true});
    }
    let now = a.now;
    let kind = m.notice.get("kind").and_then(Value::as_str).unwrap_or("");
    let at = m.notice.get("at").and_then(Value::as_u64).unwrap_or(0);
    let notice = (now.saturating_sub(at) < NOTICE_KEEP && !kind.is_empty()).then_some(&m.notice);
    json!({
        "configured": true,
        "where": {
            "serving_mcc": a.serving_mcc,
            "true_roaming": a.true_roaming,
            "in_china": a.serving_mcc.map(|m| m == 460),
        },
        "blocked": a.blocked,
        "items": {
            "media": {"on": m.media},
            "ip": {"on": m.ip, "sites": a.sites},
            "rest": {
                "on": m.rest,
                "until": m.rest.then_some(m.rest_until),
                "can_extend": m.rest && now < m.rest_until && m.rest_until - now <= EXTEND_WINDOW,
            },
        },
        "home": {"ok": m.home_ok, "ms": m.home_ms, "checked_at": (m.probed_at > 0).then_some(m.probed_at)},
        "fallback_to": a.fallback_to,
        "tailscale": a.tailscale,
        "busy": m.busy,
        "notice": notice,
    })
}

/// What one call to mihomo's API gave.
#[derive(Clone, Debug, PartialEq)]
pub enum Got {
    Json(u16, Value),
    Status(u16),
    Unreachable,
}

/// The URL every exit probe asks for (manual-first, deep diagnosis' proxy layer).
pub const PROBE_URL: &str = "https://www.gstatic.com/generate_204";

/// Percent-encoding for one path segment or query value of mihomo's API.
pub fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(*b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// node name → provider, from `/providers/proxies`. Only file/http
/// providers: mihomo also lists every proxy group as a "Compatible" provider.
pub fn node_providers(all: &Value) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let Some(provs) = all.get("providers").and_then(Value::as_object) else { return map };
    for (name, p) in provs {
        if p.get("vehicleType").and_then(Value::as_str) == Some("Compatible") {
            continue;
        }
        for n in p.get("proxies").and_then(Value::as_array).into_iter().flatten() {
            if let Some(n) = n.get("name").and_then(Value::as_str) {
                map.entry(n.to_string()).or_insert_with(|| name.clone());
            }
        }
    }
    map
}

/// The node the main group ends at (following groups inside it), and one health check of it
/// against [`PROBE_URL`] (deep diagnosis' proxy layer, D11). Two tries of 2 s, since an idle node
/// often needs a moment. `call(path, slow)`: one GET to mihomo's API (`slow`: allow ~15 s).
/// `None` = mihomo didn't answer; the node is the main group itself when nothing is chosen.
pub fn probe_exit(call: &dyn Fn(&str, bool) -> Got) -> (String, Option<crate::diagnose::NodeProbe>) {
    use crate::diagnose::NodeProbe;
    let get = |path: &str| match call(path, false) {
        Got::Json(c, v) if c < 400 => Some(v),
        _ => None,
    };
    let mut node = MAIN_GROUP.to_string();
    for _ in 0..6 {
        let next = get(&format!("/proxies/{}", enc(&node)))
            .and_then(|v| v.get("now").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default();
        if next.is_empty() {
            break;
        }
        node = next;
    }
    if node == MAIN_GROUP {
        return (node, None);
    }
    let provider = get("/providers/proxies").and_then(|all| node_providers(&all).get(&node).cloned());
    let path = match provider {
        Some(prov) => format!("/providers/proxies/{}/{}/healthcheck?url={}&timeout=2000", enc(&prov), enc(&node), enc(PROBE_URL)),
        None => format!("/proxies/{}/delay?url={}&timeout=2000", enc(&node), enc(PROBE_URL)),
    };
    let mut answered = false;
    for _ in 0..2 {
        match call(&path, true) {
            Got::Json(200, v) => {
                answered = true;
                if let Some(d) = v.get("delay").and_then(Value::as_u64).filter(|d| *d > 0) {
                    return (node, Some(NodeProbe::Ms(d as u32)));
                }
            }
            Got::Json(408 | 503 | 504, _) | Got::Status(408 | 503 | 504) => answered = true,
            _ => {}
        }
    }
    (node, answered.then_some(NodeProbe::Timeout))
}

/// mihomo's main group and the manual-first group.
pub const MAIN_GROUP: &str = "🚀 节点选择";
pub const MANUAL_GROUP: &str = "🎯 手动节点";
/// The main group's member before it went DIRECT ("proxy" goes back to it).
pub const LAST_MAIN: &str = "/data/chill/last-main";
/// The scenario engine's restore entry for an exit changed abroad.
pub const CHILL_EXIT_KEY: &str = "chill-exit-after-abroad";

/// What PUT `/api/services/chill/exit` does to mihomo.
#[derive(Clone, Debug, PartialEq)]
pub struct ExitPlan {
    /// The main group's new member, when it changes.
    pub member: Option<String>,
    /// The member to remember before going DIRECT (already through [`main_to_remember`]).
    pub remember: Option<String>,
    /// mihomo's mode afterwards (`rule`, `direct`, `global`).
    pub mode: &'static str,
}

/// A body's `state` the exit can take; else the 400 reply.
pub fn exit_wanted(body: &Value) -> Result<&str, (u16, Value)> {
    let want = body["state"].as_str().unwrap_or("");
    if matches!(want, "proxy" | "direct_keep_ai" | "direct_all" | "global") {
        Ok(want)
    } else {
        Err((
            400,
            json!({"ok": false, "error": "state must be proxy, direct_keep_ai, direct_all or global"}),
        ))
    }
}

/// While manual-first holds a backup region, the owner's choice is still the manual group.
pub fn main_to_remember<'a>(now: &'a str, backup: &str) -> &'a str {
    if !backup.is_empty() && backup == now {
        MANUAL_GROUP
    } else {
        now
    }
}

/// Where "proxy" puts the main group back: the remembered member if the group
/// still offers it, else the first member that is not DIRECT.
pub fn proxy_member(remembered: Option<&str>, options: &[&str]) -> Option<String> {
    remembered
        .filter(|m| *m != "DIRECT" && options.contains(m))
        .or_else(|| options.iter().copied().find(|m| *m != "DIRECT"))
        .map(str::to_string)
}

/// The exit change from the main group as mihomo has it (`now`, `options`), the remembered member
/// and manual-first's backup region; else the 409 reply.
pub fn exit_plan(
    want: &str,
    now: &str,
    options: &[&str],
    remembered: Option<&str>,
    backup: &str,
) -> Result<ExitPlan, (u16, Value)> {
    let mut remember = None;
    let member = match want {
        "direct_keep_ai" if now != "DIRECT" => {
            if !options.contains(&"DIRECT") {
                return Err((
                    409,
                    json!({"ok": false, "error": "the main group has no DIRECT option"}),
                ));
            }
            let r = main_to_remember(now, backup);
            if !r.is_empty() && r != "DIRECT" {
                remember = Some(r.to_string());
            }
            Some("DIRECT".to_string())
        }
        "proxy" if now == "DIRECT" || now.is_empty() => match proxy_member(remembered, options) {
            Some(m) => Some(m),
            None => {
                return Err((
                    409,
                    json!({"ok": false, "error": "the main group has no node to go back to"}),
                ));
            }
        },
        _ => None,
    };
    let mode = match want {
        "direct_all" => "direct",
        "global" => "global",
        _ => "rule",
    };
    Ok(ExitPlan {
        member,
        remember,
        mode,
    })
}

/// The one subscription whose URL the owner edits (`SUB_URL` in chill.env).
pub const EDITABLE_PROVIDER: &str = "sub";
const MAX_URL_LEN: usize = 4096;

/// http(s) only, no quotes, spaces or control characters (chill.env quotes the value with `'`).
pub fn valid_url(u: &str) -> bool {
    (u.starts_with("http://") || u.starts_with("https://"))
        && u.len() <= MAX_URL_LEN
        && !u.contains('\'')
        && !u.contains('"')
        && !u.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// PUT `/api/services/chill/providers` `{"name": "sub", "url": …}`: the new URL, or the 400.
pub fn provider_url_wanted(body: &[u8]) -> Result<String, (u16, Value)> {
    let parsed: Value =
        serde_json::from_slice(body).map_err(|_| (400, json!({"ok": false, "error": "invalid JSON"})))?;
    let name = parsed["name"].as_str().unwrap_or("").trim();
    if name != EDITABLE_PROVIDER {
        return Err((400, json!({"ok": false, "error": format!("provider '{name}' has no editable URL")})));
    }
    let url = parsed["url"].as_str().unwrap_or("").trim().to_string();
    if !valid_url(&url) {
        return Err((400, json!({"ok": false, "error": "invalid URL (http(s) only, no quotes/spaces)"})));
    }
    Ok(url)
}

/// chill.sh, the CHILL supervisor (start, stop, profile).
pub const CHILL_SH: &str = "/data/chill/chill.sh";
/// Below this much free memory or above this temperature, starting CHILL risks a hang.
pub const MIN_MEM_AVAIL_MB: i64 = 100;
pub const MAX_TEMP_C: i64 = 78;

/// Before switching CHILL on: refuse when the device is low on memory or too hot (from
/// chill.state; no state file yet = nothing to check against).
pub fn precheck(state: Option<&Value>) -> Result<(), String> {
    let Some(st) = state else { return Ok(()) };
    if let Some(mb) = st.get("mem_avail_mb").and_then(Value::as_i64)
        && mb < MIN_MEM_AVAIL_MB
    {
        return Err(format!(
            "device low on memory ({mb} MB available) — this risks a hang; retry once it settles"
        ));
    }
    if let Some(c) = st.get("cpuss_c").and_then(Value::as_i64)
        && c >= MAX_TEMP_C
    {
        return Err(format!(
            "device too hot ({c}°C) — this risks a hang; let it cool, then retry"
        ));
    }
    Ok(())
}

/// The last `n` non-empty lines of a command's output, joined with " | ".
pub fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join(" | ")
}

/// The profile a PUT /api/services/chill/profile body asks for; else the 400 reply.
pub fn profile_wanted(body: &Value) -> Result<&str, (u16, Value)> {
    match body["profile"].as_str().unwrap_or("") {
        p @ ("eco" | "standard" | "perf") => Ok(p),
        _ => Err((
            400,
            json!({"ok": false, "error": "profile must be eco, standard or perf"}),
        )),
    }
}

/// The scenario engine's restore entry for CHILL switched off abroad: switch it back on when home
/// (the agent's `chill_on_restore`, as JSON).
pub fn chill_on_restore(saved_at: i64) -> Value {
    json!({
        "key": CHILL_ON_KEY,
        "action": {
            "method": "POST",
            "path": "/api/services/chill/enable",
            "verify_job": "/api/services/chill/job",
            "best_effort": true,
        },
        "saved_at": saved_at,
        "while_abroad": true,
    })
}

/// The restore entry for an exit changed abroad: back to proxy when home (the agent's
/// `chill_exit_restore`, as JSON).
pub fn chill_exit_restore(saved_at: i64) -> Value {
    json!({
        "key": CHILL_EXIT_KEY,
        "action": {
            "method": "PUT",
            "path": "/api/services/chill/exit",
            "body": {"state": "proxy"},
            "verify_field": {"path": "/api/services/chill", "pointer": "/data/exit", "equals": "proxy"},
            "best_effort": true,
        },
        "saved_at": saved_at,
        "while_abroad": true,
    })
}

#[cfg(test)]
mod tests;

// ── scenario ─────────────────────────────────────────────────────────────────

/// Key of the "switch CHILL back on when home" entry in the scenario engine's `restore.json`.
pub const CHILL_ON_KEY: &str = "chill-on-after-abroad";

/// The CHILL fields of the scenario summary: `chill_on_when_home` (a pending "CHILL back on" restore)
/// and `auto_direct` (abroad, the main group goes DIRECT by itself: an abroad scenario with an action
/// that sets the regions' member to DIRECT). `actions`: each action's (path, body.member).
pub fn scenario_fields<'a>(
    summary: &mut Value,
    chill_on_when_home: bool,
    abroad: bool,
    mut actions: impl Iterator<Item = (&'a str, Option<&'a str>)>,
) {
    summary["chill_on_when_home"] = json!(chill_on_when_home);
    summary["auto_direct"] = json!(
        abroad
            && actions
                .any(|(path, member)| path == "/api/services/chill/regions"
                    && member == Some("DIRECT"))
    );
}

// ── the status page, regions and bypass IPs (moved from zte-agent `chill.rs`, step 9h) ──────────
// The scenario engine reads the status page (`/data/region/active`, `/data/exit`) and drives the
// regions and the bypass list; u60d serves them once it runs the engine. The checks and the
// rendering are here so the agent (ureq) and u60d (reqwest) do the same around their own calls.

/// The five `type: select` groups defined in template.yaml — the only groups a
/// region/AI-exit switch is allowed to touch. Whitelisted so a client can't PUT
/// an arbitrary mihomo group name.
pub const REGION_GROUPS: [&str; 5] = ["🚀 节点选择", "🤖 AI", "📺 流媒体", "🍎 Apple", "🐟 漏网之鱼"];
pub const AI_GROUP: &str = "🤖 AI";
/// Must match chill.sh's own $BYPASS_PRIO — the `ip rule` priority its
/// apply_bypass()/flush() manage. Changing this here without changing chill.sh
/// would leave stale rules neither side cleans up.
pub const BYPASS_PRIO: &str = "8999";
pub const MAX_BYPASS_IPS: usize = 32;

/// What the traffic does, in the owner's terms rather than mihomo's:
///   proxy           rule mode, main group on a node (normal, at home)
///   direct_keep_ai  rule mode, main group DIRECT: everything that follows the
///                   main group goes direct; 🤖 AI and 📞 VoWiFi are separate
///                   groups and keep their nodes (abroad on a local SIM)
///   direct_all      direct mode: everything direct, AI included
///   global          global mode (only set from mihomo's own dashboard)
pub fn exit_state(mode: &str, main_now: &str) -> &'static str {
    match mode {
        "direct" => "direct_all",
        "global" => "global",
        _ if main_now == "DIRECT" => "direct_keep_ai",
        _ => "proxy",
    }
}

/// The region groups from mihomo's `/proxies`: name, current member, size.
pub fn summarize_groups(proxies: Option<&Value>) -> Vec<Value> {
    let Some(proxies) = proxies.and_then(|v| v.get("proxies")).and_then(Value::as_object) else {
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

/// One group from mihomo's `/proxies`: its current member and options.
pub fn group_now(proxies: Option<&Value>, group: &str) -> Option<Value> {
    let g = proxies?.get("proxies")?.as_object()?.get(group)?;
    Some(json!({
        "active": g.get("now").and_then(Value::as_str),
        "options": g.get("all").cloned().unwrap_or(Value::Array(vec![])),
    }))
}

/// GET /api/services/chill's data: the state file, and while running what mihomo says
/// (`version`, `proxies`, `mode` from its `/configs`), then the two summaries.
pub fn status_data(
    state: Option<Value>,
    version: Option<&Value>,
    proxies: Option<&Value>,
    mode: Option<String>,
    manual_first: Value,
    home_exit: Value,
) -> Value {
    let Some(st) = state else {
        // chill.sh hasn't written a state file yet (never started since boot).
        return json!({"state": "unknown"});
    };
    let running = st.get("state").and_then(Value::as_str) == Some("running");
    let mut data = st;
    if running {
        data["version"] = json!(version.and_then(|v| v.get("version")).and_then(Value::as_str));
        data["groups"] = json!(summarize_groups(proxies));
        data["region"] = json!(group_now(proxies, MAIN_GROUP));
        data["ai_exit"] = json!(group_now(proxies, AI_GROUP));
        let main_now = group_now(proxies, MAIN_GROUP)
            .and_then(|g| g.get("active").and_then(Value::as_str).map(str::to_string));
        if let (Some(m), Some(n)) = (&mode, &main_now) {
            data["exit"] = json!(exit_state(m, n));
        }
        data["mode"] = json!(mode);
    }
    data["manual_first"] = manual_first;
    data["home_exit"] = home_exit;
    data
}

/// PUT /api/services/chill/regions's body: `(group, member)`, or the reply refusing it.
pub fn region_wanted(body: &[u8]) -> Result<(String, String), (u16, Value)> {
    let parsed: Value =
        serde_json::from_slice(body).map_err(|_| (400, json!({"ok": false, "error": "invalid JSON"})))?;
    let group = parsed["group"].as_str().unwrap_or("").trim().to_string();
    let member = parsed["member"].as_str().unwrap_or("").trim().to_string();
    if !REGION_GROUPS.contains(&group.as_str()) {
        return Err((400, json!({"ok": false, "error": "unknown group"})));
    }
    if member.is_empty() {
        return Err((400, json!({"ok": false, "error": "missing 'member'"})));
    }
    Ok((group, member))
}

/// `member` is one of the group's options (mihomo's `/proxies/<group>` reply).
pub fn in_group(info: &Value, member: &str) -> bool {
    info.get("all")
        .and_then(Value::as_array)
        .is_some_and(|a| a.iter().any(|n| n.as_str() == Some(member)))
}

pub fn valid_ipv4(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 4
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.len() <= 3
                && p.chars().all(|c| c.is_ascii_digit())
                && p.parse::<u16>().map(|n| n <= 255).unwrap_or(false)
        })
}

/// PUT /api/services/chill/bypass's body (`{ips: [...]}`, the full desired list): sorted and
/// deduplicated, or the reply refusing it.
pub fn bypass_wanted(body: &[u8]) -> Result<Vec<String>, (u16, Value)> {
    let parsed: Value =
        serde_json::from_slice(body).map_err(|_| (400, json!({"ok": false, "error": "invalid JSON"})))?;
    let Some(arr) = parsed["ips"].as_array() else {
        return Err((400, json!({"ok": false, "error": "missing 'ips' array"})));
    };
    let mut ips: Vec<String> = arr
        .iter()
        .filter_map(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if ips.len() > MAX_BYPASS_IPS {
        return Err((400, json!({"ok": false, "error": "too many IPs"})));
    }
    if let Some(bad) = ips.iter().find(|ip| !valid_ipv4(ip)) {
        return Err((400, json!({"ok": false, "error": format!("invalid IP: {bad}")})));
    }
    ips.sort();
    ips.dedup();
    Ok(ips)
}

/// GET /api/services/chill/bypass's data from chill.env and the state file.
pub fn bypass_data(env: &str, state: Option<&Value>) -> Value {
    let ips: Vec<String> = env_var(env, "CHILL_BYPASS_IP").split_whitespace().map(str::to_string).collect();
    let stale = state.and_then(|v| v.get("bypass_stale").cloned()).unwrap_or(Value::Array(vec![]));
    json!({"ips": ips, "stale": stale})
}

#[cfg(test)]
mod status_tests {
    use super::*;

    #[test]
    fn exit_state_reads_mode_first_then_the_main_group() {
        assert_eq!(exit_state("rule", "🇹🇼 台湾"), "proxy");
        assert_eq!(exit_state("rule", "DIRECT"), "direct_keep_ai");
        assert_eq!(exit_state("direct", "🇹🇼 台湾"), "direct_all");
        assert_eq!(exit_state("direct", "DIRECT"), "direct_all");
        assert_eq!(exit_state("global", "DIRECT"), "global");
    }

    #[test]
    fn status_shows_the_region_and_exit_while_running() {
        let proxies = json!({"proxies": {"🚀 节点选择": {"now": "DIRECT", "all": ["JP", "DIRECT"]}}});
        let d = status_data(
            Some(json!({"state": "running"})),
            Some(&json!({"version": "1.19"})),
            Some(&proxies),
            Some("rule".into()),
            json!(null),
            json!(null),
        );
        assert_eq!(d["region"]["active"], "DIRECT");
        assert_eq!(d["exit"], "direct_keep_ai");
        assert_eq!(d["version"], "1.19");
        assert_eq!(d["groups"][0]["size"], 2);
        let stopped = status_data(Some(json!({"state": "stopped"})), None, None, None, json!(1), json!(2));
        assert!(stopped.get("region").is_none());
        assert_eq!(stopped["manual_first"], 1);
        assert_eq!(status_data(None, None, None, None, json!(1), json!(2)), json!({"state": "unknown"}));
    }

    #[test]
    fn regions_and_bypass_are_checked() {
        assert_eq!(region_wanted(r#"{"group":"🚀 节点选择","member":" JP "}"#.as_bytes()).unwrap().1, "JP");
        assert_eq!(region_wanted(br#"{"group":"x","member":"JP"}"#).unwrap_err().0, 400);
        assert!(in_group(&json!({"all": ["JP", "DIRECT"]}), "DIRECT"));
        assert!(!in_group(&json!({"all": ["JP"]}), "DIRECT"));
        assert_eq!(bypass_wanted(br#"{"ips":["10.0.0.2"," 10.0.0.1","10.0.0.2"]}"#).unwrap(), vec!["10.0.0.1", "10.0.0.2"]);
        assert_eq!(bypass_wanted(br#"{"ips":["10.0.0.256"]}"#).unwrap_err().0, 400);
        assert_eq!(bypass_data("CHILL_BYPASS_IP=\"10.0.0.1 10.0.0.2\"\n", None)["ips"][1], "10.0.0.2");
    }
}
