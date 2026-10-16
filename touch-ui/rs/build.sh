#!/bin/sh
# Build the Rust touch UI in the shared build image (no toolchain on the Mac).
#   rs/build.sh test            cargo test, native (arm64 container)
#   rs/build.sh native          release build, native, for offscreen runs
#   rs/build.sh device [FILE]   static aarch64-musl release build (cargo zigbuild); FILE: a copy of
#                               the binary there (onboard/build-kit.sh)
#   rs/build.sh clippy
# Needs the docker volume $LUYAZ_RS_SRC_VOL (old name U60_RS_SRC_VOL still read) (default u60-spike-src) with the patched LVGL + FreeType
# sources: rs/setup-src.sh makes it. LUYAZ_RS_REG_VOL / LUYAZ_RS_TARGET_VOL (or U60_*) name the cargo registry and
# target volumes (defaults u60-cargo-registry, u60-devui-rs-target): fresh names = a build from
# nothing (review D11).
# Since 10f these crates are members of the manager's Cargo workspace (one Cargo.lock at the
# manager root); the profile is the root's release-device (opt "s", lto, cg1, panic=abort, strip =
# the old rs/ release), so the output is target/<triple>/release-device/luyaz-ui (Phase 10 R5; u60pro-devui before).
# SPDX-License-Identifier: MIT
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
P="-p lvgl-sys -p plat -p devui-model -p devui"
OUTV=
if [ "${1:-}" = device ] && [ -n "${2:-}" ]; then
    mkdir -p "$(dirname "$2")"
    OUTV="-v $(cd "$(dirname "$2")" && pwd):/o"
    OUTF=/o/$(basename "$2")
fi
case ${1:-native} in
    test) cmd="cargo test $P" ;;
    native) cmd="cargo build --profile release-device $P" ;;
    device) cmd="cargo zigbuild --profile release-device --target aarch64-unknown-linux-musl $P"
        [ -z "$OUTV" ] || cmd="$cmd && cp /t/aarch64-unknown-linux-musl/release-device/luyaz-ui $OUTF" ;;
    clippy) cmd="cargo clippy --all-targets $P -- -D warnings" ;;
    *) echo "rs/build.sh test|native|device|clippy" >&2; exit 2 ;;
esac
# shellcheck disable=SC2086
exec docker run --rm $OUTV -v "$HERE/../..":/repo -v "${LUYAZ_RS_SRC_VOL:-${U60_RS_SRC_VOL:-u60-spike-src}}":/s:ro \
    -v "${LUYAZ_RS_REG_VOL:-${U60_RS_REG_VOL:-u60-cargo-registry}}":/usr/local/cargo/registry -v "${LUYAZ_RS_TARGET_VOL:-${U60_RS_TARGET_VOL:-u60-devui-rs-target}}":/t -e CARGO_TARGET_DIR=/t -w /repo -e LVGL_DIR=/s/lvgl -e FT_SRC=/s/freetype -e LV_CONF_DIR=/repo/touch-ui \
    u60-rust-ci:1.99.0 sh -c "$cmd"
