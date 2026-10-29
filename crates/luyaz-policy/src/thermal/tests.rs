use super::*;

const S: Duration = Duration::from_secs(1);

fn r(batt: i32, chip: i32, plugged: bool) -> Reading {
    Reading {
        batt: Some(batt),
        chip: Some(chip),
        plugged: Some(plugged),
    }
}

/// Feeds `reading` every 30 s from `*t`, `n` times; returns the last level.
fn feed(k: &mut Tracker, t: &mut Duration, reading: Reading, n: u32) -> Level {
    let l = Limits::default();
    let mut lv = k.level;
    for _ in 0..n {
        lv = k.step(*t, &reading, &l);
        *t += 30 * S;
    }
    lv
}

#[test]
fn reached_levels_at_the_lines() {
    let l = Limits::default();
    assert_eq!(reached(&r(43, 60, true), &l), Level::Normal);
    assert_eq!(reached(&r(44, 60, true), &l), Level::Warm);
    assert_eq!(
        reached(&r(49, 60, false), &l),
        Level::Normal,
        "warm needs a charger"
    );
    assert_eq!(reached(&r(50, 60, false), &l), Level::Hot);
    assert_eq!(reached(&r(30, 80, false), &l), Level::Hot);
    assert_eq!(reached(&r(55, 60, false), &l), Level::Critical);
    assert_eq!(reached(&r(30, 90, false), &l), Level::Critical);
    let none = Reading::default();
    assert_eq!(reached(&none, &l), Level::Normal);
}

#[test]
fn one_hot_reading_is_not_enough() {
    let (mut k, mut t) = (Tracker::default(), Duration::ZERO);
    assert_eq!(feed(&mut k, &mut t, r(56, 60, true), 1), Level::Normal);
    assert_eq!(feed(&mut k, &mut t, r(35, 50, true), 1), Level::Normal);
    assert_eq!(feed(&mut k, &mut t, r(56, 60, true), 1), Level::Normal);
    assert_eq!(feed(&mut k, &mut t, r(56, 60, true), 1), Level::Critical);
}

#[test]
fn a_rising_pair_enters_the_lower_of_the_two() {
    let (mut k, mut t) = (Tracker::default(), Duration::ZERO);
    feed(&mut k, &mut t, r(56, 60, true), 1);
    assert_eq!(feed(&mut k, &mut t, r(45, 60, true), 1), Level::Warm);
    assert_eq!(feed(&mut k, &mut t, r(56, 60, true), 2), Level::Critical);
}

#[test]
fn cools_down_only_after_the_hold() {
    let (mut k, mut t) = (Tracker::default(), Duration::ZERO);
    feed(&mut k, &mut t, r(51, 60, false), 2);
    assert_eq!(k.level, Level::Hot);
    // 47 °C is under the entry but over the exit line: stays hot
    assert_eq!(feed(&mut k, &mut t, r(47, 60, false), 20), Level::Hot);
    // under the exit: 10 readings = 270 s from the first, still hot; the 11th is at 300 s
    assert_eq!(feed(&mut k, &mut t, r(45, 60, false), 10), Level::Hot);
    assert_eq!(feed(&mut k, &mut t, r(45, 60, false), 1), Level::Normal);
}

#[test]
fn a_warm_reading_in_the_hold_restarts_it() {
    let (mut k, mut t) = (Tracker::default(), Duration::ZERO);
    feed(&mut k, &mut t, r(51, 60, false), 2);
    feed(&mut k, &mut t, r(45, 60, false), 9);
    feed(&mut k, &mut t, r(47, 60, false), 1);
    assert_eq!(feed(&mut k, &mut t, r(45, 60, false), 10), Level::Hot);
    assert_eq!(feed(&mut k, &mut t, r(45, 60, false), 1), Level::Normal);
}

#[test]
fn hot_cools_to_warm_while_charging_and_warm() {
    let (mut k, mut t) = (Tracker::default(), Duration::ZERO);
    feed(&mut k, &mut t, r(30, 82, true), 2);
    assert_eq!(k.level, Level::Hot);
    // chip cooled, battery 45 and charging: drops to warm, not normal
    assert_eq!(feed(&mut k, &mut t, r(45, 60, true), 11), Level::Warm);
}

#[test]
fn warm_ends_at_once_on_unplugging() {
    let (mut k, mut t) = (Tracker::default(), Duration::ZERO);
    feed(&mut k, &mut t, r(45, 60, true), 2);
    assert_eq!(k.level, Level::Warm);
    assert_eq!(feed(&mut k, &mut t, r(45, 60, false), 1), Level::Normal);
}

#[test]
fn unknown_readings_neither_enter_nor_cool() {
    let (mut k, mut t) = (Tracker::default(), Duration::ZERO);
    let none = Reading::default();
    assert_eq!(feed(&mut k, &mut t, none, 5), Level::Normal);
    feed(&mut k, &mut t, r(56, 60, false), 2);
    assert_eq!(feed(&mut k, &mut t, none, 40), Level::Critical);
    let half = Reading {
        batt: Some(40),
        chip: None,
        plugged: Some(false),
    };
    assert_eq!(
        feed(&mut k, &mut t, half, 40),
        Level::Critical,
        "chip unknown is not cool"
    );
}

#[test]
fn actions_per_level() {
    let l = Limits::default();
    assert_eq!(
        actions(Level::Normal, Some(80), false, &l),
        Actions::default()
    );
    let w = actions(Level::Warm, Some(80), false, &l);
    assert!(w.pause_charge && w.chill == Chill::Leave && !w.incident);
    let h = actions(Level::Hot, Some(80), false, &l);
    assert!(h.pause_charge && h.chill == Chill::Eco && !h.incident);
    let c = actions(Level::Critical, Some(80), false, &l);
    assert!(c.pause_charge && c.chill == Chill::Direct && c.incident);
}

#[test]
fn manual_resume_and_low_battery_beat_warm_and_hot_not_critical() {
    let l = Limits::default();
    for lv in [Level::Warm, Level::Hot] {
        assert!(!actions(lv, Some(80), true, &l).pause_charge);
        assert!(!actions(lv, Some(30), false, &l).pause_charge);
        assert!(actions(lv, Some(31), false, &l).pause_charge);
        assert!(actions(lv, None, false, &l).pause_charge);
    }
    assert!(actions(Level::Critical, Some(10), true, &l).pause_charge);
    assert_eq!(actions(Level::Hot, Some(10), true, &l).chill, Chill::Eco);
}
