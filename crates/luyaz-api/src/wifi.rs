//! The guest Wi-Fi write's shape (moved from zte-agent `wifi.rs`, u60-platform.md step 9h): which
//! uci keys a body field sets, cleaned like every agent uci value. The scenario engine's guest
//! action runs it in u60d too.

use serde_json::{Map, Value, json};

/// Drop the characters a uci value never needs and a shell would read.
pub fn sanitize_uci_value(v: &str) -> String {
    v.chars()
        .filter(|c| !matches!(c, '\'' | '"' | ';' | '$' | '`' | '\\' | '|' | '<' | '>' | '&'))
        .collect()
}

const GUEST_MAP: &[(&str, &[&str])] = &[
    ("guest_ssid", &["wireless.guest_2g.ssid", "wireless.guest_5g.ssid"]),
    ("guest_key", &["wireless.guest_2g.key", "wireless.guest_5g.key"]),
    ("guest_encryption", &["wireless.guest_2g.encryption", "wireless.guest_5g.encryption"]),
    ("guest_disabled", &["wireless.guest_2g.disabled", "wireless.guest_5g.disabled"]),
    ("guest_disabled_2g", &["wireless.guest_2g.disabled"]),
    ("guest_disabled_5g", &["wireless.guest_5g.disabled"]),
    ("guest_hidden", &["wireless.guest_2g.hidden", "wireless.guest_5g.hidden"]),
    ("guest_isolate", &["wireless.guest_2g.isolate", "wireless.guest_5g.isolate"]),
    ("guest_active_time", &["wireless.guest_2g.guest_active_time", "wireless.guest_5g.guest_active_time"]),
];

/// PUT /api/wifi/guest's body as the uci keys to set (`wifi.apply`'s `set`), or the reply
/// refusing it. Unknown fields and values that are not strings, numbers or booleans are ignored.
pub fn guest_set(body: &[u8]) -> Result<Map<String, Value>, (u16, Value)> {
    let parsed: Value =
        serde_json::from_slice(body).map_err(|_| (400, json!({"ok": false, "error": "invalid JSON"})))?;
    let obj = parsed
        .as_object()
        .ok_or((400, json!({"ok": false, "error": "expected JSON object"})))?;
    let mut set = Map::new();
    for (key, value) in obj {
        let val_str = match value {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => if *b { "1" } else { "0" }.to_string(),
            _ => continue,
        };
        let val_str = sanitize_uci_value(&val_str);
        if let Some(&(_, paths)) = GUEST_MAP.iter().find(|&&(k, _)| k == key) {
            for &path in paths {
                set.insert(path.into(), json!(val_str));
            }
        }
    }
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_fields_map_to_both_bands() {
        let set = guest_set(br#"{"guest_disabled": true, "guest_ssid": "Cafe;x", "other": 1}"#).unwrap();
        assert_eq!(set["wireless.guest_2g.disabled"], "1");
        assert_eq!(set["wireless.guest_5g.disabled"], "1");
        assert_eq!(set["wireless.guest_5g.ssid"], "Cafex");
        assert_eq!(set.len(), 4);
        assert_eq!(guest_set(b"[1]").unwrap_err().0, 400);
    }
}
