//! Fixture minutes for the state machine (tailnet-poller.md §6), the flow pick, the allow list.

use super::*;
use std::net::IpAddr;

const GW: &str = "10.5.8.88";
const T0: i64 = 1_790_000_000;

/// Feeds minute `i` (60 s apart) with `pkts` from `peer` to :80; the opened events.
fn step(m: &mut Machine, i: i64, counted: bool, peer: &str, pkts: u64) -> Vec<Event> {
    let s = [Sample {
        peer: peer.into(),
        port: 80,
        pkts,
    }];
    let samples: &[Sample] = if pkts == 0 { &[] } else { &s };
    m.feed(
        &Minute {
            wall: T0 + 60 * i,
            mono: 1000 + 60 * i,
            counted,
            samples,
        },
        |_, _| false,
    )
}

/// A run of minutes; how many events opened in all.
fn run(m: &mut Machine, from: i64, counts: &[(bool, u64)]) -> usize {
    counts
        .iter()
        .enumerate()
        .map(|(n, (c, p))| step(m, from + n as i64, *c, GW, *p).len())
        .sum()
}

fn on(n: usize, pkts: u64) -> Vec<(bool, u64)> {
    vec![(true, pkts); n]
}

#[test]
fn stock_web_tab_opens_one_event() {
    // §18: three connections, about 550 packets a minute, 12 minutes
    let mut m = Machine::new("0123abcd");
    let mut opened = Vec::new();
    for i in 0..12 {
        opened.extend(step(&mut m, i, true, GW, 550));
        if i < 9 {
            assert!(opened.is_empty(), "nothing before the 10th minute");
        }
    }
    assert_eq!(opened.len(), 1);
    let e = &opened[0];
    assert_eq!((e.peer.as_str(), e.port, e.minutes), (GW, 80, 10));
    assert_eq!(e.pkts_per_min, 550);
    assert!(e.via_gateway);
    assert!(e.id.starts_with("0123abcd-") && luyaz_api::alerts::valid_event_id(&e.id));
    assert_eq!(e.since, T0);
    let ev = &m.events()[0];
    assert_eq!((ev.state, ev.minutes), ("active", 12));
}

#[test]
fn a_60_s_probe_opens_one() {
    // §15's doctor probe: about 21 packets a minute
    let mut m = Machine::new("0123abcd");
    assert_eq!(run(&mut m, 0, &on(10, 21)), 1);
}

#[test]
fn below_threshold_never_opens() {
    let mut m = Machine::new("0123abcd");
    assert_eq!(run(&mut m, 0, &on(30, 19)), 0);
}

#[test]
fn half_hourly_hermes_never_opens() {
    // one minute with packets, 29 without, for 3 hours
    let mut m = Machine::new("0123abcd");
    let mut c = Vec::new();
    for _ in 0..6 {
        c.push((true, 40));
        c.extend(on(29, 0));
    }
    assert_eq!(run(&mut m, 0, &c), 0);
    assert!(m.events().is_empty());
}

#[test]
fn screen_on_or_charging_minutes_neither_count_nor_reset() {
    let mut m = Machine::new("0123abcd");
    let mut c = on(5, 550);
    c.extend(vec![(false, 550); 3]);
    c.extend(on(4, 550));
    assert_eq!(run(&mut m, 0, &c), 0, "9 counted minutes");
    assert_eq!(
        step(&mut m, 12, true, GW, 550).len(),
        1,
        "the 10th counted one"
    );
    // polling while the screen is on all along: nothing
    let mut m = Machine::new("0123abcd");
    assert_eq!(run(&mut m, 0, &vec![(false, 550); 20]), 0);
}

#[test]
fn one_short_minute_is_let_through_two_reset() {
    let mut m = Machine::new("0123abcd");
    let mut c = on(5, 100);
    c.push((true, 3));
    c.extend(on(5, 100));
    assert_eq!(run(&mut m, 0, &c), 1);

    let mut m = Machine::new("0123abcd");
    let mut c = on(5, 100);
    c.extend(on(2, 0));
    c.extend(on(9, 100));
    assert_eq!(
        run(&mut m, 0, &c),
        0,
        "two misses: the run starts again at 9"
    );
}

#[test]
fn three_empty_minutes_end_it() {
    let mut m = Machine::new("0123abcd");
    run(&mut m, 0, &on(10, 100));
    run(&mut m, 10, &on(2, 0));
    assert_eq!(m.events()[0].state, "active", "two empty minutes");
    run(&mut m, 12, &[(true, 5)]);
    run(&mut m, 13, &on(2, 0));
    assert_eq!(
        m.events()[0].state,
        "active",
        "a packet in between restarts the count"
    );
    run(&mut m, 15, &on(1, 0));
    let e = &m.events()[0];
    assert_eq!(e.state, "ended");
    assert_eq!(e.ended_at, Some(T0 + 60 * 12), "where it was last seen");
}

#[test]
fn back_within_30_minutes_is_the_same_event() {
    let mut m = Machine::new("0123abcd");
    run(&mut m, 0, &on(10, 100));
    run(&mut m, 10, &on(3, 0));
    assert_eq!(m.events()[0].state, "ended");
    // 20 minutes later it comes back: no new alert, the same event goes on
    let id = m.events()[0].id.clone();
    run(&mut m, 13, &on(17, 0));
    assert_eq!(run(&mut m, 30, &on(12, 100)), 0);
    assert_eq!(m.events().len(), 1);
    let e = &m.events()[0];
    assert_eq!(
        (e.id.as_str(), e.state, e.minutes),
        (id.as_str(), "active", 22)
    );
}

#[test]
fn back_after_30_minutes_is_a_new_event() {
    let mut m = Machine::new("0123abcd");
    run(&mut m, 0, &on(10, 100));
    run(&mut m, 10, &on(3, 0));
    run(&mut m, 13, &on(40, 0));
    assert_eq!(run(&mut m, 53, &on(10, 100)), 1);
    assert_eq!(m.events().len(), 2);
    assert_ne!(m.events()[0].id, m.events()[1].id);
}

#[test]
fn allowed_peer_opens_nothing() {
    let mut m = Machine::new("0123abcd");
    let mut asked = 0;
    for i in 0..15 {
        let s = [Sample {
            peer: GW.into(),
            port: 80,
            pkts: 550,
        }];
        let o = m.feed(
            &Minute {
                wall: T0 + 60 * i,
                mono: 1000 + 60 * i,
                counted: true,
                samples: &s,
            },
            |p, port| {
                asked += 1;
                p == GW && port == 80
            },
        );
        assert!(o.is_empty());
    }
    assert!(m.events().is_empty());
    assert_eq!(asked, 1, "the list is read only when a run reaches 10");
}

#[test]
fn a_gap_in_the_checks_breaks_the_run() {
    let mut m = Machine::new("0123abcd");
    run(&mut m, 0, &on(6, 100));
    // the next check 5 minutes later (the core was away)
    assert_eq!(run(&mut m, 11, &on(9, 100)), 0);
    assert_eq!(run(&mut m, 20, &on(1, 100)), 1);
}

#[test]
fn peers_and_ports_count_apart_and_unwatched_ports_not_at_all() {
    let mut m = Machine::new("0123abcd");
    let mut opened = 0;
    for i in 0..10 {
        let s = [
            Sample {
                peer: GW.into(),
                port: 80,
                pkts: 12,
            },
            Sample {
                peer: GW.into(),
                port: 80,
                pkts: 12,
            },
            Sample {
                peer: "100.101.1.2".into(),
                port: 9090,
                pkts: 30,
            },
            Sample {
                peer: "100.101.1.3".into(),
                port: 2222,
                pkts: 900,
            },
        ];
        opened += m
            .feed(
                &Minute {
                    wall: T0 + 60 * i,
                    mono: 60 * i,
                    counted: true,
                    samples: &s,
                },
                |_, _| false,
            )
            .len();
    }
    // two connections of 12 add up to 24 a minute; :2222 is never watched
    assert_eq!(opened, 2);
    let mut got: Vec<_> = m
        .events()
        .iter()
        .map(|e| (e.peer.as_str(), e.port, e.via_gateway))
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![("10.5.8.88", 80, true), ("100.101.1.2", 9090, false)]
    );
}

#[test]
fn ack_and_allow_mark_the_event() {
    let mut m = Machine::new("0123abcd");
    run(&mut m, 0, &on(10, 100));
    let id = m.events()[0].id.clone();
    assert!(m.ack(&id) && m.events()[0].acked);
    assert!(!m.ack("0123abcd-1"));
    m.mark_allowed(GW, 80, true);
    assert!(m.events()[0].allowed);
}

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

#[test]
fn pick_takes_tailnet_and_routed_peers_only() {
    let nets = Nets {
        addrs: vec![
            ("br-lan".into(), ip("10.5.6.1"), 24),
            ("tailscale0".into(), ip("100.64.0.5"), 32),
            ("rmnet_data0".into(), ip("10.170.3.4"), 30),
            ("lo".into(), ip("127.0.0.1"), 8),
        ],
    };
    let d = [
        // §18: the home Surge gateway, through the subnet router
        (ip("10.5.8.88"), ip("10.5.6.1"), 80, 300),
        (ip("10.5.8.88"), ip("10.5.6.1"), 80, 250),
        // a Wi-Fi client of the device's own LAN
        (ip("10.5.6.23"), ip("10.5.6.1"), 80, 900),
        // a tailnet node straight to the tailnet address
        (ip("100.101.1.2"), ip("100.64.0.5"), 9090, 40),
        // ssh, never watched
        (ip("100.101.1.2"), ip("100.64.0.5"), 2222, 999),
        // something on the cellular side
        (ip("10.9.9.9"), ip("10.170.3.4"), 80, 50),
        // the device itself, and a public address
        (ip("127.0.0.1"), ip("127.0.0.1"), 80, 50),
        (ip("8.8.8.8"), ip("10.5.6.1"), 80, 50),
        // the device as the client (dst not local)
        (ip("10.5.6.1"), ip("10.5.8.88"), 80, 50),
    ];
    let mut got = pick(&d, &nets);
    got.sort_by(|a, b| a.peer.cmp(&b.peer));
    assert_eq!(
        got,
        vec![
            Sample {
                peer: "10.5.8.88".into(),
                port: 80,
                pkts: 550
            },
            Sample {
                peer: "100.101.1.2".into(),
                port: 9090,
                pkts: 40
            },
        ]
    );
}

#[test]
fn allow_list_round_trip_expiry_and_removal() {
    let now = T0;
    let mut l = parse_allow("not json");
    assert!(l.is_empty());
    upsert(
        &mut l,
        Allow {
            peer: GW.into(),
            port: 80,
            label: "家里".into(),
            until: Some(now + ALLOW_DEFAULT_S),
        },
        now,
    )
    .unwrap();
    upsert(
        &mut l,
        Allow {
            peer: "100.101.1.2".into(),
            port: 9090,
            label: String::new(),
            until: None,
        },
        now,
    )
    .unwrap();
    assert!(allowed(&l, GW, 80, now + 3600));
    assert!(!allowed(&l, GW, 9090, now), "peer and port both");
    assert!(
        !allowed(&l, GW, 80, now + ALLOW_DEFAULT_S),
        "24 h later it is gone"
    );
    assert!(
        allowed(&l, "100.101.1.2", 9090, now + 10 * ALLOW_DEFAULT_S),
        "for good"
    );
    // the same pair again replaces it (made permanent)
    upsert(
        &mut l,
        Allow {
            peer: GW.into(),
            port: 80,
            label: "家里".into(),
            until: None,
        },
        now,
    )
    .unwrap();
    assert_eq!(l.len(), 2);
    let text = serde_json::to_string(&l).unwrap();
    assert_eq!(parse_allow(&text), l);
    // expired entries go at the next write
    let mut l2 = vec![Allow {
        peer: "10.5.8.1".into(),
        port: 80,
        label: String::new(),
        until: Some(now - 1),
    }];
    upsert(
        &mut l2,
        Allow {
            peer: GW.into(),
            port: 80,
            label: String::new(),
            until: None,
        },
        now,
    )
    .unwrap();
    assert_eq!(l2.len(), 1);
    assert!(remove(&mut l, GW, 80));
    assert!(!remove(&mut l, GW, 80));
    assert_eq!(l.len(), 1);
}

#[test]
fn node_name_from_tailscale_status() {
    let s = serde_json::json!({"Peer": {
        "k1": {"HostName": "mbp", "DNSName": "mbp.tail.ts.net.", "TailscaleIPs": ["100.101.1.2", "fd7a:115c:a1e0::1"]},
        "k2": {"HostName": "", "DNSName": "nas.tail.ts.net.", "TailscaleIPs": ["100.101.1.9"]},
    }});
    assert_eq!(name_of(&s, "100.101.1.2").as_deref(), Some("mbp"));
    assert_eq!(
        name_of(&s, "100.101.1.9").as_deref(),
        Some("nas.tail.ts.net")
    );
    assert_eq!(name_of(&s, "100.101.1.7"), None);
}

#[test]
fn control_rejects_bad_parameters() {
    let bad = |a: &str, p: serde_json::Value| control(a, &p).unwrap().unwrap_err();
    assert!(bad("poller.allow", serde_json::json!({"peer": "x", "port": 80})).contains("peer"));
    assert!(
        bad(
            "poller.allow",
            serde_json::json!({"peer": GW, "port": 2222})
        )
        .contains("port")
    );
    assert!(
        bad(
            "poller.allow",
            serde_json::json!({"peer": GW, "port": 80, "hours": 0})
        )
        .contains("hours")
    );
    assert!(bad("poller.ack", serde_json::json!({})).contains("id"));
    assert!(control("wifi.apply", &serde_json::json!({})).is_none());
}

#[test]
fn state_survives_a_restart_and_an_acked_event_is_not_told_again() {
    let mut m = Machine::new("0123abcd");
    run(&mut m, 0, &on(10, 550));
    let id = m.events()[0].id.clone();
    let before = m.mark();
    run(&mut m, 10, &on(1, 550));
    assert_eq!(
        m.mark(),
        before,
        "a counted minute alone does not rewrite the file"
    );
    assert!(m.ack(&id));
    assert_ne!(m.mark(), before, "an ack does");
    let text = m.save();
    // the core restarts: same events, the polling goes on, nothing new opens
    let mut r = Machine::restore("89abcdef", &text);
    assert_eq!(r.events(), m.events());
    assert_eq!(run(&mut r, 11, &on(15, 550)), 0);
    assert_eq!(r.events().len(), 1);
    assert!(r.events()[0].acked && r.events()[0].state == "active");
    // and the id counter goes on
    run(&mut r, 26, &on(3, 0));
    run(&mut r, 29, &on(40, 0));
    run(&mut r, 69, &on(10, 550));
    assert_ne!(r.events()[1].id, id);
}

#[test]
fn a_missing_or_broken_state_file_is_a_fresh_start() {
    for t in [
        "",
        "not json",
        "[]",
        r#"{"events": [{"id": 1}, {"state": "odd", "id": "a", "peer": "b", "port": 80}]}"#,
    ] {
        let m = Machine::restore("0123abcd", t);
        assert!(m.events().is_empty(), "{t}");
    }
}
