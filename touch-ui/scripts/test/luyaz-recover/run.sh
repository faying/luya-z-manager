#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# Tests for luyaz-recover.sh (boot-time clean-up of an interrupted ship
# transaction; docs/SHIP.md). The transaction logs here are written by hand,
# including torn and hostile ones; tests/u60-ship covers the logs the real
# executor leaves behind at every power-cut point. Everything lives under $T.
#
#   scripts/test/docker.sh        (runs every suite under scripts/test/)
# SPDX-License-Identifier: MIT
# ─────────────────────────────────────────────────────────────────────────────

SCRIPTS=${SCRIPTS:-/scripts}
REC=$SCRIPTS/luyaz-recover.sh
PASS=0
FAIL=0

ok() { PASS=$((PASS + 1)); echo "  ok   $1"; }
bad() { FAIL=$((FAIL + 1)); echo "  FAIL $1"; }
check() {
    _d=$1
    shift
    if eval "$@"; then ok "$_d"; else bad "$_d  [$*]"; fi
}

md5() { md5sum "$1" 2>/dev/null | cut -d' ' -f1; }
leak_snapshot() { ls -la /data /tmp/luyaz/ship 2>&1 | md5sum; }
LEAK0=$(leak_snapshot)

TXN=20260930-120000-core
setup() {
    T=$(mktemp -d)
    R=$T/root
    mkdir -p "$R/data/luyaz/ship" "$R/data/p" "$R/data/s"
    echo bbbb-2222 >"$T/boot"
    printf 'old prog\n' >"$T/o1"
    printf 'new prog\n' >"$T/n1"
    printf 'old conf\n' >"$T/o2"
    printf 'new conf\n' >"$T/n2"
    printf 'old state\n' >"$T/os"
    O1=$(md5 "$T/o1") N1=$(md5 "$T/n1") O2=$(md5 "$T/o2") N2=$(md5 "$T/n2") OS=$(md5 "$T/os")
    P1=$R/data/p/prog P2=$R/data/p/conf S1=$R/data/s/state.json S2=$R/data/s/pin
    export U60R_ROOT=$R U60R_BOOT_ID=$T/boot
    unset U60R_DIR U60R_SYNC
}
teardown() {
    check "$CASE: nothing written outside the sandbox" '[ "$(leak_snapshot)" = "$LEAK0" ]'
    rm -rf "$T"
}

# half <phase>: a transaction stopped half-way through promotion: prog and
# conf are the new ones, .prev hold the old; state.json was rewritten by the
# new version, pin did not exist before and was created by it.
half() {
    cp "$T/n1" "$P1"
    cp "$T/n2" "$P2"
    cp "$T/o1" "$P1.prev-$TXN"
    cp "$T/o2" "$P2.prev-$TXN"
    echo "state by new" >"$S1"
    cp "$T/os" "$S1.prev-$TXN"
    echo 1234 >"$S2"
    txn "$1"
}
txn() { # txn <phase> [boot]
    cat >"$R/data/luyaz/ship/txn" <<EOF
v=1
txn=$TXN
comp=core
phase=$1
reason=
boot_id=${2:-aaaa-1111}
exec_pid=77
file=$P1|$O1|$N1
file=$P2|$O2|$N2
statelist=$S1
statelist=$S2
state=$S1|$OS
state=$S2|-
end=1
EOF
}
addline() { # addline <line>: one more entry, before the final end=1
    grep -v '^end=1$' "$R/data/luyaz/ship/txn" >"$T/txn.new"
    printf '%s\nend=1\n' "$1" >>"$T/txn.new"
    mv "$T/txn.new" "$R/data/luyaz/ship/txn"
}
phase() { sed -n 's/^phase=//p' "$R/data/luyaz/ship/txn"; }
reason() { sed -n 's/^reason=//p' "$R/data/luyaz/ship/txn"; }
tree() { find "$R" | sort; find "$R" -type f | sort | while read -r f; do md5sum "$f"; done; }
cs() { tr -d . </proc/uptime | cut -d' ' -f1; } # centiseconds
recover() {
    _c0=$(cs)
    sh "$REC" >"$T/out" 2>&1
    RC=$?
    _c1=$(cs)
    CS=$((_c1 - _c0))
}

echo "== u60-recover =="

# ── normal boots: at once, not a byte written ───────────────────────────────
noop() { # noop <case> (the sandbox is ready)
    CASE=$1
    tree >"$T/before"
    recover
    tree >"$T/after"
    check "$1: exit 0" '[ "$RC" = 0 ]'
    check "$1: within 1 s (${CS}0 ms)" '[ "$CS" -le 100 ]'
    check "$1: whole tree byte for byte the same, no log" 'cmp -s "$T/before" "$T/after" && [ ! -e "$R/data/luyaz/ship/recover.log" ]'
}

setup
noop "no transaction log"
teardown

setup
rm -rf "$R/data/luyaz/ship"
noop "no ship directory"
teardown

for p in done rolledback aborted manifest_pending failed; do
    setup
    half "$p"
    noop "terminal $p (other boot)"
    teardown
done

for p in staged trial promote check manifest rollback; do
    setup
    half "$p"
    txn "$p" bbbb-2222
    noop "same boot, $p"
    teardown
done

# ── the phase table (docs/SHIP.md) ──────────────────────────────────────────
# staged: no state= lines yet (they are written after the snapshot)
setup
CASE="power cut in staged"
half staged
sed -i '/^state=/d' "$R/data/luyaz/ship/txn"
tree | grep -v "/data/luyaz/ship" >"$T/before"
recover
check "$CASE: aborted" '[ "$(phase)" = aborted ] && reason | grep -q "开机时已中止"'
tree | grep -v "/data/luyaz/ship" >"$T/after"
check "$CASE: no program or state file touched" 'cmp -s "$T/before" "$T/after"'
teardown

# trial: programs stay, state files go back to before the trial (R14)
setup
CASE="power cut in trial"
half trial
P1MD=$(md5 "$P1") P2MD=$(md5 "$P2")
recover
check "$CASE: aborted" '[ "$RC" = 0 ] && [ "$(phase)" = aborted ] && reason | grep -q "开机时已中止（断电时在 trial，程序没动过，状态文件已换回试跑前）"'
check "$CASE: programs untouched" '[ "$(md5 "$P1")" = "$P1MD" ] && [ "$(md5 "$P2")" = "$P2MD" ]'
check "$CASE: state back, created file removed" '[ "$(md5 "$S1")" = "$OS" ] && [ ! -e "$S2" ]'
tree >"$T/t1"
recover
tree >"$T/t2"
check "$CASE: second run changes nothing" 'cmp -s "$T/t1" "$T/t2"'
teardown

setup
CASE="power cut in trial, state .prev torn"
half trial
printf 'old st' >"$S1.prev-$TXN"
recover
check "$CASE: failed, state file not overwritten" '[ "$(phase)" = failed ] && [ "$(cat "$S1")" = "state by new" ]'
check "$CASE: the other state file still handled" '[ ! -e "$S2" ]'
teardown

for p in promote check rollback; do
    setup
    CASE="power cut in $p"
    half "$p"
    recover
    check "$CASE: rolled back" '[ "$RC" = 0 ] && [ "$(phase)" = rolledback ] && reason | grep -q "开机时已退回（断电时在 $p）"'
    check "$CASE: programs back to the old md5" '[ "$(md5 "$P1")" = "$O1" ] && [ "$(md5 "$P2")" = "$O2" ]'
    check "$CASE: state back, created file removed" '[ "$(md5 "$S1")" = "$OS" ] && [ ! -e "$S2" ]'
    check "$CASE: .prev kept" '[ "$(md5 "$P1.prev-$TXN")" = "$O1" ]'
    check "$CASE: other log lines kept" "grep -qx 'exec_pid=77' '$R/data/luyaz/ship/txn' && grep -qx 'file=$P1|$O1|$N1' '$R/data/luyaz/ship/txn'"
    check "$CASE: logged" "grep -q '还原：$P1' '$R/data/luyaz/ship/recover.log'"
    tree >"$T/t1"
    recover
    tree >"$T/t2"
    check "$CASE: second run changes nothing" 'cmp -s "$T/t1" "$T/t2"'
    teardown
done

setup
CASE="power cut in manifest"
half manifest
recover
check "$CASE: manifest_pending, new version stays" '[ "$(phase)" = manifest_pending ] && [ "$(md5 "$P1")" = "$N1" ] && [ "$(md5 "$S1")" != "$OS" ]'
teardown

# ── torn files (what ext4 can leave after a power cut) ──────────────────────
setup
CASE="live file 0 bytes"
half promote
: >"$P1"
recover
check "$CASE: restored from .prev" '[ "$(phase)" = rolledback ] && [ "$(md5 "$P1")" = "$O1" ]'
teardown

setup
CASE="live already old, .prev truncated"
half promote
cp "$T/o1" "$P1"
printf 'old' >"$P1.prev-$TXN"
recover
check "$CASE: skipped, rolled back" '[ "$(phase)" = rolledback ] && [ "$(md5 "$P1")" = "$O1" ]'
teardown

setup
CASE=".prev truncated, live new"
half check
printf 'old' >"$P1.prev-$TXN"
recover
check "$CASE: failed, never overwritten with the torn .prev" '[ "$(phase)" = failed ] && [ "$(md5 "$P1")" = "$N1" ]'
check "$CASE: the other files still restored" '[ "$(md5 "$P2")" = "$O2" ] && [ "$(md5 "$S1")" = "$OS" ]'
check "$CASE: reason names it" 'reason | grep -q "prog"'
teardown

setup
CASE=".prev 0 bytes, live 0 bytes"
half promote
: >"$P1"
: >"$P1.prev-$TXN"
recover
check "$CASE: failed, file left as it is" '[ "$(phase)" = failed ] && [ ! -s "$P1" ]'
teardown

setup
CASE=".prev missing, live new"
half promote
rm "$P1.prev-$TXN"
recover
check "$CASE: failed" '[ "$(phase)" = failed ] && [ "$(md5 "$P1")" = "$N1" ]'
teardown

setup
CASE=".prev name comes from path + txn"
half promote
rm "$P1.prev-$TXN"
cp "$T/o1" "$P1.prev-20260101-000000-core"
cp "$T/o1" "$P1.prev"
recover
check "$CASE: other .prev files are not used" '[ "$(phase)" = failed ] && [ "$(md5 "$P1")" = "$N1" ]'
teardown

# ── logs it must not act on ─────────────────────────────────────────────────
hostile() { # hostile <case>: log already written; nothing outside the log may change
    CASE=$1
    tree | grep -v "/data/luyaz/ship/recover.log" >"$T/before"
    recover
    tree | grep -v "/data/luyaz/ship/recover.log" >"$T/after"
    check "$1: exit 0" '[ "$RC" = 0 ]'
    check "$1: nothing touched" 'cmp -s "$T/before" "$T/after"'
}

setup
half promote
sed -i 's/^v=1/v=9/' "$R/data/luyaz/ship/txn"
hostile "unknown log version"
check "$CASE: one line in recover.log" '[ "$(wc -l <"$R/data/luyaz/ship/recover.log")" = 1 ]'
teardown

setup
half promote
echo outside >"$T/outside"
cp "$T/o1" "$T/outside.prev-$TXN"
addline "file=$T/outside|$O1|$N1"
hostile "path outside /data"
check "$CASE: the outside file untouched" '[ "$(cat "$T/outside")" = outside ]'
teardown

setup
half promote
addline "file=$R/data/p/../../etc/x|$O1|$N1"
hostile "path with .."
teardown

setup
half promote
addline "state=$R/data/s/a b|$OS"
hostile "path with a space"
teardown

setup
half promote
addline "file=$R/data/p/x|notamd5|$N1"
hostile "bad md5 field"
teardown

setup
half promote
addline "file=$R/data/p/x|-|$N1"
hostile "program marked as absent"
teardown

setup
half promote
sed -i 's/^txn=.*/txn=..\/..\/etc/' "$R/data/luyaz/ship/txn"
hostile "bad txn id"
teardown

setup
half promote
sed -i 's/^phase=.*/phase=launch-missiles/' "$R/data/luyaz/ship/txn"
hostile "unknown phase"
teardown

setup
half promote
head -c 3000 /dev/urandom >"$R/data/luyaz/ship/txn"
hostile "garbage log"
teardown

setup
half promote
printf 'v=1\nphase=promote\nboot_id=aaaa-1111\ntxn=%s\nfile=' "$TXN" >"$R/data/luyaz/ship/txn"
hostile "truncated log (no end=1)"
teardown

setup
CASE="empty boot_id file still acts on an old log"
half promote
: >"$T/boot"
recover
check "$CASE: rolled back" '[ "$(phase)" = rolledback ]'
teardown

# ── v=2 (phase two: a directory, programs that did not exist, /etc/init.d) ──
OLDREC=$SCRIPTS/test/fixtures/u60-recover.v1-2bb17b1.sh
# half2 <phase>: half-way through a v=2 promotion. /data/admin is the new
# directory (the old one is admin.prev-<txn>), /etc/init.d/u60-guard is new
# (its .prev under /data/luyaz/ship/prev/), added.sh did not exist before.
half2() {
    half "$1"
    A=$R/data/admin G=$R/etc/init.d/u60-guard N=$R/data/p/added.sh
    mkdir -p "$A.prev-$TXN/_next/static" "$A/_next/static" "${G%/*}" "$R/data/luyaz/ship/prev"
    printf 'old index\n' >"$A.prev-$TXN/index.html"
    printf 'old js\n' >"$A.prev-$TXN/_next/static/a b.js"
    printf 'new index\n' >"$A/index.html"
    printf 'new js\n' >"$A/_next/static/c.js"
    AO=$(sh "$REC" tree-fp "$A.prev-$TXN") AN=$(sh "$REC" tree-fp "$A")
    printf 'old init\n' >"$R/data/luyaz/ship/prev/etc.init.d.u60-guard.prev-$TXN"
    printf 'new init\n' >"$G"
    GO=$(md5 "$R/data/luyaz/ship/prev/etc.init.d.u60-guard.prev-$TXN") GN=$(md5 "$G")
    printf 'brand new\n' >"$N"
    sed -i 's/^v=1/v=2/' "$R/data/luyaz/ship/txn"
    addline "file=$G|$GO|$GN"
    addline "file=$N|-|$(md5 "$N")"
    addline "dir=$A|$AO|$AN"
}
all_old2() {
    check "$CASE: directory is the old one" '[ "$(sh "$REC" tree-fp "$A")" = "$AO" ]'
    check "$CASE: init script from luyaz/ship/prev, new program deleted" '[ "$(cat "$G")" = "old init" ] && [ ! -e "$N" ]'
    check "$CASE: programs and state old" '[ "$(md5 "$P1")" = "$O1" ] && [ "$(md5 "$S1")" = "$OS" ] && [ ! -e "$S2" ]'
    check "$CASE: no .rec-tmp or .bad left, .prev kept" '[ ! -e "$A.rec-tmp" ] && [ ! -e "$A.bad-$TXN" ] && [ -d "$A.prev-$TXN" ]'
    check "$CASE: no .prev made in /etc/init.d" '[ -z "$(ls "${G%/*}" | grep -v "^u60-guard$")" ]'
}

for p in promote check rollback; do
    setup
    CASE="v=2 power cut in $p"
    half2 $p
    recover
    check "$CASE: rolled back" '[ "$RC" = 0 ] && [ "$(phase)" = rolledback ]'
    all_old2
    tree >"$T/t1"
    recover
    tree >"$T/t2"
    check "$CASE: again: nothing changes" 'cmp -s "$T/t1" "$T/t2"'
    teardown
done

setup
CASE="v=2 between the two renames (no live directory)"
half2 promote
rm -rf "$A"
recover
check "$CASE: rolled back" '[ "$(phase)" = rolledback ]'
all_old2
teardown

setup
CASE="v=2 cut inside a directory restore (live moved to .bad, .rec-tmp half copied)"
half2 rollback
mv "$A" "$A.bad-$TXN"
mkdir -p "$A.rec-tmp"
printf 'half\n' >"$A.rec-tmp/index.html"
recover
check "$CASE: rolled back" '[ "$(phase)" = rolledback ]'
all_old2
teardown

setup
CASE="v=2 directory already old"
half2 check
rm -rf "$A"
cp -a "$A.prev-$TXN" "$A"
INO=$(ls -id "$A" | awk '{ print $1 }')
recover
check "$CASE: rolled back, the directory itself not replaced" '[ "$(phase)" = rolledback ] && [ "$(ls -id "$A" | awk "{ print \$1 }")" = "$INO" ]'
teardown

setup
CASE="v=2 directory .prev is not the old one"
half2 check
printf 'x\n' >"$A.prev-$TXN/extra"
recover
check "$CASE: failed, the live directory untouched" '[ "$(phase)" = failed ] && [ "$(sh "$REC" tree-fp "$A")" = "$AN" ] && grep -q "^reason=.*admin/" "$R/data/luyaz/ship/txn"'
teardown

setup
CASE="v=2 directory .prev has a symlink"
half2 check
ln -s index.html "$A.prev-$TXN/link"
recover
check "$CASE: failed, live untouched" '[ "$(phase)" = failed ] && [ "$(sh "$REC" tree-fp "$A")" = "$AN" ]'
teardown

for p in staged trial; do
    setup
    CASE="v=2 power cut in $p"
    half2 $p
    recover
    check "$CASE: aborted, directory and programs untouched" '[ "$(phase)" = aborted ] && [ "$(sh "$REC" tree-fp "$A")" = "$AN" ] && [ "$(cat "$G")" = "new init" ] && [ -f "$N" ]'
    teardown
done

setup
CASE="v=2 power cut in manifest"
half2 manifest
recover
check "$CASE: manifest_pending, nothing touched" '[ "$(phase)" = manifest_pending ] && [ "$(sh "$REC" tree-fp "$A")" = "$AN" ] && [ -f "$N" ]'
teardown

setup
half2 done
noop "v=2 done"
teardown

setup
half2 promote
addline "dir=$R/data/luyaz/ship|$AO|$AN"
hostile "v=2 a directory ship may not replace"
teardown

setup
half2 promote
addline "dir=$R/data/admin|nothex|$AN"
hostile "v=2 bad fingerprint field"
teardown

setup
half2 promote
sed -i 's/^v=2/v=1/' "$R/data/luyaz/ship/txn"
hostile "v=1 log with dir= and an absent program"
teardown

setup
half2 promote
sed -i 's/^v=2/v=1/' "$R/data/luyaz/ship/txn"
sed -i '/^dir=/d' "$R/data/luyaz/ship/txn"
hostile "v=1 log with an absent program only"
teardown

# the luyaz-recover.sh installed in phase one (2bb17b1, md5 cabc78d3…) must
# leave every v=2 log alone
for p in staged trial promote check manifest rollback; do
    setup
    CASE="old luyaz-recover.sh, v=2 log in $p"
    half2 $p
    tree | grep -v "/data/luyaz/ship/recover.log" >"$T/before"
    sh "$OLDREC" >"$T/out" 2>&1
    RC=$?
    tree | grep -v "/data/luyaz/ship/recover.log" >"$T/after"
    check "$CASE: exit 0, nothing touched" '[ "$RC" = 0 ] && cmp -s "$T/before" "$T/after"'
    teardown
done

setup
CASE="formats"
check "$CASE: prints 1 2 3 4 5" '[ "$(sh "$REC" formats)" = "1 2 3 4 5" ]'
sh "$OLDREC" formats >"$T/out" 2>&1
RC=$?
check "$CASE: the old one has no such command (exit 2, no 2 printed)" '[ "$RC" = 2 ] && ! grep -qw 2 "$T/out"'
check "$CASE: the fixture is the device's old copy (cabc78d3)" '[ "$(md5 "$OLDREC" | cut -c1-8)" = cabc78d3 ]'
teardown

setup
CASE="usage"
sh "$REC" nonsense >"$T/out" 2>&1
RC=$?
check "$CASE: exit 2" '[ "$RC" = 2 ]'
teardown

setup
CASE="selftest"
unset U60R_ROOT U60R_BOOT_ID
sh "$REC" selftest >"$T/out" 2>&1
RC=$?
check "$CASE: PASS" '[ "$RC" = 0 ] && grep -q "selftest: PASS" "$T/out"'
teardown

# ── Phase 10 10c (D7): a legacy rc.local is put back only while its programs exist ──
for have in no yes; do
    setup
    CASE="sentinel rc.local, u60-uid + u60-guard.sh present: $have"
    LEAK0=$(leak_snapshot)
    mkdir -p "$R/etc" "$R/data/luyaz/ship/prev"
    printf '#!/bin/sh\n/etc/init.d/luyaz-sentinel start   # luyaz\nexit 0\n' >"$R/etc/rc.local"
    printf '#!/bin/sh\n/etc/init.d/u60-uid start\n/etc/init.d/u60-guard start\nexit 0\n' >"$R/data/luyaz/ship/prev/etc.rc.local.prev-$TXN"
    RN=$(md5 "$R/etc/rc.local") RO=$(md5 "$R/data/luyaz/ship/prev/etc.rc.local.prev-$TXN")
    if [ $have = yes ]; then
        mkdir -p "$R/data/plugins/u60pro-devui" "$R/data/u60-guard"
        printf 'uid\n' >"$R/data/plugins/u60pro-devui/u60-uid"
        chmod 755 "$R/data/plugins/u60pro-devui/u60-uid"
        printf 'guard\n' >"$R/data/u60-guard/u60-guard.sh"
    fi
    printf 'v=3\ntxn=%s\ncomp=sentinel\nphase=check\nreason=\nboot_id=aaaa-1111\nexec_pid=77\nfile=%s|%s|%s\nend=1\n' \
        "$TXN" "$R/etc/rc.local" "$RO" "$RN" >"$R/data/luyaz/ship/txn"
    recover
    if [ $have = yes ]; then
        check "$CASE: the legacy rc.local back (exit 10)" '[ "$RC" = 10 ] && [ "$(md5 "$R/etc/rc.local")" = "$RO" ]'
    else
        check "$CASE: rc.local left as the sentinel one, says so" '[ "$RC" != 10 ] && [ "$(md5 "$R/etc/rc.local")" = "$RN" ] && grep -q "不还原" "$R/data/luyaz/ship/recover.log"'
    fi
    teardown
done

# ── Phase 10 R4 floor (round ③): never a sentinel that does not know /tmp/luyaz-screen.ctl ──
for mark in no yes; do
    setup
    CASE="sentinel .prev with the marker: $mark"
    LEAK0=$(leak_snapshot)
    SP=$R/data/luyaz/luyaz-sentinel
    mkdir -p "$R/data/luyaz"
    printf 'NEW-SENT /tmp/luyaz-screen.ctl\n' >"$SP"
    if [ $mark = yes ]; then printf 'OLD-SENT /tmp/luyaz-screen.ctl\n' >"$SP.prev-$TXN"; else printf 'OLD-SENT /tmp/u60-uid.ctl\n' >"$SP.prev-$TXN"; fi
    SN=$(md5 "$SP") SO=$(md5 "$SP.prev-$TXN")
    printf 'v=1\ntxn=%s\ncomp=sentinel\nphase=check\nreason=\nboot_id=aaaa-1111\nexec_pid=77\nfile=%s|%s|%s\nend=1\n' \
        "$TXN" "$SP" "$SO" "$SN" >"$R/data/luyaz/ship/txn"
    recover
    if [ $mark = yes ]; then
        check "$CASE: the old sentinel back" '[ "$(md5 "$SP")" = "$SO" ] && [ "$(phase)" = rolledback ]'
    else
        check "$CASE: the new sentinel left, says so, failed" '[ "$(md5 "$SP")" = "$SN" ] && grep -q "R4 之前的" "$R/data/luyaz/ship/recover.log" && [ "$(phase)" = failed ]'
    fi
    teardown
done
# ── round ③: a luyaz-ui new in the transaction is deleted only while the old program exists ──
for old in no yes; do
    setup
    CASE="luyaz-ui new in the transaction, old program present: $old"
    LEAK0=$(leak_snapshot)
    UP=$R/data/luyaz/ui/luyaz-ui
    mkdir -p "${UP%/*}"
    printf 'NEW-UI\n' >"$UP"
    UN=$(md5 "$UP")
    if [ $old = yes ]; then
        mkdir -p "$R/data/plugins/u60pro-devui"
        printf 'OLD-UI\n' >"$R/data/plugins/u60pro-devui/u60pro-devui"
    fi
    printf 'v=2\ntxn=%s\ncomp=touch\nphase=check\nreason=\nboot_id=aaaa-1111\nexec_pid=77\nfile=%s|-|%s\nend=1\n' \
        "$TXN" "$UP" "$UN" >"$R/data/luyaz/ship/txn"
    recover
    if [ $old = yes ]; then
        check "$CASE: luyaz-ui deleted (it did not exist before)" '[ ! -e "$UP" ] && [ "$(phase)" = rolledback ]'
    else
        check "$CASE: luyaz-ui kept, says so, failed" '[ "$(md5 "$UP")" = "$UN" ] && grep -q "不删" "$R/data/luyaz/ship/recover.log" && [ "$(phase)" = failed ]'
    fi
    teardown
done

# ── Phase 10 10e (D4/D7): no old-core rc.local or program back (always since rename F5/6:
# no .migrated mark any more; a stray one changes nothing) ──
for moved in no yes; do
    setup
    CASE="rc.local starting zwrt-datad, moved: $moved"
    LEAK0=$(leak_snapshot)
    mkdir -p "$R/etc" "$R/data/luyaz/ship/prev"
    printf '#!/bin/sh\n/etc/init.d/luyaz-core start\n/etc/init.d/luyaz-sentinel start\nexit 0\n' >"$R/etc/rc.local"
    printf '#!/bin/sh\n/etc/init.d/zwrt-datad start\n/etc/init.d/luyaz-sentinel start\nexit 0\n' >"$R/data/luyaz/ship/prev/etc.rc.local.prev-$TXN"
    RN=$(md5 "$R/etc/rc.local") RO=$(md5 "$R/data/luyaz/ship/prev/etc.rc.local.prev-$TXN")
    [ $moved = yes ] && mkdir -p "$R/data/luyaz/core" && : >"$R/data/luyaz/core/.migrated"
    printf 'v=3\ntxn=%s\ncomp=sentinel\nphase=check\nreason=\nboot_id=aaaa-1111\nexec_pid=77\nfile=%s|%s|%s\nend=1\n' \
        "$TXN" "$R/etc/rc.local" "$RO" "$RN" >"$R/data/luyaz/ship/txn"
    recover
    check "$CASE: rc.local left alone, says so, failed" '[ "$RC" != 10 ] && [ "$(md5 "$R/etc/rc.local")" = "$RN" ] && grep -q "不能退回旧布局" "$R/data/luyaz/ship/recover.log" && grep -q "^phase=failed" "$R/data/luyaz/ship/txn"'
    teardown
done
# fail closed: a retired start in any shape, not only the plain line (luyaz-ship.sh rc_retired)
for shape in indented conditional chained uid-only datad-amp commented; do
    setup
    CASE="rc.local .prev, retired start $shape (moved, programs gone)"
    LEAK0=$(leak_snapshot)
    mkdir -p "$R/etc" "$R/data/luyaz/ship/prev" "$R/data/luyaz/core"
    : >"$R/data/luyaz/core/.migrated"
    printf '#!/bin/sh\n/etc/init.d/luyaz-core start\n/etc/init.d/luyaz-sentinel start\nexit 0\n' >"$R/etc/rc.local"
    case $shape in
        indented) l='  /etc/init.d/u60-uid start' ;;
        conditional) l='[ -x /etc/init.d/u60-guard ] && /etc/init.d/u60-guard start' ;;
        chained) l='/etc/init.d/luyaz-sentinel start; /etc/init.d/u60-uid start' ;;
        uid-only) l='/etc/init.d/u60-uid start   # screen' ;;
        datad-amp) l='	/etc/init.d/zwrt-datad start &' ;;
        commented) l='# /etc/init.d/u60-uid start' ;;
    esac
    printf '#!/bin/sh\n/etc/init.d/luyaz-core start\n%s\nexit 0\n' "$l" >"$R/data/luyaz/ship/prev/etc.rc.local.prev-$TXN"
    RN=$(md5 "$R/etc/rc.local") RO=$(md5 "$R/data/luyaz/ship/prev/etc.rc.local.prev-$TXN")
    printf 'v=3\ntxn=%s\ncomp=sentinel\nphase=check\nreason=\nboot_id=aaaa-1111\nexec_pid=77\nfile=%s|%s|%s\nend=1\n' \
        "$TXN" "$R/etc/rc.local" "$RO" "$RN" >"$R/data/luyaz/ship/txn"
    recover
    if [ $shape = commented ]; then
        check "$CASE: only a comment, so the old rc.local back (exit 10)" '[ "$RC" = 10 ] && [ "$(md5 "$R/etc/rc.local")" = "$RO" ]'
    else
        check "$CASE: refused, rc.local untouched, says so" '[ "$RC" != 10 ] && [ "$(md5 "$R/etc/rc.local")" = "$RN" ] && grep -q "不还原" "$R/data/luyaz/ship/recover.log"'
    fi
    teardown
done
for moved in no yes; do
    setup
    CASE="core program at the old path, moved: $moved"
    LEAK0=$(leak_snapshot)
    P=$R/data/plugins/zwrt-datad/zwrt-datad
    mkdir -p "${P%/*}"
    printf 'new\n' >"$P"
    printf 'old\n' >"$P.prev-$TXN"
    PN=$(md5 "$P") PO=$(md5 "$P.prev-$TXN")
    [ $moved = yes ] && mkdir -p "$R/data/luyaz/core" && : >"$R/data/luyaz/core/.migrated"
    printf 'v=1\ntxn=%s\ncomp=datad\nphase=check\nreason=\nboot_id=aaaa-1111\nexec_pid=77\nfile=%s|%s|%s\nend=1\n' \
        "$TXN" "$P" "$PO" "$PN" >"$R/data/luyaz/ship/txn"
    recover
    check "$CASE: left alone, failed" '[ "$(md5 "$P")" = "$PN" ] && grep -q "^phase=failed" "$R/data/luyaz/ship/txn"'
    teardown
done

setup
CASE="scripts txn changed /etc/init.d/zwrt-datad"
LEAK0=$(leak_snapshot)
mkdir -p "$R/etc/init.d" "$R/data/luyaz/ship/prev" "$R/data/luyaz/core"
: >"$R/data/luyaz/core/.migrated"
P=$R/etc/init.d/zwrt-datad
printf 'new init\n' >"$P"
printf 'old init\n' >"$R/data/luyaz/ship/prev/etc.init.d.zwrt-datad.prev-$TXN"
PN=$(md5 "$P") PO=$(md5 "$R/data/luyaz/ship/prev/etc.init.d.zwrt-datad.prev-$TXN")
printf 'v=5\ntxn=%s\ncomp=scripts\nphase=check\nreason=\nboot_id=aaaa-1111\nexec_pid=77\nfile=%s|%s|%s\nend=1\n' \
    "$TXN" "$P" "$PO" "$PN" >"$R/data/luyaz/ship/txn"
recover
check "$CASE: put back, rolledback" '[ "$(md5 "$P")" = "$PO" ] && grep -q "^phase=rolledback" "$R/data/luyaz/ship/txn"'
teardown

echo "u60-recover: $PASS passed, $FAIL failed"
[ "$FAIL" = 0 ]
