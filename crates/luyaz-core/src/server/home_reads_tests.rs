//! `/v2/latency/direct` and `/v2/timeline/today` through the real router (the shared fixture of
//! `device_capabilities_tests`), with an injected probe and timeline source: no network, no env.

use super::*;
use std::sync::atomic::{AtomicU32, Ordering};

const DAY: i64 = 20_000 * 86_400;
const NOW: i64 = DAY + 15 * 3600;

async fn serve(app: App) -> (String, tokio::task::JoinHandle<std::io::Result<()>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(axum::serve(listener, app.router(true, false, None)).into_future());
    (base, task)
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap()
}

async fn get(base: &str, path: &str) -> Value {
    let r = client()
        .get(format!("{base}{path}"))
        .bearer_auth("synthetic-token")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK, "{path}");
    serde_json::from_str(&r.text().await.unwrap()).unwrap()
}

/// The fixture app with `f` applied to its (still unshared) state.
async fn app_with(f: impl FnOnce(&mut Inner)) -> (App, crate::ubus::mock_ubusd::MockUbusd) {
    let (mut app, m) = super::device_capabilities_tests::app().await;
    f(Arc::get_mut(&mut app.inner).expect("unshared"));
    (app, m)
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "u60d-home-reads-{name}-{:016x}",
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[tokio::test]
async fn both_routes_need_authentication() {
    let (app, _m) = app_with(|_| {}).await;
    let (base, task) = serve(app).await;
    for path in ["/v2/latency/direct", "/v2/timeline/today"] {
        for token in [None, Some("wrong")] {
            let mut r = client().get(format!("{base}{path}"));
            if let Some(t) = token {
                r = r.bearer_auth(t);
            }
            assert_eq!(
                r.send().await.unwrap().status(),
                StatusCode::UNAUTHORIZED,
                "{path}"
            );
        }
    }
    task.abort();
}

#[tokio::test]
async fn latency_shape_cache_and_one_shared_probe() {
    let n = Arc::new(AtomicU32::new(0));
    let probes = n.clone();
    let (app, _m) = app_with(move |i| {
        i.latency = latency::Direct::new(Arc::new(move || {
            let n = probes.clone();
            Box::pin(async move {
                n.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(100)).await;
                Some(37)
            })
        }));
    })
    .await;
    let (base, task) = serve(app).await;
    // concurrent callers while the probe runs: one probe answers all
    let (a, b, c) = tokio::join!(
        get(&base, "/v2/latency/direct"),
        get(&base, "/v2/latency/direct"),
        get(&base, "/v2/latency/direct")
    );
    assert_eq!(n.load(Ordering::SeqCst), 1);
    assert_eq!(a["ok"], true);
    let d = &a["data"];
    let keys: Vec<&str> = d.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys.len(), 5, "{d}");
    for k in ["ms", "at", "ok", "age_s", "roaming"] {
        assert!(d.get(k).is_some(), "{k} in {d}");
    }
    assert_eq!(
        (d["ms"].as_u64(), d["ok"].as_bool()),
        (Some(37), Some(true))
    );
    assert!(d["at"].as_i64().is_some());
    // no signal block in the fixture's hub: roaming unknown
    assert_eq!(d["roaming"], Value::Null);
    assert_eq!(
        (b["data"]["ms"].clone(), c["data"]["ms"].clone()),
        (json!(37), json!(37))
    );
    // a later call within the TTL: cached, no new probe
    let again = get(&base, "/v2/latency/direct").await;
    assert_eq!(n.load(Ordering::SeqCst), 1);
    assert_eq!(again["data"]["ms"], 37);
    assert!(again["data"]["age_s"].as_u64().unwrap() < latency::TTL.as_secs());
    task.abort();
}

#[tokio::test]
async fn latency_failure_is_ms_null_ok_false() {
    let (app, _m) = app_with(|_| {}).await; // the fixture's probe answers None
    let (base, task) = serve(app).await;
    let v = get(&base, "/v2/latency/direct").await;
    assert_eq!(v["ok"], true, "the request itself succeeds");
    assert_eq!(
        (v["data"]["ms"].clone(), v["data"]["ok"].clone()),
        (Value::Null, json!(false))
    );
    assert_eq!(v["data"]["age_s"], 0);
    task.abort();
}

#[tokio::test]
async fn timeline_off_and_untrusted_clock() {
    let (app, _m) = app_with(|_| {}).await; // fixture: layer off
    let (base, task) = serve(app).await;
    let v = get(&base, "/v2/timeline/today").await;
    assert_eq!(v, json!({"ok": true, "data": {"available": false}}));
    task.abort();

    let d = tmp("untrusted");
    std::fs::write(
        d.join("events.jsonl"),
        format!("{}\n", json!({"t": NOW - 60, "k": "link", "state": "down"})),
    )
    .unwrap();
    let (dir, alerts) = (d.clone(), d.clone());
    let (app, _m) = app_with(move |i| {
        i.timeline = crate::longterm::today::Source {
            dir: Some(dir),
            alerts,
            clock: || (NOW, false),
        };
    })
    .await;
    let (base, task) = serve(app).await;
    let v = &get(&base, "/v2/timeline/today").await["data"];
    assert_eq!(
        (
            v["available"].clone(),
            v["clock_trusted"].clone(),
            v["events"].clone()
        ),
        (json!(true), json!(false), json!([]))
    );
    task.abort();
    let _ = std::fs::remove_dir_all(&d);
}

#[tokio::test]
async fn timeline_from_fixture_files() {
    let d = tmp("files");
    let mut ev = String::new();
    ev.push_str(&format!(
        "{}\n",
        json!({"t": DAY - 5, "k": "link", "state": "down"})
    ));
    for i in 0..230 {
        let state = if i % 2 == 0 { "down" } else { "up" };
        ev.push_str(&format!(
            "{}\n",
            json!({"t": DAY + 60 + i, "k": "link", "state": state})
        ));
    }
    std::fs::write(d.join("events.jsonl"), ev).unwrap();
    let hours = [
        json!({"k": "hour", "ft": DAY - 3600, "tt": DAY, "sinr_avg": 1.0, "rat": "LTE"}),
        json!({"k": "hour", "ft": DAY + 3600, "tt": DAY + 7200, "sinr_avg": 12.5, "rat": "SA"}),
        json!({"k": "hour", "ft": DAY + 2 * 3600, "tt": DAY + 3 * 3600, "sinr_avg": 4.0, "rat": "NSA"}),
    ];
    let text: String = hours.iter().map(|h| format!("{h}\n")).collect();
    std::fs::write(d.join("hours.jsonl"), text).unwrap();
    let (dir, alerts) = (d.clone(), d.clone());
    let (app, _m) = app_with(move |i| {
        i.timeline = crate::longterm::today::Source {
            dir: Some(dir),
            alerts,
            clock: || (NOW, true),
        };
    })
    .await;
    let (base, task) = serve(app).await;
    let v = &get(&base, "/v2/timeline/today").await["data"];
    assert_eq!(
        (
            v["available"].clone(),
            v["clock_trusted"].clone(),
            v["since"].clone()
        ),
        (json!(true), json!(true), json!(DAY))
    );
    let events = v["events"].as_array().unwrap();
    assert_eq!(events.len(), crate::longterm::today::EVENTS_MAX);
    // the newest 200 kept (yesterday's and the oldest 30 of today dropped), in time order
    assert_eq!(events[0]["t"], DAY + 60 + 30);
    assert_eq!(events.last().unwrap()["t"], DAY + 60 + 229);
    assert!(
        events
            .windows(2)
            .all(|w| w[0]["t"].as_i64() <= w[1]["t"].as_i64())
    );
    let h = v["hours"].as_array().unwrap();
    assert_eq!(h.len(), 2, "yesterday's hour left out");
    assert_eq!(
        (
            h[0]["hour"].clone(),
            h[0]["sinr_avg"].clone(),
            h[0]["rat"].clone()
        ),
        (json!(1), json!(12.5), json!("SA"))
    );
    assert_eq!(
        (h[1]["sinr_avg"].clone(), h[1]["rat"].clone()),
        (json!(4.0), json!("NSA"))
    );
    task.abort();
    let _ = std::fs::remove_dir_all(&d);
}
