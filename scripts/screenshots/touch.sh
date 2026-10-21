#!/bin/sh
# Touch UI screenshots on made-up data (docs/images/touch-*.png; README.md here).
#
#   LUYAZ_DEVICE_FONTS=<dir with ZTEZhengYuan.ttf, Roboto.ttf> LUYAZ_NUNITO_DIR=<dir with Nunito-*.ttf> \
#   MODELS=<model dir> scripts/screenshots/touch.sh [OUT_DIR]     (default OUT_DIR: scripts/screenshots/out/touch)
#
# Renders the Rust UI off-screen (luyaz-ui --render, touch-ui/rs/devui/src/render/mod.rs) on the
# view models in MODELS (`index-<scene>.txt` per scene). The old ones came from the C render
# test and are no longer used; new ones are to be generated from the Rust UI. Needs Docker and
# `touch-ui/rs/build.sh native` (volume u60-devui-rs-target). The device fonts are not in the
# repo (ZTEZhengYuan is proprietary): copy them from /usr/ui/fonts on a U60 Pro.
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
out=${1:-$here/out/touch}
mkdir -p "$out"; out=$(cd "$out" && pwd)
: "${LUYAZ_DEVICE_FONTS:?dir with ZTEZhengYuan.ttf and Roboto.ttf}" "${LUYAZ_NUNITO_DIR:?dir with Nunito-600/700/800.ttf}"

# file name ← scene, page, scroll step (.0 = top); scenes without CHILL lines, English
shots='touch-home us-light-en home 0
touch-home-dark op-notice-dark-en home 0
touch-notice op-notice-light-en home 0
touch-lowbat lowbat-light-en home 0
touch-cellular good-light-en cellular 1
touch-wifi good-light-en wifi 0
touch-system good-light-en system 0
touch-system-charts good-light-en system 1
touch-alerts good-light-en alerts 0
touch-bandlock bandlock-light-en lock 0'

models=${MODELS:?dir with the view models (index-<scene>.txt)}
for s in $(printf '%s\n' "$shots" | awk '{print $2}' | sort -u); do
    [ -f "$models/index-$s.txt" ] || { echo "no model for $s in $models" >&2; exit 1; }
done

echo "== render → $out"
raw=$(mktemp -d)
printf '%s\n' "$shots" | awk '{print $2, $3}' | sort -u | awk '{p[$1] = p[$1] ? p[$1] "," $2 : $2} END {for (s in p) print s, p[s]}' |
while read -r scene pages; do
    docker run --rm -v u60-devui-rs-target:/t:ro -v "$models":/m:ro -v "$raw":/o \
        -v "$LUYAZ_DEVICE_FONTS":/fonts/dev:ro -v "$LUYAZ_NUNITO_DIR":/fonts/nunito:ro \
        -e LUYAZ_UI_CJK_FONT=/fonts/dev/ZTEZhengYuan.ttf -e LUYAZ_UI_ROBOTO=/fonts/dev/Roboto.ttf \
        -e LUYAZ_UI_FONT_DIR=/fonts/nunito u60-rust-ci:1.99.0 \
        /t/release-device/luyaz-ui --render --models=/m --name="$scene" --pages="$pages" --png=/o --keep-going >/dev/null 2>&1 || true  # exits 1 when its parity checks differ; the PNGs are still written
done
printf '%s\n' "$shots" | while read -r file scene page k; do
    src=$raw/$scene-$page.$k.png
    [ -f "$src" ] || { echo "missing $src" >&2; exit 1; }
    # the renderer writes uncompressed PNGs (~460 KB); re-encode (~30 KB)
    if command -v sips >/dev/null; then sips -s format png "$src" --out "$out/$file.png" >/dev/null
    elif command -v optipng >/dev/null; then cp "$src" "$out/$file.png"; optipng -quiet "$out/$file.png"
    else cp "$src" "$out/$file.png"; fi
    echo "$out/$file.png"
done
rm -rf "$raw"
