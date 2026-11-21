//! Stock-firmware anomalies and recovery.
//!
//! Two checks, both riding the usage log's minute tick (no poller of their own):
//!
//! - **A `wifi_post_stuck`:** ZTE's `/sbin/zte_post_wifi.sh` spins without a sleep in
//!   `check_and_start_hostapd_cli()` until "enabled main*/guest* in uci" equals "running
//!   `hostapd_cli … wps-hostapd`" (firmware-capabilities §12.9). Open when the script has lived
//!   over [`WIFI_MIN_AGE`] s **and** the two counts differ, two ticks in a row; close when it
//!   exits or the counts meet. Opening queues `wifi-post-stuck`. On the verified stock script,
//!   an orphan still spinning with the module, radios and mesh all off can be terminated once
//!   the kernel confirms that there are no Wi-Fi interfaces left to configure. With one radio
//!   off and all effective APs already served, run the verified stock post steps before stopping it.
//! - **B `no_service`:** every loss of registration is one ledger row (start, end, length and
//!   which stock recovery was seen meanwhile: radio mode change → `lpm_cycle`, SIM identity
//!   epoch → `sim_reset`, only the data connection epoch → `redial`, none → `unknown`). SSR and
//!   whole-modem restarts have no verified source yet and land in `unknown`. Only an outage over
//!   [`NO_SERVICE_ALERT`] s with nothing seen queues `no-service-long`.
//!
//! A also has an event path ([`spawn_wifi_watch`]): an inotify `IN_OPEN` on the script wakes a
//! thread that looks again just past [`WIFI_MIN_AGE`] and once more after that, so a spin opens
//! about a minute after the script starts instead of two or three minute ticks later. The thread
//! sleeps in a blocking `read` the rest of the time (no polling); without inotify the minute tick
//! alone still covers it.
//!
//! Neither kind is sent as an SMS (owner 10-23: inbox and MCP only while false alarms are watched).

use luyaz_wifi::layout::{self, Uci};
use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Script age (s) before a count mismatch counts; a normal run ends in about 5 s.
pub const WIFI_MIN_AGE: u64 = 30;
/// Ticks in a row the mismatch must hold.
const WIFI_HITS: u8 = 2;
const USER_HZ: u64 = 100; // independent of CONFIG_HZ
const CLEANUP_WINDOW: u64 = 30;
const CLEANUP_MIN_AGE: u64 = 60;
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

/// The signed script counts explicitly enabled main/guest flags without consulting the radios.
fn enabled_aps(cfg: &Uci) -> impl Iterator<Item = &str> {
    cfg.iter().filter_map(|(key, value)| {
        let section = key.strip_prefix("wireless.")?.strip_suffix(".disabled")?;
        (value == "0" && (section.starts_with("main") || section.starts_with("guest")))
            .then_some(section)
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PostWifiProc {
    pid: i32,
    start_ticks: u64,
    parent_pid: i32,
    cpu_ticks: u64,
}

impl PostWifiProc {
    fn parse(pid: i32, stat: &str) -> Option<Self> {
        let mut fields = stat.rsplit_once(')')?.1.split_whitespace();
        let parent_pid = fields.nth(1)?.parse().ok()?; // field 4
        let utime = fields.nth(9)?.parse::<u64>().ok()?; // field 14
        let stime = fields.next()?.parse().ok()?;
        let cutime = fields.next()?.parse().ok()?;
        let cstime = fields.next()?.parse().ok()?;
        Some(Self {
            pid,
            start_ticks: fields.nth(4)?.parse().ok()?, // field 22
            parent_pid,
            cpu_ticks: utime
                .checked_add(stime)?
                .checked_add(cutime)?
                .checked_add(cstime)?,
        })
    }

    fn id(self) -> (i32, u64) {
        (self.pid, self.start_ticks)
    }
}

fn is_post_wifi(cmdline: &[u8]) -> bool {
    matches!(
        cmdline,
        b"sh\0/sbin/zte_post_wifi.sh\0" | b"/bin/sh\0/sbin/zte_post_wifi.sh\0"
    )
}

/// Scan `proc` once: the oldest post-wifi script and the `-i` of each WPS hostapd_cli.
fn scan_proc(proc: &Path) -> (Option<PostWifiProc>, Vec<String>) {
    let mut script: Option<PostWifiProc> = None;
    let mut clis = Vec::new();
    let Ok(dir) = std::fs::read_dir(proc) else {
        return (None, clis);
    };
    for e in dir.flatten() {
        let name = e.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|n| n.parse::<i32>().ok())
            .filter(|&n| n > 0)
        else {
            continue;
        };
        let path = e.path();
        let Ok(cmd) = std::fs::read(path.join("cmdline")) else {
            continue;
        };
        if is_post_wifi(&cmd) {
            let post = std::fs::read_to_string(path.join("stat"))
                .ok()
                .and_then(|stat| PostWifiProc::parse(pid, &stat));
            if let Some(post) = post
                && script.is_none_or(|old| post.start_ticks < old.start_ticks)
            {
                script = Some(post);
            }
        } else {
            let mut args = cmd.split(|&b| b == 0);
            let exe = args.next().and_then(|a| a.rsplit(|&b| b == b'/').next());
            // The stock loop runs grep with both names in its pattern on each iteration.
            if exe != Some(b"hostapd_cli")
                || !args.clone().any(|a| a.windows(b"wps-hostapd".len()).any(|s| s == b"wps-hostapd"))
            {
                continue;
            }
            let i = args
                .filter(|a| !a.is_empty())
                .skip_while(|a| *a != b"-i")
                .nth(1)
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .unwrap_or_default();
            clis.push(i);
        }
    }
    clis.sort();
    (script, clis)
}

fn flag<'a>(cfg: &'a Uci, key: &str) -> Option<&'a str> {
    cfg.get(key).map(String::as_str)
}

fn off_layout(cfg: &Uci) -> bool {
    [
        "wireless.zte_mbb.wifi_onoff",
        "wireless.zte_mbb.wifi_onoff_by_user",
        "wireless.zte_mbb.mesh_onoff",
    ]
    .into_iter()
    .all(|key| flag(cfg, key) == Some("0"))
        && ["wireless.wifi0.disabled", "wireless.wifi1.disabled"]
            .into_iter()
            .all(|key| flag(cfg, key) == Some("1"))
}

fn active_ifnames(cfg: &Uci, enabled: usize, clis: &[String]) -> Option<Vec<String>> {
    if flag(cfg, "wireless.zte_mbb.wifi_onoff") != Some("1")
        || flag(cfg, "wireless.zte_mbb.mesh_onoff") != Some("0")
    {
        return None;
    }
    let main = match (flag(cfg, "wireless.wifi0.disabled"), flag(cfg, "wireless.wifi1.disabled")) {
        (Some("1"), Some("0")) => "main_5g",
        (Some("0"), Some("1")) => "main_2g",
        _ => return None,
    };
    let effective = layout::running(cfg);
    if enabled <= effective.len() || !effective.contains(&main) {
        return None;
    }
    let mut names = Vec::with_capacity(effective.len());
    for section in effective {
        let name = layout::ifname(cfg, section)?;
        // The vendor's watcher and post steps address the explicitly configured interface.
        if flag(cfg, &format!("wireless.{section}.ifname")) != Some(name)
            || flag(cfg, &format!("wireless.{section}.mode")) != Some("ap")
        {
            return None;
        }
        names.push(name.to_owned());
    }
    names.sort_unstable();
    (names == clis).then_some(names)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RecoveryKind {
    Off,
    Active,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Recovery {
    post: PostWifiProc,
    kind: RecoveryKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RecoveryResult {
    Deferred,
    Attempted,
    Stopped,
}

#[derive(Default)]
struct WifiCleanup {
    first: Option<(Recovery, u64)>,
    attempting: bool,
    handled: Vec<(i32, u64)>,
}

impl WifiCleanup {
    fn observe(
        &mut self,
        post: Option<PostWifiProc>,
        probe: &WifiProbe,
        cfg: Option<&Uci>,
        up: u64,
    ) -> Option<Recovery> {
        if self.attempting {
            return None;
        }
        let candidate = || {
            let post = post.filter(|p| p.parent_pid == 1 && !self.handled.contains(&p.id()))?;
            let cfg = cfg?;
            let enabled = probe.enabled.as_ref()?.len();
            if enabled > 0 && probe.clis.is_empty() && off_layout(cfg) {
                Some(Recovery { post, kind: RecoveryKind::Off })
            } else {
                active_ifnames(cfg, enabled, &probe.clis)?;
                Some(Recovery { post, kind: RecoveryKind::Active })
            }
        };
        let Some(target) = candidate() else {
            self.first = None;
            return None;
        };
        let post = target.post;
        let Some((first, since)) = self.first.filter(|(p, _)| p.post.id() == post.id() && p.kind == target.kind) else {
            self.first = Some((target, up));
            return None;
        };
        let elapsed = up.checked_sub(since)?;
        if elapsed < CLEANUP_WINDOW
            || up.saturating_sub(post.start_ticks / USER_HZ) < CLEANUP_MIN_AGE
        {
            return None;
        }
        // An attempt or an interval below the CPU threshold both start a new sample window.
        self.first = Some((target, up));
        if post.cpu_ticks.saturating_sub(first.post.cpu_ticks) < elapsed.saturating_mul(USER_HZ / 2) {
            return None;
        }
        self.attempting = true;
        Some(target)
    }

    fn finish(&mut self, target: Recovery, result: RecoveryResult) {
        self.attempting = false;
        if result != RecoveryResult::Deferred {
            self.handled.push(target.post.id());
        }
    }
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
    wifi_cleanup: WifiCleanup,
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

fn live() -> std::sync::MutexGuard<'static, Option<Live>> {
    LIVE.lock().unwrap_or_else(|e| e.into_inner())
}

fn live_of(slot: &mut Option<Live>) -> &mut Live {
    slot.get_or_insert_with(|| Live {
        wifi: WifiWatch::default(),
        wifi_cleanup: WifiCleanup::default(),
        ledger: Ledger::default(),
        forks: None,
        gens: Gens::default(),
        up: 0,
    })
}

fn wifi_sample(up: u64) -> (Option<PostWifiProc>, WifiProbe, Option<Uci>, Option<u64>) {
    let proc = crate::state::host_path("/proc");
    let (post, clis) = scan_proc(Path::new(&proc));
    let script = post.map(|p| (p.start_ticks, up.saturating_sub(p.start_ticks / USER_HZ)));
    let wireless = post.and_then(|_| crate::state::wifi_map_sync());
    let fk = std::fs::read_to_string(format!("{proc}/stat"))
        .ok()
        .and_then(|s| forks(&s));
    let probe = WifiProbe {
        script,
        enabled: wireless.as_ref().map(|cfg| enabled_aps(cfg).map(str::to_owned).collect()),
        clis,
        forks_per_s: None,
    };
    (post, probe, wireless, fk)
}

fn sample_forks(l: &mut Live, probe: &mut WifiProbe, up: u64, fk: Option<u64>) {
    probe.forks_per_s = match (l.forks, fk) {
        (Some((u0, f0)), Some(f)) if up > u0 => {
            Some((f.saturating_sub(f0)) as f64 / (up - u0) as f64)
        }
        _ => None,
    };
    if let Some(f) = fk && l.forks.is_none_or(|(u0, _)| up >= u0) {
        l.forks = Some((up, f));
    }
}

/// The minute tick and event watcher share one claim. All device I/O runs outside LIVE.
fn wifi_check(up: u64) {
    let (post, mut probe, wireless, fk) = wifi_sample(up);
    let (edge, recovery) = {
        let mut slot = live();
        let l = live_of(&mut slot);
        if l.wifi_cleanup.attempting || l.forks.is_some_and(|(u0, _)| up < u0) {
            return;
        }
        sample_forks(l, &mut probe, up, fk);
        let recovery = l.wifi_cleanup.observe(post, &probe, wireless.as_ref(), up);
        (l.wifi.step(&probe), recovery)
    };
    if let Some(target) = recovery {
        let result = signal_stale_wifi_post(target);
        if result == RecoveryResult::Stopped {
            let now = uptime();
            let (_, mut fresh, _, fk) = wifi_sample(now);
            let mut slot = live();
            let l = live_of(&mut slot);
            l.wifi_cleanup.finish(target, result);
            sample_forks(l, &mut fresh, now, fk);
            l.wifi.step(&fresh);
            return;
        }
        live_of(&mut live()).wifi_cleanup.finish(target, result);
    }
    if let Edge::Opened(s) = edge {
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
}

fn no_wifi_interfaces(success: bool, stdout: &[u8]) -> bool {
    success && stdout.iter().all(u8::is_ascii_whitespace)
}

const STOCK_SHA: [u8; 32] = [
    0x6c, 0x92, 0x3d, 0x29, 0x85, 0x4f, 0xa7, 0xa5, 0x03, 0x77, 0xb2, 0xef, 0x60, 0xaa,
    0x79, 0x68, 0x4f, 0xe4, 0x34, 0x54, 0xd2, 0x2c, 0xe7, 0xf0, 0x2f, 0x12, 0xda, 0x6d,
    0xea, 0xaf, 0xfb, 0x75,
];
const POST_BOUNDARY: &str = "\necho \"zte_post_wifi $LINENO: sleep 3s and go on...\" > /dev/kmsg\n";
const POST_COMPLETE: &str = "luyaz-stock-wifi-post-complete";

fn verified_stock(script: &[u8], digest: &[u8; 32]) -> bool {
    use sha2::{Digest, Sha256};
    Sha256::digest(script)[..] == digest[..]
}

fn post_functions(script: &str) -> Option<&str> {
    let (definitions, rest) = script.split_once(POST_BOUNDARY)?;
    (!rest.contains(POST_BOUNDARY)).then_some(definitions)
}

fn replay_post(definitions: &str, limit: Duration, stop: impl FnOnce() -> bool) -> RecoveryResult {
    // Preserve the stock sequence. Its function statuses are not a success contract (e.g. the
    // txpower loop can end on a disabled radio). Redirection also covers its background DFS helper.
    let command = format!(
        "{definitions}\n{{\nhandle_macfilter_at_post\nzte_post_txpower\n}} >/dev/null 2>&1\nprintf '%s\\n' '{POST_COMPLETE}'\n"
    );
    let result = luyaz_wifi::output_within(Command::new("/bin/sh").args(["-c", &command]), limit);
    match result {
        Ok(out) if out.status.success() && out.stdout.trim_ascii() == POST_COMPLETE.as_bytes() => {
            if stop() { RecoveryResult::Stopped } else { RecoveryResult::Attempted }
        }
        Ok(_) => RecoveryResult::Attempted,
        Err(e) => {
            eprintln!("[stock_anomaly] stock Wi-Fi post steps: {e}");
            RecoveryResult::Attempted
        }
    }
}

fn only_expected_aps(iw: &str, expected: &[String], down: impl Fn(&str) -> bool) -> bool {
    let mut current = None;
    let mut seen = 0;
    for line in iw.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix("Interface ") {
            current = Some(name);
        } else if line.starts_with("phy#") {
            current = None;
        } else if line == "type AP" {
            let Some(name) = current.take() else { return false };
            if expected.iter().any(|ap| ap == name) {
                seen += 1;
            } else if !down(name) {
                return false;
            }
        }
    }
    seen == expected.len()
}

fn proc_matches(proc: &Path, post: PostWifiProc) -> bool {
    let path = proc.join(post.pid.to_string());
    std::fs::read(path.join("cmdline")).is_ok_and(|cmd| is_post_wifi(&cmd))
        && std::fs::read_to_string(path.join("stat"))
            .ok()
            .and_then(|s| PostWifiProc::parse(post.pid, &s))
            .is_some_and(|p| p.id() == post.id() && p.parent_pid == 1)
}

/// Take a fresh layout and process sample with the Wi-Fi lock held.
fn recovery_layout(target: Recovery) -> Option<Vec<String>> {
    let cfg = crate::state::wifi_map_sync()?;
    let (post, clis) = scan_proc(Path::new("/proc"));
    if post.is_none_or(|p| p.id() != target.post.id() || p.parent_pid != 1) {
        return None;
    }
    let enabled = enabled_aps(&cfg).count();
    let names = match target.kind {
        RecoveryKind::Off if enabled != 0 && clis.is_empty() && off_layout(&cfg) => Vec::new(),
        RecoveryKind::Off => return None,
        RecoveryKind::Active => active_ifnames(&cfg, enabled, &clis)?,
    };
    if luyaz_wifi::settled_layout(&cfg) != Some(true) {
        return None;
    }
    let iw = luyaz_wifi::output_within(Command::new("iw").arg("dev"), Duration::from_secs(2)).ok()?;
    let ready = if names.is_empty() {
        no_wifi_interfaces(iw.status.success(), &iw.stdout)
    } else {
        iw.status.success() && only_expected_aps(std::str::from_utf8(&iw.stdout).ok()?, &names, |ifname| {
            ifname != "." && ifname != ".." && !ifname.contains('/')
                && std::fs::read(format!("/sys/class/net/{ifname}/operstate"))
                    .is_ok_and(|state| state.trim_ascii() == b"down")
        })
    };
    ready.then_some(names)
}

fn with_wifi_lock(path: &str, recover: impl FnOnce() -> RecoveryResult) -> RecoveryResult {
    let Ok(Some(_lock)) = crate::ops::wifi_lock::try_take(path) else {
        return RecoveryResult::Deferred;
    };
    recover()
}

fn signal_stale_wifi_post(target: Recovery) -> RecoveryResult {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use RecoveryResult::{Attempted, Deferred, Stopped};

    if !cfg!(all(target_os = "linux", target_arch = "aarch64"))
        || crate::state::host_path("/proc") != "/proc"
    {
        return Deferred;
    }
    let Ok(script) = std::fs::read(WIFI_SCRIPT) else {
        return Deferred;
    };
    if !verified_stock(&script, &STOCK_SHA) {
        return Deferred;
    }
    let definitions = std::str::from_utf8(&script).ok().and_then(post_functions);
    if target.kind == RecoveryKind::Active && definitions.is_none() {
        return Deferred;
    }
    let Some(path) = crate::ops::wifi_lock::Cfg::from_env().path else { return Deferred };
    with_wifi_lock(&path, || {
        let post = target.post;
        // SAFETY: the syscall takes scalar arguments and returns a fresh fd on success.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, post.pid, 0) };
        if fd < 0 {
            return Deferred;
        }
        // SAFETY: this scope owns the descriptor from pidfd_open.
        let fd = unsafe { OwnedFd::from_raw_fd(fd as i32) };
        let proc = Path::new("/proc");
        if !proc_matches(proc, post) {
            return Deferred;
        }
        let Some(names) = recovery_layout(target) else { return Deferred };
        if !proc_matches(proc, post) {
            return Deferred;
        }
        let stop = || {
            if !proc_matches(proc, post) {
                return false;
            }
            // SAFETY: fd pins the matching process identity through signal delivery.
            let sent = unsafe {
                libc::syscall(libc::SYS_pidfd_send_signal, fd.as_raw_fd(), libc::SIGTERM, std::ptr::null::<libc::siginfo_t>(), 0)
            } == 0;
            let mut p = libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            // SAFETY: poll borrows one initialized record for up to one second.
            sent && unsafe { libc::poll(&mut p, 1, 1000) } == 1 && p.revents & libc::POLLIN != 0
        };
        let result = match target.kind {
            RecoveryKind::Off if stop() => Stopped,
            RecoveryKind::Off => Deferred,
            RecoveryKind::Active => replay_post(definitions.unwrap(), Duration::from_secs(20), || {
                recovery_layout(target).is_some_and(|fresh| fresh == names) && stop()
            }),
        };
        match (target.kind, result) {
            (RecoveryKind::Active, Stopped) => eprintln!(
                "[stock_anomaly] completed stock Wi-Fi post steps and stopped stale pid={}", post.pid
            ),
            (RecoveryKind::Active, Attempted) => eprintln!(
                "[stock_anomaly] stock Wi-Fi post recovery unconfirmed pid={}", post.pid
            ),
            (RecoveryKind::Off, Stopped) => eprintln!(
                "[stock_anomaly] signalled stale Wi-Fi post script pid={}", post.pid
            ),
            _ => (),
        }
        result
    })
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
    let t = {
        let mut slot = live();
        let l = live_of(&mut slot);
        let t = l.ledger.step(registered, &g, up, wall);
        l.gens = g;
        l.up = up;
        t
    };
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
    wifi_check(up);
}

pub const WIFI_SCRIPT: &str = "/sbin/zte_post_wifi.sh";
/// After the script starts: first look just past `WIFI_MIN_AGE`, the second one this much later
/// (the two hits `WifiWatch` needs).
const WATCH_FIRST: u64 = WIFI_MIN_AGE + 5;
const WATCH_AGAIN: u64 = 30;

/// Event path for check A (see the module docs). Returns at once; the thread ends quietly when
/// inotify or the script is missing (the minute tick still covers it).
pub fn spawn_wifi_watch() {
    let _ = std::thread::Builder::new()
        .name("wifi-post-watch".into())
        .spawn(|| {
            let Some(fd) = open_watch(&crate::state::host_path(WIFI_SCRIPT)) else {
                eprintln!("[stock_anomaly] no inotify on {WIFI_SCRIPT}: minute tick only");
                return;
            };
            let mut buf = [0u8; 1024];
            loop {
                // blocking read: the thread sleeps until something opens the script
                let n = unsafe {
                    // SAFETY: read into a buffer we own, from an fd we own.
                    libc::read(
                        std::os::fd::AsRawFd::as_raw_fd(&fd),
                        buf.as_mut_ptr().cast(),
                        buf.len(),
                    )
                };
                if n < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                if n <= 0 {
                    eprintln!("[stock_anomaly] wifi watch read failed: minute tick only");
                    return;
                }
                for wait in [WATCH_FIRST, WATCH_AGAIN] {
                    std::thread::sleep(std::time::Duration::from_secs(wait));
                    wifi_check(uptime());
                }
                // opens while we waited belong to the run just looked at (or a later one the
                // minute tick sees): drop them, then wait for the next start
                drain(&fd);
            }
        });
}

fn open_watch(path: &str) -> Option<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd;
    let c = std::ffi::CString::new(path).ok()?;
    // SAFETY: inotify_init1 / add_watch with plain values; the fd is owned below.
    unsafe {
        let fd = libc::inotify_init1(libc::IN_CLOEXEC);
        if fd < 0 {
            return None;
        }
        let owned = std::os::fd::OwnedFd::from_raw_fd(fd);
        (libc::inotify_add_watch(fd, c.as_ptr(), libc::IN_OPEN) >= 0).then_some(owned)
    }
}

fn drain(fd: &std::os::fd::OwnedFd) {
    use std::os::fd::AsRawFd;
    let raw = fd.as_raw_fd();
    let mut buf = [0u8; 1024];
    loop {
        let mut p = libc::pollfd {
            fd: raw,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one pollfd we own, no wait.
        if unsafe { libc::poll(&mut p, 1, 0) } <= 0 {
            return;
        }
        // SAFETY: read into a buffer we own.
        if unsafe { libc::read(raw, buf.as_mut_ptr().cast(), buf.len()) } <= 0 {
            return;
        }
    }
}

/// `stock_anomalies` for `/v2/device/capabilities`.
pub fn report() -> Value {
    let live = live();
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

    fn config(text: &str) -> Uci {
        let pkg = crate::uci::parse(text.as_bytes()).unwrap();
        crate::state::uci_show_map(&crate::uci::render_show("wireless", &pkg))
    }

    #[test]
    fn enabled_aps_counts_only_explicit_zero_flags() {
        let cfg = config(
            "config wifi-iface 'main_2g'\noption disabled '0'\n\
             config wifi-iface 'main_5g'\noption disabled '1'\n\
             config wifi-iface 'guest_2g'\n\
             config wifi-iface 'guest_5g'\noption disabled '0'\n\
             config wifi-iface 'mesh'\noption disabled '0'\n",
        );
        assert_eq!(enabled_aps(&cfg).collect::<Vec<_>>(), ["guest_5g", "main_2g"]);
    }

    fn proc_fixture(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("sa-proc-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn proc_entry(d: &Path, pid: &str, cmd: &[u8], start: u64, utime: u64) {
        let p = d.join(pid);
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("cmdline"), cmd).unwrap();
        std::fs::write(
            p.join("stat"),
            format!("{pid} (sh) x) S 1 1 1 0 -1 0 0 0 0 0 {utime} 20 30 40 20 0 1 0 {start} 0 0"),
        )
        .unwrap();
    }

    #[test]
    fn scan_finds_script_and_wps_clis() {
        let d = proc_fixture("scan");
        let mk = |pid, cmd, start| proc_entry(&d, pid, cmd, start, 10);
        mk("10", b"/bin/sh\0/sbin/zte_post_wifi.sh\0", 5000);
        mk("11", b"sh\0/sbin/zte_post_wifi.sh\0", 4000);
        mk(
            "12",
            b"/usr/sbin/hostapd_cli\0-i\0ath1\0-B\0-a\0/lib/wifi/wps-hostapd-update-uci.sh\0",
            1,
        );
        mk("13", b"hostapd_cli\0-i\0ath0\0-p\0/var/run\0", 1);
        mk("14", b"sh\0/sbin/zte_post_wifi.sh\0extra\0", 1);
        mk("15", b"sh\0/tmp/zte_post_wifi.sh\0", 1);
        mk("16", b"sh\0-c\0sh /sbin/zte_post_wifi.sh\0", 1);
        mk("17", b"grep\0-c\0hostapd_cli.*wps-hostapd\0", 1);
        mk("self", b"x", 1);
        let (s, c) = scan_proc(&d);
        std::fs::remove_dir_all(&d).unwrap();
        assert_eq!(
            s,
            Some(PostWifiProc {
                pid: 11,
                start_ticks: 4000,
                parent_pid: 1,
                cpu_ticks: 100,
            })
        );
        assert_eq!(c, ["ath1"]);
    }

    #[test]
    fn stale_wifi_post_requires_powered_off_module_and_same_process() {
        let d = proc_fixture("cleanup");
        let sample = |start, utime| {
            proc_entry(&d, "5202", b"sh\0/sbin/zte_post_wifi.sh\0", start, utime);
            scan_proc(&d).0
        };
        let first = sample(1000, 10);
        let spinning = sample(1000, 1510);
        let reused = sample(1100, 1510);
        let low_cpu = sample(1000, 1509);
        std::fs::remove_dir_all(&d).unwrap();

        let wireless = "config device 'zte_mbb'\n\
            option wifi_onoff '0'\noption wifi_onoff_by_user '0'\noption mesh_onoff '0'\n\
            config wifi-device 'wifi0'\noption disabled '1'\n\
            config wifi-device 'wifi1'\noption disabled '1'\n\
            config wifi-iface 'main_2g'\noption disabled '0'\n";
        let off = config(wireless);
        let off_probe = probe(90, 1, 0);
        let no_aps = probe(90, 0, 0);
        let matched = probe(90, 1, 1);
        let still_child = spinning.map(|p| PostWifiProc { parent_pid: 200, ..p });

        for (second, snapshot, pkg, up) in [
            (reused, &off_probe, Some(&off), 130),
            (low_cpu, &off_probe, Some(&off), 130),
            (still_child, &off_probe, Some(&off), 130),
            (spinning, &matched, Some(&off), 130),
            (spinning, &no_aps, Some(&off), 130),
            (spinning, &off_probe, None, 130),
            (spinning, &off_probe, Some(&off), 129),
        ] {
            let mut w = WifiCleanup::default();
            assert_eq!(w.observe(first, &off_probe, Some(&off), 100), None);
            assert_eq!(w.observe(second, snapshot, pkg, up), None);
        }

        let mut young = WifiCleanup::default();
        assert_eq!(young.observe(first, &off_probe, Some(&off), 39), None);
        assert_eq!(young.observe(spinning, &off_probe, Some(&off), 69), None);
        for (old, new) in [
            ("wifi_onoff '0'", "wifi_onoff '1'"),
            ("wifi_onoff_by_user '0'", "wifi_onoff_by_user '1'"),
            ("mesh_onoff '0'", "mesh_onoff '1'"),
            ("wifi0'\noption disabled '1'", "wifi0'\noption disabled '0'"),
            ("wifi1'\noption disabled '1'", "wifi1'\noption disabled '0'"),
        ] {
            let mut w = WifiCleanup::default();
            let on = config(&wireless.replace(old, new));
            assert_eq!(w.observe(first, &off_probe, Some(&off), 100), None);
            assert_eq!(w.observe(spinning, &off_probe, Some(&on), 130), None, "{new}");
            assert_eq!(w.observe(spinning, &off_probe, Some(&off), 131), None);
        }

        assert!(!no_wifi_interfaces(true, b"phy#1\n\tInterface wlan2\n"));
        assert!(!no_wifi_interfaces(false, b""));
        assert!(no_wifi_interfaces(true, b" \r\n"));

        let mut w = WifiCleanup::default();
        assert_eq!(w.observe(first, &off_probe, Some(&off), 100), None);
        assert_eq!(w.observe(spinning, &off_probe, Some(&off), 129), None);
        let claimed = w.observe(spinning, &off_probe, Some(&off), 130).unwrap();
        let later = spinning.map(|p| PostWifiProc { cpu_ticks: p.cpu_ticks + 1500, ..p });
        assert_eq!(w.observe(later, &off_probe, Some(&off), 160), None);
        w.finish(claimed, RecoveryResult::Deferred);
        assert_eq!(w.observe(later, &off_probe, Some(&off), 159), None);
        let retry = w.observe(later, &off_probe, Some(&off), 160).unwrap();
        w.finish(retry, RecoveryResult::Stopped);
        assert_eq!(w.observe(later, &off_probe, Some(&off), 200), None);
    }

    fn single_band() -> Uci {
        config(
            "config device 'zte_mbb'\noption wifi_onoff '1'\noption mesh_onoff '0'\n\
             config wifi-device 'wifi0'\noption disabled '1'\n\
             config wifi-device 'wifi1'\noption disabled '0'\n\
             config wifi-iface 'main_2g'\noption disabled '0'\noption ifname 'wlan0'\noption mode 'ap'\n\
             config wifi-iface 'main_5g'\noption disabled '0'\noption ifname 'wlan2'\noption mode 'ap'\n",
        )
    }

    #[test]
    fn active_cleanup_matches_the_exact_watcher_and_live_interface_set() {
        let mut cfg = single_band();
        let watcher = ["wlan2".into()];
        assert_eq!(active_ifnames(&cfg, 2, &watcher).as_deref(), Some(watcher.as_slice()));
        assert!(active_ifnames(&cfg, 2, &["wlan0".into()]).is_none());
        assert!(active_ifnames(&cfg, 2, &["wlan2".into(), "wlan2".into()]).is_none());
        assert!(active_ifnames(&cfg, 1, &watcher).is_none());
        cfg.remove("wireless.main_5g.ifname");
        assert!(active_ifnames(&cfg, 2, &watcher).is_none());
        let iw = "phy#6\n\tInterface wlan2\n\t\ttype AP\n\tInterface wlan0\n\t\ttype AP\n";
        assert!(only_expected_aps(iw, &watcher, |name| name == "wlan0"));
        assert!(!only_expected_aps(iw, &watcher, |_| false));
        assert!(!only_expected_aps("", &watcher, |_| true));
    }

    #[test]
    fn busy_lock_and_attempted_active_cleanup_are_distinct() {
        let dir = proc_fixture("active");
        let lock_path = dir.join("wifi.lock");
        let sample = |start, cpu| {
            proc_entry(&dir, "5202", b"sh\0/sbin/zte_post_wifi.sh\0", start, cpu);
            scan_proc(&dir).0.unwrap()
        };
        let first = sample(1000, 100);
        let cfg = single_band();
        let mut probe = probe(90, 2, 1);
        probe.clis = vec!["wlan2".into()];
        let mut w = WifiCleanup::default();
        assert_eq!(w.observe(Some(first), &probe, Some(&cfg), 100), None);
        let second = sample(1000, 1600);
        let target = w.observe(Some(second), &probe, Some(&cfg), 130).unwrap();
        assert_eq!(target, Recovery { post: second, kind: RecoveryKind::Active });
        let path = lock_path.to_str().unwrap();
        let busy = crate::ops::wifi_lock::try_take(path).unwrap().unwrap();
        let result = with_wifi_lock(path, || panic!("busy must not run any stock commands"));
        assert_eq!(result, RecoveryResult::Deferred);
        w.finish(target, result);
        drop(busy);
        let third = sample(1000, 3100);
        let next = w.observe(Some(third), &probe, Some(&cfg), 160).unwrap();
        w.finish(next, RecoveryResult::Attempted);
        let fourth = sample(1000, 4600);
        assert_eq!(w.observe(Some(fourth), &probe, Some(&cfg), 190), None);
        assert!(proc_matches(&dir, fourth));
        let reused = sample(1001, 4600);
        assert!(!proc_matches(&dir, fourth));
        assert_eq!(w.observe(Some(reused), &probe, Some(&cfg), 220), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn verified_post_steps_run_in_order_before_signal() {
        let dir = proc_fixture("post");
        std::fs::create_dir(&dir).unwrap();
        let file = dir.join("steps");
        let quote = format!("'{}'", file.display().to_string().replace('\'', "'\\''"));
        let script = format!(
            "handle_macfilter_at_post() {{ echo mac > {quote}; }}\n\
             zte_post_txpower() {{ echo tx >> {quote}; echo ignored; }}\n\
             {POST_BOUNDARY}\nexit 9\n"
        );
        use sha2::{Digest, Sha256};
        let sig = Sha256::digest(script.as_bytes()).into();
        assert!(verified_stock(script.as_bytes(), &sig));
        assert!(!verified_stock(script.as_bytes(), &STOCK_SHA));
        assert!(post_functions("no boundary").is_none());
        assert!(post_functions(&format!("{script}{POST_BOUNDARY}")).is_none());
        let defs = post_functions(&script).unwrap();
        let result = replay_post(defs, Duration::from_secs(2), || {
            assert_eq!(std::fs::read_to_string(&file).unwrap(), "mac\ntx\n");
            true
        });
        assert_eq!(result, RecoveryResult::Stopped);
        for defs in [
            "handle_macfilter_at_post() { sleep 2; }; zte_post_txpower() { :; }",
            "handle_macfilter_at_post() { :; }; zte_post_txpower() { exit 7; }",
            "handle_macfilter_at_post() { exit 0; }; zte_post_txpower() { :; }",
        ] {
            assert_eq!(
                replay_post(defs, Duration::from_millis(100), || panic!("incomplete post steps")),
                RecoveryResult::Attempted,
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stat_fields() {
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
