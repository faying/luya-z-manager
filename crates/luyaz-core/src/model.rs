//! Snapshot types served by `/v2/state`, plus an offline shape-diff tool.

use serde::Serialize;
use serde_json::{Map, Value};
use std::{collections::BTreeMap, fs};

/// One published state document: a timestamp, who produced it, and every
/// collected field flattened in beside them.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Snapshot {
    pub ts: i64,
    pub core: CoreVersion,
    #[serde(flatten)]
    pub fields: Map<String, Value>,
}

/// Producer identity. The wire name stays `u60d` for existing clients.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct CoreVersion {
    pub name: &'static str,
    pub version: &'static str,
}

impl Default for CoreVersion {
    fn default() -> Self {
        let version = env!("CARGO_PKG_VERSION");
        CoreVersion {
            name: "u60d",
            version,
        }
    }
}

/// JSON type name used in shape reports.
fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn join_path(parent: &str, key: &str) -> String {
    if parent.is_empty() {
        key.to_owned()
    } else {
        format!("{parent}.{key}")
    }
}

/// Flatten `value` into dotted path -> type name. Objects are walked into and
/// never appear themselves; arrays are leaves (their contents are not listed).
fn shape(value: &Value, path: &str, out: &mut BTreeMap<String, &'static str>) {
    if let Value::Object(members) = value {
        for (key, member) in members {
            shape(member, &join_path(path, key), out);
        }
    } else {
        out.insert(path.to_owned(), kind_of(value));
    }
}

/// Paths (present on both sides) whose leaf values differ. Recursion only
/// follows keys that exist in both objects; anything that is not a pair of
/// objects is compared as a whole.
fn changed_paths(left: &Value, right: &Value, path: &str, out: &mut Vec<String>) {
    match (left.as_object(), right.as_object()) {
        (Some(a), Some(b)) => {
            for (key, lhs) in a {
                if let Some(rhs) = b.get(key) {
                    changed_paths(lhs, rhs, &join_path(path, key), out);
                }
            }
        }
        _ => {
            if left != right {
                out.push(path.to_owned());
            }
        }
    }
}

/// Size limit for each input file.
const SHAPE_INPUT_MAX: u64 = 32 * 1024 * 1024;

fn load_json(path: &str) -> Result<Value, String> {
    let with_path = |e: &dyn std::fmt::Display| format!("{path}: {e}");
    let size = fs::metadata(path).map_err(|e| with_path(&e))?.len();
    if size > SHAPE_INPUT_MAX {
        return Err(format!("{path}: input exceeds 32 MiB"));
    }
    let bytes = fs::read(path).map_err(|e| with_path(&e))?;
    serde_json::from_slice(&bytes).map_err(|e| with_path(&e))
}

/// CLI diagnostic: report how two state snapshots differ in *structure*
/// (missing/extra paths, type changes) plus the list of paths whose values
/// differ. Values themselves are never printed, so a device-local snapshot can
/// be compared without leaking its contents.
pub fn compare_state_shape(paths: &[String]) -> Result<Value, String> {
    let [base_path, cand_path] = paths else {
        return Err("usage: --compare-state-shape BASELINE CANDIDATE".into());
    };
    let base_doc = load_json(base_path)?;
    let cand_doc = load_json(cand_path)?;

    let mut base = BTreeMap::new();
    shape(&base_doc, "", &mut base);
    let mut cand = BTreeMap::new();
    shape(&cand_doc, "", &mut cand);

    let mut missing = Vec::new();
    let mut type_mismatches = Vec::new();
    for (path, &was) in &base {
        match cand.get(path) {
            None => missing.push(path.clone()),
            Some(&now) if now != was => type_mismatches.push(serde_json::json!({
                "path": path, "baseline": was, "candidate": now,
            })),
            Some(_) => {}
        }
    }
    let extra: Vec<String> = cand
        .keys()
        .filter(|path| !base.contains_key(*path))
        .cloned()
        .collect();

    let mut different_values = Vec::new();
    changed_paths(&base_doc, &cand_doc, "", &mut different_values);

    Ok(serde_json::json!({
        "baseline_paths": base.len(),
        "candidate_paths": cand.len(),
        "missing": missing,
        "extra": extra,
        "type_mismatches": type_mismatches,
        "different_values": different_values,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_never_contains_values() {
        let doc = serde_json::json!({"secret": "12345", "items": [{"id": 7}]});
        let mut paths = BTreeMap::new();
        shape(&doc, "", &mut paths);
        // Types are reported, values are not.
        assert_eq!(paths.get("secret"), Some(&"string"));
        assert_eq!(paths.get("items"), Some(&"array"));
        let rendered = format!("{paths:?}");
        assert!(!rendered.contains("12345"));
    }
}
