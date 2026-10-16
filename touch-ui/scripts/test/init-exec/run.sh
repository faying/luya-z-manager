#!/bin/sh
# Phase 10 X9: the four procd inits run the program itself (no supervise.sh). Runs each init's
# `procd_set_param command` with a fake program and a temp log: the log keeps what it had and
# gains the start line and the program's output; the pid procd would track is the program's own
# (exec, no sh left). Busybox container: scripts/test/docker.sh.
# SPDX-License-Identifier: MIT

SCRIPTS=${SCRIPTS:-/scripts}
PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); echo "  ok   $1"; }
bad() { FAIL=$((FAIL + 1)); echo "  FAIL $1"; }
check() { _d=$1; shift; if eval "$@"; then ok "$_d"; else bad "$_d  [$*]"; fi; }

T=$(mktemp -d)
# a program whose comm is "fakeprog": prints its arguments, then waits to be looked at
printf '#!/bin/sh\necho "fake says: $*"\nexec sleep 30\n' >"$T/fakeprog"
chmod +x "$T/fakeprog"

for init in luyaz-core luyaz-web luyaz-sentinel; do
    F=$SCRIPTS/$init.init
    check "$init: no supervise.sh" '! grep -q supervise "$F"'
    CMD=$(sed -n '/procd_set_param command/,/[^\\]$/p' "$F" | sed 's/\\$//' | tr '\n' ' ')
    LOG=$(printf '%s' "$CMD" | sed -n 's/.*exec >>\([^ ]*\) 2>&1.*/\1/p')
    case $init in luyaz-web) want=/tmp/luyaz-web.log ;; luyaz-sentinel) want=/tmp/luyaz-sentinel.out ;; luyaz-core) want=/tmp/luyaz-core.log ;; *) want=/tmp/zwrt-datad.log ;; esac
    check "$init: output appended to $want" '[ "$LOG" = "$want" ]'
    # the same command, the log and the program swapped for the test's
    CMD=$(printf '%s' "$CMD" | sed "s#$LOG#$T/$init.log#; s/^[[:space:]]*procd_set_param command //")
    echo "earlier line" >"$T/$init.log"
    # the init's variables the arguments use
    BIN=$T/fakeprog LAN_BIND=0.0.0.0 LAN_PORT=9461 TOKEN_FILE=/x/auth.token
    eval "set -- $CMD"
    "$@" &
    P=$!
    _i=0
    while ! grep -q 'fake says' "$T/$init.log" 2>/dev/null && [ $_i -lt 50 ]; do sleep 0.1; _i=$((_i + 1)); done
    check "$init: log kept, start line, program output" 'head -n 1 "$T/$init.log" | grep -qx "earlier line" && grep -q "^=== .* start $T/fakeprog\$" "$T/$init.log" && grep -q "fake says: " "$T/$init.log"'
    # fakeprog exec'd sleep: the started pid is that process, not a shell around it
    check "$init: the pid procd tracks is the program (exec, no wrapper)" '[ "$(cat /proc/$P/comm)" = sleep ]'
    kill $P 2>/dev/null
    wait $P 2>/dev/null
done
check "luyaz-sentinel: no procd stdout/stderr params" '! grep -q "procd_set_param std" "$SCRIPTS/luyaz-sentinel.init"'
check "luyaz-core: no SUPERVISE_COMM" '! grep -q SUPERVISE_COMM "$SCRIPTS/luyaz-core.init"'

rm -rf "$T"
echo "passed $PASS, failed $FAIL"
[ "$FAIL" = 0 ]
