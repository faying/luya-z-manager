//! Step 9d: the health check, u60d's with writes on.

use super::tests_ops::start;
use super::*;
use std::fs;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn call(addr: std::net::SocketAddr, method: &str, path: &str, body: &str) -> (u16, Value) {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    let status = out[9..12].parse().unwrap();
    let json = out.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
    (status, serde_json::from_str(json).unwrap_or(Value::Null))
}

fn runs(root: &std::path::Path) -> usize {
    fs::read_to_string(root.join("runs"))
        .map(|t| t.lines().count())
        .unwrap_or(0)
}

async fn wait_runs(root: &std::path::Path, n: usize) {
    for _ in 0..1000 {
        if runs(root) >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("doctor ran {} times, wanted {n}", runs(root));
}

#[tokio::test]
async fn u60d_runs_the_doctor_with_writes_on() {
    let root = std::env::temp_dir().join(format!("u60d-health-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("data/luyaz/scripts")).unwrap();
    fs::create_dir_all(root.join("tmp")).unwrap();
    // a doctor that counts its runs and reports the count
    fs::write(
        root.join("data/luyaz/scripts/doctor.sh"),
        format!(
            "echo x >> {r}/runs; n=$(wc -l < {r}/runs); printf '#tsv2\\nwarn\\tsms\\t告警短信\\t第%s次\\tAlert SMS\\trun %s\\n' $n $n\n",
            r = root.display()
        ),
    )
    .unwrap();
    // the last snapshot
    let agents = luyaz_api::health::Snapshot {
        checks: vec![],
        at_device: 1,
        ran: true,
        error: None,
    };
    fs::write(root.join("tmp/health.json"), agents.to_json().to_string()).unwrap();
    let c = Compat::new(root.to_str().unwrap(), Sources::none()).with_writes(true);
    let addr = start(c.clone()).await;
    tokio::spawn(health::run(c.clone()));

    // the last snapshot is read as it is
    let (st, b) = call(addr, "GET", "/api/health", "").await;
    assert_eq!(
        (st, b["data"]["checked_at"].clone()),
        (200, json!(1)),
        "{b}"
    );
    // nothing for the first 20 s, but every ask runs it
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(runs(&root), 0, "the first background run waits");
    // the screen marks alerts read
    let (st, _) = call(addr, "POST", "/api/alerts/read", r#"{"seq":0}"#).await;
    assert_eq!(st, 200);
    wait_runs(&root, 1).await;
    // the web's 重新检查: run, then answer
    let (st, b) = call(addr, "GET", "/api/health?refresh=1", "").await;
    assert_eq!(st, 200);
    assert_eq!(runs(&root), 2);
    let checks = b["data"]["checks"].as_array().unwrap();
    assert_eq!(checks[0]["detail_en"], "run 2", "{b}");
    // the public status counts it
    let (_, b) = call(addr, "GET", "/api/public/status", "").await;
    assert_eq!(
        b["data"]["health"],
        json!({"checked": true, "bad": 0, "warn": 1})
    );
}
