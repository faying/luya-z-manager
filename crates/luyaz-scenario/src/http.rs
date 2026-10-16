//! The engine's web routes (`/api/scenario*`), as `(status, body)`. The agent's tiny_http and
//! u60d's axum each mount them. Two of them do device work and block for a while — disabling
//! restores away (bootsafe: seconds, minutes when Wi-Fi needs repairing) and the scan (a few
//! seconds of `iw`) — so a host with a single runtime thread runs those off it.

use std::fs;

use serde_json::{json, Value};
use luyaz_policy::scenario::{self as sc, Config};

use crate::{atomic_write, lk, Engine, CONFIG_FILE, DIR, DISABLED_FLAG, LOG_FILE, LOG_TAIL_LINES, PIN_FILE};

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
        *lk(&self.cfg) = cfg.clone();
        self.write_json(CONFIG_FILE, &cfg);
        self.log("configuration replaced");
        self.ok_state()
    }

    /// GET /api/scenario/template — a starting configuration, with no home
    /// networks filled in so it cannot trigger until the user names one.
    pub fn http_template(&self) -> (u16, Value) {
        (200, json!({"ok": true, "data": sc::template()}))
    }

    /// POST /api/scenario/pin — body `{"id": "home"}` or `{"id": null}`.
    pub fn http_pin(&self, body: &[u8]) -> (u16, Value) {
        let Ok(parsed) = serde_json::from_slice::<Value>(body) else {
            return bad_json();
        };
        let _ = fs::create_dir_all(self.p(DIR));
        match parsed.get("id").and_then(|v| v.as_str()) {
            Some(id) => {
                if lk(&self.cfg).scenario(id).is_none() {
                    return (400, json!({"ok": false, "error": format!("unknown scenario {id:?}")}));
                }
                let _ = atomic_write(&self.p(PIN_FILE), id.as_bytes());
                self.log(&format!("pinned to {id:?}"));
            }
            None => {
                let _ = fs::remove_file(self.p(PIN_FILE));
                self.log("pin cleared");
            }
        }
        self.ok_state()
    }

    /// PUT /api/scenario/enabled — body `{"enabled": bool}`.
    ///
    /// Disabling does not freeze the device where it stands: it restores the away
    /// scenario first. Freezing would be a trap — disable it while the APs are off
    /// and you would have locked yourself out of the network you were trying to get
    /// back. Blocks for the bootsafe (see the module docs).
    pub fn http_enabled(&self, body: &[u8]) -> (u16, Value) {
        let Ok(parsed) = serde_json::from_slice::<Value>(body) else {
            return bad_json();
        };
        let enabled = parsed.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
        let _ = fs::create_dir_all(self.p(DIR));
        if enabled {
            let _ = fs::remove_file(self.p(DISABLED_FLAG));
            self.log("engine enabled");
        } else {
            let _ = fs::write(self.p(DISABLED_FLAG), "");
            self.log("engine disabled — restoring away scenario");
            self.bootsafe();
            self.run_restores(None, false);
        }
        self.ok_state()
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
        let lines: Vec<&str> = text.lines().collect();
        let start = lines.len().saturating_sub(LOG_TAIL_LINES);
        (200, json!({"ok": true, "data": {"log": lines[start..].join("\n")}}))
    }
}
