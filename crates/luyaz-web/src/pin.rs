//! Wrong-device guard (R18-P0-04, docs/designs/remote-path-guard.md): every page carries this
//! luyaz-web's own device ID as the Datastar signal `luyazdev`, Datastar sends it with every
//! `@post`, and the gate refuses a write whose page was opened on another device (the same
//! address now reaching another U60). The ID is random (no device data), kept in a file beside the
//! sessions; no file and none can be made → no pin (logged), everything else as before.
//!
//! Small in practice: sessions are per device already, so a swapped device usually sends the old
//! tab to the login page first. This covers what is left (a copied session file, a reused name).

use std::path::Path;
use std::sync::OnceLock;

/// Where the device keeps it (with a password file, like `/data/luyaz/web-sessions`).
pub const FILE: &str = "/data/luyaz/web-device-id";

static ID: OnceLock<String> = OnceLock::new();

/// Read the ID, or make one; call once at start.
pub fn init(path: &Path) {
    let id = load(path).unwrap_or_else(|e| {
        eprintln!("[luyaz-web] device pin off: {}: {e}", path.display());
        String::new()
    });
    let _ = ID.set(id);
}

fn valid(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn load(path: &Path) -> std::io::Result<String> {
    if let Ok(s) = std::fs::read_to_string(path) {
        let s = s.trim();
        if valid(s) {
            return Ok(s.to_ascii_lowercase());
        }
    }
    let mut b = [0u8; 16];
    std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut b)?;
    let id: String = b.iter().map(|x| format!("{x:02x}")).collect();
    // a temporary name and a rename: never a half-written ID
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, format!("{id}\n"))?;
    std::fs::rename(&tmp, path)?;
    Ok(id)
}

/// This device's ID; empty = no pin.
pub fn id() -> &'static str {
    ID.get().map(String::as_str).unwrap_or("")
}

/// A Datastar POST body (the page's signals as JSON) names another device. No body, not JSON, or
/// no `luyazdev` (a page from before the pin, a form) is not a mismatch.
pub fn other_device(mine: &str, body: &[u8]) -> bool {
    if mine.is_empty() {
        return false;
    }
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("luyazdev")?
                .as_str()
                .map(|s| !s.eq_ignore_ascii_case(mine))
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mismatch_only_when_named_and_different() {
        let me = "0123456789abcdef0123456789abcdef";
        assert!(other_device(
            me,
            br#"{"luyazdev":"ffffffffffffffffffffffffffffffff","pin":""}"#
        ));
        assert!(!other_device(
            me,
            br#"{"luyazdev":"0123456789ABCDEF0123456789abcdef"}"#
        ));
        assert!(
            !other_device(me, br#"{"pin":"1234"}"#),
            "no signal: an old page"
        );
        assert!(!other_device(me, b""), "no body");
        assert!(!other_device(me, b"user=a&pw=b"), "a form");
        assert!(!other_device("", br#"{"luyazdev":"ff"}"#), "pin off");
    }

    #[test]
    fn made_once_then_kept() {
        let dir = std::env::temp_dir().join(format!("luyaz-pin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("web-device-id");
        let a = load(&f).unwrap();
        assert!(valid(&a));
        assert_eq!(load(&f).unwrap(), a, "kept across restarts");
        std::fs::write(&f, "junk").unwrap();
        assert_ne!(load(&f).unwrap(), "junk", "a broken file is replaced");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
