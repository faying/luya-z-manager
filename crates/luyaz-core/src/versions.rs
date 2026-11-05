//! Component versions (docs/designs/component-versions.md): what is installed on the device,
//! and the newest the public repo lists in its `versions.json`.
//!
//! Installed: the core's own version; luyaz-web, the sentinel, the touch UI (and the phone
//! bridge's two programs) carry a
//! `LUYAZ_VERSION=<x.y.z>;` marker in their binary, found by reading the file (never by running
//! it: an old touch UI started with an argument it does not know would open the screen); the
//! device scripts have `/data/luyaz/scripts/VERSION`. None of those (a build from before
//! 1.0.0): no version.
//!
//! Newest: one GET of [`LATEST_URL`] a day (the owner chose it, 2026-10-17), skipped while the
//! device is truly roaming; `POST /v2/versions/check` asks now, roaming or not.
//! `LUYAZ_VERSION_CHECK=0` turns the daily check off. The answer is kept in
//! `/data/luyaz/core/versions-latest.json`, so a restart shows the last one at once.
//!
//! Integrity (OTA-05): the same report says whether the running program, the file on disk and
//! the last ship's manifest are the same build, what the ship transaction is doing and which
//! builds are still being watched. Md5s are computed once per file identity, and only when
//! someone asks. `components` keeps its entries short: a touch UI before 1.29.0 reads at most
//! 256 bytes of one and 2048 of the list, so the md5s sit in a separate `integrity` list.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const LATEST_URL: &str =
    "https://raw.githubusercontent.com/faying/luya-z-manager/main/versions.json";
const CHECK_EVERY: Duration = Duration::from_secs(24 * 3600);
/// After a failed check, try again sooner than a day.
const RETRY_AFTER: Duration = Duration::from_secs(3600);
const FIRST_AFTER: Duration = Duration::from_secs(120);
const TIMEOUT: Duration = Duration::from_secs(20);
const CACHE: &str = "/data/luyaz/core/versions-latest.json";
const MARK: &[u8] = b"LUYAZ_VERSION=";
const CORE_PATH: &str = "/data/luyaz/luyaz-core";
const SHIP_TXN: &str = "/data/luyaz/ship/txn";
const SHIP_MANIFEST: &str = "/data/luyaz/ship/manifest.jsonl";
const SHIP_WATCH: &str = "/data/luyaz/ship/watch";

/// (name, where its version is): the order the screen and the web list them in.
const COMPONENTS: &[(&str, Source)] = &[
    ("core", Source::Own),
    ("lweb", Source::Binary("/data/luyaz/luyaz-web")),
    ("sentinel", Source::Binary("/data/luyaz/luyaz-sentinel")),
    ("touch", Source::Binary("/data/luyaz/ui/luyaz-ui")),
    ("scripts", Source::Text("/data/luyaz/scripts/VERSION")),
];

#[derive(Clone, Copy)]
enum Source {
    Own,
    Binary(&'static str),
    Text(&'static str),
}

/// The last check's result, as kept in [`CACHE`].
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Latest {
    /// Unix seconds of the last attempt (0: never).
    pub checked_at: i64,
    /// Unix seconds of the last answer that parsed (0: never).
    pub ok_at: i64,
    pub components: BTreeMap<String, String>,
    /// Why the last attempt failed (empty: it worked).
    pub error: String,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn latest() -> &'static Mutex<Latest> {
    static L: OnceLock<Mutex<Latest>> = OnceLock::new();
    L.get_or_init(|| {
        let kept = std::fs::read(crate::state::host_path(CACHE))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Mutex::new(kept)
    })
}

/// `x.y.z` as numbers (anything else: None).
pub fn parse(v: &str) -> Option<(u64, u64, u64)> {
    let mut it = v.trim().split('.').map(|p| p.parse::<u64>().ok());
    let r = (it.next()??, it.next()??, it.next()??);
    it.next().is_none().then_some(r)
}

/// The version in a `LUYAZ_VERSION=<x.y.z>;` marker in a binary's bytes. The `;` is needed:
/// Rust strings have no terminator, and the bytes after the marker can be digits (the commit).
pub fn marker(bytes: &[u8]) -> Option<String> {
    let at = bytes.windows(MARK.len()).position(|w| w == MARK)? + MARK.len();
    let tail = &bytes[at..bytes.len().min(at + 32)];
    let end = tail
        .iter()
        .position(|b| !(b.is_ascii_digit() || *b == b'.'))
        .unwrap_or(tail.len());
    if tail.get(end) != Some(&b';') {
        return None;
    }
    let v = std::str::from_utf8(&tail[..end]).ok()?;
    parse(v).map(|_| v.to_string())
}

/// Binaries are read once per (size, mtime): a few MB each, and they change only on a ship.
fn binary_version(path: &str) -> Option<String> {
    type Seen = BTreeMap<String, ((u64, i64), Option<String>)>;
    static SEEN: OnceLock<Mutex<Seen>> = OnceLock::new();
    let path = crate::state::host_path(path);
    let meta = std::fs::metadata(&path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);
    let key = (meta.len(), mtime);
    let seen = SEEN.get_or_init(Default::default);
    if let Some((k, v)) = seen.lock().unwrap().get(&path)
        && *k == key
    {
        return v.clone();
    }
    let v = std::fs::read(&path).ok().and_then(|b| marker(&b));
    seen.lock().unwrap().insert(path, (key, v.clone()));
    v
}

fn installed(src: Source) -> Option<String> {
    match src {
        Source::Own => Some(env!("CARGO_PKG_VERSION").to_string()),
        Source::Binary(p) => binary_version(p),
        Source::Text(p) => std::fs::read_to_string(crate::state::host_path(p))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|v| parse(v).is_some()),
    }
}

fn hex(d: [u8; 16]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// A file's md5, computed once per identity: a replaced program is a new inode, so the program
/// a process still runs and the file on disk never share an entry unless they are one file.
fn cached_md5(path: &std::path::Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    type Seen = BTreeMap<(u64, u64, u64, i64), String>;
    static SEEN: OnceLock<Mutex<Seen>> = OnceLock::new();
    let m = std::fs::metadata(path).ok()?;
    let key = (m.dev(), m.ino(), m.len(), m.mtime());
    let seen = SEEN.get_or_init(Default::default);
    if let Some(v) = seen.lock().unwrap().get(&key) {
        return Some(v.clone());
    }
    let v = hex(crate::crashwatch::md5_file(path)?);
    seen.lock().unwrap().insert(key, v.clone());
    Some(v)
}

/// The md5 of what a process started from `path` runs: found by the exe link, not by name, so a
/// bypass copy under another name is not taken for it. A replaced file reads `path (deleted)`,
/// and `/proc/<pid>/exe` still opens the old program.
fn running_md5(path: &str) -> Option<String> {
    for e in std::fs::read_dir("/proc").ok()?.flatten() {
        let name = e.file_name();
        if !name.to_string_lossy().bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let exe = e.path().join("exe");
        let Ok(link) = std::fs::read_link(&exe) else { continue };
        let link = link.to_string_lossy();
        if link.strip_suffix(" (deleted)").unwrap_or(&link) == path {
            return cached_md5(&exe);
        }
    }
    None
}

/// The manifest's picture: component → path → md5, the last `ship` or `kit` line of a component
/// being what it holds (docs SHIP.md). The core's old component name `datad` counts as `core`.
pub fn parse_manifest(text: &str) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        // cheap filter: the file is hundreds of KB, mostly `record` lines
        if !line.contains("\"kind\":\"ship\"") && !line.contains("\"kind\":\"kit\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let Some(comp) = v["comp"].as_str() else { continue };
        let comp = if comp == "datad" { "core" } else { comp };
        let files = v["files"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|f| Some((f["path"].as_str()?.to_string(), f["md5"].as_str()?.to_string())))
            .collect();
        out.insert(comp.to_string(), files);
    }
    out
}

/// The manifest, parsed again only when the file changed.
fn manifest() -> BTreeMap<String, BTreeMap<String, String>> {
    type Kept = Option<((u64, i64), BTreeMap<String, BTreeMap<String, String>>)>;
    static KEPT: Mutex<Kept> = Mutex::new(None);
    use std::os::unix::fs::MetadataExt;
    let path = crate::state::host_path(SHIP_MANIFEST);
    let Ok(m) = std::fs::metadata(&path) else { return BTreeMap::new() };
    let key = (m.len(), m.mtime());
    let mut kept = KEPT.lock().unwrap();
    if let Some((k, v)) = kept.as_ref()
        && *k == key
    {
        return v.clone();
    }
    let v = parse_manifest(&std::fs::read_to_string(&path).unwrap_or_default());
    *kept = Some((key, v.clone()));
    v
}

/// The transaction log's summary. Null without one. A log with no phase yet is being staged.
pub fn parse_txn(text: &str) -> Value {
    let field = |k: &str| {
        text.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix('=')).unwrap_or("")
    };
    if field("txn").is_empty() {
        return Value::Null;
    }
    let phase = field("phase");
    let terminal = matches!(phase, "done" | "rolledback" | "aborted" | "manifest_pending" | "failed");
    json!({"txn": field("txn"), "comp": field("comp"), "phase": phase,
           "reason": field("reason"), "active": !terminal})
}

/// (restart needed, disk differs from the manifest). While a transaction is moving files both
/// are expected, so neither is claimed. A program not running needs no restart: the touch UI is
/// legitimately stopped while the stock screen has the display.
pub fn compare(
    running: Option<&str>,
    disk: Option<&str>,
    manifest: Option<&str>,
    active: bool,
) -> (bool, bool) {
    if active {
        return (false, false);
    }
    let restart = matches!((running, disk), (Some(r), Some(d)) if r != d);
    let drift = matches!((disk, manifest), (Some(d), Some(m)) if d != m);
    (restart, drift)
}

/// Builds still on watch after they were promoted (a file per transaction, removed by the
/// sentinel once the build has proved itself). `<date>-<time>-<component>`.
fn watching() -> Vec<Value> {
    let mut ids: Vec<String> = std::fs::read_dir(crate::state::host_path(SHIP_WATCH))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    ids.sort();
    ids.into_iter()
        .map(|id| {
            let comp = id.rsplit('-').next().unwrap_or("").to_string();
            json!({"txn": id, "comp": comp})
        })
        .collect()
}

/// `GET /v2/versions`: every component with what is installed, the newest known, and whether
/// that is newer; whether what runs is what is on disk and in the manifest. May read programs
/// for their md5 the first time: call it off the async workers.
pub fn report() -> Value {
    let l = latest().lock().unwrap().clone();
    let ship = std::fs::read_to_string(crate::state::host_path(SHIP_TXN))
        .map_or(Value::Null, |t| parse_txn(&t));
    let active = ship["active"] == true;
    let recorded = manifest();
    let mut integrity = Vec::new();
    let comps: Vec<Value> = COMPONENTS
        .iter()
        .map(|(name, src)| {
            let cur = installed(*src);
            let new = l.components.get(*name).cloned();
            let update = match (cur.as_deref().and_then(parse), new.as_deref().and_then(parse)) {
                (Some(c), Some(n)) => n > c,
                // a build from before 1.0.0 against a numbered release
                (None, Some(_)) => true,
                _ => false,
            };
            let (path, runs) = match src {
                Source::Own => (CORE_PATH, true),
                Source::Binary(p) => (*p, true),
                Source::Text(p) => (*p, false),
            };
            let on_disk = crate::state::host_path(path);
            let disk = cached_md5(std::path::Path::new(&on_disk));
            let running = match src {
                Source::Own => cached_md5(std::path::Path::new("/proc/self/exe")),
                _ if runs => running_md5(&on_disk),
                _ => None,
            };
            let listed = recorded.get(*name).and_then(|f| f.get(path)).cloned();
            let (restart, drift) =
                compare(running.as_deref(), disk.as_deref(), listed.as_deref(), active);
            integrity.push(json!({"name": name, "running_md5": running, "disk_md5": disk,
                                  "manifest_md5": listed}));
            json!({"name": name, "installed": cur, "latest": new, "update": update,
                   "restart_needed": restart, "drift": drift})
        })
        .collect();
    let any = comps.iter().any(|c| c["update"] == true);
    json!({
        "components": comps,
        "integrity": integrity,
        "ship": ship,
        "watching": watching(),
        "update_available": any,
        "checked_at": (l.checked_at > 0).then_some(l.checked_at),
        "ok_at": (l.ok_at > 0).then_some(l.ok_at),
        "error": (!l.error.is_empty()).then_some(l.error),
        "source": LATEST_URL,
    })
}

/// The `components` of a `versions.json`, only entries that are versions.
pub fn parse_latest(body: &str) -> Result<BTreeMap<String, String>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| format!("看不懂返回：{e}"))?;
    let comps = v
        .get("components")
        .and_then(Value::as_object)
        .ok_or("返回里没有 components")?;
    let out: BTreeMap<String, String> = comps
        .iter()
        .filter_map(|(k, v)| {
            let s = v.as_str()?;
            parse(s).map(|_| (k.clone(), s.to_string()))
        })
        .collect();
    if out.is_empty() {
        return Err("返回里没有版本号".into());
    }
    Ok(out)
}

async fn fetch() -> Result<BTreeMap<String, String>, String> {
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        // IPv4 only: on the device the IPv6 route to GitHub hangs (measured 10-17) while IPv4 works
        .local_address(std::net::IpAddr::from(std::net::Ipv4Addr::UNSPECIFIED))
        .user_agent(concat!("luyaz-core/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;
    let r = client
        .get(LATEST_URL)
        .send()
        .await
        .map_err(|e| format!("连不上：{e}"))?;
    if !r.status().is_success() {
        return Err(format!("HTTP {}", r.status().as_u16()));
    }
    let body = r.text().await.map_err(|e| e.to_string())?;
    if body.len() > 16 * 1024 {
        return Err("返回太大".into());
    }
    parse_latest(&body)
}

/// One check: fetch, keep the answer (a failure keeps the last good versions).
pub async fn check() -> Value {
    let got = fetch().await;
    let kept = {
        let mut l = latest().lock().unwrap();
        l.checked_at = now();
        match got {
            Ok(c) => {
                l.components = c;
                l.ok_at = l.checked_at;
                l.error.clear();
            }
            Err(e) => l.error = e,
        }
        l.clone()
    };
    if let Ok(b) = serde_json::to_vec(&kept) {
        let path = crate::state::host_path(CACHE);
        let tmp = format!("{path}.tmp");
        if std::fs::write(&tmp, b).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
    tokio::task::spawn_blocking(report).await.unwrap_or(Value::Null)
}

pub fn enabled() -> bool {
    std::env::var("LUYAZ_VERSION_CHECK").map_or(true, |v| v != "0")
}

/// The daily check: two minutes after start, then once a day after the last attempt (an hour
/// after a failed one); a roaming device waits for the next turn.
pub fn spawn(app: crate::server::App) {
    tokio::spawn(async move {
        tokio::time::sleep(FIRST_AFTER).await;
        loop {
            let l = latest().lock().unwrap().clone();
            let due = if l.checked_at == 0 {
                Duration::ZERO
            } else {
                let every = if l.error.is_empty() { CHECK_EVERY } else { RETRY_AFTER };
                let since = Duration::from_secs((now() - l.checked_at).max(0) as u64);
                every.saturating_sub(since)
            };
            tokio::time::sleep(due).await;
            if roaming(&app).await {
                tokio::time::sleep(RETRY_AFTER).await;
                continue;
            }
            check().await;
        }
    });
}

async fn roaming(app: &crate::server::App) -> bool {
    let snap = app.snapshot().await;
    snap.fields
        .get("signal")
        .and_then(|s| s.get("true_roaming"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_and_versions() {
        assert_eq!(marker(b"xx\0LUYAZ_VERSION=1.2.3;yy").as_deref(), Some("1.2.3"));
        // no terminator: the commit right after it would read as part of the version
        assert_eq!(marker(b"LUYAZ_VERSION=1.0.03151cc1"), None);
        assert_eq!(marker(b"LUYAZ_VERSION=1.2;"), None);
        assert_eq!(marker(b"nothing here"), None);
        assert_eq!(parse("1.10.0"), Some((1, 10, 0)));
        assert!(parse("1.10.0") > parse("1.9.9"));
        assert_eq!(parse("1.0.0.0"), None);
    }

    #[test]
    fn manifest_last_line_wins() {
        let m = parse_manifest(concat!(
            r#"{"v":1,"kind":"ship","comp":"datad","files":[{"path":"/a","md5":"11"}]}"#, "\n",
            r#"{"v":1,"kind":"record","name":"rc.local","path":"/etc/rc.local","md5":"22"}"#, "\n",
            "not json \"kind\":\"ship\"\n",
            r#"{"v":1,"kind":"ship","comp":"core","files":[{"path":"/a","md5":"33","prev":"/p"}]}"#, "\n",
            r#"{"v":1,"kind":"kit","comp":"touch","files":[{"path":"/t","md5":"44"}]}"#, "\n",
        ));
        assert_eq!(m["core"]["/a"], "33");
        assert_eq!(m["touch"]["/t"], "44");
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn transaction_phases() {
        assert_eq!(parse_txn(""), Value::Null);
        let t = parse_txn("v=1\ntxn=20261104-032053-core\ncomp=core\nreason=x\nphase=check\n");
        assert_eq!(t["phase"], "check");
        assert_eq!(t["active"], true);
        for p in ["done", "rolledback", "aborted", "manifest_pending", "failed"] {
            assert_eq!(parse_txn(&format!("txn=a\nphase={p}\n"))["active"], false, "{p}");
        }
        assert_eq!(parse_txn("txn=a\n")["active"], true);
    }

    #[test]
    fn comparing_builds() {
        let (a, b) = (Some("a"), Some("b"));
        assert_eq!(compare(a, a, a, false), (false, false));
        assert_eq!(compare(a, b, b, false), (true, false), "file replaced, not restarted");
        assert_eq!(compare(a, a, b, false), (false, true), "file differs from the manifest");
        assert_eq!(compare(a, b, a, true), (false, false), "a transaction is moving files");
        assert_eq!(compare(None, a, a, false), (false, false), "not running");
        assert_eq!(compare(a, a, None, false), (false, false), "not in the manifest");
    }

    #[test]
    fn a_replaced_file_is_not_the_cached_one() {
        let d = std::env::temp_dir().join(format!("versions-md5-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let (f, t) = (d.join("f"), d.join("t"));
        std::fs::write(&f, b"one").unwrap();
        assert_eq!(cached_md5(&f).unwrap(), "f97c5d29941bfb1b2fdab0874906ab82");
        assert_eq!(cached_md5(&f).unwrap(), "f97c5d29941bfb1b2fdab0874906ab82");
        std::fs::write(&t, b"two").unwrap();
        std::fs::rename(&t, &f).unwrap();
        assert_eq!(cached_md5(&f).unwrap(), "b8a9f715dbb64fd5c56e7783c6820a61");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn report_stays_readable_by_an_old_touch_ui() {
        let r = report();
        for c in r["components"].as_array().unwrap() {
            assert!(c.to_string().len() < 256, "{c}");
        }
        assert!(r["components"].to_string().len() < 2048);
        assert!(r.to_string().len() < 4096);
    }

    #[test]
    fn latest_json() {
        let ok = parse_latest(r#"{"v":1,"components":{"core":"1.1.0","touch":"x","lweb":2}}"#);
        assert_eq!(ok.unwrap().into_iter().collect::<Vec<_>>(), [("core".into(), "1.1.0".into())]);
        assert!(parse_latest("{}").is_err());
        assert!(parse_latest("<html>").is_err());
    }
}
