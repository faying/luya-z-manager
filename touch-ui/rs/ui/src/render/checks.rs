//! The render test's cheap checks, line for line, so the Rust
//! screen gets the same pass/fail lines as the C one:
//!   - expect*.txt: every line that applies to this scene must be in a visible label of its page;
//!   - missing glyphs: characters a label shows that its font chain cannot draw;
//!   - geometry: text off the screen or out of its container, text on text, text running into a
//!     control; DOT/CLIP labels whose text is wider than the box are counted as truncated.
//!
//! Messages are bytes, cut like C's `%.60s` (a byte count, not characters).
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use crate::lv::{self, sys, Obj};
use std::ffi::c_char;
use sys::lv_area_t;

const W: i32 = 320;
const H: i32 = 480;
const MAX_TEXTS: usize = 1024;
const MAX_MISSING: usize = 32;
const MAX_GEO: usize = 32;
const MAX_EXPECT: usize = 1024;

unsafe extern "C" {
    static lv_label_class: sys::lv_obj_class_t;
    static lv_switch_class: sys::lv_obj_class_t;
    static lv_slider_class: sys::lv_obj_class_t;
    // private headers (misc/lv_text_private.h, misc/lv_area_private.h): not in the bindings
    static lv_text_encoded_next: unsafe extern "C" fn(*const c_char, *mut u32) -> u32;
    fn lv_area_intersect(res: *mut lv_area_t, a1: *const lv_area_t, a2: *const lv_area_t) -> bool;
}

const PART_MAIN: u32 = sys::lv_part_t::LV_PART_MAIN;

fn is_type(o: Obj, c: *const sys::lv_obj_class_t) -> bool {
    // SAFETY: o is a live object
    unsafe { sys::lv_obj_check_type(o.0, c) }
}
pub(super) fn is_label(o: Obj) -> bool {
    is_type(o, &raw const lv_label_class)
}
fn font(o: Obj) -> lv::Font {
    o.style_text_font(PART_MAIN)
}
fn style_opa(o: Obj) -> u8 {
    // SAFETY: as above
    unsafe { sys::lv_obj_get_style_opa(o.0, PART_MAIN) }
}
fn text_opa(o: Obj) -> u8 {
    // SAFETY: as above
    unsafe { sys::lv_obj_get_style_text_opa(o.0, PART_MAIN) }
}
fn letter_space(o: Obj) -> i32 {
    // SAFETY: as above
    unsafe { sys::lv_obj_get_style_text_letter_space(o.0, PART_MAIN) }
}
fn line_space(o: Obj) -> i32 {
    // SAFETY: as above
    unsafe { sys::lv_obj_get_style_text_line_space(o.0, PART_MAIN) }
}
fn long_mode(o: Obj) -> sys::lv_label_long_mode_t::Type {
    // SAFETY: o is a label
    unsafe { sys::lv_label_get_long_mode(o.0) }
}
fn content_coords(o: Obj) -> lv_area_t {
    let mut a = lv_area_t { x1: 0, y1: 0, x2: 0, y2: 0 };
    // SAFETY: as above
    unsafe { sys::lv_obj_get_content_coords(o.0, &mut a) };
    a
}
fn aw(a: &lv_area_t) -> i32 {
    a.x2 - a.x1 + 1
}
fn ah(a: &lv_area_t) -> i32 {
    a.y2 - a.y1 + 1
}
fn intersect(a: &lv_area_t, b: &lv_area_t) -> Option<lv_area_t> {
    let mut x = lv_area_t { x1: 0, y1: 0, x2: 0, y2: 0 };
    // SAFETY: plain structs
    unsafe { lv_area_intersect(&mut x, a, b) }.then_some(x)
}
/// lv_text_get_size with the label's spacing
fn text_size(t: &[u8], o: Obj, max_w: i32) -> (i32, i32) {
    let c = lv::cstr(t);
    let mut p = sys::lv_point_t { x: 0, y: 0 };
    // SAFETY: c is NUL-terminated; the font lives for the process
    unsafe {
        sys::lv_text_get_size(&mut p, c.as_ptr().cast(), font(o).0, letter_space(o), line_space(o), max_w, sys::lv_text_flag_t::LV_TEXT_FLAG_NONE)
    };
    (p.x, p.y)
}

/// C's "%.Ns": at most n bytes
fn cut(t: &[u8], n: usize) -> &[u8] {
    &t[..t.len().min(n)]
}

fn glyph_exempt(c: u32) -> bool {
    c < 0x20 || c == 0x200D || (0xFE00..=0xFE0F).contains(&c) || c >= 0x1F000
}

#[derive(Default)]
struct Expect {
    page: Vec<u8>,
    text: Vec<u8>,
}

/// The checks' state across one render run.
#[derive(Default)]
pub struct Checks {
    pub texts: Vec<Vec<u8>>,
    missing: Vec<u32>,
    geo: Vec<Vec<u8>>,
    pub ntrunc: i32,
    exp: Vec<Expect>,
    /// --dump: TEXT and TRUNC lines on stdout
    pub dump: bool,
    /// this page's results: passes and failure messages (without the "  FAIL [..] " prefix)
    pub pass: i32,
    pub fails: Vec<Vec<u8>>,
}

impl Checks {
    /// expect.txt lines "<scenes>\t<page>\t<text>" that apply to `scene`
    pub fn load_expect(&mut self, path: &str, scene: &str) -> Result<(), String> {
        let data = std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))?;
        for line in data.split(|&b| b == b'\n') {
            if self.exp.len() >= MAX_EXPECT {
                break;
            }
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.is_empty() || line[0] == b'#' {
                continue;
            }
            // strtok(line, "\t") twice, then the rest (strtok(NULL, ""))
            let mut rest = line;
            let mut tok = || {
                let s = rest.iter().position(|&b| b != b'\t')?;
                rest = &rest[s..];
                let e = rest.iter().position(|&b| b == b'\t').unwrap_or(rest.len());
                let t = &rest[..e];
                rest = if e < rest.len() { &rest[e + 1..] } else { &[] };
                Some(t)
            };
            let (Some(a), Some(b)) = (tok(), tok()) else { continue };
            if rest.is_empty() {
                continue;
            }
            let c = rest;
            if !scene_matches(a, scene.as_bytes()) {
                continue;
            }
            self.exp.push(Expect { page: cut(b, 31).to_vec(), text: cut(c, 159).to_vec() });
        }
        Ok(())
    }

    pub fn texts_clear(&mut self) {
        self.texts.clear();
        self.missing.clear();
    }

    /// Visible label texts under o (collect), with their missing glyphs.
    pub fn collect(&mut self, o: Obj) {
        if o.is_null() || o.hidden() {
            return;
        }
        if is_label(o) && self.texts.len() < MAX_TEXTS {
            let t = o.text();
            if !t.is_empty() {
                self.check_glyphs(o, &t);
                self.texts.push(t);
            }
        }
        for i in 0..o.child_count() as i32 {
            self.collect(o.child(i));
        }
    }

    fn check_glyphs(&mut self, label: Obj, t: &[u8]) {
        let f = font(label);
        let c = lv::cstr(t);
        let mut i: u32 = 0;
        loop {
            // SAFETY: c is NUL-terminated; i stays inside it
            let ch = unsafe { lv_text_encoded_next(c.as_ptr().cast(), &mut i) };
            if ch == 0 {
                break;
            }
            if glyph_exempt(ch) || f.glyph(ch, 0).is_some() {
                continue;
            }
            if !self.missing.contains(&ch) && self.missing.len() < MAX_MISSING {
                self.missing.push(ch);
            }
        }
    }

    fn geo_bad(&mut self, m: Vec<u8>) {
        if self.geo.len() >= MAX_GEO || self.geo.contains(&m) {
            return;
        }
        self.geo.push(m);
    }

    /// Where a label's text actually is (ink_box).
    fn ink_box(o: Obj) -> lv_area_t {
        let c = content_coords(o);
        let w = aw(&c);
        let wrap = long_mode(o) == sys::lv_label_long_mode_t::LV_LABEL_LONG_MODE_WRAP;
        let (sx, sy) = text_size(&o.text(), o, if wrap { w } else { lv::COORD_MAX });
        let tw = sx.min(w);
        let th = sy.min(ah(&c));
        // SAFETY: o is live
        let mut a = unsafe { sys::lv_obj_get_style_text_align(o.0, PART_MAIN) };
        if a == sys::lv_text_align_t::LV_TEXT_ALIGN_AUTO {
            a = sys::lv_text_align_t::LV_TEXT_ALIGN_LEFT;
        }
        let x1 = if a == sys::lv_text_align_t::LV_TEXT_ALIGN_RIGHT {
            c.x2 - tw + 1
        } else if a == sys::lv_text_align_t::LV_TEXT_ALIGN_CENTER {
            c.x1 + (w - tw) / 2
        } else {
            c.x1
        };
        lv_area_t { x1, x2: x1 + tw - 1, y1: c.y1, y2: c.y1 + th - 1 }
    }

    fn geo_label(&mut self, l: Obj) {
        let a = l.coords();
        let t = l.text();
        if a.x1 < 0 || a.x2 >= W {
            self.geo_bad(cat(&[b"\"", cut(&t, 60), format!("\" runs off the screen (x {}..{})", a.x1, a.x2).as_bytes()]));
        }
        let mut p = l.parent();
        while !p.is_null() {
            // SAFETY: p is live
            let (sr, sl) = unsafe { (sys::lv_obj_get_scroll_right(p.0), sys::lv_obj_get_scroll_left(p.0)) };
            if p.scroll_dir() & sys::lv_dir_t::LV_DIR_HOR != 0 && p.has_flag(lv::LV_OBJ_FLAG_SCROLLABLE) && sr + sl > 0 {
                break; // the tileview
            }
            let pa = p.coords();
            if a.x1 < pa.x1 || a.x2 > pa.x2 {
                self.geo_bad(cat(&[
                    b"\"",
                    cut(&t, 60),
                    format!("\" (x {}..{}) sticks out of its container (x {}..{})", a.x1, a.x2, pa.x1, pa.x2).as_bytes(),
                ]));
                break;
            }
            p = p.parent();
        }
        let m = long_mode(l);
        if m == sys::lv_label_long_mode_t::LV_LABEL_LONG_MODE_DOTS || m == sys::lv_label_long_mode_t::LV_LABEL_LONG_MODE_CLIP {
            let (sx, _) = text_size(&t, l, lv::COORD_MAX);
            // SAFETY: l is live
            let cw = unsafe { sys::lv_obj_get_content_width(l.0) };
            if sx > cw + 1 {
                self.ntrunc += 1;
                if self.dump {
                    use std::io::Write;
                    let mut o = std::io::stdout().lock();
                    let _ = o.write_all(&cat(&[b"TRUNC\t", &t, format!("\t{sx}>{cw}\n").as_bytes()]));
                }
            }
        }
    }

    fn is_text_label(o: Obj) -> bool {
        if o.is_null() || o.hidden() || !is_label(o) {
            return false;
        }
        if style_opa(o) == lv::OPA_TRANSP || text_opa(o) == lv::OPA_TRANSP {
            return false;
        }
        !o.text().is_empty()
    }

    fn is_block(d: Obj, parent: Obj) -> bool {
        if d.is_null() || d.hidden() || is_label(d) {
            return false;
        }
        if style_opa(d) == lv::OPA_TRANSP {
            return false;
        }
        if d.style_bg_opa(PART_MAIN) < OPA_20 && !is_type(d, &raw const lv_switch_class) && !is_type(d, &raw const lv_slider_class) {
            return false;
        }
        d.width() < parent.width() - 8 && d.height() < parent.height() - 8
    }

    pub fn geo_walk(&mut self, o: Obj) {
        if o.is_null() || o.hidden() {
            return;
        }
        if is_label(o) {
            let t = o.text();
            let a = o.coords();
            if !t.is_empty() && on_screen(&a) {
                self.geo_label(o);
            }
        }
        let n = o.child_count() as i32;
        for i in 0..n {
            let c = o.child(i);
            if !Self::is_text_label(c) {
                continue;
            }
            let a = Self::ink_box(c);
            if !on_screen(&a) {
                continue;
            }
            for j in i + 1..n {
                let d = o.child(j);
                if !Self::is_text_label(d) {
                    continue;
                }
                let b = Self::ink_box(d);
                let hmin = ah(&a).min(ah(&b));
                if let Some(x) = intersect(&a, &b)
                    && aw(&x) > 1
                    && ah(&x) * 5 > hmin * 2
                {
                    self.geo_bad(cat(&[b"\"", cut(&c.text(), 40), b"\" overlaps \"", cut(&d.text(), 40), b"\""]));
                }
            }
        }
        for i in 0..n {
            let c = o.child(i);
            if !Self::is_text_label(c) || c.style_bg_opa(PART_MAIN) >= lv::OPA_50 {
                continue;
            }
            let a = Self::ink_box(c);
            if !on_screen(&a) {
                continue;
            }
            for j in 0..n {
                let d = o.child(j);
                if !Self::is_block(d, o) {
                    continue;
                }
                let b = d.coords();
                if let Some(x) = intersect(&a, &b)
                    && aw(&x) > 1
                    && ah(&x) > 1
                {
                    self.geo_bad(cat(&[b"\"", cut(&c.text(), 40), format!("\" runs into a {}x{} block", aw(&b), ah(&b)).as_bytes()]));
                }
            }
        }
        for i in 0..n {
            self.geo_walk(o.child(i));
        }
    }

    fn ok(&mut self) {
        self.pass += 1;
    }
    fn bad(&mut self, m: Vec<u8>) {
        self.fails.push(m);
    }

    /// page_done: expect, glyphs, geometry (and --dump's TEXT lines). Results in pass/fails.
    pub fn page_done(&mut self, page: &str) {
        self.pass = 0;
        self.fails.clear();
        let pg = page.as_bytes();
        for k in 0..self.exp.len() {
            if self.exp[k].page != pg {
                continue;
            }
            let want = &self.exp[k].text;
            if self.texts.iter().any(|t| contains(t, want)) {
                self.ok();
            } else {
                let m = cat(&[pg, b": no visible text \"", want, b"\""]);
                self.bad(m);
            }
        }
        let missing = std::mem::take(&mut self.missing);
        for &c in &missing {
            let u = char::from_u32(c).filter(|_| c < 0x10000).map(|ch| ch.to_string()).unwrap_or_default();
            self.bad(cat(&[pg, format!(": no glyph for U+{c:04X} \"{u}\" in the label's fonts").as_bytes()]));
        }
        if missing.is_empty() {
            self.ok();
        }
        let nf = self.fails.len();
        for g in std::mem::take(&mut self.geo) {
            self.bad(cat(&[pg, b": ", &g]));
        }
        if self.fails.len() == nf {
            self.ok();
        }
        if self.dump {
            use std::io::Write;
            let mut o = std::io::stdout().lock();
            for t in &self.texts {
                let one: Vec<u8> = cut(t, 199).iter().map(|&b| if b == b'\n' { b'|' } else { b }).collect();
                let _ = o.write_all(&cat(&[b"TEXT\t", pg, b"\t", &one, b"\n"]));
            }
        }
    }
}

const OPA_20: u8 = 51;

fn on_screen(a: &lv_area_t) -> bool {
    a.y2 >= 0 && a.y1 < H && a.x2 >= 0 && a.x1 < W
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || hay.windows(needle.len()).any(|w| w == needle)
}

fn cat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

/// scene_matches: "*", "a,b,c", or "!a,b" (every scene but these)
fn scene_matches(spec: &[u8], scene: &[u8]) -> bool {
    let neg = spec.first() == Some(&b'!');
    if !neg && spec == b"*" {
        return true;
    }
    let body = if neg { &spec[1..] } else { spec };
    let hit = body.split(|&b| b == b',').any(|t| !t.is_empty() && t == scene);
    if neg { !hit } else { hit }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenes() {
        assert!(scene_matches(b"*", b"good"));
        assert!(scene_matches(b"good,weak", b"weak"));
        assert!(!scene_matches(b"good,weak", b"lte"));
        assert!(scene_matches(b"!good", b"lte"));
        assert!(!scene_matches(b"!good,lte", b"lte"));
    }

    #[test]
    fn cuts_bytes() {
        assert_eq!(cut(b"abc", 2), b"ab");
        assert_eq!(cut(b"abc", 9), b"abc");
        assert!(contains(b"China Telecom", b"Tele"));
        assert!(!contains(b"China", b"Tele"));
    }
}
