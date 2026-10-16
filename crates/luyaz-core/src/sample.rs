//! The IO edge: reads /proc, sysfs and netlink into `Snapshot`s. Never forks.

use crate::ledger::{ProcSample, Snapshot, ThreadSample};
use crate::nl::{Conntrack, Flow, InetDiag, Taskstats};
use crate::procfs;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Rescan /proc/*/fd for unknown socket inodes at most this often.
const FD_SCAN_EVERY_MS: u64 = 120_000;
/// Forget a learned remote end after this long without a live socket to it.
const LEARN_KEEP_MS: u64 = 24 * 3_600_000;

pub struct Collector {
    pub proc_root: PathBuf,
    pub sys_root: PathBuf,
    pub wan: String,
    pub ct: Option<Conntrack>,
    pub ts: Option<Taskstats>,
    pub diag: Option<InetDiag>,
    keys: HashMap<(u32, u64), String>,
    inode_pid: HashMap<u32, u32>,
    last_fd_scan: Option<u64>,
    /// DESTROY events drained by the diagnostic window, kept for the resident tick
    destroyed_for_ledger: Vec<Flow>,
    ct_overflow_for_ledger: bool,
    /// remote end → (process key, last seen mono ms)
    learned: HashMap<(u8, IpAddr, u16), (String, u64)>,
    /// processes that held a socket at the last full fd scan
    sock_pids: HashSet<u32>,
    /// (sockets listed, sockets placed on a process, inodes known) of the last read
    pub sock_stats: (usize, usize, usize),
    pub boot: String,
    pub ncpu: u32,
    pub self_pid: u32,
}

pub struct SecondSample {
    pub owners: HashMap<(u8, u16), String>,
    pub peers: HashMap<(u8, IpAddr, u16), String>,
    pub wan_pkts: u64,
    pub flows: Vec<Flow>,
    pub destroyed: Vec<Flow>,
    pub power_uw: Option<i64>,
    pub screen_on: bool,
    pub self_ctxt: u64,
}

fn read(p: &Path) -> Option<String> {
    fs::read_to_string(p).ok()
}

impl Collector {
    pub fn new(wan: &str, event_buf: i32) -> Collector {
        let proc_root = PathBuf::from("/proc");
        let ncpu = read(&proc_root.join("cpuinfo"))
            .map(|s| s.lines().filter(|l| l.starts_with("processor")).count() as u32)
            .unwrap_or(1)
            .max(1);
        let cpus = format!("0-{}", ncpu - 1);
        Collector {
            boot: read(&proc_root.join("sys/kernel/random/boot_id"))
                .map(|s| s.trim().to_string())
                .unwrap_or_default(),
            proc_root,
            sys_root: PathBuf::from("/sys"),
            wan: wan.to_string(),
            ct: Conntrack::open(event_buf)
                .map_err(|e| eprintln!("luyaz-core: conntrack unavailable: {e}"))
                .ok(),
            ts: Taskstats::open(&cpus, event_buf)
                .map_err(|e| eprintln!("luyaz-core: taskstats unavailable: {e}"))
                .ok(),
            diag: InetDiag::open()
                .map_err(|e| eprintln!("luyaz-core: inet_diag unavailable: {e}"))
                .ok(),
            keys: HashMap::new(),
            inode_pid: HashMap::new(),
            last_fd_scan: None,
            destroyed_for_ledger: Vec::new(),
            ct_overflow_for_ledger: false,
            learned: HashMap::new(),
            sock_pids: HashSet::new(),
            sock_stats: (0, 0, 0),
            ncpu,
            self_pid: std::process::id(),
        }
    }

    pub fn mono_ms(&self) -> u64 {
        read(&self.proc_root.join("uptime"))
            .and_then(|s| procfs::uptime_ms(&s))
            .unwrap_or(0)
    }

    fn wall() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }

    fn screen_on(&self) -> bool {
        read(&self.sys_root.join("class/leds/led:lcd/brightness"))
            .and_then(|s| procfs::first_u64(&s))
            .unwrap_or(0)
            > 0
    }

    /// (µW drawn, positive = discharging; capacity %; charging)
    fn battery(&self) -> (Option<i64>, Option<u8>, bool) {
        let b = self.sys_root.join("class/power_supply/battery");
        let v = read(&b.join("voltage_now")).and_then(|s| procfs::first_i64(&s));
        let c = read(&b.join("current_now")).and_then(|s| procfs::first_i64(&s));
        let pct = read(&b.join("capacity"))
            .and_then(|s| procfs::first_u64(&s))
            .map(|x| x.min(100) as u8);
        let charging = read(&b.join("status"))
            .map(|s| s.trim() == "Charging")
            .unwrap_or(false);
        // current_now is negative while discharging (power-sample)
        let uw = match (v, c) {
            (Some(v), Some(c)) => Some((-(v as i128) * c as i128 / 1_000_000) as i64),
            _ => None,
        };
        (uw, pct, charging)
    }

    fn idle_deep_us(&self) -> u64 {
        let mut deep = 0;
        for cpu in 0..self.ncpu {
            let dir = self
                .sys_root
                .join(format!("devices/system/cpu/cpu{cpu}/cpuidle"));
            let Ok(rd) = fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let name = e.file_name();
                let name = name.to_string_lossy();
                if !name.starts_with("state") || name == "state0" {
                    continue;
                }
                deep += read(&e.path().join("time"))
                    .and_then(|s| procfs::first_u64(&s))
                    .unwrap_or(0);
            }
        }
        deep
    }

    fn wan_pkts(&self) -> u64 {
        read(&self.proc_root.join("net/dev"))
            .map(|s| procfs::net_dev(&s))
            .unwrap_or_default()
            .into_iter()
            .find(|d| d.name == self.wan)
            .map(|d| d.rx_pkts + d.tx_pkts)
            .unwrap_or(0)
    }

    fn self_ctxt(&self) -> u64 {
        let dir = self.proc_root.join("self/task");
        fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| {
                        read(&e.path().join("schedstat")).and_then(|s| procfs::schedstat_runs(&s))
                    })
                    .sum()
            })
            .unwrap_or(0)
    }

    /// The resident read.
    pub fn snapshot(&mut self) -> Snapshot {
        let t0 = Instant::now();
        let mut s = Snapshot {
            boot: self.boot.clone(),
            ncpu: self.ncpu,
            self_pid: self.self_pid,
            ..Default::default()
        };

        // exits and DESTROY events first, then the dump and /proc (see ledger ghost rule)
        if let Some(ts) = &self.ts {
            let (e, o) = ts.exits();
            s.exits = e;
            s.exit_overflow = o;
        }
        let mut destroyed = std::mem::take(&mut self.destroyed_for_ledger);
        s.ct_overflow = std::mem::take(&mut self.ct_overflow_for_ledger);
        if let Some(ct) = &self.ct {
            let (d, o) = ct.destroyed();
            destroyed.extend(d);
            s.ct_overflow |= o;
        }
        s.destroyed = destroyed;
        if let Some(ct) = &mut self.ct {
            s.flows = ct
                .dump()
                .map_err(|e| eprintln!("luyaz-core: conntrack dump: {e}"))
                .ok();
        }

        s.mono_ms = self.mono_ms();
        s.wall = Self::wall();
        s.screen_on = self.screen_on();
        (s.power_uw, s.battery_pct, s.charging) = self.battery();
        s.stat = read(&self.proc_root.join("stat"))
            .map(|x| procfs::sys_stat(&x))
            .unwrap_or_default();
        s.idle_deep_us = self.idle_deep_us();
        s.ifaces = read(&self.proc_root.join("net/dev"))
            .map(|x| procfs::net_dev(&x))
            .unwrap_or_default();
        s.irqs = read(&self.proc_root.join("interrupts"))
            .map(|x| procfs::interrupts(&x))
            .unwrap_or_default();
        self.scan_procs(&mut s);
        (s.local_addrs, s.wan_addrs) = addrs(&self.wan);
        if s.flows.is_some() {
            let key_of: HashMap<u32, String> =
                s.procs.iter().map(|p| (p.pid, p.key.clone())).collect();
            (s.owners, s.peers) = self.owners(&key_of, s.mono_ms, true);
        }
        s.self_rss_kb = read(&self.proc_root.join("self/status"))
            .and_then(|x| procfs::status_rss_kb(&x))
            .unwrap_or(0);
        s.collect_ms = t0.elapsed().as_millis() as u64;
        s
    }

    fn scan_procs(&mut self, s: &mut Snapshot) {
        let Ok(rd) = fs::read_dir(&self.proc_root) else {
            return;
        };
        let mut live = HashSet::new();
        for e in rd.flatten() {
            let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else {
                continue;
            };
            let dir = e.path();
            let Some(st) = read(&dir.join("stat")).and_then(|x| procfs::pid_stat(&x)) else {
                continue;
            };
            let k = (pid, st.starttime);
            live.insert(k);
            let key = match self.keys.get(&k) {
                Some(key) => key.clone(),
                None => {
                    let cmd = fs::read(dir.join("cmdline")).unwrap_or_default();
                    let key = procfs::proc_key(&cmd, &st.comm);
                    self.keys.insert(k, key.clone());
                    key
                }
            };
            if let Ok(tasks) = fs::read_dir(dir.join("task")) {
                for t in tasks.flatten() {
                    let Some(tid) = t.file_name().to_str().and_then(|n| n.parse::<u32>().ok())
                    else {
                        continue;
                    };
                    if let Some(c) =
                        read(&t.path().join("schedstat")).and_then(|x| procfs::schedstat_runs(&x))
                    {
                        s.threads.push(ThreadSample { pid, tid, ctxt: c });
                    }
                }
            }
            s.procs.push(ProcSample {
                pid,
                ppid: st.ppid,
                starttime: st.starttime,
                comm: st.comm,
                cpu_ticks: st.cpu_ticks,
                key,
            });
        }
        self.keys.retain(|k, _| live.contains(k));
    }

    /// (proto, local port) → process key for every TCP/UDP socket we can place, and
    /// the learned remote-end map. Unknown inodes are looked up in the processes that
    /// held sockets last time; a full /proc/*/fd scan only when `full_ok` and due.
    #[allow(clippy::type_complexity)]
    fn owners(
        &mut self,
        key_of: &HashMap<u32, String>,
        mono: u64,
        full_ok: bool,
    ) -> (
        HashMap<(u8, u16), String>,
        HashMap<(u8, IpAddr, u16), String>,
    ) {
        let Some(diag) = &mut self.diag else {
            return Default::default();
        };
        let Ok(socks) = diag.sockets() else {
            return Default::default();
        };
        let missing = socks
            .iter()
            .any(|k| k.inode != 0 && !self.inode_pid.contains_key(&k.inode));
        if missing {
            let due = self
                .last_fd_scan
                .is_none_or(|t| mono.saturating_sub(t) >= FD_SCAN_EVERY_MS);
            if full_ok && due {
                self.last_fd_scan = Some(mono);
                self.inode_pid = scan_fds(&self.proc_root, None);
                self.sock_pids = self.inode_pid.values().copied().collect();
            } else {
                let more = scan_fds(&self.proc_root, Some(&self.sock_pids));
                self.inode_pid.extend(more);
            }
        }
        let live: HashSet<u32> = socks.iter().map(|k| k.inode).collect();
        self.inode_pid.retain(|i, _| live.contains(i));
        self.sock_stats = (
            socks.len(),
            socks
                .iter()
                .filter(|k| self.inode_pid.contains_key(&k.inode))
                .count(),
            self.inode_pid.len(),
        );
        let mut m = HashMap::new();
        for k in &socks {
            let Some(key) = self.inode_pid.get(&k.inode).and_then(|pid| key_of.get(pid)) else {
                continue;
            };
            m.insert((k.proto, k.sport), key.clone());
            if k.dport != 0 && !k.dst.is_unspecified() {
                self.learned
                    .insert((k.proto, k.dst, k.dport), (key.clone(), mono));
            }
        }
        self.learned
            .retain(|_, v| mono.saturating_sub(v.1) < LEARN_KEEP_MS);
        let peers = self
            .learned
            .iter()
            .map(|(k, v)| (*k, v.0.clone()))
            .collect();
        (m, peers)
    }

    /// The light per-second read for a diagnostic window.
    pub fn second(&mut self) -> SecondSample {
        let mut destroyed = Vec::new();
        let mut flows = Vec::new();
        if let Some(ct) = &self.ct {
            let (d, o) = ct.destroyed();
            self.destroyed_for_ledger.extend(d.iter().cloned());
            self.ct_overflow_for_ledger |= o;
            destroyed = d;
        }
        if let Some(ct) = &mut self.ct {
            flows = ct.dump().unwrap_or_default();
        }
        let (power_uw, _, _) = self.battery();
        let mut key_of: HashMap<u32, String> = HashMap::new();
        for ((pid, _), k) in &self.keys {
            key_of.insert(*pid, k.clone());
        }
        let (owners, peers) = if flows.is_empty() {
            Default::default()
        } else {
            self.owners(&key_of, self.mono_ms(), false)
        };
        SecondSample {
            owners,
            peers,
            wan_pkts: self.wan_pkts(),
            flows,
            destroyed,
            power_uw,
            screen_on: self.screen_on(),
            self_ctxt: self.self_ctxt(),
        }
    }
}

/// socket inode → pid, from /proc/<pid>/fd links ("socket:[N]"); every process, or
/// only `only`.
fn scan_fds(proc_root: &Path, only: Option<&HashSet<u32>>) -> HashMap<u32, u32> {
    let mut m = HashMap::new();
    let pids: Vec<u32> = match only {
        Some(set) => set.iter().copied().collect(),
        None => match fs::read_dir(proc_root) {
            Ok(rd) => rd
                .flatten()
                .filter_map(|e| e.file_name().to_str().and_then(|n| n.parse().ok()))
                .collect(),
            Err(_) => return m,
        },
    };
    for pid in pids {
        let Ok(fds) = fs::read_dir(proc_root.join(pid.to_string()).join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            if let Ok(link) = fs::read_link(fd.path()) {
                let l = link.to_string_lossy();
                if let Some(n) = l
                    .strip_prefix("socket:[")
                    .and_then(|x| x.strip_suffix(']'))
                    .and_then(|x| x.parse().ok())
                {
                    m.entry(n).or_insert(pid);
                }
            }
        }
    }
    m
}

/// (all local addresses, addresses on the WAN interface), via getifaddrs.
pub fn addrs(wan: &str) -> (HashSet<IpAddr>, HashSet<IpAddr>) {
    let mut all = HashSet::new();
    let mut w = HashSet::new();
    let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut ifap) } != 0 {
        return (all, w);
    }
    let mut p = ifap;
    while !p.is_null() {
        let ifa = unsafe { &*p };
        p = ifa.ifa_next;
        if ifa.ifa_addr.is_null() {
            continue;
        }
        let fam = unsafe { (*ifa.ifa_addr).sa_family } as i32;
        let ip = match fam {
            libc::AF_INET => {
                let sa = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in) };
                IpAddr::V4(Ipv4Addr::from(u32::from_be(sa.sin_addr.s_addr)))
            }
            libc::AF_INET6 => {
                let sa = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in6) };
                IpAddr::V6(Ipv6Addr::from(sa.sin6_addr.s6_addr))
            }
            _ => continue,
        };
        let name = unsafe { std::ffi::CStr::from_ptr(ifa.ifa_name) }.to_string_lossy();
        if name == wan {
            w.insert(ip);
        }
        all.insert(ip);
    }
    unsafe { libc::freeifaddrs(ifap) };
    (all, w)
}
