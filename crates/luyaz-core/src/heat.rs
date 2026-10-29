//! Heat care (`docs/designs/thermal-care.md`): reads the battery and CPU temperatures on the
//! charge policy's tick (no timer of its own), keeps the level (`luyaz_policy::thermal`) and asks
//! for what it calls for:
//! - charging: a pause through the charge executor (`api/charge.rs`, `Job::Heat`), the one writer
//!   of the charger switch; a manual resume wins until unplugged, except at critical;
//! - CHILL: `/tmp/luyaz/heat` holds `hot` (eco) or `critical` (direct); chill.sh acts on it and
//!   stays the one controller of the proxy core;
//! - alerts: a level going up to warm or hot is an inbox notice (`heat-protect`, never texted);
//!   critical is the regress incident `regress-heat` (texted; `longterm::regress`).
//!
//! The stock driver's own protection (`thermal_protection_mbb`) is read along for display only.

use luyaz_policy::thermal::{self as t, Chill, Level, Limits, Reading, Tracker};
use serde_json::{Value, json};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Readings this far apart at least (the tick comes every 5 s).
pub const EVERY: Duration = Duration::from_secs(30);
/// The request chill.sh reads.
pub const CHILL_REQ: &str = "/tmp/luyaz/heat";
/// A device test: whole °C added to both readings (tmpfs, gone on reboot; design §5).
pub const TEST_OFFSET: &str = "/tmp/luyaz/heat-test";
/// What chill.sh is doing now (`state`, `reason`, `thermal_eco`).
const CHILL_STATE: &str = "/tmp/chill.state";
pub const NOTICE_KIND: &str = "heat-protect";

#[derive(Default)]
struct State {
    t0: Option<Instant>,
    last: Option<Duration>,
    tracker: Tracker,
    reading: Reading,
    capacity: Option<u8>,
    /// Wall clock (s) the level was entered.
    since: i64,
    manual_resume: bool,
    /// Charging is stopped because of the heat (the executor's write went through).
    held: bool,
    chill_written: Option<Chill>,
    /// What CHILL is doing for the heat now: eco, direct, or neither (not running, or not yet).
    chill_now: Chill,
    /// CHILL is running at all (its state file says running or direct).
    chill_up: bool,
    stock_protect: Option<i64>,
    test_offset: i32,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(State::default))
}

/// Millidegrees from a zone or sysfs file, kept when plausible (as `state::thermal_zones`).
fn plausible_mc(v: i64) -> Option<i64> {
    (v != 0 && v > -40_000 && v <= 200_000).then_some(v)
}

/// The battery (°C) and the hottest CPU/PMIC zone (°C) under `root`. Main-chip side only: the
/// modem-side sensors (`mdmss*`, `sdr0*`, `mmw*`, `epm*`) are not read (9-28: reading them was
/// the prime suspect for the modem crashes, d77768e), so a zone's `temp` is opened only for kinds
/// named here.
pub fn read_zones(root: &std::path::Path) -> (Option<i32>, Option<i32>) {
    let (mut batt, mut chip): (Option<i64>, Option<i64>) = (None, None);
    let Ok(dir) = std::fs::read_dir(root) else {
        return (None, None);
    };
    for item in dir.flatten() {
        if !item
            .file_name()
            .to_string_lossy()
            .starts_with("thermal_zone")
        {
            continue;
        }
        let p = item.path();
        let Ok(kind) = std::fs::read_to_string(p.join("type")) else {
            continue;
        };
        let kind = kind.trim();
        let is_chip = kind.starts_with("cpuss") || kind == "pmx75_tz";
        if kind != "battery" && !is_chip {
            continue;
        }
        let Some(mc) = std::fs::read_to_string(p.join("temp"))
            .ok()
            .and_then(|s| s.trim().parse::<i64>().ok())
            .and_then(plausible_mc)
        else {
            continue;
        };
        let slot = if is_chip { &mut chip } else { &mut batt };
        *slot = Some(slot.map_or(mc, |m| m.max(mc)));
    }
    let c = |v: Option<i64>| v.map(|mc| (mc as f64 / 1000.0).round() as i32);
    (c(batt), c(chip))
}

/// CHILL's own state file: whether it is up, and what it is doing for the heat.
fn read_chill() -> (bool, Chill) {
    let Some(v) = std::fs::read_to_string(crate::state::host_path(CHILL_STATE))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
    else {
        return (false, Chill::Leave);
    };
    let state = v["state"].as_str().unwrap_or("");
    let now = if state == "direct" && v["reason"] == "overheat" {
        Chill::Direct
    } else if state == "running" && v["thermal_eco"] == true {
        Chill::Eco
    } else {
        Chill::Leave
    };
    (matches!(state, "running" | "direct"), now)
}

fn read_offset() -> i32 {
    std::fs::read_to_string(crate::state::host_path(TEST_OFFSET))
        .ok()
        .and_then(|s| s.trim().trim_start_matches('+').parse::<i32>().ok())
        .filter(|v| (-60..=60).contains(v))
        .unwrap_or(0)
}

fn read_reading(plugged: Option<bool>) -> (Reading, Option<i64>) {
    let root = std::env::var("LUYAZ_THERMAL_ROOT").unwrap_or_else(|_| "/sys/class/thermal".into());
    let (zone_batt, chip) = read_zones(std::path::Path::new(&root));
    let zte = |f: &str| {
        std::fs::read_to_string(crate::state::host_path(&format!(
            "/sys/class/power_supply/battery_zte/{f}"
        )))
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
    };
    // the driver's own °C first (what its protection table reads), the zone otherwise
    let batt = zte("temp_mbb")
        .filter(|v| *v > -40 && *v < 120)
        .map(|v| v as i32)
        .or(zone_batt);
    (
        Reading {
            batt,
            chip,
            plugged,
        },
        zte("thermal_protection_mbb"),
    )
}

/// Whether charging should be paused now (the executor asks).
pub fn wants_pause() -> bool {
    with(|s| want(s))
}

fn want(s: &State) -> bool {
    s.reading.plugged == Some(true)
        && t::actions(
            s.tracker.level,
            s.capacity,
            s.manual_resume,
            &Limits::default(),
        )
        .pause_charge
}

pub fn held() -> bool {
    with(|s| s.held)
}

/// The executor paused (true) or resumed (false) charging for the heat.
pub fn set_held(on: bool) {
    with(|s| s.held = on);
}

/// The owner resumed charging by hand: it wins over a warm/hot pause until unplugged.
pub fn manual_resume() {
    with(|s| {
        s.manual_resume = true;
        s.held = false;
    });
}

/// The level, for regress: None = no reading at all, Some(critical).
pub fn critical() -> Option<bool> {
    with(|s| {
        (s.reading.batt.is_some() || s.reading.chip.is_some())
            .then_some(s.tracker.level == Level::Critical)
    })
}

/// On every charge policy tick: `plugged` = a charger with a real input (or charging stopped),
/// `unplugged` = charger_connect reads 0 (ends a manual resume), `capacity` the battery %.
/// Returns whether the executor should look at the heat pause.
pub fn tick(plugged: Option<bool>, unplugged: bool, capacity: u8) -> bool {
    let (reading, stock, offset, (chill_up, chill_now)) = {
        let due = with(|s| {
            let t0 = *s.t0.get_or_insert_with(Instant::now);
            let now = t0.elapsed();
            let due = s.last.is_none_or(|l| now.saturating_sub(l) >= EVERY);
            if due {
                s.last = Some(now);
            }
            due
        });
        if !due {
            return false;
        }
        let (mut r, stock) = read_reading(plugged);
        let offset = read_offset();
        if offset != 0 {
            r.batt = r.batt.map(|v| v + offset);
            r.chip = r.chip.map(|v| v + offset);
        }
        (r, stock, offset, read_chill())
    };
    let wall = luyaz_api::alerts::now();
    let (from, to, chill_change, ask, plan) = with(|s| {
        let now = s.t0.map_or(Duration::ZERO, |t| t.elapsed());
        s.reading = reading;
        s.capacity = (capacity > 0).then_some(capacity);
        s.stock_protect = stock;
        s.chill_up = chill_up;
        s.chill_now = chill_now;
        if offset != s.test_offset {
            eprintln!("[heat] test offset {offset:+} °C");
            s.test_offset = offset;
        }
        if unplugged {
            s.manual_resume = false;
        }
        let from = s.tracker.level;
        let to = s.tracker.step(now, &reading, &Limits::default());
        if to != from {
            s.since = wall;
        }
        let chill = t::actions(to, s.capacity, s.manual_resume, &Limits::default()).chill;
        let chill_change = (s.chill_written != Some(chill)).then_some(chill);
        // the executor looks when the pause and what holds it disagree, and only plugged in
        let ask = plugged == Some(true) && want(s) != s.held;
        // what the notice can say will happen: a pause only plugged in, eco only with CHILL up
        let plan = (want(s), chill == Chill::Eco && s.chill_up);
        (from, to, chill_change, ask, plan)
    });
    if let Some(c) = chill_change
        && write_chill(c)
    {
        with(|s| s.chill_written = Some(c));
    }
    if to != from {
        eprintln!(
            "[heat] {} -> {} (battery {:?} °C, chip {:?} °C, plugged {:?})",
            from.as_str(),
            to.as_str(),
            reading.batt,
            reading.chip,
            plugged
        );
        if to > from && matches!(to, Level::Warm | Level::Hot) {
            notice(&reading, plan, wall);
        }
    }
    ask
}

fn write_chill(c: Chill) -> bool {
    let path = crate::state::host_path(CHILL_REQ);
    let path = std::path::Path::new(&path);
    if c == Chill::Leave {
        return match std::fs::remove_file(path) {
            Ok(()) => true,
            Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        };
    }
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = path.with_extension("tmp");
    let ok = std::fs::write(&tmp, format!("{}\n", c.as_str())).is_ok()
        && std::fs::rename(&tmp, path).is_ok();
    if !ok {
        eprintln!("[heat] could not write {}", path.display());
    }
    ok
}

/// `plan`: (charging will pause, CHILL will go to eco). Says only what is going to happen.
fn notice(r: &Reading, plan: (bool, bool), wall: i64) {
    let boot = crate::longterm::boot::proc_boot_id();
    let Some(b) = boot.get(..8) else {
        return;
    };
    let up = std::fs::read_to_string(crate::state::host_path("/proc/uptime"))
        .ok()
        .and_then(|s| s.split('.').next()?.parse::<u64>().ok())
        .unwrap_or(0);
    // `<boot>-<n>` as the queue wants it; ×100 + 71 keeps clear of sim_alerts' `epoch×10 + kind`
    let id = format!("{}-{}", b.to_ascii_lowercase(), up * 100 + 71);
    let deg = |v: Option<i32>| v.map_or("?".to_string(), |v| v.to_string());
    let head = format!(
        "温度偏高（电池 {} °C、芯片 {} °C）",
        deg(r.batt),
        deg(r.chip)
    );
    let text = match plan {
        (true, true) => format!("{head}：暂停充电、CHILL 降到省电档，凉下来自动恢复"),
        (true, false) => format!("{head}：暂停充电，凉下来自动恢复"),
        (false, true) => format!("{head}：CHILL 降到省电档，凉下来自动恢复"),
        (false, false) => format!("{head}，放到通风的地方"),
    };
    let dir = crate::state::host_path(luyaz_api::alerts::DIR);
    let a = luyaz_api::alerts::Alerts::at(std::path::Path::new(&dir));
    if let Err(e) = a.add_once(&id, NOTICE_KIND, &text, wall, up) {
        eprintln!("[heat] notice: {e}");
    }
}

/// The `heat` block: what the screen, the web and doctor show.
pub fn report(owned: bool) -> Value {
    with(|s| {
        let l = Limits::default();
        let a = t::actions(s.tracker.level, s.capacity, s.manual_resume, &l);
        json!({
            "level": s.tracker.level.as_str(),
            "since": (s.tracker.level != Level::Normal).then_some(s.since),
            "battery_c": s.reading.batt,
            "chip_c": s.reading.chip,
            "plugged": s.reading.plugged,
            "charge_paused": s.held,
            "manual_resume": s.manual_resume,
            "chill": a.chill.as_str(),
            "chill_now": s.chill_now.as_str(),
            "test_offset": (s.test_offset != 0).then_some(s.test_offset),
            "stock_protect": s.stock_protect,
            "acting": owned,
        })
    })
}

#[cfg(test)]
mod tests;
