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

/// `GET /v2/versions`: every component with what is installed, the newest known, and whether
/// that is newer.
pub fn report() -> Value {
    let l = latest().lock().unwrap().clone();
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
            json!({"name": name, "installed": cur, "latest": new, "update": update})
        })
        .collect();
    let any = comps.iter().any(|c| c["update"] == true);
    json!({
        "components": comps,
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
    report()
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
    fn latest_json() {
        let ok = parse_latest(r#"{"v":1,"components":{"core":"1.1.0","touch":"x","lweb":2}}"#);
        assert_eq!(ok.unwrap().into_iter().collect::<Vec<_>>(), [("core".into(), "1.1.0".into())]);
        assert!(parse_latest("{}").is_err());
        assert!(parse_latest("<html>").is_err());
    }
}
