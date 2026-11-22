//! CHILL's status page, regions and bypass list in u60d: the
//! scenario engine reads the page (`/data/region/active`, `/data/exit`) and drives the other two,
//! so they are here while u60d runs it (and the agent forwards the web's to them). The agent's
//! `chill.rs`, with the checks and the rendering from u60-api; mihomo straight over its API.

use axum::{body::Bytes, extract::State, response::Response};
use luyaz_api::chill as ch;
use serde_json::{Value, json};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

use super::{Compat, reply};

fn mihomo(c: &Compat, secs: u64) -> Option<(reqwest::Client, String)> {
    let env = std::fs::read_to_string(c.path(ch::CHILL_ENV)).unwrap_or_default();
    let secret = ch::env_var(&env, "CHILL_SECRET");
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(secs))
        .build()
        .ok()?;
    Some((client, secret))
}

fn with_secret(r: reqwest::RequestBuilder, secret: &str) -> reqwest::RequestBuilder {
    if secret.is_empty() {
        r
    } else {
        r.header("Authorization", format!("Bearer {secret}"))
    }
}

async fn get_json(client: &reqwest::Client, secret: &str, url: &str) -> Option<Value> {
    let r = with_secret(client.get(url), secret).send().await.ok()?;
    if !r.status().is_success() {
        return None;
    }
    serde_json::from_slice(&r.bytes().await.ok()?).ok()
}

fn read_state(c: &Compat) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(c.path(ch::CHILL_STATE)).ok()?).ok()
}

/// GET /api/services/chill
pub async fn status(State(c): State<Compat>) -> Response {
    let st = read_state(&c);
    let running = st
        .as_ref()
        .and_then(|v| v.get("state"))
        .and_then(Value::as_str)
        == Some("running");
    let (mut version, mut proxies, mut mode, mut configs_raw, mut providers, mut connections_raw) = (None, None, None, None, None, None);
    if running && let Some((client, secret)) = mihomo(&c, 3) {
        let api = &c.src.mihomo;
        let version_url = format!("{api}/version");
        let proxies_url = format!("{api}/proxies");
        let configs_url = format!("{api}/configs");
        let providers_url = format!("{api}/providers/proxies");
        let connections_url = format!("{api}/connections");
        (version, proxies, configs_raw, providers, connections_raw) = tokio::join!(
            get_json(&client, &secret, &version_url),
            get_json(&client, &secret, &proxies_url),
            get_json(&client, &secret, &configs_url),
            get_json(&client, &secret, &providers_url),
            get_json(&client, &secret, &connections_url),
        );
        mode = configs_raw.as_ref().and_then(|v| v["mode"].as_str().map(str::to_owned));
    }
    let mut data = ch::status_data(
        st,
        version.as_ref(),
        proxies.as_ref(),
        mode,
        super::status::manual_first_summary(&c),
    );
    let configs = crate::state::blocking(move || super::chill_profiles::summary(&c)).await;
    data["active_profile"] = configs["profiles"]
        .as_array()
        .and_then(|all| all.iter().find(|p| p["active"] == true))
        .cloned()
        .unwrap_or(Value::Null);
    data["configs"] = configs;
    let ipv6_mode = match data["ipv6_mode"].as_str() {
        Some(mode @ ("blocked" | "tun")) => mode,
        _ => "unavailable",
    };
    data["ipv6_mode"] = json!(ipv6_mode);
    data["tun"] = tun_data(configs_raw.as_ref());
    data["connections"] = connections_data(connections_raw.as_ref());
    let mut stats = providers_data(providers.as_ref());
    data["provider_count"] = json!(stats["providers"].as_array().map_or(0, Vec::len));
    data["group_count"] = json!(data["groups"].as_array().map_or(0, Vec::len));
    data["providers"] = stats["providers"].take();
    reply(200, json!({"ok": true, "data": data}))
}

fn tun_data(config: Option<&Value>) -> Value {
    let tun = config.and_then(|v| v.get("tun"));
    json!({
        "enabled": tun.and_then(|v| v.get("enable")).and_then(Value::as_bool).unwrap_or(false),
        "auto_route": tun.and_then(|v| v.get("auto-route")).and_then(Value::as_bool).unwrap_or(false),
        "stack": tun.and_then(|v| v.get("stack")).and_then(Value::as_str).unwrap_or(""),
        "device": tun.and_then(|v| v.get("device")).and_then(Value::as_str).unwrap_or(""),
    })
}

fn connections_data(raw: Option<&Value>) -> Value {
    let Some(raw) = raw else {
        return Value::Null;
    };
    let list = raw["connections"].as_array().map_or(&[][..], Vec::as_slice);
    let upload = raw["uploadTotal"].as_u64().unwrap_or(0);
    let download = raw["downloadTotal"].as_u64().unwrap_or(0);
    let items = list.iter().take(32).map(|conn| {
        let metadata = &conn["metadata"];
        json!({
            "id": conn.get("id").and_then(Value::as_str).unwrap_or(""),
            "host": metadata["host"].as_str().filter(|host| !host.is_empty())
                .or_else(|| metadata["destinationIP"].as_str()).unwrap_or(""),
            "network": metadata["network"].as_str().unwrap_or(""),
            "rule": conn.get("rule").and_then(Value::as_str).unwrap_or(""),
            "chains": conn.get("chains").cloned().unwrap_or_else(|| json!([])),
            "upload": conn.get("upload").and_then(Value::as_u64).unwrap_or(0),
            "download": conn.get("download").and_then(Value::as_u64).unwrap_or(0),
        })
    }).collect::<Vec<_>>();
    json!({"count": list.len(), "upload_total": upload, "download_total": download, "items": items})
}

/// The backup region manual-first holds, if any (its live engine, or its file).
fn backup(c: &Compat) -> String {
    c.manual_first
        .live()
        .map(|m| m.backup())
        .or_else(|| {
            std::fs::read_to_string(c.path(ch::MANUAL_FIRST_STATE))
                .ok()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .and_then(|v| v["backup"].as_str().map(str::to_string))
        })
        .unwrap_or_default()
}

/// PUT /api/services/chill/regions `{group, member}`
pub async fn regions(State(c): State<Compat>, body: Bytes) -> Response {
    let (group, member) = match ch::region_wanted(&body) {
        Ok(w) => w,
        Err((s, b)) => return reply(s, b),
    };
    let Some(_slot) = super::chill_jobs::reserve(&c) else {
        return super::chill_jobs::busy();
    };
    let Some((client, secret)) = mihomo(&c, 3) else {
        return reply(500, json!({"ok": false, "error": "internal"}));
    };
    let group_url = format!("{}/proxies/{}", c.src.mihomo, super::home::enc(&group));
    let Some(info) = get_json(&client, &secret, &group_url).await else {
        return reply(502, json!({"ok": false, "error": "mihomo unreachable"}));
    };
    if !ch::in_group(&info, &member) {
        return reply(400, json!({"ok": false, "error": "member not in group"}));
    }
    if group == ch::MAIN_GROUP
        && member == "DIRECT"
        && let Some(now) = info.get("now").and_then(Value::as_str)
    {
        // While manual-first holds a backup region, the owner's choice is still 🎯.
        let keep = ch::main_to_remember(now, &backup(&c));
        if !keep.is_empty() && keep != "DIRECT" {
            let _ = std::fs::write(c.path(ch::LAST_MAIN), keep);
        }
    }
    let r = with_secret(
        client
            .put(&group_url)
            .header("Content-Type", "application/json")
            .body(json!({"name": member}).to_string()),
        &secret,
    )
    .send()
    .await;
    match r {
        Ok(r) if r.status().as_u16() < 400 => reply(
            200,
            json!({"ok": true, "data": {"group": group, "active": member}}),
        ),
        Ok(r) => {
            let code = r.status().as_u16();
            reply(
                502,
                json!({"ok": false, "error": format!("mihomo rejected switch (HTTP {code})")}),
            )
        }
        Err(e) => reply(
            502,
            json!({"ok": false, "error": format!("switch request failed: {e}")}),
        ),
    }
}

/// GET /api/services/chill/bypass
pub async fn bypass_get(State(c): State<Compat>) -> Response {
    let env = std::fs::read_to_string(c.path(ch::CHILL_ENV)).unwrap_or_default();
    reply(
        200,
        json!({"ok": true, "data": ch::bypass_data(&env, read_state(&c).as_ref())}),
    )
}

/// PUT /api/services/chill/bypass `{ips: [...]}`. CHILL commits the DNS and route
/// changes together, then persists the list so it can replay it at startup.
pub async fn bypass_set(State(c): State<Compat>, body: Bytes) -> Response {
    let ips = match ch::bypass_wanted(&body) {
        Ok(ips) => ips,
        Err((s, b)) => return reply(s, b),
    };
    let script = c.path("/data/chill/chill.sh");
    if !script.is_file() {
        return reply(409, json!({"ok": false, "error": "chill_not_installed"}));
    }
    let Some(slot) = super::chill_jobs::reserve(&c) else {
        return super::chill_jobs::busy();
    };
    // The detached worker owns the slot until the script finishes even if
    // the HTTP request is cancelled during the network update.
    tokio::spawn(async move {
        let _release = slot;
        // `bypass_wanted` canonicalizes each entry to an IPv4 address.
        let command = format!(
            "sh {} bypass-set {}",
            super::chill_jobs::shell_quote(&script.to_string_lossy()),
            ips.join(" ")
        );
        match crate::ops::chill::sh_within(&command, Duration::from_secs(12)).await {
            Ok((true, _)) => reply(200, json!({"ok": true, "data": {"ips": ips}})),
            Ok((false, _)) => reply(409, json!({"ok": false, "error": "bypass_dns_unavailable"})),
            Err(_) => reply(500, json!({"ok": false, "error": "bypass_update_failed"})),
        }
    })
    .await
    .unwrap_or_else(|_| reply(500, json!({"ok": false, "error": "bypass_update_failed"})))
}

// ---- the page's other reads, and the subscription refresh (8d-1b, batch 5b) ----

const CHILL_LOG: &str = "/tmp/chill.log";
const LOG_TAIL_BYTES: u64 = 512 * 1024;
/// A refresh can fetch the subscription.
const REFRESH_SECS: u64 = 15;

fn log_limit(query: &str) -> usize {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .filter(|(key, _)| matches!(*key, "lines" | "n"))
        .find_map(|(_, value)| value.parse::<usize>().ok())
        .map_or(200, |n| n.clamp(1, 2000))
}

fn tail_log(path: &Path, limit: usize) -> std::io::Result<Vec<String>> {
    let mut file = std::fs::File::open(path)?;
    let start = file.metadata()?.len().saturating_sub(LOG_TAIL_BYTES);
    if start > 0 {
        file.seek(SeekFrom::Start(start))?;
    }
    let mut buf = Vec::new();
    file.take(LOG_TAIL_BYTES).read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf);
    let text = if start > 0 {
        text.split_once('\n').map_or("", |(_, rest)| rest)
    } else {
        text.as_ref()
    };
    let mut lines: Vec<_> = text.lines().rev().take(limit).map(str::to_owned).collect();
    lines.reverse();
    Ok(lines)
}

/// GET /api/services/chill/log?lines=N
pub async fn log(
    State(c): State<Compat>,
    axum::extract::RawQuery(q): axum::extract::RawQuery,
) -> Response {
    let limit = log_limit(q.as_deref().unwrap_or_default());
    let path = c.path(CHILL_LOG);
    let read = tokio::task::spawn_blocking(move || tail_log(&path, limit));
    let lines = match tokio::time::timeout(Duration::from_secs(8), read).await {
        Ok(Ok(Ok(lines))) => lines,
        _ => Vec::new(),
    };
    reply(200, json!({"ok": true, "data": {"path": CHILL_LOG, "lines": lines, "limit": limit}}))
}

/// The agent's `providers_list` from mihomo's `/providers/proxies`: the real subscriptions (not
/// mihomo's "Compatible" wrappers of DIRECT/REJECT and the groups), sorted by name.
pub(super) fn providers_data(raw: Option<&Value>) -> Value {
    let Some(providers) = raw
        .and_then(|v| v.get("providers"))
        .and_then(Value::as_object)
    else {
        return json!({"providers": []});
    };
    let mut out: Vec<Value> = providers
        .iter()
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
                "node_count": info.get("proxies").and_then(Value::as_array).map_or(0, Vec::len),
                "subscription": info.get("subscriptionInfo").cloned(),
                "editable": name.as_str() == ch::EDITABLE_PROVIDER,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        a["name"]
            .as_str()
            .unwrap_or("")
            .cmp(b["name"].as_str().unwrap_or(""))
    });
    json!({"providers": out})
}

/// GET /api/services/chill/providers
pub async fn providers_list(State(c): State<Compat>) -> Response {
    let raw = match mihomo(&c, 3) {
        Some((client, secret)) => {
            get_json(
                &client,
                &secret,
                &format!("{}/providers/proxies", c.src.mihomo),
            )
            .await
        }
        None => None,
    };
    reply(
        200,
        json!({"ok": true, "data": providers_data(raw.as_ref())}),
    )
}

/// POST /api/services/chill/providers/refresh `{"name"}`: mihomo re-fetches that provider.
pub async fn providers_refresh(State(c): State<Compat>, body: Bytes) -> Response {
    let Ok(parsed) = serde_json::from_slice::<Value>(&body) else {
        return reply(400, json!({"ok": false, "error": "invalid JSON"}));
    };
    let name = parsed["name"].as_str().unwrap_or("").trim().to_string();
    if name.is_empty() {
        return reply(400, json!({"ok": false, "error": "missing 'name'"}));
    }
    let Some((client, secret)) = mihomo(&c, REFRESH_SECS) else {
        return reply(
            502,
            json!({"ok": false, "error": "refresh request failed: no client"}),
        );
    };
    let url = format!(
        "{}/providers/proxies/{}",
        c.src.mihomo,
        super::home::enc(&name)
    );
    match with_secret(
        client
            .put(url)
            .header("Content-Type", "application/json")
            .body("{}"),
        &secret,
    )
    .send()
    .await
    {
        Ok(r) if r.status().as_u16() < 400 => reply(200, json!({"ok": true})),
        Ok(r) => reply(
            502,
            json!({"ok": false, "error": format!("mihomo rejected refresh (HTTP {})", r.status().as_u16())}),
        ),
        Err(e) => reply(
            502,
            json!({"ok": false, "error": format!("refresh request failed: {e}")}),
        ),
    }
}
