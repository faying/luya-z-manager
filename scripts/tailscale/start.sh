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
# test hooks (scripts/tailscale/test/start.sh); defaults are the device's own
BOOT_ID_FILE=${TSS_BOOT_ID:-/proc/sys/kernel/random/boot_id}
RESOLV=${TSS_RESOLV:-/tmp/resolv.conf.d/resolv.conf.auto}
TUN=${TSS_TUN:-/dev/net/tun}
IP=${TSS_IP:-ip}
PIDOF=${TSS_PIDOF:-pidof}
KILL=${TSS_KILL:-kill}
SLEEP=${TSS_SLEEP:-sleep}
DF=${TSS_DF:-df}
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
    __run)
        worker
        rm -rf "$LOCK"
        ;;
    __stable) stable "$2" ;;
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
        sed -n '7,13p' "$SELF"
        exit 2
        ;;
esac
