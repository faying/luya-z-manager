//! The touchscreen: the first input device with X/Y axes (multitouch ones preferred), read without
//! blocking, one (x, y, pressed) per SYN_REPORT, turned 180° for the upside-down panel. Press and
//! release edges are queued and handed out one per read, so every press is reported pressed for at
//! least one read before its release, however short it was (a touchsim tap, a quick finger, a long
//! LVGL frame) and however many taps landed between two reads; reports are counted for the 性能测试
//! page.

use crate::evdev::{self, AbsInfo, Device, EV_ABS, EV_KEY, EV_SYN};
use std::collections::VecDeque;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicU64, Ordering};

const SYN_REPORT: u16 = 0x00;
const BTN_TOUCH: u16 = 0x14a;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;
/// edges kept for the reads to come; more than this between two reads drops the oldest pair
const MAX_EDGES: usize = 16;

/// SYN_REPORTs seen since start.
pub static REPORTS: AtomicU64 = AtomicU64::new(0);

pub fn report_count() -> u64 {
    REPORTS.load(Ordering::Relaxed)
}

/// `v` within `range` mapped onto 0..=out_max (rounding down); an empty range maps to 0.
fn scale(v: i32, (lo, hi): (i32, i32), out_max: i32) -> i32 {
    if hi <= lo {
        return 0;
    }
    let span = (hi - lo) as i64;
    ((v.clamp(lo, hi) - lo) as i64 * out_max as i64 / span) as i32
}

pub struct Touch {
    fd: Option<OwnedFd>,
    w: i32,
    h: i32,
    x_range: (i32, i32),
    y_range: (i32, i32),
    use_mt: bool,
    pub raw_cur_x: i32,
    pub raw_cur_y: i32,
    cur_x: i32,
    cur_y: i32,
    /// the touch state as of the last SYN_REPORT
    pressed: bool,
    /// BTN_TOUCH / tracking id seen since that SYN_REPORT
    new_press: bool,
    /// press/release edges not yet handed to a read: (x, y, pressed)
    edges: VecDeque<(i32, i32, bool)>,
    /// what the last read reported
    out_pressed: bool,
    rot180: bool,
}

/// What a touchscreen candidate offers: multitouch or plain axes, nothing usable = None.
fn axes_of(dev: &Device) -> Option<bool> {
    let abs = dev.codes::<1>(EV_ABS)?;
    let both = |x: u16, y: u16| evdev::has_bit(&abs, x as usize) && evdev::has_bit(&abs, y as usize);
    if both(ABS_MT_POSITION_X, ABS_MT_POSITION_Y) {
        Some(true)
    } else if both(ABS_X, ABS_Y) {
        Some(false)
    } else {
        None
    }
}

impl Touch {
    /// Find and open the touchscreen, scaled to w × h. Without one the UI still draws, untouchable.
    pub fn open(w: i32, h: i32) -> Touch {
        let mut t = Touch::none(w, h);
        let found = evdev::nodes().into_iter().find_map(|path| {
            let dev = Device::open(&path)?;
            axes_of(&dev).map(|mt| (path, dev, mt))
        });
        let Some((path, dev, mt)) = found else {
            eprintln!("touch: no touchscreen found under /dev/input");
            return t;
        };
        t.use_mt = mt;
        t.take_ranges(&dev);
        eprintln!(
            "touch: using {} ({}, x[{}..{}] y[{}..{}])",
            path.display(),
            if mt { "MT" } else { "ST" },
            t.x_range.0,
            t.x_range.1,
            t.y_range.0,
            t.y_range.1
        );
        t.fd = Some(dev.into_fd());
        t
    }

    /// The axis codes positions arrive on.
    fn axes(&self) -> (u16, u16) {
        if self.use_mt {
            (ABS_MT_POSITION_X, ABS_MT_POSITION_Y)
        } else {
            (ABS_X, ABS_Y)
        }
    }

    /// Ranges and where the finger last was. The current value matters: evdev does not repeat a
    /// value equal to the last one it sent, so a tap on the same row as the one before arrives
    /// with no Y at all.
    fn take_ranges(&mut self, dev: &Device) {
        let (ax, ay) = self.axes();
        let usable = |a: AbsInfo| (a.maximum > a.minimum).then_some(a);
        if let Some(a) = dev.axis(ax).and_then(usable) {
            (self.x_range, self.raw_cur_x) = ((a.minimum, a.maximum), a.value);
        }
        if let Some(a) = dev.axis(ay).and_then(usable) {
            (self.y_range, self.raw_cur_y) = ((a.minimum, a.maximum), a.value);
        }
        if !self.use_mt {
            return;
        }
        // with multitouch the live position is per slot: take the active slot's
        let slot = dev.axis(ABS_MT_SLOT).map(|s| s.value).filter(|v| (0..16).contains(v)).unwrap_or(0) as usize;
        if let Some(v) = dev.slot_values(ax) {
            self.raw_cur_x = v[slot];
        }
        if let Some(v) = dev.slot_values(ay) {
            self.raw_cur_y = v[slot];
        }
    }

    /// No touchscreen (offscreen runs).
    pub fn none(w: i32, h: i32) -> Touch {
        Touch {
            fd: None,
            w,
            h,
            x_range: (0, w - 1),
            y_range: (0, h - 1),
            use_mt: false,
            raw_cur_x: 0,
            raw_cur_y: 0,
            cur_x: 0,
            cur_y: 0,
            pressed: false,
            new_press: false,
            edges: VecDeque::new(),
            out_pressed: false,
            rot180: true,
        }
    }

    pub fn fd(&self) -> i32 {
        self.fd.as_ref().map_or(-1, |f| f.as_raw_fd())
    }

    /// Take in what is waiting, then say what this read reports, in display pixels: the oldest
    /// edge not reported yet, else the latest position and state.
    pub fn read(&mut self) -> (i32, i32, bool) {
        match self.fd.as_ref().map(|f| f.as_raw_fd()) {
            Some(fd) => evdev::drain(fd, |e| self.event(e.kind, e.code, e.value)),
            None if self.edges.is_empty() => return (self.cur_x, self.cur_y, false),
            None => {}
        }
        self.report()
    }

    /// Edges not handed out yet (read again before waiting for the next period).
    pub fn pending(&self) -> bool {
        self.edges.iter().any(|e| e.2 != self.out_pressed)
    }

    /// One evdev event: positions on the axes we chose, contact from BTN_TOUCH or the MT
    /// tracking id, the frame closed by SYN_REPORT.
    fn event(&mut self, kind: u16, code: u16, value: i32) {
        let (ax, ay) = self.axes();
        if kind == EV_ABS && code == ax {
            self.raw_cur_x = value;
        } else if kind == EV_ABS && code == ay {
            self.raw_cur_y = value;
        } else if kind == EV_ABS && code == ABS_MT_TRACKING_ID {
            self.new_press = value >= 0;
        } else if kind == EV_KEY && code == BTN_TOUCH {
            self.new_press = value != 0;
        } else if kind == EV_SYN && code == SYN_REPORT {
            self.syn();
        }
    }

    /// A SYN_REPORT: the frame is complete.
    fn syn(&mut self) {
        let mut sx = scale(self.raw_cur_x, self.x_range, self.w - 1);
        let mut sy = scale(self.raw_cur_y, self.y_range, self.h - 1);
        if self.rot180 {
            sx = (self.w - 1) - sx;
            sy = (self.h - 1) - sy;
        }
        self.cur_x = sx;
        self.cur_y = sy;
        if self.new_press != self.pressed {
            self.pressed = self.new_press;
            if self.edges.len() >= MAX_EDGES {
                // keep the queue in press/release pairs
                self.edges.pop_front();
                self.edges.pop_front();
            }
            self.edges.push_back((sx, sy, self.pressed));
        }
        REPORTS.fetch_add(1, Ordering::Relaxed);
    }

    /// The state for this read: one queued edge at a time (an edge equal to what was reported last,
    /// after a dropped pair, is skipped); a press still going on reports where the finger is now.
    fn report(&mut self) -> (i32, i32, bool) {
        while let Some((x, y, p)) = self.edges.pop_front() {
            if p == self.out_pressed {
                continue;
            }
            self.out_pressed = p;
            if p && self.edges.is_empty() {
                // the press is still on: where the finger is now
                return (self.cur_x, self.cur_y, true);
            }
            return (x, y, p);
        }
        self.out_pressed = self.pressed;
        (self.cur_x, self.cur_y, self.pressed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_and_turns() {
        assert_eq!(scale(0, (0, 4095), 319), 0);
        assert_eq!(scale(4095, (0, 4095), 319), 319);
        assert_eq!(scale(5000, (0, 4095), 319), 319);
        assert_eq!(scale(-7, (0, 4095), 319), 0);
        assert_eq!(scale(2048, (0, 4095), 319), 159);
        assert_eq!(scale(10, (10, 10), 319), 0);
    }

    /// A 320 × 480 MT panel with raw = display range and no rotation, so raw = display pixels.
    fn mt() -> Touch {
        let mut t = Touch::none(320, 480);
        t.use_mt = true;
        t.rot180 = false;
        t
    }

    /// touchsim's tap press frame
    fn down(t: &mut Touch, x: i32, y: i32) {
        t.event(EV_ABS, ABS_MT_TRACKING_ID, 0);
        t.event(EV_ABS, ABS_MT_POSITION_X, x);
        t.event(EV_ABS, ABS_MT_POSITION_Y, y);
        t.event(EV_KEY, BTN_TOUCH, 1);
        t.event(EV_SYN, SYN_REPORT, 0);
    }

    /// touchsim's tap release frame (no position)
    fn up(t: &mut Touch) {
        t.event(EV_KEY, BTN_TOUCH, 0);
        t.event(EV_ABS, ABS_MT_TRACKING_ID, -1);
        t.event(EV_SYN, SYN_REPORT, 0);
    }

    fn mv(t: &mut Touch, x: i32, y: i32) {
        t.event(EV_ABS, ABS_MT_POSITION_X, x);
        t.event(EV_ABS, ABS_MT_POSITION_Y, y);
        t.event(EV_SYN, SYN_REPORT, 0);
    }

    #[test]
    fn tap_across_two_reads() {
        let mut t = mt();
        assert_eq!(t.report(), (0, 0, false));
        down(&mut t, 100, 200);
        assert_eq!(t.report(), (100, 200, true));
        assert_eq!(t.report(), (100, 200, true));
        up(&mut t);
        assert_eq!(t.report(), (100, 200, false));
        assert_eq!(t.report(), (100, 200, false));
    }

    #[test]
    fn whole_tap_inside_one_read_is_pressed_then_released() {
        let mut t = mt();
        down(&mut t, 100, 200);
        up(&mut t);
        assert!(t.pending());
        assert_eq!(t.report(), (100, 200, true));
        assert!(t.pending());
        assert_eq!(t.report(), (100, 200, false));
        assert!(!t.pending());
        assert_eq!(t.report(), (100, 200, false));
    }

    #[test]
    fn two_whole_taps_inside_one_read_are_both_reported() {
        let mut t = mt();
        down(&mut t, 100, 200);
        up(&mut t);
        down(&mut t, 30, 40);
        up(&mut t);
        assert_eq!(t.report(), (100, 200, true));
        assert_eq!(t.report(), (100, 200, false));
        assert_eq!(t.report(), (30, 40, true));
        assert_eq!(t.report(), (30, 40, false));
        assert_eq!(t.report(), (30, 40, false));
    }

    #[test]
    fn a_new_press_right_after_a_whole_tap_still_releases_first() {
        let mut t = mt();
        down(&mut t, 100, 200);
        up(&mut t);
        assert_eq!(t.report(), (100, 200, true));
        down(&mut t, 30, 40);
        assert_eq!(t.report(), (100, 200, false));
        assert_eq!(t.report(), (30, 40, true));
        up(&mut t);
        assert_eq!(t.report(), (30, 40, false));
    }

    #[test]
    fn a_finger_that_moves_reports_where_it_is() {
        let mut t = mt();
        down(&mut t, 100, 200);
        mv(&mut t, 110, 210);
        // the press is still on: the latest position, as before
        assert_eq!(t.report(), (110, 210, true));
        mv(&mut t, 150, 210);
        mv(&mut t, 200, 210);
        assert_eq!(t.report(), (200, 210, true));
        mv(&mut t, 220, 210);
        up(&mut t);
        assert_eq!(t.report(), (220, 210, false));
    }

    #[test]
    fn a_drag_that_ends_inside_one_read_presses_where_it_began() {
        let mut t = mt();
        down(&mut t, 100, 200);
        mv(&mut t, 160, 200);
        up(&mut t);
        assert_eq!(t.report(), (100, 200, true));
        assert_eq!(t.report(), (160, 200, false));
    }

    #[test]
    fn single_touch_btn_only_and_rotation() {
        let mut t = Touch::none(320, 480);
        t.event(EV_ABS, ABS_X, 0);
        t.event(EV_ABS, ABS_Y, 0);
        t.event(EV_KEY, BTN_TOUCH, 1);
        t.event(EV_SYN, SYN_REPORT, 0);
        t.event(EV_KEY, BTN_TOUCH, 0);
        t.event(EV_SYN, SYN_REPORT, 0);
        assert_eq!(t.report(), (319, 479, true));
        assert_eq!(t.report(), (319, 479, false));
    }

    #[test]
    fn a_flood_of_taps_keeps_pairs_and_ends_released() {
        let mut t = mt();
        for i in 0..20 {
            down(&mut t, i, i);
            up(&mut t);
        }
        let mut last = false;
        let mut n = 0;
        loop {
            let (_, _, p) = t.report();
            if !p && !last {
                break;
            }
            assert_ne!(p, last, "edges alternate");
            last = p;
            n += 1;
        }
        assert_eq!(n, MAX_EDGES);
        assert_eq!(t.report(), (19, 19, false));
    }

    #[test]
    fn read_without_a_device_drains_nothing() {
        let mut t = mt();
        assert_eq!(t.read(), (0, 0, false));
    }
}
