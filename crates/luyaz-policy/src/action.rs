//! The action a scenario or a scheduled job sends (zte-agent `src/action.rs`, manager 20c3e35),
//! and the JSON value its body is, without serde_json so the wasm guest builds without it.
//!
//! `Json` only carries a body from the config to the decisions that look into it (`validate`
//! reads `ap_2g`/`ap_5g`, `restore_key` prints the body, `restore_action` puts a saved value
//! back). Its `Display` is serde_json's compact output byte for byte (sorted keys, raw UTF-8,
//! the same escapes), because restore keys built from it are compared with the ones the agent
//! wrote into `restore.json`. Numbers keep their JSON text, so `1` and `1.0` stay what they were.
//! With the `serde` feature it (de)serialises through `serde_json::Value`.

use std::collections::BTreeMap;
use std::fmt;

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    /// The number as serde_json writes it.
    Number(String),
    String(String),
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}

impl Json {
    pub fn obj<K: Into<String>>(fields: impl IntoIterator<Item = (K, Json)>) -> Json {
        Json::Object(fields.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(m) => m.get(key),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_object_mut(&mut self) -> Option<&mut BTreeMap<String, Json>> {
        match self {
            Json::Object(m) => Some(m),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }
}

impl From<&str> for Json {
    fn from(s: &str) -> Self {
        Json::String(s.to_string())
    }
}

impl From<String> for Json {
    fn from(s: String) -> Self {
        Json::String(s)
    }
}

impl From<bool> for Json {
    fn from(b: bool) -> Self {
        Json::Bool(b)
    }
}

/// serde_json's string escaping (`format_escaped_str`).
fn write_str(f: &mut fmt::Formatter<'_>, s: &str) -> fmt::Result {
    f.write_str("\"")?;
    let mut start = 0;
    for (i, b) in s.bytes().enumerate() {
        let esc: Option<&str> = match b {
            b'"' => Some("\\\""),
            b'\\' => Some("\\\\"),
            0x08 => Some("\\b"),
            0x0c => Some("\\f"),
            b'\n' => Some("\\n"),
            b'\r' => Some("\\r"),
            b'\t' => Some("\\t"),
            0..=0x1f => None,
            _ => continue,
        };
        f.write_str(&s[start..i])?;
        match esc {
            Some(e) => f.write_str(e)?,
            None => write!(f, "\\u00{b:02x}")?,
        }
        start = i + 1;
    }
    f.write_str(&s[start..])?;
    f.write_str("\"")
}

impl fmt::Display for Json {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Json::Null => f.write_str("null"),
            Json::Bool(b) => write!(f, "{b}"),
            Json::Number(n) => f.write_str(n),
            Json::String(s) => write_str(f, s),
            Json::Array(a) => {
                f.write_str("[")?;
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        f.write_str(",")?;
                    }
                    write!(f, "{v}")?;
                }
                f.write_str("]")
            }
            Json::Object(m) => {
                f.write_str("{")?;
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        f.write_str(",")?;
                    }
                    write_str(f, k)?;
                    write!(f, ":{v}")?;
                }
                f.write_str("}")
            }
        }
    }
}

#[cfg(any(test, feature = "serde"))]
impl From<serde_json::Value> for Json {
    fn from(v: serde_json::Value) -> Self {
        use serde_json::Value;
        match v {
            Value::Null => Json::Null,
            Value::Bool(b) => Json::Bool(b),
            Value::Number(n) => Json::Number(n.to_string()),
            Value::String(s) => Json::String(s),
            Value::Array(a) => Json::Array(a.into_iter().map(Json::from).collect()),
            Value::Object(m) => {
                Json::Object(m.into_iter().map(|(k, v)| (k, Json::from(v))).collect())
            }
        }
    }
}

#[cfg(any(test, feature = "serde"))]
impl From<&Json> for serde_json::Value {
    fn from(j: &Json) -> Self {
        use serde_json::Value;
        match j {
            Json::Null => Value::Null,
            Json::Bool(b) => Value::Bool(*b),
            Json::Number(n) => serde_json::from_str::<serde_json::Number>(n)
                .map(Value::Number)
                .unwrap_or(Value::Null),
            Json::String(s) => Value::String(s.clone()),
            Json::Array(a) => Value::Array(a.iter().map(Value::from).collect()),
            Json::Object(m) => {
                Value::Object(m.iter().map(|(k, v)| (k.clone(), Value::from(v))).collect())
            }
        }
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for Json {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        serde_json::Value::from(self).serialize(s)
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for Json {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        serde_json::Value::deserialize(d).map(Json::from)
    }
}

/// An HTTP call routed inside the agent; the shape scenarios.json and scheduler.json store.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Action {
    pub method: String,
    pub path: String,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub body: Option<Json>,
}

/// The agent's `parse_method(..).is_some()`.
pub fn is_method(s: &str) -> bool {
    matches!(s, "GET" | "POST" | "PUT" | "DELETE")
}

#[cfg(test)]
mod tests;
