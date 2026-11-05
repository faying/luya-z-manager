#!/bin/sh
# Tests for luyaz-ship.sh auto-rollback, unquarantine and the stage refusal of a quarantined
# build (docs/SHIP.md「转正后自动退回」). Uses the sandbox-only `selftest` component.
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
SCRIPTS=${SCRIPTS:-/scripts}
SHIP=$SCRIPTS/luyaz-ship.sh
PASS=0
FAIL=0
check() {
    _d=$1
    shift
    if eval "$@"; then PASS=$((PASS + 1)); echo "  ok   $_d"; else FAIL=$((FAIL + 1)); echo "  FAIL $_d  [$*]"; fi
}
md5() { md5sum "$1" 2>/dev/null | cut -d' ' -f1; }
TXN=20260930-120000-selftest
setup() {
    T=$(mktemp -d)
    R=$T/root
    D=$R/data/luyaz/ship
    mkdir -p "$D" "$R/data/selftest" "$R/data/selftest-state" "$R/data/luyaz/scripts" "$T/tmp"
    export U60S_ROOT=$R U60S_TMP=$T/tmp U60S_SYNC=:
    F=$(sh "$SHIP" print-files selftest 2>/dev/null | head -n 1)
    [ -n "$F" ] || F=$R/data/selftest/prog
    printf 'old\n' >"$F.prev-$TXN"
    printf 'new\n' >"$F"
    O=$(md5 "$F.prev-$TXN") N=$(md5 "$F")
    echo "kept" >"$R/data/selftest-state/a"
    {
        printf 'v=1\ntxn=%s\ncomp=selftest\nphase=done\nreason=\nboot_id=b\ncommit=abcdef1\nmac_time=1790000000\nformat=1\n' "$TXN"
        printf 'act_boot=b\nact_uptime=5\nwatch=%s\nfile=%s|%s|%s\nend=1\n' "${1:-1}" "$F" "$O" "$N"
    } >"$D/txn"
    mkdir -p "$D/watch"
    [ "${1:-1}" = 1 ] && cp "$D/txn" "$D/watch/$TXN"
    printf '{"v":1,"kind":"ship","comp":"selftest","txn":"20260101-000000-selftest","commit":"1234567","format":2}\n' >"$D/manifest.jsonl"
}
run_auto() { sh "$SHIP" auto-rollback "$1" "ui gone" >/dev/null 2>&1; RC=$?; }

setup
run_auto "$TXN"
check "puts the old program back" '[ "$(md5 "$F")" = "$O" ] && grep -q "^phase=rolledback" "$D/txn"'
check "settings stay" '[ "$(cat "$R/data/selftest-state/a")" = kept ]'
check "quarantined" 'grep -q "^selftest|abcdef1|$N|$TXN|ui gone" "$D/quarantine"'
check "manifest names the old commit" 'tail -n 1 "$D/manifest.jsonl" | grep -q "\"commit\":\"1234567\".*\"note\":\"auto-rollback\""'
check "the watch on it ends" '[ ! -e "$D/watch/$TXN" ]'
check "status shows it" 'sh "$SHIP" status | grep -q "^quarantined=1$"'
stage_new() { # stage_new <commit>: upload a build of that commit; its output
    _t=20260930-130000-selftest
    mkdir -p "$D/stage/$_t"
    printf 'other\n' >"$D/stage/$_t/prog"
    printf 'conf\n' >"$D/stage/$_t/conf"
    printf 'v=1\ntxn=%s\ncomp=selftest\ncommit=%s\nmac_time=1790000000\nformat=2\nfile=prog %s\nfile=conf %s\n' \
        "$_t" "$1" "$(md5 "$D/stage/$_t/prog")" "$(md5 "$D/stage/$_t/conf")" >"$D/stage/$_t/meta"
    U60S_MIN_FREE_KB=0 sh "$SHIP" stage "$_t" 2>&1
}
check "stage refuses the same commit" 'stage_new abcdef1 | grep -q 隔离'
check "stage takes another commit past that check" '! stage_new 7654321 | grep -q 隔离'
rm -rf "$D/stage"
run_auto "$TXN"
check "a second call refuses" '[ $RC = 1 ] && [ "$(grep -c . "$D/quarantine")" = 1 ]'
sh "$SHIP" unquarantine abcdef1
check "unquarantine clears it" '[ ! -s "$D/quarantine" ]'
rm -rf "$T"

setup
run_auto 20260930-120000-other
check "another txn: refused, untouched" '[ $RC = 1 ] && [ "$(md5 "$F")" = "$N" ]'
rm -rf "$T"

setup
_t2=20260930-130000-other
sed "s/$TXN/$_t2/" "$D/txn" >"$D/txn.new" && mv "$D/txn.new" "$D/txn" # a newer ship is the last transaction
run_auto "$TXN"
check "an older build under a newer ship goes back too" '[ "$RC" = 0 ] && [ "$(md5 "$F")" = "$O" ]'
rm -rf "$T"

setup
sed -i "s/^phase=done/phase=check/" "$D/txn"
echo "$$ $$" >/dev/null
run_auto "$TXN"
check "a transaction in progress: not now" '[ $RC = 3 ] && [ "$(md5 "$F")" = "$N" ]'
rm -rf "$T"

setup 0
run_auto "$TXN"
check "unwatched: refused" '[ $RC = 1 ] && [ "$(md5 "$F")" = "$N" ]'
rm -rf "$T"

setup
: >"$D/auto-rollback-off"
run_auto "$TXN"
check "off switch: not now" '[ $RC = 3 ] && [ "$(md5 "$F")" = "$N" ]'
rm -rf "$T"

# lost reachability: the same rollback, no quarantine, one marker for the boot, its own alert
setup
sh "$SHIP" auto-rollback "$TXN" "conn lost: tailscale" >/dev/null 2>&1
RC=$?
check "conn: old program back" '[ $RC = 0 ] && [ "$(md5 "$F")" = "$O" ] && grep -q "^phase=rolledback" "$D/txn"'
check "conn: no quarantine" '[ ! -s "$D/quarantine" ]'
check "conn: marker for this boot" '[ -s "$D/watch-conn-rolled" ]'
check "conn: watch ended, manifest says so" '[ ! -e "$D/watch/$TXN" ] && grep -q "auto-rollback" "$D/manifest.jsonl"'
rm -rf "$T"

setup
run_auto "$TXN"
check "loop: still quarantines, no conn marker" '[ -s "$D/quarantine" ] && [ ! -e "$D/watch-conn-rolled" ]'
rm -rf "$T"

echo "u60-auto-rollback: $PASS passed, $FAIL failed"
[ "$FAIL" = 0 ]
