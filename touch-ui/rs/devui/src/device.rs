//! The device main loop: the panel (DRM dumb buffer, 180°, one DIRTYFB per frame),
//! the touchscreen through the screen's touch gate (with the lost-tap replay), the data side
//! pumped every pass, and the three loop states:
//!
//! - lit and busy: wake every ≤ 8 ms (animations, touch);
//! - lit and quiet (three passes in a row drew nothing, no animation, no finger, no key): the
//!   refresh and touch-read timers pause, the key poll slows to 1 s, and the loop sleeps in
//!   poll() on touch, key and core until the next timer that still runs (≤ 1 s); a touch, a
//!   key, or anything invalidated wakes it at once (LUYAZ_UI_QUIET=0: off);
//! - dark: the same timers pause, the key poll too, and the loop sleeps on the same fds.
//!
//! SIGTERM / SIGINT end it cleanly: the core's pace back to 1 s, the backlight off only when we
//! started a shutdown, the DRM buffer released.
//!
//!   luyaz-ui [--tab=N] [--scroll=Y] [--screen-off[=key]] [--autooff=MS] [--bright=N] [--exec-…]
//!   luyaz-ui --offscreen [--exit-after=MS] [--texts=FILE] [--png=FILE]   (no panel, no input)
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use crate::conf::{Conf, CONF_FILE};
use crate::data::live::Live;
use crate::logic;
use crate::lv::{self, sys, Obj};
use crate::ui;
use plat::touch::Touch;
use plat::{Panel, Rect, H, W};
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static RUN: AtomicBool = AtomicBool::new(true);
/// flushes pushed to the panel (g_frame_count)
static FRAMES: AtomicU64 = AtomicU64::new(0);
/// something asked for a redraw since the last push (cleared by a refresh's last flush)
static INVAL: AtomicBool = AtomicBool::new(false);
static PUSHES: AtomicU64 = AtomicU64::new(0);

pub fn frames() -> u64 {
    FRAMES.load(Ordering::Relaxed)
}

thread_local! {
    static PANEL: RefCell<Option<Panel>> = const { RefCell::new(None) };
    static TOUCH: RefCell<Option<Touch>> = const { RefCell::new(None) };
    static RD: RefCell<ReadState> = const { RefCell::new(ReadState { was: false, fwd_was: false, taplog: false }) };
}

struct ReadState {
    was: bool,
    fwd_was: bool,
    taplog: bool,
}

extern "C" fn request_stop(_sig: libc::c_int) {
    RUN.store(false, Ordering::Relaxed);
}

/// INT and TERM only lower RUN; the loop notices and winds down itself.
fn trap_stop_signals() {
    let handler = request_stop as *const () as libc::sighandler_t;
    for sig in [libc::SIGTERM, libc::SIGINT] {
        // SAFETY: the handler does nothing but an atomic store
        unsafe { libc::signal(sig, handler) };
    }
}

unsafe extern "C" fn tick_cb() -> u32 {
    crate::data::util::mono_ms32()
}

unsafe extern "C" fn inval_cb(_e: *mut sys::lv_event_t) {
    INVAL.store(true, Ordering::Relaxed);
}

/// An area LVGL drew: into the scanout buffer (turned 180°); the refresh's last area pushes the
/// box of them all with one DIRTYFB (drm_qpic costs ~38 ms per DIRTYFB whatever its size).
unsafe extern "C" fn flush_cb(d: *mut sys::lv_display_t, a: *const sys::lv_area_t, px: *mut u8) {
    // SAFETY: LVGL's area and its pixel map for this flush (w × h RGB565)
    unsafe {
        let a = &*a;
        let (w, h) = ((a.x2 - a.x1 + 1) as usize, (a.y2 - a.y1 + 1) as usize);
        let pix = std::slice::from_raw_parts(px as *const u16, w * h);
        let last = sys::lv_display_flush_is_last(d);
        PANEL.with(|p| {
            if let Some(p) = p.borrow_mut().as_mut() {
                let before = p.pushes;
                p.put(Rect { x: a.x1 as usize, y: a.y1 as usize, w, h }, pix, w);
                if last {
                    p.commit();
                }
                PUSHES.fetch_add(p.pushes - before, Ordering::Relaxed);
            }
        });
        if last {
            INVAL.store(false, Ordering::Relaxed);
        }
        FRAMES.fetch_add(1, Ordering::Relaxed);
        sys::lv_display_flush_ready(d);
    }
}

/// LUYAZ_UI_TAPLOG: what each press lands on (taps that "go nowhere" on the device).
unsafe extern "C" fn taplog_cb(e: *mut sys::lv_event_t) {
    // SAFETY: an indev event; LVGL's objects
    unsafe {
        let ind = sys::lv_indev_active();
        let mut p = sys::lv_point_t { x: 0, y: 0 };
        if !ind.is_null() {
            sys::lv_indev_get_point(ind, &mut p);
        }
        let o = if ind.is_null() { std::ptr::null_mut() } else { sys::lv_indev_get_active_obj() };
        let mut ar = sys::lv_area_t { x1: 0, y1: 0, x2: 0, y2: 0 };
        if !o.is_null() {
            sys::lv_obj_get_coords(o, &mut ar);
        }
        let pressed = sys::lv_event_get_code(e) == sys::lv_event_code_t::LV_EVENT_PRESSED;
        eprintln!("tap: {} at {},{} -> obj {o:?} [{},{} {},{}]", if pressed { "press" } else { "click" }, p.x, p.y, ar.x1, ar.y1, ar.x2, ar.y2);
        if pressed {
            ui::with(|u| u.debug_tap());
        }
    }
}

/// The pointer: the touchscreen (Touch hands out one press/release edge per read, so a tap shorter
/// than a read period is still pressed for one read, then released; edges still queued are read
/// again in the same pass) and the screen's gate (a dark screen takes no taps; a press it cuts off
/// is cancelled, not released, or the release would count as a click).
unsafe extern "C" fn read_cb(ind: *mut sys::lv_indev_t, data: *mut sys::lv_indev_data_t) {
    let (x, y, pressed, raw, more) = TOUCH.with(|t| match t.borrow_mut().as_mut() {
        Some(t) => {
            let (x, y, p) = t.read();
            (x, y, p, (t.raw_cur_x, t.raw_cur_y), t.pending())
        }
        None => (0, 0, false, (0, 0), false),
    });
    RD.with(|r| {
        let mut r = r.borrow_mut();
        if r.taplog && pressed != r.was {
            eprintln!("tap: indev {} at {x},{y} (raw {},{})", if pressed { "down" } else { "up" }, raw.0, raw.1);
        }
        r.was = pressed;
    });
    let fwd = ui::with(|u| u.touch_filter(pressed, x, y));
    let cut = RD.with(|r| {
        let mut r = r.borrow_mut();
        let cut = r.fwd_was && !fwd && pressed;
        r.fwd_was = fwd;
        cut
    });
    // SAFETY: LVGL's indev and its read buffer for this call
    unsafe {
        if cut {
            sys::lv_indev_reset(ind, std::ptr::null_mut());
        }
        (*data).point = sys::lv_point_t { x, y };
        (*data).continue_reading = more;
        (*data).state = if fwd { sys::lv_indev_state_t::LV_INDEV_STATE_PRESSED } else { sys::lv_indev_state_t::LV_INDEV_STATE_RELEASED };
    }
}

/// `touch /tmp/luyaz/dumpfb` on the device: the scanout buffer as raw RGB565 to /tmp/fb.dump.
fn maybe_dump_fb() {
    if std::fs::metadata("/tmp/luyaz/dumpfb").is_err() {
        return;
    }
    PANEL.with(|p| {
        if let Some(p) = p.borrow().as_ref() {
            let px = p.scanout();
            let bytes: Vec<u8> = px.iter().flat_map(|v| v.to_ne_bytes()).collect();
            let _ = std::fs::write("/tmp/fb.dump", bytes);
        }
    });
}

/// Wait on the input fds and the core's socket; true when touch or the key woke it.
fn wait_fds(fds: &[i32], data_fd: i32, ms: i32) -> bool {
    let mut pf: Vec<libc::pollfd> = fds.iter().filter(|&&f| f >= 0).map(|&fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 }).collect();
    let input_n = pf.len();
    if data_fd >= 0 {
        pf.push(libc::pollfd { fd: data_fd, events: libc::POLLIN, revents: 0 });
    }
    // SAFETY: a vector of pollfds
    let r = unsafe { libc::poll(pf.as_mut_ptr(), pf.len() as libc::nfds_t, ms) };
    r > 0 && pf[..input_n].iter().any(|p| p.revents != 0)
}

/// Visible label texts under o (a hidden parent hides them).
fn texts(o: Obj, out: &mut Vec<String>) {
    if o.is_null() || o.hidden() {
        return;
    }
    if o.check_type_label() {
        let t = o.text();
        if !t.is_empty() {
            out.push(String::from_utf8_lossy(&t).into_owned());
        }
    }
    for i in 0..o.child_count() as i32 {
        texts(o.child(i), out);
    }
}

fn arg(args: &[String], k: &str) -> Option<String> {
    args.iter().find_map(|a| a.strip_prefix(k).map(String::from))
}

/// The device program. Returns the exit code.
pub fn run(args: &[String]) -> i32 {
    trap_stop_signals();
    let offscreen = args.iter().any(|a| a == "--offscreen");
    let exit_after: Option<u32> = arg(args, "--exit-after=").and_then(|v| v.parse().ok());
    let panel = match offscreen {
        true => Panel::offscreen(),
        false => match Panel::drm() {
            Ok(panel) => panel,
            Err(e) => {
                eprintln!("luyaz-ui: no panel ({e}), giving up");
                return 1;
            }
        },
    };
    PANEL.with(|p| *p.borrow_mut() = Some(panel));
    // without a touchscreen the screen still shows everything; only taps are lost
    let (tw, th) = (W as i32, H as i32);
    let touch = if offscreen { Touch::none(tw, th) } else { Touch::open(tw, th) };
    if !offscreen && touch.fd() < 0 {
        eprintln!("luyaz-ui: no touchscreen found, running display-only");
    }
    let touch_fd = touch.fd();
    TOUCH.with(|t| *t.borrow_mut() = Some(touch));
    let taplog = std::env::var_os("LUYAZ_UI_TAPLOG").is_some();
    RD.with(|r| r.borrow_mut().taplog = taplog);

    // two buffers the size of the panel, so redrawing everything (a page swipe) still costs a
    // single flush and a single DIRTYFB
    #[repr(C, align(64))]
    struct Buf([u16; W * H]);
    static mut BUF1: Buf = Buf([0; W * H]);
    static mut BUF2: Buf = Buf([0; W * H]);
    // SAFETY: LVGL is set up once on this thread; the buffers are static
    let (disp, indev) = unsafe {
        sys::lv_init();
        sys::lv_tick_set_cb(Some(tick_cb));
        let disp = sys::lv_display_create(W as i32, H as i32);
        sys::lv_display_set_flush_cb(disp, Some(flush_cb));
        sys::lv_display_add_event_cb(disp, Some(inval_cb), sys::lv_event_code_t::LV_EVENT_INVALIDATE_AREA, std::ptr::null_mut());
        sys::lv_display_set_buffers(
            disp,
            (&raw mut BUF1).cast(),
            (&raw mut BUF2).cast(),
            (W * H * 2) as u32,
            sys::lv_display_render_mode_t::LV_DISPLAY_RENDER_MODE_PARTIAL,
        );
        let indev = sys::lv_indev_create();
        sys::lv_indev_set_type(indev, sys::lv_indev_type_t::LV_INDEV_TYPE_POINTER);
        sys::lv_indev_set_read_cb(indev, Some(read_cb));
        // damp the fling: LVGL's default glide carries a flick several pages past the aim
        sys::lv_indev_set_scroll_throw(indev, 40);
        if taplog {
            sys::lv_indev_add_event_cb(indev, Some(taplog_cb), sys::lv_event_code_t::LV_EVENT_PRESSED, std::ptr::null_mut());
            sys::lv_indev_add_event_cb(indev, Some(taplog_cb), sys::lv_event_code_t::LV_EVENT_CLICKED, std::ptr::null_mut());
        }
        (disp, indev)
    };

    let argv0 = args.first().cloned().unwrap_or_else(|| "luyaz-ui".into());
    let launch = logic::launch_parse(args);
    let conf = Conf::load(CONF_FILE);
    let key = if offscreen { plat::key::Key::default() } else { plat::key::Key::open() };
    let mut live = Live::new(key);
    let model = std::rc::Rc::new(live.build());
    ui::create(conf, launch, model, Box::new(live), &argv0);

    // SAFETY: LVGL's own timers for this display and input
    let (refr_timer, read_timer) = unsafe { (lv::Timer(sys::lv_display_get_refr_timer(disp)), lv::Timer(sys::lv_indev_get_read_timer(indev))) };
    let quiet_on = std::env::var("LUYAZ_UI_QUIET").map_or(true, |v| v != "0");
    let loopstats = std::env::var_os("LUYAZ_UI_LOOPSTATS").is_some();
    let (mut dark, mut quiet, mut still) = (false, false, 0);
    let mut last_frames = frames();
    let (mut quiets, mut wake_input, mut wake_inval) = (0u64, 0u64, 0u64);
    let start = crate::data::util::mono_ms32();
    let mut stats_at = start;
    let pause = |on: bool| {
        for t in [refr_timer, read_timer] {
            if !t.is_null() {
                if on {
                    t.pause();
                } else {
                    t.resume();
                }
            }
        }
    };

    while RUN.load(Ordering::Relaxed) {
        let now = crate::data::util::mono_ms32();
        if exit_after.is_some_and(|ms| now.wrapping_sub(start) >= ms) {
            break;
        }
        // the data side's transport: the snapshot moves only when the stream is drained
        ui::with(|u| u.src.pump(now));

        // SAFETY: one thread
        let idle = unsafe { sys::lv_timer_handler() };
        if !offscreen {
            maybe_dump_fb();
        }
        if loopstats && crate::data::util::mono_ms32().wrapping_sub(stats_at) >= 10000 {
            stats_at = crate::data::util::mono_ms32();
            eprintln!(
                "loop: flushes {} pushes {} quiet {quiets} (now {}) wakes input {wake_input} inval {wake_inval}",
                frames(),
                PUSHES.load(Ordering::Relaxed),
                quiet as i32
            );
        }

        let (key_fd, data_fd, lit, held) = ui::with(|u| (u.src.key_fd(), u.src.data_fd(), u.bl_is_on(), u.key_held()));
        // a power key held on a dark screen keeps being polled, or its long press is lost
        let want_dark = !lit && !held;
        if want_dark != dark {
            dark = want_dark;
            if quiet {
                quiet = false;
                still = 0;
                ui::with(|u| u.quiet(false));
            }
            pause(dark);
            ui::with(|u| u.idle(dark));
        }
        let wait_ms = idle.min(1000) as i32;
        if dark {
            if wait_fds(&[touch_fd, key_fd], data_fd, wait_ms) {
                // touch or key: let LVGL and the key poll handle it now
                dark = false;
                pause(false);
                ui::with(|u| u.idle(false));
            }
            continue;
        }
        if quiet {
            // something asked for a redraw during the last pass: draw it now
            let mut wake = INVAL.load(Ordering::Relaxed);
            if !wake {
                wake = wait_fds(&[touch_fd, key_fd], data_fd, wait_ms);
                if wake {
                    wake_input += 1;
                }
            } else {
                wake_inval += 1;
            }
            if wake {
                quiet = false;
                still = 0;
                pause(false);
                for t in [refr_timer, read_timer] {
                    if !t.is_null() {
                        t.ready();
                    }
                }
                ui::with(|u| u.quiet(false));
            }
            continue;
        }
        if quiet_on {
            // SAFETY: LVGL state reads on this thread
            let busy = frames() != last_frames
                || unsafe { sys::lv_anim_count_running() } > 0
                || held
                || unsafe { sys::lv_indev_get_state(indev) } == sys::lv_indev_state_t::LV_INDEV_STATE_PRESSED
                || unsafe { !sys::lv_indev_get_scroll_obj(indev).is_null() };
            last_frames = frames();
            still = if busy { 0 } else { still + 1 };
            if still >= 3 {
                // nothing may be left behind the paused refresh: draw it now (the last flush
                // clears INVAL; a later invalidation sets it again)
                lv::refr_now();
                quiet = true;
                quiets += 1;
                pause(true);
                ui::with(|u| u.quiet(true));
                continue;
            }
        }
        // render promptly during animations
        std::thread::sleep(std::time::Duration::from_millis(idle.min(8) as u64));
    }

    ui::with(|u| u.src.data_set_pace(true)); // don't leave core slow behind a stopped UI
    if let Some(f) = arg(args, "--texts=") {
        let mut t = Vec::new();
        ui::with(|u| texts(u.tiles[ui::TAB_HOME], &mut t));
        texts(Obj::layer_top(), &mut t);
        if let Err(e) = std::fs::write(&f, t.join("\n") + "\n") {
            eprintln!("--texts: {f}: {e}");
        }
    }
    if let Some(f) = arg(args, "--png=") {
        PANEL.with(|p| {
            if let Some(p) = p.borrow().as_ref()
                && let Err(e) = p.write_png(std::path::Path::new(&f))
            {
                eprintln!("--png: {f}: {e}");
            }
        });
    }
    // killed by the shutdown we started: dark before the buffer goes, or the panel flashes
    let down = ui::with(|u| u.powering_down());
    if down {
        ui::with(|u| u.src.bl_off());
    }
    PANEL.with(|p| *p.borrow_mut() = None);
    TOUCH.with(|t| *t.borrow_mut() = None);
    0
}
