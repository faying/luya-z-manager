//! `usage.long_idle` evidence (decision-execution-loop.md §4.11): the same closed ledger hours as
//! `hot_drain` (one parse, one cache) plus a few reads at request time, turned into
//! `luyaz_api::execution::long_idle::Evidence` for `/api/diagnose`. Read-only: the replay never
//! carries an action.
//!
//! - Per hour: covered = the `hour` line's `g_core` 0 (the core never away, so none of its
//!   readings are missing; the crash watcher's routes don't matter here) and awake + asleep
//!   within [`SLACK_S`] of the span; adjacent = it starts where this boot's hour before it ended; screen on = any
//!   `on_*_s`; clients and traffic only for screen-off-at-home hours (`off_home_cli_s`, which
//!   counts unknown as clients; `off_home_wan_pkts` / `quiet_pm`), a roaming hour is unknown.
//! - Remote management (Tailscale, Hermes, a web session) is not read on its own: it rides the
//!   cellular link, so the traffic bound is its upper bound. Calibrated 10-20 on the device's own
//!   client-free nights: 600–8 500 packets an hour and 40–57 % quiet with the 30-minute night
//!   Hermes poll, against 100 k+ and under 30 % with clients.
//! - Now: our transaction (`op.active`), the SMS list readable, the sentinel's heartbeat at most
//!   [`HB_STALE_S`] old, CHILL meant on, and whether the 6e idle pause is on.

use luyaz_api::execution::long_idle::{
    Evidence, Hour, MAX_PKTS, MIN_HOURS, MIN_QUIET_PM, quiet, replay,
};
use serde_json::{Value, json};

use super::hot_drain::{FRESH_S, Row};
use crate::engines::sentinel_watch::{HB_FILE, HB_STALE_S, mono_s, parse_hb};

/// awake + asleep may be off from the span by this much (the fold's last check).
pub const SLACK_S: i64 = 60;
/// Quiet hours counted back at most (for the words only).
const RUN_MAX: usize = 24 * 7;

fn hour(r: &Row, prev: Option<&Row>) -> Hour {
    let span = r.to - r.from;
    let home = r.cell_s[1] == 0 && r.cell_s[3] == 0;
    Hour {
        covered: r
            .ledger_s
            .zip(r.ledger_core_gap)
            .map(|(s, g)| g == 0 && (s - span).abs() <= SLACK_S),
        adjacent: Some(prev.is_some_and(|p| p.power && p.boot == r.boot && p.to == r.from)),
        charging: Some(r.chg_s > 0),
        screen_on: Some(r.cell_s[0] + r.cell_s[1] > 0),
        no_clients: home.then_some(r.off_home_cli_s).flatten().map(|s| s == 0),
        low_traffic: home
            .then(|| Some(r.wan_pkts[0]? <= MAX_PKTS && r.quiet_pm[0]? >= MIN_QUIET_PM))
            .flatten(),
    }
}

/// What is read at request time.
#[derive(Clone, Debug, Default)]
pub struct Now {
    pub txn_running: Option<bool>,
    pub sms_known: Option<bool>,
    pub watchdog_ok: Option<bool>,
    pub chill_on: Option<bool>,
    pub idle_pause_on: Option<bool>,
}

pub fn evidence(rows: &[Row], boot8: &str, now_up: i64, now: &Now) -> Evidence {
    let pw: Vec<&Row> = rows.iter().filter(|r| r.power).collect();
    let hours: Vec<Hour> = pw
        .iter()
        .enumerate()
        .map(|(n, r)| hour(r, n.checked_sub(1).map(|p| pw[p])))
        .collect();
    let fresh = pw
        .last()
        .map(|w| w.boot == boot8 && (0..=FRESH_S).contains(&(now_up - w.to)));
    // quiet hours back from the last, as long as each chains to the one before
    let mut run = 0;
    for h in hours.iter().rev().take(RUN_MAX) {
        if quiet(h).is_err() {
            break;
        }
        run += 1;
        if h.adjacent != Some(true) {
            break;
        }
    }
    let keep = hours.len().saturating_sub(MIN_HOURS);
    Evidence {
        fresh,
        hours: hours[keep..].to_vec(),
        quiet_run: run,
        txn_running: now.txn_running,
        sms_known: now.sms_known,
        watchdog_ok: now.watchdog_ok,
        chill_on: now.chill_on,
        idle_pause_on: now.idle_pause_on,
    }
}

fn now(hub: Option<&crate::block::Hub>) -> Now {
    let view = |name: &str| hub.and_then(|h| h.view(name));
    let hb = std::fs::read_to_string(crate::state::host_path(HB_FILE))
        .ok()
        .and_then(|t| parse_hb(&t));
    Now {
        txn_running: view("op")
            .filter(|v| !v.stale)
            .map(|v| !v.data.get("active").is_none_or(Value::is_null)),
        sms_known: view("sms_list").map(|v| !v.stale),
        watchdog_ok: hb.map(|m| mono_s() - m <= HB_STALE_S),
        chill_on: Some(crate::engines::chill_idle::meant_on()),
        idle_pause_on: Some(crate::engines::chill_idle::enabled()),
    }
}

/// `/api/diagnose`'s `long_idle`: the replay's answer and the hours it saw.
pub fn view(hub: Option<&crate::block::Hub>) -> Value {
    let rows = super::hot_drain::ledger();
    let up = (super::band_flap::boot_ms() / 1000) as i64;
    let e = evidence(&rows, &super::hot_drain::boot8(), up, &now(hub));
    json!({
        "advice": replay(&e),
        "quiet_run": e.quiet_run,
        "min_hours": MIN_HOURS,
    })
}
