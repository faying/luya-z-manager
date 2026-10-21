//! The health check (u60-platform.md Phase 4 step 9d; the agent's until Phase 10).
//!
//! With u60d's writes on it runs the doctor itself (u60-api `health::Runner`): 20 s after start,
//! then every 60 s, and again in the background after alerts are marked read or the alert SMS
//! number is set; `GET /api/health?refresh=1` runs it and answers. It writes the snapshot file
//! `/api/health` and the public status read.

use axum::{
    extract::{RawQuery, State},
    response::Response,
};
use serde_json::json;
use std::sync::Arc;
use luyaz_api::health::{self as h, Runner, Snapshot};

use super::{Compat, reply};

/// How often a due run is looked for.
const LOOK_SECS: u64 = 1;

pub(super) fn runner(root: &str) -> Arc<Runner> {
    let r = Runner::new(
        &format!("{root}{}", h::DOCTOR),
        format!("{root}{}", h::SNAPSHOT_FILE),
        h::DOCTOR_TIMEOUT,
    );
    // X11: on the device the core runs its own `doctor` (this very program); under a test root
    // (and in unit tests, where /proc/self/exe is the test harness) only the script
    if root.is_empty() && !cfg!(test) {
        Arc::new(r.with_core("/proc/self/exe"))
    } else {
        Arc::new(r)
    }
}

/// The background runs: the first [`h::FIRST_WAIT`] after start (until then the last snapshot
/// stands), then every [`h::EVERY`].
pub async fn run(c: Compat) {
    let mut look = tokio::time::interval(c.clock.secs(LOOK_SECS));
    c.health.seed(Snapshot::read(&c.path(h::SNAPSHOT_FILE)));
    let mut due = tokio::time::Instant::now() + scaled(&c, h::FIRST_WAIT);
    loop {
        look.tick().await;
        if tokio::time::Instant::now() >= due {
            let r = c.health.clone();
            let _ = tokio::task::spawn_blocking(move || r.refresh()).await;
            due = tokio::time::Instant::now() + scaled(&c, h::EVERY);
        }
    }
}

fn scaled(c: &Compat, d: std::time::Duration) -> std::time::Duration {
    c.clock.secs(d.as_secs())
}

/// After something that changes the check's answer (alerts read): one more run in the background,
/// with the writes on.
pub(super) fn refresh_soon(c: &Compat) {
    if c.writes {
        c.health.refresh_soon();
    }
}

/// GET /api/health[?refresh=1]: the snapshot file; `refresh=1` runs the doctor first (up to 20 s)
/// with the writes on.
pub async fn get(State(c): State<Compat>, RawQuery(q): RawQuery) -> Response {
    let refresh = q
        .as_deref()
        .unwrap_or("")
        .split('&')
        .any(|kv| kv == "refresh=1");
    let data = tokio::task::spawn_blocking(move || {
        if refresh && c.writes {
            c.health.refresh();
        }
        let s = Snapshot::read(&c.path(h::SNAPSHOT_FILE));
        let mut v = h::reply(&s, h::crash_logs(&c.path(h::CRASH_DIR)));
        // the read-only scenarios worth a word (touch health page); not checks: they never
        // count as a warn or raise an alert
        let notices: Vec<serde_json::Value> = [
            super::hot_drain::view(),
            super::long_idle::view(c.src.hub.as_deref()),
        ]
        .into_iter()
        .filter_map(|v| v.get("say").filter(|s| !s.is_null()).cloned())
        .collect();
        if let Some(o) = v.as_object_mut() {
            o.insert("notices".into(), notices.into());
        }
        v
    })
    .await
    .unwrap_or(serde_json::Value::Null);
    reply(200, json!({"ok": true, "data": data}))
}

/// The most of one crash log sent back (same as the agent).
const CRASHLOG_MAX: u64 = 256 * 1024;

/// GET /api/health/crashlog?program=<p>&file=<f> (5a, the agent's route and reply): one crash log,
/// read whoever owns the check (it only reads a file).
pub async fn crashlog(State(c): State<Compat>, RawQuery(q): RawQuery) -> Response {
    let q = q.unwrap_or_default();
    let get = |k: &str| {
        q.split('&').find_map(|kv| {
            kv.strip_prefix(k)
                .and_then(|v| v.strip_prefix('='))
                .map(str::to_string)
        })
    };
    let (Some(prog), Some(file)) = (get("program"), get("file")) else {
        return reply(
            400,
            json!({"ok": false, "error": "program and file are required"}),
        );
    };
    if !h::safe_name(&prog) || !h::safe_name(&file) || !file.ends_with(".log") {
        return reply(400, json!({"ok": false, "error": "bad name"}));
    }
    let path = c.path(h::CRASH_DIR).join(&prog).join(&file);
    let text = tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let f = std::fs::File::open(path).ok()?;
        let mut text = String::new();
        let _ = f.take(CRASHLOG_MAX).read_to_string(&mut text);
        Some(text)
    })
    .await
    .ok()
    .flatten();
    match text {
        Some(text) => reply(
            200,
            json!({"ok": true, "data": {"program": prog, "file": file, "text": text}}),
        ),
        None => reply(404, json!({"ok": false, "error": "no such crash log"})),
    }
}
