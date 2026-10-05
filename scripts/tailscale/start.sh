#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# /data/tailscale/start.sh — start tailscaled and bring the node up.
# Called from /etc/rc.local at boot, and by apply.sh when trying a tuning
# variant. Installed from source/manager/scripts/tailscale/start.sh.
#
#   sh start.sh              start (what rc.local and apply.sh run)
#   sh start.sh check        preflight (starts nothing, no network change); exit 1 if start would fail
#   sh start.sh status       mode, safe mode, boot strikes, disabled
#   sh start.sh stop         stop tailscaled and remove its routes/rules (--cleanup)
#   sh start.sh disable      stop, and do not start again until "enable"
#   sh start.sh enable       undo disable, then start
#   sh start.sh clear-safe   leave safe mode now (next start uses TS_MODE again)
#   sh start.sh revive       one supervision step (what the watcher does each
#                            poll); exit 0 nothing to do, 1 restarted, 2 gave
#                            up for now, 3 apply.sh is switching
#
#   binaries  /data/tailscale/{tailscaled,tailscale}
#   state     /data/tailscale/state
#   socket    /tmp/tailscaled.sock
#   log       /data/tailscaled.log  (appended; u60-guard keeps it ≤ 1 MB + one .old)
#   boot log  /data/tailscale/boot.log  (what each start decided; caps itself)
#
# Boot safety. rc.local runs inside
# S95done, and S97ab-updater (abctl --set_success) only runs after rc.local
# returns, so the part rc.local waits for only checks a few files and returns;
# waiting for the WAN, starting tailscaled and "up" happen in a detached
# worker, each with a limit.
#   - /data/tailscale/disabled present: never started.
#   - Kernel TUN mode (tailscale0) changes routing and iptables; userspace mode
#     changes neither, and still accepts tailnet connections to the device's
#     own 0.0.0.0 listeners (ssh :2222, :9090). Userspace is used instead of
#     TUN for this boot when the WAN address, gateway or a DNS server is in
#     100.64.0.0/10 (tailscaled's ts-input drops that range on every other
#     interface, ahead of the ESTABLISHED accept), or when there is no TUN.
#   - Boot strikes: a boot that started TUN mode and did not stay up for
#     TS_STABLE seconds is a strike. TS_MAX_STRIKES in a row → safe mode
#     (/data/tailscale/safe-mode): userspace from then on. After TS_SAFE_RETRY
#     seconds up in safe mode, the next boot tries TUN once more (one more
#     strike puts it back). Counted once per boot_id, so apply.sh restarts do
#     not count. Modem-crash reboots count too: that is why this only degrades.
#   - Watcher: after a start, a detached "__watch" loop (one per device,
#     $RUN/tailscale-watch.pid) checks every TS_WATCH_POLL seconds whether
#     tailscaled is still running, and if it died, starts it again through
#     the same worker (so disabled, safe mode, the CGNAT check, the tuning.env
#     and TS_TAILSCALED_BIN fallbacks all apply again), after --cleanup.
#     Liveness only: a tailscaled that runs but is not healthy is apply.sh's
#     and u60-guard's business, not this. Waits TS_REVIVE_DELAYS seconds
#     before the 1st, 2nd, 3rd... restart (the last value repeats), starts the
#     backoff over once tailscaled stayed up TS_STABLE seconds, and at most
#     TS_REVIVE_MAX restarts per TS_REVIVE_WINDOW seconds of uptime; past that
#     it gives up until the window ends (boot log + one alert in /data/alerts).
#     Never restarts while /data/tailscale/disabled exists, after "stop"
#     (until the next start; $RUN/tailscale.stopped), or while apply.sh runs
#     ($RUN/tailscale-apply.pid; apply.sh also ends the watcher before each of
#     its restarts, and the start it runs brings up a new one). Restarts in a
#     boot do not count as staying up: a TUN tailscaled that had to be
#     revived is not marked stable, so the boot still counts as a strike.
#     tailscaled is not under procd and nothing else restarts it; u60-guard
#     only records its health. TS_WATCH=0 turns the watcher off.
#
# Nothing here is specific to one owner's network: the advertised subnet is
# read from br-lan at start, the hostname defaults to u60pro. Per-device
# choices go in /data/tailscale/tuning.env (not in any repository; checked
# with sh -n and a trial in a subshell first, ignored if either fails):
#   TS_HOSTNAME=...            node name
#   TS_ROUTES=a.b.c.0/24       override the subnet read from br-lan
#   TS_TAILSCALED_FLAGS=...    extra tailscaled flags, e.g. --no-logs-no-support
#   TS_TAILSCALED_ENV=...      extra environment for tailscaled, e.g.
#                              TS_DISABLE_PORTMAPPER=1
#   TS_TAILSCALED_BIN=...      another tailscaled build to run (keep the file
#                              name "tailscaled": pidof finds it by name), e.g.
#                              /data/tailscale/nofight/tailscaled; falls back
#                              to $D/tailscaled when it is not executable
#   TS_MODE=tun|userspace      default tun
#   TS_ACCEPT_ROUTES=true|false   use subnet routes other nodes advertise (default true)
#   TS_ACCEPT_DNS=true|false      let Tailscale manage DNS (default false)
#   TS_EXIT_NODE=...              send internet traffic through this node (IP or
#                                 name; default none; LAN access stays allowed)
#   TS_STABLE / TS_MAX_STRIKES / TS_SAFE_RETRY / TS_WAN_WAIT   seconds/count
#                              (defaults 600 / 3 / 3600 / 180)
#   TS_WATCH=1|0 / TS_WATCH_POLL / TS_REVIVE_DELAYS / TS_REVIVE_MAX / TS_REVIVE_WINDOW
#                              the watcher (defaults 1 / 60 / "300 900 3600" /
#                              6 / 86400)
# The node logs in once (scripts/tailscale/README.md); no auth key is kept here.
# Older installs kept the identity in $D/tailscaled.state or in a file named
# $D/state; it is moved to $D/state/tailscaled.state before the first start,
# so the node keeps its identity.
# SPDX-License-Identifier: MIT
# ─────────────────────────────────────────────────────────────────────────────

SELF=$0
D=${TS_DIR:-/data/tailscale}
LOG=${TS_LOG:-/data/tailscaled.log}
SOCK=${TSS_SOCK:-/tmp/tailscaled.sock}
BLOG=${TSS_BOOT_LOG:-$D/boot.log}
RUN=${TSS_RUN:-/tmp}                 # lock and the current mode (memory only)
STRIKES=$D/boot-strikes
SAFE=$D/safe-mode
DISABLED=$D/disabled
LOCK=$RUN/tailscale-start.lock
MODE_FILE=$RUN/tailscale.mode
WATCH_PID=$RUN/tailscale-watch.pid   # the watcher
REVIVE=$RUN/tailscale-revive         # its backoff state (this boot only)
STOPPED=$RUN/tailscale.stopped       # "stop" ran: the watcher leaves it down
APPLYING=$RUN/tailscale-apply.pid    # apply.sh is switching (it writes this)
# test hooks (scripts/tailscale/test/start.sh); defaults are the device's own
BOOT_ID_FILE=${TSS_BOOT_ID:-/proc/sys/kernel/random/boot_id}
RESOLV=${TSS_RESOLV:-/tmp/resolv.conf.d/resolv.conf.auto}
TUN=${TSS_TUN:-/dev/net/tun}
IP=${TSS_IP:-ip}
PIDOF=${TSS_PIDOF:-pidof}
KILL=${TSS_KILL:-kill}
SLEEP=${TSS_SLEEP:-sleep}
DF=${TSS_DF:-df}
UPTIME=${TSS_UPTIME:-/proc/uptime}
ALERT_LIB=${TSS_ALERT_LIB:-/data/u60-guard/alert-lib.sh}
WATCH_TICKS=${TSS_WATCH_TICKS:-0}    # >0: the watcher ends after that many polls
FG=${TSS_FG:-0}                      # 1: run the worker in the foreground
TIMER=${TSS_TIMER:-1}                # 0: do not arm the stable timer
BLOG_MAX=65536

log() {
    echo "$(date '+%Y-%m-%d %H:%M:%S' 2>/dev/null) $*" >>"$BLOG" 2>/dev/null
    _sz=$(wc -c <"$BLOG" 2>/dev/null)
    case "$_sz" in '' | *[!0-9]*) return 0 ;; esac
    if [ "$_sz" -gt "$BLOG_MAX" ]; then
        tail -n 200 "$BLOG" >"$BLOG.tmp" 2>/dev/null && mv -f "$BLOG.tmp" "$BLOG"
    fi
    return 0
}

num() { # <value> <default>: a small non-negative integer, else the default
    case "$1" in '' | *[!0-9]*) echo "$2" ;; *) if [ ${#1} -le 9 ]; then echo "$1"; else echo "$2"; fi ;; esac
}

boot_id() { tr -dc '0-9a-f-' <"$BOOT_ID_FILE" 2>/dev/null; }

uptime_s() { _u=$(cut -d. -f1 "$UPTIME" 2>/dev/null); num "$_u" 0; }

live_pid() { # <pid file>: prints the pid when it is a live process
    _lp=$(cat "$1" 2>/dev/null)
    case "$_lp" in '' | *[!0-9]*) return 1 ;; esac
    [ ${#_lp} -le 9 ] && kill -0 "$_lp" 2>/dev/null && echo "$_lp"
}

# ── configuration ───────────────────────────────────────────────────────────
load_config() {
    TS_HOSTNAME=u60pro
    TS_ROUTES=
    TS_TAILSCALED_FLAGS=
    TS_TAILSCALED_ENV=
    TS_TAILSCALED_BIN=
    TS_MODE=tun
    TS_ACCEPT_ROUTES=true
    TS_ACCEPT_DNS=false
    TS_EXIT_NODE=
    TS_STABLE=${TS_STABLE:-600}
    TS_MAX_STRIKES=${TS_MAX_STRIKES:-3}
    TS_SAFE_RETRY=${TS_SAFE_RETRY:-3600}
    TS_WAN_WAIT=${TS_WAN_WAIT:-180}
    TS_WATCH=${TS_WATCH:-1}
    TS_WATCH_POLL=${TS_WATCH_POLL:-60}
    TS_REVIVE_DELAYS=${TS_REVIVE_DELAYS:-300 900 3600}
    TS_REVIVE_MAX=${TS_REVIVE_MAX:-6}
    TS_REVIVE_WINDOW=${TS_REVIVE_WINDOW:-86400}
    TUNING_BAD=
    _t=$D/tuning.env
    if [ -f "$_t" ]; then
        # busybox ash exits the whole shell on a bad "." — try it in a subshell first
        if sh -n "$_t" 2>/dev/null && (. "$_t") >/dev/null 2>&1; then
            . "$_t"
        else
            TUNING_BAD=1
        fi
    fi
    TS_STABLE=$(num "$TS_STABLE" 600)
    TS_MAX_STRIKES=$(num "$TS_MAX_STRIKES" 3)
    [ "$TS_MAX_STRIKES" -ge 1 ] || TS_MAX_STRIKES=1
    TS_SAFE_RETRY=$(num "$TS_SAFE_RETRY" 3600)
    TS_WAN_WAIT=$(num "$TS_WAN_WAIT" 180)
    case "$TS_WATCH" in 0) ;; *) TS_WATCH=1 ;; esac
    TS_WATCH_POLL=$(num "$TS_WATCH_POLL" 60)
    [ "$TS_WATCH_POLL" -ge 1 ] || TS_WATCH_POLL=1
    TS_REVIVE_MAX=$(num "$TS_REVIVE_MAX" 6)
    TS_REVIVE_WINDOW=$(num "$TS_REVIVE_WINDOW" 86400)
    case "$TS_MODE" in tun | userspace) ;; *) TS_MODE=tun ;; esac
    case "$TS_ACCEPT_ROUTES" in false) ;; *) TS_ACCEPT_ROUTES=true ;; esac
    case "$TS_ACCEPT_DNS" in true) ;; *) TS_ACCEPT_DNS=false ;; esac
    BIN=$D/tailscaled
    BIN_NOTE=
    if [ -n "$TS_TAILSCALED_BIN" ]; then
        if [ -x "$TS_TAILSCALED_BIN" ]; then
            BIN=$TS_TAILSCALED_BIN
        else
            BIN_NOTE="TS_TAILSCALED_BIN $TS_TAILSCALED_BIN not executable, using $BIN"
        fi
    fi
}

# ── boot strikes: one line "boot=<id> strikes=<n> stable=<0|1> mode=<m>" ───
rec_read() {
    R_BOOT= R_STRIKES=0 R_STABLE=0 R_MODE=
    _l=
    [ -f "$STRIKES" ] && read -r _l <"$STRIKES" 2>/dev/null
    set -f
    for _kv in $_l; do
        case "$_kv" in
            boot=*) R_BOOT=$(echo "${_kv#boot=}" | tr -dc '0-9a-f-') ;;
            strikes=*) R_STRIKES=$(num "${_kv#strikes=}" 0) ;;
            stable=1) R_STABLE=1 ;;
            mode=tun | mode=userspace | mode=pending) R_MODE=${_kv#mode=} ;;
        esac
    done
    set +f
}
rec_write() { # <boot> <strikes> <stable> <mode>
    echo "boot=$1 strikes=$2 stable=$3 mode=$4" >"$STRIKES.tmp" 2>/dev/null &&
        mv -f "$STRIKES.tmp" "$STRIKES" && sync
}

# ── identity from older layouts ─────────────────────────────────────────────
# $D/state used to be the state file itself on some installs, and others kept
# $D/tailscaled.state. Starting against a missing state file silently makes a
# new node, so move the old one into place (only while tailscaled is stopped).
old_state() { # prints the old state file that would be moved, if any
    [ -f "$D/state/tailscaled.state" ] && return 1
    if [ -f "$D/state" ]; then echo "$D/state"; return 0; fi
    if [ -f "$D/tailscaled.state" ]; then echo "$D/tailscaled.state"; return 0; fi
    return 1
}
migrate_state() {
    _o=$(old_state) || { mkdir -p "$D/state"; return 0; }
    if [ "$_o" = "$D/state" ]; then
        mv -f "$D/state" "$D/state.migrating" && mkdir -p "$D/state" &&
            mv -f "$D/state.migrating" "$D/state/tailscaled.state"
    else
        mkdir -p "$D/state" && mv -f "$_o" "$D/state/tailscaled.state"
    fi
    _rc=$?
    sync
    log "moved old state $_o → $D/state/tailscaled.state (rc $_rc)"
}

# ── network checks ──────────────────────────────────────────────────────────
in_cgnat() { # <a.b.c.d>: 100.64.0.0/10, except tailscale's own 100.100.100.100
    case "$1" in 100.100.100.100) return 1 ;; 100.*.*.*) ;; *) return 1 ;; esac
    _o=$(echo "$1" | cut -d. -f2)
    case "$_o" in '' | *[!0-9]*) return 1 ;; esac
    [ ${#_o} -le 3 ] && [ "$_o" -ge 64 ] && [ "$_o" -le 127 ]
}
wan_default() { $IP -4 route show default 2>/dev/null | head -n 1; }
cgnat_hit() { # prints the first offending address, returns 0 if any
    _r=$(wan_default)
    _dev=$(echo "$_r" | awk '{ for (i = 1; i < NF; i++) if ($i == "dev") { print $(i + 1); exit } }')
    _gw=$(echo "$_r" | awk '{ for (i = 1; i < NF; i++) if ($i == "via") { print $(i + 1); exit } }')
    _addrs=$_gw
    [ -n "$_dev" ] && _addrs="$_addrs $($IP -4 -o addr show dev "$_dev" 2>/dev/null | awk '{ print $4 }' | cut -d/ -f1)"
    [ -f "$RESOLV" ] && _addrs="$_addrs $(awk '$1 == "nameserver" { print $2 }' "$RESOLV" 2>/dev/null)"
    for _a in $_addrs; do
        in_cgnat "$_a" && { echo "$_a"; return 0; }
    done
    return 1
}

# run_bounded <seconds> <cmd...>: our own child, killed when it overruns (124).
# Real sleep on purpose (not $SLEEP): the limit is wall time.
run_bounded() {
    _s=$1; shift
    "$@" &
    _p=$!
    _i=0
    while kill -0 "$_p" 2>/dev/null; do
        if [ "$_i" -ge "$_s" ]; then
            kill "$_p" 2>/dev/null
            sleep 1
            kill -9 "$_p" 2>/dev/null
            wait "$_p" 2>/dev/null
            return 124
        fi
        sleep 1
        _i=$((_i + 1))
    done
    wait "$_p"
}

# ── watcher: restart a tailscaled that died, with a bounded backoff ─────────
# State, one line in $REVIVE (memory: a reboot starts it over):
#   n=<restarts since it last stayed up> wn=<restarts in this window>
#   win=<uptime the window began> dead=<uptime first seen down, 0 = up>
#   last=<uptime of the last restart> total=<restarts this boot> gaveup=<0|1>
rv_read() {
    RV_N=0 RV_WN=0 RV_WIN=0 RV_DEAD=0 RV_LAST=0 RV_TOTAL=0 RV_GAVEUP=0
    _l=
    [ -f "$REVIVE" ] && read -r _l <"$REVIVE" 2>/dev/null
    set -f
    for _kv in $_l; do
        case "$_kv" in
            n=*) RV_N=$(num "${_kv#n=}" 0) ;;
            wn=*) RV_WN=$(num "${_kv#wn=}" 0) ;;
            win=*) RV_WIN=$(num "${_kv#win=}" 0) ;;
            dead=*) RV_DEAD=$(num "${_kv#dead=}" 0) ;;
            last=*) RV_LAST=$(num "${_kv#last=}" 0) ;;
            total=*) RV_TOTAL=$(num "${_kv#total=}" 0) ;;
            gaveup=1) RV_GAVEUP=1 ;;
        esac
    done
    set +f
}
rv_write() {
    echo "n=$RV_N wn=$RV_WN win=$RV_WIN dead=$RV_DEAD last=$RV_LAST total=$RV_TOTAL gaveup=$RV_GAVEUP" \
        >"$REVIVE.tmp" 2>/dev/null && mv -f "$REVIVE.tmp" "$REVIVE"
}
rv_delay() { # <restarts so far>: seconds to wait before the next one
    _d=
    _k=0
    set -f
    for _x in $TS_REVIVE_DELAYS; do
        _x=$(num "$_x" "")
        [ -n "$_x" ] || continue
        _d=$_x
        [ "$_k" -ge "$1" ] && break
        _k=$((_k + 1))
    done
    set +f
    echo "${_d:-300}"
}
alert() { # <kind> <text>: into /data/alerts when u60-guard's library is there
    [ -f "$ALERT_LIB" ] || return 0
    (. "$ALERT_LIB" && alert_add "$1" "$2") >/dev/null 2>&1
    return 0
}

# One step. 0 nothing to do (or waiting out the backoff), 1 restarted,
# 2 gave up until the window ends, 3 apply.sh is switching. load_config first.
revive_tick() {
    _now=$(uptime_s)
    rv_read
    if [ -f "$DISABLED" ] || [ -f "$STOPPED" ]; then
        RV_DEAD=0
        rv_write
        return 0
    fi
    if live_pid "$APPLYING" >/dev/null; then # its kill and its start are one switch
        RV_DEAD=0
        rv_write
        return 3
    fi
    if $PIDOF tailscaled >/dev/null 2>&1 || live_pid "$LOCK/pid" >/dev/null; then
        _chg=0
        [ "$RV_DEAD" = 0 ] || { RV_DEAD=0; _chg=1; }
        if [ "$RV_N" -gt 0 ] && [ $((_now - RV_LAST)) -ge "$TS_STABLE" ]; then
            log "watch: tailscaled stayed up ${TS_STABLE}s since the last restart: backoff starts over"
            RV_N=0 _chg=1
        fi
        [ "$_chg" = 0 ] || rv_write # the usual tick: nothing written
        return 0
    fi
    if [ "$RV_WIN" = 0 ] || [ $((_now - RV_WIN)) -ge "$TS_REVIVE_WINDOW" ]; then
        RV_WIN=$_now RV_WN=0 RV_GAVEUP=0
    fi
    _d=$(rv_delay "$RV_N")
    if [ "$RV_DEAD" = 0 ]; then
        RV_DEAD=$_now
        [ "$RV_WN" -ge "$TS_REVIVE_MAX" ] || log "watch: tailscaled is not running; restart in ${_d}s"
    fi
    if [ "$RV_WN" -ge "$TS_REVIVE_MAX" ]; then
        if [ "$RV_GAVEUP" != 1 ]; then
            RV_GAVEUP=1
            log "watch: GAVE UP: $RV_WN restarts within ${TS_REVIVE_WINDOW}s; next try when that window ends"
            alert tailscale-gave-up "tailscaled died $RV_WN times; restarts paused for up to ${TS_REVIVE_WINDOW}s"
        fi
        rv_write
        return 2
    fi
    if [ $((_now - RV_DEAD)) -lt "$_d" ]; then
        rv_write
        return 0
    fi
    RV_N=$((RV_N + 1)) RV_WN=$((RV_WN + 1)) RV_TOTAL=$((RV_TOTAL + 1)) RV_LAST=$_now RV_DEAD=0
    rv_write
    log "watch: restarting tailscaled (down ${_d}s+; restart $RV_WN of $TS_REVIVE_MAX in this window)"
    # a TUN tailscaled that died leaves ip rules, table 52 and ts-* chains behind
    run_bounded 30 "$BIN" --cleanup >/dev/null 2>&1
    rm -f "$MODE_FILE"
    live_pid "$LOCK/pid" >/dev/null || rm -rf "$LOCK"
    if mkdir "$LOCK" 2>/dev/null; then
        echo $$ >"$LOCK/pid"
        worker
        rm -rf "$LOCK"
    fi
    return 1
}

watch_spawn() { # after a start: one watcher per device
    [ "$TS_WATCH" = 1 ] || return 0
    live_pid "$WATCH_PID" >/dev/null && return 0
    nohup sh "$SELF" __watch </dev/null >/dev/null 2>&1 &
    echo $! >"$WATCH_PID"
}

watch() {
    load_config
    echo $$ >"$WATCH_PID"
    _ticks=0
    _max=$(num "$WATCH_TICKS" 0)
    while [ "$TS_WATCH" = 1 ]; do
        $SLEEP "$TS_WATCH_POLL"
        [ "$(cat "$WATCH_PID" 2>/dev/null)" = "$$" ] || return 0 # stop, apply.sh or a newer watcher
        revive_tick
        _ticks=$((_ticks + 1))
        [ "$_max" -gt 0 ] && [ "$_ticks" -ge "$_max" ] && break
    done
    [ "$(cat "$WATCH_PID" 2>/dev/null)" = "$$" ] && rm -f "$WATCH_PID"
    return 0
}

# ── worker: everything rc.local must not wait for ───────────────────────────
worker() {
    load_config
    [ -n "$TUNING_BAD" ] && log "tuning.env failed sh -n or a trial load: ignored, defaults used"
    [ -n "$BIN_NOTE" ] && log "$BIN_NOTE"

    NOW=$(boot_id)
    rec_read
    FIRST=0
    N=$R_STRIKES
    STABLE=$R_STABLE
    if [ -z "$NOW" ] || [ "$R_BOOT" != "$NOW" ]; then
        FIRST=1
        STABLE=0
        # the previous boot started TUN and never stayed up TS_STABLE seconds
        [ -n "$R_BOOT" ] && [ "$R_STABLE" = 0 ] && [ "$R_MODE" = tun ] && N=$((N + 1))
        rec_write "$NOW" "$N" 0 pending
    fi

    MODE=$TS_MODE
    WHY=configured
    if [ -f "$SAFE" ]; then
        MODE=userspace WHY=safe-mode
    elif [ "$MODE" = tun ] && [ "$N" -ge "$TS_MAX_STRIKES" ]; then
        echo "$(date '+%Y-%m-%d %H:%M:%S' 2>/dev/null) $N boots in a row did not stay up ${TS_STABLE}s in TUN mode" >"$SAFE"
        sync
        MODE=userspace WHY=safe-mode
        log "SAFE MODE: $N strikes (>= $TS_MAX_STRIKES); userspace until it stays up ${TS_SAFE_RETRY}s or clear-safe"
    fi
    if [ "$MODE" = tun ] && [ ! -c "$TUN" ]; then
        mkdir -p "$(dirname "$TUN")" 2>/dev/null
        mknod "$TUN" c 10 200 2>/dev/null
        [ -c "$TUN" ] || { MODE=userspace WHY=no-tun; }
    fi

    # The LAN subnet, e.g. 192.168.0.1/24 on br-lan → 192.168.0.0/24
    if [ -z "$TS_ROUTES" ]; then
        TS_ROUTES=$($IP -o -4 addr show br-lan 2>/dev/null | awk '{print $4; exit}' |
            awk -F'[./]' 'NF == 5 && $5 == 24 {print $1 "." $2 "." $3 ".0/24"}')
    fi

    # The WAN first: nothing reaches the control plane before it, and the
    # CGNAT check needs its address. Bounded; start anyway when it overruns.
    _w=0
    while [ -z "$(wan_default)" ] && [ "$_w" -lt "$TS_WAN_WAIT" ]; do
        $SLEEP 2
        _w=$((_w + 2))
    done
    if [ -z "$(wan_default)" ]; then
        log "no default route after ${_w}s; starting anyway"
    elif [ "$MODE" = tun ]; then
        _bad=$(cgnat_hit) && { MODE=userspace WHY="cgnat $_bad"; }
    fi

    if [ -f "$DISABLED" ]; then # "disable" ran while this worker waited
        log "disabled while waiting: not started"
        return 0
    fi
    if $PIDOF tailscaled >/dev/null 2>&1; then
        log "tailscaled already running; nothing started"
        watch_spawn
        return 0
    fi
    migrate_state
    if [ "$MODE" = tun ]; then TUNARG=--tun=tailscale0; else TUNARG=--tun=userspace-networking; fi
    # Append (>>), never truncate-on-open: u60-guard caps the file by copying it
    # to .old and truncating, which is only safe for an O_APPEND writer.
    # shellcheck disable=SC2086 # the extra flags/env are deliberately word-split
    env $TS_TAILSCALED_ENV nohup "$BIN" --state="$D/state/tailscaled.state" --statedir="$D/state" \
        --socket="$SOCK" --port=41641 $TUNARG $TS_TAILSCALED_FLAGS >>"$LOG" 2>&1 &
    echo "$MODE" >"$MODE_FILE" 2>/dev/null
    rec_read
    if [ -z "$NOW" ] || [ "$R_BOOT" = "$NOW" ]; then rec_write "$NOW" "$N" "$STABLE" "$MODE"; fi
    log "started $BIN mode=$MODE ($WHY) strikes=$N first=$FIRST wan_wait=${_w}s"

    _i=0
    while [ ! -S "$SOCK" ] && [ "$_i" -lt 30 ]; do $SLEEP 1; _i=$((_i + 1)); done
    [ -S "$SOCK" ] || log "socket $SOCK not there after ${_i}s"
    run_bounded "${TSS_UP_LIMIT:-90}" "$D/tailscale" --socket="$SOCK" up ${TS_ROUTES:+--advertise-routes=$TS_ROUTES} \
        --accept-routes="$TS_ACCEPT_ROUTES" --accept-dns="$TS_ACCEPT_DNS" \
        ${TS_EXIT_NODE:+--exit-node=$TS_EXIT_NODE --exit-node-allow-lan-access} \
        --hostname="$TS_HOSTNAME" --timeout=60s >"$RUN/tailscale-up.log" 2>&1
    _rc=$?
    log "up rc=$_rc: $(tail -n 1 "$RUN/tailscale-up.log" 2>/dev/null)"

    if [ "$FIRST" = 1 ] && [ "$TIMER" = 1 ] && [ -n "$NOW" ]; then
        if [ "$WHY" = safe-mode ]; then _t=$TS_SAFE_RETRY; else _t=$TS_STABLE; fi
        ($SLEEP "$_t"; sh "$SELF" __stable "$NOW") </dev/null >/dev/null 2>&1 &
    fi
    watch_spawn
    return 0
}

# __stable <boot>: this boot stayed up long enough
stable() {
    load_config
    rec_read
    [ -n "$1" ] && [ "$R_BOOT" = "$1" ] && [ "$(boot_id)" = "$1" ] || return 0
    if [ -f "$SAFE" ] && [ "$R_MODE" = userspace ]; then
        rm -f "$SAFE"
        rec_write "$R_BOOT" $((TS_MAX_STRIKES - 1)) 1 "$R_MODE"
        log "safe mode stayed up ${TS_SAFE_RETRY}s: next boot tries TUN once more"
    elif [ "$R_MODE" = tun ] && rv_read && [ "$RV_TOTAL" -gt 0 ]; then
        # it was up at the timer, but only because the watcher restarted it
        log "TUN up ${TS_STABLE}s but restarted $RV_TOTAL time(s) this boot: not counted as stable"
    elif [ "$R_MODE" = tun ]; then
        rec_write "$R_BOOT" 0 1 "$R_MODE"
        [ "$R_STRIKES" = 0 ] || log "TUN stayed up ${TS_STABLE}s: strikes cleared"
    else
        rec_write "$R_BOOT" "$R_STRIKES" 1 "$R_MODE"
    fi
}

# ── front: what rc.local waits for ──────────────────────────────────────────
start() {
    mkdir -p "$D" 2>/dev/null
    if [ -f "$DISABLED" ]; then
        log "disabled ($DISABLED): not started"
        return 0
    fi
    rm -f "$STOPPED"
    $PIDOF tailscaled >/dev/null 2>&1 && return 0
    if ! mkdir "$LOCK" 2>/dev/null; then
        _o=$(cat "$LOCK/pid" 2>/dev/null)
        case "$_o" in '' | *[!0-9]*) _o= ;; esac
        if [ -n "$_o" ] && kill -0 "$_o" 2>/dev/null; then return 0; fi
        rm -rf "$LOCK"
        mkdir "$LOCK" 2>/dev/null || return 0
    fi
    if [ "$FG" = 1 ]; then
        echo $$ >"$LOCK/pid"
        worker
        rm -rf "$LOCK"
    else
        nohup sh "$SELF" __run </dev/null >/dev/null 2>&1 &
        echo $! >"$LOCK/pid"
    fi
    return 0
}

stop() {
    load_config
    # the watcher first, or it starts tailscaled again after the backoff
    : >"$STOPPED" 2>/dev/null
    _w=$(live_pid "$WATCH_PID") && [ "$_w" != $$ ] && kill "$_w" 2>/dev/null
    rm -f "$WATCH_PID"
    # a worker still waiting for the WAN would start tailscaled after us
    _w=$(cat "$LOCK/pid" 2>/dev/null)
    case "$_w" in '' | *[!0-9]*) ;; *) [ "$_w" != $$ ] && kill "$_w" 2>/dev/null ;; esac
    rm -rf "$LOCK"
    _p=$($PIDOF tailscaled)
    [ -n "$_p" ] && $KILL $_p 2>/dev/null
    _i=0
    while [ -n "$($PIDOF tailscaled)" ] && [ $_i -lt 15 ]; do $SLEEP 1; _i=$((_i + 1)); done
    _p=$($PIDOF tailscaled)
    [ -n "$_p" ] && $KILL -9 $_p 2>/dev/null
    # removes what a TUN tailscaled leaves behind: ip rules 52xx, table 52, ts-* chains
    run_bounded 30 "$BIN" --cleanup >/dev/null 2>&1
    _rc=$?
    rm -f "$MODE_FILE"
    log "stopped (${1:-stop}; cleanup rc=$_rc)"
}

status() {
    load_config
    rec_read
    echo "disabled:  $([ -f "$DISABLED" ] && echo yes || echo no)"
    echo "safe mode: $([ -f "$SAFE" ] && cat "$SAFE" || echo no)"
    echo "mode now:  $(cat "$MODE_FILE" 2>/dev/null || echo -)"
    echo "strikes:   $R_STRIKES of $TS_MAX_STRIKES (this boot stable: $([ "$R_BOOT" = "$(boot_id)" ] && echo "$R_STABLE" || echo -))"
    echo "running:   $($PIDOF tailscaled || echo no)"
    rv_read
    echo "watcher:   $(live_pid "$WATCH_PID" || echo no)$([ -f "$STOPPED" ] && echo ' (stopped by hand: no restarts)')"
    echo "restarts:  $RV_TOTAL this boot, $RV_WN of $TS_REVIVE_MAX in this window$([ "$RV_GAVEUP" = 1 ] && echo ', GAVE UP until the window ends')"
    echo "binary:    $BIN${BIN_NOTE:+ ($BIN_NOTE)}"
    [ -n "$TUNING_BAD" ] && echo "tuning.env: IGNORED (failed sh -n or a trial load)"
    return 0
}

check() {
    _bad=0
    ok() { echo "ok    $*"; }
    no() { echo "FAIL  $*"; _bad=1; }
    load_config
    if [ -n "$TUNING_BAD" ]; then no "$D/tuning.env: sh -n or trial load failed"; else ok "tuning.env"; fi
    if [ -n "$BIN_NOTE" ]; then no "$BIN_NOTE"; fi
    [ "$(basename "$BIN")" = tailscaled ] || no "$BIN: file name must be tailscaled (pidof finds it by name)"
    for _b in "$BIN" "$D/tailscale"; do
        if [ -x "$_b" ]; then ok "$_b executable"; else no "$_b missing or not executable"; fi
    done
    _dv=
    if [ -x "$BIN" ]; then
        run_bounded 10 "$BIN" --version >"$RUN/ts-check.out" 2>&1
        _rc=$?
        _dv=$(head -n 1 "$RUN/ts-check.out" 2>/dev/null)
        if [ "$_rc" = 0 ]; then ok "tailscaled --version: $_dv"; else no "tailscaled --version failed (rc $_rc)"; fi
    fi
    if [ -x "$D/tailscale" ]; then
        run_bounded 10 "$D/tailscale" version >"$RUN/ts-check.out" 2>&1
        _cv=$(head -n 1 "$RUN/ts-check.out" 2>/dev/null)
        [ -n "$_dv" ] && [ "$_cv" != "$_dv" ] && echo "warn  CLI $_cv != daemon $_dv"
    fi
    rm -f "$RUN/ts-check.out"
    _free=$($DF -Pk "$D" 2>/dev/null | awk 'NR > 1 && NF >= 6 { v = $4 } END { print v }')
    _free=$(num "$_free" "")
    if [ -z "$_free" ]; then echo "warn  free space on $D unknown"
    elif [ "$_free" -ge 102400 ]; then ok "$D free ${_free} KB"
    else no "$D free ${_free} KB < 100 MB"; fi
    if [ -c "$TUN" ]; then ok "$TUN"; else echo "warn  $TUN missing (start makes it, else userspace)"; fi
    if _o=$(old_state); then
        echo "warn  old state layout $_o: start moves it to $D/state/tailscaled.state"
    elif mkdir -p "$D/state" 2>/dev/null && : >"$D/state/.w" 2>/dev/null; then
        rm -f "$D/state/.w"; ok "$D/state writable"
    else no "$D/state not writable"; fi
    [ -f "$D/state/tailscaled.state" ] || old_state >/dev/null || echo "warn  no saved identity yet: log in once (README)"
    [ -f "$DISABLED" ] && echo "warn  disabled: start does nothing"
    [ -f "$SAFE" ] && echo "warn  safe mode: start uses userspace"
    return $_bad
}

case "${1:-start}" in
    start) start ;;
    # The long-lived arms exit here: ash reads a script as it goes, so a
    # start.sh rewritten in place under a running watcher must not be read on.
    # Replace it only by rename (cp to a temp name, mv).
    __run)
        worker
        rm -rf "$LOCK"
        exit 0
        ;;
    __stable) stable "$2" ;;
    __watch)
        watch
        exit 0
        ;;
    revive)
        load_config
        revive_tick
        ;;
    check) check ;;
    status) status ;;
    stop) stop stop ;;
    disable)
        mkdir -p "$D" && : >"$DISABLED" && sync
        stop disable
        ;;
    enable)
        rm -f "$DISABLED"
        log "enabled"
        start
        ;;
    clear-safe)
        load_config
        rm -f "$SAFE"
        rec_read
        rec_write "$R_BOOT" 0 "$R_STABLE" "${R_MODE:-pending}"
        log "safe mode cleared by hand"
        ;;
    *)
        sed -n '7,16p' "$SELF"
        exit 2
        ;;
esac
