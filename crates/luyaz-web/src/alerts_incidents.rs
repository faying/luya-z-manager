//! /alerts 「事故 · 7 天」 (design 2026-10-24 「變差才通知」; design review 1A/2A/5A/6A/10A/12A/13A,
//! eng review round 3 D2/D3/D6/D7/D8/D10): the core's incidents (`/v2/incidents`, core ≥ 1.45.0)
//! above the event list. Open ones first, then those that ended in the last 7 days (newest first,
//! at most 50). A row opens its details (`/v2/incidents/<id>`, read when opened, kept while the
//! list refreshes). An alert of an incident drawn here leaves 「记录」 (its details list it); an
//! incident not drawn (past 50, older than 7 days) or no incident list at all leaves 「记录」 whole.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::agent::{Fail, esc};
use crate::i18n::Lang;

/// Ended incidents drawn (design review 6A).
pub const CLOSED_SHOWN: usize = 50;
/// An opened incident's details are kept this long, so the list's refresh draws them again
/// instead of "reading…" (DESIGN §5 DR12: open details are not overwritten by a refresh).
const KEEP_DETAIL: Duration = Duration::from_secs(600);

fn details() -> &'static Mutex<HashMap<(String, &'static str), (Instant, String)>> {
    static D: std::sync::OnceLock<Mutex<HashMap<(String, &'static str), (Instant, String)>>> =
        std::sync::OnceLock::new();
    D.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lang_key(l: Lang) -> &'static str {
    l.pick("en", "zh")
}

/// Keep the inner HTML of an opened incident's details.
pub fn keep_detail(id: &str, l: Lang, inner: &str) {
    let mut d = details().lock().unwrap_or_else(|e| e.into_inner());
    d.retain(|_, (t, _)| t.elapsed() < KEEP_DETAIL);
    d.insert((id.to_string(), lang_key(l)), (Instant::now(), inner.to_string()));
}

fn kept_detail(id: &str, l: Lang) -> Option<String> {
    let d = details().lock().unwrap_or_else(|e| e.into_inner());
    d.get(&(id.to_string(), lang_key(l)))
        .filter(|(t, _)| t.elapsed() < KEEP_DETAIL)
        .map(|(_, h)| h.clone())
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Inc {
    pub id: String,
    pub kind: String,
    pub name_zh: String,
    pub name_en: String,
    pub sub: Option<String>,
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub closed: bool,
    pub end: Option<String>,
    pub count: u64,
    pub observed: Option<(String, String)>,
    /// (component, seconds from the ship to the start) when the core says may be related.
    pub ship: Option<(String, i64)>,
    pub ship_uncertain: bool,
    /// (the switch, seconds from the drill to the start; None across a reboot) when the core says
    /// a drill may be related.
    pub drill: Option<(String, Option<i64>)>,
    pub unread: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct List {
    pub open: Vec<Inc>,
    pub closed: Vec<Inc>,
    pub gap: bool,
}

/// How the last read went.
#[derive(Clone, Debug, PartialEq)]
pub enum Got {
    List(List),
    /// 404: a core without incidents.
    Old,
    /// The core answered `{error}` (the layer has not run).
    Error(String),
}

fn s(v: &Value) -> Option<String> {
    v.as_str().filter(|x| !x.is_empty()).map(Into::into)
}

fn one(v: &Value) -> Inc {
    let ship = &v["ship"];
    let clock = v["ship_clock"].as_str().unwrap_or("");
    Inc {
        id: v["id"].as_str().unwrap_or("").into(),
        kind: v["kind"].as_str().unwrap_or("").into(),
        name_zh: s(&v["name_zh"]).unwrap_or_else(|| "设备异常".into()),
        name_en: s(&v["name_en"]).unwrap_or_else(|| "Device issue".into()),
        sub: s(&v["sub"]),
        from: v["from"].as_i64(),
        to: v["to"].as_i64(),
        closed: v["closed"].as_bool().unwrap_or(!v["to"].is_null()),
        end: s(&v["end"]),
        count: v["count"].as_u64().unwrap_or(1),
        observed: s(&v["observed"]["zh"]).map(|z| (z, s(&v["observed"]["en"]).unwrap_or_default())),
        ship: (clock == "same_boot")
            .then(|| Some((s(&ship["comp"])?, ship["ago_s"].as_i64()?)))
            .flatten(),
        ship_uncertain: clock == "uncertain",
        drill: s(&v["drill"]["what"]).map(|w| {
            let same = v["drill"]["clock"] == "same_boot";
            (w, v["drill"]["ago_s"].as_i64().filter(|_| same))
        }),
        unread: v["unread"].as_bool().unwrap_or(false),
    }
}

pub fn parse(v: &Value) -> Got {
    if let Some(e) = s(&v["error"]) {
        return Got::Error(e);
    }
    let list = |k: &str| {
        v[k].as_array()
            .map(|a| a.iter().map(one).filter(|i| !i.id.is_empty()).collect())
            .unwrap_or_default()
    };
    Got::List(List {
        open: list("open"),
        closed: list("closed"),
        gap: v["ingest_gap"].as_bool().unwrap_or(false),
    })
}

/// A read's failure: 404 is an older core (no incidents), anything else cannot-read.
pub fn failed(f: &Fail) -> Option<Got> {
    matches!(f, Fail::Refused(404, ..)).then_some(Got::Old)
}

fn name(i: &Inc, l: Lang) -> &str {
    match l {
        Lang::En => &i.name_en,
        Lang::Zh => &i.name_zh,
    }
}

/// "2h 10m" / "17 min" / "3 d 4 h".
pub fn dur(s: i64, l: Lang) -> String {
    let s = s.max(0);
    let (d, h, m) = (s / 86_400, s % 86_400 / 3600, s % 3600 / 60);
    match l {
        Lang::En if d > 0 => format!("{d} d {h} h"),
        Lang::Zh if d > 0 => format!("{d} 天 {h} 小时"),
        _ if h > 0 => format!("{h}h {m}m"),
        Lang::En => format!("{m} min"),
        Lang::Zh => format!("{m} 分钟"),
    }
}

fn hhmm(t: i64) -> String {
    let r = t.rem_euclid(86_400);
    format!("{:02}:{:02}", r / 3600, r / 60 % 60)
}

/// The state word and its tone (design review 12A; round 3 D8): open ▲ warn, recovered ● ok,
/// ended quiet ● ok with the count, cut by a reboot and "can't watch" ● neutral.
pub fn state(i: &Inc, l: Lang) -> (&'static str, String) {
    if !i.closed {
        let tone = if i.kind == "regress-blind" { "neutral" } else { "warn" };
        return (tone, l.pick("still open", "还开着").into());
    }
    match i.end.as_deref() {
        Some("quiet") => (
            "ok",
            match l {
                Lang::En => format!("Ended · {}×", i.count),
                Lang::Zh => format!("已结束 · {} 次", i.count),
            },
        ),
        Some("reboot") => ("neutral", l.pick("cut by a reboot", "因重启中断").into()),
        _ => ("ok", l.pick("Recovered", "已恢复").into()),
    }
}

/// The second line: when, how long, and a ship that may be related.
fn when(i: &Inc, now: i64, l: Lang) -> String {
    let unknown = l.pick("time not trusted", "时间不可信");
    let mut out = match (i.from, i.to, i.closed) {
        (Some(f), _, false) => match l {
            Lang::En => format!("since {} · {}", crate::alerts_page::mdhm(f), dur(now - f, l)),
            Lang::Zh => format!("{} 起 · 已 {}", crate::alerts_page::mdhm(f), dur(now - f, l)),
        },
        (Some(f), Some(t), true) => {
            format!("{}–{} · {}", crate::alerts_page::mdhm(f), hhmm(t), dur(t - f, l))
        }
        _ => unknown.into(),
    };
    if let Some((comp, ago)) = &i.ship
        && *ago <= 3600
    {
        out.push_str(&match l {
            Lang::En => format!(" · may be related: {comp} ship"),
            Lang::Zh => format!(" · 可能相关：{comp} 上机"),
        });
    }
    if matches!(&i.drill, Some((_, Some(ago))) if *ago <= 3600) {
        out.push_str(l.pick(" · may be related: a drill", " · 可能相关：演练"));
    }
    out
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

fn unread_mark(l: Lang) -> String {
    format!(
        "<span class=\"al-unread\"><span aria-hidden=\"true\">▲ </span><span class=\"sr-only\">{}: </span></span>",
        l.pick("unread", "未读")
    )
}

/// The ids drawn this time (their alerts leave 「记录」).
pub fn drawn(list: &List) -> HashSet<String> {
    list.open
        .iter()
        .chain(list.closed.iter().take(CLOSED_SHOWN))
        .map(|i| i.id.clone())
        .collect()
}

/// Only ids the core could have made reach an attribute or a URL.
fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn row(i: &Inc, now: i64, l: Lang) -> String {
    let (tone, st) = state(i, l);
    let head = format!(
        "<span class=\"nd-row__label\">{}{}</span>",
        if i.unread { unread_mark(l) } else { String::new() },
        mark(tone, &format!("{} · {st}", name(i, l)))
    );
    let sub = format!("<span class=\"nd-row__sub block al-wrap\">{}</span>", esc(&when(i, now, l)));
    if !safe_id(&i.id) {
        return format!("<li class=\"nd-row nd-row--two al-item\"><span class=\"nd-row__text\">{head}{sub}</span></li>");
    }
    let id = &i.id;
    let open = format!("$_alinc === '{id}'");
    let load = format!("@post('/alerts/incident?id={id}')");
    let (show, hide) = match l {
        Lang::En => (format!("Show details: {}", name(i, l)), format!("Hide details: {}", name(i, l))),
        Lang::Zh => (format!("查看详情：{}", name(i, l)), format!("收起详情：{}", name(i, l))),
    };
    format!(
        "<li class=\"al-inc\"><button type=\"button\" class=\"nd-row nd-row--two al-inc__row\" aria-expanded=\"false\" aria-controls=\"al-ip-{id}\" aria-label=\"{}\" data-attr:aria-expanded=\"{open} ? 'true' : 'false'\" data-attr:aria-label=\"{open} ? '{}' : '{}'\" data-on:click=\"$_alinc = {open} ? '' : '{id}'; {open} &amp;&amp; {load}\"><span class=\"nd-row__text\">{head}{sub}</span><span class=\"nd-aux al-chev\" aria-hidden=\"true\">›</span></button>\
<div id=\"al-ip-{id}\" class=\"al-panel\" style=\"display:none\" data-show=\"{open}\"><div id=\"al-it-{id}\" class=\"al-detail\">{}</div></div></li>",
        esc(&show),
        esc(&hide).replace('\'', "&#39;"),
        esc(&show).replace('\'', "&#39;"),
        kept_detail(id, l).unwrap_or_else(|| format!(
            "<p class=\"nd-aux\" role=\"status\">{}</p>",
            l.pick("Reading…", "正在读取…")
        )),
    )
}

/// The section's body. `stale_at`: the list shown is an older one (the last read failed).
pub fn leaf(
    got: Option<&Got>,
    fail: Option<&Fail>,
    stale_at: Option<std::time::SystemTime>,
    now: i64,
    l: Lang,
) -> String {
    let retry = format!(
        "<button type=\"button\" class=\"nd-btn nd-btn--secondary nd-btn--sm\" data-on:click=\"@post('/alerts/refresh')\">{}</button>",
        l.pick("Retry", "重试")
    );
    let cant = |why: &str| {
        format!(
            "<p class=\"al-note\" role=\"alert\">{} {retry} <a class=\"nd-link\" href=\"/health\">{}</a></p>",
            mark("bad", &format!("{}{why}", l.pick("Can't read incidents: ", "读不到事故："))),
            l.pick("Health", "健康")
        )
    };
    match got {
        None if fail.is_none() => {
            "<div class=\"nd-group\"><div class=\"nd-row\"><span class=\"nd-skel\" style=\"width:22ch\"></span></div></div>".into()
        }
        None => cant(l.pick("the core does not answer", "core 没回应")),
        Some(Got::Old) => format!(
            "<p class=\"nd-body al-note\">{}</p>",
            mark(
                "neutral",
                l.pick("The core is too old for incidents.", "core 版本太旧，不支持事故")
            )
        ),
        Some(Got::Error(e)) => cant(e),
        Some(Got::List(list)) => {
            let mut out = String::new();
            if stale_at.is_some() {
                out.push_str(&format!(
                    "<p class=\"al-note\" role=\"alert\">{} {retry}</p>",
                    mark(
                        "warn",
                        &crate::agent::stopped(l, "Incidents", "事故列表", stale_at)
                    )
                ));
            }
            if list.open.is_empty() && list.closed.is_empty() {
                out.push_str(&format!(
                    "<p class=\"nd-body al-note\">{}<span class=\"nd-aux block\">{}</span></p>",
                    mark("ok", l.pick("No incidents in 7 days", "7 天内没有事故")),
                    l.pick(
                        "If a kind of check goes blind, that is said separately.",
                        "某一类看不见时会另外通知"
                    )
                ));
            } else {
                out.push_str(&format!(
                    "<ul class=\"nd-group al-list{}\" data-signals__ifmissing=\"{{_alinc: ''}}\">",
                    if stale_at.is_some() { " nd-stale" } else { "" }
                ));
                for i in list.open.iter().chain(list.closed.iter().take(CLOSED_SHOWN)) {
                    out.push_str(&row(i, now, l));
                }
                out.push_str("</ul>");
                let more = list.closed.len().saturating_sub(CLOSED_SHOWN);
                if more > 0 {
                    out.push_str(&format!(
                        "<p class=\"nd-aux al-note\">{}</p>",
                        match l {
                            Lang::En => format!("{more} more — ask through MCP or tools/luyaz"),
                            Lang::Zh => format!("还有 {more} 条，可用 MCP 或 tools/luyaz 查"),
                        }
                    ));
                }
            }
            if list.gap {
                out.push_str(&format!(
                    "<p class=\"nd-aux al-note\">{}</p>",
                    l.pick(
                        "Some alerts were trimmed before the core read them; incidents may miss them.",
                        "有一段告警没被读到就被剪掉了，事故里可能缺这几条。"
                    )
                ));
            }
            out
        }
    }
}

/// The status block's word when incidents are open (design review IA): "N still open" and the
/// newest one.
pub fn status(list: &List, l: Lang) -> Option<(String, String)> {
    let first = list.open.first()?;
    let n = list.open.len();
    let state = match l {
        Lang::En => format!("{n} incident(s) still open"),
        Lang::Zh => format!("{n} 个事故还开着"),
    };
    let since = first
        .from
        .map(|f| match l {
            Lang::En => format!(" · since {}", hhmm(f)),
            Lang::Zh => format!(" · {} 起", hhmm(f)),
        })
        .unwrap_or_default();
    let more = if n > 1 { format!(" (+{})", n - 1) } else { String::new() };
    Some((state, format!("{}{since}{more}", name(first, l))))
}

/// `/alerts/incident?id=`: the details of one (design review 10A), read when it is opened.
pub fn detail(id: &str, got: Result<Value, Fail>, now: i64, l: Lang) -> String {
    let wrap = |inner: String| {
        keep_detail(id, l, &inner);
        format!("<div id=\"al-it-{id}\" class=\"al-detail\">{inner}</div>")
    };
    let v = match got {
        Ok(v) => v,
        Err(Fail::Refused(404, ..)) => {
            return wrap(format!(
                "<p class=\"nd-aux\">{}</p>",
                l.pick("These details are past their keeping time.", "这条详情已超过保存期限")
            ));
        }
        Err(f) => {
            return wrap(format!(
                "<p class=\"nd-aux\" role=\"alert\">{} · {} <button type=\"button\" class=\"nd-btn nd-btn--secondary nd-btn--sm\" data-on:click=\"@post('/alerts/incident?id={id}')\">{}</button></p>",
                l.pick("Can't read the details", "读不到详情"),
                esc(&f.text(l)),
                l.pick("Retry", "重试")
            ));
        }
    };
    let i = one(&v);
    let mut out = String::from("<dl class=\"al-dl\">");
    let item = |out: &mut String, k: &str, v: &str| {
        out.push_str(&format!("<dt>{}</dt><dd>{v}</dd>", esc(k)));
    };
    let (tone, st) = state(&i, l);
    let long = match l {
        Lang::En => s(&v["label_en"]),
        Lang::Zh => s(&v["label"]),
    };
    let mut first = mark(tone, &st);
    if let Some(t) = long {
        first.push_str(&format!("<span class=\"nd-row__sub block\">{}</span>", esc(&t)));
    }
    if i.end.as_deref() == Some("quiet") {
        let w = if i.kind == "regress-reboot" {
            l.pick("none again within 6 hours", "6 小时内没再发生")
        } else {
            l.pick("none again within an hour", "1 小时内没再发生")
        };
        first.push_str(&format!("<span class=\"nd-row__sub block\">{w}</span>"));
    }
    item(&mut out, l.pick("State", "状态"), &first);
    if let Some((zh, en)) = &i.observed {
        let t = match l {
            Lang::En if !en.is_empty() => en,
            _ => zh,
        };
        item(&mut out, l.pick("Seen at the same time", "同时观察到"), &esc(t));
    }
    let when = |t: Option<i64>| t.map_or_else(|| l.pick("time not trusted", "时间不可信").to_string(), crate::alerts_page::mdhm);
    item(&mut out, l.pick("Started", "开始"), &format!("<span class=\"nd-mono\">{}</span>", esc(&when(i.from))));
    if i.closed {
        item(
            &mut out,
            l.pick("Ended", "结束"),
            &format!("<span class=\"nd-mono\">{}</span>", esc(&when(i.to))),
        );
    }
    if let Some(f) = i.from {
        let d = i.to.unwrap_or(now) - f;
        item(&mut out, l.pick("Lasted", "持续"), &esc(&dur(d, l)));
    }
    if let Some((comp, ago)) = &i.ship {
        item(
            &mut out,
            l.pick("A ship that may be related", "可能相关的上机"),
            &esc(&match l {
                Lang::En => format!("{comp} shipped {} min before it started", ago / 60),
                Lang::Zh => format!("开始前 {} min 上机 {comp}", ago / 60),
            }),
        );
    } else if i.ship_uncertain {
        item(
            &mut out,
            l.pick("A ship that may be related", "可能相关的上机"),
            &esc(&match l {
                Lang::En => format!(
                    "{} shipped across a reboot: the clocks cannot tell which came first",
                    s(&v["ship"]["comp"]).unwrap_or_default()
                ),
                Lang::Zh => format!(
                    "{} 上机时间不确定，分不出先后",
                    s(&v["ship"]["comp"]).unwrap_or_default()
                ),
            }),
        );
    }
    if let Some((what, ago)) = &i.drill {
        item(
            &mut out,
            l.pick("A drill that may be related", "可能相关的演练"),
            &esc(&match (l, ago) {
                (Lang::En, Some(a)) => format!("test switch {what} on {} min before it started", a / 60),
                (Lang::Zh, Some(a)) => format!("开始前 {} min 开了测试开关 {what}", a / 60),
                (Lang::En, None) => format!("test switch {what} across a reboot: the clocks cannot tell which came first"),
                (Lang::Zh, None) => format!("测试开关 {what} 在重启前，分不出先后"),
            }),
        );
    }
    if let Some(m) = s(&v["memsnap"]) {
        item(
            &mut out,
            l.pick("Memory snapshot", "内存快照"),
            &format!("<pre class=\"nd-mono al-pre\">{}</pre>", esc(&m)),
        );
    }
    let detail = s(&v["detail"]).unwrap_or_default();
    item(
        &mut out,
        l.pick("Build · id", "build · id"),
        &format!(
            "<span class=\"nd-mono nd-aux\">{} · {}{}</span>",
            esc(s(&v["build"]).as_deref().unwrap_or("-")),
            esc(&i.id),
            if detail.is_empty() { String::new() } else { format!(" · {}", esc(&detail)) }
        ),
    );
    out.push_str("</dl>");
    if let Some(a) = v["alerts"].as_array().filter(|a| !a.is_empty()) {
        out.push_str(&format!(
            "<h3 class=\"nd-group-title al-sub\">{}</h3><ul class=\"al-list\">",
            l.pick("Its alerts", "归入的告警")
        ));
        for e in a {
            let unread = e["unread"].as_bool().unwrap_or(false);
            let t = e["time"].as_i64().map_or("—".to_string(), crate::alerts_page::mdhm);
            out.push_str(&format!(
                "<li class=\"nd-row al-item\"><span class=\"nd-row__text\"><span class=\"nd-row__label\">{}{}</span><span class=\"nd-row__sub al-wrap\">{}</span></span><span class=\"al-time nd-mono\">{}</span></li>",
                if unread { unread_mark(l) } else { String::new() },
                esc(e["kind"].as_str().unwrap_or("")),
                esc(e["text"].as_str().unwrap_or("")),
                esc(&t)
            ));
        }
        out.push_str("</ul>");
        if let Some(more) = v["alert_more"].as_u64().filter(|m| *m > 0) {
            out.push_str(&format!(
                "<p class=\"nd-aux\">{}</p>",
                match l {
                    Lang::En => format!("and {more} more"),
                    Lang::Zh => format!("另有 {more} 条"),
                }
            ));
        }
    }
    wrap(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn inc(id: &str, closed: bool, end: Option<&str>) -> Value {
        json!({"id": id, "kind": "regress-mem", "name_zh": "内存超标", "name_en": "High memory",
               "from": 1_792_800_000, "to": if closed { json!(1_792_801_020) } else { Value::Null },
               "closed": closed, "end": end, "count": 3, "unread": false,
               "ship": {"comp": "core", "txn": "t", "ago_s": 600}, "ship_clock": "same_boot"})
    }

    /// Design §4 「前 60 分鐘 ship/演練」: a drill in the same boot is named on the row and in the
    /// details; across a reboot only in the details, as uncertain.
    #[test]
    fn a_drill_that_may_be_related() {
        let mut v = inc("a", false, None);
        v["drill"] = json!({"what": "rx-quiet", "ago_s": 300, "clock": "same_boot"});
        let i = one(&v);
        let now = 1_792_800_000 + 600;
        assert!(when(&i, now, Lang::Zh).ends_with("可能相关：core 上机 · 可能相关：演练"));
        assert!(when(&i, now, Lang::En).ends_with("may be related: a drill"));
        let d = detail("a", Ok(v.clone()), now, Lang::Zh);
        assert!(d.contains("可能相关的演练") && d.contains("开始前 5 min 开了测试开关 rx-quiet"), "{d}");
        v["drill"] = json!({"what": "rx-quiet", "ago_s": null, "clock": "uncertain"});
        let i = one(&v);
        assert!(!when(&i, now, Lang::Zh).contains("演练"));
        let d = detail("a", Ok(v), now, Lang::Zh);
        assert!(d.contains("测试开关 rx-quiet 在重启前，分不出先后"), "{d}");
    }

    #[test]
    fn states_and_lines_as_designed() {
        let Got::List(l) = parse(&json!({"v": 1, "open": [inc("a", false, None)],
            "closed": [inc("b", true, Some("recovered")), inc("c", true, Some("quiet"))]})) else {
            panic!()
        };
        assert_eq!(state(&l.open[0], Lang::Zh), ("warn", "还开着".to_string()));
        assert_eq!(state(&l.closed[0], Lang::Zh).1, "已恢复");
        assert_eq!(state(&l.closed[1], Lang::Zh).1, "已结束 · 3 次");
        assert_eq!(state(&l.closed[1], Lang::En).1, "Ended · 3×");
        let now = 1_792_800_000 + 7800;
        let f = crate::alerts_page::mdhm(1_792_800_000);
        assert_eq!(when(&l.open[0], now, Lang::Zh), format!("{f} 起 · 已 2h 10m · 可能相关：core 上机"));
        assert_eq!(when(&l.closed[0], now, Lang::Zh), format!("{f}–00:17 · 17 分钟 · 可能相关：core 上机"));
        let html = leaf(Some(&Got::List(l.clone())), None, None, now, Lang::Zh);
        assert!(html.contains("aria-expanded=\"false\"") && html.contains("aria-controls=\"al-ip-a\""));
        assert!(html.contains("id=\"al-it-a\""));
        // details read once are drawn again by the next refresh (not "reading…")
        keep_detail("a", Lang::Zh, "<p>kept</p>");
        assert!(leaf(Some(&Got::List(l.clone())), None, None, now, Lang::Zh).contains("<p>kept</p>"));
        assert!(!leaf(Some(&Got::List(l.clone())), None, None, now, Lang::En).contains("<p>kept</p>"));
        assert_eq!(drawn(&l).len(), 3);
        assert_eq!(status(&l, Lang::Zh), Some(("1 个事故还开着".into(), "内存超标 · 00:00 起".into())));
    }

    #[test]
    fn empty_old_error_stale_and_more_than_fifty() {
        let now = 1_792_800_000;
        let e = leaf(Some(&parse(&json!({"v":1,"open":[],"closed":[]}))), None, None, now, Lang::Zh);
        assert!(e.contains("7 天内没有事故") && e.contains("某一类看不见时会另外通知"));
        assert!(leaf(Some(&Got::Old), None, None, now, Lang::Zh).contains("core 版本太旧，不支持事故"));
        let err = leaf(Some(&parse(&json!({"v":1,"open":[],"closed":[],"error":"not yet"}))), None, None, now, Lang::Zh);
        assert!(err.contains("读不到事故：not yet") && err.contains("重试"));
        assert!(leaf(None, Some(&Fail::Unreachable), None, now, Lang::Zh).contains("读不到事故：core 没回应"));
        assert_eq!(failed(&Fail::Refused(404, String::new(), String::new())), Some(Got::Old));
        let many: Vec<Value> = (0..53).map(|k| inc(&format!("q{k}"), true, Some("recovered"))).collect();
        let g = parse(&json!({"v":1,"open":[inc("x", false, None)],"closed": many}));
        let Got::List(l) = &g else { panic!() };
        let h = leaf(Some(&g), None, Some(std::time::SystemTime::now()), now, Lang::Zh);
        assert!(h.contains("还有 3 条，可用 MCP 或 tools/luyaz 查"));
        assert!(h.contains("nd-stale"));
        assert_eq!(drawn(l).len(), 51, "the open one and 50 ended");
        assert!(!drawn(l).contains("q52"));
    }

    #[test]
    fn details_in_order_and_their_failures() {
        let mut v = inc("a", true, Some("quiet"));
        v["kind"] = json!("core-crash");
        v["label"] = json!("数据服务意外退出，已自动重启");
        v["observed"] = json!({"zh": "原厂动作：redial", "en": "stock firmware: redial"});
        v["memsnap"] = json!("Rss: 1 kB");
        v["build"] = json!("880e05c");
        v["alerts"] = json!([{"seq": 7, "time": 1_792_800_000, "kind": "core-crash", "text": "x", "unread": true}]);
        let h = detail("a", Ok(v), 1_792_802_000, Lang::Zh);
        let pos = |s: &str| h.find(s).unwrap_or_else(|| panic!("{s} in {h}"));
        assert!(pos("状态") < pos("同时观察到") && pos("同时观察到") < pos("开始") && pos("开始") < pos("可能相关的上机"));
        assert!(pos("可能相关的上机") < pos("内存快照") && pos("内存快照") < pos("build") && pos("build") < pos("归入的告警"));
        assert!(h.contains("1 小时内没再发生") && h.contains("数据服务意外退出") && h.contains("开始前 10 min 上机 core"));
        assert!(h.contains("al-unread"));
        assert!(detail("a", Err(Fail::Refused(404, String::new(), String::new())), 0, Lang::Zh).contains("这条详情已超过保存期限"));
        assert!(detail("a", Err(Fail::Unreachable), 0, Lang::Zh).contains("读不到详情"));
    }
}
