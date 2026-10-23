//! luyaz-core's /v2/screen "net" parsed (the rules are the core's), its verdict
//! codes, "is this a verdict 网络诊断 can say more about", and the placeholder view.
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use crate::json;
use crate::lang;
use crate::logic::{CAUSE_NONE, NET_BAD, NET_NEUTRAL, NET_OK, NET_WARN};
use ui_model::{net_view_t, nv_carrier_t};

pub const NV_CA_MAX: usize = 5;

// core story.state (screen.rs State)
pub const ST_UNKNOWN: i32 = 0;
pub const ST_OK: i32 = 1;
pub const ST_NOSIM: i32 = 2;
pub const ST_AIRPLANE: i32 = 3;
pub const ST_SOS: i32 = 4;
pub const ST_NOSVC: i32 = 5;
pub const ST_NODATA: i32 = 6;
pub const ST_LIMIT: i32 = 7;
pub const ST_WEAK: i32 = 8;
pub const ST_NOISE: i32 = 9;
pub const ST_CROWD: i32 = 10;
pub const ST_ONLY2G: i32 = 11;
pub const ST_ONLY3G: i32 = 12;
pub const ST_NARROW: i32 = 13;
pub const ST_STALL: i32 = 14;
pub const ST_CHANGING: i32 = 15;
pub const ST_REVERT_FAIL: i32 = 16;
pub const ST_HOT: i32 = 17;

/// Slow, not connected, connected but nothing comes back (slow-diagnosis §12.2).
pub fn abnormal(st: i32, cause: i32) -> bool {
    match st {
        ST_LIMIT | ST_WEAK | ST_NOISE | ST_CROWD | ST_NARROW | ST_NODATA | ST_STALL => true,
        ST_UNKNOWN => cause != CAUSE_NONE,
        _ => false,
    }
}

fn str(o: &[u8], key: &str, cap: usize) -> String {
    match json::get(o, key, cap) {
        Some(v) if v != b"null" => String::from_utf8_lossy(&v).into_owned(),
        _ => String::new(),
    }
}

/// A text field: in English its <key>_en sibling when core sent one that is not empty.
fn text(o: &[u8], key: &str, cap: usize) -> String {
    if lang::is_en() {
        let k = format!("{key}_en");
        let v = str(o, &k[..k.len().min(23)], cap);
        if !v.is_empty() {
            return v;
        }
    }
    str(o, key, cap)
}

fn flag(o: &[u8], key: &str) -> i32 {
    json::get(o, key, 8).is_some_and(|v| v == b"true") as i32
}

fn tone(o: &[u8], key: &str) -> i32 {
    match json::get(o, key, 12).as_deref() {
        Some(b"ok") => NET_OK,
        Some(b"warn") => NET_WARN,
        Some(b"bad") => NET_BAD,
        _ => NET_NEUTRAL,
    }
}

fn cause(o: &[u8]) -> i32 {
    const NAMES: [&str; 6] = ["none", "limit", "weak", "noise", "crowd", "narrow"];
    json::get(o, "cause", 12).and_then(|v| NAMES.iter().position(|n| n.as_bytes() == v.as_slice())).map_or(CAUSE_NONE, |i| i as i32)
}

fn state(o: &[u8]) -> i32 {
    const NAMES: [&str; 18] = [
        "", "ok", "nosim", "airplane", "sos", "nosvc", "nodata", "limit", "weak", "noise", "crowd", "only2g", "only3g", "narrow", "stall",
        "changing", "revert_fail", "hot",
    ];
    match json::get(o, "state", 12) {
        Some(v) if !v.is_empty() => NAMES.iter().skip(1).position(|n| n.as_bytes() == v.as_slice()).map_or(ST_UNKNOWN, |i| i as i32 + 1),
        _ => ST_UNKNOWN,
    }
}

fn carrier(o: &[u8]) -> nv_carrier_t {
    nv_carrier_t {
        kind: if str(o, "kind", 8) == "nr" { b'n' as i32 } else { b'B' as i32 },
        band: json::get_int(o, "band", 0) as i32,
        pci: json::get_int(o, "pci", 0) as i32,
        bw: json::get_int(o, "bw", 0) as i32,
        active: flag(o, "active"),
        arfcn: json::get_int(o, "arfcn", 0),
        rsrp: str(o, "rsrp", 12),
        rsrq: str(o, "rsrq", 12),
        sinr: str(o, "sinr", 12),
        label: str(o, "label", 16),
        label_short: str(o, "label_short", 16),
        sinr_tone: tone(o, "sinr_tone"),
    }
}

/// Parse /v2/screen's "net" (net_view_parse). true = it carried a verdict. In English each
/// text field is read from its <field>_en sibling when that is there and not empty.
pub fn parse(net: &[u8]) -> (bool, net_view_t) {
    let mut v = net_view_t { ca: vec![nv_carrier_t::default(); NV_CA_MAX], bars_tier: -1, ..Default::default() };
    let Some(mut st) = json::get(net, "story", 1024) else { return (false, v) };
    // E4: "home" = the story with a write transaction laid over it, only while there is one
    if let Some(home) = json::get(net, "home", 1024)
        && home.first() == Some(&b'{')
    {
        st = home;
    }
    if let Some(arr) = json::get(net, "carriers", 4096)
        && arr.first() == Some(&b'[')
    {
        let mut p = 0;
        while (v.ca_n as usize) < NV_CA_MAX {
            let Some((item, np)) = json::arr_next(&arr, p, 768) else { break };
            p = np;
            if item.first() == Some(&b'{') {
                v.ca[v.ca_n as usize] = carrier(&item);
                v.ca_n += 1;
            }
        }
    }
    v.act_n = json::get_int(net, "act_n", 0) as i32;
    v.roam_known = flag(net, "roam_known");
    v.roam = flag(net, "roam");
    v.nosvc = flag(net, "nosvc");
    v.sim_usable = flag(net, "sim_usable");
    v.other = flag(net, "other");
    v.logo = str(net, "logo", 24);
    v.fine = text(net, "fine", 32);
    v.name = text(net, "name", 48);
    v.r#where = text(net, "where", 64);
    v.ca_val = text(net, "ca_val", 48);
    v.ca_sub = text(net, "ca_sub", 160);
    v.bars_tier = json::get_int(net, "bars_tier", -1) as i32;
    v.mode_word = text(net, "mode_word", 32);
    v.mode_auto = flag(net, "mode_auto");
    let s = &mut v.story;
    s.tone = tone(&st, "tone");
    s.cause = cause(&st);
    s.sig_tone = tone(&st, "sig_tone");
    s.noise_tone = tone(&st, "noise_tone");
    s.headline = text(&st, "headline", 32);
    s.hint = text(&st, "hint", 128);
    s.rat = text(&st, "rat", 32);
    s.link = text(&st, "link", 96);
    s.sig = text(&st, "sig", 8);
    s.noise = text(&st, "noise", 8);
    s.load = text(&st, "load", 12);
    s.limit = text(&st, "limit", 8);
    v.state = state(&st);
    let ok = !v.story.headline.is_empty();
    (ok, v)
}

/// A view that only says core has not given one (yet).
pub fn placeholder(headline: &str, why: &str) -> net_view_t {
    let mut v = net_view_t { ca: vec![nv_carrier_t::default(); NV_CA_MAX], bars_tier: -1, sim_usable: 1, ..Default::default() };
    v.story.tone = NET_NEUTRAL;
    v.story.headline = headline.into();
    v.name = why.into();
    v.ca_val = "—".into();
    v.mode_word = "-".into();
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logic::*;

    // the abnormal and placeholder cases (parsing is the data side's)
    #[test]
    fn abnormal_verdicts() {
        for s in [ST_LIMIT, ST_WEAK, ST_NOISE, ST_CROWD, ST_NARROW, ST_NODATA, ST_STALL] {
            assert!(abnormal(s, CAUSE_NONE), "{s}");
        }
        for s in [ST_OK, ST_NOSIM, ST_AIRPLANE, ST_SOS, ST_NOSVC, ST_ONLY2G, ST_ONLY3G, ST_CHANGING, ST_REVERT_FAIL, ST_HOT] {
            assert!(!abnormal(s, CAUSE_WEAK), "{s}");
        }
        assert!(abnormal(ST_UNKNOWN, CAUSE_CROWD) && !abnormal(ST_UNKNOWN, CAUSE_NONE));
    }

    #[test]
    fn placeholder_view() {
        let v = placeholder("读取中…", "");
        assert!(v.story.headline == "读取中…" && v.story.tone == NET_NEUTRAL && v.bars_tier == -1 && v.sim_usable == 1);
        assert!(v.ca_n == 0 && v.ca_val == "—" && v.mode_word == "-" && v.name.is_empty());
        let v = placeholder("—", "数据服务版本太旧");
        assert!(v.name == "数据服务版本太旧" && v.story.headline == "—");
    }
}

/// One carrier of an nrca / lteca string.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Carrier {
    pub kind: u8,
    pub pci: i32,
    pub band: i32,
    pub arfcn: i64,
    pub bw: i32,
    pub active: bool,
}

/// C strtod on a prefix: the number and the bytes it took (0 = none).
fn strtod_prefix(s: &[u8]) -> (f64, usize) {
    let mut i = 0;
    while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | b'\r' | b'\x0b' | b'\x0c') {
        i += 1;
    }
    let start = i;
    if i < s.len() && matches!(s[i], b'+' | b'-') {
        i += 1;
    }
    let digits0 = i;
    while i < s.len() && s[i].is_ascii_digit() {
        i += 1;
    }
    let mut nd = i - digits0;
    if i < s.len() && s[i] == b'.' {
        i += 1;
        let f0 = i;
        while i < s.len() && s[i].is_ascii_digit() {
            i += 1;
        }
        nd += i - f0;
    }
    if nd == 0 {
        return (0.0, 0);
    }
    if i < s.len() && matches!(s[i], b'e' | b'E') {
        let mut j = i + 1;
        if j < s.len() && matches!(s[j], b'+' | b'-') {
            j += 1;
        }
        let e0 = j;
        while j < s.len() && s[j].is_ascii_digit() {
            j += 1;
        }
        if j > e0 {
            i = j;
        }
    }
    let v = std::str::from_utf8(&s[start..i]).ok().and_then(|t| t.parse::<f64>().ok()).unwrap_or(0.0);
    (v, i)
}

/// sscanf(rec, "%lf,%lf,…") with up to `max` numbers: what it read.
fn scan_numbers(rec: &[u8], max: usize) -> Vec<f64> {
    let mut out = Vec::new();
    let mut p = 0;
    while out.len() < max {
        let (v, n) = strtod_prefix(&rec[p..]);
        if n == 0 {
            break;
        }
        out.push(v);
        p += n;
        if out.len() == max || rec.get(p) != Some(&b',') {
            break;
        }
        p += 1;
    }
    out
}

/// nrca / lteca: "idx,pci,?,band,arfcn,bw,?,rsrp,rsrq,sinr,rssi;…" (a carrier at the -140
/// floor is configured but not active); B27's lteca has five fields "PCI,band,?,EARFCN,bw".
/// At most `max` carriers.
pub fn parse_ca(s: &str, max: usize, kind: u8) -> Vec<Carrier> {
    let b = s.as_bytes();
    let b = &b[..b.len().min(255)];
    let mut out = Vec::new();
    for rec in b.split(|&c| c == b';').filter(|r| !r.is_empty()) {
        if out.len() >= max {
            break;
        }
        let f = scan_numbers(rec, 11);
        if f.len() == 5 {
            out.push(Carrier { kind, pci: f[0] as i32, band: f[1] as i32, arfcn: f[3] as i64, bw: f[4] as i32, active: true });
            continue;
        }
        if f.len() != 11 {
            continue;
        }
        out.push(Carrier { kind, pci: f[1] as i32, band: f[3] as i32, arfcn: f[4] as i64, bw: f[5] as i32, active: f[7] > -140.0 });
    }
    out
}

#[cfg(test)]
mod ca_tests {
    use super::*;

    #[test]
    fn carriers_from_both_formats() {
        let c = parse_ca("1,100,0,3,1850,20,0,-95,-10,12,-70;2,200,0,1,350,10,0,-140,-20,0,-100", 5, b'B');
        assert_eq!(c.len(), 2);
        assert_eq!((c[0].pci, c[0].band, c[0].arfcn, c[0].bw, c[0].active), (100, 3, 1850, 20, true));
        assert!(!c[1].active, "the -140 floor is configured, not active");
        let c = parse_ca("441,3,0,1850,20", 5, b'B');
        assert_eq!((c[0].pci, c[0].band, c[0].arfcn, c[0].bw, c[0].active), (441, 3, 1850, 20, true));
        assert!(parse_ca("1,2,3", 5, b'B').is_empty(), "neither shape: skipped");
        assert_eq!(parse_ca("441,3,0,1850,20;442,3,0,1850,20", 1, b'B').len(), 1, "at most max");
        assert!(parse_ca("", 5, b'B').is_empty());
    }
}

/// tests/render/views.h's k_views: each render scene's /v2/screen "net", as (scene, json).
#[cfg(test)]
pub fn test_views() -> Vec<(String, String)> {
    let src = crate::testc::strip_comments(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/views.h")));
    let body = &src[src.find("k_views[] = {").expect("k_views") + 13..];
    let b = body.as_bytes();
    let (mut out, mut lits, mut i, mut depth) = (Vec::new(), Vec::<String>::new(), 0, 0);
    while i < b.len() {
        match b[i] {
            b'"' => {
                // a C string literal; adjacent ones join
                let mut s = Vec::new();
                i += 1;
                while b[i] != b'"' {
                    if b[i] == b'\\' {
                        i += 1;
                        s.push(match b[i] {
                            b'n' => b'\n',
                            c => c,
                        });
                    } else {
                        s.push(b[i]);
                    }
                    i += 1;
                }
                let s = String::from_utf8(s).unwrap();
                if depth == 1 && lits.len() == 2 && i >= 1 {
                    let last = lits.pop().unwrap();
                    lits.push(last + &s);
                } else {
                    lits.push(s);
                }
            }
            b'{' => {
                depth += 1;
                lits.clear();
            }
            b'}' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
                if lits.len() == 2 {
                    out.push((lits[0].clone(), lits[1].clone()));
                }
            }
            _ => {}
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod parse_tests {
    use super::*;
    use crate::logic::*;

    fn view(scene: &str) -> String {
        test_views().into_iter().find(|v| v.0 == scene).unwrap_or_else(|| panic!("no view {scene}")).1
    }

    fn p(s: &str) -> (bool, net_view_t) {
        parse(s.as_bytes())
    }

    #[test]
    fn parses_core_net() {
        lang::set_en(false);
        assert!(test_views().len() >= 20);
        assert!(p(r#"{"story":{"headline":"x"},"carriers":"}"}"#).1.ca_n == 0);
        let (ok, v) = p(r#"{"story":{"headline":"x"},"carriers":[}]}"#);
        assert!(ok && v.ca_n == 0);

        // the good scene: 5G SA, three NR carriers
        let (ok, v) = p(&view("good"));
        assert!(ok && v.story.headline == "顺畅" && v.story.tone == NET_OK && v.story.cause == CAUSE_NONE);
        assert!(v.ca_n >= 1 && v.ca[0].kind == b'n' as i32 && v.ca[0].active != 0 && v.ca[0].label == "n78");
        assert!(!v.ca[0].rsrp.is_empty() && !v.ca[0].sinr.is_empty() && v.ca[0].sinr_tone == NET_OK);
        assert!(v.sim_usable != 0 && v.bars_tier == 2 && v.story.sig_tone == NET_OK);
        assert!(v.story.sig == "强" && !v.fine.is_empty() && !v.name.is_empty() && !v.ca_val.is_empty());

        // every scene parses and carries a headline
        for (s, j) in test_views() {
            let (ok, v) = p(&j);
            assert!(ok && v.ca_n as usize <= NV_CA_MAX && (-1..=2).contains(&v.bars_tier), "{s}");
        }
        let (ok, v) = p(&view("nosvc"));
        assert!(ok && v.nosvc != 0 && (v.state == ST_NOSVC || v.state == ST_SOS));
        let (ok, v) = p(&view("nosim"));
        assert!(ok && v.sim_usable == 0);

        // broken or empty input: nothing claimed
        let (ok, v) = p("");
        assert!(!ok && v.ca_n == 0 && v.bars_tier == -1);
        assert!(!p("{}").0 && !p(r#"{"story":{}}"#).0 && !p("not json").0);
        let (ok, v) = p(r#"{"story":{"headline":"x","tone":"new-tone"},"carriers":"oops"}"#);
        assert!(ok && v.story.tone == NET_NEUTRAL && v.ca_n == 0);
        // more carriers than slots
        let cs: Vec<String> = (0..9).map(|i| format!(r#"{{"kind":"lte","band":{},"active":true}}"#, i + 1)).collect();
        let (ok, v) = p(&format!(r#"{{"story":{{"headline":"x"}},"carriers":[{}]}}"#, cs.join(",")));
        assert!(ok && v.ca_n as usize == NV_CA_MAX && v.ca[4].band == 5 && v.ca[4].kind == b'B' as i32);

        // story.state as a code; an old core without it, or a new code → unknown
        let st = [
            ("ok", ST_OK),
            ("nosim", ST_NOSIM),
            ("airplane", ST_AIRPLANE),
            ("sos", ST_SOS),
            ("nosvc", ST_NOSVC),
            ("nodata", ST_NODATA),
            ("limit", ST_LIMIT),
            ("weak", ST_WEAK),
            ("noise", ST_NOISE),
            ("crowd", ST_CROWD),
            ("only2g", ST_ONLY2G),
            ("only3g", ST_ONLY3G),
            ("narrow", ST_NARROW),
            ("stall", ST_STALL),
            ("", ST_UNKNOWN),
            ("later", ST_UNKNOWN),
        ];
        for (s, want) in st {
            let (ok, v) = p(&format!(r#"{{"story":{{"headline":"x","state":"{s}"}}}}"#));
            assert!(ok && v.state == want, "{s}");
        }
        assert_eq!(p(r#"{"story":{"headline":"x"}}"#).1.state, ST_UNKNOWN);
    }

    #[test]
    fn stall_and_later_states_and_english() {
        let js = r#"{"story":{"tone":"bad","cause":"none","headline":"连上了但不通","hint":"有信号、已拨号，但 30 秒没收到任何数据","rat":"5G","state":"stall","headline_en":"No traffic","hint_en":"Signal and data are up, but nothing came back for 30 s"}}"#;
        lang::set_en(false);
        let (ok, v) = p(js);
        assert!(ok && v.state == ST_STALL && v.story.tone == NET_BAD);
        assert!(v.story.headline == "连上了但不通" && v.story.hint == "有信号、已拨号，但 30 秒没收到任何数据");
        lang::set_en(true);
        let (ok, v) = p(js);
        assert!(ok && v.story.headline == "No traffic" && v.story.tone == NET_BAD);
        lang::set_en(false);
        let (ok, v) = p(&view("stall"));
        assert!(ok && v.state == ST_STALL && v.story.tone == NET_BAD);

        let js = r#"{"story":{"tone":"warn","cause":"none","headline":"以后的结论","hint":"以后的提示","rat":"5G","state":"later","headline_en":"Later","hint_en":"A later hint"}}"#;
        let (ok, v) = p(js);
        assert!(ok && v.state == ST_UNKNOWN && v.story.tone == NET_WARN && v.story.headline == "以后的结论");
        lang::set_en(true);
        let (_, v) = p(js);
        assert!(v.story.headline == "Later" && v.story.hint == "A later hint");
        lang::set_en(false);

        // which verdicts get 查原因 ›
        let (_, v) = p(&view("crowd"));
        assert!(abnormal(v.state, v.story.cause));
        let (_, v) = p(&view("good"));
        assert!(!abnormal(v.state, v.story.cause));

        // English: every *_en sibling replaces its field; missing / empty → the Chinese
        let js = r#"{"story":{"tone":"warn","cause":"weak","headline":"慢：信号弱","hint":"离基站远","rat":"5G","link":"单载波 · 带宽一般","sig":"弱","noise":"大","load":"高","limit":"无","state":"weak","headline_en":"Slow","hint_en":"Weak signal; try near a window","link_en":"Single carrier · Fair","sig_en":"Weak","noise_en":"high","load_en":"","limit_en":"none"},"fine":"5G NSA · 4G 锚点","fine_en":"5G NSA · 4G anchor","name":"中国移动","name_en":"China Mobile","where":"本地","where_en":"Local","ca_val":"单载波","ca_val_en":"Single carrier","ca_sub":"↓ n78   ↑ n78","mode_word":"自动","mode_word_en":"Auto"}"#;
        let (ok, v) = p(js);
        assert!(ok && v.state == ST_WEAK && v.story.headline == "慢：信号弱" && v.name == "中国移动" && v.story.load == "高");
        lang::set_en(true);
        let (ok, v) = p(js);
        assert!(ok && v.state == ST_WEAK && v.story.cause == CAUSE_WEAK);
        assert!(v.story.headline == "Slow" && v.story.hint == "Weak signal; try near a window");
        assert!(v.story.rat == "5G" && v.story.link == "Single carrier · Fair");
        assert!(v.story.sig == "Weak" && v.story.noise == "high" && v.story.limit == "none");
        assert_eq!(v.story.load, "高");
        assert!(v.fine == "5G NSA · 4G anchor" && v.name == "China Mobile" && v.r#where == "Local");
        assert!(v.ca_val == "Single carrier" && v.ca_sub == "↓ n78   ↑ n78" && v.mode_word == "Auto");
        let (ok, v) = p(r#"{"story":{"headline":"顺畅"},"name":"未注册"}"#);
        assert!(ok && v.story.headline == "顺畅" && v.name == "未注册");
        let (ok, v) = p(&view("good"));
        assert!(ok && v.story.headline == "All good" && v.state == ST_OK);
        lang::set_en(false);
    }

    #[test]
    fn home_story_wins() {
        // E4: net.home (the story with a write transaction over it) wins over story
        let with = r#"{"story":{"tone":"bad","state":"nosvc","headline":"无服务","hint":"正在搜网"},"nosvc":true,"home":{"tone":"neutral","state":"changing","headline":"正在确认","hint":"1:42 后没通就退回到自动"}}"#;
        let (ok, v) = p(with);
        assert!(ok && v.state == ST_CHANGING && v.story.tone == NET_NEUTRAL);
        assert!(v.story.headline == "正在确认" && v.story.hint == "1:42 后没通就退回到自动" && v.nosvc != 0);
        assert!(!abnormal(v.state, v.story.cause));
        let (ok, v) = p(r#"{"story":{"tone":"ok","state":"ok","headline":"顺畅"},"home":{"tone":"bad","state":"revert_fail","headline":"退回也没通"}}"#);
        assert!(ok && v.state == ST_REVERT_FAIL && v.story.tone == NET_BAD);
        let (ok, v) = p(r#"{"story":{"tone":"warn","cause":"none","state":"hot","headline":"慢：过热限速"}}"#);
        assert!(ok && v.state == ST_HOT && v.story.tone == NET_WARN && !abnormal(v.state, v.story.cause));
    }
}
