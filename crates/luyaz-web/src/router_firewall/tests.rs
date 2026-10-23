use super::*;
use crate::hub::Hub;
use crate::sse::Frame;
use std::sync::{Arc, Mutex};

fn blk(data: Value) -> Value {
    json!({"revision": 1, "observed_at": 1, "stale": false, "data": data})
}

/// Synthetic values only (documentation-style LAN).
fn router_data() -> Value {
    json!({
        "firewall": {"enabled": true, "level": "medium", "nat": true, "dmz": false, "dmz_ip": "", "port_forward": true, "wan_ping": false, "remote_web": null},
        "upnp": {"enabled": true, "natpmp": 1, "notify_interval": 30, "ttl": null},
        "port_forward": [{"id": "1", "name": "NAS", "protocol": "TCP", "wan_port": "8443", "lan_ip": "192.168.0.20", "lan_port": "443", "enabled": true},
                         {"id": "", "name": "odd", "protocol": "UDP", "wan_port": "1", "lan_ip": "192.168.0.21", "lan_port": "1", "enabled": false}],
        "filter_rules": [],
        "alg": null, "qos": null, "blocked_domains": []
    })
}

fn hub_with(router: Value) -> Arc<Hub> {
    let hub = Hub::new();
    hub.apply(&Frame {
        event: "snapshot".into(),
        data: json!({"epoch": "e", "blocks": {
            "router": blk(router),
            "dhcp": blk(json!({"ip": "192.168.0.1", "netmask": "255.255.255.0", "start": "2", "limit": "252", "leasetime": "86400", "dhcp_enabled": true})),
        }})
        .to_string(),
    });
    hub.set_link_for_test(Link::Live);
    hub
}

fn html(v: &View) -> String {
    Leaves { v }.render().unwrap()
}

fn lan24() -> Option<Lan> {
    Some(Lan {
        ip: "192.168.0.1".parse().unwrap(),
        mask: Some("255.255.255.0".parse().unwrap()),
    })
}

#[test]
fn reads_the_router_block_and_says_dash_for_null() {
    let h = hub_with(router_data());
    let v = h.read(|st| view(st, Lang::Zh));
    assert_eq!(v.tone, "ok");
    assert_eq!(v.state, "防火墙已开启");
    assert_eq!(v.reason.as_deref(), Some("级别：中"));
    assert_eq!(v.sw("fw"), Some(true));
    assert_eq!(v.sw("upnp"), Some(true));
    assert_eq!(
        h.read(lan).map(|n| n.text()).as_deref(),
        Some("192.168.0.0/24")
    );
    // remote_web null: no row (not "off"); wan_ping false: a row
    assert_eq!(v.ro.len(), 1);
    let s = html(&v);
    assert!(
        s.contains("id=\"fw-sws\"")
            && s.contains("k=nat")
            && s.contains("k=pf")
            && s.contains("k=upnp"),
        "{s}"
    );
    assert!(s.contains("2 条规则"));
    // a rule without an id has no delete button
    assert_eq!(s.matches("/router/firewall/del/ask?id=").count(), 1);
    assert!(s.contains("删除规则「NAS」"));
    assert!(s.contains("设备上没有过滤规则"));

    let mut d = router_data();
    d["firewall"]["nat"] = Value::Null;
    d["firewall"]["level"] = json!("");
    d["upnp"] = Value::Null;
    let h = hub_with(d);
    let v = h.read(|st| view(st, Lang::En));
    assert_eq!(v.sw("nat"), None);
    assert!(
        v.level_dis(),
        "B31's empty level: the level buttons are off"
    );
    assert_eq!(v.level_text, "—");
    let s = html(&v);
    assert!(
        s.contains("Couldn&#39;t read UPnP") || s.contains("Couldn't read UPnP"),
        "{s}"
    );
    assert!(
        !s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
        "English leaves without Chinese"
    );
}

#[test]
fn status_off_dmz_and_unreadable() {
    let mut d = router_data();
    d["firewall"]["enabled"] = json!(false);
    let v = hub_with(d.clone()).read(|st| view(st, Lang::Zh));
    assert_eq!((v.tone, v.state.as_str()), ("warn", "防火墙已关闭"));
    d["firewall"]["enabled"] = json!(true);
    d["firewall"]["dmz"] = json!(true);
    d["firewall"]["dmz_ip"] = json!("192.168.0.50");
    let v = hub_with(d.clone()).read(|st| view(st, Lang::Zh));
    assert_eq!(v.tone, "warn");
    assert!(v.reason.unwrap().starts_with("192.168.0.50"));
    assert_eq!(v.dmz, "已启用 · 192.168.0.50");
    d["firewall"] = Value::Null;
    d["port_forward"] = Value::Null;
    let v = hub_with(d).read(|st| view(st, Lang::Zh));
    assert_eq!((v.tone, v.retry), ("bad", true));
    assert!(v.dis);
    let s = html(&v);
    assert!(
        !s.contains("role=\"switch\""),
        "no switches without a reading"
    );
}

#[test]
fn host_check_keeps_to_the_lan() {
    let l = Lang::En;
    assert_eq!(
        host_check(" 192.168.0.20 ", lan24(), l),
        Ok("192.168.0.20".parse().unwrap())
    );
    for bad in [
        "192.168.1.20",
        "192.168.0.1",
        "192.168.0.0",
        "192.168.0.255",
        "192.168.0",
        "300.1.1.1",
        "a.b.c.d",
        "",
    ] {
        assert!(host_check(bad, lan24(), l).is_err(), "{bad}");
    }
    assert!(
        host_check("192.168.1.20", lan24(), Lang::Zh)
            .unwrap_err()
            .contains("192.168.0.0/24")
    );
    // no netmask: IPv4 and not the U60 itself
    let nomask = Some(Lan {
        ip: "192.168.0.1".parse().unwrap(),
        mask: None,
    });
    assert!(host_check("10.1.2.3", nomask, l).is_ok());
    assert!(host_check("192.168.0.1", nomask, l).is_err());
    assert!(host_check("10.1.2.3", None, l).is_ok());
    // ...but the writes need the subnet: no netmask / no dhcp → refused (user decision 10-13)
    for dhcp in [
        json!({"ip": "192.168.0.1", "netmask": ""}),
        json!({"netmask": "255.255.255.0"}),
        Value::Null,
    ] {
        let hub = Hub::new();
        let mut blocks = json!({"router": blk(router_data())});
        if !dhcp.is_null() {
            blocks["dhcp"] = blk(dhcp);
        }
        hub.apply(&Frame {
            event: "snapshot".into(),
            data: json!({"epoch": "e", "blocks": blocks}).to_string(),
        });
        hub.set_link_for_test(Link::Live);
        let o = hub
            .read(|st| dmz_check(true, "192.168.0.50", st, Lang::Zh))
            .unwrap_err();
        assert_eq!(o.text(Lang::Zh), "还没读到局域网网段，稍后再试");
        let f = PfIn {
            name: "x".into(),
            protocol: "TCP".into(),
            wan: "1".into(),
            ip: "192.168.0.30".into(),
            port: "1".into(),
            enabled: true,
        };
        let o = hub.read(|st| pf_check(&f, st, Lang::En)).unwrap_err();
        assert_eq!(
            o.text(Lang::En),
            "LAN subnet not read yet, try again shortly"
        );
        // turning DMZ off still goes (it sends the current host, like the old page)
        assert!(
            hub.read(|st| dmz_check(false, "192.168.0.50", st, Lang::Zh))
                .is_ok()
        );
    }
    assert_eq!(lan24().unwrap().hint(), "192.168.0.x");
    let wide = Lan {
        ip: "192.168.0.1".parse().unwrap(),
        mask: Some("255.255.0.0".parse().unwrap()),
    };
    assert_eq!(
        (wide.text(), wide.hint()),
        ("192.168.0.0/16".to_string(), "192.168.0.0".to_string())
    );
    assert!(host_check("192.168.77.9", Some(wide), l).is_ok());
}

#[test]
fn ports() {
    for ok in ["1", "80", "65535", "8000-8010"] {
        assert!(port_ok(ok), "{ok}");
    }
    for bad in ["0", "65536", "", "-", "80-", "a", "123456", "1-2-3", " 80"] {
        assert!(!port_ok(bad), "{bad}");
    }
}

#[test]
fn the_bodies_are_the_agents() {
    let up = Upnp {
        enabled: Some(true),
        natpmp: Some(1),
        notify_interval: Some(30),
        ttl: None,
    };
    assert_eq!(
        Sw::Fw(false).args(None),
        (
            "router_set_firewall_switch",
            json!({"firewall_switch": "0"})
        )
    );
    assert_eq!(
        Sw::Nat(true).args(None),
        ("router_set_nat_switch", json!({"nat_switch": "1"}))
    );
    assert_eq!(
        Sw::Pf(false).args(None),
        (
            "router_set_portforward_switch",
            json!({"port_forward_switch": "0"})
        )
    );
    assert_eq!(
        Sw::Level("high").args(None),
        (
            "router_set_firewall_level",
            json!({"firewall_level": "high"})
        )
    );
    assert_eq!(
        Sw::Upnp(false).args(Some(&up)),
        (
            "router_set_upnp_switch",
            json!({"enable_upnp": 0, "natpmp": 1, "notify_interval": 30})
        )
    );
    let w = sw_write(Sw::Nat(false), None, Lang::Zh);
    assert_eq!(w.action, "vendor.call");
    assert_eq!(
        w.params,
        json!({"object": "zwrt_router.api", "method": "router_set_nat_switch", "args": {"nat_switch": "0"}})
    );
    let d = dmz_write(
        &Dmz {
            on: true,
            ip: "192.168.0.50".into(),
        },
        Lang::Zh,
    );
    assert_eq!(
        d.params["args"],
        json!({"dmz_enabled": "1", "dmz_ip": "192.168.0.50"})
    );
    let r = Rule {
        id: "7".into(),
        name: "Cam".into(),
        protocol: "Both".into(),
        wan_port: "8000-8010".into(),
        lan_ip: "192.168.0.9".into(),
        lan_port: "80".into(),
        enabled: Some(false),
    };
    assert_eq!(
        pf_write(&r, 0, Lang::Zh).params["args"],
        json!({"action": "add", "name": "Cam", "protocol": "Both", "wan_port": "8000-8010", "lan_ip": "192.168.0.9", "lan_port": "80", "enabled": "0"})
    );
    assert_eq!(
        del_write(&r, Lang::Zh).params["args"],
        json!({"action": "delete", "id": "7"})
    );
}

#[test]
fn tiers_nat_off_and_dmz_always_ask_in_a_dialog() {
    use write::Tier::{Dialog, Inline};
    let l = Lang::Zh;
    assert_eq!(sw_confirm(Sw::Fw(false), Via::Local, lan24(), l).tier, Inline);
    assert_eq!(sw_confirm(Sw::Fw(false), Via::Tailnet, lan24(), l).tier, Dialog);
    assert_eq!(sw_confirm(Sw::Nat(true), Via::Local, lan24(), l).tier, Inline);
    let c = sw_confirm(Sw::Nat(false), Via::Local, lan24(), l);
    assert_eq!(c.tier, Dialog);
    assert!(
        c.what.contains("断网")
            && c.downtime.is_some()
            && c.recovery.unwrap().contains("192.168.0.1")
    );
    assert_eq!(sw_confirm(Sw::Level("low"), Via::Local, lan24(), l).tier, Inline);
    assert_eq!(sw_confirm(Sw::Upnp(true), Via::Tailnet, None, l).tier, Dialog);
    for on in [true, false] {
        let c = dmz_confirm(
            &Dmz {
                on,
                ip: "192.168.0.50".into(),
            },
            lan24(),
            l,
        );
        assert_eq!(c.tier, Dialog);
        assert!(c.downtime.unwrap().contains("断网"));
    }
    let r = Rule {
        id: "1".into(),
        name: "NAS".into(),
        ..Rule::default()
    };
    assert_eq!(pf_confirm(&r, Via::Local, None, l).tier, Inline);
    assert_eq!(del_confirm(&r, Via::Tailnet, None, l).tier, Dialog);
    // the English texts have no Chinese
    let en = sw_confirm(Sw::Nat(false), Via::Tailnet, lan24(), Lang::En);
    let all = format!(
        "{}{}{}{}{}",
        en.title,
        en.what,
        en.action,
        en.downtime.unwrap(),
        en.recovery.unwrap()
    );
    assert!(
        !all.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
        "{all}"
    );
}

#[test]
fn checks_of_the_forms() {
    let h = hub_with(router_data());
    let l = Lang::Zh;
    let f = |name: &str, wan: &str, ip: &str, port: &str| PfIn {
        name: name.into(),
        protocol: "UDP".into(),
        wan: wan.into(),
        ip: ip.into(),
        port: port.into(),
        enabled: true,
    };
    let r = h
        .read(|st| pf_check(&f(" Game ", "3074", "192.168.0.30", "3074"), st, l))
        .unwrap();
    assert_eq!(
        (r.name.as_str(), r.protocol.as_str(), r.lan_ip.as_str()),
        ("Game", "UDP", "192.168.0.30")
    );
    for (bad, why) in [
        (f("", "1", "192.168.0.30", "1"), "都要填"),
        (f("x", "0", "192.168.0.30", "1"), "端口"),
        (f("x", "1", "10.0.0.3", "1"), "LAN IP"),
        (f("x", "1", "192.168.0.1", "1"), "U60 自己"),
    ] {
        let o = h.read(|st| pf_check(&bad, st, l)).unwrap_err();
        assert!(o.text(l).contains(why), "{}", o.text(l));
    }
    // an unknown protocol falls back to TCP
    let mut odd = f("x", "1", "192.168.0.30", "1");
    odd.protocol = "SCTP".into();
    assert_eq!(h.read(|st| pf_check(&odd, st, l)).unwrap().protocol, "TCP");
    // DMZ: on needs a LAN host; off sends what it has
    assert!(
        h.read(|st| dmz_check(true, "192.168.0.300", st, l))
            .is_err()
    );
    assert_eq!(
        h.read(|st| dmz_check(true, "192.168.0.50", st, l))
            .unwrap()
            .ip,
        "192.168.0.50"
    );
    assert!(h.read(|st| dmz_check(false, "", st, l)).is_ok());
    // no reading: not sent
    let mut d = router_data();
    d["port_forward"] = Value::Null;
    let h2 = hub_with(d);
    assert!(matches!(
        h2.read(|st| pf_check(&f("x", "1", "192.168.0.30", "1"), st, l)),
        Err(write::Outcome::NotYet(_))
    ));
}

#[test]
fn readbacks() {
    let h = hub_with(router_data());
    let rb = |w: write::Write| h.read(|st| (w.readback.unwrap())(st));
    assert!(rb(sw_write(Sw::Fw(true), None, Lang::Zh)));
    assert!(!rb(sw_write(Sw::Fw(false), None, Lang::Zh)));
    assert!(rb(sw_write(Sw::Level("medium"), None, Lang::Zh)));
    assert!(!rb(dmz_write(
        &Dmz {
            on: true,
            ip: "192.168.0.50".into()
        },
        Lang::Zh
    )));
    assert!(rb(dmz_write(
        &Dmz {
            on: false,
            ip: "192.168.0.50".into()
        },
        Lang::Zh
    )));
    let nas = h.read(|st| find(st, "1", Lang::Zh)).unwrap();
    assert!(!rb(del_write(&nas, Lang::Zh)), "still listed");
    assert!(
        rb(pf_write(&nas, 0, Lang::Zh)) && !rb(pf_write(&nas, 1, Lang::Zh)),
        "an identical rule has to show up as a new row"
    );
    assert!(
        h.read(|st| find(st, "", Lang::Zh)).is_err(),
        "a rule without an id is never deleted"
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
async fn a_switch_reads_back_from_the_router_block() {
    let (url, seen) = control().await;
    let h = hub_with(router_data());
    let h2 = h.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut d = router_data();
        d["firewall"]["nat"] = json!(false);
        h2.apply(&Frame { event: "block".into(), data: json!({"epoch":"e","name":"router","revision":2,"observed_at":2,"stale":false,"data":d}).to_string() });
    });
    let o = write::execute(
        &reqwest::Client::new(),
        &url,
        &h,
        &sw_write(Sw::Nat(false), None, Lang::Zh),
        Lang::Zh,
    )
    .await;
    assert_eq!(o, write::Outcome::Applied("已关掉NAT".into()));
    let b = seen.lock().unwrap().clone();
    assert_eq!(b.len(), 1);
    assert_eq!(b[0]["params"]["args"], json!({"nat_switch": "0"}));
}

#[test]
fn forms_render_open_and_closed() {
    let h = hub_with(router_data());
    let closed = h.read(|st| form("pf", false, st, Lang::Zh));
    assert!(
        closed.contains("id=\"fw-pfbtn\"")
            && closed.contains("aria-expanded=\"false\"")
            && closed.contains("添加规则")
    );
    assert!(closed.contains("<div id=\"fw-pfform\"></div>") && !closed.contains("<!--leaf-->"));
    let open = h.read(|st| form("pf", true, st, Lang::Zh));
    assert!(
        open.contains("aria-expanded=\"true\"")
            && open.contains("data-bind=\"_pi\"")
            && open.contains("placeholder=\"192.168.0.x\"")
    );
    assert!(open.contains("filterSignals") && open.contains("192.168.0.0/24"));
    let dmz = h.read(|st| form("dmz", true, st, Lang::En));
    assert!(
        dmz.contains("data-bind=\"_dip\"")
            && dmz.contains("DMZ Host IP")
            && dmz.contains("aria-checked=\"false\"")
    );
}
