//! The network recorder (2a, first batch; docs/designs/net-performance.md「2a 记录仪细节设计」).
//!
//! Off unless `LUYAZ_NETREC=1` (read once). When off nothing here runs. When on, it rides the
//! collect round (`state::read_inputs`, inside the blocking closure that already reads /proc and
//! /sys): no timer, no thread, no ubus of its own.
//!
//! **Trigger.** Inputs are what the round already read: the vendor counters (`zwrt_data
//! get_wwandst type 1`, IPA-forwarded + device-local traffic), `rmnet_data0` (from
//! `link_rates`, read every round), the LAN + Wi-Fi client lists (TTL 10 s; a failed read counts
//! as "has clients"). Device-local traffic (speed tests, tailscaled) does not count (user
//! decision 10-26):
//!
//! ```text
//! local  = max(0, Δrmnet_data0 − Δchill0)      CPU-path bytes that are not proxied clients
//! client = max(0, Δvendor − local)             rate = client · 1000 / window_ms   (bytes/s)
//! ```
//!
//! `chill0` is the only extra read, and only when a session is open or the vendor rate alone is
//! ≥ the start threshold (`client ≤ vendor`, so below it nothing could start). No `chill0`
//! (CHILL off) = Δchill0 0, all of rmnet is local. A first `chill0` read of a present
//! interface has no baseline: `local` = 0 for that round (it can only over-count one round; a start
//! needs three). So an idle round costs a subtraction and a comparison.
//!
//! - start: 3 rounds in a row ≥ 1 Mbps ([`ON_BPS`]) and clients ≠ 0;
//! - stop: [`OFF_MS`] of monotonic time < 0.5 Mbps ([`OFF_BPS`]), clients = 0, or the 2 h cap
//!   ([`CAP_MS`]; a new session needs three fresh rounds);
//! - a round without a usable reading (vendor read failed, counters went backwards = redial or
//!   SIM change, no time passed) neither starts nor stops; it breaks the start streak, does not
//!   touch the stop timer.
//!
//! **Storage.** One fixed 160-byte row per round in session → [`RING`] (tmpfs, 2 MiB, wraps;
//! a restart resumes at the header's index). Every 10 s a summary line, plus start/end lines,
//! → `DIR/YYYYMMDD.jsonl` (device date), buffered to 4 KiB or session end, one write + fsync.
//! A day file past [`DAY_CAP`] takes only start/end lines (the session is marked `trimmed`);
//! the directory is kept under [`DIR_CAP`] (oldest day first, never today). No session, no flash.
//!
//! **Privacy.** Nothing here has an address: counters, radio numbers, band/RAT strings. Not a
//! hub block, so not in the event log or anything built from it (the shareable bundle).

use serde_json::{Value, json};
use std::fs;
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub const RING: &str = "/tmp/luyaz-netrec.ring";
pub const DIR: &str = "/data/luyaz/core/netrec";
pub const RING_BYTES: u64 = 2 << 20;
pub const ROW: usize = 160;
const HEAD: u64 = 64;
const MAGIC: &[u8; 8] = b"NETREC1\0";
/// 1 Mbps and 0.5 Mbps, in bytes per second.
pub const ON_BPS: u64 = 125_000;
pub const OFF_BPS: u64 = 62_500;
pub const ON_ROUNDS: u32 = 3;
pub const OFF_MS: u64 = 60_000;
pub const CAP_MS: u64 = 2 * 3600 * 1000;
pub const SUMMARY_MS: u64 = 10_000;
pub const FLUSH_BYTES: usize = 4096;
pub const DIR_CAP: u64 = 8 << 20;
pub const DAY_CAP: u64 = 1 << 20;
const NONE: u64 = u64::MAX;

/// `LUYAZ_NETREC=1`, read once.
pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("LUYAZ_NETREC").as_deref() == Ok("1"))
}

fn slots() -> u64 {
    (RING_BYTES - HEAD) / ROW as u64
}

// ---------------------------------------------------------------------------------------------
// Inputs

/// The vendor counters of one round (`get_wwandst type 1`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Vendor {
    pub rx: u64,
    pub tx: u64,
    pub rx_pkts: u64,
    pub tx_pkts: u64,
    pub rx_speed: u64,
    pub tx_speed: u64,
}

/// Radio numbers of one round (`nwinfo_get_netinfo`). SNR in tenths of a dB.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Radio {
    pub rat: String,
    pub band: String,
    pub bandwidth: String,
    pub pci: i64,
    pub cell_id: i64,
    pub nr_snr: i64,
    pub lte_snr: i64,
    pub rsrp: i64,
}

/// What the round read before the blocking closure (plain values, `Send`).
#[derive(Clone, Debug, Default)]
pub struct Pre {
    pub vendor: Option<Vendor>,
    pub clients: Option<usize>,
    pub radio: Radio,
}

fn num(v: &Value, k: &str) -> u64 {
    match v.get(k) {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

fn tenths(v: &Value, k: &str) -> i64 {
    let s = match v.get(k) {
        Some(Value::String(s)) => s.trim().to_owned(),
        Some(Value::Number(n)) => n.to_string(),
        _ => return i64::MIN,
    };
    s.parse::<f64>()
        .map_or(i64::MIN, |f| (f * 10.0).round() as i64)
}

impl Pre {
    pub fn from_reads(
        traffic: &Result<Value, String>,
        lan: &Result<Value, String>,
        wifi: &Result<Value, String>,
        net: &Result<Value, String>,
    ) -> Pre {
        let vendor = traffic.as_ref().ok().and_then(|t| {
            t.get("real_rx_bytes")?;
            Some(Vendor {
                rx: num(t, "real_rx_bytes"),
                tx: num(t, "real_tx_bytes"),
                rx_pkts: num(t, "real_rx_packets"),
                tx_pkts: num(t, "real_tx_packets"),
                rx_speed: num(t, "real_rx_speed"),
                tx_speed: num(t, "real_tx_speed"),
            })
        });
        let count = |r: &Result<Value, String>, k: &str| {
            r.as_ref().ok().map(|v| {
                // as `connected_clients`: an entry without a MAC is not a client
                v.get(k).and_then(Value::as_array).map_or(0, |a| {
                    a.iter()
                        .filter(|e| {
                            ["mac_address", "mac", "mac_addr"]
                                .iter()
                                .any(|m| !crate::project::snapshot::string(e, m).is_empty())
                        })
                        .count()
                })
            })
        };
        // either read failing counts as "has clients" (None)
        let clients = match (
            count(lan, "lan_access_list_info"),
            count(wifi, "wireless_access_list_info"),
        ) {
            (Some(a), Some(b)) => Some(a + b),
            _ => None,
        };
        let radio = net.as_ref().map(radio).unwrap_or_default();
        Pre {
            vendor,
            clients,
            radio,
        }
    }
}

fn radio(n: &Value) -> Radio {
    let s = |k: &str| crate::project::snapshot::string(n, k);
    let i = |k: &str| crate::project::snapshot::integer(n, k);
    let rat = s("network_type");
    let nr = rat == "SA" || rat == "NSA";
    Radio {
        band: s("wan_active_band"),
        bandwidth: s(if nr {
            "nr5g_bandwidth"
        } else {
            "lte_bandwidth"
        }),
        pci: i(if rat == "SA" { "nr5g_pci" } else { "lte_pci" }),
        cell_id: i(if rat == "SA" {
            "nr5g_cell_id"
        } else {
            "cell_id"
        }),
        nr_snr: tenths(n, "nr5g_snr"),
        lte_snr: tenths(n, "lte_snr"),
        rsrp: i(if rat == "SA" { "nr5g_rsrp" } else { "lte_rsrp" }),
        rat,
    }
}

// ---------------------------------------------------------------------------------------------
// Trigger

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    Idle,
    Start,
    Keep,
    Stop(&'static str),
}

/// The start/stop state machine. `rate`: client bytes/s of a usable round, `None` otherwise.
#[derive(Clone, Debug, Default)]
pub struct Trigger {
    streak: u32,
    low_since: Option<u64>,
    open_at: Option<u64>,
}

impl Trigger {
    pub fn open(&self) -> bool {
        self.open_at.is_some()
    }

    pub fn step(&mut self, at: u64, rate: Option<u64>, clients: Option<usize>) -> Act {
        if let Some(start) = self.open_at {
            let stop = if clients == Some(0) {
                Some("clients")
            } else if at.saturating_sub(start) >= CAP_MS {
                Some("cap")
            } else {
                match rate {
                    Some(r) if r < OFF_BPS => {
                        let since = *self.low_since.get_or_insert(at);
                        (at.saturating_sub(since) >= OFF_MS).then_some("idle")
                    }
                    Some(_) => {
                        self.low_since = None;
                        None
                    }
                    None => None,
                }
            };
            return match stop {
                Some(why) => {
                    *self = Trigger::default();
                    Act::Stop(why)
                }
                None => Act::Keep,
            };
        }
        match rate {
            Some(r) if r >= ON_BPS && clients != Some(0) => {
                self.streak += 1;
                if self.streak >= ON_ROUNDS {
                    self.streak = 0;
                    self.low_since = None;
                    self.open_at = Some(at);
                    return Act::Start;
                }
            }
            _ => self.streak = 0,
        }
        Act::Idle
    }
}

// ---------------------------------------------------------------------------------------------
// I/O seam

/// Everything the recorder touches outside memory. Tests count the calls.
pub trait Io {
    /// Cumulative (rx, tx) of a network interface.
    fn iface(&mut self, name: &str) -> Option<(u64, u64)>;
    /// One ring row.
    fn ring(&mut self, row: &[u8; ROW]);
    /// Bytes in the day file now.
    fn day_size(&mut self, day: &str) -> u64;
    /// Append and fsync.
    fn append(&mut self, day: &str, data: &[u8]);
    /// Keep the day files under [`DIR_CAP`], never deleting `today`.
    fn prune(&mut self, today: &str);
}

/// The device's files.
pub struct Fs {
    pub ring: PathBuf,
    pub dir: PathBuf,
    pub net: PathBuf,
    file: Option<fs::File>,
    next: u64,
    count: u64,
}

impl Fs {
    pub fn new(ring: &Path, dir: &Path, net: &Path) -> Fs {
        Fs {
            ring: ring.into(),
            dir: dir.into(),
            net: net.into(),
            file: None,
            next: 0,
            count: 0,
        }
    }

    fn ring_file(&mut self) -> Option<&fs::File> {
        if self.file.is_none() {
            let f = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&self.ring)
                .ok()?;
            let mut head = [0u8; 24];
            let ok = f.read_exact_at(&mut head, 0).is_ok() && &head[..8] == MAGIC;
            let (next, count) = if ok {
                let g = |i: usize| u64::from_le_bytes(head[i..i + 8].try_into().unwrap());
                (g(8), g(16))
            } else {
                (0, 0)
            };
            // a header that does not fit the ring starts over
            (self.next, self.count) = if next < slots() && count <= slots() {
                (next, count)
            } else {
                (0, 0)
            };
            let _ = f.set_len(RING_BYTES);
            self.file = Some(f);
        }
        self.file.as_ref()
    }
}

impl Io for Fs {
    fn iface(&mut self, name: &str) -> Option<(u64, u64)> {
        let st = self.net.join(name).join("statistics");
        let c =
            |f: &str| -> Option<u64> { fs::read_to_string(st.join(f)).ok()?.trim().parse().ok() };
        Some((c("rx_bytes")?, c("tx_bytes")?))
    }

    fn ring(&mut self, row: &[u8; ROW]) {
        if self.ring_file().is_none() {
            return;
        }
        let at = HEAD + self.next * ROW as u64;
        let next = (self.next + 1) % slots();
        let count = (self.count + 1).min(slots());
        let mut head = [0u8; 24];
        head[..8].copy_from_slice(MAGIC);
        head[8..16].copy_from_slice(&next.to_le_bytes());
        head[16..24].copy_from_slice(&count.to_le_bytes());
        let f = self.file.as_ref().unwrap();
        if f.write_all_at(row, at).is_ok() && f.write_all_at(&head, 0).is_ok() {
            (self.next, self.count) = (next, count);
        }
    }

    fn day_size(&mut self, day: &str) -> u64 {
        fs::metadata(self.dir.join(format!("{day}.jsonl"))).map_or(0, |m| m.len())
    }

    fn append(&mut self, day: &str, data: &[u8]) {
        let _ = fs::create_dir_all(&self.dir);
        if let Ok(mut f) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(format!("{day}.jsonl")))
        {
            let _ = f.write_all(data);
            let _ = f.sync_data();
        }
    }

    fn prune(&mut self, today: &str) {
        let files = day_files(&self.dir);
        let sizes: Vec<(String, u64)> = files
            .iter()
            .map(|n| {
                (
                    n.clone(),
                    fs::metadata(self.dir.join(n)).map_or(0, |m| m.len()),
                )
            })
            .collect();
        for n in doomed(&sizes, today) {
            let _ = fs::remove_file(self.dir.join(n));
        }
    }
}

fn is_day_file(n: &str) -> bool {
    n.len() == 14 && n.ends_with(".jsonl") && n.as_bytes()[..8].iter().all(u8::is_ascii_digit)
}

/// Day file names in `dir`, oldest first.
pub fn day_files(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok()?.file_name().into_string().ok())
                .filter(|n| is_day_file(n))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// The oldest files to delete so the rest fit in [`DIR_CAP`]; `today` stays.
pub fn doomed(sizes: &[(String, u64)], today: &str) -> Vec<String> {
    let mut total: u64 = sizes.iter().map(|(_, s)| s).sum();
    let mut out = Vec::new();
    for (n, s) in sizes {
        if total <= DIR_CAP {
            break;
        }
        if n.starts_with(today) {
            continue;
        }
        total -= s;
        out.push(n.clone());
    }
    out
}

/// `YYYYMMDD` of a device epoch (local digits, see dsds_log).
pub fn day(wall: i64) -> String {
    crate::dsds_log::date(wall).replace('-', "")
}

// ---------------------------------------------------------------------------------------------
// Rows

/// One round in a session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    pub t_ms: u64,
    pub wall: i64,
    pub session: i64,
    pub window_ms: u32,
    pub ev: u8,
    pub cell: Vendor,
    pub rmnet: Option<(u64, u64)>,
    pub chill0: Option<(u64, u64)>,
    pub brlan: Option<(u64, u64)>,
    pub rate: u64,
    pub radio: Radio,
    pub cpu_tenths: i64,
}

pub const EV_CELL: u8 = 1;
pub const EV_BAND: u8 = 2;
pub const EV_CA: u8 = 4;

fn put_str(b: &mut [u8], s: &str) {
    let n = s.len().min(b.len());
    b[..n].copy_from_slice(&s.as_bytes()[..n]);
}

fn get_str(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

fn clamp16(v: i64) -> i16 {
    if v == i64::MIN {
        i16::MIN
    } else {
        v.clamp(-32767, 32767) as i16
    }
}

fn un16(v: i16) -> Option<i64> {
    (v != i16::MIN).then_some(v as i64)
}

impl Row {
    pub fn encode(&self) -> [u8; ROW] {
        let mut b = [0u8; ROW];
        let mut u = |at: usize, v: u64| b[at..at + 8].copy_from_slice(&v.to_le_bytes());
        u(0, self.t_ms);
        u(8, self.wall as u64);
        u(16, self.session as u64);
        u(32, self.cell.rx);
        u(40, self.cell.tx);
        u(48, self.cell.rx_pkts);
        u(56, self.cell.tx_pkts);
        u(64, self.cell.rx_speed);
        u(72, self.cell.tx_speed);
        let pair = |p: Option<(u64, u64)>| p.unwrap_or((NONE, NONE));
        let (a, c) = pair(self.rmnet);
        u(80, a);
        u(88, c);
        let (a, c) = pair(self.chill0);
        u(96, a);
        u(104, c);
        let (a, c) = pair(self.brlan);
        u(112, a);
        u(120, c);
        u(140, self.radio.cell_id as u64);
        u(24, u64::from(self.window_ms) | (u64::from(self.ev) << 32));
        b[128..132].copy_from_slice(&(self.radio.pci as u32).to_le_bytes());
        b[132..134].copy_from_slice(&clamp16(self.radio.nr_snr).to_le_bytes());
        b[134..136].copy_from_slice(&clamp16(self.radio.lte_snr).to_le_bytes());
        b[136..138].copy_from_slice(&clamp16(self.radio.rsrp).to_le_bytes());
        b[138..140].copy_from_slice(&clamp16(self.cpu_tenths).to_le_bytes());
        put_str(&mut b[148..156], &self.radio.band);
        put_str(&mut b[156..160], &self.radio.rat);
        // rate is not stored: derived from the deltas when read
        b
    }

    pub fn decode(b: &[u8]) -> Row {
        let g = |at: usize| u64::from_le_bytes(b[at..at + 8].try_into().unwrap());
        let i16at = |at: usize| i16::from_le_bytes([b[at], b[at + 1]]);
        let pair = |at: usize| {
            let (a, c) = (g(at), g(at + 8));
            (a != NONE).then_some((a, c))
        };
        let w = g(24);
        Row {
            t_ms: g(0),
            wall: g(8) as i64,
            session: g(16) as i64,
            window_ms: w as u32,
            ev: (w >> 32) as u8,
            cell: Vendor {
                rx: g(32),
                tx: g(40),
                rx_pkts: g(48),
                tx_pkts: g(56),
                rx_speed: g(64),
                tx_speed: g(72),
            },
            rmnet: pair(80),
            chill0: pair(96),
            brlan: pair(112),
            rate: 0,
            radio: Radio {
                rat: get_str(&b[156..160]),
                band: get_str(&b[148..156]),
                bandwidth: String::new(),
                pci: u32::from_le_bytes(b[128..132].try_into().unwrap()) as i64,
                cell_id: g(140) as i64,
                nr_snr: un16(i16at(132)).unwrap_or(i64::MIN),
                lte_snr: un16(i16at(134)).unwrap_or(i64::MIN),
                rsrp: un16(i16at(136)).unwrap_or(i64::MIN),
            },
            cpu_tenths: un16(i16at(138)).unwrap_or(-1),
        }
    }
}

fn opt(v: i64) -> Value {
    if v == i64::MIN { Value::Null } else { json!(v) }
}

fn ev_names(ev: u8) -> Vec<&'static str> {
    [(EV_CELL, "cell"), (EV_BAND, "band"), (EV_CA, "ca")]
        .into_iter()
        .filter(|(b, _)| ev & b != 0)
        .map(|(_, n)| n)
        .collect()
}

/// One row as JSON; `prev` (the row before, same session) gives the deltas.
pub fn row_json(r: &Row, prev: Option<&Row>) -> Value {
    let d = |a: Option<(u64, u64)>, b: Option<(u64, u64)>| -> Value {
        match (a, b) {
            (Some(a), Some(b)) => json!(a.0.saturating_sub(b.0) + a.1.saturating_sub(b.1)),
            _ => Value::Null,
        }
    };
    let pairs = |p: Option<(u64, u64)>| p.map_or(json!(null), |(a, b)| json!([a, b]));
    let (dcell, drmnet, dchill) = match prev {
        Some(p) => (
            d(Some((r.cell.rx, r.cell.tx)), Some((p.cell.rx, p.cell.tx))),
            d(r.rmnet, p.rmnet),
            d(r.chill0, p.chill0),
        ),
        None => (Value::Null, Value::Null, Value::Null),
    };
    let ipa = match (dcell.as_u64(), drmnet.as_u64()) {
        (Some(c), Some(m)) => json!(c.saturating_sub(m)),
        _ => Value::Null,
    };
    json!({
        "t": r.t_ms, "wall": r.wall, "session": r.session, "window_ms": r.window_ms,
        "cell_rx_bytes": r.cell.rx, "cell_tx_bytes": r.cell.tx,
        "cell_rx_pkts": r.cell.rx_pkts, "cell_tx_pkts": r.cell.tx_pkts,
        "vendor_rx_speed": r.cell.rx_speed, "vendor_tx_speed": r.cell.tx_speed,
        "rmnet": pairs(r.rmnet), "chill0": pairs(r.chill0), "brlan": pairs(r.brlan),
        "d_cell": dcell, "d_rmnet": drmnet, "d_chill0": dchill, "ipa_share": ipa,
        "rat": r.radio.rat, "band": r.radio.band, "pci": r.radio.pci, "cell_id": r.radio.cell_id,
        "nr_snr": opt(r.radio.nr_snr), "lte_snr": opt(r.radio.lte_snr), "rsrp": opt(r.radio.rsrp),
        "cpu": r.cpu_tenths, "ev": ev_names(r.ev),
    })
}

// ---------------------------------------------------------------------------------------------
// Recorder

#[derive(Clone, Debug, Default)]
struct Window {
    from_ms: u64,
    rows: u32,
    rates: Vec<u64>,
    cell: u64,
    direct: u64,
    proxy: u64,
    local: u64,
    snr: Option<(i64, i64)>,
    ev: u8,
    cpu_sum: i64,
    cpu_n: i64,
}

#[derive(Clone, Debug)]
struct Session {
    id: i64,
    start_ms: u64,
    bytes: u64,
    trimmed: bool,
    win: Window,
}

/// A line waiting for the flush; `mark` = start/end (kept past the day budget).
struct Line {
    day: String,
    mark: bool,
    text: String,
}

#[derive(Default)]
pub struct Recorder {
    trigger: Trigger,
    vendor: Option<(Vendor, u64)>,
    rmnet: Option<(u64, u64)>,
    chill0: Option<(u64, u64)>,
    /// The round `chill0` was read last (a baseline only counts from the round before).
    chill0_round: u64,
    round: u64,
    radio: Option<Radio>,
    session: Option<Session>,
    pending: Vec<Line>,
    pending_bytes: usize,
}

fn delta(now: Option<(u64, u64)>, then: Option<(u64, u64)>) -> Option<u64> {
    let (n, t) = (now?, then?);
    (n.0 >= t.0 && n.1 >= t.1).then(|| (n.0 - t.0) + (n.1 - t.1))
}

impl Recorder {
    pub fn active(&self) -> Option<Value> {
        self.session
            .as_ref()
            .map(|s| json!({"session": s.id, "bytes": s.bytes, "trimmed": s.trimmed}))
    }

    /// One collect round. `rmnet` comes from the round's own `link_rates`.
    pub fn tick(
        &mut self,
        io: &mut dyn Io,
        pre: Pre,
        rmnet: Option<(u64, u64)>,
        cpu_tenths: i64,
        at_ms: u64,
        wall: i64,
    ) {
        self.round += 1;
        let open = self.trigger.open();
        let prev_vendor = self.vendor.take();
        let prev_rmnet = std::mem::replace(&mut self.rmnet, rmnet);
        self.vendor = pre.vendor.map(|v| (v, at_ms));
        let usable = match (pre.vendor, prev_vendor) {
            (Some(v), Some((p, then))) if at_ms > then && v.rx >= p.rx && v.tx >= p.tx => {
                Some(((v.rx - p.rx) + (v.tx - p.tx), at_ms - then))
            }
            _ => None,
        };
        let mut chill_d = None;
        let mut chill_now = None;
        let mut chill_read = false;
        let rate = usable.map(|(dv, win)| {
            let vendor_rate = dv.saturating_mul(1000) / win;
            if !open && vendor_rate < ON_BPS {
                return vendor_rate;
            }
            chill_now = io.iface("chill0");
            chill_read = true;
            // no chill0 (CHILL off): nothing is proxied, all of rmnet is the device's own
            if chill_now.is_none() {
                chill_d = Some(0);
            } else if self.chill0_round + 1 == self.round {
                chill_d = delta(chill_now, self.chill0);
            }
            let local = match delta(rmnet, prev_rmnet) {
                Some(dr) => chill_d.map_or(0, |dc| dr.saturating_sub(dc)),
                None => 0,
            };
            dv.saturating_sub(local).saturating_mul(1000) / win
        });
        if chill_read {
            self.chill0 = chill_now;
            self.chill0_round = self.round;
        }
        let act = self.trigger.step(at_ms, rate, pre.clients);
        // radio events against the previous round (kept only while it matters)
        let ev = match &self.radio {
            Some(p) if matches!(act, Act::Keep | Act::Stop(_)) => {
                let r = &pre.radio;
                (if p.pci != r.pci || p.cell_id != r.cell_id {
                    EV_CELL
                } else {
                    0
                }) | (if p.band != r.band { EV_BAND } else { 0 })
                    | (if p.bandwidth != r.bandwidth { EV_CA } else { 0 })
            }
            _ => 0,
        };
        match act {
            Act::Idle => {
                self.radio = None;
                return;
            }
            Act::Start => {
                self.session = Some(Session {
                    id: wall,
                    start_ms: at_ms,
                    bytes: 0,
                    trimmed: false,
                    win: Window {
                        from_ms: at_ms,
                        ..Default::default()
                    },
                });
                self.line(
                    wall,
                    true,
                    json!({"kind":"start","session":wall,"ts":wall,
                           "wall":crate::ops::record::civil(wall.max(0) as u64)}),
                );
            }
            Act::Keep | Act::Stop(_) => {}
        }
        if !chill_read {
            self.chill0 = io.iface("chill0");
            self.chill0_round = self.round;
        }
        let brlan = io.iface("br-lan");
        let Some(mut s) = self.session.take() else {
            return;
        };
        let row = Row {
            t_ms: at_ms,
            wall,
            session: s.id,
            window_ms: usable.map_or(0, |(_, w)| w.min(u64::from(u32::MAX)) as u32),
            ev,
            cell: pre.vendor.unwrap_or_default(),
            rmnet,
            chill0: self.chill0,
            brlan,
            rate: rate.unwrap_or(0),
            radio: pre.radio.clone(),
            cpu_tenths,
        };
        io.ring(&row.encode());
        // the window
        if let Some((dv, _)) = usable {
            let dr = delta(rmnet, prev_rmnet);
            let w = &mut s.win;
            w.rates.push(row.rate);
            w.cell += dv;
            s.bytes += dv;
            if let Some(dr) = dr {
                w.direct += dv.saturating_sub(dr);
                let dc = chill_d.unwrap_or(0);
                w.proxy += dc;
                w.local += dr.saturating_sub(dc);
            } else {
                w.direct += dv;
            }
        }
        {
            let w = &mut s.win;
            w.rows += 1;
            w.ev |= ev;
            let snr = if matches!(pre.radio.rat.as_str(), "SA" | "NSA") {
                pre.radio.nr_snr
            } else {
                pre.radio.lte_snr
            };
            if snr != i64::MIN {
                w.snr = Some(w.snr.map_or((snr, snr), |(a, b)| (a.min(snr), b.max(snr))));
            }
            if cpu_tenths >= 0 {
                w.cpu_sum += cpu_tenths;
                w.cpu_n += 1;
            }
        }
        self.radio = Some(pre.radio);
        let stop = match act {
            Act::Stop(why) => Some(why),
            _ => None,
        };
        if stop.is_some() || at_ms.saturating_sub(s.win.from_ms) >= SUMMARY_MS {
            let w = std::mem::replace(
                &mut s.win,
                Window {
                    from_ms: at_ms,
                    ..Default::default()
                },
            );
            if w.rows > 0 {
                self.line(wall, false, summary(s.id, wall, at_ms, &w));
            }
        }
        if let Some(why) = stop {
            self.line(
                wall,
                true,
                json!({"kind":"end","session":s.id,"ts":wall,"reason":why,
                       "duration_ms":at_ms.saturating_sub(s.start_ms),"bytes":s.bytes,
                       "trimmed":s.trimmed}),
            );
            self.flush(io, &mut s);
            self.radio = None;
            return;
        }
        if self.pending_bytes >= FLUSH_BYTES {
            self.flush(io, &mut s);
        }
        self.session = Some(s);
    }

    fn line(&mut self, wall: i64, mark: bool, v: Value) {
        let mut text = v.to_string();
        text.push('\n');
        self.pending_bytes += text.len();
        self.pending.push(Line {
            day: day(wall),
            mark,
            text,
        });
    }

    fn flush(&mut self, io: &mut dyn Io, s: &mut Session) {
        let lines = std::mem::take(&mut self.pending);
        self.pending_bytes = 0;
        let mut i = 0;
        while i < lines.len() {
            let d = lines[i].day.clone();
            let size = io.day_size(&d);
            let mut buf = String::new();
            while i < lines.len() && lines[i].day == d {
                let l = &lines[i];
                if l.mark || size + (buf.len() + l.text.len()) as u64 <= DAY_CAP {
                    buf.push_str(&l.text);
                } else {
                    s.trimmed = true;
                }
                i += 1;
            }
            if !buf.is_empty() {
                // an end line written before the session knew it was trimmed says so here
                let buf = if s.trimmed {
                    buf.replace("\"trimmed\":false", "\"trimmed\":true")
                } else {
                    buf
                };
                io.append(&d, buf.as_bytes());
                io.prune(&d);
            }
        }
    }
}

fn summary(id: i64, wall: i64, at_ms: u64, w: &Window) -> Value {
    let mut r = w.rates.clone();
    r.sort_unstable();
    let p50 = r.get(r.len() / 2).copied();
    json!({
        "kind": "sum", "session": id, "ts": wall,
        "wall": crate::ops::record::civil(wall.max(0) as u64),
        "span_ms": at_ms.saturating_sub(w.from_ms), "rows": w.rows,
        // bits per second
        "rate_p50_bps": p50.map(|v| v * 8), "rate_max_bps": r.last().map(|v| v * 8),
        "cell_bytes": w.cell, "direct": w.direct, "proxy": w.proxy, "local": w.local,
        "snr": w.snr.map(|(a, b)| json!([a, b])),
        "ev": ev_names(w.ev),
        "cpu": (w.cpu_n > 0).then(|| w.cpu_sum / w.cpu_n),
    })
}

// ---------------------------------------------------------------------------------------------
// The process-wide recorder

fn global() -> &'static Mutex<(Recorder, Fs)> {
    static R: OnceLock<Mutex<(Recorder, Fs)>> = OnceLock::new();
    R.get_or_init(|| {
        let net = std::env::var("LUYAZ_NET_CLASS_ROOT").unwrap_or_else(|_| "/sys/class/net".into());
        Mutex::new((
            Recorder::default(),
            Fs::new(Path::new(RING), Path::new(DIR), Path::new(&net)),
        ))
    })
}

/// Called from the collect round (blocking pool) when [`enabled`].
pub fn tick(pre: Pre, runtime: &crate::contract::Runtime, at_ms: u64, wall: i64) {
    let rmnet = runtime
        .link_rates
        .iter()
        .find(|l| l.interface == "rmnet_data0")
        .map(|l| (l.rx_bytes, l.tx_bytes));
    let mut g = global().lock().unwrap_or_else(|e| e.into_inner());
    let (rec, fs) = &mut *g;
    rec.tick(fs, pre, rmnet, runtime.cpu_usage_tenths, at_ms, wall);
}

// ---------------------------------------------------------------------------------------------
// Readers (API, doctor)

/// `GET /v2/netrec/sessions`: newest first, at most `limit`.
pub fn sessions(dir: &Path, limit: usize) -> Value {
    let mut by: std::collections::BTreeMap<i64, Value> = Default::default();
    for n in day_files(dir) {
        let Ok(text) = fs::read_to_string(dir.join(&n)) else {
            continue;
        };
        for l in text.lines() {
            let Ok(v) = serde_json::from_str::<Value>(l) else {
                continue;
            };
            let Some(id) = v["session"].as_i64() else {
                continue;
            };
            let e = by.entry(id).or_insert_with(|| {
                json!({"session":id,"start":id,"end":null,"duration_ms":null,"bytes":0,
                       "trimmed":false,"closed":false})
            });
            match v["kind"].as_str() {
                Some("end") => {
                    e["end"] = v["ts"].clone();
                    e["duration_ms"] = v["duration_ms"].clone();
                    e["bytes"] = v["bytes"].clone();
                    e["trimmed"] = v["trimmed"].clone();
                    e["reason"] = v["reason"].clone();
                    e["closed"] = json!(true);
                }
                Some("sum") if e["closed"] != true => {
                    // no end line (core restarted): the last summary is the end
                    e["end"] = v["ts"].clone();
                    let b =
                        e["bytes"].as_u64().unwrap_or(0) + v["cell_bytes"].as_u64().unwrap_or(0);
                    e["bytes"] = json!(b);
                }
                _ => {}
            }
        }
    }
    let active = global()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .0
        .active();
    let list: Vec<Value> = by.into_values().rev().take(limit.clamp(1, 1000)).collect();
    json!({"enabled": enabled(), "active": active, "sessions": list})
}

/// `GET /v2/netrec/summary?since=&cursor=`: JSONL lines with `ts ≥ since`, from the cursor
/// (`YYYYMMDD:offset`) on, at most `limit` lines or 256 KiB. Returns (body, next cursor).
pub fn summary_lines(
    dir: &Path,
    since: i64,
    cursor: Option<&str>,
    limit: usize,
) -> (String, Option<String>) {
    let (cday, coff) = cursor
        .and_then(|c| c.split_once(':'))
        .map(|(d, o)| (d.to_owned(), o.parse::<u64>().unwrap_or(0)))
        .unwrap_or_else(|| (day(since), 0));
    let mut out = String::new();
    let mut n = 0;
    for f in day_files(dir) {
        let d = &f[..8];
        if d < cday.as_str() {
            continue;
        }
        let Ok(text) = fs::read_to_string(dir.join(&f)) else {
            continue;
        };
        let start = if d == cday { coff as usize } else { 0 }.min(text.len());
        let mut off = start;
        for l in text[start..].split_inclusive('\n') {
            if !l.ends_with('\n') {
                break; // a line still being written
            }
            if n >= limit.clamp(1, 5000) || out.len() + l.len() > 256 << 10 {
                return (out, Some(format!("{d}:{off}")));
            }
            off += l.len();
            let ts = serde_json::from_str::<Value>(l)
                .ok()
                .and_then(|v| v["ts"].as_i64());
            if ts.is_some_and(|t| t >= since) {
                out.push_str(l);
                n += 1;
            }
        }
    }
    (out, None)
}

/// `GET /v2/netrec/ring?session=&offset=&limit=`: the ring's rows of one session (default:
/// the newest), oldest first.
pub fn ring_rows(path: &Path, session: Option<i64>, offset: usize, limit: usize) -> Value {
    let Ok(b) = fs::read(path) else {
        return json!({"enabled": enabled(), "session": null, "total": 0, "rows": []});
    };
    if b.len() < HEAD as usize || &b[..8] != MAGIC {
        return json!({"enabled": enabled(), "session": null, "total": 0, "rows": []});
    }
    let g = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
    let (next, count) = (g(8).min(slots()), g(16).min(slots()));
    let first = (next + slots() - count) % slots();
    let rows: Vec<Row> = (0..count)
        .filter_map(|k| {
            let at = (HEAD + ((first + k) % slots()) * ROW as u64) as usize;
            b.get(at..at + ROW).map(Row::decode)
        })
        .collect();
    let want = session.or_else(|| rows.last().map(|r| r.session));
    let mine: Vec<&Row> = rows.iter().filter(|r| Some(r.session) == want).collect();
    let limit = limit.clamp(1, 2000);
    let page: Vec<Value> = mine
        .iter()
        .enumerate()
        .skip(offset)
        .take(limit)
        .map(|(i, r)| row_json(r, i.checked_sub(1).map(|j| mine[j])))
        .collect();
    json!({"enabled": enabled(), "session": want, "total": mine.len(), "offset": offset,
           "more": offset + page.len() < mine.len(), "rows": page})
}

/// The doctor line: on/off, bytes written today (from the day file itself).
pub fn doctor_detail(dir: &Path, on: bool, wall: i64) -> (String, String) {
    if !on {
        return ("关".into(), "off".into());
    }
    let size = fs::metadata(dir.join(format!("{}.jsonl", day(wall)))).map_or(0, |m| m.len());
    let kb = size.div_ceil(1024);
    (
        format!("开，今天写了 {kb} KB"),
        format!("on, {kb} KB written today"),
    )
}

#[cfg(test)]
mod tests;
