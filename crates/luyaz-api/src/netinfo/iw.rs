//! The `iw` station read, shared by u60d's `/clients` + netinfo refresh and the deep diagnosis
//! (resilience audit R6). Each `iw` has its own deadline and is killed as a process group past
//! it, and at most one read runs at a time: a caller that finds one going gets the last result
//! instead of starting another, so a wedged driver costs one blocked thread, not one per poll.

use super::{Station, ap_ifaces, parse_ap_info, parse_station_dump};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Per `iw` call. A healthy `iw dev … station dump` answers in milliseconds (nl80211 is local);
/// 2 s only cuts off a driver that stopped answering. A full read is 1 + 2 × APs calls.
pub const IW_TIMEOUT: Duration = Duration::from_secs(2);

/// One in-flight run; the rest get the last result.
pub struct SingleFlight<T> {
    busy: AtomicBool,
    last: Mutex<T>,
}

impl<T: Clone + Default> SingleFlight<T> {
    pub const fn new(empty: T) -> Self {
        SingleFlight {
            busy: AtomicBool::new(false),
            last: Mutex::new(empty),
        }
    }
    /// `f()` if nothing is running (its result is kept), else the last kept result.
    pub fn run(&self, f: impl FnOnce() -> T) -> T {
        if self.busy.swap(true, Ordering::SeqCst) {
            return self.last.lock().unwrap_or_else(|e| e.into_inner()).clone();
        }
        /// Frees the slot even if `f` panics.
        struct Free<'a>(&'a AtomicBool);
        impl Drop for Free<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        let _free = Free(&self.busy);
        let v = f();
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = v.clone();
        v
    }
}

static STATIONS: SingleFlight<Vec<Station>> = SingleFlight::new(Vec::new());

/// `sh -c cmd`'s stdout, "" on failure or past `limit` (the group is killed).
pub fn sh_within(cmd: &str, limit: Duration) -> String {
    crate::health::output_within_group(Command::new("sh").args(["-c", cmd]), limit)
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Every station on every AP interface, with its interface's band / channel / width, from `sh`.
pub fn stations_with(sh: impl Fn(&str) -> String) -> Vec<Station> {
    let mut out = Vec::new();
    for iface in ap_ifaces(&sh("iw dev 2>/dev/null")) {
        let ap = parse_ap_info(&sh(&format!("iw dev {iface} info 2>/dev/null")));
        for mut st in parse_station_dump(
            &sh(&format!("iw dev {iface} station dump 2>/dev/null")),
            &iface,
        ) {
            st.band = ap.band;
            st.channel = ap.channel;
            st.width_mhz = ap.width_mhz;
            out.push(st);
        }
    }
    out
}

/// The device's stations: bounded `iw`, single-flight. Blocking.
pub fn stations() -> Vec<Station> {
    STATIONS.run(|| stations_with(|c| sh_within(c, IW_TIMEOUT)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    #[test]
    fn a_hung_iw_is_cut_off() {
        let t0 = Instant::now();
        let got = stations_with(|c| {
            assert!(c.starts_with("iw "));
            sh_within("sleep 3600", Duration::from_millis(200))
        });
        assert!(got.is_empty());
        assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
    }

    #[test]
    fn concurrent_reads_start_one_run() {
        let sf = Arc::new(SingleFlight::new(vec![0u32]));
        let runs = Arc::new(AtomicUsize::new(0));
        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
        let first = {
            let (sf, runs) = (sf.clone(), runs.clone());
            std::thread::spawn(move || {
                sf.run(|| {
                    runs.fetch_add(1, Ordering::SeqCst);
                    go_rx.recv().unwrap(); // "hung" until released
                    vec![7]
                })
            })
        };
        while runs.load(Ordering::SeqCst) == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
        // 20 more while it hangs: none starts a run, each returns the last result at once.
        for _ in 0..20 {
            let t0 = Instant::now();
            assert_eq!(
                sf.run(|| {
                    runs.fetch_add(1, Ordering::SeqCst);
                    vec![9]
                }),
                vec![0]
            );
            assert!(t0.elapsed() < Duration::from_millis(100));
        }
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        go_tx.send(()).unwrap();
        assert_eq!(first.join().unwrap(), vec![7]);
        // slot released: the next call runs again and its result is kept.
        assert_eq!(sf.run(|| vec![8]), vec![8]);
        assert_eq!(sf.run(|| vec![1]), vec![1]);
    }
}
