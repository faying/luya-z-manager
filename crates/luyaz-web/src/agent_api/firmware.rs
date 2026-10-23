//! R18-P1-07: two more reads for the MCP gateway, both from what the core already has.
//!
//! - `/agent-api/capabilities[?capability=<prefix>]`: the core's passive firmware capability
//!   report (`/v2/device/capabilities`, firmware-capabilities.md §7), cut to a field whitelist.
//!   Reading it never starts a probe. A report this side cannot decode (another schema, or an
//!   old core without the route) is `unverified`, never "supported" or an empty list.
//! - `/agent-api/recovery-actions`: the core's action table (`/v2/recovery/actions`: rollback
//!   class, completion evidence, who may approve, the loop's allow-list). An old core without it
//!   is `not_described`.
//!
//! No raw probe values or call arguments leave the device (an APN or address can be in them):
//! evidence is its id, verification, summary and limitations only.

use axum::extract::{Query, Request, State};
use axum::response::Response;
use luyaz_api::firmware::CapabilityReport;
use serde_json::{Value, json};

use super::{REPLY_MAX, admit, core_down, log, pick, read, reply, untrusted};
use crate::pages::Shared;

const LIMITATIONS: &[&str] = &[
    "passive report: reading it ran no new probe",
    "supported is not the same as a verified write",
    "unknown is not unsupported: the device has no evidence either way",
    "after a core restart live assessments stay unknown until new passive evidence",
    "context_coverage.authoritative=false: the context epochs are not authoritative, nothing is auto-approved from them",
];

fn assessment(a: &Value) -> Value {
    json!({
        "capability_id": a["capability_id"],
        "status": a["status"],
        "reason": a["reason"],
        "availability": pick(&a["availability"], &["state", "reason"]),
        "freshness": pick(&a["freshness"], &["fresh", "age_seconds", "max_age_seconds"]),
        "validation": a["validation"],
        "selected_dialect": a["selected_dialect"],
        "evidence_ids": a["evidence_ids"],
        "contradicting_evidence_ids": a["contradicting_evidence_ids"],
    })
}

fn evidence(e: &Value) -> Value {
    let lim = e["limitations"]
        .as_array()
        .map(|l| l.iter().take(4).map(untrusted).collect::<Vec<_>>())
        .unwrap_or_default();
    json!({
        "id": e["id"],
        "capability_id": e["capability_id"],
        "verification": e["verification"],
        "source_kind": e["source_kind"],
        "recorded_on": e["recorded_on"],
        "summary": untrusted(&e["summary"]),
        "limitations": lim,
    })
}

/// The report for the agent; `prefix` keeps only capabilities starting with it. Over the reply
/// cap: evidence goes first, then assessments from the end, and `truncated` says so.
pub fn shape_capabilities(raw: &Value, prefix: Option<&str>) -> Value {
    let report = serde_json::to_string(raw)
        .map_err(|e| e.to_string())
        .and_then(|t| CapabilityReport::decode(&t));
    if let Err(why) = report {
        return json!({
            "state": "unverified",
            "why": "the core's capability report could not be read on this side",
            "detail": untrusted(&json!(why)),
            "schema_version": raw["schema_version"],
        });
    }
    let keep = |id: &Value| prefix.is_none_or(|p| id.as_str().is_some_and(|s| s.starts_with(p)));
    let all = raw["assessments"].as_array().cloned().unwrap_or_default();
    let mut assessments: Vec<Value> = all
        .iter()
        .filter(|a| keep(&a["capability_id"]))
        .map(assessment)
        .collect();
    let mut evs: Vec<Value> = raw["evidence"]
        .as_array()
        .map(|e| {
            e.iter()
                .filter(|e| keep(&e["capability_id"]))
                .map(evidence)
                .collect()
        })
        .unwrap_or_default();
    let count = |s: &str| assessments.iter().filter(|a| a["status"] == s).count();
    let counts = json!({
        "assessments": assessments.len(),
        "supported": count("supported"),
        "unsupported": count("unsupported"),
        "unknown": count("unknown"),
        "evidence": evs.len(),
        "report_assessments": all.len(),
    });
    let p = &raw["profile"];
    // generation and rule_revision live in each assessment's scope (§7 lists them at the top)
    let scope = all
        .first()
        .map(|a| pick(&a["scope"], &["generation", "rule_revision", "cohort"]));
    let coverage: serde_json::Map<String, Value> = raw["context_coverage"]
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(k, v)| (k.clone(), pick(v, &["authoritative", "reason", "source"])))
                .collect()
        })
        .unwrap_or_default();
    let build = |a: &[Value], e: &[Value], truncated: bool| {
        json!({
            "state": "ok",
            "schema_version": raw["schema_version"],
            "filter": prefix,
            "identity": {
                "identity_status": p["identity_status"],
                "vendor": p["vendor"],
                "model": p["model"],
                "firmware": pick(&p["firmware"], &["version", "build", "region", "product_line", "modem_build", "active_slot", "completeness"]),
            },
            "scope": scope,
            "last_discovery": raw.get("last_discovery").cloned(),
            "context_coverage": coverage,
            "counts": counts,
            "assessments": a,
            "evidence": e,
            "truncated": truncated,
            "limitations": LIMITATIONS,
        })
    };
    let mut truncated = false;
    loop {
        let v = build(&assessments, &evs, truncated);
        if v.to_string().len() <= REPLY_MAX || (assessments.is_empty() && evs.is_empty()) {
            return v;
        }
        truncated = true;
        if evs.pop().is_none() {
            assessments.pop();
        }
    }
}

/// The core's action catalogue, its own words; one that does not describe itself (an old core)
/// is said so.
pub fn shape_recovery(raw: &Value) -> Value {
    if raw["schema_version"] != 1 || !raw["actions"].is_array() {
        return json!({"state": "not_described", "why": "this core does not describe its recovery actions (too old, or another schema)"});
    }
    let mut v = raw.clone();
    v["state"] = json!("ok");
    v
}

#[derive(serde::Deserialize)]
pub struct CapQ {
    capability: Option<String>,
}

pub async fn capabilities(
    State(app): State<Shared>,
    Query(q): Query<CapQ>,
    req: Request,
) -> Response {
    let name = match admit(&req) {
        Ok(n) => n,
        Err(r) => return r,
    };
    log(&name, "capabilities");
    let prefix = q.capability.filter(|p| !p.is_empty() && p.len() <= 64);
    match read(&app, "/device/capabilities").await {
        Ok(Ok(r)) => reply(shape_capabilities(&r, prefix.as_deref())),
        Ok(Err(code)) => reply(json!({"core": "up", "state": "unverified",
            "why": "the core serves no capability report", "http": code})),
        Err(_) => core_down(),
    }
}

pub async fn recovery_actions(State(app): State<Shared>, req: Request) -> Response {
    let name = match admit(&req) {
        Ok(n) => n,
        Err(r) => return r,
    };
    log(&name, "recovery-actions");
    match read(&app, "/recovery/actions").await {
        Ok(Ok(r)) => reply(shape_recovery(&r)),
        Ok(Err(code)) => reply(json!({"core": "up", "state": "not_described",
            "why": "this core does not describe its recovery actions (too old)", "http": code})),
        Err(_) => core_down(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> Value {
        // the core's own empty-collector shape: everything unknown, coverage not authoritative
        serde_json::from_str(include_str!("firmware_fixture.json")).unwrap()
    }

    #[test]
    fn unknown_report_passes_through_with_limitations() {
        let v = shape_capabilities(&report(), None);
        assert_eq!(v["state"], "ok");
        assert_eq!(v["truncated"], false);
        assert!(v["limitations"].as_array().unwrap().len() >= 3);
        assert_eq!(v["counts"]["unknown"], v["counts"]["assessments"]);
        assert!(
            v["assessments"]
                .as_array()
                .unwrap()
                .iter()
                .all(|a| a["status"] == "unknown")
        );
        for (_, c) in v["context_coverage"].as_object().unwrap() {
            assert_eq!(c["authoritative"], false);
        }
    }

    #[test]
    fn prefix_filters_and_no_raw_values_leave() {
        let v = shape_capabilities(&report(), Some("usb."));
        assert!(
            v["assessments"]
                .as_array()
                .unwrap()
                .iter()
                .all(|a| a["capability_id"].as_str().unwrap().starts_with("usb."))
        );
        let s = v.to_string();
        assert!(!s.contains("\"args\"") && !s.contains("\"value\""));
    }

    #[test]
    fn undecodable_report_is_unverified_not_supported() {
        let v = shape_capabilities(&json!({"schema_version": 99, "assessments": []}), None);
        assert_eq!(v["state"], "unverified");
        assert!(v.get("assessments").is_none());
    }

    #[test]
    fn oversized_report_is_cut_and_says_so() {
        let mut r = report();
        let e0 = r["evidence"].as_array().and_then(|e| e.first().cloned());
        if let Some(mut e) = e0 {
            let base = e["id"].as_str().unwrap().to_string();
            e["summary"] = json!("x".repeat(60));
            e["limitations"] = json!(["y".repeat(60), "z".repeat(60)]);
            let ev = r["evidence"].as_array_mut().unwrap();
            for i in 0..400 {
                let mut c = e.clone();
                c["id"] = json!(format!("{base}-{i}"));
                ev.push(c);
            }
        }
        let v = shape_capabilities(&r, None);
        if v["state"] == "ok" && v["counts"]["evidence"].as_u64().unwrap_or(0) > 100 {
            assert_eq!(v["truncated"], true);
            assert!(v.to_string().len() <= REPLY_MAX);
        }
    }

    #[test]
    fn old_core_recovery_is_not_described() {
        assert_eq!(shape_recovery(&json!({}))["state"], "not_described");
        let ok = shape_recovery(
            &json!({"schema_version": 1, "actions": [], "allow_list": {"actions": [], "empty": true}}),
        );
        assert_eq!(ok["state"], "ok");
        assert_eq!(ok["allow_list"]["empty"], true);
    }
}
