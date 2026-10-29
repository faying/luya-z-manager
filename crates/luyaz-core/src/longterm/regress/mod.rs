//! Tell me when it gets worse (design 2026-10-24 「變差才通知」; the implementation spec beside
//! it is the current word): incidents, kept in `long/incidents.json`, opened and closed by
//! detectors that only read what the core already reads (no new probes, no new wake-ups: the
//! long-term thread's minute check drives them).
//!
//! - **Interval incidents** (memory, cellular, exits, standby power, the sentinel's recorder and
//!   core-down): one alert when they open, one inbox-only alert when they close (`recovered`).
//! - **Point incidents** (a crash, the touch UI giving up, the sentinel silent, an unexpected
//!   reboot): the queue's alerts of one kind each within the window of the one before are one
//!   incident with a count; it ends `quiet` when the window passes without another.
//!
//! Every transition is an alert with a stable id `rg-<incident id>-<open|esc<n>|close>`
//! ([`luyaz_api::alerts::Alerts::add_once`]): the incident is saved with the transition pending
//! first, then queued, then the pending mark goes (a restart in between queues it again, once).
//! The queue is read back each check (`ingested_seq`): the alerts of an incident are kept with it
//! (`alert_seqs`), so the web can show them under the incident instead of twice.
//!
//! Sans-IO: [`Regress`] takes readings and gives transitions; [`io`] reads, queues and writes.

pub mod base;
pub mod cli;
pub mod detect;
pub mod io;
pub mod ship;
pub mod sync;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::Clock;

/// The file format; another number is renamed aside and started over (a rollback and back).
pub const VERSION: u32 = 1;
pub const FILE: &str = "incidents.json";
/// Closed incidents kept (design R13): 8 days, at most 200.
pub const CLOSED_KEEP_S: i64 = 8 * 86_400;
pub const CLOSED_MAX: usize = 200;
/// Alerts kept with one incident; past this only counted (eng review round 3, D9).
pub const ALERT_SEQS_MAX: usize = 50;
/// A point event's window (the next one within it is the same incident); a reboot's is longer.
pub const POINT_WINDOW_S: i64 = luyaz_api::alerts::POINT_WINDOW_S;
pub const REBOOT_WINDOW_S: i64 = 6 * 3600;
/// The touch screen keeps an ended incident this long (design review 14A, round 3 D11).
pub const SHOWN_CLOSED_S: i64 = 86_400;
/// The `regress` block: at most this many open and recently closed (design review 6A, 14A).
pub const BLOCK_OPEN: usize = 5;
pub const BLOCK_CLOSED: usize = 3;
/// `/v2/incidents`: closed incidents of the last 7 days (design review 6A).
pub const LIST_CLOSED_S: i64 = 7 * 86_400;

/// Kinds the sentinel opens and closes in the queue (the core only keeps them).
pub const SENTINEL_KINDS: &[&str] = &["regress-recorder", "regress-core-down", "regress-disk"];

/// A moment as every clock the device has can tell it: the boot (8 hex), CLOCK_BOOTTIME s, the
/// wall clock and whether it was trusted then.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct At {
    pub boot: String,
    pub up: i64,
    pub wall: i64,
    pub ok: bool,
}

impl At {
    pub fn of(c: &Clock) -> At {
        At {
            boot: super::boot8(&c.boot),
            up: c.boottime,
            wall: c.wall,
            ok: c.wall_ok,
        }
    }

    /// Seconds from `self` to `now`: by BOOTTIME in the same boot, else by trusted wall clocks;
    /// `None` when neither can tell.
    pub fn until(&self, now: &At) -> Option<i64> {
        if self.boot == now.boot {
            Some(now.up - self.up)
        } else if self.ok && now.ok {
            Some(now.wall - self.wall)
        } else {
            None
        }
    }

    /// The wall clock when it was trusted.
    pub fn wall_ok(&self) -> Option<i64> {
        self.ok.then_some(self.wall)
    }
}

/// What else was seen when an incident opened (CEO review X3r): words, not a cause.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Said {
    pub zh: String,
    pub en: String,
}

/// A ship that may be related (CEO review X1, eng review A1/O2).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Ship {
    pub comp: String,
    pub txn: String,
    /// Seconds from the ship to the incident's start; None when the clocks cannot tell.
    pub ago_s: Option<i64>,
}

/// A drill (a real-device test switch such as `/tmp/luyaz-test-rx-quiet`) that may be related,
/// by the same rules as a ship (design §4 「前 60 分鐘 ship/演練」).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Drill {
    /// The switch (`rx-quiet`).
    pub what: String,
    /// Seconds from the drill to the incident's start; None when the clocks cannot tell.
    pub ago_s: Option<i64>,
    /// `same_boot` or `uncertain`.
    pub clock: String,
}

/// A transition saved before it is queued (eng review R8).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Pending {
    pub id: String,
    pub kind: String,
    pub text: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Incident {
    pub id: String,
    pub kind: String,
    /// The exit (`chill`, `tailscale`) or the blind class (`cellular`, `egress`, `power`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub: Option<String>,
    pub from: At,
    #[serde(default)]
    pub to: Option<At>,
    /// `recovered` (a signal said it is over), `quiet` (a point incident's window passed),
    /// `reboot` (an interval incident cut by a reboot, which `regress-reboot` reports).
    #[serde(default)]
    pub end: Option<String>,
    /// Point incidents: how many; interval ones: 1.
    #[serde(default = "one")]
    pub count: u32,
    /// Point incidents: the last one.
    #[serde(default)]
    pub last: Option<At>,
    #[serde(default)]
    pub build: String,
    /// ASCII, the numbers behind it (also the alert's text after its id).
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub observed: Option<Said>,
    #[serde(default)]
    pub ship: Option<Ship>,
    /// `same_boot` or `uncertain` (eng review A1).
    #[serde(default)]
    pub ship_clock: Option<String>,
    #[serde(default)]
    pub alert_seqs: Vec<u64>,
    #[serde(default)]
    pub alert_more: u32,
    /// Escalations queued (a reboot row's 2nd, 4th, 8th).
    #[serde(default)]
    pub esc: u32,
    #[serde(default)]
    pub pending: Vec<Pending>,
    #[serde(default)]
    pub evidence: bool,
    /// The level that opened it (memory kB, power mW), for the recovery threshold.
    #[serde(default)]
    pub level: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drill: Option<Drill>,
    /// The 3-day 「accept the new level?」 went to the inbox (design §4).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub asked: bool,
}

fn one() -> u32 {
    1
}

impl Incident {
    pub fn new(id: String, kind: &str, from: At) -> Incident {
        Incident {
            id,
            kind: kind.into(),
            from,
            count: 1,
            ..Default::default()
        }
    }

    /// Keep an alert with this incident (once; past [`ALERT_SEQS_MAX`] only counted).
    pub fn keep_alert(&mut self, seq: u64) -> bool {
        if self.alert_seqs.contains(&seq) {
            return false;
        }
        if self.alert_seqs.len() < ALERT_SEQS_MAX {
            self.alert_seqs.push(seq);
        } else {
            self.alert_more += 1;
        }
        true
    }

    fn transition(&mut self, what: &str, text: String) {
        let id = format!("rg-{}-{what}", self.id);
        if !self.pending.iter().any(|p| p.id == id) {
            self.pending.push(Pending {
                id,
                kind: self.kind.clone(),
                text,
            });
        }
    }
}

/// Per class, while it cannot be judged (design §5 `regress-blind`).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Blind {
    /// The current run of "cannot tell" began here.
    #[serde(default)]
    pub since: Option<At>,
    /// The last blind incident opened (wall), for one in 7 days.
    #[serde(default)]
    pub said: Option<i64>,
}

/// `long/incidents.json`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct File {
    pub v: u32,
    /// The boot whose reboot was judged (or seeded on the first run).
    pub boot: String,
    /// The alert queue is read up to here (CEO review OV-C2).
    pub ingested_seq: u64,
    pub open: Vec<Incident>,
    pub closed: Vec<Incident>,
    /// The queue lost alerts before they were read (trimmed): when.
    #[serde(default)]
    pub gap_at: Option<At>,
    /// The build the previous process ran (a reboot alert names it).
    #[serde(default)]
    pub last_build: String,
    #[serde(default)]
    pub blind: std::collections::BTreeMap<String, Blind>,
    /// Drills seen (when each began), the last [`DRILLS_MAX`] within [`CLOSED_KEEP_S`].
    #[serde(default)]
    pub drills: Vec<(String, At)>,
}

/// Drills kept in the file.
pub const DRILLS_MAX: usize = 20;
/// A relative incident open this long asks whether its level is the new normal (design §4).
pub const ASK_AFTER_S: i64 = 3 * 86_400;

/// The owner's words for each kind (design review 9A, round 3 D12): the screen ≤ 6 Chinese
/// characters, English ≤ 18 characters; the long sentences stay `kind_label`'s.
pub fn name(kind: &str, sub: Option<&str>) -> (String, String) {
    let (zh, en) = match kind {
        "regress-mem" | "regress-mem-abs" => ("内存超标", "High memory"),
        "regress-reboot" => ("意外重启", "Unexpected reboot"),
        "regress-cellular" => ("蜂窝断线", "Cellular down"),
        "regress-egress" => ("出口不通", "Route down"),
        "regress-heat" => ("温度过高", "Too hot"),
        "regress-power" | "regress-power-abs" => ("耗电变高", "Higher power draw"),
        "regress-core-down" => ("数据服务停了", "Data service down"),
        "regress-recorder" => ("记录停了", "Recorder stalled"),
        "regress-disk" => ("存储快满", "Storage low"),
        "regress-blind" => {
            let (z, e) = match sub {
                Some("cellular") => ("蜂窝", "cellular"),
                Some("egress") => ("出口", "routes"),
                Some("power") => ("耗电", "power"),
                _ => ("某一类", "a check"),
            };
            return (format!("看不见：{z}"), format!("Can't watch {e}"));
        }
        "core-crash" => ("数据服务崩溃", "Data service crash"),
        "ui-crash" => ("触屏闪退", "Screen UI crash"),
        "web-crash" => ("管理网页崩溃", "Admin web crash"),
        "sentinel-crash" => ("哨兵崩溃", "Sentinel crash"),
        "ui-gave-up" => ("触屏换回原厂", "Stock UI fallback"),
        "wifi-restore-failed" => ("Wi-Fi没打开", "Wi-Fi won't start"),
        "sentinel-silent" => ("哨兵没心跳", "Sentinel silent"),
        _ => ("设备异常", "Device issue"),
    };
    (zh.into(), en.into())
}

/// The id part for a kind (`regress-mem-abs` → `mem-abs`).
pub fn short(kind: &str) -> &str {
    kind.strip_prefix("regress-").unwrap_or(kind)
}

/// The incident id for an interval incident opened at `at`: its kind, sub and when (the wall
/// clock when trusted, else the boot and its seconds).
pub fn interval_id(kind: &str, sub: Option<&str>, at: &At) -> String {
    let when = match at.wall_ok() {
        Some(w) => w.to_string(),
        None => format!("{}-{}", at.boot, at.up),
    };
    match sub {
        Some(s) => format!("{}-{s}-{when}", short(kind)),
        None => format!("{}-{when}", short(kind)),
    }
}

/// A transition's id split: (incident id, `open` / `esc<n>` / `close`).
pub fn split_tid(tid: &str) -> Option<(&str, &str)> {
    let r = tid.strip_prefix("rg-")?;
    let (iid, what) = r.rsplit_once('-')?;
    let ok = what == "open"
        || what == "close"
        || what == "ask"
        || what.strip_prefix("esc").is_some_and(|n| !n.is_empty());
    (ok && !iid.is_empty()).then_some((iid, what))
}

/// The state, sans IO.
#[derive(Debug, Default)]
pub struct Regress {
    pub file: File,
    pub det: detect::State,
    pub base: base::Base,
    pub build: String,
    /// The build the process before this one ran (a reboot alert names it).
    pub prev_build: String,
    /// Incidents opened since the IO side last looked (ship, evidence, memory snapshot).
    pub fresh: Vec<String>,
    /// Incidents ended since the IO side last looked (their evidence is written again).
    pub ended: Vec<String>,
    /// Something to save.
    pub dirty: bool,
    /// `[regress] …` lines for the core log.
    pub log: Vec<String>,
}

impl Regress {
    /// From the saved file (None: the first run, or a file renamed aside), at `now`.
    /// The first run seeds the queue's read point at `queue_head` and takes this boot as judged:
    /// nothing old becomes an incident, and installing does not report the boot it happens in
    /// (implementation spec §1: no news is good news).
    pub fn load(
        file: Option<File>,
        base: base::Base,
        build: &str,
        now: &At,
        queue_head: u64,
        boot: &str,
    ) -> Regress {
        let fresh = file.is_none();
        let mut f = file.unwrap_or_else(|| File {
            v: VERSION,
            boot: boot.into(),
            ingested_seq: queue_head,
            ..Default::default()
        });
        let mut r = Regress {
            build: build.into(),
            base,
            dirty: fresh,
            ..Default::default()
        };
        r.prev_build = f.last_build.clone();
        if f.last_build != build {
            f.last_build = build.into();
            r.dirty = true;
        }
        if f.boot != boot {
            // a reboot: interval incidents end here, cut (the reboot is reported by itself)
            let open = std::mem::take(&mut f.open);
            for mut i in open {
                if is_point(&i.kind) {
                    f.open.push(i);
                    continue;
                }
                i.to = Some(now.clone());
                i.end = Some("reboot".into());
                r.log
                    .push(format!("[regress] cut {} {} by a reboot", i.id, i.kind));
                r.ended.push(i.id.clone());
                f.closed.push(i);
            }
            r.dirty = true;
        }
        r.file = f;
        r
    }

    pub fn pending(&self) -> Vec<Pending> {
        self.file
            .open
            .iter()
            .chain(&self.file.closed)
            .flat_map(|i| i.pending.iter().cloned())
            .collect()
    }

    /// A transition was queued as `seq`.
    pub fn queued(&mut self, tid: &str, seq: u64) {
        for i in self.file.open.iter_mut().chain(self.file.closed.iter_mut()) {
            if let Some(p) = i.pending.iter().position(|p| p.id == tid) {
                i.pending.remove(p);
                i.keep_alert(seq);
                self.dirty = true;
            }
        }
    }

    pub fn find_open(&self, kind: &str, sub: Option<&str>) -> Option<usize> {
        self.file
            .open
            .iter()
            .position(|i| i.kind == kind && i.sub.as_deref() == sub)
    }

    /// Open an interval incident.
    pub fn open(
        &mut self,
        kind: &str,
        sub: Option<&str>,
        at: &At,
        detail: String,
        level: Option<f64>,
        observed: Option<Said>,
    ) -> usize {
        let mut i = Incident::new(interval_id(kind, sub, at), kind, at.clone());
        i.sub = sub.map(str::to_string);
        i.build = self.build.clone();
        i.detail = clip(&detail);
        i.level = level;
        i.observed = observed;
        i.transition("open", i.detail.clone());
        self.log
            .push(format!("[regress] open {} {} {}", i.id, i.kind, i.detail));
        self.fresh.push(i.id.clone());
        self.file.open.push(i);
        self.dirty = true;
        self.file.open.len() - 1
    }

    /// Close an open incident (`recovered`, `quiet`); `recovered` ones queue a close alert
    /// (inbox only).
    pub fn close(&mut self, idx: usize, at: &At, end: &str, detail: &str) {
        let mut i = self.file.open.remove(idx);
        i.to = Some(at.clone());
        i.end = Some(end.into());
        if end == "recovered" {
            i.transition("close", clip(detail));
        }
        self.log.push(format!(
            "[regress] close {} {} {end} {detail}",
            i.id, i.kind
        ));
        self.ended.push(i.id.clone());
        self.file.closed.push(i);
        self.dirty = true;
    }

    /// A drill began (the IO side saw its switch appear).
    pub fn drill(&mut self, what: &str, at: &At) {
        self.log.push(format!("[regress] drill {what} began"));
        self.file.drills.push((what.into(), at.clone()));
        self.file
            .drills
            .retain(|(_, d)| d.until(at).is_none_or(|s| s <= CLOSED_KEEP_S));
        let n = self.file.drills.len();
        if n > DRILLS_MAX {
            self.file.drills.drain(..n - DRILLS_MAX);
        }
        self.dirty = true;
    }

    /// The drill an incident starting at `at` may be related to: in the same boot up to
    /// [`ship::RELATED_S`] before; across a boot only early in this one ("time uncertain").
    pub fn drill_for(&self, at: &At) -> Option<Drill> {
        let same = self
            .file
            .drills
            .iter()
            .filter(|(_, d)| d.boot == at.boot && d.up <= at.up && at.up - d.up <= ship::RELATED_S)
            .max_by_key(|(_, d)| d.up);
        if let Some((w, d)) = same {
            return Some(Drill {
                what: w.clone(),
                ago_s: Some(at.up - d.up),
                clock: "same_boot".into(),
            });
        }
        let (w, d) = self.file.drills.last()?;
        if d.boot == at.boot || at.up > ship::RELATED_S {
            return None;
        }
        let ago = match (d.wall_ok(), at.wall_ok()) {
            (Some(a), Some(b)) => Some(b - a),
            _ => None,
        };
        if ago.is_some_and(|a| !(0..=ship::ACROSS_S).contains(&a)) {
            return None;
        }
        Some(Drill {
            what: w.clone(),
            ago_s: ago,
            clock: "uncertain".into(),
        })
    }

    /// Relative incidents open 3 days ask, once, whether their level is the new normal (design §4;
    /// inbox and MCP only: `rg-…-ask` never costs an SMS). The owner answers with
    /// `<core> regress accept <kind>`; the Agent never does by itself.
    pub fn ask(&mut self, now: &At) {
        let build = self.build.clone();
        for i in &mut self.file.open {
            if i.asked
                || !matches!(i.kind.as_str(), "regress-mem" | "regress-power")
                || i.from.until(now).is_none_or(|s| s < ASK_AFTER_S)
            {
                continue;
            }
            i.asked = true;
            let level = match (i.kind.as_str(), i.level) {
                ("regress-mem", Some(kb)) => format!(" at {:.0}MiB", kb / 1024.0),
                (_, Some(mw)) => format!(" at {mw:.0}mW"),
                _ => String::new(),
            };
            let builds = if i.build == build || i.build.is_empty() {
                format!("build {build}")
            } else {
                format!("build {}>{build}", i.build)
            };
            let text = clip(&format!(
                "open 3d; new normal? {builds}{level}; regress accept {}",
                i.kind
            ));
            i.transition("ask", text);
            self.log.push(format!("[regress] ask {} {}", i.id, i.kind));
            self.dirty = true;
        }
    }

    /// Ended incidents past the keep time go (design R13).
    pub fn prune(&mut self, now: &At) {
        let before = self.file.closed.len();
        self.file.closed.retain(|i| {
            !i.pending.is_empty()
                || i.to
                    .as_ref()
                    .and_then(|t| t.until(now))
                    .is_none_or(|s| s <= CLOSED_KEEP_S)
        });
        let n = self.file.closed.len();
        if n > CLOSED_MAX {
            self.file.closed.drain(..n - CLOSED_MAX);
        }
        if self.file.closed.len() != before {
            self.dirty = true;
        }
    }
}

impl File {
    /// The incident an alert belongs to, by its queue seq.
    pub fn incident_of(&self, seq: u64) -> Option<&Incident> {
        self.open
            .iter()
            .chain(&self.closed)
            .find(|i| i.alert_seqs.contains(&seq))
    }
}

/// A point event kind (the sender's and the core's reboot).
pub fn is_point(kind: &str) -> bool {
    luyaz_api::alerts::is_point(kind) || kind == "regress-reboot"
}

/// Details in an alert's text: printable ASCII (the queue keeps nothing else), at most 72.
pub fn clip(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii() && !c.is_ascii_control())
        .take(72)
        .collect()
}

// ── what readers get ──

fn at_json(a: &At) -> Value {
    json!(a.wall_ok())
}

/// One incident as `/v2/incidents` lists it (`unread` is the server's: it reads the queue).
pub fn listed(i: &Incident) -> Value {
    let (zh, en) = name(&i.kind, i.sub.as_deref());
    json!({
        "id": i.id,
        "kind": i.kind,
        "sub": i.sub,
        "name_zh": zh,
        "name_en": en,
        "from": at_json(&i.from),
        "to": i.to.as_ref().map(at_json),
        "closed": i.to.is_some(),
        "end": i.end,
        "count": i.count,
        "build": i.build,
        "detail": i.detail,
        "observed": i.observed,
        "ship": i.ship,
        "ship_clock": i.ship_clock,
        "drill": i.drill,
        "evidence": i.evidence,
        "alert_seqs": i.alert_seqs,
    })
}

/// `/v2/incidents` without `unread`: open ones (newest first), then those that ended in the last
/// 7 days (newest first).
pub fn list(r: &Regress, now: &At) -> Value {
    let mut open: Vec<&Incident> = r.file.open.iter().collect();
    open.sort_by_key(|i| std::cmp::Reverse(i.from.wall));
    let mut closed: Vec<&Incident> = r
        .file
        .closed
        .iter()
        .filter(|i| {
            i.to.as_ref()
                .and_then(|t| t.until(now))
                .is_some_and(|s| s <= LIST_CLOSED_S)
        })
        .collect();
    closed.sort_by(|a, b| {
        let w = |i: &Incident| i.to.as_ref().map_or(0, |t| t.wall);
        w(b).cmp(&w(a))
    });
    json!({
        "v": VERSION,
        "open": open.into_iter().map(listed).collect::<Vec<_>>(),
        "closed": closed.into_iter().map(listed).collect::<Vec<_>>(),
        "ingest_gap": r.file.gap_at.as_ref().and_then(|g| g.until(now)).is_some_and(|s| s <= LIST_CLOSED_S),
    })
}

/// Whether an ended incident is still shown on the touch screen (round 3 D11): in the same boot
/// by BOOTTIME, across boots only by trusted wall clocks that did not go back.
pub fn shown(to: &At, now: &At) -> bool {
    if to.boot == now.boot {
        return now.up - to.up <= SHOWN_CLOSED_S;
    }
    to.ok && now.ok && (0..=SHOWN_CLOSED_S).contains(&(now.wall - to.wall))
}

fn item(i: &Incident) -> Value {
    let (zh, en) = name(&i.kind, i.sub.as_deref());
    json!({
        "id": i.id,
        "kind": i.kind,
        "name_zh": zh,
        "name_en": en,
        "sub": i.sub.clone().unwrap_or_default(),
        "from": at_json(&i.from),
        "to": i.to.as_ref().and_then(At::wall_ok),
        "end": i.end.clone().unwrap_or_default(),
        "count": i.count,
        "observed_zh": i.observed.as_ref().map(|o| o.zh.clone()).unwrap_or_default(),
        "observed_en": i.observed.as_ref().map(|o| o.en.clone()).unwrap_or_default(),
    })
}

/// The `regress` block (design review 14A, round 3 D8/D11): open ones (≤ 5, newest first, and how
/// many in all), ended ones the screen still shows (≤ 3). Nothing in it moves with time alone,
/// so it is published only when an incident moves or leaves the 24 hours.
pub fn block(r: &Regress, now: &At) -> Value {
    let mut open: Vec<&Incident> = r.file.open.iter().collect();
    open.sort_by(|a, b| {
        b.from
            .wall
            .cmp(&a.from.wall)
            .then(b.from.up.cmp(&a.from.up))
    });
    let mut closed: Vec<&Incident> = r
        .file
        .closed
        .iter()
        .filter(|i| {
            i.end.as_deref() != Some("reboot") && i.to.as_ref().is_some_and(|t| shown(t, now))
        })
        .collect();
    closed.sort_by(|a, b| {
        let k = |i: &Incident| i.to.as_ref().map_or((0, 0), |t| (t.wall, t.up));
        k(b).cmp(&k(a))
    });
    json!({
        "open_total": open.len(),
        "open": open.iter().take(BLOCK_OPEN).map(|i| item(i)).collect::<Vec<_>>(),
        "closed": closed.iter().take(BLOCK_CLOSED).map(|i| item(i)).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests;
