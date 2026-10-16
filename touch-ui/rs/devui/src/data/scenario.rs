//! Home's 情景 card and the agent's health (src/scenario.c): zte-agent's GET /api/public/status
//! (no login), every 30 s on any page (the status bar's 「后台失联」 must be right everywhere),
//! at once when Home comes into view unless the agent is not answering.
// SPDX-License-Identifier: MIT

use super::core_api::CoreApi;
use super::util::{fnv, jstr, jtrue, mono_ms, FNV0};
use crate::json;
use crate::lang;
use devui_model::{agent_health_t, scenario_status_t};

/// a local request takes tens of ms; a stuck agent must not hold Home up
const SC_IO_MS: i32 = 800;
/// the engine switches no faster than every second scan: 30 s is enough
const SC_TTL_MS: i64 = 30000;
const SC_RESP_MAX: usize = 16384;
/// lost only after 2 minutes: an agent restart takes seconds
const SC_LOST_MS: i64 = 120000;

#[derive(Default)]
pub struct Scenario {
    ok: bool,
    configured: bool,
    enabled: bool,
    wifi_off: bool,
    name: String,
    pin: String,
    last_switch: i64,
    abroad: bool,
    /// changed locally (a switch from the screen): redraw on the next round
    dirty: bool,
    was_active: bool,
    poll_ms: i64,
    sig: u32,
    /// the last good read; 0 = never, counted from first_ms
    ok_ms: i64,
    first_ms: i64,
    unread: i32,
    h_checked: bool,
    h_bad: i32,
    h_warn: i32,
}

impl Scenario {
    pub fn new() -> Scenario {
        Scenario {
            ..Default::default()
        }
    }

    /// Every refresh; active = Home is up on a lit screen. true = what shows changed.
    pub fn poll(&mut self, agent: &mut CoreApi, active: bool) -> bool {
        let just_shown = active && !self.was_active;
        let t = mono_ms();
        if self.first_ms == 0 {
            self.first_ms = t;
        }
        self.was_active = active;
        if self.poll_ms != 0 && t - self.poll_ms < SC_TTL_MS && !(just_shown && self.ok) {
            if !self.dirty {
                return false;
            }
            self.dirty = false;
            return true;
        }
        self.dirty = false;
        self.poll_ms = t;

        self.ok = false;
        let (code, b) = agent.http("GET", "/api/public/status", None, SC_RESP_MAX, SC_IO_MS);
        let data = b.filter(|_| code == 200).and_then(|b| json::get(&b, "data", SC_RESP_MAX)).unwrap_or_default();
        if !data.is_empty() {
            self.ok_ms = t;
            self.unread = json::get(&data, "alerts", 128).map_or(0, |al| json::get_int(&al, "unread", 0) as i32);
            match json::get(&data, "health", 128) {
                Some(h) => {
                    self.h_checked = jtrue(&h, "checked");
                    self.h_bad = json::get_int(&h, "bad", 0) as i32;
                    self.h_warn = json::get_int(&h, "warn", 0) as i32;
                }
                None => self.h_checked = false,
            }
        }
        if !data.is_empty()
            && let Some(sc) = json::get(&data, "scenario", 1024)
        {
            self.ok = true;
            self.configured = jtrue(&sc, "configured");
            self.enabled = jtrue(&sc, "enabled");
            self.wifi_off = jtrue(&sc, "wifi_off");
            // English: name_en for the factory scenarios not renamed; display only
            self.name = if lang::is_en() { jstr(&sc, "name_en", 48) } else { String::new() };
            if self.name.is_empty() {
                self.name = jstr(&sc, "name", 48);
            }
            self.pin = jstr(&sc, "pin", 48);
            self.last_switch = json::get_int(&sc, "last_switch", 0);
            self.abroad = jtrue(&sc, "abroad");
        }

        let nums = format!(
            "{}/{}/{}/{}/{}/{}/{}",
            self.ok as i32,
            self.configured as i32,
            self.enabled as i32,
            self.wifi_off as i32,
            self.last_switch,
            self.unread,
            self.abroad as i32
        );
        let mut h = fnv(FNV0, nums.as_bytes());
        h = fnv(h, self.name.as_bytes());
        h = fnv(h, self.pin.as_bytes());
        if h == self.sig {
            return false;
        }
        self.sig = h;
        true
    }

    pub fn status(&self) -> scenario_status_t {
        scenario_status_t {
            available: (self.ok && self.configured) as i32,
            enabled: self.enabled as i32,
            name: self.name.clone(),
            wifi_off: self.wifi_off as i32,
            pin: self.pin.clone(),
            last_switch: self.last_switch,
            abroad: self.abroad as i32,
            ..Default::default()
        }
    }


    /// Read again on the next round (after pinning a scenario).
    pub fn kick(&mut self) {
        self.poll_ms = 0;
        self.sig = 0;
    }

    /// The agent lost (more than 2 minutes without a read, counted from the start if it never
    /// answered), its unread alerts and the device check's counts.
    pub fn health(&self) -> agent_health_t {
        let t = mono_ms();
        let since = if self.ok_ms != 0 { self.ok_ms } else { self.first_ms };
        let lost = if since != 0 && t - since > SC_LOST_MS { (t - since) / 1000 } else { 0 };
        agent_health_t {
            lost_secs: lost,
            unread: if lost != 0 { 0 } else { self.unread },
            checked: (lost == 0 && self.h_checked) as i32,
            bad: self.h_bad,
            warn: self.h_warn,
        }
    }
}
