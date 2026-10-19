//! Bearer-token login (moved from zte-agent `src/auth.rs`, Phase 4 step 8a): the password's
//! SHA-256, tokens for an hour. luyaz-web and the core's touch compatibility layer use this one copy.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

/// Simple bearer-token auth.
/// Password is the same as the router web UI admin password.
/// Stored as SHA-256 of the plaintext password (uppercase hex).
pub struct AuthState {
    /// SHA256(password) uppercase hex — stored so we can verify login attempts.
    password_hash: Mutex<Option<String>>,
    /// Active bearer tokens mapped to expiry timestamps.
    tokens: Mutex<Vec<Token>>,
}

struct Token {
    value: String,
    expires: u64,
}

const TOKEN_TTL_SECS: u64 = 3600; // 1 hour

impl Default for AuthState {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthState {
    pub fn new() -> Self {
        Self {
            password_hash: Mutex::new(None),
            tokens: Mutex::new(Vec::new()),
        }
    }

    /// Set the admin password (called once at startup from env or config).
    pub fn set_password(&self, password: &str) {
        let hash = format!("{:X}", Sha256::digest(password.as_bytes()));
        *self.password_hash.lock().unwrap() = Some(hash);
    }

    /// Check if a password has been configured.
    pub fn has_password(&self) -> bool {
        self.password_hash.lock().unwrap().is_some()
    }

    /// Attempt login with plaintext password. Returns bearer token on success.
    pub fn login(&self, password: &str) -> Option<String> {
        let stored = self.password_hash.lock().unwrap();
        let ok = stored.as_deref().is_some_and(|s| hash_matches(s, password));
        if !ok {
            return None;
        }
        drop(stored);
        Some(self.issue())
    }

    /// A fresh random token for an hour. Login uses it after the password checks out; a caller
    /// inside the process (u60d's scenario engine calling its own routes) uses it directly.
    pub fn issue(&self) -> String {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        // random, not derived from anything a caller could know
        let mut seed = [0u8; 16];
        let read = std::fs::File::open("/dev/urandom").and_then(|mut f| std::io::Read::read_exact(&mut f, &mut seed));
        if read.is_err() {
            let material = format!("internal{now}{:?}{}", std::time::Instant::now(), std::process::id());
            seed.copy_from_slice(&Sha256::digest(material.as_bytes())[..16]);
        }
        let token = hex::encode(seed);
        let mut tokens = self.tokens.lock().unwrap_or_else(|e| e.into_inner());
        tokens.retain(|t| t.expires > now);
        tokens.push(Token { value: token.clone(), expires: now + TOKEN_TTL_SECS });
        token
    }

    /// Validate a bearer token.
    pub fn validate(&self, token: &str) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let tokens = self.tokens.lock().unwrap();
        tokens.iter().any(|t| t.value == token && t.expires > now)
    }
}

/// `attempt` against the stored SHA-256 (uppercase hex), in constant time: the comparison doesn't
/// stop at the first differing byte.
fn hash_matches(stored: &str, attempt: &str) -> bool {
    let hash = format!("{:X}", Sha256::digest(attempt.as_bytes()));
    stored.len() == hash.len() && stored.bytes().zip(hash.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

/// `attempt` against the plaintext admin password, compared as their hashes (constant time).
/// luyaz-web checks its login this way against the password file (u60-web.md 8d-1a).
pub fn password_matches(password: &str, attempt: &str) -> bool {
    hash_matches(&format!("{:X}", Sha256::digest(password.as_bytes())), attempt)
}

/// The admin password file (`/data/luyaz/web.env`;
/// written by web-auth.sh, mode 600): one line
/// `LUYAZ_WEB_PASSWORD=<value>`, the value taken literally to the end of the line (no shell
/// quoting). Empty counts as missing — "" would be accepted as a real password.
pub fn password_from_env_file(text: &str) -> Option<&str> {
    let key = |k: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(k))
            .map(|v| v.trim_end_matches('\r'))
            .filter(|v| !v.is_empty())
    };
    key("LUYAZ_WEB_PASSWORD=")
}

mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
    }
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
