//! LAN client identification, the live path (docs/designs/lan-client-id.md §4, §4a; T3 + T4):
//! the `lanid` thread ([`crate::lanid_capture`]) queues BPF-filtered frames from br-lan, the
//! lanid-parse wasm component parses them, and what it says about each sender is merged per MAC
//! into the `clients` block (name, vendor, type, per-field sources) next to the user's aliases.
//!
//! The core's own code never parses a frame: it reads the source MAC at bytes 6..12 (after a
//! length check) and hands whole frames to the component. Everything else here is on the
//! component's (validated) output.
//!
//! - **No built-in fallback** (§4): unlike the policies in [`crate::plugin`], there is no native
//!   parser to fall back to. No manifest entry, a wrong sha256, a load error, or a BPF that won't
//!   attach: the sources are off, clients show DHCP names (and aliases) as before, the status
//!   says why. A trap does not turn the component off (one bad packet must not end the feature):
//!   the batch is re-run one frame per instance, only the frames that trap are dropped and their
//!   sender is blocked for [`BLOCK_FOR`].
//! - Rate: a token bucket per source MAC, [`RATE_PER_MIN`] frames a minute, at most [`MAX_MACS`]
//!   MACs tracked; frames over it are dropped before parsing.
//! - Output limits: at most [`MAX_FACTS`] facts per frame, each value at most [`MAX_STR`] bytes
//!   with no control characters; a frame whose answer breaks them is dropped whole.
//! - Storage: [`DIR`]/clients.json (version 1, tmp + rename; missing, bad or another version =
//!   empty), at most [`MAX_RECORDS`] MACs, dropped after 30 days unseen; written only when a
//!   name/vendor/type fact changed, at most every [`FLUSH_EVERY`], and on exit. `last` is kept
//!   in memory and written rounded down to the hour. Aliases are [`DIR`]/aliases.json, written
//!   at once with a readback, no cap, never evicted. A side-by-side core (`luyaz-core.test`)
//!   neither captures nor writes.
//! - The results are for display only: a MAC can be forged, so nothing here feeds a policy, an
//!   auth decision or an automatic action. The two follow-ups in [`watch`] (a new MAC taking an
//!   offline device's alias, an alert when a stranger joins) are display and a notice too.

pub mod classify;
mod watch;
pub mod selftest;

use crate::contract as c;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const NAME: &str = "lanid-parse";
/// The WIT package this core was built against (`plugins.json` `abi`).
pub const ABI: &str = "luyaz:lanid@0.1.0";
pub const DIR: &str = "/data/luyaz/lanid";
/// Frames handed to one instance.
pub const QUEUE: usize = 64;
pub const RATE_PER_MIN: f64 = 30.0;
pub const MAX_MACS: usize = 256;
pub const BLOCK_FOR: Duration = Duration::from_secs(3600);
pub const MAX_STR: usize = 64;
pub const MAX_FACTS: usize = 8;
pub const MAX_RECORDS: usize = 256;
/// mDNS service types kept per MAC.
pub const MAX_SERVICES: usize = 8;
pub const EVICT_AFTER: u64 = 30 * 86400;
pub const FLUSH_EVERY: Duration = Duration::from_secs(3600);
/// The `clients` block's status is refreshed at most this often (an event each time).
pub const PUBLISH_EVERY: Duration = Duration::from_secs(600);
pub const VERSION: u64 = 1;

mod wit {
    wasmtime::component::bindgen!({ path: "../luyaz-policy/wit/lanid-parse.wit", world: "lanid-parse" });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    DhcpHostname,
    DhcpVendorClass,
    DhcpParams,
    MdnsInstance,
    MdnsModel,
    MdnsService,
    SsdpServer,
    NetbiosName,
}

impl Kind {
    pub fn source(self) -> &'static str {
        match self {
            Kind::DhcpHostname | Kind::DhcpVendorClass | Kind::DhcpParams => "dhcp",
            Kind::MdnsInstance | Kind::MdnsModel | Kind::MdnsService => "mdns",
            Kind::SsdpServer => "ssdp",
            Kind::NetbiosName => "netbios",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fact {
    pub kind: Kind,
    pub value: String,
}

/// What turns frames into facts: the component, or a test double.
pub trait Parse {
    /// One answer per frame; `Err` for a trap, fuel or memory running out, or a broken component.
    fn parse(&self, frames: &[Vec<u8>]) -> Result<Vec<Vec<Fact>>, String>;
}

/// The loaded component, instantiated fresh per call (fuel and memory caps of
/// [`crate::plugin::store`]).
pub struct Component {
    engine: wasmtime::Engine,
    pre: wit::LanidParsePre<crate::plugin::Limits>,
}

impl Component {
    pub fn new(engine: &wasmtime::Engine, bytes: &[u8]) -> Result<Self, String> {
        // SAFETY: the bytes matched the sha256 in the manifest, which plugc wrote for a .cwasm it
        // compiled with this Config; wasmtime also refuses another version's or Config's output.
        let comp = unsafe { wasmtime::component::Component::deserialize(engine, bytes) }
            .map_err(|e| format!("deserialize: {}", crate::plugin::short(&e)))?;
        let linker = wasmtime::component::Linker::new(engine);
        let pre = linker
            .instantiate_pre(&comp)
            .and_then(wit::LanidParsePre::new)
            .map_err(|e| format!("link: {}", crate::plugin::short(&e)))?;
        Ok(Self {
            engine: engine.clone(),
            pre,
        })
    }

    /// The component from `dir`'s manifest, or why not ("no component" when it has no entry).
    pub fn load(dir: &Path) -> Result<Self, String> {
        match crate::plugin::load_checked(dir, NAME, ABI) {
            Err(e) if e.is_empty() => Err("no component".into()),
            Err(e) => Err(e),
            Ok((bytes, _sha)) => crate::plugin::engine().and_then(|e| Self::new(&e, &bytes)),
        }
    }
}

impl Parse for Component {
    fn parse(&self, frames: &[Vec<u8>]) -> Result<Vec<Vec<Fact>>, String> {
        let run = || -> wasmtime::Result<Vec<Vec<wit::Fact>>> {
            let mut store = crate::plugin::store(&self.engine)?;
            let inst = self.pre.instantiate(&mut store)?;
            inst.call_parse(&mut store, frames)
        };
        let out = run().map_err(|e| crate::plugin::short(&e))?;
        Ok(out
            .into_iter()
            .map(|fs| {
                fs.into_iter()
                    .map(|f| Fact {
                        kind: match f.kind {
                            wit::FactKind::DhcpHostname => Kind::DhcpHostname,
                            wit::FactKind::DhcpVendorClass => Kind::DhcpVendorClass,
                            wit::FactKind::DhcpParams => Kind::DhcpParams,
                            wit::FactKind::MdnsInstance => Kind::MdnsInstance,
                            wit::FactKind::MdnsModel => Kind::MdnsModel,
                            wit::FactKind::MdnsService => Kind::MdnsService,
                            wit::FactKind::SsdpServer => Kind::SsdpServer,
                            wit::FactKind::NetbiosName => Kind::NetbiosName,
                        },
                        value: f.value,
                    })
                    .collect()
            })
            .collect())
    }
}

// ── MACs ─────────────────────────────────────────────────────────────────────

pub type Mac = [u8; 6];

/// The frame's source MAC: the one thing the core reads in a frame (fixed offset).
pub fn src_mac(frame: &[u8]) -> Option<Mac> {
    frame.get(6..12)?.try_into().ok()
}

pub fn mac_str(m: &Mac) -> String {
    m.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// `aa:bb:cc:dd:ee:ff`, `AA-BB-…` or 12 hex digits.
pub fn parse_mac(s: &str) -> Option<Mac> {
    let hex: String = s.chars().filter(|c| !matches!(c, ':' | '-')).collect();
    if hex.len() != 12 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut m = [0u8; 6];
    for (i, b) in m.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(m)
}

/// Locally administered (random per network on phones): no OUI.
pub fn is_private(m: &Mac) -> bool {
    m[0] & 0x02 != 0
}

fn valid_value(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_STR && !s.chars().any(char::is_control)
}

// ── the thread's own state: rate limit and blocks ───────────────────────────

struct Bucket {
    tokens: f64,
    at: Instant,
}

/// Counter changes from one batch.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Counters {
    pub batches: u64,
    pub frames: u64,
    pub facts: u64,
    pub traps: u64,
    /// frames whose own instance trapped (their MAC got blocked)
    pub bad_frames: u64,
    pub rate_dropped: u64,
    pub blocked_dropped: u64,
    /// too short, a multicast source MAC, or an answer over the limits
    pub invalid: u64,
}

impl Counters {
    fn add(&mut self, o: &Counters) {
        self.batches += o.batches;
        self.frames += o.frames;
        self.facts += o.facts;
        self.traps += o.traps;
        self.bad_frames += o.bad_frames;
        self.rate_dropped += o.rate_dropped;
        self.blocked_dropped += o.blocked_dropped;
        self.invalid += o.invalid;
    }
    fn dropped(&self) -> u64 {
        self.bad_frames + self.rate_dropped + self.blocked_dropped + self.invalid
    }
}

/// The `lanid` thread's limiter (never shared).
#[derive(Default)]
pub struct Gate {
    rate: HashMap<Mac, Bucket>,
    blocked: HashMap<Mac, Instant>,
}

impl Gate {
    fn is_blocked(&mut self, m: &Mac, now: Instant) -> bool {
        match self.blocked.get(m) {
            Some(until) if *until > now => true,
            Some(_) => {
                self.blocked.remove(m);
                false
            }
            None => false,
        }
    }

    fn block(&mut self, m: Mac, now: Instant) {
        if self.blocked.len() >= MAX_MACS {
            self.blocked.retain(|_, u| *u > now);
            if self.blocked.len() >= MAX_MACS
                && let Some(k) = self
                    .blocked
                    .iter()
                    .min_by_key(|(_, u)| **u)
                    .map(|(k, _)| *k)
            {
                self.blocked.remove(&k);
            }
        }
        self.blocked.insert(m, now + BLOCK_FOR);
    }

    fn take(&mut self, m: Mac, now: Instant) -> bool {
        if !self.rate.contains_key(&m) && self.rate.len() >= MAX_MACS {
            // full: forget buckets that have refilled, else the least recently used
            self.rate
                .retain(|_, b| now.duration_since(b.at) < Duration::from_secs(60));
            if self.rate.len() >= MAX_MACS
                && let Some(k) = self.rate.iter().min_by_key(|(_, b)| b.at).map(|(k, _)| *k)
            {
                self.rate.remove(&k);
            }
        }
        let b = self.rate.entry(m).or_insert(Bucket {
            tokens: RATE_PER_MIN,
            at: now,
        });
        let dt = now.duration_since(b.at).as_secs_f64();
        b.tokens = (b.tokens + dt * RATE_PER_MIN / 60.0).min(RATE_PER_MIN);
        b.at = now;
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    pub fn blocked_now(&self, now: Instant) -> u64 {
        self.blocked.values().filter(|u| **u > now).count() as u64
    }

    /// One batch: gate, parse (re-run per frame after a trap), validate. Returns the facts per
    /// sender and the counter changes. No lock held here: the component runs outside it.
    pub fn run(
        &mut self,
        p: &dyn Parse,
        frames: Vec<Vec<u8>>,
        now: Instant,
    ) -> (Vec<(Mac, Vec<Fact>)>, Counters) {
        let mut n = Counters::default();
        let mut keep = Vec::new();
        let mut macs = Vec::new();
        for f in frames.into_iter().take(QUEUE) {
            n.frames += 1;
            let Some(m) = src_mac(&f) else {
                n.invalid += 1;
                continue;
            };
            if m[0] & 0x01 != 0 {
                n.invalid += 1;
                continue;
            }
            if self.is_blocked(&m, now) {
                n.blocked_dropped += 1;
                continue;
            }
            if !self.take(m, now) {
                n.rate_dropped += 1;
                continue;
            }
            keep.push(f);
            macs.push(m);
        }
        if keep.is_empty() {
            return (Vec::new(), n);
        }
        n.batches += 1;
        // None: the answer was the wrong shape; Some(None): this frame trapped (counted here)
        let answers: Vec<Option<Option<Vec<Fact>>>> = match p.parse(&keep) {
            Ok(out) if out.len() == keep.len() => out.into_iter().map(|v| Some(Some(v))).collect(),
            Ok(_) => keep.iter().map(|_| None).collect(),
            Err(_) => {
                n.traps += 1;
                keep.iter()
                    .zip(&macs)
                    .map(|(f, m)| match p.parse(std::slice::from_ref(f)) {
                        Ok(mut v) if v.len() == 1 => v.pop().map(Some),
                        Ok(_) => None,
                        Err(_) => {
                            n.traps += 1;
                            n.bad_frames += 1;
                            self.block(*m, now);
                            Some(None)
                        }
                    })
                    .collect()
            }
        };
        let mut out = Vec::new();
        for (m, a) in macs.into_iter().zip(answers) {
            match a {
                Some(None) => {}
                Some(Some(v)) if v.is_empty() => {}
                Some(Some(v))
                    if v.len() <= MAX_FACTS && v.iter().all(|f| valid_value(&f.value)) =>
                {
                    n.facts += v.len() as u64;
                    out.push((m, v));
                }
                _ => n.invalid += 1,
            }
        }
        (out, n)
    }
}

// ── what was learned, shared with the projection and the API ────────────────

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Record {
    /// device seconds
    pub first: u64,
    pub last: u64,
    #[serde(default)]
    pub facts: BTreeMap<Kind, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<String>,
    /// the MAC whose alias this one took over ([`watch`]: the same device on a new address)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub from: String,
}

#[derive(Serialize, Deserialize)]
struct ClientsFile {
    version: u64,
    records: BTreeMap<String, Record>,
    /// every MAC seen in the client list ([`watch`]), with when; absent until the first round
    #[serde(default, skip_serializing_if = "Option::is_none")]
    known: Option<BTreeMap<String, u64>>,
}

#[derive(Serialize, Deserialize)]
struct AliasesFile {
    version: u64,
    aliases: BTreeMap<String, String>,
}

/// One candidate value of a client field.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Source {
    /// name, vendor or kind
    pub field: String,
    /// alias, mdns, dhcp, netbios, ssdp, oui
    pub source: String,
    pub value: String,
}

pub struct Shared {
    dir: PathBuf,
    /// the main core (not a side-by-side build): it may write
    persist: bool,
    /// [`start`] ran (serve mode): the status goes into the `clients` block
    started: bool,
    pub records: BTreeMap<String, Record>,
    pub aliases: BTreeMap<String, String>,
    oui: Option<classify::Oui>,
    loaded: bool,
    running: bool,
    why: String,
    pub counters: Counters,
    pub per_source: BTreeMap<&'static str, u64>,
    blocked: u64,
    dirty: bool,
    last_write: Instant,
    pub writes: u64,
    pub write_errors: u64,
    published: Option<(Instant, c::LanidStatus)>,
    known: Option<BTreeMap<String, u64>>,
    /// MACs in the client list not known yet, since when ([`watch::SETTLE`])
    pending: BTreeMap<String, Instant>,
}

static SHARED: OnceLock<Mutex<Shared>> = OnceLock::new();

fn default_dir() -> PathBuf {
    #[cfg(test)]
    {
        std::env::temp_dir().join(format!("luyaz-lanid-test-{}", std::process::id()))
    }
    #[cfg(not(test))]
    {
        PathBuf::from(crate::state::host_path(DIR))
    }
}

/// The shared state (loaded from [`DIR`] on first use; read-only until [`start`] says this is
/// the main core).
pub fn shared() -> &'static Mutex<Shared> {
    SHARED.get_or_init(|| Mutex::new(Shared::open(&default_dir(), false)))
}

fn lock() -> std::sync::MutexGuard<'static, Shared> {
    shared().lock().unwrap_or_else(|e| e.into_inner())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// tmp + rename, mode 0600, the directory 0700.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    if let Some(d) = path.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(d)?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)
}

impl Shared {
    pub fn open(dir: &Path, persist: bool) -> Shared {
        let (records, known) = read_json::<ClientsFile>(&dir.join("clients.json"))
            .filter(|f| f.version == VERSION)
            .map(|f| (f.records, f.known))
            .unwrap_or_default();
        let aliases = read_json::<AliasesFile>(&dir.join("aliases.json"))
            .filter(|f| f.version == VERSION)
            .map(|f| {
                f.aliases
                    .into_iter()
                    .filter_map(|(m, a)| Some((mac_str(&parse_mac(&m)?), a)))
                    .filter(|(_, a)| valid_value(a))
                    .collect()
            })
            .unwrap_or_default();
        let mut s = Shared {
            dir: dir.to_owned(),
            persist,
            started: false,
            records: records
                .into_iter()
                .filter_map(|(m, r)| Some((mac_str(&parse_mac(&m)?), r)))
                .collect(),
            aliases,
            oui: classify::Oui::load(&dir.join("oui.tsv")),
            loaded: false,
            running: false,
            why: "not started".into(),
            counters: Counters::default(),
            per_source: BTreeMap::new(),
            blocked: 0,
            dirty: false,
            last_write: Instant::now(),
            writes: 0,
            write_errors: 0,
            published: None,
            known,
            pending: BTreeMap::new(),
        };
        s.evict(crate::qmi_nas::now());
        s
    }

    /// Over [`MAX_RECORDS`]: the least recently seen go; unseen for [`EVICT_AFTER`]: gone.
    fn evict(&mut self, wall: u64) {
        self.records
            .retain(|_, r| r.last.saturating_add(EVICT_AFTER) > wall || r.last > wall);
        while self.records.len() > MAX_RECORDS {
            let Some(k) = self
                .records
                .iter()
                .min_by_key(|(_, r)| r.last)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            self.records.remove(&k);
        }
    }

    /// Merges one sender's facts; whether a name/vendor/type fact changed.
    pub fn learn(&mut self, m: &Mac, facts: &[Fact], wall: u64) -> bool {
        let key = mac_str(m);
        if !self.records.contains_key(&key) && self.records.len() >= MAX_RECORDS {
            self.evict(wall);
            if self.records.len() >= MAX_RECORDS
                && let Some(k) = self
                    .records
                    .iter()
                    .min_by_key(|(_, r)| r.last)
                    .map(|(k, _)| k.clone())
            {
                self.records.remove(&k);
            }
        }
        let r = self.records.entry(key).or_insert(Record {
            first: wall,
            ..Default::default()
        });
        let mut changed = r.facts.is_empty() && r.services.is_empty();
        r.last = wall;
        for f in facts {
            *self.per_source.entry(f.kind.source()).or_default() += 1;
            if f.kind == Kind::MdnsService {
                if !r.services.contains(&f.value) && r.services.len() < MAX_SERVICES {
                    r.services.push(f.value.clone());
                    changed = true;
                }
            } else if r.facts.get(&f.kind) != Some(&f.value) {
                // a name, once readable, sticks: a Mac announces several services under
                // different names (a UUID, a NetBIOS name) and the last one heard must not
                // replace the one already shown
                let sticky = matches!(
                    f.kind,
                    Kind::MdnsInstance | Kind::DhcpHostname | Kind::NetbiosName | Kind::MdnsModel
                ) && r.facts.get(&f.kind).is_some_and(|v| classify::readable_name(v));
                if sticky {
                    continue;
                }
                r.facts.insert(f.kind, f.value.clone());
                changed = true;
            }
        }
        self.dirty |= changed;
        changed
    }

    fn clients_json(&self) -> Vec<u8> {
        let records = self
            .records
            .iter()
            .map(|(k, r)| {
                let mut r = r.clone();
                r.last -= r.last % 3600;
                (k.clone(), r)
            })
            .collect();
        serde_json::to_vec(&ClientsFile {
            version: VERSION,
            records,
            known: self.known.clone(),
        })
        .unwrap_or_default()
    }

    /// Writes clients.json when something changed and (unless `now_too`) the last write is
    /// [`FLUSH_EVERY`] ago.
    pub fn maybe_flush(&mut self, now: Instant, now_too: bool) {
        if !self.dirty || !self.persist {
            return;
        }
        if !now_too && now.duration_since(self.last_write) < FLUSH_EVERY {
            return;
        }
        self.evict(crate::qmi_nas::now());
        match write_atomic(&self.dir.join("clients.json"), &self.clients_json()) {
            Ok(()) => {
                self.writes += 1;
                self.dirty = false;
            }
            Err(e) => {
                self.write_errors += 1;
                eprintln!("lanid: clients.json: {e}");
            }
        }
        self.last_write = now;
    }

    /// Sets (or with an empty `alias`, removes) the alias of `mac`, writes aliases.json and
    /// reads it back. The alias as stored.
    pub fn set_alias(&mut self, mac: &str, alias: &str) -> Result<(String, String), String> {
        let m = parse_mac(mac).ok_or("bad mac")?;
        let key = mac_str(&m);
        let alias = alias.trim();
        if !alias.is_empty() && !valid_value(alias) {
            return Err(format!(
                "alias: at most {MAX_STR} bytes, no control characters"
            ));
        }
        if !self.persist {
            return Err("not the main core: aliases are read-only here".into());
        }
        let mut next = self.aliases.clone();
        if alias.is_empty() {
            next.remove(&key);
        } else {
            next.insert(key.clone(), alias.to_owned());
        }
        if let Some(r) = self.records.get_mut(&key).filter(|r| !r.from.is_empty()) {
            // the owner named it themselves: no longer "taken over"
            r.from.clear();
            self.dirty = true;
        }
        self.write_aliases(next)?;
        let stored = self.aliases.get(&key).cloned().unwrap_or_default();
        Ok((key, stored))
    }

    /// Writes aliases.json, reads it back, then takes `next` as the aliases.
    fn write_aliases(&mut self, next: BTreeMap<String, String>) -> Result<(), String> {
        let path = self.dir.join("aliases.json");
        let bytes = serde_json::to_vec(&AliasesFile {
            version: VERSION,
            aliases: next.clone(),
        })
        .map_err(|e| e.to_string())?;
        write_atomic(&path, &bytes).map_err(|e| format!("aliases.json: {e}"))?;
        let back = read_json::<AliasesFile>(&path)
            .filter(|f| f.version == VERSION)
            .ok_or("aliases.json: readback failed")?;
        if back.aliases != next {
            return Err("aliases.json: readback differs".into());
        }
        self.aliases = next;
        Ok(())
    }

    fn status_now(&self) -> c::LanidStatus {
        let on = self.loaded && self.running;
        c::LanidStatus {
            batches: self.counters.batches,
            blocked: self.blocked,
            dropped: self.counters.dropped(),
            frames: self.counters.frames,
            loaded: self.loaded,
            records: self.records.len() as u64,
            sources: c::LanidSources {
                dhcp: on,
                mdns: on,
                netbios: on,
                ssdp: on,
            },
            traps: self.counters.traps,
            why: if on { String::new() } else { self.why.clone() },
        }
    }

    /// The status for the `clients` block: a new copy when on/off changed or after
    /// [`PUBLISH_EVERY`], else the last one (so the counters don't make an event per poll).
    fn published(&mut self, now: Instant) -> c::LanidStatus {
        let fresh = self.status_now();
        let keep = self.published.as_ref().is_some_and(|(at, p)| {
            now.duration_since(*at) < PUBLISH_EVERY
                && p.loaded == fresh.loaded
                && p.sources == fresh.sources
                && p.why == fresh.why
        });
        if !keep {
            self.published = Some((now, fresh));
        }
        self.published
            .as_ref()
            .map(|(_, p)| p.clone())
            .unwrap_or_default()
    }

    /// Fills in a client from the alias and (with the component on) what was learned; every
    /// candidate value with its source, the winner of each field first.
    pub fn enrich(&self, cl: &mut c::Client) -> Vec<Source> {
        let Some(m) = parse_mac(&cl.mac) else {
            return Vec::new();
        };
        let key = mac_str(&m);
        let alias = self.aliases.get(&key);
        if alias.is_none() && !self.loaded {
            return Vec::new();
        }
        let rec = self.records.get(&key).filter(|_| self.loaded);
        let fact = |k: Kind| rec.and_then(|r| r.facts.get(&k));
        let mut src: Vec<Source> = Vec::new();
        let mut push = |field: &str, source: &str, value: &str| {
            if !value.is_empty()
                && !src
                    .iter()
                    .any(|s| s.field == field && s.source == source && s.value == value)
            {
                src.push(Source {
                    field: field.into(),
                    source: source.into(),
                    value: value.into(),
                });
            }
        };
        if let Some(a) = alias {
            push("name", "alias", a);
        }
        let mdns = fact(Kind::MdnsInstance).filter(|v| classify::readable_name(v));
        // Android sends its device name (`OnePlus-Ace-3-Pro`) as the DHCP hostname, while an
        // iPhone sends a bare `iPhone`: for Android the DHCP name goes first
        let dhcp_first = fact(Kind::DhcpVendorClass).is_some_and(|v| v.starts_with("android"))
            && fact(Kind::DhcpHostname)
                .is_some_and(|v| classify::readable_name(v) && !v.starts_with("android"));
        if let Some(v) = mdns.filter(|_| !dhcp_first) {
            push("name", "mdns", v);
        }
        // the router's own lists (vendor access list, dnsmasq leases) are DHCP names too
        if !cl.name.is_empty() && cl.name != cl.mac {
            push("name", "dhcp", &cl.name);
        }
        if let Some(v) = fact(Kind::DhcpHostname).filter(|v| classify::readable_name(v)) {
            push("name", "dhcp", v);
        }
        if let Some(v) = mdns.filter(|_| dhcp_first) {
            push("name", "mdns", v);
        }
        if let Some(v) = fact(Kind::NetbiosName).filter(|v| classify::readable_name(v)) {
            push("name", "netbios", v);
        }
        if let Some(v) = classify::model_name(rec) {
            push("name", "mdns", &v);
        }
        let private = is_private(&m);
        if self.loaded {
            let oui = if private { None } else { self.oui.as_ref() };
            for (source, v) in classify::vendors(rec, &m, oui) {
                push("vendor", source, &v);
            }
            for (source, v) in classify::kinds(rec) {
                push("kind", source, v);
            }
        }
        let first = |f: &str| {
            src.iter()
                .find(|s| s.field == f)
                .map(|s| s.value.clone())
                .unwrap_or_default()
        };
        if let Some(n) = Some(first("name")).filter(|n| !n.is_empty()) {
            cl.name = n;
        }
        cl.vendor = first("vendor");
        cl.kind = first("kind");
        cl.alias = alias.cloned().unwrap_or_default();
        cl.alias_from = rec
            .filter(|_| alias.is_some())
            .map(|r| r.from.clone())
            .unwrap_or_default();
        cl.private = private && self.loaded;
        src
    }

    /// `GET /api/lanid`: the status, every record and the aliases, and per MAC the merged
    /// fields with every source (the detail view; display only).
    pub fn report(&self) -> Value {
        let macs: std::collections::BTreeSet<&String> =
            self.records.keys().chain(self.aliases.keys()).collect();
        let merged: BTreeMap<&String, Value> = macs
            .into_iter()
            .map(|m| {
                let mut cl = c::Client {
                    mac: m.clone(),
                    name: m.clone(),
                    ..Default::default()
                };
                let sources = self.enrich(&mut cl);
                (m, json!({"client": cl, "sources": sources}))
            })
            .collect();
        json!({
            "merged": merged,
            "status": self.status_now(),
            "counters": self.counters,
            "per_source": self.per_source,
            "writes": self.writes,
            "write_errors": self.write_errors,
            "oui": self.oui.as_ref().map(|o| o.prefixes()),
            "records": self.records,
            "aliases": self.aliases,
        })
    }
}

// ── the hooks the rest of the core calls ────────────────────────────────────

/// Serve mode, once (from [`crate::lanid_capture::start`]): the shared state for `dir` and the
/// component, when this is the main core and one loads. The reason it is off goes in the status.
pub fn start(dir: &Path, main: bool, plugins: &Path) -> Option<Component> {
    let (comp, why) = if main {
        match Component::load(plugins) {
            Ok(c) => (Some(c), String::new()),
            Err(e) => (None, e),
        }
    } else {
        (None, "side-by-side core".into())
    };
    if let Some(e) = Some(&why).filter(|w| !w.is_empty()) {
        eprintln!("lanid: off ({e})");
    }
    let mut s = Shared::open(dir, main);
    s.started = true;
    s.loaded = comp.is_some();
    s.running = false;
    s.why = if why.is_empty() {
        "not running yet".into()
    } else {
        why
    };
    match SHARED.get() {
        Some(m) => *m.lock().unwrap_or_else(|e| e.into_inner()) = s,
        None => {
            let _ = SHARED.set(Mutex::new(s));
        }
    }
    comp
}

/// The socket is up (or, with `why`, could not be opened: all sources off).
pub fn running(why: Option<String>) {
    let mut s = lock();
    s.running = why.is_none();
    if let Some(w) = why {
        s.why = w;
    }
}

/// One batch on the `lanid` thread: parse outside the lock, merge inside.
pub fn process(gate: &mut Gate, p: &dyn Parse, frames: Vec<Vec<u8>>) {
    let now = Instant::now();
    let (learned, n) = gate.run(p, frames, now);
    let wall = crate::qmi_nas::now();
    let mut s = lock();
    for (m, facts) in &learned {
        s.learn(m, facts, wall);
    }
    s.counters.add(&n);
    s.blocked = gate.blocked_now(now);
    s.maybe_flush(now, false);
}

/// Idle wake-up or exit: write what is pending (on exit without waiting for the hour).
pub fn flush(exiting: bool) {
    if let Some(m) = SHARED.get() {
        m.lock()
            .unwrap_or_else(|e| e.into_inner())
            .maybe_flush(Instant::now(), exiting);
    }
}

/// The projection's hook: fill in each client, and the status for the block (None unless
/// [`start`] ran, so other modes and tests see the block as before).
pub fn project(list: &mut [c::Client]) -> Option<c::LanidStatus> {
    let m = SHARED.get()?;
    let mut s = m.lock().unwrap_or_else(|e| e.into_inner());
    let present: Vec<String> = list
        .iter()
        .filter_map(|cl| Some(mac_str(&parse_mac(&cl.mac)?)))
        .collect();
    let strangers = s.watch(&present, Instant::now(), crate::qmi_nas::now());
    for cl in list.iter_mut() {
        s.enrich(cl);
    }
    let status = s.started.then(|| s.published(Instant::now()));
    drop(s);
    if !strangers.is_empty() {
        watch::alert(&strangers, list);
    }
    status
}

/// `POST /api/lanid/alias` and `GET /api/lanid`.
pub fn set_alias(mac: &str, alias: &str) -> Result<(String, String), String> {
    lock().set_alias(mac, alias)
}

pub fn report() -> Value {
    lock().report()
}

/// Tests: let the shared state write (it is the main core's).
#[cfg(test)]
pub fn test_writable() {
    lock().persist = true;
}

#[cfg(test)]
mod tests;
