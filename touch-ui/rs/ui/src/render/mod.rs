//! Offscreen render (--render): replay one run of the C render test
//! on the Rust screen, so the two can be compared shot for shot (manager
//! docs/designs/u60-touch-rewrite.md §4).
//!
//! The C test's --dump-model=DIR writes the run log DIR/index-<name>.txt (its header has the
//! grammar) and the view models DIR/<hash>.json. This harness sets up LVGL as the C test does
//! (DIRECT render mode into one 320×480 RGB565 buffer, a tick that only moves in settle(), 20 ms
//! per lv_timer_handler call, ui.conf with the theme, autooff=0 and the language), feeds the
//! models at the same refreshes, makes the same taps with a real pointer indev, shoots the same
//! pages and runs the same text/glyph/geometry checks. Each shot's pixel hash is compared with
//! the C one in the log; the checks' outcome with C's.
//!
//!   luyaz-ui --render --models=DIR --name=SCENE-THEME[-en] [--pages=home,…] [--png=DIR]
//!                [--expect=FILE] [--keep-going] [--dump]
//!
//! --pages: report only these pages (all steps still run). --png: PNGs of the reported shots,
//! DIR/<name>-<page>.<k>.png (the C test's names). The run stops at the first step the Rust
//! screen does not have yet (a page or a control from a later milestone): what came before
//! counts, nothing after it is reported. --keep-going skips such steps instead and marks every
//! later result "(after a skipped step)".
//!
//! Output, one line each: "SAME <shot>", "DIFF <shot>", "  FAIL [scene/theme(/en)] <check>"
//! (as C prints it), "CHECKS <page> same|differ …", "TIMELINE …" (the run drifted from C's),
//! "STOP <line>: <step>", and a summary.
//!
//! --still: after the run, each tab is left on the last model for a few refreshes and every area
//! invalidated meanwhile is printed ("STILL <tab> <n> x1,y1-x2,y2 …"). A refresh that finds
//! nothing changed must invalidate nothing, else the device's quiet loop wakes every second.
//! LUYAZ_STILL_WHO=1 names the object in each area; LUYAZ_STILL_BT=1 prints who invalidated it (needs
//! a build with symbols: CARGO_PROFILE_RELEASE_STRIP=false CARGO_PROFILE_RELEASE_DEBUG=1).
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

mod checks;

use crate::conf::Conf;
use crate::logic::{self, Launch};
use crate::source::{self, Activity, NiAct, Polled, Source, WifiRead, WifiReply};
use crate::lv::{self, sys, Obj};
use crate::ui::{self, Sub, SUBS, TABS};
use checks::Checks;
use ui_model::Model;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};

const W: usize = 320;
const H: usize = 480;

static TICK: AtomicU32 = AtomicU32::new(0);
/// LVGL wants its draw buffer aligned (LV_DRAW_BUF_ALIGN)
#[repr(C, align(64))]
struct Fb([u16; W * H]);
static mut FB: Fb = Fb([0; W * H]);

unsafe extern "C" fn tick_cb() -> u32 {
    TICK.load(Ordering::Relaxed)
}

unsafe extern "C" fn flush_cb(d: *mut sys::lv_display_t, _a: *const sys::lv_area_t, _px: *mut u8) {
    // SAFETY: LVGL's display; DIRECT mode drew into FB already
    unsafe { sys::lv_display_flush_ready(d) };
}

/// The virtual finger (render_test finger_cb): taps go through LVGL's real hit-testing, behind
/// the same gate the device puts in front of the touchscreen.
static FX: AtomicI32 = AtomicI32::new(0);
static FY: AtomicI32 = AtomicI32::new(0);
static FDOWN: AtomicBool = AtomicBool::new(false);

unsafe extern "C" fn finger_cb(ind: *mut sys::lv_indev_t, d: *mut sys::lv_indev_data_t) {
    let (x, y, down) = (FX.load(Ordering::Relaxed), FY.load(Ordering::Relaxed), FDOWN.load(Ordering::Relaxed));
    let (pressed, cancel) = ui::with(|u| u.touch_filter(down, x, y));
    // SAFETY: LVGL's read buffer for this call
    unsafe {
        if cancel {
            sys::lv_indev_reset(ind, std::ptr::null_mut());
        }
        (*d).point = sys::lv_point_t { x, y };
        (*d).state = if pressed { sys::lv_indev_state_t::LV_INDEV_STATE_PRESSED } else { sys::lv_indev_state_t::LV_INDEV_STATE_RELEASED };
    }
}

fn settle(ms: u32) {
    let mut t = 0;
    while t < ms {
        TICK.fetch_add(20, Ordering::Relaxed);
        // SAFETY: LVGL is initialised; one thread
        unsafe { sys::lv_timer_handler() };
        t += 20;
    }
}

fn fnv64(b: &[u8]) -> u64 {
    let mut h: u64 = 1469598103934665603;
    for &c in b {
        h ^= c as u64;
        h = h.wrapping_mul(1099511628211);
    }
    h
}

/// The view models of one run, loaded once per hash.
struct Models {
    dir: String,
    cache: HashMap<String, Rc<Model>>,
}

impl Models {
    fn get(&mut self, hash: &str) -> Result<Rc<Model>, String> {
        if let Some(m) = self.cache.get(hash) {
            return Ok(m.clone());
        }
        let path = format!("{}/{hash}.json", self.dir);
        let s = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
        let m = Rc::new(Model::from_json(&s).map_err(|e| format!("{path}: {e}"))?);
        self.cache.insert(hash.to_string(), m.clone());
        Ok(m)
    }
}

/// A C subpage name (SUB_SMS …) as the Rust one.
fn sub_by_name(n: &str) -> Option<Sub> {
    let want: String = n.strip_prefix("SUB_")?.split('_').map(|w| w[..1].to_string() + &w[1..].to_lowercase()).collect();
    SUBS.iter().copied().find(|s| format!("{s:?}") == want)
}

/// An object by its path from the C log ("scr/0/1/3"), if this tree has it.
fn resolve(path: &str) -> Option<Obj> {
    let mut it = path.split('/');
    // SAFETY: LVGL is initialised
    let mut o = match it.next()? {
        "scr" => Obj::screen_active(),
        "top" => Obj::layer_top(),
        "sys" => Obj(unsafe { sys::lv_display_get_layer_sys(std::ptr::null_mut()) }),
        // Stable overlay roots for motion replays; children are panel (0), cancel (1).
        "power" => ui::with(|u| u.pw.sheet.scrim),
        "usb" => ui::with(|u| u.um.sheet.scrim),
        "exit" => ui::with(|u| u.ex.xm.scrim),
        _ => return None,
    };
    for k in it {
        let i: u32 = k.parse().ok()?;
        if i >= o.child_count() {
            return None;
        }
        o = o.child(i as i32);
    }
    Some(o)
}

/// The scroll body of a page: the first scrollable descendant that has overflow.
fn find_scroller(o: Obj) -> Option<Obj> {
    if o.is_null() || o.hidden() {
        return None;
    }
    if o.has_flag(lv::LV_OBJ_FLAG_SCROLLABLE) && o.scroll_dir() & sys::lv_dir_t::LV_DIR_VER != 0 {
        o.update_layout();
        if o.scroll_bottom() > 0 || o.scroll_y() > 0 {
            return Some(o);
        }
    }
    (0..o.child_count() as i32).find_map(|i| find_scroller(o.child(i)))
}

/// The model an action in the current step leaves, by hash.
type Pending = Rc<RefCell<Option<(String, Rc<Model>)>>>;

/// The data side as the C render test's fixtures answer: every model
/// comes from the run log (one per refresh where it changed, one after each step that changed
/// it), the polls say what the fixtures' polls say, the actions answer what theirs answer, and
/// what C reads live is read from the model.
struct Replay {
    /// the model of refresh n
    feed: BTreeMap<u32, (String, Rc<Model>)>,
    /// the model the current step leaves (the log's model line after it)
    step: Pending,
    /// the hash of the model last handed out
    cur: Rc<RefCell<String>>,
    next: Option<Rc<Model>>,
    #[cfg(test)]
    wifi_test: Option<Rc<RefCell<WifiTest>>>,
}

#[cfg(test)]
#[derive(Default)]
struct WifiTest {
    read: WifiRead,
    reply: Option<WifiReply>,
    op_id: Option<&'static str>,
    emergency: bool,
    block_readback: bool,
    writes: Vec<(String, String)>,
    fresh_reads: usize,
}

impl Replay {
    /// An action: what it changed is the model line that follows the step.
    fn acted(&mut self) {
        if let Some((h, m)) = self.step.borrow_mut().take() {
            *self.cur.borrow_mut() = h;
            self.next = Some(m);
        }
    }
}

impl Source for Replay {
    fn pump(&mut self, _now_ms: u32) -> bool {
        false
    }
    fn data_fd(&self) -> i32 {
        -1
    }
    fn poll(&mut self, n: u32, a: &Activity) -> Polled {
        if let Some((h, m)) = self.feed.remove(&n) {
            *self.cur.borrow_mut() = h;
            self.next = Some(m);
        }
        // the fixtures' polls: most say "changed" every time, these never, diagnose when active
        Polled {
            scenario: true,
            netinfo: true,
            screen: false,
            chill: true,
            esim: true,
            speedtest: true,
            servers: true,
            ladder: false,
            battery: false,
            versions: false,
            landing: false,
            direct_lat: false,
            today: false,
            usage: false,
            alerts: true,
            diagnose: a.diag,
        }
    }
    fn take_model(&mut self) -> Option<Rc<Model>> {
        self.next.take()
    }
    fn now(&self, m: &Model) -> i64 {
        m.now
    }
    fn boot_s(&self, m: &Model) -> i64 {
        m.boot_s
    }
    fn bl_is_on(&self, m: &Model) -> bool {
        m.bl_is_on != 0
    }
    fn bl_panel_lit(&self, m: &Model) -> bool {
        m.bl_panel_lit != 0
    }
    fn bl_is_lit(&mut self, m: &Model) -> bool {
        m.bl_is_lit != 0
    }
    fn bl_get(&self, m: &Model) -> i32 {
        m.bl_get
    }
    fn bl_max(&self, m: &Model) -> i32 {
        m.bl_max
    }
    fn frames(&self, m: &Model) -> u64 {
        m.frames
    }
    fn touch_reports(&self, m: &Model) -> u64 {
        m.touch_reports
    }
    fn core_version(&self, m: &Model) -> u64 {
        m.core_version
    }
    fn health_count(&self, m: &Model) -> i32 {
        m.health_n
    }
    fn sms_delete_armed(&self, m: &Model, i: usize) -> bool {
        m.sms_armed == i as i32
    }
    // the backlight's effect comes as the model line after the step
    fn bl_on(&mut self) {
        self.acted();
    }
    fn bl_off(&mut self) {
        self.acted();
    }
    fn bl_set(&mut self, _level: i32) {
        self.acted();
    }
    fn bl_remember(&mut self, _level: i32) {}
    fn key_poll(&mut self, _now_ms: u32) -> i32 {
        source::KEY_EV_NONE
    }
    fn key_fd(&self) -> i32 {
        -1
    }
    fn key_held(&self) -> bool {
        false
    }
    fn data_set_pace(&mut self, _panel_lit: bool) {}
    fn data_release_pace(&mut self) {}
    fn data_control_fb(&mut self, _a: &str, _p: &str, _fa: &str, _fb: Option<&str>) -> bool {
        let sent = true;
        #[cfg(test)]
        let sent = if let Some(wifi) = &self.wifi_test {
            let mut wifi = wifi.borrow_mut();
            wifi.reply = None;
            wifi.writes.push((_a.into(), _p.into()));
            wifi.op_id = if wifi.emergency { None } else {
                ["screen-1-1", "screen-1-2", "screen-1-3", "screen-1-4", "screen-1-5"]
                    .get(wifi.writes.len() - 1).copied()
            };
            !wifi.emergency
        } else {
            sent
        };
        self.acted();
        sent
    }
    fn data_control_undo(&mut self, _a: &str, _p: &str) -> bool {
        self.acted();
        true
    }
    fn journal_list(&mut self, m: &Model) -> Option<String> {
        m.journal.clone()
    }
    fn sms_mark_read_id(&mut self, _id: i64) {
        self.acted();
    }
    fn sms_mark_all_read(&mut self) {
        self.acted();
    }
    fn sms_delete_id(&mut self, _id: i64) {
        self.acted();
    }
    fn sms_delete_arm(&mut self, _i: usize) {
        self.acted();
    }
    fn sms_delete_tap(&mut self, _i: usize) -> bool {
        self.acted();
        false
    }
    fn alerts_mark_all_read(&mut self) {
        self.acted();
    }
    fn versions_check(&mut self) {
        self.acted();
    }
    /// profile 0 is the one in use; another arms
    fn esim_select(&mut self, i: usize) -> i32 {
        self.acted();
        if i == 0 { source::ESIM_SEL_CURRENT } else { source::ESIM_SEL_ARMED }
    }
    fn speedtest_start(&mut self, _chill: bool, _roaming_ok: bool) -> bool {
        self.acted();
        true
    }
    fn ladder_start(&mut self, _busy_ok: bool, _roaming_ok: bool) {
        self.acted();
    }
    fn ladder_stop(&mut self) {
        self.acted();
    }
    fn ladder_dismiss(&mut self) {
        self.acted();
    }
    fn ladder_browse(&mut self, _open: bool) {
        self.acted();
    }
    fn ladder_open(&mut self, _i: usize) {
        self.acted();
    }
    fn speedtest_stop(&mut self) -> bool {
        self.acted();
        true
    }
    fn speedtest_select_server(&mut self, _i: i32) {
        self.acted();
    }
    fn scenario_kick(&mut self) {
        self.acted();
    }
    fn netinfo_act(&mut self, _a: NiAct) {
        self.acted();
    }
    fn netinfo_pin(&mut self, _id: &str) {
        self.acted();
    }
    fn netinfo_apn_use(&mut self, _id: &str) {
        self.acted();
    }
    fn netinfo_hurry(&mut self, _secs: i32) {}
    fn diagnose_kick(&mut self) {}
    fn diagnose_poll(&mut self, active: bool) -> bool {
        self.acted();
        active
    }
    fn diagnose_start(&mut self) -> bool {
        self.acted();
        true
    }
    fn diagnose_speed(&mut self) -> bool {
        self.acted();
        true
    }
    fn diagnose_feedback(&mut self, _right: bool) -> bool {
        self.acted();
        true
    }
    fn scenario_chill_set(&mut self, _on: bool) -> bool {
        self.acted();
        true
    }
    fn chill_set_exit(&mut self, _state: &str) -> bool {
        self.acted();
        true
    }
    fn chill_select_node(&mut self, _i: usize) -> bool {
        self.acted();
        true
    }
    fn chill_select_group(&mut self, _i: usize) -> bool {
        self.acted();
        true
    }
    fn chill_set_profile(&mut self, _p: &str) -> bool {
        self.acted();
        true
    }
    fn chill_test_delay(&mut self) -> i32 {
        println!("ACTION chill_test_delay");
        self.acted();
        1
    }
    /// the harness stays in this process
    fn exec(&mut self, _l: &Launch) -> i32 {
        self.acted();
        -1
    }
    fn request_vendor(&mut self) -> bool {
        true
    }
    fn vendor_fallback(&mut self) {}
    fn power_fallback(&mut self, _reboot: bool) {}
    fn power_last_resort(&mut self) {}
    fn power_mode(&mut self) -> String {
        String::new()
    }
    fn lang_hold(&mut self) {}
    fn alert_theme_paused(&mut self) {}
    /// `LUYAZ_RENDER_AUX="w24,w5,psm"` gives the screenshot fixture's settings.
    fn wifi_read(&mut self, _fresh: bool) -> Option<WifiRead> {
        #[cfg(test)]
        if let Some(wifi) = &self.wifi_test {
            let mut wifi = wifi.borrow_mut();
            if _fresh {
                wifi.fresh_reads += 1;
                return None; // a pre-write sample cannot complete the operation
            }
            return (!wifi.block_readback).then_some(wifi.read);
        }
        let v = std::env::var("LUYAZ_RENDER_AUX").unwrap_or_else(|_| "0,0,0".into());
        let mut flags = v.split(',').map(|x| x.trim() == "1");
        let up = [flags.next()?, flags.next()?];
        let on = up[0] || up[1];
        Some(WifiRead { master: Some(on), radios: up.map(Some), power_save: flags.next(), module: Some(on), main: [Some(true); 2], up })
    }

    fn wifi_reply(&self) -> Option<WifiReply> {
        #[cfg(test)]
        if let Some(wifi) = &self.wifi_test {
            return wifi.borrow().reply;
        }
        Some(WifiReply::CoreDone)
    }

    fn wifi_op_id(&self) -> Option<&str> {
        #[cfg(test)]
        if let Some(wifi) = &self.wifi_test {
            return wifi.borrow().op_id;
        }
        None
    }
}


struct Shot {
    name: String,
    refreshes: u32,
    tick: u32,
    model: String,
    px: u64,
}

struct Run {
    disp: *mut sys::lv_display_t,
    name: String,
    /// "[scene/theme(/en)]" as C prints it
    tag: String,
    png: Option<String>,
    pages: Option<Vec<String>>,
    checks: Checks,
    shots: Vec<Shot>,
    /// this page's shots not yet matched with the C log's shot lines (by index into shots)
    pending: Vec<usize>,
    /// a step was skipped (--keep-going): what follows may not be what C drew
    skipped: bool,
    /// the model the screen has now (hash)
    cur: Rc<RefCell<String>>,
    n_same: usize,
    n_diff: usize,
    n_checked: usize,
    n_checks_differ: usize,
    timeline: usize,
}

impl Run {
    fn wanted(&self, page: &str) -> bool {
        self.pages.as_ref().is_none_or(|p| p.iter().any(|x| x == page))
    }

    fn shot(&mut self, page: &str, k: usize) {
        self.capture(page, k, true);
    }

    /// Motion frames intentionally clip moving content. Check geometry on settled shots.
    fn capture(&mut self, page: &str, k: usize, check_geometry: bool) {
        Obj::screen_active().invalidate();
        Obj::layer_top().invalidate();
        // SAFETY: the display this harness created
        unsafe { sys::lv_refr_now(self.disp) };
        if check_geometry {
            self.checks.geo_walk(Obj::screen_active());
            self.checks.geo_walk(Obj::layer_top());
        }
        // SAFETY: rendering is done; FB is not touched until the next lv call
        let fb = unsafe { std::slice::from_raw_parts((&raw const FB).cast::<u16>(), W * H) };
        // SAFETY: the same memory as bytes
        let px = fnv64(unsafe { std::slice::from_raw_parts(fb.as_ptr().cast::<u8>(), W * H * 2) });
        let name = format!("{page}.{k}");
        if self.wanted(page)
            && let Some(dir) = &self.png
        {
            let path = format!("{dir}/{}-{name}.png", self.name);
            if let Err(e) = std::fs::write(&path, plat::png_rgb565(W as u32, H as u32, fb)) {
                eprintln!("render: {path}: {e}");
            }
        }
        let refreshes = ui::with(|u| u.refreshes);
        self.shots.push(Shot { name, refreshes, tick: TICK.load(Ordering::Relaxed), model: self.cur.borrow().clone(), px });
        self.pending.push(self.shots.len() - 1);
    }

    /// Before the next page: shots Rust took that C did not.
    fn unmatched(&mut self) {
        for i in std::mem::take(&mut self.pending) {
            let s = &self.shots[i];
            let page = s.name.rsplit_once('.').map_or(s.name.as_str(), |p| p.0);
            if self.wanted(page) {
                println!("TIMELINE {} {}: Rust shot it, C did not", self.tag, s.name);
                self.timeline += 1;
            }
        }
    }

    /// Shoot a page top to bottom; collect its texts (render_test shoot_page).
    fn shoot_page(&mut self, page: &str, root: Obj) {
        self.unmatched();
        self.checks.texts_clear();
        self.checks.collect(root);
        self.checks.collect(Obj::layer_top());
        let Some(sc) = find_scroller(root) else {
            self.shot(page, 0);
            return;
        };
        sc.scroll_to_y(0);
        let step = (sc.height() - 60).max(60);
        let mut y = 0;
        for k in 0..12 {
            sc.scroll_to_y(y);
            sc.update_layout();
            self.shot(page, k);
            if sc.scroll_bottom() <= 0 {
                break;
            }
            y += step;
        }
        sc.scroll_to_y(0);
    }

    /// A C shot line: the next Rust shot must be the same one, drawn at the same moment from
    /// the same model, with the same pixels.
    fn c_shot(&mut self, f: &[&str]) {
        let [name, model, refreshes, tick, px] = f[..] else { return };
        let page = name.rsplit_once('.').map_or(name, |p| p.0);
        let Some(k) = self.pending.iter().position(|&i| self.shots[i].name == name) else {
            if self.wanted(page) {
                println!("TIMELINE {} {name}: C shot it, Rust did not", self.tag);
                self.timeline += 1;
            }
            return;
        };
        let s = &self.shots[self.pending.remove(k)];
        if s.refreshes.to_string() != refreshes || s.tick.to_string() != tick || s.model != model {
            println!(
                "TIMELINE {} C {name} refresh {refreshes} tick {tick} model {model}, Rust {} refresh {} tick {} model {}",
                self.tag, s.name, s.refreshes, s.tick, s.model
            );
            self.timeline += 1;
        }
        if !self.wanted(page) {
            return;
        }
        let note = if self.skipped { " (after a skipped step)" } else { "" };
        if format!("{:016x}", s.px) == px {
            println!("SAME {}-{name}{note}", self.name);
            self.n_same += 1;
        } else {
            println!("DIFF {}-{name}{note}", self.name);
            self.n_diff += 1;
        }
    }

    /// page_done, then C's outcome of the same checks (the cfail lines and the checked line).
    fn page_done(&mut self, page: &str, c_fails: &[&[u8]], c_checked: &[&str]) {
        self.checks.page_done(page);
        if !self.wanted(page) {
            return;
        }
        self.n_checked += 1;
        let mut o = std::io::stdout().lock();
        for f in &self.checks.fails {
            let _ = o.write_all(format!("  FAIL {} ", self.tag).as_bytes());
            let _ = o.write_all(f);
            let _ = o.write_all(b"\n");
        }
        let rust = format!("{} {} {}", self.checks.pass, self.checks.fails.len(), self.checks.ntrunc);
        let c = c_checked.get(1..).map(|v| v.join(" ")).unwrap_or_default();
        let same_fails = c_fails.len() == self.checks.fails.len() && c_fails.iter().zip(&self.checks.fails).all(|(a, b)| a == &b.as_slice());
        if c == rust && same_fails {
            let _ = writeln!(o, "CHECKS {} {page} same (pass fail trunc {rust})", self.tag);
        } else {
            self.n_checks_differ += 1;
            let _ = writeln!(o, "CHECKS {} {page} differ: C pass fail trunc {c}, Rust {rust}", self.tag);
            for f in c_fails {
                let _ = o.write_all(b"  C:    ");
                let _ = o.write_all(f);
                let _ = o.write_all(b"\n");
            }
        }
    }
}

fn arg(args: &[String], k: &str) -> Option<String> {
    args.iter().find_map(|a| a.strip_prefix(k).map(String::from))
}

#[cfg(test)]
fn isolated_render(test: &str, variants: &[&'static str]) -> Option<&'static str> {
    // Theme and FreeType font pointers have process lifetime, like the device UI.
    const VARIANT: &str = "LUYAZ_TEST_RENDER_VARIANT";
    if let Ok(active) = std::env::var(VARIANT) {
        return Some(variants.iter().copied().find(|&v| v == active).unwrap());
    }
    let exe = std::env::current_exe().unwrap();
    for &variant in variants {
        let out = std::process::Command::new(&exe)
            .args(["--exact", test, "--nocapture"])
            .env(VARIANT, variant)
            .output()
            .unwrap();
        assert!(
            out.status.success() && out.stdout.starts_with(b"\nrunning 1 test"),
            "{test} ({variant}): {}\n{}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }
    None
}

#[cfg(test)]
fn init_render_test() {
    // SAFETY: this test is the sole LVGL user in the process; FB outlives the display.
    unsafe {
        sys::lv_init();
        sys::lv_tick_set_cb(Some(tick_cb));
        let disp = sys::lv_display_create(W as i32, H as i32);
        sys::lv_display_set_flush_cb(disp, Some(flush_cb));
        sys::lv_display_set_buffers(disp, (&raw mut FB).cast(), std::ptr::null_mut(),
            (W * H * 2) as u32, sys::lv_display_render_mode_t::LV_DISPLAY_RENDER_MODE_DIRECT);
        let finger = sys::lv_indev_create();
        sys::lv_indev_set_type(finger, sys::lv_indev_type_t::LV_INDEV_TYPE_POINTER);
        sys::lv_indev_set_read_cb(finger, Some(finger_cb));
    }
}

#[cfg(test)]
#[test]
fn wifi_switch_waits_for_readback_and_blocks_touch() {
    if isolated_render("render::wifi_switch_waits_for_readback_and_blocks_touch", &["dark"]).is_none() {
        return;
    }
    init_render_test();
    let initial = WifiRead {
        master: Some(false), module: Some(false), radios: [Some(false), Some(true)],
        main: [Some(true); 2], power_save: Some(true), up: [false; 2],
    };
    let wifi = Rc::new(RefCell::new(WifiTest { read: initial, ..WifiTest::default() }));
    let src = Box::new(Replay {
        feed: BTreeMap::new(), step: Rc::new(RefCell::new(None)), cur: Rc::new(RefCell::new(String::new())),
        next: None, wifi_test: Some(wifi.clone()),
    });
    let model = Model { bl_is_on: 1, bl_panel_lit: 1, bl_is_lit: 1, ..Model::default() };
    ui::create(Conf::parse("appearance=dark\nautooff=0\n"), logic::launch_parse(&["luyaz-ui"]),
        Rc::new(model), src, "luyaz-ui");
    ui::with(|u| u.to_tab(ui::TAB_WIFI));
    settle(1200);

    let finger = |x, y, down| {
        FX.store(x, Ordering::Relaxed);
        FY.store(y, Ordering::Relaxed);
        FDOWN.store(down, Ordering::Relaxed);
        settle(100);
    };
    let tap_switch = |i: usize| {
        let a = ui::with(|u| u.wifi.sw[i].coords());
        let (x, y) = ((a.x1 + a.x2) / 2, (a.y1 + a.y2) / 2);
        finger(x, y, true);
        finger(x, y, false);
    };
    let checked = || ui::with(|u| std::array::from_fn(|i| u.wifi.sw[i].has_state(lv::LV_STATE_CHECKED)));
    fn has_text(o: Obj, text: &[u8]) -> bool {
        !o.hidden() && ((checks::is_label(o) && o.text() == text)
            || (0..o.child_count() as i32).any(|i| has_text(o.child(i), text)))
    }
    let overlay_has = |text: &str| has_text(Obj::layer_top(), text.as_bytes());
    let pending = || overlay_has("正在处理，等设备确认 Wi-Fi");
    let warned = || overlay_has("Wi-Fi 更改未确认");
    let show_op = |slot: &str, id: &str, phase: &str, registered: bool| {
        let mut model = ui::with(|u| (*u.model).clone());
        model.screen_op = Some(format!(r#"{{"{slot}":{{"op_id":"{id}","phase":"{phase}","steps":[{{"key":"applied","done":true}},{{"key":"registered","done":{registered}}}]}}}}"#));
        ui::set_model(Rc::new(model));
        ui::with(|u| u.refresh());
    };
    assert_eq!(checked(), [false, false, true, true]);
    tap_switch(2); // band controls are disabled with the module off
    assert!(wifi.borrow().writes.is_empty());
    tap_switch(0);
    assert_eq!(wifi.borrow().writes, [("wifi.settings".into(), r#"{"wifi_onoff":"1"}"#.into())]);
    assert!(pending());

    tap_switch(0);
    tap_switch(2);
    show_op("active", "screen-1-1", "accepted", false);
    finger(35, 450, true); // a tab tap started under the overlay
    wifi.borrow_mut().read = WifiRead { master: Some(true), module: Some(true), ..initial };
    show_op("active", "screen-1-1", "verifying", true);
    show_op("last", "screen-9-1", "confirmed", true);
    settle(1100);
    assert_eq!(wifi.borrow().writes.len(), 1);
    assert_eq!(checked(), [false, false, true, true]);
    assert_eq!(wifi.borrow().fresh_reads, 0);
    assert!(pending());

    // The HTTP reply was lost. The matching Core verdict starts one fresh read.
    wifi.borrow_mut().block_readback = true;
    show_op("last", "screen-1-1", "confirmed", true);
    assert_eq!(wifi.borrow().fresh_reads, 1);
    assert!(pending());
    show_op("last", "screen-9-2", "confirmed", true); // the verdict must survive this overwrite
    wifi.borrow_mut().block_readback = false;
    settle(1200);
    assert!(!pending());
    finger(35, 450, false);
    assert_eq!(ui::with(|u| u.cur_tab()), ui::TAB_WIFI);
    assert_eq!(checked(), [true, false, true, true]);
    assert!(!warned());

    tap_switch(1);
    assert_eq!(wifi.borrow().writes[1], ("wifi.settings".into(), r#"{"radio2_disabled":"0"}"#.into()));
    show_op("last", "screen-1-2", "confirmed", false); // terminal without Core's layout confirmation
    settle(1200);
    assert!(!pending());
    assert!(warned());
    assert_eq!(checked(), [true, false, true, true]);

    // Known saved preference but no running module: preserve display and reject a band tap.
    wifi.borrow_mut().read.module = Some(false);
    settle(6100);
    tap_switch(2);
    assert_eq!(wifi.borrow().writes.len(), 2);
    wifi.borrow_mut().read.module = Some(true);
    wifi.borrow_mut().read.radios[1] = None;
    settle(6100);
    assert_eq!(checked(), [true, false, true, true]);
    assert!(ui::with(|u| u.wifi.sw[2].has_state(lv::LV_STATE_DISABLED)));
    tap_switch(2);
    assert_eq!(wifi.borrow().writes.len(), 2);
    wifi.borrow_mut().read.radios[1] = Some(true);
    settle(6100);
    tap_switch(2);
    assert_eq!(wifi.borrow().writes[2], ("wifi.settings".into(), r#"{"radio5_disabled":"1"}"#.into()));
    wifi.borrow_mut().reply = Some(WifiReply::Failed);
    settle(2200);
    assert!(warned());
    assert!(!pending());
    assert_eq!(checked(), [true, false, true, true]);

    // A missing HTTP response and missing outcome must never send the request twice.
    tap_switch(0);
    assert_eq!(wifi.borrow().writes[3].1, r#"{"wifi_onoff":"0"}"#);
    TICK.fetch_add(75_000, Ordering::Relaxed);
    ui::with(|u| u.refresh());
    assert!(pending(), "a sent transaction gets longer than the emergency deadline");
    TICK.fetch_add(70_000, Ordering::Relaxed);
    ui::with(|u| u.refresh());
    assert!(!pending());
    assert!(warned());
    assert_eq!(wifi.borrow().writes.len(), 4);
    settle(1200);
    assert_eq!(checked(), [true, false, true, true]);

    wifi.borrow_mut().emergency = true;
    tap_switch(0);
    wifi.borrow_mut().reply = Some(WifiReply::EmergencyDone);
    wifi.borrow_mut().read = WifiRead { up: [false, true], ..initial };
    settle(2200);
    assert!(pending(), "the emergency script still needs a running-layout readback");
    wifi.borrow_mut().read = initial;
    settle(1200);
    assert!(!pending());
    assert_eq!(checked(), [false, false, true, true]);
}

/// The NFC row says only what a live core actually read: without a model, a silent core or
/// a block that never carried a real value, it shows —, stays disabled and cannot send
/// nfc.set; the handle keeps its position across the transient unknown.
#[cfg(test)]
#[test]
fn wifi_nfc_row_shows_only_a_live_known_value() {
    if isolated_render("render::wifi_nfc_row_shows_only_a_live_known_value", &["dark"]).is_none() {
        return;
    }
    init_render_test();
    let initial = WifiRead {
        master: Some(true), module: Some(true), radios: [Some(true), Some(true)],
        main: [Some(true); 2], power_save: Some(true), up: [true, true],
    };
    let wifi = Rc::new(RefCell::new(WifiTest { read: initial, ..WifiTest::default() }));
    let src = Box::new(Replay {
        feed: BTreeMap::new(), step: Rc::new(RefCell::new(None)), cur: Rc::new(RefCell::new(String::new())),
        next: None, wifi_test: Some(wifi.clone()),
    });
    let mut model = Model { bl_is_on: 1, bl_panel_lit: 1, bl_is_lit: 1, ..Model::default() };
    ui::create(Conf::parse("appearance=dark\nautooff=0\n"), logic::launch_parse(&["luyaz-ui"]),
        Rc::new(model.clone()), src, "luyaz-ui");
    ui::with(|u| u.to_tab(ui::TAB_WIFI));
    settle(1200);

    let finger = |x, y, down| {
        FX.store(x, Ordering::Relaxed);
        FY.store(y, Ordering::Relaxed);
        FDOWN.store(down, Ordering::Relaxed);
        settle(100);
    };
    let tap_nfc = || {
        let a = ui::with(|u| u.wifi.sw[4].coords());
        let (x, y) = ((a.x1 + a.x2) / 2, (a.y1 + a.y2) / 2);
        finger(x, y, true);
        finger(x, y, false);
    };
    let nfc = || {
        ui::with(|u| {
            (
                u.wifi.sw[4].has_state(lv::LV_STATE_CHECKED),
                u.wifi.sw[4].has_state(lv::LV_STATE_DISABLED),
                u.wifi.sw_st[4].text(),
            )
        })
    };

    // no data at all: unknown, disabled, nothing to send
    assert_eq!(nfc(), (false, true, "—".into()));
    tap_nfc();
    assert!(wifi.borrow().writes.is_empty());

    // a live model with the switch on: the row is a real control again
    model.data_ok = 1;
    model.data.nfc_on = 1;
    ui::set_model(Rc::new(model.clone()));
    ui::with(|u| u.refresh());
    settle(1100);
    assert_eq!(nfc(), (true, false, "已开启".into()));
    tap_nfc();
    assert_eq!(wifi.borrow().writes, [("nfc.set".into(), r#"{"enabled":0,"flag":2}"#.into())]);
    assert_eq!(nfc().0, false, "the flip shows until the core says otherwise");
    model.data.nfc_on = 0;
    ui::set_model(Rc::new(model.clone()));
    ui::with(|u| u.refresh());
    settle(1100);
    assert_eq!(nfc(), (false, false, "已关闭".into()));

    // the core goes silent: unknown again, the handle keeps its word, no second write
    model.backend_silent = 1;
    ui::set_model(Rc::new(model.clone()));
    ui::with(|u| u.refresh());
    settle(1100);
    assert_eq!(nfc(), (false, true, "—".into()));
    tap_nfc();
    assert_eq!(wifi.borrow().writes.len(), 1);

    // and back: the value the model carries, not the one the handle held
    model.backend_silent = 0;
    model.data.nfc_on = 1;
    ui::set_model(Rc::new(model.clone()));
    ui::with(|u| u.refresh());
    settle(1100);
    assert_eq!(nfc(), (true, false, "已开启".into()));
}

/// 节能模式 through the core: CoreDone alone is not the saved selection — the fresh read
/// must carry it; an explicit failure warns and leaves the row on the last known value.
#[cfg(test)]
#[test]
fn wifi_power_save_confirms_on_the_fresh_saved_selection() {
    if isolated_render("render::wifi_power_save_confirms_on_the_fresh_saved_selection", &["dark"]).is_none() {
        return;
    }
    init_render_test();
    let initial = WifiRead {
        master: Some(true), module: Some(true), radios: [Some(true), Some(true)],
        main: [Some(true); 2], power_save: Some(true), up: [true, true],
    };
    let wifi = Rc::new(RefCell::new(WifiTest { read: initial, ..WifiTest::default() }));
    let src = Box::new(Replay {
        feed: BTreeMap::new(), step: Rc::new(RefCell::new(None)), cur: Rc::new(RefCell::new(String::new())),
        next: None, wifi_test: Some(wifi.clone()),
    });
    let model = Model { bl_is_on: 1, bl_panel_lit: 1, bl_is_lit: 1, ..Model::default() };
    ui::create(Conf::parse("appearance=dark\nautooff=0\n"), logic::launch_parse(&["luyaz-ui"]),
        Rc::new(model), src, "luyaz-ui");
    ui::with(|u| u.to_tab(ui::TAB_WIFI));
    settle(1200);

    let finger = |x, y, down| {
        FX.store(x, Ordering::Relaxed);
        FY.store(y, Ordering::Relaxed);
        FDOWN.store(down, Ordering::Relaxed);
        settle(100);
    };
    let tap = |i: usize| {
        let a = ui::with(|u| u.wifi.sw[i].coords());
        let (x, y) = ((a.x1 + a.x2) / 2, (a.y1 + a.y2) / 2);
        finger(x, y, true);
        finger(x, y, false);
    };
    let psm_checked = || ui::with(|u| u.wifi.sw[3].has_state(lv::LV_STATE_CHECKED));
    fn has_text(o: Obj, text: &[u8]) -> bool {
        !o.hidden() && ((checks::is_label(o) && o.text() == text)
            || (0..o.child_count() as i32).any(|i| has_text(o.child(i), text)))
    }
    let overlay_has = |text: &str| has_text(Obj::layer_top(), text.as_bytes());
    let pending = || overlay_has("正在处理，等设备确认 Wi-Fi");
    let warned = || overlay_has("Wi-Fi 更改未确认");

    assert!(psm_checked());
    tap(3);
    assert_eq!(wifi.borrow().writes, [("wifi.power_save".into(), r#"{"enabled":0}"#.into())]);
    assert!(pending());

    // core said done but the read still carries the old selection: not confirmed
    wifi.borrow_mut().reply = Some(WifiReply::CoreDone);
    settle(1100);
    assert!(pending(), "CoreDone alone does not confirm the power-save write");
    assert!(psm_checked(), "the handle stays on the confirmed value");

    // the fresh read carries the saved selection: done
    wifi.borrow_mut().read.power_save = Some(false);
    settle(1200);
    assert!(!pending());
    assert!(!warned());
    assert!(!psm_checked());

    // an explicit failure warns without claiming the change
    tap(3);
    assert_eq!(wifi.borrow().writes[1], ("wifi.power_save".into(), r#"{"enabled":1}"#.into()));
    wifi.borrow_mut().reply = Some(WifiReply::Failed);
    settle(2200);
    assert!(!pending());
    assert!(warned());
    assert!(!psm_checked(), "a refused write does not flip the row");
}

#[cfg(test)]
#[test]
fn swipe_unlock_blocks_underlying_controls() {
    if isolated_render("render::swipe_unlock_blocks_underlying_controls", &["dark"]).is_none() {
        return;
    }
    init_render_test();
    let initial = WifiRead {
        master: Some(false), module: Some(false), radios: [Some(false), Some(true)],
        main: [Some(true); 2], power_save: Some(true), up: [false; 2],
    };
    let wifi = Rc::new(RefCell::new(WifiTest { read: initial, ..WifiTest::default() }));
    let src = Box::new(Replay {
        feed: BTreeMap::new(), step: Rc::new(RefCell::new(None)), cur: Rc::new(RefCell::new(String::new())),
        next: None, wifi_test: Some(wifi.clone()),
    });
    let model = Model { bl_is_on: 1, bl_panel_lit: 1, bl_is_lit: 1, ..Model::default() };
    ui::create(Conf::parse("appearance=dark\nautooff=0\n"), logic::launch_parse(&["luyaz-ui"]),
        Rc::new(model), src, "luyaz-ui");
    ui::with(|u| u.to_tab(ui::TAB_WIFI));
    settle(1200);
    let a = ui::with(|u| u.wifi.sw[0].coords());
    let (x, y) = ((a.x1 + a.x2) / 2, (a.y1 + a.y2) / 2);
    assert!(y <= 345, "the actual Wi-Fi switch must be within reach of a bottom swipe: {y}");
    let finger = |x, y, down, ms| {
        FX.store(x, Ordering::Relaxed);
        FY.store(y, Ordering::Relaxed);
        FDOWN.store(down, Ordering::Relaxed);
        settle(ms);
    };
    ui::with(|u| { u.init_screen_lock(); u.prepare_screen_wake(); });
    finger(x, 445, true, 40);
    finger(x, y, false, 40);
    assert!(ui::with(|u| u.screen_locked()), "wake-suppressed contact unlocked the screen");
    assert!(wifi.borrow().writes.is_empty());
    settle(400);
    finger(x, y, true, 100);
    finger(x, y, false, 100);
    assert!(ui::with(|u| u.screen_locked()));
    assert!(wifi.borrow().writes.is_empty());
    let guide = |expected: &str| {
        let mut labels = Vec::new();
        let layer = Obj(unsafe { sys::lv_layer_sys() });
        lock_test_labels(layer, &mut labels);
        assert!(labels.iter().any(|(s, _)| s == expected), "{labels:?}");
    };
    let start_y = y + 92;
    finger(x - 64, start_y, true, 100);
    guide("继续上滑");
    finger(x - 40, start_y - 36, true, 100);
    assert!(ui::with(|u| u.lock_needs_touch_samples()));
    lock_test_capture("dark-swipe-partial");
    guide("继续上滑");
    finger(x, y, true, 100);
    assert!(ui::with(|u| u.screen_locked()));
    assert!(wifi.borrow().writes.is_empty());
    guide("松手解锁");
    lock_test_capture("dark-swipe-ready");
    finger(x, y, false, 100); // release over the real write-producing switch
    assert!(!ui::with(|u| u.screen_locked()));
    assert!(!ui::with(|u| u.lock_needs_touch_samples()));
    assert_eq!(ui::with(|u| u.cur_tab()), ui::TAB_WIFI);
    assert!(wifi.borrow().writes.is_empty());

    // A previously forwarded press must be cancelled even if the blocking sample is its release.
    finger(x, y, true, 100);
    ui::with(|u| u.engage_screen_lock());
    finger(x, y, false, 100);
    assert!(wifi.borrow().writes.is_empty());
    assert!(ui::with(|u| u.screen_locked()));
    finger(x, 295, true, 100);
    finger(x, 265, true, 100);
    guide("继续上滑");
    finger(x, 265, false, 100);
    guide("从下半屏向上滑动解锁");
    assert!(!ui::with(|u| u.lock_needs_touch_samples()));
    finger(x, 445, true, 100);
    finger(x, y, true, 100);
    finger(x, y, false, 100);
    assert!(!ui::with(|u| u.screen_locked()));
    finger(x, y, true, 100);
    finger(x, y, false, 100);
    assert_eq!(wifi.borrow().writes, [("wifi.settings".into(), r#"{"wifi_onoff":"1"}"#.into())]);
}

#[cfg(test)]
fn lock_test_labels(o: Obj, out: &mut Vec<(String, sys::lv_area_t)>) {
    if o.is_null() || o.hidden() {
        return;
    }
    if checks::is_label(o) {
        out.push((String::from_utf8_lossy(&o.text()).into_owned(), o.coords()));
    }
    for i in 0..o.child_count() as i32 {
        lock_test_labels(o.child(i), out);
    }
}

#[cfg(test)]
fn lock_test_capture(name: &str) {
    // SAFETY: the test owns the default display and framebuffer.
    unsafe { sys::lv_refr_now(std::ptr::null_mut()) };
    if let Some(dir) = std::env::var_os("LUYAZ_LOCK_TEST_PNG_DIR") {
        let path = std::path::PathBuf::from(dir).join(format!("lock-{name}.png"));
        let fb = unsafe { std::slice::from_raw_parts((&raw const FB).cast::<u16>(), W * H) };
        std::fs::write(&path, plat::png_rgb565(W as u32, H as u32, fb))
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    }
}

#[cfg(test)]
#[test]
fn swipe_unlock_render_themes_and_data() {
    if let Some(theme) = isolated_render("render::swipe_unlock_render_themes_and_data", &["light", "dark"]) {
        render_lock_theme(theme);
    }
}

#[cfg(test)]
fn render_lock_theme(theme: &str) {
    init_render_test();
    let now = 1_790_250_112;
    let tm = crate::clock::local(now);
    let time = format!("{:02}:{:02}", tm.hour, tm.min);
    let date = format!("{:02}-{:02}", tm.mon, tm.mday);
    {
        let src = Box::new(Replay {
            feed: BTreeMap::new(), step: Rc::new(RefCell::new(None)), cur: Rc::new(RefCell::new(String::new())),
            next: None, wifi_test: None,
        });
        let mut model = Model { now, bl_is_on: 1, bl_panel_lit: 1, bl_is_lit: 1, ..Model::default() };
        ui::create(Conf::parse(&format!("appearance={theme}\nautooff=0\nspeed_bits=0\n")), logic::launch_parse(&["luyaz-ui"]),
            Rc::new(model.clone()), src, "luyaz-ui");
        ui::with(|u| u.init_screen_lock());
        settle(1200);
        let labels = || {
            // SAFETY: this test owns the default display.
            let root = Obj(unsafe { sys::lv_display_get_layer_sys(std::ptr::null_mut()) });
            let mut labels = Vec::new();
            lock_test_labels(root, &mut labels);
            labels
        };
        let text = || labels().into_iter().map(|(s, _)| s).collect::<Vec<_>>().join(" ");
        let unavailable = text();
        assert!(unavailable.contains(&time), "{theme}: {unavailable}");
        assert!(unavailable.contains(&date), "{theme}: {unavailable}");
        assert!(unavailable.contains("状态暂不可用"), "{theme}: {unavailable}");
        assert!(unavailable.matches('—').count() >= 2, "{theme}: {unavailable}");
        for (s, a) in labels().into_iter().filter(|(s, _)| !s.is_empty()) {
            assert!(a.x1 >= 0 && a.y1 >= 0 && a.x2 < W as i32 && a.y2 < H as i32,
                "{theme}: label {s:?} out of bounds: {},{}-{},{}", a.x1, a.y1, a.x2, a.y2);
        }
        let check_geometry = || {
            let root = Obj(unsafe { sys::lv_display_get_layer_sys(std::ptr::null_mut()) });
            let mut checks = Checks::default();
            checks.collect(root);
            checks.geo_walk(root);
            checks.page_done("lock");
            assert!(checks.fails.is_empty(), "{theme}: {:?}", checks.fails);
            assert_eq!(checks.ntrunc, 0, "{theme}: truncated lock text");
        };
        check_geometry();
        lock_test_capture(&format!("{theme}-unavailable"));

        model.data_ok = 1;
        model.data.populated = 1;
        model.data.radio = "LTE".into();
        model.data.ap_on = 0;
        model.data.batt_pct = 75;
        ui::set_model(Rc::new(model.clone()));
        ui::with(|u| u.refresh());
        settle(100);
        let zero = text();
        assert!(!zero.contains("状态暂不可用"), "{theme}: {zero}");
        assert!(zero.contains("Wi-Fi 关"), "{theme}: {zero}");
        assert_eq!(zero.matches("0 B/s").count(), 2, "{theme}: {zero}");
        lock_test_capture(&format!("{theme}-zero-bytes"));

        // Repeat the paint with exactly the same input; unchanged labels must not invalidate.
        STILL.lock().unwrap().clear();
        let disp = unsafe { sys::lv_display_get_default() };
        unsafe { sys::lv_display_add_event_cb(disp, Some(still_cb), sys::lv_event_code_t::LV_EVENT_INVALIDATE_AREA, std::ptr::null_mut()) };
        ui::with(|u| u.paint_screen_lock());
        unsafe { sys::lv_display_remove_event_cb_with_user_data(disp, Some(still_cb), std::ptr::null_mut()) };
        assert!(STILL.lock().unwrap().is_empty(), "{theme}: identical lock frame invalidated");

        model.data.down_bps = 1024;
        model.data.up_bps = 2048;
        model.data.ap_on = 1;
        model.now += 1;
        ui::set_model(Rc::new(model.clone()));
        ui::with(|u| u.refresh());
        settle(100);
        let bytes = text();
        assert!(bytes.contains("1 KB/s") && bytes.contains("2 KB/s"), "{theme}: {bytes}");
        assert!(bytes.contains("Wi-Fi 开"), "{theme}: {bytes}");
        check_geometry();
        lock_test_capture(&format!("{theme}-bytes"));
        ui::with(|u| { u.conf.speed_bits = true; u.paint_screen_lock(); });
        settle(100);
        let bits = text();
        assert!(bits.contains("8 Kbps") && bits.contains("16 Kbps"), "{theme}: {bits}");
        check_geometry();
        lock_test_capture(&format!("{theme}-bits"));
        model.backend_silent = 1;
        ui::set_model(Rc::new(model));
        ui::with(|u| u.refresh());
        settle(100);
        let silent = text();
        assert!(silent.contains("状态暂不可用"), "{theme}: {silent}");
        lock_test_capture(&format!("{theme}-silent"));
    }
}

/// `luyaz-ui --hitcheck` (eng review round 3 D5): Home, every tab and every subpage with the ▲
/// shown (an open incident can keep it lit for days): nothing tappable may sit under its 44×44
/// box. One line per page, "HIT ok <page>" or "HIT FAIL <page>: <what> at x,y w×h"; exit 1 on any.
pub fn hitcheck(args: &[String]) -> i32 {
    let dark = args.iter().any(|a| a == "--dark");
    // SAFETY: LVGL is set up once, on this thread; FB outlives it
    unsafe {
        sys::lv_init();
        sys::lv_tick_set_cb(Some(tick_cb));
        let disp = sys::lv_display_create(W as i32, H as i32);
        sys::lv_display_set_flush_cb(disp, Some(flush_cb));
        sys::lv_display_set_buffers(
            disp,
            (&raw mut FB).cast(),
            std::ptr::null_mut(),
            (W * H * 2) as u32,
            sys::lv_display_render_mode_t::LV_DISPLAY_RENDER_MODE_DIRECT,
        );
    }
    let conf = Conf::parse(if dark { "appearance=dark\nautooff=0\n" } else { "appearance=light\nautooff=0\n" });
    let src = Box::new(Replay {
        feed: BTreeMap::new(),
        step: Rc::new(RefCell::new(None)),
        cur: Rc::new(RefCell::new(String::new())),
        next: None,
        #[cfg(test)]
        wifi_test: None,
    });
    ui::create(conf, logic::launch_parse(&["luyaz-ui"]), Rc::new(Model::default()), src, "luyaz-ui");
    settle(200);
    fn tappable_under(o: Obj, box_: &sys::lv_area_t, skip: Obj, out: &mut Vec<String>) {
        if o.is_null() || o.hidden() {
            return;
        }
        if o != skip && o.has_flag(lv::LV_OBJ_FLAG_CLICKABLE) && o.event_count() > 0 {
            let a = o.coords();
            let (w, h) = (a.x2 - a.x1 + 1, a.y2 - a.y1 + 1);
            // a page-sized container (gestures, swipes) is not a control; a control counts when
            // the box covers a fifth of it or more (a card's corner strip still taps)
            let ix = (a.x2.min(box_.x2) - a.x1.max(box_.x1) + 1).max(0);
            let iy = (a.y2.min(box_.y2) - a.y1.max(box_.y1) + 1).max(0);
            let page = w >= W as i32 / 2 && h >= 200;
            if !page && ix * iy * 5 >= w * h {
                let mut t: Vec<u8> = Vec::new();
                for i in 0..o.child_count() as i32 {
                    let c = o.child(i);
                    if !c.hidden() && checks::is_label(c) && !c.text().is_empty() {
                        t = c.text();
                        break;
                    }
                }
                out.push(format!(
                    "\"{}\" at {},{} {}x{}",
                    String::from_utf8_lossy(&t[..t.len().min(40)]),
                    a.x1,
                    a.y1,
                    w,
                    h
                ));
            }
        }
        for i in 0..o.child_count() as i32 {
            tappable_under(o.child(i), box_, skip, out);
        }
    }
    let check = |page: &str| -> usize {
        let hit = ui::with(|u| u.alert_probe());
        settle(60);
        let b = hit.coords();
        let mut out = Vec::new();
        tappable_under(Obj::screen_active(), &b, hit, &mut out);
        tappable_under(Obj::layer_top(), &b, hit, &mut out);
        if out.is_empty() {
            println!("HIT ok {page} (box {},{} {}x{})", b.x1, b.y1, b.x2 - b.x1 + 1, b.y2 - b.y1 + 1);
        }
        for o in &out {
            println!("HIT FAIL {page}: {o}");
        }
        out.len()
    };
    let mut bad = 0;
    for tab in 0..TABS {
        ui::with(|u| {
            u.sub_close();
            u.tab_go(tab);
        });
        settle(400);
        bad += check(&format!("tab{tab}"));
    }
    for &s in SUBS {
        ui::with(|u| {
            u.sub_close();
            u.sub_open(s);
        });
        settle(400);
        bad += check(&format!("{s:?}"));
    }
    println!("HITCHECK {} pages, {bad} under the ▲", TABS + SUBS.len());
    if let Some(dir) = arg(args, "--incshots=") {
        incshots(&dir, dark);
    }
    i32::from(bad > 0)
}



/// `--hitcheck --incshots=DIR [--dark]`: the alerts subpage's 事故 section in each state (design
/// review 4A/11A/14A), one PNG each: inc-<state>-<light|dark>.png.
fn incshots(dir: &str, dark: bool) {
    use ui_model::{regress_item_t, regress_t};
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let it = |id: &str, kind: &str, name: &str, sub: &str, from: i64, to: i64, end: &str, count: i32, obs: &str| regress_item_t {
        id: id.into(),
        kind: kind.into(),
        name: name.into(),
        sub: sub.into(),
        from,
        to,
        end: end.into(),
        count,
        observed: obs.into(),
    };
    let open = vec![
        it("mem-abs-1", "regress-mem-abs", "内存超标", "", now - 7800, 0, "", 1, ""),
        it("egress-chill-1", "regress-egress", "出口不通", "chill", now - 1900, 0, "", 1, "症状：出口不通；原厂动作：redial"),
    ];
    let closed = vec![
        it("q3", "core-crash", "数据服务崩溃", "", now - 9000, now - 5000, "quiet", 3, ""),
        it("cell-1", "regress-cellular", "蜂窝断线", "", now - 20_000, now - 19_000, "recovered", 1, "原厂动作：lpm_cycle"),
    ];
    let reg = |fresh: i32, has: i32, open: Vec<regress_item_t>, total: i32, closed: Vec<regress_item_t>| regress_t {
        fresh,
        has,
        at: now - 1500,
        open_total: total,
        open,
        closed,
    };
    let many: Vec<regress_item_t> = (0..5)
        .map(|k| it(&format!("p{k}"), "regress-egress", "出口不通", if k % 2 == 0 { "chill" } else { "tailscale" }, now - 600 * (k + 1) as i64, 0, "", 1, ""))
        .collect();
    let states: Vec<(&str, Option<regress_t>)> = vec![
        ("open", Some(reg(1, 1, open.clone(), 2, closed.clone()))),
        ("many", Some(reg(1, 1, many, 7, vec![]))),
        ("stale", Some(reg(0, 1, open, 2, closed))),
        ("never", Some(reg(0, 0, vec![], 0, vec![]))),
        ("old", None),
        ("empty", Some(reg(1, 1, vec![], 0, vec![]))),
    ];
    for (name, r) in states {
        // a lit screen at this moment (the replay source reads these from the model)
        let m = Model {
            data_ok: 1,
            data: ui_model::core_snapshot_t { populated: 1, regress: r, ..Default::default() },
            bl_is_on: 1,
            bl_panel_lit: 1,
            now,
            ..Default::default()
        };
        ui::set_model(Rc::new(m));
        ui::with(|u| {
            u.sub_close();
            u.sub_open(Sub::Alerts);
        });
        settle(1500);
        Obj::screen_active().invalidate();
        Obj::layer_top().invalidate();
        // SAFETY: the default display, created above
        unsafe { sys::lv_refr_now(std::ptr::null_mut()) };
        // SAFETY: rendering is done; FB is not touched until the next lv call
        let fb = unsafe { std::slice::from_raw_parts((&raw const FB).cast::<u16>(), W * H) };
        let path = format!("{dir}/inc-{name}-{}.png", if dark { "dark" } else { "light" });
        if let Err(e) = std::fs::write(&path, plat::png_rgb565(W as u32, H as u32, fb)) {
            eprintln!("incshots: {path}: {e}");
        } else {
            println!("SHOT {path}");
        }
    }
}

pub fn run(args: &[String]) -> i32 {
    let (Some(dir), Some(name)) = (arg(args, "--models="), arg(args, "--name=")) else {
        eprintln!("render: --models=DIR --name=SCENE-THEME[-en] (see render/mod.rs)");
        return 2;
    };
    let en = name.ends_with("-en");
    let base = name.strip_suffix("-en").unwrap_or(&name);
    let Some((scene, theme)) = base.rsplit_once('-').filter(|(_, t)| *t == "light" || *t == "dark") else {
        eprintln!("render: --name={name}: SCENE-light|dark[-en]");
        return 2;
    };
    let log_path = format!("{dir}/index-{name}.txt");
    let log = match std::fs::read(&log_path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("render: {log_path}: {e}");
            return 2;
        }
    };
    let lines: Vec<&[u8]> = log.split(|&b| b == b'\n').filter(|l| !l.is_empty()).collect();
    let mut models = Models { dir: dir.clone(), cache: HashMap::new() };

    // the models C's refreshes saw, by refresh number
    let mut timeline: BTreeMap<u32, String> = BTreeMap::new();
    for l in &lines {
        let f: Vec<&str> = std::str::from_utf8(l).unwrap_or("").split(' ').collect();
        if f[0] == "refresh"
            && f.len() == 4
            && let Ok(n) = f[1].parse()
        {
            timeline.insert(n, f[3].to_string());
        }
    }
    let Some(first) = timeline.values().next().cloned() else {
        eprintln!("render: {log_path}: no refresh line (an old log? run render.sh --model again)");
        return 2;
    };
    let mut first_model = match models.get(&first) {
        Ok(m) => Some(m),
        Err(e) => {
            eprintln!("render: {e}");
            return 2;
        }
    };
    let cur = Rc::new(RefCell::new(first.clone()));
    let mut feed_models: BTreeMap<u32, (String, Rc<Model>)> = BTreeMap::new();
    for (n, h) in &timeline {
        match models.get(h) {
            Ok(m) => {
                feed_models.insert(*n, (h.clone(), m));
            }
            Err(e) => {
                eprintln!("render: {e}");
                return 2;
            }
        }
    }
    // the model an action inside the current step leaves (the C log's model line right after it)
    let pending: Pending = Rc::new(RefCell::new(None));
    let mut replay = Some(Box::new(Replay {
        feed: feed_models, step: pending.clone(), cur: cur.clone(), next: None,
        #[cfg(test)]
        wifi_test: None,
    }));

    let mut checks = Checks::default();
    checks.dump = args.iter().any(|a| a == "--dump");
    if let Some(e) = arg(args, "--expect=")
        && let Err(e) = checks.load_expect(&e, scene)
    {
        eprintln!("render: {e}");
        return 2;
    }

    // SAFETY: LVGL is set up once, on this thread; FB outlives it
    let disp = unsafe {
        sys::lv_init();
        sys::lv_tick_set_cb(Some(tick_cb));
        let disp = sys::lv_display_create(W as i32, H as i32);
        sys::lv_display_set_flush_cb(disp, Some(flush_cb));
        sys::lv_display_set_buffers(
            disp,
            (&raw mut FB).cast(),
            std::ptr::null_mut(),
            (W * H * 2) as u32,
            sys::lv_display_render_mode_t::LV_DISPLAY_RENDER_MODE_DIRECT,
        );
        let finger = sys::lv_indev_create();
        sys::lv_indev_set_type(finger, sys::lv_indev_type_t::LV_INDEV_TYPE_POINTER);
        sys::lv_indev_set_read_cb(finger, Some(finger_cb));
        disp
    };

    let mut r = Run {
        disp,
        name: name.clone(),
        tag: format!("[{scene}/{theme}{}]", if en { "/en" } else { "" }),
        png: arg(args, "--png="),
        pages: arg(args, "--pages=").map(|p| p.split(',').map(String::from).collect()),
        checks,
        shots: Vec::new(),
        pending: Vec::new(),
        skipped: false,
        cur: cur.clone(),
        n_same: 0,
        n_diff: 0,
        n_checked: 0,
        n_checks_differ: 0,
        timeline: 0,
    };

    let keep_going = args.iter().any(|a| a == "--keep-going");
    let mut stop: Option<String> = None;
    let mut created = false;
    let mut i = 0;
    while i < lines.len() {
        let line = String::from_utf8_lossy(lines[i]).into_owned();
        let f: Vec<&str> = line.split(' ').collect();
        *pending.borrow_mut() = None;
        if matches!(f[0], "click" | "settle" | "call" | "finger" | "state_add" | "state_remove")
            && let Some(n) = lines.get(i + 1).and_then(|l| l.strip_prefix(b"model "))
        {
            let h = String::from_utf8_lossy(n).into_owned();
            match models.get(&h) {
                Ok(m) => *pending.borrow_mut() = Some((h, m)),
                Err(e) => {
                    eprintln!("render: {e}");
                    return 2;
                }
            }
        }
        let num = |k: usize| f.get(k).and_then(|v| v.parse::<i32>().ok());
        let ok = match f[0] {
            "refresh" => true,
            "create" => {
                // ui.conf: autooff=0 keeps the screen lit however long it runs
                let conf = Conf::parse(&format!("appearance={theme}\nautooff=0\n{}", if en { "lang=en\n" } else { "" }));
                match first_model.take() {
                    Some(m) => match replay.take() {
                        Some(src) => {
                            ui::create(conf, logic::launch_parse(&["luyaz-ui"]), m, src, "luyaz-ui");
                            created = true;
                            true
                        }
                        None => false,
                    },
                    None => false,
                }
            }
            "model" if f.len() == 2 => match models.get(f[1]) {
                Ok(m) => {
                    *cur.borrow_mut() = f[1].to_string();
                    if created {
                        ui::set_model(m);
                    } else {
                        first_model = Some(m);
                    }
                    true
                }
                Err(e) => {
                    eprintln!("render: {e}");
                    return 2;
                }
            },
            "settle" => num(1).map(|ms| settle(ms as u32)).is_some(),
            // Deterministic motion evidence: no tick advance and no production UI hooks.
            "motion_probe" => match (f.get(1), f.get(2).and_then(|p| resolve(p))) {
                (Some(name), Some(o)) => {
                    // SAFETY: resolve returned a live LVGL object, on the render thread.
                    let (active, x, y) = unsafe {
                        (sys::lv_anim_count_running(),
                         sys::lv_obj_get_style_translate_x(o.0, lv::LV_PART_MAIN),
                         sys::lv_obj_get_style_translate_y(o.0, lv::LV_PART_MAIN))
                    };
                    println!("MOTION {name} active={active} x={x} y={y} hidden={} pressed={}",
                             o.hidden(), o.has_state(lv::LV_STATE_PRESSED));
                    true
                }
                _ => false,
            },
            "motion_frame" => match f.get(1) {
                Some(name) => {
                    r.unmatched();
                    r.capture(name, 0, false);
                    true
                }
                None => false,
            },
            "to_tab" => num(1).filter(|&t| (0..TABS as i32).contains(&t)).map(|t| ui::with(|u| u.to_tab(t as usize))).is_some(),
            "sub_open" => f.get(1).and_then(|n| sub_by_name(n)).map(|s| ui::with(|u| u.sub_open(s))).is_some(),
            "sub_open_child" => match (f.get(1).and_then(|n| sub_by_name(n)), f.get(2).and_then(|n| sub_by_name(n))) {
                (Some(s), Some(p)) => {
                    ui::with(|u| u.sub_open_child(s, p));
                    true
                }
                _ => false,
            },
            "sub_close" => {
                ui::with(|u| u.sub_close());
                true
            }
            "sub_back" => {
                ui::with(|u| u.sub_back());
                true
            }
            // an object without a handler here has one in C that is not ported yet: not the same step
            "click" => f
                .get(1)
                .and_then(|p| resolve(p))
                .filter(|o| o.event_count() > 0)
                .map(|o| o.send_event(sys::lv_event_code_t::LV_EVENT_CLICKED))
                .is_some(),
            "finger" => match (num(1), num(2), num(3)) {
                (Some(x), Some(y), Some(d)) => {
                    FX.store(x, Ordering::Relaxed);
                    FY.store(y, Ordering::Relaxed);
                    FDOWN.store(d != 0, Ordering::Relaxed);
                    true
                }
                _ => false,
            },
            "state_add" | "state_remove" => f.get(1).and_then(|p| resolve(p)).map(|o| {
                if f[0] == "state_add" {
                    o.add_state(sys::lv_state_t::LV_STATE_PRESSED)
                } else {
                    o.remove_state(sys::lv_state_t::LV_STATE_PRESSED)
                }
            }).is_some(),
            "scroll_to_y" => match (f.get(1).and_then(|p| resolve(p)), num(2)) {
                (Some(o), Some(y)) => {
                    o.scroll_to_y(y);
                    true
                }
                _ => false,
            },
            // the backlight is the data side's: its effect comes as a model line
            "call" => match (f.get(1).copied(), num(2)) {
                (Some("refresh_data()"), _) => {
                    ui::with(|u| u.refresh_data());
                    true
                }
                (Some("refresh()"), _) => {
                    ui::with(|u| u.refresh());
                    true
                }
                (Some("backlight_on()" | "backlight_off()"), _) => true,
                (Some("net_clear_arms()"), _) => {
                    ui::with(|u| u.net_clear_arms());
                    true
                }
                (Some("net_paint"), Some(v)) => {
                    ui::with(|u| u.net_paint(v != 0));
                    true
                }
                // the status card's footer action, as a tap on it would (fixtures of story.fix)
                (Some("home_act()"), _) => {
                    ui::with(|u| u.home_act());
                    true
                }
                (Some("diag_paint()"), _) => {
                    ui::with(|u| u.diag_paint());
                    true
                }
                (Some("lang_frame_show"), Some(v)) => {
                    ui::with(|u| u.lang_frame_show(v != 0));
                    true
                }
                (Some("lang_frame_hide()"), _) => {
                    ui::with(|u| u.lang_frame_hide());
                    true
                }
                (Some("lang_note_reset_cb(NULL)"), _) => {
                    ui::with(|u| u.lang_note_reset());
                    true
                }
                (Some("power_menu_set"), Some(v)) => {
                    ui::with(|u| u.power_menu_set(v != 0));
                    true
                }
                (Some("exit_menu_set"), Some(v)) => {
                    ui::with(|u| u.exit_menu_set(v != 0));
                    true
                }
                _ => false,
            },
            "poke" => match (f.get(1).copied(), f.get(2).and_then(|v| v.parse::<i64>().ok())) {
                (Some("s_lk_mode_pending"), Some(v)) => {
                    ui::with(|u| u.lk.mode_pending = v as i32);
                    true
                }
                (Some("s_lk_mode_want"), Some(v)) => {
                    ui::with(|u| u.lk.mode_want = v as i32);
                    true
                }
                (Some("s_lk_mode_sent"), Some(v)) => {
                    ui::with(|u| u.lk.mode_sent = v as u32);
                    true
                }
                (Some(k), Some(v)) => ui::with(|u| u.poke(k, v)),
                _ => false,
            },
            "shoot_page" => match (f.get(1), f.get(2).and_then(|p| resolve(p))) {
                (Some(page), Some(root)) => {
                    r.shoot_page(page, root);
                    true
                }
                _ => false,
            },
            "overlay" => match (f.get(1), f.get(2).and_then(|p| resolve(p))) {
                (Some(page), Some(root)) => {
                    r.unmatched();
                    r.checks.texts_clear();
                    r.checks.collect(root);
                    r.shot(page, 0);
                    true
                }
                _ => false,
            },
            "shot" => {
                r.c_shot(&f[1..]);
                true
            }
            "page_done" => {
                // C's outcome follows: cfail lines, then "checked PAGE PASS FAIL TRUNC"
                let mut c_fails: Vec<&[u8]> = Vec::new();
                let mut c_checked: Vec<String> = Vec::new();
                while i + 1 < lines.len() {
                    let n = lines[i + 1];
                    if let Some(m) = n.strip_prefix(b"cfail ") {
                        c_fails.push(m);
                    } else if n.starts_with(b"checked ") {
                        c_checked = String::from_utf8_lossy(n).split(' ').skip(1).map(String::from).collect();
                        i += 1;
                        break;
                    } else {
                        break;
                    }
                    i += 1;
                }
                let cc: Vec<&str> = c_checked.iter().map(String::as_str).collect();
                match f.get(1) {
                    Some(page) => {
                        r.page_done(page, &c_fails, &cc);
                        true
                    }
                    None => false,
                }
            }
            _ => false,
        };
        if !ok && keep_going {
            println!("SKIP {} line {}: {line}", r.tag, i + 1);
            r.skipped = true;
        } else if !ok {
            stop = Some(format!("{}: {line}", i + 1));
            break;
        }
        i += 1;
    }
    r.unmatched();
    if args.iter().any(|a| a == "--still") && created {
        still_check(&r.tag);
    }
    // the shots C took of the reported pages, and the ones this run never got to
    let shot_name = |l: &[u8]| l.strip_prefix(b"shot ").map(|t| String::from_utf8_lossy(t).split(' ').next().unwrap_or("").to_string());
    let page_of = |n: &str| n.rsplit_once('.').map_or(n.to_string(), |p| p.0.to_string());
    let c_wanted = lines.iter().filter_map(|l| shot_name(l)).filter(|n| r.wanted(&page_of(n))).count();
    let mut not_reached = 0;
    if let Some(s) = &stop {
        println!("STOP {} line {s}", r.tag);
        for n in lines[i..].iter().filter_map(|l| shot_name(l)).filter(|n| r.wanted(&page_of(n))) {
            println!("NOTREACHED {}-{n}", r.name);
            not_reached += 1;
        }
    }
    println!(
        "{} rust: {} shots, C shots reported {}, same {}, differ {}, not reached {}, checked pages {} (differ {}), timeline drifts {}{}",
        r.tag,
        r.shots.len(),
        c_wanted,
        r.n_same,
        r.n_diff,
        not_reached,
        r.n_checked,
        r.n_checks_differ,
        r.timeline,
        if stop.is_some() { ", stopped early" } else { "" }
    );
    i32::from(r.n_diff > 0 || r.n_checks_differ > 0 || r.timeline > 0 || not_reached > 0)
}

static STILL: std::sync::Mutex<Vec<sys::lv_area_t>> = std::sync::Mutex::new(Vec::new());

unsafe extern "C" fn still_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LV_EVENT_INVALIDATE_AREA's parameter is the area
    let a = unsafe { *(sys::lv_event_get_param(e) as *const sys::lv_area_t) };
    if std::env::var_os("LUYAZ_STILL_BT").is_some() {
        let bt = std::backtrace::Backtrace::force_capture().to_string();
        let f: Vec<&str> = bt.lines().filter(|l| l.contains("luyaz_ui::") && !l.contains("still")).take(4).map(str::trim).collect();
        println!("  BT {},{}-{},{} {}", a.x1, a.y1, a.x2, a.y2, f.join(" < "));
    }
    STILL.lock().unwrap().push(a);
}

/// --still: nothing changes, so nothing may be invalidated (the device's quiet loop): each tab,
/// then each subpage (they refresh too while open, e.g. the eSIM page's 双卡 and 分卡来电).
fn still_check(tag: &str) {
    let views = (0..TABS).map(|t| (format!("tab{t}"), None)).chain(SUBS.iter().map(|s| (format!("sub{s:?}"), Some(*s))));
    for (name, sub) in views {
        match sub {
            None => ui::with(|u| u.to_tab(name[3..].parse().unwrap_or(0))),
            Some(s) => ui::with(|u| {
                u.sub_close();
                u.sub_open(s)
            }),
        }
        settle(3000);
        STILL.lock().unwrap().clear();
        // SAFETY: LVGL is initialised; one thread
        let disp = unsafe { sys::lv_display_get_default() };
        unsafe { sys::lv_display_add_event_cb(disp, Some(still_cb), sys::lv_event_code_t::LV_EVENT_INVALIDATE_AREA, std::ptr::null_mut()) };
        settle(5000);
        unsafe { sys::lv_display_remove_event_cb_with_user_data(disp, Some(still_cb), std::ptr::null_mut()) };
        let v = STILL.lock().unwrap();
        let mut areas: Vec<String> = v.iter().map(|a| format!("{},{}-{},{}", a.x1, a.y1, a.x2, a.y2)).collect();
        areas.sort();
        areas.dedup();
        if std::env::var_os("LUYAZ_STILL_WHO").is_some() {
            for a in v.iter() {
                let mut best = None;
                who(Obj::screen_active(), a, &mut best);
                who(Obj::layer_top(), a, &mut best);
                if let Some((o, _)) = best {
                    let o: Obj = o;
                    let t = if o.check_type_label() { String::from_utf8_lossy(&o.text()).into_owned() } else { String::new() };
                    println!("  {},{}-{},{} {:?} {t:?}", a.x1, a.y1, a.x2, a.y2, o.0);
                }
            }
        }
        println!("STILL {tag} {name} {} {}", v.len(), areas.join(" "));
    }
    ui::with(|u| u.sub_close());
}

/// The smallest visible object inside an invalidated area (--still, LUYAZ_STILL_WHO=1).
fn who(o: Obj, a: &sys::lv_area_t, best: &mut Option<(Obj, i64)>) {
    if o.is_null() || o.hidden() {
        return;
    }
    let c = o.coords();
    if c.x1 >= a.x1 - 8 && c.y1 >= a.y1 - 8 && c.x2 <= a.x2 + 8 && c.y2 <= a.y2 + 8 {
        let area = i64::from(c.x2 - c.x1 + 1) * i64::from(c.y2 - c.y1 + 1);
        if best.is_none_or(|b| area >= b.1) {
            *best = Some((o, area));
        }
    }
    for i in 0..o.child_count() as i32 {
        who(o.child(i), a, best);
    }
}
