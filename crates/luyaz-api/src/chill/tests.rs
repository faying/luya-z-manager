use super::*;

// ── home exit and the public status block ──

#[test]
fn home_exit_value() {
    assert_eq!(
        parse_home_exit("192.0.2.10:1080"),
        Some((Ipv4Addr::new(192, 0, 2, 10), 1080))
    );
    for bad in [
        "",
        "10.0.0.1",
        "10.0.0.1:",
        ":1080",
        "1.2.3.4:70000",
        "1.2.3.4:0",
        "1.2.3.256:80",
        "1.2.3:80",
        "a.b.c.d:1",
        "1.2.3.4:80:1",
        "1.2.3.4:1080'",
        "01.2.3.4:+5",
    ] {
        assert_eq!(parse_home_exit(bad), None, "{bad}");
    }
}

#[test]
fn sites_in_the_list() {
    assert_eq!(count_sites("\n# note\n+.a.cn\n  \nb.com\n"), 2);
}

#[test]
fn tailscale_path() {
    let ip = Ipv4Addr::new(192, 168, 50, 88);
    let st = |online: bool, cur: &str| {
        json!({"Peer": {
            "a": {"Online": true, "CurAddr": "1.1.1.1:41641", "PrimaryRoutes": ["192.168.9.0/24"]},
            "b": {"Online": online, "CurAddr": cur, "PrimaryRoutes": ["192.168.50.0/24"]},
        }})
    };
    assert_eq!(ts_path(&st(true, "203.0.113.5:41641"), ip), Some("direct"));
    assert_eq!(ts_path(&st(true, ""), ip), Some("relay"));
    assert_eq!(ts_path(&st(false, ""), ip), Some("offline"));
    assert_eq!(ts_path(&st(true, ""), Ipv4Addr::new(192, 168, 51, 1)), None);
    assert_eq!(ts_path(&json!({}), ip), None);
}

#[test]
fn chill_modes() {
    let run = json!({"state": "running"});
    assert_eq!(chill_mode(Some(&run), "rule\n"), ChillMode::Rule);
    assert_eq!(chill_mode(Some(&run), ""), ChillMode::Rule);
    assert_eq!(chill_mode(Some(&run), "global"), ChillMode::Global);
    assert_eq!(chill_mode(Some(&run), "direct"), ChillMode::Direct);
    assert_eq!(
        chill_mode(
            Some(&json!({"state": "direct", "reason": "overheat"})),
            "rule"
        ),
        ChillMode::Hot
    );
    assert_eq!(
        chill_mode(
            Some(&json!({"state": "direct", "reason": "disabled"})),
            "rule"
        ),
        ChillMode::Off
    );
    assert_eq!(chill_mode(None, "rule"), ChillMode::Off);
}

/// D7③: old screens read these four; the rest may only ever be added.
#[test]
fn chill_shape_only_grows() {
    let mf = json!({"on_backup": true, "notice": "手选的 JP 03 不通，已临时换到 🇯🇵 日本（自动）",
        "notice_en": "JP 03 is down; using 🇯🇵 日本 for now (auto)", "notice_at": 1_790_942_400u64, "notice_kind": "backup"});
    let v = public("running", None, true, &mf, Value::Null);
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "home_exit",
            "notice",
            "notice_at",
            "notice_en",
            "notice_kind",
            "on",
            "reason",
            "state"
        ]
    );
    assert_eq!(
        (v["state"].clone(), v["reason"].clone(), v["on"].clone()),
        (json!("running"), Value::Null, json!(true))
    );
    assert_eq!(v["notice"], mf["notice"]);
    assert_eq!(v["notice_kind"], "backup");
    // no notice: every notice field null, the old ones unchanged
    let v = public(
        "stopped",
        Some("user"),
        false,
        &json!({"on_backup": false}),
        Value::Null,
    );
    for k in ["notice", "notice_en", "notice_at", "notice_kind"] {
        assert!(v[k].is_null(), "{k}");
    }
    assert_eq!(v["reason"], "user");
}

// ── new with the move ──

#[test]
fn env_values_as_chill_sh_writes_them() {
    let env = "CHILL_HOME_EXIT='192.0.2.10:1080'\nCHILL_HOME_IP=1\nCHILL_HOME_MEDIA=\"0\"\n";
    assert_eq!(env_var(env, "CHILL_HOME_EXIT"), "192.0.2.10:1080");
    assert_eq!(env_var(env, "CHILL_HOME_IP"), "1");
    assert_eq!(env_var(env, "CHILL_HOME_MEDIA"), "0");
    assert_eq!(env_var(env, "CHILL_HOME"), "");
    assert_eq!(state_and_reason(None), ("unknown".into(), None));
    let st = json!({"state": "direct", "reason": "overheat"});
    assert_eq!(
        state_and_reason(Some(&st)),
        ("direct".into(), Some("overheat".into()))
    );
}

#[test]
fn manual_first_notice_keeps_ten_minutes_unless_on_backup() {
    let p = ManualFirst {
        manual: "JP 03",
        notice: "恢复手选",
        notice_en: "Back",
        notice_at: 1000,
        notice_kind: "back",
        ..Default::default()
    };
    let v = manual_first_summary(&p, 1000 + NOTICE_KEEP - 1);
    assert_eq!(
        (
            v["notice"].as_str(),
            v["notice_kind"].as_str(),
            v["notice_at"].as_u64()
        ),
        (Some("恢复手选"), Some("back"), Some(1000))
    );
    assert_eq!(
        (
            v["on_backup"].clone(),
            v["backup"].clone(),
            v["manual"].clone()
        ),
        (json!(false), Value::Null, json!("JP 03"))
    );
    let v = manual_first_summary(&p, 1000 + NOTICE_KEEP);
    for k in ["notice", "notice_en", "notice_at", "notice_kind"] {
        assert!(v[k].is_null(), "{k}");
    }
    // on the backup the notice stays however old
    let b = ManualFirst {
        backup: "🇯🇵 日本",
        ..p
    };
    assert_eq!(manual_first_summary(&b, 1_000_000)["notice"], "恢复手选");
    // a notice from before L2: no English, kind may be empty
    let old = ManualFirst {
        notice_en: "",
        notice_kind: "",
        ..p
    };
    let v = manual_first_summary(&old, 1001);
    assert!(v["notice_en"].is_null() && v["notice_kind"].is_null() && v["notice"] == "恢复手选");
}

fn home() -> Home {
    Home {
        ready: true,
        ..Default::default()
    }
}

#[test]
fn home_line_only_when_something_is_on() {
    assert!(home_summary(&home()).is_null());
    assert!(
        home_summary(&Home {
            ready: false,
            ip: true,
            ..home()
        })
        .is_null()
    );
    let busy = Home {
        busy: json!({"item": "rest", "on": true, "phase": "trial", "until": 9}),
        ..home()
    };
    assert_eq!(
        home_summary(&busy),
        json!({"media": false, "ip": false, "rest": false, "rest_until": null, "fallback": false})
    );
    let rest = Home {
        rest: true,
        rest_until: 7200,
        home_ok: Some(false),
        ..home()
    };
    assert_eq!(
        home_summary(&rest),
        json!({"media": false, "ip": false, "rest": true, "rest_until": 7200, "fallback": true})
    );
    assert!(wants_fallback(&rest, ChillMode::Rule) && !wants_fallback(&rest, ChillMode::Global));
    assert!(!wants_fallback(
        &Home {
            media: true,
            home_ok: Some(false),
            ..home()
        },
        ChillMode::Rule
    ));
}

#[test]
fn home_get_as_the_screens_read_it() {
    assert_eq!(
        home_get(&Home::default(), &Around::default()),
        json!({"configured": true, "updating": true})
    );
    let notice = json!({"kind": "failed", "item": "ip", "text": "家里不通", "text_en": "Home down", "at": 5000, "code": "home_no_answer", "detail": ""});
    let m = Home {
        ip: true,
        rest: true,
        rest_until: 6000,
        home_ok: Some(true),
        home_ms: Some(42),
        probed_at: 4990,
        notice: notice.clone(),
        busy: Value::Null,
        ..home()
    };
    let a = Around {
        serving_mcc: Some(460),
        true_roaming: Some(false),
        sites: 3,
        tailscale: Some("direct"),
        now: 5500,
        ..Default::default()
    };
    let v = home_get(&m, &a);
    assert_eq!(
        v["where"],
        json!({"serving_mcc": 460, "true_roaming": false, "in_china": true})
    );
    assert_eq!(v["items"]["ip"], json!({"on": true, "sites": 3}));
    assert_eq!(
        v["items"]["rest"],
        json!({"on": true, "until": 6000, "can_extend": true})
    );
    assert_eq!(v["home"], json!({"ok": true, "ms": 42, "checked_at": 4990}));
    assert_eq!(
        (
            v["tailscale"].as_str(),
            v["blocked"].clone(),
            v["busy"].clone()
        ),
        (Some("direct"), Value::Null, Value::Null)
    );
    assert_eq!(v["notice"], notice);
    // the notice goes after ten minutes; rest can't be extended with more than ten left or after its end
    let later = Around {
        now: 5000 + NOTICE_KEEP,
        ..a.clone()
    };
    assert!(home_get(&m, &later)["notice"].is_null());
    assert_eq!(
        home_get(
            &m,
            &Around {
                now: 6000 - EXTEND_WINDOW - 1,
                ..a.clone()
            }
        )["items"]["rest"]["can_extend"],
        false
    );
    assert_eq!(
        home_get(&m, &Around { now: 6000, ..a })["items"]["rest"]["can_extend"],
        false
    );
}

/// luyaz-ui reads GET's `data` into 3072 bytes and `services.chill.home_exit` into 256:
/// the fullest replies must fit.
#[test]
fn the_fullest_replies_fit_the_screen_buffers() {
    let long = "很".repeat(60);
    let notice = json!({"kind": "failed", "item": "rest", "text": long, "text_en": "E".repeat(180), "at": 5000, "code": "home_no_answer", "detail": "D".repeat(200)});
    let m = Home {
        media: true,
        ip: true,
        rest: true,
        rest_until: 1_790_000_000,
        home_ok: Some(false),
        home_ms: Some(65535),
        probed_at: 1_790_000_000,
        busy: json!({"item": "media", "on": true, "phase": "checking", "until": 1_790_000_000u64}),
        notice,
        ..home()
    };
    let a = Around {
        serving_mcc: Some(440),
        true_roaming: Some(true),
        blocked: Some("mode_global"),
        sites: 9999,
        fallback_to: Some("🇯🇵 日本 自动选择 很长的名字".into()),
        tailscale: Some("offline"),
        now: 5001,
    };
    let get = home_get(&m, &a).to_string();
    assert!(get.len() < 3072 - 64, "GET data {} bytes", get.len());
    let line = home_summary(&m).to_string();
    assert!(line.len() < 256, "home_exit {} bytes", line.len());
}

#[test]
fn exit_plans_as_the_agent_switches() {
    let opts = ["🇯🇵 日本", "🇹🇼 台湾", "DIRECT"];
    // keep AI: remember the node (or the manual group while manual-first holds a backup)
    let p = exit_plan("direct_keep_ai", "🇹🇼 台湾", &opts, None, "").unwrap();
    assert_eq!(
        (p.member.as_deref(), p.remember.as_deref(), p.mode),
        (Some("DIRECT"), Some("🇹🇼 台湾"), "rule")
    );
    let p = exit_plan("direct_keep_ai", "🇹🇼 台湾", &opts, None, "🇹🇼 台湾").unwrap();
    assert_eq!(p.remember.as_deref(), Some(MANUAL_GROUP));
    assert_eq!(
        exit_plan("direct_keep_ai", "🇹🇼 台湾", &["🇯🇵 日本"], None, "")
            .unwrap_err()
            .0,
        409
    );
    // already DIRECT: nothing to switch, nothing remembered
    assert_eq!(
        exit_plan("direct_keep_ai", "DIRECT", &opts, None, "")
            .unwrap()
            .member,
        None
    );
    // back to the remembered node, or the first one
    assert_eq!(
        exit_plan("proxy", "DIRECT", &opts, Some("🇹🇼 台湾"), "")
            .unwrap()
            .member
            .as_deref(),
        Some("🇹🇼 台湾")
    );
    assert_eq!(
        exit_plan("proxy", "", &opts, Some("gone"), "")
            .unwrap()
            .member
            .as_deref(),
        Some("🇯🇵 日本")
    );
    assert_eq!(
        exit_plan("proxy", "DIRECT", &["DIRECT"], None, "")
            .unwrap_err()
            .0,
        409
    );
    assert_eq!(
        exit_plan("direct_all", "🇯🇵 日本", &opts, None, "").unwrap(),
        ExitPlan {
            member: None,
            remember: None,
            mode: "direct"
        }
    );
    assert_eq!(
        exit_plan("global", "🇯🇵 日本", &opts, None, "")
            .unwrap()
            .mode,
        "global"
    );
    assert_eq!(exit_wanted(&json!({"state": "x"})).unwrap_err().0, 400);
    assert_eq!(exit_wanted(&json!({"state": "proxy"})).unwrap(), "proxy");
}

#[test]
fn probe_exit_follows_the_main_group_and_checks_the_leaf() {
    use crate::diagnose::NodeProbe;
    let calls = std::cell::RefCell::new(Vec::new());
    let mihomo = |path: &str, slow: bool| -> Got {
        calls.borrow_mut().push((path.to_string(), slow));
        if path == format!("/proxies/{}", enc(MAIN_GROUP)) {
            return Got::Json(200, json!({"now": "JP"}));
        }
        if path == "/proxies/JP" {
            return Got::Json(200, json!({"now": "jp-1"}));
        }
        if path == "/providers/proxies" {
            return Got::Json(
                200,
                json!({"providers": {
                "sub": {"vehicleType": "HTTP", "proxies": [{"name": "jp-1"}]},
                "JP": {"vehicleType": "Compatible", "proxies": [{"name": "jp-1"}]}}}),
            );
        }
        if path.starts_with("/providers/proxies/sub/jp-1/healthcheck?") {
            return Got::Json(200, json!({"delay": 180}));
        }
        Got::Status(404)
    };
    assert_eq!(
        probe_exit(&mihomo),
        ("jp-1".to_string(), Some(NodeProbe::Ms(180)))
    );
    assert!(
        calls
            .borrow()
            .iter()
            .any(|(p, slow)| p.contains("healthcheck") && *slow)
    );
    // nothing chosen: no probe; mihomo silent on the check: None; a timeout answer: Timeout
    assert_eq!(
        probe_exit(&|_, _| Got::Unreachable),
        (MAIN_GROUP.to_string(), None)
    );
    let timeout = |path: &str, _| match path {
        p if p.starts_with("/proxies/%") => Got::Json(200, json!({"now": "DIRECT-ish"})),
        p if p.contains("/delay?") => Got::Status(504),
        _ => Got::Status(404),
    };
    assert_eq!(
        probe_exit(&timeout),
        ("DIRECT-ish".to_string(), Some(NodeProbe::Timeout))
    );
}
