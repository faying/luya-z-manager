//! Fake-device tests (D10). Sections:
//!  1. u60-uid's unit tests (touch-ui `tests/uid_core_test.c`), every check carried over as it was;
//!  2. the screen against a fake world: C10, D14, hold, the empty-screen check, log wording;
//!  3. the worker against a fake device: the u60-guard cases (`scripts/test/u60-guard/run.sh`) for
//!     G1/G2/G3/G4/G7, plus dropbear and the core verdict;
//!  4. the Wi-Fi restore (D3);
//!  5. shadow mode changes nothing.

use std::collections::HashMap;

use crate::screen::*;
use crate::wifi::{self, Core, WifiOps};
use crate::worker::*;

// ── 1. u60-uid's unit tests ─────────────────────────────────────────────────

fn d(
    want: bool,
    gave_up: bool,
    attempts: u32,
    alive: bool,
    alive_ms: i64,
    vendor: bool,
) -> Decision {
    decide(
        &Persist {
            want_devui: want,
            gave_up,
            attempts,
        },
        alive,
        alive_ms,
        vendor,
    )
}
fn w_exit(code: i32) -> i32 {
    (code & 0xff) << 8
}
fn w_sig(sig: i32) -> i32 {
    sig & 0x7f
}

#[test]
fn uid_launching_and_giving_up() {
    assert_eq!(
        d(true, false, 0, false, 0, true),
        Decision::Launch,
        "wanted, not running, no attempts yet: launch"
    );
    assert_eq!(
        d(true, false, 1, false, 0, true),
        Decision::Launch,
        "one unsteady attempt so far: launch again"
    );
    assert_eq!(
        d(true, false, 2, false, 0, true),
        Decision::GiveUp,
        "two unsteady attempts: give up (never a third)"
    );
    assert_eq!(
        d(true, false, 7, false, 0, false),
        Decision::GiveUp,
        "more than the limit: give up"
    );
}

#[test]
fn uid_steady_running() {
    assert_eq!(
        d(true, false, 1, true, STABLE_MS - 1, false),
        Decision::Nothing,
        "running under 10 min: nothing yet"
    );
    assert_eq!(
        d(true, false, 1, true, STABLE_MS, false),
        Decision::MarkSteady,
        "running 10 min with attempts pending"
    );
    assert_eq!(
        d(true, false, 0, true, STABLE_MS * 3, false),
        Decision::Nothing,
        "already steady: nothing to write"
    );
}

#[test]
fn uid_after_giving_up() {
    assert_eq!(
        d(true, true, 2, false, 0, false),
        Decision::StartVendor,
        "gave up, vendor down: start vendor"
    );
    assert_eq!(
        d(true, true, 2, false, 0, true),
        Decision::Nothing,
        "gave up, vendor up: leave it"
    );
    assert_eq!(
        d(true, true, 2, true, 5000, false),
        Decision::Nothing,
        "gave up but devui started by hand: leave it"
    );
}

#[test]
fn uid_owner_chose_vendor() {
    assert_eq!(
        d(false, false, 0, true, 99999, false),
        Decision::StopDevui,
        "devui still up: stop it"
    );
    assert_eq!(
        d(false, false, 0, false, 0, false),
        Decision::StartVendor,
        "devui gone, vendor down: start vendor"
    );
    assert_eq!(
        d(false, false, 0, false, 0, true),
        Decision::Nothing,
        "devui gone, vendor up: nothing"
    );
    assert_ne!(
        d(false, false, 0, false, 0, true),
        Decision::Launch,
        "vendor chosen never relaunches devui"
    );
}

#[test]
fn uid_how_exits_count() {
    assert!(
        !exit_is_failure(true, Some(w_sig(libc::SIGKILL))),
        "requested stop is not a failure even if killed"
    );
    assert!(
        !exit_is_failure(false, None),
        "adopted process (unknown status) is not a failure"
    );
    assert!(
        !exit_is_failure(false, Some(w_exit(0))),
        "clean exit 0 is not a failure"
    );
    assert!(
        exit_is_failure(false, Some(w_exit(1))),
        "exit 1 is a failure"
    );
    assert!(
        exit_is_failure(false, Some(w_sig(libc::SIGSEGV))),
        "SIGSEGV is a failure"
    );
    assert!(
        exit_is_failure(false, Some(w_sig(libc::SIGTERM))),
        "unrequested SIGTERM is a failure"
    );
    assert_eq!(exit_words(w_exit(1)), "exit 1");
    assert_eq!(exit_words(w_sig(11)), "killed by signal 11");
}

#[test]
fn uid_attempts_file() {
    assert_eq!(parse_attempts(None), 0, "missing: 0");
    assert_eq!(parse_attempts(Some("1\n")), 1);
    assert_eq!(
        parse_attempts(Some("")),
        MAX_ATTEMPTS,
        "empty file: treat as max (stop)"
    );
    assert_eq!(parse_attempts(Some("x1")), MAX_ATTEMPTS, "garbage: max");
    assert_eq!(
        parse_attempts(Some("1x")),
        MAX_ATTEMPTS,
        "trailing junk: max"
    );
    assert_eq!(parse_attempts(Some("-3")), MAX_ATTEMPTS, "negative: max");
    assert_eq!(
        parse_attempts(Some(" 2 \r\n")),
        2,
        "blanks around: as strtol"
    );
    assert_eq!(parse_attempts(Some("99999")), 1000, "capped at 1000");
}

#[test]
fn uid_corner_long_press() {
    let mut c = Corner::default();
    assert!(
        !c.feed(true, true, 0, 3000) && !c.feed(true, true, 5000, 3000),
        "press already down when listening starts: ignored"
    );
    assert!(!c.feed(true, false, 5100, 3000), "release arms it");
    assert!(
        !c.feed(true, true, 6000, 3000),
        "press in the box starts timing"
    );
    assert!(!c.feed(true, true, 8900, 3000), "not yet at 2.9 s");
    assert!(c.feed(true, true, 9000, 3000), "fires at 3 s");
    assert!(
        !c.feed(true, true, 15000, 3000),
        "does not fire again while still held"
    );

    let mut c = Corner {
        fresh: true,
        armed: false,
        press_start: 0,
    };
    c.feed(true, true, 0, 3000);
    c.feed(false, true, 1500, 3000); // slid out of the box
    assert!(
        !c.feed(true, true, 3200, 3000),
        "sliding out of the box resets the timer"
    );
    assert!(c.feed(true, true, 6200, 3000), "…and times from re-entry");
}

#[test]
fn corner_box_and_comm_prefix() {
    assert!(in_corner_box(319, 479) && in_corner_box(256, 416));
    assert!(!in_corner_box(255, 479) && !in_corner_box(319, 415));
    assert!(!is_devui_comm("zte_topsw_devui") && !is_devui_comm("luyaz-sentinel"));
    // Phase 10 R5: luyaz-ui and its side-by-side build; the old name no longer (round ③)
    assert!(is_devui_comm("luyaz-ui") && is_devui_comm("luyaz-ui.test") && is_devui_comm("luyaz-ui.te"));
    assert!(!is_devui_comm("u60pro-devui") && !is_devui_comm("u60pro-devui.te"));
    assert!(!is_devui_comm("luyaz-core") && !is_devui_comm("luyaz-web") && !is_devui_comm("luyaz-u"));
}

#[test]
fn both_fifos_listened_to() {
    // R4 round ③: the new FIFO only, also in the give-up hint
    assert_eq!(CTL_FIFOS, ["/tmp/luyaz-screen.ctl"]);
    assert_eq!(CTL_FIFO, "/tmp/luyaz-screen.ctl");
}

#[cfg(target_os = "linux")]
#[test]
fn ui_path_new_when_there_else_old() {
    use crate::run::*;
    assert_eq!(pick_ui(true), ("/data/luyaz/ui/luyaz-ui", "/data/luyaz/ui"));
    assert_eq!(pick_ui(false), ("/data/luyaz/ui/luyaz-ui", "/data/luyaz/ui"), "round ③: never the old one");
}

#[test]
fn ctl_commands() {
    assert_eq!(parse_cmds("vendor\n"), vec![Cmd::Vendor]);
    assert_eq!(parse_cmds("devui\n"), vec![Cmd::Devui]);
    assert_eq!(parse_cmds("hold\n"), vec![Cmd::Hold(HOLD_DEFAULT_S)]);
    assert_eq!(
        parse_cmds("hold 120\nrelease\n"),
        vec![Cmd::Hold(120), Cmd::Release]
    );
    assert_eq!(parse_cmds("hold 999999\n"), vec![Cmd::Hold(HOLD_MAX_S)]);
    assert!(parse_cmds("bogus\n").is_empty());
}

// ── 2. the screen against a fake world ──────────────────────────────────────

/// A fake panel: our UI (one pid or none), the vendor UI, card0. Applies the acts the way the device
/// would and checks the invariant after every step: never two UIs at once.
struct World {
    now: i64,
    p: Persist,
    devui: Option<i32>,
    /// The current devui is our child (status readable).
    child: bool,
    vendor: bool,
    /// card0 held by something other than the two UIs (for D14).
    card0_stuck: bool,
    next_pid: i32,
    ship_yield: bool,
    exit_status: Option<i32>,
    alerts: Vec<String>,
    logs: Vec<String>,
    spawns: u32,
    vendor_starts: u32,
    explains: u32,
    vendor_stops: u32,
    /// The vendor UI is started by someone else this long after it is asked (vendor init being slow).
    two_uis: u32,
    empty_since: Option<i64>,
    longest_empty: i64,
}

impl World {
    fn new() -> World {
        World {
            now: 1_000_000,
            p: Persist {
                want_devui: true,
                gave_up: false,
                attempts: 0,
            },
            devui: None,
            child: false,
            vendor: false,
            card0_stuck: false,
            next_pid: 100,
            ship_yield: false,
            exit_status: None,
            alerts: vec![],
            logs: vec![],
            spawns: 0,
            vendor_starts: 0,
            explains: 0,
            vendor_stops: 0,
            two_uis: 0,
            empty_since: None,
            longest_empty: 0,
        }
    }

    fn card0(&self) -> bool {
        self.card0_stuck || self.devui.is_some() || self.vendor
    }

    fn apply(&mut self, s: &mut Screen, acts: Vec<Act>) {
        let mut q: std::collections::VecDeque<Act> = acts.into();
        while let Some(a) = q.pop_front() {
            match a {
                Act::Log(m) => self.logs.push(m),
                Act::Alert(k, _) => self.alerts.push(k.to_string()),
                Act::SetAttempts(n) => self.p.attempts = n,
                Act::SetGaveUp => self.p.gave_up = true,
                Act::ClearGaveUp => self.p.gave_up = false,
                Act::WantVendor => self.p.want_devui = false,
                Act::ClearWant => self.p.want_devui = true,
                Act::RecordCrash(_) => {}
                Act::Explain => {
                    assert!(!self.vendor, "the frame only before the vendor UI");
                    self.explains += 1;
                }
                Act::StopVendor => {
                    self.vendor_stops += 1;
                    self.vendor = false;
                }
                Act::StartVendor => {
                    self.vendor_starts += 1;
                    self.vendor = true;
                }
                Act::Spawn => {
                    assert!(
                        !self.vendor && !self.card0(),
                        "spawned with the vendor UI up or card0 held"
                    );
                    self.spawns += 1;
                    self.next_pid += 1;
                    self.devui = Some(self.next_pid);
                    self.child = true;
                    let n = self.p.attempts;
                    q.extend(s.spawned(self.now, Ok(self.next_pid), n));
                }
                Act::Stop(pid) => {
                    if self.devui == Some(pid) {
                        self.devui = None;
                        let st = if self.child {
                            Some(Some(w_sig(libc::SIGTERM)))
                        } else {
                            Some(None)
                        };
                        q.extend(s.stopped(st));
                    }
                }
                Act::Touch(_) => {}
            }
        }
        if self.devui.is_some() && self.vendor {
            self.two_uis += 1;
        }
        if self.devui.is_none() && !self.vendor {
            let since = *self.empty_since.get_or_insert(self.now);
            self.longest_empty = self.longest_empty.max(self.now - since);
        } else {
            self.empty_since = None;
        }
    }

    fn step_with(&mut self, s: &mut Screen, cmds: Vec<Cmd>, corner: bool) {
        let ui_exit = match s.cur {
            Some(ui) if self.devui != Some(ui.pid) => Some(if ui.kind == Kind::Child {
                self.exit_status.take()
            } else {
                None
            }),
            _ => None,
        };
        let inp = Input {
            now_ms: self.now,
            persist: self.p,
            ui_exit,
            found_devui: if s.wants_scan() { self.devui } else { None },
            vendor_alive: self.vendor,
            card0_held: s.needs_card0() && self.card0(),
            cmds,
            corner_fired: corner,
            ship_yield: self.ship_yield,
        };
        let acts = s.step(&inp);
        self.apply(s, acts);
    }

    fn step(&mut self, s: &mut Screen) {
        self.step_with(s, vec![], false);
    }

    /// Let `ms` pass, stepping whenever the screen asks to be woken (and at least every second).
    fn run(&mut self, s: &mut Screen, ms: i64) {
        let end = self.now + ms;
        while self.now < end {
            let next = s
                .next_wake_ms(self.now, &self.p)
                .min(self.now + 1000)
                .max(self.now + 1);
            self.now = next.min(end);
            self.step(s);
        }
    }

    /// Our UI dies by itself.
    fn crash(&mut self, status: i32) {
        self.devui = None;
        self.exit_status = Some(status);
    }

    fn start(&mut self) -> Screen {
        let (mut s, acts) = Screen::new(self.now, self.devui);
        if self.devui.is_some() {
            self.child = false;
        }
        self.apply(&mut s, acts);
        s
    }
}

#[test]
fn cold_start_launches_ours() {
    let mut w = World::new();
    w.vendor = true; // the firmware's UI is up at boot
    let mut s = w.start();
    w.run(&mut s, 10_000);
    assert_eq!(w.spawns, 1);
    assert_eq!(w.p.attempts, 1, "counted before the launch");
    assert!(w.devui.is_some() && !w.vendor);
    assert_eq!(w.two_uis, 0);
    assert!(
        w.logs
            .iter()
            .any(|l| l.starts_with("launched u60pro-devui pid ")
                && l.ends_with("(attempt 1 of 2 before giving up)"))
    );
    w.run(&mut s, STABLE_MS + 2000);
    assert_eq!(w.p.attempts, 0, "steady after 10 min: cleared");
    assert_eq!(
        w.logs
            .iter()
            .filter(|l| l.contains("steady for 10 min"))
            .count(),
        1,
        "steady written once"
    );
}

#[test]
fn c10_restart_ten_times_one_ui() {
    // Phase 5 acceptance: the sentinel restarted 10 times over a running UI adopts it every time.
    let mut w = World::new();
    w.devui = Some(4242);
    for i in 0..10 {
        let mut s = w.start();
        assert_eq!(s.cur.map(|u| u.pid), Some(4242), "restart {i}: adopted");
        w.run(&mut s, 5000);
        assert_eq!(w.spawns, 0, "restart {i}: never a second UI");
        assert_eq!(w.vendor_starts + w.vendor_stops, 0);
    }
    assert_eq!(w.two_uis, 0);
    assert_eq!(
        w.logs
            .iter()
            .filter(|l| *l == "adopted running u60pro-devui pid 4242")
            .count(),
        10
    );
}

#[test]
fn crash_twice_gives_up_to_vendor_once() {
    let mut w = World::new();
    let mut s = w.start();
    w.run(&mut s, 2000);
    w.crash(w_sig(libc::SIGSEGV));
    w.run(&mut s, 5000);
    assert_eq!(w.spawns, 2, "relaunched once");
    assert_eq!(w.alerts, vec!["devui-crash"]);
    w.crash(w_exit(1));
    w.run(&mut s, 60_000);
    assert_eq!(w.spawns, 2, "never a third launch");
    assert!(w.p.gave_up && w.vendor);
    assert_eq!(
        w.explains, 1,
        "the explanation frame once, before the vendor UI"
    );
    assert_eq!(
        w.alerts,
        vec!["devui-crash", "devui-crash", "devui-gave-up"]
    );
    assert!(w.logs.iter().any(|l| l == "giving up: 2 launches did not stay up; vendor UI on screen. Corner long-press or 'echo devui > /tmp/luyaz-screen.ctl' to retry"));
    assert!(
        s.touch_open(),
        "corner listener on while the vendor UI is up"
    );
    assert_eq!(w.two_uis, 0);
    // corner long-press brings ours back and clears everything
    w.step_with(&mut s, vec![], true);
    w.run(&mut s, 10_000);
    assert!(!w.p.gave_up && w.devui.is_some() && !w.vendor && w.spawns == 3);
    assert_eq!(w.p.attempts, 1);
}

#[test]
fn log_wording_is_u60_uids() {
    // ship's UID_BAD and the ledger's ledger_uid match these words (D13): keep them byte for byte.
    let mut w = World::new();
    let mut s = w.start();
    w.run(&mut s, 2000);
    let pid = w.devui.unwrap();
    w.crash(w_sig(11));
    w.run(&mut s, 5000);
    assert!(w.logs.contains(&format!(
        "u60pro-devui pid {pid} ended: killed by signal 11"
    )));
    w.step_with(&mut s, vec![Cmd::Vendor], false);
    w.run(&mut s, 7000);
    assert!(w.logs.contains(&"request: vendor UI".to_string()));
    assert!(
        w.logs
            .iter()
            .any(|l| l.starts_with("stopping u60pro-devui pid ") && l.ends_with("(requested)"))
    );
    assert!(
        w.logs
            .iter()
            .any(|l| l.ends_with("ended: killed by signal 15 (requested)")),
        "requested stops say so (UID_BAD skips them)"
    );
    w.step_with(&mut s, vec![Cmd::Devui], false);
    assert!(
        w.logs
            .contains(&"request: our UI (clears give-up and the attempt count)".to_string())
    );
    let bad = [
        "giving up:",
        "starting the vendor UI",
        "vendor UI on screen",
        "ended:",
    ];
    for l in &w.logs {
        if l.contains("launched u60pro-devui") {
            assert!(
                !bad.iter().any(|b| l.contains(b)),
                "a normal launch line must not look bad: {l}"
            );
        }
    }
}

#[test]
fn d14_card0_stuck_gives_the_panel_back_and_does_not_flap() {
    let mut w = World::new();
    w.vendor = true;
    w.card0_stuck = true;
    let mut s = w.start();
    w.run(&mut s, 5 * 60_000);
    assert_eq!(w.spawns, 0, "never launched over a held card0");
    assert_eq!(
        w.p.attempts, 0,
        "launches given up at the gates are not counted"
    );
    assert!(w.vendor, "the vendor UI is back");
    assert!(
        w.longest_empty <= DRM_WAIT_MS + 1000,
        "screen empty {} ms",
        w.longest_empty
    );
    assert!(
        w.vendor_stops <= 6,
        "vendor UI stopped {} times in 5 min (once a minute at most)",
        w.vendor_stops
    );
    assert!(
        w.logs
            .iter()
            .any(|l| l.contains("not launching this time (not counted)"))
    );
    // card0 frees up: next try launches
    w.card0_stuck = false;
    w.run(&mut s, ABANDON_GAP_MS + 2000);
    assert_eq!(w.spawns, 1);
    assert_eq!(w.two_uis, 0);
}

#[test]
fn d14_vendor_turns_up_right_after_our_launch() {
    let mut w = World::new();
    let mut s = w.start();
    w.run(&mut s, 1500);
    assert_eq!(w.spawns, 1);
    w.vendor = true; // someone (the core's rescue, SSH) started the vendor UI too
    w.step(&mut s);
    assert!(
        w.devui.is_none() && w.vendor,
        "ours stopped, the vendor's kept"
    );
    assert!(w.alerts.contains(&"screen-two-uis".to_string()));
    assert!(
        !w.alerts.contains(&"devui-crash".to_string()),
        "our requested stop is not a crash"
    );
}

#[test]
fn empty_screen_backstop_and_ship_yield() {
    let mut w = World::new();
    w.p.want_devui = false; // vendor chosen…
    let mut s = w.start();
    w.run(&mut s, 1000);
    assert!(w.vendor);
    let starts = w.vendor_starts;
    w.vendor = false; // …and it dies
    w.ship_yield = true;
    w.run(&mut s, 30_000);
    assert_eq!(w.vendor_starts, starts + 1, "the 30 s retry still runs");
    // a launch blocked by its gap, nothing on screen: the backstop
    let mut w = World::new();
    let mut s = w.start();
    w.run(&mut s, 500);
    w.step_with(&mut s, vec![Cmd::Hold(30)], false);
    w.devui = None; // ship stopped ours
    w.ship_yield = false;
    w.run(&mut s, 25_000);
    assert_eq!(w.vendor_starts, 0, "held: the screen is ship's");
    w.run(&mut s, 10_000);
    assert!(
        w.alerts.contains(&"screen-hold-expired".to_string()),
        "hold ran out"
    );
    assert!(w.devui.is_some(), "taken back: ours launched again");
}

#[test]
fn hold_and_release() {
    let mut w = World::new();
    w.devui = Some(77);
    let mut s = w.start();
    w.step_with(&mut s, vec![Cmd::Hold(600)], false);
    assert_eq!(s.on_screen(), OnScreen::Held);
    w.devui = Some(88); // ship's side-by-side u60pro-devui.test
    w.run(&mut s, 60_000);
    assert!(s.cur.is_none(), "held: not adopted");
    assert_eq!(w.spawns, 0);
    w.step_with(&mut s, vec![Cmd::Release], false);
    w.step(&mut s);
    assert_eq!(
        s.cur.map(|u| u.pid),
        Some(88),
        "released: adopts what is running"
    );
    assert_eq!(w.spawns, 0);
    // default hold runs out after 600 s
    w.step_with(&mut s, vec![Cmd::Hold(HOLD_DEFAULT_S)], false);
    w.run(&mut s, HOLD_DEFAULT_S * 1000 + 2000);
    assert!(!s.held());
    assert_eq!(
        w.alerts
            .iter()
            .filter(|a| *a == "screen-hold-expired")
            .count(),
        1
    );
}

#[test]
fn sleeps_long_when_all_is_well() {
    let mut w = World::new();
    w.devui = Some(5);
    let s = w.start();
    let p = Persist {
        want_devui: true,
        gave_up: false,
        attempts: 0,
    };
    assert_eq!(
        s.next_wake_ms(w.now, &p) - w.now,
        HEARTBEAT_MS,
        "ours up and steady: only the heartbeat"
    );
}

// ── 3. the worker against a fake device ─────────────────────────────────────

#[derive(Default)]
struct Dev {
    up: u64,
    wall: i64,
    mono: i64,
    files: HashMap<String, String>,
    beacon: bool,
    health: Option<Health>,
    restore_ok: bool,
    restores: u32,
    autosleep: Vec<bool>,
    alerts: Vec<String>,
    logs: Vec<String>,
    recover_calls: u32,
    cmdlines: HashMap<i32, String>,
    non_append: Vec<String>,
    capped: Vec<String>,
    dropbear: bool,
    dropbear_starts: u32,
    changes: u32,
    core_replacing: bool,
    sms_rounds: u32,
    guard: bool,
    charging: bool,
}

impl Dev {
    fn new() -> Dev {
        Dev {
            wall: 1_790_000_000,
            dropbear: true,
            ..Default::default()
        }
    }
    fn kinds(&self) -> String {
        self.alerts.join(" ")
    }
    fn hb(&mut self, up: u64) {
        self.files
            .insert("/tmp/scenario.heartbeat".into(), format!("{up}\n"));
    }
}

impl WorkerIo for Dev {
    fn uptime(&mut self) -> u64 {
        self.up
    }
    fn wall(&mut self) -> i64 {
        self.wall
    }
    fn mono_s(&mut self) -> i64 {
        self.mono
    }
    fn read(&mut self, path: &str) -> Option<String> {
        self.files.get(path).cloned()
    }
    fn exists(&mut self, path: &str) -> bool {
        self.files.contains_key(path)
    }
    fn size(&mut self, path: &str) -> Option<u64> {
        self.files.get(path).map(|c| c.len() as u64)
    }
    fn cmdline_has(&mut self, pid: i32, needle: &str) -> bool {
        self.cmdlines
            .get(&pid)
            .map(|c| c.contains(needle))
            .unwrap_or(false)
    }
    fn appenders_only(&mut self, path: &str) -> bool {
        !self.non_append.iter().any(|p| p == path)
    }
    fn beaconing(&mut self) -> bool {
        self.beacon
    }
    fn healthz(&mut self) -> Health {
        self.health.clone().unwrap_or(Health::Ok)
    }
    fn dropbear_listening(&mut self) -> bool {
        self.dropbear
    }
    fn write(&mut self, path: &str, content: &str) {
        if !path.starts_with("/tmp/luyaz-sentinel/") {
            self.changes += 1;
        }
        self.files.insert(path.into(), content.into());
    }
    fn poke(&mut self, path: &str, content: &str) -> bool {
        self.changes += 1;
        self.files.insert(path.into(), content.into());
        true
    }
    fn remove(&mut self, path: &str) {
        self.changes += 1;
        self.files.remove(path);
    }
    fn log(&mut self, msg: &str) {
        self.logs.push(msg.into());
    }
    fn alert(&mut self, kind: &str, _text: &str) {
        self.changes += 1;
        self.alerts.push(kind.into());
    }
    fn restore_wifi(&mut self) -> Result<(), String> {
        self.changes += 1;
        self.restores += 1;
        if self.restore_ok {
            self.beacon = true;
            Ok(())
        } else {
            Err("no hostapd within 45s".into())
        }
    }
    fn set_autosleep(&mut self, on: bool) {
        self.changes += 1;
        self.autosleep.push(on);
    }
    fn recover_live(&mut self) -> bool {
        self.changes += 1;
        self.recover_calls += 1;
        true
    }
    fn cap_log(&mut self, path: &str) -> bool {
        self.changes += 1;
        let c = self.files.get(path).cloned().unwrap_or_default();
        self.files.insert(format!("{path}.old"), c);
        self.files.insert(path.into(), String::new());
        self.capped.push(path.into());
        true
    }
    fn start_dropbear(&mut self) -> bool {
        self.changes += 1;
        self.dropbear_starts += 1;
        true
    }
    fn core_being_replaced(&mut self) -> bool {
        self.core_replacing
    }
    fn alert_sms(&mut self) {
        self.sms_rounds += 1;
    }
    fn guard_running(&mut self) -> bool {
        self.guard
    }
    fn boot_is_normal(&mut self) -> bool {
        !self.charging
    }
}

fn wk() -> Worker {
    Worker::new(Paths::default())
}

fn marker(d: &Dev) -> Option<&String> {
    let m = d.files.get("/tmp/luyaz/wifiguard.took-over");
    assert!(!d.files.contains_key("/tmp/u60-wifiguard.took-over"), "the old name is not written");
    m
}

#[test]
fn guard_grace_and_first_heartbeat() {
    let (mut w, mut d) = (wk(), Dev::new());
    d.up = 100;
    w.round(&mut d);
    assert!(
        d.restores == 0 && marker(&d).is_none(),
        "inside the 5-minute grace: nothing happens"
    );
    d.up = 400;
    w.round(&mut d);
    assert!(
        d.restores == 0 && d.alerts.is_empty(),
        "never seen a heartbeat, 400 s after boot: still waiting"
    );
    d.hb(390);
    w.round(&mut d);
    assert!(
        d.restores == 0 && w.wifi.seen,
        "fresh heartbeat: nothing to do"
    );
}

#[test]
fn guard_engine_never_came_up() {
    let (mut w, mut d) = (wk(), Dev::new());
    d.up = 1000;
    d.restore_ok = true;
    w.round(&mut d);
    assert!(
        marker(&d).is_some() && d.restores == 1,
        "no heartbeat since boot + AP off after 15 min: takes over"
    );
    assert_eq!(d.kinds(), "agent-silent wifi-takeover");
    assert_eq!(d.autosleep, vec![false], "holds sleep off while rescuing");
}

#[test]
fn guard_dies_while_wifi_up() {
    let (mut w, mut d) = (wk(), Dev::new());
    d.up = 2000;
    d.hb(1600);
    d.beacon = true;
    w.round(&mut d);
    w.round(&mut d);
    assert!(
        d.restores == 0 && marker(&d).is_none(),
        "stale but beaconing: no restore"
    );
    assert_eq!(d.kinds(), "agent-silent", "alerted once");
}

/// run.sh 386/393: the vendor switch is read only inside the restore, so "left alone" is "the
/// restore is never asked for".
#[test]
fn guard_vendor_switch_off_but_engine_alive_or_beaconing() {
    let (mut w, mut d) = (wk(), Dev::new());
    d.up = 2000;
    d.hb(1990);
    w.round(&mut d);
    assert!(
        d.restores == 0 && d.alerts.is_empty(),
        "agent alive: a deliberate off is the agent's/user's"
    );
    let (mut w, mut d) = (wk(), Dev::new());
    d.up = 2000;
    d.hb(1600);
    d.beacon = true;
    w.round(&mut d);
    assert!(
        d.restores == 0 && marker(&d).is_none(),
        "agent gone, but something beacons: no rescue"
    );
}

#[test]
fn guard_restore_fails_backs_off_alerts_once() {
    let (mut w, mut d) = (wk(), Dev::new());
    d.up = 2000;
    d.hb(1600);
    w.round(&mut d);
    assert_eq!(d.restores, 1);
    assert_eq!(d.kinds(), "agent-silent wifi-takeover wifi-restore-failed");
    let m1 = marker(&d).cloned();
    let nt = w.wifi.next_try;
    assert_eq!(nt, d.up + 60, "next try 60 s after the failed attempt");
    d.up = nt - 1;
    w.round(&mut d);
    assert_eq!(d.restores, 1, "no attempt before next-try");
    d.up = nt;
    w.round(&mut d);
    assert_eq!(d.restores, 2, "retried at next-try");
    assert_eq!(
        w.wifi.backoff, 240,
        "backoff doubled to 240 for the next wait"
    );
    assert_eq!(
        d.kinds(),
        "agent-silent wifi-takeover wifi-restore-failed",
        "failure not re-alerted"
    );
    assert_eq!(marker(&d).cloned(), m1, "marker written once");
    d.restore_ok = true;
    d.up = w.wifi.next_try;
    w.round(&mut d);
    assert!(d.beacon && w.wifi.next_try == 0, "succeeds eventually");
    let up = d.up;
    d.hb(up);
    w.round(&mut d);
    assert!(
        w.wifi.episode.is_none() && d.autosleep.last() == Some(&true),
        "heartbeat back: episode over, sleep handed back"
    );
    assert!(marker(&d).is_some(), "marker left for the engine to remove");
}

#[test]
fn guard_backoff_caps_at_480() {
    let (mut w, mut d) = (wk(), Dev::new());
    d.up = 2000;
    d.hb(1600);
    for _ in 0..6 {
        if w.wifi.next_try > 0 {
            d.up = w.wifi.next_try;
        }
        w.round(&mut d);
    }
    assert_eq!(w.wifi.backoff, 480);
}

#[test]
fn guard_device_suspended() {
    let (mut w, mut d) = (wk(), Dev::new());
    for (up, hb) in [(1600, Some(1590)), (1660, Some(1650)), (2900, None)] {
        d.up = up;
        if let Some(h) = hb {
            d.hb(h);
        }
        w.round(&mut d);
    }
    assert!(
        d.alerts.is_empty() && d.restores == 0 && marker(&d).is_none(),
        "woke 20 min later: no alert, no action"
    );
    d.up = 2960;
    d.hb(2955);
    w.round(&mut d);
    assert!(
        d.alerts.is_empty() && d.restores == 0,
        "checks in after waking: still nothing"
    );
    for up in [3020, 3080, 3140, 3200, 3260] {
        d.up = up;
        w.round(&mut d);
    }
    assert_eq!(
        d.kinds(),
        "agent-silent wifi-takeover wifi-restore-failed",
        "then silent past STALE: judged normally"
    );

    let (mut w, mut d) = (wk(), Dev::new());
    d.up = 2000;
    d.hb(1600);
    w.round(&mut d);
    assert!(
        w.wifi.episode.is_some() && d.restores == 1,
        "outage already under way before the sleep…"
    );
    d.up = w.wifi.next_try + 1200;
    w.round(&mut d);
    assert_eq!(d.restores, 2, "…keeps being handled after waking");
}

#[test]
fn guard_rtc_wake_while_aps_down() {
    let (mut w, mut d) = (wk(), Dev::new());
    d.up = 100;
    d.files
        .insert("/sys/class/rtc/rtc0/since_epoch".into(), "1000\n".into());
    d.files
        .insert("/sys/class/rtc/rtc0/wakealarm".into(), "".into());
    let alarm = |d: &Dev| d.files["/sys/class/rtc/rtc0/wakealarm"].trim().to_string();
    w.round(&mut d);
    assert_eq!(alarm(&d), "1300", "arms an alarm when none is pending");
    d.files
        .insert("/sys/class/rtc/rtc0/wakealarm".into(), "1100".into());
    w.round(&mut d);
    assert_eq!(alarm(&d), "1100", "keeps an earlier alarm");
    d.files
        .insert("/sys/class/rtc/rtc0/wakealarm".into(), "5000".into());
    w.round(&mut d);
    assert_eq!(alarm(&d), "1300", "pulls a later alarm in");
    d.beacon = true;
    d.files
        .insert("/sys/class/rtc/rtc0/wakealarm".into(), "5000".into());
    w.round(&mut d);
    assert_eq!(alarm(&d), "5000", "leaves the RTC alone while beaconing");
}

#[test]
fn guard_modem_crash_recovery() {
    let (mut w, mut d) = (wk(), Dev::new());
    let p = "/sys/class/remoteproc/remoteproc0/recovery";
    d.files.insert(p.into(), "disabled\n".into());
    w.round(&mut d);
    assert_eq!(d.files[p].trim(), "enabled");
    assert_eq!(
        d.logs
            .iter()
            .filter(|l| l.contains("modem crash recovery was disabled"))
            .count(),
        1
    );
    assert!(d.files.contains_key("/tmp/luyaz/recovery-set"));
    w.round(&mut d);
    assert_eq!(
        d.logs
            .iter()
            .filter(|l| l.contains("modem crash recovery"))
            .count(),
        1,
        "already enabled: not logged again"
    );
    d.files.remove(p);
    w.round(&mut d);
    assert!(!d.files.contains_key(p), "no remoteproc: left alone");
}

fn clock_ok(d: &Dev) -> String {
    let f: Vec<String> = d.files["/tmp/luyaz/clock-ok"]
        .split_whitespace()
        .map(String::from)
        .collect();
    format!("{} {}", f[0], f[3])
}

#[test]
fn guard_clock_trust() {
    let (mut w, mut d) = (wk(), Dev::new());
    d.up = 1000;
    d.hb(995);
    d.wall = 1_735_948_800; // 2025-01-04: what the device reads before NTP
    w.round(&mut d);
    assert_eq!(clock_ok(&d), "0 none", "2025-01-04 before NTP: not trusted");
    d.up = 1060;
    d.hb(1055);
    d.wall = 1_790_000_000;
    w.round(&mut d);
    assert_eq!(
        clock_ok(&d),
        "1 jump",
        "NTP sets the clock (offset leaps a day+): trusted by the jump"
    );

    let (mut w, mut d) = (wk(), Dev::new());
    d.up = 1000;
    w.round(&mut d);
    assert_eq!(
        clock_ok(&d),
        "1 year",
        "first deployment, 2026, no last trusted clock: trusted by the year"
    );

    let (mut w, mut d) = (wk(), Dev::new());
    d.up = 1000;
    let now = d.wall;
    d.files.insert(
        "/data/ledger/state/wall.last".into(),
        format!("{}\n", now + 3 * 86_400),
    );
    w.round(&mut d);
    assert_eq!(
        clock_ok(&d),
        "0 none",
        "earlier than the last trusted clock minus a day: not trusted"
    );
    d.files.insert(
        "/data/ledger/state/wall.last".into(),
        format!("{}\n", now - 3600),
    );
    w.round(&mut d);
    assert_eq!(clock_ok(&d), "1 last_wall", "not earlier than it: trusted");
    d.up = 1060;
    d.wall = now - 2 * 86_400;
    w.round(&mut d);
    assert_eq!(
        clock_ok(&d),
        "0 revoked",
        "a trusted clock that falls back two days: revoked"
    );
}

#[test]
fn trusted_wall_clock_kept_on_flash_hourly_once_no_guard_runs() {
    let wl = "/data/ledger/state/wall.last";
    let (mut w, mut d) = (wk(), Dev::new());
    d.up = 1000;
    let now = d.wall;
    d.guard = true;
    w.clock_round(&mut d);
    assert!(!d.files.contains_key(wl), "the guard's while it runs");
    d.guard = false;
    w.clock_round(&mut d);
    assert_eq!(
        d.files[wl],
        format!("{now}\n"),
        "trusted by the year: written"
    );
    d.wall = now + 3599;
    w.clock_round(&mut d);
    assert_eq!(d.files[wl], format!("{now}\n"), "not within the hour");
    d.wall = now + 3600;
    w.clock_round(&mut d);
    assert_eq!(d.files[wl], format!("{}\n", now + 3600));
    // not trusted: left alone
    let (mut w, mut d) = (wk(), Dev::new());
    d.wall = 1_735_948_800;
    w.clock_round(&mut d);
    assert!(!d.files.contains_key(wl));
}

fn ship_dev() -> Dev {
    let mut d = Dev::new();
    d.files
        .insert("/data/luyaz/scripts/luyaz-ship.sh".into(), "#!/bin/sh\n".into());
    d.up = 2000;
    d.hb(1995);
    d
}
fn txn(d: &mut Dev, phase: &str, comp: &str, t_phase: u64) {
    d.files.insert(
        "/data/luyaz/ship/txn".into(),
        format!("v=1\ntxn=20260930-120000-{comp}\ncomp={comp}\nphase={phase}\nt_phase={t_phase}\nend=1\n"),
    );
}
fn executor(d: &mut Dev, hb_up: u64, alive: bool) {
    d.files.insert(
        "/tmp/luyaz/ship/heartbeat".into(),
        format!("{hb_up} 4711 20260930-120000-datad check\n"),
    );
    d.cmdlines.remove(&4711);
    if alive {
        d.cmdlines
            .insert(4711, "sh /data/luyaz/scripts/luyaz-ship.sh run ".into());
    }
}

#[test]
fn ship_recover_live() {
    let mut w = wk();
    let mut d = ship_dev();
    w.ship_round(&mut d);
    assert_eq!(d.recover_calls, 0, "no transaction, nothing called");
    for ph in [
        "done",
        "rolledback",
        "aborted",
        "manifest_pending",
        "failed",
    ] {
        let mut d = ship_dev();
        txn(&mut d, ph, "datad", 1900);
        executor(&mut d, 1500, false);
        w.ship_round(&mut d);
        assert_eq!(d.recover_calls, 0, "{ph} is finished, not touched");
    }
    let mut d = ship_dev();
    txn(&mut d, "check", "datad", 1900);
    executor(&mut d, 1990, true);
    w.ship_round(&mut d);
    assert_eq!(
        d.recover_calls, 0,
        "executor alive, heartbeat 10 s old: left alone"
    );
    let mut d = ship_dev();
    txn(&mut d, "check", "datad", 1900);
    executor(&mut d, 1969, true);
    w.ship_round(&mut d);
    assert_eq!(
        d.recover_calls, 1,
        "heartbeat 31 s old (stuck): recover-live"
    );
    assert!(d.logs.iter().any(|l| l.contains(
        "u60-ship: transaction (datad, check) has no live executor (heartbeat 31s old)"
    )));
    let mut d = ship_dev();
    txn(&mut d, "trial", "datad", 1900);
    executor(&mut d, 1999, false);
    w.ship_round(&mut d);
    assert_eq!(
        d.recover_calls, 1,
        "fresh heartbeat but the executor is gone: recover-live"
    );
    let mut d = ship_dev();
    txn(&mut d, "promote", "datad", 1900);
    w.ship_round(&mut d);
    assert!(
        d.recover_calls == 1 && d.logs.iter().any(|l| l.contains("heartbeat missing")),
        "no heartbeat at all"
    );
    let mut d = ship_dev();
    txn(&mut d, "check", "sentinel", 1900);
    executor(&mut d, 1500, false);
    w.ship_round(&mut d);
    assert_eq!(
        d.recover_calls, 0,
        "a sentinel transaction is not the sentinel's to finish"
    );
    let mut d = ship_dev();
    txn(&mut d, "check", "guard", 1900);
    executor(&mut d, 1500, false);
    w.ship_round(&mut d);
    assert_eq!(
        d.recover_calls, 1,
        "a guard transaction is (the guard cannot finish its own)"
    );
    let mut d = ship_dev();
    txn(&mut d, "staged", "datad", 1900);
    w.ship_round(&mut d);
    assert_eq!(
        d.recover_calls, 0,
        "staged 100 s ago, not started yet: left alone"
    );
    txn(&mut d, "staged", "datad", 1600);
    w.ship_round(&mut d);
    assert_eq!(
        d.recover_calls, 1,
        "staged 400 s ago and never started: recover-live"
    );
    let mut d = ship_dev();
    txn(&mut d, "check", "datad", 1900);
    executor(&mut d, 1500, false);
    d.files.remove("/data/luyaz/scripts/luyaz-ship.sh");
    w.ship_round(&mut d);
    assert!(
        d.recover_calls == 0 && d.logs.is_empty(),
        "no luyaz-ship.sh: nothing to call"
    );
    let mut d = ship_dev();
    d.files.insert(
        "/data/luyaz/ship/txn".into(),
        "\u{fffd}\0\u{1}phase=\u{7f}garbage".into(),
    );
    w.ship_round(&mut d);
    assert_eq!(
        d.recover_calls, 0,
        "garbage transaction log: nothing called"
    );
}

#[test]
fn ship_left_to_a_running_guard_except_its_own() {
    let mut w = wk();
    let mut d = ship_dev();
    d.guard = true;
    txn(&mut d, "check", "datad", 1900);
    executor(&mut d, 1500, false);
    w.ship_round(&mut d);
    assert_eq!(
        d.recover_calls, 0,
        "a guard runs: it finishes this one (no lock in recover-live)"
    );
    txn(&mut d, "check", "guard", 1900);
    w.ship_round(&mut d);
    assert_eq!(d.recover_calls, 1, "the guard's own: still the sentinel's");
}

/// run.sh 2218: killed right after a heartbeat at 2000, rounds 60 s apart: taken over ≤ 90 s.
#[test]
fn ship_executor_killed_taken_over_by_the_next_round() {
    let mut w = wk();
    let mut d = ship_dev();
    txn(&mut d, "check", "datad", 1900);
    executor(&mut d, 2000, true);
    d.up = 2029;
    w.ship_round(&mut d);
    assert_eq!(
        d.recover_calls, 0,
        "29 s after the last heartbeat, alive: not yet"
    );
    d.cmdlines.remove(&4711); // the OOM killer
    d.up = 2089;
    w.ship_round(&mut d);
    assert_eq!(
        d.recover_calls, 1,
        "executor killed: recover-live by the next round"
    );
}

#[test]
fn logcap() {
    let mut w = wk();
    w.paths.cap_logs = vec!["/t/ts.log".into(), "/t/missing.log".into()];
    w.paths.cap_bytes = 100;
    w.paths.cap_tmp_logs = vec![];
    let mut d = Dev::new();
    d.files.insert("/t/ts.log".into(), "a".repeat(150));
    w.logcap_round(&mut d);
    assert_eq!(
        d.files["/t/ts.log.old"].len(),
        150,
        "over the cap: previous copy kept as .old"
    );
    assert!(
        d.files["/t/ts.log"].is_empty(),
        "over the cap: log started over"
    );
    assert!(
        d.logs
            .iter()
            .any(|l| l == "capped /t/ts.log at 150 bytes (previous copy in /t/ts.log.old)")
    );
    d.files.insert("/t/ts.log".into(), "b".repeat(50));
    w.logcap_round(&mut d);
    assert_eq!(d.files["/t/ts.log"].len(), 50, "under the cap: untouched");
    d.files.insert("/t/ts.log".into(), "c".repeat(150));
    d.non_append = vec!["/t/ts.log".into()];
    d.up = 1000;
    w.logcap_round(&mut d);
    assert_eq!(
        d.files["/t/ts.log"].len(),
        150,
        "non-append writer: not truncated"
    );
    w.logcap_round(&mut d);
    assert_eq!(
        d.logs.iter().filter(|l| l.contains("not capping")).count(),
        1,
        "logged once"
    );
    d.non_append.clear();
    w.logcap_round(&mut d);
    assert_eq!(
        d.files["/t/ts.log"].len(),
        150,
        "within the hour after a refusal: not rescanned"
    );
    d.up += 3700;
    w.logcap_round(&mut d);
    assert!(
        d.files["/t/ts.log"].is_empty(),
        "an hour later, append writer: capped"
    );

    // the /tmp list has its own, smaller cap; one refused log does not hold the others back
    let mut w = wk();
    w.paths.cap_logs = vec!["/t/ts.log".into()];
    w.paths.cap_bytes = 100;
    w.paths.cap_tmp_logs = vec!["/t/agent.log".into(), "/t/devui.log".into()];
    w.paths.cap_tmp_bytes = 60;
    let mut d = Dev::new();
    d.up = 1000;
    d.files.insert("/t/ts.log".into(), "a".repeat(80));
    d.files.insert("/t/agent.log".into(), "b".repeat(80));
    w.logcap_round(&mut d);
    assert!(d.files["/t/agent.log"].is_empty() && d.files["/t/agent.log.old"].len() == 80);
    assert!(d.files["/t/ts.log"].len() == 80 && !d.files.contains_key("/t/ts.log.old"));
    d.files.insert("/t/ts.log".into(), "c".repeat(150));
    d.non_append = vec!["/t/ts.log".into()];
    w.logcap_round(&mut d);
    d.files.insert("/t/devui.log".into(), "d".repeat(70));
    w.logcap_round(&mut d);
    assert!(
        d.files["/t/devui.log"].is_empty(),
        "within the hour of another log's refusal: this one still capped"
    );
    assert_eq!(d.files["/t/ts.log"].len(), 150);
}

#[test]
fn logcap_default_covers_every_service_log() {
    // every RAM log our procd services write (supervise.sh: /tmp/<name>.log), luyaz-web included
    let p = Paths::default();
    for l in [
        "/tmp/zte-agent.log",
        "/tmp/zwrt-datad.log",
        "/tmp/luyaz-sentinel.log",
        "/tmp/luyaz-sentinel.out",
        "/tmp/luyaz-web.log",
    ] {
        assert!(p.cap_tmp_logs.iter().any(|x| x == l), "{l} not capped");
    }
}

#[test]
fn dropbear() {
    let (mut w, mut d) = (wk(), Dev::new());
    d.beacon = true;
    d.up = 100;
    d.dropbear = false;
    w.round(&mut d);
    w.round(&mut d);
    assert_eq!(d.dropbear_starts, 0, "not in the boot grace");
    d.up = 400;
    w.round(&mut d);
    assert_eq!(d.dropbear_starts, 0, "one round missing: not yet");
    w.round(&mut d);
    assert_eq!(d.dropbear_starts, 1, "two rounds missing: started");
    assert!(d.alerts.contains(&"dropbear-restarted".to_string()));
    for _ in 0..10 {
        d.mono += 60;
        w.round(&mut d);
    }
    assert_eq!(d.dropbear_starts, 3, "at most 3 an hour");
    assert_eq!(
        d.alerts.iter().filter(|a| *a == "dropbear-down").count(),
        1,
        "the limit said once"
    );
    d.mono += 3600;
    w.round(&mut d);
    w.round(&mut d);
    assert_eq!(d.dropbear_starts, 4, "an hour on: again");
    d.dropbear = true;
    w.round(&mut d);
    assert_eq!(d.dropbear_starts, 4);
}

#[test]
fn core_verdict() {
    let (mut w, mut d) = (wk(), Dev::new());
    d.up = 100;
    d.health = Some(Health::Starting);
    w.core_round(&mut d);
    w.core_round(&mut d);
    assert!(
        !w.core.unhealthy,
        "starting in the first 180 s does not count"
    );
    d.up = 400;
    w.core_round(&mut d);
    assert!(!w.core.unhealthy, "one round is not enough");
    w.core_round(&mut d);
    assert!(
        w.core.unhealthy,
        "still starting 400 s after boot, two rounds: unhealthy"
    );
    d.health = Some(Health::Ok);
    w.core_round(&mut d);
    assert!(!w.core.unhealthy);
    d.health = Some(Health::Down("refused".into()));
    w.core_round(&mut d);
    w.core_round(&mut d);
    w.core_round(&mut d);
    assert!(w.core.unhealthy);
    assert_eq!(
        d.logs
            .iter()
            .filter(|l| l.starts_with("core unhealthy"))
            .count(),
        2,
        "said when it turns, not every round"
    );
}

#[test]
fn alert_sms_only_while_the_core_is_unhealthy_and_not_being_replaced() {
    let (mut w, mut d) = (wk(), Dev::new());
    d.up = 400;
    d.health = Some(Health::Ok);
    w.core_round(&mut d);
    assert_eq!(d.sms_rounds, 0);
    d.health = Some(Health::Down("refused".into()));
    w.core_round(&mut d);
    assert_eq!(
        d.sms_rounds, 0,
        "one bad round: the core may just be restarting"
    );
    w.core_round(&mut d);
    assert_eq!(d.sms_rounds, 1);
    d.core_replacing = true;
    w.core_round(&mut d);
    assert_eq!(
        d.sms_rounds, 1,
        "a datad ship in its trial: its replacement sends (D6)"
    );
    d.core_replacing = false;
    d.health = Some(Health::Ok);
    w.core_round(&mut d);
    assert_eq!(d.sms_rounds, 1);
}

#[test]
fn with_no_guard_the_sentinel_hands_its_duties_to_the_core() {
    use crate::worker::CORE_DUTIES;
    const GUARD_HANDOVER: &str = "/tmp/luyaz/handover";
    let (mut w, mut d) = (wk(), Dev::new());
    d.guard = true;
    d.files.insert(GUARD_HANDOVER.into(), "sms\n".into());
    w.handover_round(&mut d);
    assert_eq!(
        d.files[GUARD_HANDOVER], "sms\n",
        "the guard's file while it runs"
    );
    d.guard = false;
    w.handover_round(&mut d);
    assert_eq!(d.files[GUARD_HANDOVER], CORE_DUTIES);
    let logs = d.logs.len();
    w.handover_round(&mut d);
    assert_eq!(d.logs.len(), logs, "written once, not every round");
}

#[test]
fn power_off_charging_keeps_the_worker_idle() {
    let (mut w, mut d) = (wk(), Dev::new());
    d.charging = true;
    d.up = 2000;
    d.health = Some(Health::Down("refused".into()));
    let rec = w.paths.mss_recovery.clone();
    d.files.insert(rec.clone(), "disabled\n".into());
    for _ in 0..5 {
        w.round(&mut d);
    }
    assert_eq!((d.changes, d.restores, d.sms_rounds), (0, 0, 0));
    assert_eq!(d.files[&rec], "disabled\n", "modem recovery left alone");
    assert!(d.alerts.is_empty());
    assert_eq!(d.logs.iter().filter(|l| l.contains("power-off charging")).count(), 1);
}

#[test]
fn boot_modes_as_the_guard_reads_them() {
    use crate::sys::boot_mode_normal;
    assert!(!boot_mode_normal("mode_power_off_charger", "silent_boot.mode=nonsilent"));
    assert!(boot_mode_normal("mode_power_on", ""));
    assert!(boot_mode_normal("mode_power_on_charger", ""));
    assert!(boot_mode_normal("", "x silent_boot.mode=nonsilent y"));
    assert!(!boot_mode_normal("", "silent_boot.mode=silent"));
}

#[test]
fn worker_state_survives_a_restart() {
    let (mut w, mut d) = (wk(), Dev::new());
    d.up = 2000;
    d.hb(1600);
    w.round(&mut d);
    let mut w2 = wk();
    w2.load(&mut d);
    assert_eq!(w2.wifi, w.wifi);
    assert_eq!(w2.clock, w.clock);
    d.up = w.wifi.next_try;
    w2.round(&mut d);
    assert_eq!(
        d.kinds(),
        "agent-silent wifi-takeover wifi-restore-failed",
        "no second takeover alert after a restart"
    );
}

// ── 4. the Wi-Fi restore (D3) ───────────────────────────────────────────────

#[derive(Default)]
struct W {
    listening: bool,
    replies: HashMap<&'static str, Core>,
    wifi_busy: bool,
    write_busy: bool,
    held: Vec<&'static str>,
    ops: Vec<String>,
    onoff: Option<String>,
    lbd: Option<String>,
    comes_up: bool,
    up: bool,
    /// other uci keys (`wireless.guest_*`)
    uci: HashMap<String, String>,
}

impl WifiOps for W {
    fn core_listening(&mut self) -> bool {
        self.listening
    }
    fn core(&mut self, body: &str) -> Core {
        let key = if body.contains("wifi.set_module") {
            "set_module"
        } else if body.contains("wifi.apply") {
            "apply"
        } else {
            "other"
        };
        self.ops.push(format!("core {key} held={:?}", self.held));
        if !self.listening {
            return Core::Direct;
        }
        let r = self.replies.get(key).cloned().unwrap_or(Core::Done);
        if r == Core::Done && key == "apply" && self.comes_up {
            self.up = true;
        }
        r
    }
    fn lock_wifi(&mut self, _w: std::time::Duration) -> Result<(), String> {
        self.ops.push(format!("lock wifi held={:?}", self.held));
        if self.wifi_busy {
            return Err("busy".into());
        }
        self.held.push("wifi");
        Ok(())
    }
    fn unlock_wifi(&mut self) {
        self.held.retain(|h| *h != "wifi");
    }
    fn lock_write(&mut self, _w: std::time::Duration) -> Result<(), String> {
        self.ops.push(format!("lock write held={:?}", self.held));
        assert!(
            !self.held.contains(&"wifi"),
            "never the write lock while holding the Wi-Fi lock"
        );
        if self.write_busy {
            return Err("busy".into());
        }
        self.held.push("write");
        Ok(())
    }
    fn unlock_write(&mut self) {
        self.held.retain(|h| *h != "write");
    }
    fn uci_get(&mut self, key: &str) -> Option<String> {
        if key.ends_with("wifi_onoff") {
            self.onoff.clone()
        } else if key.ends_with(".lbd") {
            self.lbd.clone()
        } else {
            self.uci.get(key).cloned()
        }
    }
    fn uci_set_commit(&mut self, pairs: &[(&str, &str)]) -> Result<(), String> {
        let keys: Vec<String> = pairs.iter().map(|(k, v)| format!("{k}={v}")).collect();
        self.ops
            .push(format!("uci {} held={:?}", keys.join(","), self.held));
        Ok(())
    }
    fn ubus(&mut self, object: &str, method: &str, args: &str) -> Result<(), String> {
        self.ops.push(format!("ubus {object} {method} {args}"));
        if method == "reload" && self.comes_up {
            self.up = true;
        }
        Ok(())
    }
    fn beaconing(&mut self) -> bool {
        self.up
    }
    fn sleep_s(&mut self, _s: u64) {}
    fn log(&mut self, msg: &str) {
        self.ops.push(format!("log {msg}"));
    }
}

fn wops(w: &W, prefix: &str) -> Vec<String> {
    w.ops
        .iter()
        .filter(|o| o.starts_with(prefix))
        .cloned()
        .collect()
}

#[test]
fn wifi_through_the_core_writes_nothing_directly() {
    let mut w = W {
        listening: true,
        comes_up: true,
        ..Default::default()
    };
    assert_eq!(wifi::restore(&mut w), Ok(()));
    assert_eq!(
        wops(&w, "core"),
        vec![r#"core apply held=["wifi"]"#],
        "wifi.apply through the core, under the Wi-Fi lock"
    );
    assert!(
        wops(&w, "uci").is_empty()
            && wops(&w, "ubus").is_empty()
            && wops(&w, "lock write").is_empty()
    );
    assert!(w.held.is_empty(), "locks let go");
    assert!(wifi::APPLY.contains(r#""wireless.wifi0.disabled":"0","wireless.wifi1.disabled":"0","wireless.main_2g.disabled":"0","wireless.main_5g.disabled":"0","wireless.guest_2g.disabled":"1","wireless.guest_5g.disabled":"1"},"reload":true,"best_effort":true"#));
}

#[test]
fn wifi_core_says_no_or_hangs_no_direct_write() {
    for r in [
        Core::No(r#"{"ok":false,"error":{"code":"failed"}}"#.into()),
        Core::No("timed out".into()),
    ] {
        let mut w = W {
            listening: true,
            comes_up: true,
            ..Default::default()
        };
        w.replies.insert("apply", r);
        assert!(wifi::restore(&mut w).is_err());
        assert!(
            wops(&w, "uci").is_empty() && wops(&w, "ubus").is_empty(),
            "D18: no direct write"
        );
        assert!(w.held.is_empty());
    }
}

#[test]
fn wifi_core_too_old_or_gone_direct_with_both_locks_in_order() {
    let mut w = W {
        listening: true,
        comes_up: true,
        ..Default::default()
    };
    w.replies.insert("apply", Core::Direct); // unknown_action
    assert_eq!(wifi::restore(&mut w), Ok(()));
    let locks: Vec<String> = wops(&w, "lock");
    assert_eq!(
        locks,
        vec![
            r#"lock wifi held=[]"#,
            r#"lock write held=[]"#,
            r#"lock wifi held=["write"]"#
        ],
        "let go, then write lock, then Wi-Fi lock"
    );
    let uci = wops(&w, "uci");
    assert_eq!(uci.len(), 1);
    assert!(uci[0].contains("wireless.wifi0.disabled=0,wireless.wifi1.disabled=0,wireless.main_2g.disabled=0,wireless.main_5g.disabled=0"));
    assert!(uci[0].ends_with(r#"held=["write", "wifi"]"#));
    assert!(w.ops.contains(&"ubus zwrt_wlan reload {}".to_string()));
    assert!(w.held.is_empty());

    let mut w = W {
        listening: false,
        comes_up: true,
        ..Default::default()
    };
    assert_eq!(wifi::restore(&mut w), Ok(()));
    assert!(
        wops(&w, "core").is_empty(),
        "nothing listening: straight to the direct path"
    );
    assert_eq!(
        wops(&w, "lock"),
        vec![r#"lock write held=[]"#, r#"lock wifi held=["write"]"#]
    );
}

#[test]
fn wifi_direct_rescue_turns_a_lone_guest_off() {
    // one band's guest on + both mains = three APs, which the firmware cannot run
    let mut w = W {
        listening: false,
        comes_up: true,
        ..Default::default()
    };
    for (k, v) in [
        ("wireless.guest_2g", "wifi-iface"),
        ("wireless.guest_2g.disabled", "1"),
        ("wireless.guest_5g", "wifi-iface"),
        ("wireless.guest_5g.disabled", "0"),
    ] {
        w.uci.insert(k.into(), v.into());
    }
    assert_eq!(wifi::restore(&mut w), Ok(()));
    let uci = wops(&w, "uci");
    assert_eq!(uci.len(), 1);
    assert!(
        uci[0].contains("wireless.main_5g.disabled=0,wireless.guest_5g.disabled=1 "),
        "{}",
        uci[0]
    );
    assert!(!uci[0].contains("guest_2g"), "already off: not written");

    // no guest sections on this firmware: nothing extra
    let mut w = W {
        listening: false,
        comes_up: true,
        ..Default::default()
    };
    assert_eq!(wifi::restore(&mut w), Ok(()));
    assert!(!wops(&w, "uci")[0].contains("guest"));
}

#[test]
fn wifi_write_lock_busy_gives_up_this_round_then_works() {
    let mut w = W {
        listening: false,
        comes_up: true,
        write_busy: true,
        ..Default::default()
    };
    let e = wifi::restore(&mut w).unwrap_err();
    assert!(e.contains("write lock"), "{e}");
    assert!(
        wops(&w, "uci").is_empty() && w.held.is_empty(),
        "nothing written, nothing held"
    );
    w.write_busy = false;
    w.ops.clear();
    assert_eq!(wifi::restore(&mut w), Ok(()), "next round: works");
    let mut w = W {
        listening: false,
        comes_up: true,
        wifi_busy: true,
        ..Default::default()
    };
    assert!(wifi::restore(&mut w).is_err());
    assert!(
        w.held.is_empty(),
        "write lock let go when the Wi-Fi lock is not got"
    );
}

#[test]
fn wifi_vendor_master_switch() {
    // direct: band steering sent along as it stands
    let mut w = W {
        listening: false,
        comes_up: true,
        onoff: Some("0".into()),
        lbd: Some("1".into()),
        ..Default::default()
    };
    wifi::restore(&mut w).unwrap();
    let ubus = wops(&w, "ubus");
    assert_eq!(
        ubus[0], r#"ubus zwrt_wlan set {"zte_mbb":{"wifi_onoff":"1","lbd":"1"}}"#,
        "switch on before the reload"
    );
    assert_eq!(ubus[1], "ubus zwrt_wlan reload {}");
    // lbd junk: left out
    let mut w = W {
        listening: false,
        comes_up: true,
        onoff: Some("0".into()),
        lbd: Some("1\"}}; x".into()),
        ..Default::default()
    };
    wifi::restore(&mut w).unwrap();
    assert_eq!(
        wops(&w, "ubus")[0],
        r#"ubus zwrt_wlan set {"zte_mbb":{"wifi_onoff":"1"}}"#
    );
    // key missing (= on): no vendor call
    let mut w = W {
        listening: false,
        comes_up: true,
        ..Default::default()
    };
    wifi::restore(&mut w).unwrap();
    assert!(!w.ops.iter().any(|o| o.contains("zwrt_wlan set")));
    // through the core: set_module before apply
    let mut w = W {
        listening: true,
        comes_up: true,
        onoff: Some("0".into()),
        ..Default::default()
    };
    wifi::restore(&mut w).unwrap();
    assert_eq!(
        wops(&w, "core"),
        vec![
            r#"core set_module held=["wifi"]"#,
            r#"core apply held=["wifi"]"#
        ]
    );
    assert!(
        wifi::SET_MODULE
            == r#"{"action":"wifi.set_module","source":"guard","params":{"enabled":1}}"#
    );
    // the core refuses the switch: logged, the restore still asked, no direct set
    let mut w = W {
        listening: true,
        comes_up: true,
        onoff: Some("0".into()),
        ..Default::default()
    };
    w.replies.insert("set_module", Core::No("timeout".into()));
    wifi::restore(&mut w).unwrap();
    assert!(!w.ops.iter().any(|o| o.contains("zwrt_wlan set")));
    assert!(
        w.ops
            .iter()
            .any(|o| o.contains("refused or did not answer the vendor Wi-Fi switch"))
    );
}

#[test]
fn wifi_vendor_master_switch_more() {
    // switched on directly: the APs' disabled restore still runs, after it
    let mut w = W {
        listening: false,
        comes_up: true,
        onoff: Some("0".into()),
        ..Default::default()
    };
    wifi::restore(&mut w).unwrap();
    let set = w
        .ops
        .iter()
        .position(|o| o.contains("zwrt_wlan set"))
        .unwrap();
    let uci = w.ops.iter().position(|o| o.starts_with("uci ")).unwrap();
    assert!(
        set < uci && w.ops[uci].contains("wireless.main_2g.disabled=0,wireless.main_5g.disabled=0")
    );
    // wifi_onoff=1: no vendor call at all, either way
    for listening in [false, true] {
        let mut w = W {
            listening,
            comes_up: true,
            onoff: Some("1".into()),
            ..Default::default()
        };
        wifi::restore(&mut w).unwrap();
        assert!(
            !w.ops
                .iter()
                .any(|o| o.contains("zwrt_wlan set") || o.contains("set_module")),
            "wifi_onoff=1 (listening={listening}): no vendor call"
        );
    }
    // the core too old for both: vendor switch and APs written directly
    let mut w = W {
        listening: true,
        comes_up: true,
        onoff: Some("0".into()),
        ..Default::default()
    };
    w.replies.insert("set_module", Core::Direct);
    w.replies.insert("apply", Core::Direct);
    wifi::restore(&mut w).unwrap();
    assert_eq!(wops(&w, "core"), vec![r#"core set_module held=["wifi"]"#]);
    assert!(
        w.ops
            .iter()
            .any(|o| o.contains(r#"zwrt_wlan set {"zte_mbb":{"wifi_onoff":"1"}}"#))
    );
    assert_eq!(wops(&w, "uci").len(), 1);
    assert!(w.held.is_empty());
}

#[test]
fn wifi_autosleep_through_the_core_or_direct() {
    let mut w = W {
        listening: true,
        ..Default::default()
    };
    wifi::set_autosleep(&mut w, false);
    assert!(wops(&w, "ubus").is_empty(), "the core took it: not ubus");
    w.replies.insert("other", Core::No("timed out".into()));
    wifi::set_autosleep(&mut w, false);
    assert!(
        wops(&w, "ubus").is_empty(),
        "the core says no: no direct write either (D18)"
    );
    let mut w = W::default();
    wifi::set_autosleep(&mut w, true);
    assert_eq!(
        wops(&w, "ubus"),
        vec![r#"ubus zwrt_zte_sleep_faw.wakelock enableAutoSleep {"switch":true}"#],
        "the core not there: direct"
    );
}

#[test]
fn wifi_no_hostapd_is_a_failure() {
    let mut w = W {
        listening: false,
        comes_up: false,
        ..Default::default()
    };
    assert!(
        wifi::restore(&mut w)
            .unwrap_err()
            .contains("no hostapd within 45s")
    );
}

#[test]
fn wifi_reply_classes() {
    assert_eq!(
        wifi::classify(Ok(r#"{"action":"wifi.apply","ok":true}"#.into()), false),
        Core::Done
    );
    assert_eq!(
        wifi::classify(
            Ok(r#"{"error":{"code":"unknown_action","message":"unsupported"},"ok":false}"#.into()),
            false
        ),
        Core::Direct
    );
    assert!(matches!(
        wifi::classify(Ok(r#"{"ok":false}"#.into()), false),
        Core::No(_)
    ));
    assert!(matches!(
        wifi::classify(Err("timed out".into()), false),
        Core::No(_)
    ));
    assert_eq!(wifi::classify(Err("refused".into()), true), Core::Direct);
    assert_eq!(
        wifi::autosleep_body(false),
        r#"{"action":"vendor.call","source":"guard","params":{"object":"zwrt_zte_sleep_faw.wakelock","method":"enableAutoSleep","args":{"switch":false}}}"#
    );
}

// ── 5. shadow mode changes nothing ──────────────────────────────────────────

#[cfg(target_os = "linux")]
#[test]
fn shadow_worker_changes_nothing() {
    use crate::run::ShadowWorker;
    // every worker scenario above that makes a change, through the shadow wrapper
    let mut d = Dev::new();
    d.up = 2000;
    d.hb(1600);
    d.files.insert(
        "/sys/class/remoteproc/remoteproc0/recovery".into(),
        "disabled\n".into(),
    );
    d.files
        .insert("/sys/class/rtc/rtc0/since_epoch".into(), "1000\n".into());
    d.files
        .insert("/data/luyaz/scripts/luyaz-ship.sh".into(), "x".into());
    txn(&mut d, "check", "datad", 1900);
    d.files
        .insert("/tmp/zte-agent.log".into(), "z".repeat(300_000));
    d.dropbear = false;
    let mut sh = ShadowWorker::new(d);
    let mut w = wk();
    for _ in 0..5 {
        sh.real.up += 60;
        sh.real.mono += 60;
        w.round(&mut sh);
    }
    assert_eq!(sh.real.changes, 0, "the device saw no change");
    assert!(sh.recorded > 0, "but the decisions were recorded");
    assert!(
        w.wifi.took_over && w.wifi.episode.is_some(),
        "the shadow's own state moved on"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn write_synced_leaves_the_directory_mode_alone() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("sentinel-ws-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o1777)).unwrap();
    let f = dir.join("want");
    crate::sys::write_synced(f.to_str().unwrap(), "vendor\n").unwrap();
    crate::sys::write_atomic(f.to_str().unwrap(), "vendor\n").unwrap();
    let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o7777;
    assert_eq!(
        mode, 0o1777,
        "a sticky world-writable dir (/tmp) keeps its mode"
    );
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "vendor\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_new_tmp_tree_is_marked_live_only_after_its_files_are_in_place() {
    use crate::worker::CORE_DUTIES;
    const MARK: &str = "/tmp/luyaz/.migrated";
    let (mut w, mut d) = (wk(), Dev::new());
    w.migrated_round(&mut d);
    assert!(!d.files.contains_key(MARK), "nothing written yet");
    w.handover_round(&mut d);
    w.migrated_round(&mut d);
    assert!(!d.files.contains_key(MARK), "no clock-ok yet");
    w.clock_round(&mut d);
    w.migrated_round(&mut d);
    assert_eq!(d.files.get(MARK).map(String::as_str), Some(""));
    assert_eq!(d.files["/tmp/luyaz/handover"], CORE_DUTIES);
    assert!(!d.files.keys().any(|k| k.starts_with("/tmp/u60-guard")), "old tree untouched");
    let logs = d.logs.len();
    w.migrated_round(&mut d);
    assert_eq!(d.logs.len(), logs, "written once");
}

#[test]
fn a_whole_round_leaves_the_mark_and_a_guard_keeps_it_away() {
    let (mut w, mut d) = (wk(), Dev::new());
    d.guard = true;
    w.round(&mut d);
    assert!(!d.files.contains_key("/tmp/luyaz/.migrated"), "the guard's handover");
    d.guard = false;
    w.round(&mut d);
    assert!(d.files.contains_key("/tmp/luyaz/clock-ok"));
    assert!(d.files.contains_key("/tmp/luyaz/.migrated"));
}
