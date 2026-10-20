//! `/agent-api/*` (u60-mcp.md phase 1): a small read-only HTTP interface for a program (the
//! host-side `luyaz-mcp`), not for a browser. Not MCP itself (D-h): the MCP server runs on the
//! computer and calls this.
//!
//! - Off unless `/data/luyaz/agent.token` has at least one `name token` line (R3): 404.
//! - Source (R1): loopback, the tailnet (`100.64.0.0/10`, `fd7a:115c:a1e0::/48`) and the CIDRs in
//!   `/data/luyaz/agent.allow`; anything else 403. The token is still needed from all of them.
//! - `Authorization: Bearer <token>`, compared in constant time; 401 otherwise. No cookie session.
//! - GET only; each route asks the core over loopback and rebuilds the reply from a field
//!   whitelist (R2): a field the core adds later does not reach the agent. Secrets (the scenario
//!   PIN, the ICCID, cell ids) are in no list.
//! - Text that came from outside (health details, evidence, alert text) is cut to 64 characters
//!   and wrapped as `{"untrusted": "…"}`.
//! - Core down: `{"core": "down", "sentinel_hb_age_s": …}`, 200, not a 502.
//! - Every answer carries `device_id` (`agent.id` beside the token file, 12 random hex digits,
//!   made once): the computer's `luyaz-mcp enroll` pins it (u60-mcp.md 多台 R2/R5).
//! - Nothing runs between requests; both files are read on each request, so deleting a token line
//!   revokes it at once.
//!
//! The leg from the computer to the home router is plain HTTP (R5). Second phase (D8): a token
//! line may carry a third column `act` (missing or `read` = read only); writes are in
//! `agent_act.rs` and take an `act` token from an address in `agent.act-allow`.

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::OnceLock;

use axum::Router;
use axum::extract::{ConnectInfo, Query, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::{Value, json};

use crate::agent::Fail;
use crate::pages::Shared;

pub const TOKEN_FILE: &str = "/data/luyaz/agent.token";
pub const ALLOW_FILE: &str = "/data/luyaz/agent.allow";
const SENTINEL_HB: &str = "/tmp/luyaz-sentinel.hb";
const UNTRUSTED_MAX: usize = 64;
/// The reply cap: events are dropped from the end of the page until it fits.
const REPLY_MAX: usize = 16 * 1024;
const EVENTS_DEFAULT: usize = 20;
const EVENTS_MAX: usize = 50;

static FILES: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();

/// The token and allow files (`--agent-token-file`, `--agent-allow-file`).
pub fn set_files(token: PathBuf, allow: PathBuf) {
    let _ = FILES.set((token, allow));
}

fn files() -> (PathBuf, PathBuf) {
    FILES
        .get()
        .cloned()
        .unwrap_or_else(|| (TOKEN_FILE.into(), ALLOW_FILE.into()))
}

pub fn ours(path: &str) -> bool {
    path == "/agent-api" || path.starts_with("/agent-api/")
}

pub fn routes() -> Router<Shared> {
    Router::new()
        .merge(crate::agent_act::routes())
        .route("/agent-api/health", get(health))
        .route("/agent-api/diagnose", get(diagnose))
        .route("/agent-api/power", get(power))
        .route("/agent-api/events", get(events))
}

// ---- the gate: token file, source, token ----

/// What a token may do (second phase X1): the third column, `read` when there is none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Perm {
    Read,
    Act,
}

/// `name token [read|act]` per line; `#` lines, blank lines and malformed lines are skipped (an
/// unknown third column too: never guess `act`).
#[cfg(test)]
pub fn parse_tokens(text: &str) -> Vec<(String, String)> {
    parse_tokens_perm(text)
        .into_iter()
        .map(|(n, t, _)| (n, t))
        .collect()
}

pub fn parse_tokens_perm(text: &str) -> Vec<(String, String, Perm)> {
    let mut out = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = line.split_whitespace().collect();
        let perm = match f.get(2).copied() {
            None | Some("read") => Some(Perm::Read),
            Some("act") => Some(Perm::Act),
            Some(_) => None,
        };
        match perm {
            Some(perm) if (2..=3).contains(&f.len()) && f[1].len() >= 16 => {
                out.push((f[0].to_string(), f[1].to_string(), perm));
            }
            _ => eprintln!("[agent-api] token file: skipped a malformed line"),
        }
    }
    out
}

/// One CIDR (`192.168.1.0/24`, `fd00::/8`, or a bare address).
fn parse_cidr(s: &str) -> Option<(IpAddr, u8)> {
    let (a, n) = match s.split_once('/') {
        Some((a, n)) => (a, Some(n)),
        None => (s, None),
    };
    let ip: IpAddr = a.parse().ok()?;
    let max = if ip.is_ipv4() { 32 } else { 128 };
    let n = match n {
        Some(n) => n.parse().ok().filter(|&n| n <= max)?,
        None => max,
    };
    Some((ip, n))
}

pub(crate) fn in_cidr(ip: IpAddr, (net, n): (IpAddr, u8)) -> bool {
    match (ip, net) {
        (IpAddr::V4(a), IpAddr::V4(b)) => {
            let m = if n == 0 { 0 } else { u32::MAX << (32 - n) };
            u32::from(a) & m == u32::from(b) & m
        }
        (IpAddr::V6(a), IpAddr::V6(b)) => {
            let m = if n == 0 { 0 } else { u128::MAX << (128 - n) };
            u128::from(a) & m == u128::from(b) & m
        }
        _ => false,
    }
}

/// The allow file: one CIDR per line, `#` comments; a bad line is skipped.
pub fn parse_allow(text: &str) -> Vec<(IpAddr, u8)> {
    text.lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter(|l| !l.is_empty())
        .filter_map(|l| {
            let c = parse_cidr(l);
            if c.is_none() {
                eprintln!("[agent-api] allow file: skipped {l:?}");
            }
            c
        })
        .collect()
}

pub fn source_ok(ip: IpAddr, extra: &[(IpAddr, u8)]) -> bool {
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    };
    const TAILNET: [&str; 2] = ["100.64.0.0/10", "fd7a:115c:a1e0::/48"];
    ip.is_loopback()
        || TAILNET
            .iter()
            .filter_map(|c| parse_cidr(c))
            .chain(extra.iter().copied())
            .any(|c| in_cidr(ip, c))
}

fn ct_eq(a: &str, b: &str) -> bool {
    use sha2::{Digest, Sha256};
    let (x, y) = (Sha256::digest(a.as_bytes()), Sha256::digest(b.as_bytes()));
    x.iter()
        .zip(y.iter())
        .fold(0u8, |acc, (p, q)| acc | (p ^ q))
        == 0
}

/// The client name the bearer token belongs to.
pub fn who<'a>(tokens: &'a [(String, String)], bearer: &str) -> Option<&'a str> {
    let mut found = None;
    for (name, t) in tokens {
        if ct_eq(t, bearer) && found.is_none() {
            found = Some(name.as_str());
        }
    }
    found
}

/// The peer address of the request.
pub(crate) fn source_ip(req: &Request) -> Option<IpAddr> {
    req.extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0.ip())
}

/// The checks in order; Ok = the client's name.
fn admit(req: &Request) -> Result<String, Response> {
    admit_perm(req).map(|(n, _)| n)
}

/// [`admit`], and what the token may do.
pub(crate) fn admit_perm(req: &Request) -> Result<(String, Perm), Response> {
    let (token_file, allow_file) = files();
    let lines = parse_tokens_perm(&std::fs::read_to_string(token_file).unwrap_or_default());
    let tokens: Vec<(String, String)> = lines.iter().map(|(n, t, _)| (n.clone(), t.clone())).collect();
    if tokens.is_empty() {
        return Err(StatusCode::NOT_FOUND.into_response());
    }
    let peer = source_ip(req);
    let allow = parse_allow(&std::fs::read_to_string(allow_file).unwrap_or_default());
    if !peer.is_some_and(|ip| source_ok(ip, &allow)) {
        eprintln!("[agent-api] refused source {peer:?}");
        return Err(StatusCode::FORBIDDEN.into_response());
    }
    let bearer = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    match who(&tokens, bearer.trim()) {
        Some(name) => {
            // the first line with this token (as `who`)
            let perm = lines
                .iter()
                .find(|(n, t, _)| n == name && ct_eq(t, bearer.trim()))
                .map_or(Perm::Read, |(_, _, p)| *p);
            Ok((name.to_string(), perm))
        }
        None => {
            eprintln!("[agent-api] bad token from {peer:?}");
            Err((StatusCode::UNAUTHORIZED, [("WWW-Authenticate", "Bearer")]).into_response())
        }
    }
}

// ---- shaping ----

pub fn untrusted(v: &Value) -> Value {
    match v.as_str() {
        Some(s) => json!({"untrusted": s.chars().take(UNTRUSTED_MAX).collect::<String>()}),
        None => Value::Null,
    }
}

/// Only the listed keys of an object (missing ones stay out).
fn pick(v: &Value, keys: &[&str]) -> Value {
    let mut m = serde_json::Map::new();
    for k in keys {
        if let Some(x) = v.get(*k) {
            m.insert((*k).to_string(), x.clone());
        }
    }
    Value::Object(m)
}

/// A device clock reading (local digits labelled UTC) as RFC 3339 with its real offset.
pub fn iso(wall: i64, offset: i64) -> String {
    let (y, mo, d) = crate::scheduler::civil(wall.div_euclid(86_400));
    let s = wall.rem_euclid(86_400);
    let sign = if offset < 0 { '-' } else { '+' };
    let o = offset.abs();
    format!(
        "{y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}{sign}{:02}:{:02}",
        s / 3600,
        s % 3600 / 60,
        s % 60,
        o / 3600,
        o % 3600 / 60
    )
}

fn device_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Whitelisted `/agent-api/health` from the core's public status and health check.
pub fn shape_health(status: &Value, health: &Value) -> Value {
    let checks: Vec<Value> = health["checks"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|c| {
                    json!({
                        "id": c["id"], "level": c["level"],
                        "label": c["label_en"].as_str().or(c["label"].as_str()),
                        "detail": untrusted(if c["detail_en"].is_string() { &c["detail_en"] } else { &c["detail"] }),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let f = &status["network"]["finding"];
    json!({
        "core": "up",
        "verdict": status["network"]["verdict"],
        "finding": if f.is_object() { pick(f, &["kind", "layer", "symptom_en"]) } else { Value::Null },
        "alerts_unread": status["alerts"]["unread"],
        "health": {
            "checked": status["health"]["checked"], "bad": health["bad"], "warn": health["warn"],
            "checks": checks,
        },
        "services": {
            "tailscale": pick(&status["services"]["tailscale"], &["installed", "running"]),
            "chill": pick(&status["services"]["chill"], &["state", "on"]),
        },
        "utc_offset": status["clock"]["utc_offset"],
    })
}

/// Whitelisted `/agent-api/diagnose` (R8): the stored finding with its age, or `no_open_finding`.
pub fn shape_diagnose(d: &Value, now: i64, offset: i64) -> Value {
    let run = if d["state"].is_string() && d["state"] != "idle" {
        json!({
            "state": d["state"],
            "finished_at": d["finished_at"].as_i64().map(|t| iso(t, offset)),
            "main": if d["main"].is_object() {
                json!({"layer": d["main"]["layer"], "level": d["main"]["level"], "text": d["main"]["text_en"]})
            } else { Value::Null },
        })
    } else {
        Value::Null
    };
    let f = &d["finding"];
    if !f.is_object() {
        return json!({
            "state": "no_open_finding",
            "note": "No known problem stored. This is not a fresh test and not proof that the connection works.",
            "last_run": run,
        });
    }
    let at = f["at"].as_i64().unwrap_or(0);
    let evidence: Vec<Value> = f["evidence"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|e| {
                    json!({
                        "layer": e["layer"], "level": e["level"],
                        "detail": untrusted(if e["detail_en"].is_string() { &e["detail_en"] } else { &e["detail"] }),
                        "at": e["at"].as_i64().map(|t| iso(t, offset)),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    json!({
        "state": "open_finding",
        "note": "A stored judgement, not a fresh test.",
        "finding": {
            "kind": f["kind"], "layer": f["layer"], "confidence": f["confidence"],
            "symptom": f["symptom_en"], "evidence": evidence,
            "at": iso(at, offset), "age_s": (now - at).max(0),
            "first": f["first"].as_i64().map(|t| iso(t, offset)),
            "count": f["count"],
        },
        "last_run": run,
    })
}

/// Whitelisted `/agent-api/power`.
pub fn shape_power(battery: Option<&Value>, modem: Option<&Value>, psm: Option<&Value>) -> Value {
    let b = battery.map(|b| {
        json!({
            "capacity_pct": b["capacity"], "status": b["status"],
            "temperature_dc": b["temperature"], "health": b["health"],
            "charger_online": b["charger"]["online"],
            "estimate": pick(&b["estimate"], &["state", "kind", "minutes", "target_pct"]),
        })
    });
    json!({
        "battery": b,
        "modem_mode": modem.map(|m| m["operate_mode"].clone()),
        "wifi_power_save": psm.map(|p| p["enabled"].clone()),
    })
}

/// Whitelisted cursor page (R4/R6/R9): events with their recorded offset, the SMS snapshot.
pub fn shape_events(a: &Value, events: &[Value]) -> Value {
    let cur = a["utc_offset"].as_i64().unwrap_or(0);
    let ev: Vec<Value> = events
        .iter()
        .map(|e| {
            let off = e["offset"].as_i64();
            json!({
                "seq": e["seq"],
                "time": e["time"].as_i64().map(|t| iso(t, off.unwrap_or(cur))),
                "offset_known": off.is_some(),
                "kind": e["kind"], "label": e["label_en"],
                "text": untrusted(&e["text"]), "unread": e["unread"],
            })
        })
        .collect();
    let sms: Vec<Value> = a["sms"]
        .as_array()
        .map(|s| {
            s.iter()
                .map(|r| {
                    json!({
                        "seq": r["seq"], "kind": r["kind"], "result": r["result"],
                        "time": r["time"].as_i64().map(|t| iso(t, cur)),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    json!({
        "events": ev, "next": a["next"], "more": a["more"], "gap": a["gap"],
        "unread": a["unread"], "sms": sms,
    })
}

// ---- the routes ----

/// `data` of a 2xx `{ok, data}`; Ok(Err(status)) for any other answer. Err = the core did not
/// answer.
async fn read(app: &Shared, path: &str) -> Result<Result<Value, u16>, Fail> {
    let (code, v) = crate::agent::get_internal(app, path).await?;
    Ok(if (200..300).contains(&code) && v["ok"] != false {
        Ok(v.get("data").cloned().unwrap_or(v))
    } else {
        Err(code)
    })
}

/// The core answered with an error: say so, never shape it into "nothing wrong" (R8).
fn unavailable(what: &str, code: u16) -> Response {
    reply(json!({"core": "up", "state": "unavailable", "source": what, "http": code}))
}

fn core_down() -> Response {
    let age = std::fs::metadata(SENTINEL_HB)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map(|d| d.as_secs());
    reply(json!({"core": "down", "sentinel_hb_age_s": age}))
}

pub(crate) fn reply(v: Value) -> Response {
    (
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        axum::Json(with_device_id(v)),
    )
        .into_response()
}

// ---- which device this is (u60-mcp.md 多台 R2, M-T1) ----

/// Beside the token file: `/data/luyaz/agent.id`.
fn id_file() -> PathBuf {
    let (token, _) = files();
    token
        .parent()
        .map_or_else(|| PathBuf::from("agent.id"), |d| d.join("agent.id"))
}

fn valid_id(s: &str) -> bool {
    s.len() == 12 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// This device's agent id: 12 random hex digits, made on the first `/agent-api` answer and kept
/// (0600, written to a temporary name and renamed). Not the admin web's `web-device-id`: the
/// agent sees only this one. Deleting the file makes a new one (the computer then has to
/// re-enroll the device). None = no file and none could be made (logged).
pub fn device_id_at(path: &std::path::Path) -> Option<String> {
    // two first answers at once must not make two ids
    static MAKING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _g = MAKING.lock().unwrap_or_else(|e| e.into_inner());
    if let Ok(s) = std::fs::read_to_string(path) {
        let s = s.trim();
        if valid_id(s) {
            return Some(s.to_ascii_lowercase());
        }
    }
    let made = (|| -> std::io::Result<String> {
        use std::io::{Read, Write};
        use std::os::unix::fs::OpenOptionsExt;
        let mut b = [0u8; 6];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
        let id: String = b.iter().map(|x| format!("{x:02x}")).collect();
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(format!("{id}\n").as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)?;
        Ok(id)
    })();
    match made {
        Ok(id) => Some(id),
        Err(e) => {
            eprintln!("[agent-api] no device id: {}: {e}", path.display());
            None
        }
    }
}

/// Every `/agent-api` answer (reads, changes, refusals, core down) says which device it is, so
/// the computer can tell when an address now reaches another one.
fn with_device_id(mut v: Value) -> Value {
    if let Some(o) = v.as_object_mut()
        && let Some(id) = device_id_at(&id_file())
    {
        o.insert("device_id".into(), id.into());
    }
    v
}

fn log(name: &str, path: &str) {
    eprintln!("[agent-api] {name} GET {path}");
}

async fn health(State(app): State<Shared>, req: Request) -> Response {
    let name = match admit(&req) {
        Ok(n) => n,
        Err(r) => return r,
    };
    log(&name, "health");
    let (Ok(s), Ok(h)) = (
        read(&app, "/api/public/status").await,
        read(&app, "/api/health").await,
    ) else {
        return core_down();
    };
    match (s, h) {
        (Ok(s), Ok(h)) => reply(shape_health(&s, &h)),
        (Err(c), _) => unavailable("status", c),
        (_, Err(c)) => unavailable("health", c),
    }
}

async fn utc_offset(app: &Shared) -> i64 {
    read(app, "/api/public/status")
        .await
        .ok()
        .and_then(Result::ok)
        .and_then(|s| s["clock"]["utc_offset"].as_i64())
        .unwrap_or(0)
}

async fn diagnose(State(app): State<Shared>, req: Request) -> Response {
    let name = match admit(&req) {
        Ok(n) => n,
        Err(r) => return r,
    };
    log(&name, "diagnose");
    let Ok(d) = read(&app, "/api/diagnose").await else {
        return core_down();
    };
    let d = match d {
        Ok(d) => d,
        Err(c) => return unavailable("diagnose", c),
    };
    let offset = utc_offset(&app).await;
    reply(shape_diagnose(&d, device_now(), offset))
}

async fn power(State(app): State<Shared>, req: Request) -> Response {
    let name = match admit(&req) {
        Ok(n) => n,
        Err(r) => return r,
    };
    log(&name, "power");
    let Ok(b) = read(&app, "/api/battery").await else {
        return core_down();
    };
    // each part may be missing on its own (no battery info = 503): null, not a failure
    let b = b.ok();
    let m = read(&app, "/api/modem/status")
        .await
        .ok()
        .and_then(Result::ok);
    let p = read(&app, "/api/wifi/power-save")
        .await
        .ok()
        .and_then(Result::ok);
    reply(shape_power(b.as_ref(), m.as_ref(), p.as_ref()))
}

#[derive(serde::Deserialize)]
pub struct EventsQ {
    since: Option<u64>,
    limit: Option<usize>,
}

async fn events(State(app): State<Shared>, Query(q): Query<EventsQ>, req: Request) -> Response {
    let name = match admit(&req) {
        Ok(n) => n,
        Err(r) => return r,
    };
    log(&name, "events");
    let limit = q.limit.unwrap_or(EVENTS_DEFAULT).clamp(1, EVENTS_MAX);
    let page = |since: u64, limit: usize| {
        let app = app.clone();
        async move {
            match read(&app, &format!("/api/alerts?since={since}&limit={limit}")).await {
                Ok(Ok(a)) => Ok(a),
                Ok(Err(c)) => Err(unavailable("alerts", c)),
                Err(_) => Err(core_down()),
            }
        }
    };
    let (a, mut evs) = match q.since {
        Some(since) => match page(since, limit).await {
            Ok(a) => {
                let e = a["events"].as_array().cloned().unwrap_or_default();
                (a, e)
            }
            Err(r) => return r,
        },
        // no cursor yet: the newest `limit` (the queue holds at most 300 lines, 6 pages of 50)
        None => {
            let mut all: Vec<Value> = Vec::new();
            let mut since = 0;
            let mut last;
            loop {
                last = match page(since, EVENTS_MAX).await {
                    Ok(a) => a,
                    Err(r) => return r,
                };
                all.extend(last["events"].as_array().cloned().unwrap_or_default());
                since = last["next"].as_u64().unwrap_or(since);
                if last["more"] != true || all.len() > 400 {
                    break;
                }
            }
            let tail = all.split_off(all.len().saturating_sub(limit));
            last["gap"] = json!(false);
            last["more"] = json!(false);
            (last, tail)
        }
    };
    // over the cap: drop from the end and stop the cursor at the last event given, so the
    // dropped ones come with the next call
    let mut cut = false;
    loop {
        let mut v = shape_events(&a, &evs);
        if evs.is_empty() || v.to_string().len() <= REPLY_MAX {
            if cut && let Some(last) = evs.last() {
                v["next"] = last["seq"].clone();
                v["more"] = json!(true);
            }
            return reply(v);
        }
        evs.pop();
        cut = true;
    }
}

#[cfg(test)]
mod tests;
