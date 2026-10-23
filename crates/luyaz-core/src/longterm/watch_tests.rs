use super::*;

/// A device under a temp root: /proc entries, crash files, logs.
struct Dev {
    root: PathBuf,
    p: Paths,
}

impl Dev {
    fn new(tag: &str) -> Dev {
        let root = std::env::temp_dir().join(format!("u60d-watch-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["proc", "crashlog", "long", "guard", "tmp"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let p = Paths {
            proc_: root.join("proc"),
            crashlog: root.join("crashlog"),
            uid_log: root.join("tmp/luyaz-screen.log"),
            recovery: root.join("recovery"),
            recovery_set: root.join("tmp/recovery-set"),
            ship_txn: root.join("txn"),
            guard_dir: root.join("guard"),
            long: root.join("long"),
            self_pid: 100,
            ubus: "socket".into(),
        };
        let d = Dev { root, p };
        d.proc(100, "luyaz-core", 5, b"core-1");
        d
    }

    /// A process: comm, stat with start time `st`, exe contents.
    fn proc(&self, pid: u32, comm: &str, st: u64, exe: &[u8]) {
        let d = self.p.proc_.join(pid.to_string());
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("comm"), format!("{comm}\n")).unwrap();
        let mut stat = format!("{pid} ({comm}) S");
        for _ in 0..18 {
            stat.push_str(" 0");
        }
        stat.push_str(&format!(" {st} 0 0\n"));
        std::fs::write(d.join("stat"), stat).unwrap();
        std::fs::write(d.join("exe"), exe).unwrap();
    }

    fn gone(&self, pid: u32) {
        let _ = std::fs::remove_dir_all(self.p.proc_.join(pid.to_string()));
    }

    fn write(&self, rel: &str, text: &str) {
        let f = self.root.join(rel);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, text).unwrap();
    }

    /// One look, its file steps done (as the long-term thread does after writing the lines).
    fn look(&self, st: &mut State, up: i64) -> Look {
        let lk = st.look(&self.p, "b1b1b1b1-0000", up);
        commit(&self.p.long, &lk.after).unwrap();
        lk
    }
}

impl Drop for Dev {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn of<'a>(lk: &'a Look, k: &str) -> Vec<&'a Value> {
    lk.lines
        .iter()
        .filter(|(x, _)| *x == k)
        .map(|(_, v)| v)
        .collect()
}

#[test]
fn stat_start_time_and_comm_search() {
    let d = Dev::new("stat");
    d.proc(7, "tailscaled", 1234, b"a");
    d.proc(3, "tailscaled", 99, b"b");
    assert_eq!(start_time(&d.p.proc_, 7), Some(1234));
    assert_eq!(start_time(&d.p.proc_, 8), None);
    // the first hit by pid, as the guard's find_pid over /proc/[0-9]*
    assert_eq!(find_pids(&d.p.proc_, &["tailscaled"])["tailscaled"], 3);
    // the core's card child (a lower pid after a wrap) is not the core
    d.proc(40, "luyaz-core", 500, b"d");
    d.proc(2, "luyaz-core", 900, b"d");
    let st = d.p.proc_.join("2/stat");
    let t = std::fs::read_to_string(&st)
        .unwrap()
        .replacen("S 0", "S 40", 1);
    std::fs::write(&st, t).unwrap();
    assert_eq!(find_pids(&d.p.proc_, &["luyaz-core"])["luyaz-core"], 40);
    // a comm with ") " in it does not move the fields
    d.proc(9, "a) b", 42, b"c");
    assert_eq!(start_time(&d.p.proc_, 9), Some(42));
}

#[test]
fn ver_once_a_boot_then_on_a_new_build() {
    let d = Dev::new("ver");
    d.proc(200, "tailscaled", 10, b"ts-1");
    d.write("guard/doctor.sh", "#!/bin/sh\n");
    let mut st = State::default();
    let lk = d.look(&mut st, 60);
    let v = of(&lk, "ver");
    let get = |p: &str| v.iter().find(|x| x["prog"] == p).copied().unwrap().clone();
    assert_eq!(get("tailscaled")["md5"], md5_hex(b"ts-1", 8));
    assert_eq!(get("tailscaled")["pid"], 200);
    assert_eq!(get("luyaz-core")["extra"], "ubus=socket");
    assert_eq!(
        get("luyaz-ui")["md5"],
        Value::Null,
        "not running: written as such"
    );
    assert_eq!(get("doctor.sh")["how"], "file");
    assert!(
        !v.iter().any(|x| x["prog"] == "alert-lib.sh"),
        "no file, no line"
    );
    assert!(of(&d.look(&mut st, 120), "ver").is_empty());
    // restarted, same build: no ver (a proc_restart, below)
    d.gone(200);
    d.proc(201, "tailscaled", 500, b"ts-1");
    assert!(of(&d.look(&mut st, 180), "ver").is_empty());
    // a new build
    d.gone(201);
    d.proc(202, "tailscaled", 600, b"ts-2");
    let lk = d.look(&mut st, 240);
    let v = of(&lk, "ver");
    assert_eq!(v.len(), 1);
    assert_eq!(v[0]["md5"], md5_hex(b"ts-2", 8));
    // the touch UI comes up later: found on the rescan, md5 differs from "not running"
    d.proc(300, "luyaz-ui", 700, b"ui");
    assert!(
        of(&d.look(&mut st, 300), "ver").is_empty(),
        "rescan not due yet"
    );
    // (the last search by name was at 240, when tailscaled's pid changed)
    assert!(of(&d.look(&mut st, 239 + RESCAN_S), "ver").is_empty());
    let v = d.look(&mut st, 240 + RESCAN_S);
    assert_eq!(of(&v, "ver").len(), 1);
}

#[test]
fn proc_restart_with_crashlog_and_ship() {
    let d = Dev::new("pr");
    let mut st = State::default();
    d.look(&mut st, 60);
    // ours: a new process of this boot resumes the state with the old pid
    let mut st2 = st.clone();
    let mut p2 = Paths {
        self_pid: 150,
        ..Paths::device()
    };
    p2.proc_ = d.p.proc_.clone();
    p2.crashlog = d.p.crashlog.clone();
    p2.long = d.p.long.clone();
    p2.uid_log = d.p.uid_log.clone();
    p2.recovery = d.p.recovery.clone();
    p2.ship_txn = d.p.ship_txn.clone();
    p2.guard_dir = d.p.guard_dir.clone();
    d.proc(150, "luyaz-core", 1100, b"core-2");
    d.write(
        "crashlog/luyaz-core/20261010-up1080.log",
        "program: luyaz-core\nstatus: signal 11\n",
    );
    // restarted by a deployment of the core that is still running
    d.write(
        "txn",
        "boot_id=b1b1b1b1-0000\ncomp=core\nphase=check\nt_phase=1100\n",
    );
    let lk = st2.look(&p2, "b1b1b1b1-0000", 1200);
    let pr = of(&lk, "proc_restart");
    assert_eq!(pr.len(), 1);
    assert_eq!(pr[0]["prog"], "luyaz-core");
    assert_eq!(pr[0]["id"], "pr-b1b1b1b1-150-1100");
    assert_eq!(pr[0]["crashlog"], 1);
    assert_eq!(pr[0]["ship"], 1);
    let sx = of(&lk, "svc_exit");
    assert_eq!(sx.len(), 1);
    assert_eq!(sx[0]["status"], "signal 11");
    assert_eq!(sx[0]["crash_up"], 1080);
    assert_eq!(sx[0]["found"], "round");
    assert_eq!(
        sx[0]["id"],
        format!(
            "cl-{}",
            md5_hex(b"program: luyaz-core\nstatus: signal 11\n", 12)
        )
    );
    assert_eq!(
        (pr[0]["old"].clone(), pr[0]["new"].clone()),
        (json!(100), json!(150))
    );
    let v = of(&lk, "ver");
    assert_eq!(v.len(), 1, "a new build of ours: {v:?}");
}

#[test]
fn svc_exit_first_go_live_takes_what_is_there() {
    let d = Dev::new("sx");
    d.write("crashlog/luyaz-ui/old-up10.log", "status: exit 1\n");
    let mut st = State::default();
    let lk = d.look(&mut st, 60);
    assert!(of(&lk, "svc_exit").is_empty());
    assert!(d.p.long.join("crashlog.seen").exists());
    assert!(of(&d.look(&mut st, 120), "svc_exit").is_empty());
    // a file that grew is new again (name + size), as the guard's list
    d.write(
        "crashlog/luyaz-ui/old-up10.log",
        "status: exit 1\nmore\n",
    );
    assert_eq!(of(&d.look(&mut st, 180), "svc_exit").len(), 1);
    // not readable: crashlog down
    std::fs::remove_dir_all(&d.p.crashlog).unwrap();
    let lk = d.look(&mut st, 240);
    assert!(lk.cov.contains(&("crashlog".into(), false)));
}

#[test]
fn svc_exit_at_boot_says_boot_init() {
    let d = Dev::new("sxb");
    d.write("long/crashlog.seen", "");
    d.write("crashlog/tailscaled/a-up5.log", "status: signal 9\n");
    let mut st = State::default();
    let lk = d.look(&mut st, 60);
    assert_eq!(of(&lk, "svc_exit")[0]["found"], "boot_init");
}

#[test]
fn uid_lines_numbered_over_the_boot_through_a_rename() {
    let d = Dev::new("uid");
    let mut st = State::default();
    let lk = d.look(&mut st, 60);
    assert!(lk.cov.contains(&("uidlog".into(), false)), "no log yet");
    d.write(
        "tmp/luyaz-screen.log",
        "10:00:01 started\n10:00:02 giving up after 2 failures\n10:00:03 request: back\n",
    );
    let lk = d.look(&mut st, 120);
    let u = of(&lk, "uid");
    assert_eq!(u.len(), 2);
    assert_eq!(u[0]["id"], "uid-b1b1b1b1-2");
    assert_eq!(u[0]["what"], "gave_up");
    assert_eq!(u[1]["what"], "other");
    assert!(lk.cov.contains(&("uidlog".into(), true)));
    // renamed after one more line; a new file starts
    d.write(
        "tmp/luyaz-screen.log.old",
        "10:00:01 started\n10:00:02 giving up after 2 failures\n10:00:03 request: back\n10:00:04 starting the vendor UI\n",
    );
    d.write("tmp/luyaz-screen.log", "10:01:00 corner long-press: back\n");
    let lk = d.look(&mut st, 180);
    let ids: Vec<&Value> = of(&lk, "uid").iter().map(|v| &v["id"]).collect();
    assert_eq!(ids, [&json!("uid-b1b1b1b1-4"), &json!("uid-b1b1b1b1-5")]);
    assert!(of(&d.look(&mut st, 240), "uid").is_empty());
}

#[test]
fn recovery_first_and_falling_back() {
    let d = Dev::new("rec");
    let mut st = State::default();
    d.write("recovery", "enabled\n");
    d.write("tmp/recovery-set", "");
    let r = d.look(&mut st, 60);
    let v = of(&r, "recovery_seen");
    assert_eq!(v[0]["value"], "enabled");
    assert_eq!(v[0]["set_by_guard"], 1);
    assert!(of(&d.look(&mut st, 120), "recovery_seen").is_empty());
    d.write("recovery", "disabled\n");
    assert_eq!(
        of(&d.look(&mut st, 180), "recovery_seen")[0]["value"],
        "disabled"
    );
    d.write("recovery", "enabled\n");
    assert!(of(&d.look(&mut st, 240), "recovery_seen").is_empty());
}

/// Every program running: nothing is "missing", no /proc search every RESCAN_S (the retired agent
/// is no longer looked for).
#[test]
fn nothing_missing_no_rescan() {
    let d = Dev::new("ret1");
    d.proc(301, "mihomo", 10, b"m");
    d.proc(302, "luyaz-ui", 10, b"ui");
    d.proc(304, "tailscaled", 10, b"ts");
    let mut st = State::default();
    d.look(&mut st, 60);
    assert_eq!(st.scanned, Some(60));
    d.look(&mut st, 60 + RESCAN_S);
    assert_eq!(st.scanned, Some(60));
}

#[test]
fn the_core_is_found_by_prefix() {
    let d = Dev::new("corename");
    d.gone(100);
    d.proc(30, "luyaz-core.test", 700, b"c");
    d.proc(31, "otherd", 701, b"u");
    let got = find_pids(&d.p.proc_, &PROGS);
    assert_eq!(got.get("luyaz-core"), Some(&30));
    assert!(!got.contains_key("otherd"));
    assert!(!got.contains_key("luyaz-core.test"));
    d.proc(20, "luyaz-core", 650, b"c");
    assert_eq!(find_pids(&d.p.proc_, &RESTARTS)["luyaz-core"], 20);
}

#[test]
fn ship_restart_is_the_cores_own_deployment() {
    let t = "comp=core\nboot_id=b\nphase=trial\nt_phase=100\n";
    assert_eq!(ship_restart(Some(t), "luyaz-core", "b", 150), 1);
    let t = "comp=sentinel\nboot_id=b\nphase=trial\nt_phase=100\n";
    assert_eq!(ship_restart(Some(t), "luyaz-core", "b", 150), 0);
}
