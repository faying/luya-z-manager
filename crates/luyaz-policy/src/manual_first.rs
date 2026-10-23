//! CHILL "manual first".
//!
//! When 🚀 节点选择 points at 🎯 手动节点 and the hand-picked node stops working while the
//! proxy is in use, switch 🚀 to that node's region group and say so; once the node has been
//! fine for 10 minutes, switch back. [`Machine`] calls [`Io`] for every outside
//! answer, so the decisions are tested with a fake.
//!
//! Notes:
//! - [`Machine`]'s fields are public (and serialisable with the `serde` feature): a shadow record
//!   carries the whole machine before and after a tick, since `manual`, the counts and
//!   `last_probe` live only in memory.
//! - `say` does not log (the host logs from the notice).
//! - [`Replay`] is an `Io` that answers from what a recorded tick got, in order, and notes
//!   where this copy asked something else. That is how u60d replays a tick (it must not probe
//!   nodes itself), and how a component runs without host imports: input = machine before +
//!   [`Obs`] + recorded answers, output = machine after + the calls made.
//! - The mihomo helpers (connection filters, provider map), the runtime thread and the status
//!   for the screens (`summary`, `main_to_remember`) are I/O: `crates/luyaz-manual-first`, which
//!   runs this `Machine`.

pub const MAIN_GROUP: &str = "🚀 节点选择";
pub const MANUAL_GROUP: &str = "🎯 手动节点";
pub const REGIONS: [&str; 4] = ["🇯🇵 日本", "🇹🇼 台湾", "🇸🇬 新加坡", "🇺🇸 美国"];

pub const PROBE_EVERY: u64 = 60;
pub const PROBE_BACK_EVERY: u64 = 120;
pub const SLOW_MS: u32 = 800;
pub const TIMEOUTS_TO_LEAVE: u32 = 2;
pub const BAD_TO_LEAVE: u32 = 3;
pub const GOOD_TO_RETURN: u64 = 600;
pub const MAX_SWITCHES_PER_HOUR: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
pub enum Probe {
    Ms(u32),
    Timeout,
}

/// What the loop sees on each tick (cheap local reads only).
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Obs {
    pub now: u64,
    /// CHILL running and mihomo in rule mode.
    pub active: bool,
    pub main_now: String,
    pub manual_now: String,
}

/// Everything that talks to the outside. `None` from a probe = could not ask
/// (mihomo not answering): skip, never count it against the node.
pub trait Io {
    fn in_use(&mut self) -> bool;
    fn probe(&mut self, node: &str) -> Option<Probe>;
    fn cell_alive(&mut self) -> bool;
    fn direct_ms(&mut self) -> Option<u32>;
    /// Region group whose members include `node`.
    fn region_of(&mut self, node: &str) -> Option<String>;
    /// Re-test `group` now; return what it picked and whether that works.
    fn retest(&mut self, group: &str) -> Option<(String, bool)>;
    fn select_main(&mut self, member: &str) -> bool;
    /// Close connections still going through the dead node via 🎯.
    fn drop_manual_conns(&mut self);
    fn save(&mut self, p: &Persist);
}

/// Survives a core restart (written only on switch / return / clear). The file is
/// `/data/chill/manual-first.json`.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Persist {
    /// Non-empty = we moved 🚀 off 🎯 onto this group.
    pub backup: String,
    pub manual: String,
    pub switched_at: u64,
    /// Times we switched away, for the 2-per-hour cap.
    pub switches: Vec<u64>,
    pub last_region: String,
    pub notice: String,
    /// English twin of `notice`. Absent in files written before L2.
    #[cfg_attr(feature = "serde", serde(default))]
    pub notice_en: String,
    pub notice_at: u64,
    /// "backup", "back", "alldown", "repick", "unstable". Absent in files written before it.
    #[cfg_attr(feature = "serde", serde(default))]
    pub notice_kind: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Machine {
    pub p: Persist,
    pub manual: String,
    pub timeouts: u32,
    pub bad: u32,
    pub good_since: Option<u64>,
    pub last_probe: u64,
}

impl Machine {
    pub fn new(p: Persist) -> Self {
        Machine {
            manual: p.manual.clone(),
            p,
            ..Default::default()
        }
    }

    /// Whether a tick showing 🚀 on `main_now` can do more than reset the counts, so the loop
    /// has to ask mihomo for the mode and 🎯's pick too (`/configs` is ~170 KB on the device).
    pub fn needs_detail(&self, main_now: &str) -> bool {
        main_now == MANUAL_GROUP
            || !self.p.backup.is_empty()
            || (REGIONS.contains(&main_now) && main_now != self.p.last_region)
    }

    fn reset_counts(&mut self) {
        self.timeouts = 0;
        self.bad = 0;
        self.good_since = None;
    }

    /// Set the notice, Chinese and English together. Node and group names
    /// are proper nouns and go into both as they are.
    fn say(&mut self, now: u64, kind: &str, text: String, text_en: String) {
        self.p.notice = text;
        self.p.notice_en = text_en;
        self.p.notice_at = now;
        self.p.notice_kind = kind.to_string();
    }

    fn clear_backup(&mut self, io: &mut dyn Io) {
        self.p.backup.clear();
        self.reset_counts();
        io.save(&self.p);
    }

    pub fn tick(&mut self, o: &Obs, io: &mut dyn Io) {
        if !o.active || o.main_now.is_empty() {
            return;
        }
        if REGIONS.contains(&o.main_now.as_str())
            && o.main_now != self.p.backup
            && o.main_now != self.p.last_region
        {
            self.p.last_region = o.main_now.clone();
        }
        if !self.p.backup.is_empty() {
            self.tick_on_backup(o, io);
        } else {
            self.tick_on_manual(o, io);
        }
    }

    fn tick_on_backup(&mut self, o: &Obs, io: &mut dyn Io) {
        if o.main_now != self.p.backup {
            // The owner (or the exit switch, or abroad mode) chose something else: hands off.
            self.p.notice.clear();
            self.p.notice_en.clear();
            self.p.notice_kind.clear();
            self.clear_backup(io);
            return;
        }
        if !o.manual_now.is_empty() && o.manual_now != self.p.manual {
            // A new hand pick while on the backup: go straight back to 🎯.
            if io.select_main(MANUAL_GROUP) {
                self.say(
                    o.now,
                    "repick",
                    format!("按你新选的 {} 换回手动节点", o.manual_now),
                    format!("Back to the manual node: {}, as you picked", o.manual_now),
                );
                self.p.manual = o.manual_now.clone();
                self.manual = o.manual_now.clone();
                self.clear_backup(io);
            }
            return;
        }
        if o.now.saturating_sub(self.last_probe) < PROBE_BACK_EVERY || !io.in_use() {
            return;
        }
        self.last_probe = o.now;
        if !io.cell_alive() {
            return;
        }
        let manual = self.p.manual.clone();
        match io.probe(&manual) {
            Some(Probe::Ms(ms)) if ms <= SLOW_MS => {
                let since = *self.good_since.get_or_insert(o.now);
                if o.now - since >= GOOD_TO_RETURN && io.select_main(MANUAL_GROUP) {
                    self.say(
                        o.now,
                        "back",
                        format!("{manual} 恢复，已换回"),
                        format!("{manual} is back; switched back to it"),
                    );
                    self.clear_backup(io);
                }
            }
            Some(_) => self.good_since = None,
            None => {}
        }
    }

    fn tick_on_manual(&mut self, o: &Obs, io: &mut dyn Io) {
        if o.main_now != MANUAL_GROUP || o.manual_now.is_empty() {
            self.reset_counts();
            return;
        }
        if o.manual_now != self.manual {
            self.manual = o.manual_now.clone();
            self.reset_counts();
        }
        if o.now.saturating_sub(self.last_probe) < PROBE_EVERY || !io.in_use() {
            return;
        }
        self.last_probe = o.now;
        let manual = self.manual.clone();
        match io.probe(&manual) {
            None => return,
            Some(Probe::Ms(ms)) if ms <= SLOW_MS => {
                self.reset_counts();
                return;
            }
            Some(Probe::Timeout) => {
                self.timeouts += 1;
                self.bad += 1;
            }
            Some(Probe::Ms(_)) => {
                self.timeouts = 0;
                self.bad += 1;
            }
        }
        if self.timeouts < TIMEOUTS_TO_LEAVE && self.bad < BAD_TO_LEAVE {
            return;
        }
        // From here on this round decides; start counting afresh either way.
        self.reset_counts();
        if !io.cell_alive() {
            return; // cellular down: of course the node fails
        }
        if io.direct_ms().is_none_or(|ms| ms > SLOW_MS) {
            return; // direct is slow too: signal/cell problem, not the node
        }
        self.p.switches.retain(|t| o.now.saturating_sub(*t) < 3600);
        if self.p.switches.len() >= MAX_SWITCHES_PER_HOUR {
            let n = self.p.switches.len();
            self.say(
                o.now,
                "unstable",
                format!("手选的 {manual} 不稳，1 小时内已换走 {n} 次，这次不换"),
                format!(
                    "{manual} is unstable; already switched away {n} times this hour, staying put"
                ),
            );
            return;
        }
        let backup = io
            .region_of(&manual)
            .or_else(|| (!self.p.last_region.is_empty()).then(|| self.p.last_region.clone()))
            .unwrap_or_else(|| REGIONS[0].to_string());
        match io.retest(&backup) {
            Some((picked, true)) if picked != manual => {}
            _ => {
                self.say(
                    o.now,
                    "alldown",
                    format!("手选的 {manual} 和同地区备用 {backup} 都不通"),
                    format!("{manual} and its backup {backup} are both down"),
                );
                return;
            }
        }
        if !io.select_main(&backup) {
            return;
        }
        io.drop_manual_conns();
        self.p.backup = backup.clone();
        self.p.manual = manual.clone();
        self.p.switched_at = o.now;
        self.p.switches.push(o.now);
        self.say(
            o.now,
            "backup",
            format!("手选的 {manual} 不通，已临时换到 {backup}（自动）"),
            format!("{manual} is down; using {backup} for now (auto)"),
        );
        io.save(&self.p);
    }
}

/* -------------------------------------------------------------- *
 *  Replay: one recorded tick through this copy
 * -------------------------------------------------------------- */

/// One `Io` call and what it answered. The recorder keeps these in order (JSON tag `call`);
/// [`Replay`] answers from them.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "call", rename_all = "snake_case"))]
pub enum Event {
    InUse {
        got: bool,
    },
    Probe {
        node: String,
        got: Option<Probe>,
    },
    CellAlive {
        got: bool,
    },
    DirectMs {
        got: Option<u32>,
    },
    RegionOf {
        node: String,
        got: Option<String>,
    },
    Retest {
        group: String,
        got: Option<(String, bool)>,
    },
    SelectMain {
        member: String,
        ok: bool,
    },
    DropManualConns,
    Save,
}

impl Event {
    pub fn name(&self) -> &'static str {
        match self {
            Event::InUse { .. } => "in_use",
            Event::Probe { .. } => "probe",
            Event::CellAlive { .. } => "cell_alive",
            Event::DirectMs { .. } => "direct_ms",
            Event::RegionOf { .. } => "region_of",
            Event::Retest { .. } => "retest",
            Event::SelectMain { .. } => "select_main",
            Event::DropManualConns => "drop_manual_conns",
            Event::Save => "save",
        }
    }
}

/// Answers each call from the recorded events, in order. A call that is not the next recorded
/// one (another kind, another argument, or past the end) is a divergence: the first is kept, and
/// from then on every call gets the "could not ask" answer, so the rest of the tick stays inert.
/// `calls` is what this copy actually asked, with the answers it got, for the report.
#[derive(Debug, Default)]
pub struct Replay {
    recorded: Vec<Event>,
    pos: usize,
    pub calls: Vec<Event>,
    /// (index, what this copy asked) at the first divergence.
    pub diverged: Option<(usize, String)>,
}

impl Replay {
    pub fn new(recorded: Vec<Event>) -> Self {
        Replay {
            recorded,
            ..Default::default()
        }
    }

    /// Recorded calls this copy never made (the recorded tick asked more).
    pub fn unused(&self) -> usize {
        if self.diverged.is_some() {
            0
        } else {
            self.recorded.len() - self.pos
        }
    }

    /// Next recorded event if it is this call with this argument.
    fn next(&mut self, name: &'static str, arg: &str) -> Option<Event> {
        if self.diverged.is_some() {
            return None;
        }
        let ev = self.recorded.get(self.pos).cloned();
        let same = ev.as_ref().is_some_and(|e| {
            e.name() == name
                && match e {
                    Event::Probe { node, .. } | Event::RegionOf { node, .. } => node == arg,
                    Event::Retest { group, .. } => group == arg,
                    Event::SelectMain { member, .. } => member == arg,
                    _ => true,
                }
        });
        if !same {
            let asked = if arg.is_empty() {
                name.to_string()
            } else {
                format!("{name}({arg})")
            };
            self.diverged = Some((self.pos, asked));
            return None;
        }
        self.pos += 1;
        ev
    }
}

impl Io for Replay {
    fn in_use(&mut self) -> bool {
        let got = matches!(self.next("in_use", ""), Some(Event::InUse { got: true }));
        self.calls.push(Event::InUse { got });
        got
    }
    fn probe(&mut self, node: &str) -> Option<Probe> {
        let got = match self.next("probe", node) {
            Some(Event::Probe { got, .. }) => got,
            _ => None,
        };
        self.calls.push(Event::Probe {
            node: node.to_string(),
            got,
        });
        got
    }
    fn cell_alive(&mut self) -> bool {
        let got = matches!(
            self.next("cell_alive", ""),
            Some(Event::CellAlive { got: true })
        );
        self.calls.push(Event::CellAlive { got });
        got
    }
    fn direct_ms(&mut self) -> Option<u32> {
        let got = match self.next("direct_ms", "") {
            Some(Event::DirectMs { got }) => got,
            _ => None,
        };
        self.calls.push(Event::DirectMs { got });
        got
    }
    fn region_of(&mut self, node: &str) -> Option<String> {
        let got = match self.next("region_of", node) {
            Some(Event::RegionOf { got, .. }) => got,
            _ => None,
        };
        self.calls.push(Event::RegionOf {
            node: node.to_string(),
            got: got.clone(),
        });
        got
    }
    fn retest(&mut self, group: &str) -> Option<(String, bool)> {
        let got = match self.next("retest", group) {
            Some(Event::Retest { got, .. }) => got,
            _ => None,
        };
        self.calls.push(Event::Retest {
            group: group.to_string(),
            got: got.clone(),
        });
        got
    }
    fn select_main(&mut self, member: &str) -> bool {
        let ok = matches!(
            self.next("select_main", member),
            Some(Event::SelectMain { ok: true, .. })
        );
        self.calls.push(Event::SelectMain {
            member: member.to_string(),
            ok,
        });
        ok
    }
    fn drop_manual_conns(&mut self) {
        self.next("drop_manual_conns", "");
        self.calls.push(Event::DropManualConns);
    }
    fn save(&mut self, _p: &Persist) {
        self.next("save", "");
        self.calls.push(Event::Save);
    }
}

/// What replaying one recorded tick gave.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub after: Machine,
    pub calls: Vec<Event>,
    pub diverged: Option<(usize, String)>,
    pub unused: usize,
}

/// Run one recorded tick: `before` (the recorded machine before it), the tick's [`Obs`] and the
/// answers it got. The host compares `after` with the recorded machine after the tick.
pub fn replay(before: &Machine, obs: &Obs, recorded: Vec<Event>) -> Outcome {
    let mut m = before.clone();
    let mut io = Replay::new(recorded);
    m.tick(obs, &mut io);
    Outcome {
        unused: io.unused(),
        after: m,
        calls: io.calls,
        diverged: io.diverged,
    }
}

#[cfg(test)]
mod tests;
