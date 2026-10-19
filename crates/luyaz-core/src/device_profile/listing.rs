use std::collections::{BTreeMap, BTreeSet};

use luyaz_api::firmware::{ProbeOutcome, ProbeResult, Proof};
use serde::{
    Deserialize,
    de::{self, MapAccess, Visitor},
};

struct SingleMethod(String);

impl<'de> Deserialize<'de> for SingleMethod {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct MethodVisitor;
        impl<'de> Visitor<'de> for MethodVisitor {
            type Value = SingleMethod;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("one method with a string-valued argument schema")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let Some((name, _)) = map.next_entry::<String, BTreeMap<String, String>>()? else {
                    return Err(de::Error::custom("empty method line"));
                };
                if map.next_entry::<String, serde::de::IgnoredAny>()?.is_some() || name.is_empty() {
                    return Err(de::Error::custom("multiple or empty method members"));
                }
                Ok(SingleMethod(name))
            }
        }
        deserializer.deserialize_map(MethodVisitor)
    }
}

pub(crate) struct ListingInput<'a> {
    pub text: &'a str,
    pub exit_code: Option<i32>,
    pub complete: bool,
    pub truncated: bool,
    pub duration_ms: u64,
}

/// Decode captured bytes only. Other objects' methods never match the requested object.
/// An absence is meaningful only after a successful, complete, bounded CLI listing.
pub(crate) fn method_result(input: ListingInput<'_>, object: &str, method: &str) -> ProbeResult {
    let mut result = ProbeResult {
        outcome: ProbeOutcome::Malformed,
        exit_code: input.exit_code,
        vendor_code: None,
        value: None,
        proof: None,
        complete: input.complete,
        truncated: input.truncated,
        duration_ms: input.duration_ms,
    };
    if input.exit_code != Some(0)
        || !input.complete
        || input.truncated
        || input.text.len() > 256 * 1024
    {
        return result;
    }
    let mut objects: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    let mut current = None;
    for line in input.text.lines().filter(|s| !s.trim().is_empty()) {
        if let Some(header) = line.strip_prefix('\'') {
            let Some((name, handle)) = header.split_once("' @") else {
                return result;
            };
            if name.is_empty()
                || handle.is_empty()
                || !handle.bytes().all(|b| b.is_ascii_hexdigit())
                || objects.insert(name, BTreeSet::new()).is_some()
            {
                return result;
            }
            current = Some(name);
            continue;
        }
        let Some(current) = current else {
            return result;
        };
        // Reject duplicate members even on the same line (Value/maps would overwrite them).
        let Ok(SingleMethod(name)) =
            serde_json::from_str::<SingleMethod>(&format!("{{{}}}", line.trim()))
        else {
            return result;
        };
        if !objects
            .get_mut(current)
            .expect("current object")
            .insert(name)
        {
            return result;
        }
    }
    let Some(methods) = objects.get(object) else {
        return result;
    };
    // A header by itself may be cut off at a line boundary. Do not call that an absence.
    if methods.is_empty() {
        return result;
    }
    let present = methods.contains(method);
    result.value = Some(serde_json::json!({"object":object,"method":method}));
    result.outcome = if present {
        ProbeOutcome::Success
    } else {
        ProbeOutcome::Absent
    };
    result.proof = Some(if present {
        Proof::MethodPresent
    } else {
        Proof::MethodAbsent
    });
    result
}
