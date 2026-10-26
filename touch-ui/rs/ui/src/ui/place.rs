//! 摆放模式 (manager docs/DESIGN.md §4 "Placement page", docs/designs/travel-assist.md §8): one
//! score says whether here is good, and the owner walks around with it. Score 0–100 = the share
//! of this link's fastest speed: 3GPP's SINR-to-throughput curve × the primary carrier's width.
//! Strength and load reach the score through SINR and are named as the reason. The
//! status card carries the verdict, the score and its last 3 minutes; the equation card shows
//! where the score comes from; up to three recorded places are compared by score. The device has
//! no compass or motion sensor: places are 「第 N 处」 with the time they were recorded, never a
//! direction or a name. The screen stays on while the page is up.
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use super::diagnose::{ago, dg_hm};
use super::{Sub, Ui, SUB_VIEW};
use crate::cf;
use crate::kit::{self, BtnKind, CARD_W, MARGIN, PAD, ROW_H};
use crate::lang::tr;
use crate::logic::{self, Rat};
use crate::lv::{self, sys, Obj};
use crate::theme::{t, uf};

/// the live SINR is smoothed per new snapshot (time constant about 3 s): the modem holds a reading
/// for seconds and then steps, and the score should glide, not jump
const SMOOTH: f64 = 0.3;
/// a reason's thresholds: RSRP at or under this is weak, RSRQ at or under this is busy
const WEAK_RSRP: i32 = -105;
const BUSY_RSRQ: i32 = -15;
/// the curve: one point per core snapshot (~1 s), the last 3 minutes
const CURVE_N: u32 = 180;
/// recording: this long, at least this many new snapshots; give up past the limit
const REC_MS: u32 = 8000;
const REC_MIN: usize = 5;
const REC_GIVE_UP_MS: u32 = 15000;
/// places kept; the 4th drops the oldest one that isn't the best
const RECS: usize = 3;
/// two scores this close are the same
const SAME: i32 = 8;
/// how long 「已清零」/「换了小区」/「没记上」 stay
const NOTE_MS: u32 = 10000;
/// ping-pong between cells: this many returns to a cell left within the window (a walk through
/// new cells is not ping-pong; 3GPP's mobility robustness calls a handover back to the cell just
/// left a ping-pong)
const FLAP_N: usize = 3;
/// the highest NR PCI (TS 38.211 §7.4.2.1: 1008 identities); the modem says 65535 for none
const PCI_MAX: i32 = 1007;
/// switching back and forth only headlines when the score is under 良好: above it the link
/// holds up and the switching costs nothing worth moving for
const FLAP_HURTS: i32 = 60;
const FLAP_MS: u32 = 120_000;

const HERO_Y: i32 = 4;
const HERO_H: i32 = 198;
const CHART_Y: i32 = 90;
const CHART_H: i32 = HERO_H - CHART_Y;
const PIN: i32 = 20;
const EQ_H: i32 = 66;
const CELL_H: i32 = 62;
const BTN_H: i32 = 44;
const RESET_W: i32 = 110;

// ---------------------------------------------------------------- the score
/// SINR's score, 0–100: 3GPP TR 36.942 Annex A.2's link curve, the attenuated and truncated Shannon
/// bound used to turn SINR into throughput in 3GPP's own system studies: α·log2(1 + SINR) with
/// α = 0.6, nothing under -10 dB, capped at 4.4 b/s/Hz (Table A.1, downlink), reached about 22 dB.
/// Continuous in SINR: 13 dB is about 60, 17 dB about 77, 5 dB about 28, 0 dB 14. Strength and
/// load are not multiplied in again: the modem's SINR already has the noise of a weak signal and
/// the interference in it, and nothing published turns RSRP or RSRQ into a throughput factor.
pub(crate) fn sinr_score(sinr10: f64) -> f64 {
    const ALPHA: f64 = 0.6;
    const MIN_DB: f64 = -10.0;
    const MAX_BPS: f64 = 4.4;
    let db = sinr10 / 10.0;
    if db < MIN_DB {
        return 0.0;
    }
    let lin = 10f64.powf(db / 10.0);
    (ALPHA * (1.0 + lin).log2()).min(MAX_BPS) / MAX_BPS * 100.0
}

pub(crate) fn score(sinr10: i32) -> i32 {
    sinr_score(sinr10 as f64).round() as i32
}

/// bandwidth discount: this place's active MHz against the widest seen this visit (one n78
/// carrier is 100 MHz, n3 20). Throughput is linear in the carrier's resource blocks (3GPP TS
/// 38.306 §4.1.2), so the discount is the plain ratio. Relative, so a visit that only ever sees
/// 20 MHz isn't marked down for it; unknown = none.
pub(crate) fn width_k(mhz: i32, widest: i32) -> f64 {
    if mhz <= 0 || widest <= 0 { 1.0 } else { (mhz as f64 / widest as f64).min(1.0) }
}

fn with_width(base: i32, mhz: i32, widest: i32) -> i32 {
    (base as f64 * width_k(mhz, widest)).round() as i32
}

/// The leading digits of "100" / "20MHz"; 0 = none.
fn lead_int(t: &str) -> i32 {
    let t = t.trim();
    let n = t.bytes().take_while(u8::is_ascii_digit).count();
    t[..n].parse().unwrap_or(0)
}

/// The primary carrier's own width and band: SA the NR cell, 4G the LTE cell, NSA the LTE anchor
/// plus the NR leg when there is one. Secondary carriers (CA) are left out: the base station adds
/// them for a download and drops them when idle, so they follow the traffic, not the spot.
fn primary_width(d: &ui_model::core_snapshot_t, rat: Rat) -> (i32, String) {
    let nr_mhz = lead_int(&d.nr_width);
    let lte_mhz = lead_int(&d.bandwidth);
    let lte_band = if d.band_text.is_empty() { String::new() } else { logic::band_short(&d.band_text, false) };
    match rat {
        Rat::G5Sa => (nr_mhz, d.nr_band_text.clone()),
        Rat::G4 => (lte_mhz, lte_band),
        Rat::G5Nsa => {
            // an idle NSA leg still reports an RSRP but no band or width (core screen.rs)
            let leg = d.nr_rsrp != 0 && (!d.nr_band_text.is_empty() || nr_mhz > 0);
            if leg && nr_mhz > 0 {
                (lte_mhz + nr_mhz, format!("{lte_band}+{}", d.nr_band_text))
            } else {
                (lte_mhz, lte_band)
            }
        }
        _ => (0, String::new()),
    }
}

/// The score in two rounded steps: SINR's score, then after the width discount. The equation
/// card shows the difference; the last is the score.
pub(crate) fn steps(sinr10: f64, mhz: i32, widest: i32) -> [i32; 2] {
    let c = sinr_score(sinr10);
    [c, c * width_k(mhz, widest)].map(|x| x.round() as i32)
}

/// What holds the score down most: 3 width when it takes more than SINR's shortfall, else why
/// SINR is short: 1 a weak signal, 2 a busy cell, 0 interference; None = nothing much (80 or more).
pub(crate) fn limiter(sinr10: f64, rsrp: i32, rsrq: i32, mhz: i32, widest: i32) -> Option<usize> {
    let p = steps(sinr10, mhz, widest);
    if p[1] >= 80 {
        return None;
    }
    if p[0] - p[1] > 100 - p[0] {
        return Some(3);
    }
    Some(if rsrp != 0 && rsrp <= WEAK_RSRP {
        1
    } else if rsrq != 0 && rsrq <= BUSY_RSRQ {
        2
    } else {
        0
    })
}

/// 0 很好 · 1 良好 · 2 一般 · 3 差
fn tier(s: i32) -> usize {
    match s {
        80.. => 0,
        60.. => 1,
        40.. => 2,
        _ => 3,
    }
}

/// "17.7" / "-2.5" → tenths; None = no number
fn tenths(s: &str) -> Option<i32> {
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
fn fmt10(t: i32) -> String {
    let a = t.abs();
    format!("{}{}.{}", if t < 0 { "-" } else { "" }, a / 10, a % 10)
}

// ---------------------------------------------------------------- the verdict
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Verdict {
    /// no number to judge
    NoSignal,
    /// nothing to compare yet: by the live score's tier
    Here(usize),
    Recording,
    /// place N is clearly best and now is more than SAME below it
    BackTo(u32, i32),
    /// now is within SAME of the best place
    HereBest,
    /// all places within SAME
    AllSame(usize, i32),
    /// even the best place scores under 20
    AllBad(usize),
    /// all the same and middling, with a decent signal and a busy cell
    NotPlacement(usize),
    /// switching back and forth between cells: n returns in the window
    Flapping(usize),
}

/// A recorded place as the verdict sees it.
#[derive(Clone, Copy)]
pub(crate) struct Mark {
    pub no: u32,
    pub score: i32,
    pub rsrp: i32,
    pub rsrq: i32,
    /// the cell changed while it was recorded
    pub flap: bool,
}

/// `flaps`: returns to a cell left within FLAP_MS, at this spot (a recording starts the count
/// again); they headline only while the score is under FLAP_HURTS. A place recorded across a switch is only
/// crowned when every place was.
pub(crate) fn judge(now: Option<i32>, recording: bool, flaps: usize, marks: &[Mark]) -> Verdict {
    if recording {
        return Verdict::Recording;
    }
    if flaps >= FLAP_N && now.is_none_or(|s| s < FLAP_HURTS) {
        return Verdict::Flapping(flaps);
    }
    if marks.len() < 2 {
        return match now {
            Some(s) => Verdict::Here(tier(s)),
            None => Verdict::NoSignal,
        };
    }
    let stable = marks.iter().any(|m| !m.flap);
    let best = marks.iter().copied().filter(|m| !stable || !m.flap).max_by_key(|m| (m.score, m.no)).unwrap();
    let low = marks.iter().map(|m| m.score).min().unwrap_or(0);
    let high = marks.iter().map(|m| m.score).max().unwrap_or(0);
    if high < 30 {
        return Verdict::AllBad(marks.len());
    }
    if high - low <= SAME {
        let busy = best.rsrq != 0 && best.rsrq <= BUSY_RSRQ && (best.rsrp == 0 || best.rsrp > WEAK_RSRP);
        if busy && high < 60 {
            return Verdict::NotPlacement(marks.len());
        }
        return Verdict::AllSame(marks.len(), high - low);
    }
    match now {
        Some(s) if s >= best.score - SAME => Verdict::HereBest,
        Some(s) => Verdict::BackTo(best.no, best.score - s),
        None => Verdict::BackTo(best.no, 0),
    }
}

/// 0 ok · 1 warn · 2 bad · 3 neutral (blue) · 4 grey
fn tone(v: Verdict, best: i32) -> usize {
    let by = |s: i32| match tier(s) {
        0 | 1 => 0,
        2 => 1,
        _ => 2,
    };
    match v {
        Verdict::NoSignal => 4,
        Verdict::Recording => 3,
        Verdict::Here(t) => [0, 0, 1, 2][t],
        Verdict::AllBad(_) => 2,
        Verdict::NotPlacement(_) | Verdict::Flapping(_) => 1,
        Verdict::BackTo(..) | Verdict::HereBest | Verdict::AllSame(..) => by(best),
    }
}

// ---------------------------------------------------------------- state
#[derive(Clone, Default)]
struct Rec {
    no: u32,
    /// the curve sample it was taken at
    seq: u32,
    at: i64,
    /// the score without the bandwidth discount, and the width it had
    base: i32,
    mhz: i32,
    /// with the bandwidth discount against the widest seen so far
    score: i32,
    sinr10: i32,
    rsrp: i32,
    rsrq: i32,
    key: String,
    flap: bool,
}

impl Rec {
    fn mark(&self) -> Mark {
        Mark { no: self.no, score: self.score, rsrp: self.rsrp, rsrq: self.rsrq, flap: self.flap }
    }
}

#[derive(Default, Clone, Copy)]
struct Cell {
    bx: Obj,
    k: Obj,
    n: Obj,
    s: Obj,
}

#[derive(Default)]
pub struct Place {
    hero: Obj,
    verdict: Obj,
    score_l: Obj,
    why: Obj,
    /// why it's good or bad: the biggest deduction, with its reading
    reason: Obj,
    /// this second's score, the last 6 (for 「比 5 秒前」)
    trail: std::collections::VecDeque<i32>,
    span: Obj,
    chart: Obj,
    ser: lv::Series,
    best_line: Obj,
    pins: [Obj; RECS],
    pin_l: [Obj; RECS],
    /// the score card: the total, then SINR, width, strength, load — each a number, its name
    /// and what it means
    eq_v: [Obj; 5],
    eq_k: [Obj; 5],
    eq_raw: [Obj; 5],
    cells: [Cell; RECS],
    btn: Obj,
    btn_l: Obj,
    best_l: Obj,
    cmp: Obj,
    cell: Obj,
    /// the live SINR (tenths), smoothed; None = no reading
    sm: Option<f64>,
    /// core snapshot the last sample came from
    ver: u64,
    /// samples pushed onto the curve
    seq: u32,
    /// the best live score this visit (this cell)
    best: i32,
    have_best: bool,
    best_at: i64,
    /// the cell now (RAT · PCI; the band is left out: with carrier aggregation the modem
    /// reports the PCI under whichever carrier's band), and a different one seen once
    /// (counted when it holds)
    key: String,
    pci: i32,
    /// the last two cells switched back and forth between
    flap_pair: (i32, i32),
    cand: String,
    /// the widest active MHz seen this visit
    widest: i32,
    /// when the cell last changed (ticks), newest last
    switches: Vec<u32>,
    /// cells left in the last FLAP_MS, and when
    left: Vec<(String, u32)>,
    /// the cell changed during this recording
    rec_flap: bool,
    /// a note in the why line since (tick), and which
    note_at: u32,
    note: Vec<u8>,
    recs: Vec<Rec>,
    next_no: u32,
    /// recording since (tick; 0 = not recording) and its samples (sinr10, rsrp, rsrq)
    rec_t0: u32,
    rec_buf: Vec<(i32, i32, i32)>,
    /// the button means 去测速 now
    to_speed: bool,
    tone: Option<usize>,
}

unsafe extern "C" fn pl_btn_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| u.place_btn_tap());
}

unsafe extern "C" fn pl_reset_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| u.place_reset_tap());
}

fn median(a: &[i32]) -> i32 {
    let mut a = a.to_vec();
    a.sort_unstable();
    a[(a.len() - 1) / 2] // two readings: the lower one
}

impl Ui {
    /// Opening the page: this visit starts clean (a new walk), the curve too.
    pub(super) fn place_on_open(&mut self) {
        let p = &mut self.pl;
        p.recs.clear();
        p.next_no = 1;
        p.rec_t0 = 0;
        p.rec_buf.clear();
        p.key.clear();
        p.pci = 0;
        p.cand.clear();
        p.widest = 0;
        p.switches.clear();
        p.left.clear();
        p.note_at = 0;
        self.place_clear_live();
    }

    fn place_clear_live(&mut self) {
        let p = &mut self.pl;
        p.sm = None;
        p.have_best = false;
        p.seq = 0;
        p.trail.clear();
        if !p.chart.is_null() {
            p.chart.chart_set_all_values(p.ser, lv::CHART_POINT_NONE);
        }
    }

    fn place_note(&mut self, note: Vec<u8>) {
        self.pl.note = note;
        self.pl.note_at = lv::tick_get().max(1);
    }

    fn place_reset_tap(&mut self) {
        self.place_on_open();
        self.place_note(tr("已清零，重新计").as_bytes().to_vec());
        self.place_paint(true); // answered at once, the numbers follow
    }

    fn place_btn_tap(&mut self) {
        if self.pl.rec_t0 != 0 {
            self.pl.rec_t0 = 0;
            self.pl.rec_buf.clear();
        } else if self.pl.to_speed {
            self.sub_open_child(Sub::Speed, Sub::Place);
            return;
        } else {
            self.pl.rec_t0 = lv::tick_get().max(1);
            self.pl.rec_buf.clear();
            self.pl.rec_flap = false;
            // switching back and forth is judged per spot: moving on counts afresh
            self.pl.switches.clear();
            self.pl.left.clear();
        }
        self.place_paint(true);
    }

    pub(super) fn build_sub_place(&mut self, p: Obj) {
        let th = t();
        let f = uf();
        let eq_y = HERO_Y + HERO_H + 8;
        let cell_y = eq_y + EQ_H + 8;
        let btn_y = cell_y + CELL_H + 8;
        let rows_y = btn_y + BTN_H + 10;
        let sc = kit::scroll(p, 0, SUB_VIEW, rows_y + 3 * ROW_H + 10 + 40);
        let pl = &mut self.pl;

        // status card: verdict + score, one line, the score's last 3 minutes along the bottom
        let h = kit::bx(sc, MARGIN, HERO_Y, CARD_W, HERO_H, th.wash, 20);
        h.set_style_clip_corner(true, 0);
        pl.hero = h;
        pl.verdict = kit::label_w(h, f.cj22b, th.ok_t, PAD, 10, CARD_W - 2 * PAD - 90, false, "");
        pl.score_l = kit::label_r(h, f.n36, th.t1, CARD_W - PAD, 4, "--");
        pl.why = kit::label_w(h, f.cj13, th.t1, PAD, 50, CARD_W - 2 * PAD, false, "");
        pl.reason = kit::label_w(h, f.cj13, th.t2, PAD, 69, CARD_W - 2 * PAD, false, "");
        (pl.chart, pl.ser, _) = kit::chart(h, 0, CHART_Y, CARD_W, CHART_H, CURVE_N, th.fill_green, 0);
        pl.chart.chart_set_div_line_count(0, 0);
        pl.chart.set_style_line_width(3, lv::LV_PART_ITEMS);
        pl.span = kit::label(h, f.cj11, th.t2, PAD, HERO_H - 18, tr("近 3 分钟"));
        pl.best_line = kit::bx(h, 0, 0, CARD_W, 2, th.fill_green, 0);
        kit::show(pl.best_line, false);
        for i in 0..RECS {
            let o = kit::bx(h, 0, 0, PIN, PIN, th.card, PIN / 2 + 1);
            o.set_style_border_width(3, 0);
            o.set_style_border_color(th.fill_green, 0);
            pl.pin_l[i] = kit::label(o, f.n12, th.t1, 0, 0, "");
            pl.pin_l[i].center();
            kit::show(o, false);
            pl.pins[i] = o;
        }

        // the equation: SINR's score less the width's, then the strength and load readings
        let e = kit::card(sc, MARGIN, eq_y, CARD_W, EQ_H);
        // the total first, a rule, then the four parts in their own columns
        let tot_w = 52;
        let pw = (CARD_W - 2 * 10 - tot_w - 13 - 3 * 6) / 4;
        for i in 0..5 {
            let x = if i == 0 { 10 } else { 10 + tot_w + 13 + (i as i32 - 1) * (pw + 6) };
            let w = if i == 0 { tot_w } else { pw + 4 };
            pl.eq_v[i] = kit::label(e, f.n20, th.t1, x, 6, "");
            pl.eq_k[i] = kit::label_w(e, f.cj11, th.t2, x, 32, w, false, "");
            pl.eq_raw[i] = kit::label_w(e, f.cj11, th.t2, x, 47, w, false, "");
        }
        kit::bx(e, 10 + tot_w + 4, 10, 1, EQ_H - 20, th.sep, 0);

        // three recorded places
        let cw = (CARD_W - 2 * 8) / 3;
        for i in 0..RECS {
            let b = kit::bx(sc, MARGIN + i as i32 * (cw + 8), cell_y, cw, CELL_H, th.card, 18);
            let k = kit::label_w(b, f.cj11, th.t3, 12, 8, cw - 16, false, "");
            let n = kit::label(b, f.n20, th.t1, 12, 22, "");
            let s = kit::label_w(b, f.cj11, th.t2, 12, 45, cw - 16, false, "");
            pl.cells[i] = Cell { bx: b, k, n, s };
        }

        (pl.btn, pl.btn_l) = kit::button(sc, MARGIN, btn_y, CARD_W - 8 - RESET_W, BTN_H, tr("记下这里"), BtnKind::Primary, Some(pl_btn_cb), 0);
        kit::button(sc, MARGIN + CARD_W - RESET_W, btn_y, RESET_W, BTN_H, tr("重新开始"), BtnKind::Plain, Some(pl_reset_cb), 0);

        // below the fold: this visit's best, how far off it now, the cell in use
        let rc = kit::card(sc, MARGIN, rows_y, CARD_W, 3 * ROW_H);
        pl.best_l = kit::row(rc, 0, tr("这次最好"), true);
        pl.cmp = kit::row(rc, ROW_H, tr("对比"), false);
        pl.cell = kit::row(rc, 2 * ROW_H, tr("在用"), false);
        kit::label_w(sc, f.cj12, th.t3, MARGIN + 6, rows_y + 3 * ROW_H + 10, CARD_W - 12, true, tr("换了小区会标出来；这页开着不息屏"));
        sc.scroll_to_y(0);
    }

    /// live = core answered this second; the model's data is its snapshot (or the last one).
    pub(super) fn place_paint(&mut self, live: bool) {
        if self.pl.hero.is_null() {
            return;
        }
        let th = t();
        let m = self.model.clone();
        let d = &m.data;
        let rat = logic::rat(&d.radio);
        // NSA: quality from the LTE anchor, always. The NR leg often reports no SINR (issue #4);
        // taking it when it does would swap legs mid-walk, jump the score and look like a cell
        // change. The leg counts in the width. SA has no LTE leg; 3G/2G have no SINR here.
        let nr = rat == Rat::G5Sa;
        let lte_or_nr = nr || rat == Rat::G4 || rat == Rat::G5Nsa;
        let (snr, rsrp, rsrq, pci) =
            if nr { (&d.nr_snr, d.nr_rsrp, d.nr_rsrq, d.nr_pci) } else { (&d.lte_snr, d.lte_rsrp, d.lte_rsrq, d.lte_pci) };
        let band: String = if nr {
            d.nr_band_text.clone()
        } else if !d.band_text.is_empty() {
            logic::band_short(&d.band_text, false)
        } else {
            String::new()
        };
        let sinr10 = if live && d.populated != 0 && lte_or_nr { tenths(snr) } else { None };
        let now_ms = lv::tick_get();
        let (mhz, bands) = primary_width(d, rat);
        if sinr10.is_some() {
            self.pl.widest = self.pl.widest.max(mhz);
        }
        let widest = self.pl.widest;

        // a different cell (band · PCI; RAT left out; a reading without a band or a valid PCI
        // ignored: mid-handover the modem reports 65535 or no band), once it holds for two
        // snapshots: this visit's best was somewhere else; places keep theirs
        let key = format!("{}{pci}", if nr { "N" } else { "L" });
        let fresh_ver = self.src.core_version(&self.model) != self.pl.ver;
        if sinr10.is_some() && (1..=PCI_MAX).contains(&pci) && fresh_ver {
            if key == self.pl.key {
                self.pl.cand.clear();
            } else if self.pl.key.is_empty() {
                self.pl.key = key;
                self.pl.pci = pci;
            } else if key != self.pl.cand {
                self.pl.cand = key;
            } else {
                let back = self.pl.left.iter().any(|(k, t)| *k == key && now_ms.wrapping_sub(*t) < FLAP_MS);
                eprintln!("place: cell {} -> {} {band} ({}){}", self.pl.key, key, d.radio, if back { " back" } else { "" });
                self.pl.have_best = false;
                self.pl.sm = None;
                if back {
                    self.pl.switches.push(now_ms);
                    self.pl.flap_pair = (self.pl.pci, pci);
                }
                let gone = std::mem::take(&mut self.pl.key);
                self.pl.left.retain(|(k, t)| *k != gone && now_ms.wrapping_sub(*t) < FLAP_MS);
                self.pl.left.push((gone, now_ms));
                self.pl.rec_flap |= self.pl.rec_t0 != 0;
                self.place_note(cf!(tr("换到小区 %d，重新计"), pci));
                self.pl.key = key;
                self.pl.pci = pci;
                self.pl.cand.clear();
            }
        }

        // a new sample per new core snapshot: the SINR smoothed, the score on it
        let ver = self.src.core_version(&self.model);
        let fresh = live && ver != self.pl.ver;
        if fresh {
            self.pl.ver = ver;
            let p = &mut self.pl;
            p.sm = sinr10.map(|v| p.sm.map_or(v as f64, |a| a + SMOOTH * (v as f64 - a)));
            if let (Some(v), true) = (sinr10, p.rec_t0 != 0) {
                p.rec_buf.push((v, rsrp, rsrq));
            }
            if !p.chart.is_null() {
                let pt = p.sm.map_or(lv::CHART_POINT_NONE, |a| steps(a, mhz, widest)[1]);
                p.chart.chart_set_next_value(p.ser, pt);
                p.seq = p.seq.wrapping_add(1);
            }
        }
        let sm = if sinr10.is_some() { self.pl.sm } else { None };
        let now = sm.map(|a| steps(a, mhz, widest)[1]);
        if let Some(s) = now {
            let p = &mut self.pl;
            if !p.have_best || s > p.best {
                p.best = s;
                p.best_at = m.now;
                p.have_best = true;
            }
        }

        // recording: 8 s and at least 5 snapshots, the median of each reading
        if self.pl.rec_t0 != 0 {
            let el = now_ms.wrapping_sub(self.pl.rec_t0);
            let p = &mut self.pl;
            if el >= REC_MS && p.rec_buf.len() >= REC_MIN {
                let col = |f: fn(&(i32, i32, i32)) -> i32| median(&p.rec_buf.iter().map(f).collect::<Vec<_>>());
                let (v, r, q) = (col(|a| a.0), col(|a| a.1), col(|a| a.2));
                let base = score(v);
                let rec = Rec { no: p.next_no, seq: p.seq, at: m.now, base, mhz, score: with_width(base, mhz, widest), sinr10: v, rsrp: r, rsrq: q, key: p.key.clone(), flap: p.rec_flap };
                p.next_no += 1;
                if p.recs.len() >= RECS {
                    let best = p.recs.iter().map(|r| r.score).max().unwrap_or(0);
                    let drop = p.recs.iter().position(|r| r.score != best).unwrap_or(0);
                    p.recs.remove(drop);
                }
                p.recs.push(rec);
                p.rec_t0 = 0;
                p.rec_buf.clear();
            } else if el >= REC_GIVE_UP_MS {
                p.rec_t0 = 0;
                p.rec_buf.clear();
                self.place_note(tr("读不到信号，没记上").as_bytes().to_vec());
            }
        }

        for r in &mut self.pl.recs {
            r.score = with_width(r.base, r.mhz, widest);
        }
        self.pl.switches.retain(|&t| now_ms.wrapping_sub(t) < FLAP_MS);
        let flaps = self.pl.switches.len();
        let marks: Vec<Mark> = self.pl.recs.iter().map(Rec::mark).collect();
        let recording = self.pl.rec_t0 != 0;
        let v = judge(now, recording, flaps, &marks);
        let best_rec = marks.iter().filter(|k| !k.flap || marks.iter().all(|j| j.flap)).map(|k| k.score).max();
        let tn = tone(v, best_rec.unwrap_or(0));
        let pl = &self.pl;
        let (hero, verdict, score_l, why, chart, ser, btn, btn_l) = (pl.hero, pl.verdict, pl.score_l, pl.why, pl.chart, pl.ser, pl.btn, pl.btn_l);

        // status card colours: wash, verdict text, curve
        if self.pl.tone != Some(tn) {
            self.pl.tone = Some(tn);
            let (wash, txt, line) = match tn {
                0 => (th.wash, th.ok_t, th.fill_green),
                1 => (th.wash_w, th.warn_t, th.fill_orange),
                2 => (th.wash_b, th.bad_t, th.fill_red),
                3 => (th.acc_s, th.acc_t, th.fill_blue),
                _ => (th.track, th.t2, th.t3),
            };
            kit::bg(hero, wash);
            kit::text_color(verdict, txt);
            chart.chart_set_series_color(ser, line);
            kit::bg(self.pl.best_line, line);
            for o in self.pl.pins {
                o.set_style_border_color(line, 0);
            }
            chart.invalidate();
        }

        let n_recs = self.pl.recs.len() as i32;
        let vt = match v {
            Verdict::NoSignal => tr("读不到信号").as_bytes().to_vec(),
            Verdict::Here(0 | 1) => tr("这里可以放").as_bytes().to_vec(),
            Verdict::Here(2) => tr("这里一般").as_bytes().to_vec(),
            Verdict::Here(_) => tr("这里信号差").as_bytes().to_vec(),
            Verdict::Recording => cf!(tr("正在记第 %d 处"), self.pl.next_no as i32),
            Verdict::BackTo(no, _) => cf!(tr("放回第 %d 处"), no as i32),
            Verdict::HereBest => tr("就放这里").as_bytes().to_vec(),
            Verdict::AllSame(..) => tr("放哪都差不多").as_bytes().to_vec(),
            Verdict::AllBad(_) => tr("放哪都不行").as_bytes().to_vec(),
            Verdict::NotPlacement(_) => tr("不是摆放的问题").as_bytes().to_vec(),
            Verdict::Flapping(_) => tr("卡在两个基站之间").as_bytes().to_vec(),
        };
        self.set(verdict, vt);

        // the score (as the equation below adds it up), or the countdown
        let inst = now;
        if fresh {
            let t = &mut self.pl.trail;
            match inst {
                Some(s) => {
                    t.push_back(s);
                    if t.len() > 6 {
                        t.pop_front();
                    }
                }
                None => t.clear(),
            }
        }
        // hot / cold: against 5 snapshots ago
        let trend = match (self.pl.trail.front(), self.pl.trail.back()) {
            (Some(a), Some(b)) if self.pl.trail.len() == 6 => b - a,
            _ => 0,
        };
        if recording {
            let left = REC_MS.saturating_sub(now_ms.wrapping_sub(self.pl.rec_t0)).div_ceil(1000).max(1);
            self.set(score_l, cf!("%d s", left as i32));
            kit::text_color(score_l, th.acc_t);
        } else {
            self.set(score_l, inst.map_or("--".to_string(), |s| s.to_string()));
            kit::text_color(score_l, if inst.is_some() { th.t1 } else { th.t3 });
        }

        // the line under it: a note first (10 s), then what the verdict means
        let na = self.pl.note_at;
        let wt: Vec<u8> = if na != 0 && now_ms.wrapping_sub(na) < NOTE_MS && !matches!(v, Verdict::Flapping(_)) {
            self.pl.note.clone()
        } else if !live {
            let alive = m.backend_alive_wall;
            cf!(tr("数字停在 %s"), &dg_hm(if alive != 0 { alive } else { self.home.last_valid_wall }))
        } else {
            match v {
                Verdict::NoSignal if live && !lte_or_nr && rat != Rat::None => tr("这个制式没有 SINR").as_bytes().to_vec(),
                Verdict::NoSignal => tr("等有信号再看。").as_bytes().to_vec(),
                Verdict::Recording => tr("放稳，手拿开，别碰它。").as_bytes().to_vec(),
                Verdict::Here(t) if n_recs == 1 => {
                    let _ = t;
                    tr("再记一处就能比。").as_bytes().to_vec()
                }
                Verdict::Here(_) if trend >= 3 => cf!(tr("比 5 秒前 +%d，继续往这边。"), trend),
                Verdict::Here(_) if trend <= -3 => cf!(tr("比 5 秒前 %d，往回走。"), trend),
                Verdict::Here(0 | 1) => tr("走动看曲线：往上就是变好。").as_bytes().to_vec(),
                Verdict::Here(2) => tr("靠窗、高一点再试试。").as_bytes().to_vec(),
                Verdict::Here(_) => tr("换个地方，或换个房间。").as_bytes().to_vec(),
                Verdict::BackTo(_, gap) if gap > 0 => cf!(tr("现在比它低 %d 分，往回走。"), gap),
                Verdict::BackTo(..) => tr("它分数最高。").as_bytes().to_vec(),
                Verdict::HereBest => tr("现在跟最好的差不多。").as_bytes().to_vec(),
                Verdict::AllSame(k, gap) => cf!(tr("%d 处只差 %d 分，放哪个都行。"), k as i32, gap),
                Verdict::AllBad(k) => cf!(tr("%d 处都差：换个房间，或换张卡。"), k as i32),
                Verdict::NotPlacement(k) => cf!(tr("%d 处一样；基站忙，换位置没用。"), k as i32),
                Verdict::Flapping(k) => cf!(tr("在小区 %d、%d 之间来回切了 %d 次，往一边挪。"), self.pl.flap_pair.0, self.pl.flap_pair.1, k as i32),
            }
        };
        self.set(why, wt);

        // why it is good or bad: the biggest deduction, with its reading
        let rs = self.pl.reason;
        let (rt, rc): (Vec<u8>, u32) = match sm {
            None => (Vec::new(), th.t2),
            Some(a) => match limiter(a, rsrp, rsrq, mhz, widest) {
                None => (tr("干扰低、信号强、基站不忙。").as_bytes().to_vec(), th.t2),
                Some(0) => (cf!(tr("差在干扰：SINR %s dB，转个方向或靠窗。"), &fmt10(a.round() as i32)), th.t1),
                Some(1) => (cf!(tr("差在信号弱：RSRP %d，往窗边、高处挪。"), rsrp), th.t1),
                Some(2) => (cf!(tr("差在基站忙：RSRQ %d，挪位置没用。"), rsrq), th.t1),
                Some(_) => (cf!(tr("差在频宽：只有 %s %d MHz，刚才有过 %d。"), &bands, mhz, widest), th.t1),
            },
        };
        self.set(rs, rt);
        kit::text_color(rs, rc);

        // the score card, on the live readings: the total (= SINR's points less the width's), then
        // each part with its reading and what it means; strength and load take no points of their
        // own (they are in SINR already), so theirs says 正常 / 弱, 不忙 / 忙
        let pl = &self.pl;
        let (eq_v, eq_k, eq_raw) = (pl.eq_v, pl.eq_k, pl.eq_raw);
        let names = [tr("总分"), "SINR dB", tr("频宽 MHz"), tr("信号强度"), tr("基站负载")];
        for i in 0..5 {
            self.set(eq_k[i], names[i]);
        }
        let weak = rsrp != 0 && rsrp <= WEAK_RSRP;
        let busy = rsrq != 0 && rsrq <= BUSY_RSRQ;
        let dash = || "—".to_string();
        let cells: [(String, Vec<u8>, u32, u32); 5] = match sm {
            Some(a) => {
                let pts = steps(a, mhz, widest);
                let off = pts[0] - pts[1];
                let tone_of = |s: i32| match tier(s) {
                    0 | 1 => th.ok_t,
                    2 => th.warn_t,
                    _ => th.bad_t,
                };
                let tiers = [tr("很好"), tr("良好"), tr("一般"), tr("差")];
                [
                    (pts[1].to_string(), tiers[tier(pts[1])].as_bytes().to_vec(), th.t1, tone_of(pts[1])),
                    (fmt10(a.round() as i32), cf!(tr("%d 分"), pts[0]), th.t1, tone_of(pts[0])),
                    (
                        if mhz > 0 { mhz.to_string() } else { dash() },
                        if off > 0 { cf!(tr("扣 %d"), off) } else { tr("不扣").as_bytes().to_vec() },
                        th.t1,
                        if off <= 0 { th.t2 } else if off * 4 < pts[0].max(1) { th.warn_t } else { th.bad_t },
                    ),
                    (
                        if rsrp != 0 { rsrp.to_string() } else { dash() },
                        if rsrp == 0 { Vec::new() } else if weak { tr("弱").as_bytes().to_vec() } else { tr("正常").as_bytes().to_vec() },
                        th.t1,
                        if weak { th.warn_t } else { th.t2 },
                    ),
                    (
                        if rsrq != 0 { rsrq.to_string() } else { dash() },
                        if rsrq == 0 { Vec::new() } else if busy { tr("忙").as_bytes().to_vec() } else { tr("不忙").as_bytes().to_vec() },
                        th.t1,
                        if busy { th.warn_t } else { th.t2 },
                    ),
                ]
            }
            None => std::array::from_fn(|_| (dash(), Vec::new(), th.t3, th.t2)),
        };
        for (i, (v, st, vc, sc)) in cells.into_iter().enumerate() {
            self.set(eq_v[i], v);
            kit::text_color(eq_v[i], vc);
            self.set(eq_raw[i], st);
            kit::text_color(eq_raw[i], sc);
        }

        // places: three cells, and pins on the curve where they were taken
        let stable = marks.iter().any(|k| !k.flap);
        let best = marks.iter().filter(|k| !stable || !k.flap).map(|k| k.score).max().unwrap_or(0);
        let same = marks.len() >= 2 && best - marks.iter().map(|k| k.score).min().unwrap_or(0) <= SAME;
        let pl = &self.pl;
        let (cells, pins, pin_l, best_line, seq) = (pl.cells, pl.pins, pl.pin_l, pl.best_line, pl.seq);
        let recs = pl.recs.clone();
        let cur_key = pl.key.clone();
        let next_no = pl.next_no;
        for i in 0..RECS {
            let c = cells[i];
            match recs.get(i) {
                Some(r) => {
                    let win = !same && recs.len() >= 2 && r.score == best && (!stable || !r.flap);
                    kit::bg(c.bx, if win { th.wash } else { th.card });
                    c.bx.set_style_border_width(0, 0);
                    self.set(c.k, cf!(tr("第 %d 处 · %s"), r.no as i32, &dg_hm(r.at)));
                    self.set(c.n, cf!(tr("%d 分"), r.score));
                    kit::text_color(c.n, if win { th.ok_t } else { th.t1 });
                    let (st, col) = if r.flap {
                        (tr("切过基站").as_bytes().to_vec(), th.warn_t)
                    } else if r.key != cur_key {
                        (cf!(tr("小区 %s"), r.key.get(1..).unwrap_or("")), th.t2)
                    } else if recs.len() < 2 {
                        (Vec::new(), th.t2)
                    } else if same {
                        (tr("差不多").as_bytes().to_vec(), th.t2)
                    } else if win {
                        (tr("最好").as_bytes().to_vec(), th.ok_t)
                    } else if best - r.score <= SAME {
                        (tr("也行").as_bytes().to_vec(), th.t2)
                    } else {
                        let why = match limiter(r.sinr10 as f64, r.rsrp, r.rsrq, r.mhz, widest) {
                            Some(0) => tr("干扰"),
                            Some(1) => tr("信号弱"),
                            Some(2) => tr("基站忙"),
                            Some(_) => tr("频宽窄"),
                            None => "",
                        };
                        if why.is_empty() { (cf!(tr("低 %d"), best - r.score), th.t2) } else { (cf!(tr("低 %d · %s"), best - r.score, why), th.t2) }
                    };
                    self.set(c.s, st);
                    kit::text_color(c.s, col);
                }
                None => {
                    // an empty slot: outlined; the next one says so
                    kit::bg(c.bx, th.bg);
                    c.bx.set_style_border_width(2, 0);
                    c.bx.set_style_border_color(th.sep, 0);
                    let no = next_no as i32 + (i as i32 - recs.len() as i32);
                    self.set(c.k, cf!(tr("第 %d 处"), no));
                    self.set(c.n, "");
                    self.set(c.s, if i == recs.len() { tr("下一处") } else { "" });
                    kit::text_color(c.s, th.acc_t);
                }
            }
        }
        let y_of = |s: i32| CHART_Y + (CHART_H - 1) - s.clamp(0, 100) * (CHART_H - 1) / 100;
        for i in 0..RECS {
            let at = recs.get(i).map(|r| seq.wrapping_sub(r.seq));
            match (recs.get(i), at) {
                (Some(r), Some(back)) if back < CURVE_N => {
                    let idx = (CURVE_N - 1 - back) as i32;
                    let x = idx * (CARD_W - 1) / (CURVE_N as i32 - 1);
                    let win = !same && recs.len() >= 2 && r.score == best && (!stable || !r.flap);
                    pins[i].set_pos((x - PIN / 2).clamp(0, CARD_W - PIN), (y_of(r.score) - PIN / 2).clamp(CHART_Y, HERO_H - PIN));
                    kit::bg(pins[i], if win { th.fill_green } else { th.card });
                    self.set(pin_l[i], (r.no % 10).to_string());
                    kit::text_color(pin_l[i], if win { 0xffffff } else { th.t1 });
                    kit::show(pins[i], true);
                    pins[i].move_foreground();
                }
                _ => kit::show(pins[i], false),
            }
        }
        let line_on = recs.len() >= 2 && !same;
        if line_on {
            best_line.set_y(y_of(best) - 1);
        }
        kit::show(best_line, line_on);

        // the button follows the verdict
        let to_speed = matches!(v, Verdict::NotPlacement(_));
        self.pl.to_speed = to_speed;
        let (bt, kind) = match v {
            Verdict::Recording => (tr("取消这次"), BtnKind::Plain),
            Verdict::NotPlacement(_) => (tr("去测速"), BtnKind::Primary),
            Verdict::AllBad(_) => (tr("换个房间再记"), BtnKind::Primary),
            _ if !recs.is_empty() => (tr("再记一处"), BtnKind::Primary),
            _ => (tr("记下这里"), BtnKind::Primary),
        };
        self.set(btn_l, bt);
        kit::button_kind(btn, btn_l, kind);

        // below the fold
        let pl = &self.pl;
        let (best_l, cmp, cell) = (pl.best_l, pl.cmp, pl.cell);
        if pl.have_best {
            let (b, at) = (pl.best, pl.best_at);
            self.set(best_l, cf!(tr("%d 分 · %s"), b, &ago(m.now - at)));
            match now {
                Some(s) if b - s <= SAME => {
                    self.set(cmp, tr("● 接近最好"));
                    kit::text_color(cmp, th.ok_t);
                }
                Some(s) => {
                    self.set(cmp, cf!(tr("▲ 比最好低 %d 分"), b - s));
                    kit::text_color(cmp, th.warn_t);
                }
                None => {
                    self.set(cmp, "—");
                    kit::text_color(cmp, th.t3);
                }
            }
        } else {
            self.set(best_l, "—");
            self.set(cmp, "—");
            kit::text_color(cmp, th.t3);
        }
        kit::text_color(best_l, if live { th.t1 } else { th.t3 });
        let span = self.pl.span;
        if self.pl.pci != 0 {
            self.set(span, cf!(tr("近 3 分钟 · 小区 %d"), self.pl.pci));
        } else {
            self.set(span, tr("近 3 分钟"));
        }
        if !band.is_empty() && pci != 0 {
            self.set(cell, cf!("%s · PCI %d", &band, pci));
        } else {
            self.set(cell, "—");
        }
        kit::text_color(cell, if live { th.t1 } else { th.t3 });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(no: u32, s10: i32, r: i32, q: i32) -> Mark {
        Rec { no, score: score(s10), rsrp: r, rsrq: q, ..Default::default() }.mark()
    }

    fn flapped(mut m: Mark) -> Mark {
        m.flap = true;
        m
    }

    #[test]
    fn score_is_the_3gpp_curve() {
        // TR 36.942 A.2: 0.6·log2(1+SINR), from -10 dB, capped at 4.4 b/s/Hz (about 22 dB)
        assert_eq!(score(250), 100);
        assert_eq!(score(220), 100);
        assert_eq!(score(200), 91);
        assert_eq!(score(170), 77);
        assert_eq!(score(130), 60);
        assert_eq!(score(50), 28);
        assert_eq!(score(0), 14);
        assert_eq!(score(-100), 2);
        assert_eq!(score(-101), 0);
        // continuous: a tenth of a dB never moves it more than a point
        for t in -100..250 {
            assert!(sinr_score(t as f64 + 1.0) - sinr_score(t as f64) < 1.0, "{t}");
        }
    }

    #[test]
    fn the_reason() {
        // SINR 15.2 scores 69; why it is short: interference, a weak signal or a busy cell
        assert_eq!(steps(152.0, 100, 100), [69, 69]);
        assert_eq!(limiter(152.0, -86, -11, 100, 100), Some(0));
        assert_eq!(limiter(152.0, -112, -11, 100, 100), Some(1));
        assert_eq!(limiter(152.0, -86, -17, 100, 100), Some(2));
        // n5 15 MHz where n78 100 MHz was seen: the width takes most
        assert_eq!(steps(250.0, 15, 100), [100, 15]);
        assert_eq!(limiter(250.0, -80, -9, 15, 100), Some(3));
        assert_eq!(limiter(250.0, -80, -9, 100, 100), None);
    }

    #[test]
    fn width_is_the_primary_carrier() {
        let mut d = ui_model::core_snapshot_t::default();
        d.nr_width = "100".into();
        d.nr_band_text = "n78".into();
        d.bandwidth = "20MHz".into();
        d.band_text = "LTE BAND 3".into();
        assert_eq!(primary_width(&d, Rat::G5Sa).0, 100);
        assert_eq!(primary_width(&d, Rat::G4).0, 20);
        // NSA: anchor + the leg, only when the leg is there
        assert_eq!(primary_width(&d, Rat::G5Nsa).0, 20);
        d.nr_rsrp = -95;
        assert_eq!(primary_width(&d, Rat::G5Nsa).0, 120);
        assert_eq!(primary_width(&d, Rat::G3).0, 0);
    }

    #[test]
    fn width_is_relative() {
        // n3 20 MHz where n78 100 MHz was seen: a fifth; a visit that only sees 20 MHz: none
        assert!((width_k(20, 100) - 0.2).abs() < 1e-9);
        assert_eq!(width_k(20, 20), 1.0);
        assert_eq!(width_k(0, 100), 1.0);
        assert_eq!(with_width(60, 20, 100), 12);
    }

    #[test]
    fn verdicts() {
        assert_eq!(judge(None, false, 0, &[]), Verdict::NoSignal);
        assert_eq!(judge(Some(65), false, 0, &[]), Verdict::Here(1));
        assert_eq!(judge(Some(49), false, 0, &[]), Verdict::Here(2));
        assert_eq!(judge(Some(10), false, 0, &[]), Verdict::Here(3));
        assert_eq!(judge(Some(49), true, 0, &[]), Verdict::Recording);
        // one clearly best: go back to it, or stay when here is as good
        let good = [mk(1, 184, -84, -12), mk(2, 193, -81, -12), mk(3, 96, -84, -12)];
        assert!(matches!(judge(Some(49), false, 0, &good), Verdict::BackTo(2, _)));
        assert_eq!(judge(Some(good[1].score - 3), false, 0, &good), Verdict::HereBest);
        // all poor: say so, don't crown the least bad
        let bad = [mk(1, 34, -113, -11), mk(2, 26, -115, -10), mk(3, 30, -114, -11)];
        assert_eq!(judge(Some(9), false, 0, &bad), Verdict::AllBad(3));
        // all the same and middling, a decent signal in a busy cell: not a placement problem
        let busy = [mk(1, 100, -78, -17), mk(2, 96, -80, -17), mk(3, 98, -79, -17)];
        assert_eq!(judge(Some(40), false, 0, &busy), Verdict::NotPlacement(3));
        // all the same, load fine
        let same = [mk(1, 164, -78, -9), mk(2, 156, -80, -9)];
        assert!(matches!(judge(Some(60), false, 0, &same), Verdict::AllSame(2, _)));
        // ping-pong between cells comes first; a place recorded across a switch isn't crowned
        assert_eq!(judge(Some(59), false, 3, &good), Verdict::Flapping(3));
        // switching while the score holds up (良好 and above) is not the headline
        assert_eq!(judge(Some(good[1].score - 3), false, 3, &good), Verdict::HereBest);
        // a tie goes to the later place: the one last called best
        let tie = [mk(1, 50, -95, -10), mk(2, 130, -90, -10), mk(3, 130, -90, -10)];
        assert!(matches!(judge(Some(5), false, 0, &tie), Verdict::BackTo(3, _)));
        let mixed = [mk(1, 120, -84, -12), flapped(mk(2, 220, -81, -12))];
        assert!(matches!(judge(Some(20), false, 0, &mixed), Verdict::BackTo(1, _)));
    }

    #[test]
    fn sinr_in_tenths() {
        assert_eq!(tenths("17.7"), Some(177));
        assert_eq!(tenths("-2.5"), Some(-25));
        assert_eq!(tenths("13"), Some(130));
        assert_eq!(tenths(""), None);
        assert_eq!(tenths("-"), None);
        assert_eq!(fmt10(177), "17.7");
        assert_eq!(fmt10(-25), "-2.5");
        assert_eq!(fmt10(-5), "-0.5");
        assert_eq!(fmt10(0), "0.0");
    }
}
