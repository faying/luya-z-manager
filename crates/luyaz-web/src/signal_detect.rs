//! /router/signal-detect (信号检测), the old `router/signal-detect/page.tsx`. Loads nothing from
//! the device on entry (as before); Start and Stop are tier 2 (inline confirm).
//!
//! ```text
//!  Start ─▶ agent start ─▶ Running ─▶ readback: progress moves (≤ 10 × 2 s)
//!  Running: every stream of this page polls progress every 2 s; ≥ 100 after a reading below
//!           100 (or after 60 s of 100s: the device may still say 100 from the last run) ─▶ Done,
//!           results read once
//!  Stop  ─▶ ① agent stop ─▶ Stopped ② read results (two steps, R10)
//! ```
//! The old page kept the run in the tab; here it is one state in luyaz-web (the detection is
//! device-wide), so a reload or a second tab shows the run, and any open stream finishes it.
//! The results endpoint is a ubus passthrough of unconfirmed shape: rows from cells[] /
//! results[] / list[], else the object is one row; all-empty = no result (B27 answers `{}`).

use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use askama::Template;
use axum::extract::{Query, State};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::Stream;
use serde_json::{Value, json};

use crate::agent::{Caller, Fail, Polled, stopped};
use crate::i18n::Lang;
use crate::pages::{Shared, StreamQuery, async_stream, patch, split_leaves};

const PATH: &str = "/router/signal-detect";
const POLL: Duration = Duration::from_secs(2);
const VERIFY_TRIES: u32 = 10;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    Idle,
    Running { started: Instant, saw_below: bool },
    Done,
    Stopped,
}

type Rows = Vec<Vec<(String, String)>>;

struct Run {
    phase: Phase,
    pct: Option<f64>,
    quality: Option<String>,
    /// None = not read this run.
    rows: Option<Rows>,
    results_err: Option<Fail>,
    loading: bool,
    progress_err: Option<String>,
    last_ok: Option<SystemTime>,
}

static RUN: Mutex<Run> = Mutex::new(Run {
    phase: Phase::Idle,
    pct: None,
    quality: None,
    rows: None,
    results_err: None,
    loading: false,
    progress_err: None,
    last_ok: None,
});

fn pct_of(v: &Value) -> Option<f64> {
    match &v["progress"] {
        Value::Number(n) => n.as_f64(),
        Value::String(s) if !s.trim().is_empty() => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|x: &f64| x.is_finite())
}

fn quality_of(v: &Value) -> Option<String> {
    match &v["quality"] {
        Value::Null => None,
        Value::String(s) => Some(s.trim().to_owned()),
        other => Some(other.to_string()),
    }
    .filter(|s| !s.is_empty())
}

fn cell(v: &Value) -> String {
    match v {
        Value::Null => "—".into(),
        Value::String(s) if s.is_empty() => "—".into(),
        Value::String(s) => s.clone(),
        Value::Object(_) | Value::Array(_) => v.to_string(),
        other => other.to_string(),
    }
}

fn to_rows(d: &Value) -> Rows {
    let Some(o) = d.as_object() else {
        return Vec::new();
    };
    let arr = ["cells", "results", "list"]
        .iter()
        .find_map(|k| o.get(*k).and_then(Value::as_array));
    if let Some(a) = arr {
        return a
            .iter()
            .filter_map(Value::as_object)
            .map(|r| r.iter().map(|(k, v)| (k.clone(), cell(v))).collect())
            .collect();
    }
    if o.is_empty() || o.values().all(|v| v.is_null() || v.as_str() == Some("")) {
        return Vec::new();
    }
    vec![o.iter().map(|(k, v)| (k.clone(), cell(v))).collect()]
}

async fn load_results(app: &Shared, who: &Caller) -> Result<(), Fail> {
    RUN.lock().unwrap().loading = true;
    let r = crate::agent::get(app, who, "/api/cell/signal-detect/results").await;
    let mut run = RUN.lock().unwrap();
    run.loading = false;
    match r {
        Ok(d) => {
            run.rows = Some(to_rows(&d));
            run.results_err = None;
            Ok(())
        }
        Err(e) => {
            // keep the old rows, say why
            run.results_err = Some(e.clone());
            Err(e)
        }
    }
}

/// One progress reading while running; true when it just finished.
fn on_progress(run: &mut Run, d: &Value) -> bool {
    run.last_ok = Some(SystemTime::now());
    run.progress_err = None;
    run.quality = quality_of(d);
    let Phase::Running { started, saw_below } = run.phase else {
        return false;
    };
    let Some(p) = pct_of(d) else {
        return false;
    };
    let saw_below = saw_below || p < 100.0;
    run.phase = Phase::Running { started, saw_below };
    if p >= 100.0 && !saw_below && started.elapsed() < Duration::from_secs(60) {
        return false; // the last run's 100
    }
    run.pct = Some(p);
    if p >= 100.0 {
        run.phase = Phase::Done;
        return true;
    }
    false
}

// ---- view ----

pub struct View {
    pub l: Lang,
    pub tone: &'static str,
    pub word: String,
    pub reason: Option<String>,
    pub running: bool,
    pub bar: Option<i64>,
    pub show_bar: bool,
    pub quality: Option<String>,
    pub fresh: Option<String>,
    pub results_err: Option<String>,
    pub results_note: Option<String>,
    pub keys: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

fn view(run: &Run, l: Lang) -> View {
    let running = matches!(run.phase, Phase::Running { .. });
    let done = run.phase == Phase::Done;
    let pct = if running {
        run.pct
    } else if done {
        Some(100.0)
    } else {
        None
    };
    let pct = pct.map(|p| p.round().clamp(0.0, 100.0) as i64);
    let (tone, word, reason) = match run.phase {
        Phase::Running { .. } => (
            if run.progress_err.is_some() {
                "stale"
            } else {
                "neutral"
            },
            match (pct, l) {
                (Some(p), Lang::En) => format!("Detecting · {p}%"),
                (Some(p), Lang::Zh) => format!("正在检测 · {p}%"),
                (None, _) => l.pick("Detecting…", "正在检测…").into(),
            },
            Some(match (&run.progress_err, run.last_ok, l) {
                (Some(e), None, Lang::En) => {
                    format!("Can't read progress: {e}. Still trying every 2 seconds.")
                }
                (Some(e), None, Lang::Zh) => format!("读不到进度：{e}。每 2 秒会再试一次。"),
                _ => l
                    .pick(
                        "The modem is measuring signal quality. You can leave this tab; it keeps checking.",
                        "调制解调器正在测量信号质量。可以切到别的标签页，进度会继续查。",
                    )
                    .into(),
            }),
        ),
        Phase::Done => (
            "ok",
            l.pick("Detection complete", "检测完成").into(),
            None,
        ),
        Phase::Stopped => (
            "neutral",
            l.pick("Detection stopped", "检测已停止").into(),
            Some(
                l.pick(
                    "Results collected so far are shown below.",
                    "已经测到的结果在下面。",
                )
                .into(),
            ),
        ),
        Phase::Idle => (
            "neutral",
            l.pick("Not detecting", "没在检测").into(),
            Some(
                l.pick(
                    "Press “Start Detection” to measure signal quality.",
                    "按「开始检测」测量当前的信号质量。",
                )
                .into(),
            ),
        ),
    };
    let (keys, rows, note) = match &run.rows {
        None => (
            Vec::new(),
            Vec::new(),
            Some(
                if run.loading {
                    l.pick("Reading results…", "正在读取结果…")
                } else if running {
                    l.pick(
                        "Results appear here when the detection finishes.",
                        "检测完成后，结果会显示在这里。",
                    )
                } else {
                    l.pick(
                        "No result yet · press “Start Detection”",
                        "还没有结果 · 按「开始检测」",
                    )
                }
                .to_string(),
            ),
        ),
        Some(r) if r.is_empty() => (
            Vec::new(),
            Vec::new(),
            Some(
                l.pick(
                    "No results available. Try detecting again.",
                    "暂无可用结果。可以再检测一次。",
                )
                .into(),
            ),
        ),
        Some(r) => {
            let mut keys: Vec<String> = Vec::new();
            for row in r {
                for (k, _) in row {
                    if !keys.contains(k) {
                        keys.push(k.clone());
                    }
                }
            }
            let rows = r
                .iter()
                .map(|row| {
                    keys.iter()
                        .map(|k| {
                            row.iter()
                                .find(|(kk, _)| kk == k)
                                .map_or("—".to_string(), |(_, v)| v.clone())
                        })
                        .collect()
                })
                .collect();
            (keys, rows, None)
        }
    };
    View {
        l,
        tone,
        word,
        reason,
        running,
        bar: pct,
        show_bar: running || done,
        quality: (running || done).then(|| run.quality.clone()).flatten(),
        fresh: (running && run.progress_err.is_some())
            .then(|| stopped(l, "Progress", "进度", run.last_ok)),
        results_err: run.results_err.as_ref().map(|e| match l {
            Lang::En => format!("Couldn't read the results: {}", e.text(l)),
            Lang::Zh => format!("没读到结果：{}", e.text(l)),
        }),
        results_note: note,
        keys,
        rows,
    }
}

#[derive(Template)]
#[template(path = "signal_detect_leaves.html")]
struct Leaves<'a> {
    v: &'a View,
}

fn leaves(l: Lang) -> Vec<(String, String)> {
    let v = view(&RUN.lock().unwrap(), l);
    split_leaves(&Leaves { v: &v }.render().unwrap())
}

pub struct Poll {
    who: Caller,
    l: Lang,
}

impl Polled for Poll {
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        let running = matches!(RUN.lock().unwrap().phase, Phase::Running { .. });
        if running {
            match crate::agent::get(app, &self.who, "/api/cell/signal-detect/progress").await {
                Ok(d) => {
                    let finished = on_progress(&mut RUN.lock().unwrap(), &d);
                    if finished {
                        let _ = load_results(app, &self.who).await;
                    }
                }
                Err(e) => RUN.lock().unwrap().progress_err = Some(e.text(self.l)),
            }
        }
        // not running: nothing to ask the device; re-render now and then (another tab's run)
        let every = if running {
            POLL
        } else {
            Duration::from_secs(5)
        };
        (leaves(self.l), every)
    }
}

#[derive(Template)]
#[template(path = "signal_detect.html")]
struct Page {
    shell: crate::shell::Shell,
    leaves: Vec<(String, String)>,
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
    let shell = app.hub.read(|st| {
        crate::shell::Shell::new(&headers, PATH).live(
            st,
            &ctx,
            Some(format!("{PATH}/events?g={}", st.generation)),
        )
    });
    let html = Page {
        shell,
        leaves: leaves(ctx.lang),
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
    let p = Poll {
        who: Caller::of(&app, &req),
        l: ctx.lang,
    };
    crate::agent::stream(app, ctx, q.g, PATH, p)
}

// ---- writes ----

fn op(l: Lang, tone: &str, text: &str) -> String {
    let (sym, role) = match tone {
        "ok" => ("●", "status"),
        "warn" => ("▲", "alert"),
        _ => ("■", "alert"),
    };
    let _ = l;
    format!(
        "<div id=\"sd-op\" class=\"grid gap-1\"><p role=\"{role}\"><span class=\"nd-mark nd-mark--{tone}\"><span class=\"nd-mark__sym\" aria-hidden=\"true\">{sym}</span><span>{}</span></span></p></div>",
        crate::agent::esc(text)
    )
}

fn sending(l: Lang) -> String {
    format!(
        "<div id=\"sd-op\" class=\"grid gap-1\"><p class=\"nd-aux\" role=\"status\">{}</p></div>",
        l.pick("Sending…", "提交中…")
    )
}

fn failed(l: Lang, e: &str) -> String {
    match l {
        Lang::En => format!("Not applied: {e}"),
        Lang::Zh => format!("没有生效：{e}"),
    }
}

pub async fn start(
    State(app): State<Shared>,
    req: axum::extract::Request,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let l = Lang::from_headers(req.headers());
    let who = Caller::of(&app, &req);
    let stream = async_stream(move |tx| async move {
        let _ = tx.send(patch(&sending(l))).await;
        // the reading before start: a different one after it also counts as "moving"
        let baseline = crate::agent::get(&app, &who, "/api/cell/signal-detect/progress")
            .await
            .ok()
            .and_then(|d| pct_of(&d));
        let html = match crate::agent::post(&app, &who, "/api/cell/signal-detect/start", &json!({}))
            .await
        {
            Err(e) => op(l, "bad", &failed(l, &e.text(l))),
            Ok(_) => {
                {
                    let mut run = RUN.lock().unwrap();
                    run.phase = Phase::Running {
                        started: Instant::now(),
                        saw_below: false,
                    };
                    run.rows = None;
                    run.results_err = None;
                    run.pct = None;
                    run.progress_err = None;
                }
                let _ = app.kick.send(PATH);
                let mut moved = false;
                for _ in 0..VERIFY_TRIES {
                    tokio::time::sleep(POLL).await;
                    if !matches!(RUN.lock().unwrap().phase, Phase::Running { .. }) {
                        moved = true; // finished or stopped meanwhile
                        break;
                    }
                    let p = crate::agent::get(&app, &who, "/api/cell/signal-detect/progress")
                        .await
                        .ok()
                        .and_then(|d| pct_of(&d));
                    if let Some(p) = p {
                        if (p > 0.0 && p < 100.0) || baseline.is_some_and(|b| b != p) {
                            moved = true;
                            break;
                        }
                    }
                }
                if moved {
                    op(l, "ok", l.pick("Applied", "已生效"))
                } else {
                    op(
                        l,
                        "bad",
                        l.pick(
                            "Not applied: the device still shows the old value",
                            "没有生效：设备上还是原来的值",
                        ),
                    )
                }
            }
        };
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
        let html = match crate::agent::post(&app, &who, "/api/cell/signal-detect/stop", &json!({}))
            .await
        {
            Err(e) => op(l, "bad", &failed(l, &e.text(l))),
            Ok(_) => {
                RUN.lock().unwrap().phase = Phase::Stopped;
                let _ = app.kick.send(PATH);
                match load_results(&app, &who).await {
                    Ok(()) => op(l, "ok", l.pick("Applied", "已生效")),
                    Err(_) => op(
                        l,
                        "warn",
                        l.pick(
                            "Partly applied: Stop done, Load results not confirmed",
                            "部分生效：停止 已完成，读取结果 还没确认",
                        ),
                    ),
                }
            }
        };
        let _ = app.kick.send(PATH);
        let _ = tx.send(patch(&html)).await;
    });
    Sse::new(stream)
}

/// Retry on "couldn't read the results".
pub async fn results(
    State(app): State<Shared>,
    req: axum::extract::Request,
) -> axum::http::StatusCode {
    let who = Caller::of(&app, &req);
    let _ = load_results(&app, &who).await;
    let _ = app.kick.send(PATH);
    axum::http::StatusCode::NO_CONTENT
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(phase: Phase) -> Run {
        Run {
            phase,
            pct: None,
            quality: None,
            rows: None,
            results_err: None,
            loading: false,
            progress_err: None,
            last_ok: None,
        }
    }

    #[test]
    fn rows_from_any_of_the_shapes() {
        assert!(to_rows(&json!({})).is_empty(), "B27's {{}}");
        assert!(to_rows(&json!({"a":"","b":null})).is_empty());
        let r = to_rows(&json!({"cells":[{"pci":5,"rsrp":-97},{"pci":7}]}));
        assert_eq!(r.len(), 2);
        assert_eq!(
            r[0],
            vec![("pci".into(), "5".into()), ("rsrp".into(), "-97".into())]
        );
        assert_eq!(
            to_rows(&json!({"pci":"5","x":{"y":1}}))[0][1].1,
            "{\"y\":1}"
        );
    }

    #[test]
    fn the_last_runs_100_is_not_the_end() {
        let mut r = run(Phase::Running {
            started: Instant::now(),
            saw_below: false,
        });
        assert!(!on_progress(&mut r, &json!({"progress":"100"})));
        assert!(r.pct.is_none());
        assert!(!on_progress(
            &mut r,
            &json!({"progress":"40","quality":"good"})
        ));
        assert_eq!(view(&r, Lang::Zh).word, "正在检测 · 40%");
        assert!(on_progress(
            &mut r,
            &json!({"progress":100,"quality":"good"})
        ));
        assert_eq!(r.phase, Phase::Done);
        let v = view(&r, Lang::Zh);
        assert_eq!((v.word.as_str(), v.bar), ("检测完成", Some(100)));
        assert_eq!(v.quality.as_deref(), Some("good"));
    }

    #[test]
    fn a_table_with_every_key_and_english_without_chinese() {
        let mut r = run(Phase::Stopped);
        r.rows = Some(to_rows(&json!({"list":[{"pci":5},{"rsrp":-97}]})));
        let v = view(&r, Lang::En);
        assert_eq!(v.keys, vec!["pci", "rsrp"]);
        assert_eq!(v.rows[1], vec!["—", "-97"]);
        let mut html = Leaves { v: &v }.render().unwrap();
        for ph in [
            Phase::Idle,
            Phase::Done,
            Phase::Running {
                started: Instant::now(),
                saw_below: true,
            },
        ] {
            let mut r = run(ph);
            r.results_err = Some(Fail::Unreachable);
            r.progress_err = Some("y".into());
            html += &Leaves {
                v: &view(&r, Lang::En),
            }
            .render()
            .unwrap();
        }
        assert!(
            !html.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)
                || ('\u{3000}'..='\u{303f}').contains(&c)
                || ('\u{ff00}'..='\u{ffef}').contains(&c)),
            "{html}"
        );
    }
}
