#!/bin/sh
# doctor.sh with every device command stubbed (busybox container).
# SPDX-License-Identifier: MIT
SCRIPTS=${SCRIPTS:-/scripts}
PASS=0; FAIL=0
ok() { PASS=$((PASS + 1)); echo "  ok   $1"; }
bad() { FAIL=$((FAIL + 1)); echo "  FAIL $1"; }
check() { _d=$1; shift; if eval "$@"; then ok "$_d"; else bad "$_d  [$*]"; fi; }
MCOL=

setup() {
    T=$(mktemp -d); mkdir -p "$T/bin" "$T/initd" "$T/alerts" "$T/crash" "$T/uid" "$T/rcd" "$T/proc/100" "$T/proc/101"
    echo u60pro-devui >"$T/proc/100/comm"; echo zwrt-datad >"$T/proc/101/comm"
    echo "1000.5 1" >"$T/uptime"; echo 995 >"$T/hb"
    echo "sync success" >"$T/sync"; echo 0 >"$T/mode"; echo 1 >"$T/poll"; echo 8 >"$T/aps"
    for s in zte-agent zwrt-datad u60-guard u60-uid; do printf '#!/bin/sh\n' >"$T/initd/$s"; chmod +x "$T/initd/$s"; done
    # /etc/init.d/zte-agent stays on the device, rc.local no longer starts it (8d-2b)
    { echo "#!/bin/sh"; for s in zwrt-datad u60-guard u60-uid; do echo "$T/initd/$s start"; done; echo "exit 0"; } >"$T/rc"
    printf 'sync    ALL        zte_topsw_devui\nsync    ALL        zte_topsw_wlan\n' >"$T/daemon.conf"
    echo "+8612300000000" >"$T/alerts/sms-to"
    cat >"$T/bin/ubus" <<X
#!/bin/sh
case "\$2 \$3" in
  "zwrt_topsw_daemon.sync get_sync_info") echo "{ \"noSyncModuleName\": \"\$(cat $T/sync)\" }" ;;
  "service list") [ -f $T/down-\$(echo "\$4" | sed 's/.*"name":"\([^"]*\)".*/\1/') ] && echo '{}' || echo '{ "x": { "instances": { "i": { "running": true } } } }' ;;
esac
X
    cat >"$T/bin/uci" <<X
#!/bin/sh
case "\$*" in
  *dm_update_mode*) cat $T/mode ;; *TURNOFFPOLLING*) cat $T/poll ;;
  "-q show zwrt_mqtt.config") cat $T/mqtt 2>/dev/null ;;
  "-q show zwrt_tr069.ManagementServer") cat $T/tr069 2>/dev/null ;;
  "-q show zwrt_zte_dadian_debug") cat $T/dadian 2>/dev/null ;;
esac
X
    # stock phone-home as on the device (10-04, B31): reporting on, TR-069 without a server
    printf "zwrt_mqtt.config=config\nzwrt_mqtt.config.reportFreqInS='1000'\nzwrt_mqtt.config.mqttOnreportEnable='1'\n" >"$T/mqtt"
    printf "zwrt_tr069.ManagementServer=zudata_mgt\nzwrt_tr069.ManagementServer.EnableCWMP='1'\nzwrt_tr069.ManagementServer.PeriodicInformInterval='3600'\n" >"$T/tr069"
    printf "zwrt_zte_dadian_debug.wlan.switch='1'\nzwrt_zte_dadian_debug.wlan.appid='1'\nzwrt_zte_dadian_debug.web.switch='1'\nzwrt_zte_dadian_debug.sms.switch='0'\n" >"$T/dadian"
    cat >"$T/bin/ps" <<X
#!/bin/sh
n=\$(cat $T/aps); i=0; while [ \$i -lt \$n ]; do echo "1 root 0 S /usr/sbin/hostapd -g x"; i=\$((i+1)); done
X
    printf '#!/bin/sh\ncase "$*" in *public/status*) cat %s/public 2>/dev/null ;; esac\nexit 0\n' "$T" >"$T/bin/wget"
    # fixed clock and disk, so the outputs can be compared with the snapshots below
    printf '#!/bin/sh\ncase "$1" in +%%s) cat %s/now ;; *) echo "2026-09-27 10:00" ;; esac\n' "$T" >"$T/bin/date"
    printf '#!/bin/sh\necho "Filesystem 1K-blocks Used Available Use%% Mounted on"\necho "/dev/ubi0 4000000 2800000 1200000 70%% /data"\n' >"$T/bin/df"
    echo 1790500000 >"$T/now"
    chmod +x "$T"/bin/*
    export DOC_UBUS=$T/bin/ubus DOC_UCI=$T/bin/uci DOC_PS=$T/bin/ps DOC_WGET=$T/bin/wget DOC_RC=$T/rc \
        DOC_INITD=$T/initd DOC_UPTIME=$T/uptime DOC_HEARTBEAT=$T/hb DOC_MARKER=$T/marker DOC_ALERTS=$T/alerts \
        DOC_CRASH=$T/crash DOC_UID_STATE=$T/uid DOC_OVERLAY_RCD=$T/rcd DOC_DAEMON_CONF=$T/daemon.conf DOC_DATA=/ DOC_PROC=$T/proc \
        DOC_DATE=$T/bin/date DOC_DF=$T/bin/df DOC_UID_WANT=$T/uid.want DOC_CLOCK_OK=$T/clock-ok DOC_WALL_LAST=$T/wall.last \
        DOC_FP=$T/fp DOC_FP_CHANGED=$T/fp-changed DOC_TS_TUNING=$T/tuning.env \
        DOC_ROOT=$T/root DOC_LUYAZ_TMP=$T/tmp-luyaz
}
# Every run also checks --tsv2 against --tsv (docs/designs/ui-english.md R2):
# first line "#tsv2", then exactly 6 columns, columns 1-4 equal to the --tsv
# row byte for byte, the English columns 5-6 filled, printable ASCII, no
# "Please", no closing full stop (docs/DESIGN.md §1 item 6); same exit code.
run1() { sh "$SCRIPTS/doctor.sh" --tsv >"$T/out"; RC=$?; }
run() { run1; tsv2_check; }
tsv2_lint() { # tsv2_lint <--tsv output> <--tsv2 output>: prints what is wrong
    [ "$(head -n 1 "$2")" = "#tsv2" ] || echo "first line is not #tsv2"
    tail -n +2 "$2" | awk -F'\t' 'NF != 6 { print "not 6 columns: " $0 }'
    tail -n +2 "$2" | cut -f1-4 | cmp -s - "$1" || echo "columns 1-4 differ from --tsv"
    tail -n +2 "$2" | awk -F'\t' '$5 == "" || $6 == "" { print "English missing: " $2 }'
    tail -n +2 "$2" | cut -f5,6 | tr -d ' -~\t\n' | grep -q . && echo "English columns not printable ASCII"
    tail -n +2 "$2" | cut -f5,6 | grep -i -q please && echo "English says Please"
    tail -n +2 "$2" | awk -F'\t' '$5 ~ /\.$/ || $6 ~ /\.$/ { print "closing full stop: " $2 }'
}
TSV2_N=0
tsv2_check() {
    TSV2_N=$((TSV2_N + 1))
    sh "$SCRIPTS/doctor.sh" --tsv2 >"$T/out2"; RC2=$?
    TSV2_BAD=$(tsv2_lint "$T/out" "$T/out2" | head -n 5)
    check "--tsv2 #$TSV2_N agrees with --tsv, English clean" '[ -z "$TSV2_BAD" ] && [ "$RC2" = "$RC" ]'
    [ -z "$TSV2_BAD" ] || echo "       $TSV2_BAD"
}
level() { awk -F'\t' -v id="$1" '$2 == id {print $1}' "$T/out"; }

echo "the --tsv2 lint itself"
T=$(mktemp -d)
printf 'ok\ta\t甲\t乙\nok\tb\t甲\t乙\nok\tc\t甲\t乙\nok\td\t甲\t乙\n' >"$T/t1"
printf 'old doctor\nok\ta\t甲\t乙\tA\t中文\nok\tb\t甲\t乙\tB\tDone.\nok\tc\t甲\t乙\tC\tPlease wait\nok\td\t甲\t乙\tD\n' >"$T/t2"
L=$(tsv2_lint "$T/t1" "$T/t2")
has_all() { for w in "first line is not #tsv2" "not 6 columns" "not printable ASCII" "says Please" "closing full stop: b"; do case "$L" in *"$w"*) ;; *) return 1 ;; esac; done; }
check "it catches a missing marker, 5 columns, Chinese, Please, a full stop" has_all
{ echo "#tsv2"; printf 'ok\ta\t甲\t乙\tA\tFine (x 1)\n'; } >"$T/t2"; printf 'ok\ta\t甲\t乙\n' >"$T/t1"
check "and passes a clean one" '[ -z "$(tsv2_lint "$T/t1" "$T/t2")" ]'
rm -rf "$T"

echo "healthy device"
setup; run
check "every line has 4 fields and a known level" '! awk -F"\t" "NF != 4 || (\$1 != \"ok\" && \$1 != \"warn\" && \$1 != \"bad\")" $T/out | grep -q .'
check "no bad checks, exit 0" '[ $RC = 0 ] && ! grep -q "^bad" $T/out'
check "services ok" '[ "$(level svc-zte-agent)" = ok ] && [ "$(level svc-u60-uid)" = ok ]'
check "fota ok" '[ "$(level fota)" = ok ]'
rm -rf "$T"

echo "after the sentinel took over (5f)"
setup
sed -i "s#.*u60-uid start#$T/initd/luyaz-sentinel start#; /u60-guard start/d" "$T/rc"
for f in u60-uid u60-guard; do rm -f "$T/initd/$f"; touch "$T/down-$f"; done
cp "$T/initd/zte-agent" "$T/initd/luyaz-sentinel" 2>/dev/null
run
check "the sentinel is checked, the old two are not" '[ -n "$(level svc-luyaz-sentinel)" ] && [ -z "$(level svc-u60-uid)" ] && [ -z "$(level svc-u60-guard)" ]'
rm -rf "$T"

echo "luyaz-web in rc.local"
setup
run
check "no luyaz-web line: not checked" '[ -z "$(level svc-luyaz-web)" ]'
sed -i "s#.*zwrt-datad start#&\\n$T/initd/luyaz-web start#" "$T/rc"
cp "$T/initd/zte-agent" "$T/initd/luyaz-web" 2>/dev/null
touch "$T/down-luyaz-web"
run
check "its line: checked, not running = bad" '[ "$(level svc-luyaz-web)" = bad ]'
rm -rf "$T"
setup
: >"$T/luyaz-web"
export DOC_LWEB_BIN=$T/luyaz-web
cp "$T/initd/zte-agent" "$T/initd/luyaz-web" 2>/dev/null
run
unset DOC_LWEB_BIN
check "program there, running, no rc.local line: warn (won't come back after a reboot)" '[ "$(level svc-luyaz-web)" = warn ]'
rm -rf "$T"

echo "problems are reported"
setup
echo "zte_topsw_devui" >"$T/sync"; echo 1 >"$T/mode"; echo 0 >"$T/aps"
touch "$T/down-u60-guard"; echo 500 >"$T/hb"; touch "$T/uid/gave-up"; : >"$T/alerts/sms-to"
mknod "$T/rcd/S99zte_topsw_devui" c 0 0 2>/dev/null
grep -v "u60-uid start" "$T/rc" >"$T/rc2"; mv "$T/rc2" "$T/rc"
run
check "exit non-zero" '[ $RC != 0 ]'
check "boot sync failure: bad" '[ "$(level boot-sync)" = bad ]'
check "auto-update on: bad" '[ "$(level fota)" = bad ]'
check "stopped service: bad" '[ "$(level svc-u60-guard)" = bad ]'
check "running but not in rc.local: warn" '[ "$(level svc-u60-uid)" = warn ]'
check "stale heartbeat: bad" '[ "$(level heartbeat)" = bad ]'
check "screen gave up: bad" '[ "$(level screen)" = bad ]'
check "no SMS number: warn" '[ "$(level sms)" = warn ]'
mkdir -p "$T/proc/102"; echo zwrt-datad >"$T/proc/102/comm"; run
check "two zwrt-datad processes: bad" '[ "$(level dup-zwrt-datad)" = bad ]'
check "Wi-Fi off, scenario did not ask: warn" '[ "$(level wifi)" = warn ]'
echo '{"ok":true,"data":{"scenario":{"current":"home","wifi_off":true}}}' >"$T/public"; run
check "Wi-Fi off because the scenario turns it off: ok" '[ "$(level wifi)" = ok ]'
touch "$T/crash/x.log"; run
check "crash log, its alert read: ok" '[ "$(level crashes)" = ok ]'
printf '9\t1\tcrash\tx\ty\n' >"$T/alerts/queue"; run
check "crash log with unread alerts: warn" '[ "$(level crashes)" = warn ]'
[ -c "$T/rcd/S99zte_topsw_devui" ] && check "whiteout on a boot-barrier daemon: bad" '[ "$(level whiteout)" = bad ]'
rm -rf "$T"

echo "zte-agent retired (8d-2c: no marker needed)"
setup
rm -f "$T/initd/zte-agent"
printf '#!/bin/sh\necho "$*" >>%s/wget.log\ncase "$*" in *public/status*) cat %s/public 2>/dev/null ;; esac\nexit 0\n' "$T" "$T" >"$T/bin/wget"
echo 0 >"$T/aps"; echo '{"ok":true,"data":{"scenario":{"current":"home","wifi_off":true}}}' >"$T/public"
run
check "no agent, no init: ok, says retired" '[ "$(level svc-zte-agent)" = ok ] && grep -q "已退役" $T/out'
check ":9090 probed at luyaz-web's /health" 'grep -q "127.0.0.1:9090/health" $T/wget.log && ! grep -q "9090/api" $T/wget.log'
check "Wi-Fi reason from u60d's /v2/api/public/status" '[ "$(level wifi)" = ok ] && grep -q "127.0.0.1:9460/v2/api/public/status" $T/wget.log'
mkdir -p "$T/proc/103"; echo zte-agent >"$T/proc/103/comm"; run
check "retired but still running: warn" '[ "$(level svc-zte-agent)" = warn ]'
rm -rf "$T"

echo "old install (no procd services)"
setup; rm -f "$T"/initd/*; run
check "missing services are warnings, not errors" '[ "$(level svc-zwrt-datad)" = warn ]'
rm -rf "$T"

echo "the core's move (Phase 10 10e)"
setup
mkdir -p "$T/root/data/luyaz/core"; : >"$T/root/data/luyaz/core/.migrated"
printf 'bin\n' >"$T/root/data/luyaz/luyaz-core"; chmod 755 "$T/root/data/luyaz/luyaz-core"
printf '#!/bin/sh\n' >"$T/initd/luyaz-core"; chmod +x "$T/initd/luyaz-core"
sed -i "s#.*zwrt-datad start#$T/initd/luyaz-core start#" "$T/rc"
echo luyaz-core >"$T/proc/101/comm"
run
check "moved: the core is checked as luyaz-core, supervised, in rc.local" '[ "$(level svc-luyaz-core)" = ok ] && [ -z "$(level svc-zwrt-datad)" ]'
check "moved and complete: no core-move row, exit 0" '[ -z "$(level core-move)" ] && [ $RC = 0 ]'
mkdir -p "$T/proc/102"; echo zwrt-datad >"$T/proc/102/comm"; run
check "an old zwrt-datad next to luyaz-core: two cores, bad" '[ "$(level dup-zwrt-datad)" = bad ]'
rm -rf "$T/proc/102" "$T/initd/luyaz-core"; run
check "switch there, new init missing: core-move bad" '[ "$(level core-move)" = bad ] && grep -q "luyaz-core" "$T/out"'
rm -rf "$T"
setup
mkdir -p "$T/root/data/luyaz/core"; run
check "/data/luyaz/core without .migrated: half-way, bad; still the old names" '[ "$(level core-move)" = bad ] && [ "$(level svc-zwrt-datad)" = ok ]'
rm -rf "$T"
setup
mkdir -p "$T/tmp-luyaz"; run
check "/tmp/luyaz without .migrated: half-way, bad" '[ "$(level tmp-move)" = bad ]'
: >"$T/tmp-luyaz/.migrated"; run
check "/tmp/luyaz with .migrated: no row" '[ -z "$(level tmp-move)" ]'
rm -rf "$T"

echo "clock (docs/LEDGER.md §7)"
setup
echo 1735948800 >"$T/now"; run # 2025-01-04: the device before NTP
check "before NTP, up 1000 s: warn" '[ "$(level clock)" = warn ]'
echo "100.0 1" >"$T/uptime"; run
check "before NTP in the first 5 min: ok, still syncing" '[ "$(level clock)" = ok ] && grep -q "刚开机" $T/out'
echo "1000.5 1" >"$T/uptime"; echo 1790500000 >"$T/now"
echo "0 1790499000 990 none" >"$T/clock-ok"; run
check "u60-guard says not trusted (fresh): warn even in 2026" '[ "$(level clock)" = warn ]'
echo "0 1790499000 100 none" >"$T/clock-ok"; run
check "its verdict is stale: decide here (2026, no last clock): ok" '[ "$(level clock)" = ok ]'
rm -f "$T/clock-ok"; echo $((1790500000 + 3 * 86400)) >"$T/wall.last"; run
check "earlier than the last trusted clock minus a day: warn" '[ "$(level clock)" = warn ]'
rm -rf "$T"

fpfix() { # u60-guard's fingerprint cache: "<program> <pid> <md5>"
    printf 'tailscaled 501 aaaaaaaa\n'
    printf 'u60pro-devui 503 cccccccc\nzwrt-datad 504 dddddddd\nzte-agent 505 eeeeeeee\n'
}
echo "standby sentinel"
setup; export DOC_STANDBY_STAT=$T/sb DOC_STANDBY_BASE=$T/sb.base   # uptime is 1000: recent = 100 and later
fpfix >"$T/fp" # u60-guard's fingerprint cache
detail() { awk -F'\t' -v id="$1" '$2 == id {print $4}' "$T/out"; }
rows() { # rows <n> <first uptime> <wan pkts/min> [tailscaled wakeups/s]
    _i=0; while [ $_i -lt "$1" ]; do echo "$(($2 + _i * 60)) $3 20 ${4:-30.0}$MCOL 2.0 1.0 0.5"; _i=$((_i + 1)); done
}
run
check "no baseline: ok, says not calibrated" '[ "$(level standby)" = ok ] && detail standby | grep -q 未校准'
rows 10 100 60 >"$T/sb"
sh "$SCRIPTS/doctor.sh" --calibrate-standby >"$T/cal"; RCC=$?
check "calibration refuses fewer than 30 lines" '[ $RCC != 0 ] && [ ! -f "$T/sb.base" ]'
: >"$T/sb"; i=0; while [ $i -lt 40 ]; do echo "$((100 + i * 20)) $((50 + i)) 20 30.0$MCOL 2.0 1.0 0.5" >>"$T/sb"; i=$((i + 1)); done
sh "$SCRIPTS/doctor.sh" --calibrate-standby >"$T/cal"; RCC=$?
check "calibration: median 69.5, MAD 10.0 for 50..89" '[ $RCC = 0 ] && grep -qx "2 69.5 10.0" "$T/sb.base"'
check "calibration: constant column has MAD 0" 'grep -qx "3 20.0 0.0" "$T/sb.base"'
rows 5 700 300 >"$T/sb"; run
check "too few recent rows: no verdict" '[ "$(level standby)" = ok ] && detail standby | grep -q 不判定'
rows 10 300 72 >"$T/sb"; run
check "within the baseline: ok" '[ "$(level standby)" = ok ] && detail standby | grep -q "正常（蜂窝每分钟 72"'
rows 10 300 200 >"$T/sb"; run
check "cellular traffic well above the idle baseline: someone is using it, not judged" '[ "$(level standby)" = ok ] && detail standby | grep -q "有流量在用，不判待机"'
rows 10 300 70 90.0 >"$T/sb"; run
check "idle, one program waking far more: warn names it" '[ "$(level standby)" = warn ] && detail standby | grep -q "tailscaled唤醒/秒 90（基线 30）"'
{ rows 4 300 70; echo "600 70 20 - - - - -"; rows 5 660 70; } >"$T/sb"; run
check "missing values (-) are skipped" '[ "$(level standby)" = ok ]'
: >"$T/sb"; i=1; while [ $i -le 10 ]; do echo "$((i * 9)) 500 20 30.0$MCOL 2.0 1.0 0.5" >>"$T/sb"; i=$((i + 1)); done; run   # all before uptime 100
check "old rows (over 15 min) are ignored" '[ "$(level standby)" = ok ] && detail standby | grep -q 不判定'
unset DOC_STANDBY_STAT DOC_STANDBY_BASE
rm -rf "$T"

echo "standby fingerprints (docs/LEDGER.md §12)"
setup; export DOC_STANDBY_STAT=$T/sb DOC_STANDBY_BASE=$T/sb.base
fpfix >"$T/fp"
echo "TS_NO_LOGS_NO_SUPPORT=true" >"$T/tuning.env"
: >"$T/sb"; i=0; while [ $i -lt 40 ]; do echo "$((100 + i * 20)) $((50 + i)) 20 30.0$MCOL 2.0 1.0 0.5" >>"$T/sb"; i=$((i + 1)); done
sh "$SCRIPTS/doctor.sh" --calibrate-standby >/dev/null
check "calibration writes version 2 with a fingerprint per column" '[ "$(head -n 1 $T/sb.base)" = v2 ] && grep -q "^fp 4 aaaaaaaa+" $T/sb.base'
rows 10 300 70 90.0 >"$T/sb"
sed -i 's/^tailscaled 501 aaaaaaaa/tailscaled 601 ffffffff/' "$T/fp"; run
check "new tailscaled binary: its columns are stale, not judged" '[ "$(level standby)" = ok ] && detail standby | grep -q "tailscaled唤醒/秒 基线过期"'
sed -i 's/^tailscaled 601 ffffffff/tailscaled 601 aaaaaaaa/' "$T/fp"; run
check "same binary again: judged (warn)" '[ "$(level standby)" = warn ]'
echo "TS_DEBUG_FIREWALL_MODE=auto" >>"$T/tuning.env"; run
check "Tailscale tuning changed: stale, not judged" '[ "$(level standby)" = ok ] && detail standby | grep -q "基线过期"'
echo "TS_NO_LOGS_NO_SUPPORT=true" >"$T/tuning.env"; echo 800 >"$T/fp-changed"; run
check "only records after the last binary change are judged" '[ "$(level standby)" = ok ] && detail standby | grep -q "只有 1 行"'
rm -f "$T/fp-changed" "$T/fp"; run
check "no fingerprint cache from u60-guard: program columns are stale" '[ "$(level standby)" = ok ] && detail standby | grep -q "基线过期"'
printf '2 69.5 10.0\n3 20.0 0.0\n4 30.0 0.0\n' >"$T/sb.base"; run
check "old-format baseline: not judged, asks for a recalibration" '[ "$(level standby)" = ok ] && detail standby | grep -q "基线格式旧"'
unset DOC_STANDBY_STAT DOC_STANDBY_BASE
rm -rf "$T"

echo "standby: the retired agent's column (8d-2c)"
setup; export DOC_STANDBY_STAT=$T/sb DOC_STANDBY_BASE=$T/sb.base
fpfix >"$T/fp"
: >"$T/sb"; i=0; while [ $i -lt 40 ]; do echo "$((100 + i * 20)) $((50 + i)) 20 30.0$MCOL 2.0 1.0 0.5" >>"$T/sb"; i=$((i + 1)); done
sh "$SCRIPTS/doctor.sh" --calibrate-standby >/dev/null
rows 10 300 70 | sed 's/ 0.5$/ 9.0/' >"$T/sb"; run
check "retired: its column is neither judged nor stale" '[ "$(level standby)" = ok ] && ! detail standby | grep -q "后台唤醒"'
unset DOC_STANDBY_STAT DOC_STANDBY_BASE
rm -rf "$T"

# ── snapshots ───────────────────────────────────────────────────────────────
# The --tsv rows go verbatim onto the touch screen and the web health page, so
# every word is pinned here. A deliberate wording change regenerates them:
#   docker run --rm -v "$PWD/scripts":/scripts busybox:latest \
#       sh -c 'DOCTOR_GOLDEN_UPDATE=1 sh /scripts/test/doctor/run.sh'
# and the diff of golden/ shows exactly which rows moved.
echo "the scorecard: --report (docs/LEDGER.md §11)"
# A made-up ledger: boot 1 from wall W0, hours of lines written by an awk
# generator, events added per case. Every hour at home, fully covered.
W0=1790500000
lsetup() {
    setup
    export DOC_LEDGER_DIR=$T/ledger DOC_CRASHCAP_DIR=$T/crashcap
    mkdir -p "$T/ledger" "$T/crashcap"
    SEG=$T/ledger/boot-000001-aaaaaaaa-001.jsonl
    : >"$SEG"
}
# hours <seq> <wall at uptime 0> <first from> <count> [<awk: per-hour overrides>]
XF=
hours() {
    awk -v s="$1" -v w0="$2" -v u0="$3" -v cnt="$4" -v xf="$XF" "BEGIN {
        for (i = 0; i < cnt; i++) {
            f = u0 + i * 3600; t = f + 3600; gw = 0; ab = 0; net = \"460-00\"; cpu = 10; rss = 20000; thr = 0; e = 900.0
            ton = 3600; tck = 3600; tok = 3600; old = 0 # Tailscale on, checked, healthy; old: a line from before the check
            $5
            tsx = (old ? \",\\\"ts_ok_s\\\":null\" : sprintf(\",\\\"ts_chk_s\\\":%d,\\\"ts_ok_s\\\":%d\", tck, tok))
            printf \"{\\\"v\\\":1,\\\"seq\\\":%d,\\\"n\\\":1,\\\"up\\\":%d,\\\"t\\\":%d,\\\"k\\\":\\\"hour\\\",\\\"id\\\":\\\"h-%d-%d\\\",\\\"from\\\":%d,\\\"to\\\":%d,\\\"ft\\\":%d,\\\"tt\\\":%d,\\\"awake\\\":3600,\\\"asleep\\\":0,\\\"rounds\\\":60,\\\"skipped\\\":0,\\\"max_gap\\\":60,\\\"net\\\":\\\"%s\\\",\\\"home\\\":\\\"460-00\\\",\\\"abroad\\\":%d,\\\"free_mb\\\":900,\\\"dump_mb\\\":1,\\\"budget\\\":0,\\\"g_guard\\\":0,\\\"g_job\\\":0,\\\"g_watcher\\\":%d,\\\"g_capture\\\":0,\\\"g_uidlog\\\":0,\\\"g_crashlog\\\":0,\\\"g_datad\\\":0}\n\", s, t, w0 + t, s, f, f, t, w0 + f, w0 + t, net, ab, gw
            printf \"{\\\"v\\\":1,\\\"seq\\\":%d,\\\"n\\\":2,\\\"up\\\":%d,\\\"t\\\":%d,\\\"k\\\":\\\"hour_power\\\",\\\"id\\\":\\\"hp-%d-%d\\\",\\\"src\\\":\\\"counter\\\",\\\"e\\\":%.1f,\\\"on_home_s\\\":600,\\\"on_home_e\\\":%.1f,\\\"on_abroad_s\\\":0,\\\"on_abroad_e\\\":0.0,\\\"off_home_s\\\":3000,\\\"off_home_e\\\":%.1f,\\\"off_abroad_s\\\":0,\\\"off_abroad_e\\\":0.0,\\\"chg_s\\\":0,\\\"batt_from\\\":80,\\\"batt_to\\\":79,\\\"batt_tmax\\\":350,\\\"zone_tmax\\\":45000,\\\"throttle_s\\\":%d,\\\"e_partial\\\":0}\n\", s, t, w0 + t, s, f, e, e / 3, e * 2 / 3, thr
            printf \"{\\\"v\\\":1,\\\"seq\\\":%d,\\\"n\\\":3,\\\"up\\\":%d,\\\"t\\\":%d,\\\"k\\\":\\\"hour_proc\\\",\\\"id\\\":\\\"hq-%d-%d\\\",\\\"rss_agent\\\":%d,\\\"rss_datad\\\":%d,\\\"rss_devui\\\":8000,\\\"rss_ts\\\":45000,\\\"cpu_datad\\\":%d,\\\"idle_rows\\\":40,\\\"idle_wan\\\":3%s,\\\"ts_on_s\\\":%d%s}\n\", s, t, w0 + t, s, f, rss, rss, cpu, xf, ton, tsx
        } }" >>"$SEG"
}
ev() { printf '{"v":1,"seq":%s,"n":9,"up":%s,"t":%s,"k":"%s"%s}\n' "$1" "$2" "$3" "$4" "$5" >>"$SEG"; } # ev <seq> <up> <t> <k> <,fields>
boot1() { ev 1 50 null boot ',"boot":"aaaaaaaa-0000-4000-8000-000000000001","code":1150,"mode":"mode_power_on","fw":null,"net_select":null,"rb_weekly":null,"rb_cutoff":null,"rb_connfail":null'
          ev 1 50 null guard_start ',"id":"gs-aaaaaaaa-50","requested":0,"by":null,"why":null'
          ev 1 60 null ver ',"prog":"zwrt-datad","md5":"d1d1d1d1","how":"exe","pid":700,"extra":"ubus=cli"'; }
# DOCTOR_REPORT_DUMP=<dir>: every --report here (and its --summary) also leaves a fixture
# there (doctor.sh DOC_REPORT_KEEP) for the Rust port's parity run (tools/scorecard-parity.sh)
REPN=0
rep() {
    if [ -n "${DOCTOR_REPORT_DUMP:-}" ]; then
        REPN=$((REPN + 1))
        DOC_REPORT_KEEP=$DOCTOR_REPORT_DUMP/$(printf %03d $REPN)-$1-sum sh "$SCRIPTS/doctor.sh" --report "$1" --summary >/dev/null 2>&1
        export DOC_REPORT_KEEP=$DOCTOR_REPORT_DUMP/$(printf %03d $REPN)-$1
    fi
    sh "$SCRIPTS/doctor.sh" --report "$1" >"$T/rep" 2>&1; RC=$?
    unset DOC_REPORT_KEEP
}
line() { grep "^  $1 " "$T/rep"; }

lsetup
boot1
hours 1 "$W0" 3600 168
rep 7d
check "a clean week: runs, says how far it goes and how much ledger it read" '[ $RC = 0 ] && grep -q "^设备成绩单（7 天，截至" $T/rep && grep -q "账本：1 次开机，168 小时的记录" $T/rep'
check "S1: no unexpected reboot over a whole week, covered: 达标" 'line S1 | grep -q "0 次 · 达标 · 7 天 / 7 天 · 覆盖完整"'
check "S2b, S3: no crash, covered: 达标" 'line S2b | grep -q "在家 没有崩溃；国外 没有崩溃 · 达标" && line S3 | grep -q "在家 0 次；国外 0 次 · 达标"'
check "S6 looks at one day" 'line S6 | grep -q "0 次 · 达标 · 1 天 / 1 天"'
check "S8: Tailscale healthy all week, checked throughout: 达标" 'line S8 | grep -q "Tailscale 100.0% · 达标 · 7 天 / 7 天 · 覆盖完整 · 自动"'
check "U1-U5 by known facts, U6 for a person" 'line U1 | grep -q "不达标 · 第二步前靠已知事实" && line U6 | grep -q "· 人工 ·"'
check "P1: average power from the hours; target not set yet" 'line P1 | grep -q "平均 900 mW（7 天 的样本） · 没测（目标待定）"'
check "P2: each cell over 6 h has a figure, the rest too few" 'line P2 | grep -q "亮屏在家 1800 mW；亮屏国外 样本不足；息屏在家 720 mW；息屏国外 样本不足"'
check "P4a: datad at 1.0% of a core: 达标; P4b: cli backend: 不达标" 'line P4a | grep -q "平均 1.0%（24 小时） · 达标" && line P4b | grep -q "cli 后端.* · 不达标"'
check "P5: a boot that ran 24 h, RSS flat: 达标" 'line P5 | grep -q "RSS 增长都在 10% 以内；OOM 0 次 · 达标"'
check "P6: no throttling, no thermal rise: 达标" 'line P6 | grep -q "降频 0 分；过热等级上升 0 次 · 达标"'
check "S4: a clean week, every program 0: 达标" 'line S4 | grep -q "agent 0；datad 0；触屏 0；u60-uid 0；guard 0 · 达标"'
# a power cut leaves a run of NULs: busybox awk ends a record at each; one segment, not N lines
printf '\000\000\000\000' >"$T/ledger/boot-000001-aaaaaaaa-900.jsonl"
rep 7d
check "NULs: said once a segment, not as lines it could not read" 'grep -q "；1 段有断电留下的空字节，已跳过$" $T/rep && ! grep -q "读不懂" $T/rep'
rm -f "$T/ledger/boot-000001-aaaaaaaa-900.jsonl"
# u60-uid restarted three times: twice by a deployment (ship=1), once not;
# lines from before 10-04 have no ship field and still count
ev 1 400000 $((W0 + 400000)) proc_restart ',"id":"pr-aaaaaaaa-900-1","prog":"u60-uid","old":800,"new":900,"crashlog":0,"ship":1'
ev 1 400100 $((W0 + 400100)) proc_restart ',"id":"pr-aaaaaaaa-901-2","prog":"u60-uid","old":900,"new":901,"crashlog":0,"ship":1'
ev 1 400200 $((W0 + 400200)) proc_restart ',"id":"pr-aaaaaaaa-902-3","prog":"u60-uid","old":901,"new":902,"crashlog":0,"ship":0'
ev 1 400300 $((W0 + 400300)) proc_restart ',"id":"pr-aaaaaaaa-903-4","prog":"u60-uid","old":902,"new":903,"crashlog":0'
rep 7d
check "S4: a deployment's restarts are left out, the others counted" 'line S4 | grep -q "u60-uid 2（不达标）"'
rep 24h
check "--report 24h: 7-day items show 1 day of 7: 注意" 'line S1 | grep -q "0 次 · 注意 · 1 天 / 7 天"'
check "summary: three lines for the default output" 'sh "$SCRIPTS/doctor.sh" --report 7d --summary >$T/sum && [ "$(wc -l <$T/sum)" = 3 ] && grep -q "意外重启 0 次（达标）" $T/sum && grep -q "耗电：平均 900 mW" $T/sum'
cp "$T/sum" "$T/ledger/summary"
# T9b: --report asks the core first (u60d /v2/scorecard), and works it out here when there is no
# card to be had: the core down, or a reply without its rc line
printf '#!/bin/sh\nfor a; do case "$a" in *scorecard*) echo "$a" >%s/sc-url ;; esac; done\nwhile [ $# -gt 0 ]; do [ "$1" = --post-file ] && cp "$2" %s/sc-body; shift; done\ncat %s/sc-resp 2>/dev/null\nexit $(cat %s/sc-exit 2>/dev/null || echo 0)\n' "$T" "$T" "$T" "$T" >"$T/bin/wget-sc"
chmod +x "$T/bin/wget-sc"
printf '核心的卡\n第二行\nrc=3\n' >"$T/sc-resp"
DOC_WGET=$T/bin/wget-sc sh "$SCRIPTS/doctor.sh" --report 7d >"$T/rep" 2>&1; RC=$?
check "--report: the core's card and its exit code, as the core gave them" '[ $RC = 3 ] && [ "$(cat $T/rep)" = "$(printf "核心的卡\n第二行")" ]'
check "--report: asks for the window, sends standby_check then the health rows" 'grep -q "/v2/scorecard?w=7d&summary=0$" $T/sc-url && head -n 1 $T/sc-body | grep -q "^\(ok\|warn\)	"'
DOC_WGET=$T/bin/wget-sc sh "$SCRIPTS/doctor.sh" --report 7d --summary >"$T/rep" 2>&1
check "--report --summary: asks with summary=1" 'grep -q "summary=1$" $T/sc-url'
printf '<html>oops</html>\n' >"$T/sc-resp"
DOC_REPORT_SRC=local DOC_WGET=$T/bin/wget-sc sh "$SCRIPTS/doctor.sh" --report 7d >"$T/rep-local" 2>&1; RCL=$?
DOC_WGET=$T/bin/wget-sc sh "$SCRIPTS/doctor.sh" --report 7d >"$T/rep" 2>&1; RC=$?
check "--report: a reply without rc= is no card: worked out here instead" '[ $RC = $RCL ] && cmp -s $T/rep $T/rep-local'
echo 1 >"$T/sc-exit"; : >"$T/sc-resp"
DOC_REPORT_SRC=local DOC_WGET=$T/bin/wget-sc sh "$SCRIPTS/doctor.sh" --report 7d >"$T/rep-local" 2>&1; RCL=$?
DOC_WGET=$T/bin/wget-sc sh "$SCRIPTS/doctor.sh" --report 7d >"$T/rep" 2>&1; RC=$?
check "--report: the core down: worked out here instead" '[ $RC = $RCL ] && cmp -s $T/rep $T/rep-local'
DOC_REPORT_SRC=core DOC_WGET=$T/bin/wget-sc sh "$SCRIPTS/doctor.sh" --report 7d >"$T/rep" 2>&1; RC=$?
check "--report with DOC_REPORT_SRC=core: the core down says so, exit 2" '[ $RC = 2 ] && grep -q "核心的成绩单读不到" $T/rep'
rm -f "$T/sc-exit"
printf '  账本（核心）\n  耗电：核心\n  详细成绩单：doctor.sh --report 7d\nrc=0\n' >"$T/sc-resp"
: >"$T/sc-url"
check "the default output ends with the core's three lines when it answers (u60-sentinel.md §20)" 'DOC_WGET=$T/bin/wget-sc sh "$SCRIPTS/doctor.sh" >$T/out2; [ "$(tail -n 3 $T/out2)" = "$(printf "  账本（核心）\n  耗电：核心\n  详细成绩单：doctor.sh --report 7d")" ] && grep -q "w=7d&summary=1$" $T/sc-url'
rm -f "$T/sc-exit" "$T/sc-resp"
check "the default output ends with them (the core down: the file)" 'sh "$SCRIPTS/doctor.sh" >$T/out2; tail -n 3 $T/out2 | cmp -s - $T/sum'
rm -rf "$T"

lsetup # two days only, and a boot that ended in a modem crash with guard up
boot1
hours 1 "$W0" 3600 40
ev 1 20000 $((W0 + 20000)) ssr ',"id":"ssr-aaaaaaaa-500","kseq":500,"line":"x","rat":"SA","band":"n78","nrband":"78","net":"460-00","home":"460-00","net_age":10,"wan_ppm":3'
SEG=$T/ledger/boot-000002-bbbbbbbb-001.jsonl
ev 2 50 null boot ',"boot":"bbbbbbbb-0000-4000-8000-000000000002","code":1182,"mode":"mode_power_on","fw":null,"net_select":null,"rb_weekly":null,"rb_cutoff":null,"rb_connfail":null'
hours 2 $((W0 + 40 * 3600 + 3600 + 120)) 60 8
rep 7d
check "1182 after a crash with no result: the modem, guard was up: S1 and S2a 不达标" 'line S1 | grep -q "1 次 · 不达标" && line S2a | grep -q "1 次（开机空档 0 次） · 不达标"'
check "the crash that rebooted counts as 600 s or more in S2b" 'line S2b | grep -q "在家 P95 600 秒（1 次）.* · 不达标"'
check "under a week of ledger, no violation elsewhere: 注意 with how long" 'line S5a | grep -q "0 次 · 注意 · 2 天"'
rm -rf "$T"

lsetup # recovery times at home and abroad; a watcher gap of 12 minutes
boot1
hours 1 "$W0" 3600 168 'if (i == 100) gw = 720; if (i >= 120) { ab = 3600; net = "440-10" }'
for k in 1 2 3; do
    ev 1 $((100000 + k * 1000)) null ssr ',"id":"ssr-aaaaaaaa-'$k'","kseq":'$k',"line":"x","rat":"SA","band":"n78","nrband":"78","net":"460-00","home":"460-00","net_age":10,"wan_ppm":3'
    ev 1 $((100030 + k * 1000)) null ssr_result ',"id":"res-ssr-aaaaaaaa-'$k'","ssr":"ssr-aaaaaaaa-'$k'","result":"ok","recovered_s":'$((10 + k))',"rx_seen_s":null,"down_seen":1,"resumed":0,"gap_s":0,"slept":0'
done
ev 1 500000 null ssr ',"id":"ssr-aaaaaaaa-9","kseq":9,"line":"rf_nr5g_sub6_tx.c:5428","rat":"SA","band":"n77","nrband":"77","net":"440-10","home":"460-00","net_age":10,"wan_ppm":90'
ev 1 500100 null ssr_result ',"id":"res-ssr-aaaaaaaa-9","ssr":"ssr-aaaaaaaa-9","result":"ok","recovered_s":80,"rx_seen_s":84,"down_seen":1,"resumed":0,"gap_s":0,"slept":0'
rep 7d
check "one watcher gap over 10 minutes: S2b and S3 not measured" 'line S2b | grep -q "· 没测 ·.*覆盖不完整（.*有一段超过 10 分钟）" && line S3 | grep -q "· 没测 ·"'
check "S2b still shows the figures: home P95 13 s of 3, abroad 80 s" 'line S2b | grep -q "在家 P95 13 秒（3 次）；国外 P95 80 秒（1 次）"'
check "S3 lists abroad crashes by network, RAT and band for a person" 'line S3 | grep -q "在家 3 次；国外 1 次" && grep -q "国外：440-10 SA n77/n77 1 次" $T/rep'
check "S1 is not about the watcher: still covered" 'line S1 | grep -q "覆盖完整"'
rm -rf "$T"


# S8, the Tailscale half (docs/LEDGER.md §8 and §11): healthy over checked, its own coverage
s8() { # s8 <awk: per-hour overrides>: a clean week, then the S8 row
    lsetup; boot1; hours 1 "$W0" 3600 168 "$1"; rep 7d; line S8 >"$T/s8"; S8=$(cat "$T/s8"); rm -rf "$T"
}
s8 'old = 1'
check "S8: hour lines from before the check: never checked, not measured (not 0%)" 'echo "$S8" | grep -q "Tailscale 没查到过 · 没测 · .*覆盖不完整（Tailscale 开着 7 天，查到 0 分，有一小时里超过 10 分钟没查到）"'
s8 'tok = 3500'
check "S8: Tailscale healthy 97.2% of the checked time: 不达标" 'echo "$S8" | grep -q "Tailscale 97.2% · 不达标 · .*覆盖完整"'
s8 'tck = 3240; tok = 3240'
check "S8: 10% of the on time unchecked: not measured, whatever the checked part says" 'echo "$S8" | grep -q "Tailscale 100.0% · 没测 · .*覆盖不完整（Tailscale 开着 7 天，查到 6 天 7 小时）"'
s8 'if (i == 50) { tck = 2940; tok = 2940 }'
check "S8: one hour with 11 minutes unchecked: not measured" 'echo "$S8" | grep -q "Tailscale 100.0% · 没测 · .*有一小时里超过 10 分钟没查到"'
s8 'if (i == 50) { tck = 3060; tok = 3060 }'
check "S8: one hour with 9 minutes unchecked: still covered" 'echo "$S8" | grep -q "Tailscale 100.0% · 达标 · .*覆盖完整"'
s8 'tck = 3000; tok = 2000'
check "S8: under 99% even if every unchecked second was healthy: 不达标 despite the coverage" 'echo "$S8" | grep -q "Tailscale 66.7% · 不达标 · .*覆盖不完整"'
s8 'ton = 0; tck = 0; tok = 0'
check "S8: Tailscale never meant on: nothing to judge" 'echo "$S8" | grep -q "Tailscale 没开过 · 没测"'

lsetup # datad degraded, datad hungry, memory growing, throttled
boot1
hours 1 "$W0" 3600 168 'if (i >= 140) cpu = 30; if (i >= 100) rss = 26000; if (i == 160) thr = 120'
ev 1 600000 null datad_degraded ',"id":"dds-aaaaaaaa-1790000000","state":"start","since":1790000000,"reason":"x","dur_s":null,"how":null'
ev 1 600400 null datad_degraded ',"id":"dde-aaaaaaaa-1790000000","state":"end","since":1790000000,"reason":null,"dur_s":400,"how":"gone"'
ev 1 500000 null thermal ',"id":"th-aaaaaaaa-1","level":"0x3","line":"x"'
ev 1 550000 null thermal ',"id":"th-aaaaaaaa-2","level":"0x5","line":"x"'
rep 7d
check "S6: one episode today, 6 min 40 s: 不达标" 'line S6 | grep -q "1 次，共 6 分 · 不达标"'
check "P4a: datad over 2% of a core today: 不达标" 'line P4a | grep -q "平均 3.0%.* · 不达标"'
check "P5: RSS up 30% over a 24 h boot: 不达标, named" 'line P5 | grep -q "agent +30%；datad +30%；.* · 不达标"'
check "P6: throttled 2 min and one thermal rise above the boot's first level: 不达标" 'line P6 | grep -q "降频 2 分；过热等级上升 1 次 · 不达标"'
rm -rf "$T"

lsetup # a hole between boots that only ends inside the window: only its part inside counts
boot1
hours 1 "$W0" 3600 30
B2=$((W0 + 31 * 3600 + 3900)) # boot 2's first hour starts 65 min after boot 1's last one ended
SEG=$T/ledger/boot-000002-bbbbbbbb-001.jsonl
ev 2 50 null boot ',"boot":"bbbbbbbb-0000-4000-8000-000000000002","code":1134,"mode":"mode_power_on","fw":null,"net_select":null,"rb_weekly":null,"rb_cutoff":null,"rb_connfail":null'
hours 2 $((B2 - 60)) 60 23
printf '{"v":1,"seq":2,"n":9,"up":86160,"t":%s,"k":"hour","id":"h-2-82860","from":82860,"to":86160,"ft":%s,"tt":%s,"awake":3300,"asleep":0,"rounds":55,"skipped":0,"max_gap":60,"net":"460-00","home":"460-00","abroad":0,"free_mb":900,"dump_mb":1,"budget":0,"g_guard":0,"g_job":0,"g_watcher":0,"g_capture":0,"g_uidlog":0,"g_crashlog":0,"g_datad":0}\n' \
    $((B2 - 60 + 86160)) $((B2 - 60 + 82860)) $((B2 - 60 + 86160)) >>"$SEG" # the last hour ends 5 min short
rep 24h
check "a 65-minute hole that reaches 5 minutes into the window: 5 minutes counted, no 10-minute gap" 'line S2b | grep -q "· 覆盖完整 ·"'
rm -rf "$T"

lsetup # a merged chain that slept in its first part: kept apart like any slept sample
boot1
hours 1 "$W0" 3600 168
ev 1 100000 null ssr ',"id":"ssr-aaaaaaaa-1","kseq":1,"line":"x","rat":"SA","band":"n78","nrband":"78","net":"460-00","home":"460-00","net_age":10,"wan_ppm":3'
ev 1 100070 null ssr_result ',"id":"res-ssr-aaaaaaaa-1","ssr":"ssr-aaaaaaaa-1","result":"merged","recovered_s":null,"rx_seen_s":null,"down_seen":1,"resumed":0,"gap_s":40,"slept":1'
ev 1 100070 null ssr ',"id":"ssr-aaaaaaaa-2","kseq":2,"line":"x","rat":"SA","band":"n78","nrband":"78","net":"460-00","home":"460-00","net_age":10,"wan_ppm":3'
ev 1 100100 null ssr_result ',"id":"res-ssr-aaaaaaaa-2","ssr":"ssr-aaaaaaaa-2","result":"ok","recovered_s":20,"rx_seen_s":null,"down_seen":1,"resumed":0,"gap_s":0,"slept":0'
rep 7d
check "S2b: the chain is a slept sample, not a 90 s recovery; nothing to judge 达标 on" 'line S2b | grep -q "在家 没有崩溃；国外 没有崩溃；跟随中睡过 1 次另列 · 注意"'
rm -rf "$T"

lsetup # a crash in this boot with no result yet is still being followed, not a 600 s sample
boot1
hours 1 "$W0" 3600 168
ev 1 600000 $((W0 + 600000)) ssr ',"id":"ssr-aaaaaaaa-7","kseq":7,"line":"x","rat":"SA","band":"n78","nrband":"78","net":"460-00","home":"460-00","net_age":10,"wan_ppm":3'
rep 7d
check "S2b: a crash still being followed is no sample yet" 'line S2b | grep -q "在家 没有崩溃；国外 没有崩溃 · 达标"'
check "S3 counts it all the same" 'line S3 | grep -q "在家 1 次；国外 0 次 · 不达标"'
rm -rf "$T"

lsetup # a boot whose boot line has aged out (only a later segment left): no reboot of unknown cause
boot1
hours 1 "$W0" 3600 30
SEG=$T/ledger/boot-000002-bbbbbbbb-002.jsonl
hours 2 $((W0 + 31 * 3600 + 3600)) 60 100
rep 7d
check "S1: a boot with no boot line after a hole is not counted as a reboot" 'line S1 | grep -q "：0 次 · "'
check "the hole before it still counts against coverage" 'line S2a | grep -q "覆盖不完整"'
rm -rf "$T"

lsetup # alerts in the window, a trimmed queue
boot1
hours 1 "$W0" 3600 168
printf '150\t%s\t100\tagent-crash\tx\n151\t%s\t200\tsms-test\tx\n' $((W0 + 400000)) $((W0 + 400100)) >"$T/alerts/queue"
rep 7d
check "U6: alerts in the window listed (sms-test left out), queue trimmed after the window began: 没测" 'line U6 | grep -q "窗口内告警 1 条.* · 没测 ·.*队列剪过" && grep -q "告警：.* agent-crash；" $T/rep'
rm -rf "$T"

lsetup # bad lines and newer formats are skipped and counted
boot1
hours 1 "$W0" 3600 3
echo 'not json' >>"$SEG"
printf '{"v":2,"seq":1,"n":1,"up":1,"t":null,"k":"future"}\n' >>"$SEG"
printf '{"v":1,"seq":1,"n":9,"up":5,"t":null,"k":"hour","id":"h-1-3600","from":3600}\n' >>"$SEG"
rep 7d
check "unreadable lines and a newer version are skipped and counted; a repeated id is taken once" '[ $RC = 0 ] && grep -q "账本：1 次开机，3 小时的记录" $T/rep && grep -q "1 行读不懂，已跳过；1 行是更新的格式" $T/rep'
rm -rf "$T"

lsetup # 16 MB of ledger (the retention cap): how long the report takes (F24; recorded, no limit)
boot1
hours 1 "$W0" 3600 720
awk -v w0="$W0" 'BEGIN { for (i = 0; i < 36000; i++) printf "{\"v\":1,\"seq\":1,\"n\":%d,\"up\":%d,\"t\":%d,\"k\":\"link\",\"id\":\"link-aaaaaaaa-%d\",\"state\":\"up\",\"cause\":\"other\",\"res\":2,\"pad\":\"%s\"}\n", i, 4000 + i * 60, w0 + 4000 + i * 60, i, sprintf("%300s", "") }' >>"$SEG"
sz=$(wc -c <"$SEG")
t0=$(date +%s)
rep 7d
t1=$(date +%s)
rep 24h
t2=$(date +%s)
echo "  (16 MB ledger: $((sz / 1048576)) MB; --report 7d $((t1 - t0)) s, --report 24h $((t2 - t1)) s in this container)"
check "a 16 MB ledger is read to the end" '[ $RC = 0 ] && grep -q "720 小时的记录" $T/rep'
rm -rf "$T"

lsetup # hours on record, but the clock never trusted: nothing can be placed in a window
boot1
printf '{"v":1,"seq":1,"n":5,"up":7200,"t":null,"k":"hour","id":"h-1-3600","from":3600,"to":7200,"ft":null,"tt":null,"awake":3600,"asleep":0,"rounds":60,"skipped":0,"max_gap":60,"net":null,"home":null,"abroad":0,"free_mb":null,"dump_mb":null,"budget":0,"g_guard":0,"g_job":0,"g_watcher":0,"g_capture":0,"g_uidlog":0,"g_crashlog":0,"g_datad":0}\n' >>"$SEG"
rep 7d
check "hours on record but the clock never set: says that, not 'no records'" '[ $RC = 1 ] && grep -q "账本有 1 小时的记录，但设备时钟一直没对上" $T/rep'
rm -rf "$T"

lsetup
rm -f "$SEG"
rep 7d
check "no ledger yet: says so" '[ $RC = 1 ] && grep -q "账本还没有记录" $T/rep'
rm -rf "$T"

echo "--ledger-selftest (docs/LEDGER.md §13)"
ssetup() {
    setup
    export DOC_LEDGER_DIR=$T/ledger DOC_CRASHCAP_DIR=$T/crashcap DOC_KEYLOG=$T/key.log DOC_MSS_RECOVERY=$T/recovery \
        DOC_BOOT_ID_FILE=$T/bootid DOC_JSONFILTER=$T/bin/jsonfilter DOC_LEDGER_TMP=$T/lt DOC_SELFTEST_WAIT=2
    mkdir -p "$T/ledger" "$T/crashcap" "$T/lt"
    echo aaaaaaaa-0000-4000-8000-000000000001 >"$T/bootid"
    export DOC_KMSG=$T/crashcap/kmsg-aaaaaaaa.log # the capture, as if the kernel log were copied at once
    printf '6,1,1000,-;boot\n' >"$DOC_KMSG"
    printf '2026-09-28 10:00:00 : reboot_reason_code=1150!!! \n' >"$T/key.log"
    echo enabled >"$T/recovery"
    printf '#!/bin/sh\necho "export X='"'"'1'"'"'; export Z='"'"'1'"'"'; "\n' >"$T/bin/jsonfilter"
    printf '#!/bin/sh\ncase "$*" in *9460/v2/state*) echo "{\\"epoch\\":\\"e\\",\\"seq\\":1,\\"blocks\\":{}}" ;; *public/status*) cat %s/public 2>/dev/null ;; esac\nexit 0\n' "$T" >"$T/bin/wget"
    printf '#!/bin/sh\necho "9: rmnet_data0    inet 10.1.2.3/30 scope global rmnet_data0"\n' >"$T/bin/ip"
    printf 'Iface\tDestination\tGateway\tFlags\nrmnet_data0\t00000000\t00000000\t0001\n' >"$T/route"
    chmod +x "$T/bin/jsonfilter" "$T/bin/wget" "$T/bin/ip"
    export DOC_IP=$T/bin/ip DOC_ROUTE=$T/route
}
self() { sh "$SCRIPTS/doctor.sh" --ledger-selftest >"$T/st" 2>&1; RC=$?; }
ssetup
self
check "everything in place: every item PASS, exit 0" '[ $RC = 0 ] && ! grep -q "^FAIL" $T/st && grep -q "^PASS kmsg 落盘" $T/st && grep -q "全部通过" $T/st'
check "the link as the watcher sees it: an address and the default route" 'grep -q "^PASS ip -4 -o addr 看得到 rmnet_data0 的地址" $T/st && grep -q "^PASS 默认路由走 rmnet_data0" $T/st'
check "it names the device tools u60-guard leans on" 'grep -q "^PASS dd iflag=skip_bytes" $T/st && grep -q "^PASS awk 的 mktime" $T/st && grep -q "^PASS flock -n" $T/st && grep -q "^PASS jsonfilter 多个 -e" $T/st && grep -q "^PASS wget -T 2" $T/st'
check "the ledger is left as it was: no test file, nothing else written" '[ -z "$(ls -A $T/ledger)" ]'
mv "$T/key.log" "$T/key.log.0"
: >"$T/key.log"
self
check "this boot's reason code only in key.log.0 (rotated): PASS" '[ $RC = 0 ] && grep -q "^PASS key.log 的原因码（这次开机 1150）" $T/st'
printf '2026-09-29 10:00:00 : reboot_reason_code=1185!!! \n' >"$T/key.log"
self
check "both files: the newest code (key.log) wins" 'grep -q "^PASS key.log 的原因码（这次开机 1185）" $T/st'
rm -f "$T/key.log.0"
printf '2026-09-28 10:00:00 : reboot_reason_code=1150!!! \n' >"$T/key.log"
echo "10 1 0" >"$T/lt/w.pos" # a watcher that stays behind the test line
echo "123 456 789" >"$T/lt/watcher.pid"
self
check "watcher running but not past the test line: FAIL" '[ $RC != 0 ] && grep -q "^FAIL 观察循环读过了测试行" $T/st'
rm -f "$T/lt/watcher.pid"
self
check "watcher stopped or handed to the core (no pid file), its old position left: not checked (§20)" '[ $RC = 0 ] && ! grep -q "观察循环读过了测试行" $T/st'
rm -f "$T/lt/w.pos"
: >"$T/key.log"
rm -f "$T/crashcap/kmsg-aaaaaaaa.log"
export DOC_KMSG=$T/no/such/kmsg
self
printf 'Iface\tDestination\tGateway\tFlags\n' >"$T/route" # no default route
self
check "no reason code, no capture file, no default route: each a FAIL, the rest still tried" 'grep -q "^FAIL key.log 的原因码" $T/st && grep -q "^FAIL kmsg 落盘：没有这次开机的落盘文件" $T/st && grep -q "^FAIL 默认路由走 rmnet_data0" $T/st && grep -q "^PASS 账本目录可写" $T/st && grep -q "3 项没通过" $T/st'
unset DOC_LEDGER_DIR DOC_CRASHCAP_DIR DOC_KEYLOG DOC_MSS_RECOVERY DOC_BOOT_ID_FILE DOC_JSONFILTER DOC_LEDGER_TMP DOC_SELFTEST_WAIT DOC_KMSG DOC_IP DOC_ROUTE
rm -rf "$T"

echo "the device manifest (docs/SHIP.md): first line, last --tsv row, --manifest"
msetup() {
    setup
    export DOC_MANIFEST=$T/m.jsonl DOC_SHIP_TXN=$T/txn DOC_SHIP_HB=$T/hb.ship DOC_MD5_CACHE=$T/md5c/cache \
        DOC_TS_DIR=$T/ts DOC_BOOT_ID_FILE=$T/bootid DOC_MD5SUM=$T/bin/md5log
    echo bbbb-0000 >"$T/bootid"
    printf '#!/bin/sh\necho "$1" >>%s/md5.calls\nexec md5sum "$@"\n' "$T" >"$T/bin/md5log"
    chmod +x "$T/bin/md5log"
    printf 'datad build A\n' >"$T/datad"
    DM=$(md5sum "$T/datad" | cut -d' ' -f1)
    RM=$(md5sum "$T/rc" | cut -d' ' -f1)
}
mship() { # mship <comp> <path> <md5> [format]
    printf '{"v":1,"kind":"ship","comp":"%s","txn":"20260930-120000-%s","commit":"abc1234","format":1,"mac_time":1790000000,"boot_id":"x","uptime":5,"files":[{"path":"%s","md5":"%s","prev":"%s.prev-x"}],"state":[]}\n' "$1" "$1" "$2" "$3" "$2" >>"$T/m.jsonl"
}
mrec() { # mrec <name> <path> <md5>
    printf '{"v":1,"kind":"record","name":"%s","path":"%s","md5":"%s","mac_time":1790000000,"boot_id":"x","uptime":5,"why":"t"}\n' "$1" "$2" "$3" >>"$T/m.jsonl"
}
mline() { awk -F'\t' '$2 == "manifest" {print $1 "|" $4}' "$T/out"; }
txnf() { printf 'v=1\ntxn=20260930-120000-datad\ncomp=datad\nphase=%s\nreason=%s\nboot_id=%s\nt_phase=%s\nend=1\n' "$1" "$2" "${3:-bbbb-0000}" "${4:-900}" >"$T/txn"; }

msetup; mship datad "$T/datad" "$DM"; mrec rc.local "$T/rc" "$RM"; run
check "manifest matches: ok, counted" '[ "$(mline)" = "ok|一致（2 项）" ]'
check "the manifest row is the last --tsv row" '[ "$(tail -n 1 $T/out | cut -f2)" = manifest ]'
sh "$SCRIPTS/doctor.sh" >"$T/human"
check "people see it first" '[ "$(head -n 1 $T/human)" = "  ● 清单：一致（2 项）" ]'
printf 'datad build B\n' >"$T/datad"; run
check "a changed file: warn, names it with both md5s" "[ \"\$(mline)\" = \"warn|不一致的是 datad（清单 $(echo $DM | cut -c1-8)，实际 \$(md5sum $T/datad | cut -c1-8)）\" ]"
echo "# edited" >>"$T/rc"; run
check "two changed: says how many" 'mline | grep -q "^warn|不一致的是 .* 等 2 项$"'
sh "$SCRIPTS/doctor.sh" >"$T/human"
check "people: the warn line first, and counted" 'head -n 1 $T/human | grep -q "^  ▲ 清单：不一致的是" && grep -q "需要注意" $T/human'
rm -rf "$T"

msetup; printf 'datad build B\n' >"$T/datad"; mship datad "$T/datad" "$DM"; mship datad "$T/datad" "$(md5sum $T/datad | cut -d' ' -f1)"; run
check "the last line of a component wins" '[ "$(mline)" = "ok|一致（1 项）" ]'
mrec tailscaled "$T/ts/tailscaled" -; run
check "recorded as absent and still absent: consistent" '[ "$(mline)" = "ok|一致（2 项）" ]'
mkdir -p "$T/ts"; echo bin >"$T/ts/tailscaled"; run
check "recorded as absent, now there: differs" 'mline | grep -q "^warn|不一致的是 tailscaled（清单 -，实际 "'
rm -rf "$T"

msetup; printf 'doctor v2\n' >"$T/datad"; mship guard "$T/datad" "$DM"; mship scripts "$T/datad" "$(md5sum $T/datad | cut -d' ' -f1)"; run
check "a path two components list: the newer line wins (scripts after the retired guard)" '[ "$(mline)" = "ok|一致（1 项）" ]'
mship guard "$T/datad" "$DM"; run
check "…and a later line of the other component takes it back" 'mline | grep -q "^warn|不一致的是 datad"'
rm -rf "$T"

msetup; echo 'garbage {' >"$T/m.jsonl"; run
check "unreadable manifest: warn" 'mline | grep -q "^warn|清单读不懂"'
rm -rf "$T"

msetup; mship datad "$T/datad" "$DM"
txnf manifest_pending x; run
check "manifest owed: warn" 'mline | grep -q "^warn|清单待补：datad 已转正"'
txnf failed '退回不完整'; run
check "failed transaction: warn, needs a person" '[ "$(mline)" = "warn|上次上机停在半路，要人处理：datad（退回不完整）" ]'
txnf check ''; run
check "half-way, no executor: warn" 'mline | grep -q "^warn|上次上机停在半路：datad（检查中）"'
mkdir -p "$T/proc/700"; printf 'sh\000/data/u60-guard/u60-ship.sh\000run\000' >"$T/proc/700/cmdline"; echo "995 700 20260930-120000-datad check" >"$T/hb.ship"; run
check "half-way with a live executor: ok, shipping now" '[ "$(mline)" = "ok|正在上机：datad（检查中）" ]'
txnf done '' bbbb-0000 900; run
check "done 100 s ago, same boot: observing, 9 min left" '[ "$(mline)" = "ok|观察中（还剩 9 分钟）：datad 刚上机；一致（1 项）" ]'
txnf done '' cccc-0000 900; run
check "done in another boot: just consistent" '[ "$(mline)" = "ok|一致（1 项）" ]'
txnf rolledback '开机时已退回（断电时在 check）'; run
check "last ship rolled back: ok, said" '[ "$(mline)" = "ok|一致（1 项）；上次上机已退回：datad（开机时已退回（断电时在 check））" ]'
rm -rf "$T"

msetup; mship datad "$T/datad" "$DM"; mrec rc.local "$T/rc" "$RM"
sh "$SCRIPTS/doctor.sh" --manifest >"$T/man"; RCM=$?
check "--manifest: a row per entry" "[ \$RCM = 0 ] && grep -qx \"same	comp:datad	$T/datad	$DM	$DM\" $T/man && grep -qx \"same	record:rc.local	$T/rc	$RM	$RM\" $T/man"
check "--manifest: files never recorded are listed as such" "grep -qx \"unrecorded	record:init.d/zwrt-datad	$T/initd/zwrt-datad	-	\$(md5sum $T/initd/zwrt-datad | cut -d' ' -f1)\" $T/man && grep -qx 'unrecorded	record:tuning.env	$T/tuning.env	-	-' $T/man"
check "--manifest: the verdict last" '[ "$(tail -n 1 $T/man)" = "state	ok	一致（2 项）" ]'
rm -rf "$T"

# directories in the manifest (docs/SHIP.md 第二期): fingerprints, fresh for
# --manifest, cached for the minute-by-minute --tsv
msetup
export DOC_DEVUI_DIR=$T/devui
export DOC_UI_LOGOS=$T/devui/operator-logos
mkdir -p "$T/admin/_next/static" "$T/devui/fonts"
printf 'page\n' >"$T/admin/index.html"
printf 'js\n' >"$T/admin/_next/static/a b.js"
printf 'font\n' >"$T/devui/fonts/x.ttf"
AF=$(sh "$SCRIPTS/doctor.sh" --tree-fp "$T/admin")
FF=$(sh "$SCRIPTS/doctor.sh" --tree-fp "$T/devui/fonts")
printf '{"v":1,"kind":"ship","comp":"web","txn":"20260930-120000-web","commit":"abc1234","format":1,"mac_time":1790000000,"boot_id":"x","uptime":5,"files":[{"path":"%s","tree":"%s","prev":"%s.prev-20260930-120000-web"}],"state":[]}\n' "$T/admin" "$AF" "$T/admin" >>"$T/m.jsonl"
printf '{"v":1,"kind":"record","name":"devui-fonts","path":"%s","tree":"%s","mac_time":1790000000,"boot_id":"x","uptime":5,"why":""}\n' "$T/devui/fonts" "$FF" >>"$T/m.jsonl"
sh "$SCRIPTS/doctor.sh" --manifest >"$T/man"
check "directories: --manifest rows by fingerprint" "grep -qx \"same	comp:web	$T/admin	$AF	$AF\" $T/man && grep -qx \"same	record:ui-fonts	$T/devui/fonts	$FF	$FF\" $T/man"
check "directories: the logos never recorded, listed as such" "grep -qx 'unrecorded	record:ui-logos	$T/devui/operator-logos	-	-' $T/man"
check "directories: consistent" '[ "$(tail -n 1 $T/man)" = "state	ok	一致（2 项）" ]'
printf '{"v":1,"kind":"record","name":"ui-fonts","path":"%s","tree":"%s","mac_time":1790000001,"boot_id":"x","uptime":6,"why":"D4c"}\n' "$T/devui/fonts" "$FF" >>"$T/m.jsonl"
sh "$SCRIPTS/doctor.sh" --manifest >"$T/man"
check "directories: the old and the D4c name are one record" '[ "$(grep -c "record:ui-fonts" $T/man)" = 1 ] && ! grep -q devui-fonts $T/man && [ "$(tail -n 1 $T/man)" = "state	ok	一致（2 项）" ]'
echo core-new >"$T/corebin"
CN=$(md5sum <"$T/corebin" | cut -c1-32)
printf '{"v":1,"kind":"ship","comp":"datad","txn":"20261015-120000-datad","commit":"a","format":1,"mac_time":1790000000,"boot_id":"x","uptime":5,"files":[{"path":"%s","md5":"%s"}],"state":[]}\n' "$T/corebin" 0123456789abcdef0123456789abcdef >>"$T/m.jsonl"
printf '{"v":1,"kind":"ship","comp":"core","txn":"20261016-120000-core","commit":"b","format":1,"mac_time":1790000000,"boot_id":"x","uptime":5,"files":[{"path":"%s","md5":"%s"}],"state":[]}\n' "$T/corebin" "$CN" >>"$T/m.jsonl"
sh "$SCRIPTS/doctor.sh" --manifest >"$T/man"
check "D4c: a datad ship then a core ship of one file: one row, the core one, consistent" "grep -qx \"same	comp:core	$T/corebin	$CN	$CN\" $T/man && ! grep -q 'comp:datad' $T/man && [ \"\$(tail -n 1 $T/man)\" = 'state	ok	一致（3 项）' ]"
run
check "directories: --tsv agrees, fingerprints cached" '[ "$(mline)" = "ok|一致（3 项）" ] && grep -q "^tree:$T/admin " $T/md5c/cache'
sed "s|^\(tree:$T/admin [0-9a-f]*\) [0-9a-f]*\$|\1 ffffffffffffffffffffffffffffffff|" $T/md5c/cache >$T/c2 && mv $T/c2 $T/md5c/cache
run
check "directories: --tsv takes the cached value while nothing changed" 'mline | grep -q "^warn|不一致的是 admin（清单 ${AF%"${AF#????????}"}，实际 ffffffff）"'
sh "$SCRIPTS/doctor.sh" --manifest >"$T/man"
check "directories: --manifest computes afresh" '[ "$(tail -n 1 $T/man)" = "state	ok	一致（3 项）" ]'
printf 'new js\n' >"$T/admin/_next/static/c.js"
run
check "directories: a new file → the cache key changes, differs" 'mline | grep -q "^warn|不一致的是 admin"'
rm "$T/admin/_next/static/c.js"
ln -s index.html "$T/admin/link"
sh "$SCRIPTS/doctor.sh" --manifest >"$T/man"
check "directories: a symlink inside → unreadable, warn" "grep -qx \"differs	comp:web	$T/admin	$AF	unreadable\" $T/man && tail -n 1 $T/man | grep -q '^state	warn	'"
rm "$T/admin/link"
rm -rf "$T/devui/fonts"
sh "$SCRIPTS/doctor.sh" --manifest >"$T/man"
check "directories: a recorded directory gone → -, warn" "grep -qx \"differs	record:ui-fonts	$T/devui/fonts	$FF	-\" $T/man"
unset DOC_DEVUI_DIR DOC_UI_LOGOS
rm -rf "$T"

# md5 cache: a 28 MB binary is hashed once, then --tsv stays under 2 s
msetup
dd if=/dev/zero of="$T/big" bs=1048576 count=28 2>/dev/null
mship lweb "$T/big" "$(md5sum $T/big | cut -d' ' -f1)"; run
check "28 MB: hashed on the first run" '[ "$(grep -c "$T/big" $T/md5.calls)" = 1 ] && [ "$(mline)" = "ok|一致（1 项）" ]'
c0=$(tr -d . </proc/uptime | cut -d" " -f1); run1; c1=$(tr -d . </proc/uptime | cut -d" " -f1)
check "28 MB: not hashed again, --tsv in ≤ 2 s ($(( (c1 - c0) * 10 )) ms)" '[ "$(grep -c "$T/big" $T/md5.calls)" = 1 ] && [ $((c1 - c0)) -le 200 ]'
touch -d '2026-01-01 00:00' "$T/big"; run
check "28 MB: a new mtime is hashed again" '[ "$(grep -c "$T/big" $T/md5.calls)" = 2 ]'
rm -rf "$T"

echo "the rarer rows in English (--tsv2)"
en() { awk -F'\t' -v id="$1" '$2 == id { print $1 "|" $5 "|" $6 }' "$T/out2"; }
setup
touch "$T/marker"; echo vendor >"$T/uid.want"; rm -f "$T/hb"
printf '#!/bin/sh\nexit 1\n' >"$T/bin/wget"
printf '#!/bin/sh\necho "/dev/ubi0 4000000 3990000 10000 99%% /data"\n' >"$T/bin/df"
printf 'v=1\ncomp=datad\nphase=failed\nreason=executor gone\nend=1\n' >"$T/txn"; export DOC_SHIP_TXN=$T/txn
run
check "takeover, stock UI asked for, no heartbeat, nothing reachable, disk full" '[ "$(en takeover)" = "warn|Wi-Fi watchdog|In control: scenarios paused; handed back after 10 min of stable admin backend" ] &&
    [ "$(en screen)" = "ok|Screen UI|Stock UI on, as asked" ] && [ "$(en heartbeat)" = "bad|Admin heartbeat|No heartbeat file (admin backend not running?)" ] &&
    [ "$(en agent-http)" = "bad|Web admin :9090|Not reachable" ] && [ "$(en datad-http)" = "bad|Data service :9460|Can'"'"'t read /v2/state (the screen will have no data)" ] &&
    [ "$(en disk)" = "bad|/data space|Only 9 MB left" ]'
check "an ASCII ship reason is kept in English" '[ "$(en manifest)" = "warn|Manifest|Last deploy stopped midway, needs attention: datad (executor gone)" ]'
rm -f "$T/uid.want"; echo 2 >"$T/uid/attempts"
printf '#!/bin/sh\necho "/dev/ubi0 4000000 3950000 50000 99%% /data"\n' >"$T/bin/df"
printf 'v=1\ncomp=datad\nphase=failed\nreason=执行器没起来\nend=1\n' >"$T/txn"
run
check "just started, disk low, a Chinese ship reason left out of English" '[ "$(en screen)" = "ok|Screen UI|Running (just started; confirmed after 10 min stable)" ] &&
    [ "$(en disk)" = "warn|/data space|Only 48 MB left" ] && [ "$(en manifest)" = "warn|Manifest|Last deploy stopped midway, needs attention: datad" ]'
: >"$T/bin/df"
run
check "disk unreadable" '[ "$(en disk)" = "warn|/data space|Unreadable" ]'
unset DOC_SHIP_TXN; rm -rf "$T"

echo "stock phone-home (read only)"
setup
mkdir -p "$T/proc/200" "$T/proc/201"; echo zte_mqtt_sdk_st >"$T/proc/200/comm"; echo zte_smart_manag >"$T/proc/201/comm"
run
check "phone-home: MQTT on and running, TR-069 on without a server, 2 switches on, smart_manage running; ok, never a warning" '[ "$(level phonehome)" = ok ] &&
    [ "$(detail phonehome)" = "MQTT 上报开着（在跑）；TR-069 开着（没在跑），没配服务器；打点开关 2 个开着；应用库更新（smart_manage）在跑" ] &&
    [ "$(awk -F"\t" "\$2 == \"phonehome\" {print \$6}" "$T/out2")" = "MQTT reporting on (running); TR-069 on (not running), no server set; 2 analytics switches on; app catalog updates (smart_manage) running" ]'
printf "zwrt_mqtt.config.mqttOnreportEnable='0'\n" >"$T/mqtt"
printf "zwrt_tr069.ManagementServer.EnableCWMP='1'\nzwrt_tr069.ManagementServer.URL='http://acs.example'\n" >"$T/tr069"
rm -f "$T/dadian"; rm -rf "$T/proc/200" "$T/proc/201"
run
check "phone-home: MQTT off, TR-069 with a server (the address not shown), nothing else" '[ "$(level phonehome)" = ok ] &&
    [ "$(detail phonehome)" = "MQTT 上报关着；TR-069 开着（没在跑），配了服务器；打点开关 0 个开着；应用库更新（smart_manage）没在跑" ] && ! grep -q acs.example "$T/out"'
rm -f "$T/mqtt" "$T/tr069"
run
check "phone-home: unreadable says so" 'detail phonehome | grep -q "^MQTT 读不到；TR-069 读不到；"'
rm -rf "$T"

echo "snapshots"
GOLD=$SCRIPTS/test/doctor/golden
snap() { # snap <name>
    sh "$SCRIPTS/doctor.sh" --tsv >"$T/$1.tsv" 2>&1
    sh "$SCRIPTS/doctor.sh" >"$T/$1.txt" 2>&1
    sh "$SCRIPTS/doctor.sh" --tsv2 >"$T/$1.tsv2" 2>&1
    TSV2_BAD=$(tsv2_lint "$T/$1.tsv" "$T/$1.tsv2" | head -n 5)
    check "snapshot $1: --tsv2 agrees with --tsv, English clean" '[ -z "$TSV2_BAD" ]'
    [ -z "$TSV2_BAD" ] || echo "       $TSV2_BAD"
    # A real --tsv2 sample with warn and bad rows: zte-agent tests its parser on it.
    if [ "$1" = problems ]; then
        if [ -n "$DOCTOR_GOLDEN_UPDATE" ]; then
            cp "$T/$1.tsv2" "$SCRIPTS/test/doctor/tsv2-sample.tsv" && ok "tsv2-sample.tsv written"
        elif cmp -s "$T/$1.tsv2" "$SCRIPTS/test/doctor/tsv2-sample.tsv"; then
            ok "snapshot $1.tsv2 = tsv2-sample.tsv"
        else
            bad "snapshot $1.tsv2 differs from tsv2-sample.tsv"
            diff "$SCRIPTS/test/doctor/tsv2-sample.tsv" "$T/$1.tsv2" 2>&1 | head -20
        fi
    fi
    for ext in tsv txt; do
        if [ -n "$DOCTOR_GOLDEN_UPDATE" ]; then
            mkdir -p "$GOLD" && cp "$T/$1.$ext" "$GOLD/$1.$ext" && ok "golden $1.$ext written"
        elif cmp -s "$T/$1.$ext" "$GOLD/$1.$ext"; then
            ok "snapshot $1.$ext"
        else
            bad "snapshot $1.$ext differs from golden/$1.$ext"
            diff "$GOLD/$1.$ext" "$T/$1.$ext" 2>&1 | head -20
        fi
    done
}
setup; snap healthy; rm -rf "$T"
setup
echo "zte_topsw_devui" >"$T/sync"; echo 1 >"$T/mode"; echo 0 >"$T/aps"
touch "$T/down-u60-guard"; echo 500 >"$T/hb"; touch "$T/uid/gave-up"; : >"$T/alerts/sms-to"
grep -v "u60-uid start" "$T/rc" >"$T/rc2"; mv "$T/rc2" "$T/rc"
printf '9\t1\tcrash\tx\ty\n' >"$T/alerts/queue"
snap problems; rm -rf "$T"
setup; export DOC_STANDBY_STAT=$T/sb DOC_STANDBY_BASE=$T/sb.base DOC_LEDGER_DIR=$T/ledger DOC_BOOT_ID_FILE=$T/bootid
echo aaaaaaaa-0000-4000-8000-000000000001 >"$T/bootid"
i=0; while [ $i -lt 40 ]; do echo "$((100 + i * 20)) $((50 + i)) 20 30.0$MCOL 2.0 1.0 0.5" >>"$T/sb"; i=$((i + 1)); done
sh "$SCRIPTS/doctor.sh" --calibrate-standby >/dev/null
check "calibration, the core down: a calib event in the /data spool, as a producer writes it (§5)" 'f=$T/ledger/spool/calib-aaaaaaaa-1000.ev; [ -f "$f" ] && [ "$(head -n 1 "$f")" = "calib-aaaaaaaa-1000	aaaaaaaa-0000-4000-8000-000000000001	1000	calib" ] && [ "$(sed -n 2p "$f")" = ",\"rows\":40" ] && [ -z "$(ls $T/ledger/spool | grep tmp)" ]'
# the core answers (u60-sentinel.md §20): it keeps the event, nothing in the spool
rm -rf "$T/ledger/spool"
printf '#!/bin/sh\nfor a; do case "$a" in *calib*) echo "$a" >%s/calib-url ;; esac; done\necho "{\\"ok\\":true}"\n' "$T" >"$T/bin/wget-cal"
chmod +x "$T/bin/wget-cal"
DOC_WGET=$T/bin/wget-cal sh "$SCRIPTS/doctor.sh" --calibrate-standby >/dev/null
check "calibration, the core answers: the event to the core, nothing in the spool" '[ "$(cat $T/calib-url)" = "http://127.0.0.1:9460/v2/longterm/calib?id=calib-aaaaaaaa-1000&rows=40" ] && [ -z "$(ls $T/ledger/spool 2>/dev/null)" ]'
unset DOC_LEDGER_DIR DOC_BOOT_ID_FILE
: >"$T/sb"; i=0; while [ $i -lt 10 ]; do echo "$((300 + i * 60)) 200 20 30.0$MCOL 2.0 1.0 0.5" >>"$T/sb"; i=$((i + 1)); done
snap standby; unset DOC_STANDBY_STAT DOC_STANDBY_BASE; rm -rf "$T"

echo; echo "passed $PASS, failed $FAIL"; [ "$FAIL" = 0 ]
