//! The panel as a DRM dumb buffer: RGB565, CPU-drawn, each finished rectangle pushed with
//! DIRTYFB. Plain ioctls, since there is no libdrm on the device to link against.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;

const CARD: &str = "/dev/dri/card0";
/// What this panel's card reports when asking it fails (the U60's DSI connector and its CRTC).
const PANEL_CONNECTOR: u32 = 31;
const PANEL_CRTC: u32 = 34;
const PANEL_W: u16 = 320;
const PANEL_H: u16 = 480;

#[repr(C)]
#[derive(Default)]
struct CardRes {
    fb_id_ptr: u64,
    crtc_id_ptr: u64,
    connector_id_ptr: u64,
    encoder_id_ptr: u64,
    count_fbs: u32,
    count_crtcs: u32,
    count_connectors: u32,
    count_encoders: u32,
    min_width: u32,
    max_width: u32,
    min_height: u32,
    max_height: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ModeInfo {
    clock: u32,
    hdisplay: u16,
    hsync_start: u16,
    hsync_end: u16,
    htotal: u16,
    hskew: u16,
    vdisplay: u16,
    vsync_start: u16,
    vsync_end: u16,
    vtotal: u16,
    vscan: u16,
    vrefresh: u32,
    flags: u32,
    kind: u32,
    name: [u8; 32],
}

#[repr(C)]
#[derive(Default)]
struct GetConnector {
    encoders_ptr: u64,
    modes_ptr: u64,
    props_ptr: u64,
    prop_values_ptr: u64,
    count_modes: u32,
    count_props: u32,
    count_encoders: u32,
    encoder_id: u32,
    connector_id: u32,
    connector_type: u32,
    connector_type_id: u32,
    connection: u32,
    mm_width: u32,
    mm_height: u32,
    subpixel: u32,
    pad: u32,
}

#[repr(C)]
#[derive(Default)]
struct GetEncoder {
    encoder_id: u32,
    encoder_type: u32,
    crtc_id: u32,
    possible_crtcs: u32,
    possible_clones: u32,
}

#[repr(C)]
#[derive(Default)]
struct Crtc {
    set_connectors_ptr: u64,
    count_connectors: u32,
    crtc_id: u32,
    fb_id: u32,
    x: u32,
    y: u32,
    gamma_size: u32,
    mode_valid: u32,
    mode: ModeInfo,
}

#[repr(C)]
#[derive(Default)]
struct CreateDumb {
    height: u32,
    width: u32,
    bpp: u32,
    flags: u32,
    handle: u32,
    pitch: u32,
    size: u64,
}

#[repr(C)]
#[derive(Default)]
struct MapDumb {
    handle: u32,
    pad: u32,
    offset: u64,
}

#[repr(C)]
#[derive(Default)]
struct FbCmd {
    fb_id: u32,
    width: u32,
    height: u32,
    pitch: u32,
    bpp: u32,
    depth: u32,
    handle: u32,
}

#[repr(C)]
struct ClipRect {
    x1: u16,
    y1: u16,
    x2: u16,
    y2: u16,
}

#[repr(C)]
struct FbDirty {
    fb_id: u32,
    flags: u32,
    color: u32,
    num_clips: u32,
    clips_ptr: u64,
}

// DRM_IOWR numbers
const GETRESOURCES: u64 = 0xA0;
const SETCRTC: u64 = 0xA2;
const GETENCODER: u64 = 0xA6;
const GETCONNECTOR: u64 = 0xA7;
const ADDFB: u64 = 0xAE;
const DIRTYFB: u64 = 0xB1;
const CREATE_DUMB: u64 = 0xB2;
const MAP_DUMB: u64 = 0xB3;

/// One DRM ioctl with its argument struct, retried when a signal interrupts it.
fn drm<T>(card: &File, nr: u64, arg: &mut T) -> io::Result<()> {
    let req = (3u64 << 30) | ((size_of::<T>() as u64) << 16) | ((b'd' as u64) << 8) | nr;
    loop {
        // SAFETY: `arg` is the #[repr(C)] struct the kernel expects for request `nr`
        if unsafe { libc::ioctl(card.as_raw_fd(), req as _, arg as *mut T) } == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// A list the kernel fills in: point it at a buffer of `n` ids.
fn ids(n: u32) -> Vec<u32> {
    vec![0; n as usize]
}

/// One connector as the kernel described it, reduced to what choosing a CRTC needs.
#[derive(Default)]
struct Output {
    id: u32,
    /// connected, with at least one mode
    usable: bool,
    /// the CRTC its current encoder drives (0 = none / unknown)
    driven_by: u32,
    /// possible_crtcs of each encoder it can use, in the kernel's order
    reachable: Vec<u32>,
}

/// The first usable connector and its CRTC: the one already driving it, else the first CRTC any of
/// its encoders can reach, else the card's first CRTC. Index into `outputs`, CRTC id.
fn choose(crtcs: &[u32], outputs: &[Output]) -> Option<(usize, u32)> {
    let reachable = |mask: u32| crtcs.iter().enumerate().find(|(i, _)| mask & (1 << i) != 0).map(|(_, c)| *c);
    outputs.iter().enumerate().filter(|(_, o)| o.usable).find_map(|(i, o)| {
        let crtc = Some(o.driven_by)
            .filter(|&c| c != 0)
            .or_else(|| o.reachable.iter().find_map(|&m| reachable(m)))
            .or_else(|| crtcs.first().copied())
            .filter(|&c| c != 0)?;
        Some((i, crtc))
    })
}

/// Ask the card for its connectors and pick one to drive: (connector, CRTC, its first mode).
fn pick(card: &File) -> io::Result<(u32, u32, ModeInfo)> {
    // first call: how many; second: the ids
    let mut res = CardRes::default();
    drm(card, GETRESOURCES, &mut res)?;
    let (mut crtcs, mut conns, mut encs) = (ids(res.count_crtcs), ids(res.count_connectors), ids(res.count_encoders));
    res.count_fbs = 0;
    res.crtc_id_ptr = crtcs.as_mut_ptr() as u64;
    res.connector_id_ptr = conns.as_mut_ptr() as u64;
    res.encoder_id_ptr = encs.as_mut_ptr() as u64;
    drm(card, GETRESOURCES, &mut res)?;

    let encoder = |id: u32| {
        let mut e = GetEncoder { encoder_id: id, ..Default::default() };
        drm(card, GETENCODER, &mut e).ok().map(|()| e)
    };
    let mut outputs = Vec::with_capacity(conns.len());
    let mut modes = Vec::with_capacity(conns.len());
    for &id in &conns {
        let (out, mode) = describe(card, id, &encoder).unwrap_or_else(|| (Output { id, ..Default::default() }, ModeInfo::default()));
        outputs.push(out);
        modes.push(mode);
    }
    let (i, crtc) = choose(&crtcs, &outputs).ok_or_else(|| io::Error::other("no connected connector"))?;
    Ok((outputs[i].id, crtc, modes[i]))
}

/// A connected connector with modes: what `choose` needs and its first mode. None otherwise.
fn describe(card: &File, id: u32, encoder: &dyn Fn(u32) -> Option<GetEncoder>) -> Option<(Output, ModeInfo)> {
    let mut c = GetConnector { connector_id: id, ..Default::default() };
    drm(card, GETCONNECTOR, &mut c).ok()?;
    if c.connection != 1 || c.count_modes == 0 {
        return None;
    }
    let mut modes = vec![ModeInfo::default(); c.count_modes as usize];
    let mut encs = ids(c.count_encoders);
    c.modes_ptr = modes.as_mut_ptr() as u64;
    c.encoders_ptr = encs.as_mut_ptr() as u64;
    c.count_props = 0;
    drm(card, GETCONNECTOR, &mut c).ok()?;
    if c.count_modes == 0 {
        return None;
    }
    let driven_by = match c.encoder_id {
        0 => 0,
        e => encoder(e).map_or(0, |e| e.crtc_id),
    };
    let reachable = encs.iter().filter_map(|&e| encoder(e)).map(|e| e.possible_crtcs).collect();
    Some((Output { id, usable: true, driven_by, reachable }, modes[0]))
}

/// The panel's own mode, for when the card cannot be asked.
fn panel_mode() -> ModeInfo {
    let mut m = ModeInfo {
        clock: 1,
        hdisplay: PANEL_W,
        hsync_start: PANEL_W,
        hsync_end: PANEL_W,
        htotal: PANEL_W,
        vdisplay: PANEL_H,
        vsync_start: PANEL_H,
        vsync_end: PANEL_H,
        vtotal: PANEL_H,
        vrefresh: 1,
        kind: 0x48, // DRM_MODE_TYPE_PREFERRED | DRM_MODE_TYPE_DRIVER
        ..Default::default()
    };
    let name = format!("{PANEL_W}x{PANEL_H}");
    m.name[..name.len()].copy_from_slice(name.as_bytes());
    m
}

pub struct Drm {
    card: File,
    map: *mut u16,
    map_len: usize,
    fb_id: u32,
    pub width: usize,
    pub height: usize,
    pub pitch_px: usize,
}

impl Drm {
    pub fn open() -> io::Result<Drm> {
        let card = OpenOptions::new().read(true).write(true).open(CARD)?;
        let (conn, crtc, mode) = pick(&card).unwrap_or_else(|e| {
            eprintln!("drm: {e}, fallback connector {PANEL_CONNECTOR} crtc {PANEL_CRTC}");
            (PANEL_CONNECTOR, PANEL_CRTC, panel_mode())
        });
        let (w, h) = (u32::from(mode.hdisplay), u32::from(mode.vdisplay));

        // a 16-bit buffer the size of the mode, a framebuffer on it, and a CPU mapping of it
        let mut buf = CreateDumb { width: w, height: h, bpp: 16, ..Default::default() };
        drm(&card, CREATE_DUMB, &mut buf)?;
        let mut fb = FbCmd { width: w, height: h, pitch: buf.pitch, bpp: 16, depth: 16, handle: buf.handle, ..Default::default() };
        drm(&card, ADDFB, &mut fb)?;
        let mut where_ = MapDumb { handle: buf.handle, ..Default::default() };
        drm(&card, MAP_DUMB, &mut where_)?;
        let len = buf.size as usize;
        // SAFETY: MAP_DUMB handed out this offset for a `len`-byte mapping of the buffer on `card`
        let map = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, card.as_raw_fd(), where_.offset as libc::off_t) };
        if map == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the `len` bytes just mapped
        unsafe { std::ptr::write_bytes(map.cast::<u8>(), 0, len) };
        let d = Drm { card, map: map.cast(), map_len: len, fb_id: fb.fb_id, width: w as usize, height: h as usize, pitch_px: buf.pitch as usize / 2 };

        // show it
        let mut conn_ids = [conn];
        let mut set = Crtc { set_connectors_ptr: conn_ids.as_mut_ptr() as u64, count_connectors: 1, crtc_id: crtc, fb_id: d.fb_id, mode_valid: 1, mode, ..Default::default() };
        drm(&d.card, SETCRTC, &mut set)?;
        eprintln!("drm: connector {conn} crtc {crtc} {w}x{h} pitch {}", buf.pitch);
        d.dirty(0, 0, d.width, d.height);
        Ok(d)
    }

    fn all(&self) -> &[u16] {
        // SAFETY: our mapping, map_len bytes, alive as long as self
        unsafe { std::slice::from_raw_parts(self.map, self.map_len / 2) }
    }

    /// The whole buffer, pitch padding included.
    pub fn pixels(&mut self) -> &mut [u16] {
        // SAFETY: as in all(), and &mut self keeps it exclusive
        unsafe { std::slice::from_raw_parts_mut(self.map, self.map_len / 2) }
    }

    /// The visible rows (width × height, without the pitch's padding).
    pub fn rows(&self) -> Vec<u16> {
        self.all().chunks(self.pitch_px).take(self.height).flat_map(|row| row[..self.width].iter().copied()).collect()
    }

    /// Send [x1,x2) × [y1,y2) to the panel. Not every driver has DIRTYFB, so failure is fine.
    pub fn dirty(&self, x1: usize, y1: usize, x2: usize, y2: usize) {
        let mut clip = ClipRect { x1: x1 as u16, y1: y1 as u16, x2: x2 as u16, y2: y2 as u16 };
        let mut cmd = FbDirty { fb_id: self.fb_id, flags: 0, color: 0, num_clips: 1, clips_ptr: &mut clip as *mut ClipRect as u64 };
        let _ = drm(&self.card, DIRTYFB, &mut cmd);
    }
}

impl Drop for Drm {
    fn drop(&mut self) {
        // SAFETY: unmapping our own mapping once
        unsafe { libc::munmap(self.map.cast(), self.map_len) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(id: u32, usable: bool, driven_by: u32, reachable: &[u32]) -> Output {
        Output { id, usable, driven_by, reachable: reachable.to_vec() }
    }

    #[test]
    fn the_crtc_already_driving_it_wins() {
        assert_eq!(choose(&[34, 35], &[out(31, true, 35, &[0b01])]), Some((0, 35)));
    }

    #[test]
    fn else_the_first_crtc_an_encoder_reaches() {
        assert_eq!(choose(&[34, 35, 36], &[out(31, true, 0, &[0b100, 0b010])]), Some((0, 36)));
        assert_eq!(choose(&[34, 35, 36], &[out(31, true, 0, &[0, 0b110])]), Some((0, 35)));
    }

    #[test]
    fn else_the_cards_first_crtc() {
        assert_eq!(choose(&[34, 35], &[out(31, true, 0, &[])]), Some((0, 34)));
        assert_eq!(choose(&[34, 35], &[out(31, true, 0, &[0b1000])]), Some((0, 34)));
    }

    #[test]
    fn unusable_connectors_are_skipped() {
        let o = [out(30, false, 34, &[1]), out(31, true, 0, &[0b10])];
        assert_eq!(choose(&[34, 35], &o), Some((1, 35)));
        assert_eq!(choose(&[34], &[out(30, false, 34, &[1])]), None);
    }

    #[test]
    fn no_crtc_at_all() {
        assert_eq!(choose(&[], &[out(31, true, 0, &[1])]), None);
    }
}
