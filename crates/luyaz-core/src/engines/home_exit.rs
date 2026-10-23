//! The home exit run by u60d: the engine the agent runs
//! (`crates/luyaz-home-exit`) with u60d as its host. Only with `LUYAZ_HOME_EXIT=1` (core.env;
//! one machine, one loop, one writer of chill.env's `CHILL_HOME_*` and `home-exit.json`). Off, nothing here runs and the
//! touch screen's GET reads the shadow as before (`home.rs`).
//!
//! The engine is blocking (ureq to mihomo, `chill.sh reload`, the 30 s trial), like the agent's:
//! its loop and jobs run on threads of their own, the handlers on the blocking pool.

use luyaz_api::chill as ch;
use luyaz_home_exit::Host;
use serde_json::Value;
use std::sync::Arc;

const BACKLIGHT: &str = "/sys/class/leds/led:lcd/brightness";

pub const ENV: &str = "LUYAZ_HOME_EXIT";

/// Whether u60d runs the home exit (`LUYAZ_HOME_EXIT=1`).
pub fn enabled() -> bool {
    std::env::var(ENV).is_ok_and(|v| v == "1")
}

/// The engine u60d runs, for [`sim_changed`] (set once at start-up when `LUYAZ_HOME_EXIT=1`).
static ENGINE: std::sync::OnceLock<std::sync::Weak<luyaz_home_exit::HomeExit>> =
    std::sync::OnceLock::new();

pub fn register(e: &Arc<luyaz_home_exit::HomeExit>) {
    let _ = ENGINE.set(Arc::downgrade(e));
}

/// A new SIM is online (`sim_follow`): the engine's next tick starts its miss count over.
pub fn sim_changed() {
    if let Some(e) = ENGINE.get().and_then(std::sync::Weak::upgrade) {
        e.sim_changed();
    }
}

/// u60d as the engine's host: files under the compat root, the signal and clients blocks from
/// the hub, alerts through the shared queue.
pub struct U60dHost {
    pub root: String,
    pub hub: Option<Arc<crate::block::Hub>>,
    pub mihomo: String,
    pub tailscale: Arc<dyn Fn() -> Option<Value> + Send + Sync>,
}

impl U60dHost {
    fn path(&self, p: &str) -> std::path::PathBuf {
        format!("{}{p}", self.root).into()
    }

    fn block(&self, name: &str) -> Option<Value> {
        self.hub
            .as_ref()?
            .view(name)
            .filter(|v| !v.stale)
            .map(|v| v.data.clone())
    }
}

/// `chill.sh reload` with a deadline: its process group is killed past `limit` and the tick goes
/// on with an error, so the engine's next tick still runs.
pub(crate) fn chill_reload_within(cmd: &str, limit: std::time::Duration) -> Result<(), String> {
    let mut c = std::process::Command::new("sh");
    c.arg("-c").arg(cmd);
    match luyaz_api::health::output_within_group(&mut c, limit) {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => {
            let s = format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            );
            Err(format!("reload failed: {}", ch::tail(&s, 6)))
        }
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
            eprintln!(
                "[home_exit] chill.sh reload timed out after {} s, killed",
                limit.as_secs()
            );
            Err(format!("reload timed out after {} s", limit.as_secs()))
        }
        Err(e) => Err(format!("reload failed: {e}")),
    }
}

impl Host for U60dHost {
    fn env_var(&self, name: &str) -> String {
        ch::env_var(
            &std::fs::read_to_string(self.path(ch::CHILL_ENV)).unwrap_or_default(),
            name,
        )
    }
    fn set_env_vars(&self, vars: &[(&str, &str)]) -> Result<(), String> {
        let p = self.path(ch::CHILL_ENV);
        let out = ch::env_with(&std::fs::read_to_string(&p).unwrap_or_default(), vars)?;
        // chill.env holds the subscription and mihomo's secret: keep its mode (the agent's
        // fsutil::atomic_write does too)
        let mode = std::fs::metadata(&p).ok().map(|m| m.permissions());
        crate::api::ops::atomic_write(&p, out.as_bytes())
            .map_err(|e| format!("cannot write chill.env: {e}"))?;
        if let Some(m) = mode {
            let _ = std::fs::set_permissions(&p, m);
        }
        Ok(())
    }
    fn chill_state(&self) -> Option<Value> {
        serde_json::from_str(&std::fs::read_to_string(self.path(ch::CHILL_STATE)).ok()?).ok()
    }
    fn chill_reload(&self) -> Result<(), String> {
        // bounded (resilience audit R7): a wedged reload would stall this engine's 30 s loop
        let cmd = format!("{} reload", self.path(ch::CHILL_SH).display());
        chill_reload_within(&cmd, crate::ops::chill::RELOAD_TIMEOUT)
    }
    fn tailscale_json(&self) -> Option<Value> {
        (self.tailscale)()
    }
    fn alert(&self, kind: &str, text: &str) {
        let a = luyaz_api::alerts::Alerts::at(&self.path(luyaz_api::alerts::DIR));
        let up = std::fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|s| s.split('.').next()?.trim().parse().ok())
            .unwrap_or(0);
        if let Err(e) = a.add(kind, text, luyaz_api::alerts::now(), up) {
            eprintln!("[home_exit] could not raise {kind}: {e}");
        }
    }
    fn where_now(&self) -> (Option<i64>, Option<bool>) {
        let Some(sig) = self.block("signal") else {
            return (None, None);
        };
        (
            sig.get("serving_mcc").and_then(Value::as_i64),
            sig.get("true_roaming").and_then(Value::as_bool),
        )
    }
    fn clients_now(&self) -> Option<i64> {
        self.block("clients")?.get("total")?.as_i64()
    }
    /// The backlight, as the long-term layer reads it (u60-radio.md §5).
    fn screen_on(&self) -> Option<bool> {
        let b = std::fs::read_to_string(self.path(BACKLIGHT)).ok()?;
        Some(b.trim().parse::<i64>().ok()? > 0)
    }
    /// Packets in the cell's last ≥ 30 s window (D8); none yet = unknown, no waiting.
    fn cell_busy(&self) -> Option<bool> {
        crate::cell_window::current().map(|w| w.tx_packets + w.rx_packets > 0)
    }
    /// The underlay gate for the proxy layer (forks `ip`; asked only after a missed probe):
    /// held = the miss is the cell's, not home's.
    fn cell_up(&self) -> Option<bool> {
        use crate::api::underlay::{Kind, Layer, ask};
        Some(!ask("home-exit", Layer::Proxy, Kind::Inferential).holds())
    }
    fn read(&self, path: &str) -> Option<String> {
        std::fs::read_to_string(self.path(path)).ok()
    }
    fn write(&self, path: &str, data: &[u8]) -> std::io::Result<()> {
        crate::api::ops::atomic_write(&self.path(path), data)
    }
    fn mihomo_api(&self) -> String {
        self.mihomo.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    #[test]
    fn a_hung_reload_returns_at_its_deadline() {
        let t0 = Instant::now();
        let e = super::chill_reload_within("sleep 3600", Duration::from_millis(300)).unwrap_err();
        assert!(e.contains("timed out"), "{e}");
        assert!(t0.elapsed() < Duration::from_secs(2));
        // the next tick runs as usual
        assert_eq!(
            super::chill_reload_within("true", Duration::from_secs(5)),
            Ok(())
        );
        assert!(
            super::chill_reload_within("echo no; exit 1", Duration::from_secs(5))
                .unwrap_err()
                .contains("no")
        );
    }
}
