use super::*;
#[test]
fn start_does_not_wait_for_server_discovery() {
    let srv = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}/servers", srv.server_addr());
    let worker = std::thread::spawn(move || {
        let rq = srv.recv().unwrap();
        std::thread::sleep(Duration::from_millis(1800));
        let _ = rq.respond(tiny_http::Response::from_string("[]"));
    });
    let st = SpeedTest::new(&url);
    let t = Instant::now();
    let reply = st.start(b"{}", &claim_running);
    let elapsed = t.elapsed();
    worker.join().unwrap();
    st.stop();
    assert!(
        elapsed < Duration::from_millis(500),
        "start blocked for {elapsed:?}: {reply:?}"
    );
    assert_eq!(reply.0, 200);
}
/// `left` bytes at about `bps` bytes per second (sleeps only when ahead).
struct Slow {
    left: u64,
    bps: u64,
    sent: u64,
    start: Option<Instant>,
}

impl std::io::Read for Slow {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.left == 0 {
            return Ok(0);
        }
        let start = *self.start.get_or_insert_with(Instant::now);
        let due = Duration::from_secs_f64(self.sent as f64 / self.bps as f64);
        if let Some(ahead) = due.checked_sub(start.elapsed()) {
            std::thread::sleep(ahead);
        }
        let n = buf.len().min(self.left as usize).min(64 * 1024);
        buf[..n].fill(0x5a);
        self.left -= n as u64;
        self.sent += n as u64;
        Ok(n)
    }
}

/// latency.txt, random4000x4000.jpg (8 MB at ~20 MB/s), upload.php.
fn server() -> TestServer {
    let srv = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let port = srv.server_addr().to_ip().unwrap().port();
    std::thread::spawn(move || {
        for mut rq in srv.incoming_requests() {
            std::thread::spawn(move || {
                let url = rq.url().to_string();
                if url
                    .split('?')
                    .next()
                    .unwrap()
                    .ends_with("random4000x4000.jpg")
                {
                    let len = 8_000_000;
                    let body = Slow {
                        left: len,
                        bps: 20_000_000,
                        sent: 0,
                        start: None,
                    };
                    let _ = rq.respond(tiny_http::Response::new(
                        tiny_http::StatusCode(200),
                        vec![],
                        body,
                        Some(len as usize),
                        None,
                    ));
                } else {
                    let mut sink = Vec::new();
                    let _ = rq.as_reader().read_to_end(&mut sink);
                    let _ = rq.respond(tiny_http::Response::from_string("size=1"));
                }
            });
        }
    });
    let base = format!("http://127.0.0.1:{port}/");
    TestServer {
        id: 1,
        name: "local".into(),
        sponsor: "test".into(),
        country: "".into(),
        host: format!("127.0.0.1:{port}"),
        url: format!("{base}upload.php"),
        provider: Provider::Ookla,
        ping_url: format!("{base}latency.txt"),
        download_url: format!("{base}random4000x4000.jpg"),
        base_url: base,
    }
}

fn run(lim: Limits) -> (SpeedTestProgress, Duration) {
    let st = SpeedTest::new("http://127.0.0.1:1/");
    let t = Instant::now();
    let result = runner::run(
        &server(),
        &st.progress,
        &st.cancel,
        lim,
        &mut runner::Budget::new(lim),
    );
    assert!(result.is_ok());
    let mut p = st.progress.lock().unwrap().clone();
    p.phase = Phase::Complete;
    p.progress = 100;
    (p, t.elapsed())
}

/// The page's test is unchanged: ping, 15 s of download, then upload.
#[test]
fn full_run_does_all_three_phases() {
    let (p, took) = run(FULL);
    assert!(
        p.phase == Phase::Complete && p.progress == 100,
        "{:?}",
        p.error
    );
    assert!(p.ping_ms.is_some() && p.upload_mbps.is_some() && p.download_mbps.is_some());
    assert_eq!(p.upload_bytes, (UPLOAD_SIZE * UPLOAD_ROUNDS) as u64);
    assert!(took >= DOWNLOAD_DURATION, "{took:?}");
    // ~20 MB/s for 15 s: well past the capped run's 30 MB
    assert!(p.download_bytes > 60_000_000, "{}", p.download_bytes);
    assert_eq!(
        FULL,
        Limits {
            ping: true,
            dl_time: Duration::from_secs(15),
            dl_max_bytes: None,
            upload: true
        }
    );
}

/// "Add speed test": download only, stops at 30 MB (here ~1.5 s) or 5 s.
#[test]
fn capped_run_stops_early_and_skips_ping_and_upload() {
    let (p, took) = run(CAPPED);
    assert!(
        p.phase == Phase::Complete && p.progress == 100,
        "{:?}",
        p.error
    );
    assert!(p.ping_ms.is_none() && p.upload_mbps.is_none() && p.upload_bytes == 0);
    assert!(
        p.download_bytes >= 30_000_000 && p.download_bytes < 30_000_000 + BUF_SIZE as u64,
        "{}",
        p.download_bytes
    );
    assert!(took < Duration::from_secs(6), "{took:?}");
    assert!(p.download_mbps().is_some_and(|m| m > 10.0));
    assert_eq!(p.failure(), None);
}

#[test]
fn second_start_is_refused_and_leaves_progress_alone() {
    let st = SpeedTest::new("http://127.0.0.1:1/");
    st.running.store(true, Ordering::SeqCst);
    st.progress.lock().unwrap().download_bytes = 1234;
    let (code, body) = st.start(b"{}", &claim_running);
    assert_eq!(
        (code, body["error"].as_str()),
        (409, Some("test already running"))
    );
    assert_eq!(st.progress.lock().unwrap().download_bytes, 1234);
}

fn wait(st: &SpeedTest) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    while st.running.load(Ordering::SeqCst) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!st.running.load(Ordering::SeqCst), "worker did not finish");
    st.progress().1["data"].clone()
}
fn configure_local(st: &SpeedTest, base: &str) -> Value {
    let cfg = json!({"librespeed_servers":[{"id":1,"name":"Local fallback","server":base,"dlURL":"random4000x4000.jpg","ulURL":"upload.php","pingURL":"latency.txt"}]});
    assert_eq!(st.configure(&serde_json::to_vec(&cfg).unwrap()).0, 200);
    cfg
}
fn short() -> Limits {
    Limits {
        dl_time: Duration::from_millis(100),
        ..FULL
    }
}

#[test]
fn blocked_ookla_falls_back_and_manual_ookla_does_not() {
    let st = SpeedTest::new("http://127.0.0.1:1/blocked");
    configure_local(&st, &server().base_url);
    assert_eq!(st.begin(None, None, short(), None, &claim_running).0, 200);
    let p = wait(&st);
    assert_eq!(p["phase"], "complete", "{p}");
    assert_eq!(p["provider"], "librespeed");
    assert_eq!(p["fallback_reason"], "provider_unreachable");
    assert!(p["server"].as_str().unwrap().starts_with("LibreSpeed"));
    assert!(p["download_bytes"].as_u64().unwrap() > 0);
    assert_eq!(p["upload_bytes"], 10_000_000);
    assert_eq!(st.start(br#"{"provider":"ookla"}"#, &claim_running).0, 200);
    let p = wait(&st);
    assert_eq!(p["phase"], "error");
    assert_eq!(p["provider"], "ookla");
    assert_eq!(p["error_code"], "provider_unreachable");
}

#[test]
fn manual_librespeed_bypasses_ookla_discovery_and_provider_ids_do_not_collide() {
    // This fake list would hang if it were ever contacted.
    let list = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let st = SpeedTest::new(&format!("http://{}/servers", list.server_addr()));
    configure_local(&st, &server().base_url);
    assert_eq!(
        st.begin(
            Some(Provider::Librespeed),
            Some(1),
            short(),
            None,
            &claim_running
        )
        .0,
        200
    );
    assert_eq!(wait(&st)["phase"], "complete");
    assert!(list.try_recv().unwrap().is_none());
    assert_eq!(
        st.start(
            br#"{"provider":"librespeed","server_id":999}"#,
            &claim_running
        )
        .0,
        200
    );
    assert_eq!(wait(&st)["error_code"], "no_servers");
}

#[test]
fn no_fallback_and_unconfigured_provider_report_service_errors_and_release_slot() {
    let st = SpeedTest::new("http://127.0.0.1:1/blocked");
    for (body, code) in [
        (b"{}".as_slice(), "provider_unreachable"),
        (
            br#"{"provider":"librespeed"}"#.as_slice(),
            "provider_not_configured",
        ),
    ] {
        assert_eq!(st.start(body, &claim_running).0, 200);
        let p = wait(&st);
        assert_eq!(p["phase"], "error");
        assert_eq!(p["error_code"], code);
        assert!(p["error_en"]
            .as_str()
            .unwrap()
            .contains(if code == "provider_unreachable" {
                "Ookla"
            } else {
                "LibreSpeed"
            }));
    }
}

#[test]
fn discovery_is_cache_only_single_flight_and_stop_cancels_before_transfers() {
    let srv = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let st = SpeedTest::new(&format!("http://{}/servers", srv.server_addr()));
    let worker = std::thread::spawn(move || {
        let rq = srv.recv().unwrap();
        std::thread::sleep(Duration::from_millis(700));
        let _ = rq.respond(tiny_http::Response::from_string("[]"));
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            srv.try_recv().unwrap().is_none(),
            "duplicate discovery request"
        );
    });
    let start = Instant::now();
    assert_eq!(st.servers().0, 200);
    assert!(start.elapsed() < Duration::from_millis(500));
    assert_eq!(st.start(b"{}", &claim_running).0, 200);
    for _ in 0..10 {
        let _ = st.servers();
    }
    assert_eq!(st.progress().1["data"]["phase"], "discovering");
    assert_eq!(st.stop().1["data"]["status"], "stopping");
    assert_eq!(wait(&st)["phase"], "cancelled");
    worker.join().unwrap();
    assert_eq!(st.servers().0, 503);
    assert_eq!(st.servers().1["error_code"], "no_servers");
}

#[test]
fn local_configuration_is_validated_persisted_and_recovered() {
    let dir = std::env::temp_dir().join(format!("luyaz-speed-config-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("speedtest.json");
    let st = SpeedTest::new("http://127.0.0.1:1/").with_config_path(&path);
    let cfg = configure_local(&st, "https://example.com/test/");
    let restored = SpeedTest::new("http://127.0.0.1:1/").with_config_path(&path);
    assert_eq!(restored.config().1["data"]["config"], cfg);
    for field in ["server", "dlURL", "ulURL", "pingURL"] {
        let mut bad = cfg.clone();
        bad["librespeed_servers"][0][field] = json!("file:///etc/passwd");
        assert_eq!(st.configure(&serde_json::to_vec(&bad).unwrap()).0, 400);
    }
    let mut bad = cfg.clone();
    bad["librespeed_servers"][0]["server"] = json!("https://user:password@example.com/");
    assert_eq!(st.configure(&serde_json::to_vec(&bad).unwrap()).0, 400);
    assert_eq!(st.config().1["data"]["config"], cfg);
    std::fs::write(&path, "bad JSON").unwrap();
    let st = SpeedTest::new("http://127.0.0.1:1/").with_config_path(&path);
    assert!(st.config().1["data"]["load_error"].is_string());
    configure_local(&st, "https://example.com/test/");
    assert!(st.config().1["data"]["load_error"].is_null());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn transfer_failures_are_not_successful_zero_speed_and_share_the_byte_cap() {
    let srv = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let base = format!("http://{}/", srv.server_addr());
    std::thread::spawn(move || {
        for rq in srv.incoming_requests() {
            let _ = rq.respond(tiny_http::Response::empty(503));
        }
    });
    let st = SpeedTest::new("http://127.0.0.1:1/");
    configure_local(&st, &base);
    assert_eq!(
        st.begin(
            Some(Provider::Librespeed),
            None,
            Limits {
                ping: false,
                ..short()
            },
            None,
            &claim_running
        )
        .0,
        200
    );
    let p = wait(&st);
    assert_eq!(p["phase"], "error");
    assert!(p["download_mbps"].is_null());
    let progress = Arc::new(Mutex::new(SpeedTestProgress::idle()));
    let lim = Limits {
        dl_max_bytes: Some(1000),
        ..CAPPED
    };
    // A deliberately tiny shared budget is exhausted exactly, including across attempts.
    let mut budget = runner::Budget::new(lim);
    runner::run(
        &server(),
        &progress,
        &AtomicBool::new(false),
        lim,
        &mut budget,
    )
    .unwrap_or_else(|_| panic!("local download failed"));
    assert_eq!(progress.lock().unwrap().download_bytes, 1000);
    assert!(runner::run(
        &server(),
        &progress,
        &AtomicBool::new(false),
        lim,
        &mut budget
    )
    .is_err());
    assert_eq!(progress.lock().unwrap().download_bytes, 1000);
}

#[test]
fn slow_upload_is_allowed_and_time_budget_is_not_reset_between_attempts() {
    let srv = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let base = format!("http://{}/", srv.server_addr());
    let worker = std::thread::spawn(move || {
        for i in 0..UPLOAD_ROUNDS {
            let mut rq = srv.recv().unwrap();
            let mut sink = Vec::new();
            rq.as_reader().read_to_end(&mut sink).unwrap();
            // A valid slow first upload must not be mistaken for a blocked provider.
            if i == 0 {
                std::thread::sleep(Duration::from_millis(1200));
            }
            let _ = rq.respond(tiny_http::Response::from_string("ok"));
        }
    });
    let target = server();
    let st = SpeedTest::new("http://127.0.0.1:1/");
    let lim = Limits {
        ping: false,
        dl_time: Duration::from_millis(50),
        ..FULL
    };
    let mut budget = runner::Budget::new(lim);
    let mut target = target;
    target.url = format!("{base}upload.php");
    assert!(runner::run(&target, &st.progress, &st.cancel, lim, &mut budget).is_ok());
    worker.join().unwrap();
    assert_eq!(st.progress.lock().unwrap().upload_bytes, 10_000_000);
    let before = budget.remaining();
    std::thread::sleep(Duration::from_millis(20));
    assert!(budget.remaining() < before);
}

#[test]
fn reachable_directory_with_unreachable_ookla_node_uses_fallback() {
    let directory = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}/servers", directory.server_addr());
    let worker = std::thread::spawn(move || {
        let rq = directory.recv().unwrap();
        let _ = rq.respond(tiny_http::Response::from_string(r#"[{"id":1,"name":"Blocked node","sponsor":"Ookla","url":"http://127.0.0.1:1/upload.php"}]"#));
    });
    let st = SpeedTest::new(&url);
    configure_local(&st, &server().base_url);
    assert_eq!(st.begin(None, None, short(), None, &claim_running).0, 200);
    let p = wait(&st);
    worker.join().unwrap();
    assert_eq!(p["phase"], "complete", "{p}");
    assert_eq!(p["provider"], "librespeed");
    assert_eq!(p["fallback_reason"], "provider_unreachable");
}

#[test]
fn explicit_list_retry_bypasses_negative_cache_without_blocking() {
    let directory = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let st = SpeedTest::new(&format!("http://{}/servers", directory.server_addr()));
    let worker = std::thread::spawn(move || {
        let rq = directory.recv().unwrap();
        let _ = rq.respond(tiny_http::Response::empty(503));
        let rq = directory.recv().unwrap();
        let _ = rq.respond(tiny_http::Response::from_string(
            r#"[{"id":9,"name":"Recovered","url":"http://127.0.0.1:1/upload.php"}]"#,
        ));
    });
    let _ = st.servers();
    for _ in 0..100 {
        if !st.servers_cache.lock().unwrap().refreshing {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(st.servers().0, 503);
    assert_eq!(st.refresh_servers().0, 200);
    for _ in 0..100 {
        if !st.servers_cache.lock().unwrap().refreshing {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(st.servers().1["data"][0]["id"], 9);
    worker.join().unwrap();
}
