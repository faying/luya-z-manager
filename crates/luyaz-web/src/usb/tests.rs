use super::*;
use crate::sse::Frame;
use std::sync::{Arc, Mutex};

fn blk(data: Value) -> Value {
    json!({"revision": 1, "observed_at": 1, "stale": false, "data": data})
}

/// The B31 device as recorded: mode "user", nothing plugged in, fast charging off.
fn hub_with(cc: i64, pb: i64, mode: &str) -> Arc<Hub> {
    let hub = Hub::new();
    hub.apply(&Frame {
        event: "snapshot".into(),
        data: json!({"epoch": "e", "blocks": {
            "qos": blk(json!({"usb_mode": mode, "qci": 9, "bearer": "nr_pdu", "ambr_dl": "1", "ambr_ul": "1", "stale": false})),
            "typec": blk(json!({"cc_attch_state": cc, "data_role": "device", "power_role": "sink"})),
            "powerbank": blk(json!({"state": pb})),
            "battery": blk(json!({"charger_connect": 1, "charger_type": 2, "charging": 1})),
            "charger": blk(json!({"direct_supply": {"supported": true, "enabled": false, "mode": "disable"}})),
        }})
        .to_string(),
    });
    hub.set_link_for_test(Link::Live);
    hub
}

fn agent(settable: Option<bool>) -> Agent {
    let mut v = json!({"connect": 0, "mode": "rndis", "typec_cc": "cc1", "usb2rj45": 0});
    if let Some(s) = settable {
        v["mode_settable"] = json!(s);
    }
    let mut a = Agent::default();
    a.update(Ok(v));
    a
}

fn html(v: &View) -> String {
    Leaves { v }.render().unwrap()
}

fn cjk(s: &str) -> bool {
    s.chars().any(|c| {
        ('\u{4e00}'..='\u{9fff}').contains(&c)
            || ('\u{3000}'..='\u{303f}').contains(&c)
            || ('\u{ff00}'..='\u{ffef}').contains(&c)
    })
}

#[test]
fn agent_fields_like_the_old_page() {
    let p = port(
        &json!({"connect": "1", "typec_cc": "no_cc", "mode": "mtp", "usb2rj45": 1, "mode_settable": false}),
    );
    assert_eq!(
        p,
        Port {
            connect: Some(true),
            cc: Some("no_cc".into()),
            mode: Some("mtp".into()),
            rj45: Some(true),
            settable: Some(false)
        }
    );
    assert_eq!(port(&json!({})).settable, None, "absent = unknown");
    assert_eq!(tri(&json!("maybe")), None);
}

#[test]
fn values_come_from_the_blocks_and_the_agent() {
    let hub = hub_with(0, 0, "user");
    let v = hub.read(|st| view(st, &agent(None), &Busy::default(), Lang::Zh));
    // mode from qos (not the agent's "rndis"); "user" is none of the three
    assert_eq!(v.state, "没插 USB 线 · 模式 USER");
    assert_eq!(v.other.as_deref(), Some("user"));
    assert!(v.modes.iter().all(|m| !m.2), "no radio selected");
    assert!(
        !v.mode_dis && !v.fixed,
        "unknown settable: like the old page"
    );
    // nothing plugged in (typec block): fast charging can't be switched
    assert!(v.pb_dis && v.pb == Some(false));
    assert!(v.pb_sub.starts_with("先在 USB-C 口插上手机"));
    let h = html(&v);
    for s in [
        "已断开",
        "cc1",
        "USER",
        "没有",
        "充电器已接入",
        "允许充电",
        "disable",
    ] {
        assert!(h.contains(s), "{s}");
    }
    assert!(
        h.contains("<span class=\"nd-row__value\">是</span>"),
        "charger connected from battery block"
    );
    // plugged in, mode known: the radio is selected and the switch works
    let hub = hub_with(1, 1, "mtp");
    let v = hub.read(|st| view(st, &agent(Some(true)), &Busy::default(), Lang::Zh));
    assert!(v.modes.iter().any(|m| m.0 == "mtp" && m.2) && v.other.is_none());
    assert!(!v.pb_dis && v.pb == Some(true));
    assert!(v.reason.as_deref().unwrap().contains("快速充电宝开着"));
    let h = html(&v);
    assert!(
        h.contains("aria-checked=\"true\" data-selected=\"true\""),
        "{h}"
    );
    assert!(
        !h.contains("/usb/mode/ask?mode=mtp"),
        "the current mode posts nothing"
    );
    assert!(h.contains("/usb/mode/ask?mode=rndis"));
}

#[test]
fn settable_false_is_read_only_before_any_press() {
    let hub = hub_with(1, 0, "mtp");
    let a = agent(Some(false));
    let v = hub.read(|st| view(st, &a, &Busy::default(), Lang::Zh));
    assert!(v.fixed && v.mode_dis);
    let h = html(&v);
    assert!(h.contains("这版固件不能改 USB 模式"));
    assert!(!h.contains("/usb/mode/ask"), "no mode button posts");
    assert_eq!(h.matches("disabled data-disabled").count(), 3);
    assert_eq!(
        mode_gate(&a, Lang::Zh),
        Some(write::Outcome::NotYet("这版固件不能改 USB 模式".into()))
    );
    assert_eq!(mode_gate(&agent(None), Lang::Zh), None);
    assert_eq!(mode_gate(&agent(Some(true)), Lang::Zh), None);
    let mut down = Agent::default();
    down.update(Err(Fail::Unreachable));
    assert!(matches!(
        mode_gate(&down, Lang::Zh),
        Some(write::Outcome::Failed(_))
    ));
}

#[test]
fn agent_failures_show_in_the_status() {
    let hub = hub_with(0, 0, "user");
    let mut a = Agent::default();
    a.update(Err(Fail::Unreachable));
    let v = hub.read(|st| view(st, &a, &Busy::default(), Lang::Zh));
    assert_eq!((v.tone, v.state.as_str()), ("bad", "读不到 USB 状态"));
    assert!(v.retry && v.mode_dis);
    let mut a = agent(None);
    a.update(Err(Fail::Unreachable));
    let v = hub.read(|st| view(st, &a, &Busy::default(), Lang::Zh));
    assert_eq!(v.tone, "stale");
    assert!(v.meta.as_deref().unwrap().starts_with("数字停在") && v.retry && v.mode_dis);
    // reading
    let v = hub.read(|st| view(st, &Agent::default(), &Busy::default(), Lang::Zh));
    assert_eq!(v.state, "正在读取 USB…");
    // no battery block from a live u60d: the charger group says so
    let h = Hub::new();
    h.set_link_for_test(Link::Down);
    let v = h.read(|st| view(st, &agent(None), &Busy::default(), Lang::Zh));
    assert_eq!(
        v.chg_err.as_deref(),
        Some("读不到充电器信息：连不上数据服务")
    );
    assert!(v.pb_dis && v.chg.iter().all(|r| r.value == "—"));
}

#[test]
fn gates_and_confirms() {
    let hub = hub_with(0, 0, "user");
    assert!(matches!(
        hub.read(|st| pb_gate(st, &agent(None), Lang::Zh)),
        Err(write::Outcome::NotYet(_))
    ));
    let hub = hub_with(1, 0, "user");
    assert_eq!(
        hub.read(|st| pb_gate(st, &agent(None), Lang::Zh)),
        Ok(false)
    );
    // no typec block: the agent's CC decides
    let h = Hub::new();
    h.apply(&Frame {
        event: "snapshot".into(),
        data: json!({"epoch":"e","blocks":{"powerbank": blk(json!({"state": 1}))}}).to_string(),
    });
    h.set_link_for_test(Link::Live);
    let mut nocc = agent(None);
    nocc.port.as_mut().unwrap().cc = Some("no_cc".into());
    assert!(h.read(|st| pb_gate(st, &nocc, Lang::Zh)).is_err());
    assert_eq!(h.read(|st| pb_gate(st, &agent(None), Lang::Zh)), Ok(true));

    let c = mode_confirm("debug", Some("user"), Lang::Zh);
    assert!(c.danger && c.cuts_uplink && c.tier == write::Tier::Dialog);
    assert_eq!(c.title, "把 USB 切换到 DEBUG？");
    assert!(c.recovery.unwrap().contains("（USER）"));
    assert!(c.downtime.unwrap().starts_with("USB 连接会断开"));
    assert!(!mode_confirm("mtp", None, Lang::Zh).danger);
    let c = pb_confirm(true, Lang::Zh);
    assert_eq!(c.tier, write::Tier::Inline);
    assert_eq!(c.action, "确认：打开快速充电宝");
}

/// A stand-in `/control`: records every body.
async fn control() -> (String, Arc<Mutex<Vec<Value>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    let app = axum::Router::new().route(
        "/control",
        axum::routing::post(move |axum::Json(b): axum::Json<Value>| {
            let s = s.clone();
            async move {
                s.lock().unwrap().push(b);
                axum::Json(json!({"ok": true}))
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/control", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (url, seen)
}

#[tokio::test]
async fn writes_send_once_and_read_back_from_the_blocks() {
    let (url, seen) = control().await;
    let hub = hub_with(1, 0, "user");
    let http = reqwest::Client::new();
    let h2 = hub.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        h2.apply(&Frame {
            event: "block".into(),
            data: json!({"epoch":"e","name":"qos","revision":2,"observed_at":2,"stale":false,
                "data":{"usb_mode":"mtp"}})
            .to_string(),
        });
        h2.apply(&Frame {
            event: "block".into(),
            data: json!({"epoch":"e","name":"powerbank","revision":2,"observed_at":2,"stale":false,
                "data":{"state":1}})
            .to_string(),
        });
    });
    let o = write::execute(&http, &url, &hub, &mode_write("mtp", Lang::Zh), Lang::Zh).await;
    assert_eq!(o, write::Outcome::Applied("USB 已切到 MTP".into()));
    let o = write::execute(&http, &url, &hub, &pb_write(true, Lang::Zh), Lang::Zh).await;
    assert_eq!(o, write::Outcome::Applied("已打开快速充电宝".into()));
    let b = seen.lock().unwrap().clone();
    assert_eq!(b.len(), 2, "one POST each");
    assert_eq!(b[0]["action"], "usb.set");
    assert_eq!(b[0]["params"], json!({"mode": "mtp"}));
    assert_eq!(b[1]["action"], "vendor.call");
    assert_eq!(
        b[1]["params"],
        json!({"object": "zwrt_bsp.powerbank", "method": "set", "args": {"state": 1}})
    );
    assert_ne!(b[0]["op_id"], b[1]["op_id"]);
    // nothing changes: "accepted", never success
    let mut w = pb_write(false, Lang::Zh);
    w.timeout = Duration::from_millis(100);
    assert_eq!(
        write::execute(&http, &url, &hub, &w, Lang::Zh).await,
        write::Outcome::Accepted
    );
}

#[test]
fn the_english_page_has_no_chinese() {
    for (hub, a) in [
        (hub_with(0, 0, "user"), agent(Some(false))),
        (hub_with(1, 1, "mtp"), agent(None)),
        (Hub::new(), Agent::default()),
    ] {
        let v = hub.read(|st| {
            view(
                st,
                &a,
                &Busy {
                    mode: true,
                    pb: Some(true),
                },
                Lang::En,
            )
        });
        let h = html(&v);
        assert!(!cjk(&h), "{h}");
        for m in MODES {
            let c = write::Phase::Ask(mode_confirm(m, Some("user"), Lang::En));
            assert!(!cjk(&mode_area(Lang::En, true, m, &c)));
        }
        for on in [true, false] {
            let c = write::Phase::Ask(pb_confirm(on, Lang::En));
            assert!(!cjk(&pb_area(Lang::En, on, &c)));
        }
        if let Some(o) = mode_gate(&a, Lang::En) {
            assert!(!cjk(&o.text(Lang::En)));
        }
        if let Err(o) = hub.read(|st| pb_gate(st, &a, Lang::En)) {
            assert!(!cjk(&o.text(Lang::En)));
        }
    }
}

#[test]
fn a_lost_reply_area_posts_back_with_the_target() {
    let h = mode_area(Lang::Zh, false, "rndis", &write::Phase::Pending("x".into()));
    assert!(
        h.contains("/usb/mode/cancel?want=rndis&#38;lost=true")
            || h.contains("/usb/mode/cancel?want=rndis&amp;lost=true"),
        "{h}"
    );
    let d = mode_area(
        Lang::Zh,
        true,
        "debug",
        &write::Phase::Ask(mode_confirm("debug", None, Lang::Zh)),
    );
    assert!(d.contains("<dialog") && d.contains("Tailscale") && d.contains("nd-btn--danger"));
    assert!(d.contains("/usb/mode/set?mode=debug"));
}
