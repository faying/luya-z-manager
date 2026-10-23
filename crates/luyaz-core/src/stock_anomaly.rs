//! Stock-firmware anomalies, read-only (R18-P2-01; docs/designs/stock-anomaly-recovery.md).
//!
//! Two checks, both riding the usage log's minute tick (no poller of their own) and neither
//! touching anything:
//!
//! - **A `wifi_post_stuck`:** ZTE's `/sbin/zte_post_wifi.sh` spins without a sleep in
//!   `check_and_start_hostapd_cli()` until "enabled main*/guest* in uci" equals "running
//!   `hostapd_cli … wps-hostapd`" (firmware-capabilities §12.9). Open when the script has lived
//!   over [`WIFI_MIN_AGE`] s **and** the two counts differ, two ticks in a row; close when it
//!   exits or the counts meet. Age alone never decides. Opening queues `wifi-post-stuck`. The script is never killed: it still has MAC filter, power and mesh to set.
//! - **B `no_service`:** every loss of registration is one ledger row (start, end, length and
//!   which stock recovery was seen meanwhile: radio mode change → `lpm_cycle`, SIM identity
//!   epoch → `sim_reset`, only the data connection epoch → `redial`, none → `unknown`). SSR and
//!   whole-modem restarts have no verified source yet and land in `unknown`. Only an outage over
//!   [`NO_SERVICE_ALERT`] s with nothing seen queues `no-service-long`.
//!
//! Neither kind is sent as an SMS (owner 10-23: inbox and MCP only while false alarms are watched).

use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Mutex, OnceLock};

/// Script age (s) before a count mismatch counts; a normal run ends in about 5 s.
pub const WIFI_MIN_AGE: u64 = 30;
/// Ticks in a row the mismatch must hold.
const WIFI_HITS: u8 = 2;
/// Outage length (s) before the alert; clears the stock ~100 s guard (owner 10-23: 5 min).
pub const NO_SERVICE_ALERT: u64 = 300;
/// Ledger rows kept.
pub const LEDGER_KEEP: usize = 200;
pub const LEDGER: &str = "/data/luyaz/core/no-service.jsonl";
pub const WIFI_STUCK: &str = "wifi-post-stuck";
pub const NO_SERVICE_LONG: &str = "no-service-long";

// ---- A: zte_post_wifi.sh ----

/// One look at the script and both counts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WifiProbe {
    /// `(pid start in clock ticks, seconds alive)`; `None` = not running.
    pub script: Option<(u64, u64)>,
    /// Enabled main*/guest* sections; `None` = uci not readable without a fork this round.
    pub enabled: Option<Vec<String>>,
    /// `-i` interface of every running `hostapd_cli … wps-hostapd`.
    pub clis: Vec<String>,
    /// Machine-wide forks per second since the last tick, when known.
    pub forks_per_s: Option<f64>,
}

#[derive(Debug, Default)]
pub struct WifiWatch {
    hits: u8,
    open: Option<Value>,
}

#[derive(Debug, PartialEq)]
pub enum Edge {
    None,
    Opened(u64),
    Closed,
}

impl WifiWatch {
    pub fn step(&mut self, p: &WifiProbe) -> Edge {
        let stuck = match (&p.script, &p.enabled) {
            (Some((_, age)), Some(en)) => *age > WIFI_MIN_AGE && en.len() != p.clis.len(),
            // uci unknown this round: keep whatever we had
            (Some(_), None) => return Edge::None,
            (None, _) => false,
        };
        if !stuck {
            self.hits = 0;
            return if self.open.take().is_some() {
                Edge::Closed
            } else {
                Edge::None
            };
        }
        self.hits = self.hits.saturating_add(1);
        let (start, age) = p.script.unwrap_or_default();
        let en = p.enabled.clone().unwrap_or_default();
        let ev = json!({
            "alive_s": age,
            "enabled": en,
            "hostapd_cli": p.clis,
            "enabled_count": en.len(),
            "hostapd_cli_count": p.clis.len(),
            "forks_per_s": p.forks_per_s,
        });
        let was = self.open.is_some();
        if was || self.hits >= WIFI_HITS {
            self.open = Some(ev);
        }
        if !was && self.open.is_some() {
            Edge::Opened(start)
        } else {
            Edge::None
        }
    }

    pub fn open(&self) -> Option<&Value> {
        self.open.as_ref()
    }
}

/// Enabled `main*`/`guest*` `wifi-iface` sections, as the script counts them (disabled ≠ 1).
pub fn enabled_aps(pkg: &crate::uci::Package) -> Vec<String> {
    pkg.sections
        .iter()
        .filter_map(|s| {
            let name = String::from_utf8_lossy(s.name.as_deref()?).into_owned();
            if !(name.starts_with("main") || name.starts_with("guest")) {
                return None;
            }
            let off = s.options.iter().any(|(k, v)| {
                k == b"disabled" && matches!(v, crate::uci::Value::Str(x) if x == b"1")
            });
            (!off).then_some(name)
        })
        .collect()
}

/// Scan `proc` once: the post-wifi script (start ticks) and the `-i` of each wps hostapd_cli.
pub fn scan_proc(proc: &Path) -> (Option<u64>, Vec<String>) {
    let mut script = None;
    let mut clis = Vec::new();
    let Ok(dir) = std::fs::read_dir(proc) else {
        return (None, clis);
    };
    for e in dir.flatten() {
        let name = e.file_name();
        let Some(pid) = name
            .to_str()
            .filter(|n| n.bytes().all(|b| b.is_ascii_digit()))
        else {
            continue;
        };
        let Ok(cmd) = std::fs::read(proc.join(pid).join("cmdline")) else {
            continue;
        };
        let args: Vec<&[u8]> = cmd.split(|&b| b == 0).filter(|a| !a.is_empty()).collect();
        let has = |w: &[u8]| args.iter().any(|a| a.windows(w.len()).any(|x| x == w));
        if has(b"zte_post_wifi.sh") {
            let start = std::fs::read_to_string(proc.join(pid).join("stat"))
                .ok()
                .and_then(|s| start_ticks(&s));
            // the oldest copy decides
            if let Some(t) = start {
                script = Some(script.map_or(t, |o: u64| o.min(t)));
            }
        } else if has(b"hostapd_cli") && has(b"wps-hostapd") {
            let i = args
                .iter()
                .position(|a| *a == b"-i")
                .and_then(|i| args.get(i + 1))
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .unwrap_or_default();
            clis.push(i);
        }
    }
    clis.sort();
    (script, clis)
}

/// Field 22 of `/proc/<pid>/stat` (after the `)` that ends comm).
pub fn start_ticks(stat: &str) -> Option<u64> {
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

/// `processes` in `/proc/stat`: forks since boot.
pub fn forks(stat: &str) -> Option<u64> {
    stat.lines()
        .find_map(|l| l.strip_prefix("processes "))?
        .trim()
        .parse()
        .ok()
}

// ---- B: no-service ledger ----

/// Stock recovery markers at one moment.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Gens {
    /// `Collector::radio_mark`: real radio mode changes.
    pub radio: u64,
    /// `sim_identity` epoch.
    pub sim: u64,
    /// Data connection epoch (opaque).
    pub conn: Option<String>,
}

#[derive(Clone, Debug)]
struct Outage {
    start_up: u64,
    start_wall: i64,
    at: Gens,
    alerted: bool,
}

#[derive(Debug, Default)]
pub struct Ledger {
    open: Option<Outage>,
}

/// What a tick produced.
#[derive(Debug, Default, PartialEq)]
pub struct Tick {
    /// A finished outage, to append.
    pub row: Option<Value>,
    /// Queue `no-service-long` for the outage that started at this uptime.
    pub alert: Option<u64>,
}

/// What the stock firmware did between `a` and `b`, strongest first.
fn seen(a: &Gens, b: &Gens) -> Vec<&'static str> {
    let mut v = Vec::new();
    if a.radio != b.radio {
        v.push("lpm_cycle");
    }
    if a.sim != b.sim {
        v.push("sim_reset");
    }
    if a.conn != b.conn {
        v.push("redial");
    }
    v
}

impl Ledger {
    /// `registered`: `None` = unknown this round (nothing changes).
    pub fn step(&mut self, registered: Option<bool>, g: &Gens, up: u64, wall: i64) -> Tick {
        let mut t = Tick::default();
        match (registered, &mut self.open) {
            (Some(false), None) => {
                self.open = Some(Outage {
                    start_up: up,
                    start_wall: wall,
                    at: g.clone(),
                    alerted: false,
                })
            }
            (Some(false), Some(o)) => {
                if !o.alerted
                    && up.saturating_sub(o.start_up) >= NO_SERVICE_ALERT
                    && seen(&o.at, g).is_empty()
                {
                    o.alerted = true;
                    t.alert = Some(o.start_up);
                }
            }
            (Some(true), Some(_)) => {
                let o = self.open.take().unwrap_or_else(|| unreachable!());
                let s = seen(&o.at, g);
                t.row = Some(json!({
                    "start": o.start_wall,
                    "end": wall,
                    "secs": up.saturating_sub(o.start_up),
                    "stock": s.first().copied().unwrap_or("unknown"),
                    "seen": s,
                    "verified": !s.is_empty(),
                    "alerted": o.alerted,
                }));
            }
            _ => {}
        }
        t
    }

    pub fn current(&self, g: &Gens, up: u64) -> Option<Value> {
        let o = self.open.as_ref()?;
        Some(json!({
            "start": o.start_wall,
            "secs": up.saturating_sub(o.start_up),
            "seen": seen(&o.at, g),
            "alerted": o.alerted,
        }))
    }
}

/// Append `row` to the ledger text, keeping the last [`LEDGER_KEEP`] rows.
pub fn appended(text: &str, row: &Value) -> String {
    let mut rows: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let new = row.to_string();
    rows.push(&new);
    let skip = rows.len().saturating_sub(LEDGER_KEEP);
    rows[skip..].iter().map(|l| format!("{l}\n")).collect()
}

/// The newest `limit` rows, newest first.
pub fn history(limit: usize) -> Vec<Value> {
    let text = std::fs::read_to_string(crate::state::host_path(LEDGER)).unwrap_or_default();
    text.lines()
        .rev()
        .filter_map(|l| serde_json::from_str(l).ok())
        .take(limit)
        .collect()
}

// ---- IO ----

struct Live {
    wifi: WifiWatch,
    ledger: Ledger,
    forks: Option<(u64, u64)>,
    gens: Gens,
    up: u64,
}

static LIVE: Mutex<Option<Live>> = Mutex::new(None);
static HUB: OnceLock<std::sync::Arc<crate::block::Hub>> = OnceLock::new();

/// The core's hub, for the radio mark and connection epoch.
pub fn set_hub(hub: std::sync::Arc<crate::block::Hub>) {
    let _ = HUB.set(hub);
}

fn uptime() -> u64 {
    std::fs::read_to_string(crate::state::host_path("/proc/uptime"))
        .ok()
        .and_then(|s| s.split('.').next()?.parse().ok())
        .unwrap_or(0)
}

fn gens() -> Gens {
    let (radio, conn) = HUB.get().map_or((0, None), |h| {
        let f = h.firmware();
        (f.radio_mark(), f.token().context.connection_epoch)
    });
    let sim = crate::sim_identity::global()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .epoch();
    Gens { radio, sim, conn }
}

fn raise(code: u64, kind: &str, text: &str, up: u64) {
    let boot = crate::longterm::boot::proc_boot_id();
    let Some(b) = boot.get(..8) else { return };
    let id = format!("{}-{code}", b.to_ascii_lowercase());
    let dir = crate::state::host_path(luyaz_api::alerts::DIR);
    let a = luyaz_api::alerts::Alerts::at(Path::new(&dir));
    match a.add_once(&id, kind, text, luyaz_api::alerts::now(), up) {
        Ok((_, true)) => eprintln!("[stock_anomaly] {kind}: {text}"),
        Ok(_) => {}
        Err(e) => eprintln!("[stock_anomaly] {kind}: {e}"),
    }
}

fn save_row(row: &Value) {
    let p = crate::state::host_path(LEDGER);
    let text = appended(&std::fs::read_to_string(&p).unwrap_or_default(), row);
    let tmp = format!("{p}.tmp");
    if let Err(e) = std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, &p)) {
        eprintln!("[stock_anomaly] ledger: {e}");
    }
}

/// One minute tick, from the usage log with the snapshot's fields.
pub fn tick(fields: Option<&serde_json::Map<String, Value>>, wall: i64) {
    let up = uptime();
    let g = gens();
    let registered = fields
        .and_then(|f| f.get("net")?.get("type")?.as_str())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(crate::ops::registered);
    let proc = crate::state::host_path("/proc");
    let (start, clis) = scan_proc(Path::new(&proc));
    let hz = 100; // CONFIG_HZ-independent USER_HZ
    let script = start.map(|s| (s, up.saturating_sub(s / hz)));
    let enabled =
        script.and_then(|_| crate::state::uci_parsed("wireless").map(|p| enabled_aps(&p)));
    let fk = std::fs::read_to_string(format!("{proc}/stat"))
        .ok()
        .and_then(|s| forks(&s));

    let mut live = LIVE.lock().unwrap_or_else(|e| e.into_inner());
    let l = live.get_or_insert_with(|| Live {
        wifi: WifiWatch::default(),
        ledger: Ledger::default(),
        forks: None,
        gens: Gens::default(),
        up: 0,
    });
    let forks_per_s = match (l.forks, fk) {
        (Some((u0, f0)), Some(f)) if up > u0 => {
            Some((f.saturating_sub(f0)) as f64 / (up - u0) as f64)
        }
        _ => None,
    };
    if let Some(f) = fk {
        l.forks = Some((up, f));
    }
    let probe = WifiProbe {
        script,
        enabled,
        clis,
        forks_per_s,
    };
    if let Edge::Opened(s) = l.wifi.step(&probe) {
        raise(
            s,
            WIFI_STUCK,
            &format!(
                "zte_post_wifi.sh alive {}s, enabled {} vs hostapd_cli {}",
                probe.script.map_or(0, |x| x.1),
                probe.enabled.as_ref().map_or(0, Vec::len),
                probe.clis.len()
            ),
            up,
        );
    }
    let t = l.ledger.step(registered, &g, up, wall);
    if let Some(row) = &t.row {
        save_row(row);
    }
    if let Some(s) = t.alert {
        raise(
            s,
            NO_SERVICE_LONG,
            &format!(
                "no service {}s, no stock recovery seen",
                up.saturating_sub(s)
            ),
            up,
        );
    }
    l.gens = g;
    l.up = up;
}

/// `stock_anomalies` for `/v2/device/capabilities`.
pub fn report() -> Value {
    let live = LIVE.lock().unwrap_or_else(|e| e.into_inner());
    let (wifi, outage) = live.as_ref().map_or((None, None), |l| {
        (l.wifi.open().cloned(), l.ledger.current(&l.gens, l.up))
    });
    json!({
        "wifi_post_stuck": wifi,
        "no_service": {"open": outage, "recent": history(5)},
        "unverified": ["ssr", "modem_restart"],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(age: u64, en: usize, clis: usize) -> WifiProbe {
        WifiProbe {
            script: Some((1000, age)),
            enabled: Some((0..en).map(|i| format!("main{i}")).collect()),
            clis: (0..clis).map(|i| format!("ath{i}")).collect(),
            forks_per_s: None,
        }
    }

    #[test]
    fn wifi_opens_after_two_mismatched_ticks_and_closes() {
        let mut w = WifiWatch::default();
        assert_eq!(w.step(&probe(40, 2, 1)), Edge::None);
        assert_eq!(w.step(&probe(100, 2, 1)), Edge::Opened(1000));
        assert_eq!(w.open().unwrap()["enabled_count"], 2);
        assert_eq!(w.step(&probe(160, 2, 1)), Edge::None);
        assert_eq!(w.step(&probe(220, 2, 2)), Edge::Closed);
        assert!(w.open().is_none());
    }

    #[test]
    fn wifi_never_judges_on_age_alone_or_young_script() {
        let mut w = WifiWatch::default();
        for _ in 0..3 {
            assert_eq!(w.step(&probe(500, 2, 2)), Edge::None);
            assert_eq!(w.step(&probe(10, 2, 0)), Edge::None);
        }
        // gone
        let gone = WifiProbe::default();
        assert_eq!(w.step(&gone), Edge::None);
        // uci unknown keeps the count
        assert_eq!(w.step(&probe(40, 1, 0)), Edge::None);
        let mut p = probe(100, 1, 0);
        p.enabled = None;
        assert_eq!(w.step(&p), Edge::None);
        assert_eq!(w.step(&probe(160, 1, 0)), Edge::Opened(1000));
    }

    #[test]
    fn enabled_aps_counts_main_and_guest_not_disabled() {
        let pkg = crate::uci::parse(
            b"config wifi-iface 'main_2g'\n\toption disabled '0'\nconfig wifi-iface 'main_5g'\n\toption disabled '1'\nconfig wifi-iface 'guest_2g'\nconfig wifi-device 'wifi0'\nconfig wifi-iface 'mesh'\n",
        )
        .unwrap();
        assert_eq!(enabled_aps(&pkg), ["main_2g", "guest_2g"]);
    }

    #[test]
    fn scan_finds_script_and_wps_clis() {
        let d = std::env::temp_dir().join(format!("sa-proc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let mk = |pid: &str, cmd: &[u8], stat: &str| {
            let p = d.join(pid);
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("cmdline"), cmd).unwrap();
            std::fs::write(p.join("stat"), stat).unwrap();
        };
        let stat = |t: u64| format!("7 (sh x) S 1 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 {t} 0 0");
        mk("10", b"sh\0/sbin/zte_post_wifi.sh\0", &stat(5000));
        mk("11", b"sh\0/sbin/zte_post_wifi.sh\0", &stat(4000));
        mk(
            "12",
            b"hostapd_cli\0-i\0ath1\0-a\0/usr/sbin/wps-hostapd\0-B\0",
            &stat(1),
        );
        mk("13", b"hostapd_cli\0-i\0ath0\0-p\0/var/run\0", &stat(1));
        mk("self", b"x", &stat(1));
        let (s, c) = scan_proc(&d);
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(s, Some(4000));
        assert_eq!(c, ["ath1"]);
    }

    #[test]
    fn stat_fields() {
        assert_eq!(
            start_ticks("1 (a) b) S 1 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 777 0"),
            Some(777)
        );
        assert_eq!(
            forks("cpu 1 2\nprocesses 4321\nprocs_running 1\n"),
            Some(4321)
        );
    }

    fn g(radio: u64, sim: u64, conn: &str) -> Gens {
        Gens {
            radio,
            sim,
            conn: Some(conn.into()),
        }
    }

    #[test]
    fn ledger_alerts_once_only_when_stock_did_nothing() {
        let mut l = Ledger::default();
        assert_eq!(
            l.step(Some(false), &g(1, 1, "a"), 100, 9100),
            Tick::default()
        );
        assert_eq!(l.step(None, &g(1, 1, "a"), 200, 9200), Tick::default());
        assert_eq!(
            l.step(Some(false), &g(1, 1, "a"), 399, 9399),
            Tick::default()
        );
        let t = l.step(Some(false), &g(1, 1, "a"), 400, 9400);
        assert_eq!(t.alert, Some(100));
        assert_eq!(l.step(Some(false), &g(1, 1, "a"), 460, 9460).alert, None);
        let row = l.step(Some(true), &g(1, 1, "a"), 500, 9500).row.unwrap();
        assert_eq!(row["stock"], "unknown");
        assert_eq!(row["verified"], false);
        assert_eq!(row["alerted"], true);
        assert_eq!(row["secs"], 400);
    }

    #[test]
    fn ledger_labels_stock_recovery() {
        let mut l = Ledger::default();
        l.step(Some(false), &g(1, 1, "a"), 0, 0);
        assert_eq!(l.step(Some(false), &g(2, 1, "b"), 600, 600).alert, None);
        let row = l.step(Some(true), &g(2, 1, "b"), 660, 660).row.unwrap();
        assert_eq!(row["stock"], "lpm_cycle");
        assert_eq!(row["seen"], json!(["lpm_cycle", "redial"]));
        l.step(Some(false), &g(2, 1, "b"), 700, 700);
        let row = l.step(Some(true), &g(2, 2, "b"), 760, 760).row.unwrap();
        assert_eq!(row["stock"], "sim_reset");
        // registered with nothing open: no row
        assert_eq!(l.step(Some(true), &g(2, 2, "b"), 800, 800), Tick::default());
    }

    #[test]
    fn ledger_file_keeps_last_rows() {
        let mut text = String::new();
        for i in 0..(LEDGER_KEEP + 5) {
            text = appended(&text, &json!({"i": i}));
        }
        let rows: Vec<&str> = text.lines().collect();
        assert_eq!(rows.len(), LEDGER_KEEP);
        assert_eq!(rows[0], r#"{"i":5}"#);
    }
}
