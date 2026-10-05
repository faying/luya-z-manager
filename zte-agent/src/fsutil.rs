//! Small file helpers shared by modules that keep state on /data.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Write `path.tmp`, then rename it over `path`: a full disk or a crash
/// mid-write leaves the old file whole instead of a torn one that fails to
/// parse on the next start.
///
/// The rename puts a new inode in place, so the mode would otherwise reset to
/// the umask default; the old file's mode is carried over instead (an
/// in-place `fs::write` kept it, and someone may have tightened it by hand).
/// A file that did not exist yet gets the default, as `fs::write` gave it.
pub(crate) fn atomic_write(path: impl AsRef<Path>, data: &[u8]) -> std::io::Result<()> {
    let path = path.as_ref();
    let keep = fs::metadata(path).ok().map(|m| m.permissions().mode() & 0o7777);
    write_then_rename(path, data, keep)
}

/// [`atomic_write`] with a fixed mode (e.g. 0o600 for a secret). The mode is
/// set on the tmp file before any data goes in, so the content is never
/// readable under a wider one.
pub(crate) fn atomic_write_mode(path: impl AsRef<Path>, data: &[u8], mode: u32) -> std::io::Result<()> {
    write_then_rename(path.as_ref(), data, Some(mode))
}

fn write_then_rename(path: &Path, data: &[u8], mode: Option<u32>) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        let mut f = OpenOptions::new().write(true).create(true).truncate(true).open(&tmp)?;
        // After open, not via OpenOptions::mode: that one is masked by the
        // umask and ignored for a .tmp left over from a crash.
        if let Some(m) = mode {
            f.set_permissions(fs::Permissions::from_mode(m))?;
        }
        f.write_all(data)?;
    }
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("u60-atomic-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn mode(p: &Path) -> u32 {
        fs::metadata(p).unwrap().permissions().mode() & 0o7777
    }

    #[test]
    fn atomic_write_keeps_the_old_file_when_the_write_fails() {
        let d = dir("fail");
        let p = d.join("state.json");
        let path = p.to_str().unwrap();
        atomic_write(path, b"{\"last_id\":7}").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "{\"last_id\":7}");
        assert!(!d.join("state.json.tmp").exists());
        // A tmp that cannot be written (here: a directory in the way) stands
        // in for ENOSPC: the error comes back and the old content stays.
        fs::create_dir(d.join("state.json.tmp")).unwrap();
        assert!(atomic_write(path, b"{\"last_id\":8}").is_err());
        assert_eq!(fs::read_to_string(&p).unwrap(), "{\"last_id\":7}");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn atomic_write_keeps_the_mode_of_the_file_it_replaces() {
        let d = dir("mode");
        let p = d.join("pin");
        fs::write(&p, "away").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
        atomic_write(&p, b"home").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "home");
        assert_eq!(mode(&p), 0o600);
        // A new file gets what fs::write would have given it.
        let fresh = d.join("fresh");
        atomic_write(&fresh, b"x").unwrap();
        let reference = d.join("reference");
        fs::write(&reference, "x").unwrap();
        assert_eq!(mode(&fresh), mode(&reference));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn atomic_write_mode_forces_the_mode_even_over_a_stale_tmp() {
        let d = dir("force");
        let p = d.join("secret");
        fs::write(&p, "old").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
        // A world-readable .tmp left by a crash must not keep its mode.
        let tmp = d.join("secret.tmp");
        fs::write(&tmp, "stale").unwrap();
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o666)).unwrap();
        atomic_write_mode(&p, b"new", 0o600).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "new");
        assert_eq!(mode(&p), 0o600);
        assert!(!tmp.exists());
        let _ = fs::remove_dir_all(&d);
    }
}
