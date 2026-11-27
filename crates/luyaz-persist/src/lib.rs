//! One way to write and read a state file on the device (P-6).
//!
//! `/data` is ext4 on eMMC with `data=ordered` and `auto_da_alloc` (checked on the device 11-13,
//! `docs/designs/fault-injection.md` §2), so two write tiers:
//! - [`Tier::Cheap`]: temp file in the same directory, then rename. A power cut leaves the old file
//!   or the new one, never an empty one when it replaces a file; at most ~5 s (`commit=5`) is lost.
//!   For state written often that can be rebuilt or is fine one step old.
//! - [`Tier::Durable`]: the same with the file and its directory fsynced. For what cannot be made
//!   again (the pairing identity, 400 days of usage, a write to the eSIM card in flight) and for
//!   the owner's settings. Costs flash and time: not for files written every minute.
//!
//! [`load`] keeps four outcomes apart that used to fall into "use the default" and get written over:
//! missing (start fresh), corrupt (moved aside first, so the default may be saved), newer (written
//! by a newer program: leave it, do not save over it) and an I/O error (the file may be fine: do not
//! save over it either).
//!
//! The crate never prints: callers say what happened, in their own log.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// How hard a write tries to survive a power cut (see the crate docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Cheap,
    Durable,
}

/// Suffix of the temp file next to the target (a leftover one is just written over next time).
pub const TMP_SUFFIX: &str = ".tmp-luyaz";
/// Moved-aside copies kept per file (`<name>.corrupt-<unix>`), newest first: a bug that writes a
/// bad file in a restart loop must not fill `/data` one copy per start.
pub const CORRUPT_KEEP: usize = 3;

/// Write `bytes` to `path` whole: the old file or the new one, never half (a temp file in the
/// same directory, then rename; [`Tier::Durable`] also fsyncs both). The directory must exist:
/// making it is the caller's call (a missing one can mean "not set up", e.g. no eSIM).
/// On any error the temp file is removed and the old file is untouched.
pub fn write(path: &Path, bytes: &[u8], tier: Tier) -> io::Result<()> {
    write_with(&mut RealFs { private: false }, path, bytes, tier)
}

/// [`write`] for a secret (a key, a salt): the file is created mode 0600 (the temp file too, so it
/// is never readable by others for a moment).
pub fn write_private(path: &Path, bytes: &[u8], tier: Tier) -> io::Result<()> {
    write_with(&mut RealFs { private: true }, path, bytes, tier)
}

/// What the caller's parser says about the bytes it was given.
#[derive(Debug, PartialEq)]
pub enum Parsed<T> {
    Ok(T),
    /// Not readable at all (bad JSON, bad UTF-8, wrong size…): [`load`] moves it aside.
    Corrupt(String),
    /// Readable as a later format (a version above ours): left alone, never written over.
    Newer(String),
}

/// What [`load`] found.
#[derive(Debug)]
pub enum Load<T> {
    /// No file: start fresh, saving is fine.
    Missing,
    Ok(T),
    /// The file did not read and has been moved to `moved_to`: the default may be saved.
    Corrupt { why: String, moved_to: PathBuf },
    /// Written by a newer program: run on defaults in memory and do not save over it.
    Newer(String),
    /// Could not read it, or could not move a corrupt one aside: the file may still be good.
    /// Do not save over it.
    Io(io::Error),
}

impl<T> Load<T> {
    /// May the caller write this path (missing, read, or the bad copy is out of the way)?
    pub fn may_save(&self) -> bool {
        matches!(self, Load::Missing | Load::Ok(_) | Load::Corrupt { .. })
    }
}

/// Read `path` and hand the bytes to `parse`; a corrupt file is moved to `<path>.corrupt-<now>`
/// (keeping the newest [`CORRUPT_KEEP`]) before [`Load::Corrupt`] is returned.
pub fn load<T>(path: &Path, parse: impl FnOnce(&[u8]) -> Parsed<T>) -> Load<T> {
    load_at(path, parse, unix_now())
}

/// [`load`] with the caller's clock for the `.corrupt-<now>` name (the device's clock may not be the
/// system's: the core keeps its own).
pub fn load_at<T>(path: &Path, parse: impl FnOnce(&[u8]) -> Parsed<T>, now: u64) -> Load<T> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Load::Missing,
        Err(e) => return Load::Io(e),
    };
    match parse(&bytes) {
        Parsed::Ok(t) => Load::Ok(t),
        Parsed::Newer(why) => Load::Newer(why),
        Parsed::Corrupt(why) => match move_aside(path, now) {
            Ok(moved_to) => Load::Corrupt { why, moved_to },
            Err(e) => Load::Io(io::Error::new(e.kind(), format!("{why}; not moved aside: {e}"))),
        },
    }
}

/// `<path>.corrupt-<now>` (another number when that name is taken), then drop all but the newest
/// [`CORRUPT_KEEP`] of them.
pub fn move_aside(path: &Path, now: u64) -> io::Result<PathBuf> {
    let mut to = with_suffix(path, &format!(".corrupt-{now}"));
    let mut n = now;
    while to.exists() {
        n += 1;
        to = with_suffix(path, &format!(".corrupt-{n}"));
    }
    fs::rename(path, &to)?;
    prune_corrupt(path);
    Ok(to)
}

fn prune_corrupt(path: &Path) {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else {
        return;
    };
    let dir = if dir.as_os_str().is_empty() { Path::new(".") } else { dir };
    let prefix = format!("{name}.corrupt-");
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut found: Vec<(u64, PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().into_string().ok()?;
            let t = n.strip_prefix(&prefix)?.parse().ok()?;
            Some((t, e.path()))
        })
        .collect();
    found.sort_by_key(|a| std::cmp::Reverse(a.0));
    for (_, p) in found.into_iter().skip(CORRUPT_KEEP) {
        let _ = fs::remove_file(p);
    }
}

fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The steps of a write, so a test can make any one of them fail (F-1).
trait Fs {
    /// Create (or truncate) `tmp`, write `bytes`, and fsync it when `sync`.
    fn write_file(&mut self, tmp: &Path, bytes: &[u8], sync: bool) -> io::Result<()>;
    fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()>;
    fn sync_dir(&mut self, dir: &Path) -> io::Result<()>;
    fn remove(&mut self, p: &Path);
}

struct RealFs {
    /// create the temp file 0600
    private: bool,
}

impl Fs for RealFs {
    fn write_file(&mut self, tmp: &Path, bytes: &[u8], sync: bool) -> io::Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let mut o = fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        if self.private {
            o.mode(0o600);
        }
        let mut f = o.open(tmp)?;
        f.write_all(bytes)?;
        if sync {
            f.sync_all()?;
        }
        Ok(())
    }
    fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }
    fn sync_dir(&mut self, dir: &Path) -> io::Result<()> {
        fs::File::open(dir)?.sync_all()
    }
    fn remove(&mut self, p: &Path) {
        let _ = fs::remove_file(p);
    }
}

fn write_with(fs: &mut impl Fs, path: &Path, bytes: &[u8], tier: Tier) -> io::Result<()> {
    let durable = tier == Tier::Durable;
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    let tmp = with_suffix(path, TMP_SUFFIX);
    if let Err(e) = fs.write_file(&tmp, bytes, durable).and_then(|()| fs.rename(&tmp, path)) {
        fs.remove(&tmp);
        return Err(e);
    }
    // the rename is done: a failed directory fsync means "maybe not on flash yet", not "not written"
    if durable {
        fs.sync_dir(dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
