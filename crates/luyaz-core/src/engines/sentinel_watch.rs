//! The core watching the sentinel and the screen (u60-sentinel.md §4, §7; T5, D4, O7).
//!
//! Two rules, kept apart (never "either"):
//! 1. **Heartbeat stale**: `/tmp/luyaz-sentinel.hb` (`mono pid state launches worker_mono`, the
//!    sentinel's CLOCK_MONOTONIC — the same clock as ours, so suspend counts for neither side) not
//!    advanced for more than [`HB_STALE_S`] → alert `sentinel-silent` once per episode. Nothing
//!    else: a stuck sentinel next to a live touch UI must not get a second screen owner (9-17 →
//!    1185). No heartbeat file at all = the sentinel is not in charge (shadow mode, or before 5f):
//!    not judged.
//! 2. **Empty screen**: no process whose comm starts with `luyaz-ui` and no `zte_topsw_devui`,
//!    for [`EMPTY_S`] → `/etc/init.d/zte_topsw_devui start`, alert `screen-empty`; at most once per
//!    [`EMPTY_S`]. The core only ever starts the vendor UI, never ours (one supervisor).
//!
//! Both yield while a ship transaction of `touch` or `sentinel` has a live executor (it judges an
//! empty screen itself and rolls back).
//!
//! As a trial judge (§7): while a `sentinel` or `touch` transaction is in `trial`/`check`, write
//! `/data/luyaz/ship/verdict` (`<txn> <ok|fail|pending> <reason>`); the executor ANDs it with its own
//! checks (D5). Sentinel ok = the heartbeat advanced at least twice since the transaction's phase
//! began and is at most [`HB_OK_S`] old; fail = stale (rule 1's line). Touch fail = the screen
//! empty for [`TOUCH_EMPTY_S`]; ok = our UI on screen and never empty that long.
//!
//! **recover-live for the sentinel's own transactions** (§7 item 3, G2's core half): a `sentinel`
//! transaction left in an unfinished phase with no live executor (heartbeat > [`EXEC_DEAD_S`] old or
//! its pid gone — the script's own `HB_STALE`; `staged` gets [`STAGED_GRACE_S`] for the Mac to start
//! it) → `luyaz-ship.sh recover-live`, one at a time. Never with `/proc` unreadable: a live executor
//! would look dead, and recover-live has no lock, so that would be two rollbacks at once (the
//! an old watchdog this used to wait for is gone since 10c). The sentinel finishes the
//! others and skips its own (it may be the one being replaced).
//!
//! Runs while u60d's writes are on, every [`LOOK_SECS`]. Rule 2 only logs what it would do unless
//! [`SCREEN_GUARD_ON`] exists: starting a UI next to one we failed to see would be two DRM owners
//! (1185), and an empty screen cannot be staged on the device to try it, so it earns its switch
//! with a record of no false calls first (the owner turns it on).

use std::process::Command;
use std::time::Duration;

use crate::api::Compat;

/// A full /proc scan each look: 30 s is fine for a 60 s rule (5e: share the wake ledger's sample).
const LOOK_SECS: u64 = 30;
pub const HB_FILE: &str = "/tmp/luyaz-sentinel.hb";
pub const SCREEN_GUARD_ON: &str = "/data/luyaz/core/screen-guard-on";
pub const VERDICT_FILE: &str = "/data/luyaz/ship/verdict";
const TXN_FILE: &str = "/data/luyaz/ship/txn";
/// The executor's heartbeat `<uptime> <pid> <txn> <phase>` (touch-ui docs/SHIP.md; tmpfs).
const SHIP_HB: &str = "/tmp/luyaz/ship/heartbeat";
const VENDOR_INIT: &str = "/etc/init.d/zte_topsw_devui";
pub const HB_STALE_S: i64 = 150;
pub const HB_OK_S: i64 = 75;
pub const EMPTY_S: i64 = 60;
pub const TOUCH_EMPTY_S: i64 = 20;
const SHIP_STALE_S: u64 = 90;
/// recover-live: executor heartbeat older than this = dead (luyaz-ship.sh `HB_STALE`).
pub const EXEC_DEAD_S: u64 = 30;
/// recover-live: a `staged` transaction the executor has not started after this long.
pub const STAGED_GRACE_S: u64 = 300;
/// recover-live asked again for the same transaction at most this often.
pub const RECOVER_EVERY_S: i64 = 120;
const SHIP_SCRIPT: &str = "/data/luyaz/scripts/luyaz-ship.sh";
const RECOVER_LOG: &str = "/tmp/luyaz/ship/recover-live.core.log";
const ACTIVE: [&str; 6] = [
    "staged", "trial", "promote", "check", "manifest", "rollback",
];

/// One look at the device.
#[derive(Clone, Debug, Default)]
pub struct Obs {
    /// Our CLOCK_MONOTONIC, seconds.
    pub mono: i64,
    /// The heartbeat's monotonic field, if the file exists and parses.
    pub hb: Option<i64>,
    pub ours: bool,
    pub vendor: bool,
    /// A live transaction (executor alive): (txn id, comp, phase, t_phase as uptime, our uptime).
    pub txn: Option<Txn>,
    /// A `sentinel` transaction in an unfinished phase whose executor is not live by the 30 s rule,
    /// with what recover-live needs to know; `None` when there is none.
    pub orphan: Option<Orphan>,
}

/// The facts about a sentinel transaction that may need recover-live.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Orphan {
    pub id: String,
    pub phase: String,
    /// Our uptime and the transaction's `t_phase` (same clock: /proc/uptime, whole seconds).
    pub uptime: u64,
    pub t_phase: Option<u64>,
    /// The executor heartbeat's uptime field, if there is one.
    pub hb: Option<u64>,
    pub script: bool,
    /// `/proc` could not be read: no executor can be told alive.
    pub blind: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Txn {
    pub id: String,
    pub comp: String,
    pub phase: String,
}

#[derive(Debug, PartialEq)]
pub enum Act {
    Alert(&'static str, String),
    StartVendor,
    Verdict(String),
    Log(String),
    /// Start `luyaz-ship.sh recover-live` (the line says why).
    RecoverLive(String),
}

#[derive(Default)]
pub struct Watch {
    silent: bool,
    empty_since: Option<i64>,
    last_vendor_start: Option<i64>,
    /// The transaction being judged: (id, heartbeat values seen, longest empty, our UI seen).
    judging: Option<Judging>,
    last_verdict: String,
    /// The transaction recover-live was last asked for, and when (mono).
    recover_asked: Option<(String, i64)>,
}

struct Judging {
    id: String,
    hb_seen: Vec<i64>,
    empty_from: Option<i64>,
    longest_empty: i64,
}

impl Watch {
    pub fn step(&mut self, o: &Obs) -> Vec<Act> {
        let mut acts = Vec::new();
        let yield_ship = o
            .txn
            .as_ref()
            .is_some_and(|t| matches!(t.comp.as_str(), "touch" | "sentinel"));

        // rule 1
        if let Some(hb) = o.hb {
            let age = o.mono - hb;
            if age > HB_STALE_S && !self.silent && !yield_ship {
                self.silent = true;
                acts.push(Act::Alert(
                    "sentinel-silent",
                    format!("luyaz-sentinel heartbeat {age}s old"),
                ));
            } else if age <= HB_STALE_S && self.silent {
                self.silent = false;
                acts.push(Act::Log("sentinel heartbeat is back".into()));
            }
        }

        // rule 2
        let empty = !o.ours && !o.vendor;
        if empty {
            let since = *self.empty_since.get_or_insert(o.mono);
            let cooled = self.last_vendor_start.is_none_or(|t| o.mono - t >= EMPTY_S);
            if !yield_ship && o.mono - since >= EMPTY_S && cooled {
                self.last_vendor_start = Some(o.mono);
                acts.push(Act::StartVendor);
                acts.push(Act::Alert(
                    "screen-empty",
                    format!("no UI on screen for {}s", o.mono - since),
                ));
            }
        } else {
            self.empty_since = None;
        }

        // the trial judge
        match &o.txn {
            Some(t)
                if matches!(t.comp.as_str(), "touch" | "sentinel")
                    && matches!(t.phase.as_str(), "trial" | "check") =>
            {
                if self.judging.as_ref().is_none_or(|j| j.id != t.id) {
                    self.judging = Some(Judging {
                        id: t.id.clone(),
                        hb_seen: vec![],
                        empty_from: None,
                        longest_empty: 0,
                    });
                }
                let j = self.judging.as_mut().expect("set above");
                if let Some(hb) = o.hb
                    && j.hb_seen.last() != Some(&hb)
                {
                    j.hb_seen.push(hb);
                }
                if empty {
                    let f = *j.empty_from.get_or_insert(o.mono);
                    j.longest_empty = j.longest_empty.max(o.mono - f);
                } else {
                    j.empty_from = None;
                }
                let (word, why) = if t.comp == "sentinel" {
                    let age = o.hb.map(|h| o.mono - h);
                    match age {
                        Some(a) if a > HB_STALE_S => ("fail", format!("heartbeat {a}s old")),
                        Some(a) if j.hb_seen.len() >= 3 && a <= HB_OK_S => (
                            "ok",
                            format!("heartbeat advanced {} times, {a}s old", j.hb_seen.len() - 1),
                        ),
                        None => ("pending", "no heartbeat yet".into()),
                        Some(a) => (
                            "pending",
                            format!(
                                "heartbeat advanced {} times, {a}s old",
                                j.hb_seen.len().saturating_sub(1)
                            ),
                        ),
                    }
                } else if j.longest_empty >= TOUCH_EMPTY_S {
                    ("fail", format!("screen empty for {}s", j.longest_empty))
                } else if o.ours {
                    ("ok", "our UI on screen".into())
                } else {
                    ("pending", "our UI not on screen yet".into())
                };
                let line = format!("{} {word} {why}", t.id);
                if line != self.last_verdict {
                    self.last_verdict = line.clone();
                    acts.push(Act::Verdict(line));
                }
            }
            _ => self.judging = None,
        }

        // recover-live for the sentinel's own transactions
        if let Some(o2) = &o.orphan
            && let Some(why) = orphan_why(o2)
        {
            let again = self
                .recover_asked
                .as_ref()
                .is_none_or(|(id, t)| *id != o2.id || o.mono - t >= RECOVER_EVERY_S);
            if again {
                self.recover_asked = Some((o2.id.clone(), o.mono));
                acts.push(Act::RecoverLive(why));
            }
        }
        acts
    }
}

/// Why recover-live is due for this sentinel transaction, or `None` when it is not (yet).
pub fn orphan_why(o: &Orphan) -> Option<String> {
    if !o.script || o.blind {
        return None;
    }
    // staged, executor not started yet: the Mac starts it seconds later (no t_phase: not waited
    // for, as in the guard and the sentinel)
    if o.phase == "staged"
        && o.hb.is_none()
        && o.t_phase
            .is_some_and(|t| o.uptime.saturating_sub(t) <= STAGED_GRACE_S)
    {
        return None;
    }
    let age = match o.hb {
        Some(h) => format!("{}s old", o.uptime.saturating_sub(h)),
        None => "missing".into(),
    };
    Some(format!(
        "u60-ship: transaction (sentinel, {}) has no live executor (heartbeat {age}); starting recover-live",
        o.phase
    ))
}

pub fn parse_hb(text: &str) -> Option<i64> {
    text.split_whitespace().next()?.parse().ok()
}

/// The transaction file and the executor heartbeat, read once: the live transaction (heartbeat
/// ≤ 90 s and its pid running luyaz-ship.sh), and a sentinel transaction that is not live by the 30 s
/// rule (recover-live).
fn ship_state(c: &Compat, uptime: u64) -> (Option<Txn>, Option<Orphan>) {
    let Ok(txn) = std::fs::read_to_string(c.path(TXN_FILE)) else {
        return (None, None);
    };
    let mut t = Txn::default();
    let mut t_phase = None;
    for l in txn.lines() {
        if let Some(v) = l.strip_prefix("txn=") {
            t.id = v.into();
        } else if let Some(v) = l.strip_prefix("comp=") {
            t.comp = v.into();
        } else if let Some(v) = l.strip_prefix("phase=") {
            t.phase = v.into();
        } else if let Some(v) = l.strip_prefix("t_phase=") {
            t_phase = v.parse().ok();
        }
    }
    if !ACTIVE.contains(&t.phase.as_str()) {
        return (None, None);
    }
    let hb = std::fs::read_to_string(c.path(SHIP_HB)).unwrap_or_default();
    let mut f = hb.split_whitespace();
    let hu: Option<u64> = f.next().and_then(|v| v.parse().ok());
    let hp: Option<u32> = f.next().and_then(|v| v.parse().ok());
    let alive = hp.is_some_and(|p| ship_running(c, p));
    let age = hu.map(|h| uptime.saturating_sub(h));
    let live = (alive && age.is_some_and(|a| a <= SHIP_STALE_S)).then(|| t.clone());
    let dead = !(alive && age.is_some_and(|a| a <= EXEC_DEAD_S));
    let orphan = (t.comp == "sentinel" && dead).then(|| Orphan {
        id: t.id.clone(),
        phase: t.phase.clone(),
        uptime,
        t_phase,
        hb: hu,
        script: c.path(SHIP_SCRIPT).exists(),
        blind: std::fs::read_dir(c.path("/proc")).is_err(),
    });
    (live, orphan)
}

/// `pid` runs the ship executor (either name, Luya-Z rename D4).
fn ship_running(c: &Compat, pid: u32) -> bool {
    std::fs::read(c.path(&format!("/proc/{pid}/cmdline")))
        .is_ok_and(|b| crate::paths::is_ship_cmdline(&String::from_utf8_lossy(&b)))
}

/// Which UIs run: (ours = comm starts with luyaz-ui ([`crate::paths::is_ui`]),
/// vendor = comm zte_topsw_devui).
///
/// Our UI's pid is remembered (per root): while that pid's comm is still ours, one small read
/// answers and `vendor` is reported as not seen (no rule needs it while ours runs: the screen is
/// not empty, and the touch verdict looks at ours). Gone or reused → the whole /proc again.
fn uis(c: &Compat) -> (bool, bool) {
    static OURS: std::sync::Mutex<Option<(std::path::PathBuf, String)>> =
        std::sync::Mutex::new(None);
    let ours_comm = |dir: &std::path::Path| {
        std::fs::read_to_string(dir.join("comm")).is_ok_and(|t| crate::paths::is_ui(t.trim_end()))
    };
    let proc = c.path("/proc");
    let mut cache = OURS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((root, pid)) = cache.as_ref()
        && *root == proc
        && ours_comm(&proc.join(pid))
    {
        return (true, false);
    }
    *cache = None;
    let (mut ours, mut vendor) = (false, false);
    let Ok(rd) = std::fs::read_dir(&proc) else {
        return (true, true); // cannot see: assume fine, never act blind
    };
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(n) = name.to_str() else { continue };
        if !n.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let comm = std::fs::read_to_string(e.path().join("comm")).unwrap_or_default();
        let comm = comm.trim_end();
        if crate::paths::is_ui(comm) {
            ours = true;
            *cache = Some((proc.clone(), n.to_string()));
        }
        vendor |= comm == "zte_topsw_devui";
    }
    (ours, vendor)
}

pub(crate) fn mono_s() -> i64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime into a valid timespec.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec
}

fn uptime(c: &Compat) -> u64 {
    std::fs::read_to_string(c.path("/proc/uptime"))
        .ok()
        .and_then(|s| s.split('.').next()?.trim().parse().ok())
        .unwrap_or(0)
}

pub fn observe(c: &Compat, mono: i64) -> Obs {
    let (ours, vendor) = uis(c);
    let (txn, orphan) = ship_state(c, uptime(c));
    Obs {
        mono,
        hb: std::fs::read_to_string(c.path(HB_FILE))
            .ok()
            .and_then(|t| parse_hb(&t)),
        ours,
        vendor,
        txn,
        orphan,
    }
}

pub async fn run(c: Compat) {
    let mut look = tokio::time::interval(c.clock.secs(LOOK_SECS));
    let mut w = Watch::default();
    loop {
        look.tick().await;
        let c2 = c.clone();
        let acts = tokio::task::spawn_blocking(move || {
            let o = observe(&c2, mono_s());
            w.step(&o).into_iter().for_each(|a| act(&c2, a));
            w
        })
        .await;
        match acts {
            Ok(back) => w = back,
            Err(_) => w = Watch::default(),
        }
    }
}

fn act(c: &Compat, a: Act) {
    match a {
        Act::Log(m) => eprintln!("[compat] sentinel watch: {m}"),
        Act::Alert(kind, text) => {
            eprintln!("[compat] sentinel watch: alert {kind}: {text}");
            if kind == "screen-empty" && !c.path(SCREEN_GUARD_ON).exists() {
                return; // record-only: the log line is the evidence
            }
            let al = luyaz_api::alerts::Alerts::at(&c.alerts_dir());
            if let Err(e) = al.add(kind, &text, luyaz_api::alerts::now(), uptime(c)) {
                eprintln!("[compat] sentinel watch: could not raise {kind}: {e}");
            }
        }
        Act::StartVendor => {
            if !c.path(SCREEN_GUARD_ON).exists() {
                eprintln!(
                    "[compat] sentinel watch: empty screen: would start the stock UI (off: no {SCREEN_GUARD_ON})"
                );
                return;
            }
            let init = c.path(VENDOR_INIT);
            let r = run_limited(Command::new(&init).arg("start"));
            eprintln!(
                "[compat] sentinel watch: empty screen: {} start: {r}",
                init.display()
            );
        }
        Act::RecoverLive(why) => {
            eprintln!("[compat] sentinel watch: {why}");
            recover_live(c);
        }
        Act::Verdict(line) => {
            let p = c.path(VERDICT_FILE);
            let tmp = p.with_extension("tmp");
            let r =
                std::fs::write(&tmp, format!("{line}\n")).and_then(|_| std::fs::rename(&tmp, &p));
            eprintln!(
                "[compat] sentinel watch: verdict {line}{}",
                r.err()
                    .map(|e| format!(" (not written: {e})"))
                    .unwrap_or_default()
            );
        }
    }
}

/// The recover-live we started and when, while it runs: one at a time.
static RECOVER: std::sync::Mutex<luyaz_wifi::Slot> = std::sync::Mutex::new(None);

/// A recover-live still running after this is stuck: its session is killed and the slot is free
/// again (a rollback takes minutes, not this).
const RECOVER_CAP: Duration = Duration::from_secs(15 * 60);

/// `sh luyaz-ship.sh recover-live` in its own session (stopping u60d must not stop a rollback),
/// output to [`RECOVER_LOG`]; the executor's own log has the steps.
fn recover_live(c: &Compat) {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    let mut slot = RECOVER.lock().unwrap_or_else(|e| e.into_inner());
    match luyaz_wifi::check_slot(&mut slot, RECOVER_CAP) {
        luyaz_wifi::SlotState::Running(pid) => {
            eprintln!("[compat] sentinel watch: recover-live still running (pid {pid})");
            return;
        }
        luyaz_wifi::SlotState::Killed(pid) => eprintln!(
            "[compat] sentinel watch: recover-live (pid {pid}) still running after {} s: killed",
            RECOVER_CAP.as_secs()
        ),
        luyaz_wifi::SlotState::Ended(Some(st)) => {
            eprintln!("[compat] sentinel watch: last recover-live ended: {st}")
        }
        luyaz_wifi::SlotState::Ended(None) | luyaz_wifi::SlotState::Free => {}
    }
    let log = c.path(RECOVER_LOG);
    if let Some(d) = log.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log);
    let mut cmd = Command::new("sh");
    cmd.arg(c.path(SHIP_SCRIPT))
        .arg("recover-live")
        .stdin(Stdio::null());
    match out.and_then(|f| Ok((f.try_clone()?, f))) {
        Ok((a, b)) => {
            cmd.stdout(a).stderr(b);
        }
        Err(_) => {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }
    // SAFETY: setsid is async-signal-safe and allocates nothing.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    match cmd.spawn() {
        Ok(ch) => {
            eprintln!(
                "[compat] sentinel watch: recover-live started (pid {})",
                ch.id()
            );
            *slot = Some((ch, std::time::Instant::now()));
        }
        Err(e) => eprintln!("[compat] sentinel watch: cannot start recover-live: {e}"),
    }
}

/// Run with a 30 s limit; "ok" or the error.
fn run_limited(cmd: &mut Command) -> String {
    use std::process::Stdio;
    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return e.to_string(),
    };
    for _ in 0..300 {
        match child.try_wait() {
            Ok(Some(s)) if s.success() => return "ok".into(),
            Ok(Some(s)) => return s.to_string(),
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return e.to_string(),
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    "timed out".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(mono: i64, hb: Option<i64>, ours: bool, vendor: bool) -> Obs {
        Obs {
            mono,
            hb,
            ours,
            vendor,
            txn: None,
            orphan: None,
        }
    }
    fn kinds(a: &[Act]) -> Vec<String> {
        a.iter()
            .map(|a| match a {
                Act::Alert(k, _) => k.to_string(),
                Act::StartVendor => "start-vendor".into(),
                Act::Verdict(v) => format!("verdict {}", v.split(' ').nth(1).unwrap()),
                Act::Log(_) => "log".into(),
                Act::RecoverLive(_) => "recover-live".into(),
            })
            .collect()
    }

    #[test]
    fn stuck_sentinel_with_live_ui_only_alerts() {
        let mut w = Watch::default();
        assert!(w.step(&obs(1000, Some(950), true, false)).is_empty());
        assert_eq!(
            kinds(&w.step(&obs(1101, Some(950), true, false))),
            ["sentinel-silent"]
        );
        assert!(
            w.step(&obs(1200, Some(950), true, false)).is_empty(),
            "once per episode"
        );
        assert_eq!(kinds(&w.step(&obs(1210, Some(1205), true, false))), ["log"]);
    }

    #[test]
    fn no_heartbeat_file_is_not_judged() {
        let mut w = Watch::default();
        assert!(w.step(&obs(99_999, None, true, false)).is_empty());
    }

    #[test]
    fn suspend_does_not_count() {
        // both sides on CLOCK_MONOTONIC: a 2-hour suspend moves neither
        let mut w = Watch::default();
        assert!(w.step(&obs(500, Some(460), true, false)).is_empty());
        assert!(w.step(&obs(505, Some(460), true, false)).is_empty());
    }

    #[test]
    fn empty_screen_60_s_starts_the_vendor_ui_once_a_minute() {
        let mut w = Watch::default();
        assert!(w.step(&obs(0, None, false, false)).is_empty());
        assert!(w.step(&obs(59, None, false, false)).is_empty());
        assert_eq!(
            kinds(&w.step(&obs(60, None, false, false))),
            ["start-vendor", "screen-empty"]
        );
        assert!(w.step(&obs(100, None, false, false)).is_empty());
        assert_eq!(
            kinds(&w.step(&obs(120, None, false, false)))[0],
            "start-vendor"
        );
        // a UI back resets
        assert!(w.step(&obs(125, None, false, true)).is_empty());
        assert!(w.step(&obs(150, None, false, false)).is_empty());
    }

    #[test]
    fn a_ship_of_touch_or_sentinel_with_a_live_executor_yields() {
        let mut w = Watch::default();
        let t = |phase: &str| {
            Some(Txn {
                id: "x".into(),
                comp: "touch".into(),
                phase: phase.into(),
            })
        };
        let mut o = obs(0, Some(-500), false, false);
        o.txn = t("promote");
        assert!(w.step(&o).is_empty());
        o.mono = 100;
        assert!(w.step(&o).is_empty(), "no vendor start, no sentinel-silent");
        o.txn = None;
        o.mono = 105;
        assert_eq!(
            kinds(&w.step(&o)),
            ["sentinel-silent", "start-vendor", "screen-empty"]
        );
    }

    #[test]
    fn sentinel_trial_verdict() {
        let mut w = Watch::default();
        let txn = Some(Txn {
            id: "t1".into(),
            comp: "sentinel".into(),
            phase: "check".into(),
        });
        let mut o = obs(1000, Some(990), true, false);
        o.txn = txn.clone();
        assert_eq!(kinds(&w.step(&o)), ["verdict pending"]);
        o.mono = 1060;
        o.hb = Some(1050);
        assert!(
            w.step(&o)
                .iter()
                .all(|a| matches!(a, Act::Verdict(v) if v.contains("pending")))
        );
        o.mono = 1120;
        o.hb = Some(1110);
        let a = w.step(&o);
        assert_eq!(kinds(&a), ["verdict ok"]);
        // stale → fail
        o.mono = 1300;
        assert_eq!(kinds(&w.step(&o)), ["verdict fail"]);
    }

    #[test]
    fn touch_trial_verdict() {
        let mut w = Watch::default();
        let mut o = obs(0, None, false, false);
        o.txn = Some(Txn {
            id: "t2".into(),
            comp: "touch".into(),
            phase: "trial".into(),
        });
        assert_eq!(kinds(&w.step(&o)), ["verdict pending"]);
        o.mono = 5;
        o.ours = true;
        assert_eq!(kinds(&w.step(&o)), ["verdict ok"]);
        o.ours = false;
        o.mono = 10;
        w.step(&o);
        o.mono = 30;
        assert_eq!(kinds(&w.step(&o)), ["verdict fail"]);
        o.ours = true;
        o.mono = 35;
        assert!(
            w.step(&o).is_empty(),
            "a fail stays a fail for this transaction"
        );
    }

    #[test]
    fn heartbeat_line() {
        assert_eq!(parse_hb("94162 32044 ours 1 94100\n"), Some(94162));
        assert_eq!(parse_hb(""), None);
    }

    fn orphan(phase: &str, hb: Option<u64>) -> Orphan {
        Orphan {
            id: "20261010-120000-sentinel".into(),
            phase: phase.into(),
            uptime: 2000,
            t_phase: Some(1900),
            hb,
            script: true,
            blind: false,
        }
    }

    #[test]
    fn recover_live_for_a_sentinel_transaction_with_no_executor() {
        let o = orphan("check", Some(1969));
        assert!(
            orphan_why(&o)
                .unwrap()
                .contains("(sentinel, check) has no live executor (heartbeat 31s old)")
        );
        assert!(
            orphan_why(&orphan("promote", None))
                .unwrap()
                .contains("heartbeat missing")
        );
        assert_eq!(
            orphan_why(&Orphan {
                blind: true,
                ..o.clone()
            }),
            None,
            "/proc unreadable: never recovered blind (no lock in recover-live)"
        );
        assert_eq!(
            orphan_why(&Orphan {
                script: false,
                ..o.clone()
            }),
            None,
            "no luyaz-ship.sh: nothing to call"
        );
        assert_eq!(
            orphan_why(&orphan("staged", None)),
            None,
            "staged 100 s ago, not started yet"
        );
        assert!(
            orphan_why(&Orphan {
                t_phase: Some(1600),
                ..orphan("staged", None)
            })
            .is_some(),
            "staged 400 s ago and never started"
        );
        assert!(
            orphan_why(&Orphan {
                t_phase: None,
                ..orphan("staged", None)
            })
            .is_some(),
            "staged with no t_phase: not waited for (guard, sentinel)"
        );
    }

    #[test]
    fn recover_live_asked_once_then_every_two_minutes() {
        let mut w = Watch::default();
        let mut o = obs(1000, None, true, false);
        o.orphan = Some(orphan("check", Some(1500)));
        assert_eq!(kinds(&w.step(&o)), ["recover-live"]);
        o.mono = 1030;
        assert!(w.step(&o).is_empty(), "the next look: not again");
        o.mono = 1120;
        assert_eq!(
            kinds(&w.step(&o)),
            ["recover-live"],
            "still stuck after 2 min: again"
        );
        o.orphan = Some(Orphan {
            id: "other".into(),
            ..orphan("check", Some(1500))
        });
        o.mono = 1121;
        assert_eq!(
            kinds(&w.step(&o)),
            ["recover-live"],
            "another transaction: at once"
        );
    }

    /// The real spawn: `sh <root>/data/luyaz/scripts/luyaz-ship.sh recover-live`, one at a time, in its
    /// own session, output in the log.
    #[test]
    fn recover_live_runs_the_script_once_at_a_time() {
        use crate::api::{Compat, Sources};
        use std::fs;
        let root = std::env::temp_dir().join(format!("u60d-recover-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("data/luyaz/scripts")).unwrap();
        let mark = root.join("ran");
        fs::write(
            root.join("data/luyaz/scripts/luyaz-ship.sh"),
            format!(
                "echo \"$1 sid=$(cut -d' ' -f6 /proc/self/stat) pid=$$\" >> {m}; echo said; sleep 1\n",
                m = mark.display()
            ),
        )
        .unwrap();
        let c = Compat::new(root.to_str().unwrap(), Sources::none());
        let lines = || {
            fs::read_to_string(&mark)
                .map(|t| t.lines().count())
                .unwrap_or(0)
        };
        let until = |f: &dyn Fn() -> bool| {
            for _ in 0..400 {
                if f() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            panic!("timed out");
        };
        recover_live(&c);
        recover_live(&c);
        until(&|| lines() >= 1);
        let ended = || {
            RECOVER
                .lock()
                .unwrap()
                .as_mut()
                .is_none_or(|(ch, _)| matches!(ch.try_wait(), Ok(Some(_))))
        };
        until(&ended);
        let ran = fs::read_to_string(&mark).unwrap();
        assert_eq!(ran.lines().count(), 1, "one at a time: {ran}");
        let l = ran.lines().next().unwrap();
        assert!(l.starts_with("recover-live "), "{l}");
        #[cfg(target_os = "linux")]
        {
            let pid = l.rsplit("pid=").next().unwrap();
            let sid = l.split("sid=").nth(1).unwrap().split(' ').next().unwrap();
            assert_eq!(sid, pid, "its own session");
        }
        let log = fs::read_to_string(root.join("tmp/luyaz/ship/recover-live.core.log")).unwrap();
        assert!(log.contains("said"));
        recover_live(&c); // the first has ended: reaped, a new one may start
        until(&|| lines() == 2);
        let _ = fs::remove_dir_all(&root);
    }

    /// Real files under a root, at the paths the executor really writes (its heartbeat is on
    /// tmpfs: `/tmp/luyaz/ship/heartbeat`, touch-ui docs/SHIP.md).
    #[test]
    fn observe_reads_the_executors_files() {
        use crate::api::{Compat, Sources};
        use std::fs;
        let root = std::env::temp_dir().join(format!("u60d-swatch-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for d in ["tmp/luyaz/ship", "data/luyaz/ship", "proc/4711", "proc/12"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        fs::write(root.join("proc/uptime"), "2010.55 100.00\n").unwrap();
        fs::write(root.join("proc/12/comm"), "luyaz-ui\n").unwrap();
        fs::write(
            root.join("data/luyaz/ship/txn"),
            "v=1\ntxn=20261010-120000-touch\ncomp=touch\nphase=trial\nt_phase=1990\nend=1\n",
        )
        .unwrap();
        fs::write(
            root.join("tmp/luyaz/ship/heartbeat"),
            "2000 4711 20261010-120000-touch trial\n",
        )
        .unwrap();
        fs::write(
            root.join("proc/4711/cmdline"),
            "sh\0/data/luyaz/scripts/luyaz-ship.sh\0run\0",
        )
        .unwrap();
        let c = Compat::new(root.to_str().unwrap(), Sources::none());
        let o = observe(&c, 500);
        assert!(o.ours && !o.vendor);
        // our UI remembered: another process appearing is not looked for while ours runs …
        fs::create_dir_all(root.join("proc/13")).unwrap();
        fs::write(root.join("proc/13/comm"), "zte_topsw_devui\n").unwrap();
        assert_eq!(uis(&c), (true, false));
        // … its pid reused by something else: the whole /proc again
        fs::write(root.join("proc/12/comm"), "sh\n").unwrap();
        assert_eq!(uis(&c), (false, true));
        fs::remove_dir_all(root.join("proc/13")).unwrap();
        assert_eq!(uis(&c), (false, false), "nothing on screen");
        fs::write(root.join("proc/12/comm"), "luyaz-ui\n").unwrap();
        assert_eq!(uis(&c), (true, false), "back: found by the scan");
        // 10h R5: the new name is ours too (scan, then the remembered pid)
        fs::write(root.join("proc/12/comm"), "luyaz-ui\n").unwrap();
        assert_eq!(uis(&c), (true, false), "luyaz-ui is ours");
        fs::write(root.join("proc/12/comm"), "luyaz-ui.test\n").unwrap();
        assert_eq!(uis(&c), (true, false), "side-by-side luyaz-ui.test is ours");
        fs::write(root.join("proc/12/comm"), "luyaz-ui\n").unwrap();
        assert_eq!(
            o.txn,
            Some(Txn {
                id: "20261010-120000-touch".into(),
                comp: "touch".into(),
                phase: "trial".into(),
            }),
            "a live executor: the touch trial is seen (yield + verdict)"
        );
        assert_eq!(
            o.orphan, None,
            "not a sentinel transaction: the sentinel's to finish"
        );
        fs::remove_dir_all(root.join("proc/4711")).unwrap();
        assert_eq!(observe(&c, 500).txn, None, "executor gone: not live");

        // a sentinel transaction whose executor was killed
        fs::write(
            root.join("data/luyaz/ship/txn"),
            "v=1\ntxn=20261010-120000-sentinel\ncomp=sentinel\nphase=check\nt_phase=1990\nend=1\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("data/luyaz/scripts")).unwrap();
        fs::write(root.join("data/luyaz/scripts/luyaz-ship.sh"), "#!/bin/sh\n").unwrap();
        fs::create_dir_all(root.join("proc/77")).unwrap();
        fs::write(
            root.join("proc/77/cmdline"),
            "/bin/sh\0/data/luyaz/scripts/doctor.sh\0",
        )
        .unwrap();
        let o = observe(&c, 500)
            .orphan
            .expect("a sentinel transaction with no executor");
        assert_eq!((o.uptime, o.t_phase, o.hb), (2010, Some(1990), Some(2000)));
        assert!(o.script && !o.blind);
        assert!(
            orphan_why(&o).unwrap().contains("heartbeat 10s old"),
            "a process that is not the executor holds nothing back"
        );
        fs::remove_dir_all(root.join("proc/77")).unwrap();
        fs::rename(root.join("proc"), root.join("proc.off")).unwrap();
        let o = observe(&c, 500).orphan.unwrap();
        assert!(o.blind);
        assert_eq!(
            orphan_why(&o),
            None,
            "/proc unreadable: never recovered blind"
        );
        fs::rename(root.join("proc.off"), root.join("proc")).unwrap();
        let o = observe(&c, 500).orphan.unwrap();
        assert!(!o.blind);
        assert!(orphan_why(&o).unwrap().contains("heartbeat 10s old"));
        // alive and fresh: not an orphan
        fs::create_dir_all(root.join("proc/4711")).unwrap();
        fs::write(
            root.join("proc/4711/cmdline"),
            "sh\0/data/luyaz/scripts/luyaz-ship.sh\0run\0",
        )
        .unwrap();
        assert_eq!(observe(&c, 500).orphan, None);
        // alive but 31 s silent (hung): the script kills it and takes over
        fs::write(root.join("proc/uptime"), "2031.00 100.00\n").unwrap();
        assert!(observe(&c, 500).orphan.is_some());
        let _ = fs::remove_dir_all(&root);
    }
}
