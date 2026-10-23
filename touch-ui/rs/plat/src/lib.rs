//! The touch UI's platform (manager docs/designs/u60-touch-rewrite.md): the panel, touch and
//! meters, grown out of the Phase 7 spike.
//!
//! The UI draws in UI space (320×480, upright); [`Panel`] turns each dirty rectangle 180° into
//! the scanout buffer, and [`Panel::commit`] pushes a frame's changes with one DIRTYFB (drm_qpic
//! costs ~38 ms per DIRTYFB whatever its size).

pub mod backlight;
pub mod drm;
mod evdev;
pub mod key;
pub mod meter;
pub mod touch;

pub const W: usize = 320;
pub const H: usize = 480;

/// A rectangle in UI space, end-exclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

/// Where the pixels go: the panel (DRM dumb buffer) or memory (offscreen runs).
pub struct Panel {
    drm: Option<drm::Drm>,
    mem: Vec<u16>,
    /// rotate 180° on the way out (the U60 Pro panel is mounted upside down)
    pub rotate: bool,
    pub pushed_px: u64,
    /// DIRTYFB calls (panel pushes)
    pub pushes: u64,
    /// rectangles drawn since the last commit
    pub rects: u64,
    /// changed area since the last commit, panel space: (x1, y1, x2, y2) end-exclusive
    pending: Option<(usize, usize, usize, usize)>,
    /// one DIRTYFB per rectangle instead of one per frame (LUYAZ_UI_DIRTY=each)
    each: bool,
    /// UI-space copy of what is on the panel, for --png (empty = not kept: the device runs
    /// measure without it)
    shadow: Vec<u16>,
}

impl Panel {
    pub fn offscreen() -> Panel {
        Panel { drm: None, mem: vec![0; W * H], rotate: true, pushed_px: 0, pushes: 0, rects: 0, pending: None, each: each(), shadow: vec![0; W * H] }
    }

    pub fn drm() -> std::io::Result<Panel> {
        let d = drm::Drm::open()?;
        if (d.width, d.height) != (W, H) {
            return Err(std::io::Error::other(format!("panel is {}x{}, not {W}x{H}", d.width, d.height)));
        }
        Ok(Panel { drm: Some(d), mem: Vec::new(), rotate: true, pushed_px: 0, pushes: 0, rects: 0, pending: None, each: each(), shadow: Vec::new() })
    }

    /// A real panel (not an offscreen run).
    pub fn on_device(&self) -> bool {
        self.drm.is_some()
    }

    /// Copy `px` (the rectangle's pixels, row stride `stride`) to the panel and push it.
    pub fn put(&mut self, r: Rect, px: &[u16], stride: usize) {
        if r.w == 0 || r.h == 0 {
            return;
        }
        if !self.shadow.is_empty() {
            for row in 0..r.h {
                let src = &px[row * stride..row * stride + r.w];
                self.shadow[(r.y + row) * W + r.x..(r.y + row) * W + r.x + r.w].copy_from_slice(src);
            }
        }
        let (dst, pitch) = match &mut self.drm {
            Some(d) => {
                let p = d.pitch_px;
                (d.pixels(), p)
            }
            None => (&mut self.mem[..], W),
        };
        let (px1, py1) = if self.rotate { (W - r.x - r.w, H - r.y - r.h) } else { (r.x, r.y) };
        for row in 0..r.h {
            let src = &px[row * stride..row * stride + r.w];
            if self.rotate {
                // UI row y goes to panel row H-1-y, and runs right to left
                let drow = H - 1 - (r.y + row);
                let base = drow * pitch + (W - r.x - r.w);
                let d = &mut dst[base..base + r.w];
                for (o, s) in d.iter_mut().zip(src.iter().rev()) {
                    *o = *s;
                }
            } else {
                let base = (r.y + row) * pitch + r.x;
                dst[base..base + r.w].copy_from_slice(src);
            }
        }
        self.rects += 1;
        let b = (px1, py1, px1 + r.w, py1 + r.h);
        self.pending = Some(match self.pending {
            Some(p) => (p.0.min(b.0), p.1.min(b.1), p.2.max(b.2), p.3.max(b.3)),
            None => b,
        });
        if self.each {
            self.commit();
        }
    }

    /// Push what changed since the last commit to the panel: one DIRTYFB with the bounding box.
    /// On the U60 Pro (drm_qpic) every DIRTYFB costs ~38 ms whatever its size (device run
    /// 2026-10-11), so a frame is one call, not one per rectangle.
    pub fn commit(&mut self) {
        let Some((x1, y1, x2, y2)) = self.pending.take() else { return };
        if let Some(d) = &self.drm {
            d.dirty(x1, y1, x2, y2);
        }
        self.pushed_px += ((x2 - x1) * (y2 - y1)) as u64;
        self.pushes += 1;
    }

    /// The scanout buffer as it is (panel orientation), width × height rows (/tmp/fb.dump).
    pub fn scanout(&self) -> Vec<u16> {
        match &self.drm {
            Some(d) => d.rows(),
            None => self.mem.clone(),
        }
    }

    /// What is on the panel, upright, as RGB565.
    pub fn shadow(&self) -> &[u16] {
        &self.shadow
    }

    /// Keep a UI-space copy for [`Panel::write_png`].
    pub fn keep_shadow(&mut self) {
        self.shadow = vec![0; W * H];
    }

    pub fn write_png(&self, path: &std::path::Path) -> std::io::Result<()> {
        if self.shadow.is_empty() {
            return Err(std::io::Error::other("no shadow kept"));
        }
        std::fs::write(path, png_rgb565(W as u32, H as u32, &self.shadow))
    }
}

/// RGB565 pixels as a PNG, expanded as the C render test does (r5 → r<<3 | r>>2, g6 → g<<2 |
/// g>>4), so equal framebuffers give equal files.
pub fn png_rgb565(w: u32, h: u32, px: &[u16]) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(px.len() * 3);
    for &p in px {
        let (r, g, b) = ((p >> 11) & 31, (p >> 5) & 63, p & 31);
        rgb.extend_from_slice(&[(r << 3 | r >> 2) as u8, (g << 2 | g >> 4) as u8, (b << 3 | b >> 2) as u8]);
    }
    png_rgb(w, h, &rgb)
}

/// LUYAZ_UI_DIRTY=each: one DIRTYFB per rectangle, as before
fn each() -> bool {
    std::env::var("LUYAZ_UI_DIRTY").is_ok_and(|v| v == "each")
}

/// RGB888 → RGB565
pub const fn rgb565(rgb: u32) -> u16 {
    (((rgb >> 19) & 31) << 11 | ((rgb >> 10) & 63) << 5 | ((rgb >> 3) & 31)) as u16
}

/// A minimal stored-deflate PNG writer (no compression library on either side).
fn png_rgb(w: u32, h: u32, rgb: &[u8]) -> Vec<u8> {
    fn crc(data: &[u8]) -> u32 {
        let mut c = 0xffff_ffffu32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let c = crc(&out[start..]);
        out.extend_from_slice(&c.to_be_bytes());
    }
    let mut raw = Vec::with_capacity((w as usize * 3 + 1) * h as usize);
    for row in rgb.chunks(w as usize * 3) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut z = vec![0x78, 0x01];
    for (i, block) in raw.chunks(65535).enumerate() {
        let last = (i + 1) * 65535 >= raw.len();
        z.push(last as u8);
        z.extend_from_slice(&(block.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        z.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&(b << 16 | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotated_put_lands_upside_down() {
        let mut p = Panel::offscreen();
        p.put(Rect { x: 0, y: 0, w: 2, h: 1 }, &[1, 2], 2);
        assert_eq!(p.pushes, 0, "pushed on commit");
        p.put(Rect { x: 10, y: 10, w: 1, h: 1 }, &[3], 1);
        p.commit();
        assert_eq!((p.pushes, p.rects), (1, 2));
        assert_eq!(p.pushed_px, 11 * 11, "one bounding box");
        // UI (0,0),(1,0) → panel (319,479),(318,479)
        assert_eq!(p.mem[479 * W + 319], 1);
        assert_eq!(p.mem[479 * W + 318], 2);
        assert_eq!(p.shadow()[0..2], [1, 2]);
    }

    #[test]
    fn rgb565_packs() {
        assert_eq!(rgb565(0xffffff), 0xffff);
        assert_eq!(rgb565(0xff0000), 0xf800);
        assert_eq!(rgb565(0x00ff00), 0x07e0);
    }
}
