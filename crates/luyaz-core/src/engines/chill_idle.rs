//! 6e: stop mihomo while nobody can use it (manager docs/designs/u60-radio.md §6 6e, §13; D4,
//! D13). Only with `LUYAZ_CHILL_IDLE=1`.
//!
//! Pause only while the screen is known off, the client block reports zero clients, and CHILL
//! is meant on. Missing or stale readings block the pause. After `HOLD_S`, renew a boot-bound
//! lease every `TICK_S`. The supervisor stops mihomo during the lease and restarts it when
//! the lease is removed or expires. A core crash leaves at most `LEASE_S` of pause.

use luyaz_api::alerts::{
    clock::BootTime,
    deadline::{self, Deadline},
};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const ENV: &str = "LUYAZ_CHILL_IDLE";
pub const LEASE: &str = crate::longterm::power::CHILL_IDLE;
const BACKLIGHT: &str = "/sys/class/leds/led:lcd/brightness";
const CHILL_DIR: &str = "/data/chill";
/// Nobody for this long before mihomo stops.
pub const HOLD_S: u64 = 300;
/// A lease runs this far past its writing.
pub const LEASE_S: u64 = 180;
pub const TICK_S: u64 = 5;

pub fn enabled() -> bool {
    std::env::var(ENV).is_ok_and(|v| v == "1")
}

/// One look (None = not readable).
#[derive(Clone, Debug, Default)]
pub struct Obs {
    pub screen_on: Option<bool>,
    pub clients: Option<i64>,
    /// CHILL meant on: its directory there, not switched off, not paused by hand
    pub chill_on: bool,
}

/// Nobody can use mihomo now.
pub fn nobody(o: &Obs) -> bool {
    o.chill_on && o.screen_on == Some(false) && o.clients == Some(0)
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

struct Paths {
    backlight: PathBuf,
    chill_dir: PathBuf,
    lease: PathBuf,
}

fn look(p: &Paths, hub: &crate::block::Hub, now: Option<&BootTime>) -> Obs {
    let num = |f: &Path| -> Option<i64> { std::fs::read_to_string(f).ok()?.trim().parse().ok() };
    let paused = deadline::manual_paused(&p.chill_dir, now);
    Obs {
        screen_on: num(&p.backlight).map(|b| b > 0),
        clients: hub
            .view("clients")
            .filter(|v| !v.stale)
            .and_then(|v| v.data["total"].as_i64()),
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
        chill_dir: h(CHILL_DIR),
        lease: h(LEASE),
    };
    let _ = std::thread::Builder::new()
        .name("chill-idle".into())
        .spawn(move || {
            let mut idle = Idle::default();
            let mut on = false;
            loop {
                let now = BootTime::read();
                let o = look(&p, &hub, now.as_ref());
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

    fn quiet() -> Obs {
        Obs {
            screen_on: Some(false),
            clients: Some(0),
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
                chill_on: false,
                ..quiet()
            },
        ] {
            assert!(!nobody(&o), "{o:?}");
        }
    }

    #[test]
    fn a_failed_client_read_cancels_the_lease_even_after_zero_clients() {
        use crate::block::{BlockSpec, Hub, NoSink, OnChange};
        let hub = Hub::new(
            vec![BlockSpec::derived("clients", Box::new(OnChange))],
            Box::new(NoSink),
        );
        let dir = std::env::temp_dir().join(format!("luyaz-chill-idle-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = Paths {
            backlight: dir.join("brightness"),
            chill_dir: dir.clone(),
            lease: dir.join("lease"),
        };
        std::fs::write(&paths.backlight, "0\n").unwrap();

        let mut idle = Idle::default();
        let at = |seconds| BootTime {
            boot_id: "fixture".into(),
            seconds,
        };
        assert_eq!(look(&paths, &hub, Some(&at(0))).clients, None);
        hub.record("clients", Ok(serde_json::json!({"total": 0})), tokio::time::Instant::now());
        let quiet = look(&paths, &hub, Some(&at(0)));
        assert!(nobody(&quiet));
        assert_eq!(idle.step(Some(&at(0)), &quiet), None);
        assert!(idle.step(Some(&at(HOLD_S)), &quiet).is_some());

        hub.record("clients", Err("unreadable".into()), tokio::time::Instant::now());
        let now = at(HOLD_S + TICK_S);
        let unknown = look(&paths, &hub, Some(&now));
        assert_eq!(unknown.clients, None);
        assert_eq!(idle.step(Some(&now), &unknown), None);
        std::fs::remove_dir_all(dir).unwrap();
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
}
