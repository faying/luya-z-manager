use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

/// Simple bearer-token auth.
/// Password is the same as the router web UI admin password.
/// Hash scheme: double-SHA256 matching the ZTE web UI.
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
        let hash = format!("{:X}", Sha256::digest(password.as_bytes()));
        let stored = self.password_hash.lock().unwrap();
        if stored.as_deref() != Some(&hash) {
            return None;
        }
        drop(stored);

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let token_bytes: [u8; 16] = {
            // Simple token: SHA256(password_hash + timestamp)
            let material = format!("{hash}{now}");
            let digest = Sha256::digest(material.as_bytes());
            let mut arr = [0u8; 16];
            arr.copy_from_slice(&digest[..16]);
            arr
        };
        let token = hex::encode(token_bytes);

        let mut tokens = self.tokens.lock().unwrap();
        // Prune expired
        tokens.retain(|t| t.expires > now);
        tokens.push(Token {
            value: token.clone(),
            expires: now + TOKEN_TTL_SECS,
        });
        Some(token)
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

mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}
