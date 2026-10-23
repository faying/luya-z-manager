//! Tick, start-up and extend tests (`back_home` starts from the state the host's `run_job`
//! leaves). The request and parsing tests live with the host.

use super::*;

mod replay;

#[derive(Default)]
struct Fake {
    env: Items,
    applied: Vec<Items>,
    apply_fail: bool,
    probes: Vec<Option<Option<u32>>>,
    checks: Vec<Result<(), Fail>>,
    pre: Option<Fail>,
    alerts: Vec<String>,
    saves: usize,
}

impl Io for Fake {
    fn probe_home(&mut self) -> Option<Option<u32>> {
        if self.probes.is_empty() {
            Some(Some(40))
        } else {
            self.probes.remove(0)
        }
    }
    fn apply(&mut self, items: Items) -> Result<(), String> {
        if self.apply_fail {
            return Err("mihomo -t: bad".into());
        }
        self.env = items;
        self.applied.push(items);
        Ok(())
    }
    fn check(&mut self) -> Result<(), Fail> {
        if self.checks.is_empty() {
            Ok(())
        } else {
            self.checks.remove(0)
        }
    }
    fn precheck(&mut self) -> Result<(), Fail> {
        self.pre.map_or(Ok(()), Err)
    }
    fn alert(&mut self, kind: &str, _text: &str) {
        self.alerts.push(kind.into());
    }
    fn save(&mut self, _p: &Persist) {
        self.saves += 1;
    }
}

const T0: u64 = 1_000_000;

fn on(items: &[Item]) -> Items {
    items.iter().fold(Items::default(), |a, i| a.with(*i, true))
}

fn obs(now: u64, mcc: Option<i64>, env: Items) -> Obs {
    Obs {
        now,
        serving_mcc: mcc,
        env,
        clients: None,
        ..Default::default()
    }
}

#[test]
fn rest_ends_after_two_hours() {
    let mut m = Machine::default();
    m.p.items = on(&[Item::Rest]);
    m.p.rest_until = T0 + REST_SECS;
    let mut io = Fake {
        env: m.p.items,
        ..Default::default()
    };
    m.tick(
        &obs(T0 + REST_SECS - 1, Some(440), on(&[Item::Rest])),
        &mut io,
    );
    assert!(m.p.items.rest);
    m.tick(&obs(T0 + REST_SECS, Some(440), on(&[Item::Rest])), &mut io);
    assert!(!m.p.items.rest);
    assert_eq!(m.p.notice.kind, "expired");
}

/// DD8: extend before the end, not after; the D7 auto-off still applies.
#[test]
fn extend() {
    let mut m = Machine::default();
    let mut io = Fake::default();
    assert_eq!(m.extend(T0, &mut io), Err("not_on"));
    m.p.items = on(&[Item::Rest]);
    m.p.rest_until = T0 + 300;
    assert_eq!(m.extend(T0, &mut io), Ok(T0 + REST_SECS));
    m.p.rest_until = T0;
    assert_eq!(m.extend(T0 + 1, &mut io), Err("not_on"));
    // extended, then back home: still closes after 10 minutes
    m.p.rest_until = T0 + REST_SECS;
    let env = on(&[Item::Rest]);
    m.tick(&obs(T0, Some(460), env), &mut io);
    m.tick(&obs(T0 + BACK_HOME_SECS, Some(460), env), &mut io);
    assert!(!m.p.items.rest);
}

#[test]
fn three_failed_probes_close_rest_only() {
    let mut m = Machine::default();
    m.p.items = on(&[Item::Rest, Item::Media]);
    m.p.rest_until = T0 + 10 * REST_SECS;
    let env = m.p.items;
    let mut io = Fake {
        env,
        probes: vec![Some(None), None, Some(None), Some(None)],
        ..Default::default()
    };
    // probe 1 fails; a mihomo silence in between does not count
    m.tick(&obs(T0, Some(440), env), &mut io);
    m.tick(&obs(T0 + PROBE_EVERY, Some(440), env), &mut io);
    m.tick(&obs(T0 + 2 * PROBE_EVERY, Some(440), env), &mut io);
    assert!(m.p.items.rest && m.home_ok == Some(false));
    m.tick(&obs(T0 + 3 * PROBE_EVERY, Some(440), env), &mut io);
    assert_eq!(m.p.items, on(&[Item::Media]));
    assert_eq!(m.p.notice.kind, "home_down");
    assert_eq!(io.alerts, vec!["home-exit-down"]);
}

#[test]
fn a_good_probe_resets_the_count() {
    let mut m = Machine::default();
    m.p.items = on(&[Item::Rest]);
    m.p.rest_until = T0 + 10 * REST_SECS;
    let env = m.p.items;
    let mut io = Fake {
        probes: vec![Some(None), Some(None), Some(Some(50)), Some(None)],
        ..Default::default()
    };
    for k in 0..4 {
        m.tick(&obs(T0 + k * PROBE_EVERY, Some(440), env), &mut io);
    }
    assert!(m.p.items.rest);
}

/// §16: no client pauses the probe (and its miss count); the first client
/// probes on that tick, then every 5 minutes again; unknown probes.
#[test]
fn no_client_pauses_the_probe() {
    let mut m = Machine::default();
    m.p.items = on(&[Item::Rest]);
    m.p.rest_until = T0 + 10 * REST_SECS;
    let env = m.p.items;
    let mut io = Fake {
        probes: vec![
            Some(None),
            Some(None),
            Some(Some(40)),
            Some(None),
            Some(Some(41)),
        ],
        ..Default::default()
    };
    let at = |t: u64, n: Option<i64>| Obs {
        clients: n,
        ..obs(t, Some(440), env)
    };
    m.tick(&at(T0, Some(1)), &mut io);
    m.tick(&at(T0 + PROBE_EVERY, Some(2)), &mut io);
    assert_eq!((io.probes.len(), m.fails), (3, 2));
    for k in 2..20 {
        m.tick(&at(T0 + k * PROBE_EVERY, Some(0)), &mut io);
    }
    assert_eq!((io.probes.len(), m.fails, m.p.items.rest), (3, 0, true));
    // a client connects 30 s after a pause tick: probe right away
    let t = T0 + 19 * PROBE_EVERY + 30;
    m.tick(&at(t, Some(1)), &mut io);
    assert_eq!((io.probes.len(), m.home_ms, m.probed_at), (2, Some(40), t));
    m.tick(&at(t + 30, Some(1)), &mut io);
    assert_eq!(io.probes.len(), 2);
    m.tick(&at(t + PROBE_EVERY, None), &mut io);
    assert_eq!(io.probes.len(), 1);
}

/// D7: 9 minutes no, 10 yes; null never counts; ip unaffected; reopened kept.
#[test]
fn back_home() {
    let env = on(&[Item::Media, Item::Rest, Item::Ip]);
    let fresh = || {
        let mut m = Machine::default();
        m.p.items = env;
        m.p.rest_until = T0 + 10 * REST_SECS;
        m
    };
    let mut m = fresh();
    let mut io = Fake::default();
    m.tick(&obs(T0, Some(460), env), &mut io);
    m.tick(&obs(T0 + 540, Some(460), env), &mut io);
    assert_eq!(m.p.items, env, "9 minutes");
    m.tick(&obs(T0 + 600, Some(460), env), &mut io);
    assert_eq!(m.p.items, on(&[Item::Ip]), "10 minutes; ip stays");
    assert_eq!(m.p.notice.kind, "back_home");

    // unknown in between restarts the count
    let mut m = fresh();
    m.tick(&obs(T0, Some(460), env), &mut io);
    m.tick(&obs(T0 + 300, None, env), &mut io);
    m.tick(&obs(T0 + 600, Some(460), env), &mut io);
    m.tick(&obs(T0 + 1000, Some(460), env), &mut io);
    assert_eq!(m.p.items, env);
    m.tick(&obs(T0 + 1200, Some(460), env), &mut io);
    assert_eq!(m.p.items, on(&[Item::Ip]));

    // opened by hand in China: left alone (the host gets here through run_job;
    // this is the state it leaves)
    let mut io = Fake::default();
    let mut m = Machine::default();
    m.p.items = on(&[Item::Media]);
    m.p.reopened_in_china = on(&[Item::Media]);
    m.tick(&obs(T0, Some(460), on(&[Item::Media])), &mut io);
    m.tick(&obs(T0 + 700, Some(460), on(&[Item::Media])), &mut io);
    assert!(m.p.items.media);
    // abroad again forgets it
    m.tick(&obs(T0 + 800, Some(440), on(&[Item::Media])), &mut io);
    assert!(!m.p.reopened_in_china.media);
}

/// A2: what was on when nothing was watching.
#[test]
fn startup_reconcile() {
    // no saved state but env on → off
    let mut m = Machine::default();
    let mut io = Fake::default();
    m.startup(false, on(&[Item::Rest, Item::Media]), T0, &mut io);
    assert_eq!(m.p.items, Items::default());
    assert_eq!(io.applied, vec![Items::default()]);
    assert_eq!(m.p.notice.kind, "stale");

    // rest past its time → off, media kept
    let mut m = Machine::default();
    m.p.items = on(&[Item::Rest, Item::Media]);
    m.p.rest_until = T0 - 1;
    let mut io = Fake::default();
    m.startup(true, on(&[Item::Rest, Item::Media]), T0, &mut io);
    assert_eq!(m.p.items, on(&[Item::Media]));

    // still in time → adopt
    let mut m = Machine::default();
    m.p.rest_until = T0 + 60;
    let mut io = Fake::default();
    m.startup(true, on(&[Item::Rest]), T0, &mut io);
    assert_eq!(m.p.items, on(&[Item::Rest]));
    assert!(io.applied.is_empty());

    // nothing on, nothing to do
    let mut m = Machine::default();
    let mut io = Fake::default();
    m.startup(false, Items::default(), T0, &mut io);
    assert!(io.applied.is_empty() && io.saves == 0);
}

#[test]
fn tick_follows_a_hand_edited_env() {
    let mut m = Machine::default();
    let mut io = Fake::default();
    m.tick(&obs(T0, None, on(&[Item::Rest])), &mut io);
    assert!(m.p.items.rest);
    assert_eq!(m.p.rest_until, T0 + REST_SECS);
    m.tick(&obs(T0 + 30, None, Items::default()), &mut io);
    assert_eq!(m.p.rest_until, 0);
}
