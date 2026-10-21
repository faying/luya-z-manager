//! `power.tailnet_poller` (docs/designs/tailnet-poller.md): someone keeps polling the device's web
//! ports over Tailscale while the screen is off, which keeps Tailscale's send gate (45 s) open and
//! the cellular link out of idle. Only told, never acted on (decision-execution-loop §1.3).
//!
//! - [`Machine`] is the sans-IO state machine (§3): one [`Minute`] in per check, the events it
//!   opened out. Fixture tests in `poller/tests.rs` (§6).
//! - Input (§2, corrected 10-21): the design meant to read the resident minute ledger's
//!   `Minute.tailnet`, but the normal core (`luyaz-core serve`) does not run that ledger (it is the
//!   separate :9470 mode, docs/research/power-measurement.md), subnet-routed flows
//!   (`10.5.8.88 → 10.5.6.1`) land in its LAN class, and an inbound row's `port` is the client's.
//!   So [`Reader`] dumps conntrack itself, once per long-term check (60 s, no new timer), and only
//!   on screen-off, not-charging checks; the socket is closed otherwise.
//! - A sample: an inbound flow to a local address on [`PORTS`] (not on a cellular interface) from a
//!   tailnet address (100.64/10, fd7a:115c:a1e0::/48) or a private address outside every local
//!   LAN subnet (a peer behind a subnet router, seen as the router). Packets are conntrack's, both
//!   directions, summed per (peer, port).
//! - Allow list [`ALLOW_FILE`]: read only when a run would open an event; written by the
//!   `poller.allow` / `poller.unallow` `/control` actions. `poller.ack` hides the banner.
//! - An opened event becomes one alert (`tailnet-poller`, once per event id).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::Path;
use std::sync::Mutex;

#[cfg(test)]
mod tests;

pub const KIND: &str = "power.tailnet_poller";
/// The alert queue's kind (lowercase, digits and `-` only).
pub const ALERT_KIND: &str = "tailnet-poller";
pub const ALLOW_FILE: &str = "/data/luyaz/core/poller-allow.json";
/// The events (and acks) across a core restart; written only when one changes.
pub const STATE_FILE: &str = "/data/luyaz/core/poller-state.json";
/// Watched local ports: the stock web (`/ubus`) and luyaz-web. Not :2222 (interactive ssh).
pub const PORTS: [u16; 2] = [80, 9090];
/// Packets a minute from one peer to one port that count as polling.
pub const MIN_PKTS: u64 = 20;
/// Polling minutes in a run before an event opens.
pub const RUN_MIN: u32 = 10;
/// Minutes without a packet that end an event.
pub const END_EMPTY: u32 = 3;
/// An event that ended this long ago or less takes the same peer back without a new alert.
pub const MERGE_S: i64 = 1800;
/// "是我自己的" without a duration.
pub const ALLOW_DEFAULT_S: i64 = 86_400;
/// Checks further apart than this (monotonic) break a run.
pub const MAX_GAP_S: i64 = 120;
/// Ended events kept for the view.
const KEEP_ENDED: usize = 5;
/// Allow-list entries at most.
pub const ALLOW_MAX: usize = 32;

/// One (peer, port) packet count of a minute.
#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    pub peer: String,
    pub port: u16,
    pub pkts: u64,
}

/// One check.
#[derive(Clone, Debug)]
pub struct Minute<'a> {
    pub wall: i64,
    pub mono: i64,
    /// Screen off and not charging, both known, and a reading taken (a first, baseline reading
    /// is not counted).
    pub counted: bool,
    pub samples: &'a [Sample],
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Event {
    pub id: String,
    pub peer: String,
    pub peer_name: Option<String>,
    /// The peer is not a tailnet address: a subnet router forwarded it, the real client is behind.
    pub via_gateway: bool,
    pub port: u16,
    pub since: i64,
    pub last: i64,
    /// Polling minutes so far.
    pub minutes: u32,
    /// Median of the polling minutes.
    pub pkts_per_min: u64,
    pub state: &'static str,
    pub ended_at: Option<i64>,
    pub acked: bool,
    pub allowed: bool,
    #[serde(skip)]
    counts: Vec<u64>,
    #[serde(skip)]
    empty: u32,
}

#[derive(Clone, Debug, Default)]
struct Track {
    run: u32,
    miss: u32,
    since: i64,
    counts: Vec<u64>,
}

#[derive(Debug, Default)]
pub struct Machine {
    prefix: String,
    tracks: BTreeMap<(String, u16), Track>,
    events: Vec<Event>,
    last_mono: Option<i64>,
    opened: u64,
}

fn median(v: &[u64]) -> u64 {
    let mut s = v.to_vec();
    s.sort_unstable();
    s.get(s.len() / 2).copied().unwrap_or(0)
}

pub fn is_tailnet(a: &IpAddr) -> bool {
    match a {
        IpAddr::V4(v) => v.octets()[0] == 100 && (v.octets()[1] & 0xc0) == 64,
        IpAddr::V6(v) => v.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}

impl Machine {
    /// `prefix`: this boot's 8 hex digits, the alert id's first part.
    pub fn new(prefix: &str) -> Machine {
        Machine {
            prefix: prefix.to_string(),
            ..Machine::default()
        }
    }

    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// One check in; the events it opened out (each to be told once). `allowed(peer, port)` is
    /// asked only when a run reaches [`RUN_MIN`]: an allowed one opens nothing.
    pub fn feed(&mut self, m: &Minute, mut allowed: impl FnMut(&str, u16) -> bool) -> Vec<Event> {
        if self.last_mono.is_some_and(|t| m.mono - t > MAX_GAP_S) {
            self.tracks.clear();
        }
        self.last_mono = Some(m.mono);
        // an active event nobody saw for a while (screen on, charging) ends where it was last seen
        for e in self.events.iter_mut().filter(|e| e.state == "active") {
            if m.wall - e.last > MERGE_S {
                e.state = "ended";
                e.ended_at = Some(e.last);
            }
        }
        if !m.counted {
            return Vec::new();
        }
        let mut agg: BTreeMap<(String, u16), u64> = BTreeMap::new();
        for s in m.samples.iter().filter(|s| PORTS.contains(&s.port)) {
            *agg.entry((s.peer.clone(), s.port)).or_default() += s.pkts;
        }
        let got = |k: &(String, u16)| agg.get(k).copied().unwrap_or(0);

        // events first: they own their (peer, port)
        let mut owned = Vec::new();
        for e in self.events.iter_mut() {
            let k = (e.peer.clone(), e.port);
            let n = got(&k);
            match e.state {
                "active" => {
                    owned.push(k);
                    if n > 0 {
                        e.empty = 0;
                        e.last = m.wall;
                    } else {
                        e.empty += 1;
                        if e.empty >= END_EMPTY {
                            e.state = "ended";
                            e.ended_at = Some(e.last);
                        }
                    }
                    if n >= MIN_PKTS {
                        e.counts.push(n);
                        e.minutes += 1;
                        e.pkts_per_min = median(&e.counts);
                    }
                }
                _ if n >= MIN_PKTS
                    && e.ended_at.is_some_and(|t| m.wall - t <= MERGE_S)
                    && !owned.contains(&k) =>
                {
                    // back within 30 minutes: the same event, not told again
                    owned.push(k);
                    e.state = "active";
                    e.ended_at = None;
                    e.empty = 0;
                    e.last = m.wall;
                    e.counts.push(n);
                    e.minutes += 1;
                    e.pkts_per_min = median(&e.counts);
                }
                _ => {}
            }
        }

        let mut keys: Vec<(String, u16)> = agg.keys().cloned().collect();
        keys.extend(self.tracks.keys().cloned());
        keys.sort();
        keys.dedup();
        let mut opened = Vec::new();
        for k in keys {
            if owned.contains(&k) {
                self.tracks.remove(&k);
                continue;
            }
            let n = got(&k);
            if n >= MIN_PKTS {
                let t = self.tracks.entry(k.clone()).or_insert_with(|| Track {
                    since: m.wall,
                    ..Track::default()
                });
                t.run += 1;
                t.miss = 0;
                t.counts.push(n);
            } else if let Some(t) = self.tracks.get_mut(&k) {
                // one short minute in a run is let through (not counted); the second ends it
                t.miss += 1;
                if t.miss > 1 {
                    self.tracks.remove(&k);
                }
                continue;
            } else {
                continue;
            }
            let t = &self.tracks[&k];
            if t.run < RUN_MIN {
                continue;
            }
            let t = self.tracks.remove(&k).expect("just read");
            if allowed(&k.0, k.1) {
                continue;
            }
            let peer_ip = k.0.parse::<IpAddr>().ok();
            let e = Event {
                // `<boot8>-<digits>`: since × 100 + a counter, ≤ 12 digits until 2286
                id: format!(
                    "{}-{}",
                    self.prefix,
                    t.since.max(0) * 100 + (self.opened % 100) as i64
                ),
                via_gateway: !peer_ip.as_ref().is_some_and(is_tailnet),
                peer: k.0.clone(),
                peer_name: None,
                port: k.1,
                since: t.since,
                last: m.wall,
                minutes: t.run,
                pkts_per_min: median(&t.counts),
                state: "active",
                ended_at: None,
                acked: false,
                allowed: false,
                counts: t.counts,
                empty: 0,
            };
            self.opened += 1;
            opened.push(e.clone());
            self.events.push(e);
        }
        // ended events: the newest few stay for the view
        let ended = self.events.iter().filter(|e| e.state == "ended").count();
        if ended > KEEP_ENDED {
            let mut drop = ended - KEEP_ENDED;
            self.events.retain(|e| {
                if drop > 0 && e.state == "ended" {
                    drop -= 1;
                    false
                } else {
                    true
                }
            });
        }
        opened
    }

    /// What the state file keeps changing on: an event opened, ended, came back, acked, allowed,
    /// named (not every minute's count).
    pub fn mark(&self) -> String {
        self.events
            .iter()
            .map(|e| {
                format!(
                    "{}/{}/{}/{}/{:?}/{:?};",
                    e.id, e.state, e.acked, e.allowed, e.ended_at, e.peer_name
                )
            })
            .collect()
    }

    /// The state file's text: the events in full (counts too) and the id counter.
    pub fn save(&self) -> String {
        let evs: Vec<Value> = self
            .events
            .iter()
            .map(|e| {
                let mut v = serde_json::to_value(e).unwrap_or(Value::Null);
                v["counts"] = json!(e.counts);
                v["empty"] = json!(e.empty);
                v
            })
            .collect();
        json!({"v": 1, "opened": self.opened, "events": evs}).to_string()
    }

    /// A machine from the state file's text; missing or unreadable parts are left out (a bad file
    /// is a fresh start). Runs are not kept: a restart is a gap.
    pub fn restore(prefix: &str, text: &str) -> Machine {
        let mut m = Machine::new(prefix);
        let Ok(v) = serde_json::from_str::<Value>(text) else {
            return m;
        };
        m.opened = v["opened"].as_u64().unwrap_or(0);
        let evs = v["events"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        for e in evs {
            let state = match e["state"].as_str() {
                Some("active") => "active",
                Some("ended") => "ended",
                _ => continue,
            };
            let (Some(id), Some(peer), Some(port)) = (
                e["id"].as_str(),
                e["peer"].as_str(),
                e["port"].as_u64().and_then(|p| u16::try_from(p).ok()),
            ) else {
                continue;
            };
            let int = |k: &str| e[k].as_i64().unwrap_or(0);
            m.events.push(Event {
                id: id.into(),
                peer: peer.into(),
                peer_name: e["peer_name"].as_str().map(str::to_string),
                via_gateway: e["via_gateway"].as_bool().unwrap_or(true),
                port,
                since: int("since"),
                last: int("last"),
                minutes: int("minutes").clamp(0, u32::MAX as i64) as u32,
                pkts_per_min: e["pkts_per_min"].as_u64().unwrap_or(0),
                state,
                ended_at: e["ended_at"].as_i64(),
                acked: e["acked"] == true,
                allowed: e["allowed"] == true,
                counts: e["counts"]
                    .as_array()
                    .map(|c| c.iter().filter_map(Value::as_u64).collect())
                    .unwrap_or_default(),
                empty: int("empty").clamp(0, u32::MAX as i64) as u32,
            });
        }
        m
    }

    pub fn set_name(&mut self, id: &str, name: Option<String>) {
        if let Some(e) = self.events.iter_mut().find(|e| e.id == id) {
            e.peer_name = name;
        }
    }

    /// "知道了": the banner goes; whether there was such an event.
    pub fn ack(&mut self, id: &str) -> bool {
        self.events
            .iter_mut()
            .find(|e| e.id == id)
            .map(|e| e.acked = true)
            .is_some()
    }

    /// "是我自己的": every event of (peer, port) is marked allowed (and so off the banner).
    pub fn mark_allowed(&mut self, peer: &str, port: u16, on: bool) {
        for e in self
            .events
            .iter_mut()
            .filter(|e| e.peer == peer && e.port == port)
        {
            e.allowed = on;
        }
    }
}

// ---- the allow list ----

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Allow {
    pub peer: String,
    pub port: u16,
    #[serde(default)]
    pub label: String,
    /// Wall seconds; None = for good.
    #[serde(default)]
    pub until: Option<i64>,
}

pub fn parse_allow(text: &str) -> Vec<Allow> {
    serde_json::from_str(text).unwrap_or_default()
}

pub fn load_allow(path: &Path) -> Vec<Allow> {
    std::fs::read_to_string(path)
        .map(|t| parse_allow(&t))
        .unwrap_or_default()
}

pub fn allowed(list: &[Allow], peer: &str, port: u16, now: i64) -> bool {
    list.iter()
        .any(|a| a.peer == peer && a.port == port && a.until.is_none_or(|u| u > now))
}

/// Adds or replaces (peer, port); expired entries go. Err = the list is full.
pub fn upsert(list: &mut Vec<Allow>, a: Allow, now: i64) -> Result<(), String> {
    list.retain(|x| x.until.is_none_or(|u| u > now) && !(x.peer == a.peer && x.port == a.port));
    if list.len() >= ALLOW_MAX {
        return Err(format!("at most {ALLOW_MAX} entries"));
    }
    list.push(a);
    Ok(())
}

/// Whether (peer, port) was there.
pub fn remove(list: &mut Vec<Allow>, peer: &str, port: u16) -> bool {
    let n = list.len();
    list.retain(|x| !(x.peer == peer && x.port == port));
    list.len() != n
}

fn save_allow(path: &Path, list: &[Allow]) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(list).unwrap_or_default())?;
    std::fs::rename(&tmp, path)
}

// ---- reading conntrack ----

/// The device's interface addresses with their prefix lengths.
#[derive(Clone, Debug, Default)]
pub struct Nets {
    /// (interface, address, prefix length)
    pub addrs: Vec<(String, IpAddr, u8)>,
}

fn private(a: &IpAddr) -> bool {
    match a {
        IpAddr::V4(v) => v.is_private(),
        IpAddr::V6(v) => (v.segments()[0] & 0xfe00) == 0xfc00,
    }
}

fn same_net(a: &IpAddr, b: &IpAddr, len: u8) -> bool {
    match (a, b) {
        (IpAddr::V4(x), IpAddr::V4(y)) => {
            let m = if len == 0 {
                0
            } else {
                u32::MAX << (32 - len.min(32))
            };
            (u32::from(*x) & m) == (u32::from(*y) & m)
        }
        (IpAddr::V6(x), IpAddr::V6(y)) => {
            let m = if len == 0 {
                0
            } else {
                u128::MAX << (128 - len.min(128))
            };
            (u128::from(*x) & m) == (u128::from(*y) & m)
        }
        _ => false,
    }
}

impl Nets {
    fn local(&self, a: &IpAddr) -> Option<&str> {
        self.addrs
            .iter()
            .find(|(_, x, _)| x == a)
            .map(|(i, _, _)| i.as_str())
    }

    /// On one of the device's own LAN subnets (any interface but tailscale and cellular).
    fn on_lan(&self, a: &IpAddr) -> bool {
        self.addrs.iter().any(|(i, x, len)| {
            !i.starts_with("tailscale") && !i.starts_with("rmnet") && same_net(a, x, *len)
        })
    }
}

/// The samples of one minute's flow deltas (`(orig src, orig dst, dst port, Δpackets)`).
pub fn pick(deltas: &[(IpAddr, IpAddr, u16, u64)], nets: &Nets) -> Vec<Sample> {
    let mut out: Vec<Sample> = Vec::new();
    for (src, dst, dport, n) in deltas {
        if *n == 0 || !PORTS.contains(dport) || src.is_loopback() {
            continue;
        }
        match nets.local(dst) {
            Some(i) if !i.starts_with("rmnet") => {}
            _ => continue,
        }
        if !(is_tailnet(src) || (private(src) && !nets.on_lan(src))) {
            continue;
        }
        let peer = src.to_string();
        match out.iter_mut().find(|s| s.peer == peer && s.port == *dport) {
            Some(s) => s.pkts += n,
            None => out.push(Sample {
                peer,
                port: *dport,
                pkts: *n,
            }),
        }
    }
    out
}

fn nets() -> Nets {
    let mut n = Nets::default();
    let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut ifap) } != 0 {
        return n;
    }
    let mut p = ifap;
    while !p.is_null() {
        let ifa = unsafe { &*p };
        p = ifa.ifa_next;
        if ifa.ifa_addr.is_null() || ifa.ifa_netmask.is_null() {
            continue;
        }
        let fam = unsafe { (*ifa.ifa_addr).sa_family } as i32;
        let (ip, len) = match fam {
            libc::AF_INET => {
                let a = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in) };
                let m = unsafe { &*(ifa.ifa_netmask as *const libc::sockaddr_in) };
                (
                    IpAddr::V4(std::net::Ipv4Addr::from(u32::from_be(a.sin_addr.s_addr))),
                    u32::from_be(m.sin_addr.s_addr).count_ones() as u8,
                )
            }
            libc::AF_INET6 => {
                let a = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in6) };
                let m = unsafe { &*(ifa.ifa_netmask as *const libc::sockaddr_in6) };
                (
                    IpAddr::V6(std::net::Ipv6Addr::from(a.sin6_addr.s6_addr)),
                    u128::from_be_bytes(m.sin6_addr.s6_addr).count_ones() as u8,
                )
            }
            _ => continue,
        };
        let name = unsafe { std::ffi::CStr::from_ptr(ifa.ifa_name) }
            .to_string_lossy()
            .into_owned();
        n.addrs.push((name, ip, len));
    }
    unsafe { libc::freeifaddrs(ifap) };
    n
}

/// Conntrack, open only across counted checks.
#[derive(Default)]
pub struct Reader {
    ct: Option<crate::nl::Conntrack>,
    deltas: crate::ledger::FlowDeltas,
    primed: bool,
}

impl Reader {
    /// This check's samples; None = not counted (screen on, charging, unknown, the first
    /// reading after a pause, or conntrack not readable).
    pub fn read(&mut self, counted: bool) -> Option<Vec<Sample>> {
        if !counted {
            *self = Reader::default();
            return None;
        }
        if self.ct.is_none() {
            self.ct = crate::nl::Conntrack::open(256 << 10).ok();
        }
        let ct = self.ct.as_mut()?;
        let flows = ct.dump().ok()?;
        let (destroyed, _) = ct.destroyed();
        let first = !self.primed;
        self.primed = true;
        let d: Vec<_> = self
            .deltas
            .step(&flows, &destroyed, first)
            .into_iter()
            .map(|(f, n, _)| (f.orig.src, f.orig.dst, f.orig.dport, n))
            .collect();
        if first {
            return None;
        }
        Some(pick(&d, &nets()))
    }
}

// ---- the live state (the long-term thread feeds it, the API reads and acks it) ----

struct Live {
    m: Machine,
    reader: Reader,
}

static LIVE: Mutex<Option<Live>> = Mutex::new(None);

fn state_path() -> std::path::PathBuf {
    crate::state::host_path(STATE_FILE).into()
}

/// The live machine (restored from [`STATE_FILE`] on first use); the file is rewritten (tmp +
/// rename) when `f` changed an event's [`Machine::mark`].
fn with<T>(f: impl FnOnce(&mut Live) -> T) -> T {
    let mut g = LIVE.lock().unwrap_or_else(|e| e.into_inner());
    let l = g.get_or_insert_with(|| Live {
        m: Machine::restore(
            &crate::api::hot_drain::boot8(),
            &std::fs::read_to_string(state_path()).unwrap_or_default(),
        ),
        reader: Reader::default(),
    });
    let before = l.m.mark();
    let r = f(l);
    if l.m.mark() != before
        && let Err(e) = save_state(&state_path(), &l.m.save())
    {
        eprintln!("[poller] {STATE_FILE}: {e}");
    }
    r
}

fn save_state(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

fn allow_path() -> std::path::PathBuf {
    crate::state::host_path(ALLOW_FILE).into()
}

/// A tailnet peer's node name from `tailscale status --json` (`Peer[].TailscaleIPs`).
pub fn name_of(status: &Value, peer: &str) -> Option<String> {
    status["Peer"].as_object()?.values().find_map(|p| {
        p["TailscaleIPs"]
            .as_array()?
            .iter()
            .any(|ip| ip.as_str() == Some(peer))
            .then(|| {
                p["HostName"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .or(p["DNSName"].as_str())
                    .map(|s| s.trim_end_matches('.').to_string())
            })
            .flatten()
    })
}

fn alert_text(e: &Event) -> String {
    let who = match (&e.peer_name, e.via_gateway) {
        (Some(n), _) => format!("{n}（{}）", e.peer),
        (None, true) => format!("{}（经网关转发）", e.peer),
        (None, false) => e.peer.clone(),
    };
    format!(
        "{who} 经 Tailscale 持续访问 :{}，已 {} 分钟（约 {} 包/分钟），息屏约多耗 0.3–0.6 W",
        e.port, e.minutes, e.pkts_per_min
    )
}

/// One long-term check (the long-term thread, every 60 s): read, feed, tell what opened.
pub fn check(wall: i64, mono: i64, screen_on: Option<bool>, charging: Option<bool>) {
    let counted = screen_on == Some(false) && charging == Some(false);
    let opened = with(|l| {
        let samples = l.reader.read(counted);
        let m = Minute {
            wall,
            mono,
            counted: samples.is_some(),
            samples: samples.as_deref().unwrap_or(&[]),
        };
        let mut list = None;
        let path = allow_path();
        l.m.feed(&m, |peer, port| {
            let list = list.get_or_insert_with(|| load_allow(&path));
            allowed(list, peer, port, wall)
        })
    });
    if opened.is_empty() {
        return;
    }
    let status = opened
        .iter()
        .any(|e| !e.via_gateway)
        .then(crate::api::home::tailscale_status)
        .flatten();
    let dir = crate::state::host_path(luyaz_api::alerts::DIR);
    let a = luyaz_api::alerts::Alerts::at(Path::new(&dir));
    let up = std::fs::read_to_string(crate::state::host_path("/proc/uptime"))
        .ok()
        .and_then(|s| s.split('.').next()?.parse::<u64>().ok())
        .unwrap_or(0);
    for mut e in opened {
        e.peer_name = status.as_ref().and_then(|s| name_of(s, &e.peer));
        with(|l| l.m.set_name(&e.id, e.peer_name.clone()));
        let text = alert_text(&e);
        eprintln!("[poller] {KIND} {}: {text}", e.id);
        if let Err(err) = a.add_once(&e.id, ALERT_KIND, &text, wall, up) {
            eprintln!("[poller] alert: {err}");
        }
    }
}

/// `GET /api/poller` and `/api/diagnose`'s `tailnet_poller`.
pub fn view() -> Value {
    let now = luyaz_api::alerts::now();
    let mut allow = load_allow(&allow_path());
    allow.retain(|a| a.until.is_none_or(|u| u > now));
    let events: Vec<Value> = with(|l| {
        l.m.events()
            .iter()
            .map(|e| {
                let mut v = serde_json::to_value(e).unwrap_or(Value::Null);
                v["kind"] = json!(KIND);
                // a later allow-list entry hides it too
                if allowed(&allow, &e.peer, e.port, now) {
                    v["allowed"] = json!(true);
                }
                v
            })
            .collect()
    });
    json!({
        "events": events,
        "allow": allow,
        "ports": PORTS,
        "min_pkts": MIN_PKTS,
        "run_min": RUN_MIN,
        "allow_default_s": ALLOW_DEFAULT_S,
    })
}

/// `/control` `poller.allow` / `poller.unallow` / `poller.ack`; None = not one of these.
pub fn control(action: &str, params: &Value) -> Option<Result<Value, String>> {
    let now = luyaz_api::alerts::now();
    let peer = || {
        params["peer"]
            .as_str()
            .filter(|p| p.parse::<IpAddr>().is_ok())
            .map(str::to_string)
            .ok_or_else(|| "peer must be an IP address".to_string())
    };
    let port = || {
        params["port"]
            .as_u64()
            .and_then(|p| u16::try_from(p).ok())
            .filter(|p| PORTS.contains(p))
            .ok_or_else(|| format!("port must be one of {PORTS:?}"))
    };
    let path = allow_path();
    Some(match action {
        "poller.allow" => (|| {
            let (peer, port) = (peer()?, port()?);
            let until = match (params["forever"].as_bool(), params["hours"].as_i64()) {
                (Some(true), _) => None,
                (_, Some(h)) if (1..=24 * 365).contains(&h) => Some(now + h * 3600),
                (_, Some(_)) => return Err("hours must be 1..8760".into()),
                _ => Some(now + ALLOW_DEFAULT_S),
            };
            let label: String = params["label"]
                .as_str()
                .unwrap_or("")
                .chars()
                .filter(|c| !c.is_control())
                .take(40)
                .collect();
            let mut list = load_allow(&path);
            upsert(
                &mut list,
                Allow {
                    peer: peer.clone(),
                    port,
                    label,
                    until,
                },
                now,
            )?;
            save_allow(&path, &list).map_err(|e| format!("write {ALLOW_FILE}: {e}"))?;
            with(|l| l.m.mark_allowed(&peer, port, true));
            Ok(json!({"peer": peer, "port": port, "until": until}))
        })(),
        "poller.unallow" => (|| {
            let (peer, port) = (peer()?, port()?);
            let mut list = load_allow(&path);
            let was = remove(&mut list, &peer, port);
            if was {
                save_allow(&path, &list).map_err(|e| format!("write {ALLOW_FILE}: {e}"))?;
            }
            with(|l| l.m.mark_allowed(&peer, port, false));
            Ok(json!({"peer": peer, "port": port, "removed": was}))
        })(),
        "poller.ack" => match params["id"].as_str() {
            Some(id) => match with(|l| l.m.ack(id)) {
                true => Ok(json!({"id": id, "acked": true})),
                false => Err("no such event".into()),
            },
            None => Err("missing parameter: id".into()),
        },
        _ => return None,
    })
}
