//! F1 (ext-1031): SHA-256 of key stock binaries, computed at most once per boot and only
//! when the firmware version string or a file's size/mtime changed. The version string only
//! selects the cache entry; capabilities are still decided by runtime probes, never by it.
//! Unreadable files are left out. Never reads partitions or block devices.
use std::{
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

use luyaz_api::firmware::Component;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Stock components whose identity matters for adapter decisions (paths checked against
/// docs/research/ext-1031-fw-adapter-esim.md and the device process list).
pub(crate) const PATHS: &[(&str, &str)] = &[
    ("zte_topsw_mdm", "/usr/bin/zte_topsw_mdm"),
    ("zte_topsw_daemon", "/usr/bin/zte_topsw_daemon"),
    ("diag-router", "/usr/bin/diag-router"),
    ("ipacm", "/usr/bin/ipacm"),
    ("qcrilnr", "/usr/bin/qcrilnr"),
];

pub(crate) const CACHE_FILE: &str = "stock-components.json";
const DEFAULT_DIR: &str = "/data/luyaz/core";
/// Larger files are skipped rather than hashed (these binaries are a few MB at most).
const MAX_BYTES: u64 = 64 << 20;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Entry {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub mtime: i64,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Manifest {
    pub schema: u32,
    pub firmware_version: String,
    pub components: Vec<Entry>,
}

fn stat(path: &Path) -> Option<(u64, i64)> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(path).ok()?;
    m.is_file().then(|| (m.len(), m.mtime()))
}

fn hash(path: &Path) -> Option<String> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > MAX_BYTES {
            return None;
        }
        h.update(&buf[..n]);
    }
    Some(format!("{:x}", h.finalize()))
}

/// Reuse the cache when the version and every file's size+mtime still match; otherwise
/// hash again and rewrite the cache atomically. `root` prefixes the absolute paths (tests).
pub(crate) fn load_or_compute(
    dir: &Path,
    root: &Path,
    version: &str,
    paths: &[(&str, &str)],
) -> Manifest {
    let cache = dir.join(CACHE_FILE);
    let on_disk = |p: &str| root.join(p.trim_start_matches('/'));
    if let Some(m) = std::fs::read(&cache)
        .ok()
        .and_then(|b| serde_json::from_slice::<Manifest>(&b).ok())
        && m.schema == 1
        && m.firmware_version == version
        && m.components.len()
            == paths
                .iter()
                .filter(|(_, p)| stat(&on_disk(p)).is_some())
                .count()
        && m.components
            .iter()
            .all(|e| stat(&on_disk(&e.path)) == Some((e.size, e.mtime)))
    {
        return m;
    }
    let components = paths
        .iter()
        .filter_map(|(name, p)| {
            let file = on_disk(p);
            let (size, mtime) = stat(&file)?;
            let sha256 = hash(&file)?;
            // A file replaced while hashing is left for the next boot rather than mislabelled.
            (stat(&file) == Some((size, mtime))).then(|| Entry {
                name: (*name).into(),
                path: (*p).into(),
                size,
                mtime,
                sha256,
            })
        })
        .collect();
    let m = Manifest {
        schema: 1,
        firmware_version: version.into(),
        components,
    };
    if let Ok(bytes) = serde_json::to_vec_pretty(&m) {
        let tmp = dir.join(format!("{CACHE_FILE}.tmp"));
        if std::fs::create_dir_all(dir).is_ok() && std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(&tmp, &cache);
        }
    }
    m
}

pub(crate) fn components(m: &Manifest) -> Vec<Component> {
    m.components
        .iter()
        .map(|e| Component {
            name: format!("stock:{}", e.name),
            version: Some(m.firmware_version.clone()),
            commit: None,
            hash: Some(format!("sha256:{}", e.sha256)),
        })
        .collect()
}

/// Once per process: wait (cheaply, memory only) until the firmware version is known, then
/// hash on a blocking thread. Firmware only changes across a reboot, so there is no re-run.
pub(crate) fn spawn(hub: std::sync::Arc<crate::block::Hub>) {
    tokio::spawn(async move {
        let dir =
            PathBuf::from(std::env::var("LUYAZ_CORE_DIR").unwrap_or_else(|_| DEFAULT_DIR.into()));
        // ~30 minutes at most; identity is normally read within the first rounds.
        for _ in 0..60 {
            tokio::time::sleep(Duration::from_secs(30)).await;
            let Some(version) = hub.firmware().firmware_key() else {
                continue;
            };
            let m = tokio::task::spawn_blocking(move || {
                load_or_compute(&dir, Path::new("/"), &version, PATHS)
            })
            .await;
            if let Ok(m) = m {
                hub.firmware().set_stock(components(&m));
            }
            return;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn setup(tag: &str) -> (Tmp, PathBuf, PathBuf) {
        let t = std::env::temp_dir().join(format!("core-stock-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        let root = t.join("root");
        let dir = t.join("state");
        std::fs::create_dir_all(root.join("usr/bin")).unwrap();
        std::fs::write(root.join("usr/bin/a"), b"abc").unwrap();
        (Tmp(t), root, dir)
    }

    const P: &[(&str, &str)] = &[("a", "/usr/bin/a"), ("missing", "/usr/bin/missing")];

    #[test]
    fn stock_hash_omits_missing_and_caches_by_version() {
        let (_t, root, dir) = setup("cache");
        let m = load_or_compute(&dir, &root, "V1", P);
        assert_eq!(m.components.len(), 1);
        assert_eq!(
            m.components[0].sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(dir.join(CACHE_FILE).exists());
        // Cache hit: a forged cached hash survives while version and size/mtime match.
        let mut forged = m.clone();
        forged.components[0].sha256 = "cached".into();
        std::fs::write(dir.join(CACHE_FILE), serde_json::to_vec(&forged).unwrap()).unwrap();
        assert_eq!(
            load_or_compute(&dir, &root, "V1", P).components[0].sha256,
            "cached"
        );
        // A different firmware version string forces a recompute.
        let again = load_or_compute(&dir, &root, "V2", P);
        assert_eq!(again.firmware_version, "V2");
        assert_ne!(again.components[0].sha256, "cached");
    }

    #[test]
    fn stock_hash_recomputes_when_file_changes() {
        let (_t, root, dir) = setup("change");
        let first = load_or_compute(&dir, &root, "V1", P);
        std::fs::write(root.join("usr/bin/a"), b"abcd").unwrap();
        let second = load_or_compute(&dir, &root, "V1", P);
        assert_ne!(first.components[0].sha256, second.components[0].sha256);
        // A newly appearing file also invalidates the cache.
        std::fs::write(root.join("usr/bin/missing"), b"x").unwrap();
        assert_eq!(load_or_compute(&dir, &root, "V1", P).components.len(), 2);
    }

    #[test]
    fn stock_components_map_to_profile_entries() {
        let (_t, root, dir) = setup("map");
        let c = components(&load_or_compute(&dir, &root, "V1", P));
        assert_eq!(c[0].name, "stock:a");
        assert!(c[0].hash.as_deref().unwrap().starts_with("sha256:ba78"));
    }
}
