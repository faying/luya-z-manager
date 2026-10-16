//! Deep diagnosis' runner: "why is it slow?" on request (`docs/designs/slow-diagnosis.md` §4.2,
//! §12.3, §12.5; eng review 10-02 D4/D8/D10/D11/D12). Moved from zte-agent's `deep_diag.rs`
//! (u60-platform.md Phase 4 step 9b) so the agent and u60d run the same one; the judgements are
//! u60-api's (`luyaz_api::diagnose`), every device read goes through a [`Host`].
//!
//! `start` begins one run (about 10 s, ≤ 5 s per layer, ≤ 20 s in all); `get` reads it. One run
//! at a time: a second start while one is waiting or running gets that same run. A finished run
//! is kept 10 minutes.
//!
//! Layers, in the order the main cause is picked from: Wi-Fi, signal, speed cap, cellular link,
//! cell load, proxy. Each ends ok / warn / bad / na ("can't tell", never the main cause).
//!
//! Sharing the modem and the uplink (D4/D10): one gate, handed in by the caller because the same
//! lock guards its search/register and speed test. A run that finds one of those going waits for
//! it (up to 120 s) and starts by itself; while a run is going they are refused at once
//! (`luyaz_api::diagnose::refusal_at`). Both sides check and claim under the gate, so neither can
//! slip in between. The run does not drive the modem.
//!
//! What it sends: 10 DNS queries to the operator's resolver, and, with the proxy on, one health
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

use crate::record::{Event, Reading};
use crate::{dns_query, now_unix, operator_dns};

/// What a run reads and where it writes, on the agent or in u60d.
pub trait Host: Send + Sync + 'static {
    /// datad's state in the old `/state` shape and the home screen (`/v2/screen`), one read each.
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
    /// Into the signal history (`diagnose`, `speed`, `diagnose_feedback`).
    fn push_event(&self, kind: &str, data: Value);
    /// History events with `from <= t < to`.
    fn events(&self, from: u64, to: u64) -> Vec<Event>;
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
            if let Some(m) = p.download_mbps() {
                let mut ev = key_json(&key);
                ev["id"] = json!(id);
                ev["mbps"] = json!(m);
                ev["bytes"] = json!(p.download_bytes());
                ev["route"] = json!("direct");
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
        loop {
            let mut g = self.gate();
            match self.host.others_busy() {
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

        // signal + cap, one read of datad
        self.update(id, |r| {
            r.running("signal");
            r.running("limit");
        });
        let h = Arc::clone(&self.host);
        let read = within(left(), move || {
            let (st, sc) = h.radio_inputs();
            radio_from(st.as_ref(), sc.as_ref())
        });
        let (radio, key, dns, _) = read.clone().unwrap_or_default();
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

        // cellular link
        self.update(id, |r| r.running("link"));
        let probe = if radio.connected && !dns.is_empty() {
            let d = dns.clone();
            let h = Arc::clone(&self.host);
            within(left(), move || h.link_probe(&d))
        } else {
            None
        };
        self.update(id, |r| r.put(judge_link(radio.connected, !dns.is_empty(), probe.as_ref())));

        // cell load, from history
        self.update(id, |r| r.running("crowd"));
        let now = now_unix();
        let events = self.host.events(now.saturating_sub(7 * 86_400), now + 1);
        let (past, speeds) = past_from_events(&events);
        self.update(id, |r| r.put(judge_crowd(&radio, &key, probe.as_ref(), &past, &speeds)));

        // proxy
        let has_proxy = self.slot().as_ref().is_some_and(|r| r.layers.iter().any(|l| l.id == "proxy"));
        if has_proxy {
            self.update(id, |r| r.running("proxy"));
            let h = Arc::clone(&self.host);
            let p = within(left(), move || h.proxy_probe());
            self.update(id, |r| r.put(judge_proxy(p.as_ref())));
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

/// Past runs and capped speed tests, from the history's events.
pub fn past_from_events(events: &[Event]) -> (Vec<PastRun>, Vec<PastSpeed>) {
    let ev: Vec<(String, Value)> =
        events.iter().map(|e| (e.kind.clone(), Value::Object(e.data.clone().into_iter().collect()))).collect();
    past_from(&ev)
}

/// A number from datad: JSON number or numeric string, finite.
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
    let story = screen.and_then(|s| s.get("net")).and_then(|n| n.get("story")).cloned();
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
        connected: reading.connected,
        rx_bps: state.and_then(|s| s.get("traffic")).and_then(|t| t.get("rx_speed")).and_then(Value::as_i64).unwrap_or(0),
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

/// Every station on every AP interface (`iw`), with the band of the interface it is on.
pub fn real_stations() -> Vec<luyaz_api::netinfo::Station> {
    use luyaz_api::netinfo::{ap_ifaces, parse_ap_info, parse_station_dump};
    let sh = |cmd: &str| {
        std::process::Command::new("sh")
            .args(["-c", cmd])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    };
    let mut stations = Vec::new();
    for iface in ap_ifaces(&sh("iw dev 2>/dev/null")) {
        let ap = parse_ap_info(&sh(&format!("iw dev {iface} info 2>/dev/null")));
        for mut st in parse_station_dump(&sh(&format!("iw dev {iface} station dump 2>/dev/null")), &iface) {
            st.band = ap.band;
            st.channel = ap.channel;
            st.width_mhz = ap.width_mhz;
            stations.push(st);
        }
    }
    stations
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
