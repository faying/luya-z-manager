//! The exit lookups (the agent's `netinfo::keep_exits`, 8d-1c ⑥), next to `/api/netinfo`.
//!
//! While someone is looking (the netinfo refresh touches [`ni::EXITS_WANTED`], fresh for
//! [`ni::EXITS_WANTED_SECS`]) a thread of its own looks the exits up every [`PASS_SECS`] when they
//! are due, and writes [`ni::EXITS_FILE`] when the answer changed: the same file, so
//! `compat/netinfo.rs` reads it as before. The direct exit is asked again when the default route
//! (interface + address) changes or the answer is old.
//! Blocking HTTP (5 s per service, several services), so never on the runtime's thread.

use serde_json::json;
use std::time::Duration;
use luyaz_api::netinfo::{self as ni, Config, Geo, Lookup, Slot};

use super::Compat;

/// The agent's `PASS_MIN_GAP`.
const PASS_SECS: u64 = 10;
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Asks `urls` in order (through the proxy on `proxy_port` if any) until one answers with
/// something `parse_geo` understands: `(geo, host that answered)`.
fn lookup(urls: &[String], proxy_port: Option<u16>) -> Result<(Geo, String), String> {
    let proxy = match proxy_port {
        Some(p) => Some(
            ureq::Proxy::new(&format!("http://127.0.0.1:{p}")).map_err(|e| e.to_string())?,
        ),
        None => None,
    };
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(LOOKUP_TIMEOUT))
        // the cellular side has IPv6 too; myip.ipip.net then answers with the v6 address
        .ip_family(ureq::config::IpFamily::Ipv4Only)
        .proxy(proxy)
        .build()
        .into();
    let mut last_err = String::from("没有可用的查询地址");
    for url in urls {
        let host = url
            .split("://")
            .nth(1)
            .and_then(|r| r.split('/').next())
            .unwrap_or(url)
            .to_string();
        let text = agent
            .get(url)
            .header("User-Agent", "curl/8")
            .call()
            .map_err(|e| e.to_string())
            .and_then(|mut r| r.body_mut().read_to_string().map_err(|e| e.to_string()));
        match text {
            Ok(t) => match ni::parse_geo(&t) {
                Some(g) => return Ok((g, host)),
                None => last_err = format!("{host}: 看不懂返回"),
            },
            Err(e) => last_err = format!("{host}: {e}"),
        }
    }
    Err(last_err)
}

/// The main table's default route as `"<iface> <ipv4>"` (empty: none): it changes when the
/// cellular session is set up again, and so (usually) does the public address.
pub(super) fn route_key(route_table: &str, ipv4_of: impl Fn(&str) -> Option<String>) -> String {
    let iface = route_table.lines().skip(1).find_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        (f.len() > 7 && f[1] == "00000000" && f[7] == "00000000").then(|| f[0].to_string())
    });
    let Some(iface) = iface else {
        return String::new();
    };
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
                    out = Some(
                        std::net::Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr)).to_string(),
                    );
                    break;
                }
            }
            p = ifa.ifa_next;
        }
        libc::freeifaddrs(head);
    }
    out
}


/// The thread's slots and what it last wrote.
#[derive(Default)]
pub(super) struct Exits {
    direct: Slot,
    written: String,
}

/// One pass at `t` (the agent's `lookup_exits`): the lookups that are due, then the file.
pub(super) fn pass(c: &Compat, x: &mut Exits, t: i64, route_table: &str) {
    let cfg = Config::parse(&std::fs::read_to_string(c.path(ni::CONF_PATH)).unwrap_or_default());
    if cfg.enabled {
        let dkey = route_key(route_table, ipv4_of);
        if !dkey.is_empty() && x.direct.due(&dkey, t) {
            let res = lookup(&cfg.direct, None);
            x.direct.store(&dkey, t, res, None);
        }
    }
    let text = json!({
        "direct": x.direct.value.as_ref().map(Lookup::json),
    })
    .to_string();
    if text != x.written && super::ops::atomic_write(&c.path(ni::EXITS_FILE), text.as_bytes()).is_ok() {
        x.written = text;
    }
}

/// Someone looked within [`ni::EXITS_WANTED_SECS`].
fn wanted(c: &Compat) -> bool {
    std::fs::metadata(c.path(ni::EXITS_WANTED))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|m| m.elapsed().ok())
        .is_some_and(|age| age.as_secs() <= ni::EXITS_WANTED_SECS)
}

/// The thread: while someone is looking, a pass every [`PASS_SECS`].
pub fn spawn(c: Compat) {
    let _ = std::thread::Builder::new()
        .name("u60d-exits".into())
        .spawn(move || {
            let mut x = Exits::default();
            loop {
                std::thread::sleep(Duration::from_secs(PASS_SECS));
                if wanted(&c) {
                    let route = std::fs::read_to_string(c.path("/proc/net/route")).unwrap_or_default();
                    pass(&c, &mut x, now(), &route);
                }
            }
        });
}
