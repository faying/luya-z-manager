//! The resident ledger, sans-IO (u60-ledger.md §3–4): snapshots in, minute records out.
//! Same snapshots always give the same records.

use crate::nl::{Flow, TaskExit};
use crate::procfs::{NetDev, SysStat};
use crate::record::*;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::IpAddr;

pub const CLK_TCK: u64 = 100;
/// a process older than this is a "long-lived ancestor" for fork attribution
const ANCESTOR_AGE_MS: u64 = 60_000;
const TOP_PROCS: usize = 20;
const TOP_FLOWS: usize = 10;
const TOP_FORKS: usize = 10;
const TOP_IRQS: usize = 10;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProcSample {
    pub pid: u32,
    pub ppid: u32,
    pub starttime: u64,
    pub comm: String,
    pub cpu_ticks: u64,
    pub key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ThreadSample {
    pub pid: u32,
    pub tid: u32,
    pub ctxt: u64,
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub mono_ms: u64,
    pub wall: i64,
    pub boot: String,
    pub screen_on: bool,
    /// V×I in µW, positive = discharging
    pub power_uw: Option<i64>,
    pub battery_pct: Option<u8>,
    pub charging: bool,
    pub stat: SysStat,
    pub idle_deep_us: u64,
    pub ncpu: u32,
    pub ifaces: Vec<NetDev>,
    pub procs: Vec<ProcSample>,
    pub threads: Vec<ThreadSample>,
    pub irqs: Vec<(String, u64)>,
    /// None = conntrack not available this tick
    pub flows: Option<Vec<Flow>>,
    pub destroyed: Vec<Flow>,
    pub ct_overflow: bool,
    pub exits: Vec<TaskExit>,
    pub exit_overflow: bool,
    /// (proto, local port) → process key, for the device's own sockets
    pub owners: HashMap<(u8, u16), String>,
    /// (proto, remote addr, remote port) → process key, learned from live sockets
    /// over the last 24 h; used when the flow's own socket is already closed
    pub peers: HashMap<(u8, IpAddr, u16), String>,
    pub local_addrs: HashSet<IpAddr>,
    pub wan_addrs: HashSet<IpAddr>,
    pub self_pid: u32,
    pub self_rss_kb: u64,
    pub collect_ms: u64,
}

// ── flow classification (shared with diag) ─────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Cellular = 0,
    Lan = 1,
    Local = 2,
    /// inside tailscale0 (100.64/10, fd7a:115c:a1e0::/48): what the outer UDP carries
    Tailnet = 3,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FlowKey {
    pub owner: String,
    pub proto: u8,
    pub peer: String,
    pub port: u16,
}

impl FlowKey {
    pub fn row(&self, pkts: u64, bytes: u64) -> FlowRow {
        FlowRow {
            owner: self.owner.clone(),
            proto: self.proto,
            peer: Personal(self.peer.clone()),
            port: self.port,
            pkts,
            bytes,
        }
    }
}

pub fn classify(f: &Flow, s: &Snapshot) -> (Class, FlowKey) {
    let o = &f.orig;
    let r = &f.reply;
    let cellular = [o.src, o.dst, r.src, r.dst]
        .iter()
        .any(|a| s.wan_addrs.contains(a));
    // own socket first; else whoever was last seen talking to the same remote end ("~")
    let owner = |port: u16, peer: IpAddr, pport: u16| {
        s.owners
            .get(&(o.proto, port))
            .cloned()
            .or_else(|| {
                s.peers
                    .get(&(o.proto, peer, pport))
                    .map(|k| format!("{k} ~"))
            })
            .unwrap_or_else(|| "?".into())
    };
    let (owner, peer, port) = if s.local_addrs.contains(&o.src) || o.src.is_loopback() {
        (owner(o.sport, o.dst, o.dport), o.dst, o.dport)
    } else if s.local_addrs.contains(&o.dst) {
        (
            format!("← {}", owner(o.dport, o.src, o.sport)),
            o.src,
            o.sport,
        )
    } else {
        ("LAN".to_string(), o.dst, o.dport)
    };
    let class = if cellular {
        Class::Cellular
    } else if o.src.is_loopback() || o.dst.is_loopback() || is_tun(&o.src) || is_tun(&o.dst) {
        Class::Local
    } else if is_tailnet(&o.src) || is_tailnet(&o.dst) {
        Class::Tailnet
    } else {
        Class::Lan
    };
    (
        class,
        FlowKey {
            owner,
            proto: o.proto,
            peer: peer.to_string(),
            port,
        },
    )
}

/// Tailscale addresses: 100.64.0.0/10 and fd7a:115c:a1e0::/48
fn is_tailnet(a: &IpAddr) -> bool {
    match a {
        IpAddr::V4(v) => v.octets()[0] == 100 && (v.octets()[1] & 0xc0) == 64,
        IpAddr::V6(v) => v.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}

/// mihomo TUN / fake-ip range 198.18.0.0/15
fn is_tun(a: &IpAddr) -> bool {
    matches!(a, IpAddr::V4(v) if v.octets()[0] == 198 && (v.octets()[1] & 0xfe) == 18)
}

fn flow_id(f: &Flow) -> FlowId {
    (f.id, f.orig.proto, f.orig.sport, f.orig.dport)
}

/// Packet/byte deltas per flow between ticks, with one tick of grace for flows that
/// vanished (their DESTROY event may arrive a tick late). Shared by ledger and diag.
/// conntrack id, proto, original ports
type FlowId = (u32, u8, u16, u16);

#[derive(Default)]
pub struct FlowDeltas {
    /// last packets, bytes, and whether the flow is a one-tick ghost
    prev: HashMap<FlowId, (u64, u64, bool)>,
}

impl FlowDeltas {
    /// Returns (flow, Δpkts, Δbytes) for every flow that moved. The first call only
    /// records a baseline when `baseline` is true.
    pub fn step<'a>(
        &mut self,
        flows: &'a [Flow],
        destroyed: &'a [Flow],
        baseline: bool,
    ) -> Vec<(&'a Flow, u64, u64)> {
        let mut out = Vec::new();
        let mut next: HashMap<_, _> = HashMap::with_capacity(flows.len());
        let live: HashSet<_> = flows.iter().map(flow_id).collect();
        for f in flows {
            let id = flow_id(f);
            let (pp, pb) = self.prev.get(&id).map(|x| (x.0, x.1)).unwrap_or((0, 0));
            if !baseline && f.pkts > pp {
                out.push((f, f.pkts - pp, f.bytes.saturating_sub(pb)));
            }
            next.insert(id, (f.pkts, f.bytes, false));
        }
        let mut done = HashSet::new();
        for f in destroyed {
            let id = flow_id(f);
            if live.contains(&id) || !done.insert(id) {
                continue;
            }
            let (pp, pb) = self.prev.get(&id).map(|x| (x.0, x.1)).unwrap_or((0, 0));
            if !baseline && f.pkts > pp {
                out.push((f, f.pkts - pp, f.bytes.saturating_sub(pb)));
            }
        }
        // keep vanished, not-yet-destroyed flows for one more tick
        for (id, v) in &self.prev {
            if !live.contains(id) && !done.contains(id) && !v.2 {
                next.insert(*id, (v.0, v.1, true));
            }
        }
        self.prev = next;
        out
    }
}

// ── ledger ──────────────────────────────────────────────────────────────────

struct PrevThread {
    pid: u32,
    pstart: u64,
    ctxt: u64,
    key: String,
    ghost: bool,
}

struct Prev {
    mono_ms: u64,
    stat: SysStat,
    idle_deep_us: u64,
    ifaces: HashMap<String, NetDev>,
    procs: HashMap<u32, (u64, u64)>, // pid → (starttime, cpu_ticks)
    threads: HashMap<u32, PrevThread>,
    irqs: HashMap<String, u64>,
}

#[derive(Default)]
struct ProcAcc {
    n: u32,
    wakes: u64,
    cpu_ticks: u64,
}

#[derive(Default)]
struct Acc {
    span_ms: u64,
    screen_on_ms: u64,
    mw_sum: i64,
    mw_n: i64,
    busy: u64,
    total: u64,
    deep_us: u64,
    ncpu: u32,
    ctxt: u64,
    intr: u64,
    clones: u64,
    exits: u64,
    exit_overflow: bool,
    ct_overflow: bool,
    mihomo: bool,
    ifaces: BTreeMap<String, [u64; 4]>,
    procs: HashMap<String, ProcAcc>,
    flows: HashMap<FlowKey, (u64, u64)>,
    tailnet: HashMap<FlowKey, (u64, u64)>,
    flow_pkts: [u64; 4],
    forks: HashMap<(String, String), u32>,
    irqs: HashMap<String, u64>,
    self_wakes: u64,
    self_cpu: u64,
    ticks: u32,
    collect_ms: u64,
}

pub struct Ledger {
    prev: Option<Prev>,
    /// pid → ppid of the previous tick's exit records (parents that died a tick earlier)
    recent_exits: HashMap<u32, u32>,
    flows: FlowDeltas,
    acc: Acc,
    last: Option<Last>,
}

#[derive(Clone)]
struct Last {
    boot: String,
    mono_ms: u64,
    wall: i64,
    battery_pct: Option<u8>,
    charging: bool,
    rss_kb: u64,
}

impl Default for Ledger {
    fn default() -> Self {
        Self::new()
    }
}

impl Ledger {
    pub fn new() -> Ledger {
        Ledger {
            prev: None,
            recent_exits: HashMap::new(),
            flows: FlowDeltas::default(),
            acc: Acc::default(),
            last: None,
        }
    }

    /// Feeds one snapshot; returns a minute record when ≥55 s have accumulated.
    pub fn tick(&mut self, s: &Snapshot) -> Option<Minute> {
        let Some(prev) = self.prev.take() else {
            self.flows
                .step(s.flows.as_deref().unwrap_or(&[]), &s.destroyed, true);
            self.prev = Some(Self::to_prev(s, HashMap::new()));
            self.acc = Acc::default();
            return None;
        };
        let dt = s.mono_ms.saturating_sub(prev.mono_ms);
        let a = &mut self.acc;
        a.span_ms += dt;
        if s.screen_on {
            a.screen_on_ms += dt;
        }
        if let Some(p) = s.power_uw {
            a.mw_sum += p / 1000;
            a.mw_n += 1;
        }
        a.busy += s.stat.busy.saturating_sub(prev.stat.busy);
        a.total += s.stat.total.saturating_sub(prev.stat.total);
        a.deep_us += s.idle_deep_us.saturating_sub(prev.idle_deep_us);
        a.ncpu = s.ncpu.max(1);
        a.ctxt += s.stat.ctxt.saturating_sub(prev.stat.ctxt);
        a.intr += s.stat.intr.saturating_sub(prev.stat.intr);
        a.clones += s.stat.processes.saturating_sub(prev.stat.processes);
        a.exits += s.exits.len() as u64;
        a.exit_overflow |= s.exit_overflow;
        a.ct_overflow |= s.ct_overflow;
        a.ticks += 1;
        a.collect_ms += s.collect_ms;

        for d in &s.ifaces {
            if let Some(p) = prev.ifaces.get(&d.name) {
                let e = a.ifaces.entry(d.name.clone()).or_default();
                e[0] += d.rx_pkts.saturating_sub(p.rx_pkts);
                e[1] += d.tx_pkts.saturating_sub(p.tx_pkts);
                e[2] += d.rx_bytes.saturating_sub(p.rx_bytes);
                e[3] += d.tx_bytes.saturating_sub(p.tx_bytes);
            }
        }
        for (name, n) in &s.irqs {
            if let Some(p) = prev.irqs.get(name) {
                *a.irqs.entry(name.clone()).or_default() += n.saturating_sub(*p);
            }
        }

        // processes: CPU and instance counts
        let by_pid: HashMap<u32, &ProcSample> = s.procs.iter().map(|p| (p.pid, p)).collect();
        let mut instances: HashMap<&str, u32> = HashMap::new();
        for p in &s.procs {
            *instances.entry(&p.key).or_default() += 1;
            let cpu = match prev.procs.get(&p.pid) {
                Some((st, c)) if *st == p.starttime => p.cpu_ticks.saturating_sub(*c),
                _ => p.cpu_ticks, // started during the interval
            };
            let e = a.procs.entry(p.key.clone()).or_default();
            e.cpu_ticks += cpu;
            if p.pid == s.self_pid {
                a.self_cpu += cpu;
            }
            a.mihomo |= p.key.contains("mihomo");
        }
        for (k, n) in instances {
            let e = a.procs.entry(k.to_string()).or_default();
            e.n = e.n.max(n);
        }

        // threads: live deltas
        let pstart: HashMap<u32, u64> = s.procs.iter().map(|p| (p.pid, p.starttime)).collect();
        let mut seen = HashSet::with_capacity(s.threads.len());
        for t in &s.threads {
            seen.insert(t.tid);
            let Some(p) = by_pid.get(&t.pid) else {
                continue;
            };
            let st = pstart.get(&t.pid).copied().unwrap_or(0);
            let d = match prev.threads.get(&t.tid) {
                Some(pt) if pt.pid == t.pid && pt.pstart == st && t.ctxt >= pt.ctxt => {
                    t.ctxt - pt.ctxt
                }
                _ => t.ctxt,
            };
            a.procs.entry(p.key.clone()).or_default().wakes += d;
            if t.pid == s.self_pid {
                a.self_wakes += d;
            }
        }

        // exits: residuals of threads we knew, whole counts of ones born in between
        let mut children: HashMap<u32, Vec<&ProcSample>> = HashMap::new();
        for p in &s.procs {
            children.entry(p.ppid).or_default().push(p);
        }
        let mut nthreads: HashMap<u32, u32> = HashMap::new();
        for t in &s.threads {
            *nthreads.entry(t.pid).or_default() += 1;
        }
        let this_tick: HashMap<u32, u32> = s.exits.iter().map(|e| (e.pid, e.ppid)).collect();
        let mut exited = std::mem::take(&mut self.recent_exits);
        exited.extend(this_tick.iter().map(|(k, v)| (*k, *v)));
        for e in s.exits.iter().filter(|e| !e.tgid_aggregate) {
            if let Some(pt) = prev.threads.get(&e.pid) {
                if !seen.contains(&e.pid) {
                    a.procs.entry(pt.key.clone()).or_default().wakes +=
                        e.ctxt.saturating_sub(pt.ctxt);
                }
                continue;
            }
            // never seen: a thread of a live multi-threaded sibling with the same name
            // (a thread's ppid is its process's parent), or a short-lived process
            if let Some(owner) = children.get(&e.ppid).and_then(|v| {
                v.iter().find(|p| {
                    p.comm == e.comm
                        && p.pid != e.pid
                        && nthreads.get(&p.pid).copied().unwrap_or(0) > 1
                })
            }) {
                a.procs.entry(owner.key.clone()).or_default().wakes += e.ctxt;
                continue;
            }
            let anc = ancestor(e.ppid, &by_pid, &exited, s.mono_ms);
            let row = format!("{anc} → {}", e.comm);
            let pa = a.procs.entry(row).or_default();
            pa.wakes += e.ctxt;
            pa.cpu_ticks += e.cpu_us * CLK_TCK / 1_000_000;
            *a.forks.entry((anc, e.comm.clone())).or_default() += 1;
        }

        self.recent_exits = this_tick;

        // flows
        if let Some(flows) = &s.flows {
            for (f, dp, db) in self.flows.step(flows, &s.destroyed, false) {
                let (class, key) = classify(f, s);
                a.flow_pkts[class as usize] += dp;
                let table = match class {
                    Class::Cellular => &mut a.flows,
                    Class::Tailnet => &mut a.tailnet,
                    _ => continue,
                };
                let e = table.entry(key).or_default();
                e.0 += dp;
                e.1 += db;
            }
        }

        // new prev; threads that vanished stay one more tick for late exit records
        let mut ghosts = HashMap::new();
        for (tid, pt) in prev.threads {
            if !seen.contains(&tid) && !pt.ghost && !s.exits.iter().any(|e| e.pid == tid) {
                ghosts.insert(tid, PrevThread { ghost: true, ..pt });
            }
        }
        self.prev = Some(Self::to_prev(s, ghosts));
        self.last = Some(Last {
            boot: s.boot.clone(),
            mono_ms: s.mono_ms,
            wall: s.wall,
            battery_pct: s.battery_pct,
            charging: s.charging,
            rss_kb: s.self_rss_kb,
        });

        if self.acc.span_ms >= 55_000 {
            let m = self.emit();
            self.acc = Acc::default();
            return Some(m);
        }
        None
    }

    fn to_prev(s: &Snapshot, mut threads: HashMap<u32, PrevThread>) -> Prev {
        let by_pid: HashMap<u32, &ProcSample> = s.procs.iter().map(|p| (p.pid, p)).collect();
        for t in &s.threads {
            if let Some(p) = by_pid.get(&t.pid) {
                threads.insert(
                    t.tid,
                    PrevThread {
                        pid: t.pid,
                        pstart: p.starttime,
                        ctxt: t.ctxt,
                        key: p.key.clone(),
                        ghost: false,
                    },
                );
            }
        }
        Prev {
            mono_ms: s.mono_ms,
            stat: s.stat,
            idle_deep_us: s.idle_deep_us,
            ifaces: s
                .ifaces
                .iter()
                .map(|d| (d.name.clone(), d.clone()))
                .collect(),
            procs: s
                .procs
                .iter()
                .map(|p| (p.pid, (p.starttime, p.cpu_ticks)))
                .collect(),
            threads,
            irqs: s.irqs.iter().cloned().collect(),
        }
    }

    /// What has accumulated since the last minute record (None before two ticks).
    pub fn partial(&self) -> Option<Minute> {
        (self.acc.ticks > 0).then(|| self.emit())
    }

    fn emit(&self) -> Minute {
        let a = &self.acc;
        let s = self.last.clone().expect("emit after a tick");
        let tick_ms = 1000 / CLK_TCK;
        let mut procs: Vec<ProcRow> = a
            .procs
            .iter()
            .filter(|(_, v)| v.wakes > 0 || v.cpu_ticks > 0)
            .map(|(k, v)| ProcRow {
                name: k.clone(),
                n: v.n,
                wakes: v.wakes,
                cpu_ms: v.cpu_ticks * tick_ms,
            })
            .collect();
        procs.sort_by(|x, y| {
            y.wakes
                .cmp(&x.wakes)
                .then(y.cpu_ms.cmp(&x.cpu_ms))
                .then(x.name.cmp(&y.name))
        });
        let wakes_total = procs.iter().map(|p| p.wakes).sum();
        let mut procs_other = ProcRow {
            name: "other".into(),
            ..Default::default()
        };
        for p in procs.iter().skip(TOP_PROCS) {
            procs_other.n += 1;
            procs_other.wakes += p.wakes;
            procs_other.cpu_ms += p.cpu_ms;
        }
        procs.truncate(TOP_PROCS);

        let mut flows: Vec<FlowRow> = a.flows.iter().map(|(k, v)| k.row(v.0, v.1)).collect();
        flows.sort_by(|x, y| {
            y.pkts
                .cmp(&x.pkts)
                .then(x.owner.cmp(&y.owner))
                .then(x.peer.cmp(&y.peer))
        });
        let mut flows_other = FlowRow {
            owner: "other".into(),
            ..Default::default()
        };
        for f in flows.iter().skip(TOP_FLOWS) {
            flows_other.pkts += f.pkts;
            flows_other.bytes += f.bytes;
        }
        flows.truncate(TOP_FLOWS);
        let mut tailnet: Vec<FlowRow> = a.tailnet.iter().map(|(k, v)| k.row(v.0, v.1)).collect();
        tailnet.sort_by(|x, y| {
            y.pkts
                .cmp(&x.pkts)
                .then(x.owner.cmp(&y.owner))
                .then(x.peer.cmp(&y.peer))
        });
        tailnet.truncate(TOP_FLOWS);

        let mut forks: Vec<ForkRow> = a
            .forks
            .iter()
            .map(|((an, c), n)| ForkRow {
                ancestor: an.clone(),
                comm: c.clone(),
                n: *n,
            })
            .collect();
        forks.sort_by(|x, y| {
            y.n.cmp(&x.n)
                .then(x.ancestor.cmp(&y.ancestor))
                .then(x.comm.cmp(&y.comm))
        });
        forks.truncate(TOP_FORKS);

        let mut irqs: Vec<Count> = a
            .irqs
            .iter()
            .filter(|(_, n)| **n > 0)
            .map(|(k, n)| Count {
                name: k.clone(),
                n: *n,
            })
            .collect();
        irqs.sort_by(|x, y| y.n.cmp(&x.n).then(x.name.cmp(&y.name)));
        irqs.truncate(TOP_IRQS);

        let ifaces = a
            .ifaces
            .iter()
            .filter(|(_, v)| v.iter().any(|x| *x > 0))
            .map(|(k, v)| Iface {
                name: k.clone(),
                rx_pkts: v[0],
                tx_pkts: v[1],
                rx_bytes: v[2],
                tx_bytes: v[3],
            })
            .collect();

        let wall_dur = a.span_ms * 1000 * a.ncpu.max(1) as u64;
        Minute {
            meta: meta(&s.boot, s.mono_ms, s.wall),
            trimmed: false,
            span_ms: a.span_ms,
            screen_on_ms: a.screen_on_ms,
            mw: (a.mw_n > 0).then(|| a.mw_sum / a.mw_n),
            battery_pct: s.battery_pct,
            charging: s.charging,
            busy_pm: (a.busy * 1000).checked_div(a.total).unwrap_or(0) as u32,
            deep_idle_pm: (a.deep_us * 1000)
                .checked_div(wall_dur)
                .unwrap_or(0)
                .min(1000) as u32,
            ctxt: a.ctxt,
            intr: a.intr,
            clones: a.clones,
            exits: a.exits,
            exit_overflow: a.exit_overflow,
            ct_overflow: a.ct_overflow,
            mihomo: a.mihomo,
            ifaces,
            procs,
            procs_other,
            wakes_total,
            flows,
            flows_other,
            tailnet,
            flow_pkts: a.flow_pkts,
            forks,
            irqs,
            self_cost: SelfCost {
                cpu_ms: a.self_cpu * tick_ms,
                wakes: a.self_wakes,
                rss_kb: s.rss_kb,
                ticks: a.ticks,
                collect_ms: a.collect_ms,
            },
        }
    }
}

/// First ancestor that has lived ≥60 s (or pid 1), as its process key.
fn ancestor(
    mut pid: u32,
    by_pid: &HashMap<u32, &ProcSample>,
    exited: &HashMap<u32, u32>,
    now_ms: u64,
) -> String {
    for _ in 0..32 {
        if let Some(p) = by_pid.get(&pid) {
            let age_ms = now_ms.saturating_sub(p.starttime * (1000 / CLK_TCK));
            if pid == 1 || age_ms >= ANCESTOR_AGE_MS {
                return p.key.clone();
            }
            pid = p.ppid;
        } else if let Some(pp) = exited.get(&pid) {
            // a short-lived parent (subshell) that exited too: keep climbing
            pid = *pp;
        } else {
            return if pid <= 1 {
                "init".into()
            } else {
                "(gone)".into()
            };
        }
    }
    "(deep)".into()
}

/// 2026-01-01 00:00:00: anything earlier means the clock was not set yet.
const WALL_FLOOR: i64 = 1_767_225_600;

pub fn meta(boot: &str, mono_ms: u64, wall: i64) -> Meta {
    Meta {
        boot: boot.to_string(),
        mono_ms,
        wall,
        wall_ok: wall >= WALL_FLOOR,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nl::Tuple;

    fn proc_(pid: u32, ppid: u32, start: u64, key: &str, comm: &str, cpu: u64) -> ProcSample {
        ProcSample {
            pid,
            ppid,
            starttime: start,
            comm: comm.into(),
            cpu_ticks: cpu,
            key: key.into(),
        }
    }

    fn flow(id: u32, src: &str, dst: &str, sport: u16, dport: u16, pkts: u64) -> Flow {
        let t = |a: &str, b: &str, sp, dp| Tuple {
            src: a.parse().unwrap(),
            dst: b.parse().unwrap(),
            proto: 17,
            sport: sp,
            dport: dp,
        };
        Flow {
            id,
            orig: t(src, dst, sport, dport),
            reply: t(dst, src, dport, sport),
            pkts,
            bytes: pkts * 100,
        }
    }

    fn base(mono: u64) -> Snapshot {
        let wan: IpAddr = "10.20.30.40".parse().unwrap();
        Snapshot {
            mono_ms: mono,
            wall: 1_790_000_000,
            boot: "b".into(),
            ncpu: 4,
            self_pid: 999,
            wan_addrs: [wan].into_iter().collect(),
            local_addrs: [wan, "192.168.0.1".parse().unwrap()].into_iter().collect(),
            owners: [((17u8, 41641u16), "/data/tailscale/tailscaled".to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn minute_accounts_threads_exits_forks_flows() {
        let mut l = Ledger::new();
        // t0: tailscaled (2 threads), a shell loop (old), the ledger itself
        let mut s0 = base(1_000_000);
        s0.stat = SysStat {
            ctxt: 1000,
            processes: 500,
            busy: 10,
            total: 100,
            intr: 50,
        };
        s0.procs = vec![
            proc_(100, 1, 100, "/data/tailscale/tailscaled", "tailscaled", 10),
            proc_(200, 1, 100, "sh /data/u60-guard/u60-guard.sh", "sh", 5),
            proc_(999, 1, 100, "u60d", "u60d", 1),
        ];
        s0.threads = vec![
            ThreadSample {
                pid: 100,
                tid: 100,
                ctxt: 50,
            },
            ThreadSample {
                pid: 100,
                tid: 101,
                ctxt: 70,
            },
            ThreadSample {
                pid: 200,
                tid: 200,
                ctxt: 5,
            },
            ThreadSample {
                pid: 999,
                tid: 999,
                ctxt: 1,
            },
        ];
        s0.flows = Some(vec![flow(
            1,
            "10.20.30.40",
            "203.0.113.9",
            41641,
            41641,
            10,
        )]);
        assert!(l.tick(&s0).is_none());

        // t1 = +60 s: thread 101 exited (exit record), sh forked awk ×2, flow moved 5, a DNS flow lived and died
        let mut s1 = base(1_060_000);
        s1.stat = SysStat {
            ctxt: 2000,
            processes: 503,
            busy: 30,
            total: 300,
            intr: 80,
        };
        s1.procs = vec![
            proc_(100, 1, 100, "/data/tailscale/tailscaled", "tailscaled", 14),
            proc_(200, 1, 100, "sh /data/u60-guard/u60-guard.sh", "sh", 6),
            proc_(999, 1, 100, "u60d", "u60d", 2),
        ];
        s1.threads = vec![
            ThreadSample {
                pid: 100,
                tid: 100,
                ctxt: 80,
            },
            ThreadSample {
                pid: 200,
                tid: 200,
                ctxt: 9,
            },
            ThreadSample {
                pid: 999,
                tid: 999,
                ctxt: 2,
            },
        ];
        s1.exits = vec![
            TaskExit {
                pid: 101,
                ppid: 1,
                comm: "tailscaled".into(),
                ctxt: 75,
                ..Default::default()
            },
            TaskExit {
                pid: 300,
                ppid: 200,
                comm: "awk".into(),
                ctxt: 3,
                cpu_us: 20_000,
                ..Default::default()
            },
            TaskExit {
                pid: 301,
                ppid: 200,
                comm: "awk".into(),
                ctxt: 4,
                ..Default::default()
            },
            // `$(stat …)` inside a subshell: both exit within the interval
            TaskExit {
                pid: 310,
                ppid: 200,
                comm: "sh".into(),
                ctxt: 1,
                ..Default::default()
            },
            TaskExit {
                pid: 311,
                ppid: 310,
                comm: "stat".into(),
                ctxt: 1,
                ..Default::default()
            },
        ];
        s1.flows = Some(vec![flow(
            1,
            "10.20.30.40",
            "203.0.113.9",
            41641,
            41641,
            15,
        )]);
        s1.destroyed = vec![flow(2, "10.20.30.40", "8.8.8.8", 5353, 53, 2)];
        s1.ifaces = vec![];
        let m = l.tick(&s1).expect("minute");

        let row = |n: &str| {
            m.procs
                .iter()
                .find(|p| p.name == n)
                .cloned()
                .unwrap_or_default()
        };
        // tailscaled: main 30 + exited thread residual 5
        assert_eq!(row("/data/tailscale/tailscaled").wakes, 35);
        assert_eq!(row("/data/tailscale/tailscaled").cpu_ms, 40);
        assert_eq!(row("sh /data/u60-guard/u60-guard.sh → awk").wakes, 7);
        assert_eq!(row("sh /data/u60-guard/u60-guard.sh → awk").cpu_ms, 20);
        let g = "sh /data/u60-guard/u60-guard.sh".to_string();
        assert_eq!(
            m.forks[0],
            ForkRow {
                ancestor: g.clone(),
                comm: "awk".into(),
                n: 2
            }
        );
        assert!(m.forks.contains(&ForkRow {
            ancestor: g.clone(),
            comm: "stat".into(),
            n: 1
        }));
        assert!(m.forks.contains(&ForkRow {
            ancestor: g,
            comm: "sh".into(),
            n: 1
        }));
        assert_eq!(m.self_cost.wakes, 1);
        assert_eq!((m.ctxt, m.clones, m.exits), (1000, 3, 5));
        assert_eq!(m.busy_pm, 100);
        // flows: tailscaled +5, DNS 2 whole (owner unknown: no socket for 5353)
        assert_eq!(m.flow_pkts[0], 7);
        assert_eq!(m.flows[0].owner, "/data/tailscale/tailscaled");
        assert_eq!(m.flows[0].pkts, 5);
        assert_eq!(m.flows[1].owner, "?");
        assert_eq!(
            (m.flows[1].port, m.flows[1].peer.0.as_str()),
            (53, "8.8.8.8")
        );
    }

    #[test]
    fn late_destroy_after_vanish_is_not_double_counted() {
        let mut d = FlowDeltas::default();
        let f = flow(7, "10.20.30.40", "1.1.1.1", 1000, 53, 4);
        d.step(std::slice::from_ref(&f), &[], true);
        // gone from the dump, no event yet
        assert!(d.step(&[], &[], false).is_empty());
        // event arrives one tick late with final count 6 → only +2
        let fin = flow(7, "10.20.30.40", "1.1.1.1", 1000, 53, 6);
        let v = d.step(&[], std::slice::from_ref(&fin), false);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].1, 2);
    }

    #[test]
    fn lan_and_inbound_classes() {
        let s = base(0);
        let fwd = flow(1, "192.168.0.50", "203.0.113.1", 5555, 443, 1);
        let mut fwd_nat = fwd.clone();
        fwd_nat.reply.dst = "10.20.30.40".parse().unwrap();
        let (c, k) = classify(&fwd_nat, &s);
        assert_eq!((c, k.owner.as_str()), (Class::Cellular, "LAN"));
        let inbound = flow(2, "203.0.113.5", "10.20.30.40", 33333, 41641, 1);
        let (c, k) = classify(&inbound, &s);
        assert_eq!(c, Class::Cellular);
        assert_eq!(k.owner, "← /data/tailscale/tailscaled");
        let mut s2 = base(0);
        s2.peers.insert(
            (17, "203.0.113.1".parse().unwrap(), 443),
            "/data/chill/bin/mihomo /data/chill".into(),
        );
        let closed = flow(4, "10.20.30.40", "203.0.113.1", 50000, 443, 1);
        assert_eq!(
            classify(&closed, &s2).1.owner,
            "/data/chill/bin/mihomo /data/chill ~"
        );
        assert_eq!(classify(&closed, &s).1.owner, "?");
        let tun = flow(3, "198.18.0.2", "198.18.0.1", 1, 2, 1);
        assert_eq!(classify(&tun, &s).0, Class::Local);
    }
}
