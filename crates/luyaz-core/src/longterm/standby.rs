//! The guard's standby records and program fingerprints, once it hands them over
//! (u60-sentinel.md §3 G10, G11; §20): the same files at the same places, so doctor reads them as
//! before (touch-ui docs/LEDGER.md §12).
//!
//! - `standby` (the guard's `standby_round`): each check, the WAN and Tailscale packet counts and
//!   each program's context switches (all its threads); while the backlight is off and the check
//!   before is close enough (not across a sleep), one line `<uptime> <wan pkts/min> <ts pkts/min>
//!   <switches/s per program or ->` to `/tmp/standby.stat`, the last 60 kept. The previous sample
//!   is ours (`/tmp/luyaz/core/standby.prev`), so a core restart goes on from it.
//! - `fp` (the guard's `fp_round`): `/tmp/luyaz/ledger/fp` = `<program> <pid> <md5 prefix>`
//!   per program (`- -` when not running), the md5 taken only when the pid changed; when a
//!   program's md5 changed, `fp-changed` gets the uptime (doctor then judges only standby records
//!   taken after it).
//!
//! The core runs either only while the guard's handover file names it (`standby`, `fp`), as the
//! SMS and LAN IPv6 duties: the guard and the core never both write one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The programs measured (the guard's `STANDBY_PROGS`, in its order: doctor reads by position).
/// These are ledger keys ([`crate::paths::prog_key`]). A baseline's columns and `fp` lines are
/// by position: renaming or dropping a key orphans every baseline and fingerprint on the device.
pub const PROGS: [&str; 4] = ["tailscaled", "mihomo", crate::paths::UI, crate::paths::CORE];
/// Lines kept in the standby file.
const KEEP: usize = 60;
/// Checks further apart than this were across a sleep (the guard's `WAKE_GAP`, 2 rounds + 30 s).
pub const WAKE_GAP: i64 = 150;
pub const STANDBY: &str = "standby";
pub const FP: &str = "fp";

pub struct Paths {
    pub proc_: PathBuf,
    pub netdev: PathBuf,
    pub backlight: PathBuf,
    pub stat: PathBuf,
    pub prev: PathBuf,
    pub ltmp: PathBuf,
    pub handover: PathBuf,
    pub wan_if: String,
    pub ts_if: String,
}

impl Paths {
    pub fn device() -> Paths {
        let h = |p: &str| PathBuf::from(crate::state::host_path(p));
        Paths {
            proc_: h("/proc"),
            netdev: h("/proc/net/dev"),
            backlight: h("/sys/class/leds/led:lcd/brightness"),
            stat: h("/tmp/standby.stat"),
            prev: h("/tmp/luyaz/core/standby.prev"),
            ltmp: h("/tmp/luyaz/ledger"),
            handover: h("/tmp/luyaz/handover"),
            wan_if: "rmnet_data0".into(),
            ts_if: "tailscale0".into(),
        }
    }
}

/// Which of the two the guard has handed over.
pub fn owned(p: &Paths) -> (bool, bool) {
    let t = std::fs::read_to_string(&p.handover).unwrap_or_default();
    (
        luyaz_api::handover::names(&t, STANDBY),
        luyaz_api::handover::names(&t, FP),
    )
}

fn read(p: &Path) -> Option<String> {
    std::fs::read(p)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}

fn write_atomic(p: &Path, text: &str) -> std::io::Result<()> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, p)
}

/// Received + sent packets of `ifname` in `/proc/net/dev` (0 when absent, as the guard's).
pub fn packets(netdev: &str, ifname: &str) -> u64 {
    for l in netdev.lines() {
        let Some((name, rest)) = l.split_once(':') else {
            continue;
        };
        if name.trim() != ifname {
            continue;
        }
        let f: Vec<u64> = rest
            .split_whitespace()
            .map(|x| x.parse().unwrap_or(0))
            .collect();
        // rx: bytes packets errs drop fifo frame compressed multicast | tx: bytes packets …
        return f.get(1).copied().unwrap_or(0) + f.get(9).copied().unwrap_or(0);
    }
    0
}

/// All of a process's context switches, voluntary and not, over its threads.
fn switches(proc_: &Path, pid: u32) -> u64 {
    let Ok(rd) = std::fs::read_dir(proc_.join(pid.to_string()).join("task")) else {
        return 0;
    };
    let mut n = 0;
    for t in rd.flatten() {
        let Some(s) = read(&t.path().join("status")) else {
            continue;
        };
        for l in s.lines() {
            if let Some(v) = l
                .strip_prefix("voluntary_ctxt_switches:")
                .or_else(|| l.strip_prefix("nonvoluntary_ctxt_switches:"))
            {
                n += v.trim().parse::<u64>().unwrap_or(0);
            }
        }
    }
    n
}

/// One sample, as the guard keeps it: `<uptime> <wan> <ts> <pid:switches or ->…`.
pub fn sample(p: &Paths, up: i64, pids: &BTreeMap<String, u32>) -> String {
    let nd = read(&p.netdev).unwrap_or_default();
    let mut s = format!(
        "{up} {} {}",
        packets(&nd, &p.wan_if),
        packets(&nd, &p.ts_if)
    );
    for prog in PROGS {
        match pids.get(prog) {
            Some(&pid) => s.push_str(&format!(" {pid}:{}", switches(&p.proc_, pid))),
            None => s.push_str(" -"),
        }
    }
    s
}

/// The standby line between two samples, or None (the guard's awk: too far apart or not after,
/// a counter that went back).
pub fn line(prev: &str, cur: &str, gap: i64) -> Option<String> {
    let a: Vec<&str> = prev.split_whitespace().collect();
    let b: Vec<&str> = cur.split_whitespace().collect();
    if a.len() < 3 || a.len() != b.len() {
        return None;
    }
    let n = |s: &str| s.parse::<f64>().unwrap_or(0.0);
    let el = n(b[0]) - n(a[0]);
    if el <= 0.0 || el > gap as f64 {
        return None;
    }
    let wan = (n(b[1]) - n(a[1])) * 60.0 / el;
    let ts = (n(b[2]) - n(a[2])) * 60.0 / el;
    if wan < 0.0 || ts < 0.0 {
        return None;
    }
    let mut out = format!("{} {wan:.0} {ts:.0}", n(b[0]) as i64);
    for i in 3..a.len() {
        let (x, y) = (a[i], b[i]);
        let pair = |s: &str| s.split_once(':').map(|(p, c)| (p.to_string(), n(c)));
        match (pair(x), pair(y)) {
            (Some((pa, ca)), Some((pb, cb))) if x != "-" && y != "-" && pa == pb && cb >= ca => {
                out.push_str(&format!(" {:.1}", (cb - ca) / el))
            }
            _ => out.push_str(" -"),
        }
    }
    Some(out)
}

/// The guard's `standby_round`: sample, keep it, and while the screen is off add a line.
pub fn standby_round(p: &Paths, up: i64, pids: &BTreeMap<String, u32>) -> std::io::Result<()> {
    let cur = sample(p, up, pids);
    let prev = read(&p.prev).unwrap_or_default();
    write_atomic(&p.prev, &format!("{cur}\n"))?;
    let bl: i64 = read(&p.backlight)
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    if bl > 0 {
        return Ok(());
    }
    let Some(l) = line(prev.trim(), &cur, WAKE_GAP) else {
        return Ok(());
    };
    let old = read(&p.stat).unwrap_or_default();
    let mut lines: Vec<&str> = old.lines().filter(|l| !l.is_empty()).collect();
    lines.push(&l);
    let from = lines.len().saturating_sub(KEEP);
    let text: String = lines[from..].iter().map(|l| format!("{l}\n")).collect();
    write_atomic(&p.stat, &text)
}

/// The guard's `fp_round`: one line per program; the md5 only for a pid not seen before.
pub fn fp_round(p: &Paths, up: i64, pids: &BTreeMap<String, u32>) -> std::io::Result<()> {
    let file = p.ltmp.join("fp");
    let old = read(&file).unwrap_or_default();
    let mut text = String::new();
    let mut changed = false;
    for prog in PROGS {
        let (op, om) = old
            .lines()
            .find_map(|l| {
                let mut f = l.split_whitespace();
                (f.next() == Some(prog)).then(|| {
                    (
                        f.next().unwrap_or("").to_string(),
                        f.next().unwrap_or("").to_string(),
                    )
                })
            })
            .unwrap_or_default();
        let Some(&pid) = pids.get(prog) else {
            text.push_str(&format!("{prog} - -\n"));
            continue;
        };
        if pid.to_string() == op && !om.is_empty() && om != "-" {
            text.push_str(&format!("{prog} {pid} {om}\n"));
            continue;
        }
        let m = super::watch::md5_path(&p.proc_.join(pid.to_string()).join("exe"), 8);
        text.push_str(&format!("{prog} {pid} {}\n", m.as_deref().unwrap_or("-")));
        if let Some(m) = &m
            && !om.is_empty()
            && om != "-"
            && *m != om
        {
            changed = true;
        }
    }
    write_atomic(&file, &text)?;
    if changed {
        write_atomic(&p.ltmp.join("fp-changed"), &format!("{up}\n"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn line_as_the_guards_awk() {
        // 60 s apart, 120 + 30 packets, core 600 switches → 10.0/s, one program gone, one new pid
        let prev = "1000 500 100 11:100 - 13:1000 14:50 15:7";
        let cur = "1060 620 130 11:160 - 13:1600 99:60 15:7";
        assert_eq!(
            line(prev, cur, 150).as_deref(),
            Some("1060 120 30 1.0 - 10.0 - 0.0")
        );
        // 30 s apart: per minute doubles
        assert_eq!(
            line("1000 0 0 - - - - -", "1030 10 1 - - - - -", 150).as_deref(),
            Some("1030 20 2 - - - - -")
        );
        // across a sleep, not after, counters reset, switches went back
        assert_eq!(
            line(prev, "1200 620 130 11:160 - 13:1600 14:60 15:7", 150),
            None
        );
        assert_eq!(line(prev, prev, 150), None);
        assert_eq!(
            line(prev, "1060 10 130 11:160 - 13:1600 14:60 15:7", 150),
            None
        );
        assert_eq!(
            line(prev, "1060 620 130 11:90 - 13:1600 14:60 15:7", 150).as_deref(),
            Some("1060 120 30 - - 10.0 0.2 0.0")
        );
        assert_eq!(line("", "1060 0 0 - - - - -", 150), None);
        // rounding as busybox awk's printf (checked against it): halves to even
        for (cur, want) in [
            ("1007 1 1 1:1", "1007 9 9 0.1"),
            ("1008 1 1 1:3", "1008 8 8 0.4"),
            ("1024 1 1 1:6", "1024 2 2 0.2"),
            ("1040 1 3 1:1", "1040 2 4 0.0"),
        ] {
            assert_eq!(line("1000 0 0 1:0", cur, 150).as_deref(), Some(want));
        }
    }

    #[test]
    fn packets_rx_plus_tx() {
        let nd = "Inter-|   Receive |  Transmit\n face |bytes packets errs drop fifo frame compressed multicast|bytes packets\n rmnet_data0: 9000 40 0 0 0 0 0 0 7000 25 0 0 0 0 0 0\ntailscale0:1 2 0 0 0 0 0 0 3 4 0 0 0 0 0 0\n";
        assert_eq!(packets(nd, "rmnet_data0"), 65);
        assert_eq!(packets(nd, "tailscale0"), 6);
        assert_eq!(packets(nd, "wlan0"), 0);
    }

    fn root(name: &str) -> (PathBuf, Paths) {
        let r = std::env::temp_dir().join(format!("u60d-standby-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&r);
        fs::create_dir_all(&r).unwrap();
        let p = Paths {
            proc_: r.join("proc"),
            netdev: r.join("netdev"),
            backlight: r.join("bl"),
            stat: r.join("standby.stat"),
            prev: r.join("u60d/standby.prev"),
            ltmp: r.join("ledger"),
            handover: r.join("handover"),
            wan_if: "rmnet_data0".into(),
            ts_if: "tailscale0".into(),
        };
        (r, p)
    }

    fn task(p: &Paths, pid: u32, tid: u32, vol: u64, non: u64) {
        let d = p.proc_.join(format!("{pid}/task/{tid}"));
        fs::create_dir_all(&d).unwrap();
        fs::write(
            d.join("status"),
            format!(
                "Name:\tx\nvoluntary_ctxt_switches:\t{vol}\nnonvoluntary_ctxt_switches:\t{non}\n"
            ),
        )
        .unwrap();
    }

    #[test]
    fn standby_lines_only_with_the_screen_off_and_the_last_60_kept() {
        let (r, p) = root("round");
        // the programs' columns: only the core runs
        let cols = |core: &str| {
            PROGS
                .iter()
                .map(|q| if *q == crate::paths::CORE { core } else { "-" })
                .collect::<Vec<_>>()
                .join(" ")
        };
        let pids: BTreeMap<String, u32> = [(crate::paths::CORE.to_string(), 7)].into();
        task(&p, 7, 7, 10, 2);
        task(&p, 7, 8, 5, 0);
        fs::write(
            &p.netdev,
            " rmnet_data0: 0 10 0 0 0 0 0 0 0 5 0 0 0 0 0 0\n",
        )
        .unwrap();
        fs::write(&p.backlight, "0\n").unwrap();
        standby_round(&p, 1000, &pids).unwrap();
        assert!(!p.stat.exists(), "no line from the first sample");
        assert_eq!(
            fs::read_to_string(&p.prev).unwrap(),
            format!("1000 15 0 {}\n", cols("7:17"))
        );
        task(&p, 7, 8, 65, 0);
        fs::write(
            &p.netdev,
            " rmnet_data0: 0 40 0 0 0 0 0 0 0 5 0 0 0 0 0 0\n",
        )
        .unwrap();
        standby_round(&p, 1060, &pids).unwrap();
        assert_eq!(
            fs::read_to_string(&p.stat).unwrap(),
            format!("1060 30 0 {}\n", cols("1.0"))
        );
        // screen on: sampled, no line
        fs::write(&p.backlight, "120\n").unwrap();
        standby_round(&p, 1120, &pids).unwrap();
        assert_eq!(fs::read_to_string(&p.stat).unwrap().lines().count(), 1);
        fs::write(&p.backlight, "0\n").unwrap();
        for i in 0..70 {
            standby_round(&p, 1180 + i * 60, &pids).unwrap();
        }
        let t = fs::read_to_string(&p.stat).unwrap();
        assert_eq!(t.lines().count(), 60);
        assert!(t.ends_with(&format!("{} 0 0 {}\n", 1180 + 69 * 60, cols("0.0"))));
        let _ = fs::remove_dir_all(&r);
    }

    #[test]
    fn fingerprints_md5_once_a_pid_and_fp_changed_when_the_binary_changed() {
        let (r, p) = root("fp");
        fs::create_dir_all(p.proc_.join("7")).unwrap();
        fs::write(p.proc_.join("7/exe"), b"one").unwrap();
        let pids: BTreeMap<String, u32> = [(crate::paths::CORE.to_string(), 7)].into();
        fp_round(&p, 100, &pids).unwrap();
        let m1 = super::super::watch::md5_path(&p.proc_.join("7/exe"), 8).unwrap();
        assert_eq!(
            fs::read_to_string(p.ltmp.join("fp")).unwrap(),
            PROGS
                .iter()
                .map(|q| if *q == crate::paths::CORE {
                    format!("{q} 7 {m1}\n")
                } else {
                    format!("{q} - -\n")
                })
                .collect::<String>()
        );
        // same pid: not read again (a changed file under the same pid keeps the old md5)
        fs::write(p.proc_.join("7/exe"), b"two").unwrap();
        fp_round(&p, 160, &pids).unwrap();
        assert!(fs::read_to_string(p.ltmp.join("fp")).unwrap().contains(&m1));
        assert!(!p.ltmp.join("fp-changed").exists());
        // new pid, new binary: fp-changed
        fs::create_dir_all(p.proc_.join("9")).unwrap();
        fs::write(p.proc_.join("9/exe"), b"two").unwrap();
        let pids: BTreeMap<String, u32> = [(crate::paths::CORE.to_string(), 9)].into();
        fp_round(&p, 220, &pids).unwrap();
        assert_eq!(
            fs::read_to_string(p.ltmp.join("fp-changed")).unwrap(),
            "220\n"
        );
        // gone
        fp_round(&p, 280, &BTreeMap::new()).unwrap();
        assert!(
            fs::read_to_string(p.ltmp.join("fp"))
                .unwrap()
                .ends_with("luyaz-core - -\n")
        );
        let _ = fs::remove_dir_all(&r);
    }

    #[test]
    fn owned_by_the_handover_file() {
        let (r, p) = root("own");
        assert_eq!(owned(&p), (false, false));
        fs::write(&p.handover, "sms\nstandby\n").unwrap();
        assert_eq!(owned(&p), (true, false));
        fs::write(&p.handover, "fp\nstandby\n").unwrap();
        assert_eq!(owned(&p), (true, true));
        let _ = fs::remove_dir_all(&r);
    }
}
