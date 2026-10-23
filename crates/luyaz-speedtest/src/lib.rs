//! One device-wide speed test, shared by the admin web, touch UI and diagnosis.
//! Discovery and transfers are background work. Auto tries Ookla, then locally configured
//! LibreSpeed endpoints, within a single time/data budget. Manual choices never switch provider.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
mod config;
pub use config::{Config, LibreServer};
mod runner;

/// Where a run stands; serialised in snake_case for the clients.
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Idle, // nothing started yet
    Discovering, // picking a server
    Latency,     // ping round
    Download,    // timed download
    Upload,      // fixed upload rounds
    // terminal states
    Complete,
    Cancelled,
    Error,
}
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    #[default]
    Ookla,
    Librespeed,
}
impl Provider {
    fn label(self) -> &'static str {
        match self {
            Self::Ookla => "Ookla",
            Self::Librespeed => "LibreSpeed",
        }
    }
}
/// The snapshot every client polls. Field order is the JSON order.
#[derive(Serialize, Clone, Default)]
pub struct SpeedTestProgress {
    phase: Phase,
    progress: u8,               // percent, 0..=100
    live_speed_mbps: f64,       // the current phase's running rate
    ping_ms: Option<f64>,       // median RTT
    jitter_ms: Option<f64>,     // mean RTT spread
    download_mbps: Option<f64>, // set once the download phase ends
    upload_mbps: Option<f64>,   // set once the upload phase ends
    download_bytes: u64,        // counted against the data budget
    upload_bytes: u64,          // likewise
    server: String,             // display name of the chosen endpoint
    provider: Option<Provider>,
    error: Option<String>,
    error_en: Option<String>,
    error_code: Option<String>,
    fallback_reason: Option<String>,
}
impl SpeedTestProgress {
    pub fn download_mbps(&self) -> Option<f64> {
        self.download_mbps.filter(|_| self.phase == Phase::Complete)
    }
    pub fn download_bytes(&self) -> u64 {
        self.download_bytes
    }
    #[doc(hidden)]
    pub fn ended(phase: Phase, download_mbps: Option<f64>) -> Self {
        Self {
            phase,
            progress: 100,
            download_mbps,
            ..Self::idle()
        }
    }
    /// Nothing run yet: every number zero, every option empty.
    fn idle() -> Self {
        Self::default()
    }
    pub fn failure(&self) -> Option<String> {
        match self.phase {
            Phase::Complete => None,
            Phase::Cancelled => Some("cancelled".into()),
            _ => Some(
                self.error_en
                    .clone()
                    .or_else(|| self.error.clone())
                    .unwrap_or_else(|| "failed".into()),
            ),
        }
    }
}
#[derive(Serialize, Clone)]
pub struct TestServer {
    pub id: u64,         // provider's server id (LibreSpeed: config index)
    pub name: String,    // city or label shown in lists
    pub sponsor: String, // who runs it
    pub country: String, // as the provider reports it
    pub provider: Provider,
    pub host: String, // host[:port], for display
    pub url: String,  // the provider's own upload URL
    // Not sent to clients: where the runner actually connects.
    #[serde(skip)]
    pub base_url: String, // scheme://host[:port]/path prefix
    #[serde(skip)]
    ping_url: String,
    #[serde(skip)]
    download_url: String,
}
#[derive(Clone)]
struct Failure {
    code: &'static str,
    zh: String,
    en: String,
}
impl Failure {
    fn service(code: &'static str, p: Provider, what: &str) -> Self {
        Self {
            code,
            zh: format!(
                "{} 测速服务{}，可能受网络限制；请重试或切换来源。",
                p.label(),
                match what {
                    "timed out" => "连接超时",
                    "has no available servers" => "没有可用节点",
                    "returned an invalid server list" => "返回的节点列表无效",
                    "could not complete the transfer" => "传输失败",
                    _ => "无法连接",
                }
            ),
            en: format!(
                "{} speed-test service {what}; it may be restricted. Retry or switch provider.",
                p.label()
            ),
        }
    }
    fn reply(&self, status: u16) -> (u16, Value) {
        (
            status,
            json!({"ok":false,"error":self.zh,"error_en":self.en,"error_code":self.code}),
        )
    }
}
#[derive(Default)]
struct Discovery {
    list: Vec<TestServer>,
    at: Option<Instant>,
    refreshing: bool,
    error: Option<Failure>,
}
#[derive(Clone)]
pub struct SpeedTest {
    progress: Arc<Mutex<SpeedTestProgress>>, // what the status route returns
    cancel: Arc<AtomicBool>,                 // set by stop, polled by the runner
    running: Arc<AtomicBool>,                // one run at a time
    servers_cache: Arc<Mutex<Discovery>>,
    servers_url: String,
    config: Arc<Mutex<Config>>,
    config_path: Option<PathBuf>,
    config_error: Arc<Mutex<Option<String>>>,
}
// Run shape: ten pings, a 15 s download, ten 1 MB uploads, read in 16 KiB chunks.
const PING_COUNT: usize = 10;
const DOWNLOAD_DURATION: Duration = Duration::from_secs(15); // time-boxed, not size-boxed
const UPLOAD_ROUNDS: usize = 10;
const UPLOAD_SIZE: usize = 1_000_000;
const BUF_SIZE: usize = 16 * 1024;
/// How long a discovered server list stays fresh.
const CACHE_TTL: Duration = Duration::from_secs(5 * 60);
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limits {
    pub ping: bool,
    pub dl_time: Duration,
    pub dl_max_bytes: Option<u64>,
    pub upload: bool,
}
pub const FULL: Limits = Limits {
    ping: true,
    dl_time: DOWNLOAD_DURATION,
    dl_max_bytes: None,
    upload: true,
};
pub const CAPPED: Limits = Limits {
    ping: false,
    dl_time: Duration::from_secs(5),
    dl_max_bytes: Some(30_000_000),
    upload: false,
};
pub type OnDone = Box<dyn FnOnce(&SpeedTestProgress) + Send>;
pub type Admit<'a> = &'a dyn Fn(&AtomicBool) -> Result<(), (u16, Value)>;
pub fn claim_running(running: &AtomicBool) -> Result<(), (u16, Value)> {
    running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::Relaxed)
        .map(|_| ())
        .map_err(|_| (409, json!({"ok":false,"error":"test already running"})))
}
pub const SERVERS_URL: &str = "https://www.speedtest.net/api/js/servers?engine=js&limit=20";
impl SpeedTest {
    pub fn new(servers_url: &str) -> Self {
        Self {
            progress: Arc::new(Mutex::new(SpeedTestProgress::idle())),
            cancel: Arc::new(AtomicBool::new(false)),
            running: Arc::new(AtomicBool::new(false)),
            servers_cache: Arc::new(Mutex::new(Discovery::default())),
            servers_url: servers_url.into(),
            config: Arc::new(Mutex::new(Config::default())),
            config_path: None,
            config_error: Arc::new(Mutex::new(None)),
        }
    }
    pub fn with_config_path(mut self, path: impl AsRef<Path>) -> Self {
        self.config_path = Some(path.as_ref().to_owned());
        match std::fs::read(path) {
            Ok(bytes) => match serde_json::from_slice::<Config>(&bytes)
                .map_err(|e| e.to_string())
                .and_then(|c| c.servers().map(|_| c))
            {
                Ok(c) => *self.config.lock().unwrap() = c,
                Err(e) => *self.config_error.lock().unwrap() = Some(e),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => *self.config_error.lock().unwrap() = Some(e.to_string()),
        }
        self
    }
    pub fn config(&self) -> (u16, Value) {
        (
            200,
            json!({"ok":true,"data":{"config":*self.config.lock().unwrap(),"load_error":*self.config_error.lock().unwrap()}}),
        )
    }
    pub fn configure(&self, body: &[u8]) -> (u16, Value) {
        let cfg = match serde_json::from_slice::<Config>(body) {
            Ok(c) => c,
            Err(_) => {
                return (
                    400,
                    json!({"ok":false,"error":"Invalid LibreSpeed configuration"}),
                );
            }
        };
        if let Err(e) = cfg.servers() {
            return (400, json!({"ok":false,"error":e}));
        }
        let mut current = self.config.lock().unwrap();
        if let Some(path) = &self.config_path {
            let save = || -> std::io::Result<()> {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let tmp = path.with_extension("json.tmp");
                std::fs::write(&tmp, serde_json::to_vec_pretty(&cfg)?)?;
                std::fs::rename(tmp, path)
            };
            if let Err(e) = save() {
                return (
                    500,
                    json!({"ok":false,"error":format!("Could not save speed-test configuration: {e}")}),
                );
            }
        }
        *current = cfg;
        *self.config_error.lock().unwrap() = None;
        drop(current);
        self.config()
    }
    pub fn running_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.running)
    }
    /// Cache-only reply; a single background refresh has a negative-cache backoff too.
    pub fn servers(&self) -> (u16, Value) {
        let mut cache = self.servers_cache.lock().unwrap();
        let ttl = if cache.error.is_some() {
            Duration::from_secs(30)
        } else {
            CACHE_TTL
        };
        if !cache.refreshing && cache.at.is_none_or(|t| t.elapsed() >= ttl) {
            cache.refreshing = true;
            let st = self.clone();
            std::thread::spawn(move || {
                let _ = st.discover(None);
            });
        }
        let mut list = self.config.lock().unwrap().servers().unwrap_or_default();
        list.extend(cache.list.clone());
        if list.is_empty() && !cache.refreshing {
            if let Some(e) = &cache.error {
                return e.reply(503);
            }
        }
        (
            200,
            json!({"ok":true,"data":list,"loading":cache.refreshing}),
        )
    }
    /// Explicit UI retry bypasses backoff without duplicating an active refresh.
    pub fn refresh_servers(&self) -> (u16, Value) {
        {
            let mut cache = self.servers_cache.lock().unwrap();
            if !cache.refreshing {
                cache.at = None;
            }
        }
        self.servers()
    }
    fn discover(&self, budget: Option<&runner::Budget>) -> Result<Vec<TestServer>, Failure> {
        let result = fetch_servers(
            &self.servers_url,
            budget
                .map(|b| b.remaining().min(Duration::from_secs(3)))
                .unwrap_or(Duration::from_secs(3)),
        );
        let mut cache = self.servers_cache.lock().unwrap();
        cache.refreshing = false;
        cache.at = Some(Instant::now());
        match &result {
            Ok(list) => {
                cache.list = list.clone();
                cache.error = None;
            }
            Err(e) => {
                cache.list.clear();
                cache.error = Some(e.clone());
            }
        }
        result
    }
    fn ookla(&self, budget: &runner::Budget) -> Result<Vec<TestServer>, Failure> {
        // The same single-flight refresh serves both the list and a starting test.
        let _ = self.servers();
        loop {
            {
                let cache = self.servers_cache.lock().unwrap();
                if !cache.refreshing {
                    if let Some(e) = &cache.error {
                        return Err(e.clone());
                    }
                    return Ok(cache.list.clone());
                }
            }
            if self.cancel.load(Ordering::Relaxed) || budget.remaining().is_zero() {
                return Err(Failure::service(
                    "provider_timeout",
                    Provider::Ookla,
                    "timed out",
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    pub fn start(&self, body: &[u8], admit: Admit) -> (u16, Value) {
        #[derive(Deserialize)]
        struct Request {
            provider: Option<String>,
            server_id: Option<u64>,
        }
        let Ok(req) = serde_json::from_slice::<Request>(body) else {
            return (400, json!({"ok": false, "error": "invalid JSON"}));
        };
        let mode = match req
            .provider
            .as_deref()
            .unwrap_or(if req.server_id.is_some() {
                "ookla"
            } else {
                "auto"
            }) {
            "auto" if req.server_id.is_none() => None,
            "ookla" => Some(Provider::Ookla),
            "librespeed" => Some(Provider::Librespeed),
            _ => {
                return (
                    400,
                    json!({"ok":false,"error":"Invalid speed-test provider"}),
                );
            }
        };
        self.begin(mode, req.server_id, FULL, None, admit)
    }
    pub fn start_capped(&self, on_done: OnDone, admit: Admit) -> (u16, Value) {
        self.begin(None, None, CAPPED, Some(on_done), admit)
    }
    fn begin(
        &self,
        mode: Option<Provider>,
        id: Option<u64>,
        lim: Limits,
        on_done: Option<OnDone>,
        admit: Admit,
    ) -> (u16, Value) {
        if let Err(e) = admit(&self.running) {
            return e;
        }
        self.cancel.store(false, Ordering::Relaxed);
        {
            let mut cache = self.servers_cache.lock().unwrap();
            if cache.error.is_some() && !cache.refreshing {
                cache.at = None;
            }
        }
        *self.progress.lock().unwrap() = SpeedTestProgress {
            phase: Phase::Discovering,
            provider: mode,
            ..SpeedTestProgress::idle()
        };
        let st = self.clone();
        std::thread::spawn(move || {
            let outcome =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| st.work(mode, id, lim)));
            if outcome.is_err() {
                st.fail(Failure {
                    code: "internal",
                    zh: "测速任务异常，请重试。".into(),
                    en: "Speed-test worker failed. Retry.".into(),
                });
            }
            // Capture this run before releasing the slot: a new start can reset progress.
            let last = st
                .progress
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            st.running.store(false, Ordering::SeqCst);
            if let Some(f) = on_done {
                f(&last);
            }
        });
        (200, json!({"ok":true,"data":{"status":"started"}}))
    }
    fn work(&self, mode: Option<Provider>, id: Option<u64>, lim: Limits) {
        let mut budget = runner::Budget::new(lim);
        let local = self.config.lock().unwrap().servers().unwrap_or_default();
        let providers =
            mode.map_or_else(|| vec![Provider::Ookla, Provider::Librespeed], |p| vec![p]);
        let mut last = Failure::service("no_servers", Provider::Ookla, "has no available servers");
        for provider in providers {
            if self.cancelled() {
                return;
            }
            if budget.remaining().is_zero() {
                break;
            }
            {
                let mut p = self.progress.lock().unwrap();
                p.phase = Phase::Discovering;
                p.provider = Some(provider);
            }
            let list = if provider == Provider::Ookla {
                self.ookla(&budget)
            } else if local.is_empty() {
                Err(Failure { code:"provider_not_configured", zh:"尚未配置 LibreSpeed 节点，请在管理网页添加可用节点。".into(), en:"No LibreSpeed endpoints configured. Add a reachable endpoint in the admin web.".into() })
            } else {
                Ok(local.clone())
            };
            match list {
                Ok(list) => {
                    let choices: Vec<_> = list
                        .into_iter()
                        .filter(|s| id.is_none_or(|i| i == s.id))
                        .take(if id.is_some() { 1 } else { 3 })
                        .collect();
                    if choices.is_empty() {
                        last = Failure::service("no_servers", provider, "has no available servers");
                    }
                    for server in choices {
                        if self.cancelled() {
                            return;
                        }
                        {
                            let mut p = self.progress.lock().unwrap();
                            p.server = format!(
                                "{} · {} ({})",
                                provider.label(),
                                server.sponsor,
                                server.name
                            );
                            p.ping_ms = None;
                            p.jitter_ms = None;
                            p.download_mbps = None;
                            p.upload_mbps = None;
                            p.progress = 0;
                        }
                        match runner::run(&server, &self.progress, &self.cancel, lim, &mut budget) {
                            Ok(()) => {
                                if !self.cancelled() {
                                    let mut p = self.progress.lock().unwrap();
                                    p.phase = Phase::Complete;
                                    p.progress = 100;
                                    p.live_speed_mbps = 0.0;
                                }
                                return;
                            }
                            Err(e) => last = e,
                        }
                        // A manual provider selects one node unless a node ID was supplied; it never switches sources.
                        if mode.is_some() || budget.remaining().is_zero() {
                            break;
                        }
                    }
                }
                Err(e) => last = e,
            }
            if self.cancelled() {
                return;
            }
            if mode.is_none() && provider == Provider::Ookla {
                self.progress.lock().unwrap().fallback_reason = Some(last.code.into());
                // Keep the failed Ookla reason when there is no configured fallback.
                if local.is_empty() {
                    break;
                }
            }
        }
        if !self.cancelled() {
            self.fail(last);
        }
    }
    fn fail(&self, e: Failure) {
        let mut p = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        p.phase = Phase::Error;
        p.live_speed_mbps = 0.0;
        p.error = Some(e.zh);
        p.error_en = Some(e.en);
        p.error_code = Some(e.code.into());
    }
    fn cancelled(&self) -> bool {
        if !self.cancel.load(Ordering::Relaxed) {
            return false;
        }
        let mut p = self.progress.lock().unwrap();
        p.phase = Phase::Cancelled;
        p.live_speed_mbps = 0.0;
        true
    }
    pub fn progress(&self) -> (u16, Value) {
        (
            200,
            json!({"ok":true,"data":*self.progress.lock().unwrap()}),
        )
    }
    pub fn stop(&self) -> (u16, Value) {
        let status = if self.running.load(Ordering::Relaxed) {
            self.cancel.store(true, Ordering::Relaxed);
            "stopping"
        } else {
            "not_running"
        };
        (200, json!({"ok":true,"data":{"status":status}}))
    }
}
fn fetch_servers(url: &str, timeout: Duration) -> Result<Vec<TestServer>, Failure> {
    if timeout.is_zero() {
        return Err(Failure::service(
            "provider_timeout",
            Provider::Ookla,
            "timed out",
        ));
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .build()
        .into();
    let resp = agent.get(url).call().map_err(|e| {
        Failure::service(
            if matches!(e, ureq::Error::Timeout(_)) {
                "provider_timeout"
            } else {
                "provider_unreachable"
            },
            Provider::Ookla,
            if matches!(e, ureq::Error::Timeout(_)) {
                "timed out"
            } else {
                "is unreachable"
            },
        )
    })?;
    let data: Vec<Value> = resp
        .into_body()
        .with_config()
        .limit(128 * 1024)
        .read_json()
        .map_err(|_| {
            Failure::service(
                "invalid_server_list",
                Provider::Ookla,
                "returned an invalid server list",
            )
        })?;
    let list: Vec<_> = data
        .iter()
        .take(20)
        .filter_map(|entry| {
            let url = config::http_url(entry["url"].as_str()?).ok()?;
            let base = url.join(".").ok()?;
            Some(TestServer {
                id: entry["id"]
                    .as_u64()
                    .or_else(|| entry["id"].as_str()?.parse().ok())?,
                name: entry["name"]
                    .as_str()
                    .unwrap_or_default()
                    .chars()
                    .take(48)
                    .collect(),
                sponsor: entry["sponsor"]
                    .as_str()
                    .unwrap_or_default()
                    .chars()
                    .take(48)
                    .collect(),
                country: entry["country"]
                    .as_str()
                    .unwrap_or_default()
                    .chars()
                    .take(32)
                    .collect(),
                provider: Provider::Ookla,
                host: entry["host"].as_str().unwrap_or_default().into(),
                url: url.to_string(),
                base_url: base.to_string(),
                ping_url: base.join("latency.txt").ok()?.to_string(),
                download_url: base.join("random4000x4000.jpg").ok()?.to_string(),
            })
        })
        .collect();
    if list.is_empty() {
        Err(Failure::service(
            "no_servers",
            Provider::Ookla,
            "has no available servers",
        ))
    } else {
        Ok(list)
    }
}
fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}
#[cfg(test)]
#[path = "tests.rs"]
mod tests;
