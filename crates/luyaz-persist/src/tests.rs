use super::*;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("luyaz-persist-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn text(p: &[u8]) -> Parsed<String> {
    match std::str::from_utf8(p) {
        Ok(t) if t.starts_with("v2:") => Parsed::Newer("v2".into()),
        Ok(t) => Parsed::Ok(t.to_owned()),
        Err(e) => Parsed::Corrupt(e.to_string()),
    }
}

#[test]
fn both_tiers_write_whole_and_replace() {
    let d = scratch("tiers");
    for tier in [Tier::Cheap, Tier::Durable] {
        let p = d.join("sub/state.json");
        // the directory is the caller's to make
        assert_eq!(write(&p, b"one", tier).unwrap_err().kind(), io::ErrorKind::NotFound);
        fs::create_dir(d.join("sub")).unwrap();
        write(&p, b"one", tier).unwrap();
        write(&p, b"two", tier).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two");
        assert!(!with_suffix(&p, TMP_SUFFIX).exists());
        fs::remove_dir_all(d.join("sub")).unwrap();
    }
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_private_file_is_0600() {
    use std::os::unix::fs::PermissionsExt;
    let d = scratch("private");
    let p = d.join("key");
    write_private(&p, b"secret", Tier::Durable).unwrap();
    assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn load_tells_missing_ok_newer_corrupt_and_io_apart() {
    let d = scratch("load");
    let p = d.join("f");
    assert!(matches!(load(&p, text), Load::Missing));
    fs::write(&p, "hello").unwrap();
    assert!(matches!(load(&p, text), Load::Ok(t) if t == "hello"));
    // newer: left where it is, and the caller may not save
    fs::write(&p, "v2:later").unwrap();
    let l = load(&p, text);
    assert!(matches!(l, Load::Newer(_)) && !l.may_save());
    assert_eq!(fs::read(&p).unwrap(), b"v2:later");
    // corrupt: moved aside first, then the caller may save
    fs::write(&p, [0xff, 0xfe]).unwrap();
    let l = load_at(&p, text, 1000);
    let Load::Corrupt { moved_to, .. } = &l else { panic!("{l:?}") };
    assert!(l.may_save());
    assert_eq!(moved_to, &d.join("f.corrupt-1000"));
    assert!(!p.exists());
    // an I/O error (here: a directory where the file should be): the caller may not save
    fs::create_dir(&p).unwrap();
    let l = load(&p, text);
    assert!(matches!(l, Load::Io(_)) && !l.may_save(), "{l:?}");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn only_the_newest_corrupt_copies_stay() {
    let d = scratch("prune");
    let p = d.join("f");
    for t in 0..6 {
        fs::write(&p, [0xff]).unwrap();
        assert!(matches!(load_at(&p, text, 100 + t), Load::Corrupt { .. }));
    }
    let mut left: Vec<String> = fs::read_dir(&d)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    left.sort();
    assert_eq!(left, ["f.corrupt-103", "f.corrupt-104", "f.corrupt-105"]);
    // the same second twice: the second one does not replace the first
    fs::write(&p, [0xff]).unwrap();
    assert!(matches!(load_at(&p, text, 105), Load::Corrupt { moved_to, .. } if moved_to == d.join("f.corrupt-106")));
    let _ = fs::remove_dir_all(&d);
}

// ── F-1: every step failing, with each error kind ─────────────────────────────

/// A filesystem whose `fail_at`-th step fails with `kind`; records the steps and what is on disk.
struct Faulty {
    fail_at: usize,
    kind: io::ErrorKind,
    steps: usize,
    files: std::collections::BTreeMap<PathBuf, Vec<u8>>,
}

impl Faulty {
    fn step(&mut self) -> io::Result<()> {
        self.steps += 1;
        if self.steps == self.fail_at {
            return Err(io::Error::from(self.kind));
        }
        Ok(())
    }
}

impl Fs for Faulty {
    fn write_file(&mut self, tmp: &Path, bytes: &[u8], sync: bool) -> io::Result<()> {
        // create, then write (a failed write leaves a part), then fsync
        self.step()?;
        self.files.insert(tmp.into(), Vec::new());
        self.step()?;
        self.files.insert(tmp.into(), bytes.to_vec());
        if sync {
            self.step()?;
        }
        Ok(())
    }
    fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        self.step()?;
        let b = self.files.remove(from).unwrap();
        self.files.insert(to.into(), b);
        Ok(())
    }
    fn sync_dir(&mut self, _: &Path) -> io::Result<()> {
        self.step()
    }
    fn remove(&mut self, p: &Path) {
        self.files.remove(p);
    }
}

#[test]
fn a_failing_step_never_leaves_a_half_file_or_a_temp_file() {
    let p = PathBuf::from("/data/x/state.json");
    for (tier, steps) in [(Tier::Cheap, 3), (Tier::Durable, 5)] {
        for kind in [
            io::ErrorKind::StorageFull,
            io::ErrorKind::ReadOnlyFilesystem,
            io::ErrorKind::PermissionDenied,
        ] {
            for fail_at in 1..=steps {
                let mut f = Faulty { fail_at, kind, steps: 0, files: Default::default() };
                f.files.insert(p.clone(), b"old".to_vec());
                let r = write_with(&mut f, &p, b"new", tier);
                let at = format!("{tier:?} step {fail_at} {kind:?}");
                assert_eq!(r.as_ref().err().map(io::Error::kind), Some(kind), "{at}");
                assert!(!f.files.contains_key(&with_suffix(&p, TMP_SUFFIX)), "{at}: temp file left");
                let now = f.files[&p].clone();
                // before the rename: the old file; only the directory fsync comes after it
                let renamed = tier == Tier::Durable && fail_at == steps;
                assert_eq!(now, if renamed { b"new".to_vec() } else { b"old".to_vec() }, "{at}");
            }
        }
        // and with nothing failing, every step ran once
        let mut f = Faulty { fail_at: 0, kind: io::ErrorKind::Other, steps: 0, files: Default::default() };
        write_with(&mut f, &p, b"new", tier).unwrap();
        assert_eq!(f.steps, steps);
    }
}

// ── F-1 on real filesystems (containers: crates/luyaz-persist/fault.sh) ─────────

/// A directory from the environment, or skip (these need a container set up for them).
fn real(var: &str) -> Option<PathBuf> {
    let d = std::env::var_os(var).map(PathBuf::from);
    if d.is_none() {
        eprintln!("skipped: {var} not set (crates/luyaz-persist/fault.sh runs it)");
    }
    d
}

/// A real write to `dir` fails with `kind`, leaves `old` as it was and no temp file.
fn real_failure(dir: &Path, kind: io::ErrorKind, has_old: bool) {
    let p = dir.join("state.json");
    for tier in [Tier::Cheap, Tier::Durable] {
        let before = fs::read(&p).ok();
        let e = write(&p, &vec![b'x'; 256 * 1024], tier).unwrap_err();
        assert_eq!(e.kind(), kind, "{tier:?}: {e}");
        assert_eq!(fs::read(&p).ok(), before, "{tier:?}: the old file changed");
        assert_eq!(before.is_some(), has_old);
        assert!(!with_suffix(&p, TMP_SUFFIX).exists(), "{tier:?}: temp file left");
        // reading what is there still works and says it may be saved over only when it read
        assert!(matches!(load(&p, text), Load::Ok(_)) == has_old);
    }
}

#[test]
fn real_enospc() {
    // a tiny tmpfs with a small state file already in it
    let Some(d) = real("LUYAZ_PERSIST_ENOSPC") else { return };
    real_failure(&d, io::ErrorKind::StorageFull, true);
}

#[test]
fn real_erofs() {
    // a read-only bind mount (what `errors=remount-ro` makes of /data), even for root
    let Some(d) = real("LUYAZ_PERSIST_EROFS") else { return };
    real_failure(&d, io::ErrorKind::ReadOnlyFilesystem, true);
}

#[test]
fn real_eacces() {
    // a directory the test user may not write (run as nobody)
    let Some(d) = real("LUYAZ_PERSIST_EACCES") else { return };
    real_failure(&d, io::ErrorKind::PermissionDenied, true);
}
