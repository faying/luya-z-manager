// Synthetic frames for the lanid tests (lan-client-id.md §5: the public tests use made-up data,
// never a capture). Shared by this crate's tests and luyaz-core's (`#[path]` include), so: plain
// functions over std only, no crate items.
#![allow(dead_code)]

pub const MAC_A: [u8; 6] = [0x02, 0x00, 0x5e, 0x10, 0x00, 0x01];
/// RFC 7042 documentation range
pub const MAC_B: [u8; 6] = [0x00, 0x00, 0x5e, 0x00, 0x53, 0x02];
pub const IP_A: [u8; 4] = [10, 9, 0, 21];
pub const MDNS4: [u8; 4] = [224, 0, 0, 251];
pub const SSDP4: [u8; 4] = [239, 255, 255, 250];
pub const BCAST: [u8; 4] = [255, 255, 255, 255];

fn csum0() -> [u8; 2] {
    [0, 0]
}

/// Ethernet + IPv4 + UDP around `payload`.
pub fn ipv4_udp(
    src_mac: [u8; 6],
    src: [u8; 4],
    dst: [u8; 4],
    sport: u16,
    dport: u16,
    payload: &[u8],
) -> Vec<u8> {
    let mut f = vec![0xff; 6];
    f.extend_from_slice(&src_mac);
    f.extend_from_slice(&[0x08, 0x00]);
    let total = (20 + 8 + payload.len()) as u16;
    f.extend_from_slice(&[0x45, 0]);
    f.extend_from_slice(&total.to_be_bytes());
    f.extend_from_slice(&[0, 0, 0, 0, 64, 17]);
    f.extend_from_slice(&csum0());
    f.extend_from_slice(&src);
    f.extend_from_slice(&dst);
    f.extend_from_slice(&sport.to_be_bytes());
    f.extend_from_slice(&dport.to_be_bytes());
    f.extend_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
    f.extend_from_slice(&csum0());
    f.extend_from_slice(payload);
    f
}

/// Ethernet + IPv6 + UDP around `payload`.
pub fn ipv6_udp(
    src_mac: [u8; 6],
    src: [u8; 16],
    sport: u16,
    dport: u16,
    payload: &[u8],
) -> Vec<u8> {
    let mut f = vec![0x33, 0x33, 0, 0, 0, 0xfb];
    f.extend_from_slice(&src_mac);
    f.extend_from_slice(&[0x86, 0xdd]);
    f.extend_from_slice(&[0x60, 0, 0, 0]);
    f.extend_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
    f.extend_from_slice(&[17, 255]);
    f.extend_from_slice(&src);
    let mut dst = [0u8; 16];
    dst[0] = 0xff;
    dst[1] = 0x02;
    dst[15] = 0xfb;
    f.extend_from_slice(&dst);
    f.extend_from_slice(&sport.to_be_bytes());
    f.extend_from_slice(&dport.to_be_bytes());
    f.extend_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
    f.extend_from_slice(&csum0());
    f.extend_from_slice(payload);
    f
}

/// A DHCPREQUEST with options 12, 60, 55 when given.
pub fn dhcp_request(
    chaddr: [u8; 6],
    hostname: Option<&str>,
    vendor: Option<&str>,
    params: &[u8],
) -> Vec<u8> {
    let mut p = vec![0u8; 236];
    p[0] = 1;
    p[1] = 1;
    p[2] = 6;
    p[28..34].copy_from_slice(&chaddr);
    p.extend_from_slice(&[99, 130, 83, 99]);
    p.extend_from_slice(&[53, 1, 3]);
    if let Some(h) = hostname {
        p.push(12);
        p.push(h.len() as u8);
        p.extend_from_slice(h.as_bytes());
    }
    if let Some(v) = vendor {
        p.push(60);
        p.push(v.len() as u8);
        p.extend_from_slice(v.as_bytes());
    }
    if !params.is_empty() {
        p.push(55);
        p.push(params.len() as u8);
        p.extend_from_slice(params);
    }
    p.push(255);
    p
}

pub fn dhcp_frame(
    mac: [u8; 6],
    hostname: Option<&str>,
    vendor: Option<&str>,
    params: &[u8],
) -> Vec<u8> {
    ipv4_udp(
        mac,
        [0; 4],
        BCAST,
        68,
        67,
        &dhcp_request(mac, hostname, vendor, params),
    )
}

/// A DNS name, uncompressed.
pub fn name(labels: &[&str]) -> Vec<u8> {
    let mut b = Vec::new();
    for l in labels {
        b.push(l.len() as u8);
        b.extend_from_slice(l.as_bytes());
    }
    b.push(0);
    b
}

/// One resource record: owner bytes, type, rdata (class IN with cache flush, TTL 120).
pub fn rr(owner: &[u8], rtype: u16, rdata: &[u8]) -> Vec<u8> {
    let mut b = owner.to_vec();
    b.extend_from_slice(&rtype.to_be_bytes());
    b.extend_from_slice(&[0x80, 0x01, 0, 0, 0, 120]);
    b.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
    b.extend_from_slice(rdata);
    b
}

/// An mDNS response with these answers.
pub fn mdns_response(answers: &[Vec<u8>]) -> Vec<u8> {
    let mut p = vec![0, 0, 0x84, 0, 0, 0];
    p.extend_from_slice(&(answers.len() as u16).to_be_bytes());
    p.extend_from_slice(&[0, 0, 0, 0]);
    for a in answers {
        p.extend_from_slice(a);
    }
    p
}

pub fn txt(items: &[&str]) -> Vec<u8> {
    let mut b = Vec::new();
    for i in items {
        b.push(i.len() as u8);
        b.extend_from_slice(i.as_bytes());
    }
    b
}

/// A laptop announcing itself: `_device-info` with a model, an SRV for `_ssh._tcp`, its A record.
pub fn mdns_laptop(mac: [u8; 6], ip: [u8; 4], instance: &str, model: &str) -> Vec<u8> {
    let svc = name(&["_ssh", "_tcp", "local"]);
    let inst = name(&[instance, "_ssh", "_tcp", "local"]);
    let mut srv = vec![0, 0, 0, 0, 0, 22];
    srv.extend_from_slice(&name(&["test-host", "local"]));
    let answers = vec![
        rr(&svc, 12, &inst),
        rr(&inst, 33, &srv),
        rr(
            &name(&[instance, "_device-info", "_tcp", "local"]),
            16,
            &txt(&[&format!("model={model}"), "osxvers=99"]),
        ),
        rr(&name(&["test-host", "local"]), 1, &ip),
    ];
    ipv4_udp(mac, ip, MDNS4, 5353, 5353, &mdns_response(&answers))
}

/// A response whose PTR target is a compression pointer to itself (a loop).
pub fn mdns_pointer_loop(mac: [u8; 6], ip: [u8; 4]) -> Vec<u8> {
    let mut p = vec![0, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 0];
    // owner: a pointer to offset 12, i.e. to itself
    p.extend_from_slice(&[0xc0, 12]);
    p.extend_from_slice(&[0, 12, 0, 1, 0, 0, 0, 120, 0, 2, 0xc0, 12]);
    ipv4_udp(mac, ip, MDNS4, 5353, 5353, &p)
}

/// A response with one record that claims more rdata than there is, then a label of 63 bytes
/// repeated past the 255-byte name limit.
pub fn mdns_oversize(mac: [u8; 6], ip: [u8; 4]) -> Vec<u8> {
    let long = "x".repeat(63);
    let owner = name(&[&long, &long, &long, &long, &long, "local"]);
    let mut a = rr(&owner, 16, &txt(&["model=Oversize1,1"]));
    // rdlen far past the end
    let n = a.len();
    a[n - 20] = 0xff;
    a[n - 19] = 0xff;
    ipv4_udp(mac, ip, MDNS4, 5353, 5353, &mdns_response(&[a]))
}

pub fn ssdp_notify(mac: [u8; 6], ip: [u8; 4], server: &str) -> Vec<u8> {
    let p = format!(
        "NOTIFY * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nNT: upnp:rootdevice\r\nNTS: ssdp:alive\r\nSERVER: {server}\r\nLOCATION: http://10.9.0.30:1400/x.xml\r\n\r\n"
    );
    ipv4_udp(mac, ip, SSDP4, 1900, 1900, p.as_bytes())
}

/// A NetBIOS name registration (opcode 5) for `nb` with this suffix; `group` sets the G flag.
pub fn netbios_register(mac: [u8; 6], ip: [u8; 4], nb: &str, suffix: u8, group: bool) -> Vec<u8> {
    let mut raw = [b' '; 16];
    for (i, b) in nb.bytes().take(15).enumerate() {
        raw[i] = b;
    }
    raw[15] = suffix;
    let mut p = vec![0x12, 0x34, 0x29, 0x10, 0, 1, 0, 0, 0, 0, 0, 1];
    p.push(32);
    for b in raw {
        p.push(b'A' + (b >> 4));
        p.push(b'A' + (b & 0x0f));
    }
    p.push(0);
    p.extend_from_slice(&[0, 0x20, 0, 1]);
    p.extend_from_slice(&[0xc0, 12, 0, 0x20, 0, 1, 0, 0, 0, 0, 0, 6]);
    p.extend_from_slice(&[if group { 0x80 } else { 0 }, 0]);
    p.extend_from_slice(&ip);
    ipv4_udp(mac, ip, [10, 9, 0, 255], 137, 137, &p)
}
