//! Colour tokens and font roles (src/ui_theme.c, include/ui_theme.h).
//!
//! One theme per process, picked before the first object (switching appearance execs a new
//! process), so pages read t() at build time and never restyle. Every colour is already
//! quantised to RGB565 and contrast-checked: text on a fill* is white; vivid colours carry no
//! text; dimming = t3, never a lower opacity.
// SPDX-License-Identifier: MIT

use crate::lang;
use crate::lv::{self, Font};
use std::sync::OnceLock;

#[derive(Debug)]
pub struct Theme {
    pub bg: u32,
    pub card: u32,
    pub sep: u32,
    pub t1: u32,
    pub t2: u32,
    pub t3: u32,
    pub ok_t: u32,
    pub warn_t: u32,
    pub bad_t: u32,
    pub acc_t: u32,
    pub fill_blue: u32,
    pub fill_orange: u32,
    pub fill_green: u32,
    pub fill_red: u32,
    pub green: u32,
    pub orange: u32,
    pub red: u32,
    pub blue: u32,
    pub wash: u32,
    pub wash_w: u32,
    pub wash_b: u32,
    pub glass: u32,
    pub track: u32,
    /// a switch's off track: darker than `track`, which is also the neutral card wash (10-11)
    pub sw_off: u32,
    pub acc_s: u32,
    pub pill: u32,
    pub on_fill: u32,
    /// card top highlight (dark only, 0 = none)
    pub hl_opa: u8,
    /// 1 px black rim on glass
    pub rim_opa: u8,
    /// sheet backdrop
    pub scrim_opa: u8,
    pub dark: bool,
}

pub static LIGHT: Theme = Theme {
    bg: 0xeff3f7,
    card: 0xffffff,
    sep: 0xdee3e7,
    t1: 0x080c10,
    t2: 0x393c42,
    t3: 0x5a616b,
    ok_t: 0x107131,
    warn_t: 0xa54908,
    bad_t: 0xc60008,
    acc_t: 0x005dbd,
    fill_blue: 0x0065ce,
    fill_orange: 0xb55108,
    fill_green: 0x187d39,
    fill_red: 0xd60010,
    green: 0x31c75a,
    orange: 0xff8e29,
    red: 0xff3839,
    blue: 0x008aff,
    wash: 0xd6efde,
    wash_w: 0xffebd6,
    wash_b: 0xffe3e7,
    glass: 0xf7fbff,
    track: 0xe7e7ef,
    sw_off: 0xc7c7cc,
    acc_s: 0xe7f3ff,
    pill: 0xffffff,
    on_fill: 0xe7f3ff,
    hl_opa: 0,
    rim_opa: 26,
    scrim_opa: 90,
    dark: false,
};

pub static DARK: Theme = Theme {
    bg: 0x080c10,
    card: 0x181c21,
    sep: 0x29303a,
    t1: 0xf7f7f7,
    t2: 0xc6c7ce,
    t3: 0xa5a6ad,
    ok_t: 0x31d35a,
    warn_t: 0xff9230,
    bad_t: 0xff797b,
    acc_t: 0x5aaaff,
    fill_blue: 0x0065ce,
    fill_orange: 0xb55108,
    fill_green: 0x187d39,
    fill_red: 0xd60010,
    green: 0x31d35a,
    orange: 0xff9230,
    red: 0xff4245,
    blue: 0x0091ff,
    wash: 0x103018,
    wash_w: 0x392408,
    wash_b: 0x391018,
    glass: 0x212831,
    track: 0x31363a,
    sw_off: 0x5a5f66,
    acc_s: 0x10304a,
    pill: 0x39414a,
    on_fill: 0xe7f3ff,
    hl_opa: 40,
    rim_opa: 150,
    scrim_opa: 140,
    dark: true,
};

static THEME: OnceLock<&'static Theme> = OnceLock::new();

/// Pick the theme: once, before any object exists.
pub fn select(dark: bool) {
    let _ = THEME.set(if dark { &DARK } else { &LIGHT });
}

/// The theme (light until select()).
pub fn t() -> &'static Theme {
    THEME.get().copied().unwrap_or(&LIGHT)
}

/// Font roles. n* = Nunito (rounded numerals, weight in the name) over the CJK font, so
/// "12 条" draws the digits in Nunito and 条 in CJK. cj* = the device CJK font (ZTEZhengYuan),
/// *b = synthetic bold (LVGL patch). Missing Nunito → device Roboto at the same sizes; missing
/// Roboto too → the CJK font. Missing device CJK font → the bundled subset, then Montserrat.
#[derive(Clone, Copy, Debug)]
pub struct Fonts {
    pub nbat: Font,
    pub n11: Font,
    pub n12: Font,
    pub n15: Font,
    pub n17: Font,
    pub n20: Font,
    pub n32: Font,
    pub n36: Font,
    pub cj11: Font,
    pub cj12: Font,
    pub cj13: Font,
    pub cj14: Font,
    pub cj15: Font,
    pub cj15b: Font,
    pub cj17b: Font,
    pub cj20b: Font,
    pub cj22b: Font,
    pub cj24b: Font,
    /// regular 20: fallback under cj20b
    pub cj20: Font,
    /// "nunito" / "roboto" / "cjk", for the startup log
    pub numerals: &'static str,
    /// "device" / "bundled" / "montserrat": which CJK font loaded
    pub cjk: &'static str,
}

static FONTS: OnceLock<Fonts> = OnceLock::new();

/// The font roles; load() must have run.
pub fn uf() -> &'static Fonts {
    FONTS.get().expect("fonts::load first")
}

const CJK_FONT_DEFAULT: &str = "/usr/ui/fonts/ZTEZhengYuan.ttf";
const ROBOTO_FONT_DEFAULT: &str = "/usr/ui/fonts/Roboto.ttf";
/// Phase 10 R5 (review D5): the fonts moved with the program; the old directory while a device
/// still has them there.
const FONT_DIR_DEFAULT: &str = "/data/luyaz/ui/fonts";
const FONT_DIR_OLD: &str = "/data/plugins/u60pro-devui/fonts";

/// The font directory: the new one when it is there, else the old one (D5).
pub fn font_dir_default(new_exists: bool) -> &'static str {
    if new_exists { FONT_DIR_DEFAULT } else { FONT_DIR_OLD }
}
/// Bundled CJK subset (scripts/fonts/build-cjk-fallback.sh), used when the device font is missing.
const CJK_FALLBACK_NAME: &str = "u60-cjk-fallback.ttf";

fn env_or(name: &str, def: &str) -> String {
    std::env::var(name).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| def.to_string())
}

/// A FreeType font, or None. The path stays allocated: font objects may keep it.
fn ft(path: &str, size: u32, bold: bool) -> Option<*mut lv::sys::lv_font_t> {
    use lv::sys::{lv_freetype_font_render_mode_t as rm, lv_freetype_font_style_t as fs};
    let c: &'static [u8] = Box::leak(lv::cstr(path.as_bytes()).into_boxed_slice());
    // SAFETY: lv_init ran (LV_USE_FREETYPE initialises FreeType); c is NUL-terminated and leaked
    let f = unsafe {
        lv::sys::lv_freetype_font_create(
            c.as_ptr().cast(),
            rm::LV_FREETYPE_FONT_RENDER_MODE_BITMAP,
            size,
            if bold { fs::LV_FREETYPE_FONT_STYLE_BOLD } else { fs::LV_FREETYPE_FONT_STYLE_NORMAL },
        )
    };
    (!f.is_null()).then_some(f)
}

fn set_fallback(f: *mut lv::sys::lv_font_t, under: Font) {
    // SAFETY: f is a live FreeType font this module created
    unsafe { (*f).fallback = under.0 };
}

fn cjk(path: &str, size: u32, bold: bool, fallback: Font) -> Font {
    ft(path, size, bold).map_or(fallback, |f| Font(f))
}

/// Nunito at `weight`, else Roboto, else the CJK font; CJK as glyph fallback.
fn num(dir: &str, roboto: &str, weight: u32, size: u32, cjk_fb: Font, used_roboto: &mut bool, used_cjk: &mut bool) -> Font {
    let f = ft(&format!("{dir}/Nunito-{weight}.ttf"), size, false).or_else(|| {
        let r = ft(roboto, size, false);
        if r.is_some() {
            *used_roboto = true;
        }
        r
    });
    match f {
        None => {
            *used_cjk = true;
            cjk_fb
        }
        Some(f) => {
            set_fallback(f, cjk_fb);
            Font(f)
        }
    }
}

/// Nunito `weight` at `size` over the CJK font `under`; `under` if it fails.
fn text_en(dir: &str, weight: u32, size: u32, under: Font) -> Font {
    match ft(&format!("{dir}/Nunito-{weight}.ttf"), size, false) {
        None => under,
        Some(f) => {
            set_fallback(f, under);
            Font(f)
        }
    }
}

/// Load every font role (ui_fonts_load). Paths: $LUYAZ_UI_CJK_FONT (default the device font),
/// $LUYAZ_UI_FONT_DIR/Nunito-{600,700,800}.ttf, $LUYAZ_UI_ROBOTO. Returns false if
/// everything fell back to Montserrat.
pub fn load() -> bool {
    let mut cj = env_or("LUYAZ_UI_CJK_FONT", CJK_FONT_DEFAULT);
    let dir = env_or("LUYAZ_UI_FONT_DIR", font_dir_default(std::path::Path::new(FONT_DIR_DEFAULT).is_dir()));
    let rob = env_or("LUYAZ_UI_ROBOTO", ROBOTO_FONT_DEFAULT);

    // lv_init() already ran lv_freetype_init (LV_USE_FREETYPE)
    let mut probe = ft(&cj, 13, false);
    let mut cjk_src = "device";
    if probe.is_none() {
        let fb = format!("{dir}/{CJK_FALLBACK_NAME}");
        probe = ft(&fb, 13, false);
        if probe.is_some() {
            cj = fb;
            cjk_src = "bundled";
        } else {
            cjk_src = "montserrat";
        }
    }
    let ok = probe.is_some();
    let (m12, m14, m16, m20) = (lv::montserrat(12), lv::montserrat(14), lv::montserrat(16), lv::montserrat(20));

    let cj13 = probe.map_or(m14, |f| Font(f));
    let cj11 = cjk(&cj, 11, false, m12);
    let cj12 = cjk(&cj, 12, false, m12);
    let cj14 = cjk(&cj, 14, false, m14);
    let cj15 = cjk(&cj, 15, false, m14);
    let cj20 = cjk(&cj, 20, false, m20);
    let cj15b = cjk(&cj, 15, true, cj15);
    let cj17b = cjk(&cj, 17, true, m16); // montserrat 16 only if the CJK font is missing
    let cj20b = cjk(&cj, 20, true, cj20);
    let cj22b = cjk(&cj, 22, true, cj20b);
    let cj24b = cjk(&cj, 24, true, cj22b);

    let (mut rb, mut cf) = (false, false);
    let nbat = num(&dir, &rob, 800, 12, cj12, &mut rb, &mut cf); // battery digits: bold, filling the body
    let n11 = num(&dir, &rob, 800, 11, cj11, &mut rb, &mut cf);
    let n12 = num(&dir, &rob, 600, 12, cj12, &mut rb, &mut cf);
    let n15 = num(&dir, &rob, 600, 15, cj15, &mut rb, &mut cf);
    let n17 = num(&dir, &rob, 700, 17, cj17b, &mut rb, &mut cf);
    let n20 = num(&dir, &rob, 700, 20, cj20b, &mut rb, &mut cf);
    let n32 = num(&dir, &rob, 800, 32, cj24b, &mut rb, &mut cf);
    let n36 = num(&dir, &rob, 800, 36, cj24b, &mut rb, &mut cf);
    let numerals = if cf {
        "cjk"
    } else if rb {
        "roboto"
    } else {
        "nunito"
    };

    let mut f = Fonts {
        nbat,
        n11,
        n12,
        n15,
        n17,
        n20,
        n32,
        n36,
        cj11,
        cj12,
        cj13,
        cj14,
        cj15,
        cj15b,
        cj17b,
        cj20b,
        cj22b,
        cj24b,
        cj20,
        numerals,
        cjk: cjk_src,
    };
    // English: body text in Nunito too (design review D2, 10-01; regular → 600, bold → 800),
    // the same-size CJK font underneath for Chinese names (SSIDs, operators). No Nunito → the
    // CJK set stays, as in Chinese.
    let en_text = lang::is_en() && !rb && !cf;
    if en_text {
        f.cj11 = text_en(&dir, 600, 11, f.cj11);
        f.cj12 = text_en(&dir, 600, 12, f.cj12);
        f.cj13 = text_en(&dir, 600, 13, f.cj13);
        f.cj14 = text_en(&dir, 600, 14, f.cj14);
        f.cj15 = text_en(&dir, 600, 15, f.cj15);
        f.cj20 = text_en(&dir, 600, 20, f.cj20);
        f.cj15b = text_en(&dir, 800, 15, f.cj15b);
        f.cj17b = text_en(&dir, 800, 17, f.cj17b);
        f.cj20b = text_en(&dir, 800, 20, f.cj20b);
        f.cj22b = text_en(&dir, 800, 22, f.cj22b);
        f.cj24b = text_en(&dir, 800, 24, f.cj24b);
    }
    eprintln!("ui: fonts cjk={cjk_src} numerals={numerals} text={} (dir {dir})", if en_text { "nunito" } else { "cjk" });
    let _ = FONTS.set(f);
    ok
}
