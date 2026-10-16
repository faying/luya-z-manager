#!/bin/sh
# apply.sh against stubbed tailscale/jsonfilter/pidof/kill (busybox container).
# SPDX-License-Identifier: MIT
HERE=$(cd "$(dirname "$0")/.." && pwd)
PASS=0; FAIL=0
ok() { PASS=$((PASS + 1)); echo "  ok   $1"; }
bad() { FAIL=$((FAIL + 1)); echo "  FAIL $1"; }
check() { _d=$1; shift; if eval "$@"; then ok "$_d"; else bad "$_d  [$*]"; fi; }

setup() {
    T=$(mktemp -d); mkdir -p "$T/d" "$T/bin"
    echo 10.9.9.0/24 > "$T/route"; echo 100.64.0.2 > "$T/peers"; echo 0 > "$T/pingrc"; : > "$T/starts"
    touch "$T/running"
    # health follows the active tuning: a variant containing BAD never comes up,
    # LOSTROUTE comes up without the subnet route
    cat > "$T/bin/jf" <<X
#!/bin/sh
cat >/dev/null
t=\$(cat $T/d/tuning.env 2>/dev/null)
case "\$2" in
  '@.BackendState') case "\$t" in *BAD*) echo Stopped ;; *) echo Running ;; esac ;;
  '@.Self.PrimaryRoutes[*]'|'@.Self.PrimaryRoutes[0]') case "\$t" in *LOSTROUTE*) ;; *) cat $T/route ;; esac ;;
  '@.Self.Online') echo true ;;
  '@.Peer[@.Online=true].TailscaleIPs[0]') cat $T/peers ;;
esac
X
    cat > "$T/bin/ts" <<X
#!/bin/sh
case "\$2" in status) echo '{}' ;; ping) exit \$(cat $T/pingrc) ;; esac
X
    printf '#!/bin/sh\n[ -f %s/running ] && echo 4242\n' "$T" > "$T/bin/pidof"
    printf '#!/bin/sh\nrm -f %s/running\n' "$T" > "$T/bin/kill"
    printf '#!/bin/sh\n(cat %s/d/tuning.env 2>/dev/null || echo none) | tr "\\n" " " >> %s/starts; echo >> %s/starts; touch %s/running\n[ -f %s/run/tailscale-apply.pid ] && echo applying >> %s/marker\n' "$T" "$T" "$T" "$T" "$T" "$T" > "$T/start.sh"
    mkdir -p "$T/run"
    chmod +x "$T"/bin/*
    export TSA_RUN=$T/run TSA_DIR=$T/d TSA_CLI=$T/bin/ts TSA_START=$T/start.sh TSA_LOG=$T/log TSA_TIMEOUT=30 TSA_POLL=10 \
        TSA_JSONFILTER=$T/bin/jf TSA_PIDOF=$T/bin/pidof TSA_KILL=$T/bin/kill TSA_SLEEP=true TSA_INIT=$T/noinit
    echo 'TS_TAILSCALED_FLAGS=' > "$T/d/tuning.env"
}
run() { sh "$HERE/apply.sh" "$@" >/dev/null 2>&1; RC=$?; }

setup; echo 'TS_TAILSCALED_FLAGS=--no-logs-no-support' > "$T/good.env"
run "$T/good.env"
check "healthy variant: exit 0" '[ $RC = 0 ]'
check "healthy variant: kept as tuning.env" 'grep -q no-logs "$T/d/tuning.env"'
check "healthy variant: restarted once" '[ $(wc -l < "$T/starts") = 1 ]'
check "healthy variant: logged kept" 'grep -q "kept" "$T/log"'
check "no rollback copy left behind" '[ ! -f "$T/d/tuning.env.rollback" ]'
check "marker present while it restarts, gone after" '[ "$(cat "$T/marker")" = applying ] && [ ! -f "$T/run/tailscale-apply.pid" ]'

setup; echo 'X=BAD' > "$T/bad.env"
run "$T/bad.env"
check "backend never Running: exit 1" '[ $RC = 1 ]'
check "backend never Running: previous tuning restored" '[ "$(cat "$T/d/tuning.env")" = "TS_TAILSCALED_FLAGS=" ]'
check "backend never Running: restarted twice (variant, then rollback)" '[ $(wc -l < "$T/starts") = 2 ]'
check "backend never Running: reason logged" 'grep -q "backend not Running" "$T/log"'
check "marker held through the rollback too, gone after" '[ "$(wc -l < "$T/marker")" = 2 ] && [ ! -f "$T/run/tailscale-apply.pid" ]'

setup; echo 'X=LOSTROUTE' > "$T/lost.env"
run "$T/lost.env"
check "subnet route lost: rolled back" '[ $RC = 1 ] && grep -q "route 10.9.9.0/24 not primary" "$T/log"'

setup; echo 1 > "$T/pingrc"
run "$T/good.env" 2>/dev/null; run "$T/d/tuning.env" node
check "node mode, no peer answers: rolled back" '[ $RC = 1 ] && grep -q "no online peer" "$T/log"'
setup; echo 1 > "$T/pingrc"; echo 'Y=1' > "$T/y.env"
run "$T/y.env" relay
check "relay mode does not need a peer ping" '[ $RC = 0 ]'

setup; printf '#!/bin/sh\nls %s/d >> %s/seen; touch %s/running\n' "$T" "$T" "$T" > "$T/start.sh"
echo 'TS_TAILSCALED_FLAGS=--no-logs-no-support' > "$T/good.env"; run "$T/good.env"
check "during the trial the previous tuning is saved for start.sh" 'grep -qx tuning.env.rollback "$T/seen" && [ ! -f "$T/d/tuning.env.rollback" ]'
setup; rm -f "$T/d/tuning.env"; printf '#!/bin/sh\nls %s/d >> %s/seen; touch %s/running\n' "$T" "$T" "$T" > "$T/start.sh"
echo 'TS_TAILSCALED_FLAGS=--no-logs-no-support' > "$T/good.env"; run "$T/good.env"
check "no previous tuning: marker during the trial, gone after" 'grep -qx tuning.env.rollback-none "$T/seen" && [ ! -f "$T/d/tuning.env.rollback-none" ]'
setup; rm -f "$T/d/tuning.env"; echo 'X=BAD' > "$T/bad.env"; run "$T/bad.env"
check "no previous tuning, rolled back: marker gone too" '[ ! -f "$T/d/tuning.env.rollback-none" ]'

setup; mkdir -p "$T/failbin"; printf '#!/bin/sh\nexit 1\n' >"$T/failbin/cp"; chmod +x "$T/failbin/cp"
echo 'TS_TAILSCALED_FLAGS=--no-logs-no-support' > "$T/good.env"; PATH=$T/failbin:$PATH run "$T/good.env"
check "current tuning cannot be saved: nothing switched, exit 1" '[ $RC = 1 ] && [ "$(cat "$T/d/tuning.env")" = "TS_TAILSCALED_FLAGS=" ] && [ ! -s "$T/starts" ] && grep -q "not applied" "$T/log"'

setup; run --none
check "--none: tuning.env removed" '[ $RC = 0 ] && [ ! -f "$T/d/tuning.env" ]'
setup; rm -f "$T/d/tuning.env"; echo 'X=BAD' > "$T/bad.env"; run "$T/bad.env"
check "no previous tuning: rollback leaves none" '[ $RC = 1 ] && [ ! -f "$T/d/tuning.env" ]'

setup; echo 'X=BAD' > "$T/bad.env"; printf '#!/bin/sh\necho old >> %s/starts; touch %s/running\n' "$T" "$T" > "$T/old-start.sh"
export TSA_ROLLBACK_START=$T/old-start.sh; run "$T/bad.env"; unset TSA_ROLLBACK_START
check "rollback uses TSA_ROLLBACK_START when the start script is on trial" '[ $RC = 1 ] && [ "$(tail -n 1 "$T/starts")" = old ] && grep -q "rollback starts with" "$T/log"'

setup; printf '#!/bin/sh\necho "$1" >>%s/init.calls\n' "$T" > "$T/init"; chmod +x "$T/init"
printf '#!/bin/sh\necho killed >> %s/kills; rm -f %s/running\n' "$T" "$T" > "$T/bin/kill"
echo 'Z=1' > "$T/z.env"; export TSA_INIT=$T/init; run "$T/z.env"; export TSA_INIT=$T/noinit
check "under procd: init stop, then the start script; never a kill" '[ $RC = 0 ] && grep -qx stop "$T/init.calls" && [ ! -f "$T/kills" ] && [ $(wc -l < "$T/starts") = 1 ]'

run /nonexistent
check "missing variant file: usage error" '[ $RC = 2 ]'
run "$T/good.env" sideways
check "bad mode: usage error" '[ $RC = 2 ]'
echo "tailscale apply: $PASS passed, $FAIL failed"
[ "$FAIL" = 0 ]
