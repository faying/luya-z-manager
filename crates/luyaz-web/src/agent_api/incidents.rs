//! The regress incidents for the MCP gateway (design 2026-10-24, CEO review X5/OV-C5): the core's
//! `/v2/incidents` and `/v2/incidents/<id>`, field by field; free text (details, what was seen,
//! alert texts) as `untrusted`. An older core without them is `not_described`, never "no incidents".

use axum::extract::{Path, Request, State};
use axum::response::Response;
use serde_json::{Value, json};

use super::{REPLY_MAX, admit, core_down, log, read, reply, untrusted};
use crate::pages::Shared;

fn one(i: &Value) -> Value {
    json!({
        "id": i["id"], "kind": i["kind"], "sub": i["sub"],
        "name_zh": i["name_zh"], "name_en": i["name_en"],
        "from": i["from"], "to": i["to"], "closed": i["closed"], "end": i["end"], "count": i["count"],
        "build": i["build"], "detail": untrusted(&i["detail"]),
        "observed_zh": untrusted(&i["observed"]["zh"]), "observed_en": untrusted(&i["observed"]["en"]),
        "ship": i["ship"], "ship_clock": i["ship_clock"], "drill": i["drill"], "evidence": i["evidence"], "unread": i["unread"],
    })
}

/// `GET /agent-api/incidents`: open ones, then those ended in the last 7 days.
pub async fn list(State(app): State<Shared>, req: Request) -> Response {
    let name = match admit(&req) {
        Ok(n) => n,
        Err(r) => return r,
    };
    log(&name, "incidents");
    match read(&app, "/incidents").await {
        Ok(Ok(v)) if v["error"].is_string() => reply(json!({"core": "up", "state": "unavailable",
            "why": untrusted(&v["error"])})),
        Ok(Ok(v)) => {
            let side = |k: &str| v[k].as_array().map(|a| a.iter().map(one).collect::<Vec<_>>()).unwrap_or_default();
            let (open, mut closed) = (side("open"), side("closed"));
            let mut out = json!({"state": "ok", "open": open, "closed": [], "ingest_gap": v["ingest_gap"]});
            // newest ended first: drop from the end until the reply fits
            while !closed.is_empty() {
                out["closed"] = json!(closed);
                if out.to_string().len() <= REPLY_MAX {
                    break;
                }
                closed.pop();
                out["truncated"] = json!(true);
            }
            reply(out)
        }
        Ok(Err(code)) => reply(json!({"core": "up", "state": "not_described",
            "why": "this core keeps no incidents (older than 1.45.0)", "http": code})),
        Err(_) => core_down(),
    }
}

/// `GET /agent-api/incidents/<id>`: one incident, its alerts, evidence hours and memory snapshot.
pub async fn one_id(State(app): State<Shared>, Path(id): Path<String>, req: Request) -> Response {
    let name = match admit(&req) {
        Ok(n) => n,
        Err(r) => return r,
    };
    if id.is_empty()
        || id.len() > 64
        || !id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return reply(json!({"state": "bad_id", "why": "an incident id is [a-z0-9-], at most 64"}));
    }
    log(&name, "incidents/<id>");
    match read(&app, &format!("/incidents/{id}")).await {
        Ok(Ok(v)) => {
            let mut out = one(&v);
            out["state"] = json!("ok");
            out["label"] = v["label"].clone();
            out["label_en"] = v["label_en"].clone();
            out["memsnap"] = untrusted(&v["memsnap"]);
            out["evidence_hours"] = v["evidence_hours"].clone();
            out["alert_more"] = v["alert_more"].clone();
            out["archived"] = v["archived"].clone();
            out["recovery_s"] = v["recovery_s"].clone();
            out["boot_code"] = v["boot_code"].clone();
            let mut alerts: Vec<Value> = v["alerts"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|e| json!({"seq": e["seq"], "time": e["time"], "kind": e["kind"],
                                        "text": untrusted(&e["text"]), "unread": e["unread"]}))
                        .collect()
                })
                .unwrap_or_default();
            loop {
                out["alerts"] = json!(alerts);
                if out.to_string().len() <= REPLY_MAX || alerts.is_empty() {
                    break;
                }
                alerts.remove(0);
                out["truncated"] = json!(true);
            }
            if out.to_string().len() > REPLY_MAX {
                out["evidence_hours"] = Value::Null;
            }
            reply(out)
        }
        Ok(Err(404)) => reply(json!({"state": "pruned",
            "why": "no such incident now (past its keeping time, or an older core)"})),
        Ok(Err(code)) => reply(json!({"core": "up", "state": "unavailable", "http": code})),
        Err(_) => core_down(),
    }
}
