//! The charge limit as its owner holds it, for battery_eta and charge_policy.
//!
//! While the agent owns the `charge` part (shadow), this mirrors it:
//! - The limit: the agent's `/data/local/tmp/charge_limit.json` (`enabled`, `limit`, `hysteresis`),
//!   read the way `ChargeLimitEnforcer::new` reads it.
//! - The manual override: in the agent's memory only. The agent sets it to the requested state
//!   when the web stops or resumes charging by hand (`charge_control_set`: a
//!   `power.direct_supply.set` with source web/screen), and clears it when the limit settings are
//!   saved (`set`, which rewrites the file). u60d sees both: the write comes through `/control`
//!   (`observe_control`), the save changes the file's mtime. Saving the settings with the limit
//!   on makes the agent enforce at once under the same web source; a web/screen charge write
//!   within 2 s of a file change is taken as that, not as a manual stop. (A manual stop sent in
//!   the same request as new settings comes first, then the save clears it, as in the agent.)
//! - Restarts: the agent starts with the override off, so does u60d.
//!
//! Once u60d owns it (step 9c, [`set_owned`]) nothing is guessed: the touch layer's
//! `/api/device/charge-control` sets the override ([`set_override`]) and saves the settings
//! ([`save`]), and `/control` writes are not looked at. Taking it over or giving it back drops the
//! override, as an agent restart does.

use serde_json::Value;
use std::sync::Mutex;
use std::time::SystemTime;

const CHARGE_LIMIT: &str = luyaz_api::charge::FILE;
/// A web/screen charge write this soon after the limit file changed belongs to the settings save.
const SAVE_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);

pub type Limit = luyaz_api::charge::Settings;

struct Mirror {
    /// u60d owns the part (step 9c)
    owned: bool,
    /// owned: the settings, held here as the agent holds them (loaded at takeover, then saved)
    settings: Limit,
    manual_override: bool,
    /// the file's mtime when last looked at; `None` = not looked yet / no file
    seen: Option<Option<SystemTime>>,
}

static MIRROR: Mutex<Mirror> = Mutex::new(Mirror {
    owned: false,
    settings: Limit {
        enabled: false,
        limit: 100,
        hysteresis: luyaz_api::charge::DEFAULT_HYSTERESIS,
    },
    manual_override: false,
    seen: None,
});

fn lock() -> std::sync::MutexGuard<'static, Mirror> {
    MIRROR.lock().unwrap_or_else(|e| e.into_inner())
}

/// The limit and whether the agent's manual override is on.
pub fn current() -> (Limit, bool) {
    {
        let m = lock();
        if m.owned {
            return (m.settings, m.manual_override);
        }
    }
    let path = crate::state::host_path(CHARGE_LIMIT);
    let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    let limit = parse(std::fs::read_to_string(&path).ok().as_deref());
    let mut m = lock();
    if let Some(prev) = m.seen
        && prev != mtime
    {
        // the agent saved the settings: it cleared the override
        m.manual_override = false;
    }
    m.seen = Some(mtime);
    (limit, m.manual_override)
}

/// The agent's file format (u60-api `charge`).
pub fn parse(text: Option<&str>) -> Limit {
    luyaz_api::charge::parse(text)
}

/// Whether u60d owns the part now.
pub fn owned() -> bool {
    lock().owned
}

/// u60d takes the part over (the settings read from `file`, as the agent reads them at start) or
/// gives it back: the override starts off either way. Returns whether it changed.
pub fn set_owned(on: bool, file: &std::path::Path) -> bool {
    let mut m = lock();
    if m.owned == on {
        return false;
    }
    m.owned = on;
    if on {
        m.settings = parse(std::fs::read_to_string(file).ok().as_deref());
    }
    m.manual_override = false;
    m.seen = None;
    true
}

/// A manual stop or resume (owner only): the policy leaves charging alone while it is on.
pub fn set_override(on: bool) {
    lock().manual_override = on;
}

/// New settings (owner only): taken at once and the override cleared, as the agent's `set()`, and
/// saved to `file` as the agent saves them (atomically, same bytes; a failed save is only logged,
/// as the agent).
pub fn save(s: &Limit, file: &std::path::Path) {
    {
        let mut m = lock();
        m.settings = *s;
        m.manual_override = false;
    }
    let tmp = file.with_extension("json.tmp");
    let res = std::fs::write(&tmp, luyaz_api::charge::file_text(s))
        .and_then(|_| std::fs::rename(&tmp, file));
    if let Err(e) = res {
        eprintln!("[compat] charge: saving {}: {e}", file.display());
    }
}

/// Every `/control` charge write that succeeded: the agent's own charge writes (source auto) go to the
/// charge_policy comparison, a manual one (web/screen) sets the override mirror.
pub fn observe_control(action: &str, body: &Value) {
    if action != "power.direct_supply.set" || !super::enabled() || owned() {
        // owned: the override is what the route set, and the auto writes are u60d's own
        return;
    }
    let Some(source) = body
        .get("source")
        .and_then(Value::as_str)
        .and_then(crate::ops::Source::parse)
    else {
        return;
    };
    let params = body.get("params").cloned().unwrap_or(Value::Null);
    let Ok(stopped) = crate::control::direct_supply_enabled(&params) else {
        return;
    };
    match source {
        crate::ops::Source::Auto => super::charge_policy::agent_wrote(!stopped),
        crate::ops::Source::Web | crate::ops::Source::Screen => {
            // Saving the limit settings rewrites the file and then enforces at once under the same
            // source: a charge write right after the file changed is that, not a manual stop. The
            // save cleared the override (agent `set`), so take the new mtime as seen and clear it.
            let mtime = std::fs::metadata(crate::state::host_path(CHARGE_LIMIT))
                .and_then(|m| m.modified())
                .ok();
            let fresh = mtime
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age < SAVE_WINDOW);
            let mut m = lock();
            if fresh {
                m.manual_override = false;
                m.seen = Some(mtime);
            } else {
                m.manual_override = stopped;
            }
        }
        _ => {}
    }
}
