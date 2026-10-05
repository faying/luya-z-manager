#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# apply.sh — switch tailscaled to a tuning variant, and put the previous one
# back by itself if Tailscale does not come back healthy.
#
#   sh /data/tailscale/apply.sh <variant.env> [node|relay]
#   sh /data/tailscale/apply.sh --none [node|relay]      no tuning (plain start)
#
# Tailscale is how this device is reached from outside, and restarting it
# drops every session that came in over it, including the SSH that would be
# used to repair a bad variant. So this runs detached (nohup) and decides on
# its own: after the restart it waits up to TSA_TIMEOUT seconds (default 300)
# for all of
#   - the backend state is Running,
#   - the advertised subnet route is still primary (when one was before),
#   - reachability: "node" mode needs a ping to one online peer to succeed;
#     "relay" mode (default; use it when no peer is sure to be awake, e.g. a
#     locked phone) needs the node to be online with the control plane.
# If any check fails the previous tuning.env is restored and tailscaled is
# restarted again. Every run is logged to /data/power/tailscale-apply.log;
# exit 0 = variant kept, 1 = rolled back, 2 = usage.
# While it runs, /tmp/tailscale-apply.pid holds its pid: start.sh's watcher
# does not restart tailscaled then. Before each of its restarts it ends that
# watcher too, so the one left afterwards is the one the start script that
# was actually used brings up (a start script on trial is never run again by
# a watcher after the rollback).
# SPDX-License-Identifier: MIT
# ─────────────────────────────────────────────────────────────────────────────

D=${TSA_DIR:-/data/tailscale}
CLI=${TSA_CLI:-$D/tailscale}
SOCK=--socket=/tmp/tailscaled.sock
START=${TSA_START:-$D/start.sh}
# the start script the rollback uses: set it when the start script itself is
# what is on trial (TSA_START=start.sh.new), so a bad one is not run twice
ROLLBACK_START=${TSA_ROLLBACK_START:-$START}
LOG=${TSA_LOG:-/data/power/tailscale-apply.log}
TIMEOUT=${TSA_TIMEOUT:-300}
POLL=${TSA_POLL:-10}
JF=${TSA_JSONFILTER:-jsonfilter}
PIDOF=${TSA_PIDOF:-pidof}
SLEEP=${TSA_SLEEP:-sleep}
KILL=${TSA_KILL:-kill}
RUN=${TSA_RUN:-/tmp}
APPLYING=$RUN/tailscale-apply.pid
WATCH_PID=$RUN/tailscale-watch.pid

log() { mkdir -p "$(dirname "$LOG")"; echo "$(date '+%Y-%m-%d %H:%M:%S') $*" >> "$LOG"; }

[ $# -ge 1 ] || { sed -n '5,8p' "$0"; exit 2; }
VARIANT=$1; MODE=${2:-relay}
case "$MODE" in node | relay) ;; *) echo "mode must be node or relay"; exit 2 ;; esac
if [ "$VARIANT" != --none ] && [ ! -f "$VARIANT" ]; then echo "no such variant: $VARIANT"; exit 2; fi

status_json() { "$CLI" $SOCK status --json 2>/dev/null; }
field() { status_json | $JF -e "$1" 2>/dev/null; }

restart() { # [start script]
    w=$(cat "$WATCH_PID" 2>/dev/null)
    case "$w" in '' | *[!0-9]*) ;; *) [ "$w" != $$ ] && kill "$w" 2>/dev/null ;; esac
    rm -f "$WATCH_PID"
    p=$($PIDOF tailscaled)
    [ -n "$p" ] && $KILL $p 2>/dev/null
    i=0; while [ -n "$($PIDOF tailscaled)" ] && [ $i -lt 15 ]; do $SLEEP 1; i=$((i + 1)); done
    [ -n "$($PIDOF tailscaled)" ] && $KILL -9 $($PIDOF tailscaled) 2>/dev/null
    sh "${1:-$START}"
}

healthy() { # $1 = route that must stay primary ("" = none required)
    [ "$(field '@.BackendState')" = Running ] || { WHY="backend not Running"; return 1; }
    if [ -n "$1" ]; then
        field '@.Self.PrimaryRoutes[*]' | grep -qxF "$1" || { WHY="route $1 not primary"; return 1; }
    fi
    if [ "$MODE" = node ]; then
        for ip in $(status_json | $JF -e '@.Peer[@.Online=true].TailscaleIPs[0]' 2>/dev/null); do
            "$CLI" $SOCK ping -c 1 --timeout=5s "$ip" >/dev/null 2>&1 && return 0
        done
        WHY="no online peer answered a ping"; return 1
    fi
    [ "$(field '@.Self.Online')" = true ] || { WHY="not online with the control plane"; return 1; }
    return 0
}

wait_healthy() {
    t=0
    while [ $t -lt "$TIMEOUT" ]; do
        healthy "$1" && return 0
        $SLEEP "$POLL"; t=$((t + POLL))
    done
    return 1
}

echo $$ >"$APPLYING"
trap 'rm -f "$APPLYING"' EXIT
trap 'exit 1' INT TERM
ROUTE=$(field '@.Self.PrimaryRoutes[0]')
[ -f "$D/tuning.env" ] && cp "$D/tuning.env" "$D/tuning.env.rollback" || rm -f "$D/tuning.env.rollback"
if [ "$VARIANT" = --none ]; then rm -f "$D/tuning.env"; else cp "$VARIANT" "$D/tuning.env"; fi
log "apply $VARIANT (mode $MODE, route ${ROUTE:-none})"
restart
if wait_healthy "$ROUTE"; then
    log "kept $VARIANT"
    rm -f "$D/tuning.env.rollback"
    exit 0
fi
log "FAILED $VARIANT: $WHY — rolling back"
if [ -f "$D/tuning.env.rollback" ]; then mv -f "$D/tuning.env.rollback" "$D/tuning.env"; else rm -f "$D/tuning.env"; fi
[ "$ROLLBACK_START" != "$START" ] && log "rollback starts with $ROLLBACK_START"
restart "$ROLLBACK_START"
if wait_healthy "$ROUTE"; then log "rolled back, healthy"; else log "rolled back, STILL UNHEALTHY: $WHY"; fi
exit 1
