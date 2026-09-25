//! Feed from zwrt-datad: the `/v2/events` subscriber, the 「订阅中 / 退路」
//! state machine (R18) and the "degraded" marker (R5).
//!
//! One thread holds the `/v2` SSE connection and keeps the latest battery,
//! charger, signal and live blocks. Consumers (charge_policy, sms_forward,
//! scenario) read them through [`Feed`] and wait on it with a timeout; the
//! version bumps on every mode change and every battery/charger change, so a
//! state change wakes them at once (and every SMS block max id / count
//! change, for sms_forward, T10). In 退路 (fallback) they read ubus
//! themselves, at most every [`FALLBACK_POLL`].
//!
//! Mode rules (STATE_V2.md V2-1..V2-10, V2-23):
//! - [`SILENCE`] (M = 20 s, monotonic) without any event → drop the
//!   connection, reconnect, and go to fallback. The silence clock is not reset
//!   by reconnecting: a seq gap / epoch change / server close reconnects at
//!   once and, if the snapshot arrives in time, never enters fallback.
//! - battery or charger block stale (or missing) for [`STALE_LIMIT`] (N) →
//!   fallback, even while connected.
//! - Back to subscribed only with a live stream AND both key blocks fresh
//!   (not stale, data present). A datad that has just restarted reports
//!   never-read blocks as stale/null (V2-14); recovering on that would bounce.
//!
//! When datad's `/v2` stops answering, the agent falls back to reading ubus
//! itself. The owner should hear about it if that lasts, but the agent cannot
//! be trusted to text about its own troubles, so it only leaves a marker and
//! u60-guard (touch-ui `scripts/u60-guard.sh`, `datad_round`) sends the SMS.
//!
//! Contract for `/data/u60-guard/datad-degraded` (decision R5):
//! - The agent is its ONLY writer and remover. u60-guard only reads it.
//! - Content: line 1 = epoch seconds when the fallback started, line 2 = a
//!   one-line printable-ASCII reason (<= 120 chars). Plain lines so busybox
//!   `read` can take it apart.
//! - Written on entering the fallback; rewritten (same start, new mtime) at
//!   most every [`REFRESH_EVERY`] seconds while it lasts; deleted on recovery.
//!   On startup any old marker is deleted — the previous agent's episode is
//!   not ours to vouch for — and the state is judged again from `/v2`.
//! - u60-guard alerts once per start time when mtime is < 3 min old and the
//!   start is > 5 min ago. A stale mtime means the agent stopped refreshing:
//!   that is the heartbeat alert's job, so no datad alert then.
//!
//! The feed thread drives it: [`DegradedMarker::startup`] once, then
//! [`DegradedMarker::enter`] / [`DegradedMarker::tick`] (every loop pass, the
//! loop wakes at least every second) / [`DegradedMarker::recover`].

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// Where u60-guard looks for the marker (touch-ui `u60-guard.sh` DATAD_MARKER).
pub const MARKER_PATH: &str = "/data/u60-guard/datad-degraded";
/// Rewrite the marker this often while degraded. u60-guard treats it as
/// stale after 180 s, so this leaves room for two missed refreshes.
pub const REFRESH_EVERY: u64 = 60;
const REASON_MAX: usize = 120;

pub fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The agent's side of the marker. Times are wall-clock epoch seconds passed
/// in by the caller (tests inject them; production uses [`now_epoch`]).
#[derive(Debug)]
pub struct DegradedMarker {
    path: PathBuf,
    /// `Some` while degraded: (start epoch, reason, last write epoch).
    active: Option<Active>,
}

#[derive(Debug, Clone)]
struct Active {
    start: u64,
    reason: String,
    written: u64,
}

impl DegradedMarker {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            active: None,
        }
    }

    pub fn is_degraded(&self) -> bool {
        self.active.is_some()
    }

    /// Start of the current episode, if degraded.
    pub fn since(&self) -> Option<u64> {
        self.active.as_ref().map(|a| a.start)
    }

    /// Call once at agent start, before judging `/v2` health: drops whatever
    /// a previous agent left behind. The caller (T7) then calls [`enter`]
    /// if `/v2` is unhealthy, which starts a fresh episode.
    pub fn startup(&mut self) -> io::Result<()> {
        self.active = None;
        remove_if_present(&self.path)
    }

    /// Entering the fallback. Writes the marker with `now` as the start.
    /// Already degraded: keeps the original start (the episode goes on);
    /// only the reason is updated and the marker refreshed if due.
    pub fn enter(&mut self, reason: &str, now: u64) -> io::Result<()> {
        let reason = clean_reason(reason);
        match &mut self.active {
            Some(a) => {
                a.reason = reason;
                self.tick(now).map(|_| ())
            }
            None => {
                let a = Active {
                    start: now,
                    reason,
                    written: now,
                };
                write_marker(&self.path, a.start, &a.reason)?;
                self.active = Some(a);
                Ok(())
            }
        }
    }

    /// Call every loop pass while degraded. Rewrites the marker (same start)
    /// when [`REFRESH_EVERY`] has passed since the last write, or the clock
    /// went backwards. Also recreates it if something deleted it.
    /// Returns whether it wrote. No-op when not degraded.
    pub fn tick(&mut self, now: u64) -> io::Result<bool> {
        let Some(a) = &mut self.active else {
            return Ok(false);
        };
        let due = now < a.written || now - a.written >= REFRESH_EVERY;
        if !due {
            return Ok(false);
        }
        write_marker(&self.path, a.start, &a.reason)?;
        a.written = now;
        Ok(true)
    }

    /// Back on `/v2`: delete the marker.
    pub fn recover(&mut self) -> io::Result<()> {
        self.active = None;
        remove_if_present(&self.path)
    }
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// One line of printable ASCII, at most [`REASON_MAX`] chars.
fn clean_reason(reason: &str) -> String {
    let s: String = reason
        .chars()
        .map(|c| if c == '\t' || c == '\n' || c == '\r' { ' ' } else { c })
        .filter(|c| c.is_ascii() && !c.is_ascii_control())
        .take(REASON_MAX)
        .collect();
    let s = s.trim();
    if s.is_empty() {
        "unknown".to_string()
    } else {
        s.to_string()
    }
}

/// Write via a temp file in the same directory + rename, so u60-guard never
/// reads half a marker.
fn write_marker(path: &Path, start: u64, reason: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        let mut f = fs::File::create(&tmp)?;
        write!(f, "{start}\n{reason}\n")?;
    }
    fs::rename(&tmp, path)
}

// ── /v2 subscriber ─────────────────────────────────────────────────────────

/// datad's `/v2/events`. Local, no token.
pub const V2_ADDR: &str = "127.0.0.1:9460";
/// M (V2-23): no event at all for this long = the connection is dead.
pub const SILENCE: Duration = Duration::from_secs(20);
/// N: battery or charger stale (or missing) this long = fall back.
pub const STALE_LIMIT: Duration = Duration::from_secs(30);
/// How often a consumer reads ubus itself while in fallback. 30 s, not 60:
/// a plug-in landing just after a read must still be acted on within 60 s.
pub const FALLBACK_POLL: Duration = Duration::from_secs(30);
/// No consumer loop may sleep longer than this (R18, and the marker refresh).
pub const MAX_WAIT: Duration = Duration::from_secs(60);
/// The blocks whose staleness sends us to fallback.
const KEY_BLOCKS: [&str; 2] = ["battery", "charger"];
/// The blocks whose changes wake consumers ([`fingerprint`] picks the
/// fields). `sms` is not a key block: a datad without it (older build) must
/// still count as healthy — sms_forward then just reads ubus itself.
const WAKE_BLOCKS: [&str; 3] = ["battery", "charger", "sms"];

/// One SSE event: `event:` name and the joined `data:` lines.
#[derive(Debug, Clone, PartialEq)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
}

/// Incremental SSE parser. Bytes may be split anywhere (inside a line, inside
/// a UTF-8 character); only complete lines are interpreted.
#[derive(Debug, Default)]
pub struct SseParser {
    line: Vec<u8>,
    event: String,
    data: Vec<String>,
    /// Bytes in `data` so far (one event's worth).
    data_len: usize,
}

/// Longest line / event the parser buffers; beyond it the stream is broken
/// (or hostile) and the connection is dropped (`Err` → Resync).
pub const SSE_MAX_LINE: usize = 1 << 20;

impl SseParser {
    /// Parse `bytes`; `Err` when a line or an event outgrows
    /// [`SSE_MAX_LINE`] (the parser is reset; drop the connection).
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>, String> {
        let mut out = Vec::new();
        for &b in bytes {
            if b != b'\n' {
                if self.line.len() >= SSE_MAX_LINE {
                    *self = SseParser::default();
                    return Err(format!("SSE line over {SSE_MAX_LINE} bytes"));
                }
                self.line.push(b);
                continue;
            }
            let mut line = std::mem::take(&mut self.line);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8_lossy(&line);
            if line.is_empty() {
                if !self.data.is_empty() || !self.event.is_empty() {
                    self.data_len = 0;
                    out.push(SseEvent {
                        event: std::mem::take(&mut self.event),
                        data: std::mem::take(&mut self.data).join("\n"),
                    });
                }
                continue;
            }
            if line.starts_with(':') {
                continue; // comment / keep-alive
            }
            let (field, value) = match line.split_once(':') {
                Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
                None => (&*line, ""),
            };
            match field {
                "event" => self.event = value.to_string(),
                "data" => {
                    self.data_len += value.len() + 1;
                    if self.data_len > SSE_MAX_LINE {
                        *self = SseParser::default();
                        return Err(format!("SSE event over {SSE_MAX_LINE} bytes"));
                    }
                    self.data.push(value.to_string())
                }
                _ => {}
            }
        }
        Ok(out)
    }
}

/// HTTP/1.1 chunked transfer decoding, incremental.
#[derive(Debug, Default)]
struct Chunked {
    buf: Vec<u8>,
    /// Bytes left in the current chunk's payload; `None` = expecting a size line.
    remaining: Option<usize>,
    /// Payload finished, expecting its trailing CRLF.
    need_crlf: bool,
    done: bool,
}

impl Chunked {
    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<u8>, String> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            if self.done {
                return Ok(out);
            }
            if self.need_crlf {
                if self.buf.len() < 2 {
                    return Ok(out);
                }
                if &self.buf[..2] != b"\r\n" {
                    return Err("bad chunk terminator".into());
                }
                self.buf.drain(..2);
                self.need_crlf = false;
            }
            match self.remaining {
                None => {
                    let Some(pos) = self.buf.windows(2).position(|w| w == b"\r\n") else {
                        if self.buf.len() > 64 {
                            return Err("chunk size line too long".into());
                        }
                        return Ok(out);
                    };
                    let line = String::from_utf8_lossy(&self.buf[..pos]).to_string();
                    self.buf.drain(..pos + 2);
                    let hex = line.split(';').next().unwrap_or("").trim();
                    let n = usize::from_str_radix(hex, 16).map_err(|_| format!("bad chunk size {hex:?}"))?;
                    if n == 0 {
                        self.done = true;
                    } else {
                        self.remaining = Some(n);
                    }
                }
                Some(n) => {
                    if self.buf.is_empty() {
                        return Ok(out);
                    }
                    let take = n.min(self.buf.len());
                    out.extend(self.buf.drain(..take));
                    if take == n {
                        self.remaining = None;
                        self.need_crlf = true;
                    } else {
                        self.remaining = Some(n - take);
                    }
                }
            }
        }
    }
}

/// One `/v2` block as last seen.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub revision: u64,
    /// Device clock seconds of the last successful read (from `block`,
    /// `snapshot` or — the only thing that advances it — `heartbeat`).
    pub observed_at: i64,
    pub stale: bool,
    pub data: Value,
}

impl Block {
    fn from_json(v: &Value) -> Option<Block> {
        Some(Block {
            revision: v.get("revision")?.as_u64()?,
            observed_at: v.get("observed_at").and_then(Value::as_i64).unwrap_or(0),
            stale: v.get("stale").and_then(Value::as_bool).unwrap_or(true),
            data: v.get("data").cloned().unwrap_or(Value::Null),
        })
    }

    /// Usable: not stale and has data.
    pub fn fresh(&self) -> bool {
        !self.stale && !self.data.is_null()
    }
}

/// Stream bookkeeping for one datad lifetime: epoch, expected seq, blocks.
/// The blocks survive a reconnect (the next snapshot replaces them).
#[derive(Debug, Default)]
pub struct Stream {
    epoch: Option<String>,
    next_seq: Option<u64>,
    pub blocks: HashMap<String, Block>,
}

/// What applying an event changed.
#[derive(Debug, Default, PartialEq)]
pub struct Applied {
    /// Blocks whose revision/data/stale changed (all of them for a snapshot).
    pub changed: Vec<String>,
    pub snapshot: bool,
}

impl Stream {
    /// A new connection: the first event must be a snapshot again.
    pub fn new_connection(&mut self) {
        self.epoch = None;
        self.next_seq = None;
    }

    /// Apply one event. `Err` = must drop the connection and reconnect
    /// (epoch changed, seq gap, no snapshot first, malformed).
    pub fn apply(&mut self, ev: &SseEvent) -> Result<Applied, String> {
        let v: Value = serde_json::from_str(&ev.data).map_err(|e| format!("bad json: {e}"))?;
        let epoch = v.get("epoch").and_then(Value::as_str).ok_or("no epoch")?;
        let seq = v.get("seq").and_then(Value::as_u64).ok_or("no seq")?;
        // The seq after this one; at u64::MAX there is none: resync.
        let next = seq.checked_add(1).ok_or("seq overflow")?;
        if ev.event == "snapshot" {
            let blocks = v.get("blocks").and_then(Value::as_object).ok_or("snapshot without blocks")?;
            let mut changed = Vec::new();
            let mut fresh = HashMap::new();
            for (name, b) in blocks {
                let b = Block::from_json(b).ok_or_else(|| format!("bad block {name}"))?;
                if self.blocks.get(name) != Some(&b) {
                    changed.push(name.clone());
                }
                fresh.insert(name.clone(), b);
            }
            changed.extend(self.blocks.keys().filter(|k| !fresh.contains_key(*k)).cloned());
            self.blocks = fresh;
            self.epoch = Some(epoch.to_string());
            self.next_seq = Some(next);
            changed.sort();
            return Ok(Applied { changed, snapshot: true });
        }
        let Some(want_epoch) = &self.epoch else {
            return Err(format!("{} before snapshot", ev.event));
        };
        if want_epoch != epoch {
            return Err(format!("epoch changed {want_epoch} -> {epoch}"));
        }
        let want = self.next_seq.unwrap_or(0);
        if seq != want {
            return Err(format!("seq gap: want {want}, got {seq}"));
        }
        self.next_seq = Some(next);
        match ev.event.as_str() {
            "block" => {
                let name = v.get("name").and_then(Value::as_str).ok_or("block without name")?;
                let b = Block::from_json(&v).ok_or("bad block")?;
                let changed = if self.blocks.get(name) != Some(&b) { vec![name.to_string()] } else { vec![] };
                self.blocks.insert(name.to_string(), b);
                Ok(Applied { changed, snapshot: false })
            }
            "heartbeat" => {
                // observed_at only moves through heartbeats: update in place,
                // not a data change.
                if let Some(obs) = v.get("blocks").and_then(Value::as_object) {
                    for (name, t) in obs {
                        if let (Some(b), Some(t)) = (self.blocks.get_mut(name), t.as_i64()) {
                            b.observed_at = t;
                        }
                    }
                }
                Ok(Applied::default())
            }
            _ => Ok(Applied::default()), // unknown type still used its seq
        }
    }
}

/// 订阅中 / 退路.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Just started, not judged yet. Consumers treat it like fallback.
    Starting,
    Subscribed,
    Fallback,
}

/// The pure mode machine. Times are monotonic offsets supplied by the caller.
#[derive(Debug)]
pub struct Health {
    pub mode: Mode,
    last_event: Duration,
    stale_since: HashMap<&'static str, Duration>,
    silence: Duration,
    stale_limit: Duration,
}

/// A mode change the caller must act on.
#[derive(Debug, PartialEq)]
pub enum Transition {
    Enter(String),
    Recover,
}

impl Health {
    /// `now` = start time: counts as the last event, so startup gets M to
    /// reach a healthy snapshot before falling back.
    #[cfg(test)]
    pub fn new(now: Duration) -> Self {
        Self::with_limits(now, SILENCE, STALE_LIMIT)
    }

    pub fn with_limits(now: Duration, silence: Duration, stale_limit: Duration) -> Self {
        Health {
            mode: Mode::Starting,
            last_event: now,
            stale_since: KEY_BLOCKS.iter().map(|k| (*k, now)).collect(),
            silence,
            stale_limit,
        }
    }

    /// Any event arrived.
    pub fn on_event(&mut self, now: Duration, stream: &Stream) {
        self.last_event = now;
        for k in KEY_BLOCKS {
            let fresh = stream.blocks.get(k).is_some_and(Block::fresh);
            if fresh {
                self.stale_since.remove(k);
            } else {
                self.stale_since.entry(k).or_insert(now);
            }
        }
    }

    /// Silence exceeded M: the caller must drop the connection.
    pub fn silent(&self, now: Duration) -> bool {
        now.saturating_sub(self.last_event) >= self.silence
    }

    fn bad_reason(&self, now: Duration) -> Option<String> {
        if self.silent(now) {
            return Some(format!("no v2 event for {}s", now.saturating_sub(self.last_event).as_secs()));
        }
        for k in KEY_BLOCKS {
            if let Some(since) = self.stale_since.get(k) {
                if now.saturating_sub(*since) >= self.stale_limit {
                    return Some(format!("{k} block stale for {}s", now.saturating_sub(*since).as_secs()));
                }
            }
        }
        None
    }

    fn healthy(&self, now: Duration) -> bool {
        !self.silent(now) && self.stale_since.is_empty()
    }

    /// Judge the mode at `now`. Returns the transition to act on, if any.
    pub fn evaluate(&mut self, now: Duration) -> Option<Transition> {
        match self.mode {
            Mode::Subscribed | Mode::Starting => {
                if let Some(reason) = self.bad_reason(now) {
                    self.mode = Mode::Fallback;
                    return Some(Transition::Enter(reason));
                }
                if self.mode == Mode::Starting && self.healthy(now) {
                    self.mode = Mode::Subscribed;
                    return Some(Transition::Recover);
                }
                None
            }
            Mode::Fallback => {
                if self.healthy(now) {
                    self.mode = Mode::Subscribed;
                    Some(Transition::Recover)
                } else {
                    None
                }
            }
        }
    }
}

/// What consumers see.
#[derive(Debug, Clone)]
pub struct View {
    pub mode: Mode,
    /// Bumps on every mode change and every battery/charger/SMS change that
    /// consumers act on ([`fingerprint`]).
    pub version: u64,
    pub blocks: HashMap<String, Block>,
}

impl View {
    pub fn subscribed(&self) -> bool {
        self.mode == Mode::Subscribed
    }

    fn fresh_block(&self, name: &str) -> Option<&Value> {
        self.blocks.get(name).filter(|b| b.fresh()).map(|b| &b.data)
    }

    /// Charger plugged in, per the battery block (`charger_connect`). `None`
    /// when not subscribed, or battery/charger stale, or the field is missing
    /// — "don't know", never "unplugged".
    pub fn charger_connected(&self) -> Option<bool> {
        if !self.subscribed() || self.fresh_block("charger").is_none() {
            return None;
        }
        let v = self.fresh_block("battery")?.get("charger_connect")?.as_i64()?;
        Some(v != 0)
    }

    /// SMS summary `(max_id, count)` from the `sms` block (T10). `None` when
    /// not subscribed or the block is stale/missing (older datad) — then the
    /// SMS forwarder reads ubus itself.
    pub fn sms_summary(&self) -> Option<(u64, u64)> {
        if !self.subscribed() {
            return None;
        }
        let b = self.fresh_block("sms")?;
        Some((b.get("max_id")?.as_u64()?, b.get("count").and_then(Value::as_u64).unwrap_or(0)))
    }

    /// Charging stopped by direct supply (charger block
    /// `direct_supply.enabled`, true = stopped). `None` = don't know.
    pub fn charging_stopped(&self) -> Option<bool> {
        if !self.subscribed() {
            return None;
        }
        self.fresh_block("charger")?.get("direct_supply")?.get("enabled")?.as_bool()
    }
}

/// The shared feed: a watch-style cell (Mutex + Condvar + version).
#[derive(Debug)]
pub struct Feed {
    view: Mutex<View>,
    cv: Condvar,
}

impl Default for Feed {
    fn default() -> Self {
        Feed {
            view: Mutex::new(View { mode: Mode::Starting, version: 0, blocks: HashMap::new() }),
            cv: Condvar::new(),
        }
    }
}

impl Feed {
    pub fn view(&self) -> View {
        self.view.lock().unwrap().clone()
    }

    /// Block until the version differs from `seen` or `timeout` passes.
    pub fn wait_change(&self, seen: u64, timeout: Duration) -> View {
        let g = self.view.lock().unwrap();
        let (g, _) = self.cv.wait_timeout_while(g, timeout, |v| v.version == seen).unwrap();
        g.clone()
    }

    fn set_mode(&self, mode: Mode) {
        let mut v = self.view.lock().unwrap();
        if v.mode != mode {
            v.mode = mode;
            v.version += 1;
            self.cv.notify_all();
        }
    }

    /// Store the blocks; bump the version only if what consumers act on
    /// changed. The battery block carries sysfs voltage/current and changes
    /// on nearly every 5 s read; waking the charge policy on that would bring
    /// back the background polling D2 removed.
    fn set_blocks(&self, stream: &Stream, changed: &[String]) {
        let mut v = self.view.lock().unwrap();
        if !changed.iter().any(|c| WAKE_BLOCKS.contains(&c.as_str())) {
            v.blocks = stream.blocks.clone();
            return;
        }
        let before = fingerprint(&v.blocks);
        v.blocks = stream.blocks.clone();
        if fingerprint(&v.blocks) != before {
            v.version += 1;
            self.cv.notify_all();
        }
    }
}

/// The fields consumers act on: plug state, percent, charging, direct
/// supply, the SMS summary (max id, count), and those blocks' health.
fn fingerprint(blocks: &HashMap<String, Block>) -> [Value; 9] {
    let field = |b: &str, path: &[&str]| {
        let mut v = blocks.get(b).map(|b| &b.data);
        for k in path {
            v = v.and_then(|x| x.get(k));
        }
        v.cloned().unwrap_or(Value::Null)
    };
    let fresh = |b: &str| Value::Bool(blocks.get(b).is_some_and(Block::fresh));
    [
        field("battery", &["charger_connect"]),
        field("battery", &["percent"]),
        field("battery", &["charging"]),
        field("charger", &["direct_supply", "enabled"]),
        fresh("battery"),
        fresh("charger"),
        field("sms", &["max_id"]),
        field("sms", &["count"]),
        fresh("sms"),
    ]
}

static FEED: OnceLock<Arc<Feed>> = OnceLock::new();

/// The running feed, if [`start`] was called (never in sidecar mode).
pub fn global() -> Option<&'static Arc<Feed>> {
    FEED.get()
}

/// Wait on the feed if there is one, else just sleep. Returns the view (a
/// `Starting` view without a feed, so callers read ubus themselves).
pub fn wait(seen: u64, timeout: Duration) -> View {
    let timeout = timeout.min(MAX_WAIT);
    match global() {
        Some(f) => f.wait_change(seen, timeout),
        None => {
            std::thread::sleep(timeout);
            View { mode: Mode::Starting, version: seen, blocks: HashMap::new() }
        }
    }
}

/// Charger plugged in: from the feed when subscribed and fresh, otherwise a
/// direct read cached for [`MAX_WAIT`] (so a 15 s caller stays low-frequency).
pub fn charger_connected_cached(direct: impl FnOnce() -> Option<bool>) -> Option<bool> {
    static CACHE: Mutex<Option<(Instant, Option<bool>)>> = Mutex::new(None);
    if let Some(c) = global().and_then(|f| f.view().charger_connected()) {
        return Some(c);
    }
    let mut cache = CACHE.lock().unwrap();
    if let Some((t, v)) = *cache {
        if t.elapsed() < MAX_WAIT {
            return v;
        }
    }
    let v = direct();
    *cache = Some((Instant::now(), v));
    v
}

// ── /control client ─────────────────────────────────────────────────

/// A failed datad `/control` call.
#[derive(Debug, Clone, PartialEq)]
pub enum ControlError {
    /// 503 `busy`: the control queue is full (V2-25); retry later.
    Busy,
    Other(String),
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ControlError::Busy => write!(f, "datad control queue busy"),
            ControlError::Other(e) => write!(f, "{e}"),
        }
    }
}

/// datad's address: the same one the feed subscribes to.
pub fn datad_addr() -> SocketAddr {
    std::env::var("ZTE_AGENT_DATAD_V2")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| V2_ADDR.parse().unwrap())
}

/// One `POST /control {action, params}` to datad (local, no token). Returns
/// `result` of an `ok:true` reply. The connection is kept until the reply
/// (datad drops requests whose client closed early).
pub fn control(addr: SocketAddr, action: &str, params: &Value, timeout: Duration) -> Result<Value, ControlError> {
    let other = |e: String| ControlError::Other(format!("datad {action}: {e}"));
    let mut sock = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).map_err(|e| other(format!("connect: {e}")))?;
    let _ = sock.set_read_timeout(Some(timeout));
    let _ = sock.set_write_timeout(Some(timeout));
    let body = serde_json::json!({"action": action, "params": params}).to_string();
    let req = format!(
        "POST /control HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    sock.write_all(req.as_bytes()).map_err(|e| other(format!("write: {e}")))?;
    let mut raw = Vec::new();
    sock.read_to_end(&mut raw).map_err(|e| other(format!("read: {e}")))?;
    let pos = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(|| other("no HTTP header".into()))?;
    let head = String::from_utf8_lossy(&raw[..pos]).to_ascii_lowercase();
    let status: u16 = head.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let mut payload = raw[pos + 4..].to_vec();
    if head.lines().any(|l| l.starts_with("transfer-encoding:") && l.contains("chunked")) {
        payload = Chunked::default().feed(&payload).map_err(other)?;
    }
    let reply: Value = serde_json::from_slice(&payload).map_err(|e| other(format!("status {status}, bad JSON: {e}")))?;
    if status == 503 && reply["error"]["code"] == "busy" {
        return Err(ControlError::Busy);
    }
    if status != 200 || reply["ok"] != true {
        let msg = reply["error"]["message"].as_str().unwrap_or("failed");
        return Err(other(format!("status {status}: {msg}")));
    }
    Ok(reply.get("result").cloned().unwrap_or(Value::Null))
}

/// Delays between retries of a busy `/control` (V2-25): a bounded number of
/// tries, then give up and let the caller fall back.
pub const BUSY_RETRY: [Duration; 3] =
    [Duration::from_millis(200), Duration::from_millis(500), Duration::from_millis(1000)];

/// [`control`], retrying `503 busy` after each of `delays`.
pub fn control_retry(addr: SocketAddr, action: &str, params: &Value, timeout: Duration, delays: &[Duration]) -> Result<Value, ControlError> {
    let mut tries = delays.iter();
    loop {
        match control(addr, action, params, timeout) {
            Err(ControlError::Busy) => match tries.next() {
                Some(d) => std::thread::sleep(*d),
                None => return Err(ControlError::Busy),
            },
            r => return r,
        }
    }
}

/// Tunables, so tests can run the real loop fast.
#[derive(Debug, Clone)]
pub struct Timing {
    pub silence: Duration,
    pub stale_limit: Duration,
    pub read_timeout: Duration,
    pub backoff_max: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            silence: SILENCE,
            stale_limit: STALE_LIMIT,
            read_timeout: Duration::from_secs(1),
            backoff_max: Duration::from_secs(5),
        }
    }
}

/// Start the feed thread (non-sidecar only: `startup` deletes the marker, and
/// a sidecar must not touch the live instance's marker).
pub fn start() -> Arc<Feed> {
    let feed = Arc::clone(FEED.get_or_init(|| Arc::new(Feed::default())));
    let f = Arc::clone(&feed);
    std::thread::spawn(move || {
        let addr = datad_addr();
        run(&f, addr, DegradedMarker::new(MARKER_PATH), Timing::default(), &|| false);
    });
    feed
}

/// Why a connection ended.
#[derive(Debug, PartialEq)]
enum End {
    Closed,
    Silent,
    Resync(String),
    Io(String),
    Status(u16),
}

/// Quick reconnects (Closed / Resync) allowed in a row before backing off.
const QUICK_FREE: u32 = 3;
const QUICK_WAIT: Duration = Duration::from_millis(300);
const BACKOFF_MIN: Duration = Duration::from_millis(200);
/// A connection that got a snapshot and then lasted this long was healthy:
/// the next end starts the counts afresh.
const STABLE_AFTER: Duration = Duration::from_secs(10);

/// How long to wait before the next connection. Pure.
#[derive(Debug)]
struct Reconnect {
    /// Quick ends in a row without a stable connection between them.
    quick_streak: u32,
    /// Next wait after a non-quick end (Silent / Io / Status).
    backoff: Duration,
}

impl Reconnect {
    fn new() -> Self {
        Reconnect { quick_streak: 0, backoff: BACKOFF_MIN }
    }

    /// A connection ended. `stable` = it got a snapshot and then lasted
    /// [`STABLE_AFTER`]. Resync / Closed reconnect at once (V2-3, V2-8) for
    /// the first [`QUICK_FREE`] in a row, then back off exponentially up to
    /// `max`, so a datad that keeps dropping us is not hammered.
    fn next(&mut self, quick: bool, stable: bool, max: Duration) -> Duration {
        if stable {
            self.quick_streak = 0;
            self.backoff = BACKOFF_MIN;
        }
        if quick {
            self.quick_streak += 1;
            if self.quick_streak <= QUICK_FREE {
                return QUICK_WAIT;
            }
            let exp = (self.quick_streak - QUICK_FREE).min(16);
            return (QUICK_WAIT * 2u32.pow(exp)).min(max);
        }
        let wait = self.backoff;
        self.backoff = (self.backoff * 2).min(max);
        wait
    }
}

/// The feed loop. `stop` lets tests end it.
pub fn run(feed: &Feed, addr: SocketAddr, mut marker: DegradedMarker, t: Timing, stop: &dyn Fn() -> bool) {
    if let Err(e) = marker.startup() {
        eprintln!("[datad_feed] cannot remove old marker: {e}");
    }
    let t0 = Instant::now();
    let mono = || t0.elapsed();
    let mut health = Health::with_limits(mono(), t.silence, t.stale_limit);
    let mut stream = Stream::default();
    let mut reconnect = Reconnect::new();
    while !stop() {
        let mut snapshot_at = None;
        let end = session(feed, addr, &mut health, &mut stream, &mut marker, &t, &mono, stop, &mut snapshot_at);
        let quick = matches!(end, End::Resync(_) | End::Closed);
        if !matches!(end, End::Closed) {
            eprintln!("[datad_feed] /v2 connection ended: {end:?}");
        }
        // Back off while still judging the mode and refreshing the marker
        // every tick.
        let stable = snapshot_at.is_some_and(|at| mono().saturating_sub(at) >= STABLE_AFTER);
        let wait = reconnect.next(quick, stable, t.backoff_max);
        let until = Instant::now() + wait;
        loop {
            step(feed, &mut health, &mut marker, mono());
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() || stop() {
                break;
            }
            std::thread::sleep(left.min(t.read_timeout));
        }
    }
}

/// Judge the mode and keep the marker in step with it.
fn step(feed: &Feed, health: &mut Health, marker: &mut DegradedMarker, now: Duration) {
    match health.evaluate(now) {
        Some(Transition::Enter(reason)) => {
            eprintln!("[datad_feed] 退路: {reason}");
            if let Err(e) = marker.enter(&reason, now_epoch()) {
                eprintln!("[datad_feed] marker write failed: {e}");
            }
            feed.set_mode(Mode::Fallback);
        }
        Some(Transition::Recover) => {
            if marker.is_degraded() {
                eprintln!("[datad_feed] 订阅中 again (fallback since {:?})", marker.since());
            }
            if let Err(e) = marker.recover() {
                eprintln!("[datad_feed] marker remove failed: {e}");
            }
            feed.set_mode(Mode::Subscribed);
        }
        None => {
            if marker.is_degraded() {
                if let Err(e) = marker.tick(now_epoch()) {
                    eprintln!("[datad_feed] marker refresh failed: {e}");
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn session(
    feed: &Feed,
    addr: SocketAddr,
    health: &mut Health,
    stream: &mut Stream,
    marker: &mut DegradedMarker,
    t: &Timing,
    mono: &dyn Fn() -> Duration,
    stop: &dyn Fn() -> bool,
    snapshot_at: &mut Option<Duration>,
) -> End {
    let mut sock = match TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
        Ok(s) => s,
        Err(e) => return End::Io(format!("connect: {e}")),
    };
    let _ = sock.set_read_timeout(Some(t.read_timeout));
    let req = format!(
        "GET /v2/events HTTP/1.1\r\nHost: {addr}\r\nAccept: text/event-stream\r\nConnection: close\r\n\r\n"
    );
    if let Err(e) = sock.write_all(req.as_bytes()) {
        return End::Io(format!("write: {e}"));
    }
    stream.new_connection();
    let started = mono();
    let mut head: Vec<u8> = Vec::new();
    let mut body: Option<(bool, Chunked)> = None; // (chunked?, decoder)
    let mut sse = SseParser::default();
    let mut buf = [0u8; 4096];
    loop {
        if stop() {
            return End::Closed;
        }
        step(feed, health, marker, mono());
        // M counts from the last event, but a fresh connection gets its own M
        // to deliver the snapshot (otherwise a reconnect after silence would
        // be dropped before reading anything).
        if health.silent(mono()) && mono().saturating_sub(started) >= t.silence {
            return End::Silent;
        }
        let n = match sock.read(&mut buf) {
            Ok(0) => return End::Closed,
            Ok(n) => n,
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => continue,
            Err(e) => return End::Io(format!("read: {e}")),
        };
        let mut payload: Vec<u8> = Vec::new();
        match &mut body {
            None => {
                head.extend_from_slice(&buf[..n]);
                let Some(pos) = head.windows(4).position(|w| w == b"\r\n\r\n") else {
                    if head.len() > 16 * 1024 {
                        return End::Io("header too long".into());
                    }
                    continue;
                };
                let text = String::from_utf8_lossy(&head[..pos]).to_ascii_lowercase();
                let status: u16 = text.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                if status != 200 {
                    // 503 sse_client_limit included: back off and retry.
                    return End::Status(status);
                }
                let chunked = text.lines().any(|l| l.starts_with("transfer-encoding:") && l.contains("chunked"));
                let rest = head[pos + 4..].to_vec();
                let mut dec = Chunked::default();
                if chunked {
                    match dec.feed(&rest) {
                        Ok(p) => payload = p,
                        Err(e) => return End::Io(e),
                    }
                } else {
                    payload = rest;
                }
                body = Some((chunked, dec));
            }
            Some((chunked, dec)) => {
                if *chunked {
                    match dec.feed(&buf[..n]) {
                        Ok(p) => payload = p,
                        Err(e) => return End::Io(e),
                    }
                    if dec.done {
                        return End::Closed;
                    }
                } else {
                    payload.extend_from_slice(&buf[..n]);
                }
            }
        }
        let events = match sse.feed(&payload) {
            Ok(evs) => evs,
            Err(e) => return End::Resync(e),
        };
        for ev in events {
            match stream.apply(&ev) {
                Ok(applied) => {
                    if applied.snapshot {
                        *snapshot_at = Some(mono());
                    }
                    health.on_event(mono(), stream);
                    feed.set_blocks(stream, &applied.changed);
                }
                Err(e) => return End::Resync(e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("u60-datad-feed-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn read(p: &Path) -> String {
        fs::read_to_string(p).unwrap()
    }

    fn mtime(p: &Path) -> SystemTime {
        fs::metadata(p).unwrap().modified().unwrap()
    }

    #[test]
    fn enter_writes_start_and_reason() {
        let d = dir("enter");
        let p = d.join("sub/datad-degraded"); // parent created on demand
        let mut m = DegradedMarker::new(&p);
        m.enter("v2 unreachable", 1_758_600_000).unwrap();
        assert_eq!(read(&p), "1758600000\nv2 unreachable\n");
        assert!(m.is_degraded());
        assert_eq!(m.since(), Some(1_758_600_000));
        assert!(!p.with_extension("tmp").exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn enter_again_keeps_start() {
        let d = dir("reenter");
        let p = d.join("datad-degraded");
        let mut m = DegradedMarker::new(&p);
        m.enter("first", 1000).unwrap();
        m.enter("second", 1030).unwrap(); // not due: file untouched
        assert_eq!(read(&p), "1000\nfirst\n");
        m.enter("second", 1100).unwrap(); // due: rewritten, same start
        assert_eq!(read(&p), "1000\nsecond\n");
        assert_eq!(m.since(), Some(1000));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn tick_refreshes_every_60s_with_same_start() {
        let d = dir("tick");
        let p = d.join("datad-degraded");
        let mut m = DegradedMarker::new(&p);
        assert!(!m.tick(1000).unwrap(), "not degraded: nothing to do");
        assert!(!p.exists());
        m.enter("v2 down", 1000).unwrap();
        assert!(!m.tick(1059).unwrap());
        // Backdate the file so the rewrite is visible in the mtime.
        let old = UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        fs::File::options().write(true).open(&p).unwrap().set_modified(old).unwrap();
        assert!(m.tick(1060).unwrap());
        assert!(mtime(&p) > old);
        assert_eq!(read(&p), "1000\nv2 down\n");
        assert!(!m.tick(1100).unwrap());
        assert!(m.tick(1120).unwrap());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn tick_recreates_deleted_marker() {
        let d = dir("recreate");
        let p = d.join("datad-degraded");
        let mut m = DegradedMarker::new(&p);
        m.enter("v2 down", 1000).unwrap();
        fs::remove_file(&p).unwrap();
        assert!(m.tick(1060).unwrap());
        assert_eq!(read(&p), "1000\nv2 down\n");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn tick_after_clock_went_back_rewrites() {
        let d = dir("clockback");
        let p = d.join("datad-degraded");
        let mut m = DegradedMarker::new(&p);
        m.enter("v2 down", 5000).unwrap();
        assert!(m.tick(100).unwrap(), "clock jumped back: refresh, don't stall");
        assert!(!m.tick(120).unwrap());
        assert_eq!(read(&p), "5000\nv2 down\n");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn recover_deletes_and_tolerates_missing() {
        let d = dir("recover");
        let p = d.join("datad-degraded");
        let mut m = DegradedMarker::new(&p);
        m.enter("v2 down", 1000).unwrap();
        m.recover().unwrap();
        assert!(!p.exists());
        assert!(!m.is_degraded());
        m.recover().unwrap(); // already gone: fine
        assert!(!m.tick(2000).unwrap(), "recovered: no refresh");
        assert!(!p.exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn startup_drops_old_marker() {
        let d = dir("startup");
        let p = d.join("datad-degraded");
        fs::write(&p, "123\nleft by the previous agent\n").unwrap();
        let mut m = DegradedMarker::new(&p);
        m.startup().unwrap();
        assert!(!p.exists());
        assert!(!m.is_degraded());
        m.startup().unwrap(); // nothing there: fine
        // Still unhealthy after restart: a fresh episode with a new start.
        m.enter("v2 down", 9000).unwrap();
        assert_eq!(read(&p), "9000\nv2 down\n");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn reason_is_one_line_printable_ascii_capped() {
        assert_eq!(clean_reason("a\tb\nc\r"), "a b c");
        assert_eq!(clean_reason("连接 refused \u{7}x"), "refused x");
        assert_eq!(clean_reason("  \n "), "unknown");
        assert_eq!(clean_reason(&"x".repeat(300)).len(), REASON_MAX);
    }
}

#[cfg(test)]
mod feed_tests {
    use super::*;
    use serde_json::json;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn secs(s: f64) -> Duration {
        Duration::from_secs_f64(s)
    }

    fn ev(event: &str, data: Value) -> SseEvent {
        SseEvent { event: event.into(), data: data.to_string() }
    }

    fn block(rev: u64, stale: bool, data: Value) -> Value {
        json!({"revision": rev, "observed_at": 100, "stale": stale, "data": data})
    }

    fn healthy_snapshot(epoch: &str, seq: u64, connected: i64) -> SseEvent {
        ev("snapshot", json!({"epoch": epoch, "seq": seq, "blocks": {
            "battery": block(1, false, json!({"percent": 70, "charger_connect": connected})),
            "charger": block(1, false, json!({"direct_supply": {"supported": true, "enabled": false, "mode": "disable"}})),
            "live": block(1, false, json!({"system": {}})),
        }}))
    }

    fn heartbeat(epoch: &str, seq: u64) -> SseEvent {
        ev("heartbeat", json!({"epoch": epoch, "seq": seq, "blocks": {"battery": 200, "charger": 200}}))
    }

    fn subscribed_health() -> (Health, Stream) {
        let mut st = Stream::default();
        let mut h = Health::new(secs(0.0));
        st.apply(&healthy_snapshot("e1", 0, 0)).unwrap();
        h.on_event(secs(0.0), &st);
        assert_eq!(h.evaluate(secs(0.0)), Some(Transition::Recover));
        assert_eq!(h.mode, Mode::Subscribed);
        (h, st)
    }

    /// STATE_V2 V2-23, T7's named test: 20 s without any event → disconnect
    /// and fallback; an event in between restarts the clock.
    #[test]
    fn datad_feed_disconnects_after_20s_silence() {
        let (mut h, mut st) = subscribed_health();
        assert!(!h.silent(secs(19.9)));
        assert_eq!(h.evaluate(secs(19.9)), None);
        // A heartbeat at 10 s restarts the count.
        st.apply(&heartbeat("e1", 1)).unwrap();
        h.on_event(secs(10.0), &st);
        assert!(!h.silent(secs(29.9)));
        assert_eq!(h.evaluate(secs(29.9)), None);
        assert!(h.silent(secs(30.0)), "20 s after the last event: drop the connection");
        match h.evaluate(secs(30.0)) {
            Some(Transition::Enter(r)) => assert!(r.contains("no v2 event"), "{r}"),
            other => panic!("expected fallback, got {other:?}"),
        }
        assert_eq!(h.mode, Mode::Fallback);
        // Reconnect + healthy snapshot → back.
        st.new_connection();
        st.apply(&healthy_snapshot("e1", 5, 0)).unwrap();
        h.on_event(secs(31.0), &st);
        assert_eq!(h.evaluate(secs(31.0)), Some(Transition::Recover));
    }

    #[test]
    fn startup_gets_m_then_falls_back() {
        let mut h = Health::new(secs(0.0));
        assert_eq!(h.evaluate(secs(19.0)), None);
        assert_eq!(h.mode, Mode::Starting);
        assert!(matches!(h.evaluate(secs(20.0)), Some(Transition::Enter(_))));
    }

    #[test]
    fn key_block_stale_30s_falls_back_even_connected() {
        let (mut h, mut st) = subscribed_health();
        st.apply(&ev("block", json!({"epoch": "e1", "seq": 1, "name": "charger", "revision": 2,
            "observed_at": 100, "stale": true, "data": {"direct_supply": {}}}))).unwrap();
        h.on_event(secs(5.0), &st);
        for i in 1..=6 {
            st.apply(&heartbeat("e1", 1 + i)).unwrap();
            h.on_event(secs(5.0 + 4.0 * i as f64), &st);
        }
        assert_eq!(h.evaluate(secs(34.9)), None);
        match h.evaluate(secs(35.0)) {
            Some(Transition::Enter(r)) => assert!(r.contains("charger"), "{r}"),
            other => panic!("{other:?}"),
        }
    }

    /// datad restarted: its never-read blocks are stale/null. Connected and
    /// chatty, but not healthy: no recover, and the marker keeps its start.
    #[test]
    fn reconnect_with_stale_blocks_does_not_recover() {
        let d = std::env::temp_dir().join(format!("u60-feed-norecover-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        let p = d.join("datad-degraded");
        let mut m = DegradedMarker::new(&p);
        let feed = Feed::default();
        let (mut h, mut st) = subscribed_health();
        step(&feed, &mut h, &mut m, secs(20.0));
        assert!(m.is_degraded());
        let start = m.since();
        st.new_connection();
        st.apply(&ev("snapshot", json!({"epoch": "e2", "seq": 0, "blocks": {
            "battery": {"revision": 0, "observed_at": 0, "stale": true, "data": null},
            "charger": {"revision": 0, "observed_at": 0, "stale": true, "data": null}}}))).unwrap();
        for i in 0..40 {
            if i > 0 {
                st.apply(&heartbeat("e2", i)).unwrap();
            }
            let now = secs(21.0 + i as f64);
            h.on_event(now, &st);
            step(&feed, &mut h, &mut m, now);
            assert_eq!(h.mode, Mode::Fallback);
        }
        assert_eq!(m.since(), start);
        assert!(p.exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn stream_checks_epoch_seq_and_snapshot_cut() {
        let mut st = Stream::default();
        assert!(st.apply(&heartbeat("e1", 1)).is_err(), "must start with a snapshot");
        let a = st.apply(&healthy_snapshot("e1", 41, 1)).unwrap();
        assert!(a.snapshot);
        assert_eq!(a.changed, vec!["battery", "charger", "live"]);
        // next must be cut + 1
        assert!(st.apply(&heartbeat("e1", 42)).is_ok());
        assert_eq!(st.blocks["battery"].observed_at, 200, "observed_at comes from heartbeats");
        let a = st.apply(&ev("block", json!({"epoch": "e1", "seq": 43, "name": "battery", "revision": 2,
            "observed_at": 300, "stale": false, "data": {"charger_connect": 0}}))).unwrap();
        assert_eq!(a.changed, vec!["battery"]);
        assert!(st.apply(&heartbeat("e1", 45)).unwrap_err().contains("gap"));
        st.new_connection();
        st.apply(&healthy_snapshot("e1", 50, 1)).unwrap();
        assert!(st.apply(&heartbeat("e2", 51)).unwrap_err().contains("epoch"));
    }

    #[test]
    fn sse_parser_handles_arbitrary_splits() {
        let text = "event: snapshot\r\ndata: {\"a\":\"\u{4e2d}\"}\r\n\r\n: keepalive\n\nevent: heartbeat\ndata: {}\n\n";
        let bytes = text.as_bytes();
        for cut in 0..bytes.len() {
            let mut p = SseParser::default();
            let mut out = p.feed(&bytes[..cut]).unwrap();
            out.extend(p.feed(&bytes[cut..]).unwrap());
            assert_eq!(out.len(), 2, "cut at {cut}");
            assert_eq!(out[0], SseEvent { event: "snapshot".into(), data: "{\"a\":\"\u{4e2d}\"}".into() });
            assert_eq!(out[1].event, "heartbeat");
        }
    }

    #[test]
    fn reconnect_backs_off_after_quick_streak() {
        let max = Duration::from_secs(5);
        let ms = Duration::from_millis;
        let mut r = Reconnect::new();
        let waits: Vec<_> = (0..8).map(|_| r.next(true, false, max)).collect();
        assert_eq!(waits, vec![ms(300), ms(300), ms(300), ms(600), ms(1200), ms(2400), ms(4800), ms(5000)]);
        assert_eq!(r.next(true, false, max), max, "capped");
        // A stable connection (snapshot + 10 s) resets it.
        assert_eq!(r.next(true, true, max), ms(300));
        assert_eq!(r.next(true, false, max), ms(300));
        // Non-quick ends keep the old doubling, reset by a stable one too.
        let mut r = Reconnect::new();
        assert_eq!(r.next(false, false, max), ms(200));
        assert_eq!(r.next(false, false, max), ms(400));
        assert_eq!(r.next(false, true, max), ms(200));
        // Many quick ends never overflow.
        let mut r = Reconnect::new();
        for _ in 0..100 {
            assert!(r.next(true, false, max) <= max);
        }
    }

    /// seq at u64::MAX: no panic / wrap, the connection resyncs.
    #[test]
    fn seq_overflow_is_resync() {
        let mut st = Stream::default();
        assert!(st.apply(&healthy_snapshot("e1", u64::MAX, 1)).unwrap_err().contains("overflow"));
        st.apply(&healthy_snapshot("e1", u64::MAX - 1, 1)).unwrap();
        assert!(st.apply(&heartbeat("e1", u64::MAX)).unwrap_err().contains("overflow"));
    }

    #[test]
    fn sse_parser_caps_line_and_event() {
        let mut p = SseParser::default();
        // Just under the cap, fed in pieces: fine.
        let big = format!("data: {}", "x".repeat(SSE_MAX_LINE - 7));
        assert!(p.feed(&big.as_bytes()[..1000]).unwrap().is_empty());
        assert!(p.feed(&big.as_bytes()[1000..]).unwrap().is_empty());
        let out = p.feed(b"\n\n").unwrap();
        assert_eq!(out[0].data.len(), SSE_MAX_LINE - 7);
        // A line with no end: error once over the cap, then usable again.
        let junk = vec![b'y'; 64 * 1024];
        let mut err = None;
        for _ in 0..17 {
            if let Err(e) = p.feed(&junk) {
                err = Some(e);
                break;
            }
        }
        assert!(err.unwrap().contains("line"));
        assert_eq!(p.feed(b"event: heartbeat\ndata: {}\n\n").unwrap().len(), 1, "reset after overflow");
        // Many short data lines adding up past the cap: also an error.
        let line = format!("data: {}\n", "z".repeat(1000));
        let mut err = None;
        for _ in 0..1100 {
            if let Err(e) = p.feed(line.as_bytes()) {
                err = Some(e);
                break;
            }
        }
        assert!(err.unwrap().contains("event"));
    }

    /// An endless line from the server ends the connection as Resync.
    #[test]
    fn session_resyncs_on_oversized_line() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut req = [0u8; 1024];
            let _ = s.read(&mut req);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\ndata: ");
            let junk = vec![b'q'; 64 * 1024];
            for _ in 0..20 {
                if s.write_all(&junk).is_err() {
                    break;
                }
            }
        });
        let feed = Feed::default();
        let d = std::env::temp_dir().join(format!("u60-feed-cap-{}", std::process::id()));
        let _ = fs::create_dir_all(&d);
        let mut marker = DegradedMarker::new(d.join("m"));
        let mut health = Health::with_limits(Duration::ZERO, Duration::from_secs(30), Duration::from_secs(30));
        let mut stream = Stream::default();
        let t = Timing::default();
        let t0 = Instant::now();
        let mono = || t0.elapsed();
        let end = session(&feed, addr, &mut health, &mut stream, &mut marker, &t, &mono, &|| false, &mut None);
        assert!(matches!(&end, End::Resync(e) if e.contains("line")), "{end:?}");
        server.join().unwrap();
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn chunked_decoder_byte_by_byte() {
        let wire = b"5\r\nhello\r\n7;ext=1\r\n, world\r\n0\r\n\r\n";
        let mut d = Chunked::default();
        let mut out = Vec::new();
        for b in wire {
            out.extend(d.feed(std::slice::from_ref(b)).unwrap());
        }
        assert_eq!(out, b"hello, world");
        assert!(d.done);
        assert!(Chunked::default().feed(b"zz\r\n").is_err());
    }

    #[test]
    fn view_unknown_is_none_never_unplugged() {
        let mut st = Stream::default();
        st.apply(&healthy_snapshot("e1", 0, 1)).unwrap();
        let mut v = View { mode: Mode::Subscribed, version: 1, blocks: st.blocks.clone() };
        assert_eq!(v.charger_connected(), Some(true));
        assert_eq!(v.charging_stopped(), Some(false));
        v.blocks.get_mut("charger").unwrap().stale = true;
        assert_eq!(v.charger_connected(), None);
        assert_eq!(v.charging_stopped(), None);
        v.blocks.get_mut("charger").unwrap().stale = false;
        v.blocks.get_mut("charger").unwrap().data = json!({});
        assert_eq!(v.charging_stopped(), None, "{{}} charger block: unknown");
        v.mode = Mode::Fallback;
        assert_eq!(v.charger_connected(), None);
    }

    #[test]
    fn wait_change_wakes_on_mode_change() {
        let feed = Arc::new(Feed::default());
        let f = Arc::clone(&feed);
        let h = std::thread::spawn(move || f.wait_change(0, Duration::from_secs(10)));
        std::thread::sleep(Duration::from_millis(50));
        feed.set_mode(Mode::Fallback);
        let v = h.join().unwrap();
        assert_eq!(v.mode, Mode::Fallback);
        assert_eq!(v.version, 1);
        // No change: returns after the timeout with the same version.
        let t = Instant::now();
        assert_eq!(feed.wait_change(1, Duration::from_millis(50)).version, 1);
        assert!(t.elapsed() >= Duration::from_millis(50));
    }

    #[test]
    fn sms_block_wakes_on_max_id_or_count_only() {
        let feed = Feed::default();
        let mut st = Stream::default();
        let a = st.apply(&healthy_snapshot("e1", 0, 1)).unwrap();
        feed.set_blocks(&st, &a.changed);
        // Old datad: no sms block → no summary (read ubus directly).
        feed.set_mode(Mode::Subscribed);
        assert_eq!(feed.view().sms_summary(), None);
        let sms = |seq: u64, rev: u64, max_id: u64, count: u64, unread: u64, stale: bool| {
            ev("block", json!({"epoch": "e1", "seq": seq, "name": "sms", "revision": rev,
                "observed_at": 100, "stale": stale,
                "data": {"unread": unread, "max_id": max_id, "count": count}}))
        };
        let v0 = feed.view().version;
        let a = st.apply(&sms(1, 1, 40, 12, 0, false)).unwrap();
        feed.set_blocks(&st, &a.changed);
        assert_eq!(feed.view().version, v0 + 1, "block appeared: wake");
        assert_eq!(feed.view().sms_summary(), Some((40, 12)));
        let a = st.apply(&sms(2, 2, 40, 12, 1, false)).unwrap();
        feed.set_blocks(&st, &a.changed);
        assert_eq!(feed.view().version, v0 + 1, "only unread moved: no wake");
        let a = st.apply(&sms(3, 3, 41, 13, 1, false)).unwrap();
        feed.set_blocks(&st, &a.changed);
        assert_eq!(feed.view().version, v0 + 2, "new SMS: wake");
        let a = st.apply(&sms(4, 4, 41, 13, 1, true)).unwrap();
        feed.set_blocks(&st, &a.changed);
        assert_eq!(feed.view().version, v0 + 3, "went stale: wake (source switch)");
        assert_eq!(feed.view().sms_summary(), None);
        feed.set_mode(Mode::Fallback);
        let a = st.apply(&sms(5, 5, 41, 13, 1, false)).unwrap();
        feed.set_blocks(&st, &a.changed);
        assert_eq!(feed.view().sms_summary(), None, "fallback: never from datad");
    }

    #[test]
    fn version_ignores_voltage_current_noise() {
        let feed = Feed::default();
        let mut st = Stream::default();
        let a = st.apply(&healthy_snapshot("e1", 0, 1)).unwrap();
        feed.set_blocks(&st, &a.changed);
        let v0 = feed.view().version;
        let bat = |seq: u64, rev: u64, percent: i64, ua: i64| {
            ev("block", json!({"epoch": "e1", "seq": seq, "name": "battery", "revision": rev,
                "observed_at": 100, "stale": false,
                "data": {"percent": percent, "charger_connect": 1, "bat_ua": ua}}))
        };
        let a = st.apply(&bat(1, 2, 70, -120)).unwrap();
        assert_eq!(a.changed, vec!["battery"]);
        feed.set_blocks(&st, &a.changed);
        let a = st.apply(&bat(2, 3, 70, -135)).unwrap();
        feed.set_blocks(&st, &a.changed);
        assert_eq!(feed.view().version, v0, "only bat_ua moved: no wake");
        let a = st.apply(&bat(3, 4, 71, -135)).unwrap();
        feed.set_blocks(&st, &a.changed);
        assert_eq!(feed.view().version, v0 + 1, "percent moved: wake");
    }

    fn chunk(s: &str) -> Vec<u8> {
        format!("{:x}\r\n{s}\r\n", s.len()).into_bytes()
    }

    fn sse_text(e: &SseEvent) -> String {
        format!("event: {}\ndata: {}\n\n", e.event, e.data)
    }

    /// The real loop against a local fake datad (chunked, like hyper):
    /// subscribed → server goes silent → fallback + marker → reconnect → back.
    #[test]
    fn run_loop_against_fake_server() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let d = std::env::temp_dir().join(format!("u60-feed-run-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        let marker_path = d.join("datad-degraded");
        fs::create_dir_all(&d).unwrap();
        fs::write(&marker_path, "1\nold\n").unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let server_stop = Arc::clone(&stop);
        let server = std::thread::spawn(move || {
            let mut conns = 0;
            for s in listener.incoming() {
                if server_stop.load(Ordering::SeqCst) {
                    break;
                }
                let mut s = s.unwrap();
                conns += 1;
                let mut req = [0u8; 1024];
                let _ = s.read(&mut req);
                if conns == 1 {
                    // First: 503 like a full SSE slot table.
                    let _ = s.write_all(b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\n\r\n");
                    continue;
                }
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n");
                let snap = sse_text(&healthy_snapshot("e1", 0, 1));
                // Split the snapshot across two chunks, mid-line.
                let (a, b) = snap.split_at(snap.len() / 2);
                let _ = s.write_all(&chunk(a));
                let _ = s.write_all(&chunk(b));
                if conns == 2 {
                    // then silence, holding the socket open
                    std::thread::sleep(Duration::from_millis(1500));
                    continue;
                }
                for i in 1..200 {
                    if s.write_all(&chunk(&sse_text(&heartbeat("e1", i)))).is_err() || server_stop.load(Ordering::SeqCst) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        });

        let feed = Arc::new(Feed::default());
        let f = Arc::clone(&feed);
        let mp = marker_path.clone();
        let st = Arc::clone(&stop);
        let runner = std::thread::spawn(move || {
            let t = Timing {
                silence: Duration::from_millis(400),
                stale_limit: Duration::from_millis(600),
                read_timeout: Duration::from_millis(20),
                backoff_max: Duration::from_millis(50),
            };
            run(&f, addr, DegradedMarker::new(mp), t, &|| st.load(Ordering::SeqCst));
        });

        let wait_mode = |want: Mode| {
            let t = Instant::now();
            let mut seen = 0;
            loop {
                let v = feed.wait_change(seen, Duration::from_millis(50));
                seen = v.version;
                if v.mode == want {
                    return v;
                }
                assert!(t.elapsed() < Duration::from_secs(5), "timed out waiting for {want:?}");
            }
        };
        let v = wait_mode(Mode::Subscribed);
        assert!(!marker_path.exists(), "startup removed the old marker");
        assert_eq!(v.charger_connected(), Some(true));
        wait_mode(Mode::Fallback);
        assert!(marker_path.exists(), "fallback writes the marker");
        wait_mode(Mode::Subscribed);
        assert!(!marker_path.exists(), "recovery removes it");

        stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(addr); // unblock accept
        runner.join().unwrap();
        server.join().unwrap();
        let _ = fs::remove_dir_all(&d);
    }
}
