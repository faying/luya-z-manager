use super::*;
use std::path::PathBuf;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("heat-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn zone(root: &std::path::Path, n: usize, kind: &str, mc: &str) {
    let d = root.join(format!("thermal_zone{n}"));
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("type"), format!("{kind}\n")).unwrap();
    std::fs::write(d.join("temp"), format!("{mc}\n")).unwrap();
}

#[test]
fn zones_take_the_battery_and_the_hottest_chip_and_skip_bad_values() {
    let dir = tmp("zones");
    let r = dir.as_path();
    zone(r, 0, "battery", "28400");
    zone(r, 1, "cpuss-0", "41200");
    zone(r, 2, "cpuss-1", "43600");
    // modem side: never read, however hot
    zone(r, 3, "mdmss-0", "99000");
    zone(r, 4, "pmx75_tz", "39000");
    // not chip zones, however hot
    zone(r, 5, "sys-therm-1", "99000");
    zone(r, 6, "pmg1110_tz", "99000");
    // a sensor reading -273 °C, a dead one reading 0
    zone(r, 7, "cpuss-4", "-273000");
    zone(r, 8, "cpuss-2", "0");
    zone(r, 9, "cpuss-3", "junk");
    assert_eq!(read_zones(r), (Some(28), Some(44)));
}

#[test]
fn no_zones_is_no_reading() {
    let dir = tmp("empty");
    assert_eq!(read_zones(&dir), (None, None));
    assert_eq!(read_zones(&dir.join("missing")), (None, None));
}

/// The notice goes through the real queue (ASCII only) and still reads in Chinese on the web:
/// core 1.98.4 wrote Chinese here and the queue kept ` 45 C 54 C`.
#[test]
fn the_notice_survives_the_queue_and_reads_in_chinese() {
    let dir = tmp("notice-queue");
    let a = luyaz_api::alerts::Alerts::at(&dir);
    let r = Reading {
        batt: Some(45),
        chip: None,
        plugged: Some(true),
    };
    let plans = [(true, true), (true, false), (false, true), (false, false)];
    for (i, p) in plans.iter().enumerate() {
        let id = format!("34d373b7-{}", 9_899_371 + i);
        a.add_once(&id, NOTICE_KIND, &notice_text(&r, *p), 1_794_000_000, 1)
            .unwrap();
    }
    let got: Vec<String> = a
        .events()
        .iter()
        .map(|ev| luyaz_api::alerts::detail_zh(&ev.kind, &ev.text).unwrap())
        .collect();
    let head = "电池 45 °C、芯片 ? °C";
    assert_eq!(
        got,
        [
            format!("{head}：暂停充电、CHILL 降到省电档，凉下来自动恢复"),
            format!("{head}：暂停充电，凉下来自动恢复"),
            format!("{head}：CHILL 降到省电档，凉下来自动恢复"),
            format!("{head}：放到通风的地方"),
        ]
    );
}
