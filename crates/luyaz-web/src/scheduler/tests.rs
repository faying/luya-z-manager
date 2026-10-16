use super::*;

#[test]
fn forbidden_matches_spelling_variants() {
    for p in [
        "/api/esim/switch",
        "/api/esim",
        "/API/eSIM/profiles/",
        "/api/device/factory-reset/",
        "/api/device/factory-reset?x=1",
        "/api//system/kill-bloat",
        "/api/device/poweroff#a",
        "/api/at/send",
    ] {
        assert!(forbidden(p).is_some(), "{p}");
    }
    for p in [
        "/api/device/reboot",
        "/api/esimx",
        "/api/at/sendx",
        "/api/wifi/apply",
    ] {
        assert!(forbidden(p).is_none(), "{p}");
    }
    assert!(forbidden_text("power off", Lang::Zh).contains("关机"));
    assert!(
        !forbidden_text("eSIM", Lang::En)
            .chars()
            .any(|c| c > '\u{2fff}')
    );
}

#[test]
fn actions_u60d_cannot_run_are_refused_in_the_form() {
    for p in [
        "/api/modem/network-mode",
        "/api/cell/band/nr",
        "/API/cell/lock/lte/",
        "/api/router/apn/profiles?x=1",
        "/api/system/kill",
        "/api/sms/delete",
    ] {
        assert!(not_on_u60d(p).is_some(), "{p}");
        assert!(refusal(p, Lang::Zh).unwrap().contains("没有这个操作"), "{p}");
    }
    for p in [
        "/api/device/reboot",
        "/api/cell/stc/reset",
        "/api/usb/mode",
        "/api/sim/pin/verify",
        "/api/sms/forward/log/clear",
        "/api/cell/bandx",
        "/api/system/top",
    ] {
        assert!(refusal(p, Lang::En).is_none(), "{p}");
    }
    // forbidden keeps its own reason
    assert!(refusal("/api/system/kill-bloat", Lang::Zh).unwrap().contains("一键结束进程"));
    let f = |path: &str| JobIn {
        name: "x".into(),
        method: "PUT".into(),
        path: path.into(),
        ..Default::default()
    };
    let e = check(&f("/api/cell/band/nr"), Lang::En).unwrap_err();
    assert!(e.contains("can't call /api/cell/band"), "{e}");
}

#[test]
fn tier3_is_normalised_and_get_is_never() {
    assert!(tier3("POST", "/api/device/reboot"));
    assert!(tier3("post", "/API/device/reboot/"));
    assert!(tier3("PUT", "/api/cell/band/nr"));
    assert!(tier3("DELETE", "/api/sms/forward/rules"));
    assert!(!tier3("PUT", "/api/sms/forward/rules"));
    assert!(!tier3("GET", "/api/device/reboot"));
    assert!(!tier3("PUT", "/api/device/power-save"));
    assert!(is_reboot("post", "/api/device/reboot/"));
    assert!(!is_reboot("GET", "/api/device/reboot"));
}

#[test]
fn device_clock_digits_round_trip() {
    let at = parse_wall("2026-10-14T04:30").unwrap();
    assert_eq!(fmt_at(at), "2026-10-14 04:30");
    assert_eq!(wall_input(at), "2026-10-14T04:30");
    assert_eq!(parse_wall("2024-02-29T23:59:30"), Some(1_709_251_170));
    assert_eq!(fmt_at(0), "1970-01-01 00:00");
    assert!(parse_wall("2026-13-01T00:00").is_none() && parse_wall("").is_none());
    assert_eq!(parse_time("4:5").as_deref(), Some("04:05"));
    assert!(parse_time("24:00").is_none() && parse_time("0430").is_none());
}

#[test]
fn schedules_read_like_the_old_page() {
    let all = Sched::Recurring {
        time: "00:30".into(),
        days: (0..7).collect(),
    };
    assert_eq!(fmt_sched(&all, Lang::En), "00:30 — daily");
    let mon = Sched::Recurring {
        time: "04:30".into(),
        days: vec![0, 6],
    };
    assert_eq!(fmt_sched(&mon, Lang::En), "04:30 — Mon, Sun", "0 = Monday");
    assert_eq!(fmt_sched(&mon, Lang::Zh), "04:30 — 周一、周日");
    let once = Sched::Once {
        at: Some(parse_wall("2026-10-14T04:30").unwrap()),
    };
    assert_eq!(fmt_sched(&once, Lang::En), "Once at 2026-10-14 04:30");
}

fn input() -> JobIn {
    JobIn {
        name: " Nightly ".into(),
        method: "POST".into(),
        path: "/api/device/reboot".into(),
        kind: "recurring".into(),
        time: "04:30".into(),
        d0: true,
        ..Default::default()
    }
}

#[test]
fn the_form_checks() {
    let d = check(&input(), Lang::En).unwrap();
    assert_eq!(d.name, "Nightly");
    assert_eq!(
        d.sched,
        Sched::Recurring {
            time: "04:30".into(),
            days: vec![0]
        }
    );
    let mut f = input();
    f.d0 = false;
    assert_eq!(
        check(&f, Lang::En).unwrap().sched,
        Sched::Recurring {
            time: "04:30".into(),
            days: (0..7).collect()
        },
        "no day = every day (never an empty list: the agent refuses it)"
    );
    let mut f = input();
    f.path = "/api/esim/switch".into();
    assert!(check(&f, Lang::En).unwrap_err().contains("eSIM"));
    f.path = "/api/scheduler/jobs".into();
    assert!(check(&f, Lang::En).is_err());
    f.path = "/api/auth/login".into();
    assert!(check(&f, Lang::En).is_err());
    let mut f = input();
    f.body = "{nope".into();
    assert!(check(&f, Lang::En).unwrap_err().contains("JSON"));
    f.body = "{\"a\":1}".into();
    assert_eq!(check(&f, Lang::En).unwrap().body, Some(json!({"a": 1})));
    let mut f = input();
    f.kind = "once".into();
    assert!(check(&f, Lang::Zh).unwrap_err().contains("日期"));
    f.at = "2026-10-14T04:30".into();
    assert!(matches!(
        check(&f, Lang::En).unwrap().sched,
        Sched::Once { at: Some(_) }
    ));
    let e = check(&JobIn::default(), Lang::En).unwrap_err();
    assert!(
        e.starts_with("Give the job a name") && e.contains("method"),
        "{e}"
    );
}

fn mock_list() -> Value {
    json!([
        {"id":1,"name":"夜间省电","enabled":true,"schedule":{"type":"recurring","time":"00:30","days":[0,1,2,3,4,5,6]},
         "action":{"method":"PUT","path":"/api/device/power-save","body":{"x":1}},"restore":{"time":"07:30","body":{"x":0}},
         "last_run":1760311800,"last_status":200,"last_restore":1760340600,"last_restore_status":200,"created_at":1},
        {"id":2,"name":"每周一凌晨重启","enabled":false,"schedule":{"type":"recurring","time":"04:30","days":[0]},
         "action":{"method":"POST","path":"/api/device/reboot"},"last_run":null,"last_status":null,
         "last_restore":null,"last_restore_status":null,"created_at":2},
        {"id":3,"name":"old","enabled":false,"schedule":{"type":"once","at":1760400000},
         "action":{"method":"POST","path":"/api/esim/switch"},"last_run":1760300000,"last_status":403,
         "last_error":"定时任务不能调eSIM","last_restore":null,"last_restore_status":null,"created_at":3}
    ])
}

#[test]
fn the_view() {
    let got = Got {
        jobs: jobs(&mock_list()),
        err: None,
        at: Some(SystemTime::now()),
    };
    let v = view(&got, Lang::En);
    assert_eq!(v.tone, "warn");
    assert_eq!(v.state, "1 job(s) failed last time");
    assert_eq!(v.names.as_deref(), Some("old"));
    assert_eq!(v.reboot.1, "No scheduled reboot");
    assert!(v.reboot.2.starts_with("1 reboot job(s) exist"));
    let rows = v.rows.as_ref().unwrap();
    assert!(
        rows[0]
            .restore
            .as_deref()
            .unwrap()
            .starts_with("Restores at 07:30 · last restore ")
    );
    assert_eq!(rows[1].last.1, "Hasn't run yet");
    assert!(rows[2].banned.is_some() && rows[2].last.0 == "bad");
    assert!(!v.locked);

    // the reboot job on: the old /router/schedule's "on" line
    let mut l = mock_list();
    l[1]["enabled"] = json!(true);
    let got = Got {
        jobs: jobs(&l),
        err: None,
        at: None,
    };
    let v = view(&got, Lang::Zh);
    assert_eq!(
        v.reboot,
        ("ok", "定时重启已开".into(), "04:30 — 周一".into())
    );

    // a later read fails: the list stays, greyed, writes locked
    let got = Got {
        jobs: jobs(&l),
        err: Some(Fail::Unreachable),
        at: Some(SystemTime::now()),
    };
    let v = view(&got, Lang::En);
    assert!(
        v.stale && v.locked && v.tone == "stale" && v.meta.unwrap().starts_with("List stopped at")
    );

    let html = Leaves {
        v: &view(
            &Got {
                jobs: jobs(&mock_list()),
                err: None,
                at: None,
            },
            Lang::En,
        ),
    }
    .render()
    .unwrap();
    assert!(
        html.contains("aria-labelledby=\"sch-on-2 sch-n-2\""),
        "switch named by the job's name element"
    );
    assert!(
        html.contains("/scheduler/toggle/ask?id=2&on=true")
            && html.contains("/scheduler/toggle/ask?id=1&on=false")
    );
    for leaf in split_leaves(&html) {
        // names, paths and errors (device data) never in an aria-label
        assert!(!leaf.1.contains("aria-label=\"每"), "{}", leaf.0);
    }
}

#[test]
fn the_form_renders_its_job() {
    let js = jobs(&mock_list()).unwrap();
    let h = form_html(Lang::En, Some(&js[1]), true);
    assert!(h.contains("Edit job") && h.contains("Update"));
    assert!(
        h.contains("&#34;_ji&#34;:2") || h.contains("&quot;_ji&quot;:2"),
        "{h}"
    );
    assert!(h.contains("_jd0&#34;:true") || h.contains("_jd0&quot;:true"));
    let h = form_html(Lang::Zh, None, false);
    assert!(h.contains("新建任务") && h.contains("创建"));
    assert!(!form_closed(Lang::En).contains("section"));
}

#[tokio::test]
async fn router_schedule_redirects_here() {
    let r = to_reboot().await;
    assert_eq!(r.status(), StatusCode::FOUND);
    assert_eq!(r.headers()[header::LOCATION], "/scheduler#reboot");
}
