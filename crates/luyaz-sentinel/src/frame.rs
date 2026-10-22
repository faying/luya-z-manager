//! The hand-back explanation frame (u60-sentinel.md §5 new 1; u60-platform.md DR8, T16/T24):
//! when the sentinel gives up on our UI it shows one frame for 3 s — what happened, that the
//! internet is fine, how to come back — before starting the vendor UI. The text is pre-rendered
//! (`frame/gen.py` → `frame_text.rs`, 4-bit alpha, run-length coded); this composes it into an
//! RGB565 picture the size of the panel, rotated for its 180° mounting like the touch UI does.
//! Drawing it (`sys::show_frame`) takes DRM master or skips.

/// One pre-rendered line: `w` × `h` alpha values, as (alpha 0–15, run 1–255) byte pairs.
pub struct Line {
    pub title: bool,
    pub w: usize,
    pub h: usize,
    pub rle: &'static [u8],
}

/// How long the frame stays up before the vendor UI starts.
pub const SHOW_MS: u64 = 3000;
const BG: (u8, u8, u8) = (0x10, 0x12, 0x14);
const TITLE: (u8, u8, u8) = (0xFF, 0xB0, 0x20);
const BODY: (u8, u8, u8) = (0xE8, 0xEA, 0xEC);
/// Space after the title, and between body lines.
const TITLE_GAP: usize = 18;
const LINE_GAP: usize = 6;

pub fn lines(en: bool) -> &'static [Line] {
    if en {
        crate::frame_text::EN
    } else {
        crate::frame_text::ZH
    }
}

/// The alpha values of a line, row by row (`w * h` of them; short data is padded with 0).
pub fn decode(l: &Line) -> Vec<u8> {
    let mut out = Vec::with_capacity(l.w * l.h);
    for p in l.rle.as_chunks::<2>().0.iter() {
        out.extend(std::iter::repeat_n(p[0], p[1] as usize));
    }
    out.resize(l.w * l.h, 0);
    out
}

pub fn rgb565((r, g, b): (u8, u8, u8)) -> u16 {
    ((r as u16 >> 3) << 11) | ((g as u16 >> 2) << 5) | (b as u16 >> 3)
}

fn mix(bg: (u8, u8, u8), fg: (u8, u8, u8), a: u8) -> (u8, u8, u8) {
    let m = |b: u8, f: u8| ((b as u32 * (15 - a as u32) + f as u32 * a as u32) / 15) as u8;
    (m(bg.0, fg.0), m(bg.1, fg.1), m(bg.2, fg.2))
}

/// The frame as the user sees it (`w` × `h`, row-major, upright): the lines centred, the block
/// centred vertically.
pub fn compose(en: bool, w: usize, h: usize) -> Vec<u16> {
    let mut px = vec![rgb565(BG); w * h];
    let ls = lines(en);
    let total: usize = ls
        .iter()
        .enumerate()
        .map(|(i, l)| {
            l.h + if i + 1 == ls.len() {
                0
            } else if l.title {
                TITLE_GAP
            } else {
                LINE_GAP
            }
        })
        .sum();
    let mut y0 = h.saturating_sub(total) / 2;
    for l in ls {
        let a = decode(l);
        let x0 = w.saturating_sub(l.w) / 2;
        let fg = if l.title { TITLE } else { BODY };
        for y in 0..l.h {
            for x in 0..l.w {
                let al = a[y * l.w + x];
                let (px_x, px_y) = (x0 + x, y0 + y);
                if al == 0 || px_x >= w || px_y >= h {
                    continue;
                }
                px[px_y * w + px_x] = rgb565(mix(BG, fg, al));
            }
        }
        y0 += l.h + if l.title { TITLE_GAP } else { LINE_GAP };
    }
    px
}

/// The panel is mounted upside down: what goes into the framebuffer is the picture turned 180°.
pub fn rotate180(px: &mut [u16]) {
    px.reverse();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_line_decodes_to_its_size_and_fits_the_panel() {
        for en in [false, true] {
            for l in lines(en) {
                let n: usize = l.rle.as_chunks::<2>().0.iter().map(|p| p[1] as usize).sum();
                assert_eq!(n, l.w * l.h, "rle covers the whole line");
                assert!(
                    l.rle
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .all(|p| p[0] <= 15 && p[1] >= 1)
                );
                assert!(l.w <= 300);
                assert!(decode(l).contains(&15), "has solid ink");
            }
            let total: usize = lines(en).iter().map(|l| l.h + TITLE_GAP).sum();
            assert!(total < 480);
        }
    }

    #[test]
    fn the_frame_has_text_in_the_middle_and_rotates() {
        let (w, h) = (320, 480);
        let mut f = compose(false, w, h);
        let bg = rgb565(BG);
        assert_eq!(f.len(), w * h);
        assert_eq!(f[0], bg);
        assert_eq!(f[w * h - 1], bg);
        let ink: Vec<usize> = (0..f.len()).filter(|&i| f[i] != bg).collect();
        assert!(ink.len() > 2000, "{} pixels of text", ink.len());
        let (top, bottom) = (ink[0] / w, ink[ink.len() - 1] / w);
        assert!(
            top > 100 && bottom < 380,
            "block centred: rows {top}..{bottom}"
        );
        assert!(f.contains(&rgb565(TITLE)));
        let first_ink = ink[0];
        rotate180(&mut f);
        assert_ne!(f[w * h - 1 - first_ink], bg, "turned around");
        // a smaller panel than expected does not panic
        let _ = compose(true, 100, 50);
    }

    /// `FRAME_DUMP=<dir>`: write both frames upright as raw RGB565 (320×480) for a preview.
    #[test]
    fn dump_for_preview() {
        if let Ok(d) = std::env::var("FRAME_DUMP") {
            for (name, en) in [("zh", false), ("en", true)] {
                let px = compose(en, 320, 480);
                let bytes: Vec<u8> = px.iter().flat_map(|p| p.to_le_bytes()).collect();
                std::fs::write(format!("{d}/frame-{name}.rgb565"), bytes).unwrap();
            }
        }
    }

    #[test]
    fn colours() {
        assert_eq!(rgb565((255, 255, 255)), 0xFFFF);
        assert_eq!(rgb565((0, 0, 0)), 0);
        assert_eq!(mix((0, 0, 0), (255, 255, 255), 15), (255, 255, 255));
        assert_eq!(mix((0, 0, 0), (255, 255, 255), 0), (0, 0, 0));
    }
}
