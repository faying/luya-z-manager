//! 锁频 (src/ui_parts/lock.c): one band picker per technology and 恢复默认 on the 锁频 page,
//! and the 网络模式 segment on 蜂窝. Everything that reaches the modem is two taps (arm, then
//! send within 5 s): a mis-tap drops the connection.
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use super::cellular::nz;
use super::{Ui, SUB_VIEW};
use crate::cf;
use crate::kit::{self, BtnKind, Seg, CARD_W, MARGIN, PAD};
use crate::lang::{n_, tr};
use crate::lv::{self, sys, Obj};
use crate::theme::{t, uf};

/// 网络模式: the vendor web page's values and order (5G SA must stay at index 2: the
/// abroad warning looks at it).
pub const LK_MODES: usize = 5;
pub const LK_MODE_V: [&str; LK_MODES] = ["WL_AND_5G", "LTE_AND_5G", "Only_5G", "WCDMA_AND_LTE", "Only_LTE"];
/// tr() where shown (kit::seg does)
pub const LK_MODE_N: [&str; LK_MODES] = [n_("自动"), "5G NSA", "5G SA", "4G+3G", "4G"];
const LK_MODE_WAIT_MS: u32 = 20000;
pub const BAND_MAX: usize = 28;
pub const BG_SA: usize = 0;
pub const BG_NSA: usize = 1;
pub const BG_LTE: usize = 2;

#[derive(Clone)]
pub struct BandGroup {
    pub card: Obj,
    pub sec: Obj,
    pub summary: Obj,
    pub apply_btn: Obj,
    pub apply_lbl: Obj,
    pub chip: [Obj; BAND_MAX],
    pub chip_lbl: [Obj; BAND_MAX],
    pub band_no: [i32; BAND_MAX],
    pub sel: [bool; BAND_MAX],
    pub n: usize,
    pub prefix: u8,
    /// "supported|locked" the chips were built from
    pub last_csv: String,
    /// two-tap confirm
    pub arm: u32,
    /// 已下发… since (0 = not waiting)
    pub sent: u32,
}

impl Default for BandGroup {
    fn default() -> Self {
        BandGroup {
            card: Obj::NULL,
            sec: Obj::NULL,
            summary: Obj::NULL,
            apply_btn: Obj::NULL,
            apply_lbl: Obj::NULL,
            chip: [Obj::NULL; BAND_MAX],
            chip_lbl: [Obj::NULL; BAND_MAX],
            band_no: [0; BAND_MAX],
            sel: [false; BAND_MAX],
            n: 0,
            prefix: b'n',
            last_csv: String::new(),
            arm: 0,
            sent: 0,
        }
    }
}

pub struct Lock {
    pub seg: Seg,
    pub mode_lbl: Obj,
    pub reset_lbl: Obj,
    pub reset_btn: Obj,
    pub reset_card: Obj,
    pub reset_sec: Obj,
    pub scroll: Obj,
    pub mode_arm: u32,
    pub reset_arm: u32,
    pub mode_pending: i32,
    /// sent, waiting for the read-back: the target and when (-1 = not waiting)
    pub mode_want: i32,
    pub mode_sent: u32,
    /// the mode last read back (index, -1 = not in this row or never read)
    pub mode_real: i32,
    pub bg: [BandGroup; 3],
    /// E4: the line last taken from the core's transaction, "" none
    c_op: String,
}

impl Default for Lock {
    fn default() -> Self {
        Lock {
            seg: Seg::default(),
            mode_lbl: Obj::NULL,
            reset_lbl: Obj::NULL,
            reset_btn: Obj::NULL,
            reset_card: Obj::NULL,
            reset_sec: Obj::NULL,
            scroll: Obj::NULL,
            mode_arm: 0,
            reset_arm: 0,
            mode_pending: -1,
            mode_want: -1,
            mode_sent: 0,
            mode_real: -1,
            bg: Default::default(),
            c_op: String::new(),
        }
    }
}

pub(super) unsafe extern "C" fn lk_mode_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let i = unsafe { lv::event_user(e) };
    super::with(|u| u.lk_mode_tap(i as i32));
}

unsafe extern "C" fn band_chip_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let code = unsafe { lv::event_user(e) };
    super::with(|u| u.band_chip_tap(code / 64, code % 64));
}

unsafe extern "C" fn band_apply_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let gi = unsafe { lv::event_user(e) };
    super::with(|u| u.band_apply_tap(gi));
}

unsafe extern "C" fn lk_reset_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| u.lk_reset_tap());
}

/// C atoi: leading blanks, a sign, digits.
fn atoi(s: &str) -> i32 {
    super::atol(s) as i32
}

/// Whether the comma list csv has band b.
fn band_in_csv(csv: &str, b: i32) -> bool {
    csv.split(',').any(|p| atoi(p) == b)
}

impl Ui {
    fn band_group_apply(&mut self, gi: usize) {
        let g = &self.lk.bg[gi];
        let csv: Vec<String> = (0..g.n).filter(|&i| g.sel[i]).map(|i| g.band_no[i].to_string()).collect();
        if csv.is_empty() {
            return; // locking zero bands would strand the modem
        }
        let csv = csv.join(",");
        let action = match gi {
            BG_LTE => "band.set_lte",
            BG_SA => "band.set_nr_sa",
            _ => "band.set_nr_nsa",
        };
        self.data_control(action, &format!("{{\"bands\":\"{csv}\"}}"), Some(&format!("bands={csv}")));
    }

    fn band_summary_set(&self, gi: usize) {
        let g = &self.lk.bg[gi];
        let mut buf = String::new();
        let (mut n, mut all) = (0, true);
        for i in 0..g.n {
            if !g.sel[i] {
                all = false;
                continue;
            }
            n += 1;
            if buf.len() < 200 - 8 {
                if !buf.is_empty() {
                    buf.push(' ');
                }
                buf.push(g.prefix as char);
                buf.push_str(&g.band_no[i].to_string());
            }
        }
        g.summary.set_text(if g.n == 0 {
            "—"
        } else if all {
            tr("全部频段（未锁定）")
        } else if n == 0 {
            tr("未选频段")
        } else {
            buf.as_str()
        });
    }

    fn band_chip_paint(&self, gi: usize, i: usize) {
        let g = &self.lk.bg[gi];
        // every band selected = not locked, the default: muted, since 21 saturated chips would
        // read as 21 alarms. A real lock selection is fillBlue with white text.
        let all = (0..g.n).all(|k| g.sel[k]);
        if g.sel[i] && all {
            kit::bg(g.chip[i], t().acc_s);
            kit::text_color(g.chip_lbl[i], t().acc_t);
        } else {
            kit::chip_set(g.chip[i], g.chip_lbl[i], g.sel[i]);
        }
    }

    fn band_chip_tap(&mut self, gi: usize, i: usize) {
        if gi > 2 || i >= self.lk.bg[gi].n {
            return;
        }
        self.lk.bg[gi].sel[i] = !self.lk.bg[gi].sel[i];
        for k in 0..self.lk.bg[gi].n {
            self.band_chip_paint(gi, k);
        }
        self.band_summary_set(gi);
    }

    fn band_apply_tap(&mut self, gi: usize) {
        let now = lv::tick_get();
        let g = &mut self.lk.bg[gi];
        if g.arm != 0 && now.wrapping_sub(g.arm) < 5000 {
            g.arm = 0;
            g.apply_lbl.set_text(tr("已下发…"));
            kit::button_kind(g.apply_btn, g.apply_lbl, BtnKind::Plain);
            g.sent = nz(now);
            self.band_group_apply(gi);
            return;
        }
        g.arm = nz(now);
        g.apply_lbl.set_text(tr("再按一次确认"));
        kit::button_kind(g.apply_btn, g.apply_lbl, BtnKind::Armed);
    }

    fn lk_mode_tap(&mut self, idx: i32) {
        let now = lv::tick_get();
        let lk = &mut self.lk;
        if lk.mode_want >= 0 {
            // the last one has not read back: no second on top, the highlight stays on it
            lk.mode_lbl.set_text(tr("正在切换，等设备确认…"));
            kit::seg_set(&mut lk.seg, lk.mode_want);
            return;
        }
        if lk.mode_pending == idx && lk.mode_arm != 0 && now.wrapping_sub(lk.mode_arm) < 5000 {
            let v = LK_MODE_V[idx as usize];
            lk.mode_arm = 0;
            lk.mode_pending = -1;
            // the highlight stays on the target until it reads back (已切到) or times out
            lk.mode_want = idx;
            lk.mode_sent = nz(now);
            kit::seg_set(&mut lk.seg, idx);
            lk.mode_lbl.set_text(tr("已下发，等设备确认…"));
            self.data_control("network.set_mode", &format!("{{\"mode\":\"{v}\"}}"), Some(&format!("mode={v}")));
            return;
        }
        lk.mode_arm = nz(now);
        lk.mode_pending = idx;
        // 5G SA only: many countries have no SA and roaming SIMs often can't use it
        lk.mode_lbl.set_text(if idx == 2 { tr("再按一次确认 · 国外和漫游卡常没有 SA") } else { tr("再按一次确认切换") });
        kit::seg_set(&mut lk.seg, -1);
        kit::seg_arm(&lk.seg, idx);
    }

    /// Every refresh, core or not (a tap always gets an ending): highlight the mode the modem
    /// is on, close the confirm window, finish a wait for the read-back. sel = the device's
    /// mode as an index; known = false when core can't be read (the highlight then stays on
    /// the last mode read back). Skipped while a tap is armed.
    pub(super) fn lk_mode_sync(&mut self, mut sel: i32, known: bool) {
        let now = lv::tick_get();
        if known {
            self.lk.mode_real = sel;
        } else {
            sel = self.lk.mode_real;
        }
        let lk = &mut self.lk;
        if lk.mode_pending >= 0 {
            if now.wrapping_sub(lk.mode_arm) >= 5000 {
                lk.mode_pending = -1; // confirm window lapsed
                lk.seg.sel = -2; // repaint the real selection next tick
                lk.mode_lbl.set_text(tr("切换会短暂断网，需要按两次确认"));
            }
            return;
        }
        if lk.mode_want >= 0 || self.op.view.active.have || !lk.c_op.is_empty() {
            // E4: while core has a transaction for this item, its verdict and countdown (from
            // the screen or the web page); a result only if it ended after this tap
            let since = if lk.mode_want >= 0 {
                nz(lk.mode_sent)
            } else if !lk.c_op.is_empty() {
                1
            } else {
                0
            };
            if let Some(ol) = self.op_mode_line(since) {
                let a = &self.op.view.active;
                let lk = &mut self.lk;
                if ol != lk.c_op {
                    lk.c_op = ol;
                    lk.mode_lbl.set_text(&lk.c_op);
                }
                if a.have && a.item == "network.mode" {
                    if lk.mode_want >= 0 {
                        sel = lk.mode_want;
                    }
                } else {
                    lk.mode_want = -1;
                }
                if sel != lk.seg.sel {
                    kit::seg_set(&mut lk.seg, sel);
                }
                return;
            }
            let lk = &mut self.lk;
            if !lk.c_op.is_empty() && lk.mode_want < 0 {
                lk.mode_lbl.set_text(tr("切换会短暂断网，需要按两次确认"));
            }
            lk.c_op.clear();
        }
        let lk = &mut self.lk;
        if lk.mode_want >= 0 {
            // read-back: 已切到 when it is there; on timeout say it did not take, show the real
            // mode. After a few baseband crashes the vendor switch only logs: reboot, then switch.
            if known && sel == lk.mode_want {
                lk.mode_lbl.set_text(cf!(tr("已切到「%s」"), tr(LK_MODE_N[sel as usize])));
                lk.mode_want = -1;
            } else if now.wrapping_sub(lk.mode_sent) >= LK_MODE_WAIT_MS {
                if known && sel >= 0 {
                    lk.mode_lbl.set_text(cf!(tr("没切成，设备还是「%s」。\n再试一次，不行就重启设备、开机后马上切"), tr(LK_MODE_N[sel as usize])));
                } else {
                    lk.mode_lbl.set_text(tr("没切成，设备没确认。\n再试一次，不行就重启设备、开机后马上切"));
                }
                lk.mode_want = -1;
            } else {
                sel = lk.mode_want;
            }
        }
        if sel != lk.seg.sel {
            kit::seg_set(&mut lk.seg, sel);
        }
    }

    fn lk_reset_tap(&mut self) {
        let now = lv::tick_get();
        let lk = &mut self.lk;
        if lk.reset_arm != 0 && now.wrapping_sub(lk.reset_arm) < 5000 {
            lk.reset_arm = 0;
            lk.reset_lbl.set_text(tr("已恢复默认"));
            kit::button_kind(lk.reset_btn, lk.reset_lbl, BtnKind::Danger);
            self.data_control("band.reset", "{}", Some(""));
            return;
        }
        lk.reset_arm = nz(now);
        lk.reset_lbl.set_text(tr("再按一次确认"));
        kit::button_kind(lk.reset_btn, lk.reset_lbl, BtnKind::Armed);
    }

    /// Chips flow left to right, as many per line as fit; the card grows with them.
    fn band_group_layout(&mut self, gi: usize) -> i32 {
        let g = &self.lk.bg[gi];
        let (mut x, mut y, maxx) = (PAD, 36, CARD_W - PAD);
        for i in 0..g.n {
            g.chip_lbl[i].update_layout();
            let w = g.chip_lbl[i].width() + 22;
            g.chip[i].set_width(w);
            if x + w > maxx {
                x = PAD;
                y += 36;
            }
            g.chip[i].set_pos(x, y);
            x += w + 6;
        }
        y += if g.n > 0 { 36 } else { 0 };
        g.apply_btn.set_y(y + 4);
        let h = y + 4 + 36 + 12;
        g.card.set_height(h);
        h
    }

    fn lock_reflow(&mut self) {
        let lk = &self.lk;
        let mut y = 4; // 网络模式 moved to 蜂窝 (9-25): only the bands here
        for g in &lk.bg {
            g.sec.set_y(y);
            g.card.set_y(y + 20);
            y += 20 + g.card.style_height(0) + 10;
        }
        lk.reset_sec.set_y(y);
        lk.reset_card.set_y(y + 20);
        kit::scroll_extent(lk.scroll, y + 20 + 88 + 16);
    }

    fn band_group_build(&mut self, p: Obj, gi: usize, title: &'static str, prefix: u8) {
        let th = t();
        let g = &mut self.lk.bg[gi];
        g.prefix = prefix;
        g.sec = kit::section(p, 0, tr(title));
        g.card = kit::card(p, MARGIN, 0, CARD_W, 100);
        g.summary = kit::label_w(g.card, uf().cj13, th.t2, PAD, 11, CARD_W - 2 * PAD, false, "—");
        for i in 0..BAND_MAX {
            let (c, l) = kit::chip(g.card, PAD, 36, "");
            kit::tappable(c, Some(band_chip_cb), gi * 64 + i);
            kit::show(c, false);
            g.chip[i] = c;
            g.chip_lbl[i] = l;
        }
        (g.apply_btn, g.apply_lbl) = kit::button(g.card, PAD, 40, CARD_W - 2 * PAD, 36, tr("应用锁频"), BtnKind::Plain, Some(band_apply_cb), gi);
        self.band_group_layout(gi);
    }

    pub(super) fn build_sub_lock(&mut self, p: Obj) {
        let sc = kit::scroll(p, 0, SUB_VIEW, 1400);
        self.lk.scroll = sc;
        self.band_group_build(sc, BG_SA, n_("5G SA 频段"), b'n');
        self.band_group_build(sc, BG_NSA, n_("5G NSA 频段"), b'n');
        self.band_group_build(sc, BG_LTE, n_("4G 频段"), b'B');
        let th = t();
        let lk = &mut self.lk;
        lk.reset_sec = kit::section(sc, 0, tr("恢复"));
        lk.reset_card = kit::card(sc, MARGIN, 0, CARD_W, 88);
        (lk.reset_btn, lk.reset_lbl) =
            kit::button(lk.reset_card, PAD, 12, CARD_W - 2 * PAD, 36, tr("恢复默认配置"), BtnKind::Danger, Some(lk_reset_cb), 0);
        kit::label_w(lk.reset_card, uf().cj12, th.t3, PAD, 58, CARD_W - 2 * PAD, false, tr("清掉全部锁频和锁小区设置，回到自动选网"));
        self.lock_reflow();
        sc.scroll_to_y(0);
    }

    /// Chips = every band the modem supports; selected = the current lock set. sup is empty
    /// on a core without the vendor defaults: fall back to the lock set.
    pub(super) fn band_group_sync(&mut self, gi: usize, sup: &str, lock: &str) {
        let sup = if sup.is_empty() { lock } else { sup };
        let key = format!("{sup}|{lock}");
        let g = &mut self.lk.bg[gi];
        if g.last_csv == key {
            return;
        }
        g.last_csv = key;
        g.n = 0;
        for tk in sup.split(',').filter(|s| !s.is_empty()) {
            if g.n >= BAND_MAX {
                break;
            }
            let b = atoi(tk);
            if b <= 0 {
                continue;
            }
            let n = g.n;
            g.band_no[n] = b;
            g.sel[n] = lock.is_empty() || band_in_csv(lock, b); // all selected == not locked
            g.chip_lbl[n].set_text(cf!("%c%d", g.prefix as i32, b));
            g.chip[n].remove_flag(lv::LV_OBJ_FLAG_HIDDEN);
            g.n += 1;
        }
        let n = g.n;
        for i in n..BAND_MAX {
            g.chip[i].add_flag(lv::LV_OBJ_FLAG_HIDDEN);
        }
        // the lock read back changed: whatever 已下发… was waiting for is here
        if g.sent != 0 {
            g.sent = 0;
            g.apply_lbl.set_text(tr("应用锁频"));
        }
        for i in 0..n {
            self.band_chip_paint(gi, i);
        }
        self.band_summary_set(gi);
        self.band_group_layout(gi);
        self.lock_reflow();
    }

    /// The 锁频 part of the refresh: confirm windows, then the bands and the 网络模式 row.
    pub(super) fn lock_refresh(&mut self, sa_sup: &str, sa: &str, nsa_sup: &str, nsa: &str, lte_sup: &str, lte: &str, sel: i32) {
        let now = lv::tick_get();
        for g in &mut self.lk.bg {
            // confirm window over, or the lock never read back
            if g.arm != 0 && now.wrapping_sub(g.arm) >= 5000 {
                g.arm = 0;
                g.apply_lbl.set_text(tr("应用锁频"));
                kit::button_kind(g.apply_btn, g.apply_lbl, BtnKind::Plain);
            }
            if g.sent != 0 && now.wrapping_sub(g.sent) >= 30000 {
                g.sent = 0;
                g.apply_lbl.set_text(tr("没生效，可再试一次"));
            }
        }
        self.band_group_sync(super::lock::BG_SA, sa_sup, sa);
        self.band_group_sync(super::lock::BG_NSA, nsa_sup, nsa);
        self.band_group_sync(super::lock::BG_LTE, lte_sup, lte);
        self.lk_mode_sync(sel, true);
    }

    /// 已锁定 / 未锁定 for the 锁频 row; "" before the bands are known.
    pub(super) fn lock_word(&self) -> &'static str {
        let mut known = false;
        let mut locked = false;
        for g in &self.lk.bg {
            for i in 0..g.n {
                known = true;
                if !g.sel[i] {
                    locked = true;
                }
            }
        }
        if !known {
            ""
        } else if locked {
            tr("已锁定")
        } else {
            tr("未锁定")
        }
    }
}
