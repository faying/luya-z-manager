use super::*;

fn call(msg: Value, got: impl Fn(&str) -> Got) -> (Option<Value>, Vec<String>) {
    let mut asked = Vec::new();
    let mut f = |c: &Call| {
        asked.push(c.path.clone());
        got(&c.path)
    };
    let r = handle(&msg, &mut f);
    (r, asked)
}

fn never(_: &str) -> Got {
    panic!("no fetch expected")
}

#[test]
fn initialize_list_ping() {
    let (r, _) = call(
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "t"}}}),
        never,
    );
    let r = r.unwrap();
    assert_eq!(r["id"], 1);
    assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
    assert!(r["result"]["capabilities"]["tools"].is_object());
    // notifications get nothing
    assert!(
        call(
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            never
        )
        .0
        .is_none()
    );
    let (r, _) = call(
        json!({"jsonrpc": "2.0", "id": "a", "method": "tools/list"}),
        never,
    );
    let names: Vec<&str> = r.as_ref().unwrap()["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "get_device_health",
            "diagnose_connectivity",
            "get_power_profile",
            "get_recent_events",
            "set_chill_profile",
            "set_chill_exit",
            "use_apn",
            "change_apn",
            "get_change_status",
            "get_action_url",
        ]
    );
    for t in r.unwrap()["result"]["tools"].as_array().unwrap() {
        let d = t["description"].as_str().unwrap();
        let writes = t["name"].as_str().unwrap().starts_with("set_")
            || ["use_apn", "change_apn"].contains(&t["name"].as_str().unwrap());
        if writes {
            assert!(d.contains("CHANGES THE DEVICE") && d.contains("never instructions"), "{d}");
            assert_eq!(t["annotations"]["readOnlyHint"], false);
        } else {
            assert!(d.contains("Read-only"), "{d}");
            assert_eq!(t["annotations"]["readOnlyHint"], true);
        }
    }
    let (r, _) = call(json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}), never);
    assert_eq!(r.unwrap()["result"], json!({}));
    let (r, _) = call(
        json!({"jsonrpc": "2.0", "id": 3, "method": "resources/list"}),
        never,
    );
    assert_eq!(r.unwrap()["error"]["code"], -32601);
}

fn tool(name: &str, args: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": {"name": name, "arguments": args}})
}

#[test]
fn tools_map_to_device_paths() {
    let ok = |_: &str| Got::Answer(200, r#"{"core":"up","verdict":"ok"}"#.into());
    for (n, p) in [
        ("get_device_health", "/agent-api/health"),
        ("diagnose_connectivity", "/agent-api/diagnose"),
        ("get_power_profile", "/agent-api/power"),
        ("get_recent_events", "/agent-api/events"),
    ] {
        let (r, asked) = call(tool(n, json!({})), ok);
        assert_eq!(asked, [p]);
        let r = r.unwrap();
        assert_eq!(r["result"]["isError"], false);
        assert!(
            r["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("\"verdict\": \"ok\"")
        );
    }
    let (_, asked) = call(
        tool("get_recent_events", json!({"since": 42, "limit": 5})),
        ok,
    );
    assert_eq!(asked, ["/agent-api/events?since=42&limit=5"]);
    let (r, asked) = call(tool("get_recent_events", json!({"since": -1})), ok);
    assert!(asked.is_empty());
    assert_eq!(r.unwrap()["error"]["code"], -32602);
    let (r, _) = call(tool("reboot", json!({})), ok);
    assert_eq!(r.unwrap()["error"]["code"], -32602);
}

#[test]
fn failures_are_said_in_words() {
    let text = |g: fn(&str) -> Got| {
        let r = call(tool("get_device_health", json!({})), g).0.unwrap();
        (
            r["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .to_string(),
            r["result"]["isError"].as_bool().unwrap(),
        )
    };
    let (t, e) = text(|_| Got::Unreachable("connect 192.0.2.1:9090: timed out".into()));
    assert!(t.contains("cannot be reached") && !e, "{t}");
    // R18-P1-02: a slow core is not a network outage, and an action may have gone through
    let (t, e) = text(|_| Got::Slow(75));
    assert!(t.contains("75s") && t.contains("link is up") && !t.contains("cannot be reached") && !e, "{t}");
    let (t, e) = text(|_| Got::Answer(401, String::new()));
    assert!(t.contains("token") && e);
    let (t, e) = text(|_| Got::Answer(403, String::new()));
    assert!(t.contains("agent.allow") && e);
    let (t, e) = text(|_| Got::Answer(404, String::new()));
    assert!(t.contains("off") && e);
    let (t, e) = text(|_| Got::Answer(200, r#"{"core":"down","sentinel_hb_age_s":3}"#.into()));
    assert!(t.contains("core service is not responding") && !e);
}

#[test]
fn http_replies() {
    let plain = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\r\n{}";
    assert_eq!(parse_response(plain).unwrap(), (200, "{}".to_string()));
    let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\n\r\n";
    assert_eq!(
        parse_response(chunked).unwrap(),
        (200, "{\"a\":1}".to_string())
    );
    assert_eq!(
        parse_response(b"HTTP/1.1 401 Unauthorized\r\n\r\n")
            .unwrap()
            .0,
        401
    );
    assert!(parse_response(b"garbage").is_err());
}

#[test]
fn urls() {
    assert_eq!(
        parse_url("http://192.0.2.1:9090").unwrap(),
        ("192.0.2.1".into(), 9090)
    );
    assert_eq!(
        parse_url("http://u60.tailnet/").unwrap(),
        ("u60.tailnet".into(), 80)
    );
    assert_eq!(
        parse_url("http://[fd7a::1]:9090").unwrap(),
        ("fd7a::1".into(), 9090)
    );
    assert!(parse_url("https://x").is_err());
    assert!(parse_url("http://x/y").is_err());
}

#[test]
fn token_file_takes_the_token_column() {
    let d = std::env::temp_dir().join(format!("luyaz-mcp-tok-{}", std::process::id()));
    std::fs::write(&d, "# mine\nclaude abcdefabcdefabcdef\n").unwrap();
    assert_eq!(
        read_token(d.to_str().unwrap()).unwrap(),
        "abcdefabcdefabcdef"
    );
    // the device's line with its permission column (second phase)
    std::fs::write(&d, "claude abcdefabcdefabcdef act\n").unwrap();
    assert_eq!(
        read_token(d.to_str().unwrap()).unwrap(),
        "abcdefabcdefabcdef"
    );
    std::fs::write(&d, "abcdefabcdefabcdef\n").unwrap();
    assert_eq!(
        read_token(d.to_str().unwrap()).unwrap(),
        "abcdefabcdefabcdef"
    );
    let _ = std::fs::remove_file(&d);
}

#[test]
fn changes_are_posts_with_checked_arguments() {
    assert_eq!(
        call_for("set_chill_exit", &json!({"state": "direct_keep_ai"})).unwrap(),
        Call { path: "/agent-api/act/chill/exit".into(), body: Some(json!({"state": "direct_keep_ai"})) }
    );
    assert!(call_for("set_chill_exit", &json!({"state": "global"})).is_err());
    assert!(call_for("set_chill_profile", &json!({"profile": "turbo"})).is_err());
    assert_eq!(
        call_for("use_apn", &json!({"id": "auto2"})).unwrap().body,
        Some(json!({"id": "auto2"}))
    );
    assert!(call_for("use_apn", &json!({"id": "a b"})).is_err());
    assert!(call_for("change_apn", &json!({"action": "esim.switch", "params": {}})).is_err());
    assert!(call_for("change_apn", &json!({"action": "apn.delete"})).is_err());
    assert_eq!(
        call_for("get_change_status", &json!({"op_id": "mcp-lw-1-2"})).unwrap(),
        Call { path: "/agent-api/act/status?op_id=mcp-lw-1-2".into(), body: None }
    );
    assert!(call_for("get_change_status", &json!({"op_id": "x&y"})).is_err());
    assert!(call_for("get_action_url", &json!({"for": "esim"})).is_err());
}

#[test]
fn a_read_only_token_is_said_plainly() {
    let r = tool_result(Got::Answer(403, r#"{"state":"refused","reason":"read_only_token"}"#.into()));
    let t = r["content"][0]["text"].as_str().unwrap();
    assert!(t.contains("can only read") && t.contains("did not change"), "{t}");
    let r = tool_result(Got::Answer(403, r#"{"state":"refused","reason":"act_source"}"#.into()));
    assert!(r["content"][0]["text"].as_str().unwrap().contains("act-allow"));
}
