//! The panel backlight, through the LED class device's `brightness` and `max_brightness`. Turning
//! it off keeps the level it had so on brings that back. `is_on` is what we last did; the vendor
//! key daemon sees the power key too, so the key handler asks the panel (`is_lit`) and resyncs.
//!
//! A start from a lit panel goes dark until the first frame is on it ([`hold_dark`], issue #11):
//! between the vendor UI letting go and our first frame the panel shows black, then noise (the
//! modeset), then black again. [`Hold`] keeps the level it took from the panel in a marker file so
//! a UI that dies in that window leaves it to the next one (or to the sentinel, which hands the
//! panel back lit at it). The level the hold lights to is another one: what the UI made its "on"
//! level meanwhile ([`Backlight::remember`]: `ui.conf`'s `bright=` replacing the vendor's boot
//! preset, issue #10), else the one taken. The marker keeps the taken one.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const LED: &str = "/sys/class/leds/led:lcd";
/// what a panel without a readable max is assumed to go up to
const DEFAULT_MAX: i32 = 255;
/// The level a dark-until-the-first-frame start took from the panel. luyaz-sentinel reads the
/// same path (`sys::BL_HOLD`): keep the two equal.
pub const HOLD_MARKER: &str = "/tmp/luyaz/ui-bl-hold";
/// The longest the panel stays dark waiting for the first frame; then it comes on anyway.
pub const HOLD_MAX_MS: u64 = 8000;

pub struct Backlight {
    level_file: PathBuf,
    max_file: PathBuf,
    /// the level "on" means
    on_level: i32,
    is_on: bool,
    /// dark until the first frame (None = no hold, or it ended)
    hold: Option<Hold>,
}

/// The first number in a sysfs file.
fn read_num(path: &Path) -> Option<i32> {
    std::fs::read_to_string(path).ok()?.split_whitespace().next()?.parse().ok()
}

/// The LED directory: LUYAZ_UI_BACKLIGHT_DIR or the device's own.
fn led_dir() -> PathBuf {
    PathBuf::from(std::env::var("LUYAZ_UI_BACKLIGHT_DIR").unwrap_or_else(|_| LED.into()))
}

/// Write `level` and read it back: Ok only when the panel took it.
fn write_checked(level_file: &Path, level: i32) -> std::io::Result<()> {
    std::fs::write(level_file, level.to_string())?;
    match read_num(level_file) {
        Some(v) if (v > 0) == (level > 0) => Ok(()),
        v => Err(std::io::Error::other(format!("read back {v:?} after writing {level}"))),
    }
}

/// What a hold keeps: the level to come back to (the taken one until [`Hold::retarget`]) and
/// where the taken one is written down.
struct HoldState {
    level: i32,
    level_file: PathBuf,
    marker: PathBuf,
}

impl HoldState {
    /// The panel back on at the level to come back to. Someone else's write in the window (lit
    /// at another level, or dark) is replaced, never taken over: the "on" level stays ours, so
    /// the panel and the slider agree (issue #10). The marker goes only once the panel is lit, so
    /// a failed write leaves it for the next UI or the sentinel. Ok(the level the panel is at).
    fn restore(&self) -> std::io::Result<i32> {
        if read_num(&self.level_file) != Some(self.level) {
            write_checked(&self.level_file, self.level)?;
        }
        let _ = std::fs::remove_file(&self.marker);
        Ok(self.level)
    }
}

type Shared = Arc<(Mutex<Option<HoldState>>, Condvar)>;

fn lock(s: &Shared) -> std::sync::MutexGuard<'_, Option<HoldState>> {
    s.0.lock().unwrap_or_else(|e| e.into_inner())
}

/// The panel held dark until the first frame. One lock serialises everything that ends it (the
/// first frame, a key, the timeout, an exit), so the timeout can never light a panel a key has
/// just turned off. Dropped while still held (the panel could not be opened): the level back.
pub struct Hold {
    shared: Shared,
    since: Instant,
}

/// Before the panel is opened: a lit panel goes dark, its level written to the marker first (no
/// marker, no dark: a UI dying in the window would leave the panel off for good). A dark panel
/// with a marker is a hold the last UI did not finish: taken over. A dark panel without one stays
/// as it is (a start in the dark). The timeout runs from here.
pub fn hold_dark() -> Option<Hold> {
    let marker = std::env::var_os("LUYAZ_UI_BL_HOLD").map_or_else(|| PathBuf::from(HOLD_MARKER), PathBuf::from);
    hold_dark_at(&led_dir(), &marker, Duration::from_millis(HOLD_MAX_MS))
}

fn hold_dark_at(dir: &Path, marker: &Path, max: Duration) -> Option<Hold> {
    let level_file = dir.join("brightness");
    let cur = read_num(&level_file).unwrap_or(0);
    let level = if cur > 0 {
        let written = marker.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(marker, format!("{cur}\n")));
        if let Err(e) = written {
            eprintln!("bl: no hold ({}: {e}), the panel stays lit", marker.display());
            return None;
        }
        if let Err(e) = write_checked(&level_file, 0) {
            // whatever it is at now, back to where it was
            let _ = std::fs::write(&level_file, cur.to_string());
            let _ = std::fs::remove_file(marker);
            eprintln!("bl: no hold (cannot go dark: {e})");
            return None;
        }
        eprintln!("bl: dark until the first frame (was {cur})");
        cur
    } else {
        let left = std::fs::read_to_string(marker).ok()?;
        match left.trim().parse::<i32>() {
            Ok(v) if v > 0 => {
                let max = read_num(&dir.join("max_brightness")).filter(|&m| m > 0).unwrap_or(DEFAULT_MAX);
                let v = v.min(max);
                eprintln!("bl: dark until the first frame (the last UI's hold, {v})");
                v
            }
            _ => {
                let _ = std::fs::remove_file(marker);
                return None;
            }
        }
    };
    let shared: Shared = Arc::new((Mutex::new(Some(HoldState { level, level_file, marker: marker.to_path_buf() })), Condvar::new()));
    let since = Instant::now();
    let s = shared.clone();
    let spawned = std::thread::Builder::new().name("bl-hold".into()).spawn(move || {
        let deadline = since + max;
        let mut g = lock(&s);
        while g.is_some() {
            let now = Instant::now();
            if now >= deadline {
                if let Some(st) = g.take() {
                    match st.restore() {
                        Ok(l) => eprintln!("bl: lit {l} after the {} ms timeout, no first frame", max.as_millis()),
                        Err(e) => eprintln!("bl: timeout: cannot light the panel ({e}), marker kept"),
                    }
                }
                return;
            }
            g = s.1.wait_timeout(g, deadline - now).unwrap_or_else(|e| e.into_inner()).0;
        }
    });
    let h = Hold { shared, since };
    if spawned.is_err() {
        // no timeout to fall back on: no hold either
        h.release("(no timeout thread)");
        return None;
    }
    Some(h)
}

impl Hold {
    /// Still dark, waiting: the level the panel comes back to. None once the hold ended.
    pub fn level(&self) -> Option<i32> {
        lock(&self.shared).as_ref().map(|st| st.level)
    }

    /// Still dark, waiting.
    pub fn active(&self) -> bool {
        self.level().is_some()
    }

    /// Still dark: the panel comes back at `level` instead (the first frame, the timeout, an
    /// exit alike). The marker keeps the level taken from the panel: what the sentinel hands
    /// back to the vendor UI is the vendor's own. Nothing once the hold ended.
    fn retarget(&self, level: i32) {
        if let Some(st) = lock(&self.shared).as_mut() {
            st.level = level;
        }
    }

    /// The panel on (see [`HoldState::restore`]); `why` goes in the log ("after first frame": the
    /// line a device test times). Some(level) when this call ended the hold and the panel is
    /// lit; None when it had ended already or the write failed (marker kept).
    pub fn release(&self, why: &str) -> Option<i32> {
        let mut g = lock(&self.shared);
        let st = g.take()?;
        self.shared.1.notify_all();
        let ms = self.since.elapsed().as_millis();
        match st.restore() {
            Ok(l) => {
                eprintln!("bl: lit {l} {why} {ms} ms");
                Some(l)
            }
            Err(e) => {
                eprintln!("bl: cannot light the panel {why} {ms} ms ({e}), marker kept");
                None
            }
        }
    }

    /// Something decided the panel itself (a key, a set): the hold ends and `then` writes, under
    /// the same lock as the timeout.
    fn end_then(&self, then: impl FnOnce()) {
        let mut g = lock(&self.shared);
        if let Some(st) = g.take() {
            let _ = std::fs::remove_file(&st.marker);
            self.shared.1.notify_all();
        }
        then();
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        self.release("on the way out");
    }
}

impl Backlight {
    /// LUYAZ_UI_BACKLIGHT_DIR points at another directory holding the same two files (the
    /// offscreen smoke test; never set on the device).
    pub fn init() -> Backlight {
        Backlight::init_at(&led_dir())
    }

    fn init_at(dir: &Path) -> Backlight {
        let mut b = Backlight { level_file: dir.join("brightness"), max_file: dir.join("max_brightness"), on_level: DEFAULT_MAX, is_on: false, hold: None };
        match b.level() {
            lit if lit > 0 => (b.on_level, b.is_on) = (lit, true),
            _ => b.on_level = b.max(),
        }
        b
    }

    /// What the panel is at now (0 when unreadable); held dark, the level it comes back to.
    fn level(&self) -> i32 {
        match self.hold.as_ref().and_then(Hold::level) {
            Some(l) => l,
            None => read_num(&self.level_file).unwrap_or(0),
        }
    }

    /// Any write of ours ends a hold first (a key during the dark start still turns it off).
    fn write(&mut self, level: i32) {
        let f = &self.level_file;
        match self.hold.take() {
            Some(h) => h.end_then(|| {
                let _ = std::fs::write(f, level.to_string());
            }),
            None => {
                let _ = std::fs::write(f, level.to_string());
            }
        }
    }

    /// Dark until the first frame ([`hold_dark`]): until [`Backlight::release`], everything asks
    /// as if the panel were lit at the held level, so nothing treats the start as a dark screen.
    pub fn hold(&mut self, h: Hold) {
        if let Some(l) = h.level() {
            self.on_level = l;
        }
        self.is_on = true;
        self.hold = Some(h);
    }

    /// Still waiting for the first frame.
    pub fn is_held(&self) -> bool {
        self.hold.as_ref().is_some_and(Hold::active)
    }

    /// The first frame is up (or the program ends; `why` says which, for the log): the panel on at
    /// the level held for it ([`HoldState::restore`]). Nothing when no hold is on.
    pub fn release(&mut self, why: &str) {
        let Some(h) = self.hold.take() else { return };
        match h.release(why) {
            Some(l) => {
                self.remember(l);
                self.is_on = true;
            }
            None => self.is_on = read_num(&self.level_file).unwrap_or(0) > 0,
        }
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

    /// Make `level` the "on" level without touching the panel (a start in the dark; held dark
    /// until the first frame, the level it comes up at); 0 is ignored.
    pub fn remember(&mut self, level: i32) {
        if level > 0 {
            self.on_level = level;
            if let Some(h) = &self.hold {
                h.retarget(level);
            }
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
    fn remember_and_on_replaces_boot_preset_when_lit() {
        panel(3, 255, |b, read| {
            assert!(b.panel_lit());
            assert_eq!(b.get(), 3);
            b.remember(232);
            b.on();
            assert_eq!((read(), b.get(), b.is_on()), (232, 232, true));
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

    // ---- dark until the first frame ----

    const LONG: Duration = Duration::from_secs(600);

    /// A panel directory at `cur` (max 255) and a marker path beside it, both fresh.
    fn hold_dir(tag: &str, cur: i32) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("plat-hold-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("brightness"), format!("{cur}\n")).unwrap();
        std::fs::write(dir.join("max_brightness"), "255\n").unwrap();
        let marker = dir.join("tmp").join("ui-bl-hold");
        (dir, marker)
    }

    fn cur(dir: &Path) -> i32 {
        read_num(&dir.join("brightness")).unwrap()
    }

    fn marker(m: &Path) -> Option<String> {
        std::fs::read_to_string(m).ok().map(|s| s.trim().to_string())
    }

    #[test]
    fn hold_lit_panel_goes_dark_marker_first() {
        let (dir, m) = hold_dir("lit", 120);
        let h = hold_dark_at(&dir, &m, LONG).expect("held");
        assert_eq!((h.level(), cur(&dir), marker(&m).as_deref()), (Some(120), 0, Some("120")));
        assert!(h.active());
        assert_eq!(h.release("t"), Some(120));
        assert_eq!((cur(&dir), marker(&m)), (120, None));
    }

    #[test]
    fn hold_dark_panel_without_marker_is_left_alone() {
        let (dir, m) = hold_dir("dark", 0);
        assert!(hold_dark_at(&dir, &m, LONG).is_none());
        assert_eq!((cur(&dir), marker(&m)), (0, None));
    }

    #[test]
    fn hold_takes_over_the_last_uis_marker() {
        let (dir, m) = hold_dir("left", 0);
        std::fs::create_dir_all(m.parent().unwrap()).unwrap();
        std::fs::write(&m, "90\n").unwrap();
        let h = hold_dark_at(&dir, &m, LONG).expect("held");
        assert_eq!((h.level(), cur(&dir)), (Some(90), 0));
        assert_eq!(h.release("t"), Some(90));
        assert_eq!((cur(&dir), marker(&m)), (90, None));
    }

    #[test]
    fn hold_marker_above_max_is_clamped_and_junk_is_dropped() {
        let (dir, m) = hold_dir("junk", 0);
        std::fs::create_dir_all(m.parent().unwrap()).unwrap();
        std::fs::write(&m, "999\n").unwrap();
        let h = hold_dark_at(&dir, &m, LONG).expect("held");
        assert_eq!(h.level(), Some(255));
        drop(h);
        assert_eq!(cur(&dir), 255);
        for junk in ["0", "-4", "abc", ""] {
            std::fs::write(dir.join("brightness"), "0\n").unwrap();
            std::fs::write(&m, junk).unwrap();
            assert!(hold_dark_at(&dir, &m, LONG).is_none(), "{junk:?}");
            assert_eq!((cur(&dir), marker(&m)), (0, None), "{junk:?}");
        }
    }

    #[test]
    fn hold_without_a_writable_marker_stays_lit() {
        let (dir, _) = hold_dir("nomarker", 120);
        // the marker's directory would have to be a file
        let m = dir.join("brightness").join("ui-bl-hold");
        assert!(hold_dark_at(&dir, &m, LONG).is_none());
        assert_eq!(cur(&dir), 120);
    }

    #[test]
    fn held_asks_as_lit_at_the_held_level() {
        let (dir, m) = hold_dir("asks", 120);
        let h = hold_dark_at(&dir, &m, LONG).unwrap();
        let mut b = Backlight::init_at(&dir);
        assert!(!b.is_on(), "init saw the dark panel");
        b.hold(h);
        assert!(b.is_held() && b.is_on() && b.panel_lit() && b.is_lit());
        assert_eq!(b.get(), 120);
        assert_eq!(cur(&dir), 0, "nothing written by asking");
        b.release("t");
        assert!(!b.is_held() && b.is_on());
        assert_eq!((cur(&dir), marker(&m)), (120, None));
        b.release("t"); // again: nothing
        assert_eq!(cur(&dir), 120);
    }

    #[test]
    fn release_replaces_what_someone_else_wrote() {
        // a level written by someone else in the window is not taken over (no adopt, issue #10)
        for (tag, other) in [("other-lit", "3"), ("other-dark", "0")] {
            let (dir, m) = hold_dir(tag, 120);
            let mut b = Backlight::init_at(&dir);
            b.hold(hold_dark_at(&dir, &m, LONG).unwrap());
            b.remember(232);
            std::fs::write(dir.join("brightness"), format!("{other}\n")).unwrap();
            b.release("t");
            assert_eq!((cur(&dir), b.get(), b.is_on(), marker(&m)), (232, 232, true, None), "{tag}");
        }
    }

    // ---- the saved level over the vendor's boot preset (issue #10 inside #11's hold) ----

    /// The vendor's boot preset 3 held dark, `ui.conf`'s 232 remembered.
    fn preset_held(tag: &str, max: Duration) -> (PathBuf, PathBuf, Backlight) {
        let (dir, m) = hold_dir(tag, 3);
        let mut b = Backlight::init_at(&dir);
        b.hold(hold_dark_at(&dir, &m, max).unwrap());
        b.remember(232);
        (dir, m, b)
    }

    #[test]
    fn preset_held_stays_dark_and_comes_up_at_the_saved_level() {
        let (dir, m, mut b) = preset_held("preset-release", LONG);
        assert!(b.is_held() && b.is_on() && b.panel_lit());
        assert_eq!((cur(&dir), b.get(), marker(&m).as_deref()), (0, 232, Some("3")), "dark; the marker keeps the vendor's 3");
        b.release("t");
        assert_eq!((cur(&dir), b.get(), b.is_on(), marker(&m)), (232, 232, true, None));
    }

    #[test]
    fn preset_held_timeout_lights_the_saved_level() {
        let (dir, m, b) = preset_held("preset-timeout", Duration::from_millis(50));
        std::thread::sleep(Duration::from_millis(400));
        assert!(!b.is_held());
        assert_eq!((cur(&dir), marker(&m)), (232, None));
    }

    #[test]
    fn preset_held_off_then_on_wakes_to_the_saved_level() {
        let (dir, m, mut b) = preset_held("preset-off", LONG);
        b.off();
        assert_eq!((cur(&dir), b.get(), b.is_on(), marker(&m)), (0, 232, false, None));
        b.on();
        assert_eq!(cur(&dir), 232);
    }

    #[test]
    fn preset_held_dropped_comes_back_at_the_target() {
        // dropped before anything was remembered (no panel, say): the vendor's level
        let (dir, m) = hold_dir("preset-drop", 3);
        drop(hold_dark_at(&dir, &m, LONG).unwrap());
        assert_eq!((cur(&dir), marker(&m)), (3, None));
        // dropped (the way out) after the saved level was remembered: that one
        let (dir, m, b) = preset_held("preset-drop2", LONG);
        drop(b);
        assert_eq!((cur(&dir), marker(&m)), (232, None));
    }

    #[test]
    fn remember_after_the_hold_ended_does_not_touch_the_panel() {
        let (dir, _m, mut b) = preset_held("preset-after", LONG);
        b.release("t");
        b.remember(100);
        assert_eq!((cur(&dir), b.get()), (232, 100));
    }

    #[test]
    fn off_during_the_hold_ends_it_and_the_timeout_stays_out() {
        let (dir, m) = hold_dir("off", 120);
        let mut b = Backlight::init_at(&dir);
        b.hold(hold_dark_at(&dir, &m, Duration::from_millis(100)).unwrap());
        b.off();
        assert!(!b.is_held() && !b.is_on());
        assert_eq!((cur(&dir), marker(&m)), (0, None));
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(cur(&dir), 0, "the timeout must not light a panel the key turned off");
        b.on();
        assert_eq!(cur(&dir), 120);
    }

    #[test]
    fn set_during_the_hold_ends_it() {
        let (dir, m) = hold_dir("set", 120);
        let mut b = Backlight::init_at(&dir);
        b.hold(hold_dark_at(&dir, &m, LONG).unwrap());
        b.set(60);
        assert!(!b.is_held());
        assert_eq!((cur(&dir), marker(&m)), (60, None));
    }

    #[test]
    fn the_timeout_lights_a_panel_that_got_no_frame() {
        let (dir, m) = hold_dir("timeout", 120);
        let mut b = Backlight::init_at(&dir);
        b.hold(hold_dark_at(&dir, &m, Duration::from_millis(50)).unwrap());
        std::thread::sleep(Duration::from_millis(400));
        assert!(!b.is_held());
        assert_eq!((cur(&dir), marker(&m)), (120, None));
        b.release("t"); // after the timeout: nothing more
        assert!(b.is_on() && b.panel_lit());
        assert_eq!(cur(&dir), 120);
    }

    #[test]
    fn dropped_while_held_comes_back_on() {
        let (dir, m) = hold_dir("drop", 120);
        drop(hold_dark_at(&dir, &m, LONG).unwrap());
        assert_eq!((cur(&dir), marker(&m)), (120, None));
    }

    #[test]
    fn a_failed_restore_keeps_the_marker() {
        let (dir, m) = hold_dir("fail", 0);
        std::fs::create_dir_all(m.parent().unwrap()).unwrap();
        std::fs::write(&m, "120\n").unwrap();
        let st = HoldState { level: 120, level_file: dir.join("gone").join("brightness"), marker: m.clone() };
        assert!(st.restore().is_err());
        assert_eq!(marker(&m).as_deref(), Some("120"));
    }
}
