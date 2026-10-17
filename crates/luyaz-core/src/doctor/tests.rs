//! Parity with touch-ui scripts/doctor.sh: the shell test's own `setup` (scripts/test/doctor/run.sh)
//! builds the fixture, then the Rust rows are compared with the goldens and with doctor.sh run live
//! on the same fixture, row by row for the ids ported so far.

use super::*;
use std::collections::HashMap;
use std::path::PathBuf;

fn scripts() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../touch-ui/scripts");
    p.join("doctor.sh").is_file().then_some(p)
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

/// run.sh's `setup`, then `scenario` (sh, with `$T` the fixture).
fn fixture(scenario: &str) -> Option<Fx> {
    let s = scripts()?;
    let script = format!(
        "RUN='{run}'\neval \"$(sed -n '/^setup() {{/,/^}}/p' \"$RUN\")\"\nsetup\n{scenario}\nenv | grep '^DOC_' >\"$T/doc.env\"\necho \"$T\"\n",
        run = s.join("test/doctor/run.sh").display()
    );
    let o = Command::new("sh")
        .arg("-c")
        .arg(script)
        .stderr(Stdio::inherit())
        .output()
        .ok()?;
    let dir = PathBuf::from(String::from_utf8_lossy(&o.stdout).trim());
    let env = std::fs::read_to_string(dir.join("doc.env"))
        .ok()?
        .lines()
        .filter_map(|l| l.split_once('=').map(|(k, v)| (k.to_owned(), v.to_owned())))
        .collect();
    Some(Fx { dir, env })
}

fn cfg(fx: &Fx) -> Cfg {
    Cfg::from(&|k| fx.env.get(k).cloned())
}

fn rust(fx: &Fx, mode: Mode) -> (String, i32) {
    full(&cfg(fx), mode)
}

fn shell(fx: &Fx, args: &[&str]) -> (String, i32) {
    let o = Command::new("sh")
        .arg(scripts().unwrap().join("doctor.sh"))
        .args(args)
        .envs(&fx.env)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&o.stdout).into_owned(),
        o.status.code().unwrap_or(-1),
    )
}

macro_rules! fx_or_skip {
    ($s:expr) => {
        match fixture($s) {
            Some(f) => f,
            None => {
                eprintln!("skipped: touch-ui/scripts/doctor.sh or sh not here");
                return;
            }
        }
    };
}

/// Rust == doctor.sh on one scenario, byte for byte and exit code: --tsv, --tsv2, text and
/// --manifest; returns the Rust --tsv
fn same_as_shell(scenario: &str) -> Option<String> {
    let fx = fixture(scenario)?;
    for (mode, arg) in [
        (Mode::Tsv, "--tsv"),
        (Mode::Tsv2, "--tsv2"),
        (Mode::Text, ""),
    ] {
        let args: &[&str] = if arg.is_empty() { &[] } else { &[arg] };
        assert_eq!(
            rust(&fx, mode),
            shell(&fx, args),
            "{arg:?}, scenario:\n{scenario}"
        );
    }
    assert_eq!(
        (manifest::report(&cfg(&fx)), 0),
        shell(&fx, &["--manifest"]),
        "--manifest, scenario:\n{scenario}"
    );
    Some(rust(&fx, Mode::Tsv).0)
}

const PROBLEMS: &str = r#"echo "zte_topsw_devui" >"$T/sync"; echo 1 >"$T/mode"; echo 0 >"$T/aps"
touch "$T/down-u60-guard"; echo 500 >"$T/hb"; touch "$T/uid/gave-up"; : >"$T/alerts/sms-to"
grep -v "u60-uid start" "$T/rc" >"$T/rc2"; mv "$T/rc2" "$T/rc"
printf '9\t1\tcrash\tx\ty\n' >"$T/alerts/queue""#;

fn golden(name: &str) -> String {
    std::fs::read_to_string(scripts().unwrap().join("test/doctor").join(name)).unwrap()
}

const STANDBY: &str = r#"export DOC_STANDBY_STAT=$T/sb DOC_STANDBY_BASE=$T/sb.base DOC_LEDGER_DIR=$T/ledger DOC_BOOT_ID_FILE=$T/bootid
echo aaaaaaaa-0000-4000-8000-000000000001 >"$T/bootid"
i=0; while [ $i -lt 40 ]; do echo "$((100 + i * 20)) $((50 + i)) 20 30.0 5.0 2.0 1.0 0.5" >>"$T/sb"; i=$((i + 1)); done
DOC_WGET=/bin/false sh "$(dirname "$RUN")/../../doctor.sh" --calibrate-standby >/dev/null
: >"$T/sb"; i=0; while [ $i -lt 10 ]; do echo "$((300 + i * 60)) 200 20 30.0 5.0 2.0 1.0 0.5" >>"$T/sb"; i=$((i + 1)); done
unset DOC_LEDGER_DIR DOC_BOOT_ID_FILE"#;

#[test]
fn the_goldens_byte_for_byte() {
    let mut cases = vec![("healthy", ""), ("problems", PROBLEMS)];
    cases.push(("standby", STANDBY)); // the golden's records carry the private mihomo column
    for (name, scenario) in cases {
        let fx = fx_or_skip!(scenario);
        let (t, rc) = rust(&fx, Mode::Tsv);
        assert_eq!(t, golden(&format!("golden/{name}.tsv")), "{name}.tsv");
        assert_eq!(rc, if name == "problems" { 1 } else { 0 });
        assert_eq!(
            rust(&fx, Mode::Text).0,
            golden(&format!("golden/{name}.txt")),
            "{name}.txt"
        );
        if name == "problems" {
            assert_eq!(rust(&fx, Mode::Tsv2).0, golden("tsv2-sample.tsv"));
        }
    }
}

#[test]
fn scenarios_match_doctor_sh_live() {
    if scripts().is_none() {
        eprintln!("skipped: no doctor.sh");
        return;
    }
    let scenarios = [
        // healthy / problems (the goldens)
        "",
        PROBLEMS,
        // boot-sync unreadable, uci gives nothing
        r#"printf '#!/bin/sh\n' >"$T/bin/ubus"; : >"$T/mode"; : >"$T/poll""#,
        // two lines with the name, one with two names (sed: the last per line)
        r#"printf '#!/bin/sh\necho "{ \\"noSyncModuleName\\": \\"a\\", \\"noSyncModuleName\\":\\"b\\" }"\necho "\\"noSyncModuleName\\": \\"c\\""\n' >"$T/bin/ubus""#,
        r#"echo 0 >"$T/poll""#,
        // crashes (run.sh): read alerts, then unread; nested, dotted and old files
        r#"touch "$T/crash/x.log""#,
        r#"touch "$T/crash/x.log"; printf '9\t1\tcrash\tx\ty\n' >"$T/alerts/queue""#,
        r#"touch "$T/crash/x.log"; printf '9\t1\tcrash\tx\ty\n' >"$T/alerts/queue"; echo 9 >"$T/alerts/read""#,
        r#"mkdir -p "$T/crash/a"; touch "$T/crash/a/y.log" "$T/crash/.log" "$T/crash/z.txt"; touch -d '3 days ago' "$T/crash/old.log"; printf '7x\t1\tc\tx\ty\n3\tshort\n' >"$T/alerts/queue"; echo 5 >"$T/alerts/read""#,
        // core-move (run.sh 10e): complete, init missing, both missing, half-way
        r#"mkdir -p "$T/root/data/luyaz/core"; : >"$T/root/data/luyaz/core/.migrated"; printf 'bin\n' >"$T/root/data/luyaz/luyaz-core"; chmod 755 "$T/root/data/luyaz/luyaz-core"; printf '#!/bin/sh\n' >"$T/initd/luyaz-core"; chmod +x "$T/initd/luyaz-core""#,
        r#"mkdir -p "$T/root/data/luyaz/core"; : >"$T/root/data/luyaz/core/.migrated"; printf 'bin\n' >"$T/root/data/luyaz/luyaz-core"; chmod 755 "$T/root/data/luyaz/luyaz-core""#,
        r#"mkdir -p "$T/root/data/luyaz/core"; : >"$T/root/data/luyaz/core/.migrated"; printf 'bin\n' >"$T/root/data/luyaz/luyaz-core""#,
        r#"mkdir -p "$T/root/data/luyaz/core""#,
        // tmp-move
        r#"mkdir -p "$T/tmp-luyaz""#,
        r#"mkdir -p "$T/tmp-luyaz"; : >"$T/tmp-luyaz/.migrated""#,
        // clock (run.sh)
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
        // services (run.sh): the sentinel took over, luyaz-web, zte-agent, old install, two cores
        r#"sed -i "s#.*u60-uid start#$T/initd/luyaz-sentinel start#; /u60-guard start/d" "$T/rc"; for f in u60-uid u60-guard; do rm -f "$T/initd/$f"; touch "$T/down-$f"; done; cp "$T/initd/zte-agent" "$T/initd/luyaz-sentinel""#,
        r#"sed -i "s#.*zwrt-datad start#&\\n$T/initd/luyaz-web start#" "$T/rc"; cp "$T/initd/zte-agent" "$T/initd/luyaz-web"; touch "$T/down-luyaz-web""#,
        r#": >"$T/luyaz-web"; export DOC_LWEB_BIN=$T/luyaz-web; cp "$T/initd/zte-agent" "$T/initd/luyaz-web""#,
        r#"mkdir -p "$T/proc/103"; echo zte-agent >"$T/proc/103/comm""#,
        r#"rm -f "$T"/initd/*"#,
        r#"mkdir -p "$T/proc/102" "$T/proc/104"; echo zwrt-datad >"$T/proc/102/comm"; echo u60pro-devui-x >"$T/proc/104/comm""#,
        // 10h R5: the touch UI under its new name next to the old one is two
        r#"mkdir -p "$T/proc/104"; echo luyaz-ui.te >"$T/proc/104/comm""#,
        r#"mkdir -p "$T/root/data/luyaz/core" "$T/proc/102"; : >"$T/root/data/luyaz/core/.migrated"; printf '#!/bin/sh\n' >"$T/initd/luyaz-core"; chmod +x "$T/initd/luyaz-core"; sed -i "s#.*zwrt-datad start#$T/initd/luyaz-core start#" "$T/rc"; echo luyaz-core >"$T/proc/101/comm"; echo zwrt-datad.test >"$T/proc/102/comm""#,
        r#"sed -i "s#^$T/initd/u60-guard start#\# &#" "$T/rc""#,
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
        // whiteouts: on a boot-barrier daemon, unrelated ones, no overlay
        r#"mknod "$T/rcd/S99zte_topsw_devui" c 0 0"#,
        r#"mknod "$T/rcd/K10foo" c 0 0; mknod "$T/rcd/S20bar" c 0 0; touch "$T/rcd/S30plain""#,
        r#"mknod "$T/rcd/K10foo" c 0 0; mknod "$T/rcd/K05zte_topsw_wlan" c 0 0"#,
        r#"rm -rf "$T/rcd""#,
        // phone-home (run.sh)
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
    for s in scenarios {
        same_as_shell(s).expect("fixture");
    }
}

#[test]
fn chill_rows_match_doctor_sh_live() {
    if scripts().is_none() {
        return;
    }
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
    for s in scenarios {
        let t = same_as_shell(&format!("{base}{s}")).expect("fixture");
        if s.contains("history") {
            assert!(t.contains("\thome-exit\t"), "{t}");
        }
    }
}

const MS: &str = r#"export DOC_MANIFEST=$T/m.jsonl DOC_SHIP_TXN=$T/txn DOC_SHIP_HB=$T/hb.ship DOC_MD5_CACHE=$T/md5c/cache DOC_TS_DIR=$T/ts DOC_BOOT_ID_FILE=$T/bootid DOC_CHILL_DIR=$T/chill
echo bbbb-0000 >"$T/bootid"
printf 'datad build A\n' >"$T/datad"; DM=$(md5sum "$T/datad" | cut -d' ' -f1); RM=$(md5sum "$T/rc" | cut -d' ' -f1)
mship() { printf '{"v":1,"kind":"ship","comp":"%s","txn":"20260930-120000-%s","commit":"abc1234","format":1,"mac_time":1790000000,"boot_id":"x","uptime":5,"files":[{"path":"%s","md5":"%s","prev":"%s.prev-x"}],"state":[]}\n' "$1" "$1" "$2" "$3" "$2" >>"$T/m.jsonl"; }
mrec() { printf '{"v":1,"kind":"record","name":"%s","path":"%s","md5":"%s","mac_time":1790000000,"boot_id":"x","uptime":5,"why":"t"}\n' "$1" "$2" "$3" >>"$T/m.jsonl"; }
txnf() { printf 'v=1\ntxn=20260930-120000-datad\ncomp=datad\nphase=%s\nreason=%s\nboot_id=%s\nt_phase=%s\nend=1\n' "$1" "$2" "${3:-bbbb-0000}" "${4:-900}" >"$T/txn"; }
"#;

#[test]
fn manifest_matches_doctor_sh_live() {
    if scripts().is_none() {
        return;
    }
    let dirs = r#"export DOC_DEVUI_DIR=$T/devui
mkdir -p "$T/admin/_next/static" "$T/devui/fonts" "$T/admin/empty"
printf 'page\n' >"$T/admin/index.html"; printf 'js\n' >"$T/admin/_next/static/a b.js"; printf 'font\n' >"$T/devui/fonts/x.ttf"
AF=$(sh "$(dirname "$RUN")/../../doctor.sh" --tree-fp "$T/admin"); FF=$(sh "$(dirname "$RUN")/../../doctor.sh" --tree-fp "$T/devui/fonts")
printf '{"v":1,"kind":"ship","comp":"web","txn":"t","commit":"a","format":1,"files":[{"path":"%s","tree":"%s","prev":"%s.prev"}],"state":[]}\n' "$T/admin" "$AF" "$T/admin" >>"$T/m.jsonl"
printf '{"v":1,"kind":"record","name":"devui-fonts","path":"%s","tree":"%s","why":""}\n' "$T/devui/fonts" "$FF" >>"$T/m.jsonl"
"#;
    let scenarios = [
        r#"mship datad "$T/datad" "$DM"; mrec rc.local "$T/rc" "$RM""#,
        r#"mship datad "$T/datad" "$DM"; mrec rc.local "$T/rc" "$RM"; printf 'datad build B\n' >"$T/datad""#,
        r#"mship datad "$T/datad" "$DM"; mrec rc.local "$T/rc" "$RM"; printf 'datad build B\n' >"$T/datad"; echo "x edited" >>"$T/rc""#,
        r#"printf 'datad build B\n' >"$T/datad"; mship datad "$T/datad" "$DM"; mship datad "$T/datad" "$(md5sum $T/datad | cut -d' ' -f1)"; mrec tailscaled "$T/ts/tailscaled" -"#,
        r#"mship datad "$T/datad" "$DM"; mrec tailscaled "$T/ts/tailscaled" -; mkdir -p "$T/ts"; echo bin >"$T/ts/tailscaled""#,
        r#"printf 'doctor v2\n' >"$T/datad"; mship guard "$T/datad" "$DM"; mship scripts "$T/datad" "$(md5sum $T/datad | cut -d' ' -f1)""#,
        r#"printf 'doctor v2\n' >"$T/datad"; mship guard "$T/datad" "$DM"; mship scripts "$T/datad" "$(md5sum $T/datad | cut -d' ' -f1)"; mship guard "$T/datad" "$DM""#,
        r#"mship datad "$T/datad" "$DM"; mship old "$T/rc" "$RM"; mrec tuning.env "$T/tuning.env" -; printf '{"v":1,"kind":"retire","comps":"old x","records":"tuning.env"}\n' >>"$T/m.jsonl""#,
        r#"echo 'garbage {' >"$T/m.jsonl""#,
        r#"mship datad "$T/datad" "$DM"; txnf manifest_pending x"#,
        r#"mship datad "$T/datad" "$DM"; txnf failed '退回不完整'"#,
        r#"mship datad "$T/datad" "$DM"; txnf failed 'executor gone'"#,
        r#"mship datad "$T/datad" "$DM"; txnf check ''"#,
        r#"mship datad "$T/datad" "$DM"; txnf check ''; mkdir -p "$T/proc/700"; printf 'sh\000/data/luyaz/scripts/luyaz-ship.sh\000run\000' >"$T/proc/700/cmdline"; echo "995 700 20260930-120000-datad check" >"$T/hb.ship""#,
        r#"mship datad "$T/datad" "$DM"; txnf trial ''; mkdir -p "$T/proc/700"; printf 'sh\000other\000' >"$T/proc/700/cmdline"; echo "995 700" >"$T/hb.ship""#,
        r#"mship datad "$T/datad" "$DM"; txnf done '' bbbb-0000 900"#,
        r#"mship datad "$T/datad" "$DM"; txnf done '' cccc-0000 900"#,
        r#"mship datad "$T/datad" "$DM"; txnf rolledback '开机时已退回（断电时在 check）'"#,
        r#"mship datad "$T/datad" "$DM"; txnf aborted 'user said stop'; printf 'datad build B\n' >"$T/datad""#,
        r#"printf 'datad build B\n' >"$T/datad"; mship datad "$T/datad" "$DM"; mship core "$T/datad" "$(md5sum $T/datad | cut -d' ' -f1)"; mrec u60-recover.sh "$T/rc" "$RM"; mrec luyaz-recover.sh "$T/rc" "$RM""#,
        r#"mship core "$T/datad" "$DM"; mship datad "$T/datad" "$DM"; txnf trial ''"#,
        dirs,
        &format!(r#"{dirs}ln -s index.html "$T/admin/link""#),
        &format!(r#"{dirs}rm -rf "$T/devui/fonts""#),
        &format!(r#"{dirs}printf 'new js\n' >"$T/admin/_next/static/c.js""#),
        &format!(
            r#"{dirs}export DOC_MD5SUM=md5sum; dd if=/dev/zero of="$T/big" bs=1024 count=300 2>/dev/null; mship lweb "$T/big" "$(md5sum $T/big | cut -d' ' -f1)""#
        ),
    ];
    for s in scenarios {
        same_as_shell(&format!("{MS}{s}")).expect("fixture");
    }
}

#[test]
fn tree_fp_matches_doctor_sh() {
    let Some(fx) = fixture(
        r#"mkdir -p "$T/a/b c/d" "$T/e" "$T/l" "$T/p" "$T/n"; printf 'x\n' >"$T/a/1"; printf 'y' >"$T/a/b c/d/2"; : >"$T/a/b c/3"
ln -s 1 "$T/l/x"; : >"$T/p/a|b"; ln -s "$T/a" "$T/link"; : >"$T/n/$(printf 'a\nb')""#,
    ) else {
        return;
    };
    for d in ["a", "e", "l", "p", "n", "link", "missing", "a/1"] {
        let dir = fx.dir.join(d).to_string_lossy().into_owned();
        let (out, rc) = shell(&fx, &["--tree-fp", &dir]);
        let mine = manifest::tree_fp(&dir).map(|f| format!("{f}\n"));
        assert_eq!(
            (
                mine.clone().unwrap_or_default(),
                if mine.is_some() { 0 } else { 1 }
            ),
            (out, rc),
            "{d}"
        );
    }
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
printf 'tailscaled 501 aaaaaaaa\nmihomo 502 bbbbbbbb\nu60pro-devui 503 cccccccc\nzwrt-datad 504 dddddddd\nzte-agent 505 eeeeeeee\n' >"$T/fp"
rows() { _i=0; while [ $_i -lt "$1" ]; do echo "$(($2 + _i * 60)) $3 20 ${4:-30.0}$MCOL 2.0 1.0 0.5"; _i=$((_i + 1)); done; }
cal() { : >"$T/sb"; i=0; while [ $i -lt 40 ]; do echo "$((100 + i * 20)) $((50 + i)) 20 30.0$MCOL 2.0 1.0 0.5" >>"$T/sb"; i=$((i + 1)); done
  DOC_WGET=/bin/false DOC_LEDGER_DIR=$T/ledger sh "$(dirname "$RUN")/../../doctor.sh" --calibrate-standby >/dev/null; }
"#;

#[test]
fn standby_matches_doctor_sh_live() {
    if scripts().is_none() {
        return;
    }
    // the shell comments in FPFIX are Rust comments for the public strip; drop them for sh
    let fpfix: String = FPFIX
        .lines()
        .filter(|l| !l.starts_with("//"))
        .map(|l| format!("{l}\n"))
        .collect();
    for s in [
        "",
        r#"cal; rows 5 700 300 >"$T/sb""#,
        r#"cal; rows 10 300 72 >"$T/sb""#,
        r#"cal; rows 10 300 200 >"$T/sb""#,
        r#"cal; rows 10 300 70 90.0 >"$T/sb""#,
        r#"cal; rows 9 300 70 90.0 >"$T/sb"; rows 1 900 70 90.5 >>"$T/sb""#,
        r#"cal; { rows 4 300 70; echo "600 70 20 - - - - -"; rows 5 660 70; } >"$T/sb""#,
        r#"cal; : >"$T/sb"; i=1; while [ $i -le 10 ]; do echo "$((i * 9)) 500 20 30.0$MCOL 2.0 1.0 0.5" >>"$T/sb"; i=$((i + 1)); done"#,
        r#"cal; rm -f "$T/sb""#,
        r#"echo "TS_NO_LOGS_NO_SUPPORT=true" >"$T/tuning.env"; cal; rows 10 300 70 90.0 >"$T/sb"; sed -i 's/^tailscaled 501 aaaaaaaa/tailscaled 601 ffffffff/' "$T/fp""#,
        r#"echo "TS_NO_LOGS_NO_SUPPORT=true" >"$T/tuning.env"; cal; rows 10 300 70 90.0 >"$T/sb"; echo "TS_DEBUG_FIREWALL_MODE=auto" >>"$T/tuning.env""#,
        r#"cal; rows 10 300 70 90.0 >"$T/sb"; echo 800 >"$T/fp-changed""#,
        r#"cal; rows 10 300 70 90.0 >"$T/sb"; rm -f "$T/fp""#,
        r#"cal; rows 10 300 70 >"$T/sb"; printf '2 69.5 10.0\n3 20.0 0.0\n4 30.0 0.0\n' >"$T/sb.base""#,
        r#"cal; rows 10 300 70 | sed 's/ 0.5$/ 9.0/' >"$T/sb""#,
        r#"cal; rows 10 300 70 | sed 's/ 2.0 / 7.5 /' >"$T/sb""#,
    ] {
        let t = same_as_shell(&format!("export DOC_FP=$T/fp DOC_FP_CHANGED=$T/fp-changed DOC_TS_TUNING=$T/tuning.env\n{fpfix}{s}")).expect("fixture");
        assert!(t.contains("\tstandby\t"));
    }
}

#[test]
fn calibrate_standby_matches_doctor_sh_live() {
    if scripts().is_none() {
        return;
    }
    let fpfix: String = FPFIX
        .lines()
        .filter(|l| !l.starts_with("//"))
        .map(|l| format!("{l}\n"))
        .collect();
    let pre = format!(
        "export DOC_FP=$T/fp DOC_FP_CHANGED=$T/fp-changed DOC_TS_TUNING=$T/tuning.env DOC_WGET=/bin/false\n{fpfix}"
    );
    for s in [
        r#"rows 40 100 70 >"$T/sb""#,
        r#"rows 29 100 70 >"$T/sb""#,
        r#"rm -f "$T/sb""#,
        r#"{ rows 20 100 70; echo "1400 - - - - - - -"; rows 15 1500 90 45.5; } >"$T/sb"; rm -f "$T/fp""#,
        r#"rows 31 100 70 | sed 's/ 0.5$/ -/' >"$T/sb"; echo "TS_NO_LOGS_NO_SUPPORT=true" >"$T/tuning.env""#,
    ] {
        let (fa, fb) = (
            fixture(&format!("{pre}{s}")).expect("fixture"),
            fixture(&format!("{pre}{s}")).expect("fixture"),
        );
        let (mine, rc) = standby::calibrate(&cfg(&fa));
        let (theirs, rc2) = shell(&fb, &["--calibrate-standby"]);
        let (a, b) = (
            fa.dir.to_string_lossy().into_owned(),
            fb.dir.to_string_lossy().into_owned(),
        );
        assert_eq!(
            (mine.replace(&a, "$T"), rc),
            (theirs.replace(&b, "$T"), rc2),
            "scenario:\n{s}"
        );
        let base = |d: &std::path::Path| std::fs::read_to_string(d.join("sb.base")).ok();
        assert_eq!(base(&fa.dir), base(&fb.dir), "baseline, scenario:\n{s}");
    }
}

#[test]
fn ledger_selftest_matches_doctor_sh_live() {
    if scripts().is_none() {
        return;
    }
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
        // each on its own fixture: the test line each appends moves the other's offset
        let (fa, fb) = (
            fixture(&format!("{ss}{s}")).expect("fixture"),
            fixture(&format!("{ss}{s}")).expect("fixture"),
        );
        let (mine, rc) = selftest::run(&cfg(&fa));
        let (theirs, rc2) = shell(&fb, &["--ledger-selftest"]);
        let (a, b) = (
            fa.dir.to_string_lossy().into_owned(),
            fb.dir.to_string_lossy().into_owned(),
        );
        assert_eq!(
            (mine.replace(&a, "$T"), rc),
            (theirs.replace(&b, "$T"), rc2),
            "scenario:\n{s}"
        );
    }
}

#[test]
fn modes_not_ported_say_so() {
    assert_eq!(main(&["--report".to_owned()]), EXIT_NOT_PORTED);
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
