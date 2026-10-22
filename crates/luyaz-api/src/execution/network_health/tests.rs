use super::*;

fn finding(layer: &str) -> Option<Finding> {
    Some(Finding {
        kind: format!("no_{layer}"),
        layer: layer.into(),
        symptom: "已连上，但外网不通".into(),
        symptom_en: "Connected, but no internet".into(),
    })
}

fn held(released: bool) -> Vec<Paused> {
    vec![Paused { who: "home-exit".into(), reason: "lower", layer: Some("internet".into()), since: 1, released }]
}

/// Both devices' `/proc` on 10-22 (u60 with CHILL running, u60g without), addresses replaced:
/// the IPv4 default route and the IPv6 one both leave by rmnet_data0.
#[test]
fn real_b31_proc_reads_as_up() {
    use crate::netinfo::bearer_from_proc;
    let route = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
rmnet_data0\t00000000\t0100000A\t0003\t0\t0\t256\t00000000\t0\t0\t0\n\
rmnet_data0\t0000000A\t00000000\t0001\t0\t0\t0\tF8FFFFFF\t0\t0\t0\n";
    let inet6 = "fe800000000000000000000000000001 11 40 20 80 rmnet_data0\n\
20010db8000000000000000000000001 11 40 00 80 rmnet_data0\n";
    let v6 = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 00000000000000000000000000000000 ffffffff 00000001 00000000 00200200 lo\n\
00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe800000000000000000000000000001 00000400 00000005 00000000 00000003 rmnet_data0\n";
    assert_eq!(bearer_from_proc(Some(route), inet6, v6, "rmnet_data0"), Some(true));
    // IPv4 gone, IPv6 still there
    let head = route.lines().next().unwrap();
    assert_eq!(bearer_from_proc(Some(head), inet6, v6, "rmnet_data0"), Some(true));
    // the unreachable `lo` defaults alone are not a way out
    assert_eq!(bearer_from_proc(Some(head), inet6, v6.lines().next().unwrap(), "rmnet_data0"), Some(false));
}

#[test]
fn healthy_says_nothing() {
    let v = build(&Input { bearer: Some(true), ..Input::default() });
    assert_eq!(v["state"], "ok");
    assert!(v["say"].is_null());
    assert!(v["next"].is_null());
    assert_eq!(v["paused"], json!([]));
}

#[test]
fn unread_bearer_is_unknown_and_quiet() {
    let v = build(&Input::default());
    assert_eq!(v["state"], "unknown");
    assert!(v["say"].is_null());
}

#[test]
fn no_bearer_says_the_proxy_is_paused() {
    let p = vec![Paused { who: "home-exit".into(), reason: "no_bearer", layer: None, since: 1, released: false }];
    let v = build(&Input { bearer: Some(false), paused: p, ..Input::default() });
    assert_eq!(v["state"], "down");
    assert_eq!(v["next"], "wait");
    assert_eq!(v["layers"]["bearer"], "down");
    assert_eq!(v["say"]["id"], "network.no_bearer");
    assert_eq!(v["say"]["detail"], "移动网络暂时不可用，已暂停代理重连。恢复后会自动检查。");
    // nothing paused (airplane, data off, roaming off, a switch): data, but no notice, no step
    let v = build(&Input { bearer: Some(false), ..Input::default() });
    assert_eq!((v["state"].as_str(), v["layers"]["bearer"].as_str()), (Some("down"), Some("down")));
    assert!(v["say"].is_null() && v["next"].is_null());
}

#[test]
fn a_finding_carries_its_symptom_and_the_pause() {
    let v = build(&Input { bearer: Some(true), finding: finding("internet"), paused: held(false), ..Input::default() });
    assert_eq!(v["state"], "down");
    assert_eq!(v["finding"], "no_internet");
    assert_eq!(v["say"]["id"], "network.internet");
    assert_eq!(v["say"]["label"], "外网不通");
    assert_eq!(v["say"]["detail"], "已连上，但外网不通，已暂停代理重连。");
    assert_eq!(v["next"], "wait");
}

#[test]
fn dns_says_mobile_data_is_fine() {
    let v = build(&Input { bearer: Some(true), finding: finding("dns"), ..Input::default() });
    assert_eq!(v["say"]["detail"], "已连上，但外网不通；移动数据正常。");
    assert_eq!(v["say"]["detail_en"], "Connected, but no internet; mobile data is fine.");
}

#[test]
fn a_released_hold_with_the_finding_open_points_at_the_apn() {
    let v = build(&Input { bearer: Some(true), finding: finding("data"), paused: held(true), ..Input::default() });
    assert_eq!(v["next"], "check_apn");
    assert_eq!(v["say"]["detail"], "已连上，但外网不通。自动恢复没有成功，建议检查 APN。");
    // a signal finding: the APN is not it
    let v = build(&Input { bearer: Some(true), finding: finding("signal"), paused: held(true), ..Input::default() });
    assert_eq!(v["next"], "wait");
}

#[test]
fn recovered_shows_for_a_while_then_goes() {
    let v = build(&Input { bearer: Some(true), recovered_secs: Some(30), ..Input::default() });
    assert_eq!(v["state"], "ok");
    assert_eq!(v["say"]["id"], "network.recovered");
    assert_eq!(v["say"]["detail"], "移动网络已恢复，正在检查代理。");
    let v = build(&Input { bearer: Some(true), recovered_secs: Some(RECOVERED_SECS), ..Input::default() });
    assert!(v["say"].is_null());
}

#[test]
fn details_fit_the_touch_screen() {
    for i in [
        Input { bearer: Some(false), paused: held(false), ..Input::default() },
        Input { bearer: Some(true), finding: finding("dns"), paused: held(false), ..Input::default() },
        Input { bearer: Some(true), finding: finding("data"), paused: held(true), ..Input::default() },
    ] {
        let s = super::super::say::health(&i).unwrap();
        assert!(s.detail.len() <= super::super::say::DETAIL_MAX);
    }
}

#[test]
fn proxy_down_speaks_only_when_below_is_fine() {
    let v = build(&Input { bearer: Some(true), proxy: Some(false), ..Input::default() });
    assert_eq!(v["layers"]["proxy"], "down");
    assert_eq!(v["say"]["id"], "network.proxy");
    assert!(v["next"].is_null());
    // a lower finding is the story; the proxy failing under it says nothing new
    let v = build(&Input { bearer: Some(true), proxy: Some(false), finding: finding("internet"), ..Input::default() });
    assert_eq!(v["say"]["id"], "network.internet");
    // data off: nothing to say about the proxy either
    let v = build(&Input { bearer: Some(false), proxy: Some(false), ..Input::default() });
    assert!(v["say"].is_null());
    // a good probe or none: quiet
    for proxy in [Some(true), None] {
        let v = build(&Input { bearer: Some(true), proxy, ..Input::default() });
        assert!(v["say"].is_null());
    }
}
