//! `data.reconnect`: redial the data session (B31, decision-execution-loop.md §4.6).
//!
//! Loop-only: the spec is in [`super::spec::LOOP_ONLY`], not [`super::spec::SPECS`], so
//! `/control`, the screen, the web and MCP can never call it (it cuts the uplink).
//!
//! Measured on the device (10-20): `set_qcliiface` `enable:0` on both families drops both at once;
//! the stock daemon redials by itself within ~15 s, and `enable:1` brings back whatever it didn't.
//! `get_wwaniface` keeps saying connected while down, so the readback is the rmnet addresses.
//!
//! The session must never be left disabled (the device is often reached over this very link):
//! a marker is written before the first `enable:0`; a guard re-enables on every early exit,
//! panic or cancel; and [`recover`] re-enables at core start when the marker is still there.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

pub const IFACE: &str = "rmnet_data0";
const MARKER: &str = "/data/luyaz/core/reconnect.inflight";
/// Down this long before `enable:1` (the 10-20 tests used 8 s).
const DOWN: Duration = Duration::from_secs(8);
/// IPv4, IPv6.
const FAMILIES: [u8; 2] = [1, 2];

fn args(family: u8, enable: u8) -> Value {
    json!({"source_module": "zte_topsw_data", "type": family, "enable": enable, "sub_id": 1})
}

/// "up" when the data interface has a global IPv4 or IPv6 address, else "down".
/// `v4` = `ip -4 -o addr show dev rmnet_data0`, `if_inet6` = `/proc/net/if_inet6`.
pub fn session(v4: &str, if_inet6: &str) -> &'static str {
    let has_v4 = v4.lines().any(|l| l.contains(" inet "));
    // fields: address, index, prefix length, scope (00 = global), flags, name
    let has_v6 = if_inet6.lines().any(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        f.len() == 6 && f[3] == "00" && f[5] == IFACE
    });
    if has_v4 || has_v6 { "up" } else { "down" }
}

pub async fn read_session() -> Result<&'static str, String> {
    let out = tokio::process::Command::new("ip")
        .args(["-4", "-o", "addr", "show", "dev", IFACE])
        .output()
        .await
        .map_err(|e| format!("ip: {e}"))?;
    let v6 = tokio::fs::read_to_string("/proc/net/if_inet6")
        .await
        .unwrap_or_default();
    Ok(session(&String::from_utf8_lossy(&out.stdout), &v6))
}

/// Re-enables both families when dropped armed. Sync on purpose: it also runs on panic and on
/// a cancelled future, where nothing async can be awaited.
struct Guard {
    marker: PathBuf,
    armed: bool,
}

impl Drop for Guard {
    fn drop(&mut self) {
        if self.armed {
            eprintln!("reconnect: re-enabling the data session after an early exit");
            enable_sync();
            let _ = std::fs::remove_file(&self.marker);
        }
    }
}

fn enable_sync() {
    for f in FAMILIES {
        for _ in 0..2 {
            let ok = std::process::Command::new("ubus")
                .args(["call", "zwrt_qcmap_cli", "set_qcliiface", &args(f, 1).to_string()])
                .status()
                .is_ok_and(|s| s.success());
            if ok {
                break;
            }
        }
    }
}

/// At core start: a marker left behind means a redial was cut short; bring the session back.
pub fn recover() {
    recover_at(Path::new(MARKER), enable_sync);
}

fn recover_at(marker: &Path, enable: impl FnOnce()) -> bool {
    if !marker.exists() {
        return false;
    }
    eprintln!("reconnect: a redial was cut short; re-enabling the data session");
    enable();
    let _ = std::fs::remove_file(marker);
    true
}

/// Disable both families, wait, enable both. `call` is the ubus call (the executor's, in the
/// engine's executor task). Err before the marker is saved = nothing was touched.
pub async fn redial<F, Fut>(call: F) -> Result<Value, String>
where
    F: Fn(Value) -> Fut,
    Fut: std::future::Future<Output = Result<Value, String>>,
{
    redial_at(Path::new(MARKER), DOWN, call).await
}

async fn redial_at<F, Fut>(marker: &Path, down: Duration, call: F) -> Result<Value, String>
where
    F: Fn(Value) -> Fut,
    Fut: std::future::Future<Output = Result<Value, String>>,
{
    std::fs::write(marker, b"redial\n")
        .map_err(|e| format!("cannot save the redial marker; not changing anything: {e}"))?;
    let mut guard = Guard {
        marker: marker.to_owned(),
        armed: true,
    };
    for f in FAMILIES {
        // a failed disable still goes on to enable: the guard covers the rest
        if let Err(e) = call(args(f, 0)).await {
            eprintln!("reconnect: disable type {f}: {e}");
        }
    }
    tokio::time::sleep(down).await;
    let mut last = Ok(Value::Null);
    for f in FAMILIES {
        let mut r = call(args(f, 1)).await;
        if r.is_err() {
            r = call(args(f, 1)).await;
        }
        if r.is_err() {
            last = r;
        }
    }
    if last.is_ok() {
        guard.armed = false;
        let _ = std::fs::remove_file(marker);
    }
    // on error the guard re-enables once more on drop, synchronously
    last
}

#[cfg(test)]
#[path = "reconnect_tests.rs"]
mod tests;
