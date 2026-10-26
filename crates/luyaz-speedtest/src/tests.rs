use super::*;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::AtomicUsize;

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
/// One rate for all connections together: each 16 KiB chunk waits for its slot.
/// `rate(t)` is bytes per second `t` seconds after the first chunk.
struct Limiter {
    next: Mutex<Option<(Instant, Instant)>>, // (first chunk, next free slot)
    rate: fn(f64) -> f64,
}
struct Limited {
    lim: Arc<Limiter>,
    left: u64,
}
impl std::io::Read for Limited {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = buf.len().min(self.left as usize).min(16 * 1024);
        if n == 0 {
            return Ok(0);
        }
        let slot = {
            let mut g = self.lim.next.lock().unwrap();
            let now = Instant::now();
            let (first, next) = *g.get_or_insert((now, now));
            let rate = (self.lim.rate)(now.duration_since(first).as_secs_f64());
            let slot = next.max(now) + Duration::from_secs_f64(n as f64 / rate);
            *g = Some((first, slot));
            slot
        };
        std::thread::sleep(slot.saturating_duration_since(Instant::now()));
        buf[..n].fill(0x5a);
        self.left -= n as u64;
        Ok(n)
    }
}
fn node(base: String, name: &str) -> TestServer {
    TestServer {
        id: 1,
        name: name.into(),
        sponsor: "test".into(),
        country: "".into(),
        host: base
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .into(),
        url: format!("{base}upload.php"),
        provider: Provider::Ookla,
        ping_url: format!("{base}latency.txt"),
        download_url: format!("{base}random4000x4000.jpg"),
        base_url: base,
    }
}
#[derive(Clone, Copy)]
enum Body {
    /// 8 MB per response at ~20 MB/s per connection
    Fast,
    /// 1 MB/s per connection
    Slow,
    /// headers, then nothing
    Never,
}
/// latency.txt, random4000x4000.jpg, upload.php (any body).
fn serve(body: Body, limiter: Option<Arc<Limiter>>) -> String {
    let srv = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let port = srv.server_addr().to_ip().unwrap().port();
    std::thread::spawn(move || {
        for mut rq in srv.incoming_requests() {
            let limiter = limiter.clone();
            std::thread::spawn(move || {
                let url = rq.url().to_string();
                if url
                    .split('?')
                    .next()
                    .unwrap()
                    .ends_with("random4000x4000.jpg")
                {
                    let len = 8_000_000u64;
                    let reader: Box<dyn Read + Send> = match (limiter, body) {
                        (Some(lim), _) => Box::new(Limited {
                            lim,
                            left: 200_000_000,
                        }),
                        (None, Body::Never) => {
                            std::thread::sleep(Duration::from_secs(3));
                            Box::new(std::io::empty())
                        }
                        (None, b) => Box::new(Slow {
                            left: len,
                            bps: if matches!(b, Body::Slow) {
                                1_000_000
                            } else {
                                20_000_000
                            },
                            sent: 0,
                            start: None,
                        }),
                    };
                    let _ = rq.respond(tiny_http::Response::new(
                        tiny_http::StatusCode(200),
                        vec![],
                        reader,
                        None,
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
    format!("http://127.0.0.1:{port}/")
}
fn server() -> TestServer {
    node(serve(Body::Fast, None), "local")
}
/// Every stage, shrunk so a run takes about a second.
fn quick() -> Limits {
    Limits {
        idle_time: Duration::from_millis(100),
        single_time: Duration::from_millis(300),
        multi_time: Duration::from_millis(400),
        upload_time: Duration::from_millis(300),
        ramp: Duration::from_millis(100),
        min_steady: Duration::from_millis(100),
        probe_every: Duration::from_millis(20),
        min_probes: 3,
        trial_time: Duration::from_millis(150),
        trial_zero: Duration::from_millis(300),
        ..FULL
    }
}
fn job(lim: Limits) -> Job {
    Job {
        mode: None,
        id: None,
        lim,
        chill: chill::Plan::Off,
        ctx: RunContext::default(),
    }
}
fn run_on(
    st: &SpeedTest,
    server: &TestServer,
    lim: Limits,
    budget: &runner::Budget,
) -> Result<runner::Outcome, Failure> {
    let j = job(lim);
    runner::run(server, &st.env(&j, budget))
}
fn run(lim: Limits) -> (SpeedTestProgress, Duration) {
    let st = SpeedTest::new("http://127.0.0.1:1/");
    let t = Instant::now();
    let result = run_on(&st, &server(), lim, &runner::Budget::new(lim, false));
    assert!(result.is_ok());
    let mut p = st.progress.lock().unwrap().clone();
    p.phase = Phase::Complete;
    p.progress = 100;
    (p, t.elapsed())
}

/// Every stage in order: idle latency, single, multi (with the probe), upload.
#[test]
fn full_run_does_every_stage() {
    let lim = quick();
    let (p, took) = run(lim);
    assert!(
        p.phase == Phase::Complete && p.progress == 100,
        "{:?}",
        p.error
    );
    assert!(p.ping_ms.is_some() && p.upload_mbps.is_some() && p.download_mbps.is_some());
    assert!(p.single_mbps.is_some() && p.download_whole_mbps.is_some());
    assert!(p.loaded_down_ms.is_some() && p.loaded_up_ms.is_some());
    assert_eq!(p.idle_ms, p.ping_ms);
    assert!(p.upload_bytes > 0);
    assert!(
        took >= lim.idle_time + lim.single_time + lim.multi_time,
        "{took:?}"
    );
    // (the loopback upload meets its 150 MB cap at once, so it is not timed here)
    // 6 connections at ~20 MB/s each for 0.4 s, plus the single one
    assert!(p.download_bytes > 20_000_000, "{}", p.download_bytes);
    assert!(
        p.download_mbps.unwrap() > p.single_mbps.unwrap() * 2.0,
        "{:?} {:?}",
        p.download_mbps,
        p.single_mbps
    );
    // the device's run, as the design fixes it
    assert_eq!(
        (
            FULL.idle_time,
            FULL.single_time,
            FULL.multi_time,
            FULL.upload_time
        ),
        (
            Duration::from_secs(2),
            Duration::from_secs(8),
            Duration::from_secs(10),
            Duration::from_secs(10)
        )
    );
    assert_eq!(
        (FULL.connections, FULL.ramp, FULL.min_steady),
        (6, Duration::from_secs(2), Duration::from_secs(3))
    );
    assert_eq!(
        (
            FULL.single_max,
            FULL.multi_max,
            FULL.upload_max,
            FULL.chill_max,
            FULL.total_max,
            FULL.upload_chunk
        ),
        (200 * MB, 400 * MB, 150 * MB, 400 * MB, 750 * MB, 4_000_000)
    );
    assert_eq!(
        (FULL.probe_every, FULL.probe_cap, FULL.min_probes),
        (Duration::from_millis(200), Duration::from_secs(2), 20)
    );
}

/// "Add speed test": multi-connection download only, stops at 30 MB (here well under 1 s) or 5 s.
#[test]
fn capped_run_stops_early_and_skips_ping_and_upload() {
    let (p, took) = run(CAPPED);
    assert!(
        p.phase == Phase::Complete && p.progress == 100,
        "{:?}",
        p.error
    );
    assert!(p.ping_ms.is_none() && p.upload_mbps.is_none() && p.upload_bytes == 0);
    assert!(p.single_mbps.is_none() && p.loaded_down_ms.is_none());
    // reserved before read: the cap is exact across six threads
    assert_eq!(p.download_bytes, 30_000_000);
    assert!(took < Duration::from_secs(6), "{took:?}");
    assert!(p.download_mbps().is_some_and(|m| m > 10.0));
    assert_eq!(p.failure(), None);
    // ~120 MB/s: 30 MB is over long before a 1 s stable part (R3)
    assert!(p.sample_short);
    assert_eq!(
        (
            CAPPED.multi_time,
            CAPPED.ramp,
            CAPPED.min_steady,
            CAPPED.total_max
        ),
        (
            Duration::from_secs(5),
            Duration::from_millis(500),
            Duration::from_secs(1),
            30 * MB
        )
    );
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

#[test]
fn blocked_ookla_falls_back_and_manual_ookla_does_not() {
    let st = SpeedTest::new("http://127.0.0.1:1/blocked");
    configure_local(&st, &server().base_url);
    assert_eq!(
        st.begin(None, None, quick(), false, None, &claim_running).0,
        200
    );
    let p = wait(&st);
    assert_eq!(p["phase"], "complete", "{p}");
    assert_eq!(p["provider"], "librespeed");
    assert_eq!(p["fallback_reason"], "provider_unreachable");
    assert!(p["server"].as_str().unwrap().starts_with("LibreSpeed"));
    assert!(p["download_bytes"].as_u64().unwrap() > 0);
    assert!(p["upload_bytes"].as_u64().unwrap() > 0);
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
            quick(),
            false,
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
    assert_eq!(st.start(br#"{"path":"nowhere"}"#, &claim_running).0, 400);
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
    assert_eq!(st.progress().1["data"]["step"], 0);
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
                ..quick()
            },
            false,
            None,
            &claim_running
        )
        .0,
        200
    );
    let p = wait(&st);
    assert_eq!(p["phase"], "error");
    assert!(p["download_mbps"].is_null());
    assert!(p["verdict"].is_null());
    let lim = Limits {
        multi_max: 1000,
        total_max: 1000,
        ..CAPPED
    };
    // A deliberately tiny shared budget is exhausted exactly, including across attempts.
    let st = SpeedTest::new("http://127.0.0.1:1/");
    let budget = runner::Budget::new(lim, false);
    assert!(run_on(&st, &server(), lim, &budget).is_ok());
    assert_eq!(st.progress.lock().unwrap().download_bytes, 1000);
    assert!(run_on(&st, &server(), lim, &budget).is_err());
    assert_eq!(st.progress.lock().unwrap().download_bytes, 1000);
}

#[test]
fn slow_upload_is_allowed_and_time_budget_is_not_reset_between_attempts() {
    let srv = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let base = format!("http://{}/", srv.server_addr());
    let first = Arc::new(AtomicBool::new(true));
    std::thread::spawn(move || {
        for mut rq in srv.incoming_requests() {
            let first = first.clone();
            std::thread::spawn(move || {
                let mut sink = Vec::new();
                let _ = rq.as_reader().read_to_end(&mut sink);
                // A valid slow first upload must not be mistaken for a blocked provider.
                if first.swap(false, Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(1200));
                }
                let _ = rq.respond(tiny_http::Response::from_string("ok"));
            });
        }
    });
    let lim = Limits {
        ping: false,
        single: false,
        multi_time: Duration::from_millis(50),
        upload_time: Duration::from_secs(2),
        ..quick()
    };
    let mut target = server();
    target.url = format!("{base}upload.php");
    let st = SpeedTest::new("http://127.0.0.1:1/");
    let budget = runner::Budget::new(lim, false);
    assert!(run_on(&st, &target, lim, &budget).is_ok());
    let p = st.progress.lock().unwrap().clone();
    assert!(
        p.upload_mbps.is_some_and(|m| m > 0.0),
        "{:?}",
        p.upload_mbps
    );
    assert!(p.upload_bytes > 0);
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
    assert_eq!(
        st.begin(None, None, quick(), false, None, &claim_running).0,
        200
    );
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

// ── v2 ────────────────────────────────────────────────────────────────────────────────────

fn multi_only(time: Duration, ramp: Duration) -> Limits {
    Limits {
        ping: false,
        single: false,
        upload: false,
        multi_time: time,
        ramp,
        min_steady: Duration::from_millis(500),
        ..FULL
    }
}
fn limited(rate: fn(f64) -> f64) -> TestServer {
    let lim = Arc::new(Limiter {
        next: Mutex::new(None),
        rate,
    });
    node(serve(Body::Fast, Some(lim)), "limited")
}

/// Six connections against a 32 Mbit/s link: the stable rate is within 5%.
#[test]
fn multi_connection_rate_is_within_5_percent_of_the_link() {
    let lim = multi_only(Duration::from_millis(2500), Duration::from_millis(500));
    let st = SpeedTest::new("http://127.0.0.1:1/");
    let out = run_on(
        &st,
        &limited(|_| 4_000_000.0),
        lim,
        &runner::Budget::new(lim, false),
    );
    assert!(out.is_ok());
    let p = st.progress.lock().unwrap().clone();
    let m = p.download_mbps.unwrap();
    assert!((m - 32.0).abs() / 32.0 < 0.05, "{m} Mbps");
    assert!(!p.sample_short);
}

/// A slow first 0.6 s is not counted in the stable rate, only in the whole mean.
#[test]
fn ramp_is_discarded_from_the_stable_rate() {
    let lim = multi_only(Duration::from_millis(2500), Duration::from_millis(800));
    let st = SpeedTest::new("http://127.0.0.1:1/");
    let server = limited(|t| if t < 0.6 { 500_000.0 } else { 4_000_000.0 });
    assert!(run_on(&st, &server, lim, &runner::Budget::new(lim, false)).is_ok());
    let p = st.progress.lock().unwrap().clone();
    let (steady, whole) = (p.download_mbps.unwrap(), p.download_whole_mbps.unwrap());
    assert!((steady - 32.0).abs() / 32.0 < 0.08, "{steady}");
    assert!(whole < steady * 0.9, "{whole} vs {steady}");
}

/// The upload runs on six connections at once, each a loop of POSTs on its own connection.
#[test]
fn upload_is_parallel_and_counts_acknowledged_posts() {
    let srv = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let base = format!("http://{}/", srv.server_addr());
    let (inflight, peak, posts) = (
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    );
    let (i2, p2, n2) = (inflight.clone(), peak.clone(), posts.clone());
    std::thread::spawn(move || {
        for mut rq in srv.incoming_requests() {
            let (inflight, peak, posts) = (i2.clone(), p2.clone(), n2.clone());
            std::thread::spawn(move || {
                let now = inflight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                let mut sink = Vec::new();
                let _ = rq.as_reader().read_to_end(&mut sink);
                std::thread::sleep(Duration::from_millis(60));
                posts.fetch_add(1, Ordering::SeqCst);
                inflight.fetch_sub(1, Ordering::SeqCst);
                let _ = rq.respond(tiny_http::Response::from_string("ok"));
            });
        }
    });
    let lim = Limits {
        ping: false,
        single: false,
        multi_time: Duration::from_millis(100),
        upload_time: Duration::from_secs(1),
        ..quick()
    };
    let mut target = server();
    target.url = format!("{base}upload.php");
    let st = SpeedTest::new("http://127.0.0.1:1/");
    assert!(run_on(&st, &target, lim, &runner::Budget::new(lim, false)).is_ok());
    let p = st.progress.lock().unwrap().clone();
    assert!(
        peak.load(Ordering::SeqCst) >= 4,
        "{}",
        peak.load(Ordering::SeqCst)
    );
    assert!(posts.load(Ordering::SeqCst) >= 12);
    assert!(p.upload_mbps.is_some_and(|m| m > 0.0));
    assert!(p.upload_whole_mbps.is_some());
}

/// A short budget cuts the upload first, then the single connection, the multi download last.
#[test]
fn budget_cuts_upload_then_single_then_multi() {
    let base = Limits {
        ping: false,
        single_max: 2 * MB,
        multi_max: 2 * MB,
        upload_max: 2 * MB,
        ..quick()
    };
    let srv = server();
    let case = |total: u64| {
        let lim = Limits {
            total_max: total,
            ..base
        };
        let st = SpeedTest::new("http://127.0.0.1:1/");
        let out = run_on(&st, &srv, lim, &runner::Budget::new(lim, false));
        let p = st.progress.lock().unwrap().clone();
        assert!(p.download_bytes + p.upload_bytes <= total, "{total}");
        (out, p)
    };
    let (out, p) = case(4_400_000);
    let out = out.ok().unwrap();
    assert!(
        p.single_mbps.is_some() && p.upload_mbps.is_none(),
        "upload cut first"
    );
    assert!(out.inputs(None).single.is_ok());
    let (out, p) = case(2_200_000);
    let out = out.ok().unwrap();
    assert!(p.single_mbps.is_none() && p.upload_mbps.is_none() && p.download_mbps.is_some());
    let v = verdict::judge(&out.inputs(None));
    assert_eq!(v.layers[0].reason, Some("budget"));
    assert_eq!(p.download_bytes, 2 * MB);
    let (out, p) = case(400_000);
    assert_eq!(out.err().map(|e| e.code), Some("budget_exhausted"));
    assert_eq!(p.download_bytes, 0);
}

/// The diagnosis' run: coarse, no verdict, and `on_done` gets the final snapshot.
#[test]
fn coarse_run_has_no_verdict_and_on_done_sees_the_end() {
    let st = SpeedTest::new("http://127.0.0.1:1/blocked");
    configure_local(&st, &server().base_url);
    let (tx, rx) = std::sync::mpsc::channel();
    let running = st.running_flag();
    let done: OnDone = Box::new(move |p: &SpeedTestProgress| {
        let _ = tx.send((
            serde_json::to_value(p).unwrap(),
            running.load(Ordering::SeqCst),
        ));
    });
    assert_eq!(st.start_capped(done, &claim_running).0, 200);
    let (p, still_running) = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(!still_running);
    assert_eq!(p["phase"], "complete", "{p}");
    assert_eq!(p["mode"], "coarse");
    assert_eq!(p["sample_short"], true);
    assert!(p["verdict"].is_null() && p["upload_mbps"].is_null() && p["ping_ms"].is_null());
    assert_eq!(
        (p["step"].as_u64(), p["steps"].as_u64()),
        (Some(5), Some(5))
    );
    assert_eq!(p["method_version"], 2);
    assert_eq!(p["roaming_capped"], false);
    // a roaming device: the full request is run coarse (R9)
    let st = st.with_context(|| RunContext {
        roaming: true,
        ..RunContext::default()
    });
    assert_eq!(st.start(br#"{"path":"chill"}"#, &claim_running).0, 200);
    let p = st.progress().1["data"].clone();
    assert_eq!(
        (p["mode"].as_str(), p["roaming_capped"].as_bool()),
        (Some("coarse"), Some(true))
    );
    assert_eq!(p["path"], "chill");
    let p = wait(&st);
    assert_eq!(p["phase"], "complete", "{p}");
    assert!(p["upload_mbps"].is_null() && p["verdict"].is_null());
}

/// R10: a 0-byte node is dropped, the fastest tried node wins; a coarse run on the same cell
/// starts on it without trying the list again.
#[test]
fn node_trial_drops_dead_nodes_picks_the_fastest_and_coarse_reuses_it() {
    let (never, slow, fast) = (
        serve(Body::Never, None),
        serve(Body::Slow, None),
        serve(Body::Fast, None),
    );
    let list = json!([
        {"id": 1, "name": "Dead", "sponsor": "T", "url": format!("{never}upload.php")},
        {"id": 2, "name": "Slow", "sponsor": "T", "url": format!("{slow}upload.php")},
        {"id": 3, "name": "Fast", "sponsor": "T", "url": format!("{fast}upload.php")},
    ])
    .to_string();
    let directory = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}/servers", directory.server_addr());
    std::thread::spawn(move || {
        for rq in directory.incoming_requests() {
            let _ = rq.respond(tiny_http::Response::from_string(list.clone()));
        }
    });
    let st = SpeedTest::new(&url).with_context(|| RunContext {
        cell: Some("SA:7".into()),
        ..RunContext::default()
    });
    assert_eq!(
        st.begin(None, None, quick(), false, None, &claim_running).0,
        200
    );
    let p = wait(&st);
    assert_eq!(p["phase"], "complete", "{p}");
    assert_eq!(
        (p["server_name"].as_str(), p["server_sponsor"].as_str()),
        (Some("Fast"), Some("T"))
    );
    assert!(p["fallback_reason"].is_null());
    let t = Instant::now();
    assert_eq!(
        st.begin(None, None, CAPPED, false, None, &claim_running).0,
        200
    );
    let p = wait(&st);
    assert_eq!(p["phase"], "complete", "{p}");
    assert_eq!(p["server_name"], "Fast");
    assert_eq!(p["provider"], "ookla");
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
}

/// A CHILL listener look-alike: HTTP CONNECT, then bytes both ways (optionally throttled per
/// connection); `die_after` the first CONNECT it drops every tunnel and stops listening.
fn connect_proxy(rate: Option<u64>, die_after: Option<Duration>) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap().to_string();
    l.set_nonblocking(true).unwrap();
    let dead = Arc::new(AtomicBool::new(false));
    std::thread::spawn(move || {
        let timer = Arc::new(AtomicBool::new(false));
        while !dead.load(Ordering::SeqCst) {
            let Ok((mut c, _)) = l.accept() else {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            };
            c.set_nonblocking(false).unwrap();
            let (dead, timer) = (dead.clone(), timer.clone());
            std::thread::spawn(move || {
                let mut head = Vec::new();
                let mut b = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    match c.read(&mut b) {
                        Ok(1) => head.push(b[0]),
                        _ => return,
                    }
                }
                let head = String::from_utf8_lossy(&head).to_string();
                let Some(target) = head
                    .strip_prefix("CONNECT ")
                    .and_then(|r| r.split(' ').next())
                else {
                    return;
                };
                let Ok(mut up) = TcpStream::connect(target) else {
                    return;
                };
                // the timer starts at the first tunnel (not at a listener check)
                if let Some(d) = die_after.filter(|_| !timer.swap(true, Ordering::SeqCst)) {
                    let dead = dead.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(d);
                        dead.store(true, Ordering::SeqCst);
                    });
                }
                let _ = c.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n");
                let (mut c2, mut up2) = (c.try_clone().unwrap(), up.try_clone().unwrap());
                std::thread::spawn(move || {
                    let _ = std::io::copy(&mut c2, &mut up2);
                });
                let mut buf = vec![0u8; 16 * 1024];
                let start = Instant::now();
                let mut sent = 0u64;
                loop {
                    if dead.load(Ordering::SeqCst) {
                        let _ = c.shutdown(std::net::Shutdown::Both);
                        let _ = up.shutdown(std::net::Shutdown::Both);
                        return;
                    }
                    let n = match up.read(&mut buf) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    if c.write_all(&buf[..n]).is_err() {
                        return;
                    }
                    sent += n as u64;
                    if let Some(r) = rate {
                        let due = Duration::from_secs_f64(sent as f64 / r as f64);
                        std::thread::sleep(due.saturating_sub(start.elapsed()));
                    }
                }
            });
        }
    });
    addr
}
/// mihomo's controller: 🚀 节点选择 is `main`; any other group answers `jp-node-1`.
fn controller(main: &'static str) -> String {
    let srv = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let base = format!("http://{}", srv.server_addr());
    std::thread::spawn(move || {
        for rq in srv.incoming_requests() {
            let now = if rq.url().contains("%F0%9F%9A%80") {
                main
            } else {
                "jp-node-1"
            };
            let _ = rq.respond(tiny_http::Response::from_string(
                json!({ "now": now }).to_string(),
            ));
        }
    });
    base
}
fn chill_run(proxy: String, main: &'static str) -> Value {
    let st = SpeedTest::new("http://127.0.0.1:1/blocked").with_chill(&proxy, &controller(main));
    configure_local(&st, &server().base_url);
    assert_eq!(
        st.begin(
            Some(Provider::Librespeed),
            None,
            quick(),
            true,
            None,
            &claim_running
        )
        .0,
        200
    );
    wait(&st)
}
fn proxy_layer(p: &Value) -> Value {
    p["verdict"]["layers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["key"] == "proxy")
        .unwrap()
        .clone()
}

/// The pair: direct multi, then the same node through 7894; a slow proxy is the bottleneck.
#[test]
fn chill_pair_through_connect_finds_a_slow_proxy() {
    let p = chill_run(connect_proxy(Some(1_000_000), None), "🇯🇵 日本");
    assert_eq!(p["phase"], "complete", "{p}");
    assert_eq!(
        (p["path"].as_str(), p["steps"].as_u64(), p["step"].as_u64()),
        (Some("chill"), Some(6), Some(6))
    );
    let (direct, chill) = (
        p["download_mbps"].as_f64().unwrap(),
        p["chill_mbps"].as_f64().unwrap(),
    );
    assert!(chill < direct * 0.7, "{chill} vs {direct}");
    assert_eq!(p["chill_region"], "日本");
    assert!(
        !p.to_string().contains("jp-node-1"),
        "the node never leaves the engine"
    );
    assert_eq!(proxy_layer(&p)["state"], "warn");
    assert!(p["verdict"]["bottlenecks"]
        .as_array()
        .unwrap()
        .contains(&json!("proxy")));
}

/// CHILL stops midway: the proxy row says so; no 0 Mbps, no silent direct retry.
#[test]
fn chill_stopping_midway_is_reported_not_zero() {
    let p = chill_run(
        connect_proxy(None, Some(Duration::from_millis(150))),
        "DIRECT",
    );
    assert_eq!(p["phase"], "complete", "{p}");
    assert!(p["chill_mbps"].is_null() && p["download_mbps"].as_f64().is_some());
    assert_eq!(p["chill_region"], "直连");
    let l = proxy_layer(&p);
    assert_eq!(
        (l["state"].as_str(), l["reason"].as_str()),
        (Some("untested"), Some("chill_stopped"))
    );
}

/// Nothing on 7894: the run is direct with 5 steps; the proxy row: CHILL not running.
#[test]
fn chill_not_running_is_untested_and_never_started() {
    let addr = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .to_string();
    let p = chill_run(addr, "DIRECT");
    assert_eq!(p["phase"], "complete", "{p}");
    assert_eq!(
        (p["path"].as_str(), p["steps"].as_u64()),
        (Some("chill"), Some(5))
    );
    assert!(p["chill_mbps"].is_null() && p["chill_region"].is_null());
    let l = proxy_layer(&p);
    assert_eq!(l["reason"], "chill_not_running");
}

/// Old clients: every old field keeps its name, type and meaning; `phase` keeps its values.
#[test]
fn old_clients_see_the_old_fields_unchanged() {
    let st = SpeedTest::new("http://127.0.0.1:1/blocked").with_context(|| RunContext {
        net_tune: Some(true),
        radio: Some(Radio {
            sinr_db: Some(14.0),
            band: Some("n78".into()),
            ca: Some(2),
        }),
        ..RunContext::default()
    });
    configure_local(&st, &server().base_url);
    let idle = st.progress().1["data"].clone();
    assert_eq!(
        (idle["phase"].as_str(), idle["method_version"].as_u64()),
        (Some("idle"), Some(2))
    );
    assert_eq!(
        st.begin(None, None, quick(), false, None, &claim_running).0,
        200
    );
    let mut phases = std::collections::BTreeSet::new();
    while st.running.load(Ordering::SeqCst) {
        phases.insert(
            st.progress().1["data"]["phase"]
                .as_str()
                .unwrap()
                .to_string(),
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let p = wait(&st);
    for ph in &phases {
        assert!(
            ["discovering", "latency", "download", "upload", "complete"].contains(&ph.as_str()),
            "{ph}"
        );
    }
    assert_eq!(p["phase"], "complete", "{p}");
    for (k, is) in [
        ("progress", Value::is_u64 as fn(&Value) -> bool),
        ("live_speed_mbps", Value::is_f64),
        ("ping_ms", Value::is_f64),
        ("jitter_ms", Value::is_f64),
        ("download_mbps", Value::is_f64),
        ("upload_mbps", Value::is_f64),
        ("download_bytes", Value::is_u64),
        ("upload_bytes", Value::is_u64),
        ("server", Value::is_string),
        ("provider", Value::is_string),
        ("error", Value::is_null),
        ("error_en", Value::is_null),
        ("error_code", Value::is_null),
    ] {
        assert!(is(&p[k]), "{k}: {}", p[k]);
    }
    assert!(p.get("fallback_reason").is_some());
    assert_eq!(p["net_tune"], "on");
    assert_eq!(p["radio"], json!({"sinr_db": 14.0, "band": "n78", "ca": 2}));
    assert_eq!(p["verdict"]["layers"].as_array().unwrap().len(), 5);
    assert!(p["verdict"]["summary_zh"].is_string() && p["verdict"]["summary_en"].is_string());
    assert_eq!(p["connections"], 6);
    assert!(p["stage"].is_null());
}
