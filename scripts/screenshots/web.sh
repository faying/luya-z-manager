#!/bin/sh
# Admin web screenshots on made-up data (docs/images/web-*.png; README.md here).
#
#   scripts/screenshots/web.sh [OUT_DIR] [name…]      (default OUT_DIR: scripts/screenshots/out/web)
#
# Builds luyaz-web from this checkout in Docker, starts `luyaz-web mock` replaying a synthetic
# event stream (gen-fake-sse.py) and `luyaz-web serve` in one container, puts the typed fake
# agent (crates/luyaz-web/e2e/fake-agent.mjs) behind rewrite-agent.mjs (one made-up operator)
# and core-front.mjs, then shoots every page in English (shoot-web.mjs). Stops everything at
# the end. Needs Docker, node, and `npm ci` in crates/luyaz-web once.
# Env: IMAGE (Rust build image, default u60-rust-ci:1.99.0), CHROME (browser for Playwright;
# default its own, else the system Google Chrome), PORT_BASE (default 28000: uses +81, +461, +470, +90 (+92 its server), +91, +95).
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
out=${1:-$here/out/web}; [ $# -gt 0 ] && shift
mkdir -p "$out"; out=$(cd "$out" && pwd)
image=${IMAGE:-u60-rust-ci:1.99.0}
pb=${PORT_BASE:-28000}
web=$((pb + 81)) mock=$((pb + 461)) front=$((pb + 470)) agent=$((pb + 90)) rewrite=$((pb + 91)) rewrite2=$((pb + 95))
name=luyaz-shots-web
[ -d "$root/crates/luyaz-web/node_modules/@playwright/test" ] || { echo "first: (cd crates/luyaz-web && npm ci)" >&2; exit 2; }

work=$(mktemp -d)
pids=
cleanup() {
    docker rm -f $name >/dev/null 2>&1 || true
    # children too: the fake agent runs its server as a child
    for p in $pids; do pkill -P "$p" 2>/dev/null || true; done
    [ -z "$pids" ] || kill $pids 2>/dev/null || true
    rm -rf "$work"
}
trap cleanup EXIT INT TERM

echo "== build luyaz-web ($image)"
# target in a volume, not in the checkout; Cargo.lock put back if the build rewrites it
cp "$root/Cargo.lock" "$work/Cargo.lock"
docker run --rm -v "$root":/w -v luyaz-shots-target:/target -v u60-cargo-registry:/usr/local/cargo/registry \
    "$image" sh -c 'cd /w && CARGO_TARGET_DIR=/target cargo build -q -p luyaz-web'
cp "$work/Cargo.lock" "$root/Cargo.lock"

python3 "$here/gen-fake-sse.py" "$work/fake-events.sse"
printf 'LUYAZ_WEB_PASSWORD=x\n' > "$work/pw.env"

echo "== start mock + serve, fake agent"
docker rm -f $name >/dev/null 2>&1 || true
# LUYAZ_WEB_IFACES: the port mapping arrives on the container's eth0, not lo (guard.rs serves only
# listed interfaces); LUYAZ_WEB_CHILL_SH: no CHILL here, as on a device without it
docker run -d --name $name -e LUYAZ_WEB_IFACES=lo,eth0 -e LUYAZ_WEB_CHILL_SH=/nonexistent/chill.sh \
    -p 127.0.0.1:$web:8080 -p 127.0.0.1:$mock:9460 \
    --add-host host.docker.internal:host-gateway -v luyaz-shots-target:/target:ro -v "$work":/s:ro "$image" \
    sh -c "/target/debug/luyaz-web mock --bind 0.0.0.0:9460 --replay /s/fake-events.sse --speed 1 &
           exec /target/debug/luyaz-web serve --bind 0.0.0.0:8080 --upstream http://host.docker.internal:$rewrite2 \
                --password-file /s/pw.env --session-file /tmp/s" >/dev/null
(cd "$root/crates/luyaz-web" && exec node e2e/fake-agent.mjs $agent) >"$work/agent.log" 2>&1 & pids="$pids $!"
node "$here/rewrite-agent.mjs" $rewrite $agent >"$work/rewrite.log" 2>&1 & pids="$pids $!"
node "$here/rewrite-agent.mjs" $rewrite2 $front 0.0.0.0 >"$work/rewrite2.log" 2>&1 & pids="$pids $!"
(cd "$root/crates/luyaz-web" && exec node e2e/core-front.mjs $front $rewrite $mock) >"$work/front.log" 2>&1 & pids="$pids $!"

# let the replay fill the 5-minute charts and the signal trend
i=0
until curl -fs --noproxy '*' -o /dev/null http://127.0.0.1:$web/login; do
    i=$((i + 1)); [ $i -lt 60 ] || { docker logs $name | tail -n 20; exit 1; }; sleep 1
done
echo "== waiting ${WARMUP:-60}s for the charts to fill"
sleep "${WARMUP:-60}"

echo "== shoot → $out"
WEB=http://127.0.0.1:$web OUT="$out" node "$here/shoot-web.mjs" "$@"
