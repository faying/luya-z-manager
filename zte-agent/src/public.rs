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
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("")
}

fn sh(cmd: &str) -> String {
    Command::new("sh")
        .args(["-c", cmd])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
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
    // ── Network (signal / type / operator / connection) ──
    // 多基带机型 (MU5252/TopFlow) 优先走 msim：单 modem 的 nwinfo_get_netinfo
    // 在这类设备上只返回主基带，若主基带恰好离线就会报成空信号。
    // U60 Pro 没有这个方法：第一次失败后记住，免得每次轮询都多起一个 ubus 进程。
    static NO_MSIM: AtomicBool = AtomicBool::new(false);
    let msim = if NO_MSIM.load(Ordering::Relaxed) {
        json!({})
    } else {
        ubus::call("zte_nwinfo_api", "nwinfo_get_msim_netinfo", Some("{}")).unwrap_or_else(|e| {
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

    let net = ubus::call("zte_nwinfo_api", "nwinfo_get_netinfo", Some("{}")).unwrap_or(json!({}));
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
    let wwan = ubus::call(
        "zwrt_data",
        "get_wwaniface",
        Some(r#"{"source_module":"zte_topsw_data","cid":1}"#),
    )
    .unwrap_or(json!({}));
    let connect_status = s(&wwan, "connect_status"); // e.g. ipv4_ipv6_connected
    let connected = connect_status.contains("connected") || online_links > 0;

    // ── Wi-Fi ──
    let wlan = ubus::call("zwrt_wlan", "report", Some("{}")).unwrap_or(json!({}));
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
    let ts_running = !sh("pidof tailscaled").is_empty();
    let ts_node = if ts_running {
        // first self line of `tailscale status`: "<100.x.y.z> <hostname> ..."
        let line = sh("/data/tailscale/tailscale --socket=/tmp/tailscaled.sock status 2>/dev/null | head -1");
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
            let model = sh("uci -q get zwrt_common_info.common_config.model_name");
            let market = sh("uci -q get zwrt_common_info.common_config.device_market_name");
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
    let cap = ubus::call("zwrt_wms", "zwrt_wms_get_wms_capacity", Some("{}")).unwrap_or(json!({}));
    let unread_i = |k: &str| -> i64 {
        cap.get(k)
            .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
            .unwrap_or(0)
    };
    let sms_unread = unread_i("sms_dev_unread_num") + unread_i("sms_sim_unread_num");

    (
        200,
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
        }),
    )
}
