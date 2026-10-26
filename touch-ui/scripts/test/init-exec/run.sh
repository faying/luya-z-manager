#!/bin/sh
# Phase 10 X9: the four procd inits run the program itself (no supervise.sh). Runs each init's
# `procd_set_param command` with a fake program and a temp log: the log keeps what it had and
# gains the start line and the program's output; the pid procd would track is the program's own
# (exec, no sh left). Busybox container: scripts/test/docker.sh.
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

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
    case $init in luyaz-web) want=/tmp/luyaz-web.log ;; luyaz-sentinel) want=/tmp/luyaz-sentinel.out ;; luyaz-core) want=/tmp/luyaz-core.log ;; *) want=unknown ;; esac
    check "$init: output appended to $want" '[ "$LOG" = "$want" ]'
    # the same command, the log and the program swapped for the test's
    CMD=$(printf '%s' "$CMD" | sed "s#$LOG#$T/$init.log#; s/^[[:space:]]*procd_set_param command //")
    echo "earlier line" >"$T/$init.log"
    # the init's variables the arguments use
    BIN=$T/fakeprog LAN_BIND=127.0.0.1 LAN_PORT=9461 TOKEN_FILE=/x/auth.token
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

# net-performance 1b A1: start_service runs net-tune.sh as a subprocess capped at 5 s; whatever it
# does (missing, erroring, a fatal ash error, no want file, hanging) the core's instance is still set up.
core_start() { # <net-tune script path>: runs start_service in a subshell; prints what procd got
    (
        . "$SCRIPTS/luyaz-core.init"
        boot_is_normal() { return 0; }
        seed_token() { return 0; }
        procd_open_instance() { echo open; }
        procd_set_param() { [ "$1" = command ] && echo command; }
        procd_append_param() { :; }
        procd_close_instance() { echo close; }
        BIN=$T/fakeprog NET_TUNE=$1 NET_TUNE_LOG=$T/nt.log
        start_service
        echo "rc=$?"
    ) 2>&1
}
printf 'exit 3\n' >"$T/nt-err.sh"
printf 'x=abc\necho $((x + ))\necho not-reached\n' >"$T/nt-fatal.sh"
printf 'sleep 30\n' >"$T/nt-hang.sh"
mkdir -p "$T/nt/sys/net/ipv4"
echo 1 >"$T/nt/sys/net/ipv4/tcp_slow_start_after_idle"
export NT_SYSCTL=$T/nt/sys NT_NET=$T/nt/net NT_WANT=$T/nt/want NT_BASELINE=$T/nt/baseline NT_INTENT=$T/nt/intent NT_LOCK=$T/nt/lock
for c in missing err fatal nowant hang; do
    case $c in
    missing) f=$T/nt-none.sh ;; nowant) f=$SCRIPTS/net-tune.sh ;; *) f=$T/nt-$c.sh ;;
    esac
    _t0=$(date +%s)
    out=$(core_start "$f" | tr '\n' ' ')
    _dt=$(($(date +%s) - _t0))
    check "luyaz-core: net-tune $c -> core instance still set up ($out)" '[ "$out" = "open command close rc=0 " ]'
    case $c in hang) check "luyaz-core: hanging net-tune capped at 5 s (${_dt}s)" '[ "$_dt" -le 7 ]' ;; esac
done
check "luyaz-core: no want file -> nothing changed, no baseline" '[ "$(cat "$T/nt/sys/net/ipv4/tcp_slow_start_after_idle")" = 1 ] && [ ! -e "$T/nt/baseline" ]'
check "luyaz-core: net-tune never sourced" '! grep -Eq "^[[:space:]]*\\.[[:space:]].*NET_TUNE|source .*NET_TUNE" "$SCRIPTS/luyaz-core.init"'
unset NT_SYSCTL NT_NET NT_WANT NT_BASELINE NT_INTENT NT_LOCK

rm -rf "$T"
echo "passed $PASS, failed $FAIL"
[ "$FAIL" = 0 ]
