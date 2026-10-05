#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# A fake-device test of the kit's u60 ship pieces (onboard/device/install.sh):
# the `recover` component (u60-recover.sh into /data/u60-ship after its own
# selftest, and the rc.local line before the service start lines) and
# place_recover (what admin/devui do: put it there only when it is missing),
# kit_record (agent; touch, uid), uid_restart (devui) and the check for an
# unfinished u60 ship transaction (txn v=1 and v=2).
# Every path is moved into a temp dir (KIT_RC, KIT_STATE, KIT_SHIP_DIR,
# KIT_GUARD); nothing touches a device. In the device's busybox:
#
#   docker run --rm -v "$PWD/onboard":/onboard:ro -v <touch-ui>/scripts:/scripts:ro \
#       u60-device-busybox:20260928 sh /onboard/test/device-recover.sh
#
# The full install still needs the real-device run (onboard/test/sandbox.sh).
# SPDX-License-Identifier: MIT
# ─────────────────────────────────────────────────────────────────────────────

ONB=${ONB:-/onboard}
SCRIPTS=${SCRIPTS:-/scripts}
PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); echo "  ok   $1"; }
bad() { FAIL=$((FAIL + 1)); echo "  FAIL $1"; [ -f "$T/out" ] && sed 's/^/       | /' "$T/out" | tail -n 8; }
check() { _d=$1; shift; if eval "$@"; then ok "$_d"; else bad "$_d  [$*]"; fi; }
md5() { md5sum "$1" 2>/dev/null | cut -d' ' -f1; }

setup() {
    T=$(mktemp -d)
    K=$T/kit
    mkdir -p "$K/device" "$K/payload/guard" "$T/data"
    cp "$ONB/device/install.sh" "$K/device/"
    cp "$SCRIPTS/u60-recover.sh" "$K/payload/guard/"
    cat >"$T/rc.local" <<'EOF'
#!/bin/sh
/data/local/tmp/start_dropbear.sh &
/etc/init.d/zte-agent start
/etc/init.d/zwrt-datad start
/etc/init.d/u60-guard start
/etc/init.d/u60-uid start
/etc/init.d/zte_topsw_get_brand stop
exit 0
EOF
    export KIT_RC=$T/rc.local KIT_STATE=$T/data/u60-kit KIT_SHIP_DIR=$T/data/u60-ship KIT_GUARD=$T/data/u60-guard
}
inst() { sh "$K/device/install.sh" "$@" >"$T/out" 2>&1; RC=$?; }
# the same rule tools/u60 checks before a first ship
order() {
    awk '/^[[:space:]]*#/ { next } /u60-recover\.sh/ && !r { r = NR } /\/etc\/init\.d\/(zte-agent|zwrt-datad|u60-guard|u60-uid)[[:space:]]+start/ && !s { s = NR } END { if (!r) print "missing"; else if (s && s < r) print "late"; else print "ok" }' "$T/rc.local"
}

echo "== kit: u60-recover =="

setup
inst recover
check "recover: done" '[ $RC = 0 ]'
check "recover: the script in /data/u60-ship, same as the kit's, executable" '[ "$(md5 "$T/data/u60-ship/u60-recover.sh")" = "$(md5 "$K/payload/guard/u60-recover.sh")" ] && [ -x "$T/data/u60-ship/u60-recover.sh" ]'
check "recover: rc.local line right before the first service start" '[ "$(sed -n 3p "$T/rc.local" | cut -d" " -f1-2)" = "sh $T/data/u60-ship/u60-recover.sh" ] && [ "$(sed -n 4p "$T/rc.local")" = "/etc/init.d/zte-agent start" ]'
check "recover: the order tools/u60 wants" '[ "$(order)" = ok ]'
check "recover: rc.local still parses, the rest as it was" 'sh -n "$T/rc.local" && [ "$(grep -v u60-recover "$T/rc.local" | md5sum)" = "$(md5sum <"$T/data/u60-kit/rc.local.orig")" ]'
check "recover: the original rc.local backed up" '[ -f "$T/data/u60-kit/rc.local.orig" ]'
cp "$T/rc.local" "$T/rc.1"
inst recover
check "recover again: rc.local unchanged" '[ $RC = 0 ] && cmp -s "$T/rc.local" "$T/rc.1" && grep -q "已有调用" "$T/out"'
rm -rf "$T"

setup
printf '#!/bin/sh\n/data/local/tmp/start_dropbear.sh &\nexit 0\n' >"$T/rc.local"
inst recover
check "no service lines yet: before exit 0" '[ $RC = 0 ] && [ "$(tail -n 1 "$T/rc.local")" = "exit 0" ] && [ "$(order)" = ok ]'
rm -rf "$T"

setup
printf '#!/bin/sh\n# /etc/init.d/zte-agent start  (old, commented)\n/etc/init.d/u60-uid start\nexit 0\n' >"$T/rc.local"
inst recover
check "a commented service line is not the first one" '[ "$(sed -n 3p "$T/rc.local" | cut -d" " -f1)" = sh ] && [ "$(order)" = ok ]'
rm -rf "$T"

setup
printf '#!/bin/sh\necho selftest: FAIL\nexit 1\n' >"$K/payload/guard/u60-recover.sh"
cp "$T/rc.local" "$T/rc.0"
inst recover
check "a kit u60-recover.sh that fails its selftest: refused, nothing changed" '[ $RC != 0 ] && grep -q "selftest 不过" "$T/out" && [ ! -e "$T/data/u60-ship/u60-recover.sh" ] && cmp -s "$T/rc.local" "$T/rc.0"'
rm -rf "$T"

setup
mkdir -p "$T/data/u60-ship"
echo 'older one' >"$T/data/u60-ship/u60-recover.sh"
inst recover
check "recover replaces an older one, keeps it aside" '[ $RC = 0 ] && [ "$(md5 "$T/data/u60-ship/u60-recover.sh")" = "$(md5 "$K/payload/guard/u60-recover.sh")" ] && grep -q "older one" "$T/data/u60-ship/u60-recover.sh.prev-kit"'
rm -rf "$T"

# place_recover (admin / devui): only when missing; never touches rc.local
setup
sed -n '/^recover_check() {/,/^do_recover() {/p' "$K/device/install.sh" | sed '$d' >"$T/fns.sh"
place() { (
    P=$K/payload SHIPD=$KIT_SHIP_DIR
    log() { echo "[device] $*"; }
    warn() { echo "[device] 注意: $*"; }
    die() { echo "[device] 失败: $*"; exit 1; }
    . "$T/fns.sh"
    place_recover
) >"$T/out" 2>&1; RC=$?; }
cp "$T/rc.local" "$T/rc.0"
place
check "admin/devui: missing → put there, rc.local untouched" '[ $RC = 0 ] && [ -f "$T/data/u60-ship/u60-recover.sh" ] && cmp -s "$T/rc.local" "$T/rc.0" && grep -q "install.sh recover 才加" "$T/out"'
echo 'a different one' >"$T/data/u60-ship/u60-recover.sh"
place
check "admin/devui: a different one already there → not replaced, said so" '[ $RC = 0 ] && grep -q "a different one" "$T/data/u60-ship/u60-recover.sh" && grep -q "没换" "$T/out"'
rm -rf "$T"

# kit_record (after admin): kit-source → one kind=kit line through u60-ship.sh
setup
mkdir -p "$T/root/data" "$T/data/u60-guard"
cp "$SCRIPTS/u60-ship.sh" "$T/data/u60-guard/"
echo 'agent build' >"$T/root/data/zte-agent"
printf 'stamp=20261001\nagent_commit=abc1234def\nagent_format=1\nagent_dirty=1\n' >"$K/payload/guard/kit-source"
sed -n '/^kit_record() {/,/^}/p' "$K/device/install.sh" >"$T/fns.sh"
rec() { (
    P=$K/payload G=$T/data/u60-guard
    export U60S_ROOT=$T/root U60S_TMP=$T/tmp KIT_MAC_TIME=1790000000
    log() { echo "[device] $*"; }
    warn() { echo "[device] 注意: $*"; }
    . "$T/fns.sh"
    kit_record agent
) >"$T/out" 2>&1; RC=$?; }
rec
check "kit_record: one kit line from kit-source, dirty noted" "[ \$RC = 0 ] && grep -q '\"kind\":\"kit\",\"comp\":\"agent\",\"kit\":\"20261001\",\"commit\":\"abc1234def\",\"format\":1,\"mac_time\":1790000000' '$T/root/data/u60-manifest.jsonl' && grep -q 'kit-dirty' '$T/root/data/u60-manifest.jsonl' && grep -q '设备清单: 记下装机包 20261001' '$T/out'"
rm "$K/payload/guard/kit-source"
rec
check "kit_record: an old kit without kit-source: warned, install goes on" '[ $RC = 0 ] && grep -q "没记进设备清单" "$T/out" && [ "$(wc -l <"$T/root/data/u60-manifest.jsonl")" = 1 ]'
rm -rf "$T"

# kit_record datad (after devui) through the real u60-ship.sh: its own keys in
# kit-source; a kit-source without datad_* keys (a kit from before them) still
# records it, as commit 0000000 and dirty
setup
mkdir -p "$T/root/data/plugins/zwrt-datad" "$T/data/u60-guard"
cp "$SCRIPTS/u60-ship.sh" "$T/data/u60-guard/"
echo 'datad build' >"$T/root/data/plugins/zwrt-datad/zwrt-datad"
sed -n '/^kit_record() {/,/^}/p' "$K/device/install.sh" >"$T/fns.sh"
recd() { (
    P=$K/payload G=$T/data/u60-guard
    export U60S_ROOT=$T/root U60S_TMP=$T/tmp KIT_MAC_TIME=1790000000
    log() { echo "[device] $*"; }
    warn() { echo "[device] 注意: $*"; }
    . "$T/fns.sh"
    kit_record datad
) >"$T/out" 2>&1; RC=$?; }
printf 'stamp=20261004\nagent_commit=abc1234\nagent_format=1\nagent_dirty=0\n' >"$K/payload/guard/kit-source"
recd
check "kit_record datad, kit-source without datad_* keys: recorded as 0000000, dirty" "[ \$RC = 0 ] && grep -q '\"kind\":\"kit\",\"comp\":\"datad\",\"kit\":\"20261004\",\"commit\":\"0000000\",\"format\":1,' '$T/root/data/u60-manifest.jsonl' && grep -q '\"path\":\"$T/root/data/plugins/zwrt-datad/zwrt-datad\",\"md5\":\"$(md5 "$T/root/data/plugins/zwrt-datad/zwrt-datad")\"' '$T/root/data/u60-manifest.jsonl' && grep -q 'kit-dirty' '$T/root/data/u60-manifest.jsonl'"
: >"$T/root/data/u60-manifest.jsonl"
printf 'stamp=20261004\nagent_commit=abc1234\nagent_format=1\nagent_dirty=0\ndatad_commit=fed9876\ndatad_format=1\ndatad_dirty=0\n' >"$K/payload/guard/kit-source"
recd
check "kit_record datad with its keys: its commit, not dirty" "[ \$RC = 0 ] && grep -q '\"comp\":\"datad\",\"kit\":\"20261004\",\"commit\":\"fed9876\",\"format\":1,' '$T/root/data/u60-manifest.jsonl' && ! grep -q 'kit-dirty' '$T/root/data/u60-manifest.jsonl'"
check "do_devui records datad" "sed -n '/^do_devui() {/,/^}/p' '$K/device/install.sh' | grep -q '^ *kit_record datad\$'"
rm -rf "$T"

# an unfinished ship transaction on the device: admin / devui / recover refuse
for case_ in "check:有没结束的上机事务" "trial:有没结束的上机事务" "staged:有没结束的上机事务" "failed:停在 failed" "weird:阶段看不懂"; do
    for comp in admin devui recover; do
        setup
        mkdir -p "$T/data/u60-ship"
        printf 'v=1\ntxn=20261001-120000-datad\ncomp=datad\nphase=%s\nend=1\n' "${case_%%:*}" >"$T/data/u60-ship/txn"
        cp "$T/rc.local" "$T/rc.0"
        inst ssh "$comp"
        check "transaction ${case_%%:*}: $comp refused, says why, nothing done" "[ \$RC != 0 ] && grep -q '${case_#*:}' '$T/out' && grep -q '这次什么都没装' '$T/out' && cmp -s '$T/rc.local' '$T/rc.0' && [ ! -e '$T/data/u60-ship/u60-recover.sh' ] && ! grep -q 'dropbear' '$T/out'"
        rm -rf "$T"
    done
done
for txt in 'garbage' 'v=1\nphase=check\n' 'v=3\nphase=done\nend=1\n' 'v=2\nphase=done\n'; do
    setup
    mkdir -p "$T/data/u60-ship"
    printf "$txt" >"$T/data/u60-ship/txn"
    inst recover
    check "unreadable transaction log ($(printf "$txt" | head -n 1)…): refused" '[ $RC != 0 ] && grep -q "读不懂" "$T/out" && [ ! -e "$T/data/u60-ship/u60-recover.sh" ]'
    rm -rf "$T"
done
for ph in done rolledback aborted manifest_pending; do
    setup
    mkdir -p "$T/data/u60-ship"
    printf 'v=1\ntxn=20261001-120000-datad\ncomp=datad\nphase=%s\nend=1\n' "$ph" >"$T/data/u60-ship/txn"
    inst recover
    check "finished transaction ($ph): recover goes ahead" '[ $RC = 0 ] && [ -f "$T/data/u60-ship/u60-recover.sh" ]'
    rm -rf "$T"
done
# v=2 (web, guard: a directory or a file under /etc): the same phases
for case_ in done:0 check:1; do
    setup
    mkdir -p "$T/data/u60-ship"
    printf 'v=2\ntxn=20261001-120000-web\ncomp=web\nphase=%s\ndir=/data/admin|a|b\nend=1\n' "${case_%%:*}" >"$T/data/u60-ship/txn"
    inst recover
    if [ "${case_#*:}" = 0 ]; then
        check "v=2 transaction done: recover goes ahead" '[ $RC = 0 ] && [ -f "$T/data/u60-ship/u60-recover.sh" ]'
    else
        check "v=2 transaction in check: refused" '[ $RC != 0 ] && grep -q "有没结束的上机事务" "$T/out" && [ ! -e "$T/data/u60-ship/u60-recover.sh" ]'
    fi
    rm -rf "$T"
done

# kit_record touch / uid (after devui): their own keys in kit-source
setup
mkdir -p "$T/data/u60-guard"
printf '#!/bin/sh\necho "$*" >>%s/ship.calls\n' "$T" >"$T/data/u60-guard/u60-ship.sh"
printf 'stamp=20261001\nagent_commit=abc1234\nagent_format=1\nagent_dirty=0\ntouch_commit=def5678\ntouch_format=1\ntouch_dirty=0\nuid_commit=def5678\nuid_format=2\nuid_dirty=1\n' >"$K/payload/guard/kit-source"
sed -n '/^kit_record() {/,/^}/p' "$K/device/install.sh" >"$T/fns.sh"
(
    P=$K/payload G=$T/data/u60-guard KIT_MAC_TIME=1790000000
    log() { echo "[device] $*"; }
    warn() { echo "[device] 注意: $*"; }
    . "$T/fns.sh"
    kit_record touch
    kit_record uid
) >"$T/out" 2>&1
check "kit_record touch, uid: each with its own commit, format, dirty" "[ \"\$(cat '$T/ship.calls')\" = 'record-kit touch 20261001 def5678 1 1790000000
record-kit uid 20261001 def5678 2 1790000000 dirty' ]"
check "do_devui records touch and uid after the screen is up" "sed -n '/^do_devui() {/,/^}/p' '$K/device/install.sh' | tail -n 4 | tr -d ' ' | grep -qx 'kit_recordtouch' && sed -n '/^do_devui() {/,/^}/p' '$K/device/install.sh' | grep -q '^ *kit_record uid\$'"
rm -rf "$T"

# uid_restart (do_devui): the kit's u60-ship.sh uid-restart with the two md5s;
# a kit u60-ship.sh without it: not called (the old restart instead)
setup
mkdir -p "$T/data/u60-guard" "$T/d"
echo 'ui build' >"$T/d/u60pro-devui"
echo 'uid build' >"$T/d/u60-uid"
printf '#!/bin/sh\ncase "$1" in\n  uid-restart) echo "$*" >>%s/ship.calls; exit 0 ;;\nesac\n' "$T" >"$T/data/u60-guard/u60-ship.sh"
sed -n '/^uid_restart() {/,/^}/p' "$K/device/install.sh" >"$T/fns.sh"
ur() { (
    G=$T/data/u60-guard
    warn() { echo "[device] 注意: $*"; }
    . "$T/fns.sh"
    uid_restart "$T/d/u60pro-devui" "$T/d/u60-uid"
) >"$T/out" 2>&1; RC=$?; }
ur
check "uid_restart: u60-ship.sh uid-restart <ui md5> <uid md5>" "[ \"\$(cat '$T/ship.calls')\" = \"uid-restart \$(md5 '$T/d/u60pro-devui') \$(md5 '$T/d/u60-uid')\" ]"
printf '#!/bin/sh\necho "$*" >>%s/ship.calls\nexit 1\n# uid-restart)\n' "$T" >"$T/data/u60-guard/u60-ship.sh"
: >"$T/ship.calls"
ur
check "uid_restart: it says no: a warning, the install goes on" '[ $RC = 0 ] && grep -q "uid-restart 没确认" "$T/out"'
printf '#!/bin/sh\necho "$*" >>%s/ship.calls\n' "$T" >"$T/data/u60-guard/u60-ship.sh"
: >"$T/ship.calls"
ur
check "uid_restart: an older u60-ship.sh (no uid-restart): not called" '[ ! -s "$T/ship.calls" ]'
rm -rf "$T"

# rc.local writers (rc_add, both rc_replace branches, do_recover): a candidate,
# sh -n, a temp file next to rc.local, sync, mv, sync. A rename, not a rewrite
# in place, so a power cut mid-write leaves the old or the new file, never half.
echo "== kit: rc.local written atomically =="
ino() { ls -i "$1" | awk '{ print $1 }'; }
perm() { ls -ln "$1" | awk '{ print $1 }'; }
tmps() { ls -a "$(dirname "$T/rc.local")" | grep -c 'u60kit' ; }
setup
sed -n '/^RC=/,/^# ── 进程监督/p' "$K/device/install.sh" | sed '$d' >"$T/rcfns.sh"
rcf() { ( . "$T/rcfns.sh"; "$@" ) >"$T/out" 2>&1; RC=$?; }
chmod 750 "$T/rc.local"; i0=$(ino "$T/rc.local"); p0=$(perm "$T/rc.local")
rcf rc_add "/etc/init.d/example-svc start" "/etc/init.d/example-svc start"
check "rc_add: line in before exit 0" '[ $RC = 0 ] && [ "$(tail -n 2 "$T/rc.local" | head -n 1)" = "/etc/init.d/example-svc start" ] && sh -n "$T/rc.local"'
check "rc_add: replaced by rename (new inode), mode kept, no temp file left" '[ "$(ino "$T/rc.local")" != "$i0" ] && [ "$(perm "$T/rc.local")" = "$p0" ] && [ "$(tmps)" = 0 ] && [ ! -e /tmp/rc.local.new ]'
check "rc_add: original backed up once" '[ -f "$T/data/u60-kit/rc.local.orig" ] && ! grep -q example-svc "$T/data/u60-kit/rc.local.orig"'
cp "$T/rc.local" "$T/rc.0"; i0=$(ino "$T/rc.local")
rcf rc_add "/etc/init.d/example-svc start" "/etc/init.d/example-svc start"
check "rc_add again: nothing written" '[ $RC = 0 ] && cmp -s "$T/rc.local" "$T/rc.0" && [ "$(ino "$T/rc.local")" = "$i0" ]'
rcf rc_replace "/etc/init.d/u60-uid start # new" "/etc/init.d/u60-uid start" "# new"
check "rc_replace: swapped in place, by rename" '[ $RC = 0 ] && grep -qx "/etc/init.d/u60-uid start # new" "$T/rc.local" && ! grep -qx "/etc/init.d/u60-uid start" "$T/rc.local" && [ "$(ino "$T/rc.local")" != "$i0" ] && [ "$(perm "$T/rc.local")" = "$p0" ]'
echo "sh /x/start.sh # u60pro_devui" >>"$T/rc.local"; i0=$(ino "$T/rc.local")
rcf rc_replace "/etc/init.d/u60-uid start # new" "u60pro_devui" "# new"
check "rc_replace (already migrated): old line dropped, by rename" '[ $RC = 0 ] && ! grep -q u60pro_devui "$T/rc.local" && [ "$(ino "$T/rc.local")" != "$i0" ] && [ "$(tmps)" = 0 ]'
cp "$T/rc.local" "$T/rc.0"; i0=$(ino "$T/rc.local")
rcf rc_add "if true; then" "if true; then"
check "rc_add of a line that breaks sh -n: refused, rc.local byte for byte the same, same inode" '[ $RC != 0 ] && grep -q "语法检查没过" "$T/out" && cmp -s "$T/rc.local" "$T/rc.0" && [ "$(ino "$T/rc.local")" = "$i0" ] && [ "$(tmps)" = 0 ] && [ ! -e /tmp/rc.local.new ]'
mkdir "$T/rc.local.u60kit.tmp"
rcf rc_add "/etc/init.d/x start" "/etc/init.d/x start"
check "the temp file cannot be written: refused, rc.local untouched" '[ $RC != 0 ] && grep -q "写 rc.local 失败" "$T/out" && cmp -s "$T/rc.local" "$T/rc.0" && ! grep -q /etc/init.d/x "$T/rc.local"'
rm -rf "$T"

setup
printf '#!/bin/sh
if true; then
/etc/init.d/zte-agent start
exit 0
' >"$T/rc.local"
chmod 755 "$T/rc.local"; cp "$T/rc.local" "$T/rc.0"; i0=$(ino "$T/rc.local")
inst recover
check "recover on an rc.local that already fails sh -n: refused, untouched, no temp file" '[ $RC != 0 ] && grep -q "语法检查没过" "$T/out" && cmp -s "$T/rc.local" "$T/rc.0" && [ "$(ino "$T/rc.local")" = "$i0" ] && [ "$(tmps)" = 0 ]'
rm -rf "$T"
setup
chmod 755 "$T/rc.local"; i0=$(ino "$T/rc.local")
inst recover
check "recover: rc.local replaced by rename, still 755, no temp file" '[ $RC = 0 ] && [ "$(ino "$T/rc.local")" != "$i0" ] && [ "$(perm "$T/rc.local")" = "-rwxr-xr-x" ] && [ "$(tmps)" = 0 ]'
rm -rf "$T"

echo "kit u60-recover: $PASS passed, $FAIL failed"
[ "$FAIL" = 0 ]
