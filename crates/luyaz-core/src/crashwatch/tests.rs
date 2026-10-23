//! The crash watcher's cases as timelines through the replay, plus the event-driven parts.

use super::replay::{parse, run};
use super::*;
use serde_json::Value;

const BOOT: &str = "feedc0de-0000-4000-8000-000000000001";
const MODEM: &str = "qcom_q6v5_pas 4080000.remoteproc-mss:";

fn events(tl: &str) -> Vec<Value> {
    run(&parse(tl).expect("timeline"), BOOT, false).events
}
fn kind<'a>(ev: &'a [Value], k: &str) -> Vec<&'a Value> {
    ev.iter().filter(|e| e["k"] == k).collect()
}
fn result(ev: &[Value], seq: u64) -> Value {
    ev.iter()
        .find(|e| e["k"] == "ssr_result" && e["ssr"] == format!("ssr-feedc0de-{seq}"))
        .cloned()
        .unwrap_or(Value::Null)
}

#[test]
fn parses_kmsg_records_like_the_guard() {
    let r = parse_rec(
        "3,3008,484565365,-,caller=T64;qcom: fatal error received: x.c:1:y\n SUBSYSTEM=a\n",
    )
    .unwrap();
    assert_eq!((r.seq, r.ts_us), (3008, 484_565_365));
    assert_eq!(r.text, "qcom: fatal error received: x.c:1:y");
    assert!(parse_rec(" SUBSYSTEM=platform").is_none());
    assert!(parse_rec("ror received: torn").is_none());
    assert!(parse_rec("6,1,1000000;no flags field").is_none());
    assert_eq!(parse_rec("6,2,5,;empty flags").unwrap().text, "empty flags");
}

#[test]
fn classifies_the_guards_patterns() {
    for t in [
        "qcom_q6v5_pas 4080000.remoteproc-mss: fatal error received: a",
        "x: rproc recovery state: enabled and kick reovery process",
        "remoteproc remoteproc0: crash detected in 4080000.remoteproc-mss: type fatal error",
        "remoteproc remoteproc0: recovering 4080000.remoteproc-mss",
        "Subsystem Restart: x",
        "ERR_FATAL",
        "watchdog received: y",
    ] {
        assert_eq!(classify(t), Some(Find::Crash), "{t}");
    }
    assert_eq!(classify("recovering something else"), None);
    assert_eq!(
        classify("Out of memory: Killed process 1 (a)"),
        Some(Find::Oom)
    );
    assert_eq!(
        classify("Memory cgroup out of memory: Killed process 1"),
        Some(Find::Oom)
    );
    assert_eq!(
        classify("zte_thermal_update_throttling_level() to 0x3"),
        Some(Find::Thermal)
    );
    assert_eq!(classify("PM: suspend entry (deep)"), Some(Find::Suspend));
    assert_eq!(classify("BATTERY_CHG: x"), None);
}

#[test]
fn clean_matches_ledger_clean() {
    assert_eq!(clean("a \"b\\c\"\td\u{1}é"), "a bc d");
    assert_eq!(clean(&"y".repeat(400)).len(), 160);
}

#[test]
fn md5_ids_match_md5sum() {
    // printf ipa_driver_1.elf | md5sum | cut -c1-8
    assert_eq!(md5_8("ipa_driver_1.elf"), "96ba8103");
    assert_eq!(md5_8(""), "d41d8cd9");
    assert_eq!(md5_8("abc"), "90015098");
}

#[test]
fn one_crash_four_lines_down_back_traffic_ok() {
    let ev = events(&format!(
        "t 1000\nR 1000 10\nt 1002\nk 3008 484 {MODEM} fatal error received: rf.c:5428:Assertion ((a == \"x\\y\"))\n\
         k 3009 484 {MODEM} rproc recovery state: enabled and kick reovery process\n\
         k 3010 484 remoteproc remoteproc0: crash detected in 4080000.remoteproc-mss: type fatal error\n\
         k 3012 484 remoteproc remoteproc0: recovering 4080000.remoteproc-mss\nL down\n\
         t 1008\nL up\nt 1012\nR 5000 40\nt 1030\nE\n"
    ));
    let ssr = kind(&ev, "ssr");
    assert_eq!(ssr.len(), 1);
    assert_eq!(ssr[0]["id"], "ssr-feedc0de-3008");
    assert_eq!(ssr[0]["line"], "rf.c:5428:Assertion ((a == xy))");
    let r = result(&ev, 3008);
    assert_eq!(r["result"], "ok");
    assert_eq!(r["recovered_s"], 6);
    assert_eq!(r["rx_seen_s"], 10);
    assert_eq!(
        (
            r["down_seen"].clone(),
            r["resumed"].clone(),
            r["gap_s"].clone(),
            r["slept"].clone()
        ),
        (1.into(), 0.into(), 0.into(), 0.into())
    );
    let links = kind(&ev, "link");
    assert_eq!(links.len(), 2);
    assert!(links.iter().all(|l| l["cause"] == "ssr" && l["res"] == 0));
}

#[test]
fn no_down_in_30s_is_no_drop_by_first_traffic() {
    let ev = events(&format!(
        "t 1000\nR 1000 10\nt 1002\nk 500 600 {MODEM} fatal error received: a.c:1:boom\nt 1008\nR 3000 30\nt 1100\nE\n"
    ));
    let r = result(&ev, 500);
    assert_eq!(r["result"], "no_drop");
    assert_eq!(r["recovered_s"], Value::Null);
    assert_eq!(r["rx_seen_s"], 6);
    assert_eq!(r["down_seen"], 0);
}

#[test]
fn no_down_no_traffic_no_drop_at_90s() {
    let ev = events(&format!(
        "t 1000\nR 1000 10\nt 1002\nk 500 600 {MODEM} fatal error received: a.c:1:boom\nt 1200\nE\n"
    ));
    let r = result(&ev, 500);
    assert_eq!(r["result"], "no_drop");
    assert_eq!(r["rx_seen_s"], Value::Null);
    assert_eq!(r["up"], 1092.0);
}

#[test]
fn down_and_not_back_times_out_at_600s() {
    let ev = events(
        "t 1000\nt 1002\nk 500 600 remoteproc remoteproc0: crash detected in 4080000.remoteproc-mss: type watchdog\nL down\nt 1700\nE\n",
    );
    let r = result(&ev, 500);
    assert_eq!(r["result"], "timeout");
    assert_eq!(r["up"], 1602.0);
    assert_eq!(
        kind(&ev, "ssr")[0]["line"],
        "remoteproc remoteproc0: crash detected in 4080000.remoteproc-mss: type watchdog"
    );
}

#[test]
fn same_crash_within_60_kernel_s_merged_after() {
    let ev = events(&format!(
        "t 1000\nt 1002\nk 500 600 {MODEM} fatal error received: a.c:1:first\nL down\n\
         t 1020\nk 520 640 {MODEM} fatal error received: a.c:1:again\n\
         t 1080\nk 540 700 {MODEM} fatal error received: a.c:1:second\nt 1090\nE\n"
    ));
    let ids: Vec<_> = kind(&ev, "ssr").iter().map(|e| e["id"].clone()).collect();
    assert_eq!(
        ids,
        vec![
            Value::from("ssr-feedc0de-500"),
            Value::from("ssr-feedc0de-540")
        ]
    );
    assert_eq!(result(&ev, 500)["result"], "merged");
}

#[test]
fn back_no_traffic_then_another_crash_first_ends_ok() {
    let ev = events(&format!(
        "t 1000\nt 1002\nk 500 600 {MODEM} fatal error received: a.c:1:first\nL down\nt 1006\nL up\n\
         t 1012\nk 540 700 {MODEM} fatal error received: a.c:1:second\nt 1014\nE\n"
    ));
    let r = result(&ev, 500);
    assert_eq!(r["result"], "ok");
    assert_eq!(r["recovered_s"], 4);
    assert_eq!(r["rx_seen_s"], Value::Null);
    assert_eq!(kind(&ev, "ssr").len(), 2);
}

#[test]
fn restarted_while_following_carries_on_resumed() {
    let ev = events(&format!(
        "t 1000\nR 1000 10\nt 1002\nk 500 600 {MODEM} fatal error received: a.c:1:x\nL down\n\
         t 1034\nX\nL up\nt 1036\nR 9000 90\nt 1040\nE\n"
    ));
    let r = result(&ev, 500);
    assert_eq!(r["result"], "ok");
    assert_eq!(r["resumed"], 1);
    assert_eq!(r["down_seen"], 1);
    assert!(kind(&ev, "sleep").is_empty());
    // a restart re-reads the ring: the crash is not opened twice
    assert_eq!(kind(&ev, "ssr").len(), 1);
}

#[test]
fn crash_before_the_core_started_counts_resumed_gap_since_boot() {
    let ev = events(&format!(
        "t 0\nk 500 600 {MODEM} fatal error received: a.c:1:early\nt 1000\nX\nR 5000 50\nt 1100\nE\n"
    ));
    // the first wake is at 0 (the timeline's start); X at 1000 is the core's real start
    let r = result(&ev, 500);
    assert!(!r.is_null());
}

#[test]
fn first_go_live_takes_the_ring_as_seen() {
    let tl = format!(
        "t 1000\nk 500 600 {MODEM} fatal error received: a.c:1:before\nk 501 700 zte_thermal_monitor: zte_thermal_update_throttling_level() to 0x3\n\
         t 1010\nk 520 800 {MODEM} fatal error received: a.c:1:after\nt 1020\nE\n"
    );
    let ev = run(&parse(&tl).unwrap(), BOOT, true).events;
    let ids: Vec<_> = kind(&ev, "ssr").iter().map(|e| e["id"].clone()).collect();
    assert_eq!(ids, vec![Value::from("ssr-feedc0de-520")]);
    assert!(kind(&ev, "thermal").is_empty());
}

#[test]
fn oom_and_thermal() {
    let ev = events(
        "t 1000\nt 1002\nk 800 900 Out of memory: Killed process 1234 (luyaz-web) total-vm:1kB\n\
         k 801 900 zte_thermal_monitor soc:zte_thermal_monitor: zte_thermal_update_throttling_level() to 0x3\nt 1004\nE\n",
    );
    assert_eq!(kind(&ev, "oom")[0]["id"], "oom-feedc0de-800");
    let th = kind(&ev, "thermal");
    assert_eq!(th[0]["id"], "th-feedc0de-801");
    assert_eq!(th[0]["level"], "0x3");
}

#[test]
fn sleep_by_the_clocks_with_and_without_evidence() {
    let ev = events(
        "t 1000\nS 4\nt 1006\nk 900 950 PM: suspend entry (deep)\nZ 40\nt 1046\nk 901 951 PM: suspend exit\n\
         t 1060\nZ 30\nt 1090\nS 5\nk 902 960 rmnet: woke\nt 1100\nZ 30\nt 1130\nk 903 970 rmnet: woke\nt 1140\nE\n",
    );
    let s = kind(&ev, "sleep");
    assert_eq!(s.len(), 3, "{s:?}");
    assert_eq!(
        (s[0]["from"].clone(), s[0]["to"].clone(), s[0]["ev"].clone()),
        (1006.into(), 1046.into(), "pm".into())
    );
    assert_eq!(s[1]["ev"], "stats");
    assert_eq!(s[2]["ev"], "clock");
    assert_eq!(s[2]["up"], 1136.0);
}

#[test]
fn asleep_while_following_marks_slept() {
    let ev = events(&format!(
        "t 1000\nR 1000 10\nt 1002\nk 500 600 {MODEM} fatal error received: a.c:1:x\nL down\nt 1004\nZ 40\nt 1044\nk 501 601 PM: suspend exit\nL up\nt 1048\nR 9000 90\nt 1060\nE\n"
    ));
    let r = result(&ev, 500);
    assert_eq!(r["slept"], 1);
    assert!(r["gap_s"].as_u64().unwrap() >= 38);
}

#[test]
fn dumps_in_the_window_once_each() {
    let ev = events(&format!(
        "t 1000\nD old_1.elf 10\nt 1002\nk 500 600 {MODEM} fatal error received: a.c:1:x\n\
         t 1004\nD ipa_driver_1.elf 100\nt 1006\nD ipa_driver_1.elf 200\nt 1012\nD x.elf 50\n\
         t 1096\nD late.elf 70\nt 1100\nE\n"
    ));
    let d = kind(&ev, "ssr_dump");
    let files: Vec<_> = d
        .iter()
        .map(|e| (e["file"].clone(), e["size"].clone()))
        .collect();
    assert!(
        files.contains(&("ipa_driver_1.elf".into(), 200.into())),
        "{files:?}"
    );
    assert!(files.contains(&("x.elf".into(), 50.into())));
    assert_eq!(files.len(), 2);
    assert_eq!(d[0]["ssr"], "ssr-feedc0de-500");
}

#[test]
fn quiet_device_wakes_only_on_records() {
    // 50 lone records an hour, no crash: one wake each, no timer
    let mut tl = String::from("t 0\n");
    for i in 0..50u64 {
        tl.push_str(&format!(
            "t {}\nk {} {} BATTERY_CHG: x\n",
            10 + i * 72,
            i + 2,
            10 + i * 72
        ));
    }
    tl.push_str("t 3600\nE\n");
    let o = run(&parse(&tl).unwrap(), BOOT, false);
    assert_eq!(o.timer_wakes, 0);
    assert!(o.wakes <= 51, "{}", o.wakes);
}

#[test]
fn a_burst_is_read_at_most_every_interval() {
    // 10 records a second for a minute
    let mut tl = String::from("t 0\n");
    for i in 0..600u64 {
        tl.push_str(&format!(
            "t {:.1}\nk {} 1 wifi_bs: spam\n",
            i as f64 / 10.0,
            i + 2
        ));
    }
    tl.push_str("t 70\nE\n");
    let o = run(&parse(&tl).unwrap(), BOOT, false);
    assert!(o.wakes <= 35, "{}", o.wakes);
}

#[test]
fn net_from_blocks() {
    let sig =
        serde_json::json!({"type": "SA", "band": "n78", "nr_band": "n78", "mcc": 460, "mnc": 11});
    let sim = serde_json::json!({"imsi": "460110123456789"});
    let n = Net::from_blocks(Some(&sig), Some(&sim), Some(5));
    assert_eq!(n.net.as_deref(), Some("460-11"));
    assert_eq!(n.home.as_deref(), Some("460-11"));
    let n = Net::from_blocks(
        Some(&serde_json::json!({"mcc": 310, "mnc": 260})),
        Some(&serde_json::json!({"imsi": "310260000000000"})),
        None,
    );
    assert_eq!(n.net.as_deref(), Some("310-260"));
    assert_eq!(n.home.as_deref(), Some("310-260"));
}

#[test]
fn route_and_netdev_parsers() {
    let rt = "Iface\tDestination\tGateway\tFlags\nbr-lan\t0042000A\t00000000\t0001\nrmnet_data0\t00000000\t00000000\t0001\n";
    assert!(io::route_up(rt, "rmnet_data0"));
    assert!(!io::route_up(
        "Iface\tDestination\nrmnet_data0\t0042000A\n",
        "rmnet_data0"
    ));
    let nd = "Inter-|   Receive\n face |bytes packets\n  rmnet_data0: 1000 10 0 0 0 0 0 0 5000 12 0 0 0 0 0 0\n";
    assert_eq!(io::netdev_of(nd, "rmnet_data0"), (Some(1000), Some(22)));
}

#[cfg(target_os = "linux")]
#[test]
fn inotify_drain_leaves_the_fd_quiet() {
    use std::os::fd::AsRawFd;
    let dir = std::env::temp_dir().join(format!("cw-ino-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let fd = io::open_inotify(&dir).expect("inotify");
    std::fs::write(dir.join("ipa_driver_x.elf"), b"x").unwrap();
    let ready = |fd: i32| {
        let mut p = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll one pollfd we own, no wait.
        unsafe { libc::poll(&mut p, 1, 0) }
    };
    assert_eq!(ready(fd.as_raw_fd()), 1);
    assert_eq!(io::drain_inotify(fd.as_raw_fd()), (true, false));
    assert_eq!(
        ready(fd.as_raw_fd()),
        0,
        "still readable after the drain: poll would spin"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A real queue overflow (more events than `max_queued_events`) is seen as one (§19 T9c: the
/// dump route is down from its last clean read), and the fd is quiet after the drain.
#[cfg(target_os = "linux")]
#[test]
fn inotify_overflow_is_seen_on_a_real_fd() {
    use std::os::fd::AsRawFd;
    let max: usize = std::fs::read_to_string("/proc/sys/fs/inotify/max_queued_events")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(16384);
    let dir = std::env::temp_dir().join(format!("cw-ino-over-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // two files opened for writing and closed in turn: one close-write each (alternating names,
    // so the kernel does not merge them), no files created: light on a busy test run
    let (a, b) = (dir.join("a"), dir.join("b"));
    std::fs::write(&a, b"").unwrap();
    std::fs::write(&b, b"").unwrap();
    let fd = io::open_inotify(&dir).expect("inotify");
    for i in 0..max + 64 {
        drop(
            std::fs::OpenOptions::new()
                .write(true)
                .open(if i % 2 == 0 { &a } else { &b })
                .unwrap(),
        );
    }
    assert_eq!(io::drain_inotify(fd.as_raw_fd()), (true, true));
    let mut p = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: poll one pollfd we own, no wait.
    assert_eq!(unsafe { libc::poll(&mut p, 1, 0) }, 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn inotify_overflow_parser_walks_names() {
    let ev = |mask: u32, name: &[u8]| {
        let mut v = Vec::new();
        v.extend_from_slice(&1i32.to_ne_bytes());
        v.extend_from_slice(&mask.to_ne_bytes());
        v.extend_from_slice(&0u32.to_ne_bytes());
        v.extend_from_slice(&(name.len() as u32).to_ne_bytes());
        v.extend_from_slice(name);
        v
    };
    let mut buf = ev(libc::IN_CREATE, &[b'a'; 16]);
    assert!(!io::inotify_overflow(&buf));
    // an overflow bit inside a name is not an event
    buf.extend(ev(
        libc::IN_CLOSE_WRITE,
        &libc::IN_Q_OVERFLOW.to_ne_bytes().repeat(4),
    ));
    assert!(!io::inotify_overflow(&buf));
    buf.extend(ev(libc::IN_Q_OVERFLOW, &[]));
    assert!(io::inotify_overflow(&buf));
}

/// The probe is a real eventfd: readable after a probe, quiet after the watcher took it.
#[cfg(target_os = "linux")]
#[test]
fn probe_wakes_and_is_taken() {
    let sh = crate::longterm::shared();
    let fd = sh.probe_fd().expect("eventfd");
    let ready = || {
        let mut p = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll one pollfd we own, no wait.
        unsafe { libc::poll(&mut p, 1, 0) }
    };
    sh.take_probe();
    assert_eq!(ready(), 0);
    sh.probe();
    sh.probe();
    assert_eq!(ready(), 1);
    sh.take_probe();
    assert_eq!(ready(), 0, "still readable after the take: poll would spin");
}

/// The streaming MD5 gives the same as the one-shot one, whatever the pieces; md5_file too.
#[test]
fn md5_in_pieces_and_from_a_file() {
    let data: Vec<u8> = (0..200_003u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    let whole = super::md5(&data);
    for step in [1usize, 3, 63, 64, 65, 4096, 65_536] {
        let mut m = super::Md5::new();
        for c in data.chunks(step) {
            m.update(c);
        }
        assert_eq!(m.finish(), whole, "pieces of {step}");
    }
    // RFC 1321's own vectors
    let hx = |d: [u8; 16]| d.iter().map(|b| format!("{b:02x}")).collect::<String>();
    assert_eq!(hx(super::md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
    assert_eq!(
        hx(super::md5(
            b"12345678901234567890123456789012345678901234567890123456789012345678901234567890"
        )),
        "57edf4a22be3c955ac49da2e2107b67a"
    );
    let f = std::env::temp_dir().join(format!("u60d-md5-{}", std::process::id()));
    std::fs::write(&f, &data).unwrap();
    assert_eq!(super::md5_file(&f), Some(whole));
    let _ = std::fs::remove_file(&f);
    assert_eq!(
        super::md5_file(std::path::Path::new("/nonexistent/x")),
        None
    );
}
