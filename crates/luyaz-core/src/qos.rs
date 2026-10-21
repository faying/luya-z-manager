//! QCI / 5QI and AMBR of the current data connection, recovered from the
//! modem's `[DATA]` lines in `/data/logfs/key.log` (and its rotated `.0`).

use serde::Serialize;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::net::Ipv4Addr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The tail of each log read. key.log grows about 1 MB a day (B31, b31-community §1), so a
/// connection older than about two days has its lines rolled out of it; then the current
/// address's `CONNECTED` line is not found and the values are marked `stale` instead of an
/// older bearer passing for the current one.
const TAIL_WINDOW: u64 = 2 << 20;

/// Which core the data connection rides on. NSA (EN-DC) still uses EPS
/// bearers; only SA has NR PDU sessions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Core {
    #[default]
    Unknown,
    Eps,
    Nr,
}

impl Core {
    fn label(self) -> &'static str {
        match self {
            Self::Eps => "eps",
            Self::Nr => "nr_pdu",
            Self::Unknown => "",
        }
    }
}

/// What the caller knows about the current registration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Query {
    /// Registered (serving) PLMN, `None` when not registered.
    pub serving: Option<(i64, i64)>,
    /// The SIM's home PLMN from the IMSI. A home-routed roaming APN names
    /// this one, not the visited network, so it never counts as foreign.
    pub home: Option<(i64, i64)>,
    pub core: Core,
    /// The WAN's IPv4 address now (`network.interface.zte_wan`), `None` when
    /// not known: matched against the log's `cidN CONNECTED … addr=` lines to
    /// tell the current connection's bearer from an older one.
    pub addr: Option<Ipv4Addr>,
    /// The vendor's QCI/5QI now (`zwrt_data_tmp.wwaniface1.qci`), `None` when not read:
    /// wins over the logs' value. On B31 SA it holds the NR session's 5QI (9 beside
    /// `[DATA] qci = 9 9` after the PDU session lines, 10-17); the label still comes from `bearer`.
    pub qci_now: Option<i64>,
}

/// The `qos` block's contract (every reader carries `bearer` and `stale` along):
/// the AMBR is the network's **ceiling** for the connection, not a promised or
/// measured speed; `qci` is a QCI for `bearer = eps` and a 5QI for `nr_pdu`
/// (label by `bearer`, not by the current radio mode); when `stale` is true the
/// values are history and nothing may be concluded from them (no "limited to
/// N Mbps" verdict).
///
/// Field order is the JSON key order: `qci`, `ambr_dl`, `ambr_ul`, `bearer`, `stale`.
#[derive(Clone, Default, Debug, PartialEq, Serialize)]
pub struct Values {
    /// QCI (EPS) or 5QI (NR PDU session); 0 when unknown.
    pub qci: i64,
    /// Downlink ceiling in Mbps with three decimals, `""` when unknown.
    pub ambr_dl: String,
    /// Uplink ceiling, same format.
    pub ambr_ul: String,
    /// `eps`, `nr_pdu`, or `""` if the logs held no data bearer at all.
    pub bearer: &'static str,
    /// The values are history, not the current limit: the bearer found is of
    /// the other core than the one the device is on now (e.g. an LTE bearer
    /// after moving to SA with no PDU session line logged yet), or it belongs
    /// to another connection than the current address's, or the current
    /// address's connection is no longer in the part of the logs read.
    pub stale: bool,
}

/// One data bearer / PDU session as reconstructed from the logs. Lines that
/// share core, PLMN and APN/DNN fold into the same entry.
#[derive(Clone, Default, Debug)]
struct Bearer {
    core: Core,
    plmn: (Option<i64>, Option<i64>),
    /// APN / DNN, lower case, cut at the first '.'
    apn: String,
    /// seen in the live log (not only in the rotated one)
    in_live_log: bool,
    /// ordinal of the last `[DATA]` line that mentioned it
    last_seen: usize,
    qci: Option<i64>,
    down: Option<f64>,
    up: Option<f64>,
    /// IPv4 of the `CONNECTED … addr=` line paired with its latest mention
    addr: Option<Ipv4Addr>,
}

/// How many `[DATA]` lines apart a bearer line and its `CONNECTED` line may be.
const PAIRING_DISTANCE: usize = 4;

// Scanning up to 4 MiB of log text every second (the state poll rate on a
// U60 Pro / MU5250) is wasteful when QCI/AMBR change only on bearer events,
// so the parsed result is kept for 30 s. Choosing a bearer for a query is
// cheap and is redone each call, so SA/NSA flapping costs no rescan.
// LUYAZ_CACHE=0 disables reuse, same switch as the state cache.
const CACHE_TTL: Duration = Duration::from_secs(30);
static CACHED: Mutex<Option<(Instant, Arc<Parsed>)>> = Mutex::new(None);

pub fn invalidate() {
    if let Ok(mut slot) = CACHED.lock() {
        slot.take();
    }
}

pub fn read(query: Query) -> Values {
    select(&logs(), query)
}

/// The parsed bearer lines of both logs (reused for 30 s). Picking one for a
/// query is [`select`], a pure function.
pub fn logs() -> Arc<Parsed> {
    let reuse = !matches!(std::env::var("LUYAZ_CACHE").as_deref(), Ok("0"));
    if reuse {
        let fresh = CACHED.lock().ok().and_then(|slot| {
            slot.as_ref()
                .filter(|(stamp, _)| stamp.elapsed() < CACHE_TTL)
                .map(|(_, p)| Arc::clone(p))
        });
        if let Some(hit) = fresh {
            return hit;
        }
    }
    let mut scanned = scan_log_files();
    scanned.observed_at = luyaz_api::clock::BootTime::read();
    let shared = Arc::new(scanned);
    if reuse && let Ok(mut slot) = CACHED.lock() {
        *slot = Some((Instant::now(), Arc::clone(&shared)));
    }
    shared
}

fn scan_log_files() -> Parsed {
    let env_or = |name: &str, fallback: &str| std::env::var(name).unwrap_or_else(|_| fallback.to_owned());
    let live = env_or("LUYAZ_KEY_LOG", "/data/logfs/key.log");
    let older = env_or("LUYAZ_KEY_LOG_ROTATED", "/data/logfs/key.log.0");
    // Oldest first: the rotated file, then the live one.
    let mut bodies = Vec::with_capacity(2);
    for (path, is_live) in [(older, false), (live, true)] {
        if let Some(body) = tail_of(Path::new(&path)) {
            bodies.push((body, is_live));
        }
    }
    let views: Vec<(&str, bool)> = bodies.iter().map(|(b, l)| (b.as_str(), *l)).collect();
    collect(&views)
}

/// Reads the "external" log shape: `[DATA] cid1, QCI=[9], DL_AMBR=[150000]kbps, …`.
/// The last complete cid1 line wins; a cid1 line missing one of the three
/// numbers makes the whole read fail (`None`).
pub fn parse_external(text: &str) -> Option<Values> {
    let mut last = None;
    for raw in text.lines().filter(|l| l.contains("[DATA]")) {
        let line = raw.to_ascii_lowercase();
        if !line.contains("cid1") {
            continue;
        }
        let qci = first_int_after(&line, "qci=")?;
        let down_kbps = first_int_after(&line, "dl_ambr=")?;
        let up_kbps = first_int_after(&line, "ul_ambr=")?;
        let sane = (1..=255).contains(&qci) && down_kbps >= 0 && up_kbps >= 0;
        if sane {
            last = Some(Values {
                qci,
                ambr_dl: format_mbps(down_kbps as f64 / 1000.0),
                ambr_ul: format_mbps(up_kbps as f64 / 1000.0),
                ..Values::default()
            });
        }
    }
    last
}

/// Last `TAIL_WINDOW` bytes of a file as text; when the read starts mid-file
/// the partial first line is dropped.
fn tail_of(path: &Path) -> Option<String> {
    let mut f = File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let from = len.saturating_sub(TAIL_WINDOW);
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut buf = Vec::new();
    f.take(TAIL_WINDOW).read_to_end(&mut buf).ok()?;
    let body = if from == 0 {
        &buf[..]
    } else {
        match buf.iter().position(|&b| b == b'\n') {
            Some(nl) => &buf[nl + 1..],
            None => &buf[..],
        }
    };
    Some(String::from_utf8_lossy(body).into_owned())
}

/// Every data bearer found in the logs, oldest first, plus a bare `qci = …`
/// seen outside any bearer context.
#[derive(Clone, Default, Debug)]
pub struct Parsed {
    /// Original physical log-read time. Reusing the parsed cache never renews this.
    pub(crate) observed_at: Option<luyaz_api::clock::BootTime>,
    bearers: Vec<Bearer>,
    /// first QCI seen on a line that belonged to no bearer
    bare_qci: Option<i64>,
    /// (line ordinal, address) of every `CONNECTED … addr=` line
    connected: Vec<(usize, Ipv4Addr)>,
}

/// Stronger than the display fallback: a complete bearer in the current log, of the
/// current core and connection. Bare UCI QCI, rotated/history and partial AMBR never prove it.
pub(crate) fn validated_read(
    parsed: &Parsed,
    query: Query,
) -> Option<(&'static str, serde_json::Value)> {
    let want_addr = query.addr?;
    let plmn_ok = |b: &Bearer| {
        // NR PDU sessions do not name a PLMN; EPS bearers must name ours.
        b.core == Core::Nr
            || match b.plmn {
                (Some(mcc), Some(mnc)) => {
                    let p = Some((mcc, mnc));
                    p == query.serving || p == query.home
                }
                _ => false,
            }
    };
    let b = parsed.bearers.iter().rev().find(|b| {
        b.in_live_log
            && b.core != Core::Unknown
            && b.core == query.core
            && b.addr == Some(want_addr)
            && plmn_ok(b)
    })?;
    let usable = |x: &Option<f64>| x.filter(|v| v.is_finite() && *v >= 0.0);
    let qci = b.qci.filter(|q| (1..=255).contains(q))?;
    let (down, up) = (usable(&b.down)?, usable(&b.up)?);
    let capability = match b.core {
        Core::Eps => "qos.eps.log.read",
        _ => "qos.nr_pdu.log.read",
    };
    let body = serde_json::json!({
        "qci": qci,
        "ambr_dl": format_mbps(down),
        "ambr_ul": format_mbps(up),
        "bearer": b.core.label(),
    });
    Some((capability, body))
}

/// The address of a `cidN CONNECTED … addr=a.b.c.d` line (not DISCONNECTED).
fn connected_addr(lower: &str) -> Option<Ipv4Addr> {
    let has_word = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| word == "connected");
    if !has_word {
        return None;
    }
    let rest = after(lower, "addr=")?;
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// The APN / DNN a bearer line names, lower case, up to the first '.'.
fn access_name(lower: &str) -> String {
    let Some(rest) = after(lower, "dnn=").or_else(|| after(lower, "access_point=")) else {
        return String::new();
    };
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '.' || c == ',')
        .unwrap_or(rest.len());
    rest[..end].to_owned()
}

/// Text following the first occurrence of `key`.
fn after<'a>(s: &'a str, key: &str) -> Option<&'a str> {
    s.find(key).map(|at| &s[at + key.len()..])
}

#[cfg(test)]
fn parse_texts(texts: &[(&str, bool)], query: Query) -> Values {
    select(&collect(texts), query)
}

/// Lines whose APN/DNN marks them as voice or emergency, never the data path.
const NON_DATA: [&str; 4] = ["dnn=ims", "dnn=sos", "dnn=emergency", "access_point=ims"];

/// State that lives for one log file: which bearer the next few lines talk
/// about, and a "default bearer qci" waiting for the bearer line after it.
#[derive(Default)]
struct FileWindow {
    /// bearer the following lines refer to, with lines left (this line included)
    subject: Option<(usize, u8)>,
    /// a QCI announced just before its bearer line, with lines left
    early_qci: Option<(i64, u8)>,
}

/// Line-by-line reconstruction across both log files.
#[derive(Default)]
struct Scanner {
    out: Parsed,
    line_no: usize,
    /// latest bearer line still without a `CONNECTED` line: (bearer index, line)
    unpaired_bearer: Option<(usize, usize)>,
    /// latest `CONNECTED` line no bearer has taken yet: (line, address)
    unpaired_conn: Option<(usize, Ipv4Addr)>,
}

impl Scanner {
    fn file(&mut self, text: &str, live: bool) {
        let mut win = FileWindow::default();
        for raw in text.lines().filter(|l| l.contains("[DATA]")) {
            self.line_no += 1;
            let line = raw.to_ascii_lowercase();
            self.note_connection(&line);
            let touched = self.bearer_line(&line, live, &mut win);
            self.qci_line(&line, &mut win);
            if let Some(i) = touched {
                self.ambr_line(&line, i);
            }
            // age the per-file windows by one line
            win.subject = win.subject.and_then(|(i, n)| (n > 1).then_some((i, n - 1)));
            win.early_qci = win.early_qci.and_then(|(q, n)| (n > 1).then_some((q, n - 1)));
        }
    }

    fn note_connection(&mut self, line: &str) {
        let Some(addr) = connected_addr(line) else {
            return;
        };
        let now = self.line_no;
        self.out.connected.push((now, addr));
        match self.unpaired_bearer.take() {
            Some((i, at)) if now - at <= PAIRING_DISTANCE => {
                self.out.bearers[i].addr = Some(addr);
                self.unpaired_conn = None;
            }
            _ => self.unpaired_conn = Some((now, addr)),
        }
    }

    /// Records a data bearer line; returns the bearer's index.
    fn bearer_line(&mut self, line: &str, live: bool, win: &mut FileWindow) -> Option<usize> {
        let names_access = line.contains("dnn=") || line.contains("access_point=");
        if !names_access || NON_DATA.iter().any(|tag| line.contains(tag)) {
            return None;
        }
        let core = line_core(line);
        let plmn = (leading_digits(line, "mcc"), leading_digits(line, "mnc"));
        let apn = access_name(line);
        let now = self.line_no;
        let bearers = &mut self.out.bearers;
        let i = match bearers
            .iter()
            .position(|b| b.core == core && b.plmn == plmn && b.apn == apn)
        {
            Some(i) => i,
            None => {
                bearers.push(Bearer { core, plmn, apn, in_live_log: live, last_seen: now, ..Bearer::default() });
                bearers.len() - 1
            }
        };
        // Pair with a CONNECTED line seen shortly before that no earlier
        // bearer took; otherwise wait for one shortly after.
        let conn = self.unpaired_conn.take();
        let b = &mut bearers[i];
        b.in_live_log |= live;
        b.last_seen = now;
        b.addr = conn.and_then(|(at, a)| (now - at <= PAIRING_DISTANCE).then_some(a));
        self.unpaired_bearer = if b.addr.is_none() { Some((i, now)) } else { None };
        if let Some((q, _)) = win.early_qci.take() {
            b.qci = Some(q);
        }
        win.subject = Some((i, 4));
        Some(i)
    }

    fn qci_line(&mut self, line: &str, win: &mut FileWindow) {
        if !line.contains("qci") {
            return;
        }
        let Some(q) = first_int_after(line, "qci") else {
            return;
        };
        if let Some((i, _)) = win.subject {
            self.out.bearers[i].qci = Some(q);
            return;
        }
        if line.contains("default bearer qci") {
            win.early_qci = Some((q, 4));
        }
        self.out.bare_qci.get_or_insert(q);
    }

    fn ambr_line(&mut self, line: &str, i: usize) {
        let (down, up) = if line.contains("session_ambr") {
            (
                session_ambr(line, "session_ambr_dl=", "session_ambr_dl_unit="),
                session_ambr(line, "session_ambr_ul=", "session_ambr_ul_unit="),
            )
        } else if line.contains("apn_ambr") {
            (apn_ambr(line, "apn_ambr_dl"), apn_ambr(line, "apn_ambr_ul"))
        } else {
            return;
        };
        let b = &mut self.out.bearers[i];
        b.down = down;
        b.up = up;
    }
}

fn collect(texts: &[(&str, bool)]) -> Parsed {
    let mut scanner = Scanner::default();
    for &(text, live) in texts {
        scanner.file(text, live);
    }
    scanner.out
}

pub fn select(parsed: &Parsed, query: Query) -> Values {
    // Ranking, highest first: seen in the live log; not naming a foreign
    // PLMN; tied to the current address on the current core; most recent.
    //
    // NR PDU sessions carry no PLMN, so a PLMN match alone must not lift an
    // old LTE bearer above a newer 5G session — only a bearer that names a
    // PLMN which is neither serving nor home is pushed down (abroad, a
    // home-routed APN legitimately names the home network).
    // The address tie-break only counts on the current core: after LTE → SA
    // the address stays, and the newer PDU session line must still win.
    let rank = |b: &Bearer| {
        let foreign = match (query.serving, b.plmn) {
            (Some(serving), (Some(mcc), Some(mnc))) => {
                (mcc, mnc) != serving && Some((mcc, mnc)) != query.home
            }
            _ => false,
        };
        let on_our_addr = query.addr.is_some() && b.addr == query.addr && b.core == query.core;
        let weight = if b.in_live_log { 1000 } else { 0 }
            + if foreign { 0 } else { 100 }
            + if on_our_addr { 10 } else { 0 };
        (weight, b.last_seen)
    };
    let mut ranked: Vec<&Bearer> = parsed.bearers.iter().collect();
    ranked.sort_by_key(|b| rank(b));
    let any = !ranked.is_empty();
    let mut pick = ranked.last().map(|b| (*b).clone()).unwrap_or_default();
    // Gaps are filled only from the same core and APN/DNN, best-ranked first:
    // an LTE AMBR never stands in for an NR session, nor one DNN for another.
    for older in ranked.iter().rev().filter(|b| b.core == pick.core && b.apn == pick.apn) {
        pick.qci = pick.qci.or(older.qci);
        pick.down = pick.down.or(older.down);
        pick.up = pick.up.or(older.up);
    }
    // The bare `qci = …` comes from LTE lines; never use it as an NR 5QI.
    let bare = parsed.bare_qci.filter(|_| !any || pick.core == Core::Eps);
    let live_qci = query.qci_now.filter(|q| (1..=255).contains(q));
    // Core mismatch makes the values history, with one exception: an NR
    // session whose CONNECTED line names the current address stays valid
    // while the firmware reports EPS/ENDC (CMHK roaming 2026-10-20: hours of
    // ENDC with no new bearer line). An EPS bearer seen while on SA stays
    // stale: on B31 that move logged a fresh PDU session with other values.
    let core_moved = query.core != Core::Unknown
        && pick.core != Core::Unknown
        && pick.core != query.core;
    let nr_on_our_addr = pick.core == Core::Nr && query.addr.is_some() && pick.addr == query.addr;
    let stale = any
        && ((core_moved && !nr_on_our_addr) || other_connection(parsed, &pick, query.addr));
    Values {
        qci: live_qci.or(pick.qci).or(bare).unwrap_or(0),
        ambr_dl: pick.down.map(format_mbps).unwrap_or_default(),
        ambr_ul: pick.up.map(format_mbps).unwrap_or_default(),
        bearer: if any { pick.core.label() } else { "" },
        stale,
    }
}

/// The bearer is not the current connection's, by the `CONNECTED … addr=`
/// lines: it sits next to another address's, or the logs read have CONNECTED lines but none for the current address (it rolled
/// out of the window). With the address or every CONNECTED line unknown,
/// nothing is concluded here.
fn other_connection(parsed: &Parsed, bearer: &Bearer, addr: Option<Ipv4Addr>) -> bool {
    let Some(addr) = addr else {
        return false;
    };
    if parsed.connected.is_empty() {
        return false;
    }
    if bearer.addr.is_some_and(|a| a != addr) {
        return true;
    }
    // A CONNECTED line for the current address after the bearer is not taken as a new
    // connection: whether the vendor logs CONNECTED again without a bearer line is not
    // known, and a new connection logs its own bearer line anyway.
    parsed.connected.iter().all(|&(_, a)| a != addr)
}

/// PDU-session vocabulary (`pdu_session_id`, `session_ambr`, or `dnn=`
/// without `access_point=`) means 5GC; anything else is an EPS bearer.
fn line_core(lower: &str) -> Core {
    let nr = ["pdu_session_id", "session_ambr"].iter().any(|t| lower.contains(t))
        || (lower.contains("dnn=") && !lower.contains("access_point="));
    if nr { Core::Nr } else { Core::Eps }
}

/// Up to six digits right after `tag` (no skipping), e.g. `mcc460`.
fn leading_digits(s: &str, tag: &str) -> Option<i64> {
    let rest = after(s, tag)?;
    let n = rest.bytes().take(6).take_while(u8::is_ascii_digit).count();
    if n == 0 { None } else { rest[..n].parse().ok() }
}

/// First run of digits anywhere after `tag` (skipping `=`, `[`, spaces, …).
fn first_int_after(s: &str, tag: &str) -> Option<i64> {
    let rest = after(s, tag)?;
    let start = rest.find(|c: char| c.is_ascii_digit())?;
    let tail = &rest[start..];
    let len = tail.find(|c: char| !c.is_ascii_digit()).unwrap_or(tail.len());
    tail[..len].parse().ok()
}

/// Decimal number (digits and dots) right after `key`.
fn decimal_after(s: &str, key: &str) -> Option<f64> {
    let rest = after(s, key)?;
    let len = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(rest.len());
    rest[..len].parse().ok()
}

/// APN-AMBR of an EPS bearer, in Mbps (TS 24.301 §9.9.4.2).
///
/// The IE has three layers: the base octet (up to 8640 kbps), an extended
/// octet that supersedes the base when present, and an extended-2 octet
/// worth N × 256 Mbps added on top of the extended value. The vendor log
/// prints them as `<key>=<kbps>`, `<key>_ext=<Mbps>` and `<key>_ext2=<Mbps>`,
/// where `_ext2` is *cumulative*: base + extended + N × 256 (500 Mbps shows
/// as `508.640Mbps`; on B27 the U60 Pro printed `1668.640`/`1008.640`), and
/// an absent octet prints as `0.000`.
///
/// So with a non-zero `_ext2` above base + extended, the surplus is the
/// 256-Mbps part. When the surplus is (within 0.01) a whole number of steps
/// the true value is the extended value (or base if none) plus those steps —
/// this also absorbs the log's rounding (`20008.641Mbps` = 32 + 78 × 256).
/// Otherwise only the base is removed. Without a usable `_ext2`, the
/// extended value wins over the base.
/// (The cumulative-print reading first came from upstream 1ffd943 / 75247fc.)
fn apn_ambr(s: &str, key: &str) -> Option<f64> {
    let nonzero = |v: Option<f64>| v.filter(|x| *x > 0.0);
    let base_mbps = decimal_after(s, &format!("{key}=")).map(|kbps| kbps / 1000.0);
    let extended = nonzero(decimal_after(s, &format!("{key}_ext=")));
    let Some(printed) = nonzero(decimal_after(s, &format!("{key}_ext2="))) else {
        return extended.or(base_mbps);
    };
    let ext_part = extended.unwrap_or(0.0);
    let Some(base) = base_mbps.filter(|b| printed > b + ext_part) else {
        return Some(printed);
    };
    let steps = (printed - base - ext_part) / 256.0;
    let n = steps.round();
    if n >= 1.0 && (steps - n).abs() < 0.01 {
        Some(extended.unwrap_or(base) + n * 256.0)
    } else {
        Some(printed - base)
    }
}

/// Session-AMBR of an NR PDU session, in Mbps. The unit follows `unit_key`
/// either as a bare TS 24.501 code (`6`) or as a code with its meaning in
/// parentheses (`6(1Mbps)`, or just `(100Mbps)`); the parenthesised form wins.
fn session_ambr(s: &str, value_key: &str, unit_key: &str) -> Option<f64> {
    let amount = decimal_after(s, value_key)?;
    let unit = after(s, unit_key)?;
    let code_len = unit.bytes().take_while(u8::is_ascii_digit).count();
    let Some(inner) = unit[code_len..].strip_prefix('(') else {
        let code: u32 = unit[..code_len].parse().ok()?;
        return Some(amount * unit_code_mbps(code)?);
    };
    let num_len = inner
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(inner.len());
    let factor: f64 = inner[..num_len].parse().ok()?;
    let suffix = inner[num_len..].to_ascii_lowercase();
    // "gbps", "mbps" and "kbps" all contain "bps", so the bare one goes last.
    type ToMbps = fn(f64) -> f64;
    let to_mbps: [(&str, ToMbps); 4] = [
        ("gbps", |f| f * 1000.0),
        ("mbps", |f| f),
        ("kbps", |f| f / 1000.0),
        ("bps", |f| f / 1_000_000.0),
    ];
    let (_, convert) = to_mbps.iter().find(|(unit, _)| suffix.contains(unit))?;
    Some(amount * convert(factor))
}

/// Session-AMBR unit code (TS 24.501 §9.11.4.14, Table 9.11.4.14.1) in Mbps:
/// 1 = 1 Kbps, 2 = 4 Kbps, … each step ×4, 6 = 1 Mbps, 11 = 1 Gbps, 16 = 1 Tbps,
/// 21 = 1 Pbps, up to 25 = 256 Pbps. 0 (not used) and anything above: none.
/// Matches the B31 log's `1(1Kbps)` and `6(1Mbps)`.
fn unit_code_mbps(code: u32) -> Option<f64> {
    if code == 0 || code > 25 {
        return None;
    }
    let (decade, within) = ((code - 1) / 5, (code - 1) % 5);
    let kbps = 4f64.powi(within as i32) * 1000f64.powi(decade as i32);
    Some(kbps / 1000.0)
}

fn format_mbps(v: f64) -> String {
    format!("{v:.3}")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_helpers() {
        // a QCI after free text and padding
        assert_eq!(first_int_after("default bearer qci = 9", "qci"), Some(9));
        // a lone base octet in kbps comes out in Mbps
        let base_only = apn_ambr("apn_ambr_dl=64000", "apn_ambr_dl");
        assert_eq!(base_only, Some(64.0));
        // a parenthesised unit multiplies the amount
        let line = "session_ambr_dl=2 session_ambr_dl_unit=(100Mbps)";
        let dl = session_ambr(line, "session_ambr_dl=", "session_ambr_dl_unit=");
        assert_eq!(dl, Some(200.0));
        // the bracketed "external" shape, kbps → Mbps
        let ext = "[DATA] cid1, QCI=[9], DL_AMBR=[150000]kbps, UL_AMBR=[75000]kbps";
        let want = Values {
            qci: 9,
            ambr_dl: "150.000".into(),
            ambr_ul: "75.000".into(),
            ..Values::default()
        };
        assert_eq!(parse_external(ext), Some(want));
    }

    fn mbps(line: &str, key: &str) -> Option<String> {
        apn_ambr(&line.to_ascii_lowercase(), key).map(format_mbps)
    }

    // Line shapes follow upstream zwrt-datad's TopFlow / CMHK samples
    // (1ffd943, 75247fc); no raw MU5250 key.log line is in the repo yet.
    // 1668.640 / 1008.640 is the output this parser used to show on the
    // user's U60 Pro (MU5250) B27 (manager docs/designs/slow-diagnosis.md §3);
    // only those outputs were observed, the input lines below are reconstructed.
    #[test]
    fn apn_ambr_follows_ts_24_301() {
        let k = "apn_ambr_dl";
        // 100 Mbps: extended octet only, ext2 absent (printed 0.000).
        assert_eq!(
            mbps(
                "apn_ambr_dl=8640kbps apn_ambr_dl_ext=100.000Mbps apn_ambr_dl_ext2=0.000Mbps",
                k
            ),
            Some("100.000".into())
        );
        // 500 Mbps = 244 + 1 x 256, logged with the 8.64 base on top.
        assert_eq!(
            mbps(
                "apn_ambr_dl=8640kbps apn_ambr_dl_ext=244.000Mbps apn_ambr_dl_ext2=508.640Mbps",
                k
            ),
            Some("500.000".into())
        );
        // 2 Gbps = 208 + 7 x 256.
        assert_eq!(
            mbps(
                "apn_ambr_dl=8640kbps apn_ambr_dl_ext=208.000Mbps apn_ambr_dl_ext2=2008.640Mbps",
                k
            ),
            Some("2000.000".into())
        );
        // 20 Gbps = 32 + 78 x 256, vendor sum 0.001 off.
        assert_eq!(
            mbps(
                "apn_ambr_dl=8640kbps apn_ambr_dl_ext=32.000Mbps apn_ambr_dl_ext2=20008.641Mbps",
                k
            ),
            Some("20000.000".into())
        );
        // Rounding artifact below the step as well.
        assert_eq!(
            mbps(
                "apn_ambr_dl=8640kbps apn_ambr_dl_ext=32.000Mbps apn_ambr_dl_ext2=20008.639Mbps",
                k
            ),
            Some("20000.000".into())
        );
        // Off by more than 0.01 step: not snapped, only the base comes out.
        assert_eq!(
            mbps(
                "apn_ambr_dl=8640kbps apn_ambr_dl_ext=32.000Mbps apn_ambr_dl_ext2=300.640Mbps",
                k
            ),
            Some("292.000".into())
        );
        // MU5250 B27 as shown before the fix; the extended octet was not
        // kept, so either way the base comes back out.
        assert_eq!(
            mbps("apn_ambr_dl=8640kbps apn_ambr_dl_ext2=1668.640Mbps", k),
            Some("1660.000".into())
        );
        assert_eq!(
            mbps(
                "apn_ambr_ul=8640kbps apn_ambr_ul_ext=232.000Mbps apn_ambr_ul_ext2=1008.640Mbps",
                "apn_ambr_ul"
            ),
            Some("1000.000".into())
        );
        // ext2 = 0 and no ext: the base value in kbps.
        assert_eq!(
            mbps("apn_ambr_dl=64000kbps apn_ambr_dl_ext2=0.000Mbps", k),
            Some("64.000".into())
        );
        assert_eq!(mbps("apn_ambr_ul=1 apn_ambr_ul_ext=2", k), None);
    }

    const LTE_HOME: &str = "[DATA] eps_bearer_id=5 msg_type=193 access_point=cmnet.MNC000.MCC460.GPRS apn_ambr_dl=8640kbps apn_ambr_ul=8640kbps apn_ambr_dl_ext=244.000Mbps apn_ambr_ul_ext=100.000Mbps apn_ambr_dl_ext2=508.640Mbps apn_ambr_ul_ext2=0.000Mbps
[DATA] qci = 8 8
";
    const NR_SESSION: &str = "[DATA] pdu_session_id=1 msg_type=194 dnn=IMS session_ambr_dl=30000 session_ambr_dl_unit=1(1Kbps) session_ambr_ul=30000 session_ambr_ul_unit=1(1Kbps)
[DATA] pdu_session_id=2 msg_type=194 dnn=cmnet session_ambr_dl=2000 session_ambr_dl_unit=6(1Mbps) session_ambr_ul=200 session_ambr_ul_unit=6(1Mbps)
[DATA] qci = 6 6
";

    fn q(serving: (i64, i64), home: (i64, i64), core: Core) -> Query {
        Query {
            serving: Some(serving),
            home: Some(home),
            core,
            addr: None,
            qci_now: None,
        }
    }

    fn at(query: Query, addr: &str) -> Query {
        Query {
            addr: Some(addr.parse().unwrap()),
            ..query
        }
    }

    // Real B31 key.log lines (b31-community-2026-10-10.md §1, China Telecom 460-11): the
    // bearer lines' starts and numbers are as read on the device; their tails after "…" in the
    // notes, the timestamps and the exact shape of the CONNECTED line are reconstructed.
    const B31_LTE: &str = "[DATA] cid1 LTE default bearer qci = 9
[DATA] eps_bearer_id=5 msg_type=193 access_point=ctiot.MNC011.MCC460.GPRS apn_ambr_dl=8640kbps apn_ambr_ul=8640kbps apn_ambr_dl_ext=124.000Mbps apn_ambr_ul_ext=232.000Mbps apn_ambr_dl_ext2=1668.640Mbps apn_ambr_ul_ext2=1008.640Mbps
[DATA] cid1 CONNECTED ip_type=ipv4 addr=10.208.147.50
";
    const B31_SA: &str = "[DATA] pdu_session_id=1 msg_type=194 dnn=ims session_ambr_dl=100 session_ambr_dl_unit=6(1Mbps) session_ambr_ul=100 session_ambr_ul_unit=6(1Mbps)
[DATA] pdu_session_id=2 msg_type=194 dnn=ctnet session_ambr_dl=2000 session_ambr_dl_unit=6(1Mbps) session_ambr_ul=1000 session_ambr_ul_unit=6(1Mbps)
[DATA] qci = 9 9
";
    const CT: (i64, i64) = (460, 11);

    #[test]
    fn b31_samples() {
        // 10-09 on the device: SA now, the address still the one the 4G bearer came up with,
        // no PDU session line since: the 4G values, marked stale (the core moved)
        let sa_now = at(q(CT, CT, Core::Nr), "10.208.147.50");
        let v = parse_texts(&[(B31_LTE, true)], sa_now);
        assert_eq!(got(&v), (9, "1660.000", "1000.000", "eps", true));
        // on LTE with that address: current
        let lte_now = at(q(CT, CT, Core::Eps), "10.208.147.50");
        let v = parse_texts(&[(B31_LTE, true)], lte_now);
        assert_eq!(got(&v), (9, "1660.000", "1000.000", "eps", false));
        // the SA session (IMS beside it is not the data one), logged after the 4G connection
        let log = format!("{B31_LTE}{B31_SA}");
        let v = parse_texts(&[(&log, true)], sa_now);
        assert_eq!(got(&v), (9, "2000.000", "1000.000", "nr_pdu", false));
    }

    #[test]
    fn firmware_qos_validator_requires_complete_current_core_and_connection() {
        // B31_* are the module's existing reconstructed examples, not new live evidence.
        let eps = collect(&[(B31_LTE, true)]);
        let query = at(q(CT, CT, Core::Eps), "10.208.147.50");
        let (id, value) = validated_read(&eps, query).unwrap();
        assert_eq!(id, "qos.eps.log.read");
        assert_eq!(value["qci"], 9);
        assert_eq!(value["ambr_dl"], "1660.000");
        assert!(
            validated_read(
                &eps,
                Query {
                    addr: None,
                    ..query
                }
            )
            .is_none()
        );
        assert!(
            validated_read(
                &eps,
                Query {
                    core: Core::Nr,
                    ..query
                }
            )
            .is_none()
        );
        assert!(
            validated_read(
                &eps,
                Query {
                    addr: Some("10.0.0.1".parse().unwrap()),
                    ..query
                }
            )
            .is_none()
        );
        assert!(validated_read(&collect(&[(B31_LTE, false)]), query).is_none());
        assert!(
            validated_read(
                &Parsed::default(),
                Query {
                    qci_now: Some(9),
                    ..query
                }
            )
            .is_none()
        );
        let nr_text = format!("{B31_SA}[DATA] cid1 CONNECTED addr=10.208.147.50\n");
        let nr = collect(&[(&nr_text, true)]);
        assert_eq!(
            validated_read(
                &nr,
                Query {
                    core: Core::Nr,
                    ..query
                }
            )
            .unwrap()
            .0,
            "qos.nr_pdu.log.read"
        );
    }

    #[test]
    fn firmware_qos_observations_keep_original_cache_age_and_unknown_context() {
        use luyaz_api::{clock::BootTime, firmware::*};
        let mut parsed = collect(&[(B31_LTE, true)]);
        parsed.observed_at = Some(BootTime {
            boot_id: "synthetic".into(),
            seconds: 1,
        });
        let c = crate::device_profile::collect::Collector::new();
        let query = at(q(CT, CT, Core::Eps), "10.208.147.50");
        c.qos(c.token(), &parsed, query);
        let before = c.report(None);
        c.qos(c.token(), &parsed, query);
        let after = c.report(None);
        let e = after
            .evidence
            .iter()
            .find(|e| e.capability_id == "qos.eps.log.read")
            .unwrap();
        assert_eq!(e.time.observed_at, parsed.observed_at);
        assert_eq!(e.time.observed_at, before.evidence[0].time.observed_at);
        assert_eq!(e.result.as_ref().unwrap().proof, Some(Proof::ReadValidated));
        assert!(
            after
                .assessments
                .iter()
                .all(|a| a.status == Support::Unknown)
        );
        let encoded = serde_json::to_string(e).unwrap();
        assert!(!encoded.contains("10.208.147.50"));
        assert!(!encoded.contains("ctnet"));
        after.validate().unwrap();
    }

    #[test]
    fn a_bearer_of_another_connection_is_stale() {
        let lte = q(CT, CT, Core::Eps);
        // the address changed (new connection, its bearer line not logged)
        let v = parse_texts(&[(B31_LTE, true)], at(lte, "10.1.2.3"));
        assert_eq!(got(&v), (9, "1660.000", "1000.000", "eps", true));
        // the same address logged CONNECTED again later, no bearer line: still current
        let again = format!(
            "{B31_LTE}[DATA] x\n[DATA] x\n[DATA] x\n[DATA] x\n[DATA] x\n[DATA] cid1 CONNECTED addr=10.208.147.50\n"
        );
        assert!(!parse_texts(&[(&again, true)], at(lte, "10.208.147.50")).stale);
        // address unknown, or no CONNECTED line in the logs at all: not concluded
        assert!(!parse_texts(&[(B31_LTE, true)], lte).stale);
        assert!(!parse_texts(&[(LTE_HOME, true)], at(q((460, 0), (460, 0), Core::Eps), "10.1.2.3")).stale);
        // DISCONNECTED is not a connection
        let gone = format!("{B31_LTE}[DATA] cid1 DISCONNECTED addr=10.208.147.50\n");
        assert!(!parse_texts(&[(&gone, true)], at(lte, "10.208.147.50")).stale);
    }

    #[test]
    fn the_current_connection_rolled_out_of_the_window_is_stale() {
        // only an older connection's lines are left in the part read
        let old = B31_LTE.replace("10.208.147.50", "10.208.1.1");
        let v = parse_texts(&[(&old, false), ("", true)], at(q(CT, CT, Core::Eps), "10.208.147.50"));
        assert!(v.stale);
        assert_eq!(v.ambr_dl, "1660.000");
    }

    #[test]
    fn rotation_just_happened() {
        // the bearer and its CONNECTED line in key.log.0, key.log new and empty
        let lte = at(q(CT, CT, Core::Eps), "10.208.147.50");
        let v = parse_texts(&[(B31_LTE, false), ("", true)], lte);
        assert_eq!(got(&v), (9, "1660.000", "1000.000", "eps", false));
        // the CONNECTED line made it into key.log, the bearer is in key.log.0
        let (bearer, connected) = B31_LTE.split_at(B31_LTE.find("[DATA] cid1 CONNECTED").unwrap());
        let v = parse_texts(&[(bearer, false), (connected, true)], lte);
        assert_eq!(got(&v), (9, "1660.000", "1000.000", "eps", false));
    }

    #[test]
    fn several_data_dnns_the_current_connections_wins() {
        let sa = q(CT, CT, Core::Nr);
        let two = "[DATA] pdu_session_id=2 msg_type=194 dnn=ctnet session_ambr_dl=2000 session_ambr_dl_unit=6(1Mbps) session_ambr_ul=1000 session_ambr_ul_unit=6(1Mbps)
[DATA] qci = 9 9
[DATA] cid1 CONNECTED addr=10.208.147.50
[DATA] pdu_session_id=3 msg_type=194 dnn=ctwap session_ambr_dl=10 session_ambr_dl_unit=6(1Mbps) session_ambr_ul=5 session_ambr_ul_unit=6(1Mbps)
[DATA] qci = 8 8
[DATA] cid2 CONNECTED addr=10.99.0.7
";
        let v = parse_texts(&[(two, true)], at(sa, "10.208.147.50"));
        assert_eq!(got(&v), (9, "2000.000", "1000.000", "nr_pdu", false));
        let v = parse_texts(&[(two, true)], at(sa, "10.99.0.7"));
        assert_eq!(got(&v), (8, "10.000", "5.000", "nr_pdu", false));
        // address unknown: the newest, as before
        assert_eq!(parse_texts(&[(two, true)], sa).ambr_dl, "10.000");
        // one DNN's AMBR never fills in for another's
        let bare = "[DATA] pdu_session_id=2 dnn=ctnet session_ambr_dl=2000 session_ambr_dl_unit=6(1Mbps)\n[DATA] pdu_session_id=3 dnn=ctwap\n";
        assert_eq!(parse_texts(&[(bare, true)], sa).ambr_dl, "");
    }

    #[test]
    fn a_5qi_line_too_far_after_the_session_is_not_its() {
        let late = "[DATA] pdu_session_id=2 dnn=ctnet session_ambr_dl=2000 session_ambr_dl_unit=6(1Mbps)
[DATA] a\n[DATA] b\n[DATA] c\n[DATA] d
[DATA] qci = 9 9
";
        let v = parse_texts(&[(late, true)], q(CT, CT, Core::Nr));
        // the bare qci is an LTE line, never an NR 5QI: unknown rather than a guess
        assert_eq!((v.qci, v.ambr_dl.as_str()), (0, "2000.000"));
    }

    #[test]
    fn the_vendor_qci_now_wins_over_the_logs() {
        let two = "[DATA] pdu_session_id=2 dnn=ctnet session_ambr_dl=2000 session_ambr_dl_unit=6(1Mbps)
[DATA] qci = 9 9
";
        let sa = q(CT, CT, Core::Nr);
        let now = |n| Query { qci_now: Some(n), ..sa };
        let v = parse_texts(&[(two, true)], now(6));
        assert_eq!(got(&v), (6, "2000.000", "", "nr_pdu", false));
        // out of range or not read: the logs' value
        assert_eq!(parse_texts(&[(two, true)], now(0)).qci, 9);
        assert_eq!(parse_texts(&[(two, true)], sa).qci, 9);
        // no bearer line at all: still the value now, with no label
        let v = parse_texts(&[("", true)], now(9));
        assert_eq!((v.qci, v.bearer), (9, ""));
    }

    #[test]
    fn session_ambr_units() {
        let dl = |unit: &str| {
            session_ambr(
                &format!("session_ambr_dl=3 session_ambr_dl_unit={unit}"),
                "session_ambr_dl=",
                "session_ambr_dl_unit=",
            )
            .map(format_mbps)
        };
        // the code alone (TS 24.501 Table 9.11.4.14.1)
        assert_eq!(dl("6").as_deref(), Some("3.000"));
        assert_eq!(dl("1").as_deref(), Some("0.003"));
        assert_eq!(dl("3").as_deref(), Some("0.048"));
        assert_eq!(dl("7").as_deref(), Some("12.000"));
        assert_eq!(dl("11").as_deref(), Some("3000.000"));
        assert_eq!(dl("12").as_deref(), Some("12000.000"));
        assert_eq!(dl("0"), None);
        assert_eq!(dl("26"), None);
        // with its name
        assert_eq!(dl("11(1Gbps)").as_deref(), Some("3000.000"));
        assert_eq!(dl("1(1Kbps)").as_deref(), Some("0.003"));
        // a kbps data DNN
        let v = parse_texts(
            &[("[DATA] pdu_session_id=2 dnn=ctnet session_ambr_dl=30000 session_ambr_dl_unit=1(1Kbps) session_ambr_ul=20000 session_ambr_ul_unit=1\n", true)],
            Query::default(),
        );
        assert_eq!((v.ambr_dl.as_str(), v.ambr_ul.as_str()), ("30.000", "20.000"));
    }

    fn got(v: &Values) -> (i64, &str, &str, &str, bool) {
        (
            v.qci,
            v.ambr_dl.as_str(),
            v.ambr_ul.as_str(),
            v.bearer,
            v.stale,
        )
    }

    #[test]
    fn lte_to_nr_newer_pdu_session_wins() {
        let log = format!("{LTE_HOME}{NR_SESSION}");
        let v = parse_texts(&[(&log, true)], q((460, 0), (460, 0), Core::Nr));
        assert_eq!(got(&v), (6, "2000.000", "200.000", "nr_pdu", false));
    }

    #[test]
    fn nr_to_lte_newer_eps_bearer_wins() {
        let log = format!("{NR_SESSION}{LTE_HOME}");
        let v = parse_texts(&[(&log, true)], q((460, 0), (460, 0), Core::Eps));
        assert_eq!(got(&v), (8, "500.000", "100.000", "eps", false));
    }

    #[test]
    fn nr_to_lte_without_a_new_bearer_line_is_stale() {
        // Back on LTE, the PDU session maps to a PDN connection and the
        // vendor log may print nothing: keep the values, say they are old.
        let log = format!("{LTE_HOME}{NR_SESSION}");
        let v = parse_texts(&[(&log, true)], q((460, 0), (460, 0), Core::Eps));
        assert_eq!(got(&v), (6, "2000.000", "200.000", "nr_pdu", true));
    }

    #[test]
    fn sa_session_kept_after_falling_back_to_endc_is_current() {
        // CMHK roaming, 2026-10-20: the session came up on SA, then the radio
        // reported ENDC; the CONNECTED line still names the current address.
        let log = "[DATA] pdu_session_id=2 msg_type=194 dnn=3gnet session_ambr_dl=3000 session_ambr_dl_unit=6(1Mbps) session_ambr_ul=300 session_ambr_ul_unit=6(1Mbps)
[DATA] qci = 6 6
[DATA] cid1 CONNECTED connect_status=ipv4_connected, ipv4 dev=rmnet_data0, addr=10.170.63.190, gw=10.170.63.189
";
        let endc = q((454, 12), (454, 12), Core::Eps);
        let v = parse_texts(&[(log, true)], at(endc, "10.170.63.190"));
        assert_eq!(got(&v), (6, "3000.000", "300.000", "nr_pdu", false));
        // another address: still the old connection's values
        assert!(parse_texts(&[(log, true)], at(endc, "10.170.63.200")).stale);
    }

    #[test]
    fn old_lte_bearer_left_in_the_log_is_stale_on_sa() {
        let v = parse_texts(&[(LTE_HOME, true)], q((460, 0), (460, 0), Core::Nr));
        assert_eq!(got(&v), (8, "500.000", "100.000", "eps", true));
        // NSA still rides EPS bearers.
        let v = parse_texts(&[(LTE_HOME, true)], q((460, 0), (460, 0), Core::Eps));
        assert!(!v.stale);
        // Not registered: nothing to compare against.
        let v = parse_texts(&[(LTE_HOME, true)], Query::default());
        assert_eq!(got(&v), (8, "500.000", "100.000", "eps", false));
    }

    #[test]
    fn lte_ambr_never_fills_in_for_an_nr_session() {
        let log = format!(
            "{LTE_HOME}[DATA] pdu_session_id=3 msg_type=194 dnn=cmnet session_ambr_dl=300 session_ambr_dl_unit=6(1Mbps)\n"
        );
        let v = parse_texts(&[(&log, true)], q((460, 0), (460, 0), Core::Nr));
        assert_eq!((v.ambr_dl.as_str(), v.ambr_ul.as_str()), ("300.000", ""));
    }

    #[test]
    fn plmns_home_routed_roaming_and_foreign_bearers() {
        // Abroad on 454-12 with a 460-00 SIM: the home-routed APN names the
        // home PLMN and is the real bearer.
        let roam = q((454, 12), (460, 0), Core::Eps);
        let v = parse_texts(&[(LTE_HOME, true)], roam);
        assert_eq!(got(&v), (8, "500.000", "100.000", "eps", false));
        // A newer bearer naming a third network loses to the home-routed one.
        let third = format!(
            "{LTE_HOME}[DATA] eps_bearer_id=6 access_point=x.MNC001.MCC440.GPRS apn_ambr_dl=8640kbps apn_ambr_dl_ext=64.000Mbps apn_ambr_ul_ext=32.000Mbps\n"
        );
        assert_eq!(parse_texts(&[(&third, true)], roam).ambr_dl, "500.000");
        // A local-breakout APN naming the visited network is fine too.
        let local = format!(
            "{LTE_HOME}[DATA] eps_bearer_id=6 access_point=x.MNC012.MCC454.GPRS apn_ambr_dl=8640kbps apn_ambr_dl_ext=64.000Mbps apn_ambr_ul_ext=32.000Mbps\n"
        );
        assert_eq!(parse_texts(&[(&local, true)], roam).ambr_dl, "64.000");
        // After a SIM swap to a 454-12 card the old 460-00 bearer is foreign.
        let swapped = q((454, 12), (454, 12), Core::Eps);
        let mixed = format!(
            "[DATA] eps_bearer_id=5 access_point=y.MNC012.MCC454.GPRS apn_ambr_dl=8640kbps apn_ambr_dl_ext=150.000Mbps\n{LTE_HOME}"
        );
        assert_eq!(parse_texts(&[(&mixed, true)], swapped).ambr_dl, "150.000");
    }

    #[test]
    fn current_log_beats_the_rotated_one() {
        let v = parse_texts(
            &[(NR_SESSION, false), (LTE_HOME, true)],
            q((460, 0), (460, 0), Core::Eps),
        );
        assert_eq!(got(&v), (8, "500.000", "100.000", "eps", false));
    }

    #[test]
    fn malformed_and_empty_input() {
        let v = parse_texts(&[], Query::default());
        assert_eq!(v, Values::default());
        let junk = "[DATA] access_point= apn_ambr_dl=abc\n[DATA] dnn=ims session_ambr_dl=5 session_ambr_dl_unit=6(1Mbps)\nno tag qci=9\n[DATA] dnn=x session_ambr_dl=7 session_ambr_dl_unit=(furlongs)\n";
        let v = parse_texts(&[(junk, true)], q((460, 0), (460, 0), Core::Nr));
        assert_eq!((v.ambr_dl.as_str(), v.qci), ("", 0));
    }

    #[test]
    fn one_parse_serves_every_core_and_bare_qci_stays_lte() {
        let log = format!("[DATA] qci = 7\n{LTE_HOME}{NR_SESSION}");
        let parsed = collect(&[(&log, true)]);
        let home = ((460, 0), (460, 0));
        let sa = select(&parsed, q(home.0, home.1, Core::Nr));
        let nsa = select(&parsed, q(home.0, home.1, Core::Eps));
        assert_eq!(got(&sa), (6, "2000.000", "200.000", "nr_pdu", false));
        assert_eq!(got(&nsa), (6, "2000.000", "200.000", "nr_pdu", true));
        assert_eq!(parsed.bare_qci, Some(7));
        // An NR session without its own 5QI line does not borrow the bare one.
        let nr_only = "[DATA] qci = 7\n[DATA] pdu_session_id=2 dnn=cmnet session_ambr_dl=10 session_ambr_dl_unit=6(1Mbps)\n";
        assert_eq!(parse_texts(&[(nr_only, true)], Query::default()).qci, 0);
        assert_eq!(
            parse_texts(&[("[DATA] qci = 7\n", true)], Query::default()).qci,
            7
        );
    }
}
