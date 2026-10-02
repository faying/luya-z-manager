use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};

use crate::handlers::AppState;

// --- Data types ---

#[derive(Serialize, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Idle,
    Latency,
    Download,
    Upload,
    Complete,
    Cancelled,
    Error,
}

#[derive(Serialize, Clone)]
pub struct SpeedTestProgress {
    phase: Phase,
    progress: u8,
    live_speed_mbps: f64,
    ping_ms: Option<f64>,
    jitter_ms: Option<f64>,
    download_mbps: Option<f64>,
    upload_mbps: Option<f64>,
    download_bytes: u64,
    upload_bytes: u64,
    server: String,
    error: Option<String>,
}

impl SpeedTestProgress {
    pub fn download_mbps(&self) -> Option<f64> {
        self.download_mbps.filter(|_| self.phase == Phase::Complete)
    }
    pub fn download_bytes(&self) -> u64 {
        self.download_bytes
    }
    #[cfg(test)]
    pub fn ended(phase: Phase, download_mbps: Option<f64>) -> Self {
        SpeedTestProgress {
            phase,
            progress: 100,
            live_speed_mbps: 0.0,
            ping_ms: None,
            jitter_ms: None,
            download_mbps,
            upload_mbps: None,
            download_bytes: 0,
            upload_bytes: 0,
            server: String::new(),
            error: None,
        }
    }

    /// Why it didn't finish ("cancelled" when stopped).
    pub fn failure(&self) -> Option<String> {
        match self.phase {
            Phase::Complete => None,
            Phase::Cancelled => Some("cancelled".into()),
            _ => Some(self.error.clone().unwrap_or_else(|| "failed".into())),
        }
    }
}

#[derive(Serialize, Clone)]
pub struct TestServer {
    pub id: u64,
    pub name: String,
    pub sponsor: String,
    pub country: String,
    pub host: String,
    pub url: String,
    #[serde(skip)]
    pub base_url: String,
}

pub struct SpeedTest {
    progress: Arc<Mutex<SpeedTestProgress>>,
    cancel: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    servers_cache: Arc<Mutex<(Vec<TestServer>, Instant)>>,
}

const CACHE_TTL: Duration = Duration::from_secs(300);
const DOWNLOAD_DURATION: Duration = Duration::from_secs(15);
const UPLOAD_SIZE: usize = 1_000_000; // 1 MB
const UPLOAD_ROUNDS: usize = 10;
const PING_COUNT: usize = 10;
const BUF_SIZE: usize = 16384; // 16 KB

/// How much one run does. [`FULL`] is the speed-test page, unchanged;
/// [`CAPPED`] is deep diagnosis' "add speed test" (`slow-diagnosis.md` §4.2):
/// download only, about 5 s and at most 30 MB, so it is cheap on a metered SIM.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limits {
    pub ping: bool,
    pub dl_time: Duration,
    pub dl_max_bytes: Option<u64>,
    pub upload: bool,
}

pub const FULL: Limits = Limits { ping: true, dl_time: DOWNLOAD_DURATION, dl_max_bytes: None, upload: true };
pub const CAPPED: Limits = Limits { ping: false, dl_time: Duration::from_secs(5), dl_max_bytes: Some(30_000_000), upload: false };

/// Called with the final progress when a run ends (any way it ends).
pub type OnDone = Box<dyn FnOnce(&SpeedTestProgress) + Send>;

/// The live `running` flag (the first `SpeedTest::new`), for [`running`].
static LIVE: std::sync::OnceLock<Arc<AtomicBool>> = std::sync::OnceLock::new();

/// A speed test is going (deep diagnosis waits for it, D4).
pub fn running() -> bool {
    LIVE.get().is_some_and(|r| r.load(Ordering::SeqCst))
}

/// Check and claim `running` under deep diagnosis' gate (D10): refused while
/// a diagnosis runs or another test is going.
fn admit(running: &AtomicBool) -> Result<(), (u16, Value)> {
    let gate = crate::deep_diag::gate();
    if let Some(busy) = crate::deep_diag::refusal(&gate) {
        return Err(busy);
    }
    if running.compare_exchange(false, true, Ordering::SeqCst, Ordering::Relaxed).is_err() {
        return Err((409, json!({"ok": false, "error": "test already running"})));
    }
    Ok(())
}

impl SpeedTest {
    pub fn new() -> Self {
        let running = Arc::new(AtomicBool::new(false));
        let _ = LIVE.set(Arc::clone(&running));
        Self {
            progress: Arc::new(Mutex::new(SpeedTestProgress {
                phase: Phase::Idle,
                progress: 0,
                live_speed_mbps: 0.0,
                ping_ms: None,
                jitter_ms: None,
                download_mbps: None,
                upload_mbps: None,
                download_bytes: 0,
                upload_bytes: 0,
                server: String::new(),
                error: None,
            })),
            cancel: Arc::new(AtomicBool::new(false)),
            running,
            servers_cache: Arc::new(Mutex::new((Vec::new(), Instant::now() - CACHE_TTL))),
        }
    }
}

// --- Server fetching ---

fn fetch_servers() -> Result<Vec<TestServer>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .into();

    let resp = agent
        .get("https://www.speedtest.net/api/js/servers?engine=js&limit=20")
        .call()
        .map_err(|e| format!("fetch servers: {e}"))?;

    let data: Vec<Value> = resp
        .into_body()
        .read_json()
        .map_err(|e| format!("parse servers: {e}"))?;

    let mut servers = Vec::new();
    for entry in &data {
        let url = entry["url"].as_str().unwrap_or_default();
        // base_url: everything up to and including the last '/'
        let base_url = match url.rfind('/') {
            Some(i) => &url[..=i],
            None => continue,
        };

        servers.push(TestServer {
            id: entry["id"].as_u64()
                .or_else(|| entry["id"].as_str().and_then(|s| s.parse().ok()))
                .unwrap_or(0),
            name: entry["name"].as_str().unwrap_or("").to_string(),
            sponsor: entry["sponsor"].as_str().unwrap_or("").to_string(),
            country: entry["country"].as_str().unwrap_or("").to_string(),
            host: entry["host"].as_str().unwrap_or("").to_string(),
            url: url.to_string(),
            base_url: base_url.to_string(),
        });
    }
    Ok(servers)
}

fn get_servers(cache: &Arc<Mutex<(Vec<TestServer>, Instant)>>) -> Result<Vec<TestServer>, String> {
    let guard = cache.lock().unwrap();
    if !guard.0.is_empty() && guard.1.elapsed() < CACHE_TTL {
        return Ok(guard.0.clone());
    }
    drop(guard);

    let servers = fetch_servers()?;
    let mut guard = cache.lock().unwrap();
    guard.0 = servers.clone();
    guard.1 = Instant::now();
    Ok(servers)
}

// --- Background test logic ---

fn run_test(
    server: &TestServer,
    progress: &Arc<Mutex<SpeedTestProgress>>,
    cancel: &Arc<AtomicBool>,
    lim: &Limits,
) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .into();

    let ping_agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .build()
        .into();

    // --- Latency phase ---
    if lim.ping {
    {
        let mut guard = progress.lock().unwrap();
        guard.phase = Phase::Latency;
        guard.progress = 0;
    }

    let ping_url = format!("{}latency.txt", server.base_url);
    let mut rtts = Vec::with_capacity(PING_COUNT);

    for i in 0..PING_COUNT {
        if cancel.load(Ordering::Relaxed) {
            set_cancelled(progress);
            return;
        }

        let start = Instant::now();
        match ping_agent.get(&ping_url).call() {
            Ok(resp) => {
                let _ = resp.into_body().read_to_vec();
                rtts.push(start.elapsed().as_secs_f64() * 1000.0);
            }
            Err(_) => {} // skip failed pings
        }

        let pct = ((i + 1) as u8 * 20) / PING_COUNT as u8;
        let mut guard = progress.lock().unwrap();
        guard.progress = pct;
    }

    if rtts.is_empty() {
        set_error(progress, "all ping attempts failed");
        return;
    }

    rtts.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median_ping = rtts[rtts.len() / 2];
    let jitter = if rtts.len() > 1 {
        let diffs: Vec<f64> = rtts.windows(2).map(|w| (w[1] - w[0]).abs()).collect();
        diffs.iter().sum::<f64>() / diffs.len() as f64
    } else {
        0.0
    };

    {
        let mut guard = progress.lock().unwrap();
        guard.ping_ms = Some(round2(median_ping));
        guard.jitter_ms = Some(round2(jitter));
    }
    }
    // progress range of the download: 20–60 % with the other phases, else 0–100 %
    let (dl_from, dl_span) = match (lim.ping, lim.upload) {
        (false, false) => (0u8, 100.0),
        _ => (20u8, 40.0),
    };

    // --- Download phase ---
    {
        let mut guard = progress.lock().unwrap();
        guard.phase = Phase::Download;
        guard.progress = dl_from;
    }

    let download_url = format!("{}random4000x4000.jpg", server.base_url);
    let dl_start = Instant::now();
    let mut dl_bytes: u64 = 0;
    let mut buf = [0u8; BUF_SIZE];

    // Download for lim.dl_time (or up to lim.dl_max_bytes), re-fetching the file if it finishes early
    'dl_outer: while dl_start.elapsed() < lim.dl_time {
        if cancel.load(Ordering::Relaxed) {
            set_cancelled(progress);
            return;
        }

        let resp = match agent.get(&download_url).call() {
            Ok(r) => r,
            Err(_) => break,
        };

        let mut body = resp.into_body();
        let mut reader = body.as_reader();
        loop {
            if cancel.load(Ordering::Relaxed) {
                set_cancelled(progress);
                return;
            }
            if dl_start.elapsed() >= lim.dl_time {
                break 'dl_outer;
            }

            match reader.read(&mut buf) {
                Ok(0) => break, // EOF, re-fetch
                Ok(n) => {
                    dl_bytes += n as u64;
                    let elapsed = dl_start.elapsed().as_secs_f64();
                    let speed = if elapsed > 0.0 {
                        (dl_bytes as f64 * 8.0) / (elapsed * 1_000_000.0)
                    } else {
                        0.0
                    };
                    let pct = dl_from + ((dl_start.elapsed().as_secs_f64() / lim.dl_time.as_secs_f64()) * dl_span).min(dl_span) as u8;
                    {
                        let mut guard = progress.lock().unwrap();
                        guard.live_speed_mbps = round2(speed);
                        guard.download_bytes = dl_bytes;
                        guard.progress = pct;
                    }
                    if lim.dl_max_bytes.is_some_and(|m| dl_bytes >= m) {
                        break 'dl_outer;
                    }
                }
                Err(_) => break,
            }
        }
    }

    let dl_elapsed = dl_start.elapsed().as_secs_f64();
    let dl_speed = if dl_elapsed > 0.0 {
        (dl_bytes as f64 * 8.0) / (dl_elapsed * 1_000_000.0)
    } else {
        0.0
    };

    {
        let mut guard = progress.lock().unwrap();
        guard.download_mbps = Some(round2(dl_speed));
        guard.download_bytes = dl_bytes;
        guard.progress = 60;
        if !lim.upload {
            guard.phase = Phase::Complete;
            guard.progress = 100;
            guard.live_speed_mbps = 0.0;
        }
    }
    if !lim.upload {
        return;
    }

    // --- Upload phase ---
    {
        let mut guard = progress.lock().unwrap();
        guard.phase = Phase::Upload;
        guard.live_speed_mbps = 0.0;
    }

    let upload_buf = vec![0u8; UPLOAD_SIZE];
    let ul_start = Instant::now();
    let mut ul_bytes: u64 = 0;

    for i in 0..UPLOAD_ROUNDS {
        if cancel.load(Ordering::Relaxed) {
            set_cancelled(progress);
            return;
        }

        match agent.post(&server.url).send(&upload_buf[..]) {
            Ok(_) => {
                ul_bytes += UPLOAD_SIZE as u64;
            }
            Err(_) => {} // continue on error
        }

        let elapsed = ul_start.elapsed().as_secs_f64();
        let speed = if elapsed > 0.0 {
            (ul_bytes as f64 * 8.0) / (elapsed * 1_000_000.0)
        } else {
            0.0
        };
        // u32: (i + 1) * 40 passes 255 at round 7
        let pct = 60 + ((i as u32 + 1) * 40 / UPLOAD_ROUNDS as u32) as u8;
        let mut guard = progress.lock().unwrap();
        guard.live_speed_mbps = round2(speed);
        guard.upload_bytes = ul_bytes;
        guard.progress = pct;
    }

    let ul_elapsed = ul_start.elapsed().as_secs_f64();
    let ul_speed = if ul_elapsed > 0.0 {
        (ul_bytes as f64 * 8.0) / (ul_elapsed * 1_000_000.0)
    } else {
        0.0
    };

    {
        let mut guard = progress.lock().unwrap();
        guard.upload_mbps = Some(round2(ul_speed));
        guard.upload_bytes = ul_bytes;
        guard.phase = Phase::Complete;
        guard.progress = 100;
        guard.live_speed_mbps = 0.0;
    }
}

fn set_cancelled(progress: &Arc<Mutex<SpeedTestProgress>>) {
    let mut guard = progress.lock().unwrap();
    guard.phase = Phase::Cancelled;
    guard.live_speed_mbps = 0.0;
}

fn set_error(progress: &Arc<Mutex<SpeedTestProgress>>, msg: &str) {
    let mut guard = progress.lock().unwrap();
    guard.phase = Phase::Error;
    guard.error = Some(msg.to_string());
    guard.live_speed_mbps = 0.0;
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

// --- Handler functions ---

pub fn servers(state: &AppState) -> (u16, Value) {
    match get_servers(&state.speedtest.servers_cache) {
        Ok(list) => (200, json!({"ok": true, "data": list})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn start(state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };

    let server_id = parsed["server_id"].as_u64();

    let server_list = match get_servers(&state.speedtest.servers_cache) {
        Ok(s) => s,
        Err(e) => return (503, json!({"ok": false, "error": e})),
    };

    let server = if let Some(id) = server_id {
        match server_list.iter().find(|s| s.id == id) {
            Some(s) => s.clone(),
            None => return (404, json!({"ok": false, "error": "server not found"})),
        }
    } else {
        match server_list.into_iter().next() {
            Some(s) => s,
            None => return (503, json!({"ok": false, "error": "no servers available"})),
        }
    };

    launch(&state.speedtest, server, FULL, None)
}

/// Deep diagnosis' "add speed test": [`CAPPED`] on the first (nearest)
/// server; `on_done` gets the final progress. Same gate and "already running"
/// rules as the page's test, and its progress shows on the page too.
pub fn start_capped(state: &AppState, on_done: OnDone) -> (u16, Value) {
    let server = match get_servers(&state.speedtest.servers_cache) {
        Ok(list) => match list.into_iter().next() {
            Some(s) => s,
            None => return (503, json!({"ok": false, "error": "no servers available"})),
        },
        Err(e) => return (503, json!({"ok": false, "error": e})),
    };
    launch(&state.speedtest, server, CAPPED, Some(on_done))
}

fn launch(st: &SpeedTest, server: TestServer, lim: Limits, on_done: Option<OnDone>) -> (u16, Value) {
    // Claim first, so a refused start leaves the running test's progress alone
    if let Err(refused) = admit(&st.running) {
        return refused;
    }

    // Reset state
    st.cancel.store(false, Ordering::Relaxed);
    {
        let mut guard = st.progress.lock().unwrap();
        *guard = SpeedTestProgress {
            phase: Phase::Idle,
            progress: 0,
            live_speed_mbps: 0.0,
            ping_ms: None,
            jitter_ms: None,
            download_mbps: None,
            upload_mbps: None,
            download_bytes: 0,
            upload_bytes: 0,
            server: format!("{} ({})", server.sponsor, server.name),
            error: None,
        };
    }

    let progress = Arc::clone(&st.progress);
    let cancel = Arc::clone(&st.cancel);
    let running = Arc::clone(&st.running);

    std::thread::spawn(move || {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_test(&server, &progress, &cancel, &lim);
        }));
        running.store(false, Ordering::Relaxed);
        if let Some(f) = on_done {
            let last = progress.lock().unwrap_or_else(|e| e.into_inner()).clone();
            f(&last);
        }
    });

    (200, json!({"ok": true, "data": {"status": "started"}}))
}

pub fn progress(state: &AppState) -> (u16, Value) {
    let guard = state.speedtest.progress.lock().unwrap();
    (200, json!({"ok": true, "data": *guard}))
}

pub fn stop(state: &AppState, _body: &[u8]) -> (u16, Value) {
    if !state.speedtest.running.load(Ordering::Relaxed) {
        return (200, json!({"ok": true, "data": {"status": "not_running"}}));
    }
    state.speedtest.cancel.store(true, Ordering::Relaxed);
    (200, json!({"ok": true, "data": {"status": "stopping"}}))
}

#[cfg(test)]
mod tests {
    use super::*;
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
                    if url.ends_with("random4000x4000.jpg") {
                        let len = 8_000_000;
                        let body = Slow { left: len, bps: 20_000_000, sent: 0, start: None };
                        let _ = rq.respond(tiny_http::Response::new(tiny_http::StatusCode(200), vec![], body, Some(len as usize), None));
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
            base_url: base,
        }
    }

    fn run(lim: Limits) -> (SpeedTestProgress, Duration) {
        let st = SpeedTest::new();
        let (tx, rx) = std::sync::mpsc::channel();
        let t = Instant::now();
        let (code, _) = launch(&st, server(), lim, Some(Box::new(move |p: &SpeedTestProgress| tx.send(p.clone()).unwrap())));
        assert_eq!(code, 200);
        let p = rx.recv_timeout(Duration::from_secs(60)).unwrap();
        assert!(!st.running.load(Ordering::SeqCst));
        (p, t.elapsed())
    }

    /// The page's test is unchanged: ping, 15 s of download, then upload.
    #[test]
    fn full_run_does_all_three_phases() {
        let (p, took) = run(FULL);
        assert!(p.phase == Phase::Complete && p.progress == 100, "{:?}", p.error);
        assert!(p.ping_ms.is_some() && p.upload_mbps.is_some() && p.download_mbps.is_some());
        assert_eq!(p.upload_bytes, (UPLOAD_SIZE * UPLOAD_ROUNDS) as u64);
        assert!(took >= DOWNLOAD_DURATION, "{took:?}");
        // ~20 MB/s for 15 s: well past the capped run's 30 MB
        assert!(p.download_bytes > 60_000_000, "{}", p.download_bytes);
        assert_eq!(FULL, Limits { ping: true, dl_time: Duration::from_secs(15), dl_max_bytes: None, upload: true });
    }

    /// "Add speed test": download only, stops at 30 MB (here ~1.5 s) or 5 s.
    #[test]
    fn capped_run_stops_early_and_skips_ping_and_upload() {
        let (p, took) = run(CAPPED);
        assert!(p.phase == Phase::Complete && p.progress == 100, "{:?}", p.error);
        assert!(p.ping_ms.is_none() && p.upload_mbps.is_none() && p.upload_bytes == 0);
        assert!(p.download_bytes >= 30_000_000 && p.download_bytes < 30_000_000 + BUF_SIZE as u64, "{}", p.download_bytes);
        assert!(took < Duration::from_secs(6), "{took:?}");
        assert!(p.download_mbps().is_some_and(|m| m > 10.0));
        assert_eq!(p.failure(), None);
    }

    #[test]
    fn second_start_is_refused_and_leaves_progress_alone() {
        let st = SpeedTest::new();
        st.running.store(true, Ordering::SeqCst);
        st.progress.lock().unwrap().download_bytes = 1234;
        let (code, body) = launch(&st, server(), FULL, None);
        assert_eq!((code, body["error"].as_str()), (409, Some("test already running")));
        assert_eq!(st.progress.lock().unwrap().download_bytes, 1234);
    }
}
