//! O2 bound HTTPS (decision-execution-loop.md §4.7): verified TLS to several organisations over
//! the cellular interface and source address, with each `Date` header kept for the clock check.
//! The rules are in `luyaz_api::execution::https`; this is only the transport. Nothing calls it
//! on a timer: it is for a recheck after an approved change and for `<core> o2-check`.

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use luyaz_api::execution::https::{Attempt, Sample, Verdict, parse_http_date};
use luyaz_netwatch::finding::path::{Path, Resolved, resolve};
use socket2::{Domain, Protocol, Socket, Type};

/// Per target at most, connect + handshake + headers.
const TIMEOUT: Duration = Duration::from_secs(5);
/// One lookup at most (two operator servers share it).
const DNS_BUDGET: Duration = Duration::from_secs(3);
/// The whole run, DNS included (§2: a recheck after one approval stays within 20 s).
pub const BUDGET: Duration = Duration::from_secs(20);
/// Response headers kept at most.
const HEAD_MAX: usize = 8 * 1024;

/// Reachable from mainland China and abroad; one organisation each. Three, not §2's two: a CDN
/// can answer with a cached `Date` (Apple 76 s old on the device 10-20), and the clock rule
/// needs two that agree.
pub const TARGETS: [(&str, &str); 3] = [
    ("www.apple.com", "apple"),
    ("www.qq.com", "tencent"),
    ("www.baidu.com", "baidu"),
];

fn config() -> Arc<rustls::ClientConfig> {
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    Arc::new(
        rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("ring supports the default versions")
        .with_root_certificates(roots)
        .with_no_client_auth(),
    )
}

/// A TCP connection out of `iface` from `source`. Err(None): could not bind (says nothing).
fn bound(
    iface: &str,
    source: IpAddr,
    to: SocketAddr,
    timeout: Duration,
) -> Result<TcpStream, Option<std::io::Error>> {
    let s = Socket::new(Domain::for_address(to), Type::STREAM, Some(Protocol::TCP))
        .map_err(|_| None)?;
    #[cfg(target_os = "linux")]
    s.bind_device(Some(iface.as_bytes())).map_err(|_| None)?;
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (iface, timeout);
        return Err(None);
    }
    #[allow(unreachable_code)]
    {
        s.bind(&SocketAddr::new(source, 0).into())
            .map_err(|_| None)?;
        s.connect_timeout(&to.into(), timeout).map_err(Some)?;
        Ok(s.into())
    }
}

/// Only a public unicast address says anything about the cellular exit; a fake-ip
/// (198.18.0.0/15, unreachable in the main table), private, loopback or link-local one doesn't.
pub fn global(a: IpAddr) -> bool {
    match a {
        IpAddr::V4(v) => {
            let o = v.octets();
            !(v.is_private()
                || v.is_loopback()
                || v.is_link_local()
                || v.is_unspecified()
                || v.is_broadcast()
                || v.is_multicast()
                || o[0] == 0
                || o[0] >= 240
                || (o[0] == 100 && (64..128).contains(&o[1]))
                || (o[0] == 198 && (o[1] & 0xfe) == 18)
                || (o[0] == 192 && o[1] == 0 && o[2] == 2)
                || (o[0] == 198 && o[1] == 51 && o[2] == 100)
                || (o[0] == 203 && o[1] == 0 && o[2] == 113)
                || (o[0] == 192 && o[1] == 88 && o[2] == 99))
        }
        IpAddr::V6(v) => {
            let s = v.segments();
            // global unicast 2000::/3, minus documentation 2001:db8::/32 and 6to4 2002::/16
            (s[0] & 0xe000) == 0x2000 && !(s[0] == 0x2001 && s[1] == 0x0db8) && s[0] != 0x2002
        }
    }
}

/// One target over the bound path: the name through the operator DNS on that path, then the
/// first public address of the source's family; never the system resolver. Everything,
/// DNS included, ends within `budget`. Only a refusal, a reset or a timeout that had the full
/// [`TIMEOUT`] counts as `Unreachable`; a local error or a wait cut short by the budget says
/// nothing (`NoName`).
pub fn attempt(path: &Path, host: &str, budget: Duration) -> Attempt {
    let began = Instant::now();
    let to = match resolve(path, host, budget.min(DNS_BUDGET)) {
        Resolved::Addrs(a) => match a.into_iter().find(|a| global(*a)) {
            Some(a) => SocketAddr::new(a, 443),
            None => return Attempt::NoName,
        },
        Resolved::Failed => return Attempt::DnsFailed,
        Resolved::Unknown => return Attempt::NoName,
    };
    let Some(rest) = budget.checked_sub(began.elapsed()).filter(|d| !d.is_zero()) else {
        return Attempt::NoName;
    };
    let timeout = rest.min(TIMEOUT);
    let full = timeout >= TIMEOUT;
    let (iface, source) = (path.interface.as_str(), path.source);
    let started = Instant::now();
    let tcp = match bound(iface, source, to, timeout) {
        Ok(t) => t,
        Err(Some(e)) => return classify(&e, full),
        Err(None) => return Attempt::NoBind,
    };
    let left = || {
        timeout
            .checked_sub(started.elapsed())
            .filter(|d| !d.is_zero())
    };
    let Ok(name) = rustls::pki_types::ServerName::try_from(host.to_owned()) else {
        return Attempt::NoName;
    };
    let Ok(conn) = rustls::ClientConnection::new(config(), name) else {
        return Attempt::NoName;
    };
    let mut tls = rustls::StreamOwned::new(conn, tcp);
    // every read and write gets only what is left of this target's time
    let arm = |s: &TcpStream| {
        let l = left()?;
        (s.set_read_timeout(Some(l)).is_ok() && s.set_write_timeout(Some(l)).is_ok()).then_some(())
    };
    if arm(&tls.sock).is_none() {
        return if full {
            Attempt::Unreachable
        } else {
            Attempt::NoName
        };
    }
    let req = format!("HEAD / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    if let Err(e) = tls.write_all(req.as_bytes()) {
        return classify(&e, full);
    }
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < HEAD_MAX {
        if arm(&tls.sock).is_none() {
            break;
        }
        match tls.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => head.extend_from_slice(&buf[..n]),
            Err(e) if head.is_empty() => return classify(&e, full),
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&head);
    if !text.starts_with("HTTP/1.") {
        // verified TLS but no status line within the time: can't tell
        return Attempt::NoName;
    }
    let date = text.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case("date")
            .then(|| parse_http_date(v))?
    });
    Attempt::Verified { date }
}

/// A rejected certificate; the far end refusing, resetting or not answering in the full time;
/// anything else (local errors, waits cut short) says nothing.
fn classify(e: &std::io::Error, full: bool) -> Attempt {
    use std::io::ErrorKind::*;
    if let Some(rustls::Error::InvalidCertificate(_)) =
        e.get_ref().and_then(|i| i.downcast_ref::<rustls::Error>())
    {
        return Attempt::TlsRejected;
    }
    match e.kind() {
        ConnectionRefused | ConnectionReset | ConnectionAborted | UnexpectedEof => {
            Attempt::Unreachable
        }
        TimedOut | WouldBlock if full => Attempt::Unreachable,
        _ => Attempt::NoName,
    }
}

/// Every target in turn, all within [`BUDGET`]; a target with no time left is `NoName`.
pub fn run(path: &Path) -> Vec<Sample> {
    run_within(path, BUDGET)
}

/// [`run`] within `budget` (a deep diagnosis has its own cap).
pub fn run_within(path: &Path, budget: Duration) -> Vec<Sample> {
    let start = Instant::now();
    TARGETS
        .iter()
        .map(|(host, org)| Sample {
            org,
            attempt: match budget.checked_sub(start.elapsed()).filter(|d| !d.is_zero()) {
                Some(left) => attempt(path, host, left),
                None => Attempt::NoName,
            },
        })
        .collect()
}

/// The operator's DNS servers as netifd wrote them (the main `/etc/resolv.conf` can be another).
fn operator_dns() -> Vec<IpAddr> {
    std::fs::read_to_string("/tmp/resolv.conf.d/resolv.conf.auto")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.strip_prefix("nameserver "))
        .filter_map(|a| a.trim().parse().ok())
        .collect()
}

fn uci(key: &str) -> Option<String> {
    let o = std::process::Command::new("uci")
        .args(["-q", "get", key])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&o.stdout).trim().to_owned();
    (o.status.success() && !s.is_empty()).then_some(s)
}

/// The clock rule on one run's samples: (verdict, device UTC, vendor synced).
fn judge(samples: &[Sample]) -> (Verdict, Option<i64>, Option<bool>) {
    // the device clock is local time labelled UTC (CLAUDE.md): back to real UTC
    let offset = luyaz_api::clock::utc_offset(
        uci("zwrt_zte_sntp.settings.time_from_utc").as_deref(),
        uci("zwrt_zte_sntp.settings.timezone").as_deref(),
    );
    let device_utc = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64 - offset);
    let synced = uci("zwrt_zte_sntp.status.syn_done").map(|s| s == "1");
    (
        luyaz_api::execution::https::judge(samples, device_utc, synced),
        device_utc,
        synced,
    )
}

/// The first global address on `iface` (IPv4 first), the source a recheck binds to.
pub fn cellular_source(iface: &str) -> Option<IpAddr> {
    let out = std::process::Command::new("ip")
        .args(["-o", "addr", "show", "dev", iface])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let text = String::from_utf8_lossy(&out.stdout);
    let addrs: Vec<IpAddr> = text
        .lines()
        .filter_map(|l| {
            let mut it = l
                .split_whitespace()
                .skip_while(|w| *w != "inet" && *w != "inet6");
            it.next()?;
            it.next()?.split('/').next()?.parse().ok()
        })
        .filter(|a| global(*a))
        .collect();
    addrs
        .iter()
        .find(|a| a.is_ipv4())
        .or(addrs.first())
        .copied()
}

/// One bound run over the cellular interface for a recheck after an approved change
/// (`api/approve.rs`). None = no global source address to bind to. Blocking, ≤ [`BUDGET`].
pub fn check(iface: &str) -> Option<(Vec<Sample>, Verdict)> {
    let path = Path {
        interface: iface.to_owned(),
        source: cellular_source(iface)?,
        source_kind: "recheck",
        dns: operator_dns(),
    };
    let samples = run(&path);
    let (v, _, _) = judge(&samples);
    Some((samples, v))
}

/// One run on a path the caller already has (a deep diagnosis: `finding::path::paths`).
pub fn check_path(path: &Path, budget: Duration) -> (Vec<Sample>, Verdict) {
    let samples = run_within(path, budget);
    let (v, _, _) = judge(&samples);
    (samples, v)
}

/// `<core> o2-check <iface> <source-ip>`: one read-only run, JSON on stdout. Exit 0 = ran.
pub fn main(args: &[String]) -> i32 {
    let (Some(iface), Some(Ok(source))) = (args.first(), args.get(1).map(|s| s.parse::<IpAddr>()))
    else {
        eprintln!("usage: o2-check <iface> <source-ip> [dns-server…]");
        return 2;
    };
    let mut dns: Vec<IpAddr> = args[2..].iter().filter_map(|a| a.parse().ok()).collect();
    if dns.is_empty() {
        dns = operator_dns();
    }
    let path = Path {
        interface: iface.clone(),
        source,
        source_kind: "o2-check",
        dns,
    };
    let started = Instant::now();
    let samples = run(&path);
    let took_ms = started.elapsed().as_millis() as u64;
    let (v, device_utc, synced) = judge(&samples);
    let attempts: Vec<_> = samples
        .iter()
        .map(|s| serde_json::json!({"org": s.org, "attempt": format!("{:?}", s.attempt)}))
        .collect();
    println!(
        "{}",
        serde_json::json!({"verdict": v, "attempts": attempts, "took_ms": took_ms, "device_utc": device_utc, "vendor_synced": synced})
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_public_unicast_counts() {
        for a in [
            "198.18.0.5",
            "198.19.255.1",
            "192.168.0.1",
            "100.84.210.91",
            "127.0.0.1",
            "169.254.30.1",
            "192.0.2.1",
            "fd7a:115c:a1e0::1",
            "fe80::1",
            "::1",
            "2001:db8::1",
            "2002:c000:0204::1",
            "198.51.100.7",
            "203.0.113.7",
        ] {
            assert!(!global(a.parse().unwrap()), "{a}");
        }
        for a in ["17.253.144.10", "198.20.0.1", "240e:96c:6400:601:3::1d"] {
            assert!(global(a.parse().unwrap()), "{a}");
        }
    }

    #[test]
    fn no_dns_server_is_unknown_and_the_run_keeps_its_budget() {
        let path = Path {
            interface: "lo".into(),
            source: "127.0.0.1".parse().unwrap(),
            source_kind: "test",
            dns: vec![],
        };
        let t = Instant::now();
        let s = run(&path);
        assert!(s.iter().all(|s| s.attempt == Attempt::NoName));
        assert!(t.elapsed() < BUDGET);
    }

    #[test]
    fn three_targets_three_organisations() {
        let mut orgs: Vec<_> = TARGETS.iter().map(|t| t.1).collect();
        orgs.dedup();
        assert_eq!(orgs.len(), TARGETS.len());
        assert!(TARGETS.len() > luyaz_api::execution::https::MIN_ORGS);
    }
}
