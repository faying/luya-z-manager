//! The device scorecard (touch-ui docs/LEDGER.md §11; u60-sentinel.md §19 T9a): doctor's
//! `--report 24h|7d [--summary]` moved into the core, line for line.
//!
//! Sans-IO: [`Card::line`] takes ledger lines (touch-ui LEDGER.md §3–§4: `{"v":1,"seq":…,"n":…,
//! "up":…,"t":…,"k":…,…}`) in order, [`Card::finish`] takes the other inputs doctor gives its awk
//! ([`Inputs`]) and returns the text and the exit code. Two sources feed it: the guard's ledger
//! segments as they are ([`guard_lines`]), and the core's long-term layer
//! (`/data/u60d/long/{hours,events}.jsonl`), which [`core::lines`] turns into ledger lines.
//!
//! **Parity first** (§19, the first leg): the same guard ledger gives byte for byte what
//! `doctor.sh --report` gives (tools/scorecard-parity.sh over the doctor test's fixtures and a
//! copy of the device's ledger). So this follows the awk, and where the awk was wrong both were
//! fixed together with a doctor test case (§19): two lookups that created what they looked for
//! (the S2b "still being followed" skip never fired; a boot with no `boot` line after a hole
//! counted as a reboot), and RSS compared as text (a baseline of "00" divided by zero).
//! - awk walks `for (x in …)` in hash order (the S2b detail line, the S3 abroad list); here they
//!   are sorted, and the parity tool compares those two lines as sets.

pub mod awk;
pub mod core;
pub mod live;
pub mod replay;
#[cfg(test)]
mod tests;

use awk::{Num, fixed, fld, gt, int, isn, le, looks_numeric, lt, md_hm, mktime, num, ymd_hm};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;

const D7: f64 = 604_800.0;
const D1: f64 = 86_400.0;
/// The sources an hour line counts gaps for (`g_<src>`).
const GS: [&str; 7] = [
    "guard", "job", "watcher", "capture", "uidlog", "crashlog", "datad",
];
/// The four power cells of `hour_power`.
const CS: [&str; 4] = ["on_home", "on_abroad", "off_home", "off_abroad"];
const RS: [&str; 4] = ["agent", "datad", "devui", "ts"];

/// What doctor passes its awk besides the ledger (`report_ledger`).
#[derive(Clone, Debug, Default)]
pub struct Inputs {
    /// The window in s (86400 or 604800).
    pub w: f64,
    /// "24 小时" / "7 天".
    pub label: String,
    /// Recovery's first go-live and u60-uid's takeover (wall s): fixed in doctor.
    pub recov: Num,
    pub uidtk: Num,
    /// `<boot8> <0|1>` lines: does each kept kernel log end in a modem crash line.
    pub cap: String,
    /// The health rows that are not ok, `<label>：<detail>` a line. `None`: no health check
    /// came with the request (`/v2/scorecard` without doctor's body), so U6's ▲/■ half and P3
    /// read 没测 instead of looking clean (§19 T9b).
    pub warn: Option<String>,
    /// standby_check's output (its second tab field is the verdict); `None` as for `warn`.
    pub standby: Option<String>,
    /// The alert queue's text.
    pub alerts: String,
    pub gaveup: bool,
    /// The standby idle threshold (pkts/min), or "".
    pub thr: String,
    pub summary: bool,
}

#[derive(Default)]
struct Boot {
    code: String,
    id: String,
    mode: String,
    /// `planned`: someone asked for the reboot that started this boot (boot.rs).
    planned: String,
}
impl Boot {
    fn planned(&self) -> bool {
        !self.planned.is_empty() && self.planned != "null"
    }
}
struct Ssr {
    id: String,
    s: i64,
    up: f64,
    net: String,
    home: String,
    rat: String,
    band: String,
    nr: String,
    ppm: String,
}
#[derive(Default, Clone)]
struct Res {
    result: String,
    rec: String,
    slept: String,
}
struct At {
    s: i64,
    up: f64,
}
struct Hour {
    s: i64,
    f: f64,
    t: f64,
    ft: String,
    tt: String,
    awake: f64,
    ab: f64,
    g: [f64; 7],
}
#[derive(Default)]
struct HourPower {
    e: String,
    part: String,
    thr: String,
    cs: [f64; 4],
    ce: [String; 4],
}
#[derive(Default)]
struct HourProc {
    cpu: String,
    con: String,
    cok: String,
    ton: String,
    tck: String,
    tok: String,
    rss: [String; RS.len()],
}

/// The scorecard being read.
#[derive(Default)]
pub struct Card {
    bad: u64,
    newer: u64,
    /// Segments with empty lines (a power cut's NULs; awk: `FILENAME in NULF`), and whether
    /// this segment has been counted.
    nuls: u64,
    nul_here: bool,
    seen: HashSet<String>,
    /// Boots (ledger `seq`) in the order first seen.
    sq: Vec<i64>,
    sqs: HashSet<i64>,
    /// Wall clock at uptime 0 of each boot, from its last trusted line.
    off: HashMap<i64, f64>,
    boots: HashMap<i64, Boot>,
    backfills: Vec<(i64, String, String)>,
    bf_gap: HashSet<i64>,
    gs_n: HashMap<i64, u32>,
    gs_up: HashMap<i64, f64>,
    gx: Vec<At>,
    ver_m: HashMap<String, String>,
    ver_c: HashMap<String, (i64, f64)>,
    datad_x: String,
    ssr: Vec<Ssr>,
    last_ssr: HashMap<i64, usize>,
    res: HashMap<String, Res>,
    sx: Vec<(At, String, String)>,
    pr: Vec<(At, String, String, String)>,
    ug: Vec<At>,
    /// datad_degraded episodes: start, since, dur_s ("" until it ends).
    dd: Vec<(At, String)>,
    ddi: HashMap<String, usize>,
    gaps: Vec<(i64, String, String, String)>,
    nc: Vec<At>,
    oom: Vec<At>,
    th: Vec<(At, String)>,
    h: Vec<Hour>,
    hk: HashMap<(i64, u64), usize>,
    hp: HashMap<usize, HourPower>,
    hq: HashMap<usize, HourProc>,
}

/// The ledger line's fixed head (doctor's first test): `{"v":N,"seq":N,"n":N,"up":[0-9.]+,
/// "t":(null|N),"k":"[a-z_]+"`.
fn head_ok(l: &str) -> bool {
    fn digits(s: &str, set: fn(u8) -> bool) -> Option<&str> {
        let n = s.bytes().take_while(|&c| set(c)).count();
        (n > 0).then(|| &s[n..])
    }
    let d = |c: u8| c.is_ascii_digit();
    let step = |s: &str, lit: &str, set: fn(u8) -> bool| -> Option<String> {
        digits(s.strip_prefix(lit)?, set).map(str::to_string)
    };
    let go = || -> Option<()> {
        let s = step(l, "{\"v\":", d)?;
        let s = step(&s, ",\"seq\":", d)?;
        let s = step(&s, ",\"n\":", d)?;
        let s = step(&s, ",\"up\":", |c| c.is_ascii_digit() || c == b'.')?;
        let s = s.strip_prefix(",\"t\":")?;
        let s = match s.strip_prefix("null") {
            Some(r) => r.to_string(),
            None => digits(s, d)?.to_string(),
        };
        let s = s.strip_prefix(",\"k\":\"")?;
        let s = digits(s, |c| c.is_ascii_lowercase() || c == b'_')?;
        s.starts_with('"').then_some(())
    };
    go().is_some()
}

/// `idfrom(id)`: the number after the id's last `-` (where its hour began).
fn idfrom(id: &str) -> f64 {
    num(id.rsplit('-').next().unwrap_or(""))
}

fn klass(code: &str) -> &'static str {
    match code {
        "1150" | "1152" => "manual",
        "1112" => "scheduled",
        "1134" | "1132" => "modem",
        "1185" => "noui",
        "1155" => "cutoff",
        "1182" => "1182",
        _ => "other",
    }
}

fn place(net: &str, home: &str) -> &'static str {
    if net.is_empty() || net == "null" || home.is_empty() || home == "null" {
        return "不明";
    }
    let p3 = |s: &str| s.chars().take(3).collect::<String>();
    if p3(net) == p3(home) {
        "在家"
    } else {
        "国外"
    }
}

/// Seconds as 天/小时/分.
fn dur(x: f64) -> String {
    let x = int(x);
    let (d, h, m) = (x / 86_400, x % 86_400 / 3600, x % 3600 / 60);
    if d > 0 {
        return format!(
            "{d} 天{}",
            if h > 0 {
                format!(" {h} 小时")
            } else {
                String::new()
            }
        );
    }
    if h > 0 {
        return format!(
            "{h} 小时{}",
            if m > 0 {
                format!(" {m} 分")
            } else {
                String::new()
            }
        );
    }
    format!("{m} 分")
}

fn when(x: Num) -> String {
    x.map_or_else(|| "时间不明".into(), md_hm)
}

fn trim(x: &str) -> &str {
    x.strip_suffix('；').unwrap_or(x)
}

/// The 95th percentile (nearest rank) of the values.
fn p95(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len() as f64;
    let mut k = (n * 0.95) as usize;
    if (k as f64) < n * 0.95 {
        k += 1;
    }
    v[k.max(1) - 1]
}

/// A ledger segment's lines as busybox awk reads them: a NUL ends a record like a newline does
/// (a power cut leaves a run of NULs at a segment's end: the device's ledger has one of 2490,
/// each a line awk cannot read); bytes that are not UTF-8 replaced.
pub fn guard_lines(text: &[u8]) -> impl Iterator<Item = String> + '_ {
    let mut v: Vec<&[u8]> = text.split(|&c| c == b'\n' || c == 0).collect();
    if v.last().is_some_and(|l| l.is_empty()) {
        v.pop();
    }
    v.into_iter()
        .map(|l| String::from_utf8_lossy(l).into_owned())
}

impl Card {
    pub fn new() -> Card {
        Card::default()
    }

    /// The next lines are from another guard ledger segment (file).
    pub fn segment(&mut self) {
        self.nul_here = false;
    }

    /// One ledger line.
    pub fn line(&mut self, l: &str) {
        if l.is_empty() {
            if !self.nul_here {
                self.nul_here = true;
                self.nuls += 1;
            }
            return;
        }
        if !head_ok(l) {
            self.bad += 1;
            return;
        }
        if num(fld(l, "v")) > 1.0 {
            self.newer += 1;
            return;
        }
        let id = fld(l, "id");
        if !id.is_empty() && !self.seen.insert(id.to_string()) {
            return;
        }
        let k = fld(l, "k");
        let s = num(fld(l, "seq")) as i64;
        let up = num(fld(l, "up"));
        let t = fld(l, "t");
        if self.sqs.insert(s) {
            self.sq.push(s);
        }
        if t != "null" {
            self.off.insert(s, num(t) - up.trunc());
        }
        let f = |n: &str| fld(l, n).to_string();
        let at = At { s, up };
        match k {
            "boot" => {
                self.boots.insert(
                    s,
                    Boot {
                        code: f("code"),
                        id: f("boot"),
                        mode: f("mode"),
                        planned: f("planned"),
                    },
                );
            }
            "boot_backfill" => {
                self.backfills.push((s, f("code"), f("at")));
                if fld(l, "gap") == "1" {
                    self.bf_gap.insert(s);
                }
            }
            "guard_start" => {
                let n = self.gs_n.entry(s).or_default();
                *n += 1;
                let n = *n;
                self.gs_up.entry(s).or_insert(up);
                if fld(l, "requested") != "1" && n > 1 {
                    self.gx.push(at);
                }
            }
            "ver" => {
                let (p, m) = (f("prog"), f("md5"));
                if m != "null" && self.ver_m.get(&p).is_some_and(|o| *o != m) {
                    self.ver_c.insert(p.clone(), (s, up));
                }
                if m != "null" {
                    self.ver_m.insert(p.clone(), m);
                }
                if crate::paths::is_core(&p) {
                    self.datad_x = f("extra");
                }
            }
            "ssr" => {
                self.ssr.push(Ssr {
                    id: id.to_string(),
                    s,
                    up,
                    net: f("net"),
                    home: f("home"),
                    rat: f("rat"),
                    band: f("band"),
                    nr: f("nrband"),
                    ppm: f("wan_ppm"),
                });
                self.last_ssr.insert(s, self.ssr.len() - 1);
            }
            "ssr_result" => {
                self.res.insert(
                    f("ssr"),
                    Res {
                        result: f("result"),
                        rec: f("recovered_s"),
                        slept: f("slept"),
                    },
                );
            }
            "svc_exit" => self.sx.push((at, f("prog"), f("status"))),
            "proc_restart" => self.pr.push((at, f("prog"), f("crashlog"), f("ship"))),
            "uid" => {
                if fld(l, "what") == "gave_up" {
                    self.ug.push(at);
                }
            }
            "datad_degraded" => {
                let (st, sc) = (f("state"), f("since"));
                if st == "start" {
                    self.dd.push((at, String::new()));
                    self.ddi.insert(sc, self.dd.len() - 1);
                } else if let Some(&i) = self.ddi.get(&sc) {
                    self.dd[i].1 = f("dur_s");
                }
            }
            "gap" => self.gaps.push((s, f("src"), f("from"), f("to"))),
            "net_change" => self.nc.push(at),
            "oom" => self.oom.push(at),
            "thermal" => self.th.push((at, f("level"))),
            "hour" => {
                let mut g = [0.0; 7];
                for (j, src) in GS.iter().enumerate() {
                    g[j] = num(fld(l, &format!("g_{src}")));
                }
                let h = Hour {
                    s,
                    f: num(fld(l, "from")),
                    t: num(fld(l, "to")),
                    ft: f("ft"),
                    tt: f("tt"),
                    awake: num(fld(l, "awake")),
                    ab: num(fld(l, "abroad")),
                    g,
                };
                self.hk.insert((s, h.f.to_bits()), self.h.len());
                if isn(&h.tt) {
                    self.off.insert(s, num(&h.tt) - h.t);
                }
                self.h.push(h);
            }
            "hour_power" => {
                if let Some(&i) = self.hk.get(&(s, idfrom(id).to_bits())) {
                    let mut p = HourPower {
                        e: f("e"),
                        part: f("e_partial"),
                        thr: f("throttle_s"),
                        ..Default::default()
                    };
                    for (j, c) in CS.iter().enumerate() {
                        p.cs[j] = num(fld(l, &format!("{c}_s")));
                        p.ce[j] = f(&format!("{c}_e"));
                    }
                    self.hp.insert(i, p);
                }
            }
            "hour_proc" => {
                if let Some(&i) = self.hk.get(&(s, idfrom(id).to_bits())) {
                    let mut q = HourProc {
                        cpu: f("cpu_datad"),
                        ton: f("ts_on_s"),
                        tck: f("ts_chk_s"),
                        tok: f("ts_ok_s"),
                        ..Default::default()
                    };
                    for (j, r) in RS.iter().enumerate() {
                        q.rss[j] = f(&format!("rss_{r}"));
                    }
                    self.hq.insert(i, q);
                }
            }
            _ => {}
        }
    }

    /// `wall(s, up)`: the wall clock of an uptime in a boot, "" when that boot's clock is unknown.
    fn wall(&self, s: i64, up: f64) -> Num {
        self.off.get(&s).map(|o| up.trunc() + o)
    }

    /// The scorecard: the text and doctor's exit code.
    pub fn finish(self, inp: &Inputs) -> (String, i32) {
        let mut out = String::new();
        let nh = self.h.len();
        // ── time: each hour's wall span, the window end, the start of the ledger ──
        let mut hws: Vec<Num> = Vec::with_capacity(nh);
        let mut hwe: Vec<Num> = Vec::with_capacity(nh);
        let mut wend: Num = None;
        let mut lstart: Num = None;
        let mut blast: HashMap<i64, usize> = HashMap::new();
        let mut bfirst: HashMap<i64, usize> = HashMap::new();
        for (i, h) in self.h.iter().enumerate() {
            let s = h.s;
            let ws = if isn(&h.ft) {
                Some(num(&h.ft))
            } else {
                self.wall(s, h.f)
            };
            let we = if isn(&h.tt) {
                Some(num(&h.tt))
            } else {
                self.wall(s, h.t)
            };
            if let Some(e) = we
                && wend.is_none_or(|w| e > w)
            {
                wend = Some(e);
            }
            if let Some(b) = ws
                && lstart.is_none_or(|l| b < l)
            {
                lstart = Some(b);
            }
            if blast.get(&s).is_none_or(|&j| h.t > self.h[j].t) {
                blast.insert(s, i);
            }
            if bfirst.get(&s).is_none_or(|&j| h.f < self.h[j].f) {
                bfirst.insert(s, i);
            }
            hws.push(ws);
            hwe.push(we);
        }
        let Some(wend) = wend else {
            if nh > 0 {
                let _ = writeln!(
                    out,
                    "账本有 {nh} 小时的记录，但设备时钟一直没对上，排不进时间窗口（对时以后再看）"
                );
            } else {
                out.push_str("账本里还没有一小时的记录（上线一小时后再看）\n");
            }
            return (out, 1);
        };
        // holes between boots that no power-off explains: coverage gaps (§8)
        let mut holes: Vec<(f64, f64)> = Vec::new();
        for x in 1..self.sq.len() {
            let (s, p) = (self.sq[x], self.sq[x - 1]);
            let (Some(&bf), Some(&bl)) = (bfirst.get(&s), blast.get(&p)) else {
                continue;
            };
            let (Some(hs), Some(he)) = (hwe[bl], hws[bf]) else {
                continue;
            };
            if he - hs <= 300.0 {
                continue;
            }
            let b = self.boots.get(&s);
            if b.is_some_and(|b| {
                klass(&b.code) == "manual" || b.planned() || b.mode.contains("charg")
            }) {
                continue;
            }
            holes.push((hs, he));
        }
        let mut gaps: Vec<(String, Num, Num)> = Vec::new();
        for (s, src, f, t) in &self.gaps {
            gaps.push((src.clone(), self.wall(*s, num(f)), self.wall(*s, num(t))));
        }
        let c = Cov {
            h: &self.h,
            hws: &hws,
            hwe: &hwe,
            holes: &holes,
            gaps: &gaps,
            wend,
        };
        // ── reboots, in order: each boot line and the key.log backfills written before it ──
        struct Reboot {
            code: String,
            ledger: i64,
            w: Num,
            class: &'static str,
            place: &'static str,
            gapcol: bool,
        }
        let mut rb: Vec<Reboot> = Vec::new();
        for &s in &self.sq {
            for (bs, code, at) in &self.backfills {
                if *bs != s {
                    continue;
                }
                let y: String = at.chars().take(4).collect();
                // `substr(at, 1, 4) >= 2026` compares text ("null" passes; mktime then gives -1)
                let w = if y.as_str() >= "2026" {
                    Some(mktime(&at.replace(['-', ':'], " ")))
                } else {
                    self.off.get(&s).map(|o| o - 1.0)
                };
                rb.push(Reboot {
                    code: code.clone(),
                    ledger: 0,
                    w,
                    class: "",
                    place: "",
                    gapcol: false,
                });
            }
            if let Some(b) = self.boots.get(&s) {
                rb.push(Reboot {
                    code: b.code.clone(),
                    ledger: s,
                    w: self.off.get(&s).copied(),
                    class: "",
                    place: "",
                    gapcol: false,
                });
            }
        }
        let capend: HashMap<&str, &str> = awk::lines(&inp.cap)
            .filter_map(|l| {
                let x = awk::split_blank(l);
                Some((*x.first()?, x.get(1).copied().unwrap_or("")))
            })
            .collect();
        for i in 0..rb.len() {
            let mut cl = klass(&rb[i].code);
            // asked for (web, touch screen, `plan-reboot`): manual whatever the code says;
            // CN B31 logs a deliberate reboot as 1134, the modem-crash code
            if rb[i].ledger != 0 && self.boots.get(&rb[i].ledger).is_some_and(Boot::planned) {
                cl = "manual";
            }
            let ps = if i > 0 { rb[i - 1].ledger } else { 0 };
            let mut pl = "不明";
            if ps != 0
                && let Some(&b) = blast.get(&ps)
            {
                pl = if self.h[b].ab * 2.0 > self.h[b].awake {
                    "国外"
                } else {
                    "在家"
                };
            }
            if cl == "1182" {
                cl = "unknown";
                if ps != 0 {
                    let j = self.last_ssr.get(&ps);
                    let b8: String = self
                        .boots
                        .get(&ps)
                        .map(|b| b.id.chars().take(8).collect())
                        .unwrap_or_default();
                    if j.is_some_and(|&j| !self.res.contains_key(&self.ssr[j].id))
                        || capend
                            .get(b8.as_str())
                            .is_some_and(|v| looks_numeric(v) && num(v) == 1.0)
                    {
                        cl = "modem";
                    }
                }
            }
            let mut gapcol = false;
            if cl == "modem" {
                // before guard was up in the boot that ended: the S2a gap column
                match self.gs_up.get(&ps) {
                    _ if ps == 0 => gapcol = true,
                    None => gapcol = true,
                    Some(&g) => {
                        if let Some(&j) = self.last_ssr.get(&ps)
                            && self.ssr[j].up < g
                        {
                            gapcol = true;
                        }
                    }
                }
            }
            rb[i].class = cl;
            rb[i].place = pl;
            rb[i].gapcol = gapcol;
        }
        let summary = inp.summary;
        let wstart = |req: f64, change: Num| -> f64 {
            let mut s = wend - req.min(inp.w);
            if let Some(ch) = change
                && ch > s
            {
                s = ch;
            }
            if let Some(l) = lstart
                && l > s
            {
                s = l;
            }
            s
        };
        let tier = |viol: bool, cv: &str, ws: f64, req: f64| -> &'static str {
            if cv != "完整" {
                "没测"
            } else if viol {
                "不达标"
            } else if wend - ws < req - 60.0 {
                "注意"
            } else {
                "达标"
            }
        };
        let row = |out: &mut String,
                   id: &str,
                   name: &str,
                   val: &str,
                   tier: &str,
                   ws: f64,
                   req: f64,
                   cv: &str,
                   how: &str| {
            if summary {
                return;
            }
            let _ = write!(out, "  {id} {name}：{val} · {tier}");
            if req > 0.0 {
                let _ = write!(out, " · {} / {}", dur(wend - ws), dur(req));
            }
            if !cv.is_empty() {
                let _ = write!(out, " · 覆盖{cv}");
            }
            let _ = writeln!(out, " · {how}");
        };
        let sub = |out: &mut String, text: &str| {
            if !summary {
                let _ = writeln!(out, "      {text}");
            }
        };
        let inw = |x: Num, ws: f64| gt(x, ws) && le(x, wend) && x.is_some();
        if !summary {
            let _ = writeln!(out, "设备成绩单（{}，截至 {}）", inp.label, ymd_hm(wend));
            let _ = writeln!(
                out,
                "  账本：{} 次开机，{} 小时的记录，从 {} 起{}{}{}\n",
                self.sq.len(),
                nh,
                md_hm(lstart.unwrap_or(0.0)),
                if self.nuls > 0 {
                    format!("；{} 段有断电留下的空字节，已跳过", self.nuls)
                } else {
                    String::new()
                },
                if self.bad > 0 {
                    format!("；{} 行读不懂，已跳过", self.bad)
                } else {
                    String::new()
                },
                if self.newer > 0 {
                    format!("；{} 行是更新的格式，已跳过", self.newer)
                } else {
                    String::new()
                }
            );
            out.push_str("稳\n");
        }
        // S1 unexpected reboots (key.log reason codes)
        let ws = wstart(D7, inp.recov);
        let (mut n, mut sch, mut man, mut det) = (0, 0, 0, String::new());
        for r in &rb {
            if inw(r.w, ws) {
                if r.class == "scheduled" {
                    sch += 1;
                } else if r.class == "manual" {
                    man += 1;
                } else {
                    n += 1;
                    let _ = write!(
                        det,
                        "{} {}（{}，{}）；",
                        when(r.w),
                        r.code,
                        r.class,
                        r.place
                    );
                }
            }
        }
        let mut cv = "完整".to_string();
        if self.sq.iter().any(|s| self.bf_gap.contains(s)) {
            cv = "不完整（key.log 补记的锚点丢过一次）".into();
        }
        let s1n = n;
        let s1t = tier(n > 0, &cv, ws, D7);
        let mut also = Vec::new();
        if sch > 0 {
            also.push(format!("原厂定时 {sch} 次"));
        }
        if man > 0 {
            also.push(format!("人为 {man} 次"));
        }
        let v = format!(
            "{n} 次{}",
            if also.is_empty() {
                String::new()
            } else {
                format!("（另有{}）", also.join("、"))
            }
        );
        row(&mut out, "S1", "意外整机重启", &v, s1t, ws, D7, &cv, "自动");
        if !det.is_empty() {
            sub(&mut out, &det);
        }
        // S2a modem crash -> whole reboot, from guard up; the boot-gap column apart
        let ws = wstart(D7, inp.recov);
        let (mut n, mut ng) = (0, 0);
        for r in &rb {
            if inw(r.w, ws) && r.class == "modem" {
                if r.gapcol {
                    ng += 1;
                } else {
                    n += 1;
                }
            }
        }
        let cv = c.cov(ws, &["watcher", "capture"]);
        let v = format!("{n} 次（开机空档 {ng} 次）");
        row(
            &mut out,
            "S2a",
            "基带崩溃升级成整机重启",
            &v,
            tier(n > 0, &cv, ws, D7),
            ws,
            D7,
            &cv,
            "自动",
        );
        // S2b recovery time P95, from the ssr_result lines; S3 counts
        let mut lastnc: Num = None;
        for a in &self.nc {
            let x = self.wall(a.s, a.up);
            if let Some(x) = x
                && lastnc.is_none_or(|l| x > l)
            {
                lastnc = Some(x);
            }
        }
        let ws = wstart(D7, lastnc);
        // crashes in the order they happened (spool files are drained by name, not by time)
        let mut ord: Vec<usize> = (0..self.ssr.len()).collect();
        ord.sort_by(|&a, &b| {
            let (x, y) = (&self.ssr[a], &self.ssr[b]);
            x.s.cmp(&y.s)
                .then(x.up.partial_cmp(&y.up).unwrap_or(std::cmp::Ordering::Equal))
        });
        let (mut ph, mut pa) = (Vec::new(), Vec::new());
        let (mut nsl, mut nnd) = (0, 0);
        let mut nb: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        let thr = inp.thr.as_str();
        let empty = Res::default();
        let res = |id: &str| self.res.get(id).unwrap_or(&empty);
        for z in 0..ord.len() {
            let i = ord[z];
            let q = &self.ssr[i];
            if !inw(self.wall(q.s, q.up), ws) {
                continue;
            }
            if !self.res.contains_key(&q.id) && self.sq.last() == Some(&q.s) {
                continue; // still being followed
            }
            let r = res(&q.id);
            let pl = place(&q.net, &q.home);
            if r.result == "no_drop" {
                nnd += 1;
                continue;
            }
            if r.result == "merged" {
                continue; // its chain is counted at the crash that ends it
            }
            let mut v = if r.result == "ok" { num(&r.rec) } else { 600.0 };
            // merged crashes just before this one: one long outage from the first of them
            let mut slept = r.slept == "1";
            let mut y = z;
            while y >= 1
                && self.ssr[ord[y - 1]].s == q.s
                && res(&self.ssr[ord[y - 1]].id).result == "merged"
            {
                v += self.ssr[ord[y]].up - self.ssr[ord[y - 1]].up;
                if res(&self.ssr[ord[y - 1]].id).slept == "1" {
                    slept = true;
                }
                y -= 1;
            }
            if slept {
                nsl += 1; // asleep anywhere in the chain: kept apart, never for 达标
                continue;
            }
            if pl == "国外" {
                pa.push(v);
            } else {
                ph.push(v);
            }
            let b = if thr.is_empty() || !isn(&q.ppm) {
                "不明"
            } else if num(&q.ppm) <= num(thr) {
                "空闲"
            } else {
                "有流量"
            };
            nb.entry(format!("{pl}·{b}")).or_default().push(v);
        }
        let cv = c.cov(ws, &["watcher", "capture"]);
        let (nhh, naa) = (ph.len(), pa.len());
        let phv = if nhh > 0 { p95(&mut ph) } else { 0.0 };
        let pav = if naa > 0 { p95(&mut pa) } else { 0.0 };
        let p = |n: usize, v: f64| {
            if n > 0 {
                format!("P95 {} 秒（{n} 次）", int(v))
            } else {
                "没有崩溃".into()
            }
        };
        let mut val = format!("在家 {}；国外 {}", p(nhh, phv), p(naa, pav));
        if nnd > 0 {
            let _ = write!(val, "；没断网 {nnd} 次另列");
        }
        if nsl > 0 {
            let _ = write!(val, "；跟随中睡过 {nsl} 次另列");
        }
        let mut s2bt = tier(
            (nhh > 0 && phv > 45.0) || (naa > 0 && pav > 45.0),
            &cv,
            ws,
            D7,
        );
        if s2bt == "达标" && nsl > 0 && nhh == 0 && naa == 0 {
            s2bt = "注意";
        }
        row(
            &mut out,
            "S2b",
            "基带崩溃后恢复用时",
            &val,
            s2bt,
            ws,
            D7,
            &cv,
            "自动",
        );
        let mut det = String::new();
        for (x, mut v) in nb {
            let n = v.len();
            let _ = write!(det, "{x} P95 {} 秒（{n} 次）；", int(p95(&mut v)));
        }
        if !det.is_empty() {
            sub(&mut out, &det);
        }
        let mut nh3 = 0;
        let mut cnt: BTreeMap<String, u32> = BTreeMap::new();
        for q in &self.ssr {
            if !inw(self.wall(q.s, q.up), ws) {
                continue;
            }
            if place(&q.net, &q.home) == "国外" {
                let nr = if q.nr != "null" && !q.nr.is_empty() {
                    format!("/n{}", q.nr)
                } else {
                    String::new()
                };
                *cnt.entry(format!("{} {} {}{nr}", q.net, q.rat, q.band))
                    .or_default() += 1;
            } else {
                nh3 += 1;
            }
        }
        let mut na3 = 0;
        let mut det = String::new();
        for (x, n) in &cnt {
            na3 += n;
            let _ = write!(det, "{x} {n} 次；");
        }
        let s3n = nh3 + na3;
        let v = format!("在家 {nh3} 次；国外 {na3} 次");
        let how = if na3 > 0 {
            "在家：自动；国外有没有对策：人工"
        } else {
            "自动"
        };
        row(
            &mut out,
            "S3",
            "基带崩溃（按国家、制式、频段）",
            &v,
            tier(nh3 > 0, &cv, ws, D7),
            ws,
            D7,
            &cv,
            how,
        );
        if !det.is_empty() {
            sub(&mut out, &format!("国外：{det}"));
        }
        // S4 our programs, each from its last new build
        const PG: [&str; 5] = [
            "zte-agent",
            "zwrt-datad",
            "u60pro-devui",
            "u60-uid",
            "u60-guard.sh",
        ];
        const PL: [&str; 5] = ["agent", "datad", "触屏", "u60-uid", "guard"];
        let mut val = String::new();
        let mut worst = "达标";
        for q in 0..5 {
            let p = PG[q];
            let ch = self.ver_c.get(p).and_then(|&(s, u)| self.wall(s, u));
            let ws = wstart(D7, ch);
            let mut n = 0;
            for (a, prog, st) in &self.sx {
                if prog == p && st != "0" && st != "exit 0" && inw(self.wall(a.s, a.up), ws) {
                    n += 1;
                }
            }
            for (a, prog, cl, sh) in &self.pr {
                if prog == p && cl != "1" && sh != "1" && inw(self.wall(a.s, a.up), ws) {
                    n += 1;
                }
            }
            if p == "u60-guard.sh" {
                for a in &self.gx {
                    if inw(self.wall(a.s, a.up), ws) {
                        n += 1;
                    }
                }
            }
            let cv = c.cov(ws, &["crashlog", "guard"]);
            let t = tier(n > 0, &cv, ws, D7);
            let _ = write!(val, "{} {n}", PL[q]);
            if t != "达标" {
                let extra = if t == "注意" {
                    format!(" {}", dur(wend - ws))
                } else {
                    String::new()
                };
                let _ = write!(val, "（{t}{extra}）");
            }
            val.push('；');
            if t == "不达标"
                || (t == "没测" && worst != "不达标")
                || (t == "注意" && worst == "达标")
            {
                worst = t;
            }
        }
        row(
            &mut out,
            "S4",
            "我们的程序意外退出",
            trim(&val),
            worst,
            0.0,
            0.0,
            "",
            "自动；上机事务里的重启不算（10-04 起记）；手工重启仍算；退出码 0 的意外退出第二步才看得到",
        );
        // S5a / S5b the screen
        let ws = wstart(D7, inp.uidtk);
        let n = rb
            .iter()
            .filter(|r| inw(r.w, ws) && r.code == "1185")
            .count();
        row(
            &mut out,
            "S5a",
            "屏幕没界面导致重启",
            &format!("{n} 次"),
            tier(n > 0, "完整", ws, D7),
            ws,
            D7,
            "完整",
            "自动",
        );
        let n = self
            .ug
            .iter()
            .filter(|a| inw(self.wall(a.s, a.up), ws))
            .count();
        let cv = c.cov(ws, &["uidlog"]);
        let v = format!(
            "{n} 次{}",
            if inp.gaveup {
                "（现在仍停在原厂界面）"
            } else {
                ""
            }
        );
        row(
            &mut out,
            "S5b",
            "屏幕交还原厂界面",
            &v,
            tier(n > 0 || inp.gaveup, &cv, ws, D7),
            ws,
            D7,
            &cv,
            "自动",
        );
        // S6 datad degraded (one day)
        let ws = wstart(D1, None);
        let (mut n, mut tot, mut unk) = (0, 0.0, 0);
        for (a, d) in &self.dd {
            if inw(self.wall(a.s, a.up), ws) {
                n += 1;
                if isn(d) {
                    tot += num(d);
                } else {
                    unk += 1;
                }
            }
        }
        let cv = c.cov(ws, &["guard", "datad"]);
        let s6n = n;
        let v = if n > 0 {
            format!(
                "{n} 次，共 {}{}",
                dur(tot),
                if unk > 0 {
                    format!("（{unk} 次时长不明）")
                } else {
                    String::new()
                }
            )
        } else {
            format!("{n} 次")
        };
        row(
            &mut out,
            "S6",
            "datad 降级",
            &v,
            tier(n > 0, &cv, ws, D1),
            ws,
            D1,
            &cv,
            "自动",
        );
        row(
            &mut out,
            "S7",
            "上机安全和一致",
            "设备上的程序还没有全部对得上提交",
            "不达标",
            0.0,
            0.0,
            "",
            "E1 前靠已知事实",
        );
        // S8 exits. Tailscale: healthy seconds over the seconds guard could check it;
        // an hour line without ts_chk_s is from before that check, all of it unchecked.
        let ws = wstart(D7, None);
        let (mut con, mut cok, mut ton, mut tck, mut tok) = (0.0, 0.0, 0.0, 0.0, 0.0);
        let mut tlong = false;
        for i in 0..nh {
            if !(gt(hwe[i], ws) && lt(hws[i], wend)) {
                continue;
            }
            let Some(q) = self.hq.get(&i) else { continue };
            con += num(&q.con);
            cok += num(&q.cok);
            ton += num(&q.ton);
            let x = if isn(&q.tck) { num(&q.tck) } else { 0.0 };
            tck += x;
            if isn(&q.tck) && isn(&q.tok) {
                tok += num(&q.tok);
            }
            if num(&q.ton) - x > 600.0 {
                tlong = true;
            }
        }
        let mut cv = c.cov(ws, &["guard"]);
        // the coverage of Tailscale itself (docs/LEDGER.md §8): unchecked on-time <= 5%, and no
        // hour with over 10 minutes of it
        if cv == "完整" && ton != 0.0 && (ton - tck > 0.05 * ton || tlong) {
            cv = format!(
                "不完整（Tailscale 开着 {}，查到 {}{}）",
                dur(ton),
                dur(tck),
                if tlong {
                    "，有一小时里超过 10 分钟没查到"
                } else {
                    ""
                }
            );
        }
        let tv = if ton != 0.0 {
            if tck != 0.0 {
                format!("{}%", fixed(tok * 100.0 / tck, 1))
            } else {
                "没查到过".into()
            }
        } else {
            "没开过".into()
        };
        // under 99% even if every unchecked second was healthy
        let tbad = ton != 0.0 && tok + ton - tck < 0.99 * ton;
        let val = format!("Tailscale {tv}");
        let mut t = tier(tck != 0.0 && tok < 0.99 * tck, if ton != 0.0 { &cv } else { "没有记录" }, ws, D7);
        if tbad { t = "不达标"; }
        row(&mut out, "S8", "出口可用", &val, t, ws, D7, &cv, "自动");
        // ── 好用 ──
        if !summary {
            out.push_str("\n好用\n");
        }
        for (id, name, val, how) in [
            (
                "U1",
                "点了当场有反应",
                "首页每秒同步请求会卡几十到上百 ms，超过 50 ms",
                "第二步前靠已知事实",
            ),
            (
                "U2",
                "屏幕说的是真的",
                "崩溃后照常显示旧值；缺 SINR 当「中」；WAN 为空当「已连上」",
                "第二步前靠已知事实",
            ),
            (
                "U3",
                "出事说得清",
                "基带崩溃时屏幕没有一句解释",
                "第二步前靠已知事实",
            ),
            (
                "U4",
                "操作闭环",
                "原厂界面切制式没生效也不报错",
                "E4 前靠已知事实",
            ),
            (
                "U5",
                "不用 ssh",
                "出行中不得不开会话修",
                "设备锁做好前靠已知事实",
            ),
        ] {
            row(&mut out, id, name, val, "不达标", 0.0, 0.0, "", how);
        }
        // U6 alerts in the window, and the ▲ of the health check now
        let ws = wstart(D7, None);
        let (mut n, mut det) = (0, String::new());
        let (mut oldest, mut firstseq) = (String::new(), String::new());
        for l in awk::lines(&inp.alerts) {
            let a: Vec<&str> = if l.is_empty() {
                Vec::new()
            } else {
                l.split('\t').collect()
            };
            let ai = |k: usize| a.get(k).copied().unwrap_or("");
            if firstseq.is_empty() {
                firstseq = ai(0).to_string();
                oldest = ai(1).to_string();
            }
            if ai(3) == "sms-test" {
                continue;
            }
            let w = num(ai(1));
            if w > ws && w <= wend {
                n += 1;
                let _ = write!(det, "{} {}；", md_hm(w), ai(3));
            }
        }
        let (mut nw, mut wdet) = (0, String::new());
        for l in awk::lines(inp.warn.as_deref().unwrap_or("")) {
            nw += 1;
            let _ = write!(wdet, "{l}；");
        }
        // `firstseq > 1`: a number from split, compared as one when it looks like one
        let trimmed = !firstseq.is_empty()
            && if looks_numeric(&firstseq) {
                num(&firstseq) > 1.0
            } else {
                firstseq.as_str() > "1"
            }
            && num(&oldest) > ws;
        let cv = if trimmed {
            "不完整（告警队列剪过，最老一条晚于窗口起点）"
        } else {
            "完整"
        };
        let v = match inp.warn {
            Some(_) => format!("窗口内告警 {n} 条，现在 ▲/■ {nw} 项"),
            None => format!("窗口内告警 {n} 条，现在 ▲/■ 没测（没带体检）"),
        };
        row(
            &mut out,
            "U6",
            "体检和告警可信",
            &v,
            if cv == "完整" { "人工" } else { "没测" },
            ws,
            D7,
            cv,
            "人工：逐条看是不是真问题",
        );
        if !det.is_empty() {
            sub(&mut out, &format!("告警：{det}"));
        }
        if !wdet.is_empty() {
            sub(&mut out, &format!("体检：{wdet}"));
        }
        // ── 省 ──
        if !summary {
            out.push_str("\n省\n");
        }
        let ws = wstart(D7, None);
        let (mut e, mut es, mut part) = (0.0, 0.0, 0);
        let (mut ce, mut css) = ([0.0f64; 4], [0.0f64; 4]);
        for i in 0..nh {
            if !(gt(hwe[i], ws) && lt(hws[i], wend)) {
                continue;
            }
            let Some(p) = self.hp.get(&i) else { continue };
            if !isn(&p.e) {
                continue;
            }
            e += num(&p.e);
            if p.part == "1" {
                part += 1;
            }
            for j in 0..4 {
                css[j] += p.cs[j];
                if isn(&p.ce[j]) {
                    ce[j] += num(&p.ce[j]);
                }
            }
        }
        for v in css {
            es += v;
        }
        let cv = c.cov(ws, &["guard"]);
        let p1v = if es != 0.0 {
            format!("{} mW", fixed(e * 3600.0 / es, 0))
        } else {
            String::new()
        };
        let v = if es != 0.0 {
            format!(
                "平均 {p1v}（{} 的样本{}）",
                dur(es),
                if part > 0 {
                    format!("，{part} 小时只有部分能量")
                } else {
                    String::new()
                }
            )
        } else {
            "没有能量记录".into()
        };
        row(
            &mut out,
            "P1",
            "真实使用的续航",
            &v,
            "没测（目标待定）",
            ws,
            D7,
            &cv,
            "自动",
        );
        let mut val = String::new();
        for (j, cl) in ["亮屏在家", "亮屏国外", "息屏在家", "息屏国外"]
            .iter()
            .enumerate()
        {
            let x = if css[j] >= 21_600.0 {
                format!("{} mW", fixed(ce[j] * 3600.0 / css[j], 0))
            } else {
                "样本不足".into()
            };
            let _ = write!(val, "{cl} {x}；");
        }
        row(
            &mut out,
            "P2",
            "同类场景的耗电",
            trim(&val),
            "没测（目标待定）",
            ws,
            D7,
            &cv,
            "自动；每格少于 6 小时写样本不足；小时记录不分空闲和有流量",
        );
        let stb = awk::lines(inp.standby.as_deref().unwrap_or(""))
            .next()
            .unwrap_or("");
        let sb = match inp.standby {
            Some(_) => stb.split('\t').nth(1).unwrap_or(""),
            None => "没带体检",
        };
        let t = if inp.standby.is_none() || sb.contains("未校准") || sb.contains("没有息屏记录")
        {
            "没测"
        } else if sb.contains("格式旧") || sb.contains("基线过期") {
            "注意"
        } else {
            "达标"
        };
        row(
            &mut out,
            "P3",
            "待机判断正确",
            sb,
            t,
            0.0,
            0.0,
            "",
            "自动（只判空闲的息屏分钟，基线过期的列停判）",
        );
        let ch = self
            .ver_c
            .get("zwrt-datad")
            .and_then(|&(s, u)| self.wall(s, u));
        let ws = wstart(D1, ch);
        let (mut cpu, mut nc) = (0.0, 0);
        for i in 0..nh {
            if !(gt(hwe[i], ws) && lt(hws[i], wend)) {
                continue;
            }
            if let Some(q) = self.hq.get(&i)
                && isn(&q.cpu)
            {
                cpu += num(&q.cpu);
                nc += 1;
            }
        }
        let cv = c.cov(ws, &["guard"]);
        let v = if nc > 0 {
            format!("平均 {}%（{nc} 小时）", fixed(cpu / nc as f64 / 10.0, 1))
        } else {
            "没有记录".into()
        };
        let t = tier(
            nc > 0 && cpu / nc as f64 > 20.0,
            if nc > 0 { &cv } else { "没有记录" },
            ws,
            D1,
        );
        row(&mut out, "P4a", "datad 的 CPU", &v, t, ws, D1, &cv, "自动");
        let x = self.datad_x.as_str();
        let p4b = x == "ubus=socket" || x == "ubus=auto";
        let v = if !p4b {
            "cli 后端，每读一次起一个 ubus 进程"
        } else if x == "ubus=auto" {
            "socket 后端（auto：连不上 ubusd 才临时退回 CLI）"
        } else {
            "socket 后端"
        };
        row(
            &mut out,
            "P4b",
            "读数不派生进程",
            v,
            if p4b { "达标" } else { "不达标" },
            0.0,
            0.0,
            "",
            "设计事实",
        );
        // P5 memory over boots that ran 24 h; oom kills
        let ws = wstart(D7, None);
        let (mut val, mut viol, mut long, mut cmp) = (String::new(), false, false, 0);
        for &s in &self.sq {
            let Some(&l) = blast.get(&s) else { continue };
            if self.h[l].t < 86_400.0 {
                continue;
            }
            long = true;
            let mut b: Option<usize> = None;
            for (i, h) in self.h.iter().enumerate() {
                if h.s == s
                    && h.t >= 3600.0
                    && self.hq.contains_key(&i)
                    && b.is_none_or(|b| h.f < self.h[b].f)
                {
                    b = Some(i);
                }
            }
            let (Some(b), Some(ql)) = (b, self.hq.get(&l)) else {
                continue;
            };
            let qb = &self.hq[&b];
            for (j, r) in RS.iter().enumerate() {
                let (rb_, rl) = (qb.rss[j].as_str(), ql.rss[j].as_str());
                if isn(rb_) && isn(rl) && num(rb_) > 0.0 {
                    cmp += 1;
                    let g = (num(rl) - num(rb_)) * 100.0 / num(rb_);
                    if g > 10.0 {
                        viol = true;
                        let _ = write!(val, "{r} +{}%；", fixed(g, 0));
                    }
                }
            }
        }
        let n = self
            .oom
            .iter()
            .filter(|a| inw(self.wall(a.s, a.up), ws))
            .count();
        let cv = c.cov(ws, &["guard", "watcher"]);
        let m = if !long {
            "没有连续运行满 24 小时的开机"
        } else if cmp == 0 {
            "没有内存记录"
        } else if !val.is_empty() {
            trim(&val)
        } else {
            "RSS 增长都在 10% 以内"
        };
        let v = format!("{m}；OOM {n} 次");
        let t = tier(
            viol || n > 0,
            if long && cmp > 0 { &cv } else { "没有记录" },
            ws,
            D7,
        );
        row(&mut out, "P5", "内存不涨", &v, t, ws, D7, &cv, "自动");
        // P6 throttling and thermal level rises
        let (mut thr_s, mut nul) = (0.0, 0);
        for i in 0..nh {
            if !(gt(hwe[i], ws) && lt(hws[i], wend)) {
                continue;
            }
            let Some(p) = self.hp.get(&i) else { continue };
            if isn(&p.thr) {
                thr_s += num(&p.thr);
            } else {
                nul += 1;
            }
        }
        let mut n = 0;
        let mut fl: HashMap<i64, f64> = HashMap::new();
        for (a, lv) in &self.th {
            let Some(v) = lv.strip_prefix("0x").and_then(hex) else {
                continue;
            };
            let Some(&first) = fl.get(&a.s) else {
                fl.insert(a.s, v);
                continue;
            };
            if v > first && inw(self.wall(a.s, a.up), ws) {
                n += 1;
            }
        }
        let mut cv = c.cov(ws, &["guard", "watcher"]);
        if nul > 0 {
            cv = format!("不完整（{nul} 小时没有降频数据）");
        }
        let v = format!("降频 {}；过热等级上升 {n} 次", dur(thr_s));
        row(
            &mut out,
            "P6",
            "温度和降频",
            &v,
            tier(thr_s > 0.0 || n > 0, &cv, ws, D7),
            ws,
            D7,
            &cv,
            "自动",
        );
        if summary {
            let _ = writeln!(
                out,
                "  账本（{}，截至 {}）：意外重启 {s1n} 次（{s1t}）、基带崩溃 {s3n} 次、datad 降级今天 {s6n} 次",
                inp.label,
                md_hm(wend)
            );
            let _ = writeln!(
                out,
                "  耗电：{}",
                if p1v.is_empty() {
                    "还没有能量记录".to_string()
                } else {
                    format!("平均 {p1v}")
                }
            );
            out.push_str("  详细成绩单：doctor.sh --report 7d\n");
        }
        (out, 0)
    }
}

/// doctor's `strtonum_` after `0x`: hex digits, any case; none at all is 0; a bad digit is "".
fn hex(h: &str) -> Option<f64> {
    let mut v = 0.0;
    for c in h.chars() {
        v = v * 16.0 + f64::from(c.to_digit(16)?);
    }
    Some(v)
}

/// Coverage of [ws, end] for some sources (doctor's `cov`).
struct Cov<'a> {
    h: &'a [Hour],
    hws: &'a [Num],
    hwe: &'a [Num],
    holes: &'a [(f64, f64)],
    gaps: &'a [(String, Num, Num)],
    wend: f64,
}

impl Cov<'_> {
    /// The awake seconds, the seconds the sources were not working (the `g_*` of hour lines,
    /// plus holes between boots that no power-off explains), and whether one gap passed 10 min.
    fn cov(&self, ws: f64, srcs: &[&str]) -> String {
        let wend = self.wend;
        let (mut a, mut g, mut long) = (0.0, 0.0, false);
        let idx: Vec<usize> = srcs
            .iter()
            .filter_map(|s| GS.iter().position(|g| g == s))
            .collect();
        for (i, h) in self.h.iter().enumerate() {
            let (Some(hs), he) = (self.hws[i], self.hwe[i]) else {
                continue;
            };
            if le(he, ws) || hs >= wend {
                continue;
            }
            let he = he.unwrap_or(0.0);
            let mut f = 1.0;
            if he > hs {
                let x = he.min(wend) - hs.max(ws);
                f = x / (he - hs);
            }
            a += h.awake * f;
            let mut gi = 0.0;
            for &j in &idx {
                gi += h.g[j] * f;
                if h.g[j] > 600.0 {
                    long = true;
                }
            }
            g += if gi > h.awake * f { h.awake * f } else { gi };
        }
        for &(s, e) in self.holes {
            // only the part inside the window counts
            let x = e.min(wend) - s.max(ws);
            if x <= 0.0 {
                continue;
            }
            g += x;
            a += x;
            if x > 600.0 {
                long = true;
            }
        }
        for (src, s, e) in self.gaps {
            let (Some(s), Some(e)) = (s, e) else { continue };
            let x = e.min(wend) - s.max(ws);
            if srcs.contains(&src.as_str()) && x > 600.0 {
                long = true;
            }
        }
        if a <= 0.0 {
            return "没有记录".into();
        }
        if g > 0.05 * a || long {
            return format!(
                "不完整（缺 {} / 醒着 {}{}）",
                dur(g),
                dur(a),
                if long {
                    "，有一段超过 10 分钟"
                } else {
                    ""
                }
            );
        }
        "完整".into()
    }
}
