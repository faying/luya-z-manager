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
    printf '#!/bin/sh\n(cat %s/d/tuning.env 2>/dev/null || echo none) | tr "\\n" " " >> %s/starts; echo >> %s/starts; touch %s/running\n' "$T" "$T" "$T" "$T" > "$T/start.sh"
    chmod +x "$T"/bin/*
    export TSA_DIR=$T/d TSA_CLI=$T/bin/ts TSA_START=$T/start.sh TSA_LOG=$T/log TSA_TIMEOUT=30 TSA_POLL=10 \
        TSA_JSONFILTER=$T/bin/jf TSA_PIDOF=$T/bin/pidof TSA_KILL=$T/bin/kill TSA_SLEEP=true
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

setup; echo 'X=BAD' > "$T/bad.env"
run "$T/bad.env"
check "backend never Running: exit 1" '[ $RC = 1 ]'
check "backend never Running: previous tuning restored" '[ "$(cat "$T/d/tuning.env")" = "TS_TAILSCALED_FLAGS=" ]'
check "backend never Running: restarted twice (variant, then rollback)" '[ $(wc -l < "$T/starts") = 2 ]'
check "backend never Running: reason logged" 'grep -q "backend not Running" "$T/log"'

setup; echo 'X=LOSTROUTE' > "$T/lost.env"
run "$T/lost.env"
check "subnet route lost: rolled back" '[ $RC = 1 ] && grep -q "route 10.9.9.0/24 not primary" "$T/log"'

setup; echo 1 > "$T/pingrc"
run "$T/good.env" 2>/dev/null; run "$T/d/tuning.env" node
check "node mode, no peer answers: rolled back" '[ $RC = 1 ] && grep -q "no online peer" "$T/log"'
setup; echo 1 > "$T/pingrc"; echo 'Y=1' > "$T/y.env"
run "$T/y.env" relay
check "relay mode does not need a peer ping" '[ $RC = 0 ]'

setup; run --none
check "--none: tuning.env removed" '[ $RC = 0 ] && [ ! -f "$T/d/tuning.env" ]'
setup; rm -f "$T/d/tuning.env"; echo 'X=BAD' > "$T/bad.env"; run "$T/bad.env"
check "no previous tuning: rollback leaves none" '[ $RC = 1 ] && [ ! -f "$T/d/tuning.env" ]'

run /nonexistent
check "missing variant file: usage error" '[ $RC = 2 ]'
run "$T/good.env" sideways
check "bad mode: usage error" '[ $RC = 2 ]'
echo "tailscale apply: $PASS passed, $FAIL failed"
[ "$FAIL" = 0 ]
