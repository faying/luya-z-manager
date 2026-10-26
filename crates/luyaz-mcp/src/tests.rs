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
            "get_network_health",
            "get_power_profile",
            "get_recent_events",
            "get_firmware_capabilities",
            "explain_recovery_action",
            "get_no_service_history",
            "get_incidents",
            "get_sim_identity",
            "set_chill_profile",
            "set_chill_exit",
            "use_apn",
            "change_apn",
            "change_band_or_cell",
            "accept_regress_level",
            "get_change_status",
            "get_action_url",
            "list_devices",
        ]
    );
    for t in r.unwrap()["result"]["tools"].as_array().unwrap() {
        let d = t["description"].as_str().unwrap();
        let writes = t["name"].as_str().unwrap().starts_with("set_")
            || ["use_apn", "change_apn", "change_band_or_cell", "accept_regress_level"]
                .contains(&t["name"].as_str().unwrap());
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
        ("get_network_health", "/agent-api/network-health"),
        ("get_power_profile", "/agent-api/power"),
        ("get_sim_identity", "/agent-api/sim"),
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
    let (_, asked) = call(tool("get_no_service_history", json!({"limit": 3})), ok);
    assert_eq!(asked, ["/agent-api/no-service-history?limit=3"]);
    let (_, asked) = call(tool("get_no_service_history", json!({})), ok);
    assert_eq!(asked, ["/agent-api/no-service-history"]);
    let (r, asked) = call(tool("get_no_service_history", json!({"limit": 0})), ok);
    assert!(asked.is_empty());
    assert_eq!(r.unwrap()["error"]["code"], -32602);
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
    // D-g: link changes only with the owner's instruction, passed on as `instruction: owner`
    assert_eq!(
        call_for(
            "change_band_or_cell",
            &json!({"action": "band.set_lte", "params": {"bands": "1,3"}, "owner_instruction": true})
        )
        .unwrap(),
        Call {
            path: "/agent-api/act/link".into(),
            body: Some(json!({"action": "band.set_lte", "params": {"bands": "1,3"}, "instruction": "owner"}))
        }
    );
    for args in [
        json!({"action": "band.set_lte", "params": {"bands": "1,3"}}),
        json!({"action": "band.set_lte", "params": {"bands": "1,3"}, "owner_instruction": false}),
        json!({"action": "network.set_mode", "params": {}, "owner_instruction": true}),
    ] {
        assert!(call_for("change_band_or_cell", &args).is_err(), "{args}");
    }
    assert_eq!(
        call_for("get_change_status", &json!({"op_id": "mcp-lw-1-2"})).unwrap(),
        Call { path: "/agent-api/act/status?op_id=mcp-lw-1-2".into(), body: None }
    );
    assert!(call_for("get_change_status", &json!({"op_id": "x&y"})).is_err());
    // regress: reads with and without an id; accepting only on the owner's instruction
    assert_eq!(call_for("get_incidents", &json!({})).unwrap().path, "/agent-api/incidents");
    assert_eq!(
        call_for("get_incidents", &json!({"id": "mem-abs-1792800000"})).unwrap().path,
        "/agent-api/incidents/mem-abs-1792800000"
    );
    assert!(call_for("get_incidents", &json!({"id": "../core.env"})).is_err());
    assert!(call_for("get_incidents", &json!({"id": "Q1"})).is_err());
    assert_eq!(
        call_for("accept_regress_level", &json!({"kind": "regress-mem", "owner_instruction": true})).unwrap(),
        Call {
            path: "/agent-api/act/regress-accept".into(),
            body: Some(json!({"kind": "regress-mem", "instruction": "owner"}))
        }
    );
    assert!(call_for("accept_regress_level", &json!({"kind": "regress-mem"})).is_err());
    assert!(call_for("accept_regress_level", &json!({"kind": "regress-mem-abs", "owner_instruction": true})).is_err());
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

#[test]
fn pin_parses_hex_only() {
    assert_eq!(parse_pin(None).unwrap(), None);
    assert_eq!(parse_pin(Some("  ")).unwrap(), None);
    assert_eq!(
        parse_pin(Some(" A1b2C3 ")).unwrap().as_deref(),
        Some("a1b2c3")
    );
    assert!(parse_pin(Some("not-an-id")).is_err());
}

fn health_call(pin: Option<&str>, body: &'static str) -> Value {
    let mut f = |_: &Call| Got::Answer(200, body.into());
    handle_pinned(
        &json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call",
                "params": {"name": "get_device_health", "arguments": {}}}),
        pin,
        &mut f,
    )
    .unwrap()["result"]
        .clone()
}

#[test]
fn pinned_device_must_answer_as_itself() {
    // same id (case-insensitive): passed on
    let r = health_call(Some("abc123"), r#"{"device_id":"ABC123","verdict":"ok"}"#);
    assert_eq!(r["isError"], false);
    assert!(r["content"][0]["text"].as_str().unwrap().contains("verdict"));
    // another device: refused, its data not shown
    let r = health_call(Some("abc123"), r#"{"device_id":"def456","verdict":"secret"}"#);
    assert_eq!(r["isError"], true);
    let t = r["content"][0]["text"].as_str().unwrap();
    assert!(t.contains("another device") && t.contains("def456") && !t.contains("secret"));
    // pinned, no id (old admin web): refused
    let r = health_call(Some("abc123"), r#"{"verdict":"secret"}"#);
    assert_eq!(r["isError"], true);
    assert!(!r["content"][0]["text"].as_str().unwrap().contains("secret"));
    // not pinned: no check
    let r = health_call(None, r#"{"device_id":"def456","verdict":"ok"}"#);
    assert_eq!(r["isError"], false);
}

#[test]
fn pin_leaves_non_answers_alone() {
    assert!(matches!(
        check_identity(Some("ab"), Got::Answer(401, String::new())),
        Got::Answer(401, _)
    ));
    assert!(matches!(check_identity(Some("ab"), Got::Slow(8)), Got::Slow(8)));
}

// ---- 多台: the device list ----

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("luyaz-mcp-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

const LIST: &str = "# my routers\n\nhome 192.0.2.10 ~/.config/luyaz/hermes.token abcdef012345\ntravel http://100.64.0.9:9090 travel.token   # the one abroad\nlab [fd7a:115c::1]:9091 /etc/lab.token\nold http://10.1.1.1 x.token\n";

#[test]
fn device_list_parses_good_lines_and_comments() {
    let l = parse_devices(LIST, Path::new("/cfg"), Path::new("/home/u")).unwrap();
    type Row<'a> = (&'a str, &'a str, u16, &'a str, Option<&'a str>, usize);
    let got: Vec<Row> = l
        .iter()
        .map(|d| (d.name.as_str(), d.host.as_str(), d.port, d.token_file.to_str().unwrap(), d.id.as_deref(), d.line))
        .collect();
    assert_eq!(
        got,
        [
            ("home", "192.0.2.10", 9090, "/home/u/.config/luyaz/hermes.token", Some("abcdef012345"), 3),
            ("travel", "100.64.0.9", 9090, "/cfg/travel.token", None, 4),
            ("lab", "fd7a:115c::1", 9091, "/etc/lab.token", None, 5),
            ("old", "10.1.1.1", 80, "/cfg/x.token", None, 6),
        ]
    );
}

/// D6: one bad line or a repeated name refuses the whole list, naming the line.
#[test]
fn device_list_is_strict() {
    let p = |t: &str| parse_devices(t, Path::new("/"), Path::new("/h"));
    for (text, want) in [
        ("a 10.0.0.1 t\nb 10.0.0.2\n", "line 2"),
        ("a 10.0.0.1 t\na 10.0.0.2 t\n", "already used on line 1"),
        ("a 10.0.0.1 t zz-not-hex\n", "line 1: device id"),
        ("a https://10.0.0.1 t\n", "line 1: address"),
        ("a 10.0.0.1/x t\n", "line 1: address"),
        ("a/b 10.0.0.1 t\n", "line 1: name"),
        ("a 10.0.0.1 t abc extra\n", "line 1: expected"),
        ("# nothing\n\n", "no device lines"),
    ] {
        let e = p(text).unwrap_err();
        assert!(e.contains(want), "{text:?}: {e}");
    }
}

#[test]
fn no_list_file_means_the_single_device() {
    let d = tmpdir("setup");
    let f = d.join("devices");
    let single = || Setup::Single { url: "http://192.0.2.1:9090".into(), pin: None };
    assert!(matches!(load_setup(&f, single), Setup::Single { .. }));
    std::fs::write(&f, "a 10.0.0.1 t\nbroken\n").unwrap();
    match load_setup(&f, single) {
        Setup::Broken(why) => assert!(why.contains("line 2") && why.contains("No device is used"), "{why}"),
        other => panic!("{other:?}"),
    }
    std::fs::write(&f, "a 10.0.0.1 t\n").unwrap();
    assert!(matches!(load_setup(&f, single), Setup::List(l) if l.len() == 1));
    let _ = std::fs::remove_dir_all(&d);
}

fn list_setup() -> Setup {
    Setup::List(parse_devices(LIST, Path::new("/cfg"), Path::new("/home/u")).unwrap())
}

/// One call on a setup; Vec of (device name, path) the fetch saw.
fn call_on(setup: &Setup, msg: Value, got: impl Fn(&str, &str) -> Got) -> (Value, Vec<(String, String)>) {
    let mut asked = Vec::new();
    let mut f = |d: Option<&Device>, c: &Call| {
        let n = d.map_or("-".to_string(), |d| d.name.clone());
        asked.push((n.clone(), c.path.clone()));
        got(&n, &c.path)
    };
    let r = handle_setup(&msg, setup, &mut f).unwrap();
    (r["result"].clone(), asked)
}

fn text(r: &Value) -> &str {
    r["content"][0]["text"].as_str().unwrap()
}

#[test]
fn device_argument_picks_the_device() {
    let s = list_setup();
    let ok = |n: &str, _: &str| Got::Answer(200, format!(r#"{{"device_id":"{}","verdict":"ok"}}"#, if n == "home" { "abcdef012345" } else { "0000000000aa" }));
    let (r, asked) = call_on(&s, tool("get_device_health", json!({"device": "travel"})), ok);
    assert_eq!(asked, [("travel".to_string(), "/agent-api/health".to_string())]);
    let v: Value = serde_json::from_str(text(&r)).unwrap();
    assert_eq!((v["device"].as_str(), r["isError"].as_bool()), (Some("travel"), Some(false)));
    assert!(v["identity"].as_str().unwrap().starts_with("unconfirmed") && v["identity"].as_str().unwrap().contains("enroll travel"));
    let (r, _) = call_on(&s, tool("get_recent_events", json!({"device": "home", "since": 3})), ok);
    let v: Value = serde_json::from_str(text(&r)).unwrap();
    assert_eq!((v["device"].as_str(), v["identity"].as_str()), (Some("home"), Some("confirmed")));
    // several devices, none named; an unknown one: said, with the names, nothing asked
    for (args, code) in [(json!({}), "device_required"), (json!({"device": "moon"}), "unknown_device")] {
        let (r, asked) = call_on(&s, tool("get_device_health", args), ok);
        assert!(asked.is_empty());
        assert_eq!((r["isError"].as_bool(), r["structuredContent"]["error"].as_str()), (Some(true), Some(code)));
        assert!(text(&r).contains("home, travel, lab, old"), "{}", text(&r));
    }
    // one device: may be left out
    let one = Setup::List(parse_devices("solo 10.0.0.1 t\n", Path::new("/"), Path::new("/")).unwrap());
    let (r, asked) = call_on(&one, tool("get_power_profile", json!({})), ok);
    assert_eq!((asked[0].0.as_str(), r["isError"].as_bool()), ("solo", Some(false)));
    // other answers are prefixed with the name
    let (r, _) = call_on(&s, tool("get_device_health", json!({"device": "lab"})), |_, _| Got::Unreachable("refused".into()));
    assert!(text(&r).starts_with("[lab] The device cannot be reached"), "{}", text(&r));
}

#[test]
fn enrolled_device_answering_as_another_is_refused() {
    let s = list_setup();
    let (r, _) = call_on(&s, tool("get_device_health", json!({"device": "home"})), |_, _| Got::Answer(200, r#"{"device_id":"999999999999","verdict":"secret"}"#.into()));
    assert_eq!(r["isError"], true);
    assert_eq!(r["structuredContent"], json!({"error": "wrong_device", "device": "home", "device_id": "999999999999"}));
    assert!(text(&r).contains("luyaz-mcp enroll home") && !text(&r).contains("secret"));
    let (r, _) = call_on(&s, tool("get_device_health", json!({"device": "home"})), |_, _| Got::Answer(200, r#"{"verdict":"secret"}"#.into()));
    assert_eq!(r["structuredContent"]["error"], "no_device_id");
    // a change to an enrolled device: its identity is read first, a wrong one gets no POST
    let (r, asked) = call_on(&s, tool("set_chill_exit", json!({"device": "home", "state": "proxy"})), |_, _| Got::Answer(200, r#"{"device_id":"999999999999"}"#.into()));
    assert_eq!(asked, [("home".to_string(), "/agent-api/health".to_string())]);
    assert_eq!(r["structuredContent"]["error"], "wrong_device");
    let (r, asked) = call_on(&s, tool("set_chill_exit", json!({"device": "home", "state": "proxy"})), |_, _| Got::Answer(200, r#"{"device_id":"abcdef012345","state":"trial"}"#.into()));
    assert_eq!(asked.iter().map(|a| a.1.as_str()).collect::<Vec<_>>(), ["/agent-api/health", "/agent-api/act/chill/exit"]);
    assert_eq!(r["isError"], false);
}

#[test]
fn list_devices_gives_names_and_addresses_only() {
    let (r, asked) = call_on(&list_setup(), tool("list_devices", json!({})), |_, _| panic!("no fetch"));
    assert!(asked.is_empty());
    let v: Value = serde_json::from_str(text(&r)).unwrap();
    assert_eq!(v["devices"][0], json!({"name": "home", "address": "192.0.2.10", "enrolled": true, "device_id": "abcdef012345"}));
    assert_eq!(v["devices"][1], json!({"name": "travel", "address": "http://100.64.0.9:9090", "enrolled": false}));
    assert!(!text(&r).contains("token"), "{}", text(&r));
}

#[test]
fn broken_list_is_said_by_every_tool() {
    let s = Setup::Broken("The device list /x has a problem on line 2: bad. No device is used until it is fixed.".into());
    for (n, a) in [("get_device_health", json!({})), ("list_devices", json!({})), ("use_apn", json!({"id": "auto", "device": "home"}))] {
        let (r, asked) = call_on(&s, tool(n, a), |_, _| panic!("no fetch"));
        assert!(asked.is_empty());
        assert_eq!(r["structuredContent"]["error"], "devices_file", "{n}");
        assert!(text(&r).contains("line 2"));
    }
}

#[test]
fn single_device_refuses_a_device_name_and_keeps_its_shape() {
    let s = Setup::Single { url: "http://192.0.2.1:9090".into(), pin: None };
    let (r, asked) = call_on(&s, tool("get_device_health", json!({"device": "home"})), |_, _| panic!("no fetch"));
    assert!(asked.is_empty());
    assert_eq!(r["structuredContent"]["error"], "unknown_device");
    let (r, _) = call_on(&s, tool("get_device_health", json!({})), |_, _| Got::Answer(200, r#"{"verdict":"ok"}"#.into()));
    let v: Value = serde_json::from_str(text(&r)).unwrap();
    assert_eq!(v, json!({"verdict": "ok"}), "no name added without a list");
}

#[test]
fn every_tool_takes_device() {
    for t in tools().as_array().unwrap() {
        if t["name"] != "list_devices" {
            assert_eq!(t["inputSchema"]["properties"]["device"]["type"], "string", "{}", t["name"]);
        }
    }
}

/// R5: enroll writes the fourth column of its line only; comments and other lines stay.
#[test]
fn write_id_keeps_the_rest() {
    let d = tmpdir("write");
    let f = d.join("devices");
    std::fs::write(&f, LIST).unwrap();
    assert_eq!(write_id(&f, "travel", "00112233aabb").unwrap(), None);
    assert_eq!(write_id(&f, "home", "ffffffffffff").unwrap().as_deref(), Some("abcdef012345"));
    let t = std::fs::read_to_string(&f).unwrap();
    assert_eq!(
        t,
        LIST.replace("travel.token   # the one abroad", "travel.token 00112233aabb # the one abroad")
            .replace("hermes.token abcdef012345", "hermes.token ffffffffffff")
    );
    assert!(write_id(&f, "moon", "00").is_err());
    let _ = std::fs::remove_dir_all(&d);
}

/// O2: two enrolls at once (separate opens of the lock, as two processes) both stay.
#[test]
fn concurrent_enrolls_both_stay() {
    let d = tmpdir("race");
    let f = d.join("devices");
    let names: Vec<String> = (0..8).map(|i| format!("d{i}")).collect();
    std::fs::write(&f, names.iter().map(|n| format!("{n} 10.0.0.1 t\n")).collect::<String>()).unwrap();
    let hs: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let (f, n) = (f.clone(), n.clone());
            std::thread::spawn(move || {
                for k in 0..20 {
                    write_id(&f, &n, &format!("{i:02x}{k:010x}")).unwrap();
                }
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
    let l = parse_devices(&std::fs::read_to_string(&f).unwrap(), &d, &d).unwrap();
    for (i, dev) in l.iter().enumerate() {
        assert_eq!(dev.id.as_deref(), Some(format!("{i:02x}{:010x}", 19).as_str()), "{}", dev.name);
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn host_header_carries_the_port() {
    assert_eq!(host_header("192.168.0.1", 9090), "192.168.0.1:9090");
    assert_eq!(host_header("u60.local", 80), "u60.local");
    assert_eq!(host_header("fd00::1", 9090), "[fd00::1]:9090");
}

// ---- R18-P1-07: get_firmware_capabilities, explain_recovery_action ----

const CAPS: &str = r#"{"state":"ok","device_id":"a1b2c3d4e5f6","schema_version":1,
  "identity":{"identity_status":"partial","model":"MU5250"},
  "context_coverage":{"sim_epoch":{"authoritative":false,"reason":"unverified"}},
  "assessments":[{"capability_id":"usb.mode.set","status":"unknown","reason":"no_evidence",
    "freshness":{"fresh":false,"age_seconds":null,"max_age_seconds":300}}],
  "truncated":false,"limitations":["passive report: reading it ran no new probe"]}"#;

const ACTIONS: &str = r#"{"state":"ok","device_id":"a1b2c3d4e5f6","schema_version":1,
  "allow_list":{"actions":[],"empty":true,"note":"empty"},"loop_missing_conditions":[],
  "actions":[
   {"action":"data.reconnect","kind":"loop_only","rollback":{"capability":"unsupported","on_loop_execution":false},
    "approval":{"person":"loop","agent_mcp":null,"auto_allow_list":false}},
   {"action":"apn.use","kind":"transaction","rollback":{"capability":"unsupported","on_agent_write":true},
    "approval":{"person":"screen or web","agent_mcp":{"layer":"apn"},"auto_allow_list":false}}]}"#;

#[test]
fn new_tools_are_read_only_with_strict_schemas() {
    let (r, _) = call(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}), never);
    let r = r.unwrap();
    for n in ["get_firmware_capabilities", "explain_recovery_action"] {
        let t = r["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == n).unwrap();
        assert_eq!(t["annotations"]["readOnlyHint"], true, "{n}");
        assert_eq!(t["inputSchema"]["additionalProperties"], false, "{n}");
        assert!(t["inputSchema"]["properties"]["device"].is_object(), "{n}");
        assert!(t["inputSchema"].get("required").is_none(), "{n}");
    }
    for (n, a) in [("get_firmware_capabilities", json!({"capability": "usb."})), ("explain_recovery_action", json!({"action": "data.reconnect"}))] {
        let c = call_for(n, &a).unwrap();
        assert!(c.body.is_none(), "{n} must be a GET");
    }
}

#[test]
fn capabilities_path_filter_and_bad_arguments() {
    let ok = |_: &str| Got::Answer(200, CAPS.into());
    let (_, asked) = call(tool("get_firmware_capabilities", json!({})), ok);
    assert_eq!(asked, ["/agent-api/capabilities"]);
    let (r, asked) = call(tool("get_firmware_capabilities", json!({"capability": "usb."})), ok);
    assert_eq!(asked, ["/agent-api/capabilities?capability=usb."]);
    let t = r.unwrap()["result"]["content"][0]["text"].as_str().unwrap().to_string();
    // unknown, age, coverage and device_id pass through as the device said
    for k in ["\"unknown\"", "\"age_seconds\": null", "\"authoritative\": false", "a1b2c3d4e5f6", "gateway_cache_age_s"] {
        assert!(t.contains(k), "{k} in {t}");
    }
    for bad in [json!({"capability": "a&b=c"}), json!({"capability": 3}), json!({"capability": ""})] {
        let (r, asked) = call(tool("get_firmware_capabilities", bad), ok);
        assert!(asked.is_empty());
        assert_eq!(r.unwrap()["error"]["code"], -32602);
    }
    let (r, asked) = call(tool("explain_recovery_action", json!({"action": "x y"})), ok);
    assert!(asked.is_empty());
    assert_eq!(r.unwrap()["error"]["code"], -32602);
}

#[test]
fn explain_picks_one_action_or_lists_them() {
    let ok = |_: &str| Got::Answer(200, ACTIONS.into());
    let (r, asked) = call(tool("explain_recovery_action", json!({"action": "data.reconnect"})), ok);
    assert_eq!(asked, ["/agent-api/recovery-actions"]);
    let r = r.unwrap();
    assert_eq!(r["result"]["isError"], false);
    let v: Value = serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(v["action"]["kind"], "loop_only");
    assert_eq!(v["action"]["rollback"]["capability"], "unsupported");
    assert_eq!(v["allow_list"]["empty"], true);
    assert_eq!(v["device_id"], "a1b2c3d4e5f6");
    assert!(v.get("actions").is_none());
    let (r, _) = call(tool("explain_recovery_action", json!({})), ok);
    let v: Value = serde_json::from_str(r.unwrap()["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(v["actions"].as_array().unwrap().len(), 2);
    assert_eq!(v["actions"][1]["agent_layer"], "apn");
}

#[test]
fn explain_errors_are_explicit() {
    let ok = |_: &str| Got::Answer(200, ACTIONS.into());
    let (r, _) = call(tool("explain_recovery_action", json!({"action": "device.reboot"})), ok);
    let r = r.unwrap()["result"].clone();
    assert_eq!(r["isError"], true);
    assert_eq!(r["structuredContent"]["error"], "unknown_action");
    assert!(text(&r).contains("data.reconnect") && text(&r).contains("apn.use"));
    let old = |_: &str| Got::Answer(200, r#"{"core":"up","state":"not_described","http":404}"#.into());
    let (r, _) = call(tool("explain_recovery_action", json!({"action": "data.reconnect"})), old);
    let r = r.unwrap()["result"].clone();
    assert_eq!(r["isError"], true);
    assert_eq!(r["structuredContent"]["error"], "not_described");
    let down = |_: &str| Got::Unreachable("connect: refused".into());
    let (r, _) = call(tool("explain_recovery_action", json!({"action": "data.reconnect"})), down);
    assert!(text(&r.unwrap()["result"]).contains("cannot be reached"));
    let old = |_: &str| Got::Answer(404, String::new());
    let (r, _) = call(tool("get_firmware_capabilities", json!({})), old);
    assert_eq!(r.unwrap()["result"]["isError"], true);
}

#[test]
fn new_reads_respect_the_pin() {
    let msg = tool("get_firmware_capabilities", json!({}));
    let mut f = |_: &Call| Got::Answer(200, CAPS.replace("a1b2c3d4e5f6", "ffffffffffff"));
    let r = handle_pinned(&msg, Some("a1b2c3d4e5f6"), &mut f).unwrap();
    assert_eq!(r["result"]["isError"], true);
    assert_eq!(r["result"]["structuredContent"]["error"], "wrong_device");
}

#[test]
fn gateway_throttles_only_the_new_reads() {
    let setup = Setup::Single { url: DEFAULT_URL.into(), pin: None };
    let mut cache = Cache::default();
    let t0 = Instant::now();
    let mut n = 0;
    let ask = |cache: &mut Cache, msg: Value, at: Instant, n: &mut usize, got: Got| {
        let mut got = Some(got);
        let mut f = |_: Option<&Device>, _: &Call| {
            *n += 1;
            got.take().unwrap_or(Got::Unreachable("again".into()))
        };
        handle_cached(&msg, &setup, cache, at, &mut f).unwrap()["result"].clone()
    };
    let caps = || Got::Answer(200, CAPS.into());
    let m = tool("get_firmware_capabilities", json!({}));
    ask(&mut cache, m.clone(), t0, &mut n, caps());
    let r = ask(&mut cache, m.clone(), t0 + Duration::from_secs(10), &mut n, caps());
    assert_eq!(n, 1, "second ask within the TTL is served by the gateway");
    assert!(text(&r).contains("\"gateway_cache_age_s\": 10"));
    ask(&mut cache, m.clone(), t0 + CACHE_TTL + Duration::from_secs(1), &mut n, caps());
    assert_eq!(n, 2, "after the TTL the device is asked again");
    // a failure is never kept
    let mut c2 = Cache::default();
    let mut n2 = 0;
    ask(&mut c2, m.clone(), t0, &mut n2, Got::Answer(503, String::new()));
    ask(&mut c2, m.clone(), t0, &mut n2, caps());
    assert_eq!(n2, 2);
    // neither is "core down" or "not described" (lweb says those with a 200)
    let mut c3 = Cache::default();
    let mut n4 = 0;
    let ex = tool("explain_recovery_action", json!({}));
    ask(&mut c3, ex.clone(), t0, &mut n4, Got::Answer(200, r#"{"core":"down"}"#.into()));
    ask(&mut c3, ex.clone(), t0, &mut n4, Got::Answer(200, r#"{"core":"up","state":"not_described"}"#.into()));
    ask(&mut c3, ex.clone(), t0, &mut n4, Got::Answer(200, ACTIONS.into()));
    ask(&mut c3, ex, t0, &mut n4, Got::Answer(200, ACTIONS.into()));
    assert_eq!(n4, 3);
    // the other reads always reach the device
    let mut n3 = 0;
    let ev = tool("get_recent_events", json!({}));
    ask(&mut cache, ev.clone(), t0, &mut n3, Got::Answer(200, "{}".into()));
    ask(&mut cache, ev, t0, &mut n3, Got::Answer(200, "{}".into()));
    assert_eq!(n3, 2);
}
