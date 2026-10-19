//! `hour_power` and `hour_proc` (touch-ui docs/LEDGER.md §4, §6; u60-sentinel.md §19 T9c), as
//! the guard's ledger task sums them each round, here each check (60 s):
//!
//! - Energy: the fuel gauge's charge counter (µAh) difference × voltage, right across a sleep;
//!   without a counter V×I over the awake part only (`e_partial` when the hour had a sleep);
//!   charging time goes to `chg_s`, never into a cell. Cells: screen on/off × home/abroad.
//! - `zone_tmax` from the application side's thermal zones only (a modem-side zone read is a QMI
//!   request), `throttle_s` when a cpufreq policy is capped below the highest cap it had this boot.
//! - The exits (S8): Tailscale meant on while the cellular link has the default route and
//!   rc.local starts it, checked over its LocalAPI socket (HTTP by hand, no `tailscale` fork);
//!   CHILL on unless switched off or paused, healthy when its supervisor said "running" within
//!   150 s and the mihomo it named is that process.
//! - `hour_proc`: RSS of our programs and this process's CPU (per mille, only when the same
//!   process ran the whole hour), from the hour's last check.
//!
//! - Radio (u60-radio.md §4, D9/D10): screen off, not charging, per home/abroad cell — the share
//!   of time in zero-packet runs ≥ 10 s (`cell_window`'s quiet fold, taken each check) and the
//!   cellular packets; seconds screen off at home with clients (unknown counts as clients); the
//!   hour's CHILL profile (`off` when CHILL is off or paused, `mixed` when it changed).
//!
//! An interval that crosses the hour goes to the hour it ends in (the guard's to the one it
//! starts in): totals are the same, the split moves by at most one check.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const BAT: &str = "/sys/class/power_supply/battery";
const BACKLIGHT: &str = "/sys/class/leds/led:lcd/brightness";
const THERMAL: &str = "/sys/class/thermal";
const CPUFREQ: &str = "/sys/devices/system/cpu/cpufreq";
const RC_LOCAL: &str = "/etc/rc.local";
const TS_START: &str = "/data/tailscale/start.sh";
const TS_SOCK: &str = "/tmp/tailscaled.sock";
const CHILL_DIR: &str = "/data/chill";
const CHILL_STATE: &str = "/tmp/chill.state";
const THERMAL_ECO: &str = "/tmp/chill.thermal-eco";
/// The core's boot-bound BOOTTIME screen-off pause lease (u60-radio.md 6e).
pub const CHILL_IDLE: &str = "/tmp/chill.idle.v2";
const DUMP_DIR: &str = "/data/vendor/ramdump";
/// Programs whose RSS `hour_proc` keeps, by the card's names.
pub const RSS: [(&str, &str); 5] = [
    ("agent", "zte-agent"),
    ("core", crate::paths::CORE),
    ("ui", crate::paths::UI),
    ("mihomo", "mihomo"),
    ("ts", "tailscaled"),
];
const CELLS: [&str; 4] = ["on_home", "on_abroad", "off_home", "off_abroad"];

/// One check's readings (None = not readable).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Read {
    pub v: Option<i64>,
    pub i: Option<i64>,
    pub q: Option<i64>,
    pub cap: Option<i64>,
    pub bt: Option<i64>,
    pub charging: bool,
    /// Evidence from the existing status/current reads; None = absent or conflicting.
    pub charging_known: Option<bool>,
    pub screen_on: bool,
    pub screen_known: Option<bool>,
    pub zt: Option<i64>,
    /// Some(true) = a policy is capped below its boot maximum; None = no cpufreq.
    pub thr: Option<bool>,
    pub ts_on: bool,
    /// None = not checked (or not meant on).
    pub ts_ok: Option<bool>,
    pub chill_on: bool,
    pub chill_ok: bool,
    /// This process: (pid, start time, user+system ticks).
    pub cpu: Option<(u32, u64, u64)>,
    /// Minimal CPU/task counters from the same existing minute check.
    pub activity: Option<super::activity::Read>,
    /// The card's program name → RSS kB.
    pub rss: BTreeMap<String, i64>,
    pub free_mb: Option<i64>,
    pub dump_mb: Option<i64>,
    /// The quiet fold since the previous check (set by [`super::Fold::power`]); None = not taken.
    pub wan: Option<crate::cell_window::QuietTake>,
    /// Clients connected (the `clients` block's total); None = not known.
    pub clients: Option<i64>,
    /// CHILL's effective profile; None = no CHILL here.
    pub profile: Option<String>,
}

/// The hour's sums (µWh for energy).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Acc {
    pub(super) energy: super::energy::Acc,
    #[serde(default)]
    pub(super) activity: super::activity::Acc,
    e: f64,
    n_e: u32,
    n_vi: u32,
    e_partial: bool,
    cell_s: [i64; 4],
    cell_e: [f64; 4],
    chg_s: i64,
    zone_tmax: Option<i64>,
    n_thr: u32,
    throttle_s: i64,
    chill_on_s: i64,
    chill_ok_s: i64,
    ts_on_s: i64,
    ts_chk_s: i64,
    ts_ok_s: i64,
    /// This process at the hour's first check and its last: (pid, start, ticks, boot s).
    cpu_first: Option<(u32, u64, u64, i64)>,
    cpu_last: Option<(u32, u64, u64, i64)>,
    rss: BTreeMap<String, i64>,
    free_mb: Option<i64>,
    dump_mb: Option<i64>,
    batt_tmax: Option<i64>,
    cap_from: Option<i64>,
    cap_to: Option<i64>,
    /// Screen off, not charging, [home, abroad]: quiet ms, compared ms, packets.
    off_quiet_ms: [u64; 2],
    off_obs_ms: [u64; 2],
    off_pkts: [u64; 2],
    off_home_cli_s: i64,
    profile: Option<String>,
    profile_mixed: bool,
}

impl Acc {
    /// One interval ending at this reading: `dt` boot seconds, `awake` of them awake, the
    /// previous reading's counter `q0`.
    pub fn add(&mut self, r: &Read, q0: Option<i64>, dt: i64, awake: i64, abroad: bool, up: i64) {
        let dt = dt.max(0);
        let awake = awake.clamp(0, dt);
        let sl = dt - awake;
        let (mut e, mut es) = (None, 0);
        if let (Some(q), Some(q0), Some(v)) = (r.q, q0, r.v) {
            // Keep the historical fields, but never overflow on a corrupt/reset counter.
            e = q0.checked_sub(q).map(|dq| dq as f64 * v as f64 / 1e6);
            es = dt;
        } else if let (Some(v), Some(i)) = (r.v, r.i)
            && i < 0
        {
            e = Some(v as f64 * -(i as f64) * awake as f64 / 3.6e9);
            es = awake;
            self.n_vi += 1;
            if sl > 0 {
                self.e_partial = true;
            }
        } else if dt > 0 {
            self.e_partial = true;
        }
        if r.charging {
            self.chg_s += dt;
        } else {
            let c = usize::from(!r.screen_on) * 2 + usize::from(abroad);
            let secs = if e.is_none() { awake } else { es };
            self.cell_s[c] += secs;
            if !r.screen_on {
                let a = usize::from(abroad);
                if let Some(w) = r.wan {
                    self.off_quiet_ms[a] += w.quiet_ms;
                    self.off_obs_ms[a] += w.obs_ms;
                    self.off_pkts[a] += w.pkts;
                }
                if !abroad && r.clients != Some(0) {
                    self.off_home_cli_s += secs;
                }
            }
            if let Some(e) = e
                && e >= 0.0
            {
                self.cell_e[c] += e;
                self.e += e;
                self.n_e += 1;
            }
        }
        if let Some(t) = r.bt {
            self.batt_tmax = Some(self.batt_tmax.map_or(t, |m| m.max(t)));
        }
        if let Some(t) = r.zt {
            self.zone_tmax = Some(self.zone_tmax.map_or(t, |m| m.max(t)));
        }
        if let Some(t) = r.thr {
            self.n_thr += 1;
            if t {
                self.throttle_s += awake;
            }
        }
        if r.chill_on {
            self.chill_on_s += awake;
            if r.chill_ok {
                self.chill_ok_s += awake;
            }
        }
        if r.ts_on {
            self.ts_on_s += awake;
            if let Some(ok) = r.ts_ok {
                self.ts_chk_s += awake;
                if ok {
                    self.ts_ok_s += awake;
                }
            }
        }
        self.note(r, up);
    }

    /// What is read at a moment rather than summed: the last check's.
    pub fn note(&mut self, r: &Read, up: i64) {
        if let Some((p, s, t)) = r.cpu {
            if self.cpu_first.is_none() {
                self.cpu_first = Some((p, s, t, up));
            }
            self.cpu_last = Some((p, s, t, up));
        }
        if self.cap_from.is_none() {
            self.cap_from = r.cap;
        }
        if let Some(p) = &r.profile {
            match &self.profile {
                None => self.profile = Some(p.clone()),
                Some(q) if q != p => self.profile_mixed = true,
                _ => {}
            }
        }
        if r.cap.is_some() {
            self.cap_to = r.cap;
        }
        self.rss = r.rss.clone();
        self.free_mb = r.free_mb;
        self.dump_mb = r.dump_mb;
    }

    fn mwh(x: Option<f64>) -> Value {
        x.map_or(Value::Null, |x| {
            // printf "%.1f" of µWh / 1000
            let s = format!("{:.1}", x / 1000.0);
            s.parse::<f64>().map_or(Value::Null, Value::from)
        })
    }

    /// The `hour_power` fields (besides the head).
    pub fn power(&self) -> Value {
        let mut o = json!({
            "counter_quality": self.energy.value(),
            "src": if self.n_vi > 0 { json!("vi") } else if self.n_e > 0 { json!("counter") } else { Value::Null },
            "e": Self::mwh((self.n_e > 0).then_some(self.e)),
        });
        let m = o.as_object_mut().expect("object");
        for (j, c) in CELLS.iter().enumerate() {
            m.insert(format!("{c}_s"), json!(self.cell_s[j]));
            m.insert(
                format!("{c}_e"),
                Self::mwh((self.n_e > 0).then_some(self.cell_e[j])),
            );
        }
        m.insert("chg_s".into(), json!(self.chg_s));
        m.insert("batt_from".into(), json!(self.cap_from));
        m.insert("batt_to".into(), json!(self.cap_to));
        m.insert("batt_tmax".into(), json!(self.batt_tmax));
        m.insert("zone_tmax".into(), json!(self.zone_tmax));
        m.insert(
            "throttle_s".into(),
            if self.n_thr > 0 {
                json!(self.throttle_s)
            } else {
                Value::Null
            },
        );
        m.insert("e_partial".into(), json!(u8::from(self.e_partial)));
        for (a, c) in ["off_home", "off_abroad"].iter().enumerate() {
            let obs = self.off_obs_ms[a];
            m.insert(
                format!("{c}_quiet_pm"),
                json!((self.off_quiet_ms[a] * 1000).checked_div(obs)),
            );
            m.insert(
                format!("{c}_wan_pkts"),
                if obs > 0 {
                    json!(self.off_pkts[a])
                } else {
                    Value::Null
                },
            );
        }
        m.insert("off_home_cli_s".into(), json!(self.off_home_cli_s));
        m.insert(
            "chill_profile".into(),
            if self.profile_mixed {
                json!("mixed")
            } else {
                json!(self.profile)
            },
        );
        o
    }

    /// The `hour_proc` fields.
    pub fn proc_(&self) -> Value {
        let cpu = match (self.cpu_first, self.cpu_last) {
            (Some((p0, s0, t0, u0)), Some((p1, s1, t1, u1)))
                if p0 == p1 && s0 == s1 && t1 >= t0 && u1 > u0 =>
            {
                // ticks at 100 Hz → per mille
                json!(((t1 - t0) * 10) as i64 / (u1 - u0))
            }
            _ => Value::Null,
        };
        let mut o = json!({"cpu_core": cpu, "idle_rows": null, "idle_wan": null,
                          "activity": self.activity.value()});
        let m = o.as_object_mut().expect("object");
        for (k, _) in RSS {
            m.insert(format!("rss_{k}"), json!(self.rss.get(k)));
        }
        m.insert("chill_on_s".into(), json!(self.chill_on_s));
        m.insert("chill_ok_s".into(), json!(self.chill_ok_s));
        m.insert("ts_on_s".into(), json!(self.ts_on_s));
        m.insert("ts_chk_s".into(), json!(self.ts_chk_s));
        m.insert("ts_ok_s".into(), json!(self.ts_ok_s));
        o
    }

    pub fn free_dump(&self) -> (Option<i64>, Option<i64>) {
        (self.free_mb, self.dump_mb)
    }
}

// ── reading ──

/// Where the readings are (the device's, or a test root).
pub struct Paths {
    pub bat: PathBuf,
    pub backlight: PathBuf,
    pub thermal: PathBuf,
    pub cpufreq: PathBuf,
    pub route: PathBuf,
    pub rc_local: PathBuf,
    pub ts_sock: PathBuf,
    pub chill_dir: PathBuf,
    pub chill_state: PathBuf,
    pub thermal_eco: PathBuf,
    pub chill_idle: PathBuf,
    pub proc_: PathBuf,
    pub data: PathBuf,
    pub dumps: PathBuf,
    pub self_pid: u32,
}

impl Paths {
    pub fn device() -> Paths {
        let h = |p: &str| PathBuf::from(crate::state::host_path(p));
        Paths {
            bat: h(BAT),
            backlight: h(BACKLIGHT),
            thermal: h(THERMAL),
            cpufreq: h(CPUFREQ),
            route: h("/proc/net/route"),
            rc_local: h(RC_LOCAL),
            ts_sock: h(TS_SOCK),
            chill_dir: h(CHILL_DIR),
            chill_state: h(CHILL_STATE),
            thermal_eco: h(THERMAL_ECO),
            chill_idle: h(CHILL_IDLE),
            proc_: h("/proc"),
            data: h("/data"),
            dumps: h(DUMP_DIR),
            self_pid: std::process::id(),
        }
    }
}

/// What reading keeps between checks of a boot (saved with the fold).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ReadState {
    /// The application-side thermal zone directories, found once a boot.
    zones: Option<Vec<String>>,
    /// cpufreq policy → the highest `scaling_max_freq` seen this boot.
    cpumax: BTreeMap<String, i64>,
}

fn rd(p: &Path) -> Option<String> {
    std::fs::read_to_string(p)
        .ok()
        .and_then(|s| s.lines().next().map(|l| l.trim().to_string()))
}

fn num(p: &Path) -> Option<i64> {
    rd(p).and_then(|s| s.parse().ok())
}

/// An application-side zone type (the guard's `ledger_zones`).
pub fn app_zone(t: &str) -> bool {
    t.starts_with("cpuss-")
        || t.starts_with("aoss-")
        || t.starts_with("sys-therm-")
        || t == "xo-therm"
        || (t.starts_with("pm") && t.ends_with("_tz"))
        || t == "battery"
}

/// `BackendState` Running and `Self.Online` in a LocalAPI status answer.
pub fn ts_healthy(v: &Value) -> Option<bool> {
    let st = v["BackendState"].as_str()?;
    Some(st == "Running" && v["Self"]["Online"].as_bool() == Some(true))
}

/// GET `path` from tailscaled's LocalAPI socket, 2 s: `Err` when it can't be reached (no socket,
/// refused, timed out or dropped), `Ok(None)` for an answer that is not HTTP, else the status
/// code and the body.
pub(crate) fn ts_get(sock: &Path, path: &str) -> Result<Option<(u16, String)>, ()> {
    use std::io::{Read as _, Write as _};
    if !sock.exists() {
        return Err(());
    }
    let mut s = std::os::unix::net::UnixStream::connect(sock).map_err(|_| ())?;
    let t = Some(std::time::Duration::from_secs(2));
    let _ = s.set_read_timeout(t);
    let _ = s.set_write_timeout(t);
    let req = format!("GET {path} HTTP/1.0\r\nHost: local-tailscaled.sock\r\n\r\n");
    s.write_all(req.as_bytes()).map_err(|_| ())?;
    let mut buf = Vec::new();
    if s.take(4 << 20).read_to_end(&mut buf).is_err() && buf.is_empty() {
        return Err(()); // timed out or dropped
    }
    let text = String::from_utf8_lossy(&buf);
    let Some((head, body)) = text.split_once("\r\n\r\n") else {
        return Ok(None);
    };
    Ok(head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .map(|c| (c, body.to_string())))
}

/// GET /localapi/v0/status over tailscaled's socket, 2 s: Some(ok), or None when it answered
/// something that says nothing (the guard: not checked).
fn ts_check(sock: &Path) -> Option<bool> {
    let Ok(answer) = ts_get(sock, "/localapi/v0/status") else {
        return Some(false);
    };
    let (code, body) = answer?;
    match code {
        200 => ts_healthy(&serde_json::from_str(&body).ok()?),
        500..=599 => Some(false),
        _ => None,
    }
}

fn mtime_age(p: &Path, wall: i64) -> Option<i64> {
    let m = std::fs::metadata(p).ok()?.modified().ok()?;
    let s = m.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    Some(wall - s)
}

/// chill.sh's `profile_effective`: eco while too hot, else the stored profile (standard when
/// missing or not one of the three).
fn chill_profile(dir: &Path, thermal_eco: &Path) -> String {
    if thermal_eco.exists() {
        return "eco".into();
    }
    match rd(&dir.join("profile")).as_deref() {
        Some(p @ ("eco" | "standard" | "perf")) => p.into(),
        _ => "standard".into(),
    }
}

fn rss_kb(proc_: &Path, pid: u32) -> Option<i64> {
    let s = std::fs::read_to_string(proc_.join(pid.to_string()).join("status")).ok()?;
    s.lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn du_mb(dir: &Path) -> Option<i64> {
    use std::os::unix::fs::MetadataExt;
    fn walk(d: &Path, n: &mut u64) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let Ok(m) = e.metadata() else { continue };
            *n += m.blocks() * 512;
            if m.is_dir() {
                walk(&e.path(), n);
            }
        }
    }
    std::fs::metadata(dir).ok()?;
    let mut n = 0;
    walk(dir, &mut n);
    Some((n / 1024 / 1024) as i64)
}

fn free_mb(dir: &Path) -> Option<i64> {
    let c = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()).ok()?;
    // SAFETY: statvfs into a struct we own.
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: a valid C string and our struct.
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    Some((s.f_bavail as u64 * s.f_frsize as u64 / 1024 / 1024) as i64)
}

impl ReadState {
    /// One check's readings. `pids`: our programs as the minute look found them; `wall` for
    /// file ages.
    pub fn read(&mut self, p: &Paths, pids: &BTreeMap<String, u32>, wall: i64) -> Read {
        let b = |f: &str| num(&p.bat.join(f));
        let i = b("current_now");
        let status = rd(&p.bat.join("status")).unwrap_or_default();
        let screen = num(&p.backlight);
        let mut r = Read {
            v: b("voltage_now"),
            i,
            q: b("charge_counter"),
            cap: b("capacity"),
            bt: b("temp"),
            charging: status == "Charging" || status == "Full" || i.is_some_and(|i| i > 0),
            charging_known: super::energy::charging(&status, i),
            screen_on: screen.is_some_and(|x| x > 0),
            screen_known: screen.filter(|x| *x >= 0).map(|x| x > 0),
            ..Default::default()
        };
        let zones = self.zones.get_or_insert_with(|| {
            let mut v: Vec<String> = std::fs::read_dir(&p.thermal)
                .map(|dir| {
                    dir.flatten()
                        .map(|e| e.path())
                        .filter(|d| {
                            d.file_name()
                                .is_some_and(|n| n.to_string_lossy().starts_with("thermal_zone"))
                                && rd(&d.join("type")).is_some_and(|t| app_zone(&t))
                        })
                        .map(|d| d.to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            v.sort();
            v
        });
        for z in zones.iter() {
            if let Some(t) = num(&Path::new(z).join("temp")) {
                r.zt = Some(r.zt.map_or(t, |m| m.max(t)));
            }
        }
        if let Ok(rd_) = std::fs::read_dir(&p.cpufreq) {
            let mut pol: Vec<PathBuf> = rd_
                .flatten()
                .map(|e| e.path())
                .filter(|d| {
                    d.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with("policy"))
                })
                .collect();
            pol.sort();
            for d in pol {
                let Some(f) = num(&d.join("scaling_max_freq")) else {
                    continue;
                };
                let name = d
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let thr = r.thr.get_or_insert(false);
                let m = self.cpumax.entry(name).or_insert(0);
                if f > *m {
                    *m = f;
                } else if f < *m {
                    *thr = true;
                }
            }
        }
        let wan = std::fs::read_to_string(&p.route)
            .is_ok_and(|t| crate::crashwatch::io::route_up(&t, "rmnet_data0"));
        r.ts_on = wan && std::fs::read_to_string(&p.rc_local).is_ok_and(|t| t.contains(TS_START));
        if r.ts_on {
            r.ts_ok = ts_check(&p.ts_sock);
        }
        // paused by hand, or by the core while the screen is off and nobody is connected (6e):
        // off by intent, not unhealthy
        let boot = std::fs::read_to_string(p.proc_.join("sys/kernel/random/boot_id"))
            .ok()
            .zip(std::fs::read_to_string(p.proc_.join("uptime")).ok())
            .and_then(|(id, up)| luyaz_api::alerts::clock::BootTime::from_uptime(&id, &up));
        let paused = luyaz_api::alerts::deadline::manual_paused(&p.chill_dir, boot.as_ref())
            || std::fs::read_to_string(&p.chill_idle)
                .ok()
                .is_some_and(|s| luyaz_api::alerts::deadline::active(&s, boot.as_ref()));
        r.chill_on =
            wan && p.chill_dir.is_dir() && !p.chill_dir.join("disabled").exists() && !paused;
        if r.chill_on
            && mtime_age(&p.chill_state, wall).is_some_and(|a| a < 150)
            && let Ok(st) = std::fs::read_to_string(&p.chill_state)
            && st
                .lines()
                .next()
                .is_some_and(|l| l.contains("\"state\":\"running\""))
        {
            let pid = st.split("\"core_pid\":").nth(1).and_then(|x| {
                x.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse::<u32>()
                    .ok()
            });
            r.chill_ok = pid.is_some_and(|pid| {
                std::fs::read_link(p.proc_.join(pid.to_string()).join("exe"))
                    .is_ok_and(|x| x.to_string_lossy().ends_with("/mihomo"))
            });
        }
        if p.chill_dir.is_dir() {
            r.profile = Some(if r.chill_on {
                chill_profile(&p.chill_dir, &p.thermal_eco)
            } else {
                "off".into()
            });
        }
        let me = p.self_pid;
        let activity = super::activity::read(&p.proc_, me);
        r.cpu = activity.process.as_ref().map(|p| (p.pid, p.start, p.cpu));
        r.activity = Some(activity);
        for (k, prog) in RSS {
            let pid = if crate::paths::is_core(prog) {
                Some(me)
            } else {
                pids.get(prog).copied()
            };
            if let Some(kb) = pid.and_then(|pid| rss_kb(&p.proc_, pid)) {
                r.rss.insert(k.to_string(), kb);
            }
        }
        r.free_mb = free_mb(&p.data);
        r.dump_mb = du_mb(&p.dumps);
        r
    }
}

#[cfg(test)]
#[path = "power_tests.rs"]
mod tests;
