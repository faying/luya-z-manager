//! The long-term layer's once-a-check look at the device (u60-sentinel.md §19 T9c): the ledger
//! kinds the guard's ledger task writes each round, as it writes them (touch-ui docs/LEDGER.md
//! §4, §10, §11):
//!
//! - `ver`: each program once a boot (`md5` of `/proc/<pid>/exe`, `how=exe`; `luyaz-core` is
//!   this process, `extra` its ubus backend), then again when its pid changed and its md5 too;
//!   the scripts once a boot (`how=file`).
//! - `proc_restart`: the core when its pid or start time changed (one that is gone keeps its
//!   record and counts when it comes back); `crashlog`, `ship` as the guard.
//! - `svc_exit`: a new or changed file under `/data/crashlog/<program>/`; the core's own list
//!   (`long/crashlog.seen`), the first look ever only takes what is there.
//! - `uid`: give-ups, hand-backs and requests in the screen log, numbered by line over the boot
//!   (`.old` when it was renamed).
//! - `recovery_seen`: the modem's remoteproc recovery, first reading of the boot and any fall
//!   back to disabled.
//!
//! Programs are found by `comm` in /proc only when one of the known pids is gone or changed its
//! start time, or at most every [`RESCAN_S`] for one not running; otherwise only
//! `/proc/<pid>/stat` of the known ones is read. Files the look writes besides the event lines
//! (`crashlog.seen`) are returned as [`After`] steps, done once the lines are on
//! disk, so a kill in between repeats an event (same id) rather than losing one.
//!
//! Coverage: `crashlog` = the directory is readable, `uidlog` = the log's last line (or
//! `.old`'s) starts with a time, each check.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A program not running is looked for again at most this often.
pub const RESCAN_S: i64 = 600;
const PROGS: [&str; 4] = [crate::paths::CORE, crate::paths::UI, "tailscaled", "mihomo"];
const SCRIPTS: [&str; 2] = ["doctor.sh", "alert-lib.sh"];
const RESTARTS: [&str; 1] = [crate::paths::CORE];
/// Our own program name: this process.
const SELF: &str = crate::paths::CORE;

/// Where the look reads (the device's, or a test root).
pub struct Paths {
    pub proc_: PathBuf,
    pub crashlog: PathBuf,
    /// The screen log.
    pub uid_log: PathBuf,
    pub recovery: PathBuf,
    pub recovery_set: PathBuf,
    pub ship_txn: PathBuf,
    pub guard_dir: PathBuf,
    pub long: PathBuf,
    pub self_pid: u32,
    /// This process's ubus backend (`LUYAZ_UBUS`), for `ver`'s `extra`.
    pub ubus: String,
}

impl Paths {
    pub fn device() -> Paths {
        let h = |p: &str| PathBuf::from(crate::state::host_path(p));
        Paths {
            proc_: h("/proc"),
            crashlog: h("/data/crashlog"),
            uid_log: h("/tmp/luyaz-screen.log"),
            recovery: h("/sys/class/remoteproc/remoteproc0/recovery"),
            recovery_set: h("/tmp/luyaz/recovery-set"),
            ship_txn: h("/data/luyaz/ship/txn"),
            guard_dir: h("/data/luyaz/scripts"),
            long: h(super::DIR),
            self_pid: std::process::id(),
            ubus: std::env::var("LUYAZ_UBUS")
                .ok()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "cli".into()),
        }
    }
}

/// This boot's look state (saved with the fold, so a restart goes on from it).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct State {
    /// Program → (pid, start time) last seen.
    procs: BTreeMap<String, (u32, u64)>,
    /// Program → md5 of its last `ver` line ("" = written as not running).
    vers: BTreeMap<String, String>,
    /// Program → pid its md5 was last taken for.
    ver_pid: BTreeMap<String, u32>,
    /// Programs running at the last look: (pid, start time).
    run: BTreeMap<String, (u32, u64)>,
    /// The first look of the boot is done (ver lines, crashlog `boot_init`).
    booted: bool,
    /// When /proc was last searched by name (boot seconds).
    scanned: Option<i64>,
    /// The screen log: lines read in the current file, lines in the files before it.
    uid: (u64, u64),
    recovery: Option<String>,
}

/// A file write to do once the lines are on disk.
#[derive(Debug, PartialEq)]
pub enum After {
    /// Lines to add to `long/crashlog.seen`.
    Seen(Vec<String>),
}

/// What one look gives.
#[derive(Debug, Default)]
pub struct Look {
    /// (kind, fields with `id` where the kind has one).
    pub lines: Vec<(&'static str, Value)>,
    pub after: Vec<After>,
    /// Sources and whether they worked this check (`crashlog`, `uidlog`).
    pub cov: Vec<(String, bool)>,
}

fn read(p: &Path) -> Option<String> {
    std::fs::read(p)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}

fn hex(d: [u8; 16], n: usize) -> String {
    let s: String = d.iter().map(|x| format!("{x:02x}")).collect();
    s[..n].to_string()
}

fn md5_hex(b: &[u8], n: usize) -> String {
    hex(crate::crashwatch::md5(b), n)
}

/// A program's or script's md5 prefix, the file read in pieces.
pub(crate) fn md5_path(p: &Path, n: usize) -> Option<String> {
    crate::crashwatch::md5_file(p).map(|d| hex(d, n))
}

/// `/proc/<pid>/stat`'s start time (field 22), None when gone.
pub fn start_time(proc_: &Path, pid: u32) -> Option<u64> {
    let s = read(&proc_.join(pid.to_string()).join("stat"))?;
    let rest = s.get(s.rfind(')')? + 2..)?;
    rest.split_whitespace().nth(19)?.parse().ok()
}

/// The first pid whose `comm` is each of `names` (the guard's `find_pid`).
pub fn find_pids(proc_: &Path, names: &[&str]) -> BTreeMap<String, u32> {
    let mut found = BTreeMap::new();
    let Ok(rd) = std::fs::read_dir(proc_) else {
        return found;
    };
    let mut pids: Vec<u32> = rd
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse().ok())
        .collect();
    pids.sort_unstable();
    for pid in pids {
        let Some(c) = read(&proc_.join(pid.to_string()).join("comm")) else {
            continue;
        };
        let c = c.trim_end_matches('\n');
        // a side-by-side core is found under the core's ledger key
        let k = crate::paths::prog_key(c);
        if names.contains(&k) && !found.contains_key(k) && !child_of_same(proc_, pid, c) {
            found.insert(k.to_string(), pid);
        }
    }
    found
}

/// A child under its parent's name is not the program (the core's `/v2/scorecard` runs the card in
/// a child of its own binary; taking it for the core reads as a restart).
fn child_of_same(proc_: &Path, pid: u32, comm: &str) -> bool {
    let Some(st) = read(&proc_.join(pid.to_string()).join("stat")) else {
        return false;
    };
    let Some(ppid) = st
        .rsplit_once(") ")
        .and_then(|(_, r)| r.split_whitespace().nth(1))
    else {
        return false;
    };
    read(&proc_.join(ppid).join("comm")).is_some_and(|p| p.trim_end_matches('\n') == comm)
}

/// 1 when `prog`'s restart is a deployment's own (the guard's `ship_restart`).
fn ship_restart(txn: Option<&str>, prog: &str, boot: &str, up: i64) -> u8 {
    let Some(t) = txn else { return 0 };
    if prog != crate::paths::CORE {
        return 0;
    }
    let get = |k: &str| {
        t.lines()
            .find_map(|l| l.strip_prefix(k).and_then(|v| v.strip_prefix('=')))
            .unwrap_or("")
    };
    if get("boot_id") != boot || get("comp") != "core" {
        return 0;
    }
    if matches!(
        get("phase"),
        "staged" | "trial" | "promote" | "check" | "manifest" | "rollback"
    ) {
        return 1;
    }
    match get("t_phase").parse::<i64>() {
        Ok(tp) if up >= tp && up - tp <= 600 => 1,
        _ => 0,
    }
}

/// What a crash file's name and text say: (`status`, `crash_up`).
fn crash_file(name: &str, text: &str) -> (Value, Value) {
    let status = text
        .lines()
        .find_map(|l| l.strip_prefix("status:"))
        .map(|s| s.trim_start().chars().take(64).collect::<String>());
    let up = name
        .rsplit_once("-up")
        .and_then(|(_, r)| r.strip_suffix(".log"))
        .filter(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|d| d.parse::<i64>().ok());
    (json!(status), json!(up))
}

/// The screen log's lines: what kind, if any.
fn uid_what(line: &str) -> Option<&'static str> {
    let rest = line.split_once(' ').map_or("", |x| x.1);
    if rest.starts_with("giving up") {
        Some("gave_up")
    } else if rest.starts_with("starting the vendor UI") {
        Some("handback")
    } else if rest.starts_with("request: ") || rest.starts_with("corner long-press:") {
        Some("other")
    } else {
        None
    }
}

fn clean(s: &str) -> String {
    s.chars()
        .map(|c| if (' '..='~').contains(&c) { c } else { '_' })
        .filter(|c| *c != '"' && *c != '\\')
        .take(160)
        .collect()
}

fn lines_of(t: &str) -> Vec<&str> {
    let mut v: Vec<&str> = t.split('\n').collect();
    if v.last() == Some(&"") {
        v.pop();
    }
    v
}

impl State {
    /// Our programs as the last look found them.
    pub fn pids(&self) -> BTreeMap<String, u32> {
        self.run.iter().map(|(k, v)| (k.clone(), v.0)).collect()
    }

    fn ver(&mut self, p: &Paths, prog: &str, pid: Option<u32>, out: &mut Look) {
        let exe = |pid: u32| p.proc_.join(pid.to_string()).join("exe");
        let md5 = pid.and_then(|pid| md5_path(&exe(pid), 8));
        let extra = if prog == SELF && pid.is_some() {
            json!(format!("ubus={}", p.ubus))
        } else {
            Value::Null
        };
        out.lines.push((
            "ver",
            json!({"prog": prog, "md5": md5, "how": "exe", "pid": pid, "extra": extra}),
        ));
        self.vers.insert(prog.into(), md5.unwrap_or_default());
        if let Some(pid) = pid {
            self.ver_pid.insert(prog.into(), pid);
        }
    }

    /// One look. `boot` is the full boot id, `up` boot seconds.
    pub fn look(&mut self, p: &Paths, boot: &str, up: i64) -> Look {
        let mut out = Look::default();
        let b8: String = boot
            .chars()
            .filter(|c| c.is_ascii_hexdigit())
            .take(8)
            .collect();
        // ── the programs: known pids by their stat; /proc searched only when one changed ──
        let mut pids: BTreeMap<String, u32> = BTreeMap::new();
        let mut changed = !self.booted;
        for (prog, &(pid, st)) in &self.run {
            if start_time(&p.proc_, pid) == Some(st) {
                pids.insert(prog.clone(), pid);
            } else {
                changed = true;
            }
        }
        pids.insert(SELF.into(), p.self_pid);
        let missing = PROGS.iter().any(|q| !pids.contains_key(*q));
        if changed || (missing && self.scanned.is_none_or(|s| up - s >= RESCAN_S)) {
            self.scanned = Some(up);
            pids = find_pids(&p.proc_, &PROGS);
            pids.insert(SELF.into(), p.self_pid);
        }
        self.run = pids
            .iter()
            .filter_map(|(q, &pid)| Some((q.clone(), (pid, start_time(&p.proc_, pid)?))))
            .collect();
        // ── svc_exit (before proc_restart: its crashlog field) ──
        let seen_path = p.long.join("crashlog.seen");
        let seen = read(&seen_path);
        let first_ever = seen.is_none();
        let seen = seen.unwrap_or_default();
        let mut new_seen = Vec::new();
        let mut cl_new: Vec<String> = Vec::new();
        let ok = match std::fs::read_dir(&p.crashlog) {
            Err(_) => false,
            Ok(rd) => {
                let mut progs: Vec<PathBuf> = rd
                    .flatten()
                    .map(|e| e.path())
                    .filter(|x| x.is_dir())
                    .collect();
                progs.sort();
                for d in progs {
                    let Ok(files) = std::fs::read_dir(&d) else {
                        continue;
                    };
                    let prog = d
                        .file_name()
                        .map(|x| x.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let mut files: Vec<PathBuf> = files
                        .flatten()
                        .map(|e| e.path())
                        .filter(|x| x.extension().is_some_and(|e| e == "log") && x.is_file())
                        .collect();
                    files.sort();
                    for f in files {
                        let name = f
                            .file_name()
                            .map(|x| x.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        let rel = format!("{prog}/{name}");
                        // name + size first; the file is read only when that is new
                        let Ok(m) = std::fs::metadata(&f) else {
                            continue;
                        };
                        let key = format!("{rel} {} ", m.len());
                        if seen.lines().any(|l| l.starts_with(&key)) {
                            continue;
                        }
                        let Ok(bytes) = std::fs::read(&f) else {
                            continue;
                        };
                        let m = md5_hex(&bytes, 12);
                        if !first_ever {
                            let (status, cu) = crash_file(&name, &String::from_utf8_lossy(&bytes));
                            out.lines.push((
                                "svc_exit",
                                json!({
                                    "id": format!("cl-{m}"), "prog": clean(&prog), "file": clean(&name),
                                    "status": status, "found": if self.booted { "round" } else { "boot_init" },
                                    "crash_up": cu,
                                }),
                            ));
                            cl_new.push(prog.clone());
                        }
                        new_seen.push(format!("{key}{m}"));
                    }
                }
                true
            }
        };
        out.cov.push(("crashlog".into(), ok));
        if !new_seen.is_empty() || first_ever && ok {
            out.after.push(After::Seen(new_seen));
        }
        // ── ver ──
        if !self.booted {
            for prog in PROGS {
                self.ver(p, prog, pids.get(prog).copied(), &mut out);
            }
            for s in SCRIPTS {
                let f = p.guard_dir.join(s);
                if let Some(m) = md5_path(&f, 8) {
                    out.lines.push((
                        "ver",
                        json!({"prog": s, "md5": m, "how": "file", "pid": null, "extra": null}),
                    ));
                }
            }
        } else {
            for prog in PROGS {
                let Some(&pid) = pids.get(prog) else { continue };
                if self.ver_pid.get(prog) == Some(&pid) {
                    continue;
                }
                let md5 = md5_path(&p.proc_.join(pid.to_string()).join("exe"), 8);
                self.ver_pid.insert(prog.into(), pid);
                if md5.is_some() && md5.as_deref() != self.vers.get(prog).map(String::as_str) {
                    self.ver(p, prog, Some(pid), &mut out);
                }
            }
        }
        // ── proc_restart ──
        let txn = read(&p.ship_txn);
        for prog in RESTARTS {
            let Some(&pid) = pids.get(prog) else { continue };
            let Some(st) = start_time(&p.proc_, pid) else {
                continue;
            };
            if let Some(&(op, os)) = self.procs.get(prog)
                && (op, os) != (pid, st)
            {
                out.lines.push((
                    "proc_restart",
                    json!({
                        "id": format!("pr-{b8}-{pid}-{st}"), "prog": prog, "old": op, "new": pid,
                        "crashlog": u8::from(cl_new.iter().any(|c| c == prog)),
                        "ship": ship_restart(txn.as_deref(), prog, boot, up),
                    }),
                ));
            }
            self.procs.insert(prog.into(), (pid, st));
        }
        // ── uid ──
        self.uid_look(p, &b8, &mut out);
        // ── recovery_seen ──
        if let Some(v) = read(&p.recovery)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
        {
            let last = self.recovery.as_deref();
            if last.is_none() || (last == Some("enabled") && v == "disabled") {
                out.lines.push((
                    "recovery_seen",
                    json!({"value": clean(&v), "set_by_guard": u8::from(p.recovery_set.exists())}),
                ));
            }
            self.recovery = Some(v);
        }
        self.booted = true;
        out
    }

    fn uid_look(&mut self, p: &Paths, b8: &str, out: &mut Look) {
        let cur = read(&p.uid_log);
        let old_path = PathBuf::from(format!("{}.old", p.uid_log.display()));
        let take = |t: &str, from: u64, to: u64, base: u64, out: &mut Look| {
            for (i, l) in lines_of(t).iter().enumerate() {
                let n = i as u64 + 1;
                if n <= from || n > to {
                    continue;
                }
                if let Some(w) = uid_what(l) {
                    out.lines.push((
                        "uid",
                        json!({"id": format!("uid-{b8}-{}", base + n), "what": w, "detail": clean(l)}),
                    ));
                }
            }
        };
        let (mut read_n, mut base) = self.uid;
        let n = cur.as_deref().map_or(0, |t| lines_of(t).len() as u64);
        if n < read_n {
            // renamed to .old by the guard (or cut): the rest of it there, then count on from 0
            let old = read(&old_path).unwrap_or_default();
            let on = lines_of(&old).len() as u64;
            if on > read_n {
                take(&old, read_n, on, base, out);
                base += on;
            } else {
                base += read_n;
            }
            read_n = 0;
        }
        if let Some(t) = &cur
            && n > read_n
        {
            take(t, read_n, n, base, out);
            read_n = n;
        }
        self.uid = (read_n, base);
        // coverage: the last line starts with a time (else .old's)
        let timed = |t: Option<String>| {
            t.and_then(|t| {
                lines_of(&t)
                    .last()
                    .map(|l| l.as_bytes().first().is_some_and(u8::is_ascii_digit))
            })
        };
        let ok = timed(cur)
            .or_else(|| timed(read(&old_path)))
            .unwrap_or(false);
        out.cov.push(("uidlog".into(), ok));
    }
}

/// The [`After`] steps, once the lines are written.
pub fn commit(long: &Path, after: &[After]) -> std::io::Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(long)?;
    for a in after {
        match a {
            After::Seen(lines) => {
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(long.join("crashlog.seen"))?;
                for l in lines {
                    writeln!(f, "{l}")?;
                }
                f.sync_all()?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "watch_tests.rs"]
mod tests;
