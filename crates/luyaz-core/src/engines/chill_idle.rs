//! 6e: stop mihomo while nobody can use it (manager docs/designs/u60-radio.md §6 6e, §13; D4,
//! D13). Only with `LUYAZ_CHILL_IDLE=1`.
//!
//! Nobody = the screen off, 0 LAN clients (unknown is somebody), tailscaled readable and not
//! advertising an exit node (unreadable is somebody: no tailnet peer can reach the internet
//! through this device otherwise, §13), and CHILL meant on. Held for [`HOLD_S`], the core writes
//! a lease — the wall second mihomo may stay stopped until — to [`LEASE`] and renews it every
//! [`TICK_S`] (local reads only, so a phone joining or the screen waking ends the pause within
//! seconds; tailscaled's prefs are read every [`PREFS_S`]); chill.sh's supervisor stops the core while the lease runs and starts it again
//! when it lapses. Any condition failing removes the lease at once; a core that dies leaves a
//! lease that lapses within [`LEASE_S`], so mihomo never stays down without the core saying so.
//! tmpfs: a reboot clears it.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const ENV: &str = "LUYAZ_CHILL_IDLE";
pub const LEASE: &str = crate::longterm::power::CHILL_IDLE;
const BACKLIGHT: &str = "/sys/class/leds/led:lcd/brightness";
const TS_SOCK: &str = "/tmp/tailscaled.sock";
const CHILL_DIR: &str = "/data/chill";
/// Nobody for this long before mihomo stops.
pub const HOLD_S: u64 = 300;
/// A lease runs this far past its writing.
pub const LEASE_S: u64 = 180;
pub const TICK_S: u64 = 5;
pub const PREFS_S: u64 = 60;

pub fn enabled() -> bool {
    std::env::var(ENV).is_ok_and(|v| v == "1")
}

/// One look (None = not readable).
#[derive(Clone, Debug, Default)]
pub struct Obs {
    pub screen_on: Option<bool>,
    pub clients: Option<i64>,
    /// tailscaled advertises 0.0.0.0/0 or ::/0
    pub exit_node: Option<bool>,
    /// CHILL meant on: its directory there, not switched off, not paused by hand
    pub chill_on: bool,
}

/// Nobody can use mihomo now.
pub fn nobody(o: &Obs) -> bool {
    o.chill_on && o.screen_on == Some(false) && o.clients == Some(0) && o.exit_node == Some(false)
}

#[derive(Debug, Default)]
pub struct Idle {
    since: Option<u64>,
}

impl Idle {
    /// The lease to write (its wall second), or None to remove it. `mono` for the hold, `wall`
    /// for the lease (chill.sh compares it with `date +%s`).
    pub fn step(&mut self, mono: u64, wall: u64, o: &Obs) -> Option<u64> {
        if !nobody(o) {
            self.since = None;
            return None;
        }
        let since = *self.since.get_or_insert(mono);
        (mono.saturating_sub(since) >= HOLD_S).then_some(wall + LEASE_S)
    }
}

/// tailscaled's prefs: is it an exit node (None = could not tell).
pub fn exit_node(prefs: &Value) -> Option<bool> {
    let routes = &prefs["AdvertiseRoutes"];
    let list = match routes {
        Value::Null => return prefs.is_object().then_some(false),
        Value::Array(a) => a,
        _ => return None,
    };
    Some(
        list.iter()
            .any(|r| matches!(r.as_str(), Some("0.0.0.0/0" | "::/0"))),
    )
}

struct Paths {
    backlight: PathBuf,
    ts_sock: PathBuf,
    chill_dir: PathBuf,
    lease: PathBuf,
}

fn exit_node_now(p: &Paths) -> Option<bool> {
    match crate::longterm::power::ts_get(&p.ts_sock, "/localapi/v0/prefs") {
        Ok(Some((200, body))) => serde_json::from_str(&body).ok().and_then(|v| exit_node(&v)),
        _ => None,
    }
}

fn look(p: &Paths, hub: &crate::block::Hub, wall: u64, exit_node: Option<bool>) -> Obs {
    let num = |f: &Path| -> Option<i64> { std::fs::read_to_string(f).ok()?.trim().parse().ok() };
    let paused = num(&p.chill_dir.join("pause_until")).is_some_and(|t| t > wall as i64);
    Obs {
        screen_on: num(&p.backlight).map(|b| b > 0),
        clients: hub
            .view("clients")
            .filter(|v| !v.stale)
            .and_then(|v| v.data["total"].as_i64()),
        exit_node,
        chill_on: p.chill_dir.is_dir() && !p.chill_dir.join("disabled").exists() && !paused,
    }
}

fn wall_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The loop: a look every [`TICK_S`], the lease written or removed.
pub fn spawn(hub: Arc<crate::block::Hub>) {
    let h = |x: &str| PathBuf::from(crate::state::host_path(x));
    let p = Paths {
        backlight: h(BACKLIGHT),
        ts_sock: h(TS_SOCK),
        chill_dir: h(CHILL_DIR),
        lease: h(LEASE),
    };
    let t0 = std::time::Instant::now();
    let _ = std::thread::Builder::new()
        .name("chill-idle".into())
        .spawn(move || {
            let mut idle = Idle::default();
            let mut on = false;
            let mut prefs: Option<(u64, Option<bool>)> = None;
            loop {
                let wall = wall_now();
                let mono = t0.elapsed().as_secs();
                // an unreadable answer is not kept: asked again next tick
                let x = match prefs {
                    Some((at, Some(x))) if mono - at < PREFS_S => Some(x),
                    _ => {
                        let x = exit_node_now(&p);
                        prefs = Some((mono, x));
                        x
                    }
                };
                let o = look(&p, &hub, wall, x);
                match idle.step(mono, wall, &o) {
                    Some(until) => {
                        if let Err(e) =
                            crate::api::ops::atomic_write(&p.lease, format!("{until}\n").as_bytes())
                        {
                            eprintln!("[chill-idle] lease: {e}");
                        } else if !on {
                            on = true;
                            eprintln!("[chill-idle] nobody for {HOLD_S} s: CHILL paused");
                        }
                    }
                    None => {
                        if p.lease.exists() {
                            let _ = std::fs::remove_file(&p.lease);
                        }
                        if std::mem::take(&mut on) {
                            eprintln!("[chill-idle] {o:?}: CHILL back on");
                        }
                    }
                }
                std::thread::sleep(std::time::Duration::from_secs(TICK_S));
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn quiet() -> Obs {
        Obs {
            screen_on: Some(false),
            clients: Some(0),
            exit_node: Some(false),
            chill_on: true,
        }
    }

    #[test]
    fn nobody_needs_every_condition_known() {
        assert!(nobody(&quiet()));
        for o in [
            Obs {
                screen_on: Some(true),
                ..quiet()
            },
            Obs {
                screen_on: None,
                ..quiet()
            },
            Obs {
                clients: Some(1),
                ..quiet()
            },
            Obs {
                clients: None,
                ..quiet()
            },
            Obs {
                exit_node: Some(true),
                ..quiet()
            },
            Obs {
                exit_node: None,
                ..quiet()
            },
            Obs {
                chill_on: false,
                ..quiet()
            },
        ] {
            assert!(!nobody(&o), "{o:?}");
        }
    }

    #[test]
    fn five_minutes_then_a_lease_renewed_and_dropped_at_once() {
        let mut i = Idle::default();
        let w = 1_800_000_000;
        assert_eq!(i.step(0, w, &quiet()), None);
        assert_eq!(i.step(270, w + 270, &quiet()), None, "4.5 min: not yet");
        assert_eq!(i.step(300, w + 300, &quiet()), Some(w + 300 + LEASE_S));
        assert_eq!(
            i.step(305, w + 305, &quiet()),
            Some(w + 305 + LEASE_S),
            "renewed"
        );
        let on = Obs {
            screen_on: Some(true),
            ..quiet()
        };
        assert_eq!(i.step(310, w + 310, &on), None, "the screen on: at once");
        assert_eq!(
            i.step(315, w + 315, &quiet()),
            None,
            "and the hold starts over"
        );
        assert_eq!(i.step(615, w + 615, &quiet()), Some(w + 615 + LEASE_S));
    }

    #[test]
    fn exit_node_from_prefs() {
        assert_eq!(
            exit_node(&json!({"AdvertiseRoutes": ["192.168.0.0/24"]})),
            Some(false)
        );
        assert_eq!(exit_node(&json!({"AdvertiseRoutes": null})), Some(false));
        assert_eq!(exit_node(&json!({"WantRunning": true})), Some(false));
        assert_eq!(
            exit_node(&json!({"AdvertiseRoutes": ["192.168.0.0/24", "0.0.0.0/0", "::/0"]})),
            Some(true)
        );
        assert_eq!(exit_node(&json!({"AdvertiseRoutes": ["::/0"]})), Some(true));
        assert_eq!(exit_node(&json!({"AdvertiseRoutes": "x"})), None);
        assert_eq!(exit_node(&json!([])), None);
    }

    /// A real socket: prefs read, an exit node seen, nothing listening = unknown.
    #[test]
    fn prefs_over_a_real_socket() {
        use std::io::{Read, Write};
        let dir = std::env::temp_dir().join(format!("u60d-idle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("ts.sock");
        assert!(crate::longterm::power::ts_get(&sock, "/localapi/v0/prefs").is_err());
        let l = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let t = std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut b = [0u8; 512];
            let n = c.read(&mut b).unwrap();
            assert!(String::from_utf8_lossy(&b[..n]).starts_with("GET /localapi/v0/prefs "));
            let body = r#"{"AdvertiseRoutes":["192.168.0.0/24","0.0.0.0/0"]}"#;
            write!(
                c,
                "HTTP/1.0 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let (code, body) = crate::longterm::power::ts_get(&sock, "/localapi/v0/prefs")
            .unwrap()
            .unwrap();
        t.join().unwrap();
        assert_eq!(code, 200);
        assert_eq!(exit_node(&serde_json::from_str(&body).unwrap()), Some(true));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
