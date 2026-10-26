//! The CHILL card of the home page (the old `ChillModule`) and its exit control.
//!
//! ```text
//!  poll  GET /api/services/chill (u60d first, 10 s; a write asks again at once)
//!  leaves hm-chill-t (head) · hm-chill-body · hm-chill-seg (the three exits)
//!  write POST /home/chill/exit?x=proxy|global|direct_keep_ai  (tier 1: no confirm)
//!        ─▶ "正在切换…" ─▶ PUT /api/services/chill/exit {"state":x} ─▶ read GET /api/services/chill
//!           until exit == x (≤ 15 s) ─▶ result line in #hm-chill-op
//! ```
//! Only the write's own replies touch `#hm-chill-op`; a push sends the exit control alone and not at
//! all while a write is in flight, so a result line or the pending choice survives every push.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{Query, Request, State};
use axum::response::IntoResponse;
use axum::routing::post;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Src, mark};
use crate::agent::{self, Caller, Fail};
use crate::i18n::Lang;
use crate::pages::Shared;
use crate::write;

const EXITS: [&str; 3] = ["proxy", "global", "direct_keep_ai"];
const AREA: &str = "hm-chill";
const STATUS: &str = "/api/services/chill";

/// Writes in flight (any tab): a push must not put the old exit back under a pending choice.
static WRITING: AtomicUsize = AtomicUsize::new(0);

struct Writing;
impl Writing {
    fn begin() -> Writing {
        WRITING.fetch_add(1, Ordering::SeqCst);
        Writing
    }
}
impl Drop for Writing {
    fn drop(&mut self) {
        WRITING.fetch_sub(1, Ordering::SeqCst);
    }
}

pub(super) fn routes(r: Router<Shared>) -> Router<Shared> {
    r.route("/home/chill/exit", post(exit_set))
        .route("/home/chill/cancel", post(exit_cancel))
}

fn exit_label(l: Lang, x: &str) -> &'static str {
    match x {
        "proxy" => l.pick("Proxy", "代理"),
        "global" => l.pick("Global", "全局"),
        "direct_keep_ai" => l.pick("Direct · AI stays", "直连 · AI 不动"),
        "direct_all" => l.pick("All direct", "全部直连"),
        _ => "—",
    }
}

fn caret() -> String {
    crate::shell::icon("CaretRight", false, 16, "nd-row__chev")
}

/// The card as the first HTML has it: head, a skeleton, and the (empty) write area.
pub(super) fn frame(l: Lang) -> String {
    let idle = write::Area {
        id: AREA,
        l,
        remote: crate::topbar::Via::Local,
        set: "",
        cancel: "/home/chill/cancel",
        phase: &write::Phase::Idle,
    }
    .html();
    format!(
        "<section id=\"hm-chill\" class=\"nd-module nd-module--lilac hm-o2\"><a class=\"nd-module__head\" href=\"/services/chill\"><span id=\"hm-chill-t\">CHILL</span>{}</a><div id=\"hm-chill-body\"><span class=\"nd-skel\" style=\"width:12ch\"></span></div><div id=\"hm-chill-seg\" hidden></div>{idle}</section>",
        caret()
    )
}

/// The "CHILL exit" row of the network identity group (only when it differs from the cellular
/// exit); a hidden row otherwise, so the id stays for the next push.
pub(super) fn id_row(l: Lang, proxy: Option<&Value>, sub: &Option<String>) -> String {
    let Some(p) = proxy else {
        return "<div id=\"hi-chill\" class=\"nd-row\" hidden></div>".into();
    };
    let ip = p["ip"].as_str().filter(|s| !s.is_empty()).unwrap_or("—");
    let sub = sub
        .as_deref()
        .map(|s| format!("<span class=\"nd-row__sub\" data-data>{}</span>", agent::esc(s)))
        .unwrap_or_default();
    format!(
        "<a id=\"hi-chill\" class=\"nd-row\" href=\"/services/chill\"><span class=\"nd-row__text\"><span class=\"nd-row__label\">{}</span>{sub}</span><span class=\"nd-row__value nd-mono\" data-data>{}</span>{}</a>",
        l.pick("CHILL exit", "CHILL 出口"),
        agent::esc(ip),
        caret()
    )
}

pub(super) struct Chill {
    src: Src<Value>,
    /// The last read was `running` with no proxy groups (mihomo stuck): not a valid read.
    core_down: bool,
    seg: String,
}

impl Chill {
    pub(super) fn new() -> Chill {
        Chill {
            src: Src::new(10),
            core_down: false,
            seg: String::new(),
        }
    }

    pub(super) fn next(&self) -> Instant {
        self.src.next()
    }

    /// The last good `/api/services/chill` read (the landing card's home exit).
    pub(super) fn status(&self) -> Option<&Value> {
        self.src.val.as_ref()
    }

    pub(super) async fn poll(&mut self, app: &Shared, who: &Caller, now: Instant, force: bool) {
        if !force && !self.src.due(now) {
            return;
        }
        let r = agent::get(app, who, STATUS).await.ok();
        let at = Instant::now();
        match r {
            Some(v)
                if v["state"] == "running"
                    && v["groups"].as_array().is_none_or(|g| g.is_empty()) =>
            {
                self.core_down = true;
                self.src.set(at, None);
            }
            r => {
                self.core_down = false;
                self.src.set(at, r);
            }
        }
    }

    pub(super) fn leaves(&mut self, app: &Shared, l: Lang, now: Instant) -> Vec<(String, String)> {
        let notice = app
            .hub
            .read(|st| st.side.public.as_ref().map(|p| p["services"]["chill"].clone()))
            .filter(|n| n.is_object());
        let c = self.src.val.as_ref();
        let stale = self.src.stale(now);
        let state = c.and_then(|c| c["state"].as_str());
        let running = state == Some("running");
        let head = match state {
            Some("running") => mark("ok", &format!("CHILL · {}", l.pick("Running", "运行中"))),
            Some("direct") => mark("warn", &format!("CHILL · {}", l.pick("Direct", "直连"))),
            Some("stopped") => mark("neutral", &format!("CHILL · {}", l.pick("Stopped", "已停止"))),
            Some("unknown") => mark("neutral", &format!("CHILL · {}", l.pick("Not started", "还没启动"))),
            _ => "CHILL".to_owned(),
        };
        let mut body = String::new();
        match c {
            None if self.src.failed() => body.push_str(&format!(
                "<p class=\"nd-body text-nd-t2\">{}</p>",
                l.pick("Can't read CHILL status", "读不到 CHILL 状态")
            )),
            None => body.push_str("<span class=\"nd-skel\" style=\"width:12ch\"></span>"),
            Some(c) => {
                if self.core_down {
                    body.push_str(&format!(
                        "<p class=\"nd-aux\">{}</p>",
                        l.pick("The proxy core is not answering", "代理核心没有回应")
                    ));
                }
                if state == Some("direct")
                    && let Some(r) = c["reason"].as_str().filter(|r| !r.is_empty())
                {
                    body.push_str(&format!(
                        "<p class=\"nd-body text-nd-t2\">{}</p>",
                        agent::esc(reason_text(l, r))
                    ));
                }
                if matches!(state, Some("unknown" | "stopped")) {
                    body.push_str(&format!(
                        "<p class=\"nd-body text-nd-t2\">{}</p>",
                        l.pick("Start it on the CHILL page.", "到 CHILL 页启动。")
                    ));
                }
                if let Some(line) = notice.as_ref().and_then(|n| ai_gate_line(l, &n["ai_gate"])) {
                    body.push_str(&line);
                }
                if running {
                    if let Some(n) = &notice
                        && let Some(line) = pick_notice(n, l)
                    {
                        let kind = n["notice_kind"].as_str().unwrap_or("");
                        let tone = match kind {
                            "backup" | "unstable" => "warn",
                            "alldown" => "bad",
                            "back" | "repick" => "ok",
                            _ => "neutral",
                        };
                        let sym = match tone {
                            "warn" => "▲",
                            "bad" => "■",
                            _ => "●",
                        };
                        body.push_str(&format!(
                            "<p role=\"status\" class=\"hm-note\"><span class=\"nd-mark nd-mark--{tone}\"><span class=\"nd-mark__sym\" aria-hidden=\"true\">{sym}</span><span data-data>{}</span></span>",
                            agent::esc(&line)
                        ));
                        if kind == "alldown" {
                            body.push_str(&format!(
                                "<a class=\"t-link\" href=\"/services/chill#chill-nodes\">{}</a>",
                                l.pick("Pick a node →", "去选节点 →")
                            ));
                        }
                        body.push_str("</p>");
                    }
                    body.push_str(&format!(
                        "<p class=\"nd-body\" data-data>{}</p>",
                        agent::esc(c["region"]["active"].as_str().unwrap_or("—"))
                    ));
                    if c["exit"] == "direct_all" {
                        let gated = notice.as_ref().is_some_and(|n| n["ai_gate"].is_object());
                        body.push_str(&format!(
                            "<p class=\"nd-aux mt-2\">{}</p>",
                            if gated {
                                l.pick(
                                    "Everything else is going direct (VoWiFi too). Change it on the CHILL page.",
                                    "其余全部直连（VoWiFi 也直连）。到 CHILL 页修改。"
                                )
                            } else {
                                l.pick(
                                    "Everything is going direct (AI and VoWiFi too). Change it on the CHILL page.",
                                    "现在是全部直连（AI 和 VoWiFi 也直连）。到 CHILL 页修改。"
                                )
                            }
                        ));
                    }
                    if stale {
                        body.push_str(&format!(
                            "<p class=\"nd-aux mt-2\">{}</p>",
                            l.pick("The selection may have changed since.", "状态可能已变。")
                        ));
                    }
                }
            }
        }
        let mut out = vec![
            (
                "hm-chill-t".to_owned(),
                format!("<span id=\"hm-chill-t\">{head}</span>"),
            ),
            (
                "hm-chill-body".to_owned(),
                format!(
                    "<div id=\"hm-chill-body\"{}>{body}</div>",
                    if stale { " class=\"nd-stale\"" } else { "" }
                ),
            ),
        ];
        // the control: not while a write is in flight (its own replies show the pending choice)
        if WRITING.load(Ordering::SeqCst) == 0 {
            let current = c
                .and_then(|c| c["exit"].as_str())
                .filter(|x| EXITS.contains(x));
            self.seg = seg_html(l, running, current, None);
        }
        out.push(("hm-chill-seg".to_owned(), self.seg.clone()));
        out
    }
}

/// The AI gate (`services.chill.ai_gate`, luyaz-core engines/chill_ai_gate.rs): one line while AI is
/// not going through 🤖 AI — blocked, or out the other way with its exit checked in the region.
fn ai_gate_line(l: Lang, g: &Value) -> Option<String> {
    if !g.is_object() || g["way"] == "ai" {
        return None;
    }
    let region = match g["region"].as_str().unwrap_or("TW") {
        "JP" => l.pick("Japan", "日本"),
        "SG" => l.pick("Singapore", "新加坡"),
        "US" => l.pick("the US", "美国"),
        _ => l.pick("Taiwan", "台湾"),
    };
    let (tone, sym, text) = if g["open"] == true {
        (
            "ok",
            "●",
            match l {
                Lang::En => format!("AI on: exit checked in {region}"),
                Lang::Zh => format!("AI 可用：出口已确认在{region}"),
            },
        )
    } else {
        (
            "warn",
            "▲",
            match l {
                Lang::En => format!("AI paused: exit not in {region} (AI only leaves through the AI group's region)"),
                Lang::Zh => format!("AI 已暂停：出口不在{region}（AI 只走 AI 策略组选的地区）"),
            },
        )
    };
    Some(format!(
        "<p role=\"status\" class=\"hm-note\"><span class=\"nd-mark nd-mark--{tone}\"><span class=\"nd-mark__sym\" aria-hidden=\"true\">{sym}</span><span>{}</span></span></p>",
        agent::esc(&text)
    ))
}

fn pick_notice(n: &Value, l: Lang) -> Option<String> {
    let zh = n["notice"].as_str().filter(|s| !s.is_empty());
    let en = n["notice_en"].as_str().filter(|s| !s.is_empty());
    match l {
        Lang::En => en.or(zh),
        Lang::Zh => zh,
    }
    .map(str::to_owned)
}

fn reason_text(l: Lang, r: &str) -> &str {
    match r {
        "overheat" => l.pick("Paused while hot", "过热，已暂停"),
        "lowmem" => l.pick("Paused, low memory", "内存不足，已暂停"),
        "gaveup" => l.pick("Gave up after repeated failures", "多次失败，已放弃"),
        "ruleset_missing" => l.pick("Rule set missing", "规则集缺失"),
        "captive_wan" => l.pick("Waiting for the network sign-in page", "等待网络登录页"),
        "paused" => l.pick("Paused", "已暂停"),
        "idle" => l.pick("Idle", "空闲"),
        "disabled" => l.pick("Switched off", "已关闭"),
        other => other,
    }
}

/// The exit control. `busy` = a write towards this exit is in flight: that exit shows selected and
/// the control is off. Hidden unless CHILL runs.
fn seg_html(l: Lang, running: bool, current: Option<&str>, busy: Option<&str>) -> String {
    if !running {
        return "<div id=\"hm-chill-seg\" class=\"hm-seg\" hidden></div>".into();
    }
    let shown = busy.or(current);
    let mut items = String::new();
    for x in EXITS {
        let on = shown == Some(x);
        items.push_str(&format!(
            "<button type=\"button\" role=\"radio\" class=\"nd-seg__item\" aria-checked=\"{on}\"{}{} data-write=\"{AREA}\" data-on:click=\"{}\">{}</button>",
            if on { " data-selected=\"true\"" } else { "" },
            if busy.is_some() { " disabled data-disabled=\"true\"" } else { "" },
            write::post_once(&format!(
                "/home/chill/exit?x={x}{}",
                current.map(|c| format!("&was={c}")).unwrap_or_default()
            )),
            exit_label(l, x)
        ));
    }
    format!(
        "<div id=\"hm-chill-seg\" class=\"hm-seg\"><div class=\"nd-seg nd-seg--block\" role=\"radiogroup\" aria-label=\"{}\"><div class=\"nd-seg__track\">{items}</div></div></div>",
        l.pick("Exit", "出口")
    )
}

fn area(l: Lang, phase: &write::Phase) -> String {
    write::Area {
        id: AREA,
        l,
        remote: crate::topbar::Via::Local,
        set: "",
        cancel: "/home/chill/cancel",
        phase,
    }
    .html()
}

#[derive(Deserialize)]
struct ExitQuery {
    x: String,
    /// The exit that was selected when the button was pressed: what to show again if the status
    /// cannot be read after the write.
    #[serde(default)]
    was: String,
}

/// What the card says about the exit now (a fresh read, not the page's cache).
async fn read_exit(app: &Shared, who: &Caller) -> (bool, Option<String>) {
    match agent::get(app, who, STATUS).await {
        Ok(v) => (
            v["state"] == "running",
            v["exit"].as_str().map(str::to_owned),
        ),
        Err(_) => (false, None),
    }
}

async fn exit_set(
    State(app): State<Shared>,
    Query(q): Query<ExitQuery>,
    req: Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, "/");
    let l = ctx.lang;
    let who = Caller::of(&app, &req);
    let (x, was) = (q.x, q.was);
    let pending = format!(
        "{}{}",
        seg_html(l, true, None, Some(&x)),
        area(
            l,
            &write::Phase::Pending(
                match l {
                    Lang::En => format!("Switching the exit to {}…", exit_label(l, &x)),
                    Lang::Zh => format!("正在把出口切到{}…", exit_label(l, &x)),
                }
            )
        )
    );
    let guard = Writing::begin();
    write::respond(pending, async move {
        let _guard = guard;
        let o = run(&app, &who, l, &x).await;
        let (running, now) = read_exit(&app, &who).await;
        let _ = app.kick.send("/");
        format!(
            "{}{}",
            seg_html(
                l,
                running,
                now.as_deref().filter(|e| EXITS.contains(e)).or(Some(was.as_str()).filter(|e| EXITS.contains(e))),
                None
            ),
            area(l, &write::Phase::Done(o))
        )
    })
}

/// Tier 1: PUT, then read the status until `exit` shows it. No confirm.
async fn run(app: &Shared, who: &Caller, l: Lang, x: &str) -> write::Outcome {
    if !EXITS.contains(&x) {
        return write::Outcome::Failed(l.pick("unknown exit", "不认识这个出口").into());
    }
    let sent_ok = match agent::put_raw(app, who, "/api/services/chill/exit", &json!({"state": x})).await {
        Ok((code, v)) if (200..300).contains(&code) && v["ok"].as_bool() != Some(false) => true,
        Ok((code, v)) => return write::Outcome::Failed(agent::refused(code, &v).text(l)),
        // no answer: it may have happened; read back anyway
        Err(Fail::Unreachable) => false,
        Err(f) => return write::Outcome::Failed(f.text(l)),
    };
    let held = write::read_back(
        &app.hub,
        write::READBACK,
        Duration::from_secs(1),
        Duration::from_secs(2),
        || async move { read_exit(app, who).await.1.as_deref() == Some(x) },
    )
    .await;
    match (held, sent_ok) {
        (true, _) => write::Outcome::Applied(match l {
            Lang::En => format!("Exit is now {}", exit_label(l, x)),
            Lang::Zh => format!("出口已改为{}", exit_label(l, x)),
        }),
        (false, true) => write::Outcome::Accepted,
        (false, false) => write::Outcome::Unknown,
    }
}

#[derive(Deserialize, Default)]
struct Cancel {
    #[serde(default)]
    lost: bool,
}

/// `lost=true`: the send's reply dropped (write.rs).
async fn exit_cancel(
    State(app): State<Shared>,
    Query(q): Query<Cancel>,
    req: Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, "/");
    let who = Caller::of(&app, &req);
    let (running, now) = read_exit(&app, &who).await;
    let phase = write::cancel_phase(&write::CancelQuery { lost: q.lost });
    let html = format!(
        "{}{}",
        seg_html(ctx.lang, running, now.as_deref().filter(|e| EXITS.contains(e)), None),
        area(ctx.lang, &phase)
    );
    axum::response::sse::Sse::new(futures_util::stream::once(async move {
        Ok::<_, std::convert::Infallible>(crate::pages::patch(&html))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_ai_gate_line_only_while_ai_is_not_going_through_its_group() {
        assert_eq!(ai_gate_line(Lang::Zh, &Value::Null), None, "gate off");
        assert_eq!(ai_gate_line(Lang::Zh, &json!({"way": "ai", "open": true, "region": "TW"})), None);
        let closed = ai_gate_line(Lang::Zh, &json!({"way": "cellular", "open": false, "region": "TW"})).unwrap();
        assert!(closed.contains("AI 已暂停：出口不在台湾") && closed.contains("nd-mark--warn"), "{closed}");
        let open = ai_gate_line(Lang::En, &json!({"way": "global", "open": true, "region": "JP"})).unwrap();
        assert!(open.contains("AI on: exit checked in Japan") && open.contains("nd-mark--ok"), "{open}");
    }
}
