//! Home's 「今天」 timeline: luyaz-core's `GET /v2/timeline/today` (core 1.50) →
//! `{ok, data: {available, clock_trusted, since, now, events: [{t, kind, text_zh, text_en, tone}],
//! hours: [{hour, sinr_avg, rat, link_down, …}]}}`, events oldest first. Read at most every
//! 5 minutes while Home is up on a lit screen (the core reads two files for it). An older core
//! (404), `available: false` or no reply: None, and the section is hidden.
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use super::core_api::CoreApi;
use super::util::{jstr, jtrue, mono_ms};
use crate::json;
use crate::lang;
use crate::logic::{NET_BAD, NET_NEUTRAL, NET_OK, NET_WARN};
use ui_model::{today_event_t, today_hour_t, today_t};

const POLL_MS: i64 = 300_000;
/// a failed read is tried again sooner
const RETRY_MS: i64 = 60_000;
const IO_MS: i32 = 1500;
/// up to 200 events with both languages, and 24 hours
const RESP_MAX: usize = 98304;
/// the newest this many are shown; the rest are counted (「还有 N 条」)
pub const TODAY_EVENTS: usize = 8;

fn tone(s: &str) -> i32 {
    match s {
        "ok" => NET_OK,
        "warn" => NET_WARN,
        "bad" => NET_BAD,
        _ => NET_NEUTRAL,
    }
}

/// The timeline in a reply; None when there is none to show.
pub fn parse(body: &[u8]) -> Option<today_t> {
    let d = json::get(body, "data", RESP_MAX)?;
    if !jtrue(&d, "available") {
        return None;
    }
    let mut o = today_t { clock_trusted: jtrue(&d, "clock_trusted") as i32, now: json::get_int(&d, "now", 0), ..Default::default() };
    let en = lang::is_en();
    let mut all: Vec<today_event_t> = Vec::new();
    if let Some(arr) = json::get(&d, "events", RESP_MAX)
        && arr.first() == Some(&b'[')
    {
        let mut p = 0;
        while let Some((e, np)) = json::arr_next(&arr, p, 1024) {
            p = np;
            if e.first() != Some(&b'{') {
                continue;
            }
            let zh = jstr(&e, "text_zh", 160);
            let en_t = jstr(&e, "text_en", 160);
            let text = if en && !en_t.is_empty() { en_t } else { zh };
            let t = json::get_int(&e, "t", 0);
            if text.is_empty() || t <= 0 {
                continue;
            }
            all.push(today_event_t { t, kind: jstr(&e, "kind", 24), text, tone: tone(&jstr(&e, "tone", 12)) });
        }
    }
    let n = all.len();
    o.more = n.saturating_sub(TODAY_EVENTS) as i32;
    o.events = all.into_iter().rev().take(TODAY_EVENTS).collect();
    if let Some(arr) = json::get(&d, "hours", 16384)
        && arr.first() == Some(&b'[')
    {
        let mut p = 0;
        while let Some((h, np)) = json::arr_next(&arr, p, 512) {
            p = np;
            let hour = json::get_int(&h, "hour", -1);
            if !(0..24).contains(&hour) {
                continue;
            }
            let sinr = json::get(&h, "sinr_avg", 24)
                .filter(|v| v.first().is_some_and(|c| c.is_ascii_digit() || *c == b'-'))
                .map(|v| json::atof(&v));
            o.hours.push(today_hour_t {
                hour: hour as i32,
                sinr,
                rat: jstr(&h, "rat", 12),
                link_down: json::get_int(&h, "link_down", 0).max(0) as i32,
            });
        }
    }
    Some(o)
}

#[derive(Default, Clone)]
pub struct Today {
    now: Option<today_t>,
    last_try: i64,
    failed: bool,
}

impl Today {
    /// active = Home is up on a lit screen. true = what it shows changed.
    pub fn poll(&mut self, agent: &mut CoreApi, active: bool) -> bool {
        let t = mono_ms();
        let wait = if self.failed { RETRY_MS } else { POLL_MS };
        if !active || (self.last_try != 0 && t - self.last_try < wait) {
            return false;
        }
        self.last_try = t;
        let (code, b) = agent.api_ms("GET", "/timeline/today", None, RESP_MAX, IO_MS);
        // unreachable or a timeout: keep what is shown (the card must not vanish under a finger);
        // an answer replaces it, `available: false` or 404 (an older core) hides it
        self.failed = code == 0 || code >= 500;
        if self.failed {
            return false;
        }
        let got = b.filter(|_| code == 200).and_then(|b| parse(&b));
        if got == self.now {
            return false;
        }
        self.now = got;
        true
    }

    pub fn get(&self) -> Option<&today_t> {
        self.now.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_timeline() {
        let mut ev = String::new();
        for i in 0..12 {
            ev.push_str(&format!(r#"{{"t":{},"kind":"link_down","text_zh":"断线 {i}","text_en":"Link down {i}","tone":"bad"}},"#, 1_790_000_000 + i));
        }
        ev.push_str(r#"{"t":1790000100,"kind":"scenario","text_zh":"切到 在家","text_en":"Switched to Home","tone":"ok"}"#);
        let b = format!(
            r#"{{"ok":true,"data":{{"available":true,"clock_trusted":true,"since":1789948800,"now":1790000200,"events":[{ev}],"hours":[{{"hour":0,"from":1,"to":2,"sinr_avg":12.5,"rat":"SA","link_down":1,"batt_to":null}},{{"hour":1,"sinr_avg":null,"rat":null,"link_down":0}},{{"hour":2,"sinr_avg":-3.0,"rat":"LTE"}}]}}}}"#
        );
        let v = parse(b.as_bytes()).unwrap();
        assert_eq!(v.clock_trusted, 1);
        assert_eq!(v.events.len(), TODAY_EVENTS);
        assert_eq!(v.more, 13 - TODAY_EVENTS as i32);
        // newest first
        assert_eq!((v.events[0].text.as_str(), v.events[0].tone, v.events[0].kind.as_str()), ("切到 在家", NET_OK, "scenario"));
        assert_eq!(v.events[1].text, "断线 11");
        assert_eq!(v.hours.len(), 3);
        assert_eq!((v.hours[0].hour, v.hours[0].sinr, v.hours[0].rat.as_str(), v.hours[0].link_down), (0, Some(12.5), "SA", 1));
        assert_eq!((v.hours[1].sinr, v.hours[1].rat.as_str()), (None, ""));
        assert_eq!(v.hours[2].sinr, Some(-3.0));
        // the clock not set: paused, nothing listed
        let v = parse(br#"{"ok":true,"data":{"available":true,"clock_trusted":false,"since":null,"events":[],"hours":[]}}"#).unwrap();
        assert_eq!((v.clock_trusted, v.events.len()), (0, 0));
        // the long-term layer off, or an older core
        assert!(parse(br#"{"ok":true,"data":{"available":false}}"#).is_none());
        assert!(parse(b"").is_none());
    }
}
