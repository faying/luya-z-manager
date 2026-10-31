//! Deep diagnosis' runner: "why is it slow?" on request (`docs/designs/slow-diagnosis.md` §4.2,
//! §12.3, §12.5; eng review 10-02 D4/D8/D10/D11/D12). The judgements are
//! u60-api's (`luyaz_api::diagnose`), every device read goes through a [`Host`].
//!
//! `start` begins one run (about 10 s, ≤ 5 s per layer, ≤ 20 s in all); `get` reads it. One run
//! at a time: a second start while one is waiting or running gets that same run. A finished run
//! is kept 10 minutes.
//!
//! Layers, in the order the main cause is picked from (`deep-diag-v2.md` §2): Wi-Fi, SIM, signal,
//! speed cap, bearer, cellular link, DNS, internet (e2e), clock, cell load, proxy. Each ends ok /
//! warn / bad / na ("can't tell", never the main cause), or blocked: a layer it depends on
//! (`luyaz_api::diagnose::DEPS`) is bad, so it sends nothing and is left out of "N/M" (§3).
//!
//! DNS and e2e reuse the Finding path (§2.1): one `finding::path::observe` on the bound cellular
//! sockets (at most 2 HTTP + 2 DNS), plus 2 queries to the device's own dnsmasq. A Finding probe
//! holding the gate is waited for, and what it just saw is used instead of probing again.
//!
//! Sharing the modem and the uplink (D4/D10): one gate, handed in by the caller because the same
//! lock guards its search/register and speed test. A run that finds one of those going waits for
//! it (up to 120 s) and starts by itself; while a run is going they are refused at once
//! (`luyaz_api::diagnose::refusal_at`). Both sides check and claim under the gate, so neither can
//! slip in between. The run does not drive the modem.
//!
//! What it sends: 10 DNS queries to the operator's resolver, the Finding path's 2 + 2, 2 local
//! queries, and, with the proxy on, one health
//! check of the node in use plus one direct request to the same generate_204 URL (D11). Nothing
//! while idle.
//!
//! Cell-load history (D8/D12): evidence ② compares with earlier runs on the same cell made while
//! nobody was using the network (their link latency is in each `diagnose` event); evidence ③ uses
//! only "add speed test" results (`speed` events) for the same carrier + full cell ID + channel +
//! hour. Without a full cell ID neither counts.

use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use luyaz_api::diagnose::*;
use luyaz_speedtest::{OnDone, SpeedTestProgress};

use luyaz_api::clock::EvidenceStatus;
use luyaz_api::finding::{NetworkEvidence, ResultState};

use crate::record::{Event, Reading};
use crate::{dns_query, now_unix, operator_dns};

/// The Finding path's worst case is 2 × 5 s HTTP + 2 × 2 s DNS: its own cap, above that.
pub const PATH_MAX: Duration = Duration::from_secs(16);
/// After a Finding probe frees the gate, how long its result may take to be stored.
pub const FRESH_WAIT: Duration = Duration::from_millis(1500);
/// The device's own resolver gets this long for its two names.
pub const LOCAL_DNS_MAX: Duration = Duration::from_secs(2);
/// One domestic name, one foreign one (v2 §2 dns).
pub const LOCAL_DNS_NAMES: [&str; 2] = ["www.qq.com", "www.google.com"];

/// What a run reads and where it writes, supplied by the host.
pub trait Host: Send + Sync + 'static {
    /// the core's state in the old `/state` shape and the home screen (`/v2/screen`), one read each.
    fn radio_inputs(&self) -> (Option<Value>, Option<Value>);
    /// Every Wi-Fi station, named from the DHCP leases.
    fn stations(&self) -> Vec<Sta> {
        let leases = std::fs::read_to_string(luyaz_api::netinfo::LEASES).unwrap_or_default();
        stations_named(real_stations(), &leases)
    }
    /// The lease MAC for a LAN address, if it has one.
    fn lease_mac(&self, ip: &str) -> Option<String> {
        let leases = std::fs::read_to_string(luyaz_api::netinfo::LEASES).unwrap_or_default();
        luyaz_api::netinfo::parse_leases(&leases).into_iter().find(|(_, i, _)| i == ip).map(|(m, _, _)| m)
    }
    /// A search, a manual register or a speed test going: "scan" / "register" / "speedtest".
    fn others_busy(&self) -> Option<&'static str>;
    /// The proxy is on (the run then has a proxy layer).
    fn proxy_on(&self) -> bool {
        false
    }
    fn proxy_probe(&self) -> ProxyProbe {
        ProxyProbe::default()
    }
    fn link_probe(&self, dns: &[String]) -> LinkProbe {
        link_probe(dns)
    }
    /// The sim block (`get_sim_info`): the card's state only.
    fn sim_inputs(&self) -> SimIn {
        SimIn::default()
    }
    /// The eSIM channel check, asked only with the card ready; must not run while an eSIM
    /// operation holds the card (the host claims the eSIM engine for it). None = no eSIM tools.
    fn esim_channel(&self) -> Option<EsimChannel> {
        None
    }
    /// The bearer layer's inputs. The default reads the state alone; the device adds routes and
    /// rp_filter.
    fn bearer_inputs(&self, state: Option<&Value>) -> BearerIn {
        bearer_base(state)
    }
    /// The Finding path's bound observation (`finding::path::observe`) for this state.
    fn path_probe(&self, _state: Option<&Value>) -> NetworkEvidence {
        NetworkEvidence { reason: "not_probed".into(), ..NetworkEvidence::default() }
    }
    /// A Finding observation that is still fresh (written within `finding::path::MAX_AGE`).
    fn fresh_network(&self) -> Option<NetworkEvidence> {
        None
    }
    /// The device's own resolver (dnsmasq); None = not asked.
    fn local_dns(&self) -> Option<LocalDns> {
        None
    }
    /// Bound multi-target HTTPS over the cellular path of this state, within `budget`
    /// (audit 10-09 P2); None = not probed (no cellular IPv4 path, or the host has none).
    fn https_probe(&self, _state: Option<&Value>, _budget: Duration) -> Option<HttpsEvidence> {
        None
    }
    /// Seconds the device clock (as real UTC) is ahead of an HTTP server's `Date`. None = no answer.
    fn clock_skew(&self) -> Option<i64> {
        None
    }
    /// The clock evidence's status now.
    fn clock_status(&self) -> Option<EvidenceStatus> {
        None
    }
    /// Into the signal history (`diagnose`, `speed`, `diagnose_feedback`).
    fn push_event(&self, kind: &str, data: Value);
    /// History events of one of `kinds` with `from <= t < to`.
    fn events(&self, from: u64, to: u64, kinds: &[&str]) -> Vec<Event>;
}

/// The runner: the gate it shares with its caller's search/register and speed test, and the run.
pub struct Diagnosis {
    host: Arc<dyn Host>,
    gate: Arc<Mutex<Gate>>,
    run: Mutex<Option<Run>>,
}

impl Diagnosis {
    pub fn new(host: Arc<dyn Host>, gate: Arc<Mutex<Gate>>) -> Arc<Self> {
        Arc::new(Diagnosis { host, gate, run: Mutex::new(None) })
    }

    /// Take it before checking and claiming; hold it until the claim is made.
    pub fn gate(&self) -> MutexGuard<'_, Gate> {
        self.gate.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn slot(&self) -> MutexGuard<'_, Option<Run>> {
        self.run.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn update(&self, id: u64, f: impl FnOnce(&mut Run)) {
        if let Some(r) = self.slot().as_mut().filter(|r| r.id == id) {
            f(r);
        }
    }

    /// `POST /api/diagnose`: start a run, or join the one going. `remote`: who asked (loopback =
    /// the front screen; a LAN address picks that client's Wi-Fi link).
    pub fn start(self: &Arc<Self>, remote: Option<IpAddr>) -> (u16, Value) {
        let now = now_unix();
        let mut slot = self.slot();
        if let Some(r) = slot.as_ref() {
            if matches!(r.state, RunState::Waiting | RunState::Running) {
                return (202, json!({"ok": true, "data": view(r, now), "joined": true}));
            }
        }
        let (from, who) = self.who_from(remote);
        let id = slot.as_ref().map_or(1, |r| r.id + 1);
        let run = Run::new(id, now, from, self.host.proxy_on());
        let out = view(&run, now);
        *slot = Some(run);
        drop(slot);
        let me = Arc::clone(self);
        thread::Builder::new()
            .name("deep-diag".into())
            .spawn(move || {
                let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| me.execute(id, who))).is_ok();
                if !ok {
                    me.gate().diag_since = None;
                    me.update(id, |r| {
                        for l in r.layers.clone() {
                            if matches!(l.level, Level::Pending | Level::Running) {
                                r.put(l.na("出错", "Error"));
                            }
                        }
                        r.main = Some(pick_main(&r.layers));
                        r.state = RunState::Done;
                        r.finished_at = Some(now_unix());
                    });
                }
            })
            .ok();
        (202, json!({"ok": true, "data": out}))
    }

    /// `GET /api/diagnose`: the current or last run (kept [`KEEP_SECS`]).
    pub fn get(&self) -> (u16, Value) {
        let now = now_unix();
        let slot = self.slot();
        match slot.as_ref() {
            Some(r) if r.state != RunState::Done || r.finished_at.is_some_and(|f| now.saturating_sub(f) <= KEEP_SECS) => {
                (200, json!({"ok": true, "data": view(r, now)}))
            }
            _ => (200, json!({"ok": true, "data": {"state": "idle"}})),
        }
    }

    /// `POST /api/diagnose/speed`, body {"id": n}: "add speed test" for a finished run (capped:
    /// download only, ~5 s, ≤ 30 MB), started by `start_capped` (the caller's engine, claimed
    /// through the same gate). The two-step confirm when roaming is the screens' job.
    pub fn speed(self: &Arc<Self>, body: &[u8], start_capped: impl FnOnce(OnDone) -> (u16, Value)) -> (u16, Value) {
        let Some(id) = serde_json::from_slice::<Value>(body).ok().and_then(|b| b.get("id").and_then(Value::as_u64)) else {
            return (400, json!({"ok": false, "error": "id is required"}));
        };
        let (key, before) = {
            let mut slot = self.slot();
            let Some(r) = slot.as_mut().filter(|r| r.id == id && r.state == RunState::Done) else {
                return (404, json!({"ok": false, "error": "no such finished run"}));
            };
            if r.speed.as_ref().is_some_and(|l| l.level == Level::Running) {
                return (202, json!({"ok": true, "data": view(r, now_unix())}));
            }
            let before = r.speed.take();
            // shown as running before the test can possibly finish
            r.speed = Some(Layer { level: Level::Running, ..Layer::new("speed") });
            (r.key.clone(), before)
        };
        let me = Arc::clone(self);
        let done: OnDone = Box::new(move |p: &SpeedTestProgress| {
            let row = speed_row(p);
            me.update(id, |r| r.speed = Some(row));
            // R3: a short sample (v2 engine's `sample_short`) still shows in the row but never
            // enters the history. Read through the JSON so an engine without the field is fine.
            let short = serde_json::to_value(p).ok().and_then(|v| v.get("sample_short").and_then(Value::as_bool)).unwrap_or(false);
            if let Some(m) = p.download_mbps().filter(|_| !short) {
                let mut ev = key_json(&key);
                ev["id"] = json!(id);
                ev["mbps"] = json!(m);
                ev["bytes"] = json!(p.download_bytes());
                ev["route"] = json!("direct");
                ev["method"] = json!(SPEED_METHOD);
                me.host.push_event("speed", ev);
            }
        });
        let (code, v) = start_capped(done);
        if code != 200 {
            self.update(id, |r| r.speed = before);
            return (code, v);
        }
        let slot = self.slot();
        (202, json!({"ok": true, "data": slot.as_ref().map(|r| view(r, now_unix()))}))
    }

    /// `POST /api/diagnose/feedback`, body {"id": n, "right": true|false}; once per run.
    pub fn feedback(&self, body: &[u8]) -> (u16, Value) {
        let Ok(b) = serde_json::from_slice::<Value>(body) else {
            return (400, json!({"ok": false, "error": "invalid JSON"}));
        };
        let (Some(id), Some(right)) = (b.get("id").and_then(Value::as_u64), b.get("right").and_then(Value::as_bool)) else {
            return (400, json!({"ok": false, "error": "id and right are required"}));
        };
        let mut slot = self.slot();
        let Some(r) = slot.as_mut().filter(|r| r.id == id && r.state == RunState::Done) else {
            return (404, json!({"ok": false, "error": "no such finished run"}));
        };
        if r.feedback.is_some() {
            return (200, json!({"ok": true, "data": {"already": true}}));
        }
        r.feedback = Some(right);
        let mut ev = key_json(&r.key);
        ev["id"] = json!(id);
        ev["right"] = json!(right);
        ev["main"] = json!(r.main.as_ref().map(|m| m.layer));
        drop(slot);
        self.host.push_event("diagnose_feedback", ev);
        (200, json!({"ok": true, "data": {"recorded": true}}))
    }

    /// Who is asking, from the request's source address.
    fn who_from(&self, remote: Option<IpAddr>) -> (&'static str, Who) {
        match remote {
            Some(ip) if ip.is_loopback() => ("touch", Who::Touch),
            Some(ip) => ("client", Who::Client(self.host.lease_mac(&ip.to_string()))),
            None => ("other", Who::Client(None)),
        }
    }

    fn execute(self: &Arc<Self>, id: u64, who: Who) {
        // wait for a search / register / speed test, then claim the gate
        let asked = Instant::now();
        let mut waited_finding = false;
        loop {
            let mut g = self.gate();
            // a write transaction's reservation (decision-execution-loop §1.3) waits like the rest
            let busy = if g.txn.is_some() {
                Some("txn")
            } else if g.finding_since.is_some() {
                Some("finding")
            } else {
                self.host.others_busy()
            };
            waited_finding |= busy == Some("finding");
            match busy {
                None => {
                    g.diag_since = Some(now_unix());
                    drop(g);
                    break;
                }
                Some(k) => {
                    drop(g);
                    if asked.elapsed().as_secs() >= WAIT_MAX {
                        self.update(id, |r| {
                            gave_up(r, k);
                            r.state = RunState::Done;
                            r.waiting_for = None;
                            r.finished_at = Some(now_unix());
                        });
                        return;
                    }
                    self.update(id, |r| r.waiting_for = Some(k));
                    thread::sleep(Duration::from_secs(1));
                }
            }
        }
        let start = Instant::now();
        self.update(id, |r| {
            r.state = RunState::Running;
            r.waiting_for = None;
            r.started_at = Some(now_unix());
        });
        let left = || RUN_MAX.saturating_sub(start.elapsed()).min(LAYER_MAX);
        let timed_out = |l: Layer| l.na("超时", "Timed out");

        // Wi-Fi
        self.update(id, |r| r.running("wifi"));
        let h = Arc::clone(&self.host);
        let w = within(left(), move || judge_wifi(&who, &h.stations())).unwrap_or_else(|| timed_out(Layer::new("wifi")));
        self.update(id, |r| r.put(w));

        // SIM: the card's state; the eSIM channel check only with the card otherwise ok (v2 Q1)
        self.update(id, |r| r.running("sim"));
        let h = Arc::clone(&self.host);
        let sim = within(left(), move || {
            let mut i = h.sim_inputs();
            if judge_sim(&i).level == Level::Ok {
                i.channel = h.esim_channel();
            }
            judge_sim(&i)
        })
        .unwrap_or_else(|| timed_out(Layer::new("sim")));
        self.update(id, |r| r.put(sim));

        // signal + cap, one read of core
        self.update(id, |r| {
            r.running("signal");
            r.running("limit");
        });
        let h = Arc::clone(&self.host);
        let read = within(left(), move || {
            let (st, sc) = h.radio_inputs();
            (radio_from(st.as_ref(), sc.as_ref()), st)
        });
        let ((radio, key, dns, _), state) = read.clone().unwrap_or_default();
        match read {
            Some(_) => self.update(id, |r| {
                r.put(judge_signal(&radio));
                r.put(judge_limit(&radio));
                r.key = key.clone();
            }),
            None => self.update(id, |r| {
                r.put(timed_out(Layer::new("signal")));
                r.put(timed_out(Layer::new("limit")));
            }),
        }

        // its own statement: the slot's lock is let go before anything else takes it
        let blocked = |id: &str| {
            let slot = self.slot();
            slot.as_ref().and_then(|r| blocked_by(&r.layers, id))
        };

        // bearer: local reads only
        let by = blocked("bearer");
        if let Some(by) = by {
            self.update(id, |r| r.put(Layer::new("bearer").blocked(by)));
        } else {
            self.update(id, |r| r.running("bearer"));
            let h = Arc::clone(&self.host);
            let st = state.clone();
            let b = within(left(), move || judge_bearer(&h.bearer_inputs(st.as_ref())))
                .unwrap_or_else(|| timed_out(Layer::new("bearer")));
            self.update(id, |r| r.put(b));
        }

        // link, dns and e2e need the bearer; their probes run side by side
        let need_bearer = blocked("link");
        let net_rx = if need_bearer.is_none() {
            self.update(id, |r| {
                r.running("link");
                r.running("dns");
                r.running("e2e");
            });
            // a Finding probe just ran: use what it saw (its result is stored right after it frees the gate)
            let reused = if waited_finding {
                let t = Instant::now();
                loop {
                    // an observation that sent nothing (CHILL on, stale snapshot) is no answer
                    if let Some(n) = self.host.fresh_network().filter(|n| n.path_reachable != ResultState::Unknown) {
                        break Some(n);
                    }
                    if t.elapsed() >= FRESH_WAIT {
                        break None;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            } else {
                None
            };
            let (tx, rx) = mpsc::channel();
            match reused {
                Some(n) => {
                    let _ = tx.send(n);
                }
                None => {
                    let h = Arc::clone(&self.host);
                    let st = state.clone();
                    thread::spawn(move || {
                        let _ = tx.send(h.path_probe(st.as_ref()));
                    });
                }
            }
            let h = Arc::clone(&self.host);
            let local = thread::spawn(move || h.local_dns());
            // HTTPS beside them, on its own cap: it holds bound sockets like the path probe
            let (htx, hrx) = mpsc::channel();
            let h = Arc::clone(&self.host);
            let st = state.clone();
            thread::spawn(move || {
                let _ = htx.send(h.https_probe(st.as_ref(), PATH_MAX));
            });
            Some((rx, local, hrx))
        } else {
            None
        };

        // cellular link
        let probe = if need_bearer.is_none() && radio.connected && !dns.is_empty() {
            let d = dns.clone();
            let h = Arc::clone(&self.host);
            within(left(), move || h.link_probe(&d))
        } else {
            None
        };
        match need_bearer {
            Some(by) => self.update(id, |r| {
                for l in ["link", "dns", "e2e"] {
                    r.put(Layer::new(l).blocked(by));
                }
            }),
            None => self.update(id, |r| r.put(judge_link(radio.connected, !dns.is_empty(), probe.as_ref()))),
        }

        // dns + e2e: the bound path observation (its own cap) and the local resolver
        let mut net_pending = None;
        let mut https = None;
        if let Some((rx, local, hrx)) = net_rx {
            let t = Instant::now();
            let net = rx.recv_timeout(PATH_MAX).ok();
            if net.is_none() {
                net_pending = Some(rx);
            }
            https = hrx.recv_timeout(PATH_MAX.saturating_sub(t.elapsed())).ok().flatten();
            let local = local.join().ok().flatten();
            self.update(id, |r| {
                r.put(judge_dns(local.as_ref(), net.as_ref()));
                r.put(judge_e2e_https(net.as_ref(), https.as_ref()));
            });
        }

        // clock: verified HTTPS dates, else an HTTP Date against the device clock (one HEAD), else
        // the evidence's status
        let by = blocked("clock");
        if let Some(by) = by {
            self.update(id, |r| r.put(Layer::new("clock").blocked(by)));
        } else {
            self.update(id, |r| r.running("clock"));
            // verified HTTPS dates already judged the clock: no plain HTTP HEAD on top
            let decided = https.as_ref().is_some_and(|h| {
                h.verdict.clock == luyaz_api::execution::https::ClockTrust::Trusted
                    || h.verdict.why == "device_clock_off"
            });
            let h = Arc::clone(&self.host);
            let skew = if decided { None } else { within(left(), move || h.clock_skew()).flatten() };
            let st = self.host.clock_status();
            self.update(id, |r| r.put(judge_clock_https(st, skew, https.as_ref())));
        }

        // cell load, from history
        let by = blocked("crowd");
        if let Some(by) = by {
            self.update(id, |r| r.put(Layer::new("crowd").blocked(by)));
        } else {
            self.update(id, |r| r.running("crowd"));
            let now = now_unix();
            let events = self.host.events(now.saturating_sub(7 * 86_400), now + 1, &["diagnose", "speed"]);
            let (past, speeds) = past_from_events(&events);
            self.update(id, |r| r.put(judge_crowd(&radio, &key, probe.as_ref(), &past, &speeds)));
        }

        // proxy
        let has_proxy = self.slot().as_ref().is_some_and(|r| r.layers.iter().any(|l| l.id == "proxy"));
        if has_proxy {
            let by = blocked("proxy");
            if let Some(by) = by {
                self.update(id, |r| r.put(Layer::new("proxy").blocked(by)));
            } else {
                self.update(id, |r| r.running("proxy"));
                let h = Arc::clone(&self.host);
                let p = within(left(), move || h.proxy_probe());
                self.update(id, |r| r.put(judge_proxy(p.as_ref())));
            }
        }

        // an observation past its cap still holds bound sockets: the gate stays until it ends
        if let Some(rx) = net_pending {
            let _ = rx.recv_timeout(PATH_MAX);
        }
        self.gate().diag_since = None;
        let mut done = None;
        self.update(id, |r| {
            r.main = Some(pick_main(&r.layers));
            r.state = RunState::Done;
            r.finished_at = Some(now_unix());
            done = Some(r.clone());
        });
        if let Some(r) = done {
            let levels: serde_json::Map<String, Value> = r.layers.iter().map(|l| (l.id.to_string(), json!(l.level))).collect();
            let mut ev = key_json(&r.key);
            ev["id"] = json!(r.id);
            ev["main"] = json!(r.main.as_ref().map(|m| m.layer));
            ev["layers"] = Value::Object(levels);
            ev["link_ms"] = json!(probe.as_ref().and_then(LinkProbe::median));
            ev["loss"] = json!(probe.as_ref().map(|p| (p.loss() * 100.0).round() / 100.0));
            ev["in_use"] = json!(radio.rx_bps >= IN_USE_BPS);
            ev["from"] = json!(r.from);
            self.host.push_event("diagnose", ev);
        }
    }
}

fn view(r: &Run, now: u64) -> Value {
    let mut v = json!(r);
    v["age_s"] = json!(r.finished_at.map(|f| now.saturating_sub(f)));
    v
}

/// Run `f` on its own thread; None if it takes longer than `max`.
fn within<T: Send + 'static>(max: Duration, f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(max).ok()
}

/// The bearer layer from the state alone: `wan_status` and finding::path's addresses and paths.
pub fn bearer_base(state: Option<&Value>) -> BearerIn {
    let wan = state.and_then(|s| s["net"]["wan_status"].as_str()).unwrap_or("").trim();
    let paths = state.map(crate::finding::path::paths).unwrap_or_default();
    BearerIn {
        connected: (!wan.is_empty()).then(|| crate::wan_connected(wan)),
        has_ip: state.and_then(crate::finding::path::has_ip),
        iface: paths.first().map(|p| p.interface.clone()).unwrap_or_default(),
        ..BearerIn::default()
    }
}

/// `/proc/net/route`: is there a default route (any interface), and on which interfaces.
/// None = not a route table.
pub fn default_routes(text: &str) -> Option<Vec<String>> {
    let mut lines = text.lines();
    if !lines.next()?.starts_with("Iface") {
        return None;
    }
    let mut out: Vec<String> = Vec::new();
    for l in lines {
        let f: Vec<&str> = l.split_whitespace().collect();
        // Destination and Mask both 0; flags RTF_UP
        let up = f.get(3).and_then(|x| u32::from_str_radix(x, 16).ok()).is_some_and(|x| x & 1 != 0);
        let mask0 = f.get(7).is_none_or(|m| *m == "00000000");
        if f.len() >= 2 && f[1] == "00000000" && up && mask0 && !out.iter().any(|i| i == f[0]) {
            out.push(f[0].to_string());
        }
    }
    Some(out)
}

/// The other ways out: interfaces with a default route besides the cellular one, plus tunnels
/// that route by policy (Tailscale's table 52, CHILL's TUN) found among the network devices.
pub fn ways_out(cellular: &str, defaults: &[String], devices: &[String]) -> usize {
    // tun0, utun1, wg0…: a prefix and digits (not tunl0 / sit0, the kernel's idle ipip/sit devices)
    let numbered = |d: &str, p: &str| d.strip_prefix(p).is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    let tunnel = |d: &str| d.starts_with("tailscale") || ["tun", "utun", "wg"].iter().any(|p| numbered(d, p)) || d == "Meta";
    let mut ways: Vec<&str> = vec![cellular];
    for d in defaults.iter().map(String::as_str).chain(devices.iter().map(String::as_str).filter(|d| tunnel(d))) {
        if !ways.contains(&d) {
            ways.push(d);
        }
    }
    ways.len()
}

/// The effective rp_filter on an interface: the larger of `all` and its own (kernel rule).
pub fn rp_effective(all: Option<u8>, own: Option<u8>) -> Option<u8> {
    match (all, own) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0).max(b.unwrap_or(0))),
    }
}

/// The LAN bridge's own IPv4 address: its connected route in `/proc/net/route`, then the local
/// /32 inside that subnet in `/proc/net/fib_trie`.
pub fn lan_addr(route: &str, fib_trie: &str) -> Option<std::net::Ipv4Addr> {
    let hex = |h: &str| u32::from_str_radix(h, 16).ok().map(u32::from_be);
    let (net, mask) = route.lines().skip(1).find_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        (f.len() >= 8 && f[0] == "br-lan" && f[2] == "00000000").then(|| Some((hex(f[1])?, hex(f[7])?))).flatten()
    })?;
    let mut last: Option<std::net::Ipv4Addr> = None;
    for l in fib_trie.lines() {
        let t = l.trim_start_matches([' ', '|', '+', '-', '`']);
        if let Ok(a) = t.trim().parse::<std::net::Ipv4Addr>() {
            last = Some(a);
        } else if t.trim() == "/32 host LOCAL" {
            if let Some(a) = last.filter(|a| u32::from(*a) & mask == net & mask && mask != 0) {
                return Some(a);
            }
        }
    }
    None
}

/// Two names to the device's own resolver on one socket; only answers with an address count.
/// Loopback first (mihomo with CHILL on); stock dnsmasq leaves out `lo` and binds br-lan only,
/// so a refused loopback asks the LAN address instead (issue #6).
pub fn local_dns_probe() -> LocalDns {
    let (out, refused) = local_dns_at("127.0.0.1".parse().unwrap());
    if !refused {
        return out;
    }
    let read = |p| std::fs::read_to_string(p).ok();
    match read("/proc/net/route").zip(read("/proc/net/fib_trie")).and_then(|(r, t)| lan_addr(&r, &t)) {
        Some(a) => local_dns_at(a).0,
        None => out,
    }
}

/// One probe to `addr:53`; true when the port refused (nothing listening there).
fn local_dns_at(addr: std::net::Ipv4Addr) -> (LocalDns, bool) {
    let mut out = LocalDns { asked: 0, answered: 0, ms: None };
    let mut refused = false;
    let Ok(sock) = UdpSocket::bind((addr, 0)) else { return (out, refused) };
    if sock.connect((addr, 53)).is_err() {
        return (out, refused);
    }
    let base = (now_unix() as u16) ^ 0x6d6d;
    let qs: Vec<Vec<u8>> = LOCAL_DNS_NAMES.iter().enumerate().map(|(i, n)| dns_query(base.wrapping_add(i as u16), n)).collect();
    let start = Instant::now();
    for q in &qs {
        match sock.send(q) {
            Ok(_) => out.asked += 1,
            // the first query's port-unreachable can surface on the next send
            Err(e) => refused |= e.kind() == std::io::ErrorKind::ConnectionRefused,
        }
    }
    if refused {
        return (out, refused);
    }
    let mut got = vec![false; qs.len()];
    let mut buf = [0u8; 512];
    while out.answered < out.asked {
        let Some(left) = LOCAL_DNS_MAX.checked_sub(start.elapsed()).filter(|d| !d.is_zero()) else { break };
        if sock.set_read_timeout(Some(left)).is_err() {
            break;
        }
        let n = match sock.recv(&mut buf) {
            Ok(n) => n,
            Err(e) => {
                refused = e.kind() == std::io::ErrorKind::ConnectionRefused && out.answered == 0;
                break;
            }
        };
        for (i, q) in qs.iter().enumerate() {
            if !got[i] && crate::finding::path::dns_answer(&buf[..n], q) == Some(ResultState::Passed) {
                got[i] = true;
                out.answered += 1;
                out.ms = Some(start.elapsed().as_millis() as u32);
            }
        }
    }
    (out, refused)
}

/// Past runs and capped speed tests, from the history's events.
pub fn past_from_events(events: &[Event]) -> (Vec<PastRun>, Vec<PastSpeed>) {
    let ev: Vec<(String, Value)> =
        events.iter().map(|e| (e.kind.clone(), Value::Object(e.data.clone().into_iter().collect()))).collect();
    past_from(&ev)
}

/// A number from core: JSON number or numeric string, finite.
pub fn num(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|x: &f64| x.is_finite())
}

/// Radio inputs from one `/state` + `/v2/screen`.
pub fn radio_from(state: Option<&Value>, screen: Option<&Value>) -> (Radio, CellKey, Vec<String>, u64) {
    let now = now_unix();
    let reading = Reading::parse(now, state, screen, None);
    let net = screen.and_then(|s| s.get("net"));
    let story = net.and_then(|n| n.get("story")).cloned();
    let tier = screen.and_then(|s| s.get("net")).and_then(|n| n.get("bars_tier")).and_then(Value::as_i64).filter(|t| *t >= 0);
    let nr = reading.rat == "SA";
    let pick = |a: Option<f64>, b: Option<f64>| if nr { a } else { b };
    let qos = state.and_then(|s| s.get("qos"));
    let dns = state.map(operator_dns).unwrap_or_default();
    let radio = Radio {
        story,
        rsrp: pick(reading.nr_rsrp, reading.lte_rsrp),
        sinr: pick(reading.nr_sinr, reading.lte_sinr),
        rsrq: pick(reading.nr_rsrq, reading.lte_rsrq),
        tier,
        nr,
        ambr_dl: num(qos.and_then(|q| q.get("ambr_dl"))).filter(|x| *x > 0.0),
        qci: reading.qci,
        qos_stale: qos.and_then(|q| q.get("stale")).and_then(Value::as_bool).unwrap_or(false),
        qos_nr: qos.and_then(|q| q.get("bearer")).and_then(Value::as_str) == Some("nr_pdu"),
        connected: reading.connected,
        rx_bps: state.and_then(|s| s.get("traffic")).and_then(|t| t.get("rx_speed")).and_then(Value::as_i64).unwrap_or(0),
        // "↓ n41 100M + B3 20M   ↑ n41" → the downlink part
        carriers: net
            .and_then(|n| n.get("ca_sub"))
            .and_then(Value::as_str)
            .map(|c| c.split('↑').next().unwrap_or("").trim().trim_start_matches('↓').trim().to_string())
            .unwrap_or_default(),
        bw_mhz: net.and_then(|n| n.get("act_bw")).and_then(Value::as_i64).filter(|b| *b > 0),
    };
    let key = CellKey { plmn: reading.plmn, cell: reading.cell, ch: reading.channel, hour: ((now % 86_400) / 3600) as u8 };
    (radio, key, dns, now)
}

/// Ten queries, ~0.35 s apart, on one socket; replies collected until 1.5 s after the last send.
/// About 5 s in all.
pub fn link_probe(dns: &[String]) -> LinkProbe {
    let Some(ip) = dns.first() else { return LinkProbe::default() };
    let Ok(addr) = format!("{ip}:53").parse::<SocketAddr>() else { return LinkProbe::default() };
    let Ok(sock) = UdpSocket::bind("0.0.0.0:0") else { return LinkProbe::default() };
    let base = (now_unix() as u16) ^ 0x3c3c;
    let mut sent_at: Vec<Option<Instant>> = vec![None; LINK_PROBES];
    let mut got: Vec<Option<u32>> = vec![None; LINK_PROBES];
    let start = Instant::now();
    let gap = Duration::from_millis(350);
    let end = start + gap * (LINK_PROBES as u32 - 1) + Duration::from_millis(1500);
    let mut next = 0;
    let mut buf = [0u8; 512];
    while Instant::now() < end {
        if next < LINK_PROBES && Instant::now() >= start + gap * next as u32 {
            let q = dns_query(base.wrapping_add(next as u16), "www.qq.com");
            if sock.send_to(&q, addr).is_ok() {
                sent_at[next] = Some(Instant::now());
            }
            next += 1;
        }
        let wait = if next < LINK_PROBES { (start + gap * next as u32).saturating_duration_since(Instant::now()) } else { end.saturating_duration_since(Instant::now()) };
        let _ = sock.set_read_timeout(Some(wait.max(Duration::from_millis(5))));
        if let Ok((n, from)) = sock.recv_from(&mut buf) {
            if from == addr && n >= 12 && buf[2] & 0x80 != 0 {
                let id = u16::from_be_bytes([buf[0], buf[1]]).wrapping_sub(base) as usize;
                if id < LINK_PROBES {
                    if let (Some(t), None) = (sent_at[id], got[id]) {
                        got[id] = Some(t.elapsed().as_millis() as u32);
                    }
                }
            }
        }
    }
    LinkProbe { sent: sent_at.iter().filter(|t| t.is_some()).count(), rtts: got.into_iter().flatten().collect() }
}

/// Every station on every AP interface (`iw`), with the band of the interface it is on. Bounded
/// per call and single-flight with u60d's own reads (resilience audit R6).
pub fn real_stations() -> Vec<luyaz_api::netinfo::Station> {
    luyaz_api::netinfo::iw::stations()
}

/// The Wi-Fi layer's view of the stations, named from the leases text (`/tmp/dhcp.leases`).
pub fn stations_named(stations: Vec<luyaz_api::netinfo::Station>, leases: &str) -> Vec<Sta> {
    let names: Vec<(String, String)> = luyaz_api::netinfo::parse_leases(leases)
        .into_iter()
        .filter(|(_, _, n)| !n.is_empty())
        .map(|(m, _, n)| (m, n))
        .collect();
    stations
        .into_iter()
        .map(|s| Sta {
            name: names.iter().find(|(m, _)| *m == s.mac).map(|(_, n)| n.clone()).unwrap_or_default(),
            signal: s.signal(),
            rate_mbps: s.link_down_mbps(),
            tx_packets: s.tx_packets(),
            tx_retries: s.tx_retries(),
            mac: s.mac,
        })
        .collect()
}

/// The speed row from a finished capped test. The device's own requests go out directly (the
/// proxy only carries LAN clients), so the route is "direct".
pub fn speed_row(p: &SpeedTestProgress) -> Layer {
    let l = Layer::new("speed");
    match (p.download_mbps(), p.failure()) {
        (Some(m), _) => l.set(Level::Info, format!("直连 ↓ {m:.0} Mbps"), format!("Direct ↓ {m:.0} Mbps")),
        (None, Some(f)) if f == "cancelled" => l.na("被停下", "Stopped"),
        _ => l.na("没测成", "Didn't finish"),
    }
}

#[cfg(test)]
mod tests;
