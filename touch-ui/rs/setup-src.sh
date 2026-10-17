#!/bin/sh
# Fill a docker volume with the C sources the Rust UI builds against (lvgl-sys/build.rs):
#   /s/lvgl      LVGL v9.5.0 (upstream tag) + patches/lvgl-v9.5.0-freetype-bold.patch
#   /s/freetype  FreeType VER-2-13-3 (upstream tag; built by lvgl-sys, only the modules it needs)
#
#   rs/setup-src.sh [volume]      default $LUYAZ_RS_SRC_VOL (or $U60_RS_SRC_VOL), else u60-spike-src
#
# Nothing else is needed: no C toolchain image, no other cache (Phase 10 X5, review D11). An
# existing complete volume is left as it is; remove it (docker volume rm) to start over.
# LUYAZ_RS_IMAGE: the image to run in (default u60-rust-ci:1.99.0; build-kit.sh passes its own).
# SPDX-License-Identifier: MIT
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
VOL=${1:-${LUYAZ_RS_SRC_VOL:-${U60_RS_SRC_VOL:-u60-spike-src}}}
exec docker run --rm -v "$VOL":/s -v "$HERE/../patches":/p:ro "${LUYAZ_RS_IMAGE:-u60-rust-ci:1.99.0}" sh -c '
set -eu
cd /s
if [ -f lvgl/.u60-ready ] && [ -f freetype/.u60-ready ]; then echo "volume already set up"; exit 0; fi
rm -rf lvgl freetype lvgl.tmp freetype.tmp
git -c advice.detachedHead=false clone -q --depth 1 --branch v9.5.0 https://github.com/lvgl/lvgl.git lvgl.tmp
(cd lvgl.tmp && git apply -p3 /p/lvgl-v9.5.0-freetype-bold.patch)
git -c advice.detachedHead=false clone -q --depth 1 --branch VER-2-13-3 https://gitlab.freedesktop.org/freetype/freetype.git freetype.tmp
mv lvgl.tmp lvgl && mv freetype.tmp freetype
: >lvgl/.u60-ready; : >freetype/.u60-ready
echo "LVGL $(git -C lvgl describe --tags) + patch, FreeType $(git -C freetype describe --tags)"'
