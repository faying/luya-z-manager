//! SMS forwarding.
//!
//! With the core's writes on it runs the engine in `crates/luyaz-sms-forward` with the core as its host:
//! - woken by the hub: the `sms` block's max id / count, service from the `signal` block's `type`,
//!   the data call from the `interfaces` block's `cellular.connect_status`, down when the bearer in
//!   `/proc` is (looked at when one of those blocks publishes, else every [`LOOK_SECS`] for the
//!   bearer and the SIM switch hold; passed on only when they change; no `ubus listen` of its own);
//! - `sms.list_after`, mark-read, delete and the SMS sends go through u60d's own `/control`, the
//!   loop's as `auto`, a web test or retry under the caller's source;
//! - the engine is blocking (15 s HTTP, 5/15/60 s retries, 1 s between SMS): its loop runs on a
//!   thread of its own and the routes on the blocking pool, never on the runtime's one thread.
//!
//! The forwarder is built from the two state files at start and holds the lease
//! (`/tmp/sms-forward.owner`).

use luyaz_sms_forward::{self as sf, Host, Page, SmsForwarder};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use crate::api::Compat;
use crate::api::control::Client;

/// The blocks whose publish wakes the loop at once.
const WOKEN_BY: &[&str] = &["sms", "signal", "interfaces"];
/// The look without a publish: the bearer in `/proc` and the SIM switch hold are not blocks
/// (R18-P1-04: was 2 s for everything).
const LOOK_SECS: u64 = 10;
/// Timeout for one `sms.list_after` page.
const LIST_TIMEOUT: Duration = Duration::from_secs(60);
/// A SIM switch holds the sends until the phase has been `online` this long (the bearer settles).
const SIM_SETTLE: Duration = Duration::from_secs(30);
/// ... but never longer than this from the hold's start: a stuck phase (stalled) sends anyway.
const SIM_HOLD_CAP: Duration = Duration::from_secs(10 * 60);

thread_local! {
    /// Whose write an SMS send is: `auto` on the forwarder's loop, the caller's on a route.
    pub(crate) static SOURCE: std::cell::Cell<&'static str> = const { std::cell::Cell::new("screen") };
}

/// u60d as the engine's host.
pub struct U60dHost {
    root: String,
    hub: Option<Arc<crate::block::Hub>>,
    ubus: Arc<dyn Fn(&'static str, &'static str, Value) -> crate::api::UbusReply + Send + Sync>,
    control: Arc<Client>,
    handle: tokio::runtime::Handle,
    /// This forwarder still owns forwarding; false for good once taken back.
    active: Arc<AtomicBool>,
    /// Sleeps run `scale` times faster (tests).
    scale: u32,
    /// A SIM switch holds the checks ([`SimHold`]).
    held: Arc<AtomicBool>,
}

impl U60dHost {
    fn send(
        &self,
        source: &str,
        action: &str,
        params: &Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        self.handle
            .block_on(
                self.control
                    .send_with(source, None, action, params, timeout),
            )
            .into_result()
    }
}

fn fresh(hub: Option<&Arc<crate::block::Hub>>, name: &str) -> Option<Value> {
    hub?.view(name).filter(|v| !v.stale).map(|v| v.data.clone())
}

/// The reads the engine makes, as the `'static` names u60d's collector takes.
fn ubus_name(obj: &str, method: &str) -> Option<(&'static str, &'static str)> {
    Some(match (obj, method) {
        ("zte_nwinfo_api", "nwinfo_get_netinfo") => ("zte_nwinfo_api", "nwinfo_get_netinfo"),
        ("zwrt_data", "get_wwaniface") => ("zwrt_data", "get_wwaniface"),
        ("zwrt_wms", "zte_libwms_get_sms_data") => ("zwrt_wms", "zte_libwms_get_sms_data"),
        _ => return None,
    })
}

impl Host for U60dHost {
    fn sms_summary(&self) -> Option<(u64, u64)> {
        sms_summary(self.hub.as_ref())
    }
    /// One `sms.list_after` page through `/control`, as source `auto`.
    fn list_after(&self, after: u64, limit: u64) -> Result<Page, String> {
        let r = self.send(
            "auto",
            "sms.list_after",
            &json!({"after_id": after, "limit": limit}),
            LIST_TIMEOUT,
        )?;
        let items = r["items"]
            .as_array()
            .cloned()
            .ok_or("sms.list_after: no items")?;
        let has_more = r["has_more"]
            .as_bool()
            .ok_or("sms.list_after: no has_more")?;
        Ok(Page { items, has_more })
    }

    fn ubus_read(
        &self,
        obj: &str,
        method: &str,
        params: Option<&str>,
        _long: bool,
    ) -> Result<Value, String> {
        let (o, m) =
            ubus_name(obj, method).ok_or_else(|| format!("{obj} {method}: not read by u60d"))?;
        let args = match params {
            None => json!({}),
            Some(p) => {
                serde_json::from_str(p).map_err(|e| format!("{obj} {method}: params: {e}"))?
            }
        };
        self.handle.block_on((self.ubus)(o, m, args))
    }

    /// A vendor setter through `/control` (`vendor.call`), under the thread's source.
    fn vendor(&self, obj: &str, method: &str, params: Option<&str>) -> Result<Value, String> {
        let args: Value = match params {
            None => json!({}),
            Some(p) => {
                serde_json::from_str(p).map_err(|e| format!("{obj} {method}: params: {e}"))?
            }
        };
        if !args.is_object() {
            return Err(format!("{obj} {method}: params must be an object"));
        }
        let source = SOURCE.with(|s| s.get());
        self.send(
            source,
            "vendor.call",
            &json!({"object": obj, "method": method, "args": args}),
            crate::api::control::TIMEOUT,
        )
    }
    /// A core action (`sms.delete`, `sms.mark_read`) as source `auto`.
    fn control(&self, action: &str, params: &Value) -> Result<Value, String> {
        self.send("auto", action, params, crate::api::control::TIMEOUT)
    }

    fn alert(&self, key: &str, text: &str) {
        let a = luyaz_api::alerts::Alerts::at(&PathBuf::from(format!(
            "{}{}",
            self.root,
            luyaz_api::alerts::DIR
        )));
        let up = std::fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|s| s.split('.').next()?.trim().parse().ok())
            .unwrap_or(0);
        if let Err(e) = a.add(key, text, luyaz_api::alerts::now(), up) {
            eprintln!("[compat] sms_forward: could not raise {key}: {e}");
        }
    }

    /// Shadow records are not kept: nothing replays them here.
    fn tick(&self, _record: Value) {}

    fn sms_zone_hours(&self) -> i64 {
        let off = self.handle.block_on(crate::api::utc_offset());
        luyaz_api::clock::zone_from((off != 0).then_some(off), luyaz_api::clock::system_gmtoff())
            / 3600
    }

    fn active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    fn sleep(&self, d: Duration) {
        std::thread::sleep(d / self.scale);
    }

    fn run_worker(&self, f: Box<dyn FnOnce() + '_>) {
        SOURCE.with(|s| s.set("auto"));
        f()
    }

    fn hold(&self) -> bool {
        self.held.load(Ordering::SeqCst)
    }
}

/// Sends wait out a SIM switch (TODOS P1, 10-22): held from leaving/changed (and the
/// reloaded/stalled that follow within the hold; boot and no SIM read `online`) until it has been `online` for
/// [`SIM_SETTLE`], at most [`SIM_HOLD_CAP`] from the hold's start. Nothing is dropped: the
/// engine only skips its checks, the watermark stays.
#[derive(Default)]
struct SimHold {
    since: Option<Duration>,
    online_since: Option<Duration>,
}

impl SimHold {
    /// `now` = a monotonic clock. True = hold the sends.
    fn held(&mut self, phase: crate::sim_identity::Phase, now: Duration) -> bool {
        use crate::sim_identity::Phase;
        if matches!(phase, Phase::Reloaded | Phase::Stalled) && self.since.is_none() {
            return false; // only a switch (leaving/changed) starts a hold
        }
        if phase != Phase::Online {
            self.online_since = None;
            let since = *self.since.get_or_insert(now);
            return now.saturating_sub(since) < SIM_HOLD_CAP;
        }
        let Some(since) = self.since else {
            return false;
        };
        let up = *self.online_since.get_or_insert(now);
        if now.saturating_sub(up) < SIM_SETTLE && now.saturating_sub(since) < SIM_HOLD_CAP {
            return true;
        }
        *self = Self::default();
        false
    }
}

/// The forwarder u60d runs, if any.
#[derive(Default)]
pub struct Forwarding {
    now: Mutex<Option<Running>>,
}

struct Running {
    fwd: Arc<SmsForwarder>,
    active: Arc<AtomicBool>,
    /// What the hub said last: passed on only when it changes.
    service: Option<String>,
    wan: Option<String>,
    sms: Option<(u64, u64)>,
    sim: SimHold,
    held: Arc<AtomicBool>,
}

impl Forwarding {
    pub(crate) fn current(&self) -> Option<Arc<SmsForwarder>> {
        let g = self.now.lock().unwrap_or_else(|e| e.into_inner());
        g.as_ref()
            .filter(|r| r.active.load(Ordering::SeqCst))
            .map(|r| r.fwd.clone())
    }
}

fn lease(c: &Compat) -> PathBuf {
    c.path(sf::LEASE_FILE)
}

/// Starting, then passing on the hub.
pub async fn run(c: Compat) {
    let woken = c.src.hub.as_ref().map(|h| h.watch(WOKEN_BY));
    loop {
        {
            let mut g = c.sms.now.lock().unwrap_or_else(|e| e.into_inner());
            match g.as_mut() {
                None => *g = Some(take_over(&c)),
                Some(r) => pass_on(&c, r),
            }
        }
        let look = c.clock.sleep(LOOK_SECS);
        match &woken {
            Some(n) => tokio::select! {
                _ = n.notified() => {}
                _ = look => {}
            },
            None => look.await,
        }
    }
}

fn take_over(c: &Compat) -> Running {
    let active = Arc::new(AtomicBool::new(true));
    let held = Arc::new(AtomicBool::new(false));
    let host = U60dHost {
        root: c.root.to_string(),
        hub: c.src.hub.clone(),
        ubus: c.src.ubus.clone(),
        control: c.src.control.clone(),
        handle: tokio::runtime::Handle::current(),
        active: active.clone(),
        scale: c.clock.scale() as u32,
        held: held.clone(),
    };
    let fwd = Arc::new(SmsForwarder::new(
        Arc::new(host),
        c.path(sf::CONFIG_PATH),
        c.path(sf::STATE_PATH),
    )
    .queue_at(c.path(sf::QUEUE_PATH)));
    if let Err(e) = sf::write_lease(&lease(c), "u60d") {
        eprintln!("[compat] sms_forward: could not write the lease: {e}");
    }
    // the hub's SMS summary before the loop's first pass: one landing after it wakes the loop at
    // the first look instead of waiting for the idle poll
    let sms = sms_summary(c.src.hub.as_ref());
    // the connectivity seed sleeps between tries: off the runtime's thread
    let f = fwd.clone();
    std::thread::spawn(move || f.start(None));
    eprintln!("[compat] sms_forward: u60d forwards");
    Running {
        fwd,
        active,
        service: None,
        wan: None,
        sms,
        sim: SimHold::default(),
        held,
    }
}

fn pass_on(c: &Compat, r: &mut Running) {
    let hub = c.src.hub.as_ref();
    if let Some(t) = fresh(hub, "signal").and_then(|s| s["type"].as_str().map(str::to_string))
        && r.service.as_deref() != Some(t.as_str())
    {
        r.fwd.set_service(&t);
        r.service = Some(t);
    }
    if let Some(w) = fresh(hub, "interfaces")
        .and_then(|i| {
            i.pointer("/cellular/connect_status")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        // B31 can say connected with no bearer: then it is down for the forwarder too
        .map(|w| if c.bearer() == Some(false) { "bearer_down".to_string() } else { w })
        && r.wan.as_deref() != Some(w.as_str())
    {
        r.fwd.set_wan(&w);
        r.wan = Some(w);
    }
    let phase = crate::sim_identity::global()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .phase();
    let held = r.sim.held(phase, START.elapsed());
    if r.held.swap(held, Ordering::SeqCst) != held {
        eprintln!("[compat] sms_forward: SIM switch hold {}", if held { "on" } else { "off" });
        if !held {
            r.fwd.wake();
        }
    }
    let sms = sms_summary(hub);
    if sms != r.sms {
        r.fwd.wake();
        r.sms = sms;
    }
}

static START: std::sync::LazyLock<std::time::Instant> =
    std::sync::LazyLock::new(std::time::Instant::now);

/// The hub's `sms` block: (max id, count).
fn sms_summary(hub: Option<&Arc<crate::block::Hub>>) -> Option<(u64, u64)> {
    let b = fresh(hub, "sms")?;
    Some((
        b.get("max_id")?.as_u64()?,
        b.get("count").and_then(Value::as_u64).unwrap_or(0),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim_identity::Phase;

    fn s(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn sim_hold_leaving_changed_then_online_settle() {
        let mut h = SimHold::default();
        assert!(!h.held(Phase::Online, s(0)), "no switch: sends");
        assert!(h.held(Phase::Leaving, s(10)));
        assert!(h.held(Phase::Changed, s(20)));
        assert!(h.held(Phase::Online, s(30)), "online, not settled yet");
        assert!(h.held(Phase::Online, s(59)));
        assert!(!h.held(Phase::Online, s(60)), "settled: sends");
        assert!(!h.held(Phase::Online, s(61)));
    }

    #[test]
    fn sim_hold_settle_restarts_on_flap() {
        let mut h = SimHold::default();
        assert!(h.held(Phase::Changed, s(0)));
        assert!(h.held(Phase::Online, s(10)));
        assert!(h.held(Phase::Leaving, s(30)));
        assert!(h.held(Phase::Online, s(50)), "settle counts from the last online");
        assert!(!h.held(Phase::Online, s(80)));
    }

    #[test]
    fn sim_hold_not_started_by_reloaded_or_stalled() {
        let mut h = SimHold::default();
        assert!(!h.held(Phase::Stalled, s(0)));
        assert!(!h.held(Phase::Reloaded, s(1)));
        assert!(!h.held(Phase::Online, s(2)));
    }

    #[test]
    fn sim_hold_capped_when_stuck() {
        let mut h = SimHold::default();
        assert!(h.held(Phase::Leaving, s(0)));
        assert!(h.held(Phase::Stalled, s(599)));
        assert!(!h.held(Phase::Stalled, s(600)), "cap: sends anyway");
        assert!(!h.held(Phase::Stalled, s(3600)), "stays released while stuck");
        assert!(!h.held(Phase::Online, s(3601)), "past the cap: no settle wait");
        assert!(!h.held(Phase::Online, s(3700)));
        assert!(h.held(Phase::Leaving, s(4000)), "a new switch holds again");
    }
}
