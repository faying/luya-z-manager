//! 落地 (manager docs/designs/travel-assist.md §5): the core's last landing,
//! `GET /v2/usage/landing` → `{ok, data: {mcc, from, at, rx, tx} | null}`, every 60 s while Home is
//! up. `from` null is the first record (where the device started), not a landing.
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use super::core_api::CoreApi;
use super::util::mono_ms;
use crate::json;
use ui_model::landing_t;

const POLL_MS: i64 = 60000;
const IO_MS: i32 = 800;
const RESP_MAX: usize = 1024;

/// The landing in a reply; None when there is none to show (no record, or the first one).
pub fn parse(body: &[u8]) -> Option<landing_t> {
    let d = json::get(body, "data", RESP_MAX)?;
    let from = json::get_int(&d, "from", 0);
    let l = landing_t {
        mcc: json::get_int(&d, "mcc", 0) as i32,
        from: from as i32,
        at: json::get_int(&d, "at", 0),
        rx: json::get_int(&d, "rx", 0),
        tx: json::get_int(&d, "tx", 0),
    };
    (l.mcc > 0 && l.from > 0 && l.at > 0).then_some(l)
}

#[derive(Default)]
#[derive(Clone)]
pub struct Landing {
    now: Option<landing_t>,
    was_active: bool,
    last_try: i64,
}

impl Landing {
    /// active = Home is on screen. true = what it shows changed.
    pub fn poll(&mut self, agent: &mut CoreApi, active: bool) -> bool {
        let t = mono_ms();
        let came_back = active && !self.was_active;
        self.was_active = active;
        if !active || (!came_back && self.last_try != 0 && t - self.last_try < POLL_MS) {
            return false;
        }
        self.last_try = t;
        let (code, b) = agent.api_ms("GET", "/usage/landing", None, RESP_MAX, IO_MS);
        // unreachable or an older core (404): nothing to show
        let got = b.filter(|_| code == 200).and_then(|b| parse(&b));
        if got == self.now {
            return false;
        }
        self.now = got;
        true
    }

    pub fn get(&self) -> Option<&landing_t> {
        self.now.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_landing() {
        let b = br#"{"ok":true,"data":{"mcc":440,"from":460,"at":1790000000,"rx":5000000,"tx":1000000}}"#;
        let l = parse(b).unwrap();
        assert_eq!((l.mcc, l.from, l.at, l.rx, l.tx), (440, 460, 1_790_000_000, 5_000_000, 1_000_000));
        // the first record (where it started) is not a landing
        assert!(parse(br#"{"ok":true,"data":{"mcc":460,"from":null,"at":1790000000,"rx":0,"tx":0}}"#).is_none());
        assert!(parse(br#"{"ok":true,"data":null}"#).is_none());
        assert!(parse(b"{}").is_none());
    }
}
