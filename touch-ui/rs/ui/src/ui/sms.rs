//! 短信 and 短信详情, 健康与告警 and its
//! 详情. The lists are drawn by refresh.
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use super::{Sub, Ui, SUB_VIEW, W};
use crate::cf;
use crate::clock;
use crate::kit::{self, BtnKind, CARD_W, MARGIN, PAD, ROW_H};
use crate::lang::{self, tr};
use crate::lv::{self, sys, Obj};
use crate::theme::{t, uf};

pub const SMS_MAX_ROWS: usize = 12;
const SMS_TOOLBAR_H: i32 = 44;
const SMS_ROW_H: i32 = 72;
const AL_ROW_H: i32 = 64;
const HC_ROW_H: i32 = 58;
const HC_TOP: i32 = 24;
/// the 「›」 at a row's end
const AL_CHEV_W: i32 = 16;
pub const ALERTS_MAX: usize = 12;
pub const HEALTH_MAX: usize = 8;
/// 事故 rows: open ones (at most 5) and those ended in the last 24 h (at most 3) (design review 14A).
pub const INC_MAX: usize = 8;
const INC_ROW_H: i32 = 50;
/// a row with 「同时观察到」 under its two lines
const INC_ROW_OBS_H: i32 = 66;
const INC_TOP: i32 = 24;

#[derive(Default)]
pub struct Sms {
    list: Obj,
    count: Obj,
    allread_btn: Obj,
    empty: Obj,
    row: [Obj; SMS_MAX_ROWS],
    num: [Obj; SMS_MAX_ROWS],
    date: [Obj; SMS_MAX_ROWS],
    body: [Obj; SMS_MAX_ROWS],
    dot: [Obj; SMS_MAX_ROWS],
    pub row_id: [i64; SMS_MAX_ROWS],
    // 短信详情
    pub smsd_id: i64,
    smsd_scroll: Obj,
    smsd_num: Obj,
    smsd_date: Obj,
    smsd_body: Obj,
    smsd_body_txt: String,
    smsd_del_btn: Obj,
    smsd_del_lbl: Obj,
    smsd_del_arm: u32,
    /// The current press already fired its long press: its own release click is not a tap.
    lp_fired: bool,
    // 健康与告警
    al_scroll: Obj,
    al_body: Obj,
    al_count: Obj,
    al_allread_btn: Obj,
    al_empty: Obj,
    al_list: Obj,
    pub al_row: [Obj; ALERTS_MAX],
    al_label: [Obj; ALERTS_MAX],
    al_time: [Obj; ALERTS_MAX],
    al_text: [Obj; ALERTS_MAX],
    al_mark: [Obj; ALERTS_MAX],
    hc_card: Obj,
    pub hc_row: [Obj; HEALTH_MAX],
    hc_mark: [Obj; HEALTH_MAX],
    hc_label: [Obj; HEALTH_MAX],
    hc_detail: [Obj; HEALTH_MAX],
    hc_none: Obj,
    // 事故 (design 2026-10-24): above 体检; the rest of the page moves with its height
    inc_note: Obj,
    inc_card: Obj,
    inc_none: Obj,
    inc_row: [Obj; INC_MAX],
    inc_mark: [Obj; INC_MAX],
    inc_label: [Obj; INC_MAX],
    inc_time: [Obj; INC_MAX],
    inc_obs: [Obj; INC_MAX],
    inc_more: Obj,
    al_rest: Obj,
    /// what the section was last laid out as (rows' heights, note, more), so a quiet refresh moves nothing
    inc_layout: Vec<i32>,
    // its 详情
    ald_scroll: Obj,
    ald_title: Obj,
    ald_meta: Obj,
    ald_body: Obj,
}

/// As much of `src` as fits in `cap` bytes without splitting a UTF-8 character.
pub fn utf8_prefix(src: &str, cap: usize) -> &str {
    let mut n = src.len();
    if n >= cap {
        n = cap - 1;
        while n > 0 && !src.is_char_boundary(n) {
            n -= 1;
        }
    }
    &src[..n]
}

unsafe extern "C" fn sms_row_click_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let i = unsafe { lv::event_user(e) };
    super::with(|u| u.sms_row_click(i));
}

unsafe extern "C" fn sms_row_longpress_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let i = unsafe { lv::event_user(e) };
    super::with(|u| {
        u.sms.lp_fired = true;
        u.sms_delete_arm(i);
    });
}

unsafe extern "C" fn sms_row_pressed_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| u.sms.lp_fired = false);
}

unsafe extern "C" fn sms_allread_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| {
        u.src.sms_mark_all_read();
        u.sync();
    });
}

unsafe extern "C" fn smsd_delete_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| u.smsd_delete());
}

unsafe extern "C" fn al_allread_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| {
        u.src.alerts_mark_all_read();
        u.sync();
    });
}

unsafe extern "C" fn hc_row_click_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let i = unsafe { lv::event_user(e) };
    super::with(|u| u.hc_row_click(i));
}

unsafe extern "C" fn al_row_click_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let i = unsafe { lv::event_user(e) };
    super::with(|u| u.al_row_click(i));
}

/// A list page's first line: "N 条未读 · 共 M 条" and a 全部已读 pill.
fn list_toolbar(p: Obj, cb: lv::EventCb) -> (Obj, Obj) {
    let count = kit::label(p, uf().cj13, t().t2, MARGIN + 6, 14, "");
    let (btn, l) = kit::button(p, 0, 8, 0, 30, tr("全部已读"), BtnKind::Plain, Some(cb), 0);
    l.set_style_text_font(uf().cj13, 0);
    btn.align(lv::LV_ALIGN_TOP_RIGHT, -MARGIN, 8);
    (count, btn)
}

/// The detail pages: a scrolling flex column with one card in it.
fn detail_scroll(p: Obj, row_gap: Option<i32>) -> Obj {
    let sc = Obj::create(p);
    sc.remove_style_all();
    sc.set_size(W, SUB_VIEW);
    sc.set_scroll_dir(sys::lv_dir_t::LV_DIR_VER);
    sc.set_scrollbar_mode(sys::lv_scrollbar_mode_t::LV_SCROLLBAR_MODE_ACTIVE);
    sc.flex_column();
    sc.flex_align_start_center();
    sc.set_style_pad_top(4, 0);
    sc.set_style_pad_bottom(24, 0);
    if let Some(g) = row_gap {
        sc.set_style_pad_row(g, 0);
    }
    sc
}

/// The card of a detail page: fits its content, a column with 6 px between lines.
fn detail_card(sc: Obj) -> Obj {
    let c = kit::card(sc, 0, 0, CARD_W, 10);
    c.set_height(lv::SIZE_CONTENT);
    c.set_style_pad_all(PAD, 0);
    c.set_style_pad_bottom(18, 0);
    c.flex_column();
    c.set_style_pad_row(6, 0);
    c
}

impl Ui {
    pub(super) fn build_sub_sms(&mut self, p: Obj) {
        let th = t();
        let f = uf();
        let sc = kit::scroll(p, 0, SUB_VIEW, SMS_TOOLBAR_H + SMS_MAX_ROWS as i32 * SMS_ROW_H + 16);
        let s = &mut self.sms;
        (s.count, s.allread_btn) = list_toolbar(sc, sms_allread_cb);
        s.empty = kit::label_w(sc, f.cj14, th.t3, MARGIN + 6, SMS_TOOLBAR_H + 4, CARD_W - 12, true, tr("没有短信。新短信会显示在这里，点开可看全文。"));
        kit::show(s.empty, false);
        s.list = kit::card(sc, MARGIN, SMS_TOOLBAR_H, CARD_W, SMS_MAX_ROWS as i32 * SMS_ROW_H);
        s.list.set_style_clip_corner(true, 0);
        for i in 0..SMS_MAX_ROWS {
            s.row_id[i] = -1;
            let c = kit::bx(s.list, 0, i as i32 * SMS_ROW_H, CARD_W, SMS_ROW_H, th.card, 0);
            kit::tappable(c, Some(sms_row_click_cb), i);
            c.add_event_cb(sms_row_longpress_cb, lv::LV_EVENT_LONG_PRESSED, i);
            c.add_event_cb(sms_row_pressed_cb, lv::LV_EVENT_PRESSED, i);
            s.row[i] = c;
            if i > 0 {
                kit::sep(c, 0);
            }
            s.dot[i] = kit::dot(c, 8, 16, 6, th.blue);
            s.num[i] = kit::label_w(c, f.cj15b, th.t1, 20, 9, 170, false, "");
            s.date[i] = kit::label_r(c, f.n12, th.t3, CARD_W - PAD, 11, "");
            s.body[i] = kit::label_w(c, f.cj13, th.t2, 20, 31, CARD_W - 20 - PAD, true, "");
            s.body[i].set_height(2 * f.cj13.line_height());
            s.body[i].set_long_mode(lv::LV_LABEL_LONG_MODE_DOTS);
            kit::show(c, false);
        }
        s.smsd_id = -1;
        sc.scroll_to_y(0);
    }

    pub(super) fn build_sub_alerts(&mut self, p: Obj) {
        let th = t();
        let f = uf();
        let inc_max_h = INC_TOP + 20 + INC_MAX as i32 * INC_ROW_OBS_H + 30;
        let sc = kit::scroll(
            p,
            0,
            SUB_VIEW,
            inc_max_h + HC_TOP + HEALTH_MAX as i32 * HC_ROW_H + 30 + SMS_TOOLBAR_H + ALERTS_MAX as i32 * AL_ROW_H + 16,
        );
        let s = &mut self.sms;
        s.al_scroll = sc;
        // 事故: rows not tappable (details are on the web, design review 11A)
        kit::section(sc, 4, tr("事故"));
        s.inc_note = kit::label_w(sc, f.cj12, th.t3, MARGIN + 6, INC_TOP, CARD_W - 12, false, "");
        kit::show(s.inc_note, false);
        s.inc_card = kit::card(sc, MARGIN, INC_TOP, CARD_W, ROW_H);
        s.inc_none = kit::label_w(s.inc_card, f.cj14, th.t2, PAD, 11, CARD_W - 2 * PAD, false, tr("读取中…"));
        for i in 0..INC_MAX {
            let c = kit::bx(s.inc_card, 0, i as i32 * INC_ROW_H, CARD_W, INC_ROW_H, th.card, 0);
            c.set_style_bg_opa(lv::OPA_TRANSP, 0);
            s.inc_row[i] = c;
            if i > 0 {
                kit::sep(c, 0);
            }
            s.inc_mark[i] = kit::label(c, f.cj12, th.warn_t, PAD, 10, "▲");
            s.inc_label[i] = kit::label_w(c, f.cj14, th.t1, PAD + 18, 7, CARD_W - 2 * PAD - 18, false, "");
            s.inc_time[i] = kit::label_w(c, f.cj12, th.t2, PAD + 18, 28, CARD_W - 2 * PAD - 18, false, "");
            s.inc_obs[i] = kit::label_w(c, f.cj12, th.t2, PAD + 18, 46, CARD_W - 2 * PAD - 18, false, "");
            s.inc_obs[i].set_height(f.cj12.line_height());
            s.inc_obs[i].set_long_mode(lv::LV_LABEL_LONG_MODE_DOTS);
            kit::show(s.inc_obs[i], false);
            kit::show(c, false);
        }
        s.inc_more = kit::label_w(sc, f.cj12, th.t3, MARGIN + 6, 0, CARD_W - 12, false, "");
        kit::show(s.inc_more, false);
        // 体检 and 告警记录 below it, in one block that moves with 事故's height
        let rest = kit::bx(sc, 0, INC_TOP + ROW_H + 10, W, HC_TOP + HEALTH_MAX as i32 * HC_ROW_H + 30 + SMS_TOOLBAR_H + ALERTS_MAX as i32 * AL_ROW_H, th.bg, 0);
        rest.set_style_bg_opa(lv::OPA_TRANSP, 0);
        s.al_rest = rest;
        let sc = rest;
        kit::section(sc, 4, tr("体检"));
        s.hc_card = kit::card(sc, MARGIN, HC_TOP, CARD_W, ROW_H);
        s.hc_none = kit::label_w(s.hc_card, f.cj14, th.t2, PAD, 11, CARD_W - 2 * PAD, false, tr("读取中…"));
        for i in 0..HEALTH_MAX {
            let c = kit::bx(s.hc_card, 0, i as i32 * HC_ROW_H, CARD_W, HC_ROW_H, th.card, 0);
            c.set_style_bg_opa(lv::OPA_TRANSP, 0);
            kit::tappable(c, Some(hc_row_click_cb), i);
            s.hc_row[i] = c;
            if i > 0 {
                kit::sep(c, 0);
            }
            s.hc_mark[i] = kit::label(c, f.cj12, th.warn_t, PAD, 11, "▲");
            s.hc_label[i] = kit::label_w(c, f.cj14, th.t1, PAD + 18, 9, CARD_W - 2 * PAD - 18 - AL_CHEV_W, false, "");
            s.hc_detail[i] = kit::label_w(c, f.cj12, th.t2, PAD + 18, 31, CARD_W - 2 * PAD - 18 - AL_CHEV_W, false, "");
            kit::label_r(c, f.cj15, th.t3, CARD_W - PAD, 18, "›");
            s.hc_detail[i].set_height(f.cj12.line_height());
            s.hc_detail[i].set_long_mode(lv::LV_LABEL_LONG_MODE_DOTS);
            kit::show(c, false);
        }
        // 告警记录: the whole block moves with the health card's height
        let b = kit::bx(sc, 0, HC_TOP + ROW_H + 10, W, SMS_TOOLBAR_H + ALERTS_MAX as i32 * AL_ROW_H, th.bg, 0);
        b.set_style_bg_opa(lv::OPA_TRANSP, 0);
        s.al_body = b;
        (s.al_count, s.al_allread_btn) = list_toolbar(b, al_allread_cb);
        s.al_empty = kit::label_w(b, f.cj14, th.t3, MARGIN + 6, SMS_TOOLBAR_H + 4, CARD_W - 12, true, "");
        s.al_list = kit::card(b, MARGIN, SMS_TOOLBAR_H, CARD_W, ALERTS_MAX as i32 * AL_ROW_H);
        for i in 0..ALERTS_MAX {
            let c = kit::bx(s.al_list, 0, i as i32 * AL_ROW_H, CARD_W, AL_ROW_H, th.card, 0);
            c.set_style_bg_opa(lv::OPA_TRANSP, 0);
            kit::tappable(c, Some(al_row_click_cb), i);
            s.al_row[i] = c;
            if i > 0 {
                kit::sep(c, 0);
            }
            s.al_mark[i] = kit::label(c, f.cj12, th.warn_t, PAD, 12, "▲");
            s.al_label[i] = kit::label_w(c, f.cj14, th.t1, PAD + 18, 10, CARD_W - 2 * PAD - 18 - AL_CHEV_W, false, "");
            s.al_time[i] = kit::label_r(c, f.n12, th.t3, CARD_W - PAD - AL_CHEV_W, 36, "");
            s.al_text[i] = kit::label_w(c, f.n11, th.t3, PAD + 18, 37, CARD_W - 2 * PAD - 18 - 90 - AL_CHEV_W, false, "");
            s.al_text[i].set_height(f.n11.line_height());
            s.al_text[i].set_long_mode(lv::LV_LABEL_LONG_MODE_DOTS);
            kit::label_r(c, f.cj15, th.t3, CARD_W - PAD, 20, "›");
            kit::show(c, false);
        }
        sc.scroll_to_y(0);
    }

    /// Like 短信详情: one card, title + kind/time + the whole text.
    pub(super) fn build_sub_alert_detail(&mut self, p: Obj) {
        let th = t();
        let f = uf();
        let sc = detail_scroll(p, None);
        self.sms.ald_scroll = sc;
        let c = detail_card(sc);
        let s = &mut self.sms;
        s.ald_title = kit::label(c, f.cj17b, th.t1, 0, 0, "");
        s.ald_title.set_width(lv::pct(100));
        s.ald_title.set_long_mode(lv::LV_LABEL_LONG_MODE_WRAP);
        s.ald_meta = kit::label(c, f.cj13, th.t3, 0, 0, "");
        s.ald_body = kit::label(c, f.cj15, th.t1, 0, 0, "");
        s.ald_body.set_width(lv::pct(100));
        s.ald_body.set_style_text_line_space(6, 0);
        s.ald_body.set_long_mode(lv::LV_LABEL_LONG_MODE_WRAP);
    }

    pub(super) fn build_sub_sms_detail(&mut self, p: Obj) {
        let th = t();
        let f = uf();
        let sc = detail_scroll(p, Some(10));
        self.sms.smsd_scroll = sc;
        let c = detail_card(sc);
        let s = &mut self.sms;
        s.smsd_num = kit::label(c, f.cj17b, th.t1, 0, 0, "");
        s.smsd_num.set_width(lv::pct(100)); // a long sender wraps, not off the card
        s.smsd_num.set_long_mode(lv::LV_LABEL_LONG_MODE_WRAP);
        s.smsd_date = kit::label(c, f.n12, th.t3, 0, 0, "");
        s.smsd_body = kit::label(c, f.cj15, th.t1, 0, 0, "");
        s.smsd_body.set_width(lv::pct(100));
        s.smsd_body.set_style_text_line_space(6, 0);
        s.smsd_body.set_long_mode(lv::LV_LABEL_LONG_MODE_WRAP);
        (s.smsd_del_btn, s.smsd_del_lbl) = kit::button(sc, 0, 0, CARD_W, 40, tr("删除这条"), BtnKind::Danger, Some(smsd_delete_cb), 0);
    }

    // ---- taps ----
    /// A long press arms the row (the data side keeps it by message id: a list re-read in
    /// between does not move it); a second tap after the debounce deletes.
    fn sms_delete_arm(&mut self, i: usize) {
        self.src.sms_delete_arm(i);
        self.sync();
    }

    fn sms_delete_armed(&self, i: usize) -> bool {
        self.src.sms_delete_armed(&self.model, i)
    }

    fn sms_delete_tap(&mut self, i: usize) -> bool {
        let r = self.src.sms_delete_tap(i);
        self.sync();
        r
    }

    fn sms_row_click(&mut self, i: usize) {
        // LVGL still sends CLICKED when a long press is released: holding past the
        // debounce must not turn the arming press into the delete
        if std::mem::take(&mut self.sms.lp_fired) {
            return;
        }
        if self.sms_delete_tap(i) {
            return; // second tap on an armed row: deleted
        }
        if self.sms.row_id[i] < 0 {
            return;
        }
        // open it: reading it in full is what marks it read
        self.sms.smsd_id = self.sms.row_id[i];
        self.sms.smsd_del_arm = 0;
        let id = self.sms.smsd_id;
        self.src.sms_mark_read_id(id);
        self.sync();
        self.sms.smsd_scroll.scroll_to_y(0);
        self.sub_open_child(Sub::SmsDetail, Sub::Sms);
    }

    /// Two taps within 4 s.
    fn smsd_delete(&mut self) {
        let now = lv::tick_get();
        let a = self.sms.smsd_del_arm;
        if a != 0 && now.wrapping_sub(a) > 300 && now.wrapping_sub(a) < 4000 {
            let id = self.sms.smsd_id;
            self.src.sms_delete_id(id);
            self.sync();
            self.sms.smsd_del_arm = 0;
            self.sms.smsd_id = -1;
            self.sub_back();
            return;
        }
        self.sms.smsd_del_arm = now;
    }

    /// A row in full: the list has room for one line of a check's note or an alert's text.
    /// What it says is copied at the tap.
    fn alert_detail_open(&mut self, title: &str, meta: &[u8], body: &str) {
        let s = &self.sms;
        let (t1, m1, b1, sc) = (s.ald_title, s.ald_meta, s.ald_body, s.ald_scroll);
        self.set(t1, title);
        self.set(m1, meta);
        self.set(b1, if body.is_empty() { tr("（没有更多说明）") } else { body });
        sc.scroll_to_y(0);
        self.sub_open_child(Sub::AlertDetail, Sub::Alerts);
    }

    fn hc_row_click(&mut self, i: usize) {
        let Some(h) = self.model.health.get(i).cloned() else { return };
        let meta = if h.note != 0 {
            tr("● 提示")
        } else if h.bad != 0 {
            tr("■ 体检：异常")
        } else {
            tr("▲ 体检：需要注意")
        };
        self.alert_detail_open(&h.label, meta.as_bytes(), &h.detail);
    }

    fn al_row_click(&mut self, i: usize) {
        let Some(a) = self.model.alerts.get(i).cloned() else { return };
        let meta = if a.time > 0 {
            // device clock = local time labelled UTC: local() gives the right digits
            let when = clock::strftime(if lang::is_en() { "%d %b %H:%M:%S" } else { "%Y-%m-%d %H:%M:%S" }, &clock::local(a.time));
            cf!(tr("告警 · %s"), &when)
        } else {
            cf!(tr("告警 · 开机后 %ld 分钟"), a.uptime / 60)
        };
        self.alert_detail_open(&a.label, &meta, &a.text);
    }

    // ---- the refresh ----
    /// 短信 and 短信详情.
    pub(super) fn sms_paint(&mut self) {
        let th = t();
        let m = self.model.clone();
        let d = &m.data;
        let sms_n = d.inbox_n.max(0) as usize;
        let n = sms_n.min(SMS_MAX_ROWS);
        let unread = d.inbox.iter().take(sms_n).filter(|s| s.unread != 0).count();
        let s = &self.sms;
        let (count, btn, empty, list) = (s.count, s.allread_btn, s.empty, s.list);
        if sms_n == 0 {
            self.set(count, "");
        } else if unread > 0 {
            self.set(count, cf!(tr("%d 条未读 · 共 %d 条"), unread, d.inbox_n));
        } else {
            self.set(count, cf!(tr("共 %d 条，都已读"), d.inbox_n));
        }
        kit::show(btn, unread > 0);
        kit::show(empty, sms_n == 0);
        kit::show(list, n > 0);
        if n > 0 {
            list.set_height(n as i32 * SMS_ROW_H);
        }
        for i in 0..SMS_MAX_ROWS {
            let s = &self.sms;
            let (row, num, date, body, dot) = (s.row[i], s.num[i], s.date[i], s.body[i], s.dot[i]);
            if i >= n {
                self.sms.row_id[i] = -1;
                kit::show(row, false);
                continue;
            }
            let it = &d.inbox[i];
            self.sms.row_id[i] = it.id;
            kit::show(row, true);
            self.set(num, &it.num);
            self.set(date, &it.date);
            // only two lines show: cut on a UTF-8 boundary (the label adds the "…")
            self.set(body, utf8_prefix(&it.text, 150));
            kit::show(dot, it.unread != 0);
            kit::bg(row, if self.sms_delete_armed(i) { th.wash_b } else { th.card });
        }

        // 短信详情
        if self.sub_cur == Some(Sub::SmsDetail) {
            let k = d.inbox.iter().take(sms_n).position(|s| s.id == self.sms.smsd_id);
            match k {
                None => {
                    // deleted (here or elsewhere) while open: nothing left to show
                    self.sms.smsd_id = -1;
                    self.sub_back();
                }
                Some(k) => {
                    let it = &d.inbox[k];
                    let s = &self.sms;
                    let (num, date, body, lbl, btn) = (s.smsd_num, s.smsd_date, s.smsd_body, s.smsd_del_lbl, s.smsd_del_btn);
                    self.set(num, &it.num);
                    self.set(date, &it.date);
                    // the full text: too big for set()
                    if self.sms.smsd_body_txt != it.text {
                        self.sms.smsd_body_txt = it.text.clone();
                        body.set_text(&it.text);
                    }
                    let a = self.sms.smsd_del_arm;
                    let armed = a != 0 && lv::tick_get().wrapping_sub(a) < 4000;
                    if !armed {
                        self.sms.smsd_del_arm = 0;
                    }
                    self.set(lbl, if armed { tr("再点一次确认删除") } else { tr("删除这条") });
                    kit::button_kind(btn, lbl, if armed { BtnKind::Armed } else { BtnKind::Danger });
                }
            }
        }
    }

    /// the health checks that are not fine, then the alert log.
    /// 事故 (design review 3A/4A/11A/14A; round 3 D8/D11): open ones, then those ended in 24 h
    /// (the core's `regress` block chose them); stale = the last rows in grey and when they
    /// stopped; never read / an older core said in words, never hidden.
    pub(super) fn incidents_paint(&mut self) {
        let th = t();
        let m = self.model.clone();
        let r = m.data.regress.as_ref();
        let fresh = r.is_some_and(|r| r.fresh != 0 && r.has != 0) && m.backend_silent == 0;
        let rows: Vec<&ui_model::regress_item_t> = r.map_or_else(Vec::new, |r| r.open.iter().chain(r.closed.iter()).take(INC_MAX).collect());
        let note: Vec<u8> = match r {
            Some(r) if r.has != 0 && !fresh => {
                let when = clock::strftime("%H:%M", &clock::local(r.at));
                cf!(tr("事故数据停在 %s"), &when)
            }
            _ => Vec::new(),
        };
        let none: &str = match r {
            None => tr("core 版本太旧，不支持事故"),
            Some(r) if r.has == 0 => tr("读不到事故数据"),
            _ => tr("● 24 小时内没有事故"),
        };
        let now = self.now();
        let mut heights = Vec::with_capacity(INC_MAX + 3);
        for i in 0..INC_MAX {
            let s = &self.sms;
            let (row, mark, label, time, obs) = (s.inc_row[i], s.inc_mark[i], s.inc_label[i], s.inc_time[i], s.inc_obs[i]);
            let Some(it) = rows.get(i) else {
                kit::show(row, false);
                continue;
            };
            kit::show(row, true);
            let open = it.end.is_empty();
            let state: Vec<u8> = match it.end.as_str() {
                "" => tr("还开着").into(),
                "quiet" => cf!(tr("已结束 · %d 次"), it.count),
                _ => tr("已恢复").into(),
            };
            let line1 = [it.name.as_bytes(), " · ".as_bytes(), &state].concat();
            let hm = |t: i64| -> Vec<u8> {
                if t <= 0 {
                    return tr("时间不可信").into();
                }
                let f = if now - t > 20 * 3600 { if lang::is_en() { "%d %b %H:%M" } else { "%m-%d %H:%M" } } else { "%H:%M" };
                clock::strftime(f, &clock::local(t)).into()
            };
            let mut line2: Vec<u8> = if open {
                cf!(tr("%s 起"), &hm(it.from))
            } else {
                [hm(it.from), "–".as_bytes().to_vec(), clock::strftime("%H:%M", &clock::local(it.to)).into()].concat()
            };
            match it.sub.as_str() {
                "chill" => line2.extend_from_slice(" · CHILL".as_bytes()),
                "tailscale" => line2.extend_from_slice(" · Tailscale".as_bytes()),
                _ => {}
            }
            let blind = it.kind == "regress-blind";
            let (sym, col) = if !fresh {
                (if open { "▲" } else { "●" }, th.t3)
            } else if open && !blind {
                ("▲", th.warn_t)
            } else if open {
                ("●", th.t2)
            } else {
                ("●", th.ok_t)
            };
            self.set(mark, sym);
            kit::text_color(mark, col);
            self.set(label, &line1);
            kit::text_color(label, if fresh { th.t1 } else { th.t3 });
            self.set(time, &line2);
            kit::text_color(time, if fresh { th.t2 } else { th.t3 });
            let has_obs = !it.observed.is_empty();
            if has_obs {
                self.set(obs, &it.observed);
            }
            kit::show(obs, has_obs);
            heights.push(if has_obs { INC_ROW_OBS_H } else { INC_ROW_H });
        }
        let more = r.map_or(0, |r| (r.open_total - r.open.len() as i32).max(0));
        heights.push(!note.is_empty() as i32);
        heights.push(more);
        heights.push(rows.is_empty() as i32);
        let s = &self.sms;
        let (note_l, card, none_l, more_l, rest) = (s.inc_note, s.inc_card, s.inc_none, s.inc_more, s.al_rest);
        if !note.is_empty() {
            self.set(note_l, &note);
        }
        if rows.is_empty() {
            self.set(none_l, none);
        }
        if more > 0 {
            self.set(more_l, cf!(tr("还有 %d 个开着"), more));
        }
        if self.sms.inc_layout == heights {
            return;
        }
        // lay out only when something moved (a quiet refresh invalidates nothing)
        let note_h = if note.is_empty() { 0 } else { 20 };
        kit::show(note_l, note_h > 0);
        let card_y = INC_TOP + note_h;
        let mut y = 0;
        for (i, &h) in heights.iter().take(rows.len()).enumerate() {
            self.sms.inc_row[i].set_y(y);
            self.sms.inc_row[i].set_height(h);
            y += h;
        }
        let card_h = if rows.is_empty() { ROW_H } else { y };
        kit::show(none_l, rows.is_empty());
        card.set_y(card_y);
        card.set_height(card_h);
        let more_h = if more > 0 { 22 } else { 0 };
        kit::show(more_l, more > 0);
        more_l.set_y(card_y + card_h + 4);
        rest.set_y(card_y + card_h + more_h + 10);
        self.sms.inc_layout = heights;
    }

    pub(super) fn alerts_paint(&mut self) {
        let th = t();
        let m = self.model.clone();
        let n = m.alerts.len();
        let un = m.alerts_unread;
        let err = m.alerts_error.as_deref().unwrap_or("");
        {
            let hc = self.src.health_count(&m);
            let hn = hc.max(0) as usize;
            for i in 0..HEALTH_MAX {
                let s = &self.sms;
                let (row, mark, label, detail) = (s.hc_row[i], s.hc_mark[i], s.hc_label[i], s.hc_detail[i]);
                if i >= hn {
                    kit::show(row, false);
                    continue;
                }
                let h = &m.health[i];
                kit::show(row, true);
                mark.set_text(if h.note != 0 { "●" } else if h.bad != 0 { "■" } else { "▲" });
                kit::text_color(mark, if h.note != 0 { th.t2 } else if h.bad != 0 { th.bad_t } else { th.warn_t });
                self.set(label, &h.label);
                self.set(detail, &h.detail);
            }
            let none = self.sms.hc_none;
            if hc == -2 {
                self.set(none, tr("读不到体检结果（管理后台没响应）"));
            } else if hc < 0 {
                self.set(none, tr("体检结果读取中…"));
            } else {
                self.set(none, cf!(tr("● %d 项检查都正常"), m.health_checked));
            }
            kit::show(none, hn == 0);
            let hh = if hn > 0 { hn as i32 * HC_ROW_H } else { ROW_H };
            self.sms.hc_card.set_height(hh);
            self.sms.al_body.set_y(HC_TOP + hh + 10);
        }
        let s = &self.sms;
        let (count, btn, empty, list) = (s.al_count, s.al_allread_btn, s.al_empty, s.al_list);
        if !err.is_empty() {
            self.set(count, err);
        } else if n == 0 {
            self.set(count, tr("告警记录"));
        } else if un != 0 {
            self.set(count, cf!(tr("告警记录 · %d 条未读"), un));
        } else {
            self.set(count, tr("告警记录 · 都已读"));
        }
        kit::show(btn, un != 0 && err.is_empty());
        if n == 0 && err.is_empty() {
            empty.set_text(tr("没有告警记录。程序崩溃、Wi-Fi 被看门狗打开这类事会记在这里。"));
            kit::show(empty, true);
        } else {
            kit::show(empty, false);
        }
        kit::show(list, n > 0);
        if n > 0 {
            list.set_height(n.min(ALERTS_MAX) as i32 * AL_ROW_H);
        }
        for i in 0..ALERTS_MAX {
            let s = &self.sms;
            let (row, label, time, text, mark) = (s.al_row[i], s.al_label[i], s.al_time[i], s.al_text[i], s.al_mark[i]);
            if i >= n {
                kit::show(row, false);
                continue;
            }
            let a = &m.alerts[i];
            kit::show(row, true);
            let when: Vec<u8> = if a.time > 0 {
                clock::strftime(if lang::is_en() { "%d %b %H:%M" } else { "%m-%d %H:%M" }, &clock::local(a.time)).into()
            } else {
                cf!(tr("开机后%ld分"), a.uptime / 60)
            };
            self.set(label, &a.label);
            self.set(time, &when);
            self.set(text, &a.text);
            kit::show(mark, a.unread != 0);
            kit::text_color(label, if a.unread != 0 { th.t1 } else { th.t2 });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::utf8_prefix;

    /// utf8_prefix: never a split character
    #[test]
    fn prefix_keeps_whole_characters() {
        assert_eq!(utf8_prefix("abc", 150), "abc");
        assert_eq!(utf8_prefix("abcdef", 4), "abc");
        // 中 is 3 bytes: a 5-byte buffer holds 4 bytes, one character
        assert_eq!(utf8_prefix("中文", 5), "中");
        assert_eq!(utf8_prefix("中文", 7), "中文");
        assert_eq!(utf8_prefix("中文", 3), "");
    }
}
