//! HTTP face of the forwarder: each `/api/sms/forward/*` route takes the raw request body and
//! returns `(status, JSON)`; the core's router only wires them up.

use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::*;

/// `{"ok": false, "error": …}` with a status.
fn fail(status: u16, why: impl Into<Value>) -> (u16, Value) {
    (status, json!({ "ok": false, "error": why.into() }))
}

/// `{"ok": true, "data": …}` with a status.
fn done(status: u16, data: impl serde::Serialize) -> (u16, Value) {
    (status, json!({ "ok": true, "data": data }))
}

/// A typed body, or the 400 that says why it is not one.
fn typed<T: DeserializeOwned>(body: &[u8]) -> Result<T, (u16, Value)> {
    serde_json::from_slice(body).map_err(|e| fail(400, format!("invalid JSON: {e}")))
}

/// A loose body (`{"id": …}` style), or a bare "invalid JSON" 400.
fn loose(body: &[u8]) -> Result<Value, (u16, Value)> {
    serde_json::from_slice(body).map_err(|_| fail(400, "invalid JSON"))
}

/// The rule id a loose body names (truncated to `u32` like every id here).
fn rule_id_in(v: &Value) -> Result<u32, (u16, Value)> {
    v.get("id")
        .and_then(Value::as_u64)
        .map(|n| n as u32)
        .ok_or_else(|| fail(400, "missing 'id' field"))
}

fn no_rule() -> (u16, Value) {
    fail(404, "rule not found")
}

fn sent() -> (u16, Value) {
    done(200, json!({ "status": "sent" }))
}

/// Settings fields a caller may change; anything left out stays.
#[derive(Deserialize)]
struct SettingsPatch {
    /// master switch
    enabled: Option<bool>,
    /// at least 10
    poll_interval_secs: Option<u64>,
    /// tidy the device's copy after a full success
    mark_read_after_forward: Option<bool>,
    delete_after_forward: Option<bool>,
}

/// A new rule (the id is handed out here). `enabled` defaults to on.
#[derive(Deserialize)]
struct NewRule {
    destination: ForwardDestination,
    filter: SmsFilter,
    name: String,
    #[serde(default = "on")]
    enabled: bool,
}

fn on() -> bool {
    true
}

/// A full replacement of an existing rule's editable fields.
#[derive(Deserialize)]
struct RuleEdit {
    id: u32,
    enabled: bool,
    name: String,
    destination: ForwardDestination,
    filter: SmsFilter,
}

#[derive(Deserialize)]
struct TestSend {
    destination: ForwardDestination,
}

/// Position in the newest-first log the UI shows.
#[derive(Deserialize)]
struct LogPick {
    index: usize,
}

impl SmsForwarder {
    /// Edits the config under its lock and saves it if `edit` says so (`Ok`); a switch from
    /// idle to running resets the baseline (see [`commit_config`]).
    fn edit_config(
        &self,
        edit: impl FnOnce(&mut SmsForwardConfig) -> Result<(u16, Value), (u16, Value)>,
    ) -> (u16, Value) {
        let mut cfg = self.config.lock().unwrap();
        let running_before = is_active(&cfg);
        match edit(&mut cfg) {
            Ok(reply) => {
                commit_config(self, running_before, &cfg);
                reply
            }
            Err(reply) => reply,
        }
    }

    /// GET /api/sms/forward/config
    pub fn config_get(&self) -> (u16, Value) {
        let cfg = self.config.lock().unwrap().clone();
        let watermark = self.state.lock().unwrap().last_forwarded_id;
        let queue = self.queue.lock().unwrap().status();
        done(
            200,
            json!({ "config": cfg, "last_forwarded_id": watermark, "queue": queue }),
        )
    }

    /// PUT /api/sms/forward/config
    pub fn config_set(&self, body: &[u8]) -> (u16, Value) {
        let patch: SettingsPatch = match typed(body) {
            Ok(p) => p,
            Err(r) => return r,
        };
        if matches!(patch.poll_interval_secs, Some(s) if s < 10) {
            return fail(400, "poll_interval_secs must be >= 10");
        }
        self.edit_config(|cfg| {
            cfg.enabled = patch.enabled.unwrap_or(cfg.enabled);
            cfg.poll_interval_secs = patch.poll_interval_secs.unwrap_or(cfg.poll_interval_secs);
            cfg.mark_read_after_forward = patch
                .mark_read_after_forward
                .unwrap_or(cfg.mark_read_after_forward);
            cfg.delete_after_forward = patch
                .delete_after_forward
                .unwrap_or(cfg.delete_after_forward);
            Ok(done(200, &*cfg))
        })
    }

    /// POST /api/sms/forward/rules
    pub fn rules_create(&self, body: &[u8]) -> (u16, Value) {
        let new: NewRule = match typed(body) {
            Ok(n) => n,
            Err(r) => return r,
        };
        self.edit_config(|cfg| {
            let id = 1 + cfg.rules.iter().map(|r| r.id).max().unwrap_or(0);
            let NewRule {
                name,
                enabled,
                filter,
                destination,
            } = new;
            let rule = ForwardRule {
                id,
                destination,
                filter,
                enabled,
                name,
            };
            let reply = done(201, &rule);
            cfg.rules.push(rule);
            Ok(reply)
        })
    }

    /// PUT /api/sms/forward/rules
    pub fn rules_update(&self, body: &[u8]) -> (u16, Value) {
        let edit: RuleEdit = match typed(body) {
            Ok(e) => e,
            Err(r) => return r,
        };
        self.edit_config(|cfg| {
            let rule = cfg
                .rules
                .iter_mut()
                .find(|r| r.id == edit.id)
                .ok_or_else(no_rule)?;
            *rule = ForwardRule {
                id: edit.id,
                name: edit.name,
                enabled: edit.enabled,
                filter: edit.filter,
                destination: edit.destination,
            };
            Ok(done(200, &*rule))
        })
    }

    /// DELETE /api/sms/forward/rules
    pub fn rules_delete(&self, body: &[u8]) -> (u16, Value) {
        let id = match loose(body).and_then(|v| rule_id_in(&v)) {
            Ok(id) => id,
            Err(r) => return r,
        };
        self.edit_config(|cfg| {
            let before = cfg.rules.len();
            cfg.rules.retain(|r| r.id != id);
            if cfg.rules.len() == before {
                return Err(no_rule());
            }
            Ok((200, json!({ "ok": true })))
        })
    }

    /// PUT /api/sms/forward/rules/toggle
    pub fn rules_toggle(&self, body: &[u8]) -> (u16, Value) {
        let picked = loose(body).and_then(|v| {
            let id = rule_id_in(&v)?;
            let on = v
                .get("enabled")
                .and_then(Value::as_bool)
                .ok_or_else(|| fail(400, "missing 'enabled' field"))?;
            Ok((id, on))
        });
        let (id, on) = match picked {
            Ok(p) => p,
            Err(r) => return r,
        };
        self.edit_config(|cfg| {
            let rule = cfg
                .rules
                .iter_mut()
                .find(|r| r.id == id)
                .ok_or_else(no_rule)?;
            rule.enabled = on;
            Ok(done(200, &*rule))
        })
    }

    /// POST /api/sms/forward/test: one made-up SMS to the given destination, no retries.
    pub fn test_forward(&self, body: &[u8]) -> (u16, Value) {
        let TestSend { destination } = match typed(body) {
            Ok(t) => t,
            Err(r) => return r,
        };
        let sample = DecodedSms {
            id: 0,
            date: "2025-01-01 12:00:00".into(),
            sender: "+1234567890".into(),
            content: "This is a test message from the Luya-Z SMS forwarder.".into(),
            late_secs: None,
        };
        match forward_to(&*self.host, &destination, &sample, &http_agent()) {
            Ok(()) => sent(),
            Err(e) => fail(502, e),
        }
    }

    /// GET /api/sms/forward/log (newest first)
    pub fn log_get(&self) -> (u16, Value) {
        let newest_first: Vec<ForwardLogEntry> = self
            .state
            .lock()
            .unwrap()
            .log
            .iter()
            .rev()
            .cloned()
            .collect();
        done(200, newest_first)
    }

    /// POST /api/sms/forward/log/clear
    pub fn log_clear(&self) -> (u16, Value) {
        let mut st = self.state.lock().unwrap();
        st.log.clear();
        self.save_state(&st);
        (200, json!({ "ok": true }))
    }

    /// POST /api/sms/forward/retry: one manual attempt for a failed log line (`index` counts
    /// from the newest). Success also marks the rule as done for a held SMS.
    pub fn retry_forward(&self, body: &[u8]) -> (u16, Value) {
        let LogPick { index } = match typed(body) {
            Ok(p) => p,
            Err(r) => return r,
        };
        let (slot, line) = {
            let st = self.state.lock().unwrap();
            let found = st
                .log
                .len()
                .checked_sub(index + 1)
                .and_then(|slot| st.log.get(slot).map(|l| (slot, l.clone())));
            match found {
                None => return fail(404, "log entry not found"),
                Some((_, l)) if l.success => return fail(400, "entry already succeeded"),
                Some(hit) => hit,
            }
        };
        let rule_id = line.rule_id;
        let target = self
            .config
            .lock()
            .unwrap()
            .rules
            .iter()
            .find(|r| r.id == rule_id)
            .map(|r| r.destination.clone());
        let Some(target) = target else {
            return fail(404, "rule no longer exists");
        };
        let again = DecodedSms {
            id: line.sms_id,
            sender: line.sender,
            content: line.content,
            date: line.date,
            late_secs: None,
        };
        let outcome = forward_to(&*self.host, &target, &again, &http_agent());

        let mut st = self.state.lock().unwrap();
        let st = &mut *st;
        if let Some(l) = st.log.get_mut(slot) {
            l.timestamp = now_ts();
            l.error = outcome.as_ref().err().cloned();
            if outcome.is_ok() {
                l.success = true;
                // the held SMS must not be sent to this rule again by the loop
                self.delivered.lock().unwrap().insert((l.sms_id, rule_id));
                let mut q = self.queue.lock().unwrap();
                let mut hit = false;
                for j in q.jobs.iter_mut().filter(|j| j.sms_id == l.sms_id && j.rule_id == rule_id && j.state != crate::JobState::Sent) {
                    j.state = crate::JobState::Sent;
                    j.last_error = None;
                    hit = true;
                }
                if hit {
                    self.save_queue(&q);
                }
                if let Some(h) = st.held.as_mut() {
                    if h.sms_id == l.sms_id && !h.delivered_rules.contains(&rule_id) {
                        h.delivered_rules.push(rule_id);
                    }
                }
            }
        }
        self.save_state(st);
        match outcome {
            Ok(()) => sent(),
            Err(e) => fail(502, e),
        }
    }
}
