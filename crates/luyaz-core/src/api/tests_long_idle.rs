use super::hot_drain::{FRESH_S, rows};
use super::long_idle::*;
use luyaz_api::execution::long_idle::{Advice, replay};
use serde_json::json;

/// One closed hour of boot `b` starting at `n` h: screen off at home, `cli_s` with clients,
/// `pkts` packets, `pm` quiet per mille, `on_s` screen on, `gap` down seconds.
fn hour(b: &str, n: i64, cli_s: i64, pkts: i64, pm: i64, on_s: i64, gap: i64) -> Vec<String> {
    let (from, to) = (n * 3600, n * 3600 + 3600);
    let pw = json!({"k": "hour_power", "boot": b, "from": from, "to": to,
        "on_home_s": on_s, "on_abroad_s": 0, "off_home_s": 3600 - on_s, "off_abroad_s": 0,
        "chg_s": 0, "off_home_cli_s": cli_s, "off_home_wan_pkts": pkts, "off_home_quiet_pm": pm,
        "batt_tmax": 300});
    let h = json!({"k": "hour", "boot": b, "from": from, "to": to, "awake": 3600, "asleep": 0,
        "chg_s": 3600, "g_core": gap, "g_link": 0});
    vec![h.to_string(), pw.to_string()]
}

fn quiet(b: &str, n: i64) -> Vec<String> {
    hour(b, n, 0, 3000, 500, 0, 0)
}

fn all_known() -> Now {
    Now {
        txn_running: Some(false),
        sms_known: Some(true),
        watchdog_ok: Some(true),
        chill_on: Some(true),
        idle_pause_on: Some(false),
    }
}

const B: &str = "0c178ee6";
const NOW: i64 = 4 * 3600 + 300;

fn run(lines: Vec<Vec<String>>, now: &Now) -> (Advice, u32) {
    let l: Vec<String> = lines.into_iter().flatten().collect();
    let e = evidence(&rows(l.iter()), B, NOW, now);
    (replay(&e), e.quiet_run)
}

#[test]
fn quiet_night_suggests_the_pause_and_counts_the_run() {
    let (a, n) = run(
        vec![
            hour(B, 0, 900, 200_000, 100, 600, 0),
            quiet(B, 1),
            quiet(B, 2),
            quiet(B, 3),
        ],
        &all_known(),
    );
    assert_eq!(n, 3);
    assert_eq!(
        a,
        Advice::Suggest {
            hours: 3,
            step: "chill_idle_pause"
        }
    );
}

#[test]
fn the_pause_already_on_is_nothing_to_say() {
    let now = Now {
        idle_pause_on: Some(true),
        ..all_known()
    };
    let (a, _) = run(vec![quiet(B, 2), quiet(B, 3)], &now);
    assert_eq!(
        a,
        Advice::None {
            why: "already_saving"
        }
    );
}

#[test]
fn clients_traffic_screen_and_coverage() {
    for (h, want) in [
        (
            hour(B, 3, 60, 3000, 500, 0, 0),
            Advice::None { why: "clients" },
        ),
        (
            hour(B, 3, 0, 20_000, 500, 0, 0),
            Advice::None { why: "traffic" },
        ),
        (
            hour(B, 3, 0, 3000, 200, 0, 0),
            Advice::None { why: "traffic" },
        ),
        (
            hour(B, 3, 0, 3000, 500, 30, 0),
            Advice::None { why: "in_use" },
        ),
        (
            hour(B, 3, 0, 3000, 500, 0, 120),
            Advice::Unknown {
                why: "coverage_gap",
            },
        ),
    ] {
        let (a, n) = run(vec![quiet(B, 2), h], &all_known());
        assert_eq!(a, want);
        assert_eq!(n, 0);
    }
}

#[test]
fn a_restart_between_hours_is_a_clock_gap() {
    // the hour before ends at 2 h, this one runs 2 h 10 min to 3 h 10 min
    let mut late = quiet(B, 3);
    for l in &mut late {
        *l = l
            .replace("\"from\":10800", "\"from\":7800")
            .replace("\"to\":14400", "\"to\":11400");
    }
    let (a, n) = run(vec![quiet(B, 1), late], &all_known());
    assert_eq!(a, Advice::Unknown { why: "clock_gap" });
    assert_eq!(n, 1);
}

#[test]
fn roaming_hours_have_no_client_count() {
    let mut r = quiet(B, 3);
    r[1] = r[1].replace("\"off_abroad_s\":0", "\"off_abroad_s\":600");
    let (a, _) = run(vec![quiet(B, 2), r], &all_known());
    assert_eq!(
        a,
        Advice::Unknown {
            why: "clients_unknown"
        }
    );
}

#[test]
fn the_battery_blocks_charging_flag_is_not_read() {
    // every `hour` line here says chg_s 3600: the fuel gauge's 0 wins
    let (a, _) = run(vec![quiet(B, 2), quiet(B, 3)], &all_known());
    assert!(matches!(a, Advice::Suggest { .. }));
}

#[test]
fn stale_or_another_boot_or_missing_now_is_unknown() {
    let l: Vec<String> = [quiet(B, 2), quiet(B, 3)].concat();
    let r = rows(l.iter());
    let e = evidence(&r, "ffffffff", NOW, &all_known());
    assert_eq!(
        replay(&e),
        Advice::Unknown {
            why: "window_stale"
        }
    );
    let e = evidence(&r, B, 4 * 3600 + FRESH_S + 1, &all_known());
    assert_eq!(
        replay(&e),
        Advice::Unknown {
            why: "window_stale"
        }
    );
    let e = evidence(&r, B, NOW, &Now::default());
    assert_eq!(replay(&e), Advice::Unknown { why: "txn_unknown" });
    let e = evidence(&[], B, NOW, &all_known());
    assert_eq!(
        replay(&e),
        Advice::Unknown {
            why: "window_stale"
        }
    );
}
