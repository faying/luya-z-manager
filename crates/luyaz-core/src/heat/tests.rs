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
