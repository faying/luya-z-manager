//! Diagnostic window, sans-IO (u60-ledger.md §3): one sample per second, answers
//! "who breaks cellular quiet". Quiet statistics use the power-sample definitions.

use crate::ledger::{Class, FlowDeltas, FlowKey, Snapshot, classify};
use crate::nl::Flow;
use crate::record::{Breaker, Diag};
use std::collections::{BTreeMap, HashSet};

pub const MAX_SECS: u32 = 7200;
const TOP: usize = 15;

/// One per-second input. `snap` carries the attribution context (owners, addresses)
/// and may be reused between seconds; only flows/destroyed/wan counters change.
pub struct Second<'a> {
    pub wan_pkts: u64,
    pub flows: &'a [Flow],
    pub destroyed: &'a [Flow],
    pub power_uw: Option<i64>,
    pub screen_on: bool,
    pub self_ctxt: u64,
    pub ctx: &'a Snapshot,
}

#[derive(Default, Clone, Copy)]
struct Stat {
    busy: u32,
    solo: u32,
    gaps: u32,
    pkts: u64,
    bytes: u64,
}

/// Busy/solo/gap counts per flow and per owner for one class of flows.
#[derive(Default)]
struct Table {
    flows: BTreeMap<FlowKey, Stat>,
    owners: BTreeMap<String, Stat>,
}

impl Table {
    /// One cellular-busy second in which `active` flows of this class moved.
    fn add(&mut self, active: BTreeMap<FlowKey, (u64, u64)>, ended_gap: bool) {
        let solo = active.len() == 1;
        // per owner, an inferred "x ~" counts as x (else one busy second looks shared)
        let owners: HashSet<String> = active
            .keys()
            .map(|k| k.owner.trim_end_matches(" ~").to_string())
            .collect();
        for (k, (p, b)) in active {
            let st = self.flows.entry(k).or_default();
            st.busy += 1;
            st.solo += solo as u32;
            st.gaps += ended_gap as u32;
            st.pkts += p;
            st.bytes += b;
        }
        let solo_owner = owners.len() == 1;
        for o in owners {
            let st = self.owners.entry(o).or_default();
            st.busy += 1;
            st.solo += solo_owner as u32;
            st.gaps += ended_gap as u32;
        }
    }

    /// (per flow, per owner), each ranked and cut to TOP.
    fn ranked(&self) -> (Vec<Breaker>, Vec<Breaker>) {
        let rank = |mut v: Vec<Breaker>| {
            v.sort_by(|a, b| {
                b.solo_secs
                    .cmp(&a.solo_secs)
                    .then(b.gaps_ended.cmp(&a.gaps_ended))
                    .then(b.busy_secs.cmp(&a.busy_secs))
                    .then(a.flow.owner.cmp(&b.flow.owner))
            });
            v.truncate(TOP);
            v
        };
        let flows = self
            .flows
            .iter()
            .map(|(k, s)| Breaker {
                flow: k.row(s.pkts, s.bytes),
                busy_secs: s.busy,
                solo_secs: s.solo,
                gaps_ended: s.gaps,
            })
            .collect();
        let owners = self
            .owners
            .iter()
            .map(|(o, s)| Breaker {
                flow: crate::record::FlowRow {
                    owner: o.clone(),
                    ..Default::default()
                },
                busy_secs: s.busy,
                solo_secs: s.solo,
                gaps_ended: s.gaps,
            })
            .collect();
        (rank(flows), rank(owners))
    }
}

pub struct Window {
    pub wan: String,
    pub target_secs: u32,
    deltas: FlowDeltas,
    last_wan: Option<u64>,
    first_self: Option<u64>,
    last_self: u64,
    per_sec: Vec<u64>,
    gap: u32,
    cellular: Table,
    inner: Table,
    unattributed: u32,
    mw_sum: i64,
    mw_n: u32,
    screen_on: u32,
    baseline_done: bool,
}

impl Window {
    pub fn new(wan: &str, secs: u32) -> Window {
        Window {
            wan: wan.to_string(),
            target_secs: secs.clamp(10, MAX_SECS),
            deltas: FlowDeltas::default(),
            last_wan: None,
            first_self: None,
            last_self: 0,
            per_sec: Vec::new(),
            gap: 0,
            cellular: Table::default(),
            inner: Table::default(),
            unattributed: 0,
            mw_sum: 0,
            mw_n: 0,
            screen_on: 0,
            baseline_done: false,
        }
    }

    pub fn elapsed(&self) -> u32 {
        self.per_sec.len() as u32
    }

    pub fn done(&self) -> bool {
        self.elapsed() >= self.target_secs
    }

    pub fn push(&mut self, s: &Second) {
        self.first_self.get_or_insert(s.self_ctxt);
        self.last_self = s.self_ctxt;
        let moved = self.deltas.step(s.flows, s.destroyed, !self.baseline_done);
        let Some(last) = self.last_wan.replace(s.wan_pkts) else {
            self.baseline_done = true;
            return;
        };
        let wan_d = s.wan_pkts.saturating_sub(last);
        self.per_sec.push(wan_d);
        if let Some(p) = s.power_uw
            && self.per_sec.len().is_multiple_of(5)
        {
            self.mw_sum += p / 1000;
            self.mw_n += 1;
        }
        if s.screen_on {
            self.screen_on += 1;
        }
        let mut active: BTreeMap<FlowKey, (u64, u64)> = BTreeMap::new();
        let mut inner: BTreeMap<FlowKey, (u64, u64)> = BTreeMap::new();
        for (f, dp, db) in moved {
            let (class, key) = classify(f, s.ctx);
            let table = match class {
                Class::Cellular => &mut active,
                Class::Tailnet => &mut inner,
                _ => continue,
            };
            let e = table.entry(key).or_default();
            e.0 += dp;
            e.1 += db;
        }
        if wan_d == 0 {
            self.gap += 1;
            return;
        }
        let ended_gap = self.gap >= 2;
        self.gap = 0;
        if !inner.is_empty() {
            self.inner.add(inner, ended_gap);
        }
        if active.is_empty() {
            self.unattributed += 1;
            return;
        }
        self.cellular.add(active, ended_gap);
    }

    pub fn result(&self, meta: crate::record::Meta, ended: &str) -> Diag {
        let q = quiet_stats(&self.per_sec);
        let n = self.per_sec.len() as u64;
        let total: u64 = self.per_sec.iter().sum();
        let (breakers, by_owner) = self.cellular.ranked();
        let (inner, inner_by_owner) = self.inner.ranked();
        Diag {
            meta,
            secs: n as u32,
            ended: ended.to_string(),
            wan: self.wan.clone(),
            wan_busy_secs: q.busy,
            quiet10_pm: q.quiet10_pm,
            median_gap_ds: q.median_gap_ds,
            pkts_per_min: (total * 60).checked_div(n).unwrap_or(0) as u32,
            mw: (self.mw_n > 0).then(|| self.mw_sum / self.mw_n as i64),
            mw_samples: self.mw_n,
            screen_on_secs: self.screen_on,
            breakers,
            by_owner,
            inner,
            inner_by_owner,
            unattributed_secs: self.unattributed,
            self_wakes: self.last_self.saturating_sub(self.first_self.unwrap_or(0)),
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct Quiet {
    pub busy: u32,
    pub quiet10_pm: u32,
    pub median_gap_ds: u32,
}

/// power-sample `wan_stats`: busy seconds, share of seconds in quiet runs ≥10 s,
/// median quiet-run length (all runs, including the trailing one).
pub fn quiet_stats(per_sec: &[u64]) -> Quiet {
    let mut gaps = Vec::new();
    let mut g = 0u32;
    let mut busy = 0u32;
    for &p in per_sec {
        if p > 0 {
            busy += 1;
            if g > 0 {
                gaps.push(g);
            }
            g = 0;
        } else {
            g += 1;
        }
    }
    if g > 0 {
        gaps.push(g);
    }
    let n = per_sec.len() as u64;
    let q: u64 = gaps.iter().filter(|&&x| x >= 10).map(|&x| x as u64).sum();
    gaps.sort_unstable();
    let c = gaps.len();
    let median_ds = if c == 0 {
        0
    } else if c % 2 == 1 {
        gaps[c / 2] * 10
    } else {
        (gaps[c / 2 - 1] + gaps[c / 2]) * 5
    };
    Quiet {
        busy,
        quiet10_pm: (q * 1000).checked_div(n).unwrap_or(0) as u32,
        median_gap_ds: median_ds,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nl::Tuple;
    use std::net::IpAddr;

    #[test]
    fn quiet_matches_power_sample() {
        // 3 busy seconds, quiet runs 2, 12, 1 (worked by hand with the power-sample awk)
        let v = [0, 0, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 2];
        let q = quiet_stats(&v);
        assert_eq!(q.busy, 3);
        assert_eq!(q.quiet10_pm, 12 * 1000 / 18);
        assert_eq!(q.median_gap_ds, 20);
        assert_eq!(
            quiet_stats(&[]),
            Quiet {
                busy: 0,
                quiet10_pm: 0,
                median_gap_ds: 0
            }
        );
        assert_eq!(quiet_stats(&[0, 0, 0, 0]).median_gap_ds, 40);
        assert_eq!(quiet_stats(&[1, 0, 1, 0, 0, 0, 1]).median_gap_ds, 20);
    }

    fn flow(id: u32, dport: u16, pkts: u64) -> Flow {
        let t = |a: &str, b: &str, sp, dp| Tuple {
            src: a.parse().unwrap(),
            dst: b.parse().unwrap(),
            proto: 17,
            sport: sp,
            dport: dp,
        };
        Flow {
            id,
            orig: t("10.0.0.2", "203.0.113.1", 40000 + id as u16, dport),
            reply: t("203.0.113.1", "10.0.0.2", dport, 40000 + id as u16),
            pkts,
            bytes: pkts,
        }
    }

    #[test]
    fn breakers() {
        let wan: IpAddr = "10.0.0.2".parse().unwrap();
        let ctx = Snapshot {
            wan_addrs: [wan].into_iter().collect(),
            local_addrs: [wan].into_iter().collect(),
            ..Default::default()
        };
        let mut w = Window::new("rmnet_data0", 100);
        // second 0 = baseline; then: keepalive every 5 s alone, plus one burst with DNS at s=3
        let mut ka = 0u64;
        let mut dns = 0u64;
        let mut wanp = 0u64;
        for sec in 0..=20u64 {
            if sec > 0 && sec % 5 == 0 {
                ka += 1;
                wanp += 1;
            }
            if sec == 3 {
                dns += 2;
                ka += 1;
                wanp += 3;
            }
            let mut flows = vec![flow(1, 41641, ka), flow(2, 53, dns)];
            // inside tailscale0: the proxy to the home exit, moving with every keepalive
            let t = |a: &str, b: &str, sp, dp| Tuple {
                src: a.parse().unwrap(),
                dst: b.parse().unwrap(),
                proto: 6,
                sport: sp,
                dport: dp,
            };
            flows.push(Flow {
                id: 9,
                orig: t("100.100.1.2", "100.64.0.9", 50000, 7890),
                reply: t("100.64.0.9", "100.100.1.2", 7890, 50000),
                pkts: ka,
                bytes: ka,
            });
            w.push(&Second {
                wan_pkts: wanp,
                flows: &flows,
                destroyed: &[],
                power_uw: Some(600_000),
                screen_on: false,
                self_ctxt: sec,
                ctx: &ctx,
            });
        }
        let r = w.result(Default::default(), "time");
        assert_eq!(r.secs, 20);
        assert_eq!(r.wan_busy_secs, 5); // s3, s5, s10, s15, s20
        let ka_row = r.breakers.iter().find(|b| b.flow.port == 41641).unwrap();
        assert_eq!(
            (ka_row.busy_secs, ka_row.solo_secs, ka_row.gaps_ended),
            (5, 4, 4)
        );
        let dns_row = r.breakers.iter().find(|b| b.flow.port == 53).unwrap();
        assert_eq!(
            (dns_row.busy_secs, dns_row.solo_secs, dns_row.gaps_ended),
            (1, 0, 1)
        );
        assert_eq!(r.breakers[0].flow.port, 41641);
        assert_eq!(r.mw, Some(600));
        assert_eq!(r.self_wakes, 20);
        assert_eq!(r.unattributed_secs, 0);
        assert_eq!(r.inner.len(), 1);
        assert_eq!(
            (
                r.inner[0].busy_secs,
                r.inner[0].solo_secs,
                r.inner[0].flow.port
            ),
            (5, 5, 7890)
        );
    }
}
