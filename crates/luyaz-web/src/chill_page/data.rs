//! /services/chill: the pure parts (words, clock sums, name lookups).
//! Nothing here talks to the
//! agent, so it is all unit-tested.

use serde_json::Value;

use crate::i18n::Lang;

/// Must match the whitelist in luyaz-api `chill.rs` (MAIN_GROUP / AI_GROUP).
pub const MAIN_GROUP: &str = "🚀 节点选择";
pub const AI_GROUP: &str = "🤖 AI";

pub fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}

/// The device clock now: local wall time labelled UTC, so the epoch is the device's own.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// "2026-10-13T08:05:00Z" or with fractions (mihomo's Go time): the epoch of its digits.
pub fn epoch(iso: &str) -> Option<i64> {
    let b = iso.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || (b[10] != b'T' && b[10] != b' ') {
        return None;
    }
    let n = |a: usize, z: usize| iso.get(a..z)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (n(0, 4)?, n(5, 7)?, n(8, 10)?, n(11, 13)?, n(14, 16)?, n(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || y < 1970 {
        return None;
    }
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + se)
}

/// "2026-10-13" of an epoch's digits.
pub fn date(secs: i64) -> String {
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// "5h 12m" / "2d 3h" / "9m" since `iso`; None if unreadable or in the future.
pub fn uptime(iso: &str, now: i64) -> Option<String> {
    let s = now - epoch(iso)?;
    if s < 0 {
        return None;
    }
    let (d, h, m) = (s / 86_400, s % 86_400 / 3600, s % 3600 / 60);
    Some(if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    })
}

/// "3 min ago" for a provider's `updated_at` (mihomo's own time).
pub fn ago(l: Lang, iso: &str, now: i64) -> String {
    let Some(at) = epoch(iso).filter(|a| *a > 0) else {
        return "—".into();
    };
    let s = now - at;
    match (l, s) {
        (Lang::En, ..60) => "just now".into(),
        (Lang::Zh, ..60) => "刚刚".into(),
        (Lang::En, ..3600) => format!("{} min ago", s / 60),
        (Lang::Zh, ..3600) => format!("{} 分钟前", s / 60),
        (Lang::En, ..86_400) => format!("{} h ago", s / 3600),
        (Lang::Zh, ..86_400) => format!("{} 小时前", s / 3600),
        (Lang::En, ..604_800) => format!("{} d ago", s / 86_400),
        (Lang::Zh, ..604_800) => format!("{} 天前", s / 86_400),
        _ => date(at),
    }
}

/// A short stand-in for a device-supplied name, so the name itself never goes into a URL: the
/// server looks the entry up again by position and refuses if this no longer matches.
pub fn tag(name: &str) -> String {
    let mut h: u32 = 0x811c_9dc5;
    for b in name.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    format!("{h:08x}")
}

pub fn ipv4(s: &str) -> bool {
    let p: Vec<&str> = s.split('.').collect();
    p.len() == 4
        && p.iter().all(|x| {
            !x.is_empty() && x.len() <= 3 && x.bytes().all(|b| b.is_ascii_digit()) && x.parse::<u16>().is_ok_and(|n| n <= 255)
        })
}

/// A subscription link the agent would take (`providers_set`): http(s), no quotes, no blanks.
pub fn sub_url_ok(u: &str) -> bool {
    (u.starts_with("http://") || u.starts_with("https://"))
        && u.len() <= 4096
        && !u.contains(['\'', '"'])
        && !u.chars().any(|c| c.is_whitespace() || c.is_control())
}

pub fn reason(code: &str, l: Lang) -> String {
    let (en, zh) = match code {
        "overheat" => ("overheated", "过热"),
        "lowmem" => ("low on memory", "内存不足"),
        "gaveup" => ("gave up after repeated crashes", "多次崩溃已放弃重试"),
        "ruleset_missing" => ("rule set missing", "规则集缺失"),
        "captive_wan" => ("uplink is not cellular (captive portal)", "出口不是蜂窝网络（门户认证中）"),
        "paused" => ("paused by hand", "已手动暂停"),
        "idle" => ("paused while the screen is off (resumes when it lights or a device joins)", "息屏暂停（没人在用，亮屏或有设备连上就恢复）"),
        "disabled" => ("switched off by hand", "已手动停用"),
        _ => return code.to_owned(),
    };
    l.pick(en, zh).to_owned()
}

pub const EXITS: [&str; 4] = ["proxy", "direct_keep_ai", "direct_all", "global"];

pub fn exit_label(x: &str, l: Lang) -> &'static str {
    match x {
        "proxy" => l.pick("Proxy", "代理"),
        "direct_keep_ai" => l.pick("Direct · AI stays", "直连 · AI 不动"),
        "direct_all" => l.pick("All direct", "全部直连"),
        _ => l.pick("Global", "全局"),
    }
}

pub fn exit_hint(x: &str, l: Lang) -> &'static str {
    match x {
        "proxy" => l.pick("Normal, as at home.", "正常状态，和在国内一样。"),
        "direct_keep_ai" => l.pick(
            "Abroad on a local SIM: everything direct except AI and VoWiFi.",
            "在国外用当地卡时：除 AI 和 VoWiFi 外都直连，AI、VoWiFi 照旧走各自的节点。",
        ),
        "direct_all" => l.pick("AI and VoWiFi go direct too.", "AI 和 VoWiFi 也直连。"),
        _ => l.pick("mihomo's global mode, same as before.", "mihomo 全局模式，和原来的「全局」一样。"),
    }
}

pub const PROFILES: [&str; 3] = ["eco", "standard", "perf"];

pub fn profile_label(p: &str, l: Lang) -> &'static str {
    match p {
        "eco" => l.pick("Eco", "省电"),
        "perf" => l.pick("Performance", "性能"),
        _ => l.pick("Standard", "标准"),
    }
}

pub fn profile_hint(p: &str, l: Lang) -> &'static str {
    match p {
        "eco" => l.pick(
            "Probes nodes hourly, fewer keep-alives, core limited to 2 cores. Cooler and lighter on battery; a dead node takes longer to be noticed.",
            "每小时测一次节点、少发保活包、核心只用 2 个核。更凉、更省电；节点挂了要更久才发现。",
        ),
        "perf" => l.pick(
            "Probes nodes every 10 minutes and dials several addresses at once. Faster failover and connects, a little more power.",
            "每 10 分钟测一次节点，同时向多个地址发起连接。切换和连接更快，耗电稍多。",
        ),
        _ => l.pick("The settings CHILL has always used.", "CHILL 一直以来用的那一套。"),
    }
}

// ---- manual-first notice (old `NOTICE_TONE`) ----

pub fn notice_tone(kind: &str, on_backup: bool) -> &'static str {
    match kind {
        "backup" | "unstable" => "warn",
        "alldown" => "bad",
        "back" | "repick" => "ok",
        _ if on_backup => "warn",
        _ => "ok",
    }
}

// ---- device bypass ----

/// A device's name: lease hostname → the hosts hint for its MAC → its IP.
pub fn client_name(clients: &Value, ip: &str) -> String {
    let leases = clients["dhcp_leases"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let Some(le) = leases.iter().find(|x| s(x, "ipaddr") == ip) else {
        return ip.to_owned();
    };
    if !s(le, "hostname").is_empty() {
        return s(le, "hostname").to_owned();
    }
    let mac = s(le, "macaddr");
    for k in [mac.to_owned(), mac.to_uppercase(), mac.to_lowercase()] {
        let n = s(&clients["hosts"][k.as_str()], "name");
        if !n.is_empty() {
            return n.to_owned();
        }
    }
    ip.to_owned()
}

/// Every lease IP, then the bypassed ones with no lease.
pub fn bypass_rows(clients: Option<&Value>, ips: &[String]) -> Vec<String> {
    let mut rows: Vec<String> = clients
        .and_then(|c| c["dhcp_leases"].as_array())
        .map(|a| a.iter().map(|x| s(x, "ipaddr")).filter(|i| !i.is_empty()).map(str::to_owned).collect())
        .unwrap_or_default();
    for ip in ips {
        if !rows.contains(ip) {
            rows.push(ip.clone());
        }
    }
    rows
}

/// The bypass list after switching `ip` on or off (no duplicates, order kept).
pub fn bypass_next(ips: &[String], ip: &str, on: bool) -> Vec<String> {
    let mut v: Vec<String> = ips.iter().filter(|x| x.as_str() != ip).cloned().collect();
    if on {
        v.push(ip.to_owned());
    }
    v
}

pub fn same_set(a: &[String], b: &[String]) -> bool {
    a.len() == b.len() && a.iter().all(|x| b.contains(x)) && b.iter().all(|x| a.contains(x))
}

pub fn strs(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn clock_sums() {
        let t = epoch("2026-10-13T08:05:00Z").unwrap();
        assert_eq!(date(t), "2026-10-13");
        assert_eq!(epoch("2026-10-13T08:05:00.412773021Z"), Some(t));
        assert_eq!(uptime("2026-10-13T08:05:00Z", t + 5 * 3600 + 12 * 60).as_deref(), Some("5h 12m"));
        assert_eq!(uptime("2026-10-13T08:05:00Z", t + 2 * 86_400 + 3 * 3600).as_deref(), Some("2d 3h"));
        assert_eq!(uptime("2026-10-13T08:05:00Z", t - 1), None);
        assert_eq!(uptime("", t), None);
        assert_eq!(ago(Lang::En, "2026-10-13T08:05:00Z", t + 30), "just now");
        assert_eq!(ago(Lang::En, "2026-10-13T08:05:00Z", t + 600), "10 min ago");
        assert_eq!(ago(Lang::Zh, "2026-10-13T08:05:00Z", t + 7200), "2 小时前");
        assert_eq!(ago(Lang::En, "2026-10-13T08:05:00Z", t + 30 * 86_400), "2026-10-13");
        assert_eq!(ago(Lang::En, "", t), "—");
    }

    #[test]
    fn tags_and_checks() {
        assert_eq!(tag("🚀 节点选择"), tag("🚀 节点选择"));
        assert_ne!(tag("a"), tag("b"));
        assert!(ipv4("192.168.0.5") && !ipv4("192.168.0.256") && !ipv4("a.b.c.d") && !ipv4("1.2.3") && !ipv4("1.2.3.4'"));
        assert!(sub_url_ok("https://x.example/sub?token=abc"));
        assert!(!sub_url_ok("ftp://x") && !sub_url_ok("https://x y") && !sub_url_ok("https://x'y"));
    }

    #[test]
    fn bypass_names_and_lists() {
        let c = json!({"hosts": {"02:00:00:00:00:01": {"name": "hint"}}, "dhcp_leases": [
            {"hostname": "", "macaddr": "02:00:00:00:00:01", "ipaddr": "192.168.0.10"},
            {"hostname": "phone", "macaddr": "02:00:00:00:00:02", "ipaddr": "192.168.0.11"}]});
        assert_eq!(client_name(&c, "192.168.0.10"), "hint");
        assert_eq!(client_name(&c, "192.168.0.11"), "phone");
        assert_eq!(client_name(&c, "192.168.0.99"), "192.168.0.99");
        let ips = vec!["192.168.0.11".to_owned(), "192.168.0.99".to_owned()];
        assert_eq!(bypass_rows(Some(&c), &ips), ["192.168.0.10", "192.168.0.11", "192.168.0.99"]);
        assert_eq!(bypass_next(&ips, "192.168.0.10", true), ["192.168.0.11", "192.168.0.99", "192.168.0.10"]);
        assert_eq!(bypass_next(&ips, "192.168.0.11", false), ["192.168.0.99"]);
        assert!(same_set(&ips, &["192.168.0.99".to_owned(), "192.168.0.11".to_owned()]));
    }
}
