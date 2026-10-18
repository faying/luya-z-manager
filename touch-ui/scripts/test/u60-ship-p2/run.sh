#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# Tests for the phase-two components of u60-ship.sh (docs/SHIP.md「第二期」):
# touch (T12), uid-restart (R8), install-recover (uid, guard: removed in 10c),
# the v=2 transaction log, and what u60-recover.sh (new and
# the phase-one copy on the device) makes of every power cut in between.
# Every device command is stubbed, every device path lives under $T; after
# each case the container's real /data, /tmp and /etc/init.d are untouched.
#
#   docker run --rm -v "$PWD/scripts":/scripts:ro u60-device-busybox:20260928 \
#       sh /scripts/test/u60-ship-p2/run.sh
#
# Time is fake: $T/uptime is /proc/uptime; the stub `sleep` advances it, then
# runs $T/hook. /proc is $T/proc: <pid>/exe is a copy of the program at the
# moment the stub started it (like the kernel's view of a running binary),
# <pid>/cmdline as the kernel writes it.
# SPDX-License-Identifier: MIT
# ─────────────────────────────────────────────────────────────────────────────

SCRIPTS=${SCRIPTS:-/scripts}
SHIP=$SCRIPTS/u60-ship.sh
RECOVER=$SCRIPTS/u60-recover.sh
OLDREC=$SCRIPTS/test/fixtures/u60-recover.v1-2bb17b1.sh
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

leak_snapshot() { ls -la /data /tmp/luyaz/ship /tmp/u60-guard /tmp/luyaz-screen.log /etc/init.d 2>&1 | md5sum; }
LEAK0=$(leak_snapshot)

setup() {
    T=$(mktemp -d)
    R=$T/root
    DV=$R/data/plugins/u60pro-devui
    UD=$R/data/luyaz/ui
    GD=$R/data/u60-guard
    mkdir -p "$T/bin" "$T/pids" "$T/tmp" "$T/proc" "$DV" "$UD" "$GD" "$R/data/luyaz/ship/stage" "$R/etc/init.d" "$R/data/u60-uid" "$R/data/luyaz/screen"
    # the screen state already moved (R4; E3 refuses a device where it did not)
    : >"$R/data/luyaz/screen/.migrated"
    # …and the core (the zwrt-datad layout is refused since E3)
    mkdir -p "$R/data/luyaz/core" && : >"$R/data/luyaz/core/.migrated"
    echo 1000 >"$T/uptime"
    echo aaaa-1111 >"$T/boot_id"
    : >"$T/uidlog"
    : >"$T/kill.log"
    : >"$T/setsid.log"
    : >"$T/uid-initd.log"
    : >"$T/guard-initd.log"
    printf ':\n' >"$T/hook"
    cp "$RECOVER" "$R/data/luyaz/ship/luyaz-recover.sh"
    : >"$R/data/luyaz/ship/.migrated"
    # the touch UI's direct-write fallback (u60-ship.sh: touch waits for it)
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
done
exit 0
EOF
    # u60-uid under procd: stop leaves the UI on screen; start adopts a
    # running UI or launches one, counting launches like src/uid.c
    # (UID_MAX_ATTEMPTS 2: the third unsteady launch gives up)
    cat >"$T/bin/uid-initd" <<EOF
#!/bin/sh
echo "\$1 \$(cut -d. -f1 $T/uptime)" >>$T/uid-initd.log
case "\$1" in
    stop) rm -f $T/pids/u60-uid ;;
    start)
        [ -s $T/pids/u60-uid ] && exit 0
        # 9-26: u60-uid came up and was gone a second later
        [ -f $T/uid-dies-once ] && { rm -f $T/uid-dies-once; exit 0; }
        [ -f $T/uid-broken ] && grep -q NEW $DV/u60-uid && exit 0
        echo 8100 >$T/pids/u60-uid
        $T/bin/proc_new 8100 $DV/u60-uid $DV/u60-uid
        [ -s $T/pids/luyaz-ui ] && { echo "adopted running luyaz-ui pid \$(cat $T/pids/luyaz-ui)" >>$T/uidlog; exit 0; }
        [ -f $R/data/u60-uid/gave-up ] && exit 0
        a=\$(cat $R/data/u60-uid/attempts 2>/dev/null); a=\$(( \${a:-0} + 1 ))
        echo \$a >$R/data/u60-uid/attempts
        if [ \$a -gt 2 ]; then
            echo "giving up: \$a launches did not stay up; vendor UI on screen" >>$T/uidlog
            echo 1 >$R/data/u60-uid/gave-up
            exit 0
        fi
        un=luyaz-ui; ub=$UD/luyaz-ui
        if [ -f $T/ui-broken ] && grep -q NEW \$ub; then
            echo "luyaz-ui pid 9999 ended: exit 1" >>$T/uidlog
            exit 0
        fi
        n=\$(( \$(cat $T/nextpid 2>/dev/null || echo 8200) + 1 )); echo \$n >$T/nextpid
        echo \$n >$T/pids/\$un
        $T/bin/proc_new \$n \$ub \$ub
        echo "launched \$un pid \$n (attempt \$a of 2 before giving up)" >>$T/uidlog
        echo "ui \$n \$(cut -d. -f1 $T/uptime)" >>$T/events
        ;;
esac
exit 0
EOF
    cat >"$T/bin/guard-initd" <<EOF
#!/bin/sh
echo "\$1 \$(cut -d. -f1 $T/uptime)" >>$T/guard-initd.log
case "\$1" in
    stop) rm -rf $T/proc/7300; rm -f $T/pids/guard ;;
    start)
        [ -d $T/proc/7300 ] && exit 0
        grep -q BROKEN $GD/u60-guard.sh && exit 0
        $T/bin/proc_new 7300 /bin/sh /bin/sh $GD/u60-guard.sh
        echo 7300 >$T/pids/guard
        ;;
esac
exit 0
EOF
    # procd's record of the guard instance (only the main loop's pid)
    cat >"$T/bin/ubus" <<EOF
#!/bin/sh
echo "\$*" >>$T/ubus.log
if [ -d $T/proc/7300 ]; then
    printf '{\n\t"u60-guard": {\n\t\t"instances": {\n\t\t\t"instance1": {\n\t\t\t\t"running": true,\n\t\t\t\t"pid": 7300,\n\t\t\t\t"command": [ "/bin/sh" ]\n\t\t\t}\n\t\t}\n\t}\n}\n'
else
    echo '{ }'
fi
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
    *u60-ship.sh\ run\ *) "\$@" ;;
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
    export U60S_MANIFEST=$R/data/luyaz/ship/manifest.jsonl U60S_UID_INITD=$T/bin/uid-initd U60S_GUARD_INITD=$T/bin/guard-initd
    export U60S_UBUS=$T/bin/ubus
    export U60S_GUARD_STOPREQ=$T/guardtmp/stop-requested U60S_KMSG=$T/kmsg U60S_TO_DOCTOR=3
    unset U60S_CRASH_AT U60S_DIR U60S_LOG U60S_TEST_LOG U60S_MIN_FREE_KB U60S_TO_INITD U60S_UID_STATE
    export DT_UPTIME=$T/uptime DT_PIDFILE=$T/trial.pid DT_CURL=$T/bin/curl DT_PIDOF=$T/bin/pidof
    unset DT_LOGREAD # the default reader: <log>.old, then <log>
    export DT_UID_LOG=$T/uidlog DT_KILL=$T/bin/kill DT_SLEEP=$T/bin/sleep DT_DATE=$T/bin/date DT_SETSID=$T/bin/setsid
    export DT_RC=$T/rc.local DT_INITD=$T/bin/none
    export U60R_ROOT=$R
    unset U60R_DIR U60R_BOOT_ID

    # the screen as it is before every case: old u60-uid running, old UI on it
    # (R5 round ③: luyaz-ui is the only touch UI; the old program is deleted)
    printf 'OLD-UI /data/luyaz/scripts\n' >"$UD/luyaz-ui"
    printf '#!/bin/sh\n# OLD start.sh\nexit 0\n' >"$DV/start.sh"
    printf 'OLD-UID\n' >"$DV/u60-uid"
    # a legacy install has both (legacy_here); 10c cases delete them
    mkdir -p "$GD" && printf '#!/bin/sh\nexit 0\n' >"$GD/u60-guard.sh"
    chmod 755 "$UD/luyaz-ui" "$DV/start.sh" "$DV/u60-uid"
    UI_OLD=$(md5 "$UD/luyaz-ui") SS_OLD=$(md5 "$DV/start.sh") UID_OLD=$(md5 "$DV/u60-uid")
    echo 8100 >"$T/pids/u60-uid"
    "$T/bin/proc_new" 8100 "$DV/u60-uid" "$DV/u60-uid"
    echo 8200 >"$T/pids/luyaz-ui"
    echo 8200 >"$T/nextpid"
    "$T/bin/proc_new" 8200 "$UD/luyaz-ui" "$UD/luyaz-ui"
    echo 1 >"$R/data/u60-uid/attempts"
}

teardown() {
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
uid_md5() { md5sum <"$T/proc/$(cat "$T/pids/u60-uid" 2>/dev/null)/exe" 2>/dev/null | cut -d' ' -f1; }
recover_newboot() { # recover_newboot [old]: rc.local's line after a power cut
    echo bbbb-2222 >"$T/newboot"
    if [ "$1" = old ]; then
        # (the v1 one only knows /data/u60-ship by itself)
        U60R_BOOT_ID=$T/newboot U60R_DIR=$R/data/luyaz/ship sh "$OLDREC" >"$T/rout" 2>&1
    else
        U60R_BOOT_ID=$T/newboot sh "$R/data/luyaz/ship/luyaz-recover.sh" >"$T/rout" 2>&1
    fi
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
    printf '%s\n' "${2:-NEW-UI $_t /data/luyaz/scripts}" >"$R/data/luyaz/ship/stage/$_t/luyaz-ui"
    printf '#!/bin/sh\n# NEW start.sh %s\nexit 0\n' "$_t" >"$R/data/luyaz/ship/stage/$_t/start.sh"
    UI_NEW=$(md5 "$R/data/luyaz/ship/stage/$_t/luyaz-ui") SS_NEW=$(md5 "$R/data/luyaz/ship/stage/$_t/start.sh")
    meta "$_t" touch "file=luyaz-ui $UI_NEW${NL}file=start.sh $SS_NEW"
}
NL='
'

setup
CASE="ship touch"
upload_touch
stage $TT
check "$CASE: staged as v=1 (the phase-one u60-recover.sh reads it)" '[ "$RC" = 0 ] && [ "$(phase)" = staged ] && [ "$(txnv)" = 1 ]'
check "$CASE: both side files in place" '[ "$(md5 "$UD/luyaz-ui.test")" = "$UI_NEW" ] && [ -x "$UD/luyaz-ui.test" ] && [ "$(md5 "$DV/start.sh.test")" = "$SS_NEW" ]'
cat >"$T/hook" <<EOF
read -r hb _p _t ph <$T/tmp/heartbeat 2>/dev/null && echo "\$ph \$(( \$(cut -d. -f1 $T/uptime) - hb ))" >>$T/hb.trace
:
EOF
run $TT
check "$CASE: done" '[ "$RC" = 0 ] && [ "$(phase)" = done ]'
check "$CASE: live files are the new ones, previous kept" '[ "$(md5 "$UD/luyaz-ui")" = "$UI_NEW" ] && [ "$(md5 "$DV/start.sh")" = "$SS_NEW" ] && [ "$(md5 "$UD/luyaz-ui.prev-$TT")" = "$UI_OLD" ] && [ "$(md5 "$DV/start.sh.prev-$TT")" = "$SS_OLD" ]'
check "$CASE: the test build ran from the UI directory, as the sentinel runs the UI" "grep -qx 'cwd=$UD bin=$UD/luyaz-ui.test' '$T/launch.log' && grep -q 'nohup sh -c cd \"\$1\" && exec \"\$2\" sh $UD $UD/luyaz-ui.test' '$T/setsid.log'"
check "$CASE: u60-uid stopped for the trial, started once after promotion" '[ "$(cut -d" " -f1 "$T/uid-initd.log" | tr "\n" " ")" = "stop start " ]'
check "$CASE: the old UI was killed by pid, the test build after the trial" "grep -qx '8200' '$T/kill.log' && grep -qx '9100' '$T/kill.log'"
check "$CASE: the new UI runs (exe md5), u60-uid runs" '[ "$(ui_md5)" = "$UI_NEW" ] && [ "$(uid_md5)" = "$UID_OLD" ] && [ ! -s "$T/pids/luyaz-ui.test" ]'
check "$CASE: launch count cleared before the start (1 = this launch)" '[ "$(cat "$R/data/u60-uid/attempts")" = 1 ] && [ ! -e "$R/data/u60-uid/gave-up" ]'
check "$CASE: trial heartbeat every second" '[ "$(grep -c "^trial " "$T/hb.trace")" -ge 60 ] && [ -z "$(awk "\$1 == \"trial\" && \$2 > 1" "$T/hb.trace")" ]'
check "$CASE: manifest line with both files" "grep -q '\"comp\":\"touch\",\"txn\":\"$TT\"' '$U60S_MANIFEST' && grep -q '\"path\":\"$DV/start.sh\",\"md5\":\"$SS_NEW\"' '$U60S_MANIFEST'"
check "$CASE: no side file left" '[ ! -e "$UD/luyaz-ui.test" ] && [ ! -e "$DV/start.sh.test" ]'
teardown

# R5 (round ②): the rollback floor, and the first ship after the rename
setup
CASE="touch rollback after an ordinary ship"
upload_touch
stage $TT
run $TT
sh "$SHIP" prepare-rollback touch 20261001-130000-touch 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: prepared, luyaz-ui and start.sh from the .prev" '[ "$RC" = 0 ] && [ "$(md5 "$R/data/luyaz/ship/stage/20261001-130000-touch/luyaz-ui")" = "$UI_OLD" ] && [ "$(md5 "$R/data/luyaz/ship/stage/20261001-130000-touch/start.sh")" = "$SS_OLD" ] && grep -qx "file=luyaz-ui $UI_OLD" "$R/data/luyaz/ship/stage/20261001-130000-touch/meta"'
teardown

setup
CASE="touch rollback: the last ship first put luyaz-ui (no prev)"
printf '{"kind":"ship","comp":"touch","txn":"20261001-120000-touch","commit":"7e8d223","format":1,"files":[{"path":"%s","md5":"%s"},{"path":"%s","md5":"%s","prev":"%s"}]}\n' \
    "$UD/luyaz-ui" "$UI_OLD" "$DV/start.sh" "$SS_OLD" "$DV/start.sh.prev-20261001-120000-touch" >"$U60S_MANIFEST"
cp "$DV/start.sh" "$DV/start.sh.prev-20261001-120000-touch"
sh "$SHIP" prepare-rollback touch 20261001-130000-touch 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: refused (the rename floor; start.sh's prev does not count)" '[ "$RC" != 0 ] && grep -q "改名" "$T/out" && [ ! -e "$R/data/luyaz/ship/stage/20261001-130000-touch" ]'
teardown

setup
CASE="touch rollback: the last ship is from before the rename"
printf '{"kind":"ship","comp":"touch","txn":"20260930-120000-touch","commit":"7e8d223","format":1,"files":[{"path":"%s","md5":"%s","prev":"%s"},{"path":"%s","md5":"%s","prev":"%s"}]}\n' \
    "$DV/u60pro-devui" "$UI_OLD" "$DV/u60pro-devui.prev-20260930-120000-touch" "$DV/start.sh" "$SS_OLD" "$DV/start.sh.prev-20260930-120000-touch" >"$U60S_MANIFEST"
cp "$UD/luyaz-ui" "$DV/u60pro-devui.prev-20260930-120000-touch"
cp "$DV/start.sh" "$DV/start.sh.prev-20260930-120000-touch"
sh "$SHIP" prepare-rollback touch 20261001-130000-touch 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: refused, says 改名" '[ "$RC" != 0 ] && grep -q "改名" "$T/out" && [ ! -e "$R/data/luyaz/ship/stage/20261001-130000-touch" ]'
teardown

# Luya-Z rename D1 floor: /data/u60-guard's copies are gone, so no core or UI from before D1
# (unconditional since the de-old: an old copy left there does not lift it)
d1_clean() { mkdir -p "$R/data/luyaz/scripts"; echo new >"$R/data/luyaz/scripts/doctor.sh"; echo old >"$GD/doctor.sh"; }
setup
CASE="D1 floor: touch rollback to a UI that does not know /data/luyaz/scripts"
upload_touch
stage $TT
run $TT
echo "OLD-UI" >"$UD/luyaz-ui.prev-$TT"
d1_clean
sh "$SHIP" prepare-rollback touch 20261001-130000-touch 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: old copies gone: refused, says D1, nothing prepared" '[ "$RC" != 0 ] && grep -q "D1 之前" "$T/out" && [ ! -e "$R/data/luyaz/ship/stage/20261001-130000-touch" ]'
echo "OLD-UI /data/luyaz/scripts/luyaz-fallback.sh" >"$UD/luyaz-ui.prev-$TT"
sh "$SHIP" prepare-rollback touch 20261001-130000-touch 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: a prev that knows it: prepared" '[ "$RC" = 0 ] && grep -q "^OLD-UI /data/luyaz/scripts" "$R/data/luyaz/ship/stage/20261001-130000-touch/luyaz-ui"'
teardown

setup
CASE="D1 floor: core rollback"
CB=$R/data/luyaz/luyaz-core
mkdir -p "${CB%/*}"
echo "NEW-CORE /data/luyaz/scripts" >"$CB"
echo "OLD-CORE /data/u60-guard" >"$CB.prev-20261015-111557-datad"
printf '{"kind":"ship","comp":"datad","txn":"20261015-111557-datad","commit":"7e8d223","format":1,"files":[{"path":"%s","md5":"%s","prev":"%s"}]}\n' \
    "$CB" "$(md5 "$CB")" "$CB.prev-20261015-111557-datad" >"$U60S_MANIFEST"
d1_clean
sh "$SHIP" prepare-rollback datad 20261015-130000-datad 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: a prev from before D1: refused, names LAN IPv6" '[ "$RC" != 0 ] && grep -q "D1 之前" "$T/out" && grep -q "lan-ipv6-off" "$T/out" && [ ! -e "$R/data/luyaz/ship/stage/20261015-130000-datad" ]'
# D2: the prev knows the scripts but not the moved ops dir
echo "OLD-CORE /data/luyaz/scripts /tmp/luyaz-screen.log" >"$CB.prev-20261015-111557-datad"
sh "$SHIP" prepare-rollback datad 20261015-130000-datad 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: ops not moved: prepared" '[ "$RC" = 0 ]'
rm -rf "$R/data/luyaz/ship/stage/20261015-130000-datad"
mkdir -p "$R/data/luyaz/core/ops"
sh "$SHIP" prepare-rollback datad 20261015-130000-datad 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: ops moved, a prev from before D2: refused" '[ "$RC" != 0 ] && grep -q "D2 之前" "$T/out" && [ ! -e "$R/data/luyaz/ship/stage/20261015-130000-datad" ]'
echo "OLD-CORE /data/luyaz/scripts /data/luyaz/core/ops /data/luyaz/ship/manifest.jsonl /tmp/luyaz-screen.log" >"$CB.prev-20261015-111557-datad"
sh "$SHIP" prepare-rollback datad 20261015-130000-datad 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: ops moved, a prev that knows it: prepared" '[ "$RC" = 0 ]'
# D3: once datad.env is on the LUYAZ_ names, the prev must carry the D3 build marker
rm -rf "$R/data/luyaz/ship/stage/20261015-130000-datad"
mkdir -p "$R/data/luyaz/core" && printf 'LUYAZ_HOME_EXIT=1\n' >"$R/data/luyaz/core/datad.env"  # the one tree since E3 (no .migrated needed)
sh "$SHIP" prepare-rollback datad 20261015-130000-datad 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: datad.env renamed, a prev from before D3: refused" '[ "$RC" != 0 ] && grep -q "D3 之前" "$T/out" && [ ! -e "$R/data/luyaz/ship/stage/20261015-130000-datad" ]'
echo "OLD-CORE /data/luyaz/scripts /data/luyaz/core/ops LUYAZ_CORE_BUILD_SELF_CONTAINED /data/luyaz/ship/manifest.jsonl /tmp/luyaz-screen.log" >"$CB.prev-20261015-111557-datad"
sh "$SHIP" prepare-rollback datad 20261015-130000-datad 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: datad.env renamed, a prev with the marker: prepared" '[ "$RC" = 0 ]'
teardown

setup
CASE="D1 floor: stage a touch build"
d1_clean
upload_touch $TT "NEW-UI without the mark"
stage $TT
check "$CASE: one from before D1: refused, nothing staged" '[ "$RC" = 1 ] && grep -q "D1 之前" "$T/out" && [ ! -s "$R/data/luyaz/ship/txn" ] && [ ! -e "$UD/luyaz-ui.test" ]'
rm -rf "$R/data/luyaz/ship/stage/$TT"
upload_touch $TT "NEW-UI /data/luyaz/scripts/luyaz-fallback.sh"
stage $TT
check "$CASE: one that knows /data/luyaz/scripts: staged" '[ "$RC" = 0 ] && [ "$(phase)" = staged ]'
teardown

setup
CASE="touch: test build crashes 5 s in"
upload_touch
stage $TT
hook_at 1005 "rm -f $T/pids/luyaz-ui.test; rm -rf $T/proc/9100; echo \"crash \$(cut -d. -f1 $T/uptime)\" >>$T/events"
run $TT
CRASH=$(sed -n 's/^crash //p' "$T/events")
START=$(awk '$1 == "start" { print $2; exit }' "$T/uid-initd.log")
check "$CASE: aborted, says why" '[ "$RC" = 1 ] && [ "$(phase)" = aborted ] && reason | grep -q "^试跑不过：测试版崩溃"'
check "$CASE: production started ≤ 1 s after the crash (crash $CRASH, u60-uid start $START)" '[ -n "$CRASH" ] && [ -n "$START" ] && [ $((START - CRASH)) -le 1 ]'
check "$CASE: the old UI runs again, u60-uid started once" '[ "$(ui_md5)" = "$UI_OLD" ] && [ "$(grep -c "^start" "$T/uid-initd.log")" = 1 ] && [ -s "$T/pids/u60-uid" ]'
check "$CASE: live untouched, no .prev, no side files" '[ "$(md5 "$UD/luyaz-ui")" = "$UI_OLD" ] && [ ! -e "$UD/luyaz-ui.prev-$TT" ] && [ ! -e "$UD/luyaz-ui.test" ] && [ ! -e "$DV/start.sh.test" ]'
check "$CASE: manifest untouched" '[ "$(mlines)" = 0 ]'
teardown

setup
CASE="touch: test build never shows (screen dark > 20 s)"
touch "$T/test-broken"
upload_touch
stage $TT
run $TT
KILL_AT=$(awk '$1 == "kill" && $2 == "luyaz-ui" { print $3; exit }' "$T/events")
START=$(awk '$1 == "start" { print $2; exit }' "$T/uid-initd.log")
check "$CASE: aborted after 20 s of an empty screen" '[ "$(phase)" = aborted ] && reason | grep -q "屏幕空了 20s"'
check "$CASE: production back ≤ 21 s after the old UI went (gone $KILL_AT, start $START)" '[ $((START - KILL_AT)) -le 21 ] && [ "$(ui_md5)" = "$UI_OLD" ]'
teardown

setup
CASE="touch: three ships in 10 minutes"
for n in 1 2 3; do
    t=2026100${n}-120000-touch
    upload_touch "$t"
    stage "$t"
    run "$t"
    [ "$(phase)" = done ] && ok "$CASE: ship $n done" || bad "$CASE: ship $n: $(phase) $(reason)"
done
check "$CASE: all within 10 minutes of fake time ($(($(cat "$T/uptime") - 1000)) s)" '[ $(($(cat "$T/uptime") - 1000)) -lt 600 ]'
check "$CASE: u60-uid never gave up, count back to 1 each time" '! grep -q "^giving up" "$T/uidlog" && [ ! -e "$R/data/u60-uid/gave-up" ] && [ "$(cat "$R/data/u60-uid/attempts")" = 1 ]'
check "$CASE: the third build runs" '[ "$(ui_md5)" = "$UI_NEW" ]'
# the stub counts like u60-uid: without the clearing, the third launch gives up
rm -f "$T/pids/luyaz-ui" "$T/pids/u60-uid"
"$T/bin/uid-initd" start; rm -f "$T/pids/luyaz-ui" "$T/pids/u60-uid"
"$T/bin/uid-initd" start; rm -f "$T/pids/luyaz-ui" "$T/pids/u60-uid"
"$T/bin/uid-initd" start
check "$CASE: (control) the stub gives up on its third uncleared launch" 'grep -q "^giving up" "$T/uidlog"'
teardown

setup
CASE="touch: new UI restarted during the check"
upload_touch
stage $TT
cat >"$T/hook" <<EOF
if grep -qx phase=check $R/data/luyaz/ship/txn && [ ! -f $T/once ]; then
    touch $T/once; echo 8300 >$T/pids/luyaz-ui; $T/bin/proc_new 8300 $UD/luyaz-ui $UD/luyaz-ui
fi
:
EOF
run $TT
check "$CASE: rolled back, says why" '[ "$(phase)" = rolledback ] && reason | grep -q "^转正后检查不过：界面被重启过"'
check "$CASE: old files back, old UI on screen" '[ "$(md5 "$UD/luyaz-ui")" = "$UI_OLD" ] && [ "$(md5 "$DV/start.sh")" = "$SS_OLD" ] && [ "$(ui_md5)" = "$UI_OLD" ]'
teardown

setup
CASE="touch: u60-uid gives up during the check"
upload_touch
stage $TT
cat >"$T/hook" <<EOF
grep -qx phase=check $R/data/luyaz/ship/txn && [ ! -f $T/once ] && { touch $T/once; echo "giving up: 2 launches did not stay up" >>$T/uidlog; }
:
EOF
run $TT
check "$CASE: rolled back" '[ "$(phase)" = rolledback ] && reason | grep -q "界面异常（u60-uid：giving up"'
teardown

# u60-guard moves /tmp/u60-uid.log to .old at 64 KB (mv); u60-uid then
# starts a new file with its next line
setup
CASE="touch: u60-uid gives up just before its log is rotated"
upload_touch
stage $TT
cat >"$T/hook" <<EOF
grep -qx phase=check $R/data/luyaz/ship/txn && [ ! -f $T/once ] && { touch $T/once; echo "giving up: 2 launches did not stay up" >>$T/uidlog; mv -f $T/uidlog $T/uidlog.old; }
:
EOF
run $TT
check "$CASE: rolled back all the same" '[ "$(phase)" = rolledback ] && reason | grep -q "界面异常（u60-uid：giving up"'
teardown

setup
CASE="touch: the uid log rotates during the check, nothing bad in it"
echo "giving up: an old one, before this ship" >"$T/uidlog.old"
upload_touch
stage $TT
cat >"$T/hook" <<EOF
grep -qx phase=check $R/data/luyaz/ship/txn && [ ! -f $T/once ] && { touch $T/once; echo "launched u60pro-devui pid 1 (attempt 1 of 2 before giving up)" >>$T/uidlog; mv -f $T/uidlog $T/uidlog.old; }
:
EOF
run $TT
check "$CASE: done (an old .old replaced is not a new bad line)" '[ "$(phase)" = done ] && [ -f "$T/uidlog.old" ]'
teardown

setup
CASE="touch: u60-uid gives up just before a rotation that replaces an old .old with a bad line"
echo "giving up: an old one, before this ship" >"$T/uidlog.old"
upload_touch
stage $TT
cat >"$T/hook" <<EOF
grep -qx phase=check $R/data/luyaz/ship/txn && [ ! -f $T/once ] && { touch $T/once; echo "giving up: 2 launches did not stay up" >>$T/uidlog; mv -f $T/uidlog $T/uidlog.old; }
:
EOF
run $TT
check "$CASE: rolled back" '[ "$(phase)" = rolledback ] && reason | grep -q "界面异常（u60-uid：giving up: 2 launches"'
teardown

setup
CASE="touch: an old .old with a bad line deleted during the check, then rotated twice"
echo "giving up: an old one, before this ship" >"$T/uidlog.old"
echo "u60pro-devui pid 1 ended: exit 1 (an old one)" >>"$T/uidlog"
upload_touch
stage $TT
cat >"$T/hook" <<EOF
if grep -qx phase=check $R/data/luyaz/ship/txn; then
    if [ ! -f $T/once ]; then touch $T/once; rm -f $T/uidlog.old
    elif [ ! -f $T/twice ]; then touch $T/twice; mv -f $T/uidlog $T/uidlog.old; echo "launched u60pro-devui pid 1 (attempt 1 of 2 before giving up)" >>$T/uidlog
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
upload_touch "" "NEW-UI but it crashes /data/luyaz/scripts"
stage $TT
cat >"$T/hook" <<EOF
# the test build crashes too, but only after the trial window
:
EOF
run $TT
check "$CASE: rolled back (new version will not start)" '[ "$(phase)" = rolledback ] && reason | grep -q "^新版起不来：u60-uid 两次 start 都没起好"'
check "$CASE: old UI running at the end" '[ "$(md5 "$UD/luyaz-ui")" = "$UI_OLD" ] && [ "$(ui_md5)" = "$UI_OLD" ]'
teardown

setup
CASE="touch: test build unkillable"
touch "$T/unkillable"
upload_touch
stage $TT
run $TT
check "$CASE: failed; production NOT started next to it (two UIs = reboot)" '[ "$(phase)" = failed ] && ! grep -q "^start" "$T/uid-initd.log"'
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
check "$CASE: aborted, test build never launched, u60-uid started again" '[ "$(phase)" = aborted ] && reason | grep -q "正式界面 10s 内没有退出" && [ ! -s "$T/launch.log" ] && grep -q "^start" "$T/uid-initd.log"'
teardown

setup
CASE="touch: the test build is still running"
echo 9100 >"$T/pids/luyaz-ui.test"
upload_touch
stage $TT
check "$CASE: stage refused" '[ "$RC" = 1 ] && grep -q "测试版 luyaz-ui.test 还在跑" "$T/out" && [ ! -e "$R/data/luyaz/ship/txn" ]'
teardown

setup
CASE="touch: start.sh with a syntax error"
upload_touch
printf 'if then\n' >"$R/data/luyaz/ship/stage/$TT/start.sh"
sed -i "s/^file=start.sh .*/file=start.sh $(md5 "$R/data/luyaz/ship/stage/$TT/start.sh")/" "$R/data/luyaz/ship/stage/$TT/meta"
stage $TT
check "$CASE: stage refused" '[ "$RC" = 1 ] && grep -q "start.sh 有语法错误" "$T/out" && [ ! -e "$DV/start.sh.test" ]'
teardown

# power cuts: touch logs are v=1, so the new and the old recover agree
for p in trial:running promote:moved:luyaz-ui promote:moved:start.sh check:begin; do
    for which in new old; do
        setup
        CASE="touch: power cut at $p, $which u60-recover.sh"
        upload_touch
        stage $TT
        U60S_CRASH_AT=$p run $TT
        recover_newboot $which
        case "$p" in
            trial:*) check "$CASE: aborted, live old" '[ "$(phase)" = aborted ] && [ "$(md5 "$UD/luyaz-ui")" = "$UI_OLD" ]' ;;
            *) check "$CASE: rolled back, both files old" '[ "$(phase)" = rolledback ] && [ "$(md5 "$UD/luyaz-ui")" = "$UI_OLD" ] && [ "$(md5 "$DV/start.sh")" = "$SS_OLD" ]' ;;
        esac
        teardown
    done
done

setup
CASE="touch: recover-live after kill -9 in the check"
upload_touch
stage $TT
U60S_CRASH_AT=check:begin run $TT
: >"$T/uid-initd.log"
sh "$SHIP" recover-live >"$T/out" 2>&1
check "$CASE: rolled back, u60-uid restarted on the old UI" '[ "$(phase)" = rolledback ] && [ "$(md5 "$UD/luyaz-ui")" = "$UI_OLD" ] && grep -q "^start" "$T/uid-initd.log" && [ -s "$T/pids/u60-uid" ]'
teardown

setup
CASE="touch: recover-live after kill -9 in the trial"
upload_touch
stage $TT
U60S_CRASH_AT=trial:running run $TT
sh "$SHIP" recover-live >"$T/out" 2>&1
check "$CASE: aborted, test build killed, the old UI back" '[ "$(phase)" = aborted ] && [ ! -s "$T/pids/luyaz-ui.test" ] && [ "$(ui_md5)" = "$UI_OLD" ]'
teardown

# (the touch launch vs src/uid.c: scripts/test/cross-repo, on the host)

# pidof: the names do not see each other (real busybox pidof, real
# processes: comm luyaz-ui vs luyaz-ui.test vs the pre-rename u60pro-devui)
CASE="pidof names"
P=$(mktemp -d)
printf '#!/bin/sh\nwhile :; do %s 1; done\n' "$_real_sleep" >"$P/luyaz-ui"
cp "$P/luyaz-ui" "$P/luyaz-ui.test"
cp "$P/luyaz-ui" "$P/u60pro-devui"
chmod 755 "$P/luyaz-ui" "$P/luyaz-ui.test" "$P/u60pro-devui"
"$P/luyaz-ui" &
A=$!
"$P/luyaz-ui.test" &
B=$!
"$P/u60pro-devui" &
C=$!
"$_real_sleep" 0.3
check "$CASE: pidof luyaz-ui = the live one only" '[ "$(pidof luyaz-ui)" = "$A" ]'
check "$CASE: pidof luyaz-ui.test = the test build only" '[ "$(pidof luyaz-ui.test)" = "$B" ]'
check "$CASE: pidof u60pro-devui = the pre-rename one only" '[ "$(pidof u60pro-devui)" = "$C" ]'
check "$CASE: the test build's comm fits (13 < 15 characters)" '[ "$(cat /proc/$B/comm)" = luyaz-ui.test ]'
kill $A $B $C 2>/dev/null
wait $A $B $C 2>/dev/null
rm -rf "$P"

# ═══ uid-restart (R8; the install kit calls it too) ═════════════════════════
setup
CASE="uid-restart"
sh "$SHIP" uid-restart >"$T/out" 2>&1
RC=$?
check "$CASE: exit 0, stop then start, versions as on disk" '[ $RC = 0 ] && [ "$(cut -d" " -f1 "$T/uid-initd.log" | tr "\n" " ")" = "stop start " ] && grep -q "版本对" "$T/out"'
check "$CASE: launch count and give-up cleared" '[ ! -e "$R/data/u60-uid/attempts" ] && [ ! -e "$R/data/u60-uid/gave-up" ]'
check "$CASE: the UI on screen was adopted, not restarted" '[ "$(cat "$T/pids/luyaz-ui")" = 8200 ]'
sh "$SHIP" uid-restart "$UI_OLD" "$UID_OLD" >"$T/out" 2>&1
RC=$?
check "$CASE: explicit md5s: exit 0" '[ $RC = 0 ]'
: >"$T/uid-initd.log"
sh "$SHIP" uid-restart 0123456789abcdef0123456789abcdef >"$T/out" 2>&1
RC=$?
check "$CASE: another UI version wanted: start twice, exit 1, says why" '[ $RC = 1 ] && [ "$(grep -c "^start" "$T/uid-initd.log")" = 2 ] && grep -q "版本不对" "$T/out"'
sh "$SHIP" uid-restart nothex >"$T/out" 2>&1
RC=$?
check "$CASE: a bad md5 refused" '[ $RC = 1 ] && grep -q "不是 md5" "$T/out"'
rm -f "$T/pids/luyaz-ui" "$T/pids/u60-uid"
echo 5 >"$R/data/u60-uid/attempts"
echo 1 >"$R/data/u60-uid/gave-up"
sh "$SHIP" uid-restart >"$T/out" 2>&1
RC=$?
check "$CASE: after a give-up: cleared, UI launched" '[ $RC = 0 ] && [ -s "$T/pids/luyaz-ui" ] && [ "$(cat "$R/data/u60-uid/attempts")" = 1 ]'
printf 'v=1\ntxn=%s\ncomp=datad\nphase=check\nend=1\n' 20261001-110000-datad >"$R/data/luyaz/ship/txn"
sh "$SHIP" uid-restart >"$T/out" 2>&1
RC=$?
check "$CASE: refused while a transaction is in progress" '[ $RC = 1 ] && grep -q "还在进行" "$T/out"'
teardown

setup
CASE="uid-restart: u60-uid comes up but leaves again"
touch "$T/uid-dies-once"
rm -f "$T/pids/u60-uid"
sh "$SHIP" uid-restart >"$T/out" 2>&1
RC=$?
check "$CASE: started again, exit 0" '[ $RC = 0 ] && [ "$(grep -c "^start" "$T/uid-initd.log")" = 2 ] && [ -s "$T/pids/u60-uid" ]'
teardown

setup
CASE="uid-restart after 10c, no sentinel running"
rm -f "$DV/u60-uid" "$GD/u60-guard.sh"
: >"$T/uid-initd.log"
sh "$SHIP" uid-restart >"$T/out" 2>&1
RC=$?
check "$CASE: exit 1 at once, says the sentinel is down, the u60-uid init never run" '[ $RC = 1 ] && grep -q "哨兵没在跑" "$T/out" && [ ! -s "$T/uid-initd.log" ]'
teardown

# T14 web (zte-agent's /data/admin) was removed in 8d-2c with the agent; the
# admin pages are luyaz-web's (lweb below). Directory entries (dir=, v=2) stay
# readable for old logs: u60-recover's own test covers them.

# ═══ uid, guard: removed in Phase 10 10c (X6, X7) ═══════════════════════════
# the sentinel owns the screen and the watchdog; every way in is refused by name
for c in uid guard; do
    setup
    CASE="$c: removed (Phase 10 10c)"
    mkdir -p "$R/data/luyaz/ship/stage/20261001-120000-$c"
    meta 20261001-120000-$c $c
    stage 20261001-120000-$c
    check "$CASE: stage refused" '[ "$RC" = 1 ] && grep -q "已删除" "$T/out" && [ ! -s "$R/data/luyaz/ship/txn" ]'
    sh "$SHIP" record-kit $c 20261001 abc1234 1 1790000000 >"$T/out" 2>&1
    RC=$?
    check "$CASE: record-kit refused, nothing recorded" '[ "$RC" != 0 ] && grep -q "已删除" "$T/out" && [ "$(mlines)" = 0 ]'
    sh "$SHIP" prepare-rollback $c 20261001-130000-$c 1790000100 >"$T/out" 2>&1
    RC=$?
    check "$CASE: prepare-rollback refused" '[ "$RC" != 0 ] && grep -q "已删除" "$T/out" && [ ! -e "$R/data/luyaz/ship/stage/20261001-130000-$c" ]'
    teardown
done


setup
CASE="record-kit touch (the install kit's do_devui)"
sh "$SHIP" record-kit touch 20261001 abc1234 1 1790000000 >"$T/out" 2>&1
RC=$?
check "$CASE: touch: both files" "[ \$RC = 0 ] && grep -q '\"comp\":\"touch\".*\"files\":\[{\"path\":\"$UD/luyaz-ui\",\"md5\":\"$UI_OLD\"},{\"path\":\"$DV/start.sh\",\"md5\":\"$SS_OLD\"}\]' '$U60S_MANIFEST'"
teardown

# ═══ install-recover ════════════════════════════════════════════════════════
TR=20261001-120000-recover
upload_recover() { # upload_recover [file to use]
    mkdir -p "$R/data/luyaz/ship/stage/$TR"
    cp "${1:-$RECOVER}" "$R/data/luyaz/ship/stage/$TR/u60-recover.sh"
    {
        echo v=1
        echo "txn=$TR"
        echo comp=recover
        echo commit=abcdef1
        echo mac_time=1790000000
        echo "file=u60-recover.sh $(md5 "$R/data/luyaz/ship/stage/$TR/u60-recover.sh")"
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
setup; cp "$OLDREC" "$R/data/luyaz/ship/luyaz-recover.sh"; upload_recover; echo x >>"$R/data/luyaz/ship/stage/$TR/u60-recover.sh"; irun
irefused "install-recover: md5 wrong" "md5 不对"; teardown
setup; cp "$OLDREC" "$R/data/luyaz/ship/luyaz-recover.sh"; printf 'case x in\n' >"$T/bad.sh"; upload_recover "$T/bad.sh"; irun
irefused "install-recover: syntax error" "语法错误"; teardown
setup; cp "$OLDREC" "$R/data/luyaz/ship/luyaz-recover.sh"; sed 's/t "完成 → 不动" done "new$NL" "old$NL" done new/t "完成 → 不动" done "new$NL" "old$NL" done old/' "$RECOVER" >"$T/st.sh"; upload_recover "$T/st.sh"; irun
irefused "install-recover: its selftest fails" "自检没过"; teardown
setup; cp "$OLDREC" "$R/data/luyaz/ship/luyaz-recover.sh"; upload_recover; echo "evil=1" >>"$R/data/luyaz/ship/stage/$TR/meta"; irun
irefused "install-recover: unknown meta key" "看不懂的一行"; teardown
setup; cp "$OLDREC" "$R/data/luyaz/ship/luyaz-recover.sh"; upload_recover; printf 'v=1\ntxn=x\ncomp=datad\nphase=check\nend=1\n' >"$R/data/luyaz/ship/txn"; irun
irefused "install-recover: during a transaction" "还在进行"; teardown
setup; cp "$OLDREC" "$R/data/luyaz/ship/luyaz-recover.sh"; mkdir -p "$R/data/luyaz/ship/stage/20261001-120000-guard"; sh "$SHIP" install-recover 20261001-120000-guard >"$T/out" 2>&1; RC=$?
irefused "install-recover: txn not ending in -recover" "以 -recover 结尾"; teardown

# ═══ record: the directories ════════════════════════════════════════════════
setup
CASE="record directories"
mkdir -p "$DV/fonts" "$DV/operator-logos"
printf 'font\n' >"$DV/fonts/u60-cjk-fallback.ttf"
printf 'png\n' >"$DV/operator-logos/cmcc.png"
sh "$SHIP" record devui-fonts 1790000000 >"$T/out" 2>&1
RC=$?
check "$CASE: fonts recorded by fingerprint" "[ \$RC = 0 ] && grep -q '\"name\":\"ui-fonts\",\"path\":\"$DV/fonts\",\"tree\":\"$(fp "$DV/fonts")\"' '$U60S_MANIFEST' && grep -q 'u60-ship: 已记录 ui-fonts $(fp "$DV/fonts")' '$T/out'"
rm -rf "$DV/operator-logos"
sh "$SHIP" record ui-logos 1790000000 >"$T/out" 2>&1
check "$CASE: an absent directory is recorded as -" "grep -q '\"name\":\"ui-logos\",\"path\":\"${DV%/plugins/u60pro-devui}/luyaz/ui/operator-logos\",\"tree\":\"-\"' '$U60S_MANIFEST'"
ln -s u60-cjk-fallback.ttf "$DV/fonts/link.ttf"
sh "$SHIP" record devui-fonts 1790000000 >"$T/out" 2>&1
RC=$?
check "$CASE: a symlink inside: refused" '[ $RC = 1 ] && grep -q "算不了指纹" "$T/out" && [ "$(mlines)" = 2 ]'
teardown


# ═══ T7 sentinel (manager docs/designs/u60-sentinel.md 5f) ══════════════════
TS=20261010-120000-sentinel
RC_LEGACY='#!/bin/sh
[ -x /data/ssh/dropbear ] && sh /data/local/tmp/start_dropbear.sh
sh /data/u60-ship/u60-recover.sh   # u60_recover：开机先收尾
/etc/init.d/zte-agent start
/etc/init.d/luyaz-core start
/etc/init.d/u60-uid start
/etc/init.d/u60-guard start
sh /data/tailscale/start.sh
exit 0'
# sent_setup: the screen as now (u60-uid + UI 8200), the guard running, a
# shadow sentinel (8600), rc.local legacy; the sentinel's init (a stub) in place
sent_setup() {
    setup
    mkdir -p "$R/data/luyaz"
    printf '%s\n' "$RC_LEGACY" >"$R/etc/rc.local"
    chmod 755 "$R/etc/rc.local"
    RCL_OLD=$(md5 "$R/etc/rc.local")
    printf 'OLD-SENT /tmp/luyaz-screen.ctl /data/luyaz/scripts/luyaz-ship.sh /tmp/luyaz-screen.log\n' >"$R/data/luyaz/luyaz-sentinel"
    chmod 755 "$R/data/luyaz/luyaz-sentinel"
    printf '#!/bin/sh\nexit 0\n' >"$GD/u60-guard.sh"
    "$T/bin/guard-initd" start
    echo 8600 >"$T/pids/luyaz-sentinel"
    "$T/bin/proc_new" 8600 "$R/data/luyaz/luyaz-sentinel" "$R/data/luyaz/luyaz-sentinel" --shadow
    : >"$T/sent-initd.log"
    cat >"$T/bin/sent-initd" <<SEOF
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
        echo "\$u 8500 ours 0 \$u" >$T/sent.hb
        ;;
esac
exit 0
SEOF
    # kill: also the sentinel's pids
    cat >"$T/bin/kill" <<SEOF
#!/bin/sh
echo "\$*" >>$T/kill.log
[ "\$1" = -9 ] && shift
for p in "\$@"; do
    for n in luyaz-ui.test luyaz-ui; do
        [ "\$p" = "\$(cat $T/pids/\$n 2>/dev/null)" ] || continue
        rm -f $T/pids/\$n; rm -rf $T/proc/\$p
    done
    if grep -qx "\$p" $T/pids/luyaz-sentinel 2>/dev/null; then
        [ -f $T/shadow-unkillable ] && continue
        grep -vx "\$p" $T/pids/luyaz-sentinel >$T/pids/ls.tmp; mv $T/pids/ls.tmp $T/pids/luyaz-sentinel; rm -rf $T/proc/\$p
    fi
done
exit 0
SEOF
    chmod +x "$T/bin/sent-initd" "$T/bin/kill"
    export U60S_SENT_INITD=$T/bin/sent-initd U60S_SENT_HB=$T/sent.hb
    # 10e: /tmp/luyaz/.migrated, written by a sentinel that knows /tmp/luyaz
    mkdir -p "$T/tmp-luyaz"
    : >"$T/tmp-luyaz/.migrated"
    export U60S_SENT_TMP_MIGRATED=$T/tmp-luyaz/.migrated
    # the sentinel's heartbeat moves every sleep (unless hb-stuck); the core's
    # verdict as $T/verdict-at says: "<uptime> <word> <txn>"
    cat >"$T/hook" <<SEOF
u=\$(cut -d. -f1 $T/uptime)
st=\$(cat $T/sent.state 2>/dev/null); [ -n "\$st" ] || st=ours
if grep -qx 8500 $T/pids/luyaz-sentinel 2>/dev/null && [ ! -f $T/hb-stuck ]; then echo "\$u 8500 \$st 0 \$u" >$T/sent.hb; fi
if [ -f $T/verdict-at ]; then read -r va vw vt <$T/verdict-at; [ \$u -ge \$va ] && echo "\$vt \$vw judged" >$R/data/luyaz/ship/verdict; fi
:
SEOF
}
upload_sent() { # upload_sent <txn> [note]
    mkdir -p "$R/data/luyaz/ship/stage/$1"
    printf 'NEW-SENT %s /tmp/luyaz-screen.ctl /data/luyaz/scripts/luyaz-ship.sh /tmp/luyaz-screen.log\n' "$1" >"$R/data/luyaz/ship/stage/$1/luyaz-sentinel"
    SENT_NEW=$(md5 "$R/data/luyaz/ship/stage/$1/luyaz-sentinel")
    meta "$1" sentinel "file=luyaz-sentinel $SENT_NEW${2:+${NL}note=$2}"
}
verdict_at() { echo "$(($(cut -d. -f1 "$T/uptime") + $1)) $2 ${3:-$TS}" >"$T/verdict-at"; }
# ui_launch <pid>: the sentinel launches the UI, luyaz-ui (round ③: the only one)
ui_launch() { echo "$1" >"$T/pids/luyaz-ui"; "$T/bin/proc_new" "$1" "$UD/luyaz-ui" "$UD/luyaz-ui"; }
sent_pids_t() { cat "$T/pids/luyaz-sentinel" 2>/dev/null | tr '\n' ' '; }

sent_setup
CASE="sentinel: conversion"
upload_sent $TS convert
stage $TS
check "$CASE: staged as v=3" '[ "$RC" = 0 ] && [ "$(phase)" = staged ] && [ "$(txnv)" = 3 ]'
check "$CASE: rc.local derived on the device: sentinel line, recover re-runs, no uid/guard, rest unchanged" '[ -f "$R/etc/rc.local.test" ] && grep -q "^/etc/init.d/luyaz-sentinel start   #" "$R/etc/rc.local.test" && ! grep -q "u60-uid start\|u60-guard start" "$R/etc/rc.local.test" && grep -qF "sh /data/luyaz/ship/luyaz-recover.sh; [ \$? = 10 ] && exec /bin/sh /etc/rc.local   # u60_recover" "$R/etc/rc.local.test" && [ "$(grep -c . "$R/etc/rc.local.test")" = 8 ] && sh -n "$R/etc/rc.local.test"'
check "$CASE: the change logged" 'logged "rc.local 改法"'
verdict_at 120 ok
run $TS
check "$CASE: done" '[ "$RC" = 0 ] && [ "$(phase)" = done ]'
check "$CASE: rc.local converted, the old one kept where u60-recover.sh looks" '[ "$(md5 "$R/etc/rc.local")" != "$RCL_OLD" ] && [ "$(md5 "$R/data/luyaz/ship/prev/etc.rc.local.prev-$TS")" = "$RCL_OLD" ]'
check "$CASE: shadow killed, u60-uid and guard stopped and not started again" '! grep -qx 8600 "$T/pids/luyaz-sentinel" && [ ! -s "$T/pids/u60-uid" ] && [ ! -d "$T/proc/7300" ] && ! grep -q "^start" "$T/uid-initd.log"'
check "$CASE: the new sentinel runs and adopted the same UI" '[ "$(sent_pids_t)" = "8500 " ] && [ "$(md5sum <"$T/proc/8500/exe" | cut -d" " -f1)" = "$SENT_NEW" ] && [ "$(cat "$T/pids/luyaz-ui")" = 8200 ]'
check "$CASE: manifest line" "grep -q '\"comp\":\"sentinel\",\"txn\":\"$TS\"' '$U60S_MANIFEST'"
check "$CASE: /tmp/luyaz/.migrated removed before the sentinel started (10e)" '[ ! -e "$T/tmp-luyaz/.migrated" ]'
teardown

sent_setup
CASE="sentinel: refusals"
upload_sent $TS
stage $TS
check "$CASE: legacy rc.local without --convert" '[ "$RC" = 1 ] && grep -q "要明确的 --convert" "$T/out" && [ ! -e "$R/etc/rc.local.test" ]'
printf '%s\n/etc/init.d/u60-uid start\n' "$RC_LEGACY" >"$R/etc/rc.local"
upload_sent $TS convert
stage $TS
check "$CASE: an rc.local it does not know" '[ "$RC" = 1 ] && grep -q "rc.local 不是认得的样子" "$T/out"'
printf '%s\n' "$RC_LEGACY" >"$R/etc/rc.local"
cp "$OLDREC" "$R/data/luyaz/ship/luyaz-recover.sh"
upload_sent $TS convert
stage $TS
check "$CASE: a u60-recover.sh without format 3" '[ "$RC" = 1 ] && grep -q "v=3 的事务日志" "$T/out"'
cp "$RECOVER" "$R/data/luyaz/ship/luyaz-recover.sh"
printf '{"v":1,"kind":"ship","comp":"datad","txn":"x","commit":"1","format":1,"mac_time":1,"boot_id":"aaaa-1111","uptime":900,"files":[],"state":[]}\n' >"$U60S_MANIFEST"
upload_sent $TS convert
stage $TS
check "$CASE: datad shipped 100 s ago (D4)" '[ "$RC" = 1 ] && grep -q "一次只换一个" "$T/out"'
sed -i 's/"comp":"datad"/"comp":"core"/' "$U60S_MANIFEST"
upload_sent $TS convert
stage $TS
check "$CASE: core (its D4c name) shipped 100 s ago" '[ "$RC" = 1 ] && grep -q "一次只换一个" "$T/out"'
mv "$U60S_SENT_INITD" "$T/bin/sent-initd.away"
: >"$U60S_MANIFEST"
upload_sent $TS convert
stage $TS
check "$CASE: no sentinel init yet (ship scripts first)" '[ "$RC" = 1 ] && grep -q "先 u60 ship scripts" "$T/out"'
teardown

sent_setup
CASE="sentinel: the core never says ok"
upload_sent $TS convert
stage $TS
run $TS
check "$CASE: rolled back, says why" '[ "$RC" = 1 ] && [ "$(phase)" = rolledback ] && reason | grep -q "判定没到 ok"'
check "$CASE: rc.local back, the old pair runs again, no sentinel" '[ "$(md5 "$R/etc/rc.local")" = "$RCL_OLD" ] && [ -s "$T/pids/u60-uid" ] && [ -d "$T/proc/7300" ] && [ -z "$(sent_pids_t)" ] && [ -s "$T/pids/luyaz-ui" ]'
teardown

sent_setup
CASE="sentinel: the core says fail"
upload_sent $TS convert
stage $TS
verdict_at 40 fail
run $TS
check "$CASE: rolled back early" '[ "$(phase)" = rolledback ] && reason | grep -q "核心判定不过" && [ "$(md5 "$R/etc/rc.local")" = "$RCL_OLD" ]'
teardown

sent_setup
CASE="sentinel: a verdict about another transaction"
upload_sent $TS convert
stage $TS
verdict_at 10 ok 20261010-110000-touch
run $TS
check "$CASE: not taken: rolled back" '[ "$(phase)" = rolledback ] && reason | grep -q "判定没到 ok"'
teardown

sent_setup
CASE="sentinel: heartbeat stuck"
upload_sent $TS convert
stage $TS
verdict_at 30 ok
touch "$T/hb-stuck"
run $TS
check "$CASE: rolled back" '[ "$(phase)" = rolledback ] && reason | grep -q "心跳"'
teardown

sent_setup
CASE="sentinel: a shadow that will not die"
upload_sent $TS convert
stage $TS
touch "$T/shadow-unkillable"
run $TS
check "$CASE: no new sentinel ever started, rc.local back" '[ "$(phase)" != done ] && ! grep -q "^start" "$T/sent-initd.log" && [ "$(md5 "$R/etc/rc.local")" = "$RCL_OLD" ]'
teardown

sent_setup
CASE="sentinel: power cut in check, next boot"
upload_sent $TS convert
stage $TS
U60S_CRASH_AT=check:begin run $TS
recover_newboot
check "$CASE: u60-recover.sh puts rc.local back and says so (exit 10: rc.local re-runs)" '[ "$RRC" = 10 ] && [ "$(md5 "$R/etc/rc.local")" = "$RCL_OLD" ] && [ "$(phase)" = rolledback ]'
recover_newboot
check "$CASE: the re-run is quiet (exit 0)" '[ "$RRC" = 0 ]'
teardown

sent_setup
CASE="sentinel: an upgrade after the conversion"
upload_sent $TS convert
stage $TS
verdict_at 120 ok
run $TS
RCL_CONV=$(md5 "$R/etc/rc.local")
TS2=20261010-130000-sentinel
: >"$U60S_MANIFEST"
upload_sent $TS2
stage $TS2
check "$CASE: staged without rc.local (v=1)" '[ "$RC" = 0 ] && [ "$(txnv)" = 1 ] && [ ! -e "$R/etc/rc.local.test" ]'
verdict_at 120 ok $TS2
run $TS2
check "$CASE: done, rc.local untouched, the old sentinel replaced, never two" '[ "$(phase)" = done ] && [ "$(md5 "$R/etc/rc.local")" = "$RCL_CONV" ] && [ "$(sent_pids_t)" = "8500 " ] && grep -q "$TS2" "$T/proc/8500/exe"'
# removed in Phase 10 10c: uid and guard refused by name
for c in uid guard; do
    mkdir -p "$R/data/luyaz/ship/stage/20261010-140000-$c"
    meta 20261010-140000-$c $c
    stage 20261010-140000-$c
    check "$CASE: $c refused as deleted, nothing staged" '[ "$RC" = 1 ] && grep -q "已删除（Phase 10 10c）" "$T/out" && ! grep -q "20261010-140000-$c" "$R/data/luyaz/ship/txn" 2>/dev/null'
done
teardown

# R4 floor (round ③): a sentinel that does not know /tmp/luyaz-screen.ctl starts the deleted old UI
sent_setup
CASE="sentinel R4 floor: stage without the marker"
mkdir -p "$R/data/luyaz/ship/stage/$TS"
printf 'NEW-SENT %s /tmp/u60-uid.ctl\n' "$TS" >"$R/data/luyaz/ship/stage/$TS/luyaz-sentinel"
meta $TS sentinel "file=luyaz-sentinel $(md5 "$R/data/luyaz/ship/stage/$TS/luyaz-sentinel")${NL}note=convert"
stage $TS
check "$CASE: refused, says why, the live sentinel untouched" '[ "$RC" = 1 ] && grep -q "R4 之前的哨兵" "$T/out" && [ ! -s "$R/data/luyaz/ship/txn" ] && grep -q "^OLD-SENT" "$R/data/luyaz/luyaz-sentinel" && [ ! -e "$R/data/luyaz/luyaz-sentinel.test" ]'
teardown

setup
CASE="sentinel R4 floor: prepare-rollback"
mkdir -p "$R/data/luyaz"
printf 'NEW-SENT /tmp/luyaz-screen.ctl\n' >"$R/data/luyaz/luyaz-sentinel"
printf 'OLD-SENT /tmp/u60-uid.ctl\n' >"$R/data/luyaz/luyaz-sentinel.prev-$TS"
printf '{"kind":"ship","comp":"sentinel","txn":"%s","commit":"7e8d223","format":1,"files":[{"path":"%s","md5":"%s","prev":"%s"}]}\n' \
    "$TS" "$R/data/luyaz/luyaz-sentinel" "$(md5 "$R/data/luyaz/luyaz-sentinel")" "$R/data/luyaz/luyaz-sentinel.prev-$TS" >"$U60S_MANIFEST"
sh "$SHIP" prepare-rollback sentinel 20261010-130000-sentinel 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: a prev without the marker: refused, nothing prepared" '[ "$RC" != 0 ] && grep -q "R4 之前的" "$T/out" && [ ! -e "$R/data/luyaz/ship/stage/20261010-130000-sentinel" ]'
printf 'OLD-SENT /tmp/luyaz-screen.ctl /data/luyaz/scripts/luyaz-ship.sh /tmp/luyaz-screen.log\n' >"$R/data/luyaz/luyaz-sentinel.prev-$TS"
sh "$SHIP" prepare-rollback sentinel 20261010-130000-sentinel 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: a prev with the marker: prepared" '[ "$RC" = 0 ] && grep -q "^OLD-SENT /tmp/luyaz-screen.ctl" "$R/data/luyaz/ship/stage/20261010-130000-sentinel/luyaz-sentinel"'
# Luya-Z D2: once /data/u60-ops moved, the prev must take /tmp/luyaz-wifi.lock too
rm -rf "$R/data/luyaz/ship/stage/20261010-130000-sentinel"
mkdir -p "$R/data/luyaz/core/ops"
sh "$SHIP" prepare-rollback sentinel 20261010-130000-sentinel 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: D2 moved, a prev without the new lock: refused" '[ "$RC" != 0 ] && grep -q "D2 之前" "$T/out" && [ ! -e "$R/data/luyaz/ship/stage/20261010-130000-sentinel" ]'
printf 'OLD-SENT /tmp/luyaz-screen.ctl /tmp/luyaz-wifi.lock /data/luyaz/scripts/luyaz-ship.sh /tmp/luyaz-screen.log\n' >"$R/data/luyaz/luyaz-sentinel.prev-$TS"
sh "$SHIP" prepare-rollback sentinel 20261010-130000-sentinel 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: D2 moved, a prev with it: prepared" '[ "$RC" = 0 ]'
teardown

# D7: once 10c deleted u60-uid and u60-guard.sh, the legacy rc.local never comes back
sent_setup
CASE="sentinel D7: prepare-rollback after 10c"
upload_sent $TS convert
stage $TS
verdict_at 120 ok
run $TS
RCL_CONV=$(md5 "$R/etc/rc.local")
rm -f "$DV/u60-uid" "$GD/u60-guard.sh"
sh "$SHIP" prepare-rollback sentinel 20261010-130000-sentinel 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: refused, says why, nothing prepared" '[ "$RC" != 0 ] && grep -q "哨兵转正" "$T/out" && [ ! -e "$R/data/luyaz/ship/stage/20261010-130000-sentinel" ] && [ "$(md5 "$R/etc/rc.local")" = "$RCL_CONV" ]'
teardown

sent_setup
CASE="sentinel D7: power cut in check after 10c, next boot"
upload_sent $TS convert
stage $TS
RCL_CONV=$(md5 "$R/etc/rc.local.test")
U60S_CRASH_AT=check:begin run $TS
rm -f "$DV/u60-uid" "$GD/u60-guard.sh"
recover_newboot
check "$CASE: u60-recover.sh leaves the sentinel rc.local, says so" '[ "$(md5 "$R/etc/rc.local")" = "$RCL_CONV" ] && grep -q "不还原" "$R/data/luyaz/ship/recover.log"'
teardown

sent_setup
CASE="sentinel D7: power cut in check after 10c, recover-live"
upload_sent $TS convert
stage $TS
RCL_CONV=$(md5 "$R/etc/rc.local.test")
U60S_CRASH_AT=check:begin run $TS
rm -f "$DV/u60-uid" "$GD/u60-guard.sh"
: >"$T/uid-initd.log"
sh "$SHIP" recover-live >"$T/out" 2>&1
check "$CASE: the rollback leaves the sentinel rc.local, says so" '[ "$(md5 "$R/etc/rc.local")" = "$RCL_CONV" ] && logged "不还原" && ! grep -q "^start" "$T/uid-initd.log"'
teardown


sent_setup
CASE="touch under the sentinel: hold and release"
upload_sent $TS convert
stage $TS
verdict_at 120 ok
run $TS
# the sentinel's FIFO: hold → held; release → launch the live UI, ours
mkfifo "$T/ctl"
export U60S_UID_CTL=$T/ctl
(
    while :; do
        read -r l <"$T/ctl" || continue
        echo "$l" >>"$T/ctl.log"
        case "$l" in
            hold*) echo held >"$T/sent.state" ;;
            release)
                n=$(( $(cat "$T/nextpid") + 1 )); echo $n >"$T/nextpid"
                ui_launch $n
                echo ours >"$T/sent.state"
                ;;
            stop) exit 0 ;;
        esac
    done
) &
CTLR=$!
: >"$U60S_MANIFEST"
upload_touch
stage $TT
verdict_at 30 ok $TT
run $TT
check "$CASE: done" '[ "$(phase)" = done ]'
check "$CASE: the sentinel was asked to hold for the whole transaction, then release" '[ "$(cut -d" " -f1 "$T/ctl.log" | tr "\n" " ")" = "hold release " ] && [ "$(awk "NR == 1 { print \$2 }" "$T/ctl.log")" -ge 480 ]'
check "$CASE: the sentinel kept running (never stopped), u60-uid never started" '[ "$(sent_pids_t)" = "8500 " ] && ! grep -q "^stop" "$T/sent-initd.log" && ! grep -q "^start" "$T/uid-initd.log"'
check "$CASE: the new UI runs" '[ "$(ui_md5)" = "$UI_NEW" ]'
echo stop >"$T/ctl" & sleep 0
kill $CTLR 2>/dev/null
unset U60S_UID_CTL
teardown

sent_setup
CASE="touch under the sentinel: a trial longer than the sentinel's 3600 s hold"
upload_sent $TS convert
stage $TS
verdict_at 120 ok
run $TS
# hold N is capped at 3600 s and runs out on the fake clock (checked after each sleep): the
# sentinel would then take the screen back and launch whatever the live file is (10-11)
mkfifo "$T/ctl"
export U60S_UID_CTL=$T/ctl
(
    while :; do
        read -r l <"$T/ctl" || continue
        echo "$l" >>"$T/ctl.log"
        case "$l" in
            hold*) n=${l#hold }; [ "$n" -gt 3600 ] && n=3600
                echo $(( $(cut -d. -f1 "$T/uptime") + n )) >"$T/hold.until"; echo held >"$T/sent.state" ;;
            release)
                rm -f "$T/hold.until"
                n=$(( $(cat "$T/nextpid") + 1 )); echo $n >"$T/nextpid"
                ui_launch $n
                echo ours >"$T/sent.state"
                ;;
            stop) exit 0 ;;
        esac
    done
) &
CTLR=$!
# (in front of the sentinel stub's own hook, which writes the heartbeat from sent.state)
{ cat <<HOOK
u=\$(cat $T/hold.until 2>/dev/null)
if [ -n "\$u" ] && [ "\$(cut -d. -f1 $T/uptime)" -ge "\$u" ]; then echo expired >>$T/hold.expired; rm -f $T/hold.until; echo ours >$T/sent.state; fi
HOOK
cat "$T/hook"; } >"$T/hook.new" && mv "$T/hook.new" "$T/hook"
: >"$U60S_MANIFEST"
upload_touch
printf 'trial=3900\n' >>"$R/data/luyaz/ship/stage/$TT/meta"
stage $TT
verdict_at 3930 ok $TT
timeout 600 sh "$SHIP" run $TT >"$T/out" 2>&1; RC=$?
check "$CASE: done, the new UI runs" '[ "$(phase)" = done ] && [ "$(ui_md5)" = "$UI_NEW" ]'
check "$CASE: no hold over 3600 s, renewed during the trial, never ran out" '[ -z "$(awk "\$1 == \"hold\" && \$2 > 3600" "$T/ctl.log")" ] && [ "$(grep -c "^hold" "$T/ctl.log")" -ge 7 ] && [ ! -s "$T/hold.expired" ]'
echo stop >"$T/ctl" & sleep 0
kill $CTLR 2>/dev/null
unset U60S_UID_CTL
teardown

sent_setup
CASE="touch under the sentinel: no ok from the core"
upload_sent $TS convert
stage $TS
verdict_at 120 ok
run $TS
mkfifo "$T/ctl"
export U60S_UID_CTL=$T/ctl
(
    while :; do
        read -r l <"$T/ctl" || continue
        case "$l" in
            hold*) echo held >"$T/sent.state" ;;
            release)
                n=$(( $(cat "$T/nextpid") + 1 )); echo $n >"$T/nextpid"
                ui_launch $n
                echo ours >"$T/sent.state"
                ;;
            stop) exit 0 ;;
        esac
    done
) &
CTLR=$!
: >"$U60S_MANIFEST"
upload_touch
stage $TT
run $TT
check "$CASE: rolled back (D5, fail closed), the old UI back on screen" '[ "$(phase)" = rolledback ] && reason | grep -q "判定没到 ok" && [ "$(ui_md5)" = "$UI_OLD" ]'
echo stop >"$T/ctl" & sleep 0
kill $CTLR 2>/dev/null
unset U60S_UID_CTL
teardown

# ═══ scripts (D2) ═══════════════════════════════════════════════════════════
setup
CASE="ship scripts"
TSC=20261010-140000-scripts
mkdir -p "$R/data/luyaz/ship/stage/$TSC"
_ml=
# the first ship into the new directory (setup's luyaz-fallback.sh aside); the pre-rename
# fallback a kit put there is not in the list any more: left as it is
rm -rf "$R/data/luyaz/scripts"
mkdir -p "$R/data/luyaz/scripts" && printf '#!/bin/sh\n# OLD u60-fallback.sh\n' >"$R/data/luyaz/scripts/u60-fallback.sh"
# zte-agent's init (both copies) left the list in 8d-2c: they stay as they are
printf '#!/bin/sh\n# OLD zte-agent.init\n' >"$GD/zte-agent.init"
for f in alert-lib.sh agent-auth.sh chaos.sh doctor.sh doctor-legacy.sh config-backup.sh wan-sources.sh rpc-acl.sh luyaz-fallback.sh VERSION; do
    printf '#!/bin/sh\n# OLD %s\nexit 0\n' "$f" >"$GD/$f"
    printf '#!/bin/sh\n# NEW %s luyaz-write.lock\nexit 0\n' "$f" >"$R/data/luyaz/ship/stage/$TSC/$f"
    _ml="$_ml${_ml:+$NL}file=$f $(md5 "$R/data/luyaz/ship/stage/$TSC/$f")"
done
for f in zte-agent zwrt-datad; do printf '#!/bin/sh\n# OLD init\n' >"$R/etc/init.d/$f"; done
for f in luyaz-core luyaz-sentinel luyaz-web; do
    printf '#!/bin/sh /etc/rc.common\n# NEW %s\n' "$f" >"$R/data/luyaz/ship/stage/$TSC/$f"
    _ml="$_ml$NL""file=$f $(md5 "$R/data/luyaz/ship/stage/$TSC/$f")"
done
meta "$TSC" scripts "$_ml"
stage $TSC
check "$CASE: staged as v=5 (it brings the sentinel's, luyaz-web's and luyaz-core's inits)" '[ "$RC" = 0 ] && [ "$(txnv)" = 5 ]'
run $TSC
check "$CASE: done, the sentinel's and luyaz-web's inits are new, the guard untouched" '[ "$(phase)" = done ] && grep -q "NEW luyaz-web" "$R/etc/init.d/luyaz-web" && grep -q "NEW luyaz-sentinel" "$R/etc/init.d/luyaz-sentinel" && grep -q "NEW luyaz-core" "$R/etc/init.d/luyaz-core" && grep -q "NEW doctor.sh" "$R/data/luyaz/scripts/doctor.sh" && ! grep -q "^stop\|^start" "$T/guard-initd.log"'
# Luya-Z rename D1: the first ship into the new directory makes it and adds every file there
check "$CASE: the scripts went to /data/luyaz/scripts, the old copies stay as they were" '[ "$(ls "$R/data/luyaz/scripts" | wc -l)" = 11 ] && grep -q "OLD doctor.sh" "$GD/doctor.sh" && grep -q "NEW luyaz-fallback.sh" "$R/data/luyaz/scripts/luyaz-fallback.sh" && grep -q "OLD u60-fallback.sh" "$R/data/luyaz/scripts/u60-fallback.sh" && grep -q "/data/luyaz/scripts/doctor.sh" "$R/data/luyaz/ship/txn"'
check "$CASE: zte-agent's init untouched (not in the list since 8d-2c)" 'grep -q "OLD init" "$R/etc/init.d/zte-agent" && grep -q "OLD zte-agent.init" "$GD/zte-agent.init" && ! grep -q "zte-agent" "$R/data/luyaz/ship/txn"'
recover_newboot
check "$CASE: a quiet next boot" '[ "$RRC" = 0 ]'
teardown

# a stage that still carries zte-agent's init (an old Mac side) is refused
setup
CASE="ship scripts: an upload with zte-agent's init"
TSC=20261010-150000-scripts
mkdir -p "$R/data/luyaz/ship/stage/$TSC"
_ml=
for f in alert-lib.sh agent-auth.sh chaos.sh doctor.sh doctor-legacy.sh config-backup.sh wan-sources.sh luyaz-fallback.sh zte-agent.init zwrt-datad.init zte-agent zwrt-datad luyaz-sentinel luyaz-web; do
    printf '#!/bin/sh\n# NEW %s luyaz-write.lock\n' "$f" >"$R/data/luyaz/ship/stage/$TSC/$f"
    _ml="$_ml${_ml:+$NL}file=$f $(md5 "$R/data/luyaz/ship/stage/$TSC/$f")"
done
meta "$TSC" scripts "$_ml"
stage $TSC
check "$CASE: refused" '[ "$RC" = 1 ] && grep -q "zte-agent" "$T/out" && [ ! -e "$R/data/luyaz/ship/txn" ]'
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

# ═══ scripts without a u60-recover.sh that reads v=4 ════════════════════════
setup
CASE="ship scripts: a u60-recover.sh without formats 4"
TSC=20261012-140000-scripts
mkdir -p "$R/data/luyaz/ship/stage/$TSC"
_ml=
for f in alert-lib.sh agent-auth.sh chaos.sh doctor.sh doctor-legacy.sh config-backup.sh wan-sources.sh rpc-acl.sh luyaz-fallback.sh VERSION luyaz-core luyaz-sentinel luyaz-web; do
    printf '#!/bin/sh\n# NEW %s luyaz-write.lock\n' "$f" >"$R/data/luyaz/ship/stage/$TSC/$f"
    _ml="$_ml${_ml:+$NL}file=$f $(md5 "$R/data/luyaz/ship/stage/$TSC/$f")"
done
meta "$TSC" scripts "$_ml"
sed 's/formats) echo "1 2 3 4 5" ;;/formats) echo "1 2 3" ;;/' "$RECOVER" >"$R/data/luyaz/ship/luyaz-recover.sh"
stage $TSC
check "$CASE: refused, install-recover first" '[ "$RC" = 1 ] && grep -q "v=4" "$T/out" && grep -q "install-recover" "$T/out"'
teardown

# ═══ scripts without a u60-recover.sh that reads v=5 (10e: luyaz-core's init) ═══
setup
CASE="ship scripts: a u60-recover.sh without formats 5"
TSC=20261014-140000-scripts
mkdir -p "$R/data/luyaz/ship/stage/$TSC"
_ml=
for f in alert-lib.sh agent-auth.sh chaos.sh doctor.sh doctor-legacy.sh config-backup.sh wan-sources.sh rpc-acl.sh luyaz-fallback.sh VERSION luyaz-core luyaz-sentinel luyaz-web; do
    printf '#!/bin/sh\n# NEW %s luyaz-write.lock\n' "$f" >"$R/data/luyaz/ship/stage/$TSC/$f"
    _ml="$_ml${_ml:+$NL}file=$f $(md5 "$R/data/luyaz/ship/stage/$TSC/$f")"
done
meta "$TSC" scripts "$_ml"
sed 's/formats) echo "1 2 3 4 5" ;;/formats) echo "1 2 3 4" ;;/' "$RECOVER" >"$R/data/luyaz/ship/luyaz-recover.sh"
stage $TSC
check "$CASE: refused, install-recover first" '[ "$RC" = 1 ] && grep -q "v=5" "$T/out" && grep -q "install-recover" "$T/out"'
teardown

# ═══ Luya-Z rename D4b-2: move-d4, the recover shim, the D4 floors ═══
d4_ready() { # the old layout as on the device before the move, everything that must know the new place
    unset U60S_MANIFEST
    # setup builds the moved layout: take it back to before the move
    rm -rf "$R/data/luyaz/ship"
    mkdir -p "$R/data/u60-ship/stage"
    cp "$RECOVER" "$R/data/u60-ship/u60-recover.sh"
    CB=$R/data/luyaz/luyaz-core
    mkdir -p "${CB%/*}" "$R/data/luyaz/scripts" "$R/data/u60-ship/prev"
    echo "CORE /data/luyaz/ship/manifest.jsonl /tmp/luyaz-screen.log" >"$CB"
    echo "SENT /tmp/luyaz-screen.ctl /data/luyaz/scripts/luyaz-ship.sh /tmp/luyaz-screen.log" >"$R/data/luyaz/luyaz-sentinel"
    echo "doctor /data/luyaz/ship" >"$R/data/luyaz/scripts/doctor-legacy.sh"
    cp "$SHIP" "$GD/u60-ship.sh"
    echo trial >"$GD/datad-trial.sh"
    echo "old rc" >"$R/data/u60-ship/prev/etc.rc.local.prev-20261010-113933-sentinel"
    echo "rec prev" >"$R/data/u60-ship/u60-recover.sh.prev-20261015-000000-recover"
    printf '{"kind":"ship","comp":"scripts","txn":"20261015-144406-scripts","boot_id":"old","uptime":1,"files":[{"path":"%s/etc/init.d/luyaz-core","md5":"x","prev":"%s/data/u60-ship/prev/etc.init.d.luyaz-core.prev-20261015-144406-scripts"}]}\n' "$R" "$R" >"$R/data/u60-manifest.jsonl"
}
setup
CASE="move-d4"
d4_ready
echo "CORE old" >"$CB"
sh "$SHIP" move-d4 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: a core from before D4b-1: refused, nothing moved" '[ "$RC" != 0 ] && grep -q "ship core" "$T/out" && [ ! -e "$R/data/luyaz/ship" ] && [ -f "$GD/u60-ship.sh" ]'
echo "CORE /data/luyaz/ship/manifest.jsonl /tmp/luyaz-screen.log" >"$CB"
printf '{"kind":"ship","comp":"touch","txn":"20261015-150000-touch","boot_id":"aaaa-1111","uptime":900}\n' >>"$R/data/u60-manifest.jsonl"
sh "$SHIP" move-d4 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: in an observation window: refused" '[ "$RC" != 0 ] && grep -q "观察期" "$T/out" && [ ! -e "$R/data/luyaz/ship" ]'
sed -i '$d' "$R/data/u60-manifest.jsonl"
OLDREC_MD5=$(md5 "$R/data/u60-ship/u60-recover.sh")
sh "$SHIP" move-d4 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: moved" '[ "$RC" = 0 ] && grep -q "U60 move-d4 done" "$T/out" && [ -f "$R/data/luyaz/ship/.migrated" ]'
check "$CASE: recover, prevs and logs in the new place" '[ "$(md5 "$R/data/luyaz/ship/luyaz-recover.sh")" = "$OLDREC_MD5" ] && [ -f "$R/data/luyaz/ship/prev/etc.rc.local.prev-20261010-113933-sentinel" ] && [ -f "$R/data/luyaz/ship/luyaz-recover.sh.prev-20261015-000000-recover" ]'
check "$CASE: manifest moved, its old prefix rewritten, the recover recorded" 'grep -q "\"$R/data/luyaz/ship/prev/etc.init.d.luyaz-core" "$R/data/luyaz/ship/manifest.jsonl" && ! grep -q "/data/u60-ship/" "$R/data/luyaz/ship/manifest.jsonl" && tail -n 1 "$R/data/luyaz/ship/manifest.jsonl" | grep -q "\"path\":\"$R/data/luyaz/ship/luyaz-recover.sh\"" && [ ! -e "$R/data/u60-manifest.jsonl" ]'
check "$CASE: executor and datad-trial in scripts, the old ones gone" '[ "$(md5 "$R/data/luyaz/scripts/luyaz-ship.sh")" = "$(md5 "$SHIP")" ] && [ -f "$R/data/luyaz/scripts/datad-trial.sh" ] && [ ! -e "$GD/u60-ship.sh" ] && [ ! -e "$GD/datad-trial.sh" ]'
check "$CASE: only the shim left in the old directory" '[ "$(ls "$R/data/u60-ship")" = u60-recover.sh ] && grep -q "exec sh" "$R/data/u60-ship/u60-recover.sh"'
sh "$SHIP" move-d4 1790000100 >"$T/out" 2>&1
check "$CASE: twice: already" 'grep -q "U60 move-d4 already" "$T/out"'
# the shim: the new one's exit code, 0 when it is not there
printf '#!/bin/sh\nexit 10\n' >"$R/data/luyaz/ship/luyaz-recover.sh"
sh "$R/data/u60-ship/u60-recover.sh"; RC=$?
check "$CASE: shim hands over exit 10" '[ "$RC" = 10 ]'
printf '#!/bin/sh\necho "args:$*"\n' >"$R/data/luyaz/ship/luyaz-recover.sh"
check "$CASE: shim passes the arguments" '[ "$(sh "$R/data/u60-ship/u60-recover.sh" formats)" = "args:formats" ]'
rm -f "$R/data/luyaz/ship/luyaz-recover.sh"
sh "$R/data/u60-ship/u60-recover.sh"; RC=$?
check "$CASE: shim, new one missing: 0" '[ "$RC" = 0 ]'
# the real recover, through the shim, finishes a txn in the new directory
cp "$RECOVER" "$R/data/luyaz/ship/luyaz-recover.sh"
printf 'v=1\ntxn=20261016-090000-datad\ncomp=datad\nphase=check\nboot_id=old-boot\nend=1\n' >"$R/data/luyaz/ship/txn"
sh "$R/data/u60-ship/u60-recover.sh" >/dev/null 2>&1
check "$CASE: recover reads the new directory" '[ -s "$R/data/luyaz/ship/recover.log" ] && [ ! -e "$R/data/u60-ship/recover.log" ]'
rm -f "$R/data/luyaz/ship/txn"
check "$CASE: status reads the new txn" 'sh "$R/data/luyaz/scripts/luyaz-ship.sh" status >"$T/st" 2>&1; ! grep -q "看不懂" "$T/st"'
# the rc.local line under either name
printf 'sh /data/luyaz/ship/luyaz-recover.sh; [ $? = 10 ] && exec /bin/sh /etc/rc.local\n/etc/init.d/luyaz-core start\n/etc/init.d/luyaz-sentinel start\nexit 0\n' >"$T/rc.new"
sed -n '/^rc_mode() {/,/^}/p' "$SHIP" >"$T/rcm.sh"; echo 'rc_mode "$1"' >>"$T/rcm.sh"
check "$CASE: rc_mode knows the new recover line" '[ "$(sh "$T/rcm.sh" "$T/rc.new")" = sentinel ]'
# D4 floors, staging: a sentinel and a core from before D4b-1
TS4=20261016-100000-sentinel
mkdir -p "$R/data/luyaz/ship/stage/$TS4"
echo "SENT /tmp/luyaz-screen.ctl /tmp/luyaz-wifi.lock" >"$R/data/luyaz/ship/stage/$TS4/luyaz-sentinel"
META_DIR=$R/data/luyaz/ship/stage meta "$TS4" sentinel "file=luyaz-sentinel $(md5 "$R/data/luyaz/ship/stage/$TS4/luyaz-sentinel")"
sh "$R/data/luyaz/scripts/luyaz-ship.sh" stage $TS4 >"$T/out" 2>&1
RC=$?
check "$CASE: stage a sentinel from before D4: refused" '[ "$RC" != 0 ] && grep -q "D4 之前的哨兵" "$T/out"'
teardown

setup
CASE="D4 floor: rollback"
d4_ready
sh "$SHIP" move-d4 1790000100 >/dev/null 2>&1
export U60S_MANIFEST=$R/data/luyaz/ship/manifest.jsonl
echo "NEW-CORE /data/luyaz/scripts /data/luyaz/core/ops /data/luyaz/ship/manifest.jsonl /tmp/luyaz-screen.log" >"$CB"
echo "OLD-CORE /data/luyaz/scripts /data/luyaz/core/ops" >"$CB.prev-20261016-090000-datad"
mkdir -p "$R/data/luyaz/core/ops"
printf '{"kind":"ship","comp":"datad","txn":"20261016-090000-datad","commit":"7e8d223","format":1,"files":[{"path":"%s","md5":"%s","prev":"%s"}]}\n' \
    "$CB" "$(md5 "$CB")" "$CB.prev-20261016-090000-datad" >>"$U60S_MANIFEST"
sh "$SHIP" prepare-rollback datad 20261016-100000-datad 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: core prev from before D4: refused" '[ "$RC" != 0 ] && grep -q "D4 之前" "$T/out" && [ ! -e "$R/data/luyaz/ship/stage/20261016-100000-datad" ]'
echo "OLD-CORE /data/luyaz/scripts /data/luyaz/core/ops /data/luyaz/ship/manifest.jsonl /tmp/luyaz-screen.log" >"$CB.prev-20261016-090000-datad"
sh "$SHIP" prepare-rollback datad 20261016-100000-datad 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: a prev that knows it: prepared in the new stage" '[ "$RC" = 0 ] && [ -e "$R/data/luyaz/ship/stage/20261016-100000-datad" ]'
teardown

# ═══ Luya-Z rename E2 de-old: only luyaz-write.lock / /tmp/luyaz-screen.log ═══
setup
CASE="E2 floor: stage"
TE=20261016-110000-sentinel
mkdir -p "$R/data/luyaz/ship/stage/$TE"
printf 'NEW-SENT /tmp/luyaz-screen.ctl /tmp/luyaz-wifi.lock /data/luyaz/scripts/luyaz-ship.sh\n' >"$R/data/luyaz/ship/stage/$TE/luyaz-sentinel"
meta $TE sentinel "file=luyaz-sentinel $(md5 "$R/data/luyaz/ship/stage/$TE/luyaz-sentinel")"
stage $TE
check "$CASE: a sentinel from before E2: refused" '[ "$RC" = 1 ] && grep -q "E2 之前的" "$T/out" && [ ! -s "$R/data/luyaz/ship/txn" ]'
TE=20261016-110000-datad
mkdir -p "$R/data/luyaz/ship/stage/$TE"
printf 'NEW-CORE /data/luyaz/scripts /data/luyaz/core/ops LUYAZ_CORE_BUILD_SELF_CONTAINED /data/luyaz/ship/manifest.jsonl\n' >"$R/data/luyaz/ship/stage/$TE/zwrt-datad"
meta $TE datad "file=zwrt-datad $(md5 "$R/data/luyaz/ship/stage/$TE/zwrt-datad")"
stage $TE
check "$CASE: a core from before E2: refused" '[ "$RC" = 1 ] && grep -q "luyaz-core 是 Luya-Z 改名 E2 之前的" "$T/out" && [ ! -s "$R/data/luyaz/ship/txn" ]'
TE=20261016-110000-scripts
mkdir -p "$R/data/luyaz/ship/stage/$TE"
_ml=
for f in alert-lib.sh agent-auth.sh chaos.sh doctor.sh doctor-legacy.sh config-backup.sh wan-sources.sh rpc-acl.sh luyaz-fallback.sh VERSION luyaz-core luyaz-sentinel luyaz-web; do
    printf '#!/bin/sh\n# NEW %s luyaz-write.lock\n' "$f" >"$R/data/luyaz/ship/stage/$TE/$f"
    _ml="$_ml${_ml:+$NL}file=$f $(md5 "$R/data/luyaz/ship/stage/$TE/$f")"
done
printf '#!/bin/sh\n# NEW luyaz-fallback.sh u60-write.lock\n' >"$R/data/luyaz/ship/stage/$TE/luyaz-fallback.sh"
_ml=$(printf '%s\n' "$_ml" | sed "s/^file=luyaz-fallback.sh .*/file=luyaz-fallback.sh $(md5 "$R/data/luyaz/ship/stage/$TE/luyaz-fallback.sh")/")
meta "$TE" scripts "$_ml"
stage $TE
check "$CASE: a luyaz-fallback.sh from before E2: refused" '[ "$RC" = 1 ] && grep -q "luyaz-fallback.sh 是 Luya-Z 改名 E2 之前的" "$T/out" && [ ! -s "$R/data/luyaz/ship/txn" ]'
teardown

setup
CASE="E2 floor: rollback"
d4_ready
sh "$SHIP" move-d4 1790000100 >/dev/null 2>&1
export U60S_MANIFEST=$R/data/luyaz/ship/manifest.jsonl
mkdir -p "$R/data/luyaz/core/ops"
echo "NEW-CORE /data/luyaz/scripts /data/luyaz/core/ops /data/luyaz/ship/manifest.jsonl /tmp/luyaz-screen.log" >"$CB"
echo "OLD-CORE /data/luyaz/scripts /data/luyaz/core/ops /data/luyaz/ship/manifest.jsonl" >"$CB.prev-20261016-090000-datad"
printf '{"kind":"ship","comp":"datad","txn":"20261016-090000-datad","commit":"7e8d223","format":1,"files":[{"path":"%s","md5":"%s","prev":"%s"}]}\n' \
    "$CB" "$(md5 "$CB")" "$CB.prev-20261016-090000-datad" >>"$U60S_MANIFEST"
sh "$SHIP" prepare-rollback datad 20261016-100000-datad 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: core prev from before E2: refused" '[ "$RC" != 0 ] && grep -q "E2 之前" "$T/out" && [ ! -e "$R/data/luyaz/ship/stage/20261016-100000-datad" ]'
echo "OLD-CORE /data/luyaz/scripts /data/luyaz/core/ops /data/luyaz/ship/manifest.jsonl /tmp/luyaz-screen.log" >"$CB.prev-20261016-090000-datad"
sh "$SHIP" prepare-rollback datad 20261016-100000-datad 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: a core prev that knows it: prepared" '[ "$RC" = 0 ] && [ -e "$R/data/luyaz/ship/stage/20261016-100000-datad" ]'
SB=$R/data/luyaz/luyaz-sentinel
printf 'OLD-SENT /tmp/luyaz-screen.ctl /tmp/luyaz-wifi.lock /data/luyaz/scripts/luyaz-ship.sh\n' >"$SB.prev-20261016-090000-sentinel"
printf '{"kind":"ship","comp":"sentinel","txn":"20261016-090000-sentinel","commit":"7e8d223","format":1,"files":[{"path":"%s","md5":"%s","prev":"%s"}]}\n' \
    "$SB" "$(md5 "$SB")" "$SB.prev-20261016-090000-sentinel" >>"$U60S_MANIFEST"
sh "$SHIP" prepare-rollback sentinel 20261016-100000-sentinel 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: sentinel prev from before E2: refused" '[ "$RC" != 0 ] && grep -q "E2 之前" "$T/out" && [ ! -e "$R/data/luyaz/ship/stage/20261016-100000-sentinel" ]'
printf 'OLD-SENT /tmp/luyaz-screen.ctl /tmp/luyaz-wifi.lock /data/luyaz/scripts/luyaz-ship.sh /tmp/luyaz-screen.log\n' >"$SB.prev-20261016-090000-sentinel"
sh "$SHIP" prepare-rollback sentinel 20261016-100000-sentinel 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: a sentinel prev that knows it: prepared" '[ "$RC" = 0 ] && [ -e "$R/data/luyaz/ship/stage/20261016-100000-sentinel" ]'
FBL=$R/data/luyaz/scripts/luyaz-fallback.sh
printf '#!/bin/sh\n# NEW luyaz-write.lock\n' >"$FBL"
printf '#!/bin/sh\n# OLD u60-write.lock\n' >"$FBL.prev-20261016-090000-scripts"
printf '{"kind":"ship","comp":"scripts","txn":"20261016-090000-scripts","commit":"7e8d223","format":1,"files":[{"path":"%s","md5":"%s","prev":"%s"}]}\n' \
    "$FBL" "$(md5 "$FBL")" "$FBL.prev-20261016-090000-scripts" >>"$U60S_MANIFEST"
sh "$SHIP" prepare-rollback scripts 20261016-100000-scripts 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: scripts whose luyaz-fallback.sh prev is from before E2: refused" '[ "$RC" != 0 ] && grep -q "luyaz-fallback.sh 是 Luya-Z 改名 E2 之前的" "$T/out" && [ ! -e "$R/data/luyaz/ship/stage/20261016-100000-scripts" ]'
printf '#!/bin/sh\n# OLD luyaz-write.lock\n' >"$FBL.prev-20261016-090000-scripts"
sh "$SHIP" prepare-rollback scripts 20261016-100000-scripts 1790000100 >"$T/out" 2>&1
check "$CASE: a luyaz-fallback.sh prev that takes the new lock: not refused for it" '! grep -q "E2 之前" "$T/out"'
teardown

# ═══ Luya-Z rename E3: no old-layout device gets an E3 build ═══
setup
CASE="E3: old layout"
TE=20261016-120000-sentinel
mkdir -p "$R/data/luyaz/ship/stage/$TE"
printf 'NEW-SENT /tmp/luyaz-screen.ctl /tmp/luyaz-wifi.lock /data/luyaz/scripts/luyaz-ship.sh /tmp/luyaz-screen.log\n' >"$R/data/luyaz/ship/stage/$TE/luyaz-sentinel"
meta $TE sentinel "file=luyaz-sentinel $(md5 "$R/data/luyaz/ship/stage/$TE/luyaz-sentinel")"
rm -f "$R/data/luyaz/screen/.migrated"
stage $TE
check "$CASE: the screen state not moved: refused" '[ "$RC" = 1 ] && grep -q "旧布局" "$T/out" && [ ! -s "$R/data/luyaz/ship/txn" ]'
: >"$R/data/luyaz/screen/.migrated"
mkdir -p "$R/data/u60d"
rm -f "$R/data/luyaz/core/.migrated"
stage $TE
check "$CASE: /data/u60d without the core's .migrated: refused" '[ "$RC" = 1 ] && grep -q "旧布局" "$T/out" && [ ! -s "$R/data/luyaz/ship/txn" ]'
rm -rf "$R/data/u60d"; : >"$R/data/luyaz/core/.migrated"
stage $TE
check "$CASE: new layout: not refused for it" '! grep -q "旧布局" "$T/out"'
teardown

echo "u60-ship phase two: $PASS passed, $FAIL failed"
[ "$FAIL" = 0 ]
