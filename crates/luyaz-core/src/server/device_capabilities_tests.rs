use super::*;
use luyaz_api::firmware::{CapabilityReport, Support};

// No App::new, global executor, collection loop, policy host or disk state in this fixture.
pub(super) async fn app() -> (App, crate::ubus::mock_ubusd::MockUbusd) {
    let m = crate::ubus::mock_ubusd::MockUbusd::start_new().await;
    let hub = Arc::new(Hub::new(vec![], Box::new(block::NoSink)));
    let exec = Executor::spawn(
        m.backend(Duration::from_millis(50)),
        hub,
        executor::Config::default(),
        Duration::from_secs(1),
    );
    let snapshot = Snapshot {
        ts: 0,
        core: Default::default(),
        fields: Map::new(),
    };
    let ops = crate::ops::Engine::new(
        crate::ops::UbusDevice::new(exec.clone()),
        crate::ops::Config {
            rollback: false,
            poll: Duration::from_secs(2),
            legacy_ttl_ms: 120_000,
        },
        crate::ops::pending::Store::open(None),
        crate::ops::record::Record::open(None),
    );
    (
        App {
            inner: Arc::new(Inner {
                snapshot: RwLock::new(snapshot.clone()),
                tx: watch::channel(snapshot).0,
                exec,
                _data_dir: PathBuf::new(),
                token: Some("synthetic-token".into()),
                sessions: Mutex::new(Sessions::default()),
                sse_slots: Arc::new(Semaphore::new(1)),
                feed: v2::Feed::new(v2::CAPACITY),
                ops,
                stage: watch::channel(Stage::Starting).0,
                interval: Duration::from_secs(1),
                stop: watch::channel(false).0,
            }),
        },
        m,
    )
}

#[tokio::test]
async fn device_capabilities_authentication_and_passive_shared_snapshot() {
    let (app, mock) = app().await;
    mock.add_method("zwrt_bsp.usb", 0x10, "list", json!({"mode":"user"}));
    let collector = app.inner.exec.hub().firmware();
    collector.ubus(collector.token(), luyaz_api::clock::BootTime::read(), "zwrt_zte_mdm.api", "get_zwrt_common_info",
        Some(&json!({"manufacturer":"ZTE","model_name":"MU5250","integrate_version":"CN_ZTE_MU5250V1.0.0B31"})));
    let before = collector.report(luyaz_api::clock::BootTime::read().as_ref());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(axum::serve(listener, app.router(true, false, None)).into_future());
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let path = format!("{base}/v2/device/capabilities");
    for token in [None, Some("wrong")] {
        let mut request = client.get(&path);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        assert_eq!(
            request.send().await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }
    for _ in 0..3 {
        let response = client
            .get(&path)
            .bearer_auth("synthetic-token")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cache-control"], "private, no-store");
        let report = CapabilityReport::decode(&response.text().await.unwrap()).unwrap();
        assert_eq!(report.profile, before.profile);
        assert_eq!(report.evidence, before.evidence);
        assert!(
            report
                .assessments
                .iter()
                .all(|a| a.status == Support::Unknown)
        );
    }
    assert_eq!(mock.invokes("zwrt_bsp.usb"), 0);
    assert_eq!(app.stage(), Stage::Starting); // GET did not wait for or trigger startup collection.
    let legacy: Value = serde_json::from_str(
        &client
            .get(format!("{base}/capabilities"))
            .bearer_auth("synthetic-token")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(legacy, capabilities().await.0);
    assert_eq!(
        client
            .post(&path)
            .bearer_auth("synthetic-token")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    task.abort();
}

#[tokio::test]
async fn device_capabilities_uses_existing_loopback_listener_policy() {
    let (app, _) = app().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(axum::serve(listener, app.router(false, false, None)).into_future());
    let response = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap()
        .get(format!("http://{address}/v2/device/capabilities"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    CapabilityReport::decode(&response.text().await.unwrap()).unwrap();
    task.abort();
}

#[tokio::test]
async fn usb_control_sources_share_the_final_gate_and_authentication() {
    let (app, mock) = app().await;
    app.inner.stage.send_replace(Stage::Ready);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/control", listener.local_addr().unwrap());
    let task = tokio::spawn(axum::serve(listener, app.router(true, false, None)).into_future());
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();
    let body = json!({"action":"usb.set","source":"auto","params":{"mode":"debug"}});
    assert_eq!(
        client
            .post(&url)
            .header("content-type", "application/json")
            .body(serde_json::to_vec(&body).unwrap())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    for source in [
        None,
        Some("auto"),
        Some("scenario"),
        Some("scheduler"),
        Some("guard"),
        Some("legacy"),
        Some("screen"),
    ] {
        let mut body = body.clone();
        if let Some(source) = source {
            body["source"] = json!(source);
        } else {
            body.as_object_mut().unwrap().remove("source");
        }
        if source == Some("screen") {
            body["params"]["remembered"] = true.into();
        }
        let r = client
            .post(&url)
            .bearer_auth("synthetic-token")
            .header("content-type", "application/json")
            .body(serde_json::to_vec(&body).unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::CONFLICT, "{source:?}");
        let body: Value = serde_json::from_str(&r.text().await.unwrap()).unwrap();
        assert_eq!(body["error"]["code"], "capability_unverified");
        assert_eq!(
            body["error"]["policy"]["assessment"]["capability_id"],
            "usb.mode.set"
        );
    }
    assert_eq!(mock.invokes("zwrt_bsp.usb"), 0);
    task.abort();
}
