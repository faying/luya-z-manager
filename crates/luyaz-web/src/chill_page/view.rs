//! /services/chill: the page's live parts as HTML, one leaf per block (`split_leaves` takes the
//! first `id="` of a leaf as its id, so a leaf's root element carries it first). Every leaf is
//! always rendered, empty when the state hides it, so a push always has something to replace.
//!
//! Device text (node and provider names, hostnames, log lines, the agent's notices) is escaped
//! and marked `data-data`. Names never go into a URL, an expression or an `aria-label`: buttons
//! carry a position and a [`tag`] and take their accessible name from the name's own element.

use std::time::SystemTime;

use serde_json::Value;

use super::data::*;
use crate::topbar::Via;
use crate::agent::{Fail, bytes, esc, stopped};
use crate::i18n::Lang;
use crate::sms::symbol;
use crate::write::post_once;

/// What one source last gave: the value (kept when a later read fails), the last failure, when.
#[derive(Default)]
pub struct Slot {
    pub v: Option<Value>,
    pub err: Option<Fail>,
    pub at: Option<SystemTime>,
}

impl Slot {
    pub fn stale(&self) -> bool {
        self.v.is_some() && self.err.is_some()
    }
    pub fn set(&mut self, r: Result<Value, Fail>) {
        match r {
            Ok(v) => {
                self.v = Some(v);
                self.err = None;
                self.at = Some(SystemTime::now());
            }
            Err(e) => self.err = Some(e),
        }
    }
}

#[derive(Default)]
pub struct Got {
    pub status: Slot,
    pub home: Slot,
    pub prov: Slot,
    pub bypass: Slot,
    pub clients: Slot,
    pub log: Slot,
}

fn mark(tone: &str, text: &str) -> String {
    format!(
        "<span class=\"nd-mark nd-mark--{tone}\"><span class=\"nd-mark__sym\" aria-hidden=\"true\">{}</span><span>{text}</span></span>",
        symbol(tone)
    )
}

fn stale_cls(stale: bool) -> &'static str {
    if stale { " nd-stale" } else { "" }
}

fn hidden(id: &str) -> String {
    format!("<div id=\"{id}\"></div>")
}

fn title(l: Lang, id: &str, en: &'static str, zh: &'static str) -> String {
    format!("<h2 id=\"{id}\" class=\"nd-group-title\">{}</h2>", l.pick(en, zh))
}

fn aux(text: &str) -> String {
    format!("<p class=\"nd-aux px-1\">{text}</p>")
}

fn seg(label_id: &str, write: &str, items: &[(String, String, bool, String)]) -> String {
    // (text html, label for aria, selected, action)
    let mut h = format!("<div role=\"radiogroup\" aria-labelledby=\"{label_id}\" class=\"nd-seg nd-seg--block\"><div class=\"nd-seg__track\">");
    for (text, _, sel, act) in items {
        h += &format!(
            "<button type=\"button\" role=\"radio\" class=\"nd-seg__item\" aria-checked=\"{sel}\"{} data-write=\"{write}\" data-on:click=\"{act}\">{text}</button>",
            if *sel { " data-selected=\"true\"" } else { "" }
        );
    }
    h + "</div></div>"
}

pub fn leaves(g: &Got, l: Lang, tab: &str, now: i64) -> Vec<(String, String)> {
    let st = g.status.v.as_ref();
    let state = st.map_or("", |v| s(v, "state"));
    let running = state == "running";
    let known = st.is_some() && state != "unknown";
    vec![
        ("ch-status".into(), status(g, l)),
        ("ch-exit".into(), exit(g, l, running)),
        ("ch-home".into(), home(g, l, known, now)),
        ("ch-region".into(), pick(g, l, running, "main")),
        ("ch-ai".into(), pick(g, l, running, "ai")),
        ("ch-profile".into(), profile(g, l, known)),
        ("ch-prov".into(), prov(g, l, running, now)),
        ("ch-groups".into(), groups(g, l, running)),
        ("ch-byp".into(), bypass(g, l)),
        ("ch-vitals".into(), vitals(g, l, known, now)),
        ("ch-dash".into(), dash(l, running)),
        ("ch-log".into(), log(g, l, tab)),
    ]
}

// ---- status, start / stop ----

fn status(g: &Got, l: Lang) -> String {
    let slot = &g.status;
    let stale = slot.stale();
    let mut note = String::new();
    let (mut tone, word, reason, mut meta): (&str, String, String, String) = match (&slot.v, &slot.err) {
        (None, Some(_)) => ("bad", l.pick("Can't read CHILL status", "读不到 CHILL 状态").into(), String::new(), String::new()),
        (None, None) => ("neutral", l.pick("Loading", "加载中").into(), String::new(), String::new()),
        (Some(v), _) => match s(v, "state") {
            "running" => {
                let core_down = v["version"].is_null() && v["region"].is_null();
                let reason = if core_down {
                    l.pick("The proxy core is not answering", "代理核心没有回应").to_owned()
                } else {
                    s(&v["region"], "active").to_owned()
                };
                let meta = [
                    Some(s(v, "version")).filter(|x| !x.is_empty()).map(|x| format!("mihomo {x}")),
                    Some(s(v, "mode")).filter(|x| !x.is_empty()).map(|x| format!("mode {x}")),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
                let mf = &v["manual_first"];
                if !s(mf, "notice").is_empty() {
                    let text = if l == Lang::En && !s(mf, "notice_en").is_empty() { s(mf, "notice_en") } else { s(mf, "notice") };
                    let kind = s(mf, "notice_kind");
                    note = format!(
                        "<p class=\"ch-notice\" role=\"status\">{}{}</p>",
                        mark(notice_tone(kind, mf["on_backup"].as_bool() == Some(true)), &format!("<span data-data>{}</span>", esc(text))),
                        if kind == "alldown" {
                            format!(" <a class=\"t-link\" href=\"#chill-nodes\">{}</a>", l.pick("Pick a node →", "去选节点 →"))
                        } else {
                            String::new()
                        }
                    );
                }
                (if stale { "stale" } else { "ok" }, l.pick("Running", "运行中").into(), reason, meta)
            }
            "direct" => {
                let r = s(v, "reason");
                let reason = if r.is_empty() {
                    String::new()
                } else {
                    match l {
                        Lang::En => format!("Traffic is going direct: {}", reason(r, l)),
                        Lang::Zh => format!("当前直连,原因:{}", reason(r, l)),
                    }
                };
                ("warn", l.pick("Direct", "直连").into(), reason, String::new())
            }
            "stopped" => (
                "neutral",
                l.pick("Stopped", "已停止").into(),
                l.pick("CHILL is off; traffic goes out directly. Press Start to turn it on.", "CHILL 已关，流量直接出去。点上方「启动」打开。").into(),
                String::new(),
            ),
            _ => (
                "neutral",
                l.pick("Not started", "未启动").into(),
                l.pick(
                    "CHILL hasn't run since the device last booted. Press Start to bring it up.",
                    "设备本次开机后 CHILL 还没运行过。点上方「启动」拉起。",
                )
                .into(),
                String::new(),
            ),
        },
    };
    if stale {
        tone = if tone == "ok" { "stale" } else { tone };
        let said = stopped(l, "Status", "状态", slot.at);
        meta = if meta.is_empty() { said } else { format!("{meta} · {said}") };
    }
    // "direct" because it was switched off (or paused) by hand is off: offer Start, not Stop
    let on = slot.v.as_ref().is_some_and(|v| match s(v, "state") {
        "running" => true,
        "direct" => !matches!(s(v, "reason"), "disabled" | "paused"),
        _ => false,
    });
    let action = match (&slot.v, &slot.err) {
        (Some(_), _) => {
            let (cls, act, icon, label) = if on {
                ("secondary", "stop", "Power", l.pick("Stop", "停止"))
            } else {
                ("primary", "start", "Play", l.pick("Start", "启动"))
            };
            format!(
                "<div class=\"nd-status__actions\"><button type=\"button\" class=\"nd-btn nd-btn--{cls}\" data-on:click=\"@post('/services/chill/power/ask?act={act}')\">{}{label}</button></div>",
                crate::shell::icon(icon, !on, 20, "")
            )
        }
        (None, Some(_)) => format!(
            "<div class=\"nd-status__actions\"><button type=\"button\" class=\"nd-btn nd-btn--secondary nd-btn--sm\" data-on:click=\"@post('/services/chill/refresh')\">{}</button></div>",
            l.pick("Retry", "重试")
        ),
        _ => String::new(),
    };
    format!(
        "<div id=\"ch-status\">{note}<section class=\"nd-status nd-status--{tone}{}\"><div class=\"nd-status__main\"><div class=\"nd-status__state\" role=\"status\">{}</div>{}{}</div>{action}</section></div>",
        stale_cls(tone == "stale"),
        mark(tone, &esc(&word)),
        if reason.is_empty() { String::new() } else { format!("<div class=\"nd-status__reason\"><span data-data>{}</span></div>", esc(&reason)) },
        if meta.is_empty() { String::new() } else { format!("<div class=\"nd-status__meta\"><span class=\"nd-fresh\">{}</span></div>", esc(&meta)) },
    )
}

// ---- exit ----

fn exit(g: &Got, l: Lang, running: bool) -> String {
    let Some(v) = g.status.v.as_ref().filter(|_| running) else {
        return hidden("ch-exit");
    };
    let cur = s(v, "exit");
    let items: Vec<_> = EXITS
        .iter()
        .map(|x| {
            let act = if *x == "direct_all" {
                "@post('/services/chill/exit/ask')".to_owned()
            } else {
                post_once(&format!("/services/chill/exit/set?v={x}"))
            };
            (esc(exit_label(x, l)), String::new(), *x == cur, act)
        })
        .collect();
    format!(
        "<section id=\"ch-exit\" class=\"{}\" aria-labelledby=\"ch-exit-t\">{}<div class=\"nd-group grid gap-3 p-4\">{}{}{}</div></section>",
        stale_cls(g.status.stale()).trim(),
        title(l, "ch-exit-t", "Exit", "出口"),
        seg("ch-exit-t", "ch-ex", &items),
        if cur.is_empty() { String::new() } else { format!("<p class=\"nd-aux\">{}</p>", esc(exit_hint(cur, l))) },
        format_args!(
            "<p class=\"nd-aux\">{}</p>",
            l.pick(
                "Abroad on a local SIM the device switches to “Direct · AI stays” by itself, and back to Proxy once a home SIM is in again.",
                "在国外换了当地卡时，设备会自动切到「直连 · AI 不动」；换回国内卡后自动回到「代理」。"
            )
        ),
    )
}

// ---- home exit ----

fn switch(label_id: &str, on: bool, disabled: bool, click: &str) -> String {
    format!(
        "<button type=\"button\" role=\"switch\" class=\"nd-switch t-switch\" aria-labelledby=\"{label_id}\" aria-checked=\"{on}\"{}{} data-on:click=\"{click}\"><span class=\"nd-switch__track\"><svg class=\"nd-switch__check\" viewBox=\"0 0 12 12\" aria-hidden=\"true\"><path d=\"M2.5 6.2 5 8.6 9.5 3.6\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.8\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/></svg><span class=\"nd-switch__thumb\"></span></span></button>",
        if on { " data-selected=\"true\"" } else { "" },
        if disabled { " disabled data-disabled=\"true\"" } else { "" },
    )
}

fn line_html(x: &Line) -> String {
    if x.tone == "neutral" {
        format!("<span data-data>{}</span>", esc(&x.text))
    } else {
        mark(x.tone, &format!("<span data-data>{}</span>", esc(&x.text)))
    }
}

fn home(g: &Got, l: Lang, known: bool, now: i64) -> String {
    let Some(h) = g.home.v.as_ref().filter(|h| known && h["configured"].as_bool() == Some(true) && h["updating"].as_bool() != Some(true) && h["items"].is_object()) else {
        return hidden("ch-home");
    };
    let busy = h["busy"].is_object();
    let mut rows = String::new();
    for item in HOME_ITEMS.iter().filter(|i| home_shown(h, i)) {
        let on = h["items"][item]["on"].as_bool() == Some(true);
        let grey = h["blocked"].is_string() && !on;
        let no_sites = *item == "ip" && !on && h["items"]["ip"]["sites"].as_i64().unwrap_or(0) == 0;
        let row = home_row(h, item, now, l);
        let lid = format!("ch-hl-{item}");
        rows += &format!(
            "<div class=\"nd-row\"><span class=\"nd-row__text\"><span class=\"nd-row__label{}\" id=\"{lid}\">{}</span><span class=\"nd-row__sub block\" aria-live=\"polite\">{}</span></span>{}</div>",
            if grey { " ch-grey" } else { "" },
            esc(home_label(item, l)),
            line_html(&row),
            switch(&lid, on, busy || grey || no_sites, &format!("@post('/services/chill/home/ask?item={item}&on={}')", u8::from(!on))),
        );
    }
    let foot = home_foot(h, now, l);
    let foot_html = if foot.text.is_empty() {
        String::new()
    } else {
        format!(
            "<p class=\"ch-foot px-1\" aria-live=\"polite\">{}{}</p>",
            line_html(&foot),
            if foot.extend {
                format!(
                    " <button type=\"button\" class=\"nd-btn nd-btn--secondary nd-btn--sm\" data-write=\"ch-ho\" data-on:click=\"{}\">{}</button>",
                    post_once("/services/chill/home/extend"),
                    l.pick("Extend 2h", "再开 2 小时")
                )
            } else {
                String::new()
            }
        )
    };
    let reload = if h["busy"]["phase"].as_str() == Some("applying") {
        aux(l.pick("Switching; this page may drop for a few seconds.", "正在切换，网页可能断几秒。"))
    } else {
        String::new()
    };
    format!(
        "<section id=\"ch-home\" aria-labelledby=\"ch-home-t\">{}<div class=\"nd-group{}\">{rows}</div>{foot_html}{reload}</section>",
        title(l, "ch-home-t", "Home exit", "家里出口"),
        stale_cls(g.home.stale()),
    )
}

// ---- region / AI exit ----

fn pick(g: &Got, l: Lang, running: bool, which: &str) -> String {
    let (id, group_key, tid, area) = if which == "main" {
        ("ch-region", "region", "ch-region-t", "ch-rg")
    } else {
        ("ch-ai", "ai_exit", "ch-ai-t", "ch-ax")
    };
    let Some(v) = g.status.v.as_ref().filter(|_| running) else {
        return hidden(id);
    };
    let c = &v[group_key];
    let opts = strs(&c["options"]);
    let active = s(c, "active");
    let (head, desc) = if which == "main" {
        (title(l, tid, "Main exit region", "地区"), l.pick("Main exit for everything on the router.", "全屋默认出口。"))
    } else {
        (title(l, tid, "AI exit", "AI 出口"), l.pick("Exit used for AI-service traffic specifically.", "AI 服务专用出口,可与主出口不同。"))
    };
    let body = if opts.is_empty() {
        aux(l.pick("No members configured.", "没有配置任何成员。"))
    } else {
        let mut b = format!("<div role=\"radiogroup\" aria-labelledby=\"{tid}\" class=\"nd-choices\">");
        for (i, o) in opts.iter().enumerate() {
            let sel = o == active;
            b += &format!(
                "<button type=\"button\" role=\"radio\" class=\"nd-choice\" aria-checked=\"{sel}\"{} data-write=\"{area}\" data-on:click=\"{}\"><span data-data>{}</span></button>",
                if sel { " data-selected=\"true\"" } else { "" },
                post_once(&format!("/services/chill/pick/set?g={which}&i={i}&h={}", tag(o))),
                esc(o)
            );
        }
        b + "</div>"
    };
    let anchor = if which == "main" { "<a id=\"chill-nodes\"></a>" } else { "" };
    format!(
        "<section id=\"{id}\" class=\"{}\" aria-labelledby=\"{tid}\">{anchor}{head}<p class=\"nd-aux -mt-1 mb-3 px-1\">{desc}</p>{body}</section>",
        stale_cls(g.status.stale()).trim()
    )
}

// ---- profile ----

fn profile(g: &Got, l: Lang, known: bool) -> String {
    let Some(v) = g.status.v.as_ref().filter(|_| known) else {
        return hidden("ch-profile");
    };
    let cur = Some(s(v, "profile")).filter(|p| !p.is_empty()).unwrap_or("standard");
    let eff = Some(s(v, "profile_effective")).filter(|p| !p.is_empty()).unwrap_or(cur);
    let items: Vec<_> = PROFILES
        .iter()
        .map(|p| (esc(profile_label(p, l)), String::new(), *p == cur, post_once(&format!("/services/chill/profile/set?v={p}"))))
        .collect();
    let mut extra = String::new();
    if v["thermal_eco"].as_bool() == Some(true) && eff != cur {
        extra += &format!(
            "<p class=\"ch-warn\">{}</p>",
            mark(
                "warn",
                l.pick(
                    "Running as Eco for now because the device is hot; back to your choice once it has cooled down.",
                    "设备太热，暂时按「省电」运行；凉下来后自动回到你选的档位。"
                )
            )
        );
    }
    if s(v, "state") != "running" {
        extra += &format!("<p class=\"nd-aux\">{}</p>", l.pick("Applies the next time CHILL starts.", "下次启动 CHILL 时生效。"));
    }
    format!(
        "<section id=\"ch-profile\" class=\"{}\" aria-labelledby=\"ch-profile-t\">{}<div class=\"nd-group grid gap-3 p-4\">{}<p class=\"nd-aux\">{}</p><p class=\"nd-aux\">{}</p>{extra}</div></section>",
        stale_cls(g.status.stale()).trim(),
        title(l, "ch-profile-t", "Profile", "档位"),
        seg("ch-profile-t", "ch-pf", &items),
        esc(profile_hint(cur, l)),
        l.pick(
            "Trade battery and heat against failover speed. Switching into or out of Eco restarts the proxy core: connections drop for about 10 seconds.",
            "在耗电、发热和切换速度之间取舍。进出「省电」要重启代理核心，连接会断大约 10 秒。"
        ),
    )
}

// ---- subscriptions ----

fn prov(g: &Got, l: Lang, running: bool, now: i64) -> String {
    if !running {
        return hidden("ch-prov");
    }
    let slot = &g.prov;
    let list: Vec<&Value> = slot.v.as_ref().and_then(|v| v["providers"].as_array()).map(|a| a.iter().collect()).unwrap_or_default();
    let mut rows = String::new();
    if slot.v.is_none() {
        rows = match &slot.err {
            Some(e) => format!(
                "<div class=\"nd-row flex-wrap\"><span class=\"flex-1 ch-t2\">{}: <span data-data>{}</span></span><button type=\"button\" class=\"nd-btn nd-btn--secondary nd-btn--sm\" data-on:click=\"@post('/services/chill/refresh')\">{}</button></div>",
                l.pick("Couldn't load subscriptions", "订阅加载失败"),
                esc(&e.text(l)),
                l.pick("Retry", "重试")
            ),
            None => "<div class=\"nd-row\"><span class=\"nd-skel\" style=\"width:18ch\"></span></div>".into(),
        };
    } else if list.is_empty() {
        // mihomo not answering gives an empty list with ok: not a real "no subscriptions"
        rows = format!("<div class=\"nd-row\"><span class=\"ch-t2\">{}</span></div>", l.pick("No subscriptions found.", "没有找到订阅。"));
    }
    for (i, p) in list.iter().enumerate() {
        let name = s(p, "name");
        let nodes = p["node_count"].as_i64().unwrap_or(0);
        let nodes_t = match l {
            Lang::En => format!("{nodes} nodes"),
            Lang::Zh => format!("{nodes} 个节点"),
        };
        let mut sub = Vec::new();
        if !s(p, "vehicle_type").is_empty() {
            sub.push(format!("<span data-data>{}</span>", esc(s(p, "vehicle_type"))));
        }
        sub.push(nodes_t);
        if !s(p, "updated_at").is_empty() {
            sub.push(ago(l, s(p, "updated_at"), now));
        }
        let t = tag(name);
        let usage = if p["subscription"].is_object() {
            let u = &p["subscription"];
            let (up, down, total) = (u["Upload"].as_f64().unwrap_or(0.0), u["Download"].as_f64().unwrap_or(0.0), u["Total"].as_f64().unwrap_or(0.0));
            let used = up + down;
            let bar = if total > 0.0 {
                let pct = (used / total * 100.0).clamp(0.0, 100.0);
                format!("<div class=\"ch-bar\" role=\"img\" aria-label=\"{pct:.0}%\"><div class=\"ch-bar__fill\" style=\"width:{pct:.1}%\"></div></div>")
            } else {
                String::new()
            };
            // Expire is a real UTC instant: the date is made in the browser, in its own zone (as the
            // old page did); the UTC date is only what shows until the script runs
            let exp = u["Expire"].as_i64().filter(|e| *e > 0).map(|e| {
                format!(
                    " · {} <span data-text=\"new Date({}).toLocaleDateString()\">{}</span>",
                    l.pick("Expires", "到期"),
                    e * 1000,
                    date(e)
                )
            });
            format!(
                "<div class=\"grid gap-1 ch-usage\">{bar}<span class=\"nd-aux\">{} / {}{}</span></div>",
                bytes(Some(used)).unwrap_or_else(|| "—".into()),
                bytes(Some(total)).unwrap_or_else(|| "—".into()),
                exp.unwrap_or_default()
            )
        } else {
            String::new()
        };
        let edit = if p["editable"].as_bool() == Some(true) {
            format!(
                "<button type=\"button\" id=\"ch-pe-{i}\" class=\"nd-btn nd-btn--ghost nd-btn--sm\" aria-labelledby=\"ch-pe-{i} ch-pn-{i}\" data-on:click=\"@post('/services/chill/prov/edit?i={i}&h={t}')\">{}</button>",
                l.pick("Edit link", "编辑订阅链接")
            )
        } else {
            String::new()
        };
        rows += &format!(
            "<div class=\"nd-row flex-col items-stretch gap-2\"><div class=\"flex flex-wrap items-center gap-2\"><span class=\"nd-row__text min-w-0 flex-1\"><span class=\"nd-row__label truncate\" id=\"ch-pn-{i}\" data-data>{}</span><span class=\"nd-row__sub block\">{}</span></span><button type=\"button\" id=\"ch-pr-{i}\" class=\"nd-btn nd-btn--secondary nd-btn--sm\" aria-labelledby=\"ch-pr-{i} ch-pn-{i}\" data-on:click=\"@post('/services/chill/prov/refresh/ask?i={i}&h={t}')\">{}</button>{edit}</div>{usage}</div>",
            esc(name),
            sub.join(" · "),
            l.pick("Refresh", "刷新"),
        );
    }
    format!(
        "<section id=\"ch-prov\" aria-labelledby=\"ch-prov-t\">{}<div class=\"nd-group{}\">{rows}</div><p class=\"nd-aux mt-2 px-1\">{}</p></section>",
        title(l, "ch-prov-t", "Subscriptions", "订阅"),
        stale_cls(slot.stale()),
        l.pick("Node providers feeding the region groups above.", "给地区分组供货的节点来源。")
    )
}

fn groups(g: &Got, l: Lang, running: bool) -> String {
    let list = g.status.v.as_ref().filter(|_| running).and_then(|v| v["groups"].as_array()).filter(|a| !a.is_empty());
    let Some(list) = list else {
        return hidden("ch-groups");
    };
    let mut rows = String::new();
    for x in list {
        let n = x["size"].as_i64().unwrap_or(0);
        rows += &format!(
            "<div class=\"nd-row\"><span class=\"nd-row__text\"><span class=\"nd-row__label\" data-data>{}</span><span class=\"nd-row__sub block\">{}</span></span><span class=\"nd-row__value\" data-data>{}</span></div>",
            esc(s(x, "name")),
            match l {
                Lang::En => format!("{n} nodes"),
                Lang::Zh => format!("{n} 个节点"),
            },
            esc(Some(s(x, "now")).filter(|n| !n.is_empty()).unwrap_or("—")),
        );
    }
    format!(
        "<section id=\"ch-groups\" aria-labelledby=\"ch-groups-t\">{}<div class=\"nd-group{}\">{rows}</div></section>",
        title(l, "ch-groups-t", "Proxy groups", "代理组"),
        stale_cls(g.status.stale())
    )
}

// ---- device bypass ----

fn bypass(g: &Got, l: Lang) -> String {
    let slot = &g.bypass;
    let ips = slot.v.as_ref().map(|v| strs(&v["ips"])).unwrap_or_default();
    let stale_ips: Vec<String> = slot.v.as_ref().map(|v| strs(&v["stale"])).unwrap_or_default();
    let head = format!(
        "{}<p class=\"nd-aux -mt-1 mb-3 px-1\">{}</p>",
        title(l, "ch-byp-t", "Device bypass", "设备绕行"),
        l.pick("Devices listed here skip CHILL entirely and go straight out to the internet.", "这里列出的设备完全跳过 CHILL,直接连外网。")
    );
    let warn = if stale_ips.is_empty() {
        String::new()
    } else {
        let n = stale_ips.len();
        let mut btns = String::new();
        for ip in stale_ips.iter().filter(|i| ipv4(i)) {
            btns += &format!(
                "<button type=\"button\" class=\"nd-btn nd-btn--secondary nd-btn--sm\" aria-label=\"{}\" data-on:click=\"@post('/services/chill/byp/ask?ip={ip}&on=0')\"><span class=\"nd-mono\">{ip}</span> ×</button>",
                match l {
                    Lang::En => format!("Remove {ip} from the bypass list"),
                    Lang::Zh => format!("把 {ip} 从绕行名单里移除"),
                }
            );
        }
        format!(
            "<div class=\"ch-stale grid gap-2\">{}<div class=\"flex flex-wrap gap-2\">{btns}</div></div>",
            mark(
                "warn",
                &match l {
                    Lang::En => format!("{n} bypassed device(s) no longer match a known lease — their traffic is silently going through CHILL again."),
                    Lang::Zh => format!("{n} 台绕行设备已找不到对应的租约 —— 它们的流量正在悄悄地重新经过 CHILL。"),
                }
            )
        )
    };
    let body = match (&slot.v, &slot.err) {
        (None, Some(e)) => format!(
            "<div class=\"nd-row flex-wrap\"><span class=\"flex-1 ch-t2\">{}: <span data-data>{}</span></span><button type=\"button\" class=\"nd-btn nd-btn--secondary nd-btn--sm\" data-on:click=\"@post('/services/chill/refresh')\">{}</button></div>",
            l.pick("Couldn't load bypass list", "绕行列表加载失败"),
            esc(&e.text(l)),
            l.pick("Retry", "重试")
        ),
        (None, None) => "<div class=\"nd-row\"><span class=\"nd-skel\" style=\"width:18ch\"></span></div>".into(),
        (Some(_), _) => {
            let rows = bypass_rows(g.clients.v.as_ref(), &ips);
            if rows.is_empty() {
                format!("<div class=\"nd-row\"><span class=\"ch-t2\">{}</span></div>", l.pick("No devices seen on the LAN yet.", "局域网里还没见到任何设备。"))
            } else {
                let mut b = String::new();
                for (i, ip) in rows.iter().enumerate() {
                    let on = ips.contains(ip);
                    let name = g.clients.v.as_ref().map_or_else(|| ip.clone(), |c| client_name(c, ip));
                    let stale_row = stale_ips.contains(ip);
                    b += &format!(
                        "<div class=\"nd-row\"><span class=\"nd-row__text\"><span class=\"nd-row__label\" id=\"ch-bn-{i}\" data-data>{}</span><span class=\"nd-row__sub block nd-mono\"><span data-data>{}</span>{}</span></span>{}</div>",
                        esc(&name),
                        esc(ip),
                        if stale_row { format!(" · {}", l.pick("stale", "已失效")) } else { String::new() },
                        switch(&format!("ch-bn-{i}"), on, !ipv4(ip), &format!("@post('/services/chill/byp/ask?ip={ip}&on={}')", u8::from(!on))),
                    );
                }
                b
            }
        }
    };
    format!(
        "<section id=\"ch-byp\" aria-labelledby=\"ch-byp-t\">{head}{warn}<div class=\"nd-group{}\">{body}</div></section>",
        stale_cls(slot.stale())
    )
}

// ---- advanced: vitals, dashboard, log ----

fn readout(label: &str, value: &str, unit: &str, sub: &str) -> String {
    format!(
        "<div class=\"nd-readout\"><div class=\"nd-readout__label\">{label}</div><div class=\"nd-readout__value\">{}</div>{}</div>",
        if value.is_empty() {
            "<span class=\"nd-readout__empty\">—</span>".to_owned()
        } else {
            format!("<span>{}</span>{}", esc(value), if unit.is_empty() { String::new() } else { format!("<span class=\"nd-readout__unit\">{unit}</span>") })
        },
        if sub.is_empty() { String::new() } else { format!("<div class=\"nd-readout__sub\" data-data>{}</div>", esc(sub)) }
    )
}

fn vitals(g: &Got, l: Lang, known: bool, now: i64) -> String {
    let Some(v) = g.status.v.as_ref().filter(|_| known) else {
        return hidden("ch-vitals");
    };
    let num = |k: &str| v[k].as_f64().map(|x| format!("{x:.0}")).unwrap_or_default();
    let pid = v["core_pid"].as_i64().filter(|p| *p > 0).map(|p| p.to_string()).unwrap_or_default();
    format!(
        "<div id=\"ch-vitals\" class=\"nd-wall{}\" role=\"group\" aria-label=\"{}\">{}{}{}{}</div>",
        stale_cls(g.status.stale()),
        l.pick("CHILL vitals", "CHILL 运行状况"),
        readout(l.pick("Temperature", "温度"), &num("cpuss_c"), "°C", ""),
        readout(
            l.pick("Memory available", "可用内存"),
            &num("mem_avail_mb"),
            "MB",
            if v["mem_pressure"].as_bool() == Some(true) { l.pick("Under pressure", "内存紧张") } else { "" }
        ),
        readout(l.pick("Uptime", "运行时间"), &uptime(s(v, "started_at"), now).unwrap_or_default(), "", s(v, "version")),
        readout("PID", &pid, "", ""),
    )
}

fn dash(l: Lang, running: bool) -> String {
    if !running {
        return hidden("ch-dash");
    }
    // Same origin, no secret: /chill-api is behind the luyaz-web session (chill_proxy.rs), no token in the link.
    // The address is made in the browser (Host and port), the iframe gets its src only when opened.
    const URL: &str = "'/chill-ui/#/setup?hostname=' + location.hostname + '&port=' + (location.port || (location.protocol === 'https:' ? '443' : '80')) + '&secondaryPath=/chill-api&label=CHILL' + (location.protocol === 'https:' ? '&https=1' : '')";
    let aurl = URL.replace('&', "&amp;");
    format!(
        "<section id=\"ch-dash\"><div class=\"nd-group\"><div class=\"nd-row flex-wrap gap-2\"><span class=\"nd-row__text\"><span class=\"nd-row__label\">{}</span><span class=\"nd-row__sub block\">{}</span></span><span class=\"flex flex-wrap gap-2\"><a class=\"nd-btn nd-btn--secondary nd-btn--sm\" href=\"/chill-ui/\" target=\"_blank\" rel=\"noreferrer\" data-attr:href=\"{aurl}\">{}</a><button type=\"button\" class=\"nd-btn nd-btn--ghost nd-btn--sm ch-wide\" aria-expanded=\"false\" data-attr:aria-expanded=\"$_cdo ? 'true' : 'false'\" data-on:click=\"$_cdo = !$_cdo\" data-text=\"$_cdo ? '{}' : '{}'\">{}</button></span></div><div class=\"ch-wide px-4 pb-4\" data-show=\"$_cdo\" style=\"display:none\"><iframe class=\"ch-frame\" title=\"CHILL dashboard\" sandbox=\"allow-scripts allow-same-origin allow-forms allow-popups allow-downloads\" data-attr:src=\"$_cdo &amp;&amp; ({aurl})\"></iframe></div></div></section>",
        l.pick("Dashboard", "面板"),
        l.pick("zashboard — mihomo's own panel, not restyled for CHILL", "zashboard —— mihomo 自带面板,没有为 CHILL 改样式"),
        l.pick("Open", "打开"),
        l.pick("Collapse", "收起"),
        l.pick("Show here", "在这里展开"),
        l.pick("Show here", "在这里展开"),
    )
}

fn log(g: &Got, l: Lang, tab: &str) -> String {
    let lines = g.log.v.as_ref().map(|v| strs(&v["lines"])).unwrap_or_default();
    let body = if lines.is_empty() {
        format!("<span class=\"nd-console__muted\">{}</span>", if g.log.v.is_none() && g.log.err.is_some() { l.pick("Couldn't read the log.", "读不到日志。") } else { l.pick("No log lines yet.", "暂无日志。") })
    } else {
        esc(&lines.join("\n"))
    };
    format!(
        "<div id=\"ch-log\" class=\"nd-console{}\"><div class=\"mb-3 flex flex-wrap items-center gap-2\"><span class=\"nd-console__title flex-1\">{} <span class=\"nd-console__muted\">/tmp/chill.log</span></span><button type=\"button\" class=\"nd-btn nd-btn--sm ch-cbtn\" data-on:click=\"$_chlp = !$_chlp; @post('/services/chill/log/pause?t={tab}&on=' + ($_chlp ? 1 : 0))\" data-text=\"$_chlp ? '{}' : '{}'\" aria-pressed=\"false\" data-attr:aria-pressed=\"$_chlp ? 'true' : 'false'\">{}</button><button type=\"button\" class=\"nd-btn nd-btn--sm ch-cbtn\" data-on:click=\"@post('/services/chill/refresh')\">{}</button></div><pre class=\"ch-pre\" data-data tabindex=\"0\" aria-label=\"{}\">{body}</pre></div>",
        stale_cls(g.log.stale()),
        l.pick("Service log", "服务日志"),
        l.pick("Resume", "继续"),
        l.pick("Pause", "暂停"),
        l.pick("Pause", "暂停"),
        l.pick("Refresh", "刷新"),
        l.pick("Service log", "服务日志"),
    )
}

/// The write area under a section (`{id}-op`): same shape as the other pages' [`crate::write::Area`].
pub fn area(id: &str, l: Lang, remote: Via, set: &str, phase: &crate::write::Phase) -> String {
    let cancel = format!("/services/chill/cancel?a={}", &id[3..]);
    crate::write::Area {
        id,
        l,
        remote,
        set,
        cancel: &cancel,
        phase,
    }
    .html()
}

/// The ids of the write areas, in page order.
pub const AREAS: [&str; 8] = ["ch-pw", "ch-ex", "ch-ho", "ch-rg", "ch-ax", "ch-pf", "ch-pv", "ch-bp"];
