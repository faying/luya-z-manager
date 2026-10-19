use serde::Serialize;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    net::Ipv4Addr,
    path::Path,
    sync::Arc,
};

/// The tail of each log read. key.log grows about 1 MB a day (B31, b31-community §1), so a
/// connection older than about two days has its lines rolled out of it; then the current
/// address's `CONNECTED` line is not found and the values are marked `stale` instead of an
/// older bearer passing for the current one.
const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;

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
    fn name(self) -> &'static str {
        match self {
            Core::Unknown => "",
            Core::Eps => "eps",
            Core::Nr => "nr_pdu",
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
#[derive(Clone, Default, Debug, PartialEq, Serialize)]
pub struct Values {
    /// QCI of an EPS bearer, or the 5QI of an NR PDU session (`bearer`
    /// says which). 0 = not known.
    pub qci: i64,
    /// Mbps, three decimals; empty when not known.
    pub ambr_dl: String,
    pub ambr_ul: String,
    /// `eps`, `nr_pdu`, or empty when no bearer line was found.
    pub bearer: &'static str,
    /// The values are history, not the current limit: the bearer found is of
    /// the other core than the one the device is on now (e.g. an LTE bearer
    /// after moving to SA with no PDU session line logged yet), or it belongs
    /// to another connection than the current address's, or the current
    /// address's connection is no longer in the part of the logs read.
    pub stale: bool,
}

#[derive(Clone, Default, Debug)]
struct Candidate {
    current: bool,
    core: Core,
    mcc: Option<i64>,
    mnc: Option<i64>,
    /// the APN / DNN name (lower case, up to the first '.')
    name: String,
    seq: usize,
    qci: Option<i64>,
    dl: Option<f64>,
    ul: Option<f64>,
    /// the address of a `CONNECTED … addr=` line next to its latest bearer line
    addr: Option<Ipv4Addr>,
}

/// How many `[DATA]` lines apart a bearer line and its `CONNECTED` line may be.
const NEAR: usize = 4;

// Reading the tails of key.log and key.log.0 means scanning up to 4 MiB of
// text; on a U60 Pro (MU5250) that ran every second. QCI/AMBR only change on
// a bearer event, so keep the parsed bearers for 30 s. Picking one for the
// current PLMN/core is cheap and runs every time, so flapping between SA and
// NSA does not rescan the logs.
// LUYAZ_CACHE=0 turns this off, as for the state cache.
const REUSE_FOR: std::time::Duration = std::time::Duration::from_secs(30);
static LAST: std::sync::Mutex<Option<(std::time::Instant, Arc<Parsed>)>> =
    std::sync::Mutex::new(None);

pub fn invalidate() {
    if let Ok(mut l) = LAST.lock() {
        *l = None;
    }
}

pub fn read(query: Query) -> Values {
    select(&logs(), query)
}

/// The parsed bearer lines of both logs (reused for 30 s). Picking one for a
/// query is [`select`], a pure function.
pub fn logs() -> Arc<Parsed> {
    let caching = std::env::var("LUYAZ_CACHE").as_deref() != Ok("0");
    if caching
        && let Ok(l) = LAST.lock()
        && let Some((at, parsed)) = l.as_ref()
        && at.elapsed() < REUSE_FOR
    {
        return parsed.clone();
    }
    let mut parsed = read_logs();
    parsed.observed_at = luyaz_api::clock::BootTime::read();
    let parsed = Arc::new(parsed);
    if caching && let Ok(mut l) = LAST.lock() {
        *l = Some((std::time::Instant::now(), parsed.clone()));
    }
    parsed
}

fn read_logs() -> Parsed {
    let current =
        std::env::var("LUYAZ_KEY_LOG").unwrap_or_else(|_| "/data/logfs/key.log".into());
    let rotated = std::env::var("LUYAZ_KEY_LOG_ROTATED")
        .unwrap_or_else(|_| "/data/logfs/key.log.0".into());
    let texts: Vec<(String, bool)> = [(rotated.as_str(), false), (current.as_str(), true)]
        .into_iter()
        .filter_map(|(path, current)| read_tail(Path::new(path)).map(|t| (t, current)))
        .collect();
    let texts: Vec<(&str, bool)> = texts.iter().map(|(t, c)| (t.as_str(), *c)).collect();
    collect(&texts)
}

pub fn parse_external(text: &str) -> Option<Values> {
    let mut selected = None;
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        if !line.contains("[DATA]") || !lower.contains("cid1") {
            continue;
        }
        let qci = integer_after_ci(&lower, "qci=")?;
        let dl = integer_after_ci(&lower, "dl_ambr=")?;
        let ul = integer_after_ci(&lower, "ul_ambr=")?;
        if !(1..=255).contains(&qci) || dl < 0 || ul < 0 {
            continue;
        }
        selected = Some(Values {
            qci,
            ambr_dl: format_mbps(dl as f64 / 1000.0),
            ambr_ul: format_mbps(ul as f64 / 1000.0),
            ..Default::default()
        });
    }
    selected
}

fn read_tail(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let start = size.saturating_sub(MAX_LOG_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::with_capacity((size - start) as usize);
    file.take(MAX_LOG_BYTES).read_to_end(&mut bytes).ok()?;
    if start > 0
        && let Some(pos) = bytes.iter().position(|b| *b == b'\n')
    {
        bytes.drain(..=pos);
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Every data bearer found in the logs, oldest first, plus a bare `qci = …`
/// seen outside any bearer context.
#[derive(Clone, Default, Debug)]
pub struct Parsed {
    /// Original physical log-read time. Reusing the parsed cache never renews this.
    pub(crate) observed_at: Option<luyaz_api::clock::BootTime>,
    candidates: Vec<Candidate>,
    fallback_qci: Option<i64>,
    /// every `CONNECTED … addr=` line: its place and IPv4 address
    connections: Vec<(usize, Ipv4Addr)>,
}

/// Stronger than the display fallback: a complete bearer in the current log, of the
/// current core and connection. Bare UCI QCI, rotated/history and partial AMBR never prove it.
pub(crate) fn validated_read(
    parsed: &Parsed,
    query: Query,
) -> Option<(&'static str, serde_json::Value)> {
    let addr = query.addr?;
    let candidate = parsed.candidates.iter().rev().find(|c| {
        c.current
            && c.core == query.core
            && c.core != Core::Unknown
            && c.addr == Some(addr)
            && (c.core == Core::Nr
                || matches!((c.mcc, c.mnc), (Some(m), Some(n))
                if Some((m, n)) == query.serving || Some((m, n)) == query.home))
    })?;
    let qci = candidate.qci.filter(|q| (1..=255).contains(q))?;
    let dl = candidate.dl.filter(|n| n.is_finite() && *n >= 0.0)?;
    let ul = candidate.ul.filter(|n| n.is_finite() && *n >= 0.0)?;
    Some((
        if candidate.core == Core::Eps {
            "qos.eps.log.read"
        } else {
            "qos.nr_pdu.log.read"
        },
        serde_json::json!({"qci":qci,"ambr_dl":format_mbps(dl),"ambr_ul":format_mbps(ul),"bearer":candidate.core.name()}),
    ))
}

/// The address of a `cidN CONNECTED … addr=a.b.c.d` line (not DISCONNECTED).
fn connected_addr(lower: &str) -> Option<Ipv4Addr> {
    if !lower.split(|c: char| !c.is_ascii_alphanumeric()).any(|w| w == "connected") {
        return None;
    }
    let p = lower.find("addr=")? + "addr=".len();
    let token: String = lower[p..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    token.parse().ok()
}

/// The APN / DNN a bearer line names, lower case, up to the first '.'.
fn access_name(lower: &str) -> String {
    ["dnn=", "access_point="]
        .iter()
        .find_map(|k| lower.find(k).map(|p| &lower[p + k.len()..]))
        .map(|s| {
            s.chars()
                .take_while(|c| !c.is_whitespace() && *c != '.' && *c != ',')
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
fn parse_texts(texts: &[(&str, bool)], query: Query) -> Values {
    select(&collect(texts), query)
}

fn collect(texts: &[(&str, bool)]) -> Parsed {
    let mut candidates = Vec::<Candidate>::new();
    let mut fallback = Candidate::default();
    let mut seq = 0usize;
    let mut connections = Vec::<(usize, Ipv4Addr)>::new();
    // the latest bearer line still waiting for a CONNECTED line after it
    let mut awaiting: Option<(usize, usize)> = None;
    // the latest CONNECTED line no bearer line before it took
    let mut unclaimed: Option<(usize, Ipv4Addr)> = None;
    for (text, current) in texts {
        let mut context: Option<usize> = None;
        let mut context_left: u8 = 0;
        let mut pending_qci = None;
        let mut pending_left: u8 = 0;
        for line in text.lines().filter(|line| line.contains("[DATA]")) {
            seq += 1;
            let lower = line.to_ascii_lowercase();
            let data_context = !["dnn=ims", "dnn=sos", "dnn=emergency", "access_point=ims"]
                .iter()
                .any(|v| lower.contains(v))
                && (lower.contains("dnn=") || lower.contains("access_point="));
            let mut line_candidate = None;
            if let Some(addr) = connected_addr(&lower) {
                connections.push((seq, addr));
                if let Some((idx, _)) = awaiting.take().filter(|(_, at)| seq - at <= NEAR) {
                    candidates[idx].addr = Some(addr);
                    unclaimed = None;
                } else {
                    unclaimed = Some((seq, addr));
                }
            }
            if data_context {
                let cmcc = digits_after_ci(&lower, "mcc");
                let cmnc = digits_after_ci(&lower, "mnc");
                let core = line_core(&lower);
                let name = access_name(&lower);
                let idx = candidates
                    .iter()
                    .position(|c| {
                        c.core == core && c.mcc == cmcc && c.mnc == cmnc && c.name == name
                    })
                    .unwrap_or_else(|| {
                        candidates.push(Candidate {
                            current: *current,
                            core,
                            mcc: cmcc,
                            mnc: cmnc,
                            name,
                            seq,
                            ..Default::default()
                        });
                        candidates.len() - 1
                    });
                let c = &mut candidates[idx];
                c.current |= *current;
                c.seq = seq;
                // its connection: a CONNECTED line just after it, or one just before that the
                // bearer line before it did not take
                c.addr = unclaimed
                    .take()
                    .filter(|(at, _)| seq - at <= NEAR)
                    .map(|(_, a)| a);
                awaiting = c.addr.is_none().then_some((idx, seq));
                if let Some(qci) = pending_qci.take() {
                    c.qci = Some(qci);
                }
                context = Some(idx);
                context_left = 4;
                pending_left = 0;
                line_candidate = Some(idx);
            }
            if lower.contains("qci")
                && let Some(qci) = integer_after_ci(&lower, "qci")
            {
                if let Some(idx) = context.filter(|_| context_left > 0) {
                    candidates[idx].qci = Some(qci);
                } else if lower.contains("default bearer qci") {
                    pending_qci = Some(qci);
                    pending_left = 4;
                    fallback.qci.get_or_insert(qci);
                } else {
                    fallback.qci.get_or_insert(qci);
                }
            }
            if let Some(idx) = line_candidate {
                if lower.contains("session_ambr") {
                    candidates[idx].dl =
                        session_ambr(&lower, "session_ambr_dl=", "session_ambr_dl_unit=");
                    candidates[idx].ul =
                        session_ambr(&lower, "session_ambr_ul=", "session_ambr_ul_unit=");
                } else if lower.contains("apn_ambr") {
                    candidates[idx].dl = apn_ambr(&lower, "apn_ambr_dl");
                    candidates[idx].ul = apn_ambr(&lower, "apn_ambr_ul");
                }
            }
            context_left = context_left.saturating_sub(1);
            if context_left == 0 {
                context = None;
            }
            pending_left = pending_left.saturating_sub(1);
            if pending_left == 0 {
                pending_qci = None;
            }
        }
    }
    Parsed {
        observed_at: None,
        candidates,
        fallback_qci: fallback.qci,
        connections,
    }
}

pub fn select(parsed: &Parsed, query: Query) -> Values {
    let mut candidates = parsed.candidates.clone();
    // The newest data bearer in the newest log wins. An EPS bearer names its
    // PLMN and an NR PDU session does not, so a PLMN match must not let an old
    // LTE bearer outrank a later 5G session; only a bearer naming a PLMN that
    // is neither the serving nor the SIM's home network loses.
    // (Idea from upstream zwrt-datad 1ffd943 / v0.10.65; the home-PLMN
    // exception is ours: abroad, a home-routed APN names the home network.)
    let foreign = |c: &Candidate| match (query.serving, c.mcc, c.mnc) {
        (Some(serving), Some(m), Some(n)) => {
            Some((m, n)) != Some(serving) && Some((m, n)) != query.home
        }
        _ => false,
    };
    // Among bearers of the core the device is on, the one whose CONNECTED line
    // names the current address is the current connection's (several data
    // DNNs at once). Only of that core: after a move LTE → SA the connection
    // keeps its address, and a later PDU session line must still win.
    let ours = |c: &Candidate| {
        query.addr.is_some() && c.addr == query.addr && c.core == query.core
    };
    let score = |c: &Candidate| {
        i32::from(c.current) * 1000 + if foreign(c) { 0 } else { 100 } + i32::from(ours(c)) * 10
    };
    candidates.sort_by_key(|c| (score(c), c.seq));
    let mut selected = candidates.last().cloned().unwrap_or_default();
    // Missing fields come only from older bearers of the same core and APN/DNN:
    // an LTE AMBR must never fill in for an NR session, nor one DNN's for another's.
    for c in candidates
        .iter()
        .rev()
        .filter(|c| c.core == selected.core && c.name == selected.name)
    {
        if selected.qci.is_none() {
            selected.qci = c.qci;
        }
        if selected.dl.is_none() {
            selected.dl = c.dl;
        }
        if selected.ul.is_none() {
            selected.ul = c.ul;
        }
    }
    let found = !candidates.is_empty();
    // A bare `qci = …` is an LTE line; it never stands in for an NR 5QI.
    let fallback_qci = parsed
        .fallback_qci
        .filter(|_| !found || selected.core == Core::Eps);
    Values {
        qci: query
            .qci_now
            .filter(|q| (1..=255).contains(q))
            .or(selected.qci)
            .or(fallback_qci)
            .unwrap_or(0),
        ambr_dl: selected.dl.map(format_mbps).unwrap_or_default(),
        ambr_ul: selected.ul.map(format_mbps).unwrap_or_default(),
        bearer: if found { selected.core.name() } else { "" },
        stale: found
            && ((query.core != Core::Unknown
                && selected.core != Core::Unknown
                && selected.core != query.core)
                || other_connection(parsed, &selected, query.addr)),
    }
}

/// The bearer is not the current connection's, by the `CONNECTED … addr=`
/// lines: it sits next to another address's, or the logs read have CONNECTED lines but none for the current address (it rolled
/// out of the window). With the address or every CONNECTED line unknown,
/// nothing is concluded here.
fn other_connection(parsed: &Parsed, bearer: &Candidate, addr: Option<Ipv4Addr>) -> bool {
    let Some(addr) = addr else {
        return false;
    };
    if parsed.connections.is_empty() {
        return false;
    }
    if bearer.addr.is_some_and(|a| a != addr) {
        return true;
    }
    // A CONNECTED line for the current address after the bearer is not taken as a new
    // connection: whether the vendor logs CONNECTED again without a bearer line is not
    // known, and a new connection logs its own bearer line anyway.
    !parsed.connections.iter().any(|(_, a)| *a == addr)
}

/// EPS bearer lines carry `access_point=`/`eps_bearer_id`/`apn_ambr`; NR PDU
/// session lines carry `pdu_session_id`/`session_ambr`/`dnn=`.
fn line_core(lower: &str) -> Core {
    if lower.contains("pdu_session_id")
        || lower.contains("session_ambr")
        || (lower.contains("dnn=") && !lower.contains("access_point="))
    {
        Core::Nr
    } else {
        Core::Eps
    }
}

fn digits_after_ci(s: &str, tag: &str) -> Option<i64> {
    let p = s.find(tag)? + tag.len();
    let digits: String = s[p..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .take(6)
        .collect();
    (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
}
fn integer_after_ci(s: &str, tag: &str) -> Option<i64> {
    let p = s.find(tag)? + tag.len();
    let tail = s[p..].trim_start_matches(|c: char| !c.is_ascii_digit());
    let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}
fn number_after(s: &str, key: &str) -> Option<f64> {
    let p = s.find(key)? + key.len();
    let token: String = s[p..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    token.parse().ok()
}
/// APN-AMBR from an EPS bearer line (TS 24.301 9.9.4.2). The extended octet
/// replaces the 8640 kbps base value, and a non-zero extended-2 octet adds
/// N x 256 Mbps on top of the extended value. The vendor log prints `_ext2`
/// with the base added as well (244 + 256 + 8.64 = `508.640Mbps` for
/// 500 Mbps; a U60 Pro (MU5250) on B27 showed `1668.640`/`1008.640`) and
/// `0.000` when the octet is absent, so the base is taken back out and a
/// zero `_ext2` falls through to `_ext`. The printed sum is not always exact
/// (`20008.641Mbps` for 32 + 78 x 256 = 20000 Mbps), so a value within 0.01
/// of a whole number of 256 Mbps steps is snapped to it.
/// (Upstream zwrt-datad 1ffd943 / v0.10.65 and 75247fc / v0.10.69.)
fn apn_ambr(s: &str, key: &str) -> Option<f64> {
    let positive = |v: f64| (v > 0.0).then_some(v);
    let base = number_after(s, &format!("{key}=")).map(|v| v / 1000.0);
    let ext = number_after(s, &format!("{key}_ext=")).and_then(positive);
    let ext2 = number_after(s, &format!("{key}_ext2=")).and_then(positive);
    match (ext2, base) {
        (Some(v), Some(b)) if v > b + ext.unwrap_or(0.0) => {
            let steps = (v - b - ext.unwrap_or(0.0)) / 256.0;
            let whole = steps.round();
            Some(if whole >= 1.0 && (steps - whole).abs() < 0.01 {
                ext.unwrap_or(b) + whole * 256.0
            } else {
                v - b
            })
        }
        (Some(v), _) => Some(v),
        _ => ext.or(base),
    }
}
fn session_ambr(s: &str, value_key: &str, unit_key: &str) -> Option<f64> {
    let value = number_after(s, value_key)?;
    let p = s.find(unit_key)? + unit_key.len();
    let unit = &s[p..];
    let code: String = unit.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !unit[code.len()..].starts_with('(') {
        // the unit code alone, no "(1Mbps)" after it
        return unit_code_mbps(code.parse().ok()?).map(|scale| value * scale);
    }
    let open = unit.find('(')? + 1;
    let scale_token: String = unit[open..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let mut scale: f64 = scale_token.parse().ok()?;
    let suffix = unit[open + scale_token.len()..].to_ascii_lowercase();
    if suffix.contains("gbps") {
        scale *= 1000.0;
    } else if suffix.contains("mbps") {
    } else if suffix.contains("kbps") {
        scale /= 1000.0;
    } else if suffix.contains("bps") {
        scale /= 1_000_000.0;
    } else {
        return None;
    }
    Some(value * scale)
}
/// Session-AMBR unit code (TS 24.501 §9.11.4.14, Table 9.11.4.14.1) in Mbps:
/// 1 = 1 Kbps, 2 = 4 Kbps, … each step ×4, 6 = 1 Mbps, 11 = 1 Gbps, 16 = 1 Tbps,
/// 21 = 1 Pbps, up to 25 = 256 Pbps. 0 (not used) and anything above: none.
/// Matches the B31 log's `1(1Kbps)` and `6(1Mbps)`.
fn unit_code_mbps(code: u32) -> Option<f64> {
    if !(1..=25).contains(&code) {
        return None;
    }
    let i = code - 1;
    let step = 4f64.powi((i % 5) as i32);
    Some(step * 1000f64.powi((i / 5) as i32) / 1000.0)
}

fn format_mbps(v: f64) -> String {
    format!("{v:.3}")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_helpers() {
        assert_eq!(integer_after_ci("default bearer qci = 9", "qci"), Some(9));
        assert_eq!(apn_ambr("apn_ambr_dl=64000", "apn_ambr_dl"), Some(64.0));
        assert_eq!(
            session_ambr(
                "session_ambr_dl=2 session_ambr_dl_unit=(100Mbps)",
                "session_ambr_dl=",
                "session_ambr_dl_unit="
            ),
            Some(200.0)
        );
        assert_eq!(
            parse_external("[DATA] cid1, QCI=[9], DL_AMBR=[150000]kbps, UL_AMBR=[75000]kbps"),
            Some(Values {
                qci: 9,
                ambr_dl: "150.000".into(),
                ambr_ul: "75.000".into(),
                ..Default::default()
            })
        );
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
        assert_eq!(parsed.fallback_qci, Some(7));
        // An NR session without its own 5QI line does not borrow the bare one.
        let nr_only = "[DATA] qci = 7\n[DATA] pdu_session_id=2 dnn=cmnet session_ambr_dl=10 session_ambr_dl_unit=6(1Mbps)\n";
        assert_eq!(parse_texts(&[(nr_only, true)], Query::default()).qci, 0);
        assert_eq!(
            parse_texts(&[("[DATA] qci = 7\n", true)], Query::default()).qci,
            7
        );
    }
}
