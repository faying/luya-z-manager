#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# alert-lib.sh (the alert queue doctor, supervise and luyaz-fallback write to).
# Runs in a plain busybox container (same applets as the device):
#   docker run --rm -v "$PWD/scripts":/scripts:ro busybox:latest sh /scripts/test/alert-lib/run.sh
#   scripts/test/docker.sh        (runs every suite under scripts/test/)
# The cases came from scripts/test/u60-guard/run.sh, removed with the guard
# (manager docs/designs/u60-sentinel.md 5f; its other cases are the
# sentinel's and the core's Rust tests now, §21).
# SPDX-License-Identifier: MIT
# ─────────────────────────────────────────────────────────────────────────────
SCRIPTS=${SCRIPTS:-/scripts}
PASS=0
FAIL=0

ok() { PASS=$((PASS + 1)); echo "  ok   $1"; }
bad() { FAIL=$((FAIL + 1)); echo "  FAIL $1"; }
check() { # check <description> <shell test…>
    _d=$1
    shift
    if eval "$@"; then ok "$_d"; else bad "$_d  [$*]"; fi
}

echo "== alert-lib =="
T=$(mktemp -d)
mkdir -p "$T/alerts"
echo 1000 >"$T/uptime"
export ALERT_DIR=$T/alerts ALERT_UPTIME_FILE=$T/uptime
. "$SCRIPTS/alert-lib.sh"
alert_add agent-crash "$(printf 'exit 139\tsegv\nnext line ✗ done')"
check "one clean line, 5 fields" '[ "$(wc -l <$T/alerts/queue)" = 1 ] && [ "$(awk -F"\t" "{print NF}" $T/alerts/queue)" = 5 ]'
check "tabs/newlines/non-ASCII removed" 'grep -q "exit 139 segv next line  done$" $T/alerts/queue'
check "rejects a bad kind" '! alert_add Bad_Kind x && [ "$(wc -l <$T/alerts/queue)" = 1 ]'
long=$(printf 'x%.0s' $(seq 1 200))
alert_add core-crash "$long"
check "text cut to 120 chars" '[ "$(tail -n1 $T/alerts/queue | cut -f5 | wc -c)" = 121 ]'
i=0
while [ $i -lt 300 ]; do alert_add agent-crash "n$i"; i=$((i + 1)); done
check "trimmed to 200 once past 300" '[ "$(wc -l <$T/alerts/queue)" -le 300 ] && [ "$(wc -l <$T/alerts/queue)" -ge 200 ]'
check "sequence keeps counting through trims" '[ "$(cat $T/alerts/seq)" = 302 ] && [ "$(tail -n1 $T/alerts/queue | cut -f1)" = 302 ]'
rm -rf "$T"

echo "alert-lib: $PASS passed, $FAIL failed"
[ "$FAIL" = 0 ]
