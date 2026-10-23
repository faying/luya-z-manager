//! /tools/diagnose (网络诊断)
//! (slow-diagnosis.md §4.2, §12): status block (main cause + one action, or 正在检查… N/M,
//! 等另一个操作做完…, 后台没回应) · 「HH:MM 测」 · 「再查一次」 → one row per layer, then the speed row
//! → what you can do (「加测速度」, two presses when roaming; 「对 / 不对」).
//!
//! The device service judges and words everything in both languages; the run is the
//! readback of every button (start, speed, feedback), so they are plain requests: the stream reads
//! `GET /api/diagnose` every second while the run or its speed row goes, else every 30 s (the touch
//! screen may start one). The agent picks the Wi-Fi rules by who asks: the browser's address goes
//! along (agent.rs, X-Forwarded-For).
//!
//! Starting by itself (§12.5: no result from the last 10 minutes → check on entry) only when the
//! user came from another page of this site or with `?start=1`; never on a reload or a typed URL.
//! The old single-page app told by the navigation entry; here the page decides in the browser
//! (referrer from this site, not a reload) and posts `start?auto=1`, which starts only when idle.

use std::time::{Duration, Instant, SystemTime};

use askama::Template;
use axum::extract::{Query, State};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::{Caller, Fail, Polled};
use crate::i18n::Lang;
use crate::pages::{Shared, StreamQuery, async_stream, patch, split_leaves};

const PATH: &str = "/tools/diagnose";
const LAYER_ORDER: [&str; 11] = [
    "wifi", "sim", "signal", "limit", "bearer", "link", "dns", "e2e", "clock", "crowd", "proxy",
];

fn pick(v: &Value, key: &str, l: Lang) -> String {
    let zh = v[key].as_str().unwrap_or("");
    let en = v[format!("{key}_en")].as_str().unwrap_or("");
    match l {
        Lang::En if !en.is_empty() => en.into(),
        _ => zh.into(),
    }
}

fn is_run(d: &Value) -> bool {
    matches!(d["state"].as_str(), Some("waiting" | "running" | "done")) && d["layers"].is_array()
}

fn is_live(d: &Value) -> bool {
    is_run(d) && (d["state"] != "done" || d["speed"]["level"] == "running")
}

fn layer_name(id: &str, l: Lang) -> String {
    match id {
        "wifi" => "Wi-Fi",
        "sim" => l.pick("SIM", "SIM 卡"),
        "signal" => l.pick("Signal", "信号"),
        "limit" => l.pick("Speed cap", "限速"),
        "bearer" => l.pick("Data connection", "数据连接"),
        "link" => l.pick("Cellular link", "蜂窝链路"),
        "dns" => l.pick("DNS", "域名解析"),
        "e2e" => l.pick("Internet", "外网"),
        "clock" => l.pick("Device clock", "设备时间"),
        "crowd" => l.pick("Cell load", "基站负载"),
        "proxy" => l.pick("Proxy", "代理"),
        "speed" => l.pick("Speed", "速度"),
        other => other,
    }
    .into()
}

pub struct RowV {
    pub name: String,
    pub muted: bool,
    pub tone: Option<&'static str>,
    pub word: String,
    pub detail: String,
}

impl RowV {
    pub fn symbol(&self) -> &'static str {
        match self.tone {
            Some("warn") => "▲",
            Some("bad") => "■",
            _ => "●",
        }
    }
}

fn row_view(layer: &Value, l: Lang, speed_for: Option<u64>) -> RowV {
    let detail = pick(layer, "detail", l);
    let muted = layer["counted"].as_bool() == Some(false);
    let name = layer_name(layer["id"].as_str().unwrap_or(""), l);
    let (tone, word, detail, muted) = match layer["level"].as_str().unwrap_or("") {
        "pending" => (
            None,
            l.pick("Waiting", "等待").to_string(),
            String::new(),
            true,
        ),
        "running" => {
            let w = match (speed_for, l) {
                (Some(s), Lang::En) if s > 0 => format!("Testing… {s} s"),
                (Some(s), Lang::Zh) if s > 0 => format!("测试中… {s} s"),
                _ => l.pick("Testing…", "测试中…").into(),
            };
            (None, w, String::new(), false)
        }
        "ok" => (
            Some(if muted { "neutral" } else { "ok" }),
            l.pick("OK", "正常").into(),
            detail,
            muted,
        ),
        "warn" => (
            Some("warn"),
            l.pick("Suspect", "疑点").into(),
            detail,
            muted,
        ),
        "bad" => (Some("bad"), l.pick("Poor", "差").into(), detail, muted),
        // not run: a layer before it is down (grey, not counted)
        "blocked" => (
            Some("neutral"),
            l.pick("Not checked", "没测").into(),
            detail,
            true,
        ),
        "na" => (
            Some("neutral"),
            if detail.is_empty() {
                l.pick("Can't check", "测不了").into()
            } else {
                match l {
                    Lang::En => format!("Can't check · {detail}"),
                    Lang::Zh => format!("测不了 · {detail}"),
                }
            },
            String::new(),
            true,
        ),
        _ => (None, detail, String::new(), muted),
    };
    RowV {
        name,
        muted,
        tone,
        word,
        detail,
    }
}

pub enum Action {
    None,
    Retry,
    Start,
}

pub struct Next {
    pub roaming: bool,
    pub measuring: bool,
    pub answered: bool,
}

mod restore;

pub struct View {
    pub l: Lang,
    pub tone: &'static str,
    pub word: String,
    pub reason: Option<String>,
    /// The action leads to the proxy page (a link after the reason).
    pub proxy: bool,
    pub bar: Option<(i64, i64)>,
    pub at: Option<String>,
    pub action: Action,
    pub run: bool,
    pub down: bool,
    pub rows: Vec<RowV>,
    pub next: Option<Next>,
    /// The core's open finding (registered but no internet, T6), with or without a run.
    pub finding: Option<FindingV>,
    /// The decision loop's pending proposal (core §4.9), shown under the finding for a person
    /// to approve (§4.10).
    pub proposal: Option<ProposalV>,
    /// `radio.band_flap` (core §4.11): only when the replay explains; never an action.
    pub flap: Option<String>,
    /// `power.hot_drain` (core §4.11): hot and draining fast, or hot with the drain unreadable;
    /// never an action.
    pub hot: Option<String>,
    /// `usage.long_idle` (core §4.11): only when the replay suggests; never an action.
    pub idle: Option<String>,
    /// `perf.slow_strong_signal` (core §4.11): proven vs only possible layers when the signal is
    /// strong; never an action.
    pub slow: Option<String>,
    /// The last verified connection (core `GET /api/verified`): what differs and the button.
    pub restore: Option<restore::RestoreV>,
}

/// Core `data.slow_signal`: words for the person when the replay explains, nothing otherwise.
fn slow_view(b: &Value, l: Lang) -> Option<String> {
    let a = &b["advice"];
    if a["advice"] == "suggest" {
        let m = a["busy_s"].as_u64()? / 60;
        return Some(match l {
            Lang::En => format!(
                "The signal is strong and data has been moving for {m} min. If it feels slow, \
                 run a check above: it tells which part is slowing it down. Nothing runs by itself."
            ),
            Lang::Zh => format!(
                "信号是强的，已经连续传了 {m} 分钟数据。如果觉得慢，点上面的检查，它会查出慢在哪一层；不会自己跑。"
            ),
        });
    }
    if a["advice"] != "explain" {
        return None;
    }
    let names = |k: &str| -> Vec<String> {
        a[k].as_array()
            .map(|v| {
                v.iter()
                    .filter_map(|x| x.as_str())
                    .filter(|id| luyaz_api::execution::slow_signal::LAYERS.contains(id))
                    .map(|id| layer_name(id, l))
                    .collect()
            })
            .unwrap_or_default()
    };
    let (proven, possible) = (names("proven"), names("possible"));
    if proven.is_empty() && possible.is_empty() {
        return None;
    }
    let sep = l.pick(", ", "、");
    let mut out = l
        .pick(
            "The signal is strong, so the slowness is most likely not the radio.",
            "信号是强的，所以慢多半不在信号。",
        )
        .to_string();
    if !proven.is_empty() {
        out.push(' ');
        out.push_str(&match l {
            Lang::En => format!(
                "Found by the check: {} (see its row for what to do).",
                proven.join(sep)
            ),
            Lang::Zh => format!("这次检查查实的：{}（怎么处理看那一行）。", proven.join(sep)),
        });
    }
    if !possible.is_empty() {
        out.push(' ');
        out.push_str(&match l {
            Lang::En => format!(
                "Only possible: {} (no comparison with real traffic on this cell).",
                possible.join(sep)
            ),
            Lang::Zh => format!(
                "只是可能：{}（没有同一小区实际流量的对照）。",
                possible.join(sep)
            ),
        });
    }
    out.push(' ');
    out.push_str(l.pick(
        "Nothing is changed automatically; switching network mode, band or proxy is yours to do.",
        "这里不会自动改；换制式、锁频或关代理都由你自己决定。",
    ));
    Some(out)
}

/// Core `data.long_idle`: words for the person when the replay suggests the CHILL idle pause,
/// nothing otherwise (already saving, in use and unknown stay off the page).
fn idle_view(b: &Value, l: Lang) -> Option<String> {
    let a = &b["advice"];
    if a["advice"] != "suggest" || a["step"] != "chill_idle_pause" {
        return None;
    }
    let n = a["hours"].as_u64()?;
    Some(match l {
        Lang::En => format!(
            "Nobody has used the device for the last {n} hours (screen off, no devices connected, next to no traffic). The CHILL idle pause could save power then: add LUYAZ_CHILL_IDLE=1 to core.env yourself and restart luyaz-core (/etc/init.d/luyaz-core restart); it pauses CHILL after 5 minutes with nobody around and resumes as soon as the screen or a device is back. Nothing is changed automatically."
        ),
        Lang::Zh => format!(
            "最近 {n} 小时没人用（息屏、没有设备连着、几乎没有流量）。这种时候 CHILL 闲置暂停能省电：可以自己在 core.env 加 LUYAZ_CHILL_IDLE=1 再重启 luyaz-core（/etc/init.d/luyaz-core restart），没人用 5 分钟后暂停 CHILL，一亮屏或有设备连上就恢复。这里不会自动改。"
        ),
    })
}

fn deg(dc: i64) -> String {
    format!("{:.1}", dc as f64 / 10.0)
}

/// Core `data.hot_drain`: words for the person when the replay explains, or when the battery is
/// hot but no clean counter reading says anything about the drain. Everything else stays off.
fn hot_view(b: &Value, l: Lang) -> Option<String> {
    let a = &b["advice"];
    let kind = a["advice"].as_str()?;
    let why = a["why"].as_str().unwrap_or("");
    let drain_unread =
        kind == "unknown" && matches!(why, "drain_unqualified" | "no_drain_baseline");
    if kind != "explain" && !drain_unread {
        return None;
    }
    let src = if kind == "explain" { a } else { b };
    let (t, n) = (src["batt_dc"].as_i64()?, src["batt_normal_dc"].as_i64()?);
    let cell = match b["window"]["cell"].as_str()? {
        "on_home" => l.pick("screen on, home network", "亮屏、本地网络"),
        "on_abroad" => l.pick("screen on, roaming", "亮屏、漫游"),
        "off_home" => l.pick("screen off, home network", "息屏、本地网络"),
        "off_abroad" => l.pick("screen off, roaming", "息屏、漫游"),
        _ => return None,
    };
    let head = match l {
        Lang::En => format!(
            "Last hour the battery reached {} °C; in the same condition ({cell}) it usually stays at or below {} °C.",
            deg(t),
            deg(n)
        ),
        Lang::Zh => format!(
            "上一小时电池最高 {} °C；同样情况（{cell}）下平时不超过 {} °C。",
            deg(t),
            deg(n)
        ),
    };
    let ambient = l.pick(
        "Room temperature isn't measured, so a warm place or a bag can't be ruled out.",
        "没有测环境温度，所以排除不了放在热的地方或包里。",
    );
    if drain_unread {
        let d = if why == "drain_unqualified" {
            l.pick(
                "Whether it also drained faster can't be told: no clean battery counter reading for that hour (devices connected, or the condition changed).",
                "耗电有没有变快判断不了：那一小时没有干净的电量计读数（连着设备，或中途情况变了）。",
            )
        } else {
            l.pick(
                "Whether it also drained faster can't be told yet: too few clean hours in this condition to compare with.",
                "耗电有没有变快暂时判断不了：这种情况下干净的对照小时还不够。",
            )
        };
        return Some(format!("{head} {d} {ambient}"));
    }
    let (mw, nmw) = (a["drain_mw"].as_u64()?, a["drain_normal_mw"].as_u64()?);
    let words = |k: &str| -> Option<&'static str> {
        Some(match k {
            "clients" => l.pick("devices connected", "有设备连着"),
            "cellular" => l.pick("more cellular traffic than usual", "蜂窝流量比平时多"),
            "cpu" => l.pick("the CPU busier than usual", "CPU 比平时忙"),
            "throttled" => l.pick("the CPU throttled", "CPU 降过频"),
            "chip_hot" => l.pick("the chip hotter than usual", "芯片比平时热"),
            "battery_health" => l.pick("battery health not Good", "电池健康不是 Good"),
            _ => return None,
        })
    };
    let list = |k: &str| -> Vec<&'static str> {
        a[k].as_array()
            .map(|v| v.iter().filter_map(|x| words(x.as_str()?)).collect())
            .unwrap_or_default()
    };
    let (with, unread) = (list("alongside"), list("unread"));
    let sep = l.pick(", ", "、");
    let mut out = match l {
        Lang::En => format!("{head} It also drained about {mw} mW against a usual {nmw} mW."),
        Lang::Zh => format!("{head} 耗电约 {mw} mW，平时约 {nmw} mW。"),
    };
    out.push(' ');
    out.push_str(&match (l, with.is_empty()) {
        (Lang::En, false) => format!(
            "At the same time: {} (happened together, not necessarily the cause).",
            with.join(sep)
        ),
        (Lang::Zh, false) => format!("同时：{}（只是同时发生，不一定是原因）。", with.join(sep)),
        (Lang::En, true) => "Nothing else stood out.".into(),
        (Lang::Zh, true) => "别的方面看不出异常。".into(),
    });
    if !unread.is_empty() {
        out.push(' ');
        out.push_str(&match l {
            Lang::En => format!("Not compared: {}.", unread.join(sep)),
            Lang::Zh => format!("没能比较：{}。", unread.join(sep)),
        });
    }
    let tail = l.pick(
        "Nothing is changed automatically; power-saving settings (CHILL, Wi-Fi) are yours to choose.",
        "这里不会自动改任何设置；要不要开省电（CHILL、Wi-Fi）由你决定。",
    );
    Some(format!("{out} {ambient} {tail}"))
}

/// Core `data.band_flap`: words for the person when the replay explains, nothing otherwise
/// (unknown and "nothing to say" stay off the page: no guessing).
fn flap_view(b: &Value, l: Lang) -> Option<String> {
    let a = &b["advice"];
    if a["advice"] != "explain" {
        return None;
    }
    let band: String = a["band"]
        .as_str()
        .filter(|s| !s.is_empty())?
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect();
    let n = a["drops"].as_u64()?;
    let min = b["window_s"].as_u64().unwrap_or(1800) / 60;
    Some(match l {
        Lang::En => format!(
            "The data connection dropped {n} times in the last {min} minutes, all on {band}, with good signal, on one cell and nothing else to explain it. If it keeps happening, consider changing the network mode or band yourself (Band lock); nothing is changed automatically."
        ),
        Lang::Zh => format!(
            "最近 {min} 分钟数据连接断了 {n} 次，都在 {band} 上；信号不差、没换小区，也没有别的原因能解释。如果一直这样，可以考虑自己改网络模式或频段（锁频页）；这里不会自动改。"
        ),
    })
}

pub struct ProposalV {
    pub id: String,
    pub digest: String,
    pub tail: String,
}

/// Only the one action the loop proposes today; anything else is not shown (no button for an
/// action this page can't describe).
fn proposal_view(p: &Value) -> Option<ProposalV> {
    if p["action"] != "data.reconnect" {
        return None;
    }
    let s = |k: &str| p[k].as_str().filter(|v| !v.is_empty()).map(str::to_owned);
    Some(ProposalV {
        id: s("proposal_id")?,
        digest: s("params_digest")?,
        tail: s("iccid_tail").unwrap_or_default(),
    })
}

/// The evidence page of one open finding: what it is, how sure, since when, per layer.
pub struct FindingV {
    pub kind: String,
    pub tail: String,
    pub symptom: String,
    pub sure: &'static str,
    pub conf: String,
    pub since: String,
    pub rows: Vec<RowV>,
}

fn finding_layer(id: &str, l: Lang) -> String {
    match id {
        "signal" => l.pick("Signal / registration", "信号/注册"),
        "data" => l.pick("Data connection", "数据连接"),
        "internet" => l.pick("Internet", "外网"),
        "dns" => l.pick("DNS", "域名解析"),
        _ => id,
    }
    .to_string()
}

fn finding_view(f: &Value, l: Lang) -> Option<FindingV> {
    let kind = f["kind"].as_str().filter(|k| !k.is_empty())?.to_string();
    let (sure, conf) = match f["confidence"].as_str().unwrap_or("") {
        "sure" => ("bad", l.pick("Sure", "确定")),
        "likely" => ("warn", l.pick("Likely", "可能")),
        _ => ("neutral", l.pick("A guess", "猜测")),
    };
    let hm = |k: &str| {
        f[k].as_u64()
            .map(|t| crate::agent::hhmm(SystemTime::UNIX_EPOCH + Duration::from_secs(t)))
    };
    let n = f["count"].as_u64().unwrap_or(1);
    let since = match (hm("first"), hm("last"), l) {
        (Some(a), Some(b), Lang::En) => format!("Since {a} · last seen {b} · {n}×"),
        (Some(a), Some(b), Lang::Zh) => format!("{a} 起 · 最近 {b} · 第 {n} 次"),
        _ => String::new(),
    };
    let rows = f["evidence"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|e| {
                    let mut r = row_view(e, l, None);
                    r.name = finding_layer(e["layer"].as_str().unwrap_or(""), l);
                    if e["level"] == "bad" {
                        r.word = l.pick("Down", "不通").into();
                    }
                    r
                })
                .collect()
        })
        .unwrap_or_default();
    Some(FindingV {
        kind,
        tail: f["iccid_tail"].as_str().unwrap_or("").to_string(),
        symptom: pick(f, "symptom", l),
        sure,
        conf: conf.into(),
        since,
        rows,
    })
}

impl View {
    pub fn symbol(&self) -> &'static str {
        match self.tone {
            "warn" => "▲",
            "bad" => "■",
            _ => "●",
        }
    }
    pub fn pct(&self) -> i64 {
        match self.bar {
            Some((s, n)) if n > 0 => s * 100 / n,
            _ => 0,
        }
    }
}

#[derive(Default)]
struct Got {
    diag: Option<Value>,
    err: Option<Fail>,
    roaming: bool,
    /// When this stream first saw the speed row measuring (「测试中… 3 s」).
    speed_since: Option<Instant>,
    /// Core `GET /api/verified`, and when it was read.
    verified: Option<Value>,
    verified_at: Option<Instant>,
}

fn down(e: &Option<Fail>) -> bool {
    matches!(
        e,
        Some(Fail::Unreachable) | Some(Fail::Refused(500.., _, _))
    )
}

fn view(g: &Got, l: Lang) -> View {
    let d = g.diag.as_ref().filter(|d| is_run(d));
    let down = down(&g.err);
    let no_diag = !down
        && d.is_none()
        && matches!(&g.err, Some(Fail::Refused(c, _, _)) if (400..500).contains(c));
    let state = d.and_then(|d| d["state"].as_str()).unwrap_or("");
    let checking_note = l.pick(
        "About 10 s; sends a few probe packets",
        "约 10 秒，会发少量探测包",
    );
    let mut v = View {
        l,
        tone: "neutral",
        word: String::new(),
        reason: None,
        proxy: false,
        bar: None,
        at: None,
        action: Action::None,
        run: d.is_some(),
        down,
        rows: Vec::new(),
        next: None,
        finding: g.diag.as_ref().and_then(|d| finding_view(&d["finding"], l)),
        proposal: g.diag.as_ref().and_then(|d| proposal_view(&d["proposal"])),
        flap: g.diag.as_ref().and_then(|d| flap_view(&d["band_flap"], l)),
        hot: g.diag.as_ref().and_then(|d| hot_view(&d["hot_drain"], l)),
        idle: g.diag.as_ref().and_then(|d| idle_view(&d["long_idle"], l)),
        slow: g
            .diag
            .as_ref()
            .and_then(|d| slow_view(&d["slow_signal"], l)),
        restore: g
            .verified
            .as_ref()
            .and_then(|v| restore::view(v, l)),
    };
    if down {
        v.tone = "bad";
        v.word = l.pick("Agent not responding", "后台没回应").into();
        v.reason = d.map(|_| {
            l.pick(
                "Below is the last result received",
                "下面是收到的最后一份结果",
            )
            .into()
        });
        v.action = Action::Retry;
    } else if let (Some(d), "waiting") = (d, state) {
        v.word = l.pick("Waiting…", "等另一个操作做完…").into();
        let what = match d["waiting_for"].as_str() {
            Some("scan") => l.pick("Searching for networks", "正在搜网"),
            Some("register") => l.pick("Registering on a network", "正在选网"),
            Some("speedtest") => l.pick("Speed test running", "正在测速"),
            _ => "",
        };
        v.reason = (!what.is_empty()).then(|| match l {
            Lang::En => format!("{what}; starts when it's done"),
            Lang::Zh => format!("{what}，做完自动开始"),
        });
    } else if let (Some(d), "running") = (d, state) {
        let layers = d["layers"].as_array().cloned().unwrap_or_default();
        let counted: Vec<&Value> = layers.iter().filter(|x| x["counted"] != false).collect();
        let steps = d["steps"]
            .as_i64()
            .filter(|n| *n > 0)
            .unwrap_or(counted.len() as i64);
        let step = d["step"].as_i64().unwrap_or(
            counted
                .iter()
                .filter(|x| !matches!(x["level"].as_str(), Some("pending" | "running")))
                .count() as i64,
        );
        let step = step.min(steps);
        v.word = match l {
            Lang::En => format!("Checking… {step}/{steps}"),
            Lang::Zh => format!("正在检查… {step}/{steps}"),
        };
        v.reason = Some(checking_note.into());
        v.bar = Some((step, steps));
    } else if let (Some(d), "done") = (d, state) {
        let m = &d["main"];
        if m["layer"].as_str().unwrap_or("").is_empty() {
            v.tone = "ok";
            let t = pick(m, "text", l);
            v.word = if t.is_empty() {
                l.pick("No problem found", "没查到问题").into()
            } else {
                t
            };
            let a = pick(m, "action", l);
            v.reason = Some(if a.is_empty() {
                l.pick(
                    "The site itself may be slow; you can also add a speed test",
                    "可能是对方网站慢；也可以加测速度",
                )
                .into()
            } else {
                a
            });
        } else {
            v.tone = match m["level"].as_str() {
                Some("bad") => "bad",
                Some("warn") => "warn",
                _ => "neutral",
            };
            v.word = pick(m, "text", l);
            let action = pick(m, "action", l);
            let n = m["more"].as_i64().unwrap_or(0);
            let more = (n > 0).then(|| match l {
                Lang::En => format!("+{n} more"),
                Lang::Zh => format!("另有 {n} 处疑点"),
            });
            v.proxy = m["action_to"] == "proxy";
            let parts: Vec<String> = [Some(action).filter(|a| !a.is_empty()), more]
                .into_iter()
                .flatten()
                .collect();
            v.reason = (!parts.is_empty()).then(|| parts.join(" · "));
        }
        v.at = d["finished_at"].as_u64().map(|t| {
            let hm = crate::agent::hhmm(SystemTime::UNIX_EPOCH + Duration::from_secs(t));
            match l {
                Lang::En => format!("Checked {hm}"),
                Lang::Zh => format!("{hm} 测"),
            }
        });
    } else if no_diag {
        v.word = l
            .pick(
                "The agent on the device can't diagnose yet",
                "设备上的后台还不能诊断",
            )
            .into();
        v.reason = g.err.as_ref().map(|e| e.text(l));
    } else if g.diag.is_none() {
        v.word = l.pick("Reading…", "正在读取…").into();
        if g.err == Some(Fail::SignedOut) {
            v.reason = Some(Fail::SignedOut.text(l));
        }
    } else {
        v.word = l.pick("Not checked yet", "还没查过").into();
        v.reason = Some(
            l.pick(
                "Find out whether it's Wi-Fi, the SIM, signal, a speed cap, the data connection, DNS or cell load",
                "查一查卡在 Wi-Fi、SIM 卡、信号、限速、数据连接、域名解析还是基站负载",
            )
            .into(),
        );
        v.action = Action::Start;
    }
    if let Some(d) = d {
        let mut layers = d["layers"].as_array().cloned().unwrap_or_default();
        let rank = |x: &Value| {
            LAYER_ORDER
                .iter()
                .position(|id| x["id"] == *id)
                .unwrap_or(LAYER_ORDER.len())
        };
        layers.sort_by_key(rank);
        v.rows = layers.iter().map(|x| row_view(x, l, None)).collect();
        if d["speed"].is_object() {
            let s = g.speed_since.map(|t| t.elapsed().as_secs());
            v.rows.push(row_view(&d["speed"], l, s));
        }
        if state == "done" {
            v.next = Some(Next {
                roaming: g.roaming,
                measuring: d["speed"]["level"] == "running",
                answered: !d["feedback"].is_null(),
            });
        }
    }
    v
}

#[derive(Template)]
#[template(path = "diagnose_leaves.html")]
struct Leaves<'a> {
    v: &'a View,
}

pub struct Poll {
    who: Caller,
    l: Lang,
    got: Got,
    roam_at: Option<Instant>,
}

impl Poll {
    fn new(who: Caller, l: Lang) -> Poll {
        Poll {
            who,
            l,
            got: Got::default(),
            roam_at: None,
        }
    }

    async fn fetch(&mut self, app: &Shared) {
        match crate::agent::get(app, &self.who, "/api/diagnose").await {
            Ok(d) => {
                let measuring = is_run(&d) && d["speed"]["level"] == "running";
                if !measuring {
                    self.got.speed_since = None;
                } else if self.got.speed_since.is_none() {
                    self.got.speed_since = Some(Instant::now());
                }
                self.got.diag = Some(d);
                self.got.err = None;
            }
            Err(e) => self.got.err = Some(e),
        }
        // the verified set reads the radio: every 15 s, every step while a restore runs
        let restoring = self.got.verified.as_ref().is_some_and(|v| {
            v["progress"].is_object() && v["progress"]["end"].is_null()
        });
        if restoring
            || self
                .got
                .verified_at
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(15))
        {
            if let Ok(v) = crate::agent::get(app, &self.who, "/api/verified").await {
                self.got.verified = Some(v);
                self.got.verified_at = Some(Instant::now());
            }
        }
        // roaming decides the two-press speed test (same key as the home page), every 30 s
        if self
            .roam_at
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(30))
        {
            if let Ok(n) = crate::agent::get(app, &self.who, "/api/netinfo?lite=1").await {
                self.got.roaming = n["roaming"].as_bool() == Some(true);
                self.roam_at = Some(Instant::now());
            }
        }
    }

    fn leaves(&self) -> Vec<(String, String)> {
        let v = view(&self.got, self.l);
        split_leaves(&Leaves { v: &v }.render().unwrap())
    }
}

impl Polled for Poll {
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        self.fetch(app).await;
        let live = self.got.diag.as_ref().is_some_and(is_live);
        let every = if live {
            Duration::from_secs(1)
        } else {
            Duration::from_secs(30)
        };
        (self.leaves(), every)
    }
}

#[derive(Template)]
#[template(path = "diagnose.html")]
struct Page {
    shell: crate::shell::Shell,
    leaves: Vec<(String, String)>,
    /// Nothing to show from the last 10 minutes: the page may start a run by itself.
    idle: bool,
    /// The restore's write area (its dialog), outside the streamed parts.
    rs_op: String,
}

impl Page {
    fn leaf(&self, id: &str) -> &str {
        self.leaves
            .iter()
            .find(|(k, _)| k == id)
            .map_or("", |(_, h)| h.as_str())
    }
}

pub async fn page(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let mut p = Poll::new(Caller::of(&app, &req), ctx.lang);
    p.fetch(&app).await;
    let idle = p.got.diag.as_ref().is_some_and(|d| !is_run(d));
    let shell = app.hub.read(|st| {
        crate::shell::Shell::new(&headers, PATH).live(
            st,
            &ctx,
            Some(format!("{PATH}/events?g={}", st.generation)),
        )
    });
    let html = Page {
        shell,
        leaves: p.leaves(),
        idle,
        rs_op: restore::area(ctx.lang, ctx.remote, &crate::write::Phase::Idle),
    }
    .render()
    .unwrap();
    crate::assets::maybe_gzip(
        &headers,
        "text/html; charset=utf-8",
        html.into_bytes(),
        "no-store",
    )
}

pub async fn events(
    State(app): State<Shared>,
    Query(q): Query<StreamQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let p = Poll::new(Caller::of(&app, &req), ctx.lang);
    crate::agent::stream(app, ctx, q.g, PATH, p)
}

// ---- buttons ----

fn note(tone: &str, text: &str) -> String {
    if text.is_empty() {
        return "<div id=\"dg-op\" class=\"grid gap-1\"></div>".into();
    }
    if tone.is_empty() {
        return format!(
            "<div id=\"dg-op\" class=\"grid gap-1\"><p role=\"status\" class=\"nd-aux\">{}</p></div>",
            crate::agent::esc(text)
        );
    }
    let sym = match tone {
        "warn" => "▲",
        "bad" => "■",
        _ => "●",
    };
    let role = if tone == "ok" { "status" } else { "alert" };
    format!(
        "<div id=\"dg-op\" class=\"grid gap-1\"><p role=\"{role}\"><span class=\"nd-mark nd-mark--{tone}\"><span class=\"nd-mark__sym\" aria-hidden=\"true\">{sym}</span><span>{}</span></span></p></div>",
        crate::agent::esc(text)
    )
}

/// The agent's refusal in this language (`error` / `error_en`), or why there was no answer.
fn refusal(r: &Result<(u16, Value), Fail>, l: Lang) -> Option<String> {
    match r {
        Ok((c, v)) if (200..300).contains(c) && v["ok"] != false => None,
        Ok((c, v)) => Some({
            let t = pick(v, "error", l);
            if t.is_empty() { format!("HTTP {c}") } else { t }
        }),
        Err(e) => Some(e.text(l)),
    }
}

#[derive(Deserialize)]
pub struct StartQuery {
    #[serde(default)]
    auto: bool,
}

fn one(html: String) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    Sse::new(async_stream(move |tx| async move {
        let _ = tx.send(patch(&html)).await;
    }))
}

pub async fn start(
    State(app): State<Shared>,
    Query(q): Query<StartQuery>,
    req: axum::extract::Request,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let l = Lang::from_headers(req.headers());
    let who = Caller::of(&app, &req);
    let cur = crate::agent::get(&app, &who, "/api/diagnose").await.ok();
    let mut run: Option<Value> = None;
    let html = if cur
        .as_ref()
        .is_some_and(|d| is_run(d) && (q.auto || d["state"] != "done"))
    {
        // pressing again while it runs: say so, don't start a second one; auto: there is a result
        if q.auto {
            note("", "")
        } else {
            note("", l.pick("Checking already; one moment", "正在查，稍等"))
        }
    } else {
        let r = crate::agent::post_raw(&app, &who, "/api/diagnose", &json!({})).await;
        let html = match refusal(&r, l) {
            Some(e) if matches!(r, Err(Fail::Unreachable) | Ok((500.., _))) => {
                let _ = e;
                note("bad", l.pick("Agent not responding", "后台没回应"))
            }
            Some(e) => note("bad", &e),
            None if r.as_ref().is_ok_and(|(_, v)| v["joined"] == true) => {
                note("", l.pick("Checking already; one moment", "正在查，稍等"))
            }
            None => note("", ""),
        };
        // the new run as the agent answered it: shown now, whether or not this tab's stream is
        // connected yet (an auto start can come before it)
        run = r.ok().map(|(_, v)| v["data"].clone()).filter(is_run);
        html
    };
    let _ = app.kick.send(PATH);
    let mut out = vec![html];
    if let Some(d) = run {
        let g = Got {
            diag: Some(d),
            ..Default::default()
        };
        let v = view(&g, l);
        let leaves = split_leaves(&Leaves { v: &v }.render().unwrap());
        out.extend(leaves.into_iter().map(|(_, h)| h));
    }
    one(out.join("\n"))
}

async fn current_id(app: &Shared, who: &Caller) -> Option<i64> {
    crate::agent::get(app, who, "/api/diagnose")
        .await
        .ok()
        .filter(is_run)
        .and_then(|d| d["id"].as_i64())
}

pub async fn speed(
    State(app): State<Shared>,
    req: axum::extract::Request,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let l = Lang::from_headers(req.headers());
    let who = Caller::of(&app, &req);
    let html = match current_id(&app, &who).await {
        None => note(
            "warn",
            l.pick("No finished check to add to", "没有可以加测的结果"),
        ),
        Some(id) => {
            let r =
                crate::agent::post_raw(&app, &who, "/api/diagnose/speed", &json!({"id": id})).await;
            match refusal(&r, l) {
                Some(e) => note("warn", &e),
                None => note("", ""),
            }
        }
    };
    let _ = app.kick.send(PATH);
    one(html)
}

#[derive(Deserialize)]
pub struct FeedbackQuery {
    right: bool,
}

pub async fn feedback(
    State(app): State<Shared>,
    Query(q): Query<FeedbackQuery>,
    req: axum::extract::Request,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let l = Lang::from_headers(req.headers());
    let who = Caller::of(&app, &req);
    let html = match current_id(&app, &who).await {
        None => note(
            "bad",
            l.pick("No finished check to add to", "没有可以加测的结果"),
        ),
        Some(id) => {
            let r = crate::agent::post_raw(
                &app,
                &who,
                "/api/diagnose/feedback",
                &json!({"id": id, "right": q.right}),
            )
            .await;
            match refusal(&r, l) {
                Some(e) => note("bad", &e),
                None => note("", ""),
            }
        }
    };
    let _ = app.kick.send(PATH);
    one(html)
}

#[derive(Deserialize)]
pub struct DismissQuery {
    kind: String,
    #[serde(default)]
    tail: String,
}

/// 「忽略」 on the open finding (T6): the core keeps the same kind + card closed for 24 h; the
/// readback is the finding gone from the next `GET /api/diagnose`.
pub async fn dismiss(
    State(app): State<Shared>,
    Query(q): Query<DismissQuery>,
    req: axum::extract::Request,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let l = Lang::from_headers(req.headers());
    let who = Caller::of(&app, &req);
    let mut body = json!({"kind": q.kind});
    if !q.tail.is_empty() {
        body["iccid_tail"] = q.tail.into();
    }
    let r = crate::agent::post_raw(&app, &who, "/api/diagnose/dismiss", &body).await;
    let html = match r {
        // already closed (resolved meanwhile, or another tab): the readback says so
        Ok((404, _)) => note("", ""),
        _ => match refusal(&r, l) {
            Some(e) => note("bad", &e),
            None => note("", ""),
        },
    };
    let _ = app.kick.send(PATH);
    one(html)
}

#[derive(Deserialize)]
pub struct ApproveQuery {
    id: String,
    digest: String,
}

/// The core waits for the admission gate and reads the mode before it answers.
const APPROVE_TIMEOUT: Duration = Duration::from_secs(60);

/// 「重拨」 on the proposal (decision loop §4.10): the person approves the one shown; the core
/// refuses if another one is pending by now. The readback is the transaction on /changes and the
/// finding on this page.
pub async fn approve(
    State(app): State<Shared>,
    Query(q): Query<ApproveQuery>,
    req: axum::extract::Request,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let l = Lang::from_headers(req.headers());
    let body = json!({
        "action": "loop.approve",
        "params": {"proposal_id": q.id, "params_digest": q.digest},
        "source": "web",
        "op_id": crate::write::next_op_id(),
    });
    let r = app
        .http
        .post(&app.control_url)
        .json(&crate::pin::stamp(&body))
        .timeout(APPROVE_TIMEOUT)
        .send()
        .await;
    let reply = match r {
        Err(e) if e.is_connect() => Err(true),
        Err(_) => Err(false),
        Ok(r) => r.json::<Value>().await.map_err(|_| false),
    };
    let html = match reply {
        Ok(v) if v["ok"] == true => note(
            "",
            l.pick(
                "Redialling; the data connection drops for about 15 s",
                "正在重拨，数据连接会断开约 15 秒",
            ),
        ),
        Ok(v) => {
            let (en, zh) = match v["error"]["code"].as_str().unwrap_or("") {
                "proposal_changed" => (
                    "The suggestion changed meanwhile; look again",
                    "建议已经变了，请重新看一下",
                ),
                "already_used" => (
                    "Used already; a new suggestion comes with a new check",
                    "这条建议已经用过；新的检查会给新建议",
                ),
                "cooldown" => (
                    "Tried recently; not again yet",
                    "刚试过，暂时不再试",
                ),
                "busy" | "op_busy" => (
                    "Another change is running; try again later",
                    "另一项改动正在进行，稍后再试",
                ),
                _ => ("", ""),
            };
            if en.is_empty() {
                let m = v["error"]["message"].as_str().unwrap_or("");
                note(
                    "bad",
                    &format!("{}{m}", l.pick("Not run: ", "没有执行：")),
                )
            } else {
                note("warn", l.pick(en, zh))
            }
        }
        Err(true) => note("bad", l.pick("can't reach the core service", "连不上核心服务")),
        Err(false) => note(
            "warn",
            l.pick(
                "No answer from the core; see Changes for whether it ran",
                "核心没有回答；是否执行了请看「变更」",
            ),
        ),
    };
    let _ = app.kick.send(PATH);
    one(html)
}

/// 「恢复上次能用的设置」: the core checks again and runs it step by step; progress comes back
/// through the page's own stream.
pub async fn restore(
    State(app): State<Shared>,
    req: axum::extract::Request,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    let body = json!({
        "action": "verified.restore",
        "params": {},
        "source": "web",
        "op_id": crate::write::next_op_id(),
    });
    let r = app
        .http
        .post(&app.control_url)
        .json(&crate::pin::stamp(&body))
        .timeout(APPROVE_TIMEOUT)
        .send()
        .await;
    let reply = match r {
        Err(e) if e.is_connect() => Err(true),
        Err(_) => Err(false),
        Ok(r) => r.json::<Value>().await.map_err(|_| false),
    };
    let html = match reply {
        Ok(v) if v["ok"] == true => note("", l.pick("Restoring step by step…", "正在逐项恢复…")),
        Ok(v) => note(
            "warn",
            match v["error"].as_str().unwrap_or("") {
                "already_there" => l.pick(
                    "Already the last set that worked",
                    "现在就是上次能用的那一套",
                ),
                "txn_active" | "busy" => l.pick(
                    "Another change is running; try again later",
                    "另一项改动正在进行，稍后再试",
                ),
                "no_record" => l.pick(
                    "Nothing recorded for this card here",
                    "这张卡在这里没有记录",
                ),
                "no_country" => l.pick(
                    "The current country can't be read",
                    "读不到现在在哪个国家",
                ),
                "session_active" => l.pick(
                    "A guard session holds the network now; try again later",
                    "现在有守护会话占着网络，稍后再试",
                ),
                _ => l.pick("Not run", "没有执行"),
            },
        ),
        Err(true) => note("bad", l.pick("can't reach the core service", "连不上核心服务")),
        Err(false) => note(
            "warn",
            l.pick(
                "No answer from the core; see Changes for whether it ran",
                "核心没有回答；是否执行了请看「变更」",
            ),
        ),
    };
    let _ = app.kick.send(PATH);
    // the dialog closes with the answer
    one(restore::area(l, ctx.remote, &crate::write::Phase::Idle) + &html)
}

/// The restore's dialog (tier 3, with the remote warning).
pub async fn restore_ask(
    req: axum::extract::Request,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let c = restore::confirm(ctx.lang);
    one(restore::area(ctx.lang, ctx.remote, &crate::write::Phase::Ask(c)))
}

/// Cancel / Esc (and a lost reply: the progress shows on the page anyway).
pub async fn restore_cancel(
    req: axum::extract::Request,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    one(restore::area(ctx.lang, ctx.remote, &crate::write::Phase::Idle))
}

/// Retry while the agent is not responding: read again now.
pub async fn refresh(State(app): State<Shared>) -> axum::http::StatusCode {
    let _ = app.kick.send(PATH);
    axum::http::StatusCode::NO_CONTENT
}

#[cfg(test)]
mod tests {
    use super::*;

    fn done_run() -> Value {
        json!({"id":3,"state":"done","finished_at": 1_791_800_000u64 / 86400 * 86400 + 14 * 3600 + 5 * 60,
            "layers":[
                {"id":"link","level":"warn","detail":"丢包 3%","detail_en":"3% loss","counted":true},
                {"id":"wifi","level":"ok","detail":"-45 dBm","detail_en":"-45 dBm","counted":true},
                {"id":"proxy","level":"bad","detail":"超时","detail_en":"timeout","counted":true},
                {"id":"crowd","level":"na","detail":"无数据","detail_en":"no data","counted":false},
                {"id":"clock","level":"blocked","detail":"前面的外网不通","detail_en":"Internet is down","counted":false}],
            "main":{"layer":"proxy","level":"bad","text":"代理节点慢或不通","text_en":"Proxy node slow or down",
                    "action":"换节点","action_en":"Switch node","action_to":"proxy","more":1},
            "feedback":null,
            "speed":{"id":"speed","level":"info","detail":"直连 ↓ 86 Mbps","detail_en":"Direct ↓ 86 Mbps","counted":true}})
    }

    #[test]
    fn a_finished_run_reads_like_the_old_page() {
        let g = Got {
            diag: Some(done_run()),
            roaming: true,
            ..Default::default()
        };
        let v = view(&g, Lang::Zh);
        assert_eq!((v.tone, v.word.as_str()), ("bad", "代理节点慢或不通"));
        assert_eq!(v.reason.as_deref(), Some("换节点 · 另有 1 处疑点"));
        assert!(v.proxy);
        assert_eq!(v.at.as_deref(), Some("14:05 测"));
        let names: Vec<&str> = v.rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            ["Wi-Fi", "蜂窝链路", "设备时间", "基站负载", "代理", "速度"]
        );
        assert_eq!(v.rows[3].word, "测不了 · 无数据");
        assert!(v.rows[3].muted);
        // blocked: grey, says why
        assert_eq!(
            (v.rows[2].word.as_str(), v.rows[2].tone, v.rows[2].muted),
            ("没测", Some("neutral"), true)
        );
        assert_eq!(v.rows[2].detail, "前面的外网不通");
        assert_eq!(v.rows[5].word, "直连 ↓ 86 Mbps");
        let n = v.next.unwrap();
        assert!(n.roaming && !n.answered);
    }

    #[test]
    fn running_counts_steps_and_agent_down_keeps_the_rows() {
        let mut d = done_run();
        d["state"] = json!("running");
        d["step"] = json!(2);
        d["steps"] = json!(5);
        let mut g = Got {
            diag: Some(d),
            ..Default::default()
        };
        let v = view(&g, Lang::En);
        assert_eq!(v.word, "Checking… 2/5");
        assert_eq!(v.pct(), 40);
        assert!(v.next.is_none());
        g.err = Some(Fail::Unreachable);
        let v = view(&g, Lang::En);
        assert_eq!(v.word, "Agent not responding");
        assert!(matches!(v.action, Action::Retry) && v.down && !v.rows.is_empty());
    }

    #[test]
    fn only_a_complete_redial_proposal_gets_a_button() {
        let p = json!({"proposal_id":"ab12","params_digest":"d0","action":"data.reconnect","iccid_tail":"1234"});
        let v = proposal_view(&p).unwrap();
        assert_eq!((v.id.as_str(), v.digest.as_str(), v.tail.as_str()), ("ab12", "d0", "1234"));
        let mut other = p.clone();
        other["action"] = json!("network.mode");
        assert!(proposal_view(&other).is_none());
        let mut no_digest = p.clone();
        no_digest["params_digest"] = json!("");
        assert!(proposal_view(&no_digest).is_none());
        assert!(proposal_view(&Value::Null).is_none());
    }

    #[test]
    fn band_flap_shows_only_an_explanation() {
        let b = json!({"advice":{"advice":"explain","band":"n78","drops":4},"drops":4,"covered_s":2000,"window_s":1800});
        let en = flap_view(&b, Lang::En).unwrap();
        assert!(en.contains("4 times") && en.contains("n78") && en.contains("30 minutes"), "{en}");
        assert!(flap_view(&b, Lang::Zh).unwrap().contains("断了 4 次"));
        for a in [json!({"advice":"unknown","why":"window_incomplete"}), json!({"advice":"none","why":"too_few_drops"})] {
            assert!(flap_view(&json!({"advice": a}), Lang::En).is_none());
        }
        let odd = json!({"advice":{"advice":"explain","band":"<b>","drops":3}});
        assert_eq!(flap_view(&odd, Lang::En).map(|s| s.contains('<')), Some(false));
        assert!(flap_view(&Value::Null, Lang::En).is_none());
    }

    #[test]
    fn hot_drain_explains_or_says_the_drain_is_unread() {
        let b = json!({"advice":{"advice":"explain","batt_dc":412,"batt_normal_dc":360,"drain_mw":900,"drain_normal_mw":400,
            "alongside":["cpu","clients"],"unread":["cellular"],"ambient":"unread"},
            "window":{"cell":"off_home","window_s":3600},"batt_dc":412,"batt_normal_dc":360});
        let en = hot_view(&b, Lang::En).unwrap();
        assert!(
            en.contains("41.2 °C") && en.contains("36.0 °C") && en.contains("900 mW"),
            "{en}"
        );
        assert!(
            en.contains("the CPU busier than usual, devices connected")
                && en.contains("Not compared: more cellular"),
            "{en}"
        );
        assert!(
            en.contains("Room temperature") && en.contains("not necessarily the cause"),
            "{en}"
        );
        let zh = hot_view(&b, Lang::Zh).unwrap();
        assert!(
            zh.contains("息屏、本地网络") && zh.contains("不会自动改"),
            "{zh}"
        );
        let u = json!({"advice":{"advice":"unknown","why":"drain_unqualified"},"window":{"cell":"off_home"},
            "batt_dc":420,"batt_normal_dc":390});
        assert!(hot_view(&u, Lang::En).unwrap().contains("can't be told"));
        for a in [
            json!({"advice":"unknown","why":"window_stale"}),
            json!({"advice":"none","why":"not_hot"}),
            json!({"advice":"none","why":"charging"}),
        ] {
            assert!(hot_view(&json!({"advice": a, "window":{"cell":"off_home"},"batt_dc":420,"batt_normal_dc":390}), Lang::En).is_none());
        }
        assert!(hot_view(&Value::Null, Lang::En).is_none());
        let odd = json!({"advice":{"advice":"explain","batt_dc":400,"batt_normal_dc":360,"drain_mw":900,"drain_normal_mw":400,
            "alongside":["<b>"],"unread":[]},"window":{"cell":"<b>"}});
        assert!(hot_view(&odd, Lang::En).is_none());
    }

    #[test]
    fn long_idle_shows_only_a_suggestion() {
        let b = json!({"advice":{"advice":"suggest","hours":5,"step":"chill_idle_pause"},"quiet_run":5});
        let en = idle_view(&b, Lang::En).unwrap();
        assert!(
            en.contains("last 5 hours") && en.contains("LUYAZ_CHILL_IDLE=1"),
            "{en}"
        );
        assert!(
            idle_view(&b, Lang::Zh)
                .unwrap()
                .contains("最近 5 小时没人用")
        );
        for a in [
            json!({"advice":"none","why":"already_saving"}),
            json!({"advice":"unknown","why":"coverage_gap"}),
            json!({"advice":"suggest","hours":5,"step":"other"}),
        ] {
            assert!(idle_view(&json!({ "advice": a }), Lang::En).is_none());
        }
        assert!(idle_view(&Value::Null, Lang::En).is_none());
    }

    #[test]
    fn slow_signal_names_proven_and_possible_layers() {
        let b =
            json!({"advice":{"advice":"explain","proven":["wifi","proxy"],"possible":["crowd"]}});
        let en = slow_view(&b, Lang::En).unwrap();
        assert!(
            en.contains("Found by the check: Wi-Fi, Proxy")
                && en.contains("Only possible: Cell load"),
            "{en}"
        );
        let zh = slow_view(&b, Lang::Zh).unwrap();
        assert!(
            zh.contains("查实的：Wi-Fi、代理") && zh.contains("只是可能：基站负载"),
            "{zh}"
        );
        let only = json!({"advice":{"advice":"explain","proven":[],"possible":["crowd"]}});
        assert!(!slow_view(&only, Lang::En).unwrap().contains("Found by"));
        for a in [
            json!({"advice":"unknown","why":"no_check"}),
            json!({"advice":"none","why":"signal_not_strong"}),
            json!({"advice":"explain","proven":["<b>"],"possible":[]}),
        ] {
            assert!(slow_view(&json!({ "advice": a }), Lang::En).is_none());
        }
        assert!(slow_view(&Value::Null, Lang::En).is_none());
    }

    #[test]
    fn slow_busy_suggests_a_check() {
        let b = json!({"advice": {"advice": "suggest", "busy_s": 150, "step": "deep_check"}});
        let en = slow_view(&b, Lang::En).unwrap();
        assert!(en.contains("for 2 min") && en.is_ascii(), "{en}");
        assert!(slow_view(&b, Lang::Zh).unwrap().contains("2 分钟"));
        let bad = json!({"advice": {"advice": "suggest", "busy_s": "x"}});
        assert!(slow_view(&bad, Lang::En).is_none());
    }

    #[test]
    fn idle_offers_start_and_english_has_no_chinese() {
        let g = Got {
            diag: Some(json!({"state":"idle"})),
            ..Default::default()
        };
        let v = view(&g, Lang::En);
        assert!(matches!(v.action, Action::Start));
        let mut html = Leaves { v: &v }.render().unwrap();
        let g = Got {
            diag: Some(done_run()),
            roaming: true,
            ..Default::default()
        };
        html += &Leaves {
            v: &view(&g, Lang::En),
        }
        .render()
        .unwrap();
        assert!(
            !html.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)
                || ('\u{3000}'..='\u{303f}').contains(&c)
                || ('\u{ff00}'..='\u{ffef}').contains(&c)),
            "{html}"
        );
    }
}
