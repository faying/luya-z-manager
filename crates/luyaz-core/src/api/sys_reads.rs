//! System reads moved from the agent: CPU per core, kernel, radio on/off, USB, SMS capacity, operator IPv6.
//!
//! u60-web.md 8d-1b: the agent's reads luyaz-web still used, served here at the agent's path with
//! the agent's reply shape (`{ok, data}`), so luyaz-web only changes where it asks. On demand,
//! off the collection tick (spawn_blocking / a timeout), never cached across requests unless said.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use axum::{extract::State, response::Response};
use serde_json::{Value, json};

use super::{Compat, reply};

const UBUS_TIMEOUT: Duration = Duration::from_secs(8);
const FILE_TIMEOUT: Duration = Duration::from_secs(5);

fn respond((status, body): (u16, Value)) -> Response {
    reply(status, body)
}

fn fail(e: impl std::fmt::Display) -> (u16, Value) {
    (503, json!({"ok": false, "error": e.to_string()}))
}

pub(super) async fn ubus(
    c: &Compat,
    object: &'static str,
    method: &'static str,
    args: Value,
) -> Result<Value, String> {
    match tokio::time::timeout(UBUS_TIMEOUT, (c.src.ubus)(object, method, args)).await {
        Ok(r) => r,
        Err(_) => Err(format!("ubus {object} {method}: timed out")),
    }
}

// ---- CPU ----

#[derive(Clone, Copy)]
pub(super) struct CpuSample {
    id: usize,
    user: u64,
    nice: u64,
    system: u64,
    idle: u64,
    iowait: u64,
    irq: u64,
    softirq: u64,
}

impl CpuSample {
    fn total(&self) -> u64 {
        self.user + self.nice + self.system + self.idle + self.iowait + self.irq + self.softirq
    }

    fn busy(&self) -> u64 {
        self.total() - self.idle - self.iowait
    }
}

/// The previous /proc/stat sample, kept between requests: usage is a delta (first answer is 0).
static PREV_CPU: Mutex<Vec<CpuSample>> = Mutex::new(Vec::new());

pub(super) fn parse_cpu_samples(stat: &str) -> Vec<CpuSample> {
    let mut samples = Vec::new();
    for line in stat.lines() {
        // "cpu0", "cpu1", … but not the aggregate "cpu " line
        if line.starts_with("cpu") && line.as_bytes().get(3).is_some_and(|b| b.is_ascii_digit()) {
            let mut words = line.split_whitespace();
            let id = words
                .next()
                .and_then(|w| w[3..].parse().ok())
                .unwrap_or(usize::MAX);
            let p: Vec<u64> = words.filter_map(|s| s.parse().ok()).collect();
            if p.len() >= 7 {
                samples.push(CpuSample {
                    id,
                    user: p[0],
                    nice: p[1],
                    system: p[2],
                    idle: p[3],
                    iowait: p[4],
                    irq: p[5],
                    softirq: p[6],
                });
            }
        }
    }
    samples
}

fn read_trim(path: &Path) -> Option<String> {
    let s = fs::read_to_string(path).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

fn read_num<T: std::str::FromStr>(path: &Path) -> Option<T> {
    read_trim(path)?.parse().ok()
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// Per-core online state and frequency from `root` (/sys/devices/system/cpu).
fn core_info(root: &Path, usage_by_id: &HashMap<usize, f64>) -> Vec<Value> {
    let mut ids: Vec<usize> = fs::read_dir(root)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().to_str()?.strip_prefix("cpu")?.parse().ok())
                .collect()
        })
        .unwrap_or_default();
    ids.sort_unstable();
    ids.into_iter()
        .map(|id| {
            let dir = root.join(format!("cpu{id}"));
            // cpu0 often has no `online` node: it cannot be taken offline
            let online = read_num::<u8>(&dir.join("online")).is_none_or(|v| v != 0);
            let mhz = |name: &str| {
                read_num::<u64>(&dir.join("cpufreq").join(name)).map(|k| (k / 1000) as u32)
            };
            json!({
                "id": id,
                "online": online,
                "usage": if online { usage_by_id.get(&id).copied() } else { None },
                "freq_mhz": if online { mhz("scaling_cur_freq") } else { None },
                "max_mhz": mhz("cpuinfo_max_freq"),
            })
        })
        .collect()
}

/// The agent's `CpuTracker::sample`, with the previous sample passed in.
pub(super) fn cpu_usage(prev: &mut Vec<CpuSample>, current: Vec<CpuSample>, sysfs: &Path) -> Value {
    let mut cores = Vec::new();
    let mut usage_by_id = HashMap::new();
    let (mut total_busy, mut total_all) = (0u64, 0u64);
    for (i, cur) in current.iter().enumerate() {
        match prev.get(i) {
            Some(old) => {
                let dt = cur.total().saturating_sub(old.total());
                let db = cur.busy().saturating_sub(old.busy());
                if dt > 0 {
                    let pct = round1(db as f64 / dt as f64 * 100.0);
                    cores.push(pct);
                    if old.id == cur.id {
                        usage_by_id.insert(cur.id, pct);
                    }
                    total_busy += db;
                    total_all += dt;
                } else {
                    cores.push(0.0);
                }
            }
            None => cores.push(0.0),
        }
    }
    let overall = if total_all > 0 {
        round1(total_busy as f64 / total_all as f64 * 100.0)
    } else {
        0.0
    };
    *prev = current;
    json!({"cores": cores, "overall": overall, "per_core": core_info(sysfs, &usage_by_id)})
}

/// GET /api/cpu
pub async fn cpu(State(c): State<Compat>) -> Response {
    respond(cpu_reply(&c).await)
}

async fn cpu_reply(c: &Compat) -> (u16, Value) {
    let (stat, sysfs) = (c.path("/proc/stat"), c.path("/sys/devices/system/cpu"));
    let job = tokio::task::spawn_blocking(move || {
        let current = fs::read_to_string(stat)
            .map(|s| parse_cpu_samples(&s))
            .unwrap_or_default();
        let mut prev = PREV_CPU.lock().unwrap_or_else(|e| e.into_inner());
        cpu_usage(&mut prev, current, &sysfs)
    });
    match tokio::time::timeout(FILE_TIMEOUT, job).await {
        Ok(Ok(v)) => (200, json!({"ok": true, "data": v})),
        _ => fail("cpu read failed"),
    }
}

// ---- device ----

pub(super) fn device_info(hostname: &str, uptime: &str, loadavg: &str, version: &str) -> Value {
    let uptime_secs = uptime
        .split_whitespace()
        .next()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0) as u64;
    let l: Vec<f64> = loadavg
        .split_whitespace()
        .take(3)
        .filter_map(|s| s.parse().ok())
        .collect();
    let at = |i: usize| l.get(i).copied().unwrap_or(0.0);
    json!({
        "hostname": hostname.trim(),
        "uptime_secs": uptime_secs,
        "load_avg": [at(0), at(1), at(2)],
        "kernel": version.trim(),
    })
}

/// GET /api/device
pub async fn device(State(c): State<Compat>) -> Response {
    respond(device_reply(&c).await)
}

async fn device_reply(c: &Compat) -> (u16, Value) {
    let (h, u, l, v) = (
        c.path("/proc/sys/kernel/hostname"),
        c.path("/proc/uptime"),
        c.path("/proc/loadavg"),
        c.path("/proc/version"),
    );
    let job = tokio::task::spawn_blocking(move || {
        let rd = |p| fs::read_to_string(p).unwrap_or_default();
        device_info(&rd(h), &rd(u), &rd(l), &rd(v))
    });
    match tokio::time::timeout(FILE_TIMEOUT, job).await {
        Ok(Ok(v)) => (200, json!({"ok": true, "data": v})),
        _ => fail("device read failed"),
    }
}

// ---- battery ----

/// The agent's `system::read_battery_at` on `<root>/battery` and `<root>/usb`: the first five
/// fields keep their old types (0 when a node is missing), the rest are null when missing;
/// `charger` is null without a usb directory. None only without a battery status (503).
pub(super) fn battery_at(root: &Path) -> Option<Value> {
    let bat = root.join("battery");
    let status = read_trim(&bat.join("status"))?;
    let num = |name: &str| read_num::<i64>(&bat.join(name));
    let usb = root.join("usb");
    let charger = usb.is_dir().then(|| {
        json!({
            "online": read_num::<u8>(&usb.join("online")).map(|v| v != 0),
            "voltage_uv": read_num::<i64>(&usb.join("voltage_now")),
            "current_ua": read_num::<i64>(&usb.join("current_now")),
            "input_current_limit_ua": read_num::<i64>(&usb.join("input_current_limit")),
        })
    });
    Some(json!({
        "status": status,
        "capacity": num("capacity").unwrap_or(0),
        "voltage_uv": num("voltage_now").unwrap_or(0),
        "current_ua": num("current_now").unwrap_or(0),
        "temperature": num("temp").unwrap_or(0),
        "charge_full_uah": num("charge_full"),
        "charge_full_design_uah": num("charge_full_design"),
        "charge_counter_uah": num("charge_counter"),
        "cycle_count": num("cycle_count"),
        "health": read_trim(&bat.join("health")),
        "voltage_max_uv": num("voltage_max"),
        "charger": charger,
    }))
}

/// GET /api/battery (the agent's `handlers::battery`): the supply nodes plus u60d's own battery
/// estimate (what the agent took from u60d's `/api/screen` once `battery_eta` was handed over).
pub async fn battery(State(c): State<Compat>) -> Response {
    respond(battery_reply(&c).await)
}

pub(super) async fn battery_reply(c: &Compat) -> (u16, Value) {
    let root = c.path("/sys/class/power_supply");
    let job = tokio::task::spawn_blocking(move || battery_at(&root));
    match tokio::time::timeout(FILE_TIMEOUT, job).await {
        Ok(Ok(Some(mut data))) => {
            data["estimate"] = crate::plugin::battery_eta::report();
            (200, json!({"ok": true, "data": data}))
        }
        _ => (
            503,
            json!({"ok": false, "error": "battery info not available"}),
        ),
    }
}

// ---- modem ----

/// GET /api/modem/status
pub async fn modem_status(State(c): State<Compat>) -> Response {
    respond(modem_reply(&c).await)
}

pub(super) async fn modem_reply(c: &Compat) -> (u16, Value) {
    let args = json!({"config": "zte_nwinfo", "section": "sys_info", "option": "operate_mode"});
    match ubus(c, "uci", "get", args).await {
        Ok(v) => match v["value"].as_str() {
            Some(mode) => (
                200,
                json!({"ok": true, "data": {"operate_mode": mode.trim()}}),
            ),
            None => fail("uci get zte_nwinfo.sys_info.operate_mode: no value"),
        },
        Err(e) => fail(e),
    }
}

// ---- USB ----

/// `ubus -v list` prints a `'<object>' @<id>` header, then one `"<method>":{<args>}` line per
/// method. Output without that header is not a listing we understand: None, not "no such method".
pub(super) fn lists_method(listing: &str, object: &str, method: &str) -> Option<bool> {
    let header = format!("'{object}' ");
    if !listing.lines().any(|l| l.starts_with(&header)) {
        return None;
    }
    let want = format!("\"{method}\":");
    Some(listing.lines().any(|l| l.trim_start().starts_with(&want)))
}

/// Whether the firmware offers the USB mode setter. None when it cannot be asked (including
/// read-only mode, which refuses `ubus list`).
pub(super) async fn usb_settable() -> Option<bool> {
    let out = crate::command::run(
        &crate::ubus::backend::cli_bin(),
        ["-v", "list", "zwrt_bsp.usb"],
        Duration::from_secs(5),
    )
    .await
    .ok()?;
    lists_method(&String::from_utf8_lossy(&out), "zwrt_bsp.usb", "set")
}

pub(super) fn usb_data(mut data: Value, settable: Option<bool>) -> Value {
    if let (Some(obj), Some(s)) = (data.as_object_mut(), settable) {
        obj.insert("mode_settable".into(), json!(s));
    }
    data
}

/// GET /api/usb/status
pub async fn usb_status(State(c): State<Compat>) -> Response {
    respond(usb_reply(&c).await)
}

async fn usb_reply(c: &Compat) -> (u16, Value) {
    match ubus(c, "zwrt_bsp.usb", "list", json!({})).await {
        Ok(data) => {
            let settable = usb_settable().await;
            (200, json!({"ok": true, "data": usb_data(data, settable)}))
        }
        Err(e) => fail(e),
    }
}

// ---- SMS capacity ----

/// GET /api/sms/capacity
pub async fn sms_capacity(State(c): State<Compat>) -> Response {
    respond(sms_capacity_reply(&c).await)
}

async fn sms_capacity_reply(c: &Compat) -> (u16, Value) {
    match ubus(c, "zwrt_wms", "zwrt_wms_get_wms_capacity", json!({})).await {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => fail(e),
    }
}

// ---- WAN IPv6 ----

/// PDP type 2 = IPv6, 3 = IPv4v6 (3GPP); `wan_has_ipv6` is the live PDN, which can lag the config.
pub(super) fn wan_ipv6_data(apn: &Value, wan: Option<&Value>) -> Value {
    let pdp = apn["pdpType"].as_i64().unwrap_or(0);
    let wan_has_ipv6 = wan
        .and_then(|w| w["ipv6_address"].as_str())
        .is_some_and(|s| !s.is_empty());
    json!({
        "ipv6_enabled": pdp == 2 || pdp == 3,
        "pdp_type": pdp,
        "wan_has_ipv6": wan_has_ipv6,
    })
}

/// GET /api/router/wan-ipv6
pub async fn wan_ipv6(State(c): State<Compat>) -> Response {
    respond(wan_ipv6_reply(&c).await)
}

async fn wan_ipv6_reply(c: &Compat) -> (u16, Value) {
    let apn = match ubus(c, "zwrt_apn_object", "get_apn_at_cid", json!({"cid": 1})).await {
        Ok(v) => v,
        Err(e) => return fail(e),
    };
    let wan = ubus(
        c,
        "zwrt_data",
        "get_wwaniface",
        json!({"source_module": "zte_topsw_data", "cid": 1}),
    )
    .await
    .ok();
    (
        200,
        json!({"ok": true, "data": wan_ipv6_data(&apn, wan.as_ref())}),
    )
}
