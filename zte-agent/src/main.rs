mod action;
mod alerts;
mod at_cmd;
mod at_terminal;
mod auth;
mod cell;
mod charge_policy;
mod device_ext;
pub mod doh;
mod event_bus;
mod handlers;
mod health;
mod homemode;
mod lan_test;
mod modem_ext;
mod network_ext;
mod public;
mod qos;
mod router;
mod scenario;
mod scheduler;
mod server;
mod services;
mod esim;
mod chill;
mod clock;
mod static_files;
mod sim;
mod sms;
mod sms_forward;
mod speedtest;
mod system;
mod telephony;
mod ubus;
mod usb;
mod wifi;
mod wifi_radio;
mod wifi_scan;

use std::sync::Arc;

use event_bus::EventBus;
use handlers::AppState;

const DEFAULT_BIND: &str = "0.0.0.0:9090";
const DEFAULT_THREADS: usize = 2;

fn main() {
    let bind = std::env::var("ZTE_AGENT_BIND").unwrap_or_else(|_| DEFAULT_BIND.to_string());
    let threads: usize = std::env::var("ZTE_AGENT_THREADS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_THREADS);

    let state = Arc::new(AppState::new());

    // Password: ZTE_AGENT_PASSWORD (the old start_zte_agent.sh launcher), or
    // the file named by ZTE_AGENT_PASSWORD_FILE (the procd service). The
    // service passes a path, not the secret: procd shows every instance's env
    // in `ubus call service list`.
    if let Ok(pw) = std::env::var("ZTE_AGENT_PASSWORD") {
        state.auth.set_password(&pw);
    } else if let Ok(path) = std::env::var("ZTE_AGENT_PASSWORD_FILE") {
        match std::fs::read_to_string(&path).ok().as_deref().and_then(password_from_env_file) {
            Some(pw) => state.auth.set_password(pw),
            None => {
                // Refuse to run open: with no password every request is allowed.
                eprintln!("[main] no password in {path}; refusing to start without one");
                std::process::exit(78);
            }
        }
    }

    // Sidecar mode (`ZTE_AGENT_SIDECAR=1`): serve the HTTP API but start none
    // of the background workers. Required when validating a new build beside
    // the running one — a second full instance would double-forward every SMS,
    // fight the live instance over charging policy, and race it for the DoH
    // port. Pair with `ZTE_AGENT_BIND=0.0.0.0:9091`, since :9090 is taken and
    // the bind failure would otherwise make the whole test vacuous.
    let sidecar = std::env::var("ZTE_AGENT_SIDECAR").is_ok_and(|v| v != "0");

    // Opt the scenario engine back in while sidecarring, to exercise the one
    // worker that is actually under test. Safe only because an unconfigured
    // engine does nothing at all; two *configured* engines would fight over the
    // same uci keys, so only set this against a device whose engine config you
    // control.
    let sidecar_scenario = std::env::var("ZTE_AGENT_SCENARIO").is_ok_and(|v| v != "0");
    if sidecar && sidecar_scenario {
        state.scenario.start(Arc::clone(&state));
    }

    if !sidecar {
        // Event bus: single `ubus listen` process dispatches to subscribers
        let event_bus = EventBus::new();
        let sms_rx = event_bus.subscribe("zwrt_wms_status_event");
        let charger_rx = event_bus.subscribe("BSP_CHARGER_EVENT");
        let service_rx = event_bus.subscribe("zwrt_servicestatus");
        let wan_status_rx = event_bus.subscribe("router_event_wan_connect_status");
        event_bus.start();

        state.doh.auto_start();
        state.scheduler.start(Arc::clone(&state));
        state.charge_limit.start(charger_rx);
        state.sms_forward.start(sms_rx, service_rx, wan_status_rx);

        // Boot resets the engine and repairs Wi-Fi if it is down — a reboot
        // must never be able to leave you without the network this device
        // exists to provide. Runs on the engine's own thread, so a repair that
        // takes half a minute does not hold up the admin UI.
        state.scenario.start(Arc::clone(&state));
    }

    // Read-only device check (doctor.sh) on a timer, for the health page and
    // the touch screen's summary. Harmless in sidecar mode too.
    health::start();

    server::start(&bind, threads, state);
}

/// `ZTE_AGENT_PASSWORD=<value>` from the env file written by agent-auth.sh:
/// the value is taken literally to the end of the line (no shell quoting).
/// Empty counts as missing — "" would be accepted as a real password.
fn password_from_env_file(text: &str) -> Option<&str> {
    text.lines()
        .find_map(|l| l.strip_prefix("ZTE_AGENT_PASSWORD="))
        .map(|v| v.trim_end_matches('\r'))
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::password_from_env_file;

    #[test]
    fn password_file_parsing() {
        assert_eq!(password_from_env_file("ZTE_AGENT_PASSWORD=s3cr3t!#\n"), Some("s3cr3t!#"));
        assert_eq!(password_from_env_file("# c\nZTE_AGENT_PASSWORD=a=b\n"), Some("a=b"));
        assert_eq!(password_from_env_file("ZTE_AGENT_PASSWORD=x\r\n"), Some("x"));
        assert_eq!(password_from_env_file("ZTE_AGENT_PASSWORD=\n"), None);
        assert_eq!(password_from_env_file("OTHER=1\n"), None);
        assert_eq!(password_from_env_file(""), None);
    }
}
