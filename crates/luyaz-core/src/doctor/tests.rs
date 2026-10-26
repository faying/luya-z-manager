//! The doctor on a stubbed device: `testdata/setup.sh` builds the fixture (every command and
//! path a `DOC_*` override), a scenario changes it, and the rows are compared with the goldens
//! (`testdata/golden/`) and with snapshots (`testdata/snap/`). The snapshots started as the
//! retired doctor-legacy.sh's output, row for row (Luya-Z rename F4 dropped the shell copy);
//! after a deliberate change, rewrite them with `DOCTOR_SNAP_UPDATE=1 cargo test -p luyaz-core
//! doctor::` and read the diff.

use super::*;
use std::collections::HashMap;
use std::path::PathBuf;

fn testdata() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/doctor/testdata")
}

struct Fx {
    dir: PathBuf,
    env: HashMap<String, String>,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn sh(script: &str, env: &HashMap<String, String>) -> String {
    let o = Command::new("sh")
        .arg("-c")
        .arg(script)
        .envs(env)
        .stderr(Stdio::inherit())
        .output()
        .expect("sh");
    String::from_utf8_lossy(&o.stdout).trim().to_owned()
}

fn load_env(dir: &std::path::Path) -> HashMap<String, String> {
    std::fs::read_to_string(dir.join("doc.env"))
        .expect("doc.env")
        .lines()
        .filter_map(|l| l.split_once('=').map(|(k, v)| (k.to_owned(), v.to_owned())))
        .collect()
}

/// setup.sh's `setup`, then `scenario` (sh, with `$T` the fixture).
fn fixture(scenario: &str) -> Fx {
    let script = format!(
        ". '{}'\nsetup\n{scenario}\nenv | grep '^DOC_' >\"$T/doc.env\"\necho \"$T\"\n",
        testdata().join("setup.sh").display()
    );
    let dir = PathBuf::from(sh(&script, &HashMap::new()));
    let env = load_env(&dir);
    Fx { dir, env }
}

/// More sh on a fixture (its DOC_* exported, `$T` set): `extra` adds variables first.
fn then(fx: &mut Fx, extra: &[(&str, String)], script: &str) {
    let mut env = fx.env.clone();
    env.insert("T".into(), fx.dir.to_string_lossy().into_owned());
    for (k, v) in extra {
        env.insert((*k).into(), v.clone());
    }
    sh(
        &format!("{script}\nenv | grep '^DOC_' >\"$T/doc.env\"\n"),
        &env,
    );
    fx.env = load_env(&fx.dir);
}

fn cfg(fx: &Fx) -> Cfg {
    Cfg::from(&|k| fx.env.get(k).cloned())
}

/// `cfg` with some overrides (what the old fixtures passed to one doctor.sh run)
fn cfg_with(fx: &Fx, over: &[(&str, &str)]) -> Cfg {
    Cfg::from(&|k| {
        over.iter()
            .find(|(o, _)| *o == k)
            .map(|(_, v)| (*v).to_owned())
            .or_else(|| fx.env.get(k).cloned())
    })
}

fn rust(fx: &Fx, mode: Mode) -> (String, i32) {
    full(&cfg(fx), mode)
}

/// `--calibrate-standby` on the fixture as the old scenarios ran it (no scorecard fetch)
fn calibrate(fx: &Fx) {
    calibrate_with(fx, &[]);
}

fn calibrate_with(fx: &Fx, over: &[(&str, &str)]) {
    let mut o = vec![("DOC_WGET", "/bin/false")];
    o.extend_from_slice(over);
    standby::calibrate(&cfg_with(fx, &o));
}

// The clock row is judged on live evidence (P0-01: the boot id is the host's), so the goldens
// and snapshots leave it out, and text mode's closing count with it.
fn without_clock(text: &str) -> String {
    let lines: Vec<_> = text.lines().collect();
    let text_mode = lines.iter().any(|l| l.contains("时钟："));
    if text_mode {
        // Validate each complete summary against its own visible row levels first.
        let bad = lines.iter().filter(|l| l.starts_with("  ■ ")).count();
        let warn = lines.iter().filter(|l| l.starts_with("  ▲ ")).count();
        let expected = if bad == 0 && warn == 0 {
            "  一切正常".into()
        } else {
            format!("  {bad} 项异常，{warn} 项需要注意")
        };
        assert_eq!(lines.last().copied(), Some(expected.as_str()));
    }
    lines
        .iter()
        .enumerate()
        .filter(|(i, l)| {
            !l.contains("\tclock\t")
                && !l.contains("时钟：")
                && !(text_mode && *i == lines.len() - 1)
        })
        .map(|(_, l)| format!("{l}\n"))
        .collect()
}

/// One scenario's whole output: --tsv, --tsv2, text (each with its exit code) and --manifest,
/// the fixture's path as `$T`.
fn all_modes(fx: &Fx) -> String {
    let mut out = String::new();
    for (mode, name) in [
        (Mode::Tsv, "--tsv"),
        (Mode::Tsv2, "--tsv2"),
        (Mode::Text, "text"),
    ] {
        let (t, rc) = rust(fx, mode);
        out.push_str(&format!("--- {name} rc={rc}\n{}", without_clock(&t)));
    }
    out.push_str(&format!("--- --manifest\n{}", manifest::report(&cfg(fx))));
    // rc.local holds the fixture's path, so its md5 changes with every run: name it instead
    // (setup's rc.local as `<rc0>`, the one the scenario left as `<rc>`)
    for (f, name) in [("rc.orig", "<rc0>"), ("rc", "<rc>")] {
        if let Ok(b) = std::fs::read(fx.dir.join(f)) {
            out = out.replace(&manifest::md5_hex(&b), name);
        }
    }
    out.replace(&*fx.dir.to_string_lossy(), "$T")
}

/// Compare with `testdata/snap/<name>.txt`, or write it under DOCTOR_SNAP_UPDATE=1.
fn snap(name: &str, cases: &[(String, String)]) {
    let mut text = String::new();
    for (scenario, out) in cases {
        text.push_str("=== scenario\n");
        text.push_str(scenario.trim_end());
        text.push('\n');
        text.push_str(out);
        if !out.ends_with('\n') {
            text.push('\n');
        }
    }
    let path = testdata().join("snap").join(format!("{name}.txt"));
    if std::env::var_os("DOCTOR_SNAP_UPDATE").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &text).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    if want != text {
        // the first case that differs, so the message stays readable
        let (w, t): (Vec<_>, Vec<_>) = (
            want.split("=== scenario\n").collect(),
            text.split("=== scenario\n").collect(),
        );
        for (a, b) in w.iter().zip(&t) {
            assert_eq!(b, a, "{}", path.display());
        }
        assert_eq!(t.len(), w.len(), "{}: case count", path.display());
    }
}

macro_rules! fx_or_skip {
    ($s:expr) => {
        fixture($s)
    };
}

const PROBLEMS: &str = r#"echo "zte_topsw_devui" >"$T/sync"; echo 1 >"$T/mode"; echo 0 >"$T/aps"
touch "$T/down-luyaz-sentinel"; echo 500 >"$T/hb"; touch "$T/uid/gave-up"; : >"$T/alerts/sms-to"
grep -v "luyaz-core start" "$T/rc" >"$T/rc2"; mv "$T/rc2" "$T/rc"
printf '9\t1\tcrash\tx\ty\n' >"$T/alerts/queue""#;

fn golden(name: &str) -> String {
    std::fs::read_to_string(testdata().join(name)).unwrap_or_default()
}

const STANDBY_PRE: &str = r#"export DOC_STANDBY_STAT=$T/sb DOC_STANDBY_BASE=$T/sb.base DOC_LEDGER_DIR=$T/ledger DOC_BOOT_ID_FILE=$T/bootid
echo aaaaaaaa-0000-4000-8000-000000000001 >"$T/bootid"
i=0; while [ $i -lt 40 ]; do echo "$((100 + i * 20)) $((50 + i)) 20 30.0 5.0 2.0 1.0" >>"$T/sb"; i=$((i + 1)); done"#;
const STANDBY_POST: &str = r#": >"$T/sb"; i=0; while [ $i -lt 10 ]; do echo "$((300 + i * 60)) 200 20 30.0 5.0 2.0 1.0" >>"$T/sb"; i=$((i + 1)); done
unset DOC_LEDGER_DIR DOC_BOOT_ID_FILE"#;

fn golden_fixture(name: &str) -> Fx {
    match name {
        "problems" => fixture(PROBLEMS),
        "standby" => {
            let mut fx = fixture(STANDBY_PRE);
            calibrate(&fx);
            then(&mut fx, &[], STANDBY_POST);
            fx
        }
        _ => fixture(""),
    }
}

#[test]
fn the_goldens_byte_for_byte() {
    let update = std::env::var_os("DOCTOR_SNAP_UPDATE").is_some();
    // the standby golden's records carry the private mihomo column
    for name in ["healthy", "problems", "standby"] {
        let fx = golden_fixture(name);
        let (t, rc) = rust(&fx, Mode::Tsv);
        let text = rust(&fx, Mode::Text).0;
        assert_eq!(rc, if name == "problems" { 1 } else { 0 });
        if update {
            std::fs::write(testdata().join(format!("golden/{name}.tsv")), &t).unwrap();
            std::fs::write(testdata().join(format!("golden/{name}.txt")), &text).unwrap();
            if name == "problems" {
                let t2 = rust(&fx, Mode::Tsv2).0;
                std::fs::write(testdata().join("tsv2-sample.tsv"), t2).unwrap();
            }
            continue;
        }
        assert_eq!(
            without_clock(&t),
            without_clock(&golden(&format!("golden/{name}.tsv"))),
            "{name}.tsv"
        );
        assert_eq!(
            without_clock(&text),
            without_clock(&golden(&format!("golden/{name}.txt"))),
            "{name}.txt"
        );
        if name == "problems" {
            assert_eq!(
                without_clock(&rust(&fx, Mode::Tsv2).0),
                without_clock(&golden("tsv2-sample.tsv"))
            );
        }
    }
}

#[test]
fn scenarios() {
    let scenarios = [
        // healthy / problems (the goldens)
        "",
        PROBLEMS,
        // boot-sync unreadable, uci gives nothing
        r#"printf '#!/bin/sh\n' >"$T/bin/ubus"; : >"$T/mode"; : >"$T/poll""#,
        // two lines with the name, one with two names (sed: the last per line)
        r#"printf '#!/bin/sh\necho "{ \\"noSyncModuleName\\": \\"a\\", \\"noSyncModuleName\\":\\"b\\" }"\necho "\\"noSyncModuleName\\": \\"c\\""\n' >"$T/bin/ubus""#,
        r#"echo 0 >"$T/poll""#,
        // crashes : read alerts, then unread; nested, dotted and old files
        r#"touch "$T/crash/x.log""#,
        r#"touch "$T/crash/x.log"; printf '9\t1\tcrash\tx\ty\n' >"$T/alerts/queue""#,
        r#"touch "$T/crash/x.log"; printf '9\t1\tcrash\tx\ty\n' >"$T/alerts/queue"; echo 9 >"$T/alerts/read""#,
        r#"mkdir -p "$T/crash/a"; touch "$T/crash/a/y.log" "$T/crash/.log" "$T/crash/z.txt"; touch -d '3 days ago' "$T/crash/old.log"; printf '7x\t1\tc\tx\ty\n3\tshort\n' >"$T/alerts/queue"; echo 5 >"$T/alerts/read""#,
        // clock
        r#"echo 1735948800 >"$T/now""#,
        r#"echo 1735948800 >"$T/now"; echo "100.0 1" >"$T/uptime""#,
        r#"echo "0 1790499000 990 none" >"$T/clock-ok""#,
        r#"echo "1 1790499000 990 none" >"$T/clock-ok"; echo 1735948800 >"$T/now""#,
        r#"echo "0 1790499000 100 none" >"$T/clock-ok""#,
        r#"echo $((1790500000 + 3 * 86400)) >"$T/wall.last""#,
        r#"echo $((1790500000 + 86400)) >"$T/wall.last""#,
        // (a garbage /proc/uptime is left out: the shell's later rows do arithmetic on it and dash
        // stops there, so crashes/disk never print; the Rust clock row agrees with the shell's)
        r#"rm -f "$T/uptime"; echo x >"$T/now"; echo "1 2 3" >"$T/clock-ok""#,
        // services: the sentinel down, luyaz-web (in rc.local, down; or only its program), no
        // procd services at all, a second core and touch UI (side-by-side names count), the
        // sentinel not in rc.local
        r#"touch "$T/down-luyaz-sentinel""#,
        r#"sed -i "s#.*luyaz-core start#&\\n$T/initd/luyaz-web start#" "$T/rc"; cp "$T/initd/luyaz-core" "$T/initd/luyaz-web"; touch "$T/down-luyaz-web""#,
        r#": >"$T/luyaz-web"; export DOC_LWEB_BIN=$T/luyaz-web; cp "$T/initd/luyaz-core" "$T/initd/luyaz-web""#,
        r#"rm -f "$T"/initd/*"#,
        r#"mkdir -p "$T/proc/102" "$T/proc/104"; echo luyaz-core.test >"$T/proc/102/comm"; echo luyaz-ui.test >"$T/proc/104/comm""#,
        r#"sed -i "s#^$T/initd/luyaz-sentinel start#\# &#" "$T/rc""#,
        // heartbeat, takeover, Wi-Fi, screen
        r#"rm -f "$T/hb""#,
        r#"echo 880 >"$T/hb"; touch "$T/marker""#,
        r#"echo 0 >"$T/aps"; echo '{"ok":true,"data":{"scenario":{"current":"home","wifi_off":true}}}' >"$T/public""#,
        r#"echo 0 >"$T/aps""#,
        r#"printf '#!/bin/sh\necho "1 root 0 S /usr/sbin/hostapd"\necho "2 root 0 S awk /hostapd"\necho "3 root 0 S /x/hostapdx"\n' >"$T/bin/ps""#,
        r#"echo vendor >"$T/uid.want"; rm -f "$T/proc/100/comm""#,
        r#"echo 2 >"$T/uid/attempts""#,
        r#"rm -f "$T/proc/100/comm""#,
        // alerts, SMS
        r#"printf '9\t1\tcrash\tx\ty\n10\t1\tb\tx\ty\n' >"$T/alerts/queue"; echo 9 >"$T/alerts/read"; rm -f "$T/alerts/sms-to""#,
        // regress SMS turned off (2026-10-24 X8): set up but off; no number wins
        r#"touch "$T/regress-sms-off""#,
        r#"touch "$T/regress-sms-off"; rm -f "$T/alerts/sms-to""#,
        // whiteouts: on a boot-barrier daemon, unrelated ones, no overlay
        r#"mknod "$T/rcd/S99zte_topsw_devui" c 0 0"#,
        r#"mknod "$T/rcd/K10foo" c 0 0; mknod "$T/rcd/S20bar" c 0 0; touch "$T/rcd/S30plain""#,
        r#"mknod "$T/rcd/K10foo" c 0 0; mknod "$T/rcd/K05zte_topsw_wlan" c 0 0"#,
        r#"rm -rf "$T/rcd""#,
        // phone-home
        r#"mkdir -p "$T/proc/200" "$T/proc/201"; echo zte_mqtt_sdk_st >"$T/proc/200/comm"; echo zte_smart_manag >"$T/proc/201/comm""#,
        r#"printf "zwrt_mqtt.config.mqttOnreportEnable='0'\n" >"$T/mqtt"; printf "zwrt_tr069.ManagementServer.EnableCWMP='1'\nzwrt_tr069.ManagementServer.URL='http://acs.example'\n" >"$T/tr069"; rm -f "$T/dadian""#,
        r#"rm -f "$T/mqtt" "$T/tr069""#,
        r#"printf "zwrt_mqtt.config.mqttOnreportEnable='1'x\n" >"$T/mqtt"; printf "zwrt_tr069.ManagementServer.EnableCWMP='0'\n" >"$T/tr069"; printf "a.switch=1\nb.switch='1''\nc.switch='10'\n" >"$T/dadian"; mkdir -p "$T/proc/202"; echo zte_topsw_tr069 >"$T/proc/202/comm""#,
        // HTTP down
        r#"printf '#!/bin/sh\nexit 1\n' >"$T/bin/wget""#,
        r#"export DOC_DATAD_URL=http://127.0.0.1:1/none; printf '#!/bin/sh\ncase "$*" in *1/none*) exit 1 ;; esac\nexit 0\n' >"$T/bin/wget""#,
        // disk: bad, warn, unreadable, wrapped row, nothing
        r#"printf '#!/bin/sh\necho "F 1K Used Avail Use%% Mounted"\necho "/dev/ubi0 4000000 2800000 10000 99%% /data"\n' >"$T/bin/df""#,
        r#"printf '#!/bin/sh\necho "F 1K Used Avail Use%% Mounted"\necho "/dev/ubi0 4000000 2800000 50000 99%% /data"\n' >"$T/bin/df""#,
        r#"printf '#!/bin/sh\necho "F 1K Used Avail Use%% Mounted"\necho "/dev/ubi0 4000000 2800000 n/a 99%% /data"\n' >"$T/bin/df""#,
        r#"printf '#!/bin/sh\necho "F 1K Used Avail Use%% Mounted"\necho "/dev/a-very-long-device-name"\necho "  4000000 2800000 204800 70%% /data"\n' >"$T/bin/df""#,
        r#"printf '#!/bin/sh\nexit 1\n' >"$T/bin/df""#,
        r#"printf '#!/bin/sh\necho "a b"\n' >"$T/bin/df""#,
    ];
    let cases: Vec<_> = scenarios
        .iter()
        .map(|s| (s.to_string(), all_modes(&fixture(s))))
        .collect();
    snap("scenarios", &cases);
}

#[test]
fn chill_rows() {
    let base = r#"mkdir -p "$T/proc/net"
printf '#!/bin/sh\ncase "$*" in *public/status*) cat %s/public 2>/dev/null ;; *%%E9%%87%%8C) cat %s/proxy 2>/dev/null ;; esac\nexit 0\n' "$T" "$T" >"$T/bin/wget"
printf '#!/bin/sh\ncat %s/iproute 2>/dev/null\n' "$T" >"$T/bin/ip"; chmod +x "$T/bin/wget" "$T/bin/ip"
export DOC_IP=$T/bin/ip DOC_CHILL_STATE=$T/chill.state
"#;
    let run_ts = r#"printf "CHILL_HOME_EXIT='192.0.2.10:1080'\n" >"$T/chill.env"; echo '{"state":"running","reason":null}' >"$T/chill.state"; echo "192.0.2.10 dev tailscale0 table 52" >"$T/iproute""#;
    let hist = |t: &str, d: &str| {
        format!(
            r#"{run_ts}; printf '{{"alive":true,"extra":{{"http://x/204":{{"history":[{{"time":"2026-09-27T08:00:00Z","delay":1}}]}}}},"history":[{{"time":"2026-09-27T08:00:00.1Z","delay":2}},{{"time":"{t}","delay":{d}}}],"name":"x"}}' >"$T/proxy""#
        )
    };
    let mut scenarios: Vec<String> = [
        "",
        r#": >"$T/chill.env""#,
        r#": >"$T/chill.env"; printf '  sl  local_address rem_address   st\n   0: 0100007F:270F 00000000:0000 0A 0\n   1: 00000000:0050 00000000:0000 0A 0\n   2: 0142000A:270F 0A00000A:D431 01 0\n' >"$T/proc/net/tcp""#,
        r#": >"$T/chill.env"; printf '  sl  local_address rem_address   st\n   0: 00000000:270F 00000000:0000 0A 0\n   1: 0142000A:270F 0:0 0A 0\n' >"$T/proc/net/tcp"; printf '  sl  local_address rem_address   st\n   0: 00000000000000000000000000000000:270F 00000000000000000000000000000000:0000 0A 0\n' >"$T/proc/net/tcp6""#,
        r#"printf "SUB_URL='x'\nCHILL_HOME_EXIT='192.0.2.10'\n" >"$T/chill.env""#,
        r#"printf "CHILL_HOME_EXIT=\"1.2.3.4:5:6\"\n" >"$T/chill.env""#,
        r#"printf "CHILL_HOME_EXIT=1.2.3.4:80  \nCHILL_HOME_EXIT='x' y\n" >"$T/chill.env""#,
        r#"printf "CHILL_HOME_EXIT='192.0.2.10:1080'\n" >"$T/chill.env""#,
        r#"printf "CHILL_HOME_EXIT='192.0.2.10:1080'\n" >"$T/chill.env"; echo '{"state":"direct","reason":"idle"}' >"$T/chill.state""#,
        r#"printf "CHILL_HOME_EXIT='192.0.2.10:1080'\n" >"$T/chill.env"; echo '{"state":"running","reason":null}' >"$T/chill.state"; echo "192.0.2.10 via 10.1.2.4 dev rmnet_data0" >"$T/iproute""#,
        run_ts,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    for (t, d) in [
        ("2026-09-27T09:01:40.123456789Z", "52"),
        ("2026-09-27T09:01:40.5Z", "0"),
        ("2026-09-27T08:50:00Z", "52"),
        ("2026-09-27T08:20:00Z", "52"),
        ("2026-09-27T07:00:00Z", "0"),
        ("2026-09-27T09:10:00Z", "52"),
    ] {
        scenarios.push(hist(t, d));
    }
    scenarios.push(format!(r#"{run_ts}; echo garbage >"$T/proxy""#));
    let mut cases = Vec::new();
    for s in scenarios {
        let fx = fixture(&format!("{base}{s}"));
        if s.contains("history") {
            let t = rust(&fx, Mode::Tsv).0;
            assert!(t.contains("\thome-exit\t"), "{t}");
        }
        cases.push((s, all_modes(&fx)));
    }
    snap("chill", &cases);
}

const MS: &str = r#"export DOC_MANIFEST=$T/m.jsonl DOC_SHIP_TXN=$T/txn DOC_SHIP_HB=$T/hb.ship DOC_MD5_CACHE=$T/md5c/cache DOC_TS_DIR=$T/ts DOC_BOOT_ID_FILE=$T/bootid DOC_CHILL_DIR=$T/chill
echo bbbb-0000 >"$T/bootid"
printf 'core build A\n' >"$T/core"; DM=$(md5sum "$T/core" | cut -d' ' -f1); RM=$(md5sum "$T/rc" | cut -d' ' -f1)
mship() { printf '{"v":1,"kind":"ship","comp":"%s","txn":"20260930-120000-%s","commit":"abc1234","format":1,"mac_time":1790000000,"boot_id":"x","uptime":5,"files":[{"path":"%s","md5":"%s","prev":"%s.prev-x"}],"state":[]}\n' "$1" "$1" "$2" "$3" "$2" >>"$T/m.jsonl"; }
mrec() { printf '{"v":1,"kind":"record","name":"%s","path":"%s","md5":"%s","mac_time":1790000000,"boot_id":"x","uptime":5,"why":"t"}\n' "$1" "$2" "$3" >>"$T/m.jsonl"; }
txnf() { printf 'v=1\ntxn=20260930-120000-core\ncomp=core\nphase=%s\nreason=%s\nboot_id=%s\nt_phase=%s\nend=1\n' "$1" "$2" "${3:-bbbb-0000}" "${4:-900}" >"$T/txn"; }
"#;

#[test]
fn manifest() {
    // the two trees first, then their fingerprints (computed here) go into the manifest
    let dirs_pre = r#"export DOC_UI_FONTS=$T/ui/fonts
mkdir -p "$T/admin/_next/static" "$T/ui/fonts" "$T/admin/empty"
printf 'page\n' >"$T/admin/index.html"; printf 'js\n' >"$T/admin/_next/static/a b.js"; printf 'font\n' >"$T/ui/fonts/x.ttf""#;
    let dirs = r#"printf '{"v":1,"kind":"ship","comp":"web","txn":"t","commit":"a","format":1,"files":[{"path":"%s","tree":"%s","prev":"%s.prev"}],"state":[]}\n' "$T/admin" "$AF" "$T/admin" >>"$T/m.jsonl"
printf '{"v":1,"kind":"record","name":"ui-fonts","path":"%s","tree":"%s","why":""}\n' "$T/ui/fonts" "$FF" >>"$T/m.jsonl"
"#;
    let scenarios = [
        r#"mship core "$T/core" "$DM"; mrec rc.local "$T/rc" "$RM""#,
        r#"mship core "$T/core" "$DM"; mrec rc.local "$T/rc" "$RM"; printf 'core build B\n' >"$T/core""#,
        r#"mship core "$T/core" "$DM"; mrec rc.local "$T/rc" "$RM"; printf 'core build B\n' >"$T/core"; echo "x edited" >>"$T/rc""#,
        r#"printf 'core build B\n' >"$T/core"; mship core "$T/core" "$DM"; mship core "$T/core" "$(md5sum $T/core | cut -d' ' -f1)"; mrec tailscaled "$T/ts/tailscaled" -"#,
        r#"mship core "$T/core" "$DM"; mrec tailscaled "$T/ts/tailscaled" -; mkdir -p "$T/ts"; echo bin >"$T/ts/tailscaled""#,
        r#"printf 'doctor v2\n' >"$T/core"; mship guard "$T/core" "$DM"; mship scripts "$T/core" "$(md5sum $T/core | cut -d' ' -f1)""#,
        r#"printf 'doctor v2\n' >"$T/core"; mship guard "$T/core" "$DM"; mship scripts "$T/core" "$(md5sum $T/core | cut -d' ' -f1)"; mship guard "$T/core" "$DM""#,
        r#"mship core "$T/core" "$DM"; mship old "$T/rc" "$RM"; mrec tuning.env "$T/tuning.env" -; printf '{"v":1,"kind":"retire","comps":"old x","records":"tuning.env"}\n' >>"$T/m.jsonl""#,
        r#"echo 'garbage {' >"$T/m.jsonl""#,
        r#"mship core "$T/core" "$DM"; txnf manifest_pending x"#,
        r#"mship core "$T/core" "$DM"; txnf failed '退回不完整'"#,
        r#"mship core "$T/core" "$DM"; txnf failed 'executor gone'"#,
        r#"mship core "$T/core" "$DM"; txnf check ''"#,
        r#"mship core "$T/core" "$DM"; txnf check ''; mkdir -p "$T/proc/700"; printf 'sh\000/data/luyaz/scripts/luyaz-ship.sh\000run\000' >"$T/proc/700/cmdline"; echo "995 700 20260930-120000-core check" >"$T/hb.ship""#,
        r#"mship core "$T/core" "$DM"; txnf trial ''; mkdir -p "$T/proc/700"; printf 'sh\000other\000' >"$T/proc/700/cmdline"; echo "995 700" >"$T/hb.ship""#,
        r#"mship core "$T/core" "$DM"; txnf done '' bbbb-0000 900"#,
        r#"mship core "$T/core" "$DM"; txnf done '' cccc-0000 900"#,
        r#"mship core "$T/core" "$DM"; txnf rolledback '开机时已退回（断电时在 check）'"#,
        r#"mship core "$T/core" "$DM"; txnf aborted 'user said stop'; printf 'core build B\n' >"$T/core""#,
        r#"printf 'core build B\n' >"$T/core"; mship datad "$T/core" "$DM"; mship core "$T/core" "$(md5sum $T/core | cut -d' ' -f1)"; mrec luyaz-recover.sh "$T/rc" "$RM""#,
        r#"mship core "$T/core" "$DM"; mship datad "$T/core" "$DM"; txnf trial ''"#,
        dirs,
        &format!(r#"{dirs}ln -s index.html "$T/admin/link""#),
        &format!(r#"{dirs}rm -rf "$T/ui/fonts""#),
        &format!(r#"{dirs}printf 'new js\n' >"$T/admin/_next/static/c.js""#),
        &format!(
            r#"{dirs}export DOC_MD5SUM=md5sum; dd if=/dev/zero of="$T/big" bs=1024 count=300 2>/dev/null; mship lweb "$T/big" "$(md5sum $T/big | cut -d' ' -f1)""#
        ),
    ];
    let mut cases = Vec::new();
    for s in scenarios {
        let fx = if let Some(rest) = s.strip_prefix(dirs) {
            let mut fx = fixture(&format!("{MS}{dirs_pre}"));
            let fp = |d: &str| manifest::tree_fp(&fx.dir.join(d).to_string_lossy()).unwrap();
            let extra = [("AF", fp("admin")), ("FF", fp("ui/fonts"))];
            then(&mut fx, &extra, &format!("{MS}{dirs}{rest}"));
            fx
        } else {
            fixture(&format!("{MS}{s}"))
        };
        cases.push((s.to_string(), all_modes(&fx)));
    }
    snap("manifest", &cases);
}

#[test]
fn tree_fp() {
    let fx = fixture(
        r#"mkdir -p "$T/a/b c/d" "$T/e" "$T/l" "$T/p" "$T/n"; printf 'x\n' >"$T/a/1"; printf 'y' >"$T/a/b c/d/2"; : >"$T/a/b c/3"
ln -s 1 "$T/l/x"; : >"$T/p/a|b"; ln -s "$T/a" "$T/link"; : >"$T/n/$(printf 'a\nb')""#,
    );
    let cases: Vec<_> = ["a", "e", "l", "p", "n", "link", "missing", "a/1"]
        .iter()
        .map(|d| {
            let fp = manifest::tree_fp(&fx.dir.join(d).to_string_lossy());
            (d.to_string(), format!("{fp:?}\n"))
        })
        .collect();
    snap("tree-fp", &cases);
    // the shared fixture every copy of the algorithm must agree on (touch-ui scripts/test)
    let fix =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../touch-ui/scripts/test/fixtures");
    if let Ok(want) = std::fs::read_to_string(fix.join("tree.expected")) {
        let got = manifest::tree_fp(&fix.join("tree").to_string_lossy());
        assert_eq!(got.as_deref(), Some(want.trim()));
    }
}

/// doctor lists exactly the names `luyaz-ship.sh record` writes (was a shell test against
/// doctor-legacy.sh): read off its `record_list`.
#[test]
fn record_names_are_the_ship_executors() {
    let ship = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../touch-ui/scripts/luyaz-ship.sh");
    let Ok(text) = std::fs::read_to_string(&ship) else {
        return;
    };
    let body = &text[text.find("\nrecord_list() {").expect("record_list")..];
    let body = &body[..body.find("\n}\n").unwrap()];
    let mut theirs: Vec<String> = Vec::new();
    for l in body.lines().map(str::trim) {
        if l.starts_with("for _s in $CORE_NAME tailscale;") {
            theirs.extend(["init.d/luyaz-core".into(), "init.d/tailscale".into()]);
        } else if let Some(r) = l.split("echo \"").nth(1) {
            let n = r.split(' ').next().unwrap().to_owned();
            if !theirs.contains(&n) {
                theirs.push(n);
            }
        }
    }
    let fx = fixture("");
    let mine: Vec<String> = manifest::record_list(&cfg(&fx))
        .into_iter()
        .map(|r| r.0)
        .collect();
    assert_eq!(mine, theirs);
}

#[test]
fn md5_is_md5sum() {
    for (s, h) in [
        ("", "d41d8cd98f00b204e9800998ecf8427e"),
        ("a", "0cc175b9c0f1b6a831c399e269772661"),
        ("abc", "900150983cd24fb0d6963f7d28e17f72"),
        ("message digest", "f96b697d7cb7938d525a2f31aaf161d0"),
        (
            "12345678901234567890123456789012345678901234567890123456789012345678901234567890",
            "57edf4a22be3c955ac49da2e2107b67a",
        ),
    ] {
        assert_eq!(manifest::md5_hex(s.as_bytes()), h, "{s}");
    }
    let big: Vec<u8> = (0..200_003u32).map(|i| (i * 7 % 251) as u8).collect();
    let p = std::env::temp_dir().join(format!("doctor-md5-{}", std::process::id()));
    std::fs::write(&p, &big).unwrap();
    if let Ok(o) = Command::new("md5sum").arg(&p).output() {
        let want = String::from_utf8_lossy(&o.stdout)
            .split(' ')
            .next()
            .unwrap_or("")
            .to_owned();
        assert_eq!(manifest::md5_hex(&big), want);
    }
    let _ = std::fs::remove_file(&p);
}

const FPFIX: &str = r#"export DOC_STANDBY_STAT=$T/sb DOC_STANDBY_BASE=$T/sb.base
MCOL=' 5.0'
printf 'tailscaled 501 aaaaaaaa\nmihomo 502 bbbbbbbb\nluyaz-ui 503 cccccccc\nluyaz-core 504 dddddddd\n' >"$T/fp"
rows() { _i=0; while [ $_i -lt "$1" ]; do echo "$(($2 + _i * 60)) $3 20 ${4:-30.0}$MCOL 2.0 1.0"; _i=$((_i + 1)); done; }
// cal: the 40 rows a baseline is calibrated from; the tests run --calibrate-standby right after
cal() { : >"$T/sb"; i=0; while [ $i -lt 40 ]; do echo "$((100 + i * 20)) $((50 + i)) 20 30.0$MCOL 2.0 1.0" >>"$T/sb"; i=$((i + 1)); done; }
"#;

#[test]
fn standby() {
    // the shell comments in FPFIX are Rust comments for the public strip; drop them for sh
    let fpfix: String = FPFIX
        .lines()
        .filter(|l| !l.starts_with("//"))
        .map(|l| format!("{l}\n"))
        .collect();
    let mut cases = Vec::new();
    for s in [
        "",
        r#"cal; rows 5 700 300 >"$T/sb""#,
        r#"cal; rows 10 300 72 >"$T/sb""#,
        r#"cal; rows 10 300 200 >"$T/sb""#,
        r#"cal; rows 10 300 70 90.0 >"$T/sb""#,
        r#"cal; rows 9 300 70 90.0 >"$T/sb"; rows 1 900 70 90.5 >>"$T/sb""#,
        r#"cal; { rows 4 300 70; echo "600 70 20 - - - -"; rows 5 660 70; } >"$T/sb""#,
        r#"cal; : >"$T/sb"; i=1; while [ $i -le 10 ]; do echo "$((i * 9)) 500 20 30.0$MCOL 2.0 1.0" >>"$T/sb"; i=$((i + 1)); done"#,
        r#"cal; rm -f "$T/sb""#,
        r#"echo "TS_NO_LOGS_NO_SUPPORT=true" >"$T/tuning.env"; cal; rows 10 300 70 90.0 >"$T/sb"; sed -i 's/^tailscaled 501 aaaaaaaa/tailscaled 601 ffffffff/' "$T/fp""#,
        r#"echo "TS_NO_LOGS_NO_SUPPORT=true" >"$T/tuning.env"; cal; rows 10 300 70 90.0 >"$T/sb"; echo "TS_DEBUG_FIREWALL_MODE=auto" >>"$T/tuning.env""#,
        r#"cal; rows 10 300 70 90.0 >"$T/sb"; echo 800 >"$T/fp-changed""#,
        r#"cal; rows 10 300 70 90.0 >"$T/sb"; rm -f "$T/fp""#,
        r#"cal; rows 10 300 70 >"$T/sb"; printf '2 69.5 10.0\n3 20.0 0.0\n4 30.0 0.0\n' >"$T/sb.base""#,
        r#"cal; rows 10 300 70 | sed 's/ 1.0$/ 9.0/' >"$T/sb""#,
        r#"cal; rows 10 300 70 | sed 's/ 2.0 / 7.5 /' >"$T/sb""#,
    ] {
        let pre = format!(
            "export DOC_FP=$T/fp DOC_FP_CHANGED=$T/fp-changed DOC_TS_TUNING=$T/tuning.env\n{fpfix}"
        );
        // `cal;` = the calibration rows, then --calibrate-standby (in Rust), then the rest
        let fx = match s.split_once("cal;") {
            Some((before, after)) => {
                let mut fx = fixture(&format!("{pre}{before}cal"));
                let ledger = format!("{}/ledger", fx.dir.display());
                calibrate_with(&fx, &[("DOC_LEDGER_DIR", &ledger)]);
                // (pre again for `rows`; it rewrites only what the calibration did not touch)
                then(&mut fx, &[], &format!("{pre}{after}"));
                fx
            }
            None => fixture(&format!("{pre}{s}")),
        };
        let t = rust(&fx, Mode::Tsv).0;
        assert!(t.contains("\tstandby\t"));
        cases.push((s.to_string(), all_modes(&fx)));
    }
    snap("standby", &cases);
}

#[test]
fn calibrate_standby() {
    let fpfix: String = FPFIX
        .lines()
        .filter(|l| !l.starts_with("//"))
        .map(|l| format!("{l}\n"))
        .collect();
    let pre = format!(
        "export DOC_FP=$T/fp DOC_FP_CHANGED=$T/fp-changed DOC_TS_TUNING=$T/tuning.env DOC_WGET=/bin/false\n{fpfix}"
    );
    let mut cases = Vec::new();
    for s in [
        r#"rows 40 100 70 >"$T/sb""#,
        r#"rows 29 100 70 >"$T/sb""#,
        r#"rm -f "$T/sb""#,
        r#"{ rows 20 100 70; echo "1400 - - - - - -"; rows 15 1500 90 45.5; } >"$T/sb"; rm -f "$T/fp""#,
        r#"rows 31 100 70 | sed 's/ 1.0$/ -/' >"$T/sb"; echo "TS_NO_LOGS_NO_SUPPORT=true" >"$T/tuning.env""#,
    ] {
        let fx = fixture(&format!("{pre}{s}"));
        let (out, rc) = standby::calibrate(&cfg(&fx));
        let base =
            std::fs::read_to_string(fx.dir.join("sb.base")).unwrap_or_else(|_| "(none)\n".into());
        let d = fx.dir.to_string_lossy().into_owned();
        cases.push((
            s.to_string(),
            format!("--- rc={rc}\n{}--- sb.base\n{base}", out.replace(&d, "$T")),
        ));
    }
    snap("calibrate-standby", &cases);
}

#[test]
fn ledger_selftest() {
    let ss = r#"export DOC_LEDGER_DIR=$T/ledger DOC_CRASHCAP_DIR=$T/crashcap DOC_KEYLOG=$T/key.log DOC_MSS_RECOVERY=$T/recovery DOC_BOOT_ID_FILE=$T/bootid DOC_JSONFILTER=$T/bin/jsonfilter DOC_LEDGER_TMP=$T/lt DOC_SELFTEST_WAIT=1
mkdir -p "$T/ledger" "$T/crashcap" "$T/lt"
echo aaaaaaaa-0000-4000-8000-000000000001 >"$T/bootid"
export DOC_KMSG=$T/crashcap/kmsg-aaaaaaaa.log
printf '6,1,1000,-;boot\n' >"$DOC_KMSG"
printf '2026-09-28 10:00:00 : reboot_reason_code=1150!!! \n' >"$T/key.log"
echo enabled >"$T/recovery"
printf '#!/bin/sh\necho "export X='"'"'1'"'"'; export Z='"'"'1'"'"'; "\n' >"$T/bin/jsonfilter"
printf '#!/bin/sh\ncase "$*" in *9460/v2/state*) echo "{\\"epoch\\":\\"e\\"}" ;; esac\nexit 0\n' >"$T/bin/wget"
printf '#!/bin/sh\necho "9: rmnet_data0    inet 10.1.2.3/30 scope global rmnet_data0"\n' >"$T/bin/ip"
printf 'Iface\tDestination\tGateway\tFlags\nrmnet_data0\t00000000\t00000000\t0001\n' >"$T/route"
chmod +x "$T/bin/jsonfilter" "$T/bin/wget" "$T/bin/ip"
export DOC_IP=$T/bin/ip DOC_ROUTE=$T/route
"#;
    let mut cases = Vec::new();
    for s in [
        "",
        r#"mv "$T/key.log" "$T/key.log.0"; : >"$T/key.log""#,
        r#"mv "$T/key.log" "$T/key.log.0"; printf 'reboot_reason_code=1 x reboot_reason_code=1185!!! \nreboot_reason_code=x\n' >"$T/key.log""#,
        r#"echo "999999 1 0" >"$T/lt/w.pos"; echo "123" >"$T/lt/watcher.pid""#,
        r#"echo "10 1 0" >"$T/lt/w.pos"; echo "123" >"$T/lt/watcher.pid""#,
        r#": >"$T/key.log"; rm -f "$T/crashcap/kmsg-aaaaaaaa.log" "$T/recovery"; export DOC_KMSG=$T/no/such/kmsg; printf 'Iface\tDestination\n' >"$T/route"; printf '#!/bin/sh\nexit 1\n' >"$T/bin/wget"; printf '#!/bin/sh\n' >"$T/bin/jsonfilter""#,
        r#"export DOC_KMSG=$T/no/such/kmsg"#,
        r#"export DOC_KMSG=$T/elsewhere; : >"$T/elsewhere""#,
        r#"chmod 500 "$T/ledger"; export DOC_LEDGER_DIR=$T/ledger/sub/x; rm -f "$T/bootid""#,
    ] {
        let fx = fixture(&format!("{ss}{s}"));
        let (out, rc) = selftest::run(&cfg(&fx));
        let d = fx.dir.to_string_lossy().into_owned();
        cases.push((
            s.to_string(),
            format!("--- rc={rc}\n{}", out.replace(&d, "$T")),
        ));
    }
    snap("ledger-selftest", &cases);
}

#[test]
fn report_wants_a_window() {
    assert_eq!(report(&Cfg::from(&|_| None), None, None).1, 2);
}

#[test]
fn the_sed_and_awk_and_df_helpers() {
    assert_eq!(
        no_sync_name(r#"{ "noSyncModuleName": "sync success" }"#),
        "sync success"
    );
    assert_eq!(
        no_sync_name(r#""noSyncModuleName":"a" "noSyncModuleName":  "b" x"#),
        "b"
    );
    assert_eq!(
        no_sync_name("\"noSyncModuleName\": \"a\"\nnothing\n\"noSyncModuleName\":\"c\""),
        "a\nc"
    );
    assert_eq!(no_sync_name(r#""noSyncModuleName": unquoted"#), "");
    assert_eq!(awk_num("12abc"), 12.0);
    assert_eq!(awk_num(" 3.5"), 3.5);
    assert_eq!(awk_num("abc"), 0.0);
    assert_eq!(awk_num("1e2"), 100.0);
    assert_eq!(awk_num("."), 0.0);
    assert_eq!(df_free("h\n/dev 1 2 300 4% /data\n"), Some(300));
    assert_eq!(df_free("h\n/dev 1 2 x 4% /data\n"), None);
    assert_eq!(df_free("a b\n"), None);
    assert_eq!(df_free(""), None);
}

#[test]
fn render_flattens_and_counts() {
    let r = vec![
        row("ok", "a", "甲", "x\ty\nz".into(), "A", "e\tf".into()),
        row("warn", "b", "乙", "w".into(), "B", "w".into()),
    ];
    assert_eq!(
        render(Mode::Tsv, &r),
        ("ok\ta\t甲\tx y z\nwarn\tb\t乙\tw\n".into(), 0)
    );
    assert_eq!(
        render(Mode::Tsv2, &r).0,
        "#tsv2\nok\ta\t甲\tx y z\tA\te f\nwarn\tb\t乙\tw\tB\tw\n"
    );
    assert_eq!(
        render(Mode::Text, &r).0,
        "  ● 甲：x\ty\nz\n  ▲ 乙：w\n\n  0 项异常，1 项需要注意\n"
    );
    assert_eq!(
        render(Mode::Text, &r[..1]).0.lines().last(),
        Some("  一切正常")
    );
}

#[test]
fn the_doctor_is_not_a_core_by_name() {
    assert!(!crate::paths::is_core(COMM));
    assert!(COMM.len() <= 15, "comm holds 15 bytes");
    #[cfg(target_os = "linux")]
    std::thread::spawn(|| {
        set_comm();
        let comm = std::fs::read_to_string("/proc/thread-self/comm").unwrap();
        assert_eq!(comm.trim_end(), COMM);
    })
    .join()
    .unwrap();
}

#[test]
fn clock_row_requires_fresh_boot_bound_evidence_and_does_not_claim_sync_for_an_estimate() {
    use luyaz_api::clock::{BootTime, ClockEvidence, ClockSource};
    let mut fx = fx_or_skip!("");
    let boot = fx.dir.join("clock-boot");
    std::fs::write(&boot, "a\n").unwrap();
    fx.env
        .insert("DOC_BOOT_ID_FILE".into(), boot.to_str().unwrap().into());
    let c = cfg(&fx);
    let wall = digits(&sh_out(&c.date, &["+%s"], false)).unwrap() as i64;
    let e = ClockEvidence::observation(
        wall,
        BootTime {
            boot_id: "a".into(),
            seconds: 1000,
        },
        Some(1000),
        ClockSource::Year,
    );
    std::fs::write(&c.clock_ok, e.encode()).unwrap();
    let r = clock(&c);
    assert_eq!(r.level, "ok");
    assert!(r.detail.contains("估算") && r.detail.contains("尚未验证同步"));
    assert!(r.detail_en.contains("sync unverified"));
    for (up, boot_id, raw_wall, reason) in [
        (1181, "a", wall + 181, "过期"),
        (1000, "b", wall, "另一次开机"),
        (1060, "a", wall + 60 + 28_800, "跳变"),
        (1060, "a", wall + 60 - 28_800, "跳变"),
    ] {
        std::fs::write(&c.uptime, format!("{up}.00 1.00\n")).unwrap();
        std::fs::write(&boot, boot_id).unwrap();
        std::fs::write(fx.dir.join("now"), raw_wall.to_string()).unwrap();
        let r = clock(&c);
        assert_eq!(r.level, "warn");
        assert!(r.detail.contains(reason), "{r:?}");
        assert!(r.detail.contains("暂缓发送"));
    }
    // A year and a future/stale/legacy 'trusted' record never act as a fallback.
    std::fs::write(&c.uptime, "1000.00 1.00").unwrap();
    std::fs::write(&boot, "a").unwrap();
    std::fs::write(fx.dir.join("now"), wall.to_string()).unwrap();
    for text in ["", "1 1790499000 990 year\n", "garbage"] {
        std::fs::write(&c.clock_ok, text).unwrap();
        assert_eq!(clock(&c).level, "warn");
    }
    std::fs::write(&c.uptime, "0.00 1.00").unwrap();
    assert!(clock(&c).detail.contains("刚开机"));
}

#[test]
fn lanid_row_from_the_clients_block() {
    use std::os::unix::fs::PermissionsExt;
    let fx = fixture("");
    let run = |state: &str| {
        let w = fx.dir.join("bin/wget-lanid");
        std::fs::write(fx.dir.join("state.json"), state).unwrap();
        std::fs::write(
            &w,
            format!("#!/bin/sh\ncat '{}'\n", fx.dir.join("state.json").display()),
        )
        .unwrap();
        std::fs::set_permissions(&w, std::fs::Permissions::from_mode(0o755)).unwrap();
        super::lanid(&cfg_with(&fx, &[("DOC_WGET", w.to_str().unwrap())]))
    };
    let on = r#"{"blocks":{"clients":{"data":{"lanid":{"loaded":true,"why":"","batches":3,"frames":40,
        "traps":1,"dropped":2,"blocked":1,"records":5,
        "sources":{"dhcp":true,"mdns":true,"ssdp":true,"netbios":true}}}}}}"#;
    let r = run(on).unwrap();
    assert_eq!(r.level, "ok");
    assert!(
        r.detail.contains("DHCP/mDNS/SSDP/NetBIOS") && r.detail.contains("trap 1"),
        "{}",
        r.detail
    );
    let off = r#"{"blocks":{"clients":{"data":{"lanid":{"loaded":false,"why":"no component","batches":0,
        "frames":0,"traps":0,"dropped":0,"blocked":0,"records":0,
        "sources":{"dhcp":false,"mdns":false,"ssdp":false,"netbios":false}}}}}}"#;
    let r = run(off).unwrap();
    assert_eq!(r.level, "warn");
    assert!(r.detail.contains("no component"), "{}", r.detail);
    // an older core, or none
    assert!(run(r#"{"blocks":{"clients":{"data":{"list":[]}}}}"#).is_none());
    assert!(run("").is_none());
}
