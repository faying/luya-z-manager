//! Login and bearer sessions for the LAN listener.
//!
//! Two jobs live here:
//! - asking the stock web stack (`zwrt_web` over ubus) whether a password or a
//!   stock web token is genuine, and
//! - keeping a small in-memory table of the bearer tokens we hand out after a
//!   successful check.
//!
//! The stock replies are not consistent across firmware builds, so
//! [`reply_says_ok`] accepts any of the shapes we have seen.

use crate::state;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    time::{SystemTime, UNIX_EPOCH},
};

/// A token stays valid this long after its last use (sliding window).
const SESSION_TTL: u64 = 12 * 60 * 60;
/// At most this many tokens at once; issuing one more drops the stalest.
const SESSION_LIMIT: usize = 16;
/// Random bytes per token (rendered as twice as many hex digits).
const TOKEN_BYTES: usize = 24;
/// Upper bound for user-supplied names, passwords, salts and tags.
const FIELD_MAX: usize = 256;
/// Upper bound for a stock web token.
const WEBTOKEN_MAX: usize = 4096;

/// One issued bearer token and the moment it lapses (unix seconds).
struct Grant {
    secret: String,
    valid_until: u64,
}

/// Bearer tokens issued by `/auth/login` and `/auth/exchange`.
#[derive(Default)]
pub struct Sessions(Vec<Grant>);

impl Sessions {
    fn drop_expired(&mut self, at: u64) {
        self.0.retain(|entry| entry.valid_until > at);
    }

    /// True when `token` is a live session; a hit pushes its expiry forward.
    pub fn validate(&mut self, token: &str) -> bool {
        let at = unix_secs();
        self.drop_expired(at);
        // Walk every entry so the time taken does not depend on which one matched.
        let mut found = None;
        for (index, entry) in self.0.iter().enumerate() {
            if constant_time_eq(entry.secret.as_bytes(), token.as_bytes()) {
                found = Some(index);
            }
        }
        match found {
            Some(index) => {
                self.0[index].valid_until = at + SESSION_TTL;
                true
            }
            None => false,
        }
    }

    /// Mint a fresh token and remember it. Returns the token and its expiry.
    pub fn issue(&mut self) -> std::io::Result<(String, u64)> {
        let token = random_hex(TOKEN_BYTES)?;
        let at = unix_secs();
        self.drop_expired(at);
        while self.0.len() >= SESSION_LIMIT {
            let stalest = (0..self.0.len())
                .min_by_key(|&i| self.0[i].valid_until)
                .unwrap_or(0);
            self.0.swap_remove(stalest);
        }
        let grant = Grant {
            secret: token,
            valid_until: at + SESSION_TTL,
        };
        let reply = (grant.secret.clone(), grant.valid_until);
        self.0.push(grant);
        Ok(reply)
    }
}

fn random_hex(len: usize) -> std::io::Result<String> {
    let mut raw = vec![0u8; len];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut raw)?;
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(len * 2);
    for byte in raw {
        text.push(DIGITS[usize::from(byte >> 4)] as char);
        text.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    Ok(text)
}

fn sha256_upper_hex(data: &[u8]) -> String {
    format!("{:X}", Sha256::digest(data))
}

/// Check a stock web-UI username/password.
///
/// The stock login is challenge-free: the browser sends
/// `SHA256_HEX(SHA256_HEX(password) ++ salt)` (upper-case hex both times),
/// where the salt comes from `web_login_info`. We do the same and let
/// `web_login` decide.
pub async fn verify_password(username: &str, password: &str) -> bool {
    let sane = !username.is_empty() && username.len() <= FIELD_MAX && password.len() <= FIELD_MAX;
    if !sane {
        return false;
    }
    let salt = match state::ubus("zwrt_web", "web_login_info", json!({})).await {
        Ok(info) => match info.get("zte_web_sault").and_then(Value::as_str) {
            Some(s) if !s.is_empty() && s.len() <= FIELD_MAX => s.to_owned(),
            _ => return false,
        },
        Err(_) => return false,
    };
    let inner = sha256_upper_hex(password.as_bytes());
    let mut salted = inner.into_bytes();
    salted.extend_from_slice(salt.as_bytes());
    let digest = sha256_upper_hex(&salted);
    let request = json!({"username": username, "password": digest});
    state::ubus("zwrt_web", "web_login", request)
        .await
        .is_ok_and(|reply| reply_says_ok(&reply))
}

/// Check a token issued by the stock web UI. Newer builds answer on
/// `webtoken_check`, older ones on `web_security_check`; either may confirm.
pub async fn verify_webtoken(token: &str, mode: i64, remote: &str, tag: &str) -> bool {
    let sane = (1..=WEBTOKEN_MAX).contains(&token.len()) && (1..=FIELD_MAX).contains(&tag.len());
    if !sane {
        return false;
    }
    let request = json!({
        "webtoken": token,
        "zmode": mode,
        "web_remote_addr": remote,
        "z-tag": tag,
    });
    for method in ["webtoken_check", "web_security_check"] {
        let confirmed = state::ubus("zwrt_web", method, request.clone())
            .await
            .is_ok_and(|reply| reply_says_ok(&reply));
        if confirmed {
            return true;
        }
    }
    false
}

/// Interpret a stock `zwrt_web` reply as success or failure.
///
/// Checked in order; the first rule that applies decides:
/// 1. a non-empty token string anywhere we know of means success;
/// 2. the first boolean-ish flag that is present decides;
/// 3. the first numeric/string result code that is present decides (0 = ok);
/// 4. otherwise look for a success word in a status field, or a user name.
fn reply_says_ok(reply: &Value) -> bool {
    let text = |key: &str| reply.get(key).and_then(Value::as_str);

    const TOKEN_KEYS: [&str; 4] = ["webtoken", "token", "auth_token", "secondary_auth_token"];
    if TOKEN_KEYS
        .iter()
        .any(|k| text(k).is_some_and(|t| !t.is_empty()))
    {
        return true;
    }

    const FLAG_KEYS: [&str; 4] = ["success", "ok", "web_login_flag", "login_flag"];
    if let Some(flag) = FLAG_KEYS.iter().find_map(|k| reply.get(*k)) {
        return flag_is_set(flag);
    }

    const CODE_KEYS: [&str; 4] = ["code", "errno", "ret", "result"];
    for key in CODE_KEYS {
        match reply.get(key) {
            Some(Value::String(s)) => {
                return match s.parse::<i64>() {
                    Ok(code) => code == 0,
                    Err(_) => is_success_word(s),
                };
            }
            Some(other) => {
                if let Some(code) = other.as_i64() {
                    return code == 0;
                }
                // Present but neither number nor string: try the next key.
            }
            None => {}
        }
    }

    let status_ok = ["status", "state", "msg", "message"]
        .iter()
        .filter_map(|k| text(k))
        .any(is_success_word);
    status_ok
        || ["username", "user"]
            .iter()
            .filter_map(|k| text(k))
            .any(|name| !name.is_empty())
}

fn flag_is_set(flag: &Value) -> bool {
    match flag {
        Value::Bool(b) => *b,
        Value::String(s) => s == "1" || is_success_word(s),
        other => other.as_i64() == Some(1),
    }
}

fn is_success_word(word: &str) -> bool {
    const WORDS: [&str; 8] = [
        "success", "ok", "true", "pass", "passed", "logged", "logined", "done",
    ];
    WORDS.iter().any(|w| w.eq_ignore_ascii_case(word))
}

/// Compare without an early exit on the first differing byte.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut acc = 0u8;
    for (a, b) in left.iter().zip(right) {
        acc |= a ^ b;
    }
    acc == 0
}

fn unix_secs() -> u64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(elapsed) => elapsed.as_secs(),
        Err(_) => 0,
    }
}

/// OAuth-style body returned by the login endpoints.
pub fn token_reply(token: String, expires_at: u64) -> Value {
    json!({
        "ok": true,
        "token_type": "Bearer",
        "access_token": token,
        "expires_in": SESSION_TTL,
        "expires_at": expires_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn response_shapes() {
        // Result codes: zero is success, anything else is not.
        assert!(reply_says_ok(&json!({"result": 0})));
        assert!(!reply_says_ok(&json!({"result": 1})));
        // A freshly minted token is 48 hex digits and is accepted right away.
        let mut table = Sessions::default();
        let (minted, _expiry) = table.issue().unwrap();
        assert_eq!(minted.len(), 48);
        assert!(table.validate(&minted));
    }
}
