//! Netlink by hand (u60-ledger.md §5): ctnetlink dump + DESTROY events, taskstats
//! exit records, inet_diag socket lists. Event sockets are never registered with the
//! runtime: the kernel queues messages in the socket buffer and the ledger drains
//! them on its own tick, so they add no wakeups.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

pub const NETLINK_SOCK_DIAG: i32 = 4;
pub const NETLINK_NETFILTER: i32 = 12;
pub const NETLINK_GENERIC: i32 = 16;

const NLM_F_REQUEST: u16 = 1;
const NLM_F_ACK: u16 = 4;
const NLM_F_DUMP: u16 = 0x300;
const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
const SOL_NETLINK: i32 = 270;
const NETLINK_ADD_MEMBERSHIP: i32 = 1;

// ctnetlink (linux/netfilter/nfnetlink_conntrack.h)
const NFNL_SUBSYS_CTNETLINK: u16 = 1;
const IPCTNL_MSG_CT_NEW: u16 = 0;
const IPCTNL_MSG_CT_GET: u16 = 1;
const IPCTNL_MSG_CT_DELETE: u16 = 2;
pub const NFNLGRP_CONNTRACK_DESTROY: u32 = 3;
const CTA_TUPLE_ORIG: u16 = 1;
const CTA_TUPLE_REPLY: u16 = 2;
const CTA_COUNTERS_ORIG: u16 = 9;
const CTA_COUNTERS_REPLY: u16 = 10;
const CTA_ID: u16 = 12;
const CTA_TUPLE_IP: u16 = 1;
const CTA_TUPLE_PROTO: u16 = 2;
const CTA_IP_V4_SRC: u16 = 1;
const CTA_IP_V4_DST: u16 = 2;
const CTA_IP_V6_SRC: u16 = 3;
const CTA_IP_V6_DST: u16 = 4;
const CTA_PROTO_NUM: u16 = 1;
const CTA_PROTO_SRC_PORT: u16 = 2;
const CTA_PROTO_DST_PORT: u16 = 3;
const CTA_COUNTERS_PACKETS: u16 = 1;
const CTA_COUNTERS_BYTES: u16 = 2;

// generic netlink / taskstats (linux/genetlink.h, linux/taskstats.h; offsets checked
// against the header, see u60-ledger.md §2)
const GENL_ID_CTRL: u16 = 0x10;
const CTRL_CMD_GETFAMILY: u8 = 3;
const CTRL_ATTR_FAMILY_ID: u16 = 1;
const CTRL_ATTR_FAMILY_NAME: u16 = 2;
const TASKSTATS_CMD_GET: u8 = 1;
const TASKSTATS_CMD_ATTR_REGISTER_CPUMASK: u16 = 3;
const TASKSTATS_TYPE_PID: u16 = 1;
const TASKSTATS_TYPE_TGID: u16 = 2;
const TASKSTATS_TYPE_STATS: u16 = 3;
const TASKSTATS_TYPE_AGGR_PID: u16 = 4;
const TASKSTATS_TYPE_AGGR_TGID: u16 = 5;

// sock_diag (linux/inet_diag.h)
const SOCK_DIAG_BY_FAMILY: u16 = 20;

// ── message helpers ─────────────────────────────────────────────────────────

fn align4(n: usize) -> usize {
    (n + 3) & !3
}

/// Iterates (type, payload) of the attributes in `buf`; strips NLA_F_NESTED/BYTEORDER.
pub fn attrs(mut buf: &[u8]) -> impl Iterator<Item = (u16, &[u8])> {
    std::iter::from_fn(move || {
        if buf.len() < 4 {
            return None;
        }
        let len = u16::from_ne_bytes([buf[0], buf[1]]) as usize;
        let ty = u16::from_ne_bytes([buf[2], buf[3]]) & 0x3fff;
        if len < 4 || len > buf.len() {
            buf = &[];
            return None;
        }
        let payload = &buf[4..len];
        buf = &buf[align4(len).min(buf.len())..];
        Some((ty, payload))
    })
}

fn attr(buf: &[u8], want: u16) -> Option<&[u8]> {
    attrs(buf).find(|(t, _)| *t == want).map(|(_, p)| p)
}

fn be16(p: &[u8]) -> Option<u16> {
    Some(u16::from_be_bytes(p.get(..2)?.try_into().ok()?))
}
fn be32(p: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes(p.get(..4)?.try_into().ok()?))
}
fn be64(p: &[u8]) -> Option<u64> {
    Some(u64::from_be_bytes(p.get(..8)?.try_into().ok()?))
}
fn ne16(p: &[u8]) -> Option<u16> {
    Some(u16::from_ne_bytes(p.get(..2)?.try_into().ok()?))
}
fn ne32(p: &[u8]) -> Option<u32> {
    Some(u32::from_ne_bytes(p.get(..4)?.try_into().ok()?))
}
fn ne64_at(p: &[u8], off: usize) -> Option<u64> {
    Some(u64::from_ne_bytes(p.get(off..off + 8)?.try_into().ok()?))
}
fn ne32_at(p: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(p.get(off..off + 4)?.try_into().ok()?))
}

pub fn put_attr(out: &mut Vec<u8>, ty: u16, payload: &[u8]) {
    let len = 4 + payload.len();
    out.extend_from_slice(&(len as u16).to_ne_bytes());
    out.extend_from_slice(&ty.to_ne_bytes());
    out.extend_from_slice(payload);
    out.resize(align4(out.len()), 0);
}

pub fn nlmsg(ty: u16, flags: u16, seq: u32, body: &[u8]) -> Vec<u8> {
    let len = 16 + body.len();
    let mut m = Vec::with_capacity(align4(len));
    m.extend_from_slice(&(len as u32).to_ne_bytes());
    m.extend_from_slice(&ty.to_ne_bytes());
    m.extend_from_slice(&flags.to_ne_bytes());
    m.extend_from_slice(&seq.to_ne_bytes());
    m.extend_from_slice(&0u32.to_ne_bytes());
    m.extend_from_slice(body);
    m.resize(align4(m.len()), 0);
    m
}

/// Splits one datagram into (type, payload-after-nlmsghdr).
pub fn messages(mut buf: &[u8]) -> Vec<(u16, &[u8])> {
    let mut v = Vec::new();
    while buf.len() >= 16 {
        let len = u32::from_ne_bytes(buf[0..4].try_into().unwrap()) as usize;
        let ty = u16::from_ne_bytes(buf[4..6].try_into().unwrap());
        if len < 16 || len > buf.len() {
            break;
        }
        v.push((ty, &buf[16..len]));
        buf = &buf[align4(len).min(buf.len())..];
    }
    v
}

// ── conntrack ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct Tuple {
    pub src: IpAddr,
    pub dst: IpAddr,
    pub proto: u8,
    pub sport: u16,
    pub dport: u16,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Flow {
    pub id: u32,
    pub orig: Tuple,
    pub reply: Tuple,
    /// packets / bytes, both directions
    pub pkts: u64,
    pub bytes: u64,
}

fn parse_tuple(p: &[u8]) -> Option<Tuple> {
    let ip = attr(p, CTA_TUPLE_IP)?;
    let (src, dst) = if let (Some(s), Some(d)) = (attr(ip, CTA_IP_V4_SRC), attr(ip, CTA_IP_V4_DST))
    {
        (
            IpAddr::V4(Ipv4Addr::from(<[u8; 4]>::try_from(s.get(..4)?).ok()?)),
            IpAddr::V4(Ipv4Addr::from(<[u8; 4]>::try_from(d.get(..4)?).ok()?)),
        )
    } else {
        let s = attr(ip, CTA_IP_V6_SRC)?;
        let d = attr(ip, CTA_IP_V6_DST)?;
        (
            IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(s.get(..16)?).ok()?)),
            IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(d.get(..16)?).ok()?)),
        )
    };
    let pr = attr(p, CTA_TUPLE_PROTO)?;
    Some(Tuple {
        src,
        dst,
        proto: *attr(pr, CTA_PROTO_NUM)?.first()?,
        sport: attr(pr, CTA_PROTO_SRC_PORT).and_then(be16).unwrap_or(0),
        dport: attr(pr, CTA_PROTO_DST_PORT).and_then(be16).unwrap_or(0),
    })
}

fn counters(p: Option<&[u8]>) -> (u64, u64) {
    match p {
        Some(c) => (
            attr(c, CTA_COUNTERS_PACKETS).and_then(be64).unwrap_or(0),
            attr(c, CTA_COUNTERS_BYTES).and_then(be64).unwrap_or(0),
        ),
        None => (0, 0),
    }
}

/// Parses one ctnetlink message payload (nfgenmsg + attributes).
pub fn parse_ct(payload: &[u8]) -> Option<Flow> {
    let a = payload.get(4..)?;
    let (po, bo) = counters(attr(a, CTA_COUNTERS_ORIG));
    let (pr, br) = counters(attr(a, CTA_COUNTERS_REPLY));
    Some(Flow {
        id: attr(a, CTA_ID).and_then(be32).unwrap_or(0),
        orig: parse_tuple(attr(a, CTA_TUPLE_ORIG)?)?,
        reply: parse_tuple(attr(a, CTA_TUPLE_REPLY)?)?,
        pkts: po + pr,
        bytes: bo + br,
    })
}

const CT_NEW_TYPE: u16 = (NFNL_SUBSYS_CTNETLINK << 8) | IPCTNL_MSG_CT_NEW;
const CT_DELETE_TYPE: u16 = (NFNL_SUBSYS_CTNETLINK << 8) | IPCTNL_MSG_CT_DELETE;

// ── taskstats ───────────────────────────────────────────────────────────────

/// One exit record (a thread, or a whole thread group for `tgid_aggregate`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TaskExit {
    pub pid: u32,
    pub tgid_aggregate: bool,
    pub ppid: u32,
    pub comm: String,
    pub ctxt: u64,
    pub cpu_us: u64,
    /// process start, seconds since the epoch (ac_btime)
    pub btime: u32,
}

/// Minimum struct taskstats length we read (through nivcsw).
const TS_MIN: usize = 288;

pub fn parse_taskstats(stats: &[u8]) -> Option<TaskExit> {
    if stats.len() < TS_MIN {
        return None;
    }
    let comm_raw = &stats[80..112];
    let end = comm_raw.iter().position(|&b| b == 0).unwrap_or(32);
    Some(TaskExit {
        pid: ne32_at(stats, 128)?,
        ppid: ne32_at(stats, 132)?,
        btime: ne32_at(stats, 136)?,
        cpu_us: ne64_at(stats, 152)? + ne64_at(stats, 160)?,
        ctxt: ne64_at(stats, 272)? + ne64_at(stats, 280)?,
        comm: String::from_utf8_lossy(&comm_raw[..end]).into_owned(),
        tgid_aggregate: false,
    })
}

/// Parses a TASKSTATS_CMD_NEW payload (genlmsghdr + attributes).
pub fn parse_ts_msg(payload: &[u8]) -> Vec<TaskExit> {
    let Some(a) = payload.get(4..) else {
        return vec![];
    };
    let mut v = Vec::new();
    for (ty, nested) in attrs(a) {
        if ty != TASKSTATS_TYPE_AGGR_PID && ty != TASKSTATS_TYPE_AGGR_TGID {
            continue;
        }
        let id = attr(nested, TASKSTATS_TYPE_PID)
            .or_else(|| attr(nested, TASKSTATS_TYPE_TGID))
            .and_then(ne32);
        if let Some(mut e) = attr(nested, TASKSTATS_TYPE_STATS).and_then(parse_taskstats) {
            if let Some(id) = id {
                e.pid = id;
            }
            e.tgid_aggregate = ty == TASKSTATS_TYPE_AGGR_TGID;
            v.push(e);
        }
    }
    v
}

// ── inet_diag ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct Sock {
    pub proto: u8,
    pub sport: u16,
    pub src: IpAddr,
    pub dport: u16,
    pub dst: IpAddr,
    pub inode: u32,
}

pub fn parse_diag(payload: &[u8], proto: u8) -> Option<Sock> {
    if payload.len() < 72 {
        return None;
    }
    let family = payload[0];
    let ip = |off: usize| -> Option<IpAddr> {
        Some(if family == libc::AF_INET as u8 {
            IpAddr::V4(Ipv4Addr::from(
                <[u8; 4]>::try_from(&payload[off..off + 4]).ok()?,
            ))
        } else {
            let v6 = Ipv6Addr::from(<[u8; 16]>::try_from(&payload[off..off + 16]).ok()?);
            // dual-stack sockets talking IPv4 show ::ffff:a.b.c.d; conntrack shows a.b.c.d
            v6.to_ipv4_mapped()
                .map(IpAddr::V4)
                .unwrap_or(IpAddr::V6(v6))
        })
    };
    Some(Sock {
        proto,
        sport: be16(&payload[4..6])?,
        dport: be16(&payload[6..8])?,
        src: ip(8)?,
        dst: ip(24)?,
        inode: ne32_at(payload, 68)?,
    })
}

// ── sockets ─────────────────────────────────────────────────────────────────

pub struct NlSock {
    fd: i32,
    seq: u32,
}

impl Drop for NlSock {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}

fn cvt(r: i32) -> io::Result<i32> {
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r)
    }
}

impl NlSock {
    /// `rcvbuf`: bytes for SO_RCVBUFFORCE (root; falls back to SO_RCVBUF).
    pub fn open(proto: i32, rcvbuf: i32) -> io::Result<NlSock> {
        let fd = cvt(unsafe {
            libc::socket(libc::AF_NETLINK, libc::SOCK_RAW | libc::SOCK_CLOEXEC, proto)
        })?;
        let s = NlSock { fd, seq: 1 };
        let sz = &rcvbuf as *const i32 as *const libc::c_void;
        let n = std::mem::size_of::<i32>() as u32;
        if unsafe { libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_RCVBUFFORCE, sz, n) } < 0 {
            unsafe { libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_RCVBUF, sz, n) };
        }
        // dumps block at most 2 s; event drains use MSG_DONTWAIT
        let tv = libc::timeval {
            tv_sec: 2,
            tv_usec: 0,
        };
        unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                &tv as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::timeval>() as u32,
            )
        };
        let mut sa: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        sa.nl_family = libc::AF_NETLINK as u16;
        cvt(unsafe {
            libc::bind(
                fd,
                &sa as *const _ as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_nl>() as u32,
            )
        })?;
        Ok(s)
    }

    pub fn join(&self, group: u32) -> io::Result<()> {
        cvt(unsafe {
            libc::setsockopt(
                self.fd,
                SOL_NETLINK,
                NETLINK_ADD_MEMBERSHIP,
                &group as *const u32 as *const libc::c_void,
                4,
            )
        })
        .map(|_| ())
    }

    fn send(&mut self, ty: u16, flags: u16, body: &[u8]) -> io::Result<u32> {
        self.seq = self.seq.wrapping_add(1);
        let m = nlmsg(ty, flags, self.seq, body);
        let mut sa: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        sa.nl_family = libc::AF_NETLINK as u16;
        cvt(unsafe {
            libc::sendto(
                self.fd,
                m.as_ptr() as *const libc::c_void,
                m.len(),
                0,
                &sa as *const _ as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_nl>() as u32,
            ) as i32
        })?;
        Ok(self.seq)
    }

    fn recv(&self, buf: &mut [u8], flags: i32) -> io::Result<usize> {
        let n = unsafe {
            libc::recv(
                self.fd,
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
                flags,
            )
        };
        if n < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    }

    /// Sends a dump request and collects every reply payload of `want` type until DONE.
    fn dump(&mut self, ty: u16, body: &[u8], want: u16) -> io::Result<Vec<Vec<u8>>> {
        self.send(ty, NLM_F_REQUEST | NLM_F_DUMP, body)?;
        let mut out = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = self.recv(&mut buf, 0)?;
            for (t, p) in messages(&buf[..n]) {
                match t {
                    NLMSG_DONE => return Ok(out),
                    NLMSG_ERROR => {
                        let errno = p
                            .get(..4)
                            .map(|b| i32::from_ne_bytes(b.try_into().unwrap()))
                            .unwrap_or(0);
                        if errno != 0 {
                            return Err(io::Error::from_raw_os_error(-errno));
                        }
                    }
                    t if t == want => out.push(p.to_vec()),
                    _ => {}
                }
            }
        }
    }

    /// Reads everything queued without blocking. `overflow` = the kernel dropped
    /// messages since the last drain (ENOBUFS; it does not say how many).
    pub fn drain(&self) -> (Vec<Vec<u8>>, bool) {
        let mut out = Vec::new();
        let mut overflow = false;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match self.recv(&mut buf, libc::MSG_DONTWAIT) {
                Ok(0) => break,
                Ok(n) => out.push(buf[..n].to_vec()),
                Err(e) if e.raw_os_error() == Some(libc::ENOBUFS) => overflow = true,
                Err(_) => break,
            }
        }
        (out, overflow)
    }
}

/// Conntrack: one socket for dumps, one subscribed to DESTROY events.
pub struct Conntrack {
    dump: NlSock,
    events: NlSock,
}

impl Conntrack {
    pub fn open(event_buf: i32) -> io::Result<Conntrack> {
        let dump = NlSock::open(NETLINK_NETFILTER, 1 << 20)?;
        let events = NlSock::open(NETLINK_NETFILTER, event_buf)?;
        events.join(NFNLGRP_CONNTRACK_DESTROY)?;
        Ok(Conntrack { dump, events })
    }

    pub fn dump(&mut self) -> io::Result<Vec<Flow>> {
        // nfgenmsg: AF_UNSPEC = every L3 family, version 0, res_id 0
        let body = [0u8, 0, 0, 0];
        let ty = (NFNL_SUBSYS_CTNETLINK << 8) | IPCTNL_MSG_CT_GET;
        Ok(self
            .dump
            .dump(ty, &body, CT_NEW_TYPE)?
            .iter()
            .filter_map(|p| parse_ct(p))
            .collect())
    }

    /// Flows destroyed since the last call, with their final counters.
    pub fn destroyed(&self) -> (Vec<Flow>, bool) {
        let (dgrams, overflow) = self.events.drain();
        let mut v = Vec::new();
        for d in &dgrams {
            for (t, p) in messages(d) {
                if t == CT_DELETE_TYPE {
                    v.extend(parse_ct(p));
                }
            }
        }
        (v, overflow)
    }
}

pub struct Taskstats {
    sock: NlSock,
    family: u16,
}

impl Taskstats {
    /// Registers for exit records on CPUs `cpus` (e.g. "0-3").
    pub fn open(cpus: &str, buf: i32) -> io::Result<Taskstats> {
        let mut ctl = NlSock::open(NETLINK_GENERIC, 64 * 1024)?;
        let mut body = vec![CTRL_CMD_GETFAMILY, 1, 0, 0];
        put_attr(&mut body, CTRL_ATTR_FAMILY_NAME, b"TASKSTATS\0");
        ctl.send(GENL_ID_CTRL, NLM_F_REQUEST, &body)?;
        let mut rb = vec![0u8; 8192];
        let n = ctl.recv(&mut rb, 0)?;
        let family = messages(&rb[..n])
            .into_iter()
            .find(|(t, _)| *t == GENL_ID_CTRL)
            .and_then(|(_, p)| attr(p.get(4..)?, CTRL_ATTR_FAMILY_ID).and_then(ne16))
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no TASKSTATS family"))?;
        let mut sock = NlSock::open(NETLINK_GENERIC, buf)?;
        let mut body = vec![TASKSTATS_CMD_GET, 1, 0, 0];
        let mut mask = cpus.as_bytes().to_vec();
        mask.push(0);
        put_attr(&mut body, TASKSTATS_CMD_ATTR_REGISTER_CPUMASK, &mask);
        // the kernel refuses listeners outside the initial pid/user namespace; read the ack
        sock.send(family, NLM_F_REQUEST | NLM_F_ACK, &body)?;
        let n = sock.recv(&mut rb, 0)?;
        for (t, p) in messages(&rb[..n]) {
            if t == NLMSG_ERROR {
                let errno = p
                    .get(..4)
                    .map(|b| i32::from_ne_bytes(b.try_into().unwrap()))
                    .unwrap_or(0);
                if errno != 0 {
                    return Err(io::Error::from_raw_os_error(-errno));
                }
            }
        }
        Ok(Taskstats { sock, family })
    }

    pub fn exits(&self) -> (Vec<TaskExit>, bool) {
        let (dgrams, overflow) = self.sock.drain();
        let mut v = Vec::new();
        for d in &dgrams {
            for (t, p) in messages(d) {
                if t == self.family {
                    v.extend(parse_ts_msg(p));
                }
            }
        }
        (v, overflow)
    }
}

pub struct InetDiag {
    sock: NlSock,
}

impl InetDiag {
    pub fn open() -> io::Result<InetDiag> {
        Ok(InetDiag {
            sock: NlSock::open(NETLINK_SOCK_DIAG, 1 << 20)?,
        })
    }

    /// Every TCP and UDP socket, v4 and v6.
    pub fn sockets(&mut self) -> io::Result<Vec<Sock>> {
        let mut v = Vec::new();
        for family in [libc::AF_INET as u8, libc::AF_INET6 as u8] {
            for proto in [libc::IPPROTO_TCP as u8, libc::IPPROTO_UDP as u8] {
                let mut req = vec![0u8; 56];
                req[0] = family;
                req[1] = proto;
                req[4..8].copy_from_slice(&u32::MAX.to_ne_bytes()); // all states
                for p in self
                    .sock
                    .dump(SOCK_DIAG_BY_FAMILY, &req, SOCK_DIAG_BY_FAMILY)?
                {
                    v.extend(parse_diag(&p, proto));
                }
            }
        }
        Ok(v)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn ct_payload(
        id: u32,
        src: [u8; 4],
        dst: [u8; 4],
        proto: u8,
        sport: u16,
        dport: u16,
        pkts: (u64, u64),
    ) -> Vec<u8> {
        fn tuple(src: [u8; 4], dst: [u8; 4], proto: u8, sport: u16, dport: u16) -> Vec<u8> {
            let mut ip = Vec::new();
            put_attr(&mut ip, CTA_IP_V4_SRC, &src);
            put_attr(&mut ip, CTA_IP_V4_DST, &dst);
            let mut pr = Vec::new();
            put_attr(&mut pr, CTA_PROTO_NUM, &[proto]);
            put_attr(&mut pr, CTA_PROTO_SRC_PORT, &sport.to_be_bytes());
            put_attr(&mut pr, CTA_PROTO_DST_PORT, &dport.to_be_bytes());
            let mut t = Vec::new();
            put_attr(&mut t, CTA_TUPLE_IP | 0x8000, &ip);
            put_attr(&mut t, CTA_TUPLE_PROTO | 0x8000, &pr);
            t
        }
        fn ctr(p: u64) -> Vec<u8> {
            let mut c = Vec::new();
            put_attr(&mut c, CTA_COUNTERS_PACKETS, &p.to_be_bytes());
            put_attr(&mut c, CTA_COUNTERS_BYTES, &(p * 100).to_be_bytes());
            c
        }
        let mut m = vec![2u8, 0, 0, 0];
        put_attr(
            &mut m,
            CTA_TUPLE_ORIG | 0x8000,
            &tuple(src, dst, proto, sport, dport),
        );
        put_attr(
            &mut m,
            CTA_TUPLE_REPLY | 0x8000,
            &tuple(dst, src, proto, dport, sport),
        );
        put_attr(&mut m, CTA_COUNTERS_ORIG | 0x8000, &ctr(pkts.0));
        put_attr(&mut m, CTA_COUNTERS_REPLY | 0x8000, &ctr(pkts.1));
        put_attr(&mut m, CTA_ID, &id.to_be_bytes());
        m
    }

    #[test]
    fn ct_roundtrip() {
        let p = ct_payload(77, [10, 1, 2, 3], [8, 8, 8, 8], 17, 40000, 53, (2, 1));
        let f = parse_ct(&p).unwrap();
        assert_eq!(f.id, 77);
        assert_eq!(f.orig.dst, "8.8.8.8".parse::<IpAddr>().unwrap());
        assert_eq!((f.orig.proto, f.orig.sport, f.orig.dport), (17, 40000, 53));
        assert_eq!(f.reply.sport, 53);
        assert_eq!((f.pkts, f.bytes), (3, 300));
        let m = nlmsg(CT_DELETE_TYPE, 0, 1, &p);
        let msgs = messages(&m);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].0, CT_DELETE_TYPE);
    }

    #[test]
    fn taskstats_layout() {
        let mut st = vec![0u8; 432];
        st[0..2].copy_from_slice(&10u16.to_ne_bytes());
        st[80..83].copy_from_slice(b"awk");
        st[128..132].copy_from_slice(&4242u32.to_ne_bytes());
        st[132..136].copy_from_slice(&1000u32.to_ne_bytes());
        st[152..160].copy_from_slice(&30u64.to_ne_bytes());
        st[160..168].copy_from_slice(&12u64.to_ne_bytes());
        st[272..280].copy_from_slice(&5u64.to_ne_bytes());
        st[280..288].copy_from_slice(&2u64.to_ne_bytes());
        let mut nested = Vec::new();
        put_attr(&mut nested, TASKSTATS_TYPE_PID, &4242u32.to_ne_bytes());
        put_attr(&mut nested, TASKSTATS_TYPE_STATS, &st);
        let mut msg = vec![2u8, 1, 0, 0];
        put_attr(&mut msg, TASKSTATS_TYPE_AGGR_PID | 0x8000, &nested);
        let v = parse_ts_msg(&msg);
        assert_eq!(v.len(), 1);
        let e = &v[0];
        assert_eq!((e.pid, e.ppid, e.ctxt, e.cpu_us), (4242, 1000, 7, 42));
        assert_eq!(e.comm, "awk");
        assert!(!e.tgid_aggregate);
        assert!(parse_taskstats(&st[..200]).is_none());
    }

    #[test]
    fn diag_msg() {
        let mut p = vec![0u8; 72];
        p[0] = libc::AF_INET as u8;
        p[4..6].copy_from_slice(&41641u16.to_be_bytes());
        p[6..8].copy_from_slice(&3478u16.to_be_bytes());
        p[8..12].copy_from_slice(&[0, 0, 0, 0]);
        p[24..28].copy_from_slice(&[203, 0, 113, 7]);
        p[68..72].copy_from_slice(&9999u32.to_ne_bytes());
        let s = parse_diag(&p, 17).unwrap();
        assert_eq!(
            (s.sport, s.dport, s.inode, s.proto),
            (41641, 3478, 9999, 17)
        );
        assert_eq!(s.dst, "203.0.113.7".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn attrs_survive_garbage() {
        let bad = [8u8, 0, 1, 0, 1, 2];
        assert_eq!(attrs(&bad).count(), 0);
    }
}
