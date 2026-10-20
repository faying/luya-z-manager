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
//! Identity (decision-execution-loop.md §4.1 D2): [`Store::epoch`] is random per load (never
//! persisted) and every material change (§4.3 E4: layer, confidence, a layer's evidence level,
//! the "up" count starting or clearing) gives the entry the next store-wide `revision`
//! (persisted, monotonic); re-seeing the same finding every 5 s does not. A proposal binds
//! both; an entry from an older file without a revision, or a store without an epoch, is never
//! executable ([`Store::identity`] = None). The revision rides along whenever the store is saved; it does not add saves.
//!
//! Clock (local time labelled UTC, and wrong until SNTP after a boot): a suppression is never
//! treated as expired while the clock is before `record::CLOCK_SET`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use luyaz_api::clock::BootTime;
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
    /// Store-wide revision of its last evidence/state change; None = from a file before D2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Suppressed {
    pub kind: String,
    pub iccid: String,
    pub until: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dismissed_at: Option<BootTime>,
}

impl Suppressed {
    fn active_at(&self, now: u64, boot: Option<&BootTime>) -> bool {
        if let (Some(now_b), Some(then_b)) = (boot, self.dismissed_at.as_ref()) {
            if !now_b.boot_id.is_empty() && now_b.boot_id == then_b.boot_id {
                if let Some(age) = now_b.elapsed_since(then_b) {
                    return age < SUPPRESS_SECS;
                }
                // Same boot_id but `then_b` is in the future (corrupted persisted boot timestamp):
                // fall through to the wall-clock check rather than holding forever.
            }
        }
        if !clock_set(now) || !clock_set(self.until) {
            return true;
        }
        self.until > now
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub open: Vec<Entry>,
    #[serde(default)]
    pub suppressed: Vec<Suppressed>,
    /// The last revision handed out.
    #[serde(default)]
    pub revision: u64,
    /// This load's random identity; "" = unknown, nothing executable.
    #[serde(skip)]
    pub epoch: String,
}

/// What a proposal binds to (D2): this load's epoch and the entry's revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub epoch: String,
    pub revision: u64,
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
        // a hand-edited or damaged file: never hand out a revision already used
        let top = s.open.iter().filter_map(|e| e.revision).max().unwrap_or(0);
        s.revision = s.revision.max(top);
        s
    }

    /// This load's identity (random, from the caller).
    pub fn with_epoch(mut self, epoch: impl Into<String>) -> Store {
        self.epoch = epoch.into();
        self
    }

    /// The open entry's identity, or None when it can't be bound (no epoch, no revision).
    pub fn identity(&self, kind: &str, iccid: &str) -> Option<Identity> {
        if self.epoch.is_empty() {
            return None;
        }
        let e = self
            .open
            .iter()
            .find(|e| e.kind == kind && e.iccid == iccid)?;
        Some(Identity {
            epoch: self.epoch.clone(),
            revision: e.revision?,
        })
    }

    fn next_revision(&mut self) -> u64 {
        self.revision = self.revision.saturating_add(1);
        self.revision
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    pub fn is_suppressed(&self, kind: &str, iccid: &str, now: u64) -> bool {
        self.is_suppressed_at(kind, iccid, now, None)
    }

    pub fn is_suppressed_at(
        &self,
        kind: &str,
        iccid: &str,
        now: u64,
        boot: Option<&BootTime>,
    ) -> bool {
        self.suppressed
            .iter()
            .any(|s| s.kind == kind && s.iccid == iccid && s.active_at(now, boot))
    }

    /// One check's verdict for the card in now (`iccid`, "" = unknown).
    pub fn observe(&mut self, v: &Verdict, iccid: &str, now: u64) -> Vec<Change> {
        self.observe_at(v, iccid, now, None)
    }

    pub fn observe_at(
        &mut self,
        v: &Verdict,
        iccid: &str,
        now: u64,
        boot: Option<&BootTime>,
    ) -> Vec<Change> {
        self.prune_at(now, boot);
        let mut out = Vec::new();
        // another card: the old card's findings no longer hold (no reading is not another card)
        if !iccid.is_empty() {
            self.close_where(|e| e.iccid != iccid, Close::Expired, &mut out);
        }
        match v {
            // without the card it can't be keyed
            Verdict::Open(f) if f.iccid.is_empty() => {}
            Verdict::Open(f) => self.found(f, now, boot, &mut out),
            Verdict::None { why } => match *why {
                "up" | "all_ok" | "path_dns_ok" => {
                    for i in 0..self.open.len() {
                        // only the first "up" moves the revision (§4.3 E4); the rest just count
                        let r = (self.open[i].ups == 0).then(|| self.next_revision());
                        let e = &mut self.open[i];
                        e.ups = e.ups.saturating_add(1);
                        if r.is_some() {
                            e.revision = r;
                        }
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

    /// A repair proved it fixed (decision loop: confirmed + bound HTTPS recheck passed): closes
    /// the key as resolved without waiting for the passive ups, no suppression. Only within the
    /// load the proposal was bound to (`epoch`); another load or nothing open: no change.
    pub fn resolve(&mut self, kind: &str, iccid: &str, epoch: &str) -> Vec<Change> {
        let mut out = Vec::new();
        if self.epoch.is_empty() || self.epoch != epoch {
            return out;
        }
        self.close_where(
            |e| e.kind == kind && e.iccid == iccid,
            Close::Resolved,
            &mut out,
        );
        out
    }

    /// The user's "ignore": closes it and keeps the key closed for 24 h.
    pub fn dismiss(&mut self, kind: &str, iccid: &str, now: u64) -> Vec<Change> {
        self.dismiss_at(kind, iccid, now, None)
    }

    pub fn dismiss_at(
        &mut self,
        kind: &str,
        iccid: &str,
        now: u64,
        boot: Option<&BootTime>,
    ) -> Vec<Change> {
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
            until: now.saturating_add(SUPPRESS_SECS),
            dismissed_at: boot.cloned().filter(|b| !b.boot_id.is_empty()),
        });
        out
    }

    /// Drop suppressions that ran out (never before the clock is set unless same-boot BootTime
    /// proves 24 h have elapsed).
    pub fn prune(&mut self, now: u64) {
        self.prune_at(now, None);
    }

    pub fn prune_at(&mut self, now: u64, boot: Option<&BootTime>) {
        if clock_set(now) {
            for s in &mut self.suppressed {
                if !clock_set(s.until) {
                    let rem = boot
                        .zip(s.dismissed_at.as_ref())
                        .and_then(|(now_b, then_b)| now_b.elapsed_since(then_b))
                        .map_or(SUPPRESS_SECS, |age| SUPPRESS_SECS.saturating_sub(age));
                    s.until = now.saturating_add(rem);
                }
            }
        }
        self.suppressed.retain(|s| s.active_at(now, boot));
    }

    fn found(&mut self, f: &Finding, now: u64, boot: Option<&BootTime>, out: &mut Vec<Change>) {
        if self.is_suppressed_at(f.kind, &f.iccid, now, boot) {
            return;
        }
        let mut body = serde_json::to_value(f).unwrap_or(Value::Null);
        fit(&mut body);
        if let Some(i) = self
            .open
            .iter()
            .position(|e| e.kind == f.kind && e.iccid == f.iccid)
        {
            let e = &self.open[i];
            let moved = e.finding.get("layer") != body.get("layer")
                || e.finding.get("confidence") != body.get("confidence");
            // a new revision only for a real change (§4.3 E4): layer, confidence, any layer's
            // evidence level, or the "up" count clearing; freshness is the 30 s pre-write check
            let r = (moved || levels(&e.finding) != levels(&body) || e.ups > 0)
                .then(|| self.next_revision());
            let e = &mut self.open[i];
            e.last = now;
            e.count = e.count.saturating_add(1);
            e.ups = 0;
            e.finding = body;
            if r.is_some() {
                e.revision = r;
            }
            if moved {
                out.push(Change::Update(e.clone()));
            }
            return;
        }
        let r = self.next_revision();
        self.make_room(out);
        let e = Entry {
            kind: f.kind.to_string(),
            iccid: f.iccid.clone(),
            first: now,
            last: now,
            count: 1,
            ups: 0,
            finding: body,
            revision: Some(r),
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
            } else if let Some(i) = (0..self.suppressed.len()).min_by(|&a, &b| {
                let sa = &self.suppressed[a];
                let sb = &self.suppressed[b];
                match (&sa.dismissed_at, &sb.dismissed_at) {
                    (Some(da), Some(db)) if !da.boot_id.is_empty() && da.boot_id == db.boot_id => {
                        da.seconds.cmp(&db.seconds)
                    }
                    _ => sa.until.cmp(&sb.until),
                }
            }) {
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
/// Each layer's evidence level, in order; what E4 counts as an evidence change.
fn levels(finding: &Value) -> Vec<(&Value, &Value)> {
    static NONE: Value = Value::Null;
    finding
        .get("evidence")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|v| {
                    (
                        v.get("layer").unwrap_or(&NONE),
                        v.get("level").unwrap_or(&NONE),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

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
