// ─────────────────────────────────────────────────────────────────────────────
// public.rs — UNAUTHENTICATED, read-only status summary for the login screen.
//
// Mirrors the kind of pre-login info the stock ZTE web UI shows (network / Wi-Fi
// / battery) and adds our service health (Tailscale / Home Mode / …).
// LAN-only, no secrets — never include keys, IMEI, client lists, etc.
//
//   GET /api/public/status   (allow-listed past auth in server.rs)
// ─────────────────────────────────────────────────────────────────────────────

use crate::handlers::AppState;
use crate::ubus;
use serde_json::{json, Value};
use std::fs;
use std::net::IpAddr;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The login screen and the touch screen poll this; several viewers within a
/// few seconds share one answer instead of each starting ~8 processes.
const CACHE_TTL: Duration = Duration::from_secs(5);
/// One request's total budget for its ubus reads and commands. They answer
/// in milliseconds; this only cuts off a stuck one, so the HTTP worker (one
/// of two) is not held for a sum of 10 s ubus timeouts.
const DEADLINE: Duration = Duration::from_secs(6);
/// Per-step caps within [`DEADLINE`].
const UBUS_STEP_SECS: u32 = 3;
const CMD_STEP: Duration = Duration::from_secs(2);


fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("")
}

/// What is left of one request's [`DEADLINE`]. Each step gets the smaller of
/// its own cap and what is left; once nothing is left the step is skipped and
/// reads as empty, as a failed read always has.
struct Budget {
    end: Instant,
}

impl Budget {
    fn new(limit: Duration) -> Self {
        Budget { end: Instant::now() + limit }
    }

    fn left(&self) -> Duration {
        self.end.saturating_duration_since(Instant::now())
    }

    /// `ubus call` under the budget.
    fn ubus(&self, object: &str, method: &str, params: Option<&str>) -> Result<Value, String> {
        match step_secs(self.left(), UBUS_STEP_SECS) {
            Some(t) => ubus::read_with_timeout(object, method, params, Some(t)),
            None => Err("public status: out of time".into()),
        }
    }

    /// `sh -c cmd` under the budget; stdout trimmed, "" on any failure. A
    /// timeout kills the whole pipeline (`tailscale status | head`), not just
    /// the shell.
    fn sh(&self, cmd: &str) -> String {
        let limit = self.left().min(CMD_STEP);
        if limit < Duration::from_millis(100) {
            return String::new();
        }
        crate::ubus::output_within_group(Command::new("sh").args(["-c", cmd]), limit)
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    }
}

/// Whole seconds for `ubus -t`: at most `cap`, never more than is left, and
/// `None` below one second — `-t 0` would mean no timeout at all.
fn step_secs(left: Duration, cap: u32) -> Option<u32> {
    let t = left.as_secs().min(u64::from(cap)) as u32;
    (t >= 1).then_some(t)
}

/// One cached value, refreshed at most once per `ttl`. The lock is held while
/// refreshing, so callers that arrive meanwhile wait for that answer rather
/// than starting their own.
struct TtlCache<T> {
    slot: Mutex<Option<(Instant, T)>>,
}

impl<T: Clone> TtlCache<T> {
    const fn new() -> Self {
        TtlCache { slot: Mutex::new(None) }
    }

    fn get_or(&self, ttl: Duration, clock: impl Fn() -> Instant, refresh: impl FnOnce() -> T) -> T {
        // A panic inside refresh() leaves the old value, still usable.
        let mut slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        // Read the clock after the lock: a caller that waited out someone
        // else's refresh gets that fresh answer.
        let now = clock();
        if let Some((at, v)) = slot.as_ref() {
            if now.saturating_duration_since(*at) < ttl {
                return v.clone();
            }
        }
        let v = refresh();
        *slot = Some((clock(), v.clone()));
        v
    }
}

/// 一路基带的展示用摘要。
struct MsimLink {
    nettype: String,
    operator: String,
    bar: i64,
    rsrp: i64,
}

/// MU5252 这类多基带机型的 `nwinfo_get_msim_netinfo` 返回扁平前缀结构
/// (`msim_<a>_<b>_<field>`)，且只列出在线的调制解调器。
/// U60 Pro 等单 modem 机型没有这个方法 —— 用它做特性探测，不按机型硬编码。
fn msim_links(v: &Value) -> Vec<MsimLink> {
    let mut prefixes: Vec<String> = Vec::new();
    if let Some(map) = v.as_object() {
        for k in map.keys() {
            if let Some(rest) = k.strip_prefix("msim_") {
                let mut it = rest.split('_');
                if let (Some(a), Some(b)) = (it.next(), it.next()) {
                    let pre = format!("msim_{a}_{b}_");
                    if !prefixes.contains(&pre) {
                        prefixes.push(pre);
                    }
                }
            }
        }
    }
    prefixes.sort();
    prefixes
        .into_iter()
        .map(|pre| {
            let get = |f: &str| s(v, &format!("{pre}{f}")).to_string();
            let nettype = get("network_type");
            let operator = {
                let full = get("network_provider_fullname");
                if full.is_empty() { get("network_provider") } else { full }
            };
            let bar: i64 = get("signalbar").parse().unwrap_or(-1);
            let key = if nettype == "LTE" {
                format!("{pre}lte_rsrp")
            } else {
                format!("{pre}nr5g_rsrp")
            };
            let rsrp: i64 = v
                .get(&key)
                .and_then(|x| x.as_i64().or_else(|| x.as_str().and_then(|t| t.parse().ok())))
                .unwrap_or(0);
            MsimLink { nettype, operator, bar, rsrp }
        })
        .collect()
}

pub fn public_status(state: &AppState) -> (u16, Value) {
    static CACHE: TtlCache<Value> = TtlCache::new();
    let v = CACHE.get_or(CACHE_TTL, Instant::now, || build(state, &Budget::new(DEADLINE)));
    (200, v)
}

/// [`public_status`] for one HTTP request: adds `via_tailscale`, whether this
/// request came in over Tailscale — so the web can warn before a step that
/// may cut a remote viewer off. Per request, so never inside the shared cache.
pub fn public_status_for(state: &AppState, peer: Option<IpAddr>) -> (u16, Value) {
    let (status, mut v) = public_status(state);
    if let Some(obj) = v.as_object_mut() {
        obj.insert("via_tailscale".into(), json!(via_tailscale(peer)));
    }
    (status, v)
}

// ── Did this request come in over Tailscale? ────────────────────────────────
//
// Two ways it can:
//  * from a tailnet address: 100.64.0.0/10 (Tailscale's CGNAT range) or
//    fd7a:115c:a1e0::/48 — a phone or laptop on the tailnet opening the
//    device's 100.x address or MagicDNS name;
//  * from a LAN behind another node's subnet route (accept-routes is on by
//    default, scripts/tailscale/start.sh): the source is that LAN's own
//    address, but the kernel's route back to it is `dev tailscale0`.
// Blind spot: in userspace mode (no TUN) tailscaled proxies tailnet
// connections to the agent from loopback, which reads as the touch screen.

const TS_DEV: &str = "tailscale0";
/// One `ip route get` per viewer address per minute, not per 10 s poll.
const ROUTE_TTL: Duration = Duration::from_secs(60);
const ROUTE_CMD: Duration = Duration::from_secs(1);

/// `::ffff:a.b.c.d` → `a.b.c.d`; anything else unchanged.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    }
}

/// A Tailscale node address: 100.64.0.0/10 or fd7a:115c:a1e0::/48.
pub fn is_tailnet_addr(ip: IpAddr) -> bool {
    match canonical(ip) {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            o[0] == 100 && (o[1] & 0xc0) == 64
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0
        }
    }
}

/// The `dev` of an `ip route get` answer (`192.168.50.5 dev tailscale0 table 52
/// src 100.101.102.103 uid 0`).
fn route_dev(out: &str) -> Option<&str> {
    let mut it = out.split_whitespace();
    while let Some(w) = it.next() {
        if w == "dev" {
            return it.next();
        }
    }
    None
}

fn routed_via_tailscale(ip: IpAddr) -> bool {
    static CACHE: Mutex<Vec<(IpAddr, Instant, bool)>> = Mutex::new(Vec::new());
    let now = Instant::now();
    {
        let c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(&(_, _, v)) = c.iter().find(|(a, at, _)| *a == ip && now.saturating_duration_since(*at) < ROUTE_TTL) {
            return v;
        }
    }
    let out = crate::ubus::output_within(Command::new("ip").args(["route", "get", &ip.to_string()]), ROUTE_CMD)
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let v = route_dev(&out) == Some(TS_DEV);
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    c.retain(|(a, at, _)| *a != ip && now.saturating_duration_since(*at) < ROUTE_TTL);
    if c.len() >= 32 {
        c.remove(0);
    }
    c.push((ip, now, v));
    v
}

/// The pure part of [`via_tailscale`]; `routed` answers the route question.
fn via_tailscale_with(peer: Option<IpAddr>, routed: impl FnOnce(IpAddr) -> bool) -> bool {
    let Some(ip) = peer.map(canonical) else {
        return false;
    };
    if ip.is_loopback() || ip.is_unspecified() {
        return false;
    }
    is_tailnet_addr(ip) || routed(ip)
}

pub fn via_tailscale(peer: Option<IpAddr>) -> bool {
    via_tailscale_with(peer, routed_via_tailscale)
}

fn build(state: &AppState, budget: &Budget) -> Value {
    // ── Network (signal / type / operator / connection) ──
    // 多基带机型 (MU5252/TopFlow) 优先走 msim：单 modem 的 nwinfo_get_netinfo
    // 在这类设备上只返回主基带，若主基带恰好离线就会报成空信号。
    // U60 Pro 没有这个方法：第一次失败后记住，免得每次轮询都多起一个 ubus 进程。
    static NO_MSIM: AtomicBool = AtomicBool::new(false);
    let msim = if NO_MSIM.load(Ordering::Relaxed) {
        json!({})
    } else {
        budget.ubus("zte_nwinfo_api", "nwinfo_get_msim_netinfo", Some("{}")).unwrap_or_else(|e| {
            if e.contains("Method not found") {
                NO_MSIM.store(true, Ordering::Relaxed);
            }
            json!({})
        })
    };
    let links = msim_links(&msim);
    let online_links = links.iter().filter(|l| l.bar > 0).count();
    // 取信号最好的一路展示；bar 相同时比 rsrp（负值，越大越好）。
    let best = links.iter().filter(|l| l.bar > 0).max_by_key(|l| (l.bar, l.rsrp));

    let net = budget.ubus("zte_nwinfo_api", "nwinfo_get_netinfo", Some("{}")).unwrap_or(json!({}));
    let (nettype, operator, bar, rsrp) = match best {
        Some(l) => (l.nettype.clone(), l.operator.clone(), l.bar, l.rsrp),
        None => {
            // 单 modem 机型 (U60 Pro)，或多基带机型全部离线
            let t = s(&net, "network_type").to_string();
            let o = {
                let f = s(&net, "network_provider_fullname");
                if f.is_empty() { s(&net, "network_provider").to_string() } else { f.to_string() }
            };
            let b: i64 = s(&net, "signalbar").parse().unwrap_or(-1);
            let r: i64 = if t == "LTE" {
                net.get("lte_rsrp").and_then(|v| v.as_i64()).unwrap_or(0)
            } else {
                net.get("nr5g_rsrp").and_then(|v| v.as_i64()).unwrap_or(0)
            };
            (t, o, b, r)
        }
    };
    let wwan = budget.ubus("zwrt_data", "get_wwaniface",
        Some(r#"{"source_module":"zte_topsw_data","cid":1}"#),
    )
    .unwrap_or(json!({}));
    let connect_status = s(&wwan, "connect_status"); // e.g. ipv4_ipv6_connected
    let connected = connect_status.contains("connected") || online_links > 0;

    // ── Wi-Fi ──
    let wlan = budget.ubus("zwrt_wlan", "report", Some("{}")).unwrap_or(json!({}));
    let wifi_on = s(&wlan, "wifi_onoff") == "1";
    let ssid = {
        let a = s(&wlan, "main2g_ssid");
        if a.is_empty() { s(&wlan, "main5g_ssid") } else { a }
    };

    // ── Battery ──
    let bpct: i64 = fs::read_to_string("/sys/class/power_supply/battery/capacity")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(-1);
    let bstat = fs::read_to_string("/sys/class/power_supply/battery/status")
        .unwrap_or_default()
        .trim()
        .to_string();
    let charging = bstat.eq_ignore_ascii_case("Charging");

    // ── Services ──
    let ts_running = !budget.sh("pidof tailscaled").is_empty();
    let ts_node = if ts_running {
        // first self line of `tailscale status`: "<100.x.y.z> <hostname> ..."
        let line = budget.sh("/data/tailscale/tailscale --socket=/tmp/tailscaled.sock status 2>/dev/null | head -1");
        let mut it = line.split_whitespace();
        match (it.next(), it.next()) {
            (Some(ip), Some(name)) => format!("{ip} {name}"),
            _ => String::new(),
        }
    } else {
        String::new()
    };
    let ts_installed = std::path::Path::new("/data/tailscale/tailscale").exists();
    let hm_state = fs::read_to_string("/data/homemode/state").unwrap_or_default();
    let hm_mode = hm_state.split_whitespace().next().unwrap_or("normal").to_string();
    let hm_present = std::path::Path::new("/data/homemode.sh").exists();
    let hm_enabled = hm_present && !std::path::Path::new("/data/homemode/disabled").exists();

    // ── Device identity ──
    // 厂商把展示名写在 zwrt_common_info 里，U60 Pro 和 MU5252 都有这张表：
    //   model_name        MU5250      / MU5252
    //   device_market_name "U60 Pro"  / "TOPFLOW"
    // 出厂就定了，读一次缓存起来（读不到就下次再试）。
    static DEVICE: OnceLock<(String, String)> = OnceLock::new();
    let (dev_model, dev_name) = match DEVICE.get() {
        Some(d) => d.clone(),
        None => {
            let model = budget.sh("uci -q get zwrt_common_info.common_config.model_name");
            let market = budget.sh("uci -q get zwrt_common_info.common_config.device_market_name");
            // 厂商的市场名有的是全大写；已知品牌做个显示名映射，未知值原样透传。
            let name = match market.as_str() {
                "TOPFLOW" => "TopFlow".to_string(),
                "" => String::new(),
                other => other.to_string(),
            };
            if !model.is_empty() {
                let _ = DEVICE.set((model.clone(), name.clone()));
            }
            (model, name)
        }
    };

    // ── SMS (unread count, device + SIM storage) ──
    let cap = budget.ubus("zwrt_wms", "zwrt_wms_get_wms_capacity", Some("{}")).unwrap_or(json!({}));
    let unread_i = |k: &str| -> i64 {
        cap.get(k)
            .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
            .unwrap_or(0)
    };
    let sms_unread = unread_i("sms_dev_unread_num") + unread_i("sms_sim_unread_num");

    json!({
        "ok": true,
        "data": {
            "network": {
                "connected": connected,
                "type": nettype,
                "operator": operator,
                "bar": bar,
                "rsrp": rsrp,
                "links_online": online_links,
                // datad's home verdict code (ok / weak / stall …; null when datad is
                // silent), so the web can offer "Diagnose →" for the same states
                "verdict": crate::netwatch::verdict(),
            },
            "wifi": { "on": wifi_on, "ssid": ssid },
            "battery": { "percent": bpct, "charging": charging },
            "sms": { "unread": sms_unread },
            "device": { "model": dev_model, "name": dev_name },
            "services": {
                "tailscale": { "running": ts_running, "installed": ts_installed, "node": ts_node },
                "home_mode": { "present": hm_present, "enabled": hm_enabled, "mode": hm_mode },
            },
            "scenario": crate::scenario::public_summary(&state.scenario),
            // Count only — the events themselves need a login.
            "alerts": { "unread": crate::alerts::public_unread() },
            // Seconds the device clock runs ahead of UTC (clock.rs). The web
            // needs it to compare device times with the browser's clock.
            "clock": { "utc_offset": crate::clock::utc_offset() },
            // Counts only; the checks themselves need a login (/api/health).
            "health": crate::health::public_summary(),
        }
    })
}


#[cfg(test)]
mod cache_tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;

    #[test]
    fn step_secs_never_asks_ubus_for_no_timeout() {
        assert_eq!(step_secs(Duration::from_secs(10), 3), Some(3));
        assert_eq!(step_secs(Duration::from_millis(2500), 3), Some(2));
        assert_eq!(step_secs(Duration::from_millis(999), 3), None, "-t 0 waits forever");
        assert_eq!(step_secs(Duration::ZERO, 3), None);
    }

    #[test]
    fn cache_answers_from_memory_within_the_ttl() {
        let c: TtlCache<u32> = TtlCache::new();
        let t0 = Instant::now();
        let now = Cell::new(t0);
        let runs = Cell::new(0);
        let get = || c.get_or(CACHE_TTL, || now.get(), || { runs.set(runs.get() + 1); runs.get() });
        assert_eq!(get(), 1);
        now.set(t0 + Duration::from_millis(4900));
        assert_eq!(get(), 1, "still fresh");
        now.set(t0 + CACHE_TTL);
        assert_eq!(get(), 2, "stale at the TTL");
        now.set(t0 + CACHE_TTL + Duration::from_secs(1));
        assert_eq!(get(), 2);
        assert_eq!(runs.get(), 2);
    }

    #[test]
    fn concurrent_callers_share_one_refresh() {
        let c: Arc<TtlCache<u32>> = Arc::new(TtlCache::new());
        let runs = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let (c, runs) = (Arc::clone(&c), Arc::clone(&runs));
                std::thread::spawn(move || {
                    c.get_or(CACHE_TTL, Instant::now, || {
                        std::thread::sleep(Duration::from_millis(200));
                        runs.fetch_add(1, Ordering::SeqCst) as u32 + 1
                    })
                })
            })
            .collect();
        let got: Vec<u32> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert!(got.iter().all(|&v| v == 1), "{got:?}");
    }

    #[test]
    fn a_refresh_that_panicked_does_not_break_the_cache() {
        let c: TtlCache<u32> = TtlCache::new();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            c.get_or(CACHE_TTL, Instant::now, || panic!("ubus exploded"))
        }));
        assert!(r.is_err());
        assert_eq!(c.get_or(CACHE_TTL, Instant::now, || 7), 7);
    }

    #[test]
    fn commands_stay_within_the_budget() {
        assert_eq!(Budget::new(DEADLINE).sh("echo hi | cat"), "hi");
        assert_eq!(Budget::new(Duration::ZERO).sh("echo hi"), "", "out of time: skipped");
        let t0 = Instant::now();
        assert_eq!(Budget::new(Duration::from_millis(300)).sh("sleep 5 | cat"), "");
        assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
        assert!(Budget::new(Duration::ZERO).ubus("zwrt_wlan", "report", None).unwrap_err().contains("out of time"));
    }
}
