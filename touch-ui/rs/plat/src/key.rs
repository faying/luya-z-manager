//! The power key (src/key_input.c): the /dev/input/event* that reports KEY_POWER and is not a
//! touchscreen (pmic_pwrkey); short and long presses (long = held 1.2 s, said once while held,
//! or at release when nobody polled while it was held: the screen was dark).
// SPDX-License-Identifier: MIT

use crate::touch::{event_nodes, ioc_read, open_nonblock, test_bit};
use std::os::fd::{AsRawFd, OwnedFd};

const EV_KEY: u16 = 0x01;
const EV_ABS: u64 = 0x03;
const KEY_POWER: usize = 116;
pub const KEY_LONG_MS: u32 = 1200;

pub const EV_NONE: i32 = 0;
pub const EV_SHORT: i32 = 1;
pub const EV_LONG: i32 = 2;

#[derive(Default)]
pub struct Key {
    fd: Option<OwnedFd>,
    pub pressed: bool,
    press_ms: u32,
    long_emitted: bool,
}

impl Key {
    pub fn open() -> Key {
        for p in event_nodes() {
            let Some(fd) = open_nonblock(&p) else { continue };
            let raw = fd.as_raw_fd();
            let mut ev = [0u64; 1];
            // SAFETY: EVIOCGBIT(0, len) writes at most len bytes
            if unsafe { libc::ioctl(raw, ioc_read(0x20, size_of_val(&ev)) as _, ev.as_mut_ptr()) } < 0 || test_bit(&ev, EV_ABS as usize) {
                continue; // skip touchscreens
            }
            let mut key = [0u64; KEY_POWER / 64 + 1];
            // SAFETY: EVIOCGBIT(EV_KEY, len)
            if unsafe { libc::ioctl(raw, ioc_read(0x20 + EV_KEY as u64, size_of_val(&key)) as _, key.as_mut_ptr()) } < 0 || !test_bit(&key, KEY_POWER) {
                continue;
            }
            eprintln!("key: power button on {}", p.display());
            return Key { fd: Some(fd), ..Default::default() };
        }
        eprintln!("key: no KEY_POWER device found");
        Key::default()
    }

    pub fn fd(&self) -> i32 {
        self.fd.as_ref().map_or(-1, |f| f.as_raw_fd())
    }

    /// Drain events; one EV_* edge per call.
    pub fn poll(&mut self, now_ms: u32) -> i32 {
        let Some(fd) = self.fd.as_ref().map(|f| f.as_raw_fd()) else { return EV_NONE };
        let mut result = EV_NONE;
        let mut ev = [0u8; 24];
        // SAFETY: whole events into a local buffer
        while unsafe { libc::read(fd, ev.as_mut_ptr().cast(), 24) } == 24 {
            let kind = u16::from_ne_bytes([ev[16], ev[17]]);
            let code = u16::from_ne_bytes([ev[18], ev[19]]) as usize;
            let value = i32::from_ne_bytes([ev[20], ev[21], ev[22], ev[23]]);
            if kind != EV_KEY || code != KEY_POWER {
                continue;
            }
            if value == 1 {
                self.pressed = true;
                self.press_ms = now_ms;
                self.long_emitted = false;
            } else if value == 0 {
                if self.pressed && !self.long_emitted {
                    // held past the threshold but never polled while held: still long
                    result = if now_ms.wrapping_sub(self.press_ms) < KEY_LONG_MS { EV_SHORT } else { EV_LONG };
                }
                self.pressed = false;
            }
        }
        if self.pressed && !self.long_emitted && now_ms.wrapping_sub(self.press_ms) >= KEY_LONG_MS {
            self.long_emitted = true;
            result = EV_LONG;
        }
        result
    }

    pub fn held(&self) -> bool {
        self.fd.is_some() && self.pressed
    }
}
