//! CHILL home exit's background part.
//!
//! The 30 s tick (follow a hand-edited chill.env, end `rest` after 2 hours, turn media/rest off
//! after 10 minutes back in China, probe home every 5 minutes while a client is connected and end
//! `rest` after 3 misses) and the start-up reconcile. [`Machine::tick`], [`Machine::startup`] and
//! [`Machine::extend`] work over the [`Io`] trait, so they are tested with a fake.
//!
//! Notes:
//! - [`Machine`] has no `busy`/`ready`/`ticking` (the host's request bookkeeping: the host never
//!   ticks while a request runs) and its fields are public and serialisable, for the shadow
//!   records.
//! - `say` does not log.
//! - [`Replay`], as in `manual_first`, is an `Io` that answers from a recorded tick.
//! - The requests (`run_job`: precheck, apply, the 30 s self-check), the reason codes, chill.env
//!   and Tailscale parsing stay in the host.

pub const REST_SECS: u64 = 2 * 3600;
pub const PROBE_EVERY: u64 = 300;
pub const FAILS_TO_CLOSE: u32 = 3;
pub const UNKNOWN_EVERY: u64 = 1800;
pub const PIGGYBACK: u64 = 60;
pub const BACK_HOME_SECS: u64 = 600;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Items {
    pub media: bool,
    pub ip: bool,
    pub rest: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    Media,
    Ip,
    Rest,
}

impl Item {
    pub fn name(self) -> &'static str {
        match self {
            Item::Media => "media",
            Item::Ip => "ip",
            Item::Rest => "rest",
        }
    }
}

impl Items {
    pub fn get(&self, i: Item) -> bool {
        match i {
            Item::Media => self.media,
            Item::Ip => self.ip,
            Item::Rest => self.rest,
        }
    }
    pub fn with(mut self, i: Item, on: bool) -> Items {
        match i {
            Item::Media => self.media = on,
            Item::Ip => self.ip = on,
            Item::Rest => self.rest = on,
        }
        self
    }
    pub fn any(&self) -> bool {
        self.media || self.ip || self.rest
    }
}

/// A line for the screens, Chinese and English together (the host words each kind).
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Notice {
    pub kind: String,
    pub item: String,
    pub text: String,
    pub text_en: String,
    pub at: u64,
    #[cfg_attr(feature = "serde", serde(default))]
    pub code: String,
    #[cfg_attr(feature = "serde", serde(default))]
    pub detail: String,
}

/// `/data/chill/home-exit.json`, the file's shape.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Persist {
    pub items: Items,
    pub rest_until: u64,
    #[cfg_attr(feature = "serde", serde(default))]
    pub reopened_in_china: Items,
    #[cfg_attr(feature = "serde", serde(default))]
    pub notice: Notice,
}

/// What a tick sees.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Obs {
    pub now: u64,
    pub serving_mcc: Option<i64>,
    pub env: Items,
    pub clients: Option<i64>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub screen_on: Option<bool>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub cell_busy: Option<bool>,
}

/// What the self-check found broken (used by the host's requests; part of [`Io`]).
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum Fail {
    Core,
    Home,
    Tailscale,
    Direct,
}

/// Everything that talks to the outside (faked in tests, replayed in shadow runs).
pub trait Io {
    /// `None` = mihomo did not answer; `Some(None)` = home did not answer.
    fn probe_home(&mut self) -> Option<Option<u32>>;
    fn apply(&mut self, items: Items) -> Result<(), String>;
    fn check(&mut self) -> Result<(), Fail>;
    fn precheck(&mut self) -> Result<(), Fail>;
    fn alert(&mut self, kind: &str, text: &str);
    fn save(&mut self, p: &Persist);
    /// The cellular bearer has an address (asked only after home did not answer);
    /// `None` = unknown, which counts the miss as before.
    fn cell_up(&mut self) -> Option<bool> {
        None
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Machine {
    pub p: Persist,
    pub last_probe: u64,
    pub fails: u32,
    pub idle: bool,
    pub home_ms: Option<u32>,
    pub home_ok: Option<bool>,
    pub probed_at: u64,
    pub china_since: Option<u64>,
}

impl Machine {
    pub fn new(p: Persist) -> Self {
        Machine {
            p,
            ..Default::default()
        }
    }

    fn say(&mut self, now: u64, kind: &str, item: Option<Item>, text: String, text_en: String) {
        self.p.notice = Notice {
            kind: kind.into(),
            item: item.map(Item::name).unwrap_or("").into(),
            text,
            text_en,
            at: now,
            code: String::new(),
            detail: String::new(),
        };
    }

    fn close(&mut self, io: &mut dyn Io, items: Items) -> bool {
        match io.apply(items) {
            Ok(()) => {
                if !items.rest {
                    self.p.rest_until = 0;
                }
                self.p.items = items;
                true
            }
            Err(_) => false,
        }
    }

    /// What was on before the core (re)started.
    pub fn startup(&mut self, had_file: bool, env: Items, now: u64, io: &mut dyn Io) {
        let before = self.p.clone();
        let mut want = env;
        if !had_file && env.any() {
            want = Items::default();
            self.say(
                now,
                "stale",
                None,
                "家里出口找不到记录，已关".into(),
                "Home exit had no record; turned off".into(),
            );
        } else if env.rest && (self.p.rest_until == 0 || now >= self.p.rest_until) {
            want.rest = false;
            self.say(
                now,
                "stale",
                Some(Item::Rest),
                "已过期，已关".into(),
                "Expired; turned off".into(),
            );
        }
        if want != env {
            if !self.close(io, want) {
                self.p.items = env;
            }
        } else {
            self.p.items = env;
            if !env.rest {
                self.p.rest_until = 0;
            }
        }
        if self.p != before {
            io.save(&self.p);
        }
    }

    /// One 30 s tick (the host only ticks with no request running).
    pub fn tick(&mut self, o: &Obs, io: &mut dyn Io) {
        let before = self.p.clone();
        let now = o.now;

        if o.env != self.p.items {
            if o.env.rest && !self.p.items.rest {
                self.p.rest_until = now + REST_SECS;
            }
            if !o.env.rest {
                self.p.rest_until = 0;
            }
            self.p.items = o.env;
        }

        if self.p.items.rest
            && now >= self.p.rest_until
            && self.close(io, self.p.items.with(Item::Rest, false))
        {
            self.say(
                now,
                "expired",
                Some(Item::Rest),
                "2 小时到了，已关".into(),
                "2 hours up; turned off".into(),
            );
        }

        match o.serving_mcc {
            Some(460) => {
                let since = *self.china_since.get_or_insert(now);
                let mut want = self.p.items;
                if want.media && !self.p.reopened_in_china.media {
                    want.media = false;
                }
                if want.rest && !self.p.reopened_in_china.rest {
                    want.rest = false;
                }
                if now.saturating_sub(since) >= BACK_HOME_SECS
                    && want != self.p.items
                    && self.close(io, want)
                {
                    self.say(
                        now,
                        "back_home",
                        None,
                        "回国了，已关家里出口".into(),
                        "Back in China; home exit off".into(),
                    );
                }
            }
            Some(_) => {
                self.china_since = None;
                self.p.reopened_in_china = Items::default();
            }
            None => self.china_since = None,
        }

        // u60-radio.md §5 (D3, D8): with the screen known, screen off and no client (or clients
        // unknown) is nobody too, and screen on probes whoever is connected; unknown clients
        // still probe every UNKNOWN_EVERY so a lost reading can't stop it for good. A probe that
        // is due waits up to PIGGYBACK for the cell to be awake anyway. Screen unknown (the
        // agent): the rule above, unchanged.
        let nobody = match o.screen_on {
            None => o.clients == Some(0),
            Some(on) => !on && o.clients.is_none_or(|n| n == 0),
        };
        if self.p.items.any() && nobody {
            self.idle = true;
            if o.clients.is_some() {
                self.fails = 0;
            } else if now.saturating_sub(self.last_probe) >= UNKNOWN_EVERY {
                self.probe(now, io);
            }
        } else if self.p.items.any() {
            if std::mem::take(&mut self.idle) {
                self.last_probe = 0;
            }
            let since = now.saturating_sub(self.last_probe);
            let wait = o.cell_busy == Some(false)
                && self.last_probe > 0
                && since < PROBE_EVERY + PIGGYBACK;
            if since >= PROBE_EVERY && !wait {
                self.probe(now, io);
            }
        } else {
            self.fails = 0;
            self.home_ok = None;
            self.home_ms = None;
        }

        if self.p != before {
            io.save(&self.p);
        }
    }

    /// One probe of home; 3 misses in a row end `rest`.
    fn probe(&mut self, now: u64, io: &mut dyn Io) {
        self.last_probe = now;
        match io.probe_home() {
            Some(Some(ms)) => {
                self.fails = 0;
                self.home_ok = Some(true);
                self.home_ms = Some(ms);
                self.probed_at = now;
            }
            // The cell itself is down: says nothing about home; hold the count.
            Some(None) if io.cell_up() == Some(false) => {
                self.home_ok = Some(false);
                self.home_ms = None;
                self.probed_at = now;
            }
            Some(None) => {
                self.fails += 1;
                self.home_ok = Some(false);
                self.home_ms = None;
                self.probed_at = now;
                if self.fails >= FAILS_TO_CLOSE
                    && self.p.items.rest
                    && self.close(io, self.p.items.with(Item::Rest, false))
                {
                    self.say(
                        now,
                        "home_down",
                        Some(Item::Rest),
                        "家里 15 分钟没回应，已关".into(),
                        "Home silent 15 min; turned off".into(),
                    );
                    io.alert(
                        "home-exit-down",
                        "家里出口 15 分钟没回应，「其余都走家里」已关",
                    );
                }
            }
            None => {}
        }
    }

    /// Rest only, before it ends; another 2 hours from now. No reload.
    pub fn extend(&mut self, now: u64, io: &mut dyn Io) -> Result<u64, &'static str> {
        if !self.p.items.rest || now >= self.p.rest_until {
            return Err("not_on");
        }
        self.p.rest_until = now + REST_SECS;
        io.save(&self.p);
        Ok(self.p.rest_until)
    }
}

/* -------------------------------------------------------------- *
 *  Replay: one recorded tick or start-up through this copy
 * -------------------------------------------------------------- */

/// One `Io` call and what it answered (JSON tag `call`).
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "call", rename_all = "snake_case"))]
pub enum Event {
    /// `answered` false = mihomo silent; `ms` `None` = home silent. (Not `Option<Option<_>>`:
    /// serde writes both `None`s as null.)
    ProbeHome {
        answered: bool,
        ms: Option<u32>,
    },
    /// `err`: `None` = applied, `Some(reason)` = the reload failed.
    Apply {
        items: Items,
        err: Option<String>,
    },
    Check {
        fail: Option<Fail>,
    },
    Precheck {
        fail: Option<Fail>,
    },
    Alert {
        kind: String,
    },
    Save,
    CellUp {
        up: Option<bool>,
    },
}

impl Event {
    pub fn name(&self) -> &'static str {
        match self {
            Event::ProbeHome { .. } => "probe_home",
            Event::Apply { .. } => "apply",
            Event::Check { .. } => "check",
            Event::Precheck { .. } => "precheck",
            Event::Alert { .. } => "alert",
            Event::Save => "save",
            Event::CellUp { .. } => "cell_up",
        }
    }
}

/// The event for a probe answer.
pub fn probe_event(got: Option<Option<u32>>) -> Event {
    Event::ProbeHome {
        answered: got.is_some(),
        ms: got.flatten(),
    }
}

/// Answers each call from the recorded events in order; the first call that is not the next
/// recorded one (another kind, other items to apply, another alert) is a divergence, and from then
/// on every call gets the "could not" answer.
#[derive(Debug, Default)]
pub struct Replay {
    recorded: Vec<Event>,
    pos: usize,
    pub calls: Vec<Event>,
    pub diverged: Option<(usize, String)>,
}

impl Replay {
    pub fn new(recorded: Vec<Event>) -> Self {
        Replay {
            recorded,
            ..Default::default()
        }
    }

    pub fn unused(&self) -> usize {
        if self.diverged.is_some() {
            0
        } else {
            self.recorded.len() - self.pos
        }
    }

    fn next(&mut self, asked: &Event) -> Option<Event> {
        if self.diverged.is_some() {
            return None;
        }
        let ev = self.recorded.get(self.pos).cloned();
        let same = ev.as_ref().is_some_and(|e| match (e, asked) {
            (Event::Apply { items: a, .. }, Event::Apply { items: b, .. }) => a == b,
            (Event::Alert { kind: a }, Event::Alert { kind: b }) => a == b,
            (e, a) => e.name() == a.name(),
        });
        if !same {
            let what = match asked {
                Event::Apply { items, .. } => format!(
                    "apply(media={},ip={},rest={})",
                    items.media, items.ip, items.rest
                ),
                Event::Alert { kind } => format!("alert({kind})"),
                e => e.name().to_string(),
            };
            self.diverged = Some((self.pos, what));
            return None;
        }
        self.pos += 1;
        ev
    }
}

impl Io for Replay {
    fn probe_home(&mut self) -> Option<Option<u32>> {
        let got = match self.next(&Event::ProbeHome {
            answered: false,
            ms: None,
        }) {
            Some(Event::ProbeHome { answered, ms }) => answered.then_some(ms),
            _ => None,
        };
        self.calls.push(probe_event(got));
        got
    }
    fn apply(&mut self, items: Items) -> Result<(), String> {
        let err = match self.next(&Event::Apply { items, err: None }) {
            Some(Event::Apply { err, .. }) => err,
            _ => Some("not recorded".to_string()),
        };
        self.calls.push(Event::Apply {
            items,
            err: err.clone(),
        });
        err.map_or(Ok(()), Err)
    }
    fn check(&mut self) -> Result<(), Fail> {
        let fail = match self.next(&Event::Check { fail: None }) {
            Some(Event::Check { fail }) => fail,
            _ => Some(Fail::Core),
        };
        self.calls.push(Event::Check { fail });
        fail.map_or(Ok(()), Err)
    }
    fn precheck(&mut self) -> Result<(), Fail> {
        let fail = match self.next(&Event::Precheck { fail: None }) {
            Some(Event::Precheck { fail }) => fail,
            _ => Some(Fail::Core),
        };
        self.calls.push(Event::Precheck { fail });
        fail.map_or(Ok(()), Err)
    }
    fn alert(&mut self, kind: &str, _text: &str) {
        self.next(&Event::Alert { kind: kind.into() });
        self.calls.push(Event::Alert { kind: kind.into() });
    }
    fn save(&mut self, _p: &Persist) {
        self.next(&Event::Save);
        self.calls.push(Event::Save);
    }
    fn cell_up(&mut self) -> Option<bool> {
        let up = match self.next(&Event::CellUp { up: None }) {
            Some(Event::CellUp { up }) => up,
            _ => None,
        };
        self.calls.push(Event::CellUp { up });
        up
    }
}

/// What replaying one record gave.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub after: Machine,
    pub calls: Vec<Event>,
    pub diverged: Option<(usize, String)>,
    pub unused: usize,
}

/// What a record is: a tick, or the start-up reconcile.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "step", rename_all = "snake_case"))]
pub enum Step {
    Tick {
        obs: Obs,
    },
    Startup {
        had_file: bool,
        env: Items,
        now: u64,
    },
}

pub fn replay(before: &Machine, step: &Step, recorded: Vec<Event>) -> Outcome {
    let mut m = before.clone();
    let mut io = Replay::new(recorded);
    match step {
        Step::Tick { obs } => m.tick(obs, &mut io),
        Step::Startup { had_file, env, now } => m.startup(*had_file, *env, *now, &mut io),
    }
    Outcome {
        unused: io.unused(),
        after: m,
        calls: io.calls,
        diverged: io.diverged,
    }
}

#[cfg(test)]
mod tests;
