#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# Tests for the phase-two components of luyaz-ship.sh (docs/SHIP.md「第二期」):
# touch (T12), screen-restart (R8), the sentinel (T7), install-recover, scripts,
# lweb, the v=2…v=5 transaction logs, and what luyaz-recover.sh makes of every
# power cut in between.
# Every device command is stubbed, every device path lives under $T; after
# each case the container's real /data, /tmp and /etc/init.d are untouched.
#
#   docker run --rm -v "$PWD/scripts":/scripts:ro u60-device-busybox:20260928 \
#       sh /scripts/test/luyaz-ship-p2/run.sh
#
# Time is fake: $T/uptime is /proc/uptime; the stub `sleep` advances it, then
# runs $T/hook. /proc is $T/proc: <pid>/exe is a copy of the program at the
# moment the stub started it (like the kernel's view of a running binary),
# <pid>/cmdline as the kernel writes it.
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
# ─────────────────────────────────────────────────────────────────────────────

SCRIPTS=${SCRIPTS:-/scripts}
SHIP=$SCRIPTS/luyaz-ship.sh
RECOVER=$SCRIPTS/luyaz-recover.sh
# an older luyaz-recover.sh on the device (install-recover replaces it)
OLDREC=$(mktemp)
sed '2i # an older copy' "$RECOVER" >"$OLDREC"
PASS=0
FAIL=0

ok() { PASS=$((PASS + 1)); echo "  ok   $1"; }
bad() { FAIL=$((FAIL + 1)); echo "  FAIL $1"; }
check() { # check <description> <shell test…>
    _d=$1
    shift
    if eval "$@"; then ok "$_d"; else bad "$_d  [$*]"; fi
}

_real_sleep=$(command -v sleep)
if ! command -v timeout >/dev/null 2>&1; then
    timeout() {
        _to=$1
        shift
        "$@" &
        _tp=$!
        ("$_real_sleep" "$_to" && kill -9 "$_tp") </dev/null >/dev/null 2>&1 &
        _tw=$!
        wait "$_tp"
        _trc=$?
        kill "$_tw" 2>/dev/null
        return $_trc
    }
fi

md5() { md5sum "$1" 2>/dev/null | cut -d' ' -f1; }
fp() { sh "$SHIP" tree-fp "$1"; }

leak_snapshot() { ls -la /data /tmp/luyaz/ship /tmp/luyaz-screen.log /etc/init.d 2>&1 | md5sum; }
LEAK0=$(leak_snapshot)


RC_SENT='#!/bin/sh
[ -x /data/ssh/dropbear ] && sh /data/local/tmp/start_dropbear.sh
sh /data/luyaz/ship/luyaz-recover.sh; [ $? = 10 ] && exec /bin/sh /etc/rc.local   # u60_recover
/etc/init.d/luyaz-core start
/etc/init.d/luyaz-sentinel start   # u60 ship sentinel: the screen owner and watchdog
sh /data/tailscale/start.sh
exit 0'

setup() {
    T=$(mktemp -d)
    R=$T/root
    UD=$R/data/luyaz/ui
    SD=$R/data/luyaz/screen
    mkdir -p "$T/bin" "$T/pids" "$T/tmp" "$T/proc" "$UD" "$SD" "$R/data/luyaz/ship/stage" "$R/etc/init.d" "$R/data/luyaz/core"
    echo 1000 >"$T/uptime"
    echo aaaa-1111 >"$T/boot_id"
    : >"$T/uidlog"
    : >"$T/kill.log"
    : >"$T/setsid.log"
    : >"$T/ctl.log"
    : >"$T/sent-initd.log"
    printf ':\n' >"$T/hook"
    cp "$RECOVER" "$R/data/luyaz/ship/luyaz-recover.sh"
    printf '%s\n' "$RC_SENT" >"$R/etc/rc.local"
    chmod 755 "$R/etc/rc.local"
    RCL_OLD=$(md5 "$R/etc/rc.local")
    # the touch UI's direct-write fallback (luyaz-ship.sh: touch waits for it)
    mkdir -p "$R/data/luyaz/scripts" && printf '#!/bin/sh\n# OLD luyaz-fallback.sh\n' >"$R/data/luyaz/scripts/luyaz-fallback.sh"

    cat >"$T/bin/sleep" <<EOF
#!/bin/sh
echo \$(( \$(cut -d. -f1 $T/uptime | tr -dc 0-9) + \${1%%.*} )) >$T/uptime
if [ -f $T/zombie ]; then
    z=\$(cat $T/zombie); rm -f $T/zombie
    [ "\$z" = "\$(cat $T/pids/luyaz-ui 2>/dev/null)" ] && rm -f $T/pids/luyaz-ui
    rm -rf $T/proc/\$z
fi
. $T/hook
EOF
    cat >"$T/bin/pidof" <<EOF
#!/bin/sh
cat $T/pids/"\$1" 2>/dev/null | grep . || exit 1
EOF
    # proc_new <pid> <program file> <cmdline words…>
    cat >"$T/bin/proc_new" <<EOF
#!/bin/sh
p=\$1; f=\$2; shift 2
mkdir -p $T/proc/\$p
cp "\$f" $T/proc/\$p/exe 2>/dev/null
printf '%s\0' "\$@" >$T/proc/\$p/cmdline
EOF
    cat >"$T/bin/kill" <<EOF
#!/bin/sh
echo "\$*" >>$T/kill.log
[ "\$1" = -9 ] && shift
for p in "\$@"; do
    for n in luyaz-ui.test luyaz-ui; do
        [ "\$p" = "\$(cat $T/pids/\$n 2>/dev/null)" ] || continue
        case \$n in *.test) [ -f $T/unkillable ] && continue ;; *) [ -f $T/ui-unkillable ] && continue ;; esac
        # exits but stays a zombie until its parent reaps it (the next sleep)
        if [ "\${n%.test}" = \$n ] && [ -f $T/ui-zombie ]; then
            printf '%s (%s) Z 1\n' "\$p" "\$n" >$T/proc/\$p/stat; echo "\$p" >$T/zombie; continue
        fi
        rm -f $T/pids/\$n; rm -rf $T/proc/\$p
        echo "kill \$n \$(cut -d. -f1 $T/uptime)" >>$T/events
    done
    if grep -qx "\$p" $T/pids/luyaz-sentinel 2>/dev/null; then
        [ -f $T/shadow-unkillable ] && continue
        grep -vx "\$p" $T/pids/luyaz-sentinel >$T/pids/ls.tmp; mv $T/pids/ls.tmp $T/pids/luyaz-sentinel; rm -rf $T/proc/\$p
    fi
done
exit 0
EOF
    # the sentinel under procd: start launches the program on disk as 8500 (a heartbeat,
    # adopts a running UI), stop ends it (a shadow stays)
    cat >"$T/bin/sent-initd" <<EOF
#!/bin/sh
echo "\$1 \$(cut -d. -f1 $T/uptime)" >>$T/sent-initd.log
case "\$1" in
    stop)
        for p in \$(cat $T/pids/luyaz-sentinel 2>/dev/null); do
            tr '\0' ' ' <$T/proc/\$p/cmdline | grep -q -e --shadow && continue
            rm -rf $T/proc/\$p; grep -vx "\$p" $T/pids/luyaz-sentinel >$T/pids/ls.tmp; mv $T/pids/ls.tmp $T/pids/luyaz-sentinel
        done
        ;;
    start)
        [ -f $T/sent-broken ] && grep -q NEW $R/data/luyaz/luyaz-sentinel && exit 0
        echo 8500 >>$T/pids/luyaz-sentinel
        $T/bin/proc_new 8500 $R/data/luyaz/luyaz-sentinel $R/data/luyaz/luyaz-sentinel
        u=\$(cut -d. -f1 $T/uptime)
        echo ours >$T/sent.state
        echo "\$u 8500 ours 0 \$u" >$T/sent.hb
        ;;
esac
exit 0
EOF
    cat >"$T/bin/setsid" <<EOF
#!/bin/sh
echo "\$*" >>$T/setsid.log
case "\$*" in
    *luyaz-ui.test*)
        # nohup sh -c 'cd "\$1" && exec "\$2"' sh <dir> <program>
        for a in "\$@"; do dir=\$bin; bin=\$a; done
        echo "cwd=\$dir bin=\$bin" >>$T/launch.log
        [ -f $T/test-broken ] && exit 0
        echo 9100 >$T/pids/luyaz-ui.test
        $T/bin/proc_new 9100 "\$bin" "\$bin"
        echo "test 9100 \$(cut -d. -f1 $T/uptime)" >>$T/events
        ;;
    *luyaz-ship.sh\ run\ *) "\$@" ;;
esac
exit 0
EOF
    cat >"$T/bin/date" <<'EOF'
#!/bin/sh
echo "2026-10-01 12:00:00"
EOF
    cat >"$T/bin/df" <<EOF
#!/bin/sh
echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'
echo "/dev/x 3000000 1000000 1100000 40% /data"
EOF
    # nothing else answers HTTP here (lweb's cases put their own in front)
    cat >"$T/bin/curl" <<EOF
#!/bin/sh
printf 404
EOF
    chmod +x "$T"/bin/*

    export U60S_ROOT=$R U60S_TMP=$T/tmp U60S_BOOT_ID=$T/boot_id U60S_DF=$T/bin/df U60S_PROC=$T/proc
    export U60S_MANIFEST=$R/data/luyaz/ship/manifest.jsonl
    export U60S_SENT_INITD=$T/bin/sent-initd U60S_SENT_HB=$T/sent.hb U60S_UID_CTL=$T/ctl U60S_SENT_WANT=$T/sent.want
    unset U60S_CRASH_AT U60S_DIR U60S_LOG U60S_TEST_LOG U60S_MIN_FREE_KB U60S_TO_INITD U60S_SCREEN_STATE
    export DT_UPTIME=$T/uptime DT_PIDFILE=$T/trial.pid DT_CURL=$T/bin/curl DT_PIDOF=$T/bin/pidof
    unset DT_LOGREAD # the default reader: <log>.old, then <log>
    export DT_UID_LOG=$T/uidlog DT_KILL=$T/bin/kill DT_SLEEP=$T/bin/sleep DT_DATE=$T/bin/date DT_SETSID=$T/bin/setsid
    export DT_RC=$T/rc.local DT_INITD=$T/bin/none
    export U60R_ROOT=$R
    unset U60R_DIR U60R_BOOT_ID

    # the screen as it is before every case: the old sentinel (8500) running, the old UI
    # (8200) on screen, both adopted
    printf 'OLD-UI\n' >"$UD/luyaz-ui"
    # (luyaz-recover.sh puts back only a sentinel that knows the FIFO)
    printf 'OLD-SENT /tmp/luyaz-screen.ctl\n' >"$R/data/luyaz/luyaz-sentinel"
    chmod 755 "$UD/luyaz-ui" "$R/data/luyaz/luyaz-sentinel"
    UI_OLD=$(md5 "$UD/luyaz-ui") SENT_OLD=$(md5 "$R/data/luyaz/luyaz-sentinel")
    printf '#!/bin/sh /etc/rc.common\n# luyaz-sentinel init\n' >"$R/etc/init.d/luyaz-sentinel"
    echo 8500 >"$T/pids/luyaz-sentinel"
    "$T/bin/proc_new" 8500 "$R/data/luyaz/luyaz-sentinel" "$R/data/luyaz/luyaz-sentinel"
    echo ours >"$T/sent.state"
    echo "1000 8500 ours 0 1000" >"$T/sent.hb"
    echo 8200 >"$T/pids/luyaz-ui"
    echo 8200 >"$T/nextpid"
    "$T/bin/proc_new" 8200 "$UD/luyaz-ui" "$UD/luyaz-ui"
    echo 1 >"$SD/attempts"
    # the sentinel's FIFO: hold → held; release → launch the live UI (counting launches like
    # screen.rs: the third unsteady one gives up), ours. The heartbeat follows sent.state.
    mkfifo "$T/ctl"
    (
        while :; do
            read -r l <"$T/ctl" || continue
            u=$(cut -d. -f1 "$T/uptime")
            echo "$l $u" >>"$T/ctl.log"
            case "$l" in
                hold*) echo held >"$T/sent.state"; echo "$u 8500 held 0 $u" >"$T/sent.hb" ;;
                # vendor sticks (screen.want) until ui; the vendor UI goes up once nothing holds
                vendor) echo vendor >"$T/sent.want"; echo vendor >"$T/sent.state"; echo "$u 8500 vendor 0 $u" >"$T/sent.hb" ;;
                ui) rm -f "$T/sent.want" ;;
                release)
                    grep -qx 8500 "$T/pids/luyaz-sentinel" 2>/dev/null || continue
                    echo "release $u" >>"$T/events"
                    [ -f "$T/sent.want" ] && continue
                    [ -f "$T/ui-dead" ] && { echo ours >"$T/sent.state"; echo "$u 8500 ours 0 $u" >"$T/sent.hb"; continue; }
                    if [ -s "$T/pids/luyaz-ui" ]; then echo ours >"$T/sent.state"; echo "$u 8500 ours 0 $u" >"$T/sent.hb"; continue; fi
                    a=$(cat "$SD/attempts" 2>/dev/null); a=$(( ${a:-0} + 1 )); echo $a >"$SD/attempts"
                    if [ $a -gt 2 ]; then
                        echo "giving up: $a launches did not stay up; vendor UI on screen" >>"$T/uidlog"
                        echo 1 >"$SD/gave-up"; echo vendor >"$T/sent.state"; echo "$u 8500 vendor 0 $u" >"$T/sent.hb"
                        continue
                    fi
                    if [ -f "$T/ui-broken" ] && grep -q NEW "$UD/luyaz-ui"; then
                        echo "luyaz-ui pid 9999 ended: exit 1" >>"$T/uidlog"
                        continue
                    fi
                    n=$(( $(cat "$T/nextpid") + 1 )); echo $n >"$T/nextpid"
                    echo $n >"$T/pids/luyaz-ui"
                    "$T/bin/proc_new" $n "$UD/luyaz-ui" "$UD/luyaz-ui"
                    echo "launched luyaz-ui pid $n (attempt $a of 2 before giving up)" >>"$T/uidlog"
                    echo "ui $n $u" >>"$T/events"
                    echo ours >"$T/sent.state"; echo "$u 8500 ours 0 $u" >"$T/sent.hb"
                    ;;
                quit) exit 0 ;;
            esac
        done
    ) &
    CTLR=$!
}

teardown() {
    kill $CTLR 2>/dev/null
    wait $CTLR 2>/dev/null
    check "$CASE: nothing written outside the sandbox" '[ "$(leak_snapshot)" = "$LEAK0" ]'
    rm -rf "$T"
}

hook_at() { # hook_at <uptime> <shell command>: once, at that uptime or later
    cat >"$T/hook" <<EOF
[ \$(cut -d. -f1 $T/uptime) -ge $1 ] && [ ! -f $T/hook.done ] && { touch $T/hook.done; $2; }
:
EOF
}

stage() { sh "$SHIP" stage "$1" >"$T/out" 2>&1; RC=$?; }
run() { timeout 120 sh "$SHIP" run "$1" >"$T/out" 2>&1; RC=$?; }
phase() { sed -n 's/^phase=//p' "$R/data/luyaz/ship/txn" 2>/dev/null; }
reason() { sed -n 's/^reason=//p' "$R/data/luyaz/ship/txn" 2>/dev/null; }
txnv() { sed -n 's/^v=//p' "$R/data/luyaz/ship/txn" 2>/dev/null; }
logged() { grep -q "$1" "$R/data/luyaz/ship/ship.log"; }
mlines() { grep -c . "$U60S_MANIFEST" 2>/dev/null || echo 0; }
ui_md5() { md5sum <"$T/proc/$(cat "$T/pids/luyaz-ui" 2>/dev/null)/exe" 2>/dev/null | cut -d' ' -f1; }
recover_newboot() { # recover_newboot: rc.local's line after a power cut
    echo bbbb-2222 >"$T/newboot"
    U60R_BOOT_ID=$T/newboot sh "$R/data/luyaz/ship/luyaz-recover.sh" >"$T/rout" 2>&1
    RRC=$?
}
# tree: the sandbox, byte for byte (recover.log aside: a recover that leaves
# things alone still writes its one line there)
tree() { find "$R" | grep -v "/luyaz/ship/recover\.log$" | sort; find "$R" -type f | grep -v "/luyaz/ship/recover\.log$" | sort | while read -r f; do md5sum "$f"; done; }
meta() { # meta <txn> <comp> [extra lines]
    {
        echo v=1
        echo "txn=$1"
        echo "comp=$2"
        echo commit=7e8d223
        echo mac_time=1790000000
        echo format=1
        [ -n "$3" ] && printf '%s\n' "$3"
    } >"${META_DIR:-$R/data/luyaz/ship/stage}/$1/meta"
}

echo "== u60-ship phase two =="

# ═══ T12 touch ══════════════════════════════════════════════════════════════
TT=20261001-120000-touch
upload_touch() { # upload_touch [txn] [UI content]
    _t=${1:-$TT}
    mkdir -p "$R/data/luyaz/ship/stage/$_t"
    printf '%s\n' "${2:-NEW-UI $_t}" >"$R/data/luyaz/ship/stage/$_t/luyaz-ui"
    UI_NEW=$(md5 "$R/data/luyaz/ship/stage/$_t/luyaz-ui")
    meta "$_t" touch "file=luyaz-ui $UI_NEW"
}
# vok <txn>: the core's trial judge says ok about it (touch is judged by the core, §7 D5)
vok() { echo "$1 ok judged" >"$R/data/luyaz/ship/verdict"; }
NL='
'

setup
CASE="ship touch"
upload_touch
stage $TT
check "$CASE: staged as v=1" '[ "$RC" = 0 ] && [ "$(phase)" = staged ] && [ "$(txnv)" = 1 ]'
check "$CASE: side file in place" '[ "$(md5 "$UD/luyaz-ui.test")" = "$UI_NEW" ] && [ -x "$UD/luyaz-ui.test" ]'
vok $TT
cat >"$T/hook" <<EOF
read -r hb _p _t ph <$T/tmp/heartbeat 2>/dev/null && echo "\$ph \$(( \$(cut -d. -f1 $T/uptime) - hb ))" >>$T/hb.trace
:
EOF
run $TT
check "$CASE: done" '[ "$RC" = 0 ] && [ "$(phase)" = done ]'
check "$CASE: live luyaz-ui is the new one, previous kept" '[ "$(md5 "$UD/luyaz-ui")" = "$UI_NEW" ] && [ "$(md5 "$UD/luyaz-ui.prev-$TT")" = "$UI_OLD" ]'
check "$CASE: the test build ran from the UI directory, as the sentinel runs the UI" "grep -qx 'cwd=$UD bin=$UD/luyaz-ui.test' '$T/launch.log' && grep -q 'nohup sh -c cd \"\$1\" && exec \"\$2\" sh $UD $UD/luyaz-ui.test' '$T/setsid.log'"
check "$CASE: the sentinel held the screen for the whole transaction, then released it" '[ "$(cut -d" " -f1 "$T/ctl.log" | tr "\n" " ")" = "hold release " ] && [ "$(awk "NR == 1 { print \$2 }" "$T/ctl.log")" -ge 480 ]'
check "$CASE: the sentinel kept running (never stopped)" '[ "$(cat "$T/pids/luyaz-sentinel")" = 8500 ] && [ ! -s "$T/sent-initd.log" ]'
check "$CASE: the old UI was killed by pid, the test build after the trial" "grep -qx '8200' '$T/kill.log' && grep -qx '9100' '$T/kill.log'"
check "$CASE: the new UI runs (exe md5)" '[ "$(ui_md5)" = "$UI_NEW" ] && [ ! -s "$T/pids/luyaz-ui.test" ]'
check "$CASE: launch count cleared before the release (1 = this launch)" '[ "$(cat "$SD/attempts")" = 1 ] && [ ! -e "$SD/gave-up" ]'
check "$CASE: trial heartbeat every second" '[ "$(grep -c "^trial " "$T/hb.trace")" -ge 60 ] && [ -z "$(awk "\$1 == \"trial\" && \$2 > 1" "$T/hb.trace")" ]'
check "$CASE: manifest line with luyaz-ui" "grep -q '\"comp\":\"touch\",\"txn\":\"$TT\"' '$U60S_MANIFEST'"
check "$CASE: no side file left" '[ ! -e "$UD/luyaz-ui.test" ]'
teardown

setup
CASE="touch rollback after an ordinary ship"
upload_touch
stage $TT
vok $TT
run $TT
sh "$SHIP" prepare-rollback touch 20261001-130000-touch 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: prepared, luyaz-ui from the .prev" '[ "$RC" = 0 ] && [ "$(md5 "$R/data/luyaz/ship/stage/20261001-130000-touch/luyaz-ui")" = "$UI_OLD" ] && grep -qx "file=luyaz-ui $UI_OLD" "$R/data/luyaz/ship/stage/20261001-130000-touch/meta"'
teardown

setup
CASE="touch: the core never says ok"
upload_touch
stage $TT
run $TT
check "$CASE: rolled back (D5, fail closed), the old UI back on screen" '[ "$(phase)" = rolledback ] && reason | grep -q "判定没到 ok" && [ "$(ui_md5)" = "$UI_OLD" ]'
teardown

setup
CASE="touch: the core says fail during the check"
upload_touch
stage $TT
echo "$TT fail judged" >"$R/data/luyaz/ship/verdict"
run $TT
check "$CASE: rolled back early" '[ "$(phase)" = rolledback ] && reason | grep -q "核心判定不过"'
teardown

setup
CASE="touch: test build crashes 5 s in"
upload_touch
stage $TT
hook_at 1005 "rm -f $T/pids/luyaz-ui.test; rm -rf $T/proc/9100; echo \"crash \$(cut -d. -f1 $T/uptime)\" >>$T/events"
run $TT
CRASH=$(sed -n 's/^crash //p' "$T/events")
START=$(awk '$1 == "release" { print $2; exit }' "$T/events")
check "$CASE: aborted, says why" '[ "$RC" = 1 ] && [ "$(phase)" = aborted ] && reason | grep -q "^试跑不过：测试版崩溃"'
check "$CASE: production released ≤ 1 s after the crash (crash $CRASH, release $START)" '[ -n "$CRASH" ] && [ -n "$START" ] && [ $((START - CRASH)) -le 1 ]'
check "$CASE: the old UI runs again" '[ "$(ui_md5)" = "$UI_OLD" ]'
check "$CASE: live untouched, no .prev, no side files" '[ "$(md5 "$UD/luyaz-ui")" = "$UI_OLD" ] && [ ! -e "$UD/luyaz-ui.prev-$TT" ] && [ ! -e "$UD/luyaz-ui.test" ]'
check "$CASE: manifest untouched" '[ "$(mlines)" = 0 ]'
teardown

setup
CASE="touch: test build never shows (screen dark > 20 s)"
touch "$T/test-broken"
upload_touch
stage $TT
run $TT
KILL_AT=$(awk '$1 == "kill" && $2 == "luyaz-ui" { print $3; exit }' "$T/events")
START=$(awk '$1 == "release" { print $2; exit }' "$T/events")
check "$CASE: aborted after 20 s of an empty screen" '[ "$(phase)" = aborted ] && reason | grep -q "屏幕空了 20s"'
check "$CASE: production back ≤ 21 s after the old UI went (gone $KILL_AT, release $START)" '[ $((START - KILL_AT)) -le 21 ] && [ "$(ui_md5)" = "$UI_OLD" ]'
teardown

setup
CASE="touch: three ships in 10 minutes"
for n in 1 2 3; do
    t=2026100${n}-120000-touch
    upload_touch "$t"
    stage "$t"
    vok "$t"
    run "$t"
    [ "$(phase)" = done ] && ok "$CASE: ship $n done" || bad "$CASE: ship $n: $(phase) $(reason)"
done
check "$CASE: all within 10 minutes of fake time ($(($(cat "$T/uptime") - 1000)) s)" '[ $(($(cat "$T/uptime") - 1000)) -lt 600 ]'
check "$CASE: the sentinel never gave up, count back to 1 each time" '! grep -q "^giving up" "$T/uidlog" && [ ! -e "$SD/gave-up" ] && [ "$(cat "$SD/attempts")" = 1 ]'
check "$CASE: the third build runs" '[ "$(ui_md5)" = "$UI_NEW" ]'
teardown

setup
CASE="touch: new UI restarted during the check"
upload_touch
stage $TT
vok $TT
cat >"$T/hook" <<EOF
if grep -qx phase=check $R/data/luyaz/ship/txn && [ ! -f $T/once ]; then
    touch $T/once; echo 8300 >$T/pids/luyaz-ui; $T/bin/proc_new 8300 $UD/luyaz-ui $UD/luyaz-ui
fi
:
EOF
run $TT
check "$CASE: rolled back, says why" '[ "$(phase)" = rolledback ] && reason | grep -q "^转正后检查不过：界面被重启过"'
check "$CASE: old luyaz-ui back, old UI on screen" '[ "$(md5 "$UD/luyaz-ui")" = "$UI_OLD" ] && [ "$(ui_md5)" = "$UI_OLD" ]'
teardown

setup
CASE="touch: the sentinel gives up during the check"
upload_touch
stage $TT
vok $TT
cat >"$T/hook" <<EOF
grep -qx phase=check $R/data/luyaz/ship/txn && [ ! -f $T/once ] && { touch $T/once; echo "giving up: 2 launches did not stay up" >>$T/uidlog; }
:
EOF
run $TT
check "$CASE: rolled back" '[ "$(phase)" = rolledback ] && reason | grep -q "界面异常（哨兵：giving up"'
teardown

# the sentinel moves /tmp/luyaz-screen.log to .old at 64 KB (mv); its next line then
# starts a new file
setup
CASE="touch: the sentinel gives up just before its log is rotated"
upload_touch
stage $TT
vok $TT
cat >"$T/hook" <<EOF
grep -qx phase=check $R/data/luyaz/ship/txn && [ ! -f $T/once ] && { touch $T/once; echo "giving up: 2 launches did not stay up" >>$T/uidlog; mv -f $T/uidlog $T/uidlog.old; }
:
EOF
run $TT
check "$CASE: rolled back all the same" '[ "$(phase)" = rolledback ] && reason | grep -q "界面异常（哨兵：giving up"'
teardown

setup
CASE="touch: the screen log rotates during the check, nothing bad in it"
echo "giving up: an old one, before this ship" >"$T/uidlog.old"
upload_touch
stage $TT
vok $TT
cat >"$T/hook" <<EOF
grep -qx phase=check $R/data/luyaz/ship/txn && [ ! -f $T/once ] && { touch $T/once; echo "launched luyaz-ui pid 1 (attempt 1 of 2 before giving up)" >>$T/uidlog; mv -f $T/uidlog $T/uidlog.old; }
:
EOF
run $TT
check "$CASE: done (an old .old replaced is not a new bad line)" '[ "$(phase)" = done ] && [ -f "$T/uidlog.old" ]'
teardown

setup
CASE="touch: the sentinel gives up just before a rotation that replaces an old .old with a bad line"
echo "giving up: an old one, before this ship" >"$T/uidlog.old"
upload_touch
stage $TT
vok $TT
cat >"$T/hook" <<EOF
grep -qx phase=check $R/data/luyaz/ship/txn && [ ! -f $T/once ] && { touch $T/once; echo "giving up: 2 launches did not stay up" >>$T/uidlog; mv -f $T/uidlog $T/uidlog.old; }
:
EOF
run $TT
check "$CASE: rolled back" '[ "$(phase)" = rolledback ] && reason | grep -q "界面异常（哨兵：giving up: 2 launches"'
teardown

setup
CASE="touch: an old .old with a bad line deleted during the check, then rotated twice"
echo "giving up: an old one, before this ship" >"$T/uidlog.old"
echo "luyaz-ui pid 1 ended: exit 1 (an old one)" >>"$T/uidlog"
upload_touch
stage $TT
vok $TT
cat >"$T/hook" <<EOF
if grep -qx phase=check $R/data/luyaz/ship/txn; then
    if [ ! -f $T/once ]; then touch $T/once; rm -f $T/uidlog.old
    elif [ ! -f $T/twice ]; then touch $T/twice; mv -f $T/uidlog $T/uidlog.old; echo "launched luyaz-ui pid 1 (attempt 1 of 2 before giving up)" >>$T/uidlog
    elif [ ! -f $T/thrice ]; then touch $T/thrice; mv -f $T/uidlog $T/uidlog.old
    fi
fi
:
EOF
run $TT
check "$CASE: done (nothing new)" '[ "$(phase)" = done ] && [ -f "$T/thrice" ]'
teardown

setup
CASE="touch: new UI will not stay up"
touch "$T/ui-broken"
upload_touch "" "NEW-UI but it crashes"
stage $TT
vok $TT
run $TT
check "$CASE: rolled back (new version will not start)" '[ "$(phase)" = rolledback ] && reason | grep -q "^新版起不来：哨兵收回屏幕后界面没起来"'
check "$CASE: old UI running at the end" '[ "$(md5 "$UD/luyaz-ui")" = "$UI_OLD" ] && [ "$(ui_md5)" = "$UI_OLD" ]'
teardown

setup
CASE="touch: test build unkillable"
touch "$T/unkillable"
upload_touch
stage $TT
run $TT
check "$CASE: failed; production NOT started next to it (two UIs = reboot)" '[ "$(phase)" = failed ] && ! grep -q "^release" "$T/events"'
check "$CASE: live untouched" '[ "$(md5 "$UD/luyaz-ui")" = "$UI_OLD" ]'
teardown

setup
CASE="touch: the old UI exits and stays a zombie for a while"
touch "$T/ui-zombie"
upload_touch
stage $TT
run $TT
check "$CASE: not taken for a UI that will not exit" '! reason | grep -q "没有退出"'
check "$CASE: test build launched" '[ -s "$T/launch.log" ]'
teardown

setup
CASE="touch: old UI will not exit"
touch "$T/ui-unkillable"
upload_touch
stage $TT
run $TT
check "$CASE: aborted, test build never launched, the screen released again" '[ "$(phase)" = aborted ] && reason | grep -q "正式界面 10s 内没有退出" && [ ! -s "$T/launch.log" ] && grep -q "^release" "$T/events"'
teardown

setup
CASE="touch: the test build is still running"
echo 9100 >"$T/pids/luyaz-ui.test"
upload_touch
stage $TT
check "$CASE: stage refused" '[ "$RC" = 1 ] && grep -q "测试版 luyaz-ui.test 还在跑" "$T/out" && [ ! -e "$R/data/luyaz/ship/txn" ]'
teardown

setup
CASE="touch: a meta that lists another file"
upload_touch
printf '#!/bin/sh\nexit 0\n' >"$R/data/luyaz/ship/stage/$TT/start.sh"
meta $TT touch "file=luyaz-ui $UI_NEW${NL}file=start.sh $(md5 "$R/data/luyaz/ship/stage/$TT/start.sh")"
stage $TT
check "$CASE: stage refused" '[ "$RC" != 0 ] && grep -q "不是组件 touch 的文件" "$T/out" && [ ! -e "$UD/luyaz-ui.test" ]'
teardown

setup
CASE="touch: no sentinel running"
rm -f "$T/pids/luyaz-sentinel"; rm -rf "$T/proc/8500"
upload_touch
stage $TT
run $TT
check "$CASE: not done, nobody to release the screen to: says so" '[ "$(phase)" != done ] && grep -q "哨兵没在跑" "$R/data/luyaz/ship/ship.log"'
teardown

# power cuts: touch logs are v=1
for p in trial:running promote:moved:luyaz-ui check:begin; do
    setup
    CASE="touch: power cut at $p"
    upload_touch
    stage $TT
    U60S_CRASH_AT=$p run $TT
    recover_newboot
    case "$p" in
        trial:*) check "$CASE: aborted, live old" '[ "$(phase)" = aborted ] && [ "$(md5 "$UD/luyaz-ui")" = "$UI_OLD" ]' ;;
        *) check "$CASE: rolled back, luyaz-ui old" '[ "$(phase)" = rolledback ] && [ "$(md5 "$UD/luyaz-ui")" = "$UI_OLD" ]' ;;
    esac
    teardown
done

setup
CASE="touch: recover-live after kill -9 in the check"
upload_touch
stage $TT
U60S_CRASH_AT=check:begin run $TT
: >"$T/events"
sh "$SHIP" recover-live >"$T/out" 2>&1
check "$CASE: rolled back, the screen released to the old UI" '[ "$(phase)" = rolledback ] && [ "$(md5 "$UD/luyaz-ui")" = "$UI_OLD" ] && grep -q "^release" "$T/events" && [ "$(ui_md5)" = "$UI_OLD" ]'
teardown

setup
CASE="touch: recover-live after kill -9 in the trial"
upload_touch
stage $TT
U60S_CRASH_AT=trial:running run $TT
sh "$SHIP" recover-live >"$T/out" 2>&1
check "$CASE: aborted, test build killed, the old UI back" '[ "$(phase)" = aborted ] && [ ! -s "$T/pids/luyaz-ui.test" ] && [ "$(ui_md5)" = "$UI_OLD" ]'
teardown

# pidof: the names do not see each other (real busybox pidof, real
# processes: comm luyaz-ui vs luyaz-ui.test)
CASE="pidof names"
P=$(mktemp -d)
printf '#!/bin/sh\nwhile :; do %s 1; done\n' "$_real_sleep" >"$P/luyaz-ui"
cp "$P/luyaz-ui" "$P/luyaz-ui.test"
chmod 755 "$P/luyaz-ui" "$P/luyaz-ui.test"
"$P/luyaz-ui" &
A=$!
"$P/luyaz-ui.test" &
B=$!
"$_real_sleep" 0.3
check "$CASE: pidof luyaz-ui = the live one only" '[ "$(pidof luyaz-ui)" = "$A" ]'
check "$CASE: pidof luyaz-ui.test = the test build only" '[ "$(pidof luyaz-ui.test)" = "$B" ]'
check "$CASE: the test build's comm fits (13 < 15 characters)" '[ "$(cat /proc/$B/comm)" = luyaz-ui.test ]'
kill $A $B 2>/dev/null
wait $A $B 2>/dev/null
rm -rf "$P"

# ═══ screen-restart (R8) ════════════════════════════════════════════════════
setup
CASE="screen-restart"
sh "$SHIP" screen-restart >"$T/out" 2>&1
RC=$?
check "$CASE: exit 0, released, the UI on screen kept (adopted)" '[ $RC = 0 ] && [ "$(cut -d" " -f1 "$T/ctl.log")" = release ] && grep -q "版本对" "$T/out" && [ "$(cat "$T/pids/luyaz-ui")" = 8200 ]'
check "$CASE: launch count and give-up cleared" '[ ! -e "$SD/attempts" ] && [ ! -e "$SD/gave-up" ]'
sh "$SHIP" screen-restart "$UI_OLD" >"$T/out" 2>&1
RC=$?
check "$CASE: explicit md5: exit 0" '[ $RC = 0 ]'
sh "$SHIP" screen-restart nothex >"$T/out" 2>&1
RC=$?
check "$CASE: a bad md5 refused" '[ $RC = 1 ] && grep -q "不是 md5" "$T/out"'
rm -f "$T/pids/luyaz-ui"
echo 5 >"$SD/attempts"
echo 1 >"$SD/gave-up"
sh "$SHIP" screen-restart >"$T/out" 2>&1
RC=$?
check "$CASE: after a give-up: cleared, UI launched" '[ $RC = 0 ] && [ -s "$T/pids/luyaz-ui" ] && [ "$(cat "$SD/attempts")" = 1 ]'
printf 'v=1\ntxn=%s\ncomp=core\nphase=check\nend=1\n' 20261001-110000-core >"$R/data/luyaz/ship/txn"
sh "$SHIP" screen-restart >"$T/out" 2>&1
RC=$?
check "$CASE: refused while a transaction is in progress" '[ $RC = 1 ] && grep -q "还在进行" "$T/out"'
teardown

setup
CASE="screen-restart, no sentinel running"
rm -f "$T/pids/luyaz-sentinel"; rm -rf "$T/proc/8500"
sh "$SHIP" screen-restart >"$T/out" 2>&1
RC=$?
check "$CASE: exit 1 at once, says the sentinel is down" '[ $RC = 1 ] && grep -q "哨兵没在跑" "$T/out" && [ ! -s "$T/ctl.log" ]'
check "$CASE: a UI on screen: the sentinel is not started behind the owner's back" '[ ! -s "$T/sent-initd.log" ]'
rm -f "$T/pids/luyaz-ui"; rm -rf "$T/proc/8200"
sh "$SHIP" screen-restart >"$T/out" 2>&1
RC=$?
check "$CASE: no UI either: still exit 1, the sentinel started so it launches one" '[ $RC = 1 ] && grep -q "哨兵没在跑" "$T/out" && grep -q "^start" "$T/sent-initd.log"'
teardown

# 10-25 u60g: a touch ship failed to hand the screen back and the panel stayed dark until the
# firmware rebooted (1185). A failed release puts the vendor UI up; the next release undoes it.
setup
CASE="screen-restart, the UI never comes back"
rm -f "$T/pids/luyaz-ui"; rm -rf "$T/proc/8200"
touch "$T/ui-dead"
sh "$SHIP" screen-restart >"$T/out" 2>&1
RC=$?
check "$CASE: exit 1 with the release's reason" '[ $RC = 1 ] && grep -q "界面没起来" "$T/out"'
check "$CASE: then release + vendor: the vendor UI on screen" '[ "$(cut -d" " -f1 "$T/ctl.log" | tr "\n" " ")" = "release release vendor " ] && [ "$(cat "$T/sent.state")" = vendor ]'
rm -f "$T/ui-dead"
sh "$SHIP" screen-restart >"$T/out" 2>&1
RC=$?
check "$CASE: the next release says ui first and gets our UI back" '[ $RC = 0 ] && [ "$(tail -n 2 "$T/ctl.log" | cut -d" " -f1 | tr "\n" " ")" = "ui release " ] && [ ! -e "$T/sent.want" ] && [ -s "$T/pids/luyaz-ui" ]'
teardown

setup
CASE="screen-restart, vendor UI already up"
rm -f "$T/pids/luyaz-ui"; rm -rf "$T/proc/8200"
echo 3 >"$SD/attempts"
sh "$SHIP" screen-restart >"$T/out" 2>&1
RC=$?
check "$CASE: whatever the outcome, no vendor request on top of a UI" '! grep -q "^vendor" "$T/ctl.log"'
teardown

setup
CASE="record-kit touch (the install kit's do_ui)"
sh "$SHIP" record-kit touch 20261001 abc1234 1 1790000000 >"$T/out" 2>&1
RC=$?
check "$CASE: touch: luyaz-ui only" "[ \$RC = 0 ] && grep -q '\"comp\":\"touch\".*\"files\":\[{\"path\":\"$UD/luyaz-ui\",\"md5\":\"$UI_OLD\"}\]' '$U60S_MANIFEST'"
teardown

# ═══ install-recover ════════════════════════════════════════════════════════
TR=20261001-120000-recover
upload_recover() { # upload_recover [file to use]
    mkdir -p "$R/data/luyaz/ship/stage/$TR"
    cp "${1:-$RECOVER}" "$R/data/luyaz/ship/stage/$TR/luyaz-recover.sh"
    {
        echo v=1
        echo "txn=$TR"
        echo comp=recover
        echo commit=abcdef1
        echo mac_time=1790000000
        echo "file=luyaz-recover.sh $(md5 "$R/data/luyaz/ship/stage/$TR/luyaz-recover.sh")"
    } >"$R/data/luyaz/ship/stage/$TR/meta"
}
irun() { timeout 120 sh "$SHIP" install-recover "$TR" >"$T/out" 2>&1; RC=$?; }

setup
CASE="install-recover"
cp "$OLDREC" "$R/data/luyaz/ship/luyaz-recover.sh"
upload_recover
irun
check "$CASE: exit 0, the new one live, formats 1 2 3 4 5" '[ "$RC" = 0 ] && [ "$(md5 "$R/data/luyaz/ship/luyaz-recover.sh")" = "$(md5 "$RECOVER")" ] && [ "$(sh "$R/data/luyaz/ship/luyaz-recover.sh" formats)" = "1 2 3 4 5" ]'
check "$CASE: the old one kept, the stage directory gone" '[ "$(md5 "$R/data/luyaz/ship/luyaz-recover.sh.prev-$TR")" = "$(md5 "$OLDREC")" ] && [ ! -e "$R/data/luyaz/ship/stage/$TR" ]'
check "$CASE: a record line" "grep -q '\"kind\":\"record\",\"name\":\"luyaz-recover.sh\",\"path\":\"$R/data/luyaz/ship/luyaz-recover.sh\",\"md5\":\"$(md5 "$RECOVER")\".*\"why\":\"install-recover abcdef1\"' '$U60S_MANIFEST'"
teardown

irefused() { # irefused <case> <text>
    CASE=$1
    check "$1: refused, says why" "[ \"\$RC\" = 1 ] && grep -q -- '$2' '$T/out'"
    check "$1: the live one untouched" '[ "$(md5 "$R/data/luyaz/ship/luyaz-recover.sh")" = "$(md5 "$OLDREC")" ] && [ "$(mlines)" = 0 ]'
}
setup; cp "$OLDREC" "$R/data/luyaz/ship/luyaz-recover.sh"; upload_recover; echo x >>"$R/data/luyaz/ship/stage/$TR/luyaz-recover.sh"; irun
irefused "install-recover: md5 wrong" "md5 不对"; teardown
setup; cp "$OLDREC" "$R/data/luyaz/ship/luyaz-recover.sh"; printf 'case x in\n' >"$T/bad.sh"; upload_recover "$T/bad.sh"; irun
irefused "install-recover: syntax error" "语法错误"; teardown
setup; cp "$OLDREC" "$R/data/luyaz/ship/luyaz-recover.sh"; sed 's/t "完成 → 不动" done "new$NL" "old$NL" done new/t "完成 → 不动" done "new$NL" "old$NL" done old/' "$RECOVER" >"$T/st.sh"; upload_recover "$T/st.sh"; irun
irefused "install-recover: its selftest fails" "自检没过"; teardown
setup; cp "$OLDREC" "$R/data/luyaz/ship/luyaz-recover.sh"; upload_recover; echo "evil=1" >>"$R/data/luyaz/ship/stage/$TR/meta"; irun
irefused "install-recover: unknown meta key" "看不懂的一行"; teardown
setup; cp "$OLDREC" "$R/data/luyaz/ship/luyaz-recover.sh"; upload_recover; printf 'v=1\ntxn=x\ncomp=core\nphase=check\nend=1\n' >"$R/data/luyaz/ship/txn"; irun
irefused "install-recover: during a transaction" "还在进行"; teardown
setup; cp "$OLDREC" "$R/data/luyaz/ship/luyaz-recover.sh"; mkdir -p "$R/data/luyaz/ship/stage/20261001-120000-touch"; sh "$SHIP" install-recover 20261001-120000-touch >"$T/out" 2>&1; RC=$?
irefused "install-recover: txn not ending in -recover" "以 -recover 结尾"; teardown


# ═══ record: the directories ════════════════════════════════════════════════
setup
CASE="record directories"
mkdir -p "$UD/fonts" "$UD/operator-logos"
printf 'font\n' >"$UD/fonts/u60-cjk-fallback.ttf"
printf 'png\n' >"$UD/operator-logos/cmcc.png"
sh "$SHIP" record ui-fonts 1790000000 >"$T/out" 2>&1
RC=$?
check "$CASE: fonts recorded by fingerprint" "[ \$RC = 0 ] && grep -q '\"name\":\"ui-fonts\",\"path\":\"$UD/fonts\",\"tree\":\"$(fp "$UD/fonts")\"' '$U60S_MANIFEST' && grep -q 'u60-ship: 已记录 ui-fonts $(fp "$UD/fonts")' '$T/out'"
rm -rf "$UD/operator-logos"
sh "$SHIP" record ui-logos 1790000000 >"$T/out" 2>&1
check "$CASE: an absent directory is recorded as -" "grep -q '\"name\":\"ui-logos\",\"path\":\"$UD/operator-logos\",\"tree\":\"-\"' '$U60S_MANIFEST'"
ln -s u60-cjk-fallback.ttf "$UD/fonts/link.ttf"
sh "$SHIP" record ui-fonts 1790000000 >"$T/out" 2>&1
RC=$?
check "$CASE: a symlink inside: refused" '[ $RC = 1 ] && grep -q "算不了指纹" "$T/out" && [ "$(mlines)" = 2 ]'
sh "$SHIP" record devui-fonts 1790000000 >"$T/out" 2>&1  # RESIDUE-OK: retired component name must stay refused
RC=$?
check "$CASE: the old name is not known" '[ $RC = 1 ] && grep -q "不认识" "$T/out"'
teardown

# ═══ T7 sentinel (manager docs/designs/u60-sentinel.md) ═════════════════════
TS=20261010-120000-sentinel
# sent_setup: as setup, plus a shadow sentinel (8600); the heartbeat moves every sleep
# (unless hb-stuck); the core's verdict as $T/verdict-at says: "<uptime> <word> <txn>"
sent_setup() {
    setup
    echo 8600 >>"$T/pids/luyaz-sentinel"
    "$T/bin/proc_new" 8600 "$R/data/luyaz/luyaz-sentinel" "$R/data/luyaz/luyaz-sentinel" --shadow
    cat >"$T/hook" <<SEOF
u=\$(cut -d. -f1 $T/uptime)
st=\$(cat $T/sent.state 2>/dev/null); [ -n "\$st" ] || st=ours
if grep -qx 8500 $T/pids/luyaz-sentinel 2>/dev/null && [ ! -f $T/hb-stuck ]; then echo "\$u 8500 \$st 0 \$u" >$T/sent.hb; fi
if [ -f $T/verdict-at ]; then read -r va vw vt <$T/verdict-at; [ \$u -ge \$va ] && echo "\$vt \$vw judged" >$R/data/luyaz/ship/verdict; fi
:
SEOF
}
upload_sent() { # upload_sent <txn>
    mkdir -p "$R/data/luyaz/ship/stage/$1"
    printf 'NEW-SENT %s\n' "$1" >"$R/data/luyaz/ship/stage/$1/luyaz-sentinel"
    SENT_NEW=$(md5 "$R/data/luyaz/ship/stage/$1/luyaz-sentinel")
    meta "$1" sentinel "file=luyaz-sentinel $SENT_NEW"
}
verdict_at() { echo "$(($(cut -d. -f1 "$T/uptime") + $1)) $2 ${3:-$TS}" >"$T/verdict-at"; }
sent_pids_t() { cat "$T/pids/luyaz-sentinel" 2>/dev/null | tr '\n' ' '; }

sent_setup
CASE="sentinel: upgrade"
upload_sent $TS
stage $TS
check "$CASE: staged as v=1, rc.local not in it" '[ "$RC" = 0 ] && [ "$(phase)" = staged ] && [ "$(txnv)" = 1 ] && [ ! -e "$R/etc/rc.local.test" ]'
verdict_at 120 ok
run $TS
check "$CASE: done" '[ "$RC" = 0 ] && [ "$(phase)" = done ]'
check "$CASE: rc.local untouched" '[ "$(md5 "$R/etc/rc.local")" = "$RCL_OLD" ]'
check "$CASE: shadow killed, the old sentinel replaced, never two" '! grep -qx 8600 "$T/pids/luyaz-sentinel" && [ "$(sent_pids_t)" = "8500 " ] && [ "$(md5sum <"$T/proc/8500/exe" | cut -d" " -f1)" = "$SENT_NEW" ]'
check "$CASE: the new sentinel adopted the same UI" '[ "$(cat "$T/pids/luyaz-ui")" = 8200 ]'
check "$CASE: manifest line" "grep -q '\"comp\":\"sentinel\",\"txn\":\"$TS\"' '$U60S_MANIFEST'"
teardown

sent_setup
CASE="sentinel: refusals"
printf '%s\n/etc/init.d/luyaz-sentinel start\n' "$RC_SENT" >"$R/etc/rc.local"
upload_sent $TS
stage $TS
check "$CASE: an rc.local it does not know (two sentinel lines)" '[ "$RC" = 1 ] && grep -q "rc.local 不是认得的样子" "$T/out"'
printf '%s\n' "$RC_SENT" | grep -v luyaz-recover >"$R/etc/rc.local"
upload_sent $TS
stage $TS
check "$CASE: no luyaz-recover.sh line" '[ "$RC" = 1 ] && grep -q "rc.local 不是认得的样子" "$T/out"'
printf '%s\n' "$RC_SENT" >"$R/etc/rc.local"
printf '{"v":1,"kind":"ship","comp":"core","txn":"x","commit":"1","format":1,"mac_time":1,"boot_id":"aaaa-1111","uptime":900,"files":[],"state":[]}\n' >"$U60S_MANIFEST"
upload_sent $TS
stage $TS
check "$CASE: core shipped 100 s ago (D4)" '[ "$RC" = 1 ] && grep -q "一次只换一个" "$T/out"'
mv "$U60S_SENT_INITD" "$T/bin/sent-initd.away"
: >"$U60S_MANIFEST"
upload_sent $TS
stage $TS
check "$CASE: no sentinel init yet (ship scripts first)" '[ "$RC" = 1 ] && grep -q "先 u60 ship scripts" "$T/out"'
upload_sent $TS
printf 'note=convert\n' >>"$R/data/luyaz/ship/stage/$TS/meta"
mv "$T/bin/sent-initd.away" "$U60S_SENT_INITD"
stage $TS
check "$CASE: note=convert is not known any more" '[ "$RC" = 1 ] && grep -q "note “convert” 看不懂" "$T/out"'
teardown

sent_setup
CASE="sentinel: the core never says ok"
upload_sent $TS
stage $TS
run $TS
check "$CASE: rolled back, says why" '[ "$RC" = 1 ] && [ "$(phase)" = rolledback ] && reason | grep -q "判定没到 ok"'
check "$CASE: the old sentinel runs again on the same UI, rc.local untouched" '[ "$(md5 "$R/data/luyaz/luyaz-sentinel")" = "$SENT_OLD" ] && [ "$(sent_pids_t)" = "8500 " ] && [ "$(md5sum <"$T/proc/8500/exe" | cut -d" " -f1)" = "$SENT_OLD" ] && [ -s "$T/pids/luyaz-ui" ] && [ "$(md5 "$R/etc/rc.local")" = "$RCL_OLD" ]'
teardown

sent_setup
CASE="sentinel: the core says fail"
upload_sent $TS
stage $TS
verdict_at 40 fail
run $TS
check "$CASE: rolled back early" '[ "$(phase)" = rolledback ] && reason | grep -q "核心判定不过"'
teardown

sent_setup
CASE="sentinel: a verdict about another transaction"
upload_sent $TS
stage $TS
verdict_at 10 ok 20261010-110000-touch
run $TS
check "$CASE: not taken: rolled back" '[ "$(phase)" = rolledback ] && reason | grep -q "判定没到 ok"'
teardown

sent_setup
CASE="sentinel: heartbeat stuck"
upload_sent $TS
stage $TS
verdict_at 30 ok
touch "$T/hb-stuck"
run $TS
check "$CASE: rolled back" '[ "$(phase)" = rolledback ] && reason | grep -q "心跳"'
teardown

sent_setup
CASE="sentinel: a shadow that will not die"
upload_sent $TS
stage $TS
touch "$T/shadow-unkillable"
run $TS
check "$CASE: no new sentinel ever started" '[ "$(phase)" != done ] && [ "$(md5 "$R/data/luyaz/luyaz-sentinel")" = "$SENT_OLD" ]'
teardown

sent_setup
CASE="sentinel: power cut in check, next boot"
upload_sent $TS
stage $TS
U60S_CRASH_AT=check:begin run $TS
recover_newboot
check "$CASE: luyaz-recover.sh puts the old sentinel back, rc.local untouched" '[ "$RRC" = 0 ] && [ "$(md5 "$R/data/luyaz/luyaz-sentinel")" = "$SENT_OLD" ] && [ "$(md5 "$R/etc/rc.local")" = "$RCL_OLD" ] && [ "$(phase)" = rolledback ]'
teardown

sent_setup
CASE="sentinel: prepare-rollback"
upload_sent $TS
stage $TS
verdict_at 120 ok
run $TS
sh "$SHIP" prepare-rollback sentinel 20261010-130000-sentinel 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: prepared from the .prev" '[ "$RC" = 0 ] && [ "$(md5 "$R/data/luyaz/ship/stage/20261010-130000-sentinel/luyaz-sentinel")" = "$SENT_OLD" ]'
teardown

# ═══ scripts (D2) ═══════════════════════════════════════════════════════════
setup
CASE="ship scripts"
TSC=20261010-140000-scripts
mkdir -p "$R/data/luyaz/ship/stage/$TSC"
_ml=
# the first ship into a new directory (setup's luyaz-fallback.sh aside); a file not in the
# list is left as it is
rm -rf "$R/data/luyaz/scripts"
mkdir -p "$R/data/luyaz/scripts" && printf '#!/bin/sh\n# OLD other.sh\n' >"$R/data/luyaz/scripts/other.sh"
for f in alert-lib.sh web-auth.sh chaos.sh doctor.sh config-backup.sh wan-sources.sh rpc-acl.sh luyaz-fallback.sh net-tune.sh VERSION; do
    printf '#!/bin/sh\n# NEW %s luyaz-write.lock\nexit 0\n' "$f" >"$R/data/luyaz/ship/stage/$TSC/$f"
    _ml="$_ml${_ml:+$NL}file=$f $(md5 "$R/data/luyaz/ship/stage/$TSC/$f")"
done
printf '#!/bin/sh\n# OLD init\n' >"$R/etc/init.d/other"
for f in luyaz-core luyaz-sentinel luyaz-web; do
    printf '#!/bin/sh /etc/rc.common\n# NEW %s\n' "$f" >"$R/data/luyaz/ship/stage/$TSC/$f"
    _ml="$_ml$NL""file=$f $(md5 "$R/data/luyaz/ship/stage/$TSC/$f")"
done
meta "$TSC" scripts "$_ml"
stage $TSC
check "$CASE: staged as v=5 (it brings the sentinel's, luyaz-web's and luyaz-core's inits)" '[ "$RC" = 0 ] && [ "$(txnv)" = 5 ]'
run $TSC
check "$CASE: done, the sentinel's, luyaz-web's and the core's inits are new" '[ "$(phase)" = done ] && grep -q "NEW luyaz-web" "$R/etc/init.d/luyaz-web" && grep -q "NEW luyaz-sentinel" "$R/etc/init.d/luyaz-sentinel" && grep -q "NEW luyaz-core" "$R/etc/init.d/luyaz-core" && grep -q "NEW doctor.sh" "$R/data/luyaz/scripts/doctor.sh"'
check "$CASE: the scripts went to /data/luyaz/scripts, a file not in the list stays" '[ "$(ls "$R/data/luyaz/scripts" | wc -l)" = 11 ] && grep -q "NEW luyaz-fallback.sh" "$R/data/luyaz/scripts/luyaz-fallback.sh" && grep -q "OLD other.sh" "$R/data/luyaz/scripts/other.sh" && grep -q "/data/luyaz/scripts/doctor.sh" "$R/data/luyaz/ship/txn"'
check "$CASE: an init not in the list untouched" 'grep -q "OLD init" "$R/etc/init.d/other" && ! grep -q "init.d/other" "$R/data/luyaz/ship/txn"'
recover_newboot
check "$CASE: a quiet next boot" '[ "$RRC" = 0 ]'
teardown

# a stage that carries a file not in the list is refused
setup
CASE="ship scripts: an upload with a file not in the list"
TSC=20261010-150000-scripts
mkdir -p "$R/data/luyaz/ship/stage/$TSC"
_ml=
for f in alert-lib.sh web-auth.sh chaos.sh doctor.sh config-backup.sh wan-sources.sh luyaz-fallback.sh other.init other luyaz-sentinel luyaz-web; do
    printf '#!/bin/sh\n# NEW %s luyaz-write.lock\n' "$f" >"$R/data/luyaz/ship/stage/$TSC/$f"
    _ml="$_ml${_ml:+$NL}file=$f $(md5 "$R/data/luyaz/ship/stage/$TSC/$f")"
done
meta "$TSC" scripts "$_ml"
stage $TSC
check "$CASE: refused" '[ "$RC" = 1 ] && grep -q "other" "$T/out" && [ ! -e "$R/data/luyaz/ship/txn" ]'
teardown

# ═══ lweb (luyaz-web, manager docs/designs/u60-web.md step 6) ══════════════
TW=20261012-120000-lweb
LWB=
lw_setup() { # lw_setup [old]: with "old", an old luyaz-web installed and running
    setup
    LWB=$R/data/luyaz/luyaz-web
    mkdir -p "$R/data/luyaz" "$T/proc/net"
    printf '#!/bin/sh /etc/rc.common\n# luyaz-web init\n' >"$R/etc/init.d/luyaz-web"
    : >"$T/lweb-initd.log"
    # /proc/net/tcp: the live one listens on 0.0.0.0:9090; the test build too, or
    # on 127.0.0.1 only with $T/lw-test-loopback
    printf '  sl  local_address rem_address   st\n   0: 00000000:2382 00000000:0000 0A\n' >"$T/tcp-any"
    printf '  sl  local_address rem_address   st\n   0: 0100007F:2382 00000000:0000 0A\n' >"$T/tcp-lo"
    cat >"$T/bin/lweb-initd" <<EOF
#!/bin/sh
echo "\$1" >>$T/lweb-initd.log
# procd: start registers the instance (also when the program then fails), stop drops it
case "\$1" in
    stop) p=\$(cat $T/pids/luyaz-web 2>/dev/null); [ -n "\$p" ] && rm -rf $T/proc/\$p; rm -f $T/pids/luyaz-web $T/lw-registered ;;
    start)
        [ -s $T/pids/luyaz-web ] && exit 0
        [ -x $LWB ] || exit 1
        touch $T/lw-registered
        [ -f $T/lw-new-broken ] && grep -q NEW $LWB && exit 0
        echo 8800 >$T/pids/luyaz-web
        $T/bin/proc_new 8800 $LWB $LWB serve
        cp $T/tcp-any $T/proc/net/tcp
        ;;
esac
exit 0
EOF
    mv "$T/bin/curl" "$T/bin/curl.other"
    cat >"$T/bin/curl" <<EOF
#!/bin/sh
case "\$*" in *lweb.test:9090*) ;; *) exec $T/bin/curl.other "\$@" ;; esac
out=/dev/null
while [ \$# -gt 0 ]; do case "\$1" in -o) out=\$2; shift ;; esac; shift; done
if [ ! -s $T/pids/luyaz-web ] && [ ! -s $T/pids/luyaz-web.test ]; then printf 000; exit 7; fi
case "\$*" in
    */login*) printf '<html>login</html>' >"\$out" ;;
    *) printf '{"link":"idle","connects":0,"tabs":0}' >"\$out" ;;
esac
printf 200
EOF
    mv "$T/bin/setsid" "$T/bin/setsid.p2"
    cat >"$T/bin/setsid" <<EOF
#!/bin/sh
case "\$*" in
    *luyaz-web.test*)
        echo "\$*" >>$T/setsid.log
        [ -f $T/test-broken ] && exit 0
        echo 9300 >$T/pids/luyaz-web.test
        $T/bin/proc_new 9300 $LWB.test $LWB.test
        if [ -f $T/lw-test-loopback ]; then cp $T/tcp-lo $T/proc/net/tcp; else cp $T/tcp-any $T/proc/net/tcp; fi
        exit 0
        ;;
esac
exec $T/bin/setsid.p2 "\$@"
EOF
    mv "$T/bin/kill" "$T/bin/kill.p2"
    cat >"$T/bin/kill" <<EOF
#!/bin/sh
for p in "\$@"; do
    [ "\$p" = "\$(cat $T/pids/luyaz-web.test 2>/dev/null)" ] && { rm -f $T/pids/luyaz-web.test; rm -rf $T/proc/\$p; }
done
exec $T/bin/kill.p2 "\$@"
EOF
    chmod +x "$T"/bin/*
    export U60S_LWEB_INITD=$T/bin/lweb-initd U60S_LWEB_URL=http://lweb.test:9090
    if [ "$1" = old ]; then
        printf 'OLD-LWEB\n' >"$LWB"
        chmod 755 "$LWB"
        "$T/bin/lweb-initd" start
        : >"$T/lweb-initd.log"
    fi
    LW_OLD=$(md5 "$LWB")
}
upload_lweb() { # upload_lweb [txn]
    _t=${1:-$TW}
    mkdir -p "$R/data/luyaz/ship/stage/$_t"
    printf 'NEW-LWEB %s\n' "$_t" >"$R/data/luyaz/ship/stage/$_t/luyaz-web"
    LW_NEW=$(md5 "$R/data/luyaz/ship/stage/$_t/luyaz-web")
    meta "$_t" lweb "trial=60${NL}file=luyaz-web $LW_NEW"
}
lw_teardown() {
    unset U60S_LWEB_INITD U60S_LWEB_URL
    teardown
}

lw_setup
CASE="ship lweb: the first one"
upload_lweb
stage $TW
check "$CASE: staged as v=2 (a program that did not exist)" '[ "$RC" = 0 ] && [ "$(txnv)" = 2 ]'
run $TW
check "$CASE: done, the new program runs at 8800" '[ "$(phase)" = done ] && [ "$(md5 "$LWB")" = "$LW_NEW" ] && [ "$(cat "$T/pids/luyaz-web")" = 8800 ] && [ ! -e "$LWB.test" ]'
check "$CASE: the trial started like the init script, without supervise" "grep -qx 'nohup $LWB.test serve --bind 0.0.0.0:9090 --upstream http://127.0.0.1:9460 --password-file /data/luyaz/web.env' '$T/setsid.log'"
check "$CASE: init stop (nothing to stop) for the trial, one start after promotion, test build gone" '[ "$(tr "\n" " " <"$T/lweb-initd.log")" = "stop start " ] && [ ! -s "$T/pids/luyaz-web.test" ]'
check "$CASE: manifest line" "grep -q '\"comp\":\"lweb\",\"txn\":\"$TW\"' '$U60S_MANIFEST'"
recover_newboot
check "$CASE: a quiet next boot" '[ "$RRC" = 0 ] && [ "$(md5 "$LWB")" = "$LW_NEW" ]'
lw_teardown

lw_setup old
CASE="ship lweb: an upgrade"
upload_lweb
stage $TW
run $TW
check "$CASE: done, old kept as .prev, the new one runs" '[ "$(phase)" = done ] && [ "$(md5 "$LWB.prev-$TW")" = "$LW_OLD" ] && [ "$(md5sum <"$T/proc/8800/exe" | cut -d" " -f1)" = "$LW_NEW" ]'
check "$CASE: stop for the trial, start after promotion" '[ "$(tr "\n" " " <"$T/lweb-initd.log")" = "stop start " ]'
lw_teardown

lw_setup
CASE="lweb: the first one, the test build never answers"
upload_lweb
stage $TW
touch "$T/test-broken"
run $TW
check "$CASE: aborted, no program left, the init never started" '[ "$(phase)" = aborted ] && reason | grep -q "测试版没起来" && [ ! -e "$LWB" ] && [ ! -e "$LWB.test" ] && ! grep -q start "$T/lweb-initd.log" && [ ! -f "$T/lw-registered" ]'
lw_teardown

lw_setup old
CASE="lweb: listens on 127.0.0.1 only"
touch "$T/lw-test-loopback"
upload_lweb
stage $TW
run $TW
check "$CASE: aborted, says why, the old one runs again" '[ "$(phase)" = aborted ] && reason | grep -q "0.0.0.0:9090" && [ "$(md5 "$LWB")" = "$LW_OLD" ] && [ -s "$T/pids/luyaz-web" ]'
lw_teardown

lw_setup old
CASE="lweb: restarted during the check"
upload_lweb
stage $TW
hook_at 1100 "echo 8801 >$T/pids/luyaz-web; $T/bin/proc_new 8801 $LWB $LWB"
run $TW
check "$CASE: rolled back, the old one runs" '[ "$(phase)" = rolledback ] && reason | grep -q "被重启过" && [ "$(md5 "$LWB")" = "$LW_OLD" ] && [ -s "$T/pids/luyaz-web" ]'
lw_teardown

lw_setup
CASE="lweb: the first one does not come up after promotion"
upload_lweb
stage $TW
touch "$T/lw-new-broken"
run $TW
check "$CASE: rolled back to nothing, not failed, procd's instance stopped" '[ "$(phase)" = rolledback ] && [ ! -e "$LWB" ] && [ ! -s "$T/pids/luyaz-web" ] && [ ! -f "$T/lw-registered" ]'
lw_teardown

lw_setup old
CASE="lweb: rollback after an upgrade"
upload_lweb
stage $TW
run $TW
sh "$SHIP" prepare-rollback lweb 20261012-130000-lweb 1790000100 >"$T/out" 2>&1
stage 20261012-130000-lweb
run 20261012-130000-lweb
check "$CASE: back to the old program, by an ordinary transaction" '[ "$(phase)" = done ] && [ "$(md5 "$LWB")" = "$LW_OLD" ] && [ "$(md5sum <"$T/proc/8800/exe" | cut -d" " -f1)" = "$LW_OLD" ]'
lw_teardown

lw_setup
CASE="lweb: rollback after the first ship"
upload_lweb
stage $TW
run $TW
sh "$SHIP" prepare-rollback lweb 20261012-130000-lweb 1790000100 >"$T/out" 2>&1
RC=$?
# ship never deletes a program: the "version before" is the one there now (C_ALLOW_NEW)
check "$CASE: staged as the program it has now (ship deletes no program)" '[ "$RC" = 0 ] && logged "保持现在的版本"'
stage 20261012-130000-lweb
run 20261012-130000-lweb
check "$CASE: a no-op transaction, the program stays and runs" '[ "$(phase)" = done ] && [ "$(md5 "$LWB")" = "$LW_NEW" ] && [ -s "$T/pids/luyaz-web" ]'
lw_teardown

lw_setup
CASE="lweb: no init yet"
rm "$R/etc/init.d/luyaz-web"
upload_lweb
stage $TW
check "$CASE: refused, ship scripts first" '[ "$RC" = 1 ] && grep -q "先 u60 ship scripts" "$T/out"'
lw_teardown

# drift: the trial must start what /etc/init.d/luyaz-web's start_service starts
CASE="lweb drift"
(
    unset U60S_ROOT
    INIT=$SCRIPTS/luyaz-web.init
    eval "$(grep -E '^BIN=' "$INIT")"
    WANT_ARGS=$(sed -n 's/^[[:space:]]*procd_set_param command .*"\$BIN"[[:space:]]*//p' "$INIT")
    if grep -q '^[[:space:]]*procd_set_param env' "$INIT"; then
        echo "  FAIL lweb drift: the init sets env, the trial does not"
        exit 1
    fi
    WANT="nohup $BIN.test $WANT_ARGS"
    GOT=$(sh "$SHIP" print-launch lweb)
    if [ -n "$WANT_ARGS" ] && [ "$GOT" = "$WANT" ]; then
        echo "  ok   lweb drift: trial launch = init start_service (binary, arguments)"
    else
        echo "  FAIL lweb drift: init: $WANT"
        echo "                   trial: $GOT"
        exit 1
    fi
) && PASS=$((PASS + 1)) || FAIL=$((FAIL + 1))

# ═══ scripts without a luyaz-recover.sh that reads v=4 ════════════════════════
setup
CASE="ship scripts: a luyaz-recover.sh without formats 4"
TSC=20261012-140000-scripts
mkdir -p "$R/data/luyaz/ship/stage/$TSC"
_ml=
for f in alert-lib.sh web-auth.sh chaos.sh doctor.sh config-backup.sh wan-sources.sh rpc-acl.sh luyaz-fallback.sh net-tune.sh VERSION luyaz-core luyaz-sentinel luyaz-web; do
    printf '#!/bin/sh\n# NEW %s luyaz-write.lock\n' "$f" >"$R/data/luyaz/ship/stage/$TSC/$f"
    _ml="$_ml${_ml:+$NL}file=$f $(md5 "$R/data/luyaz/ship/stage/$TSC/$f")"
done
meta "$TSC" scripts "$_ml"
sed 's/formats) echo "1 2 3 4 5" ;;/formats) echo "1 2 3" ;;/' "$RECOVER" >"$R/data/luyaz/ship/luyaz-recover.sh"
stage $TSC
check "$CASE: refused, install-recover first" '[ "$RC" = 1 ] && grep -q "v=4" "$T/out" && grep -q "install-recover" "$T/out"'
teardown

# ═══ scripts without a luyaz-recover.sh that reads v=5 (10e: luyaz-core's init) ═══
setup
CASE="ship scripts: a luyaz-recover.sh without formats 5"
TSC=20261014-140000-scripts
mkdir -p "$R/data/luyaz/ship/stage/$TSC"
_ml=
for f in alert-lib.sh web-auth.sh chaos.sh doctor.sh config-backup.sh wan-sources.sh rpc-acl.sh luyaz-fallback.sh net-tune.sh VERSION luyaz-core luyaz-sentinel luyaz-web; do
    printf '#!/bin/sh\n# NEW %s luyaz-write.lock\n' "$f" >"$R/data/luyaz/ship/stage/$TSC/$f"
    _ml="$_ml${_ml:+$NL}file=$f $(md5 "$R/data/luyaz/ship/stage/$TSC/$f")"
done
meta "$TSC" scripts "$_ml"
sed 's/formats) echo "1 2 3 4 5" ;;/formats) echo "1 2 3 4" ;;/' "$RECOVER" >"$R/data/luyaz/ship/luyaz-recover.sh"
stage $TSC
check "$CASE: refused, install-recover first" '[ "$RC" = 1 ] && grep -q "v=5" "$T/out" && grep -q "install-recover" "$T/out"'
teardown


# 10-25: c622e515 dropped UID_UP, so sent_release's wait was 0 s and a UI one second late
# failed the touch ship (u60g, twice). Every name the waits multiply must be assigned.
for v in UID_UP HOLD_MAX; do
    check "wait constant $v is assigned" 'grep -q "^$v=" "$SHIP"'
done

rm -f "$OLDREC"
echo "u60-ship phase two: $PASS passed, $FAIL failed"
[ "$FAIL" = 0 ]
