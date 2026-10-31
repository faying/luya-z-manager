//! Front-door guards: which `Host` names are served, how fast passwords may be tried, and which
//! peers may take the reserved connection slots.
//!
//! - Host (DNS rebinding): a page on a name the attacker owns, re-pointed at the device, sends
//!   `Host` and `Origin` that agree with each other, so [`crate::auth::same_origin`] alone lets it
//!   through. Only names nobody outside can own are served: IP literals, single labels
//!   (`localhost`, `u60`), the private suffixes, Tailscale's `*.ts.net`, and any name listed in
//!   `LUYAZ_WEB_HOSTS` (comma list). Anything else: 421.
//! - Login: a wrong password delays the next try from that address, 1 s doubling to 5 min; a
//!   shared bucket bounds wrong tries from all addresses together. A right password clears it.
//! - Slots: loopback and the tailnet may use the last [`RESERVED`] connection slots, so a LAN
//!   client that fills the rest does not lock out Tailscale.
//! - Interfaces: a connection is served only when the device address it reached belongs to one
//!   of the listed interfaces ([`SERVED_IFACES`]; `LUYAZ_WEB_IFACES`). The cellular side
//!   (`rmnet*`) is refused here as well as by the firewall; Tailscale arrives on `tailscale0`
//!   (or on `br-lan`'s address through a subnet route) and is served. The interface list cannot
//!   be read: refused.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// Connection slots only loopback and the tailnet may take.
pub const RESERVED: usize = 8;
/// Open connections one address may hold (a browser opens at most 6 per host).
pub const PER_PEER: usize = 16;

const PRIVATE_SUFFIXES: [&str; 6] = [
    ".ts.net",
    ".lan",
    ".local",
    ".home.arpa",
    ".internal",
    ".localhost",
];

fn extra_hosts() -> &'static [String] {
    static HOSTS: OnceLock<Vec<String>> = OnceLock::new();
    HOSTS.get_or_init(|| {
        std::env::var("LUYAZ_WEB_HOSTS")
            .unwrap_or_default()
            .split(',')
            .map(|h| h.trim().trim_end_matches('.').to_ascii_lowercase())
            .filter(|h| !h.is_empty())
            .collect()
    })
}

/// `Host` header value (with or without port) → served here?
pub fn host_ok(host: &str, extra: &[String]) -> bool {
    let name = if let Some(rest) = host.strip_prefix('[') {
        // [v6]:port
        return rest
            .split_once(']')
            .is_some_and(|(ip, _)| ip.parse::<std::net::Ipv6Addr>().is_ok());
    } else {
        host.rsplit_once(':').map_or(host, |(h, port)| {
            if port.bytes().all(|b| b.is_ascii_digit()) {
                h
            } else {
                host
            }
        })
    };
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    if name.is_empty() {
        return false;
    }
    name.parse::<IpAddr>().is_ok()
        || !name.contains('.')
        || PRIVATE_SUFFIXES.iter().any(|s| name.ends_with(s))
        || extra.contains(&name)
}

/// Outermost layer: refuse a request whose `Host` is a name someone outside could own.
pub async fn host_gate(req: Request, next: Next) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !host_ok(host, extra_hosts()) {
        eprintln!("[guard] refused Host {host:?}");
        return (
            StatusCode::MISDIRECTED_REQUEST,
            "This name is not served here. Open the device by its IP address, or add the name to LUYAZ_WEB_HOSTS.\n",
        )
            .into_response();
    }
    next.run(req).await
}

/// Loopback or the tailnet (`100.64.0.0/10`, `fd7a:115c:a1e0::/48`).
pub fn trusted_peer(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => {
            v4.is_loopback() || (v4.octets()[0] == 100 && v4.octets()[1] & 0xc0 == 64)
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || (v6.segments()[0] == 0xfd7a
                    && v6.segments()[1] == 0x115c
                    && v6.segments()[2] == 0xa1e0)
        }
    }
}

// ---- interfaces ----

/// Interfaces whose addresses are served by default.
pub const SERVED_IFACES: &str = "lo,br-lan,tailscale0";
/// Refusals are logged at most this often (with the count since the last line).
const REFUSE_LOG_EVERY: Duration = Duration::from_secs(60);

/// Which local (device) addresses may be served. `*` in the list: any.
pub struct IfaceGate {
    names: Vec<String>,
    any: bool,
    log: Mutex<(Option<Instant>, u64)>,
}

impl IfaceGate {
    pub fn new(list: &str) -> Self {
        let names: Vec<String> = list
            .split(',')
            .map(|n| n.trim().to_owned())
            .filter(|n| !n.is_empty())
            .collect();
        IfaceGate {
            any: names.iter().any(|n| n == "*"),
            names,
            log: Mutex::new((None, 0)),
        }
    }

    /// The connection reached `local` from `peer`: serve it? Refusals are logged (rate-limited).
    pub fn admit(&self, local: IpAddr, peer: IpAddr) -> bool {
        if self.any || local.to_canonical().is_loopback() {
            return true;
        }
        let ok = match ifaddrs() {
            Some(addrs) => local_ok(local, &addrs, &self.names),
            None => false,
        };
        if !ok {
            let mut g = self.log.lock().unwrap();
            g.1 += 1;
            let now = Instant::now();
            if g.0.is_none_or(|t| now.duration_since(t) >= REFUSE_LOG_EVERY) {
                eprintln!("[guard] refused {peer} -> {local} (not on {}; {} since last line)", self.names.join(","), g.1);
                *g = (Some(now), 0);
            }
        }
        ok
    }
}

/// `local` is an address of one of `names` in `addrs` (`(interface, address)`).
pub fn local_ok(local: IpAddr, addrs: &[(String, IpAddr)], names: &[String]) -> bool {
    let local = local.to_canonical();
    addrs.iter().any(|(n, a)| *a == local && names.contains(n))
}

/// Every interface address (IPv4 and IPv6); None when the list cannot be read.
fn ifaddrs() -> Option<Vec<(String, IpAddr)>> {
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills the list; it is only read below and freed with freeifaddrs.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return None;
    }
    let mut found = Vec::new();
    let mut p = head;
    while !p.is_null() {
        // SAFETY: p is a node from getifaddrs; ifa_addr may be null, checked first.
        let ifa = unsafe { &*p };
        if !ifa.ifa_addr.is_null() {
            // SAFETY: the family says which sockaddr ifa_addr points at.
            let ip = match unsafe { (*ifa.ifa_addr).sa_family } as i32 {
                libc::AF_INET => {
                    let sin = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in) };
                    Some(IpAddr::from(u32::from_be(sin.sin_addr.s_addr).to_be_bytes()))
                }
                libc::AF_INET6 => {
                    let sin6 = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in6) };
                    Some(IpAddr::from(sin6.sin6_addr.s6_addr))
                }
                _ => None,
            };
            if let Some(ip) = ip {
                // SAFETY: ifa_name is NUL-terminated.
                let name = unsafe { std::ffi::CStr::from_ptr(ifa.ifa_name) };
                found.push((name.to_string_lossy().into_owned(), ip));
            }
        }
        p = ifa.ifa_next;
    }
    // SAFETY: head came from getifaddrs.
    unsafe { libc::freeifaddrs(head) };
    Some(found)
}

// ---- login attempts ----

const FIRST_DELAY: Duration = Duration::from_secs(1);
const MAX_DELAY: Duration = Duration::from_secs(300);
/// Wrong tries from all addresses together: this many at once, then one more per [`REFILL`].
const BUCKET: f64 = 30.0;
const REFILL: Duration = Duration::from_secs(5);
/// Addresses remembered at most; the oldest unblocked ones go first.
const MAX_PEERS: usize = 1024;

struct Peer {
    fails: u32,
    until: Instant,
}

pub struct Limiter {
    inner: Mutex<LimiterState>,
}

struct LimiterState {
    peers: HashMap<IpAddr, Peer>,
    tokens: f64,
    at: Instant,
}

impl Default for Limiter {
    fn default() -> Self {
        Self::new()
    }
}

impl Limiter {
    pub fn new() -> Self {
        Limiter {
            inner: Mutex::new(LimiterState {
                peers: HashMap::new(),
                tokens: BUCKET,
                at: Instant::now(),
            }),
        }
    }

    /// Ok = may try now; Err(wait) = refuse for that long, without looking at the password.
    pub fn check(&self, ip: IpAddr, now: Instant) -> Result<(), Duration> {
        let mut s = self.inner.lock().unwrap();
        s.refill(now);
        if let Some(p) = s.peers.get(&ip.to_canonical())
            && p.until > now
        {
            return Err(p.until - now);
        }
        if s.tokens < 1.0 {
            return Err(REFILL.mul_f64(1.0 - s.tokens));
        }
        Ok(())
    }

    pub fn failed(&self, ip: IpAddr, now: Instant) {
        let mut s = self.inner.lock().unwrap();
        s.refill(now);
        s.tokens = (s.tokens - 1.0).max(0.0);
        if s.peers.len() >= MAX_PEERS && !s.peers.contains_key(&ip.to_canonical()) {
            s.peers.retain(|_, p| p.until > now);
            if s.peers.len() >= MAX_PEERS {
                // all blocked: forget the one that frees soonest
                if let Some(k) = s.peers.iter().min_by_key(|(_, p)| p.until).map(|(k, _)| *k) {
                    s.peers.remove(&k);
                }
            }
        }
        let p = s.peers.entry(ip.to_canonical()).or_insert(Peer {
            fails: 0,
            until: now,
        });
        p.fails = p.fails.saturating_add(1);
        let delay = FIRST_DELAY
            .saturating_mul(1u32 << (p.fails - 1).min(16))
            .min(MAX_DELAY);
        p.until = now + delay;
    }

    pub fn succeeded(&self, ip: IpAddr) {
        self.inner.lock().unwrap().peers.remove(&ip.to_canonical());
    }
}

impl LimiterState {
    fn refill(&mut self, now: Instant) {
        let dt = now.saturating_duration_since(self.at).as_secs_f64();
        self.tokens = (self.tokens + dt / REFILL.as_secs_f64()).min(BUCKET);
        self.at = now;
    }
}

pub fn limiter() -> &'static Limiter {
    static L: OnceLock<Limiter> = OnceLock::new();
    L.get_or_init(Limiter::new)
}

// ---- connections per address ----

/// Open (or waiting) connections per address; a guard gives its count back on drop.
#[derive(Default)]
pub struct PeerCount {
    inner: Mutex<HashMap<IpAddr, usize>>,
}

pub struct PeerSlot {
    count: std::sync::Arc<PeerCount>,
    ip: IpAddr,
}

impl PeerCount {
    /// None = that address already holds `max`.
    pub fn take(self: &std::sync::Arc<Self>, ip: IpAddr, max: usize) -> Option<PeerSlot> {
        let ip = ip.to_canonical();
        let mut m = self.inner.lock().unwrap();
        let n = m.entry(ip).or_insert(0);
        if *n >= max {
            return None;
        }
        *n += 1;
        Some(PeerSlot {
            count: self.clone(),
            ip,
        })
    }
}

impl Drop for PeerSlot {
    fn drop(&mut self) {
        let mut m = self.count.inner.lock().unwrap();
        if let Some(n) = m.get_mut(&self.ip) {
            *n -= 1;
            if *n == 0 {
                m.remove(&self.ip);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_listed_interfaces_are_served() {
        let addrs: Vec<(String, IpAddr)> = [
            ("lo", "127.0.0.1"),
            ("br-lan", "192.168.0.1"),
            ("rmnet_data0", "10.66.123.76"),
            ("rmnet_data0", "2408:840d:3630:1e79:a00e:c1c:24fa:cbe1"),
            ("tailscale0", "100.84.210.91"),
            ("tailscale0", "fd7a:115c:a1e0::bf2e:d25c"),
        ]
        .iter()
        .map(|(n, a)| (n.to_string(), a.parse().unwrap()))
        .collect();
        let names: Vec<String> = SERVED_IFACES.split(',').map(String::from).collect();
        let ok = |a: &str| local_ok(a.parse().unwrap(), &addrs, &names);
        assert!(ok("192.168.0.1"));
        assert!(ok("::ffff:192.168.0.1"));
        assert!(ok("100.84.210.91"));
        assert!(ok("fd7a:115c:a1e0::bf2e:d25c"));
        assert!(!ok("10.66.123.76"));
        assert!(!ok("2408:840d:3630:1e79:a00e:c1c:24fa:cbe1"));
        assert!(!ok("192.0.2.1"));
        // unreadable list: nothing but loopback
        assert!(!local_ok("192.168.0.1".parse().unwrap(), &[], &names));
        let gate = IfaceGate::new("br-lan");
        assert!(gate.admit("127.0.0.1".parse().unwrap(), "127.0.0.1".parse().unwrap()));
        assert!(IfaceGate::new("*").admit("192.0.2.1".parse().unwrap(), "192.0.2.9".parse().unwrap()));
        assert!(!gate.admit("192.0.2.1".parse().unwrap(), "192.0.2.9".parse().unwrap()));
    }

    #[test]
    fn hosts_nobody_outside_can_own() {
        let extra = vec!["router.example.com".to_string()];
        for ok in [
            "192.168.0.1:9090",
            "192.168.0.1",
            "[fd7a:115c:a1e0::1]:9090",
            "localhost:9090",
            "u60",
            "u60.tail1234.ts.net",
            "u60.lan:9090",
            "U60.LOCAL",
            "box.home.arpa",
            "router.example.com:9090",
            "u60.tail1234.ts.net.",
        ] {
            assert!(host_ok(ok, &extra), "{ok}");
        }
        for bad in [
            "evil.com",
            "evil.com:9090",
            "192.168.0.1.evil.com",
            "ts.net.evil.com",
            "",
            ":9090",
            "[evil]:1",
            "lan.evil.io",
        ] {
            assert!(!host_ok(bad, &extra), "{bad}");
        }
    }

    #[test]
    fn trusted_peers_are_loopback_and_tailnet() {
        for ip in [
            "127.0.0.1",
            "::1",
            "100.64.0.1",
            "100.127.255.254",
            "fd7a:115c:a1e0::5",
            "::ffff:100.100.1.1",
        ] {
            assert!(trusted_peer(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "192.168.0.2",
            "100.128.0.1",
            "100.63.0.1",
            "192.168.0.2",
            "fd7a:115c:a1e1::1",
        ] {
            assert!(!trusted_peer(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn wrong_passwords_back_off_per_address_and_a_right_one_clears() {
        let l = Limiter::new();
        let (a, b): (IpAddr, IpAddr) = ("192.168.0.2".parse().unwrap(), "192.168.0.3".parse().unwrap());
        let t = Instant::now();
        assert!(l.check(a, t).is_ok());
        l.failed(a, t);
        assert_eq!(l.check(a, t), Err(Duration::from_secs(1)));
        assert!(l.check(b, t).is_ok(), "another address is not held up");
        let t = t + Duration::from_secs(1);
        assert!(l.check(a, t).is_ok());
        l.failed(a, t);
        assert_eq!(l.check(a, t), Err(Duration::from_secs(2)));
        let mut t = t;
        for _ in 0..20 {
            t += MAX_DELAY;
            l.failed(a, t);
        }
        assert_eq!(l.check(a, t), Err(MAX_DELAY), "capped at 5 minutes");
        l.succeeded(a);
        assert!(l.check(a, t).is_ok());
        // v4-mapped is the same address
        l.failed("::ffff:192.168.0.3".parse().unwrap(), t);
        assert!(l.check(b, t).is_err());
    }

    #[test]
    fn the_shared_bucket_bounds_many_addresses() {
        let l = Limiter::new();
        let t = Instant::now();
        for i in 0..30u8 {
            l.failed(IpAddr::from([10, 1, 0, i]), t);
        }
        let fresh = IpAddr::from([10, 2, 0, 1]);
        assert!(l.check(fresh, t).is_err(), "bucket empty");
        assert!(
            l.check(fresh, t + REFILL).is_ok(),
            "one back after a refill"
        );
    }

    #[test]
    fn peer_slots_are_counted_and_given_back() {
        let c = std::sync::Arc::new(PeerCount::default());
        let ip: IpAddr = "192.168.0.2".parse().unwrap();
        let a = c.take(ip, 2).unwrap();
        let _b = c.take(ip, 2).unwrap();
        assert!(c.take(ip, 2).is_none());
        assert!(c.take("192.168.0.3".parse().unwrap(), 2).is_some());
        drop(a);
        assert!(c.take(ip, 2).is_some());
    }
}
