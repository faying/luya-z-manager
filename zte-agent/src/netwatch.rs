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
//!
//! Every sample that cannot read datad — request failed, body not JSON, or
//! no `net.wan_status` in it — adds one to an in-process error count. Each
//! change is written as one decimal line to [`ERRORS_FILE`] (temp file +
//! rename; `0` at startup). The count only grows; an agent restart starts it
//! at 0 again, so readers (touch-ui `datad-trial.sh`) treat a smaller number
//! as a restart. Logged on the first error and every [`LOG_EVERY`] after.

// Nothing in this build reads the conclusions yet; the sampler runs anyway.
#![allow(dead_code)]
use std::fs;
use std::io::{self, Write};
use std::net::{SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
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
/// Error count for readers outside the agent; format is fixed (see module comment).
pub const ERRORS_FILE: &str = "/tmp/netwatch.errors";
/// After the first error, log one line per this many (5 s samples ⇒ ~5 min).
pub const LOG_EVERY: u64 = 60;

/// Why one sample could not read datad.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadError {
    /// Connect/timeout/HTTP error status.
    Request,
    /// Body was not JSON.
    Json,
    /// JSON without a string `net.wan_status`.
    NoWanStatus,
}

impl ReadError {
    fn as_str(self) -> &'static str {
        match self {
            ReadError::Request => "request failed",
            ReadError::Json => "bad JSON",
            ReadError::NoWanStatus => "no net.wan_status",
        }
    }
}

/// The error (if any) of one fetch of datad's `/state`. The `wan_status`
/// lookup is the same one [`sample`] uses.
pub fn classify(fetched: &Result<Value, ReadError>) -> Option<ReadError> {
    match fetched {
        Err(e) => Some(*e),
        Ok(v) => {
            let net = v.get("net").unwrap_or(v);
            match net.get("wan_status").and_then(Value::as_str) {
                Some(_) => None,
                None => Some(ReadError::NoWanStatus),
            }
        }
    }
}

/// In-process count of failed datad reads, mirrored to a file.
pub struct ErrorCounter {
    path: PathBuf,
    count: u64,
}

impl ErrorCounter {
    /// Starts at 0 and writes that out, replacing whatever the last run left.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let c = ErrorCounter { path: path.into(), count: 0 };
        if let Err(e) = write_count(&c.path, 0) {
            eprintln!("netwatch: cannot write {}: {e}", c.path.display());
        }
        c
    }

    #[cfg(test)]
    pub fn count(&self) -> u64 {
        self.count
    }

    /// Account for one sample. Returns true when the count changed.
    pub fn record(&mut self, err: Option<ReadError>) -> bool {
        let Some(e) = err else { return false };
        self.count += 1;
        let written = write_count(&self.path, self.count);
        if self.count == 1 || self.count % LOG_EVERY == 0 {
            eprintln!("netwatch: datad /state unreadable ({}), {} error(s) so far", e.as_str(), self.count);
            if let Err(w) = written {
                eprintln!("netwatch: cannot write {}: {w}", self.path.display());
            }
        }
        true
    }
}

/// `"<n>\n"` via a temp file in the same directory + rename, so a reader never
/// sees a half-written number.
fn write_count(path: &Path, n: u64) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        let mut f = fs::File::create(&tmp)?;
        writeln!(f, "{n}")?;
    }
    fs::rename(&tmp, path)
}

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
            // Overridable for the local fake run.
            let datad = std::env::var("ZTE_AGENT_DATAD_STATE").unwrap_or_else(|_| DATAD_STATE.to_string());
            let errors_file =
                std::env::var("ZTE_AGENT_NETWATCH_ERRORS").unwrap_or_else(|_| ERRORS_FILE.to_string());
            let mut errors = ErrorCounter::new(errors_file);
            loop {
                let fetched: Result<Value, ReadError> = match agent.get(&datad).call() {
                    Err(_) => Err(ReadError::Request),
                    Ok(mut r) => r.body_mut().read_json::<Value>().map_err(|_| ReadError::Json),
                };
                errors.record(classify(&fetched));
                let state = fetched.ok();
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

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("u60-netwatch-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn classify_state_reads() {
        assert_eq!(classify(&Ok(json!({"net": {"wan_status": "disconnected"}}))), None);
        assert_eq!(classify(&Ok(json!({"wan_status": "ipv4_connected"}))), None);
        assert_eq!(classify(&Err(ReadError::Request)), Some(ReadError::Request));
        assert_eq!(classify(&Err(ReadError::Json)), Some(ReadError::Json));
        assert_eq!(classify(&Ok(json!({"net": {"wan_dns": "1.1.1.1"}}))), Some(ReadError::NoWanStatus));
        assert_eq!(classify(&Ok(json!({"net": {"wan_status": null}}))), Some(ReadError::NoWanStatus));
    }

    #[test]
    fn error_counter_success_does_not_count() {
        let d = tmpdir("ok");
        let p = d.join("netwatch.errors");
        let mut c = ErrorCounter::new(&p);
        assert_eq!(fs::read_to_string(&p).unwrap(), "0\n");
        let ok = Ok(json!({"net": {"wan_status": "ipv4_ipv6_connected"}}));
        for _ in 0..3 {
            assert!(!c.record(classify(&ok)));
        }
        assert_eq!(c.count(), 0);
        assert_eq!(fs::read_to_string(&p).unwrap(), "0\n");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn error_counter_counts_each_kind_once() {
        let d = tmpdir("kinds");
        let p = d.join("netwatch.errors");
        fs::write(&p, "41\n").unwrap(); // left by an earlier run: reset to 0
        let mut c = ErrorCounter::new(&p);
        assert_eq!(fs::read_to_string(&p).unwrap(), "0\n");
        let cases = [
            Err(ReadError::Request),
            Err(ReadError::Json),
            Ok(json!({"net": {"wan_dns": "1.1.1.1"}})),
        ];
        for (i, f) in cases.iter().enumerate() {
            assert!(c.record(classify(f)));
            assert_eq!(c.count(), i as u64 + 1);
            assert_eq!(fs::read_to_string(&p).unwrap(), format!("{}\n", i + 1));
        }
        assert!(!c.record(None));
        assert_eq!(fs::read_to_string(&p).unwrap(), "3\n");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn error_counter_file_is_replaced_atomically() {
        use std::io::Read;
        let d = tmpdir("atomic");
        let p = d.join("netwatch.errors");
        let mut c = ErrorCounter::new(&p);
        c.record(Some(ReadError::Request));
        // A reader holding the old file keeps seeing a whole old number: the
        // new one arrives as a different file (rename), not an in-place rewrite.
        let mut old = fs::File::open(&p).unwrap();
        c.record(Some(ReadError::Request));
        let mut seen = String::new();
        old.read_to_string(&mut seen).unwrap();
        assert_eq!(seen, "1\n");
        assert_eq!(fs::read_to_string(&p).unwrap(), "2\n");
        assert!(!d.join("netwatch.errors.tmp").exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn dns_query_shape() {
        let q = dns_query(0x1234, "a.bc");
        assert_eq!(&q[..4], &[0x12, 0x34, 0x01, 0x00]);
        assert_eq!(&q[12..], &[1, b'a', 2, b'b', b'c', 0, 0, 1, 0, 1]);
    }
}
