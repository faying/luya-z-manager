//! Login for the admin password: one shared copy for luyaz-web and the core's touch layer.
//!
//! The password is kept only as its SHA-256 digest. A successful login hands out a random
//! 128-bit session token (32 hex characters) that stays valid for an hour.

use std::collections::HashMap;
use std::io::Read;
use std::sync::{Mutex, MutexGuard};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

/// How long a session token lives, in seconds.
const SESSION_SECS: u64 = 60 * 60;

type Digest32 = [u8; 32];

/// The configured password (as a digest) and the sessions handed out so far.
#[derive(Default)]
pub struct AuthState {
    secret: Mutex<Option<Digest32>>,
    /// token → unix time it stops working
    sessions: Mutex<HashMap<String, u64>>,
}

impl AuthState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Configure the admin password; until this is called every login fails.
    pub fn set_password(&self, password: &str) {
        *lock(&self.secret) = Some(digest(password));
    }

    pub fn has_password(&self) -> bool {
        lock(&self.secret).is_some()
    }

    /// A new session token if `password` is the configured one.
    pub fn login(&self, password: &str) -> Option<String> {
        let secret = (*lock(&self.secret))?;
        same(&secret, &digest(password)).then(|| self.issue())
    }

    /// Mint a session token without a password check. Used after `login` succeeds and by code
    /// in the same process that calls its own protected routes.
    pub fn issue(&self) -> String {
        let now = unix_now();
        let token = to_hex(&random16());
        let mut sessions = lock(&self.sessions);
        sessions.retain(|_, until| *until > now);
        sessions.insert(token.clone(), now + SESSION_SECS);
        token
    }

    /// Whether `token` was issued here and has not expired.
    pub fn validate(&self, token: &str) -> bool {
        let now = unix_now();
        lock(&self.sessions)
            .get(token)
            .is_some_and(|until| *until > now)
    }
}

/// Whether `attempt` equals the plaintext `password`, comparing digests in constant time.
pub fn password_matches(password: &str, attempt: &str) -> bool {
    same(&digest(password), &digest(attempt))
}

/// The password from the text of `/data/luyaz/web.env` (written by web-auth.sh, mode 600).
/// The line is `LUYAZ_WEB_PASSWORD=<value>`; the value runs literally to the end of the line,
/// without shell quoting. An empty value means no password, never the empty password.
pub fn password_from_env_file(text: &str) -> Option<&str> {
    text.lines()
        .filter_map(|line| line.strip_prefix("LUYAZ_WEB_PASSWORD="))
        .map(|value| value.strip_suffix('\r').unwrap_or(value))
        .find(|value| !value.is_empty())
}

fn digest(text: &str) -> Digest32 {
    Sha256::digest(text.as_bytes()).into()
}

/// Constant-time equality: every byte is looked at whatever the earlier ones were.
fn same(a: &Digest32, b: &Digest32) -> bool {
    a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

/// 16 bytes from the kernel's random source; if that can't be read, a hash of the clock and
/// pid (still nothing a client can predict from outside).
fn random16() -> [u8; 16] {
    let mut out = [0u8; 16];
    let read = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut out));
    if read.is_err() {
        let seed = format!("{:?}/{}/{}", Instant::now(), unix_now(), std::process::id());
        out.copy_from_slice(&digest(&seed)[..16]);
    }
    out
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A poisoned lock still holds usable data here, so take it instead of panicking.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_checks_password_and_tokens_are_random() {
        let a = AuthState::new();
        assert!(a.login("x").is_none(), "no password set");
        a.set_password("secret");
        assert!(a.login("Secret").is_none());
        assert!(a.login("").is_none());
        let t1 = a.login("secret").unwrap();
        let t2 = a.login("secret").unwrap();
        assert_eq!(t1.len(), 32);
        assert_ne!(t1, t2, "two logins in the same second get different tokens");
        assert!(a.validate(&t1) && a.validate(&t2));
        assert!(!a.validate("00000000000000000000000000000000"));
    }

    #[test]
    fn password_file_and_match() {
        assert_eq!(password_from_env_file("LUYAZ_WEB_PASSWORD=s3cr3t!#\n"), Some("s3cr3t!#"));
        assert_eq!(password_from_env_file("X=1\r\nLUYAZ_WEB_PASSWORD=a b\r\n"), Some("a b"));
        assert_eq!(password_from_env_file("LUYAZ_WEB_PASSWORD=\n"), None);
        // the old key went after F7 rewrote the device's web.env
        assert_eq!(password_from_env_file("ZTE_AGENT_PASSWORD=old\n"), None);
        assert_eq!(password_from_env_file("ZTE_AGENT_PASSWORD=old\nLUYAZ_WEB_PASSWORD=new\n"), Some("new"));
        assert_eq!(password_from_env_file(""), None);
        assert!(password_matches("secret", "secret"));
        assert!(!password_matches("secret", "Secret") && !password_matches("secret", ""));
    }
}
