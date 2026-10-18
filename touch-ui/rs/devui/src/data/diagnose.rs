//! 网络诊断's requests to luyaz-core (src/diagnose.c): start a run (or join the one going),
//! read it (every second while it or its speed test runs, else 5 s), the capped speed test,
//! right / wrong. The run is parsed by diag_view.
// SPDX-License-Identifier: MIT

use super::core_api::CoreApi;
use super::util::{bytes_cap, jset, mono_ms, trunc};
use crate::cf;
use crate::diag_view::{self, DG_IDLE, DG_LAYERS_MAX, DG_NONE, DG_RUN, DG_RUNNING, DG_WAITING};
use crate::json;
use crate::lang::{self, tr};
use devui_model::{diag_run_t, dg_layer_t};

const DG_FAST_MS: i64 = 1000;
const DG_SLOW_MS: i64 = 5000;
/// the agent did not answer: don't stall every second
const DG_BACKOFF_MS: i64 = 5000;
const DG_RESP_MAX: usize = 8192;

pub struct Diagnose {
    run: diag_run_t,
    /// the agent has nothing to show (run: what it had)
    idle: bool,
    err: i32,
    refused: String,
    poll_ms: i64,
    kick: bool,
    sig: Option<(diag_run_t, i32, bool)>,
}

impl Default for Diagnose {
    fn default() -> Self {
        Diagnose {
            run: diag_run_t { layer: vec![dg_layer_t::default(); DG_LAYERS_MAX], ..Default::default() },
            idle: false,
            err: 0,
            refused: String::new(),
            poll_ms: 0,
            kick: true,
            sig: None,
        }
    }
}

impl Diagnose {
    fn going(&self) -> bool {
        let r = &self.run;
        r.state == DG_WAITING || r.state == DG_RUN || (r.has_speed != 0 && r.speed.level == DG_RUNNING)
    }

    /// code → err; a 2xx with a run in "data" → the run. true = taken.
    fn take(&mut self, code: i32, body: Option<&[u8]>) -> bool {
        self.err = match code {
            0 => 1,
            401 => 2,
            _ => 0,
        };
        let Some(b) = body.filter(|_| (200..300).contains(&code)) else { return false };
        let Some(data) = json::get(b, "data", DG_RESP_MAX) else { return false };
        let (ok, r) = diag_view::parse(&data);
        if !ok {
            return false;
        }
        // idle = none, or the last one over 10 minutes old: keep showing it
        self.idle = r.state == DG_IDLE;
        if !self.idle || self.run.state == DG_NONE {
            self.run = r;
        }
        true
    }

    fn refuse(&mut self, code: i32, body: Option<&[u8]>) {
        self.refused.clear();
        if (200..300).contains(&code) {
            return;
        }
        if code == 0 {
            self.refused = trunc(tr("后台没回应"), 160);
            return;
        }
        if let Some(b) = body {
            let (mut zh, mut en) = (String::new(), String::new());
            jset(b, "error", 160, &mut zh);
            jset(b, "error_en", 160, &mut en);
            if zh == "null" {
                zh.clear();
            }
            if en == "null" {
                en.clear();
            }
            self.refused = trunc(lang::pick(&zh, &en), 160);
        }
        if self.refused.is_empty() {
            self.refused = bytes_cap(&cf!(tr("被拒绝（HTTP %d）"), code), 160);
        }
    }

    /// active = the page is up on a lit screen. true = what it shows changed.
    pub fn poll(&mut self, agent: &mut CoreApi, active: bool) -> bool {
        let t = mono_ms();
        if !active {
            return false;
        }
        let gap = if self.err != 0 {
            DG_BACKOFF_MS
        } else if self.going() {
            DG_FAST_MS
        } else {
            DG_SLOW_MS
        };
        if !self.kick && self.poll_ms != 0 && t - self.poll_ms < gap {
            return false;
        }
        self.kick = false;
        self.poll_ms = t;
        let (code, body) = agent.api("GET", "/api/diagnose", None, DG_RESP_MAX);
        let body = body.filter(|_| code == 200);
        self.take(code, body.as_deref());
        let sig = (self.run.clone(), self.err, self.idle);
        if self.sig.as_ref() == Some(&sig) {
            return false;
        }
        self.sig = Some(sig);
        true
    }

    pub fn kick(&mut self) {
        self.kick = true;
    }
    pub fn run(&self) -> &diag_run_t {
        &self.run
    }
    pub fn agent_err(&self) -> i32 {
        self.err
    }
    pub fn idle(&self) -> bool {
        self.idle || self.run.state == DG_NONE || self.run.state == DG_IDLE
    }
    pub fn error(&self) -> &str {
        &self.refused
    }

    /// POST /api/diagnose: start, or join the one going (the reply's run taken at once).
    pub fn start(&mut self, agent: &mut CoreApi) -> bool {
        let (code, body) = agent.api("POST", "/api/diagnose", Some("{}"), DG_RESP_MAX);
        self.refuse(code, body.as_deref());
        self.sig = None;
        self.poll_ms = mono_ms(); // the reply carries the run: the next read in a second
        self.take(code, body.as_deref())
    }

    pub fn speed(&mut self, agent: &mut CoreApi) -> bool {
        let js = format!("{{\"id\":{}}}", self.run.id);
        let (code, body) = agent.api("POST", "/api/diagnose/speed", Some(&js), DG_RESP_MAX);
        self.refuse(code, body.as_deref());
        self.sig = None;
        self.poll_ms = mono_ms();
        if (200..300).contains(&code) {
            self.take(code, body.as_deref());
            return true;
        }
        self.err = match code {
            0 => 1,
            401 => 2,
            _ => 0,
        };
        false
    }

    pub fn feedback(&mut self, agent: &mut CoreApi, right: bool) -> bool {
        let js = format!("{{\"id\":{},\"right\":{right}}}", self.run.id);
        let (code, _) = agent.api("POST", "/api/diagnose/feedback", Some(&js), DG_RESP_MAX);
        self.err = match code {
            0 => 1,
            401 => 2,
            _ => 0,
        };
        if code != 200 {
            return false;
        }
        self.run.feedback = right as i32;
        self.sig = None;
        true
    }
}
