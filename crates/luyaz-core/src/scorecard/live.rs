//! The scorecard's side inputs read on the device (u60-sentinel.md §19 T9b): what doctor's
//! `report_ledger` works out before it runs its awk, the same way. The two that are doctor's
//! health check (`warn`, `standby`) are not here: `/v2/scorecard` takes them from the request
//! body (doctor sends them) and reads 没测 without them, until the core's own health check (9d)
//! is lined up with doctor's.

use super::Inputs;
use super::awk::num;
use std::path::{Path, PathBuf};

/// Recovery's first go-live (doctor's `RECOV_LIVE`).
pub const RECOV_LIVE: f64 = 1_790_458_980.0;

/// Where the inputs are (doctor's defaults; the doctor test points each elsewhere).
#[derive(Clone, Debug)]
pub struct Paths {
    pub ledger: PathBuf,
    /// The core's long-term layer; with a `cutover` file in it (an epoch: when the guard's ledger
    /// stopped, T9d) the card is the guard's before it and the core's from it, else the core's.
    pub long: PathBuf,
    pub crashcap: PathBuf,
    pub alerts_queue: PathBuf,
    pub uid_state: PathBuf,
    pub standby_base: PathBuf,
}

impl Paths {
    pub fn host() -> Paths {
        let h = |p: &str| PathBuf::from(crate::state::host_path(p));
        Paths {
            ledger: h("/data/ledger"),
            long: h("/data/luyaz/core/long"),
            crashcap: h("/data/crashcap"),
            alerts_queue: h("/data/alerts/queue"),
            uid_state: h("/data/luyaz/screen"),
            standby_base: h("/data/luyaz/scripts/standby.baseline"),
        }
    }
}

/// The modem crash words doctor greps the kept kernel logs for (`grep -c -i -E`).
const CRASH_WORDS: [&str; 6] = [
    "fatal error received",
    "watchdog received",
    "crash detected",
    "rproc recovery",
    "subsystem restart",
    "err_fatal",
];

/// The last `n` lines of `text`, as `tail -n` (a last line without its newline counts).
fn tail(text: &[u8], n: usize) -> Vec<&[u8]> {
    let body = text.strip_suffix(b"\n").unwrap_or(text);
    if text.is_empty() {
        return Vec::new();
    }
    let mut v: Vec<&[u8]> = body.rsplitn(n + 1, |&c| c == b'\n').collect();
    if v.len() > n {
        v.pop();
    }
    v.reverse();
    v
}

/// `^[0-9][0-9]*,[0-9][0-9]*,` (a kmsg record: level, sequence).
fn kmsg_record(l: &[u8]) -> bool {
    let mut parts = l.splitn(3, |&c| c == b',');
    let mut digits = || {
        parts
            .next()
            .is_some_and(|p| !p.is_empty() && p.iter().all(u8::is_ascii_digit))
    };
    digits() && digits() && l.splitn(3, |&c| c == b',').count() == 3
}

/// Does a kept kernel log end in a modem crash line: 1 or 0 (doctor: `tail -n 40`, the last
/// kmsg record, `grep -c -i -E` the crash words).
pub fn cap_end(text: &[u8]) -> u8 {
    let last = tail(text, 40).into_iter().rev().find(|l| kmsg_record(l));
    let Some(l) = last else { return 0 };
    let l = String::from_utf8_lossy(l).to_ascii_lowercase();
    u8::from(CRASH_WORDS.iter().any(|w| l.contains(w)))
}

/// The kept kernel logs read: doctor's `cap` file (`<boot8> <0|1>` a line).
pub fn cap(dir: &Path) -> String {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("kmsg-") && n.ends_with(".log") && n.len() >= 9)
        .collect();
    names.sort();
    let mut out = String::new();
    for n in names {
        let p = dir.join(&n);
        if !p.is_file() {
            continue;
        }
        // the logs are a few hundred KB at most (crashcap keeps them small); the end is what counts
        let text = read_end(&p, 1 << 20);
        out.push_str(&format!("{} {}\n", &n[5..n.len() - 4], cap_end(&text)));
    }
    out
}

/// A file's last `max` bytes (all of it when smaller).
fn read_end(p: &Path, max: u64) -> Vec<u8> {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(p) else {
        return Vec::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len > max {
        let _ = f.seek(SeekFrom::Start(len - max));
    }
    let mut v = Vec::new();
    let _ = f.read_to_end(&mut v);
    v
}

/// awk's `print` of a number: an integer as one, else `%.6g` (busybox).
pub fn print_num(x: f64) -> String {
    if x == x.trunc() && x.abs() < 1e15 {
        return format!("{}", x as i64);
    }
    if x == 0.0 || !x.is_finite() {
        return format!("{x}");
    }
    let e = x.abs().log10().floor() as i32;
    let trim = |s: String| {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s
        }
    };
    if !(-4..6).contains(&e) {
        let s = format!("{:.5e}", x);
        let (m, ex) = s.split_once('e').unwrap_or((&s, "0"));
        let ex: i32 = ex.parse().unwrap_or(0);
        format!(
            "{}e{}{:02}",
            trim(m.to_string()),
            if ex < 0 { '-' } else { '+' },
            ex.abs()
        )
    } else {
        trim(format!("{:.*}", (5 - e).max(0) as usize, x))
    }
}

/// The standby idle threshold: doctor's
/// `awk '$1 == "2" && $2 != "-" { print $2 + 3 * $3; exit }' standby.baseline`, or "".
pub fn thr(text: &str) -> String {
    for l in text.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        let g = |i: usize| f.get(i).copied().unwrap_or("");
        if g(0) == "2" && g(1) != "-" {
            return print_num(num(g(1)) + 3.0 * num(g(2)));
        }
    }
    String::new()
}

/// Everything but the health check, for a window of `w` s.
pub fn inputs(paths: &Paths, w: f64, summary: bool) -> Inputs {
    Inputs {
        w,
        label: if w == 86_400.0 { "24 小时" } else { "7 天" }.into(),
        recov: Some(RECOV_LIVE),
        cap: cap(&paths.crashcap),
        warn: None,
        standby: None,
        alerts: std::fs::read_to_string(&paths.alerts_queue).unwrap_or_default(),
        gaveup: paths.uid_state.join("gave-up").exists(),
        thr: thr(&std::fs::read_to_string(&paths.standby_base).unwrap_or_default()),
        summary,
    }
}

/// doctor's request body: the first line is standby_check's output, the rest the health rows
/// that are not ok (both may be empty).
pub fn health(inp: &mut Inputs, body: &str) {
    let (stb, warn) = body.split_once('\n').unwrap_or((body, ""));
    inp.standby = Some(if stb.is_empty() {
        String::new()
    } else {
        format!("{stb}\n")
    });
    inp.warn = Some(warn.to_string());
}

/// The scorecard of the device now: text, then a last line `rc=<n>` (busybox wget sees no
/// status code, so the exit code travels in the body).
pub fn card(paths: &Paths, w: f64, summary: bool, body: Option<&str>) -> String {
    let mut inp = inputs(paths, w, summary);
    if let Some(b) = body {
        health(&mut inp, b);
    }
    let cut = std::fs::read_to_string(paths.long.join("cutover"))
        .ok()
        .and_then(|c| c.trim().parse::<f64>().ok());
    // the guard's ledger stopped (10-19) and newer devices never had one: without a cut the card
    // is the core's long-term layer alone
    let (text, rc) = match cut {
        Some(c) => super::replay::stitch(&paths.ledger, &paths.long, c, &inp),
        None => super::replay::core(&paths.long, &inp),
    };
    format!("{text}rc={rc}\n")
}
