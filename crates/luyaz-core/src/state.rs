use crate::project::snapshot::{self, *};
use crate::{command, model::Snapshot};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    ffi::CString,
    fs,
    path::Path,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

// ── Per-call cache (MU5250 power) ──────────────────────────────────────────
//
// Every round used to run ~26 `ubus call`s and 12 `uci show`s, serially, each
// a fork+exec — and many vendor handlers fork helpers of their own, so on a
// U60 Pro (MU5250) stopping core dropped the device-wide fork rate from ~180–
// 300/s to ~57/s. Most of that data barely moves (IMEI, board, config
// packages, data limits), so each call gets a time-to-live and is reused until
// it expires. Anything the user changes through /control clears the whole
// cache (`invalidate_cache`), so the screen still reacts at once.
// LUYAZ_CACHE=0 turns all of it off.

struct Cached<T> {
    at: Instant,
    ttl: Duration,
    value: T,
}

/// A lazily created, process-wide TTL cache keyed by string.
type Cache<T> = Mutex<Option<HashMap<String, Cached<T>>>>;

static UBUS_CACHE: Cache<Result<Value, String>> = Mutex::new(None);
static UCI_CACHE: Cache<BTreeMap<String, String>> = Mutex::new(None);

/// `/v2` battery 块的 data = 旧 `/state` 的 `battery`（充电字段取充电器块最近一次读成功的回复）。
pub(crate) fn battery_v2(raw: &Value, other: &dyn Fn(&str) -> Option<Value>) -> Value {
    battery_object(raw, &other("charger").unwrap_or(Value::Null), read_rails())
}

/// `/v2` charger 块的 data = 旧 `/state` 的 `power`；旧 `/state` 没有 `power` 时是 `{}`。
pub(crate) fn charger_v2(raw: &Value, _other: &dyn Fn(&str) -> Option<Value>) -> Value {
    power_object(raw)
        .unwrap_or_else(|| crate::contract::value(&crate::contract::Charger::default()))
}

pub(crate) fn cache_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("LUYAZ_CACHE").as_deref() != Ok("0"))
}

/// Drop every cached result. Called after each /control action.
pub fn invalidate_cache() {
    if let Ok(mut c) = UBUS_CACHE.lock() {
        *c = None;
    }
    if let Ok(mut c) = UCI_CACHE.lock() {
        *c = None;
    }
    crate::qos::invalidate();
}

/// 短信事件（V2-31）：只丢掉短信容量和短信列表的缓存，下一次旧采集立即重读它们。
pub fn invalidate_sms_cache() {
    if let Ok(mut c) = UBUS_CACHE.lock()
        && let Some(map) = c.as_mut()
    {
        map.retain(|k, _| !is_sms_key(k));
    }
}

fn is_sms_key(key: &str) -> bool {
    key.starts_with("zwrt_wms\u{0}zwrt_wms_get_wms_capacity\u{0}")
        || key.starts_with("zwrt_wms\u{0}zte_libwms_get_sms_data\u{0}")
}

fn cache_get<T: Clone>(cache: &Cache<T>, key: &str) -> Option<T> {
    let c = cache.lock().ok()?;
    let e = c.as_ref()?.get(key)?;
    (e.at.elapsed() < e.ttl).then(|| e.value.clone())
}

fn cache_put<T>(cache: &Cache<T>, key: String, ttl: Duration, value: T) {
    if let Ok(mut c) = cache.lock() {
        c.get_or_insert_with(HashMap::new).insert(
            key,
            Cached {
                at: Instant::now(),
                ttl,
                value,
            },
        );
    }
}

/// `ubus` with a time-to-live. Failures are kept for at most 5 s: a call that
/// does not exist on this model should not fork every second, but a transient
/// error must not stick.
/// APN 块的四个读（10 分钟缓存；写过之后 `invalidate_cache`、数据卡换了（ICCID 变）都让这一轮立即读）。
/// 任何一个失败整块 stale。不再 60 秒一读：厂商每读一次往 key.log 写约 30 行，
/// 10-22 一天十几 MB，把承载记录（QoS）和别的诊断记录都冲掉了。
/// 候选列表要按数据卡所在的槽读（`apn_list_args`）：不带 slotId 读到的总是卡 1 的。
async fn apn_reads(card: &str, slot2: bool) -> Result<[Value; 4], String> {
    const O: &str = "zwrt_apn_object";
    static CARD: Mutex<String> = Mutex::new(String::new());
    if let Ok(mut seen) = CARD.lock()
        && *seen != card
    {
        *seen = card.to_owned();
        if let Ok(mut c) = UBUS_CACHE.lock()
            && let Some(map) = c.as_mut()
        {
            map.retain(|k, _| !k.starts_with(&format!("{O}\u{0}")));
        }
    }
    Ok([
        ubus_ttl(600, O, "get_apn_mode", json!({})).await?,
        ubus_ttl(600, O, "get_apn_at_cid", json!({"cid": 1})).await?,
        ubus_ttl(600, O, "get_manu_apn_list", json!({})).await?,
        ubus_ttl(600, O, "get_auto_apn_list", apn_list_args(slot2)).await?,
    ])
}

/// router 块（第四批 4-0）的读，顺序同 `snapshot::router_block`：六个 `zwrt_router.api` 读、
/// 再两个 uci（带宽分配模式、xdpi）。60 秒缓存；写过之后 `invalidate_cache` 让下一轮立即读。
/// 各读各的：一个失败只空它那一部分。
async fn router_reads() -> [Result<Value, String>; 8] {
    let r = "zwrt_router.api";
    let uci = |section: &str, option: &str| json!({"config": "zwrt_smart_mng", "section": section, "option": option});
    [
        ubus_ttl(60, r, "router_get_firewall_para", json!({})).await,
        ubus_ttl(60, r, "router_get_upnp", json!({})).await,
        ubus_ttl(60, r, "router_get_portforward_rule", json!({})).await,
        ubus_ttl(60, r, "router_get_macipport_filter_rule", json!({})).await,
        ubus_ttl(60, r, "router_get_alg_para", json!({})).await,
        ubus_ttl(60, r, "router_get_domainfilter_rule", json!({})).await,
        ubus_ttl(60, "uci", "get", uci("smart_qos", "mode")).await,
        ubus_ttl(60, "uci", "get", uci("smart_mng", "xdpi_support")).await,
    ]
}

async fn ubus_ttl(ttl_s: u64, service: &str, method: &str, args: Value) -> Result<Value, String> {
    ubus_ttl_read(ttl_s, service, method, args).await.0
}

/// `ubus_ttl`，另外回答这次的结果算不算「读到了」（`/v2` 派生块的健康用）。
///
/// 执行者这次没发请求（`Skipped`：对象本轮已超时、在跨轮冷却里、或本轮超时的对象已到
/// `executor::LEGACY_TIMEOUT_LIMIT`，V2-19）时，不覆盖缓存里上一次的结果；有 TTL 的读取有上一次读成功的值
/// 就接着用（旧 `/state` 的字段不丢），但不算读到（块按读失败、照 V2-12 置 stale）。
/// TTL 为 0 的读取（`system info`、`nwinfo`、实时流量）没有上一次的值，照旧按读失败。
async fn ubus_ttl_read(
    ttl_s: u64,
    service: &str,
    method: &str,
    args: Value,
) -> (Result<Value, String>, bool) {
    ubus_ttl_read_inner(ttl_s, service, method, args, None).await
}

async fn profile_ubus(
    hub: &crate::block::Hub,
    ttl_s: u64,
    service: &str,
    method: &str,
) -> Result<Value, String> {
    ubus_ttl_read_inner(ttl_s, service, method, json!({}), Some(hub.firmware()))
        .await
        .0
}

async fn ubus_ttl_read_inner(
    ttl_s: u64,
    service: &str,
    method: &str,
    args: Value,
    profile: Option<&crate::device_profile::collect::Collector>,
) -> (Result<Value, String>, bool) {
    // 旧采集里的安全点：这里不持有任何锁，排队的 /control 可以先做（V2-24）。
    crate::executor::preempt().await;
    let cached = ttl_s != 0 && cache_enabled();
    let key = ubus_key(service, method, &args);
    if cached && let Some(v) = cache_get(&UBUS_CACHE, &key) {
        return (v, true);
    }
    let token = profile.map(|p| p.token());
    let v = crate::executor::call(service, method, &args).await;
    if let Err(e @ crate::ubus::client::UbusError::Skipped { .. }) = &v {
        let previous = if cached {
            cache_last_ok(&UBUS_CACHE, &key)
        } else {
            None
        };
        return (previous.ok_or_else(|| e.to_string()), false);
    }
    if let Some((profile, token)) = profile.zip(token) {
        profile.ubus(
            token,
            luyaz_api::clock::BootTime::read(),
            service,
            method,
            v.as_ref().ok(),
        );
    }
    let v = v.map_err(|e| e.to_string());
    if cached {
        let ttl = if v.is_ok() { ttl_s } else { ttl_s.min(5) };
        cache_put(&UBUS_CACHE, key, Duration::from_secs(ttl), v.clone());
    }
    let fresh = v.is_ok();
    (v, fresh)
}

fn ubus_key(service: &str, method: &str, args: &Value) -> String {
    format!("{service}\u{0}{method}\u{0}{args}")
}

/// 缓存里这个键上一次读成功的值，过期了也给（`ubus_ttl_read` 没发请求时用）。
fn cache_last_ok(cache: &Cache<Result<Value, String>>, key: &str) -> Option<Value> {
    let c = cache.lock().ok()?;
    c.as_ref()?.get(key)?.value.as_ref().ok().cloned()
}

/// 按块间隔读一个 UCI 包（R19）：`ttl_s` 就是间隔（阶段 2 迁完之前沿用 UCI_CACHE 的 TTL）。
async fn uci_show_ttl(ttl_s: u64, package: &str) -> BTreeMap<String, String> {
    uci_show_ttl_inner(ttl_s, package, None).await
}

async fn uci_show_ttl_inner(
    ttl_s: u64,
    package: &str,
    profile: Option<&crate::device_profile::collect::Collector>,
) -> BTreeMap<String, String> {
    // 和 ubus_ttl 一样的安全点：排队的 /control 先做，这里读到的是控制之后的值（V2-24/V2-28）。
    crate::executor::preempt().await;
    let token = profile.map(|p| p.token());
    if ttl_s == 0 || !cache_enabled() {
        let v = uci_show(package).await;
        if let Some((p, token)) = profile.zip(token) {
            p.uci(token, package, &v, luyaz_api::clock::BootTime::read());
        }
        return v;
    }
    if let Some(v) = cache_get(&UCI_CACHE, package) {
        return v;
    }
    let v = uci_show(package).await;
    if let Some((p, token)) = profile.zip(token) {
        p.uci(token, package, &v, luyaz_api::clock::BootTime::read());
    }
    cache_put(
        &UCI_CACHE,
        package.to_string(),
        Duration::from_secs(ttl_s),
        v.clone(),
    );
    v
}

pub(crate) fn uci_bin() -> String {
    std::env::var("LUYAZ_UCI_BIN").unwrap_or_else(|_| "/sbin/uci".into())
}

/// `/etc/config`（`LUYAZ_UCI_CONFIG_DIR` 可改，测试用）。
fn uci_config_dir() -> std::path::PathBuf {
    std::env::var("LUYAZ_UCI_CONFIG_DIR")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| host_path("/etc/config"))
        .into()
}
/// uci 的 savedir `/tmp/.uci`（未保存改动；`LUYAZ_UCI_SAVEDIR` 可改，测试用）。
fn uci_save_dir() -> std::path::PathBuf {
    std::env::var("LUYAZ_UCI_SAVEDIR")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| host_path("/tmp/.uci"))
        .into()
}

/// R19：`/tmp/.uci/<包>` 不存在或为 0 字节（设备上 commit 后留下的空文件）且配置文件能解析时，
/// 核心自己解析 `/etc/config/<包>`，不 fork；否则返回 `None`，调用方退回 `uci` 子进程。
/// `LUYAZ_UCI_PARSE=0` 一律走子进程（回退开关）。
pub(crate) fn uci_parsed(package: &str) -> Option<crate::uci::Package> {
    if std::env::var("LUYAZ_UCI_PARSE").as_deref() == Ok("0") {
        return None;
    }
    crate::uci::load(package, &uci_config_dir(), &uci_save_dir())
}

/// `uci show` 文本 → `路径 → 值`（值去掉最外层单引号）。解析路径和子进程路径共用。
fn uci_show_map(raw: &[u8]) -> BTreeMap<String, String> {
    let text = String::from_utf8_lossy(raw);
    let mut out = BTreeMap::new();
    for entry in text.lines() {
        let Some(eq) = entry.find('=') else { continue };
        let (path, rest) = (&entry[..eq], &entry[eq + 1..]);
        // 'quoted' → quoted; anything else is kept verbatim.
        let unquoted = match rest.len() {
            n if n >= 2 && rest.starts_with('\'') && rest.ends_with('\'') => &rest[1..n - 1],
            _ => rest,
        };
        out.insert(path.to_string(), unquoted.to_string());
    }
    out
}

/// u60d：serve 模式（接管旧数据服务 data-service）登记短信会话密钥，和它一样；别的模式只在
/// `LUYAZ_SMS_SESSION=1` 时登记（影子模式一律不登记）。见 PORTING.md。
pub static SMS_SESSION: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// u60d 跑在单线程运行时上（u60-platform.md E2 第 1 条）：采集轮里的同步读文件（/etc/config、
/// /proc、/sys、key.log 末尾）交给阻塞线程池做，读的期间屏幕推送和别的任务照常走；
/// 调用方原地等结果，所以读的先后不变。里面 panic 照原样抛回来。
pub(crate) async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|e| std::panic::resume_unwind(e.into_panic()))
}

// uci 子进程不另走执行者队列：会 fork uci 的只有旧采集（在执行者的采集轮里）和 /control
// （整个作为控制任务在执行者里跑），两者本来就由执行者串行，同一时间最多一个 uci 在途。
pub(crate) async fn uci_show(package: &str) -> BTreeMap<String, String> {
    if crate::ubus::validate_name(package).is_err() {
        return Default::default();
    }
    let p = package.to_owned();
    let parsed = blocking(move || {
        uci_parsed(&p).map(|pkg| uci_show_map(&crate::uci::render_show(&p, &pkg)))
    })
    .await;
    match parsed {
        Some(map) => map,
        None => command::run(&uci_bin(), ["-q", "show", package], UCI_DEADLINE)
            .await
            .map(|raw| uci_show_map(&raw))
            .unwrap_or_default(),
    }
}

/// Every `uci` child process gets the same five seconds.
const UCI_DEADLINE: Duration = Duration::from_secs(5);

/// Accepts the characters a UCI path may use (`pkg.section.option`, `@type[0]`).
fn uci_path_ok(path: &str) -> bool {
    (1..=256).contains(&path.len())
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'@' | b'[' | b']'))
}

/// 采集用的 `uci get <包>.<节>.<选项>`：能自己解析时不 fork（R19），否则同 `uci_read`。
/// /control 的读改写仍用 `uci_read`（要看到未保存的改动，走 uci 子进程更直接）。
pub(crate) async fn uci_value(path: &str) -> String {
    let mut parts = path.splitn(3, '.');
    if let (Some(package), Some(section), Some(option)) = (parts.next(), parts.next(), parts.next())
        && !path.contains('@')
        && !option.contains('.')
        && crate::ubus::validate_name(package).is_ok()
    {
        let (package, section, option) =
            (package.to_owned(), section.to_owned(), option.to_owned());
        let parsed = blocking(move || {
            uci_parsed(&package).map(|pkg| {
                crate::uci::get(&pkg, &section, &option)
                    .map(|v| String::from_utf8_lossy(&v).trim().to_owned())
                    .unwrap_or_default()
            })
        })
        .await;
        if let Some(v) = parsed {
            return v;
        }
    }
    uci_read(path).await
}
pub async fn uci_read(path: &str) -> String {
    if !uci_path_ok(path) {
        return String::new();
    }
    match command::run(&uci_bin(), ["-q", "get", path], UCI_DEADLINE).await {
        Ok(out) => String::from_utf8_lossy(&out).trim().to_string(),
        Err(_) => String::new(),
    }
}

pub async fn uci_write(operation: &str, path: &str, value: Option<&str>) -> Result<(), String> {
    // Which verbs we pass through, and whether each one carries `=value`.
    let takes_value = match operation {
        "set" | "add_list" | "del_list" => true,
        "delete" | "commit" | "revert" => false,
        _ => return Err("invalid UCI mutation".into()),
    };
    let value_bad = |v: &str| v.len() > 8192 || v.chars().any(|c| matches!(c, '\0' | '\r' | '\n'));
    if !uci_path_ok(path) || value.is_some_and(value_bad) {
        return Err("invalid UCI mutation".into());
    }
    let target = if takes_value {
        let v = value.ok_or_else(|| "missing UCI value".to_string())?;
        format!("{path}={v}")
    } else {
        path.to_string()
    };
    match command::run(&uci_bin(), [operation, target.as_str()], UCI_DEADLINE).await {
        Ok(_) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

// ── Host readers: /proc, /sys and a few vendor files ──────────────────────
//
// All of these are synchronous and cheap; `read_inputs` runs them together on
// the blocking pool. Missing or unreadable files read as zero / empty.

/// Path from an override variable, or the device default.
fn env_or(var: &str, default: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| default.to_string())
}

/// First whitespace-trimmed integer in a file; 0 when absent.
fn read_i64(path: impl AsRef<Path>) -> i64 {
    match fs::read_to_string(path) {
        Ok(text) => text.trim().parse().unwrap_or(0),
        Err(_) => 0,
    }
}

/// `/proc/net/tcp` hex endpoint `AABBCCDD:PPPP` → (address hex, port).
fn hex_endpoint(field: &str) -> Option<(&str, u16)> {
    let (addr, port) = field.split_once(':')?;
    Some((addr, u16::from_str_radix(port, 16).unwrap_or(0)))
}

/// Aggregation tunnel probe for MU5252: among the TCP sockets whose inodes the
/// ICG daemon owns (`LUYAZ_ICG_SOCKET_INODES`), count the outbound established
/// ones (those whose local port is not one of its listeners) and report the
/// first peer. Returns (tunnels, peer ip, peer port, probe ran).
fn tcp_aggregation_summary() -> (usize, String, u16, bool) {
    let inode_list = env_or("LUYAZ_ICG_SOCKET_INODES", "");
    let ours: Vec<u64> = inode_list
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    if ours.is_empty() {
        return (0, String::new(), 0, false);
    }
    let table = fs::read_to_string(env_or("LUYAZ_PROC_NET_TCP", "/proc/net/tcp")).unwrap_or_default();
    let mut listen_ports: Vec<u16> = Vec::new();
    let mut connected: Vec<(u16, String, u16)> = Vec::new();
    for row in table.lines().skip(1) {
        let cols: Vec<&str> = row.split_whitespace().collect();
        if cols.len() < 10 {
            continue;
        }
        let inode: Option<u64> = cols[cols.len() - 1].parse().ok();
        if !matches!(inode, Some(n) if ours.contains(&n)) {
            continue;
        }
        let local = hex_endpoint(cols[1]);
        match cols[3] {
            "0A" => {
                if let Some((_, port)) = local {
                    listen_ports.push(port);
                }
            }
            "01" => {
                if let (Some((_, lport)), Some((raddr, rport))) = (local, hex_endpoint(cols[2])) {
                    connected.push((lport, raddr.to_string(), rport));
                }
            }
            _ => {}
        }
    }
    connected.retain(|(lport, _, _)| !listen_ports.contains(lport));
    let (peer_ip, peer_port) = match connected.first() {
        Some((_, hex, port)) => {
            // The kernel prints the IPv4 address as a little-endian u32.
            let ip = std::net::Ipv4Addr::from(u32::from_str_radix(hex, 16).unwrap_or(0).to_le_bytes());
            (ip.to_string(), *port)
        }
        None => (String::new(), 0),
    };
    (connected.len(), peer_ip, peer_port, true)
}

/// Kernel thermal zones. Returns the CPU figure (mean of the `cpuss*` zones,
/// else the hottest zone), the `/state` list and the `/v2` runtime list.
fn thermal_zones() -> (i64, Value, Vec<crate::contract::ThermalZone>) {
    let base = env_or("LUYAZ_THERMAL_ROOT", "/sys/class/thermal");
    let Ok(dir) = fs::read_dir(base) else {
        return (0, json!([]), Vec::new());
    };
    // (zone type, millidegrees), only plausible readings.
    let mut readings: Vec<(String, i64)> = Vec::new();
    for item in dir.flatten() {
        if !item.file_name().to_string_lossy().starts_with("thermal_zone") {
            continue;
        }
        let zone = item.path();
        let kind = fs::read_to_string(zone.join("type")).unwrap_or_default().trim().to_string();
        let milli = read_i64(zone.join("temp"));
        let plausible = milli != 0 && milli > -40_000 && milli <= 200_000;
        if !kind.is_empty() && plausible {
            readings.push((kind, milli));
        }
    }
    readings.sort_by(|a, b| a.0.cmp(&b.0));
    let celsius = |m: i64| m as f64 / 1000.0;
    let cpu_cluster: Vec<f64> = readings
        .iter()
        .filter(|(k, _)| k.starts_with("cpuss"))
        .map(|(_, m)| celsius(*m))
        .collect();
    let cpu = if cpu_cluster.is_empty() {
        readings.iter().map(|(_, m)| celsius(*m)).fold(0.0, f64::max)
    } else {
        cpu_cluster.iter().sum::<f64>() / cpu_cluster.len() as f64
    };
    let listed = readings
        .iter()
        .map(|(k, m)| json!({"name": k, "celsius": celsius(*m)}))
        .collect();
    let runtime_list = readings
        .into_iter()
        .map(|(kind, milli)| crate::contract::ThermalZone { r#type: kind, temp_milli: milli })
        .collect();
    (cpu.round() as i64, Value::Array(listed), runtime_list)
}

/// 充电口和电池的电压、电流（`battery` 对象的 chg_*/bat_*）。
fn read_rails() -> Rails {
    let supply = |unit: &str, file: &str| {
        read_i64(host_path(&format!("/sys/class/power_supply/{unit}/{file}")))
    };
    Rails {
        chg_uv: supply("usb", "voltage_now"),
        chg_ua: supply("usb", "current_now"),
        bat_uv: supply("battery", "voltage_now"),
        bat_ua: supply("battery", "current_now"),
    }
}

/// dnsmasq lease file → lowercase MAC → (IP, hostname; `*` means none).
fn lease_metadata() -> Leases {
    let text = fs::read_to_string(env_or("LUYAZ_DHCP_LEASES_PATH", "/tmp/dhcp.leases"))
        .unwrap_or_default();
    let mut leases = Leases::new();
    for row in text.lines() {
        // expiry mac ip hostname [client-id]
        let mut cols = row.split_whitespace().skip(1);
        let (Some(mac), Some(ip), Some(host)) = (cols.next(), cols.next(), cols.next()) else {
            continue;
        };
        let host = if host == "*" { "" } else { host };
        leases.insert(mac.to_ascii_lowercase(), (ip.to_string(), host.to_string()));
    }
    leases
}

type CpuTable = BTreeMap<String, (u64, u64)>;
static CPU_LAST: OnceLock<Mutex<CpuTable>> = OnceLock::new();

/// Number of rows in a /proc table, minus the header row when asked.
fn count_lines(path: &str, header: bool) -> u64 {
    let rows = fs::read_to_string(path).map_or(0, |t| t.lines().count() as u64);
    if header { rows.saturating_sub(1) } else { rows }
}

/// IPv4 TCP sockets in ESTABLISHED (state 01).
fn tcp_active() -> u64 {
    let table = fs::read_to_string(host_path("/proc/net/tcp")).unwrap_or_default();
    let mut n = 0;
    for row in table.lines().skip(1) {
        if row.split_whitespace().nth(3) == Some("01") {
            n += 1;
        }
    }
    n
}

/// 测试用：LUYAZ_HOST_ROOT 设了时，下面这些写死的 /proc、/sys、/data 路径整体挪到它下面，
/// 让 golden（tests/golden/）不受宿主机影响。设备上不设，路径和原来完全一样。
pub(crate) fn host_path(path: &str) -> String {
    static PREFIX: OnceLock<String> = OnceLock::new();
    let prefix = PREFIX.get_or_init(|| std::env::var("LUYAZ_HOST_ROOT").unwrap_or_default());
    format!("{prefix}{path}")
}

fn meminfo() -> crate::contract::MemoryKb {
    let text = fs::read_to_string(host_path("/proc/meminfo")).unwrap_or_default();
    // "MemTotal:  123456 kB" → 123456; unknown keys read 0.
    let kb = |name: &str| -> u64 {
        text.lines()
            .find_map(|row| {
                let rest = row.strip_prefix(name)?.strip_prefix(':')?;
                Some(rest.split_whitespace().next()?.parse().unwrap_or(0))
            })
            .unwrap_or(0)
    };
    crate::contract::MemoryKb {
        total: kb("MemTotal"),
        free: kb("MemFree"),
        available: kb("MemAvailable"),
        buffers: kb("Buffers"),
        cached: kb("Cached"),
        swap_total: kb("SwapTotal"),
        swap_free: kb("SwapFree"),
    }
}

/// Capacity of the `/data` filesystem in bytes.
// statvfs field widths differ between libc targets, hence the casts.
#[allow(clippy::unnecessary_cast)]
fn storage() -> crate::contract::Storage {
    let Ok(cpath) = CString::new(host_path("/data")) else {
        return crate::contract::Storage::default();
    };
    // SAFETY: statvfs only writes into the zeroed struct we own.
    let mut fsinfo: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(cpath.as_ptr(), &mut fsinfo) } != 0 {
        return crate::contract::Storage::default();
    }
    let unit = match fsinfo.f_frsize as u64 {
        0 => fsinfo.f_bsize as u64,
        n => n,
    };
    let bytes = |blocks: u64| unit.saturating_mul(blocks);
    let total = bytes(fsinfo.f_blocks as u64);
    crate::contract::Storage {
        total,
        used: total.saturating_sub(bytes(fsinfo.f_bfree as u64)),
        available: bytes(fsinfo.f_bavail as u64),
    }
}

/// One counter reading: cumulative bytes and when they were read.
#[derive(Clone, Copy)]
struct ByteMark {
    rx: u64,
    tx: u64,
    ms: u64,
}

static LINK_LAST: OnceLock<Mutex<BTreeMap<String, ByteMark>>> = OnceLock::new();
static USER_HISTORY: OnceLock<Mutex<VecDeque<ByteMark>>> = OnceLock::new();

/// Cumulative (rx, tx) bytes of a network interface.
fn iface_bytes(name: &str) -> Option<(u64, u64)> {
    let stats = Path::new(&env_or("LUYAZ_NET_CLASS_ROOT", "/sys/class/net"))
        .join(name)
        .join("statistics");
    let counter = |file: &str| -> Option<u64> {
        fs::read_to_string(stats.join(file)).ok()?.trim().parse().ok()
    };
    Some((counter("rx_bytes")?, counter("tx_bytes")?))
}

/// Milliseconds on a monotonic clock that starts at the first call.
fn now_ms() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    let elapsed = EPOCH.get_or_init(Instant::now).elapsed().as_millis();
    u64::try_from(elapsed).unwrap_or(u64::MAX)
}

/// Bytes per second between two counter readings `window` ms apart.
fn rate(now_bytes: u64, then_bytes: u64, window: u64) -> u64 {
    now_bytes.saturating_sub(then_bytes).saturating_mul(1000) / window
}

/// Per-interface counters and rates since the previous round.
fn link_rates(now: u64) -> Vec<crate::contract::LinkRate> {
    const WATCHED: [&str; 4] = ["rmnet_data0", "V3E1net0", "V3E2net0", "eth0"];
    let mut last = LINK_LAST.get_or_init(Default::default).lock().unwrap();
    let mut out = Vec::with_capacity(WATCHED.len());
    for ifname in WATCHED {
        let Some((rx, tx)) = iface_bytes(ifname) else {
            last.remove(ifname);
            continue;
        };
        // A rate needs an earlier sample, time in between and counters that did
        // not go backwards (interface reset).
        let usable = last.get(ifname).copied().and_then(|prev| {
            let span = now.saturating_sub(prev.ms);
            (span > 0 && rx >= prev.rx && tx >= prev.tx).then_some((prev, span))
        });
        out.push(crate::contract::LinkRate {
            interface: ifname.into(),
            source: "netdev".into(),
            window_ms: usable.map_or(0, |(_, span)| span),
            rx_bytes: rx,
            tx_bytes: tx,
            rx_bps: usable.map(|(prev, span)| rate(rx, prev.rx, span)),
            tx_bps: usable.map(|(prev, span)| rate(tx, prev.tx, span)),
        });
        last.insert(ifname.into(), ByteMark { rx, tx, ms: now });
    }
    out
}

/// Bytes seen from the users' side as (download, upload). The interface
/// counters are from the router's point of view, so they are swapped: what the
/// LAN bridge transmits is what users download. Falls back to the Wi-Fi APs,
/// then to the cellular interfaces.
fn user_bytes() -> (u64, u64) {
    let swapped_sum = |names: &[&str]| -> Option<(u64, u64)> {
        let mut found = false;
        let mut down = 0u64;
        let mut up = 0u64;
        for (rx, tx) in names.iter().filter_map(|n| iface_bytes(n)) {
            found = true;
            down = down.saturating_add(tx);
            up = up.saturating_add(rx);
        }
        found.then_some((down, up))
    };
    swapped_sum(&["br-lan"])
        .or_else(|| swapped_sum(&["wlan0", "wlan2"]))
        .or_else(|| swapped_sum(&["rmnet_data0", "rmnet_ipa0"]))
        .unwrap_or((0, 0))
}

/// Average user throughput across the last (up to) 16 rounds.
fn throughput(now: u64) -> crate::contract::Throughput {
    const KEEP: usize = 16;
    let (rx, tx) = user_bytes();
    let mut history = USER_HISTORY
        .get_or_init(|| Mutex::new(VecDeque::with_capacity(KEEP)))
        .lock()
        .unwrap();
    while history.len() >= KEEP {
        history.pop_front();
    }
    history.push_back(ByteMark { rx, tx, ms: now });
    if history.len() < 2 {
        return crate::contract::Throughput::default();
    }
    let oldest = history[0];
    let span = now.saturating_sub(oldest.ms);
    if span == 0 {
        return crate::contract::Throughput::default();
    }
    crate::contract::Throughput {
        rx_bps: rate(rx, oldest.rx, span),
        tx_bps: rate(tx, oldest.tx, span),
        window_ms: span,
    }
}

/// `/proc/stat` → label (`cpu`, `cpu0`, …) → (total jiffies, idle+iowait).
fn cpu_counters() -> CpuTable {
    let stat = fs::read_to_string(host_path("/proc/stat")).unwrap_or_default();
    let mut table = CpuTable::new();
    for row in stat.lines() {
        let mut cols = row.split_whitespace();
        let Some(label) = cols.next() else { continue };
        let Some(suffix) = label.strip_prefix("cpu") else { continue };
        if !suffix.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let ticks: Vec<u64> = cols.take(8).map(|c| c.parse().unwrap_or(0)).collect();
        if ticks.len() < 4 {
            continue;
        }
        let idle = ticks[3] + ticks.get(4).copied().unwrap_or(0);
        table.insert(label.to_string(), (ticks.iter().sum(), idle));
    }
    table
}

/// Per-mille busy share between two counter readings; -1 when unknown.
fn busy_per_mille(before: Option<&(u64, u64)>, after: (u64, u64)) -> i64 {
    let Some(&(t0, i0)) = before else { return -1 };
    let total = after.0.saturating_sub(t0);
    if total == 0 {
        return -1;
    }
    let busy = total.saturating_sub(after.1.saturating_sub(i0));
    (busy * 1000 / total) as i64
}

/// Current and maximum frequency (MHz) of one core, scaling_* first.
fn core_freq(core: &str) -> crate::contract::Freq {
    let dir = host_path(&format!("/sys/devices/system/cpu/{core}/cpufreq"));
    let khz = |primary: &str, fallback: &str| match read_i64(format!("{dir}/{primary}")) {
        0 => read_i64(format!("{dir}/{fallback}")),
        v => v,
    };
    crate::contract::Freq {
        cur: khz("scaling_cur_freq", "cpuinfo_cur_freq") / 1000,
        max: khz("scaling_max_freq", "cpuinfo_max_freq") / 1000,
    }
}

fn runtime(runtime_zones: Vec<crate::contract::ThermalZone>) -> (i64, crate::contract::Runtime) {
    let fresh = cpu_counters();
    let (overall, per_core) = {
        let mut stored = CPU_LAST.get_or_init(Default::default).lock().unwrap();
        let mut overall = -1;
        let mut per_core = BTreeMap::new();
        for (label, counters) in &fresh {
            let share = busy_per_mille(stored.get(label), *counters);
            if label == "cpu" {
                overall = share;
            } else {
                per_core.insert(label.clone(), share);
            }
        }
        *stored = fresh;
        (overall, per_core)
    };
    let freqs = per_core.keys().map(|core| (core.clone(), core_freq(core))).collect();
    let rows = |p: &str| count_lines(&host_path(p), true);
    let established = tcp_active();
    let tcp4 = rows("/proc/net/tcp");
    let connections = crate::contract::Connections {
        tcp_active: established,
        tcp_other: tcp4.saturating_sub(established),
        tcp4,
        tcp6: rows("/proc/net/tcp6"),
        udp4: rows("/proc/net/udp"),
        udp6: rows("/proc/net/udp6"),
        unix: rows("/proc/net/unix"),
    };
    let at = now_ms();
    let snapshot = crate::contract::Runtime {
        cpu_usage_tenths: overall,
        cpu_cores: per_core,
        cpu_freq_mhz: freqs,
        thermal_zones: runtime_zones,
        memory_kb: meminfo(),
        storage: storage(),
        connections,
        link_rates: link_rates(at),
        throughput: throughput(at),
    };
    (overall, snapshot)
}

/// 旧接口的一轮采集。电池、充电器已迁到块模型（`block::phase1_blocks`），从 `hub` 取；
/// stale 的块按读失败输出（V2-29）。其余读取仍走 `ubus_ttl`。
/// SIM identity (P0-2): feed this round's identity + bearer to `sim_identity`, write its
/// epoch/phase into the `sim` field and block. A round whose `get_sim_info` failed (the `sim`
/// block is an error) is not fed: an empty read is not a SIM gone. Bearer = netifd's zte_wan /
/// zte_wan6 up with an address (the eSIM switch itself checks the route too).
fn sim_identity_stamp(fields: &mut serde_json::Map<String, Value>, blocks: &mut [(&'static str, Result<Value, String>)], now: std::time::Instant) {
    let Some(sim) = fields.get("sim") else { return };
    let read = blocks.iter().any(|(n, d)| *n == "sim" && d.is_ok());
    let iccid = sim["iccid"].as_str().unwrap_or("");
    let imsi = sim["imsi"].as_str().unwrap_or("");
    let up = |k: &str, fam: &str| {
        let i = fields.get("interfaces").map(|i| &i[k]);
        i.is_some_and(|i| i["up"] == true && i[fam].as_array().is_some_and(|a| !a.is_empty()))
    };
    let bearer = up("wan4", "ipv4") || up("wan6", "ipv6");
    let (epoch, phase, settled, say) = {
        let mut s = crate::sim_identity::global().lock().unwrap_or_else(|e| e.into_inner());
        if read {
            s.observe(crate::sim_identity::Identity::new(iccid, imsi), bearer, now);
        }
        (s.epoch(), s.phase().as_str(), s.settled(), crate::sim_say::for_phase(s.phase()))
    };
    crate::sim_alerts::phase(epoch, phase);
    crate::sim_follow::phase(epoch, phase);
    if let Some(e) = settled {
        crate::sim_follow::settled(e);
    }
    let stamp = |v: &mut Value| {
        if let Some(o) = v.as_object_mut() {
            o.insert("epoch".into(), json!(epoch));
            o.insert("phase".into(), json!(phase));
            crate::sim_say::stamp(o, say);
        }
    };
    if let Some(v) = fields.get_mut("sim") {
        stamp(v);
    }
    for (name, data) in blocks.iter_mut() {
        if *name == "sim"
            && let Ok(v) = data
        {
            stamp(v);
        }
    }
}

/// 一轮采集：读（这里）→ 算（`project::snapshot::project`，纯函数）→ MU5252 外挂模组（边读边算）→ 记 `/v2` 块。
pub async fn collect(sample_interval_ms: u64, hub: &crate::block::Hub) -> Snapshot {
    let projection = snapshot::project(read_inputs(hub).await, sample_interval_ms);
    let Projection {
        mut fields,
        blocks,
        mu5252,
    } = projection;
    if let Some(ctx) = mu5252 {
        mu5252_extras(ctx, &mut fields).await;
    }
    let mut blocks = blocks;
    sim_identity_stamp(&mut fields, &mut blocks, std::time::Instant::now());
    // dual-SIM standby signal: tell the QMI sampler which slot to read (0 = none)
    let dual = fields.get("sim").and_then(|s| s.get("dual")).cloned();
    let want = dual
        .and_then(|d| serde_json::from_value::<crate::contract::SimDual>(d).ok())
        .map_or(0, |d| snapshot::standby_slot(&d));
    crate::qmi_nas::want(want);
    // dual-SIM passive log (dsds_log.rs): keeps the latest dual-mode fields, writes every 30 s
    crate::dsds_log::offer(&fields);
    // per-day per-card usage (usage_log.rs): diffs the session counters every 60 s
    crate::usage_log::offer(&fields);
    let now = tokio::time::Instant::now();
    for (name, data) in blocks {
        hub.record(name, data, now);
    }
    Snapshot { ts: unix_now(), core: Default::default(), fields }
}

/// 一轮要读的全部东西，顺序和以前在 `collect` 里一样（厂商 ubus 并发会丢回复，所以串行；
/// 执行者按这个顺序排队）。
async fn read_inputs(hub: &crate::block::Hub) -> Inputs {
    // Vendor ubus implementations on these devices lose replies under a large
    // burst of concurrent clients, so state collection is deliberately serial.
    let common = profile_ubus(hub, 60, "zwrt_zte_mdm.api", "get_zwrt_common_info").await;
    let board = profile_ubus(hub, 300, "system", "board").await;
    let info = ubus_ttl(0, "system", "info", json!({})).await;
    let net = ubus_ttl(0, "zte_nwinfo_api", "nwinfo_get_netinfo", json!({})).await;
    let traffic = ubus_ttl(
        0,
        "zwrt_data",
        "get_wwandst",
        json!({"source_module":"deviceui","cid":1,"type":1}),
    )
    .await;
    let accounting = ubus_ttl(
        10,
        "zwrt_data",
        "get_wwandst",
        json!({"source_module":"web","cid":1,"type":4}),
    )
    .await;
    let limit = ubus_ttl(
        60,
        "zwrt_data",
        "get_wwandst_monthlimit",
        json!({"source_module":"web","cid":1}),
    )
    .await;
    let clear_day = ubus_ttl(
        60,
        "zwrt_data",
        "get_wwandst_clearday",
        json!({"source_module":"web","cid":1}),
    )
    .await;
    let (sim, sim_read) = ubus_ttl_read(30, "zwrt_zte_mdm.api", "get_sim_info", json!({})).await;
    let imei = ubus_ttl(300, "zwrt_zte_mdm.api", "get_imei", json!({})).await;
    let lan_clients = ubus_ttl(
        10,
        "zwrt_router.api",
        "router_lan_access_list",
        json!({"start_id":1,"end_id":64}),
    )
    .await;
    let wifi_clients = ubus_ttl(
        10,
        "zwrt_router.api",
        "router_wireless_access_list",
        json!({"start_id":1,"end_id":64}),
    )
    .await;
    let router_status =
        ubus_ttl(5, "zwrt_router.api", "router_get_status_no_auth", json!({})).await;
    let thermal = ubus_ttl(5, "zwrt_bsp.thermal", "get_cpu_temp", json!({})).await;
    // 固件的过热限速标记（MU5250：zwrt_zte_mc_tmp.cpe.hightemp_datalimit_status）。走 ubus 的
    // uci get：它带上 /tmp/.uci 里没提交的改动，而这个包在 /tmp/.uci 里常年有几百行改动，
    // 自己解析会退回 fork `uci show`。
    let hightemp = ubus_ttl(
        30,
        "uci",
        "get",
        json!({"config":"zwrt_zte_mc_tmp","section":"cpe","option":"hightemp_datalimit_status"}),
    )
    .await;
    // QoS 的当前 QCI/5QI（qos::Query::qci_now）：厂商只在数据连接变化时写，读法同上。
    let qci_now = ubus_ttl(
        5,
        "uci",
        "get",
        json!({"config":"zwrt_data_tmp","section":"wwaniface1","option":"qci"}),
    )
    .await;
    let usb = profile_ubus(hub, 30, "zwrt_bsp.usb", "list").await;
    let battery = hub.legacy("battery");
    let charger = hub.legacy("charger");
    let nfc = ubus_ttl(60, "zwrt_nfc", "zwrt_nfc_wifi_get", json!({})).await;
    let apn = {
        let sim = object(sim.clone());
        let slot2 = data_on_slot2(&sim);
        apn_reads(&card_field(&sim, slot2, "sim_iccid"), slot2).await
    };
    let router = router_reads().await;
    // u60d：登记短信会话密钥（`zwrt_web web_http_enstr_set`）会顶掉另一个进程（比如并跑的 data-service）在厂商那里登记的那把，
    // 它之后就解不开短信（密钥缓存到重启）。所以只在 u60d 就是核心（`luyaz-core` 名字起、serve 模式）
    // 时登记（SMS_SESSION）。见 PORTING.md。
    if SMS_SESSION.load(std::sync::atomic::Ordering::Relaxed)
        || std::env::var("LUYAZ_SMS_SESSION").as_deref() == Ok("1")
    {
        let _ = crate::sms::prepare().await;
    }
    let (sms_capacity, capacity_read) =
        ubus_ttl_read(30, "zwrt_wms", "zwrt_wms_get_wms_capacity", json!({})).await;
    let (sms_nv, nv_read) = ubus_ttl_read(
        10,
        "zwrt_wms",
        "zte_libwms_get_sms_data",
        json!({"page":0,"data_per_page":8,"mem_store":1,"tags":10,"order_by":"order by id desc"}),
    )
    .await;
    let (sms_sim, sim_list_read) = ubus_ttl_read(
        10,
        "zwrt_wms",
        "zte_libwms_get_sms_data",
        json!({"page":0,"data_per_page":8,"mem_store":0,"tags":10,"order_by":"order by id desc"}),
    )
    .await;
    let lan_if = ubus_ttl(5, "network.interface.lan", "status", json!({})).await;
    let wan4_if = ubus_ttl(5, "network.interface.zte_wan", "status", json!({})).await;
    let wan6_if = ubus_ttl(5, "network.interface.zte_wan6", "status", json!({})).await;
    let lan_config = ubus_ttl(60, "zwrt_router.api", "router_get_lan_info", json!({})).await;
    let cellular = ubus_ttl(
        5,
        "zwrt_data",
        "get_wwaniface",
        json!({"source_module":"web","cid":1,"connect_status":""}),
    )
    .await;
    // stall 的 30 秒窗口：厂商收发包数（cell_window.rs；没读到就作废窗口）
    crate::cell_window::sample(
        now_ms(),
        traffic.as_ref().ok().and_then(crate::cell_window::counts),
    );
    let mut uci_sets = Vec::new();
    // Configuration packages change only when someone saves settings, and
    // every save through /control clears the cache.
    for p in snapshot::PACKAGES {
        let values = if matches!(p, "zwrt_common_info" | "zwrt_tr069" | "zte_nwinfo") {
            uci_show_ttl_inner(30, p, Some(hub.firmware())).await
        } else {
            uci_show_ttl(30, p).await
        };
        uci_sets.push(values)
    }
    // 访客 Wi-Fi 开着才问剩余时间（倒计时，60 秒一读）。
    let guest_on = ["guest_2g", "guest_5g"]
        .iter()
        .any(|g| snapshot::wifi_iface_on(&uci_sets, g));
    let guest_left = if guest_on {
        Some(
            ubus_ttl(
                60,
                "zwrt_wlan",
                "wlan_get_guest_access_left_time",
                json!({}),
            )
            .await,
        )
    } else {
        None
    };
    let sms_list = match &sms_capacity {
        Ok(_) => Some(
            crate::sms::normalize_lists(&[object(sms_nv.clone()), object(sms_sim.clone())]).await,
        ),
        Err(_) => None,
    };
    // 同步读文件一次交给阻塞线程池，先后和原来一样：thermal、runtime、key.log、租约、电源轨。
    let qos_token = hub.firmware().token();
    // 记录仪（netrec.rs，LUYAZ_NETREC=1）：只用这一轮已读到的厂商计数、客户端表、基带信息
    let netrec = crate::netrec::enabled()
        .then(|| crate::netrec::Pre::from_reads(&traffic, &lan_clients, &wifi_clients, &net));
    let (cpu_sys, zones, cpu_usage_tenths, runtime, qos_logs, leases, rails) = blocking(|| {
        let (cpu_sys, zones, runtime_zones) = thermal_zones();
        let (cpu_usage_tenths, runtime) = runtime(runtime_zones);
        if let Some(pre) = netrec {
            crate::netrec::tick(pre, &runtime, now_ms(), unix_now());
        }
        let qos_logs = crate::qos::logs();
        (
            cpu_sys,
            zones,
            cpu_usage_tenths,
            runtime,
            qos_logs,
            lease_metadata(),
            read_rails(),
        )
    })
    .await;
    let query = qos_query(
        &object(net.clone()),
        &{
            let sim = object(sim.clone());
            card_field(&sim, data_on_slot2(&sim), "sim_imsi")
        },
        &object(wan4_if.clone()),
        uci_qci(&qci_now),
    );
    hub.firmware().qos(qos_token, &qos_logs, query);
    Inputs {
        common,
        board,
        info,
        net,
        traffic,
        accounting,
        limit,
        clear_day,
        sim,
        sim_read,
        imei,
        lan_clients,
        wifi_clients,
        router_status,
        thermal,
        hightemp,
        qci_now,
        usb,
        battery,
        charger,
        nfc,
        apn,
        router,
        guest_left,
        sms_capacity,
        capacity_read,
        sms_nv,
        nv_read,
        sms_sim,
        sim_list_read,
        now_epoch: crate::qmi_nas::now(),
        standby_signal: crate::qmi_nas::last(),
        sms_list,
        lan_if,
        wan4_if,
        wan6_if,
        lan_config,
        cellular,
        uci_sets,
        qos_logs,
        leases,
        rails,
        cpu_sys,
        zones,
        cpu_usage_tenths,
        runtime,
    }
}

/// Seconds since the Unix epoch on the device clock.
fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// One of the two external 4G modules on the MU5252 (TopFlow) USB hub.
struct ExtModule {
    id: &'static str,
    /// adb serial of the module's debug interface.
    serial: &'static str,
    ifname: &'static str,
    /// mwan interface for IPv4; IPv6 is the same name with `_6`.
    wan: &'static str,
    usb_path: &'static str,
    usb_id: &'static str,
    /// subid of SIM slot 0 on this module; slot 1 is the next number.
    first_subid: i64,
}

const EXT_MODULES: [ExtModule; 2] = [
    ExtModule {
        id: "v3e1",
        serial: "V3E1T12345",
        ifname: "V3E1net0",
        wan: "zte_mwan3",
        usb_path: "1-1",
        usb_id: "19d2:0581",
        first_subid: 3,
    },
    ExtModule {
        id: "v3e2",
        serial: "V3E2T12345",
        ifname: "V3E2net0",
        wan: "zte_mwan4",
        usb_path: "1-2",
        usb_id: "19d2:1716",
        first_subid: 5,
    },
];

/// mwan3 members reported in `aggregation.paths`: (id, label, mwan interface).
const AGG_MEMBERS: [(&str, &str, &str); 4] = [
    ("x75", "X75", "zte_mwan2"),
    ("v3e1", "V3E1", "zte_mwan3"),
    ("v3e2", "V3E2", "zte_mwan4"),
    ("ethernet", "Ethernet", "waneth"),
];

async fn ubus_obj(service: &str, method: &str, args: Value) -> Value {
    object(ubus(service, method, args).await)
}

async fn netif_status(name: &str) -> Value {
    ubus_obj(&format!("network.interface.{name}"), "status", json!({})).await
}

/// `wwan` object shared by the integrated and the external modems.
fn wwan_summary(w: &Value) -> Value {
    json!({
        "status": string(w, "connect_status"),
        "ipv4_ifname": string(w, "ipv4_dev_name"),
        "ipv6_ifname": string(w, "ipv6_dev_name"),
    })
}

/// What the module itself can tell over adb: QoS from its key.log and the
/// board temperature. Without `LUYAZ_ADB_BIN` nothing is asked.
struct AdbProbe {
    qos: crate::qos::Values,
    /// When the QoS was read (0 = never).
    qos_at: i64,
    celsius: Option<i64>,
}

async fn adb_probe(adb: Option<&str>, serial: &str) -> AdbProbe {
    let mut probe = AdbProbe { qos: Default::default(), qos_at: 0, celsius: None };
    let Some(adb) = adb else { return probe };
    let shell = |cmd: &'static [&'static str]| {
        let mut argv = vec!["-s", serial, "shell"];
        argv.extend_from_slice(cmd);
        command::run(adb, argv, Duration::from_secs(5))
    };
    if let Ok(out) = shell(&["grep QCI= /logfs/key.log | tail -n 64"]).await
        && let Some(values) = crate::qos::parse_external(&String::from_utf8_lossy(&out))
    {
        probe.qos = values;
        probe.qos_at = unix_now();
    }
    if let Ok(out) = shell(&["cat", "/sys/devices/virtual/power/zte_power/adc2_temp"]).await {
        probe.celsius = String::from_utf8_lossy(&out)
            .trim()
            .parse::<i64>()
            .ok()
            .filter(|t| (-40..=125).contains(t));
    }
    probe
}

/// The built-in X75 5G modem entry of `modems`.
async fn integrated_modem(ctx: &Mu5252Ctx, subid: i64) -> Value {
    let net = &ctx.raw_net;
    let traffic = ubus_obj(
        "zwrt_data",
        "get_wwandst",
        json!({"source_module":"deviceui","cid":1,"type":1,"subid":subid}),
    )
    .await;
    let rat = string(net, "network_type");
    let bw_key = if rat == "SA" || rat == "NSA" { "nr5g_bandwidth" } else { "lte_bandwidth" };
    let q = crate::qos::read(qos_query(net, &ctx.imsi, &ctx.wan4_if, None));
    let n = |k: &str| integer(net, k);
    let s = |k: &str| string(net, k);
    json!({
        "id": "x75",
        "role": "integrated_5g",
        "transport": "rmnet",
        "subid": subid,
        "ifname": "rmnet_data0",
        "wan_interface": "zte_mwan2",
        "net": {
            "type": rat,
            "bars": n("signalbar"),
            "roaming": s("simcard_roam"),
            "operator": s("network_provider_fullname"),
            "band": s("wan_active_band"),
            "bandwidth": s(bw_key),
            "nr_rsrp": n("nr5g_rsrp"),
            "nr_rsrq": n("nr5g_rsrq"),
            "nr_snr": s("nr5g_snr"),
            "nr_pci": n("nr5g_pci"),
            "nr_cell_id": n("nr5g_cell_id"),
            "nr_channel": n("nr5g_action_channel"),
            "lte_rsrp": n("lte_rsrp"),
            "lte_rsrq": n("lte_rsrq"),
            "lte_pci": n("lte_pci"),
            "cell_id": n("cell_id"),
        },
        "sim": {
            "state": string(&ctx.sim, "sim_states"),
            "slot": integer(&ctx.sim, "current_sim_slot"),
            "iccid": string(&ctx.sim, "sim_iccid"),
            "imsi": ctx.imsi,
            "msisdn": ctx.msisdn,
            "imei": string(&ctx.imei, "imei"),
        },
        "wwan": wwan_summary(&ctx.cellular),
        "interfaces": {"ipv4": interface(&ctx.wan4_if), "ipv6": interface(&ctx.wan6_if)},
        "traffic": realtime_traffic(&traffic),
        "qos": {
            "qci": q.qci, "ambr_dl": q.ambr_dl, "ambr_ul": q.ambr_ul,
            "bearer": q.bearer, "stale": q.stale, "sampled_at": 0,
        },
    })
}

/// One external module: its `modems` entry and its `thermal.modems` entry.
async fn external_modem(
    m: &ExtModule,
    index: usize,
    v3t: &Value,
    msim: &Value,
    uci_sets: &[BTreeMap<String, String>],
    adb: Option<&str>,
) -> (Value, Value) {
    let prefix = format!("v3t_{}_", index + 1);
    let slot = integer(v3t, &format!("{prefix}st_slot")).clamp(0, 1);
    let subid = m.first_subid + slot;
    let net = topflow_external_net(msim, uci_sets, index, slot);
    let wwan = ubus_obj(
        "zwrt_data",
        "get_wwaniface",
        json!({"source_module":"deviceui","cid":1,"subid":subid}),
    )
    .await;
    let traffic = ubus_obj(
        "zwrt_data",
        "get_wwandst",
        json!({"source_module":"deviceui","cid":1,"type":1,"subid":subid}),
    )
    .await;
    let v4 = netif_status(m.wan).await;
    let v6 = netif_status(&format!("{}_6", m.wan)).await;
    let probe = adb_probe(adb, m.serial).await;
    let card = |field: &str| prefixed_string(v3t, &prefix, field);
    let thermal = json!({
        "id": m.id,
        "available": probe.celsius.is_some(),
        "celsius": probe.celsius,
        "sampled_at": if probe.celsius.is_some() { probe.qos_at } else { 0 },
    });
    let modem = json!({
        "id": m.id,
        "role": "external_4g",
        "transport": "cdc-ecm",
        "subid": subid,
        "ifname": m.ifname,
        "wan_interface": m.wan,
        "usb": {"path": m.usb_path, "id": m.usb_id, "present": false, "carrier": 0},
        "debug": {"transport": "adb", "serial": m.serial, "available": adb.is_some()},
        "net": net,
        "sim": {
            "state": card("modem_main_state"),
            "slot": slot,
            "iccid": card("sim_iccid"),
            "imsi": card("sim_imsi"),
            "msisdn": card("msisdn"),
            "imei": card("imei"),
        },
        "wwan": wwan_summary(&wwan),
        "interfaces": {"ipv4": interface(&v4), "ipv6": interface(&v6)},
        "traffic": realtime_traffic(&traffic),
        "qos": {
            "qci": probe.qos.qci, "ambr_dl": probe.qos.ambr_dl,
            "ambr_ul": probe.qos.ambr_ul, "sampled_at": probe.qos_at,
        },
    });
    (modem, thermal)
}

/// One mwan3 member as an `aggregation.paths` entry; `None` when it is
/// disabled, stopped and down (not worth listing).
async fn aggregation_path(
    (id, label, ifname): (&str, &str, &str),
    member: &Value,
    mwan_running: bool,
) -> Option<Value> {
    let enabled = parse_bool(member, "enabled");
    let running = parse_bool(member, "running");
    let member_up = parse_bool(member, "up");
    let netif = netif_status(ifname).await;
    let netif_up = parse_bool(&netif, "up");
    let up = member_up || netif_up;
    if !(enabled || running || up) {
        return None;
    }
    let status = string(member, "status");
    // Without a running mwan3 the netifd view is all there is.
    let online = status.eq_ignore_ascii_case("online")
        || (running && member_up)
        || (netif_up && !mwan_running);
    let field = |v: &Value, k: &str| v.get(k).cloned().unwrap_or(Value::Null);
    let targets: Vec<Value> = member
        .get("track_ip")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|t| {
                    let st = string(t, "status");
                    let reachable = st.eq_ignore_ascii_case("online") || st.eq_ignore_ascii_case("up");
                    json!({
                        "ip": string(t, "ip"),
                        "status": st,
                        "online": reachable,
                        "latency_ms": field(t, "latency"),
                        "packet_loss_percent": field(t, "packetloss"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    // Headline latency/loss: the first reachable target, else the first one
    // that was actually probed.
    let headline = targets
        .iter()
        .find(|t| t["online"] == true)
        .or_else(|| targets.iter().find(|t| t["status"] != "skipped"))
        .cloned();
    let mut entry = json!({
        "id": id,
        "label": label,
        "interface": ifname,
        "enabled": enabled,
        "running": running,
        "up": up,
        "online": online,
        "interface_up": netif_up,
        "interface_available": parse_bool(&netif, "available"),
        "interface_pending": parse_bool(&netif, "pending"),
        "status": status,
        "tracking": string(member, "tracking"),
        "uptime_seconds": integer(member, "uptime"),
        "targets": targets,
    });
    if let Some(h) = headline {
        for key in ["latency_ms", "packet_loss_percent"] {
            if !h[key].is_null() {
                entry[key] = h[key].clone();
            }
        }
    }
    Some(entry)
}

/// `key = value` lines of the ICG daemon config.
fn parse_kv(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for row in text.lines() {
        if let Some((k, v)) = row.split_once('=') {
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    map
}

/// The `aggregation` object (multi-WAN bonding through the ICG server).
async fn aggregation_summary(mode: &str, mwan_running: bool) -> Value {
    let status = ubus_obj("mwan3", "status", json!({})).await;
    let members = status.get("interfaces").cloned().unwrap_or(Value::Null);
    let mut paths = Vec::new();
    for spec in AGG_MEMBERS {
        if let Some(member) = members.get(spec.2)
            && let Some(entry) = aggregation_path(spec, member, mwan_running).await
        {
            paths.push(entry);
        }
    }
    let online_paths = paths.iter().filter(|p| p["online"] == true).count();
    let (tunnels, live_ip, live_port, icg_running) = blocking(tcp_aggregation_summary).await;
    let provisioned = !uci_value("zwrt_router.icgmwan.IcgDevId").await.is_empty();
    let remaining = uci_value("zwrt_router.icgmwan.residual_flow").await;
    let used_today = uci_value("zwrt_router.icgmwan.count_flow_today").await;
    let conf_path = env_or("LUYAZ_ICG_CONFIG", "/etc/config/icg.conf");
    let conf = parse_kv(&blocking(move || fs::read_to_string(conf_path)).await.unwrap_or_default());
    let conf_num = |k: &str| conf.get(k).and_then(|v| v.parse::<u16>().ok()).unwrap_or(0);
    // Prefer what the live tunnel shows over the configured server.
    let server_ip = if live_ip.is_empty() {
        conf.get("AggregationServerIP").cloned().unwrap_or_default()
    } else {
        live_ip
    };
    let tcp_port = if live_port == 0 { conf_num("AggregationServerTcpPort") } else { live_port };
    let enabled = mode == "SMULTIWAN";
    let state = match (enabled, provisioned, tunnels > 0) {
        (false, _, _) => "disabled",
        (true, false, _) => "unprovisioned",
        (true, true, true) => "online",
        (true, true, false) => "waiting",
    };
    json!({
        "enabled": enabled,
        "mode": mode,
        "state": state,
        "provisioned": provisioned,
        "online": tunnels > 0,
        "controller": {"icg_process_running": icg_running, "mwan3_running": mwan_running},
        "tcp_tunnel_count": tunnels,
        "server": {
            "ip": server_ip,
            "tcp_port": tcp_port,
            "udp_start_port": conf_num("AggregationServerUdpStartPort"),
            "source": if live_port > 0 { "runtime" } else { "config" },
        },
        "path_count": paths.len(),
        "online_path_count": online_paths,
        "paths": paths,
        "traffic": {
            "remaining_bytes": remaining.parse::<u64>().ok(),
            "remaining_raw": remaining,
            "today_used_bytes": used_today.parse::<u64>().ok(),
            "today_used_raw": used_today,
        },
    })
}

/// MU5252（TopFlow）的多模组部分：按卡槽、按外挂模组再调 ubus（和 adb），边读边算，
/// 补 `modems`、`thermal.modems`、`aggregation`、`multiwan`。Phase 2a 原样留在 IO 层，没拆成纯函数。
async fn mu5252_extras(ctx: Mu5252Ctx, fields: &mut Map<String, Value>) {
    let subid = integer(&ctx.sim, "current_sim_slot").clamp(1, 6);
    let mut modems = vec![integrated_modem(&ctx, subid).await];
    let mut temps = vec![if ctx.cpu_temp > 0 {
        json!({"id": "x75", "available": true, "celsius": ctx.cpu_temp})
    } else {
        json!({"id": "x75", "available": false, "celsius": null})
    }];
    let v3t = ubus_obj("zwrt_zte_mdm.api", "get_v3t_sim_info", json!({})).await;
    let msim = ubus_obj("zte_nwinfo_api", "nwinfo_get_msim_netinfo", json!({})).await;
    let adb = std::env::var("LUYAZ_ADB_BIN").ok();
    for (i, module) in EXT_MODULES.iter().enumerate() {
        let (modem, temp) =
            external_modem(module, i, &v3t, &msim, &ctx.uci_sets, adb.as_deref()).await;
        modems.push(modem);
        temps.push(temp);
    }
    if let Some(thermal) = fields.get_mut("thermal") {
        thermal["modems"] = Value::Array(temps);
    }
    fields.insert("modems".into(), Value::Array(modems));
    let mode = uci_value("zwrt_router.network.opms_wan_mode").await;
    let mwan_running = matches!(
        std::env::var("LUYAZ_MWAN3_RUNNING").map(|v| v.parse::<i64>()),
        Ok(Ok(n)) if n != 0
    );
    let aggregation = aggregation_summary(&mode, mwan_running).await;
    fields.insert("aggregation".into(), aggregation);
    fields.insert("multiwan".into(), topflow_multiwan(&ctx.uci_sets, &mode, mwan_running));
}

/// 一次 ubus 调用。经过单一采集执行者（`executor.rs`，V2-18）：执行者里直接发，别处排队；
/// 后端由 `LUYAZ_UBUS=cli|socket` 选（`ubus::backend`），错误文字和原来的 `ubus call` 版一致。
pub async fn ubus(service: &str, method: &str, args: Value) -> Result<Value, String> {
    crate::executor::call(service, method, &args)
        .await
        .map_err(|e| e.to_string())
}

/// 采集以外读 `get_sim_info` 的唯一入口（写前检查、读回、切槽等待、兼容层的读）。
/// 照旧新鲜读——这些地方不能用 30 秒前的卡号或槽号；读到了就放进采集那一轮同一个缓存键，
/// 下一轮 `sim` 块直接用它，不再多读一次。
pub async fn sim_info() -> Result<Value, String> {
    const SIM: (&str, &str) = ("zwrt_zte_mdm.api", "get_sim_info");
    let v = ubus(SIM.0, SIM.1, json!({})).await;
    if let Ok(sim) = &v
        && cache_enabled()
    {
        let ttl = Duration::from_secs(30);
        cache_put(&UBUS_CACHE, ubus_key(SIM.0, SIM.1, &json!({})), ttl, Ok(sim.clone()));
    }
    v
}

#[cfg(test)]
mod tests {
    /// E3 D2：真漫游按国家比，不按原厂的 roaming 标志。
    #[test]
    fn roaming_fields_compare_countries() {
        use serde_json::{Value, json};
        let run = |mcc: i64, imsi: &str| {
            let mut net = json!({"mcc": mcc, "roaming": "Home"});
            super::add_roaming_fields(&mut net, imsi);
            (
                net["serving_mcc"].clone(),
                net["home_mcc"].clone(),
                net["true_roaming"].clone(),
            )
        };
        // 国外插当地卡：原厂说不漫游，这里也不是真漫游
        assert_eq!(
            run(440, "440101234567890"),
            (json!(440), json!(440), json!(false))
        );
        // 中国卡在日本
        assert_eq!(
            run(440, "460001234567890"),
            (json!(440), json!(460), json!(true))
        );
        // 在国内
        assert_eq!(
            run(460, "460011234567890"),
            (json!(460), json!(460), json!(false))
        );
        // 同一国家的不同 MCC：美国 310 卡在 311 网、日本 441
        assert_eq!(
            run(311, "310260123456789"),
            (json!(311), json!(310), json!(false))
        );
        assert_eq!(run(441, "440101234567890").2, json!(false));
        // 香港卡在内地是真漫游
        assert_eq!(run(460, "454001234567890").2, json!(true));
        // 901 国际共享号段的卡：没有国家
        assert_eq!(
            run(460, "901281234567890"),
            (json!(460), Value::Null, Value::Null)
        );
        // 没注册上（mcc 0）
        assert_eq!(
            run(0, "460001234567890"),
            (Value::Null, json!(460), Value::Null)
        );
        // IMSI 读不到
        assert_eq!(run(460, ""), (json!(460), Value::Null, Value::Null));
        // 不是对象就不动
        let mut v = Value::Null;
        super::add_roaming_fields(&mut v, "460001234567890");
        assert!(v.is_null());
    }

    #[test]
    fn spn_decodes_vendor_ucs2_hex() {
        assert_eq!(
            super::spn_from_ucs2_hex("0043004D004C0069006E006B"),
            "CMLink"
        );
        assert_eq!(super::spn_from_ucs2_hex(""), "");
        assert_eq!(super::spn_from_ucs2_hex("zz"), "");
        assert_eq!(super::spn_from_ucs2_hex("4E2D56FD79FB52A8FFFF"), "中国移动");
    }

    /// V2-31：短信事件只清短信容量和列表的缓存，别的慢数据缓存不动。
    #[test]
    fn sms_event_invalidates_only_sms_cache() {
        let keys = [
            format!("zwrt_wms\u{0}zwrt_wms_get_wms_capacity\u{0}{}", json!({})),
            format!(
                "zwrt_wms\u{0}zte_libwms_get_sms_data\u{0}{}",
                json!({"mem_store":1})
            ),
            format!("zwrt_bsp.usb\u{0}list\u{0}{}", json!({})),
        ];
        for k in &keys {
            cache_put(
                &UBUS_CACHE,
                k.clone(),
                Duration::from_secs(60),
                Ok(json!({})),
            );
        }
        invalidate_sms_cache();
        assert!(cache_get(&UBUS_CACHE, &keys[0]).is_none());
        assert!(cache_get(&UBUS_CACHE, &keys[1]).is_none());
        // 缓存是全局的，别的测试会整体清空，所以非短信键只看判定。
        assert!(is_sms_key(&keys[0]) && is_sms_key(&keys[1]) && !is_sms_key(&keys[2]));
    }

    /// 会整体清空慢数据缓存、或依赖缓存内容的测试互相排队（缓存是全局的）。
    static CACHE_TEST: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[test]
    fn slow_state_cache_expires_and_is_cleared_by_control() {
        let _serial = CACHE_TEST.blocking_lock();
        let key = "test\u{0}cache".to_string();
        cache_put(
            &UBUS_CACHE,
            key.clone(),
            Duration::from_millis(80),
            Ok(json!({"v":1})),
        );
        assert_eq!(cache_get(&UBUS_CACHE, &key), Some(Ok(json!({"v":1}))));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            cache_get(&UBUS_CACHE, &key),
            None,
            "expired entries are not served"
        );
        cache_put(
            &UCI_CACHE,
            "wireless".into(),
            Duration::from_secs(60),
            BTreeMap::new(),
        );
        assert!(cache_get(&UCI_CACHE, "wireless").is_some());
        invalidate_cache();
        assert!(
            cache_get(&UCI_CACHE, "wireless").is_none(),
            "a /control action clears the cache"
        );
    }

    use super::*;

    #[test]
    fn hightemp_limit_is_one_zero_or_null() {
        let v = |x: &str| hightemp_limit(&Ok(json!({ "value": x })));
        assert_eq!(v("0"), json!(0));
        assert_eq!(v("1"), json!(1));
        assert_eq!(v("2"), json!(1));
        assert_eq!(v(" 1\n"), json!(1));
        assert_eq!(v(""), Value::Null);
        assert_eq!(v("on"), Value::Null);
        assert_eq!(hightemp_limit(&Ok(json!({}))), Value::Null);
        assert_eq!(hightemp_limit(&Err("Not found".into())), Value::Null);
    }

    #[test]
    fn legacy_state_stale_block_renders_as_failure() {
        use crate::block::{Hub, NoSink, phase1_blocks};
        let hub = Hub::new(phase1_blocks(), Box::new(NoSink));
        let render = |hub: &Hub| {
            let mut f = Map::new();
            power_fields(
                &mut f,
                "MU5250",
                hub.legacy("battery"),
                hub.legacy("charger"),
                read_rails(),
            );
            f
        };
        let failed = {
            let mut f = Map::new();
            power_fields(
                &mut f,
                "MU5250",
                Err("x".into()),
                Err("x".into()),
                read_rails(),
            );
            f
        };
        // 从没读成功：和读失败一样。
        assert_eq!(render(&hub), failed);
        assert!(failed.is_empty());
        let now = tokio::time::Instant::now();
        hub.record_read(
            0,
            Ok(json!({"battery_capacity":80,"battery_online":1})),
            now,
        );
        hub.record_read(
            1,
            Ok(json!({"direct_power_supply_mode":"enable","charge_status":1})),
            now,
        );
        let fresh = render(&hub);
        assert_eq!(fresh["battery"]["percent"], 80);
        assert_eq!(fresh["battery"]["charging"], 1);
        assert_eq!(fresh["power"]["direct_supply"]["enabled"], true);
        // 读失败 → stale：旧接口不给保留的旧值，按读失败输出；/v2 仍带着旧值。
        hub.record_read(0, Err("timeout".into()), now);
        hub.record_read(1, Err("timeout".into()), now);
        assert_eq!(render(&hub), failed);
        assert_eq!(hub.view("battery").unwrap().data["percent"], 80);
        // 只有电池 stale：电池不出，充电器照常。
        hub.record_read(1, Ok(json!({"direct_power_supply_mode":"disable"})), now);
        let f = render(&hub);
        assert!(!f.contains_key("battery"));
        assert_eq!(f["power"]["direct_supply"]["enabled"], false);
        // max_age 过期导致的 stale 同样按失败输出。
        hub.record_read(0, Ok(json!({"battery_capacity":81})), now);
        hub.round_end(now + Duration::from_secs(16), Duration::from_secs(1));
        assert_eq!(render(&hub), failed);
    }

    #[test]
    fn v2_block_data_same_shape_as_legacy_state() {
        use crate::block::{Hub, NoSink, phase1_blocks};
        let hub = Hub::new(phase1_blocks(), Box::new(NoSink));
        let now = tokio::time::Instant::now();
        let bat = json!({"battery_capacity":80,"battery_online":1,"battery_temperature":300});
        let chg =
            json!({"direct_power_supply_mode":"enable","charge_status":1,"charger_connect":1});
        hub.record_read(0, Ok(bat.clone()), now);
        hub.record_read(1, Ok(chg.clone()), now);
        let mut f = Map::new();
        power_fields(&mut f, "MU5250", Ok(bat), Ok(chg), read_rails());
        // 电池先读、充电器后读：充电器读成功后电池的 data 跟着重算。
        assert_eq!(hub.view("battery").unwrap().data, f["battery"]);
        assert_eq!(hub.view("battery").unwrap().data["charging"], 1);
        assert_eq!(hub.view("charger").unwrap().data, f["power"]);
        // 充电器回复里没有 direct_power_supply_mode：旧 /state 没有 power，/v2 是 {}。
        hub.record_read(1, Ok(json!({"charge_status":0})), now);
        assert_eq!(hub.view("charger").unwrap().data, json!({}));
        assert_eq!(hub.view("battery").unwrap().data["charging"], 0);
        // 电池自己读成功 1 次、跟着充电器变 2 次。
        assert_eq!(hub.view("battery").unwrap().revision, 3);
        // 充电器回复没变：电池不重算、不发。
        hub.record_read(1, Ok(json!({"charge_status":0})), now);
        assert_eq!(hub.view("battery").unwrap().revision, 3);
        // 旧接口拿到的仍是原始回复。
        assert_eq!(hub.legacy("charger"), Ok(json!({"charge_status":0})));
    }

    #[test]
    fn profile() {
        // lower-cased, every non-alphanumeric becomes `_`
        for (raw, want) in [("MU5250", "mu5250"), ("MC7523 HW1.0", "mc7523_hw1_0")] {
            assert_eq!(normalize_profile(raw), want);
        }
    }
    #[test]
    fn integer_or_default() {
        assert_eq!(integer_or(&json!({}), "missing", -1), -1);
        assert_eq!(integer_or(&json!({"x": 5}), "x", -1), 5);
        assert_eq!(integer(&json!({}), "missing"), 0);
    }
    fn wireless(pairs: &[(&str, &str)]) -> Vec<BTreeMap<String, String>> {
        vec![
            pairs
                .iter()
                .map(|(k, v)| (format!("wireless.{k}"), v.to_string()))
                .collect(),
        ]
    }
    #[test]
    fn wifi_section_mu5250_sticks_to_main_2g() {
        // 2G disabled, 5G up: the C U60 path still reports main_2g.
        let sets = wireless(&[
            ("main_2g.ssid", "home"),
            ("main_2g.disabled", "1"),
            ("main_5g.ssid", "home-5g"),
        ]);
        assert_eq!(wifi_section("MU5250", &sets), Some("main_2g"));
        assert_eq!(wifi_section("MU5252", &sets), Some("main_5g"));
    }
    #[test]
    fn wifi_section_mu5250_hidden_without_main_2g_data() {
        let sets = wireless(&[("main_5g.ssid", "only-5g")]);
        assert_eq!(wifi_section("MU5250", &sets), None);
        assert_eq!(wifi_section("legacy_compat", &sets), Some("main_5g"));
        let key_only = wireless(&[("main_2g.key", "secret")]);
        assert_eq!(wifi_section("MU5250", &key_only), Some("main_2g"));
        assert_eq!(wifi_section("MU5252", &key_only), None);
    }
    #[test]
    fn iface() {
        // an empty netifd reply is a down interface without addresses
        let empty = interface(&json!({}));
        assert!(empty["up"] == false && empty["ipv4"] == json!([]));
    }
    /// 旧采集读到的对象（块的两个也算），mock ubusd 上都注册上，正常时回 `{}`。
    const COLLECT_CALLS: &[(&str, &str)] = &[
        ("zwrt_bsp.battery", "list"),
        ("zwrt_bsp.charger", "list"),
        ("zwrt_zte_mdm.api", "get_zwrt_common_info"),
        ("zwrt_zte_mdm.api", "get_sim_info"),
        ("zwrt_zte_mdm.api", "get_imei"),
        ("system", "board"),
        ("system", "info"),
        ("zte_nwinfo_api", "nwinfo_get_netinfo"),
        ("zwrt_data", "get_wwandst"),
        ("zwrt_data", "get_wwandst_monthlimit"),
        ("zwrt_data", "get_wwandst_clearday"),
        ("zwrt_data", "get_wwaniface"),
        ("zwrt_router.api", "router_lan_access_list"),
        ("zwrt_router.api", "router_wireless_access_list"),
        ("zwrt_router.api", "router_get_status_no_auth"),
        ("zwrt_router.api", "router_get_lan_info"),
        ("zwrt_bsp.thermal", "get_cpu_temp"),
        ("uci", "get"),
        ("zwrt_bsp.usb", "list"),
        ("zwrt_nfc", "zwrt_nfc_wifi_get"),
        ("zwrt_wms", "zwrt_wms_get_wms_capacity"),
        ("zwrt_wms", "zte_libwms_get_sms_data"),
        ("network.interface.lan", "status"),
        ("network.interface.zte_wan", "status"),
        ("network.interface.zte_wan6", "status"),
    ];

    /// V2-46：迁移用的块和旧 /state 的同名对象同形；来源读不到（或旧 /state 没生成）就 stale。
    #[tokio::test(flavor = "current_thread")]
    async fn v2_legacy_mirror_blocks_same_shape_and_stale() {
        use crate::ubus::mock_ubusd::{Action, MockUbusd};
        let _serial = CACHE_TEST.lock().await;
        invalidate_cache();
        let m = MockUbusd::start_new().await;
        let mut objects: Vec<&str> = COLLECT_CALLS.iter().map(|(o, _)| *o).collect();
        objects.dedup();
        for (o, method) in COLLECT_CALLS {
            let id = 0x100 + objects.iter().position(|x| x == o).unwrap() as u32;
            m.add_method(o, id, method, json!({}));
        }
        let hub = std::sync::Arc::new(crate::block::Hub::new(
            crate::block::phase1_blocks(),
            Box::new(crate::block::NoSink),
        ));
        let exec = crate::executor::Executor::spawn(
            m.backend(Duration::from_millis(300)),
            hub.clone(),
            crate::executor::Config::default(),
            Duration::from_secs(1),
        );
        let h = hub.clone();
        let snap = exec.round_now(async move { collect(1000, &h).await }).await;
        for name in ["qos", "clients", "interfaces"] {
            let v = hub.view(name).unwrap();
            assert!(!v.stale, "{name}");
            assert_eq!(v.data, snap.fields[name], "{name}");
        }
        // mock 回 {}：没有无线 section、没有 NFC、uci 是空的。
        for name in ["wlan", "nfc", "dhcp", "uci_device_info"] {
            assert!(hub.view(name).unwrap().stale, "{name}");
        }
        // 终端列表读失败：clients stale，别的不受影响。
        invalidate_cache();
        for method in ["router_lan_access_list", "router_wireless_access_list"] {
            for _ in 0..4 {
                m.script("zwrt_router.api", method, Action::Hang);
            }
        }
        let h = hub.clone();
        exec.round_now(async move { collect(1000, &h).await }).await;
        assert!(hub.view("clients").unwrap().stale);
        assert!(!hub.view("interfaces").unwrap().stale);
    }

    /// V2-47：`sms_list` 块 = 旧 `/state` 的 `sms.list`（解密、去重后的明文）；列表读失败就 stale。
    #[tokio::test(flavor = "current_thread")]
    async fn v2_sms_list_block_mirrors_legacy_list() {
        use crate::ubus::mock_ubusd::{Action, MockUbusd};
        let _serial = CACHE_TEST.lock().await;
        invalidate_cache();
        let m = MockUbusd::start_new().await;
        let mut objects: Vec<&str> = COLLECT_CALLS.iter().map(|(o, _)| *o).collect();
        objects.dedup();
        for (o, method) in COLLECT_CALLS {
            let id = 0x100 + objects.iter().position(|x| x == o).unwrap() as u32;
            let reply = if *method == "zte_libwms_get_sms_data" {
                json!({"messages":[{"id":"7","num":"10086","date":"26,08,27,04,00,00,+,0","tag":"1","text":"6D4B8BD5"}]})
            } else {
                json!({})
            };
            m.add_method(o, id, method, reply);
        }
        let hub = std::sync::Arc::new(crate::block::Hub::new(
            crate::block::phase1_blocks(),
            Box::new(crate::block::NoSink),
        ));
        let exec = crate::executor::Executor::spawn(
            m.backend(Duration::from_millis(300)),
            hub.clone(),
            crate::executor::Config::default(),
            Duration::from_secs(1),
        );
        let h = hub.clone();
        let snap = exec.round_now(async move { collect(1000, &h).await }).await;
        let v = hub.view("sms_list").unwrap();
        assert!(!v.stale);
        assert_eq!(v.data["list"], snap.fields["sms"]["list"]);
        assert_eq!(v.data["list"][0]["text"], "测试");
        assert_eq!(v.data["list"][0]["unread"], 1);
        // 列表读失败：sms_list stale（旧值保留），旧 /state 照 V2-29 没有这条短信。
        invalidate_cache();
        for _ in 0..8 {
            m.script("zwrt_wms", "zte_libwms_get_sms_data", Action::Hang);
        }
        let h = hub.clone();
        exec.round_now(async move { collect(1000, &h).await }).await;
        let v = hub.view("sms_list").unwrap();
        assert!(v.stale);
        assert_eq!(v.data["list"][0]["text"], "测试");
    }

    /// P1-2（V2-19）：ubusd 整个不回（mock 的 `Hang`：不回也不断开）时，旧采集到了 `LEGACY_TIMEOUT_LIMIT`
    /// 个超时对象就不再发新请求，一轮的长度有上限；派生块（信号、live）这一轮就置 stale。
    #[tokio::test(flavor = "current_thread")]
    async fn legacy_round_is_bounded_when_ubusd_hangs() {
        use crate::ubus::mock_ubusd::{Action, MockUbusd};
        let _serial = CACHE_TEST.lock().await;
        invalidate_cache();
        let m = MockUbusd::start_new().await;
        let mut objects: Vec<&str> = COLLECT_CALLS.iter().map(|(o, _)| *o).collect();
        objects.dedup();
        for (o, method) in COLLECT_CALLS {
            let id = 0x100 + objects.iter().position(|x| x == o).unwrap() as u32;
            m.add_method(o, id, method, json!({}));
        }
        let timeout = Duration::from_millis(300);
        let hub = std::sync::Arc::new(crate::block::Hub::new(
            crate::block::phase1_blocks(),
            Box::new(crate::block::NoSink),
        ));
        let exec = crate::executor::Executor::spawn(
            m.backend(timeout),
            hub.clone(),
            crate::executor::Config::default(),
            Duration::from_secs(1),
        );
        // 第一轮正常：信号、live 块读到了。
        let h = hub.clone();
        exec.round_now(async move { collect(1000, &h).await }).await;
        assert!(!hub.view("signal").unwrap().stale);
        assert!(!hub.view("live").unwrap().stale);
        // ubusd 不回了（慢数据缓存也清掉：最坏情况，每个对象都要读）。
        invalidate_cache();
        for (o, method) in COLLECT_CALLS {
            for _ in 0..4 {
                m.script(o, method, Action::Hang);
            }
        }
        let invokes = |m: &MockUbusd| objects.iter().map(|o| m.invokes(o)).sum::<u64>();
        let before = invokes(&m);
        let h = hub.clone();
        let t0 = std::time::Instant::now();
        let snap = exec.round_now(async move { collect(1000, &h).await }).await;
        let took = t0.elapsed();
        let sent = invokes(&m) - before;
        // 每个超时的对象只发一次（本轮跳过），到上限就停：发出去的请求数就是上限。
        // 没有上限时是 objects.len() 个，一轮约 objects.len() × 300 ms。
        assert_eq!(
            sent,
            crate::executor::LEGACY_TIMEOUT_LIMIT as u64,
            "{} objects, took {took:?}",
            objects.len()
        );
        // the count above is the bound; the wall clock only catches a runaway, with room for a
        // loaded test host (the whole suite runs in parallel)
        assert!(
            took < timeout * (crate::executor::LEGACY_TIMEOUT_LIMIT as u32 + 3) * 2,
            "took {took:?}"
        );
        assert!(hub.view("signal").unwrap().stale);
        assert!(hub.view("live").unwrap().stale);
        assert!(hub.view("sim").unwrap().stale);
        // 旧 /state 照今天读失败的样子输出，字段都在
        assert!(snap.fields.contains_key("net") && snap.fields.contains_key("system"));
    }

    /// 执行者这次没发请求（`Skipped`）时，有 TTL 的读取沿用上一次读成功的值（过期了也给），
    /// 但不算读到；缓存里的上一次结果不被覆盖。TTL 为 0 的照旧读失败。
    #[tokio::test(flavor = "current_thread")]
    async fn skipped_read_keeps_the_previous_value_but_is_not_fresh() {
        use crate::ubus::mock_ubusd::{Action, MockUbusd};
        let _serial = CACHE_TEST.lock().await;
        invalidate_cache();
        let m = MockUbusd::start_new().await;
        m.add_method("p12.svc", 0x10, "slow", json!({"v":1}));
        m.add_method("p12.svc", 0x10, "hang", json!({}));
        m.add_method("p12.svc", 0x10, "now", json!({"n":1}));
        let hub = std::sync::Arc::new(crate::block::Hub::new(
            vec![],
            Box::new(crate::block::NoSink),
        ));
        let exec = crate::executor::Executor::spawn(
            m.backend(Duration::from_millis(200)),
            hub,
            crate::executor::Config::default(),
            Duration::from_secs(1),
        );
        let first = exec
            .round_now(async { ubus_ttl_read(1, "p12.svc", "slow", json!({})).await })
            .await;
        assert_eq!(first, (Ok(json!({"v":1})), true));
        tokio::time::sleep(Duration::from_millis(1100)).await; // TTL 过了
        m.script("p12.svc", "hang", Action::Hang);
        let (slow, now) = exec
            .round_now(async {
                // 同一个对象先超时一次：本轮之后的调用都跳过
                let _ = ubus_ttl(0, "p12.svc", "hang", json!({})).await;
                (
                    ubus_ttl_read(1, "p12.svc", "slow", json!({})).await,
                    ubus_ttl_read(0, "p12.svc", "now", json!({})).await,
                )
            })
            .await;
        assert_eq!(slow, (Ok(json!({"v":1})), false));
        assert!(now.0.is_err() && !now.1, "{now:?}");
        // 跳过的那次没把读失败写进缓存（原来会存 5 秒的失败，冲掉上一次的值）
        assert_eq!(
            cache_last_ok(&UBUS_CACHE, &format!("p12.svc\u{0}slow\u{0}{}", json!({}))),
            Some(json!({"v":1}))
        );
    }

    #[test]
    fn connected_clients_ignore_historical_leases() {
        // Only the vendor's live lists count; the lease file (whatever is on
        // this host) must not add clients.
        let live_wifi = json!({"wireless_access_list_info": [
            {"mac_address": "AA:BB:CC:DD:EE:FF", "ip_address": "192.168.0.2", "hostname": "online"}
        ]});
        let no_lan = json!({"lan_access_list_info": []});
        let (items, wifi, lan) = connected_clients(&no_lan, &live_wifi, &lease_metadata());
        assert_eq!((wifi, lan, items.len()), (1, 0, 1));
        assert_eq!(items[0].mac, "aa:bb:cc:dd:ee:ff");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn firmware_capture_uses_physical_reads_not_cache_hits_or_skips() {
        use crate::ubus::mock_ubusd::{Action, MockUbusd};
        let _serial = CACHE_TEST.lock().await;
        invalidate_cache();
        let m = MockUbusd::start_new().await;
        let object = "zwrt_zte_mdm.api";
        let method = "get_zwrt_common_info";
        m.add_method(
            object,
            0x10,
            method,
            json!({"manufacturer":"ZTE", "model_name":"MU5250",
            "integrate_version":"CN_ZTE_MU5250V1.0.0B31"}),
        );
        m.add_method(object, 0x10, "hang", json!({}));
        let hub = std::sync::Arc::new(crate::block::Hub::new(
            vec![],
            Box::new(crate::block::NoSink),
        ));
        let exec = crate::executor::Executor::spawn(
            m.backend(Duration::from_millis(50)),
            hub.clone(),
            crate::executor::Config::default(),
            Duration::from_secs(1),
        );
        let h = hub.clone();
        exec.round_now(async move {
            profile_ubus(&h, 60, object, method).await.unwrap();
        })
        .await;
        let before = hub
            .firmware()
            .report(luyaz_api::clock::BootTime::read().as_ref());
        let count = m.invokes(object);
        let h = hub.clone();
        exec.round_now(async move {
            profile_ubus(&h, 60, object, method).await.unwrap();
        })
        .await;
        assert_eq!(m.invokes(object), count);
        assert_eq!(before.evidence, hub.firmware().report(None).evidence);
        // Force a physical-read path, then make the executor skip it after the object's timeout.
        invalidate_cache();
        m.script(object, "hang", Action::Hang);
        let h = hub.clone();
        exec.round_now(async move {
            let _ = ubus_ttl(0, object, "hang", json!({})).await;
            assert!(profile_ubus(&h, 60, object, method).await.is_err());
        })
        .await;
        assert_eq!(before.evidence, hub.firmware().report(None).evidence);
        assert_eq!(m.invokes(object), count + 1); // only the existing hang call was sent
        invalidate_cache();
    }
}

#[cfg(test)]
mod sim_identity_stamp_tests {
    use super::*;

    #[test]
    fn a_failed_sim_read_is_not_fed_to_the_identity_tracking() {
        let before = {
            let s = crate::sim_identity::global().lock().unwrap();
            (s.epoch(), s.phase())
        };
        let mut fields = serde_json::Map::new();
        fields.insert("sim".into(), json!({"iccid": "", "imsi": ""}));
        let mut blocks: Vec<(&'static str, Result<Value, String>)> =
            vec![("sim", Err("zwrt_zte_mdm.api get_sim_info failed".into()))];
        sim_identity_stamp(&mut fields, &mut blocks, std::time::Instant::now());
        let s = crate::sim_identity::global().lock().unwrap();
        assert_eq!((s.epoch(), s.phase()), before);
        assert_eq!(fields["sim"]["epoch"], json!(before.0));
    }
}
