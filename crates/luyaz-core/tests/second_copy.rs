//! Phase 10 X9: without supervise.sh nothing clears a leftover copy, so a second core must refuse
//! to run: it cannot bind the port, exits, and never touches the running marker (written only once
//! both listeners are bound).

use std::time::{Duration, Instant};

#[test]
fn a_second_core_exits_on_the_busy_port_without_a_marker() {
    let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = held.local_addr().unwrap().port();
    let root = std::env::temp_dir().join(format!("u60d-second-copy-{}", std::process::id()));
    std::fs::create_dir_all(root.join("data/luyaz/core")).unwrap();
    // `luyaz-core serve` runs as luyaz-core (keeps a marker once it is the one core)
    let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_luyaz-core"))
        .args(["serve", "-p", &port.to_string(), "--data-dir"])
        .arg(root.join("core"))
        .env("LUYAZ_HOST_ROOT", &root)
        .env("LUYAZ_PID_FILE", "")
        .env("LUYAZ_UBUS", "cli")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let t0 = Instant::now();
    let st = loop {
        if let Some(s) = c.try_wait().unwrap() {
            break s;
        }
        if t0.elapsed() > Duration::from_secs(20) {
            let _ = c.kill();
            panic!("a second core kept running");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(!st.success());
    assert!(!root.join("data/luyaz/core/core.running").exists());
    let _ = std::fs::remove_dir_all(&root);
    drop(held);
}
