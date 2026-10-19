//! Deep diagnosis' pure parts (moved from zte-agent `deep_diag.rs`, Phase 4 step 8e): the gate
//! shared by diagnosis, speed tests and network search/register (D10), the result shapes, every
//! layer's judgement and the main cause, and the cell-load history from past events. The runner
//! (gathering the inputs) stays with the agent until netwatch, whose history it reads, moves too
//! (step 9); u60d's speed test and search/register take this same gate meanwhile.

use serde::Serialize;
use serde_json::{Value, json};
use std::time::Duration;

pub mod admit;

/// A run that waits longer than this for another operation gives up.
pub const WAIT_MAX: u64 = 120;
pub const LAYER_MAX: Duration = Duration::from_secs(5);
pub const RUN_MAX: Duration = Duration::from_secs(20);
/// How long a finished run is shown again instead of starting over.
pub const KEEP_SECS: u64 = 600;
/// Expected length of a run, for "try again in about N s".
pub const RUN_TYPICAL: u64 = 20;
pub const LINK_PROBES: usize = 10;
/// the core's "in use" line (screen.rs `busy`): 1 Mbit/s received.
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
/// The device's own resolver (dnsmasq) slower than this is a suspect (v2 §2 dns).
pub const LOCAL_DNS_SLOW_MS: u32 = 1000;
/// How long a Finding probe usually holds the gate (2 HTTP × 5 s + 2 DNS × 2 s, finding/path.rs).
pub const FINDING_TYPICAL: u64 = 14;

// ---- the gate shared with speedtest and netinfo search/register -------------

#[derive(Default)]
pub struct Gate {
    /// Device-clock seconds when the running diagnosis started.
    pub diag_since: Option<u64>,
    /// Device-clock seconds when the running Finding probe started (deep-diag-v2 §2.1): kept
    /// apart so a Finding probe is never shown as "diagnosing", nor timed like one.
    pub finding_since: Option<u64>,
    /// A write transaction's admission (decision-execution-loop.md §1.3 / §4.1 D1): while it is
    /// here no new diagnosis, Finding probe or speed test starts. See [`admit`].
    pub txn: Option<admit::TxnHold>,
}

/// About how many seconds until the running diagnosis is done.
pub fn eta(since: u64, now: u64) -> u64 {
    (since + RUN_TYPICAL).saturating_sub(now).max(1)
}

/// The 409 a speed test or a search/register gets while a diagnosis runs. The time and the
/// words come from the diagnosis alone; a Finding probe gets its own words and its own time.
pub fn refusal_at(g: &Gate, now: u64) -> Option<(u16, Value)> {
    if g.txn.is_some() {
        return Some((409, admit::busy_json()));
    }
    let Some(since) = g.diag_since else {
        let n = (g.finding_since? + FINDING_TYPICAL)
            .saturating_sub(now)
            .max(1);
        return Some((
            409,
            json!({
                "ok": false,
                "error": format!("正在检查网络，约 {n} 秒后再试"),
                "error_en": format!("Checking the connection; try again in about {n} s"),
                "busy": "finding",
                "retry_after_s": n,
            }),
        ));
    };
    let n = eta(since, now);
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
    /// Not run: a layer it depends on is bad (v2 §3). Grey, not counted, never the main cause.
    Blocked,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Layer {
    /// wifi / sim / signal / limit / bearer / link / dns / e2e / clock / crowd / proxy
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
    /// Not run because `by` is bad: grey, out of "N/M". The screens put "没测" before the detail.
    pub fn blocked(self, by: &str) -> Layer {
        let (zh, en) = layer_name(by);
        let mut l = self.set(
            Level::Blocked,
            format!("前面的{zh}不通"),
            format!("{en} is down"),
        );
        l.counted = false;
        l
    }
}

/// The layers in run order (the order the main cause is picked in); proxy only with the proxy on.
pub const LAYERS: [&str; 11] = [
    "wifi", "sim", "signal", "limit", "bearer", "link", "dns", "e2e", "clock", "crowd", "proxy",
];

/// What each layer needs (v2 §3): a bad one here means the layer is not run.
pub const DEPS: &[(&str, &[&str])] = &[
    // not signal: a weak signal is bad there but still carries data; with no
    // service the bearer itself is down and blocks the rest
    ("bearer", &["sim"]),
    ("link", &["bearer"]),
    ("dns", &["bearer"]),
    ("e2e", &["bearer"]),
    ("clock", &["e2e"]),
    ("crowd", &["link"]),
    ("proxy", &["e2e"]),
];

/// The first layer `id` depends on that is bad, or blocked itself (a chain stays blocked).
pub fn blocked_by(layers: &[Layer], id: &str) -> Option<&'static str> {
    let deps = DEPS.iter().find(|(l, _)| *l == id)?.1;
    for d in deps {
        let Some(l) = layers.iter().find(|l| l.id == *d) else {
            continue;
        };
        match l.level {
            Level::Bad => return Some(l.id),
            Level::Blocked => return blocked_by(layers, l.id).or(Some(l.id)),
            _ => {}
        }
    }
    None
}

/// A layer's name in a row and in "前面的 X 不通" (the screens have their own copies).
pub fn layer_name(id: &str) -> (&'static str, &'static str) {
    match id {
        "wifi" => ("Wi-Fi", "Wi-Fi"),
        "sim" => ("SIM 卡", "SIM"),
        "signal" => ("信号", "Signal"),
        "limit" => ("限速", "Speed cap"),
        "bearer" => ("数据连接", "Data connection"),
        "link" => ("蜂窝链路", "Cellular link"),
        "dns" => ("域名解析", "DNS"),
        "e2e" => ("外网", "Internet"),
        "clock" => ("设备时间", "Device clock"),
        "crowd" => ("基站负载", "Cell load"),
        "proxy" => ("代理", "Proxy"),
        _ => ("", ""),
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
        let layers: Vec<Layer> = LAYERS
            .into_iter()
            .filter(|id| proxy || *id != "proxy")
            .map(Layer::new)
            .collect();
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

/// What the runner reads for the signal and cap layers: the core's `/v2/screen`
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
    /// The active downlink carriers as the cellular page writes them ("n41 100M + B3 20M").
    pub carriers: String,
    /// Their total bandwidth, MHz.
    pub bw_mhz: Option<i64>,
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
    // plain words; the numbers are on the cellular page
    let mut zh = Vec::new();
    let mut en = Vec::new();
    match s(st, "sig") {
        "强" => (zh.push("信号强"), en.push("Strong signal")),
        "中" => (zh.push("信号一般"), en.push("Fair signal")),
        "弱" => (zh.push("信号弱"), en.push("Weak signal")),
        _ => ((), ()),
    };
    match s(st, "noise") {
        "小" => (zh.push("干扰小"), en.push("little noise")),
        "中" => (zh.push("有点干扰"), en.push("some noise")),
        "大" => (zh.push("干扰大"), en.push("heavy noise")),
        _ => ((), ()),
    };
    let mut zh = zh.join("，");
    let mut en = en.join(", ");
    if s(st, "cause") == "narrow" {
        // the words pick_main keys on: 「带宽只有」
        let bw = r.bw_mhz.map(|b| format!(" {b} MHz")).unwrap_or_default();
        zh = format!("{zh}，此刻带宽只有{bw}");
        en = format!("{en}; only{bw} of bandwidth right now");
    }
    l.set(level, zh, en)
}

pub fn judge_limit(r: &Radio) -> Layer {
    let l = Layer::new("limit");
    if r.qos_stale {
        return l.na(
            "QoS 是旧连接的，不下结论",
            "QoS is from an earlier bearer: no verdict",
        );
    }
    let label = if r.qos_nr { "5QI" } else { "QCI" };
    // the carriers the cell gives, then the QoS class
    let bw = if r.carriers.is_empty() {
        String::new()
    } else {
        format!(" · {}", r.carriers)
    };
    let qci = format!("{bw}{}", r.qci.map(|q| format!(" · {label} {q}")).unwrap_or_default());
    match r.ambr_dl {
        // the network sent a QCI but no AMBR: nothing says there is a cap
        None if r.qci.is_some() => l.set(Level::Ok, format!("没报限速{qci}"), format!("No cap reported{qci}")),
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

/// An HTTP `Date` ("Sun, 18 Oct 2026 10:50:00 GMT", RFC 7231 IMF-fixdate) as Unix seconds.
pub fn http_date_unix(s: &str) -> Option<i64> {
    let p: Vec<&str> = s.split_whitespace().collect();
    if p.len() != 6 || p[5] != "GMT" {
        return None;
    }
    let day: i64 = p[1].parse().ok()?;
    let mon = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]
        .iter()
        .position(|m| *m == p[2])? as i64
        + 1;
    let year: i64 = p[3].parse().ok()?;
    let t: Vec<i64> = p[4].split(':').map(|x| x.parse().ok()).collect::<Option<_>>()?;
    if t.len() != 3 || !(1..=31).contains(&day) {
        return None;
    }
    // days from civil (Howard Hinnant)
    let y = if mon <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (mon + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + t[0] * 3600 + t[1] * 60 + t[2])
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
    // ① good signal, poor RSRQ (the core's crowd rule without the "in use" part)
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

// ---- v2 layers (deep-diag-v2.md §2, §2.1) -------------------------------------------

/// What the eSIM channel check found (v2 Q1: only on an eUICC, only with the card ready).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EsimChannel {
    /// A logical channel is free (the lowest one).
    Free(u8),
    /// The card refused to open one (qmi_error 68): every channel is taken.
    Full,
    /// An eSIM operation holds the card: not checked.
    Busy,
    /// No eSIM tools, or the probe said something else: not known to be an eUICC.
    Unknown,
}

/// The sim layer's inputs: the hub's sim block (`get_sim_info`). Never the IMSI or ICCID.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SimIn {
    /// `sim_states` as the firmware says it; None = no sim block.
    pub state: Option<String>,
    /// None = not checked (card not ready, or no eSIM tools).
    pub channel: Option<EsimChannel>,
}

impl SimIn {
    /// The card is in and unlocked.
    /// The hub's `sim.state` is the ZTE `sim_states` ("sim ready" on the device, 10-18);
    /// "modem_init_complete" is its `modem_state` and is taken too.
    pub fn ready(&self) -> bool {
        matches!(self.state.as_deref().map(str::trim), Some("sim ready" | "modem_init_complete"))
    }
}

pub fn judge_sim(i: &SimIn) -> Layer {
    let l = Layer::new("sim");
    let st = i.state.as_deref().unwrap_or("").trim();
    if st.is_empty() {
        return l.na("读不到 SIM 卡状态", "SIM state unreadable");
    }
    if st.contains("undetected") || st.contains("destroy") || st.contains("absent") {
        return l.set(Level::Bad, "没有 SIM 卡", "No SIM card");
    }
    if st.contains("waitpuk") {
        return l.set(Level::Bad, "SIM 卡被 PUK 锁住", "SIM locked (PUK)");
    }
    if st.contains("waitpin") {
        return l.set(Level::Bad, "SIM 卡要输入 PIN", "SIM needs its PIN");
    }
    if !i.ready() {
        return l.na("SIM 卡还没就绪", "SIM not ready yet");
    }
    match i.channel {
        Some(EsimChannel::Full) => l.set(
            Level::Warn,
            "eSIM 通道占满，切换 profile 会失败",
            "eSIM channels all taken; switching profiles will fail",
        ),
        Some(EsimChannel::Free(_)) => l.set(
            Level::Ok,
            "已就绪 · eSIM 通道空闲",
            "Ready · eSIM channel free",
        ),
        Some(EsimChannel::Busy) => l.set(
            Level::Ok,
            "已就绪 · eSIM 正在操作，没查通道",
            "Ready · eSIM busy, channels not checked",
        ),
        // not an eUICC we can check: shown, not counted
        Some(EsimChannel::Unknown) | None => {
            let mut l = l.set(Level::Ok, "已就绪", "Ready");
            l.counted = false;
            l
        }
    }
}

/// The bearer layer's inputs: what the device says about its data call, read locally.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BearerIn {
    /// `wan_status` says connected; None = not readable.
    pub connected: Option<bool>,
    /// finding::path `has_ip`: None = can't tell.
    pub has_ip: Option<bool>,
    /// The cellular interface the paths use (rmnet_data0 on the device).
    pub iface: String,
    /// `/proc/net/route` has a default route; None = not readable.
    pub default_route: Option<bool>,
    /// Effective rp_filter on the cellular interface (max of `all` and its own).
    pub rp_filter: Option<u8>,
    /// Ways out besides the cellular one counted too (Tailscale, CHILL's TUN, other defaults).
    pub exits: usize,
}

pub fn judge_bearer(i: &BearerIn) -> Layer {
    let l = Layer::new("bearer");
    // Bad only on clear evidence: it blocks six layers.
    if i.connected == Some(false) {
        return l.set(Level::Bad, "没拨上号", "Not connected");
    }
    if i.has_ip == Some(false) {
        return l.set(
            Level::Bad,
            "拨上了，但没有地址",
            "Connected, but no address",
        );
    }
    if i.default_route == Some(false) {
        return l.set(Level::Bad, "没有默认路由", "No default route");
    }
    if i.connected.is_none() && i.has_ip.is_none() {
        return l.na("读不到数据连接", "Data connection unreadable");
    }
    let iface = if i.iface.is_empty() {
        "?"
    } else {
        i.iface.as_str()
    };
    if i.rp_filter == Some(1) && i.exits > 1 {
        return l.set(
            Level::Warn,
            format!("{iface} · rp_filter=1，有 {} 个出口，回包可能被丢", i.exits),
            format!(
                "{iface} · rp_filter=1 with {} ways out: replies may be dropped",
                i.exits
            ),
        );
    }
    l.set(
        Level::Ok,
        "已连上".to_string(),
        "Connected".to_string(),
    )
}

/// The device's own resolver (dnsmasq, 127.0.0.1:53) asked a few names.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LocalDns {
    pub asked: usize,
    pub answered: usize,
    /// The slowest answer.
    pub ms: Option<u32>,
}

use crate::finding::{NetworkEvidence, ResultState};

/// `NetworkEvidence.reason` in words after the row's words. The normal case (a scoped probe ran)
/// adds nothing; an unknown code is shown as is.
fn with_reason(zh: &str, en: &str, reason: &str) -> (String, String) {
    let (rz, re) = match reason {
        "" | "scoped_http_dns_only" => return (zh.into(), en.into()),
        "stale_or_unknown_snapshot" => ("状态太旧", "state too old"),
        "snapshot_or_connection_changed"
        | "snapshot_or_connection_changed_during_probe" => {
            ("检测时连接变了", "connection changed during check")
        }
        "probe_budget_unavailable" => ("刚测过，稍后再试", "checked moments ago; try shortly"),
        "device_bound_policy_unverified" => ("出口绑定没核实", "route binding unverified"),
        "proxy_route_unverified" => ("流量经代理", "traffic goes through the proxy"),
        "no_cellular_path" => ("没有蜂窝出口", "no cellular way out"),
        "ipv6_http_targets_unverified" => ("IPv6 目标没核实", "IPv6 targets unverified"),
        other => (other, other),
    };
    (format!("{zh} · {rz}"), format!("{en} · {re}"))
}

/// dns: the device's dnsmasq against the bound operator DNS of the Finding path (§2.1).
pub fn judge_dns(local: Option<&LocalDns>, net: Option<&NetworkEvidence>) -> Layer {
    let l = Layer::new("dns");
    let carrier = net.map_or(ResultState::Unknown, |n| n.dns);
    let reason = net.map_or("", |n| n.reason.as_str());
    let local = local.filter(|d| d.asked > 0);
    match local {
        Some(d) if d.answered == 0 => match carrier {
            ResultState::Passed => l.set(
                Level::Bad,
                "设备的 DNS 坏了 · 运营商 DNS 能答",
                "The device's DNS is broken · carrier DNS answers",
            ),
            ResultState::Failed => l.set(Level::Bad, "DNS 不通", "DNS not answering"),
            ResultState::Unknown => l.set(
                Level::Bad,
                "设备的 DNS 没回应",
                "The device's DNS is not answering",
            ),
        },
        Some(d) if d.answered < d.asked => l.set(
            Level::Warn,
            format!("设备的 DNS 只答了 {}/{}", d.answered, d.asked),
            format!("The device's DNS answered {}/{}", d.answered, d.asked),
        ),
        Some(d) => {
            let ms = d.ms.unwrap_or(0);
            let (zh, en) = match carrier {
                ResultState::Passed => ("运营商的也正常", "the carrier's too"),
                ResultState::Failed => ("运营商的没回应", "the carrier's not answering"),
                ResultState::Unknown => ("运营商的没测", "the carrier's not checked"),
            };
            let level = if ms > LOCAL_DNS_SLOW_MS {
                Level::Warn
            } else {
                Level::Ok
            };
            l.set(
                level,
                format!("能解析，{ms} ms · {zh}"),
                format!("Resolves in {ms} ms · {en}"),
            )
        }
        None => {
            let (level, zh, en) = match carrier {
                ResultState::Passed => (Level::Ok, "运营商 DNS 能答", "Carrier DNS answers"),
                ResultState::Failed => (Level::Bad, "DNS 不通", "DNS not answering"),
                ResultState::Unknown => (Level::Na, "没测成", "Not checked"),
            };
            let (zh, en) = with_reason(zh, en, reason);
            l.set(level, zh, en)
        }
    }
}

/// e2e: the Finding path's bound HTTP attempts (§2.1), Passed/Failed/Unknown → ok/bad/na.
pub fn judge_e2e(net: Option<&NetworkEvidence>) -> Layer {
    let l = Layer::new("e2e");
    let Some(n) = net else {
        return l.na("超时", "Timed out");
    };
    let (level, zh, en) = match n.path_reachable {
        ResultState::Passed => (Level::Ok, "能上网", "Online"),
        ResultState::Failed => (Level::Bad, "检测目标没回应", "Probe targets did not reply"),
        ResultState::Unknown => (Level::Na, "没测成", "Not checked"),
    };
    let (zh, en) = with_reason(zh, en, &n.reason);
    l.set(level, zh, en)
}

/// clock: the clock evidence's status (§2.1), not an HTTP `Date`.
/// Seconds of clock difference that are still fine, and past which HTTPS breaks.
pub const CLOCK_OK_S: i64 = 30;
pub const CLOCK_BAD_S: i64 = 300;

/// clock: the device clock against an HTTP server's `Date` when one answered (`skew` = device
/// ahead of it, s); without one, the clock evidence's status (§2.1).
pub fn judge_clock(status: Option<crate::clock::EvidenceStatus>, skew: Option<i64>) -> Layer {
    use crate::clock::EvidenceStatus as E;
    let l = Layer::new("clock");
    if let Some(d) = skew {
        let (zh, en) = if d.abs() <= 1 {
            ("和网络时间一致".to_string(), "Matches network time".to_string())
        } else if d > 0 {
            (format!("快 {d} 秒"), format!("{d} s fast"))
        } else {
            (format!("慢 {} 秒", -d), format!("{} s slow", -d))
        };
        return match d.abs() {
            a if a <= CLOCK_OK_S => l.set(Level::Ok, zh, en),
            a if a <= CLOCK_BAD_S => l.set(Level::Warn, zh, en),
            _ => l.set(
                Level::Bad,
                format!("{zh}，HTTPS 会失败"),
                format!("{en}; HTTPS will fail"),
            ),
        };
    }
    match status.unwrap_or_default() {
        E::Verified => l.set(Level::Ok, "已核对", "Verified"),
        E::Estimated => l.set(Level::Info, "估算的，没核对", "Estimated, not verified"),
        E::Stale | E::BootMismatch => l.set(
            Level::Warn,
            "时间证据过期了，HTTPS 可能失败",
            "Clock evidence is stale; HTTPS may fail",
        ),
        E::WallChanged => l.set(
            Level::Warn,
            "设备时间被改过，HTTPS 可能失败",
            "Device clock was changed; HTTPS may fail",
        ),
        E::Revoked => l.set(
            Level::Warn,
            "设备时间不再可信，HTTPS 可能失败",
            "Device clock no longer trusted; HTTPS may fail",
        ),
        E::Unknown => l.na("不知道准不准", "Unknown"),
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
        // no card / PIN: the row itself
        "sim" if m.level == Level::Bad => (m.detail.as_str(), m.detail_en.as_str(), "", "", ""),
        "sim" => (
            "eSIM 通道占满",
            "eSIM channels all taken",
            "切换 profile 前先重启设备",
            "Restart the device before switching profiles",
            "",
        ),
        "bearer" if m.level == Level::Bad => (
            "数据连接没建立",
            "Data connection is down",
            "看看数据开关和 APN",
            "Check mobile data and the APN",
            "",
        ),
        "bearer" => (
            "回包可能被丢",
            "Replies may be dropped",
            "多出口时 rp_filter 要是 0 或 2",
            "With several ways out, rp_filter should be 0 or 2",
            "",
        ),
        "dns" => (
            "域名解析有问题",
            "DNS problem",
            "过几分钟再试；还不行就重启设备",
            "Try again in a few minutes; if it stays, restart the device",
            "",
        ),
        "e2e" => (
            "外网不通",
            "Internet unreachable",
            "过几分钟再试，或换个地方",
            "Try again in a few minutes, or move",
            "",
        ),
        "clock" => (
            "设备时间可能不准",
            "Device clock may be off",
            "HTTPS 网站打不开时，先等时间同步",
            "If HTTPS sites fail, wait for the clock to sync",
            "",
        ),
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
        "signal" if m.detail.contains("带宽只有") => (
            "此刻带宽窄",
            "Narrow bandwidth right now",
            "空闲时 5G 常会暂时掉线；一直这样就换个地方",
            "Idle 5G often drops for a while; if it stays, try another spot",
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
        "finding" => ("正在检查网络", "Checking the connection"),
        "txn" => ("正在改设置", "Applying a setting"),
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
