//! 流量 page (tapped from Home's 今日 cell): the core's usage ledger (manager
//! docs/designs/travel-assist.md §4), `GET /v2/usage/days?n=30` → `{ok, data: {days: [{day,
//! cards: [{card, tail, op, rx, tx}]}]}}`, newest first, today included. Days with no ledger
//! (device off, clock not set) have no entry, so the page matches them by date, not by place.
//! Read when the page comes up and every 10 min while it stays up; nothing else reads it.
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use super::core_api::CoreApi;
use super::util::mono_ms;
use crate::json;
use ui_model::{usage_card_t, usage_day_t};

pub const DAYS: usize = 30;
const POLL_MS: i64 = 600_000;
const IO_MS: i32 = 800;
const RESP_MAX: usize = 24576;
/// cards kept per day (a day with more lists the busiest)
const CARDS: usize = 4;

fn jstr(o: &[u8], k: &str, max: usize) -> String {
    json::get_str(o, k, max).unwrap_or_default()
}

/// The days in a reply; None when the reply has no `days` (an older core, an error).
pub fn parse(body: &[u8]) -> Option<Vec<usage_day_t>> {
    let d = json::get(body, "data", RESP_MAX)?;
    let arr = json::get(&d, "days", RESP_MAX).filter(|a| a.first() == Some(&b'['))?;
    let mut out = Vec::new();
    let mut p = 0;
    while let Some((day, np)) = json::arr_next(&arr, p, RESP_MAX) {
        p = np;
        let name = jstr(&day, "day", 12);
        if name.len() != 10 {
            continue;
        }
        let mut cards = Vec::new();
        if let Some(ca) = json::get(&day, "cards", RESP_MAX).filter(|a| a.first() == Some(&b'[')) {
            let mut q = 0;
            while let Some((c, nq)) = json::arr_next(&ca, q, 512) {
                q = nq;
                if c.first() != Some(&b'{') {
                    continue;
                }
                let bytes = json::get_int(&c, "rx", 0).max(0) + json::get_int(&c, "tx", 0).max(0);
                cards.push(usage_card_t { card: jstr(&c, "card", 24), tail: jstr(&c, "tail", 8), op: jstr(&c, "op", 24), bytes });
            }
        }
        cards.sort_by_key(|c| std::cmp::Reverse(c.bytes));
        cards.truncate(CARDS);
        out.push(usage_day_t { day: name, cards });
        if out.len() == DAYS {
            break;
        }
    }
    Some(out)
}

#[derive(Default, Clone)]
pub struct Usage {
    now: Vec<usage_day_t>,
    was_active: bool,
    last_try: i64,
}

impl Usage {
    /// active = the 流量 page is up. true = what it shows changed.
    pub fn poll(&mut self, agent: &mut CoreApi, active: bool) -> bool {
        let t = mono_ms();
        let came_back = active && !self.was_active;
        self.was_active = active;
        if !active || (!came_back && self.last_try != 0 && t - self.last_try < POLL_MS) {
            return false;
        }
        self.last_try = t;
        let (code, b) = agent.api_ms("GET", &format!("/usage/days?n={DAYS}"), None, RESP_MAX, IO_MS);
        // unreachable: keep what is shown; 404 (an older core): nothing to show
        if code == 0 || code >= 500 {
            return false;
        }
        let got = b.filter(|_| code == 200).and_then(|b| parse(&b)).unwrap_or_default();
        if got == self.now {
            return false;
        }
        self.now = got;
        true
    }

    pub fn get(&self) -> &[usage_day_t] {
        &self.now
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_days() {
        let b = br#"{"ok":true,"data":{"days":[
            {"day":"2026-10-27","cards":[{"card":"a1","tail":"1234","op":"CMCC","rx":100,"tx":20}]},
            {"day":"2026-10-25","cards":[{"card":"a1","tail":"1234","op":"CMCC","rx":5,"tx":1},
                                         {"card":"b2","tail":"9876","op":"","rx":3000000,"tx":1000000}]},
            {"day":"bad","cards":[]},
            {"day":"2026-10-24","cards":[]},
            {"day":"2026-10-23","cards":[]}]}}"#;
        let d = parse(b).unwrap();
        assert_eq!(d.len(), 4);
        assert_eq!(d[0].cards[0].card, "a1");
        assert_eq!((d[0].day.as_str(), d[0].cards[0].bytes, d[0].cards[0].op.as_str()), ("2026-10-27", 120, "CMCC"));
        // the busiest card first
        assert_eq!((d[1].cards[0].tail.as_str(), d[1].cards[0].bytes, d[1].cards[1].bytes), ("9876", 4_000_000, 6));
        assert_eq!((d[2].day.as_str(), d[3].day.as_str()), ("2026-10-24", "2026-10-23"));
        assert!(parse(br#"{"ok":true,"data":{"days":[]}}"#).unwrap().is_empty());
        assert!(parse(br#"{"ok":false,"error":"x"}"#).is_none());
    }
}
