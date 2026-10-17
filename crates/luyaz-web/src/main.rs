//! luyaz-web: the admin web process (Phase 8, u60-web.md W1/E1, 8c-A W13/W14, 8d-2).
//!
//! ```text
//! browser ─▶ luyaz-web :9090 ─(one SSE)──▶ u60d 127.0.0.1:9460 /v2/events
//!              │                    ├─(writes)───▶ u60d /control (source=web)
//!              │                    └─(reads)────▶ u60d /v2/api/* (agent.rs)
//!              └─ login: the password file; anything not served here: 404 (proxy.rs)
//! ```
//! The agent is retired (8d-2), and its development fallback with it (Phase 10 X4): without
//! `--password-file` nobody can log in.
//!
//! `--bind` may be given several times (or as a comma list; env `LUYAZ_WEB_BIND`): each address
//! is bound on its own. One that is in use (the agent still holding :9090 during the switch) is
//! tried again every [`REBIND`] in the background, with one log line when it fails first and one
//! when it is bound; at least one must bind at start, else luyaz-web exits.
//!
//! `luyaz-web serve` is the page server; `luyaz-web mock --replay <capture>` stands in for u60d.

mod agent;
mod alerts_page;
mod apn;
mod assets;
mod auth;
mod bandlock;
mod celllock;
mod changes;
mod clients;
mod config_page;
mod cpu;
mod device_info;
mod diagnose;
mod esim;
mod follow;
mod health_page;
mod hub;
mod i18n;
mod icons;
mod mock;
mod netsel;
mod oplogo;
mod opbar;
mod pages;
mod proxy;
mod qci;
mod router_filter;
mod router_firewall;
mod router_lan;
mod router_qos;
mod router_vpn;
mod routes;
mod scheduler;
mod chill_page;
mod chill_proxy;
mod home;
mod scenario;
mod settings;
mod tailscale;
mod at_term;
mod device_power;
mod dns;
mod enable_adb;
mod processes;
mod sim_page;
mod stc;
mod stk;
mod shell;
mod signal_detect;
mod sms;
mod sms_compose;
mod sms_forward;
mod speedtest;
mod sse;
mod topbar;
mod trends;
mod usb;
mod view;
mod wifi_guest;
mod wifi_main;
mod write;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::{get, post};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "luyaz-web", about = "U60 Pro (MU5250) admin web")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Serve the pages.
    Serve {
        /// Listen addresses; repeat the flag or separate with commas.
        #[arg(
            long,
            env = "LUYAZ_WEB_BIND",
            value_delimiter = ',',
            default_value = "127.0.0.1:8080"
        )]
        bind: Vec<String>,
        /// u60d base URL.
        #[arg(long, default_value = "http://127.0.0.1:9460")]
        upstream: String,
        /// Drop the upstream connection this long after the last tab closes (pass line 3; N = 10 s
        /// until the owner picks another number).
        #[arg(long, default_value_t = 10)]
        idle_secs: u64,
        /// The admin password file (`ZTE_AGENT_PASSWORD=…`, the device's /data/luyaz/web.env):
        /// logins are checked here, nowhere else. Without it every login is refused.
        #[arg(long, env = "LUYAZ_WEB_PASSWORD_FILE")]
        password_file: Option<std::path::PathBuf>,
        /// Where sessions are kept so they outlive a restart (hash and expiry only). Default with a
        /// password file (the device): /data/luyaz/web-sessions; without one: in memory only.
        #[arg(long, env = "LUYAZ_WEB_SESSION_FILE")]
        session_file: Option<std::path::PathBuf>,
        /// Pages served here, comma-separated; any other page is a 404. Drop one to switch it off
        /// without a rebuild.
        #[arg(
            long,
            env = "LUYAZ_WEB_PAGES",
            default_value = "/signal,/charts,/functions,/system,/trends,/clients,/tools/speedtest,/router/signal-detect,/tools/diagnose,/device-info,/router/qci,/tools/cpu,/sms,/sms/compose,/changes,/usb,/bandlock,/router/celllock,/router/apn,/router/network-mode,/router/mobile-network,/router/qos,/router/vpn,/router/telemetry,/router/firewall,/router/lan,/router/wifi,/router/wifi-guest,/alerts,/sms/forward,/scheduler,/router/esim,/health,/settings,/config,/,/services/tailscale,/services/chill,/router/scenario,/router/device,/tools/enable-adb,/router/sim,/router/stc,/router/stk,/tools/at,/tools/processes,/router/dns"
        )]
        pages: String,
        /// Operator logo files (`<slug>.svg`/`.png`). Trademarks: read from disk, never embedded.
        /// On the device they live with luyaz (8d-2; the old web's copy was /data/admin/operator-logos).
        #[arg(long, env = "LUYAZ_WEB_LOGO_DIR", default_value = "/data/luyaz/operator-logos")]
        logo_dir: std::path::PathBuf,
        /// The CHILL dashboard (zashboard) files served at /chill-ui/ (chill_proxy.rs).
        #[arg(long, env = "LUYAZ_WEB_CHILL_UI_DIR", default_value = "/data/chill/ui")]
        chill_ui_dir: std::path::PathBuf,
    },
    /// Stand in for u60d: replay a recorded /v2/events capture.
    Mock {
        #[arg(long, default_value = "127.0.0.1:9460")]
        bind: String,
        #[arg(long)]
        replay: std::path::PathBuf,
        /// Replay speed (2 = twice as fast).
        #[arg(long, default_value_t = 1.0)]
        speed: f64,
    },
}

pub fn app(
    upstream: &str,
    idle: Duration,
    pages: &str,
    password_file: Option<std::path::PathBuf>,
    session_file: Option<std::path::PathBuf>,
    logo_dir: std::path::PathBuf,
) -> Router {
    let session_file = session_file.or_else(|| {
        password_file
            .as_ref()
            .map(|_| std::path::PathBuf::from("/data/luyaz/web-sessions"))
    });
    let hub = hub::Hub::new();
    tokio::spawn(hub::run(hub.clone(), format!("{upstream}/v2/events"), idle));
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("http client");
    let side_kick = Arc::new(tokio::sync::Notify::new());
    tokio::spawn(topbar::poll(
        hub.clone(),
        http.clone(),
        upstream.to_string(),
        side_kick.clone(),
    ));
    let state = Arc::new(pages::App {
        hub,
        control_url: format!("{upstream}/control"),
        u60d_url: upstream.to_string(),
        side_kick,
        http,
        proxy_http: reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("proxy client"),
        sessions: auth::Sessions::with_file(session_file),
        password_file,
        pages: pages
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect(),
        kick: tokio::sync::broadcast::channel(16).0,
        logo_dir,
    });
    Router::new()
        .route("/oplogo/{slug}", get(oplogo::serve))
        .route("/signal", get(pages::signal))
        .route("/signal/events", get(pages::signal_events))
        .route("/retry", post(pages::retry))
        .route("/roam/ask", post(pages::roam_ask))
        .route("/roam/cancel", post(pages::roam_cancel))
        .route("/roam/set", post(pages::roam_set))
        .route("/trends", get(trends::page))
        .route("/trends/events", get(trends::events))
        .route("/clients", get(clients::page))
        .route("/clients/events", get(clients::events))
        .route("/clients/refresh", post(clients::refresh))
        .route("/tools/speedtest", get(speedtest::page))
        .route("/tools/speedtest/events", get(speedtest::events))
        .route("/tools/speedtest/start", post(speedtest::start))
        .route("/tools/speedtest/stop", post(speedtest::stop))
        .route("/tools/speedtest/servers", post(speedtest::servers))
        .route("/tools/speedtest/refresh", post(speedtest::refresh))
        .route("/router/signal-detect", get(signal_detect::page))
        .route("/router/signal-detect/events", get(signal_detect::events))
        .route("/router/signal-detect/start", post(signal_detect::start))
        .route("/router/signal-detect/stop", post(signal_detect::stop))
        .route(
            "/router/signal-detect/results",
            post(signal_detect::results),
        )
        .route("/tools/diagnose", get(diagnose::page))
        .route("/tools/diagnose/events", get(diagnose::events))
        .route("/tools/diagnose/start", post(diagnose::start))
        .route("/tools/diagnose/speed", post(diagnose::speed))
        .route("/tools/diagnose/feedback", post(diagnose::feedback))
        .route("/tools/diagnose/refresh", post(diagnose::refresh))
        .route("/device-info", get(device_info::page))
        .route("/device-info/events", get(device_info::events))
        .route("/device-info/refresh", post(device_info::refresh))
        .route("/router/qci", get(qci::page))
        .route("/router/qci/events", get(qci::events))
        .route("/router/qci/refresh", post(qci::refresh))
        .route("/tools/cpu", get(cpu::page))
        .route("/tools/cpu/events", get(cpu::events))
        .route("/tools/cpu/refresh", post(cpu::refresh))
        .route("/sms", get(sms::page))
        .route("/sms/events", get(sms::events))
        .route("/sms/refresh", post(sms::refresh))
        .route("/sms/read", post(sms::read_set))
        .route("/sms/read/cancel", post(sms::read_cancel))
        .route("/sms/open", post(sms::open))
        .route("/sms/delete/ask", post(sms::delete_ask))
        .route("/sms/delete", post(sms::delete_set))
        .route("/sms/delete/cancel", post(sms::delete_cancel))
        .route("/sms/compose", get(sms_compose::page))
        .route("/sms/compose/ask", post(sms_compose::ask))
        .route("/sms/compose/send", post(sms_compose::send))
        .route("/sms/compose/cancel", post(sms_compose::cancel))
        .route("/changes", get(changes::page))
        .route("/changes/events", get(changes::events))
        .route("/changes/refresh", post(changes::refresh))
        .route("/changes/undo/ask", post(changes::ask))
        .route("/changes/undo", post(changes::undo))
        .route("/changes/undo/cancel", post(changes::cancel))
        .route("/bandlock", get(bandlock::page))
        .route("/bandlock/events", get(bandlock::events))
        .route("/bandlock/lte/ask", post(bandlock::lte_ask))
        .route("/bandlock/lte/set", post(bandlock::lte_set))
        .route("/bandlock/lte/cancel", post(bandlock::lte_cancel))
        .route("/bandlock/nr/ask", post(bandlock::nr_ask))
        .route("/bandlock/nr/set", post(bandlock::nr_set))
        .route("/bandlock/nr/cancel", post(bandlock::nr_cancel))
        .route("/bandlock/reset/ask", post(bandlock::reset_ask))
        .route("/bandlock/reset/set", post(bandlock::reset_set))
        .route("/bandlock/reset/cancel", post(bandlock::reset_cancel))
        .route("/router/celllock", get(celllock::page))
        .route("/router/celllock/events", get(celllock::events))
        .route("/router/celllock/nr/ask", post(celllock::nr_ask))
        .route("/router/celllock/nr/set", post(celllock::nr_set))
        .route("/router/celllock/nr/cancel", post(celllock::nr_cancel))
        .route("/router/celllock/lte/ask", post(celllock::lte_ask))
        .route("/router/celllock/lte/set", post(celllock::lte_set))
        .route("/router/celllock/lte/cancel", post(celllock::lte_cancel))
        .route("/router/celllock/reset/ask", post(celllock::reset_ask))
        .route("/router/celllock/reset/set", post(celllock::reset_set))
        .route("/router/celllock/reset/cancel", post(celllock::reset_cancel))
        .route("/router/apn", get(apn::page))
        .route("/router/apn/events", get(apn::events))
        .route("/router/apn/mode/ask", post(apn::mode_ask))
        .route("/router/apn/mode/set", post(apn::mode_set))
        .route("/router/apn/mode/cancel", post(apn::mode_cancel))
        .route("/router/apn/v6/ask", post(apn::v6_ask))
        .route("/router/apn/v6/set", post(apn::v6_set))
        .route("/router/apn/v6/cancel", post(apn::v6_cancel))
        .route("/router/apn/enable/ask", post(apn::enable_ask))
        .route("/router/apn/enable/set", post(apn::enable_set))
        .route("/router/apn/delete/ask", post(apn::delete_ask))
        .route("/router/apn/delete/set", post(apn::delete_set))
        .route("/router/apn/row/cancel", post(apn::row_cancel))
        .route("/router/apn/form", post(apn::form_open))
        .route("/router/apn/form/close", post(apn::form_close))
        .route("/router/apn/save/ask", post(apn::save_ask))
        .route("/router/apn/save/set", post(apn::save_set))
        .route("/router/apn/save/cancel", post(apn::save_cancel))
        .route("/router/apn/pick/ask", post(apn::pick_ask))
        .route("/router/apn/pick/set", post(apn::pick_set))
        .route("/router/apn/pick/cancel", post(apn::pick_cancel))
        .route("/router/network-mode", get(netsel::mode_page))
        .route("/router/network-mode/events", get(netsel::mode_events))
        .route("/router/network-mode/ask", post(netsel::mode_ask))
        .route("/router/network-mode/set", post(netsel::mode_set))
        .route("/router/network-mode/cancel", post(netsel::mode_cancel))
        .route("/router/mobile-network", get(netsel::net_page))
        .route("/router/mobile-network/events", get(netsel::net_events))
        .route("/router/mobile-network/data/ask", post(netsel::data_ask))
        .route("/router/mobile-network/data/set", post(netsel::data_set))
        .route("/router/mobile-network/data/cancel", post(netsel::data_cancel))
        .route("/router/mobile-network/roam/ask", post(netsel::roam_ask))
        .route("/router/mobile-network/roam/set", post(netsel::roam_set))
        .route("/router/mobile-network/roam/cancel", post(netsel::roam_cancel))
        .route("/router/mobile-network/scan/ask", post(netsel::scan_ask))
        .route("/router/mobile-network/scan/set", post(netsel::scan_set))
        .route("/router/mobile-network/scan/cancel", post(netsel::scan_cancel))
        .route("/router/mobile-network/reg/ask", post(netsel::reg_ask))
        .route("/router/mobile-network/reg/set", post(netsel::reg_set))
        .route("/router/mobile-network/reg/cancel", post(netsel::reg_cancel))
        .route("/router/mobile-network/auto/ask", post(netsel::auto_ask))
        .route("/router/mobile-network/auto/set", post(netsel::auto_set))
        .route("/router/mobile-network/auto/cancel", post(netsel::auto_cancel))
        .route("/router/mobile-network/reboot/ask", post(netsel::reboot_ask))
        .route("/router/mobile-network/reboot/set", post(netsel::reboot_set))
        .route("/router/mobile-network/reboot/cancel", post(netsel::reboot_cancel))
        .route("/usb", get(usb::page))
        .route("/usb/events", get(usb::events))
        .route("/usb/refresh", post(usb::refresh))
        .route("/usb/mode/ask", post(usb::mode_ask))
        .route("/usb/mode/set", post(usb::mode_set))
        .route("/usb/mode/cancel", post(usb::mode_cancel))
        .route("/usb/pb/ask", post(usb::pb_ask))
        .route("/usb/pb/set", post(usb::pb_set))
        .route("/usb/pb/cancel", post(usb::pb_cancel))
        .route("/charts", get(shell::hub))
        .route("/functions", get(shell::hub))
        .route("/system", get(shell::hub))
        .route("/shell/events", get(topbar::events))
        .route("/alerts/read", post(topbar::read))
        .route("/ops/notice", post(opbar::notice_ack))
        .route("/lang", post(shell::set_lang))
        .route("/login", get(auth::login_page).post(auth::login))
        .route("/logout", post(auth::logout))
        .route("/health", get(health_page::dispatch))
        .route("/a/{name}", get(assets::serve))
        // batch 4 (Wi-Fi and LAN): each page mounts its own routes
        .merge(router_qos::routes())
        .merge(router_vpn::routes())
        .merge(router_filter::routes())
        .merge(router_firewall::routes())
        .merge(router_lan::routes())
        .merge(wifi_guest::routes())
        .merge(wifi_main::routes())
        // batch 5a (the pages whose data u60d already has)
        .merge(alerts_page::routes())
        .merge(sms_forward::routes())
        .merge(scheduler::routes())
        .merge(esim::routes())
        .merge(health_page::routes())
        .merge(home::routes())
        .merge(tailscale::routes())
        .merge(scenario::routes())
        .merge(chill_page::routes())
        .merge(chill_proxy::routes())
        .merge(device_power::routes())
        .merge(sim_page::routes())
        .merge(stc::routes())
        .merge(stk::routes())
        .merge(at_term::routes())
        .merge(processes::routes())
        .merge(enable_adb::routes())
        .merge(dns::routes())
        .merge(settings::routes())
        .merge(config_page::routes())
        // the gate answers anything not served here with a 404 before it gets this far
        .fallback(|| async { axum::http::StatusCode::NOT_FOUND })
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            proxy::gate,
        ))
        .with_state(state)
}

/// How often an address that was in use is tried again.
const REBIND: Duration = Duration::from_secs(10);

/// Bind `addr` again every `every` until it works (Some) or `stop` turns true (None). Logs only
/// when it is bound, or when the reason it is not changes.
async fn rebind(
    addr: String,
    every: Duration,
    mut why: String,
    mut stop: tokio::sync::watch::Receiver<bool>,
) -> Option<tokio::net::TcpListener> {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(every) => {}
            _ = stop.wait_for(|s| *s) => return None,
        }
        match tokio::net::TcpListener::bind(&addr).await {
            Ok(l) => {
                eprintln!("[luyaz-web] listening on {addr} (was busy)");
                return Some(l);
            }
            Err(e) if e.to_string() != why => {
                eprintln!("[luyaz-web] bind {addr}: {e}; trying again every {}s", every.as_secs());
                why = e.to_string();
            }
            Err(_) => {}
        }
    }
}

/// Bind every address; serve each one bound. An address in use is retried in the background
/// ([`rebind`]); any other failure gives that address up. None bound at start: exit.
async fn serve_all(binds: Vec<String>, router: Router, every: Duration) {
    let (stop_tx, stop) = tokio::sync::watch::channel(false);
    let mut tasks = Vec::new();
    let serve = |l: tokio::net::TcpListener, router: Router, mut stop: tokio::sync::watch::Receiver<bool>| async move {
        let addr = l.local_addr().map(|a| a.to_string()).unwrap_or_default();
        if let Err(e) = axum::serve(
            l,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            let _ = stop.wait_for(|s| *s).await;
        })
        .await
        {
            eprintln!("[luyaz-web] serve {addr}: {e}");
        }
    };
    let mut bound = 0;
    for addr in binds.iter().map(|b| b.trim()).filter(|b| !b.is_empty()) {
        match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => {
                eprintln!("[luyaz-web] listening on {addr}");
                bound += 1;
                tasks.push(tokio::spawn(serve(l, router.clone(), stop.clone())));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                eprintln!("[luyaz-web] bind {addr}: {e}; trying again every {}s", every.as_secs());
                let (addr, why, stop, router) = (addr.to_string(), e.to_string(), stop.clone(), router.clone());
                tasks.push(tokio::spawn(async move {
                    if let Some(l) = rebind(addr, every, why, stop.clone()).await {
                        serve(l, router, stop).await;
                    }
                }));
            }
            Err(e) => eprintln!("[luyaz-web] bind {addr}: {e}; not listening there"),
        }
    }
    if bound == 0 {
        eprintln!("[luyaz-web] no address could be bound ({}); exiting", binds.join(", "));
        std::process::exit(1);
    }
    let _ = tokio::signal::ctrl_c().await;
    let _ = stop_tx.send(true);
    for t in tasks {
        let _ = t.await;
    }
}

/// `<name> <version> (<commit>)`: the commit from LUYAZ_COMMIT (tools/luyaz ship passes it)
fn version_line(name: &str) -> String {
    match option_env!("LUYAZ_COMMIT").filter(|c| !c.is_empty()) {
        Some(c) => format!(
            "{name} {} ({})",
            env!("CARGO_PKG_VERSION"),
            &c[..c.len().min(7)]
        ),
        None => format!("{name} {}", env!("CARGO_PKG_VERSION")),
    }
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("{}", version_line("luyaz-web"));
        return;
    }
    let cli = Cli::parse();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async move {
        let (binds, router) = match cli.cmd {
            Cmd::Serve {
                bind,
                upstream,
                idle_secs,
                password_file,
                session_file,
                pages,
                logo_dir,
                chill_ui_dir,
            } => {
                chill_proxy::set_ui_dir(chill_ui_dir);
                (
                    bind,
                    app(
                        upstream.trim_end_matches('/'),
                        Duration::from_secs(idle_secs),
                        &pages,
                        password_file,
                        session_file,
                        logo_dir,
                    ),
                )
            }
            Cmd::Mock {
                bind,
                replay,
                speed,
            } => {
                let raw = std::fs::read_to_string(&replay)
                    .unwrap_or_else(|e| panic!("{}: {e}", replay.display()));
                (vec![bind], mock::router(&raw, speed))
            }
        };
        serve_all(binds, router, REBIND).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_takes_repeats_and_commas() {
        let c = Cli::try_parse_from([
            "luyaz-web", "serve", "--bind", "0.0.0.0:9090,0.0.0.0:8080", "--bind", "127.0.0.1:1",
        ])
        .unwrap();
        let Cmd::Serve { bind, logo_dir, .. } = c.cmd else { panic!() };
        assert_eq!(bind, ["0.0.0.0:9090", "0.0.0.0:8080", "127.0.0.1:1"]);
        assert_eq!(logo_dir, std::path::Path::new("/data/luyaz/operator-logos"));
    }

    #[tokio::test]
    async fn a_busy_address_is_bound_once_it_is_free() {
        let held = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = held.local_addr().unwrap().to_string();
        let (tx, rx) = tokio::sync::watch::channel(false);
        let t = tokio::spawn(rebind(addr.clone(), Duration::from_millis(20), String::new(), rx));
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(!t.is_finished(), "still busy");
        drop(held);
        let l = tokio::time::timeout(Duration::from_secs(2), t).await.unwrap().unwrap();
        assert_eq!(l.unwrap().local_addr().unwrap().to_string(), addr);
        // stopped while waiting: gives up
        let held = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let busy = held.local_addr().unwrap().to_string();
        let (tx2, rx2) = tokio::sync::watch::channel(false);
        let t = tokio::spawn(rebind(busy, Duration::from_millis(20), String::new(), rx2));
        tx2.send(true).unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(2), t).await.unwrap().unwrap().is_none());
        drop(tx);
    }
}
