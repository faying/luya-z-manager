#!/bin/sh
# Builds u60d's policy components (u60-platform.md Phase 4) inside the Rust CI image
# (u60-rust-ci:1.99.0, which has wasm32-unknown-unknown and wasm-tools; tools/u60-dev build).
#
#   crates/luyaz-plugins/build.sh        out/: <name>.wasm, <name>.cwasm for the device, plugins.json
#   crates/luyaz-plugins/build.sh test   also a .cwasm for the container's own target in out/test/ and a
#                                      component that never returns per policy in out/test-hostile/
#                                      (test only; under the real names),
#                                      then u60-policy's and u60d's tests with both required
#   crates/luyaz-plugins/build.sh repro  device build, then again from a fresh target dir; fails
#                                      unless out/ comes out byte for byte the same
#
# Run from anywhere; on the Mac it starts a one-off container (same cache volume as tools/u60-dev).
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
MANAGER=$(cd "$HERE/../.." && pwd)
MODE=${1:-device}
IMAGE=${LUYAZ_RUST_CI_IMAGE:-${U60_RUST_CI_IMAGE:-u60-rust-ci:1.99.0}}

if [ ! -f /.dockerenv ]; then
    exec docker run --rm -v "$MANAGER":/m -w /m/crates/luyaz-plugins \
        -v u60-cargo-registry:/usr/local/cargo/registry \
        -e CARGO_TARGET_DIR=/m/target/docker "$IMAGE" sh build.sh "$MODE"
fi

cd "$HERE"
# The bytes depend on where the source sits: luyaz-policy is outside this workspace, so cargo's
# -C metadata for it (and every guest using it) hashes its absolute path, which moves symbols
# and the LTO layout (--remap-path-prefix doesn't help). /m is the path of the Mac wrapper
# above; built anywhere else (tools/u60-dev rust has /w/manager) the .cwasm differs.
[ "$MANAGER" = /m ] || echo "build.sh: source at $MANAGER, not /m: out/ won't match a release build" >&2
T=${CARGO_TARGET_DIR:-$HERE/target}
# every component; its ABI is the package line of its wit file (u60d checks the same string)
NAMES="battery-eta charge-policy scenario scheduler manual-first apn-pick sms-forward home-exit"
# test only: a component that never returns, standing in for the policy of the same world
HOSTILE="hostile-eta:battery-eta hostile-scenario:scenario hostile-scheduler:scheduler hostile-manual-first:manual-first hostile-apn-pick:apn-pick hostile-sms-forward:sms-forward hostile-home-exit:home-exit"
abi() { sed -n 's/^package \(.*\);$/\1/p' "../luyaz-policy/wit/$1.wit"; }
PLUGINS=
for n in $NAMES; do PLUGINS="$PLUGINS $n:$(abi "$n")"; done

cargo build -q --release --target wasm32-unknown-unknown \
    $(for n in $NAMES; do printf ' -p %s' "$n"; done) \
    $(for h in $HOSTILE; do printf ' -p %s' "${h%%:*}"; done)
cargo build -q --release -p plugc
mkdir -p out
entries=
for p in $PLUGINS; do
    name=${p%%:*} abi=${p#*:}
    lib=$(echo "$name" | tr - _)
    wasm-tools component new "$T/wasm32-unknown-unknown/release/$lib.wasm" -o "out/$name.wasm"
    entry=$("$T/release/plugc" "out/$name.wasm" "out/$name.cwasm" "$name" "$abi")
    entries=${entries:+$entries,}$entry
done
printf '{"plugins":[%s]}\n' "$entries" > out/plugins.json
ls -l out

if [ "$MODE" = repro ] && [ -z "${LUYAZ_REPRO_AGAIN:-}" ]; then
    rm -rf "$T.repro" out.repro
    mv out out.repro
    LUYAZ_REPRO_AGAIN=1 CARGO_TARGET_DIR=$T.repro sh "$HERE/build.sh" repro >/dev/null
    for f in out.repro/*; do
        cmp "$f" "out/${f#out.repro/}" || { echo "build.sh repro: $f differs" >&2; exit 1; }
    done
    rm -rf "$T.repro" out.repro
    md5sum out/*
    echo "build.sh repro: out/ is the same from both builds"
fi

if [ "$MODE" = test ]; then
    host=$(rustc -vV | sed -n 's/^host: //p')
    mkdir -p out/test
    entries=
    for p in $PLUGINS; do
        name=${p%%:*} abi=${p#*:}
        entry=$("$T/release/plugc" "out/$name.wasm" "out/test/$name.cwasm" "$name" "$abi" "$host")
        entries=${entries:+$entries,}$entry
    done
    printf '{"plugins":[%s]}\n' "$entries" > out/test/plugins.json
    # each hostile one under the name and ABI of the policy it stands in for
    mkdir -p out/test-hostile
    entries=
    for h in $HOSTILE; do
        crate=${h%%:*} name=${h#*:}
        lib=$(echo "$crate" | tr - _)
        wasm-tools component new "$T/wasm32-unknown-unknown/release/$lib.wasm" -o "out/test-hostile/$crate.wasm"
        entry=$("$T/release/plugc" "out/test-hostile/$crate.wasm" "out/test-hostile/$name.cwasm" \
            "$name" "$(abi "$name")" "$host")
        entries=${entries:+$entries,}$entry
    done
    printf '{"plugins":[%s]}\n' "$entries" > out/test-hostile/plugins.json
    cd "$MANAGER"
    LUYAZ_TEST_PLUGINS=$HERE/out/test LUYAZ_TEST_HOSTILE=$HERE/out/test-hostile LUYAZ_REQUIRE_PLUGINS=1 \
        cargo test -q -p luyaz-policy -p luyaz-core -- plugin battery_eta charge_policy scenario scheduler manual_first apn_pick sms_forward home_exit 2>&1 |
        tail -n 40
fi
