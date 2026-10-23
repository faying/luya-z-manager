//! CHILL home exit, the engine run by the core. Every side effect outside mihomo goes through
//! [`Host`].
//!
//! CHILL home exit (`docs/designs/home-exit.md`, E3): send part of the traffic
//! out through the home network — a SOCKS5 at home (`CHILL_HOME_EXIT`),
//! reached over the Tailscale subnet route.
//!
//! Three switches, each a `CHILL_HOME_*` key in chill.env that chill.sh turns
//! into template lines (§11):
//!   media  CN media (iQIYI, Youku, bilibili …) via home
//!   ip     the owner's `home_ip.list` sites via home
//!   rest   everything else via home, 2 hours at a time
//!
//! All the state lives here (D1): switching is "write env + `chill.sh reload`"
//! (D3), with the env put back when the reload fails (Q1); after turning one
//! on, a 30 s self-check (D6) turns it back off if anything that should not
//! have changed did. A 30 s tick ends `rest` after 2 hours or 3 failed home
//! probes 5 minutes apart (§4.5), and turns `media`/`rest` off after 10
//! minutes back in China (D7, unless re-opened there by hand). With no LAN
//! client nothing can use the exit, so the probe pauses and runs again as soon
//! as one connects (u60-ledger.md §16: each probe kept the radio busy 1–2 min). On start the
//! saved state is reconciled with chill.env (A2).
//!
//! The decision logic is [`Machine`] and [`run_job`], with all I/O behind
//! [`Io`] and time passed in, so the flows are unit-tested without a device.

// (de)serialised state and records
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const STATE_FILE: &str = "/data/chill/home-exit.json";
const HOME_IP_LIST: &str = "/data/chill/home_ip.list";
const MODE_FILE: &str = "/data/chill/mode";
const DEFAULT_API: &str = "http://127.0.0.1:9999";
/// Must match chill.sh's `HOME_PROBE_DEFAULT` (D5): a China-side 204, so the
/// probe measures "home answers", not home plus its own overseas route.
const DEFAULT_PROBE_URL: &str = "http://connect.rom.miui.com/generate_204";

pub const HOME_PROXY: &str = "家里";
const FISH_GROUP: &str = "🐟 漏网之鱼";

const TICK: Duration = Duration::from_secs(30);
pub const REST_SECS: u64 = 2 * 3600;
const PROBE_EVERY: u64 = 300;
const FAILS_TO_CLOSE: u32 = 3;
/// Screen off, clients unknown: probe at most this often (u60-radio.md D3).
const UNKNOWN_EVERY: u64 = 1800;
/// A due probe waits up to this long for the cell to be awake anyway (D8).
const PIGGYBACK: u64 = 60;
const BACK_HOME_SECS: u64 = 600;
const TRIAL_SECS: u64 = 30;
const TRIAL_STEP: u64 = 5;
const TS_CACHE_SECS: u64 = 60;

/* -------------------------------------------------------------- *
 *  State
 * -------------------------------------------------------------- */

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    pub fn parse(s: &str) -> Option<Item> {
        match s {
            "media" => Some(Item::Media),
            "ip" => Some(Item::Ip),
            "rest" => Some(Item::Rest),
            _ => None,
        }
    }
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

/// A line for the screens (DD6), Chinese and English together. Kinds:
/// "opened" ● on (screens show it 3 s), "failed" ■ did not open, "expired"
/// 2 hours up, "home_down" home silent 15 min, "back_home" back in China,
/// "stale" found on after an agent restart and past its time.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Notice {
    pub kind: String,
    pub item: String,
    pub text: String,
    pub text_en: String,
    pub at: u64,
    /// For "failed": `core`, `home_no_answer`, `tailscale_down`, `direct_down`.
    #[serde(default)]
    pub code: String,
    /// For "failed": what the reload said (mihomo -t's first error), if anything.
    #[serde(default)]
    pub detail: String,
}

/// Survives an agent restart (`/data/chill/home-exit.json`); written only
/// when it changes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Persist {
    pub items: Items,
    /// When `rest` ends (unix seconds); 0 when off.
    pub rest_until: u64,
    /// Turned on by hand while in China: D7 leaves these alone (R3).
    #[serde(default)]
    pub reopened_in_china: Items,
    #[serde(default)]
    pub notice: Notice,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Busy {
    pub item: &'static str,
    pub on: bool,
    /// "checking" (testing home first, §4.4), "applying" (writing env,
    /// reloading) or "trial" (D6 self-check).
    pub phase: &'static str,
    /// End of the trial countdown; 0 while applying.
    pub until: u64,
}

/// What a tick sees (cheap local reads only).
#[derive(Clone, Debug, Default)]
pub struct Obs {
    pub now: u64,
    /// The serving network's MCC from core (`/v2` signal, D2); `None` =
    /// unknown, which never counts as "in China".
    pub serving_mcc: Option<i64>,
    /// What chill.env says now.
    pub env: Items,
    /// LAN clients (Wi-Fi + wired) from the core's `clients` block; `None` =
    /// unknown, which probes as before.
    pub clients: Option<i64>,
    /// The screen's backlight; `None` = unknown, the old rule.
    pub screen_on: Option<bool>,
    /// The cell moved packets in its last ≥ 30 s window (u60d's `cell_window`); `None` =
    /// unknown, no waiting.
    pub cell_busy: Option<bool>,
}

/// What the self-check found broken (D6), worded as DD6's "没开成：<原因>".
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fail {
    /// mihomo did not answer /version, or the reload failed.
    Core,
    Home,
    Tailscale,
    Direct,
}

impl Fail {
    fn code(self) -> &'static str {
        match self {
            Fail::Core => "core",
            Fail::Home => "home_no_answer",
            Fail::Tailscale => "tailscale_down",
            Fail::Direct => "direct_down",
        }
    }
    fn zh(self) -> &'static str {
        match self {
            Fail::Core => "代理没重启成功",
            Fail::Home => "家里 Mac 没回应",
            Fail::Tailscale => "Tailscale 没连上家里",
            Fail::Direct => "直连测试不通",
        }
    }
    fn en(self) -> &'static str {
        match self {
            Fail::Core => "proxy didn't restart",
            Fail::Home => "home Mac silent",
            Fail::Tailscale => "no Tailscale to home",
            Fail::Direct => "direct test failed",
        }
    }
}

/// Everything that talks to the outside.
pub trait Io {
    /// One probe through 家里. `None` = mihomo did not answer (skip, never
    /// held against home); `Some(None)` = home did not answer.
    fn probe_home(&mut self) -> Option<Option<u32>>;
    /// Write the three switches to chill.env and reload CHILL; on a failed
    /// reload put the old values back (Q1) and return the reason.
    fn apply(&mut self, items: Items) -> Result<(), String>;
    /// D6's four checks; the first that fails.
    fn check(&mut self) -> Result<(), Fail>;
    /// §4.4: before turning anything on, can home be reached at all? The
    /// reason names Tailscale when it knows the router is gone.
    fn precheck(&mut self) -> Result<(), Fail>;
    fn alert(&mut self, kind: &str, text: &str);
    fn save(&mut self, p: &Persist);
    /// The cellular bearer has an address (asked only after home did not answer);
    /// `None` = unknown, which counts the miss as before.
    fn cell_up(&mut self) -> Option<bool> {
        None
    }
}

pub trait Clock {
    fn now(&self) -> u64;
    fn sleep(&self, secs: u64);
}

#[derive(Clone, Default)]
pub struct Machine {
    pub p: Persist,
    pub busy: Option<Busy>,
    last_probe: u64,
    fails: u32,
    /// Probing paused because no client was connected (§16).
    idle: bool,
    /// Last probe: `Some(ms)` answered, `None` not yet / did not.
    pub home_ms: Option<u32>,
    /// `Some(false)` after a failed probe: the groups are on their fallback.
    pub home_ok: Option<bool>,
    pub probed_at: u64,
    china_since: Option<u64>,
    /// Set once [`Machine::startup`] ran (until then GET says "updating").
    pub ready: bool,
    /// A tick is running on a copy (it may reload CHILL for seconds); new
    /// switches wait for it.
    pub ticking: bool,
}

impl Machine {
    pub fn new(p: Persist) -> Self {
        Machine { p, ..Default::default() }
    }

    /// A new SIM is online (core's SIM identity settled on a new epoch): misses counted on the
    /// old card say nothing about home now, and the next tick probes at once.
    pub fn sim_changed(&mut self) {
        self.fails = 0;
        self.last_probe = 0;
        self.home_ok = None;
        self.home_ms = None;
    }

    fn clone_for_tick(&self) -> Machine {
        self.clone()
    }

    fn say(&mut self, now: u64, kind: &str, item: Option<Item>, text: String, text_en: String) {
        eprintln!("[home_exit] {text}");
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

    /// Turn some items off from the tick; `false` when the reload failed
    /// (tried again next tick).
    fn close(&mut self, io: &mut dyn Io, items: Items) -> bool {
        match io.apply(items) {
            Ok(()) => {
                if !items.rest {
                    self.p.rest_until = 0;
                }
                self.p.items = items;
                true
            }
            Err(e) => {
                eprintln!("[home_exit] could not switch off: {e}");
                false
            }
        }
    }

    /// A2: what was on before the core (re)started. No saved state but env
    /// switches on = nobody knows when they end: off. `rest` past its time:
    /// off. Otherwise adopt the env.
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
            self.say(now, "stale", Some(Item::Rest), "已过期，已关".into(), "Expired; turned off".into());
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
        self.ready = true;
        if self.p != before {
            io.save(&self.p);
        }
    }

    pub fn tick(&mut self, o: &Obs, io: &mut dyn Io) {
        if self.busy.is_some() {
            return;
        }
        let before = self.p.clone();
        let now = o.now;

        // chill.env is what is rendered: if it changed under us (by hand),
        // follow it.
        if o.env != self.p.items {
            if o.env.rest && !self.p.items.rest {
                self.p.rest_until = now + REST_SECS;
            }
            if !o.env.rest {
                self.p.rest_until = 0;
            }
            self.p.items = o.env;
        }

        // §4.5: rest ends after 2 hours.
        if self.p.items.rest && now >= self.p.rest_until && self.close(io, self.p.items.with(Item::Rest, false)) {
            self.say(now, "expired", Some(Item::Rest), "2 小时到了，已关".into(), "2 hours up; turned off".into());
        }

        // D7: 10 minutes on a Chinese network turns media and rest off,
        // unless they were opened by hand while in China. Unknown never counts.
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
                if now.saturating_sub(since) >= BACK_HOME_SECS && want != self.p.items && self.close(io, want) {
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

        // §4.5: probe home every 5 minutes while anything is on; 3 misses in a
        // row end `rest` (media and ip sit on their fallback meanwhile).
        // No client: nothing can use home, so don't probe (each probe wakes the
        // tailnet and keeps the radio up); the first client gets a probe at once.
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
            let wait = o.cell_busy == Some(false) && self.last_probe > 0 && since < PROBE_EVERY + PIGGYBACK;
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
            // The cell itself is down: nothing reaches home, so this says nothing about
            // home. Hold the count; the groups sit on their fallback meanwhile.
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
                    io.alert("home-exit-down", "家里出口 15 分钟没回应，「其余都走家里」已关");
                }
            }
            None => {}
        }
    }

    /// DD8: rest only, before it ends; another 2 hours from now. No reload.
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
 *  Host: everything outside mihomo that the engine reads or changes
 * -------------------------------------------------------------- */

/// What the engine needs from the program it runs in (the core's host reads its own blocks). Paths are the device's; a test host
/// maps them into a temporary directory.
pub trait Host: Send + Sync {
    /// One `KEY` from chill.env (`''` when absent).
    fn env_var(&self, name: &str) -> String;
    /// Several `KEY='…'` lines in one atomic rewrite of chill.env.
    fn set_env_vars(&self, vars: &[(&str, &str)]) -> Result<(), String>;
    /// `/tmp/chill.state`, parsed.
    fn chill_state(&self) -> Option<Value>;
    /// `chill.sh reload`.
    fn chill_reload(&self) -> Result<(), String>;
    /// `tailscale status --json`; `None` when not installed or not answering.
    fn tailscale_json(&self) -> Option<Value>;
    fn alert(&self, kind: &str, text: &str);
    /// the core's D2 fields: (serving_mcc, true_roaming), `None` each when unknown.
    fn where_now(&self) -> (Option<i64>, Option<bool>);
    /// LAN clients (Wi-Fi + wired); `None` when unknown.
    fn clients_now(&self) -> Option<i64>;
    /// The screen's backlight is on; `None` when unknown (the old probe rule).
    fn screen_on(&self) -> Option<bool> {
        None
    }
    /// The cell moved packets in its last ≥ 30 s window; `None` when unknown (no waiting).
    fn cell_busy(&self) -> Option<bool> {
        None
    }
    /// The cellular bearer has an address; `None` when unknown (misses count as before).
    fn cell_up(&self) -> Option<bool> {
        None
    }
    /// Read a device file (`STATE_FILE`, `HOME_IP_LIST`, `MODE_FILE`).
    fn read(&self, path: &str) -> Option<String>;
    /// Replace a device file atomically.
    fn write(&self, path: &str, data: &[u8]) -> std::io::Result<()>;
    /// mihomo's controller, `http://127.0.0.1:9999` on the device.
    fn mihomo_api(&self) -> String {
        DEFAULT_API.to_string()
    }
    /// Whether this program owns the home exit now. One machine, one loop, one writer of
    /// chill.env's `CHILL_HOME_*`; while not, the loop waits and requests are refused. (u60d's host
    /// always owns it now.)
    fn owner(&self) -> bool {
        true
    }
}

impl dyn Host {
    fn running(&self) -> bool {
        self.chill_state().and_then(|v| v.get("state").and_then(Value::as_str).map(|s| s == "running")) == Some(true)
    }
}
/* -------------------------------------------------------------- *
 *  Requests
 * -------------------------------------------------------------- */

pub use luyaz_api::chill::{chill_mode, count_sites, parse_home_exit, ts_path, ChillMode};

pub struct Ctx {
    pub configured: bool,
    pub chill: ChillMode,
    pub true_roaming: Option<bool>,
    pub list_sites: usize,
    pub confirm: bool,
}

/// Can `item` go `on` now? `Err(code)` explains why not. Turning off is
/// always allowed (except while another switch is in progress).
pub fn check_request(m: &Machine, item: Item, on: bool, c: &Ctx) -> Result<(), &'static str> {
    if m.busy.is_some() || m.ticking {
        return Err("busy");
    }
    if !c.configured {
        return Err("not_configured");
    }
    if !on {
        return Ok(());
    }
    if let Some(b) = c.chill.blocked() {
        return Err(b);
    }
    if item == Item::Ip && c.list_sites == 0 {
        return Err("list_empty");
    }
    // O4/D17 roaming gate: truly roaming, or not known, needs the second tap.
    if c.true_roaming != Some(false) && !c.confirm {
        return Err("need_confirm");
    }
    Ok(())
}

/// One switch, start to end (run in its own thread; `m` is held only for the
/// state changes). Turning on: test home → apply → trial (D6: the four checks must all
/// pass within 30 s) → on, or off again with an alert. `in_china` marks a
/// hand re-open for D7.
pub fn run_job(m: &Mutex<Machine>, io: &mut dyn Io, clock: &dyn Clock, item: Item, on: bool, in_china: bool) {
    let lock = || m.lock().unwrap_or_else(|e| e.into_inner());
    let old = lock().p.items;
    let want = old.with(item, on);

    let fail = |io: &mut dyn Io, why: Fail, detail: &str| {
        let now = clock.now();
        let mut g = lock();
        let what = if on { "没开成" } else { "没关成" };
        // short: it is the second line of a row on the screen (≈220 px)
        let what_en = if on { "Didn't turn on" } else { "Didn't turn off" };
        g.say(now, "failed", Some(item), format!("{what}：{}", why.zh()), format!("{what_en}: {}", why.en()));
        g.p.notice.code = why.code().into();
        g.p.notice.detail = detail.chars().take(200).collect();
        g.busy = None;
        io.save(&g.p);
    };

    if on {
        if let Err(f) = io.precheck() {
            fail(io, f, "");
            return;
        }
        if let Some(b) = lock().busy.as_mut() {
            b.phase = "applying";
        }
    }
    if let Err(e) = io.apply(want) {
        fail(io, Fail::Core, &e);
        return;
    }
    if !on {
        let mut g = lock();
        g.p.items = want;
        if item == Item::Rest {
            g.p.rest_until = 0;
        }
        if g.p.reopened_in_china.get(item) {
            g.p.reopened_in_china = g.p.reopened_in_china.with(item, false);
        }
        g.p.notice = Notice::default();
        g.busy = None;
        io.save(&g.p);
        return;
    }

    let start = clock.now();
    {
        let mut g = lock();
        g.busy = Some(Busy { item: item.name(), on, phase: "trial", until: start + TRIAL_SECS });
    }
    let mut last = Fail::Core;
    let mut passed = false;
    while clock.now().saturating_sub(start) < TRIAL_SECS {
        clock.sleep(TRIAL_STEP);
        match io.check() {
            Ok(()) => {
                passed = true;
                break;
            }
            Err(f) => last = f,
        }
    }
    if !passed {
        // Back the way it was; the alert says why (DD6: self-check failures alert).
        if let Err(e) = io.apply(old) {
            eprintln!("[home_exit] could not undo after a failed check: {e}");
        }
        io.alert("home-exit-check-failed", &format!("家里出口自检没过（{}），已关回去", last.zh()));
        fail(io, last, "");
        return;
    }
    let now = clock.now();
    let mut g = lock();
    g.p.items = want;
    if item == Item::Rest {
        g.p.rest_until = now + REST_SECS;
    }
    if in_china {
        g.p.reopened_in_china = g.p.reopened_in_china.with(item, true);
    }
    g.say(
        now,
        "opened",
        Some(item),
        "已走家里 · 重开 App 生效".into(),
        "Via home now · reopen apps".into(),
    );
    g.home_ok = Some(true);
    g.busy = None;
    io.save(&g.p);
}

/* -------------------------------------------------------------- *
 *  Pure helpers
 * -------------------------------------------------------------- */

/// `1` in chill.env = on, anything else off (chill.sh's rule).
pub fn env_items(media: &str, ip: &str, rest: &str) -> Items {
    Items { media: media == "1", ip: ip == "1", rest: rest == "1" }
}


/* -------------------------------------------------------------- *
 *  Real I/O
 * -------------------------------------------------------------- */

/// The engine's `Io` on a device: mihomo over HTTP, everything else through the host.
pub struct Real {
    host: Arc<dyn Host>,
    api: String,
    fast: ureq::Agent,
    slow: ureq::Agent,
    home_ip: Option<Ipv4Addr>,
}

impl Real {
    pub fn new(host: Arc<dyn Host>) -> Self {
        let agent = |secs| -> ureq::Agent {
            ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(secs)))
                .http_status_as_error(false)
                .build()
                .into()
        };
        Real {
            api: host.mihomo_api(),
            fast: agent(3),
            // a 3 s delay test needs more than 3 s of HTTP
            slow: agent(6),
            home_ip: parse_home_exit(&host.env_var("CHILL_HOME_EXIT")).map(|(ip, _)| ip),
            host,
        }
    }

    fn get(&self, path: &str, slow: bool) -> Option<(u16, Value)> {
        let a = if slow { &self.slow } else { &self.fast };
        let mut req = a.get(format!("{}{path}", self.api));
        let secret = self.host.env_var("CHILL_SECRET");
        if !secret.is_empty() {
            req = req.header("Authorization", format!("Bearer {secret}"));
        }
        let mut r = req.call().ok()?;
        let code = r.status().as_u16();
        Some((code, r.body_mut().read_json::<Value>().unwrap_or(Value::Null)))
    }

    fn delay(&self, proxy: &str) -> Option<Option<u32>> {
        let path = format!("/proxies/{}/delay?url={}&timeout=3000", enc(proxy), enc(&probe_url(&*self.host)));
        match self.get(&path, true)? {
            (200, v) => Some(v.get("delay").and_then(Value::as_u64).filter(|d| *d > 0).map(|d| d as u32)),
            // mihomo answered, the target didn't
            (408 | 503 | 504, _) => Some(None),
            _ => None,
        }
    }

    fn group_now(&self, group: &str) -> Option<String> {
        let (code, v) = self.get(&format!("/proxies/{}", enc(group)), false)?;
        (code < 400).then(|| v.get("now").and_then(Value::as_str).map(str::to_string))?
    }

    fn ts_offline(&self) -> bool {
        let ts = self.home_ip.and_then(|ip| self.host.tailscale_json().and_then(|s| ts_path(&s, ip)));
        ts == Some("offline")
    }
}

impl Io for Real {
    fn cell_up(&mut self) -> Option<bool> {
        self.host.cell_up()
    }
    fn probe_home(&mut self) -> Option<Option<u32>> {
        // An idle path often misses the first try (manual_first's experience).
        match self.delay(HOME_PROXY)? {
            None => self.delay(HOME_PROXY),
            ok => Some(ok),
        }
    }

    fn apply(&mut self, items: Items) -> Result<(), String> {
        let keys = ["CHILL_HOME_MEDIA", "CHILL_HOME_IP", "CHILL_HOME_REST"];
        let old: Vec<String> = keys.iter().map(|k| self.host.env_var(k)).collect();
        let v = |b: bool| if b { "1" } else { "0" };
        self.host.set_env_vars(&[(keys[0], v(items.media)), (keys[1], v(items.ip)), (keys[2], v(items.rest))])?;
        if !self.host.running() {
            // Rendered from chill.env when CHILL next starts.
            return Ok(());
        }
        self.host.chill_reload().inspect_err(|_| {
            let back: Vec<(&str, &str)> = keys.iter().copied().zip(old.iter().map(String::as_str)).collect();
            if let Err(w) = self.host.set_env_vars(&back) {
                eprintln!("[home_exit] could not put chill.env back: {w}");
            }
        })
    }

    fn precheck(&mut self) -> Result<(), Fail> {
        if matches!(self.probe_home(), Some(Some(_))) {
            return Ok(());
        }
        Err(if self.ts_offline() { Fail::Tailscale } else { Fail::Home })
    }

    fn check(&mut self) -> Result<(), Fail> {
        if !matches!(self.get("/version", false), Some((200, _))) {
            return Err(Fail::Core);
        }
        if !matches!(self.probe_home(), Some(Some(_))) {
            // Name the deeper cause when Tailscale knows the router is gone.
            return Err(if self.ts_offline() { Fail::Tailscale } else { Fail::Home });
        }
        if !matches!(self.delay("DIRECT"), Some(Some(_))) {
            return Err(Fail::Direct);
        }
        if self.ts_offline() {
            return Err(Fail::Tailscale);
        }
        Ok(())
    }

    fn alert(&mut self, kind: &str, text: &str) {
        self.host.alert(kind, text);
    }

    fn save(&mut self, p: &Persist) {
        if let Ok(s) = serde_json::to_vec(p) {
            let _ = self.host.write(STATE_FILE, &s);
        }
    }
}

pub struct WallClock;

impl Clock for WallClock {
    fn now(&self) -> u64 {
        now_unix()
    }
    fn sleep(&self, secs: u64) {
        thread::sleep(Duration::from_secs(secs));
    }
}

fn probe_url(host: &dyn Host) -> String {
    let u = host.env_var("CHILL_HOME_PROBE_URL");
    if u.starts_with("http://") || u.starts_with("https://") {
        u
    } else {
        DEFAULT_PROBE_URL.to_string()
    }
}

fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(*b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/* -------------------------------------------------------------- *
 *  The engine: one machine, its loop and the HTTP handlers
 * -------------------------------------------------------------- */

type MakeIo = dyn Fn(Arc<dyn Host>) -> Box<dyn Io + Send> + Send + Sync;

/// One home exit: the machine, the 30 s loop and the two handlers. Exactly one program runs it
/// at a time (the core).
pub struct HomeExit {
    m: Mutex<Machine>,
    host: Arc<dyn Host>,
    clock: Arc<dyn Clock + Send + Sync>,
    make_io: Box<MakeIo>,
    /// Where u60d's shadow learns each tick (the shadow records).
    publish: Option<Box<dyn Fn(Value) + Send + Sync>>,
    ts_cache: Mutex<Option<(u64, Option<&'static str>)>>,
    ts_refreshing: std::sync::atomic::AtomicBool,
    /// [`HomeExit::sim_changed`] waiting for the next tick.
    sim_pending: std::sync::atomic::AtomicBool,
}

impl HomeExit {
    pub fn new(host: Arc<dyn Host>) -> Self {
        HomeExit {
            m: Mutex::new(Machine::default()),
            host,
            clock: Arc::new(WallClock),
            make_io: Box::new(|h| Box::new(Real::new(h))),
            publish: None,
            ts_cache: Mutex::new(None),
            ts_refreshing: std::sync::atomic::AtomicBool::new(false),
            sim_pending: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// The SIM changed and is online again: the next tick starts its miss count over
    /// ([`Machine::sim_changed`]). Taken by the tick, not here, so a tick running on its copy
    /// does not write the old count back over it.
    pub fn sim_changed(&self) {
        self.sim_pending.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Publish shadow records (`/tmp/home-exit.tick.json`).
    pub fn with_publish(mut self, f: impl Fn(Value) + Send + Sync + 'static) -> Self {
        self.publish = Some(Box::new(f));
        self
    }

    /// Tests: a faster clock and a fake `Io` instead of mihomo.
    #[doc(hidden)]
    pub fn with_test_parts(
        mut self,
        clock: Arc<dyn Clock + Send + Sync>,
        make_io: impl Fn(Arc<dyn Host>) -> Box<dyn Io + Send> + Send + Sync + 'static,
    ) -> Self {
        self.clock = clock;
        self.make_io = Box::new(make_io);
        self
    }

    fn machine(&self) -> std::sync::MutexGuard<'_, Machine> {
        self.m.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn io(&self) -> Box<dyn Io + Send> {
        (self.make_io)(Arc::clone(&self.host))
    }

    fn publish(&self, v: Value) {
        if let Some(p) = &self.publish {
            p(v);
        }
    }

    fn configured(&self) -> bool {
        parse_home_exit(&self.host.env_var("CHILL_HOME_EXIT")).is_some()
    }

    fn env_items(&self) -> Items {
        let r = |k| self.host.env_var(k);
        env_items(&r("CHILL_HOME_MEDIA"), &r("CHILL_HOME_IP"), &r("CHILL_HOME_REST"))
    }

    fn chill_mode_now(&self) -> ChillMode {
        let mode = self.host.read(MODE_FILE).unwrap_or_default();
        chill_mode(self.host.chill_state().as_ref(), &mode)
    }

    fn list_sites(&self) -> usize {
        self.host.read(HOME_IP_LIST).map(|s| count_sites(&s)).unwrap_or(0)
    }

    /// Whether this program owns the home exit now ([`Host::owner`]).
    pub fn owner(&self) -> bool {
        self.host.owner()
    }

    /// Reconcile the saved state with chill.env (A2), then tick every 30 s, on its own thread.
    /// Only while the host owns the home exit; taking it (back) starts with a fresh reconcile.
    pub fn start(self: &Arc<Self>) {
        let me = Arc::clone(self);
        thread::Builder::new()
            .name("home-exit".into())
            .spawn(move || {
                let mut owned = false;
                let mut quiet = 0u64;
                loop {
                    let now_owned = me.host.owner();
                    if now_owned && !owned {
                        me.startup();
                    } else if now_owned {
                        me.tick_once(&mut quiet);
                    } else if owned {
                        // handed back: GET says "updating" until it is ours again
                        me.machine().ready = false;
                    }
                    owned = now_owned;
                    me.clock.sleep(TICK.as_secs());
                }
            })
            .ok();
    }

    fn startup(&self) {
        let saved = self.host.read(STATE_FILE).and_then(|s| serde_json::from_str::<Persist>(&s).ok());
        let had_file = saved.is_some();
        let mut m = Machine::new(saved.unwrap_or_default());
        let (env, now) = (self.env_items(), self.clock.now());
        let before = m.clone();
        let mut io = self.io();
        let mut rec = Recorder { inner: &mut *io, events: Vec::new() };
        m.startup(had_file, env, now, &mut rec);
        let events = rec.events;
        if worth_a_record(&before, &m, &events) {
            let step = json!({"step": "startup", "had_file": had_file, "env": env, "now": now});
            self.publish(step_record(step, &before, &events, &m, 0));
        } else {
            // u60d still learns the restarted machine (its probe results are gone)
            self.publish(adopt_record(&m));
        }
        *self.machine() = m;
    }

    fn tick_once(&self, quiet: &mut u64) {
        if !self.configured() {
            return;
        }
        let o = Obs {
            now: self.clock.now(),
            serving_mcc: self.host.where_now().0,
            env: self.env_items(),
            clients: self.host.clients_now(),
            screen_on: self.host.screen_on(),
            cell_busy: self.host.cell_busy(),
        };
        // Tick a copy so GET is not held up while it probes or reloads.
        let mut copy = {
            let mut g = self.machine();
            if g.busy.is_some() {
                return;
            }
            g.ticking = true;
            g.clone_for_tick()
        };
        if self.sim_pending.swap(false, std::sync::atomic::Ordering::SeqCst) {
            eprintln!("[home_exit] SIM changed: miss count reset");
            copy.sim_changed();
        }
        let before = copy.clone();
        let mut io = self.io();
        let mut rec = Recorder { inner: &mut *io, events: Vec::new() };
        copy.tick(&o, &mut rec);
        let events = rec.events;
        if worth_a_record(&before, &copy, &events) {
            let step = json!({"step": "tick", "obs": {"now": o.now, "serving_mcc": o.serving_mcc, "env": o.env, "clients": o.clients, "screen_on": o.screen_on, "cell_busy": o.cell_busy}});
            self.publish(step_record(step, &before, &events, &copy, *quiet));
            *quiet = 0;
        } else {
            *quiet += 1;
        }
        let mut g = self.machine();
        copy.ticking = false;
        *g = copy;
    }

    /// Tailscale path to home, cached for a minute. The CLI forks and may take
    /// seconds, longer than the touch screen waits for a reply, so a stale cache
    /// is refreshed in the background and this returns what it has.
    fn ts_now(self: &Arc<Self>, ip: Ipv4Addr) -> Option<&'static str> {
        use std::sync::atomic::Ordering;
        let now = self.clock.now();
        let cached = *self.ts_cache.lock().unwrap_or_else(|e| e.into_inner());
        let fresh = cached.is_some_and(|(at, _)| now.saturating_sub(at) < TS_CACHE_SECS);
        if !fresh && !self.ts_refreshing.swap(true, Ordering::SeqCst) {
            let me = Arc::clone(self);
            let spawned = thread::Builder::new()
                .name("home-exit-ts".into())
                .spawn(move || {
                    let v = me.host.tailscale_json().and_then(|s| ts_path(&s, ip));
                    *me.ts_cache.lock().unwrap_or_else(|e| e.into_inner()) = Some((me.clock.now(), v));
                    me.ts_refreshing.store(false, Ordering::SeqCst);
                })
                .is_ok();
            if !spawned {
                self.ts_refreshing.store(false, Ordering::SeqCst);
            }
        }
        cached.and_then(|(_, v)| v)
    }

    /// The machine as the replies read it (luyaz_api::chill renders them).
    pub fn view(&self) -> luyaz_api::chill::Home {
        home_view(&self.machine())
    }

    /// For `/api/services/chill` (the home exit line on the screens, DD2):
    /// what is on, rest's end, and whether home is on its fallback.
    pub fn summary(&self) -> Value {
        luyaz_api::chill::home_summary(&self.view())
    }

    /// GET /api/services/chill/home
    pub fn get(self: &Arc<Self>) -> (u16, Value) {
        let home = parse_home_exit(&self.host.env_var("CHILL_HOME_EXIT"));
        if home.is_none() {
            return (200, json!({"ok": true, "data": {"configured": false}}));
        }
        let (serving_mcc, true_roaming) = self.host.where_now();
        let mode = self.chill_mode_now();
        let tailscale = home.and_then(|(ip, _)| self.ts_now(ip));
        let m = self.view();
        // Where the traffic goes while home is down (DD6 "暂走 X").
        let fallback_to = if m.ready && luyaz_api::chill::wants_fallback(&m, mode) {
            Real::new(Arc::clone(&self.host)).group_now(FISH_GROUP)
        } else {
            None
        };
        let around = luyaz_api::chill::Around {
            serving_mcc,
            true_roaming,
            blocked: mode.blocked(),
            sites: self.list_sites(),
            fallback_to,
            tailscale,
            now: self.clock.now(),
        };
        (200, json!({"ok": true, "data": luyaz_api::chill::home_get(&m, &around)}))
    }

    /// PUT /api/services/chill/home — `{item: "media"|"ip"|"rest", on, confirm?}`
    /// or `{action: "extend"}`.
    ///
    /// Turning on answers 202 at once and runs in the background (reload, then
    /// the 30 s trial); the screens follow `busy` and `notice` on GET.
    pub fn put(self: &Arc<Self>, body: &[u8]) -> (u16, Value) {
        if !self.owner() {
            return (409, json!({"ok": false, "error": "not_owner", "code": "busy"}));
        }
        let parsed: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
        if parsed["action"].as_str() == Some("extend") {
            let mut m = self.machine();
            if !m.ready || m.ticking {
                return (409, json!({"ok": false, "error": "busy", "code": "busy"}));
            }
            let now = self.clock.now();
            return match m.extend(now, &mut *self.io()) {
                Ok(until) => (200, json!({"ok": true, "data": {"until": until}})),
                Err(code) => (409, json!({"ok": false, "error": code, "code": code})),
            };
        }
        let Some(item) = parsed["item"].as_str().and_then(Item::parse) else {
            return (400, json!({"ok": false, "error": "item must be media, ip or rest"}));
        };
        let Some(on) = parsed["on"].as_bool() else {
            return (400, json!({"ok": false, "error": "missing 'on'"}));
        };
        let (serving, roaming) = self.host.where_now();
        let ctx = Ctx {
            configured: self.configured(),
            chill: self.chill_mode_now(),
            true_roaming: roaming,
            list_sites: self.list_sites(),
            confirm: parsed["confirm"].as_bool().unwrap_or(false),
        };
        {
            let mut m = self.machine();
            if !m.ready {
                return (409, json!({"ok": false, "error": "busy", "code": "busy"}));
            }
            if m.p.items.get(item) == on {
                return (200, json!({"ok": true, "data": {"item": item.name(), "on": on, "unchanged": true}}));
            }
            if let Err(code) = check_request(&m, item, on, &ctx) {
                return (409, json!({"ok": false, "error": code, "code": code}));
            }
            let phase = if on { "checking" } else { "applying" };
            m.busy = Some(Busy { item: item.name(), on, phase, until: 0 });
        }
        let in_china = serving == Some(460);
        let me = Arc::clone(self);
        thread::Builder::new()
            .name("home-exit-job".into())
            .spawn(move || {
                run_job(&me.m, &mut *me.io(), &*me.clock, item, on, in_china);
                me.publish(adopt_record(&me.machine()));
            })
            .ok();
        (202, json!({"ok": true, "data": {"item": item.name(), "on": on, "busy": true}}))
    }
}

/* -------------------------------------------------------------- *
 *  Shadow records for u60d
 * -------------------------------------------------------------- */

/// u60d replays each tick and the start-up through its copy of the machine (it must not probe home
/// or reload CHILL itself): the inputs, every `Io` call and answer in order, and the machine before
/// and after. Ticks that changed nothing and asked nothing are left out (counted in `quiet`).
/// Requests (`run_job`) are not replayed: an `adopt` record after each gives the machine as it is.
struct Recorder<'a> {
    inner: &'a mut dyn Io,
    events: Vec<Value>,
}

fn fail_name(f: Fail) -> &'static str {
    match f {
        Fail::Core => "core",
        Fail::Home => "home",
        Fail::Tailscale => "tailscale",
        Fail::Direct => "direct",
    }
}

impl Io for Recorder<'_> {
    fn probe_home(&mut self) -> Option<Option<u32>> {
        let got = self.inner.probe_home();
        // answered false = mihomo silent; ms null = home silent
        self.events.push(json!({"call": "probe_home", "answered": got.is_some(), "ms": got.flatten()}));
        got
    }
    fn apply(&mut self, items: Items) -> Result<(), String> {
        let r = self.inner.apply(items);
        self.events.push(json!({"call": "apply", "items": items, "err": r.as_ref().err()}));
        r
    }
    fn check(&mut self) -> Result<(), Fail> {
        let r = self.inner.check();
        self.events.push(json!({"call": "check", "fail": r.err().map(fail_name)}));
        r
    }
    fn precheck(&mut self) -> Result<(), Fail> {
        let r = self.inner.precheck();
        self.events.push(json!({"call": "precheck", "fail": r.err().map(fail_name)}));
        r
    }
    fn alert(&mut self, kind: &str, text: &str) {
        self.inner.alert(kind, text);
        self.events.push(json!({"call": "alert", "kind": kind}));
    }
    fn save(&mut self, p: &Persist) {
        self.inner.save(p);
        self.events.push(json!({"call": "save"}));
    }
    fn cell_up(&mut self) -> Option<bool> {
        let up = self.inner.cell_up();
        self.events.push(json!({"call": "cell_up", "up": up}));
        up
    }
}

/// The machine without the request bookkeeping (busy, ready, ticking).
fn machine_json(m: &Machine) -> Value {
    json!({
        "p": m.p,
        "last_probe": m.last_probe,
        "fails": m.fails,
        "idle": m.idle,
        "home_ms": m.home_ms,
        "home_ok": m.home_ok,
        "probed_at": m.probed_at,
        "china_since": m.china_since,
    })
}

/// After a request (not replayed: it reloads CHILL and probes home) and after a start-up that
/// recorded nothing: the machine as it now is, for u60d to take as it is (its touch layer reads
/// the probe results from it).
fn adopt_record(m: &Machine) -> Value {
    json!({"kind": "adopt", "after": machine_json(m)})
}

fn worth_a_record(before: &Machine, after: &Machine, events: &[Value]) -> bool {
    !events.is_empty() || machine_json(before) != machine_json(after)
}

fn step_record(step: Value, before: &Machine, events: &[Value], after: &Machine, quiet: u64) -> Value {
    json!({
        "kind": "step",
        "step": step,
        "before": machine_json(before),
        "io": events,
        "after": machine_json(after),
        "quiet": quiet,
    })
}

/// The machine as the replies read it (luyaz_api::chill renders them, so u60d's touch layer answers
/// alike).
fn home_view(m: &Machine) -> luyaz_api::chill::Home {
    luyaz_api::chill::Home {
        ready: m.ready,
        media: m.p.items.media,
        ip: m.p.items.ip,
        rest: m.p.items.rest,
        rest_until: m.p.rest_until,
        home_ok: m.home_ok,
        home_ms: m.home_ms,
        probed_at: m.probed_at,
        busy: json!(m.busy),
        notice: json!(m.p.notice),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[derive(Default)]
    struct Fake {
        env: Items,
        applied: Vec<Items>,
        apply_fail: bool,
        probes: Vec<Option<Option<u32>>>,
        checks: Vec<Result<(), Fail>>,
        pre: Option<Fail>,
        alerts: Vec<String>,
        saves: usize,
        cell: Option<bool>,
    }

    impl Io for Fake {
        fn probe_home(&mut self) -> Option<Option<u32>> {
            if self.probes.is_empty() {
                Some(Some(40))
            } else {
                self.probes.remove(0)
            }
        }
        fn apply(&mut self, items: Items) -> Result<(), String> {
            if self.apply_fail {
                return Err("mihomo -t: bad".into());
            }
            self.env = items;
            self.applied.push(items);
            Ok(())
        }
        fn check(&mut self) -> Result<(), Fail> {
            if self.checks.is_empty() {
                Ok(())
            } else {
                self.checks.remove(0)
            }
        }
        fn precheck(&mut self) -> Result<(), Fail> {
            self.pre.map_or(Ok(()), Err)
        }
        fn alert(&mut self, kind: &str, _text: &str) {
            self.alerts.push(kind.into());
        }
        fn save(&mut self, _p: &Persist) {
            self.saves += 1;
        }
        fn cell_up(&mut self) -> Option<bool> {
            self.cell
        }
    }

    struct FakeClock(Cell<u64>);
    impl Clock for FakeClock {
        fn now(&self) -> u64 {
            self.0.get()
        }
        fn sleep(&self, secs: u64) {
            self.0.set(self.0.get() + secs);
        }
    }

    const T0: u64 = 1_000_000;

    fn on(items: &[Item]) -> Items {
        items.iter().fold(Items::default(), |a, i| a.with(*i, true))
    }

    fn obs(now: u64, mcc: Option<i64>, env: Items) -> Obs {
        Obs { now, serving_mcc: mcc, env, clients: None, ..Default::default() }
    }

    fn ctx(chill: ChillMode, roaming: Option<bool>, confirm: bool) -> Ctx {
        Ctx { configured: true, chill, true_roaming: roaming, list_sites: 3, confirm }
    }

    fn job(m: Machine, io: &mut Fake, item: Item, turn_on: bool, in_china: bool) -> Machine {
        let cell = Mutex::new(m);
        let clock = FakeClock(Cell::new(T0));
        run_job(&cell, io, &clock, item, turn_on, in_china);
        cell.into_inner().unwrap()
    }

    #[test]
    fn requests_are_gated() {
        let m = Machine::default();
        assert_eq!(check_request(&m, Item::Media, true, &ctx(ChillMode::Rule, Some(false), false)), Ok(()));
        // DD4 reason codes
        assert_eq!(check_request(&m, Item::Media, true, &ctx(ChillMode::Off, Some(false), true)), Err("chill_off"));
        assert_eq!(check_request(&m, Item::Media, true, &ctx(ChillMode::Hot, Some(false), true)), Err("hot"));
        assert_eq!(check_request(&m, Item::Rest, true, &ctx(ChillMode::Global, Some(false), true)), Err("mode_global"));
        assert_eq!(check_request(&m, Item::Ip, true, &ctx(ChillMode::Direct, Some(false), true)), Err("mode_direct"));
        // turning off is always fine, even with CHILL off
        assert_eq!(check_request(&m, Item::Media, false, &ctx(ChillMode::Off, None, false)), Ok(()));
        // roaming gate: truly roaming or unknown needs the confirm
        assert_eq!(check_request(&m, Item::Media, true, &ctx(ChillMode::Rule, Some(true), false)), Err("need_confirm"));
        assert_eq!(check_request(&m, Item::Media, true, &ctx(ChillMode::Rule, None, false)), Err("need_confirm"));
        assert_eq!(check_request(&m, Item::Media, true, &ctx(ChillMode::Rule, Some(true), true)), Ok(()));
        // empty list
        let mut c = ctx(ChillMode::Rule, Some(false), true);
        c.list_sites = 0;
        assert_eq!(check_request(&m, Item::Ip, true, &c), Err("list_empty"));
        assert_eq!(check_request(&m, Item::Media, true, &c), Ok(()));
        // not configured, busy
        c.configured = false;
        assert_eq!(check_request(&m, Item::Media, false, &c), Err("not_configured"));
        let busy = Machine { busy: Some(Busy { item: "rest", on: true, phase: "trial", until: 0 }), ..Default::default() };
        assert_eq!(check_request(&busy, Item::Media, false, &ctx(ChillMode::Rule, Some(false), true)), Err("busy"));
    }

    #[test]
    fn turning_on_passes_the_trial() {
        let mut io = Fake::default();
        let m = job(Machine::default(), &mut io, Item::Rest, true, false);
        assert!(m.p.items.rest && m.busy.is_none());
        assert!(m.p.rest_until >= T0 + REST_SECS);
        assert_eq!(m.p.notice.kind, "opened");
        assert_eq!(io.applied, vec![on(&[Item::Rest])]);
    }

    /// D6: each of the four checks failing for the whole 30 s turns it back off.
    #[test]
    fn each_failed_check_undoes() {
        for f in [Fail::Core, Fail::Home, Fail::Tailscale, Fail::Direct] {
            let mut io = Fake { checks: vec![Err(f); 10], ..Default::default() };
            let m = job(Machine::default(), &mut io, Item::Media, true, false);
            assert!(!m.p.items.media, "{f:?}");
            assert_eq!(io.applied, vec![on(&[Item::Media]), Items::default()], "{f:?}");
            assert_eq!(m.p.notice.kind, "failed");
            assert!(m.p.notice.text.contains(f.zh()), "{f:?}");
            assert_eq!(io.alerts, vec!["home-exit-check-failed"]);
            assert!(m.busy.is_none());
        }
        // a failure that clears within the window is fine
        let mut io = Fake { checks: vec![Err(Fail::Home), Err(Fail::Home)], ..Default::default() };
        let m = job(Machine::default(), &mut io, Item::Media, true, false);
        assert!(m.p.items.media);
        assert!(io.alerts.is_empty());
    }

    /// §4.4: home unreachable before turning on: nothing applied, no alert.
    #[test]
    fn precheck_stops_before_applying() {
        for f in [Fail::Home, Fail::Tailscale] {
            let mut io = Fake { pre: Some(f), ..Default::default() };
            let m = job(Machine::default(), &mut io, Item::Media, true, false);
            assert!(io.applied.is_empty() && io.alerts.is_empty());
            assert_eq!(m.p.notice.kind, "failed");
            assert!(m.p.notice.text.contains(f.zh()));
            assert_eq!(m.p.notice.code, f.code());
            assert!(m.busy.is_none());
        }
        // turning off never tests home
        let mut m0 = Machine::default();
        m0.p.items = on(&[Item::Media]);
        let mut io = Fake { pre: Some(Fail::Home), ..Default::default() };
        let m = job(m0, &mut io, Item::Media, false, false);
        assert!(!m.p.items.media);
    }

    /// Q1: a failed reload leaves things as they were and says why.
    #[test]
    fn failed_reload_keeps_state() {
        let mut io = Fake { apply_fail: true, ..Default::default() };
        let m = job(Machine::default(), &mut io, Item::Ip, true, false);
        assert!(!m.p.items.ip && m.busy.is_none());
        assert_eq!(m.p.notice.kind, "failed");
        assert!(m.p.notice.text.contains("代理没重启成功"));
        assert_eq!(m.p.notice.detail, "mihomo -t: bad");
        assert_eq!(m.p.notice.text_en, "Didn't turn on: proxy didn't restart");
    }

    /// Records as u60d gets them: ticks and a start-up through the recorder parse into
    /// u60-policy's types, and replaying them there gives the machine after.
    #[test]
    fn shadow_records_replay_in_u60_policy() {
        use luyaz_policy::home_exit as pol;
        let check = |step: Value, before: &Machine, events: &[Value], after: &Machine| {
            let v: Value = serde_json::from_str(&step_record(step, before, events, after, 0).to_string()).unwrap();
            let b: pol::Machine = serde_json::from_value(v["before"].clone()).unwrap();
            let a: pol::Machine = serde_json::from_value(v["after"].clone()).unwrap();
            let st: pol::Step = serde_json::from_value(v["step"].clone()).unwrap();
            let evs: Vec<pol::Event> = serde_json::from_value(v["io"].clone()).unwrap();
            let out = pol::replay(&b, &st, evs);
            assert_eq!((out.diverged, out.unused), (None, 0), "{v}");
            assert_eq!(out.after, a, "{v}");
            assert_eq!(serde_json::to_value(&out.after).unwrap(), v["after"]);
        };
        // start-up: rest past its time
        let mut m = Machine::default();
        m.p.items = on(&[Item::Rest, Item::Media]);
        m.p.rest_until = T0 - 1;
        let mut io = Fake::default();
        let before = m.clone();
        let mut rec = Recorder { inner: &mut io, events: Vec::new() };
        let env = on(&[Item::Rest, Item::Media]);
        m.startup(true, env, T0, &mut rec);
        check(json!({"step": "startup", "had_file": true, "env": env, "now": T0}), &before, &rec.events.clone(), &m);
        // ticks: probes failing and a mihomo silence, rest closed, an alert; back home
        let mut m = Machine::default();
        m.p.items = on(&[Item::Rest, Item::Media]);
        m.p.rest_until = T0 + 10 * REST_SECS;
        let mut io = Fake { probes: vec![Some(None), None, Some(None), Some(None), Some(Some(30))], ..Default::default() };
        let mut published = 0;
        for k in 0..12u64 {
            let o = Obs { now: T0 + k * PROBE_EVERY, serving_mcc: Some(if k > 8 { 460 } else { 440 }), env: m.p.items, clients: Some(if k == 10 { 0 } else { 1 }), screen_on: (k % 2 == 0).then_some(false), cell_busy: (k % 3 == 0).then_some(true) };
            let before = m.clone();
            let mut rec = Recorder { inner: &mut io, events: Vec::new() };
            m.tick(&o, &mut rec);
            let events = rec.events;
            if worth_a_record(&before, &m, &events) {
                published += 1;
                let step = json!({"step": "tick", "obs": {"now": o.now, "serving_mcc": o.serving_mcc, "env": o.env, "clients": o.clients, "screen_on": o.screen_on, "cell_busy": o.cell_busy}});
                check(step, &before, &events, &m);
            }
        }
        assert_eq!(io.alerts, vec!["home-exit-down"]);
        assert!(published > 5, "{published}");
    }

    #[test]
    fn turning_off() {
        let mut m0 = Machine::default();
        m0.p.items = on(&[Item::Rest, Item::Media]);
        m0.p.rest_until = T0 + 100;
        let mut io = Fake::default();
        let m = job(m0, &mut io, Item::Rest, false, false);
        assert_eq!(m.p.items, on(&[Item::Media]));
        assert_eq!(m.p.rest_until, 0);
        assert!(io.alerts.is_empty());
    }

    #[test]
    fn rest_ends_after_two_hours() {
        let mut m = Machine::default();
        m.p.items = on(&[Item::Rest]);
        m.p.rest_until = T0 + REST_SECS;
        let mut io = Fake { env: m.p.items, ..Default::default() };
        m.tick(&obs(T0 + REST_SECS - 1, Some(440), on(&[Item::Rest])), &mut io);
        assert!(m.p.items.rest);
        m.tick(&obs(T0 + REST_SECS, Some(440), on(&[Item::Rest])), &mut io);
        assert!(!m.p.items.rest);
        assert_eq!(m.p.notice.kind, "expired");
    }

    /// DD8: extend before the end, not after; the D7 auto-off still applies.
    #[test]
    fn extend() {
        let mut m = Machine::default();
        let mut io = Fake::default();
        assert_eq!(m.extend(T0, &mut io), Err("not_on"));
        m.p.items = on(&[Item::Rest]);
        m.p.rest_until = T0 + 300;
        assert_eq!(m.extend(T0, &mut io), Ok(T0 + REST_SECS));
        m.p.rest_until = T0;
        assert_eq!(m.extend(T0 + 1, &mut io), Err("not_on"));
        // extended, then back home: still closes after 10 minutes
        m.p.rest_until = T0 + REST_SECS;
        let env = on(&[Item::Rest]);
        m.tick(&obs(T0, Some(460), env), &mut io);
        m.tick(&obs(T0 + BACK_HOME_SECS, Some(460), env), &mut io);
        assert!(!m.p.items.rest);
    }

    #[test]
    fn three_failed_probes_close_rest_only() {
        let mut m = Machine::default();
        m.p.items = on(&[Item::Rest, Item::Media]);
        m.p.rest_until = T0 + 10 * REST_SECS;
        let env = m.p.items;
        let mut io = Fake { env, probes: vec![Some(None), None, Some(None), Some(None)], ..Default::default() };
        // probe 1 fails; a mihomo silence in between does not count
        m.tick(&obs(T0, Some(440), env), &mut io);
        m.tick(&obs(T0 + PROBE_EVERY, Some(440), env), &mut io);
        m.tick(&obs(T0 + 2 * PROBE_EVERY, Some(440), env), &mut io);
        assert!(m.p.items.rest && m.home_ok == Some(false));
        m.tick(&obs(T0 + 3 * PROBE_EVERY, Some(440), env), &mut io);
        assert_eq!(m.p.items, on(&[Item::Media]));
        assert_eq!(m.p.notice.kind, "home_down");
        assert_eq!(io.alerts, vec!["home-exit-down"]);
    }

    #[test]
    fn a_sim_change_starts_the_count_over_and_probes_at_once() {
        let mut m = Machine::default();
        m.p.items = on(&[Item::Rest]);
        m.p.rest_until = T0 + 10 * REST_SECS;
        let env = m.p.items;
        let mut io = Fake { env, probes: vec![Some(None); 4], ..Default::default() };
        m.tick(&obs(T0, Some(440), env), &mut io);
        m.tick(&obs(T0 + PROBE_EVERY, Some(440), env), &mut io);
        assert_eq!(m.fails, 2);
        m.sim_changed();
        assert!(m.fails == 0 && m.home_ok.is_none());
        // probed on the next tick, not a PROBE_EVERY later
        m.tick(&obs(T0 + PROBE_EVERY + 30, Some(440), env), &mut io);
        assert_eq!(m.fails, 1);
        m.tick(&obs(T0 + 2 * PROBE_EVERY + 30, Some(440), env), &mut io);
        assert!(m.p.items.rest, "two misses on the new card, not three");
        assert!(io.alerts.is_empty());
    }

    #[test]
    fn cell_down_for_30_minutes_does_not_close_rest() {
        let mut m = Machine::default();
        m.p.items = on(&[Item::Rest]);
        m.p.rest_until = T0 + 10 * REST_SECS;
        let env = m.p.items;
        let mut io = Fake { env, probes: vec![Some(None); 8], cell: Some(false), ..Default::default() };
        for i in 0..6 {
            m.tick(&obs(T0 + i * PROBE_EVERY, Some(440), env), &mut io);
        }
        assert!(m.p.items.rest && m.home_ok == Some(false) && m.fails == 0);
        assert!(io.alerts.is_empty());
        // the cell is back and home still silent: counting starts from there
        io.cell = Some(true);
        m.tick(&obs(T0 + 6 * PROBE_EVERY, Some(440), env), &mut io);
        m.tick(&obs(T0 + 7 * PROBE_EVERY, Some(440), env), &mut io);
        assert!(m.p.items.rest);
        io.probes.push(Some(None));
        m.tick(&obs(T0 + 8 * PROBE_EVERY, Some(440), env), &mut io);
        assert!(!m.p.items.rest);
        assert_eq!(io.alerts, vec!["home-exit-down"]);
    }

    #[test]
    fn a_good_probe_resets_the_count() {
        let mut m = Machine::default();
        m.p.items = on(&[Item::Rest]);
        m.p.rest_until = T0 + 10 * REST_SECS;
        let env = m.p.items;
        let mut io = Fake { probes: vec![Some(None), Some(None), Some(Some(50)), Some(None)], ..Default::default() };
        for k in 0..4 {
            m.tick(&obs(T0 + k * PROBE_EVERY, Some(440), env), &mut io);
        }
        assert!(m.p.items.rest);
    }

    /// §16: no client pauses the probe (and its miss count); the first client
    /// probes on that tick, then every 5 minutes again; unknown probes.
    #[test]
    fn no_client_pauses_the_probe() {
        let mut m = Machine::default();
        m.p.items = on(&[Item::Rest]);
        m.p.rest_until = T0 + 10 * REST_SECS;
        let env = m.p.items;
        let mut io = Fake { probes: vec![Some(None), Some(None), Some(Some(40)), Some(None), Some(Some(41))], ..Default::default() };
        let at = |t: u64, n: Option<i64>| Obs { clients: n, ..obs(t, Some(440), env) };
        m.tick(&at(T0, Some(1)), &mut io);
        m.tick(&at(T0 + PROBE_EVERY, Some(2)), &mut io);
        assert_eq!((io.probes.len(), m.fails), (3, 2));
        for k in 2..20 {
            m.tick(&at(T0 + k * PROBE_EVERY, Some(0)), &mut io);
        }
        assert_eq!((io.probes.len(), m.fails, m.p.items.rest), (3, 0, true));
        // a client connects 30 s after a pause tick: probe right away
        let t = T0 + 19 * PROBE_EVERY + 30;
        m.tick(&at(t, Some(1)), &mut io);
        assert_eq!((io.probes.len(), m.home_ms, m.probed_at), (2, Some(40), t));
        m.tick(&at(t + 30, Some(1)), &mut io);
        assert_eq!(io.probes.len(), 2);
        m.tick(&at(t + PROBE_EVERY, None), &mut io);
        assert_eq!(io.probes.len(), 1);
    }

    /// u60-radio.md §5 (D3): who counts as nobody once the screen is known.
    #[test]
    fn screen_gate() {
        let env = on(&[Item::Rest]);
        let fresh = || {
            let mut m = Machine::default();
            m.p.items = env;
            m.p.rest_until = T0 + 100 * REST_SECS;
            m
        };
        // probes over 2 hours of 30 s ticks
        let run = |screen: Option<bool>, clients: Option<i64>| {
            let mut m = fresh();
            let mut io = Fake { probes: vec![Some(Some(40)); 1000], ..Default::default() };
            for k in 0..240u64 {
                let o = Obs { clients, screen_on: screen, ..obs(T0 + k * 30, Some(440), env) };
                m.tick(&o, &mut io);
            }
            1000 - io.probes.len()
        };
        // screen unknown: the old rule (no client = nobody; unknown probes)
        assert_eq!(run(None, Some(0)), 0);
        assert_eq!(run(None, None), 24);
        assert_eq!(run(None, Some(1)), 24);
        // screen off: no client and unknown are nobody; unknown still every 30 minutes
        assert_eq!(run(Some(false), Some(0)), 0);
        assert_eq!(run(Some(false), None), 4);
        assert_eq!(run(Some(false), Some(2)), 24);
        // screen on probes even with no client (someone is looking at the device)
        assert_eq!(run(Some(true), Some(0)), 24);
        assert_eq!(run(Some(true), None), 24);
    }

    /// Unknown clients with the screen off: misses still count, so a dead home still ends
    /// `rest` (after 3 half-hourly probes).
    #[test]
    fn unknown_clients_still_end_rest() {
        let env = on(&[Item::Rest]);
        let mut m = Machine::default();
        m.p.items = env;
        m.p.rest_until = T0 + 100 * REST_SECS;
        let mut io = Fake { probes: vec![Some(None); 10], ..Default::default() };
        for k in 0..=120u64 {
            m.tick(&Obs { screen_on: Some(false), ..obs(T0 + k * 30, Some(440), env) }, &mut io);
        }
        assert_eq!((io.probes.len(), m.p.items.rest), (7, false));
        assert_eq!(io.alerts, vec!["home-exit-down"]);
    }

    /// D8: a due probe waits for packets on the cell, at most 60 s; unknown never waits; a
    /// client showing up probes at once.
    #[test]
    fn piggyback_waits_at_most_a_minute() {
        let env = on(&[Item::Media]);
        let mut m = Machine::default();
        m.p.items = env;
        let mut io = Fake::default();
        let at = |t: u64, busy: Option<bool>| Obs { clients: Some(1), screen_on: Some(false), cell_busy: busy, ..obs(t, Some(440), env) };
        m.tick(&at(T0, Some(false)), &mut io);
        assert_eq!(m.probed_at, T0, "the first probe does not wait");
        m.tick(&at(T0 + 300, Some(false)), &mut io);
        m.tick(&at(T0 + 330, Some(false)), &mut io);
        assert_eq!(m.probed_at, T0, "due, the cell is asleep: wait");
        m.tick(&at(T0 + 360, Some(false)), &mut io);
        assert_eq!(m.probed_at, T0 + 360, "60 s is the most it waits");
        m.tick(&at(T0 + 690, Some(false)), &mut io);
        m.tick(&at(T0 + 720, Some(true)), &mut io);
        assert_eq!(m.probed_at, T0 + 720, "packets: go now");
        m.tick(&at(T0 + 1020, None), &mut io);
        assert_eq!(m.probed_at, T0 + 1020, "unknown: no waiting");
        // nobody for a while, then a client: at once even with the cell asleep
        m.tick(&Obs { clients: Some(0), ..at(T0 + 1050, Some(false)) }, &mut io);
        m.tick(&at(T0 + 1080, Some(false)), &mut io);
        assert_eq!(m.probed_at, T0 + 1080);
    }

    /// D14: someone there (screen on or a client), the cell asleep throughout, the 30 s loop on
    /// a fake clock: home going down at any moment shows as `home_ok` false within 6.5 minutes.
    #[test]
    fn home_down_shows_within_six_and_a_half_minutes() {
        let env = on(&[Item::Rest]);
        for (screen, clients) in [(Some(true), Some(0)), (Some(false), Some(1)), (Some(true), None)] {
            for down_after in (0..=420).step_by(7) {
                let mut m = Machine::default();
                m.p.items = env;
                m.p.rest_until = T0 + 100 * REST_SECS;
                let clock = FakeClock(Cell::new(T0));
                let down_at = T0 + 600 + down_after;
                let mut io = Fake::default();
                let mut seen = None;
                while clock.now() < down_at + 1200 {
                    let now = clock.now();
                    let o = Obs { clients, screen_on: screen, cell_busy: Some(false), ..obs(now, Some(440), env) };
                    io.probes = vec![if now >= down_at { Some(None) } else { Some(Some(40)) }];
                    m.tick(&o, &mut io);
                    if now >= down_at && m.home_ok == Some(false) {
                        seen = Some(now - down_at);
                        break;
                    }
                    clock.sleep(TICK.as_secs());
                }
                let took = seen.expect("never noticed");
                assert!(took <= 390, "{screen:?} {clients:?}: down at +{down_after}, noticed after {took} s");
            }
        }
    }

    /// D7: 9 minutes no, 10 yes; null never counts; ip unaffected; reopened kept.
    #[test]
    fn back_home() {
        let env = on(&[Item::Media, Item::Rest, Item::Ip]);
        let fresh = || {
            let mut m = Machine::default();
            m.p.items = env;
            m.p.rest_until = T0 + 10 * REST_SECS;
            m
        };
        let mut m = fresh();
        let mut io = Fake::default();
        m.tick(&obs(T0, Some(460), env), &mut io);
        m.tick(&obs(T0 + 540, Some(460), env), &mut io);
        assert_eq!(m.p.items, env, "9 minutes");
        m.tick(&obs(T0 + 600, Some(460), env), &mut io);
        assert_eq!(m.p.items, on(&[Item::Ip]), "10 minutes; ip stays");
        assert_eq!(m.p.notice.kind, "back_home");

        // unknown in between restarts the count
        let mut m = fresh();
        m.tick(&obs(T0, Some(460), env), &mut io);
        m.tick(&obs(T0 + 300, None, env), &mut io);
        m.tick(&obs(T0 + 600, Some(460), env), &mut io);
        m.tick(&obs(T0 + 1000, Some(460), env), &mut io);
        assert_eq!(m.p.items, env);
        m.tick(&obs(T0 + 1200, Some(460), env), &mut io);
        assert_eq!(m.p.items, on(&[Item::Ip]));

        // opened by hand in China: left alone
        let mut io = Fake::default();
        let m0 = job(Machine::default(), &mut io, Item::Media, true, true);
        let mut m = m0;
        m.tick(&obs(T0, Some(460), on(&[Item::Media])), &mut io);
        m.tick(&obs(T0 + 700, Some(460), on(&[Item::Media])), &mut io);
        assert!(m.p.items.media);
        // abroad again forgets it
        m.tick(&obs(T0 + 800, Some(440), on(&[Item::Media])), &mut io);
        assert!(!m.p.reopened_in_china.media);
    }

    /// A2: what was on when nothing was watching.
    #[test]
    fn startup_reconcile() {
        // no saved state but env on → off
        let mut m = Machine::default();
        let mut io = Fake::default();
        m.startup(false, on(&[Item::Rest, Item::Media]), T0, &mut io);
        assert_eq!(m.p.items, Items::default());
        assert_eq!(io.applied, vec![Items::default()]);
        assert_eq!(m.p.notice.kind, "stale");

        // rest past its time → off, media kept
        let mut m = Machine::default();
        m.p.items = on(&[Item::Rest, Item::Media]);
        m.p.rest_until = T0 - 1;
        let mut io = Fake::default();
        m.startup(true, on(&[Item::Rest, Item::Media]), T0, &mut io);
        assert_eq!(m.p.items, on(&[Item::Media]));

        // still in time → adopt
        let mut m = Machine::default();
        m.p.rest_until = T0 + 60;
        let mut io = Fake::default();
        m.startup(true, on(&[Item::Rest]), T0, &mut io);
        assert_eq!(m.p.items, on(&[Item::Rest]));
        assert!(io.applied.is_empty());

        // nothing on, nothing to do
        let mut m = Machine::default();
        let mut io = Fake::default();
        m.startup(false, Items::default(), T0, &mut io);
        assert!(io.applied.is_empty() && io.saves == 0);
    }

    #[test]
    fn tick_follows_a_hand_edited_env() {
        let mut m = Machine::default();
        let mut io = Fake::default();
        m.tick(&obs(T0, None, on(&[Item::Rest])), &mut io);
        assert!(m.p.items.rest);
        assert_eq!(m.p.rest_until, T0 + REST_SECS);
        m.tick(&obs(T0 + 30, None, Items::default()), &mut io);
        assert_eq!(m.p.rest_until, 0);
    }

    #[test]
    fn env_and_list() {
        assert_eq!(env_items("1", "0", ""), on(&[Item::Media]));
    }

    /// u60d takes an `adopt` record's machine as u60-policy's (its touch layer reads the probe
    /// results from it).
    #[test]
    fn adopt_records_read_as_the_policy_machine() {
        let mut m = Machine::new(Persist { items: on(&[Item::Ip]), ..Default::default() });
        m.home_ok = Some(true);
        m.home_ms = Some(40);
        m.busy = Some(Busy { item: "ip", on: true, phase: "trial", until: 9 });
        let r = adopt_record(&m);
        assert_eq!(r["kind"], "adopt");
        let back: luyaz_policy::home_exit::Machine = serde_json::from_value(r["after"].clone()).unwrap();
        assert_eq!((back.home_ok, back.home_ms, back.p.items.ip), (Some(true), Some(40), true));
        assert!(r["after"].get("busy").is_none(), "request bookkeeping stays out");
    }
}

