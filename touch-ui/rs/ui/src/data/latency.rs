//! The direct exit's latency for Home's CHILL lines: luyaz-core's `GET /v2/latency/direct` →
//! `{ok, data: {ms, at, ok, age_s, roaming}}` (core 1.50). The core probes only when asked and
//! answers every caller from one probe for 60 s, so this asks at most once a minute and only
//! while Home is up on a lit screen. An older core (404), a failed probe or no reply: None, and
//! the line says 「—」 as before.
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use super::core_api::CoreApi;
use super::util::{jtrue, mono_ms};
use crate::json;
use ui_model::direct_lat_t;

const POLL_MS: i64 = 60000;
/// the core's probe gives up after 3 s; a caller that comes while it runs waits for it
const IO_MS: i32 = 3500;
const RESP_MAX: usize = 1024;
/// a result older than this is not shown (the screen was off, or reads failed since)
pub const SHOW_S: i64 = 180;

/// The latency in a reply; None when the probe failed or the reply is not the shape.
pub fn parse(body: &[u8]) -> Option<direct_lat_t> {
    let d = json::get(body, "data", RESP_MAX)?;
    if !jtrue(&d, "ok") {
        return None;
    }
    let ms = json::get_int(&d, "ms", 0);
    let at = json::get_int(&d, "at", 0);
    (ms > 0 && ms < 60000 && at > 0).then_some(direct_lat_t { ms: ms as i32, at })
}

#[derive(Default, Clone)]
pub struct DirectLat {
    now: Option<direct_lat_t>,
    last_try: i64,
}

impl DirectLat {
    /// active = Home is up on a lit screen. true = what it shows changed.
    pub fn poll(&mut self, agent: &mut CoreApi, active: bool) -> bool {
        let t = mono_ms();
        // coming back within the minute keeps the last answer: the core would only repeat it
        if !active || (self.last_try != 0 && t - self.last_try < POLL_MS) {
            return false;
        }
        self.last_try = t;
        let (code, b) = agent.api_ms("GET", "/latency/direct", None, RESP_MAX, IO_MS);
        let got = b.filter(|_| code == 200).and_then(|b| parse(&b));
        if got == self.now {
            return false;
        }
        self.now = got;
        true
    }

    pub fn get(&self) -> Option<&direct_lat_t> {
        self.now.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_latency() {
        let b = br#"{"ok":true,"data":{"ms":42,"at":1790000000,"ok":true,"age_s":12,"roaming":false}}"#;
        assert_eq!(parse(b), Some(direct_lat_t { ms: 42, at: 1_790_000_000 }));
        // the probe failed: ms null, ok false
        assert!(parse(br#"{"ok":true,"data":{"ms":null,"at":1790000000,"ok":false,"age_s":0,"roaming":null}}"#).is_none());
        assert!(parse(br#"{"ok":false}"#).is_none());
        assert!(parse(b"Not Found").is_none());
    }
}
