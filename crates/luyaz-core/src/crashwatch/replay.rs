//! Replay (D11's parity check): a timeline of what the device did, run through the watcher the
//! way the IO edge would wake it, giving the events. `tools/crashwatch-parity.sh` ran the same
//! timeline through the guard's shell watcher and compared the two (until Phase 10, X2).
//!
//! Timeline lines (time in boottime seconds, decimals allowed; `#` comments):
//!
//! ```text
//! t <s>                      what follows happens at this time
//! k <seq> <kernel s> <text>  a kernel record (also: a raw `/dev/kmsg` line `<pri>,<seq>,<µs>,…;text`)
//! L up|down                  the WAN link: address and default route together
//! R <rx bytes> <packets>     WAN counters (packets each way)
//! D <name> <size>            a dump file appears or grows
//! S <n>                      the suspend counter
//! Z <s>                      the device sleeps this long (boottime runs, monotonic stops)
//! X                          the core restarts (state carried over as on the device)
//! E                          the end: wakes up to this time still happen
//! ```

use super::{Cap, Look, Net, Rec, Wake, Watch, parse_rec};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub enum Item {
    K(Rec),
    L(bool),
    R(u64, u64),
    D(String, u64),
    S(u64),
    Z(u64),
    X,
    E,
}

pub fn parse(text: &str) -> Result<Vec<(u64, Item)>, String> {
    let mut t = 0u64;
    let mut v = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let l = line.trim_end();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let bad = || format!("line {}: {l}", n + 1);
        let (cmd, rest) = l.split_once(' ').unwrap_or((l, ""));
        let num = |s: &str| s.parse::<u64>().map_err(|_| bad());
        let it = match cmd {
            "t" => {
                let s: f64 = rest.parse().map_err(|_| bad())?;
                t = (s * 1000.0).round() as u64;
                continue;
            }
            "k" => {
                let mut f = rest.splitn(3, ' ');
                let seq = num(f.next().unwrap_or(""))?;
                let ks = num(f.next().unwrap_or(""))?;
                Item::K(Rec {
                    seq,
                    ts_us: ks * 1_000_000,
                    text: f.next().unwrap_or("").to_string(),
                })
            }
            "L" => Item::L(rest == "up"),
            "R" => {
                let mut f = rest.split(' ');
                Item::R(num(f.next().unwrap_or(""))?, num(f.next().unwrap_or(""))?)
            }
            "D" => {
                let (a, b) = rest.rsplit_once(' ').ok_or_else(bad)?;
                Item::D(a.to_string(), num(b)?)
            }
            "S" => Item::S(num(rest)?),
            "Z" => Item::Z(num(rest)?),
            "X" => Item::X,
            "E" => Item::E,
            _ => match parse_rec(l) {
                Some(r) => Item::K(r),
                None => continue, // a continuation or torn line of a capture
            },
        };
        v.push((t, it));
    }
    Ok(v)
}

#[derive(Default)]
struct Sim {
    link: bool,
    rx: Option<u64>,
    pk: Option<u64>,
    dumps: BTreeMap<String, u64>,
    sus: Option<u64>,
}

impl Look for Sim {
    fn route(&mut self) -> bool {
        self.link
    }
    fn addr(&mut self) -> bool {
        self.link
    }
    fn netdev(&mut self) -> (Option<u64>, Option<u64>) {
        (self.rx, self.pk)
    }
    fn dumps(&mut self) -> Option<Vec<(String, u64)>> {
        Some(self.dumps.iter().map(|(a, b)| (a.clone(), *b)).collect())
    }
    fn suspend(&mut self) -> Option<u64> {
        self.sus
    }
    fn net(&mut self) -> Net {
        Net::default()
    }
}

/// What a replay gave: the events (in order) and how many times the watcher woke.
pub struct Out {
    pub events: Vec<Value>,
    pub wakes: u64,
    pub timer_wakes: u64,
}

/// Runs a timeline. `boot` names the boot (ids use its first 8 hex digits).
pub fn run(items: &[(u64, Item)], boot: &str, first_live: bool) -> Out {
    let mut sim = Sim {
        link: true,
        ..Default::default()
    };
    let mut ring: Vec<Rec> = Vec::new();
    let mut read_to = 0usize; // ring index the core has read up to
    let mut w = Watch::new(boot, first_live);
    let mut cap = Cap::default();
    let mut out = Out {
        events: Vec::new(),
        wakes: 0,
        timer_wakes: 0,
    };
    let mut i = 0usize;
    let mut slept_ms = 0u64;
    let mut asleep_until: Option<u64> = None;
    let mut now = items.first().map_or(0, |x| x.0);
    let end = items
        .iter()
        .rev()
        .find(|x| matches!(x.1, Item::E))
        .map_or_else(|| items.last().map_or(0, |x| x.0), |x| x.0);
    let mut first = true;
    let mut dump_changed = true;
    let mut link_changed = false;
    loop {
        // everything that has happened by `now`
        while i < items.len() && items[i].0 <= now {
            match &items[i].1 {
                Item::K(r) => ring.push(r.clone()),
                Item::L(u) => {
                    link_changed |= sim.link != *u;
                    sim.link = *u;
                }
                Item::R(a, b) => {
                    sim.rx = Some(*a);
                    sim.pk = Some(*b);
                }
                Item::D(n, s) => {
                    sim.dumps.insert(n.clone(), *s);
                    dump_changed = true;
                }
                Item::S(n) => sim.sus = Some(*n),
                Item::Z(s) => {
                    slept_ms += s * 1000;
                    asleep_until = Some(items[i].0 + s * 1000);
                }
                Item::X => {
                    let s = serde_json::to_string(&w).expect("state");
                    w = Watch::resume(&s, boot).expect("same boot");
                    cap = Cap::default();
                    read_to = 0; // a new reader starts at the ring's beginning
                    first = true;
                    dump_changed = true;
                }
                Item::E => {}
            }
            i += 1;
        }
        if let Some(a) = asleep_until {
            if now < a {
                now = a; // nothing runs while asleep
                continue;
            }
            asleep_until = None;
        }
        let unread = read_to < ring.len();
        let kmsg_ready = unread && !cap.masked(now);
        let timer_due =
            w.next_tick().is_some_and(|t| t <= now) || cap.deadline().is_some_and(|t| t <= now);
        if first || kmsg_ready || link_changed || dump_changed || timer_due {
            out.wakes += 1;
            if !(first || kmsg_ready || link_changed || dump_changed) {
                out.timer_wakes += 1;
            }
            let recs: Vec<Rec> = ring[read_to..].to_vec();
            read_to = ring.len();
            let got = !recs.is_empty();
            w.step(
                &Wake {
                    up_ms: now,
                    mono_ms: now - slept_ms,
                    recs,
                    nl_ok: true,
                    dump_changed,
                    dump_notify: true,
                },
                &mut sim,
            );
            cap.after(now, kmsg_ready && !first, got);
            out.events.append(&mut w.pending);
            first = false;
            dump_changed = false;
            link_changed = false;
        }
        // the next moment anything can happen
        let mut next: Option<u64> = items.get(i).map(|x| x.0);
        for t in [w.next_tick(), cap.deadline()].into_iter().flatten() {
            next = Some(next.map_or(t, |n| n.min(t)));
        }
        match next {
            Some(t) if t <= end => now = t.max(now + 1),
            _ => break,
        }
    }
    out
}
