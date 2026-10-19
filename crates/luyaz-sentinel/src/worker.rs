//! The worker thread (u60-sentinel.md §3, §4): what u60-guard does today that must go on when the
//! core is dead — G1 modem crash recovery, G2 ship recover-live, G3 the clock verdict, G4 the Wi-Fi
//! fallback (+ the RTC alarm), G7 log caps — plus F5 (dropbear) and the core health verdict (§4). One
//! round every 60 s, like the guard; everything that may block (locks, the Wi-Fi restore) is here,
//! never in the screen thread (O3).
//!
//! Each round is a port of the guard function of the same name (touch-ui `scripts/u60-guard.sh`),
//! with the same tunables, file formats and alert kinds; `tests.rs` carries over the guard test
//! cases (`scripts/test/u60-guard/run.sh`). All device access is through [`WorkerIo`], so the tests
//! run on a fake and shadow mode ([`crate::shadow`]) can record instead of act. The deliberate
//! changes (approved in the eng review): D3 — the direct Wi-Fi write takes the write lock before the
//! Wi-Fi lock and never kills a lock holder (`wifi.rs`); a ship transaction of the sentinel itself is
//! the one not finished here (was: the guard's own).
//!
//! Time: everything shared with another program stays in that program's clock base — the scenario
//! heartbeat, ship's heartbeat and `t_phase`, `clock-ok` and the backoff are `/proc/uptime` seconds
//! (counts through suspend), exactly as the guard compares them, sleep detection included (WAKE_GAP).
//! Only this process's own timers (dropbear's hourly limit) use the monotonic clock.

pub use luyaz_alerts::clock::CLOCK_YEAR_MIN;
use luyaz_alerts::clock::{BootTime, ClockEvidence, ClockSource, WALL_STEP_TOLERANCE_S};

pub const INTERVAL_S: u64 = 60;
/// After boot: judge nothing.
pub const GRACE_S: u64 = 300;
/// No heartbeat at all this long after boot = the scenario engine never came up.
pub const NEVER_SEEN_S: u64 = 900;
/// Heartbeat older than this = the scenario engine is gone.
pub const STALE_S: u64 = 300;
/// Rounds this far apart = the device was asleep.
pub const WAKE_GAP_S: u64 = 2 * INTERVAL_S + 30;
/// Restore retries: 60, 120, 240, 480, 480, …
pub const BACKOFF_MAX_S: u64 = 480;
/// Executor heartbeat older than this = dead or stuck.
pub const SHIP_STALE_S: u64 = 30;
/// Staged and never started for this long = the Mac went away.
pub const SHIP_STAGED_S: u64 = 300;
/// The core's `/healthz` may say `starting` this long after boot without counting against it.
pub const CORE_BOOT_S: u64 = 180;
pub const DROPBEAR_PER_HOUR: usize = 3;
pub const LOGCAP_RETRY_S: u64 = 3600;

/// Paths, all as the guard has them (D13 spirit: nothing a reader looks at moves before Phase 10).
#[derive(Clone, Debug)]
pub struct Paths {
    /// This sentinel's own state (tmpfs: a reboot starts clean, as the guard's `$STATE`).
    pub state: String,
    pub heartbeat: String,
    pub marker: String,
    /// The /tmp tree the sentinel alone writes (`/tmp/luyaz`): `clock-ok`, `recovery-set`.
    pub tmp: String,
    /// The duties handed to the core.
    pub handover: String,
    pub wall_last: String,
    pub rtc: String,
    pub mss_recovery: String,
    pub ship: String,
    pub ship_txn: String,
    pub ship_hb: String,
    pub cap_logs: Vec<String>,
    pub cap_bytes: u64,
    pub cap_tmp_logs: Vec<String>,
    pub cap_tmp_bytes: u64,
}

impl Default for Paths {
    fn default() -> Self {
        let s = |x: &str| x.to_string();
        Paths {
            state: s("/tmp/luyaz-sentinel"),
            heartbeat: s("/tmp/scenario.heartbeat"),
            marker: s("/tmp/luyaz/wifiguard.took-over"),
            tmp: s("/tmp/luyaz"),
            handover: s("/tmp/luyaz/handover"),
            wall_last: s("/data/ledger/state/wall.last"),
            rtc: s("/sys/class/rtc/rtc0"),
            mss_recovery: s("/sys/class/remoteproc/remoteproc0/recovery"),
            ship: s("/data/luyaz/scripts/luyaz-ship.sh"),
            ship_txn: s("/data/luyaz/ship/txn"),
            ship_hb: s("/tmp/luyaz/ship/heartbeat"),
            // tailscaled writes ~7.5 MB/day and nothing rotates it; start.sh opens it for append.
            cap_logs: vec![s("/data/tailscaled.log")],
            cap_bytes: 1_048_576,
            // RAM logs trimmed otherwise only when their writer starts (the .old copy is RAM too).
            cap_tmp_logs: vec![
                s("/tmp/luyaz-core.log"),
                s("/tmp/luyaz-ui.log"),
                s("/tmp/luyaz-screen.log"),
                s("/tmp/luyaz-sentinel.log"),
                s("/tmp/luyaz-sentinel.out"),
                s("/tmp/luyaz-web.log"),
            ],
            cap_tmp_bytes: 262_144,
        }
    }
}

/// A cmdline needle both executor names have (`luyaz-ship.sh`, `luyaz-ship.sh`).
pub const SHIP_CMDLINE: &str = "-ship.sh";

/// The core's `/healthz`.
#[derive(Clone, Debug, PartialEq)]
pub enum Health {
    Ok,
    /// 503 `starting` (first collection round not done yet).
    Starting,
    /// Answered, not 200 (503 `stalled`, …).
    Bad(String),
    /// No answer (refused, timeout).
    Down(String),
}

/// Everything the worker reads or changes on the device.
pub trait WorkerIo {
    /// BOOTTIME plus boot identity; unknown must not become a wall-clock fallback.
    fn boot_time(&mut self) -> Option<BootTime>;
    /// Trusted root-owned, non-symlink UTC attestation; adapters must validate file access.
    fn utc_anchor(&mut self) -> Option<String> {
        None
    }
    /// Whole seconds of `/proc/uptime` (0 when unreadable).
    fn uptime(&mut self) -> u64;
    /// The device's wall clock (local time labelled UTC).
    fn wall(&mut self) -> i64;
    /// Monotonic seconds (stops in suspend).
    fn mono_s(&mut self) -> i64;
    fn read(&mut self, path: &str) -> Option<String>;
    fn exists(&mut self, path: &str) -> bool;
    fn size(&mut self, path: &str) -> Option<u64>;
    /// Is any running process's `/proc/<pid>/cmdline` containing `needle`, for this pid?
    fn cmdline_has(&mut self, pid: i32, needle: &str) -> bool;
    /// Every writer of `path` opened it with O_APPEND (the guard's `appenders_only`).
    fn appenders_only(&mut self, path: &str) -> bool;
    fn beaconing(&mut self) -> bool;
    fn healthz(&mut self) -> Health;
    fn dropbear_listening(&mut self) -> bool;

    // ── changes (shadow mode records these instead) ─────────────────────────
    /// Whole-file atomic write (tmp + rename).
    fn write(&mut self, path: &str, content: &str);
    /// [`WorkerIo::write`], synced to flash (a file that must survive a power cut).
    fn persist(&mut self, path: &str, content: &str) {
        self.write(path, content)
    }
    /// In-place write (sysfs). False when it failed.
    fn poke(&mut self, path: &str, content: &str) -> bool;
    fn remove(&mut self, path: &str);
    fn log(&mut self, msg: &str);
    fn alert(&mut self, kind: &str, text: &str);
    /// The whole Wi-Fi-on procedure (`wifi.rs`, D3).
    fn restore_wifi(&mut self) -> Result<(), String>;
    fn set_autosleep(&mut self, on: bool);
    /// Start `luyaz-ship.sh recover-live` in the background unless the previous one we started still
    /// runs. False = not started.
    fn recover_live(&mut self) -> bool;
    /// Copy to `<path>.old`, then truncate. False when it failed.
    fn cap_log(&mut self, path: &str) -> bool;
    fn start_dropbear(&mut self) -> bool;
    /// A core ship transaction with a live executor (D6).
    fn core_being_replaced(&mut self) -> bool;
    /// One alert SMS round as the sender `sentinel` (`u60-alerts::sms`).
    fn alert_sms(&mut self);
    /// `/proc` cannot be read, so no process can be told alive (u60-guard is gone since 10c;
    /// this is all that is left of its check).
    fn proc_blind(&mut self) -> bool;
    /// A normal boot, not power-off charging (u60-guard.init `boot_is_normal`).
    fn boot_is_normal(&mut self) -> bool;
}

/// The guard's handover file (u60d `compat/guard_duties.rs`): the duties the core runs.
/// Once the guard is retired (5f), the core runs all of them: nobody else sends alert SMS while
/// the core is healthy, keeps LAN IPv6 off, writes the kernel log to flash (`crashcap`, G8), the
/// standby records (G10) or the program fingerprints (G11) (u60-sentinel.md §20).
pub const CORE_DUTIES: &str = "sms\nlanv6\ncore\ncrashcap\nstandby\nfp\n";

fn num(s: Option<String>) -> u64 {
    // the guard's `num`: the whole content a plain number, else 0
    s.map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|t| t.parse().ok())
        .unwrap_or(0)
}

fn isint(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// The Wi-Fi watchdog's state (the guard's `$STATE/{seen,episode,…}` files), one file here.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WifiState {
    pub seen: bool,
    pub episode: Option<u64>,
    pub took_over: bool,
    pub next_try: u64,
    pub backoff: u64,
    pub sleep_held: bool,
    pub last_round: u64,
    pub woke: u64,
    pub alerted_silent: bool,
    pub alerted_restore: bool,
}

impl WifiState {
    fn encode(&self) -> String {
        let b = |x: bool| if x { 1 } else { 0 };
        format!(
            "seen={}\nepisode={}\ntook_over={}\nnext_try={}\nbackoff={}\nsleep_held={}\nlast_round={}\nwoke={}\nalerted_silent={}\nalerted_restore={}\n",
            b(self.seen),
            self.episode.map(|e| e.to_string()).unwrap_or_default(),
            b(self.took_over),
            self.next_try,
            self.backoff,
            b(self.sleep_held),
            self.last_round,
            self.woke,
            b(self.alerted_silent),
            b(self.alerted_restore)
        )
    }

    fn decode(text: &str) -> WifiState {
        let mut s = WifiState::default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let n = isint(v);
            let on = v == "1";
            match k {
                "seen" => s.seen = on,
                "episode" => s.episode = n,
                "took_over" => s.took_over = on,
                "next_try" => s.next_try = n.unwrap_or(0),
                "backoff" => s.backoff = n.unwrap_or(0),
                "sleep_held" => s.sleep_held = on,
                "last_round" => s.last_round = n.unwrap_or(0),
                "woke" => s.woke = n.unwrap_or(0),
                "alerted_silent" => s.alerted_silent = on,
                "alerted_restore" => s.alerted_restore = on,
                _ => {}
            }
        }
        s
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClockState {
    pub boot: String,
    /// Initial wall-minus-boot offset, only used to explain a pre-sync forward leap.
    pub first: Option<i64>,
    /// Last accepted estimate. A later step cannot overwrite it, including across restarts.
    pub anchor: Option<i64>,
    pub last: Option<ClockEvidence>,
    pub persisted_at: Option<u64>,
}

impl ClockState {
    fn encode(&self) -> String {
        serde_json::json!({"boot": self.boot, "first": self.first, "anchor": self.anchor,
            "last": self.last, "persisted_at": self.persisted_at})
        .to_string()
    }

    fn decode(text: &str) -> ClockState {
        let v: serde_json::Value = serde_json::from_str(text).unwrap_or_default();
        ClockState {
            boot: v["boot"].as_str().unwrap_or_default().into(),
            first: v["first"].as_i64(),
            anchor: v["anchor"].as_i64(),
            last: serde_json::from_value(v["last"].clone()).ok(),
            persisted_at: v["persisted_at"].as_u64(),
        }
    }
}

/// §4: the core is unhealthy after 2 rounds in a row without a 200 (`starting` in the first
/// [`CORE_BOOT_S`] after boot does not count).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CoreState {
    pub bad_rounds: u32,
    pub unhealthy: bool,
    pub last: Option<String>,
}

#[derive(Debug, Default)]
pub struct Worker {
    pub paths: Paths,
    pub wifi: WifiState,
    pub clock: ClockState,
    pub core: CoreState,
    /// Log path → uptime of the refusal (`$STATE/logcap-skip.<name>`).
    logcap_skip: Vec<(String, u64)>,
    dropbear_missing: u32,
    dropbear_starts: Vec<i64>,
    dropbear_limit_said: bool,
    /// Monotonic seconds when the last round began (the heartbeat's 5th field).
    pub last_round_mono: i64,
    boot_checked: bool,
    charging_only: bool,
}

impl Worker {
    pub fn new(paths: Paths) -> Worker {
        Worker {
            paths,
            ..Default::default()
        }
    }

    /// Pick up where a previous run of the sentinel left off this boot (tmpfs).
    pub fn load(&mut self, io: &mut dyn WorkerIo) {
        if let Some(t) = io.read(&format!("{}/wifi", self.paths.state)) {
            self.wifi = WifiState::decode(&t);
        }
        if let Some(t) = io.read(&format!("{}/clock", self.paths.state)) {
            self.clock = ClockState::decode(&t);
        }
    }

    fn save(&mut self, io: &mut dyn WorkerIo) {
        io.write(&format!("{}/wifi", self.paths.state), &self.wifi.encode());
        io.write(&format!("{}/clock", self.paths.state), &self.clock.encode());
    }

    /// One round, in the guard's order.
    pub fn round(&mut self, io: &mut dyn WorkerIo) {
        self.last_round_mono = io.mono_s();
        // Power-off charging: the device is "off" to its owner. The screen thread still shows the
        // UI's charging screen (u60-uid ran there too), but none of the guard's duties apply — the
        // guard was not started then (u60-guard.init): no Wi-Fi switched on in an "off" device, no
        // SMS because the core is (rightly) not running.
        if !self.boot_checked {
            self.boot_checked = true;
            self.charging_only = !io.boot_is_normal();
            if self.charging_only {
                io.log("power-off charging: the worker stays idle (screen only)");
            }
        }
        if self.charging_only {
            return;
        }
        self.mss_recovery_round(io);
        self.ship_round(io);
        self.clock_round(io);
        self.core_round(io);
        self.guard_round(io);
        self.rtc_round(io);
        self.logcap_round(io);
        self.dropbear_round(io);
        self.handover_round(io);
        self.save(io);
    }

    /// u60-sentinel.md §16, the 5f precondition: u60d runs the guard's SMS and LAN IPv6 duties only
    /// while the handover file (`/tmp/luyaz/handover`) names them.
    /// The guard that used to share it is deleted (10c), so the sentinel always writes it.
    pub fn handover_round(&mut self, io: &mut dyn WorkerIo) {
        let h = self.paths.handover.clone();
        if io.read(&h).as_deref() != Some(CORE_DUTIES) {
            io.write(&h, CORE_DUTIES);
            io.log(
                "no old guard running: the core runs sms, lanv6, core, crashcap, standby and fp",
            );
        }
    }

    // ── G1: modem crash recovery (owner's choice, 2026-09-26) ───────────────
    // The firmware boots with remoteproc recovery disabled, so any modem assert reboots the whole
    // device. Enabled, only the modem restarts (~5 s without data).
    pub fn mss_recovery_round(&mut self, io: &mut dyn WorkerIo) {
        let p = self.paths.mss_recovery.clone();
        let v = io
            .read(&p)
            .map(|s| s.lines().next().unwrap_or("").to_string());
        if v.as_deref() != Some("disabled") {
            return;
        }
        if io.poke(&p, "enabled\n") {
            io.log("modem crash recovery was disabled: enabled it");
            let up = io.uptime();
            io.write(
                &format!("{}/recovery-set", self.paths.tmp),
                &format!("{up}\n"),
            );
        }
    }

    // ── G2: a ship transaction whose executor died or hangs (docs/SHIP.md) ──
    // `luyaz-ship.sh recover-live` finishes it; this only notices. A transaction of the sentinel's own
    // component is not ours to finish: its executor's timeouts, the core (5d) and the boot-time
    // luyaz-recover.sh cover it.
    pub fn ship_round(&mut self, io: &mut dyn WorkerIo) {
        let (ship, ship_txn, ship_hb) = (
            self.paths.ship.clone(),
            self.paths.ship_txn.clone(),
            self.paths.ship_hb.clone(),
        );
        let Some(txn) = io.read(&ship_txn) else {
            return;
        };
        let (mut ph, mut comp, mut tp) = (String::new(), String::new(), String::new());
        for l in txn.lines() {
            if let Some(v) = l.strip_prefix("phase=") {
                ph = v.to_string();
            } else if let Some(v) = l.strip_prefix("comp=") {
                comp = v.to_string();
            } else if let Some(v) = l.strip_prefix("t_phase=") {
                tp = v.to_string();
            }
        }
        if !matches!(
            ph.as_str(),
            "staged" | "trial" | "promote" | "check" | "manifest" | "rollback"
        ) {
            return;
        }
        if comp == "sentinel" || !io.exists(&ship) {
            return;
        }
        // recover-live has no lock: with /proc unreadable a live executor looks dead, so never
        // act blind (u60-sentinel.md §22; the guard that used to finish these is gone since 10c)
        if io.proc_blind() {
            return;
        }
        let now = io.uptime();
        let hb = io.read(&ship_hb).unwrap_or_default();
        let mut f = hb.split_whitespace();
        let hu = f.next().and_then(isint);
        let hp = f.next().and_then(isint);
        if let (Some(hu), Some(hp)) = (hu, hp)
            && now.saturating_sub(hu) <= SHIP_STALE_S
            && io.cmdline_has(hp as i32, SHIP_CMDLINE)
        {
            return;
        }
        if ph == "staged" && hu.is_none() {
            // staged, executor not started yet: the Mac starts it seconds later
            if let Some(t) = isint(&tp)
                && now.saturating_sub(t) <= SHIP_STAGED_S
            {
                return;
            }
        }
        let age = match hu {
            Some(h) => format!("{}s old", now.saturating_sub(h)),
            None => "missing".into(),
        };
        io.log(&format!(
            "u60-ship: transaction ({comp}, {ph}) has no live executor (heartbeat {age}); starting recover-live"
        ));
        io.recover_live();
    }

    // ── G3: clock trust (docs/LEDGER.md §7) ─────────────────────────────────
    // The first line stays compatible; the second carries versioned, boot-bound evidence.
    // Plausibility/continuity is only an estimated local wall clock, never verified UTC/NTP.
    pub fn clock_round(&mut self, io: &mut dyn WorkerIo) {
        let path = format!("{}/clock-ok", self.paths.tmp);
        let Some(at) = io.boot_time() else {
            io.write(&path, "0 0 0 none\n");
            return;
        };
        let w = io.wall();
        let Some(off) = i64::try_from(at.seconds)
            .ok()
            .and_then(|up| w.checked_sub(up))
        else {
            io.write(&path, "0 0 0 none\n");
            return;
        };
        if self.clock.boot != at.boot_id {
            self.clock = ClockState {
                boot: at.boot_id.clone(),
                ..Default::default()
            };
        }
        let first = *self.clock.first.get_or_insert(off);
        let last_wall = num(io.read(&self.paths.wall_last));
        let source = if self
            .clock
            .anchor
            .is_some_and(|a| off.abs_diff(a) > WALL_STEP_TOLERANCE_S)
        {
            ClockSource::Revoked
        } else if w < CLOCK_YEAR_MIN
            || i64::try_from(last_wall)
                .ok()
                .is_none_or(|last| last > 0 && w < last.saturating_sub(86_400))
        {
            ClockSource::None
        } else if self.clock.anchor.is_some() {
            ClockSource::Continuity
        } else if off.saturating_sub(first) > 86_400 {
            ClockSource::Jump
        } else if last_wall > 0 {
            ClockSource::LastWall
        } else {
            ClockSource::Year
        };
        let mono = u64::try_from(io.mono_s()).ok();
        let mut evidence = ClockEvidence::observation(w, at, mono, source);
        // Root-provisioned, bounded one-shot evidence only. No network calls or periodic
        // upstream query. Re-reading cannot refresh the anchor's original source lifetime.
        if let Some(anchor) = io
            .utc_anchor()
            .filter(|s| s.len() <= 4096)
            .and_then(|s| serde_json::from_str::<luyaz_alerts::utc::UtcAnchor>(&s).ok())
        {
            anchor.apply(&mut evidence);
        }
        if evidence
            .status(Some(&evidence.observed_at), w)
            .wall_usable()
        {
            self.clock.anchor = Some(off);
            // Preserve the existing hourly maximum, now also bounded by elapsed time. A wall
            // step cannot cause extra flash writes. This history remains an estimate.
            let up = evidence.observed_at.seconds;
            if self
                .clock
                .persisted_at
                .is_none_or(|t| up.saturating_sub(t) >= 3600)
                && u64::try_from(w)
                    .ok()
                    .is_some_and(|w| w >= last_wall.saturating_add(3600))
            {
                io.persist(&self.paths.wall_last, &format!("{w}\n"));
                self.clock.persisted_at = Some(up);
            }
        }
        io.write(&path, &evidence.encode());
        self.clock.last = Some(evidence);
    }

    // ── §4: is the core healthy ─────────────────────────────────────────────
    pub fn core_round(&mut self, io: &mut dyn WorkerIo) {
        let h = io.healthz();
        let up = io.uptime();
        let (bad, what) = match &h {
            Health::Ok => (false, "200".to_string()),
            Health::Starting => (up >= CORE_BOOT_S, "starting".to_string()),
            Health::Bad(s) => (true, s.clone()),
            Health::Down(s) => (true, format!("no answer: {s}")),
        };
        self.core.bad_rounds = if bad { self.core.bad_rounds + 1 } else { 0 };
        let unhealthy = self.core.bad_rounds >= 2;
        if unhealthy && !self.core.unhealthy {
            io.log(&format!(
                "core unhealthy: /healthz {what} for {} rounds",
                self.core.bad_rounds
            ));
        } else if !unhealthy && self.core.unhealthy {
            io.log(&format!("core healthy again (/healthz {what})"));
        }
        self.core.unhealthy = unhealthy;
        self.core.last = Some(what);
        // §6: the alert SMS while the core cannot send them, except while it is being replaced
        // (D6: its replacement is expected within the trial)
        if unhealthy && !io.core_being_replaced() {
            io.alert_sms();
        }
    }

    // ── G4: the Wi-Fi watchdog ──────────────────────────────────────────────
    fn end_episode(&mut self, io: &mut dyn WorkerIo) {
        io.log("scenario heartbeat is back; outage over");
        // We held sleep off; the engine sits in away until it releases the takeover, and away lets
        // the device sleep — so give that back.
        if self.wifi.sleep_held {
            io.set_autosleep(true);
        }
        let w = &mut self.wifi;
        w.sleep_held = false;
        w.episode = None;
        w.took_over = false;
        w.next_try = 0;
        w.backoff = 0;
        w.alerted_silent = false;
        w.alerted_restore = false;
    }

    fn arm_rtc(&mut self, io: &mut dyn WorkerIo) {
        // Make sure the RTC wakes the device within five minutes so this gets to run. Never pushes an
        // earlier alarm out — the engine arms its own, and whichever is sooner should win.
        let base = num(io.read(&format!("{}/since_epoch", self.paths.rtc)));
        if base == 0 {
            return;
        }
        let want = base + 300;
        let alarm = format!("{}/wakealarm", self.paths.rtc);
        let cur = num(io.read(&alarm));
        if cur <= base || cur > want {
            io.poke(&alarm, "0\n");
            io.poke(&alarm, &format!("{want}\n"));
        }
    }

    pub fn guard_round(&mut self, io: &mut dyn WorkerIo) {
        let now = io.uptime();
        // /proc/uptime counts through suspend, but neither our sleep nor the engine's does. Rounds
        // much further apart than INTERVAL mean we were asleep: the engine then gets a full STALE
        // window before it counts as gone.
        let prev = self.wifi.last_round;
        self.wifi.last_round = now;
        if prev > 0 && now.saturating_sub(prev) > WAKE_GAP_S {
            self.wifi.woke = now;
            io.log(&format!(
                "device was asleep ~{}s; giving the scenario engine {STALE_S}s to check in",
                now - prev
            ));
        }
        if now < GRACE_S {
            return;
        }
        let hb = num(io.read(&self.paths.heartbeat));
        if hb > 0 {
            self.wifi.seen = true;
        }

        let why = if self.wifi.seen {
            if hb > 0 && now.saturating_sub(hb) <= STALE_S {
                if self.wifi.episode.is_some() {
                    self.end_episode(io);
                }
                return;
            }
            // Just woke and no outage already under way: wait, don't judge.
            let woke = self.wifi.woke;
            if self.wifi.episode.is_none() && woke > 0 && now.saturating_sub(woke) <= STALE_S {
                return;
            }
            if hb > 0 {
                format!("no heartbeat for {}s", now.saturating_sub(hb))
            } else {
                "heartbeat file gone".into()
            }
        } else {
            // Never seen one: normal early on; well after boot the engine never started at all.
            if now < NEVER_SEEN_S {
                return;
            }
            "no heartbeat since boot".into()
        };
        if self.wifi.episode.is_none() {
            self.wifi.episode = Some(now);
            io.log(&format!("scenario engine looks gone: {why}"));
        }
        if !self.wifi.alerted_silent {
            self.wifi.alerted_silent = true;
            io.alert("agent-silent", &why);
        }
        if io.beaconing() {
            return; // Wi-Fi is up; nothing to rescue
        }
        // Engine gone and nothing broadcasting. From here the device must not suspend, and Wi-Fi comes
        // back on — over any "AP off" set by hand: with the engine dead there is no telling the two
        // apart, and a phone stranded without network is the worse mistake.
        io.set_autosleep(false);
        self.wifi.sleep_held = true;
        self.arm_rtc(io);
        if !self.wifi.took_over {
            io.write(&self.paths.marker, &format!("{now}\n"));
            self.wifi.took_over = true;
            io.log("taking over Wi-Fi");
            io.alert(
                "wifi-takeover",
                &format!("agent gone ({why}), Wi-Fi was off; turning it on"),
            );
        }
        if now < self.wifi.next_try {
            return;
        }
        match io.restore_wifi() {
            Ok(()) => {
                io.log("Wi-Fi restored");
                self.wifi.next_try = 0;
                self.wifi.backoff = 0;
            }
            Err(e) => {
                let b = if self.wifi.backoff > 0 {
                    self.wifi.backoff
                } else {
                    60
                };
                self.wifi.next_try = io.uptime() + b;
                self.wifi.backoff = (b * 2).min(BACKOFF_MAX_S);
                io.log(&format!("Wi-Fi restore failed ({e}); next try in {b}s"));
                if !self.wifi.alerted_restore {
                    self.wifi.alerted_restore = true;
                    io.alert("wifi-restore-failed", "could not turn Wi-Fi on; retrying");
                }
            }
        }
    }

    /// Keep an eye on the RTC whenever the APs are down, even with the engine alive: if it dies
    /// while the device is suspended, only a wake gets this running again.
    pub fn rtc_round(&mut self, io: &mut dyn WorkerIo) {
        if !io.beaconing() {
            self.arm_rtc(io);
        }
    }

    // ── G7: log caps ────────────────────────────────────────────────────────
    pub fn logcap_round(&mut self, io: &mut dyn WorkerIo) {
        let (a, ab) = (self.paths.cap_logs.clone(), self.paths.cap_bytes);
        let (b, bb) = (self.paths.cap_tmp_logs.clone(), self.paths.cap_tmp_bytes);
        self.logcap_list(io, ab, &a);
        self.logcap_list(io, bb, &b);
    }

    fn logcap_list(&mut self, io: &mut dyn WorkerIo, cap: u64, logs: &[String]) {
        for f in logs {
            let Some(sz) = io.size(f) else { continue };
            if sz <= cap {
                continue;
            }
            // The fd scan is a walk over every open file on the device, so after a refusal it is not
            // repeated for an hour (per log: one refused log does not hold the others back).
            let skip = self
                .logcap_skip
                .iter()
                .find(|(p, _)| p == f)
                .map(|(_, t)| *t)
                .unwrap_or(0);
            let up = io.uptime();
            if skip > 0 && up.saturating_sub(skip) < LOGCAP_RETRY_S {
                continue;
            }
            if !io.appenders_only(f) {
                // truncating under a non-append writer leaves a hole the size of the old log
                if skip == 0 {
                    io.log(&format!(
                        "not capping {f}: a writer did not open it for append"
                    ));
                }
                self.logcap_skip.retain(|(p, _)| p != f);
                self.logcap_skip.push((f.clone(), up.max(1)));
                continue;
            }
            self.logcap_skip.retain(|(p, _)| p != f);
            if io.cap_log(f) {
                io.log(&format!(
                    "capped {f} at {sz} bytes (previous copy in {f}.old)"
                ));
            }
        }
    }

    // ── F5: dropbear (10-09 audit; the only way in from abroad) ─────────────
    // Judged by a LISTEN socket on :2222, not by a process name: a session child of a dead listener
    // would still be called "dropbear". Only starts it (the kit's start_dropbear.sh); never changes
    // its configuration.
    pub fn dropbear_round(&mut self, io: &mut dyn WorkerIo) {
        if io.uptime() < GRACE_S {
            return;
        }
        if io.dropbear_listening() {
            self.dropbear_missing = 0;
            self.dropbear_limit_said = false;
            return;
        }
        self.dropbear_missing += 1;
        if self.dropbear_missing < 2 {
            return;
        }
        let now = io.mono_s();
        self.dropbear_starts.retain(|t| now - t < 3600);
        if self.dropbear_starts.len() >= DROPBEAR_PER_HOUR {
            if !self.dropbear_limit_said {
                self.dropbear_limit_said = true;
                io.log("dropbear still not listening on :2222; already started 3 times this hour, waiting");
                io.alert(
                    "dropbear-down",
                    "SSH (:2222) keeps going away; restart limit reached",
                );
            }
            return;
        }
        self.dropbear_starts.push(now);
        self.dropbear_missing = 0;
        let ok = io.start_dropbear();
        io.log(&format!(
            "dropbear not listening on :2222 for 2 rounds: started it again{}",
            if ok { "" } else { " (start script failed)" }
        ));
        io.alert(
            "dropbear-restarted",
            "SSH (:2222) was not listening; started it again",
        );
    }
}
