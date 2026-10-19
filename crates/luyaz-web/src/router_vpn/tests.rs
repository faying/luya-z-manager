use super::*;
use crate::hub::Hub;
use crate::sse::Frame;
use std::sync::{Arc, Mutex};

fn hub_with(alg: Value, stale: bool) -> Arc<Hub> {
    let hub = Hub::new();
    hub.apply(&Frame {
        event: "snapshot".into(),
        data: json!({"epoch": "e", "blocks": {
            "router": {"revision": 1, "observed_at": 1, "stale": stale, "data": {"alg": alg, "upnp": null}},
        }})
        .to_string(),
    });
    hub.set_link_for_test(Link::Live);
    hub
}

fn b31() -> Value {
    json!({"sip": true, "l2tp": null, "pptp": null, "ipsec": null})
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
fn b31_reports_only_sip() {
    let hub = hub_with(b31(), false);
    let v = hub.read(|st| view(st, None, Lang::Zh));
    assert_eq!(v.tone, "neutral");
    assert_eq!(v.state, "这版固件不报告 VPN 穿透");
    assert!(v.reason.as_deref().unwrap().contains("SIP ALG（开"));
    assert_eq!(v.sip, Some(true));
    let rows = v.rows.as_ref().unwrap();
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|r| r.on.is_none() && r.sub.contains("读不回来")));
    assert!(!v.dis);
    let h = html(&v);
    assert!(h.contains(r#"id="vpn-l2tp""#) && h.contains("状态不明") && h.contains("SIP ALG"));
    // unreported: two buttons, no switch
    assert!(!h.contains(r#"role="switch""#));
    assert!(h.contains("@post('/router/vpn/ask?f=ipsec&on=true')") && h.contains("@post('/router/vpn/ask?f=ipsec&on=false')"));
    assert!(h.contains(r#"aria-label="关掉 PPTP 穿透""#));
    // a reported field keeps its single switch
    let hub = hub_with(json!({"sip": true, "l2tp": true, "pptp": null, "ipsec": null}), false);
    let h = html(&hub.read(|st| view(st, None, Lang::Zh)));
    assert_eq!(h.matches(r#"role="switch""#).count(), 1);
    assert!(h.contains("@post('/router/vpn/ask?f=l2tp')"));
    assert_eq!(h.matches("&on=").count(), 4);
}

#[test]
fn reported_states_unread_and_stale() {
    let hub = hub_with(json!({"sip": null, "l2tp": true, "pptp": false, "ipsec": null}), false);
    let v = hub.read(|st| view(st, None, Lang::En));
    assert_eq!((v.tone, v.state.as_str()), ("ok", "On: L2TP"));
    assert_eq!(v.reason.as_deref(), Some("Not reported: IPSec"));
    assert!(v.sip.is_none() && !html(&v).contains("SIP ALG"));

    let hub = hub_with(json!({"l2tp": false, "pptp": false, "ipsec": false}), false);
    assert_eq!(hub.read(|st| view(st, None, Lang::Zh)).state, "穿透全部关着");

    let hub = hub_with(json!({}), false);
    assert_eq!(hub.read(|st| view(st, None, Lang::Zh)).tone, "warn");

    // the read failed: alg null → bad, retry, no rows
    let hub = hub_with(Value::Null, false);
    let v = hub.read(|st| view(st, None, Lang::Zh));
    assert!(v.tone == "bad" && v.retry && v.rows.is_none() && v.dis);
    assert!(html(&v).contains("nd-skel"));
    assert!(hub.read(|st| gate(st, "l2tp", Lang::Zh)).is_err());

    // stale: kept, greyed, switches off until refreshed
    let hub = hub_with(json!({"l2tp": true, "pptp": false, "ipsec": false}), true);
    let v = hub.read(|st| view(st, None, Lang::Zh));
    assert!(v.tone == "stale" && v.dis && v.stale && v.retry);
    assert!(html(&v).contains("刷新"));
    assert!(hub.read(|st| gate(st, "l2tp", Lang::Zh)).is_err());

    // nothing yet
    let hub = Hub::new();
    let v = hub.read(|st| view(st, None, Lang::Zh));
    assert_eq!(v.state, "正在读取 VPN 穿透…");
}

#[test]
fn busy_row_and_confirm_tiers() {
    let hub = hub_with(b31(), false);
    let v = hub.read(|st| view(st, Some(("pptp", true)), Lang::Zh));
    assert!(v.dis);
    assert_eq!(v.rows.as_ref().unwrap()[1].busy.as_deref(), Some("正在打开…"));
    let c = confirm("pptp", true, Via::Local, Lang::Zh);
    assert_eq!(c.tier, write::Tier::Inline);
    assert_eq!(c.action, "确认：打开 PPTP 穿透");
    let c = confirm("ipsec", false, Via::Tailnet, Lang::En);
    assert!(c.tier == write::Tier::Dialog && c.cuts_uplink && !c.danger);
    assert_eq!(c.title, "Change IPSec passthrough?");
    // what the confirm asks for: the other state when reported, the button's when not
    assert_eq!(target(Some(true), Some(true)), Some(false));
    assert_eq!(target(Some(false), None), Some(true));
    assert_eq!(target(None, Some(false)), Some(false));
    assert_eq!(target(None, None), None);
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
async fn writes_send_the_agents_args_once_and_read_back() {
    let (url, seen) = control().await;
    let hub = hub_with(json!({"sip": true, "l2tp": false, "pptp": false, "ipsec": null}), false);
    let http = reqwest::Client::new();
    let h2 = hub.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        h2.apply(&Frame {
            event: "block".into(),
            data: json!({"epoch":"e","name":"router","revision":2,"observed_at":2,"stale":false,
                "data":{"alg":{"sip":true,"l2tp":true,"pptp":false,"ipsec":null}}})
            .to_string(),
        });
    });
    let o = write::execute(&http, &url, &hub, &write_of("l2tp", true, true, Lang::Zh), Lang::Zh).await;
    assert_eq!(o, write::Outcome::Applied("已打开 L2TP 穿透".into()));
    // not reported: nothing to read back, accepted at once
    let o = write::execute(&http, &url, &hub, &write_of("ipsec", false, false, Lang::Zh), Lang::Zh).await;
    assert_eq!(o, write::Outcome::Accepted);
    let b = seen.lock().unwrap().clone();
    assert_eq!(b.len(), 2, "one POST each");
    assert_eq!(b[0]["action"], "vendor.call");
    assert_eq!(
        b[0]["params"],
        json!({"object": "zwrt_router.api", "method": "router_set_alg_switch", "args": {"l2tp_passthrough": "1"}})
    );
    assert_eq!(b[1]["params"]["args"], json!({"ipsec_passthrough": "0"}));
    assert_ne!(b[0]["op_id"], b[1]["op_id"]);
    assert!(unconfirmed("IPSec", Lang::Zh).text(Lang::Zh).starts_with("设备已接受，读不回来确认"));
}

#[test]
fn the_english_page_has_no_chinese() {
    for alg in [b31(), json!({"sip": false, "l2tp": true, "pptp": null, "ipsec": false}), Value::Null] {
        for stale in [false, true] {
            let hub = hub_with(alg.clone(), stale);
            let v = hub.read(|st| view(st, Some(("l2tp", false)), Lang::En));
            assert!(!cjk(&html(&v)), "{}", html(&v));
        }
    }
    for (k, on, remote) in [("l2tp", true, Via::Tailnet), ("ipsec", false, Via::Local)] {
        let c = confirm(k, on, remote, Lang::En);
        for s in [&c.title, &c.what, &c.action, c.downtime.as_ref().unwrap(), c.recovery.as_ref().unwrap()] {
            assert!(!cjk(s), "{s}");
        }
    }
    assert!(!cjk(&unconfirmed("L2TP", Lang::En).text(Lang::En)));
}
