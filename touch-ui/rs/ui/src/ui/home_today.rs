//! 首页 below the fold, after 落地 and Tailscale: 「今天」 (DESIGN.md §4 Home hierarchy), from
//! core 1.50's `GET /v2/timeline/today`. One card: the title, the day's signal as 24 hour cells
//! (coloured by the hour's average SINR in the core's tiers, ≥ 13 dB / ≥ 0 dB / below; empty
//! where there was no reading; the RAT written where it changes; the hour now underlined), then
//! the events, newest first (HH:MM · tone dot · what happened), at most eight and 「还有 N 条」.
//! No clock: one grey line. No timeline from the core: no card.
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use super::Ui;
use crate::cf;
use crate::clock;
use crate::data::today::TODAY_EVENTS;
use crate::kit::{self, CARD_W, PAD};
use crate::lang::tr;
use crate::logic::{NET_BAD, NET_OK, NET_WARN};
use crate::lv::Obj;
use crate::theme::{t, uf};
use ui_model::today_t;

const HOURS: usize = 24;
const CELL_GAP: i32 = 2;
const CELL_W: i32 = (CARD_W - 2 * PAD - (HOURS as i32 - 1) * CELL_GAP) / HOURS as i32;
const CELL_X0: i32 = PAD + (CARD_W - 2 * PAD - (HOURS as i32 * CELL_W + (HOURS as i32 - 1) * CELL_GAP)) / 2;
const RAT_Y: i32 = 36;
const CELL_Y: i32 = 52;
const CELL_H: i32 = 14;
const AXIS_Y: i32 = CELL_Y + CELL_H + 5;
const EV_Y: i32 = AXIS_Y + 22;
const EV_H: i32 = 24;
/// RAT words over the strip: one per change, at most this many
const RATS: usize = 8;

#[derive(Clone)]
pub struct TodayCard {
    pub card: Obj,
    title: Obj,
    sub: Obj,
    note: Obj,
    cells: [Obj; HOURS],
    now_bar: Obj,
    rats: [Obj; RATS],
    axis: [Obj; 5],
    ev_time: [Obj; TODAY_EVENTS],
    ev_dot: [Obj; TODAY_EVENTS],
    ev_text: [Obj; TODAY_EVENTS],
    more: Obj,
    /// what was painted last (bg colours and positions are not compared by LVGL)
    sig: Option<u64>,
}

impl Default for TodayCard {
    fn default() -> Self {
        TodayCard {
            card: Obj::NULL,
            title: Obj::NULL,
            sub: Obj::NULL,
            note: Obj::NULL,
            cells: [Obj::NULL; HOURS],
            now_bar: Obj::NULL,
            rats: [Obj::NULL; RATS],
            axis: [Obj::NULL; 5],
            ev_time: [Obj::NULL; TODAY_EVENTS],
            ev_dot: [Obj::NULL; TODAY_EVENTS],
            ev_text: [Obj::NULL; TODAY_EVENTS],
            more: Obj::NULL,
            sig: None,
        }
    }
}

/// The hour cell's colour: the core's SINR tiers (13 dB, 0 dB); None = no reading.
pub fn sinr_tone(sinr: Option<f64>) -> Option<i32> {
    let s = sinr?;
    Some(if s >= 13.0 {
        NET_OK
    } else if s >= 0.0 {
        NET_WARN
    } else {
        NET_BAD
    })
}

/// The hours where the RAT word goes: where it differs from the last hour that had one.
pub fn rat_marks(v: &today_t) -> Vec<(usize, String)> {
    let mut hours: Vec<_> = v.hours.iter().filter(|h| (0..HOURS as i32).contains(&h.hour)).collect();
    hours.sort_by_key(|h| h.hour);
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut last = "";
    for h in hours {
        if h.rat.is_empty() || h.rat == last {
            continue;
        }
        last = &h.rat;
        // a word needs two cells: a change right after another keeps the earlier word
        if out.last().is_some_and(|(i, _)| h.hour as usize - i < 2) {
            continue;
        }
        out.push((h.hour as usize, h.rat.clone()));
    }
    out.truncate(RATS);
    out
}

impl Ui {
    pub(super) fn build_home_today(&mut self, sc: Obj) {
        let th = t();
        let f = uf();
        let c = kit::card(sc, kit::MARGIN, 0, CARD_W, EV_Y);
        let mut d = TodayCard { card: c, ..Default::default() };
        d.title = kit::label(c, f.cj14, th.t1, PAD, 11, tr("今天"));
        d.sub = kit::label_r(c, f.cj12, th.t3, CARD_W - PAD, 13, "");
        d.note = kit::label_w(c, f.cj12, th.t3, PAD, 38, CARD_W - 2 * PAD, false, "");
        for i in 0..HOURS {
            d.cells[i] = kit::bx(c, CELL_X0 + i as i32 * (CELL_W + CELL_GAP), CELL_Y, CELL_W, CELL_H, th.track, 3);
        }
        d.now_bar = kit::bx(c, 0, CELL_Y + CELL_H + 2, CELL_W, 2, th.t1, 1);
        for r in d.rats.iter_mut() {
            *r = kit::label(c, f.n11, th.t2, 0, RAT_Y, "");
        }
        for (k, a) in d.axis.iter_mut().enumerate() {
            let h = k as i32 * 6;
            let x = if k == 4 { CELL_X0 + HOURS as i32 * (CELL_W + CELL_GAP) - CELL_GAP } else { CELL_X0 + h * (CELL_W + CELL_GAP) };
            *a = kit::label(c, f.n11, th.t3, x, AXIS_Y, cf!("%d", h));
            if k == 4 {
                a.update_layout();
                a.set_x(x - a.width());
            }
        }
        for i in 0..TODAY_EVENTS {
            let y = EV_Y + i as i32 * EV_H;
            d.ev_time[i] = kit::label(c, f.n12, th.t2, PAD, y + 3, "");
            d.ev_dot[i] = kit::dot(c, PAD + 42, y + 8, 7, th.t3);
            d.ev_text[i] = kit::label_w(c, f.cj12, th.t1, PAD + 56, y + 2, CARD_W - PAD - 56 - PAD, false, "");
        }
        d.more = kit::label(c, f.cj12, th.t3, PAD + 56, 0, "");
        kit::show(c, false);
        self.home.td = d;
    }

    /// Every refresh: repaints only when what it shows changed (still.sh: no redraw for nothing).
    pub(super) fn home_today_paint(&mut self) {
        let m = self.model.clone();
        let now = self.now();
        let tn = clock::local(now);
        // a timeline kept from yesterday (reads failed since midnight) is not today's
        let today = m.today.as_ref().filter(|v| v.clock_trusted == 0 || v.now <= 0 || clock::local(v.now).yday == tn.yday);
        let sig = today.map(|v| {
            let mut h: u64 = 1469598103934665603;
            let s = format!("{:?}/{}/{}/{}", v, tn.hour, tn.yday, t().dark);
            for b in s.bytes() {
                h = (h ^ b as u64).wrapping_mul(1099511628211);
            }
            h
        });
        if sig == self.home.td.sig && sig.is_some() {
            return;
        }
        let was = !self.home.td.card.hidden();
        self.home.td.sig = sig;
        let Some(v) = today else {
            if was {
                kit::show(self.home.td.card, false);
                self.home_reflow();
            }
            return;
        };
        let th = t();
        let d = self.home.td.clone();
        let trusted = v.clock_trusted != 0;
        // ---- the clock not set: one grey line
        kit::show(d.note, !trusted);
        for o in d.cells.iter().chain(&d.rats).chain(&d.axis).chain([&d.now_bar, &d.more]) {
            kit::show(*o, false);
        }
        for i in 0..TODAY_EVENTS {
            for o in [d.ev_time[i], d.ev_dot[i], d.ev_text[i]] {
                kit::show(o, false);
            }
        }
        if !trusted {
            self.set(d.sub, "");
            self.set(d.note, tr("设备时间还没校准，时间线暂停"));
            d.card.set_height(38 + 18 + 12);
        } else {
            // ---- the day's signal, hour by hour
            let hour_now = (if v.now > 0 { clock::local(v.now).hour } else { tn.hour }).clamp(0, 23) as usize;
            let mut seen = 0;
            for i in 0..HOURS {
                let cell = d.cells[i];
                let h = v.hours.iter().find(|h| h.hour as usize == i);
                kit::show(cell, i <= hour_now);
                let col = match sinr_tone(h.and_then(|h| h.sinr)) {
                    Some(NET_OK) => th.green,
                    Some(NET_WARN) => th.orange,
                    Some(_) => th.red,
                    None => th.track,
                };
                seen += h.is_some_and(|h| h.sinr.is_some()) as i32;
                kit::bg(cell, col);
            }
            kit::show(d.now_bar, true);
            d.now_bar.set_x(CELL_X0 + hour_now.min(HOURS - 1) as i32 * (CELL_W + CELL_GAP));
            for (k, (i, rat)) in rat_marks(v).into_iter().enumerate() {
                let r = d.rats[k];
                self.set(r, rat.as_str());
                r.set_x(CELL_X0 + i as i32 * (CELL_W + CELL_GAP));
                kit::show(r, true);
            }
            for a in d.axis {
                kit::show(a, true);
            }
            // the title's right: what the strip is (or that there is nothing in it yet)
            self.set(d.sub, if seen > 0 { tr("每小时平均信号") } else { tr("还没有信号记录") });
            // ---- the events, newest first
            let n = v.events.len().min(TODAY_EVENTS);
            for (i, e) in v.events.iter().take(n).enumerate() {
                let tm = clock::local(e.t);
                self.set(d.ev_time[i], cf!("%02d:%02d", tm.hour, tm.min));
                let col = match e.tone {
                    NET_OK => th.green,
                    NET_WARN => th.orange,
                    NET_BAD => th.red,
                    _ => th.t3,
                };
                kit::bg(d.ev_dot[i], col);
                self.set(d.ev_text[i], e.text.as_str());
                for o in [d.ev_time[i], d.ev_dot[i], d.ev_text[i]] {
                    kit::show(o, true);
                }
            }
            let mut y = EV_Y + n as i32 * EV_H;
            if n == 0 {
                kit::show(d.more, true);
                self.set(d.more, tr("今天还没有要记的事"));
                d.more.set_pos(PAD, y + 2);
                y += EV_H;
            } else if v.more > 0 {
                kit::show(d.more, true);
                self.set(d.more, cf!(tr("还有 %d 条"), v.more));
                d.more.set_pos(PAD + 56, y + 2);
                y += EV_H;
            }
            d.card.set_height(y + 8);
        }
        if !was {
            kit::show(d.card, true);
        }
        self.home_reflow();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui_model::today_hour_t;

    fn hour(h: i32, rat: &str) -> today_hour_t {
        today_hour_t { hour: h, sinr: Some(10.0), rat: rat.into(), link_down: 0 }
    }

    #[test]
    fn tiers_and_rat_words() {
        assert_eq!(sinr_tone(Some(13.0)), Some(NET_OK));
        assert_eq!(sinr_tone(Some(0.0)), Some(NET_WARN));
        assert_eq!(sinr_tone(Some(-0.5)), Some(NET_BAD));
        assert_eq!(sinr_tone(None), None);
        let v = today_t {
            clock_trusted: 1,
            hours: vec![hour(0, "SA"), hour(1, "SA"), hour(2, ""), hour(3, "LTE"), hour(4, "SA"), hour(9, "SA"), hour(10, "NSA")],
            ..Default::default()
        };
        // 0 SA; 3 LTE; 4 SA too close to 3 (dropped, but it is the RAT now); 10 NSA
        assert_eq!(rat_marks(&v), vec![(0, "SA".into()), (3, "LTE".into()), (10, "NSA".into())]);
    }
}
