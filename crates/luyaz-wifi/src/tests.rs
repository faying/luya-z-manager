use super::*;
use std::fs::OpenOptions;
use std::os::unix::io::AsRawFd;
use std::time::{Duration, Instant};

fn assert_send<T: Send>() {}

#[test]
fn guard_is_send() {
    // wifi_set moves its lock into the reload thread.
    assert_send::<WifiLock>();
}

#[test]
fn vendor_switch_only_when_exactly_off_and_turning_on() {
    assert!(vendor_switch_needs_on(true, Some("0")));
    // On, missing (= on), unreadable, or anything else: leave it.
    assert!(!vendor_switch_needs_on(true, Some("1")));
    assert!(!vendor_switch_needs_on(true, Some("")));
    assert!(!vendor_switch_needs_on(true, None));
    assert!(!vendor_switch_needs_on(true, Some(" 0")));
    // Turning off (home) never touches it.
    assert!(!vendor_switch_needs_on(false, Some("0")));
    assert!(!vendor_switch_needs_on(false, Some("1")));
}

#[test]
fn expectation_follows_radio_and_ap_flags() {
    assert_eq!(expectation("0", "0", "0", "0", false), Expect::On { ap_2g: true, ap_5g: true });
    // Missing keys mean enabled.
    assert_eq!(expectation("", "", "", "", false), Expect::On { ap_2g: true, ap_5g: true });
    // Either layer disables a band.
    assert_eq!(expectation("1", "0", "0", "0", false), Expect::On { ap_2g: false, ap_5g: true });
    assert_eq!(expectation("0", "0", "0", "1", false), Expect::On { ap_2g: true, ap_5g: false });
    assert_eq!(expectation("0", "1", "0", "1", false), Expect::Off);
    assert_eq!(expectation("1", "1", "1", "1", false), Expect::Off);
    assert_eq!(expectation("0", "1", "0", "1", true), Expect::Unknown);
}

fn try_flock(path: &str) -> bool {
    let f = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path).unwrap();
    // SAFETY: valid fd; the lock goes away when `f` is dropped.
    unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

// One test, not several: the in-process layer is a process-wide static, so
// parallel tests taking it would time each other out.
#[test]
fn lock_takes_both_layers_and_releases_both() {
    let dir = std::env::temp_dir().join(format!("u60-wifi-lock-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("u60-wifi.lock");
    let path = path.to_str().unwrap();

    let lk = lock_at(path, Duration::from_secs(1)).expect("first lock");

    // Cross-process layer: a separate open file description cannot flock it
    // (flock conflicts between descriptions even within one process — the
    // same thing `flock` in a shell would see).
    assert!(!try_flock(path), "flock should be held");
    // The holder's pid is in the file for other holders.
    let pid: u32 = std::fs::read_to_string(path).unwrap().trim().parse().unwrap();
    assert_eq!(pid, std::process::id());
    // In-process layer: a second acquisition times out instead of hanging.
    let t0 = Instant::now();
    let second = std::thread::spawn({
        let p = path.to_string();
        move || lock_at(&p, Duration::from_millis(300)).is_err()
    })
    .join()
    .unwrap();
    assert!(second, "second in-process lock must time out");
    assert!(t0.elapsed() >= Duration::from_millis(300));

    // A waiter gets it once the holder lets go — including a holder that
    // released it from another thread, the way finish_in_background does.
    let waiter = std::thread::spawn({
        let p = path.to_string();
        move || lock_at(&p, Duration::from_secs(5)).map(drop).is_ok()
    });
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        drop(lk);
    })
    .join()
    .unwrap();
    assert!(waiter.join().unwrap(), "waiter should acquire after release");

    // Both layers are free again.
    assert!(try_flock(path), "flock should be free");
    drop(lock_at(path, Duration::from_millis(100)).expect("in-process layer free"));

    // A process-external holder (another process, e.g. the sentinel) blocks the
    // flock layer until the deadline, and the in-process flag is given back.
    let outside = OpenOptions::new().read(true).write(true).open(path).unwrap();
    // SAFETY: valid fd.
    assert_eq!(unsafe { libc::flock(outside.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0);
    assert!(lock_at(path, Duration::from_millis(300)).is_err());
    drop(outside);
    drop(lock_at(path, Duration::from_millis(300)).expect("free after outside holder left"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn max_hold_covers_a_normal_apply() {
    // reload (ubus 30 s timeout) + ON_DEADLINE must fit, with room.
    assert!(Duration::from_secs(30) + ON_DEADLINE < MAX_HOLD);
    assert!(MAX_HOLD < ENGINE_WAIT);
}

#[test]
fn a_reload_is_verified_only_when_hostapd_serves_the_new_name() {
    let st = "state=ENABLED\nchannel=1\nssid[0]=U-Chill\nbss[0]=wlan0\n";
    assert!(status_serves(st, Some("U-Chill")));
    assert!(!status_serves(st, Some("U-Chill-t")), "old hostapd, old name: not yet");
    assert!(status_serves(st, None), "no name to compare: ENABLED is enough");
    assert!(!status_serves("state=DISABLED\nssid[0]=U-Chill\n", Some("U-Chill")));
    assert!(!status_serves(st, Some("U-Chil")), "whole name, not a prefix");
}
