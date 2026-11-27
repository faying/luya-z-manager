#!/bin/sh
# F-1 on real filesystems: the luyaz-persist test binary against a real ENOSPC (a tiny tmpfs), a real
# EROFS (a read-only bind mount, what `errors=remount-ro` makes of /data) and a real EACCES (a
# directory the user nobody may not write). Each in its own container, as root except EACCES.
#
#   crates/luyaz-persist/fault.sh <docker volume with the build> <test binary path in it> <image>
#
# The test binary: `cargo test -p luyaz-persist --no-run` prints it (target/debug/deps/luyaz_persist-…);
# <image> is the one it was built in (its C library). Everything stays on the docker host.
set -eu
vol=$1 bin=$2 image=$3
run() {
    docker run --rm -v "$vol":/t:ro "$@"
}
fail=0
seed='mkdir -p /x && printf "{\"old\":1}" > /x/state.json'

echo "== ENOSPC"
run --tmpfs /x:size=64k "$image" sh -c "$seed && LUYAZ_PERSIST_ENOSPC=/x /t/$bin real_enospc --nocapture" || fail=1

echo "== EROFS"
ro=luyaz-persist-ro-$$
docker volume create "$ro" >/dev/null
docker run --rm -v "$ro":/x "$image" sh -c 'printf "{\"old\":1}" > /x/state.json'
run -v "$ro":/x:ro "$image" sh -c "LUYAZ_PERSIST_EROFS=/x /t/$bin real_erofs --nocapture" || fail=1
docker volume rm "$ro" >/dev/null

echo "== EACCES"
run "$image" sh -c "$seed && chmod 755 /x && chmod 644 /x/state.json && su -s /bin/sh nobody -c 'LUYAZ_PERSIST_EACCES=/x /t/$bin real_eacces --nocapture'" || fail=1

[ $fail = 0 ] && echo "fault.sh: all three real" || { echo "fault.sh: FAILED"; exit 1; }
