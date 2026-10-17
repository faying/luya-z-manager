//! "Registered but no internet"'s runner (u60-mcp.md step 1 phase 1; T2): gathers the four
//! layers through a [`FindingHost`] and hands them to `luyaz_api::finding::judge`. The core
//! implements the host (its state, `at.rs` directly: never `/v2/api/at/send`, which sends
//! `op.interrupt` first) and keeps the open findings (T3).
//!
//! Gate (D4): the probes take the same gate as the deep diagnosis, speed test and search/register.
//! Unlike the deep diagnosis it does not wait: busy means try at the next round (rechecks are at
//! most every 10 minutes anyway). Transactions join the gate in T4.
//!
//! What it sends, only when probing is allowed (screen on, a client, or the user asked; D8) and
//! only after the passive reading says down: one DNS query to the operator's resolver and one
//! internet probe. Signal and data are reads, no packets.

use std::net::{SocketAddr, TcpStream};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use luyaz_api::diagnose::Gate;
use luyaz_api::finding::*;

use crate::{dns_rtt_ms, now_unix};

pub mod store;

/// The internet probe's limit (netwatch's per-layer cap).
pub const PROBE_MAX: Duration = Duration::from_secs(5);

pub trait FindingHost {
    fn premises(&self) -> Premises;
    /// The collection round's passive reading says down or can't tell.
    fn passive_down(&self) -> bool;
    fn can_probe(&self) -> bool;
    fn iccid(&self) -> String;
    /// A search, a manual register or a speed test going.
    fn others_busy(&self) -> Option<&'static str>;
    fn registration(&self) -> Reg;
    fn data(&self) -> DataIn;
    fn operator_dns(&self) -> Vec<String>;
    /// The internet probe: must not need DNS (see `luyaz_api::finding`).
    fn internet_probe(&self) -> Option<Probe>;
    fn dns_probe(&self, dns: &[String]) -> Option<Probe> {
        if dns.is_empty() {
            return None;
        }
        Some(dns_rtt_ms(dns).map_or(Probe::Failed, |ms| Probe::Ok { ms }))
    }
}

/// Another operation holds the gate.
#[derive(Debug, PartialEq, Eq)]
pub struct Busy(pub &'static str);

/// One check. Reads first; probes only when the verdict could become "open".
pub fn check(host: &dyn FindingHost, gate: &Mutex<Gate>) -> Result<Verdict, Busy> {
    let now = now_unix();
    let mut c = Check {
        premises: host.premises(),
        passive_down: host.passive_down(),
        can_probe: host.can_probe(),
        iccid: host.iccid(),
        layers: Vec::new(),
        now,
    };
    if !c.passive_down || !c.can_probe || expected_outage(&c.premises) != Ok(None) {
        return Ok(judge(&c));
    }
    let reg = host.registration();
    c.layers.push(judge_signal(reg, now_unix()));
    c.layers.push(judge_data(&host.data(), now_unix()));
    if !reg.registered() {
        return Ok(judge(&c));
    }
    {
        let mut g = gate.lock().unwrap_or_else(|e| e.into_inner());
        if g.diag_since.is_some() {
            return Err(Busy("diagnose"));
        }
        if let Some(k) = host.others_busy() {
            return Err(Busy(k));
        }
        g.diag_since = Some(now);
    }
    let probed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let internet = host.internet_probe();
        let t = now_unix();
        let dns = host.dns_probe(&host.operator_dns());
        (judge_internet(internet, t), judge_dns(dns, now_unix()))
    }));
    gate.lock().unwrap_or_else(|e| e.into_inner()).diag_since = None;
    let (internet, dns) = probed.unwrap_or_else(|_| {
        let t = now_unix();
        (judge_internet(None, t), judge_dns(None, t))
    });
    c.layers.push(internet);
    c.layers.push(dns);
    c.now = now_unix();
    Ok(judge(&c))
}

/// The internet probe's targets (owner 2026-10-17: one in China, one abroad; either answering
/// is enough): public resolvers' anycast addresses, which also answer plain HTTP on port 80.
/// Any HTTP status line counts: an IP literal with a reply proves the path, no DNS, no TLS.
pub const INTERNET_TARGETS: [(&str, &str, &str); 2] = [
    ("119.29.29.29:80", "119.29.29.29", "/d?dn=qq.com"),
    ("1.1.1.1:80", "1.1.1.1", "/"),
];

/// [`INTERNET_TARGETS`] one after the other (each up to [`PROBE_MAX`]): the first reply wins.
pub fn internet_either() -> Probe {
    for (addr, host, path) in INTERNET_TARGETS {
        if let Ok(a) = addr.parse() {
            if let ok @ Probe::Ok { .. } = http_get(a, host, path, any_status) {
                return ok;
            }
        }
    }
    Probe::Failed
}

/// A plain-HTTP generate_204 to an IP-literal address with a Host header: no DNS, no TLS (the
/// device clock is local time labelled UTC). Any 204 counts. The route is the system's; on this
/// device that is the cellular interface (binding to it needs SO_BINDTODEVICE, not here yet).
pub fn http_204(addr: SocketAddr, host: &str) -> Probe {
    http_get(addr, host, "/generate_204", status_204)
}

fn http_get(addr: SocketAddr, host: &str, path: &str, good: fn(&[u8]) -> bool) -> Probe {
    use std::io::{Read, Write};
    let t = Instant::now();
    let Ok(mut s) = TcpStream::connect_timeout(&addr, PROBE_MAX) else {
        return Probe::Failed;
    };
    let left = PROBE_MAX
        .saturating_sub(t.elapsed())
        .max(Duration::from_millis(100));
    let _ = s.set_read_timeout(Some(left));
    let _ = s.set_write_timeout(Some(left));
    let req = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    if s.write_all(req.as_bytes()).is_err() {
        return Probe::Failed;
    }
    let mut buf = [0u8; 64];
    let mut n = 0;
    while n < 12 {
        match s.read(&mut buf[n..]) {
            Ok(0) | Err(_) => break,
            Ok(k) => n += k,
        }
    }
    if good(&buf[..n]) {
        Probe::Ok {
            ms: t.elapsed().as_millis() as u32,
        }
    } else {
        Probe::Failed
    }
}

fn any_status(head: &[u8]) -> bool {
    head.starts_with(b"HTTP/1.") && head.len() >= 12 && head[9..12].iter().all(u8::is_ascii_digit)
}

fn status_204(head: &[u8]) -> bool {
    head.starts_with(b"HTTP/1.") && head.get(8..12) == Some(b" 204")
}

#[cfg(test)]
mod tests;
