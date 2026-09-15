use std::fs;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ubus;

const STORAGE_PATH: &str = "/data/local/tmp/charge_limit.json";
const POLL_ACTIVE_SECS: u64 = 60;
const POLL_IDLE_SECS: u64 = 300;
const DEFAULT_HYSTERESIS: u8 = 5;

const SYSFS_CAPACITY: &str = "/sys/class/power_supply/battery/capacity";
const SYSFS_STATUS: &str = "/sys/class/power_supply/battery/status";

fn default_hysteresis() -> u8 {
    DEFAULT_HYSTERESIS
}

#[derive(Serialize, Deserialize)]
struct Persisted {
    enabled: bool,
    limit: u8,
    #[serde(default = "default_hysteresis")]
    hysteresis: u8,
}

struct LimitState {
    enabled: bool,
    limit: u8,
    hysteresis: u8,
    manual_override: bool,
}

pub struct ChargeLimitEnforcer {
    inner: Mutex<LimitState>,
}

/// Check if charging is currently stopped via ubus (ground truth).
/// `direct_power_supply_mode: "enable"` = charging STOPPED (inverted naming).
fn is_charging_stopped() -> bool {
    ubus::call("zwrt_bsp.charger", "list", Some("{}"))
        .ok()
        .and_then(|v| {
            v["direct_power_supply_mode"]
                .as_str()
                .map(|s| s == "enable")
        })
        .unwrap_or(false)
}

/// Set charging state via ubus (inverted: "enable" = stop, "disable" = start)
fn set_charging(allow: bool) {
    let mode = if allow { "disable" } else { "enable" };
    let params = format!(r#"{{"direct_power_supply_mode":"{mode}"}}"#);
    let _ = ubus::call("zwrt_bsp.charger", "set", Some(&params));
}

/// Extract `charger_connect` from event payload.
/// Returns `Some(true)` for connected, `Some(false)` for disconnected, `None` if not present.
fn charger_connect_value(event: &Value) -> Option<bool> {
    match &event["charger_connect"] {
        Value::Number(n) => Some(n.as_u64() != Some(0)),
        Value::String(s) => Some(s != "0"),
        _ => None,
    }
}

impl ChargeLimitEnforcer {
    pub fn new() -> Self {
        let persisted = fs::read_to_string(STORAGE_PATH)
            .ok()
            .and_then(|s| serde_json::from_str::<Persisted>(&s).ok());

        let (enabled, limit, hysteresis) = match persisted {
            Some(p) => (p.enabled, p.limit.clamp(50, 100), p.hysteresis.clamp(1, 20)),
            None => (false, 100, DEFAULT_HYSTERESIS),
        };

        ChargeLimitEnforcer {
            inner: Mutex::new(LimitState {
                enabled,
                limit,
                hysteresis,
                manual_override: false,
            }),
        }
    }

    /// Start with event-driven charger events + fallback polling.
    pub fn start(self: &Arc<Self>, charger_events: mpsc::Receiver<Value>) {
        let enforcer = Arc::clone(self);
        std::thread::spawn(move || {
            enforcer.event_loop(charger_events);
        });
    }

    fn event_loop(&self, rx: mpsc::Receiver<Value>) {
        // Assume charger connected on startup so we enforce on first tick.
        // The first real BSP_CHARGER_EVENT will correct the state.
        let mut charger_connected = true;

        loop {
            // When charger is disconnected, block until next event (no polling needed)
            if !charger_connected {
                match rx.recv() {
                    Ok(event) => {
                        if charger_connect_value(&event) == Some(true) {
                            charger_connected = true;
                            eprintln!("[charge_policy] charger connected — resuming enforcement");
                            std::thread::sleep(std::time::Duration::from_millis(500));
                            let state = self.inner.lock().unwrap();
                            if state.enabled && !state.manual_override {
                                let (limit, hyst) = (state.limit, state.hysteresis);
                                drop(state);
                                self.enforce(limit, hyst);
                            }
                        }
                        continue;
                    }
                    Err(_) => {
                        eprintln!("[charge_policy] event channel disconnected, falling back to polling");
                        self.poll_loop();
                        return;
                    }
                }
            }

            // Charger connected — poll with timeout for periodic enforcement
            let (enabled, manual_override, limit, hysteresis) = {
                let state = self.inner.lock().unwrap();
                (state.enabled, state.manual_override, state.limit, state.hysteresis)
            };

            let timeout = if enabled && !manual_override {
                POLL_ACTIVE_SECS
            } else {
                POLL_IDLE_SECS
            };

            match rx.recv_timeout(std::time::Duration::from_secs(timeout)) {
                Ok(event) => {
                    if charger_connect_value(&event) == Some(false) {
                        charger_connected = false;
                        eprintln!("[charge_policy] charger disconnected — suspending enforcement");
                        // Re-enable charging so next plug-in starts normally
                        if is_charging_stopped() {
                            set_charging(true);
                            eprintln!("[charge_policy] re-enabled charging (was stopped by limit)");
                        }
                        continue;
                    }
                    // Other charger event while connected — enforce immediately
                    let state = self.inner.lock().unwrap();
                    if state.enabled && !state.manual_override {
                        let (limit, hyst) = (state.limit, state.hysteresis);
                        drop(state);
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        self.enforce(limit, hyst);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if enabled && !manual_override {
                        self.enforce(limit, hysteresis);
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    eprintln!("[charge_policy] event channel disconnected, falling back to polling");
                    self.poll_loop();
                    return;
                }
            }
        }
    }

    /// Pure polling fallback if event bus dies.
    fn poll_loop(&self) {
        loop {
            let state = self.inner.lock().unwrap();
            let (enabled, manual_override, limit, hysteresis) = (
                state.enabled, state.manual_override, state.limit, state.hysteresis,
            );
            drop(state);

            let interval = if enabled && !manual_override {
                self.enforce(limit, hysteresis);
                POLL_ACTIVE_SECS
            } else {
                POLL_IDLE_SECS
            };
            std::thread::sleep(std::time::Duration::from_secs(interval));
        }
    }

    /// Core enforcement logic: read capacity and stop/resume charging as needed.
    fn enforce(&self, limit: u8, hysteresis: u8) {
        let stopped = is_charging_stopped();

        let status = fs::read_to_string(SYSFS_STATUS).unwrap_or_default();
        if status.trim() == "Discharging" && !stopped {
            return;
        }

        let capacity: u8 = fs::read_to_string(SYSFS_CAPACITY)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);

        if capacity >= limit && !stopped {
            set_charging(false);
        } else if capacity <= limit.saturating_sub(hysteresis) && stopped {
            set_charging(true);
        }
    }

    pub fn get(&self) -> (bool, u8, u8, bool) {
        let state = self.inner.lock().unwrap();
        (state.enabled, state.limit, state.hysteresis, state.manual_override)
    }

    pub fn set(&self, enabled: bool, limit: u8, hysteresis: u8) -> Result<(), String> {
        if limit < 50 || limit > 100 {
            return Err("limit must be 50-100".into());
        }
        if hysteresis < 1 || hysteresis > 20 {
            return Err("hysteresis must be 1-20".into());
        }
        let mut state = self.inner.lock().unwrap();
        state.enabled = enabled;
        state.limit = limit;
        state.hysteresis = hysteresis;
        state.manual_override = false;
        self.save_locked(&state);
        drop(state);

        if enabled {
            self.enforce(limit, hysteresis);
        } else {
            set_charging(true);
        }

        Ok(())
    }

    pub fn set_manual_override(&self, override_on: bool) {
        let mut state = self.inner.lock().unwrap();
        state.manual_override = override_on;
    }

    fn save_locked(&self, state: &LimitState) {
        let p = Persisted {
            enabled: state.enabled,
            limit: state.limit,
            hysteresis: state.hysteresis,
        };
        if let Ok(json) = serde_json::to_string(&p) {
            let _ = fs::write(STORAGE_PATH, json);
        }
    }
}
