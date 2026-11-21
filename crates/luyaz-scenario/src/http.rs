//! The engine's web routes (`/api/scenario*`), as `(status, body)`. The agent's tiny_http and
//! u60d's axum each mount them. The scan does device work and blocks for a while
//! (a few seconds of `iw`), so a host with a single runtime thread runs it off
//! that thread.

use std::fs;

use serde_json::{json, Value};
use luyaz_policy::scenario::{self as sc, Config};

use crate::{atomic_write, lk, remove_if_present, DisabledState, Engine, CONFIG_FILE, DIR, LOG_FILE, PIN_FILE};

fn bad_json() -> (u16, Value) {
    (400, json!({"ok": false, "error": "invalid JSON"}))
}

impl Engine {
    fn ok_state(&self) -> (u16, Value) {
        (200, json!({"ok": true, "data": self.state_json()}))
    }

    /// GET /api/scenario
    pub fn http_get(&self) -> (u16, Value) {
        self.ok_state()
    }

    /// PUT /api/scenario — replace the whole configuration.
    pub fn http_put(&self, body: &[u8]) -> (u16, Value) {
        let cfg: Config = match serde_json::from_slice(body) {
            Ok(c) => c,
            Err(e) => return (400, json!({"ok": false, "error": format!("invalid config: {e}")})),
        };
        if let Err(e) = sc::validate(&cfg) {
            return (400, json!({"ok": false, "error": e}));
        }
        let mut stored = lk(&self.cfg);
        if let Err(e) = self.write_json_result(CONFIG_FILE, &cfg) {
            return (500, json!({"ok": false, "error": format!("write config: {e}")}));
        }
        *stored = cfg;
        *lk(&self.cfg_seen) = crate::stamp(&self.p(CONFIG_FILE));
        drop(stored);
        self.log("configuration replaced");
        self.ok_state()
    }

    /// GET /api/scenario/template — a starting configuration, with no home
    /// networks filled in so it cannot trigger until the user names one.
    pub fn http_template(&self) -> (u16, Value) {
        let mut t = sc::template();
        // the first guess at "home" is the card in use; the page shows it and it can be changed
        if let Some(m) = self.io.sim_mcc() {
            t.home_mcc = m;
        }
        (200, json!({"ok": true, "data": t}))
    }

    /// POST /api/scenario/pin — body `{"id": "home"}` or `{"id": null}`.
    pub fn http_pin(&self, body: &[u8]) -> (u16, Value) {
        let Ok(parsed) = serde_json::from_slice::<Value>(body) else {
            return bad_json();
        };
        let Some(id) = parsed.get("id") else {
            return (400, json!({"ok": false, "error": "id must be a string or null"}));
        };
        let written = match id {
            Value::String(id) => {
                if lk(&self.cfg).scenario(id).is_none() {
                    return (400, json!({"ok": false, "error": format!("unknown scenario {id:?}")}));
                }
                fs::create_dir_all(self.p(DIR)).and_then(|()| atomic_write(&self.p(PIN_FILE), id.as_bytes()))
            }
            Value::Null => remove_if_present(&self.p(PIN_FILE)),
            _ => return (400, json!({"ok": false, "error": "id must be a string or null"})),
        };
        if let Err(e) = written {
            return (500, json!({"ok": false, "error": format!("write pin: {e}")}));
        }
        if let Err(e) = self.enable() {
            return (500, json!({"ok": false, "error": format!("enable scenario engine: {e}")}));
        }
        match id.as_str() {
            Some(id) => self.log(&format!("pinned to {id:?}")),
            None => self.log("pin cleared"),
        }
        self.ok_state()
    }

    /// PUT /api/scenario/enabled — body `{"enabled": bool}`.
    pub fn http_enabled(&self, body: &[u8]) -> (u16, Value) {
        let Ok(parsed) = serde_json::from_slice::<Value>(body) else {
            return bad_json();
        };
        let Some(enabled) = parsed.get("enabled").and_then(|v| v.as_bool()) else {
            return (400, json!({"ok": false, "error": "enabled must be a boolean"}));
        };
        let result = if enabled {
            self.enable()
        } else if self.disabled_state() == DisabledState::Paused {
            Ok(())
        } else {
            self.write_disabled_state(DisabledState::Stopping)
                .and_then(|()| self.clean_for_pause())
        };
        if let Err(e) = result {
            return (500, json!({"ok": false, "error": format!("change scenario mode: {e}")}));
        }
        self.ok_state()
    }

    fn enable(&self) -> std::io::Result<()> {
        if self.disabled_state() != DisabledState::Active {
            self.write_disabled_state(DisabledState::Active)?;
            self.log("engine enabled");
        }
        Ok(())
    }

    /// Complete the one-time shutdown cleanup after the last automatic tick has drained.
    /// Also used to recover an interrupted shutdown on the next Core start.
    pub fn clean_for_pause(&self) -> std::io::Result<()> {
        remove_if_present(&self.p(PIN_FILE))?;
        self.io.set_auto_sleep(true);
        self.clear_wakealarm();
        {
            let _restore_lock = lk(&crate::RESTORE_LOCK);
            self.write_json_result(crate::RESTORE_FILE, &[] as &[sc::PendingRestore; 0])?;
        }
        self.set_abroad(false);
        self.log("engine disabled; current device state retained");
        Ok(())
    }

    /// GET /api/scenario/scan — what the detector can see right now. The helper the
    /// admin UI needs to let someone pick their home network (with its BSSID)
    /// instead of typing a name. Blocks for the scan.
    pub fn http_scan(&self) -> (u16, Value) {
        match self.io.scan() {
            Ok(nets) => {
                let list: Vec<Value> = nets.iter().map(crate::net_json).collect();
                (200, json!({"ok": true, "data": {"networks": list}}))
            }
            Err(e) => (503, json!({"ok": false, "error": e})),
        }
    }

    /// GET /api/scenario/log
    pub fn http_log(&self) -> (u16, Value) {
        let text = fs::read_to_string(self.p(LOG_FILE)).unwrap_or_default();
        (200, json!({"ok": true, "data": {"log": crate::log_tail(&text)}}))
    }
}
