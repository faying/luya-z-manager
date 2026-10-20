use super::*;
use crate::{ReadError, Watch};
use serde_json::json;

fn bt(seconds: u64) -> BootTime {
    BootTime {
        boot_id: "test-boot".into(),
        seconds,
    }
}

#[test]
fn passive_age_ignores_wall_steps_and_unset_wall() {
    let st = json!({"net":{"wan_status":"ipv4_connected"}});
    for wall in [0, 1_790_000_000, 1_790_028_800, 1_789_971_200] {
        let first = sample(
            &Snapshot::default(),
            1_790_000_000,
            Some(&bt(100)),
            Some(&st),
            Some(10),
        );
        let moved = sample(&first, wall, Some(&bt(105)), Some(&st), Some(20));
        assert_eq!(moved.at, wall, "history keeps raw wall label");
        assert_eq!(
            alive_without_probe(&moved, Some(&bt(135))),
            PassiveLink::Receiving
        );
        assert_eq!(
            alive_without_probe(&moved, Some(&bt(136))),
            PassiveLink::Unknown
        );
        // A current sample with unchanged RX keeps the last actual receive time.
        let still = sample(&moved, wall, Some(&bt(130)), Some(&st), Some(20));
        assert_eq!(
            alive_without_probe(&still, Some(&bt(136))),
            PassiveLink::NeedsProbe
        );
    }
}

#[test]
fn unknown_clock_boot_change_and_sleep_do_not_authorize_probe() {
    let st = json!({"net":{"wan_status":"ipv4_connected"}});
    let first = sample(
        &Snapshot::default(),
        1000,
        Some(&bt(0)),
        Some(&st),
        Some(10),
    );
    let moved = sample(&first, 1005, Some(&bt(5)), Some(&st), Some(20));
    for now in [
        None,
        Some(bt(4)),
        Some(BootTime {
            boot_id: "other".into(),
            seconds: 5,
        }),
        Some(bt(3605)),
        Some(BootTime {
            boot_id: String::new(),
            seconds: 5,
        }),
    ] {
        assert_eq!(
            alive_without_probe(&moved, now.as_ref()),
            PassiveLink::Unknown
        );
    }
    let w = Watch::new();
    assert!(!w.cell_alive(None), "unknown branch returns before DNS");
    assert!(!w.cell_alive(Some(&bt(0))));
    assert_eq!(
        alive_without_probe(&first, Some(&bt(0))),
        PassiveLink::NeedsProbe,
        "boot second zero is valid, not a missing timestamp"
    );
}

#[test]
fn receive_evidence_breaks_on_missing_reset_reconnect_or_gap() {
    let st = json!({"net":{"wan_status":"ipv4_connected"}});
    let first = sample(
        &Snapshot::default(),
        1000,
        Some(&bt(100)),
        Some(&st),
        Some(10),
    );
    let moved = sample(&first, 1005, Some(&bt(105)), Some(&st), Some(20));
    let down = json!({"net":{"wan_status":"disconnected"}});
    let cases = [
        sample(&moved, 1010, Some(&bt(110)), Some(&st), None),
        sample(&moved, 1010, Some(&bt(110)), Some(&st), Some(1)),
        sample(&moved, 1010, Some(&bt(110)), Some(&down), Some(30)),
        sample(&moved, 1010, Some(&bt(110)), None, Some(30)),
        sample(&moved, 1010, None, Some(&st), Some(30)),
        sample(&moved, 1010, Some(&bt(136)), Some(&st), Some(30)),
        sample(&moved, 1010, Some(&bt(105)), Some(&st), Some(30)),
        sample(
            &moved,
            1010,
            Some(&BootTime {
                boot_id: "other".into(),
                seconds: 110,
            }),
            Some(&st),
            Some(30),
        ),
    ];
    for broken in cases {
        assert!(broken.rx_moved_at.is_none(), "{broken:?}");
    }
    let missing = sample(&moved, 1010, Some(&bt(110)), Some(&st), None);
    let again = sample(&missing, 1015, Some(&bt(115)), Some(&st), Some(40));
    assert_eq!(again.rx_moved_at, None, "do not bridge a missing RX read");
    let next = sample(&again, 1020, Some(&bt(120)), Some(&st), Some(50));
    assert_eq!(next.rx_moved_at, Some(bt(120)));
}

#[test]
fn verdict_ttl_uses_boot_time_while_history_uses_wall() {
    let w = Watch::new();
    let st = json!({"net":{"wan_status":"ipv4_connected"},"traffic":{"rx_bytes":10}});
    let screen = json!({"net":{"story":{"state":"ok"}}});
    w.tick(1_790_000_000, Some(&bt(0)), Ok(st.clone()), Some(&screen));
    w.tick(1_789_971_200, Some(&bt(5)), Ok(st.clone()), None);
    assert_eq!(w.verdict(Some(&bt(30))).as_deref(), Some("ok"));
    for now in [
        None,
        Some(bt(31)),
        Some(bt(3600)),
        Some(BootTime {
            boot_id: "other".into(),
            seconds: 0,
        }),
    ] {
        assert_eq!(w.verdict(now.as_ref()), None);
    }
    w.tick(0, None, Ok(st.clone()), Some(&screen));
    assert_eq!(
        w.verdict(Some(&bt(0))),
        None,
        "no boot clock, no fallback to wall"
    );
    w.tick(0, Some(&bt(10)), Ok(st.clone()), Some(&screen));
    assert_eq!(w.verdict(Some(&bt(10))).as_deref(), Some("ok"));
    w.tick(0, None, Ok(st), None);
    assert_eq!(
        w.verdict(Some(&bt(10))),
        None,
        "missing clock revokes cached verdict"
    );
}

#[test]
fn uncertain_passive_data_sends_no_dns_and_cannot_admit_manual_fallback() {
    let w = Watch::new();
    let st = json!({"net":{"wan_status":"ipv4_connected","wan_dns":"192.0.2.53"},
                        "traffic":{"rx_bytes":10}});
    let never_probe = |_: &Snapshot| -> Option<u32> { panic!("unexpected DNS probe") };
    assert!(!w.cell_alive_with(Some(&bt(0)), never_probe));
    w.tick(0, Some(&bt(0)), Ok(st.clone()), None);
    assert!(w.cell_alive_with(Some(&bt(0)), |s| {
        assert_eq!(s.dns, ["192.0.2.53"]);
        Some(5)
    }));
    assert!(!w.cell_alive_with(None, never_probe));
    assert!(!w.cell_alive_with(Some(&bt(31)), never_probe));
    assert!(!w.cell_alive_with(
        Some(&BootTime {
            boot_id: "other".into(),
            seconds: 0
        }),
        never_probe
    ));
    w.tick(
        28_800,
        Some(&bt(5)),
        Ok(json!({"net":{"wan_status":"ipv4_connected"},
                                            "traffic":{"rx_bytes":20}})),
        None,
    );
    assert!(w.cell_alive_with(Some(&bt(5)), never_probe));
    w.tick(0, Some(&bt(10)), Err(ReadError::Request), None);
    assert!(!w.cell_alive_with(Some(&bt(10)), never_probe));
    assert_eq!(w.direct_dns_ms(Some(&bt(10))), None);
    w.tick(
        0,
        Some(&bt(12)),
        Ok(json!({"net":{"wan_status":"ipv4_connected"}})),
        None,
    );
    assert!(!w.cell_alive_with(Some(&bt(12)), never_probe));
    assert_eq!(w.direct_dns_ms(Some(&bt(12))), None);
    assert_eq!(w.direct_dns_ms(None), None);
    w.tick(
        0,
        Some(&bt(15)),
        Ok(json!({"net":{"wan_status":"disconnected"}})),
        None,
    );
    assert!(!w.cell_alive_with(Some(&bt(15)), never_probe));
}

/// R18-P0-02 ①: the snapshot carries the IPv4 cellular path of the same observation; an
/// observation without interface identity has none (unknown), and nothing is carried over.
#[test]
fn snapshot_takes_the_ipv4_cellular_path_or_none() {
    let st = json!({"net":{"wan_status":"ipv4_connected","wan_dns":"192.0.2.53"},
        "interfaces":{"cellular":{"ipv4_dev_name":"rmnet_data0","ipv4_address":"10.1.2.3"},
            "wan4":{"device":"rmnet_data0","up":true}}});
    let s = sample(&Snapshot::default(), 0, Some(&bt(1)), Some(&st), Some(1));
    let p = s.path.clone().expect("path from interfaces");
    assert_eq!(p.interface, "rmnet_data0");
    assert_eq!(p.source.to_string(), "10.1.2.3");
    let bare = json!({"net":{"wan_status":"ipv4_connected","wan_dns":"192.0.2.53"}});
    let s2 = sample(&s, 5, Some(&bt(6)), Some(&bare), Some(2));
    assert_eq!(s2.path, None, "not carried over from the older sample");
    assert_eq!(s2.dns, ["192.0.2.53"]);
}

/// Missing interface = unknown: no query on the default route, even with a DNS server known.
#[test]
fn no_interface_sends_no_direct_dns() {
    let s = Snapshot {
        dns: vec!["127.0.0.1".into()],
        path: None,
        ..Snapshot::default()
    };
    assert_eq!(crate::dns_rtt_ms(&s), None);
}

/// The direct query uses Finding's bound socket: bound to `lo` it reaches a loopback resolver,
/// bound to an interface that does not exist it says nothing. Skipped without CAP_NET_RAW.
#[test]
#[cfg(target_os = "linux")]
fn direct_dns_query_is_bound_to_the_path() {
    let sock = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = sock.local_addr().unwrap();
    std::thread::spawn(move || {
        let mut b = [0; 512];
        while let Ok((n, peer)) = sock.recv_from(&mut b) {
            let mut r = b[..n].to_vec();
            r[2] |= 0x80;
            r[3] = 0x80;
            r[7] = 1;
            r.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 192, 0, 2, 1]);
            let _ = sock.send_to(&r, peer);
        }
    });
    let path = |iface: &str| crate::finding::path::Path {
        interface: iface.into(),
        source: addr.ip(),
        source_kind: "fixture",
        dns: vec![],
    };
    if crate::finding::path::socket(&path("lo"), true).is_err() {
        eprintln!("skipped: cannot bind to a device here");
        return;
    }
    assert!(crate::dns_query_ms(&path("lo"), addr).is_some());
    assert_eq!(crate::dns_query_ms(&path("nosuch0"), addr), None);
}
