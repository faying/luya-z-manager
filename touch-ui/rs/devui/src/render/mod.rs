//! Offscreen render (--render): replay one run of the C render test (tests/render/render_test.c)
//! on the Rust screen, so the two can be compared shot for shot (manager
//! docs/designs/u60-touch-rewrite.md §4).
//!
//! The C test's --dump-model=DIR writes the run log DIR/index-<name>.txt (its header has the
//! grammar) and the view models DIR/<hash>.json. This harness sets up LVGL as the C test does
//! (DIRECT render mode into one 320×480 RGB565 buffer, a tick that only moves in settle(), 20 ms
//! per lv_timer_handler call, devui.conf with the theme, autooff=0 and the language), feeds the
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
// SPDX-License-Identifier: MIT

mod checks;

use crate::conf::Conf;
use crate::logic::{self, Launch};
use crate::source::{self, Activity, NiAct, Polled, Source};
use crate::lv::{self, sys, Obj};
use crate::ui::{self, Sub, SUBS, TABS};
use checks::Checks;
use devui_model::Model;
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

unsafe extern "C" fn finger_cb(_in: *mut sys::lv_indev_t, d: *mut sys::lv_indev_data_t) {
    let (x, y, down) = (FX.load(Ordering::Relaxed), FY.load(Ordering::Relaxed), FDOWN.load(Ordering::Relaxed));
    let pressed = ui::with(|u| u.touch_filter(down, x, y));
    // SAFETY: LVGL's read buffer for this call
    unsafe {
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

/// The data side as the C render test's fixtures answer (tests/render/fixtures.c): every model
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
            tailscale: true,
            esim: true,
            speedtest: true,
            servers: true,
            battery: false,
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
    fn datad_version(&self, m: &Model) -> u64 {
        m.datad_version
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
    fn data_control_fb(&mut self, _a: &str, _p: &str, _fa: &str, _fb: Option<&str>) -> bool {
        self.acted();
        true
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
    /// profile 0 is the one in use; another arms
    fn esim_select(&mut self, i: usize) -> i32 {
        self.acted();
        if i == 0 { source::ESIM_SEL_CURRENT } else { source::ESIM_SEL_ARMED }
    }
    fn speedtest_start(&mut self) -> bool {
        self.acted();
        true
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
    fn power_last_resort(&mut self, _reboot: bool) {}
    fn lang_hold(&mut self) {}
    fn alert_theme_paused(&mut self) {}
    fn aux_read(&mut self) -> Option<(i32, i32, i32)> {
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
        Obj::screen_active().invalidate();
        Obj::layer_top().invalidate();
        // SAFETY: the display this harness created
        unsafe { sys::lv_refr_now(self.disp) };
        self.checks.geo_walk(Obj::screen_active());
        self.checks.geo_walk(Obj::layer_top());
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
    let mut replay = Some(Box::new(Replay { feed: feed_models, step: pending.clone(), cur: cur.clone(), next: None }));

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
                // the C test's devui.conf: autooff=0 keeps the screen lit however long it runs
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
                (Some("backlight_on()" | "backlight_off()"), _) => true,
                (Some("net_clear_arms()"), _) => {
                    ui::with(|u| u.net_clear_arms());
                    true
                }
                (Some("net_paint"), Some(v)) => {
                    ui::with(|u| u.net_paint(v != 0));
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
        let f: Vec<&str> = bt.lines().filter(|l| l.contains("devui::") && !l.contains("still")).take(4).map(str::trim).collect();
        println!("  BT {},{}-{},{} {}", a.x1, a.y1, a.x2, a.y2, f.join(" < "));
    }
    STILL.lock().unwrap().push(a);
}

/// --still: nothing changes, so nothing may be invalidated (the device's quiet loop).
fn still_check(tag: &str) {
    for t in 0..TABS {
        ui::with(|u| u.to_tab(t));
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
        println!("STILL {tag} tab{t} {} {}", v.len(), areas.join(" "));
    }
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
