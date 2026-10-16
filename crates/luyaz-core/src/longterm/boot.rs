//! The boot line's facts and the key.log backfill (touch-ui docs/LEDGER.md §4 `boot`,
//! `boot_backfill`, §10; u60-sentinel.md §19 T9c), as the guard's ledger task writes them.
//!
//! - `boot`: `code` = the last `reboot_reason_code=` in key.log (the firmware writes this boot's
//!   ~20 s after power-on, so it is read [`AFTER_S`] into the boot, as the guard does at ~45 s),
//!   `mode` (`zwrt_zte_mc_tmp`'s `mode_main_state`, read from the file as the guard does), `fw`
//!   (`zwrt_web device_info`'s `wa_inner_version`, else tr069's SoftwareVersion), `net_select`,
//!   the firmware's three automatic reboots as `option=value` text cut to 60 bytes, `pon`
//!   (`zwrt_bsp.pm list`'s `power_on_reason`). ubus goes through the executor.
//! - `boot_backfill`: every reason-code line between the anchor (the line last taken in: inode,
//!   line number, a hash of it and the 20 lines before) and this boot's line, id
//!   `kl-<inode>-<line>`; looked for in key.log, then key.log.0; anchor lost = key.log only,
//!   `gap=1`. The first go-live (no `init-done`) takes no history, only sets the anchor. The
//!   anchor moves (and `init-done` is written) only after the lines are written, so a rerun
//!   writes the same ids again at worst, and readers count an id once.
//!
//! - `planned`: the reboot was asked for (web, touch screen, scheduler, `<core> plan-reboot`
//!   before a shell `reboot`). On CN B31 a deliberate reboot logs the same code 1134 as a modem
//!   crash, so the code alone cannot tell them apart. The marker [`PLANNED`] holds the boot it was
//!   written in; the next boot's line takes its reason, and it goes after the line is written.
//!   A marker still in its own boot after [`PLANNED_TTL_S`] means the reboot never came: dropped.
//!
//! The core's anchor is its own (`long/keylog.anchor`), not the guard's: the two ledgers run
//! side by side in 5e.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;

pub const KEYLOG: &str = "/data/logfs/key.log";
const MC_TMP: &str = "/etc/config/zwrt_zte_mc_tmp";
pub const ANCHOR: &str = "keylog.anchor";
pub const INIT_DONE: &str = "init-done";
/// Boot seconds before the facts are read (this boot's reason code is in key.log by then).
pub const AFTER_S: u64 = 60;

/// Someone asked for the coming reboot: `<boot_id> <uptime s> <why>`.
pub const PLANNED: &str = "/data/luyaz/core/planned-reboot";
/// A marker older than this in the boot that wrote it is stale (the reboot did not happen).
pub const PLANNED_TTL_S: f64 = 600.0;

fn proc_boot_id() -> String {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn proc_uptime() -> f64 {
    std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse().ok())
        .unwrap_or(0.0)
}

/// The reason as the ledger keeps it: letters, digits and `._:-`, at most 32.
fn why_clean(why: &str) -> String {
    let w: String = why
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || "._:-".contains(*c))
        .take(32)
        .collect();
    if w.is_empty() { "unknown".into() } else { w }
}

/// What a marker means for the boot `boot` at uptime `up`.
#[derive(Debug, PartialEq)]
pub enum Planned {
    /// Written in this boot, the reboot is still coming.
    Pending,
    /// Written in an earlier boot: this boot is the planned one.
    Ours(String),
    /// Unreadable, or its reboot never came.
    Stale,
}

pub fn planned_of(text: &str, boot: &str, up: f64) -> Planned {
    let mut it = text.split_whitespace();
    let (Some(b), Some(u), w) = (
        it.next(),
        it.next().and_then(|u| u.parse::<f64>().ok()),
        it.next(),
    ) else {
        return Planned::Stale;
    };
    if b != boot {
        Planned::Ours(why_clean(w.unwrap_or("")))
    } else if up - u > PLANNED_TTL_S || up < u {
        Planned::Stale
    } else {
        Planned::Pending
    }
}

/// Mark the coming reboot as asked for (written before the reboot is sent).
pub fn plan(why: &str) -> std::io::Result<()> {
    use std::io::Write;
    let p = std::path::PathBuf::from(crate::state::host_path(PLANNED));
    let tmp = p.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        let line = format!(
            "{} {:.0} {}\n",
            proc_boot_id(),
            proc_uptime(),
            why_clean(why)
        );
        f.write_all(line.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &p)
}

/// This boot's view of the marker; a stale one is removed.
pub fn planned_now() -> Planned {
    let p = crate::state::host_path(PLANNED);
    let Ok(t) = std::fs::read_to_string(&p) else {
        return Planned::Pending;
    };
    let r = planned_of(&t, &proc_boot_id(), proc_uptime());
    if r == Planned::Stale {
        let _ = std::fs::remove_file(&p);
    }
    r
}

/// One key.log file as the backfill needs it: its inode and its reason-code lines, each with
/// the hash of it and the 20 lines before (the guard's md5 of `sed -n` of them). Read line by
/// line, so a 5–8 MB key.log never sits in the resident process's memory.
pub struct KeyFile {
    pub ino: u64,
    /// (line number from 1, code, the line's first 19 characters, hash).
    pub reasons: Vec<(usize, i64, String, String)>,
}

/// The code of a reason-code line: the last `reboot_reason_code=` with digits (the guard's
/// greedy sed).
fn reason(l: &str) -> Option<i64> {
    l.match_indices("reboot_reason_code=")
        .filter_map(|(p, _)| {
            let d: String = l[p + 19..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            d.parse::<i64>().ok()
        })
        .last()
}

impl KeyFile {
    /// From lines (each without its newline), in order.
    fn scan<I: Iterator<Item = Vec<u8>>>(ino: u64, lines: I) -> KeyFile {
        let mut win: std::collections::VecDeque<Vec<u8>> = std::collections::VecDeque::new();
        let mut reasons = Vec::new();
        for (i, raw) in lines.enumerate() {
            win.push_back(raw);
            if win.len() > 21 {
                win.pop_front();
            }
            let l = String::from_utf8_lossy(win.back().map_or(&[][..], |v| v));
            let Some(code) = reason(&l) else { continue };
            let mut h = Sha256::new();
            for w in &win {
                h.update(w);
                h.update(b"\n");
            }
            let hash: String = h
                .finalize()
                .iter()
                .take(16)
                .map(|b| format!("{b:02x}"))
                .collect();
            reasons.push((i + 1, code, l.chars().take(19).collect(), hash));
        }
        KeyFile { ino, reasons }
    }

    #[cfg(test)]
    pub fn from_text(ino: u64, text: &str) -> KeyFile {
        let mut v: Vec<&str> = text.split('\n').collect();
        if v.last() == Some(&"") {
            v.pop();
        }
        Self::scan(ino, v.into_iter().map(|l| l.as_bytes().to_vec()))
    }

    pub fn read(path: &Path) -> Option<KeyFile> {
        use std::io::BufRead;
        use std::os::unix::fs::MetadataExt;
        let f = std::fs::File::open(path).ok()?;
        let ino = f.metadata().ok()?.ino();
        let mut r = std::io::BufReader::new(f);
        let lines = std::iter::from_fn(move || {
            let mut b = Vec::new();
            match r.read_until(b'\n', &mut b) {
                Ok(0) | Err(_) => None,
                Ok(_) => {
                    if b.last() == Some(&b'\n') {
                        b.pop();
                    }
                    Some(b)
                }
            }
        });
        Some(Self::scan(ino, lines))
    }

    /// This boot's reason code (the last one).
    pub fn code(&self) -> Option<i64> {
        self.reasons.last().map(|r| r.1)
    }
}

/// What a backfill gives: the lines and where the anchor goes once they are written.
#[derive(Debug, Default, PartialEq)]
pub struct Backfill {
    pub lines: Vec<Value>,
    /// `<inode> <line> <hash>`; None = leave it.
    pub anchor: Option<String>,
}

fn take(f: &KeyFile, after: usize, before: Option<usize>, gap: u8, out: &mut Vec<Value>) {
    for (n, code, at, _) in &f.reasons {
        let (n, code) = (*n, *code);
        if n <= after || before.is_some_and(|b| n >= b) {
            continue;
        }
        out.push(json!({
            "id": format!("kl-{}-{n}", f.ino), "code": code, "at": at, "started": null, "gap": gap,
        }));
    }
}

/// The backfill (LEDGER.md §10 step 3) over key.log (`cur`) and key.log.0 (`old`, read only
/// when the anchor is not in key.log).
pub fn backfill(
    cur: Option<&KeyFile>,
    old: impl FnOnce() -> Option<KeyFile>,
    anchor: Option<&str>,
    init_done: bool,
) -> Backfill {
    let Some(cur) = cur else {
        return Backfill::default();
    };
    let Some((kcur, _, _, khash)) = cur.reasons.last() else {
        return Backfill::default();
    };
    let kcur = *kcur;
    let at = |k: &KeyFile, ino: u64, n: usize, h: &str| {
        k.ino == ino && k.reasons.iter().any(|r| r.0 == n && r.3 == h)
    };
    let mut old_file: Option<KeyFile> = None;
    let mut lines = Vec::new();
    if init_done {
        let found = anchor.and_then(|a| {
            let mut f = a.split_whitespace();
            let (ino, n, h) = (
                f.next()?.parse::<u64>().ok()?,
                f.next()?.parse().ok()?,
                f.next()?,
            );
            if at(cur, ino, n, h) {
                return Some((false, n));
            }
            old_file = old();
            old_file
                .as_ref()
                .filter(|o| at(o, ino, n, h))
                .map(|_| (true, n))
        });
        match found {
            Some((true, n)) => {
                if let Some(o) = &old_file {
                    take(o, n, None, 0, &mut lines);
                }
                take(cur, 0, Some(kcur), 0, &mut lines);
            }
            Some((false, n)) => take(cur, n, Some(kcur), 0, &mut lines),
            None => take(cur, 0, Some(kcur), 1, &mut lines),
        }
    }
    Backfill {
        lines,
        anchor: Some(format!("{} {kcur} {khash}", cur.ino)),
    }
}

/// `uci show` output as the guard's `uci_brief`: `option=value` joined by spaces (no header
/// line, no `package.section.`), cut to 60 bytes, trailing spaces off; None when empty.
pub fn brief<'a>(opts: impl Iterator<Item = (&'a str, &'a str)>) -> Option<String> {
    let s: Vec<String> = opts.map(|(o, v)| format!("{o}={v}")).collect();
    let s = s.join(" ");
    let mut cut = s.len().min(60);
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    let s = s[..cut].trim_end_matches(' ');
    (!s.is_empty()).then(|| s.to_string())
}

/// The options of one section from `uci show <package>` (path → value).
fn section<'a>(
    map: &'a std::collections::BTreeMap<String, String>,
    pkg: &str,
    sec: &str,
) -> impl Iterator<Item = (&'a str, &'a str)> {
    let pre = format!("{pkg}.{sec}.");
    map.iter()
        .filter_map(move |(k, v)| Some((k.strip_prefix(pre.as_str())?, v.as_str())))
        .filter(|(o, _)| !o.contains('.'))
}

/// `mode_main_state` in `zwrt_zte_mc_tmp`'s text (the first `option mode_main_state '…'`).
pub fn mode_of(text: &str) -> Option<String> {
    text.lines()
        .find(|l| l.contains("option mode_main_state"))
        .and_then(|l| l.split('\'').nth(1))
        .filter(|v| !v.is_empty())
        .map(|v| v.chars().take(32).collect())
}

fn opt(s: String) -> Value {
    if s.is_empty() {
        Value::Null
    } else {
        Value::String(s.chars().take(64).collect())
    }
}

/// The boot line's fields (besides `boot_id`) and the backfill, read now.
pub async fn gather(long: &Path) -> (Value, Backfill) {
    let host = |p: &str| std::path::PathBuf::from(crate::state::host_path(p));
    let cur = KeyFile::read(&host(KEYLOG));
    let code = cur.as_ref().and_then(KeyFile::code);
    let mode = std::fs::read(host(MC_TMP))
        .ok()
        .and_then(|t| mode_of(&String::from_utf8_lossy(&t)));
    let args = json!({});
    let mut fw = crate::executor::call("zwrt_web", "device_info", &args)
        .await
        .ok()
        .and_then(|v| v["wa_inner_version"].as_str().map(str::to_string))
        .unwrap_or_default();
    if fw.is_empty() {
        fw = crate::state::uci_value("zwrt_tr069.DeviceInfo.SoftwareVersion").await;
    }
    let pon = crate::executor::call("zwrt_bsp.pm", "list", &args)
        .await
        .ok()
        .and_then(|v| v["power_on_reason"].as_i64());
    let nw = crate::state::uci_show("zte_nwinfo").await;
    let ns = nw
        .iter()
        .find(|(k, _)| k.ends_with(".net_select") && k.matches('.').count() == 2)
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    let mc = crate::state::uci_show("zwrt_zte_mc").await;
    let rt = crate::state::uci_show("zwrt_router").await;
    let dc = crate::state::uci_show("zwrt_data_commit").await;
    let fields = json!({
        "code": code,
        "mode": mode,
        "fw": opt(fw),
        "net_select": opt(ns),
        "rb_weekly": brief(section(&mc, "zwrt_zte_mc", "reboot_schedule")),
        "rb_cutoff": brief(section(&rt, "zwrt_router", "cutoff_protect")),
        "rb_connfail": brief(
            section(&dc, "zwrt_data_commit", "wwaniface1").filter(|(o, _)| o.contains("connect_fail_reboot"))
        ),
        "pon": pon,
    });
    let mut fields = fields;
    if let Planned::Ours(w) = planned_now() {
        fields["planned"] = json!(w);
    }
    let anchor = std::fs::read_to_string(long.join(ANCHOR)).ok();
    let bf = backfill(
        cur.as_ref(),
        || KeyFile::read(&host(&format!("{KEYLOG}.0"))),
        anchor.as_deref(),
        long.join(INIT_DONE).exists(),
    );
    (fields, bf)
}

/// After the lines are written: the anchor moves, the first go-live is done.
pub fn commit(long: &Path, bf: &Backfill) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(a) = &bf.anchor {
        let p = long.join(ANCHOR);
        let tmp = p.with_extension("tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(format!("{a}\n").as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &p)?;
    }
    // the boot line has its `planned` now: an earlier boot's marker goes
    if matches!(planned_now(), Planned::Ours(_)) {
        let _ = std::fs::remove_file(crate::state::host_path(PLANNED));
    }
    let i = long.join(INIT_DONE);
    if !i.exists() {
        std::fs::File::create(&i)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planned_marker() {
        assert_eq!(
            planned_of("a 100 web", "b", 50.0),
            Planned::Ours("web".into())
        );
        assert_eq!(
            planned_of("a 100 \"x,y\"", "b", 50.0),
            Planned::Ours("xy".into())
        );
        assert_eq!(
            planned_of("a 100", "b", 50.0),
            Planned::Ours("unknown".into())
        );
        assert_eq!(planned_of("a 100 web", "a", 300.0), Planned::Pending);
        assert_eq!(
            planned_of("a 100 web", "a", 100.0 + PLANNED_TTL_S + 1.0),
            Planned::Stale
        );
        assert_eq!(planned_of("a 100 web", "a", 50.0), Planned::Stale);
        assert_eq!(planned_of("", "a", 50.0), Planned::Stale);
        assert_eq!(planned_of("a x web", "b", 50.0), Planned::Stale);
    }

    fn kl(lines: &[&str]) -> String {
        lines.join("\n") + "\n"
    }

    #[test]
    fn reason_lines_and_mode() {
        let t = kl(&[
            "2026-10-01 10:00:00 boot reboot_reason_code=1185 x",
            "noise",
            "2026-10-02 11:00:00 reboot_reason_code=",
            "2026-10-03 12:00:00 reboot_reason_code=1134",
        ]);
        let k = KeyFile::from_text(1, &t);
        let r: Vec<(usize, i64, String)> =
            k.reasons.iter().map(|x| (x.0, x.1, x.2.clone())).collect();
        assert_eq!(
            r,
            [
                (1, 1185, "2026-10-01 10:00:00".into()),
                (4, 1134, "2026-10-03 12:00:00".into())
            ]
        );
        assert_eq!(k.code(), Some(1134));
        // the same file read from disk line by line: the same lines and hashes
        let f = std::env::temp_dir().join(format!("u60d-keylog-{}", std::process::id()));
        std::fs::write(&f, format!("{t}tail without newline reboot_reason_code=7")).unwrap();
        let d = KeyFile::read(&f).unwrap();
        let _ = std::fs::remove_file(&f);
        let mut want =
            KeyFile::from_text(0, &format!("{t}tail without newline reboot_reason_code=7")).reasons;
        assert_eq!(want.len(), 3);
        assert_eq!(d.reasons, std::mem::take(&mut want));
        assert_eq!(
            mode_of("config x 'y'\n\toption mode_main_state 'power_on'\n"),
            Some("power_on".into())
        );
        assert_eq!(mode_of("\toption other 'a'\n"), None);
    }

    #[test]
    fn brief_as_uci_brief() {
        let o = [
            ("enable", "1"),
            ("time", "03:00"),
            ("week", "0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17"),
        ];
        let b = brief(o.into_iter()).unwrap();
        assert!(
            b.len() <= 60 && b.starts_with("enable=1 time=03:00 week=0 1"),
            "{b}"
        );
        assert!(!b.ends_with(' '));
        assert_eq!(brief(std::iter::empty()), None);
        let mut m = std::collections::BTreeMap::new();
        m.insert("p.s".to_string(), "type".to_string());
        m.insert("p.s.a".to_string(), "1".to_string());
        m.insert("p.t.b".to_string(), "2".to_string());
        assert_eq!(brief(section(&m, "p", "s")).as_deref(), Some("a=1"));
    }

    #[test]
    fn backfill_from_the_anchor_through_a_rotation_and_lost() {
        let mut lines: Vec<String> = (1..=30).map(|i| format!("l{i}")).collect();
        lines[4] = "2026-10-01 01:00:00 reboot_reason_code=1".into(); // line 5
        lines[9] = "2026-10-01 02:00:00 reboot_reason_code=2".into(); // line 10
        lines[19] = "2026-10-01 03:00:00 reboot_reason_code=3".into(); // line 20
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let cur = KeyFile::from_text(7, &kl(&refs));
        // first go-live: no history, the anchor at this boot's line
        let first = backfill(Some(&cur), || None, None, false);
        assert!(first.lines.is_empty());
        let a = first.anchor.clone().unwrap();
        assert!(a.starts_with("7 20 "));
        // two more boots appended: lines 31, 35; this boot's is the last
        let mut more = lines.clone();
        more.extend([
            "2026-10-02 01:00:00 reboot_reason_code=4".into(),
            "x".into(),
            "y".into(),
            "z".into(),
        ]);
        more.push("2026-10-02 02:00:00 reboot_reason_code=5".into());
        let refs2: Vec<&str> = more.iter().map(String::as_str).collect();
        let cur2 = KeyFile::from_text(7, &kl(&refs2));
        let b = backfill(Some(&cur2), || None, Some(&a), true);
        let ids: Vec<&str> = b.lines.iter().map(|l| l["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["kl-7-31"]);
        assert_eq!(b.lines[0]["code"], 4);
        assert_eq!(b.lines[0]["at"], "2026-10-02 01:00:00");
        assert_eq!(b.lines[0]["gap"], 0);
        assert!(b.anchor.as_deref().unwrap().starts_with("7 35 "));
        // rotated: the anchor's file is key.log.0 now, key.log is new
        let fresh = KeyFile::from_text(
            9,
            &kl(&[
                "2026-10-03 01:00:00 reboot_reason_code=6",
                "2026-10-03 02:00:00 reboot_reason_code=7",
            ]),
        );
        let r = backfill(
            Some(&fresh),
            || Some(KeyFile::from_text(7, &kl(&refs2))),
            Some(&a),
            true,
        );
        let ids: Vec<&str> = r.lines.iter().map(|l| l["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["kl-7-31", "kl-7-35", "kl-9-1"]);
        // the anchor's lines changed (or its file is gone): key.log only, marked
        let lost = backfill(Some(&fresh), || None, Some(&a), true);
        let ids: Vec<&str> = lost
            .lines
            .iter()
            .map(|l| l["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["kl-9-1"]);
        assert_eq!(lost.lines[0]["gap"], 1);
        // no reason-code line at all: nothing, the anchor stays
        let none = KeyFile::from_text(9, "a\nb\n");
        assert_eq!(
            backfill(Some(&none), || None, Some(&a), true),
            Backfill::default()
        );
    }

    #[test]
    fn commit_moves_the_anchor_and_marks_init() {
        let d = std::env::temp_dir().join(format!("u60d-bootbf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let bf = Backfill {
            lines: vec![],
            anchor: Some("7 20 abc".into()),
        };
        commit(&d, &bf).unwrap();
        assert_eq!(
            std::fs::read_to_string(d.join(ANCHOR)).unwrap(),
            "7 20 abc\n"
        );
        assert!(d.join(INIT_DONE).exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
