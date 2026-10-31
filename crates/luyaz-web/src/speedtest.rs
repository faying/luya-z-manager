//! /tools/speedtest (测速): status (phase · reason, start /
//! stop) → readings (download as hero, upload, ping, jitter) → server choice → details.
//!
//! ```text
//!  stream: GET /api/speedtest/progress every 1 s while a test runs (or was just started), else
//!          every 10 s (another tab or the touch screen may start one); cached servers periodically
//!  Start = tier 2: inline confirm (states the data cost) ─▶ POST /tools/speedtest/start
//!          (signals: $server) ─▶ agent /api/speedtest/start; 409 = already running, shown as is
//!  Stop  = tier 2: inline confirm ─▶ POST /tools/speedtest/stop ─▶ agent stop ─▶ readback: progress
//!          leaves the running phases within 15 s
//! ```
//! Confirms are Datastar signals ($ask, $askStop) inside the pushed status leaf, so a push keeps
//! them open; `$running` (pushed as a `data-signals` leaf) disables the server choice.

use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use askama::Template;
use axum::extract::{Query, State};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::Stream;
use serde_json::{json, Value};

use crate::agent::{stopped, Caller, Fail, Polled};
use crate::i18n::Lang;
use crate::pages::{async_stream, patch, split_leaves, Shared, StreamQuery};
use crate::view::Readout;

const PATH: &str = "/tools/speedtest";
/// After a start the agent says "idle" until its worker begins: poll fast this long anyway.
const EXPECT: Duration = Duration::from_secs(15);
const STOP_WAIT: Duration = Duration::from_secs(15);

/// When a start from this server last succeeded (the test is device-wide, so is this).
static STARTED: Mutex<Option<Instant>> = Mutex::new(None);

fn expecting() -> bool {
    STARTED
        .lock()
        .unwrap()
        .is_some_and(|t| t.elapsed() < EXPECT)
}

fn active(phase: &str) -> bool {
    matches!(phase, "discovering" | "latency" | "download" | "upload")
}

fn done(phase: &str) -> bool {
    matches!(phase, "complete" | "cancelled" | "error")
}

fn f1(v: &Value) -> Option<String> {
    v.as_f64()
        .filter(|x| x.is_finite())
        .map(|x| format!("{x:.1}"))
}

fn mb(bytes: f64) -> String {
    if bytes >= 1e8 {
        format!("{:.0}", bytes / 1e6)
    } else {
        format!("{:.1}", bytes / 1e6)
    }
}

fn phase_word(p: &str, l: Lang) -> &'static str {
    match p {
        "discovering" => l.pick("Finding a test server…", "正在寻找测速节点…"),
        "latency" => l.pick("Latency…", "测延迟…"),
        "download" => l.pick("Download…", "测下载…"),
        "upload" => l.pick("Upload…", "测上传…"),
        "complete" => l.pick("Complete", "完成"),
        "cancelled" => l.pick("Cancelled", "已取消"),
        "error" => l.pick("Error", "错误"),
        _ => l.pick("Idle", "空闲"),
    }
}

/// v2's sub-stage word (DR7)
fn stage_word(s: &str, l: Lang) -> Option<&'static str> {
    Some(match s {
        "idle_latency" => l.pick("Idle latency", "空闲延迟"),
        "single_download" => l.pick("Single-connection download", "单连接下载"),
        "multi_download" => l.pick("Multi-connection download", "多连接下载"),
        "chill_download" => l.pick("Download via CHILL", "经 CHILL 下载"),
        "multi_upload" => l.pick("Multi-connection upload", "多连接上传"),
        _ => return None,
    })
}

/// How the running stage measures (DR7): the conclusion line's place while it runs.
fn stage_method(p: &Value, l: Lang) -> Option<String> {
    let n = p["connections"].as_u64().unwrap_or(6);
    let skip = if p["mode"] == "coarse" { "0.5" } else { "2" };
    Some(match p["stage"].as_str()? {
        "single_download" => match l {
            Lang::En => format!("1 connection · first {skip} s not counted"),
            Lang::Zh => format!("1 条连接 · 前 {skip} 秒不计"),
        },
        "multi_download" | "chill_download" => match l {
            Lang::En => format!("{n} connections · first {skip} s not counted"),
            Lang::Zh => format!("{n} 条连接 · 前 {skip} 秒不计"),
        },
        "multi_upload" => match l {
            Lang::En => format!("{n}-connection upload"),
            Lang::Zh => format!("{n} 条连接上传"),
        },
        _ => return None,
    })
}

fn layer_name(key: &str, l: Lang) -> &'static str {
    match key {
        "single" => l.pick("Single connection", "单连接"),
        "proxy" => l.pick("Proxy", "代理"),
        "queue" => l.pick("Lag under load", "下载时卡"),
        "radio" => l.pick("Radio", "无线"),
        _ => l.pick("Cell, Wi-Fi", "基站、Wi-Fi"),
    }
}

fn v2_of(p: &Value, phase: &str, have: bool, loading: bool, l: Lang) -> V2 {
    let complete = phase == "complete";
    let short = complete && p["sample_short"] == true;
    let coarse = complete && p["mode"] == "coarse";
    let pick = |zh: &str, en: &str| -> String {
        if l == Lang::En { en } else { zh }.to_string()
    };
    let verdict_obj = p["verdict"].as_object().filter(|_| complete && !short && !coarse);
    let verdict = if short {
        Some((
            "warn",
            pick("样本不足，数字只供参考", "Too few samples; rough figure"),
        ))
    } else if coarse {
        Some((
            "neutral",
            if p["roaming_capped"] == true {
                pick("粗测 · 漫游中只测 5 秒", "Rough test · 5 s only while roaming")
            } else {
                pick("粗测 · 只测 5 秒", "Rough test · 5 s only")
            },
        ))
    } else {
        verdict_obj.map(|v| {
            let hit = v
                .get("bottlenecks")
                .and_then(Value::as_array)
                .is_some_and(|b| !b.is_empty());
            let t = v
                .get(if l == Lang::En { "summary_en" } else { "summary_zh" })
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            (if hit { "warn" } else { "ok" }, t)
        })
    };
    let layers = verdict_obj
        .and_then(|v| v.get("layers"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|r| {
                    let key = r["key"].as_str().unwrap_or("");
                    let state = match r["state"].as_str() {
                        Some("warn") => "warn",
                        Some("ok") => "ok",
                        _ => "untested",
                    };
                    LayerRow {
                        name: layer_name(key, l),
                        state,
                        value: r[if l == Lang::En { "value_en" } else { "value_zh" }]
                            .as_str()
                            .unwrap_or("")
                            .to_string(),
                        // DR9: the proxy row (when slow) leads to CHILL's nodes; the web has no
                        // placement page, so the radio row stays plain
                        href: (key == "proxy" && state == "warn").then_some("/services/chill"),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let mut rows = Vec::new();
    if have {
        let whole: Vec<String> = [("↓", "download_whole_mbps"), ("↑", "upload_whole_mbps")]
            .iter()
            .filter_map(|(a, k)| f1(&p[*k]).map(|v| format!("{a} {v} Mbps")))
            .collect();
        if !whole.is_empty() {
            rows.push((pick("全程均值", "Whole-run average"), whole.join(" · ")));
        }
        if let Some(j) = f1(&p["jitter_ms"]) {
            rows.push((pick("抖动", "Jitter"), format!("{j} ms")));
        }
        if let Some(n) = p["connections"].as_u64() {
            rows.push((pick("连接数", "Connections"), n.to_string()));
        }
        // R11: roaming, run full after the two-step confirm
        if p["roaming_full"] == true {
            rows.push((pick("漫游", "Roaming"), pick("漫游中完整测速", "Full test while roaming")));
        }
        match p["net_tune"].as_str() {
            Some("on") => rows.push((pick("内核调参", "Kernel tuning"), pick("开", "On"))),
            Some("off") => rows.push((pick("内核调参", "Kernel tuning"), pick("关", "Off"))),
            _ => {}
        }
    }
    let loaded = if loading {
        None
    } else if have {
        Some(
            p["loaded_down_ms"]
                .as_f64()
                .filter(|x| x.is_finite())
                .map(|x| format!("{x:.1}")),
        )
    } else {
        Some(None)
    };
    V2 {
        verdict,
        dim: short,
        loaded: ro(loaded, ""),
        layers,
        rows,
    }
}

pub struct View {
    pub l: Lang,
    pub tone: &'static str,
    /// the announced state (DR11: step changes and the conclusion only)
    pub word: String,
    /// " · 62%": shown beside the state, never announced
    pub tail: Option<String>,
    pub reason: Option<String>,
    pub running: bool,
    pub pct: i64,
    pub fresh: Option<String>,
    pub retry: bool,
    pub can_start: bool,
    pub stale: bool,
    pub dl: Readout,
    pub ul: Readout,
    pub ping: Readout,
    pub jitter: Readout,
    /// (server, total MB, "download d MB · upload u MB")
    pub details: Option<(String, String, String)>,
    /// the v2 engine (`method_version` present); None = an old core, shown as before (DR4)
    pub v2: Option<V2>,
    /// `/api/netinfo?lite=1`: (roaming, CHILL running), once read
    pub net: Option<(bool, bool)>,
    /// the last run was capped because the device was roaming (R9)
    pub capped_last: bool,
}

/// What the v2 result adds (net-performance.md DR3, DR5–DR8, DR11–DR14).
pub struct V2 {
    /// the conclusion beside the state word: (tone, text); only once the run ended complete
    pub verdict: Option<(&'static str, String)>,
    /// the hero number is only for reference (DR5)
    pub dim: bool,
    /// the fourth readout: latency while downloading (replaces jitter)
    pub loaded: Readout,
    pub layers: Vec<LayerRow>,
    /// extra detail rows (label, value)
    pub rows: Vec<(String, String)>,
}

pub struct LayerRow {
    pub name: &'static str,
    /// "ok" | "warn" | "untested"
    pub state: &'static str,
    pub value: String,
    pub href: Option<&'static str>,
}

impl LayerRow {
    pub fn symbol(&self) -> &'static str {
        if self.state == "warn" {
            "▲"
        } else {
            "●"
        }
    }
    pub fn tone(&self) -> &'static str {
        match self.state {
            "warn" => "warn",
            "ok" => "ok",
            _ => "neutral",
        }
    }
    pub fn said(&self, l: &Lang) -> &'static str {
        match self.state {
            "warn" => l.pick("attention", "注意"),
            "ok" => l.pick("normal", "正常"),
            _ => l.pick("not tested", "没测"),
        }
    }
}

impl View {
    /// The data-cost sentence of 「开始」 (DR6, DR13): "old" (an old core's 60 s / 512 MB),
    /// "roaming" (the core will run coarse) or "full" (30 s / 750 MB, 40 s / 1.15 GB via CHILL).
    pub fn cost(&self) -> &'static str {
        match (&self.v2, self.net) {
            (None, _) => "old",
            (Some(_), Some((true, _))) => "roaming",
            _ => "full",
        }
    }
    /// 「仍要完整测速」 in the start confirm (R11): a v2 core that will cap the run because the
    /// device is roaming (or, roaming not read yet, just did); its own second confirm says what
    /// it costs.
    pub fn roam_full(&self) -> bool {
        self.v2.is_some()
            && !self.running
            && match self.net {
                Some((roaming, _)) => roaming,
                // not read yet: the last run says
                None => self.capped_last,
            }
    }
    /// Why 「也测经 CHILL」 can't be turned on now (DR12); None = it can.
    pub fn chill_off(&self) -> Option<&'static str> {
        match self.net {
            Some((true, _)) => Some(self.l.pick("Rough test only while roaming", "漫游中只粗测")),
            Some((false, false)) => Some(self.l.pick("CHILL not running", "CHILL 没在运行")),
            _ => None,
        }
    }
    pub fn symbol(&self) -> &'static str {
        match self.tone {
            "warn" => "▲",
            "bad" => "■",
            _ => "●",
        }
    }
}

fn ro(v: Option<Option<String>>, word: &str) -> Readout {
    match v {
        None => Readout {
            value: None,
            empty: false,
            word: String::new(),
        },
        Some(None) => Readout {
            value: Some("—".into()),
            empty: true,
            word: word.into(),
        },
        Some(Some(s)) => Readout {
            value: Some(s),
            empty: false,
            word: word.into(),
        },
    }
}

#[derive(Default)]
struct Got {
    prog: Option<Value>,
    /// (roaming, CHILL running) from `/api/netinfo?lite=1`
    net: Option<(bool, bool)>,
    err: Option<Fail>,
    last_ok: Option<SystemTime>,
}

fn view(g: &Got, expect: bool, l: Lang) -> View {
    let p = g.prog.as_ref();
    let phase = p.and_then(|p| p["phase"].as_str()).unwrap_or("");
    let running = (expect && !done(phase)) || active(phase);
    let stale = g.err.is_some() && p.is_some();
    let pct = p
        .and_then(|p| p["progress"].as_f64())
        .map_or(0, |x| x.round().clamp(0.0, 100.0) as i64);
    let live = p
        .filter(|_| active(phase))
        .and_then(|p| f1(&p["live_speed_mbps"]));
    let is_v2 = p.is_some_and(|p| p["method_version"].as_u64().is_some_and(|v| v >= 2));
    let (tone, word, reason) = match p {
        None if g.err.is_some() => (
            "bad",
            l.pick("Can't read the speed test", "读不到测速状态")
                .to_string(),
            g.err.as_ref().map(|e| e.text(l)),
        ),
        None => ("neutral", l.pick("Reading…", "正在读取…").to_string(), None),
        Some(p) if running => (
            if stale { "stale" } else { "neutral" },
            match (is_v2, p["step"].as_u64(), p["steps"].as_u64(), p["stage"].as_str().and_then(|s| stage_word(s, l))) {
                (true, Some(n), Some(m), Some(w)) if n > 0 && active(phase) => format!("{n}/{m} · {w}"),
                _ if active(phase) => phase_word(phase, l).to_string(),
                _ => l.pick("Starting…", "正在开始…").to_string(),
            },
            if is_v2 && active(phase) {
                stage_method(p, l)
            } else {
                live.clone().map(|v| match l {
                    Lang::En => format!("Live: {v} Mbps"),
                    Lang::Zh => format!("实时：{v} Mbps"),
                })
            },
        ),
        Some(p) if phase == "complete" => (
            "ok",
            phase_word(phase, l).into(),
            p["server"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(|s| match l {
                    Lang::En => format!("Last test, on {s}"),
                    Lang::Zh => format!("上次测速，服务器：{s}"),
                }),
        ),
        Some(p) if phase == "error" => {
            let e = p[if l == Lang::En && p["error_en"].is_string() {
                "error_en"
            } else {
                "error"
            }]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(l.pick("The device gave no reason", "设备没有给出原因"));
            (
                "bad",
                l.pick("Speed test failed", "测速没成功").into(),
                Some(if p["error_code"].is_string() {
                    e.to_string()
                } else {
                    match l {
                        Lang::En => format!("{e} · Retry or select another speed-test provider."),
                        Lang::Zh => format!("{e} · 请重试或切换测速来源。"),
                    }
                }),
            )
        }
        Some(_) if phase == "cancelled" => (
            "neutral",
            phase_word(phase, l).into(),
            Some(
                l.pick(
                    "The test was stopped before it finished.",
                    "测速在完成前被停止了。",
                )
                .into(),
            ),
        ),
        Some(_) => (
            "neutral",
            l.pick("No result yet", "还没有结果").into(),
            Some(
                l.pick(
                    "Pick a server below and press “Start”.",
                    "在下面选好服务器，按「开始」。",
                )
                .into(),
            ),
        ),
    };
    let have = p.is_some() && phase != "idle";
    let loading = p.is_none() && g.err.is_none();
    let v2 = p
        .filter(|_| is_v2)
        .map(|p| v2_of(p, phase, have, loading, l));
    let live_word = l.pick("Live", "实时");
    let val = |key: &str, live_here: bool| -> Option<Option<String>> {
        if loading {
            None
        } else if live_here {
            Some(live.clone())
        } else if have {
            Some(p.and_then(|p| f1(&p[key])))
        } else {
            Some(None)
        }
    };
    let details = p.filter(|_| have).map(|p| {
        let d = p["download_bytes"].as_f64().unwrap_or(0.0);
        let u = p["upload_bytes"].as_f64().unwrap_or(0.0);
        (
            p["server"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or("—")
                .to_string(),
            format!("{} MB", mb(d + u)),
            match l {
                Lang::En => format!("Download {} MB · upload {} MB", mb(d), mb(u)),
                Lang::Zh => format!("下载 {} MB · 上传 {} MB", mb(d), mb(u)),
            },
        )
    });
    View {
        l,
        tone,
        word,
        tail: running.then(|| format!(" · {pct}%")),
        reason,
        running,
        pct,
        fresh: (running && stale).then(|| stopped(l, "Progress", "进度", g.last_ok)),
        retry: p.is_none() && g.err.is_some() && g.err != Some(Fail::SignedOut),
        can_start: p.is_some() || g.err.is_some(),
        stale: running && stale,
        dl: ro(
            val("download_mbps", phase == "download"),
            if phase == "download" { live_word } else { "" },
        ),
        ul: ro(
            val("upload_mbps", phase == "upload"),
            if phase == "upload" { live_word } else { "" },
        ),
        ping: ro(val("ping_ms", false), ""),
        jitter: ro(val("jitter_ms", false), ""),
        details,
        v2,
        net: g.net,
        capped_last: p.is_some_and(|p| p["roaming_capped"] == true),
    }
}

// ---- servers ----

pub struct Servers {
    pub l: Lang,
    /// None = still getting it; Some(Err) = failed.
    pub list: Option<Result<Vec<(String, String)>, String>>,
}

fn servers_of(r: Result<Value, Fail>, l: Lang) -> Result<Vec<(String, String)>, String> {
    match r {
        Ok(v) => Ok(v
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|s| {
                        Some((
                            format!(
                                "{}:{}",
                                s["provider"].as_str().unwrap_or("ookla"),
                                s["id"].as_u64()?
                            ),
                            format!(
                                "{} · {} — {}, {}",
                                s["provider"].as_str().unwrap_or("ookla"),
                                s["sponsor"].as_str().unwrap_or(""),
                                s["name"].as_str().unwrap_or(""),
                                s["country"].as_str().unwrap_or("")
                            ),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default()),
        Err(e) => Err(e.text(l)),
    }
}

#[derive(Template)]
#[template(path = "speedtest_servers.html")]
struct ServersFrag<'a> {
    s: &'a Servers,
}

// ---- the stream ----

#[derive(Template)]
#[template(path = "speedtest_leaves.html")]
struct Leaves<'a> {
    v: &'a View,
}

pub struct Poll {
    who: Caller,
    l: Lang,
    got: Got,
    servers: Option<String>,
    servers_at: Option<Instant>,
    net_at: Option<Instant>,
}

impl Poll {
    fn new(who: Caller, l: Lang) -> Poll {
        Poll {
            who,
            l,
            got: Got::default(),
            servers: None,
            servers_at: None,
            net_at: None,
        }
    }

    async fn fetch(&mut self, app: &Shared) {
        match crate::agent::get(app, &self.who, "/api/speedtest/progress").await {
            Ok(v) => {
                if done(v["phase"].as_str().unwrap_or("")) {
                    *STARTED.lock().unwrap() = None;
                }
                self.got.prog = Some(v);
                self.got.err = None;
                self.got.last_ok = Some(SystemTime::now());
            }
            Err(e) => self.got.err = Some(e),
        }
        // roaming and CHILL decide the cost line and the CHILL switch (same read as the
        // diagnosis page), every 30 s
        if self
            .net_at
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(30))
        {
            if let Ok(n) = crate::agent::get(app, &self.who, "/api/netinfo?lite=1").await {
                self.got.net = Some((
                    n["roaming"].as_bool() == Some(true),
                    n["chill_running"].as_bool() == Some(true),
                ));
                self.net_at = Some(Instant::now());
            }
        }
    }

    fn leaves(&self) -> Vec<(String, String)> {
        let v = view(&self.got, expecting(), self.l);
        let mut out = split_leaves(&Leaves { v: &v }.render().unwrap());
        if let Some(s) = &self.servers {
            out.push(("st-server".into(), s.clone()));
        }
        out
    }
}

impl Polled for Poll {
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        if self
            .servers_at
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(5))
        {
            self.servers_at = Some(Instant::now());
            let who = self.who.clone();
            let (_, s) = tokio::join!(
                self.fetch(app),
                crate::agent::get(app, &who, "/api/speedtest/servers")
            );
            self.servers = Some(
                ServersFrag {
                    s: &Servers {
                        l: self.l,
                        list: Some(servers_of(s, self.l)),
                    },
                }
                .render()
                .unwrap(),
            );
        } else {
            self.fetch(app).await;
        }
        let v = view(&self.got, expecting(), self.l);
        let every = if v.running {
            Duration::from_secs(1)
        } else {
            Duration::from_secs(10)
        };
        (self.leaves(), every)
    }
}

#[derive(Template)]
#[template(path = "speedtest.html")]
struct Page {
    shell: crate::shell::Shell,
    leaves: Vec<(String, String)>,
    servers: String,
    config_signals: String,
    config_error: Option<String>,
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
    let shell = app.hub.read(|st| {
        crate::shell::Shell::new(&headers, PATH).live(
            st,
            &ctx,
            Some(format!("{PATH}/events?g={}", st.generation)),
        )
    });
    let cfg = crate::agent::get(&app, &p.who, "/api/speedtest/config").await;
    let (config_value, config_error) = match cfg {
        Ok(v) => (
            v["config"].clone(),
            v["load_error"].as_str().map(str::to_owned),
        ),
        Err(e) => (Value::Null, Some(e.text(ctx.lang))),
    };
    let html = Page {
        shell,
        config_signals: json!({"libreconfig":serde_json::to_string_pretty(&config_value).unwrap_or_default(),"askconfig":false}).to_string(),
        config_error,
        leaves: p.leaves(),
        servers: ServersFrag {
            s: &Servers {
                l: ctx.lang,
                list: None,
            },
        }
        .render()
        .unwrap(),
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

// ---- writes ----

/// Retry after "can't read": every stream of this page reads now.
pub async fn refresh(State(app): State<Shared>) -> axum::http::StatusCode {
    let _ = app.kick.send(PATH);
    axum::http::StatusCode::NO_CONTENT
}

fn op_html(l: Lang, tone: &str, text: &str) -> String {
    if text.is_empty() {
        return "<div id=\"st-op\" class=\"grid gap-1\"></div>".into();
    }
    let sym = match tone {
        "ok" | "neutral" => "●",
        "warn" => "▲",
        _ => "■",
    };
    let role = if tone == "ok" || tone == "neutral" {
        "status"
    } else {
        "alert"
    };
    let _ = l;
    format!(
        "<div id=\"st-op\" class=\"grid gap-1\"><p role=\"{role}\"><span class=\"nd-mark nd-mark--{tone}\"><span class=\"nd-mark__sym\" aria-hidden=\"true\">{sym}</span><span>{}</span></span></p></div>",
        crate::agent::esc(text)
    )
}

fn sending(l: Lang) -> String {
    format!(
        "<div id=\"st-op\" class=\"grid gap-1\"><p class=\"nd-aux\" role=\"status\">{}</p></div>",
        l.pick("Sending…", "提交中…")
    )
}

fn not_applied(l: Lang, e: &str) -> String {
    match l {
        Lang::En => format!("Not applied: {e}"),
        Lang::Zh => format!("没有生效：{e}"),
    }
}

/// The page's choice plus `path:"chill"` only when 「也测经 CHILL」 is on (an old core then sees
/// the same body as before).
fn start_request(sig: &Value) -> Value {
    let mut v = server_request(sig);
    if sig["chill"] == true {
        v["path"] = json!("chill");
    }
    // R11: only from the second step of 「仍要完整测速」
    if sig["roamFull"] == true {
        v["roaming_ok"] = json!(true);
    }
    v
}

fn server_request(sig: &Value) -> Value {
    if let Some(id) = sig["server"].as_u64() {
        return json!({"provider":"ookla","server_id":id});
    }
    let choice = sig["server"].as_str().unwrap_or("auto");
    if let Some((provider, id)) = choice.split_once(':') {
        return json!({"provider":provider,"server_id":id.parse::<u64>().unwrap_or(0)});
    }
    // Numeric choices are the old Ookla-only client's selection.
    if let Ok(id) = choice.parse::<u64>() {
        return json!({"provider":"ookla","server_id":id});
    }
    json!({"provider":choice})
}

/// Save only; no probes or measurements are performed while configuring endpoints.
pub async fn configure(
    State(app): State<Shared>,
    req: axum::extract::Request,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let l = Lang::from_headers(req.headers());
    let who = Caller::of(&app, &req);
    let bytes = axum::body::to_bytes(req.into_body(), 64 * 1024)
        .await
        .unwrap_or_default();
    let signals: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let parsed = signals["libreconfig"]
        .as_str()
        .and_then(|s| serde_json::from_str::<Value>(s).ok());
    let stream = async_stream(move |tx| async move {
        let result = if let Some(cfg) = parsed {
            match crate::agent::put_raw(&app, &who, "/api/speedtest/config", &cfg).await {
                Ok((code, v)) if (200..300).contains(&code) && v["ok"] != false => {
                    match crate::agent::get(&app, &who, "/api/speedtest/config").await {
                        Ok(v) if v["config"] == cfg => Ok(()),
                        _ => Err(l.pick("Saved, but readback could not be verified. Reload before retrying.", "已提交，但未能核对保存结果。请刷新后确认。").to_string()),
                    }
                },
                Ok((code, v)) => Err(crate::agent::refused(code, &v).text(l)),
                Err(e) => Err(e.text(l)),
            }
        } else {
            Err(l
                .pick(
                    "Enter valid LibreSpeed configuration JSON.",
                    "请输入有效的 LibreSpeed 配置 JSON。",
                )
                .to_string())
        };
        let (tone, message) = match result {
            Ok(()) => (
                "ok",
                l.pick(
                    "LibreSpeed endpoints saved and verified.",
                    "LibreSpeed 节点已保存并核对。",
                )
                .to_string(),
            ),
            Err(e) => ("bad", e),
        };
        let html = op_html(l, tone, &message).replace("id=\"st-op\"", "id=\"st-config-result\"");
        let _ = tx.send(patch(&html)).await;
        let _ = app.kick.send(PATH);
    });
    Sse::new(stream)
}

/// Datastar sends the page's signals as the POST body.
pub async fn start(
    State(app): State<Shared>,
    req: axum::extract::Request,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let l = Lang::from_headers(req.headers());
    let who = Caller::of(&app, &req);
    let body = axum::body::to_bytes(req.into_body(), 64 * 1024)
        .await
        .unwrap_or_default();
    let sig: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let req_body = start_request(&sig);
    let stream = async_stream(move |tx| async move {
        let _ = tx.send(patch(&sending(l))).await;
        let r = crate::agent::post_raw(&app, &who, "/api/speedtest/start", &req_body).await;
        // R11: the confirmed full run is for this one request only
        let _ = tx
            .send(
                Event::default()
                    .event("datastar-patch-signals")
                    .data("signals {\"roamFull\":false}"),
            )
            .await;
        let html = match r {
            Ok((c, v)) if (200..300).contains(&c) && v["ok"].as_bool() != Some(false) => {
                *STARTED.lock().unwrap() = Some(Instant::now());
                // DR12: 「也测经 CHILL」 is for one run, back to off
                let _ = tx
                    .send(
                        Event::default()
                            .event("datastar-patch-signals")
                            .data("signals {\"chill\":false}"),
                    )
                    .await;
                op_html(l, "ok", l.pick("Accepted by the device", "设备已接受"))
            }
            // another job holds the device (a diagnosis): the agent's own sentence says what and
            // when to retry, shown as it is (the old OpResult `busy` case)
            Ok((409, v)) if v.get("busy").is_some_and(|b| !b.is_null()) => {
                op_html(l, "warn", &crate::agent::refused(409, &v).text(l))
            }
            // already running: the status shows the running test
            Ok((409, _)) => op_html(l, "", ""),
            Ok((c, v)) => op_html(
                l,
                "bad",
                &not_applied(l, &crate::agent::refused(c, &v).text(l)),
            ),
            Err(e) => op_html(l, "bad", &not_applied(l, &e.text(l))),
        };
        let _ = app.kick.send(PATH);
        let _ = tx.send(patch(&html)).await;
    });
    Sse::new(stream)
}

pub async fn stop(
    State(app): State<Shared>,
    req: axum::extract::Request,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let l = Lang::from_headers(req.headers());
    let who = Caller::of(&app, &req);
    let stream = async_stream(move |tx| async move {
        let _ = tx.send(patch(&sending(l))).await;
        let html = match crate::agent::post(&app, &who, "/api/speedtest/stop", &json!({})).await {
            Err(e) => op_html(l, "bad", &not_applied(l, &e.text(l))),
            Ok(_) => {
                // readback: progress leaves the running phases
                let until = Instant::now() + STOP_WAIT;
                loop {
                    let _ = app.kick.send(PATH);
                    if let Ok(p) = crate::agent::get(&app, &who, "/api/speedtest/progress").await {
                        let ph = p["phase"].as_str().unwrap_or("");
                        if !active(ph) && (ph != "idle" || !expecting()) {
                            *STARTED.lock().unwrap() = None;
                            break op_html(l, "ok", l.pick("Applied", "已生效"));
                        }
                    }
                    if Instant::now() > until {
                        break op_html(
                            l,
                            "bad",
                            l.pick(
                                "Not applied: the device still shows the old value",
                                "没有生效：设备上还是原来的值",
                            ),
                        );
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        };
        let _ = app.kick.send(PATH);
        let _ = tx.send(patch(&html)).await;
    });
    Sse::new(stream)
}

/// Retry on the server list.
pub async fn servers(
    State(app): State<Shared>,
    req: axum::extract::Request,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let l = Lang::from_headers(req.headers());
    let who = Caller::of(&app, &req);
    let stream = async_stream(move |tx| async move {
        let r = crate::agent::get(&app, &who, "/api/speedtest/servers?refresh=1").await;
        let html = ServersFrag {
            s: &Servers {
                l,
                list: Some(servers_of(r, l)),
            },
        }
        .render()
        .unwrap();
        let _ = tx.send(patch(&html)).await;
    });
    Sse::new(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn got(p: Value) -> Got {
        Got {
            prog: Some(p),
            last_ok: Some(SystemTime::now()),
            ..Default::default()
        }
    }

    #[test]
    fn discovery_and_provider_errors_do_not_claim_the_core_is_down() {
        let v = view(
            &got(json!({"phase":"discovering","progress":0})),
            false,
            Lang::En,
        );
        assert!(v.running && v.word.contains("Finding"));
        let p = json!({"phase":"error","error":"Ookla 测速服务无法连接", "error_en":"Ookla speed-test service is unreachable", "error_code":"provider_unreachable"});
        let v = view(&got(p), false, Lang::En);
        assert!(v.reason.unwrap().contains("Ookla speed-test service"));
        assert_eq!(
            start_request(&json!({"server":"librespeed:7"})),
            json!({"provider":"librespeed","server_id":7})
        );
        assert_eq!(
            start_request(&json!({"server":"ookla"})),
            json!({"provider":"ookla"})
        );
        assert_eq!(
            start_request(&json!({"server":"auto"})),
            json!({"provider":"auto"})
        );
        let html = ServersFrag {
            s: &Servers {
                l: Lang::En,
                list: Some(Err("Ookla unreachable".into())),
            },
        }
        .render()
        .unwrap();
        assert!(html.contains("LibreSpeed") && !html.contains("still works"));
    }

    #[test]
    fn running_shows_phase_percent_and_the_live_rate_on_its_readout() {
        let g = got(
            json!({"phase":"download","progress":42.4,"live_speed_mbps":87.25,
            "ping_ms":21.0,"jitter_ms":3.0,"download_mbps":null,"upload_mbps":null,
            "download_bytes":5e7,"upload_bytes":0,"server":"X (Taipei)"}),
        );
        let v = view(&g, false, Lang::Zh);
        assert!(v.running);
        assert_eq!(format!("{}{}", v.word, v.tail.as_deref().unwrap_or("")), "测下载… · 42%");
        assert!(v.v2.is_none(), "an old core: no v2 parts");
        assert_eq!(v.reason.as_deref(), Some("实时：87.2 Mbps"));
        assert_eq!(v.dl.value.as_deref(), Some("87.2"));
        assert_eq!(v.dl.word, "实时");
        assert_eq!(v.ul.value.as_deref(), Some("—"));
        assert_eq!(v.ping.value.as_deref(), Some("21.0"));
    }

    #[test]
    fn just_started_reads_starting_until_the_worker_begins() {
        let g = got(json!({"phase":"idle","progress":0}));
        let v = view(&g, true, Lang::Zh);
        assert!(v.running);
        assert_eq!(format!("{}{}", v.word, v.tail.as_deref().unwrap_or("")), "正在开始… · 0%");
        let v = view(&g, false, Lang::Zh);
        assert!(!v.running);
        assert_eq!(v.word, "还没有结果");
    }

    #[test]
    fn complete_and_details() {
        let g = got(
            json!({"phase":"complete","progress":100,"download_mbps":312.4,"upload_mbps":48.0,
            "ping_ms":20.5,"jitter_ms":2.1,"download_bytes":3.9e8,"upload_bytes":1e7,"server":"CHT (Taipei)"}),
        );
        let v = view(&g, false, Lang::En);
        assert_eq!((v.tone, v.word.as_str()), ("ok", "Complete"));
        assert_eq!(v.reason.as_deref(), Some("Last test, on CHT (Taipei)"));
        // an old core (no method_version): the four readouts as before, no conclusion or layers
        assert!(v.v2.is_none() && v.cost() == "old");
        let html = Leaves { v: &v }.render().unwrap();
        assert!(html.contains("Jitter") && !html.contains("Layers") && html.contains("512 MB"));
        assert!(!html.contains("Also test via CHILL"));
        let d = v.details.unwrap();
        assert_eq!(d.1, "400 MB");
        assert_eq!(d.2, "Download 390 MB · upload 10.0 MB");
    }

    #[test]
    fn unreadable_offers_retry_and_english_has_no_chinese() {
        let g = Got {
            err: Some(Fail::Unreachable),
            ..Default::default()
        };
        let v = view(&g, false, Lang::En);
        assert!(v.retry && v.tone == "bad");
        let mut html = Leaves { v: &v }.render().unwrap();
        for p in [
            json!({"phase":"upload","progress":70,"live_speed_mbps":40}),
            json!({"phase":"error","progress":20,"error":"all ping attempts failed"}),
            json!({"phase":"cancelled","progress":30}),
        ] {
            html += &Leaves {
                v: &view(&got(p), false, Lang::En),
            }
            .render()
            .unwrap();
        }
        html += &ServersFrag {
            s: &Servers {
                l: Lang::En,
                list: Some(Err("x".into())),
            },
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

    fn v2_done(extra: Value) -> Value {
        let mut p = json!({"phase":"complete","progress":100,"download_mbps":312.4,"upload_mbps":41.2,
            "ping_ms":23.0,"jitter_ms":2.4,"download_bytes":5.5e8,"upload_bytes":1.1e8,
            "server":"Ookla · 上海电信 (上海)","method_version":2,"mode":"full","roaming_capped":false,
            "path":"direct","stage":null,"step":5,"steps":5,"connections":6,
            "download_whole_mbps":280.1,"upload_whole_mbps":38.0,"single_mbps":88.0,
            "idle_ms":23.0,"loaded_down_ms":210.0,"sample_short":false,"net_tune":"off",
            "verdict":{"bottlenecks":["single","queue"],"summary_zh":"慢在单连接、下载时卡","summary_en":"Slow at single connection, lag under load",
              "layers":[
                {"key":"single","state":"warn","value_zh":"88 Mbps，多连接的 1/3.5","value_en":"88 Mbps, 1/3.5 of multi"},
                {"key":"proxy","state":"untested","value_zh":"没开「经 CHILL」","value_en":"\"Via CHILL\" off","reason":"not_requested"},
                {"key":"queue","state":"warn","value_zh":"下载时延迟 +187 ms","value_en":"Latency +187 ms downloading"},
                {"key":"radio","state":"ok","value_zh":"SINR 14 dB · n78（只供参考）","value_en":"SINR 14 dB · n78 (reference only)"},
                {"key":"cell_wifi","state":"untested","value_zh":"没测","value_en":"Not tested","reason":"phase_two"}]}});
        for (k, v) in extra.as_object().unwrap() {
            p[k] = v.clone();
        }
        p
    }

    #[test]
    fn v2_running_shows_the_step_and_the_method_not_the_percent() {
        let g = got(json!({"phase":"download","progress":62,"live_speed_mbps":250.0,"method_version":2,
            "mode":"full","stage":"multi_download","step":3,"steps":5,"connections":6}));
        let v = view(&g, false, Lang::Zh);
        assert_eq!(v.word, "3/5 · 多连接下载");
        assert_eq!(v.tail.as_deref(), Some(" · 62%"));
        assert_eq!(v.reason.as_deref(), Some("6 条连接 · 前 2 秒不计"));
        assert_eq!(v.dl.value.as_deref(), Some("250.0"));
        let v = view(&got(json!({"phase":"upload","progress":80,"method_version":2,"stage":"multi_upload","step":5,"steps":6,"connections":6})), false, Lang::En);
        assert_eq!((v.word.as_str(), v.reason.as_deref()), ("5/6 · Multi-connection upload", Some("6-connection upload")));
        let v = view(&got(json!({"phase":"discovering","progress":0,"method_version":2,"stage":null,"step":0,"steps":5})), false, Lang::Zh);
        assert_eq!(v.word, "正在寻找测速节点…");
        // the live region holds the step only; the percent sits outside it
        let html = Leaves { v: &view(&g, false, Lang::Zh) }.render().unwrap();
        let live = html.split("aria-live=\"polite\">").nth(1).unwrap().split("</span></span></span>").next().unwrap();
        assert!(live.contains("3/5") && !live.contains('%'), "{live}");
    }

    #[test]
    fn v2_complete_with_bottlenecks_layers_and_details() {
        let v = view(&got(v2_done(json!({}))), false, Lang::Zh);
        let x = v.v2.as_ref().unwrap();
        assert_eq!(x.verdict, Some(("warn", "慢在单连接、下载时卡".into())));
        assert!(!x.dim);
        assert_eq!(x.loaded.value.as_deref(), Some("210.0"));
        let names: Vec<_> = x.layers.iter().map(|r| (r.name, r.state)).collect();
        assert_eq!(names, [("单连接","warn"),("代理","untested"),("下载时卡","warn"),("无线","ok"),("基站、Wi-Fi","untested")]);
        assert!(x.layers.iter().all(|r| r.href.is_none()), "proxy not slow: no link");
        let rows: Vec<_> = x.rows.iter().map(|r| format!("{}={}", r.0, r.1)).collect();
        assert_eq!(rows, ["全程均值=↓ 280.1 Mbps · ↑ 38.0 Mbps", "抖动=2.4 ms", "连接数=6", "内核调参=关"]);
        let html = Leaves { v: &v }.render().unwrap();
        assert!(html.contains("分层") && html.contains("下载时延迟") && !html.contains("ro-jitter") && html.contains("ro-loaded"));
        assert!(html.contains("完成</span></span> · <span class=\"nd-mark nd-mark--warn\">"));
        // a slow proxy row leads to CHILL
        let p = v2_done(json!({"verdict":{"bottlenecks":["proxy"],"summary_zh":"慢在代理","summary_en":"Slow at proxy",
            "layers":[{"key":"proxy","state":"warn","value_zh":"经 CHILL 40 Mbps，直连的 13%","value_en":"40 Mbps via CHILL, 13% of direct"}]}}));
        let v = view(&got(p), false, Lang::En);
        let x = v.v2.as_ref().unwrap();
        assert_eq!(x.layers[0].href, Some("/services/chill"));
        assert_eq!(x.verdict, Some(("warn", "Slow at proxy".into())));
        let none = v2_done(json!({"verdict":{"bottlenecks":[],"summary_zh":"没发现瓶颈","summary_en":"No bottleneck found","layers":[]}}));
        assert_eq!(view(&got(none), false, Lang::Zh).v2.unwrap().verdict, Some(("ok", "没发现瓶颈".into())));
    }

    #[test]
    fn v2_short_sample_and_rough_tests_have_no_layers() {
        let v = view(&got(v2_done(json!({"sample_short":true,"verdict":null}))), false, Lang::Zh);
        let x = v.v2.as_ref().unwrap();
        assert_eq!(x.verdict, Some(("warn", "样本不足，数字只供参考".into())));
        assert!(x.dim && x.layers.is_empty());
        assert!(Leaves { v: &v }.render().unwrap().contains("st-dim"));
        // short wins over coarse
        let v = view(&got(v2_done(json!({"sample_short":true,"mode":"coarse","verdict":null}))), false, Lang::Zh);
        assert!(v.v2.unwrap().dim);
        let v = view(&got(v2_done(json!({"mode":"coarse","roaming_capped":true,"verdict":null}))), false, Lang::Zh);
        let x = v.v2.unwrap();
        assert_eq!(x.verdict, Some(("neutral", "粗测 · 漫游中只测 5 秒".into())));
        assert!(!x.dim && x.layers.is_empty());
        let v = view(&got(v2_done(json!({"mode":"coarse","verdict":null}))), false, Lang::En);
        assert_eq!(v.v2.unwrap().verdict, Some(("neutral", "Rough test · 5 s only".into())));
    }

    #[test]
    fn v2_cost_line_and_the_chill_switch() {
        let mut g = got(json!({"phase":"idle","progress":0,"method_version":2}));
        let v = view(&g, false, Lang::Zh);
        assert_eq!((v.cost(), v.chill_off()), ("full", None));
        let html = Leaves { v: &v }.render().unwrap();
        assert!(html.contains("750 MB") && html.contains("1.15 GB") && html.contains("也测经 CHILL"));
        g.net = Some((true, true));
        let v = view(&g, false, Lang::Zh);
        assert_eq!((v.cost(), v.chill_off()), ("roaming", Some("漫游中只粗测")));
        let html = Leaves { v: &v }.render().unwrap();
        assert!(html.contains("只粗测 5 秒、最多 30 MB") && html.contains("data-signals:chill=\"false\""));
        g.net = Some((false, false));
        assert_eq!(view(&g, false, Lang::Zh).chill_off(), Some("CHILL 没在运行"));
        assert_eq!(start_request(&json!({"server":"auto","chill":true})), json!({"provider":"auto","path":"chill"}));
        assert_eq!(start_request(&json!({"server":"auto","chill":false})), json!({"provider":"auto"}));
    }

    #[test]
    fn roaming_full_needs_two_confirms_and_is_marked() {
        let mut g = got(json!({"phase":"idle","progress":0,"method_version":2}));
        assert!(!view(&g, false, Lang::Zh).roam_full());
        let html = Leaves { v: &view(&g, false, Lang::Zh) }.render().unwrap();
        assert!(!html.contains("仍要完整测速") && !html.contains("st-full-confirm"));
        // roaming: the start confirm offers it, its own confirm says the cost
        g.net = Some((true, true));
        let v = view(&g, false, Lang::Zh);
        assert!(v.roam_full());
        let html = Leaves { v: &v }.render().unwrap();
        assert!(html.contains("$ask = false; $askFull = true") && html.contains("仍要完整测速"));
        assert!(html.contains("走漫游流量，约 30 秒 · 最多 750 MB") && html.contains("$roamFull = true; @post"));
        // roaming not read yet: a capped last run offers it; home now does not
        g.net = None;
        g.prog = Some(v2_done(json!({"mode":"coarse","roaming_capped":true,"verdict":null})));
        assert!(view(&g, false, Lang::Zh).roam_full());
        g.net = Some((false, true));
        assert!(!view(&g, false, Lang::Zh).roam_full());
        // an old core never gets it; nor a running test
        assert!(!view(&got(json!({"phase":"idle","progress":0})), false, Lang::Zh).roam_full());
        let mut r = got(json!({"phase":"download","progress":40,"method_version":2}));
        r.net = Some((true, true));
        assert!(!view(&r, false, Lang::Zh).roam_full());
        // only the second step sends roaming_ok
        assert_eq!(start_request(&json!({"server":"auto","roamFull":true})), json!({"provider":"auto","roaming_ok":true}));
        assert_eq!(start_request(&json!({"server":"auto","roamFull":false})), json!({"provider":"auto"}));
        // the result says it was a full run while roaming
        let v = view(&got(v2_done(json!({"roaming_full":true}))), false, Lang::Zh);
        assert!(v.v2.unwrap().rows.contains(&("漫游".into(), "漫游中完整测速".into())));
    }

    #[test]
    fn v2_english_has_no_chinese() {
        let mut html = String::new();
        for p in [
            v2_done(json!({})),
            v2_done(json!({"sample_short":true,"verdict":null})),
            v2_done(json!({"mode":"coarse","roaming_capped":true,"verdict":null,"net_tune":"on"})),
            v2_done(json!({"roaming_full":true})),
            json!({"phase":"download","progress":40,"method_version":2,"stage":"single_download","step":2,"steps":5,"server":"x"}),
        ] {
            let mut g = got(p);
            for n in [None, Some((true, true)), Some((false, false))] {
                g.net = n;
                html += &Leaves { v: &view(&g, false, Lang::En) }.render().unwrap();
            }
        }
        // the server name is data (the device names it); everything else is ours
        let html = html.replace("上海电信 (上海)", "");
        assert!(
            !html.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)
                || ('\u{3000}'..='\u{303f}').contains(&c)
                || ('\u{ff00}'..='\u{ffef}').contains(&c)),
            "{html}"
        );
    }
}
