//! Ledger record types (u60-ledger.md §6–7). Stored as CBOR inside the envelope.
//!
//! Sensitivity (D14): every field is normal unless its type says otherwise.
//! `Personal<T>` holds data about where the device talks to (peer addresses); it is
//! stored in full on the device and redacted by `redacted()` for every view unless
//! the caller asks for `full`. There are no secret fields in the ledger.

use serde::{Deserialize, Serialize};
use std::net::IpAddr;

pub const SCHEMA: u16 = 1;

// One kind table for every record in the envelope (ledger and event log), so the
// two stores can merge later without clashes. 4 = the segment name table
// (`envelope::KIND_NAMES`).
pub const KIND_START: u16 = 1;
pub const KIND_MINUTE: u16 = 2;
pub const KIND_DIAG: u16 = 3;
/// event log (`evlog.rs`): every block's state, first record of every segment
pub const KIND_CHECKPOINT: u16 = 16;
/// event log: one published block change
pub const KIND_BLOCK: u16 = 17;
/// event log: one published block change as the fields that changed against the
/// block's previous record
pub const KIND_DELTA: u16 = 18;

pub fn known_kind(kind: u16) -> bool {
    matches!(
        kind,
        KIND_START
            | KIND_MINUTE
            | KIND_DIAG
            | crate::envelope::KIND_NAMES
            | KIND_CHECKPOINT
            | KIND_BLOCK
            | KIND_DELTA
    )
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
#[serde(transparent)]
pub struct Personal<T>(pub T);

impl Personal<String> {
    /// Peer address cut to /24 (IPv4) or /48 (IPv6); non-addresses become "…".
    pub fn redacted(&self) -> Personal<String> {
        Personal(match self.0.parse::<IpAddr>() {
            Ok(IpAddr::V4(a)) => {
                let o = a.octets();
                format!("{}.{}.{}.0/24", o[0], o[1], o[2])
            }
            Ok(IpAddr::V6(a)) => {
                let s = a.segments();
                format!("{:x}:{:x}:{:x}::/48", s[0], s[1], s[2])
            }
            Err(_) if self.0.is_empty() => String::new(),
            Err(_) => "…".into(),
        })
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Meta {
    pub boot: String,
    /// ms since boot (/proc/uptime); ordering and retention use this
    pub mono_ms: u64,
    /// device wall clock, seconds (local time labelled UTC, see CLAUDE.md)
    pub wall: i64,
    pub wall_ok: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Start {
    pub meta: Meta,
    pub version: String,
    pub conntrack: bool,
    pub taskstats: bool,
    pub inet_diag: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Iface {
    pub name: String,
    pub rx_pkts: u64,
    pub tx_pkts: u64,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct ProcRow {
    /// process key (argv[0] + script), or "<ancestor> → <comm>" for short-lived ones
    pub name: String,
    /// most instances seen at once
    #[serde(default)]
    pub n: u32,
    /// context switches, all threads (0c definition)
    pub wakes: u64,
    pub cpu_ms: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct FlowRow {
    /// process key for the device's own sockets, "← key" for inbound,
    /// "LAN" for forwarded client traffic, "?" when the socket was not found
    pub owner: String,
    pub proto: u8,
    pub peer: Personal<String>,
    pub port: u16,
    pub pkts: u64,
    pub bytes: u64,
}

impl FlowRow {
    fn redacted(&self) -> FlowRow {
        FlowRow {
            peer: self.peer.redacted(),
            ..self.clone()
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct ForkRow {
    pub ancestor: String,
    pub comm: String,
    pub n: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Count {
    pub name: String,
    pub n: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct SelfCost {
    pub cpu_ms: u64,
    pub wakes: u64,
    pub rss_kb: u64,
    /// ledger ticks in this minute
    pub ticks: u32,
    /// time spent collecting, ms (sum over ticks)
    pub collect_ms: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Minute {
    pub meta: Meta,
    /// over the daily flash budget: rows were cut (u60-ledger.md §6)
    #[serde(default)]
    pub trimmed: bool,
    pub span_ms: u64,
    pub screen_on_ms: u64,
    /// average V×I over the ticks, mW, positive = drawing from the battery
    pub mw: Option<i64>,
    pub battery_pct: Option<u8>,
    pub charging: bool,
    /// permille of all CPU time
    pub busy_pm: u32,
    /// permille of wall×cores in idle states deeper than WFI (power-sample definition)
    pub deep_idle_pm: u32,
    pub ctxt: u64,
    pub intr: u64,
    /// /proc/stat processes delta (clone calls, threads included)
    pub clones: u64,
    /// taskstats exit records received (threads included)
    pub exits: u64,
    pub exit_overflow: bool,
    pub ct_overflow: bool,
    pub mihomo: bool,
    pub ifaces: Vec<Iface>,
    pub procs: Vec<ProcRow>,
    pub procs_other: ProcRow,
    /// sum of every row's wakes (to compare with ctxt)
    pub wakes_total: u64,
    /// cellular flows, top N
    pub flows: Vec<FlowRow>,
    pub flows_other: FlowRow,
    /// flows inside tailscale0 (what the outer tailscaled UDP carries), top N
    #[serde(default)]
    pub tailnet: Vec<FlowRow>,
    /// packets by class: cellular, lan/forwarded-not-cellular, local (lo/tun), tailnet
    pub flow_pkts: [u64; 4],
    pub forks: Vec<ForkRow>,
    pub irqs: Vec<Count>,
    #[serde(rename = "self")]
    pub self_cost: SelfCost,
}

impl Minute {
    pub fn redacted(&self) -> Minute {
        Minute {
            flows: self.flows.iter().map(FlowRow::redacted).collect(),
            flows_other: self.flows_other.redacted(),
            tailnet: self.tailnet.iter().map(FlowRow::redacted).collect(),
            ..self.clone()
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Breaker {
    pub flow: FlowRow,
    /// seconds in which this flow moved packets on the cellular link
    pub busy_secs: u32,
    /// of those, seconds where it was the only flow that moved
    pub solo_secs: u32,
    /// quiet runs of ≥2 s that ended with this flow active
    pub gaps_ended: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Diag {
    pub meta: Meta,
    pub secs: u32,
    /// "time" (ran to the end), "stopped", "restarted"
    pub ended: String,
    pub wan: String,
    pub wan_busy_secs: u32,
    /// permille of seconds inside quiet runs of ≥10 s (power-sample quiet10 share)
    pub quiet10_pm: u32,
    /// median quiet-run length, tenths of a second
    pub median_gap_ds: u32,
    pub pkts_per_min: u32,
    pub mw: Option<i64>,
    pub mw_samples: u32,
    pub screen_on_secs: u32,
    pub breakers: Vec<Breaker>,
    pub by_owner: Vec<Breaker>,
    /// busy seconds with no conntrack flow moving (untracked, offloaded, RA/ND…)
    pub unattributed_secs: u32,
    /// flows inside tailscale0 that moved in cellular-busy seconds (same columns;
    /// solo = the only inner flow moving), and the same per owner
    #[serde(default)]
    pub inner: Vec<Breaker>,
    #[serde(default)]
    pub inner_by_owner: Vec<Breaker>,
    pub self_wakes: u64,
}

impl Diag {
    pub fn redacted(&self) -> Diag {
        let r = |b: &Breaker| Breaker {
            flow: b.flow.redacted(),
            ..b.clone()
        };
        Diag {
            breakers: self.breakers.iter().map(r).collect(),
            by_owner: self.by_owner.iter().map(r).collect(),
            inner: self.inner.iter().map(r).collect(),
            inner_by_owner: self.inner_by_owner.iter().map(r).collect(),
            ..self.clone()
        }
    }
}

/// Something that hands out name indices (the store's segment table).
pub trait Intern {
    fn intern(&mut self, s: &str) -> u32;
}

impl Intern for Vec<String> {
    fn intern(&mut self, s: &str) -> u32 {
        match self.iter().position(|x| x == s) {
            Some(i) => i as u32,
            None => {
                self.push(s.to_string());
                (self.len() - 1) as u32
            }
        }
    }
}

/// On-disk form of `Minute` (KIND_MINUTE, schema 1): every string is an index into
/// the segment's name table, rows are arrays, keys are short. About a quarter of
/// the plain CBOR size (u60-ledger.md §6). Views always use `Minute`.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct MinuteC {
    /// boot (name), mono_ms, wall, wall_ok
    m: (u32, u64, i64, bool),
    #[serde(default)]
    t: bool,
    s: u64,
    so: u64,
    w: Option<i64>,
    b: Option<u8>,
    c: bool,
    bu: u32,
    di: u32,
    cx: u64,
    it: u64,
    cl: u64,
    ex: u64,
    eo: bool,
    co: bool,
    mi: bool,
    /// name, rx pkts, tx pkts, rx bytes, tx bytes
    i: Vec<(u32, u64, u64, u64, u64)>,
    /// name, n, wakes, cpu ms
    p: Vec<(u32, u32, u64, u64)>,
    po: (u32, u64, u64),
    wt: u64,
    /// owner, proto, peer, port, pkts, bytes
    f: Vec<(u32, u8, u32, u16, u64, u64)>,
    fo: (u64, u64),
    fp: [u64; 4],
    #[serde(default)]
    tn: Vec<(u32, u8, u32, u16, u64, u64)>,
    /// ancestor, comm, n
    k: Vec<(u32, u32, u32)>,
    /// name, n
    q: Vec<(u32, u64)>,
    /// cpu ms, wakes, rss kB, ticks, collect ms
    z: (u64, u64, u64, u32, u64),
}

impl MinuteC {
    pub fn pack(m: &Minute, n: &mut dyn Intern) -> MinuteC {
        MinuteC {
            m: (
                n.intern(&m.meta.boot),
                m.meta.mono_ms,
                m.meta.wall,
                m.meta.wall_ok,
            ),
            t: m.trimmed,
            s: m.span_ms,
            so: m.screen_on_ms,
            w: m.mw,
            b: m.battery_pct,
            c: m.charging,
            bu: m.busy_pm,
            di: m.deep_idle_pm,
            cx: m.ctxt,
            it: m.intr,
            cl: m.clones,
            ex: m.exits,
            eo: m.exit_overflow,
            co: m.ct_overflow,
            mi: m.mihomo,
            i: m.ifaces
                .iter()
                .map(|x| {
                    (
                        n.intern(&x.name),
                        x.rx_pkts,
                        x.tx_pkts,
                        x.rx_bytes,
                        x.tx_bytes,
                    )
                })
                .collect(),
            p: m.procs
                .iter()
                .map(|x| (n.intern(&x.name), x.n, x.wakes, x.cpu_ms))
                .collect(),
            po: (m.procs_other.n, m.procs_other.wakes, m.procs_other.cpu_ms),
            wt: m.wakes_total,
            f: m.flows
                .iter()
                .map(|x| {
                    (
                        n.intern(&x.owner),
                        x.proto,
                        n.intern(&x.peer.0),
                        x.port,
                        x.pkts,
                        x.bytes,
                    )
                })
                .collect(),
            fo: (m.flows_other.pkts, m.flows_other.bytes),
            fp: m.flow_pkts,
            tn: m
                .tailnet
                .iter()
                .map(|x| {
                    (
                        n.intern(&x.owner),
                        x.proto,
                        n.intern(&x.peer.0),
                        x.port,
                        x.pkts,
                        x.bytes,
                    )
                })
                .collect(),
            k: m.forks
                .iter()
                .map(|x| (n.intern(&x.ancestor), n.intern(&x.comm), x.n))
                .collect(),
            q: m.irqs.iter().map(|x| (n.intern(&x.name), x.n)).collect(),
            z: (
                m.self_cost.cpu_ms,
                m.self_cost.wakes,
                m.self_cost.rss_kb,
                m.self_cost.ticks,
                m.self_cost.collect_ms,
            ),
        }
    }

    /// None when an index is outside the table (damaged or foreign segment).
    pub fn unpack(&self, names: &[String]) -> Option<Minute> {
        let n = |i: u32| names.get(i as usize).cloned();
        Some(Minute {
            meta: Meta {
                boot: n(self.m.0)?,
                mono_ms: self.m.1,
                wall: self.m.2,
                wall_ok: self.m.3,
            },
            trimmed: self.t,
            span_ms: self.s,
            screen_on_ms: self.so,
            mw: self.w,
            battery_pct: self.b,
            charging: self.c,
            busy_pm: self.bu,
            deep_idle_pm: self.di,
            ctxt: self.cx,
            intr: self.it,
            clones: self.cl,
            exits: self.ex,
            exit_overflow: self.eo,
            ct_overflow: self.co,
            mihomo: self.mi,
            ifaces: self
                .i
                .iter()
                .map(|x| {
                    Some(Iface {
                        name: n(x.0)?,
                        rx_pkts: x.1,
                        tx_pkts: x.2,
                        rx_bytes: x.3,
                        tx_bytes: x.4,
                    })
                })
                .collect::<Option<_>>()?,
            procs: self
                .p
                .iter()
                .map(|x| {
                    Some(ProcRow {
                        name: n(x.0)?,
                        n: x.1,
                        wakes: x.2,
                        cpu_ms: x.3,
                    })
                })
                .collect::<Option<_>>()?,
            procs_other: ProcRow {
                name: "other".into(),
                n: self.po.0,
                wakes: self.po.1,
                cpu_ms: self.po.2,
            },
            wakes_total: self.wt,
            flows: self
                .f
                .iter()
                .map(|x| {
                    Some(FlowRow {
                        owner: n(x.0)?,
                        proto: x.1,
                        peer: Personal(n(x.2)?),
                        port: x.3,
                        pkts: x.4,
                        bytes: x.5,
                    })
                })
                .collect::<Option<_>>()?,
            flows_other: FlowRow {
                owner: "other".into(),
                pkts: self.fo.0,
                bytes: self.fo.1,
                ..Default::default()
            },
            flow_pkts: self.fp,
            tailnet: self
                .tn
                .iter()
                .map(|x| {
                    Some(FlowRow {
                        owner: n(x.0)?,
                        proto: x.1,
                        peer: Personal(n(x.2)?),
                        port: x.3,
                        pkts: x.4,
                        bytes: x.5,
                    })
                })
                .collect::<Option<_>>()?,
            forks: self
                .k
                .iter()
                .map(|x| {
                    Some(ForkRow {
                        ancestor: n(x.0)?,
                        comm: n(x.1)?,
                        n: x.2,
                    })
                })
                .collect::<Option<_>>()?,
            irqs: self
                .q
                .iter()
                .map(|x| {
                    Some(Count {
                        name: n(x.0)?,
                        n: x.1,
                    })
                })
                .collect::<Option<_>>()?,
            self_cost: SelfCost {
                cpu_ms: self.z.0,
                wakes: self.z.1,
                rss_kb: self.z.2,
                ticks: self.z.3,
                collect_ms: self.z.4,
            },
        })
    }
}

pub fn to_cbor<T: Serialize>(v: &T) -> Vec<u8> {
    let mut out = Vec::new();
    ciborium::into_writer(v, &mut out).expect("cbor encode");
    out
}

pub fn from_cbor<T: for<'de> Deserialize<'de>>(b: &[u8]) -> Option<T> {
    ciborium::from_reader(b).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction() {
        assert_eq!(
            Personal("203.0.113.77".to_string()).redacted().0,
            "203.0.113.0/24"
        );
        assert_eq!(
            Personal("2001:db8:1:2::5".to_string()).redacted().0,
            "2001:db8:1::/48"
        );
        assert_eq!(Personal("".to_string()).redacted().0, "");
    }

    #[test]
    fn compact_roundtrip() {
        let m = Minute {
            meta: Meta {
                boot: "b-1".into(),
                mono_ms: 5,
                wall: 9,
                wall_ok: true,
            },
            span_ms: 60_000,
            procs: vec![ProcRow {
                name: "/data/chill/bin/mihomo /data/chill".into(),
                n: 1,
                wakes: 9000,
                cpu_ms: 5000,
            }],
            procs_other: ProcRow {
                name: "other".into(),
                n: 3,
                wakes: 7,
                cpu_ms: 1,
            },
            flows: vec![FlowRow {
                owner: "/data/tailscale/nofight/tailscaled".into(),
                proto: 17,
                peer: Personal("203.0.113.9".into()),
                port: 41641,
                pkts: 100,
                bytes: 9000,
            }],
            flows_other: FlowRow {
                owner: "other".into(),
                pkts: 2,
                bytes: 3,
                ..Default::default()
            },
            forks: vec![ForkRow {
                ancestor: "/data/luyaz/luyaz-web".into(),
                comm: "stat".into(),
                n: 741,
            }],
            irqs: vec![Count {
                name: "11 arch_timer".into(),
                n: 8000,
            }],
            ifaces: vec![Iface {
                name: "rmnet_data0".into(),
                rx_pkts: 1,
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut names: Vec<String> = Vec::new();
        let c = MinuteC::pack(&m, &mut names);
        let back = from_cbor::<MinuteC>(&to_cbor(&c))
            .unwrap()
            .unpack(&names)
            .unwrap();
        assert_eq!(back, m);
        assert!(c.unpack(&names[..1]).is_none());
        // the second minute with the same names adds nothing to the table
        let before = names.len();
        MinuteC::pack(&m, &mut names);
        assert_eq!(names.len(), before);
        assert!(to_cbor(&c).len() * 2 < to_cbor(&m).len());
    }

    #[test]
    fn minute_roundtrip_and_old_reader() {
        let m = Minute {
            span_ms: 60_000,
            flows: vec![FlowRow {
                owner: "x".into(),
                peer: Personal("1.2.3.4".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let b = to_cbor(&m);
        let back: Minute = from_cbor(&b).unwrap();
        assert_eq!(back, m);
        // an older reader that knows fewer fields still decodes (unknown keys ignored)
        #[derive(Deserialize)]
        struct Old {
            span_ms: u64,
        }
        let old: Old = from_cbor(&b).unwrap();
        assert_eq!(old.span_ms, 60_000);
        assert!(
            !serde_json::to_string(&m.redacted())
                .unwrap()
                .contains("1.2.3.4")
        );
    }

    /// Canary (T1, ext-1031): a peer address is in full in the stored record (personal,
    /// on the device) and never in a redacted view of a minute or a diagnosis.
    #[test]
    fn canary_peer_never_in_a_redacted_view() {
        const PEER: &str = "198.51.100.173";
        let flow = FlowRow {
            owner: "x".into(),
            peer: Personal(PEER.into()),
            ..Default::default()
        };
        let m = Minute {
            flows: vec![flow.clone()],
            flows_other: flow.clone(),
            tailnet: vec![flow.clone()],
            ..Default::default()
        };
        let b = Breaker {
            flow,
            ..Default::default()
        };
        let d = Diag {
            breakers: vec![b.clone()],
            by_owner: vec![b.clone()],
            inner: vec![b.clone()],
            inner_by_owner: vec![b],
            ..Default::default()
        };
        assert!(serde_json::to_string(&m).unwrap().contains(PEER));
        for view in [
            serde_json::to_string(&m.redacted()).unwrap(),
            serde_json::to_string(&d.redacted()).unwrap(),
        ] {
            assert!(!view.contains(PEER), "{view}");
            assert!(!view.contains("198.51.100.1"), "{view}");
        }
    }
}
