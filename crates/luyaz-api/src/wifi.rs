//! The guest Wi-Fi write's shape: which uci keys a body field sets, each value cleaned of shell
//! characters. The scenario engine's guest
//! action runs it in u60d too.

use serde_json::{Map, Value, json};

/// Drop the characters a uci value never needs and a shell would read.
pub fn sanitize_uci_value(v: &str) -> String {
    v.chars()
        .filter(|c| {
            !matches!(
                c,
                '\'' | '"' | ';' | '$' | '`' | '\\' | '|' | '<' | '>' | '&'
            )
        })
        .collect()
}

/// Which radios' guest section a body field writes, and the uci option it sets there.
enum Bands {
    Both,
    Only2g,
    Only5g,
}

/// Body field → (bands, option under `wireless.guest_<band>`).
const GUEST_FIELDS: [(&str, Bands, &str); 9] = [
    ("guest_ssid", Bands::Both, "ssid"),
    ("guest_key", Bands::Both, "key"),
    ("guest_encryption", Bands::Both, "encryption"),
    ("guest_hidden", Bands::Both, "hidden"),
    ("guest_isolate", Bands::Both, "isolate"),
    ("guest_active_time", Bands::Both, "guest_active_time"),
    ("guest_disabled", Bands::Both, "disabled"),
    ("guest_disabled_2g", Bands::Only2g, "disabled"),
    ("guest_disabled_5g", Bands::Only5g, "disabled"),
];

/// A JSON scalar as uci text (booleans as 1/0); `None` for null, arrays and objects.
fn uci_text(v: &Value) -> Option<String> {
    Some(match v {
        Value::Bool(true) => "1".to_owned(),
        Value::Bool(false) => "0".to_owned(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.to_owned(),
        _ => return None,
    })
}

/// PUT /api/wifi/guest's body as the uci keys to set (`wifi.apply`'s `set`), or the reply
/// refusing it. Unknown fields and values that are not strings, numbers or booleans are ignored.
pub fn guest_set(body: &[u8]) -> Result<Map<String, Value>, (u16, Value)> {
    let parsed: Value = serde_json::from_slice(body)
        .map_err(|_| (400, json!({"ok": false, "error": "invalid JSON"})))?;
    let obj = parsed
        .as_object()
        .ok_or((400, json!({"ok": false, "error": "expected JSON object"})))?;
    let mut set = Map::new();
    for (field, bands, option) in &GUEST_FIELDS {
        let Some(text) = obj.get(*field).and_then(uci_text) else {
            continue;
        };
        let clean = Value::String(sanitize_uci_value(&text));
        let radios: &[&str] = match bands {
            Bands::Both => &["2g", "5g"],
            Bands::Only2g => &["2g"],
            Bands::Only5g => &["5g"],
        };
        for radio in radios {
            set.insert(format!("wireless.guest_{radio}.{option}"), clean.clone());
        }
    }
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_fields_map_to_both_bands() {
        let set =
            guest_set(br#"{"guest_disabled": true, "guest_ssid": "Cafe;x", "other": 1}"#).unwrap();
        assert_eq!(set["wireless.guest_2g.disabled"], "1");
        assert_eq!(set["wireless.guest_5g.disabled"], "1");
        assert_eq!(set["wireless.guest_5g.ssid"], "Cafex");
        assert_eq!(set.len(), 4);
        assert_eq!(guest_set(b"[1]").unwrap_err().0, 400);
    }
}
