use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

/// Previous CPU sample for delta calculation.
pub struct CpuTracker {
    prev: Mutex<Vec<CpuSample>>,
}

struct CpuSample {
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

#[derive(Serialize)]
pub struct CpuUsage {
    /// Usage of online cores, in /proc/stat order (unchanged legacy field).
    pub cores: Vec<f64>,
    pub overall: f64,
    /// Every core known to sysfs, including offline ones.
    pub per_core: Vec<CoreInfo>,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct CoreInfo {
    pub id: usize,
    pub online: bool,
    /// None when offline or the node is missing.
    pub usage: Option<f64>,
    pub freq_mhz: Option<u32>,
    pub max_mhz: Option<u32>,
}

impl CpuTracker {
    pub fn new() -> Self {
        Self {
            prev: Mutex::new(Vec::new()),
        }
    }

    pub fn sample(&self) -> CpuUsage {
        let current = read_cpu_samples();
        let mut prev = self.prev.lock().unwrap();

        let mut cores = Vec::new();
        let mut usage_by_id = HashMap::new();
        let mut total_busy: u64 = 0;
        let mut total_all: u64 = 0;

        for (i, cur) in current.iter().enumerate() {
            if let Some(old) = prev.get(i) {
                let dt = cur.total().saturating_sub(old.total());
                let db = cur.busy().saturating_sub(old.busy());
                if dt > 0 {
                    let pct = (db as f64 / dt as f64) * 100.0;
                    let pct = (pct * 10.0).round() / 10.0;
                    cores.push(pct);
                    if old.id == cur.id {
                        usage_by_id.insert(cur.id, pct);
                    }
                    total_busy += db;
                    total_all += dt;
                } else {
                    cores.push(0.0);
                }
            } else {
                cores.push(0.0);
            }
        }

        let overall = if total_all > 0 {
            let pct = (total_busy as f64 / total_all as f64) * 100.0;
            (pct * 10.0).round() / 10.0
        } else {
            0.0
        };

        *prev = current;
        let per_core = read_core_info(Path::new(CPU_SYSFS), &usage_by_id);
        CpuUsage { cores, overall, per_core }
    }
}

const CPU_SYSFS: &str = "/sys/devices/system/cpu";

fn read_trim(path: &Path) -> Option<String> {
    let s = fs::read_to_string(path).ok()?;
    let s = s.trim();
    if s.is_empty() { None } else { Some(s.to_string()) }
}

fn read_num<T: std::str::FromStr>(path: &Path) -> Option<T> {
    read_trim(path)?.parse().ok()
}

/// Per-core online state and frequency from `root` (normally /sys/devices/system/cpu).
fn read_core_info(root: &Path, usage_by_id: &HashMap<usize, f64>) -> Vec<CoreInfo> {
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
            // cpu0 often has no `online` node: it cannot be taken offline.
            let online = read_num::<u8>(&dir.join("online")).map_or(true, |v| v != 0);
            let khz = |name: &str| {
                read_num::<u64>(&dir.join("cpufreq").join(name)).map(|k| (k / 1000) as u32)
            };
            CoreInfo {
                id,
                online,
                usage: if online { usage_by_id.get(&id).copied() } else { None },
                freq_mhz: if online { khz("scaling_cur_freq") } else { None },
                max_mhz: khz("cpuinfo_max_freq"),
            }
        })
        .collect()
}

fn read_cpu_samples() -> Vec<CpuSample> {
    let content = match fs::read_to_string("/proc/stat") {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut samples = Vec::new();
    for line in content.lines() {
        // Match "cpu0", "cpu1", etc. but not the aggregate "cpu " line
        if line.starts_with("cpu") && line.as_bytes().get(3).map_or(false, |b| b.is_ascii_digit())
        {
            let mut words = line.split_whitespace();
            let id = words
                .next()
                .and_then(|w| w[3..].parse().ok())
                .unwrap_or(usize::MAX);
            let parts: Vec<u64> = words
                .filter_map(|s| s.parse().ok())
                .collect();
            if parts.len() >= 7 {
                samples.push(CpuSample {
                    id,
                    user: parts[0],
                    nice: parts[1],
                    system: parts[2],
                    idle: parts[3],
                    iowait: parts[4],
                    irq: parts[5],
                    softirq: parts[6],
                });
            }
        }
    }
    samples
}

#[derive(Serialize)]
pub struct MemInfo {
    pub total_kb: u64,
    pub free_kb: u64,
    pub available_kb: u64,
    pub buffers_kb: u64,
    pub cached_kb: u64,
    pub used_kb: u64,
    pub usage_pct: f64,
}

pub fn read_meminfo() -> Option<MemInfo> {
    let content = fs::read_to_string("/proc/meminfo").ok()?;
    let mut map: HashMap<String, u64> = HashMap::new();
    for line in content.lines() {
        let mut parts = line.split(':');
        let key = parts.next()?.trim().to_string();
        let val_str = parts.next()?.trim();
        let val: u64 = val_str.split_whitespace().next()?.parse().ok()?;
        map.insert(key, val);
    }
    let total = *map.get("MemTotal")?;
    let free = *map.get("MemFree").unwrap_or(&0);
    let available = *map.get("MemAvailable").unwrap_or(&free);
    let buffers = *map.get("Buffers").unwrap_or(&0);
    let cached = *map.get("Cached").unwrap_or(&0);
    let used = total.saturating_sub(available);
    let usage_pct = if total > 0 {
        ((used as f64 / total as f64) * 1000.0).round() / 10.0
    } else {
        0.0
    };
    Some(MemInfo {
        total_kb: total,
        free_kb: free,
        available_kb: available,
        buffers_kb: buffers,
        cached_kb: cached,
        used_kb: used,
        usage_pct,
    })
}

#[derive(Serialize)]
pub struct DeviceInfo {
    pub hostname: String,
    pub uptime_secs: u64,
    pub load_avg: [f64; 3],
    pub kernel: String,
}

pub fn read_device_info() -> DeviceInfo {
    let hostname = fs::read_to_string("/proc/sys/kernel/hostname")
        .unwrap_or_default()
        .trim()
        .to_string();

    let uptime_str = fs::read_to_string("/proc/uptime").unwrap_or_default();
    let uptime_secs = uptime_str
        .split_whitespace()
        .next()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0) as u64;

    let loadavg_str = fs::read_to_string("/proc/loadavg").unwrap_or_default();
    let parts: Vec<f64> = loadavg_str
        .split_whitespace()
        .take(3)
        .filter_map(|s| s.parse().ok())
        .collect();
    let load_avg = [
        parts.first().copied().unwrap_or(0.0),
        parts.get(1).copied().unwrap_or(0.0),
        parts.get(2).copied().unwrap_or(0.0),
    ];

    let kernel = fs::read_to_string("/proc/version")
        .unwrap_or_default()
        .trim()
        .to_string();

    DeviceInfo {
        hostname,
        uptime_secs,
        load_avg,
        kernel,
    }
}

/// GET /api/battery. The first five fields are the original contract and keep
/// their old types; everything after them is optional (null = node missing).
#[derive(Serialize, Debug)]
pub struct BatteryInfo {
    pub status: String,
    pub capacity: i64,
    pub voltage_uv: i64,
    pub current_ua: i64,
    pub temperature: i64,
    /// Current full capacity estimated by the fuel gauge (µAh).
    pub charge_full_uah: Option<i64>,
    pub charge_full_design_uah: Option<i64>,
    /// Remaining charge (µAh).
    pub charge_counter_uah: Option<i64>,
    pub cycle_count: Option<i64>,
    pub health: Option<String>,
    pub voltage_max_uv: Option<i64>,
    /// None when the usb supply directory does not exist.
    pub charger: Option<ChargerInfo>,
}

#[derive(Serialize, Debug)]
pub struct ChargerInfo {
    pub online: Option<bool>,
    pub voltage_uv: Option<i64>,
    pub current_ua: Option<i64>,
    pub input_current_limit_ua: Option<i64>,
}

const POWER_SUPPLY_SYSFS: &str = "/sys/class/power_supply";

pub fn read_battery() -> Option<BatteryInfo> {
    read_battery_at(Path::new(POWER_SUPPLY_SYSFS))
}

/// Reads `<root>/battery` and `<root>/usb`. None only when the battery
/// directory is missing or has no status (caller answers 503).
pub fn read_battery_at(root: &Path) -> Option<BatteryInfo> {
    let bat = root.join("battery");
    let status = read_trim(&bat.join("status"))?;
    let num = |name: &str| read_num::<i64>(&bat.join(name));
    let usb = root.join("usb");
    let charger = usb.is_dir().then(|| ChargerInfo {
        online: read_num::<u8>(&usb.join("online")).map(|v| v != 0),
        voltage_uv: read_num(&usb.join("voltage_now")),
        current_ua: read_num(&usb.join("current_now")),
        input_current_limit_ua: read_num(&usb.join("input_current_limit")),
    });
    Some(BatteryInfo {
        status,
        capacity: num("capacity").unwrap_or(0),
        voltage_uv: num("voltage_now").unwrap_or(0),
        current_ua: num("current_now").unwrap_or(0),
        temperature: num("temp").unwrap_or(0),
        charge_full_uah: num("charge_full"),
        charge_full_design_uah: num("charge_full_design"),
        charge_counter_uah: num("charge_counter"),
        cycle_count: num("cycle_count"),
        health: read_trim(&bat.join("health")),
        voltage_max_uv: num("voltage_max"),
        charger,
    })
}

#[derive(Serialize)]
pub struct NetInterface {
    pub name: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_packets: u64,
    pub tx_packets: u64,
}

pub fn read_network_traffic() -> Vec<NetInterface> {
    let content = match fs::read_to_string("/proc/net/dev") {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut ifaces = Vec::new();
    for line in content.lines().skip(2) {
        let line = line.trim();
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let vals: Vec<u64> = rest
            .split_whitespace()
            .filter_map(|s| s.parse().ok())
            .collect();
        if vals.len() >= 10 {
            ifaces.push(NetInterface {
                name: name.trim().to_string(),
                rx_bytes: vals[0],
                rx_packets: vals[1],
                tx_bytes: vals[8],
                tx_packets: vals[9],
            });
        }
    }
    ifaces
}

// -- Speed tracker (ring buffer + background sampler for IPA batch smoothing) --

const SPEED_RING_SIZE: usize = 16; // 16 samples × 1s = 16s window

struct NetSample {
    rx_bytes: u64,
    tx_bytes: u64,
    time: Instant,
}

#[derive(Serialize, Clone)]
pub struct SpeedSnapshot {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_speed: f64,
    pub tx_speed: f64,
    pub elapsed_ms: u64,
}

pub struct SpeedTracker {
    latest: Arc<Mutex<SpeedSnapshot>>,
}

impl SpeedTracker {
    pub fn new() -> Self {
        let ring = Arc::new(Mutex::new(VecDeque::with_capacity(SPEED_RING_SIZE)));
        let latest = Arc::new(Mutex::new(SpeedSnapshot {
            rx_bytes: 0,
            tx_bytes: 0,
            rx_speed: 0.0,
            tx_speed: 0.0,
            elapsed_ms: 0,
        }));

        // Seed with initial sample
        let (rx, tx) = read_rmnet_bytes();
        ring.lock().unwrap().push_back(NetSample {
            rx_bytes: rx,
            tx_bytes: tx,
            time: Instant::now(),
        });

        // Background sampler thread
        let ring_c = Arc::clone(&ring);
        let latest_c = Arc::clone(&latest);
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(1));
            let (rx, tx) = read_rmnet_bytes();
            let now = Instant::now();
            let mut buf = ring_c.lock().unwrap();
            if buf.len() >= SPEED_RING_SIZE {
                buf.pop_front();
            }
            buf.push_back(NetSample {
                rx_bytes: rx,
                tx_bytes: tx,
                time: now,
            });

            // Compute rolling speed from oldest to newest
            if buf.len() >= 2 {
                let oldest = &buf[0];
                let newest = buf.back().unwrap();
                let secs = newest.time.duration_since(oldest.time).as_secs_f64();
                if secs > 0.1 {
                    *latest_c.lock().unwrap() = SpeedSnapshot {
                        rx_bytes: newest.rx_bytes,
                        tx_bytes: newest.tx_bytes,
                        rx_speed: newest.rx_bytes.saturating_sub(oldest.rx_bytes) as f64 / secs,
                        tx_speed: newest.tx_bytes.saturating_sub(oldest.tx_bytes) as f64 / secs,
                        elapsed_ms: (secs * 1000.0) as u64,
                    };
                }
            }
        });

        Self { latest }
    }

    pub fn sample(&self) -> SpeedSnapshot {
        self.latest.lock().unwrap().clone()
    }
}

fn read_rmnet_bytes() -> (u64, u64) {
    let mut rx_total: u64 = 0;
    let mut tx_total: u64 = 0;
    for iface in &["rmnet_data0", "rmnet_ipa0"] {
        let base = format!("/sys/class/net/{iface}/statistics");
        if let (Some(rx), Some(tx)) = (
            read_sysfs_u64(&format!("{base}/rx_bytes")),
            read_sysfs_u64(&format!("{base}/tx_bytes")),
        ) {
            rx_total += rx;
            tx_total += tx;
        }
    }
    if rx_total > 0 || tx_total > 0 {
        return (rx_total, tx_total);
    }
    // Fallback: parse both from /proc/net/dev
    for iface in read_network_traffic() {
        if iface.name == "rmnet_data0" || iface.name == "rmnet_ipa0" {
            rx_total += iface.rx_bytes;
            tx_total += iface.tx_bytes;
        }
    }
    (rx_total, tx_total)
}

fn read_sysfs_u64(path: &str) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

// -- Process monitor --

/// Daemons listed in /etc/config/zte_topsw_daemon.conf. zte_topsw_daemon waits
/// for every one of them to register before it releases boot, and several are
/// live services (zte_topsw_wms is SMS, zte_topsw_mc the modem controller).
/// Never kill or stop these — see CLAUDE.md "ZTE Daemon Sync Barrier".
const SYNC_BARRIER_DAEMONS: &[&str] = &[
    "zte_topsw_mc",
    "zte_router",
    "zte_topsw_data",
    "zte_topsw_nwinfo",
    "zte_topsw_mdm",
    "zte_topsw_sleep_faw",
    "zte_topsw_apn",
    "zte_topsw_wms",
    "zte_topsw_key",
    "zte_topsw_led",
    "zte_topsw_tr098db",
    "zte_dm",
    "zte_topsw_fota_result",
    "zte_topsw_devui",
    "zte_topsw_wlan",
    "zte_smart_manage",
];

/// Daemons that are safe to kill: none of them is in daemon.conf.
const BLOAT_DAEMONS: &[&str] = &[
    "zte_topsw_tr069",
    "zte_topsw_tr069_sub",
    "zte_mqtt_sdk_st",
    "zte_topsw_diag",
    "zte_topsw_samba",
    "zte_topsw_nfc",
    "zte_topsw_get_brand",
    "zte_topsw_jwxk_query",
    "zte-topsw-tunnel",
    "zte_dua",
];

#[derive(Serialize)]
pub struct ProcessEntry {
    pub pid: u32,
    pub name: String,
    pub cpu_pct: f64,
    pub rss_kb: u64,
    pub state: String,
    pub is_bloat: bool,
}

#[derive(Serialize)]
pub struct ProcessListResult {
    pub processes: Vec<ProcessEntry>,
    pub total_count: usize,
    pub bloat_count: usize,
    pub bloat_cpu_pct: f64,
    pub bloat_rss_kb: u64,
}

#[derive(Serialize)]
pub struct KilledProcess {
    pub pid: u32,
    pub name: String,
}

#[derive(Serialize)]
pub struct KillBloatResult {
    pub killed: Vec<KilledProcess>,
    pub skipped: Vec<KilledProcess>,
    pub freed_rss_kb: u64,
}

pub struct ProcessTracker {
    prev: Mutex<(HashMap<u32, u64>, u64)>, // (per-pid ticks, total CPU ticks)
}

impl ProcessTracker {
    pub fn new() -> Self {
        Self {
            prev: Mutex::new((HashMap::new(), 0)),
        }
    }

    pub fn sample(&self) -> ProcessListResult {
        let total_ticks = read_total_cpu_ticks();
        let mut prev = self.prev.lock().unwrap();
        let (prev_per_pid, prev_total) = &*prev;
        let dt = total_ticks.saturating_sub(*prev_total);

        let mut entries: Vec<ProcessEntry> = Vec::new();
        let mut new_per_pid: HashMap<u32, u64> = HashMap::new();

        let proc_dir = match fs::read_dir("/proc") {
            Ok(d) => d,
            Err(_) => {
                return ProcessListResult {
                    processes: Vec::new(),
                    total_count: 0,
                    bloat_count: 0,
                    bloat_cpu_pct: 0.0,
                    bloat_rss_kb: 0,
                };
            }
        };

        for entry in proc_dir.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            let pid: u32 = match name_str.parse() {
                Ok(p) => p,
                Err(_) => continue,
            };

            let stat_path = format!("/proc/{pid}/stat");
            let stat_str = match fs::read_to_string(&stat_path) {
                Ok(s) => s,
                Err(_) => continue,
            };

            // Parse /proc/PID/stat — comm is in parens, fields after closing paren
            let comm_start = match stat_str.find('(') {
                Some(i) => i + 1,
                None => continue,
            };
            let comm_end = match stat_str.rfind(')') {
                Some(i) => i,
                None => continue,
            };
            let comm = stat_str[comm_start..comm_end].to_string();
            let after_comm = &stat_str[comm_end + 2..]; // skip ") "
            let fields: Vec<&str> = after_comm.split_whitespace().collect();
            // fields[0]=state, fields[11]=utime, fields[12]=stime, fields[21]=rss(pages)
            if fields.len() < 22 {
                continue;
            }
            let state_char = fields[0].to_string();
            let utime: u64 = fields[11].parse().unwrap_or(0);
            let stime: u64 = fields[12].parse().unwrap_or(0);
            let proc_ticks = utime + stime;
            let rss_pages: u64 = fields[21].parse().unwrap_or(0);
            let rss_kb = rss_pages * 4; // page size = 4K

            // CPU% delta
            let cpu_pct = if dt > 0 {
                let prev_ticks = prev_per_pid.get(&pid).copied().unwrap_or(proc_ticks);
                let dp = proc_ticks.saturating_sub(prev_ticks);
                let pct = (dp as f64 / dt as f64) * 100.0;
                (pct * 10.0).round() / 10.0
            } else {
                0.0
            };

            new_per_pid.insert(pid, proc_ticks);

            // Read cmdline for better name matching
            let cmdline_name = fs::read_to_string(format!("/proc/{pid}/cmdline"))
                .ok()
                .and_then(|s| {
                    let clean = s.replace('\0', " ");
                    let first = clean.split_whitespace().next()?.to_string();
                    let basename = first.rsplit('/').next().unwrap_or(&first).to_string();
                    if basename.is_empty() { None } else { Some(basename) }
                });

            let display_name = cmdline_name.as_deref().unwrap_or(&comm);
            let is_bloat = BLOAT_DAEMONS.iter().any(|&b| display_name == b);

            let state_desc = match state_char.as_str() {
                "R" => "running",
                "S" => "sleeping",
                "D" => "disk",
                "Z" => "zombie",
                "T" => "stopped",
                _ => "other",
            };

            entries.push(ProcessEntry {
                pid,
                name: display_name.to_string(),
                cpu_pct,
                rss_kb,
                state: state_desc.to_string(),
                is_bloat,
            });
        }

        // Update stored state
        *prev = (new_per_pid, total_ticks);

        // Sort by CPU% desc
        entries.sort_by(|a, b| b.cpu_pct.partial_cmp(&a.cpu_pct).unwrap_or(std::cmp::Ordering::Equal));

        let total_count = entries.len();
        let bloat_count = entries.iter().filter(|e| e.is_bloat).count();
        let bloat_cpu_pct = entries.iter().filter(|e| e.is_bloat).map(|e| e.cpu_pct).sum::<f64>();
        let bloat_cpu_pct = (bloat_cpu_pct * 10.0).round() / 10.0;
        let bloat_rss_kb: u64 = entries.iter().filter(|e| e.is_bloat).map(|e| e.rss_kb).sum();

        // Keep top 50
        entries.truncate(50);

        ProcessListResult {
            processes: entries,
            total_count,
            bloat_count,
            bloat_cpu_pct,
            bloat_rss_kb,
        }
    }
}

fn read_total_cpu_ticks() -> u64 {
    let content = match fs::read_to_string("/proc/stat") {
        Ok(c) => c,
        Err(_) => return 0,
    };
    for line in content.lines() {
        if line.starts_with("cpu ") {
            return line
                .split_whitespace()
                .skip(1)
                .filter_map(|s| s.parse::<u64>().ok())
                .sum();
        }
    }
    0
}

/// Kill bloat daemons. If `pids` is None, kill all running bloat.
pub fn kill_bloat(pids: Option<&[u32]>) -> KillBloatResult {
    let mut killed = Vec::new();
    let mut skipped = Vec::new();
    let mut freed_rss_kb: u64 = 0;

    let targets: Vec<(u32, String, u64)> = match pids {
        Some(pid_list) => {
            pid_list.iter().filter_map(|&pid| {
                let (name, rss) = read_proc_name_rss(pid)?;
                Some((pid, name, rss))
            }).collect()
        }
        None => {
            // Find all bloat processes
            let proc_dir = match fs::read_dir("/proc") {
                Ok(d) => d,
                Err(_) => return KillBloatResult { killed, skipped, freed_rss_kb },
            };
            proc_dir.flatten().filter_map(|entry| {
                let pid: u32 = entry.file_name().to_string_lossy().parse().ok()?;
                let (name, rss) = read_proc_name_rss(pid)?;
                if BLOAT_DAEMONS.iter().any(|&b| b == name) {
                    Some((pid, name, rss))
                } else {
                    None
                }
            }).collect()
        }
    };

    for (pid, name, rss_kb) in targets {
        if !BLOAT_DAEMONS.iter().any(|&b| b == name) {
            skipped.push(KilledProcess { pid, name });
            continue;
        }
        let ret = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        if ret == 0 {
            freed_rss_kb += rss_kb;
            killed.push(KilledProcess { pid, name });
        } else {
            skipped.push(KilledProcess { pid, name });
        }
    }

    KillBloatResult { killed, skipped, freed_rss_kb }
}

fn read_proc_name_rss(pid: u32) -> Option<(String, u64)> {
    let stat_str = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let comm_end = stat_str.rfind(')')?;
    let after_comm = stat_str.get(comm_end + 2..)?;
    let fields: Vec<&str> = after_comm.split_whitespace().collect();
    let rss_pages: u64 = fields.get(21)?.parse().ok()?;

    let name = fs::read_to_string(format!("/proc/{pid}/cmdline"))
        .ok()
        .and_then(|s| {
            let clean = s.replace('\0', " ");
            let first = clean.split_whitespace().next()?.to_string();
            let basename = first.rsplit('/').next().unwrap_or(&first).to_string();
            if basename.is_empty() { None } else { Some(basename) }
        })
        .unwrap_or_else(|| {
            let comm_start = stat_str.find('(').unwrap_or(0) + 1;
            stat_str[comm_start..comm_end].to_string()
        });

    Some((name, rss_pages * 4))
}

#[cfg(test)]
mod bloat_tests {
    use super::{BLOAT_DAEMONS, SYNC_BARRIER_DAEMONS};

    #[test]
    fn bloat_list_never_contains_a_sync_barrier_daemon() {
        for d in BLOAT_DAEMONS {
            assert!(
                !SYNC_BARRIER_DAEMONS.contains(d),
                "{d} is in daemon.conf and must not be killable"
            );
        }
    }
}

#[cfg(test)]
mod sysfs_tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("zte-agent-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn put(dir: &Path, name: &str, val: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(name), format!("{val}\n")).unwrap();
    }

    #[test]
    fn battery_all_nodes_present() {
        let root = tmp("bat-full");
        let b = root.join("battery");
        for (k, v) in [
            ("status", "Discharging"), ("capacity", "93"), ("voltage_now", "4341611"),
            ("current_now", "-392221"), ("temp", "350"), ("charge_full", "11011000"),
            ("charge_full_design", "10214000"), ("charge_counter", "10280970"),
            ("cycle_count", "66"), ("health", "Good"), ("voltage_max", "4500000"),
        ] {
            put(&b, k, v);
        }
        let u = root.join("usb");
        put(&u, "online", "0");
        put(&u, "voltage_now", "31000");
        put(&u, "current_now", "0");
        let info = read_battery_at(&root).unwrap();
        assert_eq!(info.capacity, 93);
        assert_eq!(info.current_ua, -392221);
        assert_eq!(info.charge_full_design_uah, Some(10214000));
        assert_eq!(info.cycle_count, Some(66));
        assert_eq!(info.health.as_deref(), Some("Good"));
        let c = info.charger.unwrap();
        assert_eq!(c.online, Some(false));
        assert_eq!(c.input_current_limit_ua, None);
    }

    #[test]
    fn battery_missing_or_garbage_nodes_are_null_not_zero() {
        let root = tmp("bat-sparse");
        let b = root.join("battery");
        put(&b, "status", "Charging");
        put(&b, "cycle_count", "n/a");
        let info = read_battery_at(&root).unwrap();
        assert_eq!(info.cycle_count, None);
        assert_eq!(info.charge_full_uah, None);
        assert!(info.charger.is_none());
        let v = serde_json::to_value(&info).unwrap();
        assert!(v["charge_full_design_uah"].is_null());
        assert_eq!(v["capacity"], 0, "legacy field keeps its old type");
    }

    #[test]
    fn battery_dir_missing_is_none() {
        let root = tmp("bat-none");
        assert!(read_battery_at(&root).is_none());
    }

    #[test]
    fn cores_include_offline_and_map_usage_by_id() {
        let root = tmp("cpu");
        put(&root.join("cpu0/cpufreq"), "scaling_cur_freq", "1516800");
        put(&root.join("cpu0/cpufreq"), "cpuinfo_max_freq", "2208000");
        put(&root.join("cpu1"), "online", "0");
        put(&root.join("cpu1/cpufreq"), "cpuinfo_max_freq", "2208000");
        fs::create_dir_all(root.join("cpufreq")).unwrap();
        fs::create_dir_all(root.join("cpuidle")).unwrap();
        let usage = HashMap::from([(0usize, 12.5)]);
        let cores = read_core_info(&root, &usage);
        assert_eq!(cores.len(), 2);
        assert_eq!(
            cores[0],
            CoreInfo { id: 0, online: true, usage: Some(12.5), freq_mhz: Some(1516), max_mhz: Some(2208) }
        );
        assert_eq!(
            cores[1],
            CoreInfo { id: 1, online: false, usage: None, freq_mhz: None, max_mhz: Some(2208) }
        );
    }
}
