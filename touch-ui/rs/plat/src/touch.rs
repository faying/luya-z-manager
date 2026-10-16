//! The touchscreen (src/touch_input.c): the first /dev/input/event* with absolute X/Y (multitouch
//! preferred), drained without blocking, one (x, y, pressed) per SYN_REPORT, turned 180° for the
//! upside-down panel. A whole tap that came and went inside one read is latched (lost_tap) so
//! the caller can still report it; reports are counted for the 性能测试 page.
// SPDX-License-Identifier: MIT

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicU64, Ordering};

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;
const SYN_REPORT: u16 = 0x00;
const BTN_TOUCH: u16 = 0x14a;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;
/// arm64 input_event: timeval (16 bytes), type, code, value
const EV_SIZE: usize = 24;
/// a press that moved at most this far is a tap
const TAP_SLOP: i32 = 14;

/// SYN_REPORT samples seen (touch_input_report_count).
pub static REPORTS: AtomicU64 = AtomicU64::new(0);

pub fn report_count() -> u64 {
    REPORTS.load(Ordering::Relaxed)
}

#[repr(C)]
#[derive(Default)]
struct AbsInfo {
    value: i32,
    min: i32,
    max: i32,
    fuzz: i32,
    flat: i32,
    res: i32,
}

pub(crate) const fn ioc_read(nr: u64, size: usize) -> u64 {
    (2 << 30) | ((size as u64) << 16) | ((b'E' as u64) << 8) | nr
}

pub(crate) fn test_bit(bits: &[u64], bit: usize) -> bool {
    bits.get(bit / 64).is_some_and(|w| (w >> (bit % 64)) & 1 == 1)
}

/// The /dev/input/event* nodes in directory order (opendir/readdir in C).
pub(crate) fn event_nodes() -> Vec<std::path::PathBuf> {
    let Ok(d) = std::fs::read_dir("/dev/input") else { return Vec::new() };
    d.filter_map(Result::ok).filter(|e| e.file_name().to_string_lossy().starts_with("event")).map(|e| e.path()).collect()
}

pub(crate) fn open_nonblock(p: &std::path::Path) -> Option<OwnedFd> {
    let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).ok()?;
    // SAFETY: a path we own; a new fd we then own
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC) };
    // SAFETY: fd is ours when >= 0
    (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd) })
}

pub struct Touch {
    fd: Option<OwnedFd>,
    w: i32,
    h: i32,
    raw_min_x: i32,
    raw_max_x: i32,
    raw_min_y: i32,
    raw_max_y: i32,
    use_mt: bool,
    pub raw_cur_x: i32,
    pub raw_cur_y: i32,
    cur_x: i32,
    cur_y: i32,
    pressed: bool,
    press_x: i32,
    press_y: i32,
    in_tap: bool,
    lost_tap: bool,
    lost_x: i32,
    lost_y: i32,
    rot180: bool,
}

impl Touch {
    /// Probe and open the touchscreen, scaled to w × h. None = no touchscreen (the UI still
    /// draws; C: "continuing without touch input").
    pub fn open(w: i32, h: i32) -> Touch {
        let mut t = Touch::none(w, h);
        for p in event_nodes() {
            let Some(fd) = open_nonblock(&p) else { continue };
            let raw = fd.as_raw_fd();
            let mut bits = [0u64; (ABS_MT_TRACKING_ID as usize / 64) + 1];
            // SAFETY: EVIOCGBIT(EV_ABS, len) writes at most len bytes
            if unsafe { libc::ioctl(raw, ioc_read(0x20 + EV_ABS as u64, size_of_val(&bits)) as _, bits.as_mut_ptr()) } < 0 {
                continue;
            }
            let use_mt = if test_bit(&bits, ABS_MT_POSITION_X as usize) && test_bit(&bits, ABS_MT_POSITION_Y as usize) {
                true
            } else if test_bit(&bits, ABS_X as usize) && test_bit(&bits, ABS_Y as usize) {
                false
            } else {
                continue;
            };
            t.use_mt = use_mt;
            // the axis range and its current value: the input core drops a value equal to the
            // last one it sent, so a tap on the previous tap's row carries no Y at all
            let axis = |code: u16, lo: &mut i32, hi: &mut i32, cur: &mut i32| {
                let mut a = AbsInfo::default();
                // SAFETY: EVIOCGABS(code) fills an input_absinfo
                if unsafe { libc::ioctl(raw, ioc_read(0x40 + code as u64, size_of::<AbsInfo>()) as _, &mut a) } == 0 && a.max > a.min {
                    *lo = a.min;
                    *hi = a.max;
                    *cur = a.value;
                }
            };
            let (mut lx, mut hx, mut cx) = (t.raw_min_x, t.raw_max_x, t.raw_cur_x);
            let (mut ly, mut hy, mut cy) = (t.raw_min_y, t.raw_max_y, t.raw_cur_y);
            axis(if use_mt { ABS_MT_POSITION_X } else { ABS_X }, &mut lx, &mut hx, &mut cx);
            axis(if use_mt { ABS_MT_POSITION_Y } else { ABS_Y }, &mut ly, &mut hy, &mut cy);
            if use_mt {
                // multitouch positions live per slot: read the current slot's
                let mut sl = AbsInfo::default();
                let mut slot = 0usize;
                // SAFETY: EVIOCGABS(ABS_MT_SLOT)
                if unsafe { libc::ioctl(raw, ioc_read(0x40 + ABS_MT_SLOT as u64, size_of::<AbsInfo>()) as _, &mut sl) } == 0 && (0..16).contains(&sl.value) {
                    slot = sl.value as usize;
                }
                for (code, out) in [(ABS_MT_POSITION_X, &mut cx), (ABS_MT_POSITION_Y, &mut cy)] {
                    let mut q = [0i32; 17];
                    q[0] = code as i32;
                    // SAFETY: EVIOCGMTSLOTS(len) fills code + 16 values
                    if unsafe { libc::ioctl(raw, ioc_read(0x0a, size_of_val(&q)) as _, q.as_mut_ptr()) } == 0 {
                        *out = q[1 + slot];
                    }
                }
            }
            (t.raw_min_x, t.raw_max_x, t.raw_cur_x, t.raw_min_y, t.raw_max_y, t.raw_cur_y) = (lx, hx, cx, ly, hy, cy);
            eprintln!(
                "touch: using {} ({}, x[{}..{}] y[{}..{}])",
                p.display(),
                if use_mt { "MT" } else { "ST" },
                t.raw_min_x,
                t.raw_max_x,
                t.raw_min_y,
                t.raw_max_y
            );
            t.fd = Some(fd);
            return t;
        }
        eprintln!("touch: no touchscreen found under /dev/input");
        t
    }

    /// An absent touchscreen (offscreen runs).
    pub fn none(w: i32, h: i32) -> Touch {
        Touch {
            fd: None,
            w,
            h,
            raw_min_x: 0,
            raw_max_x: w - 1,
            raw_min_y: 0,
            raw_max_y: h - 1,
            use_mt: false,
            raw_cur_x: 0,
            raw_cur_y: 0,
            cur_x: 0,
            cur_y: 0,
            pressed: false,
            press_x: 0,
            press_y: 0,
            in_tap: false,
            lost_tap: false,
            lost_x: 0,
            lost_y: 0,
            rot180: true,
        }
    }

    pub fn fd(&self) -> i32 {
        self.fd.as_ref().map_or(-1, |f| f.as_raw_fd())
    }

    fn scale(v: i32, lo: i32, hi: i32, out_max: i32) -> i32 {
        if hi <= lo {
            return 0;
        }
        let v = v.clamp(lo, hi);
        ((v - lo) as i64 * out_max as i64 / (hi - lo) as i64) as i32
    }

    /// Drain what is waiting; the latest (x, y, pressed) in display pixels.
    pub fn read(&mut self) -> (i32, i32, bool) {
        let Some(fd) = self.fd.as_ref().map(|f| f.as_raw_fd()) else { return (self.cur_x, self.cur_y, false) };
        let mut new_press = self.pressed;
        let mut down_here = false; // the current press began during this call
        let mut ev = [0u8; EV_SIZE];
        // SAFETY: reading whole events into a local buffer
        while unsafe { libc::read(fd, ev.as_mut_ptr().cast(), EV_SIZE) } == EV_SIZE as isize {
            let kind = u16::from_ne_bytes([ev[16], ev[17]]);
            let code = u16::from_ne_bytes([ev[18], ev[19]]);
            let value = i32::from_ne_bytes([ev[20], ev[21], ev[22], ev[23]]);
            match kind {
                EV_ABS => match code {
                    ABS_MT_POSITION_X if self.use_mt => self.raw_cur_x = value,
                    ABS_MT_POSITION_Y if self.use_mt => self.raw_cur_y = value,
                    ABS_X if !self.use_mt => self.raw_cur_x = value,
                    ABS_Y if !self.use_mt => self.raw_cur_y = value,
                    ABS_MT_TRACKING_ID => new_press = value >= 0,
                    _ => {}
                },
                EV_KEY if code == BTN_TOUCH => new_press = value != 0,
                EV_SYN if code == SYN_REPORT => {
                    let mut sx = Self::scale(self.raw_cur_x, self.raw_min_x, self.raw_max_x, self.w - 1);
                    let mut sy = Self::scale(self.raw_cur_y, self.raw_min_y, self.raw_max_y, self.h - 1);
                    if self.rot180 {
                        sx = (self.w - 1) - sx;
                        sy = (self.h - 1) - sy;
                    }
                    let was = self.pressed;
                    self.cur_x = sx;
                    self.cur_y = sy;
                    self.pressed = new_press;
                    if !was && new_press {
                        self.press_x = sx;
                        self.press_y = sy;
                        self.in_tap = true;
                        down_here = true;
                    } else if was && !new_press && self.in_tap {
                        let (dx, dy) = (sx - self.press_x, sy - self.press_y);
                        if down_here && dx * dx + dy * dy <= TAP_SLOP * TAP_SLOP {
                            // the caller never saw this press
                            self.lost_tap = true;
                            self.lost_x = self.press_x;
                            self.lost_y = self.press_y;
                        }
                        down_here = false;
                        self.in_tap = false;
                    }
                    REPORTS.fetch_add(1, Ordering::Relaxed);
                }
                _ => {}
            }
        }
        (self.cur_x, self.cur_y, self.pressed)
    }

    /// A whole tap inside one read: its press position, once.
    pub fn take_lost_tap(&mut self) -> Option<(i32, i32)> {
        if !self.lost_tap {
            return None;
        }
        self.lost_tap = false;
        Some((self.lost_x, self.lost_y))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_and_turns() {
        assert_eq!(Touch::scale(0, 0, 4095, 319), 0);
        assert_eq!(Touch::scale(4095, 0, 4095, 319), 319);
        assert_eq!(Touch::scale(5000, 0, 4095, 319), 319);
        assert_eq!(Touch::scale(10, 10, 10, 319), 0);
        let mut bits = [0u64; 2];
        bits[0] |= 1 << 0x35;
        assert!(test_bit(&bits, 0x35) && !test_bit(&bits, 0x36));
    }
}
