//! The screen owner (u60-sentinel.md §5): who is on the panel, ours (luyaz-ui) or the vendor's
//! (zte_topsw_devui). A port of u60-uid (touch-ui `src/uid.c` + `src/uid_core.c`) with the review's
//! additions: the three launch gates (D14), `hold`/`release` for ship (§5 item 4) and the empty-screen
//! self-check (§5 item 2). No I/O here: [`Screen::step`] turns what the caller observed into
//! [`Act`]s, and `run.rs` / the tests' fake world carry them out.
//!
//! Why the counting is shaped the way it is (unchanged from u60-uid): on this firmware a UI that dies
//! again and again gets escalated into a whole-device reboot (2026-09-16), and that reboot can come
//! before we get to record "it crashed". So the count is of *launch attempts*, written to /data and
//! synced before each launch, and cleared only after the UI has run steadily for [`STABLE_MS`]. When a
//! launch would be attempt number [`MAX_ATTEMPTS`] + 1 we stop and give the screen to the vendor UI;
//! only a corner long-press or `echo devui > /tmp/luyaz-screen.ctl` resets it.
//!
//! Never leave the panel without a UI: on 2026-09-23 the device rebooted itself (reboot_reason_code
//! 1185) a few minutes after our UI was stopped with nothing in its place. Every path that stops one
//! UI starts the other in the same step; the empty-screen check is the backstop.
//!
//! The log lines are u60-uid's, word for word (D13: ship's `UID_BAD`, the ledger's `ledger_uid` and
//! doctor read them); `tests.rs` pins the ones they match.

/// Unsteady launches before giving up.
pub const MAX_ATTEMPTS: u32 = 2;
pub const STABLE_MS: i64 = 10 * 60 * 1000;
/// Never two launches closer than this.
pub const LAUNCH_GAP_MS: i64 = 3000;
/// Don't hammer the vendor init script.
pub const VENDOR_RETRY_MS: i64 = 30_000;
/// How long a launch waits for /dev/dri/card0 to be let go of (and the vendor UI to be gone).
pub const DRM_WAIT_MS: i64 = 5000;
/// After a launch: the vendor UI showing up within this long means two UIs (D14 gate 3).
pub const COEXIST_MS: i64 = 5000;
/// After a launch given up at the gates: wait this long before stopping the vendor UI again, so a
/// card0 that stays busy does not flap the vendor UI every few seconds.
pub const ABANDON_GAP_MS: i64 = 60_000;
/// Neither UI up this long (and nothing of ours in the way): start the vendor UI (§5 item 2).
pub const EMPTY_MS: i64 = 20_000;
pub const HOLD_DEFAULT_S: i64 = 600;
pub const HOLD_MAX_S: i64 = 3600;
pub const CORNER_BOX: i32 = 64;
pub const CORNER_HOLD_MS: i64 = 3000;
pub const SCREEN_W: i32 = 320;
pub const SCREEN_H: i32 = 480;
/// Longest the screen thread sleeps: the heartbeat is written at least this often.
pub const HEARTBEAT_MS: i64 = 60_000;
/// While our UI is not on screen (vendor's, or none): look at /proc this often, for a UI started by
/// hand and for the vendor UI going away (the only polling left; u60-uid did it every second).
pub const OFF_SCREEN_SCAN_MS: i64 = 5000;

pub const CTL_FIFO: &str = "/tmp/luyaz-screen.ctl";
/// Every FIFO the sentinel listens on: only the new one since Phase 10 R4 round ③ (the old
/// /tmp/u60-uid.ctl went with the last touch UI that wrote it).
pub const CTL_FIFOS: [&str; 1] = [CTL_FIFO];

/// Persisted state (`/data/luyaz/screen/{attempts,gave-up}`, `/tmp/luyaz/screen.want`), read again before
/// every step: the files are the truth (SSH may change them, and shadow mode never writes them).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Persist {
    /// Our UI should own the screen; false = the owner chose the vendor UI.
    pub want_devui: bool,
    /// Stopped relaunching after repeated failures.
    pub gave_up: bool,
    /// Launches not yet confirmed steady.
    pub attempts: u32,
}

/// The one decision per step (u60-uid's `uid_decide`, unchanged).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Decision {
    Nothing,
    /// Bump attempts (synced), stop the vendor UI, start ours.
    Launch,
    /// Ours has been up [`STABLE_MS`]: attempts = 0.
    MarkSteady,
    /// Set gave-up, alert, hand the screen to the vendor UI.
    GiveUp,
    /// Ours is not wanted/allowed and the vendor UI is not up.
    StartVendor,
    /// The owner chose the vendor UI but ours is still up.
    StopDevui,
}

pub fn decide(s: &Persist, devui_alive: bool, alive_ms: i64, vendor_alive: bool) -> Decision {
    if !s.want_devui {
        if devui_alive {
            return Decision::StopDevui;
        }
        return if vendor_alive {
            Decision::Nothing
        } else {
            Decision::StartVendor
        };
    }
    if s.gave_up {
        // A devui that is up anyway was started by hand: leave it be. Only the corner gesture or SSH
        // clears gave_up and resumes launching.
        if devui_alive {
            return Decision::Nothing;
        }
        return if vendor_alive {
            Decision::Nothing
        } else {
            Decision::StartVendor
        };
    }
    if devui_alive {
        return if s.attempts > 0 && alive_ms >= STABLE_MS {
            Decision::MarkSteady
        } else {
            Decision::Nothing
        };
    }
    if s.attempts >= MAX_ATTEMPTS {
        return Decision::GiveUp;
    }
    Decision::Launch
}

/// How a devui exit counts. `status` is the raw waitpid status, `None` when unknown (an adopted
/// process is not our child). Attempts are not changed here: the launch already counted.
pub fn exit_is_failure(requested: bool, status: Option<i32>) -> bool {
    if requested {
        return false;
    }
    match status {
        None => false,
        Some(st) if libc::WIFEXITED(st) => libc::WEXITSTATUS(st) != 0,
        Some(_) => true, // killed by a signal
    }
}

/// "exit 1" / "killed by signal 11", as u60-uid words it.
pub fn exit_words(status: i32) -> String {
    if libc::WIFEXITED(status) {
        format!("exit {}", libc::WEXITSTATUS(status))
    } else {
        format!("killed by signal {}", libc::WTERMSIG(status))
    }
}

/// The attempts file: missing (`None`) → 0; anything unreadable or non-numeric → [`MAX_ATTEMPTS`]
/// (when in doubt, do not relaunch). Same rules as `uid_parse_attempts` (strtol, blanks around).
pub fn parse_attempts(text: Option<&str>) -> u32 {
    let Some(t) = text else { return 0 };
    let t = t
        .trim_start_matches([' ', '\t'])
        .trim_end_matches([' ', '\t', '\n', '\r']);
    let digits = t.strip_prefix('+').unwrap_or(t);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return MAX_ATTEMPTS;
    }
    match digits.parse::<u64>() {
        Ok(v) => v.min(1000) as u32,
        Err(_) => 1000, // all digits but too long: strtol saturates, then the 1000 cap
    }
}

/// Corner long-press recogniser, fed one touch sample at a time.
#[derive(Clone, Copy, Debug, Default)]
pub struct Corner {
    /// A real release has been seen since listening began.
    pub fresh: bool,
    pub armed: bool,
    pub press_start: i64,
}

impl Corner {
    /// True exactly once when the press inside the box has been held for `hold_ms`. A press already
    /// down when listening starts never counts (the device reports one on open with no finger on the
    /// glass).
    pub fn feed(&mut self, in_box: bool, pressed: bool, now_ms: i64, hold_ms: i64) -> bool {
        if !pressed {
            self.fresh = true;
            self.armed = false;
            return false;
        }
        if !self.fresh || !in_box {
            self.armed = false;
            return false;
        }
        if !self.armed {
            self.armed = true;
            self.press_start = now_ms;
            return false;
        }
        if now_ms - self.press_start >= hold_ms {
            // Fire once; a new release is needed before the next one.
            self.armed = false;
            self.fresh = false;
            return true;
        }
        false
    }
}

/// Our UI, by comm prefix: `luyaz-ui`, side-by-side builds `luyaz-ui.<something>` (Phase 10 R5;
/// the old name u60pro-devui is gone since round ③, its program deleted from the device).
pub fn is_devui_comm(comm: &str) -> bool {
    comm.starts_with("luyaz-ui")
}

pub fn in_corner_box(x: i32, y: i32) -> bool {
    x >= SCREEN_W - CORNER_BOX && y >= SCREEN_H - CORNER_BOX
}

/// A command on the control FIFO. `vendor`/`devui` as u60-uid (substring match); `hold [s]` and
/// `release` are new (ship's touch trial, §5 item 4).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cmd {
    Vendor,
    Devui,
    Hold(i64),
    Release,
}

pub fn parse_cmds(text: &str) -> Vec<Cmd> {
    let mut out = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        if l.contains("vendor") {
            out.push(Cmd::Vendor);
        } else if l.contains("devui") {
            out.push(Cmd::Devui);
        } else if let Some(rest) = l.strip_prefix("hold") {
            let secs = rest.trim().parse::<i64>().unwrap_or(HOLD_DEFAULT_S);
            out.push(Cmd::Hold(secs.clamp(10, HOLD_MAX_S)));
        } else if l == "release" {
            out.push(Cmd::Release);
        }
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// Our child: its status is readable.
    Child,
    /// Found running, not our child: status unknown.
    Adopted,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ui {
    pub pid: i32,
    pub kind: Kind,
    /// When it was launched/adopted (an adopted one's real start is unknown: conservative).
    pub since_ms: i64,
    /// We asked it to stop.
    pub requested: bool,
    /// The steady mark was already written for this one.
    pub steady: bool,
}

/// What the screen thread does next.
#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    /// A line for `/tmp/luyaz-screen.log`.
    Log(String),
    Alert(&'static str, String),
    /// attempts file, written and synced before the launch it counts.
    SetAttempts(u32),
    SetGaveUp,
    ClearGaveUp,
    WantVendor,
    ClearWant,
    /// Leave a crash record in /data/crashlog/luyaz-ui (5 kept).
    RecordCrash(String),
    /// `/etc/init.d/zte_topsw_devui stop` + `killall -9`.
    StopVendor,
    /// Show the explanation frame for 3 s if the panel is free (`frame.rs`, C-R8); always followed
    /// by [`Act::StartVendor`].
    Explain,
    /// Waits for card0 (≤ 5 s), then `/etc/init.d/zte_topsw_devui start`.
    StartVendor,
    /// Fork/exec our UI; the caller reports back with [`Screen::spawned`].
    Spawn,
    /// TERM, 5 s, KILL; the caller reports back with [`Screen::stopped`].
    Stop(i32),
    /// Open (true) or close the touch device for the corner long-press.
    Touch(bool),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Launch {
    Idle,
    /// The vendor UI was asked to go; waiting for it and card0 (D14 gates 1–2).
    WaitDrm {
        since_ms: i64,
    },
}

/// What the caller saw since the last step.
#[derive(Clone, Debug, Default)]
pub struct Input {
    pub now_ms: i64,
    pub persist: Persist,
    /// The current UI ended: `Some(status)` for our child, `None` = unknown (adopted).
    pub ui_exit: Option<Option<i32>>,
    /// A process whose comm starts with `luyaz-ui` (`.test` too), from a /proc scan ([`Screen::wants_scan`]).
    pub found_devui: Option<i32>,
    pub vendor_alive: bool,
    /// Only looked at while a launch waits ([`Screen::needs_card0`]).
    pub card0_held: bool,
    pub cmds: Vec<Cmd>,
    pub corner_fired: bool,
    /// A ship transaction of `touch` or the sentinel is under way with a live executor: the
    /// empty-screen check gives way to it (§4 让给 ship).
    pub ship_yield: bool,
}

/// Coarse state for the heartbeat file.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OnScreen {
    Ours,
    Vendor,
    None,
    Held,
}

impl OnScreen {
    pub fn word(self) -> &'static str {
        match self {
            OnScreen::Ours => "ours",
            OnScreen::Vendor => "vendor",
            OnScreen::None => "none",
            OnScreen::Held => "held",
        }
    }
}

#[derive(Debug)]
pub struct Screen {
    pub cur: Option<Ui>,
    launch: Launch,
    /// No launch before this (LAUNCH_GAP after a launch, ABANDON_GAP after a launch given up).
    next_launch_ms: i64,
    last_vendor_ms: i64,
    /// When our launched UI started: the D14 gate 3 window.
    launched_ms: Option<i64>,
    empty_since_ms: Option<i64>,
    hold_until_ms: Option<i64>,
    touch_open: bool,
    pub corner: Corner,
    vendor_alive: bool,
    /// Launches since this sentinel started (the heartbeat adds the count it was started with).
    pub launches: u32,
}

impl Screen {
    /// Start-up: adopt a UI that is already running (C10: never start a second one).
    pub fn new(now_ms: i64, found_devui: Option<i32>) -> (Screen, Vec<Act>) {
        let mut s = Screen {
            cur: None,
            launch: Launch::Idle,
            next_launch_ms: i64::MIN / 2,
            last_vendor_ms: i64::MIN / 2,
            launched_ms: None,
            empty_since_ms: None,
            hold_until_ms: None,
            touch_open: false,
            corner: Corner::default(),
            vendor_alive: false,
            launches: 0,
        };
        let mut acts = Vec::new();
        if let Some(pid) = found_devui {
            s.cur = Some(Ui {
                pid,
                kind: Kind::Adopted,
                since_ms: now_ms,
                requested: false,
                steady: false,
            });
            acts.push(Act::Log(format!("adopted running luyaz-ui pid {pid}")));
        }
        (s, acts)
    }

    pub fn held(&self) -> bool {
        self.hold_until_ms.is_some()
    }

    pub fn on_screen(&self) -> OnScreen {
        if self.held() {
            OnScreen::Held
        } else if self.cur.is_some() {
            OnScreen::Ours
        } else if self.vendor_alive {
            OnScreen::Vendor
        } else {
            OnScreen::None
        }
    }

    pub fn touch_open(&self) -> bool {
        self.touch_open
    }

    /// Whether the caller should scan /proc for a devui before the next step (only when we track none:
    /// a tracked one is watched through its pidfd).
    pub fn wants_scan(&self) -> bool {
        self.cur.is_none()
    }

    pub fn needs_card0(&self) -> bool {
        matches!(self.launch, Launch::WaitDrm { .. })
    }

    /// When the caller should step again at the latest (it also steps on any event).
    pub fn next_wake_ms(&self, now_ms: i64, p: &Persist) -> i64 {
        let mut t = now_ms + HEARTBEAT_MS;
        let mut at = |x: i64| t = t.min(x.max(now_ms));
        if let Launch::WaitDrm { .. } = self.launch {
            at(now_ms + 200);
        }
        if self.corner.armed {
            at(now_ms + 250); // a held finger emits nothing
        }
        if let Some(h) = self.hold_until_ms {
            at(h);
            return t;
        }
        match &self.cur {
            Some(ui) => {
                if p.attempts > 0 && !ui.steady && ui.since_ms + STABLE_MS > now_ms {
                    at(ui.since_ms + STABLE_MS);
                }
                if let Some(l) = self.launched_ms
                    && now_ms - l < COEXIST_MS
                {
                    at(now_ms + 1000);
                }
            }
            None => {
                at(now_ms + OFF_SCREEN_SCAN_MS);
                if self.next_launch_ms > now_ms {
                    at(self.next_launch_ms);
                }
            }
        }
        t
    }

    /// Our child is running (`Ok(pid)`) or could not be started.
    pub fn spawned(&mut self, now_ms: i64, r: Result<i32, String>, attempts: u32) -> Vec<Act> {
        match r {
            Ok(pid) => {
                self.cur = Some(Ui {
                    pid,
                    kind: Kind::Child,
                    since_ms: now_ms,
                    requested: false,
                    steady: false,
                });
                self.launched_ms = Some(now_ms);
                self.launches += 1;
                vec![Act::Log(format!(
                    "launched luyaz-ui pid {pid} (attempt {attempts} of {MAX_ATTEMPTS} before giving up)"
                ))]
            }
            Err(e) => vec![Act::Log(format!("fork failed: {e}"))],
        }
    }

    /// The UI we asked to stop has ended (`status` as for [`Input::ui_exit`]).
    pub fn stopped(&mut self, status: Option<Option<i32>>) -> Vec<Act> {
        match status {
            Some(st) => self.ended(st),
            None => Vec::new(),
        }
    }

    fn ended(&mut self, status: Option<i32>) -> Vec<Act> {
        let Some(ui) = self.cur.take() else {
            return Vec::new();
        };
        self.launched_ms = None;
        let mut acts = Vec::new();
        match (ui.kind, status) {
            (Kind::Child, Some(st)) => {
                let why = exit_words(st);
                let req = if ui.requested { " (requested)" } else { "" };
                acts.push(Act::Log(format!(
                    "luyaz-ui pid {} ended: {why}{req}",
                    ui.pid
                )));
                if exit_is_failure(ui.requested, Some(st)) {
                    acts.push(Act::RecordCrash(why.clone()));
                    acts.push(Act::Alert("devui-crash", format!("luyaz-ui {why}")));
                }
            }
            (Kind::Child, None) => {
                acts.push(Act::Log(format!(
                    "lost track of luyaz-ui pid {} (already reaped?)",
                    ui.pid
                )));
            }
            (Kind::Adopted, _) => {
                acts.push(Act::Log(format!(
                    "adopted luyaz-ui pid {} is gone (exit status unknown, not counted)",
                    ui.pid
                )));
            }
        }
        acts
    }

    fn start_vendor(&mut self, now_ms: i64, acts: &mut Vec<Act>) {
        self.last_vendor_ms = now_ms;
        self.empty_since_ms = None;
        acts.push(Act::StartVendor);
    }

    pub fn step(&mut self, inp: &Input) -> Vec<Act> {
        let now = inp.now_ms;
        let mut p = inp.persist;
        let mut acts = Vec::new();
        self.vendor_alive = inp.vendor_alive;

        if let Some(st) = inp.ui_exit {
            acts.extend(self.ended(st));
        }

        for c in &inp.cmds {
            match *c {
                Cmd::Vendor => {
                    acts.push(Act::Log("request: vendor UI".into()));
                    acts.push(Act::WantVendor);
                    p.want_devui = false;
                }
                Cmd::Devui => {
                    acts.push(Act::Log(
                        "request: our UI (clears give-up and the attempt count)".into(),
                    ));
                    acts.extend([Act::ClearWant, Act::ClearGaveUp, Act::SetAttempts(0)]);
                    p = Persist {
                        want_devui: true,
                        gave_up: false,
                        attempts: 0,
                    };
                }
                Cmd::Hold(secs) => {
                    let what = match self.cur.take() {
                        Some(ui) => format!("; not watching luyaz-ui pid {}", ui.pid),
                        None => String::new(),
                    };
                    acts.push(Act::Log(format!(
                        "request: hold the screen for {secs} s (ship){what}"
                    )));
                    self.hold_until_ms = Some(now + secs * 1000);
                    self.launch = Launch::Idle;
                    self.launched_ms = None;
                    self.empty_since_ms = None;
                }
                Cmd::Release => {
                    if self.hold_until_ms.take().is_some() {
                        acts.push(Act::Log("request: release the screen".into()));
                    }
                }
            }
        }

        if let Some(until) = self.hold_until_ms
            && now >= until
        {
            self.hold_until_ms = None;
            acts.push(Act::Log(
                "screen hold expired: taking the screen back".into(),
            ));
            acts.push(Act::Alert(
                "screen-hold-expired",
                "screen held for ship past its deadline; sentinel took it back".into(),
            ));
        }

        if self.held() {
            if self.touch_open {
                self.touch_open = false;
                acts.push(Act::Touch(false));
            }
            return acts;
        }

        if inp.corner_fired {
            acts.push(Act::Log(
                "corner long-press: back to our UI (give-up and attempts cleared)".into(),
            ));
            acts.extend([Act::ClearWant, Act::ClearGaveUp, Act::SetAttempts(0)]);
            p = Persist {
                want_devui: true,
                gave_up: false,
                attempts: 0,
            };
            self.corner = Corner::default();
        }

        if self.cur.is_none()
            && let Some(pid) = inp.found_devui
        {
            // started by hand, or by ship
            self.cur = Some(Ui {
                pid,
                kind: Kind::Adopted,
                since_ms: now,
                requested: false,
                steady: false,
            });
            self.launch = Launch::Idle;
            acts.push(Act::Log(format!("adopted luyaz-ui pid {pid}")));
        }

        // D14 gate 3: the vendor UI turned up right after our launch — two owners of card0. Stop ours,
        // keep the vendor's.
        if let (Some(l), Some(ui)) = (self.launched_ms, self.cur) {
            if now - l >= COEXIST_MS {
                self.launched_ms = None;
            } else if inp.vendor_alive && ui.kind == Kind::Child {
                self.launched_ms = None;
                acts.push(Act::Log(format!(
                    "vendor UI came up within {} s of our launch: stopping luyaz-ui pid {} and keeping the vendor UI",
                    COEXIST_MS / 1000,
                    ui.pid
                )));
                acts.push(Act::Alert(
                    "screen-two-uis",
                    "vendor UI started next to ours; ours stopped".into(),
                ));
                if let Some(u) = self.cur.as_mut() {
                    u.requested = true;
                }
                acts.push(Act::Stop(ui.pid));
                self.last_vendor_ms = now;
                self.next_launch_ms = now + ABANDON_GAP_MS;
                return acts;
            }
        }

        let alive = self.cur.is_some();
        let alive_ms = self.cur.map(|u| now - u.since_ms).unwrap_or(0);
        let mut d = decide(&p, alive, alive_ms, inp.vendor_alive);
        if d == Decision::MarkSteady && self.cur.map(|u| u.steady).unwrap_or(false) {
            d = Decision::Nothing;
        }
        if d != Decision::Launch {
            self.launch = Launch::Idle;
        }
        match d {
            Decision::Launch => self.launch_step(now, &p, inp, &mut acts),
            Decision::MarkSteady => {
                acts.push(Act::Log(format!(
                    "luyaz-ui steady for {} min: attempt count cleared",
                    STABLE_MS / 60000
                )));
                acts.push(Act::SetAttempts(0));
                if let Some(u) = self.cur.as_mut() {
                    u.steady = true;
                }
            }
            Decision::GiveUp => {
                let text = format!(
                    "{} launches did not stay up; vendor UI on screen",
                    p.attempts
                );
                acts.push(Act::Log(format!(
                    "giving up: {text}. Corner long-press or 'echo devui > {CTL_FIFO}' to retry"
                )));
                acts.push(Act::SetGaveUp);
                acts.push(Act::Alert("devui-gave-up", text));
                acts.push(Act::Explain);
                self.start_vendor(now, &mut acts);
            }
            Decision::StartVendor => {
                if now - self.last_vendor_ms >= VENDOR_RETRY_MS {
                    self.start_vendor(now, &mut acts);
                }
            }
            Decision::StopDevui => {
                if let Some(u) = self.cur.as_mut() {
                    u.requested = true;
                    acts.push(Act::Log(format!(
                        "stopping luyaz-ui pid {} (requested)",
                        u.pid
                    )));
                    acts.push(Act::Stop(u.pid));
                }
                self.start_vendor(now, &mut acts);
            }
            Decision::Nothing => {}
        }

        // Backstop: nothing on the panel for EMPTY_MS (outside a launch's own wait and outside a ship
        // transaction, whose executor judges the screen itself).
        let empty = self.cur.is_none()
            && !inp.vendor_alive
            && self.launch == Launch::Idle
            && !inp.ship_yield;
        let starting = acts
            .iter()
            .any(|a| matches!(a, Act::Spawn | Act::StartVendor));
        if empty && !starting {
            let since = *self.empty_since_ms.get_or_insert(now);
            if now - since >= EMPTY_MS && now - self.last_vendor_ms >= EMPTY_MS {
                acts.push(Act::Log(format!(
                    "no UI on screen for {} s: starting the vendor UI",
                    (now - since) / 1000
                )));
                self.start_vendor(now, &mut acts);
            }
        } else if !empty {
            self.empty_since_ms = None;
        }

        // Listen for the corner only while our UI is not on screen.
        let listen = self.cur.is_none() && (!p.want_devui || p.gave_up);
        if listen != self.touch_open {
            self.touch_open = listen;
            if listen {
                self.corner = Corner::default();
            }
            acts.push(Act::Touch(listen));
        }
        acts
    }

    fn launch_step(&mut self, now: i64, p: &Persist, inp: &Input, acts: &mut Vec<Act>) {
        match self.launch {
            Launch::Idle => {
                if now < self.next_launch_ms {
                    return;
                }
                // Never straight to exec: the next step looks at card0 and /proc afresh (D14 gate 1;
                // card0 is only looked at while a launch waits, and a UI may have turned up meanwhile).
                self.next_launch_ms = now + LAUNCH_GAP_MS;
                if inp.vendor_alive {
                    acts.push(Act::StopVendor);
                }
                self.launch = Launch::WaitDrm { since_ms: now };
            }
            Launch::WaitDrm { since_ms } => {
                if !inp.vendor_alive && !inp.card0_held {
                    self.launch = Launch::Idle;
                    self.next_launch_ms = now + LAUNCH_GAP_MS;
                    self.launch_now(p, acts);
                } else if now - since_ms >= DRM_WAIT_MS {
                    // D14: no more "going ahead" with card0 busy. Give the panel back to the vendor UI
                    // in the same step (it was stopped for this launch), and do not count the launch.
                    self.launch = Launch::Idle;
                    self.next_launch_ms = now + ABANDON_GAP_MS;
                    let what = if inp.vendor_alive {
                        "the vendor UI still running"
                    } else {
                        "/dev/dri/card0 still held"
                    };
                    acts.push(Act::Log(format!(
                        "{what} after {DRM_WAIT_MS} ms: not launching this time (not counted)"
                    )));
                    if !inp.vendor_alive {
                        self.start_vendor(now, acts);
                    }
                }
            }
        }
    }

    fn launch_now(&mut self, p: &Persist, acts: &mut Vec<Act>) {
        // Count first, synced: if this launch takes the device down, it still counts.
        acts.push(Act::SetAttempts(p.attempts + 1));
        acts.push(Act::Spawn);
    }
}
