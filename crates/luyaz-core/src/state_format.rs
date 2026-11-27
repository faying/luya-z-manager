//! P-8: the files the core keeps across a ship are locked to `ship/core/format`.
//!
//! A rollback, by hand or automatic, puts the old program back over files the new one wrote:
//! the automatic one leaves the settings in `state-files` as they are (settings changed after the
//! promote stay), and the files in `state-keep` are never swapped. The old program then reads the
//! new program's file. `ship/core/format` is what makes that safe (ship refuses a build whose
//! format differs from the device's), but nothing made anyone raise it.
//!
//! Each such file has an example under `ship/core/format-<N>/` (made up, nothing private: this
//! directory is public). The test reads it with today's type and writes it back: the same JSON
//! value or the test fails, with the paths that moved. Then either:
//! - the change is one an older core reads correctly (a new field it ignores, with a default
//!   here): regenerate with `LUYAZ_UPDATE_FIXTURES=1 cargo test -p luyaz-core state_format`;
//! - or it is not (a rename, a removed field, a new enum value, a changed type): raise
//!   `ship/core/format`, add `format-<N+1>/`, and write the migration and the way back.
//!
//! Examples of older formats still have to read (a ship or a rollback meets them).
#![cfg(test)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::Value;

fn ship_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ship/core")
}

/// The format number in `ship/core/format` (first non-comment word).
fn current() -> u32 {
    let text = std::fs::read_to_string(ship_dir().join("format")).unwrap();
    text.lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .find(|l| !l.is_empty())
        .and_then(|l| l.parse().ok())
        .expect("ship/core/format has a number")
}

/// `name`'s examples: every `format-<N>/<name>` up to the current format must read, and the
/// current one must come back out unchanged. `roundtrip` reads a file's JSON the way the
/// program does and gives back what the program would write.
pub(crate) fn check(name: &str, roundtrip: impl Fn(&Value) -> Result<Value, String>) {
    let now = current();
    for n in 1..=now {
        let path = ship_dir().join(format!("format-{n}/{name}"));
        let Ok(text) = std::fs::read_to_string(&path) else {
            assert!(n < now || update(), "{}: no example for format {n}", path.display());
            continue;
        };
        let example: Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("{}: not JSON: {e}", path.display()));
        let back = roundtrip(&example)
            .unwrap_or_else(|e| panic!("{name} (format {n}) no longer reads: {e}"));
        if n < now || back == example {
            continue;
        }
        if update() {
            std::fs::write(&path, serde_json::to_string_pretty(&back).unwrap() + "\n").unwrap();
            continue;
        }
        panic!(
            "{name}: what the core writes no longer matches format {now}'s example ({}).\n{}\n\
             If an older core reads the new file correctly (a field it does not know is ignored, \
             and this one has a default): LUYAZ_UPDATE_FIXTURES=1 cargo test -p luyaz-core \
             state_format. If not: raise ship/core/format, add format-{}/, and write the \
             migration and the way back (see the top of state_format.rs).",
            path.display(),
            diff(&example, &back),
            now + 1
        );
    }
}

/// When `check` finds a missing or differing example: write it instead of failing.
fn update() -> bool {
    std::env::var_os("LUYAZ_UPDATE_FIXTURES").is_some()
}

/// Paths gone, new, of another type, or with another value (values are made up: safe to show).
fn diff(was: &Value, now: &Value) -> String {
    let (mut a, mut b) = (BTreeMap::new(), BTreeMap::new());
    crate::model::shape(was, "", &mut a);
    crate::model::shape(now, "", &mut b);
    let mut out = Vec::new();
    for (p, t) in &a {
        match b.get(p) {
            None => out.push(format!("  gone:    {p} ({t})")),
            Some(u) if u != t => out.push(format!("  type:    {p} {t} → {u}")),
            Some(_) => {}
        }
    }
    for (p, t) in &b {
        if !a.contains_key(p) {
            out.push(format!("  new:     {p} ({t})"));
        }
    }
    if out.is_empty() {
        out.push(format!("  same shape, other values:\n  was {was}\n  now {now}"));
    }
    out.join("\n")
}

/// Read as `T`, write back as the program does (`to_value` of the same type).
pub(crate) fn serde<T: serde::de::DeserializeOwned + serde::Serialize>(v: &Value) -> Result<Value, String> {
    let t: T = serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
    serde_json::to_value(&t).map_err(|e| e.to_string())
}

// The files whose types are public; the private ones are checked next to their types
// (lanid aliases, ops agent limit and roaming list, the SMS journal).

#[test]
fn scenarios() {
    check("scenarios.json", serde::<luyaz_policy::scenario::Config>);
}

#[test]
fn apn_pick() {
    check("apn-pick.json", serde::<luyaz_policy::apn_pick::Store>);
}

#[test]
fn scheduler() {
    // the file is `{"jobs":[…]}` (engines/scheduler.rs `parse` / `save`)
    check("scheduler.json", |v| {
        let jobs: Vec<luyaz_policy::scheduler::Job> =
            serde_json::from_value(v["jobs"].clone()).map_err(|e| e.to_string())?;
        Ok(serde_json::json!({ "jobs": jobs }))
    });
}

#[test]
fn charge_limit() {
    // hand-parsed (luyaz-api charge.rs): a field it does not read would silently go to the default
    check("charge_limit.json", |v| {
        let s = luyaz_api::charge::parse(Some(&v.to_string()));
        serde_json::from_str(&luyaz_api::charge::file_text(&s)).map_err(|e| e.to_string())
    });
}

#[test]
fn sms_forward_config() {
    check("sms_forward.json", serde::<luyaz_sms_forward::SmsForwardConfig>);
}

#[test]
fn sms_forward_queue() {
    check("sms-forward-queue.json", serde::<luyaz_sms_forward::Queue>);
}

#[test]
fn sms_forward_state() {
    check("sms_forward_state.json", serde::<luyaz_sms_forward::ForwardState>);
}

#[test]
fn poller_allow() {
    check("poller-allow.json", serde::<Vec<crate::poller::Allow>>);
}

#[test]
fn regress_limits() {
    check("regress-limits.json", serde::<crate::longterm::regress::base::Limits>);
}

