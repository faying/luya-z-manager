//! The scenario engine in u60d.
//!
//! The engine is the agent's own (`crates/luyaz-scenario`); u60d runs it with its writes on,
//! on a system thread of its own: an apply can block for minutes (the Wi-Fi lock's 150 s, then
//! the APs coming up), which the single runtime thread must never do.
//!
//! How it reaches the device ([`U60dIo`]): the Wi-Fi switch (`luyaz_wifi`) writes through u60d's
//! own `/control` (its Wi-Fi lock sees the caller is the lock's holder, D3); the other actions and
//! the reads after them (`verify_field`, `verify_job`, a restore's `read_path`) go to these routes
//! on u60d's own loopback listener (`/v2/api/...`), as source `scenario` (and `X-U60-Restore` inside a
//! restore). uci reads are `uci get`, the SIM and the charger come from u60d's blocks.
//!
//! u60d holds the lease `/tmp/scenario.owner` while its engine runs. Starting runs the engine's
//! bootsafe first, so a switch u60d was killed in the middle of is repaired.
//!
//! A wedged engine (no heartbeat for `luyaz_scenario::WEDGED`, on the monotonic clock: a device that
//! slept is not wedged) makes u60d exit, so procd starts it again: the Wi-Fi lock is a flock and
//! goes only with the process.

use luyaz_scenario::{Engine as ScenarioEngine, Io};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use crate::api::Compat;

/// The watchdog's look: the first one starts the engine; after that only a wedge
/// ([`luyaz_scenario::WEDGED`], 15 min) or an ended thread is looked for (R18-P1-04: was 2 s).
const LOOK_SECS: u64 = 30;

// ── how the engine reaches the device from u60d ────────────────────────────

fn http(secs: u64) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(secs)))
        .http_status_as_error(false)
        .build()
        .into()
}

/// One `/control` write as source `scenario`, blocking (the engine's thread).
fn control(
    url: &str,
    token: Option<&str>,
    action: &str,
    params: &Value,
    secs: u64,
) -> luyaz_api::control::Reply {
    let body = json!({"action": action, "source": "scenario", "params": params});
    let mut req = http(secs)
        .post(url)
        .header("Content-Type", "application/json");
    if let Some(t) = token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    match req.send(body.to_string()) {
        Ok(mut r) => {
            let status = r.status().as_u16();
            let v: Value = r.body_mut().read_json().unwrap_or(Value::Null);
            luyaz_api::control::parse(status, &v)
        }
        Err(e) => luyaz_api::control::Reply::Unreachable {
            error: e.to_string(),
            maybe_done: true,
        },
    }
}

/// The Wi-Fi switch's writes through u60d's own `/control`.
pub struct WifiHost {
    pub control: String,
    pub token: Option<String>,
}

impl luyaz_wifi::Host for WifiHost {
    fn uci_get(&self, key: &str) -> Option<String> {
        let out = luyaz_wifi::output_within(
            std::process::Command::new("uci").args(["-q", "get", key]),
            Duration::from_secs(5),
        )
        .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn write_aps(&self, ap_2g: bool, ap_5g: bool) -> Result<Option<String>, String> {
        let flag = |on: bool| if on { "0" } else { "1" };
        let mut set = json!({luyaz_wifi::AP_2G: flag(ap_2g), luyaz_wifi::AP_5G: flag(ap_5g)});
        if !ap_2g && !ap_5g {
            // both off: guests too — a guest left on alone is a layout the firmware cannot run, and the
            // core refuses it (luyaz_wifi::layout). best_effort: a firmware may lack a guest section.
            set["wireless.guest_2g.disabled"] = json!("1");
            set["wireless.guest_5g.disabled"] = json!("1");
        }
        let params = json!({"set": set, "reload": true, "best_effort": !ap_2g && !ap_5g});
        // the lock's wait (150 s) is outside /control's queue; this covers it and the reload
        match control(
            &self.control,
            self.token.as_deref(),
            "wifi.apply",
            &params,
            240,
        ) {
            luyaz_api::control::Reply::Done { result, .. } => {
                Ok(result["reload_error"].as_str().map(str::to_string))
            }
            r => Err(r.into_result().err().unwrap_or_default()),
        }
    }

    fn vendor_on(&self) -> Result<(), String> {
        control(
            &self.control,
            self.token.as_deref(),
            "wifi.set_module",
            &json!({"enabled": 1}),
            240,
        )
        .into_result()
        .map(|_| ())
    }

    fn reload(&self) -> Result<Value, String> {
        control(
            &self.control,
            self.token.as_deref(),
            "wifi.reload",
            &json!({}),
            240,
        )
        .into_result()
    }
}

pub struct U60dIo {
    root: String,
    /// the agent-shaped routes (`http://127.0.0.1:<port>/v2`)
    base: String,
    /// sent when set (u60d's listener with a token)
    token: Option<String>,
    hub: Option<Arc<crate::block::Hub>>,
    wifi: WifiHost,
    active: Arc<AtomicBool>,
}

impl U60dIo {
    /// A call to the agent-shaped routes, as source `scenario`.
    fn call(&self, method: &str, path: &str, body: &[u8]) -> (u16, Value) {
        let url = format!("{}{path}", self.base);
        let a = http(60);
        let auth = self.token.as_ref().map(|t| format!("Bearer {t}"));
        let restore = luyaz_scenario::in_restore();
        let r = if method == "GET" {
            let mut q = a
                .get(&url)
                .header(luyaz_api::headers::SOURCE_HEADER, "scenario");
            if let Some(h) = &auth {
                q = q.header("Authorization", h);
            }
            if restore {
                q = q.header(luyaz_api::headers::RESTORE_HEADER, "1");
            }
            q.call()
        } else {
            let q = match method {
                "PUT" => a.put(&url),
                "DELETE" => a.delete(&url).force_send_body(),
                _ => a.post(&url),
            };
            let mut q = q
                .header("Content-Type", "application/json")
                .header(luyaz_api::headers::SOURCE_HEADER, "scenario");
            if let Some(h) = &auth {
                q = q.header("Authorization", h);
            }
            if restore {
                q = q.header(luyaz_api::headers::RESTORE_HEADER, "1");
            }
            q.send(body)
        };
        match r {
            Ok(mut resp) => (
                resp.status().as_u16(),
                resp.body_mut().read_json().unwrap_or(Value::Null),
            ),
            Err(e) => (
                502,
                json!({"ok": false, "error": format!("luyaz-core: {e}")}),
            ),
        }
    }
}

impl Io for U60dIo {
    fn path(&self, p: &str) -> String {
        format!("{}{p}", self.root)
    }

    fn active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    /// In short steps: taken back, the loop ends at once instead of after the 15 s tick.
    fn idle(&self, d: Duration) {
        let end = std::time::Instant::now() + d;
        while self.active() {
            let left = end.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return;
            }
            std::thread::sleep(left.min(Duration::from_millis(200)));
        }
    }

    fn wifi(&self) -> &dyn luyaz_wifi::Host {
        &self.wifi
    }

    fn wifi_lock(&self, wait: Duration) -> Result<luyaz_wifi::WifiLock, String> {
        luyaz_wifi::lock_at(&self.path(luyaz_wifi::LOCK_FILE), wait)
    }

    fn wifi_apply(&self, ap_2g: bool, ap_5g: bool) -> Result<(), String> {
        let lk = self.wifi_lock(luyaz_wifi::ENGINE_WAIT)?;
        luyaz_wifi::apply_locked(&self.wifi, &lk, ap_2g, ap_5g).map(|_| ())
    }

    fn wifi_put_back(&self, set: serde_json::Map<String, Value>) {
        let _ = control(
            &self.wifi.control,
            self.wifi.token.as_deref(),
            "wifi.apply",
            &json!({"set": set, "reload": false, "best_effort": true}),
            240,
        );
    }

    fn exec(&self, method: &str, path: &str, body: &[u8]) -> Result<(), String> {
        let (status, resp) = self.call(method, path, body);
        if status >= 400 {
            return Err(resp
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("HTTP {status}")));
        }
        Ok(())
    }

    fn get(&self, path: &str) -> (u16, Value) {
        self.call("GET", path, b"")
    }

    fn set_auto_sleep(&self, enabled: bool) {
        let _ = control(
            &self.wifi.control,
            self.wifi.token.as_deref(),
            "vendor.call",
            &json!({"object": luyaz_scenario::SLEEP_UBUS_OBJ, "method": "enableAutoSleep", "args": {"switch": enabled}}),
            30,
        );
    }

    /// u60d's charger and battery blocks (the agent's `charger_connected`).
    fn charging(&self) -> Option<bool> {
        let hub = self.hub.as_ref()?;
        hub.view("charger").filter(|v| !v.stale)?;
        let b = hub.view("battery").filter(|v| !v.stale)?;
        b.data
            .get("charger_connect")
            .and_then(Value::as_i64)
            .map(|v| v != 0)
    }

    /// u60d's SIM block (its last value when stale: a SIM does not change its country unseen).
    fn sim_mcc(&self) -> Option<String> {
        let sim = self.hub.as_ref()?.view("sim")?;
        luyaz_scenario::policy::mcc_from_core_sim(&luyaz_scenario::Json::from(sim.data.clone()))
    }
}

// ── running it ──────────────────────────────────────────────────────────────

/// The engine u60d runs, if any.
#[derive(Default)]
pub struct Running {
    now: Mutex<Option<Live>>,
}

struct Live {
    engine: Arc<ScenarioEngine>,
    thread: std::thread::JoinHandle<()>,
}

impl Running {
    /// The engine once it runs.
    pub(crate) fn engine(&self) -> Option<Arc<ScenarioEngine>> {
        self.now
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|l| l.engine.clone())
    }
}

/// How the engine's [`Io`] is made (the device's, or a test's fake).
pub type MakeIo = Arc<dyn Fn(&Compat, Arc<AtomicBool>) -> Arc<dyn Io> + Send + Sync>;

/// The device's: this touch layer on `port`, `/control` as configured.
/// `base`: where the agent-shaped routes are, `http://127.0.0.1:<u60d's port>/v2` on the device.
/// The token sent: `/control`'s, when u60d's listener wants one (none on the device's loopback).
pub fn device_io(base: String) -> MakeIo {
    Arc::new(move |c: &Compat, active: Arc<AtomicBool>| {
        let (control_url, control_token) = c.src.control.target();
        Arc::new(U60dIo {
            root: c.root.to_string(),
            base: base.clone(),
            token: control_token.clone(),
            hub: c.src.hub.clone(),
            wifi: WifiHost {
                control: control_url,
                token: control_token,
            },
            active,
        }) as Arc<dyn Io>
    })
}

/// Wedged: the engine's last heartbeat (monotonic, suspend not counted) is older than the longest
/// legitimate gap. Before its first one (just started): not judged.
pub(crate) fn wedged(since_beat: Option<Duration>) -> bool {
    since_beat.is_some_and(|d| d > luyaz_scenario::WEDGED)
}

/// Starts the engine (with the writes on); the watchdog while it runs.
pub async fn run(c: Compat, make: MakeIo) {
    let mut look = tokio::time::interval(c.clock.secs(LOOK_SECS));
    loop {
        look.tick().await;
        let mut g = c.scenario.now.lock().unwrap_or_else(|e| e.into_inner());
        match g.as_ref() {
            None => {
                let active = Arc::new(AtomicBool::new(true));
                let io = make(&c, active.clone());
                let lease = c.path(luyaz_scenario::LEASE_FILE);
                if let Err(e) = luyaz_api::lease::write(&lease, "u60d") {
                    eprintln!("[compat] scenario: could not write the lease: {e}");
                }
                let engine = Arc::new(ScenarioEngine::new(io));
                engine.log("u60d runs the engine");
                let e2 = engine.clone();
                let thread = match std::thread::Builder::new()
                    .name("scenario".into())
                    .spawn(move || e2.run())
                {
                    Ok(t) => t,
                    Err(e) => {
                        eprintln!("[compat] scenario: could not start the engine thread: {e}");
                        luyaz_api::lease::remove(&lease);
                        continue;
                    }
                };
                *g = Some(Live { engine, thread });
                eprintln!("[compat] scenario: u60d runs it");
            }
            Some(l) if l.thread.is_finished() => {
                // the loop never ends by itself: it panicked (tests; on the device a panic aborts)
                eprintln!("[compat] scenario: the engine thread ended; exiting for procd");
                std::process::exit(70);
            }
            Some(l) => {
                // wedged: a flock goes only with the process, and the guard cannot take over Wi-Fi
                // while it is held
                let since = l.engine.since_beat();
                if wedged(since) {
                    eprintln!(
                        "[compat] scenario: no heartbeat for {}s; exiting so procd starts u60d again",
                        since.map_or(0, |d| d.as_secs())
                    );
                    std::process::exit(70);
                }
            }
        }
    }
}
