//! The screen: settings, the tileview with five tabs, the subpage layer, and the
//! shared chrome on lv_layer_top() (status bar, floating tab capsule, banner, 事务行). Pages are
//! in the submodules, one per C file in src/ui_parts/. Data comes only from the view model
//! (ui_model::Model): the page code does not know where it came from.
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

mod apn;
mod cellular;
mod diagnose;
mod dualsim;
mod esim;
mod exit;
mod home;
mod lock;
mod network;
mod op;
mod power;
mod refresh;
mod sms;
mod speed;
mod system;
mod usbmode;
mod wifi;

use crate::conf::Conf;
use crate::source::{Polled, Source};
use crate::kit::{self, Battery, BAR_H, CARD_W, H, MARGIN, NAV_H, TAB_GAP, TAB_H, W};
use crate::lang::{self, n_, tr};
use crate::logic::{self, Launch};
use crate::lv::{self, sys, Obj};
use crate::theme::{self, t, uf};
use ui_model::Model;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

pub const TABS: usize = 5;
pub const TAB_HOME: usize = 0;
pub const TAB_CELL: usize = 1;
pub const TAB_WIFI: usize = 2;
pub const TAB_EXIT: usize = 3;
pub const TAB_SYS: usize = 4;

/// Subpages: opened from a tab's › rows, drawn over the tileview on the active screen (so the
/// status bar and tab bar on lv_layer_top() still paint above them). Built once, shown/hidden.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum Sub {
    Sms,
    Cell,
    Lock,
    Speed,
    Chill,
    Esim,
    Perf,
    /// opened by tapping the Home Tailscale card
    Ts,
    ChillNodes,
    ChillPairs,
    SmsDetail,
    Alerts,
    Net,
    Scene,
    Apn,
    AlertDetail,
    Diag,
    Place,
    Op,
    Log,
    LogDetail,
}

pub const SUBS: &[Sub] = &[
    Sub::Sms,
    Sub::Cell,
    Sub::Lock,
    Sub::Speed,
    Sub::Chill,
    Sub::Esim,
    Sub::Perf,
    Sub::Ts,
    Sub::ChillNodes,
    Sub::ChillPairs,
    Sub::SmsDetail,
    Sub::Alerts,
    Sub::Net,
    Sub::Scene,
    Sub::Apn,
    Sub::AlertDetail,
    Sub::Diag,
    Sub::Place,
    Sub::Op,
    Sub::Log,
    Sub::LogDetail,
];

/// The title = the words on the way in.
fn sub_title(s: Sub) -> &'static str {
    match s {
        Sub::Sms => n_("短信"),
        Sub::Cell => n_("小区信息"),
        Sub::Lock => n_("锁频"),
        Sub::Speed => n_("测速"),
        Sub::Chill => "CHILL",
        Sub::ChillNodes => n_("节点"),
        Sub::ChillPairs => n_("规则 → 节点"),
        Sub::Esim => n_("SIM 与 eSIM"),
        Sub::Perf => n_("性能测试"),
        Sub::Ts => "Tailscale",
        Sub::SmsDetail => n_("短信详情"),
        Sub::Alerts => n_("健康与告警"),
        Sub::Net => n_("运营商选择"),
        Sub::Scene => n_("情景"),
        Sub::Apn => "APN",
        Sub::AlertDetail => n_("详情"),
        Sub::Diag => n_("网络诊断"),
        Sub::Place => n_("摆放模式"),
        Sub::Op => n_("改动"),
        Sub::Log => n_("改动记录"),
        Sub::LogDetail => n_("详情"),
    }
}

/// A top-level page's area: everything under the status bar (the floating tab covers
/// content; scroll bodies keep TAB_PAD clear at the end).
pub const VIEW_H: i32 = H - BAR_H;
/// subpage header below the status bar: ‹ + title
pub const SUB_HDR: i32 = NAV_H - BAR_H;
/// a subpage's area under its header
pub const SUB_VIEW: i32 = H - NAV_H;

const TOP_BATT_XR: i32 = 308;
/// time + dots + RAT end here; the rate never crosses it
const TOP_LEFT_END: i32 = 150;

/// Five top-level pages, text only.
const TAB_NAMES: [&str; TABS] = [n_("首页"), n_("蜂窝"), "Wi-Fi", n_("出口"), n_("系统")];

/// Where screen_feed_net() is None and why.
pub const SF_OLD_DATAD: i32 = 2;

pub struct Ui {
    pub model: Rc<Model>,
    pub conf: Conf,
    pub dark: bool,
    pub launch: Launch,
    pub autooff_ms: u32,

    pub tv: Obj,
    pub tiles: [Obj; TABS],
    tabs: [Obj; TABS],
    tab_bar: Obj,
    tab_pill: [Obj; TABS],
    pub sub_layer: Obj,
    sub_title: Obj,
    pub sub_page: Vec<Obj>,
    pub sub_cur: Option<Sub>,
    sub_parent: Option<Sub>,

    // status bar
    top_time: Obj,
    top_net: Obj,
    top_updown: Obj,
    top_rate_hit: Obj,
    top_sig: [Obj; 5],
    /// dual standby: two rows of 5 (data slot on top, standby below)
    top_sig2: [Obj; 10],
    /// dual rows shown (None = not yet decided)
    sig_dual: Option<bool>,
    top_alert: Obj,
    top_alert_hit: Obj,
    top_batt: Battery,
    /// 0 none, 1 unread, 2 agent lost
    alert_st: i32,
    /// 顶栏制式：信号卡算出来的 5G-A / 5G+ / 4G+ …
    pub top_label: String,
    rate_shown: Vec<u8>,
    c_sig: [u32; 5],
    c_sig2: [u32; 10],
    c_pct: i32,
    c_chg: i32,

    banner: Obj,
    banner_txt: Obj,
    banner_shown: Vec<u8>,

    /// the small text right of each 「›」 row on the tabs (by Sub index; the last row built
    /// for a page wins, as in C)
    pub tile_sub: Vec<Obj>,
    /// the last nav_card's section and card
    nav_sec: Obj,
    nav_card: Obj,

    pub home: home::Home,
    pub aux: home::Aux,
    pub cell: cellular::Cell,
    pub lk: lock::Lock,
    pub wifi: wifi::Wifi,
    pub net: network::Net,
    pub ex: exit::Exit,
    pub sys: system::Sys,
    pub charts: system::Charts,
    pub op: op::Op,
    pub pw: power::Power,
    pub um: usbmode::Usb,
    pub sms: sms::Sms,
    pub es: esim::Esim,
    pub ds: dualsim::DualSim,
    pub st: speed::Speed,
    pub ts: exit::TsPage,
    pub sg: exit::SigPage,
    pub cp: exit::ChillPage,
    pub apn: apn::Apn,
    pub dg: diagnose::Diag,
    pub pl: diagnose::Place,
    /// the benchmark timer: runs only while 性能测试 is open (bench_gate)
    bench_timer: lv::Timer,
    /// the full-screen 「正在切换到 English…」 frame, made the first time it shows
    pub lang_frame: Obj,
    /// the left-edge swipe back (edge_back_cb)
    edge_p0: (i32, i32),
    edge_armed: bool,
    /// the card in the modem (eSIM section of the refresh): the 「SIM 与 eSIM」 row
    pub sim: refresh::Sim,
    /// where the data comes from and the actions go (the device's data side, or the replay)
    pub src: Box<dyn Source>,
    /// what this refresh's polls said
    pub polled: Polled,
    /// the › rows' colours last set (tile_rows_paint)
    c_tile_on: [i32; 2],

    /// refresh() calls so far (the C test's rt_refreshes: refresh 1 is the one in create)
    pub refreshes: u32,
    /// the touch gate in front of the touchscreen (ui_touch_filter)
    tgate: logic::TGate,
    /// the screen went off by itself (not with the power key): a double tap wakes it
    pub auto_slept: bool,
    /// started with the screen off (an exec): wait for a real touch before auto-off acts
    wake_guard: bool,
    wake_idle_last: u32,
    /// the power key's poll (key_poll_cb): 50 ms, 1 s while the lit screen is quiet, paused dark
    key_timer: lv::Timer,
    /// automatic appearance switching paused (3 switches within an hour, or the exec failed)
    auto_suspended: bool,
    /// argv[0] for the exec
    pub argv0: String,

    /// set_label_fmt's caches: the text last written to a label through set()
    label_cache: HashMap<usize, Vec<u8>>,
    /// the same for a label C writes from two places, each with its own cache (set_at)
    site_cache: HashMap<(usize, u8), Vec<u8>>,
}

thread_local! {
    static UI: RefCell<Option<Box<Ui>>> = const { RefCell::new(None) };
}

/// Run f on the screen (from LVGL callbacks and the harness).
pub fn with<R>(f: impl FnOnce(&mut Ui) -> R) -> R {
    UI.with(|u| f(u.borrow_mut().as_mut().expect("ui::create first")))
}

impl Ui {
    /// Write a label only when its text changed (lv_label_set_text frees and mallocs even for
    /// the same text; refresh writes every label every second). Like set_label_fmt: at most
    /// 255 bytes.
    pub fn set(&mut self, l: Obj, text: impl AsRef<[u8]>) {
        let text = text.as_ref();
        let text = &text[..text.len().min(255)];
        let k = l.0 as usize;
        if self.label_cache.get(&k).map_or(text.is_empty(), |c| c.as_slice() == text) {
            return;
        }
        self.label_cache.insert(k, text.to_vec());
        l.set_text(text);
    }

    /// set() for a label written from more than one place: one cache per place (`site`), so
    /// what shows is what C shows (the last writer whose own text changed).
    pub fn set_at(&mut self, l: Obj, site: u8, text: impl AsRef<[u8]>) {
        let text = text.as_ref();
        let text = &text[..text.len().min(255)];
        let k = (l.0 as usize, site);
        if self.site_cache.get(&k).map_or(text.is_empty(), |c| c.as_slice() == text) {
            return;
        }
        self.site_cache.insert(k, text.to_vec());
        l.set_text(text);
    }

    /// After an action: what it changed, as the getters see it now (the eSIM row armed, the APN
    /// switching …), so the screen can draw it at once.
    pub fn sync(&mut self) {
        if let Some(m) = self.src.take_model() {
            self.model = m;
        }
    }

    /// A write through core (/control), with the emergency write the script makes of it when
    /// core cannot be reached (fb None = none).
    pub fn data_control(&mut self, action: &str, params: &str, fb: Option<&str>) -> bool {
        self.data_control_fb(action, params, action, fb)
    }

    /// data_control whose emergency write is another script action (Wi-Fi: wifi.radio).
    pub fn data_control_fb(&mut self, action: &str, params: &str, fb_action: &str, fb: Option<&str>) -> bool {
        let r = self.src.data_control_fb(action, params, fb_action, fb);
        self.sync();
        r
    }

    /// luyaz-core: turn CHILL on or off.
    pub fn scenario_chill_set(&mut self, on: bool) -> bool {
        let r = self.src.scenario_chill_set(on);
        self.sync();
        r
    }

    /// The exit (luyaz-core PUT /api/services/chill/exit).
    pub fn chill_set_exit(&mut self, state: &str) -> bool {
        let r = self.src.chill_set_exit(state);
        self.sync();
        r
    }

    /// The 情景 card repaints on the next refresh (s_sc_force).
    pub fn home_sc_force(&mut self) {
        self.home.sc_force = true;
    }

    /// A UI variable the render test sets directly (its `poke` steps); false = not one of them.
    pub fn poke(&mut self, name: &str, v: i64) -> bool {
        match name {
            "s_smsd_id" => self.sms.smsd_id = v,
            "s_sc_flash.until" => self.net.sc_flash.until = v as u32,
            "s_sc_pend" => self.net.sc_pend = v as i32,
            "s_ms_flash.until" => self.net.ms_flash.until = v as u32,
            "s_apn_flash.until" => self.apn.flash.until = v as u32,
            "s_apn_pend" => self.apn.pend = v as i32,
            "s_apn_arm_idx" => self.apn.arm_idx = v as i32,
            "s_es_flash.until" => self.es.flash.until = v as u32,
            "s_es_dirty" => self.es.dirty = v != 0,
            "s_autooff_ms" => self.autooff_ms = v as u32,
            "s_auto_slept" => self.auto_slept = v != 0,
            "s_net_roam" => self.dg.net_roam = v != 0,
            "s_cf_usb" => {
                self.conf.usb = [logic::UsbPref::Ask, logic::UsbPref::Share, logic::UsbPref::Fast, logic::UsbPref::Accessory]
                    .get(v as usize)
                    .copied()
                    .unwrap_or_default()
            }
            "s_hx_note_at" => self.cp.hx_note_at = v as u32,
            _ => return false,
        }
        true
    }

    pub fn cur_tab(&self) -> usize {
        let act = if self.tv.is_null() { Obj::NULL } else { self.tv.tile_active() };
        self.tiles.iter().position(|&t| t == act).unwrap_or(0)
    }

    pub fn tab_visible(&self, tab: usize) -> bool {
        self.bl_is_on() && self.sub_cur.is_none() && self.tv.tile_active() == self.tiles[tab]
    }

    pub fn sub_visible(&self, s: Sub) -> bool {
        self.bl_is_on() && self.sub_cur == Some(s)
    }

    /// The wall clock (time(NULL); the device's clock is local time labelled UTC).
    pub fn now(&self) -> i64 {
        self.src.now(&self.model)
    }

    /// backlight_is_on()
    pub fn bl_is_on(&self) -> bool {
        self.src.bl_is_on(&self.model)
    }

    /// The screen went dark (ui_idle): the key poll pauses, the main loop waits on the key fd.
    pub fn idle(&mut self, dark: bool) {
        if self.key_timer.is_null() {
            return;
        }
        if dark {
            self.key_timer.pause();
        } else {
            self.key_timer.resume();
        }
    }

    /// Lit and nothing moving (ui_quiet): the key poll slows to 1 s; a press wakes the loop
    /// through the key fd.
    pub fn quiet(&mut self, quiet: bool) {
        if !self.key_timer.is_null() {
            self.key_timer.set_period(if quiet { 1000 } else { 50 });
        }
    }

    /// The power key is held (its long press must keep being polled on a dark screen).
    pub fn key_held(&self) -> bool {
        self.src.key_held()
    }

    // ---- subpage layer ----
    fn sub_show(&mut self, id: Sub, forward: bool) {
        let changed = self.sub_cur != Some(id);
        for (i, &pg) in self.sub_page.iter().enumerate() {
            kit::show(pg, i == id as usize);
        }
        self.sub_title.set_text(tr(sub_title(id)));
        self.sub_layer.remove_flag(lv::LV_OBJ_FLAG_HIDDEN);
        if !forward {
            // Returning reveals the parent immediately; it must not look like another push.
            kit::anim_stop(self.sub_layer);
        } else if changed {
            kit::anim_push(self.sub_layer);
        }
        self.sub_cur = Some(id);
        self.sub_parent = None;
        self.bench_gate();
        self.update_tabs();
        self.diag_hdr_sync();
        match id {
            Sub::Diag => self.diag_on_open(),
            Sub::Place => self.place_on_open(),
            Sub::Op => self.op_on_open(),
            Sub::Log => self.op_log_on_open(),
            Sub::LogDetail => self.op_logd_on_open(),
            _ => {}
        }
    }

    /// Open a subpage fresh: always from its top.
    pub fn sub_open(&mut self, id: Sub) {
        self.sub_show(id, true);
        if let Some(sc) = obj_scroller(self.sub_page[id as usize]) {
            sc.scroll_to_y(0);
        }
    }

    pub fn sub_close(&mut self) {
        self.sub_layer.add_flag(lv::LV_OBJ_FLAG_HIDDEN);
        kit::anim_stop(self.sub_layer);
        self.sub_cur = None;
        self.sub_parent = None;
        self.bench_gate();
        self.update_tabs();
        self.diag_hdr_sync();
    }

    /// The benchmark burns a full redraw per millisecond: it runs only while its page is up.
    pub fn bench_gate(&mut self) {
        if self.bench_timer.is_null() {
            return;
        }
        if self.sub_cur == Some(Sub::Perf) {
            self.bench_timer.resume();
        } else {
            self.bench_timer.pause();
        }
    }

    /// Open a page one level below a top-level subpage; 返回 goes back to `parent`.
    pub fn sub_open_child(&mut self, id: Sub, parent: Sub) {
        self.sub_open(id);
        self.sub_parent = Some(parent);
    }

    pub fn sub_back(&mut self) {
        match self.sub_parent {
            Some(p) => self.sub_show(p, false),
            None => self.sub_close(),
        }
    }

    // ---- tabs ----
    fn any_sheet_open(&self) -> bool {
        if kit::sheet_visible(&self.ex.xm) {
            return true;
        }
        self.power_menu_visible() || self.usbmode_visible()
    }

    pub fn update_tabs(&mut self) {
        // sub_close() runs during create before the tab bar exists
        if self.tab_bar.is_null() {
            return;
        }
        let act = self.tv.tile_active();
        for i in 0..TABS {
            let on = self.tiles[i] == act;
            kit::show(self.tab_pill[i], on);
            self.tabs[i].set_style_text_font(if on { uf().cj15b } else { uf().cj13 }, 0);
            kit::text_color(self.tabs[i], if on { t().acc_t } else { t().t2 });
        }
        kit::show(self.tab_bar, self.sub_cur.is_none() && !self.any_sheet_open());
    }

    /// Jump to a tab from somewhere else (Home's summary rows): from its top.
    pub fn tab_go(&mut self, idx: usize) {
        self.sub_close();
        self.tv.set_tile_by_index(idx as u32, 0);
        if let Some(sc) = obj_scroller(self.tiles[idx]) {
            sc.scroll_to_y(0);
        }
        self.update_tabs();
    }

    /// The tab bar: another tab keeps where it was; the tab you are on goes to its top.
    fn tab_click(&mut self, idx: usize) {
        if self.sub_cur.is_none() && self.cur_tab() == idx {
            self.tab_go(idx);
            return;
        }
        self.sub_close();
        self.tv.set_tile_by_index(idx as u32, 0);
        self.update_tabs();
    }

    /// The render harness's to_tab: no subpage, no sheet, this tab.
    pub fn to_tab(&mut self, i: usize) {
        self.sub_close();
        self.power_menu_set(false);
        self.exit_menu_set(false);
        self.tv.set_tile_by_index(i as u32, 0);
        self.update_tabs();
        self.bench_gate();
    }

    /// 二级页: a swipe right from the left edge (x < 24) = back (2026-09-25). On the input
    /// device, decided on release; when it counts the press is cancelled, so the row under
    /// the finger gets no click (a swipe back must not also tap an option that cuts the net).
    fn edge_back(&mut self, pressed: bool, p: (i32, i32)) -> bool {
        const EDGE_X: i32 = 24;
        const EDGE_DX: i32 = 60;
        const EDGE_DY: i32 = 40;
        if pressed {
            self.edge_p0 = p;
            self.edge_armed = self.sub_cur.is_some() && p.0 < EDGE_X && !self.any_sheet_open();
            return false;
        }
        if !self.edge_armed {
            return false;
        }
        self.edge_armed = false;
        let (dx, dy) = (p.0 - self.edge_p0.0, p.1 - self.edge_p0.1);
        dx >= EDGE_DX && dy < EDGE_DY && dy > -EDGE_DY
    }

    /// The gate in front of the touchscreen (ui_touch_filter): a dark screen takes no taps;
    /// one that went off by itself wakes on a double tap. Returns whether LVGL gets the press.
    pub fn touch_filter(&mut self, pressed: bool, x: i32, y: i32) -> bool {
        let lit = self.bl_is_on();
        let f = self.tgate.step(lit, self.auto_slept && self.autooff_ms != 0, pressed, x, y, lv::tick_get() as i64);
        if f & logic::TG_WAKE != 0 {
            self.src.bl_on();
            self.sync();
            self.auto_slept = false;
            self.wake_guard = false;
            // SAFETY: LVGL is initialised; NULL = every display
            unsafe { sys::lv_display_trigger_activity(std::ptr::null_mut()) };
        }
        f & logic::TG_FORWARD != 0
    }

    // ---- shared chrome ----
    #[allow(clippy::needless_range_loop)]
    fn build_tabbar(&mut self) {
        let w = W - 2 * MARGIN;
        let cw = (w - 8) / TABS as i32;
        let th = t();
        let tb = kit::bx(Obj::layer_top(), MARGIN, H - TAB_GAP - TAB_H, w, TAB_H, th.glass, 22);
        tb.set_style_border_width(1, 0);
        tb.set_style_border_color(0x000000, 0);
        tb.set_style_border_opa(th.rim_opa, 0);
        tb.set_style_shadow_width(16, 0);
        tb.set_style_shadow_offset_y(6, 0);
        tb.set_style_shadow_color(0x000000, 0);
        tb.set_style_shadow_opa(36, 0);
        tb.add_flag(lv::LV_OBJ_FLAG_CLICKABLE); // taps between cells don't fall through
        if th.hl_opa != 0 {
            let hl = kit::bx(tb, 20, 0, w - 40, 1, 0xffffff, 0);
            hl.set_style_bg_opa(th.hl_opa, 0);
        }
        self.tab_bar = tb;
        for i in 0..TABS {
            let cell = kit::bx(tb, 4 + i as i32 * cw, 4, cw, 36, th.acc_s, 18);
            cell.set_style_bg_opa(lv::OPA_TRANSP, 0);
            kit::tappable(cell, Some(tab_click_cb), i);
            let pill = kit::bx(cell, 0, 0, cw, 36, th.acc_s, 18);
            pill.set_style_border_width(1, 0);
            pill.set_style_border_color(0x000000, 0);
            pill.set_style_border_opa(th.rim_opa, 0);
            self.tab_pill[i] = pill;
            self.tabs[i] = kit::label(cell, uf().cj13, th.t2, 0, 0, tr(TAB_NAMES[i]));
            self.tabs[i].center();
        }
        self.update_tabs();
    }

    /// time · 5 signal dots (two rows of 5 in dual standby) · RAT · rate [tap: Mbps/MB/s] · ▲ [tap: alerts] · battery.
    fn build_statusbar(&mut self) {
        let th = t();
        let bar = kit::bx(Obj::layer_top(), 0, 0, W, BAR_H, th.bg, 0);
        self.top_time = kit::label(bar, uf().n15, th.t1, 12, 4, "--:--");
        for i in 0..5 {
            self.top_sig[i] = kit::dot(bar, 60 + i as i32 * 8, 11, 5, th.track);
        }
        // dual standby (iPhone-style two rows, flat): 4px dots, 6px pitch, rows at y 7 / 15
        for i in 0..10 {
            let (row, col) = (i as i32 / 5, i as i32 % 5);
            self.top_sig2[i] = kit::dot(bar, 60 + col * 6, 7 + row * 8, 4, th.track);
            self.c_sig2[i] = th.track;
            kit::show(self.top_sig2[i], false);
        }
        self.top_net = kit::label(bar, uf().n12, th.t1, 104, 6, "");
        self.top_net.set_width(TOP_LEFT_END - 104 - 2);
        self.top_net.set_long_mode(lv::LV_LABEL_LONG_MODE_CLIP);
        // Real hit containers live directly on layer_top: the 26 px visual bar must
        // not clip their 40 px touch height. Their horizontal bounds never overlap.
        self.top_rate_hit = kit::bx(Obj::layer_top(), 200, 0, 40, 40, th.bg, 0);
        self.top_rate_hit.set_style_bg_opa(lv::OPA_TRANSP, 0);
        kit::tappable(self.top_rate_hit, Some(speed_unit_cb), 0);
        self.top_updown = kit::label(self.top_rate_hit, uf().n12, th.t2, 0, 6, "");
        self.top_alert_hit = kit::bx(Obj::layer_top(), 246, 0, 40, 40, th.bg, 0);
        self.top_alert_hit.set_style_bg_opa(lv::OPA_TRANSP, 0);
        kit::tappable(self.top_alert_hit, Some(open_alerts_cb), 0);
        self.top_alert = kit::label(self.top_alert_hit, uf().cj12, th.warn_t, 0, 5, "▲");
        kit::show(self.top_alert_hit, false);
        self.top_batt = kit::battery(bar, TOP_BATT_XR, 6);
        kit::battery_set(&self.top_batt, 0, false);
        kit::show(self.top_batt.body, false); // no fake "0" before the first snapshot
        kit::show(self.top_batt.nub, false);
    }

    /// Place the rate and ▲ between the RAT and the battery.
    #[allow(clippy::too_many_arguments)]
    fn statusbar_layout(&mut self, full: &[u8], shrt: &[u8], down: &[u8], pct: i32, chg: bool, alert: bool, alert_col: u32) {
        let mut right = TOP_BATT_XR - kit::battery_w(pct, chg) - 6;
        kit::show(self.top_alert_hit, alert);
        if alert {
            kit::text_color(self.top_alert, alert_col);
            let aw = lv::text_w("▲".as_bytes(), uf().cj12);
            self.top_alert_hit.set_x(right - 40);
            self.top_alert.set_x((40 - aw) / 2);
            right -= 40;
        }
        let f = [full, shrt, down];
        let w = f.map(|s| lv::text_w(s, uf().n12));
        let k = logic::rate_pick(w, right - TOP_LEFT_END);
        if self.rate_shown != f[k] {
            self.rate_shown = f[k].to_vec();
            self.top_updown.set_text(f[k]);
        }
        let hit_w = w[k].max(40);
        self.top_rate_hit.set_x(right - hit_w);
        self.top_rate_hit.set_width(hit_w);
        self.top_updown.set_x(hit_w - w[k]);
    }

    fn build_banner(&mut self) {
        let th = t();
        let b = kit::bx(Obj::layer_top(), MARGIN, H - TAB_GAP - TAB_H - 8 - 30, CARD_W, 30, th.wash_b, 15);
        b.set_style_border_width(1, 0);
        b.set_style_border_color(th.bad_t, 0);
        b.set_style_border_opa(90, 0);
        self.banner_txt = kit::label(b, uf().cj13, th.bad_t, 0, 0, "");
        self.banner_txt.center();
        kit::show(b, false);
        self.banner = b;
    }

    /// Device-wide state (data service down / agent lost …), None = no banner.
    pub fn banner_set(&mut self, txt: Option<&[u8]>) {
        if self.banner.is_null() {
            return;
        }
        if let Some(txt) = txt
            && self.banner_shown != txt
        {
            self.banner_shown = txt.to_vec();
            self.banner_txt.set_text(txt);
        }
        kit::show(self.banner, txt.is_some());
    }

}

/// C atol: leading blanks, a sign, digits; 0 when there are none.
pub fn atol(s: &str) -> i64 {
    let t = s.trim_start_matches([' ', '\t', '\n', '\r']);
    let (neg, t) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let mut v: i64 = 0;
    for c in t.bytes().take_while(|c| c.is_ascii_digit()) {
        v = v.wrapping_mul(10).wrapping_add((c - b'0') as i64);
    }
    if neg { -v } else { v }
}

/// The page's scroll body (kit::scroll), or None for a page that doesn't scroll.
pub fn obj_scroller(t: Obj) -> Option<Obj> {
    (0..t.child_count() as i32).map(|i| t.child(i)).find(|c| c.has_flag(lv::LV_OBJ_FLAG_SCROLLABLE))
}

unsafe extern "C" fn tab_click_cb(e: *mut sys::lv_event_t) {
    let i = unsafe { lv::event_user(e) };
    with(|u| u.tab_click(i));
}

pub(crate) unsafe extern "C" fn tab_go_cb(e: *mut sys::lv_event_t) {
    let i = unsafe { lv::event_user(e) };
    with(|u| u.tab_go(i));
}

/// › rows and Home cards: open a subpage (user = Sub index).
pub(crate) unsafe extern "C" fn tile_click_cb(e: *mut sys::lv_event_t) {
    let i = unsafe { lv::event_user(e) };
    with(|u| u.sub_open(SUBS[i]));
}

unsafe extern "C" fn sub_back_cb(_e: *mut sys::lv_event_t) {
    with(|u| u.sub_back());
}

pub(crate) unsafe extern "C" fn open_alerts_cb(_e: *mut sys::lv_event_t) {
    with(|u| u.sub_open(Sub::Alerts));
}

unsafe extern "C" fn speed_unit_cb(_e: *mut sys::lv_event_t) {
    with(|u| {
        u.conf.speed_bits = !u.conf.speed_bits;
        u.conf.save();
    });
}

/// Build the whole screen (ui_create): settings and theme before the first object.
pub fn create(conf: Conf, launch: Launch, model: Rc<Model>, src: Box<dyn Source>, argv0: &str) {
    lang::set_en(conf.lang_en);
    let dark = conf.dark_now(model.now);
    theme::select(dark);
    // an in-process exec carries the live screen-off time; a fresh start takes the saved one
    let autooff_ms = if launch.autooff_ms >= 0 { launch.autooff_ms as u32 } else { logic::autooff_snap(conf.autooff_ms as i64) as u32 };
    let mut src = src;
    if launch.bright > 0 {
        src.bl_remember(launch.bright);
    } else if !src.bl_panel_lit(&model) && conf.bright > 0 {
        src.bl_remember(conf.bright); // started dark: wake to the saved level, not max
    }
    let (mut auto_slept, mut wake_guard) = (false, false);
    if launch.screen_off != 0 && !src.bl_panel_lit(&model) {
        auto_slept = launch.screen_off == 1 && autooff_ms != 0;
        wake_guard = true;
    }
    eprintln!(
        "ui: appearance={} dark={} lang={} tab={} screen_off={} exec_n={}",
        logic::appear_name(conf.appear),
        dark as i32,
        if conf.lang_en { "en" } else { "zh" },
        launch.tab,
        launch.screen_off,
        launch.hist.count
    );

    let scr = Obj::screen_active();
    scr.set_style_bg_color(t().bg, 0);
    scr.set_style_text_color(t().t1, 0);
    // fonts from the device (CJK) and the plugin's fonts/ dir (Nunito); the repo ships none
    theme::load();

    let mut u = Box::new(Ui {
        model,
        conf,
        dark,
        launch,
        autooff_ms,
        tv: Obj::NULL,
        tiles: [Obj::NULL; TABS],
        tabs: [Obj::NULL; TABS],
        tab_bar: Obj::NULL,
        tab_pill: [Obj::NULL; TABS],
        sub_layer: Obj::NULL,
        sub_title: Obj::NULL,
        sub_page: Vec::new(),
        sub_cur: None,
        sub_parent: None,
        top_time: Obj::NULL,
        top_net: Obj::NULL,
        top_updown: Obj::NULL,
        top_rate_hit: Obj::NULL,
        top_sig: [Obj::NULL; 5],
        top_sig2: [Obj::NULL; 10],
        sig_dual: None,
        top_alert: Obj::NULL,
        top_alert_hit: Obj::NULL,
        top_batt: Battery::default(),
        alert_st: 0,
        top_label: String::new(),
        rate_shown: Vec::new(),
        c_sig: [0; 5],
        c_sig2: [0; 10],
        c_pct: -1,
        c_chg: -1,
        banner: Obj::NULL,
        banner_txt: Obj::NULL,
        banner_shown: Vec::new(),
        tile_sub: vec![Obj::NULL; SUBS.len()],
        nav_sec: Obj::NULL,
        nav_card: Obj::NULL,
        home: home::Home::default(),
        aux: home::Aux::default(),
        cell: cellular::Cell::default(),
        lk: lock::Lock::default(),
        wifi: wifi::Wifi::default(),
        net: network::Net::default(),
        ex: exit::Exit::default(),
        sys: system::Sys::default(),
        charts: system::Charts::default(),
        op: op::Op::default(),
        pw: power::Power::default(),
        um: usbmode::Usb::default(),
        sms: sms::Sms::default(),
        es: esim::Esim::default(),
        ds: dualsim::DualSim::default(),
        st: speed::Speed::default(),
        ts: exit::TsPage::default(),
        sg: exit::SigPage::default(),
        cp: exit::ChillPage::default(),
        apn: apn::Apn::default(),
        dg: diagnose::Diag::default(),
        pl: diagnose::Place::default(),
        bench_timer: lv::Timer::default(),
        lang_frame: Obj::NULL,
        edge_p0: (0, 0),
        edge_armed: false,
        sim: refresh::Sim::default(),
        src,
        polled: Polled::default(),
        c_tile_on: [-1, -1],
        refreshes: 0,
        tgate: logic::TGate::default(),
        auto_slept,
        wake_guard,
        wake_idle_last: lv::tick_get(),
        key_timer: lv::Timer::default(),
        auto_suspended: false,
        argv0: argv0.to_string(),
        label_cache: HashMap::new(),
        site_cache: HashMap::new(),
    });

    let th = t();
    (u.tv, u.tiles) = tab_strip(scr, th.bg);
    u.build_home(u.tiles[TAB_HOME]);
    u.build_cellular(u.tiles[TAB_CELL]);
    u.build_wifi(u.tiles[TAB_WIFI]);
    u.build_exit(u.tiles[TAB_EXIT]);
    u.build_system(u.tiles[TAB_SYS]);
    u.tv.add_event_cb(tv_changed_cb, lv::LV_EVENT_VALUE_CHANGED, 0);

    // Subpage layer: a sibling of the tileview, created after it so it paints on top, below
    // lv_layer_top()'s status bar and tab bar.
    let sl = Obj::create(scr);
    sl.remove_style_all();
    sl.remove_flag(lv::LV_OBJ_FLAG_SCROLLABLE);
    sl.set_size(W, H - BAR_H);
    sl.align(lv::LV_ALIGN_TOP_LEFT, 0, BAR_H);
    sl.set_style_bg_color(th.bg, 0);
    sl.set_style_bg_opa(lv::OPA_COVER, 0);
    u.sub_layer = sl;
    {
        // solid header (content scrolls under it), 1 px separator, round ‹ and the title
        let hdr = kit::bx(sl, 0, 0, W, SUB_HDR, th.bg, 0);
        kit::bx(hdr, 0, SUB_HDR - 1, W, 1, th.sep, 0);
        let bk = kit::bx(hdr, 10, 3, 30, 30, th.glass, 15);
        bk.set_style_border_width(1, 0);
        bk.set_style_border_color(0x000000, 0);
        bk.set_style_border_opa(th.rim_opa, 0);
        let c = kit::label(bk, uf().cj17b, th.t1, 0, 0, "‹");
        c.align(lv::LV_ALIGN_CENTER, -1, -1);
        kit::tappable(bk, Some(sub_back_cb), 0);
        bk.set_ext_click_area(10);
        u.sub_title = kit::label(hdr, uf().cj17b, th.t1, 48, 6, "");
    }
    for _ in SUBS {
        let pg = Obj::create(sl);
        pg.remove_style_all();
        pg.remove_flag(lv::LV_OBJ_FLAG_SCROLLABLE);
        pg.set_size(W, H - BAR_H - SUB_HDR);
        pg.align(lv::LV_ALIGN_TOP_LEFT, 0, SUB_HDR);
        pg.add_flag(lv::LV_OBJ_FLAG_HIDDEN);
        u.sub_page.push(pg);
    }
    // the subpages, in C's order
    let pg = |s: Sub| u.sub_page[s as usize];
    let (p_sms, p_cell, p_lock, p_speed, p_ts) = (pg(Sub::Sms), pg(Sub::Cell), pg(Sub::Lock), pg(Sub::Speed), pg(Sub::Ts));
    u.build_sub_sms(p_sms);
    u.build_sub_cell(p_cell);
    u.build_sub_lock(p_lock);
    u.build_sub_speed(p_speed);
    u.build_sub_ts(p_ts);
    let (p_chill, p_nodes, p_pairs) = (u.sub_page[Sub::Chill as usize], u.sub_page[Sub::ChillNodes as usize], u.sub_page[Sub::ChillPairs as usize]);
    u.build_sub_chill(p_chill);
    u.build_sub_chill_nodes(p_nodes);
    u.build_sub_chill_pairs(p_pairs);
    let pg = |s: Sub| u.sub_page[s as usize];
    let (p_esim, p_perf, p_smsd, p_al, p_ald) = (pg(Sub::Esim), pg(Sub::Perf), pg(Sub::SmsDetail), pg(Sub::Alerts), pg(Sub::AlertDetail));
    let (p_apn, p_net, p_diag, p_place) = (pg(Sub::Apn), pg(Sub::Net), pg(Sub::Diag), pg(Sub::Place));
    let (p_op, p_log, p_logd) = (pg(Sub::Op), pg(Sub::Log), pg(Sub::LogDetail));
    u.build_sub_esim(p_esim);
    u.build_sub_perf(p_perf);
    u.build_sub_sms_detail(p_smsd);
    u.build_sub_alerts(p_al);
    u.build_sub_alert_detail(p_ald);
    u.build_sub_apn(p_apn);
    // also fills 情景 and hangs cards on 小区信息 / 出口
    u.build_sub_net(p_net);
    u.build_sub_diag(p_diag);
    u.build_sub_place(p_place);
    u.build_sub_op(p_op);
    u.build_sub_log(p_log);
    u.build_sub_log_detail(p_logd);
    u.sub_close();

    // lay out first: switching tile on tiles without a size leaves stale scroll state
    scr.update_layout();
    let tab = if u.launch.tab > 0 { u.launch.tab as usize } else { 0 };
    u.tv.set_tile_by_index(tab as u32, 0);
    if u.launch.scroll_y > 0
        && let Some(sc) = obj_scroller(u.tiles[tab])
    {
        sc.update_layout();
        sc.scroll_to_y(u.launch.scroll_y);
    }

    // the speed page's animation ticks only while that page is up
    u.bench_timer = lv::Timer::create(speed::bench_cb, 1);
    u.bench_timer.pause();

    // shared chrome (top layer)
    u.build_statusbar();
    u.build_tabbar();
    u.build_banner();
    u.build_op_row();
    // SAFETY: LVGL is initialised; the input devices outlive the screen
    unsafe {
        let mut ind = sys::lv_indev_get_next(std::ptr::null_mut());
        while !ind.is_null() {
            sys::lv_indev_add_event_cb(ind, Some(edge_back_cb), sys::lv_event_code_t::LV_EVENT_PRESSED, std::ptr::null_mut());
            sys::lv_indev_add_event_cb(ind, Some(edge_back_cb), sys::lv_event_code_t::LV_EVENT_RELEASED, std::ptr::null_mut());
            ind = sys::lv_indev_get_next(ind);
        }
    }

    UI.with(|c| *c.borrow_mut() = Some(u));
    // SAFETY: LVGL keeps the timer; the callback reaches the screen through with()
    unsafe { sys::lv_timer_create(Some(refresh_timer_cb), 1000, std::ptr::null_mut()) };
    with(|u| u.refresh());
    eprintln!("ui: first refresh done");
    // a startup breadcrumb (and the mark tools/luyaz checks a touch build for)
    eprintln!("ui: pages+chrome built");
    with(|u| {
        u.build_power_menu();
        u.build_usbmode();
        u.build_exit_menu();
        u.key_timer = lv::Timer::create(power::key_poll_cb, 50);
    });
}

/// The tabs: one row of full-size tiles swiped sideways, below the status bar (which lives on
/// lv_layer_top() and is built later). Every tile gets the page colour itself: with no LVGL
/// theme loaded a tile would otherwise be see-through.
fn tab_strip(scr: Obj, bg: u32) -> (Obj, [Obj; TABS]) {
    let strip = Obj::tileview(scr);
    strip.set_size(W, H - BAR_H);
    strip.align(lv::LV_ALIGN_TOP_MID, 0, BAR_H);
    strip.set_scrollbar_mode(sys::lv_scrollbar_mode_t::LV_SCROLLBAR_MODE_OFF);
    let paint = |o: Obj| {
        o.set_style_bg_color(bg, 0);
        o.set_style_bg_opa(lv::OPA_COVER, 0);
    };
    paint(strip);
    let tiles = std::array::from_fn(|col| {
        let tile = strip.add_tile(col as u8, 0, sys::lv_dir_t::LV_DIR_HOR);
        paint(tile);
        tile
    });
    (strip, tiles)
}

unsafe extern "C" fn tv_changed_cb(_e: *mut sys::lv_event_t) {
    // a tile set from inside the screen's own code (tab_go …) comes here while it runs: that
    // caller updates the tabs itself
    UI.with(|c| {
        if let Ok(mut g) = c.try_borrow_mut()
            && let Some(u) = g.as_mut()
        {
            u.update_tabs();
            u.bench_gate();
        }
    });
}

unsafe extern "C" fn edge_back_cb(e: *mut sys::lv_event_t) {
    // SAFETY: an indev event: the active input device is the one that sent it
    unsafe {
        let ind = sys::lv_indev_active();
        if ind.is_null() || sys::lv_indev_get_type(ind) != sys::lv_indev_type_t::LV_INDEV_TYPE_POINTER {
            return;
        }
        let mut p = sys::lv_point_t { x: 0, y: 0 };
        sys::lv_indev_get_point(ind, &mut p);
        let pressed = sys::lv_event_get_code(e) == sys::lv_event_code_t::LV_EVENT_PRESSED;
        if with(|u| u.edge_back(pressed, (p.x, p.y))) {
            sys::lv_indev_reset(ind, std::ptr::null_mut());
            with(|u| u.sub_back());
        }
    }
}

unsafe extern "C" fn refresh_timer_cb(_t: *mut sys::lv_timer_t) {
    with(|u| u.refresh());
}

/// The data source pushes a new model; the next refresh draws it.
pub fn set_model(m: Rc<Model>) {
    with(|u| u.model = m);
}
