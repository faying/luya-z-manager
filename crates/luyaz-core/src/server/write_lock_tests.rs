//! 跨进程写入互斥（CEO review 10-12～14 D7/D8）经真实的 `/control` 处理（`write::run_control_from`）：
//! 锁被占、打不开、调用方验不了时设备写入恰为 0、执行者照常做别的任务；释放后新的请求能做完；
//! 能验证的持锁者（进程号对得上，现在是哨兵）重入照做。
//! 单线程运行时：锁路径用 `write_lock::TEST` / `wifi_lock::TEST`（线程内），不影响别的测试。

use super::*;
use crate::ops::{wifi_lock, write_lock};
use fs2::FileExt;
use std::{fs::File, io::Write, net::SocketAddr};

fn dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("u60d-xlock-{:016x}", rand::random::<u64>()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 另一个写者（应急直写脚本、哨兵）拿着锁，锁文件里写它的进程号。
fn hold(path: &std::path::Path, pid: u32) -> File {
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .unwrap();
    f.try_lock_exclusive().unwrap();
    f.set_len(0).unwrap();
    writeln!(f, "{pid}").unwrap();
    f
}

fn locks(write: Option<&std::path::Path>, wifi: Option<&std::path::Path>, wait_ms: u64) {
    let w = write.map(|p| p.to_string_lossy().into_owned());
    write_lock::TEST.with(|t| *t.borrow_mut() = Some((w, Duration::from_millis(wait_ms))));
    let cfg = wifi_lock::Cfg {
        path: wifi.map(|p| p.to_string_lossy().into_owned()),
        wait: Duration::from_millis(wait_ms),
        poll: Duration::from_millis(50),
        tries: 1,
    };
    wifi_lock::TEST.with(|t| *t.borrow_mut() = Some(cfg));
}

async fn setup() -> (App, crate::ubus::mock_ubusd::MockUbusd) {
    let (app, m) = super::device_capabilities_tests::app().await;
    m.add_method(
        "zwrt_wms",
        0x21,
        "zwrt_wms_modify_tag",
        json!({"result":"success"}),
    );
    m.add_method("zwrt_wlan", 0x22, "reload", json!({}));
    (app, m)
}

async fn control(
    app: &App,
    action: &str,
    params: Value,
    peer: Option<SocketAddr>,
) -> (StatusCode, Value) {
    let body = json!({"action": action, "params": params, "source": "web"});
    let r = write::run_control_from(app.clone(), action.into(), body, peer)
        .await
        .expect("queue not full");
    let status = r.status();
    let bytes = axum::body::to_bytes(r.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn mark_read() -> Value {
    json!({"ids": "1", "tag": 0})
}

#[tokio::test(flavor = "current_thread")]
async fn write_lock_held_by_another_writer_writes_nothing_and_collection_goes_on() {
    let (app, m) = setup().await;
    let d = dir();
    let lock = d.join("write.lock");
    let other = hold(&lock, 4_000_000);
    locks(Some(&lock), None, 800);
    let waiting = tokio::spawn({
        let app = app.clone();
        async move { control(&app, "sms.mark_read", mark_read(), None).await }
    });
    // 等锁时执行者不被占：别的执行者任务（采集轮）马上做完
    tokio::time::sleep(Duration::from_millis(150)).await;
    let t = tokio::time::Instant::now();
    assert_eq!(app.inner.exec.task(async { 1 }).await, 1);
    assert!(
        t.elapsed() < Duration::from_millis(300),
        "{:?}",
        t.elapsed()
    );
    let (status, v) = waiting.await.unwrap();
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert_eq!(v["error"]["code"], "busy");
    assert_eq!(m.invokes("zwrt_wms"), 0, "nothing written");
    // 放了以后新的请求照常做完
    drop(other);
    let (status, v) = control(&app, "sms.mark_read", mark_read(), None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(m.invokes("zwrt_wms"), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn write_lock_released_mid_wait_then_written_once() {
    let (app, m) = setup().await;
    let d = dir();
    let lock = d.join("write.lock");
    let other = hold(&lock, 4_000_000);
    locks(Some(&lock), None, 5_000);
    let t = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(400));
        drop(other);
    });
    let (status, v) = control(&app, "sms.mark_read", mark_read(), None).await;
    t.join().unwrap();
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(m.invokes("zwrt_wms"), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn unopenable_write_lock_writes_nothing() {
    let (app, m) = setup().await;
    let lock = dir().join("no-such-dir").join("write.lock");
    locks(Some(&lock), None, 5_000);
    let t = tokio::time::Instant::now();
    let (status, v) = control(&app, "sms.mark_read", mark_read(), None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert_eq!(v["error"]["code"], "lock_error");
    assert_eq!(m.invokes("zwrt_wms"), 0);
    // 不等、不重试
    assert!(t.elapsed() < Duration::from_secs(1));
}

/// D8：Wi-Fi 锁被占，调用方是局域网、内部、或本机但查不到进程：都不写。
#[tokio::test(flavor = "current_thread")]
async fn wifi_lock_held_and_caller_unverified_writes_nothing() {
    let (app, m) = setup().await;
    let d = dir();
    let (wl, fl) = (d.join("write.lock"), d.join("wifi.lock"));
    let other = hold(&fl, 4_000_000);
    locks(Some(&wl), Some(&fl), 300);
    // 本机地址，但这个端口没有连接：查不到进程（Unknown）
    let lo = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let unknown: SocketAddr = lo.local_addr().unwrap();
    drop(lo);
    let peers = [
        Some("192.168.0.20:40000".parse().unwrap()),
        None,
        Some(unknown),
    ];
    for peer in peers {
        let (status, v) = control(&app, "wifi.reload", json!({}), peer).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{peer:?} {v}");
        assert_eq!(v["error"]["code"], "busy", "{peer:?}");
    }
    assert_eq!(m.invokes("zwrt_wlan"), 0);
    drop(other);
    let (status, v) = control(&app, "wifi.reload", json!({}), None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(m.invokes("zwrt_wlan"), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn unopenable_wifi_lock_writes_nothing() {
    let (app, m) = setup().await;
    let d = dir();
    let fl = d.join("no-such-dir").join("wifi.lock");
    locks(Some(&d.join("write.lock")), Some(&fl), 5_000);
    let (status, v) = control(&app, "wifi.reload", json!({}), None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert_eq!(v["error"]["code"], "lock_error");
    assert_eq!(m.invokes("zwrt_wlan"), 0);
}

/// 写锁被占时 Wi-Fi 动作也不写（两把都要真拿到）。
#[tokio::test(flavor = "current_thread")]
async fn wifi_write_without_the_write_lock_writes_nothing() {
    let (app, m) = setup().await;
    let d = dir();
    let (wl, fl) = (d.join("write.lock"), d.join("wifi.lock"));
    let other = hold(&wl, 4_000_000);
    locks(Some(&wl), Some(&fl), 300);
    let (status, v) = control(&app, "wifi.reload", json!({}), None).await;
    drop(other);
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert_eq!(m.invokes("zwrt_wlan"), 0);
}

/// 能验证的重入：持锁者（锁文件里的进程号）就是发请求的本机进程（哨兵拿着 Wi-Fi 锁经核心写）。
#[tokio::test(flavor = "current_thread")]
async fn verified_wifi_lock_holder_re_enters() {
    let (app, m) = setup().await;
    let d = dir();
    let (wl, fl) = (d.join("write.lock"), d.join("wifi.lock"));
    // 另一个进程连上来（peer_pid 只认别的进程）
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut child = std::process::Command::new("bash")
        .args([
            "-c",
            &format!("exec 3<>/dev/tcp/127.0.0.1/{port}; sleep 20"),
        ])
        .spawn()
        .unwrap();
    let (_conn, peer) = listener.accept().unwrap();
    let other = hold(&fl, child.id());
    locks(Some(&wl), Some(&fl), 300);
    let (status, v) = control(&app, "wifi.reload", json!({}), Some(peer)).await;
    drop(other);
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(m.invokes("zwrt_wlan"), 1);
}

/// 核心自己拿着 Wi-Fi 锁（情景引擎）、经回环调自己的 `/control`：调用方是核心自己，算拿着，照做。
#[tokio::test(flavor = "current_thread")]
async fn in_process_holder_over_loopback_re_enters() {
    let (app, m) = setup().await;
    let d = dir();
    let (wl, fl) = (d.join("write.lock"), d.join("wifi.lock"));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (_conn, peer) = listener.accept().unwrap();
    let other = hold(&fl, std::process::id());
    locks(Some(&wl), Some(&fl), 300);
    let (status, v) = control(&app, "wifi.reload", json!({}), Some(peer)).await;
    drop((other, client));
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(m.invokes("zwrt_wlan"), 1);
}
