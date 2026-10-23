//! `GET /v2/recovery/actions` (R18-P1-07): what each recovery/change action does, read from the
//! core's own tables, never written by hand: the action table (`ops::spec::SPECS` +
//! `LOOP_ONLY`), the decision loop's allow-list (`api::execute::ALLOW`), its missing entry
//! conditions, and the agent whitelist (`ops::agent::layer_of`). The MCP tool
//! `explain_recovery_action` reads this through `/agent-api/recovery-actions`.
//!
//! Static: no device read, no probe, no state.

use serde_json::{Value, json};

use crate::ops::agent;
use crate::ops::spec::{self, CompletionEvidence, RollbackCapability, Spec};

pub const SCHEMA_VERSION: u32 = 1;

fn rollback_word(c: RollbackCapability) -> &'static str {
    match c {
        RollbackCapability::CapturedAndProven => "captured_and_proven",
        RollbackCapability::Unsupported => "unsupported",
        RollbackCapability::Unknown => "unknown",
    }
}

fn evidence_word(e: CompletionEvidence) -> &'static str {
    match e {
        CompletionEvidence::ReadbackRegistered => "readback_and_registered",
        CompletionEvidence::ReadbackNewConn => "readback_and_new_connection",
        CompletionEvidence::ReadbackChill => "readback_and_chill_check",
        CompletionEvidence::ApReadbackOnly => "ap_readback_only",
    }
}

fn confirm_word(s: &Spec) -> Value {
    serde_json::to_value(s.confirm).unwrap_or(Value::Null)
}

/// Who may start or approve this action, from the code's gates.
fn approval(s: &Spec, loop_only: bool) -> Value {
    let allow = crate::api::execute::ALLOW.contains(&s.action);
    let agent = if loop_only {
        None
    } else {
        agent::layer_of(s.action)
    };
    json!({
        // loop-only actions are not on /control: a person approves the loop's proposal (loop.approve)
        "person": if loop_only { "approves the decision loop's proposal (screen or web, loop.approve)" } else { "screen or web, through /control" },
        "agent_mcp": agent.map(|l| json!({
            "layer": l.as_str(),
            "per_layer_limit_s": agent::LIMIT_SECS,
            "sensitive_check": matches!(l, agent::Layer::Apn | agent::Layer::Link),
            // D-g: only on the owner's instruction; the trial also needs the device's own Tailscale
            "owner_instruction_only": l == agent::Layer::Link,
            "tailnet_check": l == agent::Layer::Link,
        })),
        "auto_allow_list": allow,
    })
}

fn spec_entry(s: &Spec, loop_only: bool, person_rollback: bool) -> Value {
    let cap = s.rollback_capability;
    json!({
        "action": s.action,
        "kind": if loop_only { "loop_only" } else { "transaction" },
        "item": s.item,
        "confirm": confirm_word(s),
        "deadline_s": s.deadline_ms() / 1000,
        "completion_evidence": evidence_word(s.completion_evidence),
        "completion_note": "proves the write took effect, not that the original fault is fixed",
        "rollback": {
            "capability": rollback_word(cap),
            // engine.rs: a loop-started write rolls back only when the action's capability is proven
            "on_loop_execution": s.auto_rollback_allowed(),
            // engine.rs: an agent-approved write always carries rollback (phase 2 D4)
            "on_agent_write": !loop_only && agent::layer_of(s.action).is_some(),
            // engine.rs: a person's write rolls back on the deadline only with LUYAZ_ROLLBACK=1
            "on_person_write": !loop_only && person_rollback,
        },
        "approval": approval(s, loop_only),
    })
}

/// Actions on transactions, and the composite restore: not in the action table, their rules
/// are in `server/write.rs` and `verified.rs`.
fn other_entries() -> Vec<Value> {
    let person_only = json!({"person": "screen or web, through /control", "agent_mcp": null, "auto_allow_list": false});
    vec![
        json!({
            "action": "verified.restore",
            "kind": "composite",
            "what": "puts back the last verified connection's settings for this card and country: one ordinary transaction per differing item, in order; stops at the first that does not confirm; one O2 recheck at the end",
            "rollback": {"capability": "per_step", "note": "each step has its own action's rollback; GET /api/verified lists the steps with theirs"},
            "approval": person_only,
        }),
        json!({
            "action": "op.revert",
            "kind": "transaction_control",
            "what": "undoes a transaction: writes back the old value it captured",
            "rollback": {"capability": "not_applicable"},
            "approval": person_only,
        }),
        json!({
            "action": "op.receipt",
            "kind": "transaction_control",
            "what": "confirms a Wi-Fi change was seen from a client (its receipt), for a transaction that asked for one",
            "rollback": {"capability": "not_applicable"},
            "approval": person_only,
        }),
        json!({
            "action": "op.keep",
            "kind": "transaction_control",
            "what": "ends a trial and keeps the new value (no automatic rollback after this)",
            "rollback": {"capability": "not_applicable"},
            "approval": person_only,
        }),
    ]
}

/// `/control` writes that are not in the action table: one direct call, no transaction.
fn direct_entry(action: &str) -> Value {
    let agent = agent::layer_of(action);
    json!({
        "action": action,
        "kind": "direct",
        "what": "one direct write through /control (control::execute): no transaction, no captured old value",
        "rollback": {"capability": "none", "on_loop_execution": false, "on_agent_write": false, "on_person_write": false},
        "approval": {
            "person": "screen or web, through /control",
            "agent_mcp": agent.map(|l| json!({
                "layer": l.as_str(),
                "per_layer_limit_s": agent::LIMIT_SECS,
                "sensitive_check": matches!(l, agent::Layer::Apn | agent::Layer::Link),
            })),
            "auto_allow_list": crate::api::execute::ALLOW.contains(&action),
        },
    })
}

const LIMITATIONS: &[&str] = &[
    "the admin web's confirm tiers (direct / inline / dialog) are set per page, not per action: not in this table",
    "completion evidence proves the write took effect, not that the original fault is fixed",
    "rollback on a person's write follows the device setting LUYAZ_ROLLBACK (person_rollback_enabled)",
];

pub fn catalog(person_rollback: bool) -> Value {
    let mut actions: Vec<Value> = spec::SPECS
        .iter()
        .map(|s| spec_entry(s, false, person_rollback))
        .collect();
    actions.extend(
        spec::LOOP_ONLY
            .iter()
            .map(|s| spec_entry(s, true, person_rollback)),
    );
    actions.extend(other_entries());
    let described: Vec<String> = actions
        .iter()
        .filter_map(|a| a["action"].as_str().map(str::to_owned))
        .collect();
    actions.extend(
        crate::control::ACTIONS
            .iter()
            .chain(crate::control::E4_ACTIONS)
            .filter(|a| !described.iter().any(|d| d == *a))
            .map(|a| direct_entry(a)),
    );
    let allow = crate::api::execute::ALLOW;
    json!({
        "schema_version": SCHEMA_VERSION,
        "core_version": env!("CARGO_PKG_VERSION"),
        "allow_list": {
            "actions": allow,
            "empty": allow.is_empty(),
            "note": if allow.is_empty() {
                "empty: no action runs on an approval that is not a person's; the owner adds each one"
            } else {
                "actions a non-person approval may run"
            },
        },
        "loop_missing_conditions": crate::api::execute::missing(),
        "person_rollback_enabled": person_rollback,
        "actions": actions,
        "limitations": LIMITATIONS,
    })
}

pub async fn actions() -> axum::Json<Value> {
    axum::Json(catalog(
        std::env::var("LUYAZ_ROLLBACK").is_ok_and(|v| v == "1"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_table_action_is_described_once() {
        let c = catalog(false);
        let list = c["actions"].as_array().unwrap();
        for s in spec::SPECS.iter().chain(spec::LOOP_ONLY) {
            assert_eq!(
                list.iter().filter(|a| a["action"] == s.action).count(),
                1,
                "{}",
                s.action
            );
        }
        for a in crate::control::ACTIONS
            .iter()
            .chain(crate::control::E4_ACTIONS)
        {
            assert_eq!(list.iter().filter(|x| x["action"] == *a).count(), 1, "{a}");
        }
        let d = list.iter().find(|a| a["action"] == "apn.delete").unwrap();
        assert_eq!(d["kind"], "direct");
        assert_eq!(d["approval"]["agent_mcp"]["layer"], "apn");
        assert_eq!(d["rollback"]["capability"], "none");
        let r = list
            .iter()
            .find(|a| a["action"] == "device.reboot")
            .unwrap();
        assert_eq!(r["approval"]["agent_mcp"], Value::Null);
    }

    #[test]
    fn allow_list_is_reported_empty() {
        let c = catalog(false);
        assert_eq!(c["allow_list"]["empty"], true);
        assert_eq!(c["allow_list"]["actions"], json!([]));
        assert!(
            c["actions"]
                .as_array()
                .unwrap()
                .iter()
                .all(|a| a["approval"]["auto_allow_list"] == false)
        );
    }

    #[test]
    fn reconnect_is_loop_only_and_never_rolls_back() {
        let c = catalog(true);
        let r = c["actions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["action"] == "data.reconnect")
            .unwrap()
            .clone();
        assert_eq!(r["kind"], "loop_only");
        assert_eq!(r["rollback"]["capability"], "unsupported");
        assert_eq!(r["rollback"]["on_loop_execution"], false);
        assert_eq!(r["rollback"]["on_person_write"], false);
        assert_eq!(r["approval"]["agent_mcp"], Value::Null);
    }

    #[test]
    fn agent_layers_and_rollback_follow_the_tables() {
        let c = catalog(false);
        let find = |n: &str| {
            c["actions"]
                .as_array()
                .unwrap()
                .iter()
                .find(|a| a["action"] == n)
                .unwrap()
                .clone()
        };
        let apn = find("apn.use");
        assert_eq!(apn["approval"]["agent_mcp"]["layer"], "apn");
        assert_eq!(apn["approval"]["agent_mcp"]["sensitive_check"], true);
        assert_eq!(apn["rollback"]["on_agent_write"], true);
        let wifi = find("wifi.settings");
        assert_eq!(wifi["approval"]["agent_mcp"], Value::Null);
        assert_eq!(wifi["completion_evidence"], "ap_readback_only");
        assert_eq!(wifi["rollback"]["on_person_write"], false);
        let band = find("band.set_lte");
        assert_eq!(band["rollback"]["capability"], "captured_and_proven");
        assert_eq!(band["rollback"]["on_loop_execution"], true);
        assert_eq!(
            find("verified.restore")["approval"]["agent_mcp"],
            Value::Null
        );
    }
}
