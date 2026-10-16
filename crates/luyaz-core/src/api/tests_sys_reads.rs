//! Tests for sys_reads.rs (8d-1b).

use super::sys_reads::*;
use super::*;
use std::fs;

fn scratch(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("u60d-sysreads-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn compat(
    root: &std::path::Path,
    ubus: fn(&'static str, &'static str, Value) -> Result<Value, String>,
) -> Compat {
    let src = Sources {
        ubus: std::sync::Arc::new(move |o, m, a| Box::pin(async move { ubus(o, m, a) })),
        ..Sources::none()
    };
    Compat::new(root.to_str().unwrap(), src)
}

#[test]
fn cpu_delta_and_sysfs() {
    let root = scratch("cpu");
    let sys = root.join("sys/devices/system/cpu");
    for (id, online, cur, max) in [
        (0, None, "1200000", "1800000"),
        (1, Some("0"), "300000", "1800000"),
    ] {
        let d = sys.join(format!("cpu{id}"));
        fs::create_dir_all(d.join("cpufreq")).unwrap();
        if let Some(o) = online {
            fs::write(d.join("online"), o).unwrap();
        }
        fs::write(d.join("cpufreq/scaling_cur_freq"), cur).unwrap();
        fs::write(d.join("cpufreq/cpuinfo_max_freq"), max).unwrap();
    }
    let a = parse_cpu_samples(
        "cpu  9 9 9 9 9 9 9\ncpu0 100 0 100 800 0 0 0\ncpu1 10 0 10 80 0 0 0\nintr 1\n",
    );
    assert_eq!(a.len(), 2);
    let b = parse_cpu_samples("cpu0 150 0 150 900 0 0 0\ncpu1 10 0 10 80 0 0 0\n");
    let mut prev = Vec::new();
    let first = cpu_usage(&mut prev, a, &sys);
    assert_eq!(first["cores"], json!([0.0, 0.0]));
    let v = cpu_usage(&mut prev, b, &sys);
    assert_eq!(v["cores"], json!([50.0, 0.0]));
    assert_eq!(v["overall"], 50.0);
    assert_eq!(
        v["per_core"][0],
        json!({"id":0,"online":true,"usage":50.0,"freq_mhz":1200,"max_mhz":1800})
    );
    assert_eq!(
        v["per_core"][1],
        json!({"id":1,"online":false,"usage":null,"freq_mhz":null,"max_mhz":1800})
    );
}

#[tokio::test]
async fn cpu_route_reads_under_root() {
    let root = scratch("cpuroute");
    fs::create_dir_all(root.join("proc")).unwrap();
    fs::write(root.join("proc/stat"), "cpu0 1 0 1 8 0 0 0\n").unwrap();
    let c = compat(&root, |_, _, _| Err("no".into()));
    let resp = cpu(State(c)).await;
    assert_eq!(resp.status(), 200);
}

#[test]
fn device_shape() {
    let v = device_info(
        "u60\n",
        "1234.56 99.0\n",
        "0.10 0.20 0.30 1/100 5\n",
        "Linux version 5.4\n",
    );
    assert_eq!(
        v,
        json!({"hostname":"u60","uptime_secs":1234,"load_avg":[0.1,0.2,0.3],"kernel":"Linux version 5.4"})
    );
    let e = device_info("", "", "", "");
    assert_eq!(e["load_avg"], json!([0.0, 0.0, 0.0]));
    assert_eq!(e["uptime_secs"], 0);
}

#[test]
fn method_listing() {
    let l = "'zwrt_bsp.usb' @1234abcd\n\t\"list\":{}\n\t\"set\":{\"mode\":\"String\"}\n";
    assert_eq!(lists_method(l, "zwrt_bsp.usb", "set"), Some(true));
    assert_eq!(
        lists_method("'zwrt_bsp.usb' @1\n\t\"list\":{}\n", "zwrt_bsp.usb", "set"),
        Some(false)
    );
    assert_eq!(
        lists_method("Command failed: Not found", "zwrt_bsp.usb", "set"),
        None
    );
    assert_eq!(
        usb_data(json!({"a":1}), Some(false)),
        json!({"a":1,"mode_settable":false})
    );
    assert_eq!(usb_data(json!({"a":1}), None), json!({"a":1}));
}

#[test]
fn wan_ipv6_shape() {
    let on = wan_ipv6_data(
        &json!({"pdpType":3}),
        Some(&json!({"ipv6_address":"2001::1"})),
    );
    assert_eq!(
        on,
        json!({"ipv6_enabled":true,"pdp_type":3,"wan_has_ipv6":true})
    );
    let off = wan_ipv6_data(&json!({"pdpType":1}), Some(&json!({"ipv6_address":""})));
    assert_eq!(
        off,
        json!({"ipv6_enabled":false,"pdp_type":1,"wan_has_ipv6":false})
    );
    assert_eq!(wan_ipv6_data(&json!({}), None)["pdp_type"], 0);
}

#[tokio::test]
async fn battery_shape() {
    let root = scratch("battery");
    let ps = root.join("sys/class/power_supply");
    let c = compat(&root, |_, _, _| Err("no".into()));
    // no battery status: the agent's 503
    assert_eq!(
        battery_reply(&c).await,
        (
            503,
            json!({"ok": false, "error": "battery info not available"})
        )
    );
    fs::create_dir_all(ps.join("battery")).unwrap();
    for (f, v) in [
        ("status", "Discharging\n"),
        ("capacity", "57"),
        ("current_now", "-800000"),
        ("temp", "312"),
        ("charge_full", "11000000"),
        ("health", "Good"),
    ] {
        fs::write(ps.join("battery").join(f), v).unwrap();
    }
    let (st, v) = battery_reply(&c).await;
    assert_eq!(st, 200);
    assert_eq!(v["ok"], true);
    let mut d = v["data"].clone();
    assert!(d.as_object_mut().unwrap().remove("estimate").is_some());
    assert_eq!(
        d,
        json!({
            "status": "Discharging", "capacity": 57, "voltage_uv": 0, "current_ua": -800000,
            "temperature": 312, "charge_full_uah": 11000000, "charge_full_design_uah": null,
            "charge_counter_uah": null, "cycle_count": null, "health": "Good",
            "voltage_max_uv": null, "charger": null,
        })
    );
    fs::create_dir_all(ps.join("usb")).unwrap();
    fs::write(ps.join("usb/online"), "1").unwrap();
    fs::write(ps.join("usb/voltage_now"), "5000000").unwrap();
    assert_eq!(
        battery_at(&ps).unwrap()["charger"],
        json!({"online": true, "voltage_uv": 5000000, "current_ua": null,
               "input_current_limit_ua": null})
    );
}
