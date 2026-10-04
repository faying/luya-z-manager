//! Deep diagnosis: "why is it slow?" on request (`docs/designs/slow-diagnosis.md`
//! §4.2, §12.3, §12.5; eng review 10-02 D4/D8/D10/D11/D12).
//!
//! `POST /api/diagnose` starts one run (about 10 s, ≤ 5 s per layer, ≤ 20 s in
//! all); `GET /api/diagnose` reads it. One run at a time: a second POST while
//! one is waiting or running gets that same run. A finished run is kept 10
//! minutes.
//!
//! Layers, in the order the main cause is picked from: Wi-Fi, signal, speed
//! cap, cellular link, cell load, proxy. Each ends ok / warn / bad / na
//! ("can't tell", never the main cause). Every judgement is a pure function
//! below with its own tests; the runner only gathers inputs.
//!
//! Sharing the modem and the uplink (D4/D10): one [`gate`] lock. A run that
//! finds a search, a manual register or a speed test going waits for it (up
//! to 120 s) and starts by itself; while a run is going, speed tests and
//! search/register are refused at once with "diagnosing, try again in about
//! N s" ([`refusal`]). Both sides check and claim under the gate, so neither
//! can slip in between. The run does not take netinfo's `claim`: it does not
//! drive the modem.
//!
//! What it sends: 10 DNS queries to the operator's resolver (the design said
//! `ping -I rmnet_data0`; busybox ping can't space 10 pings inside 5 s, and a
//! DNS query is the same one packet out and back on the same route), and,
//! with the proxy on, one health check of the node in use plus one direct
//! request to the same generate_204 URL (D11). Nothing while idle.
//!
//! Cell-load history (D8/D12): evidence ② compares with earlier runs on the
//! same cell made while nobody was using the network (their link latency is
//! in each `diagnose` event); evidence ③ uses only "add speed test" results
//! (`speed` events) for the same carrier + full cell ID + channel + hour.
//! Without a full cell ID neither counts.

use std::net::{SocketAddr, UdpSocket};
use std::sync::mpsc;
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{json, Value};

const DATAD: &str = "http://127.0.0.1:9460";
/// A run that waits longer than this for another operation gives up.
const WAIT_MAX: u64 = 120;
const LAYER_MAX: Duration = Duration::from_secs(5);
const RUN_MAX: Duration = Duration::from_secs(20);
/// How long a finished run is shown again instead of starting over.
pub const KEEP_SECS: u64 = 600;
/// Expected length of a run, for "try again in about N s".
const RUN_TYPICAL: u64 = 20;
const LINK_PROBES: usize = 10;
/// datad's "in use" line (screen.rs `busy`): 1 Mbit/s received.
const IN_USE_BPS: i64 = 125_000;
const BASELINE_MIN: usize = 3;
const SPEED_MIN: usize = 3;
/// Initial thresholds (§10: tuned later from the right/wrong feedback).
const WIFI_SIGNAL_BAD: i32 = -70;
const WIFI_RATE_BAD: u32 = 50;
/// The rate only counts with a signal at or below this: an idle station near
/// the device reports a low tx rate too (first device run, 10-02: -41 dBm at
/// 29 Mbps was called poor).
const WIFI_RATE_NEEDS_SIGNAL: i32 = -65;
const WIFI_RETRY_BAD: f64 = 0.10;
const LINK_LOSS_BAD: f64 = 0.10;
const LINK_MS_BAD: u32 = 150;
const PROXY_SLOWER_BAD: u32 = 300;
const SPEED_LOW_MBPS: f64 = 10.0;

// ---- the gate shared with speedtest and netinfo search/register -------------

#[derive(Default)]
pub struct Gate {
    /// Device-clock seconds when the running diagnosis started.
    diag_since: Option<u64>,
}

static GATE: Mutex<Gate> = Mutex::new(Gate { diag_since: None });

/// Take it before checking and claiming; hold it until the claim is made.
pub fn gate() -> MutexGuard<'static, Gate> {
    GATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// About how many seconds until the running diagnosis is done.
fn eta(since: u64, now: u64) -> u64 {
    (since + RUN_TYPICAL).saturating_sub(now).max(1)
}

/// The 409 a speed test or a search/register gets while a diagnosis runs.
pub fn refusal(g: &Gate) -> Option<(u16, Value)> {
    let n = eta(g.diag_since?, now_unix());
    Some((
        409,
        json!({
            "ok": false,
            "error": format!("正在诊断，约 {n} 秒后再试"),
            "error_en": format!("Diagnosing; try again in about {n} s"),
            "busy": "diagnose",
            "retry_after_s": n,
        }),
    ))
}

/// What a waiting run is waiting for: "scan" / "register" / "speedtest".
fn others_busy() -> Option<&'static str> {
    crate::netinfo::modem_busy().or_else(|| crate::speedtest::running().then_some("speedtest"))
}

// ---- result shapes -------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Pending,
    Running,
    Ok,
    Warn,
    Bad,
    Na,
    /// A value with no judgement (the speed row, D9).
    Info,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Layer {
    /// wifi / signal / limit / link / crowd / proxy
    pub id: &'static str,
    pub level: Level,
    /// The value side of the row (numbers, or why it can't tell for na).
    pub detail: String,
    pub detail_en: String,
    /// false = shown, but not counted in "3/6" (nothing to check here).
    pub counted: bool,
}

impl Layer {
    fn new(id: &'static str) -> Layer {
        Layer { id, level: Level::Pending, detail: String::new(), detail_en: String::new(), counted: true }
    }
    fn set(mut self, level: Level, zh: impl Into<String>, en: impl Into<String>) -> Layer {
        self.level = level;
        self.detail = zh.into();
        self.detail_en = en.into();
        self
    }
    fn na(self, zh: &str, en: &str) -> Layer {
        self.set(Level::Na, zh, en)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Main {
    /// The layer the headline is about; "" when nothing was found.
    pub layer: &'static str,
    pub level: Option<Level>,
    pub text: String,
    pub text_en: String,
    /// The one thing to do.
    pub action: String,
    pub action_en: String,
    /// Where the action leads on the screen: "placement" / "proxy" / "".
    pub action_to: &'static str,
    /// Other layers at warn or bad ("+N more").
    pub more: usize,
}

/// The history key (D12). None parts = not in the history.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CellKey {
    pub plmn: Option<String>,
    pub cell: Option<i64>,
    pub ch: Option<i64>,
    pub hour: u8,
}

impl CellKey {
    fn usable(&self) -> bool {
        self.plmn.is_some() && self.cell.is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    /// Another operation (search, register, speed test) is going.
    Waiting,
    Running,
    Done,
}

#[derive(Clone, Debug, Serialize)]
pub struct Run {
    pub id: u64,
    pub state: RunState,
    /// "scan" / "register" / "speedtest" while waiting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting_for: Option<&'static str>,
    pub asked_at: u64,
    pub started_at: Option<u64>,
    pub finished_at: Option<u64>,
    /// Layers finished / layers counted.
    pub step: usize,
    pub steps: usize,
    pub layers: Vec<Layer>,
    pub main: Option<Main>,
    pub key: CellKey,
    /// "touch" / "client" / "other" (who asked; picks the Wi-Fi rule).
    pub from: &'static str,
    pub feedback: Option<bool>,
    /// "Add speed test" row: running, then "直连 ↓ 86 Mbps" (info) or na.
    pub speed: Option<Layer>,
}

impl Run {
    fn new(id: u64, now: u64, from: &'static str, proxy: bool) -> Run {
        let mut layers: Vec<Layer> = ["wifi", "signal", "limit", "link", "crowd"].into_iter().map(Layer::new).collect();
        if proxy {
            layers.push(Layer::new("proxy"));
        }
        Run {
            id,
            state: RunState::Waiting,
            waiting_for: None,
            asked_at: now,
            started_at: None,
            finished_at: None,
            step: 0,
            steps: layers.len(),
            layers,
            main: None,
            key: CellKey::default(),
            from,
            feedback: None,
            speed: None,
        }
    }

    fn put(&mut self, l: Layer) {
        if let Some(slot) = self.layers.iter_mut().find(|x| x.id == l.id) {
            *slot = l;
        }
        self.steps = self.layers.iter().filter(|x| x.counted).count();
        self.step = self.layers.iter().filter(|x| x.counted && !matches!(x.level, Level::Pending | Level::Running)).count();
    }

    fn running(&mut self, id: &str) {
        if let Some(slot) = self.layers.iter_mut().find(|x| x.id == id) {
            slot.level = Level::Running;
        }
    }
}

static RUN: Mutex<Option<Run>> = Mutex::new(None);

fn run_slot() -> MutexGuard<'static, Option<Run>> {
    RUN.lock().unwrap_or_else(|e| e.into_inner())
}

// ---- judgements (pure) ------------------------------------------------------------

/// Who asked, for the Wi-Fi layer.
#[derive(Clone, Debug, PartialEq)]
pub enum Who {
    /// The front screen: look at the worst station.
    Touch,
    /// A LAN client with this lease MAC (None = no lease for its address).
    Client(Option<String>),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sta {
    pub mac: String,
    pub name: String,
    pub signal: Option<i32>,
    pub rate_mbps: Option<u32>,
    pub tx_packets: Option<u64>,
    pub tx_retries: Option<u64>,
}

impl Sta {
    fn retry(&self) -> Option<f64> {
        match (self.tx_packets, self.tx_retries) {
            (Some(p), Some(r)) if p >= 50 => Some(r as f64 / p as f64),
            _ => None,
        }
    }
    fn bad(&self) -> bool {
        self.signal.is_some_and(|s| s < WIFI_SIGNAL_BAD)
            || (self.rate_mbps.is_some_and(|r| r < WIFI_RATE_BAD) && self.signal.is_some_and(|s| s <= WIFI_RATE_NEEDS_SIGNAL))
            || self.retry().is_some_and(|r| r > WIFI_RETRY_BAD)
    }
    fn numbers(&self) -> String {
        let mut p = Vec::new();
        if let Some(s) = self.signal {
            p.push(format!("{s} dBm"));
        }
        if let Some(r) = self.rate_mbps {
            p.push(format!("{r} Mbps"));
        }
        if let Some(r) = self.retry() {
            p.push(format!("{:.0}%", r * 100.0));
        }
        p.join(" · ")
    }
    /// For "worst": lowest signal, then lowest rate.
    fn rank(&self) -> (i32, u32) {
        (self.signal.unwrap_or(0), self.rate_mbps.unwrap_or(u32::MAX))
    }
}

pub fn judge_wifi(who: &Who, stas: &[Sta]) -> Layer {
    let l = Layer::new("wifi");
    let (s, prefix, prefix_en) = match who {
        Who::Client(None) => return l.na("不是经 Wi-Fi 连的", "Not on Wi-Fi"),
        Who::Client(Some(mac)) => match stas.iter().find(|s| s.mac.eq_ignore_ascii_case(mac)) {
            Some(s) => (s, String::new(), String::new()),
            None => return l.na("不是经 Wi-Fi 连的", "Not on Wi-Fi"),
        },
        Who::Touch => {
            let Some(s) = stas.iter().min_by_key(|s| s.rank()) else {
                let mut l = l.na("没有设备连着", "No devices on Wi-Fi");
                l.counted = false;
                return l;
            };
            let name = if s.name.is_empty() { s.mac.clone() } else { s.name.clone() };
            if stas.len() > 1 {
                (s, format!("最差：{name} · "), format!("Worst: {name} · "))
            } else {
                (s, format!("{name} · "), format!("{name} · "))
            }
        }
    };
    let n = s.numbers();
    if n.is_empty() {
        return l.na("读不到", "Unreadable");
    }
    let level = if s.bad() { Level::Bad } else { Level::Ok };
    l.set(level, format!("{prefix}{n}"), format!("{prefix_en}{n}"))
}

fn tone_level(t: Option<&str>) -> Option<Level> {
    match t? {
        "ok" => Some(Level::Ok),
        "warn" => Some(Level::Warn),
        "bad" => Some(Level::Bad),
        _ => None,
    }
}

fn worst(a: Level, b: Level) -> Level {
    let rank = |l: Level| match l {
        Level::Bad => 3,
        Level::Warn => 2,
        Level::Ok => 1,
        _ => 0,
    };
    if rank(b) > rank(a) {
        b
    } else {
        a
    }
}

/// What the runner reads for the signal and cap layers: datad's `/v2/screen`
/// `net.story` and a few raw values from `/state`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Radio {
    pub story: Option<Value>,
    pub rsrp: Option<f64>,
    pub sinr: Option<f64>,
    pub rsrq: Option<f64>,
    /// Signal-bars tier, 0 weak / 1 / 2 strong (screen.rs `bars_tier`).
    pub tier: Option<i64>,
    pub nr: bool,
    pub ambr_dl: Option<f64>,
    pub qci: Option<i64>,
    pub connected: bool,
    pub rx_bps: i64,
}

fn s<'a>(v: &'a Option<Value>, k: &str) -> &'a str {
    v.as_ref().and_then(|v| v.get(k)).and_then(Value::as_str).unwrap_or("")
}

pub fn judge_signal(r: &Radio) -> Layer {
    let l = Layer::new("signal");
    if r.story.is_none() {
        return l.na("数据服务没回应", "Data service not answering");
    }
    let st = &r.story;
    if matches!(s(st, "state"), "nosvc" | "sos" | "nosim" | "airplane") {
        let h = s(st, "headline");
        let h_en = s(st, "headline_en");
        return l.set(Level::Bad, h, if h_en.is_empty() { h } else { h_en });
    }
    let sig = tone_level(Some(s(st, "sig_tone"))).unwrap_or(Level::Na);
    let noise = tone_level(Some(s(st, "noise_tone"))).unwrap_or(Level::Na);
    let mut level = worst(sig, noise);
    if s(st, "cause") == "narrow" {
        level = worst(level, Level::Warn);
    }
    if level == Level::Na {
        return l.na("读不到信号", "No signal reading");
    }
    let en = |k: &str| {
        let e = s(st, &format!("{k}_en"));
        if e.is_empty() { s(st, k).to_string() } else { e.to_string() }
    };
    let mut zh = Vec::new();
    let mut en_parts = Vec::new();
    if !s(st, "sig").is_empty() {
        zh.push(format!("信号{}", s(st, "sig")));
        en_parts.push(format!("Signal {}", en("sig").to_lowercase()));
    }
    if !s(st, "noise").is_empty() {
        zh.push(format!("干扰{}", s(st, "noise")));
        en_parts.push(format!("noise {}", en("noise")));
    }
    let mut nums = Vec::new();
    if let Some(x) = r.rsrp {
        nums.push(format!("RSRP {x:.0}"));
    }
    if let Some(x) = r.sinr {
        nums.push(format!("SINR {x:.1}"));
    }
    if s(st, "cause") == "narrow" {
        zh.push("载波窄".into());
        en_parts.push("narrow carrier".into());
    }
    zh.extend(nums.iter().cloned());
    en_parts.extend(nums);
    l.set(level, zh.join(" · "), en_parts.join(" · "))
}

pub fn judge_limit(r: &Radio) -> Layer {
    let l = Layer::new("limit");
    let qci = r.qci.map(|q| format!(" · QCI {q}")).unwrap_or_default();
    match r.ambr_dl {
        None => l.na("QoS 读不到", "QoS unavailable"),
        Some(a) if a > 0.0 && a < 10.0 => {
            let m = (a + 0.5) as i64;
            l.set(Level::Bad, format!("限到 {m} Mbps{qci}"), format!("Capped at {m} Mbps{qci}"))
        }
        Some(_) => l.set(Level::Ok, format!("没有限速{qci}"), format!("No cap{qci}")),
    }
}

/// Ten DNS queries to the operator's resolver: round trips of the answered ones.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LinkProbe {
    pub sent: usize,
    pub rtts: Vec<u32>,
}

impl LinkProbe {
    pub fn median(&self) -> Option<u32> {
        median_u32(&self.rtts)
    }
    pub fn loss(&self) -> f64 {
        if self.sent == 0 {
            return 0.0;
        }
        1.0 - self.rtts.len() as f64 / self.sent as f64
    }
}

fn median_u32(v: &[u32]) -> Option<u32> {
    let mut v = v.to_vec();
    v.sort_unstable();
    match v.len() {
        0 => None,
        n if n % 2 == 1 => Some(v[n / 2]),
        n => Some((v[n / 2 - 1] + v[n / 2]) / 2),
    }
}

fn median_f(v: &[f64]) -> Option<f64> {
    let mut v = v.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    match v.len() {
        0 => None,
        n if n % 2 == 1 => Some(v[n / 2]),
        n => Some((v[n / 2 - 1] + v[n / 2]) / 2.0),
    }
}

pub fn judge_link(connected: bool, dns_known: bool, p: Option<&LinkProbe>) -> Layer {
    let l = Layer::new("link");
    if !connected {
        return l.set(Level::Bad, "没有数据连接", "No data connection");
    }
    if !dns_known {
        return l.na("没有运营商 DNS", "No carrier DNS");
    }
    let Some(p) = p else { return l.na("超时", "Timed out") };
    if p.sent == 0 {
        return l.na("发不出去", "Couldn't send");
    }
    let lost = p.sent - p.rtts.len();
    let Some(m) = p.median() else {
        return l.set(Level::Bad, format!("{} 个探测全丢", p.sent), format!("All {} probes lost", p.sent));
    };
    let level = if p.loss() > LINK_LOSS_BAD || m > LINK_MS_BAD { Level::Bad } else { Level::Ok };
    l.set(level, format!("延迟 {m} ms · 丢包 {lost}/{}", p.sent), format!("{m} ms · lost {lost}/{}", p.sent))
}

/// One earlier run's link latency on a cell, from a `diagnose` event.
#[derive(Clone, Debug, PartialEq)]
pub struct PastRun {
    pub key: CellKey,
    pub link_ms: u32,
    pub in_use: bool,
}

/// One "add speed test" result, from a `speed` event.
#[derive(Clone, Debug, PartialEq)]
pub struct PastSpeed {
    pub key: CellKey,
    pub mbps: f64,
}

fn same_cell(a: &CellKey, b: &CellKey) -> bool {
    a.usable() && a.plmn == b.plmn && a.cell == b.cell && a.ch == b.ch
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Ev {
    Hit,
    No,
    Na,
}

pub fn judge_crowd(r: &Radio, key: &CellKey, link: Option<&LinkProbe>, past: &[PastRun], speeds: &[PastSpeed]) -> Layer {
    let l = Layer::new("crowd");
    let mut zh = Vec::new();
    let mut en = Vec::new();
    // ① good signal, poor RSRQ (datad's crowd rule without the "in use" part)
    let e1 = match (r.tier, r.rsrq) {
        (Some(t), Some(q)) if t >= 1 => {
            if q < if r.nr { -15.0 } else { -12.0 } {
                zh.push(format!("RSRQ {q:.0}"));
                en.push(format!("RSRQ {q:.0}"));
                Ev::Hit
            } else {
                Ev::No
            }
        }
        (Some(_), Some(_)) => Ev::No,
        _ => Ev::Na,
    };
    // ② latency while in use vs this cell's idle runs
    let e2 = if !key.usable() || r.rx_bps < IN_USE_BPS {
        Ev::Na
    } else {
        let base: Vec<u32> = past.iter().filter(|p| !p.in_use && same_cell(key, &p.key)).map(|p| p.link_ms).collect();
        match (link.and_then(LinkProbe::median), median_u32(&base)) {
            (Some(now), Some(b)) if base.len() >= BASELINE_MIN && b > 0 => {
                if now >= 2 * b {
                    let x = now as f64 / b as f64;
                    zh.push(format!("延迟是平时的 {x:.1} 倍"));
                    en.push(format!("latency {x:.1}× usual"));
                    Ev::Hit
                } else {
                    Ev::No
                }
            }
            _ => Ev::Na,
        }
    };
    // ③ speed tests on this cell at this hour, 7 days
    let e3 = if !key.usable() {
        Ev::Na
    } else {
        let v: Vec<f64> = speeds.iter().filter(|p| same_cell(key, &p.key) && p.key.hour == key.hour).map(|p| p.mbps).collect();
        match median_f(&v) {
            Some(m) if v.len() >= SPEED_MIN => {
                if m < SPEED_LOW_MBPS {
                    zh.push(format!("这个时段测速中位 {m:.0} Mbps"));
                    en.push(format!("speed tests at this hour: median {m:.0} Mbps"));
                    Ev::Hit
                } else {
                    Ev::No
                }
            }
            _ => Ev::Na,
        }
    };
    let hits = [e1, e2, e3].iter().filter(|e| **e == Ev::Hit).count();
    if e2 == Ev::Hit && e3 == Ev::Hit {
        return l.set(
            Level::Bad,
            format!("这个小区这个时段经常拥挤 · {}", zh.join(" · ")),
            format!("Often busy at this hour · {}", en.join(" · ")),
        );
    }
    if hits >= 2 {
        return l.set(Level::Warn, format!("疑似拥挤 · {}", zh.join(" · ")), format!("Likely busy · {}", en.join(" · ")));
    }
    if [e1, e2, e3].iter().all(|e| *e == Ev::Na) {
        return if key.usable() {
            l.na("历史不够", "Not enough history")
        } else {
            l.na("没有小区编号", "No cell ID")
        };
    }
    l.set(Level::Ok, "没看到拥挤迹象", "No sign of a busy cell")
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NodeProbe {
    Ms(u32),
    Timeout,
}

/// The proxy layer's inputs: the node in use, its health check, and one direct
/// request to the same URL from the device.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProxyProbe {
    pub node: String,
    /// None = the proxy didn't answer.
    pub node_probe: Option<NodeProbe>,
    /// None = the direct request failed.
    pub direct_ms: Option<u32>,
}

pub fn judge_proxy(p: Option<&ProxyProbe>) -> Layer {
    let l = Layer::new("proxy");
    let Some(p) = p else { return l.na("超时", "Timed out") };
    let Some(np) = p.node_probe else { return l.na("代理没回应", "Proxy not answering") };
    let Some(d) = p.direct_ms else { return l.na("直连也不通", "Direct also failing") };
    let n = &p.node;
    match np {
        NodeProbe::Timeout => l.set(Level::Bad, format!("{n} 超时 · 直连 {d} ms"), format!("{n} timed out · direct {d} ms")),
        NodeProbe::Ms(ms) if ms > d + PROXY_SLOWER_BAD => l.set(
            Level::Bad,
            format!("{n} {ms} ms · 比直连慢 {} ms", ms - d),
            format!("{n} {ms} ms · {} ms slower than direct", ms - d),
        ),
        NodeProbe::Ms(ms) => l.set(Level::Ok, format!("{n} {ms} ms · 直连 {d} ms"), format!("{n} {ms} ms · direct {d} ms")),
    }
}

/// §12.3: first bad in layer order, else first warn; na never.
pub fn pick_main(layers: &[Layer]) -> Main {
    let pick = layers.iter().find(|l| l.level == Level::Bad).or_else(|| layers.iter().find(|l| l.level == Level::Warn));
    let Some(m) = pick else {
        return Main {
            text: "没查到问题".into(),
            text_en: "No problem found".into(),
            action: "可能是对方网站慢；也可以加测速度".into(),
            action_en: "The site itself may be slow; you can also add a speed test".into(),
            ..Main::default()
        };
    };
    let more = layers.iter().filter(|l| l.id != m.id && matches!(l.level, Level::Warn | Level::Bad)).count();
    let (text, text_en, action, action_en, to) = match m.id {
        "wifi" => ("Wi-Fi 信号差", "Poor Wi-Fi", "靠近一点，或换个频段", "Move closer, or switch Wi-Fi band", ""),
        // no service / SOS / no SIM: the headline itself
        "signal" if !m.detail.contains("信号") => (m.detail.as_str(), m.detail_en.as_str(), "", "", ""),
        "signal" if m.detail.contains("信号弱") => (
            "信号弱",
            "Weak signal",
            "固定位置时用摆放模式；在路上只能等",
            "Use Placement if you're staying put; on the move, wait",
            "placement",
        ),
        "signal" if m.detail.contains("干扰大") => (
            "干扰大",
            "Noisy signal",
            "固定位置时用摆放模式；在路上只能等",
            "Use Placement if you're staying put; on the move, wait",
            "placement",
        ),
        "signal" if m.detail.contains("载波窄") => ("这里只给了窄载波", "Narrow carrier here", "换个地方试试", "Try another spot", ""),
        "signal" => (
            "信号一般",
            "Fair signal",
            "固定位置时用摆放模式；在路上只能等",
            "Use Placement if you're staying put; on the move, wait",
            "placement",
        ),
        "limit" => ("运营商限速", "Carrier speed cap", "找运营商", "Ask your carrier", ""),
        "link" => ("蜂窝链路不稳", "Cellular link unstable", "过几分钟再试，或换个地方", "Try again in a few minutes, or move", ""),
        "crowd" => ("疑似基站拥挤", "Cell likely busy", "过几分钟再试，或换个地方", "Try again in a few minutes, or move", ""),
        "proxy" => ("代理节点慢或不通", "Proxy node slow or down", "换节点", "Switch node", "proxy"),
        _ => ("", "", "", "", ""),
    };
    Main {
        layer: m.id,
        level: Some(m.level),
        text: text.into(),
        text_en: text_en.into(),
        action: action.into(),
        action_en: action_en.into(),
        action_to: to,
        more,
    }
}

/// What the run gave up on after waiting 120 s.
fn gave_up(run: &mut Run, on: &str) {
    let (zh, en) = match on {
        "scan" => ("正在搜网", "Searching for networks"),
        "register" => ("正在选网", "Registering on a network"),
        _ => ("正在测速", "Speed test running"),
    };
    for l in run.layers.clone() {
        run.put(l.na(zh, en));
    }
    run.main = Some(Main { text: format!("测不了：{zh}"), text_en: format!("Can't check: {en}"), ..Main::default() });
}

// ---- history (netwatch events) ------------------------------------------------

fn key_of(v: &Value) -> CellKey {
    CellKey {
        plmn: v.get("plmn").and_then(Value::as_str).map(str::to_string),
        cell: v.get("cell").and_then(Value::as_i64),
        ch: v.get("ch").and_then(Value::as_i64),
        hour: v.get("hour").and_then(Value::as_u64).unwrap_or(0) as u8,
    }
}

pub fn past_from_events(events: &[crate::netwatch::record::Event]) -> (Vec<PastRun>, Vec<PastSpeed>) {
    let (mut runs, mut speeds) = (Vec::new(), Vec::new());
    for e in events {
        let d = Value::Object(e.data.clone().into_iter().collect());
        let key = key_of(&d);
        if !key.usable() {
            continue;
        }
        match e.kind.as_str() {
            "diagnose" => {
                if let (Some(ms), Some(u)) = (d.get("link_ms").and_then(Value::as_u64), d.get("in_use").and_then(Value::as_bool)) {
                    runs.push(PastRun { key, link_ms: ms as u32, in_use: u });
                }
            }
            "speed" => {
                if let Some(m) = d.get("mbps").and_then(Value::as_f64) {
                    speeds.push(PastSpeed { key, mbps: m });
                }
            }
            _ => {}
        }
    }
    (runs, speeds)
}

/// The key fields every `diagnose` / `speed` event carries.
pub fn key_json(k: &CellKey) -> Value {
    json!({"plmn": k.plmn, "cell": k.cell, "ch": k.ch, "hour": k.hour})
}

// ---- gathering (device I/O) -------------------------------------------------------

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Run `f` on its own thread; None if it takes longer than `max`.
fn within<T: Send + 'static>(max: Duration, f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(max).ok()
}

pub(crate) fn datad_get(path: &str) -> Option<Value> {
    let base = std::env::var("ZTE_AGENT_DATAD").unwrap_or_else(|_| DATAD.to_string());
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(3))).build().into();
    agent.get(&format!("{base}{path}")).call().ok()?.body_mut().read_json::<Value>().ok()
}

pub(crate) fn num(v: Option<&Value>) -> Option<f64> {
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
    let reading = crate::netwatch::record::Reading::parse(now, state, screen, None);
    let story = screen.and_then(|s| s.get("net")).and_then(|n| n.get("story")).cloned();
    let tier = screen.and_then(|s| s.get("net")).and_then(|n| n.get("bars_tier")).and_then(Value::as_i64).filter(|t| *t >= 0);
    let nr = reading.rat == "SA";
    let pick = |a: Option<f64>, b: Option<f64>| if nr { a } else { b };
    let qos = state.and_then(|s| s.get("qos"));
    let dns = state.map(crate::netwatch::operator_dns).unwrap_or_default();
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

/// Ten queries, ~0.35 s apart, on one socket; replies collected until 1.5 s
/// after the last send. About 5 s in all.
fn link_probe(dns: &[String]) -> LinkProbe {
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
            let q = crate::netwatch::dns_query(base.wrapping_add(next as u16), "www.qq.com");
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

fn stations() -> Vec<Sta> {
    let names = crate::netinfo::lease_names();
    crate::netinfo::wifi_stations()
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

fn proxy_on() -> bool { false }
fn proxy_probe() -> ProxyProbe { ProxyProbe::default() }

// ---- the run ----------------------------------------------------------------------

fn update(id: u64, f: impl FnOnce(&mut Run)) {
    if let Some(r) = run_slot().as_mut().filter(|r| r.id == id) {
        f(r);
    }
}

fn execute(id: u64, who: Who) {
    // wait for a search / register / speed test, then claim the gate
    let asked = Instant::now();
    loop {
        let mut g = gate();
        match others_busy() {
            None => {
                g.diag_since = Some(now_unix());
                drop(g);
                break;
            }
            Some(k) => {
                drop(g);
                if asked.elapsed().as_secs() >= WAIT_MAX {
                    update(id, |r| {
                        gave_up(r, k);
                        r.state = RunState::Done;
                        r.waiting_for = None;
                        r.finished_at = Some(now_unix());
                    });
                    return;
                }
                update(id, |r| r.waiting_for = Some(k));
                thread::sleep(Duration::from_secs(1));
            }
        }
    }
    let start = Instant::now();
    update(id, |r| {
        r.state = RunState::Running;
        r.waiting_for = None;
        r.started_at = Some(now_unix());
    });
    let left = || RUN_MAX.saturating_sub(start.elapsed()).min(LAYER_MAX);
    let timed_out = |l: Layer| l.na("超时", "Timed out");

    // Wi-Fi
    update(id, |r| r.running("wifi"));
    let w = within(left(), move || judge_wifi(&who, &stations())).unwrap_or_else(|| timed_out(Layer::new("wifi")));
    update(id, |r| r.put(w));

    // signal + cap, one read of datad
    update(id, |r| {
        r.running("signal");
        r.running("limit");
    });
    let read = within(left(), || {
        let st = datad_get("/state");
        let sc = datad_get("/v2/screen");
        radio_from(st.as_ref(), sc.as_ref())
    });
    let (radio, key, dns, _) = read.clone().unwrap_or_default();
    match read {
        Some(_) => update(id, |r| {
            r.put(judge_signal(&radio));
            r.put(judge_limit(&radio));
            r.key = key.clone();
        }),
        None => update(id, |r| {
            r.put(timed_out(Layer::new("signal")));
            r.put(timed_out(Layer::new("limit")));
        }),
    }

    // cellular link
    update(id, |r| r.running("link"));
    let probe = if radio.connected && !dns.is_empty() {
        let d = dns.clone();
        within(left(), move || link_probe(&d))
    } else {
        None
    };
    update(id, |r| r.put(judge_link(radio.connected, !dns.is_empty(), probe.as_ref())));

    // cell load, from history
    update(id, |r| r.running("crowd"));
    let now = now_unix();
    let (_, events) = crate::netwatch::history(now.saturating_sub(7 * 86_400), now + 1);
    let (past, speeds) = past_from_events(&events);
    update(id, |r| r.put(judge_crowd(&radio, &key, probe.as_ref(), &past, &speeds)));

    // proxy
    let has_proxy = run_slot().as_ref().is_some_and(|r| r.layers.iter().any(|l| l.id == "proxy"));
    if has_proxy {
        update(id, |r| r.running("proxy"));
        let p = within(left(), proxy_probe);
        update(id, |r| r.put(judge_proxy(p.as_ref())));
    }

    gate().diag_since = None;
    let mut done = None;
    update(id, |r| {
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
        crate::netwatch::push_event("diagnose", ev);
    }
}

// ---- HTTP ---------------------------------------------------------------------------

/// Who is asking, from the request's source address.
fn who_from(remote: Option<std::net::IpAddr>) -> (&'static str, Who) {
    match remote {
        Some(ip) if ip.is_loopback() => ("touch", Who::Touch),
        Some(ip) => {
            let ip = ip.to_string();
            let mac = crate::netinfo::lease_mac(&ip);
            ("client", Who::Client(mac))
        }
        None => ("other", Who::Client(None)),
    }
}

fn view(r: &Run, now: u64) -> Value {
    let mut v = json!(r);
    v["age_s"] = json!(r.finished_at.map(|f| now.saturating_sub(f)));
    v
}

/// `POST /api/diagnose` — start a run, or join the one going.
pub fn start(remote: Option<std::net::IpAddr>) -> (u16, Value) {
    let now = now_unix();
    let mut slot = run_slot();
    if let Some(r) = slot.as_ref() {
        if matches!(r.state, RunState::Waiting | RunState::Running) {
            return (202, json!({"ok": true, "data": view(r, now), "joined": true}));
        }
    }
    let (from, who) = who_from(remote);
    let id = slot.as_ref().map_or(1, |r| r.id + 1);
    let run = Run::new(id, now, from, proxy_on());
    let out = view(&run, now);
    *slot = Some(run);
    drop(slot);
    thread::Builder::new()
        .name("deep-diag".into())
        .spawn(move || {
            let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| execute(id, who))).is_ok();
            if !ok {
                gate().diag_since = None;
                update(id, |r| {
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

/// `GET /api/diagnose` — the current or last run (kept [`KEEP_SECS`]).
pub fn get() -> (u16, Value) {
    let now = now_unix();
    let slot = run_slot();
    match slot.as_ref() {
        Some(r) if r.state != RunState::Done || r.finished_at.is_some_and(|f| now.saturating_sub(f) <= KEEP_SECS) => {
            (200, json!({"ok": true, "data": view(r, now)}))
        }
        _ => (200, json!({"ok": true, "data": {"state": "idle"}})),
    }
}

/// The speed row from a finished capped test. The device's own requests go
/// out directly (the proxy only carries LAN clients), so the route is "direct".
pub fn speed_row(p: &crate::speedtest::SpeedTestProgress) -> Layer {
    let l = Layer::new("speed");
    match (p.download_mbps(), p.failure()) {
        (Some(m), _) => l.set(Level::Info, format!("直连 ↓ {m:.0} Mbps"), format!("Direct ↓ {m:.0} Mbps")),
        (None, Some(f)) if f == "cancelled" => l.na("被停下", "Stopped"),
        _ => l.na("没测成", "Didn't finish"),
    }
}

/// `POST /api/diagnose/speed` — body {"id": n}: "add speed test" for a
/// finished run (capped: download only, ~5 s, ≤ 30 MB). The two-step confirm
/// when roaming is the screens' job.
pub fn speed(state: &crate::handlers::AppState, body: &[u8]) -> (u16, Value) {
    let Some(id) = serde_json::from_slice::<Value>(body).ok().and_then(|b| b.get("id").and_then(Value::as_u64)) else {
        return (400, json!({"ok": false, "error": "id is required"}));
    };
    let (key, before) = {
        let mut slot = run_slot();
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
    let done = Box::new(move |p: &crate::speedtest::SpeedTestProgress| {
        let row = speed_row(p);
        update(id, |r| r.speed = Some(row));
        if let Some(m) = p.download_mbps() {
            let mut ev = key_json(&key);
            ev["id"] = json!(id);
            ev["mbps"] = json!(m);
            ev["bytes"] = json!(p.download_bytes());
            ev["route"] = json!("direct");
            crate::netwatch::push_event("speed", ev);
        }
    });
    let (code, v) = crate::speedtest::start_capped(state, done);
    if code != 200 {
        update(id, |r| r.speed = before);
        return (code, v);
    }
    let slot = run_slot();
    (202, json!({"ok": true, "data": slot.as_ref().map(|r| view(r, now_unix()))}))
}

/// `POST /api/diagnose/feedback` — body {"id": n, "right": true|false}; once per run.
pub fn feedback(body: &[u8]) -> (u16, Value) {
    let Ok(b) = serde_json::from_slice::<Value>(body) else {
        return (400, json!({"ok": false, "error": "invalid JSON"}));
    };
    let (Some(id), Some(right)) = (b.get("id").and_then(Value::as_u64), b.get("right").and_then(Value::as_bool)) else {
        return (400, json!({"ok": false, "error": "id and right are required"}));
    };
    let mut slot = run_slot();
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
    crate::netwatch::push_event("diagnose_feedback", ev);
    (200, json!({"ok": true, "data": {"recorded": true}}))
}

#[cfg(test)]
mod tests;
