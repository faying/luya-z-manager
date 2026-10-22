//! The parser (lan-client-id.md §4, §4a): one Ethernet frame in, at most [`MAX_FACTS`] facts
//! about its sender out. Every index is checked, so no input makes it panic (in the component a
//! panic would only be a trap, but a trap costs the whole batch a second pass); a frame it does
//! not understand gives no facts.
//!
//! - DHCP (UDP to 67, BOOTREQUEST): options 12, 60, 55.
//! - mDNS (UDP from or to 5353): responses only. Instance names, `_device-info` `model=`,
//!   service types. When the message carries addresses and none is the frame's source IP, it
//!   answers for another host (a sleep proxy): nothing is taken.
//! - SSDP (UDP to 1900): `SERVER:` of a NOTIFY (an M-SEARCH names what it looks for, not itself).
//! - NetBIOS name service (UDP to 137): only registrations and refreshes (a query names another
//!   host), unique names with suffix 0x00 or 0x20.

pub const MAX_FACTS: usize = 8;
pub const MAX_LEN: usize = 64;
/// DNS names: at most this many compression jumps (each must also point backwards).
const MAX_JUMPS: usize = 16;
/// mDNS: at most this many resource records looked at per message.
const MAX_RRS: usize = 64;
/// mDNS: service types taken per message.
const MAX_SERVICES: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    DhcpHostname,
    DhcpVendorClass,
    DhcpParams,
    MdnsInstance,
    MdnsModel,
    MdnsService,
    SsdpServer,
    NetbiosName,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fact {
    pub kind: Kind,
    pub value: String,
}

fn be16(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]))
}

struct Udp<'a> {
    src_ip: &'a [u8],
    sport: u16,
    dport: u16,
    payload: &'a [u8],
}

fn udp(frame: &[u8]) -> Option<Udp<'_>> {
    let ip = frame.get(14..)?;
    let (src_ip, seg) = match be16(frame, 12)? {
        0x0800 => {
            let v = *ip.first()?;
            let ihl = usize::from(v & 0x0f) * 4;
            if v >> 4 != 4 || ihl < 20 || *ip.get(9)? != 17 {
                return None;
            }
            // first fragment of an unfragmented datagram only
            if be16(ip, 6)? & 0x3fff != 0 {
                return None;
            }
            let total = usize::from(be16(ip, 2)?).min(ip.len());
            (ip.get(12..16)?, ip.get(ihl..total)?)
        }
        0x86dd => {
            if *ip.first()? >> 4 != 6 || *ip.get(6)? != 17 {
                return None;
            }
            let end = (40 + usize::from(be16(ip, 4)?)).min(ip.len());
            (ip.get(8..24)?, ip.get(40..end)?)
        }
        _ => return None,
    };
    let ulen = usize::from(be16(seg, 4)?);
    let end = if ulen >= 8 {
        ulen.min(seg.len())
    } else {
        seg.len()
    };
    Some(Udp {
        src_ip,
        sport: be16(seg, 0)?,
        dport: be16(seg, 2)?,
        payload: seg.get(8..end)?,
    })
}

/// The facts in one frame (cleaned, deduplicated, at most [`MAX_FACTS`]).
pub fn parse(frame: &[u8]) -> Vec<Fact> {
    let Some(u) = udp(frame) else {
        return Vec::new();
    };
    let raw = if u.dport == 67 {
        dhcp(u.payload)
    } else if u.sport == 5353 || u.dport == 5353 {
        mdns(u.payload, u.src_ip)
    } else if u.dport == 1900 {
        ssdp(u.payload)
    } else if u.dport == 137 {
        netbios(u.payload)
    } else {
        Vec::new()
    };
    finish(raw)
}

/// No control characters, trimmed, at most [`MAX_LEN`] bytes (cut on a character boundary),
/// no empty or repeated facts, at most [`MAX_FACTS`].
fn finish(raw: Vec<(Kind, String)>) -> Vec<Fact> {
    let mut out: Vec<Fact> = Vec::new();
    for (kind, v) in raw {
        let clean: String = v.chars().filter(|c| !c.is_control()).collect();
        let mut s = clean.trim().to_owned();
        if s.len() > MAX_LEN {
            let mut cut = MAX_LEN;
            while !s.is_char_boundary(cut) {
                cut -= 1;
            }
            s.truncate(cut);
            s = s.trim_end().to_owned();
        }
        if s.is_empty() || out.iter().any(|f| f.kind == kind && f.value == s) {
            continue;
        }
        out.push(Fact { kind, value: s });
        if out.len() == MAX_FACTS {
            break;
        }
    }
    out
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

// ── DHCP ─────────────────────────────────────────────────────────────────────

fn dhcp(p: &[u8]) -> Vec<(Kind, String)> {
    let mut out = Vec::new();
    if p.len() < 240 || p[0] != 1 || p[236..240] != [99, 130, 83, 99] {
        return out;
    }
    let mut i = 240;
    while let Some(&code) = p.get(i) {
        match code {
            0 => {
                i += 1;
                continue;
            }
            255 => break,
            _ => {}
        }
        let Some(&l) = p.get(i + 1) else { break };
        let Some(data) = p.get(i + 2..i + 2 + usize::from(l)) else {
            break;
        };
        match code {
            12 => out.push((Kind::DhcpHostname, text(data))),
            60 => out.push((Kind::DhcpVendorClass, text(data))),
            55 => {
                let mut s = String::new();
                for c in data {
                    let n = c.to_string();
                    if s.len() + n.len() + 1 > MAX_LEN {
                        break;
                    }
                    if !s.is_empty() {
                        s.push(',');
                    }
                    s.push_str(&n);
                }
                out.push((Kind::DhcpParams, s));
            }
            _ => {}
        }
        i += 2 + usize::from(l);
    }
    out
}

// ── mDNS ─────────────────────────────────────────────────────────────────────

/// A DNS name at `start` (labels) and where the bytes after it begin. `None` for anything
/// malformed: a pointer that does not point strictly backwards, more than [`MAX_JUMPS`] jumps,
/// over 255 bytes, a label running off the end, the reserved label types.
pub fn read_name(p: &[u8], start: usize) -> Option<(Vec<String>, usize)> {
    let mut pos = start;
    let mut jumps = 0;
    let mut total = 0usize;
    let mut next = None;
    let mut labels = Vec::new();
    loop {
        let b = *p.get(pos)?;
        match b & 0xc0 {
            0xc0 => {
                let off = (usize::from(b & 0x3f) << 8) | usize::from(*p.get(pos + 1)?);
                if off >= pos {
                    return None;
                }
                jumps += 1;
                if jumps > MAX_JUMPS {
                    return None;
                }
                next.get_or_insert(pos + 2);
                pos = off;
            }
            0x00 => {
                if b == 0 {
                    next.get_or_insert(pos + 1);
                    break;
                }
                let l = usize::from(b);
                total += l + 1;
                if total > 255 {
                    return None;
                }
                labels.push(text(p.get(pos + 1..pos + 1 + l)?));
                pos += 1 + l;
            }
            _ => return None,
        }
    }
    Some((labels, next?))
}

fn is_service(l: &[String]) -> bool {
    l.len() == 2 && l[0].starts_with('_') && (l[1] == "_tcp" || l[1] == "_udp")
}

/// `<instance>.<_svc>.<_tcp>.local` → (instance, "_svc._tcp")
fn instance_of(owner: &[String]) -> Option<(String, String)> {
    if owner.len() == 4 && owner[3].eq_ignore_ascii_case("local") && is_service(&owner[1..3]) {
        Some((owner[0].clone(), format!("{}.{}", owner[1], owner[2])))
    } else {
        None
    }
}

/// `_svc._tcp.local` → "_svc._tcp"
fn service_of(name: &[String]) -> Option<String> {
    if name.len() == 3 && name[2].eq_ignore_ascii_case("local") && is_service(&name[0..2]) {
        Some(format!("{}.{}", name[0], name[1]))
    } else {
        None
    }
}

fn add_service(s: String, services: &mut Vec<String>) {
    if s != "_device-info._tcp" && !services.contains(&s) && services.len() < MAX_SERVICES {
        services.push(s);
    }
}

/// Services whose instance name is the whole device's name (set by its owner or the OS).
/// Any app can announce its own service under any name (Android Quick Share:
/// `nearby-presence-nsd-<uuid>`), so only these are taken as a name.
const DEVICE_NAMED: &[&str] = &[
    "_companion-link._tcp",
    "_airplay._tcp",
    "_raop._tcp",
    "_smb._tcp",
    "_afpovertcp._tcp",
    "_workstation._tcp",
    "_ssh._tcp",
    "_sftp-ssh._tcp",
    "_rfb._tcp",
    "_hap._tcp",
    "_hap._udp",
    "_googlecast._tcp",
    "_ipp._tcp",
    "_ipps._tcp",
    "_printer._tcp",
    "_pdl-datastream._tcp",
];

fn names_device(service: &str) -> bool {
    DEVICE_NAMED.iter().any(|d| d.eq_ignore_ascii_case(service))
}

/// AirPlay's `<MAC>@<name>` instances: the name.
fn bare_instance(s: &str) -> String {
    match s.split_once('@') {
        Some((pre, name)) if !pre.is_empty() && pre.bytes().all(|b| b.is_ascii_hexdigit()) => {
            name.to_owned()
        }
        _ => s.to_owned(),
    }
}

fn mdns(p: &[u8], src_ip: &[u8]) -> Vec<(Kind, String)> {
    let mut out = Vec::new();
    let (Some(flags), Some(qd), Some(an), Some(ns), Some(ar)) =
        (be16(p, 2), be16(p, 4), be16(p, 6), be16(p, 8), be16(p, 10))
    else {
        return out;
    };
    if flags & 0x8000 == 0 {
        return out;
    }
    let mut pos = 12;
    for _ in 0..qd.min(MAX_RRS as u16) {
        let Some((_, next)) = read_name(p, pos) else {
            return out;
        };
        pos = next + 4;
    }
    let mut addrs: Vec<&[u8]> = Vec::new();
    let mut info: Option<String> = None;
    let mut model: Option<String> = None;
    let mut instances: Vec<String> = Vec::new();
    let mut services: Vec<String> = Vec::new();
    let rrs = (usize::from(an) + usize::from(ns) + usize::from(ar)).min(MAX_RRS);
    for _ in 0..rrs {
        let Some((owner, next)) = read_name(p, pos) else {
            break;
        };
        let (Some(rtype), Some(rdlen)) = (be16(p, next), be16(p, next + 8)) else {
            break;
        };
        let rstart = next + 10;
        let Some(rdata) = p.get(rstart..rstart + usize::from(rdlen)) else {
            break;
        };
        pos = rstart + usize::from(rdlen);
        match rtype {
            1 if rdata.len() == 4 => addrs.push(rdata),
            28 if rdata.len() == 16 => addrs.push(rdata),
            12 => {
                let Some((target, _)) = read_name(p, rstart) else {
                    continue;
                };
                let lower: Vec<String> = owner.iter().map(|l| l.to_ascii_lowercase()).collect();
                if lower == ["_services", "_dns-sd", "_udp", "local"] {
                    if let Some(s) = service_of(&target) {
                        add_service(s, &mut services);
                    }
                } else if let Some(s) = service_of(&owner) {
                    if let Some((inst, ts)) = instance_of(&target)
                        && ts == s
                        && names_device(&s)
                    {
                        instances.push(inst);
                    }
                    add_service(s, &mut services);
                }
            }
            33 => {
                if let Some((inst, s)) = instance_of(&owner) {
                    if s == "_device-info._tcp" {
                        info.get_or_insert(inst);
                    } else if names_device(&s) {
                        instances.push(inst);
                    }
                    add_service(s, &mut services);
                }
            }
            16 => {
                let Some((inst, s)) = instance_of(&owner) else {
                    continue;
                };
                if s != "_device-info._tcp" {
                    continue;
                }
                info.get_or_insert(inst);
                let mut i = 0;
                while let Some(&l) = rdata.get(i) {
                    let Some(kv) = rdata.get(i + 1..i + 1 + usize::from(l)) else {
                        break;
                    };
                    if let Some(v) = kv.strip_prefix(b"model=") {
                        model.get_or_insert(text(v));
                    }
                    i += 1 + usize::from(l);
                }
            }
            _ => {}
        }
    }
    // answering for another host (a sleep proxy): its names are not this sender's
    if !addrs.is_empty() && !addrs.contains(&src_ip) {
        return out;
    }
    if let Some(i) = info.or_else(|| instances.into_iter().next()) {
        out.push((Kind::MdnsInstance, bare_instance(&i)));
    }
    if let Some(m) = model {
        out.push((Kind::MdnsModel, m));
    }
    out.extend(services.into_iter().map(|s| (Kind::MdnsService, s)));
    out
}

// ── SSDP ─────────────────────────────────────────────────────────────────────

fn ssdp(p: &[u8]) -> Vec<(Kind, String)> {
    if !p.starts_with(b"NOTIFY ") {
        return Vec::new();
    }
    let t = text(p);
    t.split('\n')
        .filter_map(|line| line.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("server"))
        .map(|(_, v)| vec![(Kind::SsdpServer, v.trim().to_owned())])
        .unwrap_or_default()
}

// ── NetBIOS ──────────────────────────────────────────────────────────────────

/// Skips a DNS-style name (NetBIOS: no compression in the question; a pointer in the
/// additional record).
fn skip_name(p: &[u8], mut pos: usize) -> Option<usize> {
    for _ in 0..128 {
        let b = *p.get(pos)?;
        if b & 0xc0 == 0xc0 {
            return Some(pos + 2);
        }
        if b == 0 {
            return Some(pos + 1);
        }
        pos += 1 + usize::from(b);
    }
    None
}

fn netbios(p: &[u8]) -> Vec<(Kind, String)> {
    let none = Vec::new;
    let (Some(flags), Some(qd)) = (be16(p, 2), be16(p, 4)) else {
        return none();
    };
    let opcode = (flags >> 11) & 0x0f;
    // requests only: registration (5), refresh (8, 9)
    if flags & 0x8000 != 0 || !matches!(opcode, 5 | 8 | 9) || qd == 0 {
        return none();
    }
    if p.get(12) != Some(&32) {
        return none();
    }
    let Some(enc) = p.get(13..45) else {
        return none();
    };
    let mut raw = [0u8; 16];
    for (k, b) in raw.iter_mut().enumerate() {
        let (hi, lo) = (
            enc[2 * k].wrapping_sub(b'A'),
            enc[2 * k + 1].wrapping_sub(b'A'),
        );
        if hi > 15 || lo > 15 {
            return none();
        }
        *b = (hi << 4) | lo;
    }
    if !matches!(raw[15], 0x00 | 0x20) {
        return none();
    }
    let Some(after) = skip_name(p, 12) else {
        return none();
    };
    if be16(p, after) != Some(0x20) {
        return none();
    }
    // the additional record's NB_FLAGS: a group name is a workgroup, not this host
    let ar = be16(p, 10).unwrap_or(0);
    if ar > 0
        && let Some(n) = skip_name(p, after + 4)
        && let Some(nb) = be16(p, n + 10)
        && nb & 0x8000 != 0
    {
        return none();
    }
    let name = &raw[..15];
    if name.iter().any(|&b| !(0x20..0x7f).contains(&b)) || name.starts_with(b"__") {
        return none();
    }
    vec![(Kind::NetbiosName, text(name).trim_end().to_owned())]
}

#[cfg(test)]
mod tests;
