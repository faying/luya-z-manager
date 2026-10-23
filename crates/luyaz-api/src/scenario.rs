//! The scenario slice of `/api/public/status`. The caller reads the engine's state (the agent from memory, u60d from the engine's files)
//! and this renders it, so both reply alike. The picker on the network page ([`picker`])
//! likewise.

use serde_json::{Value, json};

/// The built-in scenarios' factory names and their English. A scenario gets
/// `name_en` only while it still has its factory id *and* factory name: once
/// the owner renames it, their name is shown in both languages. Worked out on
/// every read and never saved, so the config file and older agents are
/// unaffected (L2 review R6).
pub const FACTORY_NAMES: [(&str, &str, &str); 3] = [
    ("home", "在家", "Home"),
    ("away", "外出", "Away"),
    ("abroad", "国外", "Abroad"),
];

pub fn name_en(id: &str, name: &str) -> Option<&'static str> {
    FACTORY_NAMES
        .iter()
        .find(|f| f.0 == id && f.1 == name)
        .map(|f| f.2)
}

/// The current scenario as the summary reads it.
#[derive(Clone, Copy, Debug)]
pub struct Current<'a> {
    pub id: &'a str,
    pub name: &'a str,
    /// inhibit_sleep marks the scenarios that take the APs down.
    pub inhibit_sleep: bool,
    pub abroad: bool,
}

/// The engine state the summary reads.
#[derive(Clone, Debug, Default)]
pub struct Engine<'a> {
    pub configured: bool,
    pub enabled: bool,
    pub current: &'a str,
    pub pin: Option<String>,
    pub guard_takeover: bool,
    pub last_switch: Option<i64>,
}

/// The slice of engine state the touch screen shows, for the unauthenticated
/// `/api/public/status`. Scenario names and times only — never the network
/// list, the SIM, or anything a LAN client could use to spoof "home".
pub fn public_summary(e: &Engine, scen: Option<Current>) -> Value {
    json!({
        "configured": e.configured,
        "enabled": e.enabled,
        "current": e.current,
        "name": scen.map(|s| s.name).unwrap_or(""),
        "name_en": scen.and_then(|s| name_en(s.id, s.name)),
        "wifi_off": scen.is_some_and(|s| s.inhibit_sleep),
        "abroad": e.enabled && scen.is_some_and(|s| s.abroad),
        "pin": e.pin,
        "guard_takeover": e.guard_takeover,
        "last_switch": e.last_switch,
    })
}

/// How a scenario is entered, as the picker words it.
#[derive(Clone, Debug)]
pub enum When<'a> {
    /// The home Wi-Fi names (empty: not set yet).
    Ssid(Vec<&'a str>),
    Mcc(&'a [String]),
    Abroad,
    Fallback,
}

/// One action of a scenario: its API path and body.
#[derive(Clone, Copy, Debug)]
pub struct Does<'a> {
    pub path: &'a str,
    pub body: Option<&'a Value>,
}

/// One scenario as the touch screen's picker shows it.
#[derive(Clone, Debug)]
pub struct Scene<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub inhibit_sleep: bool,
    pub abroad: bool,
    pub when: When<'a>,
    pub does: Vec<Does<'a>>,
}

/// The picker (`/api/netinfo` `scenes`): the engine's state and every scenario with a line on when
/// it is entered and what it changes, Chinese in `list` and English in `list_en`.
pub fn picker(
    enabled: bool,
    current: &str,
    pin: Option<String>,
    guard_takeover: bool,
    scenes: &[Scene],
    home_mcc: &str,
) -> Value {
    json!({
        "enabled": enabled,
        "current": current,
        "pin": pin,
        "guard_takeover": guard_takeover,
        "list": scenes.iter().map(|s| json!({
            "id": s.id,
            "name": s.name,
            "wifi_off": s.inhibit_sleep,
            "abroad": s.abroad,
            "when": describe_when(&s.when, home_mcc).0,
            "does": describe_does(&s.does, s.inhibit_sleep).0,
        })).collect::<Vec<_>>(),
        // English apart from `list`, and after it (keys come out sorted): the
        // old touch screen reads `list` into a 2 KB buffer that the English
        // would overflow, while the whole `scenes` object gets 8 KB.
        // `name_en` is null for a renamed scenario: the owner's name is shown
        // in both languages.
        "list_en": scenes.iter().map(|s| json!({
            "id": s.id,
            "name_en": name_en(s.id, s.name),
            "when_en": describe_when(&s.when, home_mcc).1,
            "does_en": describe_does(&s.does, s.inhibit_sleep).1,
        })).collect::<Vec<_>>(),
    })
}

/// When a scenario is entered, in Chinese and English, written side by side so a change to one is
/// made to the other. Written from the configuration, so a scenario the owner edits in the admin
/// web is explained correctly too (2026-09-25: the bare list was unreadable).
pub fn describe_when(d: &When, home_mcc: &str) -> (String, String) {
    use crate::netinfo::{country, country_en};
    let mcc_name = |m: &str| {
        m.parse::<u16>()
            .ok()
            .and_then(country)
            .map(str::to_string)
            .unwrap_or_else(|| format!("MCC {m}"))
    };
    let mcc_name_en = |m: &str| {
        m.parse::<u16>()
            .ok()
            .and_then(country_en)
            .map(str::to_string)
            .unwrap_or_else(|| format!("MCC {m}"))
    };
    match d {
        When::Ssid(entries) if entries.is_empty() => (
            "还没设家里的 Wi-Fi，不会自动进入".into(),
            "Home Wi-Fi not set yet; never entered automatically".into(),
        ),
        When::Ssid(entries) => {
            let names: Vec<String> = entries.iter().take(2).map(|e| format!("「{e}」")).collect();
            let names_en: Vec<String> =
                entries.iter().take(2).map(|e| format!("\"{e}\"")).collect();
            let more = if entries.len() > 2 {
                format!("等 {} 个", entries.len())
            } else {
                String::new()
            };
            let more_en = if entries.len() > 2 {
                format!(" ({} in all)", entries.len())
            } else {
                String::new()
            };
            (
                format!("附近有 Wi-Fi{}{}时", names.join(""), more),
                format!("When Wi-Fi {}{} is nearby", names_en.join(", "), more_en),
            )
        }
        When::Mcc(mccs) => {
            let names: Vec<String> = mccs.iter().take(3).map(|m| mcc_name(m)).collect();
            let names_en: Vec<String> = mccs.iter().take(3).map(|m| mcc_name_en(m)).collect();
            (
                format!("插的是{}的卡时", names.join("、")),
                format!("When the SIM is from {}", names_en.join(", ")),
            )
        }
        When::Abroad => (
            format!("插的不是{}的卡时（当地卡、境外 eSIM）", mcc_name(home_mcc)),
            format!(
                "SIM not from {} (local or foreign eSIM)",
                mcc_name_en(home_mcc)
            ),
        ),
        When::Fallback => (
            "其他情景都不符合时（默认）".into(),
            "When no other scenario matches (default)".into(),
        ),
    }
}

/// What entering a scenario changes, in Chinese and English. Never the cellular side: scenarios
/// may only touch Wi-Fi, services, power and thermal.
pub fn describe_does(actions: &[Does], inhibit_sleep: bool) -> (String, String) {
    let flag = |b: Option<&Value>, k: &str| b.and_then(|v| v.get(k)).and_then(Value::as_bool);
    let pair = |zh: &str, en: &str| (zh.to_string(), en.to_string());
    let mut out: Vec<(String, String)> = Vec::new();
    for a in actions {
        let b = a.body;
        let line = match a.path {
            "/api/wifi/radio" => match (flag(b, "ap_2g"), flag(b, "ap_5g")) {
                (Some(false), Some(false)) => pair("关 Wi-Fi", "Wi-Fi off"),
                (Some(true), Some(true)) => pair("开 Wi-Fi", "Wi-Fi on"),
                (x, y) => {
                    let w = |v: Option<bool>| {
                        if v == Some(false) {
                            ("关", "off")
                        } else {
                            ("开", "on")
                        }
                    };
                    (
                        format!("2.4G {} · 5G {}", w(x).0, w(y).0),
                        format!("2.4G {} · 5G {}", w(x).1, w(y).1),
                    )
                }
            },
            "/api/wifi/guest" => {
                if flag(b, "enabled") == Some(false) {
                    pair("关访客 Wi-Fi", "Guest Wi-Fi off")
                } else {
                    pair("开访客 Wi-Fi", "Guest Wi-Fi on")
                }
            }
            "/api/services/chill/enable" => pair("开 CHILL", "CHILL on"),
            "/api/services/chill/disable" => pair("关 CHILL", "CHILL off"),
            "/api/services/chill/regions" | "/api/services/chill/exit" => {
                let m = b
                    .and_then(|v| v.get("member"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                match m {
                    "DIRECT" => pair("CHILL 改成直连", "CHILL goes direct"),
                    "" => pair("改 CHILL 出口", "Change CHILL route"),
                    m => (format!("CHILL 走 {m}"), format!("CHILL via {m}")),
                }
            }
            "/api/services/chill/bypass" => pair("改 CHILL 分流", "Change CHILL split routing"),
            "/api/device/power-save" => pair("改省电设置", "Change power saving"),
            "/api/device/thermal" => pair("改温控", "Change thermal control"),
            p => pair(p, p),
        };
        if !out.contains(&line) {
            out.push(line);
        }
    }
    if inhibit_sleep {
        out.push(pair(
            "不休眠，Tailscale 一直连得上",
            "No sleep; Tailscale stays reachable",
        ));
    }
    if out.is_empty() {
        return pair("什么都不改", "Changes nothing");
    }
    let (zh, en): (Vec<String>, Vec<String>) = out.into_iter().unzip();
    (zh.join(" · "), en.join(" · "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_names_only_while_unrenamed() {
        assert_eq!(name_en("home", "在家"), Some("Home"));
        assert_eq!(name_en("away", "外出"), Some("Away"));
        assert_eq!(name_en("home", "我家"), None);
        assert_eq!(name_en("office", "在家"), None);
    }

    #[test]
    fn summary_fields() {
        let e = Engine {
            configured: true,
            enabled: true,
            current: "abroad",
            pin: None,
            guard_takeover: false,
            last_switch: Some(1_790_000_000),
        };
        let c = Current {
            id: "abroad",
            name: "国外",
            inhibit_sleep: false,
            abroad: true,
        };
        let v = public_summary(&e, Some(c));
        assert_eq!(
            (
                v["name"].as_str(),
                v["name_en"].as_str(),
                v["abroad"].as_bool()
            ),
            (Some("国外"), Some("Abroad"), Some(true))
        );
        assert_eq!(v["last_switch"], 1_790_000_000i64);
        // disabled: never abroad; no current scenario: empty name
        let off = Engine {
            enabled: false,
            ..e.clone()
        };
        assert_eq!(public_summary(&off, Some(c))["abroad"], false);
        let v = public_summary(&Engine { current: "", ..e }, None);
        assert_eq!(
            (
                v["name"].as_str(),
                v["name_en"].clone(),
                v["wifi_off"].clone()
            ),
            (Some(""), Value::Null, json!(false))
        );
    }
}
