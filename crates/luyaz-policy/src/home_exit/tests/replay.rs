//! Replaying recorded ticks and start-ups (the agent's recorder wraps its real `Io` like `Rec`).

use super::*;

struct Rec<'a> {
    inner: &'a mut dyn Io,
    events: Vec<Event>,
}

impl Io for Rec<'_> {
    fn probe_home(&mut self) -> Option<Option<u32>> {
        let got = self.inner.probe_home();
        self.events.push(probe_event(got));
        got
    }
    fn apply(&mut self, items: Items) -> Result<(), String> {
        let r = self.inner.apply(items);
        self.events.push(Event::Apply {
            items,
            err: r.clone().err(),
        });
        r
    }
    fn check(&mut self) -> Result<(), Fail> {
        let r = self.inner.check();
        self.events.push(Event::Check { fail: r.err() });
        r
    }
    fn precheck(&mut self) -> Result<(), Fail> {
        let r = self.inner.precheck();
        self.events.push(Event::Precheck { fail: r.err() });
        r
    }
    fn alert(&mut self, kind: &str, text: &str) {
        self.inner.alert(kind, text);
        self.events.push(Event::Alert { kind: kind.into() });
    }
    fn save(&mut self, p: &Persist) {
        self.inner.save(p);
        self.events.push(Event::Save);
    }
    fn cell_up(&mut self) -> Option<bool> {
        let up = self.inner.cell_up();
        self.events.push(Event::CellUp { up });
        up
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.next() % 100 < pct
    }
    fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[(self.next() % xs.len() as u64) as usize]
    }
}

struct Random<'a>(&'a mut Rng);

impl Io for Random<'_> {
    fn probe_home(&mut self) -> Option<Option<u32>> {
        self.0
            .pick(&[Some(Some(40)), Some(Some(90)), Some(None), Some(None), None])
    }
    fn apply(&mut self, _items: Items) -> Result<(), String> {
        if self.0.chance(85) {
            Ok(())
        } else {
            Err("mihomo -t: bad".into())
        }
    }
    fn check(&mut self) -> Result<(), Fail> {
        Ok(())
    }
    fn precheck(&mut self) -> Result<(), Fail> {
        Ok(())
    }
    fn alert(&mut self, _kind: &str, _text: &str) {}
    fn save(&mut self, _p: &Persist) {}
    fn cell_up(&mut self) -> Option<bool> {
        self.0.pick(&[Some(true), Some(false), None])
    }
}

fn items(r: &mut Rng) -> Items {
    Items {
        media: r.chance(40),
        ip: r.chance(40),
        rest: r.chance(50),
    }
}

/// Every random tick and start-up, recorded through `Rec` and replayed from the machine before
/// it, comes out the same; the run reaches every call and notice.
#[test]
fn random_steps_replay_exactly() {
    let mut r = Rng(0x0e3_0e3);
    let (mut calls, mut notices) = (
        std::collections::BTreeSet::new(),
        std::collections::BTreeSet::new(),
    );
    for run in 0..80 {
        let mut m = Machine::default();
        let mut now = 1_000_000 + run * 13;
        let mut env = Items::default();
        let mut mcc = Some(440);
        for k in 0..800 {
            let step = if k == 0 || r.chance(1) {
                if r.chance(50) {
                    m.p.rest_until = now + r.pick(&[0, 60, REST_SECS]);
                }
                Step::Startup {
                    had_file: r.chance(70),
                    env: items(&mut r),
                    now,
                }
            } else {
                now += r.pick(&[30, 30, 30, 30, 300, 600]);
                if r.chance(3) {
                    env = items(&mut r);
                } else {
                    env = m.p.items;
                }
                if r.chance(2) {
                    mcc = r.pick(&[Some(460), Some(440), None]);
                }
                if r.chance(5) {
                    m.p.reopened_in_china = items(&mut r);
                }
                let clients = r.pick(&[Some(0), Some(1), Some(3), None]);
                let screen_on = r.pick(&[Some(false), Some(true), None]);
                let cell_busy = r.pick(&[Some(false), Some(true), None]);
                Step::Tick {
                    obs: Obs {
                        now,
                        serving_mcc: mcc,
                        env,
                        clients,
                        screen_on,
                        cell_busy,
                    },
                }
            };
            let before = m.clone();
            let mut rng2 = Rng(r.next() | 1);
            let mut inner = Random(&mut rng2);
            let mut rec = Rec {
                inner: &mut inner,
                events: Vec::new(),
            };
            match &step {
                Step::Tick { obs } => m.tick(obs, &mut rec),
                Step::Startup { had_file, env, now } => m.startup(*had_file, *env, *now, &mut rec),
            }
            let events = rec.events;
            calls.extend(events.iter().map(Event::name));
            notices.insert(m.p.notice.kind.clone());
            let out = replay(&before, &step, events.clone());
            assert_eq!((out.diverged.clone(), out.unused), (None, 0), "{step:?}");
            assert_eq!(out.calls, events);
            assert_eq!(out.after, m);
        }
    }
    for c in ["probe_home", "apply", "alert", "save", "cell_up"] {
        assert!(calls.contains(c), "{c}: {calls:?}");
    }
    for k in ["stale", "expired", "back_home", "home_down"] {
        assert!(notices.contains(k), "{k}: {notices:?}");
    }
}

#[test]
fn a_different_apply_is_a_divergence() {
    let mut m = Machine::default();
    m.p.items = Items {
        rest: true,
        ..Default::default()
    };
    m.p.rest_until = 100;
    let step = Step::Tick {
        obs: Obs {
            now: 200,
            serving_mcc: Some(440),
            env: m.p.items,
            clients: Some(0),
            ..Default::default()
        },
    };
    // the agent turned media off instead of rest
    let rec = vec![Event::Apply {
        items: Items {
            media: false,
            ip: false,
            rest: true,
        },
        err: None,
    }];
    let out = replay(&m, &step, rec);
    assert_eq!(
        out.diverged,
        Some((0, "apply(media=false,ip=false,rest=false)".into()))
    );
    assert!(out.after.p.items.rest, "not applied = still on");
}
