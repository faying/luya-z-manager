// ─────────────────────────────────────────────────────────────────────────────
// health — the device check, for the admin web's health page and the touch
// screen's summary.
//
// The checks themselves live in one shell script, touch-ui scripts/doctor.sh
// (installed at /data/u60-guard/doctor.sh), so that `install.sh doctor` over
// SSH and this page can never disagree — and so the check still works when
// this agent is the thing that is broken. Here we only run it on a timer,
// cache the result, and list the crash logs supervise.sh / u60-uid keep.
//
// Unauthenticated `/api/public/status` gets counts only; the details (and the
// crash logs, which contain program output) need a login.
// ─────────────────────────────────────────────────────────────────────────────

use std::fs;
use std::io::Read;
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::handlers::AppState;

const DOCTOR: &str = "/data/u60-guard/doctor.sh";
const CRASH_DIR: &str = "/data/crashlog";
const EVERY: Duration = Duration::from_secs(60);
const DOCTOR_TIMEOUT: Duration = Duration::from_secs(20);
const CRASHLOG_MAX: u64 = 256 * 1024;

#[derive(Clone, Debug, PartialEq)]
struct Check {
    level: String, // ok | warn | bad
    id: String,
    label: String,
    detail: String,
    /// English twins from doctor `--tsv2`; empty when the doctor is too old
    /// to give them (sent as null, so clients fall back to the Chinese).
    label_en: String,
    detail_en: String,
}

struct Snapshot {
    checks: Vec<Check>,
    at_device: i64,      // device clock (local wall time labelled UTC; see clock.rs)
    at: Option<Instant>, // None = never ran
    error: Option<String>,
}

static LAST: Mutex<Snapshot> = Mutex::new(Snapshot { checks: Vec::new(), at_device: 0, at: None, error: None });

fn parse(tsv: &str) -> Vec<Check> {
    tsv.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.splitn(4, '\t').collect();
            if f.len() != 4 || !matches!(f[0], "ok" | "warn" | "bad") {
                return None;
            }
            Some(Check {
                level: f[0].into(),
                id: f[1].into(),
                label: f[2].into(),
                detail: f[3].into(),
                label_en: String::new(),
                detail_en: String::new(),
            })
        })
        .collect()
}

/// First line of `doctor.sh --tsv2`. An older doctor does not know the flag
/// and prints its human-readable report instead (L2 review R2), so without
/// this line the output is not ours to parse.
const TSV2_MARKER: &str = "#tsv2";

/// True when the output starts with the `--tsv2` marker line.
fn has_tsv2_marker(out: &str) -> bool {
    out.lines().next().map(|l| l.trim_end_matches('\r')) == Some(TSV2_MARKER)
}

/// `doctor.sh --tsv2`: after the marker line, six tab-separated columns
/// `level id label detail label_en detail_en`. A Chinese detail may itself
/// contain a tab (see the 4-column test), so the first three and the last two
/// fields are fixed and whatever lies between is the detail.
fn parse_tsv2(tsv: &str) -> Vec<Check> {
    tsv.lines()
        .skip(1)
        .filter_map(|l| {
            let l = l.trim_end_matches('\r');
            let f: Vec<&str> = l.split('\t').collect();
            if f.len() < 6 || !matches!(f[0], "ok" | "warn" | "bad") {
                return None;
            }
            let n = f.len();
            Some(Check {
                level: f[0].into(),
                id: f[1].into(),
                label: f[2].into(),
                detail: f[3..n - 2].join("\t"),
                label_en: f[n - 2].into(),
                detail_en: f[n - 1].into(),
            })
        })
        .collect()
}

/// Run the doctor the new way, falling back to the old one: `--tsv2` first;
/// if its first line is not the marker (an older doctor.sh), run `--tsv` and
/// leave the English empty. A timeout or a missing script is an error either
/// way — never read as "old doctor", so it cannot cost a second 20 s run.
fn run_checks(doctor: &str) -> Result<Vec<Check>, String> {
    let out = run_doctor(doctor, "--tsv2")?;
    if has_tsv2_marker(&out) {
        return Ok(parse_tsv2(&out));
    }
    Ok(parse(&run_doctor(doctor, "--tsv")?))
}

/// Run doctor.sh with a deadline. Its exit status is "no bad checks", not
/// success/failure of the run itself, so only the output matters.
///
/// The doctor is a script that starts its own children (ubus, wget, …): on a
/// timeout the whole process group goes, not just the `sh` (a hung grandchild
/// used to stay behind). stdout is read while it runs, so a long report can't
/// fill the pipe and stall the doctor until the timeout.
fn run_doctor(doctor: &str, flag: &str) -> Result<String, String> {
    run_doctor_within(doctor, flag, DOCTOR_TIMEOUT)
}

fn run_doctor_within(doctor: &str, flag: &str, limit: Duration) -> Result<String, String> {
    if !std::path::Path::new(doctor).exists() {
        return Err(format!("{doctor} is not installed"));
    }
    match crate::ubus::output_within_group(Command::new("sh").args([doctor, flag]), limit) {
        Ok(out) => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Err("doctor timed out".into()),
        Err(e) => Err(format!("run doctor: {e}")),
    }
}

fn now_device() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn refresh() {
    let result = run_checks(DOCTOR);
    let mut s = LAST.lock().unwrap_or_else(|e| e.into_inner());
    match result {
        Ok(checks) => {
            s.checks = checks;
            s.error = None;
        }
        Err(e) => s.error = Some(e),
    }
    s.at = Some(Instant::now());
    s.at_device = now_device();
}

/// Re-run the check now, in the background, after something that changes its
/// answer (alerts marked read, SMS number set). Without this the touch
/// screen's 健康 row kept saying 注意 for up to a minute after 全部已读.
/// One extra run at a time; a second request while one is running is dropped.
pub fn refresh_soon() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static BUSY: AtomicBool = AtomicBool::new(false);
    if BUSY.swap(true, Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("health-now".into())
        .spawn(|| {
            refresh();
            BUSY.store(false, Ordering::Release);
        });
    if spawned.is_err() {
        BUSY.store(false, Ordering::Release);
    }
}

/// Background refresher. The first run waits a little so boot is not slowed.
pub fn start() {
    std::thread::Builder::new()
        .name("health".into())
        .spawn(|| {
            std::thread::sleep(Duration::from_secs(20));
            loop {
                refresh();
                std::thread::sleep(EVERY);
            }
        })
        .ok();
}

fn counts(checks: &[Check]) -> (usize, usize) {
    let bad = checks.iter().filter(|c| c.level == "bad").count();
    let warn = checks.iter().filter(|c| c.level == "warn").count();
    (bad, warn)
}

/// For `/api/public/status`: counts only.
pub fn public_summary() -> Value {
    let s = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let (bad, warn) = counts(&s.checks);
    json!({ "checked": s.at.is_some() && s.error.is_none(), "bad": bad, "warn": warn })
}

/// Name safe to use as one path component: no separators, no dot-dot.
fn safe_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 96
        && !n.starts_with('.')
        && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

fn crash_logs() -> Vec<Value> {
    let mut out = Vec::new();
    let Ok(progs) = fs::read_dir(CRASH_DIR) else { return out };
    for p in progs.flatten() {
        let prog = p.file_name().to_string_lossy().to_string();
        if !safe_name(&prog) {
            continue;
        }
        let Ok(files) = fs::read_dir(p.path()) else { continue };
        for f in files.flatten() {
            let name = f.file_name().to_string_lossy().to_string();
            if !name.ends_with(".log") || !safe_name(&name) {
                continue;
            }
            let meta = f.metadata().ok();
            let mtime = meta
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            // The status line, so the list says what happened without opening it.
            let status = fs::read_to_string(f.path())
                .ok()
                .and_then(|t| t.lines().find_map(|l| l.strip_prefix("status:").map(|s| s.trim().to_string())))
                .unwrap_or_default();
            out.push(json!({
                "program": prog, "file": name, "time": mtime,
                "size": meta.map(|m| m.len()).unwrap_or(0), "status": status,
            }));
        }
    }
    out.sort_by(|a, b| b["time"].as_i64().cmp(&a["time"].as_i64()));
    out
}

/// One check for `/api/health`. The English twins are null, not "", when the
/// doctor did not give them: clients write `label_en ?? label`.
fn check_json(c: &Check) -> Value {
    let en = |s: &str| (!s.is_empty()).then(|| s.to_string());
    json!({
        "level": c.level, "id": c.id, "label": c.label, "detail": c.detail,
        "label_en": en(&c.label_en), "detail_en": en(&c.detail_en),
    })
}

/// GET /api/health[?refresh=1]
pub fn health_get(_state: &AppState, query: &str) -> (u16, Value) {
    if query.split('&').any(|kv| kv == "refresh=1") {
        refresh();
    }
    let s = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let (bad, warn) = counts(&s.checks);
    let checks: Vec<Value> = s
        .checks
        .iter()
        .map(check_json)
        .collect();
    (
        200,
        json!({"ok": true, "data": {
            "checks": checks,
            "bad": bad,
            "warn": warn,
            "checked_at": if s.at.is_some() { Some(s.at_device) } else { None },
            "error": s.error,
            "crashlogs": crash_logs(),
        }}),
    )
}

/// GET /api/health/crashlog?program=<p>&file=<f>
pub fn crashlog_get(_state: &AppState, query: &str) -> (u16, Value) {
    let get = |k: &str| {
        query.split('&').find_map(|kv| kv.strip_prefix(k).and_then(|v| v.strip_prefix('=')).map(str::to_string))
    };
    let (Some(prog), Some(file)) = (get("program"), get("file")) else {
        return (400, json!({"ok": false, "error": "program and file are required"}));
    };
    if !safe_name(&prog) || !safe_name(&file) || !file.ends_with(".log") {
        return (400, json!({"ok": false, "error": "bad name"}));
    }
    let path = format!("{CRASH_DIR}/{prog}/{file}");
    let Ok(f) = fs::File::open(&path) else {
        return (404, json!({"ok": false, "error": "no such crash log"}));
    };
    let mut text = String::new();
    let _ = f.take(CRASHLOG_MAX).read_to_string(&mut text);
    (200, json!({"ok": true, "data": {"program": prog, "file": file, "text": text}}))
}

/// Any CJK ideograph, kana, CJK punctuation or fullwidth form: what an
/// `*_en` field must not contain (netinfo checks its errors with it; tests
/// across the crate use it).
pub(crate) fn has_cjk(s: &str) -> bool {
    s.chars().any(|c| {
        matches!(c as u32,
            0x2E80..=0x2FFF | 0x3000..=0x303F | 0x3040..=0x30FF | 0x3100..=0x9FFF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_doctor_tsv_and_skips_junk() {
        let c = parse("ok\tfota\tZTE 自动升级\t已关闭\nbad\tscreen\t触屏界面\t没在运行\nnonsense\nwarn\tx\ty\tdetail\twith tab\n");
        assert_eq!(c.len(), 3);
        assert_eq!(c[1].level, "bad");
        assert_eq!(c[2].detail, "detail\twith tab");
        assert_eq!(counts(&c), (1, 1));
    }

    #[test]
    fn the_manifest_row_is_an_ordinary_row() {
        // doctor.sh --tsv ends with the device manifest's verdict (touch-ui docs/SHIP.md)
        let tsv = "ok\tstandby\t待机\t正常\n\
                   warn\tmanifest\t清单\t不一致的是 zwrt-datad（清单 6bbf6ea6，实际 af3c8ad8）\n";
        let c = parse(tsv);
        assert_eq!(c.len(), 2);
        assert_eq!(c[1].id, "manifest");
        assert_eq!(c[1].label, "清单");
        assert_eq!(c[1].detail, "不一致的是 zwrt-datad（清单 6bbf6ea6，实际 af3c8ad8）");
        assert_eq!(counts(&c), (0, 1));
        let ok = parse("ok\tmanifest\t清单\t观察中（还剩 42 分钟）：datad 刚上机；一致（12 项）\n");
        assert_eq!(counts(&ok), (0, 0));
    }

    // Built from the R2 spec (docs/designs/ui-english.md): marker line, then
    // `level id label detail label_en detail_en`.
    const TSV2: &str = "#tsv2\n\
        ok\tfota\tZTE 自动升级\t已关闭\tZTE auto-update\tOff\n\
        bad\tscreen\t触屏界面\t没在运行\tScreen UI\tNot running\n\
        nonsense\n\
        warn\tx\ty\tdetail\twith tab\tY\tDetail\n\
        ok\tshort\ta\tb\tc\n";

    #[test]
    fn parses_doctor_tsv2_with_english() {
        assert!(has_tsv2_marker(TSV2));
        let c = parse_tsv2(TSV2);
        assert_eq!(c.len(), 3, "5-column and junk rows are skipped");
        assert_eq!((c[0].label.as_str(), c[0].detail.as_str()), ("ZTE 自动升级", "已关闭"));
        assert_eq!((c[0].label_en.as_str(), c[0].detail_en.as_str()), ("ZTE auto-update", "Off"));
        assert_eq!(c[2].detail, "detail\twith tab");
        assert_eq!((c[2].label_en.as_str(), c[2].detail_en.as_str()), ("Y", "Detail"));
        assert_eq!(counts(&c), (1, 1));
        for x in &c {
            assert!(!has_cjk(&x.label_en) && !has_cjk(&x.detail_en), "{x:?}");
        }
        let j = check_json(&c[1]);
        assert_eq!(j["label_en"], "Screen UI");
        assert_eq!(j["label"], "触屏界面");
    }

    /// touch-ui's sample of real `doctor.sh --tsv2` output, when that checkout
    /// sits next to this one (it does in the workspace, not in the public repo).
    #[test]
    fn parses_touch_ui_tsv2_sample() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../touch-ui/scripts/test/doctor/tsv2-sample.tsv");
        let Ok(text) = fs::read_to_string(path) else {
            eprintln!("skipped: no {path}");
            return;
        };
        assert!(has_tsv2_marker(&text));
        let rows = text.lines().skip(1).filter(|l| !l.trim().is_empty()).count();
        let c = parse_tsv2(&text);
        assert_eq!(c.len(), rows, "every row of the sample parses");
        for x in &c {
            assert!(!x.label_en.is_empty() && !x.detail_en.is_empty(), "{x:?}");
            assert!(!has_cjk(&x.label_en) && !has_cjk(&x.detail_en), "{x:?}");
        }
        let (bad, warn) = counts(&c);
        assert_eq!(bad + warn + c.iter().filter(|x| x.level == "ok").count(), rows);
    }

    #[test]
    fn marker_must_be_the_exact_first_line() {
        assert!(has_tsv2_marker("#tsv2\r\nok\ta\tb\tc\td\te\n"));
        assert!(!has_tsv2_marker("ok\tfota\tZTE 自动升级\t已关闭\n#tsv2\n"));
        assert!(!has_tsv2_marker("#tsv2x\n"));
        assert!(!has_tsv2_marker(" #tsv2\n"));
        assert!(!has_tsv2_marker(""));
    }

    #[test]
    fn old_doctor_rows_have_no_english_and_send_null() {
        let c = parse("ok\tfota\tZTE 自动升级\t已关闭\n");
        let j = check_json(&c[0]);
        assert!(j["label_en"].is_null() && j["detail_en"].is_null());
        assert_eq!(j["detail"], "已关闭");
    }

    fn script(name: &str, body: &str) -> String {
        let p = std::env::temp_dir().join(format!("u60-doctor-{name}-{}.sh", std::process::id()));
        fs::write(&p, body).unwrap();
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn old_doctor_falls_back_to_tsv_in_chinese() {
        // What doctor.sh did before --tsv2: only `--tsv` is special, any other
        // argument gets the human-readable report (R2).
        let d = script(
            "old",
            "if [ \"$1\" = \"--tsv\" ]; then printf 'ok\\tfota\\tZTE 自动升级\\t已关闭\\nwarn\\tsms\\t告警短信\\t没设号码\\n'; \
             else echo '== U60 体检 =='; echo '  [正常] ZTE 自动升级：已关闭'; fi\n",
        );
        let c = run_checks(&d).unwrap();
        assert_eq!(c.len(), 2, "health page must not go empty with an old doctor");
        assert_eq!(c[1].label, "告警短信");
        assert!(c.iter().all(|x| x.label_en.is_empty() && x.detail_en.is_empty()));
        assert_eq!(counts(&c), (0, 1));
        let _ = fs::remove_file(&d);
    }

    #[test]
    fn new_doctor_is_read_once_with_english() {
        let d = script(
            "new",
            "if [ \"$1\" = \"--tsv2\" ]; then printf '#tsv2\\nok\\tfota\\tZTE 自动升级\\t已关闭\\tZTE auto-update\\tOff\\n'; \
             else printf 'bad\\twrong\\tread --tsv\\tx\\n'; fi\n",
        );
        let c = run_checks(&d).unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!((c[0].id.as_str(), c[0].label_en.as_str()), ("fota", "ZTE auto-update"));
        let _ = fs::remove_file(&d);
    }

    /// The touch screen reads /api/health into 32 KB (touch-ui alerts.c
    /// AL_RESP_MAX). Doctor has ~38 report rows; 40 long ones in both
    /// languages must leave a quarter for the crash-log list after them.
    #[test]
    fn forty_long_rows_fit_the_screen_buffer() {
        let mut tsv = String::from("#tsv2\n");
        for i in 0..40 {
            tsv.push_str(&format!(
                "warn\tid{i}\t{}\t{}\t{}\t{}\n",
                "标".repeat(10),
                "详".repeat(60),
                "L".repeat(24),
                "D".repeat(120)
            ));
        }
        let checks: Vec<Value> = parse_tsv2(&tsv).iter().map(check_json).collect();
        let size = serde_json::to_string(&checks).unwrap().len();
        eprintln!("health checks: {size} bytes");
        assert_eq!(checks.len(), 40);
        assert!(size < 24 * 1024, "{size} bytes");
    }

    /// Well past a 64 KB pipe buffer, then exit at once: the old code only
    /// read stdout after the exit, so this doctor sat blocked until the
    /// timeout and the run failed.
    #[test]
    fn a_long_report_does_not_stall_the_doctor() {
        let d = script("flood", "i=0; while [ $i -lt 4000 ]; do printf 'ok\\tid%s\\t%080d\\tx\\n' $i 0; i=$((i+1)); done\n");
        let t0 = Instant::now();
        let out = run_doctor_within(&d, "--tsv", Duration::from_secs(20)).unwrap();
        assert!(out.len() > 300_000, "{} bytes", out.len());
        assert_eq!(parse(&out).len(), 4000);
        assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
        let _ = fs::remove_file(&d);
    }

    /// A doctor stuck on a child of its own: the timeout takes the child too.
    #[test]
    fn a_timeout_kills_the_doctors_children() {
        let pidfile = std::env::temp_dir().join(format!("u60-doctor-child-{}.pid", std::process::id()));
        let _ = fs::remove_file(&pidfile);
        let d = script("hang", &format!("sleep 30 & echo $! > {}\nwait\n", pidfile.display()));
        let t0 = Instant::now();
        assert_eq!(run_doctor_within(&d, "--tsv2", Duration::from_millis(500)).unwrap_err(), "doctor timed out");
        assert!(t0.elapsed() < Duration::from_secs(5), "{:?}", t0.elapsed());
        let pid = fs::read_to_string(&pidfile).unwrap().trim().to_string();
        // Gone, or a zombie nobody reaps (a container's PID 1 may not):
        // either way no longer running.
        let dead = || match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => true,
            Ok(st) => st.rsplit(')').next().map(|r| r.trim_start().starts_with('Z')).unwrap_or(false),
        };
        let t1 = Instant::now();
        while !dead() && t1.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(dead(), "sleep {pid} outlived the doctor");
        let _ = fs::remove_file(&d);
        let _ = fs::remove_file(&pidfile);
    }

    #[test]
    fn missing_doctor_is_an_error_not_an_empty_list() {
        assert!(run_checks("/nonexistent/doctor.sh").is_err());
    }

    #[test]
    fn names_cannot_escape_the_crash_dir() {
        assert!(safe_name("zte-agent"));
        assert!(safe_name("20260923-150424-up2325.log"));
        assert!(!safe_name(".."));
        assert!(!safe_name("../etc"));
        assert!(!safe_name("a/b"));
        assert!(!safe_name(".hidden"));
        assert!(!safe_name(""));
    }
}
