//! The touch UI's components (src/ui_kit.c, include/ui_kit.h; design doc §3), function for
//! function. Every page is built from these and nothing else sets a colour, radius or font on
//! its own: colours from t(), fonts from uf().
//!
//! Positions are relative to the parent. "xr" = right edge at xr: such labels are aligned to
//! the parent's right side, so they stay right-aligned when their text changes.
// SPDX-License-Identifier: MIT

use crate::lang::tr;
use crate::logic::{self, BatState};
use crate::lv::{self, sys, EventCb, Font, Obj, Series};
use crate::theme::{t, uf};
use std::mem::MaybeUninit;

pub const W: i32 = 320;
pub const H: i32 = 480;
/// status bar
pub const BAR_H: i32 = 26;
/// subpage header, status bar included
pub const NAV_H: i32 = 62;
/// floating tab capsule
pub const TAB_H: i32 = 44;
/// capsule to screen bottom
pub const TAB_GAP: i32 = 8;
/// scroll content keeps this clear at the end
pub const TAB_PAD: i32 = 60;
/// card to screen edge, card to card
pub const MARGIN: i32 = 10;
pub const CARD_W: i32 = W - 2 * MARGIN;
/// row inset inside a card
pub const PAD: i32 = 14;
pub const ROW_H: i32 = 40;
pub const ROW2_H: i32 = 50;
pub const HERO_H: i32 = 72;
pub const R_CARD: i32 = 20;
pub const R_OPT: i32 = 14;
pub const R_SHEET: i32 = 22;
/// false = no animation anywhere (design doc §3)
pub const ANIM: bool = true;
/// LV_SYMBOL_OK (✓ in the Montserrat symbol range: the device CJK font has none)
pub const SYMBOL_OK: &str = "\u{F00C}";

const MAIN: u32 = lv::LV_PART_MAIN;

// ---------------------------------------------------------------- primitives
fn pw(p: Obj) -> i32 {
    let w = p.style_width(MAIN);
    if w > 0 && !lv::coord_is_spec(w) {
        return w;
    }
    p.update_layout();
    p.width()
}

pub fn bx(p: Obj, x: i32, y: i32, w: i32, h: i32, col: u32, r: i32) -> Obj {
    let o = Obj::create(p);
    o.remove_style_all();
    o.remove_flag(lv::LV_OBJ_FLAG_SCROLLABLE);
    o.remove_flag(lv::LV_OBJ_FLAG_CLICKABLE); // decoration: taps go to the parent
    o.set_pos(x, y);
    o.set_size(w, h);
    o.set_style_radius(r, 0);
    o.set_style_bg_color(col, 0);
    o.set_style_bg_opa(lv::OPA_COVER, 0);
    o
}

pub fn label(p: Obj, f: Font, col: u32, x: i32, y: i32, text: impl AsRef<[u8]>) -> Obj {
    let l = Obj::label(p);
    l.set_style_text_font(f, 0);
    l.set_style_text_color(col, 0);
    l.set_text(text);
    l.set_pos(x, y);
    l
}

pub fn label_r(p: Obj, f: Font, col: u32, xr: i32, y: i32, text: impl AsRef<[u8]>) -> Obj {
    let l = label(p, f, col, 0, 0, text);
    l.align(lv::LV_ALIGN_TOP_RIGHT, xr - pw(p), y); // re-applied whenever the text changes size
    l
}

/// Width-limited label: text past `w` ends in "…" (names), or wraps.
pub fn label_w(p: Obj, f: Font, col: u32, x: i32, y: i32, w: i32, wrap: bool, text: impl AsRef<[u8]>) -> Obj {
    let l = label(p, f, col, x, y, text);
    l.set_width(w);
    l.set_long_mode(if wrap { lv::LV_LABEL_LONG_MODE_WRAP } else { lv::LV_LABEL_LONG_MODE_DOTS });
    if !wrap {
        l.set_height(f.line_height()); // dots need a fixed height, else it wraps
    }
    l
}

pub fn text_color(l: Obj, col: u32) {
    l.set_style_text_color(col, 0);
}

pub fn bg(o: Obj, col: u32) {
    o.set_style_bg_color(col, 0);
}

pub fn dot(p: Obj, x: i32, y: i32, d: i32, col: u32) -> Obj {
    bx(p, x, y, d, d, col, d / 2 + 1)
}

pub fn show(o: Obj, visible: bool) {
    if o.is_null() {
        return;
    }
    if visible {
        o.remove_flag(lv::LV_OBJ_FLAG_HIDDEN);
    } else {
        o.add_flag(lv::LV_OBJ_FLAG_HIDDEN);
    }
}

/// Pressed = one shade towards the page: darker on light, lighter on dark. A filter rather
/// than a fixed colour, so it also works on objects whose background refresh recolours.
unsafe extern "C" fn press_filter_cb(_d: *const sys::lv_color_filter_dsc_t, c: sys::lv_color_t, opa: u8) -> sys::lv_color_t {
    // SAFETY: plain colour arithmetic
    unsafe { if t().dark { sys::lv_color_lighten(c, opa) } else { sys::lv_color_darken(c, opa) } }
}

static mut PRESS_FILTER: MaybeUninit<sys::lv_color_filter_dsc_t> = MaybeUninit::zeroed();
static PRESS_INIT: std::sync::Once = std::sync::Once::new();

/// Make `o` tappable with the standard pressed feedback (filter; a transparent object also
/// gets a track-coloured pressed background). Hit area grows to ≥ 40×40.
pub fn tappable(o: Obj, cb: Option<EventCb>, user: usize) {
    let f = &raw mut PRESS_FILTER;
    // SAFETY: one thread; the descriptor lives for the process
    PRESS_INIT.call_once(|| unsafe { sys::lv_color_filter_dsc_init((*f).as_mut_ptr(), Some(press_filter_cb)) });
    o.add_flag(lv::LV_OBJ_FLAG_CLICKABLE);
    // SAFETY: initialised above
    o.set_style_color_filter_dsc(unsafe { (*f).assume_init_ref() }, lv::LV_STATE_PRESSED);
    o.set_style_color_filter_opa(40, lv::LV_STATE_PRESSED);
    if o.style_bg_opa(MAIN) < lv::OPA_50 {
        o.set_style_bg_color(t().track, lv::LV_STATE_PRESSED);
        o.set_style_bg_opa(lv::OPA_COVER, lv::LV_STATE_PRESSED);
    }
    if let Some(cb) = cb {
        o.add_event_cb(cb, lv::LV_EVENT_CLICKED, user);
    }
    let (h, w) = (o.style_height(0), o.style_width(0));
    let hh = if h > 0 && !lv::coord_is_spec(h) { h } else { 40 };
    let ww = if w > 0 && !lv::coord_is_spec(w) { w } else { 40 };
    let small = hh.min(ww);
    if small < 40 {
        o.set_ext_click_area((40 - small + 1) / 2);
    }
}

// ---------------------------------------------------------------- cards, rows
/// 12 px t3 above a card.
pub fn section(p: Obj, y: i32, title: &str) -> Obj {
    label(p, uf().cj12, t().t3, MARGIN + 6, y, title)
}

pub fn card(p: Obj, x: i32, y: i32, w: i32, h: i32) -> Obj {
    let c = bx(p, x, y, w, h, t().card, R_CARD);
    if t().dark {
        // dark cards: 1 px top highlight instead of a shadow
        c.set_style_border_side(lv::LV_BORDER_SIDE_TOP, 0);
        c.set_style_border_width(1, 0);
        c.set_style_border_color(0xffffff, 0);
        c.set_style_border_opa(t().hl_opa, 0);
    }
    c
}

/// From x=14 to the edge.
pub fn sep(c: Obj, y: i32) -> Obj {
    bx(c, PAD, y, pw(c) - PAD, 1, t().sep, 0)
}

/// A right-aligned row value never runs into its row name: it gets what the name leaves
/// (12 px gap) and ends in "…" past that.
fn value_fits_beside(v: Obj, key: Obj, right: i32) {
    let kw = lv::text_w(&key.text(), key.style_text_font(0));
    // the x the key was placed at: lv_obj_get_x() is 0 until the first layout
    let max = (right - key.style_x(MAIN) - kw - 12).max(40);
    v.set_style_max_width(max, 0);
    v.set_long_mode(lv::LV_LABEL_LONG_MODE_DOTS);
    // dots need a fixed height; callers may switch the value to a 13–15 px CJK font later
    let (h, h2) = (v.style_text_font(0).line_height(), uf().cj15.line_height());
    v.set_height(h.max(h2));
}

/// A 40 px key/value row at y inside card c; returns the value label (right-aligned, n15).
pub fn row(c: Obj, y: i32, key: &str, first: bool) -> Obj {
    if !first {
        sep(c, y);
    }
    let k = label(c, uf().cj14, t().t2, PAD, y + 11, key);
    let v = label_r(c, uf().n15, t().t1, pw(c) - PAD, y + 10, "");
    value_fits_beside(v, k, pw(c) - PAD);
    v
}

pub fn chevron(c: Obj, y: i32) -> Obj {
    label_r(c, uf().cj15, t().t3, pw(c) - PAD, y + 10, "›")
}

/// A tappable row with "›" at the right; the value label ends before it.
pub fn row_nav(c: Obj, y: i32, key: &str, first: bool, cb: Option<EventCb>, user: usize) -> Obj {
    let hit = bx(c, 0, y, pw(c), ROW_H, t().card, 0);
    hit.set_style_bg_opa(lv::OPA_TRANSP, 0);
    hit.set_style_bg_opa(lv::OPA_COVER, lv::LV_STATE_PRESSED);
    hit.set_style_bg_color(t().track, lv::LV_STATE_PRESSED);
    hit.add_flag(lv::LV_OBJ_FLAG_CLICKABLE);
    if let Some(cb) = cb {
        hit.add_event_cb(cb, lv::LV_EVENT_CLICKED, user);
    }
    if !first {
        sep(c, y);
    }
    let k = label(c, uf().cj14, t().t1, PAD, y + 11, key);
    chevron(c, y);
    let v = label_r(c, uf().cj13, t().t2, pw(c) - PAD - 16, y + 12, "");
    value_fits_beside(v, k, pw(c) - PAD - 16);
    v
}

// ---------------------------------------------------------------- controls
pub const SEG_MAX: usize = 5;

/// Segmented control, 30 px high, up to SEG_MAX items; cb gets user = index.
#[derive(Default, Clone)]
pub struct Seg {
    pub obj: Obj,
    pub item: Vec<Obj>,
    pub lbl: Vec<Obj>,
    pub sel: i32,
}

pub fn seg(p: Obj, x: i32, y: i32, w: i32, items: &[&'static str], cb: Option<EventCb>) -> Seg {
    let n = items.len().min(SEG_MAX);
    let mut s = Seg { sel: -1, ..Default::default() };
    s.obj = bx(p, x, y, w, 30, t().track, 15);
    let iw = (w - 4) / n as i32;
    for (i, it) in items.iter().take(n).enumerate() {
        let o = bx(s.obj, 2 + i as i32 * iw, 2, iw, 26, t().pill, 13);
        o.set_style_bg_opa(lv::OPA_TRANSP, 0);
        o.set_style_shadow_color(0x000000, 0);
        o.set_style_shadow_offset_y(1, 0);
        tappable(o, cb, i);
        o.set_ext_click_area(7);
        // items are often a static n_() table: shown here
        let l = label(o, uf().cj13, t().t2, 0, 0, tr(it));
        l.center();
        s.item.push(o);
        s.lbl.push(l);
    }
    s
}

pub fn seg_set(s: &mut Seg, sel: i32) {
    s.sel = sel;
    for (i, (&it, &l)) in s.item.iter().zip(&s.lbl).enumerate() {
        let on = i as i32 == sel;
        it.set_style_bg_color(t().pill, 0);
        it.set_style_bg_opa(if on { lv::OPA_COVER } else { lv::OPA_TRANSP }, 0);
        it.set_style_shadow_width(if on { 4 } else { 0 }, 0);
        it.set_style_shadow_opa(if on { 40 } else { 0 }, 0);
        text_color(l, if on { t().t1 } else { t().t2 });
    }
}

/// Draw item i as "armed" (first tap of a two-step confirm: fillOrange).
pub fn seg_arm(s: &Seg, i: i32) {
    if i < 0 || i as usize >= s.item.len() {
        return;
    }
    s.item[i as usize].set_style_bg_color(t().fill_orange, 0);
    s.item[i as usize].set_style_bg_opa(lv::OPA_COVER, 0);
    text_color(s.lbl[i as usize], 0xffffff);
}

pub fn toggle(p: Obj, xr: i32, y: i32, cb: Option<EventCb>, user: usize) -> Obj {
    let sw = Obj::switch(p);
    let (ind, knob) = (lv::LV_PART_INDICATOR, lv::LV_PART_KNOB);
    sw.set_size(44, 26);
    sw.align(lv::LV_ALIGN_TOP_RIGHT, xr - pw(p), y);
    sw.remove_flag(lv::LV_OBJ_FLAG_SCROLL_ON_FOCUS);
    sw.set_style_bg_color(t().sw_off, MAIN);
    sw.set_style_bg_opa(lv::OPA_COVER, MAIN);
    sw.set_style_bg_color(t().green, ind | lv::LV_STATE_CHECKED);
    sw.set_style_bg_color(0xffffff, knob);
    // oval thumb: the knob widened with negative padding
    sw.set_style_pad_left(-5, knob);
    sw.set_style_pad_right(-5, knob);
    sw.set_style_pad_top(-2, knob);
    sw.set_style_pad_bottom(-2, knob);
    sw.set_style_shadow_width(4, knob);
    sw.set_style_shadow_opa(60, knob);
    sw.set_style_shadow_offset_y(1, knob);
    sw.set_style_pad_all(5, MAIN);
    sw.set_style_anim_duration(0, 0);
    sw.set_ext_click_area(7);
    if let Some(cb) = cb {
        sw.add_event_cb(cb, lv::LV_EVENT_VALUE_CHANGED, user);
    }
    sw
}

pub fn toggle_set(sw: Obj, on: bool) {
    if sw.is_null() || on == sw.has_state(lv::LV_STATE_CHECKED) {
        return;
    }
    if on {
        sw.add_state(lv::LV_STATE_CHECKED);
    } else {
        sw.remove_state(lv::LV_STATE_CHECKED);
    }
}

pub fn slider(p: Obj, x: i32, y: i32, w: i32) -> Obj {
    let sl = Obj::slider(p);
    let (ind, knob) = (lv::LV_PART_INDICATOR, lv::LV_PART_KNOB);
    sl.set_pos(x, y);
    sl.set_size(w, 6);
    sl.remove_flag(lv::LV_OBJ_FLAG_SCROLL_ON_FOCUS);
    sl.set_style_bg_color(t().track, MAIN);
    sl.set_style_bg_opa(lv::OPA_COVER, MAIN);
    sl.set_style_bg_color(t().blue, ind);
    sl.set_style_bg_color(0xffffff, knob);
    sl.set_style_pad_hor(11, knob); // 28×22 capsule knob
    sl.set_style_pad_ver(8, knob);
    sl.set_style_radius(lv::RADIUS_CIRCLE, knob);
    sl.set_style_shadow_width(6, knob);
    sl.set_style_shadow_opa(70, knob);
    sl.set_ext_click_area(17);
    sl
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BtnKind {
    Primary,
    Plain,
    Danger,
    Armed,
}

pub fn button_kind(b: Obj, lbl: Obj, k: BtnKind) {
    let th = t();
    let bgc = match k {
        BtnKind::Primary => th.fill_blue,
        BtnKind::Armed => th.fill_orange,
        _ => th.track,
    };
    let fg = match k {
        BtnKind::Plain => th.acc_t,
        BtnKind::Danger => th.bad_t,
        _ => 0xffffff,
    };
    bg(b, bgc);
    if !lbl.is_null() {
        text_color(lbl, fg);
    }
}

/// Capsule button; returns it and its label. w = 0: fit the text.
#[allow(clippy::too_many_arguments)]
pub fn button(p: Obj, x: i32, y: i32, w: i32, h: i32, text: &str, k: BtnKind, cb: Option<EventCb>, user: usize) -> (Obj, Obj) {
    let b = bx(p, x, y, if w > 0 { w } else { 80 }, h, t().track, h / 2);
    let l = label(b, uf().cj15b, t().acc_t, 0, 0, text);
    if w <= 0 {
        b.set_width(lv::SIZE_CONTENT);
        b.set_style_pad_hor(16, 0);
        l.align(lv::LV_ALIGN_LEFT_MID, 0, 0);
    }
    l.center();
    button_kind(b, l, k);
    tappable(b, cb, user);
    (b, l)
}

/// Band chip (28 high, fits the text); returns it and its label.
pub fn chip(p: Obj, x: i32, y: i32, text: &str) -> (Obj, Obj) {
    let ch = bx(p, x, y, 10, 28, t().track, 14);
    let l = label(ch, uf().n15, t().t2, 11, 4, text);
    l.update_layout();
    ch.set_width(l.width() + 22);
    (ch, l)
}

pub fn chip_set(chip: Obj, lbl: Obj, on: bool) {
    bg(chip, if on { t().fill_blue } else { t().track });
    text_color(lbl, if on { 0xffffff } else { t().t2 });
}

pub fn chip_w(chip: Obj) -> i32 {
    chip.style_width(0)
}

/// 2×2 option (134×46): title + subtitle; selected = fillBlue.
#[derive(Default, Clone, Copy)]
pub struct Opt {
    pub obj: Obj,
    pub t: Obj,
    pub s: Obj,
}

pub fn opt(p: Obj, x: i32, y: i32, title: &str, sub: &str, cb: Option<EventCb>, user: usize) -> Opt {
    let obj = bx(p, x, y, 134, 46, t().track, R_OPT);
    let tl = label(obj, uf().cj14, t().t1, 0, 0, title);
    tl.align(lv::LV_ALIGN_TOP_MID, 0, 5);
    let s = label(obj, uf().cj12, t().t3, 0, 0, sub);
    s.align(lv::LV_ALIGN_TOP_MID, 0, 26);
    tappable(obj, cb, user);
    Opt { obj, t: tl, s }
}

pub fn opt_set(o: &Opt, on: bool, armed: bool) {
    bg(o.obj, if armed { t().fill_orange } else if on { t().fill_blue } else { t().track });
    text_color(o.t, if on || armed { 0xffffff } else { t().t1 });
    text_color(o.s, if on || armed { t().on_fill } else { t().t3 });
}

// ---------------------------------------------------------------- tiles
#[derive(Default, Clone, Copy)]
pub struct Tile {
    pub obj: Obj,
    pub icon: Obj,
    pub name: Obj,
    pub sub: Obj,
    pub badge: Obj,
    pub badge_l: Obj,
    pub dot: Obj,
}

pub fn tile(p: Obj, x: i32, y: i32, icon: &str, name: &str, cb: Option<EventCb>, user: usize) -> Tile {
    let obj = card(p, x, y, 145, 78);
    let tl = Tile {
        obj,
        icon: label(obj, lv::montserrat(16), t().acc_t, 12, 12, icon),
        name: label(obj, uf().cj15, t().t1, 36, 12, name),
        sub: label_w(obj, uf().cj12, t().t2, 12, 50, 121, false, ""),
        dot: dot(obj, 12, 55, 7, t().green),
        badge: Obj::NULL,
        badge_l: Obj::NULL,
    };
    show(tl.dot, false);
    let badge = bx(obj, 145 - 12 - 22, 12, 22, 18, t().fill_red, 9);
    let badge_l = label(badge, uf().n12, 0xffffff, 0, 0, "");
    badge_l.center();
    show(badge, false);
    tappable(obj, cb, user);
    Tile { badge, badge_l, ..tl }
}

/// 「开着」= a green dot before the subtitle and the subtitle in okT; the tile stays white.
pub fn tile_set(tl: &Tile, on: bool, dim: bool, badge: i32) {
    show(tl.dot, on);
    tl.sub.set_x(if on { 24 } else { 12 });
    text_color(tl.name, if dim { t().t3 } else { t().t1 });
    text_color(tl.sub, if on { t().ok_t } else { t().t2 });
    if badge > 0 {
        tl.badge_l.set_text(badge.min(99).to_string());
    }
    show(tl.badge, badge > 0);
}

// ---------------------------------------------------------------- status block
#[derive(Default, Clone, Copy)]
pub struct Hero {
    pub wash: Obj,
    pub wash2: Obj,
    pub dot: Obj,
    pub st: Obj,
    pub rtop: Obj,
    pub big: Obj,
    pub unit: Obj,
    pub r1: Obj,
    pub r2: Obj,
}

/// big_font: n32 for numbers, cj22b/cj24b for words.
pub fn hero(card: Obj, big_font: Font) -> Hero {
    let cw = pw(card);
    let wash = bx(card, 0, 0, cw, HERO_H, t().wash, R_CARD);
    // LVGL has no per-corner radius: a square filler covers the wash's bottom corners
    let wash2 = bx(card, 0, HERO_H - 20, cw, 20, t().wash, 0);
    let dt = dot(card, 14, 15, 7, t().green);
    let st = label(card, uf().cj13, t().ok_t, 26, 10, "");
    let rtop = label_w(card, uf().cj13, t().t2, cw - PAD - 170, 10, 170, false, "");
    rtop.set_style_text_align(lv::LV_TEXT_ALIGN_RIGHT, 0);
    let big = label(card, big_font, t().t1, 12, if big_font == uf().n32 { 30 } else { 34 }, "");
    let unit = label(card, uf().n15, t().t2, 60, 44, "");
    let r1 = label_r(card, uf().n17, t().t1, cw - PAD, 30, "");
    let r2 = label_w(card, uf().cj12, t().t2, cw - PAD - 150, 52, 150, false, "");
    r2.set_style_text_align(lv::LV_TEXT_ALIGN_RIGHT, 0);
    Hero { wash, wash2, dot: dt, st, rtop, big, unit, r1, r2 }
}

/// tone: 0 good, 1 warn, 2 bad, 3 neutral (track, t3 text), 4 selected (accS);
/// filled, white text: 5 good (fillGreen), 6 warn (fillOrange), 7 bad (fillRed).
pub fn hero_tone(h: &Hero, tone: i32) {
    let th = t();
    let wash = [th.wash, th.wash_w, th.wash_b, th.track, th.acc_s, th.fill_green, th.fill_orange, th.fill_red];
    let text = [th.ok_t, th.warn_t, th.bad_t, th.t3, th.acc_t, th.on_fill, th.on_fill, th.on_fill];
    let dt = [th.green, th.orange, th.red, th.t3, th.blue, 0xffffff, 0xffffff, 0xffffff];
    let tone = if (0..8).contains(&tone) { tone as usize } else { 3 };
    bg(h.wash, wash[tone]);
    bg(h.wash2, wash[tone]);
    bg(h.dot, dt[tone]);
    text_color(h.st, text[tone]);
    // filled: white text (design doc §1); r1/r2/rtop are the caller's
    text_color(h.big, if tone == 3 { th.t3 } else if tone >= 5 { 0xffffff } else { th.t1 });
}

/// Place the unit right after the big number (call after changing big).
pub fn hero_layout(h: &Hero) {
    h.big.update_layout();
    h.unit.set_x(h.big.x() + h.big.width() + 3);
}

// ---------------------------------------------------------------- battery
#[derive(Default, Clone, Copy)]
pub struct Battery {
    pub body: Obj,
    pub fill: Obj,
    pub digits: Obj,
    pub nub: Obj,
    pub xr: i32,
}

pub fn battery(p: Obj, xr: i32, y: i32) -> Battery {
    let body = bx(p, xr - 30, y, 27, 14, t().t1, 5);
    body.set_style_clip_corner(true, 0);
    let fill = bx(body, 0, 0, 3, 14, t().red, 0);
    let digits = label(body, uf().nbat, t().bg, 0, 0, "");
    let nub = bx(p, xr - 2, y + 4, 2, 6, t().t1, 1);
    nub.set_style_bg_opa(140, 0);
    Battery { body, fill, digits, nub, xr }
}

/// body + nub + gap: what the rate must keep clear of
pub fn battery_w(pct: i32, charging: bool) -> i32 {
    logic::bat_body_w(pct, charging) + 3
}

/// Centre the digits on their ink, not on the label box (the box carries the font's
/// descender and side bearings).
fn bat_digits_place(l: Obj, s: &[u8], x0: i32, w: i32, h: i32) {
    let f = l.style_text_font(0);
    let fr = f.raw();
    let (mut pen, mut il, mut ir, mut top, mut bot, mut any) = (0i32, 0i32, 0i32, 1000i32, -1000i32, false);
    for (i, &c) in s.iter().enumerate() {
        let next = s.get(i + 1).copied().unwrap_or(0) as u32;
        let Some(g) = f.glyph(c as u32, next) else { continue };
        if !any {
            il = pen + g.ofs_x as i32;
        }
        ir = pen + g.ofs_x as i32 + g.box_w as i32;
        let gt = fr.line_height - fr.base_line - (g.ofs_y as i32 + g.box_h as i32);
        let gb = fr.line_height - fr.base_line - g.ofs_y as i32;
        top = top.min(gt);
        bot = bot.max(gb);
        pen += g.adv_w as i32;
        any = true;
    }
    if !any {
        l.align(lv::LV_ALIGN_CENTER, 0, 0);
        return;
    }
    l.align(lv::LV_ALIGN_TOP_LEFT, x0 + (w - (ir - il)) / 2 - il, (h - (bot - top)) / 2 - top);
}

/// Normal: solid body, digits knocked out in the page colour. Charging: the whole body vivid
/// green. ≤ 20 %: grey body with a red sliver, digits stay clear of it.
pub fn battery_set(b: &Battery, pct: i32, charging: bool) {
    let th = t();
    let pct = pct.clamp(0, 100);
    let s = pct.to_string();
    let st = logic::bat_state(pct, charging);
    let w = logic::bat_body_w(pct, charging);
    let body = match st {
        BatState::Charging => th.green,
        BatState::Low => th.track,
        BatState::Normal => th.t1,
    };
    let fg = match st {
        BatState::Charging => {
            if th.dark {
                th.bg
            } else {
                0xffffff
            }
        }
        BatState::Low => th.t1,
        BatState::Normal => th.bg,
    };
    b.body.set_width(w);
    b.body.set_x(b.xr - w - 3);
    bg(b.body, body);
    b.digits.set_text(&s);
    text_color(b.digits, fg);
    show(b.fill, st == BatState::Low);
    if st == BatState::Low {
        b.fill.set_width(logic::bat_red_w(pct, w));
        bat_digits_place(b.digits, s.as_bytes(), 7, w - 7, 14);
    } else {
        bat_digits_place(b.digits, s.as_bytes(), 0, w, 14);
    }
    bg(b.nub, if st == BatState::Charging { th.green } else { th.t1 });
}

// ---------------------------------------------------------------- charts
/// Area under the first series: flat low-opacity triangles + rects (LVGL has no area series);
/// the second series dashed.
unsafe extern "C" fn chart_fill_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's DRAW_TASK_ADDED event on a chart: the task, its descriptors and layer are
    // valid for the call
    unsafe {
        let dt = sys::lv_event_get_draw_task(e);
        let base = sys::lv_draw_task_get_draw_dsc(dt) as *mut sys::lv_draw_dsc_base_t;
        if (*base).part != lv::LV_PART_ITEMS || sys::lv_draw_task_get_type(dt) != sys::lv_draw_task_type_t::LV_DRAW_TASK_TYPE_LINE {
            return;
        }
        let obj = sys::lv_event_get_target_obj(e);
        if (*base).id1 == 1 {
            let ld = sys::lv_draw_task_get_line_dsc(dt);
            (*ld).dash_width = 4;
            (*ld).dash_gap = 3;
            return;
        }
        if (*base).id1 != 0 {
            return;
        }
        let mut co = sys::lv_area_t { x1: 0, y1: 0, x2: 0, y2: 0 };
        sys::lv_obj_get_coords(obj, &mut co);
        let col = sys::lv_chart_get_series_color(obj, sys::lv_chart_get_series_next(obj, std::ptr::null()));
        let ld = sys::lv_draw_task_get_line_dsc(dt);
        let n = (*ld).point_cnt;
        for i in 0..(n - 1).max(0) {
            let p1 = *(*ld).points.add(i as usize);
            let p2 = *(*ld).points.add(i as usize + 1);
            if p1.x == lv::DRAW_LINE_POINT_NONE || p2.x == lv::DRAW_LINE_POINT_NONE {
                continue;
            }
            let mut td = MaybeUninit::<sys::lv_draw_triangle_dsc_t>::zeroed();
            sys::lv_draw_triangle_dsc_init(td.as_mut_ptr());
            let mut td = td.assume_init();
            td.p[0] = p1;
            td.p[1] = p2;
            td.p[2].x = if p1.y < p2.y { p1.x } else { p2.x };
            td.p[2].y = p1.y.max(p2.y);
            td.color = col;
            td.opa = 36;
            sys::lv_draw_triangle((*base).layer, &td);
            let mut rd = MaybeUninit::<sys::lv_draw_rect_dsc_t>::zeroed();
            sys::lv_draw_rect_dsc_init(rd.as_mut_ptr());
            let mut rd = rd.assume_init();
            rd.bg_color = col;
            rd.bg_opa = 36;
            let a = sys::lv_area_t { x1: p1.x, y1: p1.y.max(p2.y), x2: p2.x - 1, y2: co.y2 };
            if a.x2 >= a.x1 && a.y2 >= a.y1 {
                sys::lv_draw_rect((*base).layer, &rd, &a);
            }
        }
    }
}

/// Line chart without its own background; second series (c2 != 0) dashed, the first with a
/// low-opacity area fill.
#[allow(clippy::too_many_arguments)]
pub fn chart(p: Obj, x: i32, y: i32, w: i32, h: i32, points: u32, c1: u32, c2: u32) -> (Obj, Series, Series) {
    use sys::{lv_chart_type_t, lv_chart_update_mode_t};
    let ch = Obj::chart(p);
    ch.set_pos(x, y);
    ch.set_size(w, h);
    ch.remove_flag(lv::LV_OBJ_FLAG_CLICKABLE);
    ch.remove_flag(lv::LV_OBJ_FLAG_SCROLLABLE);
    ch.set_style_bg_opa(lv::OPA_TRANSP, 0);
    ch.set_style_border_width(0, 0);
    ch.set_style_pad_all(0, 0);
    ch.set_style_radius(0, 0);
    ch.set_style_line_color(t().sep, MAIN);
    ch.set_style_line_width(1, MAIN);
    ch.chart_set_div_line_count(3, 0);
    ch.set_style_size(0, 0, lv::LV_PART_INDICATOR);
    ch.set_style_line_width(2, lv::LV_PART_ITEMS);
    ch.chart_set_type(lv_chart_type_t::LV_CHART_TYPE_LINE);
    ch.chart_set_point_count(points);
    ch.chart_set_update_mode(lv_chart_update_mode_t::LV_CHART_UPDATE_MODE_SHIFT);
    ch.chart_set_axis_range(sys::lv_chart_axis_t::LV_CHART_AXIS_PRIMARY_Y, 0, 100);
    ch.add_event_cb(chart_fill_cb, lv::LV_EVENT_DRAW_TASK_ADDED, 0);
    ch.add_flag(lv::LV_OBJ_FLAG_SEND_DRAW_TASK_EVENTS);
    let s1 = ch.chart_add_series(c1);
    let s2 = if c2 != 0 { ch.chart_add_series(c2) } else { Series::default() };
    ch.chart_set_all_values(s1, lv::CHART_POINT_NONE);
    if !s2.0.is_null() {
        ch.chart_set_all_values(s2, lv::CHART_POINT_NONE);
    }
    (ch, s1, s2)
}

// ---------------------------------------------------------------- scrolling
/// Scroll body of exactly the visible area; content_h fixes the scroll range (a 1 px spacer),
/// so reflowing cards never moves it under the finger.
pub fn scroll(p: Obj, y: i32, view_h: i32, content_h: i32) -> Obj {
    let sb = lv::LV_PART_SCROLLBAR;
    let sc = Obj::create(p);
    sc.remove_style_all();
    sc.set_pos(0, y);
    sc.set_size(W, view_h);
    sc.set_scroll_dir(sys::lv_dir_t::LV_DIR_VER);
    sc.set_scrollbar_mode(sys::lv_scrollbar_mode_t::LV_SCROLLBAR_MODE_ACTIVE);
    sc.set_style_bg_color(t().t3, sb);
    sc.set_style_bg_opa(110, sb);
    sc.set_style_width(3, sb);
    sc.set_style_radius(2, sb);
    sc.set_style_pad_right(2, sb);
    let sp = Obj::create(sc); // child 0: fixes the scroll range
    sp.remove_style_all();
    sp.remove_flag(lv::LV_OBJ_FLAG_CLICKABLE);
    sp.set_size(1, 1);
    sp.set_pos(0, content_h - 1);
    sc
}

pub fn scroll_extent(sc: Obj, content_h: i32) {
    let sp = sc.child(0);
    if !sp.is_null() {
        sp.set_y(content_h - 1);
    }
}

// ---------------------------------------------------------------- animation
unsafe extern "C" fn anim_tx(o: *mut std::ffi::c_void, v: i32) {
    Obj(o.cast()).set_style_translate_x(v, 0);
}
unsafe extern "C" fn anim_ty(o: *mut std::ffi::c_void, v: i32) {
    Obj(o.cast()).set_style_translate_y(v, 0);
}

fn anim_run(o: Obj, cb: unsafe extern "C" fn(*mut std::ffi::c_void, i32), from: i32, ms: u32) {
    if !ANIM {
        // SAFETY: o is live
        unsafe { cb(o.0.cast(), 0) };
        return;
    }
    let mut a = MaybeUninit::<sys::lv_anim_t>::zeroed();
    // SAFETY: lv_anim_init fills the descriptor; lv_anim_start copies it
    unsafe {
        sys::lv_anim_init(a.as_mut_ptr());
        let a = a.as_mut_ptr();
        sys::lv_anim_set_var(a, o.0.cast());
        sys::lv_anim_set_exec_cb(a, Some(cb));
        sys::lv_anim_set_values(a, from, 0);
        sys::lv_anim_set_duration(a, ms);
        sys::lv_anim_set_path_cb(a, Some(sys::lv_anim_path_ease_out));
        sys::lv_anim_start(a);
    }
}

/// Slide o in from the right (150 ms), ease-out.
pub fn anim_push(o: Obj) {
    anim_run(o, anim_tx, W, 150);
}

/// Slide o in from below (180 ms), ease-out.
pub fn anim_rise(o: Obj, from_dy: i32) {
    anim_run(o, anim_ty, from_dy, 180);
}

// ---------------------------------------------------------------- sheet
fn glass(p: Obj, x: i32, y: i32, w: i32, h: i32, r: i32) -> Obj {
    let g = bx(p, x, y, w, h, t().glass, r);
    g.set_style_border_width(1, 0);
    g.set_style_border_color(0x000000, 0);
    g.set_style_border_opa(t().rim_opa, 0);
    g
}

#[derive(Default, Clone, Copy)]
pub struct Sheet {
    pub scrim: Obj,
    pub panel: Obj,
    pub cancel: Obj,
    pub cancel_l: Obj,
    pub panel_h: i32,
}

/// On lv_layer_top(); hidden until sheet_show. cancel_cb closes it.
pub fn sheet(panel_h: i32, cancel_cb: Option<EventCb>) -> Sheet {
    let top = Obj::layer_top();
    let scrim = bx(top, 0, 0, W, H, 0x000000, 0);
    scrim.set_style_bg_opa(t().scrim_opa, 0);
    scrim.add_flag(lv::LV_OBJ_FLAG_CLICKABLE); // swallows taps; tapping it cancels
    if let Some(cb) = cancel_cb {
        scrim.add_event_cb(cb, lv::LV_EVENT_CLICKED, 0);
    }
    let panel = glass(scrim, 8, H - 8 - 46 - 8 - panel_h, W - 16, panel_h, R_SHEET);
    panel.add_flag(lv::LV_OBJ_FLAG_CLICKABLE); // taps between items don't reach the scrim
    let cancel = glass(scrim, 8, H - 8 - 46, W - 16, 46, R_SHEET);
    let cancel_l = label(cancel, uf().cj17b, t().acc_t, 0, 0, tr("取消"));
    cancel_l.center();
    tappable(cancel, cancel_cb, 0);
    show(scrim, false);
    Sheet { scrim, panel, cancel, cancel_l, panel_h }
}

pub fn sheet_visible(s: &Sheet) -> bool {
    !s.scrim.is_null() && !s.scrim.hidden()
}

pub fn sheet_show(s: &Sheet, on: bool) {
    let was = sheet_visible(s);
    show(s.scrim, on);
    if on && !was {
        s.scrim.move_foreground();
        anim_rise(s.panel, s.panel_h + 62);
        anim_rise(s.cancel, s.panel_h + 62);
    }
}

/// A full-width option line in a sheet at y; returns (its label, its row).
pub fn sheet_item(s: &Sheet, y: i32, text: &str, col: u32, cb: Option<EventCb>, user: usize) -> (Obj, Obj) {
    let w = W - 16;
    let r = bx(s.panel, 0, y, w, 40, t().glass, 0);
    r.set_style_bg_opa(lv::OPA_TRANSP, 0);
    r.set_style_bg_opa(lv::OPA_COVER, lv::LV_STATE_PRESSED);
    r.set_style_bg_color(t().track, lv::LV_STATE_PRESSED);
    r.add_flag(lv::LV_OBJ_FLAG_CLICKABLE);
    if let Some(cb) = cb {
        r.add_event_cb(cb, lv::LV_EVENT_CLICKED, user);
    }
    bx(s.panel, 12, y, w - 24, 1, t().sep, 0);
    let l = label(r, uf().cj15, col, 0, 0, text);
    l.center();
    (l, r)
}
