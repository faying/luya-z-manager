//! src/ui_parts/diagnose.c: 网络诊断 and 摆放模式 (manager docs/designs/slow-diagnosis.md §4.2,
//! §12.2–12.5).
//!
//! 网络诊断: luyaz-core runs it (about 10 s, ≤ 20 s); the page starts it, reads it every second
//! while it runs and draws what it says. The run is kept 10 minutes: coming back within that
//! shows it with when it was measured; after that, opening the page measures again. Status
//! block = the main cause and one thing to do; a row per layer; then 加测速度 and 对 / 不对.
//!
//! 摆放模式: moving a device that stays put (a window sill, a shelf): the main radio's SINR as a
//! 3-second median, big; the best this visit and how far off it we are now. A new cell starts
//! the best over. The screen stays on while the page is up.
// SPDX-License-Identifier: MIT

use super::{Sub, Ui, SUB_VIEW, TAB_CELL};
use crate::cf;
use crate::clock;
use crate::diag_view::{self as dv, DG_BAD, DG_DONE, DG_IDLE, DG_INFO, DG_NA, DG_NONE, DG_OK, DG_PENDING, DG_RUN, DG_RUNNING, DG_WAITING, DG_WARN};
use crate::kit::{self, BtnKind, Hero, CARD_W, HERO_H, MARGIN, PAD, ROW_H, W};
use crate::lang::tr;
use crate::logic::{self, Arm, Rat};
use crate::lv::{self, sys, Obj};
use crate::theme::{t, uf};
use devui_model::{dg_layer_t, diag_run_t};

/// the layers + the speed row
const DG_ROWS: usize = dv::DG_LAYERS_MAX + 1;
const DG_ARM_MS: u32 = 5000;
/// [`Arm`] ids: 0 = the roaming speed test, this = the proposal's redial
const DG_ARM_REDIAL: i32 = 1;
/// [`Arm`] id: 恢复上次能用的设置
const DG_ARM_RESTORE: i32 = 2;
const DG_NOTE_MS: u32 = 4000;
const DG_ACT_NONE: i32 = 0;
const DG_ACT_PLACE: i32 = 1;
const DG_ACT_PROXY: i32 = 2;
const PL_SAMPLES: usize = 3;

#[derive(Default, Clone, Copy)]
pub struct DgRow {
    bx: Obj,
    sep: Obj,
    name: Obj,
    val: Obj,
    detail: Obj,
    pub act: Obj,
    act_to: i32,
}

#[derive(Default)]
pub struct Diag {
    pub row: [DgRow; DG_ROWS],
    hero: Hero,
    scroll: Obj,
    card: Obj,
    pub btn: Obj,
    btn_lbl: Obj,
    /// redial for the core's proposal (decision loop §4.10), two taps
    pub rd: Obj,
    rd_lbl: Obj,
    /// 恢复上次能用的设置 (core `/api/verified`), two taps; its line under it
    pub rs: Obj,
    rs_lbl: Obj,
    rs_say: Obj,
    /// the core's line on a strong but slow signal, read-only (no ›)
    slow: Obj,
    note: Obj,
    fb: Obj,
    fb_lbl: Obj,
    pub yes: Obj,
    no: Obj,
    pub hdr: Obj,
    pub hdr_lbl: Obj,
    /// the start tap painted 正在检查…, the reply not in yet
    pending: bool,
    /// roaming: the first tap on 加测速度
    arm: Arm,
    note_at: u32,
    /// when our speed request went out (for 「3 秒」)
    speed_t0: u32,
    /// 对 / 不对 tapped for this run id
    fb_local: i32,
    fb_id: i64,
    /// roaming now (the core's view), for the speed confirm
    pub net_roam: bool,
    tone: i32,
}

#[derive(Default)]
pub struct Place {
    cap: Obj,
    big: Obj,
    word: Obj,
    note: Obj,
    best_l: Obj,
    cmp: Obj,
    cell: Obj,
    btn: Obj,
    /// SINR in tenths of a dB, the last 3 seconds
    v: [i32; PL_SAMPLES],
    n: usize,
    head: usize,
    /// core snapshot the last sample came from
    ver: u64,
    best: i32,
    have_best: bool,
    best_at: i64,
    /// RAT · band · PCI the best belongs to
    key: String,
    /// 「换了小区，重新计」 shown since
    reset_at: u32,
    /// the reset was the button
    manual: bool,
    col: Option<u32>,
}

unsafe extern "C" fn dg_start_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| u.dg_start_tap());
}

unsafe extern "C" fn dg_redial_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| u.dg_redial_tap());
}

unsafe extern "C" fn dg_restore_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| u.dg_restore_tap());
}

/// A restored item's name (core `verified::ITEMS`).
fn dg_item_name(item: &str) -> &'static str {
    match item {
        "cell.lte" => tr("LTE 锁小区"),
        "cell.nr" => tr("NR 锁小区"),
        "band.lte" => tr("LTE 频段"),
        "band.nr" => tr("NR 频段"),
        "network.mode" => tr("网络模式"),
        "apn" => "APN",
        _ => tr("其他设置"),
    }
}

unsafe extern "C" fn dg_speed_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| u.dg_speed_tap());
}

unsafe extern "C" fn dg_fb_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let right = unsafe { lv::event_user(e) };
    super::with(|u| u.dg_fb_tap(right as i32));
}

unsafe extern "C" fn dg_act_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let k = unsafe { lv::event_user(e) };
    super::with(|u| u.dg_act_tap(k));
}

unsafe extern "C" fn pl_reset_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| u.pl_reset());
}

fn dg_going(r: &diag_run_t) -> bool {
    r.state == DG_WAITING || r.state == DG_RUN
}

/// device clock seconds → "14:32" (local time labelled UTC: local()'s digits)
fn dg_hm(t: i64) -> String {
    if t > 0 { clock::strftime("%H:%M", &clock::local(t)) } else { "--:--".into() }
}

/// "17.7" / "-2.5" → tenths; None = no number
fn pl_tenths(s: &str) -> Option<i32> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    let st = i;
    if i < b.len() && matches!(b[i], b'+' | b'-') {
        i += 1;
    }
    let d0 = i;
    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
        i += 1;
    }
    if i == d0 {
        return None;
    }
    let v: f64 = s[st..i].parse().ok()?;
    Some((v * 10.0 + if v < 0.0 { -0.5 } else { 0.5 }) as i32)
}

/// tenths → "17.7" / "-2.5"
fn pl_fmt(t: i32) -> String {
    let a = t.abs();
    format!("{}{}.{}", if t < 0 { "-" } else { "" }, a / 10, a % 10)
}

impl Ui {
    fn dg_note(&mut self, t: &str, col: u32) {
        let mut c = t.as_bytes().to_vec();
        c.truncate(191);
        let n = self.dg.note;
        n.set_text(&c);
        kit::text_color(n, col);
        self.dg.note_at = if c.is_empty() { 0 } else { lv::tick_get().max(1) };
    }

    fn dg_run(&self) -> diag_run_t {
        self.model.diagnose_run.clone().unwrap_or_default()
    }

    /// 正在检查… painted first, then the request (it is synchronous).
    fn dg_start(&mut self) {
        let th = t();
        self.dg.pending = true;
        self.dg.arm.clear();
        self.dg_note("", th.t3);
        self.diag_paint();
        lv::refr_now();
        let ok = self.src.diagnose_start();
        self.sync();
        self.dg.pending = false;
        if !ok && self.model.diagnose_agent_err == 0 {
            let e = self.model.diagnose_error.clone().unwrap_or_default();
            self.dg_note(&e, th.bad_t);
        }
        self.diag_paint();
    }

    fn dg_start_tap(&mut self) {
        if dg_going(&self.dg_run()) && self.model.diagnose_agent_err == 0 {
            self.dg_note(tr("正在查，稍等"), t().t2);
            self.diag_paint();
            return;
        }
        self.dg_start();
    }

    /// Opening the page: within 10 minutes of the last run show it; else measure.
    pub(super) fn diag_on_open(&mut self) {
        // the read is synchronous: draw what there is first, so the tap is answered
        self.diag_paint();
        lv::refr_now();
        self.src.diagnose_kick();
        self.src.diagnose_poll(true);
        self.sync();
        self.dg_note("", t().t3);
        self.dg.arm.clear();
        if self.model.diagnose_idle != 0 && self.model.diagnose_agent_err == 0 {
            self.dg_start();
        } else {
            self.diag_paint();
        }
    }

    fn dg_speed_tap(&mut self) {
        let th = t();
        let r = self.dg_run();
        if self.model.diagnose_agent_err != 0 {
            self.dg_start(); // the button reads 重试 then
            return;
        }
        if r.state != DG_DONE {
            self.dg_note(tr("正在查，稍等"), th.t2);
            self.diag_paint();
            return;
        }
        if r.has_speed != 0 && r.speed.level == DG_RUNNING {
            self.dg_note(tr("正在测，稍等"), th.t2);
            self.diag_paint();
            return;
        }
        let now = lv::tick_get();
        if self.dg.net_roam && !self.dg.arm.tap(0, now, DG_ARM_MS) {
            self.diag_paint(); // the first tap while roaming
            return;
        }
        self.dg.arm.clear();
        self.dg.speed_t0 = now.max(1);
        self.dg_note(tr("已发送，测速约 5 秒"), th.t2);
        self.diag_paint();
        lv::refr_now();
        let ok = self.src.diagnose_speed();
        self.sync();
        if !ok {
            self.dg.speed_t0 = 0;
            let e = self.model.diagnose_error.clone().unwrap_or_default();
            self.dg_note(if e.is_empty() { tr("后台没回应") } else { &e }, th.bad_t);
        }
        self.diag_paint();
    }

    /// The first tap arms (the data drops ~15 s, no undo); the second approves the proposal
    /// shown. The core refuses one that changed meanwhile; its refusal comes back as the notice.
    fn dg_redial_tap(&mut self) {
        let Some(p) = self.model.diagnose_proposal.clone() else { return };
        let th = t();
        if !self.dg.arm.tap(DG_ARM_REDIAL, lv::tick_get(), DG_ARM_MS) {
            self.diag_paint();
            return;
        }
        let params = format!("{{\"proposal_id\":\"{}\",\"params_digest\":\"{}\"}}", p.id, p.digest);
        let ok = self.data_control("loop.approve", &params, None);
        if ok {
            self.dg_note(tr("已送出批准；核心核对后才重拨"), th.t2);
        } else {
            self.dg_note(tr("后台没回应"), th.bad_t);
        }
        self.src.diagnose_kick();
        self.diag_paint();
    }

    /// The first tap arms and says what will be set back; the second asks the core, which checks
    /// again and runs it item by item (progress comes back with the page's reads).
    fn dg_restore_tap(&mut self) {
        let Some(v) = self.model.diagnose_restore.clone() else { return };
        let th = t();
        if v.state == 1 {
            self.dg_note(tr("正在恢复，稍等"), th.t2);
            self.diag_paint();
            return;
        }
        if !v.avail || v.items.is_empty() {
            return;
        }
        if !self.dg.arm.tap(DG_ARM_RESTORE, lv::tick_get(), DG_ARM_MS) {
            self.diag_paint();
            return;
        }
        self.dg_note(tr("已发送，逐项恢复"), th.t2);
        self.diag_paint();
        lv::refr_now();
        let ok = self.data_control("verified.restore", "{}", None);
        if !ok {
            self.dg_note(tr("后台没回应"), th.bad_t);
        }
        self.src.diagnose_kick();
        self.diag_paint();
    }

    fn dg_fb_tap(&mut self, right: i32) {
        let r = self.dg_run();
        self.dg.fb_local = right;
        self.dg.fb_id = r.id;
        self.diag_paint(); // 已记下，谢谢 at once
        lv::refr_now();
        let ok = self.src.diagnose_feedback(right != 0);
        self.sync();
        if !ok {
            self.dg.fb_local = -1;
            self.dg_note(tr("没记上：后台没回应，可再点一次"), t().bad_t);
            self.diag_paint();
        }
    }

    fn dg_act_tap(&mut self, k: usize) {
        let to = self.dg.row[k].act_to;
        if to == DG_ACT_PLACE {
            self.sub_open_child(Sub::Place, Sub::Diag);
        }
        else if to == DG_ACT_PROXY {
            // the nodes are on the exit tab's CHILL card
            self.tab_go(super::TAB_EXIT);
            if !self.ex.chill_card.hidden() {
                let sc = self.net.nh_scroll[super::network::NH_EXIT];
                sc.update_layout();
                sc.scroll_to_y(self.ex.chill_card.style_y(0) - 4);
            }
        }
    }

    /// The title bar's 再查一次 (on the subpage header, only for this page).
    pub(super) fn diag_hdr_sync(&mut self) {
        let h = self.dg.hdr;
        if h.is_null() {
            return;
        }
        // always there on this page (a tap while it runs says so); the agent not answering
        // puts 重试 on the page instead
        kit::show(h, self.sub_cur == Some(Sub::Diag) && self.model.diagnose_agent_err == 0);
    }

    pub(super) fn build_sub_diag(&mut self, p: Obj) {
        let th = t();
        let f = uf();
        let sc = kit::scroll(p, 0, SUB_VIEW, 700);
        let dg = &mut self.dg;
        dg.scroll = sc;
        dg.fb_local = -1;
        dg.tone = -1;
        let c = kit::card(sc, MARGIN, 4, CARD_W, HERO_H + 6 * ROW_H);
        dg.card = c;
        let h = kit::hero(c, f.cj22b);
        // the main cause on top, the one thing to do under it (two lines at most)
        h.big.set_pos(PAD, 6);
        h.big.set_size(CARD_W - 2 * PAD, f.cj22b.line_height());
        h.big.set_long_mode(lv::LV_LABEL_LONG_MODE_DOTS);
        h.st.set_pos(PAD, 38);
        h.st.set_width(CARD_W - 2 * PAD);
        h.st.set_long_mode(lv::LV_LABEL_LONG_MODE_WRAP);
        for o in [h.dot, h.unit, h.rtop, h.r1, h.r2] {
            kit::show(o, false);
        }
        kit::hero_tone(&h, 3);
        dg.hero = h;
        for i in 0..DG_ROWS {
            let w = &mut dg.row[i];
            w.bx = kit::bx(c, 0, HERO_H + i as i32 * ROW_H, CARD_W, ROW_H, th.card, 0);
            w.bx.set_style_bg_opa(lv::OPA_TRANSP, 0);
            w.sep = kit::sep(w.bx, 0);
            w.name = kit::label(w.bx, f.cj14, th.t1, PAD, 11, "");
            w.val = kit::label_r(w.bx, f.cj14, th.t3, CARD_W - PAD, 11, "");
            w.detail = kit::label_w(w.bx, f.cj12, th.t2, PAD, 32, CARD_W - 2 * PAD, false, "");
            w.detail.set_long_mode(lv::LV_LABEL_LONG_MODE_DOTS);
            w.act = kit::label(w.bx, f.cj13, th.acc_t, PAD, 0, "");
            kit::tappable(w.act, Some(dg_act_cb), i);
            w.act.set_ext_click_area(12);
            kit::show(w.detail, false);
            kit::show(w.act, false);
            kit::show(w.bx, false);
        }
        (dg.rd, dg.rd_lbl) = kit::button(sc, MARGIN, 0, CARD_W, 44, tr("建议：重拨数据连接"), BtnKind::Plain, Some(dg_redial_cb), 0);
        kit::show(dg.rd, false);
        (dg.rs, dg.rs_lbl) = kit::button(sc, MARGIN, 0, CARD_W, 44, tr("恢复上次能用的设置"), BtnKind::Plain, Some(dg_restore_cb), 0);
        kit::show(dg.rs, false);
        dg.rs_say = kit::label_w(sc, f.cj12, th.t2, MARGIN + 6, 0, CARD_W - 12, true, "");
        kit::show(dg.rs_say, false);
        dg.slow = kit::label_w(sc, f.cj12, th.t2, MARGIN + 6, 0, CARD_W - 12, true, "");
        kit::show(dg.slow, false);
        (dg.btn, dg.btn_lbl) = kit::button(sc, MARGIN, 0, CARD_W, 44, tr("加测速度 · 约 5 秒、最多 30 MB"), BtnKind::Plain, Some(dg_speed_cb), 0);
        dg.note = kit::label_w(sc, f.cj12, th.t3, MARGIN + 6, 0, CARD_W - 12, true, "");
        // 「14:32 测 · 结论对吗？」 + 对 / 不对 (each ≥ 40×40)
        dg.fb = kit::bx(sc, MARGIN, 0, CARD_W, 44, th.bg, 0);
        dg.fb_lbl = kit::label_w(dg.fb, f.cj13, th.t2, 6, 12, 150, false, "");
        (dg.no, _) = kit::button(dg.fb, CARD_W - 64, 0, 64, 44, tr("不对"), BtnKind::Plain, Some(dg_fb_cb), 0);
        (dg.yes, _) = kit::button(dg.fb, CARD_W - 64 - 8 - 64, 0, 64, 44, tr("对"), BtnKind::Plain, Some(dg_fb_cb), 1);
        // title bar, right: 再查一次 (glass, like the ‹ button)
        {
            let b = kit::bx(self.sub_layer, 0, 3, 10, 30, th.glass, 15);
            b.set_style_border_width(1, 0);
            b.set_style_border_color(0x000000, 0);
            b.set_style_border_opa(th.rim_opa, 0);
            let l = kit::label(b, f.cj14, th.acc_t, 12, 6, tr("再查一次"));
            l.update_layout();
            let w = l.width() + 24;
            b.set_size(w, 30);
            b.set_x(W - MARGIN - w);
            kit::tappable(b, Some(dg_start_cb), 0);
            b.set_ext_click_area(6);
            kit::show(b, false);
            self.dg.hdr = b;
            self.dg.hdr_lbl = l;
        }
        sc.scroll_to_y(0);
    }

    /// One layer row: the name left; right the mark + word (+ the value when it fits on the
    /// line, else on a second line). Returns the row's height.
    fn dg_row_paint(&mut self, k: usize, l: &dg_layer_t, name: &str, first: bool, act: i32, act_txt: &str) -> i32 {
        let th = t();
        let f = uf();
        let w = self.dg.row[k];
        self.dg.row[k].act_to = act;
        let mut col = th.t3;
        let ncol = if l.counted != 0 { th.t1 } else { th.t3 };
        let (mark, word) = (dv::level_mark(l.level), dv::level_word(l.level));
        let mut h = ROW_H;
        let mut two = false;
        match l.level {
            DG_OK => col = th.ok_t,
            DG_WARN => col = th.warn_t,
            DG_BAD => col = th.bad_t,
            DG_INFO => col = th.t1,
            _ => {}
        }
        if l.counted == 0 {
            col = th.t3;
        }
        let sp = if mark.is_empty() { "" } else { " " };
        let mut right: Vec<u8> = if l.level == DG_INFO {
            l.detail.clone().into()
        } else if l.level == DG_RUNNING && l.id == "speed" && self.dg.speed_t0 != 0 {
            cf!(tr("测试中… %u 秒"), lv::tick_get().wrapping_sub(self.dg.speed_t0) / 1000)
        } else if l.level == DG_PENDING || l.level == DG_RUNNING || l.detail.is_empty() {
            cf!("%s%s%s", mark, sp, word)
        } else {
            cf!("%s%s%s · %s", mark, sp, word, &l.detail)
        };
        right.truncate(255);
        if w.name.text() != name.as_bytes() {
            w.name.set_text(name);
        }
        w.name.update_layout();
        let avail = CARD_W - 2 * PAD - w.name.width() - 12;
        let (sx, _) = lv::text_size(&right, f.cj14, lv::COORD_MAX);
        if sx > avail && !l.detail.is_empty() && l.level != DG_PENDING && l.level != DG_RUNNING {
            two = true; // the value on its own line, the word stays beside the name
            right = if l.level == DG_INFO { Vec::new() } else { cf!("%s%s%s", mark, sp, word) };
            if w.detail.text() != l.detail.as_bytes() {
                w.detail.set_text(&l.detail);
            }
            kit::text_color(w.detail, if l.counted != 0 && l.level != DG_NA { th.t2 } else { th.t3 });
            h = 54;
        }
        if w.val.text() != right {
            w.val.set_text(&right);
        }
        w.val.set_style_max_width(avail, 0);
        w.val.set_long_mode(lv::LV_LABEL_LONG_MODE_DOTS);
        w.val.set_height(f.cj15.line_height());
        kit::text_color(w.val, col);
        kit::text_color(w.name, ncol);
        kit::show(w.detail, two);
        kit::show(w.sep, !first);
        kit::show(w.act, act != DG_ACT_NONE);
        if act != DG_ACT_NONE {
            if w.act.text() != act_txt.as_bytes() {
                w.act.set_text(act_txt);
            }
            w.act.set_y(h - 6);
            h += 26;
        }
        w.bx.set_height(h);
        h
    }

    pub fn diag_paint(&mut self) {
        let th = t();
        if self.dg.card.is_null() {
            return;
        }
        let mut r = self.dg_run();
        let err = self.model.diagnose_agent_err;
        let now = lv::tick_get();
        self.dg.arm.expire(now, DG_ARM_MS);
        if self.dg.note_at != 0 && now.wrapping_sub(self.dg.note_at) >= DG_NOTE_MS {
            self.dg_note("", th.t3);
        }

        // the rows: what the run has; before the first reply, the layers it will have
        if self.dg.pending || r.state == DG_NONE || r.state == DG_IDLE {
            // proxy last: the CHILL trim below drops it
            let ids: &[&str] = &[
                "wifi", "sim", "signal", "limit", "bearer", "link", "dns", "e2e", "clock", "crowd",
                "proxy",
            ];
            let nids = ids.len();
            let nids = if self.model.chill_online == 0 { nids - 1 } else { nids }; // no proxy layer without CHILL
            let ids = &ids[..nids];
            let state = if self.dg.pending { DG_RUN } else { r.state };
            r = diag_run_t { state, feedback: -1, ..Default::default() };
            for id in ids {
                r.layer.push(dg_layer_t { id: id.to_string(), level: DG_PENDING, counted: 1, ..Default::default() });
            }
            r.n = ids.len() as i32;
            r.steps = r.n;
        }

        // ---- status block ----
        let mut tone = 3;
        let (big, st): (Vec<u8>, Vec<u8>) = if err != 0 {
            tone = 7;
            (tr("后台没回应").into(), (if err == 2 { tr("登录管理后台失败，点下面重试") } else { tr("管理后台没响应，点下面重试") }).into())
        } else if self.dg.pending || r.state == DG_RUN {
            let mut b = cf!(tr("正在检查… %d/%d"), if self.dg.pending { 0 } else { r.step }, r.steps);
            b.truncate(63);
            (b, tr("约 10 秒，会发少量探测包").into())
        } else if r.state == DG_WAITING {
            (tr("等另一个操作做完…").into(), tr("做完自动开始").into())
        } else if r.state == DG_DONE && r.has_main != 0 {
            let any_ok = r.layer.iter().take(r.n.max(0) as usize).any(|l| l.level == DG_OK);
            tone = if r.main_has_level != 0 {
                if r.main_level == DG_BAD { 7 } else { 6 }
            } else if any_ok {
                5
            } else {
                3
            };
            let mut st: Vec<u8> = r.main_action.clone().into();
            if r.more > 0 {
                let mut more = cf!("%s%s", &r.main_action, if r.main_action.is_empty() { "" } else { " · " });
                more.truncate(159);
                more.extend_from_slice(&cf!(tr("另有 %d 处疑点"), r.more));
                more.truncate(159);
                st = more;
            }
            (r.main_text.clone().into(), st)
        } else {
            (tr("正在读取…").into(), Vec::new())
        };
        if tone != self.dg.tone {
            self.dg.tone = tone;
            kit::hero_tone(&self.dg.hero, tone);
        }
        let hero = self.dg.hero;
        if tone == 3 {
            kit::text_color(hero.st, th.t2);
        }
        self.set(hero.big, &big);
        self.set(hero.st, &st);

        // ---- one row per layer, then the speed ----
        let (mut y, mut k) = (HERO_H, 0usize);
        for i in 0..r.n.max(0) as usize {
            if k >= DG_ROWS - 1 {
                break;
            }
            let l = r.layer[i].clone();
            let (mut act, mut act_txt) = (DG_ACT_NONE, "");
            // the signal row, when it is the trouble, leads to 摆放模式 — unless the agent said
            // its cause is something placement can't fix (main is the signal, pointing nowhere)
            if r.state == DG_DONE
                && l.id == "signal"
                && (l.level == DG_WARN || l.level == DG_BAD)
                && (r.main_layer != "signal" || r.action_to == "placement")
            {
                act = DG_ACT_PLACE;
                act_txt = tr("固定位置时用摆放模式 ›");
            }
            if r.state == DG_DONE && l.id == "proxy" && (l.level == DG_WARN || l.level == DG_BAD) {
                act = DG_ACT_PROXY;
                act_txt = tr("去换节点 ›");
            }
            let w = self.dg.row[k];
            kit::show(w.bx, true);
            w.bx.set_y(y);
            y += self.dg_row_paint(k, &l, dv::layer_name(&l.id), k == 0, act, act_txt);
            k += 1;
        }
        if r.has_speed != 0 && r.state == DG_DONE {
            let w = self.dg.row[k];
            kit::show(w.bx, true);
            w.bx.set_y(y);
            let sp = r.speed.clone();
            y += self.dg_row_paint(k, &sp, dv::layer_name("speed"), k == 0, DG_ACT_NONE, "");
            k += 1;
        }
        for w in &self.dg.row[k..] {
            kit::show(w.bx, false);
        }
        self.dg.card.set_height(y);
        y += 4 + 10;

        // ---- the core's proposal: redial ----
        let rd_show = err == 0 && self.model.diagnose_proposal.is_some();
        kit::show(self.dg.rd, rd_show);
        if rd_show {
            let (rd, lbl) = (self.dg.rd, self.dg.rd_lbl);
            let armed = self.dg.arm.live(DG_ARM_REDIAL, now, DG_ARM_MS);
            self.set(lbl, if armed { tr("会断网约 15 秒，再按一次") } else { tr("建议：重拨数据连接") });
            kit::button_kind(rd, lbl, if armed { BtnKind::Armed } else { BtnKind::Plain });
            rd.set_y(y);
            y += 44 + 6;
        }

        // ---- 恢复上次能用的设置 ----
        let rv = if err == 0 { self.model.diagnose_restore.clone() } else { None };
        kit::show(self.dg.rs, rv.as_ref().is_some_and(|v| v.state == 1 || (v.avail && !v.items.is_empty())));
        kit::show(self.dg.rs_say, rv.is_some());
        if let Some(v) = rv {
            let (rs, lbl, say) = (self.dg.rs, self.dg.rs_lbl, self.dg.rs_say);
            let armed = v.avail && self.dg.arm.live(DG_ARM_RESTORE, now, DG_ARM_MS);
            let names: Vec<&str> = v.items.iter().map(|i| dg_item_name(i)).collect();
            let joined = names.join(if crate::lang::is_en() { ", " } else { "、" });
            let (lab, kind) = if v.state == 1 {
                (cf!(tr("正在恢复 %d/%d…"), v.done, v.total), BtnKind::Plain)
            } else if armed {
                (tr("会断网一阵，再按一次").into(), BtnKind::Armed)
            } else {
                (tr("恢复上次能用的设置").into(), BtnKind::Plain)
            };
            self.set(lbl, lab);
            kit::button_kind(rs, lbl, kind);
            if !rs.hidden() {
                rs.set_y(y);
                y += 44 + 4;
            }
            let (txt, col): (Vec<u8>, u32) = match v.state {
                1 => (cf!(tr("逐项改回：%s"), &joined), th.t2),
                2 => (tr("已恢复，网络通了").into(), th.ok_t),
                3 => (tr("已恢复，但网络还是不通").into(), th.warn_t),
                4 => (cf!(tr("停在「%s」，已改的保持不变"), dg_item_name(&v.failed)), th.warn_t),
                _ => (cf!(tr("和上次能用时不同：%s。逐项改回，每项都会断网一阵"), &joined), th.t2),
            };
            self.set(say, &txt);
            kit::text_color(say, col);
            say.set_y(y);
            say.update_layout();
            y += say.height() + 6;
        }

        // ---- the core's words on a strong but slow signal ----
        let sl = self.dg.slow;
        let sl_show = err == 0 && !self.model.diagnose_slow.is_empty();
        kit::show(sl, sl_show);
        if sl_show {
            let txt = self.model.diagnose_slow.clone();
            self.set(sl, &txt);
            sl.set_y(y);
            sl.update_layout();
            y += sl.height() + 6;
        }

        // ---- 加测速度 / 重试 ----
        let done = r.state == DG_DONE && !self.dg.pending;
        let show_btn = err != 0 || done;
        let (btn, lbl) = (self.dg.btn, self.dg.btn_lbl);
        kit::show(btn, show_btn);
        if show_btn {
            let mut kind = BtnKind::Plain;
            if err != 0 {
                kind = BtnKind::Primary;
                self.set(lbl, tr("重试"));
            } else if self.dg.arm.live(0, now, DG_ARM_MS) {
                kind = BtnKind::Armed;
                self.set(lbl, tr("走漫游流量，再按一次"));
            } else if r.has_speed != 0 && r.speed.level == DG_RUNNING {
                self.set(lbl, tr("测速中…"));
            } else {
                self.set(lbl, tr("加测速度 · 约 5 秒、最多 30 MB"));
            }
            kit::button_kind(btn, lbl, kind);
            btn.set_y(y);
            y += 44 + 6;
        }
        let note = self.dg.note;
        let has_note = !note.text().is_empty();
        kit::show(note, has_note);
        if has_note {
            note.set_y(y);
            note.update_layout();
            y += note.height() + 6;
        }

        // ---- 14:32 测 · 对 / 不对 ----
        let (fb, fb_lbl, yes, no) = (self.dg.fb, self.dg.fb_lbl, self.dg.yes, self.dg.no);
        kit::show(fb, done && err == 0);
        if done && err == 0 {
            let hm = dg_hm(r.finished_at);
            let given = r.feedback >= 0 || (self.dg.fb_local >= 0 && self.dg.fb_id == r.id);
            self.set(fb_lbl, cf!(if given { tr("%s 测 · 已记下，谢谢") } else { tr("%s 测 · 结论对吗？") }, &hm));
            fb_lbl.set_width(if given { CARD_W - 12 } else { CARD_W - 2 * 64 - 8 - 12 });
            kit::show(yes, !given);
            kit::show(no, !given);
            fb.set_y(y);
            y += 44 + 10;
        }
        kit::scroll_extent(self.dg.scroll, y + 16);
        self.diag_hdr_sync();
    }

    /// Every refresh: the page while it is up, and the 网络诊断 row on 蜂窝.
    pub(super) fn diag_refresh(&mut self) {
        let vis = self.sub_visible(Sub::Diag);
        if self.polled.diagnose || vis {
            self.diag_paint();
        }
        let l = self.cell.diag_sub;
        if self.tab_visible(TAB_CELL) && !l.is_null() {
            let r = self.dg_run();
            let t: &str = if dg_going(&r) {
                tr("正在检查…")
            } else if r.state == DG_DONE && r.has_main != 0 && self.model.diagnose_idle == 0 {
                &r.main_text
            } else {
                ""
            };
            if l.text() != t.as_bytes() {
                l.set_text(t);
            }
        }
    }

    // ======================= 摆放模式 =======================
    fn pl_restart(&mut self) {
        let p = &mut self.pl;
        p.n = 0;
        p.head = 0;
        p.have_best = false;
        p.reset_at = lv::tick_get().max(1);
    }

    /// Opening the page: 这次最好 is this visit's, not a leftover from the last one.
    pub(super) fn place_on_open(&mut self) {
        self.pl_restart();
        self.pl.reset_at = 0;
        self.pl.key.clear();
        self.pl.manual = false;
    }

    fn pl_reset(&mut self) {
        self.pl_restart();
        self.pl.manual = true; // answered at once, the numbers follow
        let p = &self.pl;
        let (best, cmp, note) = (p.best_l, p.cmp, p.note);
        self.set(best, "—");
        self.set(cmp, "—");
        kit::text_color(cmp, t().t3);
        self.set(note, tr("已清零，重新计"));
    }

    pub(super) fn build_sub_place(&mut self, p: Obj) {
        let th = t();
        let f = uf();
        let sc = kit::scroll(p, 0, SUB_VIEW, 4 + 150 + 10 + 3 * ROW_H + 10 + 44 + 10 + 40 + 16);
        let c = kit::card(sc, MARGIN, 4, CARD_W, 150);
        let pl = &mut self.pl;
        pl.col = Some(1);
        pl.cap = kit::label(c, f.cj13, th.t2, 0, 14, "");
        pl.cap.set_width(CARD_W);
        pl.cap.set_style_text_align(lv::LV_TEXT_ALIGN_CENTER, 0);
        pl.big = kit::label(c, f.n36, th.t1, 0, 36, "--");
        pl.big.set_width(CARD_W);
        pl.big.set_style_text_align(lv::LV_TEXT_ALIGN_CENTER, 0);
        pl.word = kit::label(c, f.cj14, th.t2, 0, 92, "");
        pl.word.set_width(CARD_W);
        pl.word.set_style_text_align(lv::LV_TEXT_ALIGN_CENTER, 0);
        pl.note = kit::label_w(c, f.cj12, th.t3, PAD, 120, CARD_W - 2 * PAD, false, "");
        pl.note.set_style_text_align(lv::LV_TEXT_ALIGN_CENTER, 0);
        let mut y = 4 + 150 + 10;
        let rc = kit::card(sc, MARGIN, y, CARD_W, 3 * ROW_H);
        pl.best_l = kit::row(rc, 0, tr("这次最好"), true);
        pl.cmp = kit::row(rc, ROW_H, tr("对比"), false);
        pl.cell = kit::row(rc, 2 * ROW_H, tr("在用"), false);
        pl.cmp.set_style_text_font(f.cj14, 0);
        y += 3 * ROW_H + 10;
        (pl.btn, _) = kit::button(sc, MARGIN, y, CARD_W, 44, tr("重新开始"), BtnKind::Plain, Some(pl_reset_cb), 0);
        y += 44 + 10;
        kit::label_w(sc, f.cj12, th.t3, MARGIN + 6, y, CARD_W - 12, true, tr("换了小区会重新计；这页开着不息屏"));
        sc.scroll_to_y(0);
    }

    /// live = core answered this second; the model's data is its snapshot (or the last one).
    pub(super) fn place_paint(&mut self, live: bool) {
        let th = t();
        if self.pl.big.is_null() {
            return;
        }
        let m = self.model.clone();
        let d = &m.data;
        let rat = logic::rat(&d.net_type);
        // NSA: the NR leg often reports no SINR while the LTE anchor does (issue #4) — use NR
        // only when its SINR parses, else the anchor; SA has no LTE leg
        let nr = rat == Rat::G5Sa || (rat == Rat::G5Nsa && pl_tenths(&d.nr_snr).is_some());
        let snr = if nr { &d.nr_snr } else { &d.lte_snr };
        let pci = if nr { d.nr_pci } else { d.lte_pci };
        let band: String = if nr {
            d.nr_band.clone()
        } else if !d.band.is_empty() {
            logic::band_short(&d.band, false)
        } else {
            String::new()
        };
        let sample = if live && d.valid != 0 && rat != Rat::None { pl_tenths(snr) } else { None };
        let have = sample.is_some();
        let v = sample.unwrap_or(0);
        let lte_or_nr = nr || rat == Rat::G4 || rat == Rat::G5Nsa;
        let pl = self.pl.clone_handles();
        self.set(pl.cap, cf!(tr("%s SINR · 越大越好"), if nr { "5G" } else { "4G" }));
        // a different cell: the best so far was somewhere else
        let key = format!("{}·{}·{}", rat as i32, band, pci);
        if have && key != self.pl.key {
            if !self.pl.key.is_empty() {
                self.pl_restart();
                self.pl.manual = false;
            }
            self.pl.key = key;
        }
        // a new sample per new core snapshot
        let ver = self.src.core_version(&self.model);
        let p = &mut self.pl;
        if have && lte_or_nr && ver != p.ver {
            p.ver = ver;
            p.v[p.head] = v;
            p.head = (p.head + 1) % PL_SAMPLES;
            if p.n < PL_SAMPLES {
                p.n += 1;
            }
        }
        let mut med = 0;
        if p.n > 0 {
            let mut a = p.v;
            let n = p.n;
            a[..n].sort_unstable();
            med = a[(n - 1) / 2]; // two readings: the lower one
            if live && have && (!p.have_best || med > p.best) {
                p.best = med;
                p.have_best = true;
                p.best_at = m.now;
            }
        }

        let mut col = th.t3;
        if !live && self.pl.n > 0 {
            // core stopped: the last number stays, dimmed, with when
            let alive = m.backend_alive_wall;
            let hm = dg_hm(if alive != 0 { alive } else { self.home.last_valid_wall });
            self.set(pl.big, pl_fmt(med));
            self.set(pl.word, cf!(tr("数字停在 %s"), &hm));
        } else if !have || !lte_or_nr {
            self.set(pl.big, "--");
            self.set(
                pl.word,
                if !live {
                    tr("读不到数据")
                } else if !lte_or_nr && rat != Rat::None {
                    tr("这个制式没有 SINR")
                } else {
                    tr("没有信号")
                },
            );
            self.pl.n = 0;
        } else {
            let (word, mark) = if med >= 200 {
                col = th.ok_t;
                (tr("信号很好"), "●")
            } else if med >= 130 {
                col = th.ok_t;
                (tr("信号良好"), "●")
            } else if med >= 0 {
                col = th.warn_t;
                (tr("信号一般"), "▲")
            } else {
                col = th.bad_t;
                (tr("信号较差"), "■")
            };
            self.set(pl.big, pl_fmt(med));
            let rsrp = if nr { d.nr_rsrp } else { d.lte_rsrp };
            if rsrp != 0 {
                self.set(pl.word, cf!("%s %s · RSRP %d", mark, word, rsrp));
            } else {
                self.set(pl.word, cf!("%s %s", mark, word));
            }
        }
        if self.pl.col != Some(col) {
            self.pl.col = Some(col);
            kit::text_color(pl.big, col);
            kit::text_color(pl.word, col);
        }

        let ra = self.pl.reset_at;
        if ra != 0 && lv::tick_get().wrapping_sub(ra) < 10000 {
            self.set(pl.note, if self.pl.manual { tr("已清零，重新计") } else { tr("换了小区，重新计") });
        } else {
            self.set(pl.note, "");
        }

        if self.pl.have_best {
            let b = pl_fmt(self.pl.best);
            let hm = dg_hm(self.pl.best_at);
            self.set(pl.best_l, cf!("%s · %s", &b, &hm));
            let gap = self.pl.best - med;
            if self.pl.n == 0 {
                self.set(pl.cmp, "—");
            } else if gap <= 10 {
                self.set(pl.cmp, tr("● 接近最好"));
            } else {
                self.set(pl.cmp, cf!(tr("▲ 比最好低 %s dB"), &pl_fmt(gap)));
            }
            kit::text_color(pl.cmp, if self.pl.n == 0 || !live { th.t3 } else if gap <= 10 { th.ok_t } else { th.warn_t });
        } else {
            self.set(pl.best_l, "—");
            self.set(pl.cmp, "—");
            kit::text_color(pl.cmp, th.t3);
        }
        kit::text_color(pl.best_l, if live { th.t1 } else { th.t3 });
        if !band.is_empty() && pci != 0 {
            self.set(pl.cell, cf!("%s · PCI %d", &band, pci));
        } else {
            self.set(pl.cell, "—");
        }
        kit::text_color(pl.cell, if live { th.t1 } else { th.t3 });
    }
}

/// The page's labels, copied out so the painter can borrow the screen.
#[derive(Clone, Copy)]
struct PlHandles {
    cap: Obj,
    big: Obj,
    word: Obj,
    note: Obj,
    best_l: Obj,
    cmp: Obj,
    cell: Obj,
}

impl Place {
    fn clone_handles(&self) -> PlHandles {
        PlHandles { cap: self.cap, big: self.big, word: self.word, note: self.note, best_l: self.best_l, cmp: self.cmp, cell: self.cell }
    }
}

#[cfg(test)]
mod tests {
    use super::{pl_fmt, pl_tenths};

    #[test]
    fn sinr_in_tenths() {
        assert_eq!(pl_tenths("17.7"), Some(177));
        assert_eq!(pl_tenths("-2.5"), Some(-25));
        assert_eq!(pl_tenths("13"), Some(130));
        assert_eq!(pl_tenths(""), None);
        assert_eq!(pl_tenths("-"), None);
        assert_eq!(pl_fmt(177), "17.7");
        assert_eq!(pl_fmt(-25), "-2.5");
        assert_eq!(pl_fmt(-5), "-0.5");
        assert_eq!(pl_fmt(0), "0.0");
    }
}
