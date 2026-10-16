//! Phase 2: the device read path, moved from zwrt-datad (data-service `8605c28`, the build on the
//! device since 10-07). `block`, `cell_window`, `command`, `executor`, `model`, `project/`, `qos`,
//! `sms`, `state`, `uci` and `ubus/` keep datad's file names so a later datad fix can be carried
//! over with a plain diff; what was left out and what still has to follow is in `PORTING.md`.
//!
//! Shadow mode (u60-platform.md Phase 2): u60d reads the same things datad reads and the outputs
//! are compared block by block. Nothing here writes to the device.

use crate::{block, executor, model::Snapshot, state};
use std::{sync::Arc, time::Duration};

/// One collection round, the same as `zwrt-datad --once`: the executor runs the round
/// (blocks first, then the snapshot), the result is the full snapshot.
pub async fn once(interval: Duration) -> Snapshot {
    rounds(interval, 1, Box::new(block::NoSink)).await
}

/// `rounds` collection rounds, `interval` apart, events into `sink`; the last snapshot.
pub async fn rounds(interval: Duration, rounds: u32, sink: Box<dyn block::EventSink>) -> Snapshot {
    let (hub, exec) = start(interval, sink);
    let mut snap = None;
    for i in 0..rounds.max(1) {
        if i > 0 {
            tokio::time::sleep(interval).await;
        }
        snap = Some(round(&exec, &hub, interval).await);
    }
    snap.expect("one round")
}

/// The hub with every published block and the executor that reads them.
pub fn start(
    interval: Duration,
    sink: Box<dyn block::EventSink>,
) -> (Arc<block::Hub>, executor::Executor) {
    let hub = Arc::new(block::Hub::new(block::phase1_blocks(), sink));
    let cfg = executor::Config {
        cache: state::cache_enabled(),
        cooldown: executor::Config::cooldown_from(
            std::env::var(executor::ENV_COOLDOWN_MS).ok().as_deref(),
        ),
        ..executor::Config::default()
    };
    let exec = executor::Executor::spawn(
        crate::ubus::backend::Backend::from_env(),
        hub.clone(),
        cfg,
        interval,
    );
    executor::install(&exec);
    (hub, exec)
}

/// One collection round (blocks first, then the snapshot).
pub async fn round(
    exec: &executor::Executor,
    hub: &Arc<block::Hub>,
    interval: Duration,
) -> Snapshot {
    let hub = hub.clone();
    let interval_ms = interval.as_millis() as u64;
    exec.round_now(async move { state::collect(interval_ms, &hub).await })
        .await
}

/// `u60d --evlog-stats N`: N rounds into an in-memory event log, then what it wrote
/// per block (record sizes for the D9 budget; the mock's values do not change, so
/// the rates come from the device).
pub async fn evlog_stats(interval: Duration, n: u32) -> serde_json::Value {
    let log = Arc::new(std::sync::Mutex::new(crate::evlog::EvLog::memory(
        &crate::evlog::boot_id(),
    )));
    let sink = crate::evlog::LogSink(log.clone());
    if std::env::var_os("LUYAZ_CONTRACT_DEBUG").is_some() {
        rounds(interval, n, Box::new(Debug(sink))).await;
    } else {
        rounds(interval, n, Box::new(sink)).await;
    }
    let g = log.lock().unwrap_or_else(|e| e.into_inner());
    serde_json::json!({"rounds": n, "bytes": g.bytes(), "stats": g.stats})
}

/// `--evlog-stats` with LUYAZ_CONTRACT_DEBUG=1: says why a block missed its contract
/// (quotes values; for the mock and the parity gate).
struct Debug(crate::evlog::LogSink);

impl block::EventSink for Debug {
    fn emit(&self, event: &block::Event) {
        if let block::Event::Block(b) = event
            && let Some(why) = crate::contract::shape_error(b.name, &b.data)
        {
            eprintln!("luyaz-core: contract {}: {why}", b.name);
        }
        self.0.emit(event);
    }
}
