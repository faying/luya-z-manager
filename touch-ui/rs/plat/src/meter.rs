//! What the spike compares (u60-platform.md §触屏选型 1–6): CPU, wake-ups, RSS, pixels pushed,
//! and how long a change takes to reach the panel.

use std::time::Instant;

#[derive(Clone, Copy, Debug, Default)]
pub struct Proc {
    /// on-CPU time of the whole process, ns (schedstat, else utime+stime)
    pub cpu_ns: u64,
    /// voluntary + involuntary context switches, all threads
    pub switches: u64,
    pub rss_kb: u64,
    pub hwm_kb: u64,
}

fn field(s: &str, key: &str) -> Option<u64> {
    s.lines().find(|l| l.starts_with(key))?.split_whitespace().nth(1)?.parse().ok()
}

/// One look at /proc for this process (every thread, so an engine's helper threads count).
pub fn proc_now() -> Proc {
    let mut p = Proc::default();
    if let Ok(s) = std::fs::read_to_string("/proc/self/status") {
        p.rss_kb = field(&s, "VmRSS:").unwrap_or(0);
        p.hwm_kb = field(&s, "VmHWM:").unwrap_or(0);
    }
    let mut sched = 0u64;
    let mut ticks = 0u64;
    if let Ok(dir) = std::fs::read_dir("/proc/self/task") {
        for t in dir.flatten() {
            let path = t.path();
            if let Ok(s) = std::fs::read_to_string(path.join("schedstat")) {
                sched += s.split_whitespace().next().and_then(|x| x.parse().ok()).unwrap_or(0);
            }
            if let Ok(s) = std::fs::read_to_string(path.join("stat")) {
                // fields after the ")" : state is #3, utime #14, stime #15
                if let Some(rest) = s.rsplit_once(')').map(|x| x.1) {
                    let f: Vec<&str> = rest.split_whitespace().collect();
                    ticks += f.get(11).and_then(|x| x.parse::<u64>().ok()).unwrap_or(0)
                        + f.get(12).and_then(|x| x.parse::<u64>().ok()).unwrap_or(0);
                }
            }
            if let Ok(s) = std::fs::read_to_string(path.join("status")) {
                p.switches += field(&s, "voluntary_ctxt_switches:").unwrap_or(0)
                    + field(&s, "nonvoluntary_ctxt_switches:").unwrap_or(0);
            }
        }
    }
    // SAFETY: plain sysconf
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
    p.cpu_ns = if sched > 0 { sched } else { ticks * 1_000_000_000 / hz };
    p
}

/// Change-to-panel latencies of one phase, ms.
#[derive(Default)]
pub struct Lat {
    pending: Option<Instant>,
    pub ms: Vec<f64>,
}

impl Lat {
    /// Something the user would see just changed (data applied, finger down).
    pub fn mark(&mut self, at: Instant) {
        self.pending.get_or_insert(at);
    }

    /// Pixels just reached the panel.
    pub fn pushed(&mut self, at: Instant) {
        if let Some(t) = self.pending.take() {
            self.ms.push(at.duration_since(t).as_secs_f64() * 1e3);
        }
    }

    pub fn pct(&self, q: f64) -> Option<f64> {
        if self.ms.is_empty() {
            return None;
        }
        let mut v = self.ms.clone();
        v.sort_by(|a, b| a.total_cmp(b));
        Some(v[((v.len() - 1) as f64 * q).round() as usize])
    }
}

/// One phase's line in the report.
pub struct Report<'a> {
    pub engine: &'a str,
    pub phase: &'a str,
    pub secs: f64,
    pub a: Proc,
    pub b: Proc,
    pub frames: u64,
    /// rectangles the engine drew (one frame may have several)
    pub rects: u64,
    pub px: u64,
    pub lat: &'a Lat,
    /// time spent inside the engine (timers, layout, drawing), ms
    pub engine_ms: f64,
}

impl Report<'_> {
    pub fn json(&self) -> String {
        let s = self.secs.max(1e-9);
        let o = |x: Option<f64>| x.map_or("null".into(), |v| format!("{v:.1}"));
        format!(
            "{{\"engine\":\"{}\",\"phase\":\"{}\",\"secs\":{:.1},\"cpu_pct\":{:.2},\"wakeups_s\":{:.1},\"rss_kb\":{},\"hwm_kb\":{},\"frames\":{},\"rects\":{},\"px\":{},\"px_s\":{:.0},\"engine_ms\":{:.0},\"lat_n\":{},\"lat_p50\":{},\"lat_p99\":{},\"lat_max\":{}}}",
            self.engine,
            self.phase,
            self.secs,
            (self.b.cpu_ns - self.a.cpu_ns) as f64 / (s * 1e7),
            (self.b.switches - self.a.switches) as f64 / s,
            self.b.rss_kb,
            self.b.hwm_kb,
            self.frames,
            self.rects,
            self.px,
            self.px as f64 / s,
            self.engine_ms,
            self.lat.ms.len(),
            o(self.lat.pct(0.5)),
            o(self.lat.pct(0.99)),
            o(self.lat.pct(1.0)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latency_counts_from_the_first_mark() {
        let t0 = Instant::now();
        let mut l = Lat::default();
        l.pushed(t0); // nothing pending
        l.mark(t0);
        l.mark(t0 + std::time::Duration::from_millis(5)); // still the first
        l.pushed(t0 + std::time::Duration::from_millis(20));
        assert_eq!(l.ms.len(), 1);
        assert!((l.ms[0] - 20.0).abs() < 0.01);
        assert_eq!(l.pct(0.5), Some(l.ms[0]));
    }

    #[test]
    fn proc_reads_something() {
        let p = proc_now();
        assert!(p.rss_kb > 0 && p.cpu_ns > 0);
    }
}
