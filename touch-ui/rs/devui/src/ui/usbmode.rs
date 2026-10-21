//! 插线时的 USB 用法 (src/ui_parts/usbmode.c): what a phone plugged into the U60 is for. A
//! sheet that asks (or the remembered choice, 3 s after the plug-in), and the 系统 card with
//! what is in effect and the remembered choice. the core's usb.attach_mode does it.
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use super::cellular::nz;
use super::Ui;
use crate::cf;
use crate::kit::{self, BtnKind, Sheet, CARD_W, MARGIN, PAD, ROW_H, W};
use crate::lang::{n_, tr};
use crate::logic::{self, UsbEv, UsbPref, UsbTrack};
use crate::lv::{self, sys, Obj};
use crate::theme::{t, uf};
use devui_model::core_snapshot_t;

const UM_ROWS: usize = 3;
const UM_ROW_H: i32 = 52;
/// remembered choice: wait this long after the plug-in
const UM_GRACE_MS: u32 = 3000;
/// no readback by then: say it did not take
const UM_PEND_MS: u32 = 75000; // Core write lock + USB preflight/write/readback; no automatic retry
/// 已生效 stays this long, then the sheet closes
const UM_DONE_MS: u32 = 1500;
const UM_BANNER_MS: u32 = 5000;

const UM_NAME: [&str; UM_ROWS] = [n_("充电 + 上网"), n_("快速充电宝"), n_("网口配件")];
const UM_SUB: [&str; UM_ROWS] = [n_("手机经 U60 上网，同时充电"), n_("18W 给手机快充，不上网"), n_("USB 转网口等配件；手机只充电")];
const PREFS: [UsbPref; 4] = [UsbPref::Ask, UsbPref::Share, UsbPref::Fast, UsbPref::Accessory];

#[derive(Clone)]
pub struct Usb {
    track: UsbTrack,
    pub sheet: Sheet,
    row: [Obj; UM_ROWS],
    mark: [Obj; UM_ROWS],
    status: Obj,
    rem_sw: Obj,
    ok: Obj,
    ok_lbl: Obj,
    /// chosen in the sheet, not sent yet
    sel: i32,
    /// sent, waiting for the blocks to show it
    pend: i32,
    /// … by the remembered choice, not the sheet
    pend_auto: bool,
    pend_at: u32,
    done_at: u32,
    auto_at: u32,
    /// in effect now: 0 share, 1 fast charge, -1 neither
    cur: i32,
    /// something in the port (cc = 1)
    attached: bool,
    banner: Vec<u8>,
    banner_at: u32,
    // the 系统 card
    now: Obj,
    pref_mark: [Obj; 4],
}

impl Default for Usb {
    fn default() -> Self {
        Usb {
            track: UsbTrack::default(),
            sheet: Sheet::default(),
            row: [Obj::NULL; UM_ROWS],
            mark: [Obj::NULL; UM_ROWS],
            status: Obj::NULL,
            rem_sw: Obj::NULL,
            ok: Obj::NULL,
            ok_lbl: Obj::NULL,
            sel: -1,
            pend: -1,
            pend_auto: false,
            pend_at: 0,
            done_at: 0,
            auto_at: 0,
            cur: -1,
            attached: false,
            banner: Vec::new(),
            banner_at: 0,
            now: Obj::NULL,
            pref_mark: [Obj::NULL; 4],
        }
    }
}

fn usbmode_now_text(d: &core_snapshot_t) -> &'static str {
    if d.usb_cc < 0 {
        "—"
    } else if d.usb_cc != 1 {
        tr("没插设备")
    } else if d.powerbank == 1 {
        tr("快速充电宝")
    } else if d.usb_power_role == "source" {
        if d.usb_data_role == "device" { tr("充电 + 上网") } else { tr("只充电，没有网络") }
    } else {
        tr("U60 在充电")
    }
}

unsafe extern "C" fn um_cancel_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| u.usbmode_close());
}

unsafe extern "C" fn um_row_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let i = unsafe { lv::event_user(e) } as i32;
    super::with(|u| u.um_row(i));
}

unsafe extern "C" fn um_ok_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| u.um_ok());
}

unsafe extern "C" fn um_pref_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let i = unsafe { lv::event_user(e) };
    super::with(|u| u.um_pref(i));
}

unsafe extern "C" fn um_now_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| u.um_now());
}

impl Ui {
    pub fn usbmode_visible(&self) -> bool {
        kit::sheet_visible(&self.um.sheet)
    }

    fn um_status(&mut self, txt: &str, col: u32) {
        self.um.status.set_text(txt);
        kit::text_color(self.um.status, col);
    }

    fn um_paint(&mut self) {
        let th = t();
        let u = &self.um;
        for i in 0..UM_ROWS {
            let on = i as i32 == u.cur;
            kit::bg(u.mark[i], th.fill_blue);
            u.mark[i].set_style_bg_opa(if on { lv::OPA_COVER } else { lv::OPA_TRANSP }, 0);
            u.mark[i].set_style_border_color(if on { th.fill_blue } else { th.t3 }, 0);
            let sel = i as i32 == u.sel;
            kit::bg(u.row[i], if sel { th.acc_s } else { th.glass });
            u.row[i].set_style_bg_opa(if sel { lv::OPA_COVER } else { lv::OPA_TRANSP }, 0);
        }
        kit::button_kind(u.ok, u.ok_lbl, if u.sel >= 0 && u.pend < 0 { BtnKind::Primary } else { BtnKind::Plain });
    }

    fn usbmode_open(&mut self) {
        self.um.sel = -1;
        kit::toggle_set(self.um.rem_sw, false);
        let th = t();
        self.um_status(if self.um.pend >= 0 { tr("切换中…") } else { tr("选一项，再点「确定」") }, th.t3);
        self.um_paint();
        // wake the screen: a phone was plugged in
        if !self.bl_is_on() {
            self.tgate.woke(lv::tick_get() as i64);
        }
        self.src.bl_on();
        self.sync();
        self.auto_slept = false;
        lv::trigger_activity();
        if self.power_menu_visible() {
            self.power_menu_disarm();
            self.power_menu_set(false);
        }
        kit::sheet_show(&self.um.sheet, true);
        self.update_tabs();
    }

    fn usbmode_close(&mut self) {
        kit::sheet_show(&self.um.sheet, false);
        self.um.sel = -1;
        self.update_tabs();
    }

    fn um_row(&mut self, i: i32) {
        let th = t();
        if self.um.pend >= 0 {
            self.um_status(tr("切换中…"), th.t3);
            return;
        }
        self.um.sel = i;
        self.um_status(if i == self.um.cur { tr("现在就是这样 · 点「确定」也可以记住它") } else { tr("点「确定」切换") }, th.t2);
        self.um_paint();
    }

    /// Send mode i; remembered = the automatic run. false if core could not be asked.
    fn um_send(&mut self, i: i32, remembered: bool) -> bool {
        if i == 2 {
            return true; // 网口配件: the vendor writes nothing either
        }
        let params = format!(
            "{{\"mode\":\"{}\"{}}}",
            logic::usb_pref_name(PREFS[i as usize + 1]),
            if remembered { ",\"remembered\":true" } else { "" }
        );
        if !self.data_control("usb.attach_mode", &params, None) {
            return false;
        }
        self.um.pend = i;
        self.um.pend_auto = remembered;
        self.um.pend_at = lv::tick_get();
        true
    }

    /// 「选中 + 确定」 is the two-step confirm (DESIGN §4).
    fn um_ok(&mut self) {
        let th = t();
        if self.um.pend >= 0 {
            self.um_status(tr("切换中…"), th.t3);
            return;
        }
        if self.um.sel < 0 {
            self.um_status(tr("先选一项"), th.warn_t);
            return;
        }
        if !self.um.attached {
            self.um_status(tr("设备已拔掉"), th.warn_t);
            return;
        }
        if self.um.rem_sw.has_state(lv::LV_STATE_CHECKED) {
            self.conf.usb = PREFS[self.um.sel as usize + 1];
            self.conf.save();
            self.usbmode_pref_paint();
        }
        let i = self.um.sel;
        self.um.sel = -1;
        if i == 2 || i == self.um.cur {
            self.um_status(tr("已生效"), th.ok_t);
            self.um.done_at = nz(lv::tick_get());
        } else if self.um_send(i, false) {
            self.um_status(tr("切换中…"), th.t3);
        } else {
            self.um_status(tr("数据服务不可用 · 没切"), th.bad_t);
        }
        self.um_paint();
    }

    /// The remembered choice, UM_GRACE_MS after the plug-in if still there.
    fn um_auto_run(&mut self) {
        let i = self.conf.usb as i32 - 1;
        self.um.auto_at = 0;
        if i < 0 || !self.um.attached {
            return;
        }
        if i == self.um.cur || i == 2 {
            self.um.banner = cf!(tr("已按记住的方式：%s"), tr(UM_NAME[i as usize]));
        } else if self.um_send(i, true) {
            // Reaching Core is not success: unknown capabilities refuse automatic writes.
            self.um.banner = cf!(tr("正在检查记住的方式：%s"), tr(UM_NAME[i as usize]));
        } else {
            self.um.banner = tr("数据服务不可用 · 没按记住的方式切").into();
        }
        self.um.banner_at = nz(lv::tick_get());
    }

    /// The top banner line for a remembered run, for a few seconds.
    pub(super) fn usbmode_banner(&mut self) -> Option<Vec<u8>> {
        if self.um.banner_at == 0 {
            return None;
        }
        if lv::tick_get().wrapping_sub(self.um.banner_at) >= UM_BANNER_MS {
            self.um.banner_at = 0;
            return None;
        }
        Some(self.um.banner.clone())
    }

    /// Every refresh with fresh data.
    pub(super) fn usbmode_tick(&mut self, d: &core_snapshot_t) {
        let now = lv::tick_get();
        let th = t();
        let phone = d.usb_power_role == "source" && d.usb_data_role == "host";
        let ev = logic::usb_track(&mut self.um.track, d.usb_cc, phone, now);
        if d.usb_cc >= 0 {
            self.um.attached = d.usb_cc == 1;
        }
        self.um.cur = if !self.um.attached {
            -1
        } else if d.powerbank == 1 {
            1
        } else if d.usb_power_role == "source" && d.usb_data_role == "device" {
            0
        } else {
            -1
        };
        let l = self.um.now;
        self.set(l, usbmode_now_text(d));

        if ev == UsbEv::Plugged {
            self.um.pend = -1;
            if self.conf.usb == UsbPref::Ask {
                self.usbmode_open();
            } else {
                self.um.auto_at = nz(now.wrapping_add(UM_GRACE_MS));
            }
        } else if ev == UsbEv::Unplugged {
            self.um.pend = -1;
            self.um.auto_at = 0;
            if self.usbmode_visible() {
                self.usbmode_close();
            }
        }
        if self.um.auto_at != 0 && now.wrapping_sub(self.um.auto_at) as i32 >= 0 {
            self.um_auto_run();
        }

        if self.um.pend >= 0 {
            let m = self.model.clone();
            if self.um.pend == self.um.cur {
                self.um.pend = -1;
                if self.usbmode_visible() {
                    self.um_status(tr("已生效"), th.ok_t);
                    self.um.done_at = nz(now);
                }
            } else if m.notice_ok != 0 && m.notice.action == "usb.attach_mode" && m.notice.at.wrapping_sub(self.um.pend_at) as i32 >= 0 {
                self.um.pend = -1;
                if self.usbmode_visible() {
                    let why = if m.notice.say_zh.is_empty() { tr("没切成 · 可以再试一次") }
                        else { crate::lang::pick(&m.notice.say_zh, &m.notice.say_en) };
                    self.um_status(why, th.bad_t);
                } else if self.um.pend_auto {
                    self.um.banner = if m.notice.say_zh.is_empty() {
                        tr("自动切换未完成 · 请手动选择 USB 用法").into()
                    } else { crate::lang::pick(&m.notice.say_zh, &m.notice.say_en).into() };
                    self.um.banner_at = nz(now);
                }
            } else if now.wrapping_sub(self.um.pend_at) >= UM_PEND_MS {
                self.um.pend = -1;
                if self.usbmode_visible() {
                    self.um_status(tr("没有生效 · 可以再试一次"), th.bad_t);
                }
            }
            if self.usbmode_visible() {
                self.um_paint();
            }
        }
        if self.um.done_at != 0 && now.wrapping_sub(self.um.done_at) >= UM_DONE_MS {
            self.um.done_at = 0;
            if self.usbmode_visible() {
                self.usbmode_close();
            }
        }
        if self.usbmode_visible() {
            self.um_paint();
        }
    }

    pub(super) fn build_usbmode(&mut self) {
        let th = t();
        let f = uf();
        let w = W - 16;
        // 16 = room under 确定, as at the top
        let s = kit::sheet(300 + 16, Some(um_cancel_cb));
        let p = s.panel;
        let title = kit::label(p, f.cj15, th.t1, 0, 0, tr("插入的设备怎么用"));
        title.align(lv::LV_ALIGN_TOP_MID, 0, 12);
        let mut y = 40;
        for i in 0..UM_ROWS {
            let r = kit::bx(p, 0, y, w, UM_ROW_H, th.glass, 0);
            r.set_style_bg_opa(lv::OPA_TRANSP, 0);
            kit::bx(p, 12, y, w - 24, 1, th.sep, 0);
            let mk = kit::bx(r, PAD, (UM_ROW_H - 16) / 2, 16, 16, th.card, 8);
            mk.set_style_border_width(2, 0);
            kit::label(r, f.cj15, th.t1, PAD + 26, 7, tr(UM_NAME[i]));
            kit::label_w(r, f.cj12, th.t3, PAD + 26, 30, w - PAD * 2 - 26, false, tr(UM_SUB[i]));
            kit::tappable(r, Some(um_row_cb), i);
            self.um.row[i] = r;
            self.um.mark[i] = mk;
            y += UM_ROW_H;
        }
        kit::bx(p, 12, y, w - 24, 1, th.sep, 0);
        kit::label(p, f.cj14, th.t2, PAD, y + 11, tr("记住选择，以后自动这样做"));
        self.um.rem_sw = kit::toggle(p, w - PAD, y + 7, None, 0);
        y += ROW_H;
        self.um.status = kit::label_w(p, f.cj12, th.t3, PAD, y + 4, w - 2 * PAD, false, "");
        y += 24;
        (self.um.ok, self.um.ok_lbl) = kit::button(p, PAD, y, w - 2 * PAD, 40, tr("确定"), BtnKind::Plain, Some(um_ok_cb), 0);
        self.um.sheet = s;
        self.um_paint();
    }

    // ---- 系统页「插入手机等设备时」 ----
    pub(super) fn usbmode_pref_paint(&mut self) {
        let th = t();
        for i in 0..4 {
            let m = self.um.pref_mark[i];
            if m.is_null() {
                return;
            }
            let on = self.conf.usb as usize == i;
            kit::bg(m, th.fill_blue);
            m.set_style_bg_opa(if on { lv::OPA_COVER } else { lv::OPA_TRANSP }, 0);
            m.set_style_border_color(if on { th.fill_blue } else { th.t3 }, 0);
        }
    }

    fn um_pref(&mut self, i: usize) {
        // a pending automatic run follows the old choice: drop it
        self.um.auto_at = 0;
        if PREFS[i] != self.conf.usb {
            self.conf.usb = PREFS[i];
            self.conf.save();
        }
        self.usbmode_pref_paint();
    }

    fn um_now(&mut self) {
        if !self.um.attached {
            self.um.banner = tr("没插设备 · 插上后可以在这里改用法").into();
            self.um.banner_at = nz(lv::tick_get());
            let b = self.um.banner.clone();
            self.banner_set(Some(&b));
            return;
        }
        self.usbmode_open();
    }

    /// The card on the 系统 page at y; returns its height.
    pub(super) fn build_usbmode_card(&mut self, p: Obj, y: i32) -> i32 {
        const PREF: [&str; 4] = [n_("每次询问"), n_("充电 + 上网"), n_("快速充电宝"), n_("网口配件")];
        let th = t();
        let c = kit::card(p, MARGIN, y, CARD_W, 5 * ROW_H);
        self.um.now = kit::row_nav(c, 0, tr("现在"), true, Some(um_now_cb), 0);
        self.um.now.set_style_text_font(uf().cj14, 0);
        for (i, pref) in PREF.iter().enumerate() {
            let r = kit::bx(c, 0, (i as i32 + 1) * ROW_H, CARD_W, ROW_H, th.card, 0);
            r.set_style_bg_opa(lv::OPA_TRANSP, 0);
            kit::sep(r, 0);
            let mk = kit::bx(r, PAD, (ROW_H - 16) / 2, 16, 16, th.card, 8);
            mk.set_style_border_width(2, 0);
            kit::label(r, uf().cj14, th.t1, PAD + 26, 11, tr(pref));
            kit::tappable(r, Some(um_pref_cb), i);
            self.um.pref_mark[i] = mk;
        }
        self.usbmode_pref_paint();
        5 * ROW_H
    }
}
