//! SIM 与 eSIM (the card in use, its numbers,
//! the eSIM profiles; a tap on one switches to it, two taps).
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use super::dualsim::DS_H;
use super::network::NetFlash;
use super::{Ui, SUB_VIEW};
use crate::cf;
use crate::kit::{self, Hero, CARD_W, HERO_H, MARGIN, PAD, ROW_H};
use crate::lang::{n_, tr};
use crate::logic::SimKind;
use crate::lv::{self, sys, Obj};
use crate::theme::{t, uf};
use ui_model::{esim_profile_t, Model};

/// fits one screen without scrolling; the backend caps at 16
pub const ESIM_MAX_ROWS: usize = 5;
const ESIM_ROW_H: i32 = 50;

use crate::source::{ESIM_SEL_ARMED, ESIM_SEL_BUSY, ESIM_SEL_COOLDOWN, ESIM_SEL_CURRENT, ESIM_SEL_FAIL, ESIM_SEL_STARTED};

/// eUICCs a dual-slot unit can hold: one list each
pub const ESIM_CARDS: usize = 2;
/// the per-slot overview on a dual-slot unit: one row per slot
const OV_ROW_H: i32 = 50;
const OV_H: i32 = 20 + 2 * OV_ROW_H + 10;
/// a card's status line under its list (dual-slot unit)
const FOOT_H: i32 = 22;

/// One eSIM list: the single-slot unit's, or one eUICC's on a dual-slot unit.
#[derive(Default)]
struct List {
    sec: Obj,
    card: Obj,
    empty: Obj,
    /// the card's status line (dual-slot unit only)
    foot: Obj,
    row: [Obj; ESIM_MAX_ROWS],
    row_name: [Obj; ESIM_MAX_ROWS],
    row_sub: [Obj; ESIM_MAX_ROWS],
    row_tag: [Obj; ESIM_MAX_ROWS],
}

#[derive(Default)]
pub struct Esim {
    hero: Hero,
    cur: Obj,
    state: Obj,
    info: [Obj; 3],
    sc: Obj,
    info_sec: Obj,
    info_card: Obj,
    /// the 双卡 block is in (its height pushes the rest down)
    dual_in: bool,
    lists: [List; ESIM_CARDS],
    /// 当前卡槽 (dual-slot unit): one row per slot, its role, operator and profile
    ov_sec: Obj,
    ov_card: Obj,
    ov_name: [Obj; 2],
    ov_sub: [Obj; 2],
    ov_tag: [Obj; 2],
    /// what the last layout and the dual-slot paint were made from (redo only on a change)
    layout_key: String,
    paint_key: String,
    /// the answer to a tap, for a few seconds on the status line
    pub flash: NetFlash,
    /// the list the flash belongs to (dual-slot unit: that card's status line)
    flash_card: usize,
    /// tapped: the next refresh repaints the list
    pub dirty: bool,
    flash_shown: bool,
    /// 重新读取卡槽: the slot whose request is out (0 = none)
    reread_out: i32,
    /// its answer, for a few seconds on the slot's line in 当前卡槽
    reread_flash: NetFlash,
    reread_slot: i32,
    reread_shown: bool,
}

unsafe extern "C" fn ov_row_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let i = unsafe { lv::event_user(e) };
    super::with(|u| u.esim_ov_tap(i));
}

/// The core's answer to 重新读取卡槽 n, in words and colour (manager docs/designs/dual-slot-removable.md).
fn reread_words(n: i32, status: i32, err: &str) -> (Vec<u8>, u32) {
    let th = t();
    match (status, err) {
        (200, _) => (cf!(tr("已重读卡槽 %d，几秒后显示读到的卡"), n), th.acc_t),
        (0, _) => (tr("连不上核心，等一下再试").into(), th.bad_t),
        (_, "data_slot") => (tr("正在上网，没动它；换卡要重启设备").into(), th.t2),
        (_, "standby_slot") => (tr("在待机，没动它；双卡下换卡要重启设备").into(), th.t2),
        (_, "too_soon") => (tr("刚读过，过几秒再试").into(), th.t2),
        (_, "operation in progress") => (tr("eSIM 正在操作，等它结束再试").into(), th.t2),
        (_, "reset_failed") => (tr("这个卡槽没回应，其他都没变").into(), th.bad_t),
        _ => (tr("没读成，其他都没变").into(), th.bad_t),
    }
}

unsafe extern "C" fn esim_row_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let i = unsafe { lv::event_user(e) };
    super::with(|u| u.esim_row_tap(i / ESIM_MAX_ROWS, i % ESIM_MAX_ROWS));
}

/// The card table has two rows: a dual-slot unit (manager docs/designs/dual-slot-removable.md).
fn dual_slot(m: &Model) -> bool {
    m.data.cards.as_ref().is_some_and(|c| c.slots.len() >= 2)
}

/// A slot's role in words (dual-slot §6: whether a card is in use goes by its slot's role).
fn role_words(role: &str) -> &'static str {
    match role {
        "data" => tr("上网中"),
        "voice" => tr("通话用"),
        "standby" => tr("待机"),
        _ => tr("未开启"),
    }
}

/// The enabled profile's tag on a dual-slot unit: the role of the slot its card is in.
fn enabled_tag(role: &str) -> &'static str {
    match role {
        "data" | "voice" | "standby" => role_words(role),
        _ => tr("已启用"),
    }
}

/// Where each block of the page goes, from the top: a pure function of what is shown.
struct Layout {
    info_y: i32,
    ov_y: i32,
    list_y: [i32; ESIM_CARDS],
    extent: i32,
}

fn layout(dual: bool, slots: bool, list_h: &[i32]) -> Layout {
    let mut y = 24 + HERO_H + 10 + if dual { DS_H } else { 0 };
    let ov_y = y;
    if slots {
        y += OV_H;
    }
    let info_y = y;
    y += 20 + 3 * ROW_H + 10;
    let mut list_y = [y; ESIM_CARDS];
    if !slots {
        // the single-slot page as it always was: room for five rows
        return Layout { info_y, ov_y, list_y, extent: y + 20 + ESIM_MAX_ROWS as i32 * ESIM_ROW_H + 16 };
    }
    for (k, h) in list_h.iter().enumerate().take(ESIM_CARDS) {
        list_y[k] = y;
        y += 20 + h + FOOT_H + 10;
    }
    Layout { info_y, ov_y, list_y, extent: y + 6 }
}

/// List k's section, card, empty line and rows at y (k: the tap's card index).
fn build_list(sc: Obj, l: &mut List, k: usize, y: i32) {
    let th = t();
    let f = uf();
    l.sec = kit::section(sc, y, tr("eSIM 配置"));
    l.card = kit::card(sc, MARGIN, y + 20, CARD_W, ESIM_MAX_ROWS as i32 * ESIM_ROW_H);
    l.card.set_style_clip_corner(true, 0);
    l.empty = kit::label_w(l.card, f.cj14, th.t3, PAD, 11, CARD_W - 2 * PAD, true, tr("读取中…"));
    for i in 0..ESIM_MAX_ROWS {
        let row = kit::bx(l.card, 0, i as i32 * ESIM_ROW_H, CARD_W, ESIM_ROW_H, th.card, 0);
        kit::tappable(row, Some(esim_row_cb), k * ESIM_MAX_ROWS + i);
        l.row[i] = row;
        if i > 0 {
            kit::sep(row, 0);
        }
        l.row_name[i] = kit::label_w(row, f.cj14, th.t1, PAD, 8, 190, false, "");
        l.row_sub[i] = kit::label_w(row, f.n11, th.t3, PAD, 29, 200, false, "");
        l.row_tag[i] = kit::label_r(row, f.cj13, th.acc_t, CARD_W - PAD, 16, "");
        kit::show(row, false);
    }
}

impl Ui {
    pub(super) fn build_sub_esim(&mut self, p: Obj) {
        const INFO: [&str; 3] = [n_("号码"), "ICCID", "IMSI"];
        let th = t();
        let f = uf();
        let sc = kit::scroll(p, 0, SUB_VIEW, layout(false, false, &[]).extent);
        kit::section(sc, 4, tr("当前配置"));
        let cur = kit::card(sc, MARGIN, 24, CARD_W, HERO_H);
        let es = &mut self.es;
        es.hero = kit::hero(cur, f.cj22b);
        kit::hero_tone(&es.hero, 4);
        es.cur = es.hero.big;
        es.state = es.hero.r2;
        es.hero.st.set_text(tr("使用中"));
        // 卡信息: a plain SIM and an eSIM alike (9-25: with a plain SIM the page was all 「-」)
        es.sc = sc;
        let lay = layout(false, false, &[]);
        let y = lay.info_y;
        es.info_sec = kit::section(sc, y, tr("卡信息"));
        let info = kit::card(sc, MARGIN, y + 20, CARD_W, 3 * ROW_H);
        es.info_card = info;
        for (i, k) in INFO.iter().enumerate() {
            es.info[i] = kit::row(info, i as i32 * ROW_H, tr(k), i == 0);
        }
        build_list(sc, &mut es.lists[0], 0, lay.list_y[0]);
        self.build_dualsim(sc, 24 + HERO_H + 10);
        // the dual-slot unit's objects after the 双卡 block's, so the older ones keep their place
        let es = &mut self.es;
        build_list(sc, &mut es.lists[1], 1, lay.list_y[0]);
        for l in &mut es.lists {
            l.foot = kit::label_w(sc, f.cj12, th.t2, MARGIN + 6, lay.list_y[0], CARD_W - 12, false, "");
            kit::show(l.foot, false);
        }
        for o in [es.lists[1].sec, es.lists[1].card] {
            kit::show(o, false);
        }
        // 当前卡槽: hidden until the card table has two rows
        es.ov_sec = kit::section(sc, lay.info_y, tr("当前卡槽"));
        es.ov_card = kit::card(sc, MARGIN, lay.info_y + 20, CARD_W, 2 * OV_ROW_H);
        es.ov_card.set_style_clip_corner(true, 0);
        for i in 0..2 {
            let row = kit::bx(es.ov_card, 0, i as i32 * OV_ROW_H, CARD_W, OV_ROW_H, th.card, 0);
            // an off slot's row reads the slot again; the others say why it does not
            kit::tappable(row, Some(ov_row_cb), i);
            if i > 0 {
                kit::sep(row, 0);
            }
            es.ov_name[i] = kit::label_w(row, f.cj14, th.t1, PAD, 8, 200, false, "");
            es.ov_sub[i] = kit::label_w(row, f.n11, th.t3, PAD, 29, CARD_W - 2 * PAD, false, "");
            es.ov_tag[i] = kit::label_r(row, f.cj13, th.acc_t, CARD_W - PAD, 16, "");
        }
        for o in [es.ov_sec, es.ov_card] {
            kit::show(o, false);
        }
    }

    /// 双卡 and 当前卡槽 appear or go, a list grows or shrinks: move what is below, only when
    /// something moved.
    fn esim_layout(&mut self, dual: bool, slots: bool, list_h: &[i32]) {
        let key = format!("{dual}|{slots}|{list_h:?}");
        let es = &mut self.es;
        if key == es.layout_key {
            return;
        }
        es.layout_key = key;
        es.dual_in = dual;
        let lay = layout(dual, slots, list_h);
        es.info_sec.set_y(lay.info_y);
        es.info_card.set_y(lay.info_y + 20);
        es.ov_sec.set_y(lay.ov_y);
        es.ov_card.set_y(lay.ov_y + 20);
        for (k, l) in es.lists.iter().enumerate() {
            let y = lay.list_y[k];
            l.sec.set_y(y);
            l.card.set_y(y + 20);
            l.foot.set_y(y + 20 + list_h.get(k).copied().unwrap_or(0) + 4);
        }
        kit::scroll_extent(es.sc, lay.extent);
    }

    /// A tap on a profile: say why when nothing moves, never ignore it silently (9-25).
    fn esim_row_tap(&mut self, card: usize, i: usize) {
        let th = t();
        let r = self.esim_select(card, i);
        let dual = dual_slot(&self.model) && !self.model.esim_cards.is_empty();
        let state = if dual {
            self.model.esim_cards.get(card).map(|c| c.state.clone()).unwrap_or_default()
        } else {
            self.model.esim_state.clone().unwrap_or_default()
        };
        self.es.flash_card = card;
        match r {
            // a dual-slot unit: the enabled profile of a card not carrying data is not "in use"
            ESIM_SEL_CURRENT if dual => self.es.flash.set(th.acc_t, 3000, tr("这张已经启用").into()),
            ESIM_SEL_CURRENT => self.es.flash.set(th.acc_t, 3000, tr("这张就是正在用的").into()),
            ESIM_SEL_BUSY => {
                // not always a switch: the web page may be downloading, or the agent notifying
                if self.model.esim_switching != 0 {
                    self.es.flash.set(th.t2, 3000, tr("正在切换，等它做完").into());
                } else {
                    self.es.flash.set(th.t2, 3000, state.into());
                }
            }
            // the state line says why: in red
            ESIM_SEL_COOLDOWN | ESIM_SEL_FAIL => self.es.flash.set(th.bad_t, 8000, state.into()),
            // ARMED / STARTED: the row itself changes colour and says so
            ESIM_SEL_ARMED | ESIM_SEL_STARTED => self.es.flash.until = 0,
            _ => self.es.flash.until = 0,
        }
        self.es.dirty = true;
        self.esim_paint();
    }

    /// A tap on a 当前卡槽 row: an off slot is read again (a card put in while the unit was on
    /// shows without a restart); a slot in use says why nothing moves.
    fn esim_ov_tap(&mut self, i: usize) {
        let th = t();
        let slot = i as i32 + 1;
        let role = self.model.data.cards.as_ref().and_then(|c| c.slots.iter().find(|r| r.slot == slot)).map_or(String::new(), |r| r.role.clone());
        if !dual_slot(&self.model) {
            return;
        }
        let es = &mut self.es;
        es.reread_slot = slot;
        if es.reread_out != 0 {
            es.reread_flash.set(th.t2, 3000, tr("正在读取，等它做完").into());
        } else {
            match role.as_str() {
                "data" => es.reread_flash.set(th.t2, 4000, tr("正在上网，没动它；换卡要重启设备").into()),
                "voice" | "standby" => es.reread_flash.set(th.t2, 4000, tr("在待机，没动它；双卡下换卡要重启设备").into()),
                _ => {
                    if self.src.sim_reread(slot) {
                        self.es.reread_out = slot;
                        self.es.reread_flash.until = 0;
                    } else {
                        self.es.reread_flash.set(th.bad_t, 4000, tr("连不上核心，等一下再试").into());
                    }
                }
            }
        }
        self.es.dirty = true;
        self.esim_paint();
    }

    /// esim_select(): two taps on the data side; what it armed or started shows at once.
    fn esim_select(&mut self, card: usize, i: usize) -> i32 {
        let r = self.src.esim_select(card, i);
        self.sync();
        r
    }

    /// One list's rows; `enabled_tag` is the enabled profile's word.
    fn esim_rows(&self, k: usize, profiles: &[esim_profile_t], enabled_tag: &str) {
        let th = t();
        let l = &self.es.lists[k];
        for i in 0..ESIM_MAX_ROWS {
            let Some(p) = profiles.get(i) else {
                kit::show(l.row[i], false);
                continue;
            };
            kit::show(l.row[i], true);
            l.row_name[i].set_text(&p.name);
            // catBusy: waiting and retrying do not help, the card needs power cycling (10-04)
            let busy_row = p.card_busy != 0 && p.armed == 0;
            l.row_sub[i].set_text(if busy_row { tr("卡忙：重启设备或拔插卡后再切") } else { p.sub.as_str() });
            l.row_tag[i].set_text(if p.going != 0 {
                tr("切换中…")
            } else if p.armed != 0 {
                tr("再点一次确认切换")
            } else if busy_row {
                tr("没切成")
            } else if p.enabled != 0 {
                enabled_tag
            } else {
                ""
            });
            // the one in use and one switching can be tapped too: the tap says why nothing moves
            kit::bg(l.row[i], if p.armed != 0 { th.fill_orange } else if p.enabled != 0 { th.acc_s } else { th.card });
            let fg = if p.armed != 0 { 0xffffff } else { th.t1 };
            kit::text_color(l.row_name[i], fg);
            kit::text_color(l.row_sub[i], if p.armed != 0 { th.on_fill } else if busy_row { th.bad_t } else { th.t3 });
            kit::text_color(
                l.row_tag[i],
                if p.armed != 0 {
                    0xffffff
                } else if busy_row {
                    th.bad_t
                } else if p.enabled != 0 {
                    th.acc_t
                } else {
                    th.t2
                },
            );
        }
    }

    /// A list's card height: its rows, or the empty line (which wraps) when it has none.
    fn esim_list_h(&self, k: usize, n: usize) -> i32 {
        let l = &self.es.lists[k];
        let h = if n > 0 {
            n as i32 * ESIM_ROW_H
        } else {
            l.empty.update_layout();
            (l.empty.height() + 22).max(ROW_H)
        };
        if l.card.height() != h {
            l.card.set_height(h);
        }
        h
    }

    /// The eSIM list: drawn when the data changed, and at once on a tap.
    pub(super) fn esim_paint(&mut self) {
        self.es.dirty = false;
        if dual_slot(&self.model) {
            self.esim_paint_slots();
            return;
        }
        let m = self.model.clone();
        let n = m.esim_profiles.len();
        let sim = self.sim.clone();
        let plain = sim.kind == SimKind::Plain;
        let cur = m.esim_current.as_deref().unwrap_or("");
        let state = m.esim_state.as_deref().unwrap_or("");
        let tail = iccid_tail_words(&sim.iccid);
        self.esim_hero(&m, &sim, cur, state, &tail);
        let es = &self.es;
        let l = &es.lists[0];
        l.sec.set_text(if plain && n > 0 { tr("eSIM 配置（现在没在用）") } else { tr("eSIM 配置") });
        kit::show(l.empty, n == 0);
        if n == 0 {
            // esim_ready = the state line says 就绪: the code, not the (translated) text
            let ready = m.esim_ready != 0;
            l.empty.set_text(if plain {
                tr("现在插的是普通 SIM 卡，没有 eSIM 配置。换成 eSIM 卡后可以在这里切换。")
            } else if m.esim_loaded == 0 && ready {
                tr("读取中…")
            } else if !ready {
                state
            } else {
                tr("还没有 eSIM 配置 · 用管理网页添加")
            });
        }
        if let Some(p) = m.esim_profiles.iter().take(ESIM_MAX_ROWS).find(|p| p.enabled != 0)
            && !plain
        {
            es.hero.rtop.set_text(&p.sub);
        }
        self.esim_rows(0, &m.esim_profiles, if plain { tr("已启用") } else { tr("使用中") });
        // the empty line wraps (the plain-SIM sentence is two lines): the card follows it
        self.esim_list_h(0, n);
    }

    /// The block on top: the card in use (the data card); a plain SIM says its operator, an
    /// eSIM its profile.
    fn esim_hero(&self, m: &Model, sim: &super::refresh::Sim, cur: &str, state: &str, tail: &[u8]) {
        let th = t();
        let plain = sim.kind == SimKind::Plain;
        let none = sim.kind == SimKind::None;
        let es = &self.es;
        es.hero.st.set_text(if none {
            tr("没有卡")
        } else if plain {
            tr("使用中 · 实体 SIM 卡")
        } else {
            tr("使用中 · eSIM")
        });
        es.cur.set_text(if none {
            tr("没插卡")
        } else if plain {
            if sim.oper.is_empty() { tr("SIM 卡") } else { sim.oper.as_str() }
        } else if !cur.is_empty() && cur != "-" {
            cur
        } else {
            "—"
        });
        // the last switch got catBusy: the state line in red, the profile's row says what to do
        let card_busy = m.esim_profiles.iter().take(ESIM_MAX_ROWS).any(|p| p.card_busy != 0);
        // on a dual-slot unit a tap's answer goes under its own card's list
        let flash_here = es.flash.on() && !dual_slot(m);
        if flash_here {
            es.state.set_text(&es.flash.txt);
            kit::text_color(es.state, es.flash.col);
        } else {
            // a SIM identity transition (any card, also the firmware's own slot switch): the
            // core's shared word, unless our own switch is saying its own
            let say = m.data.sim_say.as_str();
            es.state.set_text(if !none && m.esim_switching == 0 && !say.is_empty() {
                say
            } else if none {
                tr("插上 SIM 卡或 eSIM 卡后这里显示卡信息")
            } else if plain {
                // dual SIM, data on slot 2: the firmware reports no number for that card
                let slot2 = m.data.sim_dual.as_ref().is_some_and(|x| x.active_slot == 2);
                if !sim.msisdn.is_empty() {
                    sim.msisdn.as_str()
                } else if slot2 {
                    tr("卡槽 2 不提供号码")
                } else {
                    tr("号码没写在卡里")
                }
            } else {
                state
            });
            kit::text_color(es.state, if card_busy && !none && !plain { th.bad_t } else { th.t2 });
        }
        es.hero.rtop.set_text(if plain { tail } else { b"" });
        let dash = |s: &str| if s.is_empty() { "—".to_string() } else { s.to_string() };
        es.info[0].set_text(dash(&sim.msisdn));
        es.info[1].set_text(dash(&sim.iccid));
        es.info[2].set_text(dash(&sim.imsi));
    }

    /// A dual-slot unit: the card in use on top, 当前卡槽 (each slot's role, operator, profile),
    /// then one list per eUICC; a slot without one has none (dual-slot §6).
    fn esim_paint_slots(&mut self) {
        let th = t();
        let m = self.model.clone();
        let sim = self.sim.clone();
        let rows = m.data.cards.as_ref().map(|c| c.slots.clone()).unwrap_or_default();
        let ecards = m.esim_cards.clone();
        let flash = if self.es.flash.on() { Some((self.es.flash_card, self.es.flash.txt.clone(), self.es.flash.col)) } else { None };
        let reread = (self.es.reread_out, self.es.reread_slot, self.es.reread_flash.on().then(|| self.es.reread_flash.txt.clone()));
        let key = format!("{rows:?}|{ecards:?}|{flash:?}|{reread:?}|{:?}|{:?}|{}|{}", m.data.sim_dual, sim, m.data.sim_say, m.esim_switching);
        if key == self.es.paint_key {
            return;
        }
        self.es.paint_key = key;
        let data_card = ecards.iter().find(|c| rows.iter().any(|r| r.slot == c.slot && r.role == "data"));
        let (cur, state) = match data_card {
            Some(c) => (c.current.clone(), c.state.clone()),
            None => (String::new(), String::new()),
        };
        let tail = iccid_tail_words(&sim.iccid);
        self.esim_hero(&m, &sim, &cur, &state, &tail);
        if let Some(p) = data_card.and_then(|c| c.profiles.iter().find(|p| p.enabled != 0))
            && sim.kind == SimKind::Esim
        {
            self.es.hero.rtop.set_text(&p.sub);
        }
        // 当前卡槽: one row per slot
        let es = &self.es;
        for i in 0..2 {
            let slot = i as i32 + 1;
            let r = rows.iter().find(|r| r.slot == slot);
            let present = r.is_some_and(|r| r.present != 0);
            let oper = m.data.sim_dual.as_ref().and_then(|d| d.slots.iter().find(|s| s.slot == slot)).map(|s| s.operator.clone()).unwrap_or_default();
            let oper = if oper.is_empty() && r.is_some_and(|r| r.role == "data") { sim.oper.clone() } else { oper };
            let name = if present && !oper.is_empty() { cf!(tr("卡槽 %d · %s"), slot, oper.as_str()) } else { cf!(tr("卡槽 %d"), slot) };
            es.ov_name[i].set_text(&name);
            let ec = ecards.iter().find(|c| c.slot == slot);
            let tail_of = |t: &str| if t.is_empty() { Vec::new() } else { cf!(tr("尾号 %.4s"), t) };
            let sub: Vec<u8> = match r {
                Some(r) if r.present == 0 => tr("没有卡").into(),
                None => tr("没有卡").into(),
                Some(r) => {
                    let mut parts: Vec<Vec<u8>> = Vec::new();
                    match r.id.as_str() {
                        "eid" => {
                            let p = ec.map(|c| c.current.as_str()).filter(|c| !c.is_empty() && *c != "-");
                            parts.push(match p {
                                Some(p) => cf!("eSIM · %s", p),
                                None => b"eSIM".to_vec(),
                            });
                        }
                        "plain" => parts.push(tr("实体卡").into()),
                        "unknown" => parts.push(tr("认不出这张卡").into()),
                        _ => parts.push(tr("正在识别卡…").into()),
                    }
                    let t = tail_of(&r.iccid_tail);
                    if !t.is_empty() {
                        parts.push(t);
                    }
                    parts.join(" · ".as_bytes())
                }
            };
            // an off slot can be read again: say so on its line
            let off = !r.is_some_and(|r| matches!(r.role.as_str(), "data" | "voice" | "standby"));
            let sub = if off { [sub, tr("点一下重新读取").into()].join(" · ".as_bytes()) } else { sub };
            let flash_here = es.reread_flash.on() && es.reread_slot == slot;
            es.ov_sub[i].set_text(if flash_here { &es.reread_flash.txt } else { &sub });
            kit::text_color(es.ov_sub[i], if flash_here { es.reread_flash.col } else { th.t3 });
            let reading = es.reread_out == slot;
            es.ov_tag[i].set_text(if reading {
                tr("读取中…")
            } else if present {
                role_words(r.map_or("", |r| r.role.as_str()))
            } else {
                ""
            });
            let on = present && r.is_some_and(|r| matches!(r.role.as_str(), "data" | "voice"));
            kit::text_color(es.ov_tag[i], if on || reading { th.acc_t } else { th.t3 });
            kit::text_color(es.ov_name[i], if present { th.t1 } else { th.t3 });
        }
        // one list per eUICC (at most two)
        let mut hs = Vec::new();
        for k in 0..ESIM_CARDS {
            let Some(c) = ecards.get(k) else {
                let l = &self.es.lists[k];
                for o in [l.sec, l.card, l.foot] {
                    kit::show(o, false);
                }
                continue;
            };
            let role = rows.iter().find(|r| r.slot == c.slot).map_or("", |r| r.role.as_str()).to_string();
            let l = &self.es.lists[k];
            for o in [l.sec, l.card, l.foot] {
                kit::show(o, true);
            }
            l.sec.set_text(cf!(tr("卡槽 %d · eSIM 配置"), c.slot));
            let n = c.profiles.len();
            kit::show(l.empty, n == 0);
            if n == 0 {
                l.empty.set_text(if c.loaded == 0 && c.ready != 0 {
                    tr("读取中…")
                } else if c.ready == 0 {
                    c.state.as_str()
                } else {
                    tr("还没有 eSIM 配置 · 用管理网页添加")
                });
            }
            // the card's status line: a tap's answer, or where its read / switch stands
            let busy = c.profiles.iter().any(|p| p.card_busy != 0);
            match &flash {
                Some((fk, txt, col)) if *fk == k => {
                    l.foot.set_text(txt);
                    kit::text_color(l.foot, *col);
                }
                _ => {
                    l.foot.set_text(&c.state);
                    kit::text_color(l.foot, if busy { th.bad_t } else { th.t2 });
                }
            }
            self.esim_rows(k, &c.profiles, enabled_tag(&role));
            hs.push(self.esim_list_h(k, n));
        }
        let dual = m.data.sim_dual.is_some();
        self.esim_layout(dual, true, &hs);
    }

    /// repaint when esim_poll (or the prefetch) read something new,
    /// the card changed, or a tap or an ended flash asks for it.
    pub(super) fn esim_refresh(&mut self, sim_changed: bool) {
        let changed = self.polled.esim || sim_changed;
        if self.es.flash_shown && !self.es.flash.on() {
            self.es.dirty = true; // the flash ended: back to the state line
        }
        if let Some((st, err)) = self.src.sim_reread_take() {
            let n = std::mem::take(&mut self.es.reread_out);
            let (txt, col) = reread_words(n, st, &err);
            self.es.reread_slot = n;
            self.es.reread_flash.set(col, 8000, txt);
            self.es.dirty = true;
        }
        if self.es.reread_shown != self.es.reread_flash.on() {
            self.es.reread_shown = self.es.reread_flash.on();
            self.es.dirty = true; // an answer came or went
        }
        if changed || self.es.dirty {
            self.es.flash_shown = self.es.flash.on();
            self.esim_paint();
        }
        let slots = dual_slot(&self.model);
        if !slots {
            self.esim_layout(self.model.data.sim_dual.is_some(), false, &[]);
            for l in &self.es.lists[1..] {
                for o in [l.sec, l.card] {
                    kit::show(o, false);
                }
            }
            for o in [self.es.lists[0].foot, self.es.lists[1].foot, self.es.ov_sec, self.es.ov_card] {
                kit::show(o, false);
            }
            self.es.paint_key.clear();
        } else {
            for o in [self.es.ov_sec, self.es.ov_card] {
                kit::show(o, true);
            }
        }
        self.ds_paint();
    }
}

/// 「尾号 1234」 of an ICCID (its BCD filler dropped), nothing for a short one.
fn iccid_tail_words(iccid: &str) -> Vec<u8> {
    let ic = crate::logic::iccid_norm(iccid);
    if ic.len() >= 4 { cf!(tr("尾号 %.4s"), &ic[ic.len() - 4..]) } else { Vec::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reread_words_say_each_answer() {
        let th = t();
        let ok = reread_words(2, 200, "");
        assert_eq!(String::from_utf8(ok.0).unwrap(), "已重读卡槽 2，几秒后显示读到的卡");
        assert_eq!(ok.1, th.acc_t);
        // every refusal the core gives has its own words; failures are red, refusals are not
        let mut seen = std::collections::BTreeSet::new();
        for (st, e, bad) in [
            (0, "", true),
            (409, "data_slot", false),
            (409, "standby_slot", false),
            (429, "too_soon", false),
            (409, "operation in progress", false),
            (502, "reset_failed", true),
            (404, "no_tool", true),
        ] {
            let (txt, col) = reread_words(1, st, e);
            assert!(seen.insert(txt), "{e}");
            assert_eq!(col == th.bad_t, bad, "{e}");
        }
        // English: each has a row in lang/en.inc
        crate::lang::set_en(true);
        for e in ["data_slot", "standby_slot", "too_soon", "operation in progress", "reset_failed", "x"] {
            let (txt, _) = reread_words(1, 409, e);
            assert!(txt.is_ascii(), "{}", String::from_utf8_lossy(&txt));
        }
        assert!(reread_words(1, 0, "").0.is_ascii());
        assert!(reread_words(1, 200, "").0.is_ascii());
        assert!(tr("点一下重新读取").is_ascii() && tr("正在读取，等它做完").is_ascii());
        crate::lang::set_en(false);
    }
}
