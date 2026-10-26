#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# A fake-device test of the kit's u60 ship pieces (onboard/device/install.sh):
# the `recover` component (luyaz-recover.sh into /data/luyaz/ship as luyaz-recover.sh after its own
# selftest, and the rc.local line before the service start lines) and
# place_recover (what admin/ui do: put it there only when it is missing),
# kit_record (lweb; touch, sentinel, core) and the check for an
# unfinished u60 ship transaction (txn v=1 and v=2).
# Every path is moved into a temp dir (KIT_RC, KIT_STATE, KIT_SHIP_DIR,
# KIT_SCRIPTS); nothing touches a device. In the device's busybox:
#
#   docker run --rm -v "$PWD/onboard":/onboard:ro -v <touch-ui>/scripts:/scripts:ro \
#       u60-device-busybox:20260928 sh /onboard/test/device-recover.sh
#
# The full install still needs the real-device run (onboard/test/sandbox.sh).
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
# ─────────────────────────────────────────────────────────────────────────────

ONB=${ONB:-/onboard}
SCRIPTS=${SCRIPTS:-/scripts}
PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); echo "  ok   $1"; }
bad() { FAIL=$((FAIL + 1)); echo "  FAIL $1"; [ -f "$T/out" ] && sed 's/^/       | /' "$T/out" | tail -n 8; }
check() { _d=$1; shift; if eval "$@"; then ok "$_d"; else bad "$_d  [$*]"; fi; }
add_tx() { { echo "KLANG=${KLANG:-zh}"; grep "^tx() {" "$K/device/install.sh"; } >>"$T/fns.sh"; }  # the extracted functions print through tx
md5() { md5sum "$1" 2>/dev/null | cut -d' ' -f1; }

setup() {
    T=$(mktemp -d)
    K=$T/kit
    mkdir -p "$K/device" "$K/payload/scripts" "$T/data"
    cp "$ONB/device/install.sh" "$K/device/"
    cp "$SCRIPTS/luyaz-recover.sh" "$K/payload/scripts/"
    cat >"$T/rc.local" <<'EOF'
#!/bin/sh
/data/local/tmp/start_dropbear.sh &
/etc/init.d/luyaz-core start
/etc/init.d/luyaz-web start
/etc/init.d/luyaz-sentinel start
/etc/init.d/zte_topsw_get_brand stop
exit 0
EOF
    export KIT_RC=$T/rc.local KIT_STATE=$T/data/luyaz/kit KIT_SHIP_DIR=$T/data/luyaz/ship KIT_SCRIPTS=$T/data/luyaz/scripts
}
inst() { sh "$K/device/install.sh" "$@" >"$T/out" 2>&1; RC=$?; }
# the same rule tools/luyaz checks before a first ship
order() {
    awk '/^[[:space:]]*#/ { next } /luyaz-recover\.sh/ && !r { r = NR } /\/etc\/init\.d\/(luyaz-core|luyaz-web|luyaz-sentinel)[[:space:]]+start/ && !s { s = NR } END { if (!r) print "missing"; else if (s && s < r) print "late"; else print "ok" }' "$T/rc.local"
}

echo "== kit: u60-recover =="

setup
inst recover
check "recover: done" '[ $RC = 0 ]'
check "recover: the script in /data/luyaz/ship as luyaz-recover.sh, same as the kit's, executable" '[ "$(md5 "$T/data/luyaz/ship/luyaz-recover.sh")" = "$(md5 "$K/payload/scripts/luyaz-recover.sh")" ] && [ -x "$T/data/luyaz/ship/luyaz-recover.sh" ]'
check "recover: rc.local line right before the first service start" '[ "$(sed -n 3p "$T/rc.local" | cut -d";" -f1)" = "sh $T/data/luyaz/ship/luyaz-recover.sh" ] && [ "$(sed -n 4p "$T/rc.local")" = "/etc/init.d/luyaz-core start" ]'
check "recover: the order tools/luyaz wants" '[ "$(order)" = ok ]'
check "recover: rc.local still parses, the rest as it was" 'sh -n "$T/rc.local" && [ "$(grep -v luyaz-recover "$T/rc.local" | md5sum)" = "$(md5sum <"$T/data/luyaz/kit/rc.local.orig")" ]'
check "recover: the original rc.local backed up" '[ -f "$T/data/luyaz/kit/rc.local.orig" ]'
check "recover: the line written is the new one" 'grep -qF "sh $T/data/luyaz/ship/luyaz-recover.sh; [ \$? = 10 ] && exec /bin/sh $T/rc.local   # luyaz recover：" "$T/rc.local"'
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
printf '#!/bin/sh\n# /etc/init.d/luyaz-web start  (commented)\n/etc/init.d/luyaz-core start\nexit 0\n' >"$T/rc.local"
inst recover
check "a commented service line is not the first one" '[ "$(sed -n 3p "$T/rc.local" | cut -d" " -f1)" = sh ] && [ "$(order)" = ok ]'
rm -rf "$T"

setup
printf '#!/bin/sh\necho selftest: FAIL\nexit 1\n' >"$K/payload/scripts/luyaz-recover.sh"
cp "$T/rc.local" "$T/rc.0"
inst recover
check "a kit luyaz-recover.sh that fails its selftest: refused, nothing changed" '[ $RC != 0 ] && grep -q "selftest 不过" "$T/out" && [ ! -e "$T/data/luyaz/ship/luyaz-recover.sh" ] && cmp -s "$T/rc.local" "$T/rc.0"'
rm -rf "$T"

setup
mkdir -p "$T/data/luyaz/ship"
echo 'older one' >"$T/data/luyaz/ship/luyaz-recover.sh"
inst recover
check "recover replaces an older one, keeps it aside" '[ $RC = 0 ] && [ "$(md5 "$T/data/luyaz/ship/luyaz-recover.sh")" = "$(md5 "$K/payload/scripts/luyaz-recover.sh")" ] && grep -q "older one" "$T/data/luyaz/ship/luyaz-recover.sh.prev-kit"'
rm -rf "$T"

# place_recover (admin / ui): only when missing; never touches rc.local
setup
sed -n '/^recover_check() {/,/^do_recover() {/p' "$K/device/install.sh" | sed '$d' >"$T/fns.sh"
add_tx
place() { (
    P=$K/payload SHIPD=$KIT_SHIP_DIR
    # the three helpers device/install.sh defines before these functions
    log() { echo "[device] $*"; }; warn() { echo "[device] 注意: $*"; }; die() { echo "[device] 失败: $*"; exit 1; }
    . "$T/fns.sh"
    place_recover
) >"$T/out" 2>&1; RC=$?; }
cp "$T/rc.local" "$T/rc.0"
place
check "admin/ui: missing → put there, rc.local untouched" '[ $RC = 0 ] && [ -f "$T/data/luyaz/ship/luyaz-recover.sh" ] && cmp -s "$T/rc.local" "$T/rc.0" && grep -q "install.sh recover 才加" "$T/out"'
echo 'a different one' >"$T/data/luyaz/ship/luyaz-recover.sh"
place
check "admin/ui: a different one already there → not replaced, said so" '[ $RC = 0 ] && grep -q "a different one" "$T/data/luyaz/ship/luyaz-recover.sh" && grep -q "没换" "$T/out"'
rm -rf "$T"

# kit_record (after admin, which installs luyaz-web since 8d-2c): kit-source → one kind=kit line through luyaz-ship.sh
setup
mkdir -p "$T/root/data/luyaz/ship" "$T/root/etc/init.d" "$T/data/luyaz/scripts"
cp "$SCRIPTS/luyaz-ship.sh" "$T/data/luyaz/scripts/luyaz-ship.sh"
echo 'lweb build' >"$T/root/data/luyaz/luyaz-web"
cp "$SCRIPTS/luyaz-web.init" "$T/root/etc/init.d/luyaz-web"
printf 'stamp=20261001\nlweb_commit=abc1234def\nlweb_format=1\nlweb_dirty=1\n' >"$K/payload/scripts/kit-source"
sed -n '/^kit_record() {/,/^}/p' "$K/device/install.sh" >"$T/fns.sh"
add_tx
rec() { (
    P=$K/payload S=$T/data/luyaz/scripts
    export U60S_ROOT=$T/root U60S_TMP=$T/tmp KIT_MAC_TIME=1790000000
    log() { echo "[device] $*"; }
    warn() { echo "[device] 注意: $*"; }
    . "$T/fns.sh"
    kit_record lweb
) >"$T/out" 2>&1; RC=$?; }
rec
check "kit_record: one kit line from kit-source, dirty noted" "[ \$RC = 0 ] && grep -q '\"kind\":\"kit\",\"comp\":\"lweb\",\"kit\":\"20261001\",\"commit\":\"abc1234def\",\"format\":1,\"mac_time\":1790000000' '$T/root/data/luyaz/ship/manifest.jsonl' && grep -q 'kit-dirty' '$T/root/data/luyaz/ship/manifest.jsonl' && grep -q '设备清单: 记下装机包 20261001' '$T/out'"
rm "$K/payload/scripts/kit-source"
rec
check "kit_record: an old kit without kit-source: warned, install goes on" '[ $RC = 0 ] && grep -q "没记进设备清单" "$T/out" && [ "$(wc -l <"$T/root/data/luyaz/ship/manifest.jsonl")" = 1 ]'
rm -rf "$T"

# kit_record core (after ui) through the real luyaz-ship.sh: its own keys in
# kit-source; a kit-source without core_* keys still records it, as commit 0000000 and dirty
setup
mkdir -p "$T/root/data/luyaz/ship" "$T/data/luyaz/scripts"
cp "$SCRIPTS/luyaz-ship.sh" "$T/data/luyaz/scripts/luyaz-ship.sh"
echo 'core build' >"$T/root/data/luyaz/luyaz-core"
sed -n '/^kit_record() {/,/^}/p' "$K/device/install.sh" >"$T/fns.sh"
add_tx
recd() { (
    P=$K/payload S=$T/data/luyaz/scripts
    export U60S_ROOT=$T/root U60S_TMP=$T/tmp KIT_MAC_TIME=1790000000
    log() { echo "[device] $*"; }
    warn() { echo "[device] 注意: $*"; }
    . "$T/fns.sh"
    kit_record core
) >"$T/out" 2>&1; RC=$?; }
printf 'stamp=20261004\nlweb_commit=abc1234\nlweb_format=1\nlweb_dirty=0\n' >"$K/payload/scripts/kit-source"
recd
check "kit_record core, kit-source without core_* keys: recorded as 0000000, dirty" "[ \$RC = 0 ] && grep -q '\"kind\":\"kit\",\"comp\":\"core\",\"kit\":\"20261004\",\"commit\":\"0000000\",\"format\":1,' '$T/root/data/luyaz/ship/manifest.jsonl' && grep -q '\"path\":\"$T/root/data/luyaz/luyaz-core\",\"md5\":\"$(md5 "$T/root/data/luyaz/luyaz-core")\"' '$T/root/data/luyaz/ship/manifest.jsonl' && grep -q 'kit-dirty' '$T/root/data/luyaz/ship/manifest.jsonl'"
: >"$T/root/data/luyaz/ship/manifest.jsonl"
printf 'stamp=20261004\nlweb_commit=abc1234\nlweb_format=1\nlweb_dirty=0\ncore_commit=fed9876\ncore_format=1\ncore_dirty=0\n' >"$K/payload/scripts/kit-source"
recd
check "kit_record core with its keys: its commit, not dirty" "[ \$RC = 0 ] && grep -q '\"comp\":\"core\",\"kit\":\"20261004\",\"commit\":\"fed9876\",\"format\":1,' '$T/root/data/luyaz/ship/manifest.jsonl' && ! grep -q 'kit-dirty' '$T/root/data/luyaz/ship/manifest.jsonl'"
check "do_ui records core" "sed -n '/^do_ui() {/,/^}/p' '$K/device/install.sh' | grep -q '^ *kit_record core\$'"
rm -rf "$T"

# an unfinished ship transaction on the device: admin / ui / recover refuse
for case_ in "check:有没结束的上机事务" "trial:有没结束的上机事务" "staged:有没结束的上机事务" "failed:停在 failed" "weird:阶段看不懂"; do
    for comp in admin ui recover; do
        setup
        mkdir -p "$T/data/luyaz/ship"
        printf 'v=1\ntxn=20261001-120000-core\ncomp=core\nphase=%s\nend=1\n' "${case_%%:*}" >"$T/data/luyaz/ship/txn"
        cp "$T/rc.local" "$T/rc.0"
        inst ssh "$comp"
        check "transaction ${case_%%:*}: $comp refused, says why, nothing done" "[ \$RC != 0 ] && grep -q '${case_#*:}' '$T/out' && grep -q '这次什么都没装' '$T/out' && cmp -s '$T/rc.local' '$T/rc.0' && [ ! -e '$T/data/luyaz/ship/luyaz-recover.sh' ] && ! grep -q 'dropbear' '$T/out'"
        rm -rf "$T"
    done
done
for txt in 'garbage' 'v=1\nphase=check\n' 'v=3\nphase=done\nend=1\n' 'v=2\nphase=done\n'; do
    setup
    mkdir -p "$T/data/luyaz/ship"
    printf "$txt" >"$T/data/luyaz/ship/txn"
    inst recover
    check "unreadable transaction log ($(printf "$txt" | head -n 1)…): refused" '[ $RC != 0 ] && grep -q "读不懂" "$T/out" && [ ! -e "$T/data/luyaz/ship/luyaz-recover.sh" ]'
    rm -rf "$T"
done
for ph in done rolledback aborted manifest_pending; do
    setup
    mkdir -p "$T/data/luyaz/ship"
    printf 'v=1\ntxn=20261001-120000-core\ncomp=core\nphase=%s\nend=1\n' "$ph" >"$T/data/luyaz/ship/txn"
    inst recover
    check "finished transaction ($ph): recover goes ahead" '[ $RC = 0 ] && [ -f "$T/data/luyaz/ship/luyaz-recover.sh" ]'
    rm -rf "$T"
done
# v=2 (a directory or a file under /etc): the same phases
for case_ in done:0 check:1; do
    setup
    mkdir -p "$T/data/luyaz/ship"
    printf 'v=2\ntxn=20261001-120000-lweb\ncomp=lweb\nphase=%s\ndir=/data/x|a|b\nend=1\n' "${case_%%:*}" >"$T/data/luyaz/ship/txn"
    inst recover
    if [ "${case_#*:}" = 0 ]; then
        check "v=2 transaction done: recover goes ahead" '[ $RC = 0 ] && [ -f "$T/data/luyaz/ship/luyaz-recover.sh" ]'
    else
        check "v=2 transaction in check: refused" '[ $RC != 0 ] && grep -q "有没结束的上机事务" "$T/out" && [ ! -e "$T/data/luyaz/ship/luyaz-recover.sh" ]'
    fi
    rm -rf "$T"
done

# kit_record touch / sentinel (after ui): their own keys in kit-source
setup
mkdir -p "$T/data/luyaz/scripts"
printf '#!/bin/sh\necho "$*" >>%s/ship.calls\n' "$T" >"$T/data/luyaz/scripts/luyaz-ship.sh"
printf 'stamp=20261001\nlweb_commit=abc1234\nlweb_format=1\nlweb_dirty=0\ntouch_commit=def5678\ntouch_format=1\ntouch_dirty=0\nsentinel_commit=def5678\nsentinel_format=2\nsentinel_dirty=1\n' >"$K/payload/scripts/kit-source"
sed -n '/^kit_record() {/,/^}/p' "$K/device/install.sh" >"$T/fns.sh"
add_tx
(
    P=$K/payload S=$T/data/luyaz/scripts KIT_MAC_TIME=1790000000
    log() { echo "[device] $*"; }
    warn() { echo "[device] 注意: $*"; }
    . "$T/fns.sh"
    kit_record touch
    kit_record sentinel
) >"$T/out" 2>&1
check "kit_record touch, sentinel: each with its own commit, format, dirty" "[ \"\$(cat '$T/ship.calls')\" = 'record-kit touch 20261001 def5678 1 1790000000
record-kit sentinel 20261001 def5678 2 1790000000 dirty' ]"
check "do_ui records touch and sentinel after the screen is up" "sed -n '/^do_ui() {/,/^}/p' '$K/device/install.sh' | grep -q '^ *kit_record touch$' && sed -n '/^do_ui() {/,/^}/p' '$K/device/install.sh' | grep -q '^ *kit_record sentinel\$'"
rm -rf "$T"

# rc.local writers (rc_add, do_recover): a candidate,
# sh -n, a temp file next to rc.local, sync, mv, sync. A rename, not a rewrite
# in place, so a power cut mid-write leaves the old or the new file, never half.
echo "== kit: rc.local written atomically =="
ino() { ls -i "$1" | awk '{ print $1 }'; }
perm() { ls -ln "$1" | awk '{ print $1 }'; }
tmps() { ls -a "$(dirname "$T/rc.local")" | grep -c 'u60kit' ; }
setup
sed -n '/^RC=/,/^# ── 设备脚本/p' "$K/device/install.sh" | sed '$d' >"$T/rcfns.sh"
rcf() { ( . "$T/rcfns.sh"; "$@" ) >"$T/out" 2>&1; RC=$?; }
chmod 750 "$T/rc.local"; i0=$(ino "$T/rc.local"); p0=$(perm "$T/rc.local")
rcf rc_add "/etc/init.d/chill start" "/etc/init.d/chill start"
check "rc_add: line in before exit 0" '[ $RC = 0 ] && [ "$(tail -n 2 "$T/rc.local" | head -n 1)" = "/etc/init.d/chill start" ] && sh -n "$T/rc.local"'
check "rc_add: replaced by rename (new inode), mode kept, no temp file left" '[ "$(ino "$T/rc.local")" != "$i0" ] && [ "$(perm "$T/rc.local")" = "$p0" ] && [ "$(tmps)" = 0 ] && [ ! -e /tmp/rc.local.new ]'
check "rc_add: original backed up once" '[ -f "$T/data/luyaz/kit/rc.local.orig" ] && ! grep -q chill "$T/data/luyaz/kit/rc.local.orig"'
cp "$T/rc.local" "$T/rc.0"; i0=$(ino "$T/rc.local")
rcf rc_add "/etc/init.d/chill start" "/etc/init.d/chill start"
check "rc_add again: nothing written" '[ $RC = 0 ] && cmp -s "$T/rc.local" "$T/rc.0" && [ "$(ino "$T/rc.local")" = "$i0" ]'
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
/etc/init.d/luyaz-core start
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
