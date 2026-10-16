//! Tests for cell_detect.rs (8d-1b).

use super::tests::{call, start};
#[allow(unused_imports)]
use super::*;
use std::sync::Mutex;

/// What CN B31 answered when no detection had run.
const B31_PROGRESS: &str = r#"{"progress":"","quality":""}"#;
const B31_RESULTS: &str = "{}";

#[test]
fn vendor_answers_pass_through_unchanged() {
    for raw in [
        B31_PROGRESS,
        B31_RESULTS,
        r#"{"progress":"40","quality":"good"}"#,
        r#"{"list":[{"rsrp":"-95","sinr":"12"}]}"#,
    ] {
        let v: Value = serde_json::from_str(raw).unwrap();
        assert_eq!(
            cell_detect::answer(Ok(v.clone())),
            (200, json!({"ok": true, "data": v}))
        );
    }
    assert_eq!(
        cell_detect::answer(Err("ubus: timed out".into())),
        (503, json!({"ok": false, "error": "ubus: timed out"}))
    );
}

type Seen = Arc<Mutex<Vec<Value>>>;

/// A `/control` that records each request and answers `answer`.
async fn control_server(seen: Seen, answer: (u16, Value)) -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let app = Router::new().route(
        "/control",
        post(move |body: Bytes| {
            let seen = seen.clone();
            let answer = answer.clone();
            async move {
                seen.lock()
                    .unwrap()
                    .push(serde_json::from_slice(&body).unwrap_or(Value::Null));
                reply(answer.0, answer.1)
            }
        }),
    );
    tokio::spawn(async move { axum::serve(l, app).await });
    format!("http://{addr}/control")
}

fn sources(control: Option<String>) -> Sources {
    let mut s = Sources {
        ubus: Arc::new(|o, m, a| {
            let r = match (o, m, a == json!({})) {
                ("zte_nwinfo_api", "nwinfo_get_progress_and_quality", true) => {
                    Ok(serde_json::from_str(B31_PROGRESS).unwrap())
                }
                ("zte_nwinfo_api", "nwinfo_get_detect_quality_recorder", true) => {
                    Ok(serde_json::from_str(B31_RESULTS).unwrap())
                }
                _ => Err(format!("{o} {m}: not faked")),
            };
            Box::pin(async move { r })
        }),
        ..Sources::none()
    };
    if let Some(url) = control {
        s.control = Arc::new(control::Client::new(url, None));
    }
    s
}

fn root(name: &str) -> String {
    let d = std::env::temp_dir().join(format!("u60d-cell-detect-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.to_str().unwrap().to_string()
}

#[tokio::test]
async fn reads_as_the_agent_with_b31_shapes() {
    let addr = start(Compat::new(&root("reads"), sources(None))).await;
    let (st, b) = call(addr, "GET", "/api/cell/signal-detect/progress", None, "").await;
    assert_eq!(
        (st, b),
        (
            200,
            json!({"ok": true, "data": {"progress": "", "quality": ""}})
        )
    );
    let (st, b) = call(addr, "GET", "/api/cell/signal-detect/results", None, "").await;
    assert_eq!((st, b), (200, json!({"ok": true, "data": {}})));

    // ubus does not answer: the agent's 503
    let addr = start(Compat::new(&root("reads-down"), Sources::none())).await;
    // writes off (the shadow period): start is not here, luyaz-web asks the agent
    let (st, b) = call(addr, "POST", "/api/cell/signal-detect/start", None, "").await;
    assert_eq!((st, b["error"].as_str()), (404, Some("not_in_compat_yet")));
    let (st, b) = call(addr, "GET", "/api/cell/signal-detect/progress", None, "").await;
    assert_eq!((st, b), (503, json!({"ok": false, "error": "no ubus"})));
}

#[tokio::test]
async fn start_and_stop_are_vendor_calls_through_control() {
    let seen: Seen = Arc::default();
    let url = control_server(
        seen.clone(),
        (200, json!({"ok": true, "result": {"result": "success"}})),
    )
    .await;
    let addr = start(Compat::new(&root("writes"), sources(Some(url))).with_writes(true)).await;
    for (path, method) in [
        (
            "/api/cell/signal-detect/start",
            "nwinfo_start_detect_signal_quality",
        ),
        (
            "/api/cell/signal-detect/stop",
            "nwinfo_end_detect_signal_quality",
        ),
    ] {
        let (st, b) = call(addr, "POST", path, None, "").await;
        assert_eq!(
            (st, b),
            (200, json!({"ok": true, "data": {"result": "success"}})),
            "{path}"
        );
        assert_eq!(
            seen.lock().unwrap().pop(),
            Some(json!({"action": "vendor.call", "source": "web",
                "params": {"object": "zte_nwinfo_api", "method": method, "args": {}}})),
            "{path}"
        );
    }
}

#[tokio::test]
async fn control_refusal_and_no_control_are_503() {
    let seen: Seen = Arc::default();
    let url = control_server(
        seen.clone(),
        (
            500,
            json!({"ok": false, "error": {"code": "failed", "message": "ubus: 7"}}),
        ),
    )
    .await;
    let addr = start(Compat::new(&root("refused"), sources(Some(url))).with_writes(true)).await;
    let (st, b) = call(addr, "POST", "/api/cell/signal-detect/start", None, "").await;
    assert_eq!((st, b), (503, json!({"ok": false, "error": "ubus: 7"})));

    let addr = start(Compat::new(&root("nocontrol"), sources(None)).with_writes(true)).await;
    let (st, b) = call(addr, "POST", "/api/cell/signal-detect/stop", None, "").await;
    assert_eq!(st, 503);
    assert!(b["error"].as_str().unwrap().starts_with("datad: "), "{b}");
}

#[tokio::test]
async fn start_waits_for_a_diagnosis_stop_does_not() {
    let seen: Seen = Arc::default();
    let url = control_server(seen.clone(), (200, json!({"ok": true, "result": {}}))).await;
    let c = Compat::new(&root("diag"), sources(Some(url))).with_writes(true);
    c.gate().diag_since = Some(c.clock.now() as u64);
    let addr = start(c).await;
    let (st, b) = call(addr, "POST", "/api/cell/signal-detect/start", None, "").await;
    assert_eq!((st, b["busy"].clone()), (409, json!("diagnose")), "{b}");
    assert!(seen.lock().unwrap().is_empty());
    let (st, _) = call(addr, "POST", "/api/cell/signal-detect/stop", None, "").await;
    assert_eq!(st, 200);
    assert_eq!(seen.lock().unwrap().len(), 1);
}
