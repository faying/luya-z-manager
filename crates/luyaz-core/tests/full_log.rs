//! P-7: the core's log (stdout and stderr, /tmp/luyaz-core.log on the device) cannot be written,
//! as when /tmp is full. std's `eprintln!` panics on a failed write and the device build aborts on
//! any panic: on 10-26 a full /tmp crash-looped the core until procd gave up (down 50 min). The
//! core must keep serving with nowhere to log.

use std::time::{Duration, Instant};

#[test]
fn the_core_keeps_serving_when_its_log_cannot_be_written() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let root = std::env::temp_dir().join(format!("luyaz-full-log-{}", std::process::id()));
    std::fs::create_dir_all(root.join("data/luyaz/core")).unwrap();
    let full = || std::fs::OpenOptions::new().write(true).open("/dev/full").unwrap();
    let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_luyaz-core"))
        .args(["-p", &port.to_string(), "--data-dir"])
        .arg(root.join("core"))
        .env("LUYAZ_HOST_ROOT", &root)
        .env("LUYAZ_PID_FILE", "")
        .env("LUYAZ_UBUS", "cli")
        .env(
            "LUYAZ_UBUS_BIN",
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/mock_ubus.sh"),
        )
        .stdout(full())
        .stderr(full())
        .spawn()
        .unwrap();
    // it prints from the first moment ([mem], [compat] …): long enough for many lines
    let t0 = Instant::now();
    let mut answered = false;
    while t0.elapsed() < Duration::from_secs(8) {
        if let Some(s) = c.try_wait().unwrap() {
            let _ = std::fs::remove_dir_all(&root);
            panic!("the core ended with its log unwritable: {s}");
        }
        answered |= std::net::TcpStream::connect(("127.0.0.1", port)).is_ok();
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = c.kill();
    let _ = c.wait();
    let _ = std::fs::remove_dir_all(&root);
    assert!(answered, "the core never listened");
}
