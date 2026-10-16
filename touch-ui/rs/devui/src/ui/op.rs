//! E4 write transactions on the screen (src/ui_parts/op.c): the 事务行 under the status bar,
//! Home's one-time 「自动退回已打开」 line, the 网络模式 segment's line while datad has a
//! transaction for it, 「上次改动」 under 网络模式, and the write controls greyed while datad is
//! stuck; the 事务页 (conclusion → 旧 → 新 · 来源 → three steps → countdown → buttons) and
//! 系统 › 改动记录 with each change in full.
// SPDX-License-Identifier: MIT

use super::{obj_scroller, Sub, Ui, SUBS, SUB_VIEW, TABS, TAB_CELL, TAB_HOME, VIEW_H};
use crate::cf;
use crate::kit::{self, BtnKind, BAR_H, CARD_W, NAV_H, PAD, MARGIN, W};
use crate::lang::tr;
use crate::lv::{self, sys, Obj};
use crate::op_view::{self, OpItem, OpLog, OpOwner, OpView};
use crate::theme::{t, uf};

const OP_ROW_H: i32 = 40;
const OP_BRIEF_MS: u32 = 3000;
/// no new "op" for this long while counting: the clock stops
const OP_STALE_MS: u32 = 20000;

const OP_ARM_MS: u32 = 5000;
const LOG_ROW_H: i32 = 56;

// the 事务页's buttons
const OPB_NONE: i32 = 0;
const OPB_REVERT: i32 = 1;
const OPB_KEEP: i32 = 2;
const OPB_ACK: i32 = 3;
const OPB_RETRY: i32 = 4;
const OPB_REBOOT: i32 = 5;

const OPR_NONE: i32 = 0;
const OPR_LIVE: i32 = 1;
const OPR_RESULT: i32 = 2;

pub struct Op {
    pub view: OpView,
    raw: String,
    /// lv tick of the last parse that changed it
    at: u32,
    parsed: bool,
    /// an op_id this screen saw in "active"
    seen_live: String,
    brief_id: String,
    brief_at: u32,
    /// 知道了 tapped here: hide before datad says so
    acked: String,
    row_state: i32,
    row_shift: i32,
    opr: Obj,
    opr_dot: Obj,
    opr_lbl: Obj,
    /// DD18 知道了 tapped here
    pub notice_acked: bool,
    guard_was: i32,
    // 改动记录 (read with journal.list) and owners["network.mode"]
    pub log: Vec<OpLog>,
    pub log_n: i32,
    pub own_mode: OpOwner,
    owner_seen: bool,
    owner_key: String,
    owner_val: String,
    owner_was: i32,
    // the 事务页
    opp_say: Obj,
    opp_meta: Obj,
    opp_note: Obj,
    opp_left: Obj,
    opp_why: Obj,
    opp_step_dot: [Obj; op_view::STEPS],
    opp_step_lbl: [Obj; op_view::STEPS],
    pub opp_btn: [Obj; 3],
    opp_btn_lbl: [Obj; 3],
    opp_role: [i32; 3],
    opp_armed: i32,
    opp_arm_at: u32,
    // 改动记录
    log_err: bool,
    pub log_sel: i32,
    log_status: Obj,
    log_card: Obj,
    pub log_row: Vec<Obj>,
    log_l1: Vec<Obj>,
    log_l2: Vec<Obj>,
    log_dot: Vec<Obj>,
    log_foot: Obj,
    logd_what: Obj,
    logd_change: Obj,
    logd_meta: Obj,
    logd_result: Obj,
    pub logd_btn: Obj,
    logd_btn_lbl: Obj,
    logd_why: Obj,
    logd_arm: u32,
}

impl Default for Op {
    fn default() -> Self {
        Op {
            view: OpView::default(),
            raw: String::new(),
            at: 0,
            parsed: false,
            seen_live: String::new(),
            brief_id: String::new(),
            brief_at: 0,
            acked: String::new(),
            row_state: -1,
            row_shift: -1,
            opr: Obj::NULL,
            opr_dot: Obj::NULL,
            opr_lbl: Obj::NULL,
            notice_acked: false,
            guard_was: -1,
            log: Vec::new(),
            log_n: -1,
            own_mode: OpOwner::default(),
            owner_seen: false,
            owner_key: String::new(),
            owner_val: String::new(),
            owner_was: -1,
            opp_say: Obj::NULL,
            opp_meta: Obj::NULL,
            opp_note: Obj::NULL,
            opp_left: Obj::NULL,
            opp_why: Obj::NULL,
            opp_step_dot: [Obj::NULL; op_view::STEPS],
            opp_step_lbl: [Obj::NULL; op_view::STEPS],
            opp_btn: [Obj::NULL; 3],
            opp_btn_lbl: [Obj::NULL; 3],
            opp_role: [OPB_NONE; 3],
            opp_armed: OPB_NONE,
            opp_arm_at: 0,
            log_err: false,
            log_sel: -1,
            log_status: Obj::NULL,
            log_card: Obj::NULL,
            log_row: Vec::new(),
            log_l1: Vec::new(),
            log_l2: Vec::new(),
            log_dot: Vec::new(),
            log_foot: Obj::NULL,
            logd_what: Obj::NULL,
            logd_change: Obj::NULL,
            logd_meta: Obj::NULL,
            logd_result: Obj::NULL,
            logd_btn: Obj::NULL,
            logd_btn_lbl: Obj::NULL,
            logd_why: Obj::NULL,
            logd_arm: 0,
        }
    }
}

fn op_mark_col(mark: i32) -> u32 {
    let th = t();
    match mark {
        op_view::MARK_OK => th.ok_t,
        op_view::MARK_WARN => th.warn_t,
        op_view::MARK_BAD => th.bad_t,
        _ => th.acc_t,
    }
}

unsafe extern "C" fn op_row_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| u.sub_open(Sub::Op));
}

/// 「上次改动」: open that change in 系统 › 改动记录 (返回 lands on the list); not among the
/// rows read (older than 20, or no op_id): the list itself.
unsafe extern "C" fn op_owner_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| {
        u.sub_open(Sub::Log); // reads the log (and owners) afresh
        u.op_owner_paint();
        u.op.log_sel = -1;
        let n = u.op.log_n.max(0) as usize;
        if !u.op.own_mode.op_id.is_empty()
            && let Some(i) = u.op.log.iter().take(n).position(|l| l.op_id == u.op.own_mode.op_id)
        {
            u.op.log_sel = i as i32;
        }
        if u.op.log_sel >= 0 {
            u.sub_open_child(Sub::LogDetail, Sub::Log);
        }
    });
}

unsafe extern "C" fn op_btn_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let slot = unsafe { lv::event_user(e) };
    super::with(|u| u.op_btn_tap(slot));
}

unsafe extern "C" fn log_row_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let i = unsafe { lv::event_user(e) };
    super::with(|u| {
        u.op.log_sel = i as i32;
        u.sub_open_child(Sub::LogDetail, Sub::Log);
    });
}

unsafe extern "C" fn logd_undo_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| u.logd_undo());
}

/// The detail pages' scrolling column (a card, then buttons).
fn column_page(p: Obj, scrollbar: bool, pad_bottom: bool) -> Obj {
    let sc = Obj::create(p);
    sc.remove_style_all();
    sc.set_size(W, SUB_VIEW);
    sc.set_scroll_dir(sys::lv_dir_t::LV_DIR_VER);
    if scrollbar {
        sc.set_scrollbar_mode(sys::lv_scrollbar_mode_t::LV_SCROLLBAR_MODE_ACTIVE);
    }
    sc.flex_column();
    sc.flex_align_start_center();
    sc.set_style_pad_top(4, 0);
    if pad_bottom {
        sc.set_style_pad_bottom(24, 0);
    }
    sc.set_style_pad_row(10, 0);
    sc
}

fn column_card(sc: Obj) -> Obj {
    let c = kit::card(sc, 0, 0, CARD_W, 10);
    c.set_height(lv::SIZE_CONTENT);
    c.set_style_pad_all(PAD, 0);
    c.flex_column();
    c.set_style_pad_row(6, 0);
    c
}

/// A wrapping label as wide as the card.
fn wrap_label(c: Obj, f: crate::lv::Font, col: u32) -> Obj {
    let l = kit::label(c, f, col, 0, 0, "");
    l.set_width(lv::pct(100));
    l.set_long_mode(lv::LV_LABEL_LONG_MODE_WRAP);
    l
}
pub(super) const OP_OWNER_CB: lv::EventCb = op_owner_cb;

impl Ui {
    /// The item this screen shows: the one in progress, else a result to show.
    fn op_shown(&self) -> (Option<OpItem>, i32) {
        let now = lv::tick_get();
        let v = &self.op.view;
        let l = &v.last;
        if v.active.have {
            return (Some(v.active.clone()), OPR_LIVE);
        }
        if !l.have || l.op_id == self.op.acked {
            return (None, OPR_NONE);
        }
        if l.needs_ack || (l.op_id == self.op.brief_id && now.wrapping_sub(self.op.brief_at) < OP_BRIEF_MS) {
            return (Some(l.clone()), OPR_RESULT);
        }
        (None, OPR_NONE)
    }

    /// Time left on the countdown, counted down here between fetches; -1 = none.
    fn op_left_ms(&self, it: &OpItem) -> i64 {
        if it.remaining_ms < 0 || it.next.is_empty() {
            return -1;
        }
        (it.remaining_ms - lv::tick_get().wrapping_sub(self.op.at) as i64).max(0)
    }

    /// datad has not said anything new for a while (or is stuck): the clock stops.
    fn op_stale(&self) -> bool {
        self.model.screen_stuck != 0 || lv::tick_get().wrapping_sub(self.op.at) >= OP_STALE_MS
    }

    fn op_parse(&mut self) {
        let raw = self.model.screen_op.clone().unwrap_or_default();
        let o = &mut self.op;
        if o.parsed && raw == o.raw {
            return;
        }
        let prev_last = o.view.last.op_id.clone();
        o.view = op_view::parse(&raw).unwrap_or_default();
        o.raw = raw;
        o.at = lv::tick_get();
        if o.view.active.have {
            o.seen_live = o.view.active.op_id.clone();
        }
        // a result this screen watched end: its brief 3 s start now
        let l = &o.view.last;
        if l.have && l.op_id != prev_last && (o.parsed || l.op_id == o.seen_live) {
            o.brief_id = l.op_id.clone();
            o.brief_at = o.at;
        }
        o.parsed = true;
    }

    // ---- the row ----
    pub(super) fn build_op_row(&mut self) {
        let th = t();
        let r = kit::bx(Obj::layer_top(), 0, BAR_H, W, OP_ROW_H, th.card, 0);
        r.set_style_border_side(lv::LV_BORDER_SIDE_BOTTOM, 0);
        r.set_style_border_width(1, 0);
        r.set_style_border_color(th.sep, 0);
        self.op.opr_dot = kit::dot(r, PAD + MARGIN, (OP_ROW_H - 8) / 2, 8, th.acc_t);
        let l = kit::label(r, uf().cj15, th.t1, PAD + MARGIN + 16, 0, "");
        l.set_width(W - (PAD + MARGIN + 16) - 40);
        l.set_long_mode(lv::LV_LABEL_LONG_MODE_DOTS);
        l.align(lv::LV_ALIGN_LEFT_MID, PAD + MARGIN + 16, 0);
        let chev = kit::label(r, uf().cj17b, th.t3, 0, 0, "›");
        chev.align(lv::LV_ALIGN_RIGHT_MID, -(PAD + MARGIN), 0);
        kit::tappable(r, Some(op_row_cb), 0);
        kit::show(r, false);
        self.op.opr = r;
        self.op.opr_lbl = l;
    }

    /// Move every tab's (not Home's) and subpage's scroller down by dy so the row covers
    /// nothing. Only when the row comes or goes.
    fn op_row_shift(&mut self, dy: i32) {
        if dy == self.op.row_shift {
            return;
        }
        self.op.row_shift = dy;
        for i in 0..TABS {
            let Some(sc) = (if i == TAB_HOME { None } else { obj_scroller(self.tiles[i]) }) else { continue };
            sc.set_y(dy);
            sc.set_height(VIEW_H - dy);
        }
        for (i, &s) in SUBS.iter().enumerate() {
            let Some(sc) = (if s == Sub::Op { None } else { obj_scroller(self.sub_page[i]) }) else { continue };
            sc.set_y(dy);
            sc.set_height(super::SUB_VIEW - dy);
        }
    }

    fn op_row_paint(&mut self) {
        let sub = self.sub_cur.is_some();
        let (it, state) = self.op_shown();
        let show = it.is_some() && self.sub_cur != Some(Sub::Op) && (sub || self.cur_tab() != TAB_HOME);
        self.op_row_shift(if it.is_some() { OP_ROW_H } else { 0 });
        let o = &self.op;
        let Some(it) = it.filter(|_| show) else {
            kit::show(o.opr, false);
            self.op.row_state = OPR_NONE;
            return;
        };
        o.opr.set_y(if sub { NAV_H } else { BAR_H });
        let c_row = if state == OPR_LIVE {
            let left = self.op_left_ms(&it);
            if left >= 0 && !self.op_stale() {
                format!("{} · {}", it.say, op_view::fill_clock("{t}", left))
            } else {
                it.say.clone()
            }
        } else {
            it.say.clone()
        };
        if o.opr_lbl.text() != c_row.as_bytes() {
            o.opr_lbl.set_text(&c_row);
        }
        kit::bg(o.opr_dot, if state == OPR_LIVE { t().acc_t } else { op_mark_col(it.mark) });
        kit::show(o.opr, true);
        o.opr.move_foreground();
        self.op.row_state = state;
    }

    // ---- DD18: 自动退回已打开 · 知道了, once, on Home ----
    pub(super) fn op_notice_paint(&mut self) {
        let want = self.op.view.notice == "rollback_on";
        if !want {
            self.op.notice_acked = false;
        }
        let want = want && !self.op.notice_acked;
        if want == !self.home.hn_box.hidden() {
            return;
        }
        kit::show(self.home.hn_box, want);
        self.home_reflow();
    }

    /// Every refresh tick.
    pub(super) fn op_refresh(&mut self) {
        self.op_parse();
        self.op_guard_paint();
        self.op_notice_paint();
        self.op_row_paint();
        self.op_owner_refresh();
        if self.sub_cur == Some(Sub::Op) {
            self.op_page_paint();
        }
    }

    // ---- the page ----
    fn op_send(&mut self, action: &str, op_id: &str) {
        let p = format!("{{\"op_id\":\"{op_id}\"}}");
        self.data_control(action, &p, None);
    }

    fn op_btn_tap(&mut self, slot: usize) {
        let role = self.op.opp_role[slot];
        let now = lv::tick_get();
        let (it, _) = self.op_shown();
        let Some(it) = it else { return };
        if role == OPB_NONE {
            return;
        }
        if role == OPB_ACK {
            self.op_send("op.ack", &it.op_id);
            self.op.acked = it.op_id.clone();
            self.sub_back();
            return;
        }
        if self.op.opp_armed != role || now.wrapping_sub(self.op.opp_arm_at) >= OP_ARM_MS {
            self.op.opp_armed = role; // the first tap arms, says what the second does
            self.op.opp_arm_at = now.max(1);
            self.op_page_paint(); // answered at once (DESIGN.md §4)
            return;
        }
        self.op.opp_armed = OPB_NONE;
        self.op_page_paint();
        match role {
            OPB_REVERT => self.op_send("op.revert", &it.op_id),
            OPB_KEEP => self.op_send("op.keep", &it.op_id),
            OPB_RETRY => {
                // 再试一次退回到 X: a new write of X (network mode is the only item with
                // transactions; other items get no such button)
                let p = format!("{{\"mode\":\"{}\"}}", it.rollback_to_raw);
                let fb = format!("mode={}", it.rollback_to_raw);
                self.data_control("network.set_mode", &p, Some(&fb));
            }
            OPB_REBOOT => self.power_run(1),
            _ => {}
        }
    }

    pub(super) fn build_sub_op(&mut self, p: Obj) {
        let th = t();
        let f = uf();
        let sc = column_page(p, true, true);
        let c = column_card(sc);
        let o = &mut self.op;
        // 1 conclusion
        o.opp_say = wrap_label(c, f.cj24b, th.t1);
        // 2 旧 → 新 · 来源
        o.opp_meta = wrap_label(c, f.cj13, th.t3);
        o.opp_note = kit::label(c, f.cj13, th.warn_t, 0, 0, "");
        // 3 three steps
        for i in 0..op_view::STEPS {
            let r = kit::bx(c, 0, 0, lv::pct(100), 26, th.card, 0);
            r.set_style_bg_opa(lv::OPA_TRANSP, 0);
            o.opp_step_dot[i] = kit::dot(r, 0, 9, 8, th.t3);
            o.opp_step_lbl[i] = kit::label(r, f.cj15, th.t2, 16, 2, "");
        }
        // 4 countdown
        o.opp_left = wrap_label(c, f.cj15, th.t1);
        o.opp_why = wrap_label(c, f.cj13, th.t2);
        // 5 buttons: two side by side, a third under them
        let b = kit::bx(sc, 0, 0, CARD_W, 44 + 10 + 44, th.bg, 0);
        b.set_style_bg_opa(lv::OPA_TRANSP, 0);
        for i in 0..3 {
            let w = if i < 2 { (CARD_W - 10) / 2 } else { CARD_W };
            let (bt, l) = kit::button(b, if i == 1 { w + 10 } else { 0 }, if i < 2 { 0 } else { 54 }, w, 44, "", BtnKind::Plain, Some(op_btn_cb), i);
            l.set_long_mode(lv::LV_LABEL_LONG_MODE_DOTS);
            l.set_size(w - 16, f.cj15b.line_height());
            l.set_style_text_align(lv::LV_TEXT_ALIGN_CENTER, 0);
            kit::show(bt, false);
            o.opp_btn[i] = bt;
            o.opp_btn_lbl[i] = l;
        }
    }

    /// Where a button sits: half = left / right half of row `row`, or the full width (-1).
    fn op_btn_place(&self, slot: usize, row: i32, half: i32) {
        let hw = (CARD_W - 10) / 2;
        let w = if half < 0 { CARD_W } else { hw };
        let (b, l) = (self.op.opp_btn[slot], self.op.opp_btn_lbl[slot]);
        b.set_pos(if half == 1 { hw + 10 } else { 0 }, row * 54);
        b.set_width(w);
        l.set_width(w - 16);
        l.center();
    }

    fn op_btn_set(&mut self, slot: usize, role: i32, text: &str, kind: BtnKind) {
        let armed = role != OPB_NONE && role == self.op.opp_armed;
        self.op.opp_role[slot] = role;
        let (b, l) = (self.op.opp_btn[slot], self.op.opp_btn_lbl[slot]);
        kit::show(b, role != OPB_NONE);
        if role == OPB_NONE {
            return;
        }
        let mut c = (if armed { tr("再按一次") } else { text }).as_bytes().to_vec();
        c.truncate(79);
        if l.text() != c {
            l.set_text(&c);
        }
        kit::button_kind(b, l, if armed { BtnKind::Armed } else { kind });
    }

    fn op_page_paint(&mut self) {
        let th = t();
        let (it, state) = self.op_shown();
        let now = lv::tick_get();
        if self.op.opp_armed != OPB_NONE && now.wrapping_sub(self.op.opp_arm_at) >= OP_ARM_MS {
            self.op.opp_armed = OPB_NONE;
        }
        let o = &self.op;
        let (say, meta, note, left, why) = (o.opp_say, o.opp_meta, o.opp_note, o.opp_left, o.opp_why);
        let (dots, lbls) = (o.opp_step_dot, o.opp_step_lbl);
        let Some(it) = it else {
            // nothing left to show (acked elsewhere, or a brief result timed out)
            self.set(say, tr("没有进行中的改动"));
            kit::text_color(say, th.t2);
            meta.set_text("");
            kit::show(note, false);
            for i in 0..op_view::STEPS {
                kit::show(dots[i], false);
                kit::show(lbls[i], false);
            }
            kit::show(left, false);
            kit::show(why, false);
            for i in 0..3 {
                self.op_btn_set(i, OPB_NONE, "", BtnKind::Plain);
            }
            return;
        };
        // the title is what is happening (正在换制式 / the item)
        let title = self.sub_title;
        self.set_at(title, 9, if state == OPR_LIVE { &it.say } else { &it.what });
        self.set(say, &it.say);
        kit::text_color(say, if state == OPR_LIVE { th.t1 } else { op_mark_col(it.mark) });
        self.set(meta, cf!("%s → %s · %s", &it.old_v, &it.target, &it.source));
        kit::show(note, !it.note.is_empty());
        if !it.note.is_empty() {
            note.set_text(&it.note);
        }
        for i in 0..op_view::STEPS {
            kit::show(dots[i], !it.step[i].is_empty());
            kit::show(lbls[i], !it.step[i].is_empty());
            if lbls[i].text() != it.step[i].as_bytes() {
                lbls[i].set_text(&it.step[i]);
            }
            kit::bg(dots[i], if it.step_done[i] { th.ok_t } else { th.t3 });
            kit::text_color(lbls[i], if it.step_done[i] { th.t1 } else { th.t3 });
        }
        // countdown: counted down here; stale → stopped and greyed (DD12)
        let mut c_left = String::new();
        if state == OPR_LIVE && self.op_left_ms(&it) >= 0 {
            if self.op_stale() {
                let tt = op_view::fill_clock("{t}", self.op_left_ms(&it));
                c_left = String::from_utf8_lossy(&cf!(tr("停在 %s · 等设备响应"), &tt)).into_owned();
            } else {
                c_left = op_view::fill_clock(&it.next, self.op_left_ms(&it));
            }
        }
        kit::show(left, !c_left.is_empty());
        if !c_left.is_empty() && left.text() != c_left.as_bytes() {
            left.set_text(&c_left);
        }
        kit::text_color(left, if self.op_stale() { th.t3 } else { th.t1 });

        // what the armed button will do, or the advice for this result
        let armed = self.op.opp_armed;
        let c_why: Vec<u8> = if armed == OPB_KEEP {
            tr("不再自动退回；还没确认通，没网要你自己改回去").into()
        } else if armed == OPB_REVERT {
            cf!(tr("马上改回「%s」，会重新注册，断网几十秒"), &it.rollback_to)
        } else if armed == OPB_RETRY {
            cf!(tr("再发一次改回「%s」"), &it.rollback_to)
        } else if armed == OPB_REBOOT {
            tr("重启设备，一两分钟没网").into()
        } else if state == OPR_LIVE && it.phase == "verifying" && !self.op.view.rollback_enabled {
            tr("自动退回没开：没通也会保持").into()
        } else if state == OPR_RESULT && it.phase == "not_applied" {
            tr("基带崩过以后常见，重启后再切一次").into()
        } else if state == OPR_RESULT && it.phase == "rollback_failed" {
            cf!(tr("现在：%s · 上次确认：%s"), if it.readback.is_empty() { tr("当前设置未知") } else { it.readback.as_str() }, &it.rollback_to)
        } else {
            Vec::new()
        };
        kit::show(why, !c_why.is_empty());
        if !c_why.is_empty() && why.text() != c_why {
            why.set_text(&c_why);
        }

        // buttons. DD13: 退回 X left (primary), 保留 Y right. DD9: 再试一次退回 on its own row
        // (the value can be long), 重启设备 and 知道了 under it.
        if state == OPR_LIVE {
            self.op_btn_place(0, 0, 0);
            self.op_btn_place(1, 0, 1);
            self.op_btn_set(0, if it.can_revert { OPB_REVERT } else { OPB_NONE }, &it.revert_label, BtnKind::Primary);
            self.op_btn_set(1, if it.can_keep { OPB_KEEP } else { OPB_NONE }, &it.keep_label, BtnKind::Plain);
            self.op_btn_set(2, OPB_NONE, "", BtnKind::Plain);
        } else if it.phase == "rollback_failed" {
            let retry = it.item == "network.mode" && !it.rollback_to_raw.is_empty();
            let mut r = cf!(tr("再试一次退回到%s"), &it.rollback_to);
            r.truncate(95);
            let r = String::from_utf8_lossy(&r).into_owned();
            self.op_btn_place(0, 0, -1);
            self.op_btn_place(1, retry as i32, 0);
            self.op_btn_place(2, retry as i32, 1);
            self.op_btn_set(0, if retry { OPB_RETRY } else { OPB_NONE }, &r, BtnKind::Primary);
            self.op_btn_set(1, OPB_REBOOT, tr("重启设备"), BtnKind::Danger);
            self.op_btn_set(2, OPB_ACK, tr("知道了"), BtnKind::Plain);
        } else if it.phase == "not_applied" {
            self.op_btn_place(1, 0, 0);
            self.op_btn_place(2, 0, 1);
            self.op_btn_set(0, OPB_NONE, "", BtnKind::Plain);
            self.op_btn_set(1, OPB_REBOOT, tr("重启设备"), BtnKind::Danger);
            self.op_btn_set(2, OPB_ACK, tr("知道了"), BtnKind::Plain);
        } else {
            self.op_btn_place(2, 0, -1);
            self.op_btn_set(0, OPB_NONE, "", BtnKind::Plain);
            self.op_btn_set(1, OPB_NONE, "", BtnKind::Plain);
            self.op_btn_set(2, if it.needs_ack { OPB_ACK } else { OPB_NONE }, tr("知道了"), BtnKind::Plain);
        }
    }

    /// The 事务页 just opened: painted now, not at the next refresh tick.
    pub(super) fn op_on_open(&mut self) {
        self.op.opp_armed = OPB_NONE;
        self.op_parse();
        self.op_page_paint();
    }

    // ---- 系统 › 改动记录 (DD5, DD10, DD11) ----
    // datad's journal.list, each line already in words. Read when the page opens; a failed
    // read keeps what was shown, dimmed, and says so. Two lines a row: what · change, then
    // time · source · result. A row opens its detail, where 撤销 / 重做 is (two taps) or,
    // greyed, why not.
    pub(super) fn build_sub_log(&mut self, p: Obj) {
        let th = t();
        let f = uf();
        let sc = kit::scroll(p, 0, SUB_VIEW, 24 + 20 + op_view::LOG_MAX as i32 * LOG_ROW_H + 60);
        let o = &mut self.op;
        o.log_status = kit::label_w(sc, f.cj13, th.t3, MARGIN + 4, 4, CARD_W - 8, true, "");
        o.log_card = kit::card(sc, MARGIN, 28, CARD_W, LOG_ROW_H);
        for i in 0..op_view::LOG_MAX {
            let r = kit::bx(o.log_card, 0, i as i32 * LOG_ROW_H, CARD_W, LOG_ROW_H, th.card, 0);
            r.set_style_bg_opa(lv::OPA_TRANSP, 0);
            if i > 0 {
                kit::sep(o.log_card, i as i32 * LOG_ROW_H);
            }
            let l1 = kit::label(r, f.cj15, th.t1, PAD, 8, "");
            l1.set_width(CARD_W - 2 * PAD - 14);
            l1.set_long_mode(lv::LV_LABEL_LONG_MODE_DOTS);
            l1.set_height(f.cj15.line_height());
            let dot = kit::dot(r, PAD, 35, 7, th.t3);
            let l2 = kit::label(r, f.cj13, th.t3, PAD + 12, 30, "");
            l2.set_width(CARD_W - 2 * PAD - 26);
            l2.set_long_mode(lv::LV_LABEL_LONG_MODE_DOTS);
            l2.set_height(f.cj13.line_height());
            kit::label(r, f.cj17b, th.t3, CARD_W - PAD - 8, 16, "›");
            kit::tappable(r, Some(log_row_cb), i);
            kit::show(r, false);
            o.log_row.push(r);
            o.log_l1.push(l1);
            o.log_l2.push(l2);
            o.log_dot.push(dot);
        }
        o.log_foot = kit::label(sc, f.cj12, th.t3, MARGIN + 4, 0, "");
    }

    fn log_paint(&mut self) {
        let o = &self.op;
        let n = o.log_n.max(0) as usize;
        let status: &str = if o.log_err && o.log_n < 0 {
            tr("读不到改动记录 · 数据服务没响应")
        } else if o.log_err {
            tr("读不到改动记录 · 下面是上次读到的")
        } else if n == 0 {
            tr("还没有改动 · 触屏、网页、情景、定时任务改的设置都会记在这里")
        } else {
            ""
        };
        o.log_status.set_text(status);
        kit::show(o.log_card, n > 0);
        o.log_card.set_y(if status.is_empty() { 28 } else { 28 + 2 * 18 });
        o.log_card.set_height(n.max(1) as i32 * LOG_ROW_H);
        o.log_card.set_style_opa(if o.log_err { lv::OPA_50 } else { lv::OPA_COVER }, 0);
        for i in 0..op_view::LOG_MAX {
            kit::show(o.log_row[i], i < n);
            if i >= n {
                continue;
            }
            let l = &o.log[i];
            let mut l1 = if l.change.is_empty() { l.what.clone().into_bytes() } else { cf!("%s · %s", &l.what, &l.change) };
            l1.truncate(159);
            // 「情景跳过 ×5（…）」 already names who: not 情景 twice
            let named = !l.source.is_empty() && l.result.starts_with(&l.source);
            let mut l2 = if !l.source.is_empty() && !named {
                cf!("%s · %s · %s", &l.when, &l.source, &l.result)
            } else {
                cf!("%s · %s", &l.when, &l.result)
            };
            l2.truncate(199);
            o.log_l1[i].set_text(&l1);
            o.log_l2[i].set_text(&l2);
            kit::bg(o.log_dot[i], op_mark_col(l.mark));
        }
        let foot = cf!(tr("只显示最近 %d 条"), op_view::LOG_MAX);
        o.log_foot.set_text(if n > 0 { foot.as_slice() } else { b"" });
        o.log_foot.set_y(o.log_card.y() + n as i32 * LOG_ROW_H + 8);
    }

    pub(super) fn op_log_on_open(&mut self) {
        self.op.log_err = !self.op_log_fetch();
        self.log_paint();
    }

    fn logd_undo(&mut self) {
        let now = lv::tick_get();
        let o = &self.op;
        if o.log_sel < 0 || o.log_sel >= o.log_n.max(0) {
            return;
        }
        let l = o.log[o.log_sel as usize].clone();
        if !l.undo_ok {
            return;
        }
        if o.logd_arm == 0 || now.wrapping_sub(o.logd_arm) >= OP_ARM_MS {
            self.op.logd_arm = now.max(1); // the first tap arms (DD10: two taps)
            let (b, lb) = (self.op.logd_btn, self.op.logd_btn_lbl);
            lb.set_text(tr("再按一次"));
            kit::button_kind(b, lb, BtnKind::Armed);
            return;
        }
        self.op.logd_arm = 0;
        self.src.data_control_undo(&l.undo_action, &l.undo_params);
        self.sync();
        self.sub_back(); // the 事务行 takes it from here
        self.sub_back();
    }

    pub(super) fn build_sub_log_detail(&mut self, p: Obj) {
        let th = t();
        let f = uf();
        let sc = column_page(p, false, false);
        let c = column_card(sc);
        let o = &mut self.op;
        o.logd_what = kit::label(c, f.cj17b, th.t1, 0, 0, "");
        o.logd_change = wrap_label(c, f.cj15, th.t1);
        o.logd_meta = kit::label(c, f.cj13, th.t3, 0, 0, "");
        o.logd_result = wrap_label(c, f.cj15, th.t1);
        (o.logd_btn, o.logd_btn_lbl) = kit::button(sc, 0, 0, CARD_W, 44, "", BtnKind::Plain, Some(logd_undo_cb), 0);
        o.logd_why = kit::label(sc, f.cj13, th.t3, 0, 0, "");
    }

    pub(super) fn op_logd_on_open(&mut self) {
        self.op.logd_arm = 0;
        let o = &self.op;
        if o.log_sel < 0 || o.log_sel >= o.log_n.max(0) {
            return;
        }
        let l = &o.log[o.log_sel as usize];
        o.logd_what.set_text(&l.what);
        o.logd_change.set_text(&l.change);
        kit::show(o.logd_change, !l.change.is_empty());
        o.logd_meta.set_text(format!("{} · {}", l.when, l.source));
        o.logd_result.set_text(&l.result);
        kit::text_color(o.logd_result, op_mark_col(l.mark));
        // DD10: no undo for writes that are not transactions (重启、eSIM …)
        kit::show(o.logd_btn, l.undo_have);
        kit::show(o.logd_why, l.undo_have && !l.undo_ok);
        if l.undo_have {
            o.logd_btn_lbl.set_text(if l.undo_label.is_empty() { tr("撤销") } else { l.undo_label.as_str() });
            kit::button_kind(o.logd_btn, o.logd_btn_lbl, BtnKind::Plain);
            o.logd_btn.set_style_opa(if l.undo_ok { lv::OPA_COVER } else { 102 }, 0);
            o.logd_why.set_text(&l.undo_why);
        }
    }

    /// The network-mode segment (lock.rs): datad's own verdict while it has a transaction for
    /// that item (replaces the screen's 20 s read-back guess), and its result if that ended
    /// (seen by this screen) at or after tick `since`.
    pub(super) fn op_mode_line(&self, since: u32) -> Option<String> {
        let (a, l) = (&self.op.view.active, &self.op.view.last);
        if a.have && a.item == "network.mode" {
            let left = self.op_left_ms(a);
            return Some(if left >= 0 && !self.op_stale() {
                format!("{} · {}", a.say, op_view::fill_clock("{t}", left))
            } else {
                a.say.clone()
            });
        }
        if since != 0 && l.have && l.item == "network.mode" && l.op_id == self.op.brief_id && self.op.brief_at.wrapping_sub(since) as i32 >= 0 {
            return Some(l.say.clone());
        }
        None
    }

    /// journal.list (POST /control, read at once: the list is shown from its answer).
    fn op_log_fetch(&mut self) -> bool {
        let Some(body) = self.src.journal_list(&self.model) else { return false };
        let Some(got) = op_view::log_parse(&body, op_view::LOG_MAX) else { return false };
        self.op.log_n = got.len() as i32;
        self.op.log = got;
        self.op.own_mode = op_view::owner_parse(&body, "network.mode").unwrap_or_default();
        true
    }

    // ---- 「上次改动」 under 网络模式 on the 蜂窝 tab (DD5, T15) ----
    pub(super) fn op_owner_paint(&mut self) {
        let show = self.op.own_mode.have;
        if show {
            let v = format!("{} · {}", self.op.own_mode.when, self.op.own_mode.source);
            if v != self.op.owner_val {
                self.cell.own_val.set_text(&v);
                self.op.owner_val = v;
            }
        }
        if show as i32 != self.op.owner_was {
            self.op.owner_was = show as i32;
            self.cell_own_layout(show);
        }
    }

    /// Read with the change log when the tab comes into view and whenever datad's op block
    /// changes while it is in view.
    fn op_owner_refresh(&mut self) {
        let v = &self.op.view;
        let k = format!("{}|{}|{}|{}", v.active.op_id, v.active.phase, v.last.op_id, v.last.phase);
        let vis = self.tab_visible(TAB_CELL);
        if vis && (!self.op.owner_seen || k != self.op.owner_key) {
            self.op.owner_key = k;
            if self.op_log_fetch() {
                self.op_owner_paint();
            }
        }
        self.op.owner_seen = vis;
    }

    // ---- DD8: write controls grey while datad is stuck ----
    fn op_guard_paint(&mut self) {
        let stuck = self.model.screen_stuck;
        if stuck == self.op.guard_was {
            return;
        }
        self.op.guard_was = stuck;
        let mut o: Vec<Obj> = self.lk.seg.item.clone();
        o.extend(self.lk.bg.iter().map(|g| g.apply_btn));
        o.push(self.lk.reset_btn);
        o.extend(self.cell.md_sw);
        o.extend(self.wifi.sw);
        o.push(self.sys.dps_sw);
        for x in o {
            if x.is_null() {
                continue;
            }
            if stuck != 0 {
                x.add_state(lv::LV_STATE_DISABLED);
            } else {
                x.remove_state(lv::LV_STATE_DISABLED);
            }
            x.set_style_opa(if stuck != 0 { lv::OPA_50 } else { lv::OPA_COVER }, 0);
        }
    }
}
