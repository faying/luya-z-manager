//! Event log (u60-platform.md §3, Phase 2 step 4): the published block changes, in
//! the ledger's envelope (`envelope.rs`: segments, CRC, per-segment name table).
//!
//! Records (kinds from the shared table in `record.rs`):
//!
//! - `KIND_CHECKPOINT`: every block's last state. The first record of every segment
//!   (first start, rotation, after a damaged tail), so any one segment is enough to
//!   rebuild the state and pruning old segments never strands the newer ones.
//! - `KIND_BLOCK`: one published change (`block::BlockEvent`; a block is only
//!   published when its revision moves, D9). Blocks with a gap in `log_gap` (live)
//!   are written at most that often; the newest value always goes into the next
//!   checkpoint and a stale flip is written at once.
//! - `KIND_DELTA`: the same change as only the fields that differ from the block's
//!   previous record (object keys recursed, anything else replaced whole; keys that
//!   went away listed apart from values set to null), with that record's revision as `base`.
//!   Written whenever it is smaller than the full record (owner 10-07: write only what
//!   changed; measured 10-08 the log is over D9's 8 MB/day without it, so it is the
//!   normal record, not a switch at the budget). Never the first record of a block
//!   after open (revisions restart with the process), never right after a checkpoint,
//!   never next to an `untyped` value. Read back only when the state's revision equals
//!   `base`; otherwise that block is dropped from the state (counted, rebuilt from
//!   ubus). An older build skips deltas as an unknown kind and keeps the block's last
//!   full value until the next full record or checkpoint.
//!
//! Every record carries boot id (name table) + ms since boot (CLOCK_BOOTTIME, the
//! clock behind /proc/uptime, as the ledger) + wall clock + "wall clock set" (C11).
//! Object keys and block names are name-table indexes.
//!
//! Reading back (T5 / D5), so a rolled-back older build keeps working on a newer log:
//! records of unknown kinds or of a newer schema are skipped and counted, not
//! treated as damage; a newer checkpoint is passed over for an earlier one, whose
//! following block records are replayed (newer ones skipped again); with no usable
//! checkpoint left the state starts empty and comes back from ubus on the next round.
//! Schema rule: adding a field (`#[serde(default)]`) keeps the schema; only a change
//! of meaning bumps it. Enums in records are unit-only with `#[serde(other)] Unknown`.
//! Every format change adds a golden under `tests/evlog/` (hex text) that this build must read.
//!
//! Blocks enter through their contracts (`contract.rs`, T7): decoded, then redacted for
//! the log (secret fields become keyed references); a block that does not decode is
//! written as `{"untyped":true}`.
//!
//! On disk (`EvLog::on_disk`, owner 10-07; nothing runs it on the device yet, the
//! resident read path comes with Phase 3): `/data/u60d/evlog/` (0700), the reference key
//! in `/data/u60d/evlog.key` (32 bytes, 0600, created once, never in the log or a
//! config backup; a broken key file is not replaced: memory mode instead). D9: raw
//! events kept 24 hours (a segment goes once the next one started more than 24 h ago,
//! by wall clock when it is set) and at most `DISK_KEEP` bytes; a stale flip (a block
//! going away or coming back: no network, a failed read) is fsynced at once, at most
//! `FSYNC_PER_HOUR` an hour; everything else is appended every `FLUSH_AT` bytes and
//! fsynced on close. Bytes per day are counted against `DAY_BUDGET` and reported.

use crate::block::BlockEvent;
use crate::contract::{Checked, Refused};
use crate::envelope::{self, HEADER, KIND_NAMES, Media, Raw, Store};
use crate::record::{KIND_BLOCK, KIND_CHECKPOINT, KIND_DELTA};
use crate::sens::{Ctx, To};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::time::Duration;

/// Schema of the event-log records this build writes and fully understands.
pub const SCHEMA: u16 = 1;
/// Memory mode (until T7): 256 KB segments, 2 MB kept.
pub const MEM_SEG_MAX: u64 = 256 << 10;
pub const MEM_KEEP: u64 = 2 << 20;
/// Queued bytes that trigger an append (same as the ledger).
const FLUSH_AT: usize = 64 << 10;
/// On disk: 1 MB segments, at most 16 MB kept (and 24 hours, `KEEP_SECS`).
pub const DISK_SEG_MAX: u64 = 1 << 20;
pub const DISK_KEEP: u64 = 16 << 20;
/// D9: raw events are kept 24 hours.
pub const KEEP_SECS: i64 = 86_400;
/// D9: at most 8 MB a day written (checkpoints included); counted and reported.
pub const DAY_BUDGET: u64 = 8 << 20;
/// D9: critical records fsynced at once, at most this many an hour.
pub const FSYNC_PER_HOUR: usize = 60;

/// How often a block that changes on every round goes into the log at most.
///
/// Measured on the device 10-07 (10 min, screen off, a round every ~5.2 s): battery
/// and live move every round (~16.5k/day; sysfs current/voltage, counters), signal
/// ~5.9k/day (its own threshold policy), uci_device_info ~4k/day (it carries traffic
/// counters and the radio_* copies of signal); the other blocks did not move. Real
/// record sizes: live ~1.0 KB, uci_device_info ~0.96 KB, signal ~0.66 KB, battery
/// ~0.09 KB. Unlimited that is ~10.8 MB/day, over D9's 8 MB; with these gaps
/// ~7.2 MB/day (signal ~3.9 of it). Next lever if needed: write only the changed
/// fields against the block's previous record.
pub fn log_gap(block: &str) -> Duration {
    match block {
        "live" | "battery" | "uci_device_info" => Duration::from_secs(60),
        _ => Duration::ZERO,
    }
}

/// Time of a record: boot id, ms since boot, wall clock seconds, wall clock set.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Tm {
    pub boot: String,
    pub mono_ms: u64,
    pub wall: i64,
    pub wall_ok: bool,
}

impl Tm {
    pub fn now(boot: &str) -> Tm {
        let wall = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let m = crate::ledger::meta(boot, boottime_ms(), wall);
        Tm {
            boot: m.boot,
            mono_ms: m.mono_ms,
            wall: m.wall,
            wall_ok: m.wall_ok,
        }
    }
}

/// CLOCK_BOOTTIME in ms (what /proc/uptime shows), without opening a file: this
/// runs inside the hub lock.
pub fn boottime_ms() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime writes into the timespec we own.
    if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) } != 0 {
        return 0;
    }
    ts.tv_sec as u64 * 1000 + ts.tv_nsec as u64 / 1_000_000
}

pub fn boot_id() -> String {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// One block as the log knows it.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockState {
    pub revision: u64,
    pub observed_at: u64,
    pub stale: bool,
    pub data: Value,
}

/// Why a checkpoint was written.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// first segment of this process
    Start,
    /// the previous segment was full, or ended in a damaged tail
    Segment,
    #[serde(other)]
    Unknown,
}

/// On-disk forms. Strings that repeat (block names, object keys, boot id) are
/// name-table indexes; tuples keep records small (same idea as `record::MinuteC`).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
struct BlockC {
    /// boot (name), mono_ms, wall, wall_ok
    m: (u32, u64, i64, bool),
    /// block (name), revision, observed_at, stale
    b: (u32, u64, u64, bool),
    d: ciborium::Value,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
struct DeltaC {
    /// boot (name), mono_ms, wall, wall_ok
    m: (u32, u64, i64, bool),
    /// block (name), revision, observed_at, stale
    b: (u32, u64, u64, bool),
    /// revision of the block's record this applies to
    base: u64,
    /// object key path (names) and its new value (null included)
    p: Vec<(Vec<u32>, ciborium::Value)>,
    /// object key paths that went away (kept apart from `p`: in CBOR a `Some(null)`
    /// and a `None` would read back the same)
    #[serde(default)]
    g: Vec<Vec<u32>>,
}

/// The time of a checkpoint, without its state (segment ages at open).
#[derive(Deserialize)]
struct CheckpointHead {
    m: (u32, u64, i64, bool),
}

/// One changed field of a block: object key path, new value (`None` = key gone).
type Change = (Vec<String>, Option<Value>);

/// Where `new` differs from `old`: object keys recursed, anything else replaced whole.
fn changes(old: &Value, new: &Value, path: &mut Vec<String>, out: &mut Vec<Change>) {
    match (old, new) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in b {
                path.push(k.clone());
                match a.get(k) {
                    Some(o) => changes(o, v, path, out),
                    None => out.push((path.clone(), Some(v.clone()))),
                }
                path.pop();
            }
            for k in a.keys().filter(|k| !b.contains_key(*k)) {
                path.push(k.clone());
                out.push((path.clone(), None));
                path.pop();
            }
        }
        _ if old != new => out.push((path.clone(), Some(new.clone()))),
        _ => {}
    }
}

fn apply_changes(v: &mut Value, ch: Vec<Change>) -> Option<()> {
    for (path, val) in ch {
        let Some((last, init)) = path.split_last() else {
            *v = val.unwrap_or(Value::Null);
            continue;
        };
        let mut cur = &mut *v;
        for k in init {
            cur = cur
                .as_object_mut()?
                .entry(k.clone())
                .or_insert_with(|| Value::Object(Default::default()));
        }
        let obj = cur.as_object_mut()?;
        match val {
            Some(x) => {
                obj.insert(last.clone(), x);
            }
            None => {
                obj.remove(last);
            }
        }
    }
    Some(())
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
struct CheckpointC {
    m: (u32, u64, i64, bool),
    r: Reason,
    /// block (name), revision, observed_at, stale, data
    s: Vec<(u32, u64, u64, bool, ciborium::Value)>,
}

trait Names {
    fn id(&mut self, s: &str) -> u32;
}

impl Names for Store {
    fn id(&mut self, s: &str) -> u32 {
        self.intern(s)
    }
}

impl Names for Vec<String> {
    fn id(&mut self, s: &str) -> u32 {
        match self.iter().position(|x| x == s) {
            Some(i) => i as u32,
            None => {
                self.push(s.to_string());
                (self.len() - 1) as u32
            }
        }
    }
}

fn pack_tm(t: &Tm, n: &mut dyn Names) -> (u32, u64, i64, bool) {
    (n.id(&t.boot), t.mono_ms, t.wall, t.wall_ok)
}

fn unpack_tm(m: (u32, u64, i64, bool), names: &[String]) -> Option<Tm> {
    Some(Tm {
        boot: names.get(m.0 as usize)?.clone(),
        mono_ms: m.1,
        wall: m.2,
        wall_ok: m.3,
    })
}

/// JSON → CBOR with object keys as name indexes (JSON keys are always strings, so
/// an integer key is never ambiguous).
fn pack_value(v: &Value, n: &mut dyn Names) -> ciborium::Value {
    use ciborium::Value as C;
    match v {
        Value::Null => C::Null,
        Value::Bool(b) => C::Bool(*b),
        Value::Number(x) => {
            if let Some(u) = x.as_u64() {
                C::Integer(u.into())
            } else if let Some(i) = x.as_i64() {
                C::Integer(i.into())
            } else {
                C::Float(x.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(s) => C::Text(s.clone()),
        Value::Array(a) => C::Array(a.iter().map(|x| pack_value(x, n)).collect()),
        Value::Object(o) => C::Map(
            o.iter()
                .map(|(k, x)| (C::Integer(n.id(k).into()), pack_value(x, n)))
                .collect(),
        ),
    }
}

fn unpack_value(v: &ciborium::Value, names: &[String]) -> Option<Value> {
    use ciborium::Value as C;
    Some(match v {
        C::Null => Value::Null,
        C::Bool(b) => Value::Bool(*b),
        C::Integer(i) => {
            let i = i128::from(*i);
            if let Ok(u) = u64::try_from(i) {
                Value::from(u)
            } else {
                Value::from(i64::try_from(i).ok()?)
            }
        }
        C::Float(f) => serde_json::Number::from_f64(*f).map_or(Value::Null, Value::Number),
        C::Text(s) => Value::String(s.clone()),
        C::Array(a) => Value::Array(
            a.iter()
                .map(|x| unpack_value(x, names))
                .collect::<Option<_>>()?,
        ),
        C::Map(m) => {
            let mut o = serde_json::Map::new();
            for (k, x) in m {
                let i = u32::try_from(i128::from(k.as_integer()?)).ok()?;
                o.insert(names.get(i as usize)?.clone(), unpack_value(x, names)?);
            }
            Value::Object(o)
        }
        _ => return None,
    })
}

fn cbor<T: Serialize>(v: &T) -> Vec<u8> {
    let mut out = Vec::new();
    ciborium::into_writer(v, &mut out).expect("cbor encode");
    out
}

/// Bytes and records written, per block (for the D9 budget).
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct BlockStats {
    pub records: u64,
    pub bytes: u64,
    /// of `records`, written as the changed fields only (`KIND_DELTA`)
    pub deltas: u64,
    /// changes not written because of `log_gap` (they reach the next record or checkpoint)
    pub held: u64,
    /// changes whose fields the contract did not all name (the rest was logged)
    pub inexact: u64,
    /// changes that did not pass the contract (logged as `{"untyped":true}`)
    pub untyped: u64,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct Stats {
    pub blocks: BTreeMap<String, BlockStats>,
    pub checkpoints: u64,
    pub checkpoint_bytes: u64,
    /// names records
    pub name_bytes: u64,
    /// bytes queued today (days since boot), against `DAY_BUDGET`
    pub day_bytes: u64,
    /// days (since this log was opened) that went over `DAY_BUDGET`
    pub days_over: u64,
    /// fsyncs for critical records, and those left to the next append (hourly cap)
    pub fsyncs: u64,
    pub fsyncs_capped: u64,
}

/// What reading a log back found.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Replay {
    pub state: BTreeMap<String, BlockState>,
    /// seq of the checkpoint the state starts from; None = no usable checkpoint
    /// (start empty, rebuild from ubus)
    pub from: Option<u64>,
    /// records of kinds this build does not know
    pub unknown: u64,
    /// records (checkpoints included) written by a newer schema
    pub newer: u64,
    /// records of a known kind that did not decode
    pub undecodable: u64,
    /// deltas whose base was not the state's revision (that block left out of the state)
    pub delta_missed: u64,
    /// bytes after the last good record of a segment
    pub dropped_bytes: usize,
    /// time of the last record applied
    pub last: Option<Tm>,
}

/// Rebuilds the state from segments (oldest first, as `Store::read_all` returns them).
pub fn replay(groups: &[Vec<Raw>], dropped_bytes: usize) -> Replay {
    let mut out = Replay {
        dropped_bytes,
        ..Default::default()
    };
    let names: Vec<Vec<String>> = groups.iter().map(|g| envelope::segment_names(g)).collect();
    // newest checkpoint this build can read
    let mut start: Option<(usize, usize)> = None;
    'find: for (gi, g) in groups.iter().enumerate().rev() {
        for (ri, r) in g.iter().enumerate().rev() {
            if r.kind == KIND_CHECKPOINT
                && r.schema <= SCHEMA
                && let Some(c) = ciborium::from_reader::<CheckpointC, _>(&r.payload[..])
                    .ok()
                    .and_then(|c| apply_checkpoint(&c, &names[gi]))
            {
                out.state = c.0;
                out.last = Some(c.1);
                out.from = Some(r.seq);
                start = Some((gi, ri + 1));
                break 'find;
            }
        }
    }
    let Some((g0, r0)) = start else {
        // nothing to start from: count what is there, keep the state empty
        for g in groups {
            for r in g {
                count_skip(r, &mut out);
            }
        }
        return out;
    };
    for (gi, g) in groups.iter().enumerate().skip(g0) {
        let first = if gi == g0 { r0 } else { 0 };
        for r in &g[first..] {
            match (r.kind, r.schema <= SCHEMA) {
                (KIND_NAMES, _) => {}
                (KIND_BLOCK, true) => {
                    match ciborium::from_reader::<BlockC, _>(&r.payload[..])
                        .ok()
                        .and_then(|b| apply_block(&b, &names[gi]))
                    {
                        Some((name, st, tm)) => {
                            out.state.insert(name, st);
                            out.last = Some(tm);
                        }
                        None => out.undecodable += 1,
                    }
                }
                (KIND_DELTA, true) => {
                    match ciborium::from_reader::<DeltaC, _>(&r.payload[..])
                        .ok()
                        .map(|d| apply_delta(&d, &names[gi], &mut out.state))
                    {
                        Some(Ok(tm)) => out.last = Some(tm),
                        Some(Err(true)) => out.delta_missed += 1,
                        Some(Err(false)) | None => out.undecodable += 1,
                    }
                }
                // a later checkpoint of this schema: already the same state; skip
                (KIND_CHECKPOINT, true) => {}
                _ => count_skip(r, &mut out),
            }
        }
    }
    out
}

fn count_skip(r: &Raw, out: &mut Replay) {
    match r.kind {
        KIND_NAMES => {}
        KIND_BLOCK | KIND_CHECKPOINT | KIND_DELTA if r.schema > SCHEMA => out.newer += 1,
        KIND_BLOCK | KIND_CHECKPOINT | KIND_DELTA => {}
        _ if crate::record::known_kind(r.kind) => {}
        _ => out.unknown += 1,
    }
}

fn apply_checkpoint(
    c: &CheckpointC,
    names: &[String],
) -> Option<(BTreeMap<String, BlockState>, Tm)> {
    let tm = unpack_tm(c.m, names)?;
    let mut m = BTreeMap::new();
    for (n, rev, obs, stale, d) in &c.s {
        m.insert(
            names.get(*n as usize)?.clone(),
            BlockState {
                revision: *rev,
                observed_at: *obs,
                stale: *stale,
                data: unpack_value(d, names)?,
            },
        );
    }
    Some((m, tm))
}

/// A delta onto the state. `Err(true)`: its base is not the state's revision (the block
/// is dropped from the state); `Err(false)`: it does not decode.
fn apply_delta(
    d: &DeltaC,
    names: &[String],
    state: &mut BTreeMap<String, BlockState>,
) -> Result<Tm, bool> {
    let name = names.get(d.b.0 as usize).ok_or(false)?;
    let tm = unpack_tm(d.m, names).ok_or(false)?;
    let path_of = |p: &Vec<u32>| {
        p.iter()
            .map(|i| names.get(*i as usize).cloned())
            .collect::<Option<Vec<_>>>()
    };
    let mut ch = Vec::with_capacity(d.p.len() + d.g.len());
    for (path, v) in &d.p {
        ch.push((
            path_of(path).ok_or(false)?,
            Some(unpack_value(v, names).ok_or(false)?),
        ));
    }
    for path in &d.g {
        ch.push((path_of(path).ok_or(false)?, None));
    }
    let Some(st) = state.get_mut(name).filter(|st| st.revision == d.base) else {
        state.remove(name);
        return Err(true);
    };
    apply_changes(&mut st.data, ch).ok_or(false)?;
    st.revision = d.b.1;
    st.observed_at = d.b.2;
    st.stale = d.b.3;
    Ok(tm)
}

fn apply_block(b: &BlockC, names: &[String]) -> Option<(String, BlockState, Tm)> {
    Some((
        names.get(b.b.0 as usize)?.clone(),
        BlockState {
            revision: b.b.1,
            observed_at: b.b.2,
            stale: b.b.3,
            data: unpack_value(&b.d, names)?,
        },
        unpack_tm(b.m, names)?,
    ))
}

/// The writer. Lives behind the hub's sink (`LogSink`); every call only queues bytes
/// (memory media: the append is a copy).
pub struct EvLog {
    store: Store,
    boot: String,
    state: BTreeMap<String, BlockState>,
    /// per block: mono_ms of the last written record
    written_at: HashMap<String, u64>,
    /// newest change not written yet because of `log_gap`, as published (not yet
    /// through the contract: that runs only for what gets written)
    held: BTreeMap<String, Held>,
    /// per block: payload bytes of its last record (segment size estimates)
    sizes: HashMap<String, usize>,
    started: bool,
    pub stats: Stats,
    /// key for secret references (never written)
    key: Vec<u8>,
    door: Door,
    /// blocks already reported as not passing their contract
    warned: std::collections::HashSet<String>,
    /// blocks with a full record since open (deltas only after one)
    full_once: std::collections::HashSet<String>,
    /// checkpoint seq -> its wall clock (when set): segment ages for `KEEP_SECS`
    seg_wall: BTreeMap<u64, i64>,
    /// mono_ms of the fsyncs in the last hour
    fsync_at: std::collections::VecDeque<u64>,
    /// days since boot of `stats.day_bytes`
    day: u64,
}

/// What a block passes through before it is logged: its contract, redacted for the
/// log (`contract::redact`). Tests of the log mechanics use `open_door`.
pub type Door = fn(&str, &Value, &Ctx) -> Result<Checked, Refused>;

/// A change waiting for its block's gap.
#[derive(Debug, Clone)]
struct Held {
    revision: u64,
    observed_at: u64,
    stale: bool,
    data: Value,
}

/// Stands in for a block that does not pass its contract: never the plain value.
pub fn untyped() -> Value {
    serde_json::json!({ "untyped": true })
}

impl EvLog {
    /// Opens a log; what is already there seeds the state (see `replay`).
    pub fn open(
        media: Box<dyn Media>,
        seg_max: u64,
        keep: u64,
        boot: &str,
        key: Vec<u8>,
        door: Door,
    ) -> io::Result<(EvLog, Replay)> {
        let store = Store::open_on(media, seg_max, keep)?;
        let (groups, dropped) = store.read_all()?;
        let r = replay(&groups, dropped);
        let mut seg_wall = BTreeMap::new();
        for g in &groups {
            let names = envelope::segment_names(g);
            for rec in g.iter().filter(|r| r.kind == KIND_CHECKPOINT) {
                if let Some(t) = ciborium::from_reader::<CheckpointHead, _>(&rec.payload[..])
                    .ok()
                    .and_then(|h| unpack_tm(h.m, &names))
                    .filter(|t| t.wall_ok)
                {
                    seg_wall.insert(rec.seq, t.wall);
                }
            }
        }
        Ok((
            EvLog {
                store,
                boot: boot.to_string(),
                state: r.state.clone(),
                written_at: HashMap::new(),
                held: BTreeMap::new(),
                sizes: HashMap::new(),
                started: false,
                stats: Stats::default(),
                key,
                door,
                warned: Default::default(),
                full_once: Default::default(),
                seg_wall,
                fsync_at: Default::default(),
                day: 0,
            },
            r,
        ))
    }

    /// On disk in `dir` (0700), secret references keyed by the file at `key_path`
    /// (`load_or_create_key`), blocks through their contracts.
    pub fn on_disk(
        dir: &std::path::Path,
        key_path: &std::path::Path,
        boot: &str,
    ) -> io::Result<(EvLog, Replay)> {
        let key = load_or_create_key(key_path)?;
        private_dir(dir)?;
        let media = envelope::Dir::open(dir)?;
        EvLog::open(
            Box::new(media),
            DISK_SEG_MAX,
            DISK_KEEP,
            boot,
            key,
            crate::contract::redact,
        )
    }

    /// `on_disk`, or memory mode (said once on stderr) when the directory or the key
    /// cannot be used: a broken key file is left for the owner to look at.
    pub fn on_disk_or_memory(
        dir: &std::path::Path,
        key_path: &std::path::Path,
        boot: &str,
    ) -> EvLog {
        match EvLog::on_disk(dir, key_path, boot) {
            Ok((log, _)) => log,
            Err(e) => {
                eprintln!(
                    "luyaz-core: evlog: {}: {e}; keeping the log in memory",
                    dir.display()
                );
                EvLog::memory(boot)
            }
        }
    }

    /// Appends what is queued and fsyncs (exit).
    pub fn close(&mut self) -> io::Result<()> {
        self.store.flush(true)
    }

    /// In memory only (until T7), blocks through their contracts, a fresh key.
    pub fn memory(boot: &str) -> EvLog {
        let key: [u8; 32] = rand::random();
        EvLog::open(
            Box::new(envelope::Mem::default()),
            MEM_SEG_MAX,
            MEM_KEEP,
            boot,
            key.to_vec(),
            crate::contract::redact,
        )
        .expect("memory log")
        .0
    }

    fn stat(&mut self, name: &str) -> &mut BlockStats {
        self.stats.blocks.entry(name.to_string()).or_default()
    }

    /// What the log has written per block (redacted for the log; held changes are not
    /// in it yet; after a restart secret fields are references until the next read).
    pub fn state(&self) -> &BTreeMap<String, BlockState> {
        &self.state
    }

    pub fn read_all(&self) -> io::Result<(Vec<Vec<Raw>>, usize)> {
        self.store.read_all()
    }

    /// One published block change. Within its block's gap it is only held (no
    /// contract work); a stale flip is written at once.
    pub fn on_block(&mut self, ev: &BlockEvent, tm: &Tm) {
        let last_stale = match self.held.get(ev.name) {
            Some(h) => Some(h.stale),
            None => self.state.get(ev.name).map(|o| o.stale),
        };
        let flipped = last_stale != Some(ev.stale);
        // a block going away or coming back (not its first value) goes to disk at once
        let critical = flipped && last_stale.is_some();
        let held = Held {
            revision: ev.revision,
            observed_at: ev.observed_at,
            stale: ev.stale,
            data: ev.data.clone(),
        };
        if !(self.due(ev.name, tm) || flipped) {
            self.held.insert(ev.name.to_string(), held);
            self.stat(ev.name).held += 1;
            return;
        }
        self.held.remove(ev.name);
        let prev = self.admit(ev.name, held);
        self.write_block(ev.name, tm, prev);
        if critical {
            self.sync_now(tm);
        }
    }

    /// fsync now, at most `FSYNC_PER_HOUR` an hour (the rest waits for the next append).
    fn sync_now(&mut self, tm: &Tm) {
        while self
            .fsync_at
            .front()
            .is_some_and(|t| tm.mono_ms.saturating_sub(*t) >= 3_600_000)
        {
            self.fsync_at.pop_front();
        }
        if self.fsync_at.len() >= FSYNC_PER_HOUR {
            self.stats.fsyncs_capped += 1;
            return;
        }
        if let Err(e) = self.store.flush(true) {
            eprintln!("luyaz-core: evlog: {e}");
        }
        self.fsync_at.push_back(tm.mono_ms);
        self.stats.fsyncs += 1;
    }

    /// End of a collection round: write the held changes whose gap is over, so the
    /// last value of a block that stopped changing does not wait for a checkpoint.
    pub fn round_end(&mut self, tm: &Tm) {
        let due: Vec<String> = self
            .held
            .keys()
            .filter(|n| self.due(n, tm))
            .cloned()
            .collect();
        for name in due {
            if let Some(h) = self.held.remove(&name) {
                let prev = self.admit(&name, h);
                self.write_block(&name, tm, prev);
            }
        }
    }

    fn due(&self, name: &str, tm: &Tm) -> bool {
        let gap = log_gap(name).as_millis() as u64;
        match self.written_at.get(name) {
            Some(at) => tm.mono_ms.saturating_sub(*at) >= gap,
            None => true,
        }
    }

    /// Through the contract (redacted for the log) into the log's state; the state it
    /// replaces.
    fn admit(&mut self, name: &str, h: Held) -> Option<BlockState> {
        let ctx = Ctx {
            to: To::Log,
            key: &self.key,
        };
        let data = match (self.door)(name, &h.data, &ctx) {
            Ok(c) => {
                if !c.exact {
                    self.stat(name).inexact += 1;
                }
                c.value
            }
            Err(why) => {
                // the reason only (serde's message would quote the value), once per block
                if self.warned.insert(name.to_string()) {
                    eprintln!("luyaz-core: evlog: {name} not logged ({why:?})");
                }
                self.stat(name).untyped += 1;
                untyped()
            }
        };
        self.state.insert(
            name.to_string(),
            BlockState {
                revision: h.revision,
                observed_at: h.observed_at,
                stale: h.stale,
                data,
            },
        )
    }

    fn write_block(&mut self, name: &str, tm: &Tm, prev: Option<BlockState>) {
        let Some(st) = self.state.get(name).cloned() else {
            return;
        };
        let est = HEADER + 64 + self.sizes.get(name).copied().unwrap_or(4096);
        let checkpointed = self.start_segment_if_needed(est, tm);
        let before = self.store.pending_len();
        let m = pack_tm(tm, &mut self.store);
        let b = (
            self.store.intern(name),
            st.revision,
            st.observed_at,
            st.stale,
        );
        let full = cbor(&BlockC {
            m,
            b,
            d: pack_value(&st.data, &mut self.store),
        });
        let delta = match prev {
            Some(p)
                if !checkpointed
                    && self.full_once.contains(name)
                    && p.data != untyped()
                    && st.data != untyped() =>
            {
                let mut ch = Vec::new();
                changes(&p.data, &st.data, &mut Vec::new(), &mut ch);
                let (mut set, mut gone) = (Vec::new(), Vec::new());
                for (path, v) in &ch {
                    let ids: Vec<u32> = path.iter().map(|k| self.store.intern(k)).collect();
                    match v {
                        Some(v) => set.push((ids, pack_value(v, &mut self.store))),
                        None => gone.push(ids),
                    }
                }
                Some(cbor(&DeltaC {
                    m,
                    b,
                    base: p.revision,
                    p: set,
                    g: gone,
                }))
            }
            _ => None,
        };
        let (kind, payload) = match delta {
            Some(d) if d.len() < full.len() => (KIND_DELTA, d),
            _ => (KIND_BLOCK, full),
        };
        self.store.push(SCHEMA, kind, &payload);
        let added = (self.store.pending_len() - before) as u64;
        let rec_bytes = (HEADER + payload.len()) as u64;
        self.stats.name_bytes += added - rec_bytes;
        self.count_day(added, tm);
        let s = self.stats.blocks.entry(name.to_string()).or_default();
        s.records += 1;
        s.bytes += rec_bytes;
        if kind == KIND_DELTA {
            s.deltas += 1;
        } else {
            self.full_once.insert(name.to_string());
            // segment-size estimates use the full size
            self.sizes.insert(name.to_string(), payload.len());
        }
        self.written_at.insert(name.to_string(), tm.mono_ms);
        self.maybe_flush();
    }

    /// Bytes against today's `DAY_BUDGET` (days since boot).
    fn count_day(&mut self, n: u64, tm: &Tm) {
        let day = tm.mono_ms / 86_400_000;
        if day != self.day {
            self.day = day;
            self.stats.day_bytes = 0;
        }
        let was = self.stats.day_bytes;
        self.stats.day_bytes += n;
        if was <= DAY_BUDGET && self.stats.day_bytes > DAY_BUDGET {
            self.stats.days_over += 1;
            eprintln!(
                "luyaz-core: evlog: over the {} MB daily budget (D9)",
                DAY_BUDGET >> 20
            );
        }
    }

    /// Drops segments whose successor started more than `KEEP_SECS` ago (D9: raw events
    /// 24 hours). Needs a set wall clock; the byte cap holds either way.
    fn prune_by_age(&mut self, tm: &Tm) {
        if !tm.wall_ok {
            return;
        }
        let Ok(segs) = self.store.segments() else {
            return;
        };
        let cutoff = tm.wall - KEEP_SECS;
        let mut kept_from = None;
        for w in segs.windows(2) {
            let next_start = self.seg_wall.range(w[1]..).next().map(|(_, t)| *t);
            if next_start.is_some_and(|t| t < cutoff) {
                if let Err(e) = self.store.remove_segment(w[0]) {
                    eprintln!("luyaz-core: evlog: {e}");
                    break;
                }
                kept_from = Some(w[1]);
            } else {
                break;
            }
        }
        if let Some(first) = kept_from {
            self.seg_wall.retain(|k, _| *k >= first);
        }
    }

    /// Every new segment starts with a checkpoint (and the names it uses); true when
    /// one was written.
    fn start_segment_if_needed(&mut self, est: usize, tm: &Tm) -> bool {
        let checkpoint_est = HEADER + 64 + self.state_size();
        if let Err(e) = self.store.begin(est + checkpoint_est) {
            eprintln!("luyaz-core: evlog: {e}");
        }
        if self.store.at_segment_start() {
            let reason = if self.started {
                Reason::Segment
            } else {
                Reason::Start
            };
            self.started = true;
            self.write_checkpoint(reason, tm);
            self.prune_by_age(tm);
            return true;
        }
        false
    }

    /// Checkpoint size estimate from the blocks' last record sizes (no serializing).
    fn state_size(&self) -> usize {
        self.state
            .keys()
            .map(|n| self.sizes.get(n).copied().unwrap_or(4096) + 24)
            .sum()
    }

    fn write_checkpoint(&mut self, reason: Reason, tm: &Tm) {
        // everything held goes into the checkpoint
        for (name, h) in std::mem::take(&mut self.held) {
            let _ = self.admit(&name, h);
            self.written_at.insert(name, tm.mono_ms);
        }
        let before = self.store.pending_len();
        let m = pack_tm(tm, &mut self.store);
        let mut s = Vec::with_capacity(self.state.len());
        for (name, st) in &self.state {
            s.push((
                self.store.intern(name),
                st.revision,
                st.observed_at,
                st.stale,
                pack_value(&st.data, &mut self.store),
            ));
        }
        let payload = cbor(&CheckpointC { m, r: reason, s });
        let seq = self.store.push(SCHEMA, KIND_CHECKPOINT, &payload);
        if tm.wall_ok {
            self.seg_wall.insert(seq, tm.wall);
        }
        let rec_bytes = (HEADER + payload.len()) as u64;
        let added = (self.store.pending_len() - before) as u64;
        self.stats.checkpoints += 1;
        self.stats.checkpoint_bytes += rec_bytes;
        self.stats.name_bytes += added - rec_bytes;
        self.count_day(added, tm);
    }

    fn maybe_flush(&mut self) {
        if self.store.pending_len() >= FLUSH_AT
            && let Err(e) = self.store.flush(false)
        {
            eprintln!("luyaz-core: evlog: {e}");
        }
    }

    /// Bytes appended to the media so far plus what is queued.
    pub fn bytes(&self) -> u64 {
        self.store.written + self.store.pending_len() as u64
    }

    pub fn boot(&self) -> &str {
        &self.boot
    }
}

/// A directory only the owner can enter (the log holds identifiers).
fn private_dir(dir: &std::path::Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

/// The 32-byte reference key at `path`: read, or created (0600 from the first byte:
/// a temp file opened with that mode, fsynced, renamed) only when there is none. A
/// file that is there but not 32 bytes is an error and is left alone: replacing it
/// would silently break every reference already in the log.
pub fn load_or_create_key(path: &std::path::Path) -> io::Result<Vec<u8>> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    match std::fs::read(path) {
        Ok(k) if k.len() == 32 => return Ok(k),
        Ok(k) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{}: {} bytes, not 32; left as it is",
                    path.display(),
                    k.len()
                ),
            ));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    if let Some(parent) = path.parent() {
        private_dir(parent)?;
    }
    let key: [u8; 32] = rand::random();
    let tmp = path.with_extension("key.new");
    match std::fs::remove_file(&tmp) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(&key)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)?;
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(key.to_vec())
}

/// `block::EventSink` that queues block events into an `EvLog`. Heartbeats are not
/// logged (they only say "still alive"; the log has its own times); every round ends
/// with one, so they drive `EvLog::round_end`.
pub struct LogSink(pub std::sync::Arc<std::sync::Mutex<EvLog>>);

impl crate::block::EventSink for LogSink {
    fn emit(&self, event: &crate::block::Event) {
        let mut log = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let tm = Tm::now(&log.boot);
        match event {
            crate::block::Event::Block(b) => log.on_block(b, &tm),
            crate::block::Event::Heartbeat(_) => log.round_end(&tm),
        }
    }
}

#[cfg(test)]
mod tests;
