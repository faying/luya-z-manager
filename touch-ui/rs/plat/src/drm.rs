//! A DRM dumb buffer (RGB565) with DIRTYFB per pushed rectangle: src/drm_disp.c in Rust,
//! raw ioctls only (the device has no libdrm we could link statically).

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;

const CARD: &str = "/dev/dri/card0";
const FALLBACK_CONNECTOR: u32 = 31;
const FALLBACK_CRTC: u32 = 34;

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

const fn iowr<T>(nr: u64) -> u64 {
    (3 << 30) | ((std::mem::size_of::<T>() as u64) << 16) | ((b'd' as u64) << 8) | nr
}

fn ioctl<T>(fd: i32, req: u64, arg: &mut T) -> std::io::Result<()> {
    loop {
        // SAFETY: `arg` is the #[repr(C)] struct the kernel expects for `req`.
        let r = unsafe { libc::ioctl(fd, req as _, arg as *mut T) };
        if r == 0 {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

pub struct Drm {
    file: File,
    map: *mut u16,
    map_len: usize,
    fb_id: u32,
    pub width: usize,
    pub height: usize,
    pub pitch_px: usize,
}

fn pick(fd: i32) -> std::io::Result<(u32, u32, ModeInfo)> {
    let mut res = CardRes::default();
    ioctl(fd, iowr::<CardRes>(0xA0), &mut res)?;
    let mut crtcs = vec![0u32; res.count_crtcs as usize];
    let mut conns = vec![0u32; res.count_connectors as usize];
    let mut encs = vec![0u32; res.count_encoders as usize];
    res.crtc_id_ptr = crtcs.as_mut_ptr() as u64;
    res.connector_id_ptr = conns.as_mut_ptr() as u64;
    res.encoder_id_ptr = encs.as_mut_ptr() as u64;
    res.count_fbs = 0;
    ioctl(fd, iowr::<CardRes>(0xA0), &mut res)?;
    for &cid in &conns {
        let mut c = GetConnector { connector_id: cid, ..Default::default() };
        if ioctl(fd, iowr::<GetConnector>(0xA7), &mut c).is_err() || c.connection != 1 || c.count_modes == 0 {
            continue;
        }
        let mut modes = vec![ModeInfo::default(); c.count_modes as usize];
        let mut cencs = vec![0u32; c.count_encoders as usize];
        c.modes_ptr = modes.as_mut_ptr() as u64;
        c.encoders_ptr = cencs.as_mut_ptr() as u64;
        c.count_props = 0;
        if ioctl(fd, iowr::<GetConnector>(0xA7), &mut c).is_err() || c.count_modes == 0 {
            continue;
        }
        let mut crtc = 0;
        if c.encoder_id != 0 {
            let mut e = GetEncoder { encoder_id: c.encoder_id, ..Default::default() };
            if ioctl(fd, iowr::<GetEncoder>(0xA6), &mut e).is_ok() {
                crtc = e.crtc_id;
            }
        }
        for &eid in &cencs {
            if crtc != 0 {
                break;
            }
            let mut e = GetEncoder { encoder_id: eid, ..Default::default() };
            if ioctl(fd, iowr::<GetEncoder>(0xA6), &mut e).is_ok() {
                crtc = crtcs.iter().enumerate().find(|(i, _)| e.possible_crtcs & (1 << i) != 0).map_or(0, |(_, c)| *c);
            }
        }
        if crtc == 0 {
            crtc = crtcs.first().copied().unwrap_or(0);
        }
        if crtc != 0 {
            return Ok((cid, crtc, modes[0]));
        }
    }
    Err(std::io::Error::other("no connected connector"))
}

impl Drm {
    pub fn open() -> std::io::Result<Drm> {
        let file = OpenOptions::new().read(true).write(true).open(CARD)?;
        let fd = file.as_raw_fd();
        let (conn, crtc, mode) = pick(fd).unwrap_or_else(|e| {
            eprintln!("drm: {e}, fallback connector {FALLBACK_CONNECTOR} crtc {FALLBACK_CRTC}");
            let mut m = ModeInfo { clock: 1, hdisplay: 320, vdisplay: 480, vrefresh: 1, kind: 0x48, ..Default::default() };
            (m.hsync_start, m.hsync_end, m.htotal) = (320, 320, 320);
            (m.vsync_start, m.vsync_end, m.vtotal) = (480, 480, 480);
            m.name[..7].copy_from_slice(b"320x480");
            (FALLBACK_CONNECTOR, FALLBACK_CRTC, m)
        });
        let (w, h) = (mode.hdisplay as u32, mode.vdisplay as u32);
        let mut dumb = CreateDumb { width: w, height: h, bpp: 16, ..Default::default() };
        ioctl(fd, iowr::<CreateDumb>(0xB2), &mut dumb)?;
        let mut fb = FbCmd { width: w, height: h, pitch: dumb.pitch, bpp: 16, depth: 16, handle: dumb.handle, ..Default::default() };
        ioctl(fd, iowr::<FbCmd>(0xAE), &mut fb)?;
        let mut map = MapDumb { handle: dumb.handle, ..Default::default() };
        ioctl(fd, iowr::<MapDumb>(0xB3), &mut map)?;
        // SAFETY: the kernel handed out this offset for a mapping of `size` bytes on `fd`.
        let p = unsafe {
            libc::mmap(std::ptr::null_mut(), dumb.size as usize, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, map.offset as libc::off_t)
        };
        if p == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: freshly mapped, `size` bytes.
        unsafe { std::ptr::write_bytes(p as *mut u8, 0, dumb.size as usize) };
        let mut conn_id = conn;
        let mut set = Crtc {
            set_connectors_ptr: &mut conn_id as *mut u32 as u64,
            count_connectors: 1,
            crtc_id: crtc,
            fb_id: fb.fb_id,
            mode_valid: 1,
            mode,
            ..Default::default()
        };
        ioctl(fd, iowr::<Crtc>(0xA2), &mut set)?;
        eprintln!("drm: connector {conn} crtc {crtc} {w}x{h} pitch {}", dumb.pitch);
        let d = Drm {
            file,
            map: p as *mut u16,
            map_len: dumb.size as usize,
            fb_id: fb.fb_id,
            width: w as usize,
            height: h as usize,
            pitch_px: dumb.pitch as usize / 2,
        };
        d.dirty(0, 0, d.width, d.height);
        Ok(d)
    }

    pub fn pixels(&mut self) -> &mut [u16] {
        // SAFETY: the mapping lives as long as self and is map_len bytes.
        unsafe { std::slice::from_raw_parts_mut(self.map, self.map_len / 2) }
    }

    /// The visible rows (width × height, without the pitch's padding).
    pub fn rows(&self) -> Vec<u16> {
        // SAFETY: the mapping lives as long as self and is map_len bytes
        let all = unsafe { std::slice::from_raw_parts(self.map, self.map_len / 2) };
        (0..self.height).flat_map(|y| all[y * self.pitch_px..y * self.pitch_px + self.width].iter().copied()).collect()
    }

    /// Push [x1,x2)×[y1,y2) to the panel. Some drivers lack DIRTYFB: errors are ignored.
    pub fn dirty(&self, x1: usize, y1: usize, x2: usize, y2: usize) {
        let mut clip = ClipRect { x1: x1 as u16, y1: y1 as u16, x2: x2 as u16, y2: y2 as u16 };
        let mut cmd = FbDirty { fb_id: self.fb_id, flags: 0, color: 0, num_clips: 1, clips_ptr: &mut clip as *mut ClipRect as u64 };
        let _ = ioctl(self.file.as_raw_fd(), iowr::<FbDirty>(0xB1), &mut cmd);
    }
}

impl Drop for Drm {
    fn drop(&mut self) {
        // SAFETY: our own mapping.
        unsafe { libc::munmap(self.map as *mut libc::c_void, self.map_len) };
    }
}
