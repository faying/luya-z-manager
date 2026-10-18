//! The open findings and the suppressed table (T3; design R4/D5, R9/D10, R10/D11): what the
//! core keeps between checks and publishes as the `findings` block. One JSON value both ways
//! ([`Store::to_value`], [`Store::from_value`]) so whatever persists it only stores that.
//!
//! Lifecycle (pilot): open → resolved (two "up" checks in a row), dismissed (the user; the same
//! kind + ICCID stays closed for 24 h), expired (deregistered, another card, an expected
//! outage). The dedup key is kind + ICCID: a check that finds the same key open updates it.
//!
//! Bounds (D10): at most [`MAX_ENTRIES`] open findings and suppressions together (the oldest
//! open one closes as `evicted` first, then the oldest suppression goes); one finding keeps
//! only the latest check's evidence (one per layer) plus first/last/count and is at most
//! [`ENTRY_MAX`] bytes. Every transition hands back a [`Change`] for `finding.*` events.
//!
//! Clock (local time labelled UTC, and wrong until SNTP after a boot): a suppression is never
//! treated as expired while the clock is before `record::CLOCK_SET`.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use luyaz_api::finding::{Finding, Verdict};

use crate::record::CLOCK_SET;

pub const MAX_ENTRIES: usize = 8;
pub const ENTRY_MAX: usize = 2048;
pub const SUPPRESS_SECS: u64 = 24 * 3600;
/// "Up" checks in a row that resolve an open finding.
pub const RESOLVE_UPS: u8 = 2;
/// Longest evidence text kept when a finding is over [`ENTRY_MAX`].
const DETAIL_CUT: usize = 120;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub kind: String,
    pub iccid: String,
    pub first: u64,
    pub last: u64,
    pub count: u32,
    /// "Up" checks in a row since the last one that found it.
    #[serde(default)]
    pub ups: u8,
    /// The latest check's `Finding` as JSON (layer, confidence, symptom, evidence).
    pub finding: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Suppressed {
    pub kind: String,
    pub iccid: String,
    pub until: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub open: Vec<Entry>,
    #[serde(default)]
    pub suppressed: Vec<Suppressed>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Close {
    Resolved,
    Dismissed,
    Expired,
    Evicted,
}

impl Close {
    pub fn as_str(self) -> &'static str {
        match self {
            Close::Resolved => "resolved",
            Close::Dismissed => "dismissed",
            Close::Expired => "expired",
            Close::Evicted => "evicted",
        }
    }
}

/// One transition, for a `finding.open` / `finding.update` / `finding.close` event.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    Open(Entry),
    /// The layer or the confidence moved (a recheck that says the same writes nothing).
    Update(Entry),
    Close(Entry, Close),
}

impl Change {
    /// Event kind and fields. The ICCID's last 4 digits only: enough to tell cards apart.
    pub fn event(&self) -> (&'static str, Value) {
        let (kind, e, why) = match self {
            Change::Open(e) => ("finding.open", e, None),
            Change::Update(e) => ("finding.update", e, None),
            Change::Close(e, c) => ("finding.close", e, Some(c.as_str())),
        };
        let tail = &e.iccid[e.iccid.len().saturating_sub(4)..];
        (
            kind,
            json!({
                "finding": e.kind,
                "iccid_tail": tail,
                "layer": e.finding.get("layer"),
                "confidence": e.finding.get("confidence"),
                "first": e.first,
                "count": e.count,
                "why": why,
            }),
        )
    }
}

fn clock_set(now: u64) -> bool {
    now >= CLOCK_SET
}

impl Store {
    /// A persisted value back; anything it does not understand is dropped, bounds applied.
    pub fn from_value(v: &Value) -> Store {
        let mut s: Store = serde_json::from_value(v.clone()).unwrap_or_default();
        s.open.truncate(MAX_ENTRIES);
        s.suppressed.truncate(MAX_ENTRIES - s.open.len());
        for e in &mut s.open {
            fit(&mut e.finding);
        }
        s
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    pub fn is_suppressed(&self, kind: &str, iccid: &str, now: u64) -> bool {
        self.suppressed
            .iter()
            .any(|s| s.kind == kind && s.iccid == iccid && (!clock_set(now) || s.until > now))
    }

    /// One check's verdict for the card in now (`iccid`, "" = unknown).
    pub fn observe(&mut self, v: &Verdict, iccid: &str, now: u64) -> Vec<Change> {
        self.prune(now);
        let mut out = Vec::new();
        // another card: the old card's findings no longer hold (no reading is not another card)
        if !iccid.is_empty() {
            self.close_where(|e| e.iccid != iccid, Close::Expired, &mut out);
        }
        match v {
            // without the card it can't be keyed
            Verdict::Open(f) if f.iccid.is_empty() => {}
            Verdict::Open(f) => self.found(f, now, &mut out),
            Verdict::None { why } => match *why {
                "up" | "all_ok" | "path_dns_ok" => {
                    for e in &mut self.open {
                        e.ups = e.ups.saturating_add(1);
                    }
                    self.close_where(|e| e.ups >= RESOLVE_UPS, Close::Resolved, &mut out);
                }
                // deregistered, data off, airplane, no card: the premise is gone
                _ => self.close_where(|_| true, Close::Expired, &mut out),
            },
            // can't tell: keep what is open as it is
            Verdict::Unverified { .. } => {}
        }
        out
    }

    /// The user's "ignore": closes it and keeps the key closed for 24 h.
    pub fn dismiss(&mut self, kind: &str, iccid: &str, now: u64) -> Vec<Change> {
        let mut out = Vec::new();
        self.close_where(
            |e| e.kind == kind && e.iccid == iccid,
            Close::Dismissed,
            &mut out,
        );
        if out.is_empty() {
            return out;
        }
        self.suppressed
            .retain(|s| !(s.kind == kind && s.iccid == iccid));
        self.make_room(&mut out);
        self.suppressed.push(Suppressed {
            kind: kind.to_string(),
            iccid: iccid.to_string(),
            until: now + SUPPRESS_SECS,
        });
        out
    }

    /// Drop suppressions that ran out (never before the clock is set).
    pub fn prune(&mut self, now: u64) {
        if clock_set(now) {
            self.suppressed.retain(|s| s.until > now);
        }
    }

    fn found(&mut self, f: &Finding, now: u64, out: &mut Vec<Change>) {
        if self.is_suppressed(f.kind, &f.iccid, now) {
            return;
        }
        let mut body = serde_json::to_value(f).unwrap_or(Value::Null);
        fit(&mut body);
        if let Some(e) = self
            .open
            .iter_mut()
            .find(|e| e.kind == f.kind && e.iccid == f.iccid)
        {
            let moved = e.finding.get("layer") != body.get("layer")
                || e.finding.get("confidence") != body.get("confidence");
            e.last = now;
            e.count = e.count.saturating_add(1);
            e.ups = 0;
            e.finding = body;
            if moved {
                out.push(Change::Update(e.clone()));
            }
            return;
        }
        self.make_room(out);
        let e = Entry {
            kind: f.kind.to_string(),
            iccid: f.iccid.clone(),
            first: now,
            last: now,
            count: 1,
            ups: 0,
            finding: body,
        };
        out.push(Change::Open(e.clone()));
        self.open.push(e);
    }

    /// Room for one more: the oldest open finding closes first, then the oldest suppression.
    fn make_room(&mut self, out: &mut Vec<Change>) {
        while self.open.len() + self.suppressed.len() >= MAX_ENTRIES {
            if let Some(i) = (0..self.open.len()).min_by_key(|&i| self.open[i].first) {
                let e = self.open.remove(i);
                out.push(Change::Close(e, Close::Evicted));
            } else if let Some(i) =
                (0..self.suppressed.len()).min_by_key(|&i| self.suppressed[i].until)
            {
                self.suppressed.remove(i);
            }
        }
    }

    fn close_where(&mut self, pick: impl Fn(&Entry) -> bool, why: Close, out: &mut Vec<Change>) {
        let mut i = 0;
        while i < self.open.len() {
            if pick(&self.open[i]) {
                out.push(Change::Close(self.open.remove(i), why));
            } else {
                i += 1;
            }
        }
    }
}

/// Keep one finding under [`ENTRY_MAX`]: cut the evidence texts, then drop the evidence.
fn fit(body: &mut Value) {
    if size(body) <= ENTRY_MAX {
        return;
    }
    if let Some(ev) = body.get_mut("evidence").and_then(Value::as_array_mut) {
        for e in ev.iter_mut() {
            for k in ["detail", "detail_en"] {
                if let Some(s) = e.get(k).and_then(Value::as_str) {
                    let cut: String = s.chars().take(DETAIL_CUT).collect();
                    e[k] = Value::String(cut);
                }
            }
        }
    }
    if size(body) > ENTRY_MAX {
        if let Some(o) = body.as_object_mut() {
            o.insert("evidence".into(), Value::Array(Vec::new()));
        }
    }
    if size(body) > ENTRY_MAX {
        if let Some(o) = body.as_object_mut() {
            o.retain(|k, _| matches!(k.as_str(), "kind" | "layer" | "confidence" | "at"));
        }
    }
}

fn size(v: &Value) -> usize {
    serde_json::to_vec(v).map_or(usize::MAX, |b| b.len())
}

#[cfg(test)]
mod tests;
