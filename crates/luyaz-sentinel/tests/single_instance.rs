//! Phase 10 X9 follow-up: one sentinel at a time. A second copy (here: the test binary under any
//! name, the lock held by this test process) exits non-zero at once, before it makes the FIFO or
//! writes a heartbeat; `--shadow` takes no lock.
#![cfg(target_os = "linux")]

use std::time::{Duration, Instant};

fn tmp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("luyaz-sentinel-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn the_lock_is_exclusive_and_goes_with_its_file() {
    let d = tmp("lock");
    let p = d.join("sub/s.lock");
    let p = p.to_str().unwrap();
    let a = luyaz_sentinel::sys::single_instance(p).expect("first");
    let e = luyaz_sentinel::sys::single_instance(p).expect_err("second");
    assert!(e.contains("another luyaz-sentinel"), "{e}");
    drop(a);
    luyaz_sentinel::sys::single_instance(p).expect("free again");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_second_sentinel_exits_before_touching_anything() {
    let d = tmp("second");
    let lock = d.join("s.lock");
    let _held = luyaz_sentinel::sys::single_instance(lock.to_str().unwrap()).unwrap();
    let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_luyaz-sentinel"))
        .env("LUYAZ_SENTINEL_LOCK", &lock)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let t0 = Instant::now();
    let st = loop {
        if let Some(s) = c.try_wait().unwrap() {
            break s;
        }
        if t0.elapsed() > Duration::from_secs(10) {
            let _ = c.kill();
            panic!("a second sentinel kept running");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(st.code(), Some(3));
    let mut err = String::new();
    std::io::Read::read_to_string(&mut c.stderr.take().unwrap(), &mut err).unwrap();
    assert!(err.contains("not starting"), "{err}");
    assert!(t0.elapsed() < Duration::from_secs(5));
    let _ = std::fs::remove_dir_all(&d);
}
