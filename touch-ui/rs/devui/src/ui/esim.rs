//! src/ui_parts/esim.c and refresh.c's esim_paint: SIM 与 eSIM (the card in use, its numbers,
//! the eSIM profiles; a tap on one switches to it, two taps).
// SPDX-License-Identifier: MIT

use super::network::NetFlash;
use super::{Ui, SUB_VIEW};
use crate::cf;
use crate::kit::{self, Hero, CARD_W, HERO_H, MARGIN, PAD, ROW_H};
use crate::lang::{n_, tr};
use crate::logic::SimKind;
use crate::lv::{self, sys, Obj};
use crate::theme::{t, uf};

/// fits one screen without scrolling; the backend caps at 16
pub const ESIM_MAX_ROWS: usize = 5;
const ESIM_ROW_H: i32 = 50;

use crate::source::{ESIM_SEL_ARMED, ESIM_SEL_BUSY, ESIM_SEL_COOLDOWN, ESIM_SEL_CURRENT, ESIM_SEL_FAIL, ESIM_SEL_STARTED};

#[derive(Default)]
pub struct Esim {
    hero: Hero,
    cur: Obj,
    state: Obj,
    list_card: Obj,
    empty: Obj,
    info: [Obj; 3],
    list_sec: Obj,
    pub row: [Obj; ESIM_MAX_ROWS],
    row_name: [Obj; ESIM_MAX_ROWS],
    row_sub: [Obj; ESIM_MAX_ROWS],
    row_tag: [Obj; ESIM_MAX_ROWS],
    /// the answer to a tap, for a few seconds on the status line
    pub flash: NetFlash,
    /// tapped: the next refresh repaints the list
    pub dirty: bool,
    flash_shown: bool,
}

unsafe extern "C" fn esim_row_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let i = unsafe { lv::event_user(e) };
    super::with(|u| u.esim_row_tap(i));
}

impl Ui {
    pub(super) fn build_sub_esim(&mut self, p: Obj) {
        const INFO: [&str; 3] = [n_("号码"), "ICCID", "IMSI"];
        let th = t();
        let f = uf();
        let sc = kit::scroll(p, 0, SUB_VIEW, 4 + 20 + HERO_H + 10 + 20 + 3 * ROW_H + 10 + 20 + ESIM_MAX_ROWS as i32 * ESIM_ROW_H + 16);
        kit::section(sc, 4, tr("当前配置"));
        let cur = kit::card(sc, MARGIN, 24, CARD_W, HERO_H);
        let es = &mut self.es;
        es.hero = kit::hero(cur, f.cj22b);
        kit::hero_tone(&es.hero, 4);
        es.cur = es.hero.big;
        es.state = es.hero.r2;
        es.hero.st.set_text(tr("使用中"));
        // 卡信息: a plain SIM and an eSIM alike (9-25: with a plain SIM the page was all 「-」)
        let mut y = 24 + HERO_H + 10;
        kit::section(sc, y, tr("卡信息"));
        let info = kit::card(sc, MARGIN, y + 20, CARD_W, 3 * ROW_H);
        for (i, k) in INFO.iter().enumerate() {
            es.info[i] = kit::row(info, i as i32 * ROW_H, tr(k), i == 0);
        }
        y += 20 + 3 * ROW_H + 10;
        es.list_sec = kit::section(sc, y, tr("eSIM 配置"));
        es.list_card = kit::card(sc, MARGIN, y + 20, CARD_W, ESIM_MAX_ROWS as i32 * ESIM_ROW_H);
        es.list_card.set_style_clip_corner(true, 0);
        es.empty = kit::label_w(es.list_card, f.cj14, th.t3, PAD, 11, CARD_W - 2 * PAD, false, tr("读取中…"));
        for i in 0..ESIM_MAX_ROWS {
            let row = kit::bx(es.list_card, 0, i as i32 * ESIM_ROW_H, CARD_W, ESIM_ROW_H, th.card, 0);
            kit::tappable(row, Some(esim_row_cb), i);
            es.row[i] = row;
            if i > 0 {
                kit::sep(row, 0);
            }
            es.row_name[i] = kit::label_w(row, f.cj14, th.t1, PAD, 8, 190, false, "");
            es.row_sub[i] = kit::label_w(row, f.n11, th.t3, PAD, 29, 200, false, "");
            es.row_tag[i] = kit::label_r(row, f.cj13, th.acc_t, CARD_W - PAD, 16, "");
            kit::show(row, false);
        }
    }

    /// A tap on a profile: say why when nothing moves, never ignore it silently (9-25).
    fn esim_row_tap(&mut self, i: usize) {
        let th = t();
        let r = self.esim_select(i);
        let state = self.model.esim_state.clone().unwrap_or_default();
        match r {
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

    /// esim_select(): two taps on the data side; what it armed or started shows at once.
    fn esim_select(&mut self, i: usize) -> i32 {
        let r = self.src.esim_select(i);
        self.sync();
        r
    }

    /// The eSIM list: drawn when the data changed, and at once on a tap.
    pub(super) fn esim_paint(&mut self) {
        let th = t();
        self.es.dirty = false;
        let m = self.model.clone();
        let n = m.esim_profiles.len();
        let sim = self.sim.clone();
        let plain = sim.kind == SimKind::Plain;
        let none = sim.kind == SimKind::None;
        let cur = m.esim_current.as_deref().unwrap_or("");
        let state = m.esim_state.as_deref().unwrap_or("");
        let tail: Vec<u8> = {
            let ic = sim.iccid.trim_end_matches(['F', 'f']);
            if ic.len() >= 4 { cf!(tr("尾号 %.4s"), &ic[ic.len() - 4..]) } else { Vec::new() }
        };
        let es = &self.es;
        // the block on top is the card in use: a plain SIM says its operator, an eSIM its profile
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
        if es.flash.on() {
            es.state.set_text(&es.flash.txt);
            kit::text_color(es.state, es.flash.col);
        } else {
            es.state.set_text(if none {
                tr("插上 SIM 卡或 eSIM 卡后这里显示卡信息")
            } else if plain {
                if sim.msisdn.is_empty() { tr("号码没写在卡里") } else { sim.msisdn.as_str() }
            } else {
                state
            });
            kit::text_color(es.state, if card_busy && !none && !plain { th.bad_t } else { th.t2 });
        }
        es.hero.rtop.set_text(if plain { tail.as_slice() } else { b"" });
        let dash = |s: &str| if s.is_empty() { "—".to_string() } else { s.to_string() };
        es.info[0].set_text(dash(&sim.msisdn));
        es.info[1].set_text(dash(&sim.iccid));
        es.info[2].set_text(dash(&sim.imsi));
        es.list_sec.set_text(if plain && n > 0 { tr("eSIM 配置（现在没在用）") } else { tr("eSIM 配置") });
        kit::show(es.empty, n == 0);
        if n == 0 {
            // esim_ready = the state line says 就绪: the code, not the (translated) text
            let ready = m.esim_ready != 0;
            es.empty.set_text(if plain {
                tr("现在插的是普通 SIM 卡，没有 eSIM 配置。换成 eSIM 卡后可以在这里切换。")
            } else if m.esim_loaded == 0 && ready {
                tr("读取中…")
            } else if !ready {
                state
            } else {
                tr("还没有 eSIM 配置 · 用管理网页添加")
            });
        }
        for i in 0..ESIM_MAX_ROWS {
            if i >= n {
                kit::show(es.row[i], false);
                continue;
            }
            let p = &m.esim_profiles[i];
            kit::show(es.row[i], true);
            es.row_name[i].set_text(&p.name);
            // catBusy: waiting and retrying do not help, the card needs power cycling (10-04)
            let busy_row = p.card_busy != 0 && p.armed == 0;
            es.row_sub[i].set_text(if busy_row { tr("卡忙：重启设备或拔插卡后再切") } else { p.sub.as_str() });
            if p.enabled != 0 && !plain {
                es.hero.rtop.set_text(&p.sub);
            }
            es.row_tag[i].set_text(if p.going != 0 {
                tr("切换中…")
            } else if p.armed != 0 {
                tr("再点一次确认切换")
            } else if busy_row {
                tr("没切成")
            } else if p.enabled != 0 {
                if plain { tr("已启用") } else { tr("使用中") }
            } else {
                ""
            });
            // the one in use and one switching can be tapped too: the tap says why nothing moves
            kit::bg(es.row[i], if p.armed != 0 { th.fill_orange } else if p.enabled != 0 { th.acc_s } else { th.card });
            let fg = if p.armed != 0 { 0xffffff } else { th.t1 };
            kit::text_color(es.row_name[i], fg);
            kit::text_color(es.row_sub[i], if p.armed != 0 { th.on_fill } else if busy_row { th.bad_t } else { th.t3 });
            kit::text_color(
                es.row_tag[i],
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
        es.list_card.set_height(if n > 0 { n as i32 * ESIM_ROW_H } else { ROW_H });
    }

    /// refresh.c 「eSIM subpage」: repaint when esim_poll (or the prefetch) read something new,
    /// the card changed, or a tap or an ended flash asks for it.
    pub(super) fn esim_refresh(&mut self, sim_changed: bool) {
        let changed = self.polled.esim || sim_changed;
        if self.es.flash_shown && !self.es.flash.on() {
            self.es.dirty = true; // the flash ended: back to the state line
        }
        if changed || self.es.dirty {
            self.es.flash_shown = self.es.flash.on();
            self.esim_paint();
        }
    }
}
