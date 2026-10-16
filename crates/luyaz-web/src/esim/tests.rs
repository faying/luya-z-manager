use super::*;

fn list() -> Value {
    json!({"installed": true, "busy": false, "profiles": [
        {"iccid": "8988600000000000001", "isdpAid": "A0000005591010FFFFFFFF8900001100", "profileState": "enabled",
         "profileNickname": "台湾旅行", "serviceProviderName": "Nova Travel", "profileName": "Taiwan 20GB 10 Days"},
        {"iccid": "8981000000000000003", "isdpAid": "A0000005591010FFFFFFFF8900001200", "profileState": "disabled",
         "profileNickname": null, "serviceProviderName": "Pacific Roam", "profileName": "Japan 5GB 7 Days"},
        {"iccid": "8986000000000000002", "isdpAid": "bad'aid", "profileState": "disabled",
         "profileNickname": null, "serviceProviderName": "", "profileName": null}
    ]})
}

fn ps() -> Vec<Profile> {
    match profiles(&list()) {
        Listed::List(p) => p,
        Listed::Busy => panic!("busy"),
    }
}

#[test]
fn profiles_parse_with_safe_keys_and_names() {
    let p = ps();
    assert_eq!(p.len(), 3);
    assert_eq!(p[0].key, "A0000005591010FFFFFFFF8900001100");
    assert!(p[0].enabled && !p[1].enabled);
    assert_eq!(p[0].name(), "台湾旅行");
    assert_eq!(p[1].name(), "Pacific Roam");
    assert_eq!(p[1].sub(), "Pacific Roam · Japan 5GB 7 Days");
    assert_eq!(
        p[2].key, "n2",
        "a key that isn't hex never reaches a URL or a JS string"
    );
    assert_eq!(
        p[2].name(),
        "8986000000000000002",
        "nothing else: the ICCID"
    );
    assert!(valid_key(&p[0].key) && valid_key("n2"));
    for k in ["", "n", "nx", "8988'", "a b", &"A".repeat(65)] {
        assert!(!valid_key(k), "{k}");
    }
    assert_eq!(
        profiles(&json!({"installed": true, "busy": true, "profiles": null})),
        Listed::Busy
    );
    assert_eq!(notifs(&Value::Null), None, "null while a job runs");
}

#[test]
fn busy_keeps_the_last_list() {
    let mut c = Card::default();
    c.take_list(Ok(profiles(&list())));
    c.take_list(Ok(Listed::Busy));
    assert!(c.busy && c.list.as_ref().unwrap().len() == 3);
    c.take_list(Err(Fail::Unreachable));
    assert!(c.stale());
}

fn card() -> Card {
    let mut c = Card {
        chip: Some(chip(
            &json!({"installed": true, "eid": "89049000000000000000000000000041",
            "free_nvm": 325632, "sgp22_version": "2.2.2", "firmware_version": "3.1.0", "root_smds": "lpa.ds.gsma.com", "default_smdp": null}),
        )),
        ..Card::default()
    };
    c.take_list(Ok(profiles(&list())));
    c
}

#[test]
fn status_follows_the_old_page() {
    let c = card();
    let v = view(&c, false, Lang::En);
    assert_eq!((v.tone, v.state.as_str()), ("ok", "In use: 台湾旅行"));
    assert!(v.state_data && v.meta_data && !v.locked);
    assert_eq!(v.chip[1].value, "318 KB");
    assert_eq!(v.chip[5].value, "—", "default SM-DP+ null");

    let mut b = c.clone();
    b.take_list(Ok(Listed::Busy));
    b.job = Some("switch".into());
    let v = view(&b, false, Lang::Zh);
    assert_eq!(v.state, "切换中…");
    assert!(v.locked && v.notif_dis);

    let v = view(&c, true, Lang::En);
    assert_eq!(v.state, "Rebooting…");
    assert!(v.locked);

    let mut s = c.clone();
    s.take_list(Err(Fail::Unreachable));
    let v = view(&s, false, Lang::En);
    assert_eq!(v.tone, "stale");
    assert!(v.retry && v.locked && v.meta.unwrap().contains("refresh before changing anything"));

    let none = Card {
        chip: Some(Chip {
            installed: false,
            ..Chip::default()
        }),
        ..Card::default()
    };
    let v = view(&none, false, Lang::En);
    assert!(v.no_lpac && v.state.contains("lpac") && v.chip.is_empty());

    let gone = Card {
        chip_fail: Some(Fail::Unreachable),
        ..Card::default()
    };
    let v = view(&gone, false, Lang::En);
    assert_eq!(v.tone, "bad");
    assert!(v.retry);
}

#[test]
fn leaves_render_without_iccids_in_urls() {
    let html = Leaves {
        v: &view(&card(), false, Lang::En),
    }
    .render()
    .unwrap();
    for id in ["es-status", "es-list", "es-dlgo", "es-notif", "es-chip"] {
        assert!(html.contains(&format!("id=\"{id}\"")), "{id}");
    }
    assert!(html.contains("/router/esim/switch/ask?k=A0000005591010FFFFFFFF8900001200"));
    assert!(
        !html.contains("?k=89") && html.contains("switch/ask?k=n2"),
        "ICCIDs never; a non-hex AID by index"
    );
    for u in html.split("@post('").skip(1) {
        let url = u.split('\'').next().unwrap();
        assert!(
            !url.contains("89886") && !url.contains("89810") && !url.contains("89860"),
            "{url}"
        );
    }
    // the profile in use has no switch / delete
    assert!(!html.contains("switch/ask?k=A0000005591010FFFFFFFF8900001100"));
    assert!(html.contains("aria-labelledby=\"es-sw1 es-n1\""));
}

#[test]
fn confirms_keep_the_inventory_tiers() {
    let r = Req {
        name: "Japan".into(),
        ..Req::default()
    };
    let c = confirm(Act::Switch, &r, Some("台湾旅行"), 0, Lang::En);
    assert_eq!(c.tier, write::Tier::Dialog);
    assert!(
        c.cuts_uplink && c.downtime.is_some() && c.recovery.as_ref().unwrap().contains("台湾旅行")
    );
    let d = confirm(Act::Delete, &r, None, 0, Lang::Zh);
    assert_eq!(d.tier, write::Tier::Dialog);
    assert!(d.danger && d.word.as_deref() == Some("删除"));
    for a in [Act::Nick, Act::Download, Act::Notif] {
        assert_eq!(
            confirm(a, &r, None, 2, Lang::En).tier,
            write::Tier::Inline,
            "{a:?}"
        );
    }
    assert!(
        confirm(Act::Notif, &r, None, 2, Lang::En)
            .what
            .contains("2 receipt")
    );
}

#[test]
fn job_slot_answers() {
    assert_eq!(job_end(5, &json!({"id": 5, "status": "running"})), None);
    assert_eq!(
        job_end(5, &json!({"id": 4, "status": "done"})),
        None,
        "not ours yet"
    );
    assert_eq!(
        job_end(5, &json!({"id": 5, "status": "done", "rebooting": true})),
        Some(JobEnd::Done { rebooting: true })
    );
    assert_eq!(
        job_end(
            5,
            &json!({"id": 5, "status": "error", "message": "x", "reason": "card_busy"})
        ),
        Some(JobEnd::Error {
            message: "x".into(),
            reason: "card_busy".into()
        })
    );
    assert_eq!(
        job_end(5, &json!({"id": 6, "status": "running"})),
        Some(JobEnd::Moved)
    );
}

#[test]
fn failures_say_why_and_what_to_do() {
    let o = job_failed("lpac: catBusy", "card_busy", Lang::En);
    assert!(
        o.text(Lang::En).contains("restart the device"),
        "{}",
        o.text(Lang::En)
    );
    let o = job_failed("operation in progress", "", Lang::Zh);
    assert!(o.text(Lang::Zh).contains("另一个 eSIM 操作还在进行"));
    let o = refusal(
        409,
        &json!({"ok": false, "error": "operation in progress"}),
        Lang::En,
    );
    assert!(o.text(Lang::En).contains("Another eSIM operation"));
    let o = refusal(
        429,
        &json!({"ok": false, "error": "card needs a cooldown after the last switch attempt — wait 280s and retry"}),
        Lang::En,
    );
    assert!(o.text(Lang::En).contains("wait 280s") && o.tone() == "bad");
}

#[test]
fn a_switch_is_proven_by_the_card_never_by_the_job() {
    let p = ps();
    let r = Req {
        iccid: p[1].iccid.clone(),
        name: p[1].name(),
        ..Req::default()
    };
    assert!(
        !shows(Act::Switch, &r, &p),
        "job done + old profile still enabled: not shown"
    );
    let o = not_read(Act::Switch, &r, Lang::En);
    assert!(o.tone() == "warn" && !o.text(Lang::En).to_lowercase().contains("switched to"));
    // the reboot verifier: the target not enabled is never Applied
    let o = after_outcome(Some(&p[1]), Some(&p[0]), true, Lang::Zh);
    assert_eq!(o.tone(), "bad");
    assert!(o.text(Lang::Zh).contains("设备没有重启") && o.text(Lang::Zh).contains("台湾旅行"));
    let mut q = p.clone();
    q[0].enabled = false;
    q[1].enabled = true;
    assert!(shows(Act::Switch, &r, &q));
    assert!(matches!(
        after_outcome(Some(&q[1]), Some(&q[1]), false, Lang::En),
        write::Outcome::Applied(_)
    ));
    // the other readbacks
    assert!(shows(
        Act::Delete,
        &Req {
            iccid: "x".into(),
            ..Req::default()
        },
        &p
    ));
    assert!(!shows(
        Act::Download,
        &Req {
            before: 3,
            ..Req::default()
        },
        &p
    ));
    assert!(shows(
        Act::Nick,
        &Req {
            iccid: p[1].iccid.clone(),
            nick: String::new(),
            ..Req::default()
        },
        &p
    ));
}

#[test]
fn codes_and_bytes() {
    assert!(code_ok("LPA:1$smdp.example$ABC") && code_ok("lpa:1$x$y") && code_ok("1$x$y"));
    assert!(!code_ok("smdp.example") && !code_ok("LPA:2$x"));
    assert_eq!(fmt_bytes(512), "512 B");
    assert_eq!(fmt_bytes(2 * 1024 * 1024), "2.0 MB");
}

#[test]
fn the_reboot_verifier_is_a_read_without_signals() {
    let h = after_html("A0");
    assert!(h.contains("@post('/router/esim/after?k=A0'") && h.contains("include: /^$/"));
    assert!(h.contains("retryMaxCount: 80") && h.contains("retryScaler: 1"));
}

#[test]
fn the_stash_is_one_shot() {
    let t = put(
        Act::Download,
        Req {
            code: "LPA:1$a$b".into(),
            ..Req::default()
        },
    );
    assert!(take(&t).is_some());
    assert!(take(&t).is_none(), "a token sends once");
}
