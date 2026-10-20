use super::*;
use ResultState::*;
use serde_json::json;
use std::cell::RefCell;
use std::net::TcpListener;

fn boot(seconds: u64) -> BootTime {
    BootTime {
        boot_id: "a".into(),
        seconds,
    }
}
fn p(v6: bool) -> Path {
    Path {
        interface: "rmnet_data0".into(),
        source_kind: "fixture",
        source: if v6 { "2001:db8::2" } else { "192.0.2.2" }
            .parse()
            .unwrap(),
        dns: vec![
            if v6 { "2001:db8::53" } else { "192.0.2.53" }
                .parse()
                .unwrap(),
        ],
    }
}

struct Fake {
    calls: RefCell<Vec<(bool, SocketAddr, String)>>,
    http: [ResultState; 2],
    dns: ResultState,
}
impl Transport for Fake {
    fn http(&self, p: &Path, t: SocketAddr) -> ResultState {
        let mut c = self.calls.borrow_mut();
        let r = self.http[c.iter().filter(|(dns, _, _)| !dns).count()];
        c.push((false, t, p.interface.clone()));
        r
    }
    fn dns(&self, p: &Path, t: SocketAddr) -> ResultState {
        self.calls.borrow_mut().push((true, t, p.interface.clone()));
        self.dns
    }
}
fn fake(http: [ResultState; 2], dns: ResultState) -> Fake {
    Fake {
        calls: RefCell::new(Vec::new()),
        http,
        dns,
    }
}

#[test]
fn ipv6_only_and_per_family_identity_are_discovered_without_guessing() {
    let v = json!({"interfaces": {"cellular": {"ipv4_address":"", "ipv6_address":"2001:db8::2/64", "ipv6_dev_name":"rmnet_data2"},
        "wan6": {"up":true, "device":"rmnet_data2", "dns":["2001:db8::53", "192.0.2.53"]}}});
    assert_eq!(has_ip(&v), Some(true));
    let paths = paths(&v);
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].interface, "rmnet_data2");
    assert_eq!(paths[0].dns, ["2001:db8::53".parse::<IpAddr>().unwrap()]);
    let mut bad = v.clone();
    bad["interfaces"]["wan6"]["device"] = "tailscale0".into();
    assert!(super::paths(&bad).is_empty());
    bad["interfaces"]["wan6"]["device"] = "rmnet_data2".into();
    bad["interfaces"]["cellular"]["ipv6_address"] = "".into();
    bad["interfaces"]["wan6"]["ipv6"] = json!([{"address":"2001:db8::9"}]);
    assert!(super::paths(&bad)[0].source.is_ipv6());
    assert_eq!(has_ip(&bad), Some(true));
    bad["interfaces"]["wan6"]["up"] = false.into();
    assert_eq!(has_ip(&bad), Some(false));
    for text in [
        "0.0.0.0",
        "::",
        "fe80::1",
        "127.0.0.1",
        "garbage",
        "224.0.0.1",
        "2001:db8::1/129",
        "192.0.2.1/bad",
    ] {
        assert!(address(text).is_none(), "{text}");
    }
    assert_eq!(
        has_ip(&json!({"interfaces":{"cellular":{"ipv4_address":""}}})),
        None
    );
    for iface in ["", "rmnet_data0;", "../../lo", "too-long-interface", "x\0y"] {
        assert!(!valid_interface(iface));
    }
}

#[test]
fn scoped_results_budget_and_unknown_tls_never_claim_internet() {
    let f = fake([Failed, Passed], Failed);
    let n = observe(
        &[p(false), p(true)],
        Some(boot(0)),
        Some(boot(30)),
        false,
        &f,
    );
    assert_eq!(
        (n.path_reachable, n.dns, n.tls, n.internet_usable),
        (Passed, Failed, Unknown, Unknown)
    );
    assert_eq!(n.paths[0].path_reachable, Failed);
    assert_eq!(n.paths[1].path_reachable, Passed);
    let calls = f.calls.borrow();
    assert_eq!(calls.len(), 4);
    assert!(calls[0].1.is_ipv4() && calls[1].1.is_ipv6());
    assert!(calls[2].1.is_ipv4() && calls[3].1.is_ipv6());
    assert!(calls.iter().all(|(_, _, iface)| iface == "rmnet_data0"));
    // An untried alternate family stays unknown when a local bind/setup failed.
    let f = fake([Unknown, Failed], Unknown);
    let n = observe(
        &[p(false), p(true)],
        Some(boot(0)),
        Some(boot(1)),
        false,
        &f,
    );
    assert_eq!(n.path_reachable, Unknown);
    // A healthy alternate target wins; target failure alone is not a global outage.
    let f = fake([Failed, Passed], Passed);
    let n = observe(&[p(false)], Some(boot(0)), Some(boot(1)), false, &f);
    assert_eq!((n.path_reachable, n.dns), (Passed, Passed));
    assert_eq!(f.calls.borrow().len(), 3);
}

#[test]
fn unknown_stale_boot_changed_or_proxy_paths_send_no_packets() {
    for (then, now, proxy, reason) in [
        (None, Some(boot(1)), false, "stale_or_unknown_snapshot"),
        (
            Some(boot(0)),
            Some(boot(31)),
            false,
            "stale_or_unknown_snapshot",
        ),
        (
            Some(boot(2)),
            Some(boot(1)),
            false,
            "stale_or_unknown_snapshot",
        ),
        (
            Some(boot(0)),
            Some(BootTime {
                boot_id: "b".into(),
                seconds: 1,
            }),
            false,
            "stale_or_unknown_snapshot",
        ),
        (Some(boot(0)), Some(boot(1)), true, "proxy_route_unverified"),
    ] {
        let f = fake([Passed, Passed], Passed);
        let n = observe(&[p(true)], then, now, proxy, &f);
        assert_eq!(n.reason, reason);
        assert_eq!(n.path_reachable, Unknown);
        assert!(f.calls.borrow().is_empty());
    }
    let f = fake([Passed, Passed], Passed);
    assert_eq!(
        observe(&[], Some(boot(0)), Some(boot(1)), false, &f).reason,
        "no_cellular_path"
    );
    assert!(f.calls.borrow().is_empty());
}

fn answer(q: &[u8], qtype: u16, data: &[u8]) -> Vec<u8> {
    let mut b = q.to_vec();
    b[2] = 0x81;
    b[3] = 0x80;
    b[7] = 1;
    b.extend_from_slice(&[0xc0, 0x0c]);
    b.extend_from_slice(&qtype.to_be_bytes());
    b.extend_from_slice(&[0, 1, 0, 0, 0, 1]);
    b.extend_from_slice(&(data.len() as u16).to_be_bytes());
    b.extend_from_slice(data);
    b
}

#[test]
fn dns_needs_the_question_and_an_address_not_just_an_id_or_nxdomain() {
    let q = crate::dns_query(123, "www.qq.com");
    let good = answer(&q, 1, &[192, 0, 2, 1]);
    assert_eq!(dns_answer(&good, &q), Some(Passed));
    let mut nx = q.clone();
    nx[2] = 0x81;
    nx[3] = 0x83;
    assert_eq!(dns_answer(&nx, &q), Some(Failed));
    nx[3] = 0x82;
    assert_eq!(dns_answer(&nx, &q), Some(Failed));
    nx[3] = 0x80;
    assert_eq!(dns_answer(&nx, &q), Some(Unknown));
    let mut wrong = good.clone();
    wrong[0] ^= 1;
    assert_eq!(dns_answer(&wrong, &q), None);
    wrong = good.clone();
    wrong[13] = b'x';
    assert_eq!(dns_answer(&wrong, &q), None);
    wrong = good.clone();
    wrong[2] |= 2;
    assert_eq!(dns_answer(&wrong, &q), None);
    assert_eq!(dns_answer(&good[..good.len() - 1], &q), None);
    assert_eq!(dns_answer(&answer(&q, 1, &[1]), &q), Some(Unknown));
    let mut q6 = q;
    let i = q6.len() - 3;
    q6[i] = 28;
    assert_eq!(dns_answer(&answer(&q6, 28, &[0; 16]), &q6), Some(Passed));
}

/// Real sockets only to local stubs. These never contact a public endpoint or a device.
#[test]
#[cfg(target_os = "linux")]
fn real_http_is_bound_for_both_families_and_never_falls_back() {
    for bind in ["127.0.0.1:0", "[::1]:0"] {
        let listener = TcpListener::bind(bind).expect("test container needs IPv4/IPv6 loopback");
        let addr = listener.local_addr().unwrap();
        let path = Path {
            interface: "lo".into(),
            source_kind: "fixture",
            source: addr.ip(),
            dns: vec![],
        };
        // Bind failures must remain unknown, not a network failure, and must not hit the stub.
        let bad = Path {
            interface: "nosuch0".into(),
            ..path.clone()
        };
        assert_eq!(BoundSockets.http(&bad, addr), Unknown);
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut b = [0; 256];
            let _ = stream.read(&mut b).unwrap();
            stream
                .write_all(b"HTTP/1.1 302 Found\r\nLocation: http://portal/\r\n\r\n")
                .unwrap();
        });
        assert_eq!(BoundSockets.http(&path, addr), Passed);
        thread.join().unwrap();
    }
}

#[test]
#[cfg(target_os = "linux")]
fn real_dns_is_bound_and_nxdomain_fails_over_both_families() {
    for bind in ["127.0.0.1:0", "[::1]:0"] {
        let sock = UdpSocket::bind(bind).unwrap();
        sock.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let addr = sock.local_addr().unwrap();
        let thread = std::thread::spawn(move || {
            let mut b = [0; 512];
            let (n, peer) = sock.recv_from(&mut b).unwrap();
            let qtype = u16::from_be_bytes([b[n - 4], b[n - 3]]);
            assert_eq!(qtype, if addr.is_ipv6() { 28 } else { 1 });
            b[2] |= 0x80;
            b[3] = 0x83;
            sock.send_to(&b[..n], peer).unwrap();
        });
        let path = Path {
            interface: "lo".into(),
            source_kind: "fixture",
            source: addr.ip(),
            dns: vec![],
        };
        assert_eq!(BoundSockets.dns(&path, addr), Failed);
        thread.join().unwrap();
    }
}

#[test]
fn dual_stack_evidence_fits_the_existing_store_and_scoped_success_resolves() {
    use crate::finding::store::{ENTRY_MAX, Store};
    use luyaz_api::diagnose::Level;
    use luyaz_api::finding::*;
    let fake = fake([Failed, Passed], Failed);
    let n = observe(
        &[p(false), p(true)],
        Some(boot(0)),
        Some(boot(1)),
        false,
        &fake,
    );
    let t = crate::record::CLOCK_SET + 1_000_000;
    let (path, dns) = judge_network(n, t);
    let mut c = Check {
        premises: Premises {
            sim_present: Some(true),
            airplane: Some(false),
            data_on: Some(true),
            roaming: Some(false),
            roam_allowed: Some(false),
        },
        passive_down: true,
        can_probe: true,
        iccid: "8986000000000000000".into(),
        now: t,
        layers: vec![
            judge_signal(Reg::Home, t),
            Evidence::new(LayerId::Data, Level::Ok, "已连接", "Connected", t),
            path,
            dns,
        ],
    };
    let v = judge(&c);
    assert!(matches!(&v, Verdict::Open(f) if f.confidence == Confidence::Likely));
    let mut store = Store::default();
    store.observe(&v, &c.iccid, t);
    let body = &store.open[0].finding;
    assert!(body.to_string().len() <= ENTRY_MAX);
    assert_eq!(body["evidence"].as_array().unwrap().len(), 4);
    assert!(body["evidence"][2]["network"].is_object());
    let n = NetworkEvidence {
        path_reachable: Passed,
        dns: Passed,
        ..NetworkEvidence::default()
    };
    let (path, dns) = judge_network(n, t);
    c.layers[2] = path;
    c.layers[3] = dns;
    assert_eq!(judge(&c), Verdict::None { why: "path_dns_ok" });
    assert!(store.observe(&judge(&c), &c.iccid, t + 1).is_empty());
    store.observe(&judge(&c), &c.iccid, t + 2);
    assert!(store.open.is_empty());
}

#[test]
fn dns_rejects_unrelated_answers_but_accepts_a_cname_chain() {
    let q = crate::dns_query(123, "www.qq.com");
    let mut b = q.clone();
    b[2] = 0x81;
    b[3] = 0x80;
    b[7] = 1;
    let other = crate::dns_query(0, "other.example");
    let name = &other[12..other.len() - 4];
    b.extend_from_slice(name);
    b.extend_from_slice(&[0, 1, 0, 1, 0, 0, 0, 1, 0, 4, 192, 0, 2, 1]);
    assert_eq!(dns_answer(&b, &q), Some(Unknown));
    let mut chain = answer(&q, 5, name);
    chain[7] = 2;
    chain.extend_from_slice(name);
    chain.extend_from_slice(&[0, 1, 0, 1, 0, 0, 0, 1, 0, 4, 192, 0, 2, 1]);
    assert_eq!(dns_answer(&chain, &q), Some(Passed));
}

#[test]
fn unvalidated_ipv6_http_service_failure_is_unknown_not_an_outage() {
    let f = fake([Failed, Failed], Passed);
    let n = observe(&[p(true)], Some(boot(0)), Some(boot(1)), false, &f);
    assert_eq!(n.path_reachable, Unknown);
    assert_eq!(n.dns, Passed);
    assert_eq!(n.reason, "ipv6_http_targets_unverified");
    assert_eq!(n.internet_usable, Unknown);
}

#[test]
fn bound_lookups_return_addresses_or_say_no() {
    let q = crate::dns_query(7, "www.qq.com");
    assert_eq!(
        answer_addrs(&answer(&q, 1, &[192, 0, 2, 9]), &q),
        Some(Resolved::Addrs(vec!["192.0.2.9".parse().unwrap()]))
    );
    let mut nx = q.clone();
    nx[2] = 0x81;
    nx[3] = 0x83;
    assert_eq!(answer_addrs(&nx, &q), Some(Resolved::Failed));
    nx[3] = 0x80;
    assert_eq!(
        answer_addrs(&nx, &q),
        Some(Resolved::Unknown),
        "NODATA is not a failure"
    );
    let mut tc = answer(&q, 1, &[192, 0, 2, 9]);
    tc[2] |= 2;
    assert_eq!(answer_addrs(&tc, &q), Some(Resolved::Unknown), "truncated");
    let mut wrong = answer(&q, 1, &[192, 0, 2, 9]);
    wrong[0] ^= 1;
    assert_eq!(answer_addrs(&wrong, &q), None, "another query's reply");
    let mut q6 = q;
    let i = q6.len() - 3;
    q6[i] = 28;
    let mut v6 = [0u8; 16];
    v6[..2].copy_from_slice(&[0x24, 0x08]);
    assert_eq!(
        answer_addrs(&answer(&q6, 28, &v6), &q6),
        Some(Resolved::Addrs(vec![IpAddr::from(v6)]))
    );
}

#[test]
fn no_server_of_the_family_is_unknown_not_failed() {
    let path = Path {
        interface: "lo".into(),
        source_kind: "fixture",
        source: "127.0.0.1".parse().unwrap(),
        dns: vec!["::1".parse().unwrap()],
    };
    assert_eq!(
        resolve(&path, "www.qq.com", Duration::from_secs(1)),
        Resolved::Unknown
    );
}
