//! Pure parsers for the /proc and sysfs files the ledger reads. No IO here.

/// `/proc/<pid>/stat` fields the ledger needs. comm may contain spaces and ')'.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PidStat {
    pub comm: String,
    pub ppid: u32,
    /// utime + stime, clock ticks (all threads of the process)
    pub cpu_ticks: u64,
    /// clock ticks after boot
    pub starttime: u64,
}

pub fn pid_stat(s: &str) -> Option<PidStat> {
    let open = s.find('(')?;
    let close = s.rfind(')')?;
    let comm = s.get(open + 1..close)?.to_string();
    let rest: Vec<&str> = s.get(close + 1..)?.split_ascii_whitespace().collect();
    // rest[0] = state (field 3); field n is rest[n - 3]
    let f = |n: usize| rest.get(n - 3).and_then(|v| v.parse::<u64>().ok());
    Some(PidStat {
        comm,
        ppid: f(4)? as u32,
        cpu_ticks: f(14)? + f(15)?,
        starttime: f(22)?,
    })
}

/// Third field of `/proc/<pid>/task/<tid>/schedstat`: times the thread got a CPU.
/// Equals voluntary + nonvoluntary switches within 1 per thread (measured on the
/// device 10-06: 0.02 % apart machine-wide) and is a 30-byte file instead of a
/// 1.3 KB one, so the ledger reads this for the 0c wakeup count.
pub fn schedstat_runs(s: &str) -> Option<u64> {
    s.split_ascii_whitespace().nth(2)?.parse().ok()
}

/// Process identity that survives restarts: argv[0] plus the first argument that is
/// not an option (usually the script path), capped at 80 bytes. Kernel threads
/// (empty cmdline) are `[comm]`.
pub fn proc_key(cmdline: &[u8], comm: &str) -> String {
    let args: Vec<&str> = cmdline
        .split(|&b| b == 0)
        .filter(|a| !a.is_empty())
        .filter_map(|a| std::str::from_utf8(a).ok())
        .collect();
    let Some(first) = args.first() else {
        return format!("[{comm}]");
    };
    let mut key = first.to_string();
    if let Some(a) = args.iter().skip(1).find(|a| !a.starts_with('-')) {
        key.push(' ');
        key.push_str(a);
    }
    if key.len() > 80 {
        let mut cut = 80;
        while !key.is_char_boundary(cut) {
            cut -= 1;
        }
        key.truncate(cut);
    }
    key
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SysStat {
    /// user+nice+system+irq+softirq+steal (power-sample `busy`)
    pub busy: u64,
    pub total: u64,
    pub ctxt: u64,
    pub intr: u64,
    /// clone() calls since boot, threads included — the canonical "new processes" count
    pub processes: u64,
}

pub fn sys_stat(s: &str) -> SysStat {
    let mut st = SysStat::default();
    for line in s.lines() {
        let mut it = line.split_ascii_whitespace();
        match it.next() {
            Some("cpu") => {
                let v: Vec<u64> = it.filter_map(|x| x.parse().ok()).collect();
                // user nice system idle iowait irq softirq steal …
                let g = |i: usize| v.get(i).copied().unwrap_or(0);
                st.busy = g(0) + g(1) + g(2) + g(5) + g(6) + g(7);
                st.total = v.iter().sum();
            }
            Some("ctxt") => st.ctxt = it.next().and_then(|x| x.parse().ok()).unwrap_or(0),
            Some("intr") => st.intr = it.next().and_then(|x| x.parse().ok()).unwrap_or(0),
            Some("processes") => st.processes = it.next().and_then(|x| x.parse().ok()).unwrap_or(0),
            _ => {}
        }
    }
    st
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct NetDev {
    pub name: String,
    pub rx_bytes: u64,
    pub rx_pkts: u64,
    pub tx_bytes: u64,
    pub tx_pkts: u64,
}

pub fn net_dev(s: &str) -> Vec<NetDev> {
    s.lines()
        .filter_map(|line| {
            let (name, rest) = line.split_once(':')?;
            let v: Vec<u64> = rest
                .split_ascii_whitespace()
                .filter_map(|x| x.parse().ok())
                .collect();
            if v.len() < 10 {
                return None;
            }
            Some(NetDev {
                name: name.trim().to_string(),
                rx_bytes: v[0],
                rx_pkts: v[1],
                tx_bytes: v[8],
                tx_pkts: v[9],
            })
        })
        .collect()
}

/// `/proc/interrupts`: (label, total over CPUs). Label = IRQ number plus the
/// last column (device name), e.g. "212 sitronix_ts".
pub fn interrupts(s: &str) -> Vec<(String, u64)> {
    let mut lines = s.lines();
    let ncpu = lines
        .next()
        .map(|h| h.split_ascii_whitespace().count())
        .unwrap_or(0);
    lines
        .filter_map(|line| {
            let (irq, rest) = line.split_once(':')?;
            let toks: Vec<&str> = rest.split_ascii_whitespace().collect();
            let counts: Vec<u64> = toks
                .iter()
                .take(ncpu)
                .map_while(|x| x.parse().ok())
                .collect();
            if counts.is_empty() {
                return None;
            }
            let desc = toks.get(counts.len()..).unwrap_or(&[]);
            let irq = irq.trim();
            // numbered IRQs: the device name is the last column; IPI/ERR rows: the whole text
            let name = if irq.bytes().all(|c| c.is_ascii_digit()) {
                desc.last().copied().unwrap_or("").to_string()
            } else {
                desc.join(" ")
            };
            let label = if name.is_empty() {
                irq.to_string()
            } else {
                format!("{irq} {name}")
            };
            Some((label, counts.iter().sum()))
        })
        .collect()
}

pub fn first_u64(s: &str) -> Option<u64> {
    s.split_ascii_whitespace().next()?.parse().ok()
}

pub fn first_i64(s: &str) -> Option<i64> {
    s.split_ascii_whitespace().next()?.parse().ok()
}

/// `/proc/uptime` first field in milliseconds.
pub fn uptime_ms(s: &str) -> Option<u64> {
    let f = s.split_ascii_whitespace().next()?;
    let (a, b) = f.split_once('.').unwrap_or((f, "0"));
    let frac: String = b.chars().chain("000".chars()).take(3).collect();
    Some(a.parse::<u64>().ok()? * 1000 + frac.parse::<u64>().ok()?)
}

/// `/proc/<pid>/status` VmRSS in kB.
pub fn status_rss_kb(s: &str) -> Option<u64> {
    s.lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(first_u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_with_odd_comm() {
        let s = "1234 (my (odd) prog) S 1 1234 1234 0 -1 4194560 100 0 0 0 7 3 0 0 20 0 2 0 5555 1000 50 18446744073709551615";
        let p = pid_stat(s).unwrap();
        assert_eq!(p.comm, "my (odd) prog");
        assert_eq!(p.ppid, 1);
        assert_eq!(p.cpu_ticks, 10);
        assert_eq!(p.starttime, 5555);
    }

    #[test]
    fn schedstat() {
        assert_eq!(schedstat_runs("123456 7890 35390\n"), Some(35390));
        assert_eq!(schedstat_runs("1 2"), None);
    }

    #[test]
    fn keys() {
        assert_eq!(
            proc_key(b"sh\0/data/luyaz/scripts/doctor.sh\0", "sh"),
            "sh /data/luyaz/scripts/doctor.sh"
        );
        assert_eq!(
            proc_key(b"/usr/bin/mihomo\0-d\0/data/chill\0", "mihomo"),
            "/usr/bin/mihomo /data/chill"
        );
        assert_eq!(proc_key(b"", "kworker/0:1"), "[kworker/0:1]");
        assert_eq!(
            proc_key(b"/data/luyaz/luyaz-web\0", "luyaz-web"),
            "/data/luyaz/luyaz-web"
        );
    }

    #[test]
    fn sys() {
        let s = "cpu  10 1 5 100 2 1 1 0 0 0\ncpu0 1 1 1 1\nintr 999 0 0\nctxt 4242\nbtime 1\nprocesses 87860\n";
        let st = sys_stat(s);
        assert_eq!(st.busy, 18);
        assert_eq!(st.total, 120);
        assert_eq!(st.ctxt, 4242);
        assert_eq!(st.intr, 999);
        assert_eq!(st.processes, 87860);
    }

    #[test]
    fn netdev() {
        let s = "Inter-|   Receive |  Transmit\n face |bytes packets errs drop fifo frame compressed multicast|bytes packets\n rmnet_data0: 1000 10 0 0 0 0 0 0 2000 20 0 0 0 0 0 0\n    lo: 5 1 0 0 0 0 0 0 5 1 0 0 0 0 0 0\n";
        let v = net_dev(s);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].name, "rmnet_data0");
        assert_eq!((v[0].rx_pkts, v[0].tx_pkts, v[0].tx_bytes), (10, 20, 2000));
    }

    #[test]
    fn irqs() {
        let s = "           CPU0       CPU1\n  5:        10          5     GICv3  27 Level     arch_timer\nIPI0:       100        200       Rescheduling interrupts\nERR:          0\n";
        let v = interrupts(s);
        assert_eq!(v[0], ("5 arch_timer".to_string(), 15));
        assert_eq!(v[1], ("IPI0 Rescheduling interrupts".to_string(), 300));
        assert_eq!(v[2], ("ERR".to_string(), 0));
    }

    #[test]
    fn uptime() {
        assert_eq!(uptime_ms("12345.67 999.00\n"), Some(12_345_670));
        assert_eq!(uptime_ms("5.1 1"), Some(5_100));
    }
}
