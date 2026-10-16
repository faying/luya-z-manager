//! The kernel log on flash (u60-sentinel.md §3 G8, §20), once the guard hands `crashcap` over:
//! `/data/crashcap/kmsg-<boot id 8>.log`, written as the guard's `crashcap_*` wrote it, so doctor,
//! the scorecard's `cap` row and whoever reads it after a crash reboot see the same file.
//!
//! - Every record line as `/dev/kmsg` gives it (`<pri>,<seq>,<ts>,<flags>;<text>` and its
//!   ` KEY=value` continuation lines), less the SELinux noise (lines with ` audit: ` or ` avc: `).
//! - Started again in the same boot (a core restart, or taking it over from the guard), it goes
//!   on after the last record the file already has: a new reader of `/dev/kmsg` starts at the
//!   oldest record the kernel still holds.
//! - Synced to flash after each batch (the crash watcher reads kmsg at most every 2 s in a burst,
//!   as the guard's fsync loop ran every 2 s): the lines before a crash reboot are what this is
//!   for.
//! - Over [`MAX`] bytes: the newest half kept. This boot's file and the newest [`KEEP`]−1 others
//!   in boot order (`.order`, one line per boot; not by mtime, a boot that ended before NTP has a
//!   2025 mtime) stay; files never on the list come last, newest first.

use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

pub const DIR: &str = "/data/crashcap";
pub const KEEP: usize = 5;
pub const MAX: u64 = 8 * 1024 * 1024;
/// The guard's handover name, and its file (compat `guard_duties::FILE`).
pub const NAME: &str = "crashcap";
pub const HANDOVER: &str = "/tmp/luyaz/handover";

pub struct KmsgFile {
    pub file: PathBuf,
    /// The last record's seq the file has (None: nothing yet).
    last: Option<u64>,
    max: u64,
}

fn header_seq(line: &str) -> Option<u64> {
    let mut f = line.splitn(3, ',');
    let pri = f.next()?;
    let seq = f.next()?;
    f.next()?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    (digits(pri) && digits(seq)).then(|| seq.parse().ok())?
}

/// Where a restart looks for the last record (the guard read the last 200 lines).
const LAST_SEQ_BYTES: u64 = 64 * 1024;

/// The file's last `n` bytes as text, a cut first line dropped.
fn tail_text(p: &Path, n: u64) -> std::io::Result<String> {
    let mut f = std::fs::File::open(p)?;
    let len = f.metadata()?.len();
    let from = len.saturating_sub(n);
    f.seek(std::io::SeekFrom::Start(from))?;
    let mut b = Vec::new();
    f.take(n).read_to_end(&mut b)?;
    let t = String::from_utf8_lossy(&b).into_owned();
    Ok(match (from > 0, t.find('\n')) {
        (true, Some(i)) => t[i + 1..].to_string(),
        (true, None) => String::new(),
        (false, _) => t,
    })
}

/// The last record's seq among the last lines of `text` (the guard's `tail -n 200 | awk`).
fn last_seq(text: &str) -> Option<u64> {
    let lines: Vec<&str> = text.lines().collect();
    let from = lines.len().saturating_sub(200);
    lines[from..].iter().rev().find_map(|l| header_seq(l))
}

/// The guard's `crashcap_order`: this boot's file on the list once; trimmed to 20 past 50.
fn order(dir: &Path, file: &Path) -> std::io::Result<()> {
    let o = dir.join(".order");
    let name = file.to_string_lossy().into_owned();
    let text = std::fs::read_to_string(&o).unwrap_or_default();
    if text.lines().any(|l| l == name) {
        return Ok(());
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&o)?;
    writeln!(f, "{name}")?;
    f.sync_all()?;
    let text = std::fs::read_to_string(&o)?;
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() > 50 {
        let keep: String = lines[lines.len() - 20..]
            .iter()
            .map(|l| format!("{l}\n"))
            .collect();
        let tmp = dir.join(".order.tmp");
        let mut t = std::fs::File::create(&tmp)?;
        t.write_all(keep.as_bytes())?;
        t.sync_all()?;
        std::fs::rename(&tmp, &o)?;
    }
    Ok(())
}

/// The guard's `crashcap_prune`: the files to delete (this boot's counts as kept even before it
/// exists).
fn prune(dir: &Path, file: &Path, keep: usize) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, String)> = rd
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            (n.starts_with("kmsg-") && n.ends_with(".log")).then(|| {
                let t = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                (t, e.path().to_string_lossy().into_owned())
            })
        })
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.0)); // newest first, as `ls -t`
    let order = std::fs::read_to_string(dir.join(".order")).unwrap_or_default();
    let listed: Vec<&str> = order.lines().collect();
    let exists = |p: &str| files.iter().any(|(_, f)| f == p);
    let mut kept: Vec<String> = vec![file.to_string_lossy().into_owned()];
    for l in listed.iter().rev() {
        if kept.len() >= keep {
            break;
        }
        if exists(l) && !kept.iter().any(|k| k == l) {
            kept.push(l.to_string());
        }
    }
    for (_, f) in &files {
        if kept.len() >= keep {
            break;
        }
        if !listed.contains(&f.as_str()) && !kept.contains(f) {
            kept.push(f.clone());
        }
    }
    for (_, f) in &files {
        if !kept.contains(f) {
            let _ = std::fs::remove_file(f);
        }
    }
}

impl KmsgFile {
    /// This boot's file under `dir`: on the boot list, older ones pruned, and where it left off.
    pub fn start(dir: &Path, boot: &str, keep: usize, max: u64) -> std::io::Result<KmsgFile> {
        std::fs::create_dir_all(dir)?;
        let b8: String = boot.chars().take(8).collect();
        let b8 = if b8.is_empty() { "boot".into() } else { b8 };
        let file = dir.join(format!("kmsg-{b8}.log"));
        order(dir, &file)?;
        prune(dir, &file, keep);
        let last = tail_text(&file, LAST_SEQ_BYTES)
            .ok()
            .and_then(|t| last_seq(&t));
        Ok(KmsgFile { file, last, max })
    }

    /// The lines of one raw record to append (empty: already in the file, or only noise).
    pub fn take(&mut self, raw: &str) -> String {
        let mut out = String::new();
        let mut k = true;
        for l in raw.split('\n').filter(|l| !l.is_empty()) {
            if let Some(seq) = header_seq(l) {
                k = self.last.is_none_or(|s| seq > s);
                if k {
                    self.last = Some(seq);
                }
            }
            if k && !l.contains(" audit: ") && !l.contains(" avc: ") {
                out.push_str(l);
                out.push('\n');
            }
        }
        out
    }

    /// Append, sync, and cut to the newest half when over the limit. Returns whether it cut.
    pub fn append(&mut self, text: &str) -> std::io::Result<bool> {
        if text.is_empty() {
            return Ok(false);
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file)?;
        f.write_all(text.as_bytes())?;
        f.sync_data()?;
        let size = f.metadata()?.len();
        drop(f);
        if size <= self.max {
            return Ok(false);
        }
        // the newest half, copied in pieces (the guard's `tail -c`): never the whole file in
        // this long-running process's memory
        let half = self.max / 2;
        let mut src = std::fs::File::open(&self.file)?;
        src.seek(std::io::SeekFrom::Start(size.saturating_sub(half)))?;
        let tmp = self.file.with_extension("log.tmp");
        let mut t = std::fs::File::create(&tmp)?;
        std::io::copy(&mut src.take(half), &mut t)?;
        t.sync_all()?;
        std::fs::rename(&tmp, &self.file)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("u60d-kmsgfile-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn records_as_kmsg_gives_them_less_selinux_noise() {
        let d = dir("take");
        let mut k = KmsgFile::start(&d, "c6f6ed41-aaaa", KEEP, MAX).unwrap();
        assert!(k.file.ends_with("kmsg-c6f6ed41.log"));
        let mut t = String::new();
        t += &k.take("6,10,100,-;hello\n SUBSYSTEM=x\n DEVICE=y\n");
        t += &k.take("5,11,101,-;type=1400 audit: avc: denied\n");
        t += &k.take("3,12,102,-;remoteproc0: crash\n");
        assert_eq!(
            t,
            "6,10,100,-;hello\n SUBSYSTEM=x\n DEVICE=y\n3,12,102,-;remoteproc0: crash\n"
        );
        k.append(&t).unwrap();
        // a restart in the same boot goes on after seq 12 (the kernel gives the old ones again)
        let mut k2 = KmsgFile::start(&d, "c6f6ed41-aaaa", KEEP, MAX).unwrap();
        assert_eq!(k2.take("6,11,101,-;old\n"), "");
        assert_eq!(k2.take("6,12,102,-;old\n"), "");
        assert_eq!(k2.take("6,13,103,-;new\n"), "6,13,103,-;new\n");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_restart_finds_its_place_from_the_files_tail_only() {
        let d = dir("tail");
        let mut k = KmsgFile::start(&d, "t1", KEEP, MAX).unwrap();
        // 200 KB of records: more than the tail read, the last one still found
        let mut t = String::new();
        for i in 1..=2000u64 {
            t += &k.take(&format!("6,{i},{i},-;{}\n", "x".repeat(90)));
        }
        k.append(&t).unwrap();
        assert!(fs::metadata(&k.file).unwrap().len() > 2 * LAST_SEQ_BYTES);
        let mut k2 = KmsgFile::start(&d, "t1", KEEP, MAX).unwrap();
        assert_eq!(k2.take("6,2000,1,-;old\n"), "");
        assert_eq!(k2.take("6,2001,1,-;new\n"), "6,2001,1,-;new\n");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn over_the_limit_keeps_the_newest_half() {
        let d = dir("cut");
        let mut k = KmsgFile::start(&d, "b1", KEEP, 100).unwrap();
        assert!(!k.append(&"x".repeat(90)).unwrap());
        assert!(k.append(&"y".repeat(20)).unwrap());
        let b = fs::read_to_string(&k.file).unwrap();
        assert_eq!(b.len(), 50);
        assert!(b.ends_with(&"y".repeat(20)));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn keeps_this_boot_and_the_newest_others_in_boot_order() {
        let d = dir("prune");
        fs::create_dir_all(&d).unwrap();
        // boots a..f in boot order; f's file has the oldest mtime (a boot before NTP)
        for b in ["a", "b", "c", "d", "e", "f"] {
            let f = d.join(format!("kmsg-{b}.log"));
            fs::write(&f, "1,1,1,-;x\n").unwrap();
            order(&d, &f).unwrap();
        }
        fs::write(d.join("kmsg-old.log"), "").unwrap(); // never on the list
        let f = d.join("kmsg-f.log");
        let t = std::fs::File::options().write(true).open(&f).unwrap();
        t.set_modified(std::time::UNIX_EPOCH).unwrap();
        KmsgFile::start(&d, "g", 5, MAX).unwrap();
        let mut left: Vec<String> = fs::read_dir(&d)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".log"))
            .collect();
        left.sort();
        assert_eq!(
            left,
            ["kmsg-c.log", "kmsg-d.log", "kmsg-e.log", "kmsg-f.log"]
        );
        // .order lists g once, even started again
        KmsgFile::start(&d, "g", 5, MAX).unwrap();
        let o = fs::read_to_string(d.join(".order")).unwrap();
        assert_eq!(o.lines().filter(|l| l.ends_with("kmsg-g.log")).count(), 1);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn order_trimmed_to_20_past_50() {
        let d = dir("order");
        fs::create_dir_all(&d).unwrap();
        for i in 0..51 {
            order(&d, &d.join(format!("kmsg-{i}.log"))).unwrap();
        }
        let o = fs::read_to_string(d.join(".order")).unwrap();
        assert_eq!(o.lines().count(), 20);
        assert!(o.ends_with("kmsg-50.log\n"));
        let _ = fs::remove_dir_all(&d);
    }
}
