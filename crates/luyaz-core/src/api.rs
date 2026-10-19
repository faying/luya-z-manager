//! The touch screen's and luyaz-web's API in u60d (u60-platform.md Phase 4 step 8,
//! u60-touch-rewrite.md M6): the routes in the retired agent's shape (`{"ok","data"}`,
//! `{"ok":false,"error"}`), served at `/v2/api/...` on u60d's own loopback listener (the same
//! auth as `/control`: none on loopback). luyaz-web and the screen call it directly.
//!
//! Named `compat` until Phase 10 (§4), when the agent-era name went; the paths did not change.
//!
//! Until Phase 10 (X1) the agent handed these over part by part (`/tmp/zte-agent.handover`, the
//! `agent-retired` marker) behind `U60D_COMPAT` and `U60D_COMPAT_WRITES`; u60d now owns them all.
//!
//! Until M6c this was its own listener, 127.0.0.1:9462, with the agent's login; the background
//! duties (`resume`) started only when that port bound. Both are gone.
//!
//! Step 8a/8b/8c: the reads (`/api/screen`, `/api/alerts`, `/api/public/status`, `/api/health`,
//! GET `/api/services/chill/home`, `/api/netinfo`) rendered by u60-api from u60d's blocks, its
//! shadow replays and the agent's files (`status.rs`, `home.rs`, `netinfo.rs`).
//! Step 8d: the writes (`ops.rs`) through u60d's own `/control` (`control.rs`). Everything else
//! answers 404 `not_in_compat_yet`.

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::sync::Arc;
#[cfg(test)]
use tokio::net::TcpListener;
use luyaz_api::alerts::Alerts;

mod cell_detect;
mod charge;
mod health;
mod chill_jobs;
mod chill_page;
pub(crate) mod control;
mod device_admin;
mod esim;
mod exits;
pub(crate) mod finding;
mod home;
pub(crate) mod home_exit;
mod net_reads;
mod netinfo;
pub(crate) mod ops;
pub(crate) mod scenario;
mod scheduler;
mod sms_forward;
mod speed;
mod status;
mod sys_reads;
mod ts_reads;
mod watch;
mod wifi_reads;

/// Starts the background duties (they run with or without anyone calling the routes).
pub fn start(c: Compat) {
    tokio::spawn(async move { c.resume().await });
}

/// Where the state the replies need comes from besides the files (tests swap these).
pub struct Sources {
    /// u60d's blocks (the signal block's roaming fields)
    pub hub: Option<Arc<crate::block::Hub>>,
    /// mihomo's API (the home exit's fallback group)
    pub mihomo: String,
    /// `tailscale status --json` (blocking; run off the async threads)
    pub tailscale: Arc<dyn Fn() -> Option<Value> + Send + Sync>,
    /// one ubus read (`state::ubus`, through the collector)
    pub ubus: Arc<dyn Fn(&'static str, &'static str, Value) -> UbusReply + Send + Sync>,
    /// the Wi-Fi stations on every AP interface (`iw`; blocking)
    pub stations: Arc<dyn Fn() -> Vec<luyaz_api::netinfo::Station> + Send + Sync>,
    /// u60d's own `/control` (the screen's writes)
    pub control: Arc<control::Client>,
    /// where the speed test's server list comes from
    pub speed_servers: String,
    /// the eSIM engine's device (lpac, the shell, the reboot): a fake in tests
    pub esim: Arc<dyn luyaz_esim::Host>,
    /// u60d's `/v2/screen` view (netwatch's verdict, the diagnosis' signal layer)
    pub screen: Arc<dyn Fn() -> ScreenReply + Send + Sync>,
}

/// What [`Sources::screen`] answers.
pub type ScreenReply = std::pin::Pin<Box<dyn std::future::Future<Output = Option<Value>> + Send>>;

/// What [`Sources::ubus`] answers.
pub type UbusReply =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send>>;

/// No eSIM bundle (tests that do not use eSIM): lpac "not installed", nothing else runs.
#[cfg(test)]
pub struct NoCard;

#[cfg(test)]
impl luyaz_esim::Host for NoCard {
    fn installed(&self) -> bool {
        false
    }
    fn lpac(&self, _: &[&str], _: std::time::Duration) -> Result<Value, String> {
        Err("lpac not installed".into())
    }
    fn sh(&self, _: &str, _: std::time::Duration) -> String {
        String::new()
    }
    fn exists(&self, _: &str) -> bool {
        false
    }
    fn reboot(&self) {}
    fn data_up(&self) -> bool {
        false
    }
    fn wake(&self, _: bool) {}
    fn sleep(&self, _: std::time::Duration) {}
}

impl Sources {
    /// No blocks, no mihomo, no Tailscale (tests).
    #[cfg(test)]
    pub fn none() -> Self {
        Sources {
            hub: None,
            mihomo: "http://127.0.0.1:1".into(),
            tailscale: Arc::new(|| None),
            ubus: Arc::new(|_, _, _| Box::pin(async { Err("no ubus".to_string()) })),
            stations: Arc::new(Vec::new),
            control: Arc::new(control::Client::new(
                "http://127.0.0.1:1/control".into(),
                None,
            )),
            speed_servers: "http://127.0.0.1:1/servers".into(),
            esim: Arc::new(NoCard),
            screen: Arc::new(|| Box::pin(async { None })),
        }
    }

    /// `control`: u60d's own `/control` URL and the token it wants, if any; `screen`: its
    /// `/v2/screen` view.
    pub fn host(
        hub: Option<Arc<crate::block::Hub>>,
        control: (String, Option<String>),
        screen: Arc<dyn Fn() -> ScreenReply + Send + Sync>,
    ) -> Self {
        let ubus: Arc<dyn Fn(&'static str, &'static str, Value) -> UbusReply + Send + Sync> =
            Arc::new(|s, m, a| Box::pin(async move { crate::state::ubus(s, m, a).await }));
        Sources {
            hub,
            mihomo: std::env::var("LUYAZ_MIHOMO_API")
                .unwrap_or_else(|_| "http://127.0.0.1:9999".into()),
            tailscale: Arc::new(home::tailscale_status),
            ubus: Arc::new(|s, m, a| Box::pin(async move { crate::state::ubus(s, m, a).await })),
            stations: Arc::new(netinfo::wifi_stations),
            control: Arc::new(control::Client::new(control.0, control.1)),
            speed_servers: luyaz_speedtest::SERVERS_URL.into(),
            esim: esim::real_host(ubus),
            screen,
        }
    }
}

#[derive(Clone)]
pub struct Compat {
    /// prefix for every device path (`LUYAZ_HOST_ROOT` on the device, a temp dir in tests)
    pub(crate) root: Arc<String>,
    pub(crate) src: Arc<Sources>,
    ts: Arc<home::TsCache>,
    net: Arc<netinfo::NetCache>,
    pub(crate) clock: Arc<ops::Clock>,
    /// deep diagnosis' gate (D10), shared by the speed test, search/register and the diagnosis
    gate: Arc<std::sync::Mutex<luyaz_api::diagnose::Gate>>,
    /// the speed test engine (the agent's)
    speed: Arc<luyaz_speedtest::SpeedTest>,
    /// the eSIM engine (the agent's)
    esim: Arc<luyaz_esim::Esim>,
    /// netwatch, deep diagnosis and the APN monitor (9b)
    pub(crate) group: Arc<watch::Group>,
    /// the health check's runner (9d)
    health: Arc<luyaz_api::health::Runner>,
    /// SMS forwarding's engine (9e)
    pub(crate) sms: Arc<crate::engines::sms_forward::Forwarding>,
    /// the scenario engine (9h)
    pub(crate) scenario: Arc<crate::engines::scenario::Running>,
    /// how that engine reaches the device (`None`: never taken over, e.g. tests that do not ask)
    scenario_io: Option<crate::engines::scenario::MakeIo>,
    /// how scheduled jobs reach u60d's routes (`None`: the jobs never run here, e.g. tests)
    scheduler_io: Option<Arc<dyn crate::engines::scheduler::Caller>>,
    /// the exit lookups run here (off in tests that do not ask)
    exits: bool,
    chill: Arc<chill_jobs::ChillJobs>,
    /// manual-first's engine (9g)
    pub(crate) manual_first: Arc<crate::engines::manual_first::Running>,
    /// the home exit, when u60d runs it (`LUYAZ_HOME_EXIT=1`, step 9)
    home_exit: Option<Arc<luyaz_home_exit::HomeExit>>,
    /// the writes and the background duties (on the device: [`Compat::from_host`]; tests without
    /// them look at the reads alone)
    pub(crate) writes: bool,
    /// the AT port and the signal the device-admin routes use (batch 5c)
    admin: device_admin::Host,
}

impl Compat {
    pub fn new(root: &str, src: Sources) -> Self {
        let root = root.trim_end_matches('/').to_string();
        let speed = Arc::new(
            luyaz_speedtest::SpeedTest::new(&src.speed_servers)
                .with_config_path(format!("{root}/speedtest.json")),
        );
        let net: Arc<netinfo::NetCache> = Arc::default();
        let gate: Arc<std::sync::Mutex<luyaz_api::diagnose::Gate>> = Arc::default();
        let esim = Arc::new(luyaz_esim::Esim::new(src.esim.clone()));
        let group = Arc::new(watch::Group::new(
            &root,
            src.hub.clone(),
            net.clone(),
            speed.running_flag(),
            src.stations.clone(),
            gate.clone(),
            &src.mihomo,
            esim.clone(),
        ));
        let health = health::runner(&root);
        Compat {
            root: Arc::new(root),
            speed,
            esim,
            src: Arc::new(src),
            ts: Arc::default(),
            net,
            clock: Arc::default(),
            gate,
            group,
            health,
            sms: Arc::default(),
            scenario: Arc::default(),
            scenario_io: None,
            scheduler_io: None,
            exits: false,
            chill: Arc::default(),
            manual_first: Arc::default(),
            home_exit: None,
            writes: false,
            admin: device_admin::Host::default(),
        }
    }

    /// The device-admin routes' AT port and signal (tests).
    #[cfg(test)]
    pub fn with_admin(mut self, h: device_admin::Host) -> Self {
        self.admin = h;
        self
    }

    /// With the writes and the background duties (the device; tests choose).
    pub fn with_writes(mut self, on: bool) -> Self {
        self.writes = on;
        self
    }

    /// u60d runs the home exit (step 9): its own machine, loop and switches.
    pub fn with_home_exit(mut self, on: bool) -> Self {
        self.home_exit = on.then(|| {
            Arc::new(luyaz_home_exit::HomeExit::new(Arc::new(crate::engines::home_exit::U60dHost {
                root: self.root.to_string(),
                hub: self.src.hub.clone(),
                mihomo: self.src.mihomo.clone(),
                tailscale: self.src.tailscale.clone(),
            })))
        });
        self
    }

    /// Tests: a home exit engine built by the test (fake mihomo, fast clock).
    #[cfg(test)]
    pub fn with_home_exit_engine(mut self, e: Arc<luyaz_home_exit::HomeExit>) -> Self {
        self.home_exit = Some(e);
        self
    }

    /// The scenario engine reaches the device this way (step 9h).
    pub fn with_scenario_io(mut self, make: crate::engines::scenario::MakeIo) -> Self {
        self.scenario_io = Some(make);
        self
    }

    /// Scheduled jobs run here (8d-1d), their calls through `io`.
    pub fn with_scheduler_io(mut self, io: Arc<dyn crate::engines::scheduler::Caller>) -> Self {
        self.scheduler_io = Some(io);
        self
    }

    /// The exit lookups run here (8d-1c ⑥).
    pub fn with_exits(mut self, on: bool) -> Self {
        self.exits = on;
        self
    }

    /// Writes run `scale` times faster (tests).
    #[cfg(test)]
    pub fn with_clock(mut self, scale: u64) -> Self {
        self.clock = Arc::new(ops::Clock::scaled(scale));
        self
    }

    /// At start, with writes on: finish what u60d's guard marker says was in flight.
    pub async fn resume(&self) {
        if let Some(e) = &self.home_exit {
            e.start();
        }
        if self.writes {
            tokio::spawn(watch::sample(self.clone()));
            tokio::spawn(watch::apn_watch(self.clone()));
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            crate::plugin::charge_policy::set_executor(tx);
            tokio::spawn(charge::run(self.clone(), rx));
            tokio::spawn(health::run(self.clone()));
            tokio::spawn(crate::engines::sms_forward::run(self.clone()));
            tokio::spawn(crate::engines::guard_duties::run(self.clone()));
            tokio::spawn(crate::engines::sentinel_watch::run(self.clone()));
            if let Some(make) = self.scenario_io.clone() {
                tokio::spawn(crate::engines::scenario::run(self.clone(), make));
            }
            if let Some(io) = self.scheduler_io.clone() {
                crate::engines::scheduler::spawn(self.clone(), io);
            }
            if self.exits {
                exits::spawn(self.clone());
            }
            tokio::spawn(crate::engines::manual_first::run(self.clone()));
            ops::resume(self.clone()).await;
        }
    }

    pub fn from_host(
        hub: Option<Arc<crate::block::Hub>>,
        control: (String, Option<String>),
        screen: Arc<dyn Fn() -> ScreenReply + Send + Sync>,
    ) -> Self {
        // the scenario engine's own calls go to u60d's listener, next to its /control (M6c)
        let v2 = format!("{}/v2", control.0.trim_end_matches("/control"));
        let token = control.1.clone();
        Compat::new(
            &crate::state::host_path(""),
            Sources::host(hub, control, screen),
        )
        .with_writes(true)
        .with_scenario_io(crate::engines::scenario::device_io(v2.clone()))
        .with_scheduler_io(Arc::new(crate::engines::scheduler::Http { base: v2, token }))
        .with_exits(true)
        .with_home_exit(crate::engines::home_exit::enabled())
        .with_chill_slot(crate::ops::chill::slot())
    }

    /// CHILL jobs on this job slot (the device's is shared with u60d's CHILL transactions, O10).
    pub fn with_chill_slot(mut self, slot: Arc<std::sync::atomic::AtomicBool>) -> Self {
        self.chill = Arc::new(chill_jobs::ChillJobs::sharing(slot));
        self
    }

    /// CHILL jobs with a short chill.sh deadline (tests of the R7 kill).
    #[cfg(test)]
    pub fn with_chill_limit(mut self, limit: std::time::Duration) -> Self {
        self.chill = Arc::new(chill_jobs::ChillJobs::sharing_within(Arc::default(), limit));
        self
    }

    /// A device path under [`Compat::root`].
    pub(crate) fn path(&self, p: &str) -> std::path::PathBuf {
        format!("{}{p}", self.root).into()
    }

    pub(crate) fn alerts_dir(&self) -> std::path::PathBuf {
        self.path(luyaz_api::alerts::DIR)
    }

    /// The routes for u60d's own loopback listener, nested at `/v2` (`/v2/api/...`): the
    /// listener's own auth applies, the peer address comes from u60d's listener.
    pub fn v2_router(self) -> Router {
        self.routes()
            .fallback(not_yet)
            // a path served for other methods (GET fast-boot, POST power-save…): "not here" too
            .method_not_allowed_fallback(not_yet)
            .layer(middleware::from_fn(peer_as_connect_info))
            .with_state(self)
    }

    fn routes(&self) -> Router<Compat> {
        let mut r = Router::new()
            .route("/api/public/status", get(status::public_status))
            .route("/api/health", get(health::get))
            .route("/api/health/crashlog", get(health::crashlog))
            .route("/api/screen", get(screen))
            .route("/api/alerts", get(alerts_get))
            .route("/api/netinfo/neighbors/scan", post(neighbors_scan))
            .route("/api/netinfo", get(netinfo::get))
            // 8d-1b: the agent's last reads luyaz-web used (agent path and reply shape)
            .route("/api/cpu", get(sys_reads::cpu))
            .route("/api/device", get(sys_reads::device))
            // 8d-2a: the touch screen's battery card (agent path and reply shape)
            .route("/api/battery", get(sys_reads::battery))
            .route("/api/modem/status", get(sys_reads::modem_status))
            .route("/api/usb/status", get(sys_reads::usb_status))
            .route("/api/sms/capacity", get(sys_reads::sms_capacity))
            .route("/api/router/wan-ipv6", get(sys_reads::wan_ipv6))
            .route("/api/network/clients", get(net_reads::clients))
            .route("/api/network/qos", get(net_reads::qos))
            .route("/api/wifi/status", get(wifi_reads::status))
            .route("/api/wifi/power-save", get(wifi_reads::power_save))
            .route(
                "/api/cell/signal-detect/progress",
                get(cell_detect::progress),
            )
            .route("/api/cell/signal-detect/results", get(cell_detect::results))
            // batch 5b: Tailscale's page (agent paths and reply shapes)
            .route("/api/services/tailscale", get(ts_reads::status))
            .route("/api/services/tailscale/log", get(ts_reads::log));
        // batch 5c: the device-admin reads (its writes and the AT port with the writes, below)
        r = device_admin::reads(r);
        // batch 5b: CHILL's page reads (the subscription list's PUT is a write, below)
        r = r
            .route("/api/services/chill/log", get(chill_page::log))
            .route("/api/services/chill/providers", get(chill_page::providers_list));
        r = r.route(
            "/api/services/chill/home",
            if self.home_exit.is_some() {
                get(home_exit::get).put(home_exit::put)
            } else {
                get(home::get)
            },
        );
        if self.writes {
            // netwatch, deep diagnosis and the APN monitor (9b)
            let group = Router::new()
                .route("/api/modem/register", post(ops::register))
                .route("/api/modem/netselect/auto", post(ops::select_auto))
                .route("/api/netinfo/scan", post(ops::scan))
                .route("/api/netinfo/apn", post(ops::apn_use))
                .route(
                    "/api/agent/roam-cheap",
                    get(ops::roam_cheap_get).put(ops::roam_cheap_put),
                )
                .route(
                    "/api/speedtest/config",
                    get(speed::config).put(speed::configure),
                )
                .route("/api/speedtest/servers", get(speed::servers))
                .route("/api/speedtest/start", post(speed::start))
                .route("/api/speedtest/progress", get(speed::progress))
                .route("/api/speedtest/stop", post(speed::stop))
                .route("/api/esim/status", get(esim::status))
                .route("/api/esim/profiles", get(esim::profiles))
                .route("/api/esim/notifications", get(esim::notifications))
                .route("/api/esim/job", get(esim::job))
                .route("/api/esim/switch", post(esim::switch))
                .route("/api/esim/download", post(esim::download))
                .route("/api/esim/nickname", post(esim::nickname))
                .route("/api/esim/delete", post(esim::delete))
                .route(
                    "/api/esim/notifications/process",
                    post(esim::notifications_process),
                )
                .route(
                    "/api/diagnose",
                    get(watch::diagnose_get).post(watch::diagnose_start),
                )
                .route("/api/diagnose/speed", post(watch::diagnose_speed))
                .route("/api/diagnose/feedback", post(watch::diagnose_feedback))
                .route("/api/diagnose/dismiss", post(watch::diagnose_dismiss));
            // the charge limit and the battery estimate (9c): only with the policies on
            let charge = Router::new()
                .route(
                    "/api/device/charge-control",
                    get(charge::get).put(charge::put),
                )
                .route_layer(middleware::from_fn_with_state(self.clone(), charge::owned));
            // SMS forwarding (9e)
            let sms = Router::new()
                .route(
                    "/api/sms/forward/config",
                    get(sms_forward::config_get).put(sms_forward::config_set),
                )
                .route(
                    "/api/sms/forward/rules",
                    post(sms_forward::rules_create)
                        .put(sms_forward::rules_update)
                        .delete(sms_forward::rules_delete),
                )
                .route(
                    "/api/sms/forward/rules/toggle",
                    axum::routing::put(sms_forward::rules_toggle),
                )
                .route("/api/sms/forward/test", post(sms_forward::test))
                .route("/api/sms/forward/log", get(sms_forward::log_get))
                .route("/api/sms/forward/log/clear", post(sms_forward::log_clear))
                .route("/api/sms/forward/retry", post(sms_forward::retry));
            // the scenario engine (9h): its routes and the actions only it needs
            #[allow(unused_mut)]
            let mut scen = Router::new()
                .route("/api/scenario", get(scenario::get).put(scenario::put))
                .route("/api/scenario/template", get(scenario::template))
                .route("/api/scenario/enabled", axum::routing::put(scenario::enabled))
                .route("/api/scenario/scan", get(scenario::scan))
                .route("/api/scenario/log", get(scenario::log))
                .route("/api/wifi/guest", axum::routing::put(scenario::guest))
                .route("/api/device/power-save", axum::routing::put(scenario::power_save));
            scen = scen
                .route("/api/services/chill", get(chill_page::status))
                .route("/api/services/chill/regions", axum::routing::put(chill_page::regions))
                .route(
                    "/api/services/chill/bypass",
                    get(chill_page::bypass_get).put(chill_page::bypass_set),
                );
            let scen = scen.route_layer(middleware::from_fn_with_state(self.clone(), scenario::owned));
            // scheduled jobs (8d-1d)
            let sched = Router::new()
                .route(
                    "/api/scheduler/jobs",
                    get(scheduler::list)
                        .post(scheduler::create)
                        .put(scheduler::update)
                        .delete(scheduler::delete),
                )
                .route("/api/scheduler/jobs/toggle", axum::routing::put(scheduler::toggle));
            r = r
                .merge(group)
                .merge(charge)
                .merge(sms)
                .merge(scen)
                .merge(sched)
                .merge(device_admin::writes())
                .route("/api/device/reboot", post(device_reboot))
                .route("/api/alerts/read", post(alerts_read))
                .route("/api/alerts/sms", axum::routing::put(alerts_sms))
                // 8d-1b: the firmware's signal detection (modem writes: only with writes on)
                .route("/api/cell/signal-detect/start", post(cell_detect::start))
                .route("/api/cell/signal-detect/stop", post(cell_detect::stop))
                .route("/api/scenario/pin", post(ops::scenario_pin));
            r = r
                .route(
                    "/api/services/chill/exit",
                    axum::routing::put(ops::chill_exit),
                )
                .route("/api/services/chill/enable", post(chill_jobs::enable))
                .route("/api/services/chill/disable", post(chill_jobs::disable))
                .route(
                    "/api/services/chill/profile",
                    axum::routing::put(chill_jobs::profile),
                )
                .route("/api/services/chill/job", get(chill_jobs::job))
                .route(
                    "/api/services/chill/providers",
                    axum::routing::put(chill_jobs::providers),
                )
                // batch 5b
                .route(
                    "/api/services/chill/providers/refresh",
                    post(chill_page::providers_refresh),
                );
        }
        r
    }
}

/// u60d's listener records the peer as `ConnectInfo<`[`crate::conn::Peer`]`>`; the handlers read
/// `ConnectInfo<SocketAddr>` (loopback = the front screen).
async fn peer_as_connect_info(mut request: Request, next: Next) -> Response {
    let peer = request
        .extensions()
        .get::<axum::extract::ConnectInfo<crate::conn::Peer>>()
        .map(|ci| ci.0.0);
    if let Some(p) = peer {
        request
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(p));
    }
    next.run(request).await
}

/// Tests: the routes on a plain listener at `/api/...`.
#[cfg(test)]
pub async fn serve(listener: TcpListener, compat: Compat) -> std::io::Result<()> {
    axum::serve(
        listener,
        compat
            .v2_router()
            .into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
}

pub(crate) fn reply(status: u16, body: Value) -> Response {
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        Json(body),
    )
        .into_response()
}

/// GET /api/screen (the agent's `handlers::screen`): the battery estimate from u60d's own
/// battery_eta.
async fn screen() -> Response {
    reply(
        200,
        json!({"ok": true, "data": {"v": 1, "battery": crate::plugin::battery_eta::report()}}),
    )
}

fn uptime_now() -> Option<u64> {
    std::fs::read_to_string(crate::state::host_path("/proc/uptime"))
        .ok()?
        .split_whitespace()
        .next()?
        .split('.')
        .next()?
        .parse()
        .ok()
}

/// `since` and `limit` from a query string; `since` absent = the plain summary (u60-mcp.md R7).
fn cursor_query(q: Option<&str>) -> Option<(u64, usize)> {
    let mut since = None;
    let mut limit = luyaz_api::alerts::CURSOR_LIMIT;
    for (k, v) in q?.split('&').filter_map(|p| p.split_once('=')) {
        match k {
            "since" => since = v.parse().ok(),
            "limit" => limit = v.parse().unwrap_or(limit),
            _ => {}
        }
    }
    Some((since?, limit))
}

/// GET /api/alerts (the agent's `alerts::alerts_get`). With `?since=<seq>[&limit=]` the cursor
/// read instead (u60-mcp.md R4); without it the reply stays exactly what the screen and web read.
async fn alerts_get(
    State(c): State<Compat>,
    axum::extract::RawQuery(q): axum::extract::RawQuery,
) -> Response {
    let offset = utc_offset().await;
    let dir = c.alerts_dir();
    let cursor = cursor_query(q.as_deref());
    let data = tokio::task::spawn_blocking(move || {
        let a = Alerts::at(&dir);
        match cursor {
            Some((since, limit)) => a.since(since, limit, luyaz_api::alerts::now(), offset),
            None => a.summary(luyaz_api::alerts::now(), uptime_now(), offset),
        }
    })
    .await
    .unwrap_or(Value::Null);
    reply(200, json!({"ok": true, "data": data}))
}

/// PUT /api/alerts/sms (8d-1c, the agent's `alerts::alerts_sms_set`): the alert SMS number and the
/// abroad switch; answers the summary's `sms`.
async fn alerts_sms(State(c): State<Compat>, body: Bytes) -> Response {
    let offset = utc_offset().await;
    let dir = c.alerts_dir();
    tokio::task::spawn_blocking(move || {
        let a = Alerts::at(&dir);
        if let Err((code, e)) = luyaz_api::alerts::set_sms(&a, &body) {
            return reply(code, json!({"ok": false, "error": e}));
        }
        health::refresh_soon(&c);
        let sms = a.summary(luyaz_api::alerts::now(), uptime_now(), offset)["sms"].clone();
        reply(200, json!({"ok": true, "data": sms}))
    })
    .await
    .unwrap_or_else(|_| reply(500, json!({"ok": false, "error": "internal"})))
}

/// POST /api/device/reboot (the agent's `device_ext::device_reboot`): `sync`, then
/// `device.reboot` as the caller's source (the stock way, through zte_topsw_mc); the plain
/// `system reboot` only when that one failed.
async fn device_reboot(State(c): State<Compat>, req: Request) -> Response {
    let source = charge::source(&req);
    let _ = tokio::task::spawn_blocking(|| std::process::Command::new("sync").status()).await;
    let w = c.writer(source);
    let r = match w.send("device.reboot", &json!({})).await {
        luyaz_api::control::Reply::Failed { error, .. } => w
            .send("vendor.call", &json!({"object": "system", "method": "reboot", "args": {}}))
            .await
            .into_result()
            .map_err(|e| format!("{error}; system reboot: {e}")),
        r => r.into_result(),
    };
    match r {
        Ok(data) => reply(200, json!({"ok": true, "data": data})),
        Err(e) => reply(503, json!({"ok": false, "error": e})),
    }
}

/// POST /api/alerts/read `{"seq": N}` (the agent's `alerts::alerts_read`): never backwards, never
/// past the newest event; then the health check runs again in the background when u60d owns it
/// (9d; the agent's does the same while it owns it).
async fn alerts_read(State(c): State<Compat>, body: Bytes) -> Response {
    let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let Some(seq) = parsed.get("seq").and_then(Value::as_u64) else {
        return reply(
            400,
            json!({"ok": false, "error": "expected {\"seq\": number}"}),
        );
    };
    let dir = c.alerts_dir();
    tokio::task::spawn_blocking(move || {
        let a = Alerts::at(&dir);
        let newest = a.events().last().map(|e| e.seq).unwrap_or(0);
        let read = seq.min(newest).max(a.read_u64("read"));
        match a.write_whole("read", &format!("{read}\n")) {
            Ok(()) => {
                // the doctor's alerts check changes with it (9d; the agent's does the same)
                health::refresh_soon(&c);
                reply(200, json!({"ok": true, "data": {"read": read}}))
            }
            Err(e) => reply(
                500,
                json!({"ok": false, "error": format!("write read cursor: {e}")}),
            ),
        }
    })
    .await
    .unwrap_or_else(|_| reply(500, json!({"ok": false, "error": "internal"})))
}

/// POST /api/netinfo/neighbors/scan (the agent's `netinfo::neighbors_scan`): switched off.
async fn neighbors_scan() -> Response {
    reply(
        410,
        json!({"ok": false, "error": "原厂扫描会断网且拿不到数据，已停用",
               "error_en": "Off: the stock scan drops data, finds nothing"}),
    )
}

async fn not_yet(request: Request) -> Response {
    reply(
        404,
        json!({"ok": false, "error": "not_in_compat_yet", "path": request.uri().path()}),
    )
}

/// Seconds the device clock runs ahead of UTC, as the agent's `clock::utc_offset` (uci, cached 5
/// minutes).
pub async fn utc_offset() -> i64 {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    static CACHE: Mutex<Option<(Instant, i64)>> = Mutex::new(None);
    if let Some((at, v)) = *CACHE.lock().unwrap_or_else(|e| e.into_inner())
        && at.elapsed() < Duration::from_secs(300)
    {
        return v;
    }
    let a = crate::state::uci_value("zwrt_zte_sntp.settings.time_from_utc").await;
    let b = crate::state::uci_value("zwrt_zte_sntp.settings.timezone").await;
    let some = |s: &str| (!s.is_empty()).then(|| s.to_string());
    let v = luyaz_api::clock::utc_offset(some(&a).as_deref(), some(&b).as_deref());
    // what new alerts are recorded with (u60-mcp.md R10)
    luyaz_api::alerts::set_current_offset(v);
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), v));
    v
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_charge;
#[cfg(test)]
mod tests_health;
#[cfg(test)]
mod tests_guard_duties;
#[cfg(test)]
mod tests_sms_forward;
#[cfg(test)]
mod tests_scheduler;
#[cfg(test)]
mod tests_exits;
#[cfg(test)]
mod tests_scenario;
#[cfg(test)]
mod tests_manual_first;
#[cfg(test)]
mod tests_ops;
#[cfg(test)]
mod tests_watch;
#[cfg(test)]
mod tests_sys_reads;
#[cfg(test)]
mod tests_net_reads;
#[cfg(test)]
mod tests_wifi_reads;
#[cfg(test)]
mod tests_cell_detect;
#[cfg(test)]
mod tests_ts_reads;
#[cfg(test)]
mod tests_device_admin;
#[cfg(test)]
mod tests_engine_routes;
#[cfg(test)]
mod tests_chill_reads;
