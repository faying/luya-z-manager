//! netinfo.rs — the touch screen's 网络 page and the web's 网络身份 card.
//!
//!   GET  /api/netinfo                     both exit IPs + where they are, the SIM's
//!                                         home operator vs the serving one, roaming,
//!                                         auto/manual selection, register guard,
//!                                         last neighbour scan
//!   POST /api/modem/register              (replaces the bare passthrough) manual
//!                                         register + a guard that returns to
//!                                         automatic selection if it does not take
//!   GET  /api/modem/register/guard        the guard's state
//!   POST /api/modem/netselect/auto        back to automatic selection now
//!   POST /api/netinfo/neighbors/scan      one neighbour scan; result lands in
//!                                         /api/netinfo `neighbors`
//!   POST /api/netinfo/scan                one operator search (drops data for
//!                                         1–3 min); result lands in `scan`
//!
//! Design: docs/designs/netinfo-screen.md. Things that bite:
//!
//! - **Nothing slow runs on an HTTP worker.** zte-agent has two. External IP
//!   lookups (5 s timeout each), AT commands (they sleep) and the neighbour scan
//!   all run on one-shot threads; the GET answers from the cache at once.
//! - **Demand-driven.** A refresh pass only starts because someone just asked,
//!   and at most every `PASS_MIN_GAP` s. With the screen off nobody asks, so
//!   nothing runs — no timer, no idle thread.
//! - **Lookups are rare.** Each exit is looked up again only when it changes
//!   (the default-route address for the direct one)
//!   or after `LOOKUP_MAX_AGE`; a failure waits `LOOKUP_RETRY` before the next.
//! - **"Automatic selection" is `AT+COPS=0`.** Not `nwinfo_set_netselect`:
//!   that takes `net_select` and is the radio-mode preference (Only_5G, …).
//!   The ubus API has no call for automatic operator selection.

use std::fs;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::at_cmd::{self, AtPort};
use crate::handlers::AppState;
use crate::ubus;

/// Every ubus call here: the modem daemon can stall while it searches, and a
/// stalled call must not hold a thread for the CLI's default 30 s.
fn ubus_call(object: &str, method: &str, params: Option<&str>) -> Result<Value, String> {
    ubus::call_with_timeout(object, method, params, Some(UBUS_TIMEOUT))
}

const CONF_PATH: &str = "/data/netinfo.conf";

/// Tried in order until one answers with something parse_geo understands, so
/// one service being down or blocked does not blank the screen (2026-09-25:
/// the user asked for several). Mainland services first for the cellular
/// exit. ip.skk.moe and ip.net.coffee were asked for too, but they are
/// browser pages that query from JavaScript; they have no endpoint a
/// program can read.
const DEFAULT_DIRECT_URLS: &[&str] = &[
    "https://myip.ipip.net/json",
    "https://cip.cc/",
    "https://ping0.cc/geo",
    "https://api.myip.la/cn?json",
    "http://ip-api.com/json/?lang=zh-CN&fields=status,query,country,regionName,city,isp,org",
];

const LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);
const LOOKUP_MAX_AGE: i64 = 600;
const LOOKUP_RETRY: i64 = 30;
const PASS_MIN_GAP: i64 = 10;
const SELECTION_MAX_AGE: i64 = 120;

const UBUS_TIMEOUT: u32 = 3;

const GUARD_POLL: Duration = Duration::from_secs(3);
/// Give up and go back to automatic this long after the register call. Worst
/// case to automatic: 45 + one poll (3) + `AT+COPS=0` (8) + read-back (2) = 58 s.
const GUARD_TIMEOUT: i64 = 45;
/// Written before a manual register, removed when the guard finishes. If the
/// agent restarts in between, `resume_guard` picks the job up again, so a
/// restart cannot leave the modem on a manual network with no data.
const GUARD_MARKER: &str = "/data/netinfo.guard";
/// Error strings end up on a 320 px screen and in fixed C buffers.
const ERR_MAX_CHARS: usize = 60;
/// A "fail" read this soon after the call can still be the previous attempt's.
const GUARD_FAIL_GRACE: i64 = 6;

const SCAN_POLL: Duration = Duration::from_secs(3);
const SCAN_TIMEOUT: i64 = 180;
const SCAN_MAX_OPS: usize = 12;

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn clip(s: String) -> String {
    match s.char_indices().nth(ERR_MAX_CHARS) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s,
    }
}

// ── config ─────────────────────────────────────────────────────────────────

/// `/data/netinfo.conf`, optional, `key=value` lines:
///   direct=<url>[,<url>…]   lookups for the cellular exit, tried in order
///   lookups=0               no external lookups at all
struct Config {
    direct: Vec<String>,
    enabled: bool,
}

impl Config {
    fn load() -> Self {
        Self::parse(&fs::read_to_string(CONF_PATH).unwrap_or_default())
    }

    fn parse(text: &str) -> Self {
        let mut c = Config {
            direct: DEFAULT_DIRECT_URLS.iter().map(|s| s.to_string()).collect(),
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
            let Some((k, v)) = line.trim().split_once('=') else { continue };
            match k.trim() {
                "direct" => c.direct = urls(v),
                "lookups" => c.enabled = v.trim() != "0",
                _ => {}
            }
        }
        c
    }
}

// ── IP lookups ─────────────────────────────────────────────────────────────

#[derive(Clone, Default, Debug, PartialEq)]
struct Geo {
    ip: String,
    geo: String,
    isp: String,
}

/// Join place names, dropping empties and any that repeat or are contained in
/// the previous one ("中国 北京 北京" → "中国 北京", "东京都 东京" → "东京都").
fn join_place(parts: &[&str]) -> String {
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

fn js<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn valid_ip(s: &str) -> bool {
    s.parse::<std::net::IpAddr>().is_ok()
}

/// Every lookup service we default to, by shape. Anything else that has an
/// `ip`/`query` field still yields the address.
fn parse_geo(body: &str) -> Option<Geo> {
    let body = body.trim();
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        // myip.ipip.net/json: {"ret":"ok","data":{"ip":…,"location":[国,省,市,"",运营商]}}
        if let Some(d) = v.get("data").filter(|d| d.get("location").is_some()) {
            let loc: Vec<&str> = d["location"].as_array()?.iter().map(|x| x.as_str().unwrap_or("")).collect();
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
            let isp = if js(&v, "isp").is_empty() { js(&v, "org") } else { js(&v, "isp") };
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
                Some((asn, rest)) if asn.starts_with("AS") && asn[2..].bytes().all(|b| b.is_ascii_digit()) => rest,
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
        return Some(Geo { ip: lines[0].to_string(), geo: join_place(&words), isp: isp.to_string() });
    }
    // cip.cc: "IP\t: 1.2.3.4" / "地址\t: 中国 上海 上海" / "运营商\t: 电信"
    if body.starts_with("IP\t") || body.starts_with("IP ") {
        let field = |k: &str| {
            body.lines()
                .find_map(|l| l.split_once(':').filter(|(key, _)| key.trim() == k).map(|(_, v)| v.trim()))
                .unwrap_or("")
        };
        let ip = field("IP");
        if !valid_ip(ip) {
            return None;
        }
        let words: Vec<&str> = field("地址").split_whitespace().collect();
        return Some(Geo { ip: ip.to_string(), geo: join_place(&words), isp: field("运营商").to_string() });
    }
    // myip.ipip.net plain text: "当前 IP：1.2.3.4  来自于：中国 广东 深圳  电信"
    let rest = body.split_once("IP：").or_else(|| body.split_once("IP:"))?.1;
    let (ip, loc) = match rest.split_once("来自于：") {
        Some((ip, loc)) => (ip.trim(), loc.trim()),
        None => (rest.trim(), ""),
    };
    if ip.is_empty() || !ip.bytes().all(|b| b.is_ascii_hexdigit() || b == b'.' || b == b':') {
        return None;
    }
    let words: Vec<&str> = loc.split_whitespace().collect();
    let (geo, isp) = match words.split_last() {
        Some((last, head)) if head.len() >= 2 => (join_place(head), last.to_string()),
        _ => (join_place(&words), String::new()),
    };
    Some(Geo { ip: ip.to_string(), geo, isp })
}

fn lookup(urls: &[String], proxy_port: Option<u16>) -> Result<(Geo, String), String> {
    let proxy = match proxy_port {
        Some(p) => Some(ureq::Proxy::new(&format!("http://127.0.0.1:{p}")).map_err(|e| e.to_string())?),
        None => None,
    };
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(LOOKUP_TIMEOUT))
        // The cellular side has IPv6 too; myip.ipip.net then answers with the
        // v6 address (measured 2026-09-25). The screen wants the v4 exit.
        .ip_family(ureq::config::IpFamily::Ipv4Only)
        .proxy(proxy)
        .build()
        .into();
    let mut last_err = String::from("没有可用的查询地址");
    for url in urls {
        let host = url.split("://").nth(1).and_then(|r| r.split('/').next()).unwrap_or(url).to_string();
        let text = agent
            .get(url)
            .header("User-Agent", "curl/8")
            .call()
            .map_err(|e| e.to_string())
            .and_then(|mut r| r.body_mut().read_to_string().map_err(|e| e.to_string()));
        match text {
            Ok(t) => match parse_geo(&t) {
                Some(g) => return Ok((g, host)),
                None => last_err = format!("{host}: 看不懂返回"),
            },
            Err(e) => last_err = format!("{host}: {e}"),
        }
    }
    Err(last_err)
}

/// Interface and IPv4 address of the main table's default route: changes when
/// the cellular session is re-established, and so (usually) does the public IP.
fn default_route_key() -> String {
    let route = fs::read_to_string("/proc/net/route").unwrap_or_default();
    let iface = route.lines().skip(1).find_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        (f.len() > 7 && f[1] == "00000000" && f[7] == "00000000").then(|| f[0].to_string())
    });
    let Some(iface) = iface else { return String::new() };
    format!("{iface} {}", ipv4_of(&iface).unwrap_or_default())
}

fn ipv4_of(iface: &str) -> Option<String> {
    let mut out = None;
    // SAFETY: getifaddrs/freeifaddrs pair; entries are only read in between.
    unsafe {
        let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut head) != 0 {
            return None;
        }
        let mut p = head;
        while !p.is_null() {
            let ifa = &*p;
            if !ifa.ifa_addr.is_null() && (*ifa.ifa_addr).sa_family as i32 == libc::AF_INET {
                let name = std::ffi::CStr::from_ptr(ifa.ifa_name).to_string_lossy();
                if name == iface {
                    let sin = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                    out = Some(std::net::Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr)).to_string());
                    break;
                }
            }
            p = ifa.ifa_next;
        }
        libc::freeifaddrs(head);
    }
    out
}

// ── operators ──────────────────────────────────────────────────────────────

/// MCCs whose networks use 3-digit MNCs (North America, parts of Latin America).
const MNC3_MCCS: &[u16] = &[
    302, 310, 311, 312, 313, 314, 315, 316, 334, 338, 342, 344, 346, 348, 350, 352, 354, 356, 358, 360, 365,
    366, 376, 708, 722, 732,
];

fn mnc_len(mcc: u16) -> usize {
    if MNC3_MCCS.contains(&mcc) {
        3
    } else {
        2
    }
}

pub(crate) fn country(mcc: u16) -> Option<&'static str> {
    Some(match mcc {
        202 => "希腊",
        204 => "荷兰",
        206 => "比利时",
        208 => "法国",
        214 => "西班牙",
        222 => "意大利",
        228 => "瑞士",
        232 => "奥地利",
        234 | 235 => "英国",
        238 => "丹麦",
        240 => "瑞典",
        242 => "挪威",
        244 => "芬兰",
        250 => "俄罗斯",
        262 => "德国",
        268 => "葡萄牙",
        272 => "爱尔兰",
        286 => "土耳其",
        302 => "加拿大",
        310..=316 => "美国",
        334 => "墨西哥",
        404 | 405 => "印度",
        420 => "沙特",
        424 => "阿联酋",
        425 => "以色列",
        427 => "卡塔尔",
        440 | 441 => "日本",
        450 => "韩国",
        452 => "越南",
        454 => "中国香港",
        455 => "中国澳门",
        456 => "柬埔寨",
        457 => "老挝",
        460 | 461 => "中国",
        466 => "中国台湾",
        502 => "马来西亚",
        505 => "澳大利亚",
        510 => "印度尼西亚",
        515 => "菲律宾",
        520 => "泰国",
        525 => "新加坡",
        530 => "新西兰",
        655 => "南非",
        722 => "阿根廷",
        724 => "巴西",
        730 => "智利",
        732 => "哥伦比亚",
        901 => "国际",
        _ => return None,
    })
}

/// ISO code → name, for lookup services that only give the code.
fn country_by_code(code: &str) -> Option<&'static str> {
    Some(match code {
        "CN" => "中国",
        "HK" => "中国香港",
        "MO" => "中国澳门",
        "TW" => "中国台湾",
        "JP" => "日本",
        "KR" => "韩国",
        "SG" => "新加坡",
        "US" => "美国",
        "CA" => "加拿大",
        "GB" => "英国",
        "DE" => "德国",
        "FR" => "法国",
        "NL" => "荷兰",
        "TH" => "泰国",
        "MY" => "马来西亚",
        "VN" => "越南",
        "PH" => "菲律宾",
        "ID" => "印度尼西亚",
        "AU" => "澳大利亚",
        "NZ" => "新西兰",
        "IN" => "印度",
        "AE" => "阿联酋",
        "RU" => "俄罗斯",
        _ => return None,
    })
}

fn operator(mcc: u16, mnc: u16) -> Option<&'static str> {
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
        (310, 160 | 200 | 210 | 220 | 230 | 240 | 250 | 260 | 270 | 310 | 490 | 660 | 800) => "T-Mobile",
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
struct Plmn {
    mcc: u16,
    mnc: u16,
    mnc_digits: usize,
}

/// Home PLMN from the IMSI; the MNC length depends on the country.
fn plmn_from_imsi(imsi: &str) -> Option<Plmn> {
    if imsi.len() < 6 || !imsi.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mcc: u16 = imsi[..3].parse().ok()?;
    let n = mnc_len(mcc);
    Some(Plmn { mcc, mnc: imsi[3..3 + n].parse().ok()?, mnc_digits: n })
}

/// A PLMN as the modem reports it ("46001", "310260"): the length says it all.
fn plmn_from_str(s: &str) -> Option<Plmn> {
    let s = s.trim();
    if !(5..=6).contains(&s.len()) || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(Plmn { mcc: s[..3].parse().ok()?, mnc: s[3..].parse().ok()?, mnc_digits: s.len() - 3 })
}

fn plmn_json(p: &Plmn, name_hint: &str) -> Value {
    let name = operator(p.mcc, p.mnc).map(str::to_string).or_else(|| (!name_hint.is_empty()).then(|| name_hint.to_string()));
    json!({
        "mcc": format!("{:03}", p.mcc),
        "mnc": format!("{:0w$}", p.mnc, w = p.mnc_digits),
        "name": name,
        "country": country(p.mcc),
    })
}

fn same_plmn(a: &str, b: &str) -> bool {
    match (plmn_from_str(a), plmn_from_str(b)) {
        (Some(x), Some(y)) => x.mcc == y.mcc && x.mnc == y.mnc,
        _ => false,
    }
}

// ── local modem state ──────────────────────────────────────────────────────

/// Fields of `nwinfo_get_netinfo` this module reads. Names follow what
/// zwrt-datad reads from the same call (state.rs); `rplmn_num` vs
/// `rmcc`+`rmnc` is still to be confirmed on the device.
fn serving_plmn(net: &Value) -> String {
    let direct = num_or_str(net, "rplmn_num");
    if plmn_from_str(&direct).is_some() {
        return direct;
    }
    // Measured 2026-09-25: `"rmcc": 460, "rmnc": 11` — numbers, so the MNC's
    // leading zero is gone and has to come back from the country's MNC length.
    let (mcc, mnc) = (num_or_str(net, "rmcc"), num_or_str(net, "rmnc"));
    let (Ok(m), Ok(n)) = (mcc.parse::<u16>(), mnc.parse::<u16>()) else { return String::new() };
    if m == 0 {
        return String::new();
    }
    let w = mnc.len().max(mnc_len(m));
    format!("{m:03}{n:0w$}")
}

/// A field that may come as a string or a number.
fn num_or_str(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// `net_select_mode`: "auto_select" / "manual_select" (measured). Read from
/// the same netinfo call, so the AT port is only needed to go back to auto.
fn selection_from_netinfo(net: &Value) -> Option<&'static str> {
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
/// contains "connected".
fn data_connected(wwan: &Value) -> bool {
    let s = js(wwan, "connect_status");
    s.ends_with("connected") && !s.contains("disconnect")
}

fn read_wwan() -> Value {
    ubus_call("zwrt_data", "get_wwaniface", Some(r#"{"source_module":"zte_topsw_data","cid":1}"#)).unwrap_or(Value::Null)
}

/// After anything that can drop the data call (operator search, manual
/// register, going back to automatic, the vendor neighbour scan): wait for it
/// to come back, and dial it again if it does not. Measured 2026-09-25:
/// `nwinfo_scan_nbr` dropped the call within 2 s and the modem never redialled
/// on its own (`roll_connect_status: init`, fail count 0) until
/// `set_qcliiface` was called — the WAN recovery in manager's CLAUDE.md.
/// Returns whether data is up at the end.
pub(crate) fn ensure_data_up() -> bool {
    // One redial at a time: a second caller waits for the first and then only
    // re-checks, instead of dialling again on top of it.
    static DIAL: Mutex<()> = Mutex::new(());
    let _one = DIAL.lock().unwrap_or_else(|e| e.into_inner());
    // "Up" = connected on two reads in a row: a call that is about to drop
    // (an operation just started) must not count as recovered.
    let wait = |secs: u64| {
        let mut streak = 0;
        for _ in 0..secs / 2 {
            if data_connected(&read_wwan()) {
                streak += 1;
                if streak >= 2 {
                    return true;
                }
            } else {
                streak = 0;
            }
            std::thread::sleep(Duration::from_secs(2));
        }
        false
    };
    if wait(20) {
        return true;
    }
    eprintln!("[netinfo] data call still down, dialling it again");
    for ty in [1, 2] {
        let arg = format!(r#"{{"source_module":"zte_topsw_data","type":{ty},"enable":1,"sub_id":1}}"#);
        let _ = ubus::call_with_timeout("zwrt_qcmap_cli", "set_qcliiface", Some(&arg), Some(5));
    }
    wait(30)
}

/// `+COPS: 0,0,"CHN-UNICOM",7` → "auto"; 1 → "manual"; 4 → manual with
/// automatic fallback, which for this page is still manual.
fn parse_cops_mode(resp: &str) -> Option<&'static str> {
    let rest = resp.split("+COPS:").nth(1)?.trim_start();
    match rest.split(|c: char| c == ',' || c.is_whitespace()).next()? {
        "0" => Some("auto"),
        "1" | "4" => Some("manual"),
        _ => None,
    }
}

// ── register guard ─────────────────────────────────────────────────────────

struct Obs<'a> {
    /// `nwinfo_m_netselect_result`'s answer; its values are unconfirmed.
    result: &'a str,
    serving: &'a str,
    connected: bool,
}

#[derive(Debug, PartialEq)]
enum Verdict {
    Wait,
    Ok,
    Revert(&'static str),
}

/// One observation → what to do. Success is judged by where the modem
/// actually is, not the result string: an unexpected "success" spelling must
/// not get a working registration undone, and a "success" while still on the
/// old network is not success.
fn decide(target: &str, o: &Obs, elapsed: i64) -> Verdict {
    if same_plmn(target, o.serving) && o.connected {
        return Verdict::Ok;
    }
    let r = o.result.trim().to_ascii_lowercase();
    if elapsed >= GUARD_FAIL_GRACE && matches!(r.as_str(), "fail" | "failed" | "failure" | "0" | "error") {
        return Verdict::Revert("注册失败");
    }
    if elapsed >= GUARD_TIMEOUT {
        return Verdict::Revert(if same_plmn(target, o.serving) { "已注册但数据没通" } else { "超时没注册上" });
    }
    Verdict::Wait
}

fn register_result_str(v: &Value) -> String {
    for k in ["result", "m_netselect_result", "netselect_result", "m_result"] {
        match v.get(k) {
            Some(Value::String(s)) => return s.clone(),
            Some(Value::Number(n)) => return n.to_string(),
            _ => {}
        }
    }
    String::new()
}

#[derive(Clone, Default)]
struct Guard {
    /// idle | registering | ok | reverting | reverted | revert_failed
    phase: &'static str,
    target: String,
    rat: String,
    reason: String,
    started_at: i64,
    finished_at: i64,
    /// Last raw register result, kept for the device probe.
    last_result: String,
}

impl Guard {
    fn json(&self) -> Value {
        json!({
            "phase": if self.phase.is_empty() { "idle" } else { self.phase },
            "target": self.target,
            "rat": self.rat,
            "reason": self.reason,
            "started_at": self.started_at,
            "finished_at": self.finished_at,
            "last_result": self.last_result,
        })
    }

}

// ── neighbours ─────────────────────────────────────────────────────────────

#[derive(Default)]
struct Nbr {
    /// idle | scanning | done | error
    state: &'static str,
    scanned_at: i64,
    error: String,
    cells: Vec<Value>,
}

fn field<'a>(o: &'a serde_json::Map<String, Value>, keys: &[&str]) -> Option<&'a Value> {
    o.iter().find(|(k, _)| {
        let k = k.to_ascii_lowercase();
        keys.iter().any(|want| k.contains(want))
    })
    .map(|(_, v)| v)
    .filter(|v| !v.is_null() && v.as_str() != Some(""))
}

/// Walk whatever the neighbour call returns and keep anything that looks like
/// a cell (has a PCI). Field names are unconfirmed, so match loosely.
// Kept for a neighbour source that works (the vendor scan does not, see neighbors_scan).
#[allow(dead_code)]
fn collect_cells(v: &Value, rat: &str, out: &mut Vec<Value>) {
    match v {
        Value::String(s) => s.split(';').filter(|r| r.contains(',')).for_each(|r| {
            if let Some(c) = cell_from_record(r, rat) {
                out.push(c);
            }
        }),
        Value::Array(a) => a.iter().for_each(|x| collect_cells(x, rat, out)),
        Value::Object(o) => {
            if let Some(pci) = field(o, &["pci"]) {
                out.push(json!({
                    "rat": rat,
                    "pci": pci,
                    "arfcn": field(o, &["arfcn", "earfcn", "channel", "freq"]),
                    "rsrp": field(o, &["rsrp"]),
                    "rsrq": field(o, &["rsrq"]),
                    "sinr": field(o, &["sinr", "snr"]),
                }));
            } else {
                o.values().for_each(|x| collect_cells(x, rat, out));
            }
        }
        _ => {}
    }
}

// Kept for a neighbour source that works (the vendor scan does not, see neighbors_scan).
#[allow(dead_code)]
fn rsrp_of(c: &Value) -> f64 {
    match &c["rsrp"] {
        Value::Number(n) => n.as_f64().unwrap_or(-999.0),
        Value::String(s) => s.trim().parse().unwrap_or(-999.0),
        _ => -999.0,
    }
}

// ── per-client traffic ─────────────────────────────────────────────────────
//
// `router_get_clients_traffic` fails on this firmware (probe 2026-09-25), so
// the numbers come from the AP side: `iw dev <ap> station dump` per Wi-Fi
// interface, names from dnsmasq's lease file. Byte counters are cumulative
// since the station associated; the rate is the difference between two
// refresh passes. The AP's "tx" is what the client downloaded.

const LEASES: &str = "/tmp/dhcp.leases";
const CLIENTS_MAX: usize = 12;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Station {
    pub mac: String,
    pub iface: String,
    down: u64,
    up: u64,
    signal: Option<i32>,
    connected_secs: u64,
    /// From the AP interface: "2.4 GHz" | "5 GHz" | "6 GHz" | "" (written
    /// as GHz so it is never read as the cellular 5G).
    band: &'static str,
    channel: Option<u32>,
    width_mhz: Option<u32>,
    /// From the station's tx bitrate: EHT = Wi-Fi 7, HE = 6, VHT = 5, HT = 4.
    wifi_gen: Option<u8>,
    /// Negotiated link rates in Mbit/s (AP tx = client download).
    link_down_mbps: Option<u32>,
    link_up_mbps: Option<u32>,
}

/// `iw dev <ap> info`: "channel 44 (5220 MHz), width: 160 MHz, center1: …".
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct ApInfo {
    band: &'static str,
    channel: Option<u32>,
    width_mhz: Option<u32>,
}

fn parse_ap_info(text: &str) -> ApInfo {
    let mut out = ApInfo::default();
    let Some(l) = text.lines().map(str::trim).find(|l| l.starts_with("channel ")) else { return out };
    out.channel = l["channel ".len()..].split_whitespace().next().and_then(|c| c.parse().ok());
    let freq: Option<u32> = l.split_once('(').and_then(|(_, r)| r.split_whitespace().next()).and_then(|f| f.parse().ok());
    out.band = match freq {
        Some(f) if (2400..2500).contains(&f) => "2.4 GHz",
        Some(f) if (5150..5900).contains(&f) => "5 GHz",
        Some(f) if (5925..7125).contains(&f) => "6 GHz",
        _ => "",
    };
    out.width_mhz = l.split_once("width:").and_then(|(_, r)| r.split_whitespace().next()).and_then(|w| w.parse().ok());
    out
}

/// "2401.9 MBit/s 160MHz HE-MCS 11 …" → (2402, Some(6)).
fn parse_bitrate(v: &str) -> (Option<u32>, Option<u8>) {
    let mbps = v.split_whitespace().next().and_then(|n| n.parse::<f64>().ok()).map(|m| m.round() as u32);
    let gen = if v.contains("EHT-") {
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
    (mbps, gen)
}

/// AP interfaces from `iw dev`: "Interface wlan0" … "type AP".
fn ap_ifaces(iw_dev: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    for l in iw_dev.lines().map(str::trim) {
        if let Some(n) = l.strip_prefix("Interface ") {
            cur = Some(n.trim().to_string());
        } else if l == "type AP" {
            if let Some(n) = cur.take() {
                out.push(n);
            }
        }
    }
    out
}

fn parse_station_dump(text: &str, iface: &str) -> Vec<Station> {
    let mut out: Vec<Station> = Vec::new();
    for l in text.lines() {
        let t = l.trim();
        if let Some(rest) = t.strip_prefix("Station ") {
            let mac = rest.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
            out.push(Station { mac, iface: iface.to_string(), ..Station::default() });
            continue;
        }
        let Some(st) = out.last_mut() else { continue };
        let Some((k, v)) = t.split_once(':') else { continue };
        let first = v.split_whitespace().next().unwrap_or("");
        match k.trim() {
            "rx bytes" => st.up = first.parse().unwrap_or(0),
            "tx bytes" => st.down = first.parse().unwrap_or(0),
            "signal" => st.signal = first.parse().ok(),
            "connected time" => st.connected_secs = first.parse().unwrap_or(0),
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
fn parse_leases(text: &str) -> Vec<(String, String, String)> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            (f.len() >= 4).then(|| {
                let name = if f[3] == "*" { String::new() } else { f[3].to_string() };
                (f[1].to_ascii_lowercase(), f[2].to_string(), name)
            })
        })
        .collect()
}

fn sh_out(cmd: &str) -> String {
    std::process::Command::new("sh")
        .args(["-c", cmd])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Rate in bytes/s from two cumulative readings; a counter that went down
/// (the station re-associated) gives no rate rather than a huge one.
fn rate(prev: Option<(u64, i64)>, now_bytes: u64, t: i64) -> Option<u64> {
    let (b, t0) = prev?;
    (now_bytes >= b && t > t0).then(|| (now_bytes - b) / (t - t0) as u64)
}

/// Every station on every AP interface, with the band of the interface it is on.
pub fn wifi_stations() -> Vec<Station> {
    let mut stations = Vec::new();
    for iface in ap_ifaces(&sh_out("iw dev 2>/dev/null")) {
        let ap = parse_ap_info(&sh_out(&format!("iw dev {iface} info 2>/dev/null")));
        for mut st in parse_station_dump(&sh_out(&format!("iw dev {iface} station dump 2>/dev/null")), &iface) {
            st.band = ap.band;
            st.channel = ap.channel;
            st.width_mhz = ap.width_mhz;
            stations.push(st);
        }
    }
    stations
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
        "connected_secs": st.connected_secs,
    })
}

fn clients_snapshot(prev: &std::collections::HashMap<String, (u64, u64, i64)>, t: i64)
    -> (Vec<Value>, std::collections::HashMap<String, (u64, u64, i64)>)
{
    let leases = parse_leases(&fs::read_to_string(LEASES).unwrap_or_default());
    let mut stations = wifi_stations();
    stations.sort_by(|a, b| (b.down + b.up).cmp(&(a.down + a.up)));
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
            o.insert("name".into(), json!(lease.map(|l| l.2.clone()).filter(|n| !n.is_empty())));
            o.insert("ip".into(), json!(lease.map(|l| l.1.clone())));
            o.insert("down_bytes".into(), json!(st.down));
            o.insert("up_bytes".into(), json!(st.up));
            o.insert("down_rate".into(), json!(rate(p.map(|p| (p.0, p.2)), st.down, t)));
            o.insert("up_rate".into(), json!(rate(p.map(|p| (p.1, p.2)), st.up, t)));
            v
        })
        .collect();
    (list, next)
}

// ── operator search ────────────────────────────────────────────────────────

#[derive(Default)]
struct Scan {
    /// idle | scanning | done | error
    state: &'static str,
    started_at: i64,
    finished_at: i64,
    error: String,
    operators: Vec<Value>,
    /// Last raw status, kept for the device probe.
    last_status: String,
}

// ── one modem operation at a time ──────────────────────────────────────────
//
// Search, manual register (with its guard) and back-to-automatic all change
// what the modem is doing; any two overlapping is a mess (a register during a
// 110 s search, a revert racing a register). One lock does check-and-claim for
// all of them, old routes included (/api/modem/scan comes here too).

#[derive(Clone, Copy, PartialEq, Debug, Default)]
enum OpKind {
    #[default]
    Idle,
    Scanning,
    Registering,
    Reverting,
}

#[derive(Default)]
struct Ops {
    kind: OpKind,
    guard: Guard,
    scan: Scan,
    /// "Back to automatic" asked for while a register is being guarded. Read
    /// and cleared under this same lock, so an accepted request is never lost.
    cancel: bool,
}

/// Claim the modem for `want`, or say what is in the way.
fn claim(ops: &mut Ops, want: OpKind) -> Result<(), &'static str> {
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

/// What the marker file says is in flight. Written before the modem is
/// touched, removed only once the outcome is confirmed, so a restart
/// (resume_guard) always knows what to finish.
#[derive(Debug, PartialEq)]
enum Marker {
    Register { target: String, started: i64 },
    Revert,
    /// Automatic again; only the data call is still to come back.
    Redial,
}

fn parse_marker(text: &str) -> Option<Marker> {
    let f: Vec<&str> = text.split_whitespace().collect();
    match f.as_slice() {
        ["revert", ..] => Some(Marker::Revert),
        ["redial", ..] => Some(Marker::Redial),
        ["register", t, s] | [t, s] if plmn_from_str(t).is_some() => {
            Some(Marker::Register { target: t.to_string(), started: s.parse().ok()? })
        }
        _ => None,
    }
}

fn write_marker(m: &Marker) -> std::io::Result<()> {
    let text = match m {
        Marker::Register { target, started } => format!("register {target} {started}\n"),
        Marker::Revert => format!("revert {}\n", now()),
        Marker::Redial => format!("redial {}\n", now()),
    };
    let tmp = format!("{GUARD_MARKER}.tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, GUARD_MARKER)
}

impl Scan {
    fn json(&self) -> Value {
        json!({
            "state": if self.state.is_empty() { "idle" } else { self.state },
            "started_at": self.started_at,
            "finished_at": self.finished_at,
            "error": (!self.error.is_empty()).then(|| self.error.clone()),
            "last_status": self.last_status,
            "operators": self.operators,
        })
    }
}

/// First string (or number) among keys containing any of `keys`.
fn loose_str(v: &Value, keys: &[&str]) -> String {
    let Some(o) = v.as_object() else { return String::new() };
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
fn scan_finished(status: &str) -> Option<bool> {
    let s = status.trim().to_ascii_lowercase();
    if s.contains("fail") || s.contains("error") || s == "3" {
        return Some(false);
    }
    if s.contains("selected") || ["done", "complete", "completed", "finish", "finished", "success", "2"].contains(&s.as_str()) {
        return Some(true);
    }
    None
}

/// Anything with an `m_mcc_mnc` in `nwinfo_m_netselect_contents`, named from
/// our own table when we know the PLMN. `m_rat` is passed back verbatim.
fn collect_operators(v: &Value, out: &mut Vec<Value>) {
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
                out.push(json!({
                    "plmn": plmn,
                    "name": p.as_ref().and_then(|p| operator(p.mcc, p.mnc)).map(str::to_string).unwrap_or(raw_name.clone()),
                    "raw_name": raw_name,
                    "country": p.as_ref().and_then(|p| country(p.mcc)),
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

fn is_small_int(f: &str) -> bool {
    !f.is_empty() && f.len() <= 2 && f.bytes().all(|b| b.is_ascii_digit())
}

/// One `nwinfo_m_netselect_contents` record. Measured 2026-09-25:
/// "status,name,plmn,rat;" e.g. "2,CT,46011,11;3,CMCC,46000,7;" — status
/// 1 available / 2 current / 3 forbidden, rat 2 UTRAN / 7 E-UTRAN / 11 NR.
/// Fields are still picked by shape (PLMN = the 5–6 digit one), so a firmware
/// that reorders them keeps working.
fn operator_from_record(r: &str) -> Option<Value> {
    let f: Vec<&str> = r.split(',').map(|x| x.trim().trim_matches('"')).collect();
    let pi = f.iter().position(|x| plmn_from_str(x).is_some())?;
    let plmn = f[pi];
    let raw_name = f.iter().find(|x| x.bytes().any(|b| b.is_ascii_alphabetic())).copied().unwrap_or("");
    let status = f[..pi].iter().rev().find(|x| is_small_int(x)).copied().unwrap_or("");
    let rat = f[pi + 1..].iter().find(|x| is_small_int(x)).copied().unwrap_or("");
    let p = plmn_from_str(plmn)?;
    Some(json!({
        "plmn": plmn,
        "name": operator(p.mcc, p.mnc).map(str::to_string).unwrap_or_else(|| raw_name.to_string()),
        "raw_name": raw_name,
        "country": country(p.mcc),
        "rat": rat,
        "status": status,
        "raw": r,
    }))
}

/// One neighbour record from `nwinfo_get_*_nbr_contents` (a string, measured
/// empty when idle). Field order unknown: PCI is the first whole number up to
/// 1007, ARFCN the first bigger one, RSRP the first value in −140…−40, then
/// RSRQ and SINR follow it. The raw record rides along for checking.
// Kept for a neighbour source that works (the vendor scan does not, see neighbors_scan).
#[allow(dead_code)]
fn cell_from_record(r: &str, rat: &str) -> Option<Value> {
    let f: Vec<&str> = r.split(',').map(str::trim).collect();
    let nums: Vec<Option<f64>> = f.iter().map(|x| x.parse::<f64>().ok()).collect();
    let is_int = |i: usize| nums[i].is_some_and(|v| v.fract() == 0.0 && v >= 0.0);
    let pci = (0..f.len()).find(|&i| is_int(i) && nums[i].unwrap() <= 1007.0)?;
    let arfcn = (0..f.len()).find(|&i| i != pci && is_int(i) && nums[i].unwrap() > 1007.0);
    let rsrp = (0..f.len()).find(|&i| nums[i].is_some_and(|v| (-140.0..=-40.0).contains(&v)));
    let after = |i: Option<usize>| i.and_then(|i| (i + 1 < f.len()).then_some(i + 1));
    let rsrq = after(rsrp).filter(|&i| nums[i].is_some_and(|v| (-40.0..0.0).contains(&v)));
    let sinr = after(rsrq).filter(|&i| nums[i].is_some());
    let s = |i: Option<usize>| i.map(|i| f[i]);
    Some(json!({
        "rat": rat, "pci": f[pci], "arfcn": s(arfcn), "rsrp": s(rsrp), "rsrq": s(rsrq), "sinr": s(sinr), "raw": r,
    }))
}

// ── the module ─────────────────────────────────────────────────────────────

#[derive(Clone, Default)]
struct Lookup {
    geo: Geo,
    source: String,
    node: Option<String>,
    fetched_at: i64,
    error: Option<String>,
}

impl Lookup {
    fn json(&self) -> Value {
        let s = |v: &str| (!v.is_empty()).then(|| v.to_string());
        json!({
            "ip": s(&self.geo.ip),
            "geo": s(&self.geo.geo),
            "isp": s(&self.geo.isp),
            "node": self.node,
            "source": s(&self.source),
            "fetched_at": self.fetched_at,
            "error": self.error,
        })
    }
}

#[derive(Default)]
struct Slot {
    value: Option<Lookup>,
    key: String,
    retry_at: i64,
}

impl Slot {
    fn due(&self, key: &str, t: i64) -> bool {
        if key != self.key {
            return true;
        }
        match &self.value {
            None => t >= self.retry_at,
            Some(l) if l.error.is_some() => t >= self.retry_at,
            Some(l) => t - l.fetched_at >= LOOKUP_MAX_AGE,
        }
    }

    fn store(&mut self, key: &str, t: i64, res: Result<(Geo, String), String>, node: Option<String>) {
        let same = key == self.key;
        self.key = key.to_string();
        match res {
            Ok((geo, source)) => {
                self.value = Some(Lookup { geo, source, node, fetched_at: t, error: None });
            }
            Err(e) => {
                self.retry_at = t + LOOKUP_RETRY;
                // Keep the last good answer only while the exit is unchanged.
                let mut l = if same { self.value.take().unwrap_or_default() } else { Lookup::default() };
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

#[derive(Default)]
struct Cache {
    direct: Slot,
    selection: Option<&'static str>,
    selection_at: i64,
    /// The netinfo call reports it: no AT reads needed.
    selection_from_netinfo: bool,
    clients: Vec<Value>,
    clients_prev: std::collections::HashMap<String, (u64, u64, i64)>,
    clients_at: i64,
    local: Value,
}

struct Inner {
    cache: Mutex<Cache>,
    refreshing: AtomicBool,
    last_pass: AtomicI64,
    /// Last non-lite GET: the selection mode is only read while someone
    /// is looking at the full page.
    last_full: AtomicI64,
    /// Last GET that wants per-client traffic (full, or lite with clients=1).
    last_clients: AtomicI64,
    at: AtPort,
    ops: Mutex<Ops>,
    nbr: Mutex<Nbr>,
    /// The scenario picker, refreshed by the pass: GET uses it when the
    /// engine's own locks are busy (it does uci I/O under them).
    scenes: Mutex<Value>,
    scenario: Mutex<Option<Arc<crate::scenario::Engine>>>,
}

pub struct NetInfo {
    inner: Arc<Inner>,
}

impl NetInfo {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                cache: Mutex::new(Cache::default()),
                refreshing: AtomicBool::new(false),
                last_pass: AtomicI64::new(0),
                last_full: AtomicI64::new(0),
                last_clients: AtomicI64::new(0),
                at: AtPort::new(),
                ops: Mutex::new(Ops::default()),
                nbr: Mutex::new(Nbr::default()),
                scenes: Mutex::new(Value::Null),
                scenario: Mutex::new(None),
            }),
        }
    }
}

/// Serving network, SIM and data state: three local ubus calls. Runs in the
/// refresh pass, never on an HTTP worker.
fn local_snapshot(inner: &Inner) {
    let net = ubus_call("zte_nwinfo_api", "nwinfo_get_netinfo", Some("{}")).unwrap_or(Value::Null);
    let sim = ubus_call("zwrt_zte_mdm.api", "get_sim_info", Some("{}")).unwrap_or(Value::Null);
    let wwan = read_wwan();

    let imsi = js(&sim, "sim_imsi");
    let sim_ready = js(&sim, "sim_states").contains("ready");
    let home = sim_ready.then(|| plmn_from_imsi(imsi)).flatten().map(|p| plmn_json(&p, ""));

    let serving_str = serving_plmn(&net);
    let provider = {
        let f = js(&net, "network_provider_fullname");
        if f.is_empty() { js(&net, "network_provider") } else { f }
    };
    let serving = match plmn_from_str(&serving_str) {
        Some(p) => plmn_json(&p, provider),
        None if !provider.is_empty() => json!({"name": provider, "mcc": null, "mnc": null, "country": null}),
        None => Value::Null,
    };
    let roam_raw = js(&net, "simcard_roam");
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
        "network_type": js(&net, "network_type"),
        "data_connected": data_connected(&wwan),
    });
    if let Some(sel) = selection_from_netinfo(&net) {
        let mut c = inner.cache.lock().unwrap();
        c.selection = Some(sel);
        c.selection_at = now();
        c.selection_from_netinfo = true;
    }
    inner.cache.lock().unwrap().local = snap;
}

/// One refresh pass on its own thread: IP lookups that are due, and the
/// selection mode if stale.
fn refresh_pass(inner: Arc<Inner>) {
    let t = now();
    let cfg = Config::load();
    local_snapshot(&inner);

    if cfg.enabled {
        let dkey = default_route_key();
        let due = !dkey.is_empty() && inner.cache.lock().unwrap().direct.due(&dkey, t);
        if due {
            let res = lookup(&cfg.direct, None);
            inner.cache.lock().unwrap().direct.store(&dkey, t, res, None);
        }

    }

    // Per-client traffic only while a page that shows it is open (it spawns iw).
    let clients_wanted = t - inner.last_full.load(Ordering::SeqCst).max(inner.last_clients.load(Ordering::SeqCst)) < 60;
    if clients_wanted {
        let prev = inner.cache.lock().unwrap().clients_prev.clone();
        let (list, next) = clients_snapshot(&prev, t);
        let mut c = inner.cache.lock().unwrap();
        c.clients = list;
        c.clients_prev = next;
        c.clients_at = t;
    }

    let engine = inner.scenario.lock().unwrap().clone();
    if let Some(p) = engine.and_then(|e| crate::scenario::picker_try(&e)) {
        *inner.scenes.lock().unwrap() = p;
    }

    let stale = {
        let c = inner.cache.lock().unwrap();
        !c.selection_from_netinfo && t - c.selection_at >= SELECTION_MAX_AGE
    };
    let wanted = t - inner.last_full.load(Ordering::SeqCst) < 60;
    let idle = inner.ops.lock().unwrap().kind == OpKind::Idle;
    if stale && wanted && idle {
        read_selection(&inner);
    }
    inner.refreshing.store(false, Ordering::SeqCst);
}

fn read_selection(inner: &Inner) {
    let mode = at_cmd::send(&inner.at, "AT+COPS?", 2).ok().and_then(|r| parse_cops_mode(&r));
    let mut c = inner.cache.lock().unwrap();
    c.selection = mode;
    c.selection_at = now();
}

/// GET /api/netinfo[?lite=1[&apn=1][&clients=1]]
///
/// `lite` is what the touch screen reads everywhere except 运营商选择: it must
/// not keep the AT-command selection read (AT+COPS?) going. A lite read can
/// still ask for the APN block (`apn=1`, 蜂窝 tab / APN page: four ubus
/// reads) or keep the per-client traffic fresh (`clients=1`, Wi-Fi tab: iw
/// per pass). A plain GET is the full read (admin web, 运营商选择).
pub fn get(state: &AppState, query: &str) -> (u16, Value) {
    let inner = &state.netinfo.inner;
    let t = now();
    let has = |k: &str| query.split('&').any(|kv| kv == k);
    let lite = has("lite=1");
    if !lite {
        inner.last_full.store(t, Ordering::SeqCst);
    }
    if has("clients=1") {
        inner.last_clients.store(t, Ordering::SeqCst);
    }
    if t - inner.last_pass.load(Ordering::SeqCst) >= PASS_MIN_GAP && !inner.refreshing.swap(true, Ordering::SeqCst) {
        inner.last_pass.store(t, Ordering::SeqCst);
        let i = Arc::clone(inner);
        std::thread::spawn(move || refresh_pass(i));
    }
    // Each lock is taken on its own and released before the next: nothing
    // here waits on another lock while holding one.
    if inner.scenario.lock().unwrap().is_none() {
        *inner.scenario.lock().unwrap() = Some(Arc::clone(&state.scenario));
    }
    let scenes = crate::scenario::picker_try(&state.scenario)
        .unwrap_or_else(|| inner.scenes.lock().unwrap().clone());
    // Four quick ubus reads; the lite (home card) poll does not need them.
    let apn = if lite && !has("apn=1") { Value::Null } else { apn_read() };
    let (guard, scan) = {
        let o = inner.ops.lock().unwrap();
        (o.guard.json(), o.scan.json())
    };
    let (nbr_at, nbr_cells) = {
        let n = inner.nbr.lock().unwrap();
        (n.scanned_at, n.cells.clone())
    };
    let c = inner.cache.lock().unwrap();
    let local = c.local.clone();
    let mut data = json!({
        "now": t,
        "direct": c.direct.value.as_ref().map(Lookup::json),
        "selection": {"mode": c.selection, "checked_at": c.selection_at},
        "guard": guard,
        "neighbors": {
            "state": "unsupported",
            "scanned_at": nbr_at,
            "error": NBR_UNSUPPORTED,
            "cells": nbr_cells,
        },
        "scan": scan,
        "clients": {"at": c.clients_at, "list": c.clients},
        "scenes": scenes,
        "apn": apn,
    });
    drop(c);
    if let (Some(d), Some(l)) = (data.as_object_mut(), local.as_object()) {
        for (k, v) in l {
            d.insert(k.clone(), v.clone());
        }
    }
    (200, json!({"ok": true, "data": data}))
}

// ---- APN: which one is dialling, and switching between existing ones -------
//
// The touch screen shows the APN in use and switches between the automatic
// choice and the manual profiles already saved (creating or editing one needs
// typing: the admin web does that). Read 2026-09-25 on the owner's device:
// `get_apn_at_cid {cid:1}` is the profile the data call dialled
// (`profileId` "auto109590" in auto mode). The manual list's `isEnable` only
// marks the profile manual mode would use, so it is not "in use" while
// `apn_mode` is 0.

fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_i64() == Some(1),
        Value::String(s) => s == "1" || s == "true",
        _ => false,
    }
}

fn apn_entry(p: &Value, in_use: &str) -> Value {
    let id = p["profileId"].as_str().unwrap_or("");
    json!({
        "id": id,
        "name": p["profilename"].as_str().unwrap_or(""),
        "apn": p["wanapn"].as_str().unwrap_or(""),
        "pdp": p["pdpType"].as_i64().unwrap_or(0),
        "selected": truthy(&p["isEnable"]),
        "in_use": !id.is_empty() && id == in_use,
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
        v["apnListArray"].as_array().map(|a| a.iter().map(|p| apn_entry(p, in_use)).collect()).unwrap_or_default()
    };
    json!({
        "mode": if mode == 1 { "manual" } else { "auto" },
        "in_use": if in_use.is_empty() { Value::Null } else { apn_entry(dialled, in_use) },
        "auto": list(auto),
        "manual": list(manual),
    })
}

fn apn_read() -> Value {
    let call = |m: &str, a: &str| ubus::call("zwrt_apn_object", m, Some(a));
    match call("get_apn_mode", "{}") {
        Err(e) => json!({"error": e}),
        Ok(mode) => apn_view(
            &mode,
            &call("get_apn_at_cid", "{\"cid\":1}").unwrap_or(Value::Null),
            &call("get_auto_apn_list", "{}").unwrap_or(Value::Null),
            &call("get_manu_apn_list", "{}").unwrap_or(Value::Null),
        ),
    }
}

/// POST /api/netinfo/apn — {"id":"auto"} or {"id":"<manual profileId>"}.
/// Manual: select the profile first, then switch the mode, so the re-dial
/// uses it. The data call drops for a moment either way (the caller says so
/// before the second tap).
pub fn apn_use(body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let id = parsed["id"].as_str().unwrap_or("");
    if id.is_empty() {
        return (400, json!({"ok": false, "error": "missing 'id'"}));
    }
    let call = |m: &str, a: String| ubus::call("zwrt_apn_object", m, Some(&a));
    if id != "auto" {
        let known = call("get_manu_apn_list", "{}".into())
            .ok()
            .and_then(|v| v["apnListArray"].as_array().map(|a| a.iter().any(|p| p["profileId"] == id)))
            .unwrap_or(false);
        if !known {
            return (404, json!({"ok": false, "error": "没有这个手动 APN"}));
        }
        if let Err(e) = call("enable_manu_apn_id", json!({"profileId": id}).to_string()) {
            return (503, json!({"ok": false, "error": e}));
        }
    }
    let mode = if id == "auto" { 0 } else { 1 };
    match call("set_apn_mode", json!({"apn_mode": mode}).to_string()) {
        Ok(_) => (200, json!({"ok": true, "data": apn_read()})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

/// POST /api/modem/register — body {"m_mcc_mnc":"46001","m_rat":"…"}
///
/// Answers at once (202). The marker is written first — refused if it cannot
/// be — and the ubus call and the guard run on their own thread: once the
/// request may have reached the modem it is guarded to the end, whatever the
/// call itself returned (a timeout does not prove it was not executed).
pub fn register(state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let target = js(&parsed, "m_mcc_mnc").to_string();
    let rat = match &parsed["m_rat"] {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    };
    if plmn_from_str(&target).is_none() {
        return (400, json!({"ok": false, "error": "m_mcc_mnc must be 5–6 digits"}));
    }
    let inner = Arc::clone(&state.netinfo.inner);
    let started = now();
    let snapshot = {
        let mut o = inner.ops.lock().unwrap();
        if let Err(why) = claim(&mut o, OpKind::Registering) {
            return (409, json!({"ok": false, "error": why, "guard": o.guard.json()}));
        }
        if let Err(e) = write_marker(&Marker::Register { target: target.clone(), started }) {
            o.kind = OpKind::Idle;
            return (500, json!({"ok": false, "error": format!("写不了保护标记，没有注册：{e}")}));
        }
        o.cancel = false;
        o.guard = Guard { phase: "registering", target: target.clone(), rat: rat.clone(), started_at: started, ..Guard::default() };
        o.guard.json()
    };
    let i = Arc::clone(&inner);
    std::thread::spawn(move || {
        let arg = json!({"m_mcc_mnc": target, "m_rat": rat}).to_string();
        if let Err(e) = ubus_call("zte_nwinfo_api", "nwinfo_manual_register", Some(&arg)) {
            eprintln!("[netinfo] manual register call: {e} (guarding anyway)");
            i.ops.lock().unwrap().guard.last_result = clip(format!("调用出错：{e}"));
        }
        guard_run(i, target, started);
    });
    (202, json!({"ok": true, "guard": snapshot}))
}

/// Watch a manual register until the modem is on the target with data (keep
/// it), or it clearly is not going to be (back to automatic). A "back to
/// automatic" request is honoured at every step, including the last.
fn guard_run(inner: Arc<Inner>, target: String, started: i64) {
    loop {
        std::thread::sleep(GUARD_POLL);
        if std::mem::take(&mut inner.ops.lock().unwrap().cancel) {
            return revert_until_auto(&inner, "手动恢复自动");
        }
        let res = ubus_call("zte_nwinfo_api", "nwinfo_m_netselect_result", Some("{}")).unwrap_or(Value::Null);
        let result = register_result_str(&res);
        let net = ubus_call("zte_nwinfo_api", "nwinfo_get_netinfo", Some("{}")).unwrap_or(Value::Null);
        let serving = serving_plmn(&net);
        let connected = data_connected(&read_wwan());
        if !result.is_empty() {
            inner.ops.lock().unwrap().guard.last_result = result.clone();
        }
        match decide(&target, &Obs { result: &result, serving: &serving, connected }, now() - started) {
            Verdict::Wait => {}
            Verdict::Revert(why) => return revert_until_auto(&inner, why),
            Verdict::Ok => {
                // Keep it only if the data call holds.
                if !ensure_data_up() {
                    return revert_until_auto(&inner, "已注册但数据没通");
                }
                let mut o = inner.ops.lock().unwrap();
                if std::mem::take(&mut o.cancel) {
                    drop(o);
                    return revert_until_auto(&inner, "手动恢复自动");
                }
                let _ = fs::remove_file(GUARD_MARKER);
                o.guard.phase = "ok";
                o.guard.finished_at = now();
                o.kind = OpKind::Idle;
                drop(o);
                let mut c = inner.cache.lock().unwrap();
                c.selection = Some("manual");
                c.selection_at = now();
                inner.last_pass.store(0, Ordering::SeqCst);
                return;
            }
        }
    }
}

/// Selection mode as the netinfo call reports it (no AT port needed).
fn selection_now() -> Option<&'static str> {
    selection_from_netinfo(&ubus_call("zte_nwinfo_api", "nwinfo_get_netinfo", Some("{}")).unwrap_or(Value::Null))
}

/// `AT+COPS=0` until the modem reports automatic selection, then get the
/// data call back. Measured: automatic again ≈30 s after the command, data
/// ≈3 s later. Does not give up: every miss backs off (30 s … 5 min) and
/// tries again, and the marker stays until it is confirmed, so a restart
/// picks it up too.
fn revert_until_auto(inner: &Inner, why: &'static str) {
    {
        let mut o = inner.ops.lock().unwrap();
        o.kind = OpKind::Reverting;
        o.cancel = false;
        o.guard.phase = "reverting";
        o.guard.reason = why.to_string();
    }
    if let Err(e) = write_marker(&Marker::Revert) {
        eprintln!("[netinfo] revert marker: {e}");
    }
    let mut backoff = 30u64;
    // Already automatic (the manual register never took, or someone pressed
    // the button twice): no command, which would only make it re-register.
    let already = selection_now() == Some("auto");
    for attempt in 1u32.. {
        if already {
            break;
        }
        let sent = at_cmd::send(&inner.at, "AT+COPS=0", 8);
        let mut auto = false;
        for _ in 0..20 {
            if selection_now() == Some("auto") {
                auto = true;
                break;
            }
            std::thread::sleep(Duration::from_secs(3));
        }
        if auto {
            break;
        }
        let err = sent.err().unwrap_or_else(|| "模组还没回到自动".into());
        eprintln!("[netinfo] back-to-automatic attempt {attempt} failed: {err}");
        inner.ops.lock().unwrap().guard.reason = clip(format!("{why}；第 {attempt} 次恢复自动没成功（{err}），{backoff} 秒后再试"));
        std::thread::sleep(Duration::from_secs(backoff));
        backoff = (backoff * 2).min(300);
    }
    let data_up = redial_until_up(inner);
    {
        let mut c = inner.cache.lock().unwrap();
        c.selection = Some("auto");
        c.selection_at = now();
    }
    inner.last_pass.store(0, Ordering::SeqCst);
    let mut o = inner.ops.lock().unwrap();
    o.guard.phase = "reverted";
    o.guard.reason = if data_up { why.to_string() } else { clip(format!("{why}；已回到自动选网，但数据还没拨上")) };
    o.guard.finished_at = now();
    o.kind = OpKind::Idle;
}

/// The selection is automatic again; get the data call back. A few tries
/// with backoff, the marker saying so meanwhile (a restart carries on) —
/// bounded, because a data call that stays down may be the owner's choice
/// (mobile data switched off) and must not be forced on forever.
fn redial_until_up(inner: &Inner) -> bool {
    let _ = write_marker(&Marker::Redial);
    let mut up = false;
    for (n, pause) in [0u64, 30, 120].into_iter().enumerate() {
        if pause > 0 {
            inner.ops.lock().unwrap().guard.reason = clip(format!("已回到自动选网，数据第 {n} 次没拨上，{pause} 秒后再试"));
            std::thread::sleep(Duration::from_secs(pause));
        }
        if ensure_data_up() {
            up = true;
            break;
        }
    }
    let _ = fs::remove_file(GUARD_MARKER);
    up
}

/// At startup: finish whatever the marker says was in flight. An unreadable
/// marker is treated as "go back to automatic" — the safe direction.
pub fn resume_guard(state: &AppState) {
    // Only "no such file" means nothing is in flight; unreadable or garbled
    // falls through to back-to-automatic, the safe direction.
    let text = match fs::read(GUARD_MARKER) {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            eprintln!("[netinfo] marker unreadable ({e}), going back to automatic");
            String::new()
        }
    };
    let inner = Arc::clone(&state.netinfo.inner);
    match parse_marker(&text) {
        Some(Marker::Register { target, started }) => {
            eprintln!("[netinfo] resuming the register guard for {target}");
            {
                let mut o = inner.ops.lock().unwrap();
                o.kind = OpKind::Registering;
                o.guard = Guard { phase: "registering", target: target.clone(), started_at: started, ..Guard::default() };
            }
            std::thread::spawn(move || guard_run(inner, target, started));
        }
        Some(Marker::Redial) => {
            eprintln!("[netinfo] resuming the redial after back-to-automatic");
            {
                let mut o = inner.ops.lock().unwrap();
                o.kind = OpKind::Reverting;
                o.guard = Guard { phase: "reverting", reason: "重启前还没拨上".into(), started_at: now(), ..Guard::default() };
            }
            std::thread::spawn(move || {
                let up = redial_until_up(&inner);
                let mut o = inner.ops.lock().unwrap();
                o.guard.phase = "reverted";
                if !up {
                    o.guard.reason = "已回到自动选网，但数据还没拨上".into();
                }
                o.guard.finished_at = now();
                o.kind = OpKind::Idle;
            });
        }
        _ => {
            eprintln!("[netinfo] resuming back-to-automatic");
            inner.ops.lock().unwrap().kind = OpKind::Reverting;
            std::thread::spawn(move || revert_until_auto(&inner, "重启前没做完"));
        }
    }
}

/// GET /api/modem/register/guard
pub fn guard_status(state: &AppState) -> (u16, Value) {
    (200, json!({"ok": true, "data": state.netinfo.inner.ops.lock().unwrap().guard.json()}))
}

/// POST /api/modem/netselect/auto — back to automatic operator selection.
pub fn select_auto(state: &AppState) -> (u16, Value) {
    let inner = Arc::clone(&state.netinfo.inner);
    let mut o = inner.ops.lock().unwrap();
    match o.kind {
        OpKind::Reverting => return (202, json!({"ok": true, "data": o.guard.json()})),
        // a search does not change the selection mode
        OpKind::Scanning => return (409, json!({"ok": false, "error": "正在搜索网络，搜完再操作"})),
        OpKind::Registering | OpKind::Idle => {}
    }
    // Accepted means it survives a restart: the marker says "revert" before
    // the 202 goes out, or the request is refused.
    if let Err(e) = write_marker(&Marker::Revert) {
        return (500, json!({"ok": false, "error": format!("写不了保护标记：{e}")}));
    }
    if o.kind == OpKind::Registering {
        // the guard reads this under the same lock at every step
        o.cancel = true;
        return (202, json!({"ok": true, "data": o.guard.json()}));
    }
    o.kind = OpKind::Reverting;
    o.guard = Guard { phase: "reverting", reason: "手动恢复自动".into(), started_at: now(), ..Guard::default() };
    let snapshot = o.guard.json();
    drop(o);
    let i = Arc::clone(&inner);
    std::thread::spawn(move || revert_until_auto(&i, "手动恢复自动"));
    (202, json!({"ok": true, "data": snapshot}))
}

/// POST /api/netinfo/scan (and the old POST /api/modem/scan) — search for
/// operators. Data is down for the whole search (≈110 s measured); every way
/// out goes through the same ending, which gets the data call back.
pub fn operator_scan(state: &AppState) -> (u16, Value) {
    let inner = Arc::clone(&state.netinfo.inner);
    {
        let mut o = inner.ops.lock().unwrap();
        if o.kind == OpKind::Scanning {
            return (202, json!({"ok": true, "data": {"state": "scanning"}}));
        }
        if let Err(why) = claim(&mut o, OpKind::Scanning) {
            return (409, json!({"ok": false, "error": why}));
        }
        o.scan = Scan { state: "scanning", started_at: now(), ..Scan::default() };
    }
    std::thread::spawn(move || {
        let outcome = run_scan(&inner);
        ensure_data_up();
        let mut o = inner.ops.lock().unwrap();
        match outcome {
            Ok(ops) if !ops.is_empty() => {
                o.scan.state = "done";
                o.scan.operators = ops;
            }
            Ok(_) => {
                o.scan.state = "error";
                o.scan.error = "没搜到网络".into();
            }
            Err(e) => {
                o.scan.state = "error";
                o.scan.error = clip(e);
            }
        }
        o.scan.finished_at = now();
        o.kind = OpKind::Idle;
    });
    (202, json!({"ok": true, "data": {"state": "scanning"}}))
}

fn run_scan(inner: &Inner) -> Result<Vec<Value>, String> {
    // A call that errors (a 3 s timeout, say) may still have started the
    // search: watch the status either way, and only a status that never
    // shows a search counts as "did not start".
    let call = ubus_call("zte_nwinfo_api", "nwinfo_manual_scan", Some("{}"));
    let started = now();
    let mut seen_search = false;
    loop {
        std::thread::sleep(SCAN_POLL);
        let st = ubus_call("zte_nwinfo_api", "nwinfo_m_netselect_status", Some("{}")).unwrap_or(Value::Null);
        let status = loose_str(&st, &["status"]);
        inner.ops.lock().unwrap().scan.last_status = status.clone();
        let searching = status.to_ascii_lowercase().contains("selecting");
        seen_search |= searching;
        match scan_finished(&status) {
            Some(true) => break,
            Some(false) => return Err(format!("模组报告搜索失败（{status}）")),
            None => {}
        }
        let waited = now() - started;
        if let Err(e) = &call {
            if !seen_search && waited >= 15 {
                return Err(e.clone());
            }
        }
        // Past the usual time, keep holding the modem while it still says
        // it is searching (hard stop at twice that).
        if waited >= SCAN_TIMEOUT && (!searching || waited >= 2 * SCAN_TIMEOUT) {
            break;
        }
    }
    let v = ubus_call("zte_nwinfo_api", "nwinfo_m_netselect_contents", Some("{}"))?;
    let mut ops = Vec::new();
    collect_operators(&v, &mut ops);
    ops.truncate(SCAN_MAX_OPS);
    Ok(ops)
}

/// POST /api/netinfo/neighbors/scan — switched off.
///
/// Measured 2026-09-25 on B27: the vendor `nwinfo_scan_nbr` dropped the data
/// call within 2 s, the modem did not redial on its own, and both neighbour
/// lists stayed empty. A button that costs the whole household its network
/// for nothing does not belong on the screen; neighbours need another source
/// (datad's diag module). The state says why, so the screen can too.
pub fn neighbors_scan(state: &AppState) -> (u16, Value) {
    let mut n = state.netinfo.inner.nbr.lock().unwrap();
    n.state = "unsupported";
    n.error = NBR_UNSUPPORTED.into();
    (410, json!({"ok": false, "error": NBR_UNSUPPORTED}))
}

const NBR_UNSUPPORTED: &str = "原厂扫描会断网且拿不到数据，已停用";

#[cfg(test)]
mod tests {
    #[test]
    fn apn_in_use_is_the_dialled_profile() {
        // Recorded on the owner's device 2026-09-25: auto mode, and the manual
        // list still marks CTNET as the manual choice.
        let mode = json!({"apn_mode": 0});
        let dialled = json!({"profilename":"China Telecom","wanapn":"ctiot","pdpType":3,"profileId":"auto109590","isEnable":false});
        let auto = json!({"apnListArray":[
            {"profilename":"China Telecom","wanapn":"ctiot","pdpType":3,"profileId":"auto109590","isEnable":true},
            {"profilename":"China Telecom 4G","wanapn":"ctnet","pdpType":3,"profileId":"auto109600","isEnable":false}]});
        let manual = json!({"apnListArray":[{"profilename":"CTNET","wanapn":"ctnet","pdpType":3,"profileId":"manu1","isEnable":true}]});
        let v = apn_view(&mode, &dialled, &auto, &manual);
        assert_eq!(v["mode"], "auto");
        assert_eq!(v["in_use"]["name"], "China Telecom");
        assert_eq!(v["in_use"]["apn"], "ctiot");
        assert_eq!(v["auto"][0]["in_use"], true);
        assert_eq!(v["manual"][0]["selected"], true);
        assert_eq!(v["manual"][0]["in_use"], false);
        let v = apn_view(&json!({"apn_mode":"1"}), &json!({"profileId":"manu1","profilename":"CTNET"}), &auto, &manual);
        assert_eq!((v["mode"].as_str(), v["manual"][0]["in_use"].as_bool()), (Some("manual"), Some(true)));
        assert!(apn_view(&mode, &Value::Null, &Value::Null, &Value::Null)["in_use"].is_null());
    }

    use super::*;

    #[test]
    fn geo_ipip_json() {
        let g = parse_geo(r#"{"ret":"ok","data":{"ip":"1.2.3.4","location":["中国","北京","北京","","联通"]}}"#).unwrap();
        assert_eq!(g, Geo { ip: "1.2.3.4".into(), geo: "中国 北京".into(), isp: "联通".into() });
    }

    #[test]
    fn geo_ipip_text() {
        let g = parse_geo("当前 IP：1.2.3.4  来自于：中国 广东 深圳  电信\n").unwrap();
        assert_eq!(g.ip, "1.2.3.4");
        assert_eq!(g.geo, "中国 广东 深圳");
        assert_eq!(g.isp, "电信");
    }

    #[test]
    fn geo_ip_api() {
        let g = parse_geo(r#"{"status":"success","country":"日本","regionName":"东京都","city":"东京","isp":"KDDI","org":"","query":"5.6.7.8"}"#).unwrap();
        assert_eq!(g, Geo { ip: "5.6.7.8".into(), geo: "日本 东京都".into(), isp: "KDDI".into() });
        assert!(parse_geo(r#"{"status":"fail","query":"x"}"#).is_none());
    }

    #[test]
    fn geo_ipinfo() {
        let g = parse_geo(r#"{"ip":"9.9.9.9","city":"Taipei","region":"Taiwan","country":"TW","org":"AS3462 Chunghwa Telecom"}"#).unwrap();
        assert_eq!(g.geo, "中国台湾 Taiwan Taipei");
        assert_eq!(g.isp, "Chunghwa Telecom");
        assert!(parse_geo("<html>").is_none());
    }

    #[test]
    fn geo_ping0() {
        let g = parse_geo("188.253.116.74\n中国 台湾 台北\nAS38136\nAkari Networks Limited\n").unwrap();
        assert_eq!(g, Geo { ip: "188.253.116.74".into(), geo: "中国 台湾 台北".into(), isp: "Akari Networks Limited".into() });
        let g = parse_geo("1.2.3.4\n中国 广东 深圳 — 电信\nAS4134\nCHINANET\n").unwrap();
        assert_eq!((g.geo.as_str(), g.isp.as_str()), ("中国 广东 深圳", "电信"));
    }

    #[test]
    fn geo_cip_cc() {
        let body = "IP\t: 116.232.29.205\n地址\t: 中国 上海 上海\n运营商\t: 电信\n\n数据二\t: 中国 上海 上海 | 电信\n\nURL\t: http://www.cip.cc/116.232.29.205\n";
        let g = parse_geo(body).unwrap();
        assert_eq!(g, Geo { ip: "116.232.29.205".into(), geo: "中国 上海".into(), isp: "电信".into() });
        assert!(parse_geo("IP\t: nope\n").is_none());
    }

    #[test]
    fn geo_myip_la() {
        let g = parse_geo(r#"{"ip":"188.253.116.74","location":{"city":"台北市","country_code":"TW","country_name":"中国","latitude":"25.03","province":"台湾"}}"#).unwrap();
        assert_eq!(g, Geo { ip: "188.253.116.74".into(), geo: "中国 台湾 台北市".into(), isp: String::new() });
    }

    #[test]
    fn geo_ip_sb_and_ipapi_co_use_the_country_code() {
        let g = parse_geo(r#"{"region":"Taipei City","isp":"Akari Networks","city":"Taipei","ip":"188.253.116.74","country":"Taiwan","country_code":"TW"}"#).unwrap();
        assert_eq!(g.geo, "中国台湾 Taipei City");
        assert_eq!(g.isp, "Akari Networks");
        let g = parse_geo(r#"{"ip":"1.1.1.1","city":"Tokyo","region":"Tokyo","country":"JP","country_name":"Japan","org":"AS13335 CLOUDFLARENET"}"#).unwrap();
        assert!(g.geo.starts_with("日本"), "{}", g.geo);
        assert_eq!(g.isp, "CLOUDFLARENET");
    }

    #[test]
    fn imsi_mnc_length_by_country() {
        assert_eq!(plmn_from_imsi("460011234567890"), Some(Plmn { mcc: 460, mnc: 1, mnc_digits: 2 }));
        assert_eq!(plmn_from_imsi("310260123456789"), Some(Plmn { mcc: 310, mnc: 260, mnc_digits: 3 }));
        assert_eq!(plmn_from_imsi("44010"), None);
        assert_eq!(plmn_from_imsi("4401x123456"), None);
        let p = plmn_from_imsi("310260123456789").unwrap();
        assert_eq!(plmn_json(&p, "")["name"], "T-Mobile");
        assert_eq!(plmn_json(&p, "")["mnc"], "260");
        let p = plmn_from_imsi("454000123456789").unwrap();
        assert_eq!(plmn_json(&p, "")["mnc"], "00");
        assert_eq!(plmn_json(&p, "")["country"], "中国香港");
    }

    #[test]
    fn plmn_strings() {
        assert!(same_plmn("46001", "46001"));
        assert!(same_plmn("46692", "466092"));
        assert!(!same_plmn("46001", "46000"));
        assert!(!same_plmn("46001", ""));
        let p = plmn_from_str("44020").unwrap();
        assert_eq!(plmn_json(&p, "x")["name"], "SoftBank");
        let p = plmn_from_str("99999").unwrap();
        assert_eq!(plmn_json(&p, "Hint")["name"], "Hint");
        assert_eq!(serving_plmn(&json!({"rmcc":"460","rmnc":"01"})), "46001");
        assert_eq!(serving_plmn(&json!({"rplmn_num":"46011"})), "46011");
    }

    #[test]
    fn connected_is_not_disconnected() {
        assert!(data_connected(&json!({"connect_status":"ipv4_ipv6_connected"})));
        assert!(!data_connected(&json!({"connect_status":"disconnected"})));
        assert!(!data_connected(&json!({})));
    }

    #[test]
    fn cops_mode() {
        assert_eq!(parse_cops_mode("\r\n+COPS: 0,0,\"CHN-UNICOM\",7\r\n\r\nOK\r\n"), Some("auto"));
        assert_eq!(parse_cops_mode("+COPS: 1,2,\"46001\",12\r\nOK"), Some("manual"));
        assert_eq!(parse_cops_mode("+COPS: 0\r\nOK"), Some("auto"));
        assert_eq!(parse_cops_mode("ERROR"), None);
    }

    fn o<'a>(result: &'a str, serving: &'a str, connected: bool) -> Obs<'a> {
        Obs { result, serving, connected }
    }

    #[test]
    fn guard_decisions() {
        // Where the modem is decides success, whatever the string says.
        assert_eq!(decide("46001", &o("weird", "46001", true), 9), Verdict::Ok);
        assert_eq!(decide("46001", &o("success", "46000", true), 9), Verdict::Wait);
        // A stale "fail" right after the call is ignored…
        assert_eq!(decide("46001", &o("fail", "46000", true), 3), Verdict::Wait);
        // …a later one reverts.
        assert_eq!(decide("46001", &o("fail", "46000", true), 9), Verdict::Revert("注册失败"));
        assert_eq!(decide("46001", &o("0", "46000", false), 9), Verdict::Revert("注册失败"));
        // Registered but no data at the deadline is still a revert.
        assert_eq!(decide("46001", &o("1", "46001", false), GUARD_TIMEOUT), Verdict::Revert("已注册但数据没通"));
        assert_eq!(decide("46001", &o("", "46000", true), GUARD_TIMEOUT), Verdict::Revert("超时没注册上"));
        assert_eq!(decide("46001", &o("", "", false), GUARD_TIMEOUT - 1), Verdict::Wait);
    }

    #[test]
    fn neighbour_cells_loose_match() {
        let v = json!({"nr5g_nbr_list":[
            {"nr5g_pci":"101","nr5g_arfcn":"504990","nr5g_rsrp":"-95","nr5g_rsrq":"-11","nr5g_sinr":"12"},
            {"nr5g_pci":"102","nr5g_arfcn":"504990","nr5g_rsrp":"-101"}
        ]});
        let mut cells = Vec::new();
        collect_cells(&v, "NR", &mut cells);
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0]["pci"], "101");
        assert_eq!(cells[0]["arfcn"], "504990");
        assert_eq!(cells[1]["sinr"], Value::Null);
        assert_eq!(rsrp_of(&cells[0]), -95.0);
    }

    #[test]
    fn slot_due_and_backoff() {
        let mut s = Slot::default();
        assert!(s.due("rmnet 10.0.0.1", 1000));
        s.store("rmnet 10.0.0.1", 1000, Ok((Geo { ip: "1.1.1.1".into(), ..Geo::default() }, "h".into())), None);
        assert!(!s.due("rmnet 10.0.0.1", 1000 + LOOKUP_MAX_AGE - 1));
        assert!(s.due("rmnet 10.0.0.1", 1000 + LOOKUP_MAX_AGE));
        assert!(s.due("rmnet 10.0.0.2", 1001));
        // A failure keeps the old answer for the same exit and backs off.
        s.store("rmnet 10.0.0.1", 2000, Err("timeout".into()), None);
        assert_eq!(s.value.as_ref().unwrap().geo.ip, "1.1.1.1");
        assert!(!s.due("rmnet 10.0.0.1", 2000 + LOOKUP_RETRY - 1));
        assert!(s.due("rmnet 10.0.0.1", 2000 + LOOKUP_RETRY));
        // A failure on a new exit drops the old, now wrong, address.
        s.store("rmnet 10.0.0.9", 3000, Err("timeout".into()), None);
        assert_eq!(s.value.as_ref().unwrap().geo.ip, "");
    }

    #[test]
    fn config_parsing() {
        let c = Config::parse("");
        assert!(c.enabled);
        let c = Config::parse("direct=https://a.example/x, ftp://no\nproxy_port=7000\nlookups=0\n");
        assert_eq!(c.direct, vec!["https://a.example/x".to_string()]);
        assert!(!c.enabled);
    }

    #[test]
    fn operator_list() {
        let v = json!({"operators":[
            {"m_mcc_mnc":"46001","m_oper_name":"CHN-UNICOM","m_rat":"12","m_status":"2"},
            {"m_mcc_mnc":"46000","m_oper_name":"CHINA MOBILE","m_rat":"7","m_status":"1"},
            {"m_mcc_mnc":"99999","m_oper_name":"Test","m_rat":7,"m_status":"3"}
        ]});
        let mut ops = Vec::new();
        collect_operators(&v, &mut ops);
        assert_eq!(ops.len(), 3);
        assert_eq!(ops[0]["name"], "中国联通");
        assert_eq!(ops[0]["raw_name"], "CHN-UNICOM");
        assert_eq!(ops[0]["rat"], "12");
        assert_eq!(ops[2]["name"], "Test");
        assert_eq!(ops[2]["rat"], "7");
        assert_eq!(ops[2]["country"], Value::Null);
        assert_eq!(scan_finished("done"), Some(true));
        assert_eq!(scan_finished("2"), Some(true));
        assert_eq!(scan_finished("fail"), Some(false));
        assert_eq!(scan_finished("1"), None);
        assert_eq!(scan_finished("manual_selecting"), None);
        assert_eq!(scan_finished("manual_selected"), Some(true));
        assert_eq!(scan_finished(""), None);
        // the device's own answer, verbatim
        let mut real = Vec::new();
        collect_operators(&json!({"m_netselect_contents": "2,CT,46011,11;1,CT,46011,7;3,CMCC,46000,11;3,UNICOM,46001,2;"}), &mut real);
        assert_eq!(real.len(), 4);
        assert_eq!((real[0]["plmn"].as_str(), real[0]["status"].as_str(), real[0]["rat"].as_str()), (Some("46011"), Some("2"), Some("11")));
        assert_eq!(real[3]["name"], "中国联通");
        assert_eq!(real[3]["rat"], "2");
    }

    #[test]
    fn marker_and_clip() {
        assert_eq!(parse_marker("46001 1790000000\n"), Some(Marker::Register { target: "46001".into(), started: 1790000000 }));
        assert_eq!(parse_marker("register 46000 5\n"), Some(Marker::Register { target: "46000".into(), started: 5 }));
        assert_eq!(parse_marker("revert 123\n"), Some(Marker::Revert));
        assert_eq!(parse_marker("redial 123\n"), Some(Marker::Redial));
        assert_eq!(parse_marker("x 1"), None);
        assert_eq!(parse_marker(""), None);
        let long: String = "错".repeat(100);
        let c = clip(long);
        assert_eq!(c.chars().count(), ERR_MAX_CHARS + 1);
        assert_eq!(clip("short".into()), "short");
    }

    #[test]
    fn measured_netinfo_shape() {
        // 2026-09-25, the device on China Telecom SA
        let net = json!({"rmcc": 460, "rmnc": 11, "net_select_mode": "auto_select", "network_provider": "CT"});
        assert_eq!(serving_plmn(&net), "46011");
        assert_eq!(serving_plmn(&json!({"rmcc": 460, "rmnc": 1})), "46001");
        assert_eq!(serving_plmn(&json!({"rmcc": 310, "rmnc": 260})), "310260");
        assert_eq!(serving_plmn(&json!({"rmcc": 0, "rmnc": 0})), "");
        assert_eq!(selection_from_netinfo(&net), Some("auto"));
        assert_eq!(selection_from_netinfo(&json!({"net_select_mode": "manual_select"})), Some("manual"));
        assert_eq!(selection_from_netinfo(&json!({})), None);
    }

    #[test]
    fn string_records() {
        let mut ops = Vec::new();
        collect_operators(&json!({"m_netselect_contents": "2,China Telecom,CT,46011,12;1,China Mobile,CMCC,46000,7;"}), &mut ops);
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0]["plmn"], "46011");
        assert_eq!(ops[0]["name"], "中国电信");
        assert_eq!(ops[0]["status"], "2");
        assert_eq!(ops[0]["rat"], "12");
        let mut none = Vec::new();
        collect_operators(&json!({"m_netselect_contents": ""}), &mut none);
        assert!(none.is_empty());

        let mut cells = Vec::new();
        collect_cells(&json!({"nr5g_nbr_contents": "633984,388,-101,-12,8.5;627264,101,-95.0,-10.0,14"}), "NR", &mut cells);
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0]["pci"], "388");
        assert_eq!(cells[0]["arfcn"], "633984");
        assert_eq!(cells[0]["rsrp"], "-101");
        assert_eq!(cells[0]["rsrq"], "-12");
        assert_eq!(cells[0]["sinr"], "8.5");
    }

    #[test]
    fn stations_and_leases() {
        let dev = "phy#1\n\tInterface wlan1\n\t\tifindex 9\n\t\ttype AP\nphy#0\n\tInterface wlan0\n\t\ttype managed\n";
        assert_eq!(ap_ifaces(dev), vec!["wlan1".to_string()]);
        let dump = "Station 02:00:00:00:00:0A (on wlan1)\n\tinactive time:\t120 ms\n\trx bytes:\t1000\n\ttx bytes:\t50000\n\tsignal:  \t-47 [-47, -49] dBm\n\tconnected time:\t360 seconds\nStation 02:00:00:00:00:0b (on wlan1)\n\trx bytes:\t10\n\ttx bytes:\t20\n";
        let st = parse_station_dump(dump, "wlan1");
        assert_eq!(st.len(), 2);
        assert_eq!(st[0].mac, "02:00:00:00:00:0a");
        assert_eq!((st[0].down, st[0].up), (50000, 1000));
        assert_eq!(st[0].signal, Some(-47));
        assert_eq!(st[0].connected_secs, 360);
        let leases = parse_leases("1790300000 02:00:00:00:00:0a 192.168.0.109 laptop 01:aa\n1790300000 02:00:00:00:00:0b 192.168.0.110 * *\n");
        assert_eq!(leases[0], ("02:00:00:00:00:0a".into(), "192.168.0.109".into(), "laptop".into()));
        assert_eq!(leases[1].2, "");
        assert_eq!(rate(Some((1000, 100)), 21000, 110), Some(2000));
        assert_eq!(rate(Some((5000, 100)), 10, 110), None);
        assert_eq!(rate(None, 10, 110), None);
    }

    #[test]
    fn band_and_link() {
        // Recorded on the owner's device 2026-09-25 (MACs made up).
        let ap5 = parse_ap_info("\tssid U-Cafe\n\ttype AP\n\tchannel 44 (5220 MHz), width: 160 MHz, center1: 5250 MHz\n");
        assert_eq!(ap5, ApInfo { band: "5 GHz", channel: Some(44), width_mhz: Some(160) });
        let ap2 = parse_ap_info("\tchannel 11 (2462 MHz), width: 40 MHz, center1: 2452 MHz\n");
        assert_eq!((ap2.band, ap2.channel, ap2.width_mhz), ("2.4 GHz", Some(11), Some(40)));
        assert_eq!(parse_ap_info("\ttype AP\n").band, "");
        let dump = "Station 02:00:00:00:00:0c (on wlan2)\n\tsignal:  \t-39 dBm\n\ttx bitrate:\t2401.9 MBit/s 160MHz HE-MCS 11 HE-NSS 2 HE-GI 0 HE-DCM 0\n\trx bitrate:\t1200.9 MBit/s 160MHz HE-MCS 11 HE-NSS 1 HE-GI 0 HE-DCM 0\n";
        let st = parse_station_dump(dump, "wlan2");
        assert_eq!((st[0].link_down_mbps, st[0].link_up_mbps, st[0].wifi_gen), (Some(2402), Some(1201), Some(6)));
        assert_eq!(parse_bitrate("866.7 MBit/s VHT-MCS 9 80MHz short GI VHT-NSS 2"), (Some(867), Some(5)));
        assert_eq!(parse_bitrate("144.4 MBit/s MCS 15 short GI"), (Some(144), Some(4)));
        assert_eq!(parse_bitrate("54.0 MBit/s"), (Some(54), None));
    }

    #[test]
    fn one_operation_at_a_time() {
        let mut o = Ops::default();
        assert!(claim(&mut o, OpKind::Scanning).is_ok());
        assert_eq!(claim(&mut o, OpKind::Registering), Err("正在搜索网络，等搜完再操作"));
        assert_eq!(claim(&mut o, OpKind::Reverting), Err("正在搜索网络，等搜完再操作"));
        o.kind = OpKind::Registering;
        assert!(claim(&mut o, OpKind::Scanning).is_err());
        o.kind = OpKind::Idle;
        assert!(claim(&mut o, OpKind::Reverting).is_ok());
        assert_eq!(o.kind, OpKind::Reverting);
    }

    #[test]
    fn place_join() {
        assert_eq!(join_place(&["中国", "北京", "北京"]), "中国 北京");
        assert_eq!(join_place(&["日本", "东京都", "东京"]), "日本 东京都");
        assert_eq!(join_place(&["", "", ""]), "");
    }
}
