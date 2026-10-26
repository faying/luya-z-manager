//! Heap counters (Phase 5: the core's RSS creeps up between restarts). A counting wrapper over
//! the heap allocator keeps the bytes handed out and not yet freed (`live`) and their high mark
//! (`peak`), so RSS growth can be told apart: `live` climbing too = something holds on to memory;
//! `live` flat while RSS climbs = the allocator keeps freed memory.
//!
//! The allocator is mimalloc. On musl's mallocng, `live` stayed at 8 MB and `peak` at 18 MB
//! over 8 h while RssAnon went 33 → 82 MB, its pages full of stale copies of buffers already
//! freed (10-24, u60g); mallocng keeps a size class's partly used groups, so the churn of
//! differently sized reads never went back to the OS.
//!
//! Each time `live` first reaches the next [`STEP`] mark, one line goes to stderr (the core log)
//! with the thread that allocated and the last HTTP path seen, so a jump points at its cause.
//! Nothing here allocates or takes a lock: the line is built on the stack and written with
//! `write(2)`; no thread, no timer.

use mimalloc::MiMalloc;
use std::alloc::{GlobalAlloc, Layout};
use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering::Relaxed};

/// One log line per this much new high mark.
const STEP: usize = 4 << 20;
const PATH_MAX: usize = 48;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static NEXT: AtomicUsize = AtomicUsize::new(STEP);
static PATH: [AtomicU8; PATH_MAX] = [const { AtomicU8::new(0) }; PATH_MAX];
static PATH_LEN: AtomicUsize = AtomicUsize::new(0);
/// CLOCK_MONOTONIC seconds when the last path was noted.
static PATH_T: AtomicU64 = AtomicU64::new(0);

pub struct Counting;

// SAFETY: every call goes straight to mimalloc; the counters only look at the sizes.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { MiMalloc.alloc(l) };
        if !p.is_null() {
            grow(l.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = unsafe { MiMalloc.alloc_zeroed(l) };
        if !p.is_null() {
            grow(l.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { MiMalloc.dealloc(p, l) };
        LIVE.fetch_sub(l.size(), Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = unsafe { MiMalloc.realloc(p, l, new) };
        if !q.is_null() {
            if new >= l.size() {
                grow(new - l.size());
            } else {
                LIVE.fetch_sub(l.size() - new, Relaxed);
            }
        }
        q
    }
}

fn grow(n: usize) {
    let live = LIVE.fetch_add(n, Relaxed) + n;
    PEAK.fetch_max(live, Relaxed);
    let next = NEXT.load(Relaxed);
    if live >= next
        && NEXT
            .compare_exchange(next, (live / STEP + 1) * STEP, Relaxed, Relaxed)
            .is_ok()
    {
        mark(live);
    }
}

/// (live, peak) in kB.
pub fn stats() -> (i64, i64) {
    (
        (LIVE.load(Relaxed) >> 10) as i64,
        (PEAK.load(Relaxed) >> 10) as i64,
    )
}

/// Note the path of a request being served (the server's first layer).
pub fn note_path(path: &str) {
    let b = path.as_bytes();
    let n = b.len().min(PATH_MAX);
    for (slot, &c) in PATH.iter().zip(&b[..n]) {
        slot.store(c, Relaxed);
    }
    PATH_LEN.store(n, Relaxed);
    PATH_T.store(mono_s(), Relaxed);
}

fn mono_s() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: a valid out pointer; clock_gettime does not allocate.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64
}

/// `[mem] live 40960 kB; thread tokio-runtime-w; last /v2/scorecard 3s ago`
fn mark(live: usize) {
    let mut buf = Line { b: [0; 192], n: 0 };
    buf.put(b"[mem] live ");
    buf.num((live >> 10) as u64);
    buf.put(b" kB; thread ");
    let mut comm = [0u8; 17];
    // SAFETY: PR_GET_NAME writes at most 16 bytes plus NUL into the buffer.
    unsafe { libc::prctl(libc::PR_GET_NAME, comm.as_mut_ptr()) };
    let cn = comm.iter().position(|&c| c == 0).unwrap_or(16);
    buf.put(&comm[..cn]);
    let n = PATH_LEN.load(Relaxed);
    if n > 0 {
        buf.put(b"; last ");
        for slot in &PATH[..n] {
            buf.put(&[slot.load(Relaxed)]);
        }
        buf.put(b" ");
        buf.num(mono_s().saturating_sub(PATH_T.load(Relaxed)));
        buf.put(b"s ago");
    }
    buf.put(b"\n");
    // SAFETY: writes the stack buffer to stderr; a short or failed write is dropped.
    unsafe { libc::write(2, buf.b.as_ptr().cast(), buf.n) };
}

struct Line {
    b: [u8; 192],
    n: usize,
}

impl Line {
    fn put(&mut self, s: &[u8]) {
        let k = s.len().min(self.b.len() - self.n);
        self.b[self.n..self.n + k].copy_from_slice(&s[..k]);
        self.n += k;
    }
    fn num(&mut self, mut v: u64) {
        let mut d = [0u8; 20];
        let mut i = d.len();
        loop {
            i -= 1;
            d[i] = b'0' + (v % 10) as u8;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        self.put(&d[i..]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_numbers_and_cap() {
        let mut l = Line { b: [0; 192], n: 0 };
        l.num(0);
        l.put(b" ");
        l.num(40960);
        assert_eq!(&l.b[..l.n], b"0 40960");
        for _ in 0..100 {
            l.put(b"xxxx");
        }
        assert_eq!(l.n, 192);
    }

    #[test]
    fn counts_live_bytes() {
        let v = vec![1u8; 8 << 20];
        let (_, peak) = stats();
        assert!(peak >= 8 << 10, "peak {peak} kB");
        drop(v);
    }
}
