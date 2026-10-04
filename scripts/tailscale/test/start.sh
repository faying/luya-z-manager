#!/bin/sh
# start.sh against stubbed tailscaled/tailscale/ip/pidof/kill/df (busybox container):
#   docker run --rm -v "$PWD":/w busybox sh /w/scripts/tailscale/test/start.sh
# Failure injection for the boot-safety rules in the header of ../start.sh.
# SPDX-License-Identifier: MIT
HERE=$(cd "$(dirname "$0")/.." && pwd)
S=$HERE/start.sh
PASS=0; FAIL=0
ok() { PASS=$((PASS + 1)); echo "  ok   $1"; }
bad() { FAIL=$((FAIL + 1)); echo "  FAIL $1"; }
check() { _d=$1; shift; if eval "$@"; then ok "$_d"; else bad "$_d  [$*]"; fi; }

setup() {
    T=$(mktemp -d); D=$T/d; mkdir -p "$D" "$T/bin" "$T/run"
    echo aaaa-0001 >"$T/boot"
    echo 'default via 10.6.0.1 dev rmnet_data0 proto static' >"$T/route"
    echo 10.6.0.2/28 >"$T/wanaddr"
    printf '# Interface zte_wan\nnameserver 202.0.0.1\n' >"$T/resolv"
    # tailscaled: --version, --cleanup, else "start" (records its arguments)
    cat >"$D/tailscaled" <<X
#!/bin/sh
case "\$1" in
  --version) echo 1.0.0; exit 0 ;;
  --cleanup) touch $T/cleaned; exit 0 ;;
esac
echo "\$0 \$*" >> $T/daemon.args; touch $T/running
X
    cat >"$D/tailscale" <<X
#!/bin/sh
case "\$2" in
  up) echo "\$*" >> $T/up.args; [ -f $T/uphang ] && exec sleep 1000; echo "up done"; exit 0 ;;
esac
[ "\$1" = version ] && echo 1.0.0
X
    cat >"$T/bin/ip" <<X
#!/bin/sh
case "\$*" in
  '-4 route show default') cat $T/route ;;
  '-4 -o addr show dev '*) echo "5: \$6    inet \$(cat $T/wanaddr) scope global \$6" ;;
  '-o -4 addr show br-lan') echo '3: br-lan    inet 10.9.9.1/24 brd 10.9.9.255 scope global br-lan' ;;
esac
X
    printf '#!/bin/sh\n[ -f %s/running ] && echo 4242\n' "$T" >"$T/bin/pidof"
    cat >"$T/bin/kill" <<X
#!/bin/sh
echo "\$*" >> $T/kills
[ "\$1" = -9 ] && { rm -f $T/running; exit 0; }
[ -f $T/stubborn ] || rm -f $T/running
X
    printf '#!/bin/sh\necho "Filesystem 1024-blocks Used Available Capacity Mounted"\necho "/dev/x 1800000 700000 $(cat %s/free) 37%% /data"\n' "$T" >"$T/bin/df"
    echo 1100000 >"$T/free"
    chmod +x "$D/tailscaled" "$D/tailscale" "$T"/bin/*
    export TS_DIR=$D TS_LOG=$T/tailscaled.log TSS_SOCK=$T/sock TSS_RUN=$T/run TSS_BOOT_ID=$T/boot \
        TSS_RESOLV=$T/resolv TSS_TUN=$T/tun TSS_IP=$T/bin/ip TSS_PIDOF=$T/bin/pidof TSS_KILL=$T/bin/kill \
        TSS_DF=$T/bin/df TSS_SLEEP=true TSS_FG=1 TSS_TIMER=0
    unset TS_STABLE TS_MAX_STRIKES TS_SAFE_RETRY TS_WAN_WAIT
    mknod "$T/tun" c 1 3 2>/dev/null || mkfifo "$T/tun"
}
run() { sh "$S" "$@" >"$T/out" 2>&1; RC=$?; }
reboot() { # <new boot id>: processes and /tmp are gone
    echo "$1" >"$T/boot"; rm -f "$T/running" "$T/daemon.args"; rm -rf "$T/run"; mkdir -p "$T/run"
}
last_args() { tail -n 1 "$T/daemon.args" 2>/dev/null; }
strikes() { sed -n 's/.*strikes=\([0-9]*\).*/\1/p' "$D/boot-strikes"; }

echo "- normal boot"
setup; run
check "exit 0" '[ $RC = 0 ]'
check "kernel TUN, same flags as before" 'last_args | grep -q -- "--tun=tailscale0" && last_args | grep -q -- "--port=41641 --tun=tailscale0"'
check "up advertises the br-lan /24, default prefs" 'grep -q -- "--advertise-routes=10.9.9.0/24 --accept-routes=true --accept-dns=false --hostname=u60pro" "$T/up.args"'
check "boot log written" 'grep -q "mode=tun (configured) strikes=0 first=1" "$D/boot.log"'
check "mode file" '[ "$(cat "$T/run/tailscale.mode")" = tun ]'
check "lock released" '[ ! -d "$T/run/tailscale-start.lock" ]'

echo "- already running / same boot"
setup; run; rm -f "$T/daemon.args"; run
check "second start in the same boot starts nothing" '[ ! -f "$T/daemon.args" ]'
rm -f "$T/running"; run
check "apply.sh-style restart in the same boot: started, no strike" '[ -f "$T/daemon.args" ] && [ "$(strikes)" = 0 ]'

echo "- disabled"
setup; touch "$D/disabled"; run
check "not started" '[ $RC = 0 ] && [ ! -f "$T/daemon.args" ]'
check "said so in the boot log" 'grep -q "disabled" "$D/boot.log"'
rm -f "$D/disabled"; touch "$T/running"; run disable
check "disable: flag written, stopped, cleaned up" '[ -f "$D/disabled" ] && [ ! -f "$T/running" ] && [ -f "$T/cleaned" ]'
run enable
check "enable: flag gone, started" '[ ! -f "$D/disabled" ] && [ -f "$T/daemon.args" ]'

echo "- boot strikes → safe mode (userspace), and back"
setup
run; reboot b2; run
check "boot 2 after an unstable TUN boot: 1 strike" '[ "$(strikes)" = 1 ]'
reboot b3; run; reboot b4; run
check "boot 4: safe mode" '[ -f "$D/safe-mode" ]'
check "boot 4: userspace" 'last_args | grep -q -- "--tun=userspace-networking"'
check "boot 4: logged" 'grep -q "SAFE MODE: 3 strikes" "$D/boot.log"'
reboot b5; run
check "safe mode is sticky" 'last_args | grep -q userspace'
run __stable b5
check "safe mode stayed up: probation (safe-mode gone, strikes 2)" '[ ! -f "$D/safe-mode" ] && [ "$(strikes)" = 2 ]'
reboot b6; run
check "probation boot tries TUN" 'last_args | grep -q -- "--tun=tailscale0"'
reboot b7; run
check "one more failure: safe mode again" '[ -f "$D/safe-mode" ] && last_args | grep -q userspace'
run clear-safe
check "clear-safe" '[ ! -f "$D/safe-mode" ] && [ "$(strikes)" = 0 ]'

setup; run; run __stable aaaa-0001
check "stable TUN boot: strikes 0, stable=1" '[ "$(strikes)" = 0 ] && grep -q "stable=1" "$D/boot-strikes"'
reboot b2; run
check "after a stable boot: no strike" '[ "$(strikes)" = 0 ]'
run __stable wrong-boot
check "__stable for another boot does nothing" 'grep -q "stable=0" "$D/boot-strikes"'

echo "- corrupt files"
for junk in 'boot=x strikes=abc stable=1 mode=tun' 'strikes=99999999999999' '*' "$(printf '\001\377 strikes=-3')" ''; do
    setup; printf '%s\n' "$junk" >"$D/boot-strikes"; run
    check "boot-strikes '$(echo "$junk" | tr -dc 'a-z0-9=* ')': started TUN, no shell error" \
        '[ $RC = 0 ] && last_args | grep -q tailscale0 && ! grep -qi -e "arithmetic" -e "syntax" "$T/out"'
done
setup; echo 'TS_TAILSCALED_FLAGS="--oops' >"$D/tuning.env"; run
check "tuning.env syntax error: ignored, started with defaults" '[ $RC = 0 ] && [ -f "$T/daemon.args" ] && grep -q "tuning.env failed" "$D/boot.log"'
setup; echo 'exit 3' >"$D/tuning.env"; run
check "tuning.env that exits: ignored" '[ -f "$T/daemon.args" ] && grep -q "tuning.env failed" "$D/boot.log"'
setup; echo 'TS_TAILSCALED_FLAGS=--no-logs-no-support' >"$D/tuning.env"; run
check "good tuning.env applied" 'last_args | grep -q -- "--no-logs-no-support"'
setup; echo "TS_TAILSCALED_BIN=$D/nofight/tailscaled" >"$D/tuning.env"; run
check "missing TS_TAILSCALED_BIN: falls back to \$D/tailscaled" 'last_args | grep -q "^$D/tailscaled " && grep -q "not executable" "$D/boot.log"'
setup; echo 'TS_MODE=bogus' >"$D/tuning.env"; run
check "unknown TS_MODE: tun" 'last_args | grep -q tailscale0'
setup; echo 'TS_MODE=userspace' >"$D/tuning.env"; run; reboot b2; run
check "configured userspace: no strikes counted" '[ "$(strikes)" = 0 ] && last_args | grep -q userspace'

echo "- prefs and old identity layouts"
setup; printf 'TS_ACCEPT_ROUTES=false\nTS_ACCEPT_DNS=true\nTS_EXIT_NODE=100.70.0.9\nTS_HOSTNAME=n1\n' >"$D/tuning.env"; run
check "accept-routes/DNS/exit node from tuning.env" 'grep -q -- "--accept-routes=false --accept-dns=true --exit-node=100.70.0.9 --exit-node-allow-lan-access --hostname=n1" "$T/up.args"'
setup; echo 'TS_ACCEPT_ROUTES=maybe' >"$D/tuning.env"; run
check "unknown accept-routes value: true" 'grep -q -- "--accept-routes=true" "$T/up.args"'
setup; echo ID1 >"$D/state"; run check
check "check: old state file named state is a warning, not touched" 'grep -q "old state layout $D/state" "$T/out" && [ -f "$D/state" ]'
run
check "state file named state moved into state/tailscaled.state" '[ "$(cat "$D/state/tailscaled.state")" = ID1 ] && grep -q "moved old state" "$D/boot.log" && last_args | grep -q -- "--state=$D/state/tailscaled.state"'
setup; echo ID2 >"$D/tailscaled.state"; run
check "tailscaled.state moved into state/" '[ "$(cat "$D/state/tailscaled.state")" = ID2 ] && [ ! -f "$D/tailscaled.state" ]'
setup; mkdir -p "$D/state"; echo NEW >"$D/state/tailscaled.state"; echo OLD >"$D/tailscaled.state"; run
check "new layout present: old file left alone" '[ "$(cat "$D/state/tailscaled.state")" = NEW ] && [ -f "$D/tailscaled.state" ]'
setup; echo ID3 >"$D/state"; touch "$T/running"; run
check "never moved while tailscaled runs" '[ -f "$D/state" ]'
setup; run check
check "check: no identity yet is a warning" '[ $RC = 0 ] && grep -q "no saved identity" "$T/out"'

echo "- network"
setup; echo 100.72.1.2/30 >"$T/wanaddr"; run
check "CGNAT WAN address: userspace this boot, not sticky" 'last_args | grep -q userspace && [ ! -f "$D/safe-mode" ] && grep -q "cgnat 100.72.1.2" "$D/boot.log"'
setup; echo 'default via 100.127.0.1 dev rmnet_data0' >"$T/route"; run
check "CGNAT gateway: userspace" 'last_args | grep -q userspace'
setup; printf 'nameserver 100.64.0.53\n' >"$T/resolv"; run
check "CGNAT DNS: userspace" 'last_args | grep -q userspace'
setup; printf 'nameserver 100.100.100.100\nnameserver 100.128.0.1\nnameserver 100.63.255.255\n' >"$T/resolv"; run
check "100.100.100.100 and addresses just outside /10 are not CGNAT" 'last_args | grep -q tailscale0'
setup; : >"$T/route"; export TS_WAN_WAIT=6; run; unset TS_WAN_WAIT
check "no WAN: started anyway after the wait" 'last_args | grep -q tailscale0 && grep -q "no default route after 6s" "$D/boot.log"'
setup; export TSS_TUN=/proc/nonexistent/tun; run
check "no TUN device possible: userspace" 'last_args | grep -q userspace && grep -q "(no-tun)" "$D/boot.log"'

echo "- bounded"
setup; touch "$T/uphang"; export TSS_UP_LIMIT=2; run; unset TSS_UP_LIMIT
check "up that hangs is killed (rc 124), start still returns" '[ $RC = 0 ] && grep -q "up rc=124" "$D/boot.log"'
setup; : >"$T/route"; export TSS_FG=0 TSS_SLEEP=sleep TS_WAN_WAIT=4
t0=$(date +%s); sh "$S"; t1=$(date +%s)
check "front returns at once while the worker waits for the WAN" '[ $((t1 - t0)) -le 1 ]'
i=0; while [ ! -f "$T/daemon.args" ] && [ $i -lt 20 ]; do sleep 1; i=$((i + 1)); done
check "detached worker started tailscaled later" '[ -f "$T/daemon.args" ]'
i=0; while [ -d "$T/run/tailscale-start.lock" ] && [ $i -lt 60 ]; do sleep 1; i=$((i + 1)); done
check "worker released the lock" '[ ! -d "$T/run/tailscale-start.lock" ]'
setup; : >"$T/route"; export TSS_FG=0 TSS_SLEEP=sleep TS_WAN_WAIT=6
sh "$S"; sleep 1; sh "$S" disable >/dev/null 2>&1
i=0; while [ -d "$T/run/tailscale-start.lock" ] && [ $i -lt 20 ]; do sleep 1; i=$((i + 1)); done; sleep 7
check "disable during the WAN wait: worker never starts tailscaled" '[ ! -f "$T/daemon.args" ] && [ -f "$D/disabled" ]'
setup; : >"$T/route"; export TSS_FG=0 TSS_SLEEP=sleep TS_WAN_WAIT=4
sh "$S"; sleep 1; touch "$D/disabled"
i=0; while [ -d "$T/run/tailscale-start.lock" ] && [ $i -lt 20 ]; do sleep 1; i=$((i + 1)); done
check "disabled flag appearing mid-wait is honoured by the worker" '[ ! -f "$T/daemon.args" ] && grep -q "disabled while waiting" "$D/boot.log"'
export TSS_FG=1 TSS_SLEEP=true; unset TS_WAN_WAIT

echo "- apply.sh driving the real start.sh"
setup; echo 'TS_TAILSCALED_FLAGS=--no-logs-no-support' >"$D/tuning.env"; run
printf '#!/bin/sh\ncat >/dev/null\ncase "$2" in @.BackendState) echo Running ;; @.Self.Online) echo true ;; esac\n' >"$T/bin/jf"; chmod +x "$T/bin/jf"
rm -f "$T/daemon.args"
TSA_DIR=$D TSA_CLI=$D/tailscale TSA_START=$S TSA_LOG=$T/apply.log TSA_TIMEOUT=30 TSA_POLL=10 TSA_JSONFILTER=$T/bin/jf \
    TSA_PIDOF=$T/bin/pidof TSA_KILL=$T/bin/kill TSA_SLEEP=true sh "$HERE/apply.sh" "$D/tuning.env" relay >/dev/null 2>&1
ARC=$?
check "apply.sh same tuning: kept, restarted with the same flags, no strike" \
    '[ $ARC = 0 ] && last_args | grep -q -- "--tun=tailscale0 --no-logs-no-support" && [ "$(strikes)" = 0 ] && grep -q kept "$T/apply.log"'
setup; mkdir -p "$T/run/tailscale-start.lock"; echo 999999 >"$T/run/tailscale-start.lock/pid"; run
check "stale lock (dead pid) is taken over" '[ -f "$T/daemon.args" ]'
setup; mkdir -p "$T/run/tailscale-start.lock"; echo $$ >"$T/run/tailscale-start.lock/pid"; run
check "live lock: a second start does nothing" '[ ! -f "$T/daemon.args" ]'

echo "- stop"
setup; touch "$T/running" "$T/stubborn"; run stop
check "stop: TERM, then KILL, then --cleanup" 'head -n 1 "$T/kills" | grep -q "^4242" && grep -q "^-9 4242" "$T/kills" && [ -f "$T/cleaned" ]'

echo "- check"
setup; run check
check "check: all fine → 0" '[ $RC = 0 ]'
setup; rm -f "$D/tailscale"; run check
check "check: CLI missing → 1" '[ $RC = 1 ] && grep -q "FAIL.*tailscale missing" "$T/out"'
setup; echo 'X="' >"$D/tuning.env"; run check
check "check: bad tuning.env → 1" '[ $RC = 1 ] && grep -q "FAIL.*tuning.env" "$T/out"'
setup; mkdir -p "$D/alt"; cp "$D/tailscaled" "$D/alt/tsd"; echo "TS_TAILSCALED_BIN=$D/alt/tsd" >"$D/tuning.env"; run check
check "check: binary not named tailscaled → 1" '[ $RC = 1 ] && grep -q "file name must be tailscaled" "$T/out"'
setup; echo 50000 >"$T/free"; run check
check "check: under 100 MB free → 1" '[ $RC = 1 ] && grep -q "< 100 MB" "$T/out"'
setup; run status
check "status prints" '[ $RC = 0 ] && grep -q "strikes:" "$T/out"'

echo "- boot log caps itself"
setup; i=0; while [ $i -lt 1200 ]; do echo "filler line $i ......................................................" >>"$D/boot.log"; i=$((i + 1)); done; run
check "boot.log back under the cap" '[ $(wc -c <"$D/boot.log") -le 65536 ] && grep -q "mode=tun" "$D/boot.log"'

run bogus
check "unknown command: usage, exit 2" '[ $RC = 2 ]'
check "sh -n" 'sh -n "$S"'
echo "tailscale start: $PASS passed, $FAIL failed"
[ "$FAIL" = 0 ]
