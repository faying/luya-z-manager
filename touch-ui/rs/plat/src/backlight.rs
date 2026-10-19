//! The panel backlight (src/backlight.c): /sys/class/leds/led:lcd/{brightness,max_brightness}.
//! The on-level is remembered across off/on; `is_on` is our own idea of it, which the power key
//! resyncs from sysfs (the vendor key daemon sees the key too).
// SPDX-License-Identifier: MIT

const BL_DIR: &str = "/sys/class/leds/led:lcd";

pub struct Backlight {
    dir: String,
    on_level: i32,
    is_on: bool,
}

fn read_int(path: &str, def: i32) -> i32 {
    std::fs::read_to_string(path).ok().and_then(|s| s.split_whitespace().next().and_then(|v| v.parse().ok())).unwrap_or(def)
}

fn write_int(path: &str, v: i32) {
    let _ = std::fs::write(path, v.to_string());
}

impl Backlight {
    /// LUYAZ_UI_BACKLIGHT_DIR: another directory with the same two files (the offscreen smoke
    /// test; the device never sets it).
    pub fn init() -> Backlight {
        let dir = std::env::var("LUYAZ_UI_BACKLIGHT_DIR").unwrap_or_else(|_| BL_DIR.into());
        let mut b = Backlight { dir, on_level: 255, is_on: true };
        let cur = read_int(&b.brightness(), 0);
        let max = read_int(&b.max_path(), 255);
        if cur > 0 {
            b.on_level = cur;
            b.is_on = true;
        } else {
            b.on_level = max;
            b.is_on = false;
        }
        b
    }

    fn brightness(&self) -> String {
        format!("{}/brightness", self.dir)
    }

    fn max_path(&self) -> String {
        format!("{}/max_brightness", self.dir)
    }

    /// Back to the remembered level.
    pub fn on(&mut self) {
        if self.on_level <= 0 {
            self.on_level = read_int(&self.max_path(), 255);
        }
        write_int(&self.brightness(), self.on_level);
        self.is_on = true;
    }

    pub fn off(&mut self) {
        let cur = read_int(&self.brightness(), 0);
        if cur > 0 {
            self.on_level = cur; // remember the level we had
        }
        write_int(&self.brightness(), 0);
        self.is_on = false;
    }

    pub fn is_on(&self) -> bool {
        self.is_on
    }

    /// sysfs brightness > 0, without touching our own flag (the core's pace).
    pub fn panel_lit(&self) -> bool {
        read_int(&self.brightness(), 0) > 0
    }

    /// What the panel really does, and our flag follows it.
    pub fn is_lit(&mut self) -> bool {
        self.is_on = read_int(&self.brightness(), 0) > 0;
        self.is_on
    }

    /// Set the level (remembered when > 0).
    pub fn set(&mut self, level: i32) {
        let max = read_int(&self.max_path(), 255);
        let level = level.clamp(0, max.max(0));
        write_int(&self.brightness(), level);
        if level > 0 {
            self.on_level = level;
            self.is_on = true;
        } else {
            self.is_on = false;
        }
    }

    /// The remembered level (not a value mid-fade).
    pub fn get(&self) -> i32 {
        self.on_level
    }

    /// The level to come back to, without touching the panel (started dark).
    pub fn remember(&mut self, level: i32) {
        if level > 0 {
            self.on_level = level;
        }
    }

    pub fn max(&self) -> i32 {
        read_int(&self.max_path(), 255)
    }
}
