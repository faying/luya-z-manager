//! The shadow's steps (what the agent's loop would have written), the pairing with the agent's
//! writes, and (with `LUYAZ_TEST_PLUGINS`) the component against the built-in on every input.

use super::*;

fn fresh() -> State {
    let d = std::env::temp_dir().join(format!("u60d-charge-none-{}", std::process::id()));
    load(&d)
}

const ON: charge::Limit = charge::Limit {
    enabled: true,
    limit: 80,
    hysteresis: 5,
};

fn obs(connected: bool, capacity: u8, stopped: bool) -> Observed {
    Observed {
        connected: Some(connected),
        changed: true,
        stopped,
        capacity,
        discharging: false,
        owned: false,
    }
}

fn whys(g: &State) -> Vec<(bool, &'static str)> {
    g.proposals.iter().map(|e| (e.allow, e.why)).collect()
}

#[test]
fn plugged_in_over_the_limit_proposes_a_stop_once() {
    let mut g = fresh();
    step(&mut g, obs(true, 85, false), ON, false, 0.0, 1);
    // the charger block has not caught up yet: the same decision, not recorded again
    step(&mut g, obs(true, 85, false), ON, false, 5.0, 2);
    step(&mut g, obs(true, 85, false), ON, false, 10.0, 3);
    assert_eq!(whys(&g), vec![(false, "plugged_in")]);
    // stopped now; down to the band's bottom: resume
    step(&mut g, obs(true, 80, true), ON, false, 15.0, 4);
    step(&mut g, obs(true, 75, true), ON, false, 300.0, 5);
    assert_eq!(whys(&g), vec![(false, "plugged_in"), (true, "limit")]);
}

#[test]
fn unplug_resumes_when_stopped_even_with_the_limit_off() {
    let mut g = fresh();
    let off = charge::Limit {
        enabled: false,
        ..ON
    };
    step(&mut g, obs(true, 90, true), off, false, 0.0, 1);
    assert!(g.proposals.is_empty(), "limit off: no enforcement");
    step(&mut g, obs(false, 90, true), off, false, 5.0, 2);
    assert_eq!(whys(&g), vec![(true, "unplugged")]);
}

#[test]
fn manual_override_or_unknown_plug_state_proposes_nothing() {
    let mut g = fresh();
    step(&mut g, obs(true, 95, false), ON, true, 0.0, 1);
    step(&mut g, obs(true, 95, false), ON, true, 100.0, 2);
    assert!(g.proposals.is_empty(), "{:?}", g.proposals);
    // never known to be plugged in: nothing either (once it was, the agent keeps enforcing
    // periodically through a stale feed, and so does the shadow)
    let mut g = fresh();
    let unknown = Observed {
        connected: None,
        ..obs(true, 95, false)
    };
    step(&mut g, unknown, ON, false, 0.0, 1);
    step(&mut g, unknown, ON, false, 200.0, 2);
    assert!(g.proposals.is_empty(), "{:?}", g.proposals);
}

#[test]
fn discharging_by_itself_is_left_alone() {
    let mut g = fresh();
    let o = Observed {
        discharging: true,
        ..obs(true, 95, false)
    };
    step(&mut g, o, ON, false, 0.0, 1);
    step(&mut g, o, ON, false, 70.0, 2);
    assert!(g.proposals.is_empty());
}

fn ev(at: f64, allow: bool) -> Event {
    Event {
        at,
        wall: at as u64,
        allow,
        why: "",
    }
}

#[test]
fn pairs_within_the_window_each_once() {
    let shadow: VecDeque<Event> = [ev(10.0, false), ev(100.0, true), ev(500.0, false)].into();
    // the agent: same stop 3 s later, a resume 50 s off (too far), and one the shadow never had
    let agent: VecDeque<Event> = [ev(13.0, false), ev(150.0, true), ev(300.0, false)].into();
    let (matched, agent_only, shadow_only) = compare(&shadow, &agent, 1000.0);
    assert_eq!(matched, 1);
    assert_eq!(
        agent_only.iter().map(|e| e.at).collect::<Vec<_>>(),
        vec![150.0, 300.0]
    );
    assert_eq!(
        shadow_only.iter().map(|e| e.at).collect::<Vec<_>>(),
        vec![100.0, 500.0]
    );
    // younger than the window: not judged yet
    let (_, a, s) = compare(&shadow, &agent, 505.0);
    assert!(s.iter().all(|e| e.at != 500.0) && a.len() == 2);
}

#[test]
fn limit_file_reads_like_the_agent() {
    use charge::{Limit, parse};
    let l = |enabled, limit, hysteresis| Limit {
        enabled,
        limit,
        hysteresis,
    };
    assert_eq!(parse(None), Limit::default());
    assert_eq!(
        parse(Some(r#"{"enabled":true,"limit":80,"hysteresis":3}"#)),
        l(true, 80, 3)
    );
    assert_eq!(
        parse(Some(r#"{"enabled":true,"limit":80}"#)),
        l(true, 80, 5)
    );
    assert_eq!(parse(Some(r#"{"limit":80}"#)), Limit::default());
    assert_eq!(parse(Some(r#"{"enabled":true}"#)), Limit::default());
    assert_eq!(
        parse(Some(r#"{"enabled":true,"limit":20,"hysteresis":0}"#)),
        l(true, 50, 1)
    );
    assert_eq!(
        parse(Some(r#"{"enabled":true,"limit":120,"hysteresis":40}"#)),
        l(true, 100, 20)
    );
    assert_eq!(
        parse(Some(r#"{"enabled":true,"limit":300}"#)),
        Limit::default()
    );
    assert_eq!(
        parse(Some(r#"{"enabled":true,"limit":80,"hysteresis":-1}"#)),
        Limit::default()
    );
    assert_eq!(parse(Some("garbage")), Limit::default());
}

#[test]
fn abi_is_the_wit_package() {
    let wit = include_str!("../../../../luyaz-policy/wit/charge-policy.wit");
    let line = wit.lines().find(|l| l.starts_with("package ")).unwrap();
    assert_eq!(line, format!("package {ABI};"));
}

#[test]
fn component_matches_the_builtin_on_every_input() {
    let Some(dir) = std::env::var_os("LUYAZ_TEST_PLUGINS") else {
        assert!(
            std::env::var_os("LUYAZ_REQUIRE_PLUGINS").is_none(),
            "LUYAZ_REQUIRE_PLUGINS set without LUYAZ_TEST_PLUGINS"
        );
        return;
    };
    let (report, ok) = check(std::path::Path::new(&dir));
    assert!(ok, "{report}");
    assert_eq!(report["cases"], all_asks().len());
}

#[test]
fn owned_the_loop_hands_out_jobs_instead_of_proposing() {
    let mut g = fresh();
    let owned = |connected, capacity, stopped| Observed {
        owned: true,
        ..obs(connected, capacity, stopped)
    };
    assert_eq!(
        step(&mut g, owned(true, 85, false), ON, false, 0.0, 1),
        vec![Job::PluggedIn]
    );
    // a battery/charger change while plugged in: enforce
    assert_eq!(
        step(&mut g, owned(true, 86, false), ON, false, 5.0, 2),
        vec![Job::Limit]
    );
    // manual override: plug-in noticed, nothing enforced
    assert_eq!(
        step(&mut g, owned(false, 86, true), ON, true, 10.0, 3),
        vec![Job::Unplugged]
    );
    assert_eq!(
        step(&mut g, owned(true, 86, false), ON, true, 15.0, 4),
        vec![]
    );
    assert!(g.proposals.is_empty(), "{:?}", g.proposals);
}
