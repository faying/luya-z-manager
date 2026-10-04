//! Small file helpers shared by modules that keep state on /data.

use std::fs;

/// Write `path.tmp`, then rename it over `path`: a full disk or a crash
/// mid-write leaves the old file whole instead of a torn one that fails to
/// parse on the next start.
pub(crate) fn atomic_write(path: &str, data: &[u8]) -> std::io::Result<()> {
    let tmp = format!("{path}.tmp");
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_keeps_the_old_file_when_the_write_fails() {
        let d = std::env::temp_dir().join(format!("u60-atomic-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
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
}
