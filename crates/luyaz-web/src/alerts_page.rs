//! /alerts (告警), the old `alerts/page.tsx`: what failed while nobody was watching, and where the
//! alert SMS goes. The events come from the core and the sentinel (process supervisor and
//! Wi-Fi watchdog); the core sends the SMS (the sentinel while the core is unhealthy).
//!
//! Where each value comes from (the old page read `/api/alerts` every 30 s; u60d serves it, the
//! agent when u60d says "not mine", both through `agent.rs` with the tab's own session):
//! - status line, event list (time, unread mark, kind, text), clock note, SMS state, number,
//!   abroad switch and its "abroad now" note, recent SMS (time / kind / result, "sent in 24 h"):
//!   all from that one answer, polled every 30 s ([`EVERY`]) and re-polled when a button here
//!   changes something (`App::kick`).
//! - Not read from the hub's alert list: this page needs the SMS part, which the top bar's read
//!   (`topbar.rs`) does not keep for the page. The bar hides itself on /alerts (its own rule).
//!
//! Writes (web/docs/controls-inventory.md §/alerts), none of them through `/control`:
//! - 「全部标为已读」, tier 1: `POST /api/alerts/read {seq: newest}` (the newest seq is read fresh,
//!   not taken from the page); read back from `/api/alerts` (`read >= seq`: the agent clamps the
//!   cursor, a newer event arriving meanwhile is not a failure). Afterwards the top bar is told to
//!   read again (`App::side_kick`), as its own 「知道了」 does.
//! - Phone number, tier 2: `PUT /api/alerts/sms {number}` ("" = SMS alerts off), inline confirm,
//!   read back from `sms.number` / `sms.configured` (after the same normalisation the agent does).
//! - 「在国外也发送」, tier 2: `PUT /api/alerts/sms {abroad}`, inline confirm, read back from
//!   `sms.abroad_allowed`.
//!   No reply but the request may have gone out → 「连接中断，还没确认是否生效」; the reply says
//!   it went through but the read shows something else → 「设备已接受，还没读回」, never "成功".

use std::convert::Infallible;
use std::time::{Duration, SystemTime};

use askama::Template;
use axum::Router;
use axum::extract::{Query, State};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::Stream;
use serde_json::{Value, json};

use crate::agent::{Caller, Fail, Polled, esc};
use crate::i18n::Lang;
use crate::pages::{Shared, StreamQuery, patch, split_leaves};
use crate::write::{self, Outcome, Phase};

const PATH: &str = "/alerts";
const EVERY: Duration = Duration::from_secs(30);
/// Before 2024-01-01 the device clock was not set (the old page's rule for the SMS log).
const CLOCK_SANE: i64 = 1_704_067_200;

// ---- what the agent says ----

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ev {
    pub seq: u64,
    pub time: Option<i64>,
    pub uptime: i64,
    pub kind: String,
    pub label: Option<String>,
    pub label_en: Option<String>,
    pub text: String,
    pub unread: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rec {
    pub time: i64,
    pub kind: String,
    pub label: Option<String>,
    pub label_en: Option<String>,
    pub result: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sms {
    pub configured: bool,
    pub number: Option<String>,
    pub abroad_allowed: bool,
    pub abroad_now: bool,
    pub sent_24h: u64,
    pub recent: Vec<Rec>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Data {
    pub events: Vec<Ev>,
    pub unread: u64,
    pub read: u64,
    pub clock_set: bool,
    pub sms: Sms,
}

fn s(v: &Value) -> Option<String> {
    v.as_str().filter(|x| !x.is_empty()).map(Into::into)
}

pub fn parse(v: &Value) -> Data {
    let events = v["events"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|e| Ev {
                    seq: e["seq"].as_u64().unwrap_or(0),
                    time: e["time"].as_i64(),
                    uptime: e["uptime"].as_i64().unwrap_or(0),
                    kind: e["kind"].as_str().unwrap_or("").into(),
                    label: s(&e["label"]),
                    label_en: s(&e["label_en"]),
                    text: e["text"].as_str().unwrap_or("").into(),
                    unread: e["unread"].as_bool().unwrap_or(false),
                })
                .collect()
        })
        .unwrap_or_default();
    let m = &v["sms"];
    let recent = m["recent"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|r| Rec {
                    time: r["time"].as_i64().unwrap_or(0),
                    kind: r["kind"].as_str().unwrap_or("").into(),
                    label: s(&r["label"]),
                    label_en: s(&r["label_en"]),
                    result: r["result"].as_str().unwrap_or("").into(),
                })
                .collect()
        })
        .unwrap_or_default();
    Data {
        events,
        unread: v["unread"].as_u64().unwrap_or(0),
        read: v["read"].as_u64().unwrap_or(0),
        clock_set: v["clock_set"].as_bool().unwrap_or(false),
        sms: Sms {
            configured: m["configured"].as_bool().unwrap_or(false),
            number: s(&m["number"]),
            abroad_allowed: m["abroad_allowed"].as_bool().unwrap_or(false),
            abroad_now: m["abroad_now"].as_bool().unwrap_or(false),
            sent_24h: m["sent_24h"].as_u64().unwrap_or(0),
            recent,
        },
    }
}

/// What an alert (or an SMS record) means in the page language, as the agent words it; a record
/// from an agent that sends no label borrows one from an event of the same kind, else its kind.
fn word(label: &Option<String>, en: &Option<String>, kind: &str, l: Lang, events: &[Ev]) -> String {
    let pick = |zh: &Option<String>, en: &Option<String>| match l {
        Lang::En => en.clone().or_else(|| zh.clone()),
        Lang::Zh => zh.clone().or_else(|| en.clone()),
    };
    pick(label, en)
        .or_else(|| {
            events
                .iter()
                .find(|e| e.kind == kind)
                .and_then(|e| pick(&e.label, &e.label_en))
        })
        .unwrap_or_else(|| kind.to_owned())
}

/// "MM-DD HH:mm" of a device-clock epoch (local wall time labelled UTC: the UTC digits are the
/// local ones).
pub fn mdhm(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // civil from days (H. Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{m:02}-{d:02} {:02}:{:02}", rem / 3600, rem / 60 % 60)
}

fn when(e: &Ev, l: Lang) -> String {
    match e.time {
        Some(t) => mdhm(t),
        None => match l {
            Lang::En => format!("{} min after boot", e.uptime / 60),
            Lang::Zh => format!("开机后 {} 分钟", e.uptime / 60),
        },
    }
}

/// alerts.rs `valid_number`: optional +, at least 3 digits, at most 20 characters; "" = off.
pub fn number_ok(n: &str) -> bool {
    if n.is_empty() {
        return true;
    }
    let digits = n.strip_prefix('+').unwrap_or(n);
    n.len() <= 20 && digits.len() >= 3 && digits.bytes().all(|b| b.is_ascii_digit())
}

/// alerts.rs `alerts_sms_set`'s normalisation: trim, drop spaces and dashes.
pub fn number_norm(n: &str) -> String {
    n.trim()
        .chars()
        .filter(|c| *c != ' ' && *c != '-')
        .collect()
}

// ---- the page's leaves ----

struct Poll {
    who: Caller,
    l: Lang,
    data: Option<Data>,
    fail: Option<Fail>,
    at: Option<SystemTime>,
}

fn mark(tone: &str, text: &str) -> String {
    let sym = match tone {
        "bad" => "■",
        "warn" => "▲",
        _ => "●",
    };
    format!(
        "<span class=\"nd-mark nd-mark--{tone}\"><span class=\"nd-mark__sym\" aria-hidden=\"true\">{sym}</span><span>{}</span></span>",
        esc(text)
    )
}

fn retry_btn(l: Lang) -> String {
    format!(
        "<button type=\"button\" class=\"nd-btn nd-btn--secondary nd-btn--sm\" data-on:click=\"@post('/alerts/refresh')\">{}</button>",
        l.pick("Retry", "重试")
    )
}

const SWITCH_TRACK: &str = "<span class=\"nd-switch__track\"><svg class=\"nd-switch__check\" viewBox=\"0 0 12 12\" aria-hidden=\"true\"><path d=\"M2.5 6.2 5 8.6 9.5 3.6\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.8\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/></svg><span class=\"nd-switch__thumb\"></span></span>";

impl Poll {
    fn new(who: Caller, l: Lang) -> Poll {
        Poll {
            who,
            l,
            data: None,
            fail: None,
            at: None,
        }
    }

    async fn fetch(&mut self, app: &Shared) {
        match crate::agent::get(app, &self.who, "/api/alerts").await {
            Ok(v) => {
                self.data = Some(parse(&v));
                self.fail = None;
                self.at = Some(SystemTime::now());
            }
            Err(f) => self.fail = Some(f),
        }
    }

    /// The latest read failed but an older answer is shown: nothing may be changed from it.
    fn stale(&self) -> bool {
        self.data.is_some() && self.fail.is_some()
    }

    fn leaves(&self) -> Vec<(String, String)> {
        let l = self.l;
        let d = self.data.as_ref();
        let stale = self.stale();
        let st_cls = if stale { " nd-stale" } else { "" };
        let mut out = String::new();

        // status
        let (mut tone, state, reason): (&str, String, Option<String>) = match (d, &self.fail) {
            (None, Some(f)) => (
                "bad",
                l.pick("Can't read alerts", "读不到告警").into(),
                Some(f.text(l)),
            ),
            (None, None) => (
                "neutral",
                l.pick("Reading alerts…", "正在读取告警…").into(),
                None,
            ),
            (Some(d), _) if d.unread > 0 => (
                "warn",
                match l {
                    Lang::En => format!("{} new alert(s)", d.unread),
                    Lang::Zh => format!("{} 条新告警", d.unread),
                },
                d.events
                    .first()
                    .map(|e| word(&e.label, &e.label_en, &e.kind, l, &d.events)),
            ),
            (Some(d), _) if d.events.is_empty() => (
                "ok",
                l.pick("No alerts", "没有告警").into(),
                Some(l.pick("The device is fine.", "设备一切正常。").into()),
            ),
            (Some(_), _) => ("ok", l.pick("No new alerts", "没有新告警").into(), None),
        };
        let meta = stale.then(|| {
            tone = "stale";
            crate::agent::stopped(l, "List", "列表", self.at)
        });
        let actions = if d.is_none() && self.fail.is_some() {
            retry_btn(l)
        } else if d.is_some_and(|d| d.unread > 0) {
            format!(
                "<button type=\"button\" class=\"nd-btn nd-btn--secondary nd-btn--sm\" data-write=\"al-mark\"{} data-on:click=\"{}\">{}</button>",
                if stale { " disabled" } else { "" },
                write::post_once("/alerts/mark"),
                l.pick("Mark all as read", "全部标为已读")
            )
        } else {
            String::new()
        };
        out.push_str(&format!(
            "<div id=\"al-status\"><section class=\"nd-status nd-status--{tone}{}\"><div class=\"nd-status__main\"><div class=\"nd-status__state\" role=\"status\">{}</div>{}{}</div>{}</section></div>\n<!--leaf-->\n",
            if tone == "stale" { " nd-stale" } else { "" },
            mark(tone, &state),
            reason.map_or(String::new(), |r| format!(
                "<div class=\"nd-status__reason\"><span{}>{}</span></div>",
                if tone == "bad" { " role=\"alert\"" } else { "" },
                esc(&r)
            )),
            meta.map_or(String::new(), |m| format!(
                "<div class=\"nd-status__meta\"><span class=\"nd-fresh\">{}</span></div>",
                esc(&m)
            )),
            if actions.is_empty() {
                String::new()
            } else {
                format!("<div class=\"nd-status__actions\">{actions}</div>")
            },
        ));

        // events
        out.push_str("<div id=\"al-events\">");
        match d {
            None if self.fail.is_none() => out.push_str(
                "<div class=\"nd-group\"><div class=\"nd-row\"><span class=\"nd-skel\" style=\"width:18ch\"></span></div></div>",
            ),
            None => {}
            Some(d) => {
                if stale {
                    out.push_str(&format!(
                        "<p class=\"al-note\" role=\"alert\">{} {}</p>",
                        mark(
                            "warn",
                            l.pick(
                                "Couldn't refresh the list; showing the last one.",
                                "列表刷新失败，下面是上一次读到的。"
                            )
                        ),
                        retry_btn(l)
                    ));
                }
                if d.events.is_empty() {
                    out.push_str(&format!(
                        "<p class=\"nd-body al-note\">{}</p>",
                        mark(
                            "ok",
                            l.pick(
                                "Nothing has gone wrong. If a program crashes or the watchdog has to turn Wi-Fi back on, it shows up here.",
                                "一切正常。程序崩溃、或看门狗不得不重新打开 Wi-Fi 时，会记在这里。"
                            )
                        )
                    ));
                } else {
                    out.push_str(&format!("<ul class=\"nd-group al-list{st_cls}\">"));
                    for e in &d.events {
                        let label = esc(&word(&e.label, &e.label_en, &e.kind, l, &d.events));
                        let head = if e.unread {
                            format!(
                                "<span class=\"nd-row__label\"><span class=\"al-unread\"><span aria-hidden=\"true\">▲ </span><span class=\"sr-only\">{}: </span></span>{label}</span>",
                                l.pick("unread", "未读")
                            )
                        } else {
                            format!("<span class=\"al-read\">{label}</span>")
                        };
                        out.push_str(&format!(
                            "<li class=\"nd-row nd-row--two al-item\"><span class=\"nd-row__text\">{head}{}</span><span class=\"al-time{}\">{}</span></li>",
                            if e.text.is_empty() {
                                String::new()
                            } else {
                                format!("<span class=\"nd-row__sub al-wrap\">{}</span>", esc(&e.text))
                            },
                            if e.time.is_some() { " nd-mono" } else { "" },
                            esc(&when(e, l))
                        ));
                    }
                    out.push_str("</ul>");
                }
                if !d.clock_set {
                    out.push_str(&format!(
                        "<p class=\"nd-aux al-note\">{}</p>",
                        l.pick(
                            "The device clock is not set yet, so times are shown relative to boot.",
                            "设备时钟还没校准，时间按开机后计算。"
                        )
                    ));
                }
            }
        }
        out.push_str("</div>\n<!--leaf-->\n");

        // SMS state
        out.push_str("<p id=\"al-smsstate\" class=\"nd-body al-note\">");
        match d {
            None if self.fail.is_none() => out.push_str("<span class=\"nd-skel\" style=\"width:24ch\"></span>"),
            None => {}
            Some(d) if d.sms.configured => {
                let n = d.sms.number.clone().unwrap_or_default();
                out.push_str(&mark(
                    "ok",
                    &match l {
                        Lang::En => format!("On — sent from this device's SIM to {n}."),
                        Lang::Zh => format!("已开启：用本机 SIM 发到 {n}。"),
                    },
                ));
            }
            Some(_) => out.push_str(&mark(
                "warn",
                l.pick(
                    "Not set up. Alerts are only shown here and on the touch screen until you add a number.",
                    "未配置。填号码之前，告警只显示在这里和触屏上。",
                ),
            )),
        }
        out.push_str("</p>\n<!--leaf-->\n");

        // the number form (a signal holds the draft, so a push keeps what is being typed)
        let saved: String = d
            .and_then(|d| d.sms.number.clone())
            .unwrap_or_default()
            .chars()
            .filter(|c| c.is_ascii_digit() || *c == '+')
            .collect();
        let locked = d.is_none() || stale;
        let can = format!("!($_albad || $_alnorm === '{saved}')");
        out.push_str(&format!(
            "<form id=\"al-numform\" class=\"nd-row al-form{st_cls}\" data-signals__ifmissing=\"{{_aln: '{saved}'}}\" data-computed:_alnorm=\"($_aln ?? '').trim().replace(/[ -]/g, '')\" data-computed:_albad=\"($_alnorm !== '' &amp;&amp; !/^\\+?\\d{{3,}}$/.test($_alnorm)) || $_alnorm.length &gt; 20\"{}>\
<label for=\"al-n\" class=\"nd-row__label\">{}</label>\
<div class=\"al-in\"><input id=\"al-n\" class=\"nd-field nd-mono\" type=\"tel\" inputmode=\"tel\" autocomplete=\"off\" data-bind=\"_aln\" aria-describedby=\"al-n-hint al-n-limits\"{}>\
<button type=\"submit\" class=\"nd-btn nd-btn--primary\" data-attr:disabled=\"{}\">{}</button></div>\
<span id=\"al-n-hint\" class=\"nd-row__sub al-bad\" data-attr:hidden=\"!$_albad\" hidden>{}</span>\
<span id=\"al-n-limits\" class=\"nd-row__sub\">{}</span></form>\n<!--leaf-->\n",
            if locked {
                String::new()
            } else {
                format!(
                    " data-on:submit__prevent=\"{} &amp;&amp; @post('/alerts/number/ask?n=' + encodeURIComponent($_alnorm))\"",
                    can.replace('&', "&amp;")
                )
            },
            l.pick("Phone number", "手机号"),
            if locked { " disabled" } else { "" },
            if locked {
                "true".to_owned()
            } else {
                format!("!({can})").replace('&', "&amp;")
            },
            l.pick("Save", "保存"),
            l.pick(
                "Digits only, an optional leading +, 3 to 20 characters.",
                "只能是数字，开头可以带 +，3 到 20 个字符。"
            ),
            l.pick(
                "At most one SMS per kind of alert per hour and five a day; anything over that is only recorded here. The message says what failed and when — nothing else. Leave empty to turn off.",
                "同一类告警每小时最多 1 条，每天最多 5 条，超出的只记在这里。短信只写哪里出了问题和时间，不含其他内容。留空即关闭。"
            ),
        ));

        // the abroad switch
        let allowed = d.is_some_and(|d| d.sms.abroad_allowed);
        let sub = d
            .filter(|d| d.sms.abroad_now && !d.sms.abroad_allowed)
            .map(|_| {
                format!(
                    "<span class=\"nd-row__sub block\">{}</span>",
                    l.pick(
                        "The device is in an abroad scenario, so SMS alerts are held back right now.",
                        "设备当前处于国外情景，短信告警暂不发送。"
                    )
                )
            })
            .unwrap_or_default();
        out.push_str(&format!(
            "<div id=\"al-abroad\" class=\"nd-row nd-row--two{st_cls}\"><span class=\"nd-row__text\"><span class=\"nd-row__label\" id=\"al-abroad-label\">{}</span>{sub}</span><span class=\"t-sw\"><button type=\"button\" role=\"switch\" class=\"nd-switch t-switch\" aria-labelledby=\"al-abroad-label\" aria-checked=\"{allowed}\"{}{}{}>{SWITCH_TRACK}</button></span></div>\n<!--leaf-->\n",
            l.pick(
                "Also send while abroad (may cost roaming fees)",
                "在国外也发送（可能产生漫游费）"
            ),
            if locked { " disabled data-disabled=\"true\"" } else { "" },
            if allowed { " data-selected=\"true\"" } else { "" },
            if locked {
                String::new()
            } else {
                format!(" data-on:click=\"@post('/alerts/abroad/ask?on={}')\"", u8::from(!allowed))
            },
        ));

        // recent SMS
        out.push_str("<div id=\"al-recent\">");
        if let Some(d) = d.filter(|d| !d.sms.recent.is_empty()) {
            out.push_str(&format!(
                "<h2 id=\"al-recent-t\" class=\"nd-group-title\">{}</h2><ul class=\"nd-group al-list{st_cls}\" aria-labelledby=\"al-recent-t\">",
                match l {
                    Lang::En => format!("Recent SMS · {} sent in the last 24 h", d.sms.sent_24h),
                    Lang::Zh => format!("最近的短信 · 24 小时内已发 {} 条", d.sms.sent_24h),
                }
            ));
            for r in &d.sms.recent {
                let (tone, text) = match r.result.as_str() {
                    "sent" => ("ok", l.pick("Sent", "已发送").to_owned()),
                    "failed" => ("bad", l.pick("Failed", "发送失败").to_owned()),
                    "suppressed-rate" => (
                        "neutral",
                        l.pick("Not sent — over the limit", "超出限额，未发")
                            .to_owned(),
                    ),
                    "suppressed-abroad" => (
                        "neutral",
                        l.pick("Not sent — abroad", "在国外，未发").to_owned(),
                    ),
                    "no-number" => (
                        "neutral",
                        l.pick("Not sent — no number set", "未配置号码，未发")
                            .to_owned(),
                    ),
                    other => ("neutral", other.to_owned()),
                };
                out.push_str(&format!(
                    "<li class=\"nd-row nd-row--two al-item\"><span class=\"nd-row__text\"><span class=\"al-read\">{}</span><span class=\"nd-row__sub block nd-mono\">{}</span></span><span class=\"al-time\">{}</span></li>",
                    esc(&word(&r.label, &r.label_en, &r.kind, l, &d.events)),
                    if r.time >= CLOCK_SANE { mdhm(r.time) } else { "—".into() },
                    mark(tone, &text),
                ));
            }
            out.push_str("</ul>");
        }
        out.push_str("</div>\n");
        split_leaves(&out)
    }
}

impl Polled for Poll {
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        self.fetch(app).await;
        // a page with nothing to show retries sooner than the 30 s of a good list
        let next = if self.data.is_none() {
            Duration::from_secs(10)
        } else {
            EVERY
        };
        (self.leaves(), next)
    }
}

// ---- the page ----

#[derive(Template)]
#[template(path = "alerts.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    leaves: Vec<(String, String)>,
    mark_op: String,
    num_op: String,
    abroad_op: String,
}

impl Page {
    fn leaf(&self, id: &str) -> &str {
        self.leaves
            .iter()
            .find(|(k, _)| k == id)
            .map_or("", |(_, h)| h.as_str())
    }
}

fn area(id: &str, set: &str, cancel: &str, l: Lang, remote: bool, phase: &Phase) -> String {
    write::Area {
        id,
        l,
        remote,
        set,
        cancel,
        phase,
    }
    .html()
}

const MARK_CANCEL: &str = "/alerts/mark/cancel";
const NUM_CANCEL: &str = "/alerts/number/cancel";
const ABROAD_CANCEL: &str = "/alerts/abroad/cancel";

fn mark_area(l: Lang, remote: bool, phase: &Phase) -> String {
    area("al-mark", "/alerts/mark", MARK_CANCEL, l, remote, phase)
}
fn num_area(l: Lang, remote: bool, set: &str, phase: &Phase) -> String {
    area("al-num", set, NUM_CANCEL, l, remote, phase)
}
fn abroad_area(l: Lang, remote: bool, set: &str, phase: &Phase) -> String {
    area("al-abroad", set, ABROAD_CANCEL, l, remote, phase)
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
    let (l, r) = (ctx.lang, ctx.remote);
    let html = Page {
        shell,
        l,
        leaves: p.leaves(),
        mark_op: mark_area(l, r, &Phase::Idle),
        num_op: num_area(l, r, "", &Phase::Idle),
        abroad_op: abroad_area(l, r, "", &Phase::Idle),
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

/// Retry: ask the agent again now.
pub async fn refresh(State(app): State<Shared>) -> axum::http::StatusCode {
    let _ = app.kick.send(PATH);
    axum::http::StatusCode::NO_CONTENT
}

// ---- writes ----

fn one(html: String) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(futures_util::stream::once(async move { Ok(patch(&html)) }))
}

/// Ask the page and the top bar to read again.
fn reread(app: &Shared) {
    let _ = app.kick.send(PATH);
    app.side_kick.notify_one();
}

async fn read_alerts(app: &Shared, who: &Caller) -> Option<Data> {
    crate::agent::get(app, who, "/api/alerts")
        .await
        .ok()
        .map(|v| parse(&v))
}

/// A write's reply: `Ok` = taken; otherwise how it ended. A request that may have gone out
/// without an answer is "not confirmed", not "failed".
fn taken(r: Result<(u16, Value), Fail>, l: Lang) -> Result<(), Outcome> {
    match r {
        Ok((c, v)) if (200..300).contains(&c) && v["ok"].as_bool() != Some(false) => Ok(()),
        Ok((c, v)) => Err(Outcome::Failed(crate::agent::refused(c, &v).text(l))),
        Err(Fail::Unreachable) => Err(Outcome::Unknown),
        Err(f) => Err(Outcome::Failed(f.text(l))),
    }
}

async fn do_mark(app: Shared, who: Caller, l: Lang, remote: bool) -> String {
    let phase = Phase::Done(mark_outcome(&app, &who, l).await);
    reread(&app);
    mark_area(l, remote, &phase)
}

async fn mark_outcome(app: &Shared, who: &Caller, l: Lang) -> Outcome {
    let Some(before) = read_alerts(app, who).await else {
        return Outcome::NotYet(
            l.pick(
                "Alerts not read yet; try again shortly",
                "还没读到告警，稍后再试",
            )
            .into(),
        );
    };
    let Some(seq) = before.events.first().map(|e| e.seq) else {
        return Outcome::NotYet(l.pick("There are no alerts", "没有告警").into());
    };
    let sent = crate::agent::post_raw(app, who, "/api/alerts/read", &json!({"seq": seq})).await;
    if let Err(o) = taken(sent, l) {
        return o;
    }
    match read_alerts(app, who).await {
        Some(d) if d.read >= seq => {
            Outcome::Applied(l.pick("All alerts marked as read", "已全部标为已读").into())
        }
        _ => Outcome::Accepted,
    }
}

pub async fn mark_send(
    State(app): State<Shared>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let (l, r) = (ctx.lang, ctx.remote);
    let who = Caller::of(&app, &req);
    write::respond(
        mark_area(
            l,
            r,
            &Phase::Pending(l.pick("Marking as read…", "正在标为已读…").into()),
        ),
        do_mark(app, who, l, r),
    )
}

fn cancel(
    f: fn(Lang, bool, &str, &Phase) -> String,
    q: &write::CancelQuery,
    req: &axum::extract::Request,
) -> String {
    let ctx = crate::topbar::Ctx::of_request(req, PATH);
    f(ctx.lang, ctx.remote, "", &write::cancel_phase(q))
}

pub async fn mark_cancel(
    Query(q): Query<write::CancelQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    one(mark_area(ctx.lang, ctx.remote, &write::cancel_phase(&q)))
}

pub async fn num_cancel(
    Query(q): Query<write::CancelQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    one(cancel(num_area, &q, &req))
}

pub async fn abroad_cancel(
    Query(q): Query<write::CancelQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    one(cancel(abroad_area, &q, &req))
}

#[derive(serde::Deserialize)]
pub struct NumQuery {
    #[serde(default)]
    n: String,
}

fn inline(l: Lang, title: String, what: String, action: String) -> write::Confirm {
    let _ = l;
    write::Confirm {
        tier: write::Tier::Inline,
        title,
        what,
        downtime: None,
        recovery: None,
        action,
        cuts_uplink: false,
        word: None,
        danger: false,
    }
}

pub fn number_confirm(n: &str, l: Lang) -> write::Confirm {
    if n.is_empty() {
        inline(
            l,
            l.pick("Turn SMS alerts off", "关闭短信告警").into(),
            l.pick(
                "No more alert SMS. Alerts are still recorded here and on the touch screen.",
                "不再发告警短信。告警照样记在这里和触屏上。",
            )
            .into(),
            l.pick("Confirm: turn SMS alerts off", "确认：关闭短信告警")
                .into(),
        )
    } else {
        inline(
            l,
            l.pick("Save the number", "保存号码").into(),
            match l {
                Lang::En => format!("Alerts will be texted from this device's SIM to {n}."),
                Lang::Zh => format!("以后的告警会用本机 SIM 发短信到 {n}。"),
            },
            l.pick("Confirm: save the number", "确认：保存号码").into(),
        )
    }
}

pub fn abroad_confirm(on: bool, l: Lang) -> write::Confirm {
    if on {
        inline(
            l,
            l.pick("Send while abroad", "在国外也发送").into(),
            l.pick(
                "Alert SMS will also go out while the device is abroad; each may cost a roaming fee.",
                "设备在国外时也会发告警短信，每条都可能产生漫游费。",
            )
            .into(),
            l.pick("Confirm: send while abroad", "确认：在国外也发送").into(),
        )
    } else {
        inline(
            l,
            l.pick("Hold SMS while abroad", "在国外不发送").into(),
            l.pick(
                "While the device is abroad, alerts are only recorded here, not texted.",
                "设备在国外时，告警只记在这里，不发短信。",
            )
            .into(),
            l.pick("Confirm: hold SMS while abroad", "确认：在国外不发送")
                .into(),
        )
    }
}

fn bad_number(l: Lang) -> Outcome {
    Outcome::Failed(
        l.pick(
            "Digits only, an optional leading +, 3 to 20 characters.",
            "只能是数字，开头可以带 +，3 到 20 个字符。",
        )
        .into(),
    )
}

fn num_set_url(n: &str) -> String {
    format!("/alerts/number/set?n={}", n.replace('+', "%2B"))
}

pub async fn number_ask(
    Query(q): Query<NumQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let (l, r) = (ctx.lang, ctx.remote);
    let n = number_norm(&q.n);
    one(if number_ok(&n) {
        num_area(l, r, &num_set_url(&n), &Phase::Ask(number_confirm(&n, l)))
    } else {
        num_area(l, r, "", &Phase::Done(bad_number(l)))
    })
}

async fn do_number(app: Shared, who: Caller, l: Lang, remote: bool, n: String) -> String {
    let o = number_outcome(&app, &who, l, &n).await;
    reread(&app);
    num_area(l, remote, "", &Phase::Done(o))
}

async fn number_outcome(app: &Shared, who: &Caller, l: Lang, n: &str) -> Outcome {
    let sent = crate::agent::put_raw(app, who, "/api/alerts/sms", &json!({"number": n})).await;
    if let Err(o) = taken(sent, l) {
        return o;
    }
    let held = read_alerts(app, who).await.is_some_and(|d| {
        if n.is_empty() {
            !d.sms.configured
        } else {
            d.sms.configured && d.sms.number.as_deref() == Some(n)
        }
    });
    if !held {
        return Outcome::Accepted;
    }
    Outcome::Applied(
        if n.is_empty() {
            l.pick("SMS alerts turned off", "短信告警已关闭")
        } else {
            l.pick("Number saved", "号码已保存")
        }
        .into(),
    )
}

pub async fn number_set(
    State(app): State<Shared>,
    Query(q): Query<NumQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let (l, r) = (ctx.lang, ctx.remote);
    let who = Caller::of(&app, &req);
    let n = number_norm(&q.n);
    let pending = num_area(
        l,
        r,
        "",
        &Phase::Pending(l.pick("Saving…", "正在保存…").into()),
    );
    write::respond(pending, async move {
        if number_ok(&n) {
            do_number(app, who, l, r, n).await
        } else {
            num_area(l, r, "", &Phase::Done(bad_number(l)))
        }
    })
}

#[derive(serde::Deserialize)]
pub struct AbroadQuery {
    #[serde(default)]
    on: u8,
}

pub async fn abroad_ask(
    Query(q): Query<AbroadQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let on = q.on == 1;
    one(abroad_area(
        ctx.lang,
        ctx.remote,
        &format!("/alerts/abroad/set?on={}", u8::from(on)),
        &Phase::Ask(abroad_confirm(on, ctx.lang)),
    ))
}

async fn do_abroad(app: Shared, who: Caller, l: Lang, remote: bool, on: bool) -> String {
    let o = abroad_outcome(&app, &who, l, on).await;
    reread(&app);
    abroad_area(l, remote, "", &Phase::Done(o))
}

async fn abroad_outcome(app: &Shared, who: &Caller, l: Lang, on: bool) -> Outcome {
    let sent = crate::agent::put_raw(app, who, "/api/alerts/sms", &json!({"abroad": on})).await;
    if let Err(o) = taken(sent, l) {
        return o;
    }
    match read_alerts(app, who).await {
        Some(d) if d.sms.abroad_allowed == on => Outcome::Applied(
            if on {
                l.pick("Will also send abroad", "在国外也会发送")
            } else {
                l.pick("Will not send abroad", "在国外不发送")
            }
            .into(),
        ),
        _ => Outcome::Accepted,
    }
}

pub async fn abroad_set(
    State(app): State<Shared>,
    Query(q): Query<AbroadQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let (l, r) = (ctx.lang, ctx.remote);
    let who = Caller::of(&app, &req);
    write::respond(
        abroad_area(
            l,
            r,
            "",
            &Phase::Pending(l.pick("Saving…", "正在保存…").into()),
        ),
        do_abroad(app, who, l, r, q.on == 1),
    )
}

/// This page's routes (the page, its stream, its buttons).
pub fn routes() -> Router<Shared> {
    Router::new()
        .route(PATH, get(page))
        .route("/alerts/events", get(events))
        .route("/alerts/refresh", post(refresh))
        .route("/alerts/mark", post(mark_send))
        .route("/alerts/mark/cancel", post(mark_cancel))
        .route("/alerts/number/ask", post(number_ask))
        .route("/alerts/number/set", post(number_set))
        .route("/alerts/number/cancel", post(num_cancel))
        .route("/alerts/abroad/ask", post(abroad_ask))
        .route("/alerts/abroad/set", post(abroad_set))
        .route("/alerts/abroad/cancel", post(abroad_cancel))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        json!({
            "events": [
                {"seq": 3, "time": null, "uptime": 125, "kind": "datad-crash", "label": "数据服务意外退出", "label_en": "Data service exited", "text": "exit 1", "unread": true},
                {"seq": 2, "time": 1_791_000_000, "uptime": 99, "kind": "wifi-takeover", "label": "Wi-Fi 看门狗", "label_en": "Wi-Fi watchdog", "text": "", "unread": false},
            ],
            "unread": 1, "read": 2, "clock_set": false,
            "sms": {"configured": true, "number": "+8612300000000", "abroad_allowed": false,
                    "abroad_now": true, "sent_24h": 2,
                    "recent": [{"time": 1_791_000_000, "seq": 2, "kind": "wifi-takeover", "result": "suppressed-abroad"},
                               {"time": 5, "seq": 3, "kind": "datad-crash", "result": "sent"}]},
        })
    }

    fn poll(l: Lang, data: Option<Data>, fail: Option<Fail>) -> Poll {
        let mut p = Poll::new(
            Caller {
                token: Some(Some("t".into())),
                peer: None,
                sid: None,
            },
            l,
        );
        p.data = data;
        p.fail = fail;
        p
    }

    fn leaf(p: &Poll, id: &str) -> String {
        p.leaves().into_iter().find(|(k, _)| k == id).unwrap().1
    }

    #[test]
    fn the_device_clock_digits_are_the_local_ones() {
        // 2026-10-13 15:04 (as epoch digits)
        assert_eq!(mdhm(1_791_990_240), "10-14 15:04");
        assert_eq!(mdhm(0), "01-01 00:00");
        assert_eq!(mdhm(86_400 * 59 + 3600 * 7 + 60 * 9), "03-01 07:09");
    }

    #[test]
    fn numbers_like_the_agent() {
        assert_eq!(number_norm(" +86 123-456 "), "+86123456");
        for ok in ["", "+8612300000000", "123", "+123"] {
            assert!(number_ok(ok), "{ok}");
        }
        for bad in ["12", "+12", "abc", "+86 1", "123456789012345678901", "1+23"] {
            assert!(!number_ok(bad), "{bad}");
        }
    }

    #[test]
    fn unread_alerts_warn_and_offer_the_mark() {
        let p = poll(Lang::En, Some(parse(&sample())), None);
        let s = leaf(&p, "al-status");
        assert!(
            s.contains("nd-status--warn") && s.contains("1 new alert(s)"),
            "{s}"
        );
        assert!(
            s.contains("Data service exited") && s.contains("Mark all as read"),
            "{s}"
        );
        assert!(
            s.contains("data-write=\"al-mark\"") && s.contains("retryMaxCount: 0"),
            "{s}"
        );
        let e = leaf(&p, "al-events");
        assert!(
            e.contains("2 min after boot") && e.contains("10-03 04:00"),
            "{e}"
        );
        assert!(e.contains("device clock is not set"), "{e}");
        assert!(
            !e.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
            "{e}"
        );
    }

    #[test]
    fn nothing_unread_has_no_button_and_a_dead_agent_has_retry() {
        let mut d = parse(&sample());
        d.unread = 0;
        let s = leaf(&poll(Lang::Zh, Some(d), None), "al-status");
        assert!(
            s.contains("没有新告警") && !s.contains("全部标为已读"),
            "{s}"
        );
        let s = leaf(&poll(Lang::Zh, None, Some(Fail::Unreachable)), "al-status");
        assert!(
            s.contains("读不到告警") && s.contains("设备服务没有回应") && s.contains("/alerts/refresh"),
            "{s}"
        );
    }

    #[test]
    fn an_older_answer_is_stale_and_cannot_be_edited() {
        let mut p = poll(Lang::En, Some(parse(&sample())), Some(Fail::Unreachable));
        p.at = Some(SystemTime::now());
        let s = leaf(&p, "al-status");
        assert!(
            s.contains("nd-status--stale") && s.contains("stopped at"),
            "{s}"
        );
        assert!(s.contains("disabled"), "{s}");
        assert!(leaf(&p, "al-numform").contains("disabled"));
        assert!(leaf(&p, "al-abroad").contains("disabled"));
        assert!(leaf(&p, "al-events").contains("Couldn't refresh the list"));
    }

    #[test]
    fn sms_part_shows_number_abroad_and_results() {
        let p = poll(Lang::En, Some(parse(&sample())), None);
        let st = leaf(&p, "al-smsstate");
        assert!(
            st.contains("sent from this device's SIM to +8612300000000"),
            "{st}"
        );
        let a = leaf(&p, "al-abroad");
        assert!(
            a.contains("aria-checked=\"false\"") && a.contains("held back right now"),
            "{a}"
        );
        assert!(a.contains("abroad/ask?on=1"), "{a}");
        let r = leaf(&p, "al-recent");
        assert!(
            r.contains("2 sent in the last 24 h") && r.contains("Not sent — abroad"),
            "{r}"
        );
        // a record before the clock was set shows "—"; its label borrows the event's of that kind
        assert!(
            r.contains("Data service exited") && r.contains(">—<"),
            "{r}"
        );
        let f = leaf(&p, "al-numform");
        assert!(
            f.contains("_aln: '+8612300000000'") && f.contains("/alerts/number/ask?n="),
            "{f}"
        );
    }

    #[test]
    fn the_confirms_say_what_changes() {
        let c = number_confirm("", Lang::En);
        assert!(c.what.contains("No more alert SMS"));
        let c = number_confirm("+8612300000000", Lang::Zh);
        assert!(c.what.contains("+8612300000000") && c.action.contains("保存号码"));
        assert!(abroad_confirm(true, Lang::En).what.contains("roaming fee"));
        assert!(abroad_confirm(false, Lang::Zh).what.contains("不发短信"));
        assert_eq!(num_set_url("+86123"), "/alerts/number/set?n=%2B86123");
    }
}
