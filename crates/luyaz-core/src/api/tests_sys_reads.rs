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
fn usb_compatibility_is_a_projection_not_a_second_resolver() {
    use luyaz_api::firmware::{Support, UsbWriteDisposition};
    let c = crate::device_profile::collect::Collector::new();
    let mut report = c.report(None);
    let data = usb_data(json!({"mode":"user", "mode_settable":true}), Some(&report));
    assert!(
        data.get("mode_settable").is_none(),
        "unknown cannot inherit a vendor bool"
    );
    assert_eq!(data["write_policies"].as_object().unwrap().len(), 3);
    for id in ["usb.mode.set", "usb.attach.share", "usb.attach.fast_charge"] {
        assert_eq!(
            data["write_policies"][id],
            json!(report.usb_write_policy(id).unwrap())
        );
    }
    report
        .assessments
        .iter_mut()
        .find(|a| a.capability_id == "usb.mode.set")
        .unwrap()
        .status = Support::Unsupported;
    let data = usb_data(json!({}), Some(&report));
    assert_eq!(data["mode_settable"], false);
    assert_eq!(
        report.usb_write_policy("usb.mode.set").unwrap().disposition,
        UsbWriteDisposition::Denied
    );
    assert_eq!(
        usb_data(json!({"a":1}), None),
        json!({"a":1,"write_policies":{}})
    );
}
#[tokio::test]
async fn usb_read_failure_is_shared_without_querying_a_method_listing() {
    let hub = std::sync::Arc::new(crate::block::Hub::new(
        vec![],
        Box::new(crate::block::NoSink),
    ));
    let src = Sources {
        hub: Some(hub.clone()),
        ubus: std::sync::Arc::new(|_, _, _| Box::pin(async { Err("temporary failure".into()) })),
        ..Sources::none()
    };
    let c = Compat::new("/nonexistent-firmware-test", src);
    let response = usb_status(State(c)).await;
    assert_eq!(response.status(), 503);
    let report = hub.firmware().report(None);
    assert_eq!(report.evidence.len(), 1);
    let e = &report.evidence[0];
    assert_eq!(e.capability_id, "usb.status.read");
    assert_eq!(
        e.result.as_ref().unwrap().outcome,
        luyaz_api::firmware::ProbeOutcome::Unavailable
    );
    assert!(e.result.as_ref().unwrap().proof.is_none());
    report.validate().unwrap();
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

#[tokio::test]
async fn usb_get_projects_shared_report_without_method_discovery() {
    let hub = std::sync::Arc::new(crate::block::Hub::new(
        vec![],
        Box::new(crate::block::NoSink),
    ));
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = calls.clone();
    let src = Sources {
        hub: Some(hub.clone()),
        ubus: std::sync::Arc::new(move |object, method, _| {
            assert_eq!((object, method), ("zwrt_bsp.usb", "list"));
            count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Box::pin(async { Ok(json!({"mode":"user","mode_settable":true})) })
        }),
        ..Sources::none()
    };
    let c = Compat::new("/nonexistent-firmware-test", src);
    let response = usb_status(State(c)).await;
    assert_eq!(response.status(), 200);
    let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert!(body["data"].get("mode_settable").is_none());
    let r = hub
        .firmware()
        .report(luyaz_api::clock::BootTime::read().as_ref());
    assert_eq!(
        body["data"]["write_policies"]["usb.mode.set"],
        json!(r.usb_write_policy("usb.mode.set").unwrap())
    );
    assert!(
        r.evidence
            .iter()
            .all(|e| e.capability_id != "usb.mode.setter.surface")
    );
}
