//! The device check's pure parts (moved from zte-agent `health.rs`, Phase 4 step 8b): reading
//! `doctor.sh --tsv` / `--tsv2`, the `/api/health` reply, the counts for `/api/public/status`, the
//! crash-log list.
//!
//! Running the doctor ([`Runner`], step 9d) belongs to whoever owns the `health` part: the agent,
//! every 60 s, until it hands the part to u60d. After each run the owner writes its [`Snapshot`]
//! to [`SNAPSHOT_FILE`]; whoever does not own it answers from that file, so the doctor never runs
//! twice.

use serde_json::{Value, json};
use std::fs;
use std::path::Path;

pub const CRASH_DIR: &str = "/data/crashlog";
/// The latest doctor run, as the agent last saw it (tmpfs; [`Snapshot::to_json`]).
pub const SNAPSHOT_FILE: &str = "/tmp/health.json";

#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    pub level: String, // ok | warn | bad
    pub id: String,
    pub label: String,
    pub detail: String,
    /// English twins from doctor `--tsv2`; empty when the doctor is too old
    /// to give them (sent as null, so clients fall back to the Chinese).
    pub label_en: String,
    pub detail_en: String,
}

/// The last doctor run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub checks: Vec<Check>,
    /// device clock (local wall time labelled UTC; see clock.rs)
    pub at_device: i64,
    /// false = never ran
    pub ran: bool,
    pub error: Option<String>,
}

impl Snapshot {
    /// What goes into [`SNAPSHOT_FILE`].
    pub fn to_json(&self) -> Value {
        json!({
            "checks": self.checks.iter().map(check_json).collect::<Vec<_>>(),
            "at_device": self.at_device,
            "ran": self.ran,
            "error": self.error,
        })
    }

    /// Back from [`Snapshot::to_json`]; `None` when it is not one.
    pub fn from_json(v: &Value) -> Option<Snapshot> {
        let s = |o: &Value, k: &str| o.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let checks = v
            .get("checks")?
            .as_array()?
            .iter()
            .map(|c| Check {
                level: s(c, "level"),
                id: s(c, "id"),
                label: s(c, "label"),
                detail: s(c, "detail"),
                label_en: s(c, "label_en"),
                detail_en: s(c, "detail_en"),
            })
            .collect();
        Some(Snapshot {
            checks,
            at_device: v.get("at_device")?.as_i64()?,
            ran: v.get("ran")?.as_bool()?,
            error: v.get("error").and_then(Value::as_str).map(str::to_string),
        })
    }

    /// [`SNAPSHOT_FILE`] (or another path); never-ran when missing or unreadable.
    pub fn read(path: &Path) -> Snapshot {
        fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| Snapshot::from_json(&v))
            .unwrap_or_default()
    }
}

pub fn parse(tsv: &str) -> Vec<Check> {
    tsv.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.splitn(4, '\t').collect();
            if f.len() != 4 || !matches!(f[0], "ok" | "warn" | "bad") {
                return None;
            }
            Some(Check {
                level: f[0].into(),
                id: f[1].into(),
                label: f[2].into(),
                detail: f[3].into(),
                label_en: String::new(),
                detail_en: String::new(),
            })
        })
        .collect()
}

/// First line of `doctor.sh --tsv2`. An older doctor does not know the flag
/// and prints its human-readable report instead (L2 review R2), so without
/// this line the output is not ours to parse.
pub const TSV2_MARKER: &str = "#tsv2";

/// True when the output starts with the `--tsv2` marker line.
pub fn has_tsv2_marker(out: &str) -> bool {
    out.lines().next().map(|l| l.trim_end_matches('\r')) == Some(TSV2_MARKER)
}

/// `doctor.sh --tsv2`: after the marker line, six tab-separated columns
/// `level id label detail label_en detail_en`. A Chinese detail may itself
/// contain a tab (see the 4-column test), so the first three and the last two
/// fields are fixed and whatever lies between is the detail.
pub fn parse_tsv2(tsv: &str) -> Vec<Check> {
    tsv.lines()
        .skip(1)
        .filter_map(|l| {
            let l = l.trim_end_matches('\r');
            let f: Vec<&str> = l.split('\t').collect();
            if f.len() < 6 || !matches!(f[0], "ok" | "warn" | "bad") {
                return None;
            }
            let n = f.len();
            Some(Check {
                level: f[0].into(),
                id: f[1].into(),
                label: f[2].into(),
                detail: f[3..n - 2].join("\t"),
                label_en: f[n - 2].into(),
                detail_en: f[n - 1].into(),
            })
        })
        .collect()
}

pub fn counts(checks: &[Check]) -> (usize, usize) {
    let bad = checks.iter().filter(|c| c.level == "bad").count();
    let warn = checks.iter().filter(|c| c.level == "warn").count();
    (bad, warn)
}

/// For `/api/public/status`: counts only.
pub fn public_summary(s: &Snapshot) -> Value {
    let (bad, warn) = counts(&s.checks);
    json!({ "checked": s.ran && s.error.is_none(), "bad": bad, "warn": warn })
}

/// One check for `/api/health`. The English twins are null, not "", when the
/// doctor did not give them: clients write `label_en ?? label`.
pub fn check_json(c: &Check) -> Value {
    let en = |s: &str| (!s.is_empty()).then(|| s.to_string());
    json!({
        "level": c.level, "id": c.id, "label": c.label, "detail": c.detail,
        "label_en": en(&c.label_en), "detail_en": en(&c.detail_en),
    })
}

/// `data` of GET /api/health.
pub fn reply(s: &Snapshot, crashlogs: Vec<Value>) -> Value {
    let (bad, warn) = counts(&s.checks);
    let checks: Vec<Value> = s.checks.iter().map(check_json).collect();
    json!({
        "checks": checks,
        "bad": bad,
        "warn": warn,
        "checked_at": if s.ran { Some(s.at_device) } else { None },
        "error": s.error,
        "crashlogs": crashlogs,
    })
}

/// Name safe to use as one path component: no separators, no dot-dot.
pub fn safe_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 96
        && !n.starts_with('.')
        && n.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

/// The crash logs the sentinel keeps under `dir` (`<program>/<file>.log`), newest first.
pub fn crash_logs(dir: &Path) -> Vec<Value> {
    let mut out = Vec::new();
    let Ok(progs) = fs::read_dir(dir) else {
        return out;
    };
    for p in progs.flatten() {
        let prog = p.file_name().to_string_lossy().to_string();
        if !safe_name(&prog) {
            continue;
        }
        let Ok(files) = fs::read_dir(p.path()) else {
            continue;
        };
        for f in files.flatten() {
            let name = f.file_name().to_string_lossy().to_string();
            if !name.ends_with(".log") || !safe_name(&name) {
                continue;
            }
            let meta = f.metadata().ok();
            let mtime = meta
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            // The status line, so the list says what happened without opening it.
            let status = fs::read_to_string(f.path())
                .ok()
                .and_then(|t| {
                    t.lines()
                        .find_map(|l| l.strip_prefix("status:").map(|s| s.trim().to_string()))
                })
                .unwrap_or_default();
            out.push(json!({
                "program": prog, "file": name, "time": mtime,
                "size": meta.map(|m| m.len()).unwrap_or(0), "status": status,
            }));
        }
    }
    out.sort_by(|a, b| b["time"].as_i64().cmp(&a["time"].as_i64()));
    out
}

/// Any CJK ideograph, kana, CJK punctuation or fullwidth form: what an
/// `*_en` field must not contain (netinfo checks its errors with it; tests
/// across the agent use it).
pub fn has_cjk(s: &str) -> bool {
    s.chars().any(|c| {
        matches!(c as u32,
            0x2E80..=0x2FFF | 0x3000..=0x303F | 0x3040..=0x30FF | 0x3100..=0x9FFF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF)
    })
}

mod runner;
pub use runner::{
    DOCTOR, DOCTOR_TIMEOUT, EVERY, FIRST_WAIT, Runner, run_checks, run_checks_core, run_doctor_within,
};

#[cfg(test)]
mod tests;
