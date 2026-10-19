#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# chaos.sh — break things on the real device and check they come back.
# Regression check for the procd setup (阶段 1a; since Phase 10 X9 the core's
# svcwatch records crashes).
#
# Runs ON the device, after the migration is complete:
#   ssh -n u60 'sh /data/luyaz/scripts/chaos.sh safe'
#
#   kill-core      kill -9 luyaz-core       → one new instance within 10 s, alert recorded
#   kill-web       kill -9 luyaz-web        → same
#   kill-sentinel  kill -9 luyaz-sentinel   → same, and it adopts the running UI
#                                             (no second luyaz-ui, nothing counted)
#   safe           all three above
#
# Uses only process-level faults. Nothing here touches rc.local, the modem, or eSIM.
# SPDX-License-Identifier: MIT
# ─────────────────────────────────────────────────────────────────────────────

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); echo "  PASS $1"; }
bad() { FAIL=$((FAIL + 1)); echo "  FAIL $1"; }

# pids whose /proc comm starts with $1 (side-by-side builds count too)
pids_of() {
    for d in /proc/[0-9]*; do
        c=$(cat "$d/comm" 2>/dev/null) || continue
        case "$c" in "$1"*) echo "${d#/proc/}" ;; esac
    done
}
count_of() { pids_of "$1" | wc -l | tr -d ' '; }
# our touch UI, and the screen's attempts
ui_pids() { pids_of luyaz-ui; }
uid_attempts() { cat /data/luyaz/screen/attempts 2>/dev/null; }
last_seq() { cut -f1 /data/alerts/queue 2>/dev/null | tail -n 1; }

# wait_until <seconds> <shell test> — 0 if the test passed in time
wait_until() {
    _i=0
    while [ "$_i" -lt "$1" ]; do
        eval "$2" && return 0
        sleep 1
        _i=$((_i + 1))
    done
    return 1
}

kill_program() { # kill_program <comm> <alert-kind>
    echo "kill -9 $1"
    old=$(pids_of "$1")
    [ -n "$old" ] || { bad "$1 is not running to begin with"; return; }
    seq0=$(last_seq)
    kill -9 $old
    if wait_until 10 "[ \"\$(count_of $1)\" = 1 ] && [ \"\$(pids_of $1)\" != \"$old\" ]"; then
        ok "$1 back within 10 s ($old → $(pids_of "$1"))"
    else
        bad "$1 not back within 10 s (running: $(pids_of "$1" | tr '\n' ' '))"
    fi
    sleep 5
    [ "$(count_of "$1")" = 1 ] && ok "exactly one $1" || bad "$(count_of "$1") copies of $1"
    if [ "$(last_seq)" != "$seq0" ] && tail -n 3 /data/alerts/queue | grep -q "	$2	"; then
        ok "$2 alert recorded"
    else
        bad "no $2 alert recorded"
    fi
}

kill_sentinel() {
    u=$(pids_of luyaz-sentinel)
    d=$(ui_pids)
    a0=$(uid_attempts)
    kill_program luyaz-sentinel sentinel-crash
    [ -n "$u" ] || return
    sleep 5
    [ "$(ui_pids)" = "$d" ] && ok "same UI still on screen (adopted, pid $d)" ||
        bad "UI changed: was '$d', now '$(ui_pids | tr '\n' ' ')'"
    [ "$(ui_pids | wc -l | tr -d ' ')" -le 1 ] && ok "at most one touch UI" || bad "$(ui_pids | wc -l | tr -d ' ') UIs running"
    [ "$(uid_attempts)" = "$a0" ] && ok "attempt count unchanged" || bad "attempt count changed"
}

case "$1" in
    kill-web) kill_program luyaz-web web-crash ;;
    kill-core) kill_program luyaz-core core-crash ;;
    kill-sentinel) kill_sentinel ;;
    safe)
        kill_program luyaz-core core-crash
        kill_program luyaz-web web-crash
        kill_sentinel
        ;;
    *)
        sed -n '4,15p' "$0"
        exit 2
        ;;
esac
echo
echo "passed $PASS, failed $FAIL"
[ "$FAIL" = 0 ]
