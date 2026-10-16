//! What the routes and the shared pages show of the engine: the full state (`GET /api/scenario`),
//! the summary on the public status and the picker on the network page. The wording is u60-api's
//! (`luyaz_api::scenario`), shared with u60d's touch layer.

use serde_json::{json, Value};
use luyaz_policy::scenario::{self as sc, Config, Detect, Scenario};

use crate::{lk, Engine, ALLOWED_PATHS};

/// English names of the factory scenarios that still have their factory name.
pub fn names_en(cfg: &Config) -> Value {
    Value::Object(
        cfg.scenarios
            .iter()
            .filter_map(|s| luyaz_api::scenario::name_en(&s.id, &s.name).map(|n| (s.id.clone(), json!(n))))
            .collect(),
    )
}

pub(crate) fn when_view(d: &Detect) -> luyaz_api::scenario::When<'_> {
    use luyaz_api::scenario::When;
    match d {
        Detect::Ssid { entries } => When::Ssid(entries.iter().map(|e| e.ssid.as_str()).collect()),
        Detect::Mcc { mccs } => When::Mcc(mccs),
        Detect::Abroad => When::Abroad,
        Detect::Fallback => When::Fallback,
    }
}

/// The picker (scenario list with what each one does, in both languages) for these scenarios.
pub fn picker(enabled: bool, current: &str, pin: Option<String>, guard_takeover: bool, cfg: &Config) -> Value {
    // the bodies as serde_json, alive while the views borrow them
    let bodies: Vec<Vec<Option<Value>>> = cfg
        .scenarios
        .iter()
        .map(|s| s.actions.iter().map(|a| a.action.body.as_ref().map(Value::from)).collect())
        .collect();
    let scenes: Vec<luyaz_api::scenario::Scene> = cfg
        .scenarios
        .iter()
        .zip(&bodies)
        .map(|(s, b): (&Scenario, &Vec<Option<Value>>)| luyaz_api::scenario::Scene {
            id: &s.id,
            name: &s.name,
            inhibit_sleep: s.inhibit_sleep,
            abroad: sc::is_abroad(s),
            when: when_view(&s.detect),
            does: s
                .actions
                .iter()
                .zip(b)
                .map(|(a, body)| luyaz_api::scenario::Does { path: &a.action.path, body: body.as_ref() })
                .collect(),
        })
        .collect();
    luyaz_api::scenario::picker(enabled, current, pin, guard_takeover, &scenes, &cfg.home_mcc)
}

impl Engine {
    /// `GET /api/scenario`'s data.
    pub fn state_json(&self) -> Value {
        let sim_mcc = self.io.sim_mcc();
        let enabled = self.enabled();
        let pin = self.pin();
        let takeover = self.takeover_marked();
        let cfg = lk(&self.cfg);
        let st = lk(&self.state);
        json!({
            "enabled": enabled,
            "pin": pin,
            "guard_takeover": takeover,
            "current": st.current,
            "candidate": st.candidate,
            "hits": st.hits,
            "misses": st.misses,
            "applied": st.applied,
            "last_switch": st.last_switch,
            "last_scan": st.last_scan,
            "last_error": st.last_error,
            "config": &*cfg,
            "names_en": names_en(&cfg),
            "sim_mcc": sim_mcc,
            "pending_restore": self.pending()
                .iter()
                .map(|p| json!({"key": p.key, "body": p.action.action.body.as_ref().map(Value::from), "saved_at": p.saved_at}))
                .collect::<Vec<_>>(),
            "allowed_paths": ALLOWED_PATHS,
        })
    }

    /// The scenario part of the public status.
    pub fn public_summary(&self) -> Value {
        let enabled = self.enabled();
        let pin = self.pin();
        let takeover = self.takeover_marked();
        let cfg = lk(&self.cfg);
        let st = lk(&self.state);
        let scen = cfg.scenario(&st.current);
        let e = luyaz_api::scenario::Engine {
            configured: !cfg.scenarios.is_empty(),
            enabled,
            current: &st.current,
            pin,
            guard_takeover: takeover,
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
        v
    }

    /// The picker, or `None` while a tick or a route holds the engine (the network page then
    /// leaves it out rather than wait).
    pub fn picker_try(&self) -> Option<Value> {
        let enabled = self.enabled();
        let pin = self.pin();
        let takeover = self.takeover_marked();
        let cfg = self.cfg.try_lock().ok()?;
        let st = self.state.try_lock().ok()?;
        Some(picker(enabled, &st.current, pin, takeover, &cfg))
    }
}

#[cfg(test)]
#[path = "views_tests.rs"]
mod tests;
