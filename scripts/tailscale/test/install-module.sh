#!/bin/bash
# install.sh's Tailscale menu (module_tailscale / uninstall 4) end to end, on a
# fake device: the container's own /etc/rc.local, /data and /tmp, with stub
# tailscaled/tailscale and the real scripts/tailscale/start.sh.
#   docker run --rm -v "$PWD":/w bash:5 bash /w/scripts/tailscale/test/install-module.sh
# It rewrites /etc/rc.local, so it refuses to run outside a container.
# SPDX-License-Identifier: MIT
[ -f /.dockerenv ] || { echo "run this in a throwaway container (it edits /etc/rc.local)"; exit 2; }
REPO=$(cd "$(dirname "$0")/../../.." && pwd)
PASS=0; FAIL=0
ok_() { PASS=$((PASS + 1)); echo "  ok   $1"; }
bad() { FAIL=$((FAIL + 1)); echo "  FAIL $1"; }
check() { _d=$1; shift; if eval "$@"; then ok_ "$_d"; else bad "$_d  [$*]"; fi; }

# install.sh without its last line (menu), the ssh helpers replaced by local ones
sed '$d' "$REPO/install.sh" >/tmp/inst.sh
# shellcheck disable=SC1091
. /tmp/inst.sh
DIR=$REPO
rcmd() { sh -c "$*"; }
rpush() { cat "$1" >"$2" && chmod "${3:-755}" "$2"; }
require_conn() { return 0; }

device() { # a device with the old tailscale-start.sh installed
    pkill -f /data/tailscale/tailscaled 2>/dev/null
    rm -rf /data /tmp/tailscale* /tmp/ts-* /tmp/tailscaled.sock
    mkdir -p /data/tailscale /etc
    printf '#!/bin/sh\n# vendor lines\nsh /data/tailscale-start.sh > /dev/null 2>&1 &\nexit 0\n' >/etc/rc.local
    printf "TS_AUTHKEY='tskey-old'\nTS_ROUTES='10.1.1.0/24'\n" >/data/tailscale/tsconfig
    echo old >/data/tailscale-start.sh
    echo IDENTITY >/data/tailscale/tailscaled.state
    echo 'TS_TAILSCALED_FLAGS=--no-logs-no-support' >/data/tailscale/tuning.env
    cat >/data/tailscale/tailscaled <<'X'
#!/bin/sh
case "$1" in --version) echo 1.0.0; exit 0 ;; --cleanup) echo cleanup >> /tmp/ts-calls; exit 0 ;; esac
echo "daemon $*" >> /tmp/ts-calls; exec sleep 300
X
    cat >/data/tailscale/tailscale <<'X'
#!/bin/sh
shift   # --socket=…
echo "cli $*" >> /tmp/ts-calls
case "$1" in
  version) echo 1.0.0 ;;
  status) [ "$2" = --json ] && echo '{"BackendState": "NeedsLogin"}' ;;
  login) [ -f /tmp/ts-authkey ] && cat /tmp/ts-authkey >> /tmp/ts-keyseen ;;
esac
exit 0
X
    chmod 755 /data/tailscale/tailscaled /data/tailscale/tailscale
}
answers() { # authkey routes exit hostname accept_routes accept_dns start_now
    printf '%s\n' "$@"
}
wait_state() { i=0; while [ ! -f /data/tailscale/state/tailscaled.state ] && [ $i -lt 20 ]; do sleep 1; i=$((i + 1)); done; }

echo "- install over the old tailscale-start.sh, log in by URL"
device
answers '' '10.2.2.0/24' '' 'node-a' '' '' 'Y' | module_tailscale >/tmp/out 2>&1
RC=$?
wait_state
check "module returns 0" '[ $RC = 0 ]'
check "rc.local: new line before exit 0" 'grep -n . /etc/rc.local | grep -B1 "exit 0" | grep -q "sh /data/tailscale/start.sh"'
check "rc.local: old tailscale-start.sh line gone" '! grep -q tailscale-start.sh /etc/rc.local'
check "rc.local passes sh -n, backup kept" 'sh -n /etc/rc.local && [ -f /data/rc.local.bak-installer ]'
check "old tsconfig (auth key) and old script removed" '[ ! -f /data/tailscale/tsconfig ] && [ ! -f /data/tailscale-start.sh ]'
check "tuning.env: settings written, other lines kept, no auth key" \
    'grep -q "^TS_HOSTNAME=.node-a.$" /data/tailscale/tuning.env && grep -q "^TS_ROUTES=.10.2.2.0/24.$" /data/tailscale/tuning.env && grep -q no-logs-no-support /data/tailscale/tuning.env && ! grep -q AUTHKEY /data/tailscale/tuning.env'
check "start.sh and apply.sh installed" '[ -f /data/tailscale/start.sh ] && [ -x /data/tailscale/apply.sh ]'
check "identity moved to state/tailscaled.state" '[ "$(cat /data/tailscale/state/tailscaled.state 2>/dev/null)" = IDENTITY ]'
check "tailscaled started" 'grep -q "^daemon .*--tun=tailscale0" /tmp/ts-calls'
check "login by URL was offered" 'grep -q "^cli login --timeout=180s" /tmp/ts-calls'

echo "- second run: idempotent"
answers '' '-' '' 'node-a' 'n' 'y' 'n' | module_tailscale >/tmp/out 2>&1
check "one rc.local line" '[ $(grep -c "/data/tailscale/start.sh" /etc/rc.local) = 1 ]'
check "'-' skips the subnet, toggles written" 'grep -q "^TS_ROUTES=..$" /data/tailscale/tuning.env && grep -q "^TS_ACCEPT_ROUTES=.false.$" /data/tailscale/tuning.env && grep -q "^TS_ACCEPT_DNS=.true.$" /data/tailscale/tuning.env'
check "settings not duplicated" '[ $(grep -c "^TS_HOSTNAME=" /data/tailscale/tuning.env) = 1 ]'

echo "- auth key: used once, never stored"
device
answers 'fake-key-0123' '' '' 'node-b' '' '' 'Y' | module_tailscale >/tmp/out 2>&1
wait_state
check "login with the key file" 'grep -q "^cli login --auth-key=file:/tmp/ts-authkey" /tmp/ts-calls && [ "$(cat /tmp/ts-keyseen)" = fake-key-0123 ]'
check "key file removed, key nowhere under /data or rc.local" '[ ! -f /tmp/ts-authkey ] && ! grep -rq fake-key-0123 /data /etc/rc.local'

echo "- bad input / failed check: rc.local untouched"
device
cp /etc/rc.local /tmp/rc.before
answers '' '' '' 'bad;name' '' '' 'Y' | module_tailscale >/tmp/out 2>&1
check "unsafe characters refused, rc.local unchanged" '[ $? != 0 ] || true; grep -q "Only letters" /tmp/out && cmp -s /etc/rc.local /tmp/rc.before'
device
mkdir -p /data/tailscale/alt && cp /data/tailscale/tailscaled /data/tailscale/alt/tsd
echo 'TS_TAILSCALED_BIN=/data/tailscale/alt/tsd' >>/data/tailscale/tuning.env
answers '' '' '' 'node-c' '' '' 'Y' | module_tailscale >/tmp/out 2>&1
check "check fails → no rc.local line, nothing started" '! grep -q "/data/tailscale/start.sh" /etc/rc.local && grep -q "Check failed" /tmp/out && ! grep -q "^daemon" /tmp/ts-calls 2>/dev/null'

echo "- uninstall"
device
answers '' '' '' 'node-d' '' '' 'Y' | module_tailscale >/tmp/out 2>&1
wait_state
echo 4 | module_uninstall >/tmp/out 2>&1
check "rc.local line removed, sh -n ok" '! grep -q tailscale /etc/rc.local && sh -n /etc/rc.local'
check "stopped with cleanup, identity kept" 'grep -q "^cleanup" /tmp/ts-calls && [ -f /data/tailscale/state/tailscaled.state ]'

echo "install module: $PASS passed, $FAIL failed"
[ "$FAIL" = 0 ]
