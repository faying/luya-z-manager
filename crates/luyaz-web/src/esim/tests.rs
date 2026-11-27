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
        _ => panic!("not a list"),
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
    assert_eq!(v.chip[5].value, "Not set", "default SM-DP+ null");

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
        Some(JobEnd::Done {
            rebooting: true,
            no_data: false,
            stuck: false
        })
    );
    assert_eq!(
        job_end(5, &json!({"id": 5, "status": "done", "reason": "no_data"})),
        Some(JobEnd::Done {
            rebooting: false,
            no_data: true,
            stuck: false
        })
    );
    assert_eq!(
        job_end(5, &json!({"id": 5, "status": "done", "reason": "identity_stuck"})),
        Some(JobEnd::Done {
            rebooting: false,
            no_data: false,
            stuck: true
        })
    );
    assert_eq!(
        job_deadline(5, &json!({"id": 5, "status": "running", "deadline_s": 470})),
        Some(Duration::from_secs(470))
    );
    assert_eq!(job_deadline(5, &json!({"id": 5, "status": "running"})), None);
    assert_eq!(job_deadline(5, &json!({"id": 4, "deadline_s": 470})), None);
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
    // switched but no data back: a warning that says so, never plain Applied
    let o = no_data(&r, Lang::Zh);
    assert!(o.tone() == "warn" && o.text(Lang::Zh).contains("移动数据还没恢复"));
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

#[test]
fn a_settling_list_is_shown_locked_and_proves_nothing() {
    let mut v = list();
    v["stale"] = json!(true);
    v["updating"] = json!(true);
    v["age_s"] = json!(7);
    let Listed::Settling(ps, 7) = profiles(&v) else {
        panic!("not settling")
    };
    assert_eq!(ps.len(), 3);
    let mut c = card();
    c.take_list(Ok(Listed::Settling(ps, 7)));
    let w = view(&c, false, Lang::Zh);
    assert_eq!(w.tone, "stale");
    assert!(w.locked && w.meta.as_deref().unwrap().contains("7 秒前"));
    assert_eq!(w.state, "使用中：台湾旅行", "the list still shows");
    c.take_list(Ok(profiles(&list())));
    let w = view(&c, false, Lang::En);
    assert!(w.tone == "ok" && !w.locked, "a fresh list unlocks");
}

#[test]
fn core_say_heads_the_switch_outcome() {
    let j = json!({"id": 5, "status": "done", "reason": "no_data", "say": "可能要查 APN", "say_en": "May need an APN check"});
    assert_eq!(job_say(5, &j, Lang::Zh).as_deref(), Some("可能要查 APN"));
    assert_eq!(job_say(5, &j, Lang::En).as_deref(), Some("May need an APN check"));
    assert_eq!(job_say(4, &j, Lang::Zh), None, "another job's word is not ours");
    assert_eq!(job_say(5, &json!({"id": 5, "say": ""}), Lang::Zh), None, "older core / nothing to say");
    let o = with_say(write::Outcome::Failed(String::new()), Some("自动恢复未成功、设置没被再改"), Lang::Zh);
    assert_eq!(o, write::Outcome::Failed("自动恢复未成功、设置没被再改".into()));
    let o = with_say(write::Outcome::Applied("已切换到「B」".into()), Some("已恢复"), Lang::Zh);
    assert_eq!(o, write::Outcome::Applied("已恢复。已切换到「B」".into()));
    assert_eq!(with_say(write::Outcome::Unknown, Some("x"), Lang::En), write::Outcome::Unknown);
}

#[test]
fn core_say_in_the_card_view() {
    let mut c = card();
    c.sim_say = say_of(&json!({"say": "正在切换", "say_en": "Switching"}));
    assert_eq!(view(&c, false, Lang::Zh).meta.as_deref(), Some("正在切换"));
    assert_eq!(view(&c, false, Lang::En).meta.as_deref(), Some("Switching"));
    c.busy = true;
    c.job = Some("switch".into());
    c.job_say = say_of(&json!({"say": "安全重试中", "say_en": "Retrying safely"}));
    assert_eq!(view(&c, false, Lang::Zh).state, "安全重试中");
    assert_eq!(say_of(&json!({"say": ""})), None, "older core: nothing");
}

fn busy_job(tries: u64, gap: u64) -> Value {
    json!({"id": 7, "kind": "switch", "status": "running", "started_unix": 1000, "attempts": tries,
           "message": format!("card busy — retrying (try {} in {gap} s)", tries + 1), "deadline_s": 505})
}

#[test]
fn busy_retry_reads_tries_next_try_and_give_up() {
    let mut seen = None;
    // until: the page's own send time + 240 s + the last try (never the job's started_unix)
    let r = busy_retry(7, &busy_job(2, 15), 1002, 1030, &mut seen).unwrap();
    assert_eq!(r, Retry { tries: 2, next_unix: 1045, until_unix: 1257 });
    // the same try polled later keeps its time; the next try gets a new one
    assert_eq!(busy_retry(7, &busy_job(2, 15), 1002, 1040, &mut seen).unwrap().next_unix, 1045);
    assert_eq!(busy_retry(7, &busy_job(3, 22), 1002, 1050, &mut seen).unwrap().next_unix, 1072);
    // not ours, not waiting on the card, or over
    assert_eq!(busy_retry(8, &busy_job(2, 15), 1002, 1030, &mut None), None);
    let mut j = busy_job(2, 15);
    j["message"] = json!("switching");
    assert_eq!(busy_retry(7, &j, 1002, 1030, &mut None), None);
    let mut j = busy_job(2, 15);
    j["status"] = json!("error");
    assert_eq!(busy_retry(7, &j, 1002, 1030, &mut None), None);
}

#[test]
fn retry_html_shows_local_times_and_a_stop_button() {
    // device clock: local wall time labelled UTC, so 1045 s = 00:17:25
    let r = Retry { tries: 1, next_unix: 1045, until_unix: 1240 };
    let zh = retry_html(Some(&r), false, Lang::Zh);
    assert!(zh.contains("已试 1 次") && zh.contains("00:17:25 再试") && zh.contains("约 00:20:40 放弃"), "{zh}");
    assert!(zh.contains("停止重试") && zh.contains("@post('/router/esim/stop', {retryMaxCount: 0})"), "{zh}");
    let en = retry_html(Some(&r), false, Lang::En);
    assert!(en.contains("1 try so far") && en.contains("Stop retrying"), "{en}");
    assert!(retry_html(Some(&r), true, Lang::Zh).contains("正在停止重试"));
    assert!(!retry_html(Some(&r), true, Lang::Zh).contains("<button"));
    assert_eq!(retry_html(None, false, Lang::En), RETRY_EMPTY);
}

#[test]
fn stop_reply_says_what_the_core_answered() {
    assert!(stop_reply(Ok((200, &json!({"ok": true}))), Lang::Zh).contains("正在停止重试"));
    let past = json!({"ok": false, "error": "the switch is past the card step"});
    assert!(stop_reply(Ok((409, &past)), Lang::Zh).contains("停不了了"));
    let none = json!({"ok": false, "error": "no switch to stop"});
    assert!(stop_reply(Ok((409, &none)), Lang::En).contains("no retry to stop"));
}

// ---- dual-slot units: one block per eUICC (dual-slot design §6) ----

fn card2() -> Card {
    Card {
        n: 2,
        role: "standby".into(),
        notifs: Some(vec![Notif { seq: 3, op: "enable".into(), addr: "smdp.example".into(), iccid: "8988600000000000001".into() }]),
        ..card()
    }
}

#[test]
fn a_dual_slot_block_carries_its_slot_everywhere() {
    let v = view(&card2(), false, Lang::Zh);
    assert_eq!(v.heading.as_deref(), Some("卡槽 2 · 待机"));
    let html = Leaves { v: &v }.render().unwrap();
    for id in ["es-hd-s2", "es-status-s2", "es-list-s2", "es-dlgo-s2", "es-notif-s2", "es-chip-s2", "es-p-s2-0", "es-n-s2-1"] {
        assert!(html.contains(&format!("id=\"{id}\"")), "{id}");
    }
    let i = html.find("switch/ask?k=").unwrap();
    assert!(html.contains("switch/ask?k=A0000005591010FFFFFFFF8900001200&#38;s=2"), "{}", &html[i..i + 60]);
    assert!(html.contains("$_esEdit === '2-A0000005591010FFFFFFFF8900001100'"));
    assert!(html.contains("download/ask?s=2") && html.contains("/^_esCo(de|nf)2$/") && html.contains("$_esCode2.trim()"));
    assert!(html.contains("notif/ask?s=2"));
    assert!(html.contains("<h3 id=\"es-chip-t-s2\""), "group titles under the card heading are h3");
    assert!(!html.contains("eid=") && !html.contains("slot="), "the page's URLs carry `s=`, the core's names are added server-side");
    // the single-slot unit's leaves are as they were
    let one = Leaves { v: &view(&card(), false, Lang::Zh) }.render().unwrap();
    assert!(!one.contains("es-hd") && !one.contains("&s=") && one.contains("id=\"es-p0\"") && one.contains("<h2 id=\"es-chip-t\""));
}

#[test]
fn a_held_card_says_why_without_an_error() {
    for (h, tone) in [(Hold::Identifying, "neutral"), (Hold::Unknown, "warn"), (Hold::Moved, "warn")] {
        let c = Card { n: 1, role: "data".into(), hold: Some(h), ..card() };
        let v = view(&c, false, Lang::Zh);
        assert_eq!(v.tone, tone, "{h:?}");
        assert!(v.locked && v.rows.is_none() && v.chip.is_empty() && v.notifs.is_empty() && !v.retry, "{h:?}");
        let html = Leaves { v: &v }.render().unwrap();
        assert!(html.contains(h.text(Lang::Zh)) && !html.contains("es-row"), "{h:?}");
    }
    let c = Card { n: 1, hold: Some(Hold::Identifying), ..Card::default() };
    assert_eq!(view(&c, false, Lang::Zh).state, "正在识别卡…");
}

#[test]
fn the_other_cards_job_reads_as_shared() {
    let mut b = card2();
    b.take_list(Ok(Listed::Busy));
    let v = view(&b, false, Lang::Zh);
    assert!(v.reason.unwrap().contains("可能是另一张卡"));
}

#[test]
fn requests_name_the_card_only_on_a_dual_slot_unit() {
    let single = Aim::default();
    assert_eq!(single.q(), "");
    assert_eq!(single.body(json!({"iccid": "1"})), json!({"iccid": "1"}));
    let two = Aim { n: 2 };
    assert_eq!(two.q(), "?slot=2");
    assert_eq!(two.body(json!({})), json!({"slot": 2}));
    assert_eq!(two.body(json!({"code": "x"})), json!({"code": "x", "slot": 2}));
    // what the core's answers mean for a dual-slot card
    assert_eq!(hold_of(&Fail::Refused(503, String::new(), String::new())), Some(Hold::Identifying));
    assert_eq!(hold_of(&Fail::Refused(404, String::new(), String::new())), Some(Hold::Identifying));
    assert_eq!(hold_of(&Fail::Refused(502, String::new(), String::new())), None);
    assert_eq!(hold_of(&Fail::Unreachable), None);
}

#[test]
fn each_block_has_its_own_form_and_retry_ids() {
    let v = json!({"_esNick": "n", "_esCode": "A", "_esConf": "B", "_esCode2": "C", "_esConf2": "D"});
    let f = form_of(&v, 0);
    assert_eq!((f.nick.as_str(), f.code.as_str(), f.conf.as_str()), ("n", "A", "B"));
    let f = form_of(&v, 2);
    assert_eq!((f.code.as_str(), f.conf.as_str()), ("C", "D"));
    assert_eq!(retry_in(0, RETRY_EMPTY.into()), RETRY_EMPTY);
    assert_eq!(retry_in(2, RETRY_EMPTY.into()), "<div id=\"es-retry-s2\"></div>");
    let r = Retry { tries: 2, next_unix: 0, until_unix: 0 };
    let h = retry_in(2, retry_html(Some(&r), false, Lang::Zh));
    assert!(h.contains("id=\"es-retry-s2\"") && h.contains("/router/esim/stop?s=2"), "{h}");
    let a = after_in("A0", 2);
    assert!(a.contains("id=\"es-after-s2\"") && a.contains("after?k=A0&s=2'"), "{a}");
    assert_eq!(after_in("A0", 0), after_html("A0"));
}

#[test]
fn the_page_knows_which_blocks_it_was_built_with() {
    assert_eq!(signals_init(&[0]), "{_esEdit: '', _esNick: '', _esCode: '', _esConf: ''}");
    assert_eq!(signals_init(&[1, 2]), "{_esEdit: '', _esNick: '', _esCode1: '', _esConf1: '', _esCode2: '', _esConf2: ''}");
    for ns in [vec![0], vec![2], vec![1, 2], vec![]] {
        assert_eq!(built_of(&built_arg(&ns)), ns);
    }
    assert_eq!(built_of(""), vec![0], "an old page's stream");
    assert_eq!(built_arg(&[0]), "", "the single-slot unit's stream URL as before");
    let lay = Layout::Dual { seats: vec![], table: true };
    assert!(cards_html(&lay, &[], false, Lang::Zh).contains("都没有 eSIM 卡"));
    let moved = cards_html(&lay, &[2], false, Lang::Zh);
    assert!(moved.contains("重新载入") && !moved.contains("data-init"), "asks, never reloads under the user's input");
    // built from a cold hub as one card: reloads by itself
    assert!(cards_html(&lay, &[0], true, Lang::Zh).contains("data-init=\"window.location.reload()\""));
    assert_eq!(cards_html(&Layout::Single, &[0], true, Lang::Zh), "<div id=\"es-cards\" hidden></div>");
    assert_eq!(cards_html(&Layout::Single, &[0], false, Lang::Zh), "<div id=\"es-cards\" hidden></div>");
}

#[test]
fn a_standby_slot_switch_does_not_cut_data() {
    let r = Req { name: "狮城".into(), aim: Aim { n: 2 }, standby: true, ..Req::default() };
    let c = confirm(Act::Switch, &r, Some("英国卡"), 0, Lang::Zh);
    assert_eq!(c.tier, write::Tier::Dialog);
    assert!(!c.cuts_uplink && c.downtime.is_none());
    assert!(c.what.contains("移动数据不会中断") && c.recovery.unwrap().contains("切回「英国卡」"));
    let d = confirm(Act::Switch, &Req { standby: false, ..r }, Some("英国卡"), 0, Lang::Zh);
    assert!(d.cuts_uplink && d.downtime.is_some(), "the data slot's switch is as before");
}
