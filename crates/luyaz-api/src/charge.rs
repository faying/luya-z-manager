//! The charge limit's settings and `/api/device/charge-control`, shared by the agent and u60d
//!. Whoever owns the `charge` part keeps the settings and the
//! manual override and writes the file; this is only the file format and the route's request and
//! reply shapes.
//!
//! Naming trap (RESIDUE-OK: vendor literal): the vendor's `direct_power_supply_mode: "enable"` (and `/control`'s
//! `power.direct_supply.set` `enabled: true`) means charging is STOPPED.

use serde_json::{Value, json};

/// The settings file (one writer: the owner of `charge`).
pub const FILE: &str = "/data/local/tmp/charge_limit.json";
pub const DEFAULT_HYSTERESIS: u8 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub enabled: bool,
    pub limit: u8,
    pub hysteresis: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            enabled: false,
            limit: 100,
            hysteresis: DEFAULT_HYSTERESIS,
        }
    }
}

/// The file as the agent reads it at start: `enabled` and a `u8` `limit` (else the defaults), the
/// hysteresis 5 when missing; the limit clamped to 50–100, the hysteresis to 1–20.
pub fn parse(text: Option<&str>) -> Settings {
    let v: Option<Value> = text.and_then(|t| serde_json::from_str(t).ok());
    let u8_of = |v: &Value| v.as_u64().filter(|n| *n <= 255).map(|n| n as u8);
    let fields = v.as_ref().and_then(|v| {
        let hysteresis = match v.get("hysteresis") {
            None => DEFAULT_HYSTERESIS,
            Some(h) => u8_of(h)?,
        };
        Some((v["enabled"].as_bool()?, u8_of(&v["limit"])?, hysteresis))
    });
    match fields {
        Some((enabled, limit, hysteresis)) => Settings {
            enabled,
            limit: limit.clamp(50, 100),
            hysteresis: hysteresis.clamp(1, 20),
        },
        None => Settings::default(),
    }
}

/// The file's text, byte for byte what the agent's `Persisted` serialises to.
pub fn file_text(s: &Settings) -> String {
    format!(
        r#"{{"enabled":{},"limit":{},"hysteresis":{}}}"#,
        s.enabled, s.limit, s.hysteresis
    )
}

/// The agent's `set()` checks, same messages.
pub fn validate(s: &Settings) -> Result<(), String> {
    if !(50..=100).contains(&s.limit) {
        return Err("limit must be 50-100".into());
    }
    if !(1..=20).contains(&s.hysteresis) {
        return Err("hysteresis must be 1-20".into());
    }
    Ok(())
}

/// A PUT `/api/device/charge-control`: a manual stop/resume first, then new settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Request {
    pub charging_stopped: Option<bool>,
    pub settings: Option<Change>,
}

/// The settings fields the request names; the rest stay.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Change {
    pub enabled: Option<bool>,
    pub limit: Option<u8>,
    pub hysteresis: Option<u8>,
}

impl Change {
    /// The new settings. Numbers are cut to `u8` the way the agent did (`as u8`), so 300 becomes
    /// 44 and fails [`validate`].
    pub fn apply(&self, cur: &Settings) -> Settings {
        Settings {
            enabled: self.enabled.unwrap_or(cur.enabled),
            limit: self.limit.unwrap_or(cur.limit),
            hysteresis: self.hysteresis.unwrap_or(cur.hysteresis),
        }
    }
}

/// The body, or the agent's 400.
pub fn parse_request(body: &[u8]) -> Result<Request, (u16, Value)> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|_| (400, json!({"ok": false, "error": "invalid JSON"})))?;
    let named = ["charge_limit_enabled", "charge_limit", "hysteresis"]
        .iter()
        .any(|k| v.get(k).is_some());
    let num = |k: &str| v[k].as_u64().map(|n| n as u8);
    Ok(Request {
        charging_stopped: v["charging_stopped"].as_bool(),
        settings: named.then(|| Change {
            enabled: v["charge_limit_enabled"].as_bool(),
            limit: num("charge_limit"),
            hysteresis: num("hysteresis"),
        }),
    })
}

/// `zwrt_bsp.charger list`'s stop flag: `Some(true)` = charging stopped; `None` = not in the reply.
pub fn stopped_in(charger_list: &Value) -> Option<bool> {
    charger_list["direct_power_supply_mode"]
        .as_str()
        .map(|s| s == "enable")
}

/// What GET `/api/device/charge-control` answers. `charging_stopped`: the ubus value, false when
/// it could not be read (as the agent).
pub fn reply(
    charging_stopped: bool,
    battery_status: &str,
    capacity: i64,
    s: &Settings,
    manual_override: bool,
) -> Value {
    let data = json!({
        "capacity": capacity,
        "battery_status": battery_status,
        "charging_stopped": charging_stopped,
        "manual_override": manual_override,
        "charge_limit": s.limit,
        "charge_limit_enabled": s.enabled,
        "hysteresis": s.hysteresis,
    });
    json!({"ok": true, "data": data})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_round_trip_and_agent_bytes() {
        let s = Settings {
            enabled: true,
            limit: 80,
            hysteresis: 5,
        };
        assert_eq!(
            file_text(&s),
            r#"{"enabled":true,"limit":80,"hysteresis":5}"#
        );
        assert_eq!(parse(Some(&file_text(&s))), s);
    }

    #[test]
    fn parse_as_the_agent() {
        assert_eq!(parse(None), Settings::default());
        assert_eq!(parse(Some("not json")), Settings::default());
        // no hysteresis: 5
        assert_eq!(
            parse(Some(r#"{"enabled":true,"limit":80}"#)),
            Settings {
                enabled: true,
                limit: 80,
                hysteresis: 5
            }
        );
        // clamped
        assert_eq!(
            parse(Some(r#"{"enabled":true,"limit":30,"hysteresis":40}"#)),
            Settings {
                enabled: true,
                limit: 50,
                hysteresis: 20
            }
        );
        // not a u8: the whole file is ignored
        assert_eq!(
            parse(Some(r#"{"enabled":true,"limit":300}"#)),
            Settings::default()
        );
        assert_eq!(
            parse(Some(r#"{"enabled":true,"limit":80,"hysteresis":-1}"#)),
            Settings::default()
        );
    }

    #[test]
    fn requests() {
        assert_eq!(parse_request(b"{").unwrap_err().0, 400);
        let r = parse_request(br#"{"charging_stopped":true}"#).unwrap();
        assert_eq!(r.charging_stopped, Some(true));
        assert_eq!(r.settings, None);
        let r = parse_request(br#"{"charge_limit":300}"#).unwrap();
        let s = r.settings.unwrap().apply(&Settings::default());
        assert_eq!(s.limit, 44);
        assert_eq!(validate(&s), Err("limit must be 50-100".into()));
        let r = parse_request(br#"{"hysteresis":0,"charge_limit_enabled":true}"#).unwrap();
        let s = r.settings.unwrap().apply(&Settings::default());
        assert!(s.enabled && s.limit == 100);
        assert_eq!(validate(&s), Err("hysteresis must be 1-20".into()));
        // a null field still names the settings (the agent tested `get(..).is_some()`)
        let r = parse_request(br#"{"charge_limit":null}"#).unwrap();
        assert_eq!(
            r.settings.unwrap().apply(&Settings::default()),
            Settings::default()
        );
    }

    #[test]
    fn stop_flag_is_inverted() {
        assert_eq!(
            stopped_in(&json!({"direct_power_supply_mode": "enable"})),
            Some(true)
        );
        assert_eq!(
            stopped_in(&json!({"direct_power_supply_mode": "disable"})),
            Some(false)
        );
        assert_eq!(stopped_in(&json!({})), None);
    }
}
