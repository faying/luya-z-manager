//! 系统: 屏幕 (language, brightness, screen-off, appearance), 电池与负载,
//! the charts, 设备, 开关, 插入手机等设备时, 调试 and 切换到原厂界面; and the charts themselves.
//! The net-speed chart is Home's (2026-09-25); CPU / 内存 / 电池 stay here. 60 points, one per
//! 5 s (the average of five 1 s readings) = the last 5 minutes.
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use super::cellular::nz;
use super::{Sub, Ui, VIEW_H};
use crate::cf;
use crate::kit::{self, BtnKind, Seg, CARD_W, MARGIN, PAD, ROW_H, TAB_PAD};
use crate::lang::{self, n_, tr};
use crate::logic::{self, Appear};
use crate::lv::{self, sys, Obj, Series};
use crate::theme::{t, uf};
use crate::trc;

pub const CHART_PTS: u32 = 60;
pub const CHART_STEP: i32 = 5;
/// points before a curve is worth showing (1 min)
pub const CHART_READY: i32 = 12;
/// The auto-off choices in segment order, in ms; 0 keeps the screen on.
const AUTOOFF_CHOICES: [u32; 3] = [0, 30_000, 120_000];
const AP_VAL: [Appear; 3] = [Appear::Light, Appear::Dark, Appear::Auto];
const LANG_NOTE: &str = "告警短信也用这个语言 · Alert SMS use this too";

#[derive(Default, Clone)]
pub struct Charts {
    pub net_card: Obj,
    net: Obj,
    rx: Series,
    tx: Series,
    net_dn: Obj,
    net_up: Obj,
    cpu: Obj,
    cpu_s: Series,
    cpu_v: Obj,
    cpu_t: Obj,
    mem: Obj,
    mem_s: Series,
    mem_v: Obj,
    mem_sub: Obj,
    bat: Obj,
    bat_s: Series,
    bat_v: Obj,
    bat_sub: Obj,
    /// 正在收集 notes: net, cpu, mem, battery
    wait: [Obj; 4],
    acc_cpu: i64,
    acc_mem: i64,
    acc_rx: i64,
    acc_tx: i64,
    acc_bat: i64,
    acc_n: i32,
    pts: i32,
}

#[derive(Default, Clone)]
pub struct Sys {
    pub scroll: Obj,
    lang_seg: Seg,
    pub lang_note: Obj,
    bright: Obj,
    bright_v: Obj,
    off_seg: Seg,
    ap_seg: Seg,
    bat: Obj,
    est: Obj,
    chg: Obj,
    cpu: Obj,
    mem: Obj,
    up: Obj,
    ver: Obj,
    imei: Obj,
    usb: Obj,
    fw: Obj,
    /// 组件版本: core, lweb, sentinel, touch, scripts (VER_NAMES), then 检查更新
    vers: [Obj; 5],
    vchk: Obj,
    pub health: Obj,
    health_col: Option<u32>,
    pub dps_sw: Obj,
    pub dps_st: Obj,
    pub speedunit_sw: Obj,
    pub speedunit_st: Obj,
    vendor_btn: Obj,
    vendor_lbl: Obj,
    /// 切换到原厂界面 tapped once (5 s)
    pub vendor_arm: u32,
    /// puts the 语言 row's line back after a note
    lang_note_t: lv::Timer,
}

/// The components in the core's order, with their row names.
const VER_NAMES: [(&str, &str); 5] = [("core", n_("核心")), ("lweb", n_("管理网页")), ("sentinel", n_("哨兵")), ("touch", n_("触屏界面")), ("scripts", n_("设备脚本"))];

/// 「1.0.0」, 「1.0.0 → 1.1.0」 when there is a newer one, 「未编号」 for a build before 1.0.0.
pub fn ver_line(r: &ui_model::ver_row_t) -> String {
    let cur = if r.installed.is_empty() { tr("未编号") } else { r.installed.as_str() };
    if r.update != 0 && !r.latest.is_empty() { format!("{cur} → {}", r.latest) } else { cur.to_string() }
}

unsafe extern "C" fn versions_check_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| u.src.versions_check());
}

/// Throughput on a 0..100 log scale: 0 B/s → 0, 1 KB/s → 25, 1 MB/s → 60, 100 MB/s → 100.
pub fn rate_scale(bps: i64) -> i32 {
    if bps <= 0 {
        return 0;
    }
    let v = (bps as f64).log10() * 100.0 / 8.0; // 10^8 B/s == full scale
    v.clamp(0.0, 100.0) as i32
}

/// A rate in bytes per second: whole B/s below a kilobyte, else KB/s or MB/s to one decimal.
pub fn fmt_rate(bps: i64) -> Vec<u8> {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * KB;
    let r = bps as f64;
    match r {
        _ if r >= MB => cf!("%.1f MB/s", r / MB),
        _ if r >= KB => cf!("%.1f KB/s", r / KB),
        _ => cf!("%ld B/s", bps),
    }
}

/// Card-coloured backing so the chart's grid line does not run through the text.
fn wait_backed(l: Obj) -> Obj {
    l.set_style_bg_color(t().card, 0);
    l.set_style_bg_opa(lv::OPA_COVER, 0);
    l.set_style_pad_hor(4, 0);
    l
}

fn chart_wait(card: Obj, x: i32, y: i32) -> Obj {
    wait_backed(kit::label(card, uf().cj12, t().t3, x, y, tr("正在收集 · 1 分钟后出现曲线")))
}

/// Every step of a drag: the panel follows the finger.
unsafe extern "C" fn bright_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let level = unsafe { lv::event_target(e) }.slider_value();
    super::with(|u| u.bright_apply(level));
}

/// Saved on release, not per step: the slider fires dozens of changes per drag.
unsafe extern "C" fn bright_save_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let v = unsafe { lv::event_target(e) }.slider_value();
    super::with(|u| {
        u.conf.bright = v;
        u.conf.save();
    });
}

unsafe extern "C" fn offsel_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let choice = unsafe { lv::event_user(e) };
    super::with(|u| u.autooff_pick(choice));
}

unsafe extern "C" fn dps_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let on = unsafe { lv::event_target(e) }.has_state(lv::LV_STATE_CHECKED);
    super::with(|u| {
        u.data_control(
            "power.direct_supply.set",
            if on { "{\"enabled\":true}" } else { "{\"enabled\":false}" },
            Some(if on { "enabled=1" } else { "enabled=0" }),
        );
        u.aux.dps = on as i32;
        u.aux.hold_dps = nz(lv::tick_get());
    });
}

/// The same setting the status-bar tap flips, with a label: both write conf.speed_bits.
unsafe extern "C" fn speedunit_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let on = unsafe { lv::event_target(e) }.has_state(lv::LV_STATE_CHECKED);
    super::with(|u| {
        u.conf.speed_bits = on;
        u.conf.save();
    });
}

/// Switch to the vendor UI: two taps (the vendor UI has no button back here).
unsafe extern "C" fn act_switch_vendor(_e: *mut sys::lv_event_t) {
    super::with(|u| {
        let now = lv::tick_get();
        let arm = u.sys.vendor_arm;
        if arm != 0 && now.wrapping_sub(arm) < 5000 {
            u.sys.vendor_lbl.set_text(tr("切换中…"));
            // the screen's owner stops us and starts the vendor UI (and does not count our
            // exit as a crash); nobody listening: do it ourselves
            if !u.src.request_vendor() {
                u.src.vendor_fallback();
            }
            return;
        }
        u.sys.vendor_arm = nz(now);
        kit::button_kind(u.sys.vendor_btn, u.sys.vendor_lbl, BtnKind::Armed);
        u.sys.vendor_lbl.set_text(tr("再按一次确认切换"));
    });
}

unsafe extern "C" fn appearance_btn_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let i = unsafe { lv::event_user(e) };
    super::with(|u| {
        u.sys.vendor_arm = 0; // a pending 切换到原厂界面 does not survive a theme switch
        u.appearance_set(AP_VAL[i]);
    });
}

unsafe extern "C" fn lang_btn_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let i = unsafe { lv::event_user(e) };
    super::with(|u| u.lang_set(i != 0));
}

unsafe extern "C" fn lang_note_reset_cb(_t: *mut sys::lv_timer_t) {
    super::with(|u| {
        u.sys.lang_note_t = lv::Timer::default();
        u.lang_note_reset();
    });
}

impl Ui {
    /// Something a switch would cut off mid-way: a running speed test, a delay test, an eSIM
    /// switch waiting for its answer.
    fn ui_busy(&self) -> bool {
        let m = &self.model;
        if m.st_running != 0 || m.chill_delay_pending != 0 {
            return true;
        }
        m.esim_profiles.iter().any(|p| p.going != 0)
    }

    /// Exec a fresh copy of this program on this tab and scroll position, which
    /// takes the new theme or language before its first object. Returns only if that failed.
    fn theme_exec(&mut self, automatic: bool) -> i32 {
        let mut l = self.launch;
        l.tab = self.cur_tab() as i32;
        l.scroll_y = super::obj_scroller(self.tiles[l.tab as usize]).map_or(0, |sc| sc.scroll_y().max(0));
        l.screen_off = if self.src.bl_panel_lit(&self.model) {
            0
        } else if self.auto_slept {
            1
        } else {
            2
        };
        l.autooff_ms = self.autooff_ms as i32;
        l.bright = self.src.bl_get(&self.model);
        if automatic {
            l.hist = logic::exec_next(Some(&self.launch.hist), self.src.boot_s(&self.model));
        }
        // the new process sets its own pace; don't leave core slow if it never does
        self.src.data_set_pace(true);
        let r = self.src.exec(&l);
        self.sync();
        r
    }

    /// appearance=auto: every second from the refresh (which runs while the screen is dark).
    pub(super) fn appearance_tick(&mut self) {
        if self.conf.appear != Appear::Auto || self.auto_suspended {
            return;
        }
        let wall = self.now();
        let min = if logic::clock_sane(wall) {
            let tm = crate::clock::local(wall);
            tm.hour * 60 + tm.min
        } else {
            -1
        };
        let i = logic::AutoIn {
            appear: self.conf.appear,
            cur_dark: self.dark,
            wall_s: wall,
            now_min: min,
            from_min: logic::hhmm_parse(Some(&self.conf.dark_from)),
            to_min: logic::hhmm_parse(Some(&self.conf.dark_to)),
            screen_off: !self.src.bl_panel_lit(&self.model),
            always_on: self.autooff_ms == 0,
            idle_ms: lv::inactive_ms() as i64,
            busy: self.ui_busy(),
            suspended: self.auto_suspended,
            hist: self.launch.hist,
            now_boot_s: self.src.boot_s(&self.model),
        };
        match logic::auto_decide(&i) {
            logic::Auto::Exec => {
                if self.theme_exec(true) != 0 {
                    // e.g. the binary was replaced on disk: retrying every second would only fill
                    // the log; next boot (or a manual switch) again
                    self.auto_suspended = true;
                    eprintln!("ui: automatic appearance switch off until restart (exec failed)");
                }
            }
            logic::Auto::Suspend => {
                self.auto_suspended = true;
                eprintln!(
                    "ui: automatic appearance switch paused until reboot: {} switches within {} s",
                    self.launch.hist.count,
                    logic::EXEC_WINDOW_S
                );
                self.src.alert_theme_paused();
            }
            _ => {}
        }
    }

    /// 系统 → 外观. Manual switches are never rate-limited.
    fn appearance_set(&mut self, a: Appear) {
        let old = self.conf.appear;
        if a == old {
            return;
        }
        let mut c = self.conf.clone();
        c.appear = a;
        let dark = c.dark_now(self.now());
        if dark != self.dark && self.ui_busy() {
            // the exec would cut a speed test or an eSIM switch short (L2 R12)
            self.appearance_ui_sync();
            self.lang_ui_note(tr("测速或切卡进行中，完成后再换外观"), true);
            return;
        }
        self.conf.appear = a;
        self.conf.save();
        if dark != self.dark && self.theme_exec(false) != 0 {
            self.conf.appear = old; // stay as we are, and say so in the file too
            self.conf.save();
        }
        self.appearance_ui_sync();
    }

    /// The full-screen 「switching」 frame, in both languages whichever way.
    pub fn lang_frame_show(&mut self, to_en: bool) {
        if self.lang_frame.is_null() {
            let th = t();
            let fr = Obj::create(Obj::layer_top());
            fr.remove_style_all();
            fr.set_size(kit::W, kit::H);
            fr.set_style_bg_color(th.bg, 0);
            fr.set_style_bg_opa(lv::OPA_COVER, 0);
            fr.add_flag(lv::LV_OBJ_FLAG_CLICKABLE); // swallows taps while it is up
            for i in 0..2 {
                let l = Obj::label(fr);
                l.set_style_text_font(uf().cj20b, 0);
                l.set_style_text_color(th.t1, 0);
                l.align(lv::LV_ALIGN_CENTER, 0, if i == 1 { 16 } else { -16 });
            }
            self.lang_frame = fr;
        }
        let fr = self.lang_frame;
        fr.child(0).set_text(if to_en { "正在切换到 English…" } else { "正在切换到中文…" });
        fr.child(1).set_text(if to_en { "Switching to English…" } else { "Switching to 中文…" });
        fr.remove_flag(lv::LV_OBJ_FLAG_HIDDEN);
    }

    pub fn lang_frame_hide(&mut self) {
        if !self.lang_frame.is_null() {
            self.lang_frame.add_flag(lv::LV_OBJ_FLAG_HIDDEN);
        }
    }

    /// 系统 → 语言 (L2): the same exec as a theme switch, back on this tab. First a full-screen
    /// switching frame; busy: no switch, say why; the exec failed: the setting goes back, the
    /// frame goes, the row says so.
    fn lang_set(&mut self, en: bool) {
        if en == self.conf.lang_en {
            return;
        }
        if self.ui_busy() {
            let cur = self.conf.lang_en as i32;
            kit::seg_set(&mut self.sys.lang_seg, cur);
            self.lang_ui_note("测速或切卡中，稍后再切 · Busy, try later", true); // one 12 px line
            return;
        }
        self.sys.vendor_arm = 0;
        kit::seg_set(&mut self.sys.lang_seg, en as i32);
        self.conf.lang_en = en;
        self.conf.save(); // the alert SMS follow from the next one
        self.lang_frame_show(en);
        lv::refr_now();
        // held a moment so the frame is seen (the exec drops the panel until the new process
        // draws)
        self.src.lang_hold();
        if self.theme_exec(false) != 0 {
            self.conf.lang_en = !en;
            self.conf.save();
            self.lang_frame_hide();
            let cur = self.conf.lang_en as i32;
            kit::seg_set(&mut self.sys.lang_seg, cur);
            self.lang_ui_note("没切成 · Couldn't switch", true);
        }
    }

    /// The 语言 row's small line: why a switch did not happen, for 5 s.
    fn lang_ui_note(&mut self, msg: &str, warn: bool) {
        let l = self.sys.lang_note;
        if l.is_null() {
            return;
        }
        l.set_text(msg);
        kit::text_color(l, if warn { t().warn_t } else { t().t3 });
        if !self.sys.lang_note_t.is_null() {
            self.sys.lang_note_t.reset();
        } else {
            let tm = lv::Timer::create(lang_note_reset_cb, 5000);
            tm.set_repeat_count(1);
            self.sys.lang_note_t = tm;
        }
    }

    pub fn lang_note_reset(&mut self) {
        self.sys.lang_note_t = lv::Timer::default();
        let l = self.sys.lang_note;
        if l.is_null() {
            return;
        }
        l.set_text(LANG_NOTE);
        kit::text_color(l, t().t3);
    }

    fn bright_apply(&mut self, level: i32) {
        self.src.bl_set(level);
        self.sync();
        self.bright_label();
    }

    /// A new auto-off delay: counts from now (a dimmed screen wakes up as it was) and is
    /// written to the config only when it differs, so it survives a restart.
    fn autooff_pick(&mut self, choice: usize) {
        let ms = AUTOOFF_CHOICES[choice];
        kit::seg_set(&mut self.sys.off_seg, choice as i32);
        self.autooff_ms = ms;
        self.auto_slept = false;
        let saved = ms as i32;
        if self.conf.autooff_ms == saved {
            return;
        }
        self.conf.autooff_ms = saved;
        self.conf.save();
    }

    fn bright_label(&mut self) {
        let mx = self.src.bl_max(&self.model);
        let max = if mx > 0 { mx } else { 255 };
        let s = &self.sys;
        s.bright_v.set_text(cf!("%d%%", s.bright.slider_value() * 100 / max));
    }

    fn appearance_ui_sync(&mut self) {
        if self.sys.ap_seg.obj.is_null() {
            return;
        }
        for (i, &a) in AP_VAL.iter().enumerate() {
            if a == self.conf.appear {
                kit::seg_set(&mut self.sys.ap_seg, i as i32);
            }
        }
    }

    pub(super) fn build_system(&mut self, tile: Obj) {
        let th = t();
        let f = uf();
        let off_lbl: [&'static str; 3] = [n_("常亮"), n_("30秒"), n_("2分钟")];
        let ap_lbl: [&'static str; 3] = [n_("浅色"), n_("深色"), n_("自动")];
        let mut y = 4;
        let sc = kit::scroll(tile, 0, VIEW_H, 1000);
        self.sys.scroll = sc;

        kit::section(sc, y, tr("屏幕"));
        y += 20;
        let card = kit::card(sc, MARGIN, y, CARD_W, 60 + 120);
        // 语言 / Language: each in its own language (whoever switched by mistake finds the way
        // back)
        kit::label(card, f.cj14, th.t2, PAD, 11, "语言 / Language");
        self.sys.lang_seg = kit::seg(card, 166, 6, 120, &["中文", "English"], Some(lang_btn_cb));
        kit::seg_set(&mut self.sys.lang_seg, lang::is_en() as i32);
        // own line under the segment (it ends at y 36)
        self.sys.lang_note = kit::label_w(card, f.cj12, th.t3, PAD, 39, CARD_W - 2 * PAD, false, LANG_NOTE);
        kit::sep(card, 60);
        // the rows below keep their offsets
        let c = kit::bx(card, 0, 60, CARD_W, 120, th.card, 0);
        c.set_style_bg_opa(lv::OPA_TRANSP, 0);
        kit::label(c, f.cj14, th.t2, PAD, 11, tr("亮度"));
        // "Brightness" is wider than 亮度: in English the slider starts later, same right end
        let sl = if lang::is_en() { kit::slider(c, 96, 17, 134) } else { kit::slider(c, 58, 17, 172) };
        let (bl_max, bl_get) = (self.src.bl_max(&self.model), self.src.bl_get(&self.model));
        // floor 10: a darker panel cannot be read; an unreadable level opens at full
        sl.slider_set_range(10, bl_max);
        sl.slider_set_value(Some(bl_get).filter(|&lvl| lvl > 0).unwrap_or(bl_max));
        sl.add_event_cb(bright_cb, lv::LV_EVENT_VALUE_CHANGED, 0);
        sl.add_event_cb(bright_save_cb, lv::LV_EVENT_RELEASED, 0);
        self.sys.bright = sl;
        self.sys.bright_v = kit::label_r(c, f.n15, th.t2, CARD_W - PAD, 10, "");
        self.bright_label();
        kit::sep(c, 40);
        kit::label(c, f.cj14, th.t2, PAD, 51, tr("自动息屏"));
        self.sys.off_seg = kit::seg(c, 116, 45, 170, &off_lbl, Some(offsel_cb));
        kit::sep(c, 80);
        kit::label(c, f.cj14, th.t2, PAD, 91, tr("外观"));
        self.sys.ap_seg = kit::seg(c, 116, 85, 170, &ap_lbl, Some(appearance_btn_cb));
        let sel = AUTOOFF_CHOICES.iter().rposition(|&ms| ms == self.autooff_ms).unwrap_or(0);
        kit::seg_set(&mut self.sys.off_seg, sel as i32);
        self.appearance_ui_sync();
        y += 60 + 120 + 10;

        kit::section(sc, y, tr("电池与负载"));
        y += 20;
        // 预估: the core's battery estimate (the same as the web page's)
        let c = kit::card(sc, MARGIN, y, CARD_W, 6 * ROW_H);
        const LOAD: [&str; 6] = [n_("电池"), n_("预估"), n_("充电器"), "CPU", n_("内存"), n_("运行")];
        let v: Vec<Obj> = (0..6).map(|i| kit::row(c, i as i32 * ROW_H, tr(LOAD[i]), i == 0)).collect();
        let s = &mut self.sys;
        (s.bat, s.est, s.chg, s.cpu, s.mem, s.up) = (v[0], v[1], v[2], v[3], v[4], v[5]);
        s.est.set_style_text_font(f.cj14, 0);
        y += 6 * ROW_H + 10;

        kit::section(sc, y, tr("近 5 分钟"));
        y += 20;
        y += self.build_charts(sc, y) + 10;

        kit::section(sc, y, tr("设备"));
        y += 20;
        let c = kit::card(sc, MARGIN, y, CARD_W, 6 * ROW_H);
        let s = &mut self.sys;
        s.ver = kit::row(c, 0, tr("版本"), true);
        s.ver.set_style_text_font(f.n12, 0);
        s.imei = kit::row(c, ROW_H, "IMEI", false);
        s.usb = kit::row(c, 2 * ROW_H, "USB", false);
        s.usb.set_style_text_font(f.cj14, 0);
        s.fw = kit::row(c, 3 * ROW_H, tr("固件"), false);
        s.fw.set_style_text_font(f.n12, 0); // English key is "Build": the OpenWrt string fits whole
        s.fw.set_y(3 * ROW_H + 12);
        // 健康: the device check's counts; the whole row opens 告警
        s.health = kit::row_nav(c, 4 * ROW_H, tr("健康"), false, Some(super::open_alerts_cb), 0);
        // E4 DD5: who changed what, when
        self.tile_sub[Sub::Log as usize] = kit::row_nav(c, 5 * ROW_H, tr("改动记录"), false, Some(super::tile_click_cb), Sub::Log as usize);
        y += 6 * ROW_H + 10;

        // component versions (docs/designs/component-versions.md); the last row asks the core to
        // look at the public repo now
        kit::section(sc, y, tr("组件版本"));
        y += 20;
        let c = kit::card(sc, MARGIN, y, CARD_W, 6 * ROW_H);
        let s = &mut self.sys;
        for (i, (_, key)) in VER_NAMES.iter().enumerate() {
            s.vers[i] = kit::row(c, i as i32 * ROW_H, tr(key), i == 0);
            s.vers[i].set_style_text_font(f.cj14, 0);
        }
        s.vchk = kit::row_nav(c, 5 * ROW_H, tr("检查更新"), false, Some(versions_check_cb), 0);
        s.vchk.set_style_text_font(f.cj14, 0);
        y += 6 * ROW_H + 6;
        // people sell devices with Luya-Z installed: say plainly that it is free
        let disclosure = kit::label_w(sc, f.cj12, th.t3, MARGIN + PAD, y, CARD_W - 2 * PAD, true, tr("Luya-Z 免费提供。付钱买了装好的设备，费用和售后归卖家，与 Luya-Z 无关。"));
        disclosure.update_layout();
        y += disclosure.height() + 12;

        kit::section(sc, y, tr("开关"));
        y += 20;
        let c = kit::card(sc, MARGIN, y, CARD_W, 2 * ROW_H);
        let s = &mut self.sys;
        kit::label(c, f.cj14, th.t2, PAD, 11, tr("停止充电"));
        s.dps_st = kit::label_r(c, f.cj12, th.t3, CARD_W - PAD - 52, 13, "");
        s.dps_sw = kit::toggle(c, CARD_W - PAD, 7, Some(dps_cb), 0);
        kit::sep(c, ROW_H);
        kit::label(c, f.cj14, th.t2, PAD, ROW_H + 11, tr("状态栏网速用 Mbps"));
        s.speedunit_st = kit::label_r(c, f.n12, th.t3, CARD_W - PAD - 52, ROW_H + 13, "");
        s.speedunit_sw = kit::toggle(c, CARD_W - PAD, ROW_H + 7, Some(speedunit_cb), 0);
        y += 2 * ROW_H + 10;

        kit::section(sc, y, tr("插入手机等设备时"));
        y += 20;
        y += self.build_usbmode_card(sc, y) + 10;

        kit::section(sc, y, tr("调试"));
        y += 20;
        let c = kit::card(sc, MARGIN, y, CARD_W, ROW_H);
        let perf = kit::row_nav(c, 0, tr("性能测试"), true, Some(super::tile_click_cb), Sub::Perf as usize);
        perf.set_text(tr("调试页"));
        kit::text_color(perf, th.t3);
        self.tile_sub[Sub::Perf as usize] = perf;
        y += ROW_H + 10;

        kit::section(sc, y, tr("系统"));
        y += 20;
        let c = kit::card(sc, MARGIN, y, CARD_W, 100);
        (self.sys.vendor_btn, self.sys.vendor_lbl) =
            kit::button(c, PAD, 12, CARD_W - 2 * PAD, 40, tr("切换到原厂界面"), BtnKind::Plain, Some(act_switch_vendor), 0);
        kit::label_w(c, f.cj12, th.t3, PAD, 62, CARD_W - 2 * PAD, true, tr("电源键：短按 亮屏/息屏  长按 电源菜单\n回到这里：长按屏幕右下角 3 秒"));
        y += 100;
        kit::scroll_extent(sc, y + TAB_PAD);
        sc.scroll_to_y(0);
    }

    /// 原「图表」标签 (2026-09-25 into 系统, under 电池与负载). y0 = the first card's place;
    /// returns the height taken. The net-speed card goes on Home.
    fn build_charts(&mut self, p: Obj, y0: i32) -> i32 {
        let th = t();
        let f = uf();
        // 网速: log scale (bytes/s spans five orders of magnitude here), two lines told apart
        // by colour and dash, legend in text colours
        let c = kit::card(self.home.scroll, MARGIN, 0, CARD_W, 116);
        kit::label(c, f.cj12, th.t3, 12, 9, tr("网速 · 对数刻度"));
        let ch = &mut self.charts;
        ch.net_card = c;
        ch.net_up = kit::label_r(c, f.n12, th.warn_t, 288, 8, "");
        ch.net_dn = kit::label_r(c, f.n12, th.acc_t, 200, 8, "");
        (ch.net, ch.rx, ch.tx) = kit::chart(c, 44, 30, 244, 60, CHART_PTS, th.blue, th.orange);
        // rate_scale: 10^8 B/s = 100 → 10 MB/s at 87.5, 100 KB/s at 62.5. The axis shows
        // 50–100 (~30 KB/s to 100 MB/s): below that is idle chatter.
        ch.net.chart_set_axis_range(sys::lv_chart_axis_t::LV_CHART_AXIS_PRIMARY_Y, 50, 100);
        kit::label(c, f.n11, th.t3, 12, 30 + 60 * 25 / 100 - 7, "10M");
        kit::label(c, f.n11, th.t3, 12, 30 + 60 * 75 / 100 - 7, "100K");
        kit::label_r(c, f.cj12, th.t3, 288, 94, tr("近 5 分钟"));
        ch.wait[0] = chart_wait(c, 56, 52);

        let a = kit::card(p, MARGIN, y0, 145, 110);
        let m = kit::card(p, MARGIN + 155, y0, 145, 110);
        kit::label(a, f.cj12, th.t3, 12, 9, "CPU");
        ch.cpu_t = kit::label_r(a, f.cj12, th.t2, 133, 9, "");
        ch.cpu_v = kit::label(a, f.n20, th.t1, 12, 26, "");
        (ch.cpu, ch.cpu_s, _) = kit::chart(a, 12, 60, 121, 40, CHART_PTS, th.blue, 0);
        ch.wait[1] = wait_backed(kit::label(a, f.cj12, th.t3, 8, 72, tr("正在收集…")));
        kit::label(m, f.cj12, th.t3, 12, 9, trc!("图表", "内存")); // 145 px card: RAM in English
        ch.mem_sub = kit::label_r(m, f.n12, th.t2, 133, 9, "");
        ch.mem_v = kit::label(m, f.n20, th.t1, 12, 26, "");
        (ch.mem, ch.mem_s, _) = kit::chart(m, 12, 60, 121, 40, CHART_PTS, th.blue, 0);
        ch.wait[2] = wait_backed(kit::label(m, f.cj12, th.t3, 8, 72, tr("正在收集…")));

        let b = kit::card(p, MARGIN, y0 + 120, CARD_W, 110);
        kit::label(b, f.cj12, th.t3, 12, 9, tr("电池"));
        ch.bat_sub = kit::label_r(b, f.cj12, th.t2, 288, 9, "");
        ch.bat_v = kit::label(b, f.n20, th.t1, 12, 26, "");
        (ch.bat, ch.bat_s, _) = kit::chart(b, 12, 56, 276, 36, CHART_PTS, th.green, 0);
        kit::label_r(b, f.cj12, th.t3, 288, 92, tr("近 5 分钟"));
        ch.wait[3] = chart_wait(b, 8, 66);
        self.home_reflow();
        120 + 110
    }

    /// 健康 on 系统: the device check's counts (refreshed before the core check: it is the
    /// agent's).
    pub(super) fn system_health_paint(&mut self) {
        let th = t();
        let ah = self.model.agent_health.clone();
        let h = self.sys.health;
        let col = if ah.checked == 0 {
            self.set(h, "—");
            th.t3
        } else if ah.bad != 0 {
            self.set(h, cf!(crate::lang::trn("■ %d 项异常", ah.bad as i64), ah.bad));
            th.bad_t
        } else if ah.warn != 0 {
            self.set(h, cf!(crate::lang::trn("▲ %d 项注意", ah.warn as i64), ah.warn));
            th.warn_t
        } else {
            self.set(h, tr("● 一切正常"));
            th.ok_t
        };
        if self.sys.health_col != Some(col) {
            self.sys.health_col = Some(col);
            kit::text_color(h, col);
        }
    }

    /// 切换到原厂界面 not confirmed within 5 s: back to plain.
    pub(super) fn vendor_arm_tick(&mut self) {
        let s = &mut self.sys;
        if s.vendor_arm != 0 && lv::tick_get().wrapping_sub(s.vendor_arm) >= 5000 {
            s.vendor_arm = 0;
            kit::button_kind(s.vendor_btn, s.vendor_lbl, BtnKind::Plain);
            s.vendor_lbl.set_text(tr("切换到原厂界面"));
        }
    }

    /// The 系统 page's values.
    pub(super) fn system_paint(&mut self) {
        let m = self.model.clone();
        let d = &m.data;
        let s = self.sys.clone();
        for (i, (name, _)) in VER_NAMES.iter().enumerate() {
            let line = m.versions.iter().find(|r| r.name == *name).map(ver_line);
            self.set(s.vers[i], line.as_deref().unwrap_or("—"));
        }
        self.set(s.vchk, m.versions_checked.as_deref().unwrap_or(""));
        self.set(s.est, m.battery_est.as_deref().unwrap_or(""));
        self.set(s.bat, cf!("%d%% · %d°C %d.%02ldV %ldmA", d.batt_pct, d.batt_temp, (d.batt_volt_uv / 1000000) as i32, (d.batt_volt_uv / 10000) % 100, d.batt_cur_ua / 1000));
        // charger_connect alone reads 1 with nothing plugged in on this hardware: a real voltage
        if d.chg_volt_uv > 1000000 {
            self.set(s.chg, cf!("%d.%02ldV %ldmA", (d.chg_volt_uv / 1000000) as i32, (d.chg_volt_uv / 10000) % 100, d.chg_cur_ua / 1000));
        } else {
            self.set(s.chg, tr("未接入"));
        }
        self.set(s.cpu, cf!(tr("占用 %ld%% · %ld°C"), d.cpu_pct, d.soc_temp));
        self.set(s.mem, cf!("%ld%% · %ldM / %ldM", d.mem_pct, (d.mem_size - d.mem_free) / 1048576, d.mem_size / 1048576));
        let up = d.uptime_s;
        self.set(s.up, cf!(if lang::is_en() { "%ldh %02ldm" } else { "%ldh%02ldm" }, up / 3600, (up / 60) % 60));
        self.set(s.ver, if d.sw_build.is_empty() { "-" } else { d.sw_build.as_str() });
        if d.imei.len() >= 8 {
            let b = d.imei.as_bytes();
            let mut masked = b[..4].to_vec();
            masked.extend_from_slice(b"********");
            masked.extend_from_slice(&b[b.len() - 3..]);
            self.set(s.imei, masked);
        } else {
            self.set(s.imei, "-");
        }
        self.set(
            s.usb,
            if d.adb_mode == "debug" {
                tr("调试已开")
            } else if !d.adb_mode.is_empty() {
                tr("调试关闭")
            } else {
                "-"
            },
        );
        self.set(s.fw, if d.firmware.is_empty() { "-" } else { d.firmware.as_str() });
    }

    /// Fed every tick whatever page is showing, so the charts open on real history. One point
    /// per CHART_STEP ticks: the average of those readings.
    pub(super) fn charts_paint(&mut self, core_silent: bool) {
        let m = self.model.clone();
        let d = &m.data;
        let ch = &mut self.charts;
        if core_silent {
            // no fake flat history while core is gone; no point averaging across the gap
            (ch.acc_cpu, ch.acc_mem, ch.acc_rx, ch.acc_tx, ch.acc_bat, ch.acc_n) = (0, 0, 0, 0, 0, 0);
            return;
        }
        ch.acc_cpu += d.cpu_pct.max(0);
        ch.acc_mem += d.mem_pct;
        ch.acc_rx += d.down_bps;
        ch.acc_tx += d.up_bps;
        ch.acc_bat += d.batt_pct as i64;
        ch.acc_n += 1;
        if ch.acc_n >= CHART_STEP {
            let n = ch.acc_n as i64;
            ch.cpu.chart_set_next_value(ch.cpu_s, (ch.acc_cpu / n) as i32);
            ch.mem.chart_set_next_value(ch.mem_s, (ch.acc_mem / n) as i32);
            ch.net.chart_set_next_value(ch.rx, rate_scale(ch.acc_rx / n));
            ch.net.chart_set_next_value(ch.tx, rate_scale(ch.acc_tx / n));
            ch.bat.chart_set_next_value(ch.bat_s, (ch.acc_bat / n) as i32);
            (ch.acc_cpu, ch.acc_mem, ch.acc_rx, ch.acc_tx, ch.acc_bat, ch.acc_n) = (0, 0, 0, 0, 0, 0);
            if ch.pts < CHART_READY {
                ch.pts += 1;
                for w in ch.wait {
                    kit::show(w, ch.pts < CHART_READY);
                }
            }
        }
        let ch = self.charts.clone();
        self.set(ch.cpu_v, cf!("%ld%%", d.cpu_pct.max(0)));
        self.set(ch.cpu_t, cf!("%ld°C", d.soc_temp));
        self.set(ch.mem_v, cf!("%ld%%", d.mem_pct));
        self.set(ch.mem_sub, cf!("%ldM / %ldM", (d.mem_size - d.mem_free) / 1048576, d.mem_size / 1048576));
        self.set(ch.net_dn, cf!("↓ %s", &fmt_rate(d.down_bps)));
        self.set(ch.net_up, cf!("↑ %s", &fmt_rate(d.up_bps)));
        ch.net_up.update_layout();
        ch.net_dn.align(lv::LV_ALIGN_TOP_RIGHT, -(12 + ch.net_up.width() + 8), 8);
        let chg = d.charger_in != 0 && d.chg_volt_uv > 1000000;
        self.set(ch.bat_v, cf!("%d%%", d.batt_pct));
        self.set(ch.bat_sub, cf!("%s · %d°C", if chg { tr("充电中") } else { tr("放电中") }, d.batt_temp));
    }
}

impl Ui {
    /// LUYAZ_UI_TAPLOG: where the pieces a tap should hit are (ui_debug_tap).
    pub fn debug_tap(&self) {
        let dk = self.sys.ap_seg.item.get(1).copied().unwrap_or(Obj::NULL);
        if dk.is_null() {
            return;
        }
        let (b, t) = (dk.coords(), self.tiles[super::TAB_SYS].coords());
        let sys = super::obj_scroller(self.tiles[super::TAB_SYS]);
        eprintln!(
            "tap:   深色 [{},{} {},{}] sys-tile [{},{} {},{}] act={} sys-scroll y={} sub={:?} hidden={}",
            b.x1,
            b.y1,
            b.x2,
            b.y2,
            t.x1,
            t.y1,
            t.x2,
            t.y2,
            self.cur_tab(),
            sys.map_or(-1, |s| s.scroll_y()),
            self.sub_cur,
            self.sub_layer.hidden()
        );
        let mut o = dk;
        while !o.is_null() {
            eprintln!(
                "tap:   chain {:?} clickable={} hidden={} scroll_y={}",
                o.0,
                o.has_flag(lv::LV_OBJ_FLAG_CLICKABLE) as i32,
                o.hidden() as i32,
                o.scroll_y()
            );
            o = o.parent();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_scale() {
        assert_eq!(rate_scale(0), 0);
        assert_eq!(rate_scale(1000), 37);
        assert_eq!(rate_scale(1532000), 77);
        assert_eq!(rate_scale(100_000_000), 100);
        assert_eq!(rate_scale(1_000_000_000_000), 100);
    }

    #[test]
    fn rates_as_c_prints_them() {
        assert_eq!(fmt_rate(1532000), b"1.5 MB/s");
        assert_eq!(fmt_rate(188000), b"183.6 KB/s");
        assert_eq!(fmt_rate(900), b"900 B/s");
        // the edges
        assert_eq!(fmt_rate(1023), b"1023 B/s");
        assert_eq!(fmt_rate(1024), b"1.0 KB/s");
        assert_eq!(fmt_rate(1048575), b"1024.0 KB/s");
        assert_eq!(fmt_rate(1048576), b"1.0 MB/s");
        assert_eq!(fmt_rate(-5), b"-5 B/s");
    }
}

