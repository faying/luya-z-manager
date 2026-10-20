//! `perf.slow_strong_signal` evidence (decision-execution-loop.md §4.11): the deep check the
//! person asked for (`/api/diagnose`'s own run, kept a while after it finishes) and the cell
//! serving now, turned into `luyaz_api::execution::slow_signal::Evidence`. Read-only: the replay
//! never carries an action, and nothing here starts a probe or a speed test.
//!
//! - Same cell: the run's `key.cell` against the `signal` block's cell now, by the rule the run's
//!   own key uses (`luyaz_netwatch::record`: `rat_of` the type, `nr_cell_id` on SA, `lte_cell_id`
//!   on LTE/NSA, none otherwise; 0 is no cell). Either unread = unknown.
//! - Layer levels as the run wrote them; an unrecognised level is left out (never counted).

use luyaz_api::diagnose::Level;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use luyaz_api::execution::slow_signal::{Busy, Evidence, replay};
use serde_json::{Value, json};

fn level(s: &str) -> Option<Level> {
    Some(match s {
        "ok" => Level::Ok,
        "warn" => Level::Warn,
        "bad" => Level::Bad,
        "na" => Level::Na,
        "info" => Level::Info,
        "blocked" => Level::Blocked,
        "pending" => Level::Pending,
        "running" => Level::Running,
        _ => return None,
    })
}

/// The serving cell now, from the `signal` block.
pub fn cell_now(signal: &Value) -> Option<i64> {
    let n = |k: &str| -> Option<i64> {
        match signal.get(k)? {
            Value::Number(x) => x.as_i64(),
            Value::String(x) => x.trim().parse().ok(),
            _ => None,
        }
        .filter(|c| *c != 0)
    };
    match luyaz_netwatch::record::rat_of(signal.get("type").and_then(Value::as_str).unwrap_or("")) {
        "SA" => n("nr_cell_id"),
        "LTE" | "NSA" => n("lte_cell_id"),
        _ => None,
    }
}

/// `run`: the deep check's `data` as `/api/diagnose` gives it; `cell`: the cell serving now.
pub fn evidence(run: &Value, cell: Option<i64>) -> Evidence {
    let done = match run.get("state").and_then(Value::as_str) {
        Some("done") => Some(true),
        Some("running" | "waiting") => Some(false),
        _ => None,
    };
    let layers = run
        .get("layers")
        .and_then(Value::as_array)
        .map(|ls| {
            ls.iter()
                .filter_map(|l| {
                    Some((
                        l.get("id")?.as_str()?.to_string(),
                        level(l.get("level")?.as_str()?)?,
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let then = run.pointer("/key/cell").and_then(Value::as_i64);
    Evidence {
        done,
        age_s: run.get("age_s").and_then(Value::as_u64),
        same_cell: then.zip(cell).map(|(a, b)| a == b),
        layers,
        busy_s: None,
    }
}

/// The passive run (`Busy`), fed by the watcher's sampler every 5 s.
static BUSY: Mutex<Busy> = Mutex::new(Busy::new());

fn mono() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs()
}

/// One sampler round: the live reading's signal level (the deep check's own rule) and downlink.
pub fn observe(state: Option<&Value>, screen: Option<&Value>) {
    let (radio, ..) = luyaz_netwatch::diagnose::radio_from(state, screen);
    let level = luyaz_api::diagnose::judge_signal(&radio).level;
    let rx = state.is_some().then_some(radio.rx_bps);
    let mut b = BUSY.lock().unwrap_or_else(|e| e.into_inner());
    b.observe(mono(), Some(level), rx);
}

/// `/api/diagnose`'s `slow_signal`.
pub fn view(run: &Value, hub: Option<&crate::block::Hub>) -> Value {
    let cell = hub
        .and_then(|h| h.view("signal"))
        .filter(|v| !v.stale)
        .and_then(|v| cell_now(&v.data));
    let mut e = evidence(run, cell);
    e.busy_s = BUSY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .seconds(mono());
    json!({ "advice": replay(&e), "busy_s": e.busy_s })
}
