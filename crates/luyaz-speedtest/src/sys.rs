//! CPU readings shown beside the verdict: each core's busy share (`/proc/stat`) and mihomo's
//! CPU time (`/proc/<pid>/stat`). Missing files (not Linux) give `None`, never an error.
use std::time::Instant;

/// Per core: (busy, total) jiffies.
pub(crate) struct Cores(Vec<(u64, u64)>);
pub(crate) fn cores() -> Option<Cores> {
    let text = std::fs::read_to_string("/proc/stat").ok()?;
    let v: Vec<(u64, u64)> = text
        .lines()
        .filter(|l| l.starts_with("cpu") && l.as_bytes().get(3).is_some_and(u8::is_ascii_digit))
        .map(|l| {
            let n: Vec<u64> = l
                .split_whitespace()
                .skip(1)
                .filter_map(|x| x.parse().ok())
                .collect();
            let total: u64 = n.iter().sum();
            // idle + iowait
            let idle = n.get(3).copied().unwrap_or(0) + n.get(4).copied().unwrap_or(0);
            (total.saturating_sub(idle), total)
        })
        .collect();
    (!v.is_empty()).then_some(Cores(v))
}
/// The busiest core between two readings, in percent.
pub(crate) fn peak(a: &Cores, b: &Cores) -> Option<f64> {
    a.0.iter()
        .zip(b.0.iter())
        .filter_map(|(x, y)| {
            let total = y.1.checked_sub(x.1)?;
            let busy = y.0.checked_sub(x.0)?;
            (total > 0).then(|| busy as f64 * 100.0 / total as f64)
        })
        .reduce(f64::max)
}

/// mihomo's utime + stime, in clock ticks, with when it was read.
pub(crate) struct Ticks(u64, Instant);
pub(crate) fn mihomo() -> Option<Ticks> {
    for e in std::fs::read_dir("/proc").ok()?.flatten() {
        let name = e.file_name();
        let Some(pid) = name
            .to_str()
            .filter(|s| s.bytes().all(|b| b.is_ascii_digit()))
        else {
            continue;
        };
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
        if comm.trim() != "mihomo" {
            continue;
        }
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // fields after "(comm)": state is field 3; utime/stime are fields 14/15
        let rest = stat.rsplit_once(')')?.1;
        let f: Vec<&str> = rest.split_whitespace().collect();
        let utime: u64 = f.get(11)?.parse().ok()?;
        let stime: u64 = f.get(12)?.parse().ok()?;
        return Some(Ticks(utime + stime, Instant::now()));
    }
    None
}
/// CPU between two readings, 100% = one core (USER_HZ 100 on this kernel).
pub(crate) fn cpu_pct(a: &Ticks, b: &Ticks) -> Option<f64> {
    let secs = b.1.duration_since(a.1).as_secs_f64();
    let ticks = b.0.checked_sub(a.0)?;
    (secs > 0.0).then(|| ticks as f64 / 100.0 / secs * 100.0)
}
