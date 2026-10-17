//! CHILL's manual-first (u60-platform.md Phase 4 step 9g; the agent's until Phase 10).
//!
//! With u60d's writes on it runs the loop (`crates/luyaz-manual-first`) on a thread of its own, with
//! u60d's netwatch (it asks whether the cell itself is alive) and the same mihomo, saving the state
//! file on every change. The status page and the exit here read the live engine. It holds the
//! lease `/tmp/manual-first.owner` while its engine runs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use luyaz_manual_first::{Host, ManualFirst};

use crate::api::Compat;


struct U60dHost {
    root: String,
    mihomo: String,
    watch: Arc<luyaz_netwatch::Watch>,
    active: Arc<AtomicBool>,
}

impl Host for U60dHost {
    fn mihomo_api(&self) -> String {
        self.mihomo.clone()
    }

    fn chill_running(&self) -> bool {
        std::fs::read_to_string(format!("{}{}", self.root, luyaz_api::chill::CHILL_STATE))
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .is_some_and(|v| v["state"] == "running")
    }

    fn cell_alive(&self) -> bool {
        self.watch.cell_alive(luyaz_netwatch::now_unix())
    }

    fn direct_dns_ms(&self) -> Option<u32> {
        self.watch.direct_dns_ms()
    }

    /// The shadow records are the agent's.
    fn tick(&self, _record: serde_json::Value) {}

    fn active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    /// In short steps: taken back, the loop ends at once instead of after the 10 s tick.
    fn sleep(&self, d: Duration) {
        let end = std::time::Instant::now() + d;
        while self.active() {
            let left = end.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return;
            }
            std::thread::sleep(left.min(Duration::from_millis(200)));
        }
    }
}

/// The engine u60d runs, if any.
#[derive(Default)]
pub struct Running {
    now: Mutex<Option<Engine>>,
}

struct Engine {
    mf: Arc<ManualFirst>,
    active: Arc<AtomicBool>,
    /// runs for the life of the process
    _thread: std::thread::JoinHandle<()>,
}

impl Running {
    /// The live engine while u60d owns it (not while it is stopping).
    pub(crate) fn live(&self) -> Option<Arc<ManualFirst>> {
        let g = self.now.lock().unwrap_or_else(|e| e.into_inner());
        g.as_ref()
            .filter(|e| e.active.load(Ordering::SeqCst))
            .map(|e| e.mf.clone())
    }
}

/// Starts the engine (with the writes on, at start).
pub async fn run(c: Compat) {
    let active = Arc::new(AtomicBool::new(true));
    let host = U60dHost {
        root: c.root.to_string(),
        mihomo: c.src.mihomo.clone(),
        watch: c.group.watch.clone(),
        active: active.clone(),
    };
    let mf = Arc::new(
        ManualFirst::new(Arc::new(host), c.path(luyaz_api::chill::MANUAL_FIRST_STATE))
            .saving_every_change(),
    );
    let lease = c.path(luyaz_manual_first::LEASE_FILE);
    if let Err(e) = luyaz_api::lease::write(&lease, "u60d") {
        eprintln!("[compat] manual_first: could not write the lease: {e}");
    }
    let m2 = mf.clone();
    let thread = std::thread::spawn(move || m2.run());
    *c.manual_first.now.lock().unwrap_or_else(|e| e.into_inner()) = Some(Engine { mf, active, _thread: thread });
    eprintln!("[compat] manual_first: u60d runs it");
}
