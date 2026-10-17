//! doctor.sh's CHILL rows: chill-api (who listens on :9999) and home-exit
//! (the 家里 chain). Same order, text and commands as the shell's CHILL block.

use super::{Row, digits, row, sh_out};
use std::process::{Command, Stdio};

pub struct Cfg {
    pub env: String,
    pub state: String,
    pub ip: Vec<String>,
    pub template: String,
    pub dir: String,
}

pub fn rows(c: &super::Cfg) -> Vec<Row> {
    let mut v = Vec::new();
    if !std::path::Path::new(&c.chill.env).is_file() {
        return v;
    }
    v.push(api(c));
    v.extend(home_exit(c));
    v
}

/// `cat tcp tcp6 | awk '$4 == "0A" && $2 ~ /:270F$/ {…print $2}' | sort -u`
fn listeners(c: &super::Cfg) -> Vec<String> {
    let mut set = std::collections::BTreeSet::new();
    for f in ["net/tcp", "net/tcp6"] {
        let Ok(b) = std::fs::read(format!("{}/{f}", c.proc_dir)) else {
            continue;
        };
        for l in String::from_utf8_lossy(&b).split('\n') {
            let w: Vec<&str> = l.split([' ', '\t']).filter(|x| !x.is_empty()).collect();
            if w.len() >= 4
                && w[3] == "0A"
                && let Some(a) = w[1].strip_suffix(":270F")
            {
                set.insert(a.to_owned());
            }
        }
    }
    set.into_iter().collect()
}

fn api(c: &super::Cfg) -> Row {
    let api = listeners(c);
    let mut apiw = String::new();
    for a in &api {
        match a.as_str() {
            "0100007F"
            | "00000000000000000000000001000000"
            | "0000000000000000FFFF00000100007F" => {}
            "00000000" => apiw.push_str(" 0.0.0.0"),
            "00000000000000000000000000000000" => apiw.push_str(" ::"),
            x => {
                apiw.push(' ');
                apiw.push_str(x);
            }
        }
    }
    if !apiw.is_empty() {
        row(
            "warn",
            "chill-api",
            "CHILL 管理接口",
            format!("对局域网开放（:9999 听在{apiw}）；chill.env 里 CHILL_API_LAN 应为 0"),
            "CHILL API",
            format!("Open to the LAN (:9999 on{apiw}); set CHILL_API_LAN=0 in chill.env"),
        )
    } else if !api.is_empty() {
        row(
            "ok",
            "chill-api",
            "CHILL 管理接口",
            "只听本机".into(),
            "CHILL API",
            "This device only".into(),
        )
    } else {
        row(
            "ok",
            "chill-api",
            "CHILL 管理接口",
            "没在监听（CHILL 没开）".into(),
            "CHILL API",
            "Not listening (CHILL off)".into(),
        )
    }
}

/// `sed -n "s/^CHILL_HOME_EXIT=['\"]\{0,1\}\([^'\"]*\)['\"]\{0,1\}[[:space:]]*\$/\1/p" | tail -n 1`
fn home_exit_value(env: &str) -> String {
    let mut last = String::new();
    for l in env.split('\n') {
        let Some(r) = l.strip_prefix("CHILL_HOME_EXIT=") else {
            continue;
        };
        let r = r.strip_prefix(['\'', '"']).unwrap_or(r);
        let end = r.find(['\'', '"']).unwrap_or(r.len());
        let rest = &r[end..];
        let rest = rest.strip_prefix(['\'', '"']).unwrap_or(rest);
        if rest
            .chars()
            .all(|ch| matches!(ch, ' ' | '\t' | '\r' | '\x0b' | '\x0c'))
        {
            last = r[..end].to_owned();
        }
    }
    last
}

fn grep_file(file: &str, needle: &str) -> bool {
    std::fs::read(file).is_ok_and(|b| {
        String::from_utf8_lossy(&b)
            .split('\n')
            .any(|l| l.contains(needle))
    })
}

/// days since 1970-01-01 of a civil date (proleptic Gregorian)
fn days(y: i64, m: i64, d: i64) -> i64 {
    let (y, m) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + d - 1;
    era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468
}

/// `awk -v now=T '{ gsub(/[-:]/, " "); a = now - mktime($0); print ((a >= 0) ? a : "") }'` (TZ=UTC)
fn age(stamp: &str, now: i64) -> Option<i64> {
    let f: Vec<i64> = stamp
        .replace(['-', ':'], " ")
        .split_whitespace()
        .map(|x| x.parse().unwrap_or(0))
        .collect();
    let t = if f.len() >= 6 {
        days(f[0], f[1], f[2]) * 86400 + f[3] * 3600 + f[4] * 60 + f[5]
    } else {
        -1
    };
    let a = now - t;
    (a >= 0).then_some(a)
}

/// The last history entry of 家里: `grep -o '"history":\[[^]]*\]' | tail -n 1 | tr '{' '\n' | tail -n 1`
fn last_entry(body: &str) -> String {
    let mut last = None;
    for l in body.split('\n') {
        let mut i = 0;
        while let Some(p) = l[i..].find("\"history\":[") {
            let s = i + p;
            let open = s + "\"history\":[".len();
            match l[open..].find(']') {
                Some(e) => {
                    last = Some(&l[s..open + e + 1]);
                    i = open + e + 1;
                }
                None => break,
            }
        }
    }
    last.map(|h| h.rsplit('{').next().unwrap_or("").to_owned())
        .unwrap_or_default()
}

/// `sed -n 's/.*"delay":\([0-9][0-9]*\).*/\1/p'` (the last on the line)
fn delay(h: &str) -> Option<String> {
    h.rmatch_indices("\"delay\":").find_map(|(i, k)| {
        let d: String = h[i + k.len()..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        (!d.is_empty()).then_some(d)
    })
}

/// `sed -n 's/.*"time":"\([0-9-]*\)T\([0-9:]*\).*/\1 \2/p'` (the last that matches)
fn time(h: &str) -> Option<String> {
    h.rmatch_indices("\"time\":\"").find_map(|(i, k)| {
        let r = &h[i + k.len()..];
        let dlen = r
            .find(|ch: char| !(ch.is_ascii_digit() || ch == '-'))
            .unwrap_or(r.len());
        let r2 = r[dlen..].strip_prefix('T')?;
        let tlen = r2
            .find(|ch: char| !(ch.is_ascii_digit() || ch == ':'))
            .unwrap_or(r2.len());
        Some(format!("{} {}", &r[..dlen], &r2[..tlen]))
    })
}

fn home_exit(c: &super::Cfg) -> Option<Row> {
    let env = std::fs::read(&c.chill.env)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let hx = home_exit_value(&env);
    if hx.is_empty() {
        return None;
    }
    let (ip, port) = match hx.rfind(':') {
        Some(i) => (&hx[..i], &hx[i + 1..]),
        None => (hx.as_str(), hx.as_str()),
    };
    let bad = hx
        .chars()
        .any(|ch| !(ch.is_ascii_digit() || ch == '.' || ch == ':'))
        || hx.matches(':').count() >= 2
        || hx.starts_with(':')
        || hx.ends_with(':')
        || ip == hx;
    let r = |lv, z: String, e: String| Some(row(lv, "home-exit", "家里出口", z, "Home exit", e));
    if bad {
        return r(
            "warn",
            format!("chill.env 的 CHILL_HOME_EXIT 不是 IPv4:端口（{hx}）"),
            format!("CHILL_HOME_EXIT in chill.env is not IPv4:port ({hx})"),
        );
    }
    if grep_file(&c.chill.state, "\"reason\":\"idle\"") {
        return r(
            "ok",
            "CHILL 息屏暂停（没人用），用不上".into(),
            "Not in use (CHILL paused: screen off, nobody connected)".into(),
        );
    }
    if !grep_file(&c.chill.state, "\"state\":\"running\"") {
        return r(
            "ok",
            "CHILL 没在跑，用不上".into(),
            "Not in use (CHILL not running)".into(),
        );
    }
    let route = match c.chill.ip.split_first() {
        Some((p, pre)) => Command::new(p)
            .args(pre)
            .args(["route", "get", ip])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default(),
        None => String::new(),
    };
    if !route.contains("dev tailscale0") {
        return r(
            "warn",
            format!("到 {ip} 不走 Tailscale（子网路由没发布，或 Tailscale 没连上）"),
            format!("No Tailscale route to {ip} (subnet route not advertised, or Tailscale down)"),
        );
    }
    let body = sh_out(
        &c.wget,
        &[
            "-q",
            "-T",
            "3",
            "-O",
            "-",
            "http://127.0.0.1:9999/proxies/%E5%AE%B6%E9%87%8C",
        ],
        true,
    );
    let h = last_entry(&body);
    let now = digits(&sh_out(&c.date, &["+%s"], false)).unwrap_or(0);
    let (mut hxz, mut hxe) = (String::new(), String::new());
    if let Some(a) = time(&h).and_then(|t| age(&t, now))
        && a >= 1800
    {
        hxz = format!("，{} 分钟前", a / 60);
        hxe = format!(", {} min ago", a / 60);
    }
    match delay(&h) {
        None => r(
            "ok",
            "经 Tailscale 通；还没测过家里（没人用时不测）".into(),
            "Routed via Tailscale; home not probed yet (no probe while nobody uses it)".into(),
        ),
        Some(d) if !d.trim_start_matches('0').is_empty() => r(
            "ok",
            format!("经 Tailscale 能用（{d} ms{hxz}）"),
            format!("Working via Tailscale ({d} ms{hxe})"),
        ),
        Some(_) => r(
            "warn",
            format!("家里没回应（{ip}:{port}{hxz}；Mac 睡了，或代理端口变了）"),
            format!("Home not answering ({ip}:{port}{hxe}; Mac asleep, or proxy port changed)"),
        ),
    }
}
