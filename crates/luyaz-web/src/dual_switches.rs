//! /signal: the 「换卡记录」 group, dual mode only (global edition). The data-slot changes
//! the core's passive log saw (`GET /v2/dual/switches?days=14`, core dsds_log.rs), newest
//! first: time, from operator → to operator, and a reason read from the sample before the
//! change against the stock rule (docs/research/dsds-smart-switch-20261021.md): the data card
//! had no service, was on 2G/3G, or was weak while the other card was good. Anything else
//! (smart switching off, or no rule matched) is 「手动/原因不明」. Read once per page load.

use crate::i18n::Lang;
use serde_json::Value;

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The stock rule's thresholds: rsrp1..3 / snr1..3 (`sim.dual.smart_rule`, else the B31 values).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rule {
    pub rsrp: [i64; 3],
    pub snr: [i64; 3],
}

impl Default for Rule {
    fn default() -> Self {
        Rule { rsrp: [-115, -108, -105], snr: [0, -3, 5] }
    }
}

impl Rule {
    pub fn of(dual: &Value) -> Rule {
        let d = Rule::default();
        let three = |k: &str, def: [i64; 3]| {
            let v: Vec<i64> = dual["smart_rule"][k].as_array().map(|a| a.iter().filter_map(Value::as_i64).collect()).unwrap_or_default();
            if v.len() >= 3 { [v[0], v[1], v[2]] } else { def }
        };
        Rule { rsrp: three("rsrp", d.rsrp), snr: three("snr", d.snr) }
    }
}

fn slot<'a>(s: &'a Value, n: &Value) -> Option<&'a Value> {
    s["slots"].as_array()?.iter().find(|v| &v["slot"] == n)
}

fn rsrp_snr(v: &Value) -> Option<(i64, f64)> {
    Some((v["rsrp"].as_i64()?, v["snr"].as_str()?.parse().ok()?))
}

fn fmt_snr(x: f64) -> String {
    if x.fract() == 0.0 { format!("{x:.0}") } else { format!("{x:.1}") }
}

/// Why the firmware moved the data card, from the sample before the move.
pub fn reason(ev: &Value, rule: Rule, l: Lang) -> String {
    let unknown = || l.pick("Manual / reason unknown", "手动/原因不明").to_owned();
    if ev["smart"] != true {
        return unknown();
    }
    let b = &ev["before"];
    let (Some(data), other) = (slot(b, &ev["from"]), slot(b, &ev["to"])) else { return unknown() };
    let other_rs = other.and_then(rsrp_snr);
    let no_service = data["rsrp"].is_null() || data["state"].as_str().is_some_and(|s| s != "ready");
    if no_service && other_rs.is_some_and(|(r, _)| r > rule.rsrp[0]) {
        return l.pick("Data card had no service", "上网卡无服务").to_owned();
    }
    let good_other = other_rs.filter(|&(r, s)| r >= rule.rsrp[2] && s >= rule.snr[2] as f64);
    let Some((orp, osn)) = good_other else { return unknown() };
    let net = b["net_type"].as_str().unwrap_or("");
    if matches!(net, "GSM" | "EDGE" | "GPRS" | "WCDMA" | "UMTS" | "HSPA" | "HSPA+" | "HSDPA" | "HSUPA" | "TD-SCDMA" | "2G" | "3G") {
        return l.pick("Data card was on 2G/3G", "上网卡只有 2G/3G").to_owned();
    }
    let Some((r, s)) = rsrp_snr(data) else { return unknown() };
    let weak = (r <= rule.rsrp[0] && s <= rule.snr[0] as f64) || (r <= rule.rsrp[1] && s <= rule.snr[1] as f64);
    if !weak {
        return unknown();
    }
    let (s, osn) = (fmt_snr(s), fmt_snr(osn));
    match l {
        Lang::En => format!("Data card weak ({r} dBm / {s} dB), the other {orp} dBm / {osn} dB"),
        _ => format!("上网卡信号差（{r} dBm / {s} dB），另一张 {orp} dBm / {osn} dB"),
    }
}

/// The group's HTML. `reply` = the core's `data` (`{days, count, switches}`); None = not read.
pub fn section(reply: Option<&Value>, rule: Rule, l: Lang) -> String {
    let title = l.pick("Card switches", "换卡记录");
    let days = reply.and_then(|r| r["days"].as_i64()).unwrap_or(14);
    let list: &[Value] = reply.and_then(|r| r["switches"].as_array()).map_or(&[], Vec::as_slice);
    let rows: String = match reply {
        None => format!(
            "<div class=\"nd-row\"><span class=\"nd-row__text\"><span class=\"nd-row__label\">{}</span></span></div>",
            l.pick("Could not read the log", "读不到记录")
        ),
        Some(_) if list.is_empty() => format!(
            "<div class=\"nd-row\"><span class=\"nd-row__text\"><span class=\"nd-row__label\">{}</span></span></div>",
            match l {
                Lang::En => format!("None in the past {days} days"),
                _ => format!("过去 {days} 天 0 次"),
            }
        ),
        Some(_) => list
            .iter()
            .map(|ev| {
                let op = |n: &Value| {
                    let o = slot(&ev["before"], n).or_else(|| slot(&ev["after"], n)).and_then(|s| s["operator"].as_str()).unwrap_or("");
                    let w = crate::home_dual::op_word(o, l);
                    if w == "—" { format!("{} {n}", l.pick("Slot", "卡槽")) } else { w.to_owned() }
                };
                format!(
                    "<div class=\"nd-row\"><span class=\"nd-row__text\"><span class=\"nd-row__label\" data-data>{} → {}</span><span class=\"nd-row__sub\">{}</span></span><span class=\"nd-row__value\" data-data>{}</span></div>",
                    esc(&op(&ev["from"])),
                    esc(&op(&ev["to"])),
                    esc(&reason(ev, rule, l)),
                    crate::alerts_page::mdhm(ev["t"].as_i64().unwrap_or(0)),
                )
            })
            .collect(),
    };
    format!("<section id=\"sig-switches\" aria-labelledby=\"sig-switches-t\"><h2 id=\"sig-switches-t\" class=\"nd-group-title\">{title}</h2><div class=\"nd-group\">{rows}</div></section>")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(data_rsrp: Value, data_snr: &str, net: &str, other: (i64, &str), smart: bool) -> Value {
        json!({"t": 1_792_584_000i64, "ev": "switch", "from": 1, "to": 2, "smart": smart,
            "before": {"net_type": net, "slots": [
                {"slot": 1, "operator": "CUCC", "state": "ready", "rsrp": data_rsrp, "snr": data_snr},
                {"slot": 2, "operator": "CMCC", "state": "ready", "rsrp": other.0, "snr": other.1}]},
            "after": {}})
    }

    #[test]
    fn reasons() {
        let r = Rule::default();
        let weak = ev(json!(-116), "-1.0", "SA", (-98, "7.0"), true);
        assert_eq!(reason(&weak, r, Lang::Zh), "上网卡信号差（-116 dBm / -1 dB），另一张 -98 dBm / 7 dB");
        assert_eq!(reason(&weak, r, Lang::En), "Data card weak (-116 dBm / -1 dB), the other -98 dBm / 7 dB");
        assert_eq!(reason(&ev(Value::Null, "", "", (-110, "1.0"), true), r, Lang::Zh), "上网卡无服务");
        assert_eq!(reason(&ev(json!(-80), "10.0", "WCDMA", (-98, "7.0"), true), r, Lang::Zh), "上网卡只有 2G/3G");
        // other card not good enough, data card fine, or smart off
        assert_eq!(reason(&ev(json!(-116), "-1.0", "SA", (-110, "7.0"), true), r, Lang::Zh), "手动/原因不明");
        assert_eq!(reason(&ev(json!(-90), "10.0", "SA", (-98, "7.0"), true), r, Lang::Zh), "手动/原因不明");
        assert_eq!(reason(&ev(json!(-116), "-1.0", "SA", (-98, "7.0"), false), r, Lang::Zh), "手动/原因不明");
    }

    #[test]
    fn rule_from_core() {
        let d = json!({"smart_rule": {"rsrp": [-120, -110, -100], "snr": [1, -2, 6], "period_s": 120}});
        assert_eq!(Rule::of(&d), Rule { rsrp: [-120, -110, -100], snr: [1, -2, 6] });
        assert_eq!(Rule::of(&json!({})), Rule::default());
    }

    #[test]
    fn rows() {
        let none = section(Some(&json!({"days": 14, "count": 0, "switches": []})), Rule::default(), Lang::Zh);
        assert!(none.contains("换卡记录") && none.contains("过去 14 天 0 次"));
        let one = json!({"days": 14, "count": 1, "switches": [ev(json!(-116), "-1.0", "SA", (-98, "7.0"), true)]});
        let h = section(Some(&one), Rule::default(), Lang::Zh);
        assert!(h.contains("中国联通 → 中国移动"), "{h}");
        assert!(h.contains("10-21 12:00"));
        assert!(section(None, Rule::default(), Lang::En).contains("Could not read the log"));
    }
}
