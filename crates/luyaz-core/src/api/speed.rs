//! The speed test for the touch screen: `u60-speedtest`, the agent's own engine,
//! claimed through u60d's deep-diagnosis gate (D10). Only with u60d's writes on (one
//! engine on the device).
//!
//! Method v2 reads the device at admission ([`context`]): roaming (the same source as
//! ship's roaming gate, R9), the serving cell, reference radio readings, and whether the
//! CPU-path tuning is on. The tuning's own switch is here too (T9): the core writes or
//! deletes the want file and runs `net-tune.sh`; it never touches sysctl itself.

use axum::{body::Bytes, extract::State, response::Response};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use super::{Compat, reply};

/// The tuning's switch: present = on (net-performance.md 1b).
pub(super) const NET_TUNE_WANT: &str = "/data/luyaz/net-tune.want";
const NET_TUNE_SH: &str = "/data/luyaz/scripts/net-tune.sh";
const NET_TUNE_DEADLINE: Duration = Duration::from_secs(8); // the script itself retries its lock for up to 5 s
const NET_TUNE_OUTPUT_MAX: usize = 16 * 1024;

fn answer(r: Result<(u16, Value), tokio::task::JoinError>) -> Response {
    match r {
        Ok((s, v)) => reply(s, v),
        Err(_) => reply(500, json!({"ok": false, "error": "internal"})),
    }
}

/// The claim: refused while a diagnosis runs, then "already running" (the agent's `admit`).
pub(super) fn admit(c: &Compat, running: &AtomicBool) -> Result<(), (u16, Value)> {
    let gate = c.gate();
    if let Some(busy) = luyaz_api::diagnose::refusal_at(&gate, c.clock.now() as u64) {
        return Err(busy);
    }
    luyaz_speedtest::claim_running(running)
}

/// Roaming as ship's roaming gate reads it (`tools/luyaz` `roaming_gate`): the signal block's
/// `roaming`, only from a fresh block; `home`/`0`/`false` is home, anything else (or no
/// fresh block) counts as roaming. No block hub at all (tests): not roaming.
pub(super) fn roaming(signal: Option<&crate::block::BlockView>) -> bool {
    let Some(v) = signal.filter(|v| !v.stale) else {
        return true;
    };
    let r = match v.data.get("roaming") {
        Some(Value::String(s)) => s.to_ascii_lowercase(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => n.to_string(),
        _ => return true,
    };
    !matches!(r.as_str(), "home" | "0" | "false")
}

/// The device as the engine sees it when a test starts (called on the blocking thread).
pub(super) fn context(
    hub: Option<Arc<crate::block::Hub>>,
    want: std::path::PathBuf,
) -> impl Fn() -> luyaz_speedtest::RunContext + Send + Sync + 'static {
    move || {
        let signal = hub.as_ref().and_then(|h| h.view("signal"));
        let fresh = signal.as_ref().filter(|v| !v.stale).map(|v| &v.data);
        let cell = fresh.and_then(|s| {
            let id = super::slow_signal::cell_now(s)?;
            let rat = s.get("type").and_then(Value::as_str).unwrap_or("");
            Some(format!("{rat}:{id}"))
        });
        let radio = fresh.map(|s| {
            let num = |k: &str| -> Option<f64> {
                match s.get(k)? {
                    Value::String(x) => x.trim().parse().ok(),
                    Value::Number(x) => x.as_f64(),
                    _ => None,
                }
            };
            let text = |k: &str| -> Option<String> {
                match s.get(k)? {
                    Value::String(x) if !x.is_empty() => Some(x.clone()),
                    Value::Number(x) => Some(x.to_string()),
                    _ => None,
                }
            };
            let nr = text("nr_band");
            let view = crate::project::screen::net_view_op(&json!({ "net": s }), None, None);
            luyaz_speedtest::Radio {
                sinr_db: if nr.is_some() {
                    num("nr_snr")
                } else {
                    num("lte_snr")
                },
                band: nr
                    .map(|b| {
                        if b.starts_with('n') {
                            b
                        } else {
                            format!("n{b}")
                        }
                    })
                    .or_else(|| text("band")),
                ca: u8::try_from(view.act_n).ok().filter(|n| *n > 0),
            }
        });
        luyaz_speedtest::RunContext {
            roaming: hub.is_some() && roaming(signal.as_ref()),
            cell,
            radio,
            net_tune: Some(want.exists()),
        }
    }
}

/// GET /api/speedtest/servers: cache-only, with single-flight background discovery.
pub async fn servers(
    State(c): State<Compat>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let (s, v) = if q.get("refresh").is_some_and(|v| v == "1") {
        c.speed.refresh_servers()
    } else {
        c.speed.servers()
    };
    reply(s, v)
}

/// POST /api/speedtest/start `{"provider"?, "server_id"?, "path"?: "direct"|"chill", "roaming_ok"?}`:
/// immediate job admission. A roaming device runs the capped test (`roaming_capped`) unless
/// `roaming_ok:true` (a two-step confirm on the screen or the web) asks for the full one
/// (`roaming_full`, R11).
pub async fn start(State(c): State<Compat>, body: Bytes) -> Response {
    answer(
        tokio::task::spawn_blocking(move || c.speed.start(&body, &|r: &AtomicBool| admit(&c, r)))
            .await,
    )
}

/// GET /api/speedtest/progress
pub async fn progress(State(c): State<Compat>) -> Response {
    let (s, v) = c.speed.progress();
    reply(s, v)
}

/// POST /api/speedtest/stop
pub async fn stop(State(c): State<Compat>) -> Response {
    let (s, v) = c.speed.stop();
    reply(s, v)
}

/// Local LibreSpeed endpoints; saving does not run a measurement or contact any server.
pub async fn config(State(c): State<Compat>) -> Response {
    let (s, v) = c.speed.config();
    reply(s, v)
}
pub async fn configure(State(c): State<Compat>, body: Bytes) -> Response {
    answer(tokio::task::spawn_blocking(move || c.speed.configure(&body)).await)
}

/// `sh net-tune.sh <arg>` as a child process (never sourced), 5 s at most: (exit code,
/// stdout + stderr), or why it did not run (`script_missing`, `timeout`, `spawn_failed`).
async fn net_tune_sh(c: &Compat, arg: &str) -> Result<(i32, String), &'static str> {
    let script = c.path(NET_TUNE_SH);
    if !script.is_file() {
        return Err("script_missing");
    }
    let mut cmd = tokio::process::Command::new("sh");
    crate::command::die_with_parent(&mut cmd);
    cmd.arg(&script)
        .arg(arg)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let child = cmd.spawn().map_err(|_| "spawn_failed")?;
    // a timeout drops the child: kill_on_drop ends it
    let out = tokio::time::timeout(NET_TUNE_DEADLINE, child.wait_with_output())
        .await
        .map_err(|_| "timeout")?
        .map_err(|_| "spawn_failed")?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    if text.len() > NET_TUNE_OUTPUT_MAX {
        let mut cut = NET_TUNE_OUTPUT_MAX;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    Ok((out.status.code().unwrap_or(-1), text))
}

/// GET /api/net-tune: whether the tuning is wanted, and `net-tune.sh show`.
pub async fn net_tune_get(State(c): State<Compat>) -> Response {
    let on = c.path(NET_TUNE_WANT).exists();
    match net_tune_sh(&c, "show").await {
        Ok((code, output)) => reply(
            200,
            json!({"ok": true, "data": {"on": on, "exit": code, "output": output}}),
        ),
        Err(why) => reply(
            200,
            json!({"ok": true, "data": {"on": on, "exit": null, "output": null, "error": why}}),
        ),
    }
}

/// PUT /api/net-tune `{"on": bool}`: write (or delete) the want file, then
/// `net-tune.sh apply` (or `restore`); the reply carries the script's output.
pub async fn net_tune_put(State(c): State<Compat>, body: Bytes) -> Response {
    let Some(on) = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|v| v.get("on").and_then(Value::as_bool))
    else {
        return reply(400, json!({"ok": false, "error": "on (bool) required"}));
    };
    let want = c.path(NET_TUNE_WANT);
    let set = if on {
        std::fs::write(&want, b"1\n")
    } else {
        match std::fs::remove_file(&want) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            r => r,
        }
    };
    if let Err(e) = set {
        return reply(
            500,
            json!({"ok": false, "error": format!("want file: {e}"), "data": {"on": want.exists()}}),
        );
    }
    let arg = if on { "apply" } else { "restore" };
    match net_tune_sh(&c, arg).await {
        Ok((0, output)) => reply(
            200,
            json!({"ok": true, "data": {"on": on, "ran": arg, "exit": 0, "output": output}}),
        ),
        Ok((code, output)) => reply(
            500,
            json!({"ok": false, "error": format!("net-tune.sh {arg} exited {code}"),
                   "data": {"on": on, "ran": arg, "exit": code, "output": output}}),
        ),
        Err(why) => reply(
            if why == "timeout" { 504 } else { 503 },
            json!({"ok": false, "error": why, "data": {"on": on, "ran": arg, "exit": null, "output": null}}),
        ),
    }
}
