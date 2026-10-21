//! The parser on synthetic frames (`fixtures.rs`), including the malformed ones of §7.

use super::*;
use crate::fixtures::*;

fn kinds(f: &[Fact]) -> Vec<(Kind, &str)> {
    f.iter().map(|x| (x.kind, x.value.as_str())).collect()
}

#[test]
fn dhcp_options() {
    let f = dhcp_frame(
        MAC_A,
        Some("test-phone"),
        Some("android-dhcp-14"),
        &[1, 3, 6, 15, 26, 28, 51, 58, 59, 43],
    );
    assert_eq!(
        kinds(&parse(&f)),
        [
            (Kind::DhcpHostname, "test-phone"),
            (Kind::DhcpVendorClass, "android-dhcp-14"),
            (Kind::DhcpParams, "1,3,6,15,26,28,51,58,59,43"),
        ]
    );
    // to 68 (a reply), not a request port: nothing
    let mut r = f.clone();
    r[36] = 0;
    r[37] = 68;
    assert!(parse(&r).is_empty());
}

#[test]
fn dhcp_long_params_cut_on_a_comma() {
    let params: Vec<u8> = (100..200).collect();
    let f = parse(&dhcp_frame(MAC_A, None, None, &params));
    assert_eq!(f[0].kind, Kind::DhcpParams);
    assert!(
        f[0].value.len() <= MAX_LEN && !f[0].value.ends_with(','),
        "{}",
        f[0].value
    );
}

#[test]
fn mdns_laptop() {
    let f = crate::fixtures::mdns_laptop(MAC_B, IP_A, "Test Laptop", "Mac14,2");
    assert_eq!(
        kinds(&parse(&f)),
        [
            (Kind::MdnsInstance, "Test Laptop"),
            (Kind::MdnsModel, "Mac14,2"),
            (Kind::MdnsService, "_ssh._tcp"),
        ]
    );
}

#[test]
fn mdns_for_another_host_is_ignored() {
    // a sleep proxy: the A record is not the frame's source address
    let f = crate::fixtures::mdns_laptop(MAC_B, IP_A, "Test Laptop", "Mac14,2");
    let mut g = f.clone();
    // the source address in the IP header
    g[26..30].copy_from_slice(&[10, 9, 0, 99]);
    assert!(parse(&g).is_empty());
}

#[test]
fn mdns_over_ipv6_without_addresses() {
    let inst = name(&["Test Speaker", "_googlecast", "_tcp", "local"]);
    let p = mdns_response(&[rr(&name(&["_googlecast", "_tcp", "local"]), 12, &inst)]);
    let mut src = [0u8; 16];
    src[0] = 0xfe;
    src[1] = 0x80;
    src[15] = 7;
    let f = ipv6_udp(MAC_B, src, 5353, 5353, &p);
    assert_eq!(
        kinds(&parse(&f)),
        [
            (Kind::MdnsInstance, "Test Speaker"),
            (Kind::MdnsService, "_googlecast._tcp")
        ]
    );
}

#[test]
fn mdns_queries_give_nothing() {
    let mut f = crate::fixtures::mdns_laptop(MAC_B, IP_A, "Test Laptop", "Mac14,2");
    // clear QR
    f[44] = 0;
    assert!(parse(&f).is_empty());
}

#[test]
fn airplay_instance_prefix() {
    assert_eq!(bare_instance("0A1B2C3D4E5F@Test Room"), "Test Room");
    assert_eq!(bare_instance("me@home"), "me@home");
}

#[test]
fn malformed_frames_give_nothing_and_never_panic() {
    assert!(parse(&mdns_pointer_loop(MAC_B, IP_A)).is_empty());
    assert!(parse(&mdns_oversize(MAC_B, IP_A)).is_empty());
    // a forward pointer and a pointer chain longer than the limit
    assert_eq!(read_name(&[0xc0, 2, 0], 0), None);
    let mut chain = vec![0u8];
    for i in 0..40u8 {
        // the pair at 1 + 2i points at the one before it (the first at the root byte)
        chain.extend_from_slice(&[0xc0, if i == 0 { 0 } else { 2 * i - 1 }]);
    }
    assert_eq!(read_name(&chain, chain.len() - 2), None);
    assert_eq!(
        read_name(&chain, 1 + 2 * 10),
        Some((vec![], 1 + 2 * 10 + 2))
    );
    // every truncation of every fixture
    let all = [
        dhcp_frame(MAC_A, Some("test-phone"), Some("MSFT 5.0"), &[1, 3, 6]),
        crate::fixtures::mdns_laptop(MAC_B, IP_A, "Test Laptop", "Mac14,2"),
        ssdp_notify(MAC_B, IP_A, "Linux/5.0 UPnP/1.0 TestCo/2.1"),
        netbios_register(MAC_B, IP_A, "TESTPC", 0x00, false),
        mdns_pointer_loop(MAC_B, IP_A),
        mdns_oversize(MAC_B, IP_A),
    ];
    for f in &all {
        for n in 0..f.len() {
            let _ = parse(&f[..n]);
        }
        // and with every byte flipped in turn
        for i in 14..f.len() {
            let mut g = f.clone();
            g[i] ^= 0xff;
            assert!(parse(&g).len() <= MAX_FACTS);
        }
    }
    assert!(parse(&[]).is_empty());
}

#[test]
fn ssdp_notify_server_only() {
    let f = ssdp_notify(MAC_B, IP_A, "Linux/5.0 UPnP/1.0 TestCo/2.1");
    assert_eq!(
        kinds(&parse(&f)),
        [(Kind::SsdpServer, "Linux/5.0 UPnP/1.0 TestCo/2.1")]
    );
    let ms = ipv4_udp(
        MAC_B,
        IP_A,
        SSDP4,
        50000,
        1900,
        b"M-SEARCH * HTTP/1.1\r\nSERVER: x\r\n\r\n",
    );
    assert!(parse(&ms).is_empty());
}

#[test]
fn netbios_registration() {
    let f = netbios_register(MAC_B, IP_A, "TESTPC", 0x00, false);
    assert_eq!(kinds(&parse(&f)), [(Kind::NetbiosName, "TESTPC")]);
    // a workgroup (group name), another suffix: nothing
    assert!(parse(&netbios_register(MAC_B, IP_A, "WORKGROUP", 0x00, true)).is_empty());
    assert!(parse(&netbios_register(MAC_B, IP_A, "TESTPC", 0x1d, false)).is_empty());
}

#[test]
fn values_are_cleaned_and_capped() {
    let long = "y".repeat(60) + "é\u{7}zzzz";
    let f = parse(&dhcp_frame(MAC_A, Some(&long), None, &[]));
    assert!(f[0].value.len() <= MAX_LEN);
    assert!(!f[0].value.chars().any(char::is_control));
    let many: Vec<(Kind, String)> = (0..20)
        .map(|i| (Kind::MdnsService, format!("_s{i}._tcp")))
        .collect();
    assert_eq!(finish(many).len(), MAX_FACTS);
}
