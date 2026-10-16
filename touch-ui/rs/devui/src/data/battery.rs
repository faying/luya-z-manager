//! The battery estimate's line (src/estimate.c, battery_est.c): zte-agent works it out
//! (battery_eta.rs); this reads GET /api/screen's battery every 10 s while 系统 is up and
//! words it; nothing read for 30 s: 「—」, not the old line as new.
// SPDX-License-Identifier: MIT

use super::core_api::CoreApi;
use super::util::{bytes_cap, mono_ms};
use crate::cf;
use crate::json;
use crate::lang::tr;

pub const EST_UNKNOWN: i32 = 0;
pub const EST_CHARGING: i32 = 1;
pub const EST_DISCHARGING: i32 = 2;
pub const EST_REACHED: i32 = 3;
pub const EST_PAUSED: i32 = 4;

const BE_POLL_MS: i64 = 10000;
const BE_STALE_MS: i64 = 30000;
const BE_IO_MS: i32 = 800;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Est {
    pub kind: i32,
    /// only for charging / discharging
    pub minutes: i64,
}

fn dur(m: i64) -> Vec<u8> {
    if m < 60 {
        cf!(tr("%ld 分钟"), m)
    } else if m % 60 == 0 {
        cf!(tr("%ld 小时"), m / 60)
    } else {
        cf!(tr("%ld 小时 %ld 分"), m / 60, m % 60)
    }
}

/// The line (est_text), at most cap - 1 bytes.
pub fn est_text(e: Est, target: i32, cap: usize) -> String {
    let d = || bytes_cap(&dur(e.minutes), 48);
    let v = match e.kind {
        EST_CHARGING => {
            if target >= 100 {
                cf!(tr("约 %s充满"), &d())
            } else {
                cf!(tr("约 %s充到 %d%%"), &d(), target)
            }
        }
        EST_DISCHARGING => cf!(tr("约可用 %s"), &d()),
        EST_REACHED => {
            if target >= 100 {
                tr("已充满").as_bytes().to_vec()
            } else {
                cf!(tr("已到上限 %d%%"), target)
            }
        }
        EST_PAUSED => tr("已到上限，暂停充电").as_bytes().to_vec(),
        _ => "—".as_bytes().to_vec(),
    };
    bytes_cap(&v, cap)
}

fn kind_of(name: &[u8]) -> i32 {
    match name {
        b"charging_eta" => EST_CHARGING,
        b"discharging_eta" => EST_DISCHARGING,
        b"reached_target" => EST_REACHED,
        b"paused_at_limit" => EST_PAUSED,
        _ => EST_UNKNOWN,
    }
}

/// /api/screen's battery object (est_from_report): (state was ok, the estimate, the target %;
/// 100 when unreadable).
pub fn est_from_report(obj: Option<&[u8]>) -> (bool, Est, i32) {
    let mut e = Est { kind: EST_UNKNOWN, minutes: 0 };
    let mut target = 100;
    let Some(obj) = obj else { return (false, e, target) };
    let t = json::get_int(obj, "target_pct", 100);
    if (1..=100).contains(&t) {
        target = t as i32;
    }
    if json::get(obj, "state", 16).is_none_or(|s| s != b"ok") {
        return (false, e, target);
    }
    let Some(kind) = json::get(obj, "kind", 24) else { return (true, e, target) };
    e.kind = kind_of(&kind);
    if e.kind == EST_CHARGING || e.kind == EST_DISCHARGING {
        e.minutes = json::get_int(obj, "minutes", -1);
        if e.minutes < 0 {
            e.kind = EST_UNKNOWN; // null / missing
        }
    }
    (true, e, target)
}

pub struct BatteryEst {
    text: String,
    was_active: bool,
    last_try: i64,
    last_ok: i64,
}

impl Default for BatteryEst {
    fn default() -> Self {
        BatteryEst { text: "—".into(), was_active: false, last_try: 0, last_ok: 0 }
    }
}

impl BatteryEst {
    fn set(&mut self, t: String) -> bool {
        if t == self.text {
            return false;
        }
        self.text = t;
        true
    }

    /// active = the line is on a lit screen. true = it changed.
    pub fn poll(&mut self, agent: &mut CoreApi, active: bool) -> bool {
        let t = mono_ms();
        let came_back = active && !self.was_active;
        self.was_active = active;
        if !active {
            return false;
        }
        if !came_back && self.last_try != 0 && t - self.last_try < BE_POLL_MS {
            // gone stale: no old line as new
            return self.last_ok != 0 && t - self.last_ok > BE_STALE_MS && self.set("—".into());
        }
        self.last_try = t;
        let (code, b) = agent.api_ms("GET", "/api/screen", None, 4096, BE_IO_MS);
        let bat = b.filter(|_| code == 200).and_then(|b| json::get(&b, "data", 1024)).and_then(|d| json::get(&d, "battery", 512));
        let Some(bat) = bat else {
            if self.last_ok != 0 && t - self.last_ok <= BE_STALE_MS {
                return false;
            }
            return self.set("—".into());
        };
        self.last_ok = t;
        let (_, e, target) = est_from_report(Some(&bat));
        self.set(est_text(e, target, 96))
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(obj: &str) -> String {
        let (_, e, t) = est_from_report(Some(obj.as_bytes()));
        est_text(e, t, 96)
    }

    // tests/estimate_test.c
    #[test]
    fn words_and_reports() {
        crate::lang::set_en(false);
        assert_eq!(est_text(Est { kind: EST_CHARGING, minutes: 198 }, 80, 96), "约 3 小时 18 分充到 80%");
        assert_eq!(est_text(Est { kind: EST_CHARGING, minutes: 45 }, 100, 96), "约 45 分钟充满");
        assert_eq!(est_text(Est { kind: EST_DISCHARGING, minutes: 120 }, 100, 96), "约可用 2 小时");
        assert_eq!(est_text(Est { kind: EST_REACHED, minutes: 0 }, 100, 96), "已充满");
        assert_eq!(est_text(Est { kind: EST_REACHED, minutes: 0 }, 80, 96), "已到上限 80%");
        assert_eq!(est_text(Est { kind: EST_PAUSED, minutes: 0 }, 80, 96), "已到上限，暂停充电");
        assert_eq!(est_text(Est { kind: EST_UNKNOWN, minutes: 0 }, 100, 96), "—");

        assert_eq!(text_of(r#"{"state":"ok","kind":"discharging_eta","minutes":891,"target_pct":100,"observed_at":1,"samples":36}"#), "约可用 14 小时 51 分");
        assert_eq!(text_of(r#"{"state":"ok","kind":"charging_eta","minutes":198,"target_pct":80}"#), "约 3 小时 18 分充到 80%");
        assert_eq!(text_of(r#"{"state":"ok","kind":"paused_at_limit","minutes":null,"target_pct":80}"#), "已到上限，暂停充电");
        assert_eq!(text_of(r#"{"state":"ok","kind":"reached_target","minutes":null,"target_pct":100}"#), "已充满");

        let r = |s: &str| est_from_report(Some(s.as_bytes()));
        let (ok, e, _) = r(r#"{"state":"stale","kind":"discharging_eta","minutes":300,"target_pct":100}"#);
        assert!(!ok && e.kind == EST_UNKNOWN, "stale is not shown");
        let (ok, e, _) = r(r#"{"state":"estimating","kind":"unknown","minutes":null,"target_pct":100}"#);
        assert!(!ok && e.kind == EST_UNKNOWN);
        let (ok, _, t) = r(r#"{"state":"unavailable","kind":"unknown","minutes":null,"target_pct":80}"#);
        assert!(!ok && t == 80, "unavailable keeps the limit");
        let (ok, e, _) = r(r#"{"state":"ok","kind":"charging_eta","minutes":null,"target_pct":100}"#);
        assert!(ok && e.kind == EST_UNKNOWN, "minutes null on an eta is unknown");
        let (ok, e, _) = r(r#"{"state":"ok","kind":"warp_speed","minutes":5,"target_pct":100}"#);
        assert!(ok && e.kind == EST_UNKNOWN, "a kind from a newer agent");
        let (ok, _, t) = r(r#"{"state":"ok","kind":"unknown","target_pct":500}"#);
        assert!(ok && t == 100, "a bad target reads as 100");
        let (ok, e, t) = est_from_report(None);
        assert!(!ok && e.kind == EST_UNKNOWN && t == 100);
        let (ok, e, _) = r("not json");
        assert!(!ok && e.kind == EST_UNKNOWN);
    }
}
