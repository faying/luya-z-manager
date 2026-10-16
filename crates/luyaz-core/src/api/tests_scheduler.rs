//! 8d-1d: scheduled jobs, u60d's with writes on.

use crate::engines::scheduler::{Caller, NO_RESULT, now_local, tick};
use super::tests_ops::{Dev, Shared, control_server, start};
use super::*;
use std::fs;
use std::sync::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn call_with(
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

async fn call(addr: std::net::SocketAddr, method: &str, path: &str, body: &str) -> (u16, Value) {
    call_with(addr, method, path, "", body).await
}

type Reply = Box<dyn Fn(&str) -> (u16, Value) + Send + Sync>;

/// Records each call; `check` runs inside it (the file as it is while the call is in flight).
struct Fake {
    calls: Mutex<Vec<(String, String, String)>>,
    reply: Reply,
    check: Box<dyn Fn() + Send + Sync>,
}

impl Fake {
    fn new(reply: impl Fn(&str) -> (u16, Value) + Send + Sync + 'static) -> Self {
        Fake {
            calls: Mutex::default(),
            reply: Box::new(reply),
            check: Box::new(|| {}),
        }
    }
    fn calls(&self) -> Vec<(String, String, String)> {
        self.calls.lock().unwrap().clone()
    }
}

impl Caller for Fake {
    fn call(&self, method: &str, path: &str, body: &[u8]) -> (u16, Value) {
        (self.check)();
        self.calls.lock().unwrap().push((
            method.into(),
            path.into(),
            String::from_utf8_lossy(body).into(),
        ));
        (self.reply)(path)
    }
}

fn dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("u60d-sched-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(d.join("data/local/tmp")).unwrap();
    d
}

fn jobs_at(root: &std::path::Path) -> std::path::PathBuf {
    root.join("data/local/tmp/scheduler.json")
}

fn read(p: &std::path::Path) -> Vec<Value> {
    let v: Value = serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap();
    v["jobs"].as_array().unwrap().clone()
}

fn job(v: &[Value], id: u64) -> Value {
    v.iter().find(|j| j["id"] == id).unwrap().clone()
}

/// Monday 03:00 local, device clock 2026-10-12 03:00 "UTC".
const MON_3: (i64, u8, u8, u8) = (1_791_774_000, 3, 0, 0);

fn base_job(id: u32, method: &str, path: &str, schedule: Value) -> Value {
    json!({"id": id, "name": format!("j{id}"), "enabled": true, "schedule": schedule,
        "action": {"method": method, "path": path}, "last_run": null, "last_status": null,
        "last_restore": null, "created_at": 1})
}

#[test]
fn a_once_reboot_is_marked_before_it_runs_and_never_runs_again() {
    let root = dir("reboot");
    let p = jobs_at(&root);
    let j = base_job(1, "POST", "/api/device/reboot", json!({"type": "once", "at": MON_3.0 - 30}));
    fs::write(&p, json!({"jobs": [j]}).to_string()).unwrap();
    let p2 = p.clone();
    let mut fake = Fake::new(|_| (200, json!({"ok": true})));
    // while the reboot is in flight the file already says it ran and is off
    fake.check = Box::new(move || {
        let j = job(&read(&p2), 1);
        assert!(j["last_run"].is_i64(), "{j}");
        assert_eq!(j["enabled"], false, "{j}");
        assert_eq!(j["last_error"], NO_RESULT, "{j}");
    });
    assert_eq!(tick(&p, &fake, MON_3), 1);
    assert_eq!(fake.calls(), vec![("POST".into(), "/api/device/reboot".into(), "".into())]);
    let j = job(&read(&p), 1);
    assert_eq!((j["last_status"].clone(), j["last_error"].clone()), (json!(200), Value::Null));

    // the device rebooted before the result came back: the mark alone keeps it from running again
    let mut j = job(&read(&p), 1);
    j["last_status"] = Value::Null;
    j["last_error"] = json!(NO_RESULT);
    fs::write(&p, json!({"jobs": [j]}).to_string()).unwrap();
    let later = (MON_3.0 + 120, 3, 2, 0);
    assert_eq!(tick(&p, &fake, later), 0);
    assert_eq!(fake.calls().len(), 1);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn recurring_runs_once_a_minute_and_unsupported_or_forbidden_are_recorded() {
    let root = dir("rec");
    let p = jobs_at(&root);
    let every = json!({"type": "recurring", "time": "03:00", "days": [0]});
    let mut forbidden = base_job(3, "POST", "/api/esim/switch", every.clone());
    forbidden["enabled"] = json!(true); // put in the file by hand
    let jobs = json!({"jobs": [
        base_job(1, "PUT", "/api/wifi/guest", every.clone()),
        base_job(2, "POST", "/api/not/served", every.clone()),
        forbidden,
    ]});
    fs::write(&p, jobs.to_string()).unwrap();
    let fake = Fake::new(|path| match path {
        "/api/wifi/guest" => (200, json!({"ok": true})),
        _ => (404, json!({"ok": false, "error": "not_in_compat_yet"})),
    });
    assert_eq!(tick(&p, &fake, MON_3), 2);
    // 30 s later, same minute: nothing again
    assert_eq!(tick(&p, &fake, (MON_3.0 + 30, 3, 0, 0)), 0);
    let paths: Vec<String> = fake.calls().into_iter().map(|c| c.1).collect();
    assert_eq!(paths, vec!["/api/wifi/guest", "/api/not/served"]);
    let v = read(&p);
    assert_eq!(job(&v, 1)["last_status"], 200);
    assert_eq!(job(&v, 1)["enabled"], true);
    assert_eq!(job(&v, 2)["last_status"], 404);
    assert_eq!(job(&v, 2)["last_error"], "不支持：u60d 没有这个路径");
    let f = job(&v, 3);
    assert_eq!(f["enabled"], false);
    assert!(f["last_error"].as_str().unwrap().starts_with("已停用："), "{f}");
    // next Monday it runs again
    assert_eq!(tick(&p, &fake, (MON_3.0 + 7 * 86400, 3, 0, 0)), 2);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_once_job_with_restore_runs_then_puts_back_with_the_restore_body() {
    let root = dir("restore");
    let p = jobs_at(&root);
    let mut j = base_job(1, "PUT", "/api/wifi/guest", json!({"type": "once", "at": MON_3.0 - 10}));
    j["action"]["body"] = json!({"enabled": true});
    j["restore"] = json!({"time": "04:00", "body": {"enabled": false}});
    fs::write(&p, json!({"jobs": [j]}).to_string()).unwrap();
    let fake = Fake::new(|_| (200, json!({"ok": true})));
    assert_eq!(tick(&p, &fake, MON_3), 1);
    assert_eq!(job(&read(&p), 1)["enabled"], true, "a once job with a restore stays on");
    assert_eq!(tick(&p, &fake, (MON_3.0 + 3600, 4, 0, 0)), 1);
    let calls = fake.calls();
    assert_eq!(calls[0].2, r#"{"enabled":true}"#);
    assert_eq!(calls[1].2, r#"{"enabled":false}"#);
    let j = job(&read(&p), 1);
    assert_eq!((j["enabled"].clone(), j["last_restore_status"].clone()), (json!(false), json!(200)));
    assert_eq!(tick(&p, &fake, (MON_3.0 + 86400 + 3600, 4, 0, 1)), 0);
    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn routes_are_served_with_writes_on_and_the_reboot_carries_the_source() {
    let root = dir("routes");
    let d: Shared = Arc::new(Mutex::new(Dev::default()));
    let url = control_server(d.clone()).await;
    let src = Sources {
        control: Arc::new(control::Client::new(url, None)),
        ..Sources::none()
    };
    let c = Compat::new(root.to_str().unwrap(), src).with_writes(true);
    let addr = start(c).await;

    let (st, b) = call(addr, "GET", "/api/scheduler/jobs", "").await;
    assert_eq!((st, b["data"].clone()), (200, json!([])), "{b}");
    let new = r#"{"name":"重启","schedule":{"type":"recurring","time":"4:05","days":[6]},
        "action":{"method":"POST","path":"/api/device/reboot"}}"#;
    let (st, b) = call(addr, "POST", "/api/scheduler/jobs", new).await;
    assert_eq!((st, b["data"]["id"].clone()), (201, json!(1)), "{b}");
    let bad = r#"{"name":"x","schedule":{"type":"once","at":2000000000},
        "action":{"method":"POST","path":"/api/esim/switch"}}"#;
    let (st, _) = call(addr, "POST", "/api/scheduler/jobs", bad).await;
    assert_eq!(st, 400);
    let (st, b) = call(addr, "PUT", "/api/scheduler/jobs/toggle", r#"{"id":1,"enabled":false}"#).await;
    assert_eq!((st, b["data"]["enabled"].clone()), (200, json!(false)), "{b}");
    // the agent's file shape: it can take the jobs back
    let v = read(&jobs_at(&root));
    assert_eq!(v.len(), 1);
    assert_eq!(v[0]["schedule"]["time"], "4:05");
    let (st, _) = call(addr, "DELETE", "/api/scheduler/jobs", r#"{"id":1}"#).await;
    assert_eq!(st, 200);
    let (st, _) = call(addr, "DELETE", "/api/scheduler/jobs", r#"{"id":1}"#).await;
    assert_eq!(st, 404);

    // a scheduled reboot: device.reboot as the scheduler
    let (st, b) =
        call_with(addr, "POST", "/api/device/reboot", "X-U60-Source: scheduler\r\n", "").await;
    assert_eq!(st, 200, "{b}");
    let log = d.lock().unwrap().log.clone();
    let r = log.iter().find(|v| v["action"] == "device.reboot").expect("reboot sent");
    assert_eq!(r["source"], "scheduler");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn local_time_is_in_range() {
    let (ts, hh, mm, dow) = now_local();
    assert!(ts > 1_700_000_000 && hh < 24 && mm < 60 && dow < 7);
}
