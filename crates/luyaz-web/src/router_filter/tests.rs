use super::*;
use crate::sse::Frame;
use std::sync::{Arc, Mutex};

fn hub_with(domains: Value, stale: bool) -> Arc<Hub> {
    let hub = Hub::new();
    hub.apply(&Frame {
        event: "snapshot".into(),
        data: json!({"epoch": "e", "blocks": {
            "router": {"revision": 1, "observed_at": 1, "stale": stale,
                       "data": {"blocked_domains": domains, "alg": null}},
        }})
        .to_string(),
    });
    hub.set_link_for_test(Link::Live);
    hub
}

fn hdr(host: &str) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(header::HOST, host.parse().unwrap());
    h
}

#[test]
fn typed_domains_like_the_old_page() {
    assert_eq!(domain("  Example.COM. "), Ok("example.com".into()));
    assert_eq!(
        domain("a-b_c.example.co.uk"),
        Ok("a-b_c.example.co.uk".into())
    );
    assert_eq!(domain("   "), Err(Bad::Empty));
    for bad in [
        "example",
        "http://x.com",
        "x.com/a",
        "a b.com",
        "a..com",
        ".com",
        "x@y.com",
        "x.com'",
    ] {
        assert_eq!(domain(bad), Err(Bad::Shape), "{bad}");
    }
}

#[test]
fn the_host_the_browser_used() {
    assert_eq!(
        host_of(&hdr("192.168.0.1:9090")).as_deref(),
        Some("192.168.0.1")
    );
    assert_eq!(host_of(&hdr("[::1]:80")).as_deref(), Some("::1"));
    assert_eq!(host_of(&hdr("U60.Lan.")).as_deref(), Some("u60.lan"));
    assert_eq!(
        host_of(&hdr("panel.example.com:8080")).as_deref(),
        Some("panel.example.com")
    );
    assert_eq!(host_of(&HeaderMap::new()), None);
}

#[test]
fn a_rule_that_would_cut_off_the_panel() {
    assert!(cuts_panel("example.com", "example.com"));
    assert!(cuts_panel("example.com", "foo.example.com"));
    assert!(cuts_panel("example.com", "notexample.com"));
    // keyword: the rule text anywhere in the host
    assert!(cuts_panel("example.co", "u60.example.com"));
    assert!(cuts_panel("panel.local", "u60.panel.localhost"));
    assert!(!cuts_panel("other.example", "u60.example.com"));
    assert!(!cuts_panel("foo.example.com", "example.com"));
    // an IP address can't be blocked by a domain rule, except one spelled exactly like it
    assert!(!cuts_panel("66.1", "192.168.0.1"));
    assert!(cuts_panel("192.168.0.1", "192.168.0.1"));
    assert!(!cuts_panel("ts.net", "fd7a:115c:a1e0::1"));
    let h = host_of(&hdr("U60.Example.COM.:9090")).unwrap();
    assert!(cuts_panel("example.com", &h));
}

#[test]
fn device_text_is_percent_encoded_for_the_url() {
    assert_eq!(enc("a.b-c_d~"), "a.b-c_d~");
    assert_eq!(enc("x');alert(1)//"), "x%27%29%3Balert%281%29%2F%2F");
    assert_eq!(enc("例.com"), "%E4%BE%8B.com");
}

#[test]
fn null_is_unreadable_not_empty_and_locks_changes() {
    let hub = hub_with(Value::Null, false);
    hub.read(|st| {
        assert_eq!(list(st), List::Unreadable { stale: false });
        let v = view(st, Lang::En);
        assert_eq!(v.tone, "warn");
        assert!(v.dis && v.quick_dis && v.retry);
        assert!(v.rows.is_empty() && v.note.is_some());
        assert!(matches!(
            add_gate(st, "x.com", None, Lang::En),
            Err(Outcome::NotYet(_))
        ));
        assert!(matches!(
            quick_gate(st, None, Lang::En),
            Err(Outcome::NotYet(_))
        ));
    });
    // B31's `{}` (u60d makes it []) is an empty list
    let hub = hub_with(json!([]), false);
    hub.read(|st| {
        let v = view(st, Lang::Zh);
        assert!(!v.dis && v.rows.is_empty());
        assert!(v.note.as_deref().unwrap().contains("还没有拦截"));
        assert_eq!(v.state, "已知遥测域名拦截了 0 / 4 个");
    });
}

#[test]
fn status_counts_the_known_domains() {
    let hub = hub_with(json!(["iot.zte.com.cn", "ads.example.com"]), false);
    hub.read(|st| {
        let v = view(st, Lang::En);
        assert_eq!(v.tone, "warn");
        assert_eq!(v.state, "1 of 4 known telemetry domains blocked");
        assert_eq!(
            v.reason.as_deref(),
            Some("Not blocked: ztems.com, zte.com.cn, ztemt.com.cn")
        );
        assert_eq!(v.rows.len(), 2);
        assert!(v.rows[0].known && !v.rows[1].known);
        assert!(!v.quick_dis);
    });
    let hub = hub_with(json!(KNOWN), false);
    hub.read(|st| {
        let v = view(st, Lang::En);
        assert_eq!((v.tone, v.all_blocked, v.quick_dis), ("ok", true, true));
    });
    // stale: shown, locked
    let hub = hub_with(json!(["a.com"]), true);
    hub.read(|st| {
        let v = view(st, Lang::En);
        assert_eq!(v.tone, "stale");
        assert!(v.dis && v.stale && v.meta.is_some());
        assert_eq!(current(st), None);
    });
}

#[test]
fn loading_until_the_hub_answers() {
    let hub = Hub::new();
    hub.read(|st| {
        assert_eq!(list(st), List::Loading);
        let v = view(st, Lang::En);
        assert!(v.skel && v.dis && !v.retry);
    });
    hub.set_link_for_test(Link::Down);
    hub.read(|st| assert_eq!(list(st), List::Missing(true)));
}

#[test]
fn gates_check_everything_the_ask_checked() {
    let hub = hub_with(json!(["ads.example.com"]), false);
    hub.read(|st| {
        let l = Lang::En;
        assert_eq!(add_gate(st, " New.Example.org ", None, l), Ok("new.example.org".into()));
        assert!(matches!(add_gate(st, "ADS.example.com", None, l), Err(Outcome::NotYet(s)) if s.contains("already")));
        assert!(matches!(add_gate(st, "nope", None, l), Err(Outcome::NotYet(_))));
        // the panel's own name, or a name it sits under: refused, with the host named
        let o = add_gate(st, "example.org", Some("u60.example.org"), l).unwrap_err();
        assert!(matches!(&o, Outcome::Ended(s, "bad") if s.starts_with("Not sent:") && s.contains("u60.example.org")), "{o:?}");
        assert!(add_gate(st, "example.org", Some("192.168.0.1"), l).is_ok());
        // quick block: a known domain covering the host refuses the whole step list
        let o = quick_gate(st, Some("iot.zte.com.cn"), l).unwrap_err();
        assert!(matches!(&o, Outcome::Ended(s, "bad") if s.contains("iot.zte.com.cn")));
        assert_eq!(quick_gate(st, Some("192.168.0.1"), l).unwrap().len(), 4);
        // remove: only what is on the list
        assert_eq!(del_gate(st, "ads.example.com", l), Ok("ads.example.com".into()));
        assert!(del_gate(st, "gone.com", l).is_err());
    });
}

#[test]
fn the_write_is_the_agents_pass_through() {
    assert_eq!(
        params("add", "ztems.com"),
        json!({"object": "zwrt_router.api", "method": "router_set_domain_filter",
               "args": {"action": "add", "domain": "ztems.com"}})
    );
    let w = one_write(false, "a.com", Lang::Zh);
    assert_eq!(w.action, "vendor.call");
    assert_eq!(w.params["args"]["action"], "delete");
}

/// A stand-in `/control`: records each body; fails the domains in `fail`; adds the others to
/// the hub's list (the readback) unless `silent`.
async fn control(
    hub: Arc<Hub>,
    fail: &'static [&'static str],
    silent: bool,
) -> (String, Arc<Mutex<Vec<Value>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    let app = axum::Router::new().route(
        "/control",
        axum::routing::post(move |axum::Json(b): axum::Json<Value>| {
            let (s, hub) = (s.clone(), hub.clone());
            async move {
                s.lock().unwrap().push(b.clone());
                let d = b["params"]["args"]["domain"].as_str().unwrap().to_owned();
                if fail.contains(&d.as_str()) {
                    return axum::Json(json!({"ok": false, "error": {"code": "device_call_failed", "message": "timeout"}}));
                }
                if !silent {
                    let mut list: Vec<Value> = hub.read(|st| st.blocks["router"].data["blocked_domains"].as_array().cloned().unwrap());
                    list.push(json!(d));
                    let rev = s.lock().unwrap().len() as u64 + 1;
                    hub.apply(&Frame {
                        event: "block".into(),
                        data: json!({"epoch": "e", "name": "router", "revision": rev, "observed_at": rev, "stale": false,
                                     "data": {"blocked_domains": list}}).to_string(),
                    });
                }
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
async fn quick_block_sends_each_missing_domain_once_and_reads_them_back() {
    let hub = hub_with(json!(["iot.zte.com.cn"]), false);
    let (url, seen) = control(hub.clone(), &[], false).await;
    let todo = hub.read(|st| quick_gate(st, None, Lang::En)).unwrap();
    assert_eq!(todo, ["ztems.com", "zte.com.cn", "ztemt.com.cn"]);
    let o = quick_run(
        &reqwest::Client::new(),
        &url,
        &hub,
        &todo,
        Duration::from_secs(2),
        Lang::En,
    )
    .await;
    assert_eq!(o, Outcome::Applied("Blocked 3 telemetry domain(s)".into()));
    let b = seen.lock().unwrap().clone();
    assert_eq!(b.len(), 3);
    let ids: std::collections::HashSet<_> = b
        .iter()
        .map(|v| v["op_id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids.len(), 3, "a fresh op_id per step");
    assert_eq!(b[1]["params"], params("add", "zte.com.cn"));
    hub.read(|st| assert!(view(st, Lang::En).all_blocked));
}

#[tokio::test]
async fn a_failed_step_is_named_and_the_rest_is_what_is_left() {
    let hub = hub_with(json!([]), false);
    let (url, seen) = control(hub.clone(), &["zte.com.cn"], false).await;
    let todo = hub.read(|st| quick_gate(st, None, Lang::En)).unwrap();
    let o = quick_run(
        &reqwest::Client::new(),
        &url,
        &hub,
        &todo,
        Duration::from_secs(2),
        Lang::En,
    )
    .await;
    let Outcome::Ended(text, tone) = &o else {
        panic!("{o:?}")
    };
    assert_eq!(*tone, "warn");
    assert!(
        text.starts_with("Blocked: iot.zte.com.cn, ztems.com, ztemt.com.cn."),
        "{text}"
    );
    assert!(text.contains("Not blocked: zte.com.cn (timeout)"), "{text}");
    assert_eq!(
        seen.lock().unwrap().len(),
        4,
        "the others still sent, each once"
    );
    // pressing again: only the missing one
    assert_eq!(
        hub.read(|st| quick_gate(st, None, Lang::En)).unwrap(),
        ["zte.com.cn"]
    );
}

#[tokio::test]
async fn sent_but_not_read_back_is_never_called_blocked() {
    let hub = hub_with(json!([]), false);
    let (url, _) = control(hub.clone(), &[], true).await;
    let o = quick_run(
        &reqwest::Client::new(),
        &url,
        &hub,
        &["ztems.com"],
        Duration::from_millis(100),
        Lang::Zh,
    )
    .await;
    assert_eq!(o, Outcome::Accepted);
    assert_eq!(o.text(Lang::Zh), "设备已接受，还没读回");
}

#[test]
fn the_english_page_has_no_chinese() {
    for d in [
        Value::Null,
        json!([]),
        json!(["iot.zte.com.cn", "a.com"]),
        json!(KNOWN),
    ] {
        for stale in [false, true] {
            let hub = hub_with(d.clone(), stale);
            let html = hub.read(|st| {
                Leaves {
                    v: &view(st, Lang::En),
                }
                .render()
                .unwrap()
            });
            assert!(
                !html.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)
                    || ('\u{3000}'..='\u{303f}').contains(&c)
                    || ('\u{ff00}'..='\u{ffef}').contains(&c)),
                "{html}"
            );
        }
    }
}
