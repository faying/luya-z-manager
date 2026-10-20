use super::*;
use crate::device_profile::collect::Collector;
use crate::device_profile::context::Loss;

fn nlmsg(kind: u16, body: &[u8]) -> Vec<u8> {
    let len = 16 + body.len();
    let mut v = Vec::new();
    v.extend_from_slice(&(len as u32).to_ne_bytes());
    v.extend_from_slice(&kind.to_ne_bytes());
    v.extend_from_slice(&[0; 10]);
    v.extend_from_slice(body);
    while v.len() % 4 != 0 {
        v.push(0);
    }
    v
}

fn rta(kind: u16, data: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&((4 + data.len()) as u16).to_ne_bytes());
    v.extend_from_slice(&kind.to_ne_bytes());
    v.extend_from_slice(data);
    while v.len() % 4 != 0 {
        v.push(0);
    }
    v
}

const UP: u32 = 0x1 | 0x10000; // IFF_UP | IFF_LOWER_UP

fn link(kind: u16, index: u32, name: &str) -> Vec<u8> {
    link_flags(kind, index, name, UP)
}

fn link_flags(kind: u16, index: u32, name: &str, flags: u32) -> Vec<u8> {
    let mut body = vec![0u8; 16];
    body[4..8].copy_from_slice(&index.to_ne_bytes());
    body[8..12].copy_from_slice(&flags.to_ne_bytes());
    body.extend(rta(IFLA_IFNAME, format!("{name}\0").as_bytes()));
    nlmsg(kind, &body)
}

fn addr(kind: u16, index: u32) -> Vec<u8> {
    let mut body = vec![0u8; 8];
    body[4..8].copy_from_slice(&index.to_ne_bytes());
    nlmsg(kind, &body)
}

fn route(kind: u16, oif: u32) -> Vec<u8> {
    let mut body = vec![0u8; 12];
    body.extend(rta(RTA_OIF, &oif.to_ne_bytes()));
    nlmsg(kind, &body)
}

fn steps(t: &mut Tracker, bytes: &[u8]) -> Vec<Step> {
    parse(bytes).unwrap().iter().map(|m| t.on(m)).collect()
}

/// Dump as measured on the device: br-lan 5, rmnet_data0 17, tailscale0 30.
fn live() -> Tracker {
    let mut t = Tracker::default();
    let mut dump = link(RTM_NEWLINK, 5, "br-lan");
    dump.extend(link(RTM_NEWLINK, 17, "rmnet_data0"));
    dump.extend(link(RTM_NEWLINK, 30, "tailscale0"));
    dump.extend(nlmsg(NLMSG_DONE, &[0; 4]));
    assert_eq!(
        steps(&mut t, &dump),
        [Step::Nothing, Step::Nothing, Step::Nothing, Step::Live]
    );
    t
}

#[test]
fn parse_reads_link_names_addr_index_and_route_oif() {
    let mut b = link(RTM_NEWLINK, 17, "rmnet_data0");
    b.extend(addr(RTM_DELADDR, 17));
    b.extend(route(RTM_NEWROUTE, 17));
    assert_eq!(
        parse(&b).unwrap(),
        [
            Msg::Link {
                index: 17,
                name: Some("rmnet_data0".into()),
                flags: UP,
                gone: false
            },
            Msg::Addr { index: 17 },
            Msg::Route { oif: Some(17) },
        ]
    );
}

#[test]
fn malformed_or_truncated_input_is_an_error() {
    let b = addr(RTM_NEWADDR, 17);
    assert!(parse(&b[..10]).is_err());
    let mut short = b.clone();
    short[0..4].copy_from_slice(&64u32.to_ne_bytes());
    assert!(parse(&short).is_err());
    let mut bad_attr = link(RTM_NEWLINK, 1, "x");
    bad_attr[32..34].copy_from_slice(&200u16.to_ne_bytes());
    assert!(parse(&bad_attr).is_err());
}

#[test]
fn same_ip_redial_bumps_on_both_delete_and_add() {
    let mut t = live();
    let mut redial = addr(RTM_DELADDR, 17);
    redial.extend(route(RTM_DELROUTE, 17));
    redial.extend(addr(RTM_NEWADDR, 17));
    assert_eq!(steps(&mut t, &redial), [Step::Bump, Step::Bump, Step::Bump]);
}

#[test]
fn an_unchanged_rmnet_link_repeat_does_not_bump_but_a_flag_change_does() {
    // measured 10-20: ~5 identical RTM_NEWLINK a second for rmnet_data0 while idle
    let mut t = live();
    let mut b = link(RTM_NEWLINK, 17, "rmnet_data0");
    b.extend(link(RTM_NEWLINK, 17, "rmnet_data0"));
    b.extend(link_flags(RTM_NEWLINK, 17, "rmnet_data0", 0x1)); // carrier lost
    b.extend(link_flags(RTM_NEWLINK, 17, "rmnet_data0", 0x1));
    b.extend(link(RTM_NEWLINK, 17, "rmnet_data0")); // back
    b.extend(addr(RTM_NEWADDR, 17)); // an address event always bumps
    assert_eq!(
        steps(&mut t, &b),
        [
            Step::Nothing,
            Step::Nothing,
            Step::Bump,
            Step::Nothing,
            Step::Bump,
            Step::Bump
        ]
    );
    // deleted and re-created with the same flags: a new link, bumps
    let mut b = link(RTM_DELLINK, 17, "rmnet_data0");
    b.extend(link(RTM_NEWLINK, 17, "rmnet_data0"));
    assert_eq!(steps(&mut t, &b), [Step::Bump, Step::Bump]);
}

#[test]
fn lan_wifi_and_tailscale_changes_do_not_bump() {
    let mut t = live();
    let mut b = addr(RTM_NEWADDR, 5);
    b.extend(route(RTM_NEWROUTE, 30));
    b.extend(link(RTM_NEWLINK, 40, "wlan0"));
    b.extend(addr(RTM_DELADDR, 40));
    assert_eq!(steps(&mut t, &b), [Step::Nothing; 4]);
}

#[test]
fn unknown_indexes_and_new_rmnet_links_count_as_ours() {
    let mut t = live();
    let mut b = addr(RTM_NEWADDR, 99);
    b.extend(link(RTM_NEWLINK, 18, "rmnet_data1"));
    b.extend(addr(RTM_NEWADDR, 18));
    b.extend(link(RTM_DELLINK, 18, "rmnet_data1"));
    assert_eq!(steps(&mut t, &b), [Step::Bump; 4]);
}

#[test]
fn events_before_the_dump_completes_mint_nothing() {
    let mut t = Tracker::default();
    let mut b = link(RTM_NEWLINK, 17, "rmnet_data0");
    b.extend(addr(RTM_DELADDR, 17));
    b.extend(nlmsg(NLMSG_DONE, &[0; 4]));
    assert_eq!(
        steps(&mut t, &b),
        [Step::Nothing, Step::Nothing, Step::Live]
    );
}

#[test]
fn overrun_and_error_replies_lose_the_stream() {
    let mut t = live();
    assert_eq!(steps(&mut t, &nlmsg(NLMSG_OVERRUN, &[])), [Step::Lost]);
    let err = (-libc::ENOBUFS).to_ne_bytes();
    assert_eq!(steps(&mut live(), &nlmsg(NLMSG_ERROR, &err)), [Step::Lost]);
    // An ACK (code 0) is not a loss.
    assert_eq!(
        steps(&mut live(), &nlmsg(NLMSG_ERROR, &[0; 4])),
        [Step::Nothing]
    );
}

fn epoch(c: &Collector) -> Option<String> {
    c.token().context.connection_epoch
}

#[test]
fn collector_epoch_follows_the_producer() {
    let c = Collector::new();
    c.connection_event();
    assert_eq!(epoch(&c), None, "no epoch before a subscription is live");
    c.connection_live(true);
    let a = epoch(&c).expect("live mints");
    c.connection_event();
    let b = epoch(&c).expect("event mints");
    assert_ne!(a, b, "a repeat event (same-IP redial) is a new epoch");
    let g = c.token().generation;
    c.connection_live(false);
    assert_eq!(epoch(&c), None);
    assert!(c.token().generation > g);
    c.connection_live(true);
    let d = epoch(&c).unwrap();
    assert!(
        d != a && d != b,
        "resubscribing never restores an old epoch"
    );
}

#[test]
fn outside_revocations_leave_a_new_epoch_only_while_live() {
    let c = Collector::new();
    c.connection_live(true);
    let a = epoch(&c).unwrap();
    c.invalidate_context(Loss::Ubus);
    let b = epoch(&c).expect("stream still complete");
    assert_ne!(a, b);
    c.invalidate_context(Loss::Usb);
    assert_eq!(epoch(&c), Some(b), "USB loss is not a connection change");
    c.connection_live(false);
    c.invalidate_context(Loss::Ubus);
    assert_eq!(epoch(&c), None);
}

fn sim(c: &Collector) -> Option<String> {
    c.token().context.sim_epoch
}

#[test]
fn sim_epoch_needs_a_confirmed_subscription_and_changes_on_every_card_event() {
    let c = Collector::new();
    c.sim_event();
    assert_eq!(sim(&c), None, "no epoch before the probe came back");
    c.sim_listening();
    let a = sim(&c).expect("probe confirms");
    c.sim_event();
    let b = sim(&c).expect("card event mints");
    assert_ne!(a, b);
    c.invalidate_context(Loss::Usb);
    assert_eq!(sim(&c), Some(b), "USB is not a SIM change");
    c.invalidate_context(Loss::Ubus);
    assert_eq!(sim(&c), None, "a listener gap may hide a SIM event");
    c.sim_event();
    assert_eq!(sim(&c), None, "stays revoked until the next probe");
    c.sim_listening();
    assert!(sim(&c).is_some());
}

fn radio(c: &Collector) -> Option<String> {
    c.token().context.radio_epoch
}

#[test]
fn radio_epoch_needs_both_subscriptions_and_follows_every_related_event() {
    let c = Collector::new();
    c.connection_live(true);
    assert_eq!(radio(&c), None, "ubus subscription not confirmed yet");
    c.sim_listening();
    let mut seen = vec![radio(&c).expect("both live")];
    let next = |seen: &mut Vec<String>, c: &Collector, what: &str| {
        let r = radio(c).expect(what);
        assert!(!seen.contains(&r), "{what}: never an old value");
        seen.push(r);
    };
    c.connection_event();
    next(&mut seen, &c, "connection event");
    c.sim_event();
    next(&mut seen, &c, "card event");
    c.radio_event();
    next(&mut seen, &c, "power-state event");
    c.invalidate_context(Loss::Usb);
    assert_eq!(radio(&c), seen.last().cloned(), "USB is not a radio change");
    c.invalidate_context(Loss::Ubus);
    assert_eq!(radio(&c), None, "ubus gap revokes");
    c.sim_listening();
    next(&mut seen, &c, "probe re-arms");
    c.connection_live(false);
    assert_eq!(radio(&c), None, "netlink loss revokes");
    c.radio_event();
    assert_eq!(radio(&c), None);
}

#[test]
fn a_mode_read_and_a_reprobe_both_restore_the_radio_epoch() {
    let c = Collector::new();
    c.connection_live(true);
    c.sim_listening();
    // The `zte_nwinfo` UCI collection path (`uci`), not `radio_read`: 1.6.1 missed it.
    let values = [("zte_nwinfo.sys_info.operate_mode".to_string(), "ONLINE".to_string())]
        .into_iter()
        .collect();
    c.uci(c.token(), "zte_nwinfo", &values, None);
    assert_eq!(c.token().context.radio_mode.as_deref(), Some("ONLINE"));
    assert!(radio(&c).is_some(), "a mode read must not leave the radio epoch behind");
    c.radio_read(c.token(), Some("LOW_POWER"), None);
    assert!(radio(&c).is_some(), "a mode change gives a new value while both are live");
    // An executor ubus error revokes; the listener answering the re-sent probe re-arms.
    c.invalidate_context(Loss::Ubus);
    assert_eq!(radio(&c), None);
    c.sim_listening();
    assert!(radio(&c).is_some() && sim(&c).is_some());
}
