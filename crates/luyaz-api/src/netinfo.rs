//! `/api/netinfo`'s pure parts (moved from zte-agent `netinfo.rs`, Phase 4 step 8c): the operator
//! and country tables, PLMN strings, the exit lookups' parsing and English, the register guard's and
//! the operator search's JSON, the Wi-Fi station and lease parsers, the APN view, and the page
//! itself ([`page`]). The agent (which keeps serving the admin web) and u60d's touch compatibility
//! layer both build the reply from these, so the two cannot drift.
//!
//! The I/O stays with whoever owns it: ubus reads, `iw`, the AT port and the exit lookups. While
//! the agent still runs the lookups (the shadow period) it writes the result to [`EXITS_FILE`] after
//! every refresh pass and u60d reads it, so the outside services are never asked twice.

use serde_json::{Value, json};
use std::fs;

/// The agent's two exit lookups as the page shows them (`{"direct": …, "proxy": …}`, each
/// [`Lookup::json`] or null), written after every refresh pass (tmpfs).
pub const EXITS_FILE: &str = "/tmp/netinfo-exits.json";
/// Once the agent has handed netselect to u60d (step 9b) it no longer gets the `/api/netinfo`
/// reads that start its lookups: u60d touches this file on every refresh pass instead, and the
/// agent looks the exits up while it is fresh ([`EXITS_WANTED_SECS`]).
pub const EXITS_WANTED: &str = "/tmp/netinfo-exits.wanted";
pub const EXITS_WANTED_SECS: u64 = 60;

pub const CONF_PATH: &str = "/data/netinfo.conf";

/// Loopback mixed listener in the CHILL template (`listeners:` → netinfo).
pub const DEFAULT_PROXY_PORT: u16 = 7894;
/// CHILL listener pinned to 🤖 AI (template.yaml `netinfo-ai`).
pub const DEFAULT_AI_PORT: u16 = 7895;
/// Tried in order until one answers with something parse_geo understands, so
/// one service being down or blocked does not blank the screen (2026-09-25:
/// the user asked for several). Mainland services first for the cellular
/// exit, international ones first for CHILL's. ip.skk.moe and ip.net.coffee
/// were asked for too, but they are browser pages that query from JavaScript;
/// they have no endpoint a program can read.
pub const DEFAULT_DIRECT_URLS: &[&str] = &[
    "https://myip.ipip.net/json",
    "https://cip.cc/",
    "https://ping0.cc/geo",
    "https://api.myip.la/cn?json",
    "http://ip-api.com/json/?lang=zh-CN&fields=status,query,country,regionName,city,isp,org",
];
pub const DEFAULT_PROXY_URLS: &[&str] = &[
    "http://ip-api.com/json/?lang=zh-CN&fields=status,query,country,regionName,city,isp,org",
    "https://ping0.cc/geo",
    "https://ipinfo.io/json",
    "https://api-ipv4.ip.sb/geoip",
    "https://ipapi.co/json/",
];

pub const LOOKUP_MAX_AGE: i64 = 600;
pub const LOOKUP_RETRY: i64 = 30;

/// Error strings end up on a 320 px screen and in fixed C buffers.
pub const ERR_MAX_CHARS: usize = 60;

pub fn clip(s: String) -> String {
    clip_to(s, ERR_MAX_CHARS)
}

/// English says in about twice the characters what Chinese says, so the
/// English twin of a clipped message gets twice the room.
pub fn clip_en(s: String) -> String {
    clip_to(s, 2 * ERR_MAX_CHARS)
}

pub fn clip_to(s: String, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s,
    }
}

// ── config ─────────────────────────────────────────────────────────────────

/// `/data/netinfo.conf`, optional, `key=value` lines:
///   direct=<url>[,<url>…]   lookups for the cellular exit, tried in order
///   proxy=<url>[,<url>…]    lookups through CHILL
///   proxy_port=7894
///   ai_port=7895           used while the main group is on DIRECT
///   lookups=0               no external lookups at all
pub struct Config {
    pub direct: Vec<String>,
    pub proxy: Vec<String>,
    pub proxy_port: u16,
    pub ai_port: u16,
    pub enabled: bool,
}

impl Config {
    pub fn load() -> Self {
        Self::parse(&fs::read_to_string(CONF_PATH).unwrap_or_default())
    }

    pub fn parse(text: &str) -> Self {
        let mut c = Config {
            direct: DEFAULT_DIRECT_URLS.iter().map(|s| s.to_string()).collect(),
            proxy: DEFAULT_PROXY_URLS.iter().map(|s| s.to_string()).collect(),
            proxy_port: DEFAULT_PROXY_PORT,
            ai_port: DEFAULT_AI_PORT,
            enabled: true,
        };
        let urls = |v: &str| -> Vec<String> {
            v.split(',')
                .map(str::trim)
                .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
                .map(str::to_string)
                .collect()
        };
        for line in text.lines() {
            let Some((k, v)) = line.trim().split_once('=') else {
                continue;
            };
            match k.trim() {
                "direct" => c.direct = urls(v),
                "proxy" => c.proxy = urls(v),
                "proxy_port" => c.proxy_port = v.trim().parse().unwrap_or(c.proxy_port),
                "ai_port" => c.ai_port = v.trim().parse().unwrap_or(c.ai_port),
                "lookups" => c.enabled = v.trim() != "0",
                _ => {}
            }
        }
        c
    }
}

// ── IP lookups ─────────────────────────────────────────────────────────────

#[derive(Clone, Default, Debug, PartialEq)]
pub struct Geo {
    pub ip: String,
    pub geo: String,
    pub isp: String,
}

/// Join place names, dropping empties and any that repeat or are contained in
/// the previous one ("中国 北京 北京" → "中国 北京", "东京都 东京" → "东京都").
pub fn join_place(parts: &[&str]) -> String {
    let mut out: Vec<&str> = Vec::new();
    for p in parts.iter().map(|p| p.trim()).filter(|p| !p.is_empty()) {
        if out.last().is_some_and(|l| l.contains(p)) {
            continue;
        }
        if out.contains(&p) {
            continue;
        }
        out.push(p);
    }
    out.join(" ")
}

pub fn js<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

pub fn valid_ip(s: &str) -> bool {
    s.parse::<std::net::IpAddr>().is_ok()
}

/// Every lookup service we default to, by shape. Anything else that has an
/// `ip`/`query` field still yields the address.
pub fn parse_geo(body: &str) -> Option<Geo> {
    let body = body.trim();
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        // myip.ipip.net/json: {"ret":"ok","data":{"ip":…,"location":[国,省,市,"",运营商]}}
        if let Some(d) = v.get("data").filter(|d| d.get("location").is_some()) {
            let loc: Vec<&str> = d["location"]
                .as_array()?
                .iter()
                .map(|x| x.as_str().unwrap_or(""))
                .collect();
            let get = |i: usize| loc.get(i).copied().unwrap_or("");
            return Some(Geo {
                ip: js(d, "ip").to_string(),
                geo: join_place(&[get(0), get(1), get(2)]),
                isp: get(4).to_string(),
            })
            .filter(|g| !g.ip.is_empty());
        }
        // ip-api.com: {"status":"success","query":…,"country","regionName","city","isp","org"}
        if v.get("query").is_some() {
            if js(&v, "status") == "fail" {
                return None;
            }
            let isp = if js(&v, "isp").is_empty() {
                js(&v, "org")
            } else {
                js(&v, "isp")
            };
            return Some(Geo {
                ip: js(&v, "query").to_string(),
                geo: join_place(&[js(&v, "country"), js(&v, "regionName"), js(&v, "city")]),
                isp: isp.to_string(),
            })
            .filter(|g| !g.ip.is_empty());
        }
        // api.myip.la/cn?json: {"ip","location":{"country_name","province","city"}}
        if let Some(l) = v.get("location").filter(|l| l.is_object()) {
            return Some(Geo {
                ip: js(&v, "ip").to_string(),
                geo: join_place(&[js(l, "country_name"), js(l, "province"), js(l, "city")]),
                isp: String::new(),
            })
            .filter(|g| valid_ip(&g.ip));
        }
        // ipinfo.io / ip.sb / ipapi.co and similar: {"ip","country"|"country_code","region","city","org"|"isp"}
        if !js(&v, "ip").is_empty() {
            let country = js(&v, "country");
            let country = country_by_code(js(&v, "country_code"))
                .or_else(|| country_by_code(country))
                .unwrap_or(country);
            let isp = [js(&v, "isp"), js(&v, "org"), js(&v, "organization")]
                .into_iter()
                .find(|s| !s.is_empty())
                .unwrap_or("");
            // ipinfo's org is "AS2516 KDDI CORPORATION"; the AS number is noise here.
            let isp = match isp.split_once(' ') {
                Some((asn, rest))
                    if asn.starts_with("AS") && asn[2..].bytes().all(|b| b.is_ascii_digit()) =>
                {
                    rest
                }
                _ => isp,
            };
            return Some(Geo {
                ip: js(&v, "ip").to_string(),
                geo: join_place(&[country, js(&v, "region"), js(&v, "city")]),
                isp: isp.to_string(),
            });
        }
        return None;
    }
    // ping0.cc/geo: four lines — IP, 地点, ASN, 组织
    //   188.253.116.74 / 中国 台湾 台北 / AS38136 / Akari Networks Limited
    let lines: Vec<&str> = body.lines().map(str::trim).collect();
    if lines.len() >= 2 && valid_ip(lines[0]) {
        let place = lines[1];
        // some answers carry the carrier after a dash: "中国 广东 深圳 — 电信"
        let (place, isp) = match place.split_once(" — ") {
            Some((p, i)) => (p.trim(), i.trim()),
            None => (place, lines.get(3).copied().unwrap_or("")),
        };
        let words: Vec<&str> = place.split_whitespace().collect();
        return Some(Geo {
            ip: lines[0].to_string(),
            geo: join_place(&words),
            isp: isp.to_string(),
        });
    }
    // cip.cc: "IP\t: 1.2.3.4" / "地址\t: 中国 上海 上海" / "运营商\t: 电信"
    if body.starts_with("IP\t") || body.starts_with("IP ") {
        let field = |k: &str| {
            body.lines()
                .find_map(|l| {
                    l.split_once(':')
                        .filter(|(key, _)| key.trim() == k)
                        .map(|(_, v)| v.trim())
                })
                .unwrap_or("")
        };
        let ip = field("IP");
        if !valid_ip(ip) {
            return None;
        }
        let words: Vec<&str> = field("地址").split_whitespace().collect();
        return Some(Geo {
            ip: ip.to_string(),
            geo: join_place(&words),
            isp: field("运营商").to_string(),
        });
    }
    // myip.ipip.net plain text: "当前 IP：1.2.3.4  来自于：中国 广东 深圳  电信"
    let rest = body
        .split_once("IP：")
        .or_else(|| body.split_once("IP:"))?
        .1;
    let (ip, loc) = match rest.split_once("来自于：") {
        Some((ip, loc)) => (ip.trim(), loc.trim()),
        None => (rest.trim(), ""),
    };
    if ip.is_empty()
        || !ip
            .bytes()
            .all(|b| b.is_ascii_hexdigit() || b == b'.' || b == b':')
    {
        return None;
    }
    let words: Vec<&str> = loc.split_whitespace().collect();
    let (geo, isp) = match words.split_last() {
        Some((last, head)) if head.len() >= 2 => (join_place(head), last.to_string()),
        _ => (join_place(&words), String::new()),
    };
    Some(Geo {
        ip: ip.to_string(),
        geo,
        isp,
    })
}

// ── operators ──────────────────────────────────────────────────────────────

/// MCCs whose networks use 3-digit MNCs (North America, parts of Latin America).
pub const MNC3_MCCS: &[u16] = &[
    302, 310, 311, 312, 313, 314, 315, 316, 334, 338, 342, 344, 346, 348, 350, 352, 354, 356, 358,
    360, 365, 366, 376, 708, 722, 732,
];

pub fn mnc_len(mcc: u16) -> usize {
    if MNC3_MCCS.contains(&mcc) { 3 } else { 2 }
}

/// ISO 3166 code of the country an MCC belongs to. 901 (international
/// networks) has none and is handled by [`country_info`].
pub fn mcc_iso(mcc: u16) -> Option<&'static str> {
    Some(match mcc {
        202 => "GR",
        204 => "NL",
        206 => "BE",
        208 => "FR",
        214 => "ES",
        222 => "IT",
        228 => "CH",
        232 => "AT",
        234 | 235 => "GB",
        238 => "DK",
        240 => "SE",
        242 => "NO",
        244 => "FI",
        250 => "RU",
        262 => "DE",
        268 => "PT",
        272 => "IE",
        286 => "TR",
        302 => "CA",
        310..=316 => "US",
        334 => "MX",
        404 | 405 => "IN",
        420 => "SA",
        424 => "AE",
        425 => "IL",
        427 => "QA",
        440 | 441 => "JP",
        450 => "KR",
        452 => "VN",
        454 => "HK",
        455 => "MO",
        456 => "KH",
        457 => "LA",
        460 | 461 => "CN",
        466 => "TW",
        502 => "MY",
        505 => "AU",
        510 => "ID",
        515 => "PH",
        520 => "TH",
        525 => "SG",
        530 => "NZ",
        655 => "ZA",
        722 => "AR",
        724 => "BR",
        730 => "CL",
        732 => "CO",
        _ => return None,
    })
}

/// ISO code → (Chinese, English). Every code [`mcc_iso`] gives is here, and
/// so is every code the IP lookup services are known to send.
pub fn iso_names(code: &str) -> Option<(&'static str, &'static str)> {
    Some(match code {
        "GR" => ("希腊", "Greece"),
        "NL" => ("荷兰", "Netherlands"),
        "BE" => ("比利时", "Belgium"),
        "FR" => ("法国", "France"),
        "ES" => ("西班牙", "Spain"),
        "IT" => ("意大利", "Italy"),
        "CH" => ("瑞士", "Switzerland"),
        "AT" => ("奥地利", "Austria"),
        "GB" => ("英国", "United Kingdom"),
        "DK" => ("丹麦", "Denmark"),
        "SE" => ("瑞典", "Sweden"),
        "NO" => ("挪威", "Norway"),
        "FI" => ("芬兰", "Finland"),
        "RU" => ("俄罗斯", "Russia"),
        "DE" => ("德国", "Germany"),
        "PT" => ("葡萄牙", "Portugal"),
        "IE" => ("爱尔兰", "Ireland"),
        "TR" => ("土耳其", "Turkey"),
        "CA" => ("加拿大", "Canada"),
        "US" => ("美国", "United States"),
        "MX" => ("墨西哥", "Mexico"),
        "IN" => ("印度", "India"),
        "SA" => ("沙特", "Saudi Arabia"),
        "AE" => ("阿联酋", "United Arab Emirates"),
        "IL" => ("以色列", "Israel"),
        "QA" => ("卡塔尔", "Qatar"),
        "JP" => ("日本", "Japan"),
        "KR" => ("韩国", "South Korea"),
        "VN" => ("越南", "Vietnam"),
        "HK" => ("中国香港", "Hong Kong"),
        "MO" => ("中国澳门", "Macau"),
        "KH" => ("柬埔寨", "Cambodia"),
        "LA" => ("老挝", "Laos"),
        "CN" => ("中国", "China"),
        "TW" => ("中国台湾", "Taiwan"),
        "MY" => ("马来西亚", "Malaysia"),
        "AU" => ("澳大利亚", "Australia"),
        "ID" => ("印度尼西亚", "Indonesia"),
        "PH" => ("菲律宾", "Philippines"),
        "TH" => ("泰国", "Thailand"),
        "SG" => ("新加坡", "Singapore"),
        "NZ" => ("新西兰", "New Zealand"),
        "ZA" => ("南非", "South Africa"),
        "AR" => ("阿根廷", "Argentina"),
        "BR" => ("巴西", "Brazil"),
        "CL" => ("智利", "Chile"),
        "CO" => ("哥伦比亚", "Colombia"),
        _ => return None,
    })
}

/// A country by MCC: Chinese name, English name, ISO code (None for 901).
pub fn country_info(mcc: u16) -> Option<(&'static str, &'static str, Option<&'static str>)> {
    if mcc == 901 {
        return Some(("国际", "International", None));
    }
    let iso = mcc_iso(mcc)?;
    iso_names(iso).map(|(zh, en)| (zh, en, Some(iso)))
}

pub fn country(mcc: u16) -> Option<&'static str> {
    country_info(mcc).map(|c| c.0)
}

/// English country name by MCC, for wording written next to [`country`].
pub fn country_en(mcc: u16) -> Option<&'static str> {
    country_info(mcc).map(|c| c.1)
}

/// The codes the IP-lookup path has always translated. The table above
/// knows more, but a lookup answering "IT" kept showing "Italy …", and only
/// fields are being added here, not changed.
pub const GEO_CODES: [&str; 23] = [
    "CN", "HK", "MO", "TW", "JP", "KR", "SG", "US", "CA", "GB", "DE", "FR", "NL", "TH", "MY", "VN",
    "PH", "ID", "AU", "NZ", "IN", "AE", "RU",
];

/// Every code in [`iso_names`], for going from a Chinese name back to English.
pub const ISO_ALL: &[&str] = &[
    "GR", "NL", "BE", "FR", "ES", "IT", "CH", "AT", "GB", "DK", "SE", "NO", "FI", "RU", "DE", "PT",
    "IE", "TR", "CA", "US", "MX", "IN", "SA", "AE", "IL", "QA", "JP", "KR", "VN", "HK", "MO", "KH",
    "LA", "CN", "TW", "MY", "AU", "ID", "PH", "TH", "SG", "NZ", "ZA", "AR", "BR", "CL", "CO",
];

/// English for one Chinese place word a lookup service sent: country names
/// (and the short 台湾/香港/澳门 the mainland services write after 中国).
/// Provinces and cities have no table: None, and the English drops them.
pub fn place_en(w: &str) -> Option<&'static str> {
    match w {
        "台湾" => return Some("Taiwan"),
        "香港" => return Some("Hong Kong"),
        "澳门" => return Some("Macau"),
        _ => {}
    }
    ISO_ALL
        .iter()
        .filter_map(|c| iso_names(c))
        .find(|(zh, _)| *zh == w)
        .map(|(_, en)| en)
}

/// `geo_en`: the place in English, from the joined `geo`. Runs of ASCII words
/// (what an English-speaking service sent: "Taipei City") stay together;
/// Chinese words become a country name or are dropped. "日本 东京都" → "Japan",
/// "中国台湾 Taipei City" → "Taiwan, Taipei City". Empty when nothing is left.
pub fn geo_en(geo: &str) -> String {
    let mut segs: Vec<String> = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    let flush = |cur: &mut Vec<&str>, segs: &mut Vec<String>| {
        if !cur.is_empty() {
            segs.push(cur.join(" "));
            cur.clear();
        }
    };
    for w in geo.split_whitespace() {
        if w.is_ascii() {
            cur.push(w);
        } else {
            flush(&mut cur, &mut segs);
            if let Some(en) = place_en(w) {
                segs.push(en.to_string());
            }
        }
    }
    flush(&mut cur, &mut segs);
    // "中国 台湾 台北" says Taiwan; China in front of it adds nothing
    if segs
        .iter()
        .any(|s| matches!(s.as_str(), "Taiwan" | "Hong Kong" | "Macau"))
    {
        segs.retain(|s| s != "China");
    }
    let mut out: Vec<String> = Vec::new();
    for s in segs {
        // "Taiwan" then "Taiwan Taipei" (region + city from an English service)
        let s = match out.last() {
            Some(l)
                if s.len() > l.len()
                    && s.starts_with(l.as_str())
                    && s.as_bytes()[l.len()] == b' ' =>
            {
                s[l.len() + 1..].to_string()
            }
            _ => s,
        };
        if out
            .last()
            .is_some_and(|l| l.eq_ignore_ascii_case(&s) || l.contains(s.as_str()))
        {
            continue;
        }
        out.push(s);
    }
    out.join(", ")
}

/// `isp_en`: an ASCII name as it is; the mainland carriers' short names in
/// English (glossary §8); anything else empty, so clients show `isp`.
pub fn isp_en(isp: &str) -> String {
    if isp.is_ascii() {
        return isp.to_string();
    }
    let short = isp.strip_prefix("中国").unwrap_or(isp);
    match short {
        "电信" => "China Telecom",
        "联通" => "China Unicom",
        "移动" => "China Mobile",
        "广电" => "China Broadnet",
        _ => match operator_en(isp) {
            en if en.is_ascii() => en,
            _ => "",
        },
    }
    .to_string()
}

/// ISO code → name, for lookup services that only give the code.
pub fn country_by_code(code: &str) -> Option<&'static str> {
    GEO_CODES
        .contains(&code)
        .then(|| iso_names(code))
        .flatten()
        .map(|c| c.0)
}

/// The `country`, `country_en`, `country_iso` fields of an operator object.
/// `country_iso` is what clients compare ("CN"), never the Chinese name.
pub fn country_fields(mcc: Option<u16>) -> (Value, Value, Value) {
    match mcc.and_then(country_info) {
        Some((zh, en, iso)) => (json!(zh), json!(en), json!(iso)),
        None => (Value::Null, Value::Null, Value::Null),
    }
}

/// English for an operator name from [`operator`], per docs/ui-glossary.md §8:
/// the short common name, no "Co., Ltd.". Names in the table that are already
/// English stay as they are, and so does a network's broadcast name (used when
/// the PLMN is not in the table) — `operator_en` then equals `name`.
pub fn operator_en(name: &str) -> &str {
    match name {
        "中国移动" => "China Mobile",
        "中国联通" => "China Unicom",
        "中国电信" => "China Telecom",
        "中国广电" => "China Broadnet",
        "3 香港" => "3 HK",
        "中国移动香港" => "China Mobile HK",
        "中国联通香港" => "China Unicom HK",
        "SmarTone 澳门" => "SmarTone Macau",
        "中国电信澳门" => "China Telecom Macau",
        "3 澳门" => "3 Macau",
        "远传电信" => "FarEasTone",
        "中华电信" => "Chunghwa Telecom",
        "台湾大哥大" => "Taiwan Mobile",
        "楽天モバイル" => "Rakuten Mobile",
        other => other,
    }
}

pub fn operator(mcc: u16, mnc: u16) -> Option<&'static str> {
    Some(match (mcc, mnc) {
        (460, 0 | 2 | 4 | 7 | 8) => "中国移动",
        (460, 1 | 6 | 9) => "中国联通",
        (460, 3 | 5 | 11) => "中国电信",
        (460, 15) => "中国广电",
        (454, 0 | 2 | 10 | 18 | 16 | 19 | 29) => "csl",
        (454, 3 | 4) => "3 香港",
        (454, 6 | 15 | 17) => "SmarTone",
        (454, 12 | 13) => "中国移动香港",
        (454, 7) => "中国联通香港",
        (455, 0) => "SmarTone 澳门",
        (455, 1) => "CTM",
        (455, 2 | 7) => "中国电信澳门",
        (455, 3 | 5) => "3 澳门",
        (466, 1 | 5) => "远传电信",
        (466, 11 | 92) => "中华电信",
        (466, 89 | 93 | 97) => "台湾大哥大",
        (440, 10) => "NTT docomo",
        (440, 0 | 20 | 21) => "SoftBank",
        (440, 50..=54 | 70) => "au (KDDI)",
        (440, 11) => "楽天モバイル",
        (450, 5 | 11) => "SK Telecom",
        (450, 2 | 4 | 8) => "KT",
        (450, 6) => "LG U+",
        (525, 1 | 2 | 7) => "Singtel",
        (525, 3) => "M1",
        (525, 5 | 6) => "StarHub",
        (525, 10) => "SIMBA",
        (310, 410 | 150 | 170 | 280 | 380 | 560 | 680) => "AT&T",
        (310, 160 | 200 | 210 | 220 | 230 | 240 | 250 | 260 | 270 | 310 | 490 | 660 | 800) => {
            "T-Mobile"
        }
        (310, 120) | (312, 530) => "T-Mobile (Sprint)",
        (310, 4 | 10 | 12 | 13) | (311, 270..=289 | 480..=489) => "Verizon",
        (302, 220 | 221) => "Telus",
        (302, 610 | 640) => "Bell",
        (302, 720) => "Rogers",
        (234, 10) => "O2 UK",
        (234, 15) => "Vodafone UK",
        (234, 20) => "Three UK",
        (234, 30 | 33) => "EE",
        (204, 4) => "Vodafone NL",
        (204, 8) => "KPN",
        (204, 16) => "Odido",
        (208, 1) => "Orange",
        (208, 10) => "SFR",
        (208, 15) => "Free",
        (208, 20) => "Bouygues",
        (262, 1) => "Telekom",
        (262, 2) => "Vodafone DE",
        (262, 3) => "O2 DE",
        (520, 1 | 3) => "AIS",
        (520, 4 | 99) => "True",
        (520, 5) => "dtac",
        (502, 12) => "Maxis",
        (502, 13 | 16 | 19) => "CelcomDigi",
        (502, 18) => "U Mobile",
        (505, 1) => "Telstra",
        (505, 2) => "Optus",
        (505, 3) => "Vodafone AU",
        _ => return None,
    })
}

#[derive(Debug, PartialEq)]
pub struct Plmn {
    pub mcc: u16,
    pub mnc: u16,
    pub mnc_digits: usize,
}

/// Home PLMN from the IMSI; the MNC length depends on the country.
pub fn plmn_from_imsi(imsi: &str) -> Option<Plmn> {
    if imsi.len() < 6 || !imsi.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mcc: u16 = imsi[..3].parse().ok()?;
    let n = mnc_len(mcc);
    Some(Plmn {
        mcc,
        mnc: imsi[3..3 + n].parse().ok()?,
        mnc_digits: n,
    })
}

/// A PLMN as the modem reports it ("46001", "310260"): the length says it all.
pub fn plmn_from_str(s: &str) -> Option<Plmn> {
    let s = s.trim();
    if !(5..=6).contains(&s.len()) || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(Plmn {
        mcc: s[..3].parse().ok()?,
        mnc: s[3..].parse().ok()?,
        mnc_digits: s.len() - 3,
    })
}

pub fn plmn_json(p: &Plmn, name_hint: &str) -> Value {
    let name = operator(p.mcc, p.mnc)
        .map(str::to_string)
        .or_else(|| (!name_hint.is_empty()).then(|| name_hint.to_string()));
    let (country, country_en, country_iso) = country_fields(Some(p.mcc));
    json!({
        "mcc": format!("{:03}", p.mcc),
        "mnc": format!("{:0w$}", p.mnc, w = p.mnc_digits),
        "operator_en": name.as_deref().map(operator_en),
        "name": name,
        "country": country,
        "country_en": country_en,
        "country_iso": country_iso,
    })
}

pub fn same_plmn(a: &str, b: &str) -> bool {
    match (plmn_from_str(a), plmn_from_str(b)) {
        (Some(x), Some(y)) => x.mcc == y.mcc && x.mnc == y.mnc,
        _ => false,
    }
}

// ── local modem state ──────────────────────────────────────────────────────

/// Fields of `nwinfo_get_netinfo` this module reads. Names follow what
/// data-service read from the same call (state.rs); `rplmn_num` vs
/// `rmcc`+`rmnc` is still to be confirmed on the device.
pub fn serving_plmn(net: &Value) -> String {
    let direct = num_or_str(net, "rplmn_num");
    if plmn_from_str(&direct).is_some() {
        return direct;
    }
    // Measured 2026-09-25: `"rmcc": 460, "rmnc": 11` — numbers, so the MNC's
    // leading zero is gone and has to come back from the country's MNC length.
    let (mcc, mnc) = (num_or_str(net, "rmcc"), num_or_str(net, "rmnc"));
    let (Ok(m), Ok(n)) = (mcc.parse::<u16>(), mnc.parse::<u16>()) else {
        return String::new();
    };
    if m == 0 {
        return String::new();
    }
    let w = mnc.len().max(mnc_len(m));
    format!("{m:03}{n:0w$}")
}

/// A field that may come as a string or a number.
pub fn num_or_str(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// `net_select_mode`: "auto_select" / "manual_select" (measured). Read from
/// the same netinfo call, so the AT port is only needed to go back to auto.
pub fn selection_from_netinfo(net: &Value) -> Option<&'static str> {
    let m = js(net, "net_select_mode").to_ascii_lowercase();
    if m.contains("manual") {
        Some("manual")
    } else if m.contains("auto") {
        Some("auto")
    } else {
        None
    }
}

/// `connect_status` is e.g. "ipv4_ipv6_connected"; "disconnected" also
/// contains "connected". B31 can say connected while the bearer is gone: anything that acts on
/// it goes through [`data_up`].
pub fn data_connected(wwan: &Value) -> bool {
    let s = js(wwan, "connect_status");
    s.ends_with("connected") && !s.contains("disconnect")
}

/// The bearer from `/proc` alone (no `ip` fork): a default IPv4 route out of `iface`
/// (`/proc/net/route`), or a global IPv6 address on it (`/proc/net/if_inet6`) with a default
/// IPv6 route out of it (`/proc/net/ipv6_route`). `None` when `/proc/net/route` could not be
/// read (unknown, not down).
pub fn bearer_from_proc(
    route: Option<&str>,
    if_inet6: &str,
    ipv6_route: &str,
    iface: &str,
) -> Option<bool> {
    let up = |flags: &str| u32::from_str_radix(flags, 16).is_ok_and(|x| x & 1 != 0);
    // Iface Destination Gateway Flags RefCnt Use Metric Mask …
    let v4 = route?.lines().skip(1).any(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        f.len() >= 8 && f[0] == iface && f[1] == "00000000" && f[7] == "00000000" && up(f[3])
    });
    // address, index, prefix length, scope (00 = global), flags, name
    let addr6 = if_inet6.lines().any(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        f.len() == 6 && f[3] == "00" && f[5] == iface
    });
    // dest, dest prefix, src, src prefix, next hop, metric, refcnt, use, flags, name
    let route6 = ipv6_route.lines().any(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        f.len() == 10 && f[0].bytes().all(|b| b == b'0') && f[1] == "00" && f[9] == iface && up(f[8])
    });
    Some(v4 || (addr6 && route6))
}

/// The data call is up: the vendor says connected and the bearer (when it could be read) agrees.
pub fn data_up(wwan: &Value, bearer: Option<bool>) -> bool {
    data_connected(wwan) && bearer != Some(false)
}

/// `+COPS: 0,0,"CHN-UNICOM",7` → "auto"; 1 → "manual"; 4 → manual with
/// automatic fallback, which for this page is still manual.
pub fn parse_cops_mode(resp: &str) -> Option<&'static str> {
    let rest = resp.split("+COPS:").nth(1)?.trim_start();
    match rest.split(|c: char| c == ',' || c.is_whitespace()).next()? {
        "0" => Some("auto"),
        "1" | "4" => Some("manual"),
        _ => None,
    }
}

#[derive(Clone, Default)]
pub struct Guard {
    /// idle | registering | ok | reverting | reverted | revert_failed
    pub phase: &'static str,
    pub target: String,
    pub rat: String,
    pub reason: String,
    /// Which message `reason` is (see [`Reason`]); what clients compare.
    pub reason_code: &'static str,
    pub reason_en: String,
    pub started_at: i64,
    pub finished_at: i64,
    /// Last raw register result, kept for the device probe.
    pub last_result: String,
}

impl Guard {
    pub fn set_reason(&mut self, r: Reason) {
        self.reason = r.zh;
        self.reason_code = r.code;
        self.reason_en = r.en;
    }

    pub fn json(&self) -> Value {
        json!({
            "phase": if self.phase.is_empty() { "idle" } else { self.phase },
            "target": self.target,
            "rat": self.rat,
            "reason": self.reason,
            "reason_code": (!self.reason_code.is_empty()).then_some(self.reason_code),
            "reason_en": (!self.reason_en.is_empty()).then(|| self.reason_en.clone()),
            "started_at": self.started_at,
            "finished_at": self.finished_at,
            "last_result": self.last_result,
        })
    }
}

/// One `guard.reason`, in both languages, with a code naming the exact
/// message. `reason_code == "manual_auto"` means the reason is exactly
/// 手动恢复自动 and nothing else: the admin web shows its own "back to
/// automatic" line then, and the reason text otherwise (it used to compare the
/// Chinese string).
#[derive(Debug, PartialEq)]
pub struct Reason {
    pub code: &'static str,
    pub zh: String,
    pub en: String,
}

/// Code and English for the base reasons (`Verdict::Revert`, the resume
/// paths, the button). Kept apart from the Chinese, which existing callers
/// and tests pass around as `&'static str`.
pub fn why_info(why: &str) -> (&'static str, &'static str) {
    match why {
        "注册失败" => ("register_failed", "Registration failed"),
        "已注册但数据没通" => ("registered_no_data", "Registered but no data"),
        "超时没注册上" => ("register_timeout", "Timed out before registering"),
        "手动恢复自动" => ("manual_auto", "Back to automatic on request"),
        "重启前没做完" => ("resume_revert", "Unfinished before a restart"),
        "重启前还没拨上" => ("resume_redial", "No data yet before a restart"),
        _ => ("other", "Back to automatic"),
    }
}

impl Reason {
    pub fn why(why: &str) -> Self {
        let (code, en) = why_info(why);
        Reason {
            code,
            zh: why.to_string(),
            en: en.to_string(),
        }
    }

    /// A back-to-automatic attempt failed; trying again later.
    pub fn auto_retry(why: &str, attempt: u32, err: &str, backoff: u64) -> Self {
        let err_en = if err == AUTO_NOT_YET {
            AUTO_NOT_YET_EN
        } else {
            err
        };
        Reason {
            code: "auto_retry",
            zh: clip(format!(
                "{why}；第 {attempt} 次恢复自动没成功（{err}），{backoff} 秒后再试"
            )),
            en: clip_en(format!(
                "{}; back-to-automatic attempt {attempt} failed ({err_en}); retrying in {backoff} s",
                why_info(why).1
            )),
        }
    }

    /// Automatic again, but the data call did not come back. `why` empty: said
    /// on its own (after a restart).
    pub fn auto_no_data(why: &str) -> Self {
        if why.is_empty() {
            return Reason {
                code: "auto_no_data",
                zh: "已回到自动选网，但数据还没拨上".into(),
                en: "Automatic again, but no data yet".into(),
            };
        }
        Reason {
            code: "auto_no_data",
            zh: clip(format!("{why}；已回到自动选网，但数据还没拨上")),
            en: clip_en(format!(
                "{}; automatic again, but no data yet",
                why_info(why).1
            )),
        }
    }

    /// core holds back-to-automatic while a network-mode change is being
    /// confirmed (D40 `op_busy`): not a failed attempt, waiting it out.
    pub fn auto_held(why: &str) -> Self {
        Reason {
            code: "auto_held",
            zh: clip(format!("{why}；等网络模式确认完再回自动")),
            en: clip_en(format!("{}; waiting for the mode check", why_info(why).1)),
        }
    }

    /// Automatic again; data attempt `n` failed, next one after `pause`.
    pub fn redial_retry(n: usize, pause: u64) -> Self {
        Reason {
            code: "redial_retry",
            zh: clip(format!(
                "已回到自动选网，数据第 {n} 次没拨上，{pause} 秒后再试"
            )),
            en: clip_en(format!(
                "Automatic again; data attempt {n} failed; retrying in {pause} s"
            )),
        }
    }
}

pub const AUTO_NOT_YET: &str = "模组还没回到自动";
pub const AUTO_NOT_YET_EN: &str = "modem not automatic yet";

/// English for the user-facing errors this module words in Chinese
/// ([`claim`], the APN and marker refusals, scan and lookup failures). Errors
/// passed through from ubus, the AT port or HTTP are English already and come
/// back unchanged. None when there is no English for it (a message clipped
/// mid-pattern, a firmware string in Chinese): sent as null, so clients show
/// the Chinese `error` rather than a half-translated line.
pub fn error_en(zh: &str) -> Option<String> {
    Some(error_en_raw(zh)).filter(|e| !crate::health::has_cjk(e))
}

pub fn error_en_raw(zh: &str) -> String {
    let fixed = match zh {
        "正在搜索网络，等搜完再操作" | "正在搜索网络，搜完再操作" => {
            Some("Searching for networks; try again when it finishes")
        }
        "正在选网，等它结束" => Some("Registering on a network; wait for it to finish"),
        "正在回到自动选网，等它结束" => {
            Some("Going back to automatic; wait for it to finish")
        }
        "没有这个手动 APN" => Some("No such manual APN"),
        "没有这个候选 APN" => Some("No such candidate APN"),
        "复制成手动 APN 后没读到它" => {
            Some("Copied the APN to the manual list but couldn't read it back")
        }
        "读不到 SIM 卡号，没法只给这张卡记住" => {
            Some("Can't read the SIM number, so the pick can't be kept for this SIM")
        }
        "上一次 APN 切换还没做完" => Some("The last APN switch hasn't finished"),
        "没搜到网络" => Some("No networks found"),
        "没有可用的查询地址" => Some("No lookup address available"),
        NBR_UNSUPPORTED => Some(NBR_UNSUPPORTED_EN),
        _ => None,
    };
    if let Some(f) = fixed {
        return f.to_string();
    }
    if let Some(e) = zh.strip_prefix("写不了保护标记，没有注册：") {
        return format!("Couldn't write the guard marker; not registered: {e}");
    }
    if let Some(e) = zh.strip_prefix("写不了保护标记：") {
        return format!("Couldn't write the guard marker: {e}");
    }
    if let Some(st) = zh
        .strip_prefix("模组报告搜索失败（")
        .and_then(|r| r.strip_suffix('）'))
    {
        return format!("Modem reported the search failed ({st})");
    }
    if let Some(host) = zh.strip_suffix(": 看不懂返回") {
        return format!("{host}: unreadable reply");
    }
    zh.to_string()
}

/// `error_en` for an optional error, null when there is none.
pub fn error_en_opt(e: &Option<String>) -> Option<String> {
    e.as_deref().and_then(error_en)
}

pub const LEASES: &str = "/tmp/dhcp.leases";
pub const CLIENTS_MAX: usize = 12;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Station {
    pub mac: String,
    pub iface: String,
    pub down: u64,
    pub up: u64,
    pub signal: Option<i32>,
    pub connected_secs: u64,
    /// From the AP interface: "2.4 GHz" | "5 GHz" | "6 GHz" | "" (written
    /// as GHz so it is never read as the cellular 5G).
    pub band: &'static str,
    pub channel: Option<u32>,
    pub width_mhz: Option<u32>,
    /// From the station's tx bitrate: EHT = Wi-Fi 7, HE = 6, VHT = 5, HT = 4.
    pub wifi_gen: Option<u8>,
    /// Negotiated link rates in Mbit/s (AP tx = client download).
    pub link_down_mbps: Option<u32>,
    pub link_up_mbps: Option<u32>,
    /// AP → station frames and their retries (deep diagnosis' retry rate).
    pub tx_packets: Option<u64>,
    pub tx_retries: Option<u64>,
}

impl Station {
    pub fn signal(&self) -> Option<i32> {
        self.signal
    }
    pub fn link_down_mbps(&self) -> Option<u32> {
        self.link_down_mbps
    }
    pub fn tx_packets(&self) -> Option<u64> {
        self.tx_packets
    }
    pub fn tx_retries(&self) -> Option<u64> {
        self.tx_retries
    }
}

/// `iw dev <ap> info`: "channel 44 (5220 MHz), width: 160 MHz, center1: …".
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct ApInfo {
    pub band: &'static str,
    pub channel: Option<u32>,
    pub width_mhz: Option<u32>,
}

pub fn parse_ap_info(text: &str) -> ApInfo {
    let mut out = ApInfo::default();
    let Some(l) = text
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("channel "))
    else {
        return out;
    };
    out.channel = l["channel ".len()..]
        .split_whitespace()
        .next()
        .and_then(|c| c.parse().ok());
    let freq: Option<u32> = l
        .split_once('(')
        .and_then(|(_, r)| r.split_whitespace().next())
        .and_then(|f| f.parse().ok());
    out.band = match freq {
        Some(f) if (2400..2500).contains(&f) => "2.4 GHz",
        Some(f) if (5150..5900).contains(&f) => "5 GHz",
        Some(f) if (5925..7125).contains(&f) => "6 GHz",
        _ => "",
    };
    out.width_mhz = l
        .split_once("width:")
        .and_then(|(_, r)| r.split_whitespace().next())
        .and_then(|w| w.parse().ok());
    out
}

/// "2401.9 MBit/s 160MHz HE-MCS 11 …" → (2402, Some(6)).
pub fn parse_bitrate(v: &str) -> (Option<u32>, Option<u8>) {
    let mbps = v
        .split_whitespace()
        .next()
        .and_then(|n| n.parse::<f64>().ok())
        .map(|m| m.round() as u32);
    let wgen = if v.contains("EHT-") {
        Some(7)
    } else if v.contains("HE-") {
        Some(6)
    } else if v.contains("VHT-") {
        Some(5)
    } else if v.contains(" MCS ") || v.contains("HT") {
        Some(4)
    } else {
        None
    };
    (mbps, wgen)
}

/// AP interfaces from `iw dev`: "Interface wlan0" … "type AP".
pub fn ap_ifaces(iw_dev: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    for l in iw_dev.lines().map(str::trim) {
        if let Some(n) = l.strip_prefix("Interface ") {
            cur = Some(n.trim().to_string());
        } else if l == "type AP"
            && let Some(n) = cur.take()
        {
            out.push(n);
        }
    }
    out
}

pub fn parse_station_dump(text: &str, iface: &str) -> Vec<Station> {
    let mut out: Vec<Station> = Vec::new();
    for l in text.lines() {
        let t = l.trim();
        if let Some(rest) = t.strip_prefix("Station ") {
            let mac = rest
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            out.push(Station {
                mac,
                iface: iface.to_string(),
                ..Station::default()
            });
            continue;
        }
        let Some(st) = out.last_mut() else { continue };
        let Some((k, v)) = t.split_once(':') else {
            continue;
        };
        let first = v.split_whitespace().next().unwrap_or("");
        match k.trim() {
            "rx bytes" => st.up = first.parse().unwrap_or(0),
            "tx bytes" => st.down = first.parse().unwrap_or(0),
            "signal" => st.signal = first.parse().ok(),
            "connected time" => st.connected_secs = first.parse().unwrap_or(0),
            "tx packets" => st.tx_packets = first.parse().ok(),
            "tx retries" => st.tx_retries = first.parse().ok(),
            "tx bitrate" => {
                let (m, g) = parse_bitrate(v.trim());
                st.link_down_mbps = m;
                st.wifi_gen = g.or(st.wifi_gen);
            }
            "rx bitrate" => {
                let (m, g) = parse_bitrate(v.trim());
                st.link_up_mbps = m;
                st.wifi_gen = st.wifi_gen.or(g);
            }
            _ => {}
        }
    }
    out
}

/// `/tmp/dhcp.leases`: "<expiry> <mac> <ip> <name> <client-id>"; name "*" = none.
pub fn parse_leases(text: &str) -> Vec<(String, String, String)> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            (f.len() >= 4).then(|| {
                let name = if f[3] == "*" {
                    String::new()
                } else {
                    f[3].to_string()
                };
                (f[1].to_ascii_lowercase(), f[2].to_string(), name)
            })
        })
        .collect()
}

/// Rate in bytes/s from two cumulative readings; a counter that went down
/// (the station re-associated) gives no rate rather than a huge one.
pub fn rate(prev: Option<(u64, i64)>, now_bytes: u64, t: i64) -> Option<u64> {
    let (b, t0) = prev?;
    (now_bytes >= b && t > t0).then(|| (now_bytes - b) / (t - t0) as u64)
}

/// The Wi-Fi part of one station, shared by /api/netinfo clients and
/// /api/network/clients `wifi`.
pub fn station_wifi_json(st: &Station) -> Value {
    json!({
        "mac": st.mac,
        "iface": st.iface,
        "band": (!st.band.is_empty()).then_some(st.band),
        "channel": st.channel,
        "width_mhz": st.width_mhz,
        "wifi_gen": st.wifi_gen,
        "link_down_mbps": st.link_down_mbps,
        "link_up_mbps": st.link_up_mbps,
        "signal": st.signal,
        "signal_tier": st.signal.map(signal_tier),
        "connected_secs": st.connected_secs,
    })
}

/// How good a client's Wi-Fi signal is at the AP (RSSI, dBm): the one rule
/// the admin web and the touch screen both word ("great" 很好 / "good" 好 /
/// "fair" 一般 / "weak" 弱). Moved here from both of them 2026-09-26.
pub fn signal_tier(dbm: i32) -> &'static str {
    if dbm >= -55 {
        "great"
    } else if dbm >= -67 {
        "good"
    } else if dbm >= -75 {
        "fair"
    } else {
        "weak"
    }
}

#[derive(Default)]
pub struct Scan {
    /// idle | scanning | done | error
    pub state: &'static str,
    pub started_at: i64,
    pub finished_at: i64,
    pub error: String,
    pub operators: Vec<Value>,
    /// Last raw status, kept for the device probe.
    pub last_status: String,
}

impl Scan {
    pub fn json(&self) -> Value {
        json!({
            "state": if self.state.is_empty() { "idle" } else { self.state },
            "started_at": self.started_at,
            "finished_at": self.finished_at,
            "error": (!self.error.is_empty()).then(|| self.error.clone()),
            "error_en": (!self.error.is_empty()).then(|| error_en(&self.error)).flatten(),
            "last_status": self.last_status,
            "operators": self.operators,
        })
    }
}

#[derive(Clone, Default)]
pub struct Lookup {
    pub geo: Geo,
    pub source: String,
    pub node: Option<String>,
    pub fetched_at: i64,
    pub error: Option<String>,
}

impl Lookup {
    pub fn json(&self) -> Value {
        let s = |v: &str| (!v.is_empty()).then(|| v.to_string());
        json!({
            "ip": s(&self.geo.ip),
            "geo": s(&self.geo.geo),
            "geo_en": s(&geo_en(&self.geo.geo)),
            "isp": s(&self.geo.isp),
            "isp_en": s(&isp_en(&self.geo.isp)),
            "node": self.node,
            "source": s(&self.source),
            "fetched_at": self.fetched_at,
            "error": self.error,
            "error_en": error_en_opt(&self.error),
        })
    }
}

#[derive(Default)]
pub struct Slot {
    pub value: Option<Lookup>,
    pub key: String,
    pub retry_at: i64,
}

impl Slot {
    pub fn due(&self, key: &str, t: i64) -> bool {
        if key != self.key {
            return true;
        }
        match &self.value {
            None => t >= self.retry_at,
            Some(l) if l.error.is_some() => t >= self.retry_at,
            Some(l) => t - l.fetched_at >= LOOKUP_MAX_AGE,
        }
    }

    pub fn store(
        &mut self,
        key: &str,
        t: i64,
        res: Result<(Geo, String), String>,
        node: Option<String>,
    ) {
        let same = key == self.key;
        self.key = key.to_string();
        match res {
            Ok((geo, source)) => {
                self.value = Some(Lookup {
                    geo,
                    source,
                    node,
                    fetched_at: t,
                    error: None,
                });
            }
            Err(e) => {
                self.retry_at = t + LOOKUP_RETRY;
                // Keep the last good answer only while the exit is unchanged.
                let mut l = if same {
                    self.value.take().unwrap_or_default()
                } else {
                    Lookup::default()
                };
                l.error = Some(clip(e));
                l.node = node;
                if !same {
                    l.fetched_at = t;
                }
                self.value = Some(l);
            }
        }
    }
}

pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_i64() == Some(1),
        Value::String(s) => s == "1" || s == "true",
        _ => false,
    }
}

pub fn apn_entry(p: &Value, in_use: &str) -> Value {
    let id = p["profileId"].as_str().unwrap_or("");
    json!({
        "id": id,
        "name": p["profilename"].as_str().unwrap_or(""),
        "apn": p["wanapn"].as_str().unwrap_or(""),
        "pdp": p["pdpType"].as_i64().unwrap_or(0),
        "selected": truthy(&p["isEnable"]),
        "in_use": !id.is_empty() && id == in_use,
        "iot": is_iot(p["wanapn"].as_str().unwrap_or("")),
    })
}

/// mode (get_apn_mode), dialled (get_apn_at_cid cid 1), the auto and manual lists.
pub fn apn_view(mode: &Value, dialled: &Value, auto: &Value, manual: &Value) -> Value {
    let mode = match &mode["apn_mode"] {
        Value::String(s) => s.parse().unwrap_or(0),
        v => v.as_i64().unwrap_or(0),
    };
    let in_use = dialled["profileId"].as_str().unwrap_or("");
    let list = |v: &Value| -> Vec<Value> {
        v["apnListArray"]
            .as_array()
            .map(|a| a.iter().map(|p| apn_entry(p, in_use)).collect())
            .unwrap_or_default()
    };
    json!({
        "mode": if mode == 1 { "manual" } else { "auto" },
        "in_use": if in_use.is_empty() { Value::Null } else { apn_entry(dialled, in_use) },
        "auto": list(auto),
        "manual": list(manual),
    })
}

pub const NBR_UNSUPPORTED: &str = "原厂扫描会断网且拿不到数据，已停用";
pub const NBR_UNSUPPORTED_EN: &str = "Off: the stock scan drops data, finds nothing";

/// IoT / M2M APNs: they bring up a bearer but not the normal internet, which
/// is exactly why the firmware keeps picking them. Shown, tagged, not hidden.
pub fn is_iot(apn: &str) -> bool {
    let a = apn.to_ascii_lowercase();
    a.contains("iot") || a.starts_with("m2m")
}

/// Give up and go back to automatic this long after the register call. The
/// modem says `manual_fail` when it is refused, so this only catches a
/// register that hangs; the stock page waits for the answer with no limit.
/// Worst case to automatic: 90 + one poll (3) + `AT+COPS=0` (8) + read-back (2) ≈ 103 s.
pub const GUARD_TIMEOUT: i64 = 90;

/// A "fail" read this soon after the call can still be the previous attempt's.
pub const GUARD_FAIL_GRACE: i64 = 6;

pub const SCAN_MAX_OPS: usize = 12;

// ── register guard ─────────────────────────────────────────────────────────

pub struct Obs<'a> {
    /// `nwinfo_m_netselect_result`'s answer. The stock page reads
    /// "manual_success" / "manual_fail" and keeps waiting on anything else.
    pub result: &'a str,
    /// Its value just before our register call: the same value again is the
    /// previous attempt's answer, not this one's.
    pub before: &'a str,
    pub serving: &'a str,
    pub connected: bool,
}

#[derive(Debug, PartialEq)]
pub enum Verdict {
    Wait,
    Ok,
    Revert(&'static str),
}

/// One observation → what to do. Success is judged by where the modem
/// actually is, not the result string: an unexpected "success" spelling must
/// not get a working registration undone, and a "success" while still on the
/// old network is not success.
pub fn decide(target: &str, o: &Obs, elapsed: i64) -> Verdict {
    if same_plmn(target, o.serving) && o.connected {
        return Verdict::Ok;
    }
    let r = o.result.trim().to_ascii_lowercase();
    let fresh = o.result != o.before || elapsed >= GUARD_TIMEOUT / 2;
    let failed = r.contains("fail") || r == "error";
    if fresh && elapsed >= GUARD_FAIL_GRACE && failed {
        return Verdict::Revert("注册失败");
    }
    if elapsed >= GUARD_TIMEOUT {
        return Verdict::Revert(if same_plmn(target, o.serving) {
            "已注册但数据没通"
        } else {
            "超时没注册上"
        });
    }
    Verdict::Wait
}

pub fn register_result_str(v: &Value) -> String {
    for k in [
        "result",
        "m_netselect_result",
        "netselect_result",
        "m_result",
    ] {
        match v.get(k) {
            Some(Value::String(s)) => return s.clone(),
            Some(Value::Number(n)) => return n.to_string(),
            _ => {}
        }
    }
    String::new()
}

pub fn field<'a>(o: &'a serde_json::Map<String, Value>, keys: &[&str]) -> Option<&'a Value> {
    o.iter()
        .find(|(k, _)| {
            let k = k.to_ascii_lowercase();
            keys.iter().any(|want| k.contains(want))
        })
        .map(|(_, v)| v)
        .filter(|v| !v.is_null() && v.as_str() != Some(""))
}

// ── one modem operation at a time ──────────────────────────────────────────
//
// Search, manual register (with its guard) and back-to-automatic all change
// what the modem is doing; any two overlapping is a mess (a register during a
// 110 s search, a revert racing a register). One lock does check-and-claim for
// all of them.

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum OpKind {
    #[default]
    Idle,
    Scanning,
    Registering,
    Reverting,
}

#[derive(Default)]
pub struct Ops {
    pub kind: OpKind,
    pub guard: Guard,
    pub scan: Scan,
    /// "Back to automatic" asked for while a register is being guarded. Read
    /// and cleared under this same lock, so an accepted request is never lost.
    pub cancel: bool,
}

/// Claim the modem for `want`, or say what is in the way.
pub fn claim(ops: &mut Ops, want: OpKind) -> Result<(), &'static str> {
    match ops.kind {
        OpKind::Idle => {
            ops.kind = want;
            Ok(())
        }
        OpKind::Scanning => Err("正在搜索网络，等搜完再操作"),
        OpKind::Registering => Err("正在选网，等它结束"),
        OpKind::Reverting => Err("正在回到自动选网，等它结束"),
    }
}

/// First string (or number) among keys containing any of `keys`.
pub fn loose_str(v: &Value, keys: &[&str]) -> String {
    let Some(o) = v.as_object() else {
        return String::new();
    };
    match field(o, keys) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// `nwinfo_m_netselect_status`: the web page treats "done"/"complete"/"2" as
/// finished. Anything clearly failed ends it too; the rest means keep waiting.
/// Measured 2026-09-25: "manual_selecting" while searching (≈110 s, data
/// down), "manual_selected" when done.
pub fn scan_finished(status: &str) -> Option<bool> {
    let s = status.trim().to_ascii_lowercase();
    if s.contains("fail") || s.contains("error") || s == "3" {
        return Some(false);
    }
    if s.contains("selected")
        || [
            "done",
            "complete",
            "completed",
            "finish",
            "finished",
            "success",
            "2",
        ]
        .contains(&s.as_str())
    {
        return Some(true);
    }
    None
}

/// Anything with an `m_mcc_mnc` in `nwinfo_m_netselect_contents`, named from
/// our own table when we know the PLMN. `m_rat` is passed back verbatim.
pub fn collect_operators(v: &Value, out: &mut Vec<Value>) {
    match v {
        // Measured 2026-09-25: `"m_netselect_contents": ""` when idle, so the
        // filled form is a string too. Records split by ';', fields by ','; the
        // field order is not known yet, so pick fields by what they look like.
        Value::String(s) => s.split(';').filter(|r| r.contains(',')).for_each(|r| {
            if let Some(op) = operator_from_record(r) {
                out.push(op);
            }
        }),
        Value::Array(a) => a.iter().for_each(|x| collect_operators(x, out)),
        Value::Object(o) => {
            if o.contains_key("m_mcc_mnc") {
                let plmn = loose_str(v, &["m_mcc_mnc"]);
                let raw_name = loose_str(v, &["oper_name", "long", "name"]);
                let p = plmn_from_str(&plmn);
                let name = p
                    .as_ref()
                    .and_then(|p| operator(p.mcc, p.mnc))
                    .map(str::to_string)
                    .unwrap_or(raw_name.clone());
                let (country, country_en, country_iso) = country_fields(p.as_ref().map(|p| p.mcc));
                out.push(json!({
                    "plmn": plmn,
                    "operator_en": operator_en(&name),
                    "name": name,
                    "raw_name": raw_name,
                    "country": country,
                    "country_en": country_en,
                    "country_iso": country_iso,
                    "rat": loose_str(v, &["m_rat"]),
                    "status": loose_str(v, &["m_status", "stat"]),
                }));
            } else {
                o.values().for_each(|x| collect_operators(x, out));
            }
        }
        _ => {}
    }
}

pub fn is_small_int(f: &str) -> bool {
    !f.is_empty() && f.len() <= 2 && f.bytes().all(|b| b.is_ascii_digit())
}

/// One `nwinfo_m_netselect_contents` record. Measured 2026-09-25:
/// "status,name,plmn,rat;" e.g. "2,CT,46011,11;3,CMCC,46000,7;" — status
/// 1 available / 2 current / 3 forbidden, rat 2 UTRAN / 7 E-UTRAN / 11 NR.
/// Fields are still picked by shape (PLMN = the 5–6 digit one), so a firmware
/// that reorders them keeps working.
pub fn operator_from_record(r: &str) -> Option<Value> {
    let f: Vec<&str> = r.split(',').map(|x| x.trim().trim_matches('"')).collect();
    let pi = f.iter().position(|x| plmn_from_str(x).is_some())?;
    let plmn = f[pi];
    let raw_name = f
        .iter()
        .find(|x| x.bytes().any(|b| b.is_ascii_alphabetic()))
        .copied()
        .unwrap_or("");
    let status = f[..pi]
        .iter()
        .rev()
        .find(|x| is_small_int(x))
        .copied()
        .unwrap_or("");
    let rat = f[pi + 1..]
        .iter()
        .find(|x| is_small_int(x))
        .copied()
        .unwrap_or("");
    let p = plmn_from_str(plmn)?;
    let name = operator(p.mcc, p.mnc).unwrap_or(raw_name);
    let (country, country_en, country_iso) = country_fields(Some(p.mcc));
    Some(json!({
        "plmn": plmn,
        "name": name,
        "operator_en": operator_en(name),
        "raw_name": raw_name,
        "country": country,
        "country_en": country_en,
        "country_iso": country_iso,
        "rat": rat,
        "status": status,
        "raw": r,
    }))
}

/// What the marker file says is in flight. Written before the modem is
/// touched, removed only once the outcome is confirmed, so a restart
/// (resume_guard) always knows what to finish.
#[derive(Clone, Debug, PartialEq)]
pub enum Marker {
    Register {
        target: String,
        started: i64,
    },
    Revert,
    /// Automatic again; only the data call is still to come back.
    Redial,
}

pub fn parse_marker(text: &str) -> Option<Marker> {
    let f: Vec<&str> = text.split_whitespace().collect();
    match f.as_slice() {
        ["revert", ..] => Some(Marker::Revert),
        ["redial", ..] => Some(Marker::Redial),
        ["register", t, s] | [t, s] if plmn_from_str(t).is_some() => Some(Marker::Register {
            target: t.to_string(),
            started: s.parse().ok()?,
        }),
        _ => None,
    }
}

/// The marker file's line for `m`, written at `now`.
pub fn marker_text(m: &Marker, now: i64) -> String {
    match m {
        Marker::Register { target, started } => format!("register {target} {started}\n"),
        Marker::Revert => format!("revert {now}\n"),
        Marker::Redial => format!("redial {now}\n"),
    }
}

// ── the pieces u60d assembles too ─────────────────────────────────────────

/// The local part of the page from `nwinfo_get_netinfo`, the SIM's state and IMSI, and
/// `get_wwaniface`: `home_operator`, `serving_operator`, `roaming`, `roaming_raw`, `network_type`,
/// `data_connected`. Also the selection mode when the netinfo call reports it.
pub fn local_json(
    net: &Value,
    sim_state: &str,
    imsi: &str,
    wwan: &Value,
) -> (Value, Option<&'static str>) {
    let home = sim_state
        .contains("ready")
        .then(|| plmn_from_imsi(imsi))
        .flatten()
        .map(|p| plmn_json(&p, ""));

    let serving_str = serving_plmn(net);
    let provider = {
        let f = js(net, "network_provider_fullname");
        if f.is_empty() {
            js(net, "network_provider")
        } else {
            f
        }
    };
    let serving = match plmn_from_str(&serving_str) {
        Some(p) => plmn_json(&p, provider),
        None if !provider.is_empty() => json!({
            "name": provider, "operator_en": provider, "mcc": null, "mnc": null,
            "country": null, "country_en": null, "country_iso": null,
        }),
        None => Value::Null,
    };
    let roam_raw = js(net, "simcard_roam");
    let roaming = match roam_raw.to_ascii_lowercase().as_str() {
        "" => Value::Null,
        "home" | "0" => json!(false),
        _ => json!(true),
    };
    let snap = json!({
        "home_operator": home,
        "serving_operator": serving,
        "roaming": roaming,
        "roaming_raw": roam_raw,
        "network_type": js(net, "network_type"),
        "data_connected": data_connected(wwan),
    });
    (snap, selection_from_netinfo(net))
}

/// Each station's last byte counters: mac → (down, up, at).
pub type Counters = std::collections::HashMap<String, (u64, u64, i64)>;

/// Per-client rows for the page: the busiest [`CLIENTS_MAX`] stations with their lease's name and
/// address, cumulative bytes and the rate since `prev` (mac → (down, up, at)). Returns the rows and
/// the next `prev`.
pub fn clients_list(
    mut stations: Vec<Station>,
    leases: &[(String, String, String)],
    prev: &Counters,
    t: i64,
) -> (Vec<Value>, Counters) {
    stations.sort_by_key(|s| std::cmp::Reverse(s.down + s.up));
    let mut next = std::collections::HashMap::new();
    let list = stations
        .iter()
        .take(CLIENTS_MAX)
        .map(|st| {
            let lease = leases.iter().find(|l| l.0 == st.mac);
            let p = prev.get(&st.mac);
            next.insert(st.mac.clone(), (st.down, st.up, t));
            let mut v = station_wifi_json(st);
            let o = v.as_object_mut().expect("object");
            o.insert(
                "name".into(),
                json!(lease.map(|l| l.2.clone()).filter(|n| !n.is_empty())),
            );
            o.insert("ip".into(), json!(lease.map(|l| l.1.clone())));
            o.insert("down_bytes".into(), json!(st.down));
            o.insert("up_bytes".into(), json!(st.up));
            o.insert(
                "down_rate".into(),
                json!(rate(p.map(|p| (p.0, p.2)), st.down, t)),
            );
            o.insert(
                "up_rate".into(),
                json!(rate(p.map(|p| (p.1, p.2)), st.up, t)),
            );
            v
        })
        .collect();
    (list, next)
}

/// The vendor neighbour scan drops data and finds nothing, so it is off: the block always says so.
pub fn neighbors_json(scanned_at: i64, cells: &[Value]) -> Value {
    json!({
        "state": "unsupported",
        "scanned_at": scanned_at,
        "error": NBR_UNSUPPORTED,
        "error_en": NBR_UNSUPPORTED_EN,
        "cells": cells,
    })
}

/// The APN choices remembered per SIM (zte-agent `apn_pick`; `{"picks": {iccid: {"manual_id",
/// "apn"}}, "last_iccid", "notice"}`).
pub const APN_PICK_STORE: &str = "/data/apn-pick.json";

/// For the APN view: this card's picked manual id, and the last notice if it is under 10 minutes
/// old or sticky (null otherwise, so the UIs don't keep an old one).
pub fn apn_pick_view(store: &Value, iccid: &str, now: u64) -> (Option<String>, Value) {
    let picked = store["picks"]
        .get(iccid)
        .and_then(|p| p["manual_id"].as_str())
        .map(str::to_string);
    (picked, fresh_notice(&store["notice"], now))
}

pub fn fresh_notice(n: &Value, now: u64) -> Value {
    if n["sticky"].as_bool() == Some(true) {
        return n.clone();
    }
    match n["at"].as_u64() {
        Some(at) if now >= at && now - at < 600 => n.clone(),
        _ => Value::Null,
    }
}

/// The fields that make two APN profiles the same (an auto candidate and its manual copy).
pub const SAME: [&str; 6] = [
    "wanapn",
    "pdpType",
    "roamingPdpType",
    "pppAuthMode",
    "username",
    "password",
];

fn same_profile(a: &Value, b: &Value) -> bool {
    SAME.iter().all(|k| {
        let (x, y) = (&a[*k], &b[*k]);
        // Numbers and numeric strings both appear in the vendor lists.
        x == y
            || (x.is_null() && y.as_str() == Some(""))
            || (y.is_null() && x.as_str() == Some(""))
            || x.to_string().trim_matches('"') == y.to_string().trim_matches('"')
    })
}

/// The manual profile identical to `cand`, if the manual list has one.
/// The manual profile that dials the same as `cand`, if there is one.
pub fn find_same(manual: &Value, cand: &Value) -> Option<String> {
    manual["apnListArray"]
        .as_array()?
        .iter()
        .find(|p| same_profile(p, cand))
        .and_then(|p| p["profileId"].as_str())
        .map(str::to_string)
}

/// A profile's id, as a string (`profileId` or `profile_id`, string or number).
pub fn profile_id(v: &Value) -> Option<String> {
    match v.get("profileId").or_else(|| v.get("profile_id"))? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// A vendor-format APN profile (the web body, or an entry of
/// `get_auto_apn_list`) → core `apn.add` / `apn.modify` params. B27 keeps
/// `pdpType`, `pppAuthMode`, `roamingPdpType` as integers; core sends them so.
pub fn apn_params(v: &Value, modify: bool) -> Result<Value, String> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    let int = |k: &str| -> Result<Option<i64>, String> {
        match v.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Number(n)) => n
                .as_i64()
                .map(Some)
                .ok_or_else(|| format!("{k} must be a number")),
            Some(Value::String(x)) => x
                .trim()
                .parse()
                .map(Some)
                .map_err(|_| format!("{k} must be a number (this firmware stores numbers)")),
            _ => Err(format!("{k} must be a number")),
        }
    };
    let mut p = serde_json::Map::new();
    if modify {
        p.insert(
            "profile_id".into(),
            json!(profile_id(v).ok_or("missing profileId")?),
        );
    }
    p.insert(
        "name".into(),
        json!(s("profilename").ok_or("missing profilename")?),
    );
    p.insert("apn".into(), json!(s("wanapn").ok_or("missing wanapn")?));
    for (from, to) in [("username", "username"), ("password", "password")] {
        if let Some(x) = s(from) {
            p.insert(to.into(), json!(x));
        }
    }
    for (from, to) in [
        ("pppAuthMode", "auth_mode"),
        ("pdpType", "pdp_type"),
        ("roamingPdpType", "roaming_pdp_type"),
    ] {
        if let Some(x) = int(from)? {
            p.insert(to.into(), json!(x));
        }
    }
    Ok(Value::Object(p))
}

/// The `apn` block: the four `zwrt_apn_object` reads (mode, the dialled profile, the auto and
/// manual lists; `Err` when the mode read failed), this card's pick, and the switch in progress.
pub fn apn_block(
    reads: Result<(Value, Value, Value, Value), String>,
    picked: Option<String>,
    notice: Value,
    switching: bool,
    switch_error: &str,
) -> Value {
    let mut v = match reads {
        Err(e) => json!({"error": e}),
        Ok((mode, dialled, auto, manual)) => apn_view(&mode, &dialled, &auto, &manual),
    };
    if let Value::Object(o) = &mut v {
        o.insert("picked_id".into(), json!(picked));
        o.insert("notice".into(), notice);
        o.insert("switching".into(), json!(switching));
        o.insert("switch_error".into(), json!(switch_error));
    }
    v
}

/// What GET /api/netinfo asked for: `lite=1` (the touch screen's poll, no selection read),
/// `apn=1` and `clients=1` (exact `key=value` tokens).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Query {
    pub lite: bool,
    pub apn: bool,
    pub clients: bool,
}

impl Query {
    pub fn parse(query: &str) -> Query {
        let has = |k: &str| query.split('&').any(|kv| kv == k);
        Query {
            lite: has("lite=1"),
            apn: has("apn=1"),
            clients: has("clients=1"),
        }
    }

    /// The APN block's four ubus reads are only made for a full read or `apn=1`.
    pub fn wants_apn(&self) -> bool {
        !self.lite || self.apn
    }
}

/// Everything on the page, as the refresh pass and the request left it.
pub struct Page<'a> {
    pub now: i64,
    pub direct: Value,
    /// Already null unless CHILL is running.
    pub proxy: Value,
    pub chill_running: bool,
    pub selection: Option<&'a str>,
    pub selection_at: i64,
    pub guard: Value,
    pub scan: Value,
    pub neighbors: Value,
    pub clients: &'a [Value],
    pub clients_at: i64,
    pub scenes: Value,
    /// Null for a lite read without `apn=1`.
    pub apn: Value,
    /// [`local_json`]'s object, merged in at the top level.
    pub local: &'a Value,
}

/// `data` of GET /api/netinfo.
pub fn page(p: Page) -> Value {
    let mut data = json!({
        "now": p.now,
        "direct": p.direct,
        "proxy": p.proxy,
        "chill_running": p.chill_running,
        "selection": {"mode": p.selection, "checked_at": p.selection_at},
        "guard": p.guard,
        "neighbors": p.neighbors,
        "scan": p.scan,
        "clients": {"at": p.clients_at, "list": p.clients},
        "scenes": p.scenes,
        "apn": p.apn,
    });
    if let (Some(d), Some(l)) = (data.as_object_mut(), p.local.as_object()) {
        for (k, v) in l {
            d.insert(k.clone(), v.clone());
        }
    }
    data
}

pub mod iw;

#[cfg(test)]
mod tests;
