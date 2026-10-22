//! src/diag_view.c: luyaz-core's diagnosis run parsed, and the words the 网络诊断 page uses.
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use crate::json;
use crate::lang::{self, tr};
use ui_model::{diag_run_t, dg_layer_t};

pub const DG_LAYERS_MAX: usize = 12;
// dg_level_t
pub const DG_PENDING: i32 = 0;
pub const DG_RUNNING: i32 = 1;
pub const DG_OK: i32 = 2;
pub const DG_WARN: i32 = 3;
pub const DG_BAD: i32 = 4;
pub const DG_NA: i32 = 5;
pub const DG_INFO: i32 = 6;
/// not run: a layer before it is bad (deep-diag-v2 §3); grey, not counted
pub const DG_BLOCKED: i32 = 7;
// dg_state_t
pub const DG_NONE: i32 = 0;
pub const DG_IDLE: i32 = 1;
pub const DG_WAITING: i32 = 2;
pub const DG_RUN: i32 = 3;
pub const DG_DONE: i32 = 4;

fn str(o: &[u8], key: &str, cap: usize) -> String {
    match json::get(o, key, cap) {
        Some(v) if v != b"null" => String::from_utf8_lossy(&v).into_owned(),
        _ => String::new(),
    }
}

/// <key>_en in English when it is there and not empty, else <key>
fn text(o: &[u8], key: &str, cap: usize) -> String {
    if lang::is_en() {
        let v = str(o, &format!("{key}_en"), cap);
        if !v.is_empty() {
            return v;
        }
    }
    str(o, key, cap)
}

/// A number that may be null: def then (strtol: no digits = 0).
fn num(o: &[u8], key: &str, def: i64) -> i64 {
    match json::get(o, key, 24) {
        Some(v) if !v.is_empty() && v != b"null" => json::atol(&v),
        _ => def,
    }
}

fn level(s: &str) -> i32 {
    const NAMES: [&str; 8] = ["pending", "running", "ok", "warn", "bad", "na", "info", "blocked"];
    // a level from a later agent: "can't tell", never good
    NAMES.iter().position(|&n| n == s).map_or(DG_NA, |i| i as i32)
}

fn layer(o: &[u8]) -> dg_layer_t {
    dg_layer_t {
        id: str(o, "id", 12),
        level: level(&str(o, "level", 16)),
        detail: text(o, "detail", 192),
        counted: !json::get(o, "counted", 16).is_some_and(|v| v == b"false") as i32,
    }
}

/// The run in GET/POST /api/diagnose's "data" (diag_view_parse): Some on a run or on
/// {"state":"idle"}; None (and the run DG_NONE) when it is neither.
pub fn parse(d: &[u8]) -> (bool, diag_run_t) {
    let mut r = diag_run_t { layer: vec![dg_layer_t::default(); DG_LAYERS_MAX], feedback: -1, age_s: -1, ..Default::default() };
    let Some(v) = json::get(d, "state", 24) else { return (false, r) };
    r.state = match &v[..] {
        b"idle" => {
            r.state = DG_IDLE;
            return (true, r);
        }
        b"waiting" => DG_WAITING,
        b"running" => DG_RUN,
        b"done" => DG_DONE,
        _ => return (false, r),
    };
    r.id = num(d, "id", 0);
    r.waiting_for = str(d, "waiting_for", 16);
    r.asked_at = num(d, "asked_at", 0);
    r.started_at = num(d, "started_at", 0);
    r.finished_at = num(d, "finished_at", 0);
    r.step = num(d, "step", 0) as i32;
    r.steps = num(d, "steps", 0) as i32;
    r.age_s = num(d, "age_s", -1);
    if let Some(arr) = json::get(d, "layers", 12288)
        && arr.first() == Some(&b'[')
    {
        let mut p = 0;
        while (r.n as usize) < DG_LAYERS_MAX {
            let Some((item, np)) = json::arr_next(&arr, p, 768) else { break };
            p = np;
            if item.first() == Some(&b'{') {
                r.layer[r.n as usize] = layer(&item);
                r.n += 1;
            }
        }
    }
    if let Some(o) = json::get(d, "main", 1024)
        && o.first() == Some(&b'{')
    {
        r.has_main = 1;
        r.main_layer = str(&o, "layer", 12);
        let v = str(&o, "level", 24);
        if !v.is_empty() {
            r.main_has_level = 1;
            r.main_level = level(&v);
        }
        r.main_text = text(&o, "text", 96);
        r.main_action = text(&o, "action", 160);
        r.action_to = str(&o, "action_to", 16);
        r.more = num(&o, "more", 0) as i32;
    }
    if let Some(v) = json::get(d, "feedback", 24) {
        r.feedback = match &v[..] {
            b"true" => 1,
            b"false" => 0,
            _ => -1,
        };
    }
    if let Some(o) = json::get(d, "speed", 1024)
        && o.first() == Some(&b'{')
    {
        r.has_speed = 1;
        r.speed = layer(&o);
    }
    (true, r)
}

pub fn layer_name(id: &str) -> &str {
    match id {
        "wifi" => "Wi-Fi",
        "sim" => tr("SIM 卡"),
        "signal" => tr("信号"),
        "limit" => tr("限速"),
        "bearer" => tr("数据连接"),
        "link" => tr("蜂窝链路"),
        "dns" => tr("域名解析"),
        "e2e" => tr("外网"),
        "clock" => tr("设备时间"),
        "crowd" => tr("基站负载"),
        "proxy" => tr("代理"),
        "speed" => tr("速度"),
        _ => id,
    }
}

pub fn level_word(l: i32) -> &'static str {
    match l {
        DG_PENDING => tr("等待"),
        DG_RUNNING => tr("测试中…"),
        DG_OK => tr("正常"),
        DG_WARN => tr("疑点"),
        DG_BAD => crate::trc!("诊断", "差"),
        DG_NA => tr("测不了"),
        DG_BLOCKED => tr("没测"),
        _ => "",
    }
}

pub fn level_mark(l: i32) -> &'static str {
    match l {
        DG_OK | DG_NA | DG_BLOCKED => "●",
        DG_WARN => "▲",
        DG_BAD => "■",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// tests/diag_view_test.c 「words」
    #[test]
    fn words() {
        let _l = crate::lang::TEST_LANG.lock().unwrap_or_else(|e| e.into_inner());
        crate::lang::set_en(false);
        assert_eq!(layer_name("wifi"), "Wi-Fi");
        assert_eq!(layer_name("crowd"), "基站负载");
        assert_eq!(layer_name("future"), "future");
        assert_eq!(level_word(DG_OK), "正常");
        assert_eq!(level_word(DG_BAD), "差");
        assert!(level_word(DG_INFO).is_empty());
        assert_eq!(level_mark(DG_WARN), "▲");
        assert_eq!(level_mark(DG_BAD), "■");
        assert!(level_mark(DG_PENDING).is_empty());
        assert_eq!(layer_name("bearer"), "数据连接");
        assert_eq!(layer_name("e2e"), "外网");
        assert_eq!(level_word(DG_BLOCKED), "没测");
        assert_eq!(level_mark(DG_BLOCKED), "●");
        crate::lang::set_en(true);
        assert_eq!(layer_name("dns"), "DNS");
        assert_eq!(layer_name("clock"), "Device clock");
        assert_eq!(level_word(DG_BLOCKED), "Not checked");
        crate::lang::set_en(false);
    }
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    fn runs(name: &str) -> String {
        crate::testc::c_string(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/diag_runs.h")), name)
    }

    fn find<'a>(r: &'a diag_run_t, id: &str) -> Option<&'a dg_layer_t> {
        r.layer[..r.n as usize].iter().find(|l| l.id == id)
    }

    // tests/diag_view_test.c
    #[test]
    fn parses_runs() {
        // nothing, or not a run
        assert!(!parse(b"").0 && parse(b"").1.state == DG_NONE);
        assert!(!parse(b"{}").0);
        assert!(!parse(br#"{"state":"later"}"#).0 && parse(br#"{"state":"later"}"#).1.state == DG_NONE);
        assert!(!parse(b"not json").0);
        let (ok, r) = parse(br#"{"state":"idle"}"#);
        assert!(ok && r.state == DG_IDLE && r.n == 0 && r.feedback == -1);

        // running: rows in order, pending / running ones empty, no main yet
        lang::set_en(false);
        let (ok, r) = parse(runs("k_diag_running").as_bytes());
        assert!(ok && r.state == DG_RUN && r.id == 7);
        assert!(r.step == 3 && r.steps >= 5 && r.n >= 5 && r.has_main == 0 && r.has_speed == 0);
        assert!(r.layer[0].id == "wifi" && r.layer[0].level == DG_OK && r.layer[0].detail.contains("iPad"));
        assert!(r.layer[3].level == DG_RUNNING && r.layer[3].detail.is_empty());
        assert!(r.layer[4].level == DG_PENDING && r.layer[4].counted != 0);
        assert!(r.finished_at == 0 && r.age_s == -1 && r.feedback == -1);

        // done: main, more, na rows (one not counted), the speed row
        let (ok, r) = parse(runs("k_diag_result").as_bytes());
        assert!(ok && r.state == DG_DONE);
        assert!(r.has_main != 0 && r.main_layer == "link" && r.main_has_level != 0 && r.main_level == DG_WARN && r.more == 1);
        assert!(r.main_text == "蜂窝链路不稳" && r.main_action == "过几分钟再试，或换个地方" && r.action_to.is_empty());
        let l = find(&r, "wifi").unwrap();
        assert!(l.level == DG_NA && l.counted == 0 && l.detail == "没有设备连着");
        let l = find(&r, "limit").unwrap();
        assert!(l.level == DG_NA && l.counted != 0);
        assert!(r.has_speed != 0 && r.speed.level == DG_INFO && r.speed.detail == "直连 ↓ 86 Mbps");
        assert!(r.age_s == 12 && r.finished_at == 1790249511 && r.feedback == -1);
        assert!(find(&r, "nope").is_none());

        // English: every *_en sibling
        lang::set_en(true);
        let (ok, r) = parse(runs("k_diag_result").as_bytes());
        assert!(ok && r.main_text == "Cellular link unstable" && r.main_action == "Try again in a few minutes, or move");
        assert_eq!(find(&r, "wifi").unwrap().detail, "No devices on Wi-Fi");
        assert_eq!(r.speed.detail, "Direct ↓ 86 Mbps");
        lang::set_en(false);

        // the weak-signal run: placement action, feedback already given
        let (ok, r) = parse(runs("k_diag_weak").as_bytes());
        assert!(ok && r.action_to == "placement" && r.main_level == DG_BAD && r.feedback == 1 && r.more == 0);

        // all good: main without a layer or level
        let (ok, r) = parse(r#"{"id":2,"state":"done","layers":[],"main":{"layer":"","level":null,"text":"没查到问题","text_en":"No problem found","action":"可能是对方网站慢；也可以加测速度","action_en":"","action_to":"","more":0},"feedback":false}"#.as_bytes());
        assert!(ok && r.has_main != 0 && r.main_layer.is_empty() && r.main_has_level == 0 && r.feedback == 0 && r.n == 0);
        lang::set_en(true); // an empty _en falls back to the Chinese
        let (ok, r) = parse(r#"{"state":"done","main":{"text":"中","text_en":""}}"#.as_bytes());
        assert!(ok && r.main_text == "中");
        lang::set_en(false);

        // a level from a later agent reads as "can't tell"; more rows than slots
        assert_eq!(parse(br#"{"state":"done","layers":[{"id":"x","level":"new"}]}"#).1.layer[0].level, DG_NA);
        let ls: Vec<String> = (0..16).map(|i| format!(r#"{{"id":"l{i}","level":"ok"}}"#)).collect();
        let (ok, r) = parse(format!(r#"{{"state":"running","layers":[{}]}}"#, ls.join(",")).as_bytes());
        assert!(ok && r.n as usize == DG_LAYERS_MAX && r.layer[11].id == "l11");
        // a detail longer than the slot is cut
        let (ok, r) = parse(format!(r#"{{"state":"done","layers":[{{"id":"wifi","level":"ok","detail":"{}"}}]}}"#, "a".repeat(300)).as_bytes());
        assert!(ok && r.layer[0].detail.len() == 191);
    }

    /// deep-diag-v2: eleven rows with long details all fit, blocked ones grey and not counted
    #[test]
    fn a_v2_run_with_blocked_layers() {
        let _l = crate::lang::TEST_LANG.lock().unwrap_or_else(|e| e.into_inner());
        lang::set_en(false);
        let long = "很长的说明文字".repeat(9);
        let ids = ["wifi", "sim", "signal", "limit", "bearer", "link", "dns", "e2e", "clock", "crowd", "proxy"];
        let ls: Vec<String> = ids
            .iter()
            .enumerate()
            .map(|(i, id)| {
                let (lv, counted) = if i > 4 { ("blocked", false) } else { ("ok", true) };
                format!(r#"{{"id":"{id}","level":"{lv}","detail":"{long}","detail_en":"{long}","counted":{counted}}}"#)
            })
            .collect();
        let d = format!(r#"{{"state":"done","layers":[{}],"main":{{"layer":"bearer","level":"bad","text":"数据连接没建立","more":0}}}}"#, ls.join(","));
        let (ok, r) = parse(d.as_bytes());
        assert!(ok && r.n == 11, "{}", r.n);
        let l = find(&r, "proxy").unwrap();
        assert!(l.level == DG_BLOCKED && l.counted == 0 && l.detail == long);
        assert!(find(&r, "bearer").unwrap().level == DG_OK && r.main_layer == "bearer");
    }
}
