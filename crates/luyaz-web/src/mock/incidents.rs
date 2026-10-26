//! The core's regress incidents for the /alerts e2e (`e2e/incidents.mjs`): `/v2/incidents`,
//! `/v2/incidents/{id}` and the incident ids on `/v2/api/alerts`' events, by scenario
//! (`POST /mock/incidents?set=off|ok|many|empty|error|down`; `off` = a core without incidents, 404).

use std::sync::Mutex;

use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

static SET: Mutex<String> = Mutex::new(String::new());

fn set() -> String {
    let s = SET.lock().unwrap().clone();
    if s.is_empty() { "off".into() } else { s }
}

fn now() -> i64 {
    // the device clock reads local time labelled UTC: any epoch will do for the page's digits
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

#[derive(serde::Deserialize)]
pub struct Set {
    set: String,
}

pub async fn choose(Query(q): Query<Set>) -> StatusCode {
    *SET.lock().unwrap() = q.set;
    StatusCode::NO_CONTENT
}

fn inc(id: &str, kind: &str, zh: &str, en: &str, from: i64, to: Option<i64>, end: Option<&str>, count: u64, unread: bool) -> Value {
    json!({"id": id, "kind": kind, "sub": null, "name_zh": zh, "name_en": en, "from": from, "to": to,
           "closed": to.is_some(), "end": end, "count": count, "build": "880e05c", "detail": "",
           "observed": null, "ship": null, "ship_clock": null, "evidence": true, "unread": unread})
}

fn list() -> Option<Value> {
    let t = now();
    let mut mem = inc("mem-abs-1", "regress-mem-abs", "内存超标", "High memory", t - 7800, None, None, 1, true);
    mem["ship"] = json!({"comp": "core", "txn": "t1", "ago_s": 600});
    mem["ship_clock"] = json!("same_boot");
    mem["detail"] = json!("anon 100MiB > 96MiB 30m");
    let egress = inc("egress-chill-1", "regress-egress", "出口不通", "Route down", t - 8 * 86_400, None, None, 1, false);
    let crash = inc("q3", "core-crash", "数据服务崩溃", "Data service crash", t - 5000, Some(t - 1000), Some("quiet"), 3, false);
    let cell = inc("cellular-1", "regress-cellular", "蜂窝断线", "Cellular down", t - 20_000, Some(t - 19_000), Some("recovered"), 1, false);
    let gone = inc("gone-1", "regress-power-abs", "耗电变高", "Higher power draw", t - 90_000, Some(t - 80_000), Some("recovered"), 1, false);
    Some(match set().as_str() {
        "ok" => json!({"v": 1, "open": [mem, egress], "closed": [crash, cell, gone], "ingest_gap": false}),
        "many" => {
            let closed: Vec<Value> = (0..53)
                .map(|k| inc(&format!("q{}", 100 + k), "ui-crash", "触屏闪退", "Screen UI crash", t - 10_000 - k * 600, Some(t - 9_000 - k * 600), Some("quiet"), 1, false))
                .collect();
            json!({"v": 1, "open": [mem], "closed": closed, "ingest_gap": false})
        }
        "empty" => json!({"v": 1, "open": [], "closed": [], "ingest_gap": false}),
        "error" => json!({"v": 1, "open": [], "closed": [], "error": "the regress layer has not run yet"}),
        _ => return None,
    })
}

pub async fn incidents() -> Response {
    match set().as_str() {
        "down" => (StatusCode::INTERNAL_SERVER_ERROR, axum::Json(json!({"ok": false, "error": "boom"}))).into_response(),
        _ => match list() {
            Some(v) => axum::Json(v).into_response(),
            None => (StatusCode::NOT_FOUND, axum::Json(json!({"ok": false, "error": "no route"}))).into_response(),
        },
    }
}

pub async fn one(Path(id): Path<String>) -> Response {
    let Some(v) = list() else {
        return (StatusCode::NOT_FOUND, axum::Json(json!({"reason": "pruned"}))).into_response();
    };
    let found = v["open"]
        .as_array()
        .into_iter()
        .chain(v["closed"].as_array())
        .flatten()
        .find(|i| i["id"] == id.as_str())
        .cloned();
    let Some(mut i) = found.filter(|_| id != "gone-1") else {
        return (StatusCode::NOT_FOUND, axum::Json(json!({"reason": "pruned"}))).into_response();
    };
    let t = now();
    let alerts: Vec<Value> = match id.as_str() {
        "q3" => (3..=5)
            .map(|s| json!({"seq": s, "time": t - 5000 + (s as i64 - 3) * 900, "kind": "core-crash", "text": "exit 139", "unread": false}))
            .collect(),
        "mem-abs-1" => vec![json!({"seq": 6, "time": t - 7800, "kind": "regress-mem-abs", "text": "[rg-mem-abs-1-open] anon 100MiB > 96MiB 30m", "unread": true})],
        _ => vec![],
    };
    i["alerts"] = json!(alerts);
    i["alert_more"] = json!(0);
    i["label"] = json!(luyaz_api::alerts::kind_label(i["kind"].as_str().unwrap_or("")));
    i["label_en"] = json!(luyaz_api::alerts::kind_label_en(i["kind"].as_str().unwrap_or("")));
    if id == "mem-abs-1" {
        i["memsnap"] = json!("Rss: 120432 kB\nAnonymous: 110388 kB\nThreads: 21\n");
        i["observed"] = Value::Null;
    }
    if id == "q3" {
        i["observed"] = json!({"zh": "原厂动作：redial", "en": "stock firmware: redial"});
    }
    axum::Json(i).into_response()
}

/// The /v2/api/alerts events with incident ids, for scenarios that have incidents (else None:
/// the mock's own two alerts as before).
pub fn alerts(_unread: u64) -> Option<Value> {
    if set() == "off" {
        return None;
    }
    let t = now();
    let ev = |seq: u64, kind: &str, zh: &str, en: &str, text: &str, inc: Option<&str>, unread: bool| {
        json!({"seq": seq, "time": t - 9000 + seq as i64 * 600, "uptime": seq * 100, "kind": kind, "label": zh,
               "label_en": en, "text": text, "unread": unread, "incident": inc})
    };
    let many = set() == "many";
    let mut events = vec![
        ev(6, "regress-mem-abs", "数据服务内存超过上限", "Data service memory over the limit", "[rg-mem-abs-1-open] anon 100MiB > 96MiB 30m", Some("mem-abs-1"), true),
        ev(5, "core-crash", "数据服务意外退出，已自动重启", "Data service exited unexpectedly", "exit 139", Some("q3"), false),
        ev(4, "core-crash", "数据服务意外退出，已自动重启", "Data service exited unexpectedly", "exit 139", Some("q3"), false),
        ev(3, "core-crash", "数据服务意外退出，已自动重启", "Data service exited unexpectedly", "exit 139", Some("q3"), false),
        ev(2, "wifi_down", "Wi-Fi 掉了", "Wi-Fi dropped", "已自动恢复", None, false),
        ev(1, "core_restart", "数据服务重启", "Data service restarted", "", None, false),
    ];
    if many {
        events.insert(0, ev(7, "ui-crash", "触屏界面闪退，已自动重新打开", "Screen UI exited unexpectedly", "the 53rd", Some("q152"), false));
    }
    let n = events.len() as u64;
    Some(json!({"events": events, "unread": 1, "read": n - 1, "uptime_now": 3000,
                "clock_set": true, "sms": {"configured": false, "number": null, "recent": []}}))
}
