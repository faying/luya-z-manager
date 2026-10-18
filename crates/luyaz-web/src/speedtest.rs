//! /tools/speedtest (测速), the old `tools/speedtest/page.tsx`: status (phase · reason, start /
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
        "latency" => l.pick("Measuring latency…", "正在测量延迟…"),
        "download" => l.pick("Testing download…", "正在测试下载…"),
        "upload" => l.pick("Testing upload…", "正在测试上传…"),
        "complete" => l.pick("Complete", "完成"),
        "cancelled" => l.pick("Cancelled", "已取消"),
        "error" => l.pick("Error", "错误"),
        _ => l.pick("Idle", "空闲"),
    }
}

pub struct View {
    pub l: Lang,
    pub tone: &'static str,
    pub word: String,
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
}

impl View {
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
    let (tone, word, reason) = match p {
        None if g.err.is_some() => (
            "bad",
            l.pick("Can't read the speed test", "读不到测速状态")
                .to_string(),
            g.err.as_ref().map(|e| e.text(l)),
        ),
        None => ("neutral", l.pick("Reading…", "正在读取…").to_string(), None),
        Some(_) if running => (
            if stale { "stale" } else { "neutral" },
            format!(
                "{} · {pct}%",
                if active(phase) {
                    phase_word(phase, l)
                } else {
                    l.pick("Starting…", "正在开始…")
                }
            ),
            live.clone().map(|v| match l {
                Lang::En => format!("Live: {v} Mbps"),
                Lang::Zh => format!("实时：{v} Mbps"),
            }),
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
}

impl Poll {
    fn new(who: Caller, l: Lang) -> Poll {
        Poll {
            who,
            l,
            got: Got::default(),
            servers: None,
            servers_at: None,
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

fn start_request(sig: &Value) -> Value {
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
        let html = match r {
            Ok((c, v)) if (200..300).contains(&c) && v["ok"].as_bool() != Some(false) => {
                *STARTED.lock().unwrap() = Some(Instant::now());
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
        assert_eq!(v.word, "正在测试下载… · 42%");
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
        assert_eq!(v.word, "正在开始… · 0%");
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
}
