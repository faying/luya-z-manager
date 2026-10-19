//! doctor.sh's standby row (`standby_check`, docs/LEDGER.md §12): screen-off records judged
//! against a v2 baseline of median and MAD, per column, with fingerprints.

use super::{Cfg, cat, digits};
use std::collections::BTreeMap;

// Ledger keys, by position (longterm/standby.rs `PROGS`).
const PROGS: &[&str] = &crate::longterm::standby::PROGS;
const LABELS: &[&str] = &[
    "-",
    "蜂窝包/分",
    "Tailscale隧道包/分",
    "tailscaled唤醒/秒",
    "mihomo唤醒/秒",
    "触屏唤醒/秒",
    "数据服务唤醒/秒",
];
const LABELS_EN: &[&str] = &[
    "-",
    "Cell pkts/min",
    "Tailscale tunnel pkts/min",
    "tailscaled wakeups/s",
    "mihomo wakeups/s",
    "Screen UI wakeups/s",
    "Data service wakeups/s",
];

/// `standby_last`: uptime, cellular, tunnel, then one column per program
fn last() -> usize {
    3 + PROGS.len()
}

/// `cfg_md5`: the md5's first 8, "none" when the file is not there
fn cfg_md5(path: &str) -> String {
    super::manifest::md5_file(path)
        .map(|m| m[..8].to_owned())
        .unwrap_or_else(|| "none".into())
}

/// `prog_md5`: from the fingerprint cache "<program> <pid> <md5>", "?" when unknown
fn prog_md5(c: &Cfg, name: &str) -> String {
    let m = cat(&c.fp)
        .and_then(|s| {
            s.split('\n').find_map(|l| {
                let f: Vec<&str> = l.split([' ', '\t']).filter(|x| !x.is_empty()).collect();
                (f.first() == Some(&name)).then(|| f.get(2).copied().unwrap_or("").to_owned())
            })
        })
        .unwrap_or_default();
    if m.is_empty() { "?".into() } else { m }
}

/// `standby_fp`: (column, fingerprint) for columns 3..
fn fingerprints(c: &Cfg) -> Vec<(usize, String)> {
    let tsm = prog_md5(c, "tailscaled");
    let tun = cfg_md5(&c.ts_tuning);
    let mut v = vec![(3, format!("{tsm}+{tun}"))];
    for (i, p) in PROGS.iter().enumerate() {
        let f = match *p {
            "tailscaled" => format!("{tsm}+{tun}"),
            "mihomo" => format!(
                "{}+{}+{}",
                prog_md5(c, "mihomo"),
                cfg_md5(&c.chill.template),
                cfg_md5(&c.chill.env)
            ),
            p => prog_md5(c, p),
        };
        v.push((4 + i, f));
    }
    v
}

/// awk's numeric value of a string (`x + 0`)
fn num(s: &str) -> f64 {
    super::awk_num(s)
}

/// the awk `med`: insertion sort, then the middle (or the mean of the two)
fn med(a: &mut [f64]) -> f64 {
    let c = a.len();
    for i in 1..c {
        let v = a[i];
        let mut j = i;
        while j > 0 && a[j - 1] > v {
            a[j] = a[j - 1];
            j -= 1;
        }
        a[j] = v;
    }
    if c == 0 {
        return 0.0;
    }
    if c % 2 == 1 {
        a[c.div_ceil(2) - 1]
    } else {
        (a[c / 2 - 1] + a[c / 2]) / 2.0
    }
}

/// awk `%.0f`
fn f0(x: f64) -> String {
    format!("{x:.0}")
}

/// `standby_check [en]`: (level, detail)
pub fn check(c: &Cfg, en: bool) -> (&'static str, String) {
    let ok = |s: &str| ("ok", s.to_owned());
    let Some(base) = std::fs::read(&c.standby_base)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
    else {
        return ok(if en {
            "Not calibrated (run doctor.sh --calibrate-standby after 30-60 min with the screen off)"
        } else {
            "未校准（屏幕熄灭 30~60 分钟后执行 doctor.sh --calibrate-standby）"
        });
    };
    if base
        .split('\n')
        .next()
        .unwrap_or("")
        .trim_matches([' ', '\t'])
        != "v2"
    {
        return ok(if en {
            "Old baseline format, not judged; recalibrate at home while idle (doctor.sh --calibrate-standby)"
        } else {
            "基线格式旧，不判；请在家自然空闲时重新校准（doctor.sh --calibrate-standby）"
        });
    }
    let since = cat(&c.fp_changed).and_then(|s| digits(&s)).unwrap_or(0) as f64;
    let now = super::uptime_s(c) as f64;
    let lab = if en { LABELS_EN } else { LABELS };
    let label = |k: usize| lab.get(k - 1).copied().unwrap_or("");
    let (mut bm, mut bd, mut bf) = (BTreeMap::new(), BTreeMap::new(), BTreeMap::new());
    for l in base.split('\n') {
        let f: Vec<&str> = l.split([' ', '\t']).filter(|x| !x.is_empty()).collect();
        let g = |i: usize| f.get(i).copied().unwrap_or("").to_owned();
        if g(0) == "fp" {
            bf.insert(g(1), g(2));
        } else if g(0) != "v2" {
            bm.insert(g(0), g(1));
            bd.insert(g(0), g(2));
        }
    }
    let cf: BTreeMap<String, String> = fingerprints(c)
        .into_iter()
        .map(|(k, f)| (k.to_string(), f))
        .collect();
    let get =
        |m: &BTreeMap<String, String>, k: usize| m.get(&k.to_string()).cloned().unwrap_or_default();
    let thr = {
        let b2 = get(&bm, 2);
        if b2 == "-" || b2.is_empty() {
            -1.0
        } else {
            num(&b2) + 3.0 * num(&get(&bd, 2))
        }
    };
    let Ok(stat) = std::fs::read(&c.standby_stat) else {
        return ok(if en {
            "No screen-off records"
        } else {
            "没有息屏记录"
        });
    };
    let stat = String::from_utf8_lossy(&stat);
    let (mut rows, mut busy, mut idle) = (0, 0, 0);
    let mut v: BTreeMap<usize, Vec<f64>> = BTreeMap::new();
    let mut recs: Vec<&str> = stat.split('\n').collect();
    if recs.last() == Some(&"") {
        recs.pop();
    }
    for l in recs {
        let f: Vec<&str> = l.split([' ', '\t']).filter(|x| !x.is_empty()).collect();
        let g = |i: usize| f.get(i).copied().unwrap_or("");
        if f.is_empty() || !(num(g(0)) >= now - 900.0 && num(g(0)) > since) {
            continue;
        }
        rows += 1;
        if thr < 0.0 || g(1) == "-" || g(1).is_empty() || num(g(1)) > thr {
            busy += 1;
            continue;
        }
        idle += 1;
        for k in 2..=last() {
            let x = g(k - 1);
            if x != "-" && !x.is_empty() {
                v.entry(k).or_default().push(num(x));
            }
        }
    }
    if idle < 8 {
        let busy_most = rows >= 8 && busy * 2 > rows;
        return ok(&match (en, busy_most) {
            (true, true) => format!(
                "Network in use, standby not judged ({busy} of {rows} rows in the last 15 min had traffic)"
            ),
            (true, false) => {
                format!("Too few idle screen-off rows in the last 15 min ({idle}), not judged")
            }
            (false, true) => {
                format!("有流量在用，不判待机（最近 15 分钟 {rows} 行里 {busy} 行有流量）")
            }
            (false, false) => format!("最近 15 分钟空闲的息屏记录只有 {idle} 行，不判定"),
        });
    }
    let (mut out, mut stale) = (String::new(), String::new());
    let (sep, ssep) = if en { ("; ", ", ") } else { ("；", "、") };
    for k in 3..=last() {
        let b = get(&bm, k);
        if b == "-" || b.is_empty() {
            continue;
        }
        let ks = k.to_string();
        if !bf.contains_key(&ks)
            || !cf.contains_key(&ks)
            || bf[&ks] != cf[&ks]
            || cf[&ks].contains('?')
        {
            if !stale.is_empty() {
                stale.push_str(ssep);
            }
            stale.push_str(label(k));
            continue;
        }
        let Some(x) = v.get_mut(&k) else { continue };
        let m = med(x);
        let (bmk, bdk) = (num(&b), num(&get(&bd, k)));
        if m > bmk + 3.0 * bdk && m > 1.5 * bmk && m - bmk >= 1.0 {
            if !out.is_empty() {
                out.push_str(sep);
            }
            out.push_str(&if en {
                format!("{} {} (baseline {})", label(k), f0(m), f0(bmk))
            } else {
                format!("{} {}（基线 {}）", label(k), f0(m), f0(bmk))
            });
        }
    }
    let cell = med(v.entry(2).or_default());
    let tail = match (stale.is_empty(), en) {
        (true, _) => String::new(),
        (false, true) => format!("; baseline out of date for {stale}, not judged"),
        (false, false) => format!("；{stale} 基线过期，不判"),
    };
    if !out.is_empty() {
        ("warn", format!("{out}{tail}"))
    } else if en {
        ("ok", format!("Normal (cell {} pkts/min){tail}", f0(cell)))
    } else {
        ("ok", format!("正常（蜂窝每分钟 {} 个包）{tail}", f0(cell)))
    }
}

/// `calibrate_standby` (`--calibrate-standby`): a v2 baseline from the screen-off records — per
/// column 2.. its median and MAD (`- -` when it has no value), then the columns' fingerprints —
/// written to a temp file and renamed; then one `calib` event in the core's long-term layer.
/// (The guard's ledger spool, the old body's fallback, is no longer read: not written.)
pub fn calibrate(c: &Cfg) -> (String, i32) {
    let stat = std::fs::read(&c.standby_stat)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    // `wc -l`: newlines
    let n = stat.matches('\n').count();
    if n < 30 {
        return (
            format!("只有 {n} 行息屏记录，至少要 30 行（屏幕熄灭约 30 分钟后再试）\n"),
            1,
        );
    }
    let mut v: BTreeMap<usize, Vec<f64>> = BTreeMap::new();
    for l in stat.split('\n') {
        let f: Vec<&str> = l.split([' ', '\t']).filter(|x| !x.is_empty()).collect();
        for k in 2..=last() {
            let x = f.get(k - 1).copied().unwrap_or("");
            if x != "-" && !x.is_empty() {
                v.entry(k).or_default().push(num(x));
            }
        }
    }
    let mut out = String::from("v2\n");
    for k in 2..=last() {
        match v.get_mut(&k) {
            None => out.push_str(&format!("{k} - -\n")),
            Some(x) => {
                let m = med(x);
                let mut d: Vec<f64> = x.iter().map(|y| (y - m).abs()).collect();
                out.push_str(&format!("{k} {m:.1} {:.1}\n", med(&mut d)));
            }
        }
    }
    for (k, f) in fingerprints(c) {
        out.push_str(&format!("fp {k} {f}\n"));
    }
    let tmp = format!("{}.tmp", c.standby_base);
    if std::fs::write(&tmp, &out)
        .and_then(|()| std::fs::rename(&tmp, &c.standby_base))
        .is_err()
    {
        return (String::new(), 1);
    }
    calib_event(c, n);
    (format!("基线已写入 {}（{n} 行记录）\n", c.standby_base), 0)
}

/// Tell the long-term layer (docs/LEDGER.md §4 calib): `calib-<boot 8>-<uptime>`; best effort.
fn calib_event(c: &Cfg, rows: usize) {
    let boot = cat(&c.boot_id_file).unwrap_or_default();
    let boot = boot.trim_end_matches('\n');
    if boot.is_empty()
        || !boot
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b) || b == b'-')
    {
        return;
    }
    let id = format!(
        "calib-{}-{}",
        boot.get(..8).unwrap_or(boot),
        super::uptime_s(c)
    );
    let url = format!("{}?id={id}&rows={rows}", c.calib_url);
    let _ = super::sh_raw(
        &c.wget,
        &["-q", "-T", "5", "--post-data", "x", "-O", "-", &url],
        true,
    );
}
