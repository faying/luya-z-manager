#!/bin/sh
# The status bar's ▲ covers nothing tappable (eng review round 3 D5): Home, every tab and every
# subpage are opened offscreen with the ▲ shown, and every control a fifth or more under its
# 44×44 box is listed (an open incident can keep the ▲ lit for days).
#
#   LUYAZ_DEVICE_FONTS=… LUYAZ_NUNITO_DIR=… sh rs/hitcheck.sh     (after rs/build.sh device, on the Mac)
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
set -eu
docker run --rm -v u60-ui-rs-target:/t:ro \
    -v "$LUYAZ_DEVICE_FONTS":/fonts/dev:ro -v "$LUYAZ_NUNITO_DIR":/fonts/nunito:ro \
    -e LUYAZ_UI_CJK_FONT=/fonts/dev/ZTEZhengYuan.ttf -e LUYAZ_UI_ROBOTO=/fonts/dev/Roboto.ttf \
    -e LUYAZ_UI_FONT_DIR=/fonts/nunito u60-rust-ci:1.99.0 \
    sh -c '/t/aarch64-unknown-linux-musl/release-device/luyaz-ui --hitcheck 2>/dev/null'
