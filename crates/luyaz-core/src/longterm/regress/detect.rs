//! The detectors, one per kind (CEO review S5), each fed by the minute check (or the hour line,
//! or the boot facts) and keeping its timers in memory: a restart starts them again, which only
//! delays an opening or a recovery by its hold time. Holds run on CLOCK_BOOTTIME (a wall clock
//! step does not move them, CEO review S6).

use serde_json::Value;
use std::collections::BTreeMap;

use super::{At, Regress, Said, interval_id};

/// Memory, the starting values (design §3; the owner can raise the absolute ones).
pub const HEAP_REL: f64 = 1.5;
pub const ANON_REL: f64 = 2.0;
pub const HEAP_HOLD_S: i64 = 900;
pub const ANON_HOLD_S: i64 = 1800;
pub const MEM_SETTLE_S: i64 = 1800;
pub const RECOVER_HOLD_S: i64 = 1800;
pub const RECOVER_PART: f64 = 0.8;
/// Cellular and exits: down this long opens; this many good checks in a row close.
pub const DOWN_HOLD_S: i64 = 300;
pub const GOOD_CHECKS: u32 = 3;
/// A check said to be blind for this long opens `regress-blind`, at most once in 7 days.
pub const BLIND_AFTER_S: i64 = 3 * 86_400;
pub const BLIND_EVERY_S: i64 = 7 * 86_400;
/// Standby power: a qualifying hour (eng review R10).
pub const OFF_HOUR_S: i64 = 3540;
/// The cell's packets in a qualifying hour (P90 of 10-18..10-24's qualifying hours on both
/// devices: 11 367 and 13 762).
pub const WAN_PKTS_MAX: i64 = 15_000;
pub const POWER_REL: f64 = 1.3;
pub const POWER_REL_ADD: f64 = 100.0;
pub const POWER_RECOVER_ABS: f64 = 480.0;

/// What one minute check read.
#[derive(Clone, Debug, Default)]
pub struct Check {
    pub at: At,
    /// This core process's age (s).
    pub core_age: i64,
    /// The heap's live kB and RssAnon kB (mem.rs, /proc/self/status).
    pub heap_kb: Option<i64>,
    pub anon_kb: Option<i64>,
    /// None = cannot tell (no SIM, data off, roaming without data roaming, offline, an operation
    /// switching things).
    pub cell: Option<Cell>,
    /// Per exit: None = not meant on (or cannot tell), Some(ok).
    pub chill: Option<bool>,
    pub ts: Option<bool>,
    /// What the home screens' finding and the no-service ledger say now (cellular, exits).
    pub observed: Option<Said>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cell {
    pub registered: bool,
    pub route_up: bool,
}

#[derive(Clone, Debug, Default)]
struct Hold {
    since: Option<i64>,
}

impl Hold {
    /// The condition at `up`: how long it has held.
    fn step(&mut self, cond: bool, up: i64) -> i64 {
        if !cond {
            self.since = None;
            return -1;
        }
        up - *self.since.get_or_insert(up)
    }
}

#[derive(Clone, Debug, Default)]
struct Path {
    bad: Hold,
    bad_n: u32,
    good: u32,
}

#[derive(Debug, Default)]
pub struct State {
    heap: Hold,
    anon: Hold,
    heap_abs: Hold,
    anon_abs: Hold,
    mem_ok: Hold,
    mem_abs_ok: Hold,
    cell_bad: Hold,
    cell_good: u32,
    paths: BTreeMap<&'static str, Path>,
    /// Per power cell: the last qualifying hours' mW (newest last, two kept).
    power: BTreeMap<String, Vec<f64>>,
    /// Blind state changed without a save yet (saved with the next one, at most hourly).
    pub blind_moved: bool,
}

fn mib(kb: f64) -> String {
    format!("{:.0}MiB", kb / 1024.0)
}

impl Regress {
    /// One minute check.
    pub fn check(&mut self, k: &Check) {
        self.mem(k);
        self.cellular(k);
        self.egress(k);
        let cell_known = k.cell.is_some();
        let egress_known = k.chill.is_some() || k.ts.is_some();
        self.blind("cellular", cell_known, &k.at);
        self.blind("egress", egress_known, &k.at);
        super::sync::expire(self, &k.at);
        self.ask(&k.at);
        self.prune(&k.at);
    }

    fn mem(&mut self, k: &Check) {
        let Some(heap) = k.heap_kb.map(|h| h as f64) else {
            return;
        };
        let anon = k.anon_kb.map(|a| a as f64);
        let up = k.at.up;
        // absolute: always on, no baseline
        let (lh, la) = self.base.mem_limits();
        let ha = self.det.heap_abs.step(heap > lh, up);
        let aa = self.det.anon_abs.step(anon.is_some_and(|a| a > la), up);
        match self.find_open("regress-mem-abs", None) {
            None if ha >= HEAP_HOLD_S || aa >= ANON_HOLD_S => {
                let d = if ha >= HEAP_HOLD_S {
                    format!(
                        "heap {} > {} 15m; anon {}",
                        mib(heap),
                        mib(lh),
                        anon.map_or("-".into(), mib)
                    )
                } else {
                    format!(
                        "anon {} > {} 30m; heap {}",
                        anon.map_or("-".into(), mib),
                        mib(la),
                        mib(heap)
                    )
                };
                self.open_mem("regress-mem-abs", &k.at, d);
            }
            Some(i) => {
                let below = heap < lh * RECOVER_PART && anon.is_none_or(|a| a < la * RECOVER_PART);
                if self.det.mem_abs_ok.step(below, up) >= RECOVER_HOLD_S {
                    let d = format!(
                        "heap {} anon {} under 80% 30m",
                        mib(heap),
                        anon.map_or("-".into(), mib)
                    );
                    self.close(i, &k.at, "recovered", &d);
                    self.det.mem_abs_ok = Hold::default();
                }
            }
            None => {}
        }
        // relative: after the core settled, with a baseline, not while the absolute one is open
        let Some((h0, a0)) = self.base.mem() else {
            return;
        };
        let settled = k.core_age >= MEM_SETTLE_S;
        let hr = self.det.heap.step(settled && heap > h0 * HEAP_REL, up);
        let ar = self
            .det
            .anon
            .step(settled && anon.is_some_and(|a| a > a0 * ANON_REL), up);
        match self.find_open("regress-mem", None) {
            None if self.find_open("regress-mem-abs", None).is_none()
                && (hr >= HEAP_HOLD_S || ar >= ANON_HOLD_S) =>
            {
                let d = if hr >= HEAP_HOLD_S {
                    format!("heap {} > 1.5x base {} 15m", mib(heap), mib(h0))
                } else {
                    format!(
                        "anon {} > 2x base {} 30m",
                        anon.map_or("-".into(), mib),
                        mib(a0)
                    )
                };
                self.open_mem("regress-mem", &k.at, d);
            }
            Some(i) => {
                let below = heap < h0 * HEAP_REL * RECOVER_PART
                    && anon.is_none_or(|a| a < a0 * ANON_REL * RECOVER_PART);
                if self.det.mem_ok.step(below, up) >= RECOVER_HOLD_S {
                    let d = format!(
                        "heap {} anon {} back under 80% 30m",
                        mib(heap),
                        anon.map_or("-".into(), mib)
                    );
                    self.close(i, &k.at, "recovered", &d);
                    self.det.mem_ok = Hold::default();
                }
            }
            None => {}
        }
    }

    /// The memory snapshot goes with it (written by the IO side for open memory incidents
    /// without `evidence` yet, CEO review X2).
    fn open_mem(&mut self, kind: &str, at: &At, detail: String) {
        self.open(kind, None, at, detail, None, None);
    }

    fn cellular(&mut self, k: &Check) {
        let up = k.at.up;
        let open = self.find_open("regress-cellular", None);
        let Some(c) = k.cell else {
            // cannot tell: no hold runs across it
            self.det.cell_bad = Hold::default();
            return;
        };
        let bad = !c.registered || !c.route_up;
        let held = self.det.cell_bad.step(bad, up);
        match open {
            None if held >= DOWN_HOLD_S => {
                let why = if !c.registered {
                    "no service"
                } else {
                    "registered, no data route"
                };
                self.det.cell_good = 0;
                self.open(
                    "regress-cellular",
                    None,
                    &k.at,
                    format!("{why} {}m", held / 60),
                    None,
                    k.observed.clone(),
                );
            }
            Some(i) => {
                if bad {
                    self.det.cell_good = 0;
                } else {
                    self.det.cell_good += 1;
                    if self.det.cell_good >= GOOD_CHECKS {
                        self.close(
                            i,
                            &k.at,
                            "recovered",
                            "registered with a data route 3 checks",
                        );
                        self.det.cell_good = 0;
                    }
                }
            }
            None => {}
        }
    }

    fn egress(&mut self, k: &Check) {
        // while the cellular link is down its exits' failures are its own (design §3)
        let cell_open = self.find_open("regress-cellular", None).is_some();
        for (name, v) in [("chill", k.chill), ("tailscale", k.ts)] {
            let open = self.find_open("regress-egress", Some(name));
            let p = self.det.paths.entry(name).or_default();
            let v = if cell_open { None } else { v };
            match v {
                None => {
                    p.bad = Hold::default();
                    p.bad_n = 0;
                }
                Some(false) => {
                    p.good = 0;
                    p.bad_n += 1;
                    let held = p.bad.step(true, k.at.up);
                    if open.is_none() && held >= DOWN_HOLD_S && p.bad_n >= 2 {
                        let d = format!("{name} failing {}m", held / 60);
                        self.open(
                            "regress-egress",
                            Some(name),
                            &k.at,
                            d,
                            None,
                            k.observed.clone(),
                        );
                    }
                }
                Some(true) => {
                    p.bad = Hold::default();
                    p.bad_n = 0;
                    p.good += 1;
                    if let Some(i) = open
                        && p.good >= GOOD_CHECKS
                    {
                        p.good = 0;
                        self.close(i, &k.at, "recovered", &format!("{name} through 3 checks"));
                    }
                }
            }
        }
    }

    /// One class judged or not at `at` (design §5, CEO R3-6).
    pub fn blind(&mut self, class: &str, known: bool, at: &At) {
        let b = self.file.blind.entry(class.to_string()).or_default();
        if known {
            if b.since.take().is_some() {
                self.det.blind_moved = true;
            }
            if let Some(i) = self.find_open("regress-blind", Some(class)) {
                self.close(i, at, "recovered", &format!("{class} can be judged again"));
            }
            return;
        }
        if b.since.is_none() {
            b.since = Some(at.clone());
            self.det.blind_moved = true;
        }
        let long = b
            .since
            .as_ref()
            .and_then(|s| s.until(at))
            .is_some_and(|s| s >= BLIND_AFTER_S);
        let due = b
            .said
            .is_none_or(|w| !at.ok || at.wall - w >= BLIND_EVERY_S);
        if long && due && self.find_open("regress-blind", Some(class)).is_none() {
            if let Some(b) = self.file.blind.get_mut(class) {
                b.said = Some(at.wall);
            }
            let d = format!("{class} could not be judged for 3 days");
            self.open("regress-blind", Some(class), at, d, None, None);
        }
    }

    /// A new hour line (standby power: eng review R10, R1).
    pub fn hour(&mut self, line: &Value, at: &At) {
        if line["k"] != "hour_power" {
            return;
        }
        for (cell, mw) in qualifying(line) {
            let v = self.det.power.entry(cell.clone()).or_default();
            v.push(mw);
            if v.len() > 2 {
                v.remove(0);
            }
            let last2: Vec<f64> = v.clone();
            self.power_cell(&cell, &last2, at);
        }
    }

    fn power_cell(&mut self, cell: &str, last2: &[f64], at: &At) {
        let two = last2.len() == 2;
        let abs = self.base.power_limit();
        let over_abs = two && last2.iter().all(|&m| m > abs);
        let under_abs = two && last2.iter().all(|&m| m < POWER_RECOVER_ABS.min(abs * 0.8));
        let now = last2.last().copied().unwrap_or(0.0);
        match self.find_open("regress-power-abs", Some(cell)) {
            None if over_abs => {
                let d = format!("{cell} standby {now:.0}mW > {abs:.0}mW 2h");
                self.open("regress-power-abs", Some(cell), at, d, Some(now), None);
            }
            Some(i) if under_abs => {
                self.close(i, at, "recovered", &format!("{cell} standby {now:.0}mW 2h"));
            }
            _ => {}
        }
        let Some(b) = self.base.power(cell) else {
            return;
        };
        let trig = (b * POWER_REL).max(b + POWER_REL_ADD);
        let over = two && last2.iter().all(|&m| m > trig);
        let under = two && last2.iter().all(|&m| m < trig * 0.8);
        match self.find_open("regress-power", Some(cell)) {
            None if over && self.find_open("regress-power-abs", Some(cell)).is_none() => {
                let d = format!("{cell} standby {now:.0}mW > {trig:.0}mW (base {b:.0}) 2h");
                self.open("regress-power", Some(cell), at, d, Some(now), None);
            }
            Some(i) if under => {
                self.close(i, at, "recovered", &format!("{cell} standby {now:.0}mW 2h"));
            }
            _ => {}
        }
    }

    /// This boot's facts (design §3 `regress-reboot`, eng review R5): judged once a boot.
    pub fn boot(
        &mut self,
        facts: &Value,
        at: &At,
        boot: &str,
        ship: Option<(super::Ship, String)>,
    ) {
        if self.file.boot == boot {
            return;
        }
        self.file.boot = boot.into();
        self.dirty = true;
        let code = facts["code"].as_i64();
        let planned = facts["planned"].as_str().map(str::to_string);
        let class = code.map_or("unknown", |c| crate::scorecard::klass(&c.to_string()));
        if planned.is_some() || matches!(class, "manual" | "scheduled") {
            self.log.push(format!(
                "[regress] boot {} code {code:?} {class} planned {planned:?}: not an alert",
                at.boot
            ));
            return;
        }
        let words = reboot_words(code, class);
        let prev = self.prev_build.clone();
        let mut d = match code {
            Some(c) => format!("code {c}"),
            None => "code unknown".into(),
        };
        if !prev.is_empty() {
            d.push_str(&format!("; prev build {}", short_build(&prev)));
        }
        if let Some((s, clock)) = &ship {
            d.push_str(&format!(
                "; ship {} {}",
                s.comp,
                if clock == "uncertain" {
                    "before reboot?"
                } else {
                    ""
                }
            ));
        }
        // a row: within 6 h of the last reboot alert it is the same incident (2nd, 4th, 8th go)
        if let Some(i) = self.find_open("regress-reboot", None) {
            let inc = &mut self.file.open[i];
            let near = inc
                .last
                .as_ref()
                .and_then(|l| l.until(at))
                .is_some_and(|s| s <= super::REBOOT_WINDOW_S);
            if near {
                inc.count += 1;
                inc.last = Some(at.clone());
                let n = inc.count;
                self.log.push(format!(
                    "[regress] again {} regress-reboot #{n} {d}",
                    inc.id
                ));
                if matches!(n, 2 | 4 | 8) {
                    inc.esc += 1;
                    let text = super::clip(&format!("#{n} {d}"));
                    inc.transition(&format!("esc{n}"), text);
                }
                self.dirty = true;
                return;
            }
            self.close(i, at, "quiet", "no reboot for 6 h");
        }
        let id = interval_id("regress-reboot", None, at);
        let mut inc = super::Incident::new(id, "regress-reboot", at.clone());
        inc.last = Some(at.clone());
        inc.build = self.build.clone();
        inc.detail = super::clip(&d);
        inc.observed = Some(words);
        if let Some((s, clock)) = ship {
            inc.ship = Some(s);
            inc.ship_clock = Some(clock);
        }
        inc.transition("open", inc.detail.clone());
        self.log.push(format!(
            "[regress] open {} regress-reboot {}",
            inc.id, inc.detail
        ));
        self.fresh.push(inc.id.clone());
        self.file.open.push(inc);
    }
}

fn short_build(b: &str) -> String {
    b.chars().take(12).collect()
}

/// The reason code's class in words (CEO review X3r: a reboot carries its class).
fn reboot_words(code: Option<i64>, class: &str) -> Said {
    let c = code.map_or("?".to_string(), |c| c.to_string());
    let (zh, en) = match class {
        "modem" => (
            format!("原因码 {c}：基带重启"),
            format!("code {c}: modem restart"),
        ),
        "noui" => (
            format!("原因码 {c}：屏幕没有界面"),
            format!("code {c}: no UI on screen"),
        ),
        "cutoff" => (
            format!("原因码 {c}：断线保护"),
            format!("code {c}: cutoff protect"),
        ),
        "unknown" => ("原因码未知".to_string(), "reason code unknown".to_string()),
        _ => (format!("原因码 {c}"), format!("code {c}")),
    };
    Said { zh, en }
}

/// A `hour_power` line's qualifying cells and their standby mW (eng review R10): the whole hour
/// screen off, not charging, few packets in the cell, the counter observed ≥ 95 % of the cell.
pub fn qualifying(l: &Value) -> Vec<(String, f64)> {
    let n = |k: &str| l[k].as_i64().unwrap_or(0);
    if n("off_home_s") + n("off_abroad_s") < OFF_HOUR_S || l["chg_s"].as_i64() != Some(0) {
        return Vec::new();
    }
    let mut v = Vec::new();
    for (cell, name) in [("off_home", "home"), ("off_abroad", "abroad")] {
        let s = n(&format!("{cell}_s"));
        if s < OFF_HOUR_S / 2 {
            continue;
        }
        let Some(pk) = l[format!("{cell}_wan_pkts")].as_i64() else {
            continue;
        };
        if pk > WAN_PKTS_MAX {
            continue;
        }
        let q = &l["counter_quality"]["cells"][cell];
        let (Some(obs), Some(uwh)) = (q["observed_s"].as_i64(), q["uwh"].as_f64()) else {
            continue;
        };
        if obs <= 0 || (obs as f64) < 0.95 * s as f64 {
            continue;
        }
        v.push((name.to_string(), uwh / 1000.0 * 3600.0 / obs as f64));
    }
    v
}
