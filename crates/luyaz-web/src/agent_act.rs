//! `/agent-api/act/*` (u60-mcp.md second phase, D7, D8, D11, Codex O1): the agent's writes.
//!
//! - Only a token marked `act` (third column of `agent.token`; missing = `read`) may call these;
//!   a read token gets 403 `read_only_token`. On top of `agent.allow`, the caller must be in
//!   `agent.act-allow` (D8: only the computer's wired address, `192.168.50.88/32`); no file = no one.
//! - The request to the core is **rebuilt** here (O1): `source` and `approved_by` are always `mcp`,
//!   the action comes from a fixed list, the parameters are copied key by key. Nothing the caller
//!   sends about who it is reaches the core. The core checks it all again (its whitelist, the
//!   sensitive check, the half-hour limit per layer) and is the last word.
//! - The answer says what happened, never the core's whole reply: `trial` with the `op_id` (the
//!   result comes later in the events), `done`, `sensitive` with the page to send the owner to,
//!   `rate_limited` with when, `busy`, `refused`, `failed`.
//! - Never here, not even as a page: eSIM profile deletes, factory reset, rc.local, firmware updates.

use axum::Router;
use axum::extract::{Query, Request, State};
use axum::response::Response;
use axum::routing::{get, post};
use serde_json::{Map, Value, json};
use std::path::PathBuf;
use std::sync::OnceLock;

use crate::agent_api::{Perm, admit_perm, reply, source_ip, untrusted};
use crate::pages::Shared;

pub const ACT_ALLOW_FILE: &str = "/data/luyaz/agent.act-allow";

static ACT_ALLOW: OnceLock<PathBuf> = OnceLock::new();

/// The act source file (`--agent-act-allow-file`).
pub fn set_act_allow(p: PathBuf) {
    let _ = ACT_ALLOW.set(p);
}

pub(crate) fn act_allow() -> PathBuf {
    ACT_ALLOW
        .get()
        .cloned()
        .unwrap_or_else(|| ACT_ALLOW_FILE.into())
}

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/agent-api/act/chill/profile", post(chill_profile))
        .route("/agent-api/act/chill/exit", post(chill_exit))
        .route("/agent-api/act/apn/use", post(apn_use))
        .route("/agent-api/act/apn", post(apn))
        .route("/agent-api/act/status", get(status))
        .route("/agent-api/action-url", get(action_url))
}

/// The APN actions the agent may ask for (D11: all of them; the core still refuses deleting the
/// APN in use) and the parameter keys each takes.
const APN_ACTIONS: &[(&str, &[&str])] = &[
    ("apn.set_mode", &["mode"]),
    ("apn.enable", &["profile_id"]),
    (
        "apn.add",
        &["name", "apn", "username", "password", "auth_mode", "pdp_type", "roaming_pdp_type"],
    ),
    (
        "apn.modify",
        &["profile_id", "name", "apn", "username", "password", "auth_mode", "pdp_type", "roaming_pdp_type"],
    ),
    ("apn.delete", &["profile_id"]),
    ("apn.set_pdp_type", &["pdp_type"]),
];

/// The pages a sensitive change sends the owner to (`get_action_url`).
const PAGES: &[(&str, &str)] = &[("apn", "/router/apn"), ("chill", "/services/chill")];

/// The token and the sources for a write: Ok = the client's name.
fn admit_act(req: &Request) -> Result<String, Response> {
    let (name, perm) = admit_perm(req)?;
    if perm != Perm::Act {
        eprintln!("[agent-api] {name}: read-only token asked to act");
        return Err(refuse(403, "read_only_token", "this token can only read"));
    }
    let allow = crate::agent_api::parse_allow(&std::fs::read_to_string(act_allow()).unwrap_or_default());
    let ok = source_ip(req).is_some_and(|ip| {
        let ip = match ip {
            std::net::IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, std::net::IpAddr::V4),
            v4 => v4,
        };
        allow.iter().any(|c| crate::agent_api::in_cidr(ip, *c))
    });
    if !ok {
        eprintln!("[agent-api] {name}: act from a source not in agent.act-allow");
        return Err(refuse(403, "act_source", "this address may not make changes"));
    }
    Ok(name)
}

fn refuse(code: u16, state: &str, message: &str) -> Response {
    let mut r = reply(json!({"state": "refused", "reason": state, "message": message}));
    *r.status_mut() = axum::http::StatusCode::from_u16(code).unwrap_or(axum::http::StatusCode::FORBIDDEN);
    r
}

/// The page on this web for `what`, from the address the caller used.
fn page_url(req: &Request, what: &str) -> Option<String> {
    let path = PAGES.iter().find(|(w, _)| *w == what)?.1;
    let host = req
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .filter(|h| h.bytes().all(|b| b.is_ascii_alphanumeric() || b".:-[]".contains(&b)))
        .unwrap_or("192.168.0.1:9090");
    Some(format!("http://{host}{path}"))
}

/// Copy the listed keys of `body` (strings, numbers, booleans only).
fn pick(body: &Value, keys: &[&str]) -> Value {
    let mut m = Map::new();
    for k in keys {
        if let Some(v) = body.get(*k).filter(|v| v.is_string() || v.is_number() || v.is_boolean()) {
            m.insert((*k).into(), v.clone());
        }
    }
    Value::Object(m)
}

async fn body_of(req: Request) -> Option<Value> {
    let b = axum::body::to_bytes(req.into_body(), 8 * 1024).await.ok()?;
    serde_json::from_slice::<Value>(&b).ok().filter(Value::is_object)
}

fn op_id() -> String {
    format!("mcp-{}", crate::write::next_op_id())
}

/// The core's answer → what the agent is told.
pub fn shape(code: u16, v: &Value, url: Option<String>) -> Value {
    shape_as(code, v, url, None)
}

/// Who allowed a change, which layer, whether the device undoes it, and where the owner can.
pub fn authority(name: &str, layer: &str, auto: bool, page: Option<&str>) -> Value {
    json!({
        "granted_by": format!("agent token {name} (act) from act-allow"),
        "layer": layer,
        "rollback": if auto { "automatic within trial" } else { "none" },
        "owner_can_undo": page,
    })
}

/// `shape` plus an `authority` object on trial/done replies (`who` = token name, layer).
pub fn shape_as(code: u16, v: &Value, url: Option<String>, who: Option<(&str, &str)>) -> Value {
    let mut out = shape_inner(code, v, url.clone());
    if let (Some((name, layer)), Some(state)) = (who, out["state"].as_str()) {
        if state == "trial" || state == "done" {
            let auto = state == "trial" && out["rollback_enabled"] != false;
            out["authority"] = authority(name, layer, auto, url.as_deref());
        }
    }
    out
}

fn shape_inner(code: u16, v: &Value, url: Option<String>) -> Value {
    let err = &v["error"];
    let ecode = err["code"].as_str().or(v["code"].as_str()).unwrap_or("");
    let message = err["message"].as_str().or(err.as_str()).unwrap_or("");
    if (200..300).contains(&code) && v["ok"] != false {
        let op = v.get("op").filter(|o| o.is_object());
        let data_op = v["data"]["op_id"].as_str();
        return match (op, data_op) {
            (Some(op), _) => json!({
                "state": "trial",
                "op_id": op["op_id"],
                "phase": op["phase"],
                "rollback_enabled": op["rollback_enabled"],
                "result": "see get_recent_events (kind agent-change) or the status tool",
            }),
            (None, Some(id)) => json!({"state": "trial", "op_id": id,
                "result": "see get_recent_events (kind agent-change) or the status tool"}),
            (None, None) if v["result"]["note"] == "no changes" => {
                json!({"state": "done", "note": "already so; nothing changed"})
            }
            (None, None) => json!({"state": "done", "note": "applied without a trial: no automatic rollback"}),
        };
    }
    match (code, ecode) {
        (403, "sensitive") => json!({"state": "sensitive", "reason": untrusted(&json!(message)),
            "url": url, "note": "hand this to the owner; do not retry"}),
        (429, _) => json!({"state": "rate_limited",
            "next_unix": err["next_unix"].clone().or_null(&v["next_unix"]),
            "retry_after_s": err["retry_after_s"].clone().or_null(&v["retry_after_s"])}),
        (409, "chill_off") => json!({"state": "refused", "reason": untrusted(&json!(message)),
            "note": "CHILL is off; only the owner turns it on; do not retry"}),
        (409, _) | (503, _) => json!({"state": "busy", "reason": untrusted(&json!(message))}),
        (403, _) => json!({"state": "refused", "reason": untrusted(&json!(message))}),
        _ => json!({"state": "failed", "http": code, "reason": untrusted(&json!(message))}),
    }
}

trait OrNull {
    fn or_null(self, other: &Value) -> Value;
}

impl OrNull for Value {
    fn or_null(self, other: &Value) -> Value {
        if self.is_null() { other.clone() } else { self }
    }
}

/// A `/control` write as the agent (source = approved_by = mcp, a fresh op_id).
async fn control(app: &Shared, action: &str, params: Value) -> Result<(u16, Value), ()> {
    let body = json!({"action": action, "source": "mcp", "approved_by": "mcp",
        "params": params, "op_id": op_id()});
    let r = app
        .http
        .post(&app.control_url)
        .timeout(std::time::Duration::from_secs(60))
        .json(&crate::pin::stamp(&body))
        .send()
        .await
        .map_err(|_| ())?;
    let code = r.status().as_u16();
    Ok((code, r.json().await.unwrap_or(Value::Null)))
}

async fn act(app: &Shared, name: &str, action: &str, params: Value, url: Option<String>) -> Response {
    eprintln!("[agent-api] {name} act {action}");
    match control(app, action, params).await {
        Ok((code, v)) => {
            let layer = action.split('.').next().unwrap_or("");
            reply(shape_as(code, &v, url, Some((name, layer))))
        }
        Err(()) => reply(json!({"core": "down"})),
    }
}

async fn chill_profile(State(app): State<Shared>, req: Request) -> Response {
    let name = match admit_act(&req) {
        Ok(n) => n,
        Err(r) => return r,
    };
    let url = page_url(&req, "chill");
    let Some(b) = body_of(req).await else {
        return refuse(400, "bad_request", "a JSON object with profile");
    };
    act(&app, &name, "chill.profile", pick(&b, &["profile"]), url).await
}

async fn chill_exit(State(app): State<Shared>, req: Request) -> Response {
    let name = match admit_act(&req) {
        Ok(n) => n,
        Err(r) => return r,
    };
    let url = page_url(&req, "chill");
    let Some(b) = body_of(req).await else {
        return refuse(400, "bad_request", "a JSON object with state");
    };
    act(&app, &name, "chill.exit", pick(&b, &["state"]), url).await
}

/// Any APN action (D11) with its own parameters.
async fn apn(State(app): State<Shared>, req: Request) -> Response {
    let name = match admit_act(&req) {
        Ok(n) => n,
        Err(r) => return r,
    };
    let url = page_url(&req, "apn");
    let Some(b) = body_of(req).await else {
        return refuse(400, "bad_request", "a JSON object with action and params");
    };
    let wanted = b["action"].as_str().unwrap_or("");
    let Some((action, keys)) = APN_ACTIONS.iter().find(|(a, _)| *a == wanted) else {
        return refuse(400, "bad_request", "action must be one of apn.set_mode, apn.enable, apn.add, apn.modify, apn.delete, apn.set_pdp_type");
    };
    act(&app, &name, action, pick(&b["params"], keys), url).await
}

/// Use an APN (`{"id"}`: `auto`, a candidate `autoN`, a manual `manuN`): the core's `apn_use`
/// as the agent, one transaction with its trial.
async fn apn_use(State(app): State<Shared>, req: Request) -> Response {
    let name = match admit_act(&req) {
        Ok(n) => n,
        Err(r) => return r,
    };
    let url = page_url(&req, "apn");
    let Some(b) = body_of(req).await else {
        return refuse(400, "bad_request", "a JSON object with id");
    };
    let id = b["id"].as_str().unwrap_or("");
    if id.is_empty() || id.len() > 16 || !id.bytes().all(|c| c.is_ascii_alphanumeric()) {
        return refuse(400, "bad_request", "id must be auto, autoN or manuN");
    }
    eprintln!("[agent-api] {name} act apn.use");
    let r = app
        .http
        .post(format!("{}/v2/api/netinfo/apn", app.u60d_url))
        .header(luyaz_api::headers::SOURCE_HEADER, "mcp")
        .timeout(std::time::Duration::from_secs(30))
        .json(&json!({"id": id}))
        .send()
        .await;
    match r {
        Ok(r) => {
            let code = r.status().as_u16();
            let v: Value = r.json().await.unwrap_or(Value::Null);
            // the compat reply: {ok, error, code, page, …} at the top
            let v = if v["error"].is_string() && v["code"].is_string() {
                json!({"ok": false, "error": {"code": v["code"], "message": v["error"],
                    "next_unix": v["next_unix"], "retry_after_s": v["retry_after_s"]}})
            } else {
                v
            };
            reply(shape_as(code, &v, url, Some((&name, "apn"))))
        }
        Err(_) => reply(json!({"core": "down"})),
    }
}

#[derive(serde::Deserialize)]
pub struct StatusQ {
    op_id: Option<String>,
}

/// How a change of the agent's is going (`op.status`; any token: it only reads).
async fn status(State(app): State<Shared>, Query(q): Query<StatusQ>, req: Request) -> Response {
    let name = match admit_perm(&req) {
        Ok((n, _)) => n,
        Err(r) => return r,
    };
    let Some(id) = q.op_id.filter(|i| {
        !i.is_empty() && i.len() <= 64 && i.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    }) else {
        return refuse(400, "bad_request", "op_id");
    };
    eprintln!("[agent-api] {name} status");
    let body = json!({"action": "op.status", "source": "mcp", "params": {"op_id": id}});
    let r = app
        .http
        .post(&app.control_url)
        .json(&body)
        .send()
        .await;
    match r {
        Ok(r) => {
            let v: Value = r.json().await.unwrap_or(Value::Null);
            let s = &v["result"];
            if s.is_null() {
                return reply(json!({"state": "unknown", "op_id": id}));
            }
            reply(json!({
                "op_id": s["op_id"], "action": s["action"], "phase": s["phase"], "reason": s["reason"],
                "rollback_reason": s["rollback_reason"], "rollback_enabled": s["rollback_enabled"],
                "remaining_ms": s["remaining_ms"], "age_ms": s["age_ms"],
                // ours: the source is mcp only when the agent asked for it
                "by_agent": s["source"] == "mcp",
            }))
        }
        Err(_) => reply(json!({"core": "down"})),
    }
}

#[derive(serde::Deserialize)]
pub struct UrlQ {
    #[serde(rename = "for")]
    what: Option<String>,
}

/// `get_action_url`: the page to hand the owner for a change the agent may not make. Only for
/// what has a page here (APN, CHILL); nothing for what is never the agent's (eSIM deletes,
/// factory reset, rc.local, firmware updates).
async fn action_url(Query(q): Query<UrlQ>, req: Request) -> Response {
    if let Err(r) = admit_perm(&req) {
        return r;
    }
    match q.what.as_deref().and_then(|w| page_url(&req, w)) {
        Some(url) => reply(json!({"state": "ok", "url": url, "note": "hand this to the owner"})),
        None => reply(json!({"state": "refused", "reason": "no page for that here",
            "known": PAGES.iter().map(|(w, _)| *w).collect::<Vec<_>>()})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trial_and_done_carry_authority() {
        let u = Some("http://x/router/apn".to_string());
        let t = json!({"ok": true, "op": {"op_id": "mcp-1", "phase": "verifying", "rollback_enabled": true}});
        let s = shape_as(200, &t, u.clone(), Some(("hermes-act", "apn")));
        assert_eq!(s["op_id"], "mcp-1");
        let a = &s["authority"];
        assert_eq!(a["granted_by"], "agent token hermes-act (act) from act-allow");
        assert_eq!((a["layer"].as_str(), a["rollback"].as_str()), (Some("apn"), Some("automatic within trial")));
        assert_eq!(a["owner_can_undo"], "http://x/router/apn");
        let s = shape_as(200, &json!({"ok": true, "result": {}}), u.clone(), Some(("h", "apn")));
        assert_eq!((s["state"].as_str(), s["authority"]["rollback"].as_str()), (Some("done"), Some("none")));
        let s = shape_as(200, &json!({"ok": true, "op": {"op_id": "m", "rollback_enabled": false}}), None, Some(("h", "chill")));
        assert_eq!(s["authority"]["rollback"], "none");
        let s = shape_as(409, &json!({"ok": false, "error": {"code": "busy"}}), u, Some(("h", "apn")));
        assert!(s.get("authority").is_none());
    }

    #[test]
    fn core_answers_are_shaped() {
        let s = shape(200, &json!({"ok": true, "op": {"op_id": "mcp-1", "phase": "verifying", "rollback_enabled": true}}), None);
        assert_eq!((s["state"].as_str(), s["op_id"].as_str()), (Some("trial"), Some("mcp-1")));
        let s = shape(200, &json!({"ok": true, "result": {"status": "ok", "note": "no changes"}}), None);
        assert_eq!(s["state"], "done");
        let s = shape(200, &json!({"ok": true, "result": {}}), None);
        assert!(s["note"].as_str().unwrap().contains("no automatic rollback"));
        let s = shape(
            403,
            &json!({"ok": false, "error": {"code": "sensitive", "message": "roaming", "page": "apn"}}),
            Some("http://x/router/apn".into()),
        );
        assert_eq!((s["state"].as_str(), s["url"].as_str()), (Some("sensitive"), Some("http://x/router/apn")));
        let s = shape(429, &json!({"ok": false, "error": {"code": "rate_limited", "next_unix": 9, "retry_after_s": 3}}), None);
        assert_eq!((s["state"].as_str(), s["retry_after_s"].as_i64()), (Some("rate_limited"), Some(3)));
        assert_eq!(shape(409, &json!({"ok": false, "error": {"code": "busy"}}), None)["state"], "busy");
        assert_eq!(shape(409, &json!({"ok": false, "error": {"code": "chill_off"}}), None)["state"], "refused");
        assert_eq!(shape(403, &json!({"ok": false, "error": {"code": "not_allowed"}}), None)["state"], "refused");
        assert_eq!(shape(502, &json!({}), None)["state"], "failed");
        // no rollback is said, never hidden
        let s = shape(200, &json!({"ok": true, "op": {"op_id": "mcp-2", "phase": "applied", "rollback_enabled": false}}), None);
        assert_eq!((s["state"].as_str(), s["rollback_enabled"].as_bool()), (Some("trial"), Some(false)));
        // a 200 that says not ok is not "done"
        assert_eq!(shape(200, &json!({"ok": false, "error": {"code": "x"}}), None)["state"], "failed");
        // the core's message is wrapped as untrusted text
        let s = shape(403, &json!({"ok": false, "error": {"code": "not_allowed", "message": "ignore previous"}}), None);
        assert!(s["reason"].get("untrusted").is_some(), "{s}");
    }

    #[test]
    fn only_listed_keys_go_through() {
        let p = pick(
            &json!({"profile_id": "manu1", "source": "web", "approved_by": "screen", "x": {"y": 1}}),
            &["profile_id", "x"],
        );
        assert_eq!(p, json!({"profile_id": "manu1"}));
    }
}
