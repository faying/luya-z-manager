use super::*;

const BARK: &str = "https://push.example.net/bark/Q7mZr2x9Ke/";

fn cfg_json() -> Value {
    json!({"config": {
        "enabled": true, "poll_interval_secs": 30, "mark_read_after_forward": false,
        "delete_after_forward": false,
        "rules": [
            {"id": 1, "name": "验证码 → 手机推送", "enabled": true,
             "filter": {"type": "content", "keywords": ["验证码", "code"]},
             "destination": {"type": "webhook", "url": BARK, "method": "POST",
                             "headers": [{"name": "Authorization", "value": "Bearer s3cr3tHdr"}]}},
            {"id": 2, "name": "tg", "enabled": false, "filter": {"type": "all"},
             "destination": {"type": "telegram", "bot_token": "123456:AAbbCCdd", "chat_id": "-1001", "silent": false}},
            {"id": 3, "name": "n", "enabled": true, "filter": {"type": "sender", "patterns": ["+886"]},
             "destination": {"type": "ntfy", "url": "https://ntfy.sh", "topic": "my-secret-topic", "token": "tk_abcdef"}}
        ]},
        "last_forwarded_id": 106})
}

fn cfg() -> Config {
    config(&cfg_json()).unwrap()
}

#[test]
fn form_secret_hints() {
    let c = cfg();
    let ntfy = open_form(Some(&c.rules[2]), Lang::En);
    assert!(ntfy.contains("Remove the saved token") && ntfy.contains("_rnc"));
    assert!(!open_form(Some(&c.rules[1]), Lang::En).contains("Remove the saved token"));
    let slack = json!({"config": {"enabled": true, "rules": [{"id": 9, "name": "s", "enabled": true,
        "filter": {"type": "all"}, "destination": {"type": "slack", "webhook_url": "https://hooks.slack.com/x/SECRET"}}]}});
    let c2 = config(&slack).unwrap();
    let f = open_form(Some(&c2.rules[0]), Lang::En);
    // switching Slack → Discord: the kept hint gives way to "required"
    assert!(f.contains("$_rd === 'slack' ? 'Set · leave empty to keep it' : 'Required: the type changed'"));
    assert!(!f.contains("SECRET"));
}

#[test]
fn reads_the_agent_config_and_log() {
    let c = cfg();
    assert!(c.enabled && c.poll == 30 && c.last_id == 106);
    assert_eq!(c.rules.len(), 3);
    assert_eq!(c.rules[0].keywords, vec!["验证码", "code"]);
    assert_eq!(c.rules[2].patterns, vec!["+886"]);
    let g = log(&json!([
        {"timestamp": 1_790_000_000, "sms_id": 105, "sender": "+1", "content_preview": "1234",
         "rule_name": "r", "destination_type": "webhook", "success": false, "rule_id": 1, "error": "x"}
    ]))
    .unwrap();
    assert_eq!(g[0].preview, "1234", "already decoded text, shown as it is");
    assert!(!g[0].success && g[0].rule_id == 1);
}

#[test]
fn secrets_are_never_rendered() {
    let mut g = Got::default();
    g.update(
        Ok(cfg()),
        Ok(log(&json!([{"timestamp": 1, "sms_id": 1, "sender": "+1", "content_preview": "p",
            "rule_name": "tg", "destination_type": "telegram", "success": false, "rule_id": 2,
            "error": "telegram: error sending request for url (https://api.telegram.org/bot123456:AAbbCCdd/sendMessage)"}]))
        .unwrap()),
    );
    for l in [Lang::En, Lang::Zh] {
        let html: String = leaves(&g, l).into_iter().map(|(_, h)| h).collect();
        for s in [
            "Q7mZr2x9Ke",
            "bark",
            "AAbbCCdd",
            "my-secret-topic",
            "tk_abcdef",
            "s3cr3tHdr",
        ] {
            assert!(!html.contains(s), "{s} leaked: {html}");
        }
        assert!(html.contains("POST push.example.net"), "type + host only");
        assert!(html.contains("Telegram -1001") && html.contains("ntfy ntfy.sh"));
        assert!(html.contains("https://api.telegram.org/…"), "{html}");
    }
    for r in [
        None,
        Some(&cfg().rules[0]),
        Some(&cfg().rules[1]),
        Some(&cfg().rules[2]),
    ] {
        let f = open_form(r, Lang::En);
        for s in [
            "Q7mZr2x9Ke",
            "AAbbCCdd",
            "my-secret-topic",
            "tk_abcdef",
            "s3cr3tHdr",
        ] {
            assert!(!f.contains(s), "{s} in the form: {f}");
        }
    }
    let f = open_form(Some(&cfg().rules[1]), Lang::En);
    assert!(
        f.contains("Set · leave empty to keep it") && f.contains("-1001"),
        "{f}"
    );
}

#[test]
fn scrub_hides_secrets_and_url_paths() {
    let sec = secrets(&cfg());
    assert_eq!(
        scrub(
            "webhook: error for url (https://push.example.net/bark/Q7mZr2x9Ke/?x=1): timeout",
            &sec
        ),
        "webhook: error for url (https://push.example.net/…): timeout"
    );
    assert_eq!(scrub("bad token 123456:AAbbCCdd", &sec), "bad token •••");
    assert_eq!(scrub("/bark/Q7mZr2x9Ke/ refused", &sec), "••• refused");
    assert_eq!(
        scrub("see http://u:p@h.example:8080", &[]),
        "see http://h.example:8080"
    );
    assert_eq!(scrub("webhook: HTTP 502", &sec), "webhook: HTTP 502");
    assert_eq!(
        host("https://a:b@push.example.net:8443/x?y"),
        "push.example.net:8443"
    );
}

fn rin(dest: &str) -> RuleIn {
    RuleIn {
        id: None,
        name: " r ".into(),
        enabled: true,
        filter: "content".into(),
        patterns: String::new(),
        keywords: "OTP， code,, ".into(),
        dest: dest.into(),
        method: "PUT".into(),
        ..Default::default()
    }
}

#[test]
fn rule_checks_and_keeps_secrets_left_empty() {
    let l = Lang::En;
    let c = cfg();
    // a new webhook rule needs its URL
    assert!(rule_check(&rin("webhook"), None, l).is_err());
    let mut f = rin("webhook");
    f.url = "ftp://x".into();
    assert!(rule_check(&f, None, l).unwrap_err().contains("https://"));
    f.url = "https://h.example/k".into();
    let b = rule_check(&f, None, l).unwrap();
    assert_eq!(b["name"], "r");
    assert_eq!(
        b["filter"],
        json!({"type": "content", "keywords": ["OTP", "code"]})
    );
    assert_eq!(
        b["destination"],
        json!({"type": "webhook", "url": "https://h.example/k", "method": "PUT", "headers": []})
    );
    assert!(b.get("id").is_none());
    // editing: empty = keep (URL and headers), the id goes along
    let f = rin("webhook");
    let b = rule_check(&f, Some(&c.rules[0]), l).unwrap();
    assert_eq!(b["id"], 1);
    assert_eq!(b["destination"]["url"], BARK);
    assert_eq!(b["destination"]["headers"][0]["value"], "Bearer s3cr3tHdr");
    // another type: nothing kept
    let mut f = rin("telegram");
    f.chat_id = "5".into();
    assert!(
        rule_check(&f, Some(&c.rules[0]), l).is_err(),
        "a telegram rule needs its token"
    );
    f.bot_token = "9:x".into();
    assert_eq!(
        rule_check(&f, Some(&c.rules[0]), l).unwrap()["destination"]["bot_token"],
        "9:x"
    );
    // telegram kept, ntfy kept (token optional)
    let mut f = rin("telegram");
    f.chat_id = "-1001".into();
    assert_eq!(
        rule_check(&f, Some(&c.rules[1]), l).unwrap()["destination"]["bot_token"],
        "123456:AAbbCCdd"
    );
    let mut f = rin("ntfy");
    f.ntfy_url = "https://ntfy.sh".into();
    let d = rule_check(&f, Some(&c.rules[2]), l).unwrap()["destination"].clone();
    assert_eq!(
        (d["topic"].as_str(), d["token"].as_str()),
        (Some("my-secret-topic"), Some("tk_abcdef"))
    );
    // the saved token removed on purpose (the topic still kept)
    f.clear_token = true;
    let d = rule_check(&f, Some(&c.rules[2]), l).unwrap()["destination"].clone();
    assert_eq!(
        (d["topic"].as_str(), d.get("token")),
        (Some("my-secret-topic"), None)
    );
    f.clear_token = false;
    let d = rule_check(&f, None, l);
    assert!(d.is_err(), "a new ntfy rule needs its topic");
    // filters
    let mut f = rin("sms");
    f.number = "+1".into();
    f.filter = "sender".into();
    assert!(
        rule_check(&f, None, l)
            .unwrap_err()
            .contains("sender pattern")
    );
    f.filter = "sender_and_content".into();
    f.patterns = "+886".into();
    assert_eq!(
        rule_check(&f, None, l).unwrap()["filter"]["patterns"],
        json!(["+886"])
    );
    f.name = "  ".into();
    assert!(rule_check(&f, None, l).is_err());
}

#[test]
fn readback_matches_the_agents_defaults() {
    let c = cfg();
    let mut f = rin("webhook");
    f.method = "POST".into();
    f.keywords = "验证码, code".into();
    f.name = "验证码 → 手机推送".into();
    let b = rule_check(&f, Some(&c.rules[0]), Lang::En).unwrap();
    assert!(rule_is(&c.rules[0], &b));
    assert!(same_dest(
        &json!({"type": "telegram", "bot_token": "a", "chat_id": "b"}),
        &json!({"type": "telegram", "bot_token": "a", "chat_id": "b", "silent": false})
    ));
    assert!(!same_dest(
        &json!({"type": "sms", "forward_number": "1"}),
        &json!({"type": "sms", "forward_number": "2"})
    ));
}

#[test]
fn settings_check_the_interval() {
    let l = Lang::En;
    let f = |p: Value| CfgIn {
        enabled: true,
        poll: p,
        mark_read: false,
        delete_after: true,
    };
    assert!(
        cfg_check(&f(json!("5")), l).is_err(),
        "the agent wants >= 10"
    );
    assert!(cfg_check(&f(json!("3601")), l).is_err());
    assert!(cfg_check(&f(json!("1.5")), l).is_err());
    let b = cfg_check(&f(json!(" 60 ")), l).unwrap();
    assert_eq!(b["poll_interval_secs"], 60);
    assert_eq!(
        cfg_check(&f(json!(10)), l).unwrap()["poll_interval_secs"],
        10
    );
    let c = cfg_confirm(&b, Lang::Zh);
    assert!(c.what.contains("删除") && c.danger, "{}", c.what);
    assert_eq!(c.tier, write::Tier::Inline);
}

#[test]
fn tiers_follow_the_inventory() {
    let c = cfg();
    let l = Lang::Zh;
    assert_eq!(
        toggle_confirm(&c.rules[0], false, l).tier,
        write::Tier::Inline
    );
    assert_eq!(test_confirm(&c.rules[0], l).tier, write::Tier::Inline);
    assert_eq!(del_confirm(&c.rules[0], l).tier, write::Tier::Dialog);
    assert_eq!(retry_confirm(2, l).tier, write::Tier::Inline);
    assert_eq!(clear_confirm(4, l).tier, write::Tier::Dialog);
    let sms = Rule {
        dest: json!({"type": "sms", "forward_number": "+8613800000000"}),
        ..c.rules[0].clone()
    };
    assert!(
        test_confirm(&sms, Lang::En)
            .what
            .contains("carrier may charge")
    );
    assert!(
        !test_confirm(&c.rules[0], Lang::En)
            .what
            .contains("Q7mZr2x9Ke")
    );
    assert!(!del_confirm(&c.rules[1], Lang::En).what.contains("AAbbCCdd"));
}

#[test]
fn retry_follows_the_entry_not_the_index() {
    let e = |sms: u64, rule: u64, ok: bool| Entry {
        ts: 0,
        sms_id: sms,
        rule_id: rule,
        sender: format!("s{sms}"),
        preview: String::new(),
        rule_name: String::new(),
        dest_type: String::new(),
        success: ok,
        error: String::new(),
    };
    let before = vec![e(105, 1, true), e(104, 1, false), e(100, 1, false)];
    assert_eq!(failed_keys(&before).len(), 2);
    // a new message came in at the top: the indices shift by one
    let now = vec![
        e(107, 1, true),
        e(105, 1, true),
        e(104, 1, false),
        e(100, 1, false),
    ];
    assert_eq!(index_of(&now, 104, 1), Some(2));
    assert_eq!(index_of(&now, 105, 1), None, "succeeded: not retried");
    assert!(matches!(
        retry_said(2, &[], Lang::En),
        write::Outcome::Applied(_)
    ));
    let o = retry_said(2, &[("s100".into(), "no WAN".into())], Lang::En);
    assert!(
        o.text(Lang::En).contains("1 of 2 still failing"),
        "{}",
        o.text(Lang::En)
    );
}

#[test]
fn device_time_uses_the_utc_digits() {
    // 2026-09-24 08:31:22 as the device writes it
    assert_eq!(device_time(1_790_238_682), "2026-09-24 08:31");
    assert_eq!(device_time(0), "—");
}

#[test]
fn the_status_says_what_happens() {
    let mut g = Got::default();
    assert!(view(&g, Lang::En).state.starts_with("Reading"));
    g.update(Err(Fail::Unreachable), Err(Fail::Unreachable));
    let v = view(&g, Lang::En);
    assert_eq!(v.tone, "bad");
    assert!(v.retry);
    g.update(Ok(cfg()), Ok(vec![]));
    let v = view(&g, Lang::En);
    assert_eq!(v.state, "Forwarding on · 2 rules enabled");
    assert_eq!(v.last_id, Some(106));
    g.update(Err(Fail::Unreachable), Ok(vec![]));
    let v = view(&g, Lang::Zh);
    assert_eq!(v.tone, "stale");
    assert!(v.meta.unwrap().starts_with("设置停在"));
    let mut c = cfg();
    c.enabled = false;
    g.update(Ok(c), Ok(vec![]));
    assert_eq!(view(&g, Lang::En).state, "Forwarding off");
    let html: String = leaves(&g, Lang::En).into_iter().map(|(_, h)| h).collect();
    assert!(
        html.contains("data-signals__ifmissing"),
        "a push never resets the draft"
    );
}

#[test]
fn rule_check_names_the_missing_field_and_checks_formats() {
    let l = Lang::En;
    let mut f = rin("telegram");
    assert!(rule_check(&f, None, l).unwrap_err().contains("Bot Token"));
    f.bot_token = "9:x".into();
    assert!(rule_check(&f, None, l).unwrap_err().contains("Chat ID"));
    for ok in ["5", "-1001", "@my_channel"] {
        f.chat_id = ok.into();
        assert!(rule_check(&f, None, l).is_ok(), "{ok}");
    }
    for bad in ["-", "@", "12a", "@bad-name", "https://t.me/x"] {
        f.chat_id = bad.into();
        assert!(rule_check(&f, None, l).unwrap_err().contains("number"), "{bad}");
    }
    let mut f = rin("sms");
    assert!(rule_check(&f, None, l).unwrap_err().contains("number to forward"));
    for ok in ["+1", "10086", "+886 912-345-678"] {
        f.number = ok.into();
        assert!(rule_check(&f, None, l).is_ok(), "{ok}");
    }
    for bad in ["+", "abc", "+86 138x", "123456789012345678901"] {
        f.number = bad.into();
        assert!(rule_check(&f, None, l).is_err(), "{bad}");
    }
    let mut f = rin("ntfy");
    assert!(rule_check(&f, None, l).unwrap_err().contains("server URL"));
    f.ntfy_url = "https://ntfy.sh".into();
    assert!(rule_check(&f, None, l).unwrap_err().contains("topic"));
    assert!(rule_check(&rin("discord"), None, l).unwrap_err().contains("Webhook URL"));
    assert_eq!(
        rule_check(&rin("slack"), None, Lang::Zh).unwrap_err(),
        "把 Webhook 地址填上。"
    );
}
