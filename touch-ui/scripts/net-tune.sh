#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# net-tune.sh — CPU-path network tuning (manager docs/designs/net-performance.md 1b, D1/D15/R6)
#
#   sh /data/luyaz/scripts/net-tune.sh apply [iface]   tune (only when the want file exists)
#   sh /data/luyaz/scripts/net-tune.sh restore         put every recorded item back
#   sh /data/luyaz/scripts/net-tune.sh show            one status line per item
#
# The switch is /data/luyaz/net-tune.want (exists = on); the core API writes or
# removes it and then runs apply or restore. Callers: luyaz-core.init at start
# (a subprocess, capped at 5 s), chill.sh once chill0 is up (`apply chill0`),
# tailscale start.sh once tailscale0 is up (`apply tailscale0`). `apply <iface>`
# only does that interface's RPS.
#
# Items: the sysctls in ITEMS below, and RPS (rps_cpus of every rx queue) on
# br-lan, chill0 and tailscale0. rmnet_data0 (stock mask 6, rmnet_sch, IPA) and
# wlan0 are never touched.
#
# Baseline (/data/luyaz/net-tune.baseline): each item is recorded the first time
# it is seen and never overwritten, so an interface that appears later
# (chill0/tailscale0 are rebuilt on every start) still gets its own original
# value. Before a sysctl or a queue is recorded its value is checked against the
# stock table (10-24/10-25 read-outs of both devices, B31); a mismatch means
# someone else set it, so that item is refused (not recorded, not changed) and
# reported. The intent file lists what is about to change and is written first;
# restore does not trust any completion marker: it writes back every recorded
# item that still exists. apply and restore share one lock (busybox flock has
# only -sxun: `flock -n`, retried for up to 5 s, then give up and report).
#
# sysctls do not survive a reboot: after one the device is at stock again until
# the next apply. Exit: 0 ok (or off), 1 an item was refused or failed, 2 usage,
# 3 lock not obtained.
#
# Env (tests): NT_SYSCTL (/proc/sys), NT_NET (/sys/class/net), NT_WANT,
# NT_BASELINE, NT_INTENT, NT_LOCK, NT_LOCK_TRIES (25 × 0.2 s), NT_IFACES.
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
# ─────────────────────────────────────────────────────────────────────────────

SYS=${NT_SYSCTL:-/proc/sys}
NET=${NT_NET:-/sys/class/net}
WANT=${NT_WANT:-/data/luyaz/net-tune.want}
BASE=${NT_BASELINE:-/data/luyaz/net-tune.baseline}
INTENT=${NT_INTENT:-/data/luyaz/net-tune.intent}
LOCK=${NT_LOCK:-/tmp/luyaz-net-tune.lock}
TRIES=${NT_LOCK_TRIES:-25}
case $TRIES in '' | *[!0-9]*) TRIES=25 ;; esac
IFACES=${NT_IFACES:-br-lan chill0 tailscale0}

# RPS mask e = CPUs 1-3 (4× A55): CPU0 takes most hardware interrupts (Wi-Fi,
# IPA), so the protocol work moves off it; rmnet_data0's stock mask is 6.
RPS_MASK=e
# key|stock value|target ("keep max" sysctls: min and default stay, max changes;
# the target is built from the value that was there)
#   netdev_max_backlog 4096: the per-CPU backlog RPS queues into. At ~500 Mbps
#   that is ~45k packets/s, so 1000 is ~20 ms of a burst while the target CPU is
#   busy (drops show in /proc/net/softnet_stat col 2); 4096 is ~90 ms, worst
#   case ~8 MB per CPU, nothing at idle.
ITEMS="net.ipv4.tcp_slow_start_after_idle|1|0
net.ipv4.tcp_rmem|4096 131072 6291456|max:16777216
net.ipv4.tcp_wmem|4096 16384 4194304|max:16777216
net.core.rmem_max|229376|8388608
net.core.wmem_max|229376|8388608
net.ipv4.tcp_notsent_lowat|4294967295|131072
net.core.netdev_max_backlog|1000|4096"

say() { echo "net-tune: $*"; }
norm() { tr -s '\t ' '  ' | sed 's/^ //; s/ $//'; }
sys_path() { echo "$SYS/$(echo "$1" | tr . /)"; }
get() { [ -r "$1" ] && norm <"$1"; }
# a mask is "stock" when it is all zeros (0, 00, 0,00000000)
mask_zero() { case $1 in '' | *[!0,]*) return 1 ;; esac; return 0; }
mask_strip() { echo "$1" | sed 's/^[0,]*//'; }

# baseline lines: "s <key>=<value>" and "r <iface>/<queue>=<value>"
base_get() { grep -F -- "$1 $2=" "$BASE" 2>/dev/null | head -n 1 | sed 's/^[^=]*=//'; }
base_has() { grep -qF -- "$1 $2=" "$BASE" 2>/dev/null; }
base_add() { echo "$1 $2=$3" >>"$BASE" && sync; }

target_of() { # <target spec> <baseline value>
    case $1 in
    max:*) echo "$2" | awk -v m="${1#max:}" 'NF == 3 { print $1, $2, m; ok = 1 } END { exit !ok }' ;;
    *) echo "$1" ;;
    esac
}

lock() {
    : 2>/dev/null >>"$LOCK" || { say "lock $LOCK: cannot open"; exit 3; }
    exec 9>>"$LOCK" || { say "lock $LOCK: cannot open"; exit 3; }
    _n=0
    while ! flock -n 9; do
        _n=$((_n + 1))
        if [ "$_n" -gt "$TRIES" ]; then
            say "lock busy (another apply/restore); gave up after 5 s"
            exit 3
        fi
        sleep 0.2
    done
}

RC=0
# items to change go to $TODO as "path|value|label"
plan_sysctls() {
    echo "$ITEMS" | while IFS='|' read -r key stock spec; do
        [ -n "$key" ] || continue
        p=$(sys_path "$key")
        cur=$(get "$p") || { echo "skip|$key|missing"; continue; }
        if ! base_has s "$key"; then
            if [ "$cur" != "$stock" ]; then
                echo "refuse|$key|current [$cur] is not stock [$stock]"
                continue
            fi
            base_add s "$key" "$cur" || { echo "refuse|$key|cannot write $BASE"; continue; }
        fi
        b=$(base_get s "$key")
        t=$(target_of "$spec" "$b") || { echo "refuse|$key|baseline [$b] not min/default/max"; continue; }
        [ "$cur" = "$t" ] || echo "set|$p|$t|$key"
    done
}

plan_rps() { # <iface>
    for q in "$NET/$1"/queues/rx-*; do
        [ -e "$q/rps_cpus" ] || continue
        id="$1/${q##*/}"
        cur=$(get "$q/rps_cpus") || continue
        if ! base_has r "$id"; then
            if ! mask_zero "$cur"; then
                echo "refuse|$id|current rps [$cur] is not stock 0"
                continue
            fi
            base_add r "$id" "$cur" || { echo "refuse|$id|cannot write $BASE"; continue; }
        fi
        [ "$(mask_strip "$cur")" = "$RPS_MASK" ] && continue
        echo "set|$q/rps_cpus|$RPS_MASK|$id"
    done
}

do_apply() { # [iface]
    if [ ! -f "$WANT" ]; then say "off (no $WANT): nothing changed"; return 0; fi
    if [ -n "${1:-}" ]; then
        case " $IFACES " in *" $1 "*) ;; *) say "$1: not a tuned interface ($IFACES)"; return 2 ;; esac
    fi
    lock
    mkdir -p "${BASE%/*}" 2>/dev/null
    _plan=$(mktemp "${INTENT}.plan.XXXXXX") || { say "cannot make a temp file"; return 1; }
    if [ -n "${1:-}" ]; then plan_rps "$1" >"$_plan"; else
        plan_sysctls >"$_plan"
        for i in $IFACES; do plan_rps "$i"; done >>"$_plan"
    fi
    grep '^refuse|' "$_plan" | while IFS='|' read -r _ what why; do say "refused $what: $why"; done
    grep -q '^refuse|' "$_plan" && RC=1
    # the intent first (what is about to change), then the values
    { echo "apply $(date +%Y-%m-%dT%H:%M:%S) ${1:-all}"; grep '^set|' "$_plan" | cut -d'|' -f4; } >"$INTENT.tmp" &&
        mv -f "$INTENT.tmp" "$INTENT" && sync
    _n=0
    while IFS='|' read -r kind p v label; do
        [ "$kind" = set ] || continue
        if echo "$v" >"$p" 2>/dev/null; then _n=$((_n + 1)); else say "failed $label <- $v"; RC=1; fi
    done <"$_plan"
    rm -f "$_plan"
    echo "done" >>"$INTENT"
    say "applied ${1:-all}: $_n changed"
    return $RC
}

do_restore() {
    lock
    { echo "restore $(date +%Y-%m-%dT%H:%M:%S)"; } >"$INTENT.tmp" && mv -f "$INTENT.tmp" "$INTENT"
    [ -f "$BASE" ] || { say "nothing recorded"; echo done >>"$INTENT"; return 0; }
    _f=$(mktemp "${INTENT}.plan.XXXXXX") || { say "cannot make a temp file"; return 1; }
    _n=0
    # every recorded item, whatever the intent file says
    while read -r kind rest; do
        id=${rest%%=*} v=${rest#*=}
        case $kind in
        s) p=$(sys_path "$id") ;;
        r) p="$NET/${id%%/*}/queues/${id#*/}/rps_cpus" ;;
        *) continue ;;
        esac
        [ -e "$p" ] || { say "gone $id (not restored)"; continue; }
        [ "$(get "$p")" = "$v" ] && continue
        if echo "$v" >"$p" 2>/dev/null; then _n=$((_n + 1)); else say "failed restoring $id <- $v"; echo x >>"$_f"; fi
    done <"$BASE"
    [ -s "$_f" ] && RC=1
    rm -f "$_f"
    echo done >>"$INTENT"
    say "restored: $_n changed"
    return $RC
}

# show: "net-tune on|off" then "<id> cur=<v> base=<v|-> [tuned|stock|other]" per item;
# values with spaces use commas
do_show() {
    if [ -f "$WANT" ]; then echo "net-tune on"; else echo "net-tune off"; fi
    echo "$ITEMS" | while IFS='|' read -r key stock spec; do
        cur=$(get "$(sys_path "$key")") || cur=-
        b=$(base_get s "$key"); [ -n "$b" ] || b=-
        if [ "$b" != - ]; then t=$(target_of "$spec" "$b"); else t=$(target_of "$spec" "$stock"); fi
        st=other
        [ "$cur" = "$t" ] && st=tuned
        [ "$cur" = "$stock" ] && st=stock
        echo "$key cur=$(echo "$cur" | tr ' ' ,) base=$(echo "$b" | tr ' ' ,) $st"
    done
    for i in $IFACES; do
        _any=0
        for q in "$NET/$i"/queues/rx-*; do
            [ -e "$q/rps_cpus" ] || continue
            _any=1
            id="$i/${q##*/}"
            cur=$(get "$q/rps_cpus")
            b=$(base_get r "$id"); [ -n "$b" ] || b=-
            st=other
            mask_zero "$cur" && st=stock
            [ "$(mask_strip "$cur")" = "$RPS_MASK" ] && st=tuned
            echo "rps:$id cur=$cur base=$b $st"
        done
        [ $_any = 1 ] || echo "rps:$i absent"
    done
    [ -f "$INTENT" ] && echo "intent $(head -n 1 "$INTENT" | tr ' ' _) $(tail -n 1 "$INTENT")"
    return 0
}

case ${1:-} in
apply) do_apply "${2:-}" ;;
restore) do_restore ;;
show) do_show ;;
*) echo "usage: net-tune.sh apply [iface] | restore | show" >&2; exit 2 ;;
esac
