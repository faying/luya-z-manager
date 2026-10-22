//! Home: the 「双卡」 group, shown only while the global edition runs both slots
//! (`sim.dual.mode == "dual"`). One row per slot: operator, RAT, role, RSRP · SINR and
//! the reading's age. Single-slot devices and single mode render an empty hidden leaf.
//! Design: docs/designs/dual-sim-global.md「双卡待机时的首页与状态栏」.

use crate::hub::St;
use crate::i18n::Lang;
use serde_json::Value;
use std::path::Path;

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// `sim.dual` when it is in dual mode, with the sim block's observed_at.
pub(crate) fn dual(st: &St) -> Option<(&Value, i64)> {
    let b = st.blocks.get("sim")?;
    let d = &b.data["dual"];
    (d["mode"].as_str() == Some("dual")).then_some((d, b.observed_at as i64))
}

/// The data card's operator for the top RSRP/SINR readouts ("China Unicom"); None outside dual mode.
pub fn data_tag(st: &St, l: Lang) -> Option<String> {
    let (d, _) = dual(st)?;
    let v = d["slots"].as_array()?.iter().find(|v| v["role"] == "data")?;
    Some(op_word(v["operator"].as_str().unwrap_or(""), l).to_owned())
}

/// The core's short codes for the mainland operators, in the page's words.
pub(crate) fn op_word(op: &str, l: Lang) -> &str {
    match op {
        "" => "—",
        "CMCC" => l.pick("China Mobile", "中国移动"),
        "CUCC" => l.pick("China Unicom", "中国联通"),
        "CTCC" => l.pick("China Telecom", "中国电信"),
        "CBN" => l.pick("China Broadnet", "中国广电"),
        o => o,
    }
}

/// The data card's RAT comes from the core's own word; the standby card's from the
/// sampler, and an idle card on NR can only be SA (NSA needs an active LTE anchor).
fn rat_word(r: &str) -> &str {
    match r {
        "nr" => "5G SA",
        "lte" => "4G",
        _ => "",
    }
}

fn state_word(s: &str, l: Lang) -> Option<&'static str> {
    Some(match s {
        "not_ready" => l.pick("Not ready", "未就绪"),
        "locked" => l.pick("PIN locked", "PIN 锁"),
        "absent" => l.pick("No card", "无卡"),
        "off" => l.pick("Off", "未启用"),
        _ => return None,
    })
}

fn row(v: &Value, net: &str, smart: bool, switching: bool, l: Lang, logo_dir: &Path) -> String {
    let n = v["slot"].as_u64().unwrap_or(0);
    let s = |k: &str| v[k].as_str().unwrap_or("");
    let op = s("operator");
    let name = op_word(op, l);
    let logo = if op.is_empty() && s("mcc").is_empty() {
        String::new()
    } else {
        crate::oplogo::html_with(logo_dir, s("logo"), s("mcc"), s("mnc"), name, 16)
    };
    let data = s("role") == "data";
    let sig = &v["signal"];
    // the data card can be NSA: use the serving network's type, not the sampler's RAT
    let rat = match (data, net) {
        (true, "SA") => "5G SA",
        (true, "NSA") => "5G NSA",
        (true, "LTE" | "LTE-A" | "LTE_A") => "4G",
        _ => rat_word(sig["rat"].as_str().unwrap_or("")),
    };
    // where, as on the touch Home: 本地 / 漫游到中国移动 (the core's words)
    let place = if matches!(l, Lang::En) { s("place_en") } else { s("place") };
    let rat = if place.is_empty() {
        rat.to_string()
    } else if rat.is_empty() {
        place.to_string()
    } else {
        format!("{rat} · {place}")
    };
    let rat = esc(&rat);
    // the core's bars tier: 1–2 = weak
    let weak = !data && sig["bars"].as_i64().is_some_and(|b| (1..=2).contains(&b));
    let (value, warn) = if switching && data {
        (l.pick("Switching…", "切换中…").to_owned(), false)
    } else if let Some(w) = state_word(s("state"), l) {
        (w.to_owned(), false)
    } else if data && smart {
        (l.pick("In use · auto switch", "上网中 · 自动切换").to_owned(), false)
    } else if data {
        (l.pick("In use", "上网中").to_owned(), false)
    } else if weak {
        (l.pick("Standby · weak signal", "待机 · 信号弱").to_owned(), true)
    } else {
        (l.pick("Standby", "待机").to_owned(), false)
    };
    format!(
        "<div id=\"hds-{n}\" class=\"nd-row{}\"><span class=\"nd-row__text\"><span class=\"nd-row__label hm-dual__op\">{logo}<span data-data>{}</span></span><span class=\"nd-row__sub\" data-data>{rat}</span></span><span class=\"nd-row__value{}\" data-data>{}</span></div>",
        if data { "" } else { " hm-dual--idle" },
        esc(name),
        if warn { " hm-dual--weak" } else { "" },
        esc(&value),
    )
}

/// The whole `#hm-dual` section leaf (hidden outside dual mode): the data card first,
/// in the same words as the touch Home. Readings live on /signal and in the readouts above.
pub fn leaf(st: &St, l: Lang, logo_dir: &Path) -> String {
    let title = l.pick("Dual SIM", "双卡");
    let Some((d, _)) = dual(st) else {
        return "<section id=\"hm-dual\" class=\"hm-o1\" hidden></section>".to_owned();
    };
    let (smart, switching) = (d["smart"] == true, d["switching"] == true);
    let mut slots: Vec<&Value> = d["slots"].as_array().map(|a| a.iter().collect()).unwrap_or_default();
    slots.sort_by_key(|v| (v["role"] != "data", v["slot"].as_u64().unwrap_or(0)));
    let net = st.blocks.get("signal").and_then(|b| b.data["type"].as_str()).unwrap_or("");
    let rows: String = slots.iter().map(|v| row(v, net, smart, switching, l, logo_dir)).collect();
    format!(
        "<section id=\"hm-dual\" class=\"hm-o1\" aria-labelledby=\"hm-dual-t\"><h2 id=\"hm-dual-t\" class=\"nd-group-title\">{title}</h2><div class=\"nd-group hm-dual\">{rows}</div></section>"
    )
}
