//! Phase 10 X11: `<core> doctor` — the core's own copy of touch-ui `scripts/doctor.sh`
//! (manager docs/designs/u60-phase10.md「X11 核心接管体检」).
//!
//! A one-shot subcommand: main.rs dispatches it before anything else (no runtime, no flock, no pid
//! file, no crash markers, no migration, no core log), and the first thing it does is rename the
//! process (`luyaz-doctor`), so nothing that recognises the core by `/proc/<pid>/comm`
//! (`zwrt-datad*`, `luyaz-core*`) takes it for a second core.
//!
//! P0-01 replaces the old clock judgement with shared, fresh, boot-bound evidence. Other rows
//! retain doctor.sh's ids, levels, labels, details and order. Commands/paths retain the `DOC_*`
//! overrides, so the shell's fixtures (touch-ui scripts/test/doctor) still drive both.
//!
//! Batch 1: the output layer (text, `--tsv`, `--tsv2`, exit code) and the file/uci rows boot-sync,
//! fota, clock, core-move, tmp-move, crashes, disk. Batch 2: svc-*, dup-*, svc-zte-agent, heartbeat,
//! takeover, wifi, screen, alerts, sms. The other rows keep their slot
//! in [`run`]. Batch 3: whiteout, phonehome, CHILL, HTTP. Batch 4: the manifest (`--manifest`,
//! `--tree-fp`), standby, `--ledger-selftest`, text mode's scorecard lines; later
//! `--calibrate-standby` (standby.rs `calibrate`). Left to doctor.sh: `--report` (no live caller).

use std::process::{Command, Stdio};

/// What the process calls itself while it runs doctor (not a core name: `paths::is_core`).
pub const COMM: &str = "luyaz-doctor";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Text,
    Tsv,
    Tsv2,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub level: &'static str,
    pub id: &'static str,
    pub label: &'static str,
    pub detail: String,
    pub label_en: &'static str,
    pub detail_en: String,
}

fn row(
    level: &'static str,
    id: &'static str,
    label: &'static str,
    detail: String,
    label_en: &'static str,
    detail_en: String,
) -> Row {
    Row {
        level,
        id,
        label,
        detail,
        label_en,
        detail_en,
    }
}

/// Commands and paths, each `${DOC_X:-default}` as in doctor.sh. Paths are taken as the shell
/// takes them: only core-move looks under `DOC_ROOT` (the shell's `LROOT`).
pub struct Cfg {
    ubus: Vec<String>,
    uci: Vec<String>,
    date: Vec<String>,
    df: Vec<String>,
    uptime: String,
    clock_ok: String,
    alerts: String,
    crash: String,
    data_dir: String,
    initd: String,
    rc: String,
    ps: Vec<String>,
    wget: Vec<String>,
    proc_dir: String,
    heartbeat: String,
    marker: String,
    uid_state: String,
    uid_want: String,
    lweb_bin: String,
    overlay_rcd: String,
    daemon_conf: String,
    datad_url: String,
    ip: Vec<String>,
    manifest: String,
    ship_txn: String,
    ship_hb: String,
    md5_cache: String,
    /// DOC_MD5SUM: hash with that command; unset: in-process (the same hex as busybox md5sum)
    md5sum: Option<Vec<String>>,
    ts_dir: String,
    ts_tuning: String,
    /// the touch UI's fonts: /data/luyaz/ui/fonts once there (Phase 10 R5), else DOC_DEVUI_DIR/fonts
    ui_fonts: String,
    /// its operator logos (Luya-Z D5: with the program)
    ui_logos: String,
    u60d_plugins: String,
    boot_id_file: String,
    fp: String,
    fp_changed: String,
    standby_stat: String,
    standby_base: String,
    ledger_dir: String,
    ledger_tmp: String,
    crashcap_dir: String,
    keylog: String,
    mss_recovery: String,
    kmsg: String,
    jsonfilter: Vec<String>,
    wan_if: String,
    route: String,
    selftest_wait: i64,
    scorecard_url: String,
    calib_url: String,
    chill: chill::Cfg,
    root: String,
    luyaz_tmp: String,
}

impl Cfg {
    /// CORE_NAME: luyaz-core after the move (`$DOC_ROOT/data/luyaz/core/.migrated`), else zwrt-datad
    fn core_name(&self) -> &'static str {
        if crate::paths::live(&self.root, crate::paths::DATA_NEW) == crate::paths::Live::New {
            "luyaz-core"
        } else {
            "zwrt-datad"
        }
    }

    pub fn from(get: &dyn Fn(&str) -> Option<String>) -> Cfg {
        let s = |k: &str, d: &str| {
            get(k)
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| d.to_owned())
        };
        // unquoted in sh: word-split
        let w = |k: &str, d: &str| {
            s(k, d)
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        // GUARD_TMP=/tmp/luyaz, U60D_PLUGINS=/data/luyaz/core/plugins, UID_STATE=/data/luyaz/screen:
        // literal paths, no root (Luya-Z rename E3: the old trees are gone, no luyaz_dir)
        let data_dir = "/data/luyaz/core";
        let uid_state = "/data/luyaz/screen";
        let guard_tmp = "/tmp/luyaz";
        Cfg {
            ubus: w("DOC_UBUS", "ubus"),
            uci: w("DOC_UCI", "uci"),
            date: w("DOC_DATE", "date"),
            df: w("DOC_DF", "df"),
            uptime: s("DOC_UPTIME", "/proc/uptime"),
            clock_ok: s("DOC_CLOCK_OK", &format!("{guard_tmp}/clock-ok")),
            alerts: s("DOC_ALERTS", "/data/alerts"),
            crash: s("DOC_CRASH", "/data/crashlog"),
            data_dir: s("DOC_DATA", "/data"),
            initd: s("DOC_INITD", "/etc/init.d"),
            rc: s("DOC_RC", "/etc/rc.local"),
            ps: w("DOC_PS", "ps w"),
            wget: w("DOC_WGET", "wget"),
            proc_dir: s("DOC_PROC", "/proc"),
            heartbeat: s("DOC_HEARTBEAT", "/tmp/scenario.heartbeat"),
            marker: s("DOC_MARKER", "/tmp/luyaz/wifiguard.took-over"),
            uid_state: s("DOC_UID_STATE", uid_state),
            uid_want: s("DOC_UID_WANT", "/tmp/luyaz/screen.want"),
            lweb_bin: s("DOC_LWEB_BIN", "/data/luyaz/luyaz-web"),
            overlay_rcd: s("DOC_OVERLAY_RCD", "/zteoverlay/etc-upper_a/rc.d"),
            daemon_conf: s("DOC_DAEMON_CONF", "/etc/config/zte_topsw_daemon.conf"),
            datad_url: s("DOC_DATAD_URL", "http://127.0.0.1:9460/v2/state"),
            ip: w("DOC_IP", "ip"),
            manifest: s("DOC_MANIFEST", crate::paths::MANIFEST),
            ship_txn: s("DOC_SHIP_TXN", "/data/luyaz/ship/txn"),
            ship_hb: s("DOC_SHIP_HB", "/tmp/luyaz/ship/heartbeat"),
            md5_cache: s("DOC_MD5_CACHE", "/tmp/luyaz/doctor/md5"),
            md5sum: get("DOC_MD5SUM")
                .filter(|v| !v.is_empty())
                .map(|v| v.split_whitespace().map(str::to_owned).collect()),
            ts_dir: s("DOC_TS_DIR", "/data/tailscale"),
            ts_tuning: s("DOC_TS_TUNING", "/data/tailscale/tuning.env"),
            ui_fonts: s(
                "DOC_UI_FONTS",
                &if std::path::Path::new("/data/luyaz/ui/fonts").is_dir() {
                    "/data/luyaz/ui/fonts".to_owned()
                } else {
                    format!("{}/fonts", s("DOC_DEVUI_DIR", "/data/plugins/u60pro-devui"))
                },
            ),
            ui_logos: s("DOC_UI_LOGOS", "/data/luyaz/ui/operator-logos"),
            u60d_plugins: s("DOC_U60D_PLUGINS", &format!("{data_dir}/plugins")),
            boot_id_file: s("DOC_BOOT_ID_FILE", "/proc/sys/kernel/random/boot_id"),
            fp: s("DOC_FP", &format!("{guard_tmp}/ledger/fp")),
            fp_changed: s("DOC_FP_CHANGED", &format!("{guard_tmp}/ledger/fp-changed")),
            standby_stat: s("DOC_STANDBY_STAT", "/tmp/standby.stat"),
            standby_base: s("DOC_STANDBY_BASE", "/data/luyaz/scripts/standby.baseline"),
            ledger_dir: s("DOC_LEDGER_DIR", "/data/ledger"),
            ledger_tmp: s("DOC_LEDGER_TMP", &format!("{guard_tmp}/ledger")),
            crashcap_dir: s("DOC_CRASHCAP_DIR", "/data/crashcap"),
            keylog: s("DOC_KEYLOG", "/data/logfs/key.log"),
            mss_recovery: s(
                "DOC_MSS_RECOVERY",
                "/sys/class/remoteproc/remoteproc0/recovery",
            ),
            kmsg: s("DOC_KMSG", "/dev/kmsg"),
            jsonfilter: w("DOC_JSONFILTER", "jsonfilter"),
            wan_if: s("DOC_WAN_IF", "rmnet_data0"),
            route: s("DOC_ROUTE", "/proc/net/route"),
            selftest_wait: digits(&s("DOC_SELFTEST_WAIT", "6")).unwrap_or(6),
            scorecard_url: s("DOC_SCORECARD_URL", "http://127.0.0.1:9460/v2/scorecard"),
            calib_url: s("DOC_CALIB_URL", "http://127.0.0.1:9460/v2/longterm/calib"),
            chill: chill::Cfg {
                env: s("DOC_CHILL_ENV", "/data/chill/chill.env"),
                state: s("DOC_CHILL_STATE", "/tmp/chill.state"),
                ip: w("DOC_IP", "ip"),
                template: s("DOC_CHILL_TEMPLATE", "/data/chill/template.yaml"),
                dir: s("DOC_CHILL_DIR", "/data/chill"),
            },
            root: get("DOC_ROOT").unwrap_or_default(),
            luyaz_tmp: s("DOC_LUYAZ_TMP", "/tmp/luyaz"),
        }
    }
}

/// `$(cmd args)`: stdout, trailing newlines dropped; a command that is not there gives "".
fn sh_out(cmd: &[String], args: &[&str], quiet: bool) -> String {
    strip_nl(sh_raw(cmd, args, quiet))
}

/// The command's stdout as it is (a pipe's input).
fn sh_raw(cmd: &[String], args: &[&str], quiet: bool) -> String {
    let Some((prog, pre)) = cmd.split_first() else {
        return String::new();
    };
    Command::new(prog)
        .args(pre)
        .args(args)
        .stdin(Stdio::null())
        .stderr(if quiet {
            Stdio::null()
        } else {
            Stdio::inherit()
        })
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

fn strip_nl(mut s: String) -> String {
    while s.ends_with('\n') {
        s.pop();
    }
    s
}

/// `case "$x" in '' | *[!0-9]*) x=<fallback>`
fn digits(s: &str) -> Option<i64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

fn cat(path: &str) -> Option<String> {
    std::fs::read(path)
        .ok()
        .map(|b| strip_nl(String::from_utf8_lossy(&b).into_owned()))
}

/// The whole check, in doctor.sh's order. `None` slots are rows of later batches.
pub fn run(c: &Cfg, mode: Mode) -> Vec<Row> {
    let mut rows = Vec::new();
    // the manifest: first line for people, last row for --tsv (R9)
    let m = manifest::state(c, false);
    let mrow = row(m.level, "manifest", "清单", m.text, "Manifest", m.text_en);
    if mode == Mode::Text {
        rows.push(mrow.clone());
    }
    rows.push(boot_sync(c));
    rows.push(fota(c));
    rows.extend(whiteout(c));
    rows.push(phonehome(c));
    rows.push(rpc_acl(c));
    rows.push(clock(c));
    rows.extend(core_move(c));
    rows.extend(tmp_move(c));
    rows.extend(services(c));
    rows.extend(chill::rows(c));
    rows.extend(http(c));
    rows.push(heartbeat(c));
    rows.extend(takeover(c));
    rows.push(wifi(c));
    rows.push(screen(c));
    let unread = unread_alerts(c);
    rows.push(alerts(unread));
    rows.push(sms(c));
    rows.push(crashes(c, unread));
    rows.push(disk(c));
    let (lv, d) = standby::check(c, false);
    let de = if mode == Mode::Tsv2 {
        standby::check(c, true).1
    } else {
        String::new()
    };
    rows.push(row(lv, "standby", "待机", d, "Standby", de));
    if mode != Mode::Text {
        rows.push(mrow);
    }
    rows
}

/// `[ -c ]` names in the overlay's rc.d (`ls -la | awk '/^c/ {print $NF}'`), in ls order
fn char_devs(dir: &str) -> Vec<String> {
    use std::os::unix::fs::FileTypeExt;
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_char_device()))
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn whiteout(c: &Cfg) -> Option<Row> {
    if !std::path::Path::new(&c.overlay_rcd).is_dir() {
        return None;
    }
    let wo = char_devs(&c.overlay_rcd);
    let mut hit = String::new();
    for w in &wo {
        // sed 's/^[SK][0-9]*//'
        let n = match w.strip_prefix(['S', 'K']) {
            Some(r) => r.trim_start_matches(|ch: char| ch.is_ascii_digit()),
            None => w.as_str(),
        };
        if grep_q_e(&format!("^[^#]*[[:space:]]{n}$"), &c.daemon_conf) {
            hit.push(' ');
            hit.push_str(w);
        }
    }
    let d = &c.overlay_rcd;
    Some(if !hit.is_empty() {
        row(
            "bad",
            "whiteout",
            "开机链接",
            format!("被屏蔽的原厂服务:{hit}（删掉 {d} 里对应的字符设备）"),
            "Boot links",
            format!("Stock services masked:{hit} (delete their character devices in {d})"),
        )
    } else {
        let (z, e) = if wo.is_empty() {
            (String::new(), String::new())
        } else {
            // `$(echo $wo | tr '\n' ' ')` keeps a trailing blank; `$(echo $wo)` does not
            let n = wo.len();
            (
                format!("（另有 {n} 个无关的：{} ）", wo.join(" ")),
                format!(" ({n} unrelated: {})", wo.join(" ")),
            )
        };
        row(
            "ok",
            "whiteout",
            "开机链接",
            format!("开机同步名单里的服务没有被屏蔽{z}"),
            "Boot links",
            format!("No boot-sync service is masked{e}"),
        )
    })
}

fn grep_q_e(pattern: &str, file: &str) -> bool {
    Command::new("grep")
        .args(["-qE", pattern, file])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// `sed -n "s/^<key>='*\([^']*\)'*\$/\1/p"` over uci show output, one value per matching line
fn uci_val(out: &str, key: &str) -> String {
    let mut v = Vec::new();
    for l in out.split('\n') {
        if let Some(r) = l.strip_prefix(key).and_then(|r| r.strip_prefix('=')) {
            let r = r.trim_start_matches('\'');
            let end = r.find('\'').unwrap_or(r.len());
            if r[end..].bytes().all(|b| b == b'\'') {
                v.push(&r[..end]);
            }
        }
    }
    v.join("\n")
}

/// first lines of `$PROC/[0-9]*/comm` (`read -r`)
fn comms(c: &Cfg) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(&c.proc_dir) else {
        return Vec::new();
    };
    rd.flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.as_bytes().first().is_some_and(u8::is_ascii_digit))
        })
        .filter_map(|e| std::fs::read(e.path().join("comm")).ok())
        .map(|b| {
            String::from_utf8_lossy(&b)
                .split('\n')
                .next()
                .unwrap_or("")
                .trim_matches([' ', '\t'])
                .to_owned()
        })
        .collect()
}

fn phonehome(c: &Cfg) -> Row {
    let (mut pmq, mut ptr, mut psm) = (false, false, false);
    for comm in comms(c) {
        match comm.as_str() {
            "zte_mqtt_sdk_st" => pmq = true,
            "zte_topsw_tr069" => ptr = true,
            x if x.starts_with("zte_smart_manag") => psm = true,
            _ => {}
        }
    }
    let run = |b: bool| if b { "在跑" } else { "没在跑" };
    let run_en = |b: bool| if b { "running" } else { "not running" };
    let mq = uci_val(
        &sh_raw(&c.uci, &["-q", "show", "zwrt_mqtt.config"], true),
        "zwrt_mqtt.config.mqttOnreportEnable",
    );
    let (mqz, mqe) = match mq.as_str() {
        "1" => (
            format!("MQTT 上报开着（{}）", run(pmq)),
            format!("MQTT reporting on ({})", run_en(pmq)),
        ),
        "0" => ("MQTT 上报关着".to_owned(), "MQTT reporting off".to_owned()),
        _ => ("MQTT 读不到".to_owned(), "MQTT unreadable".to_owned()),
    };
    let trs = sh_out(&c.uci, &["-q", "show", "zwrt_tr069.ManagementServer"], true);
    let tre = uci_val(&trs, "zwrt_tr069.ManagementServer.EnableCWMP");
    let tru = uci_val(&trs, "zwrt_tr069.ManagementServer.URL");
    let (trz, tren) = match tre.as_str() {
        "1" if !tru.is_empty() => (
            format!("TR-069 开着（{}），配了服务器", run(ptr)),
            format!("TR-069 on ({}), server set", run_en(ptr)),
        ),
        "1" => (
            format!("TR-069 开着（{}），没配服务器", run(ptr)),
            format!("TR-069 on ({}), no server set", run_en(ptr)),
        ),
        "0" => ("TR-069 关着".to_owned(), "TR-069 off".to_owned()),
        _ => ("TR-069 读不到".to_owned(), "TR-069 unreadable".to_owned()),
    };
    // grep -c "\.switch='*1'*\$"
    let dd = sh_raw(&c.uci, &["-q", "show", "zwrt_zte_dadian_debug"], true);
    let ddn = dd
        .split('\n')
        .filter(|l| {
            l.match_indices(".switch=").any(|(i, k)| {
                let r = l[i + k.len()..].trim_start_matches('\'');
                r.strip_prefix('1')
                    .is_some_and(|t| t.bytes().all(|b| b == b'\''))
            })
        })
        .count();
    row(
        "ok",
        "phonehome",
        "原厂外联上报",
        format!(
            "{mqz}；{trz}；打点开关 {ddn} 个开着；应用库更新（smart_manage）{}",
            run(psm)
        ),
        "Stock phone-home",
        format!(
            "{mqe}; {tren}; {ddn} analytics switches on; app catalog updates (smart_manage) {}",
            run_en(psm)
        ),
    )
}

/// `$WGET -q -T 5 -O /dev/null URL` succeeded
fn wget_ok(c: &Cfg, url: &str) -> bool {
    let Some((prog, pre)) = c.wget.split_first() else {
        return false;
    };
    Command::new(prog)
        .args(pre)
        .args(["-q", "-T", "5", "-O", "/dev/null", url])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn http(c: &Cfg) -> [Row; 2] {
    [
        if wget_ok(c, "http://127.0.0.1:9090/health") {
            row(
                "ok",
                "agent-http",
                "管理网页 :9090",
                "能访问".into(),
                "Web admin :9090",
                "Reachable".into(),
            )
        } else {
            row(
                "bad",
                "agent-http",
                "管理网页 :9090",
                "打不开".into(),
                "Web admin :9090",
                "Not reachable".into(),
            )
        },
        if wget_ok(c, &c.datad_url) {
            row(
                "ok",
                "datad-http",
                "数据服务 :9460",
                "能访问".into(),
                "Data service :9460",
                "Reachable".into(),
            )
        } else {
            row(
                "bad",
                "datad-http",
                "数据服务 :9460",
                "读不到 /v2/state（屏幕会没有数据）".into(),
                "Data service :9460",
                "Can't read /v2/state (the screen will have no data)".into(),
            )
        },
    ]
}

/// The sed in doctor.sh: the last `"noSyncModuleName": "…"` on each line, one per line.
fn no_sync_name(out: &str) -> String {
    const KEY: &str = "\"noSyncModuleName\":";
    let mut found = Vec::new();
    for line in out.split('\n') {
        let mut hit = None;
        for (i, _) in line.rmatch_indices(KEY) {
            let rest = line[i + KEY.len()..].trim_start_matches(' ');
            if let Some(r) = rest.strip_prefix('"')
                && let Some(end) = r.find('"')
            {
                hit = Some(r[..end].to_owned());
                break;
            }
        }
        found.extend(hit);
    }
    found.join("\n")
}

fn boot_sync(c: &Cfg) -> Row {
    let sync = no_sync_name(&sh_out(
        &c.ubus,
        &["call", "zwrt_topsw_daemon.sync", "get_sync_info", "{}"],
        true,
    ));
    if sync == "sync success" {
        row(
            "ok",
            "boot-sync",
            "开机同步",
            "sync success".into(),
            "Boot sync",
            "All stock services registered (sync success)".into(),
        )
    } else {
        let z = if sync.is_empty() { "读不到" } else { &sync };
        let e = if sync.is_empty() { "Unreadable" } else { &sync };
        row(
            "bad",
            "boot-sync",
            "开机同步",
            format!("{z}（有原厂服务没注册：屏幕卡 logo / 不拨号的根源）"),
            "Boot sync",
            format!(
                "{e} (a stock service did not register: why the screen sticks on the logo or nothing dials)"
            ),
        )
    }
}

/// The stock rpcd ACL lets the anonymous session reach the factory/ADB methods (B31,
/// 2026-10-16). Asked with `session access`, never by calling them; a warning, not an error.
fn rpc_acl(c: &Cfg) -> Row {
    let (mut hit, mut no) = (Vec::new(), 0);
    for f in [
        "fac_open",
        "fac_close",
        "fac_reset",
        "fac_reboot",
        "fac_query_key",
        "adb_switch",
    ] {
        let q = format!(
            r#"{{"ubus_rpc_session":"00000000000000000000000000000000","scope":"ubus","object":"zwrt_mc.device.manager","function":"{f}"}}"#
        );
        let a = sh_out(&c.ubus, &["call", "session", "access", &q], true);
        if a.contains(r#""access": true"#) || a.contains(r#""access":true"#) {
            hit.push(f);
        } else if a.contains(r#""access""#) {
            no += 1;
        }
    }
    if !hit.is_empty() {
        let l = format!(" {}", hit.join(" "));
        return row(
            "warn",
            "rpc-acl",
            "匿名权限",
            format!("不登录也能调原厂出厂/ADB 方法:{l}（原厂 ACL，没收紧）"),
            "Anonymous RPC",
            format!(
                "Stock factory/ADB methods callable without login:{l} (stock ACL, not tightened)"
            ),
        );
    }
    let (z, e) = if no == 6 {
        (
            "出厂/ADB 方法要登录才能调",
            "Factory/ADB methods need a login",
        )
    } else {
        ("读不到", "Unreadable")
    };
    row(
        "ok",
        "rpc-acl",
        "匿名权限",
        z.into(),
        "Anonymous RPC",
        e.into(),
    )
}

fn fota(c: &Cfg) -> Row {
    let mode = sh_out(
        &c.uci,
        &["-q", "get", "zwrt_zte_dm.dm_update.dm_update_mode"],
        false,
    );
    let poll = sh_out(
        &c.uci,
        &["-q", "get", "zwrt_zte_dm.dm_update.TURNOFFPOLLING"],
        false,
    );
    if mode == "0" && poll == "1" {
        return row(
            "ok",
            "fota",
            "ZTE 自动升级",
            "已关闭".into(),
            "ZTE auto-update",
            "Off".into(),
        );
    }
    let q = |s: &str| {
        if s.is_empty() {
            "?".to_owned()
        } else {
            s.to_owned()
        }
    };
    let (m, p) = (q(&mode), q(&poll));
    row(
        "bad",
        "fota",
        "ZTE 自动升级",
        format!("没有完全关闭（dm_update_mode={m} TURNOFFPOLLING={p}）；升级会覆盖 rc.local"),
        "ZTE auto-update",
        format!(
            "Not fully off (dm_update_mode={m} TURNOFFPOLLING={p}); an update would overwrite rc.local"
        ),
    )
}

/// `uptime_s`: `cut -d. -f1 $UPTIME_FILE || echo 0`, then digits or 0.
fn uptime_s(c: &Cfg) -> i64 {
    let Ok(b) = std::fs::read(&c.uptime) else {
        return 0;
    };
    let s = String::from_utf8_lossy(&b);
    let mut lines: Vec<&str> = s.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    let cut: Vec<&str> = lines
        .iter()
        .map(|l| l.split('.').next().unwrap_or(""))
        .collect();
    digits(&strip_nl(cut.join("\n"))).unwrap_or(0)
}

fn clock(c: &Cfg) -> Row {
    use luyaz_api::clock::{BootTime, EvidenceStatus, evidence_status};
    let up = uptime_s(c);
    let t = digits(&sh_out(&c.date, &["+%s"], false))
        .and_then(|v| i64::try_from(v).ok())
        .unwrap_or(0);
    let at = BootTime::from_uptime(
        &cat(&c.boot_id_file).unwrap_or_default(),
        &cat(&c.uptime).unwrap_or_default(),
    );
    let status = evidence_status(&cat(&c.clock_ok).unwrap_or_default(), at.as_ref(), t);
    if status.wall_usable() {
        let dt = sh_out(&c.date, &["+%Y-%m-%d %H:%M"], false);
        let (zh, en) = if status == EvidenceStatus::Verified {
            (
                format!("同步证据已验证（{dt} 设备当地时间）"),
                format!("Verified sync evidence ({dt} device local time)"),
            )
        } else {
            (
                format!("时间为估算（{dt} 设备当地时间），尚未验证同步"),
                format!("Estimated ({dt} device local time); sync unverified"),
            )
        };
        row("ok", "clock", "时钟", zh, "Clock", en)
    } else if up < 300 {
        row(
            "ok",
            "clock",
            "时钟",
            "刚开机，尚无有效时间证据".into(),
            "Clock",
            "Just booted; no valid clock evidence yet".into(),
        )
    } else {
        let (zh, en) = match status {
            EvidenceStatus::Stale => ("时间证据已过期", "Clock evidence expired"),
            EvidenceStatus::BootMismatch => (
                "时间证据属于另一次开机",
                "Clock evidence belongs to another boot",
            ),
            EvidenceStatus::WallChanged | EvidenceStatus::Revoked => {
                ("检测到时间跳变", "Clock step detected")
            }
            _ => ("缺少有效时间证据", "No valid clock evidence"),
        };
        row(
            "warn",
            "clock",
            "时钟",
            format!("{zh}；告警短信暂缓发送"),
            "Clock",
            format!("{en}; alert SMS held"),
        )
    }
}

/// `[ -x path ]`
fn executable(path: &str) -> bool {
    let Ok(p) = std::ffi::CString::new(path) else {
        return false;
    };
    // SAFETY: a valid NUL-terminated path; access only reads it
    unsafe { libc::access(p.as_ptr(), libc::X_OK) == 0 }
}

fn core_move(c: &Cfg) -> Option<Row> {
    match crate::paths::live(&c.root, crate::paths::DATA_NEW) {
        crate::paths::Live::New => {
            let mut lm = String::new();
            let init = format!("{}/luyaz-core", c.initd);
            let prog = format!("{}/data/luyaz/luyaz-core", c.root);
            for p in [init, prog] {
                if !executable(&p) {
                    lm.push(' ');
                    lm.push_str(&p);
                }
            }
            (!lm.is_empty()).then(|| {
                row(
                    "bad",
                    "core-move",
                    "核心搬家",
                    format!("已切到新布局（.migrated），但缺:{lm}（重启后核心起不来；见 docs/designs/u60-phase10.md）"),
                    "Core move",
                    format!("Switched to the new layout (.migrated) but missing:{lm} (the core won't start after a reboot; see docs/designs/u60-phase10.md)"),
                )
            })
        }
        crate::paths::Live::HalfMoved => Some(row(
            "bad",
            "core-move",
            "核心搬家",
            "/data/luyaz/core 在但没有 .migrated：搬了一半，还在用旧路径（旧布局，见 docs/designs/u60-phase10.md）".into(),
            "Core move",
            "/data/luyaz/core exists without .migrated: moved half-way, still on the old paths (old layout, see docs/designs/u60-phase10.md)".into(),
        )),
        crate::paths::Live::Old => None,
    }
}

fn tmp_move(c: &Cfg) -> Option<Row> {
    let d = &c.luyaz_tmp;
    (std::path::Path::new(d).is_dir() && !std::path::Path::new(&format!("{d}/.migrated")).is_file())
        .then(|| {
            row(
                "bad",
                "tmp-move",
                "哨兵暂存目录",
                format!("{d} 在但没有 .migrated：哨兵搬了一半，还在用 /tmp/u60-guard"),
                "Sentinel scratch dir",
                format!("{d} exists without .migrated: moved half-way, still on /tmp/u60-guard"),
            )
        })
}

/// `grep -q PATTERN FILE` (the shell's own grep: the pattern holds `$INITD`, a regex)
fn grep_q(pattern: &str, file: &str) -> bool {
    Command::new("grep")
        .args(["-q", pattern, file])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn rc_starts(c: &Cfg, svc: &str) -> bool {
    grep_q(&format!("^[^#]*{}/{svc} start", c.initd), &c.rc)
}

/// `count_comm`: processes under `$PROC/[0-9]*` whose comm (first line, `read -r`) starts with `prefix`
fn count_comm(c: &Cfg, prefix: &str) -> usize {
    let Ok(rd) = std::fs::read_dir(&c.proc_dir) else {
        return 0;
    };
    rd.flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.as_bytes().first().is_some_and(u8::is_ascii_digit))
        })
        .filter(|e| {
            std::fs::read(e.path().join("comm")).is_ok_and(|b| {
                let s = String::from_utf8_lossy(&b);
                let first = s.split('\n').next().unwrap_or("");
                first.trim_matches([' ', '\t']).starts_with(prefix)
            })
        })
        .count()
}

/// `ubus call service list '{"name":"X"}' | grep -q '"running": *true'`
fn svc_running(c: &Cfg, name: &str) -> bool {
    let out = sh_raw(
        &c.ubus,
        &[
            "call",
            "service",
            "list",
            &format!("{{\"name\":\"{name}\"}}"),
        ],
        true,
    );
    out.match_indices("\"running\":").any(|(i, k)| {
        out[i + k.len()..]
            .trim_start_matches(' ')
            .starts_with("true")
    })
}

fn services(c: &Cfg) -> Vec<Row> {
    let mut rows = Vec::new();
    let core = if c.core_name() == "luyaz-core" {
        "luyaz-core"
    } else {
        "zwrt-datad"
    };
    let mut svcs: Vec<&str> = if rc_starts(c, "luyaz-sentinel") {
        vec![core, "luyaz-sentinel"]
    } else {
        vec![core, "u60-guard", "u60-uid"]
    };
    // zte-agent is retired (8d-2c): none expected; one still running is odd
    if count_comm(c, "zte-agent") >= 1 {
        rows.push(row(
            "warn",
            "svc-zte-agent",
            "高级后台",
            "已退役（8d-2c），却还在跑：核心不会接手它的活".into(),
            "Admin backend",
            "Retired (8d-2c) but still running: the core does not take over its work".into(),
        ));
    } else {
        rows.push(row(
            "ok",
            "svc-zte-agent",
            "高级后台",
            "已退役（核心和新管理网页接替）".into(),
            "Admin backend",
            "Retired (the core and the new admin pages took over)".into(),
        ));
    }
    if std::path::Path::new(&c.lweb_bin).exists() || rc_starts(c, "luyaz-web") {
        svcs.push("luyaz-web");
    }
    for s in svcs {
        let (id, label, label_en): (&'static str, &'static str, &'static str) = match s {
            "zwrt-datad" => ("svc-zwrt-datad", "数据服务", "Data service"),
            "luyaz-core" => ("svc-luyaz-core", "数据服务", "Data service"),
            "u60-guard" => ("svc-u60-guard", "Wi-Fi 兜底看门狗", "Wi-Fi watchdog"),
            "u60-uid" => ("svc-u60-uid", "屏幕守护进程", "Screen supervisor"),
            "luyaz-sentinel" => (
                "svc-luyaz-sentinel",
                "哨兵（屏幕和看门狗）",
                "Sentinel (screen and watchdog)",
            ),
            _ => ("svc-luyaz-web", "管理网页（:9090）", "Admin pages (:9090)"),
        };
        rows.push(if !executable(&format!("{}/{s}", c.initd)) {
            row(
                "warn",
                id,
                label,
                "没装 procd 服务（旧装法：崩了没人拉起）".into(),
                label_en,
                "No procd service (old install: nothing restarts it after a crash)".into(),
            )
        } else if svc_running(c, s) {
            if rc_starts(c, s) {
                row(
                    "ok",
                    id,
                    label,
                    "procd 监督中".into(),
                    label_en,
                    "Supervised by procd".into(),
                )
            } else {
                row(
                    "warn",
                    id,
                    label,
                    "在跑，但 rc.local 里没有启动它：重启后不会起来".into(),
                    label_en,
                    "Running, but rc.local doesn't start it: it won't come back after a reboot"
                        .into(),
                )
            }
        } else {
            row(
                "bad",
                id,
                label,
                format!("procd 服务没在运行（logread 看原因；/etc/init.d/{s} start）"),
                label_en,
                format!("procd service not running (see logread; /etc/init.d/{s} start)"),
            )
        });
    }
    // the core under either name (10e): a leftover old one next to the new is two.
    // The ids keep the old names: tests and saved doctor output key on them; only labels show.
    for (p, id, label, label_en) in [
        (
            "zwrt-datad",
            "dup-zwrt-datad",
            "核心实例数",
            "Core instances",
        ),
        (
            "u60pro-devui",
            "dup-u60pro-devui",
            "触屏实例数",
            "Touch UI instances",
        ),
    ] {
        let mut n = count_comm(c, p);
        if p == "zwrt-datad" {
            n += count_comm(c, "luyaz-core");
        }
        // the touch UI under either name (10h R5): one of each is two owners of card0
        if p == "u60pro-devui" {
            n += count_comm(c, "luyaz-ui");
        }
        if n > 1 {
            rows.push(row(
                "bad",
                id,
                label,
                format!("{n} 份在跑（应当只有 1 份）"),
                label_en,
                format!("{n} running (should be 1)"),
            ));
        }
    }
    rows
}

fn heartbeat(c: &Cfg) -> Row {
    let up = uptime_s(c);
    match cat(&c.heartbeat).and_then(|s| digits(&s)) {
        None => row(
            "bad",
            "heartbeat",
            "后台心跳",
            "没有心跳文件（后台没在运行？）".into(),
            "Admin heartbeat",
            "No heartbeat file (admin backend not running?)".into(),
        ),
        Some(hb) if up - hb <= 120 => {
            let d = up - hb;
            row(
                "ok",
                "heartbeat",
                "后台心跳",
                format!("{d} 秒前"),
                "Admin heartbeat",
                format!("{d} s ago"),
            )
        }
        Some(hb) => {
            let d = up - hb;
            row(
                "bad",
                "heartbeat",
                "后台心跳",
                format!("{d} 秒没更新（情景引擎卡住了？看门狗 5 分钟后会接管 Wi-Fi）"),
                "Admin heartbeat",
                format!(
                    "No update for {d} s (scenario engine stuck? The watchdog takes over Wi-Fi after 5 min)"
                ),
            )
        }
    }
}

fn takeover(c: &Cfg) -> Option<Row> {
    std::path::Path::new(&c.marker).is_file().then(|| {
        row(
            "warn",
            "takeover",
            "Wi-Fi 看门狗",
            "接管中：情景固定暂停，后台稳定 10 分钟后自动交回".into(),
            "Wi-Fi watchdog",
            "In control: scenarios paused; handed back after 10 min of stable admin backend".into(),
        )
    })
}

/// `awk '/\/hostapd( |$)/ && !/awk/ {n++}'` over `$PS`
fn hostapd_count(ps: &str) -> usize {
    ps.split('\n')
        .filter(|l| {
            !l.contains("awk")
                && l.match_indices("/hostapd")
                    .any(|(i, k)| matches!(l[i + k.len()..].chars().next(), None | Some(' ')))
        })
        .count()
}

fn wifi(c: &Cfg) -> Row {
    if hostapd_count(&sh_raw(&c.ps, &[], true)) > 0 {
        return row(
            "ok",
            "wifi",
            "Wi-Fi",
            "在广播".into(),
            "Wi-Fi",
            "Broadcasting".into(),
        );
    }
    // 情景有意关掉的（在家）不算问题：问后台当前情景是不是关 Wi-Fi 的那种
    let public = sh_out(
        &c.wget,
        &[
            "-q",
            "-T",
            "3",
            "-O",
            "-",
            "http://127.0.0.1:9460/v2/api/public/status",
        ],
        true,
    );
    if public.contains("\"wifi_off\":true") {
        row(
            "ok",
            "wifi",
            "Wi-Fi",
            "当前情景关着 Wi-Fi（按设定）".into(),
            "Wi-Fi",
            "Off by the current scenario (as set)".into(),
        )
    } else {
        row(
            "warn",
            "wifi",
            "Wi-Fi",
            "没有在广播，当前情景也没要求关".into(),
            "Wi-Fi",
            "Not broadcasting, and the current scenario doesn't turn it off".into(),
        )
    }
}

fn screen(c: &Cfg) -> Row {
    if std::path::Path::new(&format!("{}/gave-up", c.uid_state)).is_file() {
        row(
            "bad",
            "screen",
            "触屏界面",
            "反复启动失败，已换回原厂界面（长按屏幕右下角 3 秒，或 echo devui > /tmp/luyaz-screen.ctl）".into(),
            "Screen UI",
            "Kept failing to start; stock UI on (hold bottom-right 3 s, or echo devui > /tmp/luyaz-screen.ctl)".into(),
        )
    } else if cat(&c.uid_want).as_deref() == Some("vendor") {
        row(
            "ok",
            "screen",
            "触屏界面",
            "按要求显示原厂界面".into(),
            "Screen UI",
            "Stock UI on, as asked".into(),
        )
    } else if count_comm(c, "u60pro-devui") + count_comm(c, "luyaz-ui") >= 1 {
        match cat(&format!("{}/attempts", c.uid_state)).as_deref() {
            None | Some("") | Some("0") => row(
                "ok",
                "screen",
                "触屏界面",
                "运行中".into(),
                "Screen UI",
                "Running".into(),
            ),
            _ => row(
                "ok",
                "screen",
                "触屏界面",
                "运行中（刚启动，稳定 10 分钟后确认）".into(),
                "Screen UI",
                "Running (just started; confirmed after 10 min stable)".into(),
            ),
        }
    } else {
        row(
            "bad",
            "screen",
            "触屏界面",
            "没在运行".into(),
            "Screen UI",
            "Not running".into(),
        )
    }
}

fn alerts(unread: usize) -> Row {
    if unread > 0 {
        row(
            "warn",
            "alerts",
            "告警",
            format!("{unread} 条未读（管理网页「系统 → 告警」）"),
            "Alerts",
            format!("{unread} unread (Alerts in web admin)"),
        )
    } else {
        row(
            "ok",
            "alerts",
            "告警",
            "没有未读".into(),
            "Alerts",
            "None unread".into(),
        )
    }
}

fn sms(c: &Cfg) -> Row {
    if std::fs::metadata(format!("{}/sms-to", c.alerts)).is_ok_and(|m| m.len() > 0) {
        row(
            "ok",
            "sms",
            "短信告警",
            "已配置".into(),
            "Alert SMS",
            "Set up".into(),
        )
    } else {
        row(
            "warn",
            "sms",
            "短信告警",
            "没配置号码：后台挂了你不会知道".into(),
            "Alert SMS",
            "No number set: you won't hear if the admin backend dies".into(),
        )
    }
}

/// awk's `$1 + 0`: the longest leading number, else 0.
fn awk_num(s: &str) -> f64 {
    let s = s.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let ds = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let mut nd = i - ds;
    if i < b.len() && b[i] == b'.' {
        let j = i + 1;
        let mut k = j;
        while k < b.len() && b[k].is_ascii_digit() {
            k += 1;
        }
        nd += k - j;
        if nd > 0 {
            i = k;
        }
    }
    if nd == 0 {
        return 0.0;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut k = i + 1;
        if k < b.len() && (b[k] == b'+' || b[k] == b'-') {
            k += 1;
        }
        let e0 = k;
        while k < b.len() && b[k].is_ascii_digit() {
            k += 1;
        }
        if k > e0 {
            i = k;
        }
    }
    s[..i].parse().unwrap_or(0.0)
}

/// Unread alerts (the alerts row is batch 2; crashes needs the count now):
/// `awk -F'\t' -v r="$read_seq" 'NF == 5 && $1 + 0 > r + 0' "$ALERTS/queue" | wc -l`
fn unread_alerts(c: &Cfg) -> usize {
    let q = format!("{}/queue", c.alerts);
    if !std::path::Path::new(&q).is_file() {
        return 0;
    }
    let r = cat(&format!("{}/read", c.alerts))
        .and_then(|s| digits(&s))
        .unwrap_or(0) as f64;
    let Ok(b) = std::fs::read(&q) else { return 0 };
    let s = String::from_utf8_lossy(&b);
    let mut recs: Vec<&str> = s.split('\n').collect();
    if recs.last() == Some(&"") {
        recs.pop();
    }
    recs.iter()
        .filter(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            !l.is_empty() && f.len() == 5 && awk_num(f[0]) > r
        })
        .count()
}

fn crashes(c: &Cfg, unread: usize) -> Row {
    // the shell's own find (recursive, -mtime in whole days against the real clock)
    let out = Command::new("find")
        .args([c.crash.as_str(), "-name", "*.log", "-mtime", "-1"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|o| o.stdout)
        .unwrap_or_default();
    let recent = out.iter().filter(|&&b| b == b'\n').count();
    if recent == 0 {
        return row(
            "ok",
            "crashes",
            "崩溃记录",
            "最近 24 小时没有".into(),
            "Crash logs",
            "None in the last 24h".into(),
        );
    }
    if unread > 0 {
        let d = &c.crash;
        row(
            "warn",
            "crashes",
            "崩溃记录",
            format!("最近 24 小时 {recent} 份（{d}）"),
            "Crash logs",
            format!("{recent} in the last 24h ({d})"),
        )
    } else {
        row(
            "ok",
            "crashes",
            "崩溃记录",
            format!("最近 24 小时 {recent} 份，对应告警已读"),
            "Crash logs",
            format!("{recent} in the last 24h; their alerts read"),
        )
    }
}

/// `$DF -k "$DATA_DIR" | tail -n 1 | awk '{print $(NF - 2)}'`
fn df_free(out: &str) -> Option<i64> {
    let s = out.strip_suffix('\n').unwrap_or(out);
    let last = s.rsplit('\n').next().unwrap_or("");
    let f: Vec<&str> = last.split([' ', '\t']).filter(|x| !x.is_empty()).collect();
    // NF == 2 prints $0 (has a blank: not digits); NF < 2 is an awk error (nothing)
    if f.len() < 3 {
        return None;
    }
    digits(f[f.len() - 3]).or_else(|| {
        // all digits but beyond i64: sh compares it as a huge number
        let x = f[f.len() - 3];
        (!x.is_empty() && x.bytes().all(|b| b.is_ascii_digit())).then_some(i64::MAX)
    })
}

fn disk(c: &Cfg) -> Row {
    match df_free(&sh_raw(&c.df, &["-k", &c.data_dir], true)) {
        None => row(
            "warn",
            "disk",
            "/data 空间",
            "读不到".into(),
            "/data space",
            "Unreadable".into(),
        ),
        Some(kb) => {
            let mb = kb / 1024;
            if kb < 20480 {
                row(
                    "bad",
                    "disk",
                    "/data 空间",
                    format!("只剩 {mb} MB"),
                    "/data space",
                    format!("Only {mb} MB left"),
                )
            } else if kb < 102400 {
                row(
                    "warn",
                    "disk",
                    "/data 空间",
                    format!("只剩 {mb} MB"),
                    "/data space",
                    format!("Only {mb} MB left"),
                )
            } else {
                row(
                    "ok",
                    "disk",
                    "/data 空间",
                    format!("剩 {mb} MB"),
                    "/data space",
                    format!("{mb} MB free"),
                )
            }
        }
    }
}

/// `tr '\t\n' '  '`
fn flat(s: &str) -> String {
    s.replace(['\t', '\n'], " ")
}

/// doctor.sh's `report`, all rows, plus text mode's closing line; the exit code (0 iff no `bad`).
pub fn render(mode: Mode, rows: &[Row]) -> (String, i32) {
    let mut out = String::new();
    if mode == Mode::Tsv2 {
        out.push_str("#tsv2\n");
    }
    let (mut nbad, mut nwarn) = (0, 0);
    for r in rows {
        match r.level {
            "bad" => nbad += 1,
            "warn" => nwarn += 1,
            _ => {}
        }
        match mode {
            Mode::Tsv => out.push_str(&format!(
                "{}\t{}\t{}\t{}\n",
                r.level,
                r.id,
                r.label,
                flat(&r.detail)
            )),
            Mode::Tsv2 => out.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\t{}\n",
                r.level,
                r.id,
                r.label,
                flat(&r.detail),
                flat(r.label_en),
                flat(&r.detail_en)
            )),
            Mode::Text => {
                let s = match r.level {
                    "ok" => '●',
                    "warn" => '▲',
                    _ => '■',
                };
                out.push_str(&format!("  {s} {}：{}\n", r.label, r.detail));
            }
        }
    }
    if mode == Mode::Text {
        out.push('\n');
        if nbad == 0 && nwarn == 0 {
            out.push_str("  一切正常\n");
        } else {
            out.push_str(&format!("  {nbad} 项异常，{nwarn} 项需要注意\n"));
        }
    }
    (out, if nbad == 0 { 0 } else { 1 })
}

/// Renames the calling thread (the main thread: the process's comm) to [`COMM`].
pub fn set_comm() {
    #[cfg(target_os = "linux")]
    {
        let name = std::ffi::CString::new(COMM).expect("no NUL");
        // SAFETY: PR_SET_NAME reads a NUL-terminated string of at most 16 bytes
        unsafe {
            libc::prctl(libc::PR_SET_NAME, name.as_ptr() as libc::c_ulong, 0, 0, 0);
        }
    }
}

/// `--report 24h|7d [--summary]`: the scorecard and its exit code.
fn report(c: &Cfg, w: Option<&str>, sum: Option<&str>) -> (String, i32) {
    let w = match w {
        Some("24h") => 86_400.0,
        Some("7d") => 604_800.0,
        _ => return ("用法：doctor.sh --report 24h|7d\n".into(), 2),
    };
    let summary = sum == Some("--summary");
    let (lv, det) = standby::check(c, false);
    let mut body = format!("{lv}\t{det}\n");
    if !summary {
        for r in run(c, Mode::Tsv).iter().filter(|r| r.level != "ok") {
            body.push_str(&format!("{}：{}\n", r.label, r.detail));
        }
    }
    let p = crate::scorecard::live::Paths::host();
    let card = crate::scorecard::live::card(&p, w, summary, Some(&body));
    // the last line is `rc=<n>`
    let body = card.strip_suffix('\n').unwrap_or(&card);
    let (text, last) = body.rsplit_once('\n').map_or(("", body), |(t, l)| (t, l));
    let rc = last
        .strip_prefix("rc=")
        .and_then(|n| n.parse().ok())
        .unwrap_or(2);
    let text = if text.is_empty() {
        String::new()
    } else {
        format!("{text}\n")
    };
    (text, rc)
}

/// `<core> doctor ARGS`: the exit code. Like doctor.sh only the first argument counts.
/// Text mode's last lines: the scorecard's summary, the core's card now (`report_core 7d`, the
/// standby verdict as the body), else the ledger's summary file as it was last written.
pub fn footer(c: &Cfg) -> String {
    let (lv, d) = standby::check(c, false);
    let dir = std::env::temp_dir().join(format!("doctor-summary.{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let body = dir.join("body");
    let _ = std::fs::write(&body, format!("{lv}\t{d}\n"));
    let url = format!("{}?w=7d&summary=1", c.scorecard_url);
    let resp = sh_raw(
        &c.wget,
        &[
            "-q",
            "-T",
            "5",
            "--post-file",
            &body.to_string_lossy(),
            "-O",
            "-",
            &url,
        ],
        true,
    );
    let _ = std::fs::remove_dir_all(&dir);
    // `tail -n 1 | sed -n 's/^rc=\([0-9][0-9]*\)$/\1/p'`, then `sed '$d'`
    let r = resp.strip_suffix('\n').unwrap_or(&resp);
    let (head, last) = match r.rfind('\n') {
        Some(i) => (&r[..=i], &r[i + 1..]),
        None => ("", r),
    };
    if !resp.is_empty()
        && last
            .strip_prefix("rc=")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    {
        return format!("\n{head}");
    }
    match std::fs::read(format!("{}/summary", c.ledger_dir)) {
        Ok(b) if !b.is_empty() => format!("\n{}", String::from_utf8_lossy(&b)),
        _ => String::new(),
    }
}

/// The whole output of one mode and its exit code.
pub fn full(c: &Cfg, mode: Mode) -> (String, i32) {
    let (mut out, rc) = render(mode, &run(c, mode));
    if mode == Mode::Text {
        out.push_str(&footer(c));
    }
    (out, rc)
}

/// `<core> doctor ARGS`: the exit code. Like doctor.sh only the first argument counts
/// (and the second for --tree-fp).
pub fn main(args: &[String]) -> i32 {
    set_comm();
    let cfg = Cfg::from(&|k| std::env::var(k).ok());
    let (out, rc) = match args.first().map(String::as_str) {
        Some("--tsv") => full(&cfg, Mode::Tsv),
        Some("--tsv2") => full(&cfg, Mode::Tsv2),
        Some("--manifest") => (manifest::report(&cfg), 0),
        Some("--tree-fp") => match manifest::tree_fp(args.get(1).map(String::as_str).unwrap_or(""))
        {
            Some(f) => (format!("{f}\n"), 0),
            None => (String::new(), 1),
        },
        Some("--ledger-selftest") => selftest::run(&cfg),
        Some("--calibrate-standby") => standby::calibrate(&cfg),
        // the core's card, fed as doctor-legacy.sh fed /v2/scorecard: the standby row first,
        // then (without --summary) this run's rows that are not ok
        Some("--report") => report(
            &cfg,
            args.get(1).map(String::as_str),
            args.get(2).map(String::as_str),
        ),
        _ => full(&cfg, Mode::Text),
    };
    use std::io::Write;
    let mut so = std::io::stdout().lock();
    let _ = so.write_all(out.as_bytes());
    let _ = so.flush();
    rc
}

mod chill;
mod manifest;
mod selftest;
mod standby;
#[cfg(test)]
mod tests;
