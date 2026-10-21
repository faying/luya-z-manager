//! Linux input devices, the little of them the panel needs: find the /dev/input/event* nodes, ask
//! one what it can report, read what it has without blocking.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};

pub(crate) const EV_SYN: u16 = 0x00;
pub(crate) const EV_KEY: u16 = 0x01;
pub(crate) const EV_ABS: u16 = 0x03;

/// One `struct input_event` as aarch64 lays it out: a 16-byte timeval, then type, code, value.
const EVENT_BYTES: usize = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Event {
    pub kind: u16,
    pub code: u16,
    pub value: i32,
}

/// `struct input_absinfo`
#[repr(C)]
#[derive(Default)]
pub(crate) struct AbsInfo {
    pub value: i32,
    pub minimum: i32,
    pub maximum: i32,
    pub fuzz: i32,
    pub flat: i32,
    pub resolution: i32,
}

/// _IOR('E', nr, len)
const fn ior(nr: u64, len: usize) -> u64 {
    (2 << 30) | ((len as u64) << 16) | (0x45 << 8) | nr
}

/// Bit `n` of a kernel bitmap.
pub(crate) fn has_bit(map: &[u64], n: usize) -> bool {
    map.get(n / 64).is_some_and(|word| word & (1 << (n % 64)) != 0)
}

/// event0, event1, … as the directory lists them.
pub(crate) fn nodes() -> Vec<PathBuf> {
    let Ok(dir) = std::fs::read_dir("/dev/input") else { return Vec::new() };
    dir.flatten().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|n| n.as_encoded_bytes().starts_with(b"event"))).collect()
}

pub(crate) struct Device {
    fd: OwnedFd,
}

impl Device {
    pub fn open(path: &Path) -> Option<Device> {
        let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).ok()?;
        // SAFETY: a NUL-terminated path; the fd returned is ours to own
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC) };
        // SAFETY: a fresh fd nobody else holds
        (fd >= 0).then(|| Device { fd: unsafe { OwnedFd::from_raw_fd(fd) } })
    }

    pub fn into_fd(self) -> OwnedFd {
        self.fd
    }

    fn ioctl_into<T>(&self, nr: u64, out: &mut T) -> bool {
        // SAFETY: the kernel writes at most size_of::<T>() bytes, the length encoded in the request.
        // EVIOCGBIT answers with the number of bytes copied, the others with 0: only < 0 is a failure.
        unsafe { libc::ioctl(self.fd.as_raw_fd(), ior(nr, size_of::<T>()) as _, out as *mut T) >= 0 }
    }

    /// EVIOCGBIT(kind): which codes of `kind` it reports (kind 0: which event types). `W` words.
    pub fn codes<const W: usize>(&self, kind: u16) -> Option<[u64; W]> {
        let mut map = [0u64; W];
        self.ioctl_into(0x20 + kind as u64, &mut map).then_some(map)
    }

    /// EVIOCGABS(axis)
    pub fn axis(&self, axis: u16) -> Option<AbsInfo> {
        let mut info = AbsInfo::default();
        self.ioctl_into(0x40 + axis as u64, &mut info).then_some(info)
    }

    /// EVIOCGMTSLOTS for `axis`: its value in each of the first 16 slots.
    pub fn slot_values(&self, axis: u16) -> Option<[i32; 16]> {
        let mut q = [0i32; 17];
        q[0] = axis as i32;
        self.ioctl_into(0x0a, &mut q).then(|| {
            let mut v = [0; 16];
            v.copy_from_slice(&q[1..]);
            v
        })
    }
}

/// Everything waiting on a non-blocking input fd, oldest first.
pub(crate) fn drain(fd: i32, mut each: impl FnMut(Event)) {
    let mut buf = [0u8; EVENT_BYTES];
    loop {
        // SAFETY: reads at most EVENT_BYTES into our own buffer
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), EVENT_BYTES) };
        if n != EVENT_BYTES as isize {
            return;
        }
        each(Event {
            kind: u16::from_ne_bytes([buf[16], buf[17]]),
            code: u16::from_ne_bytes([buf[18], buf[19]]),
            value: i32::from_ne_bytes([buf[20], buf[21], buf[22], buf[23]]),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_and_requests() {
        let map = [1u64 << 0x35, 1 << 2];
        assert!(has_bit(&map, 0x35) && !has_bit(&map, 0x36));
        assert!(has_bit(&map, 66) && !has_bit(&map, 200));
        // EVIOCGABS(ABS_MT_POSITION_X) as the kernel headers spell it
        assert_eq!(ior(0x40 + 0x35, 24), 0x8018_4575);
    }
}
