//! The cellular link watcher's judgements and its signal history (`docs/designs/slow-diagnosis.md`
//! step 1), run by the core (u60-platform.md Phase 4 step 9b). Moved from zte-agent's
//! `netwatch.rs` so the two cannot drift: what one sample says ([`sample`], [`alive_without_probe`]),
//! where core puts the operator's resolvers ([`operator_dns`]), the one-packet DNS probe, the count
//! of failed core reads ([`ErrorCounter`]), and the minute records and events of
//! `/data/signal-log` ([`record`]). Who samples, and how often, stays with the caller.
//!
//! The receive counter is the modem's own `traffic.rx_bytes` (the core's copy of `real_rx_bytes`), not
//! `rmnet_data0`: client traffic goes through IPA offload and mostly never shows there (ER1,
//! checked on the device 2026-10-02).
//!
//! [`ErrorCounter`] writes its count as one decimal line (temp file + rename; `0` at startup). The
//! count only grows; a restart starts it at 0 again, so readers (touch-ui `core-trial.sh`) treat a
//! smaller number as a restart. One writer only.

use std::io::{self, Write};
use std::net::{SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use luyaz_api::clock::BootTime;
use serde_json::Value;

pub mod diagnose;
pub mod finding;
mod passive;
pub mod record;
pub use passive::{PassiveLink, Snapshot, alive_without_probe, sample};
use passive::fresh;

/// Received something this recently ⇒ the link is alive, no probe needed.
pub const RX_FRESH_SECS: u64 = 30;
const DNS_TIMEOUT: Duration = Duration::from_secs(2);
/// Error count for readers outside the agent; format is fixed (see module comment).
pub const ERRORS_FILE: &str = "/tmp/netwatch.errors";
/// After the first error, log one line per this many (5 s samples ⇒ ~5 min).
pub const LOG_EVERY: u64 = 60;

/// Why one sample could not read core.
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

/// The error (if any) of one fetch of the core's `/state`. The `wan_status`
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

/// In-process count of failed core reads, mirrored to a file.
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

    pub fn count(&self) -> u64 {
        self.count
    }

    /// Account for one sample. Returns true when the count changed.
    pub fn record(&mut self, err: Option<ReadError>) -> bool {
        let Some(e) = err else { return false };
        self.count += 1;
        let written = write_count(&self.path, self.count);
        if self.count == 1 || self.count.is_multiple_of(LOG_EVERY) {
            eprintln!("netwatch: core /state unreadable ({}), {} error(s) so far", e.as_str(), self.count);
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
    std::fs::File::create(&tmp)?.write_all(format!("{n}\n").as_bytes())?;
    std::fs::rename(&tmp, path)
}

/// `"ipv4_ipv6_connected"` → true; `"disconnected"`, `"connecting"`, `""` → false.
pub fn wan_connected(s: &str) -> bool {
    s.ends_with("connected") && !s.contains("disconnect")
}

/// The operator's IPv4 resolvers from the core's `/state`. On the device (checked
/// 2026-10-02) `wan_dns` is under `uci_device_info`, not `net` — reading only
/// `net.wan_dns` found nothing, so the DNS probe never ran and manual-first saw
/// "direct is slow too" every time. Tried in order: `net.wan_dns`,
/// `uci_device_info.wan_dns`, `interfaces.wan4.dns` (a list).
pub fn operator_dns(state: &Value) -> Vec<String> {
    let quoted = |v: Option<&Value>| v.and_then(Value::as_str).map(parse_dns).unwrap_or_default();
    let net = state.get("net").unwrap_or(state);
    for d in [quoted(net.get("wan_dns")), quoted(state.get("uci_device_info").and_then(|u| u.get("wan_dns")))] {
        if !d.is_empty() {
            return d;
        }
    }
    let list = state.get("interfaces").and_then(|i| i.get("wan4")).and_then(|w| w.get("dns")).and_then(Value::as_array);
    list.map(|a| a.iter().filter_map(Value::as_str).flat_map(parse_dns).collect()).unwrap_or_default()
}

/// the core's `wan_dns` looks like `222.66.251.8' '116.236.159.8` (shell-quoted
/// list): keep the IPv4 addresses.
pub fn parse_dns(s: &str) -> Vec<String> {
    s.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .filter(|p| p.split('.').count() == 4 && p.split('.').all(|o| o.parse::<u8>().is_ok()))
        .map(str::to_string)
        .collect()
}

/// A `GET /v2/state` reply as the old `/state` shape, for the reads that run
/// when the caller is not subscribed (the agent's deep diagnosis, `/api/cell/extra`):
/// `signal` → `net`, `live.traffic` → `traffic`, and `qos`, `sim`,
/// `uci_device_info`, `interfaces` as they are. A stale block is left out,
/// the way the old `/state` renders a failed read (STATE_V2.md V2-29), so
/// a stale signal block reads as "no wan_status" ([`classify`]).
pub fn legacy_from_v2(v2: &Value) -> Value {
    let fresh = |name: &str| {
        v2.get("blocks")?
            .get(name)
            .filter(|b| b.get("stale") == Some(&Value::Bool(false)))?
            .get("data")
            .filter(|d| d.is_object())
            .cloned()
    };
    let mut m = serde_json::Map::new();
    if let Some(v) = fresh("signal") {
        m.insert("net".into(), v);
    }
    if let Some(t) = fresh("live").and_then(|l| l.get("traffic").cloned()) {
        m.insert("traffic".into(), t);
    }
    for name in ["qos", "sim", "uci_device_info", "interfaces"] {
        if let Some(v) = fresh(name) {
            m.insert(name.into(), v);
        }
    }
    Value::Object(m)
}

/// Bytes the modem has received this data session (`traffic.rx_bytes` in
/// the core's `/state`; see the module comment for why not `rmnet_data0`).
pub fn cell_rx_bytes(state: Option<&Value>) -> Option<u64> {
    state?.get("traffic")?.get("rx_bytes")?.as_u64()
}

/// Round trip of one DNS query to the first of the first two resolvers that answers, in ms. `None`
/// when none is known or none answered within 2 s.
pub fn dns_rtt_ms(servers: &[String]) -> Option<u32> {
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

pub fn dns_query(id: u16, name: &str) -> Vec<u8> {
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

pub fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// the core's home verdict older than this = core stopped answering; say nothing rather than a
/// stale code.
pub const VERDICT_FRESH: u64 = 30;

/// One watcher, fed every 5 s: the latest [`Snapshot`], the core's home verdict, the signal log while
/// this process owns it, and the core error count where asked. `/data/signal-log` has one writer:
/// the agent until netwatch is handed to u60d, then u60d (u60-platform.md step 9b); the other
/// side's watcher samples without writing it. [`ERRORS_FILE`] stays the agent's (its own reads).
pub struct Watch {
    snap: Mutex<Snapshot>,
    verdict: Mutex<Option<(String, BootTime)>>,
    log: Mutex<Option<record::Log>>,
    errors: Mutex<Option<ErrorCounter>>,
}

impl Default for Watch {
    fn default() -> Self {
        Watch::new()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Watch {
    pub const fn new() -> Self {
        Watch {
            snap: Mutex::new(Snapshot { at: 0, observed_at: None, connected: None, rx_bytes: None, rx_moved_at: None, dns: Vec::new() }),
            verdict: Mutex::new(None),
            log: Mutex::new(None),
            errors: Mutex::new(None),
        }
    }

    /// Become the signal log's writer (the last day comes back from disk).
    pub fn open(&self, log_dir: impl Into<PathBuf>) {
        *lock(&self.log) = Some(record::Log::new(log_dir));
    }

    /// Stop writing the signal log (the other process owns it now), after saving the minutes not
    /// on disk yet; sampling goes on.
    pub fn close(&self) {
        if let Some(mut l) = lock(&self.log).take() {
            l.save();
        }
    }

    /// Count failed core reads into `path` (reset to 0 now). Only the agent does: the count is
    /// about its own reads of core, whoever writes the log.
    pub fn count_errors(&self, path: impl Into<PathBuf>) {
        *lock(&self.errors) = Some(ErrorCounter::new(path));
    }

    /// Writing the signal log.
    pub fn writes(&self) -> bool {
        lock(&self.log).is_some()
    }

    /// One sample: the core's state in the old `/state` shape (or why it could not be read) and the
    /// home screen (`/v2/screen`; `None` leaves the minute's verdict as "-" and is no error).
    pub fn tick(
        &self,
        now: u64,
        elapsed: Option<&BootTime>,
        fetched: Result<Value, ReadError>,
        screen: Option<&Value>,
    ) {
        if let Some(c) = lock(&self.errors).as_mut() {
            c.record(classify(&fetched));
        }
        let state = fetched.ok();
        let rx = cell_rx_bytes(state.as_ref());
        {
            let mut s = lock(&self.snap);
            let next = sample(&s, now, elapsed, state.as_ref(), rx);
            *s = next;
        }
        let reading = record::Reading::parse(now, state.as_ref(), screen, rx);
        if let Some(v) = &reading.verdict {
            *lock(&self.verdict) = elapsed
                .filter(|t| !t.boot_id.is_empty())
                .map(|t| (v.clone(), t.clone()));
        } else if elapsed.is_none_or(|t| t.boot_id.is_empty()) {
            *lock(&self.verdict) = None;
        }
        if let Some(l) = lock(&self.log).as_mut() {
            l.feed(&reading);
        }
    }

    /// the core's current home-screen verdict code (ok, weak, stall, …); None when core hasn't
    /// answered for [`VERDICT_FRESH`] s.
    pub fn verdict(&self, now: Option<&BootTime>) -> Option<String> {
        let v = lock(&self.verdict).clone()?;
        fresh(Some(&v.1), now, VERDICT_FRESH).then_some(v.0)
    }

    pub fn snapshot(&self) -> Snapshot {
        lock(&self.snap).clone()
    }

    /// Is the cellular link itself working? Connected, and either something arrived in the last
    /// 30 s or — only then — one plain DNS query to the operator's resolver gets an answer
    /// (≤ 2 s): call it only when about to act on the answer.
    /// Unknown observations pause manual-first decisions; they do not report a
    /// measured outage, emit an event, or start a new probe.
    pub fn cell_alive(&self, now: Option<&BootTime>) -> bool {
        self.cell_alive_with(now, dns_rtt_ms)
    }

    fn cell_alive_with(
        &self,
        now: Option<&BootTime>,
        probe: impl FnOnce(&[String]) -> Option<u32>,
    ) -> bool {
        let s = self.snapshot();
        match alive_without_probe(&s, now) {
            PassiveLink::Receiving => true,
            PassiveLink::Unknown | PassiveLink::Down => false,
            PassiveLink::NeedsProbe => probe(&s.dns).is_some(),
        }
    }

    /// An action-time DNS query only with a current, known connected observation.
    pub fn direct_dns_ms(&self, now: Option<&BootTime>) -> Option<u32> {
        let s = self.snapshot();
        match alive_without_probe(&s, now) {
            PassiveLink::Receiving | PassiveLink::NeedsProbe => dns_rtt_ms(&s.dns),
            PassiveLink::Unknown | PassiveLink::Down => None,
        }
    }

    /// Record an event (deep diagnosis, proxy fallback …): kept in the history and appended to
    /// today's event file at once. Dropped (and said so) when this watcher does not write.
    pub fn push_event(&self, e: record::Event) {
        match lock(&self.log).as_mut() {
            Some(l) => l.push_event(e),
            None => eprintln!("netwatch: event {} while not writing the log, dropped", e.kind),
        }
    }

    /// Minute records and events with `from <= t < to`, from memory or, beyond 24 h, from disk
    /// (7 days). Empty when this watcher does not write.
    pub fn history(&self, from: u64, to: u64) -> (Vec<record::Minute>, Vec<record::Event>) {
        lock(&self.log).as_ref().map(|l| l.range(from, to)).unwrap_or_default()
    }
}

/// The parts of the core's legacy `/state` that netwatch reads, from the `/v2` blocks (same shapes,
/// V2-46): `net` = signal, `traffic` = the live block's, and `qos`, `uci_device_info`,
/// `interfaces` as they are. `block` gives a fresh block's data, `None` when missing or stale
/// (left out, which is how `/state` renders a failed read). `None` without a fresh signal block.
pub fn state_from_blocks(block: impl Fn(&str) -> Option<Value>) -> Option<Value> {
    let mut m = serde_json::Map::new();
    m.insert("net".into(), block("signal")?);
    if let Some(t) = block("live").and_then(|l| l.get("traffic").cloned()) {
        m.insert("traffic".into(), t);
    }
    for name in ["qos", "uci_device_info", "interfaces"] {
        if let Some(v) = block(name) {
            m.insert(name.into(), v);
        }
    }
    Some(Value::Object(m))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    fn bt(seconds: u64) -> BootTime {
        BootTime { boot_id: "test-boot".into(), seconds }
    }

    #[test]
    fn operator_dns_wherever_core_puts_it() {
        // the device's shape: wan_dns under uci_device_info, a list under interfaces.wan4
        let dev = json!({"net": {"wan_status": "ipv4_ipv6_connected"},
            "uci_device_info": {"wan_dns": "222.66.251.8' '116.236.159.8"},
            "interfaces": {"wan4": {"dns": ["222.66.251.8", "116.236.159.8"]}}});
        assert_eq!(operator_dns(&dev), ["222.66.251.8", "116.236.159.8"]);
        let only_iface = json!({"interfaces": {"wan4": {"dns": ["192.0.2.53", "2001:db8::1"]}}});
        assert_eq!(operator_dns(&only_iface), ["192.0.2.53"]);
        assert_eq!(operator_dns(&json!({"net": {"wan_dns": "1.1.1.1"}})), ["1.1.1.1"]);
        assert!(operator_dns(&json!({"net": {}})).is_empty());
        let s = sample(&Snapshot::default(), 10, Some(&bt(10)), Some(&dev), None);
        assert_eq!(s.dns, ["222.66.251.8", "116.236.159.8"]);
    }

    #[test]
    fn legacy_from_v2_state_keeps_fresh_blocks_only() {
        let blk = |stale: bool, data: Value| json!({"revision": 1, "observed_at": 1, "stale": stale, "data": data});
        let v2 = json!({"epoch": "e", "seq": 9, "blocks": {
            "signal": blk(false, json!({"wan_status": "ipv4_connected"})),
            "live": blk(false, json!({"traffic": {"rx_bytes": 42u64}, "system": {}})),
            "qos": blk(false, json!({"qci": 9})),
            "sim": blk(false, json!({"msisdn": "1"})),
            "uci_device_info": blk(true, json!({"wan_dns": "192.0.2.1"})),
            "nfc": blk(true, Value::Null),
        }});
        let st = legacy_from_v2(&v2);
        assert_eq!(st["net"]["wan_status"], "ipv4_connected");
        assert_eq!(cell_rx_bytes(Some(&st)), Some(42));
        assert_eq!(st["qos"]["qci"], 9);
        assert_eq!(st["sim"]["msisdn"], "1");
        assert!(st.get("uci_device_info").is_none());
        assert_eq!(classify(&Ok(st)), None);
        // stale signal (or a reply that is not /v2): no wan_status, counted
        let mut v2 = v2;
        v2["blocks"]["signal"]["stale"] = json!(true);
        assert_eq!(classify(&Ok(legacy_from_v2(&v2))), Some(ReadError::NoWanStatus));
        assert_eq!(classify(&Ok(legacy_from_v2(&json!({"net": {"wan_status": "x"}})))), Some(ReadError::NoWanStatus));
    }

    #[test]
    fn receive_counter_from_core() {
        assert_eq!(cell_rx_bytes(Some(&json!({"traffic": {"rx_bytes": 317798120u64}}))), Some(317798120));
        assert_eq!(cell_rx_bytes(Some(&json!({"traffic": {}}))), None);
        assert_eq!(cell_rx_bytes(Some(&json!({"net": {}}))), None);
        assert_eq!(cell_rx_bytes(None), None);
    }

    #[test]
    fn parses_core_dns() {
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
        assert_eq!(alive_without_probe(&s0, Some(&bt(100))), PassiveLink::Unknown, "no sample is not proof");

        let s1 = sample(&s0, 100, Some(&bt(100)), Some(&st), Some(1000));
        assert_eq!(s1.rx_moved_at, None, "first reading cannot show movement");
        assert_eq!(alive_without_probe(&s1, Some(&bt(100))), PassiveLink::NeedsProbe);

        let s2 = sample(&s1, 105, Some(&bt(105)), Some(&st), Some(2000));
        assert_eq!(s2.rx_moved_at, Some(bt(105)));
        assert_eq!(alive_without_probe(&s2, Some(&bt(135))), PassiveLink::Receiving);
        assert_eq!(alive_without_probe(&s2, Some(&bt(136))), PassiveLink::Unknown, "stale observation must not start a probe");

        let s3 = sample(&s2, 140, Some(&bt(140)), Some(&st), Some(2000));
        assert_eq!(s3.rx_moved_at, None, "35s sampling gap breaks the old pair");
        assert_eq!(s3.dns, vec!["1.1.1.1"]);

        let down = json!({"net": {"wan_status": "disconnected", "wan_dns": "--"}});
        let s4 = sample(&s3, 145, Some(&bt(145)), Some(&down), Some(3000));
        assert_eq!(alive_without_probe(&s4, Some(&bt(145))), PassiveLink::Down);
        assert_eq!(s4.dns, vec!["1.1.1.1"], "\"--\" keeps the last known resolver");

        let s5 = sample(&s4, 150, Some(&bt(150)), None, None);
        assert_eq!(s5.connected, None);
        assert_eq!(alive_without_probe(&s5, Some(&bt(150))), PassiveLink::Unknown, "missing data is not proof");
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

    #[test]
    fn a_watch_writes_the_log_only_while_open() {
        let d = tmpdir("watch");
        let w = Watch::new();
        let st = json!({"net": {"wan_status": "ipv4_connected", "type": "SA", "nr_rsrp": -90}, "traffic": {"rx_bytes": 5u64}});
        let screen = json!({"net": {"story": {"state": "ok"}}});
        // 2026-10-02 12:00 device clock: past record::CLOCK_SET
        let t0 = 1_790_942_400;
        w.tick(t0, Some(&bt(100)), Ok(st.clone()), Some(&screen));
        assert!(!w.writes());
        assert_eq!(w.verdict(Some(&bt(130))).as_deref(), Some("ok"));
        assert_eq!(w.verdict(Some(&bt(131))), None, "stale verdict says nothing");
        w.push_event(record::Event::new(t0, "speed", json!({})));
        assert!(w.history(0, u64::MAX).1.is_empty(), "not writing: the event is dropped");
        w.open(&d);
        w.push_event(record::Event::new(t0 + 1, "speed", json!({"mbps": 9})));
        assert_eq!(w.history(0, u64::MAX).1.len(), 1);
        assert!(d.join("20261002.events.jsonl").exists());
        w.close();
        assert!(w.history(0, u64::MAX).1.is_empty());
        // sampling goes on after close
        w.tick(t0 + 5, Some(&bt(105)), Ok(st), None);
        assert_eq!(w.snapshot().at, t0 + 5);
        let _ = fs::remove_dir_all(&d);
    }
}
