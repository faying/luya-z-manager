//! Bounded read-only jobs. LVGL keeps the last completed state and never waits on I/O.
//! A local action invalidates an older read; workers never touch LVGL or device input.
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use std::ops::{Deref, DerefMut};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::{mpsc, Arc, Mutex, OnceLock};

type Job = Box<dyn FnOnce() + Send>;

/// Three persistent readers, at most sixteen queued jobs. Each cache permits one job.
/// No thread per refresh, no unbounded queue, and no worker lock held during I/O.
fn queue() -> &'static mpsc::SyncSender<Job> {
    static QUEUE: OnceLock<mpsc::SyncSender<Job>> = OnceLock::new();
    QUEUE.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel::<Job>(16);
        let rx = Arc::new(Mutex::new(rx));
        for i in 0..3 {
            let rx = rx.clone();
            std::thread::Builder::new().name(format!("ui-read-{i}")).spawn(move || loop {
                let job = rx.lock().unwrap().recv();
                match job {
                    Ok(job) => job(),
                    Err(_) => break,
                }
            }).expect("UI read worker");
        }
        tx
    })
}

/// Workers wake the sleeping UI only for completed changes, never for idle cache checks.
/// CLOEXEC keeps this process-local notification out of a theme/language restart.
pub fn wake_fd() -> i32 {
    static WAKE: OnceLock<Option<OwnedFd>> = OnceLock::new();
    WAKE.get_or_init(|| {
        // SAFETY: a new eventfd, owned here for the process lifetime.
        let fd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd) })
    }).as_ref().map_or(-1, AsRawFd::as_raw_fd)
}
fn wake() {
    let one = 1u64;
    // SAFETY: eventfd accepts exactly eight bytes; nonblocking, errors need no retry.
    unsafe { libc::write(wake_fd(), (&one as *const u64).cast(), 8); }
}
pub fn take_wake() -> bool {
    let mut n = 0u64;
    // SAFETY: eight writable bytes, nonblocking eventfd; a read drains the counter.
    unsafe { libc::read(wake_fd(), (&mut n as *mut u64).cast(), 8) == 8 }
}

/// The pending result is bounded too; dropping the UI never joins a slow reader.
pub struct Task<T>(mpsc::Receiver<T>);
impl<T: Send + 'static> Task<T> {
    pub fn start(f: impl FnOnce() -> T + Send + 'static) -> Option<Self> {
        Self::start_notify(f, |_| false)
    }
    pub fn start_notify(f: impl FnOnce() -> T + Send + 'static, notify: impl FnOnce(&T) -> bool + Send + 'static) -> Option<Self> {
        let (tx, rx) = mpsc::sync_channel(1);
        let en = crate::lang::is_en();
        queue().try_send(Box::new(move || {
            // Tests keep language thread-local; production chooses it once per process.
            #[cfg(test)]
            crate::lang::set_en(en);
            #[cfg(not(test))]
            let _ = en;
            let result = f();
            let changed = notify(&result);
            if tx.send(result).is_ok() && changed { wake(); }
        })).ok()?;
        Some(Self(rx))
    }
    pub fn take(&self) -> Result<T, mpsc::TryRecvError> {
        self.0.try_recv()
    }
}

pub trait ReadCopy: Send + Sized + 'static {
    fn read_copy(&self) -> Self;
    fn apply(&mut self, next: Self) { *self = next; }
}
macro_rules! copies {
    ($($ty:ty),+ $(,)?) => {$(impl ReadCopy for $ty {
        fn read_copy(&self) -> Self { self.clone() }
    })+};
}
copies!(super::scenario::Scenario, super::netinfo::Netinfo, super::alerts::Alerts,
    super::esim::Esim, super::speedtest::Speedtest, super::tailscale::Tailscale,
    super::diagnose::Diagnose, super::battery::BatteryEst, super::versions::Versions,
    super::landing::Landing, super::screen_feed::ScreenFeed);

pub struct Cached<T> {
    value: T,
    revision: u64,
    pending: Option<(u64, Task<(T, u32)>)>,
}
impl<T> Cached<T> {
    pub fn new(value: T) -> Self { Self { value, revision: 0, pending: None } }
}
impl<T: ReadCopy> Cached<T> {
    /// Collect and then schedule, without waiting. Bits retain every poll's repaint signal.
    pub fn poll(&mut self, f: impl FnOnce(&mut T) -> u32 + Send + 'static) -> u32 {
        let mut bits = 0;
        if let Some((revision, task)) = &self.pending {
            match task.take() {
                Ok((next, changed)) => {
                    if *revision == self.revision {
                        self.value.apply(next);
                        bits = changed;
                    }
                    self.pending = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => self.pending = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.pending.is_none() {
            let mut next = self.value.read_copy();
            if let Some(task) = Task::start_notify(move || {
                let bits = f(&mut next);
                (next, bits)
            }, |(_, bits)| *bits != 0) {
                self.pending = Some((self.revision, task));
            }
        }
        bits
    }
}
impl<T> Deref for Cached<T> {
    type Target = T;
    fn deref(&self) -> &T { &self.value }
}
impl<T> DerefMut for Cached<T> {
    fn deref_mut(&mut self) -> &mut T {
        self.revision = self.revision.wrapping_add(1);
        &mut self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    #[derive(Clone)]
    struct State(u32);
    impl ReadCopy for State { fn read_copy(&self) -> Self { self.clone() } }

    #[test]
    fn a_slow_read_never_waits_or_overwrites_an_action() {
        let mut cache = Cached::new(State(7));
        let (entered, started) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let at = Instant::now();
        assert_eq!(cache.poll(move |s| {
            entered.send(()).unwrap();
            wait.recv().unwrap();
            s.0 = 99;
            1
        }), 0);
        assert!(at.elapsed() < Duration::from_millis(100));
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        for _ in 0..100 { assert_eq!(cache.poll(|_| panic!("duplicate read")), 0); }
        assert_eq!(cache.0, 7, "keep the last completed state while pending");
        cache.0 = 8; // local action: invalidate the older result
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let bits = cache.poll(|s| { s.0 += 1; 2 });
            assert_eq!(bits & 1, 0, "discard the invalidated read repaint as well");
            assert_ne!(cache.0, 99);
            if cache.0 == 9 { break; }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
