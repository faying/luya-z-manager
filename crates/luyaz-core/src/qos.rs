use serde::Serialize;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::Arc,
};

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
}

#[derive(Clone, Default, Debug, PartialEq, Serialize)]
pub struct Values {
    /// QCI of an EPS bearer, or the 5QI of an NR PDU session (`bearer`
    /// says which). 0 = not known.
    pub qci: i64,
    pub ambr_dl: String,
    pub ambr_ul: String,
    /// `eps`, `nr_pdu`, or empty when no bearer line was found.
    pub bearer: &'static str,
    /// The newest bearer in the log is of the other core than the one the
    /// device is on now (e.g. an LTE bearer after moving to SA with no PDU
    /// session line logged yet): the values are history, not the current limit.
    pub stale: bool,
}

#[derive(Clone, Default, Debug)]
struct Candidate {
    current: bool,
    core: Core,
    mcc: Option<i64>,
    mnc: Option<i64>,
    seq: usize,
    qci: Option<i64>,
    dl: Option<f64>,
    ul: Option<f64>,
}

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
    let parsed = Arc::new(read_logs());
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
    candidates: Vec<Candidate>,
    fallback_qci: Option<i64>,
}

#[cfg(test)]
fn parse_texts(texts: &[(&str, bool)], query: Query) -> Values {
    select(&collect(texts), query)
}

fn collect(texts: &[(&str, bool)]) -> Parsed {
    let mut candidates = Vec::<Candidate>::new();
    let mut fallback = Candidate::default();
    let mut seq = 0usize;
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
            if data_context {
                let cmcc = digits_after_ci(&lower, "mcc");
                let cmnc = digits_after_ci(&lower, "mnc");
                let core = line_core(&lower);
                let idx = candidates
                    .iter()
                    .position(|c| c.core == core && c.mcc == cmcc && c.mnc == cmnc)
                    .unwrap_or_else(|| {
                        candidates.push(Candidate {
                            current: *current,
                            core,
                            mcc: cmcc,
                            mnc: cmnc,
                            seq,
                            ..Default::default()
                        });
                        candidates.len() - 1
                    });
                let c = &mut candidates[idx];
                c.current |= *current;
                c.seq = seq;
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
        candidates,
        fallback_qci: fallback.qci,
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
    let score = |c: &Candidate| i32::from(c.current) * 1000 + if foreign(c) { 0 } else { 100 };
    candidates.sort_by_key(|c| (score(c), c.seq));
    let mut selected = candidates.last().cloned().unwrap_or_default();
    // Missing fields come only from older bearers of the same core: an LTE
    // AMBR must never fill in for an NR session.
    for c in candidates.iter().rev().filter(|c| c.core == selected.core) {
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
        qci: selected.qci.or(fallback_qci).unwrap_or(0),
        ambr_dl: selected.dl.map(format_mbps).unwrap_or_default(),
        ambr_ul: selected.ul.map(format_mbps).unwrap_or_default(),
        bearer: if found { selected.core.name() } else { "" },
        stale: found
            && query.core != Core::Unknown
            && selected.core != Core::Unknown
            && selected.core != query.core,
    }
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
        }
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
