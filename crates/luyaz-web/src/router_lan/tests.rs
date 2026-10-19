use super::*;
use crate::hub::Hub;
use crate::sse::Frame;
use std::sync::{Arc, Mutex};

/// Synthetic values only (documentation-style LAN).
fn dhcp() -> Value {
    json!({"ip": "192.168.0.1", "netmask": "255.255.255.0", "start": "100", "limit": "50", "leasetime": "43200", "dhcp_enabled": true})
}

fn hub_with(d: Value) -> Arc<Hub> {
    let hub = Hub::new();
    hub.apply(&Frame {
        event: "snapshot".into(),
        data: json!({"epoch": "e", "blocks": {"dhcp": {"revision": 1, "observed_at": 1, "stale": false, "data": d}}})
            .to_string(),
    });
    hub.set_link_for_test(Link::Live);
    hub
}

fn draft(ip: &str, mask: &str, on: bool, s: &str, e: &str, lease: &str) -> Draft {
    Draft {
        ip: ip.into(),
        mask: mask.into(),
        on,
        start: s.into(),
        end: e.into(),
        lease: lease.into(),
    }
}

fn said(r: Result<Plan, write::Outcome>) -> String {
    match r {
        Err(o) => o.text(Lang::Zh),
        Ok(p) => format!("ok {p:?}"),
    }
}

#[test]
fn reads_the_dhcp_block_and_computes_the_end() {
    let h = hub_with(dhcp());
    let c = h.read(cur).unwrap();
    assert_eq!(
        c.range(),
        Some((
            "192.168.0.100".parse().unwrap(),
            "192.168.0.149".parse().unwrap()
        ))
    );
    assert_eq!(c.lease(), Some(43200));
    let v = h.read(|st| view(st, Lang::Zh));
    assert_eq!((v.tone, v.state.as_str()), ("ok", "DHCP 已开启"));
    assert_eq!(
        v.reason.as_deref(),
        Some("分配 192.168.0.100 – 192.168.0.149")
    );
    assert_eq!(
        v.meta.as_deref(),
        Some("U60 在 192.168.0.1 / 255.255.255.0")
    );
    let s = Leaves { v: &v }.render().unwrap();
    assert!(s.contains("12 小时") && s.contains("192.168.0.149") && s.contains("<!--leaf-->"));

    // the start is an offset from the network, not the last octet (a /16)
    let mut d = dhcp();
    d["ip"] = json!("10.0.0.1");
    d["netmask"] = json!("255.255.0.0");
    d["start"] = json!("300");
    d["limit"] = json!("100");
    let c = hub_with(d).read(cur).unwrap();
    assert_eq!(
        c.range(),
        Some(("10.0.1.44".parse().unwrap(), "10.0.1.143".parse().unwrap()))
    );
}

#[test]
fn unknown_is_a_dash_not_off() {
    // the recording's block: no netmask, no dhcp_enabled
    let h =
        hub_with(json!({"ip": "192.168.0.1", "start": "2", "limit": "252", "leasetime": "86400"}));
    let v = h.read(|st| view(st, Lang::En));
    assert_eq!(v.state, "DHCP state unknown");
    let s = Leaves { v: &v }.render().unwrap();
    assert!(s.contains("1 day"));
    assert_eq!(v.rows[1].value, "—"); // netmask
    assert_eq!(v.rows[2].value, "—"); // DHCP
    assert_eq!(v.rows[3].value, "—"); // no mask, no range
    // empty ip: warn, change locked
    let mut d = dhcp();
    d["ip"] = json!("");
    let h = hub_with(d);
    let v = h.read(|st| view(st, Lang::Zh));
    assert_eq!((v.tone, v.state.as_str()), ("warn", "读不到局域网地址"));
    let c = h.read(cur);
    assert!(
        said(check(
            &draft("192.168.0.1", "255.255.255.0", true, "100", "149", "12h"),
            c.as_ref(),
            Lang::Zh
        ))
        .contains("还没读到")
    );
}

#[test]
fn lease_times() {
    assert_eq!(lease_secs("86400"), Some(86400));
    assert_eq!(lease_secs("12h"), Some(43200));
    assert_eq!(lease_secs(" 30m "), Some(1800));
    assert_eq!(lease_secs("1w"), Some(604_800));
    for bad in ["", "h", "12x", "-5", "1.5h", "infinite"] {
        assert_eq!(lease_secs(bad), None, "{bad}");
    }
    assert_eq!(lease_text(86400, Lang::En), "1 day");
    assert_eq!(lease_text(90, Lang::Zh), "90 秒");
}

#[test]
fn checks() {
    let h = hub_with(dhcp());
    let c = h.read(cur);
    let ck = |d: Draft| said(check(&d, c.as_ref(), Lang::Zh));
    let ok = |ip, mask, s, e, lease| ck(draft(ip, mask, true, s, e, lease));
    assert!(ok("192.168.0.1", "255.255.255.0", "100", "149", "43200").contains("没有改动"));
    // a lease-only change; host numbers work
    let r = check(
        &draft("192.168.0.1", "255.255.255.0", true, "100", "149", "1d"),
        c.as_ref(),
        Lang::Zh,
    )
    .unwrap();
    assert!(!r.renumber && r.range_changed && r.lease == 86400);
    assert_eq!(r.end, "192.168.0.149".parse::<Ipv4Addr>().unwrap());
    // IPv4, private, contiguous mask
    assert!(ok("192.168.0.256", "255.255.255.0", "100", "149", "12h").contains("不是 IPv4"));
    assert!(ok("8.8.8.1", "255.255.255.0", "100", "149", "12h").contains("私有地址"));
    assert!(ok("192.168.0.1", "255.0.255.0", "100", "149", "12h").contains("不是有效的掩码"));
    assert!(ok("192.168.0.1", "255.255.255.255", "100", "149", "12h").contains("255.255.255.252"));
    assert!(ok("192.168.0.0", "255.255.255.0", "100", "149", "12h").contains("网络地址或广播地址"));
    // a new subnet: the old full addresses are out of it, host numbers move with it
    assert!(
        ok(
            "192.168.8.1",
            "255.255.255.0",
            "192.168.0.100",
            "192.168.0.149",
            "12h"
        )
        .contains("不在局域网网段（192.168.8.0/24）里")
    );
    let p = check(
        &draft("192.168.8.1", "255.255.255.0", true, "100", "149", "12h"),
        c.as_ref(),
        Lang::Zh,
    )
    .unwrap();
    assert!(p.renumber && p.ip_changed && p.start == "192.168.8.100".parse::<Ipv4Addr>().unwrap());
    // the range: in the subnet, ordered, not network/broadcast/the U60
    assert!(ok("192.168.0.1", "255.255.255.0", "0", "149", "12h").contains("超出了网段"));
    assert!(ok("192.168.0.1", "255.255.255.0", "100", "255", "12h").contains("超出了网段"));
    assert!(
        ok("192.168.0.1", "255.255.255.0", "150", "149", "12h").contains("起始地址在结束地址后面")
    );
    assert!(ok("192.168.0.1", "255.255.255.0", "1", "149", "12h").contains("U60 自己的地址"));
    assert!(
        ok("192.168.0.50", "255.255.255.0", "2", "149", "12h")
            .contains("包含了 U60 自己的地址 192.168.0.50")
    );
    assert!(ok("192.168.0.1", "255.255.255.0", "abc", "149", "12h").contains("填主机号"));
    assert!(ok("192.168.0.1", "255.255.255.0", "100", "149", "60").contains("至少 2 分钟"));
    // a range that already holds the U60: a lease-only change still goes through
    let mut d = dhcp();
    d["ip"] = json!("192.168.0.120");
    let c2 = hub_with(d).read(cur);
    assert!(
        check(
            &draft("192.168.0.120", "255.255.255.0", true, "100", "149", "1d"),
            c2.as_ref(),
            Lang::Zh
        )
        .is_ok()
    );
    // stale: no change
    let mut d = dhcp();
    d["ip"] = json!("192.168.0.1");
    let h = Hub::new();
    h.apply(&Frame { event: "snapshot".into(), data: json!({"epoch": "e", "blocks": {"dhcp": {"revision": 1, "observed_at": 1, "stale": true, "data": d}}}).to_string() });
    h.set_link_for_test(Link::Live);
    assert!(
        said(check(
            &draft("192.168.0.1", "255.255.255.0", false, "100", "149", "12h"),
            h.read(cur).as_ref(),
            Lang::Zh
        ))
        .contains("还没读到")
    );
    assert_eq!(h.read(|st| view(st, Lang::Zh)).tone, "stale");
}

#[test]
fn the_params_are_lan_set() {
    let c = hub_with(dhcp()).read(cur);
    let p = check(
        &draft("192.168.8.1", "255.255.255.0", false, "2", "200", "12h"),
        c.as_ref(),
        Lang::Zh,
    )
    .unwrap();
    assert_eq!(
        params(&p),
        json!({"ip": "192.168.8.1", "netmask": "255.255.255.0", "dhcp_disabled": 1,
               "dhcp_start": "192.168.8.2", "dhcp_end": "192.168.8.200", "lease_seconds": "43200"})
    );
    let w = lan_write(&p, Lang::Zh);
    assert_eq!((w.action, w.timeout), ("lan.set", Duration::from_secs(45)));
    assert_eq!(w.applied, "局域网设置已生效：U60 现在的地址是 192.168.8.1");
}

#[test]
fn tiers() {
    let c = hub_with(dhcp()).read(cur).unwrap();
    let p = |d: Draft| check(&d, Some(&c), Lang::En).unwrap();
    // new address: dialog even on the LAN, with the new link
    let cf = confirm(
        &p(draft(
            "192.168.8.1",
            "255.255.255.0",
            true,
            "100",
            "149",
            "12h",
        )),
        &c,
        Via::Local,
        Lang::En,
    );
    assert_eq!(cf.tier, write::Tier::Dialog);
    assert!(
        cf.what.contains("http://192.168.8.1:9090")
            && cf.recovery.unwrap().contains("http://192.168.8.1:9090")
    );
    // netmask only: dialog
    let cf = confirm(
        &p(draft(
            "192.168.0.1",
            "255.255.0.0",
            true,
            "100",
            "149",
            "12h",
        )),
        &c,
        Via::Local,
        Lang::En,
    );
    assert_eq!(cf.tier, write::Tier::Dialog);
    assert!(!cf.what.contains(":9090"));
    // DHCP off / range: inline locally, dialog remotely
    let off = p(draft(
        "192.168.0.1",
        "255.255.255.0",
        false,
        "100",
        "149",
        "12h",
    ));
    let cf = confirm(&off, &c, Via::Local, Lang::Zh);
    assert_eq!(cf.tier, write::Tier::Inline);
    assert!(cf.what.contains("DHCP 关闭") && cf.downtime.is_none());
    assert_eq!(confirm(&off, &c, Via::Tailnet, Lang::Zh).tier, write::Tier::Dialog);
}

#[test]
fn readback_and_notes() {
    let c = hub_with(dhcp()).read(cur);
    let p = check(
        &draft("192.168.0.1", "255.255.255.0", true, "100", "199", "12h"),
        c.as_ref(),
        Lang::Zh,
    )
    .unwrap();
    assert!(!hub_with(dhcp()).read(|st| holds(&p, st)));
    let mut d = dhcp();
    d["limit"] = json!("100");
    assert!(hub_with(d.clone()).read(|st| holds(&p, st)));
    d["leasetime"] = json!("12h");
    assert!(hub_with(d).read(|st| holds(&p, st)), "12h = 43200");

    let ip = Some("192.168.8.1".parse().unwrap());
    let n = note(Lang::Zh, Note::Pending, ip);
    assert!(
        n.contains("如果这个页面不再回应")
            && n.contains("href=\"http://192.168.8.1:9090\"")
            && n.contains("重新登录")
    );
    assert!(note(Lang::En, Note::StillHere, ip).contains("not confirmed"));
    assert_eq!(
        note(Lang::Zh, Note::Pending, None),
        "<div id=\"lan-new\"></div>"
    );
    // by the old LAN address: the tab dies with it
    assert_eq!(
        done_note(&write::Outcome::Applied(String::new()), true),
        Note::Moved
    );
    assert_eq!(done_note(&write::Outcome::Accepted, true), Note::StillHere);
    assert_eq!(
        done_note(&write::Outcome::Failed(String::new()), true),
        Note::None
    );
    // by another address (Tailscale): the tab survives, no "still answers here" inference
    assert_eq!(
        done_note(&write::Outcome::Applied(String::new()), false),
        Note::Elsewhere
    );
    assert_eq!(done_note(&write::Outcome::Accepted, false), Note::None);
    let e = note(Lang::Zh, Note::Elsewhere, ip);
    assert!(
        e.contains("局域网里的设备之后用")
            && e.contains("这个页面照常可用")
            && !e.contains("重新登录"),
        "{e}"
    );
    let old = Some("192.168.0.1".parse().unwrap());
    let h = |v: &str| {
        let mut m = axum::http::HeaderMap::new();
        m.insert(axum::http::header::HOST, v.parse().unwrap());
        m
    };
    assert!(by_lan_address(&h("192.168.0.1:8080"), old));
    assert!(by_lan_address(&h("192.168.0.1"), old));
    assert!(!by_lan_address(&h("100.64.0.7:8080"), old));
    assert!(!by_lan_address(&h("u60.tailnet.example:8080"), old));
    assert!(!by_lan_address(&h("192.168.0.1:8080"), None));
    // the area carries the new address to cancel, for a lost reply
    let a = area(
        Lang::Zh,
        Via::Local,
        "/router/lan/set?t=x",
        ip,
        &write::Phase::Idle,
    );
    assert!(
        a.contains("/router/lan/cancel?ip=192.168.8.1&#38;lost=true"),
        "{a}"
    );
}

#[test]
fn forms_render_open_and_closed() {
    let h = hub_with(dhcp());
    let closed = h.read(|st| form(false, st, Lang::Zh));
    assert!(
        closed.contains("id=\"lan-btn\"")
            && closed.contains("更改")
            && closed.contains("<div id=\"lan-form\" class=\"lan-form\"></div>")
    );
    let open = h.read(|st| form(true, st, Lang::Zh));
    assert!(
        open.contains("aria-expanded=\"true\"")
            && open.contains("data-bind=\"_nip\"")
            && open.contains("filterSignals")
    );
    assert!(
        open.contains("192.168.0.100") && open.contains("192.168.0.149") && open.contains("43200"),
        "{open}"
    );
}

/// A stand-in `/control` that records the body and answers ok.
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
async fn a_change_reads_back_from_the_dhcp_block() {
    let (url, seen) = control().await;
    let h = hub_with(dhcp());
    let p = check(
        &draft("192.168.0.1", "255.255.255.0", false, "100", "149", "43200"),
        h.read(cur).as_ref(),
        Lang::Zh,
    )
    .unwrap();
    let h2 = h.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut d = dhcp();
        d["dhcp_enabled"] = json!(false);
        h2.apply(&Frame { event: "block".into(), data: json!({"epoch":"e","name":"dhcp","revision":2,"observed_at":2,"stale":false,"data":d}).to_string() });
    });
    let o = write::execute(
        &reqwest::Client::new(),
        &url,
        &h,
        &lan_write(&p, Lang::Zh),
        Lang::Zh,
    )
    .await;
    assert_eq!(o, write::Outcome::Applied("局域网设置已生效".into()));
    let b = seen.lock().unwrap().clone();
    assert_eq!(b.len(), 1);
    assert_eq!(b[0]["action"], "lan.set");
    assert_eq!(b[0]["params"]["dhcp_disabled"], 1);
}
