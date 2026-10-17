//! Deep diagnosis' pure parts (moved from zte-agent `deep_diag.rs`, Phase 4 step 8e): the gate
//! shared by diagnosis, speed tests and network search/register (D10), the result shapes, every
//! layer's judgement and the main cause, and the cell-load history from past events. The runner
//! (gathering the inputs) stays with the agent until netwatch, whose history it reads, moves too
//! (step 9); u60d's speed test and search/register take this same gate meanwhile.

use serde::Serialize;
use serde_json::{Value, json};
use std::time::Duration;

/// A run that waits longer than this for another operation gives up.
pub const WAIT_MAX: u64 = 120;
pub const LAYER_MAX: Duration = Duration::from_secs(5);
pub const RUN_MAX: Duration = Duration::from_secs(20);
/// How long a finished run is shown again instead of starting over.
pub const KEEP_SECS: u64 = 600;
/// Expected length of a run, for "try again in about N s".
pub const RUN_TYPICAL: u64 = 20;
pub const LINK_PROBES: usize = 10;
/// datad's "in use" line (screen.rs `busy`): 1 Mbit/s received.
pub const IN_USE_BPS: i64 = 125_000;
pub const BASELINE_MIN: usize = 3;
pub const SPEED_MIN: usize = 3;
/// Initial thresholds (§10: tuned later from the right/wrong feedback).
pub const WIFI_SIGNAL_BAD: i32 = -70;
pub const WIFI_RATE_BAD: u32 = 50;
/// The rate only counts with a signal at or below this: an idle station near
/// the device reports a low tx rate too (first device run, 10-02: -41 dBm at
/// 29 Mbps was called poor).
pub const WIFI_RATE_NEEDS_SIGNAL: i32 = -65;
pub const WIFI_RETRY_BAD: f64 = 0.10;
pub const LINK_LOSS_BAD: f64 = 0.10;
pub const LINK_MS_BAD: u32 = 150;
pub const PROXY_SLOWER_BAD: u32 = 300;
pub const SPEED_LOW_MBPS: f64 = 10.0;

// ---- the gate shared with speedtest and netinfo search/register -------------

#[derive(Default)]
pub struct Gate {
    /// Device-clock seconds when the running diagnosis started.
    pub diag_since: Option<u64>,
}

/// About how many seconds until the running diagnosis is done.
pub fn eta(since: u64, now: u64) -> u64 {
    (since + RUN_TYPICAL).saturating_sub(now).max(1)
}

/// The 409 a speed test or a search/register gets while a diagnosis runs.
pub fn refusal_at(g: &Gate, now: u64) -> Option<(u16, Value)> {
    let n = eta(g.diag_since?, now);
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
    pub fn new(id: &'static str) -> Layer {
        Layer {
            id,
            level: Level::Pending,
            detail: String::new(),
            detail_en: String::new(),
            counted: true,
        }
    }
    pub fn set(mut self, level: Level, zh: impl Into<String>, en: impl Into<String>) -> Layer {
        self.level = level;
        self.detail = zh.into();
        self.detail_en = en.into();
        self
    }
    pub fn na(self, zh: &str, en: &str) -> Layer {
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
    pub fn usable(&self) -> bool {
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
    pub fn new(id: u64, now: u64, from: &'static str, proxy: bool) -> Run {
        let mut layers: Vec<Layer> = ["wifi", "signal", "limit", "link", "crowd"]
            .into_iter()
            .map(Layer::new)
            .collect();
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

    pub fn put(&mut self, l: Layer) {
        if let Some(slot) = self.layers.iter_mut().find(|x| x.id == l.id) {
            *slot = l;
        }
        self.steps = self.layers.iter().filter(|x| x.counted).count();
        self.step = self
            .layers
            .iter()
            .filter(|x| x.counted && !matches!(x.level, Level::Pending | Level::Running))
            .count();
    }

    pub fn running(&mut self, id: &str) {
        if let Some(slot) = self.layers.iter_mut().find(|x| x.id == id) {
            slot.level = Level::Running;
        }
    }
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
    pub fn retry(&self) -> Option<f64> {
        match (self.tx_packets, self.tx_retries) {
            (Some(p), Some(r)) if p >= 50 => Some(r as f64 / p as f64),
            _ => None,
        }
    }
    pub fn bad(&self) -> bool {
        self.signal.is_some_and(|s| s < WIFI_SIGNAL_BAD)
            || (self.rate_mbps.is_some_and(|r| r < WIFI_RATE_BAD)
                && self.signal.is_some_and(|s| s <= WIFI_RATE_NEEDS_SIGNAL))
            || self.retry().is_some_and(|r| r > WIFI_RETRY_BAD)
    }
    pub fn numbers(&self) -> String {
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
    pub fn rank(&self) -> (i32, u32) {
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
            let name = if s.name.is_empty() {
                s.mac.clone()
            } else {
                s.name.clone()
            };
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

pub fn tone_level(t: Option<&str>) -> Option<Level> {
    match t? {
        "ok" => Some(Level::Ok),
        "warn" => Some(Level::Warn),
        "bad" => Some(Level::Bad),
        _ => None,
    }
}

pub fn worst(a: Level, b: Level) -> Level {
    let rank = |l: Level| match l {
        Level::Bad => 3,
        Level::Warn => 2,
        Level::Ok => 1,
        _ => 0,
    };
    if rank(b) > rank(a) { b } else { a }
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
    /// The `qos` block's `stale`: its values are history, no cap verdict from them.
    pub qos_stale: bool,
    /// The `qos` block's `bearer` is `nr_pdu`: `qci` is a 5QI.
    pub qos_nr: bool,
    pub connected: bool,
    pub rx_bps: i64,
}

pub fn s<'a>(v: &'a Option<Value>, k: &str) -> &'a str {
    v.as_ref()
        .and_then(|v| v.get(k))
        .and_then(Value::as_str)
        .unwrap_or("")
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
        if e.is_empty() {
            s(st, k).to_string()
        } else {
            e.to_string()
        }
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
    if r.qos_stale {
        return l.na("QoS 是旧连接的，不下结论", "QoS is from an earlier bearer: no verdict");
    }
    let label = if r.qos_nr { "5QI" } else { "QCI" };
    let qci = r.qci.map(|q| format!(" · {label} {q}")).unwrap_or_default();
    match r.ambr_dl {
        None => l.na("QoS 读不到", "QoS unavailable"),
        Some(a) if a > 0.0 && a < 10.0 => {
            let m = (a + 0.5) as i64;
            l.set(
                Level::Bad,
                format!("限到 {m} Mbps{qci}"),
                format!("Capped at {m} Mbps{qci}"),
            )
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

pub fn median_u32(v: &[u32]) -> Option<u32> {
    let mut v = v.to_vec();
    v.sort_unstable();
    match v.len() {
        0 => None,
        n if n % 2 == 1 => Some(v[n / 2]),
        n => Some((v[n / 2 - 1] + v[n / 2]) / 2),
    }
}

pub fn median_f(v: &[f64]) -> Option<f64> {
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
    let Some(p) = p else {
        return l.na("超时", "Timed out");
    };
    if p.sent == 0 {
        return l.na("发不出去", "Couldn't send");
    }
    let lost = p.sent - p.rtts.len();
    let Some(m) = p.median() else {
        return l.set(
            Level::Bad,
            format!("{} 个探测全丢", p.sent),
            format!("All {} probes lost", p.sent),
        );
    };
    let level = if p.loss() > LINK_LOSS_BAD || m > LINK_MS_BAD {
        Level::Bad
    } else {
        Level::Ok
    };
    l.set(
        level,
        format!("延迟 {m} ms · 丢包 {lost}/{}", p.sent),
        format!("{m} ms · lost {lost}/{}", p.sent),
    )
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

pub fn same_cell(a: &CellKey, b: &CellKey) -> bool {
    a.usable() && a.plmn == b.plmn && a.cell == b.cell && a.ch == b.ch
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Ev {
    Hit,
    No,
    Na,
}

pub fn judge_crowd(
    r: &Radio,
    key: &CellKey,
    link: Option<&LinkProbe>,
    past: &[PastRun],
    speeds: &[PastSpeed],
) -> Layer {
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
        let base: Vec<u32> = past
            .iter()
            .filter(|p| !p.in_use && same_cell(key, &p.key))
            .map(|p| p.link_ms)
            .collect();
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
        let v: Vec<f64> = speeds
            .iter()
            .filter(|p| same_cell(key, &p.key) && p.key.hour == key.hour)
            .map(|p| p.mbps)
            .collect();
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
        return l.set(
            Level::Warn,
            format!("疑似拥挤 · {}", zh.join(" · ")),
            format!("Likely busy · {}", en.join(" · ")),
        );
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
    let Some(p) = p else {
        return l.na("超时", "Timed out");
    };
    let Some(np) = p.node_probe else {
        return l.na("代理没回应", "Proxy not answering");
    };
    let Some(d) = p.direct_ms else {
        return l.na("直连也不通", "Direct also failing");
    };
    let n = &p.node;
    match np {
        NodeProbe::Timeout => l.set(
            Level::Bad,
            format!("{n} 超时 · 直连 {d} ms"),
            format!("{n} timed out · direct {d} ms"),
        ),
        NodeProbe::Ms(ms) if ms > d + PROXY_SLOWER_BAD => l.set(
            Level::Bad,
            format!("{n} {ms} ms · 比直连慢 {} ms", ms - d),
            format!("{n} {ms} ms · {} ms slower than direct", ms - d),
        ),
        NodeProbe::Ms(ms) => l.set(
            Level::Ok,
            format!("{n} {ms} ms · 直连 {d} ms"),
            format!("{n} {ms} ms · direct {d} ms"),
        ),
    }
}

/// §12.3: first bad in layer order, else first warn; na never.
pub fn pick_main(layers: &[Layer]) -> Main {
    let pick = layers
        .iter()
        .find(|l| l.level == Level::Bad)
        .or_else(|| layers.iter().find(|l| l.level == Level::Warn));
    let Some(m) = pick else {
        return Main {
            text: "没查到问题".into(),
            text_en: "No problem found".into(),
            action: "可能是对方网站慢；也可以加测速度".into(),
            action_en: "The site itself may be slow; you can also add a speed test".into(),
            ..Main::default()
        };
    };
    let more = layers
        .iter()
        .filter(|l| l.id != m.id && matches!(l.level, Level::Warn | Level::Bad))
        .count();
    let (text, text_en, action, action_en, to) = match m.id {
        "wifi" => (
            "Wi-Fi 信号差",
            "Poor Wi-Fi",
            "靠近一点，或换个频段",
            "Move closer, or switch Wi-Fi band",
            "",
        ),
        // no service / SOS / no SIM: the headline itself
        "signal" if !m.detail.contains("信号") => {
            (m.detail.as_str(), m.detail_en.as_str(), "", "", "")
        }
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
        "signal" if m.detail.contains("载波窄") => (
            "这里只给了窄载波",
            "Narrow carrier here",
            "换个地方试试",
            "Try another spot",
            "",
        ),
        "signal" => (
            "信号一般",
            "Fair signal",
            "固定位置时用摆放模式；在路上只能等",
            "Use Placement if you're staying put; on the move, wait",
            "placement",
        ),
        "limit" => (
            "运营商限速",
            "Carrier speed cap",
            "找运营商",
            "Ask your carrier",
            "",
        ),
        "link" => (
            "蜂窝链路不稳",
            "Cellular link unstable",
            "过几分钟再试，或换个地方",
            "Try again in a few minutes, or move",
            "",
        ),
        "crowd" => (
            "疑似基站拥挤",
            "Cell likely busy",
            "过几分钟再试，或换个地方",
            "Try again in a few minutes, or move",
            "",
        ),
        "proxy" => (
            "代理节点慢或不通",
            "Proxy node slow or down",
            "换节点",
            "Switch node",
            "proxy",
        ),
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
pub fn gave_up(run: &mut Run, on: &str) {
    let (zh, en) = match on {
        "scan" => ("正在搜网", "Searching for networks"),
        "register" => ("正在选网", "Registering on a network"),
        _ => ("正在测速", "Speed test running"),
    };
    for l in run.layers.clone() {
        run.put(l.na(zh, en));
    }
    run.main = Some(Main {
        text: format!("测不了：{zh}"),
        text_en: format!("Can't check: {en}"),
        ..Main::default()
    });
}

pub fn key_of(v: &Value) -> CellKey {
    CellKey {
        plmn: v.get("plmn").and_then(Value::as_str).map(str::to_string),
        cell: v.get("cell").and_then(Value::as_i64),
        ch: v.get("ch").and_then(Value::as_i64),
        hour: v.get("hour").and_then(Value::as_u64).unwrap_or(0) as u8,
    }
}

/// The cell-load history from `diagnose` and `speed` events: (kind, data).
pub fn past_from(events: &[(String, Value)]) -> (Vec<PastRun>, Vec<PastSpeed>) {
    let (mut runs, mut speeds) = (Vec::new(), Vec::new());
    for (kind, d) in events {
        let key = key_of(d);
        if !key.usable() {
            continue;
        }
        match kind.as_str() {
            "diagnose" => {
                if let (Some(ms), Some(u)) = (
                    d.get("link_ms").and_then(Value::as_u64),
                    d.get("in_use").and_then(Value::as_bool),
                ) {
                    runs.push(PastRun {
                        key,
                        link_ms: ms as u32,
                        in_use: u,
                    });
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

#[cfg(test)]
mod tests;
