//! 网络诊断's requests to luyaz-core (src/diagnose.c): start a run (or join the one going),
//! read it (every second while it or its speed test runs, else 5 s), the capped speed test,
//! right / wrong. The run is parsed by diag_view.
// SPDX-License-Identifier: MIT

use super::core_api::CoreApi;
use super::util::{bytes_cap, jset, jstr, jtrue, mono_ms, objects, trunc};
use crate::cf;
use crate::diag_view::{self, DG_IDLE, DG_LAYERS_MAX, DG_NONE, DG_RUN, DG_RUNNING, DG_WAITING};
use crate::json;
use crate::lang::{self, tr};
use devui_model::{diag_run_t, dg_layer_t, dg_proposal_t, dg_restore_t};

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
    sig: Option<(diag_run_t, i32, bool, Option<dg_proposal_t>, String, Option<dg_restore_t>)>,
    /// the core's pending redial proposal (`data.proposal`)
    proposal: Option<dg_proposal_t>,
    /// the core's words on a strong but slow signal (`data.slow_signal.say`), this language
    slow: String,
    /// `GET /api/verified`, read with the run
    restore: Option<dg_restore_t>,
    /// when it was read: every 5 s, every read while a restore runs (it reads the radio)
    restore_ms: i64,
}

/// `GET /api/verified` → what the page shows: None when there is nothing to restore and no run
/// to report (no record here, already there, no country).
pub fn restore_of(v: &[u8]) -> Option<dg_restore_t> {
    let mut r = dg_restore_t { avail: jtrue(v, "available"), ..Default::default() };
    if let Some(s) = json::get(v, "steps", 8192) {
        r.items = objects(&s).iter().map(|o| jstr(o, "item", 32)).filter(|i| !i.is_empty()).collect();
    }
    if let Some(p) = json::get(v, "progress", 4096).filter(|p| p != b"null") {
        let steps = json::get(&p, "steps", 4096).unwrap_or_default();
        let st = objects(&steps);
        r.total = st.len() as i32;
        r.done = st.iter().filter(|o| jstr(o, "state", 16) == "done").count() as i32;
        r.failed = st.iter().find(|o| jstr(o, "state", 16) == "failed").map(|o| jstr(o, "item", 32)).unwrap_or_default();
        r.state = match jstr(&p, "end", 32).as_str() {
            "" => 1,
            "restored" => 2,
            "restored_still_down" => 3,
            _ => 4,
        };
    }
    (r.state != 0 || (r.avail && !r.items.is_empty())).then_some(r)
}

/// `data.slow_signal.say`'s detail in the screen's language; empty when there is nothing to say.
pub fn slow_of(data: &[u8]) -> String {
    let Some(s) = json::get(data, "slow_signal", 4096).and_then(|s| json::get(&s, "say", 2048)).filter(|s| s != b"null") else {
        return String::new();
    };
    if lang::is_en() {
        let en = jstr(&s, "detail_en", 320);
        if !en.is_empty() {
            return en;
        }
    }
    jstr(&s, "detail", 320)
}

/// `data.proposal` when it is a complete redial (no button for an action the screen can't name).
pub fn proposal_of(data: &[u8]) -> Option<dg_proposal_t> {
    let p = json::get(data, "proposal", 1024).filter(|p| p != b"null")?;
    if jstr(&p, "action", 64) != "data.reconnect" {
        return None;
    }
    let (id, digest) = (jstr(&p, "proposal_id", 64), jstr(&p, "params_digest", 64));
    if id.is_empty() || digest.is_empty() {
        return None;
    }
    Some(dg_proposal_t { id, digest, tail: jstr(&p, "iccid_tail", 8) })
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
            proposal: None,
            slow: String::new(),
            restore: None,
            restore_ms: 0,
        }
    }
}

impl Diagnose {
    fn going(&self) -> bool {
        let r = &self.run;
        r.state == DG_WAITING
            || r.state == DG_RUN
            || (r.has_speed != 0 && r.speed.level == DG_RUNNING)
            || self.restore.as_ref().is_some_and(|v| v.state == 1)
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
        self.proposal = proposal_of(&data);
        self.slow = slow_of(&data);
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
        if code != 200 {
            self.proposal = None;
            self.slow.clear();
        }
        if self.restore.as_ref().is_some_and(|v| v.state == 1) || t - self.restore_ms >= DG_SLOW_MS || self.restore_ms == 0 {
            self.restore_ms = t;
            let (vc, vb) = agent.api("GET", "/api/verified", None, DG_RESP_MAX);
            self.restore = vb.filter(|_| vc == 200).and_then(|b| restore_of(&b));
        }
        let sig = (self.run.clone(), self.err, self.idle, self.proposal.clone(), self.slow.clone(), self.restore.clone());
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
    pub fn proposal(&self) -> Option<&dg_proposal_t> {
        self.proposal.as_ref()
    }
    pub fn restore(&self) -> Option<&dg_restore_t> {
        self.restore.as_ref()
    }
    pub fn slow(&self) -> &str {
        &self.slow
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_slow_line_is_the_cores_words() {
        let d = r#"{"state":"idle","slow_signal":{"advice":{"advice":"suggest","busy_s":90},"say":{"id":"slow_signal","detail":"信号强","detail_en":"Strong"}}}"#;
        assert_eq!(slow_of(d.as_bytes()), "信号强");
        assert_eq!(slow_of(br#"{"slow_signal":{"advice":{"advice":"unknown"},"say":null}}"#), "");
        assert_eq!(slow_of(br#"{"state":"idle"}"#), "");
    }

    #[test]
    fn the_restore_shows_what_differs_and_how_the_run_goes() {
        let v = br#"{"available":true,"why":null,"steps":[{"item":"band.nr","to":{"a":[1]}},{"item":"apn","to":{"mode":1}}],"progress":null}"#;
        let r = restore_of(v).unwrap();
        assert_eq!((r.avail, r.items.join(","), r.state), (true, "band.nr,apn".into(), 0));
        assert!(restore_of(br#"{"available":false,"why":"already_there","steps":[],"progress":null}"#).is_none());
        let run = br#"{"available":false,"why":"busy","steps":[],"progress":{"steps":[{"item":"band.nr","state":"done"},{"item":"apn","state":"running"}],"end":null}}"#;
        let r = restore_of(run).unwrap();
        assert_eq!((r.state, r.done, r.total), (1, 1, 2));
        let stop = br#"{"available":true,"steps":[{"item":"apn"}],"progress":{"steps":[{"item":"band.nr","state":"done"},{"item":"apn","state":"failed","why":"apn_profile_gone"}],"end":"stopped"}}"#;
        let r = restore_of(stop).unwrap();
        assert_eq!((r.state, r.failed.as_str()), (4, "apn"));
    }

    #[test]
    fn only_a_complete_redial_proposal_is_kept() {
        let d = br#"{"state":"idle","proposal":{"proposal_id":"ab12","params_digest":"d0","action":"data.reconnect","iccid_tail":"1234"}}"#;
        let p = proposal_of(d).unwrap();
        assert_eq!((p.id.as_str(), p.digest.as_str(), p.tail.as_str()), ("ab12", "d0", "1234"));
        assert!(proposal_of(br#"{"proposal":null}"#).is_none());
        assert!(proposal_of(br#"{"state":"idle"}"#).is_none());
        assert!(proposal_of(br#"{"proposal":{"proposal_id":"ab12","params_digest":"d0","action":"network.mode"}}"#).is_none());
        assert!(proposal_of(br#"{"proposal":{"proposal_id":"","params_digest":"d0","action":"data.reconnect"}}"#).is_none());
    }
}
