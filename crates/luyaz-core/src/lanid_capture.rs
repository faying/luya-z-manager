//! Step 0 of the LAN client identification (docs/designs/lan-client-id.md §5 step 0): record
//! only. While the opt-in flag file [`FLAG`] exists (and is younger than its `hours=`, default
//! 48), a thread named `lanid` keeps one AF_PACKET socket on br-lan with a classic BPF that lets
//! through only UDP to port 67, to/from 5353, to 1900, to 137 (DHCP requests, mDNS, SSDP,
//! NetBIOS name service), and writes the frames as they are into a pcap ring in tmpfs
//! ([`DIR`]: two files of the flag's `mb=` each, default [`FILE_MB`], mode 0600). Nothing here parses a packet: the
//! files go to the Mac (`scripts/lanid/analyze.py`). The device's own frames are skipped.
//!
//! - The same thread and socket also feed the live path (T3/T4, [`crate::lanid`]): whenever the
//!   lanid-parse component loads, frames are batched (up to [`crate::lanid::QUEUE`], whatever is
//!   queued after the first) and parsed there; the capture is then an extra, still opt-in.
//! - No component and no flag at core start: no thread, no socket. Turning the capture on takes a
//!   core restart; removing the flag or the time running out stops it within [`WAKE`] (the
//!   thread goes on when the live path runs). With the capture off the thread wakes only for
//!   packets, and once an hour when idle (to write what it learned).
//! - The filter is attached before the bind, so nothing unfiltered is ever queued; if it can't
//!   be attached the capture stays off (never an unfiltered socket).
//! - `join=1` in the flag also joins 224.0.0.251 and 239.255.255.250 on br-lan (a UDP socket's
//!   IP_ADD_MEMBERSHIP: this sends IGMP reports, the one thing here that transmits); default off
//!   so the two can be compared (§4a).
//! - Side-by-side builds (`luyaz-core.test`) never capture ([`crate::svcwatch::marks`]).
//! - Status: [`DIR`]/status.json at most once a [`WAKE`], at each rotation and at the end.
//! - CPU: the thread's own utime/stime are in the status (also `/proc/<pid>/task/*/comm`).

use serde_json::json;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const FLAG: &str = "/data/luyaz/core/lanid-capture";
pub const DIR: &str = "/tmp/luyaz/lanid";
pub const IFACE: &str = "br-lan";
/// Default size of each of the two files (4 MB of tmpfs); the flag's `mb=` overrides it.
pub const FILE_MB: u64 = 2;
/// Largest `mb=` a flag may ask for (64 MB of tmpfs).
pub const MAX_FILE_MB: u64 = 32;
pub const SNAPLEN: u32 = 1514;
pub const RCVBUF: i32 = 64 << 10;
pub const WAKE: Duration = Duration::from_secs(60);
pub const DEFAULT_HOURS: u64 = 48;
/// Longest a flag may ask for.
pub const MAX_HOURS: u64 = 7 * 24;

/// What the flag file asks for: `join=0|1`, `hours=N`, `mb=N` (per file), one per line;
/// anything else ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Want {
    pub join: bool,
    pub hours: u64,
    pub mb: u64,
}

pub fn parse_flag(text: &str) -> Want {
    let mut w = Want {
        join: false,
        hours: DEFAULT_HOURS,
        mb: FILE_MB,
    };
    for l in text.lines() {
        match l.trim().split_once('=') {
            Some(("join", v)) => w.join = v.trim() == "1",
            Some(("hours", v)) => {
                if let Ok(h) = v.trim().parse::<u64>() {
                    w.hours = h.clamp(1, MAX_HOURS);
                }
            }
            Some(("mb", v)) => {
                if let Ok(m) = v.trim().parse::<u64>() {
                    w.mb = m.clamp(1, MAX_FILE_MB);
                }
            }
            _ => {}
        }
    }
    w
}

/// Whether a flag written at `mtime` with `want` still holds at `now` (device seconds).
pub fn live(mtime: u64, want: Want, now: u64) -> bool {
    now.saturating_sub(mtime) < want.hours * 3600
}

/// The classic BPF, from `tcpdump -y EN10MB -dd 'udp and (dst port 67 or port 5353 or dst port
/// 1900 or dst port 137)'` (tcpdump 4.99, alpine), with the accept length changed from 262144
/// to [`SNAPLEN`]. tcpdump handles the IPv4 header length, fragments and IPv6.
pub const BPF: [(u16, u8, u8, u32); 26] = [
    (0x28, 0, 0, 0x0000000c),
    (0x15, 0, 12, 0x00000800),
    (0x30, 0, 0, 0x00000017),
    (0x15, 0, 21, 0x00000011),
    (0x28, 0, 0, 0x00000014),
    (0x45, 19, 0, 0x00001fff),
    (0xb1, 0, 0, 0x0000000e),
    (0x48, 0, 0, 0x00000010),
    (0x15, 15, 0, 0x00000043),
    (0x15, 14, 0, 0x000014e9),
    (0x15, 13, 0, 0x0000076c),
    (0x15, 12, 0, 0x00000089),
    (0x48, 0, 0, 0x0000000e),
    (0x15, 10, 11, 0x000014e9),
    (0x15, 0, 10, 0x000086dd),
    (0x30, 0, 0, 0x00000014),
    (0x15, 0, 8, 0x00000011),
    (0x28, 0, 0, 0x00000038),
    (0x15, 5, 0, 0x00000043),
    (0x15, 4, 0, 0x000014e9),
    (0x15, 3, 0, 0x0000076c),
    (0x15, 2, 0, 0x00000089),
    (0x28, 0, 0, 0x00000036),
    (0x15, 0, 1, 0x000014e9),
    (0x6, 0, 0, SNAPLEN),
    (0x6, 0, 0, 0x00000000),
];

/// The pcap global header (little endian, microseconds, Ethernet).
pub fn pcap_header() -> [u8; 24] {
    let mut h = [0u8; 24];
    h[0..4].copy_from_slice(&0xa1b2_c3d4u32.to_le_bytes());
    h[4..6].copy_from_slice(&2u16.to_le_bytes());
    h[6..8].copy_from_slice(&4u16.to_le_bytes());
    h[16..20].copy_from_slice(&SNAPLEN.to_le_bytes());
    h[20..24].copy_from_slice(&1u32.to_le_bytes());
    h
}

/// One pcap record header; `orig` is the frame's length on the wire.
pub fn record_header(sec: u64, usec: u32, caplen: u32, orig: u32) -> [u8; 16] {
    let mut h = [0u8; 16];
    h[0..4].copy_from_slice(&(sec as u32).to_le_bytes());
    h[4..8].copy_from_slice(&usec.to_le_bytes());
    h[8..12].copy_from_slice(&caplen.to_le_bytes());
    h[12..16].copy_from_slice(&orig.to_le_bytes());
    h
}

/// The ring: `cap.pcap` is written; past its cap it becomes `cap.old.pcap` (the one before
/// is lost, its packets counted in `overwritten`) and a new `cap.pcap` starts.
pub struct Ring {
    dir: PathBuf,
    cap: u64,
    file: Option<std::fs::File>,
    size: u64,
    /// Packets in the current file / the old one.
    cur_pkts: u64,
    old_pkts: u64,
    pub packets: u64,
    pub bytes: u64,
    pub overwritten: u64,
    pub rotations: u64,
}

impl Ring {
    pub fn new(dir: &Path, cap: u64) -> Ring {
        Ring {
            dir: dir.to_owned(),
            cap,
            file: None,
            size: 0,
            cur_pkts: 0,
            old_pkts: 0,
            packets: 0,
            bytes: 0,
            overwritten: 0,
            rotations: 0,
        }
    }

    fn open_new(&mut self) -> std::io::Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::create_dir_all(&self.dir)?;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(self.dir.join("cap.pcap"))?;
        f.write_all(&pcap_header())?;
        self.file = Some(f);
        self.size = 24;
        self.cur_pkts = 0;
        Ok(())
    }

    /// Appends one frame; returns whether it rotated.
    pub fn push(&mut self, sec: u64, usec: u32, frame: &[u8], orig: u32) -> std::io::Result<bool> {
        let mut rotated = false;
        if self.file.is_some() && self.size + 16 + frame.len() as u64 > self.cap {
            self.file = None;
            // gone already (pulled and deleted by hand): just start a new one
            let _ = std::fs::rename(self.dir.join("cap.pcap"), self.dir.join("cap.old.pcap"));
            self.overwritten += self.old_pkts;
            self.old_pkts = self.cur_pkts;
            self.rotations += 1;
            rotated = true;
        }
        if self.file.is_none() {
            self.open_new()?;
        }
        let f = self.file.as_mut().expect("opened above");
        f.write_all(&record_header(sec, usec, frame.len() as u32, orig))?;
        f.write_all(frame)?;
        self.size += 16 + frame.len() as u64;
        self.cur_pkts += 1;
        self.packets += 1;
        self.bytes += frame.len() as u64;
        Ok(rotated)
    }
}

/// Fields 14 and 15 (utime, stime, clock ticks) of a `/proc/.../stat` line.
pub fn cpu_ticks(stat: &str) -> Option<(u64, u64)> {
    let rest = &stat[stat.rfind(')')? + 1..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    // rest starts at field 3 (state)
    Some((f.get(11)?.parse().ok()?, f.get(12)?.parse().ok()?))
}

/// Starts the `lanid` thread: always for the live path when the lanid-parse component loads
/// ([`crate::lanid`]), and for the capture when the flag is there (see the module notes). One
/// socket serves both. Side-by-side builds start neither.
pub fn start(argv0: &str) {
    let main = crate::svcwatch::marks(argv0);
    let parser = crate::lanid::start(
        Path::new(&crate::state::host_path(crate::lanid::DIR)),
        main,
        &crate::plugin::dir(),
    );
    if !main {
        return;
    }
    let flag = PathBuf::from(crate::state::host_path(FLAG));
    if parser.is_none() && !flag.exists() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("lanid".into())
        .spawn(move || {
            let dir = PathBuf::from(crate::state::host_path(DIR));
            let end = imp::run(&flag, &dir, parser);
            eprintln!("lanid: stopped ({end})");
        });
}

fn flag_state(flag: &Path) -> Option<(Want, u64)> {
    let m = std::fs::metadata(flag).ok()?;
    let mtime = m
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some((
        parse_flag(&std::fs::read_to_string(flag).unwrap_or_default()),
        mtime,
    ))
}

fn write_status(dir: &Path, v: &serde_json::Value) {
    let tmp = dir.join("status.json.tmp");
    if std::fs::write(&tmp, v.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, dir.join("status.json"));
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    const SOL_PACKET: libc::c_int = 263;
    const PACKET_STATISTICS: libc::c_int = 6;
    const PACKET_OUTGOING: u8 = 4;
    const SO_ATTACH_FILTER: libc::c_int = 26;

    #[repr(C)]
    struct SockFilter {
        code: u16,
        jt: u8,
        jf: u8,
        k: u32,
    }
    #[repr(C)]
    struct SockFprog {
        len: u16,
        filter: *const SockFilter,
    }
    #[repr(C)]
    #[derive(Default)]
    struct TpacketStats {
        packets: u32,
        drops: u32,
    }
    #[repr(C)]
    struct IpMreqn {
        multiaddr: u32,
        address: u32,
        ifindex: libc::c_int,
    }

    fn err(what: &str) -> String {
        format!("{what}: {}", std::io::Error::last_os_error())
    }

    fn setopt<T>(fd: libc::c_int, level: libc::c_int, name: libc::c_int, v: &T) -> bool {
        // SAFETY: v points at a live T of the size given.
        unsafe {
            libc::setsockopt(
                fd,
                level,
                name,
                (v as *const T).cast(),
                size_of::<T>() as libc::socklen_t,
            ) == 0
        }
    }

    /// The filtered socket, bound to `ifindex`; the filter goes on before the bind.
    fn open(ifindex: libc::c_int) -> Result<OwnedFd, String> {
        // protocol 0: nothing is queued until the bind below
        // SAFETY: plain socket(2).
        let fd = unsafe { libc::socket(libc::AF_PACKET, libc::SOCK_RAW | libc::SOCK_CLOEXEC, 0) };
        if fd < 0 {
            return Err(err("socket"));
        }
        // SAFETY: fd was just returned by socket(2) and is owned only here.
        let sock = unsafe { OwnedFd::from_raw_fd(fd) };
        let prog: Vec<SockFilter> = BPF
            .iter()
            .map(|&(code, jt, jf, k)| SockFilter { code, jt, jf, k })
            .collect();
        let fprog = SockFprog {
            len: prog.len() as u16,
            filter: prog.as_ptr(),
        };
        if !setopt(fd, libc::SOL_SOCKET, SO_ATTACH_FILTER, &fprog) {
            return Err(err("attach filter"));
        }
        let _ = setopt(fd, libc::SOL_SOCKET, libc::SO_RCVBUF, &RCVBUF);
        let tv = libc::timeval {
            tv_sec: WAKE.as_secs() as _,
            tv_usec: 0,
        };
        let _ = setopt(fd, libc::SOL_SOCKET, libc::SO_RCVTIMEO, &tv);
        // SAFETY: zeroed sockaddr_ll is valid; filled below.
        let mut sll: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
        sll.sll_family = libc::AF_PACKET as u16;
        sll.sll_protocol = (libc::ETH_P_ALL as u16).to_be();
        sll.sll_ifindex = ifindex;
        // SAFETY: sll is a valid sockaddr_ll of the size given.
        let r = unsafe {
            libc::bind(
                fd,
                (&sll as *const libc::sockaddr_ll).cast(),
                size_of::<libc::sockaddr_ll>() as libc::socklen_t,
            )
        };
        if r != 0 {
            return Err(err("bind"));
        }
        Ok(sock)
    }

    /// A UDP socket holding the two multicast groups on `ifindex` (sends IGMP joins).
    fn join(ifindex: libc::c_int) -> Result<OwnedFd, String> {
        // SAFETY: plain socket(2).
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
        if fd < 0 {
            return Err(err("udp socket"));
        }
        // SAFETY: fd was just returned by socket(2).
        let sock = unsafe { OwnedFd::from_raw_fd(fd) };
        for g in [[224u8, 0, 0, 251], [239, 255, 255, 250]] {
            let m = IpMreqn {
                multiaddr: u32::from_ne_bytes(g),
                address: 0,
                ifindex,
            };
            if !setopt(fd, libc::IPPROTO_IP, libc::IP_ADD_MEMBERSHIP, &m) {
                return Err(err("join"));
            }
        }
        Ok(sock)
    }

    fn kernel_stats(fd: libc::c_int) -> TpacketStats {
        let mut s = TpacketStats::default();
        let mut len = size_of::<TpacketStats>() as libc::socklen_t;
        // SAFETY: s is a valid tpacket_stats buffer of len bytes (the read also resets them).
        unsafe {
            libc::getsockopt(
                fd,
                SOL_PACKET,
                PACKET_STATISTICS,
                (&mut s as *mut TpacketStats).cast(),
                &mut len,
            )
        };
        s
    }

    /// The capture's own state while the flag holds.
    struct Cap {
        want: Want,
        expires: u64,
        ring: Ring,
        joined: Option<Result<OwnedFd, String>>,
        kpackets: u64,
        kdrops: u64,
        outgoing: u64,
        errors: u64,
        last_status: u64,
        started: u64,
    }

    fn set_timeout(fd: libc::c_int, d: Duration) {
        let tv = libc::timeval {
            tv_sec: d.as_secs() as _,
            tv_usec: 0,
        };
        let _ = setopt(fd, libc::SOL_SOCKET, libc::SO_RCVTIMEO, &tv);
    }

    pub fn run(flag: &Path, dir: &Path, parser: Option<crate::lanid::Component>) -> String {
        let started = crate::qmi_nas::now();
        let cap_off = |e: &str| {
            let _ = std::fs::create_dir_all(dir);
            write_status(dir, &json!({"state": "off", "error": e, "t": started}));
        };
        let want_cap = match flag_state(flag) {
            Some((w, m)) if live(m, w, started) => Some((w, m)),
            Some(_) => {
                cap_off("flag expired");
                None
            }
            None => None,
        };
        if want_cap.is_none() && parser.is_none() {
            return "nothing to do".into();
        }
        let fail = |e: String| {
            if want_cap.is_some() {
                cap_off(&e);
            }
            if parser.is_some() {
                crate::lanid::running(Some(e.clone()));
            }
            e
        };
        let name = std::ffi::CString::new(IFACE).expect("no NUL");
        // SAFETY: name is a valid C string.
        let ifindex = unsafe { libc::if_nametoindex(name.as_ptr()) } as libc::c_int;
        if ifindex == 0 {
            return fail(err("if_nametoindex"));
        }
        // never unfiltered: no BPF, no socket, all sources off
        let sock = match open(ifindex) {
            Ok(s) => s,
            Err(e) => return fail(e),
        };
        if parser.is_some() {
            crate::lanid::running(None);
        }
        let fd = sock.as_raw_fd();
        let mut cap = want_cap.map(|(want, mtime)| Cap {
            want,
            expires: mtime + want.hours * 3600,
            ring: Ring::new(dir, want.mb << 20),
            joined: want.join.then(|| join(ifindex)),
            kpackets: 0,
            kdrops: 0,
            outgoing: 0,
            errors: 0,
            last_status: 0,
            started,
        });
        // the capture looks at its flag every WAKE; the live path alone only wakes for packets
        // (and once an hour, to write what it learned while idle)
        set_timeout(
            fd,
            if cap.is_some() {
                WAKE
            } else {
                crate::lanid::FLUSH_EVERY
            },
        );
        let mut gate = crate::lanid::Gate::default();
        let mut batch: Vec<Vec<u8>> = Vec::new();
        let mut buf = vec![0u8; 65536];
        // SAFETY: gettid(2) takes no arguments.
        let tid = unsafe { libc::syscall(libc::SYS_gettid) };
        let tstat = format!("/proc/self/task/{tid}/stat");
        let end = loop {
            // the first frame blocks; then whatever else is queued, up to the batch size
            let mut got = 0usize;
            let mut idle = false;
            while got < crate::lanid::QUEUE {
                // SAFETY: zeroed sockaddr_ll is valid output space.
                let mut from: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
                let mut flen = size_of::<libc::sockaddr_ll>() as libc::socklen_t;
                let flags = libc::MSG_TRUNC | if got > 0 { libc::MSG_DONTWAIT } else { 0 };
                // MSG_TRUNC: the return value is the frame's real length
                // SAFETY: buf and from are valid for the lengths given.
                let n = unsafe {
                    libc::recvfrom(
                        fd,
                        buf.as_mut_ptr().cast(),
                        buf.len(),
                        flags,
                        (&mut from as *mut libc::sockaddr_ll).cast(),
                        &mut flen,
                    )
                };
                if n < 0 {
                    let e = std::io::Error::last_os_error();
                    if got == 0 {
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) {
                            idle = true;
                        } else if let Some(c) = cap.as_mut() {
                            c.errors += 1;
                        }
                    }
                    break;
                }
                got += 1;
                if from.sll_pkttype == PACKET_OUTGOING {
                    if let Some(c) = cap.as_mut() {
                        c.outgoing += 1;
                    }
                    continue;
                }
                let orig = n as usize;
                let len = orig.min(buf.len()).min(SNAPLEN as usize);
                let frame = &buf[..len];
                if let Some(c) = cap.as_mut() {
                    let usec = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.subsec_micros());
                    match c.ring.push(crate::qmi_nas::now(), usec, frame, orig as u32) {
                        // a rotation writes the status (below) right away
                        Ok(true) => c.last_status = 0,
                        Ok(false) => {}
                        Err(_) => c.errors += 1,
                    }
                }
                if parser.is_some() {
                    batch.push(frame.to_vec());
                }
                if cap.is_some() && parser.is_none() {
                    // the capture alone: one frame per wake, as before
                    break;
                }
            }
            if let Some(p) = parser.as_ref() {
                if !batch.is_empty() {
                    crate::lanid::process(&mut gate, p, std::mem::take(&mut batch));
                } else if idle {
                    crate::lanid::flush(false);
                }
            }
            let Some(c) = cap.as_mut() else {
                continue;
            };
            let now = crate::qmi_nas::now();
            // the flag is looked at on the status cadence only, not per packet
            if now.saturating_sub(c.last_status) < WAKE.as_secs() {
                continue;
            }
            let stop = match flag_state(flag) {
                None => Some("flag removed"),
                Some((w, m)) if !live(m, w, now) => Some("flag expired"),
                Some(_) => None,
            };
            let k = kernel_stats(fd);
            c.kpackets += k.packets as u64;
            c.kdrops += k.drops as u64;
            let cpu = cpu_ticks(&std::fs::read_to_string(&tstat).unwrap_or_default());
            let join_state = match &c.joined {
                None => json!(false),
                Some(Ok(_)) => json!(true),
                Some(Err(e)) => json!(e),
            };
            write_status(
                dir,
                &json!({
                    "state": if stop.is_some() { "off" } else { "on" },
                    "stopped": stop,
                    "t": now, "started": c.started, "expires": c.expires,
                    "iface": IFACE, "join": join_state, "file_mb": c.want.mb,
                    "packets": c.ring.packets, "bytes": c.ring.bytes,
                    "overwritten": c.ring.overwritten, "rotations": c.ring.rotations,
                    "outgoing_skipped": c.outgoing, "write_errors": c.errors,
                    "kernel_packets": c.kpackets, "kernel_drops": c.kdrops,
                    "cpu_ticks": cpu.map(|(u, s)| json!({"utime": u, "stime": s})),
                }),
            );
            c.last_status = now;
            if let Some(s) = stop {
                // the capture ends (its multicast membership with it); the live path goes on
                cap = None;
                if parser.is_none() {
                    break s.to_owned();
                }
                eprintln!("lanid capture: stopped ({s})");
                set_timeout(fd, crate::lanid::FLUSH_EVERY);
            }
        };
        drop(cap);
        drop(sock);
        end
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::*;
    pub fn run(_flag: &Path, dir: &Path, parser: Option<crate::lanid::Component>) -> String {
        let _ = std::fs::create_dir_all(dir);
        write_status(dir, &json!({"state": "off", "error": "not linux"}));
        if parser.is_some() {
            crate::lanid::running(Some("not linux".into()));
        }
        "not linux".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_defaults_and_values() {
        assert_eq!(
            parse_flag(""),
            Want {
                join: false,
                hours: 48,
                mb: FILE_MB
            }
        );
        assert_eq!(
            parse_flag("join=1\nhours=24\n"),
            Want {
                join: true,
                hours: 24,
                mb: FILE_MB
            }
        );
        assert_eq!(
            parse_flag("hours=0"),
            Want {
                join: false,
                hours: 1,
                mb: FILE_MB
            }
        );
        assert_eq!(
            parse_flag("hours=9999\njoin=yes"),
            Want {
                join: false,
                hours: MAX_HOURS,
                mb: FILE_MB
            }
        );
    }

    #[test]
    fn flag_ring_size() {
        assert_eq!(parse_flag("mb=8").mb, 8);
        assert_eq!(parse_flag("mb=0").mb, 1);
        assert_eq!(parse_flag("mb=999").mb, MAX_FILE_MB);
        assert_eq!(parse_flag("mb=x").mb, FILE_MB);
    }

    #[test]
    fn flag_runs_out() {
        let w = parse_flag("hours=2");
        assert!(live(1000, w, 1000 + 7199));
        assert!(!live(1000, w, 1000 + 7200));
        // a clock behind the flag's time is still live
        assert!(live(1000, w, 10));
    }

    #[test]
    fn bpf_is_tcpdumps_with_our_snaplen() {
        assert_eq!(BPF.len(), 26);
        assert_eq!(BPF[24], (0x6, 0, 0, SNAPLEN));
        assert_eq!(BPF[25], (0x6, 0, 0, 0));
        // every jump lands inside the program
        for (i, &(code, jt, jf, _)) in BPF.iter().enumerate() {
            if code & 0x07 == 0x05 && code != 0x05 {
                assert!(i + 1 + (jt.max(jf) as usize) < BPF.len(), "jump at {i}");
            }
        }
    }

    #[test]
    fn ring_rotates_and_counts() {
        let d = std::env::temp_dir().join(format!("lanid-ring-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        // header 24 + two 116-byte records fit in 300; the third rotates
        let mut r = Ring::new(&d, 300);
        let f = [0xabu8; 100];
        assert!(!r.push(1, 0, &f, 100).unwrap());
        assert!(!r.push(2, 0, &f, 100).unwrap());
        assert!(r.push(3, 0, &f, 100).unwrap());
        assert_eq!((r.packets, r.overwritten, r.rotations), (3, 0, 1));
        assert!(!r.push(4, 0, &f, 100).unwrap());
        assert!(r.push(5, 0, &f, 100).unwrap());
        // the first file's two packets are gone
        assert_eq!((r.packets, r.overwritten, r.rotations), (5, 2, 2));
        let cur = std::fs::read(d.join("cap.pcap")).unwrap();
        assert_eq!(cur.len(), 24 + 116);
        assert_eq!(&cur[..24], &pcap_header());
        assert_eq!(
            std::fs::read(d.join("cap.old.pcap")).unwrap().len(),
            24 + 2 * 116
        );
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(d.join("cap.pcap"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn cpu_ticks_from_stat() {
        let s = "1234 (lan id) S 1 2 3 4 5 6 7 8 9 10 17 23 0 0 20 0";
        assert_eq!(cpu_ticks(s), Some((17, 23)));
        assert_eq!(cpu_ticks("garbage"), None);
    }
}
