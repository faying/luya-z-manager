//! Cellular link watcher (minimal version, step 1 of
//! `docs/designs/slow-diagnosis.md`).
//!
//! A background thread samples every 5 s: datad's `/state` (already refreshed
//! by datad whether anyone looks or not, so this adds no modem reads) for the
//! data-call status and the operator DNS, and `/proc/net/dev` for the
//! `rmnet_data0` receive counter. It sends nothing outward.
//!
//! [`cell_alive`] is the one conclusion other modules use ("is the cellular
//! link itself working?"): connected, and either something arrived on
//! `rmnet_data0` in the last 30 s or — only when asked, and only then — one
//! plain DNS query straight to the operator's resolver gets an answer.
//! Device-local packets take the default route out of `rmnet_data0` and the
//! DNS is not hijacked (checked 2026-09-25), so that query is a direct one.

use std::fs;
use std::net::{SocketAddr, UdpSocket};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

const DATAD_STATE: &str = "http://127.0.0.1:9460/state";
const CELL_IF: &str = "rmnet_data0";
const SAMPLE_EVERY: Duration = Duration::from_secs(5);
/// Received something this recently ⇒ the link is alive, no probe needed.
pub const RX_FRESH_SECS: u64 = 30;
const DNS_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    /// Unix time of the last sample; 0 = never sampled.
    pub at: u64,
    /// datad's `wan_status` says a data call is up. `None` = datad unreadable.
    pub connected: Option<bool>,
    pub rx_bytes: Option<u64>,
    /// Last time the receive counter was seen to move.
    pub rx_moved_at: u64,
    /// Operator DNS servers (IPv4), from datad's `wan_dns`.
    pub dns: Vec<String>,
}

static SNAP: Mutex<Snapshot> = Mutex::new(Snapshot {
    at: 0,
    connected: None,
    rx_bytes: None,
    rx_moved_at: 0,
    dns: Vec::new(),
});

pub fn start() {
    thread::Builder::new()
        .name("netwatch".into())
        .spawn(|| {
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(3)))
                .build()
                .into();
            // Overridable for the local fake run, like ZTE_AGENT_MIHOMO_API.
            let datad = std::env::var("ZTE_AGENT_DATAD_STATE").unwrap_or_else(|_| DATAD_STATE.to_string());
            loop {
                let state = agent
                    .get(&datad)
                    .call()
                    .ok()
                    .and_then(|mut r| r.body_mut().read_json::<Value>().ok());
                let rx = fs::read_to_string("/proc/net/dev").ok().and_then(|t| rx_bytes(&t, CELL_IF));
                let now = now_unix();
                let mut s = SNAP.lock().unwrap_or_else(|e| e.into_inner());
                let next = sample(&s, now, state.as_ref(), rx);
                *s = next;
                drop(s);
                thread::sleep(SAMPLE_EVERY);
            }
        })
        .ok();
}

pub fn snapshot() -> Snapshot {
    SNAP.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Is the cellular link itself working? See the module comment. May send one
/// DNS query (≤ 2 s), so call it only when about to act on the answer.
pub fn cell_alive() -> bool {
    let s = snapshot();
    match alive_without_probe(&s, now_unix()) {
        Some(v) => v,
        None => dns_rtt_ms(&s.dns).is_some(),
    }
}

/// Round trip of one DNS query to the operator's resolver, in ms. `None` when
/// no resolver is known or none answered within 2 s.
pub fn direct_dns_ms() -> Option<u32> {
    dns_rtt_ms(&snapshot().dns)
}

/// `Some(answer)` when the samples decide it; `None` = connected but nothing
/// received lately, so only a probe can tell.
pub fn alive_without_probe(s: &Snapshot, now: u64) -> Option<bool> {
    // No sample yet, or datad down: don't block anything on our own blindness.
    if s.at == 0 || s.connected.is_none() {
        return Some(true);
    }
    if s.connected == Some(false) {
        return Some(false);
    }
    if s.rx_moved_at > 0 && now.saturating_sub(s.rx_moved_at) <= RX_FRESH_SECS {
        return Some(true);
    }
    None
}

/// Fold one sample into the previous snapshot.
pub fn sample(prev: &Snapshot, now: u64, state: Option<&Value>, rx: Option<u64>) -> Snapshot {
    let net = state.map(|v| v.get("net").unwrap_or(v));
    let connected = net.and_then(|n| n.get("wan_status")).and_then(Value::as_str).map(wan_connected);
    let dns = net
        .and_then(|n| n.get("wan_dns"))
        .and_then(Value::as_str)
        .map(parse_dns)
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| prev.dns.clone());
    let moved = match (prev.rx_bytes, rx) {
        (Some(a), Some(b)) => b != a,
        // First reading: we cannot tell yet.
        _ => false,
    };
    Snapshot {
        at: now,
        connected,
        rx_bytes: rx.or(prev.rx_bytes),
        rx_moved_at: if moved { now } else { prev.rx_moved_at },
        dns,
    }
}

/// `"ipv4_ipv6_connected"` → true; `"disconnected"`, `"connecting"`, `""` → false.
pub fn wan_connected(s: &str) -> bool {
    s.ends_with("connected") && !s.contains("disconnect")
}

/// datad's `wan_dns` looks like `222.66.251.8' '116.236.159.8` (shell-quoted
/// list): keep the IPv4 addresses.
fn parse_dns(s: &str) -> Vec<String> {
    s.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .filter(|p| p.split('.').count() == 4 && p.split('.').all(|o| o.parse::<u8>().is_ok()))
        .map(str::to_string)
        .collect()
}

/// Receive-bytes column of `iface` in `/proc/net/dev`.
fn rx_bytes(text: &str, iface: &str) -> Option<u64> {
    text.lines().find_map(|l| {
        let (name, rest) = l.split_once(':')?;
        (name.trim() == iface).then(|| rest.split_whitespace().next()?.parse().ok())?
    })
}

fn dns_rtt_ms(servers: &[String]) -> Option<u32> {
    servers.iter().take(2).find_map(|ip| dns_query_ms(ip))
}

/// One `A` query for a fixed name; any well-formed reply with our id counts
/// (even NXDOMAIN: the resolver answered, so the path works).
fn dns_query_ms(ip: &str) -> Option<u32> {
    let addr: SocketAddr = format!("{ip}:53").parse().ok()?;
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.set_read_timeout(Some(DNS_TIMEOUT)).ok()?;
    let id = (now_unix() as u16) ^ 0x5a5a;
    let q = dns_query(id, "www.qq.com");
    let t = Instant::now();
    sock.send_to(&q, addr).ok()?;
    let mut buf = [0u8; 512];
    let deadline = t + DNS_TIMEOUT;
    while Instant::now() < deadline {
        let (n, from) = sock.recv_from(&mut buf).ok()?;
        if from == addr && n >= 12 && buf[0..2] == id.to_be_bytes() && buf[2] & 0x80 != 0 {
            return Some(t.elapsed().as_millis() as u32);
        }
    }
    None
}

fn dns_query(id: u16, name: &str) -> Vec<u8> {
    let mut q = Vec::with_capacity(32);
    q.extend_from_slice(&id.to_be_bytes());
    q.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]); // RD, 1 question
    for label in name.split('.') {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.extend_from_slice(&[0, 0, 1, 0, 1]); // root, A, IN
    q
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const DEV: &str = "Inter-|   Receive\n face |bytes packets\n  rmnet_data0: 181760867   35107    0 0\n    lo: 5 1 0 0\n";

    #[test]
    fn parses_proc_net_dev() {
        assert_eq!(rx_bytes(DEV, "rmnet_data0"), Some(181760867));
        assert_eq!(rx_bytes(DEV, "lo"), Some(5));
        assert_eq!(rx_bytes(DEV, "rmnet_data1"), None);
    }

    #[test]
    fn parses_datad_dns() {
        assert_eq!(parse_dns("222.66.251.8' '116.236.159.8"), vec!["222.66.251.8", "116.236.159.8"]);
        assert_eq!(parse_dns("--"), Vec::<String>::new());
        assert_eq!(parse_dns("1.2.3"), Vec::<String>::new());
    }

    #[test]
    fn wan_status_values() {
        assert!(wan_connected("ipv4_ipv6_connected"));
        assert!(wan_connected("ipv4_connected"));
        assert!(!wan_connected("disconnected"));
        assert!(!wan_connected("connecting"));
        assert!(!wan_connected(""));
    }

    #[test]
    fn rx_movement_and_liveness() {
        let st = json!({"net": {"wan_status": "ipv4_ipv6_connected", "wan_dns": "1.1.1.1"}});
        let s0 = Snapshot::default();
        assert_eq!(alive_without_probe(&s0, 100), Some(true), "no sample yet: don't claim it is down");

        let s1 = sample(&s0, 100, Some(&st), Some(1000));
        assert_eq!(s1.rx_moved_at, 0, "first reading cannot show movement");
        assert_eq!(alive_without_probe(&s1, 100), None);

        let s2 = sample(&s1, 105, Some(&st), Some(2000));
        assert_eq!(s2.rx_moved_at, 105);
        assert_eq!(alive_without_probe(&s2, 135), Some(true));
        assert_eq!(alive_without_probe(&s2, 136), None, "stale after 30 s: needs a probe");

        let s3 = sample(&s2, 140, Some(&st), Some(2000));
        assert_eq!(s3.rx_moved_at, 105, "counter still ⇒ keeps the old time");
        assert_eq!(s3.dns, vec!["1.1.1.1"]);

        let down = json!({"net": {"wan_status": "disconnected", "wan_dns": "--"}});
        let s4 = sample(&s3, 145, Some(&down), Some(3000));
        assert_eq!(alive_without_probe(&s4, 145), Some(false));
        assert_eq!(s4.dns, vec!["1.1.1.1"], "\"--\" keeps the last known resolver");

        let s5 = sample(&s4, 150, None, None);
        assert_eq!(s5.connected, None);
        assert_eq!(alive_without_probe(&s5, 150), Some(true), "datad down: not our call");
    }

    #[test]
    fn dns_query_shape() {
        let q = dns_query(0x1234, "a.bc");
        assert_eq!(&q[..4], &[0x12, 0x34, 0x01, 0x00]);
        assert_eq!(&q[12..], &[1, b'a', 2, b'b', b'c', 0, 0, 1, 0, 1]);
    }
}
