//! /router/qci (QCI / 承载), the old `router/qci/page.tsx`: status (how many bearers are up), the
//! bearer table, the note, and the raw `AT+CGCONTRDP` output behind a toggle. Read-only.
//!
//! Two sources:
//! - **The bearer table is still the agent's** `/api/network/qos`: it sends `AT+CGCONTRDP` and one
//!   `AT+CGEQOSRDP=<cid>` per context on the modem's AT port (about 12 s on the device); u60d has
//!   no per-context table. The old page polled it per tab, one request at a time, 15 s apart with
//!   a 30 s timeout. Here **one read serves every open /router/qci tab** (the AT port is serial):
//!   a shared cache, at most one request in flight, a new one at most every 15 s and only while a
//!   tab's stream asks for it. The request runs in its own task, so a tab's stream (the frame,
//!   the op-bar clock) never waits 12 s for it; when it lands every /router/qci stream is kicked.
//! - **u60d's `qos` block** (new on this page; the signal page's "QCI · AMBR" row): the QCI/5QI
//!   and AMBR of the current data connection, from the modem's key log. It is a different source
//!   from the table's `qci_device` (uci `zwrt_data_tmp`), so it is shown on its own, not merged.

use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use askama::Template;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use crate::agent::{Caller, Fail, Polled, stopped};
use crate::i18n::Lang;
use crate::pages::{Shared, StreamQuery, split_leaves};

const PATH: &str = "/router/qci";
/// Between the end of one read and the start of the next (the old page's 15 s, never faster).
const EVERY: Duration = Duration::from_secs(15);
const TIMEOUT: Duration = Duration::from_secs(30);

/// The last answers of `/api/network/qos`, shared by all tabs.
#[derive(Default)]
pub struct Got {
    /// The newest answer (or failure).
    pub last: Option<Result<Value, Fail>>,
    /// The newest good answer and when it came (kept through failures: "showing the previous").
    pub ok: Option<(Value, SystemTime)>,
    done_at: Option<Instant>,
    inflight_since: Option<Instant>,
}

static CACHE: Mutex<Got> = Mutex::new(Got {
    last: None,
    ok: None,
    done_at: None,
    inflight_since: None,
});

pub fn label(q: i64, l: Lang) -> Option<&'static str> {
    Some(match q {
        1 => l.pick("VoNR / VoLTE conversational voice", "VoNR / VoLTE 会话语音"),
        2 => l.pick("Conversational video", "会话视频"),
        3 => l.pick("Real-time gaming, V2X", "实时游戏、V2X"),
        4 => l.pick("Non-conversational video buffered", "非会话视频缓冲"),
        5 => l.pick("IMS signaling", "IMS 信令"),
        6 | 8 => l.pick("TCP-based (web, email, FTP)", "基于 TCP（网页、邮件、FTP）"),
        7 => l.pick("Voice, video, interactive gaming", "语音、视频、交互式游戏"),
        9 => l.pick(
            "TCP-based default (best-effort internet)",
            "基于 TCP 的默认承载（尽力而为互联网）",
        ),
        65 => l.pick(
            "Mission-critical user plane push-to-talk",
            "关键任务用户面一键通",
        ),
        66 => l.pick("Non-mission-critical push-to-talk", "非关键任务一键通"),
        69 => l.pick("Mission-critical signaling", "关键任务信令"),
        70 => l.pick("Mission-critical data", "关键任务数据"),
        79 => l.pick("V2X messages", "V2X 消息"),
        80 => l.pick("Low-latency eMBB applications", "低时延 eMBB 应用"),
        _ => return None,
    })
}

pub struct Bearer {
    pub cid: String,
    pub bearer: String,
    pub apn: String,
    pub q: String,
    pub q_label: &'static str,
    /// "ok" measured / from the modem, "warn" inferred from the APN.
    pub source_tone: &'static str,
    pub source: &'static str,
    pub gbr: String,
}

pub enum Table {
    Loading,
    Failed,
    Empty,
    Rows(Vec<Bearer>),
}

/// u60d's `qos` block, as on the signal page.
pub struct Now {
    pub value: String,
    pub bearer: Option<&'static str>,
    /// The values are from the last bearer logged before the device moved to the other core.
    pub history: bool,
    pub stale: bool,
}

pub struct View {
    pub l: Lang,
    pub tone: &'static str,
    pub state: String,
    pub reason: Option<String>,
    /// "Numbers stopped at hh:mm" when showing an older reading.
    pub meta: Option<String>,
    pub retry: bool,
    pub signed_out: bool,
    /// The last refresh failed but an older reading is shown.
    pub refresh_failed: Option<String>,
    pub table: Table,
    pub note: Option<String>,
    pub raw: Option<String>,
    pub now: Option<Now>,
}

impl View {
    pub fn symbol(&self) -> &'static str {
        match self.tone {
            "warn" | "stale" => "▲",
            "bad" => "■",
            _ => "●",
        }
    }
}

fn n(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

/// Mbit/s as the signal page writes AMBR.
fn mbps(v: f64) -> String {
    crate::view::fmt_mbps(v)
}

pub fn now_view(q: &Value, stale: bool, l: Lang) -> Now {
    let qci = n(&q["qci"]).filter(|q| *q > 0);
    let ambr = num(&q["ambr_dl"]).filter(|d| *d > 0.0).map(|d| {
        format!(
            "↓ {} ↑ {} Mbps",
            mbps(d),
            num(&q["ambr_ul"])
                .filter(|u| *u > 0.0)
                .map(mbps)
                .unwrap_or_else(|| "—".into())
        )
    });
    let label = if q["bearer"].as_str() == Some("nr_pdu") { "5QI" } else { "QCI" };
    let v = [qci.map(|q| format!("{label} {q}")), ambr]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
    Now {
        value: if v.is_empty() { "—".into() } else { v },
        bearer: match q["bearer"].as_str() {
            Some("eps") => Some(l.pick("EPS bearer (4G core)", "EPS 承载（4G 核心网）")),
            Some("nr_pdu") => Some(l.pick("NR PDU session (5G core)", "NR PDU 会话（5G 核心网）")),
            _ => None,
        },
        history: q["stale"].as_bool() == Some(true),
        stale,
    }
}

fn bearer(c: &Value, l: Lang) -> Bearer {
    let measured = n(&c["qci"]);
    let device = n(&c["qci_device"]);
    let q = measured.or(device).or(n(&c["qci_inferred"]));
    let s = |k: &str| match &c[k] {
        Value::Null => "—".to_string(),
        Value::String(s) if s.is_empty() => "—".into(),
        Value::String(s) => s.clone(),
        v => v.to_string(),
    };
    let gbr = match (n(&c["dl_gbr_kbps"]), n(&c["ul_gbr_kbps"])) {
        (Some(d), Some(u)) => format!("{d} · {u} kbps"),
        _ => "—".into(),
    };
    let (source_tone, source) = if measured.is_some() {
        ("ok", l.pick("measured", "实测"))
    } else if device.is_some() {
        ("ok", l.pick("from modem", "来自调制解调器"))
    } else {
        ("warn", l.pick("inferred from APN", "根据 APN 推断"))
    };
    Bearer {
        cid: s("cid"),
        bearer: s("bearer_id"),
        apn: s("apn"),
        q: q.map_or("—".into(), |q| q.to_string()),
        q_label: q
            .and_then(|q| label(q, l))
            .unwrap_or(l.pick("unknown", "未知")),
        source_tone,
        source,
        gbr,
    }
}

/// The cache outlives the tabs: an answer older than one read cycle is from before this page
/// was opened (nobody was reading), so it is not shown as current.
const FRESH: Duration = Duration::from_secs(EVERY.as_secs() + TIMEOUT.as_secs());

pub fn view(g: &Got, qos: Option<(&Value, bool)>, now_at: Instant, l: Lang) -> View {
    let old = g
        .done_at
        .is_some_and(|t| now_at.saturating_duration_since(t) > FRESH);
    // an old failure (another session's sign-out, an agent restart long ago) is not this page's
    let err = g
        .last
        .as_ref()
        .and_then(|r| r.as_ref().err())
        .filter(|_| !old);
    let signed_out = err == Some(&Fail::SignedOut);
    let now = qos.map(|(q, stale)| now_view(q, stale, l));
    let Some((d, at)) = &g.ok else {
        return View {
            l,
            tone: if err.is_some() { "bad" } else { "neutral" },
            state: match err {
                Some(_) => l.pick("Can't read the bearers", "读不到承载").into(),
                None => l.pick("Reading bearers…", "正在读取承载…").into(),
            },
            reason: Some(match err {
                Some(e) => e.text(l),
                None => l
                    .pick(
                        "The modem is asked over its AT port; this takes about 12 seconds.",
                        "要经 AT 口询问调制解调器，大约 12 秒。",
                    )
                    .into(),
            }),
            meta: None,
            retry: err.is_some() && !signed_out,
            signed_out,
            refresh_failed: None,
            table: if err.is_some() {
                Table::Failed
            } else {
                Table::Loading
            },
            note: None,
            raw: None,
            now,
        };
    };
    let contexts = d["contexts"].as_array().cloned().unwrap_or_default();
    let (tone, state, reason) = if contexts.is_empty() {
        (
            "warn",
            l.pick("No PDP contexts active.", "没有活动的 PDP 上下文。")
                .to_string(),
            Some(
                l.pick(
                    "There is no mobile data connection right now. Check mobile data and signal.",
                    "现在没有移动数据连接。检查移动数据和信号。",
                )
                .to_string(),
            ),
        )
    } else {
        (
            "ok",
            match l {
                Lang::En => format!("{} bearers active", contexts.len()),
                Lang::Zh => format!("{} 个承载在用", contexts.len()),
            },
            None,
        )
    };
    let stale = err.is_some() || old;
    View {
        l,
        tone: if stale { "stale" } else { tone },
        state,
        // an old reading is being read again (the stream started a read)
        reason: if old {
            Some(l.pick("Reading bearers…", "正在读取承载…").into())
        } else {
            reason
        },
        meta: stale.then(|| stopped(l, "Numbers", "数字", Some(*at))),
        retry: err.is_some() && !signed_out,
        signed_out,
        refresh_failed: err.map(|e| match l {
            Lang::En => format!(
                "Last refresh failed: {}. Showing the previous reading.",
                e.text(l)
            ),
            Lang::Zh => format!("最近一次刷新失败：{}。显示的是上一次的读数。", e.text(l)),
        }),
        table: if contexts.is_empty() {
            Table::Empty
        } else {
            Table::Rows(contexts.iter().map(|c| bearer(c, l)).collect())
        },
        note: d["note"].as_str().map(str::to_owned),
        raw: Some(
            d["raw_cgcontrdp"]
                .as_str()
                .filter(|r| !r.is_empty())
                .unwrap_or("—")
                .to_owned(),
        ),
        now,
    }
}

// ---- the stream ----

#[derive(Template)]
#[template(path = "qci_leaves.html")]
struct Leaves<'a> {
    v: &'a View,
}

pub struct Poll {
    who: Caller,
    l: Lang,
}

fn qos_block(app: &Shared) -> Option<(Value, bool)> {
    app.hub.read(|st| {
        st.blocks
            .get("qos")
            .map(|b| (b.data.clone(), b.stale || st.link != crate::hub::Link::Live))
    })
}

impl Poll {
    fn leaves(&self, app: &Shared) -> Vec<(String, String)> {
        let q = qos_block(app);
        let c = CACHE.lock().unwrap();
        let v = view(&c, q.as_ref().map(|(v, s)| (v, *s)), Instant::now(), self.l);
        split_leaves(&Leaves { v: &v }.render().unwrap())
    }
}

/// Starts the shared read if none is running and the last one ended ≥ 15 s ago; the time until
/// this tab should look again (a finished read kicks every tab, so in flight = a long wait).
fn maybe_start(app: &Shared, who: &Caller) -> Duration {
    let now = Instant::now();
    let mut c = CACHE.lock().unwrap();
    // a read that never came back (it has its own 30 s timeout) does not block the next forever
    let inflight = c
        .inflight_since
        .is_some_and(|t| now.duration_since(t) < TIMEOUT + Duration::from_secs(10));
    if inflight {
        return TIMEOUT;
    }
    let due = c.done_at.is_none_or(|t| now.duration_since(t) >= EVERY);
    if !due {
        return EVERY.saturating_sub(now.duration_since(c.done_at.unwrap()))
            + Duration::from_millis(50);
    }
    c.inflight_since = Some(now);
    drop(c);
    let (app, who) = (app.clone(), who.clone());
    tokio::spawn(async move {
        let r = crate::agent::get_slow(&app, &who, "/api/network/qos", TIMEOUT).await;
        {
            let mut c = CACHE.lock().unwrap();
            c.inflight_since = None;
            c.done_at = Some(Instant::now());
            if let Ok(v) = &r {
                c.ok = Some((v.clone(), SystemTime::now()));
            }
            c.last = Some(r);
        }
        let _ = app.kick.send(PATH);
    });
    TIMEOUT
}

impl Polled for Poll {
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        let d = maybe_start(app, &self.who);
        (self.leaves(app), d)
    }
    fn watches(&self, block: &str) -> bool {
        block == "qos"
    }
    fn render(&mut self, app: &Shared) -> Option<Vec<(String, String)>> {
        Some(self.leaves(app))
    }
}

#[derive(Template)]
#[template(path = "qci.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
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

/// The page renders what the shared cache has (no AT read here: it would hold the page 12 s);
/// its stream starts the read.
pub async fn page(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let p = Poll {
        who: Caller::of(&app, &req),
        l: ctx.lang,
    };
    let shell = app.hub.read(|st| {
        crate::shell::Shell::new(&headers, PATH).live(
            st,
            &ctx,
            Some(format!("{PATH}/events?g={}", st.generation)),
        )
    });
    let html = Page {
        shell,
        l: ctx.lang,
        leaves: p.leaves(&app),
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

/// Retry: after a failure, read now (not 15 s after the failure).
pub async fn refresh(State(app): State<Shared>) -> axum::http::StatusCode {
    {
        let mut c = CACHE.lock().unwrap();
        if matches!(c.last, Some(Err(_))) {
            c.done_at = None;
        }
    }
    let _ = app.kick.send(PATH);
    axum::http::StatusCode::NO_CONTENT
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The mock agent's shape (scripts/mock-agent fixtures/network.ts `qos`), synthetic values.
    fn answer() -> Value {
        json!({"contexts":[
            {"cid":1,"bearer_id":5,"apn":"internet.example","qci":null,"qci_device":6,"qci_inferred":9,
             "dl_gbr_kbps":null,"ul_gbr_kbps":null,"dl_mbr_kbps":null,"ul_mbr_kbps":null},
            {"cid":2,"bearer_id":6,"apn":"ims","qci":null,"qci_device":null,"qci_inferred":5,
             "dl_gbr_kbps":null,"ul_gbr_kbps":null},
            {"cid":3,"bearer_id":7,"apn":"ims","qci":1,"qci_device":null,"qci_inferred":5,
             "dl_gbr_kbps":128,"ul_gbr_kbps":64}],
            "raw_cgcontrdp":"AT+CGCONTRDP\n+CGCONTRDP: 1,5,\"internet.example\"",
            "note":"QCI is read from the modem when available."})
    }

    fn got(last: Option<Result<Value, Fail>>, ok: bool) -> Got {
        Got {
            last,
            ok: ok.then(|| (answer(), SystemTime::now())),
            ..Default::default()
        }
    }

    #[test]
    fn the_table_like_the_old_page() {
        let v = view(
            &got(Some(Ok(answer())), true),
            None,
            Instant::now(),
            Lang::Zh,
        );
        assert_eq!(v.state, "3 个承载在用");
        assert_eq!(v.tone, "ok");
        let Table::Rows(r) = &v.table else { panic!() };
        assert_eq!(
            (r[0].q.as_str(), r[0].q_label, r[0].source),
            ("6", "基于 TCP（网页、邮件、FTP）", "来自调制解调器")
        );
        assert_eq!((r[1].q.as_str(), r[1].source_tone), ("5", "warn"));
        assert_eq!(r[1].source, "根据 APN 推断");
        assert_eq!((r[2].q.as_str(), r[2].source), ("1", "实测"));
        assert_eq!(r[2].gbr, "128 · 64 kbps");
        assert_eq!(r[0].gbr, "—");
        assert!(v.raw.unwrap().starts_with("AT+CGCONTRDP"));
        assert!(v.refresh_failed.is_none() && v.meta.is_none());
    }

    #[test]
    fn reading_failed_and_stale_states() {
        let v = view(&Got::default(), None, Instant::now(), Lang::En);
        assert_eq!(v.state, "Reading bearers…");
        assert!(v.reason.unwrap().contains("12 seconds"));
        assert!(matches!(v.table, Table::Loading) && !v.retry);
        let v = view(
            &got(Some(Err(Fail::Unreachable)), false),
            None,
            Instant::now(),
            Lang::Zh,
        );
        assert_eq!((v.tone, v.state.as_str()), ("bad", "读不到承载"));
        assert!(v.retry && matches!(v.table, Table::Failed));
        // an older good reading stays, greyed, with the failure said
        let v = view(
            &got(Some(Err(Fail::Unreachable)), true),
            None,
            Instant::now(),
            Lang::En,
        );
        assert_eq!(v.tone, "stale");
        assert!(matches!(v.table, Table::Rows(_)));
        assert!(v.meta.unwrap().starts_with("Numbers stopped at "));
        assert_eq!(
            v.refresh_failed.unwrap(),
            "Last refresh failed: The device service did not answer. Showing the previous reading."
        );
        // no contexts
        let g = Got {
            last: Some(Ok(json!({"contexts":[],"raw_cgcontrdp":""}))),
            ok: Some((json!({"contexts":[],"raw_cgcontrdp":""}), SystemTime::now())),
            ..Default::default()
        };
        let v = view(&g, None, Instant::now(), Lang::Zh);
        assert_eq!(
            (v.tone, v.state.as_str()),
            ("warn", "没有活动的 PDP 上下文。")
        );
        assert_eq!(v.raw.as_deref(), Some("—"));
        // signed out: no retry, a login link
        let v = view(
            &got(Some(Err(Fail::SignedOut)), false),
            None,
            Instant::now(),
            Lang::En,
        );
        assert!(v.signed_out && !v.retry);
        assert!(
            Leaves { v: &v }
                .render()
                .unwrap()
                .contains("/login?next=/router/qci")
        );
    }

    #[test]
    fn an_answer_from_before_the_page_opened_is_not_shown_as_current() {
        let later = Instant::now() + FRESH + Duration::from_secs(1);
        let mut g = got(Some(Ok(answer())), true);
        g.done_at = Some(Instant::now());
        let v = view(&g, None, later, Lang::Zh);
        assert_eq!(v.tone, "stale", "old table greyed");
        assert!(v.meta.unwrap().starts_with("数字停在 "));
        assert_eq!(v.reason.as_deref(), Some("正在读取承载…"));
        assert!(v.refresh_failed.is_none() && !v.retry, "nothing failed");
        // an old sign-out from another session: reading, not 'signed out'
        let mut g = got(Some(Err(Fail::SignedOut)), false);
        g.done_at = Some(Instant::now());
        let v = view(&g, None, later, Lang::En);
        assert!(!v.signed_out && v.tone == "neutral" && matches!(v.table, Table::Loading));
        // fresh: as it is
        assert!(view(&g, None, Instant::now(), Lang::En).signed_out);
    }

    #[test]
    fn the_u60d_summary_reads_like_the_signal_page() {
        let q = json!({"qci":9,"ambr_dl":"2000.000","ambr_ul":"1000.000","bearer":"nr_pdu","stale":false});
        let n = now_view(&q, false, Lang::Zh);
        // an NR PDU session's class is a 5QI (W1)
        assert_eq!(n.value, "5QI 9 · ↓ 2000 ↑ 1000 Mbps");
        assert_eq!(n.bearer, Some("NR PDU 会话（5G 核心网）"));
        assert!(!n.history);
        let q = json!({"qci":0,"ambr_dl":"","ambr_ul":"","bearer":"","stale":true});
        let n = now_view(&q, true, Lang::En);
        assert_eq!(n.value, "—");
        assert!(n.history && n.stale && n.bearer.is_none());
        assert_eq!(
            now_view(
                &json!({"qci":6,"ambr_dl":"64.5","bearer":"eps"}),
                false,
                Lang::En
            )
            .value,
            "QCI 6 · ↓ 64.5 ↑ — Mbps"
        );
    }

    #[test]
    fn the_english_page_has_no_chinese() {
        let q = json!({"qci":9,"ambr_dl":"300","ambr_ul":"50","bearer":"eps","stale":true});
        for g in [
            got(Some(Ok(answer())), true),
            got(Some(Err(Fail::Unreachable)), true),
            got(Some(Err(Fail::Unreachable)), false),
            Got::default(),
        ] {
            let v = view(&g, Some((&q, false)), Instant::now(), Lang::En);
            let html = Leaves { v: &v }.render().unwrap();
            assert!(
                !html.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)
                    || ('\u{3000}'..='\u{303f}').contains(&c)
                    || ('\u{ff00}'..='\u{ffef}').contains(&c)),
                "{html}"
            );
        }
    }
}
