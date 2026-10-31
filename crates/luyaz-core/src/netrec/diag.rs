//! Second batch of the recorder: root qdisc counters (rtnetlink `RTM_GETQDISC`) and a
//! `TCP_INFO` summary of the device's own WAN-side TCP (sock_diag with `INET_DIAG_INFO`).
//! Both are plain netlink round trips: no fork, no ptrace, no `/proc/*/fd` scan. Only called
//! while a session is open (see the recorder for the cadence).
//!
//! Offsets checked against the kernel UAPI headers (`offsetof` in the build container):
//! `tcp_info` rtt 68, snd_cwnd 80, total_retrans 100, min_rtt 148, data_segs_out 156,
//! snd_wnd 228, last_data_sent 44, last_data_recv 52; `inet_diag_msg` cookie 44, 72 bytes;
//! `tcmsg` 20 bytes (ifindex 4, parent 12); `gnet_stats_queue` qlen 0, backlog 4, drops 8,
//! requeues 12, overlimits 16; legacy `tc_stats` drops 12, overlimits 16, qlen 28, backlog 32.

use crate::nl::{self, NlSock};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::IpAddr;

const RTM_NEWQDISC: u16 = 36;
const RTM_GETQDISC: u16 = 38;
const TC_H_ROOT: u32 = 0xFFFF_FFFF;
const TCA_STATS: u16 = 3;
const TCA_STATS2: u16 = 7;
const TCA_STATS_QUEUE: u16 = 3;
const SOCK_DIAG_BY_FAMILY: u16 = 20;
const INET_DIAG_INFO: u16 = 2;
const TCP_ESTABLISHED: u32 = 1;
/// A connection counts as active when it sent or received data this recently (ms).
pub const ACTIVE_MS: u32 = 5000;
/// At most this many sockets are looked at per dump.
pub const MAX_SOCKS: usize = 4096;

/// Cumulative counters of one root qdisc (they reset when the interface is recreated).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Qdisc {
    pub drops: u32,
    pub overlimits: u32,
    pub requeues: u32,
    pub backlog: u32,
    pub qlen: u32,
}

fn u32_at(p: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(p.get(off..off + 4)?.try_into().ok()?))
}

/// One `RTM_NEWQDISC` payload → (ifindex, parent, counters).
pub fn parse_qdisc(p: &[u8]) -> Option<(i32, u32, Qdisc)> {
    let ifindex = u32_at(p, 4)? as i32;
    let parent = u32_at(p, 12)?;
    let attrs = p.get(20..)?;
    let mut legacy = None;
    for (t, a) in nl::attrs(attrs) {
        match t {
            TCA_STATS2 => {
                if let Some((_, q)) = nl::attrs(a).find(|(t, _)| *t == TCA_STATS_QUEUE) {
                    return Some((
                        ifindex,
                        parent,
                        Qdisc {
                            qlen: u32_at(q, 0)?,
                            backlog: u32_at(q, 4)?,
                            drops: u32_at(q, 8)?,
                            requeues: u32_at(q, 12)?,
                            overlimits: u32_at(q, 16)?,
                        },
                    ));
                }
            }
            TCA_STATS => {
                legacy = (|| {
                    Some(Qdisc {
                        drops: u32_at(a, 12)?,
                        overlimits: u32_at(a, 16)?,
                        qlen: u32_at(a, 28)?,
                        backlog: u32_at(a, 32)?,
                        requeues: 0,
                    })
                })();
            }
            _ => {}
        }
    }
    legacy.map(|q| (ifindex, parent, q))
}

/// The root qdisc of each wanted ifindex (an `mq` root already sums its children).
pub fn roots(payloads: &[Vec<u8>], want: &[Option<i32>]) -> Vec<Option<Qdisc>> {
    let mut out = vec![None; want.len()];
    for p in payloads {
        let Some((ifi, parent, q)) = parse_qdisc(p) else {
            continue;
        };
        if parent != TC_H_ROOT {
            continue;
        }
        for (i, w) in want.iter().enumerate() {
            if *w == Some(ifi) {
                out[i] = Some(q);
            }
        }
    }
    out
}

pub fn qdisc_request() -> [u8; 20] {
    [0u8; 20] // tcmsg, AF_UNSPEC: every interface (filtered here)
}

pub fn ifindex(name: &str) -> Option<i32> {
    let c = std::ffi::CString::new(name).ok()?;
    let i = unsafe { libc::if_nametoindex(c.as_ptr()) };
    (i != 0).then_some(i as i32)
}

pub fn dump_qdiscs(sock: &mut NlSock) -> std::io::Result<Vec<Vec<u8>>> {
    sock.dump(RTM_GETQDISC, &qdisc_request(), RTM_NEWQDISC)
}

/// One established TCP socket with its `tcp_info`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tcp {
    pub cookie: u64,
    pub dst: IpAddr,
    pub dport: u16,
    pub sport: u16,
    pub rtt_us: u32,
    pub min_rtt_us: Option<u32>,
    pub cwnd: u32,
    pub snd_wnd: Option<u32>,
    pub retrans: u32,
    pub segs_out: Option<u32>,
    pub idle_ms: u32,
}

/// One `SOCK_DIAG_BY_FAMILY` payload with an `INET_DIAG_INFO` attribute.
pub fn parse_tcp(p: &[u8]) -> Option<Tcp> {
    let s = nl::parse_diag(p, libc::IPPROTO_TCP as u8)?;
    let cookie = u64::from_ne_bytes(p.get(44..52)?.try_into().ok()?);
    let (_, i) = nl::attrs(p.get(72..)?).find(|(t, _)| *t == INET_DIAG_INFO)?;
    let sent = u32_at(i, 44)?;
    let recv = u32_at(i, 52)?;
    Some(Tcp {
        cookie,
        dst: s.dst,
        dport: s.dport,
        sport: s.sport,
        rtt_us: u32_at(i, 68)?,
        cwnd: u32_at(i, 80)?,
        retrans: u32_at(i, 100)?,
        min_rtt_us: u32_at(i, 148),
        segs_out: u32_at(i, 156),
        snd_wnd: u32_at(i, 228),
        idle_ms: sent.min(recv),
    })
}

pub fn tcp_request(family: u8) -> [u8; 56] {
    let mut r = [0u8; 56];
    r[0] = family;
    r[1] = libc::IPPROTO_TCP as u8;
    r[2] = 1 << (INET_DIAG_INFO - 1);
    r[4..8].copy_from_slice(&(1u32 << TCP_ESTABLISHED).to_ne_bytes());
    r
}

/// Every established TCP socket, v4 and v6, unfiltered.
pub fn dump_tcp(sock: &mut NlSock) -> std::io::Result<Vec<Tcp>> {
    let mut v = Vec::new();
    for fam in [libc::AF_INET as u8, libc::AF_INET6 as u8] {
        for p in sock.dump(SOCK_DIAG_BY_FAMILY, &tcp_request(fam), SOCK_DIAG_BY_FAMILY)? {
            if v.len() >= MAX_SOCKS {
                break;
            }
            v.extend(parse_tcp(&p));
        }
    }
    Ok(v)
}

/// A WAN-side peer: not loopback, private, CGNAT (tailnet), link-local, ULA, or mihomo's
/// TUN range 198.18.0.0/15 (the LAN clients' side of proxied flows).
pub fn wan(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => {
            let o = a.octets();
            !(a.is_loopback()
                || a.is_private()
                || a.is_link_local()
                || a.is_unspecified()
                || a.is_multicast()
                || a.is_broadcast()
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
                || (o[0] == 198 && (o[1] & 0xfe) == 18))
        }
        IpAddr::V6(a) => {
            let s = a.segments()[0];
            !(a.is_loopback()
                || a.is_unspecified()
                || a.is_multicast()
                || (s & 0xfe00) == 0xfc00
                || (s & 0xffc0) == 0xfe80)
        }
    }
}

/// One TCP sample: aggregates only, no per-connection columns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TcpSum {
    /// WAN-side established connections.
    pub n: u16,
    /// Of those, the ones that moved data in the last [`ACTIVE_MS`].
    pub active: u16,
    /// Segments retransmitted since the previous sample (connections seen in both, plus new
    /// ones in full). None on a session's first sample.
    pub retx: Option<u32>,
    /// Data segments sent over the same span (the denominator for a loss ratio).
    pub segs: Option<u32>,
    /// Over the active connections (µs); u32::MAX = none active.
    pub rtt_p50: u32,
    pub rtt_max: u32,
    pub min_rtt: u32,
    pub cwnd_min: u32,
    /// Smallest peer-advertised window (bytes), the "rwnd" that can cap a flow.
    pub swnd_min: u32,
    /// What the dump cost (µs).
    pub dump_us: u32,
}

pub const NA: u32 = u32::MAX;

/// Summarise `conns` (already WAN-filtered or not; the filter runs here). `prev` = cookie →
/// (retrans, segs_out) of the last sample (`None` = first sample); returns the new map.
pub fn summarize(
    conns: &[Tcp],
    prev: Option<&HashMap<u64, (u32, u32)>>,
    dump_us: u32,
) -> (TcpSum, HashMap<u64, (u32, u32)>) {
    let mut next = HashMap::new();
    let (mut retx, mut segs) = (0u64, 0u64);
    let mut rtts = Vec::new();
    let (mut min_rtt, mut cwnd_min, mut swnd_min) = (NA, NA, NA);
    let mut n = 0u32;
    for c in conns.iter().filter(|c| wan(&c.dst)) {
        n += 1;
        let so = c.segs_out.unwrap_or(0);
        next.insert(c.cookie, (c.retrans, so));
        if let Some(p) = prev {
            let (r0, s0) = p.get(&c.cookie).copied().unwrap_or((0, 0));
            retx += u64::from(c.retrans.saturating_sub(r0));
            segs += u64::from(so.saturating_sub(s0));
        }
        if c.idle_ms <= ACTIVE_MS {
            rtts.push(c.rtt_us);
            if let Some(m) = c.min_rtt_us.filter(|&m| m > 0 && m != NA) {
                min_rtt = min_rtt.min(m);
            }
            cwnd_min = cwnd_min.min(c.cwnd);
            if let Some(w) = c.snd_wnd {
                swnd_min = swnd_min.min(w);
            }
        }
    }
    rtts.sort_unstable();
    let clamp = |v: u64| v.min(u64::from(NA - 1)) as u32;
    (
        TcpSum {
            n: n.min(u32::from(u16::MAX - 1)) as u16,
            active: rtts.len().min(usize::from(u16::MAX - 1)) as u16,
            retx: prev.map(|_| clamp(retx)),
            segs: prev.map(|_| clamp(segs)),
            rtt_p50: rtts.get(rtts.len() / 2).copied().unwrap_or(NA),
            rtt_max: rtts.last().copied().unwrap_or(NA),
            min_rtt,
            cwnd_min,
            swnd_min,
            dump_us,
        },
        next,
    )
}

fn na(v: u32) -> Value {
    if v == NA { Value::Null } else { json!(v) }
}

pub fn qdisc_json(q: Option<Qdisc>) -> Value {
    q.map_or(Value::Null, |q| {
        json!({"drops": q.drops, "overlimits": q.overlimits, "requeues": q.requeues,
               "backlog": q.backlog, "qlen": q.qlen})
    })
}

pub fn tcp_json(t: Option<TcpSum>) -> Value {
    t.map_or(Value::Null, |t| {
        json!({"n": t.n, "active": t.active, "retx": t.retx, "segs": t.segs,
               "rtt_p50_us": na(t.rtt_p50), "rtt_max_us": na(t.rtt_max),
               "min_rtt_us": na(t.min_rtt), "cwnd_min": na(t.cwnd_min),
               "swnd_min": na(t.swnd_min), "dump_us": t.dump_us})
    })
}

/// Row bytes: qdisc 20 bytes (drops == u32::MAX = none).
pub fn put_qdisc(b: &mut [u8], q: Option<Qdisc>) {
    let q = q.unwrap_or(Qdisc {
        drops: NA,
        ..Default::default()
    });
    for (i, v) in [q.drops, q.overlimits, q.requeues, q.backlog, q.qlen]
        .into_iter()
        .enumerate()
    {
        b[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

pub fn get_qdisc(b: &[u8]) -> Option<Qdisc> {
    let q = Qdisc {
        drops: le32(b, 0),
        overlimits: le32(b, 4),
        requeues: le32(b, 8),
        backlog: le32(b, 12),
        qlen: le32(b, 16),
    };
    (q.drops != NA).then_some(q)
}

/// Row bytes: TCP summary 40 bytes (n == u16::MAX = not sampled this round).
pub fn put_tcp(b: &mut [u8], t: Option<TcpSum>) {
    let Some(t) = t else {
        b[0..2].copy_from_slice(&u16::MAX.to_le_bytes());
        return;
    };
    b[0..2].copy_from_slice(&t.n.to_le_bytes());
    b[2..4].copy_from_slice(&t.active.to_le_bytes());
    for (i, v) in [
        t.retx.unwrap_or(NA),
        t.segs.unwrap_or(NA),
        t.rtt_p50,
        t.rtt_max,
        t.min_rtt,
        t.cwnd_min,
        t.swnd_min,
        t.dump_us,
    ]
    .into_iter()
    .enumerate()
    {
        b[4 + i * 4..8 + i * 4].copy_from_slice(&v.to_le_bytes());
    }
}

pub fn get_tcp(b: &[u8]) -> Option<TcpSum> {
    let n = u16::from_le_bytes([b[0], b[1]]);
    if n == u16::MAX {
        return None;
    }
    let opt = |v: u32| (v != NA).then_some(v);
    Some(TcpSum {
        n,
        active: u16::from_le_bytes([b[2], b[3]]),
        retx: opt(le32(b, 4)),
        segs: opt(le32(b, 8)),
        rtt_p50: le32(b, 12),
        rtt_max: le32(b, 16),
        min_rtt: le32(b, 20),
        cwnd_min: le32(b, 24),
        swnd_min: le32(b, 28),
        dump_us: le32(b, 32),
    })
}
