//! 6e: stop mihomo while nobody can use it (manager docs/designs/u60-radio.md §6 6e, §13; D4,
//! D13). Only with `LUYAZ_CHILL_IDLE=1`.
//!
//! Nobody = the screen off, 0 LAN clients (unknown is somebody), tailscaled readable and not
//! advertising an exit node (unreadable is somebody: no tailnet peer can reach the internet
//! through this device otherwise, §13), and CHILL meant on. Held for [`HOLD_S`], the core writes
//! a boot-bound BOOTTIME lease specifying how long mihomo may stay stopped — to [`LEASE`] and renews it every
//! [`TICK_S`] (local reads only, so a phone joining or the screen waking ends the pause within
//! seconds; tailscaled's prefs are read every [`PREFS_S`]); chill.sh's supervisor stops the core while the lease runs and starts it again
//! when it lapses. Any condition failing removes the lease at once; a core that dies leaves a
//! lease that lapses within [`LEASE_S`], so mihomo never stays down without the core saying so.
//! tmpfs: a reboot clears it.

use luyaz_api::alerts::{
    clock::BootTime,
    deadline::{self, Deadline},
};
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
    since: Option<BootTime>,
}

impl Idle {
    /// Suspend counts toward the hold; unknown/backwards/different boot resets it.
    pub fn step(&mut self, now: Option<&BootTime>, o: &Obs) -> Option<Deadline> {
        if !nobody(o) || now.is_none() {
            self.since = None;
            return None;
        }
        let now = now?;
        if self
            .since
            .as_ref()
            .is_some_and(|t| now.elapsed_since(t).is_none())
        {
            self.since = None;
        }
        let since = self.since.get_or_insert_with(|| now.clone());
        (now.elapsed_since(since)? >= HOLD_S).then(|| Deadline::new(now.clone(), LEASE_S))?
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

fn look(
    p: &Paths,
    hub: &crate::block::Hub,
    now: Option<&BootTime>,
    exit_node: Option<bool>,
) -> Obs {
    let num = |f: &Path| -> Option<i64> { std::fs::read_to_string(f).ok()?.trim().parse().ok() };
    let paused = deadline::manual_paused(&p.chill_dir, now);
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

/// CHILL meant on now (the look's rule), for `/api/diagnose`'s `long_idle`.
pub fn meant_on() -> bool {
    let dir = PathBuf::from(crate::state::host_path(CHILL_DIR));
    let paused = deadline::manual_paused(&dir, BootTime::read().as_ref());
    dir.is_dir() && !dir.join("disabled").exists() && !paused
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
    let _ = std::thread::Builder::new()
        .name("chill-idle".into())
        .spawn(move || {
            let mut idle = Idle::default();
            let mut on = false;
            let mut prefs: Option<(BootTime, Option<bool>)> = None;
            loop {
                let now = BootTime::read();
                // an unreadable answer is not kept: asked again next tick
                let x = match prefs.as_ref() {
                    Some((at, Some(x)))
                        if now
                            .as_ref()
                            .and_then(|n| n.elapsed_since(at))
                            .is_some_and(|s| s < PREFS_S) =>
                    {
                        Some(*x)
                    }
                    _ => {
                        let x = exit_node_now(&p);
                        prefs = now.clone().map(|n| (n, x));
                        x
                    }
                };
                let o = look(&p, &hub, now.as_ref(), x);
                match idle.step(now.as_ref(), &o) {
                    Some(until) => {
                        if let Err(e) =
                            crate::api::ops::atomic_write(&p.lease, until.encode().as_bytes())
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
        let at = |seconds| BootTime {
            boot_id: "a".into(),
            seconds,
        };
        assert_eq!(i.step(Some(&at(0)), &quiet()), None);
        assert_eq!(i.step(Some(&at(270)), &quiet()), None);
        assert_eq!(i.step(Some(&at(300)), &quiet()).unwrap().until, 480);
        assert_eq!(i.step(Some(&at(305)), &quiet()).unwrap().until, 485);
        let on = Obs {
            screen_on: Some(true),
            ..quiet()
        };
        assert_eq!(i.step(Some(&at(310)), &on), None);
        assert_eq!(i.step(Some(&at(315)), &quiet()), None);
        assert_eq!(i.step(Some(&at(615)), &quiet()).unwrap().until, 795);
        assert_eq!(i.step(None, &quiet()), None);
        assert_eq!(i.step(Some(&at(999)), &quiet()), None);
        assert_eq!(
            i.step(Some(&at(900)), &quiet()),
            None,
            "backwards resets hold"
        );
        assert_eq!(
            i.step(
                Some(&BootTime {
                    boot_id: "b".into(),
                    seconds: 2000
                }),
                &quiet()
            ),
            None
        );
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
