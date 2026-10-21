//! The power button: whichever input device can send KEY_POWER and has no axes (so not the
//! touchscreen). A press shorter than KEY_LONG_MS is short; one held longer is long, said the
//! moment the hold crosses the line if someone is polling, else when it ends (a dark screen polls
//! nothing).

use crate::evdev::{self, Device, EV_ABS, EV_KEY};
use std::os::fd::{AsRawFd, OwnedFd};

const KEY_POWER: u16 = 116;
pub const KEY_LONG_MS: u32 = 1200;

pub const EV_NONE: i32 = 0;
pub const EV_SHORT: i32 = 1;
pub const EV_LONG: i32 = 2;

#[derive(Default)]
pub struct Key {
    fd: Option<OwnedFd>,
    pub pressed: bool,
    /// when the current press began
    down_at: u32,
    /// the current press has already been reported as long
    said_long: bool,
}

fn is_power_button(dev: &Device) -> bool {
    let types = dev.codes::<1>(0);
    let keys = dev.codes::<2>(EV_KEY);
    match (types, keys) {
        (Some(t), Some(k)) => !evdev::has_bit(&t, EV_ABS as usize) && evdev::has_bit(&k, KEY_POWER as usize),
        _ => false,
    }
}

impl Key {
    pub fn open() -> Key {
        let found = evdev::nodes().into_iter().find_map(|path| {
            let dev = Device::open(&path)?;
            is_power_button(&dev).then_some((path, dev))
        });
        match found {
            Some((path, dev)) => {
                eprintln!("key: power button on {}", path.display());
                Key { fd: Some(dev.into_fd()), ..Default::default() }
            }
            None => {
                eprintln!("key: no KEY_POWER device found");
                Key::default()
            }
        }
    }

    pub fn fd(&self) -> i32 {
        self.fd.as_ref().map_or(-1, |f| f.as_raw_fd())
    }

    /// Read what the button sent since last time; at most one EV_* comes back (the latest).
    pub fn poll(&mut self, now_ms: u32) -> i32 {
        let Some(fd) = self.fd.as_ref().map(|f| f.as_raw_fd()) else { return EV_NONE };
        let mut said = None;
        evdev::drain(fd, |e| {
            if e.kind == EV_KEY && e.code == KEY_POWER {
                said = self.edge(e.value, now_ms).or(said);
            }
        });
        self.tick(now_ms).or(said).unwrap_or(EV_NONE)
    }

    /// The button went down (1) or up (0); 2 is autorepeat and changes nothing.
    fn edge(&mut self, value: i32, now_ms: u32) -> Option<i32> {
        match value {
            1 => {
                (self.pressed, self.down_at, self.said_long) = (true, now_ms, false);
                None
            }
            0 => {
                let was = std::mem::replace(&mut self.pressed, false);
                let held = now_ms.wrapping_sub(self.down_at);
                (was && !self.said_long).then_some(if held >= KEY_LONG_MS { EV_LONG } else { EV_SHORT })
            }
            _ => None,
        }
    }

    /// A press still going on turns long once it has lasted KEY_LONG_MS.
    fn tick(&mut self, now_ms: u32) -> Option<i32> {
        let due = self.pressed && !self.said_long && now_ms.wrapping_sub(self.down_at) >= KEY_LONG_MS;
        self.said_long |= due;
        due.then_some(EV_LONG)
    }

    pub fn held(&self) -> bool {
        self.fd.is_some() && self.pressed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// what poll() says for one key event at `now` (None = no event, just the clock)
    fn at(k: &mut Key, value: Option<i32>, now: u32) -> i32 {
        let mut r = EV_NONE;
        if let Some(e) = value.and_then(|v| k.edge(v, now)) {
            r = e;
        }
        if let Some(e) = k.tick(now) {
            r = e;
        }
        r
    }

    #[test]
    fn a_short_press() {
        let mut k = Key::default();
        assert_eq!(at(&mut k, Some(1), 1000), EV_NONE);
        assert_eq!(at(&mut k, None, 1500), EV_NONE);
        assert_eq!(at(&mut k, Some(0), 1600), EV_SHORT);
        assert_eq!(at(&mut k, None, 5000), EV_NONE);
    }

    #[test]
    fn a_hold_is_long_once_while_held() {
        let mut k = Key::default();
        at(&mut k, Some(1), 1000);
        assert_eq!(at(&mut k, None, 2199), EV_NONE);
        assert_eq!(at(&mut k, None, 2200), EV_LONG);
        assert_eq!(at(&mut k, None, 3000), EV_NONE);
        assert_eq!(at(&mut k, Some(0), 3100), EV_NONE);
    }

    #[test]
    fn a_hold_nobody_polled_is_long_at_release() {
        let mut k = Key::default();
        at(&mut k, Some(1), 1000);
        assert_eq!(at(&mut k, Some(0), 2500), EV_LONG);
        assert_eq!(at(&mut k, None, 2600), EV_NONE);
    }

    #[test]
    fn press_and_release_inside_one_poll() {
        let mut k = Key::default();
        assert_eq!(k.edge(1, 100), None);
        assert_eq!(k.edge(0, 150), Some(EV_SHORT));
        assert_eq!(k.tick(150), None);
        assert!(!k.held());
    }

    #[test]
    fn the_clock_wraps() {
        let mut k = Key::default();
        at(&mut k, Some(1), u32::MAX - 100);
        assert_eq!(at(&mut k, None, 500), EV_NONE);
        assert_eq!(at(&mut k, None, 1100), EV_LONG);
        let mut k = Key::default();
        at(&mut k, Some(1), u32::MAX - 100);
        assert_eq!(at(&mut k, Some(0), 200), EV_SHORT);
    }

    #[test]
    fn autorepeat_is_ignored() {
        let mut k = Key::default();
        at(&mut k, Some(1), 0);
        assert_eq!(at(&mut k, Some(2), 300), EV_NONE);
        assert_eq!(at(&mut k, Some(0), 400), EV_SHORT);
    }
}
