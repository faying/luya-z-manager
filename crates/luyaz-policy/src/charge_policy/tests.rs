//! Moved from zte-agent `src/charge_policy.rs` (D7): `plug_in_during_fallback_enforced_once`,
//! `unknown_never_counts_as_unplugged`, `periodic_enforce_while_connected_and_waits_capped`, as
//! they were. `enforce` had no unit tests in the agent (it read sysfs and ubus inline); the cases
//! below follow its code.

use super::*;

fn s(x: u64) -> Duration {
    Duration::from_secs(x)
}

/// 拔电 → datad 断开 → 插电 → datad 恢复: the plug-in during fallback is
/// acted on within 60 s, and the recovery does not act on it again.
#[test]
fn plug_in_during_fallback_enforced_once() {
    let mut d = Driver::default();
    let mut direct_calls = 0;
    let mut plugged = false;
    let mut acts: Vec<(u64, PlugStep)> = Vec::new();

    // Subscribed, unplugged. Start: one "unplugged" (re-enable check).
    let mut now = 0u64;
    let p = d.pass(s(now), Some(false), true, true, &mut || {
        panic!("no direct read while subscribed")
    });
    assert_eq!(p.plug, Some(PlugStep::Unplugged));
    // Unplug is old news: nothing more while subscribed.
    now = 30;
    assert_eq!(
        d.pass(s(now), Some(false), false, true, &mut || unreachable!())
            .plug,
        None
    );

    // datad drops at 100: the feed has no answer; the loop polls directly.
    // Plug-in lands at 101, just after a direct read — the worst case.
    let plug_at = 101;
    now = 100;
    let mut wait;
    loop {
        if now >= plug_at {
            plugged = true;
        }
        let mut direct = || {
            direct_calls += 1;
            Some(plugged)
        };
        let p = d.pass(s(now), None, now == 100, true, &mut direct);
        if let Some(step) = p.plug {
            acts.push((now, step));
        }
        wait = p.wait;
        assert!(wait <= MAX_WAIT);
        if !acts.is_empty() {
            break;
        }
        now += wait.as_secs().max(1);
        assert!(now < 400, "never acted");
    }
    let (at, step) = acts[0];
    assert_eq!(step, PlugStep::PluggedIn);
    assert!(at - plug_at <= 60, "acted {}s after plug-in", at - plug_at);
    assert!(
        direct_calls <= 3,
        "low-frequency direct reads, got {direct_calls}"
    );

    // datad back at 200: the snapshot says connected. Not a new plug-in.
    for t in [200, 230, 260, 300, 400] {
        let p = d.pass(s(t), Some(true), t == 200, true, &mut || {
            panic!("subscribed: no direct")
        });
        assert_eq!(
            p.plug, None,
            "recovery at {t} must not repeat the plug-in action"
        );
    }
}

#[test]
fn unknown_never_counts_as_unplugged() {
    let mut d = Driver::default();
    assert_eq!(
        d.pass(s(0), Some(true), true, true, &mut || None).plug,
        Some(PlugStep::PluggedIn)
    );
    // Fallback, direct read fails: no transition, no re-enable.
    for t in [10, 40, 80, 120] {
        assert_eq!(d.pass(s(t), None, false, true, &mut || None).plug, None);
    }
}

#[test]
fn periodic_enforce_while_connected_and_waits_capped() {
    let mut d = Driver::default();
    d.pass(s(0), Some(true), true, true, &mut || None);
    let p = d.pass(s(30), Some(true), false, true, &mut || None);
    assert!(!p.enforce);
    assert_eq!(p.wait, MAX_WAIT);
    assert!(d.pass(s(60), Some(true), false, true, &mut || None).enforce);
    // A battery/charger change while connected enforces at once.
    assert!(d.pass(s(70), Some(true), true, true, &mut || None).enforce);
    // Fallback: next pass no later than the next direct read.
    let p = d.pass(s(80), None, true, true, &mut || Some(true));
    assert!(p.wait <= FALLBACK_POLL);
}

fn b(capacity: u8, discharging: bool, stopped: bool) -> Battery {
    Battery {
        capacity,
        discharging,
        stopped,
        limit: 80,
        hysteresis: 5,
    }
}

#[test]
fn enforce_stops_at_the_limit_and_resumes_below_the_band() {
    // charging, below the limit: nothing
    assert_eq!(enforce(&b(79, false, false)), None);
    // at or over the limit: stop
    assert_eq!(enforce(&b(80, false, false)), Some(false));
    assert_eq!(enforce(&b(95, false, false)), Some(false));
    // stopped, inside the band: stay stopped
    assert_eq!(enforce(&b(76, false, true)), None);
    assert_eq!(enforce(&b(80, false, true)), None);
    // stopped, at limit − hysteresis or below: resume
    assert_eq!(enforce(&b(75, false, true)), Some(true));
    assert_eq!(enforce(&b(40, true, true)), Some(true));
    // discharging by itself (not stopped by us): leave it, even over the limit
    assert_eq!(enforce(&b(90, true, false)), None);
    // unreadable capacity (0) while stopped: resume, as the agent did
    assert_eq!(enforce(&b(0, false, true)), Some(true));
    // limit below the hysteresis band does not wrap
    assert_eq!(
        enforce(&Battery {
            capacity: 0,
            discharging: false,
            stopped: true,
            limit: 3,
            hysteresis: 5
        }),
        Some(true)
    );
}

#[test]
fn unplug_resumes_only_when_stopped() {
    assert_eq!(on_unplug(true), Some(true));
    assert_eq!(on_unplug(false), None);
}
