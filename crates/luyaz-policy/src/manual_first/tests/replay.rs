//! Replaying a recorded tick (the agent's recorder wraps its real `Io` the same way as `Rec`).

use super::*;

/// Records every call and answer of the wrapped `Io`, as the agent's recorder does.
struct Rec<'a> {
    inner: &'a mut dyn Io,
    events: Vec<Event>,
}

impl Io for Rec<'_> {
    fn in_use(&mut self) -> bool {
        let got = self.inner.in_use();
        self.events.push(Event::InUse { got });
        got
    }
    fn probe(&mut self, node: &str) -> Option<Probe> {
        let got = self.inner.probe(node);
        self.events.push(Event::Probe {
            node: node.into(),
            got,
        });
        got
    }
    fn cell_alive(&mut self) -> bool {
        let got = self.inner.cell_alive();
        self.events.push(Event::CellAlive { got });
        got
    }
    fn direct_ms(&mut self) -> Option<u32> {
        let got = self.inner.direct_ms();
        self.events.push(Event::DirectMs { got });
        got
    }
    fn region_of(&mut self, node: &str) -> Option<String> {
        let got = self.inner.region_of(node);
        self.events.push(Event::RegionOf {
            node: node.into(),
            got: got.clone(),
        });
        got
    }
    fn retest(&mut self, group: &str) -> Option<(String, bool)> {
        let got = self.inner.retest(group);
        self.events.push(Event::Retest {
            group: group.into(),
            got: got.clone(),
        });
        got
    }
    fn select_main(&mut self, member: &str) -> bool {
        let ok = self.inner.select_main(member);
        self.events.push(Event::SelectMain {
            member: member.into(),
            ok,
        });
        ok
    }
    fn drop_manual_conns(&mut self) {
        self.inner.drop_manual_conns();
        self.events.push(Event::DropManualConns);
    }
    fn save(&mut self, p: &Persist) {
        self.inner.save(p);
        self.events.push(Event::Save);
    }
}

/// Small deterministic generator (no rand crate here).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn pick<T: Clone>(&mut self, xs: &[T]) -> T {
        xs[(self.next() % xs.len() as u64) as usize].clone()
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.next() % 100 < pct
    }
}

/// An `Io` answering at random (but plausibly).
struct Random<'a>(&'a mut Rng);

impl Io for Random<'_> {
    fn in_use(&mut self) -> bool {
        self.0.chance(70)
    }
    fn probe(&mut self, _n: &str) -> Option<Probe> {
        self.0.pick(&[
            Some(Probe::Ms(90)),
            Some(Probe::Ms(90)),
            Some(Probe::Ms(90)),
            Some(Probe::Ms(900)),
            Some(Probe::Timeout),
            Some(Probe::Timeout),
            None,
            Some(Probe::Ms(800)),
        ])
    }
    fn cell_alive(&mut self) -> bool {
        self.0.chance(85)
    }
    fn direct_ms(&mut self) -> Option<u32> {
        self.0.pick(&[Some(60), Some(60), Some(801), None])
    }
    fn region_of(&mut self, _n: &str) -> Option<String> {
        self.0
            .pick(&[Some(JP.to_string()), None, Some("🇺🇸 美国".to_string())])
    }
    fn retest(&mut self, _g: &str) -> Option<(String, bool)> {
        self.0.pick(&[
            Some(("JP 01".to_string(), true)),
            Some((NODE.to_string(), true)),
            Some(("JP 01".to_string(), false)),
            None,
        ])
    }
    fn select_main(&mut self, _m: &str) -> bool {
        self.0.chance(90)
    }
    fn drop_manual_conns(&mut self) {}
    fn save(&mut self, _p: &Persist) {}
}

/// Mostly what mihomo would show next (🚀 stays where the machine put it), sometimes the owner
/// moving things, so every branch comes up.
fn random_obs(r: &mut Rng, m: &Machine, prev: &Obs, now: u64, storm: bool) -> Obs {
    if storm {
        // a dead node the owner keeps putting back by hand: reaches the hourly switch cap
        let main = if m.p.backup.is_empty() || r.chance(30) {
            MANUAL_GROUP
        } else {
            m.p.backup.as_str()
        };
        return Obs {
            now,
            active: true,
            main_now: main.into(),
            manual_now: NODE.into(),
        };
    }
    if m.p.backup.is_empty() && !prev.main_now.is_empty() && r.chance(85) {
        // the owner mostly leaves things as they are
        return Obs {
            now,
            ..prev.clone()
        };
    }
    let main = if !m.p.backup.is_empty() && r.chance(80) {
        m.p.backup.as_str()
    } else {
        r.pick(&[
            MANUAL_GROUP,
            MANUAL_GROUP,
            MANUAL_GROUP,
            MANUAL_GROUP,
            MANUAL_GROUP,
            JP,
            "🇹🇼 台湾",
            "DIRECT",
            "",
        ])
    };
    let manual = r.pick(&[
        NODE,
        NODE,
        NODE,
        NODE,
        NODE,
        NODE,
        NODE,
        NODE,
        "JP 07",
        "我的节点 ✈",
        "",
    ]);
    Obs {
        now,
        active: r.chance(92),
        main_now: main.into(),
        manual_now: manual.into(),
    }
}

/// Every random tick, recorded through `Rec` and replayed from the machine before it, comes out
/// the same: same machine after, the same calls with the same answers, nothing left over.
#[test]
fn random_ticks_replay_exactly() {
    let mut r = Rng(0x5eed_0060);
    let mut kinds = std::collections::BTreeSet::new();
    let mut notices = std::collections::BTreeSet::new();
    for run in 0..60 {
        let mut m = Machine::default();
        let mut prev = Obs::default();
        let mut now = 1_000 + run * 7;
        for _ in 0..1000 {
            // half the runs stay inside the hour, so the switch cap comes up too
            now += if run % 2 == 0 {
                r.pick(&[10, 30, 60, 130])
            } else {
                r.pick(&[10, 10, 30, 60, 60, 130, 130, 700, 3600])
            };
            let o = random_obs(&mut r, &m, &prev, now, run % 4 == 0);
            prev = o.clone();
            let before = m.clone();
            let mut rng2 = Rng(r.next() | 1);
            let mut inner = Random(&mut rng2);
            let mut rec = Rec {
                inner: &mut inner,
                events: Vec::new(),
            };
            m.tick(&o, &mut rec);
            let events = rec.events;
            for e in &events {
                kinds.insert(e.name());
            }
            notices.insert(m.p.notice_kind.clone());
            let out = replay(&before, &o, events.clone());
            assert_eq!(out.diverged, None);
            assert_eq!(out.unused, 0);
            assert_eq!(out.calls, events);
            assert_eq!(out.after, m);
        }
    }
    assert_eq!(kinds.len(), 9, "every Io call came up: {kinds:?}");
    for k in ["backup", "back", "alldown", "repick", "unstable"] {
        assert!(notices.contains(k), "{k} never came up: {notices:?}");
    }
}

/// The record says the agent asked something else: that is reported, and nothing more is
/// answered (the tick goes inert instead of acting on made-up answers).
#[test]
fn a_different_call_is_a_divergence() {
    let m = Machine {
        last_probe: 0,
        ..Default::default()
    };
    let o = Obs {
        now: 1000,
        active: true,
        main_now: MANUAL_GROUP.into(),
        manual_now: NODE.into(),
    };
    let rec = vec![
        Event::InUse { got: true },
        Event::Probe {
            node: "JP 07".into(),
            got: Some(Probe::Timeout),
        },
    ];
    let out = replay(&m, &o, rec);
    assert_eq!(out.diverged, Some((1, "probe(JP 03)".into())));
    assert_eq!(out.after.timeouts, 0, "no answer = could not ask");
    assert_eq!(out.unused, 0);
}

#[test]
fn the_agent_asking_more_is_counted() {
    let m = Machine {
        last_probe: 990,
        ..Default::default()
    };
    let o = Obs {
        now: 1000,
        active: true,
        main_now: MANUAL_GROUP.into(),
        manual_now: NODE.into(),
    };
    // within the minute: this copy asks nothing, the record has a probe
    let out = replay(&m, &o, vec![Event::InUse { got: true }]);
    assert_eq!((out.diverged, out.unused), (None, 1));
}
