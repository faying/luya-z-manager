//! Wi-Fi 设置事务（第四批 4-0b）：两项 `wifi.main`（`wifi.settings`，agent 的 PUT /api/wifi/settings）和
//! `wifi.guest`（`wifi.guest`，PUT /api/wifi/guest），请求体和 agent 的一样。纯函数；读写设备在
//! `ops::wifi_dev`。
//!
//! - **值**是 `wifi` 块（`snapshot::wifi_block`，5a56c9cf）展开成一层的键：`on`、`country`、
//!   `band_2g.ssid`、`guest.on_5g`……（`txn::matches` 只比一层，目标只带改了的键）。密码不进值：
//!   换成 `*.key_fp` = 本机密钥的 HMAC 指纹（和 APN 同一把 `apn.key`），空密码为 ""。
//! - **目标**：照 agent 的做法从请求体算出要写的 uci（顺序：国家码、逐键、总开关最后），套到现在的
//!   `wireless` 上，再经同一个 `wifi_block` 展开，和现在的展开比，只留变了的键。读回走同一条路，
//!   所以 sanitize、布尔写法、总开关压过 radio 开关这些都一样。一个键都没变 = 不算事务。
//! - **写**：目标（或退回、撤销的旧值）→ uci 的逆映射，只写和现状不同的；密码按指纹从本机保管箱
//!   `wifi-vault.json`（0600）取。保管箱里存现在的和新写的密码，不离开设备。
//! - Wi-Fi 6 跟着 radio 模式一起改，这里不支持：请求要改它就拒绝（agent 回 501）。

use super::apn::Vault;
use crate::project::snapshot;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

pub type Uci = BTreeMap<String, String>;

pub const VAULT_FILE: &str = "wifi-vault.json";
/// 两个主网络 + 访客的现密码，再加几次改动的新旧密码。
pub const VAULT_MAX: usize = 16;

pub const WIFI6_REFUSED: &str = "this firmware changes Wi-Fi 6 together with the radio mode; switching it here is not supported yet";
pub const KEY_LOST: &str = "the old Wi-Fi password was not kept on the device";

/// 两个频段：值里的前缀、主网络的 AP 节、radio 节、接口（txpower 热改用）。
const BANDS: [(&str, &str, &str, &str); 2] = [
    ("band_2g", "main_2g", "wifi0", "wlan0"),
    ("band_5g", "main_5g", "wifi1", "wlan2"),
];
const GUESTS: [&str; 2] = ["guest_2g", "guest_5g"];

/// 一个频段在值里的键（不含前缀）。
const BAND_FIELDS: &[&str] = &[
    "ap_on",
    "radio_on",
    "ssid",
    "encryption",
    "hidden",
    "channel",
    "txpower",
    "htmode",
    "key_fp",
];
const GUEST_FIELDS: &[&str] = &[
    "ssid",
    "encryption",
    "hidden",
    "isolate",
    "active_time",
    "on_2g",
    "on_5g",
    "key_fp",
];

/// 主 Wi-Fi 请求体的键 → uci 路径：`<项>_2g|_5g` 落到 `main_<频段>`（SSID、密码、加密、隐藏）
/// 或 radio 节 `wifi0`/`wifi1`（信道、功率、带宽），`radio2_disabled`/`radio5_disabled` 是 radio 开关。
/// 不认得的键 = `None`。
fn main_path(key: &str) -> Option<String> {
    match key {
        "radio2_disabled" => return Some("wireless.wifi0.disabled".into()),
        "radio5_disabled" => return Some("wireless.wifi1.disabled".into()),
        _ => {}
    }
    let (item, band) = key.rsplit_once('_')?;
    let (main, radio) = match band {
        "2g" => ("main_2g", "wifi0"),
        "5g" => ("main_5g", "wifi1"),
        _ => return None,
    };
    let path = match item {
        "ssid" | "key" | "encryption" | "hidden" => format!("wireless.{main}.{item}"),
        "channel" | "htmode" => format!("wireless.{radio}.{item}"),
        "txpower" => format!("wireless.{radio}.txpowerpercent"),
        _ => return None,
    };
    Some(path)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Main,
    Guest,
}

impl Kind {
    pub fn of(item: &str) -> Option<Self> {
        match item {
            super::spec::WIFI_MAIN => Some(Self::Main),
            super::spec::WIFI_GUEST => Some(Self::Guest),
            _ => None,
        }
    }

    fn keys(self) -> Vec<String> {
        match self {
            Self::Main => {
                let mut k = vec!["on".to_owned(), "country".to_owned()];
                for (b, ..) in BANDS {
                    k.extend(BAND_FIELDS.iter().map(|f| format!("{b}.{f}")));
                }
                k
            }
            Self::Guest => GUEST_FIELDS.iter().map(|f| format!("guest.{f}")).collect(),
        }
    }
}

/// 要写的 uci（路径 → 值，按写的顺序）和要不要先打开原厂总开关（`wifi.set_module {enabled:1}`）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    pub set: Vec<(String, String)>,
    pub vendor_on: bool,
}

impl Plan {
    /// 写一个值：同一路径后写的算；和现状一样就不写（之前排的也撤掉）。
    fn put(&mut self, cur: &Uci, path: &str, val: &str) {
        self.set.retain(|(p, _)| p != path);
        if get(cur, path) != val {
            self.set.push((path.to_owned(), val.to_owned()));
        }
    }

    pub fn is_empty(&self) -> bool {
        self.set.is_empty() && !self.vendor_on
    }

    /// 只改了发射功率：agent 用 iw 热改、不重载。返回要热改的 (接口, 百分比)。
    pub fn hot_txpower(&self) -> Option<Vec<(&'static str, u32)>> {
        if self.vendor_on || self.set.is_empty() {
            return None;
        }
        self.set
            .iter()
            .map(|(p, v)| {
                let iface = BANDS
                    .iter()
                    .find(|(_, _, radio, _)| *p == format!("wireless.{radio}.txpowerpercent"))?
                    .3;
                v.parse().ok().map(|n| (iface, n))
            })
            .collect()
    }

    /// `wifi.apply` 的 `set`。
    pub fn set_json(&self) -> Value {
        Value::Object(
            self.set
                .iter()
                .map(|(p, v)| (p.clone(), json!(v)))
                .collect(),
        )
    }
}

fn get<'a>(m: &'a Uci, path: &str) -> &'a str {
    m.get(path).map(String::as_str).unwrap_or("")
}

fn exists(m: &Uci, section: &str) -> bool {
    m.contains_key(&format!("wireless.{section}"))
}

/// agent `master_on_from`：原厂开关不是 "0"，且两个主网络不是都关着。
fn master_on(m: &Uci) -> bool {
    get(m, "wireless.zte_mbb.wifi_onoff") != "0"
        && !(get(m, "wireless.main_2g.disabled") == "1"
            && get(m, "wireless.main_5g.disabled") == "1")
}

/// agent `wifi6_from`：缺省算开，只有 "0" 算关。
fn wifi6(raw: &str) -> &'static str {
    if raw == "0" { "0" } else { "1" }
}

/// agent `master_writes`：开 = 两个 AP 和两个 radio 都开；关 = 两个 AP 关。已经是这样就不写。
fn master_writes(want_on: bool, current_on: bool) -> Vec<(&'static str, &'static str)> {
    if want_on == current_on {
        return Vec::new();
    }
    if want_on {
        vec![
            ("wireless.wifi0.disabled", "0"),
            ("wireless.wifi1.disabled", "0"),
            ("wireless.main_2g.disabled", "0"),
            ("wireless.main_5g.disabled", "0"),
        ]
    } else {
        vec![
            ("wireless.main_2g.disabled", "1"),
            ("wireless.main_5g.disabled", "1"),
        ]
    }
}

/// 总开关关 = 访客也关（有的节）：两个主网络关了、访客还开着，固件跑不起来（`luyaz_wifi::layout`
/// 的 GUEST_ALONE），关 Wi-Fi 就会被拒。开的时候访客不动（和原厂一样，要自己再开）。
fn guests_off_with_mains(plan: &mut Plan, cur: &Uci, want_on: bool) {
    if want_on || master_on(cur) == want_on {
        return;
    }
    for g in GUESTS.iter().filter(|g| exists(cur, g)) {
        plan.put(cur, &format!("wireless.{g}.disabled"), "1");
    }
}

/// 请求体里的值 → uci 写法（agent：字符串原样、数字十进制、布尔 1/0，再 sanitize）；别的类型不认。
fn body_str(v: &Value) -> Option<String> {
    let s = match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => if *b { "1" } else { "0" }.to_owned(),
        _ => return None,
    };
    Some(luyaz_api::wifi::sanitize_uci_value(&s))
}

fn check_value(name: &str, v: &str) -> Result<(), String> {
    if v.len() > 128 || v.contains(['\0', '\r', '\n']) {
        return Err(format!("{name}: invalid value"));
    }
    Ok(())
}

/// WPA 密码：空（不加密时）或 8–64 个字符；更短的 hostapd 起不来，Wi-Fi 就没了。
fn check_key(name: &str, v: &str) -> Result<(), String> {
    check_value(name, v)?;
    if !v.is_empty() && !(8..=64).contains(&v.chars().count()) {
        return Err(format!("{name} must be 8-64 characters"));
    }
    Ok(())
}

/// `{restore: 值}`：只认这一项的键，类型对得上。
fn restored(kind: Kind, params: &Value) -> Option<Result<Value, String>> {
    let r = params.get("restore")?;
    let keys = kind.keys();
    let ok = r.as_object().is_some_and(|m| {
        !m.is_empty()
            && m.iter().all(|(k, v)| {
                keys.contains(k)
                    && match k.rsplit('.').next().unwrap_or(k) {
                        "on" | "ap_on" | "radio_on" | "hidden" | "isolate" | "on_2g" | "on_5g" => {
                            v.is_boolean()
                        }
                        "active_time" => v.is_i64(),
                        _ => v.as_str().is_some_and(|s| check_value(k, s).is_ok()),
                    }
            })
    });
    Some(if ok {
        Ok(r.clone())
    } else {
        Err("restore must be a Wi-Fi settings value".into())
    })
}

/// 提交前的目标值占位（真目标要看设备现状，`resolve` 里算）。
fn pending() -> Value {
    json!({ "pending": true })
}

/// `wifi.settings` 的参数检查（`Spec::normalize`）：`{restore}` 原样；否则逐个看认得的键。
pub fn check_main(params: &Value) -> Result<Value, String> {
    if let Some(r) = restored(Kind::Main, params) {
        return r;
    }
    let body = params.as_object().ok_or("params must be an object")?;
    for (k, v) in body {
        let known = main_path(k).is_some()
            || matches!(k.as_str(), "country" | "wifi_onoff" | "wifi6_switch");
        let Some(s) = known.then(|| body_str(v)).flatten() else {
            continue;
        };
        if k.starts_with("key_") {
            check_key(k, &s)?;
        } else {
            check_value(k, &s)?;
        }
    }
    Ok(pending())
}

/// `wifi.guest` 的参数检查。
pub fn check_guest(params: &Value) -> Result<Value, String> {
    if let Some(r) = restored(Kind::Guest, params) {
        return r;
    }
    let body = params.as_object().ok_or("params must be an object")?;
    let set = luyaz_api::wifi::guest_set(&serde_json::to_vec(body).expect("params serialize"))
        .map_err(|(_, e)| e["error"].as_str().unwrap_or("invalid body").to_owned())?;
    for (path, v) in &set {
        let s = v.as_str().unwrap_or_default();
        if path.ends_with(".key") {
            check_key("guest_key", s)?;
        } else {
            check_value(path, s)?;
        }
    }
    Ok(pending())
}

/// 主 Wi-Fi 的写：国家码（两个 radio）、逐键、总开关最后（压过 radio 开关）；只写和现状不同的。
pub fn main_plan(body: &Map<String, Value>, cur: &Uci) -> Result<Plan, String> {
    if let Some(v) = body.get("wifi6_switch").and_then(body_str)
        && wifi6(&v) != wifi6(get(cur, "wireless.zte_mbb.wifi6_switch"))
    {
        return Err(WIFI6_REFUSED.into());
    }
    let mut plan = Plan::default();
    if let Some(cc) = body.get("country").and_then(Value::as_str) {
        let cc = luyaz_api::wifi::sanitize_uci_value(cc).to_uppercase();
        if cc.len() == 2 && cc.chars().all(|c| c.is_ascii_alphabetic()) {
            for (.., radio, _) in BANDS {
                plan.put(cur, &format!("wireless.{radio}.country"), &cc);
            }
        }
    }
    for (key, value) in body {
        if let (Some(path), Some(v)) = (main_path(key), body_str(value)) {
            plan.put(cur, &path, &v);
        }
    }
    if let Some(want) = body.get("wifi_onoff").and_then(body_str) {
        let want_on = want != "0";
        plan.vendor_on = want_on && get(cur, "wireless.zte_mbb.wifi_onoff") == "0";
        for (path, v) in master_writes(want_on, master_on(cur)) {
            plan.put(cur, path, v);
        }
        guests_off_with_mains(&mut plan, cur, want_on);
    }
    Ok(plan)
}

/// 访客 Wi-Fi 的写（`luyaz_api::wifi::guest_set`），只写有的节、
/// 和现状不同的。
pub fn guest_plan(body: &Map<String, Value>, cur: &Uci) -> Result<Plan, String> {
    let set = luyaz_api::wifi::guest_set(&serde_json::to_vec(body).expect("params serialize"))
        .map_err(|(_, e)| e["error"].as_str().unwrap_or("invalid body").to_owned())?;
    let mut plan = Plan::default();
    for (path, v) in &set {
        let section = path.split('.').nth(1).unwrap_or_default();
        if exists(cur, section) {
            plan.put(cur, path, v.as_str().unwrap_or_default());
        }
    }
    Ok(plan)
}

/// 把计划套到现状上（写完之后 uci 会是这样）。
pub fn simulate(cur: &Uci, plan: &Plan) -> Uci {
    let mut m = cur.clone();
    for (p, v) in &plan.set {
        m.insert(p.clone(), v.clone());
    }
    if plan.vendor_on {
        m.insert("wireless.zte_mbb.wifi_onoff".into(), "1".into());
    }
    m
}

/// 一个密码的指纹；空密码为 ""。
pub fn key_fp(vault: &Vault, key: &str) -> String {
    if key.is_empty() {
        String::new()
    } else {
        vault.fp(&json!({ "wifi_key": key }))
    }
}

/// 这一项的值：`wifi` 块（同一个 `snapshot::wifi_block`）展开成一层，`has_password` 换成指纹。
/// 没有 wireless 配置为 None。
pub fn value(kind: Kind, m: &Uci, vault: &Vault) -> Option<Map<String, Value>> {
    let block = snapshot::wifi_block(std::slice::from_ref(m), None)?;
    let block = serde_json::to_value(block).expect("wifi block serializes");
    let mut out = Map::new();
    let mut put_band = |prefix: &str, b: &Value, key_path: &str, fields: &[&str]| {
        for f in fields {
            let v = if *f == "key_fp" {
                json!(key_fp(vault, get(m, key_path)))
            } else {
                b.get(*f).cloned().unwrap_or(Value::Null)
            };
            out.insert(format!("{prefix}.{f}"), v);
        }
    };
    match kind {
        Kind::Main => {
            for (b, ap, ..) in BANDS {
                put_band(b, &block[b], &format!("wireless.{ap}.key"), BAND_FIELDS);
            }
            out.insert("on".into(), block["on"].clone());
            out.insert("country".into(), block["country"].clone());
        }
        Kind::Guest => put_band(
            "guest",
            &block["guest"],
            "wireless.guest_2g.key",
            GUEST_FIELDS,
        ),
    }
    Some(out)
}

/// 和现状比变了的键（目标值）。
pub fn changed(cur: &Map<String, Value>, new: &Map<String, Value>) -> Map<String, Value> {
    new.iter()
        .filter(|(k, v)| cur.get(*k) != Some(*v))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// 新写的密码（计划里写的 `*.key`）：放进保管箱，退回、撤销要用。
pub fn new_keys(plan: &Plan) -> Vec<&str> {
    plan.set
        .iter()
        .filter(|(p, _)| p.ends_with(".key"))
        .map(|(_, v)| v.as_str())
        .filter(|v| !v.is_empty())
        .collect()
}

/// 现状里的密码（写之前放进保管箱）。
pub fn current_keys(kind: Kind, m: &Uci) -> Vec<&str> {
    let sections: &[&str] = match kind {
        Kind::Main => &["main_2g", "main_5g"],
        Kind::Guest => &GUESTS,
    };
    sections
        .iter()
        .map(|s| get(m, &format!("wireless.{s}.key")))
        .filter(|k| !k.is_empty())
        .collect()
}

fn flag(b: bool) -> &'static str {
    if b { "1" } else { "0" }
}

/// 值 → 要写的 uci（逆映射），只写和现状不同的键；总开关最后。密码按指纹从保管箱取（`lookup`）。
/// 新写、退回、撤销都走这里。
pub fn plan_for(
    kind: Kind,
    want: &Map<String, Value>,
    cur: &Uci,
    now: &Map<String, Value>,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<Plan, String> {
    let mut plan = Plan::default();
    let mut master = None;
    let s = |v: &Value| match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    };
    let b = |v: &Value| v.as_bool().unwrap_or(false);
    for (k, v) in want {
        if now.get(k) == Some(v) {
            continue;
        }
        let (prefix, field) = k.split_once('.').unwrap_or(("", k));
        let key = |fp: &Value| -> Result<String, String> {
            let fp = s(fp);
            if fp.is_empty() {
                return Ok(String::new());
            }
            lookup(&fp).ok_or_else(|| KEY_LOST.to_owned())
        };
        match (kind, prefix, field) {
            (Kind::Main, "", "on") => master = Some(b(v)),
            (Kind::Main, "", "country") => {
                for (.., radio, _) in BANDS {
                    plan.put(cur, &format!("wireless.{radio}.country"), &s(v));
                }
            }
            (Kind::Main, band, f) => {
                let (_, ap, radio, _) = BANDS
                    .iter()
                    .find(|(p, ..)| *p == band)
                    .ok_or_else(|| format!("not a Wi-Fi setting: {k}"))?;
                let (path, val) = match f {
                    "ap_on" => (format!("{ap}.disabled"), flag(!b(v)).to_owned()),
                    "radio_on" => (format!("{radio}.disabled"), flag(!b(v)).to_owned()),
                    "ssid" | "encryption" => (format!("{ap}.{f}"), s(v)),
                    "hidden" => (format!("{ap}.hidden"), flag(b(v)).to_owned()),
                    "channel" | "htmode" => (format!("{radio}.{f}"), s(v)),
                    "txpower" => (format!("{radio}.txpowerpercent"), s(v)),
                    "key_fp" => (format!("{ap}.key"), key(v)?),
                    _ => return Err(format!("not a Wi-Fi setting: {k}")),
                };
                plan.put(cur, &format!("wireless.{path}"), &val);
            }
            (Kind::Guest, "guest", f) => {
                let (sections, opt, val): (&[&str], &str, String) = match f {
                    "on_2g" => (&GUESTS[..1], "disabled", flag(!b(v)).into()),
                    "on_5g" => (&GUESTS[1..], "disabled", flag(!b(v)).into()),
                    "ssid" | "encryption" => (&GUESTS, f, s(v)),
                    "hidden" | "isolate" => (&GUESTS, f, flag(b(v)).into()),
                    "active_time" => (&GUESTS, "guest_active_time", s(v)),
                    "key_fp" => (&GUESTS, "key", key(v)?),
                    _ => return Err(format!("not a Wi-Fi setting: {k}")),
                };
                for sec in sections.iter().filter(|s| exists(cur, s)) {
                    plan.put(cur, &format!("wireless.{sec}.{opt}"), &val);
                }
            }
            _ => return Err(format!("not a Wi-Fi setting: {k}")),
        }
    }
    if let Some(want_on) = master {
        plan.vendor_on = want_on && get(cur, "wireless.zte_mbb.wifi_onoff") == "0";
        for (path, v) in master_writes(want_on, master_on(cur)) {
            plan.put(cur, path, v);
        }
        guests_off_with_mains(&mut plan, cur, want_on);
    }
    Ok(plan)
}

/// 只留 `keys` 里有的键（撤销：旧值只退回那次改过的键，别的不动）。
pub fn project(v: &Value, keys: &Value) -> Value {
    match (v.as_object(), keys.as_object()) {
        (Some(v), Some(k)) => Value::Object(
            v.iter()
                .filter(|(key, _)| k.contains_key(*key))
                .map(|(a, b)| (a.clone(), b.clone()))
                .collect(),
        ),
        _ => v.clone(),
    }
}

/// 界面文字：总开关、名称，其余只说几项（密码不显示，指纹也不显示）。
pub fn words(v: &Value) -> (String, String) {
    let Some(m) = v.as_object() else {
        return ("-".into(), "-".into());
    };
    let mut zh = Vec::new();
    let mut en = Vec::new();
    let mut used = 0;
    if let Some(on) = m.get("on").and_then(Value::as_bool) {
        used += 1;
        zh.push(if on { "Wi-Fi 开" } else { "Wi-Fi 关" }.to_owned());
        en.push(if on { "Wi-Fi on" } else { "Wi-Fi off" }.to_owned());
    }
    let guest_on = ["guest.on_2g", "guest.on_5g"]
        .iter()
        .filter_map(|k| m.get(*k).and_then(Value::as_bool))
        .collect::<Vec<_>>();
    if !guest_on.is_empty() {
        used += guest_on.len();
        let on = guest_on.iter().any(|b| *b);
        zh.push(if on { "访客开" } else { "访客关" }.to_owned());
        en.push(if on { "guest on" } else { "guest off" }.to_owned());
    }
    for (k, zh_l, en_l) in [
        ("band_2g.ssid", "2.4G ", "2.4G "),
        ("band_5g.ssid", "5G ", "5G "),
        ("guest.ssid", "", ""),
    ] {
        if let Some(s) = m.get(k).and_then(Value::as_str) {
            used += 1;
            zh.push(format!("{zh_l}{s}"));
            en.push(format!("{en_l}{s}"));
        }
    }
    let rest = m.len().saturating_sub(used);
    if rest > 0 {
        if zh.is_empty() {
            zh.push(format!("{rest} 项设置"));
            en.push(format!(
                "{rest} setting{}",
                if rest == 1 { "" } else { "s" }
            ));
        } else {
            zh.push(format!("另 {rest} 项"));
            en.push(format!("+{rest} more"));
        }
    }
    if zh.is_empty() {
        return ("-".into(), "-".into());
    }
    (zh.join(" · "), en.join(" · "))
}

#[cfg(test)]
mod tests;
