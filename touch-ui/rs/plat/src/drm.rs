//! The panel as a DRM dumb buffer: RGB565, CPU-drawn, each finished rectangle pushed with
//! DIRTYFB. Plain ioctls, since there is no libdrm on the device to link against.
//!
//! A theme or language exec hands the open card over ([`Handoff`], issue #11): the new image maps
//! the same buffer and changes nothing on the panel, which (command mode, its own RAM) keeps the
//! old frame until the new one is drawn over it. No modeset, no noise, no dark.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};

const CARD: &str = "/dev/dri/card0";
/// What this panel's card reports when asking it fails (the U60's DSI connector and its CRTC).
const PANEL_CONNECTOR: u32 = 31;
const PANEL_CRTC: u32 = 34;
const PANEL_W: u16 = 320;
const PANEL_H: u16 = 480;
/// DRM's character-device major
const DRM_MAJOR: u32 = 226;
/// the most a handed-over buffer may claim (320×480×2 is 300 KB)
const HANDOFF_MAX_SIZE: u64 = 64 << 20;

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
    /// the dumb buffer's handle on `card`
    handle: u32,
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
        let d = Drm { card, map: map.cast(), map_len: len, fb_id: fb.fb_id, handle: buf.handle, width: w as usize, height: h as usize, pitch_px: buf.pitch as usize / 2 };

        // show it
        let mut conn_ids = [conn];
        let mut set = Crtc { set_connectors_ptr: conn_ids.as_mut_ptr() as u64, count_connectors: 1, crtc_id: crtc, fb_id: d.fb_id, mode_valid: 1, mode, ..Default::default() };
        drm(&d.card, SETCRTC, &mut set)?;
        eprintln!("drm: connector {conn} crtc {crtc} {w}x{h} pitch {}", buf.pitch);
        let _ = d.dirty(0, 0, d.width, d.height);
        Ok(d)
    }

    /// The card a theme exec handed over: the same buffer mapped again, nothing sent to the panel
    /// (no modeset, no clear, no DIRTYFB), so it keeps the last frame until ours covers it. On
    /// failure the card comes back with the reason, for the caller to close once the panel is
    /// dark.
    pub fn adopt(inh: Inherited) -> Result<Drm, (io::Error, File)> {
        let Inherited { card, h } = inh;
        let h = match h.ok_or_else(|| io::Error::other("unreadable --drm= values")).and_then(|h| h.check().map(|()| h)) {
            Ok(h) => h,
            Err(e) => return Err((e, card)),
        };
        let mut where_ = MapDumb { handle: h.handle, ..Default::default() };
        if let Err(e) = drm(&card, MAP_DUMB, &mut where_) {
            return Err((io::Error::other(format!("MAP_DUMB {}: {e}", h.handle)), card));
        }
        let len = h.size as usize;
        // SAFETY: MAP_DUMB handed out this offset for the buffer on `card`; `len` is what the
        // image that made the buffer reported, checked to cover the panel
        let map = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, card.as_raw_fd(), where_.offset as libc::off_t) };
        if map == libc::MAP_FAILED {
            return Err((io::Error::other(format!("mmap: {}", io::Error::last_os_error())), card));
        }
        eprintln!("drm: adopted fd {} fb {} pitch {} (no modeset)", h.fd, h.fb, h.pitch);
        Ok(Drm { card, map: map.cast(), map_len: len, fb_id: h.fb, handle: h.handle, width: PANEL_W as usize, height: PANEL_H as usize, pitch_px: h.pitch as usize / 2 })
    }

    /// What a theme exec passes on (`--drm=`).
    pub fn handoff(&self) -> Handoff {
        Handoff { fd: self.card.as_raw_fd(), handle: self.handle, fb: self.fb_id, pitch: (self.pitch_px * 2) as u32, size: self.map_len as u64 }
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

    /// Send [x1,x2) × [y1,y2) to the panel. ENOSYS: the driver has no DIRTYFB (it scans the
    /// buffer out by itself).
    pub fn dirty(&self, x1: usize, y1: usize, x2: usize, y2: usize) -> io::Result<()> {
        let mut clip = ClipRect { x1: x1 as u16, y1: y1 as u16, x2: x2 as u16, y2: y2 as u16 };
        let mut cmd = FbDirty { fb_id: self.fb_id, flags: 0, color: 0, num_clips: 1, clips_ptr: &mut clip as *mut ClipRect as u64 };
        drm(&self.card, DIRTYFB, &mut cmd)
    }
}

/// Set or clear FD_CLOEXEC on `fd`; false when fcntl failed.
pub fn set_cloexec(fd: i32, on: bool) -> bool {
    // SAFETY: F_GETFD/F_SETFD on an fd number; a closed one just fails
    unsafe {
        let fl = libc::fcntl(fd, libc::F_GETFD);
        if fl < 0 {
            return false;
        }
        let fl = if on { fl | libc::FD_CLOEXEC } else { fl & !libc::FD_CLOEXEC };
        libc::fcntl(fd, libc::F_SETFD, fl) == 0
    }
}

/// The open card and its buffer, as one image hands them to the next: `--drm=FD:HANDLE:FB:PITCH:SIZE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Handoff {
    pub fd: i32,
    pub handle: u32,
    pub fb: u32,
    /// bytes per row
    pub pitch: u32,
    /// bytes in the buffer
    pub size: u64,
}

impl Handoff {
    /// The value after `--drm=`; None unless all five numbers are there.
    pub fn parse(v: &str) -> Option<Handoff> {
        let mut it = v.split(':');
        let h = Handoff { fd: it.next()?.parse().ok()?, handle: it.next()?.parse().ok()?, fb: it.next()?.parse().ok()?, pitch: it.next()?.parse().ok()?, size: it.next()?.parse().ok()? };
        it.next().is_none().then_some(h)
    }

    pub fn arg(&self) -> String {
        format!("--drm={}:{}:{}:{}:{}", self.fd, self.handle, self.fb, self.pitch, self.size)
    }

    /// A buffer that can be the panel's: real handle and framebuffer, whole 16-bit rows at least
    /// as wide as the panel, enough of them, and no absurd size.
    fn check(&self) -> io::Result<()> {
        let rows = u64::from(self.pitch).checked_mul(u64::from(PANEL_H));
        let bad = if self.handle == 0 {
            "handle 0"
        } else if self.fb == 0 {
            "framebuffer 0"
        } else if !self.pitch.is_multiple_of(2) || self.pitch < u32::from(PANEL_W) * 2 {
            "pitch"
        } else if rows.is_none_or(|r| self.size < r) || self.size > HANDOFF_MAX_SIZE {
            "size"
        } else {
            return Ok(());
        };
        Err(io::Error::other(format!("--drm= {bad}: {self:?}")))
    }
}

/// A DRM card the last image left open for us, owned now (close-on-exec again, closed when
/// dropped); `h` None when the rest of its `--drm=` did not parse.
pub struct Inherited {
    card: File,
    h: Option<Handoff>,
}

impl Inherited {
    /// Give up on the card (closed when the File drops).
    pub fn into_file(self) -> File {
        self.card
    }
}

/// The fd named by `--drm=`'s value, if it is a DRM card: only then is it taken (and so closed if
/// anything later fails). Anything else (stdin/out/err, a pipe, /dev/null, a closed number) is
/// left exactly as it is.
pub fn inherit(v: &str) -> io::Result<Inherited> {
    inherit_if(v, DRM_MAJOR)
}

fn inherit_if(v: &str, major: u32) -> io::Result<Inherited> {
    let fd: i32 = v.split(':').next().and_then(|f| f.parse().ok()).ok_or_else(|| io::Error::other(format!("--drm={v}: no fd")))?;
    if fd <= 2 {
        return Err(io::Error::other(format!("--drm= fd {fd} is a standard stream")));
    }
    // SAFETY: a zeroed stat for fstat to fill
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: fstat on an fd number into our struct
    if unsafe { libc::fstat(fd, &mut st) } != 0 {
        return Err(io::Error::other(format!("--drm= fd {fd}: {}", io::Error::last_os_error())));
    }
    if st.st_mode & libc::S_IFMT != libc::S_IFCHR || libc::major(st.st_rdev) as u32 != major {
        return Err(io::Error::other(format!("--drm= fd {fd} is not a DRM card")));
    }
    // SAFETY: a DRM card the last image left open under this number, which nothing else in this
    // process owns (this runs before anything else opens the card)
    let card = unsafe { File::from_raw_fd(fd) };
    if !set_cloexec(fd, true) {
        eprintln!("drm: fd {fd}: cannot set close-on-exec again");
    }
    Ok(Inherited { card, h: Handoff::parse(v) })
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

    // ---- the handoff ----

    const GOOD: Handoff = Handoff { fd: 7, handle: 1, fb: 40, pitch: 640, size: 640 * 480 };

    #[test]
    fn handoff_arg_round_trip_and_junk() {
        assert_eq!(GOOD.arg(), "--drm=7:1:40:640:307200");
        assert_eq!(Handoff::parse("7:1:40:640:307200"), Some(GOOD));
        for bad in ["", "7", "7:1:40:640", "7:1:40:640:307200:9", "x:1:40:640:307200", "7:-1:40:640:307200", "7:1:40:640:big"] {
            assert_eq!(Handoff::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn handoff_geometry_is_checked() {
        assert!(GOOD.check().is_ok());
        let with = |f: fn(&mut Handoff)| {
            let mut h = GOOD;
            f(&mut h);
            h.check().is_err()
        };
        assert!(with(|h| h.handle = 0));
        assert!(with(|h| h.fb = 0));
        assert!(with(|h| h.pitch = 641));
        assert!(with(|h| h.pitch = 638));
        assert!(with(|h| h.size = 640 * 480 - 1));
        assert!(with(|h| h.size = (64 << 20) + 1));
        assert!(with(|h| h.pitch = u32::MAX - 1));
        assert!(!with(|h| h.size = 64 << 20));
    }

    /// An fd at or above `base`, well clear of what other tests open at the same time (each test
    /// its own base, so a number one closes is not reused by another), close-on-exec.
    fn high(fd: i32, base: i32) -> i32 {
        // SAFETY: dup of an fd we own, then close the original
        unsafe {
            let n = libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, base);
            assert!(n >= base);
            libc::close(fd);
            n
        }
    }

    fn is_open(fd: i32) -> bool {
        // SAFETY: an fd probe
        unsafe { libc::fcntl(fd, libc::F_GETFD) >= 0 }
    }

    fn dev_null(base: i32) -> i32 {
        use std::os::fd::IntoRawFd;
        high(OpenOptions::new().read(true).write(true).open("/dev/null").unwrap().into_raw_fd(), base)
    }

    #[test]
    fn standard_streams_and_strangers_are_left_open() {
        for fd in 0..=2 {
            assert!(inherit(&format!("{fd}:1:40:640:307200")).is_err());
            assert!(is_open(fd), "fd {fd} closed");
        }
        let mut p = [0i32; 2];
        // SAFETY: a pipe for the test
        unsafe { libc::pipe(p.as_mut_ptr()) };
        let (r, w) = (high(p[0], 600), high(p[1], 600));
        assert!(inherit(&format!("{r}:1:40:640:307200")).is_err());
        assert!(is_open(r), "a pipe is not ours to close");
        let null = dev_null(600);
        assert!(inherit(&format!("{null}:1:40:640:307200")).is_err(), "/dev/null is not a DRM card");
        assert!(is_open(null));
        assert!(inherit("not-a-number:1:40:640:307200").is_err());
        assert!(inherit("1999:1:40:640:307200").is_err(), "a closed number");
        // SAFETY: closing the test's own fds
        unsafe {
            libc::close(r);
            libc::close(w);
            libc::close(null);
        }
    }

    #[test]
    fn a_verified_card_is_taken_and_closed_when_adopting_fails() {
        // /dev/null stands in for the card: its major (1) is the one asked for
        let fails_closed = |v: &dyn Fn(i32) -> String| {
            let fd = dev_null(500);
            set_cloexec(fd, false);
            let inh = inherit_if(&v(fd), 1).expect("taken");
            // SAFETY: an fd probe
            let fl = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            assert!(fl >= 0 && fl & libc::FD_CLOEXEC != 0, "close-on-exec again once taken");
            let (_, card) = Drm::adopt(inh).err().expect("adopt fails");
            drop(card);
            !is_open(fd)
        };
        assert!(fails_closed(&|fd| format!("{fd}:1:40:640:307200")), "MAP_DUMB fails on /dev/null");
        assert!(fails_closed(&|fd| format!("{fd}:0:40:640:307200")), "handle 0");
        assert!(fails_closed(&|fd| format!("{fd}:1:0:640:307200")), "fb 0");
        assert!(fails_closed(&|fd| format!("{fd}:1:40:641:307200")), "odd pitch");
        assert!(fails_closed(&|fd| format!("{fd}:1:40:600:307200")), "pitch below the panel");
        assert!(fails_closed(&|fd| format!("{fd}:1:40:640:1000")), "size below the panel");
        assert!(fails_closed(&|fd| format!("{fd}:1:40:4294967294:18446744073709551615")), "a huge pitch and size");
        assert!(fails_closed(&|fd| format!("{fd}:1:40")), "the rest unreadable");
        let fd = dev_null(500);
        drop(inherit_if(&format!("{fd}:1:40:640:307200"), 1).unwrap().into_file());
        assert!(!is_open(fd), "given up: closed");
    }
}
