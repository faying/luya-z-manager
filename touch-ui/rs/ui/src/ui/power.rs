//! The power menu: a bottom sheet from a long press on the power key,
//! 关机 / 重启 two taps each, then a full-screen 「正在关机…」; and the key's poll (key_poll_cb):
//! short = screen on/off, long = the menu, and the screen going off by itself.
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use super::{Sub, Ui};
use crate::source::{KEY_EV_LONG, KEY_EV_NONE, KEY_EV_SHORT};
use crate::kit::{self, Sheet, H, W};
use crate::lang::{n_, tr};
use crate::lv::{self, sys, Obj};
use crate::theme::{t, uf};

const PW_NAME: [&str; 2] = [n_("关机"), n_("重启")];

#[derive(Default, Clone)]
pub struct Power {
    pub sheet: Sheet,
    lbl: [Obj; 2],
    cover: Obj,
    cover_lbl: Obj,
    arm_ms: u32,
    /// 0 = 关机 armed, 1 = 重启 armed, -1 none
    arm: i32,
    /// shutting down (0 / 1), -1 not
    going: i32,
    go_ms: u32,
}

pub(super) unsafe extern "C" fn key_poll_cb(_t: *mut sys::lv_timer_t) {
    super::with(|u| u.key_poll());
}

unsafe extern "C" fn act_power(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let which = unsafe { lv::event_user(e) } as i32;
    super::with(|u| u.power_tap(which));
}

unsafe extern "C" fn act_cancel(_e: *mut sys::lv_event_t) {
    super::with(|u| {
        u.power_menu_disarm();
        u.power_menu_set(false);
    });
}

impl Ui {
    pub fn power_menu_set(&mut self, v: bool) {
        kit::sheet_show(&self.pw.sheet, v);
        self.update_tabs();
    }

    pub fn power_menu_visible(&self) -> bool {
        kit::sheet_visible(&self.pw.sheet)
    }

    pub(super) fn power_menu_disarm(&mut self) {
        let p = &mut self.pw;
        if p.arm < 0 {
            return;
        }
        let l = p.lbl[p.arm as usize];
        l.set_text(tr(PW_NAME[p.arm as usize]));
        l.set_style_text_color(if p.arm != 0 { t().acc_t } else { t().bad_t }, 0);
        p.arm = -1;
    }

    /// Through core (E4: it writes the request into the change log and waits for it to reach
    /// flash first); core not there: the vendor call directly. Either way the 40 s last resort.
    pub(super) fn power_run(&mut self, which: i32) {
        self.pw.going = which;
        self.pw.go_ms = lv::tick_get();
        self.power_menu_set(false);
        let p = &self.pw;
        p.cover_lbl.set_text(if which != 0 { tr("正在重启…") } else { tr("正在关机…") });
        p.cover.remove_flag(lv::LV_OBJ_FLAG_HIDDEN);
        p.cover.move_foreground();
        self.src.bl_on();
        self.sync();
        lv::refr_now();
        if self.data_control(if which != 0 { "device.reboot" } else { "device.poweroff" }, "{}", None) {
            return;
        }
        self.src.power_fallback(which != 0);
    }

    /// Killed by the shutdown we started: the main loop leaves the panel dark on the way out.
    pub fn powering_down(&self) -> bool {
        self.pw.going >= 0
    }

    /// From the key poll: expire the arm; a shutdown still not done after 40 s says so instead
    /// of leaving 「正在关机…」 up forever.
    fn power_menu_tick(&mut self, now: u32) {
        if self.pw.arm >= 0 && now.wrapping_sub(self.pw.arm_ms) >= 5000 {
            self.power_menu_disarm();
        }
        let p = &self.pw;
        if p.going >= 0 && now.wrapping_sub(p.go_ms) >= 40000 {
            if now.wrapping_sub(p.go_ms) < 41000 {
                // once: the last resort
                let reboot = p.going != 0;
                self.src.power_last_resort(reboot);
                self.pw.cover_lbl.set_text(if reboot { tr("重启没成功，再试一次…") } else { tr("关机没成功，再试一次…") });
            } else if now.wrapping_sub(p.go_ms) >= 80000 {
                self.pw.cover.add_flag(lv::LV_OBJ_FLAG_HIDDEN);
                self.pw.going = -1;
            }
        }
    }

    /// The power key (key_poll_cb, every 50 ms; 1 s while the lit screen is quiet).
    fn key_poll(&mut self) {
        let ev = self.src.key_poll(lv::tick_get());
        if ev != KEY_EV_NONE {
            // the key is not an LVGL input: without this a wake by the key was undone by the
            // auto-off below in the same pass (a one-frame flash)
            lv::trigger_activity();
        }
        self.power_menu_tick(lv::tick_get());
        if self.pw.going >= 0 {
            return; // shutting down: keep 「正在关机…」 lit
        }
        if ev == KEY_EV_SHORT {
            // from the real brightness: the vendor key daemon sees the key too
            if self.src.bl_is_lit(&self.model) {
                self.src.bl_off();
                if self.power_menu_visible() {
                    self.power_menu_disarm();
                    self.power_menu_set(false);
                }
            } else {
                self.src.bl_on();
                self.tgate.woke(lv::tick_get() as i64);
            }
            self.sync();
            self.auto_slept = false;
        } else if ev == KEY_EV_LONG {
            if !self.src.bl_is_lit(&self.model) {
                self.tgate.woke(lv::tick_get() as i64);
            }
            self.src.bl_on();
            self.sync();
            self.auto_slept = false;
            self.power_menu_disarm();
            let v = !self.power_menu_visible();
            self.power_menu_set(v);
        }

        // Auto screen-off after inactivity. Not while the power menu or the 插线用法 sheet is
        // up, nor on 摆放模式 (watched while the device is moved).
        if self.autooff_ms != 0 && !self.power_menu_visible() && !self.usbmode_visible() && self.sub_cur != Some(Sub::Place) {
            let idle = lv::inactive_ms();
            // exec'd with the screen off: inactivity restarted at zero with the new process;
            // wait for a touch that happened after the start
            if self.wake_guard && idle.wrapping_add(50) < lv::tick_elaps(self.wake_idle_last) {
                self.wake_guard = false;
            }
            if self.wake_guard {
                return;
            }
            if idle > self.autooff_ms {
                if self.bl_is_on() {
                    self.src.bl_off();
                    self.sync();
                    self.auto_slept = true;
                }
            } else if self.auto_slept {
                // activity with the screen dark: only the key gets here
                self.src.bl_on();
                self.sync();
                self.tgate.woke(lv::tick_get() as i64);
                self.auto_slept = false;
            }
        }
    }

    fn power_tap(&mut self, which: i32) {
        let now = lv::tick_get();
        if self.pw.going >= 0 {
            return;
        }
        if self.pw.arm == which && now.wrapping_sub(self.pw.arm_ms) < 5000 {
            self.power_run(which);
            return;
        }
        self.power_menu_disarm();
        let p = &mut self.pw;
        p.arm = which;
        p.arm_ms = now;
        let l = p.lbl[which as usize];
        l.set_text(if which != 0 { tr("再按一次重启") } else { tr("再按一次关机") });
        l.set_style_text_color(t().warn_t, 0);
    }

    pub(super) fn build_power_menu(&mut self) {
        let th = t();
        let s = kit::sheet(120, Some(act_cancel));
        let title = kit::label(s.panel, uf().cj13, th.t3, 0, 0, tr("电源"));
        title.align(lv::LV_ALIGN_TOP_MID, 0, 12);
        self.pw.lbl[0] = kit::sheet_item(&s, 38, tr("关机"), th.bad_t, Some(act_power), 0).0;
        self.pw.lbl[1] = kit::sheet_item(&s, 78, tr("重启"), th.acc_t, Some(act_power), 1).0;
        self.pw.sheet = s;
        self.pw.arm = -1;
        self.pw.going = -1;
        let c = kit::bx(Obj::layer_top(), 0, 0, W, H, th.bg, 0);
        c.add_flag(lv::LV_OBJ_FLAG_CLICKABLE); // swallow taps
        let l = kit::label(c, uf().cj15, th.t1, 0, 0, "");
        l.center();
        c.add_flag(lv::LV_OBJ_FLAG_HIDDEN);
        self.pw.cover = c;
        self.pw.cover_lbl = l;
    }
}
