//! `/api/public/status` and `/api/health` for the touch screen (Phase 4 step 8b).
//!
//! public/status carries only what touch-ui reads (scenario.c): `alerts.unread`,
//! `health.{checked,bad,warn}`, `services.chill` (on, the manual-first notice, the home exit's line),
//! `scenario`, plus `clock.utc_offset`; and `services.tailscale` for luyaz-web's top bar (8d-1c).
//! Network, Wi-Fi, battery, SMS and device are the admin web's, which stays on the agent.
//!
//! Where each piece comes from while the agent still runs everything (the shadow period):
//! - health: the doctor snapshot (`/tmp/health.json`), written by u60d's check (9d:
//!   `compat/health.rs`)
//! - manual-first: u60d's own replay of the agent's records; before any record, the agent's file
//! - home exit: on/off and rest from the agent's file (requests change them without a record),
//!   the probe results from u60d's replay; `busy` is null until the PUT moves here (8e)
//! - scenario: the engine's files (config, state.json, pin, disabled, restore.json, the guard's
//!   takeover flag), which the agent writes on every switch

use axum::{extract::State, response::Response};
use luyaz_api::{alerts::Alerts, health::Snapshot};
use luyaz_policy::scenario as sc;
use serde_json::{Value, json};

use super::{Compat, reply};

/// GET /api/public/status (no login).
pub async fn public_status(State(c): State<Compat>) -> Response {
    let offset = super::utc_offset().await;
    let data = tokio::task::spawn_blocking(move || {
        let mut d = gather(&c);
        d["clock"] = json!({"utc_offset": offset});
        d
    })
    .await
    .unwrap_or(Value::Null);
    reply(200, json!({"ok": true, "data": data}))
}

fn read<T: serde::de::DeserializeOwned>(c: &Compat, p: &str) -> Option<T> {
    serde_json::from_str(&std::fs::read_to_string(c.path(p)).ok()?).ok()
}

fn exists(c: &Compat, p: &str) -> bool {
    c.path(p).exists()
}

/// Everything but the clock (blocking: files and the shadow's lock).
pub(super) fn gather(c: &Compat) -> Value {
    let health = Snapshot::read(&c.path(luyaz_api::health::SNAPSHOT_FILE));
    #[allow(unused_mut)]
    let mut data = json!({
        "alerts": {"unread": Alerts::at(&c.alerts_dir()).unread()},
        "health": luyaz_api::health::public_summary(&health),
        "scenario": scenario(c),
        // netwatch's home verdict while u60d owns it (9b; the agent shows this one then)
        "network": {"verdict": super::watch::verdict(c)},
    });
    // T6: the open "sure" finding for the home page (null when none)
    data["network"]["finding"] = super::finding::home_now().map_or(Value::Null, |h| h.to_value());
    data["network"]["health"] =
        luyaz_api::execution::network_health::build(&super::underlay::health_input(&c.root));
    data["services"] = json!({"tailscale": tailscale(c)});
    data["services"]["chill"] = chill(c);
    data
}

/// The agent's `services.tailscale` (8d-1c; luyaz-web's top bar reads `installed`/`running`): the
/// CLI is there; a `tailscaled` process runs (the agent's `pidof`, as a /proc scan).
fn tailscale(c: &Compat) -> Value {
    let running = std::fs::read_dir(c.path("/proc")).is_ok_and(|d| {
        d.flatten().any(|e| {
            e.file_name()
                .to_string_lossy()
                .bytes()
                .all(|b| b.is_ascii_digit())
                && std::fs::read_to_string(e.path().join("comm"))
                    .is_ok_and(|s| s.trim_end() == "tailscaled")
        })
    });
    json!({"installed": exists(c, "/data/tailscale/tailscale"), "running": running})
}

pub(super) const SCENARIO_CONFIG: &str = "/data/scenario/scenarios.json";
pub(super) const SCENARIO_STATE: &str = "/data/scenario/state.json";
pub(super) const SCENARIO_PIN: &str = "/data/scenario/pin";
pub(super) const SCENARIO_DISABLED: &str = "/data/scenario/disabled";
const SCENARIO_RESTORE: &str = "/data/scenario/restore.json";
pub(super) const TAKEOVER_FILE: &str = "/tmp/luyaz/wifiguard.took-over";

/// The agent's `scenario::public_summary`, from the engine's files.
fn scenario(c: &Compat) -> Value {
    let cfg: sc::Config = read(c, SCENARIO_CONFIG).unwrap_or_default();
    let st: sc::RunState = read(c, SCENARIO_STATE).unwrap_or_default();
    let pin = std::fs::read_to_string(c.path(SCENARIO_PIN))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let scen = cfg.scenario(&st.current);
    let e = luyaz_api::scenario::Engine {
        configured: !cfg.scenarios.is_empty(),
        enabled: !exists(c, SCENARIO_DISABLED),
        current: &st.current,
        pin,
        guard_takeover: exists(c, TAKEOVER_FILE),
        last_switch: st.last_switch,
    };
    let current = scen.map(|s| luyaz_api::scenario::Current {
        id: &s.id,
        name: &s.name,
        inhibit_sleep: s.inhibit_sleep,
        abroad: sc::is_abroad(s),
    });
    #[allow(unused_mut)]
    let mut v = luyaz_api::scenario::public_summary(&e, current);
    let restores: Vec<sc::PendingRestore> = read(c, SCENARIO_RESTORE).unwrap_or_default();
    luyaz_api::chill::scenario_fields(
        &mut v,
        restores
            .iter()
            .any(|p| p.key == luyaz_api::chill::CHILL_ON_KEY),
        scen.is_some_and(sc::is_abroad),
        scen.iter().flat_map(|s| s.actions.iter()).map(|a| {
            let member = a
                .action
                .body
                .as_ref()
                .and_then(|b| b.get("member"))
                .and_then(|m| m.as_str());
            (a.action.path.as_str(), member)
        }),
    );
    v
}

/// `services.chill`: CHILL's state file and switch, the manual-first notice, the home exit's line.
fn chill(c: &Compat) -> Value {
    use luyaz_api::chill as ch;
    let state: Option<Value> = read(c, ch::CHILL_STATE);
    let (st, reason) = ch::state_and_reason(state.as_ref());
    ch::public(
        &st,
        reason.as_deref(),
        !exists(c, ch::CHILL_DISABLED),
        &manual_first_summary(c),
        home_exit_summary(c),
    )
}

/// The manual-first summary: the live engine's when u60d runs it (9g), else its state file.
pub(super) fn manual_first_summary(c: &Compat) -> Value {
    use luyaz_api::chill as ch;
    match c.manual_first.live() {
        Some(live) => live.summary(),
        None => {
            let mf: luyaz_policy::manual_first::Persist =
                read(c, ch::MANUAL_FIRST_STATE).unwrap_or_default();
            ch::manual_first_summary(
                &ch::ManualFirst {
                    backup: &mf.backup,
                    manual: &mf.manual,
                    notice: &mf.notice,
                    notice_en: &mf.notice_en,
                    notice_at: mf.notice_at,
                    notice_kind: &mf.notice_kind,
                },
                super::home::now_unix(),
            )
        }
    }
}

/// The home exit's line: u60d's engine when it owns it, else the agent's from its file.
pub(super) fn home_exit_summary(c: &Compat) -> Value {
    match c.home_exit.as_ref().filter(|e| e.owner()) {
        Some(e) => e.summary(),
        None => luyaz_api::chill::home_summary(&super::home::machine(c)),
    }
}
