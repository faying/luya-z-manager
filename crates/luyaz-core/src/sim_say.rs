//! The one table of words for a SIM/eSIM switch (TODOS "P1 共用文案"): the core puts them in the
//! `sim` block (`say*`, from the `sim_identity` phase) and in the eSIM job
//! (`/api/esim/job`, from its status/reason); the admin web and the touch UI only display them
//! (same pattern as `luyaz_api::execution::say`). Phase names, epochs and reason codes stay in
//! the diagnostic views.

use serde_json::Value;

use crate::sim_identity::Phase;

/// `zh`/`en` in full (the admin web); `short`/`short_en` fit the touch screen's 150 px status
/// line (cj12, about 12 CJK characters) and are the same words, cut at the comma.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Say {
    pub id: &'static str,
    pub zh: &'static str,
    pub en: &'static str,
    pub short: &'static str,
    pub short_en: &'static str,
}

const fn one(id: &'static str, zh: &'static str, en: &'static str) -> Say {
    Say {
        id,
        zh,
        en,
        short: zh,
        short_en: en,
    }
}

pub const SWITCHING: Say = one("switching", "正在切换", "Switching");
pub const RETRYING: Say = one("retrying", "安全重试中", "Retrying safely");
pub const RECOVERED: Say = one("recovered", "已恢复", "Back online");
pub const CHECK_APN: Say = one("check_apn", "可能要查 APN", "May need an APN check");
pub const GAVE_UP: Say = Say {
    id: "gave_up",
    zh: "自动恢复未成功、设置没被再改",
    en: "Auto-recovery failed; settings left as they were",
    short: "自动恢复未成功",
    short_en: "Auto-recovery failed",
};

/// The `sim` block: only a transition has words; `online` says nothing (it would stay forever).
pub fn for_phase(p: Phase) -> Option<Say> {
    match p {
        Phase::Online => None,
        Phase::Leaving | Phase::Changed | Phase::Reloaded => Some(SWITCHING),
        Phase::Stalled => Some(GAVE_UP),
    }
}

/// The eSIM job slot (`data` of `/api/esim/job`): switch jobs only.
pub fn for_job(j: &Value) -> Option<Say> {
    if j["kind"] != "switch" {
        return None;
    }
    let reason = j["reason"].as_str().unwrap_or("");
    match j["status"].as_str()? {
        "running"
            if j["message"]
                .as_str()
                .is_some_and(|m| m.starts_with("card busy")) =>
        {
            Some(RETRYING)
        }
        "running" => Some(SWITCHING),
        "done" if j["rebooting"] == true => Some(SWITCHING),
        "done" => match reason {
            "no_data" => Some(CHECK_APN),
            "identity_stuck" => Some(GAVE_UP),
            _ => Some(RECOVERED),
        },
        "error" if reason == "cancelled" => None,
        "error" => Some(GAVE_UP),
        _ => None,
    }
}

/// Write `say`, `say_en`, `say_short`, `say_short_en`, `say_id` into `o` (all "" when there is
/// nothing to say).
pub fn stamp(o: &mut serde_json::Map<String, Value>, s: Option<Say>) {
    let s = s.unwrap_or(Say {
        id: "",
        zh: "",
        en: "",
        short: "",
        short_en: "",
    });
    for (k, v) in [
        ("say", s.zh),
        ("say_en", s.en),
        ("say_short", s.short),
        ("say_short_en", s.short_en),
        ("say_id", s.id),
    ] {
        o.insert(k.into(), v.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn job(status: &str, reason: &str, message: &str) -> Option<&'static str> {
        for_job(
            &json!({"kind": "switch", "status": status, "reason": reason,
            "message": message, "rebooting": false}),
        )
        .map(|s| s.zh)
    }

    #[test]
    fn phases() {
        assert_eq!(for_phase(Phase::Online), None);
        for p in [Phase::Leaving, Phase::Changed, Phase::Reloaded] {
            assert_eq!(for_phase(p), Some(SWITCHING));
        }
        assert_eq!(for_phase(Phase::Stalled), Some(GAVE_UP));
    }

    #[test]
    fn jobs() {
        assert_eq!(job("running", "", "switching"), Some("正在切换"));
        assert_eq!(
            job("running", "", "card busy — retrying (try 2 in 30 s)"),
            Some("安全重试中")
        );
        assert_eq!(job("done", "", ""), Some("已恢复"));
        assert_eq!(job("done", "no_data", ""), Some("可能要查 APN"));
        assert_eq!(job("done", "identity_stuck", ""), Some(GAVE_UP.zh));
        assert_eq!(job("error", "card_busy", "x"), Some(GAVE_UP.zh));
        assert_eq!(job("error", "not_switched", "x"), Some(GAVE_UP.zh));
        assert_eq!(job("error", "cancelled", "x"), None);
        assert_eq!(job("idle", "", ""), None);
        let reboot = json!({"kind": "switch", "status": "done", "reason": "", "rebooting": true});
        assert_eq!(for_job(&reboot), Some(SWITCHING));
        let dl = json!({"kind": "download", "status": "done", "reason": ""});
        assert_eq!(for_job(&dl), None);
    }

    #[test]
    fn stamps_empty_when_silent() {
        let mut o = serde_json::Map::new();
        stamp(&mut o, None);
        assert_eq!(
            Value::Object(o),
            json!({"say": "", "say_en": "", "say_short": "", "say_short_en": "", "say_id": ""})
        );
        let mut o = serde_json::Map::new();
        stamp(&mut o, Some(GAVE_UP));
        assert_eq!(o["say_short"], "自动恢复未成功");
        assert!(
            GAVE_UP.zh.starts_with(GAVE_UP.short),
            "short is the same words, cut"
        );
    }
}
