//! 流量 (tapped from Home's 今日 cell; manager docs/designs/travel-assist.md §4), in 系统's
//! chart language (system.rs build_charts) and Home's 今天 strip (home_today.rs):
//! - four half cards, 今天 · 昨天 / 前天 · 本月: grey key, a note on the right (as of / against
//!   the 7-day daily average), the size in n20;
//! - 近 14 天: one blue column a day (none where the ledger has no line, a grey stub instead),
//!   the 7-day average as a grey line, the picked day underlined as Home underlines the hour
//!   now; under a hairline the picked day: date, size, against the average, per card. A tap on a
//!   column picks it (default: yesterday);
//! - 近 30 天 · 按卡: one row a card, size · share.
//!
//! 昨天, 前天 and the columns come from the core's per-card ledger (data/usage.rs), matched by
//! device date: a missing day says 没有记录, not 0; without a set clock there is no 昨天. 今天
//! and 本月 are the firmware counters Home shows; today's column is the firmware total.
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use super::Ui;
use crate::cf;
use crate::clock;
use crate::kit::{self, CARD_W, MARGIN, PAD, ROW_H};
use crate::lang::tr;
use crate::logic::clock_sane;
use crate::lv::{self, sys, Obj};
use crate::theme::{t, uf};
use ui_model::{usage_card_t, usage_day_t};

/// columns in the chart
const COLS: usize = 14;
/// days the average is over (the full days before today)
const AVG_DAYS: usize = 7;
/// days the per-card totals are over
const SUM_DAYS: i64 = 30;
/// card rows (the ledger keeps any number; the busiest four)
const CARDS: usize = 4;

/// the half cards: 145 wide as 系统's CPU / 内存
const HALF_W: i32 = (CARD_W - 10) / 2;
const HALF_H: i32 = 62;
const CH_Y: i32 = 6 + 2 * (HALF_H + 10);
const PLOT_Y: i32 = 34;
const PLOT_H: i32 = 64;
const COL_GAP: i32 = 4;
const COL_W: i32 = (CARD_W - 2 * PAD - (COLS as i32 - 1) * COL_GAP) / COLS as i32;
const PLOT_X: i32 = (CARD_W - (COLS as i32 * COL_W + (COLS as i32 - 1) * COL_GAP)) / 2;
const AXIS_Y: i32 = PLOT_Y + PLOT_H + 6;
const RD_Y: i32 = AXIS_Y + 22;
const CH_H: i32 = RD_Y + 50;
const SUM_Y: i32 = CH_Y + CH_H + 10;

#[derive(Clone, Copy, Default)]
struct Col {
    hit: Obj,
    bar: Obj,
    stub: Obj,
}

#[derive(Clone, Default)]
pub struct UsagePage {
    half_note: [Obj; 4],
    half_v: [Obj; 4],
    avg_t: Obj,
    avg_line: Obj,
    cols: [Col; COLS],
    mark: Obj,
    axis: [Obj; 3],
    pick_day: Obj,
    pick_v: Obj,
    pick_split: Obj,
    pick_cmp: Obj,
    sum_sec: Obj,
    sum_card: Obj,
    sum_sep: [Obj; CARDS],
    sum_name: [Obj; CARDS],
    sum_v: [Obj; CARDS],
    /// the column picked (None: yesterday)
    pick: Option<usize>,
    sig: Option<u64>,
}

/// The device date `back` days before `now`, as the core writes it (YYYY-MM-DD).
pub fn day_before(now: i64, back: i64) -> String {
    let tm = clock::local(now - back * 86400);
    format!("{:04}-{:02}-{:02}", tm.year, tm.mon, tm.mday)
}

/// 1.2 GB → ("1.2", "GB"): two digits of precision at every size.
pub fn size_parts(b: i64) -> (String, &'static str) {
    let b = b.max(0) as f64;
    let (v, u) = if b >= 1024.0 * 1024.0 * 1024.0 {
        (b / (1024.0 * 1024.0 * 1024.0), "GB")
    } else if b >= 1024.0 * 1024.0 {
        (b / (1024.0 * 1024.0), "MB")
    } else if b >= 1024.0 {
        (b / 1024.0, "KB")
    } else {
        (b, "B")
    };
    let s = if u == "B" || v >= 100.0 { format!("{v:.0}") } else if v >= 10.0 { format!("{v:.1}") } else { format!("{v:.2}") };
    // 2.00 → 2, 1.50 → 1.5 (only after a point: 100 stays 100)
    let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
    (s, u)
}

pub fn size(b: i64) -> String {
    let (v, u) = size_parts(b);
    format!("{v} {u}")
}

/// What the page shows, worked out from the model (pure: tested below).
#[derive(Debug, Default, PartialEq)]
pub struct View {
    /// per column, oldest first: None = no ledger that day; Some = (card index, bytes) segments
    pub cols: Vec<Option<Vec<(usize, i64)>>>,
    /// the cards seen, sorted by key: (card, 30-day bytes)
    pub cards: Vec<(usage_card_t, i64)>,
    /// mean of the full days before today that have a record (None: none)
    pub avg: Option<i64>,
}

pub fn view(days: &[usage_day_t], now: i64, today_fw: i64, trusted: bool) -> View {
    let mut v = View::default();
    if !trusted {
        v.cols = vec![None; COLS];
        return v;
    }
    let find = |back: i64| {
        let want = day_before(now, back);
        days.iter().find(|d| d.day == want)
    };
    let mut keys: Vec<usage_card_t> = Vec::new();
    for back in 0..SUM_DAYS {
        for c in find(back).map_or(&[][..], |d| &d.cards[..]) {
            if !keys.iter().any(|k| k.card == c.card) {
                keys.push(usage_card_t { bytes: 0, ..c.clone() });
            }
        }
    }
    keys.sort_by(|a, b| a.card.cmp(&b.card));
    let idx = |c: &usage_card_t| keys.iter().position(|k| k.card == c.card).unwrap_or(0);
    for k in (0..COLS as i64).rev() {
        let col = match find(k) {
            Some(d) => {
                let mut segs: Vec<(usize, i64)> = d.cards.iter().map(|c| (idx(c), c.bytes)).collect();
                if k == 0 {
                    // today: the firmware's total, split as the ledger splits it
                    let led: i64 = segs.iter().map(|s| s.1).sum();
                    if led > 0 && today_fw > 0 {
                        for s in &mut segs {
                            s.1 = (s.1 as f64 * today_fw as f64 / led as f64) as i64;
                        }
                    } else if today_fw > 0 {
                        segs = vec![(segs.first().map_or(0, |s| s.0), today_fw)];
                    }
                }
                segs.sort_by_key(|s| s.0);
                Some(segs)
            }
            None if k == 0 && today_fw > 0 => Some(vec![(0, today_fw)]),
            None => None,
        };
        v.cols.push(col);
    }
    let mut sums = vec![0i64; keys.len()];
    for back in 0..SUM_DAYS {
        for c in find(back).map_or(&[][..], |d| &d.cards[..]) {
            sums[idx(c)] += c.bytes;
        }
    }
    v.cards = keys.into_iter().zip(sums).collect();
    let full: Vec<i64> = (1..=AVG_DAYS as i64).filter_map(find).map(|d| d.cards.iter().map(|c| c.bytes).sum()).collect();
    if !full.is_empty() {
        v.avg = Some(full.iter().sum::<i64>() / full.len() as i64);
    }
    v
}

pub(super) unsafe extern "C" fn usage_col_cb(e: *mut sys::lv_event_t) {
    let i = unsafe { lv::event_user(e) };
    super::with(|u| {
        u.usage.pick = Some(i);
        u.usage.sig = None;
        u.usage_paint();
    });
}

impl Ui {
    pub(super) fn build_sub_usage(&mut self, p: Obj) {
        let th = t();
        let f = uf();
        let sc = kit::scroll(p, 0, super::SUB_VIEW, SUM_Y + 24 + CARDS as i32 * ROW_H + 16);
        let mut u = UsagePage::default();
        // ---- 今天 · 昨天 / 前天 · 本月
        let keys = [tr("今天"), tr("昨天"), tr("前天"), tr("本月")];
        for (i, k) in keys.into_iter().enumerate() {
            let x = MARGIN + (i as i32 % 2) * (HALF_W + 10);
            let y = 6 + (i as i32 / 2) * (HALF_H + 10);
            let c = kit::card(sc, x, y, HALF_W, HALF_H);
            kit::label(c, f.cj12, th.t3, 12, 9, k);
            u.half_note[i] = kit::label_r(c, f.cj12, th.t2, HALF_W - 12, 9, "");
            u.half_v[i] = kit::label(c, f.n20, th.t1, 12, 28, "");
        }
        // ---- 近 14 天
        let c = kit::card(sc, MARGIN, CH_Y, CARD_W, CH_H);
        kit::label(c, f.cj12, th.t3, 12, 9, tr("近 14 天"));
        u.avg_t = kit::label_r(c, f.cj12, th.t2, CARD_W - 12, 9, "");
        for (i, col) in u.cols.iter_mut().enumerate() {
            let x = PLOT_X + i as i32 * (COL_W + COL_GAP);
            col.stub = kit::bx(c, x, PLOT_Y + PLOT_H - 3, COL_W, 3, th.track, 1);
            col.bar = kit::bx(c, x, PLOT_Y + PLOT_H, COL_W, 0, th.blue, 3);
            kit::show(col.bar, false);
            // the finger's target: the whole column and the gap around it, not widened (the next
            // column would cover half of this one) and no pressed wash (the readout answers)
            col.hit = kit::bx(c, x - COL_GAP / 2, PLOT_Y - 8, COL_W + COL_GAP, PLOT_H + 30, th.card, 0);
            col.hit.set_style_bg_opa(lv::OPA_TRANSP, 0);
            kit::tappable(col.hit, Some(usage_col_cb), i);
            col.hit.set_ext_click_area(0);
            col.hit.set_style_bg_opa(lv::OPA_TRANSP, lv::LV_STATE_PRESSED);
        }
        u.avg_line = kit::bx(c, PLOT_X, PLOT_Y, CARD_W - 2 * PLOT_X, 1, th.t3, 0);
        u.mark = kit::bx(c, PLOT_X, PLOT_Y + PLOT_H + 2, COL_W, 2, th.t1, 1);
        for a in u.axis.iter_mut() {
            *a = kit::label(c, f.n11, th.t3, 0, AXIS_Y, "");
        }
        kit::sep(c, RD_Y - 8);
        u.pick_day = kit::label(c, f.cj14, th.t1, PAD, RD_Y, "");
        u.pick_v = kit::label_r(c, f.n15, th.t1, CARD_W - PAD, RD_Y, "");
        u.pick_split = kit::label_w(c, f.cj12, th.t2, PAD, RD_Y + 22, CARD_W - 2 * PAD - 90, false, "");
        u.pick_split.set_long_mode(lv::LV_LABEL_LONG_MODE_DOTS);
        u.pick_split.set_height(f.cj12.line_height());
        u.pick_cmp = kit::label_r(c, f.cj12, th.t2, CARD_W - PAD, RD_Y + 22, "");
        // ---- 近 30 天 · 按卡
        u.sum_sec = kit::section(sc, SUM_Y + 4, tr("近 30 天 · 按卡"));
        u.sum_card = kit::card(sc, MARGIN, SUM_Y + 24, CARD_W, ROW_H);
        for i in 0..CARDS {
            let y = i as i32 * ROW_H;
            u.sum_sep[i] = kit::sep(u.sum_card, y);
            u.sum_name[i] = kit::label_w(u.sum_card, f.cj14, th.t1, PAD, y + 11, 160, false, "");
            u.sum_name[i].set_long_mode(lv::LV_LABEL_LONG_MODE_DOTS);
            u.sum_name[i].set_height(f.cj14.line_height());
            u.sum_v[i] = kit::label_r(u.sum_card, f.n15, th.t1, CARD_W - PAD, y + 10, "");
        }
        self.usage = u;
    }

    /// Every refresh while the page is up: repaints only when what it shows changed.
    pub(super) fn usage_paint(&mut self) {
        let m = self.model.clone();
        let d = &m.data;
        let now = self.now();
        let trusted = m.today.as_ref().map_or(clock_sane(now), |v| v.clock_trusted != 0);
        let today = d.day_rx_bytes + d.day_tx_bytes;
        let month = d.month_rx_bytes + d.month_tx_bytes;
        let tn = clock::local(now);
        let sig = {
            let mut h: u64 = 1469598103934665603;
            let s = format!("{today}/{month}/{trusted}/{:?}/{}/{}/{:?}/{}", m.usage_days, tn.yday, tn.min, self.usage.pick, t().dark);
            for b in s.bytes() {
                h = (h ^ b as u64).wrapping_mul(1099511628211);
            }
            Some(h)
        };
        if sig == self.usage.sig {
            return;
        }
        self.usage.sig = sig;
        let th = t();
        let f = uf();
        let u = self.usage.clone();
        let v = view(&m.usage_days, now, today, trusted);
        let total = |i: usize| v.cols[i].as_ref().map(|s| s.iter().map(|x| x.1).sum::<i64>());
        let against = |b: i64| -> Option<String> {
            let a = v.avg.filter(|a| *a > 0)?;
            let pct = ((b - a) as f64 * 100.0 / a as f64).round() as i64;
            Some(String::from_utf8_lossy(&cf!(tr("比日均 %+ld%%"), pct)).into_owned())
        };
        let missing = if trusted { tr("没有记录") } else { tr("时间没校准") };

        // ---- the half cards
        let asof = if trusted { String::from_utf8_lossy(&cf!(tr("截至 %02d:%02d"), tn.hour, tn.min)).into_owned() } else { String::new() };
        let halves: [(Option<i64>, String); 4] = [
            (Some(today), asof),
            (total(COLS - 2), total(COLS - 2).and_then(against).unwrap_or_default()),
            (total(COLS - 3), total(COLS - 3).and_then(against).unwrap_or_default()),
            (Some(month), String::new()),
        ];
        for (i, (b, note)) in halves.into_iter().enumerate() {
            self.set(u.half_note[i], note.as_str());
            match b {
                Some(b) => {
                    self.set(u.half_v[i], size(b).as_str());
                    u.half_v[i].set_style_text_font(f.n20, 0);
                    kit::text_color(u.half_v[i], th.t1);
                }
                None => {
                    self.set(u.half_v[i], missing);
                    u.half_v[i].set_style_text_font(f.cj15, 0);
                    kit::text_color(u.half_v[i], th.t3);
                }
            }
        }

        // ---- the columns: the scale is the biggest day (or the average, if above)
        match v.avg {
            Some(a) => self.set(u.avg_t, cf!(tr("日均 %s"), size(a).as_str())),
            None => self.set(u.avg_t, ""),
        }
        let max = (0..COLS).filter_map(total).chain(v.avg).max().unwrap_or(0).max(1);
        let px = |b: i64| ((b as f64 / max as f64) * PLOT_H as f64).round() as i32;
        let pick = u.pick.unwrap_or(COLS - 2).min(COLS - 1);
        for (i, col) in u.cols.iter().enumerate() {
            let h = total(i).map_or(0, px);
            kit::show(col.stub, h < 3);
            kit::show(col.bar, h >= 3);
            if h >= 3 {
                col.bar.set_y(PLOT_Y + PLOT_H - h);
                col.bar.set_height(h);
            }
        }
        kit::show(u.avg_line, v.avg.is_some());
        if let Some(a) = v.avg {
            u.avg_line.set_y(PLOT_Y + PLOT_H - px(a));
        }
        u.mark.set_x(PLOT_X + pick as i32 * (COL_W + COL_GAP));
        // the axis: the first column's date, the middle one's, 今天
        let date = |back: i64| {
            let tm = clock::local(now - back * 86400);
            format!("{}-{}", tm.mon, tm.mday)
        };
        let marks = [(0, date(COLS as i64 - 1)), (COLS / 2, date(COLS as i64 / 2 - 1)), (COLS - 1, tr("今天").to_string())];
        for (k, (i, text)) in marks.into_iter().enumerate() {
            let a = u.axis[k];
            self.set(a, text.as_str());
            a.update_layout();
            let cx = PLOT_X + i as i32 * (COL_W + COL_GAP) + COL_W / 2;
            a.set_x((cx - a.width() / 2).clamp(PLOT_X, CARD_W - PLOT_X - a.width()));
        }
        // the picked day
        let back = (COLS - 1 - pick) as i64;
        let word = match back {
            0 => tr("今天"),
            1 => tr("昨天"),
            2 => tr("前天"),
            _ => "",
        };
        self.set(u.pick_day, if word.is_empty() { date(back) } else { format!("{word} · {}", date(back)) }.as_str());
        match total(pick) {
            Some(b) => {
                self.set(u.pick_v, size(b).as_str());
                u.pick_v.set_style_text_font(f.n15, 0);
                kit::text_color(u.pick_v, th.t1);
                let mut segs = v.cols[pick].clone().unwrap_or_default();
                segs.sort_by_key(|x| std::cmp::Reverse(x.1));
                let split = if v.cards.len() > 1 {
                    segs.iter()
                        .filter(|x| x.1 > 0)
                        .map(|&(ci, b)| format!("{} {}", v.cards.get(ci).map_or(String::new(), |c| short_op(&c.0)), size(b)))
                        .collect::<Vec<_>>()
                        .join(" · ")
                } else {
                    String::new()
                };
                self.set(u.pick_split, split.as_str());
                self.set(u.pick_cmp, if back > 0 { against(b).unwrap_or_default() } else { String::new() }.as_str());
            }
            None => {
                self.set(u.pick_v, missing);
                u.pick_v.set_style_text_font(f.cj14, 0);
                kit::text_color(u.pick_v, th.t3);
                self.set(u.pick_split, "");
                self.set(u.pick_cmp, "");
            }
        }

        // ---- per card, busiest first
        let mut cards = v.cards.clone();
        cards.sort_by_key(|c| std::cmp::Reverse(c.1));
        let all: i64 = cards.iter().map(|c| c.1).sum();
        let n = cards.len().min(CARDS);
        kit::show(u.sum_sec, n > 0);
        kit::show(u.sum_card, n > 0);
        u.sum_card.set_height(n.max(1) as i32 * ROW_H);
        for i in 0..CARDS {
            let row = cards.get(i).filter(|_| i < n);
            kit::show(u.sum_name[i], row.is_some());
            kit::show(u.sum_v[i], row.is_some());
            kit::show(u.sum_sep[i], row.is_some() && i > 0);
            if let Some((c, b)) = row {
                let name = match (c.op.is_empty(), c.tail.is_empty()) {
                    (true, true) => tr("未知的卡").to_string(),
                    (false, true) => c.op.clone(),
                    (true, false) => c.tail.clone(),
                    (false, false) => format!("{} · {}", c.op, c.tail),
                };
                self.set(u.sum_name[i], name.as_str());
                let pct = if all > 0 { *b as f64 * 100.0 / all as f64 } else { 0.0 };
                let share = if pct > 0.0 && pct < 1.0 { "<1%".to_string() } else { format!("{}%", pct.round() as i64) };
                self.set(u.sum_v[i], format!("{} · {share}", size(*b)).as_str());
            }
        }
    }
}

/// The operator alone, short (the readout line is narrow): 「Unicom」 from 「China Unicom」.
fn short_op(c: &usage_card_t) -> String {
    let o = c.op.strip_prefix("China ").unwrap_or(&c.op);
    if o.is_empty() { c.tail.clone() } else { o.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(k: &str, b: i64) -> usage_card_t {
        usage_card_t { card: k.into(), tail: "1".into(), op: "X".into(), bytes: b }
    }

    #[test]
    fn days_before_cross_months_and_years() {
        // 2026-03-01 00:30 (device clock)
        let t = 1772325000;
        assert_eq!(day_before(t, 0), "2026-03-01");
        assert_eq!(day_before(t, 1), "2026-02-28");
        assert_eq!(day_before(1767226200, 1), "2025-12-31");
    }

    #[test]
    fn sizes_keep_two_digits() {
        assert_eq!(size(232 * 1024 * 1024), "232 MB");
        assert_eq!(size(1288490188), "1.2 GB");
        assert_eq!(size(2 * 1024 * 1024 * 1024), "2 GB");
        assert_eq!(size(15 * 1024 * 1024 * 1024 + 512 * 1024 * 1024), "15.5 GB");
        assert_eq!(size(0), "0 B");
    }

    #[test]
    fn columns_by_date_today_from_firmware() {
        let t = 1772325000; // 2026-03-01
        let days = vec![
            usage_day_t { day: "2026-03-01".into(), cards: vec![card("b", 30), card("a", 10)] },
            usage_day_t { day: "2026-02-28".into(), cards: vec![card("a", 700)] },
            // 02-27 missing
            usage_day_t { day: "2026-02-26".into(), cards: vec![card("b", 100)] },
        ];
        let v = view(&days, t, 400, true);
        assert_eq!(v.cols.len(), COLS);
        // today: 400 split 1:3, in card order (a, b)
        assert_eq!(v.cols[COLS - 1], Some(vec![(0, 100), (1, 300)]));
        assert_eq!(v.cols[COLS - 2], Some(vec![(0, 700)]));
        assert_eq!(v.cols[COLS - 3], None);
        assert_eq!(v.avg, Some(400));
        assert_eq!(v.cards.iter().map(|c| (c.0.card.as_str(), c.1)).collect::<Vec<_>>(), vec![("a", 710), ("b", 130)]);
        // no clock: nothing matched
        let v = view(&days, t, 400, false);
        assert!(v.cols.iter().all(Option::is_none) && v.avg.is_none());
    }
}
