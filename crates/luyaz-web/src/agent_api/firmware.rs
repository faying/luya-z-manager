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
            "stock_anomalies": raw.get("stock_anomalies").map(stock_anomalies),
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

/// Stock recovery labels the core's no-service ledger may use.
const STOCK: &[&str] = &["lpm_cycle", "sim_reset", "redial", "unknown", "ssr", "modem_restart"];

fn labels(v: &Value) -> Value {
    v.as_array()
        .map(|a| a.iter().filter(|s| s.as_str().is_some_and(|s| STOCK.contains(&s))).cloned().collect())
        .unwrap_or_default()
}

fn ledger_row(r: &Value) -> Value {
    let mut o = pick(r, &["start", "end", "secs", "verified", "alerted"]);
    o["seen"] = labels(&r["seen"]);
    if let Some(s) = r["stock"].as_str().filter(|s| STOCK.contains(s)) {
        o["stock"] = json!(s);
    }
    o
}

fn rows(v: &Value) -> Value {
    v.as_array()
        .map(|a| a.iter().take(50).map(ledger_row).collect())
        .unwrap_or_default()
}

/// R18-P2-01: the core's read-only stock-firmware anomalies, field by field. Interface names
/// come from the device's own uci and process list, so they go out as untrusted text.
fn stock_anomalies(raw: &Value) -> Value {
    let names = |v: &Value| -> Value {
        v.as_array()
            .map(|a| a.iter().take(16).map(untrusted).collect())
            .unwrap_or_default()
    };
    let w = &raw["wifi_post_stuck"];
    let wifi = (!w.is_null()).then(|| {
        let mut o = pick(w, &["alive_s", "enabled_count", "hostapd_cli_count", "forks_per_s"]);
        o["enabled"] = names(&w["enabled"]);
        o["hostapd_cli"] = names(&w["hostapd_cli"]);
        o
    });
    let ns = &raw["no_service"];
    let open = (!ns["open"].is_null()).then(|| ledger_row(&ns["open"]));
    json!({
        "wifi_post_stuck": wifi,
        "no_service": {"open": open, "recent": rows(&ns["recent"])},
        "unverified": labels(&raw["unverified"]),
    })
}

#[derive(serde::Deserialize)]
pub struct HistQ {
    limit: Option<u32>,
}

/// R18-P2-01: the core's no-service ledger, newest first.
pub async fn no_service_history(
    State(app): State<Shared>,
    Query(q): Query<HistQ>,
    req: Request,
) -> Response {
    let name = match admit(&req) {
        Ok(n) => n,
        Err(r) => return r,
    };
    log(&name, "no-service-history");
    let n = q.limit.unwrap_or(10).clamp(1, 50);
    match read(&app, &format!("/device/no-service-history?limit={n}")).await {
        Ok(Ok(r)) => reply(json!({"state": "ok", "rows": rows(&r["rows"]), "keep": r["keep"]})),
        Ok(Err(code)) => reply(json!({"core": "up", "state": "not_described",
            "why": "this core keeps no no-service ledger (too old)", "http": code})),
        Err(_) => core_down(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stock_anomalies_only_whitelisted_fields_and_labels() {
        let raw = json!({
            "wifi_post_stuck": {"alive_s": 90, "enabled_count": 2, "hostapd_cli_count": 1,
                "forks_per_s": 250.0, "enabled": ["main_2g", "main_5g"], "hostapd_cli": ["ath0"],
                "secret": "x"},
            "no_service": {"open": {"start": 1, "secs": 400, "seen": ["redial", "rm -rf"], "x": 1},
                "recent": [{"start": 1, "end": 2, "secs": 1, "stock": "evil", "seen": [], "verified": false}]},
            "unverified": ["ssr", "modem_restart", "other"],
        });
        let s = stock_anomalies(&raw);
        let w = &s["wifi_post_stuck"];
        assert_eq!(w["alive_s"], 90);
        assert!(w.get("secret").is_none());
        assert_eq!(w["enabled"][0], json!({"untrusted": "main_2g"}));
        assert_eq!(s["no_service"]["open"]["seen"], json!(["redial"]));
        assert!(s["no_service"]["open"].get("x").is_none());
        assert!(s["no_service"]["recent"][0].get("stock").is_none());
        assert_eq!(s["unverified"], json!(["ssr", "modem_restart"]));
        let none = stock_anomalies(&json!({"wifi_post_stuck": null, "no_service": {"open": null, "recent": []}}));
        assert!(none["wifi_post_stuck"].is_null() && none["no_service"]["open"].is_null());
    }

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
