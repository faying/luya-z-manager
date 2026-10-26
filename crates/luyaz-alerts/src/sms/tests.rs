use std::sync::{Arc, Mutex};

use super::*;

fn dir(name: &str) -> Alerts {
    let d = std::env::temp_dir().join(format!("u60-alerts-sms-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    Alerts::at(&d)
}

/// 2026-10-01 14:32:05 (device local time marked UTC).
const T0: i64 = 1_790_865_125;

#[derive(Clone)]
struct Fake {
    wall: i64,
    up: u64,
    boot: String,
    alive: Vec<u32>,
    clock_ok: bool,
    en: bool,
    fail: bool,
    /// The send never reaches the device.
    unreachable: bool,
    /// Every SMS sent (decoded back from UCS-2), shared between senders.
    sent: Arc<Mutex<Vec<String>>>,
    deleted: Arc<Mutex<usize>>,
    logs: Arc<Mutex<Vec<String>>>,
    /// Sleep in send (concurrency test).
    slow_ms: u64,
    /// The device refuses this many sends, then takes them.
    refuse: u32,
    /// The owner's regress SMS switch is off.
    off: bool,
}

impl Fake {
    fn new() -> Self {
        Fake {
            wall: T0,
            up: 1000,
            boot: "boot-a".into(),
            alive: vec![10, 20],
            clock_ok: true,
            en: false,
            fail: false,
            unreachable: false,
            sent: Arc::default(),
            deleted: Arc::default(),
            logs: Arc::default(),
            slow_ms: 0,
            refuse: 0,
            off: false,
        }
    }
    fn sent(&self) -> Vec<String> {
        self.sent.lock().unwrap().clone()
    }
}

fn decode(hex: &str) -> String {
    (0..hex.len() / 4)
        .map(|i| char::from_u32(u32::from_str_radix(&hex[i * 4..i * 4 + 4], 16).unwrap()).unwrap())
        .collect()
}

impl SmsIo for Fake {
    fn wall(&mut self) -> i64 {
        self.wall
    }
    fn uptime(&mut self) -> u64 {
        self.up
    }
    fn boot_id(&mut self) -> String {
        self.boot.clone()
    }
    fn pid_alive(&mut self, pid: u32) -> bool {
        self.alive.contains(&pid)
    }
    fn clock_ok(&mut self) -> bool {
        self.clock_ok
    }
    fn lang_en(&mut self) -> bool {
        self.en
    }
    fn tz_hours(&mut self) -> i32 {
        8
    }
    fn send(&mut self, args: &serde_json::Value) -> Result<(), SendError> {
        if self.slow_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(self.slow_ms));
        }
        if self.unreachable {
            return Err(SendError::NotSent("no answer".into()));
        }
        if self.fail {
            return Err(SendError::Rejected("device rejected (result=1)".into()));
        }
        if self.refuse > 0 {
            self.refuse -= 1;
            return Err(SendError::Rejected("device rejected (result=1)".into()));
        }
        assert_eq!(args["encode_type"], "UNICODE");
        assert_eq!(args["id"], "-1");
        self.sent
            .lock()
            .unwrap()
            .push(decode(args["message_body"].as_str().unwrap()));
        Ok(())
    }
    fn delete_sent_copy(&mut self, _number: &str) {
        *self.deleted.lock().unwrap() += 1;
    }
    fn log(&mut self, msg: &str) {
        self.logs.lock().unwrap().push(msg.to_string());
    }
    fn regress_sms_off(&mut self) -> bool {
        self.off
    }
}

fn setup(name: &str, kinds: &[&str]) -> Alerts {
    let a = dir(name);
    a.write_whole("sms-to", "+8613800000000\n").unwrap();
    for k in kinds {
        a.add(k, "text", T0 - 60, 900).unwrap();
    }
    a
}

fn log_results(a: &Alerts) -> Vec<(u64, String, String)> {
    parse_sms_log(&fs::read_to_string(a.path("sms-log")).unwrap_or_default())
        .into_iter()
        .map(|r| (r.seq, r.kind, r.result))
        .collect()
}

// ── rules ──

#[test]
fn verdicts_in_the_guards_order() {
    let base = Facts {
        kind: "web-crash",
        number: Some("+8613800000000"),
        abroad: false,
        sms_abroad: false,
        clock_ok: true,
        rate_ok: true,
    };
    assert_eq!(verdict(&base), Verdict::Send);
    assert_eq!(
        verdict(&Facts {
            kind: "sms-failed",
            ..base
        }),
        Verdict::Skip
    );
    assert_eq!(
        verdict(&Facts {
            kind: "ui-theme-paused",
            number: None,
            ..base
        }),
        Verdict::Skip
    );
    assert_eq!(
        verdict(&Facts {
            number: None,
            clock_ok: false,
            ..base
        }),
        Verdict::Record("no-number")
    );
    assert_eq!(
        verdict(&Facts {
            number: Some("12"),
            ..base
        }),
        Verdict::Record("no-number")
    );
    assert_eq!(
        verdict(&Facts {
            abroad: true,
            clock_ok: false,
            ..base
        }),
        Verdict::Record("suppressed-abroad")
    );
    assert_eq!(
        verdict(&Facts {
            abroad: true,
            sms_abroad: true,
            ..base
        }),
        Verdict::Send
    );
    assert_eq!(
        verdict(&Facts {
            clock_ok: false,
            rate_ok: false,
            ..base
        }),
        Verdict::Hold
    );
    assert_eq!(
        verdict(&Facts {
            rate_ok: false,
            ..base
        }),
        Verdict::Record("suppressed-rate")
    );
}

#[test]
fn rate_limits_count_only_sent_and_future_stamps_count() {
    let now = T0;
    let l = |rows: &[(i64, &str, &str)]| {
        rows.iter()
            .enumerate()
            .map(|(i, (w, k, r))| format!("{w}\t{i}\t{k}\t{r}\n"))
            .collect::<String>()
    };
    assert!(rate_ok("", "a", now));
    assert!(!rate_ok(&l(&[(now - 3599, "a", "sent")]), "a", now));
    assert!(rate_ok(&l(&[(now - 3601, "a", "sent")]), "a", now));
    assert!(rate_ok(&l(&[(now - 10, "a", "failed")]), "a", now));
    assert!(rate_ok(&l(&[(now - 10, "b", "sent")]), "a", now));
    assert!(!rate_ok(&l(&[(now + 99_999, "a", "sent")]), "a", now));
    let five: Vec<(i64, &str, &str)> = (0..5).map(|i| (now - 4000 - i, "b", "sent")).collect();
    assert!(!rate_ok(&l(&five), "a", now));
    assert!(rate_ok(&l(&five[..4]), "a", now));
    let old: Vec<(i64, &str, &str)> = (0..5).map(|i| (now - 86_401 - i, "b", "sent")).collect();
    assert!(rate_ok(&l(&old), "a", now));
}

// ── texts ──

const KINDS: &[&str] = &[
    "wifi-takeover",
    "wifi-restore-failed",
    "sentinel-silent",
    "web-crash",
    "core-crash",
    "ui-crash",
    "ui-gave-up",
    "sms-test",
    "screen-two-uis",
    "screen-hold-expired",
    "dropbear-restarted",
    "dropbear-down",
    "sentinel-silent",
    "screen-empty",
    "web-crash",
    "sentinel-crash",
    "home-exit-down",
    "home-exit-check-failed", // the longest kind there is, through the zh fallback
];

#[test]
fn texts_word_for_word_as_the_guard_sent_them_plus_the_code() {
    assert_eq!(
        sms_text("web-crash", false, T0, 7, None),
        "【Luya-Z】管理网页意外退出，已自动重启。不用管。（10-01 14:32）#7"
    );
    assert_eq!(
        sms_text("web-crash", true, T0, 7, None),
        "[Luya-Z] Admin pages crashed; restarted; no action (01 Oct 14:32)#7"
    );
    assert_eq!(
        sms_text("x-y", false, T0, 1, None),
        "【Luya-Z】有新告警（x-y），请到管理网页「系统→告警」查看。（10-01 14:32）#1"
    );
    assert_eq!(
        sms_text("a$b", true, T0, 1, None),
        "[Luya-Z] Alert ab; see Alerts on web (01 Oct 14:32)#1"
    );
}

#[test]
fn a_device_name_goes_after_the_signature() {
    assert_eq!(
        sms_text("web-crash", false, T0, 7, Some("U60Pro全球版")),
        "【Luya-Z】U60Pro全球版：管理网页意外退出，已自动重启。不用管。（10-01 14:32）#7"
    );
    assert_eq!(
        sms_text("web-crash", true, T0, 7, Some("U60Pro")),
        "[Luya-Z] U60Pro: Admin pages crashed; restarted; no action (01 Oct 14:32)#7"
    );
}

#[test]
fn every_text_is_one_sms_with_a_long_code() {
    for k in KINDS {
        for en in [false, true] {
            let t = sms_text(k, en, T0, 99_999, None);
            assert!(t.ends_with("#999"), "{t}");
            assert!(
                t.chars().count() <= 70,
                "{k} en={en}: {} chars: {t}",
                t.chars().count()
            );
            if en {
                assert!(t.bytes().all(|b| (0x20..0x7f).contains(&b)), "{t}");
            }
        }
    }
}

#[test]
fn an_overlong_unknown_kind_is_cut_in_english() {
    let t = sms_text("some-very-long-unknown-kind-name-here", true, T0, 999, None);
    assert!(t.chars().count() <= 70, "{t}");
}

#[test]
fn ucs2_time_and_dates() {
    assert_eq!(ucs2_hex("A中"), "00414E2D");
    assert_eq!(ucs2_hex("😀"), "FFFD");
    assert_eq!(sms_time(T0, 8), "26;10;01;14;32;05;+8");
    assert_eq!(sms_time(T0, -5), "26;10;01;14;32;05;-5");
    assert_eq!(civil(0), (1970, 1, 1, 0, 0, 0));
    assert_eq!(civil(951_782_400), (2000, 2, 29, 0, 0, 0));
    assert!(send_result_ok(None));
    assert!(send_result_ok(Some(&serde_json::json!(3))));
    assert!(!send_result_ok(Some(&serde_json::json!(1))));
    let args = send_args("+861", "A", T0, 8);
    assert_eq!(args["message_body"], "0041");
    assert_eq!(args["sms_time"], "26;10;01;14;32;05;+8");
}

// ── send states ──

#[test]
fn a_round_sends_logs_moves_the_cursor_and_deletes_the_copy() {
    let a = setup("happy", &["web-crash", "sms-failed", "ui-crash"]);
    let mut io = Fake::new();
    assert_eq!(a.sms_round("core", 10, &mut io), Round::Done(3));
    let sent = io.sent();
    assert_eq!(sent.len(), 2);
    assert!(
        sent[0].ends_with("#1") && sent[1].ends_with("#3"),
        "{sent:?}"
    );
    assert_eq!(*io.deleted.lock().unwrap(), 2);
    assert_eq!(
        log_results(&a),
        vec![
            (1, "web-crash".into(), "sent".into()),
            (3, "ui-crash".into(), "sent".into())
        ]
    );
    assert_eq!(a.read_u64("sms-done"), 3);
    assert!(!a.exists("sms-sending"));
    assert_eq!(a.sms_round("core", 10, &mut io), Round::Idle);
}

#[test]
fn sender_died_mid_send_its_alert_goes_again() {
    // The sentinel claimed alert 1 and died before the send returned (pid 99 gone).
    let a = setup("crash", &["sentinel-silent"]);
    a.write_whole(
        "sms-sending",
        &format!("1\tsentinel\t99\tboot-a\t990\t{T0}\n"),
    )
    .unwrap();
    let mut io = Fake::new();
    assert_eq!(a.sms_round("core", 10, &mut io), Round::Done(1));
    assert_eq!(io.sent().len(), 1);
    assert!(io.sent()[0].ends_with("#1"));
    assert!(
        io.logs
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("sending it again"))
    );
    assert_eq!(a.read_u64("sms-done"), 1);
}

#[test]
fn uncertain_after_120_s_even_if_the_pid_lives_or_after_a_reboot() {
    for (name, claim, expect_sent) in [
        (
            "fresh",
            format!("1\tsentinel\t20\tboot-a\t{}\t{T0}\n", 1000 - 119),
            0,
        ),
        (
            "old",
            format!("1\tsentinel\t20\tboot-a\t{}\t{T0}\n", 1000 - 121),
            1,
        ),
        ("reboot", format!("1\tsentinel\t20\tboot-z\t5\t{T0}\n"), 1),
    ] {
        let a = setup(name, &["sentinel-silent"]);
        a.write_whole("sms-sending", &claim).unwrap();
        let mut io = Fake::new();
        let r = a.sms_round("core", 10, &mut io);
        assert_eq!(io.sent().len(), expect_sent, "{name}");
        if expect_sent == 0 {
            assert_eq!(r, Round::Busy("sentinel".into()));
            assert_eq!(a.read_u64("sms-done"), 0);
        }
    }
}

#[test]
fn a_resend_is_not_rate_limited() {
    // The uncertain one may never have gone out; the log already has a "sent" of the same kind
    // within the hour (another alert), which would otherwise suppress it.
    let a = setup("resend-rate", &["sentinel-silent", "sentinel-silent"]);
    a.write_whole(
        "sms-log",
        &format!("{}\t1\tsentinel-silent\tsent\n", T0 - 100),
    )
    .unwrap();
    a.write_whole("sms-done", "1\n").unwrap();
    a.write_whole(
        "sms-sending",
        &format!("2\tsentinel\t99\tboot-a\t990\t{T0}\n"),
    )
    .unwrap();
    let mut io = Fake::new();
    a.sms_round("core", 10, &mut io);
    assert_eq!(io.sent().len(), 1);
    assert_eq!(log_results(&a).last().unwrap().2, "sent");
}

#[test]
fn sender_restart_takes_its_own_old_claim_over() {
    let a = setup("restart", &["sentinel-silent"]);
    a.write_whole(
        "sms-sending",
        &format!("1\tsentinel\t77\tboot-a\t995\t{T0}\n"),
    )
    .unwrap();
    let mut io = Fake::new(); // 77 not alive: the old sentinel process
    assert_eq!(a.sms_round("sentinel", 20, &mut io), Round::Done(1));
    assert_eq!(io.sent().len(), 1);
}

#[test]
fn no_trusted_clock_holds_everything_and_drops_the_claim() {
    let a = setup("clock", &["web-crash"]);
    let mut io = Fake::new();
    io.clock_ok = false;
    assert_eq!(a.sms_round("core", 10, &mut io), Round::Held);
    assert!(io.sent().is_empty());
    assert_eq!(a.read_u64("sms-done"), 0);
    assert!(!a.exists("sms-sending"));
    io.clock_ok = true;
    assert_eq!(a.sms_round("core", 10, &mut io), Round::Done(1));
}

#[test]
fn a_failed_send_is_logged_alerted_and_not_texted_about() {
    // a kind outside the regress and point-event families: refused once is failed (eng review R9)
    let a = setup("fail", &["dropbear-down"]);
    let mut io = Fake::new();
    io.fail = true;
    assert_eq!(a.sms_round("core", 10, &mut io), Round::Done(2));
    assert_eq!(
        log_results(&a),
        vec![(1, "dropbear-down".into(), "failed".into())]
    );
    let ev = a.events();
    assert_eq!(ev.last().unwrap().kind, "sms-failed");
    assert_eq!(ev.last().unwrap().text, "alert 1 (dropbear-down) not sent");
    // the same round went on to the sms-failed it added: skipped, no SMS about a failed SMS
    assert!(io.sent().is_empty());
    assert_eq!(a.read_u64("sms-done"), 2);
}

#[test]
fn no_number_and_abroad_are_recorded() {
    let a = setup("nonum", &["web-crash"]);
    a.set_flag("sms-to", false).unwrap();
    let mut io = Fake::new();
    a.sms_round("core", 10, &mut io);
    assert_eq!(log_results(&a)[0].2, "no-number");
    a.write_whole("sms-to", "+8613800000000\n").unwrap();
    a.set_flag("abroad", true).unwrap();
    a.add("web-crash", "x", T0, 1).unwrap();
    a.sms_round("core", 10, &mut io);
    assert_eq!(log_results(&a)[1].2, "suppressed-abroad");
    assert!(io.sent().is_empty());
}

#[test]
fn english_follows_the_screen() {
    let a = setup("en", &["ui-crash"]);
    let mut io = Fake::new();
    io.en = true;
    a.sms_round("core", 10, &mut io);
    assert_eq!(
        io.sent()[0],
        "[Luya-Z] Screen UI crashed; reopened; no action (01 Oct 14:32)#1"
    );
}

#[test]
fn sms_log_keeps_the_last_100() {
    let a = setup("trim", &[]);
    let rows: String = (0..150)
        .map(|i| format!("{}\t{i}\tx\tno-number\n", T0 - 99_999))
        .collect();
    a.write_whole("sms-log", &rows).unwrap();
    a.add("web-crash", "x", T0, 1).unwrap();
    a.write_whole("sms-done", "0\n").unwrap();
    a.sms_round("core", 10, &mut Fake::new());
    let all = log_results(&a);
    assert_eq!(all.len(), 100);
    assert_eq!(all.last().unwrap().2, "sent");
}

#[test]
fn two_senders_at_once_send_each_alert_once() {
    let a = setup(
        "two",
        &[
            "web-crash",
            "ui-crash",
            "core-crash",
            "dropbear-down",
            "sentinel-silent",
        ],
    );
    let dir = a.dir.clone();
    let mut base = Fake::new();
    base.slow_ms = 30;
    let mut handles = Vec::new();
    for (name, pid) in [("core", 10u32), ("sentinel", 20u32)] {
        let mut io = base.clone();
        let dir = dir.clone();
        handles.push(std::thread::spawn(move || {
            let a = Alerts::at(&dir);
            for _ in 0..40 {
                a.sms_round(name, pid, &mut io);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    let mut sent = base.sent();
    sent.sort();
    assert_eq!(sent.len(), 5, "{sent:?}");
    sent.dedup();
    assert_eq!(sent.len(), 5);
    assert_eq!(a.read_u64("sms-done"), 5);
    assert_eq!(log_results(&a).len(), 5);
}

#[test]
fn an_idle_round_writes_nothing() {
    use std::os::unix::fs::MetadataExt;
    let a = setup("idle", &["web-crash"]);
    let mut io = Fake::new();
    a.sms_round("core", 10, &mut io);
    let before: Vec<(i64, i64)> = ["", "lock", "sms-done"]
        .iter()
        .map(|f| {
            let m = fs::metadata(a.dir.join(f)).unwrap();
            (m.ctime(), m.ctime_nsec())
        })
        .collect();
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert_eq!(a.sms_round("core", 10, &mut io), Round::Idle);
    a.ensure_dir().unwrap();
    let after: Vec<(i64, i64)> = ["", "lock", "sms-done"]
        .iter()
        .map(|f| {
            let m = fs::metadata(a.dir.join(f)).unwrap();
            (m.ctime(), m.ctime_nsec())
        })
        .collect();
    assert_eq!(before, after);
}

#[test]
fn not_reaching_the_device_keeps_the_alert_pending() {
    let a = setup("unreach", &["web-crash"]);
    let mut io = Fake::new();
    io.unreachable = true;
    assert_eq!(a.sms_round("core", 10, &mut io), Round::Done(0));
    assert_eq!(a.read_u64("sms-done"), 0);
    assert!(!a.exists("sms-sending"));
    assert!(log_results(&a).is_empty());
    assert!(a.events().iter().all(|e| e.kind != "sms-failed"));
    io.unreachable = false;
    assert_eq!(a.sms_round("core", 10, &mut io), Round::Done(1));
    assert_eq!(io.sent().len(), 1);
}

// ── what the senders read from the device (guard run.sh, "alert SMS language") ──

#[test]
fn screen_language_as_the_guard_and_the_screen_read_it() {
    assert!(!lang_en(""), "no ui.conf / empty: Chinese");
    assert!(!lang_en("lang=zh\n"));
    for v in [
        "", "fr", "1", "EN", "en ", " en", "en_US", "$((x+1))", "`reboot`", "zh;en",
    ] {
        assert!(
            !lang_en(&format!("theme=0\nlang={v}\n")),
            "lang={v:?} (not exactly en): Chinese"
        );
    }
    assert!(!lang_en("lang=en\r\n"), "lang=en with a CR: Chinese");
    assert!(lang_en("lang=en\n"));
    assert!(lang_en("theme=0\nlang=en"), "last line without a newline");
    assert!(
        lang_en("lang=zh\nlang=en\n"),
        "two lang= lines: the last wins (en)"
    );
    assert!(
        !lang_en("lang=en\nlang=zh\n"),
        "two lang= lines: the last wins (zh)"
    );
    assert!(
        !lang_en("xlang=en\n"),
        "a key that only ends in lang=: Chinese"
    );
}

#[test]
fn sntp_zone_in_whole_hours() {
    for (v, h) in [
        ("8.00", 8),
        ("8.00\n", 8),
        ("-5.30", -5),
        ("0", 0),
        ("", 0),
        ("x", 0),
        ("+8", 0), // the guard's case: not digits → 0
        ("8a", 0),
    ] {
        assert_eq!(tz_hours(v), h, "{v:?}");
    }
}

#[test]
fn newest_sent_copy_to_the_number() {
    let l = serde_json::json!({"messages": [
        {"id": "12", "number": "+861"},
        {"id": "11", "number": "+862"},
        {"id": "10", "number": "+862"}]});
    assert_eq!(
        newest_to(&l, "+862").as_deref(),
        Some("11"),
        "first match (id desc)"
    );
    assert_eq!(newest_to(&l, "+863"), None);
    for id in [
        serde_json::json!("1;x"),
        serde_json::json!(""),
        serde_json::json!(-1),
        serde_json::json!(1.5),
        serde_json::json!(null),
    ] {
        assert_eq!(
            newest_to(
                &serde_json::json!({"messages": [{"id": id, "number": "+1"}]}),
                "+1"
            ),
            None,
            "id {id}: never reaches the delete"
        );
    }
    assert_eq!(
        newest_to(
            &serde_json::json!({"messages": [{"id": 7, "number": "+1"}]}),
            "+1"
        )
        .as_deref(),
        Some("7")
    );
    assert_eq!(newest_to(&serde_json::json!({}), "+1"), None);
}

#[test]
fn tailnet_poller_is_never_an_sms() {
    let f = Facts {
        kind: "tailnet-poller",
        number: Some("+8613800000000"),
        abroad: false,
        sms_abroad: false,
        clock_ok: true,
        rate_ok: true,
    };
    assert!(matches!(verdict(&f), Verdict::Skip));
}

// ── regress (10-24) ──

/// One alert at a time, each round at its own wall clock: the sms-log results in order.
fn run_seq(name: &str, rows: &[(i64, &str)], off: bool) -> Vec<String> {
    let a = dir(name);
    a.write_whole("sms-to", "+8613800000000\n").unwrap();
    let mut io = Fake::new();
    io.off = off;
    for (t, k) in rows {
        io.wall = T0 + t;
        a.add(k, "text", T0 + t, 900).unwrap();
        a.sms_round("core", 10, &mut io);
    }
    log_results(&a).into_iter().map(|r| r.2).collect()
}

/// eng review R6 (the regression contract): kinds that are neither point events nor regress go
/// exactly as before, with the owner's regress switch on or off.
#[test]
fn other_kinds_go_as_before_whatever_the_regress_switch() {
    let rows: &[(i64, &str)] = &[
        (0, "dropbear-down"),
        (600, "dropbear-down"),
        (700, "home-exit-down"),
        (4000, "dropbear-down"),
        (4100, "sim-changed"),
        (4200, "esim-failed"),
        (4300, "wifi-restore-failed"),
        (4400, "tailnet-poller"),
    ];
    let want = [
        "sent",
        "suppressed-rate",
        "sent",
        "sent",
        "sent",
        "sent",
        "suppressed-rate",
    ];
    for off in [false, true] {
        assert_eq!(
            run_seq(&format!("contract-{off}"), rows, off),
            want,
            "off={off}"
        );
    }
}

/// eng review R4/R12: a crash row of 9 within the hour texts the 1st, 2nd, 4th and 8th; the
/// 2nd/4th/8th are not held by the hourly limit; one the sentinel queued counts the same.
#[test]
fn a_crash_row_texts_the_1st_2nd_4th_and_8th() {
    let a = dir("row");
    a.write_whole("sms-to", "+8613800000000\n").unwrap();
    let mut io = Fake::new();
    for i in 0..9i64 {
        io.wall = T0 + i * 300;
        if i == 2 {
            a.add_once("8559f5dc-7", "core-crash", "x", io.wall, 900)
                .unwrap();
        } else {
            a.add("core-crash", "x", io.wall, 900).unwrap();
        }
        a.sms_round("core", 10, &mut io);
    }
    let got: Vec<String> = log_results(&a).into_iter().map(|r| r.2).collect();
    assert_eq!(
        got,
        [
            "sent",
            "sent",
            "suppressed-repeat",
            "sent",
            "suppressed-repeat",
            "suppressed-repeat",
            "suppressed-repeat",
            "sent",
            "suppressed-repeat"
        ]
    );
    // a gap over an hour starts a new row
    io.wall += 3601;
    a.add("core-crash", "x", io.wall, 900).unwrap();
    a.sms_round("core", 10, &mut io);
    assert_eq!(log_results(&a).last().unwrap().2, "sent");
}

#[test]
fn escalations_still_count_against_the_day() {
    let a = dir("row-day");
    a.write_whole("sms-to", "+8613800000000\n").unwrap();
    let five: String = (0..5)
        .map(|i| format!("{}\t{}\tsim-changed\tsent\n", T0 - 5000 + i, 100 + i))
        .collect();
    a.write_whole("sms-log", &five).unwrap();
    let mut io = Fake::new();
    for i in 0..2i64 {
        io.wall = T0 + i * 300;
        a.add("ui-crash", "x", io.wall, 900).unwrap();
        a.sms_round("core", 10, &mut io);
    }
    let got: Vec<String> = log_results(&a).into_iter().skip(5).map(|r| r.2).collect();
    assert_eq!(got, ["suppressed-rate", "suppressed-rate"]);
    // a regress escalation goes by the day alone too
    let b = dir("esc-hour");
    b.write_whole("sms-to", "+8613800000000\n").unwrap();
    let mut io = Fake::new();
    b.add_once("rg-reboot-q5-open", "regress-reboot", "1119", T0, 900)
        .unwrap();
    b.sms_round("core", 10, &mut io);
    io.wall = T0 + 600;
    b.add_once(
        "rg-reboot-q5-esc2",
        "regress-reboot",
        "1119 x2",
        io.wall,
        900,
    )
    .unwrap();
    b.sms_round("core", 10, &mut io);
    let got: Vec<String> = log_results(&b).into_iter().map(|r| r.2).collect();
    assert_eq!(got, ["sent", "sent"]);
}

/// eng review R9: a regress alert the device refuses goes again 5, 10, 15 min after the first
/// refusal (outside the rate limits); a 4th refusal is `failed`; other kinds fail at once.
#[test]
fn a_refused_regress_alert_is_retried_three_times() {
    let a = dir("retry-ok");
    a.write_whole("sms-to", "+8613800000000\n").unwrap();
    let mut io = Fake::new();
    io.refuse = 2;
    a.add_once(
        "rg-mem-1790865000-open",
        "regress-mem",
        "heap 40MiB",
        T0,
        900,
    )
    .unwrap();
    for (dt, want_sent) in [(0, 0), (60, 0), (300, 0), (599, 0), (600, 1)] {
        io.wall = T0 + dt;
        a.sms_round("core", 10, &mut io);
        assert_eq!(io.sent().len(), want_sent, "at +{dt}");
    }
    assert_eq!(
        log_results(&a).into_iter().map(|r| r.2).collect::<Vec<_>>(),
        ["sent"]
    );
    assert!(!a.exists("sms-retry"));

    let b = dir("retry-fail");
    b.write_whole("sms-to", "+8613800000000\n").unwrap();
    let mut io = Fake::new();
    io.fail = true;
    b.add_once("rg-mem-1790865000-open", "regress-mem", "", T0, 900)
        .unwrap();
    for dt in [0, 300, 600, 900] {
        io.wall = T0 + dt;
        b.sms_round("core", 10, &mut io);
    }
    let r: Vec<String> = log_results(&b).into_iter().map(|r| r.2).collect();
    assert_eq!(r, ["failed"]);
    assert!(b.events().iter().any(|e| e.kind == "sms-failed"));

    let c = dir("retry-other");
    c.write_whole("sms-to", "+8613800000000\n").unwrap();
    let mut io = Fake::new();
    io.fail = true;
    c.add("dropbear-down", "x", T0, 900).unwrap();
    c.sms_round("core", 10, &mut io);
    let r: Vec<String> = log_results(&c).into_iter().map(|r| r.2).collect();
    assert_eq!(r, ["failed"]);
}

#[test]
fn closes_and_asks_are_inbox_only_and_the_switch_holds_regress_back() {
    let a = dir("close");
    a.write_whole("sms-to", "+8613800000000\n").unwrap();
    let mut io = Fake::new();
    a.add_once("rg-mem-1790865000-close", "regress-mem", "back", T0, 900)
        .unwrap();
    // the 3-day 「new normal?」 is the inbox's and MCP's too (design §4)
    a.add_once("rg-mem-1790865000-ask", "regress-mem", "open 3d", T0, 900)
        .unwrap();
    a.add_once("rg-cell-1790865000-open", "regress-cellular", "x", T0, 900)
        .unwrap();
    a.add_once(
        "rg-blind-power-1790865000-open",
        "regress-blind",
        "x",
        T0,
        900,
    )
    .unwrap();
    a.add("wifi-takeover", "x", T0, 900).unwrap();
    a.sms_round("core", 10, &mut io);
    assert!(io.sent().is_empty());
    assert!(log_results(&a).is_empty(), "skipped without a trace");
    assert_eq!(a.read_u64("sms-done"), 5);

    let b = dir("off");
    b.write_whole("sms-to", "+8613800000000\n").unwrap();
    let mut io = Fake::new();
    io.off = true;
    b.add_once("rg-mem-1790865000-open", "regress-mem", "x", T0, 900)
        .unwrap();
    b.add("core-crash", "x", T0, 900).unwrap();
    b.sms_round("core", 10, &mut io);
    let r: Vec<String> = log_results(&b).into_iter().map(|r| r.2).collect();
    assert_eq!(r, ["suppressed-off", "sent"]);
}

const REGRESS_TEXTED: &[&str] = &[
    "regress-reboot",
    "regress-mem",
    "regress-mem-abs",
    "regress-egress",
    "regress-power",
    "regress-power-abs",
    "regress-recorder",
    "regress-core-down",
    "regress-disk",
];

#[test]
fn regress_texts_are_template_details_and_where_the_recovery_shows() {
    assert_eq!(
        sms_text_for(
            "regress-mem",
            "[rg-mem-1790865000-open] heap 41MiB > 32MiB 15m; build 1.45.0",
            false,
            T0,
            7,
            None
        ),
        "【Luya-Z】数据服务内存比平时高很多，可能在泄漏。恢复消息见网页「告警」页 / Discord（10-01 14:32）#7"
    );
    assert_eq!(
        sms_text_for(
            "regress-reboot",
            "[rg-reboot-q9-open] ",
            true,
            T0,
            7,
            Some("U60")
        ),
        "[Luya-Z] U60: Unexpected reboot; back up now. recovery: web Alerts / Discord (01 Oct 14:32)#7"
    );
    // every other kind word for word as before
    for k in KINDS {
        for en in [false, true] {
            assert_eq!(
                sms_text_for(k, "[8559f5dc-1] whatever", en, T0, 9, Some("名字")),
                sms_text(k, en, T0, 9, Some("名字"))
            );
        }
    }
}

/// 10-26: a Chinese regress SMS carries no English details, only a ship just before it; the
/// English one keeps them. An unsettled clock prints no time; agent-silent has its own words.
#[test]
fn chinese_regress_texts_say_a_ship_and_unsettled_clocks_print_no_time() {
    let t = "[rg-reboot-x-open] code 1134; prev build 9fe962c; ship core before reboot?";
    assert_eq!(
        sms_text_for("regress-reboot", t, false, T0, 38, None),
        "【Luya-Z】U60意外重启了，现已恢复运行。之前刚换过core，可能有关。恢复消息见网页「告警」页 / Discord（10-01 14:32）#38"
    );
    assert!(sms_text_for("regress-reboot", t, true, T0, 38, None).contains("code 1134"));
    assert_eq!(
        sms_text_for(
            "regress-core-down",
            "[rg-core-down-a-1-open] /healthz no answer: refused for 10 min; attribution unknown",
            false,
            0,
            65,
            Some("U60Pro")
        ),
        "【Luya-Z】U60Pro：数据服务停了10分钟以上，屏幕数据可能不更新。恢复消息见网页「告警」页 / Discord#65"
    );
    // the space left and what holds it, in either language
    assert_eq!(
        sms_text_for(
            "regress-disk",
            "[rg-disk-data-a-1-open] /data 90/2000 MB free; luyaz/core 300M",
            false,
            0,
            70,
            None
        ),
        "【Luya-Z】存储空间快满了，程序可能写不进去而崩溃。/data 90/2000 MB free; luyaz/core 300M。恢复消息见网页「告警」页 / Discord#70"
    );
    assert_eq!(
        sms_text("agent-silent", false, 0, 64, None),
        "【Luya-Z】自动管理Wi-Fi的程序没反应了，Wi-Fi会保持打开，上网不受影响。#64"
    );
    assert!(sms_text("agent-silent", true, T0, 64, None).len() <= 70);
}

/// eng review round 3 (fact correction): every texted regress kind, both languages, a 16-character
/// device name, details empty or far too long: never over 134 characters, the template whole.
#[test]
fn regress_texts_fit_134_with_the_longest_name() {
    let name = "一二三四五六七八九十一二三四五六";
    assert_eq!(name.chars().count(), crate::NAME_MAX);
    let long = format!("[rg-x-1-open] {}", "detail ".repeat(40));
    for k in REGRESS_TEXTED {
        for en in [false, true] {
            for text in ["[rg-x-1-open] ", long.as_str()] {
                let t = sms_text_for(k, text, en, T0, 99_999, Some(name));
                assert!(
                    t.chars().count() <= REGRESS_MAX,
                    "{k} en={en}: {} chars: {t}",
                    t.chars().count()
                );
                let fixed = if en { regress_en(k) } else { regress_zh(k) }.unwrap();
                assert!(t.contains(fixed), "{t}");
                assert!(t.contains(if en { TAIL_EN } else { TAIL_ZH }), "{t}");
                assert!(t.ends_with("#999"), "{t}");
                if en && text.len() > 20 {
                    assert!(t.contains('…'), "cut: {t}");
                }
            }
        }
    }
}
