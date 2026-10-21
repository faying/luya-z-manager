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
/// APN 块的四个读（60 秒缓存；写过之后 `invalidate_cache` 让下一轮立即读）。任何一个失败整块 stale。
async fn apn_reads() -> Result<[Value; 4], String> {
    let o = "zwrt_apn_object";
    Ok([
        ubus_ttl(60, o, "get_apn_mode", json!({})).await?,
        ubus_ttl(60, o, "get_apn_at_cid", json!({"cid": 1})).await?,
        ubus_ttl(60, o, "get_manu_apn_list", json!({})).await?,
        ubus_ttl(60, o, "get_auto_apn_list", json!({})).await?,
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
    let key = format!("{service}\u{0}{method}\u{0}{args}");
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
fn uci_parsed(package: &str) -> Option<crate::uci::Package> {
    if std::env::var("LUYAZ_UCI_PARSE").as_deref() == Ok("0") {
        return None;
    }
    crate::uci::load(package, &uci_config_dir(), &uci_save_dir())
}

/// `uci show` 文本 → `路径 → 值`（值去掉最外层单引号）。解析路径和子进程路径共用。
fn uci_show_map(raw: &[u8]) -> BTreeMap<String, String> {
    String::from_utf8_lossy(raw)
        .lines()
        .filter_map(|line| {
            let (k, raw) = line.split_once('=')?;
            let v = raw
                .strip_prefix('\'')
                .and_then(|x| x.strip_suffix('\''))
                .unwrap_or(raw);
            Some((k.into(), v.into()))
        })
        .collect()
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
        return BTreeMap::new();
    }
    let p = package.to_owned();
    let parsed = blocking(move || {
        uci_parsed(&p).map(|pkg| uci_show_map(&crate::uci::render_show(&p, &pkg)))
    })
    .await;
    if let Some(map) = parsed {
        return map;
    }
    let Ok(raw) = command::run(&uci_bin(), ["-q", "show", package], Duration::from_secs(5)).await
    else {
        return BTreeMap::new();
    };
    uci_show_map(&raw)
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
    if path.is_empty()
        || path.len() > 256
        || !path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-@[]".contains(&b))
    {
        return String::new();
    }
    command::run(&uci_bin(), ["-q", "get", path], Duration::from_secs(5))
        .await
        .ok()
        .map(|raw| String::from_utf8_lossy(&raw).trim().to_owned())
        .unwrap_or_default()
}
pub async fn uci_write(operation: &str, path: &str, value: Option<&str>) -> Result<(), String> {
    if !matches!(
        operation,
        "set" | "add_list" | "del_list" | "delete" | "commit" | "revert"
    ) || path.is_empty()
        || path.len() > 256
        || !path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-@[]".contains(&b))
        || value.is_some_and(|v| v.len() > 8192 || v.contains(['\0', '\r', '\n']))
    {
        return Err("invalid UCI mutation".into());
    }
    let mut args = vec![operation.to_owned()];
    if matches!(operation, "set" | "add_list" | "del_list") {
        let Some(value) = value else {
            return Err("missing UCI value".into());
        };
        args.push(format!("{path}={value}"));
    } else {
        args.push(path.to_owned());
    }
    command::run(&uci_bin(), args, Duration::from_secs(5))
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn tcp_aggregation_summary() -> (usize, String, u16, bool) {
    let path = std::env::var("LUYAZ_PROC_NET_TCP").unwrap_or_else(|_| "/proc/net/tcp".into());
    let owned: Vec<u64> = std::env::var("LUYAZ_ICG_SOCKET_INODES")
        .unwrap_or_default()
        .split(',')
        .filter_map(|v| v.trim().parse().ok())
        .collect();
    if owned.is_empty() {
        return (0, String::new(), 0, false);
    }
    let text = fs::read_to_string(path).unwrap_or_default();
    let mut listeners = Vec::new();
    let mut established = Vec::new();
    for line in text.lines().skip(1) {
        let columns: Vec<_> = line.split_whitespace().collect();
        if columns.len() < 10 {
            continue;
        }
        let inode = columns.last().and_then(|v| v.parse::<u64>().ok());
        if !inode.is_some_and(|v| owned.contains(&v)) {
            continue;
        }
        let local = columns[1].split_once(':');
        let remote = columns[2].split_once(':');
        if columns[3] == "0A" {
            if let Some((_, port)) = local {
                listeners.push(u16::from_str_radix(port, 16).unwrap_or_default());
            }
        } else if columns[3] == "01"
            && let (Some((_, local_port)), Some((address, remote_port))) = (local, remote)
        {
            established.push((
                u16::from_str_radix(local_port, 16).unwrap_or_default(),
                address.to_owned(),
                u16::from_str_radix(remote_port, 16).unwrap_or_default(),
            ));
        }
    }
    let outgoing: Vec<_> = established
        .into_iter()
        .filter(|(port, _, _)| !listeners.contains(port))
        .collect();
    let (ip, port) = outgoing
        .first()
        .map(|(_, address, port)| {
            let raw = u32::from_str_radix(address, 16)
                .unwrap_or_default()
                .to_le_bytes();
            (
                format!("{}.{}.{}.{}", raw[0], raw[1], raw[2], raw[3]),
                *port,
            )
        })
        .unwrap_or_default();
    (outgoing.len(), ip, port, true)
}

fn read_i64(path: impl AsRef<Path>) -> i64 {
    fs::read_to_string(path)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or_default()
}
fn thermal_zones() -> (i64, Value, Vec<crate::contract::ThermalZone>) {
    let mut zones = Vec::new();
    let mut runtime_zones = Vec::new();
    let mut cpuss = Vec::new();
    let root =
        std::env::var("LUYAZ_THERMAL_ROOT").unwrap_or_else(|_| "/sys/class/thermal".into());
    let Ok(entries) = fs::read_dir(root) else {
        return (0, json!([]), Vec::new());
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with("thermal_zone")
        {
            continue;
        }
        let name = fs::read_to_string(p.join("type"))
            .unwrap_or_default()
            .trim()
            .to_owned();
        let raw = read_i64(p.join("temp"));
        if name.is_empty() || raw <= -40_000 || raw == 0 || raw > 200_000 {
            continue;
        }
        let c = raw as f64 / 1000.0;
        if name.starts_with("cpuss") {
            cpuss.push(c)
        }
        runtime_zones.push(crate::contract::ThermalZone {
            r#type: name.clone(),
            temp_milli: raw,
        });
        zones.push(json!({"name":name,"celsius":c}));
    }
    zones.sort_by_key(|a| string(a, "name"));
    runtime_zones.sort_by(|a, b| a.r#type.cmp(&b.r#type));
    let cpu = if cpuss.is_empty() {
        zones
            .iter()
            .filter_map(|v| v.get("celsius").and_then(Value::as_f64))
            .fold(0.0, f64::max)
    } else {
        cpuss.iter().sum::<f64>() / cpuss.len() as f64
    };
    (cpu.round() as i64, Value::Array(zones), runtime_zones)
}
/// 充电口和电池的电压、电流（`battery` 对象的 chg_*/bat_*）。
fn read_rails() -> Rails {
    Rails {
        chg_uv: read_i64(host_path("/sys/class/power_supply/usb/voltage_now")),
        chg_ua: read_i64(host_path("/sys/class/power_supply/usb/current_now")),
        bat_uv: read_i64(host_path("/sys/class/power_supply/battery/voltage_now")),
        bat_ua: read_i64(host_path("/sys/class/power_supply/battery/current_now")),
    }
}
fn lease_metadata() -> Leases {
    let path =
        std::env::var("LUYAZ_DHCP_LEASES_PATH").unwrap_or_else(|_| "/tmp/dhcp.leases".into());
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let p: Vec<_> = line.split_whitespace().collect();
            (p.len() >= 4).then(|| {
                (
                    p[1].to_ascii_lowercase(),
                    (
                        p[2].to_owned(),
                        if p[3] == "*" { "" } else { p[3] }.to_owned(),
                    ),
                )
            })
        })
        .collect()
}

type CpuCounters = BTreeMap<String, (u64, u64)>;
static CPU_PREVIOUS: OnceLock<Mutex<CpuCounters>> = OnceLock::new();

fn count_lines(path: &str, header: bool) -> u64 {
    let count = fs::read_to_string(path)
        .map(|v| v.lines().count() as u64)
        .unwrap_or_default();
    count.saturating_sub(u64::from(header && count > 0))
}

fn tcp_active() -> u64 {
    fs::read_to_string(host_path("/proc/net/tcp"))
        .unwrap_or_default()
        .lines()
        .skip(1)
        .filter(|line| line.split_whitespace().nth(3) == Some("01"))
        .count() as u64
}

/// 测试用：LUYAZ_HOST_ROOT 设了时，下面这些写死的 /proc、/sys、/data 路径整体挪到它下面，
/// 让 golden（tests/golden/）不受宿主机影响。设备上不设，路径和原来完全一样。
pub(crate) fn host_path(path: &str) -> String {
    static ROOT: OnceLock<Option<String>> = OnceLock::new();
    match ROOT.get_or_init(|| {
        std::env::var("LUYAZ_HOST_ROOT")
            .ok()
            .filter(|v| !v.is_empty())
    }) {
        Some(root) => format!("{root}{path}"),
        None => path.to_owned(),
    }
}

fn meminfo() -> crate::contract::MemoryKb {
    let mut values = BTreeMap::new();
    let contents = fs::read_to_string(host_path("/proc/meminfo")).unwrap_or_default();
    for line in contents.lines() {
        if let Some((key, rest)) = line.split_once(':') {
            values.insert(
                key.to_owned(),
                rest.split_whitespace()
                    .next()
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or_default(),
            );
        }
    }
    let kb = |key: &str| values.get(key).copied().unwrap_or_default();
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

// libc exposes statvfs counters with different integer widths across targets.
#[allow(clippy::unnecessary_cast)]
fn storage() -> crate::contract::Storage {
    let Ok(path) = CString::new(host_path("/data")) else {
        return Default::default();
    };
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut stat) } != 0 {
        return Default::default();
    }
    let block = if stat.f_frsize > 0 {
        stat.f_frsize
    } else {
        stat.f_bsize
    } as u64;
    let total = block.saturating_mul(stat.f_blocks as u64);
    let free = block.saturating_mul(stat.f_bfree as u64);
    crate::contract::Storage {
        total,
        used: total.saturating_sub(free),
        available: block.saturating_mul(stat.f_bavail as u64),
    }
}

#[derive(Clone, Copy)]
struct SpeedSample {
    rx: u64,
    tx: u64,
    at_ms: u64,
}

static LINK_PREVIOUS: OnceLock<Mutex<BTreeMap<String, SpeedSample>>> = OnceLock::new();
static SPEED_RING: OnceLock<Mutex<VecDeque<SpeedSample>>> = OnceLock::new();

fn iface_bytes(name: &str) -> Option<(u64, u64)> {
    let root =
        std::env::var("LUYAZ_NET_CLASS_ROOT").unwrap_or_else(|_| "/sys/class/net".into());
    let path = Path::new(&root).join(name).join("statistics");
    let rx = fs::read_to_string(path.join("rx_bytes"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let tx = fs::read_to_string(path.join("tx_bytes"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some((rx, tx))
}

fn now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START
        .get_or_init(Instant::now)
        .elapsed()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn link_rates(now: u64) -> Vec<crate::contract::LinkRate> {
    let previous = LINK_PREVIOUS.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut previous = previous.lock().unwrap();
    let mut rates = Vec::new();
    for name in ["rmnet_data0", "V3E1net0", "V3E2net0", "eth0"] {
        let Some((rx, tx)) = iface_bytes(name) else {
            previous.remove(name);
            continue;
        };
        let old = previous.get(name).copied();
        let window = old.map_or(0, |sample| now.saturating_sub(sample.at_ms));
        let valid = old.is_some_and(|sample| window > 0 && rx >= sample.rx && tx >= sample.tx);
        let bps = |now: u64, then: u64| {
            old.filter(|_| valid)
                .map(|_| now.saturating_sub(then).saturating_mul(1000) / window)
        };
        rates.push(crate::contract::LinkRate {
            interface: name.into(),
            source: "netdev".into(),
            window_ms: if valid { window } else { 0 },
            rx_bytes: rx,
            tx_bytes: tx,
            rx_bps: bps(rx, old.map_or(0, |o| o.rx)),
            tx_bps: bps(tx, old.map_or(0, |o| o.tx)),
        });
        previous.insert(name.into(), SpeedSample { rx, tx, at_ms: now });
    }
    rates
}

fn user_bytes() -> (u64, u64) {
    if let Some((rx, tx)) = iface_bytes("br-lan") {
        return (tx, rx);
    }
    let wifi: Vec<_> = ["wlan0", "wlan2"]
        .into_iter()
        .filter_map(iface_bytes)
        .collect();
    if !wifi.is_empty() {
        return wifi
            .into_iter()
            .fold((0_u64, 0_u64), |(rx, tx), (raw_rx, raw_tx)| {
                (rx.saturating_add(raw_tx), tx.saturating_add(raw_rx))
            });
    }
    ["rmnet_data0", "rmnet_ipa0"]
        .into_iter()
        .filter_map(iface_bytes)
        .fold((0_u64, 0_u64), |(rx, tx), (raw_rx, raw_tx)| {
            (rx.saturating_add(raw_tx), tx.saturating_add(raw_rx))
        })
}

fn throughput(now: u64) -> crate::contract::Throughput {
    let (rx, tx) = user_bytes();
    let ring = SPEED_RING.get_or_init(|| Mutex::new(VecDeque::with_capacity(16)));
    let mut ring = ring.lock().unwrap();
    if ring.len() == 16 {
        ring.pop_front();
    }
    ring.push_back(SpeedSample { rx, tx, at_ms: now });
    let Some(old) = ring.front().copied().filter(|_| ring.len() >= 2) else {
        return Default::default();
    };
    let window = now.saturating_sub(old.at_ms);
    if window == 0 {
        return Default::default();
    }
    crate::contract::Throughput {
        rx_bps: rx.saturating_sub(old.rx).saturating_mul(1000) / window,
        tx_bps: tx.saturating_sub(old.tx).saturating_mul(1000) / window,
        window_ms: window,
    }
}

fn runtime(runtime_zones: Vec<crate::contract::ThermalZone>) -> (i64, crate::contract::Runtime) {
    let mut current = BTreeMap::new();
    for line in fs::read_to_string(host_path("/proc/stat"))
        .unwrap_or_default()
        .lines()
    {
        let mut parts = line.split_whitespace();
        let Some(label) = parts.next() else { continue };
        if label != "cpu"
            && !label
                .strip_prefix("cpu")
                .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        {
            continue;
        }
        let nums: Vec<u64> = parts
            .take(8)
            .map(|v| v.parse().unwrap_or_default())
            .collect();
        if nums.len() < 4 {
            continue;
        }
        let total: u64 = nums.iter().sum();
        let idle = nums[3] + nums.get(4).copied().unwrap_or_default();
        current.insert(label.to_owned(), (total, idle));
    }
    let previous = CPU_PREVIOUS.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut old = previous.lock().unwrap();
    let mut usage = BTreeMap::new();
    let mut total_usage = -1;
    for (label, (total, idle)) in &current {
        let value = old
            .get(label)
            .and_then(|(ot, oi)| {
                let dt = total.saturating_sub(*ot);
                let di = idle.saturating_sub(*oi);
                (dt > 0).then(|| ((dt.saturating_sub(di)) * 1000 / dt) as i64)
            })
            .unwrap_or(-1);
        if label == "cpu" {
            total_usage = value
        } else {
            usage.insert(label.clone(), value);
        }
    }
    *old = current;
    drop(old);
    let mut freqs = BTreeMap::new();
    for core in usage.keys() {
        let root = host_path(&format!("/sys/devices/system/cpu/{core}/cpufreq"));
        let mut cur = read_i64(format!("{root}/scaling_cur_freq"));
        let mut max = read_i64(format!("{root}/scaling_max_freq"));
        if cur == 0 {
            cur = read_i64(format!("{root}/cpuinfo_cur_freq"))
        }
        if max == 0 {
            max = read_i64(format!("{root}/cpuinfo_max_freq"))
        }
        freqs.insert(
            core.clone(),
            crate::contract::Freq {
                cur: cur / 1000,
                max: max / 1000,
            },
        );
    }
    let active = tcp_active();
    let tcp4 = count_lines(&host_path("/proc/net/tcp"), true);
    let lines = |path: &str| count_lines(&host_path(path), true);
    let connections = crate::contract::Connections {
        tcp_active: active,
        tcp_other: tcp4.saturating_sub(active),
        tcp4,
        tcp6: lines("/proc/net/tcp6"),
        udp4: lines("/proc/net/udp"),
        udp6: lines("/proc/net/udp6"),
        unix: lines("/proc/net/unix"),
    };
    let now = now_ms();
    (
        total_usage,
        crate::contract::Runtime {
            cpu_usage_tenths: total_usage,
            cpu_cores: usage,
            cpu_freq_mhz: freqs,
            thermal_zones: runtime_zones,
            memory_kb: meminfo(),
            storage: storage(),
            connections,
            link_rates: link_rates(now),
            throughput: throughput(now),
        },
    )
}

/// 旧接口的一轮采集。电池、充电器已迁到块模型（`block::phase1_blocks`），从 `hub` 取；
/// stale 的块按读失败输出（V2-29）。其余读取仍走 `ubus_ttl`。
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
    // dual-SIM standby signal: tell the QMI sampler which slot to read (0 = none)
    let dual = fields.get("sim").and_then(|s| s.get("dual")).cloned();
    let want = dual
        .and_then(|d| serde_json::from_value::<crate::contract::SimDual>(d).ok())
        .map_or(0, |d| snapshot::standby_slot(&d));
    crate::qmi_nas::want(want);
    // dual-SIM passive log (dsds_log.rs): keeps the latest dual-mode fields, writes every 30 s
    crate::dsds_log::offer(&fields);
    let now = tokio::time::Instant::now();
    for (name, data) in blocks {
        hub.record(name, data, now);
    }
    Snapshot {
        ts: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64,
        core: Default::default(),
        fields,
    }
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
    let apn = apn_reads().await;
    let router = router_reads().await;
    // u60d：登记短信会话密钥（`zwrt_web web_http_enstr_set`）会顶掉另一个进程（当年并跑的 data-service）在厂商那里登记的那把，
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
    let (cpu_sys, zones, cpu_usage_tenths, runtime, qos_logs, leases, rails) = blocking(|| {
        let (cpu_sys, zones, runtime_zones) = thermal_zones();
        let (cpu_usage_tenths, runtime) = runtime(runtime_zones);
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
        &string(&object(sim.clone()), "sim_imsi"),
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

/// MU5252（TopFlow）的多模组部分：按卡槽、按外挂模组再调 ubus（和 adb），边读边算，
/// 补 `modems`、`thermal.modems`、`aggregation`、`multiwan`。Phase 2a 原样留在 IO 层，没拆成纯函数。
async fn mu5252_extras(ctx: Mu5252Ctx, fields: &mut Map<String, Value>) {
    let Mu5252Ctx {
        raw_net,
        imsi,
        msisdn,
        cpu_temp,
        sim,
        imei,
        cellular,
        wan4_if,
        wan6_if,
        uci_sets,
    } = ctx;
    let active_subid = integer(&sim, "current_sim_slot").clamp(1, 6);
    let x75_traffic = object(
        ubus(
            "zwrt_data",
            "get_wwandst",
            json!({"source_module":"deviceui","cid":1,"type":1,"subid":active_subid}),
        )
        .await,
    );
    let v3t = object(ubus("zwrt_zte_mdm.api", "get_v3t_sim_info", json!({})).await);
    let msim = object(ubus("zte_nwinfo_api", "nwinfo_get_msim_netinfo", json!({})).await);
    let mut modems = Vec::new();
    let network_type = string(&raw_net, "network_type");
    let bandwidth = if matches!(network_type.as_str(), "SA" | "NSA") {
        string(&raw_net, "nr5g_bandwidth")
    } else {
        string(&raw_net, "lte_bandwidth")
    };
    let x75_qos = crate::qos::read(qos_query(&raw_net, &imsi, &wan4_if, None));
    modems.push(json!({
        "id":"x75","role":"integrated_5g","transport":"rmnet","subid":active_subid,
        "ifname":"rmnet_data0","wan_interface":"zte_mwan2",
        "net":{"type":network_type,"bars":integer(&raw_net,"signalbar"),"roaming":string(&raw_net,"simcard_roam"),"operator":string(&raw_net,"network_provider_fullname"),"band":string(&raw_net,"wan_active_band"),"bandwidth":bandwidth,"nr_rsrp":integer(&raw_net,"nr5g_rsrp"),"nr_rsrq":integer(&raw_net,"nr5g_rsrq"),"nr_snr":string(&raw_net,"nr5g_snr"),"nr_pci":integer(&raw_net,"nr5g_pci"),"nr_cell_id":integer(&raw_net,"nr5g_cell_id"),"nr_channel":integer(&raw_net,"nr5g_action_channel"),"lte_rsrp":integer(&raw_net,"lte_rsrp"),"lte_rsrq":integer(&raw_net,"lte_rsrq"),"lte_pci":integer(&raw_net,"lte_pci"),"cell_id":integer(&raw_net,"cell_id")},
        "sim":{"state":string(&sim,"sim_states"),"slot":integer(&sim,"current_sim_slot"),"iccid":string(&sim,"sim_iccid"),"imsi":imsi,"msisdn":msisdn,"imei":string(&imei,"imei")},
        "wwan":{"status":string(&cellular,"connect_status"),"ipv4_ifname":string(&cellular,"ipv4_dev_name"),"ipv6_ifname":string(&cellular,"ipv6_dev_name")},
        "interfaces":{"ipv4":interface(&wan4_if),"ipv6":interface(&wan6_if)},
        "traffic":realtime_traffic(&x75_traffic),
        "qos":{"qci":x75_qos.qci,"ambr_dl":x75_qos.ambr_dl,"ambr_ul":x75_qos.ambr_ul,"bearer":x75_qos.bearer,"stale":x75_qos.stale,"sampled_at":0}
    }));
    let mut thermal_modems = vec![
        json!({"id":"x75","available":cpu_temp>0,"celsius":if cpu_temp>0 {json!(cpu_temp)} else {Value::Null}}),
    ];
    let adb = std::env::var("LUYAZ_ADB_BIN").ok();
    for index in 0..2usize {
        let id = if index == 0 { "v3e1" } else { "v3e2" };
        let serial = if index == 0 {
            "V3E1T12345"
        } else {
            "V3E2T12345"
        };
        let ifname = if index == 0 { "V3E1net0" } else { "V3E2net0" };
        let wan = if index == 0 { "zte_mwan3" } else { "zte_mwan4" };
        let usb_path = if index == 0 { "1-1" } else { "1-2" };
        let usb_id = if index == 0 { "19d2:0581" } else { "19d2:1716" };
        let sim_prefix = format!("v3t_{}", index + 1);
        let slot = integer(&v3t, &format!("{sim_prefix}_st_slot")).clamp(0, 1);
        let subid = if index == 0 { 3 + slot } else { 5 + slot };
        let ext_net = topflow_external_net(&msim, &uci_sets, index, slot);
        let wwan = object(
            ubus(
                "zwrt_data",
                "get_wwaniface",
                json!({"source_module":"deviceui","cid":1,"subid":subid}),
            )
            .await,
        );
        let ext_traffic = object(
            ubus(
                "zwrt_data",
                "get_wwandst",
                json!({"source_module":"deviceui","cid":1,"type":1,"subid":subid}),
            )
            .await,
        );
        let ipv4 = object(ubus(&format!("network.interface.{wan}"), "status", json!({})).await);
        let ipv6 = object(ubus(&format!("network.interface.{wan}_6"), "status", json!({})).await);
        let mut ext_qos = crate::qos::Values::default();
        let mut sampled_at = 0i64;
        let mut external_temp = None;
        if let Some(adb) = &adb {
            if let Ok(raw) = command::run(
                adb,
                [
                    "-s",
                    serial,
                    "shell",
                    "grep QCI= /logfs/key.log | tail -n 64",
                ],
                Duration::from_secs(5),
            )
            .await
                && let Some(parsed) = crate::qos::parse_external(&String::from_utf8_lossy(&raw))
            {
                ext_qos = parsed;
                sampled_at = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
            }
            if let Ok(raw) = command::run(
                adb,
                [
                    "-s",
                    serial,
                    "shell",
                    "cat",
                    "/sys/devices/virtual/power/zte_power/adc2_temp",
                ],
                Duration::from_secs(5),
            )
            .await
            {
                external_temp = String::from_utf8_lossy(&raw)
                    .trim()
                    .parse::<i64>()
                    .ok()
                    .filter(|v| (-40..=125).contains(v));
            }
        }
        thermal_modems.push(json!({"id":id,"available":external_temp.is_some(),"celsius":external_temp,"sampled_at":if external_temp.is_some(){sampled_at}else{0}}));
        modems.push(json!({
            "id":id,"role":"external_4g","transport":"cdc-ecm","subid":subid,"ifname":ifname,"wan_interface":wan,
            "usb":{"path":usb_path,"id":usb_id,"present":false,"carrier":0},
            "debug":{"transport":"adb","serial":serial,"available":adb.is_some()},
            "net":ext_net,
            "sim":{"state":prefixed_string(&v3t,&format!("{sim_prefix}_"),"modem_main_state"),"slot":slot,"iccid":prefixed_string(&v3t,&format!("{sim_prefix}_"),"sim_iccid"),"imsi":prefixed_string(&v3t,&format!("{sim_prefix}_"),"sim_imsi"),"msisdn":prefixed_string(&v3t,&format!("{sim_prefix}_"),"msisdn"),"imei":prefixed_string(&v3t,&format!("{sim_prefix}_"),"imei")},
            "wwan":{"status":string(&wwan,"connect_status"),"ipv4_ifname":string(&wwan,"ipv4_dev_name"),"ipv6_ifname":string(&wwan,"ipv6_dev_name")},
            "interfaces":{"ipv4":interface(&ipv4),"ipv6":interface(&ipv6)},
            "traffic":realtime_traffic(&ext_traffic),
            "qos":{"qci":ext_qos.qci,"ambr_dl":ext_qos.ambr_dl,"ambr_ul":ext_qos.ambr_ul,"sampled_at":sampled_at}
        }));
    }
    if let Some(thermal) = fields.get_mut("thermal") {
        thermal["modems"] = Value::Array(thermal_modems);
    }
    fields.insert("modems".into(), Value::Array(modems));
    let mode = uci_value("zwrt_router.network.opms_wan_mode").await;
    let mwan_running = std::env::var("LUYAZ_MWAN3_RUNNING")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .is_some_and(|v| v != 0);
    let mwan_status = object(ubus("mwan3", "status", json!({})).await);
    let mut paths = Vec::new();
    let interfaces = mwan_status
        .get("interfaces")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for (id, label, name) in [
        ("x75", "X75", "zte_mwan2"),
        ("v3e1", "V3E1", "zte_mwan3"),
        ("v3e2", "V3E2", "zte_mwan4"),
        ("ethernet", "Ethernet", "waneth"),
    ] {
        let Some(path) = interfaces.get(name) else {
            continue;
        };
        let enabled = parse_bool(path, "enabled");
        let running = parse_bool(path, "running");
        let mwan_up = parse_bool(path, "up");
        let iface = object(ubus(&format!("network.interface.{name}"), "status", json!({})).await);
        let interface_up = parse_bool(&iface, "up");
        let up = mwan_up || interface_up;
        if !enabled && !running && !up {
            continue;
        }
        let status = string(path, "status");
        let online = status.eq_ignore_ascii_case("online")
            || (running && mwan_up)
            || (!mwan_running && interface_up);
        let mut targets = Vec::new();
        if let Some(raw_targets) = path.get("track_ip").and_then(Value::as_array) {
            for target in raw_targets {
                let target_status = string(target, "status");
                let target_online =
                    matches!(target_status.to_ascii_lowercase().as_str(), "online" | "up");
                targets.push(json!({"ip":string(target,"ip"),"status":target_status,"online":target_online,"latency_ms":target.get("latency").cloned().unwrap_or(Value::Null),"packet_loss_percent":target.get("packetloss").cloned().unwrap_or(Value::Null)}));
            }
        }
        let preferred = targets
            .iter()
            .find(|target| target["online"] == true)
            .or_else(|| targets.iter().find(|target| target["status"] != "skipped"));
        let mut result = json!({"id":id,"label":label,"interface":name,"enabled":enabled,"running":running,"up":up,"online":online,"interface_up":interface_up,"interface_available":parse_bool(&iface,"available"),"interface_pending":parse_bool(&iface,"pending"),"status":status,"tracking":string(path,"tracking"),"uptime_seconds":integer(path,"uptime"),"targets":targets});
        if let Some(preferred) = preferred {
            if !preferred["latency_ms"].is_null() {
                result["latency_ms"] = preferred["latency_ms"].clone();
            }
            if !preferred["packet_loss_percent"].is_null() {
                result["packet_loss_percent"] = preferred["packet_loss_percent"].clone();
            }
        }
        paths.push(result);
    }
    let path_count = paths.len();
    let online_path_count = paths.iter().filter(|path| path["online"] == true).count();
    let (tcp_tunnel_count, runtime_ip, runtime_port, icg_running) =
        blocking(tcp_aggregation_summary).await;
    let provisioned = !uci_value("zwrt_router.icgmwan.IcgDevId").await.is_empty();
    let remaining = uci_value("zwrt_router.icgmwan.residual_flow").await;
    let today_used = uci_value("zwrt_router.icgmwan.count_flow_today").await;
    let config_path =
        std::env::var("LUYAZ_ICG_CONFIG").unwrap_or_else(|_| "/etc/config/icg.conf".into());
    let icg_config: BTreeMap<String, String> = blocking(move || fs::read_to_string(config_path))
        .await
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let (k, v) = line.split_once('=')?;
            Some((k.trim().into(), v.trim().into()))
        })
        .collect();
    let server_ip = if runtime_ip.is_empty() {
        icg_config
            .get("AggregationServerIP")
            .cloned()
            .unwrap_or_default()
    } else {
        runtime_ip
    };
    let tcp_port = if runtime_port == 0 {
        icg_config
            .get("AggregationServerTcpPort")
            .and_then(|v| v.parse().ok())
            .unwrap_or_default()
    } else {
        runtime_port
    };
    let udp_port = icg_config
        .get("AggregationServerUdpStartPort")
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or_default();
    let enabled = mode == "SMULTIWAN";
    fields.insert("aggregation".into(), json!({"enabled":enabled,"mode":mode,"state":if !enabled{"disabled"}else if !provisioned{"unprovisioned"}else if tcp_tunnel_count>0{"online"}else{"waiting"},"provisioned":provisioned,"online":tcp_tunnel_count>0,"controller":{"icg_process_running":icg_running,"mwan3_running":mwan_running},"tcp_tunnel_count":tcp_tunnel_count,"server":{"ip":server_ip,"tcp_port":tcp_port,"udp_start_port":udp_port,"source":if runtime_port>0{"runtime"}else{"config"}},"paths":paths,"path_count":path_count,"online_path_count":online_path_count,"traffic":{"remaining_bytes":remaining.parse::<u64>().ok(),"remaining_raw":remaining,"today_used_bytes":today_used.parse::<u64>().ok(),"today_used_raw":today_used}}));
    fields.insert(
        "multiwan".into(),
        topflow_multiwan(&uci_sets, &mode, mwan_running),
    );
}

/// 一次 ubus 调用。经过单一采集执行者（`executor.rs`，V2-18）：执行者里直接发，别处排队；
/// 后端由 `LUYAZ_UBUS=cli|socket` 选（`ubus::backend`），错误文字和原来的 `ubus call` 版一致。
pub async fn ubus(service: &str, method: &str, args: Value) -> Result<Value, String> {
    crate::executor::call(service, method, &args)
        .await
        .map_err(|e| e.to_string())
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
        assert_eq!(normalize_profile("MC7523 HW1.0"), "mc7523_hw1_0");
        assert_eq!(normalize_profile("MU5250"), "mu5250")
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
        let v = interface(&json!({}));
        assert_eq!(v["up"], false);
        assert_eq!(v["ipv4"], json!([]))
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
        let (items, wifi, lan) = connected_clients(
            &json!({"lan_access_list_info":[]}),
            &json!({"wireless_access_list_info":[{"mac_address":"AA:BB:CC:DD:EE:FF","ip_address":"192.168.0.2","hostname":"online"}]}),
            &lease_metadata(),
        );
        assert_eq!(wifi, 1);
        assert_eq!(lan, 0);
        assert_eq!(items.len(), 1);
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
