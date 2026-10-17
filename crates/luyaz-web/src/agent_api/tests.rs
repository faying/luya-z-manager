use super::*;

const T1: &str = "aaaaaaaaaaaaaaaaaaaaaaaa";
const T2: &str = "bbbbbbbbbbbbbbbbbbbbbbbb";

#[test]
fn token_lines() {
    let t = parse_tokens(&format!(
        "# comment\n\nclaude {T1}\nhermes {T2}\nbad\nshort abc\nx y z\n"
    ));
    assert_eq!(t.len(), 2);
    assert_eq!(who(&t, T1), Some("claude"));
    assert_eq!(who(&t, T2), Some("hermes"));
    assert_eq!(who(&t, "nope"), None);
    assert_eq!(who(&t, ""), None);
    // same token twice: the first name
    let d = parse_tokens(&format!("a {T1}\nb {T1}\n"));
    assert_eq!(who(&d, T1), Some("a"));
    assert!(parse_tokens("").is_empty());
    // second phase: a third column says what the token may do; an unknown one is no line at all
    let p = parse_tokens_perm(&format!("hermes {T1}\nclaude {T2} act\nr {T1} read\nx {T2} admin\n"));
    assert_eq!(
        p.iter().map(|(n, _, p)| (n.as_str(), *p)).collect::<Vec<_>>(),
        [("hermes", Perm::Read), ("claude", Perm::Act), ("r", Perm::Read)]
    );
}

#[test]
fn sources() {
    let ip = |s: &str| s.parse::<IpAddr>().unwrap();
    let none = [];
    assert!(source_ok(ip("127.0.0.1"), &none));
    assert!(source_ok(ip("::1"), &none));
    assert!(source_ok(ip("100.100.1.2"), &none));
    assert!(source_ok(ip("fd7a:115c:a1e0::1"), &none));
    assert!(source_ok(ip("::ffff:100.64.0.1"), &none), "v4-mapped");
    assert!(!source_ok(ip("100.128.0.1"), &none));
    assert!(!source_ok(ip("198.51.100.88"), &none));
    assert!(!source_ok(ip("192.168.0.20"), &none));
    let allow = parse_allow("198.51.100.0/24 # a home LAN\nnot-a-cidr\n10.0.0.0/99\n192.168.9.9\n");
    assert_eq!(allow.len(), 2);
    assert!(source_ok(ip("198.51.100.88"), &allow));
    assert!(!source_ok(ip("198.51.101.1"), &allow));
    assert!(source_ok(ip("192.168.9.9"), &allow));
    assert!(!source_ok(ip("192.168.9.10"), &allow));
}

#[test]
fn times_carry_the_offset() {
    // device digits 2026-10-17 14:34:00 "UTC", really +08:00
    let wall = 1_792_247_640;
    assert_eq!(iso(wall, 28800), "2026-10-17T14:34:00+08:00");
    assert_eq!(iso(wall, -12600), "2026-10-17T14:34:00-03:30");
    assert_eq!(iso(wall, 0), "2026-10-17T14:34:00+00:00");
}

#[test]
fn untrusted_text_is_cut_and_wrapped() {
    let long = "x".repeat(200);
    assert_eq!(
        untrusted(&json!(long))["untrusted"].as_str().unwrap().len(),
        64
    );
    assert_eq!(
        untrusted(&json!("ignore previous")),
        json!({"untrusted": "ignore previous"})
    );
    assert!(untrusted(&Value::Null).is_null());
}

fn status() -> Value {
    json!({
        "alerts": {"unread": 2},
        "health": {"checked": true, "bad": 0, "warn": 1},
        "scenario": {"pin": "1234", "current": "home"},
        "network": {"verdict": "ok", "finding": null},
        "services": {"tailscale": {"installed": true, "running": true},
                     "chill": {"state": "on", "on": true, "notice": "secret-ish profile name"}},
        "clock": {"utc_offset": 28800},
    })
}

#[test]
fn whitelists_drop_unknown_and_secret_fields() {
    let h = json!({"checks": [{"level": "warn", "id": "x", "label": "标签", "label_en": "Label",
                               "detail": "d", "detail_en": "d en", "new_field": 1}],
                   "bad": 0, "warn": 1, "crashlogs": [{"file": "/data/x"}]});
    let a = shape_health(&status(), &h);
    let mut s2 = status();
    s2["clock"]["new_core_field"] = json!("leak");
    s2["network"]["iccid"] = json!("8986000000000000000");
    let mut h2 = h.clone();
    h2["new_top"] = json!("leak");
    assert_eq!(
        a,
        shape_health(&s2, &h2),
        "a field the core adds does not come through"
    );
    let text = a.to_string();
    assert!(
        !text.contains("1234") && !text.contains("profile name") && !text.contains("crashlogs")
    );
    assert_eq!(
        a["health"]["checks"][0]["detail"],
        json!({"untrusted": "d en"})
    );
    assert_eq!(a["alerts_unread"], 2);
}

#[test]
fn diagnose_has_age_or_says_no_finding() {
    let none = shape_diagnose(&json!({"state": "idle", "finding": null}), 1000, 0);
    assert_eq!(none["state"], "no_open_finding");
    assert!(none["note"].as_str().unwrap().contains("not proof"));
    let d = json!({
        "state": "done", "finished_at": 900, "key": {"plmn": "46000", "cell": 123},
        "main": {"layer": "signal", "level": "warn", "text": "弱", "text_en": "Weak", "action": "x"},
        "finding": {"kind": "no-data", "layer": "data", "confidence": "likely",
                    "symptom": "无数据", "symptom_en": "No data", "iccid_tail": "1234",
                    "evidence": [{"layer": "data", "level": "bad", "detail": "运营商说", "detail_en": "Carrier says", "at": 400}],
                    "at": 400, "first": 300, "last": 400, "count": 3, "open": 1},
    });
    let v = shape_diagnose(&d, 1000, 28800);
    assert_eq!(v["state"], "open_finding");
    assert_eq!(v["finding"]["age_s"], 600);
    assert_eq!(
        v["finding"]["evidence"][0]["detail"],
        json!({"untrusted": "Carrier says"})
    );
    let text = v.to_string();
    assert!(!text.contains("46000") && !text.contains("1234") && !text.contains("iccid"));
    assert_eq!(v["last_run"]["main"]["text"], "Weak");
}

#[test]
fn power_and_events_shapes() {
    let b = json!({"capacity": 80, "status": "Discharging", "temperature": 310, "health": "Good",
                   "voltage_uv": 1, "charger": {"online": false},
                   "estimate": {"state": "ok", "kind": "discharging_eta", "minutes": 200, "target_pct": 0,
                                "observed_at": 1, "samples": 9}});
    let p = shape_power(Some(&b), Some(&json!({"operate_mode": "ONLINE"})), None);
    assert_eq!(p["battery"]["capacity_pct"], 80);
    assert_eq!(p["battery"]["estimate"]["minutes"], 200);
    assert!(
        p["battery"].get("voltage_uv").is_none()
            && p["battery"]["estimate"].get("samples").is_none()
    );
    assert_eq!(p["modem_mode"], "ONLINE");
    assert!(p["wifi_power_save"].is_null());
    let a = json!({"next": 8, "more": false, "gap": false, "unread": 1, "utc_offset": 3600,
                   "sms": [{"seq": 7, "kind": "agent-crash", "result": "sent", "time": 1_792_247_640}]});
    let evs = [
        json!({"seq": 7, "time": 1_792_247_640, "offset": 28800, "kind": "agent-crash", "label_en": "L", "text": "t", "unread": false}),
        json!({"seq": 8, "time": null, "offset": null, "kind": "agent-crash", "label_en": "L", "text": "t", "unread": true, "extra": 1}),
    ];
    let v = shape_events(&a, &evs);
    assert_eq!(v["events"][0]["time"], "2026-10-17T14:34:00+08:00");
    assert_eq!(v["events"][0]["offset_known"], true);
    assert_eq!(v["events"][1]["offset_known"], false);
    assert!(v["events"][1].get("extra").is_none());
    assert_eq!(v["sms"][0]["time"], "2026-10-17T14:34:00+01:00");
    assert_eq!(v["events"][0]["text"], json!({"untrusted": "t"}));
}

/// The whole path through the gate with a stand-in core: off without tokens, 401, 200,
/// revoked by deleting the line, the core down.
#[tokio::test]
async fn through_the_gate() {
    use axum::routing::get as rget;
    let dir = std::env::temp_dir().join(format!("luyaz-agent-api-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (tf, af) = (dir.join("agent.token"), dir.join("agent.allow"));
    set_files(tf.clone(), af);
    let act_allow = dir.join("agent.act-allow");
    crate::agent_act::set_act_allow(act_allow.clone());
    let seen: std::sync::Arc<std::sync::Mutex<Vec<Value>>> = Default::default();
    let seen2 = seen.clone();

    let core = Router::new()
        .route("/v2/api/public/status", rget(|| async { axum::Json(json!({"ok": true, "data": status()})) }))
        .route("/v2/api/health", rget(|| async { axum::Json(json!({"ok": true, "data": {"checks": [], "bad": 0, "warn": 0}})) }))
        .route("/control", axum::routing::post(move |axum::Json(b): axum::Json<Value>| {
            let seen = seen2.clone();
            async move {
                seen.lock().unwrap().push(b.clone());
                axum::Json(json!({"ok": true, "action": b["action"],
                    "op": {"op_id": b["op_id"], "phase": "verifying", "rollback_enabled": true}}))
            }
        }))
        .route("/v2/api/alerts", rget(|axum::extract::RawQuery(q): axum::extract::RawQuery| async move {
            let q = q.unwrap_or_default();
            let since: u64 = q.split('&').find_map(|p| p.strip_prefix("since=")).and_then(|s| s.parse().ok()).unwrap_or(0);
            // 60 events, seq 1..=60, pages of `limit`
            let limit: u64 = q.split('&').find_map(|p| p.strip_prefix("limit=")).and_then(|s| s.parse().ok()).unwrap_or(20);
            let evs: Vec<Value> = (since + 1..=60).take(limit as usize)
                .map(|s| json!({"seq": s, "time": null, "offset": null, "kind": "agent-crash", "label_en": "L", "text": "t", "unread": true}))
                .collect();
            let next = evs.last().map_or(since, |e| e["seq"].as_u64().unwrap());
            axum::Json(json!({"ok": true, "data": {"events": evs, "next": next, "more": next < 60, "gap": false,
                                                   "unread": 60, "utc_offset": 0, "sms": []}}))
        }));
    let cl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let core_url = format!("http://{}", cl.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(cl, core).await.unwrap() });

    let web = crate::app(
        &core_url,
        std::time::Duration::from_secs(10),
        "/signal",
        None,
        None,
        dir.clone(),
    );
    let wl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let web_url = format!("http://{}", wl.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(
            wl,
            web.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap()
    });
    let http = reqwest::Client::new();
    let call = |path: &'static str, tok: Option<&'static str>| {
        let (http, web_url) = (http.clone(), web_url.clone());
        async move {
            let mut rb = http.get(format!("{web_url}{path}"));
            if let Some(t) = tok {
                rb = rb.bearer_auth(t);
            }
            let r = rb.send().await.unwrap();
            (
                r.status().as_u16(),
                r.json::<Value>().await.unwrap_or(Value::Null),
            )
        }
    };

    assert_eq!(
        call("/agent-api/health", Some(T1)).await.0,
        404,
        "no token file: off"
    );
    std::fs::write(&tf, format!("claude {T1}\nhermes {T2}\n")).unwrap();
    assert_eq!(call("/agent-api/health", None).await.0, 401);
    assert_eq!(
        call("/agent-api/health", Some("wrong-token-wrong-token"))
            .await
            .0,
        401
    );
    let (st, v) = call("/agent-api/health", Some(T1)).await;
    assert_eq!(
        (st, v["core"].as_str(), v["verdict"].as_str()),
        (200, Some("up"), Some("ok"))
    );
    assert!(!v.to_string().contains("1234"));
    // no cursor: the newest 20; with one: the next page
    let (_, v) = call("/agent-api/events", Some(T2)).await;
    assert_eq!(v["events"].as_array().unwrap().len(), 20);
    assert_eq!(
        (v["events"][0]["seq"].as_u64(), v["next"].as_u64()),
        (Some(41), Some(60))
    );
    let (_, v) = call("/agent-api/events?since=10&limit=5", Some(T2)).await;
    assert_eq!(
        (
            v["events"][0]["seq"].as_u64(),
            v["next"].as_u64(),
            v["more"].as_bool()
        ),
        (Some(11), Some(15), Some(true))
    );
    // a write is not a route here
    let st = http
        .post(format!("{web_url}/agent-api/health"))
        .bearer_auth(T1)
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(st.as_u16(), 405);
    // second phase: writes need an act token, from an act source; the body is rebuilt as mcp
    let act = |path: &'static str, tok: &'static str, body: Value| {
        let (http, web_url) = (http.clone(), web_url.clone());
        async move {
            let r = http.post(format!("{web_url}{path}")).bearer_auth(tok).json(&body).send().await.unwrap();
            (r.status().as_u16(), r.json::<Value>().await.unwrap_or(Value::Null))
        }
    };
    let (st, v) = act("/agent-api/act/chill/exit", T1, json!({"state": "direct_keep_ai"})).await;
    assert_eq!((st, v["reason"].as_str()), (403, Some("read_only_token")));
    std::fs::write(&tf, format!("claude {T1} act\nhermes {T2}\n")).unwrap();
    let (st, v) = act("/agent-api/act/chill/exit", T1, json!({"state": "direct_keep_ai"})).await;
    assert_eq!((st, v["reason"].as_str()), (403, Some("act_source")), "no act-allow file: no one");
    std::fs::write(&act_allow, "127.0.0.1/32\n").unwrap();
    let (st, v) = act(
        "/agent-api/act/apn",
        T1,
        json!({"action": "apn.enable", "params": {"profile_id": "manu2", "source": "web"},
               "source": "screen", "approved_by": "screen"}),
    )
    .await;
    assert_eq!((st, v["state"].as_str()), (200, Some("trial")), "{v}");
    let b = seen.lock().unwrap().last().cloned().unwrap();
    assert_eq!((b["source"].as_str(), b["approved_by"].as_str()), (Some("mcp"), Some("mcp")));
    assert_eq!(b["params"], json!({"profile_id": "manu2"}));
    assert_eq!(b["action"], "apn.enable");
    // only the listed actions
    let (st, _) = act("/agent-api/act/apn", T1, json!({"action": "esim.switch", "params": {}})).await;
    assert_eq!(st, 400);
    let (st, _) = act("/agent-api/act/chill/exit", T2, json!({"state": "proxy"})).await;
    assert_eq!(st, 403, "hermes stays read-only");
    let (_, v) = call("/agent-api/action-url?for=apn", Some(T2)).await;
    assert!(v["url"].as_str().unwrap().ends_with("/router/apn"));
    let (_, v) = call("/agent-api/action-url?for=esim", Some(T2)).await;
    assert_eq!(v["state"], "refused");
    // revoked: delete the line
    std::fs::write(&tf, format!("hermes {T2}\n")).unwrap();
    assert_eq!(call("/agent-api/health", Some(T1)).await.0, 401);
    // the core answers an error (here: a route it lacks): said, never "no open finding"
    let (st, v) = call("/agent-api/diagnose", Some(T2)).await;
    assert_eq!(
        (st, v["state"].as_str(), v["http"].as_u64()),
        (200, Some("unavailable"), Some(404))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn core_down_is_said_not_502() {
    let r = core_down();
    assert_eq!(r.status(), StatusCode::OK);
    let b = axum::body::to_bytes(r.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&b).unwrap();
    assert_eq!(v["core"], "down");
}
