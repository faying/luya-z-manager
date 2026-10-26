#!/bin/sh
# net-tune.sh (manager docs/designs/net-performance.md 1b, R6/D15) on fake /proc/sys and
# /sys/class/net trees: apply/restore/show, the baseline per item, an interface that appears
# later, lock contention, a non-stock value refused, restore after a partial apply.
# Busybox container: scripts/test/docker.sh.
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

SCRIPTS=${SCRIPTS:-/scripts}
NT=$SCRIPTS/net-tune.sh
PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); echo "  ok   $1"; }
bad() { FAIL=$((FAIL + 1)); echo "  FAIL $1"; }
check() { _d=$1; shift; if eval "$@"; then ok "$_d"; else bad "$_d  [$*]"; fi; }

T=$(mktemp -d)
export NT_SYSCTL=$T/sys NT_NET=$T/net NT_WANT=$T/want NT_BASELINE=$T/baseline NT_INTENT=$T/intent NT_LOCK=$T/lock NT_LOCK_TRIES=5

# the stock values both devices read on 10-25 (tabs as in /proc)
stock() {
    rm -rf "$T/sys" "$T/net" "$T/baseline" "$T/intent"
    mkdir -p "$T/sys/net/ipv4" "$T/sys/net/core"
    echo 1 >"$T/sys/net/ipv4/tcp_slow_start_after_idle"
    printf '4096\t131072\t6291456\n' >"$T/sys/net/ipv4/tcp_rmem"
    printf '4096\t16384\t4194304\n' >"$T/sys/net/ipv4/tcp_wmem"
    echo 229376 >"$T/sys/net/core/rmem_max"
    echo 229376 >"$T/sys/net/core/wmem_max"
    echo 4294967295 >"$T/sys/net/ipv4/tcp_notsent_lowat"
    echo 1000 >"$T/sys/net/core/netdev_max_backlog"
    iface br-lan rx-0
    iface rmnet_data0 rx-0 6
    iface wlan0 rx-0
}
iface() { # <name> <queue> [mask]
    mkdir -p "$T/net/$1/queues/$2"
    echo "${3:-0}" >"$T/net/$1/queues/$2/rps_cpus"
}
v() { tr '\t' ' ' <"$T/sys/$1"; }
rps() { cat "$T/net/$1/queues/rx-0/rps_cpus"; }
run() { sh "$NT" "$@" >"$T/out" 2>&1; RC=$?; }

echo "## off"
stock
run apply
check "no want: exit 0, says off" '[ $RC = 0 ] && grep -q "off" "$T/out"'
check "no want: nothing changed, no baseline" '[ "$(v net/ipv4/tcp_slow_start_after_idle)" = 1 ] && [ ! -e "$T/baseline" ]'
run show
check "show off" 'head -n 1 "$T/out" | grep -qx "net-tune off"'

echo "## apply"
touch "$T/want"
run apply
check "apply: exit 0" '[ $RC = 0 ]'
check "slow start after idle 0" '[ "$(v net/ipv4/tcp_slow_start_after_idle)" = 0 ]'
check "tcp_rmem min/default kept, max 16 MB" '[ "$(v net/ipv4/tcp_rmem)" = "4096 131072 16777216" ]'
check "tcp_wmem min/default kept, max 16 MB" '[ "$(v net/ipv4/tcp_wmem)" = "4096 16384 16777216" ]'
check "rmem_max/wmem_max 8 MB" '[ "$(v net/core/rmem_max)" = 8388608 ] && [ "$(v net/core/wmem_max)" = 8388608 ]'
check "notsent_lowat, backlog" '[ "$(v net/ipv4/tcp_notsent_lowat)" = 131072 ] && [ "$(v net/core/netdev_max_backlog)" = 4096 ]'
check "br-lan RPS e" '[ "$(rps br-lan)" = e ]'
check "rmnet_data0 and wlan0 untouched" '[ "$(rps rmnet_data0)" = 6 ] && [ "$(rps wlan0)" = 0 ]'
check "baseline: 7 sysctls + br-lan" '[ "$(grep -c "^s " "$T/baseline")" = 7 ] && grep -qx "r br-lan/rx-0=0" "$T/baseline"'
check "baseline keeps stock rmem" 'grep -qx "s net.ipv4.tcp_rmem=4096 131072 6291456" "$T/baseline"'
check "intent lists the items, ends with done" 'grep -qx net.core.rmem_max "$T/intent" && tail -n 1 "$T/intent" | grep -qx done'
cp "$T/baseline" "$T/b1"
run apply
check "second apply: nothing changed, baseline not rewritten" '[ $RC = 0 ] && grep -q "0 changed" "$T/out" && cmp -s "$T/b1" "$T/baseline"'
run show
check "show on, items tuned" 'head -n 1 "$T/out" | grep -qx "net-tune on" && grep -qx "net.ipv4.tcp_rmem cur=4096,131072,16777216 base=4096,131072,6291456 tuned" "$T/out"'
check "show: rps line, absent chill0" 'grep -qx "rps:br-lan/rx-0 cur=e base=0 tuned" "$T/out" && grep -qx "rps:chill0 absent" "$T/out"'

echo "## late interface"
iface chill0 rx-0
run apply chill0
check "apply chill0: only its RPS" '[ $RC = 0 ] && [ "$(rps chill0)" = e ] && grep -q "1 changed" "$T/out"'
check "chill0 recorded once" '[ "$(grep -c "^r chill0/" "$T/baseline")" = 1 ]'
# chill0 rebuilt by the next CHILL start: RPS 0 again, recorded value kept
echo 0 >"$T/net/chill0/queues/rx-0/rps_cpus"
run apply chill0
check "rebuilt chill0 tuned again, still one record" '[ "$(rps chill0)" = e ] && [ "$(grep -c "^r chill0/" "$T/baseline")" = 1 ]'
run apply rmnet_data0
check "apply rmnet_data0 refused (usage)" '[ $RC = 2 ] && [ "$(rps rmnet_data0)" = 6 ]'
iface tailscale0 rx-0
rm -f "$T/want"
run apply tailscale0
check "apply tailscale0 with tuning off: nothing" '[ "$(rps tailscale0)" = 0 ] && ! grep -q tailscale0 "$T/baseline"'
touch "$T/want"

echo "## restore"
run restore
check "restore: exit 0" '[ $RC = 0 ]'
check "restore: all sysctls stock" '[ "$(v net/ipv4/tcp_slow_start_after_idle)" = 1 ] && [ "$(v net/ipv4/tcp_rmem)" = "4096 131072 6291456" ] && [ "$(v net/core/netdev_max_backlog)" = 1000 ] && [ "$(v net/ipv4/tcp_notsent_lowat)" = 4294967295 ]'
check "restore: br-lan and chill0 RPS 0" '[ "$(rps br-lan)" = 0 ] && [ "$(rps chill0)" = 0 ]'
rm -rf "$T/net/chill0"
run restore
check "restore with chill0 gone: ok, reported" '[ $RC = 0 ] && grep -q "gone chill0/rx-0" "$T/out"'

echo "## refuse a non-stock value"
stock
touch "$T/want"
echo 2000 >"$T/sys/net/core/netdev_max_backlog"
iface br-lan rx-0 3
run apply
check "apply: exit 1, refusal reported" '[ $RC = 1 ] && grep -q "refused net.core.netdev_max_backlog" "$T/out" && grep -q "refused br-lan/rx-0" "$T/out"'
check "refused items untouched and not recorded" '[ "$(v net/core/netdev_max_backlog)" = 2000 ] && ! grep -q netdev_max_backlog "$T/baseline" && [ "$(rps br-lan)" = 3 ] && ! grep -q br-lan "$T/baseline"'
check "the other items applied" '[ "$(v net/core/rmem_max)" = 8388608 ]'
run restore
check "restore leaves the refused items alone" '[ "$(v net/core/netdev_max_backlog)" = 2000 ] && [ "$(v net/core/rmem_max)" = 229376 ]'

echo "## lock"
stock
touch "$T/want"
( exec 9>>"$T/lock"; flock -x 9; sleep 3 ) &
H=$!
sleep 0.3
run apply
check "lock held: gives up, exit 3, nothing changed" '[ $RC = 3 ] && grep -q "lock busy" "$T/out" && [ "$(v net/core/rmem_max)" = 229376 ]'
wait $H
( exec 9>>"$T/lock"; flock -x 9; sleep 0.5 ) &
H=$!
sleep 0.2
run apply
check "lock freed within the retries: applied" '[ $RC = 0 ] && [ "$(v net/core/rmem_max)" = 8388608 ]'
wait $H

echo "## restore after a partial apply"
stock
touch "$T/want"
# an apply that was killed half-way: baseline and intent written, some values changed, no done
run apply
echo 1 >"$T/sys/net/ipv4/tcp_slow_start_after_idle"
printf 'apply 2026-10-25T00:00:00 all\nnet.core.rmem_max\n' >"$T/intent"
run restore
check "restore ignores the missing done: everything back" '[ $RC = 0 ] && [ "$(v net/core/rmem_max)" = 229376 ] && [ "$(v net/ipv4/tcp_rmem)" = "4096 131072 6291456" ] && [ "$(rps br-lan)" = 0 ]'
check "restore wrote its own intent" 'head -n 1 "$T/intent" | grep -q "^restore " && tail -n 1 "$T/intent" | grep -qx done'
# baseline exists, the values changed by hand afterwards: restore still writes the recorded ones
echo 777 >"$T/sys/net/core/wmem_max"
run restore
check "restore writes recorded values over later changes" '[ "$(v net/core/wmem_max)" = 229376 ]'
run bogus
check "usage: exit 2" '[ $RC = 2 ]'

rm -rf "$T"
echo "passed $PASS, failed $FAIL"
[ "$FAIL" = 0 ]
