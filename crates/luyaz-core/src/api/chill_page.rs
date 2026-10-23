//! CHILL's status page, regions and bypass list in u60d: the
//! scenario engine reads the page (`/data/region/active`, `/data/exit`) and drives the other two,
//! so they are here while u60d runs it (and the agent forwards the web's to them). The agent's
//! `chill.rs`, with the checks and the rendering from u60-api; mihomo straight over its API.

use axum::{body::Bytes, extract::State, response::Response};
use luyaz_api::chill as ch;
use serde_json::{Value, json};
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
    let (mut version, mut proxies, mut mode) = (None, None, None);
    if running && let Some((client, secret)) = mihomo(&c, 3) {
        let api = &c.src.mihomo;
        version = get_json(&client, &secret, &format!("{api}/version")).await;
        proxies = get_json(&client, &secret, &format!("{api}/proxies")).await;
        mode = get_json(&client, &secret, &format!("{api}/configs"))
            .await
            .and_then(|v| v.get("mode").and_then(Value::as_str).map(str::to_string));
    }
    let data = ch::status_data(
        st,
        version.as_ref(),
        proxies.as_ref(),
        mode,
        super::status::manual_first_summary(&c),
        super::status::home_exit_summary(&c),
    );
    reply(200, json!({"ok": true, "data": data}))
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

/// `sh -c` with a deadline; (ok, stdout + stderr).
fn sh(cmd: &str) -> (bool, String) {
    luyaz_wifi::output_within(
        std::process::Command::new("sh").arg("-c").arg(cmd),
        Duration::from_secs(5),
    )
    .map_or_else(
        |e| (false, format!("exec failed: {e}")),
        |o| {
            let both = [o.stdout.as_slice(), o.stderr.as_slice()].map(String::from_utf8_lossy);
            (o.status.success(), both.concat())
        },
    )
}

/// PUT /api/services/chill/bypass `{ips: [...]}` (the full list): only `ip rule` entries at
/// chill.sh's priority — no reload, no config touched — then the list into chill.env, so the
/// next core start (chill.sh's `apply_bypass`) makes the same rules.
pub async fn bypass_set(State(c): State<Compat>, body: Bytes) -> Response {
    let ips = match ch::bypass_wanted(&body) {
        Ok(ips) => ips,
        Err((s, b)) => return reply(s, b),
    };
    let rules = ips.clone();
    let added = crate::state::blocking(move || -> Result<(), String> {
        // one at a time: this `ip rule del pref N` removes one entry per call
        for _ in 0..ch::MAX_BYPASS_IPS + 8 {
            let (_, out) = sh(&format!(
                "ip rule show | awk -F: '$1+0=={} {{print $1; exit}}'",
                ch::BYPASS_PRIO
            ));
            let pref = out.trim().to_string();
            if pref.is_empty() {
                break;
            }
            let _ = sh(&format!("ip rule del pref {pref}"));
        }
        for ip in &rules {
            let (ok, out) = sh(&format!(
                "ip rule add from {ip} priority {} lookup main",
                ch::BYPASS_PRIO
            ));
            if !ok {
                return Err(format!(
                    "failed to add rule for {ip}: {}",
                    ch::tail(&out, 3)
                ));
            }
        }
        Ok(())
    })
    .await;
    if let Err(e) = added {
        return reply(500, json!({"ok": false, "error": e}));
    }
    let p = c.path(ch::CHILL_ENV);
    let out = match ch::env_with(
        &std::fs::read_to_string(&p).unwrap_or_default(),
        &[("CHILL_BYPASS_IP", &ips.join(" "))],
    ) {
        Ok(o) => o,
        Err(e) => return reply(500, json!({"ok": false, "error": e})),
    };
    // chill.env holds the subscription and mihomo's secret: keep its mode
    let mode = std::fs::metadata(&p).ok().map(|m| m.permissions());
    if let Err(e) = super::ops::atomic_write(&p, out.as_bytes()) {
        return reply(
            500,
            json!({"ok": false, "error": format!("cannot write chill.env: {e}")}),
        );
    }
    if let Some(m) = mode {
        let _ = std::fs::set_permissions(&p, m);
    }
    reply(200, json!({"ok": true, "data": {"ips": ips}}))
}

// ---- the page's other reads, and the subscription refresh (8d-1b, batch 5b) ----

const CHILL_LOG: &str = "/tmp/chill.log";
/// A refresh can fetch the subscription.
const REFRESH_SECS: u64 = 15;

/// GET /api/services/chill/log?lines=N
pub async fn log(
    State(c): State<Compat>,
    axum::extract::RawQuery(q): axum::extract::RawQuery,
) -> Response {
    super::ts_reads::log_reply(&c, CHILL_LOG, q).await
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
