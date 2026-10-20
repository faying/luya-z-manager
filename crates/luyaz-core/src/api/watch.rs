//! Netwatch, deep diagnosis and the APN SIM-change monitor in u60d (Phase 4 step 9b), with the
//! search/register, speed test and eSIM routes they share a gate or a file with. They move as one
//! group (the agent's `TOGETHER`): `/data/signal-log` has one writer (netwatch's minutes, the
//! diagnosis' events), the diagnosis gate (D10) must live where search/register and the speed test
//! claim it, and the APN monitor waits on all of them and on eSIM.
//!
//! u60d runs the group with its writes on (the agent's, handed over by a list, until Phase 10): the
//! sampler opens the log at start (the last day comes back from disk).
//!
//! Inputs are u60d's own: the hub's blocks for the state, its `/v2/screen` view for the verdict.
//! `/tmp/netwatch.errors` stays the agent's (it counts the agent's reads of core).

use axum::{
    body::Bytes,
    extract::{ConnectInfo, Request, State},
    response::Response,
};
use luyaz_api::diagnose::{BearerIn, EsimChannel, Gate, LinkProbe, SimIn, Sta};
use luyaz_api::finding::NetworkEvidence;
use luyaz_api::netinfo as ni;
use luyaz_netwatch::diagnose::{Diagnosis, Host};
use luyaz_netwatch::{ReadError, Watch, record};
use luyaz_policy::apn_pick as ap;
use serde_json::{Value, json};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{Compat, reply};

const SAMPLE_SECS: u64 = 5;
/// The APN monitor looks this often while confirming or retrying, and reads the card this often.
const QUICK: Duration = Duration::from_secs(10);
/// Without the hub's sim block (an ubus read per look), look at most this often when quiet.
const SLOW_LOOKS: u32 = 6;
const MAX_FAILS: u8 = 3;

/// The group's state in u60d.
pub struct Group {
    pub watch: Arc<Watch>,
    pub diag: Arc<Diagnosis>,
    /// the last `/v2/screen` view the sampler read (the diagnosis' signal layer)
    screen: Arc<Mutex<Option<Value>>>,
    /// registered but no internet (T3)
    pub findings: Arc<super::finding::Findings>,
    gate: Arc<Mutex<Gate>>,
    net: Arc<super::netinfo::NetCache>,
    speed_running: Arc<AtomicBool>,
    stations: Arc<dyn Fn() -> Vec<ni::Station> + Send + Sync>,
    /// a finding check is running
    finding_busy: Arc<AtomicBool>,
    /// the decision loop's pending proposal (§4.9), made after each finding check
    pub(super) producer: Arc<super::propose::Producer>,
}

/// A search, a manual register or a speed test going (the diagnosis and the finding check).
fn busy(net: &super::netinfo::NetCache, speed_running: &AtomicBool) -> Option<&'static str> {
    match lock(&net.ops).kind {
        ni::OpKind::Idle => speed_running.load(Ordering::SeqCst).then_some("speedtest"),
        ni::OpKind::Scanning => Some("scan"),
        ni::OpKind::Registering | ni::OpKind::Reverting => Some("register"),
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// What a diagnosis run reads in u60d.
struct DiagHost {
    root: String,
    hub: Option<Arc<crate::block::Hub>>,
    net: Arc<super::netinfo::NetCache>,
    speed_running: Arc<AtomicBool>,
    stations: Arc<dyn Fn() -> Vec<ni::Station> + Send + Sync>,
    watch: Arc<Watch>,
    screen: Arc<Mutex<Option<Value>>>,
    mihomo: String,
    handle: tokio::runtime::Handle,
    /// the Finding path's last observation (deep-diag-v2 §2.1)
    findings: Arc<super::finding::Findings>,
    /// the eSIM engine: its job slot guards the channel check
    esim: Arc<luyaz_esim::Esim>,
}

impl DiagHost {
    fn path(&self, p: &str) -> std::path::PathBuf {
        format!("{}{p}", self.root).into()
    }

    fn leases(&self) -> String {
        std::fs::read_to_string(self.path(ni::LEASES)).unwrap_or_default()
    }
}

fn fresh(hub: Option<&Arc<crate::block::Hub>>, name: &str) -> Option<Value> {
    hub?.view(name).filter(|v| !v.stale).map(|v| v.data.clone())
}

impl Host for DiagHost {
    fn radio_inputs(&self) -> (Option<Value>, Option<Value>) {
        let state = luyaz_netwatch::state_from_blocks(|n| fresh(self.hub.as_ref(), n));
        (state, lock(&self.screen).clone())
    }

    fn stations(&self) -> Vec<Sta> {
        luyaz_netwatch::diagnose::stations_named((self.stations)(), &self.leases())
    }

    fn lease_mac(&self, ip: &str) -> Option<String> {
        ni::parse_leases(&self.leases())
            .into_iter()
            .find(|(_, i, _)| i == ip)
            .map(|(m, _, _)| m)
    }

    fn others_busy(&self) -> Option<&'static str> {
        busy(&self.net, &self.speed_running)
    }

    fn proxy_on(&self) -> bool {
        std::fs::read_to_string(self.path(luyaz_api::chill::CHILL_STATE))
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .is_some_and(|v| v["state"] == "running")
    }

    fn proxy_probe(&self) -> luyaz_api::diagnose::ProxyProbe {
        use luyaz_api::chill::{Got, PROBE_URL, probe_exit};
        let http = reqwest::Client::new();
        let get = move |url: String, secs: u64| {
            let http = http.clone();
            async move {
                match http
                    .get(url)
                    .timeout(Duration::from_secs(secs))
                    .send()
                    .await
                {
                    Ok(r) => {
                        let code = r.status().as_u16();
                        let body = r.bytes().await.unwrap_or_default();
                        match serde_json::from_slice::<Value>(&body) {
                            Ok(v) => Got::Json(code, v),
                            Err(_) => Got::Status(code),
                        }
                    }
                    Err(_) => Got::Unreachable,
                }
            }
        };
        let direct = {
            let (h, get) = (self.handle.clone(), get.clone());
            std::thread::spawn(move || {
                let t = std::time::Instant::now();
                match h.block_on(get(PROBE_URL.to_string(), 3)) {
                    Got::Unreachable => None,
                    _ => Some(t.elapsed().as_millis() as u32),
                }
            })
        };
        // a 3 s delay test needs more than 3 s of HTTP
        let (node, node_probe) = probe_exit(&|path, slow| {
            self.handle.block_on(get(
                format!("{}{path}", self.mihomo),
                if slow { 15 } else { 3 },
            ))
        });
        luyaz_api::diagnose::ProxyProbe {
            node,
            node_probe,
            direct_ms: direct.join().ok().flatten(),
        }
    }

    fn link_probe(&self, dns: &[String]) -> LinkProbe {
        luyaz_netwatch::diagnose::link_probe(dns)
    }

    fn sim_inputs(&self) -> SimIn {
        SimIn {
            state: fresh(self.hub.as_ref(), "sim")
                // modem_state ("modem_init_complete", "modem_waitpin", …) is what judge_sim
                // reads; state is ZTE's sim_states ("sim ready"), the fallback
                .and_then(|v| {
                    [&v["modem_state"], &v["state"]]
                        .into_iter()
                        .find_map(|x| x.as_str().filter(|s| !s.is_empty()).map(str::to_string))
                }),
            channel: None,
        }
    }

    fn esim_channel(&self) -> Option<EsimChannel> {
        use luyaz_esim::ChannelCheck as C;
        Some(match self.esim.channel_check() {
            C::Free(n) => EsimChannel::Free(n),
            C::Full => EsimChannel::Full,
            C::Busy => EsimChannel::Busy,
            C::Unknown => EsimChannel::Unknown,
        })
    }

    fn bearer_inputs(&self, state: Option<&Value>) -> BearerIn {
        use luyaz_netwatch::diagnose as nd;
        let mut b = nd::bearer_base(state);
        let read = |p: &str| std::fs::read_to_string(self.path(p)).ok();
        let defaults = read("/proc/net/route").and_then(|t| nd::default_routes(&t));
        b.default_route = defaults.as_ref().map(|d| !d.is_empty());
        let rp = |i: &str| {
            read(&format!("/proc/sys/net/ipv4/conf/{i}/rp_filter"))
                .and_then(|t| t.trim().parse::<u8>().ok())
        };
        if luyaz_netwatch::finding::path::valid_interface(&b.iface) {
            b.rp_filter = nd::rp_effective(rp("all"), rp(&b.iface));
        }
        let devices: Vec<String> = std::fs::read_dir(self.path("/sys/class/net"))
            .map(|d| {
                d.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        b.exits = nd::ways_out(&b.iface, &defaults.unwrap_or_default(), &devices);
        b
    }

    fn path_probe(&self, state: Option<&Value>) -> NetworkEvidence {
        use luyaz_netwatch::finding::path;
        let paths = state.map(path::paths).unwrap_or_default();
        let now = luyaz_api::clock::BootTime::read();
        // the state was read just now (fresh blocks only): observed = now
        path::observe(
            &paths,
            now.clone(),
            now,
            // Finding stays quiet while CHILL redirects (its verdict is about the clients' path);
            // a diagnosis the owner asked for still asks the bound cellular path
            false,
            &path::BoundSockets,
        )
    }

    fn fresh_network(&self) -> Option<NetworkEvidence> {
        self.findings.fresh_network()
    }

    fn local_dns(&self) -> Option<luyaz_api::diagnose::LocalDns> {
        Some(luyaz_netwatch::diagnose::local_dns_probe())
    }

    fn clock_skew(&self) -> Option<i64> {
        // plain HTTP by address: no DNS, no TLS (a wrong clock is what breaks TLS)
        const TARGETS: [&str; 2] = ["http://1.1.1.1/", "http://119.29.29.29/"];
        let offset = self.handle.block_on(super::utc_offset());
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(3))
            .build()
            .ok()?;
        TARGETS.iter().find_map(|url| {
            let date = self.handle.block_on(async {
                let r = http.get(*url).send().await.ok()?;
                r.headers().get("date")?.to_str().ok().map(str::to_string)
            })?;
            let real = luyaz_api::diagnose::http_date_unix(&date)?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_secs() as i64;
            Some(now - offset - real)
        })
    }

    fn clock_status(&self) -> Option<luyaz_api::clock::EvidenceStatus> {
        Some(crate::longterm::clock_now(&crate::evlog::boot_id()).clock_status)
    }

    fn push_event(&self, kind: &str, data: Value) {
        self.watch
            .push_event(record::Event::new(luyaz_netwatch::now_unix(), kind, data));
    }

    fn events(&self, from: u64, to: u64) -> Vec<record::Event> {
        self.watch.history(from, to).1
    }
}

impl Group {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        root: &str,
        hub: Option<Arc<crate::block::Hub>>,
        net: Arc<super::netinfo::NetCache>,
        speed_running: Arc<AtomicBool>,
        stations: Arc<dyn Fn() -> Vec<ni::Station> + Send + Sync>,
        gate: Arc<Mutex<Gate>>,
        #[allow(unused_variables)] mihomo: &str,
        esim: Arc<luyaz_esim::Esim>,
    ) -> Group {
        let watch = Arc::new(Watch::new());
        let screen: Arc<Mutex<Option<Value>>> = Arc::default();
        let findings = Arc::new(super::finding::Findings::load(
            format!("{root}{}", super::finding::FILE).into(),
        ));
        let host = DiagHost {
            root: root.to_string(),
            hub,
            net: net.clone(),
            speed_running: speed_running.clone(),
            stations: stations.clone(),
            watch: watch.clone(),
            screen: screen.clone(),
            mihomo: mihomo.to_string(),
            handle: tokio::runtime::Handle::try_current()
                .unwrap_or_else(|_| fallback_runtime().handle().clone()),
            findings: findings.clone(),
            esim,
        };
        Group {
            diag: Diagnosis::new(Arc::new(host), gate.clone()),
            watch,
            screen,
            findings,
            gate,
            net,
            speed_running,
            stations,
            finding_busy: Arc::default(),
            producer: Arc::default(),
        }
    }
}

/// Built outside a runtime (some tests): the proxy probe's requests get a small runtime of their own.
fn fallback_runtime() -> &'static tokio::runtime::Runtime {
    static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
    })
}

/// With the writes on: u60d serves, samples into the log, and watches.
pub(super) fn owns(c: &Compat) -> bool {
    c.writes
}

/// netwatch's verdict for the public status (nothing with the writes off).
pub(super) fn verdict(c: &Compat) -> Option<String> {
    owns(c)
        .then(|| {
            c.group
                .watch
                .verdict(luyaz_api::clock::BootTime::read().as_ref())
        })
        .flatten()
}

/// The sampler: every 5 s, the hub's blocks and its screen view into the watcher (and so the
/// signal log).
pub async fn sample(c: Compat) {
    let mut tick = tokio::time::interval(c.clock.secs(SAMPLE_SECS));
    c.group.watch.open(c.path(record::DIR));
    let mut round: u32 = 0;
    loop {
        tick.tick().await;
        round = round.wrapping_add(1);
        let g = &c.group;
        let state = luyaz_netwatch::state_from_blocks(|n| fresh(c.src.hub.as_ref(), n));
        let screen = (c.src.screen)().await;
        *lock(&g.screen) = screen.clone();
        let now = luyaz_netwatch::now_unix();
        // Observe every existing five-second tick, even while a blocking probe is running.
        // Missing reads, a receive event, connection changes and sampling gaps reset the hold.
        let readings = c
            .src
            .hub
            .as_ref()
            .and_then(|hub| super::finding::sampling::Readings::capture(hub));
        let elapsed = luyaz_api::clock::BootTime::read();
        g.findings
            .sample_passive(readings.as_ref(), elapsed.as_ref());
        let check = owns(&c) && round.is_multiple_of(super::finding::EVERY_ROUNDS);
        let w = g.watch.clone();
        let tick_screen = screen.clone();
        let _ = tokio::task::spawn_blocking(move || {
            w.tick(
                now,
                elapsed.as_ref(),
                state.ok_or(ReadError::NoWanStatus),
                tick_screen.as_ref(),
            )
        })
        .await;
        // apart from the sampler: the probes can take ~12 s while the link is stalled
        if check && !g.finding_busy.swap(true, Ordering::SeqCst) {
            let c2 = c.clone();
            tokio::task::spawn_blocking(move || {
                let g = &c2.group;
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    finding_round(
                        g,
                        readings,
                        screen,
                        now,
                        c2.root.to_string(),
                        c2.src.hub.clone(),
                    )
                }));
                g.finding_busy.store(false, Ordering::SeqCst);
            });
        }
    }
}

/// One finding check after the round's sample (blocking: the probes are plain sockets).
fn finding_round(
    g: &Group,
    readings: Option<super::finding::sampling::Readings>,
    screen: Option<Value>,
    now: u64,
    root: String,
    hub: Option<Arc<crate::block::Hub>>,
) {
    let elapsed = luyaz_api::clock::BootTime::read();
    let (passive_down, held_down) = g
        .findings
        .passive_admission(readings.as_ref(), elapsed.as_ref());
    let kept = readings.clone();
    let hub2 = hub.clone();
    let expected = readings.as_ref().map(|r| r.connection.clone());
    let current_connection = Arc::new(move || {
        expected
            .as_ref()
            .zip(hub.as_ref())
            .and_then(|(expected, hub)| {
                super::finding::sampling::Readings::capture(hub).map(|r| r.connection == *expected)
            })
            == Some(true)
    });
    let findings = g.findings.clone();
    let i = super::finding::Inputs {
        state: readings.as_ref().map(|r| r.state.clone()),
        screen,
        iccid: readings
            .as_ref()
            .map(|r| r.connection.iccid.clone())
            .unwrap_or_default(),
        passive_down,
        clients: passive_down && !(g.stations)().is_empty(),
        probe_due: g.findings.probe_due(elapsed.as_ref()),
        held_down,
        busy: busy(&g.net, &g.speed_running),
        probed: AtomicBool::new(false),
        observed: readings.map(|r| r.observed),
        root,
        network: Mutex::new(None),
        current_connection,
        reserve_probe: Arc::new(move || {
            findings.reserve_probe(luyaz_api::clock::BootTime::read().as_ref())
        }),
    };
    g.findings.round(&i, &g.gate, &g.watch, now);
    super::propose::after_round(&g.producer, &g.findings, &i, kept.as_ref(), hub2.as_deref());
}

// ── deep diagnosis ─────────────────────────────────────────────────────────

/// Who asked: the peer, or for a request the agent forwards (from loopback only) the client it
/// names in [`luyaz_api::headers::CLIENT_HEADER`].
pub(super) fn remote(req: &Request) -> Option<IpAddr> {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip());
    match peer {
        Some(p) if p.is_loopback() => req
            .headers()
            .get(luyaz_api::headers::CLIENT_HEADER)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse().ok())
            .or(Some(p)),
        other => other,
    }
}

fn answer(r: Result<(u16, Value), tokio::task::JoinError>) -> Response {
    match r {
        Ok((s, v)) => reply(s, v),
        Err(_) => reply(500, json!({"ok": false, "error": "internal"})),
    }
}

/// GET /api/diagnose
pub async fn diagnose_get(State(c): State<Compat>) -> Response {
    let (s, mut v) = c.group.diag.get();
    // the run itself, before the fields below are added next to it
    let slow = super::slow_signal::view(&v["data"], c.src.hub.as_deref());
    // parses the ledger again once an hour: off the async workers
    let hub = c.src.hub.clone();
    let (hot, idle) = tokio::task::spawn_blocking(move || {
        (
            super::hot_drain::view(),
            super::long_idle::view(hub.as_deref()),
        )
    })
    .await
    .unwrap_or((Value::Null, Value::Null));
    // D6: the old fields as they are, plus the open finding (null when none)
    if let Some(d) = v.get_mut("data").and_then(Value::as_object_mut) {
        d.insert("finding".into(), c.group.findings.view());
        d.insert("proposal".into(), c.group.producer.view());
        d.insert("connectivity".into(), c.group.findings.network_view());
        d.insert("band_flap".into(), super::band_flap::view());
        d.insert("hot_drain".into(), hot);
        d.insert("long_idle".into(), idle);
        d.insert("slow_signal".into(), slow);
    }
    reply(s, v)
}

/// POST /api/diagnose
pub async fn diagnose_start(State(c): State<Compat>, req: Request) -> Response {
    let who = remote(&req);
    let (s, v) = c.group.diag.start(who);
    reply(s, v)
}

/// POST /api/diagnose/speed `{"id"}`: on u60d's speed test engine, claimed through the same gate.
pub async fn diagnose_speed(State(c): State<Compat>, body: Bytes) -> Response {
    answer(
        tokio::task::spawn_blocking(move || {
            c.group.diag.speed(&body, |done| {
                c.speed
                    .start_capped(done, &|r: &AtomicBool| super::speed::admit(&c, r))
            })
        })
        .await,
    )
}

/// POST /api/diagnose/feedback `{"id", "right"}`
pub async fn diagnose_feedback(State(c): State<Compat>, body: Bytes) -> Response {
    let (s, v) = c.group.diag.feedback(&body);
    reply(s, v)
}

/// POST /api/diagnose/dismiss `{"kind", "iccid_tail"?}`: the user's "ignore" on the finding
/// `GET /api/diagnose` shows (T6); the same kind + card stays closed for 24 h.
pub async fn diagnose_dismiss(State(c): State<Compat>, body: Bytes) -> Response {
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let Some(kind) = v["kind"].as_str().filter(|k| !k.is_empty()) else {
        return reply(400, json!({"ok": false, "error": "kind required"}));
    };
    let tail = v["iccid_tail"].as_str();
    let watch = c.group.watch.clone();
    let findings = c.group.findings.clone();
    let kind = kind.to_string();
    let tail = tail.map(str::to_string);
    let done = tokio::task::spawn_blocking(move || {
        findings.dismiss_shown(&kind, tail.as_deref(), &watch, luyaz_netwatch::now_unix())
    })
    .await
    .unwrap_or(false);
    if done {
        reply(200, json!({"ok": true}))
    } else {
        reply(404, json!({"ok": false, "error": "no such finding"}))
    }
}

// ── the APN SIM-change monitor (the agent's apn_pick thread, D39) ─────────────────

/// Manual mode, and the manual profile in use; None = could not read.
async fn apn_state(c: &Compat) -> Option<(bool, Option<String>)> {
    let mode = c
        .ubus_res("zwrt_apn_object", "get_apn_mode", json!({}))
        .await
        .ok()?;
    let manual = match &mode["apn_mode"] {
        Value::String(s) => s == "1",
        v => v.as_i64() == Some(1),
    };
    let list = c
        .ubus_res("zwrt_apn_object", "get_manu_apn_list", json!({}))
        .await
        .ok()?;
    let selected = list["apnListArray"]
        .as_array()
        .and_then(|a| a.iter().find(|p| ni::truthy(&p["isEnable"])))
        .and_then(|p| p["profileId"].as_str().map(str::to_string));
    Some((manual, selected))
}

fn read_store(c: &Compat) -> ap::Store {
    std::fs::read_to_string(c.path(ni::APN_PICK_STORE))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn commit(c: &Compat, card: &str) {
    super::ops::pick_store(c, |o| {
        if o.get("last_iccid").and_then(Value::as_str) == Some(card) {
            return false;
        }
        o.insert("last_iccid".into(), json!(card));
        true
    });
}

fn set_notice(c: &Compat, text: String, text_en: String, sticky: bool) {
    let now = luyaz_netwatch::now_unix();
    super::ops::pick_store(c, |o| {
        o.insert(
            "notice".into(),
            json!({"at": now, "text": text, "text_en": text_en, "sticky": sticky}),
        );
        true
    });
}

/// Watch for card changes: the one automatic APN write (D39), as
/// `auto`, through u60d's own `/control`.
pub async fn apn_watch(c: Compat) {
    let mut w = ap::Watch::default();
    let mut quiet = 0u32;
    loop {
        c.clock.sleep(QUICK.as_secs()).await;
        // the hub's sim block costs nothing; without it, an ubus read: then only every minute
        let from_hub = fresh(c.src.hub.as_ref(), "sim").is_some();
        quiet += 1;
        if !w.eager() && !from_hub && quiet < SLOW_LOOKS {
            continue;
        }
        quiet = 0;
        let iccid = super::ops::current_iccid(&c).await;
        let store = read_store(&c);
        let busy = c.net.apn_switching.load(Ordering::SeqCst)
            || c.net.apn_trial.load(Ordering::SeqCst)
            || lock(&c.net.ops).kind != ni::OpKind::Idle
            || c.esim.busy();
        // decide() reads the APN state only for a confirmed change: find out on a copy first
        let mut asked = false;
        ap::decide(&store, &mut w.clone(), &iccid, busy, || {
            asked = true;
            None
        });
        let read = if asked { apn_state(&c).await } else { None };
        let d = ap::decide(&store, &mut w, &iccid, busy, || read);
        let Some(card) = d.card else { continue };
        if d.action == ap::Action::None {
            commit(&c, &card);
            continue;
        }
        if c.net.apn_switching.swap(true, Ordering::SeqCst) {
            continue; // a switch started meanwhile: the watch still says act, the next look retries
        }
        c.net.apn_error("");
        let writer = c.writer("auto");
        let res = match &d.action {
            ap::Action::Auto => super::ops::dial_auto(&writer).await,
            ap::Action::Use(id) => super::ops::dial_manual(&writer, id).await,
            ap::Action::None => Ok(()),
        };
        if let Some(pause) = luyaz_api::control::retry_after(&res) {
            // a network-mode change is confirming (D40): not a failed try, again after the pause
            c.net.apn_switching.store(false, Ordering::SeqCst);
            eprintln!(
                "[compat] apn_pick: SIM …{} → {:?}: held off, again in {}s",
                ap::tail4(&card),
                d.action,
                pause.as_secs()
            );
            tokio::time::sleep(pause).await;
            continue;
        }
        c.net
            .apn_error(res.as_ref().err().map(String::as_str).unwrap_or(""));
        c.net.apn_switching.store(false, Ordering::SeqCst);
        eprintln!(
            "[compat] apn_pick: SIM …{} → {:?}: {:?}",
            ap::tail4(&card),
            d.action,
            res
        );
        let apn = store
            .picks
            .get(&card)
            .map(|p| p.apn.clone())
            .unwrap_or_default();
        match res {
            Ok(()) => {
                commit(&c, &card);
                w = ap::Watch::default();
                let (t, te) = match d.action {
                    ap::Action::Auto => (
                        "换了卡，APN 已切回自动".to_string(),
                        "SIM changed; APN back to automatic".to_string(),
                    ),
                    _ => (
                        format!("换回这张卡，APN 用回 {apn}"),
                        format!("This SIM is back; APN {apn} again"),
                    ),
                };
                set_notice(&c, t, te, false);
            }
            Err(e) => {
                w.fails += 1;
                if w.fails >= MAX_FAILS {
                    // give up: the notice stays until an APN change works
                    commit(&c, &card);
                    w = ap::Watch::default();
                    set_notice(
                        &c,
                        format!("换卡后切 APN 试了 {MAX_FAILS} 次没成功：{e}"),
                        format!(
                            "Couldn't switch APN after the SIM change ({MAX_FAILS} tries): {e}"
                        ),
                        true,
                    );
                }
            }
        }
    }
}
