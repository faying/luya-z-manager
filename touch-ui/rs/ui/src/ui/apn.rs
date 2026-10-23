//! the APN page, the › row cards on the tabs (nav_card) and net_paint,
//! which draws luyaz-core's netinfo onto the pages network.rs built.
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use super::network::{
    net_armed, net_busy, net_confirm, net_exit_sub, net_oper_text, net_rat_name, net_scene_name, NetFlash, NET_SCENE_ROWS,
    NI_MAX_CELLS, NI_MAX_OPS,
};
use super::{Sub, Ui, SUB_VIEW};
use crate::cf;
use crate::clock;
use crate::kit::{self, BtnKind, CARD_W, HERO_H, MARGIN, PAD, ROW2_H, ROW_H};
use crate::lang::{self, tr};
use crate::lv::{self, sys, Obj};
use crate::theme::{t, uf};
use ui_model::{ni_apn_t, netinfo_t};

// Rows are fixed: 0 = 自动, 1..NI_MAX_CANDS = this card's candidates, APN_MAN0.. = manual.
const NI_MAX_APNS: usize = 10;
const NI_MAX_CANDS: usize = 6;
const APN_MAN0: usize = 1 + NI_MAX_CANDS;
const APN_ROWS: usize = APN_MAN0 + NI_MAX_APNS;
/// 「这张卡的候选」 caption
const APN_CAP_H: i32 = 24;
/// candidate rows' text and separator start here (under 自动's text)
const APN_INDENT: i32 = PAD + 26;
const APN_ROW_H: i32 = ROW2_H;
const APN_PEND_MS: u32 = 45000;

/// APN (蜂窝 → APN, 9-25): which APN the data connection dials; switch between 自动 and the
/// saved manual ones (two taps, 切换中 until it reads back). Making or editing one needs typing:
/// the web page. 10-03: with two or more candidates for this card they are listed under 自动,
/// one can be picked for this card only; two cards, 自动 APN (+ candidates) and 手动 APN.
pub struct Apn {
    scroll: Obj,
    now: Obj,
    now_sub: Obj,
    card: Obj,
    note: Obj,
    foot: Obj,
    sec2: Obj,
    card2: Obj,
    ccap: Obj,
    pub row: [Obj; APN_ROWS],
    mark: [Obj; APN_ROWS],
    name: [Obj; APN_ROWS],
    sub: [Obj; APN_ROWS],
    tag: [Obj; APN_ROWS],
    arm: u32,
    pub arm_idx: i32,
    /// 0 = 自动, 1.. = the row; -1 none
    pub pend: i32,
    pend_at: u32,
    pend_id: String,
    pend_name: String,
    pend_apn: String,
    /// a candidate: read back by APN name, not id (the id is the manual copy's)
    pend_cand: bool,
    pub flash: NetFlash,
    painted: i32,
}

impl Default for Apn {
    fn default() -> Self {
        let n = Obj::NULL;
        Apn {
            scroll: n,
            now: n,
            now_sub: n,
            card: n,
            note: n,
            foot: n,
            sec2: n,
            card2: n,
            ccap: n,
            row: [n; APN_ROWS],
            mark: [n; APN_ROWS],
            name: [n; APN_ROWS],
            sub: [n; APN_ROWS],
            tag: [n; APN_ROWS],
            arm: 0,
            arm_idx: -1,
            pend: -1,
            pend_at: 0,
            pend_id: String::new(),
            pend_name: String::new(),
            pend_apn: String::new(),
            pend_cand: false,
            flash: NetFlash::default(),
            painted: -2,
        }
    }
}

fn apn_pdp(pdp: i32) -> &'static str {
    match pdp {
        1 => "IPv4",
        2 => "IPv6",
        3 => "IPv4v6",
        _ => "",
    }
}

/// The candidates listed: only two or more are worth picking from.
fn apn_nc(n: &netinfo_t) -> usize {
    if n.apn_known != 0 && n.ncands >= 2 { n.ncands as usize } else { 0 }
}

/// Which APN row i is: None = 自动 or not there; the bool = a candidate.
fn apn_row_apn(n: &netinfo_t, i: usize) -> (Option<ni_apn_t>, bool) {
    if i == 0 {
        return (None, false);
    }
    if i < APN_MAN0 {
        return (if i <= apn_nc(n) { n.cands.get(i - 1).cloned() } else { None }, true);
    }
    (if i - APN_MAN0 < n.napns.max(0) as usize { n.apns.get(i - APN_MAN0).cloned() } else { None }, false)
}

/// Whether row i is the choice in effect (自动 mode = row 0; manual = the one selected;
/// a candidate: manual mode dialling that APN, picked for this card).
fn apn_row_current(n: &netinfo_t, i: usize) -> bool {
    let (a, cand) = apn_row_apn(n, i);
    if n.apn_known == 0 {
        return false;
    }
    if i == 0 {
        return n.apn_manual == 0;
    }
    let Some(a) = a else { return false };
    if cand {
        return n.apn_manual != 0 && !n.apn_picked.is_empty() && n.apn_in_use.apn == a.apn;
    }
    n.apn_manual != 0 && a.selected != 0
}

unsafe extern "C" fn apn_row_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let i = unsafe { lv::event_user(e) };
    super::with(|u| u.apn_row_tap(i));
}

impl Ui {
    /// A card of 「›」 rows on a tab: each row opens a subpage; refresh writes the small text
    /// on its right. section and names are n_() literals, shown through tr here.
    pub(super) fn nav_card(&mut self, p: Obj, y: i32, section: &'static str, ids: &[Sub], names: &[&'static str]) -> i32 {
        let n = ids.len() as i32;
        self.nav_sec = kit::section(p, y, tr(section));
        let c = kit::card(p, MARGIN, y + 20, CARD_W, n * ROW_H);
        self.nav_card = c;
        for (k, (&id, &name)) in ids.iter().zip(names).enumerate() {
            self.tile_sub[id as usize] = kit::row_nav(c, k as i32 * ROW_H, tr(name), k == 0, Some(super::tile_click_cb), id as usize);
        }
        y + 20 + n * ROW_H + 10
    }

    fn apn_row_tap(&mut self, i: usize) {
        let th = t();
        let m = self.model.clone();
        let def = netinfo_t::default();
        let n = m.netinfo.as_ref().unwrap_or(&def);
        let (a, cand) = apn_row_apn(n, i);
        if n.apn_known == 0 {
            self.apn.flash.set(th.t2, 3000, tr("还没读到 APN，稍等").into());
        } else if self.apn.pend >= 0 {
            let t = cf!(tr("正在切换到「%s」，稍等"), &self.apn.pend_name);
            self.apn.flash.set(th.t2, 3000, t);
        } else if apn_row_current(n, i) {
            self.apn.arm_idx = -1;
            if i == 0 {
                self.apn.flash.set(th.t2, 4000, tr("现在已经是自动选择").into());
            } else {
                let t = cf!(tr("已经在用「%s」"), a.as_ref().map_or("", |a| a.name.as_str()));
                self.apn.flash.set(th.t2, 4000, t);
            }
        } else if i > 0 && a.is_none() {
            return;
        } else if self.apn.arm_idx == i as i32 && net_confirm(self.apn.arm) {
            let ap = &mut self.apn;
            ap.arm_idx = -1;
            ap.pend = i as i32;
            ap.pend_at = lv::tick_get();
            ap.pend_id = a.as_ref().map_or("auto".to_string(), |a| a.id.clone());
            ap.pend_name = match &a {
                Some(a) => if a.name.is_empty() { a.apn.clone() } else { a.name.clone() },
                None => tr("自动").to_string(),
            };
            ap.pend_apn = a.as_ref().map_or(String::new(), |a| a.apn.clone());
            ap.pend_cand = cand;
            ap.flash.until = 0;
            self.apn_paint(true);
            lv::refr_now();
            let id = self.apn.pend_id.clone();
            self.src.netinfo_apn_use(&id);
            self.sync();
            let err = self.model.netinfo_action_error.clone().unwrap_or_default();
            if !err.is_empty() {
                self.apn.pend = -1;
                self.apn.flash.set(th.bad_t, 8000, cf!(tr("没切成：%s"), &err));
            } else {
                self.src.netinfo_hurry((APN_PEND_MS / 1000) as i32);
            }
        } else {
            self.apn.arm_idx = i as i32;
            self.apn.arm = lv::tick_get();
            self.apn.flash.until = 0;
        }
        self.apn_paint(true);
    }

    pub(super) fn build_sub_apn(&mut self, p: Obj) {
        let th = t();
        let f = uf();
        let sc = kit::scroll(p, 0, SUB_VIEW, 800);
        let ap = &mut self.apn;
        ap.scroll = sc;
        kit::section(sc, 4, tr("正在用"));
        let c = kit::card(sc, MARGIN, 24, CARD_W, HERO_H);
        ap.now = kit::label_w(c, f.cj17b, th.t1, PAD, 12, CARD_W - 2 * PAD, false, tr("读取中…"));
        ap.now_sub = kit::label_w(c, f.cj13, th.t2, PAD, 42, CARD_W - 2 * PAD, false, "");
        let y = 24 + HERO_H + 10;
        kit::section(sc, y, tr("自动 APN"));
        ap.card = kit::card(sc, MARGIN, y + 20, CARD_W, APN_ROW_H);
        ap.ccap = kit::label_w(ap.card, f.cj12, th.t3, APN_INDENT, APN_ROW_H + 6, CARD_W - APN_INDENT - PAD, false, tr("这张卡的候选（运营商库）"));
        ap.sec2 = kit::section(sc, y, tr("手动 APN"));
        ap.card2 = kit::card(sc, MARGIN, y + 20, CARD_W, APN_ROW_H);
        for i in 0..APN_ROWS {
            let cand = i > 0 && i < APN_MAN0;
            let c = if i < APN_MAN0 { ap.card } else { ap.card2 };
            let r = kit::bx(c, 0, 0, CARD_W, APN_ROW_H, th.card, 0);
            r.set_style_bg_opa(lv::OPA_TRANSP, 0);
            if cand {
                kit::bx(r, APN_INDENT, 0, CARD_W - APN_INDENT, 1, th.sep, 0);
            } else if i > APN_MAN0 {
                kit::sep(r, 0);
            }
            ap.mark[i] = kit::bx(r, PAD, (APN_ROW_H - 16) / 2, 16, 16, th.card, 8);
            ap.mark[i].set_style_border_width(2, 0);
            kit::show(ap.mark[i], !cand);
            ap.name[i] = kit::label_w(r, f.cj14, th.t1, APN_INDENT, 6, 170, false, "");
            ap.sub[i] = kit::label_w(r, f.cj12, th.t3, APN_INDENT, 27, 170, false, "");
            ap.tag[i] = kit::label_r(r, f.cj13, th.t3, CARD_W - PAD, 16, "");
            kit::tappable(r, Some(apn_row_cb), i);
            kit::show(r, i == 0);
            ap.row[i] = r;
        }
        ap.note = kit::label_w(sc, f.cj12, th.t3, MARGIN + 6, 0, CARD_W - 12, true, "");
        ap.foot = kit::label_w(sc, f.cj12, th.t3, MARGIN + 6, 0, CARD_W - 12, true, tr("新建或修改 APN 请用管理网页的「APN」页"));
        sc.scroll_to_y(0);
    }

    fn apn_paint(&mut self, changed: bool) {
        let th = t();
        let m = self.model.clone();
        let def = netinfo_t::default();
        let n = m.netinfo.as_ref().unwrap_or(&def);
        let arm = if self.apn.arm_idx >= 0 && net_armed(self.apn.arm) { self.apn.arm_idx } else { -1 };

        if self.apn.pend >= 0 {
            let ap = &mut self.apn;
            let done = if ap.pend == 0 {
                n.apn_known != 0 && n.apn_manual == 0
            } else if ap.pend_cand {
                n.apn_manual != 0 && n.apn_in_use.apn == ap.pend_apn
            } else {
                n.apn_manual != 0 && n.apn_in_use.id == ap.pend_id
            };
            if done {
                let nm = if n.apn_in_use.name.is_empty() { ap.pend_name.clone() } else { n.apn_in_use.name.clone() };
                ap.flash.set(th.ok_t, 6000, cf!(tr("已切换：现在用「%s」"), &nm));
                ap.pend = -1;
            } else if !n.apn_switch_err.is_empty() {
                ap.flash.set(th.bad_t, 10000, cf!(tr("没切成：%s"), &n.apn_switch_err));
                ap.pend = -1;
            } else if lv::tick_get().wrapping_sub(ap.pend_at) > APN_PEND_MS {
                ap.flash.set(th.warn_t, 10000, tr("设备没确认这次切换，看一下上面「正在用」再试").into());
                ap.pend = -1;
            }
        }
        let key = arm + if self.apn.pend >= 0 { 1000 + self.apn.pend } else { 0 } + if self.apn.flash.on() { 10000 } else { 0 };
        if !changed && key == self.apn.painted {
            return;
        }
        self.apn.painted = key;

        // the row on 蜂窝
        let l = self.tile_sub[Sub::Apn as usize];
        if !l.is_null() {
            let buf: Vec<u8> = if n.apn_known == 0 {
                if n.err.is_empty() { "".into() } else { "—".into() }
            } else {
                cf!(
                    "%s · %s",
                    if n.apn_in_use.name.is_empty() { tr("未拨号") } else { n.apn_in_use.name.as_str() },
                    if n.apn_manual != 0 { tr("手动") } else { tr("自动") }
                )
            };
            self.set(l, &buf);
        }

        let ap = &self.apn;
        let (now_l, now_sub) = (ap.now, ap.now_sub);
        let buf: &str = if n.apn_known == 0 {
            if n.err.is_empty() { tr("读取中…") } else { tr("读不到 APN") }
        } else if n.apn_in_use.name.is_empty() {
            tr("没有拨号")
        } else {
            &n.apn_in_use.name
        };
        self.set(now_l, buf);
        let buf: Vec<u8> = if n.apn_known == 0 {
            Vec::new()
        } else if !n.apn_in_use.id.is_empty() {
            cf!(
                "%s%s%s · %s",
                &n.apn_in_use.apn,
                if n.apn_in_use.pdp != 0 { " · " } else { "" },
                apn_pdp(n.apn_in_use.pdp),
                if n.apn_manual != 0 { tr("手动指定") } else { tr("自动选择") }
            )
        } else {
            (if n.apn_manual != 0 { tr("手动模式") } else { tr("自动模式") }).into()
        };
        self.set(now_sub, &buf);

        let nc = apn_nc(n);
        let (mut y1, mut y2) = (0, 0); // where each card has got to
        for i in 0..APN_ROWS {
            let (a, cand) = apn_row_apn(n, i);
            let ap = &self.apn;
            let (row, mark, name, sub, tagl) = (ap.row[i], ap.mark[i], ap.name[i], ap.sub[i], ap.tag[i]);
            let show = i == 0 || (n.apn_known != 0 && a.is_some());
            kit::show(row, show);
            if !show {
                continue;
            }
            let sel = apn_row_current(n, i);
            let (mut tag, mut tag_col) = ("", th.t3);
            if i == 0 {
                self.set(name, tr("自动"));
                self.set(sub, tr("按 SIM 卡自动选"));
            } else if let Some(a) = &a {
                self.set(name, if a.name.is_empty() { &a.id } else { &a.name });
                self.set(sub, cf!("%s%s%s", &a.apn, if a.pdp != 0 { " · " } else { "" }, apn_pdp(a.pdp)));
                if a.in_use != 0 && a.iot != 0 {
                    (tag, tag_col) = (tr("在用 · 物联网"), th.warn_t);
                } else if a.in_use != 0 {
                    (tag, tag_col) = (tr("在用"), th.ok_t);
                } else if cand && sel {
                    (tag, tag_col) = (tr("本卡在用"), th.ok_t);
                } else if cand && a.iot != 0 {
                    (tag, tag_col) = (tr("物联网"), th.warn_t);
                } else if !cand && !n.apn_picked.is_empty() && a.id == n.apn_picked {
                    tag = tr("本卡");
                }
            }
            if i == 0 && sel && !n.apn_in_use.id.is_empty() {
                (tag, tag_col) = (tr("在用"), th.ok_t);
            }
            if self.apn.pend == i as i32 {
                (tag, tag_col) = (tr("切换中…"), th.acc_t);
            }
            if arm == i as i32 {
                (tag, tag_col) = (tr("再点一次确认"), th.warn_t);
            }
            kit::bg(row, th.wash_w);
            row.set_style_bg_opa(if arm == i as i32 { lv::OPA_COVER } else { lv::OPA_TRANSP }, 0);
            kit::bg(mark, th.fill_blue);
            // a candidate says 本卡在用 by its tag: the manual copy is the selected one
            let dot = sel && !cand;
            mark.set_style_bg_opa(if dot { lv::OPA_COVER } else { lv::OPA_TRANSP }, 0);
            mark.set_style_border_color(if dot { th.fill_blue } else { th.t3 }, 0);
            if i < APN_MAN0 {
                if i == 1 {
                    y1 += APN_CAP_H; // the caption before the candidates
                }
                row.set_y(y1);
                y1 += APN_ROW_H;
            } else {
                row.set_y(y2);
                y2 += APN_ROW_H;
            }
            self.set(tagl, tag);
            kit::text_color(tagl, tag_col);
        }

        let ap = &self.apn;
        let mut col = th.t3;
        let arm_u = arm.max(0) as usize;
        let buf: Vec<u8> = if ap.pend >= 0 {
            col = th.acc_t;
            cf!(tr("正在切换到「%s」…数据连接会重拨，断几秒"), &ap.pend_name)
        } else if ap.flash.on() {
            col = ap.flash.col;
            ap.flash.txt.clone()
        } else if arm == 0 {
            col = th.warn_t;
            tr("回到自动：按 SIM 卡选 APN，会断网几秒").into()
        } else if arm > 0 && arm_u < APN_MAN0 && arm_u <= nc {
            col = th.warn_t;
            cf!(tr("只给这张卡改用「%s」：会断网几秒；换卡会自动回到「自动」"), &n.cands[arm_u - 1].apn)
        } else if arm >= APN_MAN0 as i32 && arm_u - APN_MAN0 < n.napns.max(0) as usize {
            col = th.warn_t;
            cf!(tr("改用「%s」：会断网几秒，之后换卡也一直用它，点「自动」才恢复"), &n.apns[arm_u - APN_MAN0].name)
        } else if n.apn_known == 0 {
            (if n.err.is_empty() { tr("读取中…") } else { n.err.as_str() }).into()
        } else if !n.apn_notice.is_empty() {
            col = th.acc_t;
            n.apn_notice.clone().into()
        } else if n.apn_manual != 0 && !n.apn_picked.is_empty() && n.apn_in_use.id == n.apn_picked {
            cf!(tr("这张卡固定用「%s」，换卡会自动回到「自动」"), &n.apn_in_use.apn)
        } else if n.apn_manual == 0 && nc > 0 && !n.apn_in_use.id.is_empty() && n.apn_in_use.iot != 0 {
            col = th.warn_t;
            cf!(tr("正在用物联网 APN「%s」，可能上不了网；点一条候选可以只给这张卡换掉（两下确认）"), &n.apn_in_use.apn)
        } else if n.apn_manual == 0 && nc > 0 {
            cf!(tr("这张卡有 %d 个候选 APN，点一条可以只给这张卡固定用它（两下确认）"), nc)
        } else if n.napns == 0 {
            tr("还没有手动 APN。要用自定义 APN，先在管理网页里新建").into()
        } else if n.apn_manual != 0 {
            tr("手动：一直用选中的这条，换卡也不变").into()
        } else {
            tr("点一条手动 APN 可以改用它（两下确认）").into()
        };
        let note = ap.note;
        self.set(note, &buf);
        kit::text_color(note, col);

        // down the page: 自动 APN (+ candidates) → 手动 APN (only its caption when empty) →
        // the note → the footer
        let ap = &self.apn;
        kit::show(ap.ccap, nc > 0);
        let mut y = 24 + HERO_H + 10;
        ap.card.set_height(y1);
        y += 20 + y1 + 12;
        ap.sec2.set_y(y);
        kit::show(ap.card2, y2 > 0);
        if y2 > 0 {
            ap.card2.set_y(y + 20);
            ap.card2.set_height(y2);
            y += 20 + y2;
        } else {
            y += 18;
        }
        ap.note.set_y(y + 8);
        ap.note.update_layout();
        y += 8 + ap.note.height() + 10;
        ap.foot.set_y(y);
        kit::scroll_extent(ap.scroll, y + 40 + 16);
    }

    /// Draw the agent's netinfo (changed = a new answer, or the page that shows it came up).
    pub fn net_paint(&mut self, changed: bool) {
        let th = t();
        let f = uf();
        let m = self.model.clone();
        let def = netinfo_t::default();
        let n = m.netinfo.as_ref().unwrap_or(&def);
        let aerr = m.netinfo_action_error.as_deref().unwrap_or("");
        let net = &self.net;
        let arm: i32 = if net_armed(net.arm_scan) {
            100
        } else if net_armed(net.arm_auto) {
            101
        } else if net_armed(net.arm_nbr) {
            102
        } else if net.arm_sc_idx >= 0 && net_armed(net.arm_sc) {
            200 + net.arm_sc_idx
        } else if net.arm_idx >= 0 && net_armed(net.arm_op) {
            net.arm_idx
        } else {
            -1
        };
        let busy = net_busy(n);
        let nscenes = n.nscenes.max(0) as usize;
        let scene = |i: usize| n.scenes.get(i).cloned().unwrap_or_default();

        self.apn_paint(changed);

        // 情景切换: has it really gone over
        if self.net.sc_pend >= 0 {
            let net = &mut self.net;
            let done = if net.sc_pend == 0 {
                n.scene_pin.is_empty()
            } else {
                n.scene_pin == net.sc_pend_id && n.scene_current == net.sc_pend_id
            };
            if done {
                if net.sc_pend == 0 {
                    let cur = if n.scene_current.is_empty() { tr("判定中") } else { net_scene_name(n, &n.scene_current) };
                    net.sc_flash.set(th.ok_t, 6000, cf!(tr("已恢复自动判断 · 现在是「%s」"), cur));
                } else {
                    let t = cf!(tr("已切换到「%s」，一直保持到你点「自动」"), &net.sc_pend_name);
                    net.sc_flash.set(th.ok_t, 6000, t);
                }
                net.sc_pend = -1;
            } else if lv::tick_get().wrapping_sub(net.sc_pend_at) > super::network::SC_PEND_MS {
                if net.sc_pend != 0 && n.scene_pin == net.sc_pend_id {
                    let t = cf!(tr("已固定「%s」，但设备还没切过去；Wi-Fi 看门狗可能正在接管，过一会再看"), &net.sc_pend_name);
                    net.sc_flash.set(th.warn_t, 10000, t);
                } else {
                    net.sc_flash.set(th.warn_t, 10000, tr("设备没确认这次切换，再试一次").into());
                }
                net.sc_pend = -1;
            }
        }
        // what is drawn depends on more than the data: armed, switching, a flash
        let net = &mut self.net;
        let key = arm
            + if net.sc_pend >= 0 { 1000 + net.sc_pend } else { 0 }
            + if net.sc_flash.on() { 10000 } else { 0 }
            + if net.ms_flash.on() { 20000 } else { 0 };
        if !changed && key == net.painted_arm {
            return;
        }
        net.painted_arm = key;
        if arm < 0 {
            net.arm_idx = -1;
            net.arm_sc_idx = -1;
        }

        let err = self.net.err;
        self.set(err, &n.err);

        // 情景: 自动 + each scene (can be pinned)
        let mut scene_px = 0;
        {
            let (mut cur_name, mut cur_abroad) = ("", false);
            for s in n.scenes.iter().take(nscenes) {
                if s.id == n.scene_current {
                    cur_name = &s.name;
                    cur_abroad = s.abroad != 0;
                }
            }
            let usable = n.scene_known != 0 && n.scene_enabled != 0 && nscenes > 0;
            for i in 0..NET_SCENE_ROWS {
                let mut show = false;
                let (mut name, mut tag, mut when, mut does): (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) = (vec![], vec![], vec![], vec![]);
                let mut tag_col = th.t3;
                if i == 0 {
                    show = true;
                    name = tr("自动").into();
                    if usable {
                        when = tr("按下面每个情景的条件自己切换").into();
                        does = tr("条件变了，1–2 分钟内跟着换").into();
                    }
                    if !usable {
                        tag = "—".into();
                    } else if n.scene_pin.is_empty() {
                        tag = cf!(tr("现在：%s"), if cur_name.is_empty() { tr("判定中") } else { cur_name });
                        tag.truncate(47);
                        tag_col = th.acc_t;
                    }
                } else if i <= nscenes && usable {
                    let sc = scene(i - 1);
                    show = true;
                    name = if sc.name.is_empty() { sc.id.clone() } else { sc.name.clone() }.into();
                    when = sc.when.clone().into();
                    does = sc.does.clone().into();
                    if n.scene_pin == sc.id {
                        tag = tr("已固定").into();
                        tag_col = th.acc_t;
                    } else if n.scene_current == sc.id {
                        tag = tr("现在").into();
                        tag_col = th.acc_t;
                    } else if sc.wifi_off != 0 && does.is_empty() {
                        tag = tr("会关 Wi-Fi").into();
                    }
                }
                else if i == NET_SCENE_ROWS - 1 && usable && cur_abroad {
                    show = true;
                    name = tr("CHILL 出口").into();
                    tag = cf!("%s ›", m.chill_mode.as_deref().unwrap_or(""));
                    tag.truncate(47);
                }
                let is_exit = i == NET_SCENE_ROWS - 1;
                let sel = usable
                    && !is_exit
                    && if i == 0 { n.scene_pin.is_empty() } else { i <= nscenes && n.scene_pin == scene(i - 1).id };
                if self.net.sc_pend == i as i32 {
                    tag = tr("切换中…").into();
                    tag_col = th.acc_t;
                }
                if arm == 200 + i as i32 {
                    tag = tr("再点一次确认").into();
                    tag_col = th.warn_t;
                }
                let net = &self.net;
                let (row, mark, nm, tg, wh, ds) = (net.sc_row[i], net.sc_mark[i], net.sc_name[i], net.sc_tag[i], net.sc_when[i], net.sc_does[i]);
                kit::show(row, show);
                if !show {
                    continue;
                }
                // armed: the whole row washed (not a coloured left border, DESIGN.md §6)
                kit::bg(row, th.wash_w);
                row.set_style_bg_opa(if arm == 200 + i as i32 { lv::OPA_COVER } else { lv::OPA_TRANSP }, 0);
                // radio mark: filled = the one in effect (自动 when nothing is pinned)
                kit::show(mark, usable && !is_exit);
                kit::bg(mark, th.fill_blue);
                mark.set_style_bg_opa(if sel { lv::OPA_COVER } else { lv::OPA_TRANSP }, 0);
                mark.set_style_border_color(if sel { th.fill_blue } else { th.t3 }, 0);
                nm.set_x(if usable && !is_exit { PAD + 26 } else { PAD });
                // rows with words are three lines, the mark on the first; without, one line.
                // A long condition wraps to two lines, then ends in "…"
                let lh = f.cj12.line_height();
                let mut wl = 1;
                if !when.is_empty() {
                    let (_, h) = lv::text_size(&when, f.cj12, CARD_W - 2 * PAD - 26);
                    if h > lh {
                        wl = 2;
                    }
                }
                let rh = if when.is_empty() && does.is_empty() {
                    ROW_H
                } else {
                    32 + if when.is_empty() { 0 } else { wl * lh } + if does.is_empty() { 0 } else { lh + 2 } + 8
                };
                row.set_height(rh);
                mark.set_y(if rh == ROW_H { (ROW_H - 16) / 2 } else { 12 });
                wh.set_height(wl * lh);
                ds.set_y(32 + wl * lh + 2);
                self.set(wh, &when);
                self.set(ds, &does);
                kit::show(wh, !when.is_empty());
                kit::show(ds, !does.is_empty());
                row.set_y(scene_px);
                scene_px += rh;
                self.set(nm, &name);
                self.set(tg, &tag);
                kit::text_color(tg, tag_col);
            }
            let net = &self.net;
            let mut note_col = th.t3;
            let buf: Vec<u8> = if n.scene_known == 0 {
                if n.err.is_empty() { tr("读取中…") } else { tr("读不到情景") }.into()
            } else if n.scene_enabled == 0 {
                tr("情景引擎已停用，在管理网页的「情景」页打开").into()
            } else if nscenes == 0 {
                tr("还没配置情景，在管理网页的「情景」页设置").into()
            } else if net.sc_pend > 0 {
                note_col = th.acc_t;
                cf!(
                    tr("正在切换到「%s」…%s"),
                    &net.sc_pend_name,
                    if net.sc_pend_wifi_off { tr("会关掉 Wi-Fi") } else { tr("开关 Wi-Fi 要 10–30 秒") }
                )
            } else if net.sc_pend == 0 {
                note_col = th.acc_t;
                tr("正在恢复自动判断…").into()
            } else if net.sc_flash.on() {
                note_col = net.sc_flash.col;
                net.sc_flash.txt.clone()
            } else if n.scene_takeover != 0 && !n.scene_pin.is_empty() {
                note_col = th.warn_t;
                tr("Wi-Fi 看门狗接管中，固定暂时不生效，先按「外出」").into()
            } else if arm == 200 {
                note_col = th.warn_t;
                tr("恢复自动：按位置和 SIM 重新判断\n可能会开关 Wi-Fi").into()
            } else if arm > 200 && (arm - 200) as usize <= nscenes {
                let sc = scene((arm - 201) as usize);
                note_col = th.warn_t;
                cf!(
                    tr("切到「%s」：%s\n之后一直固定，点「自动」才恢复"),
                    if sc.name.is_empty() { &sc.id } else { &sc.name },
                    if sc.wifi_off != 0 {
                        tr("会关掉 Wi-Fi")
                    } else if sc.abroad != 0 {
                        tr("按国外设置")
                    } else {
                        tr("Wi-Fi 开着")
                    }
                )
            } else if !n.scene_pin.is_empty() {
                tr("手动固定后一直保持（重启也是），点「自动」才恢复自动判断").into()
            } else {
                tr("情景只管 Wi-Fi、CHILL 这些，不碰蜂窝网络。点一个情景就固定在它，点「自动」恢复").into()
            };
            let note = net.sc_note;
            self.set(note, &buf);
            kit::text_color(note, note_col);
        }

        // 出口: CHILL off → one row; on with the same exit (direct main group) → one row, noted;
        // different → two rows
        let two = n.proxy.present != 0 && n.proxy.ip != n.direct.ip;
        let same = n.proxy.present != 0 && !two;
        for i in 0..2 {
            let e = if i == 1 { &n.proxy } else { &n.direct };
            // 直连 · AI 不动: the agent looked up the 🤖 AI group's exit
            let key: &str = if i == 1 {
                if m.chill_exit_raw.as_deref().unwrap_or("") != "direct_keep_ai" { "CHILL" } else { "CHILL · AI" }
            } else if two {
                tr("蜂窝直连")
            } else {
                tr("出口 IP")
            };
            let mut extra: Vec<u8> = if i == 1 {
                e.node.clone().into()
            } else if same {
                cf!(tr("%s%sCHILL 直连"), &e.isp, if e.isp.is_empty() { "" } else { " · " })
            } else {
                e.isp.clone().into()
            };
            extra.truncate(79);
            let buf = net_exit_sub(e, &extra);
            let net = &self.net;
            let (k, ip, sub) = (net.ex_key[i], net.ex_ip[i], net.ex_sub[i]);
            self.set(k, key);
            self.set(
                ip,
                if !e.ip.is_empty() {
                    e.ip.as_str()
                } else if e.present != 0 || !n.err.is_empty() {
                    "—"
                } else {
                    tr("查询中…")
                },
            );
            self.set(sub, &buf);
        }
        kit::show(self.net.ex_row[1], two);

        // 运营商
        let op = self.net.op;
        self.set(op[0], net_oper_text(&n.home));
        self.set(op[1], net_oper_text(&n.serving));
        self.set(op[2], if n.roaming > 0 { tr("漫游中") } else if n.roaming == 0 { tr("本地") } else { "—" });
        kit::text_color(op[2], if n.roaming > 0 { th.warn_t } else { th.t1 });
        let buf: Vec<u8> = if n.guard_phase == "registering" {
            cf!(tr("正在注册 %s…"), &n.guard_target)
        } else if n.guard_phase == "reverting" {
            tr("正在恢复自动…").into()
        } else if n.selection == "auto" {
            tr("自动").into()
        } else if n.selection == "manual" {
            tr("手动").into()
        } else {
            "—".into()
        };
        self.set(op[3], &buf);

        // 手动选网: one sentence, what is happening first
        let ops_n = n.nops.max(0) as usize;
        let op_at = |i: usize| n.ops.get(i).cloned().unwrap_or_default();
        let net = &self.net;
        let mut st_col = th.t2;
        let mut buf: Vec<u8> = if net.ms_flash.on() {
            st_col = net.ms_flash.col;
            net.ms_flash.txt.clone()
        } else if n.guard_phase == "registering" {
            cf!(tr("正在注册到 %s。没注册上会自动回到自动选网。"), &n.guard_target)
        } else if n.guard_phase == "reverting" {
            tr("正在回到自动选网…").into()
        } else if n.scan_state == "scanning" {
            tr("正在搜索，数据连接会断 1–3 分钟…").into()
        } else if arm == 100 {
            st_col = th.warn_t;
            tr("搜索时会断网 1–3 分钟，再点一次开始。").into()
        } else if arm == 101 {
            st_col = th.warn_t;
            tr("回到自动选网，可能短暂断网，再点一次确认。").into()
        } else if arm >= 0 && (arm as usize) < ops_n {
            // only an operator row; scenes (200+) and buttons (100+) are not
            let o = op_at(arm as usize);
            st_col = th.warn_t;
            cf!(
                if o.status == "2" {
                    tr("再点一次把选网固定在 %s（改成手动）。要回自动点「恢复自动」。")
                } else {
                    tr("再点一次注册到 %s。没注册上会自动回到自动选网。")
                },
                if o.name.is_empty() { &o.plmn } else { &o.name }
            )
        } else if !aerr.is_empty() {
            st_col = th.bad_t;
            aerr.into()
        } else if n.guard_phase == "revert_failed" {
            st_col = th.bad_t;
            cf!(tr("%s。重启设备也会回到自动选网。"), &n.guard_reason)
        } else if n.guard_phase == "reverted" {
            // the user asked for it: no reason to give. By code; an agent from before L2 has
            // none and its reason is still the Chinese
            let asked = if n.guard_reason_code.is_empty() { n.guard_reason == "手动恢复自动" } else { n.guard_reason_code == "manual_auto" };
            if asked { tr("已回到自动选网。").into() } else { cf!(tr("%s，已回到自动选网。"), &n.guard_reason) }
        } else if n.guard_phase == "ok" {
            st_col = th.ok_t;
            cf!(tr("已注册到 %s。要回自动选网点「恢复自动」。"), &n.guard_target)
        } else if n.scan_state == "error" {
            st_col = th.bad_t;
            cf!(tr("搜索失败：%s"), &n.scan_err)
        } else if n.scan_state == "done" {
            tr("点一个网络，再点一次确认注册。").into()
        } else {
            tr("手动指定注册的网络。搜索会断网 1–3 分钟。").into()
        };
        buf.truncate(255);
        // a sentence that does not fit drops its full stop alone to the second line: none
        if buf.ends_with("。".as_bytes()) {
            buf.truncate(buf.len() - 3);
        } else if lang::is_en() && buf.ends_with(b".") {
            buf.pop();
        }
        let status = net.status;
        self.set(status, &buf);
        kit::text_color(status, st_col);

        let ops = if n.scan_state == "done" && !busy { ops_n } else { 0 };
        for i in 0..NI_MAX_OPS {
            let net = &self.net;
            let (r, nm, det, tg) = (net.opr[i], net.opr_name[i], net.opr_det[i], net.opr_tag[i]);
            if i >= ops {
                kit::show(r, false);
                continue;
            }
            let o = op_at(i);
            kit::show(r, true);
            self.set(nm, if o.name.is_empty() { &o.plmn } else { &o.name });
            let d = cf!("%s · %s%s%s", &o.plmn, net_rat_name(&o.rat), if o.country.is_empty() { "" } else { " · " }, &o.country);
            self.set(det, &d);
            let tag = if arm == i as i32 {
                tr("再点一次确认")
            } else if o.status == "2" {
                tr("当前")
            } else if o.status == "3" {
                tr("禁止")
            } else {
                ""
            };
            kit::bg(r, th.wash_w);
            r.set_style_bg_opa(if arm == i as i32 { lv::OPA_COVER } else { lv::OPA_TRANSP }, 0);
            self.set(tg, tag);
            kit::text_color(tg, if arm == i as i32 { th.warn_t } else { th.t3 });
            kit::text_color(nm, if o.status == "3" { th.t3 } else { th.t1 });
        }
        let net = &self.net;
        let (sl, sb, al, ab) = (net.scan_lbl, net.scan_btn, net.auto_lbl, net.auto_btn);
        self.set(sl, if arm == 100 { tr("再点开始") } else { tr("搜索网络") });
        kit::button_kind(sb, sl, if arm == 100 { BtnKind::Armed } else { BtnKind::Plain });
        self.set(al, if arm == 101 { tr("再点确认") } else { tr("恢复自动") });
        kit::button_kind(ab, al, if arm == 101 { BtnKind::Armed } else { BtnKind::Plain });

        // 邻小区. The vendor scan cuts the data and gets nothing (2026-09-25), the agent says
        // unsupported: no button, the reason instead
        let nbr_off = n.nbr_state == "unsupported";
        let net = &self.net;
        let (ns, nb, nl) = (net.nbr_state, net.nbr_btn, net.nbr_lbl);
        kit::show(nb, !nbr_off);
        ns.set_width(CARD_W - 2 * PAD - if nbr_off { 0 } else { 80 });
        let buf: Vec<u8> = if nbr_off {
            if n.nbr_err.is_empty() { tr("本机暂时读不到邻区").into() } else { n.nbr_err.clone().into() }
        } else if n.nbr_state == "scanning" {
            tr("扫描中…").into()
        } else if arm == 102 {
            tr("可能短暂影响网速").into()
        } else if n.nbr_state == "error" {
            tr("扫描失败").into()
        } else if n.nbr_at > 0 {
            // device clock = local time labelled UTC: local() gives the right numbers
            let hm = clock::strftime("%H:%M", &clock::local(n.nbr_at));
            cf!(tr("%s 扫描 · %d 个"), &hm, n.ncells)
        } else {
            tr("还没扫过，点「扫描」读一次").into()
        };
        self.set(ns, &buf);
        kit::text_color(ns, if arm == 102 { th.warn_t } else { th.t2 });
        self.set(nl, if arm == 102 { tr("再点开始") } else { tr("扫描") });
        kit::button_kind(nb, nl, if arm == 102 { BtnKind::Armed } else { BtnKind::Plain });
        let ncells = n.ncells.max(0) as usize;
        for i in 0..NI_MAX_CELLS {
            let net = &self.net;
            let (r, l, rr) = (net.nbr_row[i], net.nbr_l[i], net.nbr_r[i]);
            if i >= ncells {
                kit::show(r, false);
                continue;
            }
            let cl = n.cells.get(i).cloned().unwrap_or_default();
            kit::show(r, true);
            self.set(l, cf!("%-3s PCI %s  %s", &cl.rat, &cl.pci, &cl.arfcn));
            self.set(rr, cf!("%s%s", if cl.rsrp.is_empty() { "-" } else { cl.rsrp.as_str() }, if cl.rsrp.is_empty() { "" } else { " dBm" }));
        }

        self.net_reflow(if n.err.is_empty() { 0 } else { 22 }, scene_px, if two { 2 } else { 1 }, ops as i32, n.ncells);
    }
}
