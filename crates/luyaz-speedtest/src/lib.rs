//! One device-wide speed test, shared by the admin web, touch UI and diagnosis.
//! Discovery and transfers are background work. Auto tries Ookla, then locally configured
//! LibreSpeed endpoints, within a single time/data budget. Manual choices never switch provider.
//!
//! Method v2 (net-performance.md 1a): idle latency, one long single-connection download,
//! a 6-connection download with a latency probe under load, an optional same-node download
//! through CHILL's 7894 listener, and a 6-connection upload of fixed-size POSTs. Each stage
//! drops its ramp and reports the stable part beside the whole-stage mean.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
mod chill;
mod config;
pub use config::{Config, LibreServer};
mod runner;
mod sys;
mod verdict;
pub use verdict::{Layer, LayerState, Verdict};

/// Where a run stands; serialised in snake_case for the clients.
#[derive(Serialize, Clone, Copy, Debug, Default, PartialEq)]
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
/// v2's sub-stage (`phase` keeps its old values for old clients).
#[derive(Serialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    IdleLatency,
    SingleDownload,
    MultiDownload,
    ChillDownload,
    MultiUpload,
}
#[derive(Serialize, Clone, Copy, Debug, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Full,
    /// the diagnosis' capped run, or a full run downgraded while roaming
    Coarse,
}
#[derive(Serialize, Clone, Copy, Debug, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PathKind {
    #[default]
    Direct,
    /// direct plus the same node through CHILL (R5)
    Chill,
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
/// Reference radio readings the core passes in at the start (never a capacity figure).
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct Radio {
    pub sinr_db: Option<f64>,
    pub band: Option<String>,
    pub ca: Option<u8>,
}
/// What the core knows about the device when a test starts (read once at admission; `cell`
/// again around the CHILL pair).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunContext {
    /// the same source as ship's roaming gate: a roaming device only runs coarse (R9)
    pub roaming: bool,
    /// the serving cell's key: coarse runs reuse a node picked on this cell (R10)
    pub cell: Option<String>,
    pub radio: Option<Radio>,
    /// `/data/luyaz/net-tune.want` exists (DR14)
    pub net_tune: Option<bool>,
}
type Context = Arc<dyn Fn() -> RunContext + Send + Sync>;

/// The snapshot every client polls. Field order is the JSON order; the v2 fields only add.
#[derive(Serialize, Clone)]
pub struct SpeedTestProgress {
    phase: Phase,
    progress: u8,               // percent, 0..=100
    live_speed_mbps: f64,       // the current stage's running rate
    ping_ms: Option<f64>,       // idle median RTT
    jitter_ms: Option<f64>,     // idle mean RTT spread
    download_mbps: Option<f64>, // multi-connection stable rate, once measured
    upload_mbps: Option<f64>,   // multi-connection upload stable rate (acknowledged bytes)
    download_bytes: u64,        // every byte received, counted against the data budget
    upload_bytes: u64,          // every byte sent, likewise
    server: String,             // display name of the chosen endpoint
    provider: Option<Provider>,
    error: Option<String>,
    error_en: Option<String>,
    error_code: Option<String>,
    fallback_reason: Option<String>,
    // ── v2 (net-performance.md「第 1 期实现契约」) ──
    method_version: u8,
    mode: Mode,
    roaming_capped: bool,
    path: PathKind,
    stage: Option<Stage>,
    step: u8,
    steps: u8,
    connections: u8,
    download_whole_mbps: Option<f64>,
    single_mbps: Option<f64>,
    single_whole_mbps: Option<f64>,
    upload_whole_mbps: Option<f64>,
    chill_mbps: Option<f64>,
    chill_whole_mbps: Option<f64>,
    chill_region: Option<String>,
    idle_ms: Option<f64>,
    loaded_down_ms: Option<f64>,
    loaded_up_ms: Option<f64>,
    loaded_chill_ms: Option<f64>,
    sample_short: bool,
    server_name: String,
    server_sponsor: String,
    net_tune: Option<&'static str>,
    radio: Option<Radio>,
    cpu_peak_pct: Option<f64>,
    mihomo_cpu_pct: Option<f64>,
    verdict: Option<Verdict>,
}
impl Default for SpeedTestProgress {
    fn default() -> Self {
        Self {
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
            provider: None,
            error: None,
            error_en: None,
            error_code: None,
            fallback_reason: None,
            method_version: METHOD_VERSION,
            mode: Mode::Full,
            roaming_capped: false,
            path: PathKind::Direct,
            stage: None,
            step: 0,
            steps: STEPS,
            connections: FULL.connections,
            download_whole_mbps: None,
            single_mbps: None,
            single_whole_mbps: None,
            upload_whole_mbps: None,
            chill_mbps: None,
            chill_whole_mbps: None,
            chill_region: None,
            idle_ms: None,
            loaded_down_ms: None,
            loaded_up_ms: None,
            loaded_chill_ms: None,
            sample_short: false,
            server_name: String::new(),
            server_sponsor: String::new(),
            net_tune: None,
            radio: None,
            cpu_peak_pct: None,
            mihomo_cpu_pct: None,
            verdict: None,
        }
    }
}
impl SpeedTestProgress {
    pub fn download_mbps(&self) -> Option<f64> {
        self.download_mbps.filter(|_| self.phase == Phase::Complete)
    }
    pub fn download_bytes(&self) -> u64 {
        self.download_bytes
    }
    pub fn sample_short(&self) -> bool {
        self.sample_short
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
    /// A new node is tried: forget the last attempt's measurements (not the byte counts).
    fn clear_measurements(&mut self) {
        let keep = Self {
            phase: self.phase,
            download_bytes: self.download_bytes,
            upload_bytes: self.upload_bytes,
            server: std::mem::take(&mut self.server),
            provider: self.provider,
            fallback_reason: self.fallback_reason.take(),
            mode: self.mode,
            roaming_capped: self.roaming_capped,
            path: self.path,
            steps: self.steps,
            connections: self.connections,
            server_name: std::mem::take(&mut self.server_name),
            server_sponsor: std::mem::take(&mut self.server_sponsor),
            net_tune: self.net_tune,
            radio: self.radio.take(),
            ..Self::idle()
        };
        *self = keep;
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
/// The node the last full run picked (R10): coarse runs on the same cell reuse it for 30 min.
struct Pick {
    server: TestServer,
    at: Instant,
    cell: String,
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
    context: Context,
    /// CHILL's mixed listener bound to 🚀 节点选择 (template.yaml); never 7895 (the AI gate)
    chill_proxy: String,
    /// mihomo's controller, for the main group's `now` (the region label only)
    chill_controller: String,
    last_pick: Arc<Mutex<Option<Pick>>>,
}
const METHOD_VERSION: u8 = 2;
const STEPS: u8 = 5;
const PING_COUNT: usize = 10;
/// Read buffer per connection (heap; the transfer threads have small stacks).
const BUF_SIZE: usize = 64 * 1024;
/// How long a discovered server list stays fresh.
const CACHE_TTL: Duration = Duration::from_secs(5 * 60);
/// A coarse run reuses the last full run's node this long (R10).
const PICK_TTL: Duration = Duration::from_secs(30 * 60);
const MB: u64 = 1_000_000;
/// The shape of one run. Every duration is here so tests can shrink a run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limits {
    /// the diagnosis' capped run: multi-connection download only, no verdict (R3)
    pub coarse: bool,
    /// idle latency first, and the latency probe under load
    pub ping: bool,
    pub single: bool,
    pub upload: bool,
    pub connections: u8,
    pub idle_time: Duration,
    pub single_time: Duration,
    /// the multi-connection download, and the CHILL download of the pair
    pub multi_time: Duration,
    pub upload_time: Duration,
    /// the start of each stage that is not counted in its stable rate
    pub ramp: Duration,
    /// a stable part shorter than this is `sample_short`
    pub min_steady: Duration,
    pub probe_every: Duration,
    /// a probe answered later than this counts as this (R8)
    pub probe_cap: Duration,
    /// probes a loaded stage needs before the queue rule may judge it
    pub min_probes: usize,
    /// node trial (R10): 2 connections for `trial_time`, a node still at 0 bytes after
    /// `trial_zero` is dropped for this run
    pub trial_time: Duration,
    pub trial_zero: Duration,
    pub trial_max: u64,
    pub single_max: u64,
    pub multi_max: u64,
    pub upload_max: u64,
    /// the CHILL download's own pool, on top of `total_max` (R5)
    pub chill_max: u64,
    pub total_max: u64,
    /// the largest upload POST (R7); a connection starts smaller and doubles while fast
    pub upload_chunk: usize,
}
pub const FULL: Limits = Limits {
    coarse: false,
    ping: true,
    single: true,
    upload: true,
    connections: 6,
    idle_time: Duration::from_secs(2),
    single_time: Duration::from_secs(8),
    multi_time: Duration::from_secs(10),
    upload_time: Duration::from_secs(10),
    ramp: Duration::from_secs(2),
    min_steady: Duration::from_secs(3),
    probe_every: Duration::from_millis(200),
    probe_cap: Duration::from_secs(2),
    min_probes: 20,
    trial_time: Duration::from_secs(1),
    trial_zero: Duration::from_secs(2),
    trial_max: 15 * MB,
    single_max: 200 * MB,
    multi_max: 400 * MB,
    upload_max: 150 * MB,
    chill_max: 400 * MB,
    total_max: 750 * MB,
    upload_chunk: 4_000_000,
};
pub const CAPPED: Limits = Limits {
    coarse: true,
    ping: false,
    single: false,
    upload: false,
    multi_time: Duration::from_secs(5),
    ramp: Duration::from_millis(500),
    min_steady: Duration::from_secs(1),
    multi_max: 30 * MB,
    total_max: 30 * MB,
    ..FULL
};
impl Limits {
    /// Everything the run may take, with room for discovery and node trials.
    fn time_cap(&self) -> Duration {
        if self.coarse {
            return self.multi_time + Duration::from_secs(3);
        }
        let mut t = self.multi_time * 2 + Duration::from_secs(20);
        if self.ping {
            t += self.idle_time;
        }
        if self.single {
            t += self.single_time;
        }
        if self.upload {
            t += self.upload_time;
        }
        t
    }
}
pub type OnDone = Box<dyn FnOnce(&SpeedTestProgress) + Send>;
pub type Admit<'a> = &'a dyn Fn(&AtomicBool) -> Result<(), (u16, Value)>;
pub fn claim_running(running: &AtomicBool) -> Result<(), (u16, Value)> {
    running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::Relaxed)
        .map(|_| ())
        .map_err(|_| (409, json!({"ok":false,"error":"test already running"})))
}
pub const SERVERS_URL: &str = "https://www.speedtest.net/api/js/servers?engine=js&limit=20";
pub const CHILL_PROXY: &str = "127.0.0.1:7894";
pub const CHILL_CONTROLLER: &str = "http://127.0.0.1:9999";
/// What one start asks for.
#[derive(Clone)]
struct Job {
    mode: Option<Provider>,
    id: Option<u64>,
    lim: Limits,
    chill: chill::Plan,
    ctx: RunContext,
}
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
            context: Arc::new(RunContext::default),
            chill_proxy: CHILL_PROXY.into(),
            chill_controller: CHILL_CONTROLLER.into(),
            last_pick: Arc::new(Mutex::new(None)),
        }
    }
    /// The core's view of the device, read at admission (roaming, cell, radio, net-tune).
    pub fn with_context(mut self, f: impl Fn() -> RunContext + Send + Sync + 'static) -> Self {
        self.context = Arc::new(f);
        self
    }
    /// Where CHILL's 7894 listener and mihomo's controller are (tests swap them).
    pub fn with_chill(mut self, proxy: &str, controller: &str) -> Self {
        self.chill_proxy = proxy.into();
        self.chill_controller = controller.trim_end_matches('/').into();
        self
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
    /// `{"provider"?, "server_id"?, "path"?: "direct"|"chill"}`. Roaming is the core's call.
    pub fn start(&self, body: &[u8], admit: Admit) -> (u16, Value) {
        #[derive(Deserialize)]
        struct Request {
            provider: Option<String>,
            server_id: Option<u64>,
            path: Option<String>,
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
        let chill = match req.path.as_deref() {
            None | Some("direct") => false,
            Some("chill") => true,
            Some(_) => {
                return (400, json!({"ok":false,"error":"Invalid speed-test path"}));
            }
        };
        self.begin(mode, req.server_id, FULL, chill, None, admit)
    }
    pub fn start_capped(&self, on_done: OnDone, admit: Admit) -> (u16, Value) {
        self.begin(None, None, CAPPED, false, Some(on_done), admit)
    }
    fn begin(
        &self,
        mode: Option<Provider>,
        id: Option<u64>,
        lim: Limits,
        chill: bool,
        on_done: Option<OnDone>,
        admit: Admit,
    ) -> (u16, Value) {
        if let Err(e) = admit(&self.running) {
            return e;
        }
        // Admission: the device as it is now decides the run's shape (R9).
        let ctx = (self.context)();
        let roaming_capped = ctx.roaming && !lim.coarse;
        let lim = if roaming_capped { CAPPED } else { lim };
        let asked_chill = chill;
        let chill = if !chill || lim.coarse {
            chill::Plan::Off
        } else if chill::running(&self.chill_proxy) {
            chill::Plan::Run
        } else {
            // never start mihomo for a test, never fall back to direct silently
            chill::Plan::NotRunning
        };
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
            mode: if lim.coarse { Mode::Coarse } else { Mode::Full },
            roaming_capped,
            // the path asked for (the contract), even when roaming leaves out the pair
            path: if asked_chill {
                PathKind::Chill
            } else {
                PathKind::Direct
            },
            steps: if chill == chill::Plan::Run {
                STEPS + 1
            } else {
                STEPS
            },
            connections: lim.connections,
            net_tune: ctx.net_tune.map(|on| if on { "on" } else { "off" }),
            radio: ctx.radio.clone(),
            ..SpeedTestProgress::idle()
        };
        let job = Job {
            mode,
            id,
            lim,
            chill,
            ctx,
        };
        let st = self.clone();
        std::thread::spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| st.work(&job)));
            if outcome.is_err() {
                st.fail(Failure {
                    code: "internal",
                    zh: "测速任务异常，请重试。".into(),
                    en: "Speed-test worker failed. Retry.".into(),
                });
            }
            // Capture this run before releasing the slot: a new start can reset progress.
            // Every transfer thread is joined inside `work`, so this is the final snapshot.
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
    fn env<'a>(&'a self, job: &'a Job, budget: &'a runner::Budget) -> runner::Env<'a> {
        runner::Env {
            progress: &self.progress,
            cancel: &self.cancel,
            lim: job.lim,
            budget,
            chill: chill::Env {
                plan: job.chill,
                proxy: &self.chill_proxy,
                controller: &self.chill_controller,
                context: &self.context,
                cell: job.ctx.cell.clone(),
            },
        }
    }
    /// Run one node; true when the run is over (complete or cancelled).
    fn attempt(
        &self,
        job: &Job,
        budget: &runner::Budget,
        provider: Provider,
        server: &TestServer,
        last: &mut Failure,
    ) -> bool {
        if self.cancelled() {
            return true;
        }
        {
            let mut p = self.progress.lock().unwrap();
            p.server = format!(
                "{} · {} ({})",
                provider.label(),
                server.sponsor,
                server.name
            );
            p.provider = Some(provider);
            p.server_name = server.name.clone();
            p.server_sponsor = server.sponsor.clone();
            p.clear_measurements();
            p.progress = 0;
        }
        match runner::run(server, &self.env(job, budget)) {
            Ok(out) => {
                if !self.cancelled() {
                    let verdict = (!job.lim.coarse && !out.sample_short)
                        .then(|| verdict::judge(&out.inputs(job.ctx.radio.as_ref())));
                    let mut p = self.progress.lock().unwrap();
                    p.phase = Phase::Complete;
                    p.progress = 100;
                    p.live_speed_mbps = 0.0;
                    p.stage = None;
                    p.step = p.steps;
                    p.verdict = verdict;
                    drop(p);
                    if !job.lim.coarse {
                        if let Some(cell) = job.ctx.cell.clone() {
                            *self.last_pick.lock().unwrap() = Some(Pick {
                                server: server.clone(),
                                at: Instant::now(),
                                cell,
                            });
                        }
                    }
                }
                true
            }
            Err(e) => {
                *last = e;
                false
            }
        }
    }
    /// A coarse run on the cell of a recent full run starts on that run's node (R10).
    fn reusable(&self, job: &Job) -> Option<TestServer> {
        if !job.lim.coarse || job.id.is_some() {
            return None;
        }
        let cell = job.ctx.cell.as_deref()?;
        let pick = self.last_pick.lock().unwrap();
        let p = pick.as_ref()?;
        (p.cell == cell
            && p.at.elapsed() < PICK_TTL
            && job.mode.is_none_or(|m| m == p.server.provider))
        .then(|| p.server.clone())
    }
    fn work(&self, job: &Job) {
        let budget = runner::Budget::new(job.lim, job.chill == chill::Plan::Run);
        let local = self.config.lock().unwrap().servers().unwrap_or_default();
        let providers = job
            .mode
            .map_or_else(|| vec![Provider::Ookla, Provider::Librespeed], |p| vec![p]);
        let mut last = Failure::service("no_servers", Provider::Ookla, "has no available servers");
        if let Some(server) = self.reusable(job) {
            if self.attempt(job, &budget, server.provider, &server, &mut last) {
                return;
            }
        }
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
                p.stage = None;
                p.step = 0;
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
                    let auto = job.mode.is_none() && job.id.is_none() && !job.lim.coarse;
                    let mut choices: Vec<_> = list
                        .into_iter()
                        .filter(|s| job.id.is_none_or(|i| i == s.id))
                        .take(match (job.id.is_some(), auto) {
                            (true, _) => 1,
                            (false, true) => 5,
                            (false, false) => 3,
                        })
                        .collect();
                    if choices.is_empty() {
                        last = Failure::service("no_servers", provider, "has no available servers");
                    } else if auto && choices.len() > 1 {
                        // R10: reachable by latency, then ranked by a short real download
                        choices = runner::pick(choices, &self.env(job, &budget));
                        if choices.is_empty() {
                            last = Failure::service(
                                "provider_unreachable",
                                provider,
                                "is unreachable",
                            );
                        }
                    }
                    for server in choices {
                        if self.attempt(job, &budget, provider, &server, &mut last) {
                            return;
                        }
                        // A manual provider selects one node unless a node ID was supplied; it never switches sources.
                        if job.mode.is_some() || budget.remaining().is_zero() {
                            break;
                        }
                    }
                }
                Err(e) => last = e,
            }
            if self.cancelled() {
                return;
            }
            if job.mode.is_none() && provider == Provider::Ookla {
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
        p.stage = None;
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
        p.stage = None;
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
        .proxy(None)
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
