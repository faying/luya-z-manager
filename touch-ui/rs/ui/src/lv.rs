//! A thin layer over lvgl-sys: object and font handles, and the calls the pages make, named as
//! in LVGL.
//!
//! Safety: LVGL runs on one thread; an [`Obj`] is a live LVGL object (pages never delete
//! objects: everything is built once and shown/hidden), a [`Font`] lives for the process.
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
#![allow(clippy::missing_safety_doc)]

pub use lvgl_sys as sys;
use std::ffi::c_void;
use sys::*;

pub const OPA_TRANSP: u8 = 0;
pub const OPA_50: u8 = 127;
pub const OPA_COVER: u8 = 255;
pub const COORD_MAX: i32 = LV_COORD_MAX as i32;
pub const COORD_TYPE_SPEC: i32 = LV_COORD_TYPE_SPEC as i32;
pub const SIZE_CONTENT: i32 = COORD_MAX | COORD_TYPE_SPEC;
pub const RADIUS_CIRCLE: i32 = LV_RADIUS_CIRCLE as i32;
pub const CHART_POINT_NONE: i32 = LV_CHART_POINT_NONE as i32;
pub const DRAW_LINE_POINT_NONE: i32 = LV_DRAW_LINE_POINT_NONE as i32;

pub fn coord_is_spec(v: i32) -> bool {
    (v & (3 << 29)) == COORD_TYPE_SPEC
}

unsafe extern "C" {
    static lv_label_class: lv_obj_class_t;
    static lv_font_montserrat_12: lv_font_t;
    static lv_font_montserrat_14: lv_font_t;
    static lv_font_montserrat_16: lv_font_t;
    static lv_font_montserrat_20: lv_font_t;
}

/// The built-in Montserrat (Latin only) at 12, 14, 16 or 20 px: the last fallback.
pub fn montserrat(size: i32) -> Font {
    // LVGL's constant font descriptors (lv_conf.h LV_FONT_MONTSERRAT_*)
    Font(match size {
        12 => &raw const lv_font_montserrat_12,
        14 => &raw const lv_font_montserrat_14,
        16 => &raw const lv_font_montserrat_16,
        _ => &raw const lv_font_montserrat_20,
    })
}

pub use sys::lv_align_t::{
    LV_ALIGN_CENTER, LV_ALIGN_LEFT_MID, LV_ALIGN_RIGHT_MID, LV_ALIGN_TOP_LEFT, LV_ALIGN_TOP_MID, LV_ALIGN_TOP_RIGHT,
};
pub use sys::lv_border_side_t::{LV_BORDER_SIDE_BOTTOM, LV_BORDER_SIDE_TOP};
pub use sys::lv_event_code_t::{
    LV_EVENT_CLICKED, LV_EVENT_DRAW_TASK_ADDED, LV_EVENT_LONG_PRESSED, LV_EVENT_PRESSED, LV_EVENT_RELEASED, LV_EVENT_VALUE_CHANGED,
};
pub use sys::lv_label_long_mode_t::{LV_LABEL_LONG_MODE_CLIP, LV_LABEL_LONG_MODE_DOTS, LV_LABEL_LONG_MODE_WRAP};
pub use sys::lv_obj_flag_t::{
    LV_OBJ_FLAG_CLICKABLE, LV_OBJ_FLAG_HIDDEN, LV_OBJ_FLAG_SCROLLABLE, LV_OBJ_FLAG_SCROLL_ON_FOCUS, LV_OBJ_FLAG_SEND_DRAW_TASK_EVENTS,
};
pub use sys::lv_part_t::{LV_PART_INDICATOR, LV_PART_ITEMS, LV_PART_KNOB, LV_PART_MAIN, LV_PART_SCROLLBAR};
pub use sys::lv_state_t::{LV_STATE_CHECKED, LV_STATE_DISABLED, LV_STATE_PRESSED};
pub use sys::lv_text_align_t::{LV_TEXT_ALIGN_CENTER, LV_TEXT_ALIGN_LEFT, LV_TEXT_ALIGN_RIGHT};

pub type Align = lv_align_t::Type;
pub type EventCb = unsafe extern "C" fn(*mut lv_event_t);

pub fn color(rgb: u32) -> lv_color_t {
    lv_color_t { blue: (rgb & 0xff) as u8, green: ((rgb >> 8) & 0xff) as u8, red: ((rgb >> 16) & 0xff) as u8 }
}

/// NUL-terminated copy of a text for LVGL (which copies it again).
pub fn cstr(t: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(t.len() + 1);
    v.extend(t.iter().copied().filter(|&c| c != 0));
    v.push(0);
    v
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Font(pub *const lv_font_t);

// SAFETY: font descriptors are created once at startup and never change or go away; the UI
// uses them from one thread
unsafe impl Send for Font {}
unsafe impl Sync for Font {}

impl Font {
    pub const NULL: Font = Font(std::ptr::null());
    pub fn is_null(self) -> bool {
        self.0.is_null()
    }
    pub fn line_height(self) -> i32 {
        unsafe { lv_font_get_line_height(self.0) }
    }
    /// The font's raw pointer for struct field reads (line_height, base_line).
    pub fn raw(self) -> &'static lv_font_t {
        unsafe { &*self.0 }
    }
    pub fn glyph(self, letter: u32, next: u32) -> Option<lv_font_glyph_dsc_t> {
        let mut g = std::mem::MaybeUninit::<lv_font_glyph_dsc_t>::zeroed();
        unsafe { lv_font_get_glyph_dsc(self.0, g.as_mut_ptr(), letter, next).then(|| g.assume_init()) }
    }
}

/// The size of a text in a font, one line unless max_w wraps it (lv_text_get_size).
pub fn text_size(t: &[u8], f: Font, max_w: i32) -> (i32, i32) {
    let c = cstr(t);
    let mut p = lv_point_t { x: 0, y: 0 };
    unsafe { lv_text_get_size(&mut p, c.as_ptr().cast(), f.0, 0, 0, max_w, lv_text_flag_t::LV_TEXT_FLAG_NONE) };
    (p.x, p.y)
}

pub fn text_w(t: &[u8], f: Font) -> i32 {
    text_size(t, f, COORD_MAX).0
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Obj(pub *mut lv_obj_t);

impl Default for Obj {
    fn default() -> Self {
        Obj::NULL
    }
}

macro_rules! style_setters {
    ($($name:ident: $f:ident($t:ty) = $prop:ident),* $(,)?) => {$(
        pub fn $name(self, v: $t, sel: u32) {
            if self.local_num(STYLE::$prop, sel) == Some(v as i32) {
                return;
            }
            unsafe { $f(self.0, v as _, sel) }
        }
    )*};
}

/// LVGL's style property ids (lv_style.h _lv_style_id_t, LVGL 9.5; bindgen leaves the enum out).
/// Setting a local style to the value it has still invalidates the object: the 1 s refresh sets
/// every colour and size each time, so the setters compare first (u60-touch-rewrite.md §7).
#[allow(non_snake_case)]
mod STYLE {
    pub const WIDTH: u8 = 1;
    pub const MAX_WIDTH: u8 = 9;
    pub const TRANSLATE_X: u8 = 12;
    pub const TRANSLATE_Y: u8 = 13;
    pub const X: u8 = 16;
    pub const Y: u8 = 17;
    pub const ALIGN: u8 = 18;
    pub const PAD_TOP: u8 = 24;
    pub const PAD_BOTTOM: u8 = 25;
    pub const PAD_LEFT: u8 = 26;
    pub const PAD_RIGHT: u8 = 27;
    pub const PAD_ROW: u8 = 29;
    pub const BORDER_WIDTH: u8 = 56;
    pub const BORDER_COLOR: u8 = 57;
    pub const BORDER_OPA: u8 = 58;
    pub const BORDER_SIDE: u8 = 60;
    pub const BG_OPA: u8 = 72;
    pub const BG_COLOR: u8 = 73;
    pub const SHADOW_WIDTH: u8 = 74;
    pub const LINE_WIDTH: u8 = 75;
    pub const TEXT_FONT: u8 = 77;
    pub const IMAGE_OPA: u8 = 80;
    pub const SHADOW_OPA: u8 = 81;
    pub const SHADOW_COLOR: u8 = 88;
    pub const LINE_COLOR: u8 = 90;
    pub const TEXT_COLOR: u8 = 92;
    pub const SHADOW_OFFSET_Y: u8 = 98;
    pub const TEXT_ALIGN: u8 = 101;
    pub const TEXT_LINE_SPACE: u8 = 103;
    pub const OPA: u8 = 112;
    pub const COLOR_FILTER_OPA: u8 = 115;
    pub const ANIM_DURATION: u8 = 117;
    pub const RADIUS: u8 = 120;
    pub const CLIP_CORNER: u8 = 128;
}

impl Obj {
    pub const NULL: Obj = Obj(std::ptr::null_mut());

    pub fn is_null(self) -> bool {
        self.0.is_null()
    }
    pub fn screen_active() -> Obj {
        Obj(unsafe { lv_screen_active() })
    }
    pub fn layer_top() -> Obj {
        Obj(unsafe { lv_layer_top() })
    }
    pub fn create(parent: Obj) -> Obj {
        Obj(unsafe { lv_obj_create(parent.0) })
    }
    pub fn label(parent: Obj) -> Obj {
        Obj(unsafe { lv_label_create(parent.0) })
    }
    pub fn image(parent: Obj) -> Obj {
        Obj(unsafe { lv_image_create(parent.0) })
    }
    pub fn chart(parent: Obj) -> Obj {
        Obj(unsafe { lv_chart_create(parent.0) })
    }
    pub fn switch(parent: Obj) -> Obj {
        Obj(unsafe { lv_switch_create(parent.0) })
    }
    pub fn slider(parent: Obj) -> Obj {
        Obj(unsafe { lv_slider_create(parent.0) })
    }
    pub fn tileview(parent: Obj) -> Obj {
        Obj(unsafe { lv_tileview_create(parent.0) })
    }
    pub fn add_tile(self, col: u8, row: u8, dir: lv_dir_t::Type) -> Obj {
        Obj(unsafe { lv_tileview_add_tile(self.0, col, row, dir) })
    }
    pub fn set_tile_by_index(self, col: u32, row: u32) {
        unsafe { lv_tileview_set_tile_by_index(self.0, col, row, false) }
    }
    pub fn tile_active(self) -> Obj {
        Obj(unsafe { lv_tileview_get_tile_active(self.0) })
    }

    fn local(self, prop: u8, sel: u32) -> Option<lv_style_value_t> {
        let mut v = lv_style_value_t { num: 0 };
        let r = unsafe { lv_obj_get_local_style_prop(self.0, prop as lv_style_prop_t, &mut v, sel) };
        (r == lv_style_res_t::LV_STYLE_RES_FOUND).then_some(v)
    }
    fn local_num(self, prop: u8, sel: u32) -> Option<i32> {
        // SAFETY: the number props are stored in `num`
        self.local(prop, sel).map(|v| unsafe { v.num })
    }
    fn local_ptr(self, prop: u8, sel: u32) -> Option<*const c_void> {
        // SAFETY: TEXT_FONT is stored in `ptr`
        self.local(prop, sel).map(|v| unsafe { v.ptr })
    }
    fn local_color(self, prop: u8, sel: u32) -> Option<u32> {
        // SAFETY: the colour props are stored in `color`
        self.local(prop, sel).map(|v| unsafe { v.color }).map(|c| (u32::from(c.red) << 16) | (u32::from(c.green) << 8) | u32::from(c.blue))
    }

    pub fn remove_style_all(self) {
        unsafe { lv_obj_remove_style_all(self.0) }
    }
    pub fn add_flag(self, f: lv_obj_flag_t::Type) {
        // a flag set again still invalidates (HIDDEN)
        if !self.has_flag(f) {
            unsafe { lv_obj_add_flag(self.0, f) }
        }
    }
    pub fn remove_flag(self, f: lv_obj_flag_t::Type) {
        // a flag set again still invalidates (HIDDEN)
        if self.has_flag(f) {
            unsafe { lv_obj_remove_flag(self.0, f) }
        }
    }
    pub fn has_flag(self, f: lv_obj_flag_t::Type) -> bool {
        unsafe { lv_obj_has_flag(self.0, f) }
    }
    pub fn hidden(self) -> bool {
        self.has_flag(LV_OBJ_FLAG_HIDDEN)
    }
    pub fn add_state(self, s: lv_state_t::Type) {
        if !self.has_state(s) {
            unsafe { lv_obj_add_state(self.0, s) }
        }
    }
    pub fn remove_state(self, s: lv_state_t::Type) {
        if self.has_state(s) {
            unsafe { lv_obj_remove_state(self.0, s) }
        }
    }
    pub fn has_state(self, s: lv_state_t::Type) -> bool {
        unsafe { lv_obj_has_state(self.0, s) }
    }

    pub fn set_pos(self, x: i32, y: i32) {
        unsafe { lv_obj_set_pos(self.0, x, y) }
    }
    pub fn set_x(self, x: i32) {
        unsafe { lv_obj_set_x(self.0, x) }
    }
    pub fn set_y(self, y: i32) {
        unsafe { lv_obj_set_y(self.0, y) }
    }
    pub fn set_size(self, w: i32, h: i32) {
        unsafe { lv_obj_set_size(self.0, w, h) }
    }
    pub fn set_width(self, w: i32) {
        unsafe { lv_obj_set_width(self.0, w) }
    }
    pub fn set_height(self, h: i32) {
        unsafe { lv_obj_set_height(self.0, h) }
    }
    pub fn align(self, a: Align, x: i32, y: i32) {
        // lv_obj_align sets the align style unconditionally (lv_obj_set_pos compares)
        if self.local_num(STYLE::ALIGN, 0) == Some(a as i32) && self.local_num(STYLE::X, 0) == Some(x) && self.local_num(STYLE::Y, 0) == Some(y) {
            return;
        }
        unsafe { lv_obj_align(self.0, a, x, y) }
    }
    pub fn center(self) {
        unsafe { lv_obj_center(self.0) }
    }
    pub fn x(self) -> i32 {
        unsafe { lv_obj_get_x(self.0) }
    }
    pub fn y(self) -> i32 {
        unsafe { lv_obj_get_y(self.0) }
    }
    pub fn width(self) -> i32 {
        unsafe { lv_obj_get_width(self.0) }
    }
    pub fn height(self) -> i32 {
        unsafe { lv_obj_get_height(self.0) }
    }
    pub fn style_width(self, part: u32) -> i32 {
        unsafe { lv_obj_get_style_width(self.0, part) }
    }
    pub fn style_height(self, part: u32) -> i32 {
        unsafe { lv_obj_get_style_height(self.0, part) }
    }
    pub fn style_x(self, part: u32) -> i32 {
        unsafe { lv_obj_get_style_x(self.0, part) }
    }
    pub fn style_y(self, part: u32) -> i32 {
        unsafe { lv_obj_get_style_y(self.0, part) }
    }
    pub fn style_bg_opa(self, part: u32) -> u8 {
        unsafe { lv_obj_get_style_bg_opa(self.0, part) }
    }
    pub fn style_text_font(self, part: u32) -> Font {
        Font(unsafe { lv_obj_get_style_text_font(self.0, part) })
    }
    pub fn update_layout(self) {
        unsafe { lv_obj_update_layout(self.0) }
    }
    pub fn coords(self) -> lv_area_t {
        let mut a = lv_area_t { x1: 0, y1: 0, x2: 0, y2: 0 };
        unsafe { lv_obj_get_coords(self.0, &mut a) };
        a
    }
    pub fn parent(self) -> Obj {
        Obj(unsafe { lv_obj_get_parent(self.0) })
    }
    pub fn set_parent(self, p: Obj) {
        unsafe { lv_obj_set_parent(self.0, p.0) }
    }
    pub fn child(self, i: i32) -> Obj {
        Obj(unsafe { lv_obj_get_child(self.0, i) })
    }
    pub fn child_count(self) -> u32 {
        unsafe { lv_obj_get_child_count(self.0) }
    }
    pub fn move_foreground(self) {
        unsafe { lv_obj_move_foreground(self.0) }
    }
    pub fn invalidate(self) {
        unsafe { lv_obj_invalidate(self.0) };
    }
    pub fn set_ext_click_area(self, s: i32) {
        unsafe { lv_obj_set_ext_click_area(self.0, s) }
    }
    pub fn add_event_cb(self, cb: EventCb, code: lv_event_code_t::Type, user: usize) {
        unsafe { lv_obj_add_event_cb(self.0, Some(cb), code, user as *mut c_void) };
    }
    pub fn send_event(self, code: lv_event_code_t::Type) {
        unsafe { lv_obj_send_event(self.0, code, std::ptr::null_mut()) };
    }
    pub fn check_type_label(self) -> bool {
        unsafe { lv_obj_check_type(self.0, &raw const lv_label_class) }
    }

    // scrolling
    pub fn set_scroll_dir(self, d: lv_dir_t::Type) {
        unsafe { lv_obj_set_scroll_dir(self.0, d) }
    }
    pub fn scroll_dir(self) -> lv_dir_t::Type {
        unsafe { lv_obj_get_scroll_dir(self.0) }
    }
    pub fn set_scrollbar_mode(self, m: lv_scrollbar_mode_t::Type) {
        unsafe { lv_obj_set_scrollbar_mode(self.0, m) }
    }
    pub fn scroll_to_y(self, y: i32) {
        unsafe { lv_obj_scroll_to_y(self.0, y, false) }
    }
    pub fn scroll_y(self) -> i32 {
        unsafe { lv_obj_get_scroll_y(self.0) }
    }
    pub fn scroll_bottom(self) -> i32 {
        unsafe { lv_obj_get_scroll_bottom(self.0) }
    }

    style_setters! {
        set_style_radius: lv_obj_set_style_radius(i32) = RADIUS,
        set_style_bg_opa: lv_obj_set_style_bg_opa(u8) = BG_OPA,
        set_style_border_width: lv_obj_set_style_border_width(i32) = BORDER_WIDTH,
        set_style_border_opa: lv_obj_set_style_border_opa(u8) = BORDER_OPA,
        set_style_border_side: lv_obj_set_style_border_side(lv_border_side_t::Type) = BORDER_SIDE,
        set_style_shadow_width: lv_obj_set_style_shadow_width(i32) = SHADOW_WIDTH,
        set_style_shadow_offset_y: lv_obj_set_style_shadow_offset_y(i32) = SHADOW_OFFSET_Y,
        set_style_shadow_opa: lv_obj_set_style_shadow_opa(u8) = SHADOW_OPA,
        set_style_pad_left: lv_obj_set_style_pad_left(i32) = PAD_LEFT,
        set_style_pad_right: lv_obj_set_style_pad_right(i32) = PAD_RIGHT,
        set_style_pad_top: lv_obj_set_style_pad_top(i32) = PAD_TOP,
        set_style_pad_bottom: lv_obj_set_style_pad_bottom(i32) = PAD_BOTTOM,
        set_style_width: lv_obj_set_style_width(i32) = WIDTH,
        set_style_max_width: lv_obj_set_style_max_width(i32) = MAX_WIDTH,
        set_style_line_width: lv_obj_set_style_line_width(i32) = LINE_WIDTH,
        set_style_text_align: lv_obj_set_style_text_align(lv_text_align_t::Type) = TEXT_ALIGN,
        set_style_image_opa: lv_obj_set_style_image_opa(u8) = IMAGE_OPA,
        set_style_anim_duration: lv_obj_set_style_anim_duration(u32) = ANIM_DURATION,
        set_style_color_filter_opa: lv_obj_set_style_color_filter_opa(u8) = COLOR_FILTER_OPA,
        set_style_translate_x: lv_obj_set_style_translate_x(i32) = TRANSLATE_X,
        set_style_translate_y: lv_obj_set_style_translate_y(i32) = TRANSLATE_Y,
        set_style_opa: lv_obj_set_style_opa(u8) = OPA,
        set_style_pad_row: lv_obj_set_style_pad_row(i32) = PAD_ROW,
        set_style_text_line_space: lv_obj_set_style_text_line_space(i32) = TEXT_LINE_SPACE,
    }
    pub fn set_style_pad_all(self, v: i32, sel: u32) {
        unsafe { lv_obj_set_style_pad_all(self.0, v as _, sel) }
    }
    pub fn set_style_pad_hor(self, v: i32, sel: u32) {
        unsafe { lv_obj_set_style_pad_hor(self.0, v as _, sel) }
    }
    pub fn set_style_pad_ver(self, v: i32, sel: u32) {
        unsafe { lv_obj_set_style_pad_ver(self.0, v as _, sel) }
    }
    pub fn set_style_clip_corner(self, v: bool, sel: u32) {
        if self.local_num(STYLE::CLIP_CORNER, sel) == Some(i32::from(v)) {
            return;
        }
        unsafe { lv_obj_set_style_clip_corner(self.0, v, sel) }
    }
    pub fn set_style_bg_color(self, rgb: u32, sel: u32) {
        if self.local_color(STYLE::BG_COLOR, sel) == Some(rgb & 0xff_ffff) {
            return;
        }
        unsafe { lv_obj_set_style_bg_color(self.0, color(rgb), sel) }
    }
    pub fn set_style_text_color(self, rgb: u32, sel: u32) {
        if self.local_color(STYLE::TEXT_COLOR, sel) == Some(rgb & 0xff_ffff) {
            return;
        }
        unsafe { lv_obj_set_style_text_color(self.0, color(rgb), sel) }
    }
    pub fn set_style_border_color(self, rgb: u32, sel: u32) {
        if self.local_color(STYLE::BORDER_COLOR, sel) == Some(rgb & 0xff_ffff) {
            return;
        }
        unsafe { lv_obj_set_style_border_color(self.0, color(rgb), sel) }
    }
    pub fn set_style_shadow_color(self, rgb: u32, sel: u32) {
        if self.local_color(STYLE::SHADOW_COLOR, sel) == Some(rgb & 0xff_ffff) {
            return;
        }
        unsafe { lv_obj_set_style_shadow_color(self.0, color(rgb), sel) }
    }
    pub fn set_style_line_color(self, rgb: u32, sel: u32) {
        if self.local_color(STYLE::LINE_COLOR, sel) == Some(rgb & 0xff_ffff) {
            return;
        }
        unsafe { lv_obj_set_style_line_color(self.0, color(rgb), sel) }
    }
    pub fn set_style_text_font(self, f: Font, sel: u32) {
        if self.local_ptr(STYLE::TEXT_FONT, sel) == Some(f.0.cast()) {
            return;
        }
        unsafe { lv_obj_set_style_text_font(self.0, f.0, sel) }
    }
    pub fn set_style_size(self, w: i32, h: i32, sel: u32) {
        unsafe { lv_obj_set_style_size(self.0, w, h, sel) }
    }
    pub fn set_style_color_filter_dsc(self, d: &'static lv_color_filter_dsc_t, sel: u32) {
        unsafe { lv_obj_set_style_color_filter_dsc(self.0, d, sel) }
    }

    // labels
    pub fn set_text(self, t: impl AsRef<[u8]>) {
        // the same text again still re-lays the label out and invalidates it
        if self.text() == t.as_ref() {
            return;
        }
        let c = cstr(t.as_ref());
        unsafe { lv_label_set_text(self.0, c.as_ptr().cast()) }
    }
    pub fn text(self) -> Vec<u8> {
        unsafe {
            let p = lv_label_get_text(self.0);
            if p.is_null() { Vec::new() } else { std::ffi::CStr::from_ptr(p).to_bytes().to_vec() }
        }
    }
    pub fn set_long_mode(self, m: lv_label_long_mode_t::Type) {
        // the same mode again still re-lays the label out
        if unsafe { lv_label_get_long_mode(self.0) } == m {
            return;
        }
        unsafe { lv_label_set_long_mode(self.0, m) }
    }

    // sliders
    pub fn slider_set_range(self, lo: i32, hi: i32) {
        unsafe { lv_slider_set_range(self.0, lo, hi) }
    }
    pub fn slider_set_value(self, v: i32) {
        unsafe { lv_slider_set_value(self.0, v, false) }
    }
    pub fn slider_value(self) -> i32 {
        unsafe { lv_slider_get_value(self.0) }
    }

    /// A flex column (the detail pages): children top to bottom, centred across.
    pub fn flex_column(self) {
        unsafe {
            lv_obj_set_flex_flow(self.0, lv_flex_flow_t::LV_FLEX_FLOW_COLUMN);
        }
    }
    pub fn flex_align_start_center(self) {
        use lv_flex_align_t::*;
        unsafe { lv_obj_set_flex_align(self.0, LV_FLEX_ALIGN_START, LV_FLEX_ALIGN_CENTER, LV_FLEX_ALIGN_CENTER) }
    }
    pub fn is_visible(self) -> bool {
        unsafe { lv_obj_is_visible(self.0) }
    }

    /// Event handlers on this object (the harness: a tap on an object without one did nothing).
    pub fn event_count(self) -> u32 {
        unsafe { lv_obj_get_event_count(self.0) }
    }

    // images
    /// `src` is an LVGL file path ("A:/…"): LVGL keeps its own copy.
    pub fn set_image_src(self, src: &str) {
        let cur = unsafe { lv_image_get_src(self.0) };
        // SAFETY: this UI only ever gives images file paths, which LVGL keeps as C strings
        if !cur.is_null() && unsafe { std::ffi::CStr::from_ptr(cur.cast()) }.to_bytes() == src.as_bytes() {
            return;
        }
        let c = cstr(src.as_bytes());
        unsafe { lv_image_set_src(self.0, c.as_ptr().cast()) }
    }
}

/// The header of an image file ("A:/…"): (w, h), None if it cannot be read.
pub fn image_info(src: &str) -> Option<(u32, u32)> {
    let c = cstr(src.as_bytes());
    let mut h = std::mem::MaybeUninit::<lv_image_header_t>::zeroed();
    unsafe {
        (lv_image_decoder_get_info(c.as_ptr().cast(), h.as_mut_ptr()) == lv_result_t::LV_RESULT_OK).then(|| {
            let h = h.assume_init();
            (h.w(), h.h())
        })
    }
}

/// lv_pct(v)
pub fn pct(v: i32) -> i32 {
    unsafe { lv_pct(v) }
}

/// Render the display now (lv_refr_now(NULL)): a tap's answer before a slow call.
pub fn refr_now() {
    unsafe { lv_refr_now(std::ptr::null_mut()) }
}

pub fn inactive_ms() -> u32 {
    unsafe { lv_display_get_inactive_time(std::ptr::null()) }
}

/// A timer handle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Timer(pub *mut lv_timer_t);

impl Default for Timer {
    fn default() -> Self {
        Timer(std::ptr::null_mut())
    }
}

pub type TimerCb = unsafe extern "C" fn(*mut lv_timer_t);

impl Timer {
    pub fn create(cb: TimerCb, period: u32) -> Timer {
        Timer(unsafe { lv_timer_create(Some(cb), period, std::ptr::null_mut()) })
    }
    pub fn is_null(self) -> bool {
        self.0.is_null()
    }
    pub fn pause(self) {
        unsafe { lv_timer_pause(self.0) }
    }
    pub fn resume(self) {
        unsafe { lv_timer_resume(self.0) }
    }
    pub fn reset(self) {
        unsafe { lv_timer_reset(self.0) }
    }
    pub fn set_repeat_count(self, n: i32) {
        unsafe { lv_timer_set_repeat_count(self.0, n) }
    }
    pub fn set_period(self, ms: u32) {
        unsafe { lv_timer_set_period(self.0, ms) }
    }
    pub fn ready(self) {
        unsafe { lv_timer_ready(self.0) }
    }
}

/// lv_tick_elaps: ms since `prev` (wraps like C).
pub fn tick_elaps(prev: u32) -> u32 {
    unsafe { lv_tick_elaps(prev) }
}

/// Tell LVGL the user did something (resets the inactivity time).
pub fn trigger_activity() {
    unsafe { lv_display_trigger_activity(std::ptr::null_mut()) }
}

pub fn tick_get() -> u32 {
    unsafe { lv_tick_get() }
}

/// # Safety
/// `e` is the event LVGL passed to the callback.
pub unsafe fn event_user(e: *mut lv_event_t) -> usize {
    unsafe { lv_event_get_user_data(e) as usize }
}

/// The object an event was sent to.
///
/// # Safety
/// `e` is the event LVGL passed to the callback.
pub unsafe fn event_target(e: *mut lv_event_t) -> Obj {
    Obj(unsafe { lv_event_get_target(e) }.cast())
}

/// A chart series handle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Series(pub *mut lv_chart_series_t);

impl Default for Series {
    fn default() -> Self {
        Series(std::ptr::null_mut())
    }
}

impl Obj {
    pub fn chart_set_type(self, t: lv_chart_type_t::Type) {
        unsafe { lv_chart_set_type(self.0, t) }
    }
    pub fn chart_set_point_count(self, n: u32) {
        unsafe { lv_chart_set_point_count(self.0, n) }
    }
    pub fn chart_set_update_mode(self, m: lv_chart_update_mode_t::Type) {
        unsafe { lv_chart_set_update_mode(self.0, m) }
    }
    pub fn chart_set_axis_range(self, axis: lv_chart_axis_t::Type, lo: i32, hi: i32) {
        unsafe { lv_chart_set_axis_range(self.0, axis, lo, hi) }
    }
    pub fn chart_set_div_line_count(self, h: u32, v: u32) {
        unsafe { lv_chart_set_div_line_count(self.0, h, v) }
    }
    pub fn chart_add_series(self, rgb: u32) -> Series {
        Series(unsafe { lv_chart_add_series(self.0, color(rgb), lv_chart_axis_t::LV_CHART_AXIS_PRIMARY_Y) })
    }
    pub fn chart_set_all_values(self, s: Series, v: i32) {
        unsafe { lv_chart_set_all_values(self.0, s.0, v) }
    }
    pub fn chart_set_next_value(self, s: Series, v: i32) {
        unsafe { lv_chart_set_next_value(self.0, s.0, v) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// STYLE's ids are copied from lv_style.h: each setter must read back its own property,
    /// else a setter could skip a change it should make. One test: LVGL is not thread-safe.
    /// (Labels and flags need the real display: set_text, set_long_mode and the flags are covered
    /// by the render harness's --still runs.)
    #[test]
    fn style_ids_and_skips() {
        unsafe {
            lv_init();
            lv_display_create(16, 16);
        }
        let o = Obj(unsafe { lv_obj_create(std::ptr::null_mut()) });
        macro_rules! num {
            ($set:ident, $id:ident, $v:expr) => {
                o.$set($v, 0);
                assert_eq!(o.local_num(STYLE::$id, 0), Some($v as i32), stringify!($id));
            };
        }
        num!(set_style_width, WIDTH, 37);
        num!(set_style_max_width, MAX_WIDTH, 38);
        num!(set_style_translate_x, TRANSLATE_X, 39);
        num!(set_style_translate_y, TRANSLATE_Y, 40);
        num!(set_style_pad_top, PAD_TOP, 41);
        num!(set_style_pad_bottom, PAD_BOTTOM, 42);
        num!(set_style_pad_left, PAD_LEFT, 43);
        num!(set_style_pad_right, PAD_RIGHT, 44);
        num!(set_style_pad_row, PAD_ROW, 45);
        num!(set_style_border_width, BORDER_WIDTH, 46);
        num!(set_style_border_opa, BORDER_OPA, 47u8);
        num!(set_style_border_side, BORDER_SIDE, LV_BORDER_SIDE_TOP);
        num!(set_style_bg_opa, BG_OPA, 48u8);
        num!(set_style_shadow_width, SHADOW_WIDTH, 49);
        num!(set_style_line_width, LINE_WIDTH, 50);
        num!(set_style_image_opa, IMAGE_OPA, 51u8);
        num!(set_style_shadow_opa, SHADOW_OPA, 52u8);
        num!(set_style_shadow_offset_y, SHADOW_OFFSET_Y, 53);
        num!(set_style_text_align, TEXT_ALIGN, LV_TEXT_ALIGN_RIGHT);
        num!(set_style_text_line_space, TEXT_LINE_SPACE, 54);
        num!(set_style_opa, OPA, 55u8);
        num!(set_style_color_filter_opa, COLOR_FILTER_OPA, 56u8);
        num!(set_style_anim_duration, ANIM_DURATION, 57u32);
        num!(set_style_radius, RADIUS, 58);
        o.set_style_clip_corner(true, 0);
        assert_eq!(o.local_num(STYLE::CLIP_CORNER, 0), Some(1));
        macro_rules! col {
            ($set:ident, $id:ident, $v:expr) => {
                o.$set($v, 0);
                assert_eq!(o.local_color(STYLE::$id, 0), Some($v), stringify!($id));
            };
        }
        col!(set_style_bg_color, BG_COLOR, 0x123456);
        col!(set_style_text_color, TEXT_COLOR, 0x234567);
        col!(set_style_border_color, BORDER_COLOR, 0x345678);
        col!(set_style_shadow_color, SHADOW_COLOR, 0x456789);
        col!(set_style_line_color, LINE_COLOR, 0x56789a);
        o.align(LV_ALIGN_TOP_RIGHT, 3, -4);
        assert_eq!((o.local_num(STYLE::ALIGN, 0), o.local_num(STYLE::X, 0), o.local_num(STYLE::Y, 0)), (Some(LV_ALIGN_TOP_RIGHT as i32), Some(3), Some(-4)));
        let f = Font(unsafe { lv_obj_get_style_text_font(o.0, 0) });
        o.set_style_text_font(f, 0);
        assert_eq!(o.local_ptr(STYLE::TEXT_FONT, 0), Some(f.0.cast()));

        // a value set again is skipped; a different one is not
        o.set_style_text_color(0x234567, LV_PART_MAIN | LV_STATE_PRESSED);
        assert_eq!(o.local_color(STYLE::TEXT_COLOR, LV_PART_MAIN | LV_STATE_PRESSED), Some(0x234567));
        assert_eq!(o.local_color(STYLE::TEXT_COLOR, 0), Some(0x234567));
        o.set_style_text_color(0x000001, 0);
        assert_eq!(o.local_color(STYLE::TEXT_COLOR, 0), Some(0x000001));
    }
}
