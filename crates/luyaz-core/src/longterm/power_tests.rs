use super::*;

fn rd_(v: i64, i: i64, q: Option<i64>) -> Read {
    Read {
        v: Some(v),
        i: Some(i),
        q,
        ..Default::default()
    }
}

#[test]
fn energy_by_counter_across_a_sleep_and_by_vi_awake_only() {
    let mut a = Acc::default();
    // counter: 1000 µAh down at 4 V = 4000 µWh, over 600 s of which 300 asleep
    let mut r = rd_(4_000_000, -100_000, Some(9_000));
    r.screen_on = false;
    a.add(&r, Some(10_000), 600, 300, false, 600);
    let p = a.power();
    assert_eq!(p["src"], "counter");
    assert_eq!(p["e"], 4.0);
    assert_eq!(p["off_home_s"], 600, "the counter covers the sleep");
    assert_eq!(p["off_home_e"], 4.0);
    assert_eq!(p["e_partial"], 0);
    // no counter: V×I over the awake part, partial because of the sleep
    let mut b = Acc::default();
    let mut r = rd_(4_000_000, -360_000, None);
    r.screen_on = true;
    b.add(&r, None, 100, 90, true, 100);
    let p = b.power();
    assert_eq!(p["src"], "vi");
    // 4 V × 0.36 A × 90 s = 129.6 J = 36 mWh
    assert_eq!(p["on_abroad_e"], 36.0);
    assert_eq!(p["on_abroad_s"], 90);
    assert_eq!(p["e_partial"], 1);
    // charging: into chg_s, not a cell; nothing readable: partial, cell by awake
    let mut c = Acc::default();
    let mut r = rd_(4_000_000, 500_000, None);
    r.charging = true;
    c.add(&r, None, 60, 60, false, 60);
    c.add(&Read::default(), None, 60, 60, false, 120);
    let p = c.power();
    assert_eq!(p["chg_s"], 60);
    assert_eq!(
        p["off_home_s"], 60,
        "screen off (the default), by awake seconds"
    );
    assert_eq!(p["e"], Value::Null);
    assert_eq!(p["off_home_e"], Value::Null);
    assert_eq!(p["e_partial"], 1);
}

#[test]
fn temps_throttle_exits_and_cpu() {
    let mut a = Acc::default();
    let mut r = Read {
        bt: Some(310),
        zt: Some(45_000),
        thr: Some(false),
        ts_on: true,
        ts_ok: Some(true),
        chill_on: true,
        chill_ok: false,
        cpu: Some((100, 5, 1_000)),
        cap: Some(80),
        ..Default::default()
    };
    a.add(&r, None, 60, 60, false, 60);
    r.thr = Some(true);
    r.zt = Some(50_000);
    r.ts_ok = None; // not checked this time
    r.cpu = Some((100, 5, 1_360));
    r.cap = Some(78);
    a.add(&r, None, 60, 60, false, 3660);
    let p = a.power();
    assert_eq!(p["zone_tmax"], 50_000);
    assert_eq!(p["batt_tmax"], 310);
    assert_eq!(p["throttle_s"], 60);
    assert_eq!(
        (p["batt_from"].clone(), p["batt_to"].clone()),
        (json!(80), json!(78))
    );
    let q = a.proc_();
    assert_eq!(q["ts_on_s"], 120);
    assert_eq!(q["ts_chk_s"], 60);
    assert_eq!(q["ts_ok_s"], 60);
    assert_eq!(q["chill_on_s"], 120);
    assert_eq!(q["chill_ok_s"], 0);
    // 360 ticks in 3600 s = 1 per mille
    assert_eq!(q["cpu_datad"], 1);
    // another process by the end: unknown
    let mut b = Acc::default();
    b.note(
        &Read {
            cpu: Some((100, 5, 0)),
            ..Default::default()
        },
        0,
    );
    b.note(
        &Read {
            cpu: Some((101, 9, 50)),
            ..Default::default()
        },
        3600,
    );
    assert_eq!(b.proc_()["cpu_datad"], Value::Null);
    assert_eq!(
        Acc::default().power()["throttle_s"],
        Value::Null,
        "no cpufreq"
    );
}

#[test]
fn zone_types_and_ts_answer() {
    assert!(
        app_zone("cpuss-0") && app_zone("pm7550_tz") && app_zone("battery") && app_zone("xo-therm")
    );
    assert!(!app_zone("sdr0") && !app_zone("mdmss-0") && !app_zone("pm_x"));
    assert_eq!(
        ts_healthy(&json!({"BackendState": "Running", "Self": {"Online": true}})),
        Some(true)
    );
    assert_eq!(
        ts_healthy(&json!({"BackendState": "Running", "Self": {"Online": false}})),
        Some(false)
    );
    assert_eq!(ts_healthy(&json!({"BackendState": "Stopped"})), Some(false));
    assert_eq!(ts_healthy(&json!({})), None);
}

/// A device under a temp root.
fn root(tag: &str) -> (PathBuf, Paths) {
    let r = std::env::temp_dir().join(format!("u60d-pw-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&r);
    let w = |rel: &str, t: &str| {
        let f = r.join(rel);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, t).unwrap();
    };
    w("bat/voltage_now", "4000000\n");
    w("bat/current_now", "-250000\n");
    w("bat/charge_counter", "1500000\n");
    w("bat/capacity", "77\n");
    w("bat/temp", "305\n");
    w("bat/status", "Discharging\n");
    w("backlight", "0\n");
    w("thermal/thermal_zone0/type", "cpuss-0\n");
    w("thermal/thermal_zone0/temp", "41000\n");
    w("thermal/thermal_zone1/type", "sdr0-pa\n");
    w("thermal/thermal_zone1/temp", "99000\n");
    w("cpufreq/policy0/scaling_max_freq", "1800000\n");
    w(
        "route",
        "Iface\tDestination\tGateway\tFlags\nrmnet_data0\t00000000\t00000000\t0001\n",
    );
    w("rc.local", "sh /data/tailscale/start.sh &\nexit 0\n");
    w(
        "proc/100/stat",
        &format!(
            "100 (zwrt-datad) S{} 300 0 0 0 0 0 0 0 5 0 0\n",
            " 0".repeat(10)
        ),
    );
    w("proc/100/status", "Name:\tzwrt-datad\nVmRSS:\t   3456 kB\n");
    w("dumps/a.bin", &"x".repeat(10));
    std::fs::create_dir_all(r.join("data")).unwrap();
    let p = Paths {
        bat: r.join("bat"),
        backlight: r.join("backlight"),
        thermal: r.join("thermal"),
        cpufreq: r.join("cpufreq"),
        route: r.join("route"),
        rc_local: r.join("rc.local"),
        ts_sock: r.join("ts.sock"),
        chill_dir: r.join("chill"),
        chill_state: r.join("chill.state"),
        thermal_eco: r.join("thermal-eco"),
        chill_idle: r.join("chill.idle"),
        proc_: r.join("proc"),
        data: r.join("data"),
        dumps: r.join("dumps"),
        self_pid: 100,
    };
    (r, p)
}

#[test]
fn reads_the_device_files_as_the_guard() {
    let (r, p) = root("read");
    let mut st = ReadState::default();
    let x = st.read(&p, &BTreeMap::new(), 1_791_500_000);
    assert_eq!(
        (x.v, x.i, x.q, x.cap, x.bt),
        (
            Some(4_000_000),
            Some(-250_000),
            Some(1_500_000),
            Some(77),
            Some(305)
        )
    );
    assert!(!x.charging && !x.screen_on);
    assert_eq!(x.zt, Some(41_000), "the modem-side zone is not read");
    assert_eq!(x.thr, Some(false));
    assert!(
        x.ts_on,
        "default route by the cellular link and rc.local starts it"
    );
    assert_eq!(x.ts_ok, Some(false), "no socket: down");
    assert_eq!(x.rss.get("datad"), Some(&3456));
    assert!(x.free_mb.is_some());
    assert_eq!(x.dump_mb, Some(0));
    // the cap lowered: throttled; raised again: the new highest, not throttled
    std::fs::write(r.join("cpufreq/policy0/scaling_max_freq"), "1200000\n").unwrap();
    assert_eq!(st.read(&p, &BTreeMap::new(), 0).thr, Some(true));
    std::fs::write(r.join("cpufreq/policy0/scaling_max_freq"), "2000000\n").unwrap();
    assert_eq!(st.read(&p, &BTreeMap::new(), 0).thr, Some(false));
    // charging by a positive current
    std::fs::write(r.join("bat/current_now"), "300000\n").unwrap();
    assert!(st.read(&p, &BTreeMap::new(), 0).charging);
    let _ = std::fs::remove_dir_all(&r);
}

/// The LocalAPI check over a real unix socket.
#[test]
fn ts_check_over_a_real_socket() {
    use std::io::{Read as _, Write as _};
    let (r, p) = root("ts");
    let l = std::os::unix::net::UnixListener::bind(&p.ts_sock).unwrap();
    let srv = std::thread::spawn(move || {
        for body in [
            r#"{"BackendState":"Running","Self":{"Online":true}}"#,
            r#"{"BackendState":"NeedsLogin","Self":{"Online":false}}"#,
        ] {
            let (mut c, _) = l.accept().unwrap();
            let mut req = [0u8; 512];
            let n = c.read(&mut req).unwrap();
            assert!(
                String::from_utf8_lossy(&req[..n])
                    .starts_with("GET /localapi/v0/status HTTP/1.0\r\nHost: local-tailscaled.sock")
            );
            let _ = c.write_all(
                format!("HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n{body}")
                    .as_bytes(),
            );
        }
        let (mut c, _) = l.accept().unwrap();
        let _ = c.write_all(b"HTTP/1.0 403 Forbidden\r\n\r\n");
    });
    assert_eq!(ts_check(&p.ts_sock), Some(true));
    assert_eq!(ts_check(&p.ts_sock), Some(false));
    assert_eq!(
        ts_check(&p.ts_sock),
        None,
        "an answer that says nothing: not checked"
    );
    srv.join().unwrap();
    let _ = std::fs::remove_dir_all(&r);
}

#[test]
fn chill_on_and_healthy() {
    let (r, p) = root("chill");
    std::fs::create_dir_all(&p.chill_dir).unwrap();
    std::fs::create_dir_all(r.join("proc/777")).unwrap();
    std::os::unix::fs::symlink("/data/chill/bin/mihomo", r.join("proc/777/exe")).unwrap();
    std::fs::write(&p.chill_state, "{\"state\":\"running\",\"core_pid\":777}\n").unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let mut st = ReadState::default();
    let x = st.read(&p, &BTreeMap::new(), now);
    assert!(x.chill_on && x.chill_ok);
    std::fs::write(p.chill_dir.join("pause_until"), format!("{}\n", now + 600)).unwrap();
    assert!(!st.read(&p, &BTreeMap::new(), now).chill_on, "paused");
    std::fs::remove_file(p.chill_dir.join("pause_until")).unwrap();
    std::fs::write(&p.chill_idle, format!("{}\n", now + 120)).unwrap();
    assert!(
        !st.read(&p, &BTreeMap::new(), now).chill_on,
        "paused by the core (6e)"
    );
    std::fs::write(&p.chill_idle, format!("{}\n", now - 1)).unwrap();
    assert!(
        st.read(&p, &BTreeMap::new(), now).chill_on,
        "a lapsed lease is no pause"
    );
    std::fs::remove_file(&p.chill_idle).unwrap();
    // its state is old: on, not healthy
    let x = st.read(&p, &BTreeMap::new(), now + 300);
    assert!(x.chill_on && !x.chill_ok);
    assert_eq!(x.profile.as_deref(), Some("standard"), "no profile file");
    std::fs::write(p.chill_dir.join("profile"), "perf\n").unwrap();
    assert_eq!(
        st.read(&p, &BTreeMap::new(), now).profile.as_deref(),
        Some("perf")
    );
    std::fs::write(&p.thermal_eco, "").unwrap();
    assert_eq!(
        st.read(&p, &BTreeMap::new(), now).profile.as_deref(),
        Some("eco"),
        "too hot"
    );
    std::fs::write(p.chill_dir.join("disabled"), "").unwrap();
    assert_eq!(
        st.read(&p, &BTreeMap::new(), now).profile.as_deref(),
        Some("off")
    );
    let _ = std::fs::remove_dir_all(&r);
}

#[test]
fn chill_profile_per_hour() {
    let mut a = Acc::default();
    let r = |p: &str| Read {
        profile: Some(p.into()),
        ..Default::default()
    };
    a.add(&r("standard"), None, 60, 60, false, 60);
    a.add(&r("standard"), None, 60, 60, false, 120);
    assert_eq!(a.power()["chill_profile"], "standard");
    a.add(&r("eco"), None, 60, 60, false, 180);
    assert_eq!(a.power()["chill_profile"], "mixed");
    assert_eq!(
        Acc::default().power()["chill_profile"],
        Value::Null,
        "no CHILL"
    );
}

#[test]
fn radio_quiet_packets_and_clients_screen_off_only() {
    use crate::cell_window::QuietTake;
    let w = |q, o, k| {
        Some(QuietTake {
            quiet_ms: q,
            obs_ms: o,
            pkts: k,
        })
    };
    let mut a = Acc::default();
    // screen off at home, nobody: 45 of 60 s quiet
    let mut r = Read {
        wan: w(45_000, 60_000, 12),
        clients: Some(0),
        ..Default::default()
    };
    a.add(&r, None, 60, 60, false, 60);
    // screen off at home, clients unknown: counts as clients
    r.wan = w(15_000, 60_000, 30);
    r.clients = None;
    a.add(&r, None, 60, 60, false, 120);
    // screen on, charging, abroad: kept apart
    r.screen_on = true;
    r.wan = w(60_000, 60_000, 0);
    a.add(&r, None, 60, 60, false, 180);
    r.screen_on = false;
    r.charging = true;
    a.add(&r, None, 60, 60, false, 240);
    r.charging = false;
    r.clients = Some(2);
    r.wan = w(30_000, 60_000, 5);
    a.add(&r, None, 60, 60, true, 300);
    let p = a.power();
    assert_eq!(p["off_home_quiet_pm"], 500, "60 of 120 s");
    assert_eq!(p["off_home_wan_pkts"], 42);
    assert_eq!(
        p["off_home_cli_s"], 60,
        "the unknown minute; abroad is not home"
    );
    assert_eq!(p["off_abroad_quiet_pm"], 500);
    assert_eq!(p["off_abroad_wan_pkts"], 5);
    // nothing compared: null, not 0
    let b = Acc::default().power();
    assert_eq!(b["off_home_quiet_pm"], Value::Null);
    assert_eq!(b["off_home_wan_pkts"], Value::Null);
    assert_eq!(b["off_home_cli_s"], 0);
}

#[test]
fn an_hour_saved_before_the_radio_fields_still_loads() {
    let mut v = serde_json::to_value(Acc::default()).unwrap();
    for k in [
        "off_quiet_ms",
        "off_obs_ms",
        "off_pkts",
        "off_home_cli_s",
        "profile",
        "profile_mixed",
    ] {
        v.as_object_mut().unwrap().remove(k);
    }
    let a: Acc = serde_json::from_value(v).unwrap();
    assert_eq!(a.power()["off_home_cli_s"], 0);
}
