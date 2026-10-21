//! The panel backlight, through the LED class device's `brightness` and `max_brightness`. Turning
//! it off keeps the level it had so on brings that back. `is_on` is what we last did; the vendor
//! key daemon sees the power key too, so the key handler asks the panel (`is_lit`) and resyncs.

use std::path::PathBuf;

const LED: &str = "/sys/class/leds/led:lcd";
/// what a panel without a readable max is assumed to go up to
const DEFAULT_MAX: i32 = 255;

pub struct Backlight {
    level_file: PathBuf,
    max_file: PathBuf,
    /// the level "on" means
    on_level: i32,
    is_on: bool,
}

/// The first number in a sysfs file.
fn read_num(path: &PathBuf) -> Option<i32> {
    std::fs::read_to_string(path).ok()?.split_whitespace().next()?.parse().ok()
}

impl Backlight {
    /// LUYAZ_UI_BACKLIGHT_DIR points at another directory holding the same two files (the
    /// offscreen smoke test; never set on the device).
    pub fn init() -> Backlight {
        let dir = PathBuf::from(std::env::var("LUYAZ_UI_BACKLIGHT_DIR").unwrap_or_else(|_| LED.into()));
        let mut b = Backlight { level_file: dir.join("brightness"), max_file: dir.join("max_brightness"), on_level: DEFAULT_MAX, is_on: false };
        match b.level() {
            lit if lit > 0 => (b.on_level, b.is_on) = (lit, true),
            _ => b.on_level = b.max(),
        }
        b
    }

    /// What the panel is at now (0 when unreadable).
    fn level(&self) -> i32 {
        read_num(&self.level_file).unwrap_or(0)
    }

    fn write(&self, level: i32) {
        let _ = std::fs::write(&self.level_file, level.to_string());
    }

    pub fn max(&self) -> i32 {
        read_num(&self.max_file).unwrap_or(DEFAULT_MAX)
    }

    /// Light up at the level "on" means.
    pub fn on(&mut self) {
        if self.on_level <= 0 {
            self.on_level = self.max();
        }
        self.write(self.on_level);
        self.is_on = true;
    }

    /// Go dark, keeping the level we were at for the next `on`.
    pub fn off(&mut self) {
        self.remember(self.level());
        self.write(0);
        self.is_on = false;
    }

    pub fn is_on(&self) -> bool {
        self.is_on
    }

    /// Whether the panel is lit, leaving our own flag alone (the core's pace).
    pub fn panel_lit(&self) -> bool {
        self.level() > 0
    }

    /// Whether the panel is lit; our flag takes that over.
    pub fn is_lit(&mut self) -> bool {
        self.is_on = self.panel_lit();
        self.is_on
    }

    /// Go to `level`, held within 0..=max; anything above 0 becomes the "on" level.
    pub fn set(&mut self, level: i32) {
        let level = level.clamp(0, self.max().max(0));
        self.write(level);
        self.remember(level);
        self.is_on = level > 0;
    }

    /// The "on" level (never a step of a fade).
    pub fn get(&self) -> i32 {
        self.on_level
    }

    /// Make `level` the "on" level without touching the panel (a start in the dark); 0 is ignored.
    pub fn remember(&mut self, level: i32) {
        if level > 0 {
            self.on_level = level;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// LUYAZ_UI_BACKLIGHT_DIR is process-wide
    static ENV: Mutex<()> = Mutex::new(());

    fn panel(cur: i32, max: i32, f: impl FnOnce(&mut Backlight, &dyn Fn() -> i32)) {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("plat-bl-{}-{cur}-{max}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("brightness"), format!("{cur}\n")).unwrap();
        std::fs::write(dir.join("max_brightness"), format!("{max}\n")).unwrap();
        // SAFETY: the tests that touch the variable hold ENV
        unsafe { std::env::set_var("LUYAZ_UI_BACKLIGHT_DIR", &dir) };
        let mut b = Backlight::init();
        let d = dir.clone();
        let read = move || std::fs::read_to_string(d.join("brightness")).unwrap().trim().parse().unwrap();
        f(&mut b, &read);
        unsafe { std::env::remove_var("LUYAZ_UI_BACKLIGHT_DIR") };
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn starts_lit_and_remembers_the_level() {
        panel(120, 255, |b, read| {
            assert!(b.is_on() && b.panel_lit());
            assert_eq!(b.get(), 120);
            b.off();
            assert_eq!(read(), 0);
            assert!(!b.is_on() && !b.panel_lit());
            assert_eq!(b.get(), 120);
            b.on();
            assert_eq!(read(), 120);
            assert!(b.is_on());
        });
    }

    #[test]
    fn starts_dark_and_comes_on_at_max() {
        panel(0, 200, |b, read| {
            assert!(!b.is_on());
            assert_eq!(b.get(), 200);
            assert_eq!(b.max(), 200);
            b.on();
            assert_eq!(read(), 200);
        });
    }

    #[test]
    fn set_clamps_and_zero_is_off() {
        panel(50, 100, |b, read| {
            b.set(500);
            assert_eq!(read(), 100);
            assert_eq!(b.get(), 100);
            b.set(-3);
            assert_eq!(read(), 0);
            assert!(!b.is_on());
            assert_eq!(b.get(), 100);
            b.set(30);
            assert_eq!((read(), b.get(), b.is_on()), (30, 30, true));
        });
    }

    #[test]
    fn remember_and_follow_the_panel() {
        panel(0, 255, |b, read| {
            b.remember(0);
            assert_eq!(b.get(), 255);
            b.remember(80);
            assert_eq!(b.get(), 80);
            assert_eq!(read(), 0);
            assert!(!b.is_lit());
            b.on();
            assert_eq!(read(), 80);
            b.off();
            b.on();
            assert_eq!(read(), 80);
        });
    }

    #[test]
    fn off_while_already_dark_keeps_the_level() {
        panel(90, 255, |b, read| {
            b.off();
            b.off();
            b.on();
            assert_eq!(read(), 90);
        });
    }
}
