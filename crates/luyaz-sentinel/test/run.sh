#!/bin/sh
# The device binary against stub UIs in an arm64 container (nothing of the device; no device needed):
#   real.sh    real mode: launch, vendor/devui requests, crash ×2 → give-up, TERM leaves the UI,
#              restart adopts; /tmp keeps 1777; the UI gets a clean signal mask, SIGPIPE ignored
#   shadow.sh  --shadow changes nothing: the FIFO keeps its inode, state files unchanged, the only
#              new file is /tmp/luyaz-sentinel.shadow
# Run from source/manager:  crates/luyaz-sentinel/test/run.sh
set -eu
cd "$(dirname "$0")/../../.."
../../tools/u60-dev rust cargo zigbuild --target-dir target-test --profile release-device \
    --target aarch64-unknown-linux-musl -p luyaz-sentinel
bin=$(mktemp -d)
cp target-test/aarch64-unknown-linux-musl/release-device/luyaz-sentinel "$bin/"
fail=0
for t in real shadow; do
    echo "== $t"
    docker run --rm --platform linux/arm64 -v "$bin":/s:ro -v "$PWD/crates/luyaz-sentinel/test":/h:ro alpine:3 \
        sh "/h/$t.sh" | tee "$bin/$t.out" | grep -E '^  (ok|FAIL)|^passed'
    grep -q 'failed 0$' "$bin/$t.out" || fail=1
done
rm -rf "$bin"
exit $fail
