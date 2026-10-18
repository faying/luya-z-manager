//! On-demand cellular observations. Never fall back to an unbound socket or the host's DNS.
//! Two HTTP attempts and two DNS attempts total, including dual stack; no TLS assertion.
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, UdpSocket};
use std::time::{Duration, Instant};

use luyaz_api::clock::BootTime;
use luyaz_api::finding::{NetworkEvidence, PathEvidence, ResultState};
use serde_json::Value;
use socket2::{Domain, Protocol, Socket, Type};

const HTTP_TIMEOUT: Duration = Duration::from_secs(5);
const DNS_TIMEOUT: Duration = Duration::from_secs(2);
pub const MAX_AGE: u64 = 30;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Path {
    pub interface: String,
    pub source: IpAddr,
    pub source_kind: &'static str,
    pub dns: Vec<IpAddr>,
}

pub fn valid_interface(s: &str) -> bool {
    !s.is_empty()
        && s.len() < 16
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
}

pub fn address(s: &str) -> Option<IpAddr> {
    let (raw, prefix) = s
        .trim()
        .split_once('/')
        .map_or((s.trim(), None), |(a, b)| (a, Some(b)));
    let ip: IpAddr = raw.parse().ok()?;
    if let Some(prefix) = prefix {
        let bits = prefix.parse::<u8>().ok()?;
        if bits > if ip.is_ipv6() { 128 } else { 32 } {
            return None;
        }
    }
    let usable = match ip {
        IpAddr::V4(v) => {
            v.octets()[0] != 0
                && !v.is_unspecified()
                && !v.is_loopback()
                && !v.is_multicast()
                && !v.is_link_local()
                && !v.is_broadcast()
        }
        IpAddr::V6(v) => {
            !v.is_unspecified()
                && !v.is_loopback()
                && !v.is_multicast()
                && !v.is_unicast_link_local()
                && v.to_ipv4_mapped().is_none()
        }
    };
    usable.then_some(ip)
}

/// Existing projected get_wwaniface + zte_wan/zte_wan6. No firmware/interface-name guess.
pub fn paths(state: &Value) -> Vec<Path> {
    let i = &state["interfaces"];
    let c = &i["cellular"];
    let mut paths = Vec::new();
    for (v6, wan, addr_key, dev_key, dns_keys) in [
        (
            false,
            "wan4",
            "ipv4_address",
            "ipv4_dev_name",
            ["ipv4_dns_prefer", "ipv4_dns_standby"],
        ),
        (
            true,
            "wan6",
            "ipv6_address",
            "ipv6_dev_name",
            ["ipv6_dns_prefer", "ipv6_dns_standby"],
        ),
    ] {
        let w = &i[wan];
        let text = |v: &Value, k: &str| v[k].as_str().unwrap_or("").trim().to_string();
        let dev = text(c, dev_key);
        let netdev = text(w, "device");
        // Contradictory identities cannot be silently mixed (e.g. a different uplink).
        if !dev.is_empty() && !netdev.is_empty() && dev != netdev {
            continue;
        }
        let interface = if dev.is_empty() { netdev } else { dev };
        if !valid_interface(&interface) {
            continue;
        }
        let cell_ip = address(&text(c, addr_key)).filter(|a| a.is_ipv6() == v6);
        let net_ip = (w["up"] == true)
            .then(|| {
                w[if v6 { "ipv6" } else { "ipv4" }]
                    .as_array()?
                    .iter()
                    .filter_map(|a| address(a["address"].as_str()?))
                    .find(|a| a.is_ipv6() == v6)
            })
            .flatten();
        let Some(source) = cell_ip.or(net_ip) else {
            continue;
        };
        let mut dns: Vec<IpAddr> = dns_keys
            .iter()
            .filter_map(|k| text(c, k).parse().ok())
            .collect();
        if let Some(list) = w["dns"].as_array() {
            dns.extend(
                list.iter()
                    .filter_map(|v| v.as_str()?.parse::<IpAddr>().ok()),
            );
        }
        dns.retain(|ip| {
            ip.is_ipv6() == v6 && !ip.is_unspecified() && !ip.is_multicast() && !ip.is_loopback()
        });
        dns.dedup();
        dns.truncate(2);
        paths.push(Path {
            interface,
            source,
            source_kind: if cell_ip.is_some() {
                "get_wwaniface"
            } else {
                "netifd"
            },
            dns,
        });
    }
    paths
}

/// Missing/malformed family data is unknown, not proof that an address is absent.
pub fn has_ip(state: &Value) -> Option<bool> {
    let c = state.get("interfaces")?.get("cellular")?;
    for (v6, key, wan, array) in [
        (false, "ipv4_address", "wan4", "ipv4"),
        (true, "ipv6_address", "wan6", "ipv6"),
    ] {
        if c[key]
            .as_str()
            .and_then(address)
            .is_some_and(|a| a.is_ipv6() == v6)
        {
            return Some(true);
        }
        let w = &state["interfaces"][wan];
        if w["up"] == true
            && w[array].as_array().is_some_and(|xs| {
                xs.iter().any(|v| {
                    v["address"]
                        .as_str()
                        .and_then(address)
                        .is_some_and(|a| a.is_ipv6() == v6)
                })
            })
        {
            return Some(true);
        }
    }
    let empty = |key: &str| {
        c[key]
            .as_str()
            .is_some_and(|s| s.trim().is_empty() || s == "0.0.0.0" || s == "::")
    };
    (empty("ipv4_address") && empty("ipv6_address")).then_some(false)
}

fn fresh(observed: Option<&BootTime>, now: Option<&BootTime>) -> bool {
    observed
        .zip(now)
        .and_then(|(a, b)| b.elapsed_since(a))
        .is_some_and(|age| age <= MAX_AGE)
}

/// Endpoint pools. The IPv6 HTTP services have NOT been verified on MU5250/B31;
/// failures here cannot establish a general internet outage.
fn targets(v6: bool) -> [SocketAddr; 2] {
    if v6 {
        [
            "[2606:4700:4700::1111]:80".parse().unwrap(),
            "[2606:4700:4700::1001]:80".parse().unwrap(),
        ]
    } else {
        [
            "119.29.29.29:80".parse().unwrap(),
            "1.1.1.1:80".parse().unwrap(),
        ]
    }
}

/// Injectable transport keeps policy/family/budget tests entirely offline.
pub trait Transport {
    /// Unknown = local setup/binding failed, not evidence against the network.
    fn http(&self, path: &Path, target: SocketAddr) -> ResultState;
    fn dns(&self, path: &Path, target: SocketAddr) -> ResultState;
}

fn aggregate(states: impl Iterator<Item = ResultState>) -> ResultState {
    let s: Vec<_> = states.collect();
    if s.contains(&ResultState::Passed) {
        ResultState::Passed
    } else if !s.is_empty() && s.iter().all(|s| *s == ResultState::Failed) {
        ResultState::Failed
    } else {
        ResultState::Unknown
    }
}

pub fn observe(
    paths: &[Path],
    observed: Option<BootTime>,
    now: Option<BootTime>,
    redirected: bool,
    io: &dyn Transport,
) -> NetworkEvidence {
    let mut n = NetworkEvidence {
        observed_at: now.clone(),
        ..NetworkEvidence::default()
    };
    n.reason = if !fresh(observed.as_ref(), now.as_ref()) {
        "stale_or_unknown_snapshot"
    } else if redirected {
        "proxy_route_unverified"
    } else if paths.is_empty() {
        "no_cellular_path"
    } else {
        "scoped_http_dns_only"
    }
    .into();
    if n.reason != "scoped_http_dns_only" {
        return n;
    }
    let paths = &paths[..paths.len().min(2)];
    n.paths = paths
        .iter()
        .map(|p| PathEvidence {
            interface: p.interface.clone(),
            family: if p.source.is_ipv6() { "ipv6" } else { "ipv4" },
            source: p.source_kind,
            route: "device_bound_policy_unverified",
            path_reachable: ResultState::Unknown,
            dns: ResultState::Unknown,
        })
        .collect();
    // Interleave families. Across both families the old 2+2 attempt cap stays intact.
    for attempt in 0..2 {
        let k = attempt % paths.len();
        let p = &paths[k];
        let target = targets(p.source.is_ipv6())[attempt / paths.len()];
        let r = io.http(p, target);
        // These IPv6 HTTP endpoints have no firmware/operator validation yet. A reply is
        // observable path evidence; a failure cannot distinguish the service from the path.
        let r = if p.source.is_ipv6() && r == ResultState::Failed {
            ResultState::Unknown
        } else {
            r
        };
        n.paths[k].path_reachable = aggregate(
            [n.paths[k].path_reachable, r]
                .into_iter()
                .skip(if attempt < paths.len() { 1 } else { 0 }),
        );
        if r == ResultState::Passed {
            break;
        }
    }
    for attempt in 0..2 {
        let k = attempt % paths.len();
        let p = &paths[k];
        let Some(ip) = p.dns.get(attempt / paths.len()) else {
            continue;
        };
        let r = io.dns(p, SocketAddr::new(*ip, 53));
        n.paths[k].dns = aggregate(
            [n.paths[k].dns, r]
                .into_iter()
                .skip(if attempt < paths.len() { 1 } else { 0 }),
        );
        if r == ResultState::Passed {
            break;
        }
    }
    n.path_reachable = aggregate(n.paths.iter().map(|p| p.path_reachable));
    n.dns = aggregate(n.paths.iter().map(|p| p.dns));
    if n.path_reachable == ResultState::Unknown && paths.iter().any(|p| p.source.is_ipv6()) {
        n.reason = "ipv6_http_targets_unverified".into();
    }
    n
}

pub struct BoundSockets;

fn socket(path: &Path, udp: bool) -> std::io::Result<Socket> {
    if !valid_interface(&path.interface) {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    let s = Socket::new(
        Domain::for_address(SocketAddr::new(path.source, 0)),
        if udp { Type::DGRAM } else { Type::STREAM },
        Some(if udp { Protocol::UDP } else { Protocol::TCP }),
    )?;
    #[cfg(target_os = "linux")]
    s.bind_device(Some(path.interface.as_bytes()))?;
    #[cfg(not(target_os = "linux"))]
    return Err(std::io::ErrorKind::Unsupported.into());
    s.bind(&SocketAddr::new(path.source, 0).into())?;
    Ok(s)
}

impl Transport for BoundSockets {
    fn http(&self, path: &Path, addr: SocketAddr) -> ResultState {
        let Ok(s) = socket(path, false) else {
            return ResultState::Unknown;
        };
        let started = Instant::now();
        if s.connect_timeout(&addr.into(), HTTP_TIMEOUT).is_err() {
            return ResultState::Failed;
        }
        let mut s: TcpStream = s.into();
        let request = format!(
            "GET / HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            addr
        );
        let remaining = || {
            HTTP_TIMEOUT
                .checked_sub(started.elapsed())
                .filter(|d| !d.is_zero())
        };
        let Some(left) = remaining() else {
            return ResultState::Failed;
        };
        if s.set_write_timeout(Some(left)).is_err() || s.write_all(request.as_bytes()).is_err() {
            return ResultState::Failed;
        }
        let mut head = [0u8; 12];
        let mut used = 0;
        while used < head.len() {
            let Some(left) = remaining() else {
                return ResultState::Failed;
            };
            if s.set_read_timeout(Some(left)).is_err() {
                return ResultState::Unknown;
            }
            match s.read(&mut head[used..]) {
                Ok(0) | Err(_) => return ResultState::Failed,
                Ok(n) => used += n,
            }
        }
        if super::any_status(&head) {
            ResultState::Passed
        } else {
            ResultState::Failed
        }
    }

    fn dns(&self, path: &Path, addr: SocketAddr) -> ResultState {
        let Ok(s) = socket(path, true) else {
            return ResultState::Unknown;
        };
        let sock: UdpSocket = s.into();
        if sock.connect(addr).is_err() {
            return ResultState::Failed;
        }
        let id = (super::now_unix() as u16) ^ 0x5a5a;
        let mut q = crate::dns_query(id, "www.qq.com");
        if path.source.is_ipv6() {
            let k = q.len() - 3;
            q[k] = 28;
        }
        let start = Instant::now();
        if sock.set_write_timeout(Some(DNS_TIMEOUT)).is_err() || sock.send(&q).is_err() {
            return ResultState::Failed;
        }
        let mut buf = [0u8; 512];
        loop {
            let Some(left) = DNS_TIMEOUT
                .checked_sub(start.elapsed())
                .filter(|d| !d.is_zero())
            else {
                return ResultState::Failed;
            };
            if sock.set_read_timeout(Some(left)).is_err() {
                return ResultState::Unknown;
            }
            match sock.recv(&mut buf) {
                Ok(n) => {
                    if let Some(r) = dns_answer(&buf[..n], &q) {
                        return r;
                    }
                }
                Err(_) => return ResultState::Failed,
            }
        }
    }
}

/// Decode a bounded DNS name, including compression, and return the next wire offset.
fn name(b: &[u8], mut at: usize) -> Option<(Vec<u8>, usize)> {
    let mut out = Vec::new();
    let mut next = None;
    for _ in 0..128 {
        let c = *b.get(at)?;
        if c & 0xc0 == 0xc0 {
            let to = (((c & 0x3f) as usize) << 8) | *b.get(at + 1)? as usize;
            if to < 12 || to >= at {
                return None;
            }
            next.get_or_insert(at + 2);
            at = to;
        } else {
            at += 1;
            if c == 0 {
                return Some((out, next.unwrap_or(at)));
            }
            if c > 63 {
                return None;
            }
            let label = b.get(at..at + c as usize)?;
            out.push(c);
            out.extend(label.iter().map(u8::to_ascii_lowercase));
            if out.len() > 254 {
                return None;
            }
            at += c as usize;
        }
    }
    None
}

/// Validate question + an address for that name (possibly through CNAME), not just QR/id.
pub(crate) fn dns_answer(b: &[u8], q: &[u8]) -> Option<ResultState> {
    if b.len() < 12 || b[..2] != q[..2] || b[2] & 0xf8 != 0x80 || b[2] & 2 != 0 || b[4..6] != [0, 1]
    {
        return None;
    }
    let (asked, end) = name(q, 12)?;
    let (echo, mut offset) = name(b, 12)?;
    if asked != echo || b.get(offset..offset + 4)? != q.get(end..end + 4)? {
        return None;
    }
    offset += 4;
    if b[3] & 15 != 0 {
        return Some(ResultState::Failed);
    }
    let count = u16::from_be_bytes([b[6], b[7]]);
    if count > 32 {
        return None;
    }
    let qtype = q.get(end..end + 2)?;
    let mut addresses = Vec::new();
    let mut aliases = Vec::new();
    for _ in 0..count {
        let (owner, next) = name(b, offset)?;
        offset = next;
        let h = b.get(offset..offset + 10)?;
        let len = u16::from_be_bytes([h[8], h[9]]) as usize;
        offset += 10;
        b.get(offset..offset + len)?;
        if h[2..4] == [0, 1] {
            if &h[..2] == qtype
                && ((qtype == [0, 1] && len == 4) || (qtype == [0, 28] && len == 16))
            {
                addresses.push(owner);
            } else if h[..2] == [0, 5] {
                let (alias, next) = name(b, offset)?;
                if next != offset + len {
                    return None;
                }
                aliases.push((owner, alias));
            }
        }
        offset += len;
    }
    let mut current = asked;
    for _ in 0..=aliases.len() {
        if addresses.contains(&current) {
            return Some(ResultState::Passed);
        }
        let Some((_, to)) = aliases.iter().find(|(from, _)| *from == current) else {
            return Some(ResultState::Unknown);
        };
        current = to.clone();
    }
    Some(ResultState::Unknown)
}

#[cfg(test)]
mod tests;
