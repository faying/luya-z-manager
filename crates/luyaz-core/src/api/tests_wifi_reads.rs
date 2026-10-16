//! Tests for wifi_reads.rs (8d-1b).

use super::wifi_reads::*;
use serde_json::json;

const INFO: &str = "Interface wlan2\n\tifindex 7\n\ttype AP\n\tssid Home\n\tchannel 149 (5745 MHz), width: 80 MHz, center1: 5775 MHz\n\ttxpower 23.00 dBm\n";

#[test]
fn info_gives_channel_and_width() {
    assert_eq!(parse_info(INFO), ("149".into(), "80 MHz".into()));
    assert_eq!(
        parse_info("command failed: No such device (-19)"),
        (String::new(), String::new())
    );
}

#[test]
fn stations_are_counted_by_line() {
    let dump = "Station aa:bb:cc:dd:ee:01 (on wlan0)\n\tinactive time: 10 ms\nStation aa:bb:cc:dd:ee:02 (on wlan0)\n\trx bytes: 1\n";
    assert_eq!(count_stations(dump), 2);
    assert_eq!(count_stations(""), 0);
}

#[test]
fn status_has_exactly_the_six_fields_and_no_secret() {
    let v = status_data([("6".into(), "20 MHz".into()), parse_info(INFO)], [1, 0]);
    let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "actual_bw_2g",
            "actual_bw_5g",
            "actual_channel_2g",
            "actual_channel_5g",
            "clients_2g",
            "clients_5g"
        ]
    );
    assert!(
        keys.iter()
            .all(|k| !k.contains("key") && !k.contains("pass") && !k.contains("psk"))
    );
    assert_eq!(v["actual_channel_5g"], "149");
    assert_eq!(v["clients_2g"], 1);
    assert_eq!(v["clients_5g"], json!(0));
}

#[test]
fn power_save_reads_iw_and_the_saved_script() {
    assert_eq!(psm_from_iw("wlan0\tPower save: on\n"), Some(true));
    assert_eq!(psm_from_iw("Power save: off"), Some(false));
    assert_eq!(psm_from_iw("command failed"), None);
    let script = "#!/bin/sh\n[ \"$ACTION\" = ifup ] && {\n  iw dev wlan0 set power_save off 2>/dev/null\n}\n";
    assert_eq!(psm_from_script(script), Some(false));
    assert_eq!(psm_from_script(&script.replace("off", "on")), Some(true));
    assert_eq!(psm_from_script(""), None);
    assert_eq!(
        psm_data(None, Some(true)),
        json!({"enabled": true, "live": null, "saved": true})
    );
    assert_eq!(psm_data(Some(false), Some(true))["enabled"], false);
}
