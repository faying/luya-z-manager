//! /tools/diagnose (网络诊断), the old `tools/diagnose/page.tsx` + `lib/diagnose.ts`
//! (slow-diagnosis.md §4.2, §12): status block (main cause + one action, or 正在检查… N/M,
//! 等另一个操作做完…, 后台没回应) · 「HH:MM 测」 · 「再查一次」 → one row per layer, then the speed row
//! → what you can do (「加测速度」, two presses when roaming; 「对 / 不对」).
//!
//! The agent (deep_diag.rs) judges and words everything in both languages; the run is the
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
const LAYER_ORDER: [&str; 6] = ["wifi", "signal", "limit", "link", "crowd", "proxy"];

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
        "signal" => l.pick("Signal", "信号"),
        "limit" => l.pick("Speed cap", "限速"),
        "link" => l.pick("Cellular link", "蜂窝链路"),
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
                "Find out whether it's Wi-Fi, signal, a speed cap, the cellular link or cell load",
                "查一查卡在 Wi-Fi、信号、限速、蜂窝链路还是基站负载",
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
                {"id":"crowd","level":"na","detail":"无数据","detail_en":"no data","counted":false}],
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
        assert_eq!(names, ["Wi-Fi", "蜂窝链路", "基站负载", "代理", "速度"]);
        assert_eq!(v.rows[2].word, "测不了 · 无数据");
        assert!(v.rows[2].muted);
        assert_eq!(v.rows[4].word, "直连 ↓ 86 Mbps");
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
