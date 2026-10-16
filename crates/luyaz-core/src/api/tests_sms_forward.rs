//! Step 9e: SMS forwarding, u60d's with writes on.
//! One test: the shadow's owned flag is process-wide.

use super::tests_ops::{Dev, Shared, control_server, start};
use super::*;
use std::fs;
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn call(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    headers: &str,
    body: &str,
) -> (u16, Value) {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n{headers}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    let status = out[9..12].parse().unwrap();
    let json = out.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
    (status, serde_json::from_str(json).unwrap_or(Value::Null))
}

async fn wait(what: &str, f: impl Fn() -> bool) {
    for _ in 0..1000 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("never got there: {what}");
}

/// The SMS sends so far: (source, number).
fn sends(d: &Shared) -> Vec<(String, String)> {
    d.lock()
        .unwrap()
        .log
        .iter()
        .filter(|v| v["action"] == "vendor.call" && v["params"]["method"] == "zte_libwms_send_sms")
        .map(|v| {
            (
                v["source"].as_str().unwrap_or("").to_string(),
                v["params"]["args"]["number"]
                    .as_str()
                    .unwrap_or("")
                    .to_string(),
            )
        })
        .collect()
}

#[tokio::test]
async fn u60d_forwards_with_writes_on() {
    let root = std::env::temp_dir().join(format!("u60d-smsfwd-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("data/local/tmp")).unwrap();
    fs::create_dir_all(root.join("tmp")).unwrap();
    // forwarding on, one rule to an SMS number; the agent got as far as id 5
    fs::write(
        root.join("data/local/tmp/sms_forward.json"),
        r#"{"enabled":true,"poll_interval_secs":30,"rules":[{"id":1,"name":"all","enabled":true,
            "filter":{"type":"all"},"destination":{"type":"sms","forward_number":"+200"}}]}"#,
    )
    .unwrap();
    fs::write(
        root.join("data/local/tmp/sms_forward_state.json"),
        r#"{"last_forwarded_id":5}"#,
    )
    .unwrap();
    let d: Shared = Arc::new(Mutex::new(Dev::default()));
    let url = control_server(d.clone()).await;
    let src = Sources {
        ubus: Arc::new(move |o, m, a| {
            let r = match (o, m) {
                ("zte_nwinfo_api", "nwinfo_get_netinfo") => Ok(json!({"network_type": "SA"})),
                ("zwrt_data", "get_wwaniface") => Ok(json!({"connect_status": "ipv4_connected"})),
                // the phone memory holds 6 (new, received) and 5; the SIM nothing; sent box empty
                ("zwrt_wms", "zte_libwms_get_sms_data") => {
                    let received = a["tags"] != json!(2);
                    if a["mem_store"] == json!(1) && a["page"] == json!(0) && received {
                        Ok(json!({"messages": [
                            {"id": "6", "tag": "0", "number": "+100", "content": "hello", "date": ""},
                            {"id": "5", "tag": "1", "number": "+100", "content": "old", "date": ""}]}))
                    } else {
                        Ok(json!({"messages": []}))
                    }
                }
                _ => Err("not faked".to_string()),
            };
            Box::pin(async move { r })
        }),
        control: Arc::new(control::Client::new(url, None)),
        ..Sources::none()
    };
    let c = Compat::new(root.to_str().unwrap(), src)
        .with_writes(true)
        .with_clock(1000);
    let addr = start(c.clone()).await;
    tokio::spawn(crate::engines::sms_forward::run(c.clone()));
    let lease = root.join("tmp/sms-forward.owner");

    // the lease is u60d's, SMS 6 goes out as auto, the watermark moves on
    wait("lease", || lease.exists()).await;
    let pid = std::process::id().to_string();
    assert!(fs::read_to_string(&lease).unwrap().starts_with(&pid));
    wait("forwarded", || !sends(&d).is_empty()).await;
    assert_eq!(sends(&d), vec![("auto".to_string(), "+200".to_string())]);
    let state = || {
        serde_json::from_str::<Value>(
            &fs::read_to_string(root.join("data/local/tmp/sms_forward_state.json")).unwrap(),
        )
        .unwrap()
    };
    wait("watermark", || state()["last_forwarded_id"] == json!(6)).await;
    let (st, b) = call(addr, "GET", "/api/sms/forward/log", "", "").await;
    assert_eq!(st, 200, "{b}");

    // a web test from the agent: the caller's source
    let (st, b) = call(
        addr,
        "POST",
        "/api/sms/forward/test",
        "X-U60-Source: web\r\n",
        r#"{"destination":{"type":"sms","forward_number":"+300"}}"#,
    )
    .await;
    assert_eq!(st, 200, "{b}");
    assert_eq!(sends(&d)[1], ("web".to_string(), "+300".to_string()));

    let _ = fs::remove_dir_all(&root);
}
