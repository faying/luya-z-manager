//! Tests for CHILL's page reads and the subscription refresh in chill_page.rs (batch 5b).

use super::chill_page::providers_data;
use super::tests::{call, start};
use super::*;
use std::fs;
use std::sync::Mutex;

fn scratch(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("u60d-chillreads-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// A fake mihomo: GET /providers/proxies, PUT /providers/proxies/{name} (logged).
async fn mihomo(log: std::sync::Arc<Mutex<Vec<String>>>) -> String {
    use axum::routing::{get, put};
    let app = Router::new()
        .route(
            "/providers/proxies",
            get(|| async {
                Json(json!({"providers": {
                    "sub": {"vehicleType": "HTTP", "updatedAt": "2026-10-13T01:00:00Z",
                        "proxies": [{}, {}], "subscriptionInfo": {"Total": 1}},
                    "snap1": {"vehicleType": "File", "proxies": [{}]},
                    "default": {"vehicleType": "Compatible", "proxies": []}}}))
            }),
        )
        .route(
            "/providers/proxies/{name}",
            put(
                move |axum::extract::Path(name): axum::extract::Path<String>| {
                    let log = log.clone();
                    async move {
                        log.lock().unwrap().push(name.clone());
                        if name == "bad" {
                            StatusCode::NOT_FOUND
                        } else {
                            StatusCode::NO_CONTENT
                        }
                    }
                },
            ),
        );
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await });
    format!("http://{addr}")
}

#[test]
fn providers_shape() {
    let v = providers_data(Some(&json!({"providers": {
        "sub": {"vehicleType": "HTTP", "proxies": [{}, {}]},
        "snap1": {"vehicleType": "File"},
        "DIRECT": {"vehicleType": "compatible"}}})));
    let names: Vec<_> = v["providers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["snap1", "sub"], "sorted, Compatible dropped");
    assert_eq!(v["providers"][1]["node_count"], 2);
    assert_eq!(v["providers"][1]["editable"], true);
    assert_eq!(v["providers"][0]["node_count"], 0);
    assert_eq!(v["providers"][0]["editable"], false);
    assert_eq!(providers_data(None), json!({"providers": []}));
}

#[tokio::test]
async fn reads_with_writes_off() {
    let root = scratch("reads");
    fs::create_dir_all(root.join("tmp")).unwrap();
    let log: String = (0..300).map(|i| format!("l{i}\n")).collect();
    fs::write(root.join("tmp/chill.log"), log).unwrap();
    let calls = std::sync::Arc::new(Mutex::new(Vec::new()));
    let src = Sources {
        mihomo: mihomo(calls.clone()).await,
        ..Sources::none()
    };
    let addr = start(Compat::new(root.to_str().unwrap(), src)).await;

    let (st, b) = call(addr, "GET", "/api/services/chill/log", None, "").await;
    assert_eq!(st, 200);
    assert_eq!(b["data"]["path"], "/tmp/chill.log");
    assert_eq!(b["data"]["limit"], 200);
    assert_eq!(b["data"]["lines"].as_array().unwrap().len(), 200);
    assert_eq!(b["data"]["lines"][199], "l299");

    let (st, b) = call(addr, "GET", "/api/services/chill/providers", None, "").await;
    assert_eq!(st, 200);
    assert_eq!(b["data"]["providers"].as_array().unwrap().len(), 2, "{b}");
    assert_eq!(
        b["data"]["providers"][1]["subscription"],
        json!({"Total": 1})
    );


    // the refresh is a write: not served with writes off
    let (st, b) = call(
        addr,
        "POST",
        "/api/services/chill/providers/refresh",
        None,
        r#"{"name":"sub"}"#,
    )
    .await;
    assert_eq!((st, b["error"].as_str()), (404, Some("not_in_compat_yet")));
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn providers_unreachable_is_empty() {
    let root = scratch("down");
    let addr = start(Compat::new(root.to_str().unwrap(), Sources::none())).await;
    let (st, b) = call(addr, "GET", "/api/services/chill/providers", None, "").await;
    assert_eq!((st, b["data"].clone()), (200, json!({"providers": []})));
}

#[tokio::test]
async fn refresh_with_writes_on() {
    let root = scratch("refresh");
    let calls = std::sync::Arc::new(Mutex::new(Vec::new()));
    let src = Sources {
        mihomo: mihomo(calls.clone()).await,
        ..Sources::none()
    };
    let addr = start(Compat::new(root.to_str().unwrap(), src).with_writes(true)).await;
    let path = "/api/services/chill/providers/refresh";

    let (st, b) = call(addr, "POST", path, None, r#"{"name":"sub"}"#).await;
    assert_eq!((st, b.clone()), (200, json!({"ok": true})));
    let (st, b) = call(addr, "POST", path, None, r#"{"name":"🐟 漏网 之鱼"}"#).await;
    assert_eq!(st, 200, "{b}");
    let (st, b) = call(addr, "POST", path, None, r#"{"name":"bad"}"#).await;
    assert_eq!(st, 502);
    assert_eq!(b["error"], "mihomo rejected refresh (HTTP 404)");
    let (st, b) = call(addr, "POST", path, None, r#"{"name":" "}"#).await;
    assert_eq!((st, b["error"].as_str()), (400, Some("missing 'name'")));
    let (st, b) = call(addr, "POST", path, None, "nope").await;
    assert_eq!((st, b["error"].as_str()), (400, Some("invalid JSON")));
    assert_eq!(*calls.lock().unwrap(), ["sub", "🐟 漏网 之鱼", "bad"]);

    // GET and PUT on the same path still both answer with writes on
    let (st, b) = call(addr, "GET", "/api/services/chill/providers", None, "").await;
    assert_eq!(st, 200, "{b}");
    let (st, _) = call(addr, "PUT", "/api/services/chill/providers", None, "{}").await;
    assert_ne!(st, 404);
    assert_ne!(st, 405);
}
