//! The `/api/sms/forward/*` handlers: the request body in, `(status, JSON)` out, as the agent
//! served them.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::*;

impl SmsForwarder {
    /// GET /api/sms/forward/config
    pub fn config_get(&self) -> (u16, Value) {
        let config = self.config.lock().unwrap();
        let fwd_state = self.state.lock().unwrap();
        (
            200,
            json!({
                "ok": true,
                "data": {
                    "config": *config,
                    "last_forwarded_id": fwd_state.last_forwarded_id,
                }
            }),
        )
    }

    /// PUT /api/sms/forward/config
    pub fn config_set(&self, body: &[u8]) -> (u16, Value) {
        #[derive(Deserialize)]
        struct Req {
            #[serde(default)]
            enabled: Option<bool>,
            #[serde(default)]
            poll_interval_secs: Option<u64>,
            #[serde(default)]
            mark_read_after_forward: Option<bool>,
            #[serde(default)]
            delete_after_forward: Option<bool>,
        }

        let req: Req = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(e) => return (400, json!({"ok": false, "error": format!("invalid JSON: {e}")})),
        };

        if req.poll_interval_secs.is_some_and(|interval| interval < 10) {
            return (400, json!({"ok": false, "error": "poll_interval_secs must be >= 10"}));
        }

        let mut config = self.config.lock().unwrap();
        let was_active = is_active(&config);

        if let Some(enabled) = req.enabled {
            config.enabled = enabled;
        }
        if let Some(interval) = req.poll_interval_secs {
            config.poll_interval_secs = interval;
        }
        if let Some(v) = req.mark_read_after_forward {
            config.mark_read_after_forward = v;
        }
        if let Some(v) = req.delete_after_forward {
            config.delete_after_forward = v;
        }

        commit_config(self, was_active, &config);
        (200, json!({"ok": true, "data": *config}))
    }

    /// POST /api/sms/forward/rules — Create a new rule
    pub fn rules_create(&self, body: &[u8]) -> (u16, Value) {
        #[derive(Deserialize)]
        struct Req {
            name: String,
            #[serde(default = "default_true")]
            enabled: bool,
            filter: SmsFilter,
            destination: ForwardDestination,
        }

        let req: Req = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(e) => return (400, json!({"ok": false, "error": format!("invalid JSON: {e}")})),
        };

        let mut config = self.config.lock().unwrap();
        let was_active = is_active(&config);
        let next_id = config.rules.iter().map(|r| r.id).max().unwrap_or(0) + 1;

        let rule = ForwardRule {
            id: next_id,
            name: req.name,
            enabled: req.enabled,
            filter: req.filter,
            destination: req.destination,
        };

        let result = json!({"ok": true, "data": rule});
        config.rules.push(rule);
        commit_config(self, was_active, &config);
        (201, result)
    }

    /// PUT /api/sms/forward/rules — Update a rule
    pub fn rules_update(&self, body: &[u8]) -> (u16, Value) {
        #[derive(Deserialize)]
        struct Req {
            id: u32,
            name: String,
            enabled: bool,
            filter: SmsFilter,
            destination: ForwardDestination,
        }

        let req: Req = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(e) => return (400, json!({"ok": false, "error": format!("invalid JSON: {e}")})),
        };

        let mut config = self.config.lock().unwrap();
        let was_active = is_active(&config);
        let rule = match config.rules.iter_mut().find(|r| r.id == req.id) {
            Some(r) => r,
            None => return (404, json!({"ok": false, "error": "rule not found"})),
        };

        rule.name = req.name;
        rule.enabled = req.enabled;
        rule.filter = req.filter;
        rule.destination = req.destination;

        let result = json!({"ok": true, "data": rule.clone()});
        commit_config(self, was_active, &config);
        (200, result)
    }

    /// DELETE /api/sms/forward/rules — Delete a rule
    pub fn rules_delete(&self, body: &[u8]) -> (u16, Value) {
        let parsed: Value = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
        };
        let id = match parsed["id"].as_u64() {
            Some(id) => id as u32,
            None => return (400, json!({"ok": false, "error": "missing 'id' field"})),
        };

        let mut config = self.config.lock().unwrap();
        let was_active = is_active(&config);
        let len_before = config.rules.len();
        config.rules.retain(|r| r.id != id);

        if config.rules.len() == len_before {
            return (404, json!({"ok": false, "error": "rule not found"}));
        }

        commit_config(self, was_active, &config);
        (200, json!({"ok": true}))
    }

    /// PUT /api/sms/forward/rules/toggle — Enable/disable a rule
    pub fn rules_toggle(&self, body: &[u8]) -> (u16, Value) {
        let parsed: Value = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
        };
        let id = match parsed["id"].as_u64() {
            Some(id) => id as u32,
            None => return (400, json!({"ok": false, "error": "missing 'id' field"})),
        };
        let enabled = match parsed["enabled"].as_bool() {
            Some(e) => e,
            None => return (400, json!({"ok": false, "error": "missing 'enabled' field"})),
        };

        let mut config = self.config.lock().unwrap();
        let was_active = is_active(&config);
        let rule = match config.rules.iter_mut().find(|r| r.id == id) {
            Some(r) => r,
            None => return (404, json!({"ok": false, "error": "rule not found"})),
        };

        rule.enabled = enabled;
        let result = json!({"ok": true, "data": rule.clone()});
        commit_config(self, was_active, &config);
        (200, result)
    }

    /// POST /api/sms/forward/test — Send a test message
    pub fn test_forward(&self, body: &[u8]) -> (u16, Value) {
        #[derive(Deserialize)]
        struct Req {
            destination: ForwardDestination,
        }

        let req: Req = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(e) => return (400, json!({"ok": false, "error": format!("invalid JSON: {e}")})),
        };

        let test_sms = DecodedSms {
            id: 0,
            sender: "+1234567890".to_string(),
            content: "This is a test message from the Luya-Z SMS forwarder.".to_string(),
            date: "2025-01-01 12:00:00".to_string(),
        };

        let agent = http_agent();
        match forward_to(&*self.host, &req.destination, &test_sms, &agent) {
            Ok(()) => (200, json!({"ok": true, "data": {"status": "sent"}})),
            Err(e) => (502, json!({"ok": false, "error": e})),
        }
    }

    /// GET /api/sms/forward/log — returns newest-first
    pub fn log_get(&self) -> (u16, Value) {
        let fwd_state = self.state.lock().unwrap();
        let mut log = fwd_state.log.clone();
        log.reverse();
        (200, json!({"ok": true, "data": log}))
    }

    /// POST /api/sms/forward/log/clear
    pub fn log_clear(&self) -> (u16, Value) {
        let mut fwd_state = self.state.lock().unwrap();
        fwd_state.log.clear();
        self.save_state(&fwd_state);
        (200, json!({"ok": true}))
    }

    /// POST /api/sms/forward/retry — Retry a failed forward log entry
    pub fn retry_forward(&self, body: &[u8]) -> (u16, Value) {
        #[derive(Deserialize)]
        struct Req {
            index: usize,
        }

        let req: Req = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(e) => return (400, json!({"ok": false, "error": format!("invalid JSON: {e}")})),
        };

        // Convert reversed index (newest-first) to internal chronological index
        let (entry_clone, rule_id, internal_index) = {
            let fwd_state = self.state.lock().unwrap();
            let internal_index = match fwd_state.log.len().checked_sub(1 + req.index) {
                Some(i) => i,
                None => return (404, json!({"ok": false, "error": "log entry not found"})),
            };
            let entry = match fwd_state.log.get(internal_index) {
                Some(e) => e,
                None => return (404, json!({"ok": false, "error": "log entry not found"})),
            };
            if entry.success {
                return (400, json!({"ok": false, "error": "entry already succeeded"}));
            }
            (entry.clone(), entry.rule_id, internal_index)
        };

        // Look up rule by id from current config
        let dest = {
            let config = self.config.lock().unwrap();
            match config.rules.iter().find(|r| r.id == rule_id) {
                Some(rule) => rule.destination.clone(),
                None => return (404, json!({"ok": false, "error": "rule no longer exists"})),
            }
        };

        // Reconstruct the SMS from stored log data
        let sms = DecodedSms {
            id: entry_clone.sms_id,
            sender: entry_clone.sender.clone(),
            content: entry_clone.content.clone(),
            date: entry_clone.date.clone(),
        };

        // Single attempt — no auto-retry for manual retries
        let agent = http_agent();
        let result = forward_to(&*self.host, &dest, &sms, &agent);

        // Update the log entry
        let mut guard = self.state.lock().unwrap();
        let fwd_state = &mut *guard;
        if let Some(entry) = fwd_state.log.get_mut(internal_index) {
            match &result {
                Ok(()) => {
                    entry.success = true;
                    entry.error = None;
                    entry.timestamp = now_ts();
                    // A held SMS must not go to this rule again automatically.
                    self.delivered.lock().unwrap().insert((entry.sms_id, rule_id));
                    if let Some(h) = fwd_state.held.as_mut().filter(|h| h.sms_id == entry.sms_id) {
                        if !h.delivered_rules.contains(&rule_id) {
                            h.delivered_rules.push(rule_id);
                        }
                    }
                }
                Err(e) => {
                    entry.error = Some(e.clone());
                    entry.timestamp = now_ts();
                }
            }
        }
        self.save_state(fwd_state);

        match result {
            Ok(()) => (200, json!({"ok": true, "data": {"status": "sent"}})),
            Err(e) => (502, json!({"ok": false, "error": e})),
        }
    }
}

fn default_true() -> bool {
    true
}
