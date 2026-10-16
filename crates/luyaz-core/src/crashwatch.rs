//! Crash watcher (u60-sentinel.md §8 D11, T10): the guard's watcher loop (touch-ui
//! scripts/u60-guard.sh `watch_*`, docs/LEDGER.md §8–9) moved into the core, woken by events
//! instead of every 2 s.
//!
//! Same events, same ids, same merge/timeout rules as the guard: `ssr`, `ssr_result`, `ssr_dump`,
//! `oom`, `thermal`, `sleep`, `link`. What changed with the way it wakes (the IO edge in
//! `crashwatch/io.rs`):
//!
//! - Kernel records come from `/dev/kmsg` itself (poll: the kernel wakes us when there is a new
//!   one; a burst is read at most every [`INTERVAL_MS`], the guard's resolution), not from the
//!   capture file on flash.
//! - The link (WAN default route in the main table) is looked at on every wake, and rtnetlink
//!   route/address messages wake us when it may have changed: `link.res` is 0 (seen when it
//!   happened) while the subscription is healthy, the seconds since the last look otherwise.
//! - Only while a crash is being followed, a dump window is open or a pause waits for its
//!   evidence does a timer run (every [`INTERVAL_MS`]); otherwise nothing wakes us but events.
//! - Sleep: no loop, so no "late loop" to notice; a pause is CLOCK_BOOTTIME running ahead of
//!   CLOCK_MONOTONIC by more than [`GAP_S`] between two wakes, which only suspend does. Its
//!   evidence is as before (`pm`: a kernel suspend line, `stats`: the suspend counter went up);
//!   with neither by [`SETTLE_S`] later it is `ev="clock"` — the clocks themselves are the
//!   evidence, so it counts as sleep (the guard's `inferred` never comes from here). `from` is
//!   `to` minus the time suspended: exact length, start inferred from it.
//! - `wan_ppm`: packets a minute over the latest read pair at least a minute apart; reads are
//!   taken on wakes that happen anyway, so in a quiet hour the pair can span longer than a minute.
//! - Dump baseline: re-listed when the dump directory changed outside a window (inotify), not
//!   every 60 s; without inotify, on a wake at least 60 s after the last listing (the guard's rule).
//!
//! Sans-IO: [`Watch::step`] takes one wake's kernel records and asks [`Look`] for what it needs
//! (only what the guard would read at that point), giving events. The state is serializable so
//! a restarted core carries on where the last one stopped (`resumed=1`, `gap_s`), as a restarted
//! guard watcher does. Events wait in [`Watch::pending`] until written (ids are stable, so a
//! line written twice after a crash is the same line twice, never a different one).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// The guard's loop interval: samples while following, and the kmsg burst cap.
pub const INTERVAL_MS: u64 = 2_000;
/// Samples further apart than this while following count into `gap_s` (WATCH_GAP).
pub const GAP_S: u64 = 10;
/// A pause waits this long for its evidence before it is settled as `clock`.
pub const SETTLE_S: u64 = 6;
/// Crash lines within this many kernel seconds of the first are the same crash.
const SAME_CRASH_S: u64 = 60;
const NO_DROP_S: u64 = 30;
const NO_DROP_WAIT_S: u64 = 90;
const TIMEOUT_S: u64 = 600;
const RX_WAIT_S: u64 = 60;
const DUMP_WINDOW_S: u64 = 90;
const DUMP_BASE_S: u64 = 60;
const PPM_S: u64 = 60;

/// One kernel record: sequence number, timestamp (µs), message (first line).
#[derive(Clone, Debug, PartialEq)]
pub struct Rec {
    pub seq: u64,
    pub ts_us: u64,
    pub text: String,
}

/// A raw `/dev/kmsg` record (or a capture-file line): `<pri>,<seq>,<ts>,<flags>[,…];<text>`.
/// Continuation lines (starting with a space) and anything else that does not look like a
/// record header are not records (the guard's `^[0-9]+,[0-9]+,[0-9]+,[^;]*;`).
pub fn parse_rec(raw: &str) -> Option<Rec> {
    let line = raw.split('\n').next()?;
    let (head, text) = line.split_once(';')?;
    let mut f = head.splitn(4, ',');
    let pri = f.next()?;
    let seq = f.next()?;
    let ts = f.next()?;
    f.next()?; // the flags field must be there (may be empty only if followed by ';')
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(pri) || !digits(seq) || !digits(ts) {
        return None;
    }
    Some(Rec {
        seq: seq.parse().ok()?,
        ts_us: ts.parse().ok()?,
        text: text.to_string(),
    })
}

/// What a record is to the watcher (the guard's `watch_read` patterns).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Find {
    Crash,
    Oom,
    Thermal,
    Suspend,
}

pub fn classify(text: &str) -> Option<Find> {
    let l = text.to_ascii_lowercase();
    let recovering = l
        .find("recovering ")
        .is_some_and(|i| l[i + "recovering ".len()..].contains("remoteproc"));
    if [
        "fatal error received",
        "watchdog received",
        "crash detected",
        "rproc recovery",
        "subsystem restart",
        "err_fatal",
    ]
    .iter()
    .any(|p| l.contains(p))
        || recovering
    {
        Some(Find::Crash)
    } else if text.contains("Out of memory: Kill") || text.contains("out of memory: Kill") {
        Some(Find::Oom)
    } else if text.contains("zte_thermal_update_throttling_level") {
        Some(Find::Thermal)
    } else if text.contains("PM: suspend") || text.contains("PM: resume") {
        Some(Find::Suspend)
    } else {
        None
    }
}

/// The guard's `ledger_clean`: printable ASCII without `"` or `\`, tabs/newlines as spaces,
/// at most 160 chars.
pub fn clean(s: &str) -> String {
    s.chars()
        .map(|c| {
            if matches!(c, '\t' | '\r' | '\n') {
                ' '
            } else {
                c
            }
        })
        .filter(|c| (' '..='~').contains(c) && *c != '"' && *c != '\\')
        .take(160)
        .collect()
}

/// The guard's `jdash`: a short string (16 chars) or null for empty / `-`.
fn dash(s: Option<&str>) -> Value {
    match s {
        None | Some("") | Some("-") => Value::Null,
        Some(s) => Value::String(clean(s).chars().take(16).collect()),
    }
}

/// MCCs whose networks use 3-digit MNCs (the guard's `mnc3`, same list as screen.rs).
pub fn mnc3(mcc: i64) -> bool {
    matches!(
        mcc,
        302 | 310
            ..=316
                | 334
                | 338
                | 342
                | 344
                | 346
                | 348
                | 354
                | 356
                | 358
                | 360
                | 365
                | 376
                | 405
                | 708
                | 722
                | 732
    )
}

/// The network as last read (the guard's `$LTMP/net`): radio, band, NR band, serving MCC-MNC,
/// SIM home MCC-MNC, and the boottime second it was read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Net {
    pub at_s: Option<u64>,
    pub rat: Option<String>,
    pub band: Option<String>,
    pub nrband: Option<String>,
    pub net: Option<String>,
    pub home: Option<String>,
}

impl Net {
    /// From the `signal` and `sim` blocks (stale = not read, as the guard does).
    pub fn from_blocks(signal: Option<&Value>, sim: Option<&Value>, at_s: Option<u64>) -> Net {
        let mut n = Net {
            at_s,
            ..Default::default()
        };
        let cut = |v: &Value| -> Option<String> {
            let s = match v {
                Value::String(s) => s.clone(),
                Value::Number(x) => x.to_string(),
                _ => return None,
            };
            let s: String = s
                .chars()
                .map(|c| if ('!'..='~').contains(&c) { c } else { '_' })
                .filter(|c| *c != '"' && *c != '\\')
                .take(24)
                .collect();
            (!s.is_empty()).then_some(s)
        };
        if let Some(d) = signal {
            n.rat = cut(&d["type"]);
            n.band = cut(&d["band"]);
            n.nrband = cut(&d["nr_band"]);
            if let (Some(m), Some(c)) = (d["mcc"].as_i64(), d["mnc"].as_i64())
                && m > 0
                && c >= 0
            {
                n.net = Some(if mnc3(m) {
                    format!("{m:03}-{c:03}")
                } else {
                    format!("{m:03}-{c:02}")
                });
            }
        }
        if let Some(imsi) = sim.and_then(|d| d["imsi"].as_str()) {
            let h: String = imsi.chars().take(6).collect();
            if h.len() == 6
                && h.bytes().all(|b| b.is_ascii_digit())
                && (b'2'..=b'7').contains(&h.as_bytes()[0])
            {
                let m = &h[..3];
                let mn = if mnc3(m.parse().unwrap_or(0)) {
                    &h[3..6]
                } else {
                    &h[3..5]
                };
                n.home = Some(format!("{m}-{mn}"));
            }
        }
        n
    }
}

/// What the watcher may read during a step; each is asked only when the guard would read it.
pub trait Look {
    /// The main table's default route leaves by the WAN interface (`/proc/net/route`).
    fn route(&mut self) -> bool;
    /// The WAN interface has an IPv4 address.
    fn addr(&mut self) -> bool;
    /// (rx bytes, packets both ways) on the WAN interface; None when it is gone.
    fn netdev(&mut self) -> (Option<u64>, Option<u64>);
    /// The dump directory: (name, size) of each regular file; None when it cannot be listed.
    fn dumps(&mut self) -> Option<Vec<(String, u64)>>;
    /// The suspend counter (`/sys/power/suspend_stats/success`).
    fn suspend(&mut self) -> Option<u64>;
    /// The network as last read.
    fn net(&mut self) -> Net;
}

/// One wake: the clocks, the records read, and what the IO edge knows about how it woke.
#[derive(Clone, Debug, Default)]
pub struct Wake {
    /// CLOCK_BOOTTIME (ms): the clock of `/proc/uptime`, as the guard's `_nu`.
    pub up_ms: u64,
    /// CLOCK_MONOTONIC (ms): stops in suspend.
    pub mono_ms: u64,
    pub recs: Vec<Rec>,
    /// The route subscription has been continuous since the last wake (`link.res` = 0).
    pub nl_ok: bool,
    /// The dump directory changed (inotify) since the last wake.
    pub dump_changed: bool,
    /// Dump-directory notifications work at all (else the 60 s re-listing rule).
    pub dump_notify: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct Ssr {
    id: String,
    t0: u64,
    kts: u64,
    down: Option<u64>,
    up: Option<u64>,
    rxl: Option<u64>,
    rxat: Option<u64>,
    last: u64,
    /// the last sample's moment in ms: one sample per wake, even two wakes in one second
    #[serde(default)]
    last_ms: u64,
    gap: u64,
    res: bool,
    slept: bool,
    nd: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
struct Dump {
    ssr: String,
    until: u64,
    /// sizes at the last listing, of the files not reported yet
    sizes: BTreeMap<String, u64>,
    /// when that listing was taken (boottime ms): a size "held still" only across an interval
    #[serde(default)]
    sizes_at: u64,
    reported: BTreeSet<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct Pause {
    from: u64,
    to: u64,
}

/// The watcher's state (persisted on tmpfs between core restarts in one boot).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Watch {
    pub boot: String,
    /// `true` until the first read is done when this boot is the watcher's first go-live.
    pub base: bool,
    kseq: u64,
    ssrlast: u64,
    ssr: Option<Ssr>,
    nl: u64,
    ns: u64,
    link: Option<bool>,
    rtseen: bool,
    linkup: u64,
    pkup: u64,
    pk: Option<u64>,
    ppm: Option<u64>,
    dump: Option<Dump>,
    dbase: BTreeSet<String>,
    dbup: u64,
    prev: u64,
    /// BOOTTIME − MONOTONIC (ms) at the last wake: time suspended so far.
    slept_ms: Option<u64>,
    sus: Option<u64>,
    pause: Option<Pause>,
    /// evidence read on the last wake and not used by a pause yet
    ev_prev: Option<(u64, String)>,
    /// records seen out of order or lost (EPIPE) this boot
    pub lost: u64,
    /// Events not written yet.
    pub pending: Vec<Value>,
    #[serde(skip)]
    catchup: bool,
}

fn b8(boot: &str) -> String {
    boot.chars()
        .filter(|c| c.is_ascii_hexdigit())
        .take(8)
        .collect()
}

fn nud(up_ms: u64) -> f64 {
    ((up_ms / 1000) as f64) + ((up_ms % 1000) / 100) as f64 / 10.0
}

impl Watch {
    /// A watcher for this boot; `first_live` when the watcher has never run on this device
    /// (the guard's missing `state/watch-init`): what the kernel holds now is taken as seen.
    pub fn new(boot: &str, first_live: bool) -> Watch {
        Watch {
            boot: boot.to_string(),
            base: first_live,
            catchup: true,
            ..Default::default()
        }
    }

    /// The state a previous core left, for this boot; a crash it was following carries on
    /// (`resumed`). None when it is another boot's or unreadable.
    pub fn resume(json: &str, boot: &str) -> Option<Watch> {
        let mut w: Watch = serde_json::from_str(json).ok()?;
        if w.boot != boot {
            return None;
        }
        if let Some(s) = w.ssr.as_mut() {
            s.res = true;
        }
        w.catchup = true;
        Some(w)
    }

    pub fn kseq(&self) -> u64 {
        self.kseq
    }

    /// When the watcher wants to be woken without an event (boottime ms), if at all.
    pub fn next_tick(&self) -> Option<u64> {
        let prev_ms = self.prev * 1000;
        let mut t: Option<u64> = None;
        if self.ssr.is_some() || self.dump.is_some() {
            t = Some(prev_ms + INTERVAL_MS);
        }
        if let Some(p) = &self.pause {
            let s = (p.to + SETTLE_S) * 1000;
            t = Some(t.map_or(s, |x| x.min(s)));
        }
        t
    }

    fn event(&mut self, up_ms: u64, k: &str, id: String, fields: Value) {
        let mut v =
            json!({"t": Value::Null, "up": nud(up_ms), "boot": b8(&self.boot), "k": k, "id": id});
        if let (Some(o), Value::Object(f)) = (v.as_object_mut(), fields) {
            for (a, b) in f {
                o.insert(a, b);
            }
        }
        self.pending.push(v);
    }

    /// One wake. Order as the guard's `watch_step`: records, sleep, follow, dump, link, packets.
    pub fn step(&mut self, w: &Wake, look: &mut dyn Look) {
        let nu = w.up_ms / 1000;
        let b = b8(&self.boot);
        // ── records ──
        let mut pm = false;
        for r in &w.recs {
            if r.seq <= self.kseq {
                continue; // read twice (a restart re-reads the ring): taken already
            }
            if self.kseq > 0 && r.seq > self.kseq + 1 {
                self.lost += r.seq - self.kseq - 1;
            }
            self.kseq = r.seq;
            if self.base {
                continue;
            }
            match classify(&r.text) {
                Some(Find::Crash) => self.crash(w, look, r),
                Some(Find::Oom) => self.event(
                    w.up_ms,
                    "oom",
                    format!("oom-{b}-{}", r.seq),
                    json!({"line": clean(&r.text)}),
                ),
                Some(Find::Thermal) => {
                    let level = r.text.find("throttling_level() to 0x").map(|i| {
                        let s = &r.text[i + "throttling_level() to ".len()..];
                        let end = 2 + s[2..]
                            .find(|c: char| !c.is_ascii_hexdigit())
                            .unwrap_or(s.len() - 2);
                        s[..end].to_string()
                    });
                    self.event(
                        w.up_ms,
                        "thermal",
                        format!("th-{b}-{}", r.seq),
                        json!({"level": level.map(|s| clean(&s)), "line": clean(&r.text)}),
                    )
                }
                Some(Find::Suspend) => pm = true,
                None => {}
            }
        }
        self.base = false;
        // ── sleep: the clocks, and evidence for the pause ──
        let mut evh: Option<&str> = None;
        let sus = look.suspend();
        if let Some(s) = sus {
            if self.sus.is_some_and(|o| s > o) {
                evh = Some("stats");
            }
            self.sus = Some(s);
        }
        if pm {
            evh = Some("pm");
        }
        let slept = w.up_ms.saturating_sub(w.mono_ms);
        let mut found_pause = false;
        if let Some(prev) = self.slept_ms
            && !self.catchup
            && slept > prev + GAP_S * 1000
        {
            if self.pause.is_some() {
                self.sleep(w.up_ms, "clock");
            }
            let d = (slept - prev) / 1000;
            self.pause = Some(Pause {
                from: nu.saturating_sub(d),
                to: nu,
            });
            found_pause = true;
            // the suspend line may have been read on the wake just before the pause
            if evh.is_none()
                && let Some((at, e)) = &self.ev_prev
                && *at == self.prev
            {
                evh = Some(if e == "pm" { "pm" } else { "stats" });
            }
        }
        self.slept_ms = Some(slept);
        let mut used = false;
        if let Some(p) = &self.pause {
            if let Some(e) = evh {
                self.sleep(w.up_ms, e);
                used = true;
            } else if !found_pause && nu >= p.to + SETTLE_S {
                self.sleep(w.up_ms, "clock");
            }
        }
        self.ev_prev = match (evh, used) {
            (Some(e), false) => Some((nu, e.to_string())),
            _ => None,
        };
        // ── follow, dumps, link, packets ──
        self.follow(w, look);
        self.dump_step(w, look, false);
        let relist = if w.dump_notify {
            w.dump_changed
        } else {
            nu.saturating_sub(self.dbup) >= DUMP_BASE_S
        };
        if self.dump.is_none() && relist {
            self.dump_base(nu, look);
        }
        self.link_step(w, look);
        self.ppm_step(nu, look);
        self.prev = nu;
        self.catchup = false;
    }

    fn sleep(&mut self, up_ms: u64, ev: &str) {
        let Some(p) = self.pause.take() else { return };
        self.ns += 1;
        let id = format!("sleep-{}-{}", b8(&self.boot), self.ns);
        if let Some(s) = self.ssr.as_mut()
            && p.to > s.t0
        {
            s.slept = true;
        }
        self.event(
            up_ms,
            "sleep",
            id,
            json!({"from": p.from, "to": p.to, "ev": ev}),
        );
    }

    fn wan_sample(&mut self, look: &mut dyn Look) -> (bool, Option<u64>) {
        let rt = look.route();
        let ad = look.addr();
        let (rx, _) = look.netdev();
        // a device that never had its default route in the main table: the address alone
        (ad && (rt || !self.rtseen), rx)
    }

    fn crash(&mut self, w: &Wake, look: &mut dyn Look, r: &Rec) {
        if r.seq <= self.ssrlast {
            return; // opened already
        }
        let kts = r.ts_us / 1_000_000;
        if let Some(s) = &self.ssr {
            if s.up.is_some() {
                self.result(w.up_ms, "ok");
            } else if s.nd {
                self.result(w.up_ms, "no_drop");
            } else if kts.saturating_sub(s.kts) <= SAME_CRASH_S {
                return; // the same crash, still printing
            } else {
                self.result(w.up_ms, "merged");
            }
        }
        self.open(w, look, r, kts);
    }

    fn open(&mut self, w: &Wake, look: &mut dyn Look, r: &Rec, kts: u64) {
        let nu = w.up_ms / 1000;
        let id = format!("ssr-{}-{}", b8(&self.boot), r.seq);
        let n = look.net();
        let age = n.at_s.filter(|t| *t <= nu).map(|t| nu - t);
        let line = match r.text.find("fatal error received: ") {
            Some(i) => &r.text[i + "fatal error received: ".len()..],
            None => &r.text[..],
        };
        self.event(
            w.up_ms,
            "ssr",
            id.clone(),
            json!({
                "kseq": r.seq, "line": clean(line), "rat": dash(n.rat.as_deref()),
                "band": dash(n.band.as_deref()), "nrband": dash(n.nrband.as_deref()),
                "net": dash(n.net.as_deref()), "home": dash(n.home.as_deref()),
                "net_age": age, "wan_ppm": self.ppm,
            }),
        );
        self.ssrlast = r.seq;
        let (up, rx) = self.wan_sample(look);
        let mut s = Ssr {
            id: id.clone(),
            t0: nu,
            kts,
            down: None,
            up: None,
            rxl: rx,
            rxat: None,
            last: nu,
            last_ms: w.up_ms,
            gap: 0,
            res: false,
            slept: false,
            nd: false,
        };
        if self.catchup {
            // found in the first read after a start: we were not watching
            s.res = true;
            if nu >= self.prev {
                s.gap = nu - self.prev;
            }
        }
        if !up {
            s.down = Some(nu);
        }
        self.ssr = Some(s);
        // dump window
        if self.dump.is_some() {
            self.dump_step(w, look, true);
        }
        self.dump = Some(Dump {
            ssr: id,
            until: nu + DUMP_WINDOW_S,
            ..Default::default()
        });
        self.dump_step(w, look, false); // the window's first listing
    }

    fn result(&mut self, up_ms: u64, how: &str) {
        let Some(s) = self.ssr.take() else { return };
        let rr = (how == "ok").then_some(s.up.map(|u| u - s.t0)).flatten();
        let rx = match how {
            "ok" | "no_drop" => s.rxat.map(|a| a - s.t0),
            _ => None,
        };
        self.event(
            up_ms,
            "ssr_result",
            format!("res-{}", s.id),
            json!({
                "ssr": s.id, "result": how, "recovered_s": rr, "rx_seen_s": rx,
                "down_seen": i32::from(s.down.is_some()), "resumed": i32::from(s.res),
                "gap_s": s.gap, "slept": i32::from(s.slept),
            }),
        );
    }

    fn follow(&mut self, w: &Wake, look: &mut dyn Look) {
        let nu = w.up_ms / 1000;
        let Some(last_ms) = self.ssr.as_ref().map(|s| s.last_ms) else {
            return;
        };
        if last_ms == w.up_ms {
            return; // opened on this wake: sampled already
        }
        let (up, rx) = self.wan_sample(look);
        let s = self.ssr.as_mut().expect("open");
        let fe = nu.saturating_sub(s.last);
        if fe > GAP_S {
            s.gap += fe - INTERVAL_MS / 1000;
        }
        s.last = nu;
        s.last_ms = w.up_ms;
        let fr = matches!((rx, s.rxl), (Some(a), Some(b)) if a > b);
        s.rxl = rx;
        let fa = nu.saturating_sub(s.t0);
        let mut end: Option<&str> = None;
        if s.nd {
            if fr {
                s.rxat = Some(nu);
            }
            if s.rxat.is_some() || fa >= NO_DROP_WAIT_S {
                end = Some("no_drop");
            }
        } else if s.down.is_none() {
            if !up {
                s.down = Some(nu);
            } else {
                if fr && s.rxat.is_none() {
                    s.rxat = Some(nu);
                }
                if fa >= NO_DROP_S {
                    s.nd = true;
                    if s.rxat.is_some() {
                        end = Some("no_drop");
                    }
                }
            }
        } else if s.up.is_none() {
            if up {
                s.up = Some(nu);
                s.rxat = None;
            } else if fa >= TIMEOUT_S {
                end = Some("timeout");
            }
        } else {
            if fr {
                s.rxat = Some(nu);
            }
            if s.rxat.is_some() || nu.saturating_sub(s.up.unwrap_or(nu)) >= RX_WAIT_S {
                end = Some("ok");
            }
        }
        if let Some(e) = end {
            self.result(w.up_ms, e);
        }
    }

    fn dump_base(&mut self, nu: u64, look: &mut dyn Look) {
        if let Some(l) = look.dumps() {
            self.dbase = l.into_iter().map(|(n, _)| n).collect();
        }
        self.dbup = nu;
    }

    fn dump_step(&mut self, w: &Wake, look: &mut dyn Look, end_now: bool) {
        let nu = w.up_ms / 1000;
        let Some(d) = self.dump.as_ref() else { return };
        let fin = end_now || nu >= d.until;
        if !fin && w.up_ms < d.sizes_at + INTERVAL_MS {
            return; // listed less than an interval ago (an extra wake): not a sample of its own
        }
        let listing = look.dumps().unwrap_or_default();
        let d = self.dump.as_mut().expect("open");
        let mut report = Vec::new();
        let mut sizes = BTreeMap::new();
        for (name, size) in listing {
            if self.dbase.contains(&name) || d.reported.contains(&name) {
                continue;
            }
            if fin || d.sizes.get(&name) == Some(&size) {
                report.push((name, size));
            } else {
                sizes.insert(name, size);
            }
        }
        d.sizes = sizes;
        d.sizes_at = w.up_ms;
        let ssr = d.ssr.clone();
        for (name, size) in report {
            let h = md5_8(&name);
            self.dump
                .as_mut()
                .expect("open")
                .reported
                .insert(name.clone());
            self.event(
                w.up_ms,
                "ssr_dump",
                format!("dump-{}-{h}", b8(&self.boot)),
                json!({"ssr": ssr, "file": clean(&name), "size": size}),
            );
        }
        if fin {
            self.dump = None;
            self.dump_base(nu, look);
        }
    }

    fn link_step(&mut self, w: &Wake, look: &mut dyn Look) {
        let nu = w.up_ms / 1000;
        let rt = look.route();
        if rt {
            self.rtseen = true;
        }
        if let Some(l) = self.link
            && l != rt
        {
            self.nl += 1;
            let res = if w.nl_ok && !self.catchup {
                0
            } else {
                nu.saturating_sub(self.linkup)
            };
            let id = format!("link-{}-{}", b8(&self.boot), self.nl);
            let cause = if self.ssr.is_some() { "ssr" } else { "other" };
            self.event(
                w.up_ms,
                "link",
                id,
                json!({"state": if rt { "up" } else { "down" }, "cause": cause, "res": res}),
            );
        }
        self.link = Some(rt);
        self.linkup = nu;
    }

    fn ppm_step(&mut self, nu: u64, look: &mut dyn Look) {
        if nu.saturating_sub(self.pkup) < PPM_S {
            return;
        }
        let (_, pk) = look.netdev();
        self.ppm = None;
        if let (Some(a), Some(b)) = (pk, self.pk)
            && a >= b
            && self.pkup > 0
        {
            self.ppm = Some((a - b) * 60 / (nu - self.pkup));
        }
        self.pk = pk;
        self.pkup = nu;
    }
}

/// First 8 hex digits of the MD5 of a name (the guard's dump ids: `md5sum | cut -c1-8`).
fn md5_8(s: &str) -> String {
    let d = md5(s.as_bytes());
    d[..4].iter().map(|b| format!("{b:02x}")).collect()
}

/// A small MD5 (RFC 1321) for the dump ids and the ledger's program md5s; no crate for 60 lines.
pub(crate) fn md5(input: &[u8]) -> [u8; 16] {
    let mut m = Md5::new();
    m.update(input);
    m.finish()
}

/// MD5 of a file read in 64 KB pieces (program binaries are tens of MB; u60d is resident).
pub(crate) fn md5_file(path: &std::path::Path) -> Option<[u8; 16]> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut m = Md5::new();
    let mut buf = vec![0u8; 65536];
    loop {
        match f.read(&mut buf) {
            Ok(0) => return Some(m.finish()),
            Ok(n) => m.update(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
}

pub(crate) struct Md5 {
    st: [u32; 4],
    buf: Vec<u8>,
    len: u64,
}

impl Md5 {
    pub(crate) fn new() -> Md5 {
        Md5 {
            st: [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476],
            buf: Vec::with_capacity(64),
            len: 0,
        }
    }

    pub(crate) fn update(&mut self, mut data: &[u8]) {
        self.len = self.len.wrapping_add(data.len() as u64);
        if !self.buf.is_empty() {
            let take = (64 - self.buf.len()).min(data.len());
            self.buf.extend_from_slice(&data[..take]);
            data = &data[take..];
            if self.buf.len() == 64 {
                let b = std::mem::take(&mut self.buf);
                self.block(&b);
            }
        }
        let (blocks, rest) = data.as_chunks::<64>();
        for c in blocks {
            self.block(c);
        }
        self.buf.extend_from_slice(rest);
    }

    pub(crate) fn finish(mut self) -> [u8; 16] {
        let bits = self.len.wrapping_mul(8);
        let mut tail = std::mem::take(&mut self.buf);
        tail.push(0x80);
        while tail.len() % 64 != 56 {
            tail.push(0);
        }
        tail.extend_from_slice(&bits.to_le_bytes());
        for c in tail.chunks(64) {
            self.block(c);
        }
        let mut out = [0u8; 16];
        for (i, v) in self.st.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        out
    }

    fn block(&mut self, chunk: &[u8]) {
        const S: [u32; 64] = [
            7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20,
            5, 9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23,
            6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
        ];
        let mut m = [0u32; 16];
        for (i, w) in chunk.chunks(4).enumerate() {
            m[i] = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
        }
        static K: std::sync::OnceLock<[u32; 64]> = std::sync::OnceLock::new();
        let k = K.get_or_init(|| {
            std::array::from_fn(|i| ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32)
        });
        let [mut a, mut b, mut c, mut d] = self.st;
        for (i, (&s, &ki)) in S.iter().zip(k.iter()).enumerate() {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f2 = f.wrapping_add(a).wrapping_add(ki).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f2.rotate_left(s));
        }
        for (s, v) in self.st.iter_mut().zip([a, b, c, d]) {
            *s = s.wrapping_add(v);
        }
    }
}

/// The kmsg burst cap (IO edge and replay share it): a kmsg wake less than [`INTERVAL_MS`] after
/// the last one masks kmsg out of `poll()` for one interval; when the mask runs out the records
/// are read on that timer wake, and if there were any it stays masked one more interval.
#[derive(Clone, Debug, Default)]
pub struct Cap {
    masked_until: Option<u64>,
    last_kmsg_wake: Option<u64>,
}

impl Cap {
    pub fn masked(&self, now_ms: u64) -> bool {
        self.masked_until.is_some_and(|m| now_ms < m)
    }
    pub fn deadline(&self) -> Option<u64> {
        self.masked_until
    }
    /// After a wake: whether kmsg woke it, and whether the read found records.
    pub fn after(&mut self, now_ms: u64, kmsg_woke: bool, got: bool) {
        if self.masked_until.is_some_and(|m| now_ms >= m) {
            // the mask ran out on this wake (kmsg is readable again at the same moment, so this
            // can look like a kmsg wake too): one read per interval while the burst lasts
            self.masked_until = got.then_some(now_ms + INTERVAL_MS);
            if got {
                self.last_kmsg_wake = Some(now_ms);
            }
        } else if kmsg_woke {
            if self
                .last_kmsg_wake
                .is_some_and(|t| now_ms < t + INTERVAL_MS)
            {
                self.masked_until = Some(now_ms + INTERVAL_MS);
            }
            self.last_kmsg_wake = Some(now_ms);
        }
    }
}

pub mod io;
pub mod kmsgfile;
pub mod replay;

#[cfg(test)]
mod tests;
