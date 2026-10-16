//! Moved from zte-agent `src/scheduler.rs` (manager 84f3404): `forbidden_paths`,
//! `old_jobs_are_switched_off_with_the_reason`, `creating_one_is_refused`, as they were. The
//! `due_*` and `record_*` tests are new: `tick` had none (it read the clock and ran actions).

use super::*;

#[test]
fn forbidden_paths() {
    for (p, want) in [
        ("/api/esim/switch", Some("eSIM")),
        ("/api/esim/download", Some("eSIM")),
        ("/api/ESIM/switch", Some("eSIM")),
        ("/api/esim/switch/", Some("eSIM")),
        ("/api/esim/switch?x=1", Some("eSIM")),
        ("/api//esim/switch", Some("eSIM")),
        ("/api/esim", Some("eSIM")),
        ("/api/device/factory-reset", Some("factory reset")),
        ("/api/device/factory-reset/", Some("factory reset")),
        ("/api/system/kill-bloat", Some("kill-bloat")),
        ("/api/device/reboot", None),
        ("/api/device/poweroff", Some("power off")),
        ("/api/device/poweroff/", Some("power off")),
        ("/api/modem/network-mode", None),
        ("/api/wifi/radio", None),
        ("/api/esimx", None),
        ("/api/at/send", Some("AT terminal")),
        ("/api/AT/Send/", Some("AT terminal")),
        ("/api//at/send?x=1", Some("AT terminal")),
        ("/api/at/port", None),
        ("/api/modem/data", None),
        // batch 5c: u60d serves these routes now
        ("/api///device/poweroff", Some("power off")),
        ("/API/DEVICE/FACTORY-RESET?x=1", Some("factory reset")),
        ("/api/system//kill-bloat/", Some("kill-bloat")),
        ("/api/device/%70oweroff", Some("an encoded or dotted path")),
        ("/api/device/./factory-reset", Some("an encoded or dotted path")),
        ("/api/wifi/../at/send", Some("an encoded or dotted path")),
        ("/api/device/power-save", None),
    ] {
        assert_eq!(forbidden(p), want, "{p}");
    }
}

fn job(id: u32, path: &str, enabled: bool) -> Job {
    Job {
        id,
        name: format!("j{id}"),
        enabled,
        schedule: Schedule::Once { at: 2_000_000_000 },
        action: Action {
            method: "POST".into(),
            path: path.into(),
            body: None,
        },
        restore: None,
        last_run: None,
        last_status: None,
        last_error: None,
        last_restore: None,
        last_restore_status: None,
        last_restore_error: None,
        created_at: 0,
    }
}

#[test]
fn old_jobs_are_switched_off_with_the_reason() {
    let mut jobs = vec![
        job(1, "/api/esim/switch", true),
        job(2, "/api/device/reboot", true),
        job(3, "/api/system/kill-bloat", false),
        job(4, "/api/at/send", true),
    ];
    let stopped = disable_forbidden(&mut jobs);
    assert_eq!(stopped.iter().map(|s| s.0).collect::<Vec<_>>(), vec![1, 4]);
    assert!(
        !jobs[3].enabled
            && jobs[3]
                .last_error
                .as_deref()
                .unwrap()
                .contains("AT terminal")
    );
    assert!(!jobs[0].enabled && jobs[0].last_error.as_deref().unwrap().starts_with("已停用"));
    assert!(jobs[1].enabled && jobs[1].last_error.is_none());
    assert!(!jobs[2].enabled && jobs[2].last_error.is_some());
    assert!(disable_forbidden(&mut jobs).is_empty());
}

#[test]
fn creating_one_is_refused() {
    let s = Schedule::Once { at: 2_000_000_000 };
    for p in [
        "/api/esim/switch",
        "/api/device/factory-reset",
        "/api/system/kill-bloat",
        "/api/at/send",
    ] {
        let a = Action {
            method: "POST".into(),
            path: p.into(),
            body: None,
        };
        assert!(validate_job(&a, &s, &None).is_err(), "{p}");
    }
    let a = Action {
        method: "POST".into(),
        path: "/api/device/reboot".into(),
        body: None,
    };
    assert!(validate_job(&a, &s, &None).is_ok());
}

fn recurring(id: u32, time: &str, days: &[u8]) -> Job {
    Job {
        schedule: Schedule::Recurring {
            time: time.into(),
            days: days.to_vec(),
        },
        ..job(id, "/api/wifi/radio", true)
    }
}

const NOW: i64 = 2_000_000_000;

#[test]
fn due_recurring_matches_the_minute_and_guards_60_s() {
    let mut j = recurring(1, "07:30", &[0, 1, 2, 3, 4]);
    assert_eq!(
        due(std::slice::from_ref(&j), NOW, 7, 30, 0),
        vec![(1, false)]
    );
    assert!(
        due(std::slice::from_ref(&j), NOW, 7, 31, 0).is_empty(),
        "other minute"
    );
    assert!(
        due(std::slice::from_ref(&j), NOW, 19, 30, 0).is_empty(),
        "other hour"
    );
    // ran 30 s ago (the previous tick, same minute): not again
    j.last_run = Some(NOW - 30);
    assert!(due(std::slice::from_ref(&j), NOW, 7, 30, 0).is_empty());
    // exactly 60 s is still inside the guard; 61 s is not
    j.last_run = Some(NOW - 60);
    assert!(due(std::slice::from_ref(&j), NOW, 7, 30, 0).is_empty());
    j.last_run = Some(NOW - 61);
    assert_eq!(due(&[j], NOW, 7, 30, 0), vec![(1, false)]);
    // a time saved without the leading zero fires too (it used to be compared as text)
    let loose = recurring(2, "7:30", &[0]);
    assert!(validate_time("7:30").is_ok());
    assert_eq!(due(&[loose], NOW, 7, 30, 0), vec![(2, false)]);
    assert!(at_minute("07:05", 7, 5) && at_minute("7:5", 7, 5));
    assert!(!at_minute("7:50", 7, 5) && !at_minute("x", 7, 5) && !at_minute("", 0, 0));
}

#[test]
fn due_recurring_only_on_its_days() {
    let j = recurring(1, "22:00", &[5, 6]); // Saturday, Sunday
    assert!(
        due(std::slice::from_ref(&j), NOW, 22, 0, 0).is_empty(),
        "Monday"
    );
    assert!(
        due(std::slice::from_ref(&j), NOW, 22, 0, 4).is_empty(),
        "Friday"
    );
    assert_eq!(
        due(std::slice::from_ref(&j), NOW, 22, 0, 5),
        vec![(1, false)]
    );
    assert_eq!(due(&[j], NOW, 22, 0, 6), vec![(1, false)]);
}

#[test]
fn due_once_fires_once_from_its_time_on() {
    let mut j = job(1, "/api/wifi/radio", true);
    j.schedule = Schedule::Once { at: NOW };
    assert!(due(std::slice::from_ref(&j), NOW - 1, 0, 0, 0).is_empty());
    assert_eq!(
        due(std::slice::from_ref(&j), NOW, 0, 0, 0),
        vec![(1, false)]
    );
    assert_eq!(
        due(std::slice::from_ref(&j), NOW + 3600, 13, 5, 2),
        vec![(1, false)],
        "late is still due"
    );
    record(&mut j, false, Some(200), None, NOW);
    assert!(!j.enabled, "no restore: done");
    assert_eq!((j.last_run, j.last_status), (Some(NOW), Some(200)));
    j.enabled = true;
    assert!(due(&[j], NOW + 30, 0, 0, 0).is_empty(), "never twice");
}

#[test]
fn due_restore_after_the_action_ran() {
    let mut j = recurring(1, "23:00", &[0, 1, 2, 3, 4, 5, 6]);
    j.restore = Some(Restore {
        time: "07:00".into(),
        body: Some(Json::obj([("ap_2g", Json::from(true))])),
    });
    // never ran: nothing to restore
    assert!(due(std::slice::from_ref(&j), NOW, 7, 0, 1).is_empty());
    record(&mut j, false, Some(200), None, NOW - 8 * 3600);
    assert_eq!(due(std::slice::from_ref(&j), NOW, 7, 0, 1), vec![(1, true)]);
    record(&mut j, true, Some(200), None, NOW);
    assert_eq!(j.last_restore_status, Some(200));
    assert!(j.enabled, "a recurring job stays on");
    // restored already, action not run since: nothing (also past the 60 s guard)
    assert!(due(std::slice::from_ref(&j), NOW + 120, 7, 0, 1).is_empty());
    // same minute for action and restore: both, action first
    let mut both = recurring(2, "12:00", &[3]);
    both.restore = Some(Restore {
        time: "12:00".into(),
        body: None,
    });
    both.last_run = Some(NOW - 86_400);
    assert_eq!(due(&[both], NOW, 12, 0, 3), vec![(2, false), (2, true)]);
    // a once job with a restore stays on until the restore has run
    let mut once = job(3, "/api/wifi/radio", true);
    once.schedule = Schedule::Once { at: NOW };
    once.restore = Some(Restore {
        time: "08:00".into(),
        body: None,
    });
    record(&mut once, false, Some(200), None, NOW);
    assert!(once.enabled);
    assert_eq!(
        due(std::slice::from_ref(&once), NOW + 3600, 8, 0, 0),
        vec![(3, true)]
    );
    record(&mut once, true, Some(500), Some("boom".into()), NOW + 3600);
    assert!(!once.enabled);
    assert_eq!(once.last_restore_error.as_deref(), Some("boom"));
}

#[test]
fn due_skips_disabled_jobs() {
    let mut j = recurring(1, "07:30", &[0]);
    j.enabled = false;
    j.restore = Some(Restore {
        time: "07:30".into(),
        body: None,
    });
    j.last_run = Some(NOW - 86_400);
    assert!(due(&[j], NOW, 7, 30, 0).is_empty());
    // job order is kept
    let jobs = [recurring(5, "07:30", &[0]), recurring(2, "07:30", &[0])];
    assert_eq!(due(&jobs, NOW, 7, 30, 0), vec![(5, false), (2, false)]);
}

#[cfg(feature = "serde")]
#[test]
fn scheduler_json_round_trips() {
    let text = r#"{"id":1,"name":"n","enabled":true,"schedule":{"type":"recurring","time":"07:30","days":[0,6]},"action":{"method":"PUT","path":"/api/wifi/radio","body":{"ap_2g":false}},"restore":{"time":"08:00","body":{"ap_2g":true}},"last_run":null,"last_status":null,"last_restore":null,"last_restore_status":null,"created_at":5}"#;
    let j: Job = serde_json::from_str(text).unwrap();
    assert_eq!(serde_json::to_string(&j).unwrap(), text);
    // jobs saved before last_restore_status existed still load
    let old: Job =
        serde_json::from_str(&text.replace(",\"last_restore_status\":null", "")).unwrap();
    assert_eq!(old, j);
}
