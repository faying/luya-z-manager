use super::*;

/// The public CHILL fields the screen reads.
#[test]
fn public_chill_shape() {
    let mf = json!({"on_backup": true, "notice": "手选的 JP 03 不通，已临时换到 🇯🇵 日本（自动）",
        "notice_en": "JP 03 is down; using 🇯🇵 日本 for now (auto)", "notice_at": 1_790_942_400u64, "notice_kind": "backup"});
    let v = public("running", None, true, &mf);
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
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
    );
    for k in ["notice", "notice_en", "notice_at", "notice_kind"] {
        assert!(v[k].is_null(), "{k}");
    }
    assert_eq!(v["reason"], "user");
}

// ── new with the move ──

#[test]
fn env_values_as_chill_sh_writes_them() {
    let env = "SUB_URL='https://example.test/sub'\nCHILL_API_LAN=1\nCHILL_REGIONS=\"JP\"\n";
    assert_eq!(env_var(env, "SUB_URL"), "https://example.test/sub");
    assert_eq!(env_var(env, "CHILL_API_LAN"), "1");
    assert_eq!(env_var(env, "CHILL_REGIONS"), "JP");
    assert_eq!(env_var(env, "CHILL_API"), "");
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
