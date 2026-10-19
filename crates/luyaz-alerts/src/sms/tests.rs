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
            kind: "devui-theme-paused",
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
    "devui-crash",
    "devui-gave-up",
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
        sms_text("web-crash", false, T0, 7),
        "【Luya-Z】管理网页意外退出，已自动重启。不用管。（10-01 14:32）#7"
    );
    assert_eq!(
        sms_text("web-crash", true, T0, 7),
        "[Luya-Z] Admin pages crashed; restarted; no action (01 Oct 14:32)#7"
    );
    assert_eq!(
        sms_text("x-y", false, T0, 1),
        "【Luya-Z】有新告警（x-y），请到管理网页「系统→告警」查看。（10-01 14:32）#1"
    );
    assert_eq!(
        sms_text("a$b", true, T0, 1),
        "[Luya-Z] Alert ab; see Alerts on web (01 Oct 14:32)#1"
    );
}

#[test]
fn every_text_is_one_sms_with_a_long_code() {
    for k in KINDS {
        for en in [false, true] {
            let t = sms_text(k, en, T0, 99_999);
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
    let t = sms_text("some-very-long-unknown-kind-name-here", true, T0, 999);
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
    let a = setup("happy", &["web-crash", "sms-failed", "devui-crash"]);
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
            (3, "devui-crash".into(), "sent".into())
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
    let a = setup("fail", &["web-crash"]);
    let mut io = Fake::new();
    io.fail = true;
    assert_eq!(a.sms_round("core", 10, &mut io), Round::Done(2));
    assert_eq!(
        log_results(&a),
        vec![(1, "web-crash".into(), "failed".into())]
    );
    let ev = a.events();
    assert_eq!(ev.last().unwrap().kind, "sms-failed");
    assert_eq!(ev.last().unwrap().text, "alert 1 (web-crash) not sent");
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
    let a = setup("en", &["devui-crash"]);
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
            "devui-crash",
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
    assert!(!lang_en(""), "no devui.conf / empty: Chinese");
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
