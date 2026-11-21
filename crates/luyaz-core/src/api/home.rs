//! Helpers shared by the home status and CHILL routes.

use super::Compat;

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(super) fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Keeps the alert sender's `roaming` flag on /data in step with `signal.true_roaming`. Unknown or
/// stale leaves it as it is.
pub(crate) async fn roaming_flag_loop(c: Compat) {
    let alerts = luyaz_api::alerts::Alerts::at(&c.path(luyaz_api::alerts::DIR));
    let mut last = None;
    loop {
        let now = c
            .src
            .hub
            .as_ref()
            .and_then(|h| h.view("signal"))
            .filter(|v| !v.stale)
            .and_then(|v| v.data["true_roaming"].as_bool());
        if now.is_some() && now != last {
            match alerts.set_flag(luyaz_api::alerts::sms::ROAMING_FLAG, now == Some(true)) {
                Ok(()) => last = now,
                Err(e) => eprintln!("[alerts] could not update the roaming flag: {e}"),
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    }
}
