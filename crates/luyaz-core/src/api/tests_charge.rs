//! Step 9c: the charge limit and the battery estimate, u60d's with writes on.
//! One test: ownership is process-wide (`plugin::charge`), like the agent's single enforcer.

use super::tests_ops::{Dev, Shared, control_server, start};
use super::*;
use crate::plugin::charge_policy::Job;
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
    for _ in 0..500 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("never got there: {what}");
}

/// The charge writes so far: (source, stopped).
fn writes(d: &Shared) -> Vec<(String, bool)> {
    d.lock()
        .unwrap()
        .log
        .iter()
        .filter(|v| v["action"] == "power.direct_supply.set")
        .map(|v| {
            (
                v["source"].as_str().unwrap_or("").to_string(),
                v["params"]["enabled"].as_bool().unwrap(),
            )
        })
        .collect()
}

fn battery(root: &std::path::Path, capacity: u8, status: &str) {
    let b = root.join("sys/class/power_supply/battery");
    fs::create_dir_all(&b).unwrap();
    fs::write(b.join("capacity"), format!("{capacity}\n")).unwrap();
    fs::write(b.join("status"), format!("{status}\n")).unwrap();
}

#[tokio::test]
async fn charge_control_and_the_limit_with_writes_on() {
    let root = std::env::temp_dir().join(format!("u60d-charge-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("data/local/tmp")).unwrap();
    fs::write(
        root.join("data/local/tmp/charge_limit.json"),
        r#"{"enabled":true,"limit":80,"hysteresis":5}"#,
    )
    .unwrap();
    battery(&root, 85, "Charging");
    let d: Shared = Arc::new(Mutex::new(Dev::default()));
    let url = control_server(d.clone()).await;
    // the stop flag ubus reports; None = ubus does not answer
    let stopped: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(Some(false)));
    let s2 = stopped.clone();
    let src = Sources {
        ubus: Arc::new(move |o, m, _a| {
            let r = match ((o, m), *s2.lock().unwrap()) {
                (("zwrt_bsp.charger", "list"), Some(st)) => Ok(json!({
                    "direct_power_supply_mode": if st { "enable" } else { "disable" }, "charger_connect": 1})),
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
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(charge::run(c.clone(), rx));

    // writes and policies on: u60d owns it
    wait("owned", crate::plugin::charge::owned).await;

    // the settings as the agent left them
    let (st, b) = call(addr, "GET", "/api/device/charge-control", "", "").await;
    assert_eq!(st, 200, "{b}");
    let data = &b["data"];
    assert_eq!(
        (
            data["charge_limit_enabled"].clone(),
            data["charge_limit"].clone(),
            data["hysteresis"].clone()
        ),
        (json!(true), json!(80), json!(5))
    );
    assert_eq!(
        (data["capacity"].clone(), data["battery_status"].clone()),
        (json!(85), json!("Charging"))
    );
    assert_eq!(
        (
            data["charging_stopped"].clone(),
            data["manual_override"].clone()
        ),
        (json!(false), json!(false))
    );

    // a scheduled job's manual stop: the agent names the source; the override goes on
    let (st, b) = call(
        addr,
        "PUT",
        "/api/device/charge-control",
        "X-U60-Source: scheduler\r\n",
        r#"{"charging_stopped":true}"#,
    )
    .await;
    assert_eq!(
        (st, b["data"]["manual_override"].clone()),
        (200, json!(true)),
        "{b}"
    );
    assert_eq!(writes(&d), vec![("scheduler".to_string(), true)]);
    *stopped.lock().unwrap() = Some(true);
    // override on: the loop leaves it alone
    tx.send(Job::Limit).unwrap();
    battery(&root, 70, "Not charging");
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(writes(&d).len(), 1);

    // bad settings: 400, nothing changes
    let (st, b) = call(
        addr,
        "PUT",
        "/api/device/charge-control",
        "",
        r#"{"charge_limit":300}"#,
    )
    .await;
    assert_eq!(
        (st, b["error"].clone()),
        (400, json!("limit must be 50-100"))
    );
    assert_eq!(writes(&d).len(), 1);

    // the limit off from the web: always resumes (a manual stop too), override cleared, file saved
    let (st, b) = call(
        addr,
        "PUT",
        "/api/device/charge-control",
        "X-U60-Source: web\r\n",
        r#"{"charge_limit_enabled":false}"#,
    )
    .await;
    assert_eq!(
        (st, b["data"]["manual_override"].clone()),
        (200, json!(false)),
        "{b}"
    );
    assert_eq!(writes(&d)[1], ("web".to_string(), false));
    assert_eq!(
        fs::read_to_string(root.join("data/local/tmp/charge_limit.json")).unwrap(),
        r#"{"enabled":false,"limit":80,"hysteresis":5}"#
    );
    *stopped.lock().unwrap() = Some(false);

    // the limit on again from the screen (no source header) at 85 %: enforced at once, as the screen
    battery(&root, 85, "Charging");
    let (st, _) = call(
        addr,
        "PUT",
        "/api/device/charge-control",
        "",
        r#"{"charge_limit_enabled":true}"#,
    )
    .await;
    assert_eq!(st, 200);
    assert_eq!(writes(&d)[2], ("screen".to_string(), true));
    *stopped.lock().unwrap() = Some(true);

    // the loop: down to the band's bottom resumes, under auto
    battery(&root, 75, "Not charging");
    tx.send(Job::Limit).unwrap();
    wait("resumed", || writes(&d).len() == 4).await;
    assert_eq!(writes(&d)[3], ("auto".to_string(), false));
    *stopped.lock().unwrap() = Some(false);
    // plugged in over the limit: stops (after the agent's half second)
    battery(&root, 81, "Charging");
    tx.send(Job::PluggedIn).unwrap();
    wait("stopped", || writes(&d).len() == 5).await;
    assert_eq!(writes(&d)[4], ("auto".to_string(), true));
    // the stop flag unknown (ubus silent): nothing is written
    *stopped.lock().unwrap() = None;
    battery(&root, 60, "Not charging");
    tx.send(Job::Limit).unwrap();
    tx.send(Job::Unplugged).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(writes(&d).len(), 5);
    // unplugged while stopped: resumes for the next plug-in
    *stopped.lock().unwrap() = Some(true);
    tx.send(Job::Unplugged).unwrap();
    wait("resumed on unplug", || writes(&d).len() == 6).await;
    assert_eq!(writes(&d)[5], ("auto".to_string(), false));
    let dbg = crate::plugin::charge_policy::debug();
    assert_eq!(dbg["owner"], "u60d");
    assert_eq!(dbg["writes"][0]["why"], "unplugged");
}
