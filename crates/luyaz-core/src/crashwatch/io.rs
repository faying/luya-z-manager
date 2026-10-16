//! The crash watcher's IO edge (D11): one thread, `poll()` on `/dev/kmsg`, an rtnetlink socket
//! (IPv4 route and address groups) and an inotify watch on the dump directory. Nothing else wakes
//! it, except the timer the watcher asks for while a crash is open, a dump window runs or a pause
//! waits for evidence.
//!
//! - `/dev/kmsg` (non-blocking): every wake reads to EAGAIN (so a wake of any kind sees the
//!   records before it looks at the link or the dumps). A kmsg wake that comes less than
//!   [`super::INTERVAL_MS`] after the last one masks kmsg out of `poll()` until that much later
//!   (its own deadline), so a burst costs one wake per interval and a lone record one wake.
//!   EPIPE (records overwritten before we read them) is counted and reading goes on.
//! - rtnetlink: messages are never parsed; any message means "look at /proc/net/route again".
//!   Subscribed before the first look. ENOBUFS (overrun) is a gap: the next link event's `res`
//!   is the real time since the previous look.
//! - inotify on the dump directory (create, moved-to, close-write, delete): outside a dump window
//!   it re-lists the baseline. If the directory is missing, the 60 s rule instead.
//! - State: `/tmp/luyaz/core/crashwatch.json` (tmpfs; this boot only), rewritten when it changed.
//!   Events: appended to the long-term layer's `events.jsonl` (`t` filled in when the clock is
//!   trusted); kept in the state's pending list until the append succeeded.
//! - `/data/u60d/crashwatch-init`: the watcher has gone live on this device once (first boot
//!   with it: what the kernel held at start is taken as seen, as the guard's `watch-init`).
//!
//! - Coverage (u60-sentinel.md §19 「T9c 前定」): the long-term thread's probe (an eventfd) is in
//!   the poll set, so the thread wakes at least once a probe period; every wake, after kmsg was
//!   read clean, stamps [`crate::longterm::Shared::alive`]. Lost records (kmsg EPIPE, netlink
//!   ENOBUFS, inotify IN_Q_OVERFLOW) are reported as that route down from the previous wake (its
//!   last clean read) to this one; a route that could not be set up is down from the start.
//!
//! - The kernel log on flash ([`super::kmsgfile`], G8): while the guard's handover file has named
//!   `crashcap` at two looks in a row (the guard stops its own reader the round it names it), its
//!   own `/dev/kmsg` fd, read on every wake after the watcher's; dropped when the name goes.
//!
//! Off unless `LUYAZ_CRASHWATCH=1` (so is the kernel log on flash). It never touches the guard's
//! watcher state.

use super::{Cap, Look, Net, Rec, Wake, Watch, parse_rec};
use crate::block::Hub;
use serde_json::Value;
use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const STATE: &str = "/tmp/luyaz/core/crashwatch.json";
pub const INIT: &str = "/data/luyaz/core/crashwatch-init";
pub const DUMP_DIR: &str = "/data/vendor/ramdump";
const WAN_IF: &str = "rmnet_data0";

const RTMGRP_IPV4_IFADDR: u32 = 0x10;
const RTMGRP_IPV4_ROUTE: u32 = 0x40;

pub fn enabled() -> bool {
    std::env::var("LUYAZ_CRASHWATCH").is_ok_and(|v| v == "1")
}

fn host(p: &str) -> PathBuf {
    PathBuf::from(crate::state::host_path(p))
}

fn clock_ms(id: libc::clockid_t) -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime into a timespec we own.
    unsafe { libc::clock_gettime(id, &mut ts) };
    ts.tv_sec as u64 * 1000 + ts.tv_nsec as u64 / 1_000_000
}

/// The device as the watcher reads it.
pub struct Device {
    pub route: PathBuf,
    pub netdev: PathBuf,
    pub dumps: PathBuf,
    pub suspend: PathBuf,
    pub wan: String,
    pub hub: Option<Arc<Hub>>,
    pub up_ms: u64,
}

/// The main table's default route via `wan` (the guard's `wan_route`).
pub fn route_up(text: &str, wan: &str) -> bool {
    text.lines().skip(1).any(|l| {
        let mut f = l.split_whitespace();
        f.next() == Some(wan) && f.next() == Some("00000000")
    })
}

/// (rx bytes, packets both ways) of `wan` in /proc/net/dev text (the guard's `wan_rx`).
pub fn netdev_of(text: &str, wan: &str) -> (Option<u64>, Option<u64>) {
    for l in text.lines() {
        let Some((name, rest)) = l.split_once(':') else {
            continue;
        };
        if name.trim() != wan {
            continue;
        }
        let v: Vec<Option<u64>> = rest.split_whitespace().map(|x| x.parse().ok()).collect();
        let g = |i: usize| v.get(i).copied().flatten();
        let pk = match (g(1), g(9)) {
            (Some(a), Some(b)) => Some(a + b),
            _ => None,
        };
        return (g(0), pk);
    }
    (None, None)
}

fn has_ipv4(ifname: &str) -> bool {
    let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills a list we free below; we only read it.
    if unsafe { libc::getifaddrs(&mut ifap) } != 0 {
        return false;
    }
    let mut found = false;
    let mut p = ifap;
    while !p.is_null() {
        // SAFETY: p walks the list getifaddrs gave.
        let a = unsafe { &*p };
        if !a.ifa_addr.is_null()
            // SAFETY: ifa_addr is non-null and points at a sockaddr.
            && i32::from(unsafe { (*a.ifa_addr).sa_family }) == libc::AF_INET
            // SAFETY: ifa_name is a C string.
            && unsafe { std::ffi::CStr::from_ptr(a.ifa_name) }.to_bytes() == ifname.as_bytes()
        {
            found = true;
            break;
        }
        p = a.ifa_next;
    }
    // SAFETY: the list from getifaddrs above.
    unsafe { libc::freeifaddrs(ifap) };
    found
}

impl Look for Device {
    fn route(&mut self) -> bool {
        std::fs::read_to_string(&self.route).is_ok_and(|t| route_up(&t, &self.wan))
    }
    fn addr(&mut self) -> bool {
        has_ipv4(&self.wan)
    }
    fn netdev(&mut self) -> (Option<u64>, Option<u64>) {
        std::fs::read_to_string(&self.netdev).map_or((None, None), |t| netdev_of(&t, &self.wan))
    }
    fn dumps(&mut self) -> Option<Vec<(String, u64)>> {
        let mut v = Vec::new();
        for e in std::fs::read_dir(&self.dumps).ok()?.flatten() {
            let Ok(m) = e.metadata() else { continue };
            if m.is_file() {
                v.push((e.file_name().to_string_lossy().into_owned(), m.len()));
            }
        }
        v.sort();
        Some(v)
    }
    fn suspend(&mut self) -> Option<u64> {
        std::fs::read_to_string(&self.suspend)
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }
    fn net(&mut self) -> Net {
        let Some(hub) = &self.hub else {
            return Net::default();
        };
        let sig = hub.view("signal").filter(|v| !v.stale);
        let sim = hub.view("sim").filter(|v| !v.stale);
        // observed_at is the device's wall clock: its age in wall seconds, as boottime
        let wall = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let nu = self.up_ms / 1000;
        let at = sig
            .as_ref()
            .map(|v| nu.saturating_sub(wall.saturating_sub(v.observed_at)));
        Net::from_blocks(
            sig.as_ref().map(|v| &v.data),
            sim.as_ref().map(|v| &v.data),
            at,
        )
    }
}

fn open_kmsg(path: &Path) -> Option<OwnedFd> {
    let c = CString::new(path.as_os_str().as_encoded_bytes()).ok()?;
    // SAFETY: open with a valid C string; the fd is owned below.
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    // SAFETY: a fresh fd we own.
    (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Reads every record available now. Returns (records, records lost to an overrun).
fn read_kmsg(fd: RawFd, out: &mut Vec<Rec>) -> u64 {
    let mut raw = Vec::new();
    let lost = read_raw(fd, &mut raw);
    out.extend(raw.iter().filter_map(|r| parse_rec(r)));
    lost
}

/// Every raw record available now, as read (one record per read, with its continuation lines).
fn read_raw(fd: RawFd, out: &mut Vec<String>) -> u64 {
    let mut lost = 0;
    let mut buf = vec![0u8; 8192];
    let mut grown = false;
    loop {
        // SAFETY: read into a buffer we own.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n > 0 {
            out.push(String::from_utf8_lossy(&buf[..n as usize]).into_owned());
            continue;
        }
        if n == 0 {
            return lost;
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EPIPE) => lost += 1, // overwritten; the next read gives the oldest left
            // a record bigger than the buffer: the reader does not move on, so try once with
            // the largest record the kernel makes, else stop for this wake (never loop on it)
            Some(libc::EINVAL) if !grown => {
                grown = true;
                buf.resize(65536, 0);
            }
            _ => return lost, // EAGAIN: nothing more
        }
    }
}

/// The kernel log on flash while the guard has handed it over (G8).
#[derive(Default)]
struct Capture {
    /// Looks in a row that found `crashcap` named (two before starting).
    named: u8,
    looked_ms: Option<u64>,
    on: Option<(super::kmsgfile::KmsgFile, OwnedFd)>,
    /// A start that failed: logged once until it works.
    failed: bool,
}

/// `--longterm-once`: this boot's kernel log into `dir` as the capture would write it, twice
/// (the second start must add nothing that is already there), and how it compares with the
/// guard's file.
pub fn capture_once(dir: &Path, boot: &str) -> String {
    let mut out = String::new();
    for pass in 1..=2 {
        let k = super::kmsgfile::KmsgFile::start(
            dir,
            boot,
            super::kmsgfile::KEEP,
            super::kmsgfile::MAX,
        );
        let (mut k, fd) = match (k, open_kmsg(&host("/dev/kmsg"))) {
            (Ok(k), Some(fd)) => (k, fd),
            (k, _) => {
                out.push_str(&format!(
                    "crashcap pass {pass}: cannot start ({})\n",
                    k.err().map_or("no /dev/kmsg".into(), |e| e.to_string())
                ));
                return out;
            }
        };
        let mut raw = Vec::new();
        let lost = read_raw(fd.as_raw_fd(), &mut raw);
        let text: String = raw.iter().map(|r| k.take(r)).collect();
        let r = k.append(&text);
        out.push_str(&format!(
            "crashcap pass {pass}: {} records read, {} lost, {} bytes added ({r:?}), file {}\n",
            raw.len(),
            lost,
            text.len(),
            k.file.display()
        ));
    }
    let name = format!("kmsg-{}.log", boot.chars().take(8).collect::<String>());
    let ours = std::fs::read_to_string(dir.join(&name)).unwrap_or_default();
    let guards =
        std::fs::read_to_string(host(super::kmsgfile::DIR).join(&name)).unwrap_or_default();
    let set: std::collections::HashSet<&str> = guards.lines().collect();
    let missing = ours.lines().filter(|l| !set.contains(l)).count();
    out.push_str(&format!(
        "crashcap ours {} lines, guard's {} lines, ours not in the guard's {missing}\n",
        ours.lines().count(),
        guards.lines().count()
    ));
    out
}

/// How often the handover file is looked at (on a wake; the long-term probe wakes us each minute).
const CAPTURE_LOOK_MS: u64 = 30_000;

impl Capture {
    fn wake(&mut self, mono_ms: u64, boot: &str) {
        if self
            .looked_ms
            .is_none_or(|t| mono_ms.saturating_sub(t) >= CAPTURE_LOOK_MS)
        {
            self.looked_ms = Some(mono_ms);
            let t = std::fs::read_to_string(host(super::kmsgfile::HANDOVER)).unwrap_or_default();
            if luyaz_api::handover::names(&t, super::kmsgfile::NAME) {
                self.named = (self.named + 1).min(2);
            } else {
                self.named = 0;
                if self.on.take().is_some() {
                    eprintln!("[crashwatch] kernel log to flash: handed back to the guard");
                }
            }
            if self.named >= 2 && self.on.is_none() {
                let k = super::kmsgfile::KmsgFile::start(
                    &host(super::kmsgfile::DIR),
                    boot,
                    super::kmsgfile::KEEP,
                    super::kmsgfile::MAX,
                );
                match (k, open_kmsg(&host("/dev/kmsg"))) {
                    (Ok(k), Some(fd)) => {
                        eprintln!("[crashwatch] kernel log to flash: {}", k.file.display());
                        self.on = Some((k, fd));
                        self.failed = false;
                    }
                    (k, _) => {
                        if !self.failed {
                            self.failed = true;
                            eprintln!(
                                "[crashwatch] kernel log to flash: cannot start ({})",
                                k.err().map_or("no /dev/kmsg".into(), |e| e.to_string())
                            );
                        }
                    }
                }
            }
        }
        let Some((k, fd)) = self.on.as_mut() else {
            return;
        };
        let mut raw = Vec::new();
        read_raw(fd.as_raw_fd(), &mut raw);
        let text: String = raw.iter().map(|r| k.take(r)).collect();
        match k.append(&text) {
            Ok(true) => eprintln!("[crashwatch] cut {} to its newest half", k.file.display()),
            Ok(false) => {}
            Err(e) => eprintln!("[crashwatch] kernel log to flash: {e}"),
        }
    }
}

fn open_rtnl() -> Option<OwnedFd> {
    // SAFETY: socket/bind with plain values; the fd is owned below.
    unsafe {
        let fd = libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            libc::NETLINK_ROUTE,
        );
        if fd < 0 {
            return None;
        }
        let owned = OwnedFd::from_raw_fd(fd);
        let mut sa: libc::sockaddr_nl = std::mem::zeroed();
        sa.nl_family = libc::AF_NETLINK as u16;
        sa.nl_groups = RTMGRP_IPV4_ROUTE | RTMGRP_IPV4_IFADDR;
        let r = libc::bind(
            fd,
            (&sa as *const libc::sockaddr_nl).cast(),
            std::mem::size_of::<libc::sockaddr_nl>() as u32,
        );
        (r == 0).then_some(owned)
    }
}

/// Drains the socket. Returns (got anything, overrun).
fn drain(fd: RawFd) -> (bool, bool) {
    let mut buf = vec![0u8; 16384];
    let (mut got, mut over) = (false, false);
    loop {
        // SAFETY: recv into a buffer we own.
        let n = unsafe { libc::recv(fd, buf.as_mut_ptr().cast(), buf.len(), libc::MSG_DONTWAIT) };
        if n > 0 {
            got = true;
            continue;
        }
        if n < 0 {
            match std::io::Error::last_os_error().raw_os_error() {
                Some(libc::EINTR) => continue,
                Some(libc::ENOBUFS) => {
                    over = true;
                    continue;
                }
                _ => {}
            }
        }
        return (got, over);
    }
}

/// Whether a buffer of `struct inotify_event`s (wd, mask, cookie, len, name[len]) has one with
/// IN_Q_OVERFLOW (the kernel's queue filled: events were dropped).
pub fn inotify_overflow(buf: &[u8]) -> bool {
    let mut i = 0;
    while i + 16 <= buf.len() {
        let mask = u32::from_ne_bytes([buf[i + 4], buf[i + 5], buf[i + 6], buf[i + 7]]);
        let len = u32::from_ne_bytes([buf[i + 12], buf[i + 13], buf[i + 14], buf[i + 15]]);
        if mask & libc::IN_Q_OVERFLOW != 0 {
            return true;
        }
        i += 16 + len as usize;
    }
    false
}

/// Drains an inotify fd (a plain fd: `read`, not `recv`, which gives ENOTSOCK and would leave
/// it readable for ever). Returns (there was anything, the queue overflowed).
pub fn drain_inotify(fd: RawFd) -> (bool, bool) {
    // big enough for at least one event with the longest name, so a read never fails EINVAL
    let mut buf = vec![0u8; 4096];
    let (mut got, mut over) = (false, false);
    loop {
        // SAFETY: read into a buffer we own.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n > 0 {
            got = true;
            over |= inotify_overflow(&buf[..n as usize]);
            continue;
        }
        if n < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return (got, over);
    }
}

pub fn open_inotify(dir: &Path) -> Option<OwnedFd> {
    let c = CString::new(dir.as_os_str().as_encoded_bytes()).ok()?;
    // SAFETY: inotify_init1 / add_watch with plain values; the fd is owned below.
    unsafe {
        let fd = libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC);
        if fd < 0 {
            return None;
        }
        let owned = OwnedFd::from_raw_fd(fd);
        let mask = libc::IN_CREATE | libc::IN_MOVED_TO | libc::IN_CLOSE_WRITE | libc::IN_DELETE;
        (libc::inotify_add_watch(fd, c.as_ptr(), mask) >= 0).then_some(owned)
    }
}

/// The watcher's events with `t` (the wall clock, when trusted) filled in.
fn stamp(mut v: Vec<Value>, boot: &str) -> Vec<Value> {
    let c = crate::longterm::clock_now(boot);
    for e in &mut v {
        if c.wall_ok
            && let (Some(up), Some(o)) = (e["up"].as_f64(), e.as_object_mut())
        {
            // the event's own moment: now minus how long ago it was, by boottime
            let ago = (c.boottime as f64 - up).max(0.0) as i64;
            o.insert("t".into(), Value::from(c.wall - ago));
        }
    }
    v
}

fn save(path: &Path, w: &Watch, last: &mut String) {
    let Ok(s) = serde_json::to_string(w) else {
        return;
    };
    if s == *last {
        return;
    }
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, &s).is_ok() && std::fs::rename(&tmp, path).is_ok() {
        *last = s;
    }
}

pub fn spawn(hub: Arc<Hub>) {
    let _ = std::thread::Builder::new()
        .name("crashwatch".into())
        .spawn(move || run(Some(hub)));
}

fn run(hub: Option<Arc<Hub>>) {
    let boot = crate::evlog::boot_id();
    let state = host(STATE);
    let init = host(INIT);
    let long = crate::longterm::Store::at(&host(crate::longterm::DIR));
    let Some(kmsg) = open_kmsg(&host("/dev/kmsg")) else {
        eprintln!("[crashwatch] cannot open /dev/kmsg: off");
        return;
    };
    // subscribe before the first look, so a change between the two is not missed
    let rtnl = open_rtnl();
    let dumps = host(DUMP_DIR);
    let ino = open_inotify(&dumps);
    let mut dev = Device {
        route: host("/proc/net/route"),
        netdev: host("/proc/net/dev"),
        dumps,
        suspend: host("/sys/power/suspend_stats/success"),
        wan: WAN_IF.into(),
        hub,
        up_ms: 0,
    };
    let mut w = std::fs::read_to_string(&state)
        .ok()
        .and_then(|s| Watch::resume(&s, &boot))
        .unwrap_or_else(|| Watch::new(&boot, !init.exists()));
    let mut saved = String::new();
    let mut nl_ok = rtnl.is_some();
    let mut first = true;
    let cov = crate::longterm::shared();
    let probe = cov.probe_fd();
    let start_ms = clock_ms(libc::CLOCK_MONOTONIC);
    if rtnl.is_none() {
        cov.set_down("link", Some(start_ms));
    }
    if ino.is_none() {
        cov.set_down("capture", Some(start_ms));
    }
    // the last wake that read everything clean (MONOTONIC ms)
    let mut clean_ms = start_ms;
    let mut cap = Cap::default();
    eprintln!(
        "[crashwatch] started (route subscription {}, dump notify {}, resumed at seq {})",
        if rtnl.is_some() { "on" } else { "off" },
        if ino.is_some() { "on" } else { "off" },
        w.kseq()
    );
    // the spin guard: wakes in a row that read nothing although poll said ready
    let mut empty_wakes = 0u32;
    let mut capf = Capture::default();
    loop {
        let mut dump_changed = first;
        let mut kmsg_woke = false;
        let mut busy = false;
        let mut timer = false;
        let mut nl_got = false;
        let (mut nl_lost, mut ino_lost) = (false, false);
        if !first {
            let now = clock_ms(libc::CLOCK_BOOTTIME);
            let mut deadline = w.next_tick();
            if let Some(m) = cap.deadline() {
                deadline = Some(deadline.map_or(m, |d| d.min(m)));
            }
            let timeout =
                deadline.map_or(-1, |d| d.saturating_sub(now).min(i32::MAX as u64) as i32);
            let masked = cap.masked(now);
            let mut fds = Vec::with_capacity(4);
            let k_i = (!masked).then(|| {
                fds.push(libc::pollfd {
                    fd: kmsg.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                });
                fds.len() - 1
            });
            let r_i = rtnl.as_ref().map(|f| {
                fds.push(libc::pollfd {
                    fd: f.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                });
                fds.len() - 1
            });
            let i_i = ino.as_ref().map(|f| {
                fds.push(libc::pollfd {
                    fd: f.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                });
                fds.len() - 1
            });
            let p_i = probe.map(|fd| {
                fds.push(libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                });
                fds.len() - 1
            });
            // SAFETY: poll over pollfds we own.
            let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
            if n < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            kmsg_woke = k_i.is_some_and(|i| fds[i].revents != 0);
            let probed = p_i.is_some_and(|i| fds[i].revents != 0);
            if probed {
                cov.take_probe();
            }
            if let (Some(i), Some(f)) = (r_i, &rtnl)
                && fds[i].revents != 0
            {
                let (got, over) = drain(f.as_raw_fd());
                nl_got = got || over;
                if over {
                    nl_ok = false;
                    nl_lost = true;
                }
            }
            if let (Some(i), Some(f)) = (i_i, &ino)
                && fds[i].revents != 0
            {
                let (got, over) = drain_inotify(f.as_raw_fd());
                dump_changed |= got;
                ino_lost = over;
            }
            busy = n > 0 && !kmsg_woke && !probed;
            timer = n == 0;
        }
        let up_ms = clock_ms(libc::CLOCK_BOOTTIME);
        let mono_ms = clock_ms(libc::CLOCK_MONOTONIC);
        let mut recs = Vec::new();
        let lost = read_kmsg(kmsg.as_raw_fd(), &mut recs);
        for (src, yes) in [
            ("watcher", lost > 0),
            ("link", nl_lost),
            ("capture", ino_lost),
        ] {
            if yes {
                cov.lost(src, clean_ms, mono_ms);
            }
        }
        cov.alive(mono_ms);
        clean_ms = mono_ms;
        capf.wake(mono_ms, &boot);
        // any fd that stays ready without giving anything (a bug of ours, or an error state)
        // costs one log line and an interval's pause, not a core
        if recs.is_empty() && !dump_changed && !nl_got && !timer && (busy || kmsg_woke) {
            empty_wakes += 1;
            if empty_wakes == 50 {
                eprintln!(
                    "[crashwatch] 50 wakes in a row with nothing to read: pausing between wakes"
                );
            }
            if empty_wakes >= 50 {
                std::thread::sleep(std::time::Duration::from_millis(super::INTERVAL_MS));
            }
        } else {
            empty_wakes = 0;
        }
        cap.after(up_ms, kmsg_woke, !recs.is_empty());
        dev.up_ms = up_ms;
        w.lost += lost;
        let was_base = w.base;
        w.step(
            &Wake {
                up_ms,
                mono_ms,
                recs,
                nl_ok,
                dump_changed,
                dump_notify: ino.is_some(),
            },
            &mut dev,
        );
        nl_ok = rtnl.is_some();
        if was_base {
            let _ = std::fs::create_dir_all(init.parent().unwrap_or(Path::new("/")));
            if std::fs::write(&init, b"").is_ok() {
                eprintln!(
                    "[crashwatch] first go-live: what the kernel held at start taken as seen (seq {})",
                    w.kseq()
                );
            }
        }
        if !w.pending.is_empty() {
            let lines = stamp(w.pending.clone(), &boot);
            match long.append_events(&lines) {
                Ok(()) => {
                    for l in &lines {
                        eprintln!("[crashwatch] {}", l);
                    }
                    w.pending.clear();
                }
                Err(e) => eprintln!(
                    "[crashwatch] events not written yet ({} waiting): {e}",
                    w.pending.len()
                ),
            }
        }
        save(&state, &w, &mut saved);
        first = false;
    }
}
