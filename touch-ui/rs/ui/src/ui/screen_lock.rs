//! 上滑锁屏: the cover that shows when the screen lights up — time, date, connection and
//! battery, both rates — and leaves on an upward swipe (logic::SwipeUnlock, stepped from
//! ui_touch_filter). Built once on the LVGL system layer (above the status bar on
//! lv_layer_top(), so it covers everything), never active until ui::create's caller runs
//! init_screen_lock: the offscreen replay stays without it. Everything on it is read-only.
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use super::refresh::rate_unit;
use super::Ui;
use crate::cf;
use crate::clock;
use crate::kit::{self, H, W};
use crate::lang::tr;
use crate::logic::{self, Rat};
use crate::lv::{self, sys, Obj};
use crate::theme::{t, uf};

/// The rate card: x=26, w=268 (two 120 px columns around the card's centre).
const CARD_X: i32 = 26;
const CARD_Y: i32 = 232;
const CARD_W: i32 = 268;
const CARD_H: i32 = 92;
/// One column's centre from the card's centre: (268 - 2*14) / 2 = 120 wide halves.
const COL_DX: i32 = 60;
const COL_KEY_Y: i32 = 18;
const COL_VAL_Y: i32 = 41;
/// The 64×4 handle bar at the bottom.
const BAR_W: i32 = 64;

pub(super) struct ScreenLock {
    /// The full-screen cover on lv_layer_sys(); Obj::NULL until init_screen_lock builds it.
    root: Obj,
    /// The swipe that unlocks it (logic.rs): reset to wait for a release on every engage.
    pub(super) swipe: logic::SwipeUnlock,
    /// Engaged (the cover state, independent of powering_down hiding it for the shutdown frame).
    locked: bool,
    time: Obj,
    date: Obj,
    status: Obj,
    dn: Obj,
    up: Obj,
}

impl Default for ScreenLock {
    fn default() -> Self {
        ScreenLock {
            root: Obj::NULL,
            swipe: logic::SwipeUnlock::default(),
            locked: false,
            time: Obj::NULL,
            date: Obj::NULL,
            status: Obj::NULL,
            dn: Obj::NULL,
            up: Obj::NULL,
        }
    }
}

/// 2G / 3G / 4G / 5G as the lock's status row says the RAT (— without service).
fn rat_word(r: Rat) -> &'static str {
    match r {
        Rat::G2 => "2G",
        Rat::G3 => "3G",
        Rat::G4 => "4G",
        Rat::G5Nsa | Rat::G5Sa => "5G",
        Rat::None => "—",
    }
}

impl Ui {
    /// Build the lock once (ui::create's caller, before the first lv_timer_handler; again on a
    /// UI restart) and engage it: the first lit frame is the lock. Offscreen never calls this,
    /// so there the lock stays inert.
    pub fn init_screen_lock(&mut self) {
        if !self.screen_lock.root.is_null() {
            return;
        }
        let th = t();
        let f = uf();
        // The system layer sits above lv_layer_top(), so the cover hides the status bar and
        // every sheet too.
        // SAFETY: LVGL is initialised; the default display's sys layer
        let root = kit::bx(Obj(unsafe { sys::lv_layer_sys() }), 0, 0, W, H, th.bg, 0);

        let time = kit::label(root, f.n36, th.t1, 0, 86, "--:--");
        time.align(lv::LV_ALIGN_TOP_MID, 0, 86);
        let date = kit::label(root, f.n15, th.t3, 0, 136, "");
        date.align(lv::LV_ALIGN_TOP_MID, 0, 136);
        let status = kit::label(root, f.cj15, th.t2, 0, 192, "");
        status.align(lv::LV_ALIGN_TOP_MID, 0, 192);

        let card = kit::card(root, CARD_X, CARD_Y, CARD_W, CARD_H);
        let dn_k = kit::label(card, f.cj12, th.t3, 0, COL_KEY_Y, tr("下行"));
        dn_k.align(lv::LV_ALIGN_TOP_MID, -COL_DX, COL_KEY_Y);
        let up_k = kit::label(card, f.cj12, th.t3, 0, COL_KEY_Y, tr("上行"));
        up_k.align(lv::LV_ALIGN_TOP_MID, COL_DX, COL_KEY_Y);
        let dn = kit::label(card, f.n20, th.t1, 0, COL_VAL_Y, "");
        dn.align(lv::LV_ALIGN_TOP_MID, -COL_DX, COL_VAL_Y);
        let up = kit::label(card, f.n20, th.t1, 0, COL_VAL_Y, "");
        up.align(lv::LV_ALIGN_TOP_MID, COL_DX, COL_VAL_Y);

        let hint = kit::label(root, f.cj13, th.t2, 0, 420, tr("向上滑动解锁"));
        hint.align(lv::LV_ALIGN_TOP_MID, 0, 420);
        kit::bx(root, (W - BAR_W) / 2, 454, BAR_W, 4, th.t3, 2);

        self.screen_lock = ScreenLock { root, locked: false, time, date, status, dn, up, ..Default::default() };
        self.engage_screen_lock();
        // Placeholders only until the panel is actually lit; a dark panel takes its values
        // from the wake path, which paints before bl_on().
        if self.src.bl_panel_lit(&self.model) {
            self.paint_screen_lock();
        }
    }

    /// Lock the screen (short press, auto-off, wake): the cover goes up over whatever page
    /// was showing, the swipe waits for a lift, and the underlying press state is dropped.
    /// No-op before init_screen_lock.
    pub fn engage_screen_lock(&mut self) {
        if self.screen_lock.root.is_null() {
            return;
        }
        self.screen_lock.locked = true;
        self.screen_lock.swipe.reset();
        self.edge_armed = false;
        self.bench_gate();
        kit::show(self.screen_lock.root, !self.powering_down());
    }

    /// The cover is up (touches below it are not forwarded). False before init_screen_lock.
    pub fn screen_locked(&self) -> bool {
        self.screen_lock.locked
    }

    /// The cover's contents and visibility. Wakes call this before bl_on(); refresh calls it
    /// after checking the panel is lit — either way it paints, so the first visible frame of
    /// the lit panel is complete. powering_down hides the cover (「正在关机…」must show).
    pub fn paint_screen_lock(&mut self) {
        let (root, time, date, status, dn, up, locked) = {
            let sl = &self.screen_lock;
            (sl.root, sl.time, sl.date, sl.status, sl.dn, sl.up, sl.locked)
        };
        if root.is_null() {
            return;
        }
        kit::show(root, locked && !self.powering_down());
        if !locked {
            return;
        }
        // The clock runs on whatever the backend's state is.
        let tm = clock::local(self.now());
        self.set(time, cf!("%02d:%02d", tm.hour, tm.min));
        self.set(date, cf!("%02d-%02d", tm.mon, tm.mday));
        let m = self.model.clone();
        if m.data_ok == 0 || m.backend_silent != 0 {
            self.set(status, tr("状态暂不可用"));
            self.set(dn, "—");
            self.set(up, "—");
            return;
        }
        let wifi = if m.data.ap_on != 0 { tr("Wi-Fi 开") } else { tr("Wi-Fi 关") };
        self.set(status, cf!("%s · %s · %d%%", rat_word(logic::rat(&m.data.radio)), wifi, m.data.batt_pct));
        let bits = self.conf.speed_bits;
        let (dn_n, dn_u) = rate_unit(m.data.down_bps, bits);
        let (up_n, up_u) = rate_unit(m.data.up_bps, bits);
        self.set(dn, cf!("%s %s", &dn_n, &dn_u));
        self.set(up, cf!("%s %s", &up_n, &up_u));
    }

    /// The unlocking lift was consumed: hide the cover and go back to the page as it was —
    /// no navigation, no scroll reset. Only the unlock path calls this.
    pub(super) fn unlock_screen_lock(&mut self) {
        self.screen_lock.locked = false;
        self.edge_armed = false;
        if !self.screen_lock.root.is_null() {
            self.screen_lock.root.add_flag(lv::LV_OBJ_FLAG_HIDDEN);
        }
        self.bench_gate();
        lv::trigger_activity();
    }
}
