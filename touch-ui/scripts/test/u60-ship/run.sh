#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# Tests for u60-ship.sh (ship transactions: stage, trial, promote, check,
# manifest, rollback) and its hand-off to u60-recover.sh after a power cut.
# Every device command is stubbed and every device path lives under $T:
# after each case the container's real /data, /tmp/luyaz/ship and /etc/init.d
# must still be untouched. Runs in a plain busybox container:
#
#   scripts/test/docker.sh        (runs every suite under scripts/test/)
#
# Time is fake: $T/uptime is /proc/uptime and the stub `sleep` advances it,
# then runs $T/hook (break one thing at a set time). A "power cut" is
# U60S_CRASH_AT=<point>: the executor kill -9s itself there (no traps run),
# then u60-recover.sh runs with another boot_id, as rc.local would.
# SPDX-License-Identifier: MIT
# ─────────────────────────────────────────────────────────────────────────────

SCRIPTS=${SCRIPTS:-/scripts}
SHIP=$SCRIPTS/u60-ship.sh
RECOVER=$SCRIPTS/u60-recover.sh
PASS=0
FAIL=0

ok() { PASS=$((PASS + 1)); echo "  ok   $1"; }
bad() { FAIL=$((FAIL + 1)); echo "  FAIL $1"; }
check() { # check <description> <shell test…>
    _d=$1
    shift
    if eval "$@"; then ok "$_d"; else bad "$_d  [$*]"; fi
}

# no timeout(1) on the device busybox: a watchdog with the real sleep
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

# state of the real container paths before any case (leak check)
leak_snapshot() { ls -la /data /tmp/luyaz/ship /etc/init.d /tmp/datad-trial.pid 2>&1 | md5sum; }
LEAK0=$(leak_snapshot)

TXN=20260930-120000-datad
LIVE=
# the D1 and D4 floors (u60-ship.sh SCRIPTS_MARK, SHIP_CORE_MARK): every core build here knows them
MARKS='/data/luyaz/scripts /data/luyaz/ship/manifest.jsonl /tmp/luyaz-screen.log'
setup() {
    T=$(mktemp -d)
    R=$T/root
    D=$R/data/luyaz
    LIVE=$D/luyaz-core
    mkdir -p "$T/bin" "$T/pids" "$T/tmp" "$D/core" "$R/data/luyaz/ship/stage" "$R/etc/init.d"
    : >"$R/data/luyaz/ship/.migrated"
    # the new layout (the zwrt-datad one is refused since E3)
    : >"$D/core/.migrated"
    echo 1000 >"$T/uptime"
    echo aaaa-1111 >"$T/boot_id"
    echo 5555 >"$T/pids/luyaz-core"
    echo 3131 >"$T/pids/luyaz-ui"
    : >"$T/logread"
    : >"$T/kill.log"
    : >"$T/initd.log"
    : >"$T/setsid.log"
    echo '  PID USER       VSZ STAT COMMAND' >"$T/ps"
    printf ':\n' >"$T/hook"
    printf 'OLD-DATAD\n%s\n' "$MARKS" >"$LIVE"
    chmod 755 "$LIVE"
    OLD=$(md5 "$LIVE")
    # hand-made neighbours that ship must never touch
    for f in "$D/luyaz-core.prev-cache-20260923" "$D/luyaz-core.orig-backup" "$R/data/zte-agent" \
        "$R/data/zte-agent.env" "$R/data/zte-agent.prev-esim-20260926" "$R/data/admin.prev-reset-20260926.tgz"; do
        echo "keep $f" >"$f"
    done

    cat >"$T/bin/sleep" <<EOF
#!/bin/sh
echo \$(( \$(cut -d. -f1 $T/uptime | tr -dc 0-9) + \${1%%.*} )) >$T/uptime
. $T/hook
EOF
    cat >"$T/bin/pidof" <<EOF
#!/bin/sh
cat $T/pids/"\$1" 2>/dev/null | grep . || exit 1
EOF
    cat >"$T/bin/curl" <<EOF
#!/bin/sh
[ -f $T/curl-fail ] && exit 7
if [ -f $T/frozen ]; then ts=\$(cat $T/frozen-ts); else ts=\$(cut -d. -f1 $T/uptime | tr -dc 0-9); echo \$ts >$T/frozen-ts; fi
echo "{\"epoch\":\"e\",\"seq\":3,\"blocks\":{\"live\":{\"revision\":1,\"observed_at\":\$ts,\"stale\":false,\"data\":{}},\"signal\":{\"revision\":1,\"observed_at\":\$ts,\"stale\":false,\"data\":{\"wan_status\":\"ipv4_ipv6_connected\"}}}}"
EOF
    cat >"$T/bin/logread" <<EOF
#!/bin/sh
cat $T/logread
EOF
    cat >"$T/bin/ps" <<EOF
#!/bin/sh
cat $T/ps
EOF
    cat >"$T/bin/kill" <<EOF
#!/bin/sh
echo "\$*" >>$T/kill.log
[ "\$1" = -9 ] && shift
for p in "\$@"; do
    [ "\$p" = "\$(cat $T/pids/luyaz-core.test 2>/dev/null)" ] && [ ! -f $T/unkillable ] && rm -f $T/pids/luyaz-core.test
done
exit 0
EOF
    # production datad: starts unless the live file is the new build and
    # $T/new-broken exists (a new version that cannot come up)
    cat >"$T/bin/initd" <<EOF
#!/bin/sh
echo "\$*" >>$T/initd.log
# hangs (until the step timeout kills it); meanwhile note the heartbeat file's
# inode twice: a new inode = the executor wrote a heartbeat while we hung
if [ -f $T/hang-initd-\$1 ]; then
    /bin/sleep 0.3; ls -i $T/tmp/heartbeat >>$T/hang.inodes
    /bin/sleep 1.5; ls -i $T/tmp/heartbeat >>$T/hang.inodes
    /bin/sleep 100
fi
if [ "\$1" = start ]; then
    if [ -f $T/new-broken ] && grep -q NEW $LIVE; then exit 1; fi
    echo 5555 >$T/pids/luyaz-core
fi
[ "\$1" = stop ] && rm -f $T/pids/luyaz-core
exit 0
EOF
    cat >"$T/bin/date" <<'EOF'
#!/bin/sh
echo "2026-09-30 12:00:00"
EOF
    cat >"$T/bin/setsid" <<EOF
#!/bin/sh
echo "\$*" >>$T/setsid.log
case "\$*" in *luyaz-core.test\ *) [ -f $T/test-broken ] || echo 4242 >$T/pids/luyaz-core.test ;; esac
case "\$*" in *u60-ship.sh\ run\ *) [ -f $T/exec-broken ] || "\$@" ;; esac
exit 0
EOF
    cat >"$T/bin/df" <<EOF
#!/bin/sh
echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'
echo "/dev/x 3000000 1000000 \$(cat $T/free-kb 2>/dev/null || echo 1100000) 40% /data"
EOF
    chmod +x "$T"/bin/*
    mkdir -p "$T/fake"
    printf '/bin/sleep 30\n' >"$T/fake/datad-trial.sh"
    printf '/bin/sleep 30\n' >"$T/fake/u60-ship.sh"

    export U60S_ROOT=$R U60S_TMP=$T/tmp U60S_BOOT_ID=$T/boot_id U60S_DF=$T/bin/df
    export U60S_MANIFEST=$R/data/luyaz/ship/manifest.jsonl
    unset U60S_CRASH_AT U60S_DIR U60S_LOG U60S_TEST_LOG U60S_MIN_FREE_KB
    export DT_INITD=$T/bin/initd DT_UPTIME=$T/uptime DT_PIDFILE=$T/trial.pid
    export DT_CURL=$T/bin/curl DT_PIDOF=$T/bin/pidof DT_PS=$T/bin/ps
    export DT_LOGREAD=$T/bin/logread DT_KILL=$T/bin/kill DT_SLEEP=$T/bin/sleep
    export DT_DATE=$T/bin/date DT_SETSID=$T/bin/setsid
    export DT_RC=$T/rc.local
    unset DT_TEST_BIN DT_LOG DT_TEST_LOG DT_WINDOW
    export U60R_ROOT=$R
    unset U60R_DIR U60R_BOOT_ID
    unset U60S_TO_INITD
}

teardown() {
    check "$CASE: nothing written outside the sandbox" '[ "$(leak_snapshot)" = "$LEAK0" ]'
    rm -rf "$T"
}

hook_at() { # hook_at <uptime> <shell command>
    cat >"$T/hook" <<EOF
[ \$(cut -d. -f1 $T/uptime) -ge $1 ] && { $2; }
:
EOF
}

# upload [txn] [extra meta lines]: what tools/luyaz puts into stage/<txn>/
upload() {
    _t=${1:-$TXN}
    _c=${_t##*-}
    _f=zwrt-datad
    mkdir -p "$R/data/luyaz/ship/stage/$_t"
    printf 'NEW-%s %s %s\n' "$_c" "$_t" "$MARKS" >"$R/data/luyaz/ship/stage/$_t/$_f"
    NEW=$(md5 "$R/data/luyaz/ship/stage/$_t/$_f")
    {
        echo v=1
        echo "txn=$_t"
        echo "comp=$_c"
        echo commit=7e8d223
        echo mac_time=1790000000
        echo format=1
        echo trial=60
        echo "file=$_f $NEW"
        [ -n "$2" ] && printf '%s\n' "$2"
    } >"$R/data/luyaz/ship/stage/$_t/meta"
}

stage() { sh "$SHIP" stage "${1:-$TXN}" >"$T/out" 2>&1; RC=$?; }
run() { timeout 60 sh "$SHIP" run "${1:-$TXN}" >"$T/out" 2>&1; RC=$?; }
phase() { sed -n 's/^phase=//p' "$R/data/luyaz/ship/txn" 2>/dev/null; }
logged() { grep -q "$1" "$R/data/luyaz/ship/ship.log"; }
mlines() { grep -c . "$U60S_MANIFEST" 2>/dev/null || echo 0; }
recover_newboot() {
    echo bbbb-2222 >"$T/newboot"
    U60R_BOOT_ID=$T/newboot sh "$RECOVER" >"$T/rout" 2>&1
    RRC=$?
}
tree() { find "$R" | sort; find "$R" -type f | sort | while read -r f; do md5sum "$f"; done; }

echo "== u60-ship =="

# ── stage refuses ───────────────────────────────────────────────────────────
refused() { # refused <case> <text in the message>
    CASE=$1
    check "$1: refused" '[ "$RC" = 1 ] && grep -q "拒绝暂存" "$T/out"'
    check "$1: says why" "grep -q -- '$2' '$T/out'"
    check "$1: live untouched, no side file" '[ "$(md5 "$LIVE")" = "$OLD" ] && [ ! -e "$LIVE.test" ]'
    check "$1: no service touched" '[ ! -s "$T/initd.log" ] && [ ! -s "$T/kill.log" ]'
}
nolog() { check "$CASE: no transaction written" '[ ! -f "$R/data/luyaz/ship/txn" ]'; }

setup
stage "../etc"
refused "bad txn id" "格式不对"
nolog
teardown

setup
upload 20260930-120000-foo
stage 20260930-120000-foo
refused "unknown component" "不认识"
nolog
teardown

# agent and web were removed in 8d-2c: refused by name, pointing at lweb
for rc_ in agent web; do
    setup
    upload 20260930-120000-$rc_
    stage 20260930-120000-$rc_
    refused "removed component $rc_" "组件 “$rc_” 已删除（8d-2c）.*u60 ship lweb"
    nolog
    teardown
done


setup
upload 20260930-120000-touch
stage 20260930-120000-touch
refused "touch before luyaz-fallback.sh is there" "/data/luyaz/scripts/luyaz-fallback.sh 不在：先 u60 ship scripts"
nolog
teardown

setup
mkdir -p "$R/data/luyaz/scripts" && : >"$R/data/luyaz/scripts/luyaz-fallback.sh"
upload 20260930-120000-touch
stage 20260930-120000-touch
refused "touch with another component's file" "file=zwrt-datad 不是组件 touch 的文件"
nolog
teardown

# not moved yet (no /data/luyaz/ship/.migrated): stage and prepare-rollback wait for move-d4
setup
rm -f "$R/data/luyaz/ship/.migrated"
upload
stage
refused "before move-d4: stage" "没有 .migrated）：先 move-d4"
nolog
sh "$SHIP" prepare-rollback datad 20260930-130000-datad 1790000000 >"$T/out" 2>&1; RC=$?
check "before move-d4: prepare-rollback refused" '[ "$RC" = 1 ] && grep -q "没有 .migrated）：先 move-d4" "$T/out" && [ ! -e "$R/data/luyaz/ship/stage/20260930-130000-datad" ]'
teardown

setup
upload
sed -i "s/^file=zwrt-datad .*/file=zwrt-datad 0123456789abcdef0123456789abcdef/" "$R/data/luyaz/ship/stage/$TXN/meta"
stage
refused "md5 of the upload wrong" "md5 不对"
nolog
teardown

setup
upload
rm "$R/data/luyaz/ship/stage/$TXN/zwrt-datad"
# (the meta already in the live name: else it is "no file=luyaz-core")
sed -i 's/^file=zwrt-datad /file=luyaz-core /' "$R/data/luyaz/ship/stage/$TXN/meta"
stage
refused "product missing" "暂存目录里没有 luyaz-core"
nolog
teardown

setup
upload
sed -i '/^file=/d' "$R/data/luyaz/ship/stage/$TXN/meta"
stage
refused "file= missing" "没有 file=luyaz-core"
nolog
teardown

setup
upload "" "file=evil 0123456789abcdef0123456789abcdef"
stage
refused "extra file" "file=evil 不是组件"
nolog
teardown

setup
upload
sed -i 's/^commit=.*/commit=xyz; rm -rf \//' "$R/data/luyaz/ship/stage/$TXN/meta"
stage
refused "bad commit" "提交号"
nolog
teardown

for bad in 'format=a' 'format=' 'mac_time=12a' 'trial=10' 'trial=08' 'trial=99999999'; do
    setup
    upload
    k=${bad%%=*}
    sed -i "s/^$k=.*/$bad/" "$R/data/luyaz/ship/stage/$TXN/meta"
    stage
    refused "bad $bad" "$k"
    nolog
    teardown
done

for bad in 'check=10' 'check=08' 'check=99999999'; do
    setup
    upload "" "$bad"
    stage
    refused "bad $bad" "check"
    nolog
    teardown
done

setup
upload "" "check=30"
sed -i 's/^trial=.*/trial=30/' "$R/data/luyaz/ship/stage/$TXN/meta"
stage
check "stage accepts trial=30 and check=30 (--fast)" '[ $RC = 0 ] && grep -qx "trial=30" "$R/data/luyaz/ship/txn" && grep -qx "check=30" "$R/data/luyaz/ship/txn"'
teardown

setup
upload "" "evil=1"
stage
refused "unknown meta key" "看不懂的一行"
nolog
teardown

setup
upload
sed -i 's/^v=1/v=2/' "$R/data/luyaz/ship/stage/$TXN/meta"
stage
refused "meta version" "v=1"
nolog
teardown

setup
upload 20260930-120000-datad
sed -i 's/^txn=.*/txn=20260930-120001-datad/' "$R/data/luyaz/ship/stage/$TXN/meta"
stage
refused "meta txn differs from dir" "和目录名"
nolog
teardown

for sp in /etc/passwd "$R/data/zwrt-datad/../luyaz/ship/txn" "$R/data/scenario/log" "$R/data/zwrt-datad/wifi" "$R/data/zwrt-datad/cooling.conf x"; do
    setup
    upload "" "state=$sp"
    stage
    refused "state out of bounds: ${sp#"$R"}" "不在组件 datad 的清单里"
    nolog
    teardown
done


setup
echo 100000 >"$T/free-kb"
upload
stage
refused "/data under 200 MB" "可用只有 97 MB"
nolog
teardown

setup
rm "$LIVE"
OLD=
upload
stage
refused "live file missing" "不存在（第一次安装走装机包）"
nolog
teardown

setup
echo 4242 >"$T/pids/luyaz-core.test"
upload
stage
refused "test build still running" "还在跑"
teardown

setup
"$T/fake/datad-trial.sh" &
W=$!
sh "$T/fake/datad-trial.sh" &
W=$!
echo $W >"$T/trial.pid"
upload
stage
refused "datad-trial watcher running" "datad-trial 的守护在跑"
nolog
kill $W 2>/dev/null
teardown

setup
printf 'v=1\ntxn=20260929-100000-datad\ncomp=datad\nphase=check\nend=1\n' >"$R/data/luyaz/ship/txn"
upload
stage
refused "second transaction" "还在进行：check"
teardown

setup
printf 'v=1\ntxn=20260929-100000-datad\ncomp=datad\nphase=failed\nreason=x\nend=1\n' >"$R/data/luyaz/ship/txn"
upload
stage
refused "failed transaction blocks" "停在 failed"
teardown

setup
printf 'garbage\n' >"$R/data/luyaz/ship/txn"
upload
stage
refused "unreadable transaction log blocks" "看不懂"
teardown

setup
echo '{"v":1,"kind":"ship","comp":"datad","txn":"20260901-000000-datad","format":1}' >"$U60S_MANIFEST"
upload
sed -i 's/^format=.*/format=2/' "$R/data/luyaz/ship/stage/$TXN/meta"
stage
refused "data format goes up" "数据格式版本升高（设备 1 → 新版 2）"
nolog
teardown

setup
echo '{"v":1,"kind":"ship","comp":"datad","txn":"20260901-000000-datad","format":3}' >"$U60S_MANIFEST"
upload
sed -i 's/^format=.*/format=2/' "$R/data/luyaz/ship/stage/$TXN/meta"
stage
refused "data format goes down" "数据格式版本降低"
nolog
teardown

setup
upload
( flock -x 9; "$_real_sleep" 3 ) 9>"$R/data/luyaz/ship/lock" &
L=$!
"$_real_sleep" 0.5
stage
refused "another stage holds the lock" "另一个 stage"
nolog
wait $L
teardown

setup
upload 20260930-120000-foo
sed -i 's/^comp=.*/comp=datad/' "$R/data/luyaz/ship/stage/20260930-120000-foo/meta"
stage 20260930-120000-foo
refused "txn not ending in the component" "不是以组件名"
nolog
teardown

setup
CASE="usage"
sh "$SHIP" stage >"$T/out" 2>&1
RC=$?
check "usage: stage without txn exits 2" '[ $RC = 2 ]'
sh "$SHIP" frobnicate >"$T/out" 2>&1
RC=$?
check "usage: unknown command exits 2" '[ $RC = 2 ] && grep -q "usage:" "$T/out"'
teardown

# ── the normal ship ─────────────────────────────────────────────────────────
setup
CASE="ship datad"
upload
stage
check "$CASE: staged" '[ "$RC" = 0 ] && [ "$(phase)" = staged ] && grep -q "已暂存" "$T/out"'
check "$CASE: side file in place, upload copy gone" '[ "$(md5 "$LIVE.test")" = "$NEW" ] && [ -x "$LIVE.test" ] && [ ! -e "$R/data/luyaz/ship/stage/$TXN/zwrt-datad" ]'
check "$CASE: staging touches nothing live" '[ "$(md5 "$LIVE")" = "$OLD" ] && [ ! -s "$T/initd.log" ]'
check "$CASE: boot id and md5s logged" "grep -qx 'boot_id=aaaa-1111' '$R/data/luyaz/ship/txn' && grep -qx 'file=$LIVE|$OLD|$NEW' '$R/data/luyaz/ship/txn'"
run
check "$CASE: run exit 0, done" '[ "$RC" = 0 ] && [ "$(phase)" = done ]'
check "$CASE: live is the new build" '[ "$(md5 "$LIVE")" = "$NEW" ] && [ -x "$LIVE" ]'
check "$CASE: previous version kept" '[ "$(md5 "$LIVE.prev-$TXN")" = "$OLD" ]'
check "$CASE: side file gone" '[ ! -e "$LIVE.test" ]'
check "$CASE: no state lines (the core has none since 10-06)" "! grep -q '^state' '$R/data/luyaz/ship/txn'"
check "$CASE: stop for the trial, start after promotion" '[ "$(tr "\n" " " <"$T/initd.log")" = "stop start " ]'
check "$CASE: test build started without supervise, then killed" "grep -q 'luyaz-core.test -i 1000' '$T/setsid.log' && ! grep -q supervise '$T/setsid.log' && grep -q 4242 '$T/kill.log'"
check "$CASE: trial ran the whole window" "logged '试跑通过（60s）'"
check "$CASE: manifest one line" '[ "$(mlines)" = 1 ]'
check "$CASE: manifest fields" "grep -q '\"kind\":\"ship\",\"comp\":\"datad\",\"txn\":\"$TXN\",\"commit\":\"7e8d223\",\"format\":1,\"mac_time\":1790000000,\"boot_id\":\"aaaa-1111\"' '$U60S_MANIFEST' && grep -q '\"path\":\"$LIVE\",\"md5\":\"$NEW\",\"prev\":\"$LIVE.prev-$TXN\"' '$U60S_MANIFEST'"
check "$CASE: stage dir removed" '[ ! -e "$R/data/luyaz/ship/stage/$TXN" ]'
check "$CASE: heartbeat names the transaction" "grep -q ' $TXN done\$' '$T/tmp/heartbeat'"
check "$CASE: hand-made neighbours untouched" '[ "$(cat "$D/luyaz-core.prev-cache-20260923")" = "keep $D/luyaz-core.prev-cache-20260923" ] && [ -f "$D/luyaz-core.orig-backup" ] && [ -f "$R/data/zte-agent.prev-esim-20260926" ]'
sh "$SHIP" status >"$T/st"
check "$CASE: status: done, observing" "grep -qx 'phase=done' '$T/st' && grep -q '^observe_left=[1-9][0-9][0-9]\$' '$T/st'"
echo bbbb-2222 >"$T/boot_id"
sh "$SHIP" status >"$T/st"
check "$CASE: status: no observation after a reboot" "! grep -q observe_left '$T/st'"
sh "$SHIP" wait 10 >"$T/st"
RC=$?
check "$CASE: wait returns at once on a terminal phase" '[ $RC = 0 ] && grep -qx "phase=done" "$T/st"'
teardown

# heartbeat: every fake sleep records the heartbeat's age and phase
setup
CASE="heartbeat"
upload
stage
cat >"$T/hook" <<EOF
read -r hb _p _t ph <$T/tmp/heartbeat 2>/dev/null && echo "\$ph \$(( \$(cut -d. -f1 $T/uptime) - hb ))" >>$T/hb.trace
:
EOF
run
check "$CASE: run done" '[ "$(phase)" = done ]'
check "$CASE: trial beats every second" '[ "$(grep -c "^trial " "$T/hb.trace")" -ge 60 ] && [ -z "$(awk "\$1 == \"trial\" && \$2 > 1" "$T/hb.trace")" ]'
check "$CASE: other phases at most every 5 s" '[ "$(grep -c "^check " "$T/hb.trace")" -ge 20 ] && [ -z "$(awk "\$1 != \"trial\" && \$2 > 5" "$T/hb.trace")" ]'
teardown

# ── the trial fails: the live slot was never touched ────────────────────────
setup
CASE="trial crash"
upload
stage
hook_at 1030 "rm -f $T/pids/luyaz-core.test"
run
check "$CASE: exit 1, aborted" '[ "$RC" = 1 ] && [ "$(phase)" = aborted ]'
check "$CASE: reason" "grep -q '^reason=试跑不过：测试版崩溃' '$R/data/luyaz/ship/txn'"
check "$CASE: live is the old build, no .prev" '[ "$(md5 "$LIVE")" = "$OLD" ] && [ ! -e "$LIVE.prev-$TXN" ]'
check "$CASE: production back" '[ "$(tr "\n" " " <"$T/initd.log")" = "stop start " ] && [ -f "$T/pids/luyaz-core" ]'
check "$CASE: manifest untouched" '[ "$(mlines)" = 0 ]'
check "$CASE: side file removed" '[ ! -e "$LIVE.test" ]'
teardown

setup
CASE="test build never up"
touch "$T/test-broken"
upload
stage
run
check "$CASE: aborted" '[ "$RC" = 1 ] && [ "$(phase)" = aborted ] && grep -q "^reason=测试版没起来（进程没出现）" "$R/data/luyaz/ship/txn"'
check "$CASE: live old, production back" '[ "$(md5 "$LIVE")" = "$OLD" ] && [ "$(tr "\n" " " <"$T/initd.log")" = "stop start " ]'
teardown

setup
CASE="test build unkillable"
touch "$T/unkillable"
upload
stage
hook_at 1030 "rm -f $T/pids/luyaz-ui"
run
check "$CASE: failed, production not started next to it" '[ "$RC" = 1 ] && [ "$(phase)" = failed ] && [ "$(tr "\n" " " <"$T/initd.log")" = "stop " ]'
check "$CASE: live old" '[ "$(md5 "$LIVE")" = "$OLD" ]'
upload 20260930-130000-datad
stage 20260930-130000-datad
check "$CASE: blocks the next stage" '[ "$RC" = 1 ] && grep -q "停在 failed" "$T/out"'
teardown

setup
CASE="rebooted between stage and run"
upload
stage
echo bbbb-2222 >"$T/boot_id"
run
check "$CASE: aborted, nothing touched" '[ "$(phase)" = aborted ] && [ "$(md5 "$LIVE")" = "$OLD" ] && [ ! -s "$T/initd.log" ] && [ ! -e "$LIVE.test" ]'
teardown

# ── the check after promotion fails: back to the old version ────────────────
setup
CASE="check fails"
upload
stage
hook_at 1100 "[ ! -f $T/frozen ] && touch $T/frozen"
run
check "$CASE: exit 1, rolled back" '[ "$RC" = 1 ] && [ "$(phase)" = rolledback ]'
check "$CASE: reason" "grep -q '^reason=转正后检查不过：屏幕数据停了' '$R/data/luyaz/ship/txn'"
check "$CASE: live is the old build again" '[ "$(md5 "$LIVE")" = "$OLD" ]'
check "$CASE: stop, start new, stop, start old" '[ "$(tr "\n" " " <"$T/initd.log")" = "stop start stop start " ]'
check "$CASE: manifest untouched" '[ "$(mlines)" = 0 ]'
teardown

setup
CASE="new version will not start"
touch "$T/new-broken"
upload
stage
run
check "$CASE: rolled back" '[ "$RC" = 1 ] && [ "$(phase)" = rolledback ] && grep -q "^reason=新版起不来" "$R/data/luyaz/ship/txn"'
check "$CASE: live old and running" '[ "$(md5 "$LIVE")" = "$OLD" ] && [ -f "$T/pids/luyaz-core" ]'
teardown

# a script error in the executor (bad arithmetic) still rolls back
setup
CASE="executor script error"
upload
stage
hook_at 1100 "echo 1100x >$T/uptime"
run
check "$CASE: rolled back by the EXIT trap" '[ "$(phase)" = rolledback ] && grep -q "^reason=执行器意外退出" "$R/data/luyaz/ship/txn"'
check "$CASE: live old" '[ "$(md5 "$LIVE")" = "$OLD" ]'
teardown

# ── manifest cannot be written ──────────────────────────────────────────────
setup
CASE="manifest write fails"
: >"$T/notadir"
export U60S_MANIFEST=$T/notadir/u60-manifest.jsonl
upload
stage
run
check "$CASE: exit 2, manifest_pending" '[ "$RC" = 2 ] && [ "$(phase)" = manifest_pending ]'
check "$CASE: live stays new" '[ "$(md5 "$LIVE")" = "$NEW" ]'
rm "$T/notadir"
mkdir "$T/notadir"
sh "$SHIP" manifest-finish >"$T/out" 2>&1
RC=$?
check "$CASE: manifest-finish completes it" '[ $RC = 0 ] && [ "$(phase)" = done ] && [ "$(mlines)" = 1 ]'
sh "$SHIP" manifest-finish >"$T/out" 2>&1
check "$CASE: manifest-finish again: no second line" '[ "$(mlines)" = 1 ]'
teardown

setup
CASE="manifest finished by the next stage"
: >"$T/notadir"
export U60S_MANIFEST=$T/notadir/u60-manifest.jsonl
upload
stage
run
rm "$T/notadir"
mkdir "$T/notadir"
upload 20260930-130000-datad
stage 20260930-130000-datad
check "$CASE: next stage ok" '[ "$RC" = 0 ] && [ "$(phase)" = staged ]'
check "$CASE: owed line written first" "grep -q '\"txn\":\"$TXN\"' '$U60S_MANIFEST' && [ \"\$(mlines)\" = 1 ]"
teardown

# ── .prev housekeeping ──────────────────────────────────────────────────────
setup
CASE="keep 3 ship .prev"
for d in 01 02 03 04; do
    echo "old $d" >"$LIVE.prev-202609${d}-000000-datad"
done
upload
stage
run
check "$CASE: done" '[ "$(phase)" = done ]'
check "$CASE: three newest program .prev left" '[ "$(ls "$D" | grep -c "^luyaz-core\.prev-2026")" = 3 ] && [ -f "$LIVE.prev-20260903-000000-datad" ] && [ -f "$LIVE.prev-20260904-000000-datad" ] && [ -f "$LIVE.prev-$TXN" ]'
check "$CASE: hand-made backups untouched" '[ -f "$D/luyaz-core.prev-cache-20260923" ] && [ -f "$D/luyaz-core.orig-backup" ] && [ -f "$R/data/zte-agent.prev-esim-20260926" ] && [ -f "$R/data/admin.prev-reset-20260926.tgz" ] && [ -f "$R/data/zte-agent.env" ]'
check "$CASE: pruning logged" "logged '删掉较早的上一版 .*luyaz-core.prev-20260901-000000-datad'"
teardown

# ── power cuts at every point, then the next boot ───────────────────────────
# crash <point>: stage, run with U60S_CRASH_AT; a check failure is arranged for
# rollback:* points. (No state files: the core has none since 10-06; the state
# snapshot/restore is covered by `selftest` below and scripts/test/u60-recover.)
crash() {
    setup
    CASE="power cut at $1"
    upload
    stage
    case "$1" in rollback:*)
        cat >>"$T/hook" <<EOF
[ \$(cut -d. -f1 $T/uptime) -ge 1100 ] && touch $T/frozen
:
EOF
        ;;
    esac
    U60S_CRASH_AT=$1 run
    check "$CASE: executor died there" '[ "$RC" = 137 ]'
    check "$CASE: live is whole (old or new)" '[ "$(md5 "$LIVE")" = "$OLD" ] || [ "$(md5 "$LIVE")" = "$NEW" ]'
    recover_newboot
    check "$CASE: recover exit 0" '[ "$RRC" = 0 ]'
}
after_boot() { # after_boot <phase> old|new
    _want=$1
    check "$CASE: phase $1 after boot" '[ "$(phase)" = "$_want" ]'
    if [ "$2" = old ]; then
        check "$CASE: live is the old build" '[ "$(md5 "$LIVE")" = "$OLD" ]'
    else
        check "$CASE: live is the new build" '[ "$(md5 "$LIVE")" = "$NEW" ]'
    fi
    tree >"$T/tree1"
    recover_newboot
    tree >"$T/tree2"
    check "$CASE: recover again changes nothing" 'cmp -s "$T/tree1" "$T/tree2"'
}

for p in trial:begin trial:state trial:running; do
    crash $p
    after_boot aborted old
    check "$CASE: reason" "grep -q '^reason=开机时已中止' '$R/data/luyaz/ship/txn'"
    teardown
done
for p in promote:begin promote:prev-copied:luyaz-core promote:prev-synced:luyaz-core promote:moved:luyaz-core check:begin rollback:begin rollback:files:luyaz-core; do
    crash $p
    after_boot rolledback old
    check "$CASE: reason" "grep -q '^reason=开机时已退回' '$R/data/luyaz/ship/txn'"
    check "$CASE: manifest untouched" '[ "$(mlines)" = 0 ]'
    teardown
done
for p in manifest:begin manifest:tmp manifest:moved; do
    crash $p
    after_boot manifest_pending new
    sh "$SHIP" manifest-finish >"$T/out" 2>&1
    check "$CASE: manifest-finish: done, exactly one line" '[ "$(phase)" = done ] && [ "$(mlines)" = 1 ]'
    teardown
done
crash manifest:written
after_boot manifest_pending new
teardown
# D4c: the same boot-time recovery for a transaction written as core
_txn0=$TXN TXN=20261016-120000-core
crash trial:running
after_boot aborted old
teardown
crash promote:moved:luyaz-core
after_boot rolledback old
check "$CASE (core): reason" "grep -q '^reason=开机时已退回' '$R/data/luyaz/ship/txn' && grep -qx 'comp=core' '$R/data/luyaz/ship/txn'"
teardown
crash manifest:moved
after_boot manifest_pending new
sh "$SHIP" manifest-finish >"$T/out" 2>&1
check "$CASE (core): manifest-finish writes one core line" '[ "$(phase)" = done ] && [ "$(mlines)" = 1 ] && grep -q "\"comp\":\"core\"" "$U60S_MANIFEST"'
teardown
TXN=$_txn0

# a power cut in the same boot is not u60-recover's business
setup
CASE="same boot"
upload
stage
U60S_CRASH_AT=check:begin run
U60R_BOOT_ID=$T/boot_id sh "$RECOVER" >"$T/rout" 2>&1
check "$CASE: recover leaves it to recover-live" '[ "$(phase)" = check ] && [ "$(md5 "$LIVE")" = "$NEW" ]'
teardown

# ── recover-live (same boot; the guard calls it in T4) ──────────────────────
setup
CASE="recover-live, executor alive"
upload
stage
U60S_CRASH_AT=check:begin run
sh "$T/fake/u60-ship.sh" &
W=$!
echo "$(cat "$T/uptime") $W $TXN check" >"$T/tmp/heartbeat"
sh "$SHIP" recover-live >"$T/out" 2>&1
RC=$?
check "$CASE: refuses with 3, nothing done" '[ $RC = 3 ] && [ "$(phase)" = check ] && [ "$(md5 "$LIVE")" = "$NEW" ]'
echo "$(($(cat "$T/uptime") - 40)) $W $TXN check" >"$T/tmp/heartbeat"
sh "$SHIP" recover-live >"$T/out" 2>&1
check "$CASE: stale heartbeat: hung executor killed, rolled back" '[ "$(phase)" = rolledback ] && [ ! -d /proc/$W ] && [ "$(md5 "$LIVE")" = "$OLD" ]'
teardown

setup
CASE="recover-live after kill -9 in check"
upload
stage
U60S_CRASH_AT=check:begin run
: >"$T/initd.log"
sh "$SHIP" recover-live >"$T/out" 2>&1
check "$CASE: rolled back" '[ "$(phase)" = rolledback ] && [ "$(md5 "$LIVE")" = "$OLD" ]'
check "$CASE: service restarted on the old build" '[ "$(tr "\n" " " <"$T/initd.log")" = "stop start " ]'
teardown

setup
CASE="recover-live after kill -9 in trial"
upload
stage
U60S_CRASH_AT=trial:running run
sh "$SHIP" recover-live >"$T/out" 2>&1
check "$CASE: aborted" '[ "$(phase)" = aborted ] && [ "$(md5 "$LIVE")" = "$OLD" ]'
check "$CASE: test build killed, production up" "grep -q 4242 '$T/kill.log' && [ ! -f '$T/pids/luyaz-core.test' ] && [ -f '$T/pids/luyaz-core' ]"
teardown

setup
CASE="recover-live after kill -9 in manifest"
upload
stage
U60S_CRASH_AT=manifest:begin run
sh "$SHIP" recover-live >"$T/out" 2>&1
check "$CASE: manifest written, done" '[ "$(phase)" = done ] && [ "$(mlines)" = 1 ] && [ "$(md5 "$LIVE")" = "$NEW" ]'
teardown

setup
CASE="recover-live with nothing to do"
sh "$SHIP" recover-live >"$T/out" 2>&1
RC=$?
check "$CASE: no transaction: exit 0" '[ $RC = 0 ] && [ ! -f "$R/data/luyaz/ship/txn" ]'
teardown

# ── start / abort ───────────────────────────────────────────────────────────
setup
CASE="start"
upload
stage
timeout 60 sh "$SHIP" start "$TXN" >"$T/out" 2>&1
RC=$?
check "$CASE: exit 0, executor up" '[ "$RC" = 0 ] && grep -q "执行器已\(起来\|接手\)" "$T/out"'
check "$CASE: detached executor (nohup sh …u60-ship.sh run)" "grep -q '^nohup sh /.*u60-ship.sh run $TXN' '$T/setsid.log'"
_i=0
while [ "$(phase)" != done ] && [ $_i -lt 300 ]; do "$_real_sleep" 0.1; _i=$((_i + 1)); done
check "$CASE: the detached executor finishes the transaction" '[ "$(phase)" = done ] && [ "$(md5 "$LIVE")" = "$NEW" ]'
teardown

setup
CASE="start, executor never comes up"
touch "$T/exec-broken"
upload
stage
sh "$SHIP" start "$TXN" >"$T/out" 2>&1
RC=$?
check "$CASE: exit 1, aborted, live untouched" '[ $RC = 1 ] && [ "$(phase)" = aborted ] && [ "$(md5 "$LIVE")" = "$OLD" ]'
teardown

setup
CASE="abort"
upload
stage
printf '#!/bin/sh\n/bin/sleep 0.05\n' >"$T/bin/sleep"
sh "$SHIP" run "$TXN" >"$T/out" 2>&1 &
W=$!
_i=0
while [ ! -s "$T/pids/luyaz-core.test" ] && [ $_i -lt 50 ]; do "$_real_sleep" 0.1; _i=$((_i + 1)); done
"$_real_sleep" 0.3
sh "$SHIP" abort >"$T/aout" 2>&1
wait $W
check "$CASE: executor finished the trial by itself" '[ "$(phase)" = aborted ] && grep -q "^reason=执行器被停止" "$R/data/luyaz/ship/txn"'
check "$CASE: live old, production back" '[ "$(md5 "$LIVE")" = "$OLD" ] && [ -f "$T/pids/luyaz-core" ] && [ ! -f "$T/pids/luyaz-core.test" ]'
teardown

setup
CASE="wait times out"
printf 'v=1\ntxn=%s\ncomp=datad\nphase=check\nend=1\n' "$TXN" >"$R/data/luyaz/ship/txn"
sh "$SHIP" wait 6 >"$T/out" 2>&1
RC=$?
check "$CASE: exit 4 with the status" '[ $RC = 4 ] && grep -qx "phase=check" "$T/out"'
teardown

# ── datad-trial and ship do not run at the same time ────────────────────────
setup
CASE="datad-trial during a transaction"
printf 'v=1\ntxn=%s\ncomp=datad\nphase=trial\nend=1\n' "$TXN" >"$R/data/luyaz/ship/txn"
printf '#!/bin/sh\n' >"$T/luyaz-core.test"
chmod +x "$T/luyaz-core.test"
printf '#!/bin/sh /etc/rc.common\n' >"$T/zwrt-datad.init"
DT_TEST_BIN=$T/luyaz-core.test DT_LOG=$T/trial.log sh "$SCRIPTS/datad-trial.sh" launch >"$T/out" 2>&1
RC=$?
check "$CASE: launch refused" '[ $RC = 1 ] && grep -q "拒绝启动" "$T/out" && grep -q "u60-ship 事务 $TXN（datad）还在进行" "$T/out"'
check "$CASE: production untouched" '[ ! -s "$T/initd.log" ] && [ ! -s "$T/setsid.log" ]'
teardown

setup
CASE="datad-trial without its engine"
mkdir "$T/alone"
cp "$SCRIPTS/datad-trial.sh" "$T/alone/"
sh "$T/alone/datad-trial.sh" status >"$T/out" 2>&1
RC=$?
check "$CASE: exit 1, says what is missing" '[ $RC = 1 ] && grep -q "找不到 .*u60-ship.sh" "$T/out"'
teardown

# ── T4: every blocking step has a timeout and beats meanwhile ───────────────
setup
CASE="init.d stop hangs"
export U60S_TO_INITD=2
touch "$T/hang-initd-stop"
upload
stage
run
check "$CASE: given up after the timeout, trial never started" '[ "$(phase)" = aborted ] && grep -q "^reason=试跑没开始：正式版 15s 内没有退出" "$R/data/luyaz/ship/txn"'
check "$CASE: logged the timeout" "logged '超时（2s），已放弃'"
check "$CASE: heartbeat written while the step hung" '[ "$(sort -u "$T/hang.inodes" | wc -l)" = 2 ]'
check "$CASE: live untouched" '[ "$(md5 "$LIVE")" = "$OLD" ]'
teardown

# ── T5 datad: every check, in the trial and after promotion ────────────────
# dcheck <name> <trial action> <check action> <reason in the trial> <reason after promotion>
dcheck() {
    _r4=$4
    _r5=$5
    setup
    CASE="datad trial: $1"
    upload
    stage
    _a=$(eval "printf '%s' \"$2\"")
    hook_at 1030 "[ -f $T/once ] || { touch $T/once; $_a; }"
    run
    check "$CASE: aborted" '[ "$RC" = 1 ] && [ "$(phase)" = aborted ] && grep -q "^reason=试跑不过：$_r4" "$R/data/luyaz/ship/txn"'
    check "$CASE: live old, production back" '[ "$(md5 "$LIVE")" = "$OLD" ] && [ -f "$T/pids/luyaz-core" ] && [ "$(mlines)" = 0 ]'
    teardown

    setup
    CASE="datad check: $1"
    upload
    stage
    _a=$(eval "printf '%s' \"$3\"")
    cat >"$T/hook" <<EOF
grep -qx phase=check $R/data/luyaz/ship/txn && [ ! -f $T/once ] && { touch $T/once; $_a; }
:
EOF
    run
    check "$CASE: rolled back" '[ "$RC" = 1 ] && [ "$(phase)" = rolledback ] && grep -q "^reason=转正后检查不过：$_r5" "$R/data/luyaz/ship/txn"'
    check "$CASE: live old, production back" '[ "$(md5 "$LIVE")" = "$OLD" ] && [ -f "$T/pids/luyaz-core" ] && [ "$(mlines)" = 0 ]'
    teardown
}
dcheck "ts stops" 'touch $T/frozen' 'touch $T/frozen' "屏幕数据停了" "屏幕数据停了"
dcheck "process gone" 'rm -f $T/pids/luyaz-core.test' 'rm -f $T/pids/luyaz-core' "测试版崩溃" "正式版消失"
dcheck "u60-uid gives up" 'echo u60-uid: giving up: 2 launches >>$T/logread' 'echo u60-uid: giving up: 2 launches >>$T/logread' "界面异常（u60-uid" "界面异常（u60-uid"
dcheck "UI crash line, old name" 'echo u60pro-devui pid 812 ended: exit 1 >>$T/logread' 'echo u60pro-devui pid 812 ended: exit 1 >>$T/logread' "界面异常（u60-uid" "界面异常（u60-uid"
dcheck "UI crash line, new name" 'echo luyaz-ui pid 812 ended: exit 1 >>$T/logread' 'echo luyaz-ui pid 812 ended: exit 1 >>$T/logread' "界面异常（u60-uid" "界面异常（u60-uid"
dcheck "screen UI gone" 'rm -f $T/pids/luyaz-ui' 'rm -f $T/pids/luyaz-ui' "界面异常（luyaz-ui 进程消失）" "界面异常（luyaz-ui 进程消失）"

# a normal u60-uid launch line ("… before giving up)") is not trouble
setup
CASE="datad: a u60-uid launch line in the trial"
upload
stage
hook_at 1030 "echo 'launched u60pro-devui pid 812 (attempt 1 of 2 before giving up)' >>$T/logread"
run
check "$CASE: done, not aborted" '[ "$RC" = 0 ] && [ "$(phase)" = done ]'
teardown

# ── record: files the manifest only records (T6), and doctor agreeing ─────
undocenv() {
    unset DOC_MANIFEST DOC_SHIP_TXN DOC_SHIP_HB DOC_MD5_CACHE DOC_TS_DIR DOC_TS_TUNING DOC_INITD DOC_RC DOC_BOOT_ID_FILE DOC_UPTIME DOC_DEVUI_DIR DOC_ROOT
    unset DOC_CHILL_DIR DOC_CHILL_TEMPLATE
}
docenv() { # doctor.sh reading this sandbox
    export DOC_MANIFEST=$U60S_MANIFEST DOC_SHIP_TXN=$R/data/luyaz/ship/txn DOC_SHIP_HB=$T/tmp/heartbeat \
        DOC_MD5_CACHE=$T/md5cache DOC_TS_DIR=$R/data/tailscale DOC_TS_TUNING=$R/data/tailscale/tuning.env \
        DOC_INITD=$R/etc/init.d DOC_RC=$R/etc/rc.local DOC_BOOT_ID_FILE=$T/boot_id DOC_UPTIME=$T/uptime \
        DOC_DEVUI_DIR=$R/data/plugins/u60pro-devui DOC_ROOT=$R
    export DOC_CHILL_DIR=$R/data/chill DOC_CHILL_TEMPLATE=$R/data/chill/template.yaml
}
setup
CASE="record"
mkdir -p "$R/data/tailscale"
printf '#!/bin/sh\nexit 0\n' >"$R/etc/rc.local"
echo 'TS_X=1' >"$R/data/tailscale/tuning.env"
RCM=$(md5 "$R/etc/rc.local")
sh "$SHIP" record rc.local 1790000000 'edited by hand "on purpose"' >"$T/out" 2>&1
RC=$?
check "$CASE: one line, current md5" "[ \$RC = 0 ] && [ \"\$(mlines)\" = 1 ] && grep -q '\"kind\":\"record\",\"name\":\"rc.local\",\"path\":\"$R/etc/rc.local\",\"md5\":\"$RCM\",\"mac_time\":1790000000' '$U60S_MANIFEST'"
check "$CASE: the reason kept, quotes dropped" "grep -q '\"why\":\"edited by hand on purpose\"}' '$U60S_MANIFEST'"
sh "$SHIP" record nonsense 1790000000 >"$T/out" 2>&1
RC=$?
check "$CASE: unknown name refused" '[ $RC = 1 ] && grep -q "不认识" "$T/out" && [ "$(mlines)" = 1 ]'
sh "$SHIP" record rc.local 12a >"$T/out" 2>&1
RC=$?
check "$CASE: bad time refused" '[ $RC = 1 ] && [ "$(mlines)" = 1 ]'
sh "$SHIP" record all 1790000001 >"$T/out" 2>&1
RC=$?
NREC=$(sh "$SHIP" record all 1790000002 2>/dev/null | grep -c 已记录)
check "$CASE: all = one line per listed file, absent ones as -" "[ \$RC = 0 ] && [ \"\$NREC\" -ge 9 ] && grep -q '\"name\":\"tailscaled\",\"path\":\"$R/data/tailscale/tailscaled\",\"md5\":\"-\"' '$U60S_MANIFEST'"
docenv
sh "$SCRIPTS/doctor.sh" --manifest >"$T/man"
check "$CASE: doctor knows exactly the names ship records" '[ -z "$(grep -c unrecorded "$T/man" | grep -v "^0$")" ] && [ "$(grep -c "	record:" "$T/man")" = "$NREC" ]'
check "$CASE: doctor: consistent" '[ "$(tail -n 1 "$T/man")" = "state	ok	一致（$NREC 项）" ]'
echo '# changed' >>"$R/etc/rc.local"
check "$CASE: doctor sees the edit" 'sh "$SCRIPTS/doctor.sh" --manifest | tail -n 1 | grep -q "^state	warn	不一致的是 rc.local"'
sh "$SHIP" record rc.local 1790000003 accepted >/dev/null 2>&1
check "$CASE: recorded again: consistent again" 'sh "$SCRIPTS/doctor.sh" --manifest | tail -n 1 | grep -q "^state	ok	一致"'
printf 'v=1\ntxn=%s\ncomp=datad\nphase=check\nend=1\n' "$TXN" >"$R/data/luyaz/ship/txn"
sh "$SHIP" record rc.local 1790000004 >"$T/out" 2>&1
RC=$?
check "$CASE: refused during a transaction" '[ $RC = 1 ] && grep -q 还在进行 "$T/out"'
undocenv
teardown

setup
CASE="retire (Phase 10 10c)"
printf '%s\n' '{"v":1,"kind":"ship","comp":"uid","txn":"20261001-043415-uid","commit":"489eebb","format":1,"files":[{"path":"'"$R"'/data/plugins/u60pro-devui/u60-uid","md5":"eac4d027eac4d027eac4d027eac4d027"}],"state":[]}' \
    '{"v":1,"kind":"record","name":"init.d/u60-uid","path":"'"$R"'/etc/init.d/u60-uid","md5":"052b9334052b9334052b9334052b9334"}' >>"$U60S_MANIFEST"
docenv
check "$CASE: before: the deleted files are inconsistent" 'sh "$SCRIPTS/doctor.sh" --manifest | grep -q "^differs	comp:uid"'
sh "$SHIP" retire 12a >"$T/out" 2>&1
RC=$?
check "$CASE: bad time refused" '[ $RC = 1 ]'
sh "$SHIP" retire 1790000000 >"$T/out" 2>&1
RC=$?
sh "$SCRIPTS/doctor.sh" --manifest >"$T/man"
check "$CASE: one retire line; doctor no longer checks uid nor its init record" '[ $RC = 0 ] && [ "$(grep -c "\"kind\":\"retire\"" "$U60S_MANIFEST")" = 1 ] && ! grep -q "comp:uid\|record:init.d/u60-uid" "$T/man" && ! grep -q "^differs" "$T/man"'
undocenv
teardown

setup
CASE="retire after the 10e move"
printf '%s\n' '{"v":1,"kind":"record","name":"init.d/zwrt-datad","path":"'"$R"'/etc/init.d/zwrt-datad","md5":"4ea6f56a4ea6f56a4ea6f56a4ea6f56a"}' >>"$U60S_MANIFEST"
docenv
sh "$SHIP" retire 1790000001 >"$T/out" 2>&1
RC=$?
check "$CASE: retires it" '[ $RC = 0 ] && tail -n 1 "$U60S_MANIFEST" | grep -q "init.d/zwrt-datad"'
undocenv
teardown

setup
CASE="ship, then doctor"
upload
stage
run
docenv
sh "$SCRIPTS/doctor.sh" --manifest >"$T/man"
check "$CASE: the shipped file is in the manifest and matches" "grep -qx \"same	comp:datad	$LIVE	$NEW	$NEW\" '$T/man'"
check "$CASE: observing for 10 minutes" 'tail -n 1 "$T/man" | grep -q "^state	ok	观察中（还剩 10 分钟）：datad 刚上机；一致（1 项）"'
undocenv
teardown

# ── prepare-rollback (u60 rollback) and the note in the manifest ──────────
setup
CASE="rollback"
T1=20260930-120000-datad T2=20260930-130000-datad T3=20260930-140000-datad
upload $T1; stage $T1; run $T1; A=$NEW
upload $T2 "note=requires-ignored"; sed -i 's/^commit=.*/commit=abcdef1/' "$R/data/luyaz/ship/stage/$T2/meta"; stage $T2; run $T2; B=$NEW
check "$CASE: two ships done, the second noted" '[ "$(phase)" = done ] && [ "$(md5 "$LIVE")" = "$B" ] && tail -n 1 "$U60S_MANIFEST" | grep -q "\"state\":\[.*\],\"note\":\"requires-ignored\"}$"'
sh "$SHIP" prepare-rollback datad $T3 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: prepared from the last ship's .prev" "[ \$RC = 0 ] && [ \"\$(md5 '$R/data/luyaz/ship/stage/$T3/luyaz-core')\" = '$A' ] && grep -qx 'commit=7e8d223' '$R/data/luyaz/ship/stage/$T3/meta' && grep -qx 'note=rollback' '$R/data/luyaz/ship/stage/$T3/meta'"
stage $T3
run $T3
check "$CASE: an ordinary transaction back to the first build" '[ "$(phase)" = done ] && [ "$(md5 "$LIVE")" = "$A" ] && tail -n 1 "$U60S_MANIFEST" | grep -q "\"commit\":\"7e8d223\".*\"note\":\"rollback\"}$"'
teardown

setup
CASE="rollback after one ship"
upload; stage; run
sh "$SHIP" prepare-rollback datad 20260930-140000-datad 1790000100 >"$T/out" 2>&1
check "$CASE: the hand-installed version, commit unknown" "[ \"\$(md5 '$R/data/luyaz/ship/stage/20260930-140000-datad/luyaz-core')\" = '$OLD' ] && grep -qx 'commit=0000000' '$R/data/luyaz/ship/stage/20260930-140000-datad/meta'"
rm -f "$LIVE.prev-$TXN"
sh "$SHIP" prepare-rollback datad 20260930-150000-datad 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: .prev gone: refused, nothing staged" '[ $RC = 1 ] && grep -q "不在了" "$T/out" && [ ! -e "$R/data/luyaz/ship/stage/20260930-150000-datad" ]'
teardown

setup
CASE="rollback refusals"
sh "$SHIP" prepare-rollback datad 20260930-140000-datad 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: never shipped: refused" '[ $RC = 1 ] && grep -q "没有 datad 上机的记录" "$T/out"'
sh "$SHIP" prepare-rollback datad 20260930-140000-agent 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: txn of another component: refused" '[ $RC = 1 ]'
sh "$SHIP" prepare-rollback agent 20260930-140000-agent 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: removed component (agent): refused by name" '[ $RC = 1 ] && grep -q "已删除（8d-2c）" "$T/out"'
upload "" "note=please"
stage
refused "bad note" "note “please” 看不懂"
teardown

# ── 10e: the core's layout (/data/luyaz/core/.migrated, set up by setup) ─────
setup
CASE="new layout: ship core"
TC=20261014-120000-core
upload $TC
stage $TC
check "$CASE: staged as core (D4c), the artifact taken as luyaz-core" '[ "$RC" = 0 ] && grep -q "已暂存 $TC（core" "$T/out" && grep -q "^file=luyaz-core " "$R/data/luyaz/ship/stage/$TC/meta"'
run $TC
check "$CASE: done, /data/luyaz/luyaz-core is the new build" '[ "$(phase)" = done ] && [ "$(md5 "$LIVE")" = "$NEW" ]'
check "$CASE: the trial ran as luyaz-core.test from /data/luyaz" 'grep -q "/data/luyaz/luyaz-core.test" "$T/setsid.log"'
check "$CASE: manifest says comp core, path /data/luyaz/luyaz-core" "tail -n 1 '$U60S_MANIFEST' | grep -q '\"comp\":\"core\".*\"path\":\"$LIVE\"'"
teardown

setup
echo 6666 >"$T/pids/luyaz-core.test"
upload
stage
refused "a luyaz-core.test running" "测试版 luyaz-core.test 还在跑"
teardown

setup
CASE="new layout: rollback floor"
printf '%s\n' '{"v":1,"kind":"ship","comp":"datad","txn":"20260930-120000-datad","commit":"7e8d223","format":1,"files":[{"path":"'"$R"'/data/plugins/zwrt-datad/zwrt-datad","md5":"'"$OLD"'","prev":"'"$R"'/data/plugins/zwrt-datad/zwrt-datad.prev-20260930-120000-datad"}],"state":[]}' >>"$U60S_MANIFEST"
sh "$SHIP" prepare-rollback datad 20261014-130000-datad 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: the last ship was to the old path: refused, says no way back" '[ $RC = 1 ] && grep -q "不能退回旧布局" "$T/out" && [ ! -e "$R/data/luyaz/ship/stage/20261014-130000-datad" ]'
T2=20261014-140000-datad
upload $T2; stage $T2; run $T2; B=$NEW
T3=20261014-150000-datad
upload $T3; stage $T3; run $T3
sh "$SHIP" prepare-rollback core 20261014-160000-datad 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: core with a txn ending -datad: refused (D4c: the txn ends in the name sent)" '[ $RC = 1 ] && grep -q "不是以组件名 core 结尾" "$T/out"'
sh "$SHIP" prepare-rollback core 20261014-160000-core 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: after a ship into the new layout: prepared as core from datad ships" "[ \$RC = 0 ] && [ \"\$(md5 '$R/data/luyaz/ship/stage/20261014-160000-core/luyaz-core')\" = '$B' ] && grep -qx 'comp=core' '$R/data/luyaz/ship/stage/20261014-160000-core/meta'"
teardown

# ── D4c: the core written "core", read under both names ───────────────────
setup
CASE="D4c core name"
T1=20260930-120000-datad T2=20261016-120000-core
upload $T1; stage $T1; run $T1; A=$NEW
upload $T2; stage $T2
check "$CASE: a -core txn stages after datad ships" '[ $RC = 0 ] && [ "$(phase)" = staged ]'
run $T2; B=$NEW
check "$CASE: done; the manifest says core" '[ "$(phase)" = done ] && [ "$(md5 "$LIVE")" = "$B" ] && tail -n 1 "$U60S_MANIFEST" | grep -q "\"comp\":\"core\",\"txn\":\"$T2\""'
check "$CASE: the previous version kept as .prev-<txn>-core" '[ "$(md5 "$LIVE.prev-$T2")" = "$A" ]'
sh "$SHIP" prepare-rollback core 20261016-130000-core 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: rollback as core finds the core line, back to the datad-era build" "[ \$RC = 0 ] && [ \"\$(md5 '$R/data/luyaz/ship/stage/20261016-130000-core/luyaz-core')\" = '$A' ]"
rm -rf "$R/data/luyaz/ship/stage/20261016-130000-core"
sh "$SHIP" prepare-rollback datad 20261016-140000-datad 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: as datad (an older tools/u60) the same target" "[ \$RC = 0 ] && [ \"\$(md5 '$R/data/luyaz/ship/stage/20261016-140000-datad/luyaz-core')\" = '$A' ]"
check "$CASE: the executor carries its D4c line (tools/luyaz probes it)" "grep -qx CORE_COMP=core '$SHIP'"
teardown

# ── record-kit: the install kit's own line (T9) ────────────────────────────
setup
CASE="record-kit"
AM=$(md5 "$LIVE")
sh "$SHIP" record-kit datad 20261001 abc1234def 1 1790000000 >"$T/out" 2>&1
RC=$?
check "$CASE: a kit line with the live md5" "[ \$RC = 0 ] && grep -q '^{\"v\":1,\"kind\":\"kit\",\"comp\":\"datad\",\"kit\":\"20261001\",\"commit\":\"abc1234def\",\"format\":1,\"mac_time\":1790000000,' '$U60S_MANIFEST' && grep -q '\"files\":\[{\"path\":\"$LIVE\",\"md5\":\"$AM\"}\],\"state\":\[\]}\$' '$U60S_MANIFEST'"
sh "$SHIP" record-kit datad 20261001 abc1234def 1 1790000000 dirty >"$T/out" 2>&1
check "$CASE: dirty kit noted" 'tail -n 1 "$U60S_MANIFEST" | grep -q ",\"note\":\"kit-dirty\"}$"'
for badargs in "nope 20261001 abc1234 1 1790000000" "datad 2026 abc1234 1 1790000000" "datad 20261001 xyz 1 1790000000" "datad 20261001 abc1234 a 1790000000" "datad 20261001 abc1234 1 12" "datad 20261001 abc1234 1 1790000000 clean"; do
    sh "$SHIP" record-kit $badargs >"$T/out" 2>&1
    RC=$?
    check "$CASE: refused: $badargs" '[ $RC = 1 ] && [ "$(mlines)" = 2 ]'
done
sh "$SHIP" record-kit agent 20261001 abc1234 1 1790000000 >"$T/out" 2>&1
RC=$?
check "$CASE: removed component (agent): refused by name" '[ $RC = 1 ] && grep -q "已删除（8d-2c）" "$T/out" && [ "$(mlines)" = 2 ]'
docenv
check "$CASE: doctor reads kit lines" 'sh "$SCRIPTS/doctor.sh" --manifest | grep -q "^same	comp:datad	$LIVE	$AM	$AM"'
undocenv
echo '{"v":1,"kind":"ship","comp":"datad","txn":"20260929-100000-datad","commit":"1111111","format":3,"files":[],"state":[]}' >"$U60S_MANIFEST"
sh "$SHIP" record-kit datad 20261001 abc1234 2 1790000000 >/dev/null 2>&1
upload 20260930-120000-datad
sed -i 's/^format=.*/format=2/' "$R/data/luyaz/ship/stage/20260930-120000-datad/meta"
stage 20260930-120000-datad
check "$CASE: the device's format is the kit's when it came last" '[ "$RC" = 0 ]'
rm -f "$R/data/luyaz/ship/txn"
sh "$SHIP" prepare-rollback datad 20260930-130000-datad 1790000100 >"$T/out" 2>&1
RC=$?
check "$CASE: rollback refused when the kit came last (no ship .prev)" '[ $RC = 1 ] && grep -q "装机包装的" "$T/out"'
printf 'v=1\ntxn=%s\ncomp=datad\nphase=check\nend=1\n' "$TXN" >"$R/data/luyaz/ship/txn"
sh "$SHIP" record-kit datad 20261001 abc1234 1 1790000000 >"$T/out" 2>&1
RC=$?
check "$CASE: refused during a transaction" '[ $RC = 1 ] && grep -q 还在进行 "$T/out"'
teardown

# ── selftest (what runs on the device before a new u60-ship.sh goes live) ───
setup
CASE="selftest"
unset U60S_ROOT U60S_TMP U60S_BOOT_ID U60S_DF U60S_MANIFEST DT_SLEEP DT_SETSID
timeout 60 sh "$SHIP" selftest >"$T/out" 2>&1
RC=$?
check "$CASE: PASS" '[ $RC = 0 ] && grep -q "selftest: PASS" "$T/out"'
teardown

# ── X11: a one-shot `<core> doctor` (comm luyaz-doctor, argv0/exe still the core's) ──
setup
CASE="pidof skips doctor"
mkdir -p "$T/proc/11" "$T/proc/12"
printf '/data/luyaz/luyaz-core\0' >"$T/proc/11/cmdline"
printf '/data/luyaz/luyaz-core\0doctor\0--tsv2\0' >"$T/proc/12/cmdline"
printf '#!/bin/sh\necho "$PIDS"\n' >"$T/bin/pidof2"; chmod +x "$T/bin/pidof2"
eval "$(sed -n '/^pidof_nodoctor() {/,/^}/p' "$SHIP")"
OUT=$(PROCDIR=$T/proc PIDOF_RAW=$T/bin/pidof2 PIDS="11 12" pidof_nodoctor luyaz-core)
check "$CASE: the core stays, the doctor is dropped" '[ "$OUT" = 11 ]'
OUT=$(PROCDIR=$T/proc PIDOF_RAW=$T/bin/pidof2 PIDS="12" pidof_nodoctor luyaz-core); RC=$?
check "$CASE: only a doctor = nothing, exit 1" '[ -z "$OUT" ] && [ $RC = 1 ]'
OUT=$(PROCDIR=$T/proc PIDOF_RAW=$T/bin/pidof2 PIDS="11 99" pidof_nodoctor luyaz-core)
check "$CASE: a pid gone meanwhile still counts (as pidof said)" '[ "$OUT" = "11 99" ]'
teardown

# ── Phase 10 R4/R5: the touch UI is luyaz-ui only since round ③, the screen state in both dirs ──
setup
CASE="ui name"
mkdir -p "$T/proc/21" "$T/proc/22" "$T/proc/23"
echo "21 (u60pro-devui) S" >"$T/proc/21/stat"; echo "22 (luyaz-ui) S" >"$T/proc/22/stat"; echo "23 (luyaz-ui) Z" >"$T/proc/23/stat"
printf '#!/bin/sh\ncase $1 in u60pro-devui) echo "$OLD" ;; luyaz-ui) echo "$NEW" ;; esac\n' >"$T/bin/pidof3"; chmod +x "$T/bin/pidof3"
eval "$(sed -n '/^live_pid() {/,/^}/p; /^ui_pid() {/,/^}/p; /^uid_clear() {/p' "$SHIP")"
OUT=$(PROCDIR=$T/proc PIDOF=$T/bin/pidof3 OLD=21 NEW= ui_pid)
check "$CASE: the old name no longer counts (round ③)" '[ -z "$OUT" ]'
OUT=$(PROCDIR=$T/proc PIDOF=$T/bin/pidof3 OLD= NEW="23 22" ui_pid)
check "$CASE: new name found, its zombie skipped" '[ "$OUT" = 22 ]'
OUT=$(PROCDIR=$T/proc PIDOF=$T/bin/pidof3 OLD= NEW=23 ui_pid)
check "$CASE: only a zombie = no UI" '[ -z "$OUT" ]'
mkdir -p "$T/old" "$T/new"; for d in old new; do echo 2 >"$T/$d/attempts"; echo 1 >"$T/$d/gave-up"; done
UID_STATE=$T/old UID_STATE_NEW=$T/new uid_clear
check "$CASE: launch count and give-up cleared in both dirs" '[ -z "$(find "$T/old" "$T/new" -type f)" ]'
check "$CASE: the kill lines take luyaz-ui only" '[ "$(grep -c "KILL.* \$(\$PIDOF luyaz-ui 2>/dev/null) 2>/dev/null$" "$SHIP")" = 2 ] && ! grep -q "PIDOF u60pro-devui" "$SHIP"'
teardown

# rc_retired (restore_one's rc.local refusal): every retired start counts, in any shape
setup
CASE="rc_retired"
LEAK0=$(leak_snapshot)
sed -n '/^rc_retired() {/,/^}/p' "$SHIP" >"$T/rr.sh"
rr() { printf '#!/bin/sh\n%s\nexit 0\n' "$1" >"$T/rc"; ( . "$T/rr.sh"; rc_retired "$T/rc" ); }
check "$CASE: plain uid + guard" '[ "$(rr "/etc/init.d/u60-uid start
/etc/init.d/u60-guard start")" = "u60-uid u60-guard" ]'
check "$CASE: indented, conditional, chained, with &" '[ "$(rr "  /etc/init.d/u60-uid start")" = u60-uid ] && [ "$(rr "[ -x /etc/init.d/u60-guard ] && /etc/init.d/u60-guard start")" = u60-guard ] && [ "$(rr "true; /etc/init.d/zwrt-datad start &")" = zwrt-datad ]'
check "$CASE: a comment line is not a start" '[ -z "$(rr "  # /etc/init.d/u60-uid start")" ]'
check "$CASE: the current services are not retired" '[ -z "$(rr "/etc/init.d/luyaz-core start
/etc/init.d/luyaz-sentinel start")" ]'
check "$CASE: missing or a symlink: unreadable" '[ "$( . "$T/rr.sh"; rc_retired "$T/nope")" = unreadable ] && ln -s "$T/rc" "$T/rcl" && [ "$( . "$T/rr.sh"; rc_retired "$T/rcl")" = unreadable ]'
check "$CASE: restore_one asks it, not only rc_mode" "sed -n '/^restore_one() {/,/^}/p' '$SHIP' | grep -q 'rc_retired \"\$_rprev\"'"
teardown

echo "u60-ship: $PASS passed, $FAIL failed"
[ "$FAIL" = 0 ]
