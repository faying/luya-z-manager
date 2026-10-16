#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# chaos.sh — break things on the real device and check they come back.
# Regression check for the procd/supervise.sh/u60-guard setup (阶段 1a).
#
# Runs ON the device, after the migration is complete:
#   ssh -n u60 'sh /data/luyaz/scripts/chaos.sh kill-agent'
#
#   kill-agent     kill -9 zte-agent        → one new instance within 10 s, alert recorded
#   kill-datad     kill -9 the core (zwrt-datad; luyaz-core after the 10e move; also kill-core) → same
#   kill-guard     kill -9 u60-guard        → procd brings it back
#   kill-uid       kill -9 u60-uid          → procd brings it back, it adopts the running UI
#                                             (no second u60pro-devui, nothing counted)
#   safe           all five above
#   wifi-rescue    DISRUPTIVE: freezes the agent (SIGSTOP), turns both APs off,
#                  and waits up to 7 min for u60-guard to turn Wi-Fi back on.
#                  Every Wi-Fi client drops for that time. Ask the owner first.
#
# Uses only process-level faults plus, for wifi-rescue, the same uci keys the
# scenario engine writes. Nothing here touches rc.local, the modem, or eSIM.
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
# our touch UI under either name (Phase 10 R5), and the screen's attempts (R4; one dir since E3)
ui_pids() { pids_of u60pro-devui; pids_of luyaz-ui; }
uid_attempts() { cat /data/luyaz/screen/attempts 2>/dev/null; }
last_seq() { cut -f1 /data/alerts/queue 2>/dev/null | tail -n 1; }
hostapd_count() { ps w | awk '/\/hostapd( |$)/ && !/awk/ {n++} END {print n+0}'; }

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

guard_pid() {
    for d in /proc/[0-9]*; do
        tr '\0' ' ' 2>/dev/null <"$d/cmdline" | grep -q "u60-guard.sh" && echo "${d#/proc/}"
    done | head -n 1
}

kill_guard() {
    echo "kill -9 u60-guard"
    g=$(guard_pid)
    [ -n "$g" ] || { bad "u60-guard is not running"; return; }
    kill -9 "$g"
    if wait_until 15 '[ -n "$(guard_pid)" ] && [ "$(guard_pid)" != "$g" ]'; then
        ok "procd restarted u60-guard"
    else
        bad "u60-guard not restarted within 15 s"
    fi
}

kill_uid() {
    echo "kill -9 u60-uid"
    u=$(pids_of u60-uid)
    d=$(ui_pids)
    [ -n "$u" ] || { bad "u60-uid is not running"; return; }
    a0=$(uid_attempts)
    kill -9 $u
    if wait_until 15 "[ -n \"\$(pids_of u60-uid)\" ] && [ \"\$(pids_of u60-uid)\" != \"$u\" ]"; then
        ok "procd restarted u60-uid"
    else
        bad "u60-uid not restarted within 15 s"
    fi
    sleep 5
    [ "$(ui_pids)" = "$d" ] && ok "same UI still on screen (adopted, pid $d)" ||
        bad "UI changed: was '$d', now '$(ui_pids | tr '\n' ' ')'"
    [ "$(ui_pids | wc -l | tr -d ' ')" -le 1 ] && ok "at most one touch UI" || bad "$(ui_pids | wc -l | tr -d ' ') UIs running"
    [ "$(uid_attempts)" = "$a0" ] && ok "attempt count unchanged" || bad "attempt count changed"
}

wifi_rescue() {
    echo "wifi-rescue (DISRUPTIVE: Wi-Fi off for up to ~7 min)"
    a=$(pids_of zte-agent)
    [ -n "$a" ] || { bad "zte-agent not running"; return; }
    # Frozen, not killed: procd would restart a killed agent and the test
    # would measure the agent, not the watchdog.
    kill -STOP $a
    trap 'kill -CONT '"$a"' 2>/dev/null' EXIT INT TERM
    (
        flock 9
        echo $$ >/tmp/luyaz-wifi.lock
        uci set wireless.main_2g.disabled=1
        uci set wireless.main_5g.disabled=1
        uci commit wireless
        ubus call zwrt_wlan reload >/dev/null 2>&1
    ) 9>>/tmp/luyaz-wifi.lock
    wait_until 30 '[ "$(hostapd_count)" = 0 ]' && ok "APs down, agent frozen" || bad "APs did not go down"
    t0=$(cut -d. -f1 /proc/uptime)
    # heartbeat 300 s + one 60 s round + 45 s verify, with margin
    if wait_until 480 '[ "$(hostapd_count)" -gt 0 ]'; then
        ok "u60-guard brought Wi-Fi back after $(($(cut -d. -f1 /proc/uptime) - t0)) s"
    else
        bad "Wi-Fi still off after 8 min — turning it on by hand"
        (flock 9; uci set wireless.main_2g.disabled=0; uci set wireless.main_5g.disabled=0
         uci commit wireless; ubus call zwrt_wlan reload >/dev/null 2>&1) 9>>/tmp/luyaz-wifi.lock
    fi
    [ -f /tmp/u60-wifiguard.took-over ] && ok "takeover marker written" || bad "no takeover marker"
    m1=$(cat /tmp/u60-wifiguard.took-over 2>/dev/null)
    sleep 70
    [ "$(cat /tmp/u60-wifiguard.took-over 2>/dev/null)" = "$m1" ] && ok "marker not rewritten on the next round" ||
        bad "marker content changed"
    kill -CONT $a
    trap - EXIT INT TERM
    wait_until 60 '[ $(( $(cut -d. -f1 /proc/uptime) - $(cat /tmp/scenario.heartbeat 2>/dev/null || echo 0) )) -lt 60 ]' &&
        ok "agent heartbeat resumed" || bad "agent heartbeat did not resume"
    sleep 30
    [ "$(hostapd_count)" -gt 0 ] && ok "agent left Wi-Fi on (took the takeover)" || bad "Wi-Fi went off again after the agent resumed"
    echo "  note: the agent releases the marker 10 min after resuming; check /tmp/u60-wifiguard.took-over later"
}

# the core's process name follows the layout switch (Phase 10 10e; same line in
# u60-ship.sh, scripts/test/luyaz-dir)
luyaz_new() { [ -f "$1/data/luyaz/core/.migrated" ]; }
if luyaz_new ""; then CORE_NAME=luyaz-core; else CORE_NAME=zwrt-datad; fi

case "$1" in
    kill-agent) kill_program zte-agent agent-crash ;;
    kill-datad | kill-core) kill_program "$CORE_NAME" datad-crash ;;
    kill-guard) kill_guard ;;
    kill-uid) kill_uid ;;
    safe)
        kill_program zte-agent agent-crash
        kill_program "$CORE_NAME" datad-crash
        kill_guard
        kill_uid
        ;;
    wifi-rescue) wifi_rescue ;;
    *)
        sed -n '5,20p' "$0"
        exit 2
        ;;
esac
echo
echo "passed $PASS, failed $FAIL"
[ "$FAIL" = 0 ]
