//! Phase 10 X9: the init gives luyaz-web one `--bind`; a second copy whose only address is in use
//! exits (it does not sit retrying next to the first), so procd's pid is the only luyaz-web.

use std::time::{Duration, Instant};

#[test]
fn a_second_copy_with_its_only_address_busy_exits() {
    let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = held.local_addr().unwrap().to_string();
    let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_luyaz-web"))
        .args(["serve", "--bind", &addr, "--upstream", "http://127.0.0.1:1"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let t0 = Instant::now();
    let st = loop {
        if let Some(s) = c.try_wait().unwrap() {
            break s;
        }
        if t0.elapsed() > Duration::from_secs(10) {
            let _ = c.kill();
            panic!("a second luyaz-web kept running");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(st.code(), Some(1));
    drop(held);
}
